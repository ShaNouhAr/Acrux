//! Modifier une annotation existante : la déplacer, la redimensionner, en
//! changer la couleur, le fond, l'opacité, l'épaisseur du trait ou le texte
//! (§12.5.2) — les nôtres comme celles des autres logiciels.
//!
//! # La géométrie suit le rectangle
//!
//! Déplacer ou agrandir une annotation, c'est donner un nouveau `/Rect`.
//! Tout ce qui décrit son dessin en coordonnées de page (`/QuadPoints`,
//! `/InkList`, `/Vertices`, `/L`, `/CL`) suit par la même application
//! affine, ancien rectangle vers nouveau : un lecteur qui régénère
//! l'apparence retrouve le même dessin au même endroit.
//!
//! # L'apparence
//!
//! Un simple déplacement garde l'apparence telle quelle : le lecteur la pose
//! dans le nouveau rectangle (§12.5.5, algorithme 8.1), rien ne se dégrade.
//! Un changement de taille, de couleur, d'épaisseur ou d'opacité la
//! **régénère** ([`super::appearance::regenerate`]) : un trait de 2 pt reste
//! de 2 pt quand la forme grandit. Pour un type qu'on ne sait pas redessiner
//! (un tampon, une pièce jointe), la taille étire l'apparence d'origine,
//! l'opacité l'enveloppe dans un formulaire qui l'atténue, et la couleur est
//! refusée plutôt que mal faite.
//!
//! L'opacité est écrite **dans** l'apparence et dans `/CA`, comme le fait
//! Acrobat : notre moteur n'applique `/CA` qu'aux annotations sans
//! apparence, et les fichiers d'Acrobat ne la prennent donc pas deux fois.

use acrux_core::{Error, Rect, Result};
use acrux_document::{Dict, Document, Name, Object, ObjectRef, Page};

use super::appearance::{self, Regenerated};
use super::{
    annots_of, color_array, current_page, encode_text, numbers, pdf_date_now, rect_object, Rgb,
};

/// Ce qu'on change à une annotation ; un champ `None` reste tel quel.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnnotChanges {
    /// Nouveau rectangle, en coordonnées de page.
    pub rect: Option<Rect>,
    /// Couleur principale (voir [`super::AnnotationInfo::color`]).
    pub color: Option<Rgb>,
    /// Fond : `Some(None)` le retire.
    pub fill: Option<Option<Rgb>>,
    /// Opacité, de 0 à 1.
    pub opacity: Option<f64>,
    /// Épaisseur du trait, en points.
    pub width: Option<f64>,
    /// Texte du commentaire (`/Contents`).
    pub contents: Option<String>,
    /// Date de la modification (`/M`, forme `D:…`), tirée **par celui qui
    /// décide** de la modification, comme l'identité d'une annotation
    /// neuve : appliquée au document affiché, à la copie du fil de rendu et
    /// à chaque rejeu, elle doit donner le même fichier. `None` : maintenant.
    pub date: Option<String>,
}

impl AnnotChanges {
    /// Vrai si rien ne change.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rect.is_none()
            && self.color.is_none()
            && self.fill.is_none()
            && self.opacity.is_none()
            && self.width.is_none()
            && self.contents.is_none()
    }
}

/// Types dont la forme suit le texte balisé : ils ne se déplacent pas.
pub(crate) fn follows_text(subtype: &str) -> bool {
    matches!(
        subtype,
        "Highlight" | "Underline" | "StrikeOut" | "Squiggly" | "Redact"
    )
}

