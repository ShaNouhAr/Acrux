//! Sélection de texte : modèle pur (sans fenêtre) qui transforme le texte
//! structuré d'une page (`acrux_features::text::PageText`) en une suite de
//! glyphes dans l'ordre de lecture, puis répond aux questions du visualiseur :
//! « quel curseur textuel est le plus proche de ce point ? », « quel texte
//! couvre cette plage ? », « quels rectangles surligner ? ».
//!
//! Vocabulaire : un *curseur* (caret) est une position **entre** deux glyphes,
//! numérotée de `0` (avant le premier) à `len()` (après le dernier). Une
//! sélection est une paire de curseurs ; le glyphe `i` est sélectionné quand
//! `début <= i < fin`. Ce modèle est celui de tous les éditeurs de texte et
//! évite les cas limites des sélections « inclusives ».

use acrux_core::{Point, Rect};
use acrux_features::text::{BlockKind, PageText};

/// Un glyphe sélectionnable.
#[derive(Debug, Clone)]
pub struct SelGlyph {
    /// Boîte en espace page (points PDF, origine en bas à gauche).
    pub rect: Rect,
    /// Texte Unicode.
    pub text: String,
    /// Indice de la ligne.
    pub line: usize,
    /// Indice du glyphe dans sa ligne, espaces compris (coordonnée d'édition).
    pub glyph_in_line: usize,
    /// Dernier glyphe de son mot (un espace suit dans le texte copié).
    pub word_end: bool,
    /// Dernier glyphe de sa ligne (un saut de ligne suit).
    pub line_end: bool,
}

/// Ligne : plage de glyphes et boîte.
#[derive(Debug, Clone)]
struct LineSpan {
    start: usize,
    end: usize,
    bbox: Rect,
    /// Bande horizontale (`x0`, `x1`) où un clic revient à cette ligne
    /// plutôt qu'à une voisine d'à côté : elle ne s'arrête qu'à mi-chemin
    /// d'une colonne, d'un encadré ou d'une cellule posés à la même hauteur,
    /// et s'étend sans limite quand rien n'est à côté.
    column: (f64, f64),
}

/// Distance d'une valeur à un intervalle (nulle dedans).
fn axis_distance(v: f64, lo: f64, hi: f64) -> f64 {
    if v < lo {
        lo - v
    } else if v > hi {
        v - hi
    } else {
        0.0
    }
}

/// Bande de chaque ligne (voir `LineSpan::column`).
///
/// Chaque ligne a d'abord sa **base** : la largeur du bloc (colonne,
/// encadré) qui la contient, s'il la contient vraiment — un paragraphe qui
/// continue d'une colonne à l'autre garde ses lignes de droite dans le bloc
/// de gauche —, ou sa propre largeur hors d'un bloc et dans un tableau, dont
/// les cellules sont côte à côte. La bande part de la base et s'étend de
/// chaque côté jusqu'à mi-chemin de la première zone voisine qui occupe la
/// même hauteur ; sans voisine, jusqu'au bout.
///
/// Borner la bande à la largeur du bloc ne suffisait pas : un titre, une
/// ligne de liste ou une rangée de tableau, plus étroits que le corps de la
/// page, perdaient le clic posé à leur droite au profit de la ligne du corps
/// la plus proche, au-dessus ou en dessous.
fn line_columns(text: &PageText) -> Vec<(f64, f64)> {
    // Base de chaque ligne, et zones de la page : un bloc de texte entier
    // (toute sa hauteur : une colonne voisine borne toutes les lignes d'à
    // côté, même décalées d'une demi-ligne), ou une ligne hors de tout bloc.
    let mut base: Vec<Option<(f64, f64)>> = vec![None; text.lines.len()];
    let mut zones: Vec<Rect> = Vec::new();
    for block in &text.blocks {
        if matches!(block.kind, BlockKind::Table(_)) {
            continue;
        }
        let (x0, x1) = (block.bbox.x0, block.bbox.x1);
        let mut zone = block.bbox;
        for &li in block.paragraphs.iter().flat_map(|p| p.lines.iter()) {
            let Some(line) = text.lines.get(li) else {
                continue;
            };
            let mid = f64::midpoint(line.bbox.x0, line.bbox.x1);
            if (x0..=x1).contains(&mid) {
                base[li] = Some((x0.min(line.bbox.x0), x1.max(line.bbox.x1)));
                zone = zone.union(&line.bbox);
            }
        }
        zones.push(zone);
    }
    for (li, line) in text.lines.iter().enumerate() {
        if base[li].is_none() {
            zones.push(line.bbox);
        }
    }
    text.lines
        .iter()
        .zip(&base)
        .map(|(line, base)| {
            let (b0, b1) = base.unwrap_or((line.bbox.x0, line.bbox.x1));
            let (mut lo, mut hi) = (f64::NEG_INFINITY, f64::INFINITY);
            for z in &zones {
                let beside = z.y0.max(line.bbox.y0) < z.y1.min(line.bbox.y1);
                if !beside {
                    continue;
                }
                if z.x1 <= b0 {
                    lo = lo.max(f64::midpoint(z.x1, b0));
                } else if z.x0 >= b1 {
                    hi = hi.min(f64::midpoint(b1, z.x0));
                }
            }
            (lo, hi)
        })
        .collect()
}

