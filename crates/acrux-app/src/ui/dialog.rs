//! Fenêtres de dialogue d'Acrux : une question, des boutons, dessinés dans
//! la fenêtre comme le reste de l'interface.
//!
//! Les boîtes de message du système jurent avec une application qui dessine
//! tout elle-même : autre police, autres couleurs, « Oui » et « Non » là où
//! il faudrait nommer l'action (« Enregistrer », « Ne pas enregistrer »). Et
//! elles bloquent le programme le temps de la réponse. Celles-ci suivent le
//! thème, nomment ce qu'elles font, et ne bloquent rien : la réponse arrive
//! comme un événement.
//!
//! Clavier : Entrée valide le bouton qui a le focus (le principal au départ),
//! Échap choisit « Annuler » quand il existe, Tab et les flèches déplacent le
//! focus.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use crate::platform::{Frame, Key};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Ce que dit l'icône de la fenêtre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Une question.
    Question,
    /// Une information.
    Info,
    /// Une erreur.
    Error,
}

/// Un bouton.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    /// Libellé : un verbe qui dit ce qui va se passer.
    pub label: String,
    /// Bouton principal, mis en avant.
    pub primary: bool,
    /// Bouton d'abandon, choisi par Échap.
    pub cancel: bool,
}

/// Une fenêtre de dialogue.
#[derive(Debug, Clone)]
pub struct Dialog {
    /// Titre.
    pub title: String,
    /// Message, coupé à la largeur de la fenêtre.
    pub message: String,
    /// Icône.
    pub tone: Tone,
    /// Boutons, de gauche à droite.
    pub buttons: Vec<Button>,
    /// Bouton qui a le focus.
    pub focus: usize,
    /// Bouton survolé.
    hover: Option<usize>,
    /// Zones des boutons, remplies au dessin.
    hits: Vec<(i32, i32, i32, i32)>,
}

impl Dialog {
    /// Fenêtre libre.
    #[must_use]
    pub fn new(title: &str, message: &str, tone: Tone, buttons: Vec<Button>) -> Self {
        let focus = buttons.iter().position(|b| b.primary).unwrap_or(0);
        Self {
            title: title.to_string(),
            message: message.to_string(),
            tone,
            buttons,
            focus,
            hover: None,
            hits: Vec::new(),
        }
    }

    /// Un message avec un seul bouton.
    #[must_use]
    pub fn alert(title: &str, message: &str) -> Self {
        Self::new(
            title,
            message,
            Tone::Error,
            vec![Button {
                label: "OK".into(),
                primary: true,
                cancel: true,
            }],
        )
    }

    /// Deux, trois boutons : libellés, dont le premier est le principal et le
    /// dernier l'abandon.
    #[must_use]
    pub fn choice(title: &str, message: &str, tone: Tone, labels: &[&str]) -> Self {
        let n = labels.len();
        Self::new(
            title,
            message,
            tone,
            labels
                .iter()
                .enumerate()
                .map(|(i, l)| Button {
                    label: (*l).to_string(),
                    primary: i == 0,
                    cancel: i + 1 == n && n > 1,
                })
                .collect(),
        )
    }

    /// Bouton sous un point, s'il y en a un.
    #[must_use]
    pub fn button_at(&self, x: i32, y: i32) -> Option<usize> {
        self.hits
            .iter()
            .position(|&(bx, by, bw, bh)| x >= bx && x < bx + bw && y >= by && y < by + bh)
    }

