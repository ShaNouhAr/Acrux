//! Barre d'un document à remplir : elle dit qu'il y a des champs, et donne
//! ce qu'on fait d'un formulaire entier — surligner ses champs, l'effacer,
//! l'aplatir.
//!
//! Acrobat affiche la même à l'ouverture d'un formulaire : sans elle, rien
//! ne dit qu'un document **se remplit** avant qu'on ait cliqué au bon
//! endroit, et le surlignage des champs, qui les montre d'un coup d'œil,
//! resterait introuvable. Elle se ferme d'une croix, pour ce document-là.
//!
//! Même hauteur, mêmes boutons que la barre d'un outil
//! ([`crate::ui::modebar`]) : les deux se ressemblent et peuvent s'empiler.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss
)]

use acrux_graphics::Rasterizer;

use crate::platform::Frame;
use crate::ui::controls::{checkbox, CheckLook};
use crate::ui::icons::{self, Icon};
use crate::ui::lang::tr;
use crate::ui::modebar::ModeBar;
use crate::ui::paint::{button, round_rect, ButtonLook};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Rectangle `(x, y, largeur, hauteur)`.
type Rect = (i32, i32, i32, i32);

/// Ce qu'un clic dans la barre demande.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormBarAction {
    /// Allumer ou éteindre le surlignage des champs.
    ToggleHighlight,
    /// Effacer le formulaire (après confirmation).
    Reset,
    /// Aplatir le formulaire (après confirmation).
    Flatten,
    /// Fermer la barre.
    Close,
}

/// La barre de formulaire.
#[derive(Debug, Default)]
pub struct FormBar {
    /// Zones cliquables du dernier dessin.
    hits: Vec<(FormBarAction, Rect)>,
    /// Élément sous le pointeur.
    hover: Option<FormBarAction>,
}

fn inside(r: Rect, x: i32, y: i32) -> bool {
    x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
}

impl FormBar {
    /// Hauteur de la barre, celle de la barre d'un outil.
    #[must_use]
    pub fn height(dpi: f32) -> i32 {
        ModeBar::height(dpi)
    }

    /// Élément sous un point de la fenêtre.
    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Option<FormBarAction> {
        self.hits
            .iter()
            .find(|(_, r)| inside(*r, x, y))
            .map(|(a, _)| *a)
    }

    /// Clic : ce qu'il demande, s'il tombe sur un élément.
    #[must_use]
    pub fn mouse_down(&self, x: i32, y: i32) -> Option<FormBarAction> {
        self.hit(x, y)
    }

