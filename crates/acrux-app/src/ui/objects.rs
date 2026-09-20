//! Outil « Modifier » : sélectionner un objet de la page et le manipuler
//! directement — le déplacer, le redimensionner par ses poignées, le
//! supprimer, changer son ordre.
//!
//! Tout le travail réel est fait par
//! [`acrux_features::edit_objects`] ; ce module ne s'occupe que de ce qui se
//! voit et de ce qui se pointe : la boîte de sélection, ses huit poignées, et
//! la traduction d'un geste de souris en une transformation.
//!
//! Les coordonnées y sont de trois sortes, et les confondre est l'erreur qui
//! rend un éditeur imprécis :
//!
//! * **page**, en points, origine en bas à gauche — c'est ce que comprend le
//!   PDF ;
//! * **vue**, en pixels de la zone de document, origine en haut à gauche ;
//! * **fenêtre**, décalée de la barre d'outils et du panneau.
//!
//! La conversion entre page et vue est faite par l'appelant, qui seul connaît
//! le défilement et le zoom ; ce module travaille en coordonnées de vue.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names
)]

use acrux_core::Rect;

use crate::platform::{Cursor, Frame};
use crate::ui::theme::Theme;

/// Rectangle en pixels de la vue.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewRect {
    /// Bord gauche.
    pub x: f64,
    /// Bord haut.
    pub y: f64,
    /// Largeur.
    pub w: f64,
    /// Hauteur.
    pub h: f64,
}

impl ViewRect {
    /// Vrai si le point est dedans.
    #[must_use]
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }

    /// Aire, pour départager deux objets superposés.
    #[must_use]
    pub fn area(&self) -> f64 {
        self.w * self.h
    }
}

/// Une des huit poignées d'une boîte de sélection, ou le corps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    /// Coin bas-gauche (en coordonnées de page).
    BottomLeft,
    /// Bord bas.
    Bottom,
    /// Coin bas-droit.
    BottomRight,
    /// Bord gauche.
    Left,
    /// Bord droit.
    Right,
    /// Coin haut-gauche.
    TopLeft,
    /// Bord haut.
    Top,
    /// Coin haut-droit.
    TopRight,
    /// Le corps : l'objet se déplace.
    Body,
}

impl Handle {
    /// Les huit poignées, dans l'ordre où elles sont dessinées.
    pub const ALL: [Handle; 8] = [
        Handle::BottomLeft,
        Handle::Bottom,
        Handle::BottomRight,
        Handle::Left,
        Handle::Right,
        Handle::TopLeft,
        Handle::Top,
        Handle::TopRight,
    ];

    /// Position relative dans la boîte, de 0 à 1, en coordonnées de page
    /// (l'ordonnée monte).
    #[must_use]
    pub fn anchor(self) -> (f64, f64) {
        match self {
            Handle::BottomLeft => (0.0, 0.0),
            Handle::Bottom => (0.5, 0.0),
            Handle::BottomRight => (1.0, 0.0),
            Handle::Left => (0.0, 0.5),
            Handle::Right => (1.0, 0.5),
            Handle::TopLeft => (0.0, 1.0),
            Handle::Top => (0.5, 1.0),
            Handle::TopRight => (1.0, 1.0),
            Handle::Body => (0.5, 0.5),
        }
    }

    /// Le côté opposé, qui reste fixe pendant le redimensionnement.
    #[must_use]
    pub fn opposite(self) -> (f64, f64) {
        let (x, y) = self.anchor();
        (1.0 - x, 1.0 - y)
    }

    /// Vrai si tirer cette poignée change la largeur.
    #[must_use]
    pub fn changes_width(self) -> bool {
        !matches!(self, Handle::Top | Handle::Bottom | Handle::Body)
    }

    /// Vrai si tirer cette poignée change la hauteur.
    #[must_use]
    pub fn changes_height(self) -> bool {
        !matches!(self, Handle::Left | Handle::Right | Handle::Body)
    }

    /// Pointeur qui convient à cette poignée : comme dans Acrobat, la flèche
    /// dit dans quel sens la boîte va s'étirer.
    #[must_use]
    pub fn cursor(self) -> Cursor {
        match self {
            Handle::Left | Handle::Right => Cursor::ResizeWE,
            Handle::Top | Handle::Bottom => Cursor::ResizeNS,
            Handle::TopLeft | Handle::BottomRight => Cursor::ResizeNWSE,
            Handle::TopRight | Handle::BottomLeft => Cursor::ResizeNESW,
            Handle::Body => Cursor::Move,
        }
    }
}

/// Taille d'une poignée, en pixels logiques.
const HANDLE: f64 = 7.0;

