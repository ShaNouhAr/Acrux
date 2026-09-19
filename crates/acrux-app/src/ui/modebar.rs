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

use crate::platform::Frame;
use crate::ui::lang::tr;
use crate::ui::paint::round_rect;
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Barre d'un outil.
#[derive(Debug, Default)]
pub struct ModeBar {
    /// Zone du bouton « Terminer », remplie au dessin.
    close: Option<(i32, i32, i32, i32)>,
}

impl ModeBar {
    /// Hauteur de la barre en pixels.
    #[must_use]
    pub fn height(dpi: f32) -> i32 {
        (34.0 * dpi) as i32
    }

    /// Vrai si le clic tombe sur « Terminer ».
    #[must_use]
    pub fn closes(&self, x: i32, y: i32) -> bool {
        self.close
            .is_some_and(|(bx, by, bw, bh)| x >= bx && x < bx + bw && y >= by && y < by + bh)
    }

    /// Dessine la barre sur toute la largeur, à l'ordonnée `y`.
    #[allow(clippy::too_many_arguments)] // tout le contexte de dessin, rien de gardé
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
        y: i32,
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
        let pad = (12.0 * dpi) as i32;
        let baseline = (y + h / 2) as f32 + text.ascent(size) / 2.0;
        // Le nom de l'outil, sur une pastille : c'est ce qu'on doit voir
        // d'abord.
        let tw = text.measure(size, title) as i32 + (22.0 * dpi) as i32;
        let top = y + (4.0 * dpi) as i32;
        let inner = h - (8.0 * dpi) as i32;
        round_rect(frame, pad, top, tw, inner, 8.0 * dpi, theme.accent);
        text.draw(
            frame,
            (pad + (11.0 * dpi) as i32) as f32,
            baseline,
            size,
            tr(title),
            (255, 255, 255),
        );
        text.draw(
            frame,
            (pad + tw + (14.0 * dpi) as i32) as f32,
            baseline,
            size,
            tr(hint),
            theme.text_dim,
        );
        let label = tr("Terminer");
        let w = text.measure(size, label) as i32 + (26.0 * dpi) as i32;
        let bx = fw - pad - w;
        round_rect(frame, bx, top, w, inner, 8.0 * dpi, theme.hover);
        text.draw(
            frame,
            (bx + (13.0 * dpi) as i32) as f32,
            baseline,
            size,
            label,
            theme.text,
        );
        self.close = Some((bx, top, w, inner));
    }
}