/// Annotation désignée par son rang dans `/Annots`, sa référence et son
/// dictionnaire, après les refus communs à toutes les modifications :
/// entrée écrite dans la page, champ de formulaire, lien, fenêtre
/// contextuelle, élément de « remplir et signer ».
fn target(doc: &Document, page: &Page, index: usize) -> Result<(ObjectRef, Dict, String)> {
    let (_, page_dict) = current_page(doc, page)?;
    let annots = annots_of(doc, &page_dict)?;
    let item = annots
        .get(index)
        .ok_or_else(|| Error::Corrupt(format!("annotation {} inexistante", index + 1)))?;
    let Object::Reference(r) = item else {
        return Err(Error::Unsupported(
            "annotation écrite dans la page : non modifiable".into(),
        ));
    };
    let dict = doc
        .get(*r)
        .ok()
        .and_then(|o| o.as_dict().cloned())
        .ok_or_else(|| Error::Corrupt("annotation illisible".into()))?;
    let subtype = dict
        .get(&Name::new("Subtype"))
        .and_then(Object::as_name)
        .map(Name::as_str)
        .unwrap_or_default();
    if matches!(subtype.as_str(), "Widget" | "Link" | "Popup")
        || dict.contains_key(&Name::new(crate::fillsign::TAG))
    {
        return Err(Error::Corrupt(format!(
            "annotation {} gérée ailleurs (champ, lien, fenêtre ou « remplir et signer »)",
            index + 1
        )));
    }
    Ok((*r, dict, subtype))
}

/// Rectangle `/Rect` d'un dictionnaire, normalisé.
fn rect_of(doc: &Document, d: &Dict) -> Option<Rect> {
    let r = numbers(doc, d, "Rect")?;
    (r.len() == 4).then(|| normalized(Rect::new(r[0], r[1], r[2], r[3])))
}

/// Le même rectangle, coins dans l'ordre.
fn normalized(r: Rect) -> Rect {
    Rect::new(
        r.x0.min(r.x1),
        r.y0.min(r.y1),
        r.x0.max(r.x1),
        r.y0.max(r.y1),
    )
}

/// Application affine d'un rectangle vers un autre : échelle puis
/// translation, par axe. Un côté nul garde l'échelle 1.
#[derive(Debug, Clone, Copy)]
struct Remap {
    sx: f64,
    sy: f64,
    tx: f64,
    ty: f64,
}

impl Remap {
    fn new(from: Rect, to: Rect) -> Self {
        let sx = if from.width() > 1e-9 {
            to.width() / from.width()
        } else {
            1.0
        };
        let sy = if from.height() > 1e-9 {
            to.height() / from.height()
        } else {
            1.0
        };
        Remap {
            sx,
            sy,
            tx: to.x0 - from.x0 * sx,
            ty: to.y0 - from.y0 * sy,
        }
    }

    /// Vrai si l'application n'est qu'une translation.
    fn is_translation(&self) -> bool {
        (self.sx - 1.0).abs() < 1e-9 && (self.sy - 1.0).abs() < 1e-9
    }

    /// Tableau de nombres `[x y x y …]` transformé.
    fn points(&self, values: &[f64]) -> Object {
        Object::Array(
            values
                .chunks_exact(2)
                .flat_map(|p| {
                    [
                        Object::Real(p[0] * self.sx + self.tx),
                        Object::Real(p[1] * self.sy + self.ty),
                    ]
                })
                .collect(),
        )
    }
}

/// Applique la nouvelle géométrie au dictionnaire : `/Rect` et tout ce qui
/// décrit le dessin en coordonnées de page.
fn move_geometry(doc: &Document, d: &mut Dict, map: Remap, rect: Rect) {
    d.insert(Name::new("Rect"), rect_object(rect));
    for key in ["QuadPoints", "Vertices", "L", "CL"] {
        if let Some(values) = numbers(doc, d, key) {
            d.insert(Name::new(key), map.points(&values));
        }
    }
    let ink = doc
        .dict_get(d, "InkList")
        .ok()
        .flatten()
        .and_then(|o| o.as_array().map(<[Object]>::to_vec));
    if let Some(strokes) = ink {
        let moved: Vec<Object> = strokes
            .iter()
            .map(|s| {
                let values: Vec<f64> = doc
                    .resolve(s)
                    .ok()
                    .and_then(|a| {
                        a.as_array().map(|a| {
                            a.iter()
                                .filter_map(|v| doc.resolve(v).ok().and_then(|x| x.as_f64()))
                                .collect()
                        })
                    })
                    .unwrap_or_default();
                map.points(&values)
            })
            .collect();
        d.insert(Name::new("InkList"), Object::Array(moved));
    }
}

