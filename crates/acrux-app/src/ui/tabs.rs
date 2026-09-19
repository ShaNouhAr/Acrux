//! Barre d'onglets : un onglet par document ouvert, avec son nom de fichier,
//! une marque de modification et une croix de fermeture. Comme le reste du
//! toolkit, elle ne connaît pas les documents : elle reçoit des [`TabInfo`]
//! et rend une [`TabAction`].

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names
)]

use crate::platform::Frame;
use crate::ui::paint::round_rect;
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Ce que la barre d'onglets affiche pour un document.
#[derive(Debug, Clone)]
pub struct TabInfo {
    /// Nom affiché (nom de fichier).
    pub title: String,
    /// Modifications non enregistrées.
    pub modified: bool,
}

/// Ce que l'utilisateur demande.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabAction {
    /// Rien.
    None,
    /// Activer l'onglet.
    Select(usize),
    /// Fermer l'onglet.
    Close(usize),
}

/// Barre d'onglets.
#[derive(Debug, Default)]
pub struct Tabs {
    /// Rectangles des onglets au dernier dessin.
    hits: Vec<(i32, i32, i32, i32)>,
    /// Rectangles des croix de fermeture.
    closes: Vec<(i32, i32, i32, i32)>,
    hover: Option<usize>,
    hover_close: Option<usize>,
}

impl Tabs {
    /// Nouvelle barre.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Hauteur en pixels physiques.
    #[must_use]
    pub fn height(theme: &Theme, dpi: f32) -> i32 {
        (f32::from(theme.tab_height as u16) * dpi).round() as i32
    }

    /// Dessine la barre au sommet de `frame` (qui doit être la sous-vue de la
    /// bande réservée aux onglets).
    #[allow(clippy::too_many_arguments)] // cible, police, thème et géométrie
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
        tabs: &[TabInfo],
        active: usize,
    ) {
        let t = theme;
        let (w, h) = (frame.width as i32, frame.height as i32);
        frame.fill_rect(0, 0, w, h, t.bar.0, t.bar.1, t.bar.2);
        frame.fill_rect(0, h - 1, w, 1, t.separator.0, t.separator.1, t.separator.2);
        self.hits.clear();
        self.closes.clear();
        if tabs.len() < 2 {
            return;
        }
        let size = t.font_size * dpi;
        let pad = (10.0 * dpi).round() as i32;
        let close = (16.0 * dpi).round() as i32;
        let max_w = (220.0 * dpi).round() as i32;
        let min_w = (90.0 * dpi).round() as i32;
        // Largeur égale pour tous, bornée : une dizaine d'onglets tiennent.
        let each = (w / tabs.len() as i32).clamp(min_w, max_w);
        let mut x = 0;
        for (i, tab) in tabs.iter().enumerate() {
            if x >= w {
                break;
            }
            let tw = each.min(w - x);
            let selected = i == active;
            if selected {
                // L'onglet actif se prolonge vers le bas : il tient à la page.
                round_rect(frame, x + 2, 3, tw - 4, h + 8, 8.0 * dpi, t.canvas);
                frame.fill_rect(x + 2, 3, tw - 4, 2, t.accent.0, t.accent.1, t.accent.2);
            } else if self.hover == Some(i) {
                round_rect(frame, x + 2, 3, tw - 4, h - 7, 8.0 * dpi, t.hover);
            }
            frame.fill_rect(
                x + tw - 1,
                4,
                1,
                h - 9,
                t.separator.0,
                t.separator.1,
                t.separator.2,
            );
            let label = if tab.modified {
                format!("• {}", tab.title)
            } else {
                tab.title.clone()
            };
            let baseline = f32::midpoint(h as f32, text.ascent(size)) - 1.0;
            let color = if selected { t.text } else { t.text_dim };
            text.draw_clipped(
                frame,
                (x + pad) as f32,
                baseline,
                size,
                &label,
                color,
                (tw - 2 * pad - close) as f32,
            );
            // Croix de fermeture, à droite de l'onglet.
            let cx = x + tw - pad - close / 2;
            let cy = h / 2 - 1;
            let r = (close / 2 - 3).max(3);
            let cross = if self.hover_close == Some(i) {
                t.text
            } else {
                t.text_dim
            };
            if self.hover_close == Some(i) {
                frame.fill_rect(
                    cx - close / 2,
                    cy - close / 2,
                    close,
                    close,
                    t.separator.0,
                    t.separator.1,
                    t.separator.2,
                );
            }
            for d in -r..=r {
                frame.fill_rect(cx + d, cy + d, 1, 1, cross.0, cross.1, cross.2);
                frame.fill_rect(cx + d, cy - d, 1, 1, cross.0, cross.1, cross.2);
            }
            self.hits.push((x, 0, tw, h));
            self.closes
                .push((cx - close / 2, cy - close / 2, close, close));
            x += tw;
        }
    }

    fn at(rects: &[(i32, i32, i32, i32)], x: i32, y: i32) -> Option<usize> {
        rects
            .iter()
            .position(|&(rx, ry, rw, rh)| x >= rx && x < rx + rw && y >= ry && y < ry + rh)
    }

    /// Déplacement de la souris ; vrai si l'aspect a changé.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let close = Self::at(&self.closes, x, y);
        let tab = Self::at(&self.hits, x, y);
        let changed = tab != self.hover || close != self.hover_close;
        self.hover = tab;
        self.hover_close = close;
        changed
    }

    /// Le pointeur a quitté la barre.
    pub fn mouse_leave(&mut self) -> bool {
        let changed = self.hover.is_some() || self.hover_close.is_some();
        self.hover = None;
        self.hover_close = None;
        changed
    }

    /// Clic : la croix l'emporte sur l'onglet.
    #[must_use]
    pub fn mouse_down(&self, x: i32, y: i32) -> TabAction {
        if let Some(i) = Self::at(&self.closes, x, y) {
            return TabAction::Close(i);
        }
        Self::at(&self.hits, x, y).map_or(TabAction::None, TabAction::Select)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed() -> Tabs {
        let mut t = Tabs::new();
        t.hits = vec![(0, 0, 100, 28), (100, 0, 100, 28)];
        t.closes = vec![(80, 6, 16, 16), (180, 6, 16, 16)];
        t
    }

    #[test]
    fn click_selects_or_closes() {
        let t = placed();
        assert_eq!(t.mouse_down(20, 14), TabAction::Select(0));
        assert_eq!(t.mouse_down(120, 14), TabAction::Select(1));
        // La croix prime sur l'onglet qui la contient.
        assert_eq!(t.mouse_down(86, 14), TabAction::Close(0));
        assert_eq!(t.mouse_down(186, 14), TabAction::Close(1));
        assert_eq!(t.mouse_down(500, 14), TabAction::None);
        assert_eq!(t.mouse_down(20, 90), TabAction::None);
    }

    #[test]
    fn hover_tracks_tab_and_close() {
        let mut t = placed();
        assert!(t.mouse_move(20, 14));
        assert_eq!(t.hover, Some(0));
        assert_eq!(t.hover_close, None);
        assert!(!t.mouse_move(30, 14), "même onglet : rien à redessiner");
        assert!(t.mouse_move(86, 14));
        assert_eq!(t.hover_close, Some(0));
        assert!(t.mouse_leave());
        assert!(!t.mouse_leave());
    }
}
