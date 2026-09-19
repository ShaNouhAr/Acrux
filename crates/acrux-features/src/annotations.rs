//! Annotations (ISO 32000-2 §12.5 ; inventaire Acrobat §8) : inventaire des
//! annotations d'une page, création d'annotations simples avec leur flux
//! d'apparence (pour que tout lecteur, Acrobat compris, les affiche à
//! l'identique), suppression.
//!
//! Les apparences sont générées en syntaxe PDF minimale et non compressée :
//! lisibles, déterministes, et rendues par notre moteur comme par les autres.

use std::fmt::Write as _;

use acrux_core::{Error, Rect, Result};
use acrux_document::text::decode_text_string;
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef, Page};

/// Résumé d'une annotation existante.
#[derive(Debug, Clone)]
pub struct AnnotationInfo {
    /// Position dans `/Annots`.
    pub index: usize,
    /// Référence de l'objet (si indirect).
    pub reference: Option<ObjectRef>,
    /// `/Subtype` (`Text`, `Link`, `Highlight`, `Square`, `Widget`…).
    pub subtype: String,
    /// `/Rect`.
    pub rect: Rect,
    /// `/Contents` décodé.
    pub contents: Option<String>,
    /// `/T` (auteur).
    pub author: Option<String>,
    /// `/M` (date de modification, brute `D:…`).
    pub modified: Option<String>,
    /// `/F` (drapeaux, §12.5.3).
    pub flags: i64,
    /// Possède un flux d'apparence `/AP /N`.
    pub has_appearance: bool,
    /// `/Subtype /Link` : cible URI si `/A << /S /URI >>`.
    pub uri: Option<String>,
}

/// Couleur RVB 0..1.
pub type Rgb = [f64; 3];

/// Annotation à créer.
#[derive(Debug, Clone)]
pub enum NewAnnotation {
    /// Rectangle (`/Square`).
    Square {
        /// Emplacement.
        rect: Rect,
        /// Couleur de bordure.
        stroke: Rgb,
        /// Épaisseur de bordure en points.
        width: f64,
        /// Couleur de remplissage (aucune si `None`).
        fill: Option<Rgb>,
        /// Commentaire associé (`/Contents`), comme sur n'importe quelle
        /// annotation de balisage : c'est lui que les relecteurs lisent.
        contents: Option<String>,
    },
    /// Surlignage (`/Highlight`) d'une zone rectangulaire.
    Highlight {
        /// Zone.
        rect: Rect,
        /// Couleur (jaune Acrobat par défaut : 1, 1, 0).
        color: Rgb,
        /// Commentaire associé (`/Contents`).
        contents: Option<String>,
    },
    /// Note (`/Text`, icône « commentaire ») avec contenu.
    Note {
        /// Coin supérieur gauche de l'icône (20 × 20 pt).
        x: f64,
        /// Ordonnée du haut de l'icône.
        y: f64,
        /// Texte de la note.
        contents: String,
        /// Couleur de l'icône.
        color: Rgb,
    },
    /// Lien (`/Link`) vers une URI, sans apparence visible (comme Acrobat).
    Link {
        /// Zone cliquable.
        rect: Rect,
        /// Cible.
        uri: String,
    },
}

fn num(doc: &Document, d: &Dict, key: &str) -> Option<f64> {
    doc.dict_get(d, key).ok().flatten().and_then(|o| o.as_f64())
}

fn string(doc: &Document, d: &Dict, key: &str) -> Option<String> {
    let o = doc.dict_get(d, key).ok().flatten()?;
    match &*o {
        Object::String(s) => Some(decode_text_string(s)),
        _ => None,
    }
}

