//! Détection de tableaux, volontairement conservatrice (aucun faux positif sur
//! du texte courant vaut mieux qu'un tableau de plus) :
//!
//! - **grilles de filets** : filets horizontaux de même étendue (≥ 2) et filets
//!   verticaux qui les traversent (≥ 2) ; à défaut de filets verticaux, les
//!   colonnes sont déduites des blancs communs à toutes les lignes de la bande ;
//! - **colonnes de texte alignées** : lignes de base consécutives portant le même
//!   nombre (≥ 2) de fragments courts (≤ 3 mots, ≤ 40 % de la largeur de la
//!   rangée) dont les bords gauches, droits ou centres coïncident à 1 em près
//!   (deux colonnes de texte courant ont des lignes bien plus longues).
//!
//! Une grille n'est retenue que si elle a au moins 2 × 2 cellules et 2 cellules
//! non vides.

use acrux_core::Rect;

use super::layout::Frag;
use super::{Cell, Table, Word};

/// Grille de filets : abscisses des colonnes (croissantes) et ordonnées des
/// lignes (décroissantes, espace PDF).
pub(super) struct Grid {
    pub bbox: Rect,
    pub xs: Vec<f64>,
    pub ys: Vec<f64>,
}

/// Fusionne les filets colinéaires qui se touchent (segments d'une même ligne
/// tracés cellule par cellule). `horizontal` : fusion selon x, sinon selon y.
fn merge_collinear(rules: &[Rect], horizontal: bool) -> Vec<Rect> {
    let key = |r: &Rect| {
        if horizontal {
            (f64::midpoint(r.y0, r.y1), r.x0)
        } else {
            (f64::midpoint(r.x0, r.x1), r.y0)
        }
    };
    let mut sorted: Vec<Rect> = rules.to_vec();
    sorted.sort_by(|a, b| {
        let (ka, kb) = (key(a), key(b));
        ka.0.partial_cmp(&kb.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(ka.1.partial_cmp(&kb.1).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut out: Vec<Rect> = Vec::with_capacity(sorted.len());
    for r in sorted {
        if let Some(last) = out.last_mut() {
            let (kl, kr) = (key(last), key(&r));
            let (last_end, start) = if horizontal {
                (last.x1, r.x0)
            } else {
                (last.y1, r.y0)
            };
            if (kl.0 - kr.0).abs() <= 1.0 && start <= last_end + 2.0 {
                *last = last.union(&r);
                continue;
            }
        }
        out.push(r);
    }
    out
}

/// Regroupe des valeurs proches (tolérance) et renvoie leurs moyennes, triées.
fn cluster(mut values: Vec<f64>, tol: f64) -> Vec<f64> {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: Vec<(f64, f64)> = Vec::new(); // (somme, compte)
    for v in values {
        match out.last_mut() {
            Some((sum, n)) if (v - *sum / *n).abs() <= tol => {
                *sum += v;
                *n += 1.0;
            }
            _ => out.push((v, 1.0)),
        }
    }
    out.into_iter().map(|(s, n)| s / n).collect()
}

/// Grilles de filets validées par le texte (voir la documentation du module).
pub(super) fn ruled_grids(rules: &[Rect], word_boxes: &[Rect], med: f64) -> Vec<Grid> {
    let (h, v): (Vec<Rect>, Vec<Rect>) = rules.iter().partition(|r| r.width() >= r.height());
    let h = merge_collinear(&h, true);
    let v = merge_collinear(&v, false);
    let mut used = vec![false; h.len()];
    let mut grids: Vec<Grid> = Vec::new();
    for i in 0..h.len() {
        if used[i] {
            continue;
        }
        used[i] = true;
        let tol = (0.03 * h[i].width()).max(3.0);
        let group: Vec<usize> = (i..h.len())
            .filter(|&j| !used[j] || j == i)
            .filter(|&j| (h[j].x0 - h[i].x0).abs() <= tol && (h[j].x1 - h[i].x1).abs() <= tol)
            .collect();
        if group.len() < 2 {
            continue;
        }
        for &j in &group {
            used[j] = true;
        }
        let x0 = group.iter().map(|&j| h[j].x0).fold(f64::MAX, f64::min);
        let x1 = group.iter().map(|&j| h[j].x1).fold(f64::MIN, f64::max);
        let ys = cluster(
            group
                .iter()
                .map(|&j| f64::midpoint(h[j].y0, h[j].y1))
                .collect(),
            1.5,
        );
        if ys.len() < 2 {
            continue;
        }
        let (bottom, top) = (ys[0], ys[ys.len() - 1]);
        let band_h = top - bottom;
        let mut ys: Vec<f64> = ys;
        ys.reverse();
        let band = Rect::new(x0, bottom, x1, top);
        if grids.iter().any(|g| !g.bbox.intersect(&band).is_empty()) {
            continue;
        }
        // Colonnes : filets verticaux qui traversent la bande…
        let crossing: Vec<f64> = v
            .iter()
            .filter(|r| {
                let xc = f64::midpoint(r.x0, r.x1);
                let overlap = r.y1.min(top) - r.y0.max(bottom);
                xc >= x0 - 2.0 && xc <= x1 + 2.0 && overlap >= 0.5 * band_h
            })
            .map(|r| f64::midpoint(r.x0, r.x1))
            .collect();
        let mut xs = cluster(crossing, 1.5);
        // … ou blancs communs à toutes les rangées.
        if xs.len() < 2 {
            xs = columns_from_text(&band, &ys, word_boxes, med);
        }
        if xs.len() < 2 {
            continue;
        }
        grids.push(Grid { bbox: band, xs, ys });
    }
    grids
}

/// Séparateurs de colonnes déduits des blancs (≥ 1 em) communs à toutes les
/// rangées non vides d'une bande ; `None` si moins de deux rangées portent du texte.
fn columns_from_text(band: &Rect, ys: &[f64], word_boxes: &[Rect], med: f64) -> Vec<f64> {
    let mut rows_occupied: Vec<Vec<(f64, f64)>> = Vec::new();
    for w in ys.windows(2) {
        let (top, bottom) = (w[0], w[1]);
        let mut spans: Vec<(f64, f64)> = word_boxes
            .iter()
            .filter(|b| {
                let cy = f64::midpoint(b.y0, b.y1);
                let cx = f64::midpoint(b.x0, b.x1);
                cy <= top && cy > bottom && cx >= band.x0 && cx <= band.x1
            })
            .map(|b| (b.x0, b.x1))
            .collect();
        if spans.is_empty() {
            continue;
        }
        spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut merged: Vec<(f64, f64)> = Vec::new();
        for s in spans {
            match merged.last_mut() {
                Some(last) if s.0 <= last.1 + med => last.1 = last.1.max(s.1),
                _ => merged.push(s),
            }
        }
        rows_occupied.push(merged);
    }
    if rows_occupied.len() < 2 {
        return Vec::new();
    }
    // Blancs libres de chaque rangée, intersectés.
    let mut free: Vec<(f64, f64)> = vec![(band.x0, band.x1)];
    for row in &rows_occupied {
        let mut next = Vec::new();
        for &(f0, f1) in &free {
            let mut cursor = f0;
            for &(o0, o1) in row {
                if o0 > cursor && o0.min(f1) > cursor {
                    next.push((cursor, o0.min(f1)));
                }
                cursor = cursor.max(o1);
            }
            if cursor < f1 {
                next.push((cursor, f1));
            }
        }
        free = next;
    }
    let mut xs = vec![band.x0];
    for (f0, f1) in free {
        if f1 - f0 >= med && f0 > band.x0 + 1.0 && f1 < band.x1 - 1.0 {
            xs.push(f64::midpoint(f0, f1));
        }
    }
    xs.push(band.x1);
    xs
}

/// Répartit les mots d'une grille en cellules ; `None` si le résultat n'a pas
/// l'allure d'un tableau (moins de 2 × 2 cellules ou moins de 2 cellules pleines).
pub(super) fn grid_table(g: &Grid, words: &[Word]) -> Option<Table> {
    let (cols, rows) = (g.xs.len().checked_sub(1)?, g.ys.len().checked_sub(1)?);
    if cols < 2 || rows < 2 || cols * rows > 100_000 {
        return None;
    }
    let mut cells: Vec<Vec<Vec<&Word>>> = vec![vec![Vec::new(); cols]; rows];
    for w in words {
        let cx = f64::midpoint(w.bbox.x0, w.bbox.x1);
        let cy = f64::midpoint(w.bbox.y0, w.bbox.y1);
        let col =
            g.xs.windows(2)
                .position(|x| cx >= x[0] && cx < x[1])
                .unwrap_or(cols - 1);
        let row =
            g.ys.windows(2)
                .position(|y| cy <= y[0] && cy > y[1])
                .unwrap_or(rows - 1);
        cells[row][col].push(w);
    }
    let filled = cells.iter().flatten().filter(|c| !c.is_empty()).count();
    if filled < 2 {
        return None;
    }
    let rows_out = cells
        .into_iter()
        .enumerate()
        .map(|(r, row)| {
            row.into_iter()
                .enumerate()
                .map(|(c, mut ws)| {
                    ws.sort_by(|a, b| {
                        let (ay, by) = (
                            f64::midpoint(a.bbox.y0, a.bbox.y1),
                            f64::midpoint(b.bbox.y0, b.bbox.y1),
                        );
                        // Ligne du haut d'abord (à ½ hauteur près), puis de gauche à droite.
                        if (ay - by).abs() > 0.5 * a.bbox.height().max(b.bbox.height()) {
                            by.partial_cmp(&ay).unwrap_or(std::cmp::Ordering::Equal)
                        } else {
                            a.bbox
                                .x0
                                .partial_cmp(&b.bbox.x0)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        }
                    });
                    let text: Vec<&str> = ws.iter().map(|w| w.text.as_str()).collect();
                    Cell {
                        bbox: Rect::new(g.xs[c], g.ys[r + 1], g.xs[c + 1], g.ys[r]),
                        text: text.join(" "),
                    }
                })
                .collect()
        })
        .collect();
    Some(Table {
        bbox: g.bbox,
        rows: rows_out,
        lines: Vec::new(),
    })
}

/// Rangée candidate : fragments d'une même ligne de base, triés de gauche à droite.
struct Row {
    frags: Vec<usize>,
    base: f64,
    size: f64,
}

/// Suites de rangées alignées formant des tableaux sans filets ; chaque suite
/// est une liste de rangées, chaque rangée une liste d'indices de fragments.
pub(super) fn aligned_runs(frags: &[Frag], med: f64) -> Vec<Vec<Vec<usize>>> {
    // Rangées par ligne de base (les fragments arrivent triés de haut en bas).
    let mut rows: Vec<Row> = Vec::new();
    for (i, f) in frags.iter().enumerate() {
        if f.angle.abs() > 0.05 {
            continue;
        }
        match rows.last_mut() {
            Some(r) if (r.base - f.base).abs() <= 0.5 * r.size.max(f.size) => r.frags.push(i),
            _ => rows.push(Row {
                frags: vec![i],
                base: f.base,
                size: f.size,
            }),
        }
    }
    for r in &mut rows {
        r.frags.sort_by(|a, b| {
            frags[*a]
                .bbox
                .x0
                .partial_cmp(&frags[*b].bbox.x0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    let is_candidate = |r: &Row| {
        if r.frags.len() < 2 {
            return false;
        }
        let x0 = r
            .frags
            .iter()
            .map(|&i| frags[i].bbox.x0)
            .fold(f64::MAX, f64::min);
        let x1 = r
            .frags
            .iter()
            .map(|&i| frags[i].bbox.x1)
            .fold(f64::MIN, f64::max);
        let span = (x1 - x0).max(1.0);
        r.frags
            .iter()
            .all(|&i| frags[i].words.len() <= 3 && frags[i].width() <= 0.4 * span)
    };
    let aligned = |a: &Row, b: &Row| {
        a.frags.len() == b.frags.len()
            && (a.base - b.base) <= 2.5 * a.size.max(b.size).max(med)
            && a.frags.iter().zip(&b.frags).all(|(&i, &j)| {
                let (p, q) = (&frags[i].bbox, &frags[j].bbox);
                let tol = 1.0 * a.size.max(b.size);
                (p.x0 - q.x0).abs() <= tol
                    || (p.x1 - q.x1).abs() <= tol
                    || ((p.x0 + p.x1) - (q.x0 + q.x1)).abs() / 2.0 <= tol
            })
    };
    let mut runs: Vec<Vec<Vec<usize>>> = Vec::new();
    let mut current: Vec<usize> = Vec::new(); // indices de rangées
    let flush = |current: &mut Vec<usize>, runs: &mut Vec<Vec<Vec<usize>>>| {
        if current.len() >= 2 {
            runs.push(current.iter().map(|&r| rows[r].frags.clone()).collect());
        }
        current.clear();
    };
    for r in 0..rows.len() {
        if !is_candidate(&rows[r]) {
            flush(&mut current, &mut runs);
            continue;
        }
        if let Some(&last) = current.last() {
            if !aligned(&rows[last], &rows[r]) {
                flush(&mut current, &mut runs);
            }
        }
        current.push(r);
    }
    flush(&mut current, &mut runs);
    runs
}

/// Indice de la colonne d'un tableau dont la cellule contient `x` (utilitaire de tests).
#[cfg(test)]
pub(super) fn column_of(xs: &[f64], x: f64) -> Option<usize> {
    xs.windows(2).position(|w| x >= w[0] && x < w[1])
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::super::test_support::page;
    use super::super::BlockKind;
    use super::*;

    #[test]
    fn ruled_grid_from_thin_rectangles() {
        // Grille 2 × 2 : 3 filets horizontaux, 3 verticaux (segments par rangée, à fusionner).
        let mut rules = Vec::new();
        for y in [100.0, 120.0, 140.0] {
            rules.push(Rect::new(50.0, y, 250.0, y + 0.75));
        }
        for x in [50.0, 150.0, 250.0] {
            rules.push(Rect::new(x, 100.0, x + 0.75, 120.0));
            rules.push(Rect::new(x, 120.0, x + 0.75, 140.0));
        }
        let mut p = page(&[
            (60.0, 128.0, 10.0, "Nom"),
            (160.0, 128.0, 10.0, "Valeur"),
            (60.0, 108.0, 10.0, "a"),
            (160.0, 108.0, 10.0, "1"),
            (50.0, 300.0, 10.0, "Texte au-dessus du tableau."),
        ]);
        p.rules = rules;
        p.analyze();
        assert_eq!(p.tables.len(), 1, "{:?}", p.tables);
        let t = &p.tables[0];
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].len(), 2);
        assert_eq!(t.rows[0][0].text, "Nom");
        assert_eq!(t.rows[0][1].text, "Valeur");
        assert_eq!(t.rows[1][1].text, "1");
        assert!(matches!(p.blocks.last().unwrap().kind, BlockKind::Table(0)));
        assert_eq!(column_of(&[0.0, 1.0, 2.0], 1.5), Some(1));
        let plain = p.to_plain();
        assert!(plain.contains("Nom\tValeur\na\t1"), "{plain:?}");
        assert!(
            p.to_markdown()
                .contains("| Nom | Valeur |\n| --- | --- |\n| a | 1 |"),
            "{}",
            p.to_markdown()
        );
    }

    #[test]
    fn horizontal_rules_only_columns_from_text() {
        let mut rules = Vec::new();
        for y in [100.0, 120.0, 140.0] {
            rules.push(Rect::new(50.0, y, 250.0, y + 0.5));
        }
        let mut p = page(&[
            (60.0, 128.0, 10.0, "Nom"),
            (160.0, 128.0, 10.0, "Valeur"),
            (60.0, 108.0, 10.0, "a"),
            (160.0, 108.0, 10.0, "1"),
        ]);
        p.rules = rules;
        p.analyze();
        assert_eq!(p.tables.len(), 1);
        assert_eq!(p.tables[0].rows[1][1].text, "1");
    }

    #[test]
    fn aligned_text_columns_without_rules() {
        let mut p = page(&[
            (
                50.0,
                700.0,
                10.0,
                "Intro du document en texte courant sur une ligne.",
            ),
            (50.0, 660.0, 10.0, "Article"),
            (200.0, 660.0, 10.0, "Prix"),
            (300.0, 660.0, 10.0, "Quantite"),
            (50.0, 648.0, 10.0, "Pomme"),
            (200.0, 648.0, 10.0, "1,20"),
            (300.0, 648.0, 10.0, "3"),
            (50.0, 636.0, 10.0, "Poire"),
            (200.0, 636.0, 10.0, "2,00"),
            (300.0, 636.0, 10.0, "5"),
        ]);
        p.analyze();
        assert_eq!(p.tables.len(), 1, "{:?}", p.tables);
        let t = &p.tables[0];
        assert_eq!(t.rows.len(), 3);
        assert_eq!(t.rows[2][0].text, "Poire");
        assert_eq!(t.rows[1][2].text, "3");
        assert!(p.to_plain().starts_with("Intro du document"));
    }

    #[test]
    fn prose_is_not_a_table() {
        let mut p = page(&[
            (
                50.0,
                700.0,
                10.0,
                "Une phrase ordinaire avec des mots de longueur variable ici.",
            ),
            (
                50.0,
                688.0,
                10.0,
                "Une autre phrase ordinaire qui suit la premiere sans tableau.",
            ),
            (
                50.0,
                676.0,
                10.0,
                "Et une troisieme pour faire bonne mesure et finir.",
            ),
        ]);
        p.analyze();
        assert!(p.tables.is_empty());
        assert_eq!(p.blocks.len(), 1);
        assert_eq!(p.blocks[0].paragraphs.len(), 1);
    }
}
