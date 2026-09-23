//! Modification d'un document : objets remplacés, ajoutés ou supprimés,
//! puis enregistrement **incrémental** (ISO 32000-2 §7.5.6 : le fichier
//! d'origine est conservé octet pour octet, les changements sont ajoutés à
//! la fin) ou **complet** (réécriture propre de tous les objets).
//!
//! Règle de la charte : ce qui n'est pas touché n'est pas modifié. C'est
//! aussi ce qui préserve les signatures existantes lors d'un enregistrement
//! incrémental.

use acrux_core::{Error, Result};

use crate::crypt::md5;
use crate::crypt::random::{Random, SystemRandom};
use crate::document::Document;
use crate::objects::{Dict, Name, Object, ObjectRef};
use crate::writer::write_object;
use crate::xref::{find_startxref, XrefEntry, XrefKind};

impl Document {
    /// Remplace (ou crée) l'objet `r`.
    pub fn set(&self, r: ObjectRef, obj: Object) {
        self.edits.borrow_mut().insert(r.number, Some(obj));
        if r.number >= self.next_number.get() {
            self.next_number.set(r.number + 1);
        }
    }

    /// Supprime l'objet `r` (les références vers lui vaudront `null`).
    pub fn delete(&self, r: ObjectRef) {
        self.edits.borrow_mut().insert(r.number, None);
    }

    /// Réserve un nouveau numéro d'objet.
    pub fn allocate(&self) -> ObjectRef {
        let n = self.next_number.get();
        self.next_number.set(n + 1);
        ObjectRef {
            number: n,
            generation: 0,
        }
    }

    /// Crée un nouvel objet indirect et retourne sa référence.
    pub fn add(&self, obj: Object) -> ObjectRef {
        let r = self.allocate();
        self.set(r, obj);
        r
    }

    /// Modifie une clé du trailer (`Object::Null` la supprime).
    pub fn set_trailer_entry(&self, key: &str, value: Object) {
        self.trailer_edits
            .borrow_mut()
            .insert(Name::new(key), value);
    }

