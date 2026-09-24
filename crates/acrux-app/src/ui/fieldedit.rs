//! Saisie dans un champ posé sur la page : le curseur, la sélection, la
//! frappe, le presse-papiers et les étapes d'annulation d'un texte court ou
//! de quelques lignes.
//!
//! C'est ce qui fait qu'on remplit un formulaire **sur place**, comme dans
//! Acrobat : un clic pose le curseur dans le champ, on tape, on sélectionne,
//! on colle, Tab passe au champ suivant. Rien ici ne sait ce qu'est un
//! formulaire ni un document : le module tient le texte et sa mise en
//! lignes, l'appelant le dessine et l'écrit. Les zones de texte et les
//! réponses aux commentaires s'en serviront de même.
//!
//! La mise en lignes se fait avec la mesure que donne l'appelant (une
//! fermeture « largeur d'un texte »), dans l'unité qu'il veut — des pixels à
//! l'écran, une chasse fixe dans les épreuves. Les coupures sont celles de
//! l'apparence écrite par `acrux_features::forms` : aux espaces, puis au
//! caractère pour un mot plus large que le champ.
//!
//! La mesure est supposée **additive** (la largeur d'un texte est la somme de
//! celles de ses caractères), ce qu'est celle du texte d'interface : c'est ce
//! qui permet de retrouver le caractère sous un clic.

// Positions en pixels fractionnaires, indices en caractères.
#![allow(clippy::cast_precision_loss)]

use crate::platform::{Key, Modifiers};
use crate::ui::editpdf::{step_of, Buffer, Step, StepKind};

/// Mesure d'un texte : sa largeur, dans l'unité de l'appelant.
pub type Measure<'a> = &'a mut dyn FnMut(&str) -> f32;

/// Caractère qui remplace chaque lettre d'un champ masqué.
pub const MASK: char = '\u{2022}';

/// Ce qu'une touche a fait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKey {
    /// Rien : la touche ne concerne pas la saisie.
    None,
    /// Le texte a changé.
    Changed,
    /// Seuls le curseur ou la sélection ont bougé.
    Moved,
    /// Valider (Entrée, ou Ctrl+Entrée sur plusieurs lignes).
    Commit,
    /// Annuler la saisie (Échap).
    Cancel,
    /// Passer au champ suivant (`true`, Tab) ou précédent (Maj+Tab).
    Next(bool),
}

/// Une ligne à l'écran : les caractères `start..end` du texte, et sa
/// largeur. Le blanc où elle a été coupée n'en fait pas partie, et le
/// curseur posé sur lui s'affiche en bout de ligne.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Line {
    /// Premier caractère.
    pub start: usize,
    /// Caractère qui suit le dernier.
    pub end: usize,
    /// Largeur, dans l'unité de la mesure.
    pub width: f32,
}

/// La saisie d'un champ.
#[derive(Debug, Clone)]
pub struct FieldEditor {
    /// Texte, curseur et sélection.
    pub buffer: Buffer,
    /// Nombre de caractères permis (`/MaxLen`).
    pub max_len: Option<usize>,
    /// Plusieurs lignes : Entrée passe à la ligne.
    pub multiline: bool,
    /// Peigne : une case par caractère, ce nombre de cases.
    pub comb: Option<usize>,
    /// Masqué (mot de passe) : affiché en points, jamais copié.
    pub masked: bool,
    /// Défilement : horizontal sur une ligne, vertical sur plusieurs.
    pub scroll: f32,
    /// Étapes annulables, de la plus ancienne à la plus récente.
    undo: Vec<Step>,
    /// Étapes rétablissables.
    redo: Vec<Step>,
    /// Nature du dernier geste de frappe.
    last_step: StepKind,
    /// Curseur à la fin du dernier geste.
    last_caret: usize,
}

/// Nombre d'étapes gardées : une séance de frappe tient largement dedans.
const MAX_STEPS: usize = 60;