/// La fenêtre contextuelle suit son annotation, décalée d'autant, sans
/// changer de taille : c'est une fenêtre, pas un dessin.
fn move_popup(doc: &Document, d: &Dict, from: Rect, to: Rect) {
    let Some(Object::Reference(popup)) = d.get(&Name::new("Popup")) else {
        return;
    };
    let Some(mut pd) = doc.get(*popup).ok().and_then(|o| o.as_dict().cloned()) else {
        return;
    };
    let Some(r) = rect_of(doc, &pd) else { return };
    let (dx, dy) = (to.x0 - from.x0, to.y1 - from.y1);
    pd.insert(
        Name::new("Rect"),
        rect_object(Rect::new(r.x0 + dx, r.y0 + dy, r.x1 + dx, r.y1 + dy)),
    );
    doc.set(*popup, Object::Dict(pd));
}

/// Écrit la nouvelle couleur principale.
fn set_color(d: &mut Dict, subtype: &str, color: Rgb) {
    if subtype == "FreeText" {
        // La couleur du texte vit dans `/DA` : on n'y change que
        // l'opérateur de remplissage, la police et le cadre restent.
        let da = match d.get(&Name::new("DA")) {
            Some(Object::String(s)) => String::from_utf8_lossy(s).into_owned(),
            _ => String::new(),
        };
        let updated = appearance::replace_da_fill(&da, color);
        d.insert(Name::new("DA"), Object::String(updated.into_bytes()));
    } else {
        d.insert(Name::new("C"), color_array(color));
    }
}

/// Écrit la nouvelle épaisseur : dans `/BS` (créé au besoin), et dans
/// `/Border` s'il existe, que certains lecteurs lisent seul.
fn set_width(doc: &Document, d: &mut Dict, width: f64) {
    let mut bs = doc
        .dict_get(d, "BS")
        .ok()
        .flatten()
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_else(|| {
            let mut b = Dict::new();
            b.insert(Name::new("Type"), Object::Name(Name::new("Border")));
            b.insert(Name::new("S"), Object::Name(Name::new("S")));
            b
        });
    bs.insert(Name::new("W"), Object::Real(width));
    d.insert(Name::new("BS"), Object::Dict(bs));
    if let Some(mut border) = numbers(doc, d, "Border").filter(|b| b.len() >= 3) {
        border[2] = width;
        d.insert(
            Name::new("Border"),
            Object::Array(border.into_iter().map(Object::Real).collect()),
        );
    }
}

/// Pose l'apparence régénérée : `/AP << /N … >>`, et les clés qui vont
/// avec (le rectangle, et pour une légende sa ligne et ses marges).
fn install(doc: &Document, d: &mut Dict, regen: Regenerated) {
    let stream = doc.add(regen.stream);
    let mut ap = Dict::new();
    ap.insert(Name::new("N"), Object::Reference(stream));
    d.insert(Name::new("AP"), Object::Dict(ap));
    // Un état d'apparence n'a plus de sens sans dictionnaire d'états.
    d.remove(&Name::new("AS"));
    d.insert(Name::new("Rect"), rect_object(regen.rect));
    for (key, value) in regen.keys {
        match value {
            Some(v) => d.insert(Name::new(key), v),
            None => d.remove(&Name::new(key)),
        };
    }
}