/// Inventaire des annotations d'une page.
///
/// # Errors
/// Tableau `/Annots` illisible.
pub fn list_annotations(doc: &Document, page: &Page) -> Result<Vec<AnnotationInfo>> {
    let mut out = Vec::new();
    let Some(annots) = page.dict.get(&Name::new("Annots")) else {
        return Ok(out);
    };
    let annots = doc.resolve(annots)?;
    let Some(list) = annots.as_array() else {
        return Ok(out);
    };
    for (index, item) in list.iter().enumerate() {
        let reference = match item {
            Object::Reference(r) => Some(*r),
            _ => None,
        };
        let Ok(resolved) = doc.resolve(item) else {
            continue;
        };
        let Some(d) = resolved.as_dict() else {
            continue;
        };
        let subtype = d
            .get(&Name::new("Subtype"))
            .and_then(Object::as_name)
            .map_or_else(|| "?".to_string(), Name::as_str);
        let r: Vec<f64> = doc
            .dict_get(d, "Rect")
            .ok()
            .flatten()
            .and_then(|o| {
                o.as_array().map(|a| {
                    a.iter()
                        .filter_map(|v| doc.resolve(v).ok().and_then(|x| x.as_f64()))
                        .collect()
                })
            })
            .unwrap_or_default();
        let rect = if r.len() == 4 {
            Rect::new(r[0], r[1], r[2], r[3])
        } else {
            Rect::default()
        };
        let has_appearance = doc
            .dict_get(d, "AP")
            .ok()
            .flatten()
            .and_then(|ap| ap.as_dict().map(|a| a.contains_key(&Name::new("N"))))
            .unwrap_or(false);
        let uri = doc.dict_get(d, "A").ok().flatten().and_then(|a| {
            let a = a.as_dict()?;
            if a.get(&Name::new("S")).and_then(Object::as_name)?.0 != b"URI" {
                return None;
            }
            string(doc, a, "URI")
        });
        out.push(AnnotationInfo {
            index,
            reference,
            subtype,
            rect,
            contents: string(doc, d, "Contents"),
            author: string(doc, d, "T"),
            modified: string(doc, d, "M"),
            #[allow(clippy::cast_possible_truncation)]
            flags: num(doc, d, "F").unwrap_or(0.0) as i64,
            has_appearance,
            uri,
        });
    }
    Ok(out)
}

/// Date PDF `D:AAAAMMJJHHmmSSZ` pour l'instant présent (UTC).
#[must_use]
pub fn pdf_date_now() -> String {
    pdf_date_at(0)
}

/// Même chose, décalée de `offset_minutes` par rapport à UTC (le suffixe
/// reste `Z` : la valeur écrite est déjà celle du fuseau demandé).
#[must_use]
pub fn pdf_date_at(offset_minutes: i32) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let shifted = i64::try_from(secs).unwrap_or(0) + i64::from(offset_minutes) * 60;
    let secs = u64::try_from(shifted.max(0)).unwrap_or(0);
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Algorithme « civil from days » (Howard Hinnant), sans dépendance.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "D:{y:04}{m:02}{d:02}{:02}{:02}{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Même formatage, pour les modules voisins qui écrivent eux aussi des
/// apparences à la main (`attach`) : un nombre court et déterministe, sans
/// zéros inutiles, pour que le flux produit ne dépende pas de la plateforme.
pub(crate) fn fmt_number(v: f64) -> String {
    fmt(v)
}

/// XObject de formulaire minimal (`/BBox` + contenu), pour les modules
/// voisins qui posent leur propre annotation.
pub(crate) fn form_xobject(bbox: Rect, content: String) -> Object {
    appearance_stream(bbox, content, &[])
}

/// Flux d'apparence : dictionnaire de formulaire + contenu.
fn appearance_stream(bbox: Rect, content: String, extra: &[(&str, Object)]) -> Object {
    let mut d = Dict::new();
    d.insert(Name::new("Type"), Object::Name(Name::new("XObject")));
    d.insert(Name::new("Subtype"), Object::Name(Name::new("Form")));
    d.insert(
        Name::new("BBox"),
        Object::Array(vec![
            Object::Real(bbox.x0),
            Object::Real(bbox.y0),
            Object::Real(bbox.x1),
            Object::Real(bbox.y1),
        ]),
    );
    for (k, v) in extra {
        d.insert(Name::new(k), v.clone());
    }
    let raw = content.into_bytes();
    d.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
    );
    Object::Stream { dict: d, raw }
}

fn color_array(c: Rgb) -> Object {
    Object::Array(c.iter().map(|v| Object::Real(*v)).collect())
}

