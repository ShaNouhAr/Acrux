//! Les cases à cocher **dessinées** d'un document qui n'est pas un formulaire.
//!
//! Un formulaire papier numérisé, ou exporté d'un traitement de texte, n'a
//! pas de champs : ses cases sont des petits carrés tracés dans la page, ou
//! le caractère « ☐ ». Acrobat les repère quand on remplit : la case
//! s'encadre sous le pointeur, et la coche qu'on y pose se cale dedans.
//!
//! La détection n'est qu'une **suggestion**. Elle ne pose jamais rien
//! d'elle-même : elle dit seulement « ceci ressemble à une case », et
//! l'application s'en sert pour centrer la marque quand on clique dedans.
//! Une fausse détection ne coûte donc qu'une coche un peu recentrée — et
//! cliquer ailleurs pose librement, comme toujours.
//!
//! # Ce qui ressemble à une case
//!
//! - un **tracé** à peu près carré, de la taille d'une case (entre
//!   [`MIN_SIDE`] et [`MAX_SIDE`] points), qui est **cerné** : un trait de
//!   contour, ou un remplissage clair. Un carré plein et sombre est une puce,
//!   pas une case ;
//! - **quatre filets** qui ferment un carré : bien des producteurs (Word,
//!   les générateurs de formulaires) tracent une case trait par trait, ou la
//!   noient dans un grand tracé qui dessine tout le cadre de la page ;
//! - un **caractère** de case vide : ☐ □ ▢ ◻ ❏ ❐ ❑ ❒.
//!
//! Un tracé qui en contient un autre de même nature (le cadre d'un tableau
//! autour de ses cellules) est écarté : on garde le plus petit.

use acrux_core::{Rect, Result};
use acrux_document::{Document, Page};

use crate::edit_objects::{self, Kind};

/// Plus petit côté d'une case, en points. En dessous, c'est une puce.
pub const MIN_SIDE: f64 = 5.0;
/// Plus grand côté d'une case, en points. Au-dessus, c'est un cadre.
pub const MAX_SIDE: f64 = 40.0;

/// Caractères qui dessinent une case vide.
const BOX_CHARS: [char; 8] = ['☐', '□', '▢', '◻', '❏', '❐', '❑', '❒'];

/// Vrai si le rectangle a la taille et la forme d'une case.
fn box_shaped(r: &Rect) -> bool {
    let (w, h) = (r.width(), r.height());
    let side = MIN_SIDE..=MAX_SIDE;
    side.contains(&w) && side.contains(&h) && (w / h) > 0.75 && (w / h) < 1.34
}

/// Vrai si ces octets de contenu peignent un **contour** ou un fond clair —
/// pas un aplat sombre.
///
/// On lit le dernier opérateur de peinture du tracé : `S`, `s`, `B`, `b` et
/// leurs variantes tracent un contour. Un simple remplissage (`f`, `F`,
/// `f*`) ne compte que s'il est clair, ce qu'on juge à la dernière couleur de
/// remplissage posée dans le tracé lui-même ; sans couleur locale, on ne sait
/// pas, et l'on s'abstient.
fn outlined(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let tokens: Vec<&str> = text.split_ascii_whitespace().collect();
    let Some(paint) = tokens
        .iter()
        .rposition(|t| matches!(*t, "S" | "s" | "B" | "b" | "B*" | "b*" | "f" | "F" | "f*"))
    else {
        return false;
    };
    if !matches!(tokens[paint], "f" | "F" | "f*") {
        return true;
    }
    // Un remplissage : clair, c'est le fond blanc d'une case ; sombre, une
    // puce. Un tracé fait de deux rectangles emboîtés remplis en pair-impair
    // est un cadre dessiné « en creux » — la façon de Chrome.
    let rects = tokens.iter().filter(|t| **t == "re").count();
    if tokens[paint] == "f*" && rects >= 2 {
        return true;
    }
    let number = |i: usize| tokens.get(i).and_then(|t| t.parse::<f64>().ok());
    for i in (0..paint).rev() {
        let level = match tokens[i] {
            "g" if i >= 1 => number(i - 1),
            "rg" if i >= 3 => match (number(i - 3), number(i - 2), number(i - 1)) {
                (Some(r), Some(g), Some(b)) => Some(0.299 * r + 0.587 * g + 0.114 * b),
                _ => None,
            },
            _ => continue,
        };
        return level.is_some_and(|l| l > 0.85);
    }
    false
}

