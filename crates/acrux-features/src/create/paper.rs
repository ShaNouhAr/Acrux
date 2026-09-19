//! Formats de page, orientation et marges.
//!
//! Les dimensions sont en **points PostScript** (1/72 de pouce), l'unité du
//! `/MediaBox` (ISO 32000-2 §7.9.5). Les formats ISO 216 (série A) et les
//! enveloppes sont donnés en millimètres et convertis au point le plus proche,
//! comme le font les imprimeurs et Acrobat.

use acrux_core::Rect;

pub use crate::stamp::Margins;

/// Points par millimètre (72 / 25,4).
const MM: f64 = 72.0 / 25.4;

/// Arrondit une dimension en millimètres au point entier le plus proche.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn mm(value: f64) -> f64 {
    (value * MM).round()
}

/// Format de page nommé, ou taille libre en points.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum PageSize {
    /// A0 — 841 × 1189 mm.
    A0,
    /// A1 — 594 × 841 mm.
    A1,
    /// A2 — 420 × 594 mm.
    A2,
    /// A3 — 297 × 420 mm.
    A3,
    /// A4 — 210 × 297 mm. Format par défaut.
    #[default]
    A4,
    /// A5 — 148 × 210 mm.
    A5,
    /// A6 — 105 × 148 mm.
    A6,
    /// Lettre US — 8,5 × 11 pouces.
    Letter,
    /// Légal US — 8,5 × 14 pouces.
    Legal,
    /// Tabloïd US — 11 × 17 pouces.
    Tabloid,
    /// Enveloppe DL — 110 × 220 mm.
    EnvelopeDl,
    /// Enveloppe C4 — 229 × 324 mm (une feuille A4 non pliée).
    EnvelopeC4,
    /// Enveloppe C5 — 162 × 229 mm (A4 plié en deux).
    EnvelopeC5,
    /// Enveloppe C6 — 114 × 162 mm (A4 plié en quatre).
    EnvelopeC6,
    /// Enveloppe Monarch — 3,875 × 7,5 pouces.
    EnvelopeMonarch,
    /// Taille libre, en points, telle quelle.
    Custom {
        /// Largeur en points.
        width: f64,
        /// Hauteur en points.
        height: f64,
    },
}

impl PageSize {
    /// Largeur et hauteur **à la française** (portrait), en points.
    ///
    /// Une taille libre est rendue telle quelle, sans être redressée : c'est
    /// [`PageSetup::page_size`] qui applique l'orientation.
    #[must_use]
    pub fn dimensions(self) -> (f64, f64) {
        match self {
            PageSize::A0 => (mm(841.0), mm(1189.0)),
            PageSize::A1 => (mm(594.0), mm(841.0)),
            PageSize::A2 => (mm(420.0), mm(594.0)),
            PageSize::A3 => (mm(297.0), mm(420.0)),
            PageSize::A4 => (mm(210.0), mm(297.0)),
            PageSize::A5 => (mm(148.0), mm(210.0)),
            PageSize::A6 => (mm(105.0), mm(148.0)),
            PageSize::Letter => (612.0, 792.0),
            PageSize::Legal => (612.0, 1008.0),
            PageSize::Tabloid => (792.0, 1224.0),
            PageSize::EnvelopeDl => (mm(110.0), mm(220.0)),
            PageSize::EnvelopeC4 => (mm(229.0), mm(324.0)),
            PageSize::EnvelopeC5 => (mm(162.0), mm(229.0)),
            PageSize::EnvelopeC6 => (mm(114.0), mm(162.0)),
            PageSize::EnvelopeMonarch => (279.0, 540.0),
            PageSize::Custom { width, height } => (width, height),
        }
    }