/// Ajoute une annotation à la page et retourne sa référence.
///
/// # Errors
/// La page n'est pas un objet indirect, ou `/Annots` est illisible.
#[allow(clippy::too_many_lines)] // une branche par type d'annotation
pub fn add_annotation(
    doc: &Document,
    page: &Page,
    annot: &NewAnnotation,
    author: Option<&str>,
) -> Result<ObjectRef> {
    let page_ref = page
        .reference
        .ok_or_else(|| Error::Corrupt("la page doit être un objet indirect".into()))?;
    let mut d = Dict::new();
    d.insert(Name::new("Type"), Object::Name(Name::new("Annot")));
    d.insert(Name::new("P"), Object::Reference(page_ref));
    d.insert(Name::new("M"), Object::String(pdf_date_now().into_bytes()));
    d.insert(Name::new("F"), Object::Integer(4)); // Print
    if let Some(a) = author {
        d.insert(Name::new("T"), Object::String(encode_text(a)));
    }
    let rect_of = |r: Rect| {
        Object::Array(vec![
            Object::Real(r.x0),
            Object::Real(r.y0),
            Object::Real(r.x1),
            Object::Real(r.y1),
        ])
    };
    match annot {
        NewAnnotation::Square {
            rect,
            stroke,
            width,
            fill,
            contents,
        } => {
            if let Some(c) = contents {
                d.insert(Name::new("Contents"), Object::String(encode_text(c)));
            }
            d.insert(Name::new("Subtype"), Object::Name(Name::new("Square")));
            d.insert(Name::new("Rect"), rect_of(*rect));
            d.insert(Name::new("C"), color_array(*stroke));
            if let Some(f) = fill {
                d.insert(Name::new("IC"), color_array(*f));
            }
            let mut bs = Dict::new();
            bs.insert(Name::new("W"), Object::Real(*width));
            d.insert(Name::new("BS"), Object::Dict(bs));
            let half = width / 2.0;
            let mut content = format!(
                "{} {} {} RG {} w ",
                fmt(stroke[0]),
                fmt(stroke[1]),
                fmt(stroke[2]),
                fmt(*width)
            );
            let op = if let Some(f) = fill {
                let _ = write!(content, "{} {} {} rg ", fmt(f[0]), fmt(f[1]), fmt(f[2]));
                "B"
            } else {
                "S"
            };
            let _ = write!(
                content,
                "{} {} {} {} re {op}",
                fmt(rect.x0 + half),
                fmt(rect.y0 + half),
                fmt(rect.width() - width),
                fmt(rect.height() - width)
            );
            let ap = doc.add(appearance_stream(*rect, content, &[]));
            let mut apd = Dict::new();
            apd.insert(Name::new("N"), Object::Reference(ap));
            d.insert(Name::new("AP"), Object::Dict(apd));
        }
        NewAnnotation::Highlight {
            rect,
            color,
            contents,
        } => {
            if let Some(c) = contents {
                d.insert(Name::new("Contents"), Object::String(encode_text(c)));
            }
            d.insert(Name::new("Subtype"), Object::Name(Name::new("Highlight")));
            d.insert(Name::new("Rect"), rect_of(*rect));
            d.insert(Name::new("C"), color_array(*color));
            // QuadPoints : x1 y1 x2 y2 x3 y3 x4 y4 = haut-gauche, haut-droit, bas-gauche, bas-droit (§12.5.6.10).
            d.insert(
                Name::new("QuadPoints"),
                Object::Array(vec![
                    Object::Real(rect.x0),
                    Object::Real(rect.y1),
                    Object::Real(rect.x1),
                    Object::Real(rect.y1),
                    Object::Real(rect.x0),
                    Object::Real(rect.y0),
                    Object::Real(rect.x1),
                    Object::Real(rect.y0),
                ]),
            );
            let mut gs = Dict::new();
            gs.insert(Name::new("BM"), Object::Name(Name::new("Multiply")));
            let mut ext = Dict::new();
            ext.insert(Name::new("GS0"), Object::Dict(gs));
            let mut res = Dict::new();
            res.insert(Name::new("ExtGState"), Object::Dict(ext));
            let content = format!(
                "/GS0 gs {} {} {} rg {} {} {} {} re f",
                fmt(color[0]),
                fmt(color[1]),
                fmt(color[2]),
                fmt(rect.x0),
                fmt(rect.y0),
                fmt(rect.width()),
                fmt(rect.height())
            );
            let mut group = Dict::new();
            group.insert(Name::new("S"), Object::Name(Name::new("Transparency")));
            let ap = doc.add(appearance_stream(
                *rect,
                content,
                &[
                    ("Resources", Object::Dict(res)),
                    ("Group", Object::Dict(group)),
                ],
            ));
            let mut apd = Dict::new();
            apd.insert(Name::new("N"), Object::Reference(ap));
            d.insert(Name::new("AP"), Object::Dict(apd));
        }
        NewAnnotation::Note {
            x,
            y,
            contents,
            color,
        } => {
            let rect = Rect::new(*x, *y - 20.0, *x + 20.0, *y);
            d.insert(Name::new("Subtype"), Object::Name(Name::new("Text")));
            d.insert(Name::new("Rect"), rect_of(rect));
            d.insert(Name::new("Name"), Object::Name(Name::new("Comment")));
            d.insert(Name::new("Contents"), Object::String(encode_text(contents)));
            d.insert(Name::new("C"), color_array(*color));
            d.insert(Name::new("F"), Object::Integer(4 | 8 | 16)); // Print, NoZoom, NoRotate
                                                                   // Icône : bulle arrondie remplie avec trois lignes de « texte ».
            let (x0, y0) = (rect.x0, rect.y0);
            let content = format!(
                "{} {} {} rg 0.25 0.25 0.25 RG 0.8 w {x0} {y0} m {} {y0} l {} {} l {} {} l {x0} {} l h B 1 g 0.4 w {} {} m {} {} l S {} {} m {} {} l S {} {} m {} {} l S",
                fmt(color[0]),
                fmt(color[1]),
                fmt(color[2]),
                fmt(x0 + 20.0),
                fmt(x0 + 20.0),
                fmt(y0 + 14.0),
                fmt(x0 + 10.0),
                fmt(y0 + 20.0),
                fmt(y0 + 14.0),
                fmt(x0 + 4.0),
                fmt(y0 + 11.0),
                fmt(x0 + 16.0),
                fmt(y0 + 11.0),
                fmt(x0 + 4.0),
                fmt(y0 + 8.0),
                fmt(x0 + 16.0),
                fmt(y0 + 8.0),
                fmt(x0 + 4.0),
                fmt(y0 + 5.0),
                fmt(x0 + 12.0),
                fmt(y0 + 5.0),
                x0 = fmt(x0),
                y0 = fmt(y0)
            );
            let ap = doc.add(appearance_stream(rect, content, &[]));
            let mut apd = Dict::new();
            apd.insert(Name::new("N"), Object::Reference(ap));
            d.insert(Name::new("AP"), Object::Dict(apd));
        }
        NewAnnotation::Link { rect, uri } => {
            d.insert(Name::new("Subtype"), Object::Name(Name::new("Link")));
            d.insert(Name::new("Rect"), rect_of(*rect));
            d.insert(
                Name::new("Border"),
                Object::Array(vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Integer(0),
                ]),
            );
            let mut a = Dict::new();
            a.insert(Name::new("S"), Object::Name(Name::new("URI")));
            a.insert(Name::new("URI"), Object::String(uri.clone().into_bytes()));
            d.insert(Name::new("A"), Object::Dict(a));
        }
    }
    let annot_ref = doc.add(Object::Dict(d));
    // Ajout à /Annots (créé si absent ; un tableau indirect est réécrit en direct).
    let mut page_dict = page.dict.clone();
    let mut annots = match page_dict.get(&Name::new("Annots")) {
        Some(o) => doc
            .resolve(o)?
            .as_array()
            .map(<[Object]>::to_vec)
            .unwrap_or_default(),
        None => Vec::new(),
    };
    annots.push(Object::Reference(annot_ref));
    page_dict.insert(Name::new("Annots"), Object::Array(annots));
    doc.set(page_ref, Object::Dict(page_dict));
    Ok(annot_ref)
}

