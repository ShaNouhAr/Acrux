//! Champ de saisie sur une ligne, dessiné par le toolkit interne : boîte,
//! texte, caret, texte d'invite. Sert à la recherche, à la saisie de mot de
//! passe et aux formulaires de l'interface.
//!
//! Le champ connaît une **sélection** : tout son texte, choisi d'un coup
//! ([`TextInput::select_all`], Ctrl+A ou Ctrl+F dans la recherche), ou
//! étendu au clavier avec Maj et les flèches. La frappe, le collage, le
//! retour arrière la remplacent, comme partout sous Windows : Ctrl+F sur une
//! recherche déjà tapée la sélectionne, et il suffit de taper la suivante.

use crate::platform::{Frame, Key};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Champ de saisie.
#[derive(Debug, Clone, Default)]
pub struct TextInput {
    /// Contenu.
    pub value: String,
    /// Position du caret en caractères.
    pub caret: usize,
    /// Texte affiché quand le champ est vide.
    pub placeholder: String,
    /// Le champ a le focus (caret visible).
    pub focused: bool,
    /// Affiche des points à la place des caractères (mot de passe).
    pub masked: bool,
    /// Autre bout de la sélection (en caractères) ; la sélection va de
    /// `anchor` au caret. `None` : rien n'est sélectionné.
    pub anchor: Option<usize>,
}

/// Résultat d'un événement clavier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    /// Rien à signaler.
    None,
    /// Le contenu a changé.
    Changed,
    /// Entrée.
    Submit,
    /// Échap.
    Cancel,
}

impl TextInput {
    /// Nouveau champ avec un texte d'invite.
    #[must_use]
    pub fn new(placeholder: &str) -> Self {
        Self {
            placeholder: placeholder.to_string(),
            focused: true,
            ..Self::default()
        }
    }

    /// Remplit le champ (curseur à la fin) : sert aux invites pré-remplies,
    /// comme la modification d'un texte existant.
    pub fn set_value(&mut self, value: &str) {
        self.value = value.to_string();
        self.caret = self.value.chars().count();
        self.anchor = None;
    }

    /// Sélectionne tout le texte (le caret à la fin). Sans effet sur un
    /// champ vide.
    pub fn select_all(&mut self) {
        let n = self.char_count();
        if n > 0 {
            self.anchor = Some(0);
            self.caret = n;
        }
    }

    /// Bornes de la sélection, dans l'ordre, en caractères ; `None` si
    /// rien n'est sélectionné.
    #[must_use]
    pub fn selection(&self) -> Option<(usize, usize)> {
        let n = self.char_count();
        let anchor = self.anchor?.min(n);
        let caret = self.caret.min(n);
        (anchor != caret).then(|| (anchor.min(caret), anchor.max(caret)))
    }

