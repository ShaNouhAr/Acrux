//! Organisation des pages (voir FONCTIONNALITES_ADOBE_ACROBAT.txt §5) :
//! rotation, suppression, extraction, insertion, réordonnancement, fusion.
//!
//! Toutes les opérations travaillent sur l'arbre `/Pages` du catalogue
//! (ISO 32000-2 §7.7.3). Pour rester simples et robustes, elles
//! reconstruisent un arbre plat (un seul nœud `/Pages` racine) : les pages
//! elles-mêmes ne sont pas réécrites, sauf leur `/Parent` et les attributs
//! hérités qui sont rapatriés sur chaque page pour ne rien perdre.

use acrux_core::{Error, Result};
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef, Page};

/// Attributs héritables à rapatrier sur les pages avant de réécrire l'arbre.
const INHERITABLE: [&str; 4] = ["Resources", "MediaBox", "CropBox", "Rotate"];

/// Pages actuelles avec leurs références (les pages directes, sans référence,
/// sont rendues indirectes).
fn page_refs(doc: &Document) -> Result<Vec<(ObjectRef, Page)>> {
    let pages = collect_pages(doc)?;
    let mut out = Vec::with_capacity(pages.len());
    for p in pages {
        let r = match p.reference {
            Some(r) => r,
            None => doc.add(Object::Dict(p.dict.clone())),
        };
        out.push((r, p));
    }
    Ok(out)
}

/// Réécrit l'arbre des pages avec, dans l'ordre, les pages données. Chaque
/// page reçoit ses attributs hérités et un `/Parent` vers la nouvelle racine.
fn rebuild_tree(doc: &Document, pages: &[(ObjectRef, Page)]) -> Result<()> {
    let catalog = doc.catalog()?;
    let root_ref = match catalog.get(&Name::new("Pages")) {
        Some(Object::Reference(r)) => *r,
        _ => doc.allocate(),
    };
    let mut kids = Vec::with_capacity(pages.len());
    for (r, p) in pages {
        let mut d = p.dict.clone();
        for key in INHERITABLE {
            if let Some(v) = p.dict.get(&Name::new(key)) {
                d.insert(Name::new(key), v.clone());
            }
        }
        d.insert(Name::new("Type"), Object::Name(Name::new("Page")));
        d.insert(Name::new("Parent"), Object::Reference(root_ref));
        doc.set(*r, Object::Dict(d));
        kids.push(Object::Reference(*r));
    }
    let mut root = Dict::new();
    root.insert(Name::new("Type"), Object::Name(Name::new("Pages")));
    root.insert(Name::new("Kids"), Object::Array(kids));
    root.insert(
        Name::new("Count"),
        Object::Integer(i64::try_from(pages.len()).unwrap_or(0)),
    );
    doc.set(root_ref, Object::Dict(root));
    let mut catalog = catalog;
    catalog.insert(Name::new("Pages"), Object::Reference(root_ref));
    let cat_ref = acrux_document::xref::trailer_ref(&doc.trailer(), "Root")
        .ok_or_else(|| Error::Corrupt("trailer sans /Root".into()))?;
    doc.set(cat_ref, Object::Dict(catalog));
    Ok(())
}

/// Fait pivoter les pages d'index donnés (0 = première) de `degrees`
/// (multiple de 90, positif = sens horaire), en cumulant avec `/Rotate`.
///
/// # Errors
/// Arbre des pages illisible ou angle invalide.
pub fn rotate_pages(doc: &Document, indices: &[usize], degrees: i32) -> Result<()> {
    if degrees % 90 != 0 {
        return Err(Error::Corrupt("l'angle doit être un multiple de 90".into()));
    }
    let pages = page_refs(doc)?;
    for &i in indices {
        let Some((r, p)) = pages.get(i) else {
            return Err(Error::Corrupt(format!("page {} inexistante", i + 1)));
        };
        let current = p.rotate(doc);
        let mut d = p.dict.clone();
        d.insert(
            Name::new("Rotate"),
            Object::Integer(i64::from((current + degrees).rem_euclid(360))),
        );
        doc.set(*r, Object::Dict(d));
    }
    Ok(())
}

/// Supprime les pages d'index donnés.
///
/// # Errors
/// Index invalide, ou tentative de supprimer toutes les pages.
pub fn delete_pages(doc: &Document, indices: &[usize]) -> Result<()> {
    let pages = page_refs(doc)?;
    for &i in indices {
        if i >= pages.len() {
            return Err(Error::Corrupt(format!("page {} inexistante", i + 1)));
        }
    }
    let kept: Vec<(ObjectRef, Page)> = pages
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !indices.contains(i))
        .map(|(_, p)| p)
        .collect();
    if kept.is_empty() {
        return Err(Error::Corrupt(
            "un document doit garder au moins une page".into(),
        ));
    }
    rebuild_tree(doc, &kept)
}