    /// Vrai s'il y a des modifications non enregistrées.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        !self.edits.borrow().is_empty() || !self.trailer_edits.borrow().is_empty()
    }

    /// Marque tout comme enregistré (après un `save_*` réussi et rechargé).
    pub fn clear_edits(&self) {
        self.edits.borrow_mut().clear();
        self.trailer_edits.borrow_mut().clear();
    }

    /// Enregistrement incrémental : octets d'origine + objets modifiés +
    /// nouvelle section xref + trailer chaîné par `/Prev`.
    ///
    /// Si le document est chiffré, les nouveaux objets sont chiffrés avec la
    /// même clé (le mot de passe doit avoir été accepté).
    ///
    /// # Errors
    /// Document chiffré sans mot de passe valide, ou fichier réparé sans
    /// `startxref` exploitable (utiliser [`Document::save_full`]).
    pub fn save_incremental(&self) -> Result<Vec<u8>> {
        if self.needs_password() {
            return Err(Error::Encrypted);
        }
        let original = self.bytes();
        let prev = match self.xref_kind() {
            XrefKind::Reconstructed => {
                return Err(Error::Corrupt(
                    "fichier réparé : l'enregistrement incrémental n'est pas fiable, utiliser save_full".into(),
                ))
            }
            _ => find_startxref(original)?,
        };
        let mut out = original.to_vec();
        if !matches!(out.last(), Some(b'\n' | b'\r')) {
            out.push(b'\n');
        }
        let security = self.security();
        // Les IV et le second /ID doivent être imprévisibles : un IV deviné
        // affaiblit CBC, et /ID entre dans la clé des révisions anciennes.
        let mut rng = SystemRandom::new();
        let mut entries: Vec<(u32, XrefEntry)> = Vec::new();
        let mut numbers: Vec<u32> = self.edits.borrow().keys().copied().collect();
        numbers.sort_unstable();
        for n in numbers {
            let edit = self.edits.borrow().get(&n).cloned().flatten();
            let r = ObjectRef {
                number: n,
                generation: 0,
            };
            match edit {
                None => entries.push((n, XrefEntry::Free)),
                Some(obj) => {
                    let offset = out.len() as u64;
                    let obj = match &security {
                        Some(h) if Some(r) != self.encrypt_ref() => {
                            encrypt_for_save(h, obj, r, &mut rng)
                        }
                        _ => obj,
                    };
                    out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
                    write_object(&fix_stream_length(obj), &mut out);
                    out.extend_from_slice(b"\nendobj\n");
                    entries.push((
                        n,
                        XrefEntry::Offset {
                            offset,
                            generation: 0,
                        },
                    ));
                }
            }
        }
        let mut trailer = self.trailer();
        trailer.insert(
            Name::new("Prev"),
            Object::Integer(i64::try_from(prev).unwrap_or(0)),
        );
        trailer.insert(
            Name::new("Size"),
            Object::Integer(i64::from(self.next_number.get())),
        );
        update_id(&mut trailer, &out, &mut rng);
        let xref_offset = out.len();
        match self.xref_kind() {
            XrefKind::Stream => {
                // Un fichier à flux xref doit être mis à jour avec un flux xref (§7.5.8).
                let stream_ref = self.next_number.get();
                trailer.insert(
                    Name::new("Size"),
                    Object::Integer(i64::from(stream_ref + 1)),
                );
                entries.push((
                    stream_ref,
                    XrefEntry::Offset {
                        offset: xref_offset as u64,
                        generation: 0,
                    },
                ));
                write_xref_stream(&mut out, stream_ref, &entries, &trailer);
            }
            _ => write_xref_table(&mut out, &entries, &trailer),
        }
        out.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        Ok(out)
    }

    /// Réécriture complète : tous les objets vivants, flux d'objets dépliés,
    /// table xref classique. Le résultat est propre et compact, mais les
    /// signatures existantes sont invalidées.
    ///
    /// # Errors
    /// Document chiffré sans mot de passe valide, ou objet illisible.
    pub fn save_full(&self) -> Result<Vec<u8>> {
        self.save_full_with(&SaveOptions::default())
    }

    /// Réécriture complète avec options (voir [`SaveOptions`]).
    ///
    /// # Errors
    /// Document chiffré sans mot de passe valide, ou objet illisible.
    #[allow(clippy::too_many_lines)] // objets directs, flux d'objets, trailer, xref : linéaire
    pub fn save_full_with(&self, options: &SaveOptions) -> Result<Vec<u8>> {
        if self.needs_password() {
            return Err(Error::Encrypted);
        }
        let security = self.security();
        // Les flux d'objets ne sont pas combinés au chiffrement ici (les
        // chaînes qu'ils contiennent ne doivent pas être chiffrées une à une).
        let pack = options.object_streams && security.is_none();
        let (major, minor) = match self.version() {
            v if pack && v < (1, 5) => (1, 5),
            v => v,
        };
        let mut out = format!("%PDF-{major}.{minor}\n%\u{E2}\u{E3}\u{CF}\u{D3}\n").into_bytes();
        let mut rng = SystemRandom::new();
        let mut entries: Vec<(u32, XrefEntry)> = Vec::new();
        let mut max_number = 0u32;
        // Objets à emballer dans des flux d'objets : (numéro, sérialisation).
        let mut packed: Vec<(u32, Vec<u8>)> = Vec::new();
        let keep = options.drop_unreferenced.then(|| self.reachable_objects());
        for n in self.object_numbers() {
            if keep.as_ref().is_some_and(|k| !k.contains(&n)) {
                continue;
            }
            let r = ObjectRef {
                number: n,
                generation: 0,
            };
            let obj = self.get(r)?;
            // Les flux xref et les flux d'objets n'ont plus lieu d'être.
            if let Some(d) = obj.as_dict() {
                if let Some(Object::Name(t)) = d.get(&Name::new("Type")) {
                    if t.0 == b"XRef" || t.0 == b"ObjStm" {
                        continue;
                    }
                }
            }
            if *obj == Object::Null {
                continue;
            }
            if pack && !matches!(&*obj, Object::Stream { .. }) && Some(r) != self.encrypt_ref() {
                let mut buf = Vec::new();
                write_object(&obj, &mut buf);
                packed.push((n, buf));
                max_number = max_number.max(n);
                continue;
            }
            let offset = out.len() as u64;
            let obj = if options.compress_streams {
                compress_stream((*obj).clone(), options.compression_level)
            } else {
                (*obj).clone()
            };
            let obj = match &security {
                Some(h) if Some(r) != self.encrypt_ref() => encrypt_for_save(h, obj, r, &mut rng),
                _ => obj,
            };
            out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
            write_object(&fix_stream_length(obj), &mut out);
            out.extend_from_slice(b"\nendobj\n");
            entries.push((
                n,
                XrefEntry::Offset {
                    offset,
                    generation: 0,
                },
            ));
            max_number = max_number.max(n);
        }
        // Flux d'objets (§7.5.7) : paquets de 100 objets, compressés.
        let mut next_number = max_number + 1;
        for chunk in packed.chunks(100) {
            let stream_number = next_number;
            next_number += 1;
            let mut header = Vec::new();
            let mut body = Vec::new();
            for (index, (n, buf)) in chunk.iter().enumerate() {
                header.extend_from_slice(format!("{n} {} ", body.len()).as_bytes());
                body.extend_from_slice(buf);
                body.push(b'\n');
                entries.push((
                    *n,
                    XrefEntry::InStream {
                        stream_number,
                        index: u32::try_from(index).unwrap_or(0),
                    },
                ));
            }
            let first = header.len();
            let mut data = header;
            data.extend_from_slice(&body);
            let raw = acrux_codecs::flate::compress(&data, options.compression_level);
            let mut dict = Dict::new();
            dict.insert(Name::new("Type"), Object::Name(Name::new("ObjStm")));
            dict.insert(
                Name::new("N"),
                Object::Integer(i64::try_from(chunk.len()).unwrap_or(0)),
            );
            dict.insert(
                Name::new("First"),
                Object::Integer(i64::try_from(first).unwrap_or(0)),
            );
            dict.insert(Name::new("Filter"), Object::Name(Name::new("FlateDecode")));
            dict.insert(
                Name::new("Length"),
                Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
            );
            let offset = out.len() as u64;
            out.extend_from_slice(format!("{stream_number} 0 obj\n").as_bytes());
            write_object(&Object::Stream { dict, raw }, &mut out);
            out.extend_from_slice(b"\nendobj\n");
            entries.push((
                stream_number,
                XrefEntry::Offset {
                    offset,
                    generation: 0,
                },
            ));
        }
        let mut trailer = self.trailer();
        for k in [
            "Prev",
            "XRefStm",
            "Filter",
            "DecodeParms",
            "W",
            "Index",
            "Length",
            "Type",
        ] {
            trailer.remove(&Name::new(k));
        }
        // Avec un flux xref, celui-ci porte le dernier numéro.
        let xref_number = next_number;
        let size = if pack {
            xref_number + 1
        } else {
            max_number + 1
        };
        trailer.insert(Name::new("Size"), Object::Integer(i64::from(size)));
        update_id(&mut trailer, &out, &mut rng);
        // Table complète et contiguë : les numéros absents (objets retirés,
        // trous d'origine) deviennent libres, pour qu'aucun lecteur ne
        // prenne un numéro inconnu pour un fichier à réparer.
        let present: std::collections::HashSet<u32> = entries.iter().map(|(n, _)| *n).collect();
        for n in 0..size {
            if !present.contains(&n) {
                entries.push((n, XrefEntry::Free));
            }
        }
        let xref_offset = out.len();
        if pack {
            write_xref_stream(&mut out, xref_number, &entries, &trailer);
        } else {
            write_xref_table(&mut out, &entries, &trailer);
        }
        out.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        Ok(out)
    }
}

