//! Les cinq marques de la barre « remplir et signer ».
//!
//! Coche, croix, rond, trait, point : de quoi remplir un formulaire papier
//! numérisé, celui qui n'a pas de champs. Elles sont dessinées par la même
//! chaîne que l'encre manuscrite ([`super::ink`]), avec une plume d'épaisseur
//! constante : elles héritent donc gratuitement des bouts ronds et des virages
//! arrondis, et elles s'agrandissent sans jamais se pixéliser.
//!
//! Les coordonnées sont données dans une boîte de 100 × 100 ; le rectangle
//! demandé à la pose fixe la taille réelle.

use acrux_document::Dict;

use super::ink::{ellipse, outline, Outline, Pen, Seg, Stroke};
use super::{fmt, write_path, Form, Rgb};

/// Marque posée dans une case à cocher, une date, une signature paraphée.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Coche.
    Check,
    /// Croix.
    Cross,
    /// Point plein.
    Dot,
    /// Cercle, pour entourer.
    Circle,
    /// Trait horizontal, pour barrer ou souligner.
    Line,
}

impl Mark {
    /// Reconnaît une marque par son nom, en français ou en anglais.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Mark> {
        match name.to_ascii_lowercase().as_str() {
            "check" | "coche" | "tick" => Some(Mark::Check),
            "cross" | "croix" | "x" => Some(Mark::Cross),
            "dot" | "point" => Some(Mark::Dot),
            "circle" | "rond" | "cercle" => Some(Mark::Circle),
            "line" | "trait" | "ligne" => Some(Mark::Line),
            _ => None,
        }
    }

    /// Nom canonique.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Mark::Check => "check",
            Mark::Cross => "cross",
            Mark::Dot => "dot",
            Mark::Circle => "circle",
            Mark::Line => "line",
        }
    }

    /// Proportions naturelles, largeur sur hauteur.
    ///
    /// Un trait est long et plat, un point est carré : sans cela, une marque
    /// posée dans un rectangle large se déformerait ou flotterait.
    #[must_use]
    pub fn aspect(self) -> (f64, f64) {
        match self {
            Mark::Check | Mark::Cross | Mark::Dot | Mark::Circle => (100.0, 100.0),
            Mark::Line => (100.0, 18.0),
        }
    }
}

/// Épaisseur du trait des marques, dans la boîte de 100 × 100.
const STROKE: f64 = 11.0;

/// Plume des marques : épaisseur constante, bouts francs mais ronds.
fn pen() -> Pen {
    Pen {
        width: STROKE,
        thinning: 0.0,
        speed_ref: 1.0,
        smoothing: 0.0,
        taper: 0.0,
    }
}

/// Construit le dessin d'une marque.
pub(super) fn form(mark: Mark, color: Rgb) -> Form {
    let (width, height) = mark.aspect();
    let outline = outline_of(mark);
    let bbox = outline.bbox;
    let mut content = format!("{} {} {} rg\n", fmt(color[0]), fmt(color[1]), fmt(color[2]));
    write_path(&mut content, &outline, -bbox.x0, -bbox.y0);
    content.push_str("f\n");
    // La boîte déclarée garde les proportions voulues même si le dessin est
    // un peu plus étroit : une croix reste carrée.
    let drawn_w = (bbox.x1 - bbox.x0).max(0.01);
    let drawn_h = (bbox.y1 - bbox.y0).max(0.01);
    Form {
        content,
        width: drawn_w.max(width * drawn_h / height.max(0.01)),
        height: drawn_h,
        resources: Dict::new(),
    }
}