/// Texte d'une page prêt pour la sélection.
#[derive(Debug, Clone, Default)]
pub struct SelectableText {
    glyphs: Vec<SelGlyph>,
    lines: Vec<LineSpan>,
}

/// Résultat d'un test de position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// Curseur le plus proche.
    pub caret: usize,
    /// Le point est réellement sur du texte (boîte d'une ligne), pas dans
    /// une zone vide : sert à décider entre sélection et déplacement.
    pub on_text: bool,
}

impl SelectableText {
    /// Ligne et plage de glyphes (au sens de `acrux_features::text`) couvertes
    /// par `from..to`, si toute la plage tient sur une seule ligne. L'édition
    /// de texte travaille ligne par ligne : une plage à cheval demanderait de
    /// recomposer le paragraphe, ce qui est une autre opération.
    #[must_use]
    pub fn line_range(&self, from: usize, to: usize) -> Option<(usize, usize, usize)> {
        let slice = self.glyphs.get(from..to)?;
        let first = slice.first()?;
        let last = slice.last()?;
        if slice.iter().any(|g| g.line != first.line) {
            return None;
        }
        Some((first.line, first.glyph_in_line, last.glyph_in_line + 1))
    }

    /// Construit la suite de glyphes à partir du texte structuré.
    #[must_use]
    pub fn from_page_text(text: &PageText) -> Self {
        let mut glyphs = Vec::new();
        let mut lines = Vec::new();
        let columns = line_columns(text);
        for (li, line) in text.lines.iter().enumerate() {
            let start = glyphs.len();
            // Indice du glyphe dans la ligne au sens de `acrux_features::text`
            // (espaces compris) : c'est ce que demande l'édition de texte.
            let mut in_line = 0usize;
            for (wi, word) in line.words.iter().enumerate() {
                let last_word = wi + 1 == line.words.len();
                let n = word.glyphs.len();
                for (gi, g) in word.glyphs.iter().enumerate() {
                    let position = in_line;
                    in_line += 1;
                    if g.is_space {
                        continue;
                    }
                    let last = gi + 1 == n;
                    glyphs.push(SelGlyph {
                        rect: g.bbox,
                        text: g.text.clone(),
                        line: li,
                        glyph_in_line: position,
                        word_end: last,
                        line_end: last && last_word,
                    });
                }
            }
            if glyphs.len() > start {
                // Un mot dont tous les glyphes sont des espaces peut laisser
                // `line_end` faux : on le force sur le dernier glyphe réel.
                if let Some(g) = glyphs.last_mut() {
                    g.line_end = true;
                    g.word_end = true;
                }
                lines.push(LineSpan {
                    start,
                    end: glyphs.len(),
                    bbox: line.bbox,
                    column: columns[li],
                });
            }
        }
        Self { glyphs, lines }
    }

