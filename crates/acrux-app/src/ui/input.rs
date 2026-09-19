//! Champ de saisie sur une ligne, dessiné par le toolkit interne : boîte,
//! texte, caret, texte d'invite. Sert à la recherche, à la saisie de mot de
//! passe et aux formulaires de l'interface.

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
    }

    /// Caractère saisi.
    pub fn insert_char(&mut self, c: char) -> InputAction {
        if c.is_control() {
            return InputAction::None;
        }
        let byte = self.byte_index(self.caret);
        self.value.insert(byte, c);
        self.caret += 1;
        InputAction::Changed
    }

    /// Touche non imprimable.
    pub fn key(&mut self, key: Key, shift: bool) -> InputAction {
        match key {
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
            Key::Left => {
                self.caret = self.caret.saturating_sub(1);
                InputAction::None
            }
            Key::Right => {
                self.caret = (self.caret + 1).min(self.char_count());
                InputAction::None
            }
            Key::Home => {
                self.caret = 0;
                InputAction::None
            }
            Key::End => {
                self.caret = self.char_count();
                InputAction::None
            }
            Key::Enter => {
                let _ = shift;
                InputAction::Submit
            }
            Key::Escape => InputAction::Cancel,
            _ => InputAction::None,
        }
    }

    /// Vide le champ.
    pub fn clear(&mut self) {
        self.value.clear();
        self.caret = 0;
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
        // Fond et bordure (accent si focus).
        let border = if self.focused { t.accent } else { t.separator };
        frame.fill_rect(x, y, w, h, border.0, border.1, border.2);
        frame.fill_rect(
            x + 1,
            y + 1,
            w - 2,
            h - 2,
            t.canvas.0,
            t.canvas.1,
            t.canvas.2,
        );
        let size = t.font_size * dpi;
        let pad = 6.0 * dpi;
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
}
