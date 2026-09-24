//! Barre de propriétés d'une annotation sélectionnée : une petite barre
//! flottante, posée au-dessus du cadre de sélection, comme la mini-barre
//! qu'Acrobat montre sur un commentaire.
//!
//! Elle porte les réglages qui ont un sens pour l'annotation — couleur,
//! fond, épaisseur, opacité — sous la même forme que ceux des outils de
//! dessin (une pastille, ou une valeur à dérouler : [`Setting`]), puis trois
//! actions : changer le texte, répondre, supprimer. Changer un réglage
//! s'applique à l'annotation, pas à l'outil : c'est la différence avec la
//! barre des commentaires, qui règle ce qu'on va poser.
//!
//! La barre ne connaît pas l'annotation : elle reçoit ses réglages, les
//! dessine et rend ce qu'on clique ([`BarItem`]). Le visualiseur déroule le
//! nuancier ou la liste, et fait du choix une modification du document.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss
)]

use acrux_graphics::Rasterizer;

use crate::platform::Frame;
use crate::ui::icons::{self, Icon};
use crate::ui::modebar::{paint_setting, setting_width, Setting};
use crate::ui::paint::{round_rect, round_rect_outline, shadow};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Rectangle `(x, y, largeur, hauteur)`.
pub type Rect = (i32, i32, i32, i32);

/// Ce qu'on clique dans la barre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarItem {
    /// Un réglage, par son rang dans ceux reçus.
    Setting(usize),
    /// Changer le texte du commentaire.
    EditText,
    /// Répondre.
    Reply,
    /// Supprimer.
    Delete,
}

/// La barre, telle qu'au dernier dessin.
#[derive(Debug, Default)]
pub struct AnnotBar {
    card: Rect,
    hits: Vec<(Rect, BarItem)>,
    hover: Option<BarItem>,
}

fn inside(r: Rect, x: i32, y: i32) -> bool {
    x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
}

/// Place d'une barre de `size` pour une annotation en `anchor` : au-dessus,
/// retournée dessous quand elle manque de place, alignée sur le bord gauche
/// de l'annotation, toujours dans `bounds` (largeur, hauteur).
#[must_use]
pub fn place(anchor: Rect, size: (i32, i32), bounds: (i32, i32), gap: i32) -> (i32, i32) {
    let (w, h) = size;
    let above = anchor.1 - gap - h;
    let y = if above >= 0 {
        above
    } else {
        (anchor.1 + anchor.3 + gap).min((bounds.1 - h).max(0))
    };
    let x = anchor.0.clamp(0, (bounds.0 - w).max(0));
    (x, y)
}

impl AnnotBar {
    /// Oublie la dernière mise en page : la barre n'est plus à l'écran.
    pub fn clear(&mut self) {
        self.card = (0, 0, 0, 0);
        self.hits.clear();
        self.hover = None;
    }