/// Réordonne les pages : `order` donne, pour chaque position, l'index actuel.
/// Une page absente de `order` est supprimée ; un index répété duplique la
/// page (même objet partagé, comme le fait Acrobat pour « dupliquer »).
///
/// # Errors
/// Index invalide ou ordre vide.
pub fn reorder_pages(doc: &Document, order: &[usize]) -> Result<()> {
    let pages = page_refs(doc)?;
    if order.is_empty() {
        return Err(Error::Corrupt("ordre vide".into()));
    }
    let mut seen = std::collections::HashSet::new();
    let mut new_pages = Vec::with_capacity(order.len());
    for &i in order {
        let Some((r, p)) = pages.get(i) else {
            return Err(Error::Corrupt(format!("page {} inexistante", i + 1)));
        };
        if seen.insert(*r) {
            new_pages.push((*r, p.clone()));
        } else {
            // Duplication : une copie indirecte distincte pour garder un /Parent unique.
            let copy = doc.add(Object::Dict(p.dict.clone()));
            new_pages.push((copy, p.clone()));
        }
    }
    rebuild_tree(doc, &new_pages)
}

/// Copie profonde d'un objet d'un document vers un autre : les références
/// sont suivies et réallouées, avec mémorisation pour partager les ressources
/// communes (polices, images) et gérer les cycles.
pub struct Importer<'a> {
    src: &'a Document,
    dst: &'a Document,
    map: std::collections::HashMap<u32, ObjectRef>,
}

impl<'a> Importer<'a> {
    /// Nouvel importeur de `src` vers `dst`.
    #[must_use]
    pub fn new(src: &'a Document, dst: &'a Document) -> Self {
        Self {
            src,
            dst,
            map: std::collections::HashMap::new(),
        }
    }

    /// Importe un objet (les références indirectes sont copiées récursivement).
    ///
    /// # Errors
    /// Objet source illisible.
    pub fn import(&mut self, obj: &Object) -> Result<Object> {
        self.import_depth(obj, 0)
    }

    fn import_depth(&mut self, obj: &Object, depth: usize) -> Result<Object> {
        if depth > 256 {
            return Err(Error::Corrupt("objets imbriqués trop profondément".into()));
        }
        Ok(match obj {
            Object::Reference(r) => {
                if let Some(m) = self.map.get(&r.number) {
                    return Ok(Object::Reference(*m));
                }
                let new_ref = self.dst.allocate();
                self.map.insert(r.number, new_ref);
                let target = self.src.get(*r)?;
                let copied = self.import_depth(&target, depth + 1)?;
                self.dst.set(new_ref, copied);
                Object::Reference(new_ref)
            }
            Object::Array(a) => Object::Array(
                a.iter()
                    .map(|o| self.import_depth(o, depth + 1))
                    .collect::<Result<Vec<_>>>()?,
            ),
            Object::Dict(d) => Object::Dict(self.import_dict(d, depth)?),
            Object::Stream { dict, raw } => {
                // Le flux est copié décodé de tout chiffrement mais avec ses filtres :
                // `raw` est déjà déchiffré par `Document::get`.
                Object::Stream {
                    dict: self.import_dict(dict, depth)?,
                    raw: raw.clone(),
                }
            }
            other => other.clone(),
        })
    }

    fn import_dict(&mut self, d: &Dict, depth: usize) -> Result<Dict> {
        let mut out = Dict::new();
        for (k, v) in d {
            // `/Parent` des pages est réécrit par `rebuild_tree` ; on l'ignore pour
            // ne pas importer tout l'arbre source.
            if k.0 == b"Parent" {
                continue;
            }
            out.insert(k.clone(), self.import_depth(v, depth + 1)?);
        }
        Ok(out)
    }
}