/// Options de la réécriture complète.
#[derive(Debug, Clone)]
pub struct SaveOptions {
    /// Compresser (FlateDecode) les flux qui n'ont aucun filtre et dont la
    /// compression fait gagner de la place ; les flux déjà filtrés (images,
    /// flux compressés) sont laissés tels quels.
    pub compress_streams: bool,
    /// Niveau de compression 1..=9.
    pub compression_level: u8,
    /// Regrouper les objets non-flux dans des flux d'objets compressés et
    /// écrire une table xref sous forme de flux (PDF 1.5, §7.5.7-7.5.8) :
    /// fichiers nettement plus petits. Ignoré pour les documents chiffrés.
    pub object_streams: bool,
    /// Omettre les objets qu'aucune référence n'atteint depuis le trailer
    /// (anciennes versions laissées par des enregistrements incrémentaux,
    /// pages supprimées, ressources orphelines).
    pub drop_unreferenced: bool,
}

impl Default for SaveOptions {
    /// Pas de recompression : les octets des flux sont conservés à l'identique.
    fn default() -> Self {
        Self {
            compress_streams: false,
            compression_level: 6,
            object_streams: false,
            drop_unreferenced: false,
        }
    }
}

impl Document {
    /// Numéros des objets atteignables depuis le trailer (`/Root`, `/Info`,
    /// `/Encrypt`…) en suivant toutes les références, y compris à travers
    /// les dictionnaires de flux. Le dictionnaire `/Encrypt` est inclus.
    #[must_use]
    pub fn reachable_objects(&self) -> std::collections::HashSet<u32> {
        use std::collections::HashSet;
        let mut seen: HashSet<u32> = HashSet::new();
        let mut stack: Vec<Object> = Vec::new();
        stack.push(Object::Dict(self.trailer()));
        while let Some(obj) = stack.pop() {
            match obj {
                Object::Reference(r) => {
                    if seen.insert(r.number) {
                        if let Ok(target) = self.get(r) {
                            stack.push((*target).clone());
                        }
                    }
                }
                Object::Array(items) => stack.extend(items),
                Object::Dict(d) | Object::Stream { dict: d, .. } => {
                    stack.extend(d.values().cloned());
                }
                _ => {}
            }
        }
        seen
    }
}

