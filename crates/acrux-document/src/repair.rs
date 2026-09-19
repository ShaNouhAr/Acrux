//! Reconstruction de la table xref par balayage du fichier.
//!
//! Utilisée quand `startxref` manque, quand une table est illisible ou quand
//! un offset ne pointe pas sur le bon objet. C'est ce que fait Acrobat
//! silencieusement sur une grande partie des PDF « cassés » en circulation.
//!
//! Principe : on cherche chaque occurrence de `N G obj`, on retient la
//! dernière position de chaque numéro (les mises à jour incrémentales
//! ajoutent en fin de fichier), puis on retrouve le trailer (`trailer` ou
//! flux `/Type /XRef`) et, à défaut, le catalogue (`/Type /Catalog`).

use std::collections::HashMap;

use crate::lexer::{is_regular, is_whitespace, Lexer, Token};
use crate::objects::{Dict, Name, Object, ObjectRef};
use crate::parser::Parser;
use crate::xref::{StreamDecoder, Xref, XrefEntry, XrefKind};

/// Reconstruit une [`Xref`] à partir du contenu brut du fichier.
///
/// Ne retourne jamais d'erreur : au pire la table est vide et le trailer
/// sans `/Root`, ce que l'appelant signalera.
#[must_use]
#[allow(clippy::too_many_lines)] // quatre étapes séquentielles, plus lisibles ensemble
pub fn reconstruct(data: &[u8], decode: StreamDecoder<'_>) -> Xref {
    let mut xref = Xref::empty(XrefKind::Reconstructed);
    let offsets = scan_objects(data);
    for (&number, &(offset, generation)) in &offsets {
        xref.entries.insert(
            number,
            XrefEntry::Offset {
                offset: offset as u64,
                generation,
            },
        );
    }

    // 1. Trailers classiques : le dernier qui contient /Root l'emporte, mais
    //    on récupère aussi les clés manquantes (/Info, /ID…) des précédents.
    let mut trailer = Dict::new();
    let mut pos = 0;
    while let Some(t) = Lexer::at(data, pos).find(b"trailer") {
        pos = t + 7;
        let mut p = Parser::at(data, pos);
        if let Ok(Object::Dict(d)) = p.parse_object() {
            for (k, v) in d {
                trailer.insert(k, v);
            }
        }
    }

    // 2. Flux xref et flux d'objets : on parcourt les objets trouvés.
    let mut catalog: Option<ObjectRef> = None;
    let mut object_streams: Vec<(u32, usize)> = Vec::new();
    let mut numbers: Vec<u32> = offsets.keys().copied().collect();
    numbers.sort_unstable();
    for &number in &numbers {
        let (offset, _) = offsets[&number];
        let mut p = Parser::at(data, offset);
        let Ok((r, obj)) = p.parse_indirect(&|_| None) else {
            continue;
        };
        let Some(d) = obj.as_dict() else { continue };
        match d
            .get(&Name::new("Type"))
            .and_then(Object::as_name)
            .map(|n| n.0.as_slice())
        {
            Some(b"XRef") => {
                for key in ["Root", "Info", "Encrypt", "ID"] {
                    if let Some(v) = d.get(&Name::new(key)) {
                        trailer.insert(Name::new(key), v.clone());
                    }
                }
            }
            Some(b"Catalog") => catalog = Some(r),
            Some(b"ObjStm") => object_streams.push((r.number, offset)),
            _ => {}
        }
    }

    // 3. Objets compressés : ils ne sont visibles qu'en ouvrant les flux d'objets.
    //    Un objet trouvé directement dans le fichier a priorité (il est plus récent
    //    dans la grande majorité des fichiers réparés).
    for (stream_number, offset) in object_streams {
        let mut p = Parser::at(data, offset);
        let Ok((_, Object::Stream { dict, raw })) = p.parse_indirect(&|_| None) else {
            continue;
        };
        let Ok(content) = decode(&dict, &raw) else {
            continue;
        };
        let count = dict
            .get(&Name::new("N"))
            .and_then(Object::as_i64)
            .unwrap_or(0);
        let mut lx = Lexer::new(&content);
        for index in 0..count.max(0) {
            let (Ok(Token::Integer(n)), Ok(Token::Integer(_))) = (lx.next_token(), lx.next_token())
            else {
                break;
            };
            let Ok(n) = u32::try_from(n) else { continue };
            xref.entries.entry(n).or_insert(XrefEntry::InStream {
                stream_number,
                index: u32::try_from(index).unwrap_or(u32::MAX),
            });
            if catalog.is_none() {
                // Le catalogue peut être compressé : on regarde son type.
                if let Some(off) = object_offset_in_stream(&content, &dict, index) {
                    if let Ok(Object::Dict(d)) = Parser::at(&content, off).parse_object() {
                        if d.get(&Name::new("Type")).and_then(Object::as_name)
                            == Some(&Name::new("Catalog"))
                        {
                            catalog = Some(ObjectRef {
                                number: n,
                                generation: 0,
                            });
                        }
                    }
                }
            }
        }
    }

    // 4. /Root : celui du trailer s'il désigne un objet connu, sinon le catalogue trouvé.
    let root_ok = match trailer.get(&Name::new("Root")) {
        Some(Object::Reference(r)) => xref.entries.contains_key(&r.number),
        _ => false,
    };
    if !root_ok {
        if let Some(c) = catalog {
            trailer.insert(Name::new("Root"), Object::Reference(c));
        } else {
            trailer.remove(&Name::new("Root"));
            xref.warnings.push("aucun catalogue trouvé".into());
        }
    }
    let size = xref.entries.keys().max().map_or(0, |m| i64::from(*m) + 1);
    trailer.insert(Name::new("Size"), Object::Integer(size));
    xref.trailer = trailer;
    xref
}