impl FieldEditor {
    /// Saisie neuve sur `text`, curseur à la fin.
    #[must_use]
    pub fn new(
        text: &str,
        max_len: Option<usize>,
        multiline: bool,
        comb: Option<usize>,
        masked: bool,
    ) -> Self {
        let buffer = Buffer::new(text);
        let caret = buffer.caret;
        Self {
            buffer,
            max_len,
            multiline,
            comb: comb.filter(|n| *n > 0),
            masked,
            scroll: 0.0,
            undo: Vec::new(),
            redo: Vec::new(),
            last_step: StepKind::None,
            last_caret: caret,
        }
    }

    /// Le texte tel qu'il se montre : des points pour un champ masqué.
    #[must_use]
    pub fn display(&self) -> String {
        if self.masked {
            self.buffer.text.chars().map(|_| MASK).collect()
        } else {
            self.buffer.text.clone()
        }
    }

    /// Texte à copier : la sélection, jamais celle d'un champ masqué (un mot
    /// de passe ne sort pas du champ, comme dans Acrobat).
    #[must_use]
    pub fn copy_text(&self) -> Option<String> {
        if self.masked || !self.buffer.has_selection() {
            return None;
        }
        Some(self.buffer.selected())
    }

    /// Coupe la sélection : la rend, et l'efface du champ.
    pub fn cut(&mut self) -> Option<String> {
        let text = self.copy_text()?;
        let before = self.buffer.clone();
        self.buffer.insert("");
        self.note(&before);
        Some(text)
    }

    /// Insère un texte au curseur (ou à la place de la sélection) ; vrai si
    /// le champ a changé.
    ///
    /// Sur une ligne, les sauts de ligne de la fin d'un collage sont retirés
    /// et les autres deviennent des espaces, comme dans un champ de saisie
    /// de Windows. Ce qui dépasse `/MaxLen` est laissé de côté : un peigne
    /// n'a que ses cases.
    pub fn insert(&mut self, s: &str) -> bool {
        let before = self.buffer.clone();
        let mut text = if self.multiline {
            s.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            s.trim_end_matches(['\r', '\n'])
                .replace("\r\n", " ")
                .replace(['\r', '\n'], " ")
        };
        if let Some(max) = self.max_len {
            let (a, b) = self.buffer.range();
            let room = max.saturating_sub(self.buffer.len() - (b - a));
            text = text.chars().take(room).collect();
        }
        if text.is_empty() && !self.buffer.has_selection() {
            return false;
        }
        self.buffer.insert(&text);
        self.note(&before);
        self.buffer.text != before.text
    }