/// Supprime l'annotation d'index donné (position dans `/Annots`) ; son popup
/// éventuel est supprimé aussi.
///
/// # Errors
/// Index invalide ou page non indirecte.
pub fn remove_annotation(doc: &Document, page: &Page, index: usize) -> Result<()> {
    let page_ref = page
        .reference
        .ok_or_else(|| Error::Corrupt("la page doit être un objet indirect".into()))?;
    let mut page_dict = page.dict.clone();
    let mut annots = match page_dict.get(&Name::new("Annots")) {
        Some(o) => doc
            .resolve(o)?
            .as_array()
            .map(<[Object]>::to_vec)
            .unwrap_or_default(),
        None => Vec::new(),
    };
    if index >= annots.len() {
        return Err(Error::Corrupt(format!(
            "annotation {} inexistante",
            index + 1
        )));
    }
    let removed = annots.remove(index);
    if let Object::Reference(r) = removed {
        if let Ok(o) = doc.get(r) {
            if let Some(d) = o.as_dict() {
                if let Some(Object::Reference(popup)) = d.get(&Name::new("Popup")) {
                    annots.retain(|a| !matches!(a, Object::Reference(p) if p == popup));
                    doc.delete(*popup);
                }
            }
        }
        doc.delete(r);
    }
    page_dict.insert(Name::new("Annots"), Object::Array(annots));
    doc.set(page_ref, Object::Dict(page_dict));
    Ok(())
}

