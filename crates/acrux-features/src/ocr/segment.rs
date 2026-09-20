//! Découpe d'une image d'encre en lignes, mots et caractères.
//!
//! Le découpage est purement géométrique : profils d'encre pour les lignes,
//! composantes connexes pour les caractères, écarts pour les mots. Rien
//! n'est encore reconnu ici — c'est [`super::shapes`] qui donnera des lettres
//! à ces boîtes.
// Coordonnées et comptages de pixels : les conversions y sont partout, et
// toutes bornées par la taille de l'image.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use super::image::Ink;

/// Une boîte de pixels, bords inclus à gauche/haut, exclus à droite/bas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Box2 {
    /// Bord gauche.
    pub x0: u32,
    /// Bord haut.
    pub y0: u32,
    /// Bord droit (exclu).
    pub x1: u32,
    /// Bord bas (exclu).
    pub y1: u32,
}

impl Box2 {
    /// Largeur en pixels.
    #[must_use]
    pub fn width(self) -> u32 {
        self.x1.saturating_sub(self.x0)
    }

    /// Hauteur en pixels.
    #[must_use]
    pub fn height(self) -> u32 {
        self.y1.saturating_sub(self.y0)
    }

    /// Réunion de deux boîtes.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    /// Recouvrement horizontal avec une autre boîte, en pixels.
    #[must_use]
    pub fn overlap_x(self, other: Self) -> i64 {
        i64::from(self.x1.min(other.x1)) - i64::from(self.x0.max(other.x0))
    }
}

/// Une ligne de texte trouvée dans l'image.
#[derive(Debug, Clone)]
pub struct Row {
    /// Boîte de la ligne.
    pub bbox: Box2,
    /// Caractères supposés, de gauche à droite.
    pub glyphs: Vec<Glyph>,
}

/// Un caractère supposé : sa boîte, et l'encre qu'elle contient.
#[derive(Debug, Clone)]
pub struct Glyph {
    /// Boîte du caractère.
    pub bbox: Box2,
    /// Un blanc le précède.
    pub space_before: bool,
}

/// Découpe l'image en lignes de texte, dans l'ordre de lecture.
///
/// La coupe est celle dite **en croix** (Nagy, 1984), appliquée en
/// alternance : on sépare d'abord ce que des rangées blanches séparent, puis
/// ce que des colonnes blanches séparent, et l'on recommence sur chaque
/// morceau. Un en-tête qui traverse la page se détache ainsi du corps, et le
/// corps se sépare alors en colonnes — ce qu'une coupe verticale seule ne
/// saurait faire, puisque l'en-tête remplit la gouttière.
///
/// Les morceaux sont rendus dans l'ordre où on les lit : de haut en bas, et
/// de gauche à droite dans chaque bande.
#[must_use]
pub fn rows(ink: &Ink) -> Vec<Row> {
    let whole = Box2 {
        x0: 0,
        y0: 0,
        x1: ink.width,
        y1: ink.height,
    };
    let mut leaves = Vec::new();
    cross_cut(ink, whole, 0, &mut leaves);
    leaves
        .into_iter()
        .filter_map(|leaf| build_row(ink, leaf))
        .collect()
}

/// Coupe en croix, guidée par la taille des lettres.
///
/// Deux colonnes se reconnaissent à leur **gouttière** : un couloir blanc
/// large de plusieurs lettres. Un blanc entre deux mots ne fait qu'une
/// lettre de large ; c'est ce qui les distingue, et c'est pourquoi la mesure
/// se prend sur les taches d'encre de la bande elle-même plutôt que sur une
/// fraction de la page.
fn cross_cut(ink: &Ink, bbox: Box2, depth: u32, out: &mut Vec<Box2>) {
    if depth > 12 || bbox.width() == 0 || bbox.height() == 0 {
        out.push(bbox);
        return;
    }
    let parts = components(ink, bbox.x0, bbox.x1, bbox.y0, bbox.y1);
    if parts.is_empty() {
        return;
    }
    let mut widths: Vec<u32> = parts.iter().map(|p| p.width()).collect();
    widths.sort_unstable();
    let letter = widths[widths.len() / 2].max(2);
    // Une gouttière vaut trois lettres ; un blanc entre deux mots, une.
    let columns = column_bands(ink, bbox, letter * 3);
    if columns.len() > 1 {
        for column in columns {
            cross_cut(ink, column, depth + 1, out);
        }
        return;
    }
    let rows = row_bands(ink, bbox);
    if rows.len() > 1 {
        for row in rows {
            cross_cut(ink, row, depth + 1, out);
        }
        return;
    }
    // Une bande d'un seul tenant : c'est une ligne.
    out.push(rows.into_iter().next().unwrap_or(bbox));
}