    /// Survol ; rend vrai si l'affichage doit changer.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let over = self.button_at(x, y);
        let changed = over != self.hover;
        self.hover = over;
        changed
    }

    /// Touche : le bouton choisi, s'il y en a un.
    pub fn key(&mut self, key: Key, shift: bool) -> Option<usize> {
        let n = self.buttons.len().max(1);
        match key {
            Key::Enter | Key::Space => Some(self.focus.min(n - 1)),
            Key::Escape => self.buttons.iter().position(|b| b.cancel),
            Key::Tab if shift => {
                self.focus = (self.focus + n - 1) % n;
                None
            }
            Key::Tab | Key::Right => {
                self.focus = (self.focus + 1) % n;
                None
            }
            Key::Left => {
                self.focus = (self.focus + n - 1) % n;
                None
            }
            _ => None,
        }
    }

    /// Dessine la fenêtre au centre du cadre, sur un voile.
    #[allow(clippy::too_many_lines)] // une mise en page, lue de haut en bas
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
    ) {
        veil(frame);
        let (fw, fh) = (frame.width as i32, frame.height as i32);
        let s = |v: f32| (v * dpi) as i32;
        let width = s(460.0).min(fw - s(40.0)).max(s(260.0));
        let pad = s(24.0);
        let size = theme.font_size * dpi;
        let title_size = size * 1.3;
        let icon = s(36.0);
        // Le message, coupé à la largeur disponible.
        let text_x0 = pad + icon + s(16.0);
        let text_w = (width - text_x0 - pad).max(s(80.0));
        let lines = wrap(text, size, &self.message, text_w as f32);
        let line_h = (size * 1.45) as i32;
        let button_h = s(34.0);
        let body = s(8.0) + (title_size * 1.3) as i32 + s(8.0) + line_h * lines.len() as i32;
        let height = pad + body.max(icon) + s(24.0) + button_h + pad;
        let x = (fw - width) / 2;
        let y = ((fh - height) / 2).max(s(20.0));
        // Ombre, puis carte.
        frame.fill_rect(x + s(3.0), y + s(5.0), width, height, 0, 0, 0);
        let border = theme.separator;
        frame.fill_rect(
            x - 1,
            y - 1,
            width + 2,
            height + 2,
            border.0,
            border.1,
            border.2,
        );
        frame.fill_rect(x, y, width, height, theme.bar.0, theme.bar.1, theme.bar.2);
        // Icône : un disque et son signe.
        let (disc, sign) = match self.tone {
            Tone::Question => (theme.accent, "?"),
            Tone::Info => (theme.accent, "i"),
            Tone::Error => ((0xE0, 0x4F, 0x4F), "!"),
        };
        disc_at(frame, x + pad, y + pad + s(4.0), icon, disc);
        let sw = text.measure(title_size, sign);
        text.draw(
            frame,
            (x + pad) as f32 + (icon as f32 - sw) / 2.0,
            (y + pad + s(4.0) + icon / 2) as f32 + text.ascent(title_size) / 2.0,
            title_size,
            sign,
            (255, 255, 255),
        );
        // Titre et message.
        let mut ty = y + pad + s(8.0) + (title_size * 1.0) as i32;
        text.draw(
            frame,
            (x + text_x0) as f32,
            ty as f32,
            title_size,
            &self.title,
            theme.text,
        );
        ty += (title_size * 0.3) as i32 + s(8.0);
        for line in &lines {
            ty += line_h;
            text.draw(
                frame,
                (x + text_x0) as f32,
                ty as f32,
                size,
                line,
                theme.text_dim,
            );
        }
        // Boutons, alignés à droite, le principal le plus à droite n'est pas
        // imposé : on garde l'ordre donné, qui est l'ordre de lecture.
        self.hits.clear();
        let widths: Vec<i32> = self
            .buttons
            .iter()
            .map(|b| (text.measure(size, &b.label) as i32 + s(32.0)).max(s(88.0)))
            .collect();
        let gap = s(8.0);
        let total: i32 = widths.iter().sum::<i32>() + gap * (widths.len() as i32 - 1).max(0);
        let mut bx = x + width - pad - total;
        let by = y + height - pad - button_h;
        for (i, (button, w)) in self.buttons.iter().zip(&widths).enumerate() {
            let hovered = self.hover == Some(i);
            let (bg, fg) = if button.primary {
                let a = theme.accent;
                let bg = if hovered {
                    (
                        a.0.saturating_add(18),
                        a.1.saturating_add(18),
                        a.2.saturating_add(10),
                    )
                } else {
                    a
                };
                (bg, (255, 255, 255))
            } else if hovered {
                (theme.separator, theme.text)
            } else {
                (theme.hover, theme.text)
            };
            if self.focus == i {
                let ring = s(2.0).max(1);
                let a = theme.accent;
                frame.fill_rect(
                    bx - ring - 1,
                    by - ring - 1,
                    w + 2 * ring + 2,
                    button_h + 2 * ring + 2,
                    a.0,
                    a.1,
                    a.2,
                );
                frame.fill_rect(
                    bx - 1,
                    by - 1,
                    w + 2,
                    button_h + 2,
                    theme.bar.0,
                    theme.bar.1,
                    theme.bar.2,
                );
            }
            frame.fill_rect(bx, by, *w, button_h, bg.0, bg.1, bg.2);
            let lw = text.measure(size, &button.label);
            text.draw(
                frame,
                bx as f32 + (*w as f32 - lw) / 2.0,
                (by + button_h / 2) as f32 + text.ascent(size) / 2.0,
                size,
                &button.label,
                fg,
            );
            self.hits.push((bx, by, *w, button_h));
            bx += w + gap;
        }
    }
}

