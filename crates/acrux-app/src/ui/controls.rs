//! Contrôles composés, partagés par les barres et les panneaux : creux,
//! contrôle segmenté, pastille de couleur, case à cocher, jauge.
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
use crate::ui::paint::{line, round_rect, round_rect_alpha, round_rect_outline, Rgb};
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

/// L'état d'une case à cocher.
// Quatre états indépendants, comme ceux d'un bouton (`ButtonLook`) : une
// case cochée peut être grisée, survolée et avoir le focus à la fois.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CheckLook {
    /// Cochée.
    pub on: bool,
    /// Elle répond : grisée sinon (une autre case la commande).
    pub enabled: bool,
    /// Le pointeur est dessus.
    pub hovered: bool,
    /// Elle a le focus clavier : un anneau d'accent entoure la case.
    pub focused: bool,
}

/// Une case à cocher et son libellé, posés à partir de `(x, y)` (coin haut
/// gauche de la ligne).
///
/// Cochée, la case est pleine, de la couleur d'accent, avec une coche
/// blanche en deux traits ; grisée, tout s'estompe. Rend le rectangle
/// cliquable : la case **et** son libellé, comme partout ailleurs.
pub fn checkbox(
    frame: &mut Frame<'_>,
    text: &mut TextRenderer,
    theme: &Theme,
    dpi: f32,
    x: i32,
    y: i32,
    label: &str,
    look: CheckLook,
) -> (i32, i32, i32, i32) {
    let size = theme.font_size * dpi;
    let row = (22.0 * dpi).round() as i32;
    let square = (16.0 * dpi).round() as i32;
    let by = y + (row - square) / 2;
    let radius = 4.0 * dpi;
    let active = look.enabled;
    if look.focused {
        let ring = (2.0 * dpi).max(1.0);
        let r = ring.ceil() as i32 + 1;
        round_rect_outline(
            frame,
            x - r,
            by - r,
            square + 2 * r,
            square + 2 * r,
            radius + ring,
            ring,
            theme.accent,
        );
    }
    if look.on {
        let fill = if active { theme.accent } else { theme.text_dim };
        round_rect(frame, x, by, square, square, radius, fill);
        let ink = if active { (255, 255, 255) } else { theme.bar };
        // La coche : un trait court qui descend, un long qui remonte.
        let (fx, fy, s) = (x as f32, by as f32, square as f32);
        let width = (1.9 * dpi).max(1.5);
        line(
            frame,
            fx + s * 0.24,
            fy + s * 0.52,
            fx + s * 0.43,
            fy + s * 0.71,
            width,
            ink,
        );
        line(
            frame,
            fx + s * 0.43,
            fy + s * 0.71,
            fx + s * 0.77,
            fy + s * 0.31,
            width,
            ink,
        );
    } else {
        round_rect(frame, x, by, square, square, radius, theme.canvas);
        let edge = if look.hovered && active {
            theme.accent
        } else if active {
            theme.text_dim
        } else {
            theme.separator
        };
        round_rect_outline(
            frame,
            x,
            by,
            square,
            square,
            radius,
            (1.2 * dpi).max(1.0),
            edge,
        );
    }
    let lx = x + square + (8.0 * dpi).round() as i32;
    let ink = if active { theme.text } else { theme.text_dim };
    let baseline = (y + row / 2) as f32 + text.ascent(size) / 2.0;
    text.draw(frame, lx as f32, baseline, size, label, ink);
    let width = lx - x + text.measure(size, label).ceil() as i32;
    (x, y, width, row)
}

/// Jauge en quatre segments, pour une force (de mot de passe) de 0 à 4.
///
/// Les segments allumés prennent la teinte du niveau : danger pour 1,
/// mise en garde pour 2, accent au-delà ; les autres restent en creux.
pub fn strength_meter(
    frame: &mut Frame<'_>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    level: u8,
    theme: &Theme,
) {
    let gap = (h / 2).max(2);
    let segment = (w - 3 * gap) / 4;
    if segment <= 0 || h <= 0 {
        return;
    }
    let lit = match level {
        0 => theme.separator,
        1 => theme.danger,
        2 => theme.warning,
        _ => theme.accent,
    };
    for i in 0..4u8 {
        let sx = x + i32::from(i) * (segment + gap);
        let color = if i < level { lit } else { theme.hover };
        round_rect(frame, sx, y, segment, h, h as f32 / 2.0, color);
    }
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