/// Offset, dans les données décodées d'un flux d'objets, de l'objet d'index `index`.
#[must_use]
pub fn object_offset_in_stream(content: &[u8], dict: &Dict, index: i64) -> Option<usize> {
    let first = usize::try_from(dict.get(&Name::new("First")).and_then(Object::as_i64)?).ok()?;
    let mut lx = Lexer::new(content);
    let mut off = None;
    for _ in 0..=index {
        let (Ok(Token::Integer(_)), Ok(Token::Integer(o))) = (lx.next_token(), lx.next_token())
        else {
            return None;
        };
        if lx.pos() > first {
            return None; // l'en-tête déborde sur les objets : flux corrompu
        }
        off = usize::try_from(o).ok();
    }
    off.map(|o| first.saturating_add(o))
}

/// Balaye le fichier à la recherche de `N G obj` et retourne, pour chaque
/// numéro, l'offset du début de l'objet et sa génération (dernière occurrence).
#[must_use]
pub fn scan_objects(data: &[u8]) -> HashMap<u32, (usize, u16)> {
    let mut found = HashMap::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if &data[i..i + 3] != b"obj" {
            i += 1;
            continue;
        }
        // `obj` doit être suivi d'un caractère non régulier (ou de la fin).
        if data.get(i + 3).is_some_and(|&b| is_regular(b)) {
            i += 3;
            continue;
        }
        if let Some((start, number, generation)) = header_before(data, i) {
            found.insert(number, (start, generation));
        }
        i += 3;
    }
    found
}

/// Si `N G` précède immédiatement `obj` à `obj_pos`, retourne (offset de N, N, G).
fn header_before(data: &[u8], obj_pos: usize) -> Option<(usize, u32, u16)> {
    let mut j = obj_pos;
    // Espacement(s) entre G et obj.
    let mut n_ws = 0;
    while j > 0 && is_whitespace(data[j - 1]) {
        j -= 1;
        n_ws += 1;
    }
    if n_ws == 0 {
        return None;
    }
    let gen_end = j;
    while j > 0 && data[j - 1].is_ascii_digit() {
        j -= 1;
    }
    if j == gen_end {
        return None;
    }
    let gen_start = j;
    n_ws = 0;
    while j > 0 && is_whitespace(data[j - 1]) {
        j -= 1;
        n_ws += 1;
    }
    if n_ws == 0 {
        return None;
    }
    let num_end = j;
    while j > 0 && data[j - 1].is_ascii_digit() {
        j -= 1;
    }
    if j == num_end {
        return None;
    }
    // Le numéro doit être précédé d'un caractère non régulier (sinon `12 0 obj`
    // serait vu dans `112 0 obj` : mêmes chiffres, mais on veut le vrai début).
    if j > 0 && is_regular(data[j - 1]) {
        return None;
    }
    let number: u32 = std::str::from_utf8(&data[j..num_end]).ok()?.parse().ok()?;
    let generation: u16 = std::str::from_utf8(&data[gen_start..gen_end])
        .ok()?
        .parse::<u32>()
        .ok()?
        .min(65535)
        .try_into()
        .ok()?;
    Some((j, number, generation))
}