/// Insère dans `dst`, à la position `at` (0 = avant la première page), les
/// pages d'index `indices` de `src`. Ressources, annotations et contenus
/// sont copiés ; les champs de formulaire (`/AcroForm`) ne sont pas fusionnés.
///
/// # Errors
/// Index ou position invalide, objets illisibles.
pub fn insert_pages_from(
    dst: &Document,
    src: &Document,
    indices: &[usize],
    at: usize,
) -> Result<()> {
    let src_pages = collect_pages(src)?;
    let dst_pages = page_refs(dst)?;
    if at > dst_pages.len() {
        return Err(Error::Corrupt(format!("position {at} hors du document")));
    }
    let mut importer = Importer::new(src, dst);
    let mut new_pages = Vec::with_capacity(indices.len());
    for &i in indices {
        let Some(p) = src_pages.get(i) else {
            return Err(Error::Corrupt(format!("page source {} inexistante", i + 1)));
        };
        let dict = importer.import(&Object::Dict(p.dict.clone()))?;
        let Object::Dict(dict) = dict else {
            unreachable!("un dictionnaire reste un dictionnaire")
        };
        let r = dst.add(Object::Dict(dict.clone()));
        new_pages.push((
            r,
            Page {
                index: 0,
                reference: Some(r),
                dict,
            },
        ));
    }
    let mut all = dst_pages;
    let tail = all.split_off(at);
    all.extend(new_pages);
    all.extend(tail);
    rebuild_tree(dst, &all)
}

/// Nouveau document ne contenant que les pages d'index `indices` de `src`,
/// dans cet ordre (extraction). Les métadonnées `/Info` sont copiées.
///
/// # Errors
/// Index invalide ou objets illisibles.
pub fn extract_pages(src: &Document, indices: &[usize]) -> Result<Document> {
    let (major, minor) = src.version();
    let skeleton = format!(
        "%PDF-{major}.{minor}\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\nxref\n0 3\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n0\n%%EOF\n"
    );
    // Les offsets ci-dessus sont approximatifs : le chargeur répare et la
    // sauvegarde complète réécrit tout proprement.
    let dst = Document::from_bytes(skeleton.into_bytes())?;
    if let Some(info) = src.info() {
        let mut importer = Importer::new(src, &dst);
        let copied = importer.import(&Object::Dict(info))?;
        let r = dst.add(copied);
        dst.set_trailer_entry("Info", Object::Reference(r));
    }
    let src_pages = collect_pages(src)?;
    let mut importer = Importer::new(src, &dst);
    let mut pages = Vec::with_capacity(indices.len());
    for &i in indices {
        let Some(p) = src_pages.get(i) else {
            return Err(Error::Corrupt(format!("page {} inexistante", i + 1)));
        };
        let Object::Dict(dict) = importer.import(&Object::Dict(p.dict.clone()))? else {
            unreachable!("un dictionnaire reste un dictionnaire")
        };
        let r = dst.add(Object::Dict(dict.clone()));
        pages.push((
            r,
            Page {
                index: 0,
                reference: Some(r),
                dict,
            },
        ));
    }
    if pages.is_empty() {
        return Err(Error::Corrupt("aucune page à extraire".into()));
    }
    rebuild_tree(&dst, &pages)?;
    Ok(dst)
}

/// Fusionne plusieurs documents en un nouveau document (toutes leurs pages, dans l'ordre).
///
/// # Errors
/// Documents illisibles.
pub fn merge(docs: &[&Document]) -> Result<Document> {
    let Some(first) = docs.first() else {
        return Err(Error::Corrupt("rien à fusionner".into()));
    };
    let n = collect_pages(first)?.len();
    let all: Vec<usize> = (0..n).collect();
    let out = extract_pages(first, &all)?;
    for d in &docs[1..] {
        let n = collect_pages(d)?.len();
        let all: Vec<usize> = (0..n).collect();
        let at = collect_pages(&out)?.len();
        insert_pages_from(&out, d, &all, at)?;
    }
    Ok(out)
}