    /// Nombre de glyphes (le curseur maximal).
    #[must_use]
    pub fn len(&self) -> usize {
        self.glyphs.len()
    }

    /// Aucun texte.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }

    /// Curseur le plus proche d'un point de la page.
    ///
    /// Choisit d'abord la **colonne** sous le point (ou la plus proche), puis
    /// dans cette colonne la ligne la plus proche verticalement (distance
    /// nulle si le point est dedans), enfin la position horizontale : avant
    /// le premier glyphe, après le dernier, ou de part et d'autre du milieu
    /// d'un glyphe. Au-dessus de tout texte → `0`, en dessous → `len()`.
    ///
    /// La colonne d'abord : sur une page à deux colonnes, les lignes de
    /// gauche et de droite sont à la même hauteur, et la seule distance
    /// verticale choisissait toujours celle de gauche — cliquer ou glisser
    /// dans la colonne de droite sélectionnait à gauche, ou rien. La fin
    /// d'une ligne courte se clique toujours à sa droite : la colonne
    /// entière est à distance nulle.
    #[must_use]
    pub fn hit(&self, p: Point) -> Option<Hit> {
        if self.lines.is_empty() {
            return None;
        }
        let mut best: Option<((f64, f64, f64), usize)> = None;
        for (i, l) in self.lines.iter().enumerate() {
            let key = (
                axis_distance(p.x, l.column.0, l.column.1),
                axis_distance(p.y, l.bbox.y0, l.bbox.y1),
                axis_distance(p.x, l.bbox.x0, l.bbox.x1),
            );
            if best.is_none_or(|(bk, _)| key < bk) {
                best = Some((key, i));
            }
        }
        let ((_, dist, _), li) = best?;
        let line = &self.lines[li];
        let inside_v = dist == 0.0;
        // Au-dessus de la première ligne / en dessous de la dernière : extrémités.
        if !inside_v {
            if li == 0 && p.y > line.bbox.y1 {
                return Some(Hit {
                    caret: 0,
                    on_text: false,
                });
            }
            if li + 1 == self.lines.len() && p.y < line.bbox.y0 {
                return Some(Hit {
                    caret: self.glyphs.len(),
                    on_text: false,
                });
            }
        }
        let on_text = inside_v && p.x >= line.bbox.x0 && p.x <= line.bbox.x1;
        let caret = self.caret_in_line(line, p.x);
        Some(Hit { caret, on_text })
    }

    fn caret_in_line(&self, line: &LineSpan, x: f64) -> usize {
        let mut caret = line.end;
        for i in line.start..line.end {
            let r = &self.glyphs[i].rect;
            let mid = f64::midpoint(r.x0, r.x1);
            if x < mid {
                caret = i;
                break;
            }
        }
        caret
    }

    /// Plage du mot contenant (ou précédant) le curseur.
    #[must_use]
    pub fn word_at(&self, caret: usize) -> (usize, usize) {
        if self.glyphs.is_empty() {
            return (0, 0);
        }
        let i = caret.min(self.glyphs.len() - 1);
        let mut start = i;
        while start > 0 && !self.glyphs[start - 1].word_end {
            start -= 1;
        }
        let mut end = i;
        while end < self.glyphs.len() && !self.glyphs[end].word_end {
            end += 1;
        }
        (start, (end + 1).min(self.glyphs.len()))
    }

    /// Plage de la ligne contenant (ou précédant) le curseur.
    #[must_use]
    pub fn line_at(&self, caret: usize) -> (usize, usize) {
        if self.glyphs.is_empty() {
            return (0, 0);
        }
        let i = caret.min(self.glyphs.len() - 1);
        let li = self.glyphs[i].line;
        self.lines
            .iter()
            .find(|l| self.glyphs[l.start].line == li)
            .map_or((0, 0), |l| (l.start, l.end))
    }