    /// Efface le texte sélectionné ; rend vrai s'il y en avait.
    fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        let (start, end) = (self.byte_index(a), self.byte_index(b));
        self.value.replace_range(start..end, "");
        self.caret = a;
        self.anchor = None;
        true
    }

    /// Déplace le caret. Avec Maj, la sélection s'étend depuis là où elle
    /// commençait ; sans, elle disparaît.
    fn move_caret(&mut self, to: usize, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.caret);
        } else {
            self.anchor = None;
        }
        self.caret = to.min(self.char_count());
    }

    /// Caractère saisi : il remplace la sélection, s'il y en a une.
    pub fn insert_char(&mut self, c: char) -> InputAction {
        if c.is_control() {
            return InputAction::None;
        }
        self.delete_selection();
        let byte = self.byte_index(self.caret);
        self.value.insert(byte, c);
        self.caret += 1;
        InputAction::Changed
    }

    /// Texte collé : il entre au curseur, d'un bloc. Un champ tient sur une
    /// ligne — les sauts de ligne deviennent des espaces, et ceux du bout
    /// (une cellule de tableur copiée en amène toujours un) sont ignorés.
    pub fn paste(&mut self, text: &str) -> InputAction {
        let mut changed = if self.delete_selection() {
            InputAction::Changed
        } else {
            InputAction::None
        };
        let flat = text.trim_end_matches(['\r', '\n']).replace("\r\n", " ");
        for c in flat.chars() {
            let c = if c.is_control() { ' ' } else { c };
            if self.insert_char(c) == InputAction::Changed {
                changed = InputAction::Changed;
            }
        }
        changed
    }

    /// Touche non imprimable. Maj avec les flèches, Origine ou Fin étend la
    /// sélection.
    pub fn key(&mut self, key: Key, shift: bool) -> InputAction {
        match key {
            Key::Backspace | Key::Delete if self.delete_selection() => InputAction::Changed,
            Key::Backspace => {
                if self.caret > 0 {
                    let start = self.byte_index(self.caret - 1);
                    let end = self.byte_index(self.caret);
                    self.value.replace_range(start..end, "");
                    self.caret -= 1;
                    InputAction::Changed
                } else {
                    InputAction::None
                }
            }
            Key::Delete => {
                if self.caret < self.char_count() {
                    let start = self.byte_index(self.caret);
                    let end = self.byte_index(self.caret + 1);
                    self.value.replace_range(start..end, "");
                    InputAction::Changed
                } else {
                    InputAction::None
                }
            }
            // Sans Maj, une flèche sur une sélection la referme du côté
            // où elle va, comme dans les champs de Windows.
            Key::Left => {
                let to = match self.selection() {
                    Some((a, _)) if !shift => a,
                    _ => self.caret.saturating_sub(1),
                };
                self.move_caret(to, shift);
                InputAction::None
            }
            Key::Right => {
                let to = match self.selection() {
                    Some((_, b)) if !shift => b,
                    _ => self.caret + 1,
                };
                self.move_caret(to, shift);
                InputAction::None
            }
            Key::Home => {
                self.move_caret(0, shift);
                InputAction::None
            }
            Key::End => {
                self.move_caret(self.char_count(), shift);
                InputAction::None
            }
            Key::Enter => InputAction::Submit,
            Key::Escape => InputAction::Cancel,
            _ => InputAction::None,
        }
    }

    /// Vide le champ.
    pub fn clear(&mut self) {
        self.value.clear();
        self.caret = 0;
        self.anchor = None;
    }

    /// Texte tel qu'affiché (masqué ou non).
    fn display_value(&self) -> String {
        if self.masked {
            "•".repeat(self.char_count())
        } else {
            self.value.clone()
        }
    }

    fn char_count(&self) -> usize {
        self.value.chars().count()
    }

    fn byte_index(&self, chars: usize) -> usize {
        self.value
            .char_indices()
            .nth(chars)
            .map_or(self.value.len(), |(i, _)| i)
    }

    /// Dessine le champ dans le rectangle `(x, y, w, h)` (pixels physiques).
    #[allow(
        clippy::too_many_arguments,
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::manual_midpoint,
        clippy::many_single_char_names
    )]
    pub fn draw(
        &self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    ) {
        let t = theme;
        // Un champ aux coins arrondis, posé sur son fond ; le focus se dit
        // par un anneau d'accent, pas par un bord plus dur.
        let radius = 7.0 * dpi;
        crate::ui::paint::round_rect(frame, x, y, w, h, radius, t.canvas);
        if self.focused {
            let ring = (1.5 * dpi).max(1.0);
            crate::ui::paint::round_rect_outline(frame, x, y, w, h, radius, ring, t.accent);
        } else {
            crate::ui::paint::round_rect_outline(
                frame,
                x,
                y,
                w,
                h,
                radius,
                dpi.max(1.0),
                t.separator,
            );
        }
        let size = t.font_size * dpi;
        let pad = 10.0 * dpi;
        let baseline = y as f32 + (h as f32 + text.ascent(size)) / 2.0 - 1.0;
        if self.value.is_empty() {
            text.draw_clipped(
                frame,
                x as f32 + pad,
                baseline,
                size,
                &self.placeholder,
                t.text_dim,
                w as f32 - 2.0 * pad,
            );
        } else {
            // La sélection : une bande d'accent sous le texte choisi,
            // mesurée comme le caret, rognée au champ.
            if let Some((a, b)) = self.selection() {
                let shown = self.display_value();
                let before: String = shown.chars().take(a).collect();
                let chosen: String = shown.chars().take(b).collect();
                let x0 = x as f32 + pad + text.measure(size, &before);
                let x1 = (x as f32 + pad + text.measure(size, &chosen)).min((x + w) as f32 - pad);
                let band = (size * 1.35) as i32;
                if x1 > x0 {
                    crate::ui::paint::round_rect_alpha(
                        frame,
                        x0.round() as i32,
                        y + (h - band) / 2,
                        (x1 - x0).round() as i32,
                        band,
                        2.0 * dpi,
                        t.accent,
                        if self.focused { 0.45 } else { 0.25 },
                    );
                }
            }
            text.draw_clipped(
                frame,
                x as f32 + pad,
                baseline,
                size,
                &self.display_value(),
                t.text,
                w as f32 - 2.0 * pad,
            );
        }
        if self.focused {
            let before: String = self.display_value().chars().take(self.caret).collect();
            let cx = x as f32 + pad + text.measure(size, &before);
            let ch = (size * 1.2) as i32;
            frame.fill_rect(
                cx.round() as i32,
                y + (h - ch) / 2,
                (1.0 * dpi).max(1.0) as i32,
                ch,
                t.text.0,
                t.text.1,
                t.text.2,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing() {
        let mut f = TextInput::new("Rechercher");
        assert_eq!(f.insert_char('é'), InputAction::Changed);
        f.insert_char('a');
        assert_eq!(f.value, "éa");
        assert_eq!(f.key(Key::Left, false), InputAction::None);
        f.insert_char('x');
        assert_eq!(f.value, "éxa");
        assert_eq!(f.key(Key::Backspace, false), InputAction::Changed);
        assert_eq!(f.value, "éa");
        assert_eq!(f.key(Key::Home, false), InputAction::None);
        assert_eq!(f.key(Key::Delete, false), InputAction::Changed);
        assert_eq!(f.value, "a");
        assert_eq!(f.key(Key::Enter, false), InputAction::Submit);
        assert_eq!(f.key(Key::Escape, false), InputAction::Cancel);
        assert_eq!(f.insert_char('\u{8}'), InputAction::None);
        f.clear();
        assert!(f.value.is_empty() && f.caret == 0);
    }

    fn filled(value: &str) -> TextInput {
        let mut f = TextInput::new("Rechercher");
        f.set_value(value);
        f
    }

    #[test]
    fn select_all_puis_frappe_remplace() {
        let mut f = filled("ancienne");
        f.select_all();
        assert_eq!(f.selection(), Some((0, 8)));
        assert_eq!(f.insert_char('n'), InputAction::Changed);
        assert_eq!(f.value, "n");
        assert_eq!(f.caret, 1);
        assert_eq!(f.selection(), None);
    }

    #[test]
    fn retour_arriere_efface_la_selection() {
        let mut f = filled("requête");
        f.select_all();
        assert_eq!(f.key(Key::Backspace, false), InputAction::Changed);
        assert!(f.value.is_empty() && f.caret == 0);
        let mut f = filled("requête");
        f.select_all();
        assert_eq!(f.key(Key::Delete, false), InputAction::Changed);
        assert!(f.value.is_empty());
    }

    #[test]
    fn coller_sur_la_selection() {
        let mut f = filled("mot");
        f.select_all();
        assert_eq!(f.paste("autre chose\r\n"), InputAction::Changed);
        assert_eq!(f.value, "autre chose");
        // Coller rien sur une sélection l'efface quand même : c'est un
        // changement.
        f.select_all();
        assert_eq!(f.paste(""), InputAction::Changed);
        assert!(f.value.is_empty());
    }

    #[test]
    fn fleche_annule_la_selection() {
        let mut f = filled("abcdef");
        f.select_all();
        assert_eq!(f.key(Key::Left, false), InputAction::None);
        assert_eq!((f.caret, f.selection()), (0, None));
        f.select_all();
        f.key(Key::Right, false);
        assert_eq!((f.caret, f.selection()), (6, None));
        // Maj+flèche étend depuis le caret, Maj+Origine jusqu'au début.
        f.key(Key::Left, true);
        f.key(Key::Left, true);
        assert_eq!(f.selection(), Some((4, 6)));
        f.key(Key::Home, true);
        assert_eq!(f.selection(), Some((0, 6)));
        f.insert_char('x');
        assert_eq!(f.value, "x");
    }

    #[test]
    fn select_all_sur_champ_vide() {
        let mut f = TextInput::new("Rechercher");
        f.select_all();
        assert_eq!(f.selection(), None);
        assert_eq!(f.insert_char('a'), InputAction::Changed);
        assert_eq!(f.value, "a");
    }
}