/// Les cases à cocher dessinées d'une page, en espace de page.
///
/// # Errors
/// Flux de contenu illisible.
pub fn find(doc: &Document, page: &Page) -> Result<Vec<Rect>> {
    let content = acrux_render::page::page_content(doc, page);
    let mut found: Vec<Rect> = edit_objects::list(doc, page)?
        .into_iter()
        .filter(|o| o.kind == Kind::Path && box_shaped(&o.bbox))
        .filter(|o| content.get(o.range.0..o.range.1).is_some_and(outlined))
        .map(|o| o.bbox)
        .collect();
    // Les caractères de case vide, et les carrés fermés par quatre filets.
    if let Ok(text) = crate::text::extract_page_text(doc, page) {
        found.extend(ruled_squares(&text.rules));
        for glyph in text
            .lines
            .iter()
            .flat_map(|l| &l.words)
            .flat_map(|w| &w.glyphs)
        {
            if glyph.text.chars().any(|c| BOX_CHARS.contains(&c)) && box_shaped(&glyph.bbox) {
                found.push(glyph.bbox);
            }
        }
    }
    // Deux cases confondues (un fond puis un contour) n'en font qu'une, et un
    // cadre qui en contient une autre s'efface devant elle.
    let mut kept: Vec<Rect> = Vec::new();
    found.sort_by(|a, b| (a.width() * a.height()).total_cmp(&(b.width() * b.height())));
    for r in found {
        let holds_one = kept.iter().any(|k| {
            k.x0 >= r.x0 - 0.5 && k.x1 <= r.x1 + 0.5 && k.y0 >= r.y0 - 0.5 && k.y1 <= r.y1 + 0.5
        });
        if !holds_one {
            kept.push(r);
        }
    }
    // Ordre de lecture : de haut en bas, puis de gauche à droite.
    kept.sort_by(|a, b| b.y1.total_cmp(&a.y1).then(a.x0.total_cmp(&b.x0)));
    Ok(kept)
}

/// Écart toléré entre deux filets censés se rejoindre, en points.
const JOIN: f64 = 1.6;

/// Les carrés fermés par quatre filets : deux verticaux de même hauteur, et
/// deux horizontaux qui couvrent l'intervalle entre eux, en haut et en bas.
///
/// Les filets horizontaux peuvent dépasser — la ligne du haut d'un peigne de
/// cases court d'un bout à l'autre —, d'où « couvrent » et non « égalent ».
fn ruled_squares(rules: &[Rect]) -> Vec<Rect> {
    let mut vertical: Vec<&Rect> = rules.iter().filter(|r| r.height() > r.width()).collect();
    let horizontal: Vec<&Rect> = rules.iter().filter(|r| r.width() >= r.height()).collect();
    vertical.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    let mut squares = Vec::new();
    for (i, left) in vertical.iter().enumerate() {
        let lx = f64::midpoint(left.x0, left.x1);
        for right in &vertical[i + 1..] {
            let rx = f64::midpoint(right.x0, right.x1);
            let width = rx - lx;
            if width > MAX_SIDE {
                break;
            }
            if width < MIN_SIDE {
                continue;
            }
            // La hauteur que les deux montants ont en commun.
            let (low, high) = (left.y0.max(right.y0), left.y1.min(right.y1));
            if high - low < MIN_SIDE {
                continue;
            }
            // Les traverses qui couvrent l'intervalle, de bas en haut.
            let mut bars: Vec<f64> = horizontal
                .iter()
                .filter(|h| h.x0 <= lx + JOIN && h.x1 >= rx - JOIN)
                .map(|h| f64::midpoint(h.y0, h.y1))
                .filter(|y| *y >= low - JOIN && *y <= high + JOIN)
                .collect();
            bars.sort_by(f64::total_cmp);
            for pair in bars.windows(2) {
                let candidate = Rect::new(lx, pair[0], rx, pair[1]);
                if box_shaped(&candidate) {
                    squares.push(candidate);
                }
            }
        }
    }
    squares
}