/// Coupe un texte en lignes tenant dans une largeur ; `\n` force la coupure.
fn wrap(text: &mut TextRenderer, size: f32, message: &str, width: f32) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in message.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if text.measure(size, &candidate) <= width || line.is_empty() {
                line = candidate;
            } else {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
            }
        }
        out.push(line);
    }
    out
}

/// Assombrit tout le cadre : la fenêtre se détache, le reste attend.
fn veil(frame: &mut Frame<'_>) {
    for pixel in frame.pixels.chunks_exact_mut(4) {
        pixel[0] = (u32::from(pixel[0]) * 45 / 100) as u8;
        pixel[1] = (u32::from(pixel[1]) * 45 / 100) as u8;
        pixel[2] = (u32::from(pixel[2]) * 45 / 100) as u8;
    }
}

/// Disque plein, lissé sur ses bords.
#[allow(clippy::many_single_char_names)] // géométrie du disque
fn disc_at(frame: &mut Frame<'_>, x: i32, y: i32, d: i32, color: (u8, u8, u8)) {
    let r = d as f32 / 2.0;
    let (cx, cy) = (x as f32 + r, y as f32 + r);
    for py in y.max(0)..(y + d).min(frame.height as i32) {
        for px in x.max(0)..(x + d).min(frame.width as i32) {
            let dist = ((px as f32 + 0.5 - cx).powi(2) + (py as f32 + 0.5 - cy).powi(2)).sqrt();
            let a = (r - dist + 0.5).clamp(0.0, 1.0);
            if a <= 0.0 {
                continue;
            }
            let i = frame.index(px as usize, py as usize);
            let p = &mut frame.pixels[i..i + 4];
            let mix = |c: u8, d: u8| (f32::from(c) * a + f32::from(d) * (1.0 - a)) as u8;
            p[0] = mix(color.2, p[0]);
            p[1] = mix(color.1, p[1]);
            p[2] = mix(color.0, p[2]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_bouton_principal_a_le_focus_et_echap_annule() {
        let mut d = Dialog::choice(
            "Enregistrer ?",
            "Le document a été modifié.",
            Tone::Question,
            &["Enregistrer", "Ne pas enregistrer", "Annuler"],
        );
        assert_eq!(d.focus, 0);
        assert_eq!(d.key(Key::Enter, false), Some(0));
        assert_eq!(d.key(Key::Escape, false), Some(2));
        // Tab avance, Maj+Tab recule, et l'on reboucle.
        assert_eq!(d.key(Key::Tab, false), None);
        assert_eq!(d.focus, 1);
        assert_eq!(d.key(Key::Tab, true), None);
        assert_eq!(d.focus, 0);
        assert_eq!(d.key(Key::Left, false), None);
        assert_eq!(d.focus, 2);
        assert_eq!(d.key(Key::Enter, false), Some(2));
    }

    #[test]
    fn un_message_seul_se_ferme_par_ok_ou_echap() {
        let mut d = Dialog::alert("Erreur", "Fichier illisible.");
        assert_eq!(d.buttons.len(), 1);
        assert_eq!(d.key(Key::Escape, false), Some(0));
        assert_eq!(d.key(Key::Enter, false), Some(0));
    }

    #[test]
    fn sans_bouton_annuler_echap_ne_choisit_rien() {
        let mut d = Dialog::new(
            "Question",
            "…",
            Tone::Question,
            vec![Button {
                label: "Oui".into(),
                primary: true,
                cancel: false,
            }],
        );
        assert_eq!(d.key(Key::Escape, false), None);
    }
}