/// Bandes horizontales séparées par des rangées sans encre.
fn row_bands(ink: &Ink, bbox: Box2) -> Vec<Box2> {
    let mut out = Vec::new();
    let mut start: Option<u32> = None;
    for y in bbox.y0..bbox.y1 {
        let has = (bbox.x0..bbox.x1).any(|x| ink.at(x, y));
        match (has, start) {
            (true, None) => start = Some(y),
            (false, Some(from)) => {
                out.push(Box2 {
                    y0: from,
                    y1: y,
                    ..bbox
                });
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        out.push(Box2 {
            y0: from,
            y1: bbox.y1,
            ..bbox
        });
    }
    out
}

/// Bandes verticales séparées par une gouttière blanche.
///
/// La gouttière doit être large : un simple blanc entre deux mots ne sépare
/// pas deux colonnes.
fn column_bands(ink: &Ink, bbox: Box2, gutter: u32) -> Vec<Box2> {
    let gutter = gutter.max(8);
    let mut out = Vec::new();
    let mut start: Option<u32> = None;
    let mut blank = 0_u32;
    for x in bbox.x0..bbox.x1 {
        let has = (bbox.y0..bbox.y1).any(|y| ink.at(x, y));
        if has {
            if start.is_none() {
                start = Some(x);
            }
            blank = 0;
        } else if let Some(from) = start {
            blank += 1;
            if blank >= gutter {
                out.push(Box2 {
                    x0: from,
                    x1: x - blank + 1,
                    ..bbox
                });
                start = None;
                blank = 0;
            }
        }
    }
    if let Some(from) = start {
        out.push(Box2 {
            x0: from,
            x1: bbox.x1,
            ..bbox
        });
    }
    if out.is_empty() {
        return vec![bbox];
    }
    out
}

/// Construit une ligne entre deux rangées, si elle tient debout.
fn build_row(ink: &Ink, bbox: Box2) -> Option<Row> {
    let (x0, x1, y0, y1) = (bbox.x0, bbox.x1, bbox.y0, bbox.y1);
    if y1.saturating_sub(y0) < 4 {
        return None; // un filet, une poussière : pas une ligne de texte
    }
    let parts = components(ink, x0, x1, y0, y1);
    if parts.is_empty() {
        return None;
    }
    let glyphs = group(parts, y1 - y0);
    if glyphs.is_empty() {
        return None;
    }
    let bbox = glyphs
        .iter()
        .map(|g| g.bbox)
        .reduce(Box2::union)
        .unwrap_or(Box2 {
            x0: 0,
            y0,
            x1: 0,
            y1,
        });
    Some(Row { bbox, glyphs })
}

/// Composantes connexes d'une bande de l'image (huit voisins).
///
/// Chaque composante est une tache d'encre d'un seul tenant : une lettre, un
/// accent, le point d'un « i ».
fn components(ink: &Ink, x0: u32, x1: u32, y0: u32, y1: u32) -> Vec<Box2> {
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let mut labels = vec![0_u32; w * h];
    let mut parent: Vec<u32> = vec![0];
    // Première passe : étiquettes provisoires, unions des voisins déjà vus.
    for y in 0..h {
        for x in 0..w {
            if !ink.at(x0 + x as u32, y0 + y as u32) {
                continue;
            }
            let mut found = 0_u32;
            for (dx, dy) in [(-1_i32, 0_i32), (-1, -1), (0, -1), (1, -1)] {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    continue;
                }
                let label = labels[ny as usize * w + nx as usize];
                if label == 0 {
                    continue;
                }
                if found == 0 {
                    found = label;
                } else {
                    union(&mut parent, found, label);
                }
            }
            if found == 0 {
                let next = parent.len() as u32;
                parent.push(next);
                found = next;
            }
            labels[y * w + x] = found;
        }
    }
    // Seconde passe : boîte de chaque composante.
    let mut boxes: Vec<Option<Box2>> = vec![None; parent.len()];
    for y in 0..h {
        for x in 0..w {
            let label = labels[y * w + x];
            if label == 0 {
                continue;
            }
            let root = find(&mut parent, label) as usize;
            let here = Box2 {
                x0: x0 + x as u32,
                y0: y0 + y as u32,
                x1: x0 + x as u32 + 1,
                y1: y0 + y as u32 + 1,
            };
            boxes[root] = Some(boxes[root].map_or(here, |b| b.union(here)));
        }
    }
    let mut out: Vec<Box2> = boxes.into_iter().flatten().collect();
    out.sort_by_key(|b| (b.x0, b.y0));
    out
}

/// Racine d'une étiquette, avec compression de chemin.
fn find(parent: &mut [u32], mut label: u32) -> u32 {
    while parent[label as usize] != label {
        let grand = parent[parent[label as usize] as usize];
        parent[label as usize] = grand;
        label = grand;
    }
    label
}

/// Réunit deux étiquettes.
fn union(parent: &mut [u32], a: u32, b: u32) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        let (small, big) = if ra < rb { (ra, rb) } else { (rb, ra) };
        parent[big as usize] = small;
    }
}