/// Chaîne de texte PDF : PDFDoc si possible, sinon UTF-16BE avec BOM.
#[must_use]
pub fn encode_text(s: &str) -> Vec<u8> {
    if s.chars().all(|c| (c as u32) < 128) {
        return s.as_bytes().to_vec();
    }
    let mut out = vec![0xFE, 0xFF];
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_be_bytes());
    }
    out
}

/// Pages avec, pour chacune, ses annotations (utilitaire pour l'outil en ligne de commande).
///
/// # Errors
/// Document illisible.
pub fn list_all(doc: &Document) -> Result<Vec<(usize, Vec<AnnotationInfo>)>> {
    let pages = collect_pages(doc)?;
    let mut out = Vec::new();
    for p in &pages {
        out.push((p.index, list_annotations(doc, p)?));
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn doc() -> Document {
        Document::from_bytes(
            b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >> endobj\n".to_vec(),
        )
        .unwrap()
    }

    #[test]
    fn add_list_remove_roundtrip() {
        let d = doc();
        let page = collect_pages(&d).unwrap().remove(0);
        add_annotation(
            &d,
            &page,
            &NewAnnotation::Square {
                rect: Rect::new(10.0, 10.0, 60.0, 40.0),
                stroke: [1.0, 0.0, 0.0],
                width: 2.0,
                fill: Some([1.0, 1.0, 0.0]),
                contents: Some("À revoir".into()),
            },
            Some("Élodie"),
        )
        .unwrap();
        let page = collect_pages(&d).unwrap().remove(0);
        add_annotation(
            &d,
            &page,
            &NewAnnotation::Note {
                x: 100.0,
                y: 150.0,
                contents: "Bonjour à tous".into(),
                color: [1.0, 0.8, 0.0],
            },
            None,
        )
        .unwrap();
        let page = collect_pages(&d).unwrap().remove(0);
        add_annotation(
            &d,
            &page,
            &NewAnnotation::Highlight {
                rect: Rect::new(0.0, 0.0, 50.0, 10.0),
                color: [1.0, 1.0, 0.0],
                contents: None,
            },
            None,
        )
        .unwrap();
        let page = collect_pages(&d).unwrap().remove(0);
        add_annotation(
            &d,
            &page,
            &NewAnnotation::Link {
                rect: Rect::new(0.0, 0.0, 50.0, 10.0),
                uri: "https://example.org".into(),
            },
            None,
        )
        .unwrap();
        // Enregistrement complet puis relecture.
        let saved = d.save_full().unwrap();
        let d2 = Document::from_bytes(saved).unwrap();
        let page = collect_pages(&d2).unwrap().remove(0);
        let list = list_annotations(&d2, &page).unwrap();
        assert_eq!(list.len(), 4);
        assert_eq!(list[0].subtype, "Square");
        assert_eq!(list[0].author.as_deref(), Some("Élodie"));
        assert!(list[0].has_appearance);
        assert!(list[0].modified.as_deref().unwrap().starts_with("D:20"));
        assert_eq!(list[1].subtype, "Text");
        assert_eq!(list[1].contents.as_deref(), Some("Bonjour à tous"));
        assert_eq!(list[2].subtype, "Highlight");
        assert_eq!(list[3].uri.as_deref(), Some("https://example.org"));
        assert!(!list[3].has_appearance);
        // Le rendu prend en compte les apparences (le carré rouge/jaune est visible).
        let rendered =
            acrux_render::render_page(&d2, &page, 1.0, &acrux_render::RenderOptions::default());
        let px = rendered.bitmap.pixel(35, 200 - 25).unwrap();
        assert!(
            px[0] > 200 && px[1] > 200 && px[2] < 60,
            "remplissage jaune attendu, obtenu {px:?}"
        );
        // Suppression.
        remove_annotation(&d2, &page, 1).unwrap();
        let page = collect_pages(&d2).unwrap().remove(0);
        let list = list_annotations(&d2, &page).unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[1].subtype, "Highlight");
        assert!(remove_annotation(&d2, &page, 9).is_err());
    }

    #[test]
    fn date_and_text_encoding() {
        let d = pdf_date_now();
        assert!(
            d.starts_with("D:20") && d.ends_with('Z') && d.len() == 17,
            "{d}"
        );
        assert_eq!(encode_text("abc"), b"abc");
        assert_eq!(encode_text("é"), vec![0xFE, 0xFF, 0x00, 0xE9]);
    }
}