/// Poignée sous un point, ou `None` si le point est ailleurs que sur la boîte.
///
/// Les poignées l'emportent sur le corps : sur une petite boîte elles se
/// chevauchent, et c'est le redimensionnement qu'on vise en cliquant au bord.
#[must_use]
pub fn handle_at(box_: ViewRect, x: f64, y: f64, dpi: f64) -> Option<Handle> {
    let size = HANDLE * dpi;
    for handle in Handle::ALL {
        let (ax, ay) = handle.anchor();
        // L'ordonnée de la poignée est retournée : la vue descend.
        let hx = box_.x + ax * box_.w;
        let hy = box_.y + (1.0 - ay) * box_.h;
        if (x - hx).abs() <= size && (y - hy).abs() <= size {
            return Some(handle);
        }
    }
    box_.contains(x, y).then_some(Handle::Body)
}

/// Nouvelle boîte, en coordonnées de **page**, après avoir tiré une poignée.
///
/// `dx`, `dy` sont le déplacement en points de page (l'ordonnée monte).
/// Tirer un coin en gardant les proportions se demande avec `keep_ratio` :
/// c'est ce que fait Acrobat quand on tient Maj.
#[must_use]
pub fn resized(bbox: Rect, handle: Handle, dx: f64, dy: f64, keep_ratio: bool) -> Rect {
    if handle == Handle::Body {
        return Rect::new(bbox.x0 + dx, bbox.y0 + dy, bbox.x1 + dx, bbox.y1 + dy);
    }
    let (ox, oy) = handle.opposite();
    let fixed_x = bbox.x0 + ox * bbox.width();
    let fixed_y = bbox.y0 + oy * bbox.height();
    let (ax, ay) = handle.anchor();
    let mut moving_x = bbox.x0 + ax * bbox.width() + if handle.changes_width() { dx } else { 0.0 };
    let mut moving_y =
        bbox.y0 + ay * bbox.height() + if handle.changes_height() { dy } else { 0.0 };

    if keep_ratio && handle.changes_width() && handle.changes_height() {
        // La proportion suit celle de l'agrandissement le plus fort : c'est
        // ce qui donne l'impression de « coller » à la souris.
        let (w0, h0) = (bbox.width().max(1e-6), bbox.height().max(1e-6));
        let sx = (moving_x - fixed_x).abs() / w0;
        let sy = (moving_y - fixed_y).abs() / h0;
        let s = sx.max(sy);
        moving_x = fixed_x + (moving_x - fixed_x).signum() * w0 * s;
        moving_y = fixed_y + (moving_y - fixed_y).signum() * h0 * s;
    }
    // Une boîte ne se retourne pas : tirée au-delà du bord opposé, elle
    // s'arrête à un point de côté au lieu de basculer.
    let (x0, x1) = if handle.changes_width() {
        clamped(fixed_x, moving_x, ax > ox)
    } else {
        (bbox.x0, bbox.x1)
    };
    let (y0, y1) = if handle.changes_height() {
        clamped(fixed_y, moving_y, ay > oy)
    } else {
        (bbox.y0, bbox.y1)
    };
    Rect::new(x0, y0, x1, y1)
}

/// Les deux bornes d'un côté, le bord mobile ne croisant jamais le bord fixe.
///
/// `moving_high` dit de quel côté du bord fixe se trouve le bord tiré.
fn clamped(fixed: f64, moving: f64, moving_high: bool) -> (f64, f64) {
    if moving_high {
        (fixed, moving.max(fixed + 1.0))
    } else {
        (moving.min(fixed - 1.0), fixed)
    }
}

