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
//! focus. Souris : un bouton n'agit qu'au relâchement (voir
//! [`crate::ui::modal::ButtonRow`]).

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use crate::platform::{Frame, Key};
use crate::ui::modal::{self, Appear, ButtonRow, RowButton};
use crate::ui::paint::veil;
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
    /// Boutons, de gauche à droite : le principal d'abord, l'abandon en
    /// dernier, comme sous Windows.
    pub buttons: Vec<Button>,
    /// Bouton qui a le focus.
    pub focus: usize,
    /// La rangée dessinée : survol, appui, zones cliquables.
    row: ButtonRow,
    /// La carte arrive en fondu, en montant un peu.
    appear: Appear,
}

impl Dialog {
    /// Fenêtre libre.
    #[must_use]
    pub fn new(title: &str, message: &str, tone: Tone, buttons: Vec<Button>) -> Self {
        let focus = buttons.iter().position(|b| b.primary).unwrap_or(0);
        let row = ButtonRow::new(
            buttons
                .iter()
                .map(|b| RowButton::new(&b.label, b.primary))
                .collect(),
        );
        Self {
            title: title.to_string(),
            message: message.to_string(),
            tone,
            buttons,
            focus,
            row,
            appear: Appear::new(),
        }
    }

    /// Un message d'erreur avec un seul bouton.
    #[must_use]
    pub fn alert(title: &str, message: &str) -> Self {
        Self::single(title, message, Tone::Error)
    }

    /// Un renseignement avec un seul bouton : l'icône d'information, pas
    /// celle d'une erreur — les propriétés d'un document ne sont pas une
    /// mauvaise nouvelle.
    #[must_use]
    pub fn info(title: &str, message: &str) -> Self {
        Self::single(title, message, Tone::Info)
    }

    /// Un message et « OK », qu'Entrée comme Échap referment.
    fn single(title: &str, message: &str, tone: Tone) -> Self {
        Self::new(
            title,
            message,
            tone,
            vec![Button {
                label: crate::ui::lang::tr("OK").into(),
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

    /// Vrai tant que l'apparition a une image à peindre : il faut repeindre
    /// sans attendre d'événement, sans quoi la fenêtre resterait figée à
    /// demi transparente.
    #[must_use]
    pub fn animating(&self) -> bool {
        self.appear.animating()
    }

    /// Vrai si un bouton est sous le point : le pointeur devient une main.
    #[must_use]
    pub fn over_button(&self, x: i32, y: i32) -> bool {
        self.row.hit(x, y).is_some()
    }

    /// Survol ; rend vrai si l'affichage doit changer.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        self.row.mouse_move(x, y)
    }

    /// Appui : enfonce le bouton visé, sans rien choisir encore. Rend vrai
    /// si l'affichage doit changer.
    pub fn mouse_down(&mut self, x: i32, y: i32) -> bool {
        self.row.mouse_down(x, y)
    }

    /// Relâchement : le bouton choisi, si l'appui s'est fait sur lui et que
    /// le pointeur y est encore.
    pub fn mouse_up(&mut self, x: i32, y: i32) -> Option<usize> {
        self.row.mouse_up(x, y)
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
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
    ) {
        // Apparition : le voile s'assombrit et la carte monte de quelques
        // pixels (voir `modal::Appear`).
        let progress = self.appear.progress();
        veil(frame, progress);
        let (fw, fh) = (frame.width as i32, frame.height as i32);
        let s = |v: f32| (v * dpi) as i32;
        let width = modal::card_width(fw, modal::CARD_WIDTH, dpi);
        let pad = s(modal::PAD);
        let size = theme.font_size * dpi;
        let title_size = modal::title_size(theme, dpi);
        let icon = s(36.0);
        // Le message, coupé à la largeur disponible.
        let text_x0 = pad + icon + s(16.0);
        let text_w = (width - text_x0 - pad).max(s(80.0));
        let lines = modal::wrap(text, size, &self.message, text_w as f32);
        let line_h = (size * 1.45) as i32;
        let button_h = s(modal::BUTTON_H);
        let body = s(8.0) + (title_size * 1.3) as i32 + s(8.0) + line_h * lines.len() as i32;
        let height = pad + body.max(icon) + s(24.0) + button_h + pad;
        let (x, y) = modal::modal_rect(fw, fh, width, height, dpi, progress);
        modal::card(frame, theme, dpi, x, y, width, height, progress);
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
        let mut ty = y + pad + s(8.0) + title_size as i32;
        modal::title(frame, text, theme, dpi, x + text_x0, ty, &self.title);
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
        // Boutons, groupés à droite dans l'ordre donné : le principal
        // d'abord, à gauche d'« Annuler », comme sous Windows.
        let by = y + height - pad - button_h;
        self.row
            .layout_right(text, size, dpi, x + width - pad, by, button_h);
        let focus = self.focus.min(self.buttons.len().saturating_sub(1));
        self.row.paint(frame, text, theme, dpi, Some(focus));
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
    fn un_clic_se_decide_au_relachement() {
        let mut d = Dialog::choice(
            "Enregistrer ?",
            "…",
            Tone::Question,
            &["Enregistrer", "Ne pas enregistrer", "Annuler"],
        );
        d.row.layout_with_widths(&[100, 140, 90], 500, 0, 30, 8);
        // L'appui n'agit pas : il enfonce.
        assert!(d.mouse_down(170, 10));
        // Relâché ailleurs, on s'est ravisé.
        assert_eq!(d.mouse_up(170, 90), None);
        // Appui et relâchement sur « Annuler » : c'est lui qui est choisi.
        assert!(d.mouse_down(460, 10));
        assert_eq!(d.mouse_up(460, 12), Some(2));
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