    /// Déplacement du pointeur ; vrai si l'aspect a changé.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let over = self.hit(x, y);
        let changed = over != self.hover;
        self.hover = over;
        changed
    }

    /// Le pointeur a quitté la barre ; vrai si l'aspect a changé.
    pub fn leave(&mut self) -> bool {
        self.hover.take().is_some()
    }

    /// Dessine la barre sur toute la largeur, à l'ordonnée `y`.
    /// `highlight_on` dit si les champs sont surlignés.
    // Tout le contexte de dessin, rien de gardé ; la barre se lit de droite à
    // gauche en une passe, chaque élément calé contre le précédent.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f32,
        y: i32,
        highlight_on: bool,
    ) {
        let h = Self::height(dpi);
        let size = theme.font_size * dpi;
        let fw = frame.width as i32;
        frame.fill_rect(0, y, fw, h, theme.bar.0, theme.bar.1, theme.bar.2);
        frame.fill_rect(
            0,
            y + h - 1,
            fw,
            1,
            theme.separator.0,
            theme.separator.1,
            theme.separator.2,
        );
        let s = |v: f32| (v * dpi).round() as i32;
        let pad = s(12.0);
        let ctl = s(30.0);
        let top = y + (h - ctl) / 2;
        let baseline = (top + ctl / 2) as f32 + text.ascent(size) / 2.0;
        self.hits.clear();

        // De droite à gauche : la croix, puis les deux boutons, puis la
        // bascule. Ce qui reste à gauche est pour le message.
        let close = (fw - pad - ctl, top, ctl, ctl);
        if self.hover == Some(FormBarAction::Close) {
            round_rect(
                frame,
                close.0,
                close.1,
                close.2,
                close.3,
                7.0 * dpi,
                theme.hover,
            );
        }
        let icon = s(18.0);
        icons::draw(
            frame,
            raster,
            Icon::Close,
            close.0 + (ctl - icon) / 2,
            close.1 + (ctl - icon) / 2,
            icon as f32,
            theme.text_dim,
        );
        self.hits.push((FormBarAction::Close, close));
        let mut right = close.0 - s(8.0);
        for (action, label) in [
            (FormBarAction::Flatten, tr("Aplatir le formulaire")),
            (FormBarAction::Reset, tr("Effacer le formulaire")),
        ] {
            let w = text.measure(size, label) as i32 + s(28.0);
            let bx = right - w;
            let ink = button(
                frame,
                bx,
                top,
                w,
                ctl,
                dpi,
                theme,
                ButtonLook {
                    hovered: self.hover == Some(action),
                    ..ButtonLook::default()
                },
            );
            let lw = text.measure(size, label);
            text.draw(
                frame,
                bx as f32 + (w as f32 - lw) / 2.0,
                baseline,
                size,
                label,
                ink,
            );
            self.hits.push((action, (bx, top, w, ctl)));
            right = bx - s(8.0);
        }
        // La bascule : sa largeur se mesure avant de la poser, pour la caler
        // contre les boutons.
        let label = tr("Surligner les champs");
        let toggle_w = s(16.0) + s(8.0) + text.measure(size, label).ceil() as i32;
        let tx = right - s(10.0) - toggle_w;
        let row = s(22.0);
        let hit = checkbox(
            frame,
            text,
            theme,
            dpi,
            tx,
            y + (h - row) / 2,
            label,
            CheckLook {
                on: highlight_on,
                enabled: true,
                hovered: self.hover == Some(FormBarAction::ToggleHighlight),
                focused: false,
            },
        );
        self.hits.push((FormBarAction::ToggleHighlight, hit));

        // Le message, sur ce qui reste : il s'efface avant les commandes
        // quand la fenêtre est étroite.
        let icon_x = pad;
        let room = tx - s(18.0) - icon_x;
        if room > icon + s(40.0) {
            icons::draw(
                frame,
                raster,
                Icon::Form,
                icon_x,
                top + (ctl - icon) / 2,
                icon as f32,
                theme.accent,
            );
            let mx = icon_x + icon + s(10.0);
            text.draw_clipped(
                frame,
                mx as f32,
                baseline,
                size,
                tr("Ce document contient des champs à remplir."),
                theme.text,
                (tx - s(18.0) - mx).max(0) as f32,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Les éléments se trouvent là où ils ont été dessinés, et le survol
    /// ne repeint que quand il change d'élément.
    #[test]
    fn les_clics_tombent_sur_ce_qui_est_dessine() {
        let Some(mut text) = TextRenderer::system() else {
            return;
        };
        let mut raster = Rasterizer::new();
        let (fw, fh) = (1200_u32, 60_u32);
        let mut pixels = vec![0_u8; (fw * fh * 4) as usize];
        let mut frame = Frame::new(fw, fh, &mut pixels);
        let mut bar = FormBar::default();
        bar.paint(
            &mut frame,
            &mut text,
            &mut raster,
            &Theme::dark(),
            1.0,
            0,
            true,
        );
        let hits = bar.hits.clone();
        let center = |a: FormBarAction| {
            let (_, (x, y, w, h)) = hits
                .iter()
                .find(|(b, _)| *b == a)
                .copied()
                .unwrap_or((a, (0, 0, 0, 0)));
            (x + w / 2, y + h / 2)
        };
        for action in [
            FormBarAction::ToggleHighlight,
            FormBarAction::Reset,
            FormBarAction::Flatten,
            FormBarAction::Close,
        ] {
            let (x, y) = center(action);
            assert_eq!(bar.mouse_down(x, y), Some(action), "{action:?}");
        }
        assert_eq!(bar.mouse_down(5, 5), None, "le message ne se clique pas");
        let (x, y) = center(FormBarAction::Reset);
        assert!(bar.mouse_move(x, y));
        assert!(!bar.mouse_move(x + 1, y), "même élément : rien à repeindre");
        assert!(bar.leave());
        assert!(!bar.leave());
        // Les boutons ne se chevauchent pas, de gauche à droite.
        let xs: Vec<i32> = [
            FormBarAction::ToggleHighlight,
            FormBarAction::Reset,
            FormBarAction::Flatten,
            FormBarAction::Close,
        ]
        .iter()
        .map(|a| center(*a).0)
        .collect();
        assert!(xs.windows(2).all(|w| w[0] < w[1]), "{xs:?}");
    }
}
