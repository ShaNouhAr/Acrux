//! Contrôles composés, partagés par les barres et les panneaux : creux,
//! contrôle segmenté, pastille de couleur.
//!
//! Chaque barre dessinait les siens, à sa façon — des rectangles plats ici,
//! des carrés là. Ils sont écrits ici une fois pour toutes : la barre
//! « Modifier le PDF », le panneau « remplir et signer » et la fenêtre de
//! capture d'une signature emploient les mêmes, et se ressemblent donc.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::too_many_arguments
)]

use crate::platform::Frame;
use crate::ui::paint::{round_rect, round_rect_alpha, Rgb};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Le creux dans lequel un groupe de contrôles est posé : un peu plus sombre
/// que la barre, aux coins arrondis.
pub fn well(frame: &mut Frame<'_>, x: i32, y: i32, w: i32, h: i32, radius: f32, theme: &Theme) {
    round_rect_alpha(frame, x, y, w, h, radius, theme.hover, 0.7);
}

/// Le dessin d'un segment : le cadre, le rectangle de la case, l'encre.
pub type Draw<'a> = &'a dyn Fn(&mut Frame<'_>, (i32, i32, i32, i32), Rgb);

/// Un dessin de segment qui vit le temps d'un appel.
pub type BoxedDraw = Box<dyn Fn(&mut Frame<'_>, (i32, i32, i32, i32), Rgb)>;

/// Ce qu'un segment affiche.
pub enum Segment<'a> {
    /// Un libellé.
    Label(&'a str),
    /// Un dessin, fait par l'appelant dans le rectangle donné avec l'encre
    /// donnée, et large de `width` pixels.
    Custom {
        /// Largeur du segment.
        width: i32,
        /// Le dessin.
        draw: Draw<'a>,
    },
}

/// Un segment et son état.
pub struct SegmentItem<'a> {
    /// Ce qu'il montre.
    pub content: Segment<'a>,
    /// Il est allumé.
    pub on: bool,
    /// Le pointeur est dessus.
    pub hovered: bool,
}

/// Largeur qu'occuperait un contrôle segmenté, pour savoir s'il tient.
pub fn segmented_width(
    text: &mut TextRenderer,
    theme: &Theme,
    dpi: f32,
    h: i32,
    items: &[SegmentItem<'_>],
) -> i32 {
    let size = theme.font_size * dpi;
    let inset = (2.0 * dpi).round() as i32;
    items
        .iter()
        .map(|item| match item.content {
            Segment::Label(label) => {
                (text.measure(size, label) as i32 + (18.0 * dpi).round() as i32).max(h)
            }
            Segment::Custom { width, .. } => width,
        })
        .sum::<i32>()
        + 2 * inset
}

/// Un contrôle segmenté : des cases côte à côte dans un même creux, la case
/// allumée en couleur d'accent, la case survolée relevée.
///
/// Rend la largeur totale et le rectangle de chaque case, dans l'ordre —
/// c'est à l'appelant d'en faire ses zones cliquables.
pub fn segmented(
    frame: &mut Frame<'_>,
    text: &mut TextRenderer,
    theme: &Theme,
    dpi: f32,
    x: i32,
    y: i32,
    h: i32,
    items: &[SegmentItem<'_>],
) -> (i32, Vec<(i32, i32, i32, i32)>) {
    let size = theme.font_size * dpi;
    let inset = (2.0 * dpi).round() as i32;
    let radius = 7.0 * dpi;
    let widths: Vec<i32> = items
        .iter()
        .map(|item| match item.content {
            Segment::Label(label) => {
                (text.measure(size, label) as i32 + (18.0 * dpi).round() as i32).max(h)
            }
            Segment::Custom { width, .. } => width,
        })
        .collect();
    let total = widths.iter().sum::<i32>() + 2 * inset;
    well(frame, x, y, total, h, radius, theme);
    let mut rects = Vec::with_capacity(items.len());
    let mut sx = x + inset;
    for (item, w) in items.iter().zip(&widths) {
        let (fill, ink) = if item.on {
            (Some(theme.accent), (255, 255, 255))
        } else if item.hovered {
            (Some(theme.separator), theme.text)
        } else {
            (None, theme.text)
        };
        if let Some(fill) = fill {
            round_rect(frame, sx, y + inset, *w, h - 2 * inset, radius - dpi, fill);
        }
        match item.content {
            Segment::Label(label) => {
                let lw = text.measure(size, label);
                let baseline = (y + h / 2) as f32 + text.ascent(size) / 2.0;
                text.draw(
                    frame,
                    sx as f32 + (*w as f32 - lw) / 2.0,
                    baseline,
                    size,
                    label,
                    ink,
                );
            }
            Segment::Custom { draw, .. } => draw(frame, (sx, y, *w, h), ink),
        }
        rects.push((sx, y, *w, h));
        sx += w;
    }
    (total, rects)
}

/// L'état d'une pastille de couleur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwatchState {
    /// Rien de particulier.
    Plain,
    /// Le pointeur est dessus.
    Hovered,
    /// C'est la couleur choisie.
    Chosen,
}

/// Une pastille de couleur ronde. Choisie, elle est cerclée d'accent avec un
/// liseré ; survolée, cerclée d'un gris.
///
/// Rend le rectangle cliquable, anneau compris.
pub fn swatch(
    frame: &mut Frame<'_>,
    x: i32,
    y: i32,
    size: i32,
    dpi: f32,
    rgb: Rgb,
    state: SwatchState,
    theme: &Theme,
) -> (i32, i32, i32, i32) {
    let ring = (3.0 * dpi).round() as i32;
    let outer = size + 2 * ring;
    match state {
        SwatchState::Chosen => {
            round_rect(
                frame,
                x - ring,
                y - ring,
                outer,
                outer,
                outer as f32 / 2.0,
                theme.accent,
            );
            let gap = ring / 3;
            let inner = size + 2 * gap;
            round_rect(
                frame,
                x - gap,
                y - gap,
                inner,
                inner,
                inner as f32 / 2.0,
                theme.bar,
            );
        }
        SwatchState::Hovered => {
            round_rect(
                frame,
                x - ring,
                y - ring,
                outer,
                outer,
                outer as f32 / 2.0,
                theme.separator,
            );
        }
        SwatchState::Plain => {}
    }
    round_rect(frame, x, y, size, size, size as f32 / 2.0, rgb);
    (x - ring, y - ring, outer, outer)
}