    /// Retient l'état d'avant un geste qui a changé le texte, s'il ouvre
    /// une nouvelle étape d'annulation (voir [`step_of`]).
    fn note(&mut self, before: &Buffer) {
        if self.buffer.text == before.text {
            return;
        }
        let (kind, opens) = step_of(before, &self.buffer, self.last_step, self.last_caret);
        if opens || self.undo.is_empty() {
            self.undo.push(Step {
                text: before.text.clone(),
                caret: before.caret,
                anchor: before.anchor,
            });
            if self.undo.len() > MAX_STEPS {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_step = kind;
        self.last_caret = self.buffer.caret;
    }

    /// Vrai s'il reste une étape à défaire dans le champ.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Vrai s'il reste une étape à refaire dans le champ.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Remplace le texte par une étape, en rangeant l'état courant dans
    /// l'autre pile.
    fn restore(&mut self, step: Step, forward: bool) {
        let now = Step {
            text: self.buffer.text.clone(),
            caret: self.buffer.caret,
            anchor: self.buffer.anchor,
        };
        if forward {
            self.undo.push(now);
        } else {
            self.redo.push(now);
        }
        let n = step.text.chars().count();
        self.buffer.text = step.text;
        self.buffer.caret = step.caret.min(n);
        self.buffer.anchor = step.anchor.min(n);
        self.last_step = StepKind::None;
        self.last_caret = self.buffer.caret;
    }

    /// Défait la dernière étape ; faux s'il n'y a plus rien à défaire ici.
    pub fn undo_step(&mut self) -> bool {
        let Some(step) = self.undo.pop() else {
            return false;
        };
        self.restore(step, false);
        true
    }

    /// Refait l'étape défaite ; faux s'il n'y en a pas.
    pub fn redo_step(&mut self) -> bool {
        let Some(step) = self.redo.pop() else {
            return false;
        };
        self.restore(step, true);
        true
    }

    /// Une touche. `measure` et `width` (largeur disponible) servent aux
    /// déplacements par ligne d'un champ de plusieurs lignes.
    pub fn key(&mut self, key: Key, m: Modifiers, measure: Measure, width: f32) -> FieldKey {
        let before = self.buffer.clone();
        let len = self.buffer.len();
        match key {
            Key::Escape => return FieldKey::Cancel,
            Key::Tab => return FieldKey::Next(!m.shift),
            Key::Enter => {
                if self.multiline && !m.ctrl {
                    return if self.insert("\n") {
                        FieldKey::Changed
                    } else {
                        FieldKey::None
                    };
                }
                return FieldKey::Commit;
            }
            Key::Left if m.ctrl => self.buffer.word_left(m.shift),
            Key::Right if m.ctrl => self.buffer.word_right(m.shift),
            Key::Left => self.buffer.left(m.shift),
            Key::Right => self.buffer.right(m.shift),
            Key::Home if m.ctrl || !self.multiline => self.buffer.move_to(0, m.shift),
            Key::End if m.ctrl || !self.multiline => self.buffer.move_to(len, m.shift),
            Key::Home | Key::End => {
                let lines = self.layout(measure, width);
                let line = lines[line_of(&lines, self.buffer.caret)];
                let to = if key == Key::Home {
                    line.start
                } else {
                    line.end
                };
                self.buffer.move_to(to, m.shift);
            }
            Key::Up | Key::Down if self.multiline => {
                let (x, line) = self.caret_xy(measure, width);
                let lines = self.layout(measure, width);
                let to = if key == Key::Up {
                    match line.checked_sub(1) {
                        Some(above) => self.index_at(measure, width, x, above),
                        None => 0,
                    }
                } else if line + 1 < lines.len() {
                    self.index_at(measure, width, x, line + 1)
                } else {
                    len
                };
                self.buffer.move_to(to, m.shift);
            }
            Key::Backspace => {
                // Ctrl+Retour arrière efface le mot qui précède.
                if m.ctrl && !self.buffer.has_selection() {
                    self.buffer.word_left(true);
                }
                self.buffer.backspace();
            }
            Key::Delete => {
                if m.ctrl && !self.buffer.has_selection() {
                    self.buffer.word_right(true);
                }
                self.buffer.delete();
            }
            _ => return FieldKey::None,
        }
        if self.buffer.text != before.text {
            self.note(&before);
            FieldKey::Changed
        } else if self.buffer != before {
            FieldKey::Moved
        } else {
            FieldKey::None
        }
    }

    /// Largeur d'une case d'un peigne occupant `width`.
    fn cell(&self, width: f32) -> Option<f32> {
        self.comb.map(|n| width / n as f32)
    }

    /// Les lignes du texte affiché, pour une largeur disponible `width`.
    ///
    /// Une ligne unique pour un champ d'une ligne (il défile) ou un peigne ;
    /// sur plusieurs lignes, un paragraphe par saut de ligne, coupé aux
    /// espaces, puis au caractère pour un mot trop large.
    pub fn layout(&self, measure: Measure, width: f32) -> Vec<Line> {
        let chars: Vec<char> = self.display().chars().collect();
        let n = chars.len();
        if let Some(cell) = self.cell(width) {
            return vec![Line {
                start: 0,
                end: n,
                width: cell * n as f32,
            }];
        }
        if !self.multiline {
            let all: String = chars.iter().collect();
            return vec![Line {
                start: 0,
                end: n,
                width: measure(&all),
            }];
        }
        let mut lines = Vec::new();
        let mut start = 0;
        loop {
            let end = (start..n).find(|&i| chars[i] == '\n').unwrap_or(n);
            wrap(&chars, start, end, measure, width, &mut lines);
            if end >= n {
                break;
            }
            start = end + 1;
        }
        lines
    }

    /// Position du curseur : abscisse dans sa ligne et rang de la ligne.
    pub fn caret_xy(&self, measure: Measure, width: f32) -> (f32, usize) {
        let caret = self.buffer.caret;
        if let Some(cell) = self.cell(width) {
            return (cell * caret as f32, 0);
        }
        let lines = self.layout(measure, width);
        let line = line_of(&lines, caret);
        let l = lines[line];
        let prefix: String = self
            .display()
            .chars()
            .skip(l.start)
            .take(caret.min(l.end).saturating_sub(l.start))
            .collect();
        (measure(&prefix), line)
    }

    /// Rang du caractère le plus proche de l'abscisse `x` sur la ligne
    /// `line` : là où un clic pose le curseur.
    pub fn index_at(&self, measure: Measure, width: f32, x: f32, line: usize) -> usize {
        let len = self.buffer.len();
        if let Some(cell) = self.cell(width) {
            let at = (x / cell.max(0.001)).round().max(0.0);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // positif, borné
            let at = at as usize;
            return at.min(len);
        }
        let lines = self.layout(measure, width);
        let Some(l) = lines.get(line.min(lines.len().saturating_sub(1))).copied() else {
            return 0;
        };
        let shown: Vec<char> = self.display().chars().collect();
        // Le bord gauche de chaque caractère, puis le bout de la ligne : le
        // plus proche de `x` l'emporte.
        let mut best = (x.abs(), l.start);
        let mut pen = 0.0_f32;
        for (i, c) in shown.iter().enumerate().take(l.end).skip(l.start) {
            pen += measure(&c.to_string());
            let gap = (pen - x).abs();
            if gap < best.0 {
                best = (gap, i + 1);
            }
        }
        best.1
    }

    /// Fait défiler pour que le curseur reste visible dans une boîte de
    /// `width` × `height`, lignes de `line_h`.
    pub fn ensure_visible(&mut self, measure: Measure, width: f32, height: f32, line_h: f32) {
        if self.comb.is_some() {
            self.scroll = 0.0;
            return;
        }
        let (x, line) = self.caret_xy(measure, width);
        if self.multiline {
            let top = line as f32 * line_h;
            let bottom = top + line_h;
            if top < self.scroll {
                self.scroll = top;
            } else if bottom > self.scroll + height {
                self.scroll = bottom - height;
            }
            let lines = self.layout(measure, width);
            let content = lines.len() as f32 * line_h;
            self.scroll = self.scroll.clamp(0.0, (content - height).max(0.0));
            return;
        }
        let content = self.layout(measure, width)[0].width;
        if content <= width {
            self.scroll = 0.0;
            return;
        }
        if x - self.scroll > width - 1.0 {
            self.scroll = x - width + 1.0;
        } else if x < self.scroll {
            self.scroll = x;
        }
        self.scroll = self.scroll.clamp(0.0, (content - width + 1.0).max(0.0));
    }
}

/// Rang de la ligne qui porte l'indice `index` : la dernière qui commence
/// avant lui.
#[must_use]
pub fn line_of(lines: &[Line], index: usize) -> usize {
    lines.iter().rposition(|l| l.start <= index).unwrap_or(0)
}

/// Coupe le paragraphe `start..end` en lignes de largeur `width` au plus :
/// aux espaces d'abord, puis au caractère pour un mot seul trop large. C'est
/// la règle de l'apparence écrite (`wrap_lines` d'`acrux_features::forms`).
fn wrap(
    chars: &[char],
    start: usize,
    end: usize,
    measure: Measure,
    width: f32,
    out: &mut Vec<Line>,
) {
    let text = |a: usize, b: usize| -> String { chars[a..b].iter().collect() };
    let mut line_start = start;
    let mut line_end = start;
    let mut has_word = false;
    let mut i = start;
    loop {
        let word_end = (i..end).find(|&k| chars[k] == ' ').unwrap_or(end);
        let fits = measure(&text(line_start, word_end)) <= width;
        if fits || (!has_word && word_end == i) {
            line_end = word_end;
        } else {
            if has_word {
                out.push(Line {
                    start: line_start,
                    end: line_end,
                    width: measure(&text(line_start, line_end)),
                });
            }
            // Le mot, seul sur sa ligne : coupé au caractère s'il dépasse.
            let mut chunk = i;
            for k in i..word_end {
                if k > chunk && measure(&text(chunk, k + 1)) > width {
                    out.push(Line {
                        start: chunk,
                        end: k,
                        width: measure(&text(chunk, k)),
                    });
                    chunk = k;
                }
            }
            line_start = chunk;
            line_end = word_end;
        }
        has_word = true;
        if word_end >= end {
            break;
        }
        i = word_end + 1;
    }
    out.push(Line {
        start: line_start,
        end: line_end,
        width: measure(&text(line_start, line_end)),
    });
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // mesures entières, exactes en f32
mod tests {
    use super::*;

    /// Chasse fixe : dix unités par caractère.
    fn fixed(s: &str) -> f32 {
        s.chars().count() as f32 * 10.0
    }

    fn plain(text: &str) -> FieldEditor {
        FieldEditor::new(text, None, false, None, false)
    }

    fn lines(e: &FieldEditor, width: f32) -> Vec<(usize, usize)> {
        e.layout(&mut fixed, width)
            .iter()
            .map(|l| (l.start, l.end))
            .collect()
    }

    fn press(e: &mut FieldEditor, key: Key, m: Modifiers) -> FieldKey {
        e.key(key, m, &mut fixed, 100.0)
    }

    const NONE: Modifiers = Modifiers {
        ctrl: false,
        shift: false,
        alt: false,
    };
    const SHIFT: Modifiers = Modifiers {
        ctrl: false,
        shift: true,
        alt: false,
    };
    const CTRL: Modifiers = Modifiers {
        ctrl: true,
        shift: false,
        alt: false,
    };

    #[test]
    fn la_longueur_maximale_borne_la_frappe_et_le_collage() {
        let mut e = FieldEditor::new("abc", Some(5), false, None, false);
        assert!(e.insert("de"));
        assert!(!e.insert("f"), "plein : rien ne s'ajoute");
        assert_eq!(e.buffer.text, "abcde");
        // En remplaçant une sélection, la place qu'elle libère compte.
        e.buffer.anchor = 1;
        e.buffer.caret = 3;
        assert!(e.insert("XYZW"));
        assert_eq!(e.buffer.text, "aXYde", "deux cases libérées, deux prises");
        // Un collage trop long est tronqué.
        let mut e = FieldEditor::new("", Some(5), false, Some(5), false);
        assert!(e.insert("7500123"));
        assert_eq!(e.buffer.text, "75001");
        // Sans limite, tout passe.
        let mut e = plain("");
        assert!(e.insert("une longue phrase entière"));
        assert_eq!(e.buffer.len(), 25);
    }

    #[test]
    fn les_sauts_de_ligne_selon_le_champ() {
        let mut e = plain("");
        e.insert("un\ndeux\r\ntrois\n\n");
        assert_eq!(e.buffer.text, "un deux trois", "une ligne : des espaces");
        assert_eq!(press(&mut e, Key::Enter, NONE), FieldKey::Commit);

        let mut e = FieldEditor::new("un", None, true, None, false);
        assert_eq!(press(&mut e, Key::Enter, NONE), FieldKey::Changed);
        e.insert("deux");
        assert_eq!(e.buffer.text, "un\ndeux");
        assert_eq!(
            press(&mut e, Key::Enter, CTRL),
            FieldKey::Commit,
            "Ctrl+Entrée valide"
        );
        e.insert("\r\ntrois");
        assert_eq!(e.buffer.text, "un\ndeux\ntrois");
    }

    #[test]
    fn les_touches_de_passage_et_de_sortie() {
        let mut e = plain("x");
        assert_eq!(press(&mut e, Key::Tab, NONE), FieldKey::Next(true));
        assert_eq!(press(&mut e, Key::Tab, SHIFT), FieldKey::Next(false));
        assert_eq!(press(&mut e, Key::Escape, NONE), FieldKey::Cancel);
        assert_eq!(press(&mut e, Key::PageDown, NONE), FieldKey::None);
        assert_eq!(
            press(&mut e, Key::Up, NONE),
            FieldKey::None,
            "une ligne : Haut ne fait rien"
        );
    }

    #[test]
    fn deplacements_par_mot_ligne_et_texte() {
        let mut e = plain("bonjour le monde");
        press(&mut e, Key::Home, NONE);
        assert_eq!(e.buffer.caret, 0);
        assert_eq!(press(&mut e, Key::Right, CTRL), FieldKey::Moved);
        assert_eq!(e.buffer.caret, 8, "après « bonjour » et son espace");
        press(&mut e, Key::End, SHIFT);
        assert_eq!(e.buffer.selected(), "le monde");
        press(&mut e, Key::Left, CTRL);
        assert_eq!(e.buffer.caret, 11);

        // Plusieurs lignes, dix caractères par ligne de 100.
        let mut e = FieldEditor::new("un deux trois quatre", None, true, None, false);
        assert_eq!(lines(&e, 100.0), [(0, 7), (8, 13), (14, 20)]);
        e.buffer.move_to(10, false);
        press(&mut e, Key::Home, NONE);
        assert_eq!(e.buffer.caret, 8, "début de la ligne visuelle");
        press(&mut e, Key::End, NONE);
        assert_eq!(e.buffer.caret, 13, "fin de la ligne visuelle");
        press(&mut e, Key::Home, CTRL);
        assert_eq!(e.buffer.caret, 0, "Ctrl+Début : tout le texte");
        e.buffer.move_to(3, false);
        press(&mut e, Key::Down, NONE);
        assert_eq!(e.buffer.caret, 11, "même abscisse, ligne suivante");
        press(&mut e, Key::Up, NONE);
        assert_eq!(e.buffer.caret, 3);
        press(&mut e, Key::Up, NONE);
        assert_eq!(e.buffer.caret, 0, "au-dessus de la première ligne");
        for _ in 0..3 {
            press(&mut e, Key::Down, NONE);
        }
        assert_eq!(e.buffer.caret, 20, "sous la dernière ligne");
    }

    #[test]
    fn effacer_un_caractere_ou_un_mot() {
        let mut e = plain("bonjour le monde");
        assert_eq!(press(&mut e, Key::Backspace, NONE), FieldKey::Changed);
        assert_eq!(e.buffer.text, "bonjour le mond");
        press(&mut e, Key::Backspace, CTRL);
        assert_eq!(e.buffer.text, "bonjour le ");
        press(&mut e, Key::Home, NONE);
        press(&mut e, Key::Delete, CTRL);
        assert_eq!(e.buffer.text, "le ");
        press(&mut e, Key::End, NONE);
        assert_eq!(press(&mut e, Key::Delete, NONE), FieldKey::None);
    }

    #[test]
    fn la_coupure_des_lignes() {
        // Aux espaces d'abord.
        let e = FieldEditor::new("aa bb cc", None, true, None, false);
        assert_eq!(lines(&e, 50.0), [(0, 5), (6, 8)]);
        // Un mot plus large que le champ : coupé au caractère.
        let e = FieldEditor::new("abcdefghij", None, true, None, false);
        assert_eq!(lines(&e, 40.0), [(0, 4), (4, 8), (8, 10)]);
        // Les sauts de ligne, une ligne vide comprise.
        let e = FieldEditor::new("a\n\nb\n", None, true, None, false);
        assert_eq!(lines(&e, 100.0), [(0, 1), (2, 2), (3, 4), (5, 5)]);
        // Un champ vide a une ligne.
        assert_eq!(lines(&plain(""), 100.0), [(0, 0)]);
    }

    #[test]
    fn le_clic_et_le_curseur_se_repondent() {
        let mut e = FieldEditor::new("un deux trois quatre", None, true, None, false);
        for caret in [0, 3, 7, 8, 12, 20] {
            e.buffer.move_to(caret, false);
            let (x, line) = e.caret_xy(&mut fixed, 100.0);
            assert_eq!(
                e.index_at(&mut fixed, 100.0, x, line),
                caret,
                "curseur {caret}"
            );
        }
        // Un clic entre deux lettres va à la plus proche.
        assert_eq!(e.index_at(&mut fixed, 100.0, 24.0, 0), 2);
        assert_eq!(e.index_at(&mut fixed, 100.0, 26.0, 0), 3);
        assert_eq!(
            e.index_at(&mut fixed, 100.0, 999.0, 1),
            13,
            "au-delà : fin de ligne"
        );
        // Un peigne de cinq cases sur 100 : vingt par case.
        let mut c = FieldEditor::new("750", Some(5), false, Some(5), false);
        assert_eq!(c.caret_xy(&mut fixed, 100.0), (60.0, 0));
        assert_eq!(c.index_at(&mut fixed, 100.0, 29.0, 0), 1);
        assert_eq!(
            c.index_at(&mut fixed, 100.0, 95.0, 0),
            3,
            "pas au-delà du texte"
        );
        c.buffer.move_to(1, false);
        assert_eq!(c.caret_xy(&mut fixed, 100.0).0, 20.0);
    }

    #[test]
    fn le_champ_defile_pour_garder_le_curseur() {
        let mut e = plain("abcdefghijklmnopqrst");
        e.ensure_visible(&mut fixed, 100.0, 20.0, 20.0);
        assert_eq!(e.scroll, 101.0, "curseur à 200 : la fin se montre");
        e.buffer.move_to(0, false);
        e.ensure_visible(&mut fixed, 100.0, 20.0, 20.0);
        assert_eq!(e.scroll, 0.0);
        // Un texte court ne défile pas.
        let mut e = plain("abc");
        e.ensure_visible(&mut fixed, 100.0, 20.0, 20.0);
        assert_eq!(e.scroll, 0.0);
        // Plusieurs lignes : défilement vertical.
        let mut e = FieldEditor::new("a\nb\nc\nd", None, true, None, false);
        e.ensure_visible(&mut fixed, 100.0, 40.0, 20.0);
        assert_eq!(e.scroll, 40.0, "quatre lignes de 20 dans 40");
    }

    #[test]
    fn un_mot_de_passe_ne_se_montre_ni_ne_se_copie() {
        let mut e = FieldEditor::new("", None, false, None, true);
        e.insert("abc");
        assert_eq!(e.display(), "\u{2022}\u{2022}\u{2022}");
        e.buffer.select_all();
        assert_eq!(e.copy_text(), None);
        assert_eq!(e.cut(), None);
        assert_eq!(e.buffer.text, "abc", "rien de coupé");
        let mut e = plain("abc");
        e.buffer.select_all();
        assert_eq!(e.copy_text().as_deref(), Some("abc"));
        assert_eq!(e.cut().as_deref(), Some("abc"));
        assert_eq!(e.buffer.text, "");
    }

    #[test]
    fn annuler_dans_le_champ_mot_par_mot() {
        let mut e = plain("");
        for c in "bonjour le".chars() {
            e.insert(&c.to_string());
        }
        assert!(e.can_undo());
        assert!(e.undo_step());
        assert_eq!(
            e.buffer.text, "bonjour",
            "« le » d'un coup, avec son espace"
        );
        assert!(e.undo_step());
        assert_eq!(e.buffer.text, "");
        assert!(!e.undo_step(), "plus rien");
        assert!(e.redo_step());
        assert_eq!(e.buffer.text, "bonjour");
        assert!(e.can_redo());
        // Une frappe nouvelle vide les étapes à refaire.
        e.insert("!");
        assert!(!e.can_redo());
    }
}