#[cfg(test)]
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::unnecessary_wraps,
    clippy::format_collect
)]
mod tests {
    use super::*;
    use acrux_core::Result;

    fn identity(_: &Dict, raw: &[u8]) -> Result<Vec<u8>> {
        Ok(raw.to_vec())
    }

    #[test]
    fn scan_finds_last_occurrence_and_ignores_lookalikes() {
        let src = b"%PDF-1.4\n1 0 obj << >> endobj\n12 0 obj 5 endobj\nx112 0 obj\n1 0 obj 7 endobj\nxobj 3 0obj 4 0 objx";
        let m = scan_objects(src);
        assert_eq!(m.len(), 2, "{m:?}");
        let (off12, _) = m[&12];
        assert_eq!(&src[off12..off12 + 8], b"12 0 obj");
        let (off, g) = m[&1];
        assert_eq!(&src[off..off + 7], b"1 0 obj");
        assert_eq!(g, 0);
        assert!(off > off12, "la dernière occurrence de 1 0 obj gagne");
    }

    #[test]
    fn reconstruct_without_xref_finds_catalog() {
        let src = b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\n";
        let x = reconstruct(src, &identity);
        assert_eq!(x.kind, XrefKind::Reconstructed);
        assert_eq!(x.entries.len(), 2);
        assert_eq!(
            x.trailer.get(&Name::new("Root")),
            Some(&Object::Reference(ObjectRef {
                number: 1,
                generation: 0
            }))
        );
        assert_eq!(x.trailer.get(&Name::new("Size")), Some(&Object::Integer(3)));
    }

    #[test]
    fn reconstruct_uses_trailer_and_xref_stream_keys() {
        let src = b"1 0 obj << /Type /Catalog >> endobj\n3 0 obj << /Type /XRef /Info 9 0 R /Root 1 0 R /Length 0 >> stream\n\nendstream endobj\ntrailer << /Root 1 0 R /ID [<01> <02>] >>";
        let x = reconstruct(src, &identity);
        assert!(x.trailer.contains_key(&Name::new("ID")));
        assert!(x.trailer.contains_key(&Name::new("Info")));
    }

    #[test]
    fn reconstruct_expands_object_streams() {
        // Flux d'objets non compressé contenant l'objet 5 (un catalogue) et l'objet 6.
        let inner =
            b"5 0 6 27 << /Type /Catalog /Pages 6 0 R >> << /Type /Pages /Count 0 /Kids [] >>";
        let mut src = Vec::new();
        src.extend_from_slice(
            format!(
                "4 0 obj << /Type /ObjStm /N 2 /First 9 /Length {} >> stream\n",
                inner.len()
            )
            .as_bytes(),
        );
        src.extend_from_slice(inner);
        src.extend_from_slice(b"\nendstream endobj\n");
        let x = reconstruct(&src, &identity);
        assert_eq!(
            x.get(5),
            Some(XrefEntry::InStream {
                stream_number: 4,
                index: 0
            })
        );
        assert_eq!(
            x.get(6),
            Some(XrefEntry::InStream {
                stream_number: 4,
                index: 1
            })
        );
        assert_eq!(
            x.trailer.get(&Name::new("Root")),
            Some(&Object::Reference(ObjectRef {
                number: 5,
                generation: 0
            }))
        );
    }

    #[test]
    fn no_catalog_yields_warning() {
        let x = reconstruct(b"1 0 obj 5 endobj", &identity);
        assert!(!x.trailer.contains_key(&Name::new("Root")));
        assert_eq!(x.warnings.len(), 1);
    }
}