/// Compresse un flux sans filtre si cela le raccourcit.
fn compress_stream(obj: Object, level: u8) -> Object {
    let Object::Stream { mut dict, raw } = obj else {
        return obj;
    };
    let unfiltered =
        !dict.contains_key(&Name::new("Filter")) && !dict.contains_key(&Name::new("F"));
    if !unfiltered || raw.len() < 32 {
        return Object::Stream { dict, raw };
    }
    let packed = acrux_codecs::flate::compress(&raw, level);
    if packed.len() + 24 >= raw.len() {
        return Object::Stream { dict, raw };
    }
    dict.insert(Name::new("Filter"), Object::Name(Name::new("FlateDecode")));
    dict.remove(&Name::new("DecodeParms"));
    Object::Stream { dict, raw: packed }
}

/// Un flux dont les données ont changé doit annoncer la bonne `/Length`.
fn fix_stream_length(obj: Object) -> Object {
    match obj {
        Object::Stream { mut dict, raw } => {
            dict.insert(
                Name::new("Length"),
                Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
            );
            Object::Stream { dict, raw }
        }
        other => other,
    }
}

/// Chiffre récursivement chaînes et flux d'un objet avant écriture.
fn encrypt_for_save(
    h: &crate::crypt::SecurityHandler,
    obj: Object,
    r: ObjectRef,
    rng: &mut dyn Random,
) -> Object {
    match obj {
        Object::String(s) => Object::String(h.encrypt_string(&s, r, &rng.iv())),
        Object::Array(a) => Object::Array(
            a.into_iter()
                .map(|o| encrypt_for_save(h, o, r, rng))
                .collect(),
        ),
        Object::Dict(d) => Object::Dict(encrypt_dict(h, d, r, rng)),
        Object::Stream { dict, raw } => {
            let skip = matches!(
                dict.get(&Name::new("Type"))
                    .and_then(Object::as_name)
                    .map(|n| n.0.as_slice()),
                Some(b"XRef")
            ) || (!h.encrypts_metadata()
                && matches!(
                    dict.get(&Name::new("Type"))
                        .and_then(Object::as_name)
                        .map(|n| n.0.as_slice()),
                    Some(b"Metadata")
                ));
            let raw = if skip {
                raw
            } else {
                h.encrypt_stream(&raw, r, &rng.iv())
            };
            Object::Stream {
                dict: encrypt_dict(h, dict, r, rng),
                raw,
            }
        }
        other => other,
    }
}

