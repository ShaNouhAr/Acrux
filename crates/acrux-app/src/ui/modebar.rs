//! Barre fine d'un outil en cours : son nom, ce qu'il attend, et le moyen
//! d'en sortir.
//!
//! Un outil qui change ce que fait un clic sur la page doit le dire. Sans
//! cette barre, rien ne distingue « je sélectionne du texte » de « je
//! surligne » avant qu'il soit trop tard — et l'on cherche comment sortir.
//! Acrobat affiche pour cela une barre contextuelle ; celle-ci en est la
//! forme la plus simple.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss
)]

use acrux_graphics::Rasterizer;

use crate::platform::Frame;
use crate::ui::icons::{self, Icon};
use crate::ui::lang::tr;
use crate::ui::paint::{button, round_rect, ButtonLook};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Barre d'un outil.
#[derive(Debug, Default)]
pub struct ModeBar {
    /// Zone du bouton « Terminer », remplie au dessin.
    close: Option<(i32, i32, i32, i32)>,
    /// Le pointeur est sur « Terminer ».
    hover: bool,
}

impl ModeBar {
    /// Hauteur de la barre en pixels : la même que la barre « Modifier le
    /// PDF », pour que les deux se ressemblent.
    #[must_use]
    pub fn height(dpi: f32) -> i32 {
        (44.0 * dpi) as i32
    }

    /// Vrai si le clic tombe sur « Terminer ».
    #[must_use]
    pub fn closes(&self, x: i32, y: i32) -> bool {
        self.close
            .is_some_and(|(bx, by, bw, bh)| x >= bx && x < bx + bw && y >= by && y < by + bh)
    }

    /// Déplacement de la souris ; vrai si l'aspect a changé.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let over = self.closes(x, y);
        let changed = over != self.hover;
        self.hover = over;
        changed
    }

    /// Dessine la barre sur toute la largeur, à l'ordonnée `y` : l'icône et
    /// le nom de l'outil sur une pastille d'accent, sa consigne, et
    /// « Terminer ».
    #[allow(clippy::too_many_arguments)] // tout le contexte de dessin, rien de gardé
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f32,
        y: i32,
        icon: Icon,
        title: &'static str,
        hint: &'static str,
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
        // La pastille : l'icône de l'outil et son nom, en accent. C'est ce
        // qu'on doit voir d'abord.
        let icon_side = s(18.0);
        let title = tr(title);
        let tw = icon_side + text.measure(size, title) as i32 + s(30.0);
        round_rect(frame, pad, top, tw, ctl, 7.0 * dpi, theme.accent);
        icons::draw(
            frame,
            raster,
            icon,
            pad + s(10.0),
            top + (ctl - icon_side) / 2,
            icon_side as f32,
            (255, 255, 255),
        );
        text.draw(
            frame,
            (pad + s(10.0) + icon_side + s(8.0)) as f32,
            baseline,
            size,
            title,
            (255, 255, 255),
        );
        // La consigne.
        let label = tr("Terminer");
        let w = text.measure(size, label) as i32 + s(28.0);
        let bx = fw - pad - w;
        text.draw_clipped(
            frame,
            (pad + tw + s(14.0)) as f32,
            baseline,
            size,
            tr(hint),
            theme.text_dim,
            (bx - pad - tw - s(28.0)).max(0) as f32,
        );
        // Terminer : le bouton principal, le même que partout.
        let ink = button(
            frame,
            bx,
            top,
            w,
            ctl,
            dpi,
            theme,
            ButtonLook {
                primary: true,
                hovered: self.hover,
                focused: false,
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
        self.close = Some((bx, top, w, ctl));
    }
}