    /// Format d'après son nom, insensible à la casse, aux espaces et aux
    /// accents usuels : `A4`, `lettre`, `letter`, `legal`, `tabloid`,
    /// `enveloppe-dl`, `c5`… Une taille libre s'écrit `210x297mm`,
    /// `8.5x11in` ou `595x842` (points).
    ///
    /// Renvoie `None` si le nom ne désigne rien de connu.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let key: String = name
            .to_ascii_lowercase()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        let named = match key.as_str() {
            "a0" => PageSize::A0,
            "a1" => PageSize::A1,
            "a2" => PageSize::A2,
            "a3" => PageSize::A3,
            "a4" => PageSize::A4,
            "a5" => PageSize::A5,
            "a6" => PageSize::A6,
            "letter" | "lettre" | "us" | "usletter" => PageSize::Letter,
            "legal" | "uslegal" => PageSize::Legal,
            "tabloid" | "tabloid11x17" | "ledger" => PageSize::Tabloid,
            "dl" | "enveloppedl" | "envelopedl" => PageSize::EnvelopeDl,
            "c4" | "enveloppec4" | "envelopec4" => PageSize::EnvelopeC4,
            "c5" | "enveloppec5" | "envelopec5" => PageSize::EnvelopeC5,
            "c6" | "enveloppec6" | "envelopec6" => PageSize::EnvelopeC6,
            "monarch" | "enveloppemonarch" | "envelopemonarch" => PageSize::EnvelopeMonarch,
            _ => return parse_custom(name),
        };
        Some(named)
    }

    /// Nom canonique du format, pour l'affichage.
    #[must_use]
    pub fn name(self) -> String {
        match self {
            PageSize::A0 => "A0".into(),
            PageSize::A1 => "A1".into(),
            PageSize::A2 => "A2".into(),
            PageSize::A3 => "A3".into(),
            PageSize::A4 => "A4".into(),
            PageSize::A5 => "A5".into(),
            PageSize::A6 => "A6".into(),
            PageSize::Letter => "Lettre".into(),
            PageSize::Legal => "Légal".into(),
            PageSize::Tabloid => "Tabloïd".into(),
            PageSize::EnvelopeDl => "Enveloppe DL".into(),
            PageSize::EnvelopeC4 => "Enveloppe C4".into(),
            PageSize::EnvelopeC5 => "Enveloppe C5".into(),
            PageSize::EnvelopeC6 => "Enveloppe C6".into(),
            PageSize::EnvelopeMonarch => "Enveloppe Monarch".into(),
            PageSize::Custom { width, height } => format!("{width:.0} × {height:.0} pt"),
        }
    }
}

/// Analyse une taille libre `LxH[unité]` : `210x297mm`, `8.5x11in`, `595x842`.
fn parse_custom(name: &str) -> Option<PageSize> {
    let lower = name.trim().to_ascii_lowercase().replace('×', "x");
    let (unit, body) = if let Some(rest) = lower.strip_suffix("mm") {
        (MM, rest)
    } else if let Some(rest) = lower.strip_suffix("cm") {
        (MM * 10.0, rest)
    } else if let Some(rest) = lower.strip_suffix("in") {
        (72.0, rest)
    } else if let Some(rest) = lower.strip_suffix("pt") {
        (1.0, rest)
    } else {
        (1.0, lower.as_str())
    };
    let (w, h) = body.split_once('x')?;
    let width: f64 = w.trim().parse().ok()?;
    let height: f64 = h.trim().parse().ok()?;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    Some(PageSize::Custom {
        width: width * unit,
        height: height * unit,
    })
}

/// Orientation de la page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    /// À la française : la hauteur l'emporte.
    #[default]
    Portrait,
    /// À l'italienne : la largeur l'emporte.
    Landscape,
}

impl Orientation {
    /// Orientation d'après son nom (`portrait`, `paysage`, `landscape`).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "portrait" | "francaise" | "française" => Some(Orientation::Portrait),
            "paysage" | "landscape" | "italienne" => Some(Orientation::Landscape),
            _ => None,
        }
    }
}

/// Gabarit d'un document neuf : format, orientation, marges et nombre de pages.
#[derive(Debug, Clone, PartialEq)]
pub struct PageSetup {
    /// Format de page.
    pub size: PageSize,
    /// Orientation.
    pub orientation: Orientation,
    /// Marges, en points.
    pub margins: Margins,
    /// Nombre de pages (au moins une).
    pub pages: usize,
}

impl Default for PageSetup {
    /// A4 à la française, marges de 54 pt sur les côtés et 36 pt en haut et
    /// en bas (celles de [`Margins`]), une page.
    fn default() -> Self {
        PageSetup {
            size: PageSize::A4,
            orientation: Orientation::Portrait,
            margins: Margins::default(),
            pages: 1,
        }
    }
}

impl PageSetup {
    /// Largeur et hauteur de la page, orientation appliquée.
    #[must_use]
    pub fn page_size(&self) -> (f64, f64) {
        let (w, h) = self.size.dimensions();
        match self.orientation {
            Orientation::Portrait => (w.min(h), w.max(h)),
            Orientation::Landscape => (w.max(h), w.min(h)),
        }
    }

    /// Boîte de la page, origine en bas à gauche.
    #[must_use]
    pub fn page_box(&self) -> Rect {
        let (w, h) = self.page_size();
        Rect::new(0.0, 0.0, w, h)
    }