/// Analyse une spécification de pages « 1,3-5,8 » (1 = première) en indices 0-based.
///
/// # Errors
/// Syntaxe invalide ou page hors document.
pub fn parse_page_spec(spec: &str, page_count: usize) -> Result<Vec<usize>> {
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (part, part),
        };
        let a: usize = a
            .parse()
            .map_err(|_| Error::Corrupt(format!("page invalide : {part}")))?;
        let b: usize = if b.is_empty() {
            page_count
        } else {
            b.parse()
                .map_err(|_| Error::Corrupt(format!("page invalide : {part}")))?
        };
        if a == 0 || b == 0 || a > page_count || b > page_count {
            return Err(Error::Corrupt(format!(
                "page hors document : {part} (1 à {page_count})"
            )));
        }
        if a <= b {
            out.extend(a - 1..b);
        } else {
            out.extend((b - 1..a).rev());
        }
    }
    if out.is_empty() {
        return Err(Error::Corrupt("aucune page indiquée".into()));
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn pdf_with_pages(n: usize) -> Document {
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            String::new(),
        ];
        let mut kids = Vec::new();
        for i in 0..n {
            let num = 3 + i;
            kids.push(format!("{num} 0 R"));
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} 100] >>",
                100 + i
            ));
        }
        objects[1] = format!(
            "<< /Type /Pages /Kids [{}] /Count {n} /Rotate 90 >>",
            kids.join(" ")
        );
        let mut src = b"%PDF-1.4\n".to_vec();
        for (i, body) in objects.iter().enumerate() {
            src.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        Document::from_bytes(src).unwrap()
    }

    fn widths(doc: &Document) -> Vec<f64> {
        collect_pages(doc)
            .unwrap()
            .iter()
            .map(|p| p.media_box(doc).width())
            .collect()
    }

    fn roundtrip(doc: &Document) -> Document {
        Document::from_bytes(doc.save_full().unwrap()).unwrap()
    }

    #[test]
    fn rotate_and_delete() {
        let doc = pdf_with_pages(3);
        rotate_pages(&doc, &[0], 90).unwrap();
        rotate_pages(&doc, &[1], -90).unwrap();
        let d = roundtrip(&doc);
        let pages = collect_pages(&d).unwrap();
        assert_eq!(pages[0].rotate(&d), 180, "90 hérité + 90");
        assert_eq!(pages[1].rotate(&d), 0);
        assert_eq!(pages[2].rotate(&d), 90);
        delete_pages(&d, &[1]).unwrap();
        let d = roundtrip(&d);
        assert_eq!(widths(&d), vec![100.0, 102.0]);
        assert_eq!(
            collect_pages(&d).unwrap()[1].rotate(&d),
            90,
            "attribut hérité conservé après réécriture"
        );
        assert!(delete_pages(&d, &[0, 1]).is_err());
        assert!(rotate_pages(&d, &[0], 45).is_err());
    }

    #[test]
    fn reorder_with_duplicate() {
        let doc = pdf_with_pages(3);
        reorder_pages(&doc, &[2, 0, 2]).unwrap();
        let d = roundtrip(&doc);
        assert_eq!(widths(&d), vec![102.0, 100.0, 102.0]);
        assert!(reorder_pages(&d, &[5]).is_err());
    }

    #[test]
    fn extract_insert_and_merge() {
        let a = pdf_with_pages(2);
        let b = pdf_with_pages(3);
        let ex = extract_pages(&b, &[2, 0]).unwrap();
        let ex = roundtrip(&ex);
        assert_eq!(widths(&ex), vec![102.0, 100.0]);
        insert_pages_from(&a, &b, &[1], 1).unwrap();
        let a = roundtrip(&a);
        assert_eq!(widths(&a), vec![100.0, 101.0, 101.0]);
        let m = merge(&[&a, &b]).unwrap();
        let m = roundtrip(&m);
        assert_eq!(widths(&m), vec![100.0, 101.0, 101.0, 100.0, 101.0, 102.0]);
        assert!(!m.was_repaired());
    }

    #[test]
    fn shared_resources_are_imported_once() {
        // Deux pages partagent la même ressource indirecte.
        let src = b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /Resources 5 0 R >> endobj\n4 0 obj << /Type /Page /Parent 2 0 R /Resources 5 0 R >> endobj\n5 0 obj << /Font << >> >> endobj\n".to_vec();
        let src = Document::from_bytes(src).unwrap();
        let out = extract_pages(&src, &[0, 1]).unwrap();
        let out = roundtrip(&out);
        let pages = collect_pages(&out).unwrap();
        let r0 = pages[0].dict.get(&Name::new("Resources")).unwrap();
        let r1 = pages[1].dict.get(&Name::new("Resources")).unwrap();
        assert_eq!(r0, r1, "ressource partagée importée une seule fois");
    }

    #[test]
    fn page_spec() {
        assert_eq!(parse_page_spec("1,3-5,8", 10).unwrap(), vec![0, 2, 3, 4, 7]);
        assert_eq!(parse_page_spec("5-3", 10).unwrap(), vec![4, 3, 2]);
        assert_eq!(parse_page_spec("9-", 10).unwrap(), vec![8, 9]);
        assert!(parse_page_spec("0", 10).is_err());
        assert!(parse_page_spec("11", 10).is_err());
        assert!(parse_page_spec("a", 10).is_err());
        assert!(parse_page_spec("", 10).is_err());
    }
}