/// Réunit les composantes qui forment un même caractère, puis marque les
/// blancs.
///
/// Un « é » est fait de deux taches, un « i » aussi ; deux taches qui se
/// chevauchent horizontalement appartiennent au même caractère.
fn group(parts: Vec<Box2>, line_height: u32) -> Vec<Glyph> {
    let mut merged: Vec<Box2> = Vec::new();
    for part in parts {
        // Une poussière isolée n'est pas un caractère.
        if part.width() * part.height() < 2 {
            continue;
        }
        match merged.last_mut() {
            Some(last) if last.overlap_x(part) > i64::from(part.width().min(last.width())) / 2 => {
                *last = last.union(part);
            }
            _ => merged.push(part),
        }
    }
    // Les taches trop larges — deux lettres qui se touchent — ne sont pas
    // coupées ici : c'est la reconnaissance qui tranchera, en comparant ce
    // que donne la tache entière et ce que donnent ses deux moitiés.
    // Un blanc : un écart net par rapport aux écarts habituels de la ligne.
    let gaps: Vec<u32> = merged
        .windows(2)
        .map(|w| w[1].x0.saturating_sub(w[0].x1))
        .collect();
    let threshold = space_threshold(line_height);
    let mut out = Vec::with_capacity(merged.len());
    for (i, bbox) in merged.iter().enumerate() {
        let space_before = i > 0 && gaps[i - 1] >= threshold;
        out.push(Glyph {
            bbox: *bbox,
            space_before,
        });
    }
    out
}

/// Coupe une tache en deux, au creux d'encre le plus net.
///
/// Rien ne dit ici que la coupe est la bonne : c'est à l'appelant d'essayer
/// et de garder ce qui se lit le mieux. Deux lettres qui se touchent ne se
/// devinent pas à la géométrie seule — un « m » a lui aussi un creux entre
/// ses jambages.
#[must_use]
pub fn split_at(ink: &Ink, bbox: Box2) -> Option<(Box2, Box2)> {
    if bbox.width() < 6 {
        return None;
    }
    let profile: Vec<u32> = (bbox.x0..bbox.x1)
        .map(|x| (bbox.y0..bbox.y1).filter(|y| ink.at(x, *y)).count() as u32)
        .collect();
    // Le creux se cherche au centre : couper au bord détacherait le
    // jambage d'une lettre au lieu de séparer deux lettres.
    let margin = (profile.len() / 4).max(2);
    if profile.len() <= margin * 2 + 1 {
        return None;
    }
    let (mut at, mut lowest) = (0_usize, u32::MAX);
    for (i, v) in profile
        .iter()
        .enumerate()
        .take(profile.len() - margin)
        .skip(margin)
    {
        if *v < lowest {
            lowest = *v;
            at = i;
        }
    }
    let cut = bbox.x0 + at as u32;
    let left = tighten(ink, Box2 { x1: cut, ..bbox });
    let right = tighten(ink, Box2 { x0: cut, ..bbox });
    (left.width() >= 2 && right.width() >= 2).then_some((left, right))
}