/// La case sous un point, s'il y en a une.
#[must_use]
pub fn at(boxes: &[Rect], x: f64, y: f64) -> Option<Rect> {
    boxes
        .iter()
        .find(|r| x >= r.x0 && x <= r.x1 && y >= r.y0 && y <= r.y1)
        .copied()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use acrux_document::collect_pages;

    fn corpus(name: &str) -> String {
        format!("{}/../../tests/corpus/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    /// Le formulaire du corpus a deux cases de 15 points — « oui » et
    /// « non » — et rien d'autre n'y ressemble : ni les lignes à remplir, ni
    /// le cadre de l'en-tête.
    #[test]
    fn les_deux_cases_du_formulaire_sont_trouvees_et_elles_seules() {
        let doc = Document::load(corpus("reels/chrome-skia-formulaire-lignes-cases.pdf")).unwrap();
        let pages = collect_pages(&doc).unwrap();
        let boxes = find(&doc, &pages[0]).unwrap();
        assert_eq!(boxes.len(), 2, "cases trouvées : {boxes:?}");
        for b in &boxes {
            assert!((b.width() - 15.0).abs() < 1.0 && (b.height() - 15.0).abs() < 1.0);
        }
        assert!(boxes[0].x0 < boxes[1].x0, "de gauche à droite");
        let inside = at(&boxes, boxes[0].x0 + 7.0, boxes[0].y0 + 7.0);
        assert_eq!(inside, Some(boxes[0]));
        assert_eq!(at(&boxes, 10.0, 10.0), None);
    }

    /// Une page sans case n'en invente pas : ni le carré plein du document
    /// de test, ni ses tableaux, ni ses images.
    #[test]
    fn un_document_sans_case_nen_invente_pas() {
        for name in [
            "reels/chrome-skia-deux-colonnes-entete-pied-cesure.pdf",
            "reels/chrome-skia-images-jpeg-png-tirets-opacite-cjk.pdf",
        ] {
            let doc = Document::load(corpus(name)).unwrap();
            let pages = collect_pages(&doc).unwrap();
            for page in &pages {
                let boxes = find(&doc, page).unwrap();
                assert!(boxes.is_empty(), "{name} : {boxes:?}");
            }
        }
    }

    /// Une case tracée trait par trait — quatre filets — est une case ; trois
    /// filets n'en font pas une, et deux longues lignes parallèles non plus.
    #[test]
    fn quatre_filets_ferment_une_case() {
        let h = |x0: f64, x1: f64, y: f64| Rect::new(x0, y - 0.3, x1, y + 0.3);
        let v = |x: f64, y0: f64, y1: f64| Rect::new(x - 0.3, y0, x + 0.3, y1);
        let closed = [
            h(100.0, 112.0, 500.0),
            h(100.0, 112.0, 512.0),
            v(100.0, 500.0, 512.0),
            v(112.0, 500.0, 512.0),
        ];
        let found = ruled_squares(&closed);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!((found[0].width() - 12.0).abs() < 0.1 && (found[0].height() - 12.0).abs() < 0.1);
        assert!(
            ruled_squares(&closed[..3]).is_empty(),
            "trois côtés ne ferment rien"
        );
        // Un peigne : une longue ligne en haut et en bas, des montants réguliers.
        let mut comb = vec![h(100.0, 160.0, 500.0), h(100.0, 160.0, 512.0)];
        comb.extend((0..=5).map(|i| v(100.0 + 12.0 * f64::from(i), 500.0, 512.0)));
        let cells = ruled_squares(&comb);
        assert!(cells.len() >= 5, "cinq cellules au moins : {}", cells.len());
        // Deux lignes à remplir, l'une sous l'autre : pas de montants, pas de case.
        assert!(ruled_squares(&[h(50.0, 300.0, 400.0), h(50.0, 300.0, 415.0)]).is_empty());
    }

    #[test]
    fn un_contour_est_une_case_un_aplat_sombre_une_puce() {
        assert!(outlined(b"0 0 10 10 re S"));
        assert!(outlined(b"1 g 0 0 10 10 re f"));
        assert!(outlined(b"0 0 15 15 re 1 1 13 13 re f*"));
        assert!(!outlined(b"0 g 0 0 10 10 re f"));
        assert!(!outlined(b"0.1 0.1 0.1 rg 0 0 10 10 re f"));
        assert!(!outlined(b"0 0 10 10 re f"));
        assert!(!outlined(b"0 0 10 10 re n"));
    }
}
