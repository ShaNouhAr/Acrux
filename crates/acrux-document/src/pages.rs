//! Arbre des pages (ISO 32000-2 §7.7.3) : parcours des nœuds `/Pages`,
//! héritage des attributs (`/Resources`, `/MediaBox`, `/CropBox`, `/Rotate`)
//! et repli par balayage quand l'arbre est cassé.

use std::collections::HashSet;

use acrux_core::{Rect, Result};

use crate::document::Document;
use crate::objects::{Dict, Name, Object, ObjectRef};

/// Attributs héritables (§7.7.3.4, table 31).
const INHERITABLE: [&str; 4] = ["Resources", "MediaBox", "CropBox", "Rotate"];

/// Une page du document, attributs hérités déjà fusionnés.
#[derive(Debug, Clone)]
pub struct Page {
    /// Index (0 = première page).
    pub index: usize,
    /// Référence de l'objet page, si la page est un objet indirect.
    pub reference: Option<ObjectRef>,
    /// Dictionnaire de la page, complété par les attributs hérités.
    pub dict: Dict,
}

impl Page {
    /// `/MediaBox`, ou Letter (612 × 792 pt) si absent ou invalide (comportement d'Acrobat).
    #[must_use]
    pub fn media_box(&self, doc: &Document) -> Rect {
        self.rect(doc, "MediaBox")
            .filter(|r| !r.is_empty() && r.width() <= 14_400.0 && r.height() <= 14_400.0)
            .unwrap_or_else(|| Rect::new(0.0, 0.0, 612.0, 792.0))
    }

    /// `/CropBox` intersecté avec la `MediaBox` (§7.7.3.3), sinon la `MediaBox`.
    #[must_use]
    pub fn crop_box(&self, doc: &Document) -> Rect {
        let media = self.media_box(doc);
        match self.rect(doc, "CropBox") {
            Some(c) => {
                let i = c.intersect(&media);
                if i.is_empty() {
                    media
                } else {
                    i
                }
            }
            None => media,
        }
    }

    /// Rotation d'affichage en degrés, normalisée à 0, 90, 180 ou 270.
    #[must_use]
    pub fn rotate(&self, doc: &Document) -> i32 {
        let raw = self
            .dict
            .get(&Name::new("Rotate"))
            .and_then(|o| doc.resolve(o).ok())
            .and_then(|o| o.as_f64())
            .unwrap_or(0.0);
        #[allow(clippy::cast_possible_truncation)]
        let r = (raw / 90.0).round() as i32 * 90;
        r.rem_euclid(360)
    }

    fn rect(&self, doc: &Document, key: &str) -> Option<Rect> {
        let obj = self.dict.get(&Name::new(key))?;
        let arr = doc.resolve(obj).ok()?;
        let arr = arr.as_array()?;
        if arr.len() != 4 {
            return None;
        }
        let mut v = [0.0; 4];
        for (i, o) in arr.iter().enumerate() {
            v[i] = doc.resolve(o).ok()?.as_f64()?;
        }
        Some(Rect::new(v[0], v[1], v[2], v[3]))
    }
}

/// Toutes les pages, dans l'ordre de lecture.
///
/// # Errors
/// Catalogue illisible. Un arbre de pages cassé n'est pas une erreur : on
/// se replie sur un balayage de tous les objets `/Type /Page`.
pub fn collect_pages(doc: &Document) -> Result<Vec<Page>> {
    let catalog = doc.catalog()?;
    let mut pages = Vec::new();
    if let Some(root) = catalog.get(&Name::new("Pages")) {
        let mut visited = HashSet::new();
        walk(doc, root, &Dict::new(), &mut pages, &mut visited, 0);
    }
    if pages.is_empty() {
        pages = scan_for_pages(doc);
    }
    for (i, p) in pages.iter_mut().enumerate() {
        p.index = i;
    }
    Ok(pages)
}