    /// Boîte utile : la page moins les marges. Des marges trop grandes pour
    /// la page donnent une boîte vide plutôt qu'une boîte retournée.
    #[must_use]
    pub fn content_box(&self) -> Rect {
        let (w, h) = self.page_size();
        let m = &self.margins;
        Rect::new(
            m.left,
            m.bottom,
            (w - m.right).max(m.left),
            (h - m.top).max(m.bottom),
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn iso_formats_match_the_printers_tables() {
        assert_eq!(PageSize::A4.dimensions(), (595.0, 842.0));
        assert_eq!(PageSize::A3.dimensions(), (842.0, 1191.0));
        assert_eq!(PageSize::A5.dimensions(), (420.0, 595.0));
        assert_eq!(PageSize::A0.dimensions(), (2384.0, 3370.0));
        assert_eq!(PageSize::A6.dimensions(), (298.0, 420.0));
        // Chaque format est la moitié du précédent, à un point près.
        for (big, small) in [
            (PageSize::A0, PageSize::A1),
            (PageSize::A1, PageSize::A2),
            (PageSize::A2, PageSize::A3),
            (PageSize::A3, PageSize::A4),
            (PageSize::A4, PageSize::A5),
            (PageSize::A5, PageSize::A6),
        ] {
            let (bw, bh) = big.dimensions();
            let (sw, sh) = small.dimensions();
            assert!((bw - sh).abs() <= 1.0, "{bw} vs {sh}");
            assert!((bh / 2.0 - sw).abs() <= 1.0, "{bh} vs {sw}");
        }
    }

    #[test]
    fn american_and_envelope_formats() {
        assert_eq!(PageSize::Letter.dimensions(), (612.0, 792.0));
        assert_eq!(PageSize::Legal.dimensions(), (612.0, 1008.0));
        assert_eq!(PageSize::Tabloid.dimensions(), (792.0, 1224.0));
        assert_eq!(PageSize::EnvelopeDl.dimensions(), (312.0, 624.0));
        assert_eq!(PageSize::EnvelopeC5.dimensions(), (459.0, 649.0));
        // Une C5 accueille une A4 pliée en deux : sa largeur dépasse la
        // hauteur d'une A5.
        assert!(PageSize::EnvelopeC5.dimensions().1 >= PageSize::A5.dimensions().1);
    }

    #[test]
    fn names_are_forgiving() {
        assert_eq!(PageSize::from_name("a4"), Some(PageSize::A4));
        assert_eq!(PageSize::from_name("A4"), Some(PageSize::A4));
        assert_eq!(PageSize::from_name("Lettre"), Some(PageSize::Letter));
        assert_eq!(
            PageSize::from_name("enveloppe-dl"),
            Some(PageSize::EnvelopeDl)
        );
        assert_eq!(PageSize::from_name("zzz"), None);
    }

    #[test]
    fn custom_sizes_accept_three_units() {
        assert_eq!(
            PageSize::from_name("595x842"),
            Some(PageSize::Custom {
                width: 595.0,
                height: 842.0
            })
        );
        let mm210 = PageSize::from_name("210x297mm").unwrap();
        let (w, h) = mm210.dimensions();
        assert!(
            (w - 595.0).abs() < 1.0 && (h - 842.0).abs() < 1.0,
            "{w}×{h}"
        );
        let inches = PageSize::from_name("8.5x11in").unwrap();
        assert_eq!(inches.dimensions(), (612.0, 792.0));
        assert_eq!(PageSize::from_name("0x10mm"), None);
        assert_eq!(PageSize::from_name("10mm"), None);
    }

    #[test]
    fn orientation_swaps_the_sides() {
        let portrait = PageSetup::default();
        assert_eq!(portrait.page_size(), (595.0, 842.0));
        let landscape = PageSetup {
            orientation: Orientation::Landscape,
            ..PageSetup::default()
        };
        assert_eq!(landscape.page_size(), (842.0, 595.0));
        // Une taille libre déjà à l'italienne reste à l'italienne.
        let wide = PageSetup {
            size: PageSize::Custom {
                width: 800.0,
                height: 400.0,
            },
            orientation: Orientation::Landscape,
            ..PageSetup::default()
        };
        assert_eq!(wide.page_size(), (800.0, 400.0));
    }

    #[test]
    fn content_box_removes_the_margins_and_never_inverts() {
        let setup = PageSetup {
            margins: Margins {
                top: 50.0,
                bottom: 40.0,
                left: 30.0,
                right: 20.0,
            },
            ..PageSetup::default()
        };
        let b = setup.content_box();
        assert_eq!((b.x0, b.y0, b.x1, b.y1), (30.0, 40.0, 575.0, 792.0));
        let squeezed = PageSetup {
            size: PageSize::Custom {
                width: 50.0,
                height: 50.0,
            },
            margins: Margins {
                top: 80.0,
                bottom: 80.0,
                left: 80.0,
                right: 80.0,
            },
            ..PageSetup::default()
        };
        let b = squeezed.content_box();
        assert!(b.width() <= 0.0 && b.height() <= 0.0, "{b:?}");
    }
}