fn encrypt_dict(
    h: &crate::crypt::SecurityHandler,
    d: Dict,
    r: ObjectRef,
    rng: &mut dyn Random,
) -> Dict {
    d.into_iter()
        .map(|(k, v)| (k, encrypt_for_save(h, v, r, rng)))
        .collect()
}

/// Met à jour `/ID` : premier élément conservé, second recalculé (§14.4).
fn update_id(trailer: &mut Dict, content: &[u8], rng: &mut dyn Random) {
    let first = match trailer.get(&Name::new("ID")) {
        Some(Object::Array(a)) => match a.first() {
            Some(Object::String(s)) => s.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let mut h = md5::Md5::new();
    h.update(&content[content.len().saturating_sub(4096)..]);
    h.update(&rng.next_u64().to_le_bytes());
    h.update(&(content.len() as u64).to_le_bytes());
    let second = h.finish().to_vec();
    let first = if first.is_empty() {
        second.clone()
    } else {
        first
    };
    trailer.insert(
        Name::new("ID"),
        Object::Array(vec![Object::String(first), Object::String(second)]),
    );
}

/// Table xref classique (§7.5.4) : une sous-section par plage contiguë.
fn write_xref_table(out: &mut Vec<u8>, entries: &[(u32, XrefEntry)], trailer: &Dict) {
    let mut sorted: Vec<(u32, XrefEntry)> = entries.to_vec();
    sorted.sort_by_key(|(n, _)| *n);
    // L'objet 0 (tête de liste libre) doit toujours exister dans une table complète.
    if sorted.first().is_none_or(|(n, _)| *n != 0) && trailer.get(&Name::new("Prev")).is_none() {
        sorted.insert(0, (0, XrefEntry::Free));
    }
    out.extend_from_slice(b"xref\n");
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i].0;
        let mut j = i;
        while j + 1 < sorted.len() && sorted[j + 1].0 == sorted[j].0 + 1 {
            j += 1;
        }
        out.extend_from_slice(format!("{start} {}\n", j - i + 1).as_bytes());
        for (n, e) in &sorted[i..=j] {
            match e {
                XrefEntry::Free => {
                    let gen = if *n == 0 { 65535 } else { 1 };
                    out.extend_from_slice(format!("0000000000 {gen:05} f \n").as_bytes());
                }
                XrefEntry::Offset { offset, generation } => {
                    out.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
                }
                XrefEntry::InStream { .. } => {
                    out.extend_from_slice(b"0000000000 00000 f \n");
                }
            }
        }
        i = j + 1;
    }
    out.extend_from_slice(b"trailer\n");
    write_object(&Object::Dict(trailer.clone()), out);
    out.push(b'\n');
}