/// Modifie l'annotation de rang `index` dans le `/Annots` de la page.
///
/// # Errors
/// Index invalide ; annotation écrite dans la page, champ de formulaire,
/// lien, fenêtre contextuelle ou élément de « remplir et signer » ;
/// déplacement d'un balisage (il suit le texte) ; couleur d'un type dont on
/// ne sait pas redessiner l'apparence.
pub fn set_annotation_properties(
    doc: &Document,
    page: &Page,
    index: usize,
    changes: &AnnotChanges,
) -> Result<()> {
    let (reference, mut d, subtype) = target(doc, page, index)?;
    if changes.rect.is_some() && follows_text(&subtype) {
        return Err(Error::Corrupt(
            "un marquage suit le texte : il ne se déplace pas".into(),
        ));
    }
    let old = rect_of(doc, &d).unwrap_or_default();
    let mut resized = false;
    if let Some(rect) = changes.rect {
        let rect = normalized(rect);
        let map = Remap::new(old, rect);
        resized = !map.is_translation();
        move_geometry(doc, &mut d, map, rect);
        move_popup(doc, &d, old, rect);
    }
    if let Some(color) = changes.color {
        set_color(&mut d, &subtype, color.map(|v| v.clamp(0.0, 1.0)));
    }
    if let Some(fill) = changes.fill {
        let key = if subtype == "FreeText" { "C" } else { "IC" };
        match fill {
            Some(c) => d.insert(Name::new(key), color_array(c.map(|v| v.clamp(0.0, 1.0)))),
            None => d.remove(&Name::new(key)),
        };
    }
    let opacity = changes
        .opacity
        .filter(|o| o.is_finite())
        .map(|o| o.clamp(0.0, 1.0));
    if let Some(o) = opacity {
        if o >= 1.0 {
            d.remove(&Name::new("CA"));
        } else {
            d.insert(Name::new("CA"), Object::Real(o));
        }
    }
    if let Some(w) = changes.width.filter(|w| w.is_finite()) {
        set_width(doc, &mut d, w.clamp(0.0, 72.0));
    }
    if let Some(text) = &changes.contents {
        if text.is_empty() {
            d.remove(&Name::new("Contents"));
        } else {
            d.insert(Name::new("Contents"), Object::String(encode_text(text)));
        }
    }
    let restyled = changes.color.is_some()
        || changes.fill.is_some()
        || changes.width.is_some()
        || (changes.contents.is_some() && subtype == "FreeText");
    let has_appearance = doc.dict_get(&d, "AP").ok().flatten().is_some_and(|ap| {
        ap.as_dict()
            .is_some_and(|a| a.contains_key(&Name::new("N")))
    });
    if restyled || resized || opacity.is_some() {
        match appearance::regenerate(doc, &d, &subtype) {
            Some(regen) => install(doc, &mut d, regen),
            // Sans apparence, notre moteur la synthétise d'après les clés,
            // qui viennent d'être écrites : il n'y a rien de plus à faire.
            None if !has_appearance => {}
            None if restyled => {
                return Err(Error::Unsupported(format!(
                    "l'apparence d'une annotation /{subtype} ne se redessine pas : \
                     sa couleur et son trait ne changent pas"
                )));
            }
            None => {
                // Une taille : l'apparence d'origine s'étire au nouveau
                // rectangle. Une opacité : on l'enveloppe.
                if let Some(o) = opacity {
                    if let Some(wrapped) = appearance::with_opacity(doc, &d, o) {
                        let stream = doc.add(wrapped);
                        let mut ap = Dict::new();
                        ap.insert(Name::new("N"), Object::Reference(stream));
                        d.insert(Name::new("AP"), Object::Dict(ap));
                        d.remove(&Name::new("AS"));
                    }
                }
            }
        }
    }
    let date = changes.date.clone().unwrap_or_else(pdf_date_now);
    d.insert(Name::new("M"), Object::String(date.into_bytes()));
    doc.set(reference, Object::Dict(d));
    Ok(())
}

/// Cache ou remontre une annotation, sans la supprimer (bit 2 de `/F`,
/// §12.5.3) : tous les lecteurs le comprennent.
///
/// # Errors
/// Index invalide, annotation écrite dans la page, ou gérée ailleurs.
pub fn set_hidden(doc: &Document, page: &Page, index: usize, hidden: bool) -> Result<()> {
    let (reference, mut d, _) = target(doc, page, index)?;
    #[allow(clippy::cast_possible_truncation)] // des drapeaux sur 32 bits
    let flags = doc
        .dict_get(&d, "F")
        .ok()
        .flatten()
        .and_then(|f| f.as_f64())
        .unwrap_or(0.0) as i64;
    let flags = if hidden { flags | 2 } else { flags & !2 };
    d.insert(Name::new("F"), Object::Integer(flags));
    doc.set(reference, Object::Dict(d));
    Ok(())
}