fn walk(
    doc: &Document,
    node: &Object,
    inherited: &Dict,
    out: &mut Vec<Page>,
    visited: &mut HashSet<u32>,
    depth: usize,
) {
    if depth > 64 || out.len() > 500_000 {
        return;
    }
    let reference = match node {
        Object::Reference(r) => {
            if !visited.insert(r.number) {
                return; // cycle
            }
            Some(*r)
        }
        _ => None,
    };
    let Ok(resolved) = doc.resolve(node) else {
        return;
    };
    let Some(dict) = resolved.as_dict() else {
        return;
    };

    let kind = dict
        .get(&Name::new("Type"))
        .and_then(Object::as_name)
        .map(|n| n.0.clone());
    let kids = dict
        .get(&Name::new("Kids"))
        .and_then(|k| doc.resolve(k).ok());
    let is_pages = kind.as_deref() == Some(b"Pages") || (kind.is_none() && kids.is_some());

    // Attributs hérités : ceux du nœud écrasent ceux du parent.
    let mut merged = inherited.clone();
    for key in INHERITABLE {
        if let Some(v) = dict.get(&Name::new(key)) {
            merged.insert(Name::new(key), v.clone());
        }
    }

    if is_pages {
        if let Some(kids) = kids {
            if let Some(kids) = kids.as_array() {
                for kid in kids {
                    walk(doc, kid, &merged, out, visited, depth + 1);
                }
            }
        }
        return;
    }
    // Feuille : `/Type /Page`, ou nœud sans type mais avec du contenu (tolérance).
    if kind.as_deref() == Some(b"Page")
        || kind.is_none() && dict.contains_key(&Name::new("Contents"))
    {
        let mut full = dict.clone();
        for (k, v) in merged {
            full.entry(k).or_insert(v);
        }
        out.push(Page {
            index: 0,
            reference,
            dict: full,
        });
    }
}

/// Repli : toutes les pages trouvées par balayage des objets, triées par numéro.
fn scan_for_pages(doc: &Document) -> Vec<Page> {
    let mut pages = Vec::new();
    for n in doc.object_numbers() {
        let r = ObjectRef {
            number: n,
            generation: 0,
        };
        let Ok(o) = doc.get(r) else { continue };
        let Some(d) = o.as_dict() else { continue };
        if d.get(&Name::new("Type")).and_then(Object::as_name) == Some(&Name::new("Page")) {
            // Héritage remonté via /Parent.
            let mut full = d.clone();
            let mut parent = d.get(&Name::new("Parent")).cloned();
            let mut guard = 0;
            while let Some(p) = parent {
                guard += 1;
                if guard > 64 {
                    break;
                }
                let Ok(pd) = doc.resolve(&p) else { break };
                let Some(pd) = pd.as_dict() else { break };
                for key in INHERITABLE {
                    if let Some(v) = pd.get(&Name::new(key)) {
                        full.entry(Name::new(key)).or_insert_with(|| v.clone());
                    }
                }
                parent = pd.get(&Name::new("Parent")).cloned();
            }
            pages.push(Page {
                index: 0,
                reference: Some(r),
                dict: full,
            });
        }
    }
    pages
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::document::tests::build_pdf;

    #[test]
    fn inheritance_and_boxes() {
        let pdf = build_pdf(
            "1.4",
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 /MediaBox [0 0 200 100] /Rotate 90 >>"),
                (3, "<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 /Rotate -90 >>"),
                (4, "<< /Type /Page /Parent 3 0 R /CropBox [10 10 500 50] >>"),
                (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Rotate 450 >>"),
            ],
            "",
        );
        let doc = Document::from_bytes(pdf).unwrap();
        let pages = collect_pages(&doc).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].index, 0);
        assert_eq!(pages[0].reference.unwrap().number, 4);
        assert_eq!(pages[0].media_box(&doc), Rect::new(0.0, 0.0, 200.0, 100.0));
        assert_eq!(pages[0].crop_box(&doc), Rect::new(10.0, 10.0, 200.0, 50.0));
        assert_eq!(pages[0].rotate(&doc), 270);
        assert_eq!(pages[1].media_box(&doc), Rect::new(0.0, 0.0, 300.0, 300.0));
        assert_eq!(pages[1].rotate(&doc), 90);
    }

    #[test]
    fn broken_tree_falls_back_to_scan() {
        let pdf = build_pdf(
            "1.4",
            &[
                (1, "<< /Type /Catalog /Pages 9 0 R >>"),
                (
                    2,
                    "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 50 50] >>",
                ),
                (3, "<< /Type /Page /Parent 2 0 R >>"),
            ],
            "",
        );
        let doc = Document::from_bytes(pdf).unwrap();
        let pages = collect_pages(&doc).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].media_box(&doc), Rect::new(0.0, 0.0, 50.0, 50.0));
    }

    #[test]
    fn cycle_and_invalid_mediabox() {
        let pdf = build_pdf(
            "1.4",
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 0 0] >>"),
            ],
            "",
        );
        let doc = Document::from_bytes(pdf).unwrap();
        let pages = collect_pages(&doc).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].media_box(&doc), Rect::new(0.0, 0.0, 612.0, 792.0));
    }
}