/// Dessine la boîte de sélection et ses poignées.
pub fn paint_selection(
    frame: &mut Frame<'_>,
    theme: &Theme,
    dpi: f32,
    box_: ViewRect,
    hovered: Option<Handle>,
) {
    let (x, y, w, h) = (
        box_.x as i32,
        box_.y as i32,
        box_.w.max(1.0) as i32,
        box_.h.max(1.0) as i32,
    );
    let thickness = (1.0 * f64::from(dpi)).max(1.0) as i32;
    let (r, g, b) = theme.accent;
    // Cadre, quatre bandes : pas de tracé, donc rien à rasteriser.
    frame.fill_rect(x, y, w, thickness, r, g, b);
    frame.fill_rect(x, y + h - thickness, w, thickness, r, g, b);
    frame.fill_rect(x, y, thickness, h, r, g, b);
    frame.fill_rect(x + w - thickness, y, thickness, h, r, g, b);

    // Les poignées sont **rondes**, comme celles d'Acrobat : un disque blanc
    // cerné d'accent, qui se remplit quand le pointeur le touche.
    let size = (HANDLE * f64::from(dpi)).round().max(6.0);
    let radius = size / 2.0;
    for handle in Handle::ALL {
        let (ax, ay) = handle.anchor();
        let hx = box_.x + ax * box_.w;
        let hy = box_.y + (1.0 - ay) * box_.h;
        let (px, py) = ((hx - radius) as i32, (hy - radius) as i32);
        let carre = size as i32;
        let filled = hovered == Some(handle);
        crate::ui::paint::round_rect(frame, px, py, carre, carre, radius as f32, (r, g, b));
        if !filled {
            let inset = (f64::from(dpi)).round().max(1.0) as i32;
            crate::ui::paint::round_rect(
                frame,
                px + inset,
                py + inset,
                (carre - 2 * inset).max(1),
                (carre - 2 * inset).max(1),
                (radius - f64::from(inset)) as f32,
                (0xFF, 0xFF, 0xFF),
            );
        }
    }
}

/// Dessine le contour léger d'un objet survolé mais pas encore sélectionné.
pub fn paint_hover(frame: &mut Frame<'_>, theme: &Theme, dpi: f32, box_: ViewRect) {
    let thickness = (1.0 * f64::from(dpi)).max(1.0) as i32;
    let (x, y, w, h) = (
        box_.x as i32,
        box_.y as i32,
        box_.w.max(1.0) as i32,
        box_.h.max(1.0) as i32,
    );
    let (r, g, b) = theme.text_dim;
    frame.fill_rect(x, y, w, thickness, r, g, b);
    frame.fill_rect(x, y + h - thickness, w, thickness, r, g, b);
    frame.fill_rect(x, y, thickness, h, r, g, b);
    frame.fill_rect(x + w - thickness, y, thickness, h, r, g, b);
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::{handle_at, resized, Handle, ViewRect};
    use acrux_core::Rect;

    fn box_() -> Rect {
        Rect::new(100.0, 200.0, 300.0, 300.0)
    }

    #[test]
    fn tirer_le_corps_deplace_sans_deformer() {
        let r = resized(box_(), Handle::Body, 10.0, -5.0, false);
        assert_eq!(r, Rect::new(110.0, 195.0, 310.0, 295.0));
    }

    #[test]
    fn tirer_un_coin_garde_le_coin_oppose_fixe() {
        // Coin haut-droit tiré de 50 vers la droite et 20 vers le haut.
        let r = resized(box_(), Handle::TopRight, 50.0, 20.0, false);
        assert_eq!(r.x0, 100.0, "le bord gauche ne bouge pas");
        assert_eq!(r.y0, 200.0, "le bord bas ne bouge pas");
        assert_eq!(r.x1, 350.0);
        assert_eq!(r.y1, 320.0);
    }

    #[test]
    fn un_bord_ne_change_quune_dimension() {
        let r = resized(box_(), Handle::Right, 40.0, 999.0, false);
        assert_eq!(r.y0, 200.0);
        assert_eq!(r.y1, 300.0);
        assert_eq!(r.x1, 340.0);
    }

    #[test]
    fn les_proportions_se_gardent_sur_demande() {
        // La boîte fait 200 × 100 ; on tire le coin de 100 en largeur.
        let r = resized(box_(), Handle::TopRight, 100.0, 0.0, true);
        assert_eq!(r.width(), 300.0);
        assert_eq!(r.height(), 150.0, "la hauteur suit la largeur");
    }

    #[test]
    fn une_boite_ne_se_retourne_pas() {
        // Le coin droit tiré très loin à gauche s'arrête avant de croiser.
        let r = resized(box_(), Handle::Right, -1000.0, 0.0, false);
        assert!(r.width() >= 1.0, "largeur {}", r.width());
        assert_eq!(r.x0, 100.0);
    }

    #[test]
    fn les_poignees_se_visent_avant_le_corps() {
        let view = ViewRect {
            x: 10.0,
            y: 20.0,
            w: 200.0,
            h: 100.0,
        };
        // Coin haut-gauche de la vue = coin haut-gauche de la page.
        assert_eq!(handle_at(view, 10.0, 20.0, 1.0), Some(Handle::TopLeft));
        assert_eq!(
            handle_at(view, 210.0, 120.0, 1.0),
            Some(Handle::BottomRight)
        );
        assert_eq!(handle_at(view, 110.0, 70.0, 1.0), Some(Handle::Body));
        assert_eq!(handle_at(view, 400.0, 70.0, 1.0), None);
    }
}
