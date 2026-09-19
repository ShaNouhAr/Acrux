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
use acrux_features::text::PageText;

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
    /// Choisit la ligne la plus proche verticalement (distance nulle si le
    /// point est dedans), puis la position horizontale : avant le premier
    /// glyphe, après le dernier, ou de part et d'autre du milieu d'un glyphe.
    /// Au-dessus de tout texte → `0`, en dessous → `len()`.
    #[must_use]
    pub fn hit(&self, p: Point) -> Option<Hit> {
        if self.lines.is_empty() {
            return None;
        }
        let mut best: Option<(f64, usize)> = None;
        for (i, l) in self.lines.iter().enumerate() {
            let d = if p.y > l.bbox.y1 {
                p.y - l.bbox.y1
            } else if p.y < l.bbox.y0 {
                l.bbox.y0 - p.y
            } else {
                0.0
            };
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, i));
            }
        }
        let (dist, li) = best?;
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