/// Contour d'une marque, dans sa boîte de 100 × 100.
///
/// Publique parce que l'application dessine **le même contour** dans sa barre
/// d'outils : le bouton montre exactement ce qui sera posé sur la page.
#[must_use]
pub fn outline_of(mark: Mark) -> Outline {
    match mark {
        // Une coche a sa barre courte remontante et sa longue barre montante :
        // le point bas n'est pas au milieu, sinon elle a l'air d'un V.
        Mark::Check => ink(&[Stroke::from_points(&[
            (8.0, 52.0),
            (34.0, 20.0),
            (92.0, 84.0),
        ])]),
        Mark::Cross => ink(&[
            Stroke::from_points(&[(12.0, 12.0), (88.0, 88.0)]),
            Stroke::from_points(&[(88.0, 12.0), (12.0, 88.0)]),
        ]),
        Mark::Dot => ink(&[Stroke::from_points(&[(50.0, 50.0)])]),
        Mark::Line => ink(&[Stroke::from_points(&[(6.0, 9.0), (94.0, 9.0)])]),
        // Un anneau : l'ellipse extérieure dans un sens, l'intérieure dans
        // l'autre. La règle non nulle y perce alors le centre.
        Mark::Circle => ring(),
    }
}

/// Contour d'une suite de traits, à la plume des marques.
fn ink(strokes: &[Stroke]) -> Outline {
    outline(strokes, &pen())
}

/// Anneau : deux ellipses concentriques de sens opposés.
fn ring() -> Outline {
    let (cx, cy) = (50.0, 50.0);
    let (rx, ry) = (44.0, 44.0);
    let half = STROKE / 2.0;
    let mut segs = ellipse(cx, cy, rx + half, ry + half, false);
    segs.extend(ellipse(cx, cy, rx - half, ry - half, true));
    let mut out = Outline {
        segs,
        bbox: acrux_core::Rect::default(),
    };
    out.bbox = bbox_of(&out.segs);
    out
}

/// Boîte englobante d'une suite de segments.
fn bbox_of(segs: &[Seg]) -> acrux_core::Rect {
    let mut b: Option<acrux_core::Rect> = None;
    let mut add = |x: f64, y: f64| match &mut b {
        Some(r) => {
            r.x0 = r.x0.min(x);
            r.y0 = r.y0.min(y);
            r.x1 = r.x1.max(x);
            r.y1 = r.y1.max(y);
        }
        None => {
            b = Some(acrux_core::Rect {
                x0: x,
                y0: y,
                x1: x,
                y1: y,
            });
        }
    };
    for seg in segs {
        match *seg {
            Seg::Move(x, y) | Seg::Line(x, y) => add(x, y),
            Seg::Curve(x1, y1, x2, y2, x3, y3) => {
                add(x1, y1);
                add(x2, y2);
                add(x3, y3);
            }
            Seg::Close => {}
        }
    }
    b.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{form, Mark};

    #[test]
    fn chaque_marque_dessine_quelque_chose() {
        for mark in [
            Mark::Check,
            Mark::Cross,
            Mark::Dot,
            Mark::Circle,
            Mark::Line,
        ] {
            let f = form(mark, [0.0, 0.0, 0.0]);
            assert!(f.width > 1.0 && f.height > 1.0, "{}", mark.name());
            assert!(f.content.contains(" f\n") || f.content.ends_with("f\n"));
            assert!(f.content.contains(" c\n"), "{} sans courbe", mark.name());
        }
    }

    #[test]
    fn les_noms_font_laller_retour() {
        for mark in [
            Mark::Check,
            Mark::Cross,
            Mark::Dot,
            Mark::Circle,
            Mark::Line,
        ] {
            assert_eq!(Mark::from_name(mark.name()), Some(mark));
        }
        assert_eq!(Mark::from_name("coche"), Some(Mark::Check));
        assert_eq!(Mark::from_name("cercle"), Some(Mark::Circle));
        assert_eq!(Mark::from_name("rien"), None);
    }

    #[test]
    fn le_trait_est_plus_large_que_haut() {
        let f = form(Mark::Line, [0.0, 0.0, 0.0]);
        assert!(f.width > f.height * 3.0);
    }
}