    /// Texte d'une plage de curseurs : espaces entre les mots, sauts de
    /// ligne entre les lignes.
    #[must_use]
    pub fn text(&self, start: usize, end: usize) -> String {
        let end = end.min(self.glyphs.len());
        let mut out = String::new();
        for i in start..end {
            let g = &self.glyphs[i];
            out.push_str(&g.text);
            if i + 1 < end {
                if g.line_end {
                    out.push('\n');
                } else if g.word_end {
                    out.push(' ');
                }
            }
        }
        out
    }

    /// Rectangles à surligner : un par ligne, union des glyphes sélectionnés.
    #[must_use]
    pub fn rects(&self, start: usize, end: usize) -> Vec<Rect> {
        let end = end.min(self.glyphs.len());
        let mut out: Vec<Rect> = Vec::new();
        let mut current: Option<(usize, Rect)> = None;
        for g in &self.glyphs[start.min(end)..end] {
            match &mut current {
                Some((line, r)) if *line == g.line => {
                    *r = r.union(&g.rect);
                }
                _ => {
                    if let Some((_, r)) = current.take() {
                        out.push(r);
                    }
                    current = Some((g.line, g.rect));
                }
            }
        }
        if let Some((_, r)) = current {
            out.push(r);
        }
        out
    }
}

/// Position dans le document : page et curseur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPos {
    /// Indice de page.
    pub page: usize,
    /// Curseur dans la page.
    pub caret: usize,
}

/// Sélection courante : ancre (point de départ) et focus (point mobile).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Où la sélection a commencé.
    pub anchor: TextPos,
    /// Où elle se termine (peut précéder l'ancre).
    pub focus: TextPos,
}

impl Selection {
    /// Sélection réduite à un point.
    #[must_use]
    pub fn collapsed(pos: TextPos) -> Self {
        Self {
            anchor: pos,
            focus: pos,
        }
    }

    /// Bornes ordonnées.
    #[must_use]
    pub fn ordered(&self) -> (TextPos, TextPos) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// Vide (ancre et focus confondus).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }

    /// Plage de curseurs couverte sur une page donnée, ou `None` si la page
    /// n'est pas concernée. `len` est le nombre de glyphes de la page.
    #[must_use]
    pub fn range_on_page(&self, page: usize, len: usize) -> Option<(usize, usize)> {
        let (s, e) = self.ordered();
        if page < s.page || page > e.page {
            return None;
        }
        let start = if page == s.page { s.caret } else { 0 };
        let end = if page == e.page { e.caret } else { len };
        (start < end).then_some((start.min(len), end.min(len)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::cast_precision_loss)] // tests : données construites à la main
mod tests {
    use super::*;
    use acrux_features::text::{Glyph, Line, Word};

    fn glyph(x: f64, y: f64, c: &str) -> Glyph {
        Glyph {
            text: c.to_string(),
            bbox: Rect::new(x, y, x + 10.0, y + 10.0),
            size: 10.0,
            font: String::new(),
            is_space: false,
            angle: 0.0,
            along: x,
            along_end: x + 10.0,
            perp: y,
            ..Glyph::default()
        }
    }

    fn word(x: f64, y: f64, s: &str) -> Word {
        let glyphs: Vec<Glyph> = s
            .chars()
            .enumerate()
            .map(|(i, c)| glyph(x + 10.0 * i as f64, y, &c.to_string()))
            .collect();
        let bbox = glyphs.iter().fold(glyphs[0].bbox, |r, g| r.union(&g.bbox));
        Word {
            text: s.to_string(),
            bbox,
            glyphs,
            ..Word::default()
        }
    }

    fn page() -> PageText {
        let l1 = vec![word(0.0, 100.0, "ab"), word(30.0, 100.0, "cd")];
        let l2 = vec![word(0.0, 80.0, "ef")];
        let bb = |ws: &[Word]| ws.iter().fold(ws[0].bbox, |r, w| r.union(&w.bbox));
        PageText {
            lines: vec![
                Line {
                    bbox: bb(&l1),
                    words: l1,
                },
                Line {
                    bbox: bb(&l2),
                    words: l2,
                },
            ],
            ..PageText::default()
        }
    }

    #[test]
    fn text_and_boundaries() {
        let t = SelectableText::from_page_text(&page());
        assert_eq!(t.len(), 6);
        assert_eq!(t.text(0, 6), "ab cd\nef");
        assert_eq!(t.text(1, 3), "b c");
        assert_eq!(t.word_at(3), (2, 4));
        assert_eq!(t.line_at(5), (4, 6));
        assert_eq!(t.rects(0, 6).len(), 2);
        assert_eq!(t.rects(1, 3)[0], Rect::new(10.0, 100.0, 40.0, 110.0));
    }

    #[test]
    fn line_range_maps_to_edit_coordinates() {
        let t = SelectableText::from_page_text(&page());
        // Ligne 0 : « ab » puis « cd », soit les glyphes 0..4 de la ligne.
        assert_eq!(t.line_range(0, 2), Some((0, 0, 2)));
        assert_eq!(t.line_range(2, 4), Some((0, 2, 4)));
        assert_eq!(t.line_range(1, 4), Some((0, 1, 4)));
        // Ligne 1 : les indices repartent de zéro dans sa propre ligne.
        assert_eq!(t.line_range(4, 6), Some((1, 0, 2)));
        // À cheval sur deux lignes : refusé.
        assert_eq!(t.line_range(3, 5), None);
        assert_eq!(t.line_range(0, 0), None);
    }

    #[test]
    fn hit_testing() {
        let t = SelectableText::from_page_text(&page());
        // Sur le « b » (x 10..20), à droite du milieu → curseur 2.
        let h = t.hit(Point::new(17.0, 105.0)).unwrap();
        assert_eq!(
            h,
            Hit {
                caret: 2,
                on_text: true
            }
        );
        // Zone vide à droite de la ligne 1 → fin de ligne, pas sur du texte.
        let h = t.hit(Point::new(200.0, 105.0)).unwrap();
        assert_eq!(h.caret, 4);
        assert!(!h.on_text);
        // Au-dessus de tout → 0 ; en dessous → len.
        assert_eq!(t.hit(Point::new(5.0, 300.0)).unwrap().caret, 0);
        assert_eq!(t.hit(Point::new(5.0, -50.0)).unwrap().caret, 6);
        // Dans l'interligne : ligne la plus proche.
        assert_eq!(t.hit(Point::new(3.0, 92.0)).unwrap().caret, 4);
        assert!(SelectableText::default()
            .hit(Point::new(0.0, 0.0))
            .is_none());
    }

    /// Deux colonnes : « ab cd » / « ef » à gauche, « gh ij » / « kl » à
    /// droite, aux mêmes hauteurs, chacune dans son bloc.
    fn two_columns() -> PageText {
        use acrux_features::text::{Block, Paragraph};
        let bb = |ws: &[Word]| ws.iter().fold(ws[0].bbox, |r, w| r.union(&w.bbox));
        let rows = [
            vec![word(0.0, 100.0, "ab"), word(30.0, 100.0, "cd")],
            vec![word(0.0, 80.0, "ef")],
            vec![word(200.0, 100.0, "gh"), word(230.0, 100.0, "ij")],
            vec![word(200.0, 80.0, "kl")],
        ];
        let lines: Vec<Line> = rows
            .into_iter()
            .map(|ws| Line {
                bbox: bb(&ws),
                words: ws,
            })
            .collect();
        let block = |x0: f64, lines: Vec<usize>| Block {
            bbox: Rect::new(x0, 80.0, x0 + 100.0, 110.0),
            paragraphs: vec![Paragraph {
                lines,
                ..Paragraph::default()
            }],
            ..Block::default()
        };
        PageText {
            lines,
            blocks: vec![block(0.0, vec![0, 1]), block(200.0, vec![2, 3])],
            ..PageText::default()
        }
    }

    #[test]
    fn la_colonne_de_droite_se_selectionne() {
        let t = SelectableText::from_page_text(&two_columns());
        // Sur le « h » de « gh » (x 210..220), à droite du milieu.
        let h = t.hit(Point::new(217.0, 105.0)).unwrap();
        assert_eq!(
            h,
            Hit {
                caret: 8,
                on_text: true
            }
        );
        assert_eq!(t.text(6, 10), "gh ij");
        let (a, b) = t.word_at(h.caret - 1);
        assert_eq!(t.text(a, b), "gh");
        // Dans l'interligne de droite : une ligne de droite, pas celle de
        // gauche qui serait à la même hauteur.
        assert_eq!(t.hit(Point::new(203.0, 92.0)).unwrap().caret, 10);
        // À droite de la courte « kl », dans sa colonne : sa fin.
        let h = t.hit(Point::new(280.0, 85.0)).unwrap();
        assert_eq!(h.caret, 12);
        assert!(!h.on_text);
        // La colonne de gauche ne bouge pas : à droite de « ef », sa fin,
        // même si « cd », plus longue, est juste au-dessus.
        assert_eq!(t.hit(Point::new(60.0, 85.0)).unwrap().caret, 6);
        // Dans la gouttière, la colonne la plus proche.
        assert_eq!(t.hit(Point::new(190.0, 105.0)).unwrap().caret, 6);
        assert_eq!(t.hit(Point::new(110.0, 105.0)).unwrap().caret, 4);
    }

    /// Un titre court, dans son propre bloc, au-dessus d'un corps plus
    /// large : le clic posé à droite du titre, à sa hauteur, va au bout du
    /// titre, pas à la ligne du corps la plus proche.
    #[test]
    fn a_droite_d_un_titre_etroit() {
        use acrux_features::text::{Block, Paragraph};
        let bb = |ws: &[Word]| ws.iter().fold(ws[0].bbox, |r, w| r.union(&w.bbox));
        let rows = [
            vec![word(0.0, 120.0, "ti")],
            vec![word(0.0, 100.0, "abcdefghij"), word(110.0, 100.0, "klmnop")],
            vec![word(0.0, 88.0, "qrstuvwxyz"), word(110.0, 88.0, "abcdef")],
        ];
        let lines: Vec<Line> = rows
            .into_iter()
            .map(|ws| Line {
                bbox: bb(&ws),
                words: ws,
            })
            .collect();
        let block = |bbox: Rect, lines: Vec<usize>| Block {
            bbox,
            paragraphs: vec![Paragraph {
                lines,
                ..Paragraph::default()
            }],
            ..Block::default()
        };
        let t = SelectableText::from_page_text(&PageText {
            blocks: vec![
                block(lines[0].bbox, vec![0]),
                block(lines[1].bbox.union(&lines[2].bbox), vec![1, 2]),
            ],
            lines,
            ..PageText::default()
        });
        let title = t.hit(Point::new(80.0, 125.0)).unwrap();
        assert_eq!(title.caret, 2, "le bout du titre");
        assert!(!title.on_text);
        // Dans le corps, rien ne change.
        assert_eq!(t.hit(Point::new(80.0, 105.0)).unwrap().caret, 10);
    }

    #[test]
    fn selection_ranges() {
        let s = Selection {
            anchor: TextPos { page: 2, caret: 3 },
            focus: TextPos { page: 0, caret: 5 },
        };
        assert_eq!(s.range_on_page(0, 9), Some((5, 9)));
        assert_eq!(s.range_on_page(1, 4), Some((0, 4)));
        assert_eq!(s.range_on_page(2, 9), Some((0, 3)));
        assert_eq!(s.range_on_page(3, 9), None);
        assert!(Selection::collapsed(TextPos { page: 0, caret: 0 }).is_empty());
    }
}