    /// Vrai si le point est sur la barre.
    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        inside(self.card, x, y)
    }

    /// Ce qui est sous le point.
    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Option<BarItem> {
        self.hits
            .iter()
            .find(|(r, _)| inside(*r, x, y))
            .map(|(_, h)| *h)
    }

    /// Rectangle d'un réglage au dernier dessin : son nuancier ou sa liste
    /// se déroule dessous.
    #[must_use]
    pub fn setting_rect(&self, index: usize) -> Option<Rect> {
        self.hits
            .iter()
            .find(|(_, h)| *h == BarItem::Setting(index))
            .map(|(r, _)| *r)
    }

    /// Survol ; vrai si l'image change.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let hover = self.hit(x, y);
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    /// Dessine la barre au-dessus de `anchor` (le cadre de l'annotation dans
    /// `frame`). `open` est le réglage dont la liste est déroulée ;
    /// `can_reply` dit si l'annotation est un commentaire auquel on répond.
    #[allow(clippy::too_many_arguments)] // une barre, son contenu, son allure
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f32,
        anchor: Rect,
        settings: &[Setting],
        open: Option<usize>,
        can_reply: bool,
    ) {
        let s = |v: f32| (v * dpi).round() as i32;
        let pad = s(5.0);
        let ctl = s(30.0);
        let gap = s(4.0);
        let widths: Vec<i32> = settings
            .iter()
            .map(|setting| setting_width(text, theme, dpi, setting))
            .collect();
        let mut actions = vec![BarItem::EditText];
        if can_reply {
            actions.push(BarItem::Reply);
        }
        actions.push(BarItem::Delete);
        let settings_w: i32 = widths.iter().map(|w| w + gap).sum();
        let separator = if settings.is_empty() { 0 } else { s(9.0) };
        let width = pad * 2 + settings_w + separator + (ctl + gap) * actions.len() as i32 - gap;
        let height = ctl + pad * 2;
        let bounds = (frame.width as i32, frame.height as i32);
        let (x, y) = place(anchor, (width, height), bounds, s(8.0));
        self.card = (x, y, width, height);
        self.hits.clear();
        let radius = 9.0 * dpi;
        shadow(frame, x, y + s(2.0), width, height, radius, 10.0 * dpi, 0.3);
        round_rect(frame, x, y, width, height, radius, theme.bar);
        round_rect_outline(
            frame,
            x,
            y,
            width,
            height,
            radius,
            dpi.max(1.0),
            theme.separator,
        );
        let top = y + pad;
        let mut left = x + pad;
        for (index, (setting, &w)) in settings.iter().zip(&widths).enumerate() {
            let r = (left, top, w, ctl);
            let item = BarItem::Setting(index);
            let face = if open == Some(index) {
                theme.separator
            } else if self.hover == Some(item) {
                theme.hover
            } else {
                theme.bar
            };
            paint_setting(frame, text, raster, theme, dpi, r, setting, face);
            self.hits.push((r, item));
            left += w + gap;
        }
        if separator > 0 {
            frame.fill_rect(
                left + separator / 2 - gap,
                top + s(5.0),
                s(1.0).max(1),
                ctl - s(10.0),
                theme.separator.0,
                theme.separator.1,
                theme.separator.2,
            );
            left += separator;
        }
        let icon_side = s(18.0);
        for item in actions {
            let r = (left, top, ctl, ctl);
            if self.hover == Some(item) {
                let face = if item == BarItem::Delete {
                    theme.danger
                } else {
                    theme.hover
                };
                round_rect(frame, r.0, r.1, r.2, r.3, 7.0 * dpi, face);
            }
            let (icon, colour) = match item {
                BarItem::EditText => (Icon::EditText, theme.text),
                BarItem::Reply => (Icon::Reply, theme.text),
                _ if self.hover == Some(item) => (Icon::Trash, (255, 255, 255)),
                _ => (Icon::Trash, theme.text),
            };
            icons::draw(
                frame,
                raster,
                icon,
                r.0 + (ctl - icon_side) / 2,
                r.1 + (ctl - icon_side) / 2,
                icon_side as f32,
                colour,
            );
            self.hits.push((r, item));
            left += ctl + gap;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_barre_se_pose_au_dessus_ou_dessous_et_reste_dans_la_vue() {
        // Place au-dessus.
        assert_eq!(
            place((100, 200, 50, 40), (180, 40), (800, 600), 8),
            (100, 152)
        );
        // Pas de place au-dessus : dessous.
        assert_eq!(
            place((100, 20, 50, 40), (180, 40), (800, 600), 8),
            (100, 68)
        );
        // Trop à droite : ramenée dans la vue.
        assert_eq!(place((700, 200, 50, 40), (180, 40), (800, 600), 8).0, 620);
        // Une annotation qui occupe toute la hauteur : la barre reste dans
        // la vue, au bas.
        assert_eq!(place((10, 0, 50, 600), (180, 40), (800, 600), 8).1, 560);
    }

    #[test]
    fn les_clics_se_lisent_sur_la_derniere_mise_en_page() {
        let mut bar = AnnotBar {
            card: (0, 0, 200, 40),
            hits: vec![
                ((5, 5, 46, 30), BarItem::Setting(0)),
                ((60, 5, 30, 30), BarItem::Reply),
                ((94, 5, 30, 30), BarItem::Delete),
            ],
            hover: None,
        };
        assert_eq!(bar.hit(10, 10), Some(BarItem::Setting(0)));
        assert_eq!(bar.hit(100, 20), Some(BarItem::Delete));
        assert_eq!(bar.hit(55, 20), None, "entre deux boutons");
        assert!(bar.contains(55, 20));
        assert_eq!(bar.setting_rect(0), Some((5, 5, 46, 30)));
        assert!(bar.mouse_move(65, 10));
        assert!(!bar.mouse_move(66, 11));
        bar.clear();
        assert!(!bar.contains(55, 20));
    }
}