/// Flux xref (§7.5.8), non compressé, `/W [1 4 2]`.
fn write_xref_stream(out: &mut Vec<u8>, number: u32, entries: &[(u32, XrefEntry)], trailer: &Dict) {
    let mut sorted: Vec<(u32, XrefEntry)> = entries.to_vec();
    sorted.sort_by_key(|(n, _)| *n);
    let mut index = Vec::new();
    let mut data = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i].0;
        let mut j = i;
        while j + 1 < sorted.len() && sorted[j + 1].0 == sorted[j].0 + 1 {
            j += 1;
        }
        index.push(Object::Integer(i64::from(start)));
        index.push(Object::Integer(i64::try_from(j - i + 1).unwrap_or(0)));
        for (_, e) in &sorted[i..=j] {
            match e {
                XrefEntry::Free => {
                    data.push(0);
                    data.extend_from_slice(&0u32.to_be_bytes());
                    data.extend_from_slice(&0u16.to_be_bytes());
                }
                XrefEntry::Offset { offset, generation } => {
                    data.push(1);
                    data.extend_from_slice(
                        &u32::try_from(*offset).unwrap_or(u32::MAX).to_be_bytes(),
                    );
                    data.extend_from_slice(&generation.to_be_bytes());
                }
                XrefEntry::InStream {
                    stream_number,
                    index: idx,
                } => {
                    data.push(2);
                    data.extend_from_slice(&stream_number.to_be_bytes());
                    data.extend_from_slice(&u16::try_from(*idx).unwrap_or(0).to_be_bytes());
                }
            }
        }
        i = j + 1;
    }
    let mut dict = trailer.clone();
    dict.insert(Name::new("Type"), Object::Name(Name::new("XRef")));
    dict.insert(
        Name::new("W"),
        Object::Array(vec![
            Object::Integer(1),
            Object::Integer(4),
            Object::Integer(2),
        ]),
    );
    dict.insert(Name::new("Index"), Object::Array(index));
    dict.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(data.len()).unwrap_or(0)),
    );
    dict.remove(&Name::new("Filter"));
    dict.remove(&Name::new("DecodeParms"));
    out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
    write_object(&Object::Stream { dict, raw: data }, out);
    out.extend_from_slice(b"\nendobj\n");
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::document::tests::{build_pdf, simple_pdf};
    use crate::pages::collect_pages;

    fn r(n: u32) -> ObjectRef {
        ObjectRef {
            number: n,
            generation: 0,
        }
    }

    #[test]
    fn incremental_save_keeps_original_bytes_and_applies_edits() {
        let original = simple_pdf();
        let doc = Document::from_bytes(original.clone()).unwrap();
        let mut info = doc.info().unwrap();
        info.insert(
            Name::new("Title"),
            Object::String(b"Nouveau titre".to_vec()),
        );
        doc.set(r(6), Object::Dict(info));
        let new_ref = doc.add(Object::Integer(42));
        doc.delete(r(5));
        assert!(doc.is_modified());
        let saved = doc.save_incremental().unwrap();
        assert!(
            saved.starts_with(&original),
            "le fichier d'origine est conservé tel quel"
        );
        let doc2 = Document::from_bytes(saved).unwrap();
        assert!(!doc2.was_repaired());
        assert_eq!(
            doc2.info().unwrap().get(&Name::new("Title")),
            Some(&Object::String(b"Nouveau titre".to_vec()))
        );
        assert_eq!(*doc2.get(new_ref).unwrap(), Object::Integer(42));
        assert_eq!(*doc2.get(r(5)).unwrap(), Object::Null, "objet supprimé");
        assert_eq!(collect_pages(&doc2).unwrap().len(), 1);
        let t = doc2.trailer();
        assert!(t.contains_key(&Name::new("Prev")));
        let ids = t.get(&Name::new("ID")).unwrap().as_array().unwrap();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn full_save_rewrites_cleanly() {
        let doc = Document::from_bytes(simple_pdf()).unwrap();
        doc.set(r(5), Object::Integer(5));
        let saved = doc.save_full().unwrap();
        let doc2 = Document::from_bytes(saved.clone()).unwrap();
        assert!(!doc2.was_repaired());
        assert_eq!(doc2.object_numbers(), vec![1, 2, 3, 4, 5, 6]);
        let c = doc2.get(r(4)).unwrap();
        assert_eq!(doc2.stream_data(&c).unwrap().data, b"BT ET");
        // Deuxième réécriture identique hors /ID.
        let saved2 = doc2.save_full().unwrap();
        assert_eq!(saved.len(), saved2.len());
    }

    #[test]
    fn full_save_expands_object_streams() {
        let inner = "3 0 4 18 << /Type /Page >> << /X 1 >>";
        let objstm = format!(
            "<< /Type /ObjStm /N 2 /First 9 /Length {} >>\nstream\n{inner}\nendstream",
            inner.len()
        );
        let pdf = format!(
            "%PDF-1.5\n1 0 obj << /Type /Catalog /Pages 4 0 R >> endobj\n2 0 obj {objstm} endobj\n"
        );
        let doc = Document::from_bytes(pdf.into_bytes()).unwrap();
        let saved = doc.save_full().unwrap();
        let doc2 = Document::from_bytes(saved).unwrap();
        assert!(!doc2.was_repaired());
        assert_eq!(
            doc2.object_numbers(),
            vec![1, 3, 4],
            "le flux d'objets 2 a disparu"
        );
        assert_eq!(
            doc2.get(r(4))
                .unwrap()
                .as_dict()
                .unwrap()
                .get(&Name::new("X")),
            Some(&Object::Integer(1))
        );
    }

    #[test]
    fn incremental_save_on_xref_stream_file_writes_xref_stream() {
        let rows: Vec<u8> = vec![1, 0, 0, 0, 9, 0, 0];
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.5\n1 0 obj << /Type /Catalog >> endobj\n");
        let xo = pdf.len();
        pdf.extend_from_slice(
            format!("2 0 obj << /Type /XRef /W [1 4 2] /Index [1 1] /Size 3 /Root 1 0 R /Length {} >> stream\n", rows.len()).as_bytes(),
        );
        pdf.extend_from_slice(&rows);
        pdf.extend_from_slice(format!("\nendstream endobj\nstartxref\n{xo}\n%%EOF").as_bytes());
        let doc = Document::from_bytes(pdf).unwrap();
        assert_eq!(doc.xref_kind(), XrefKind::Stream);
        doc.set(
            r(1),
            Object::Dict(Dict::from([
                (Name::new("Type"), Object::Name(Name::new("Catalog"))),
                (Name::new("Version"), Object::Name(Name::new("1.7"))),
            ])),
        );
        let saved = doc.save_incremental().unwrap();
        let doc2 = Document::from_bytes(saved).unwrap();
        assert!(!doc2.was_repaired());
        assert_eq!(doc2.xref_kind(), XrefKind::Stream);
        assert_eq!(doc2.version(), (1, 7));
    }

    #[test]
    fn encrypted_document_roundtrip_both_ways() {
        use crate::crypt::tests::legacy_encrypt_dict;
        // Construit un PDF chiffré AESV2 avec mot de passe utilisateur vide.
        let id0 = b"0123456789abcdef".to_vec();
        let enc = legacy_encrypt_dict(4, 128, b"", b"owner", &id0, true);
        let enc_src = crate::writer::to_string(&Object::Dict(enc.clone()));
        let plain = build_pdf(
            "1.6",
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [] /Count 0 >>"),
                (3, "<< /Title (clair) >>"),
                (4, &enc_src),
            ],
            "/Info 3 0 R /Encrypt 4 0 R /ID [<30313233343536373839616263646566> <30313233343536373839616263646566>]",
        );
        // Ce fichier déclare un chiffrement mais ses chaînes sont en clair :
        // on le charge, on ré-enregistre (chiffré), puis on relit.
        let doc = Document::from_bytes(plain).unwrap();
        assert!(doc.is_encrypted() && !doc.needs_password());
        doc.set(
            r(3),
            Object::Dict(Dict::from([(
                Name::new("Title"),
                Object::String(b"secret".to_vec()),
            )])),
        );
        let saved = doc.save_full().unwrap();
        // La chaîne ne doit pas apparaître en clair.
        assert!(!saved.windows(6).any(|w| w == b"secret"));
        let doc2 = Document::from_bytes(saved).unwrap();
        assert!(!doc2.needs_password());
        assert_eq!(
            doc2.info().unwrap().get(&Name::new("Title")),
            Some(&Object::String(b"secret".to_vec()))
        );
        // Et incrémental par-dessus.
        doc2.set(
            r(3),
            Object::Dict(Dict::from([(
                Name::new("Title"),
                Object::String(b"encore".to_vec()),
            )])),
        );
        let saved2 = doc2.save_incremental().unwrap();
        assert!(!saved2.windows(6).any(|w| w == b"encore"));
        let doc3 = Document::from_bytes(saved2).unwrap();
        assert_eq!(
            doc3.info().unwrap().get(&Name::new("Title")),
            Some(&Object::String(b"encore".to_vec()))
        );
        // Avec le mot de passe propriétaire aussi.
        doc3.authenticate(b"owner").unwrap();
        assert!(doc3.security().unwrap().is_owner());
    }

    #[test]
    fn full_save_can_drop_unreferenced_objects() {
        let src = build_pdf(
            "1.4",
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Resources << /Font << /F1 5 0 R >> >> >>"),
                (4, "<< /Orphelin true >>"),
                (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
                (6, "<< /Length 5 >>
stream
abcde
endstream"),
            ],
            "",
        );
        let doc = Document::from_bytes(src).unwrap();
        let reachable = doc.reachable_objects();
        assert!(reachable.contains(&5) && reachable.contains(&3));
        assert!(!reachable.contains(&4) && !reachable.contains(&6));
        let out = doc
            .save_full_with(&SaveOptions {
                drop_unreferenced: true,
                ..SaveOptions::default()
            })
            .unwrap();
        assert!(!out.windows(8).any(|w| w == b"Orphelin"));
        assert!(!out.windows(5).any(|w| w == b"abcde"));
        let doc2 = Document::from_bytes(out).unwrap();
        assert_eq!(doc2.get(r(4)).unwrap().as_ref(), &Object::Null);
        assert!(doc2.get(r(5)).unwrap().as_dict().is_some());
        assert!(!doc2.was_repaired());
    }

    #[test]
    fn full_save_with_object_streams_reloads_identically() {
        let content = "0 0 m 100 100 l S\n".repeat(50);
        let src = build_pdf(
            "1.4",
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R /Lang (fr) >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>",
                ),
                (
                    4,
                    &format!(
                        "<< /Length {} >>\nstream\n{content}endstream",
                        content.len()
                    ),
                ),
                (5, "<< /Title (Compact) /Author (AK) >>"),
            ],
            "/Info 5 0 R",
        );
        let doc = Document::from_bytes(src).unwrap();
        let compact = doc
            .save_full_with(&SaveOptions {
                compress_streams: true,
                compression_level: 6,
                object_streams: true,
                drop_unreferenced: false,
            })
            .unwrap();
        assert!(compact.starts_with(b"%PDF-1.5"));
        assert!(compact.windows(7).any(|w| w == b"/ObjStm"));
        let doc2 = Document::from_bytes(compact).unwrap();
        assert_eq!(doc2.xref_kind(), XrefKind::Stream);
        assert!(!doc2.was_repaired());
        assert_eq!(
            doc2.info().unwrap().get(&Name::new("Title")),
            Some(&Object::String(b"Compact".to_vec()))
        );
        assert_eq!(
            doc2.catalog().unwrap().get(&Name::new("Lang")),
            Some(&Object::String(b"fr".to_vec()))
        );
        let pages = crate::pages::collect_pages(&doc2).unwrap();
        assert_eq!(pages.len(), 1);
        let stream = doc2.get(r(4)).unwrap();
        assert_eq!(doc2.stream_data(&stream).unwrap().data, content.as_bytes());
        // Rien à réparer et tous les objets se lisent.
        for n in doc2.object_numbers() {
            doc2.get(r(n)).unwrap();
        }
    }

    #[test]
    fn full_save_can_compress_plain_streams() {
        let content = "0 0 m 100 100 l S\n".repeat(200);
        let src = build_pdf(
            "1.4",
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>",
                ),
                (
                    4,
                    &format!(
                        "<< /Length {} >>\nstream\n{content}endstream",
                        content.len()
                    ),
                ),
            ],
            "",
        );
        let doc = Document::from_bytes(src).unwrap();
        let plain = doc.save_full().unwrap();
        let packed = doc
            .save_full_with(&SaveOptions {
                compress_streams: true,
                compression_level: 6,
                object_streams: false,
                drop_unreferenced: false,
            })
            .unwrap();
        assert!(
            packed.len() < plain.len() / 2,
            "{} vs {}",
            packed.len(),
            plain.len()
        );
        let doc2 = Document::from_bytes(packed).unwrap();
        let stream = doc2.get(r(4)).unwrap();
        assert!(
            matches!(&*stream, Object::Stream { dict, .. } if dict.contains_key(&Name::new("Filter")))
        );
        assert_eq!(doc2.stream_data(&stream).unwrap().data, content.as_bytes());
    }

    #[test]
    fn repaired_file_refuses_incremental_but_accepts_full() {
        let doc = Document::from_bytes(b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\n".to_vec())
            .unwrap();
        assert!(doc.save_incremental().is_err());
        let saved = doc.save_full().unwrap();
        let doc2 = Document::from_bytes(saved).unwrap();
        assert!(!doc2.was_repaired());
    }
}