/// Resserre une boîte sur l'encre qu'elle contient.
fn tighten(ink: &Ink, bbox: Box2) -> Box2 {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0_u32, 0_u32);
    for y in bbox.y0..bbox.y1 {
        for x in bbox.x0..bbox.x1 {
            if ink.at(x, y) {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x0 == u32::MAX {
        return bbox;
    }
    Box2 { x0, y0, x1, y1 }
}

/// Écart à partir duquel deux caractères sont séparés par un blanc.
///
/// Un blanc de mot vaut environ le quart d'un cadratin, et la hauteur d'une
/// ligne un cadratin entier : le cinquième de la ligne sépare donc les mots
/// sans séparer les lettres, qui se frôlent à un vingtième.
fn space_threshold(line_height: u32) -> u32 {
    (line_height / 5).max(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deux traits séparés par du blanc donnent deux lignes.
    #[test]
    fn deux_bandes_dencre_font_deux_lignes() {
        let (w, h) = (20_u32, 30_u32);
        let mut rgb = vec![255_u8; (w * h * 3) as usize];
        let mut paint = |y0: u32, y1: u32| {
            for y in y0..y1 {
                for x in 2..18 {
                    let i = ((y * w + x) * 3) as usize;
                    rgb[i] = 0;
                    rgb[i + 1] = 0;
                    rgb[i + 2] = 0;
                }
            }
        };
        paint(2, 8);
        paint(18, 26);
        let ink = Ink::from_rgb(&rgb, w, h);
        let lines = rows(&ink);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].bbox.y0, 2);
        assert_eq!(lines[1].bbox.y1, 26);
    }

    /// Deux taches voisines sur la même ligne font deux caractères, avec un
    /// blanc entre elles si l'écart est celui d'un mot.
    #[test]
    fn un_ecart_de_mot_devient_un_blanc() {
        let (w, h) = (40_u32, 12_u32);
        let mut rgb = vec![255_u8; (w * h * 3) as usize];
        let mut paint = |x0: u32, x1: u32| {
            for y in 2..10 {
                for x in x0..x1 {
                    let i = ((y * w + x) * 3) as usize;
                    rgb[i] = 0;
                    rgb[i + 1] = 0;
                    rgb[i + 2] = 0;
                }
            }
        };
        paint(1, 5);
        paint(6, 10);
        paint(17, 21);
        let ink = Ink::from_rgb(&rgb, w, h);
        let lines = rows(&ink);
        assert_eq!(lines.len(), 1, "un écart de mot ne fait pas deux colonnes");
        let glyphs = &lines[0].glyphs;
        assert_eq!(glyphs.len(), 3);
        assert!(
            !glyphs[1].space_before,
            "un écart d'un pixel n'est pas un blanc"
        );
        assert!(glyphs[2].space_before, "un écart de sept pixels en est un");
    }

    /// Un couloir blanc large de plusieurs lettres sépare deux colonnes.
    #[test]
    fn une_gouttiere_separe_deux_colonnes() {
        let (w, h) = (80_u32, 40_u32);
        let mut rgb = vec![255_u8; (w * h * 3) as usize];
        let mut paint = |x0: u32, x1: u32, y0: u32, y1: u32| {
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y * w + x) * 3) as usize;
                    rgb[i] = 0;
                    rgb[i + 1] = 0;
                    rgb[i + 2] = 0;
                }
            }
        };
        // Deux colonnes de deux lettres, décalées : aucune rangée n'est
        // blanche d'un bord à l'autre, seule la gouttière les sépare.
        paint(2, 8, 4, 12);
        paint(10, 16, 4, 12);
        paint(60, 66, 8, 16);
        paint(68, 74, 8, 16);
        let ink = Ink::from_rgb(&rgb, w, h);
        let lines = rows(&ink);
        assert_eq!(lines.len(), 2, "les deux colonnes doivent se séparer");
        assert!(lines[0].bbox.x1 < lines[1].bbox.x0);
    }
}
