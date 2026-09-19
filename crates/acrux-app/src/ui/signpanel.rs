//! Panneau de « remplir et signer », à gauche de la page.
//!
//! Une barre d'icônes ne dit pas ce qu'elle va poser. Acrobat ouvre pour cet
//! outil un panneau qui **montre** les signatures enregistrées et ce qu'on
//! peut ajouter ; celui-ci fait de même, et les aperçus sont dessinés par le
//! code qui écrira dans le PDF — ce qu'on voit dans la liste est exactement
//! ce qui sera posé.
//!
//! Le panneau tient aussi la gestion des signatures, qui n'existait nulle
//! part : en créer une autre, refaire celle-ci, en supprimer une.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    // Une mise en page : x, y, r, g, b n'ont pas de meilleur nom.
    clippy::many_single_char_names
)]

use acrux_core::Matrix;
use acrux_features::fillsign::ink::{self, Nib, Pen, Weight};
use acrux_features::fillsign::marks::{outline_of, Mark};
use acrux_graphics::Rasterizer;

use crate::platform::Frame;
use crate::ui::lang::tr;
use crate::ui::paint::{round_rect, round_rect_outline};
use crate::ui::sign::{fill_outline, ink_rgb, Item, Saved, INKS};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Largeur du panneau, en pixels logiques.
pub const WIDTH: u32 = 268;

/// Ce que le panneau demande à l'application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Poser cet élément au prochain clic.
    Pick(Item),
    /// Se servir de la signature `index` (paraphe si `true`).
    Use(usize, bool),
    /// Dessiner une signature de plus (paraphe si `true`).
    Create(bool),
    /// Refaire cette signature.
    Edit(usize, bool),
    /// Oublier cette signature.
    Delete(usize, bool),
    /// Changer la couleur d'encre.
    Ink(usize),
    /// Changer la pointe ou l'épaisseur.
    Style(Nib, Weight),
    /// Quitter l'outil.
    Close,
}

/// Les marques, dans l'ordre d'Acrobat.
pub const MARKS: [Mark; 5] = [
    Mark::Check,
    Mark::Cross,
    Mark::Circle,
    Mark::Line,
    Mark::Dot,
];

/// Le panneau.
#[derive(Debug, Default)]
pub struct SignPanel {
    /// Élément choisi, mis en avant.
    pub item: Option<Item>,
    /// Signature en cours d'usage.
    pub current: usize,
    /// Couleur d'encre.
    pub color: usize,
    /// Pointe.
    pub nib: Nib,
    /// Épaisseur.
    pub weight: Weight,
    /// Défilement, en pixels.
    pub scroll: f64,
    /// Zones cliquables, remplies au dessin.
    hits: Vec<(i32, i32, i32, i32, Action)>,
    /// Zone survolée.
    hover: Option<Action>,
    /// Hauteur dessinée au dernier passage.
    content: i32,
}

impl SignPanel {
    /// Panneau neuf, avec l'encre retenue.
    #[must_use]
    pub fn with_ink(color: usize, nib: Nib, weight: Weight) -> Self {
        SignPanel {
            color,
            nib,
            weight,
            ..SignPanel::default()
        }
    }

    /// Couleur d'encre choisie, telle que le PDF l'attend.
    #[must_use]
    pub fn rgb(&self) -> [f64; 3] {
        INKS[self.color.min(INKS.len() - 1)].1
    }

    /// Action sous un point, s'il y en a une.
    #[must_use]
    /// Le **dernier** dessiné l'emporte : « Refaire » et « Retirer » sont
    /// posés sur la carte de la signature, qui couvre toute la largeur — les
    /// chercher dans l'ordre du dessin rendrait ces boutons inatteignables.
    pub fn action_at(&self, x: i32, y: i32) -> Option<Action> {
        self.hits
            .iter()
            .rev()
            .find(|&&(bx, by, bw, bh, _)| x >= bx && x < bx + bw && y >= by && y < by + bh)
            .map(|(_, _, _, _, action)| action.clone())
    }

    /// Clic : l'action visée, et le panneau retient ce qui est choisi.
    pub fn mouse_down(&mut self, x: i32, y: i32) -> Option<Action> {
        let action = self.action_at(x, y)?;
        match &action {
            Action::Pick(item) => self.item = Some(*item),
            Action::Use(index, false) => {
                self.current = *index;
                self.item = Some(Item::Signature);
            }
            Action::Use(_, true) => self.item = Some(Item::Initials),
            Action::Ink(index) => self.color = *index,
            Action::Style(nib, weight) => {
                self.nib = *nib;
                self.weight = *weight;
            }
            _ => {}
        }
        Some(action)
    }

    /// Survol ; rend vrai si l'affichage doit changer.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let over = self.action_at(x, y);
        let changed = over != self.hover;
        self.hover = over;
        changed
    }

    /// Le pointeur quitte le panneau.
    pub fn leave(&mut self) -> bool {
        let changed = self.hover.is_some();
        self.hover = None;
        changed
    }

    /// Molette : fait défiler le contenu qui dépasse.
    pub fn wheel(&mut self, delta: f64, height: f64) {
        let max = (f64::from(self.content) - height).max(0.0);
        self.scroll = (self.scroll - delta).clamp(0.0, max);
    }

    /// Dessine le panneau dans sa sous-vue (origine en haut à gauche).
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f32,
        signatures: &[Saved],
        initials: Option<&Saved>,
        full: usize,
    ) {
        let (fw, fh) = (frame.width as i32, frame.height as i32);
        frame.fill_rect(0, 0, fw, fh, theme.bar.0, theme.bar.1, theme.bar.2);
        frame.fill_rect(
            fw - 1,
            0,
            1,
            fh,
            theme.separator.0,
            theme.separator.1,
            theme.separator.2,
        );
        self.hits.clear();
        let size = theme.font_size * dpi;
        let pad = (14.0 * dpi) as i32;
        let mut y = pad - self.scroll as i32;
        // 1. Les signatures : ce qu'on a, tel qu'il sera posé.
        y = Self::paint_heading(frame, text, theme, size, pad, y, tr("Signatures"));
        for (index, saved) in signatures.iter().enumerate() {
            y = self.paint_card(
                frame, text, raster, theme, dpi, pad, y, fw, saved, index, false,
            );
        }
        if signatures.len() < full {
            let label = if signatures.is_empty() {
                tr("Créer une signature")
            } else {
                tr("Ajouter une signature")
            };
            y = self.paint_button(
                frame,
                text,
                theme,
                dpi,
                pad,
                y,
                fw - 2 * pad,
                label,
                &Action::Create(false),
                true,
            );
            y += (6.0 * dpi) as i32;
        }
        y = Self::paint_heading(frame, text, theme, size, pad, y, tr("Paraphe"));
        if let Some(saved) = initials {
            y = self.paint_card(frame, text, raster, theme, dpi, pad, y, fw, saved, 0, true);
        } else {
            {
                y = self.paint_button(
                    frame,
                    text,
                    theme,
                    dpi,
                    pad,
                    y,
                    fw - 2 * pad,
                    tr("Créer un paraphe"),
                    &Action::Create(true),
                    true,
                );
                y += (6.0 * dpi) as i32;
            }
        }
        // 2. Ce qu'on ajoute soi-même.
        y = Self::paint_heading(frame, text, theme, size, pad, y, tr("Ajouter"));
        for (item, hint) in [
            (Item::Text, tr("Cliquez sur la page et tapez")),
            (Item::Draw, tr("Tracez à main levée")),
        ] {
            y = self.paint_row(frame, text, theme, dpi, pad, y, fw, item, hint);
        }
        y += (4.0 * dpi) as i32;
        y = Self::paint_heading(frame, text, theme, size, pad, y, tr("Marques"));
        let cell = (fw - 2 * pad) / MARKS.len() as i32;
        let cell_h = (38.0 * dpi) as i32;
        for (index, mark) in MARKS.iter().enumerate() {
            let x = pad + cell * index as i32;
            let item = Item::Mark(*mark);
            let chosen = self.item == Some(item);
            let hovered = self.hover == Some(Action::Pick(item));
            if chosen || hovered {
                let (r, g, b) = if chosen { theme.accent } else { theme.hover };
                frame.fill_rect(x, y, cell - (4.0 * dpi) as i32, cell_h, r, g, b);
            }
            let color = if chosen { (255, 255, 255) } else { theme.text };
            let outline = outline_of(*mark);
            let glyph = f64::from(20.0 * dpi);
            let (bw, bh) = (
                (outline.bbox.x1 - outline.bbox.x0).max(0.01),
                (outline.bbox.y1 - outline.bbox.y0).max(0.01),
            );
            let scale = (glyph / bw).min(glyph / bh);
            let cx = f64::from(x) + f64::from(cell - (4.0 * dpi) as i32) / 2.0 - bw * scale / 2.0;
            let cy = f64::from(y) + f64::from(cell_h) / 2.0 + bh * scale / 2.0;
            let m = Matrix::new(
                scale,
                0.0,
                0.0,
                -scale,
                cx - outline.bbox.x0 * scale,
                cy + outline.bbox.y0 * scale,
            );
            fill_outline(frame, raster, &outline, &m, color);
            self.hits
                .push((x, y, cell - (4.0 * dpi) as i32, cell_h, Action::Pick(item)));
        }
        y += cell_h + (10.0 * dpi) as i32;
        // 3. L'encre, commune à tout ce qu'on pose.
        y = Self::paint_heading(frame, text, theme, size, pad, y, tr("Encre"));
        let swatch = (22.0 * dpi) as i32;
        for index in 0..INKS.len() {
            let x = pad + (swatch + (10.0 * dpi) as i32) * index as i32;
            if self.color == index {
                frame.fill_rect(
                    x - (3.0 * dpi) as i32,
                    y - (3.0 * dpi) as i32,
                    swatch + (6.0 * dpi) as i32,
                    swatch + (6.0 * dpi) as i32,
                    theme.accent.0,
                    theme.accent.1,
                    theme.accent.2,
                );
            }
            let (r, g, b) = ink_rgb(index);
            frame.fill_rect(x, y, swatch, swatch, r, g, b);
            frame.fill_rect(x, y, swatch, 1.max(dpi as i32), 0x8A, 0x8F, 0x99);
            self.hits.push((
                x - (3.0 * dpi) as i32,
                y - (3.0 * dpi) as i32,
                swatch + (6.0 * dpi) as i32,
                swatch + (6.0 * dpi) as i32,
                Action::Ink(index),
            ));
        }
        y += swatch + (14.0 * dpi) as i32;
        // Épaisseurs : trois traits, qu'on choisit en les voyant.
        let cell = (fw - 2 * pad) / 3;
        let cell_h = (26.0 * dpi) as i32;
        for (index, weight) in Weight::all().into_iter().enumerate() {
            let x = pad + cell * index as i32;
            let chosen = self.weight == weight;
            let hovered = self.hover == Some(Action::Style(self.nib, weight));
            if chosen || hovered {
                let (r, g, b) = if chosen { theme.accent } else { theme.hover };
                frame.fill_rect(x, y, cell - (4.0 * dpi) as i32, cell_h, r, g, b);
            }
            let thickness = match weight {
                Weight::Thin => 1.0,
                Weight::Medium => 2.5,
                Weight::Thick => 5.0,
            };
            let th = ((thickness * f64::from(dpi)) as i32).max(1);
            let color = if chosen { (255, 255, 255) } else { theme.text };
            frame.fill_rect(
                x + (8.0 * dpi) as i32,
                y + (cell_h - th) / 2,
                cell - (20.0 * dpi) as i32,
                th,
                color.0,
                color.1,
                color.2,
            );
            self.hits.push((
                x,
                y,
                cell - (4.0 * dpi) as i32,
                cell_h,
                Action::Style(self.nib, weight),
            ));
        }
        y += cell_h + (8.0 * dpi) as i32;
        for (index, nib) in Nib::all().into_iter().enumerate() {
            let x = pad + cell * index as i32;
            let chosen = self.nib == nib;
            let hovered = self.hover == Some(Action::Style(nib, self.weight));
            if chosen || hovered {
                let (r, g, b) = if chosen { theme.accent } else { theme.hover };
                frame.fill_rect(x, y, cell - (4.0 * dpi) as i32, cell_h, r, g, b);
            }
            let color = if chosen {
                (255, 255, 255)
            } else {
                theme.text_dim
            };
            let label = tr(nib.label());
            let w = text.measure(size, label);
            text.draw(
                frame,
                x as f32 + (cell as f32 - (4.0 * dpi) - w) / 2.0,
                y as f32 + f32::midpoint(cell_h as f32, text.ascent(size)) - 1.0,
                size,
                label,
                color,
            );
            self.hits.push((
                x,
                y,
                cell - (4.0 * dpi) as i32,
                cell_h,
                Action::Style(nib, self.weight),
            ));
        }
        y += cell_h + (16.0 * dpi) as i32;
        y = self.paint_button(
            frame,
            text,
            theme,
            dpi,
            pad,
            y,
            fw - 2 * pad,
            tr("Terminer"),
            &Action::Close,
            false,
        );
        self.content = y + self.scroll as i32 + pad;
    }

    /// Titre de section.
    fn paint_heading(
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        size: f32,
        x: i32,
        y: i32,
        label: &str,
    ) -> i32 {
        text.draw(
            frame,
            x as f32,
            y as f32 + text.ascent(size * 0.9),
            size * 0.9,
            label,
            theme.text_dim,
        );
        y + (size * 1.9) as i32
    }

    /// Bouton pleine largeur.
    fn paint_button(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
        x: i32,
        y: i32,
        width: i32,
        label: &str,
        action: &Action,
        strong: bool,
    ) -> i32 {
        let size = theme.font_size * dpi;
        let h = (32.0 * dpi) as i32;
        let hovered = self.hover.as_ref() == Some(action);
        let (r, g, b) = match (strong, hovered) {
            (true, false) => theme.accent,
            (true, true) => (
                theme.accent.0.saturating_add(18),
                theme.accent.1.saturating_add(18),
                theme.accent.2.saturating_add(10),
            ),
            (false, true) => theme.separator,
            (false, false) => theme.hover,
        };
        round_rect(frame, x, y, width, h, 9.0 * dpi, (r, g, b));
        let w = text.measure(size, label);
        text.draw(
            frame,
            x as f32 + (width as f32 - w) / 2.0,
            y as f32 + f32::midpoint(h as f32, text.ascent(size)) - 1.0,
            size,
            label,
            if strong { (255, 255, 255) } else { theme.text },
        );
        self.hits.push((x, y, width, h, action.clone()));
        y + h + (8.0 * dpi) as i32
    }

    /// Ligne « ajouter » : un nom et sa consigne.
    fn paint_row(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
        pad: i32,
        y: i32,
        fw: i32,
        item: Item,
        hint: &str,
    ) -> i32 {
        let size = theme.font_size * dpi;
        let h = (42.0 * dpi) as i32;
        let chosen = self.item == Some(item);
        let hovered = self.hover == Some(Action::Pick(item));
        if chosen || hovered {
            let (r, g, b) = if chosen { theme.accent } else { theme.hover };
            round_rect(frame, pad, y, fw - 2 * pad, h, 9.0 * dpi, (r, g, b));
        }
        let color = if chosen { (255, 255, 255) } else { theme.text };
        let dim = if chosen {
            (230, 235, 245)
        } else {
            theme.text_dim
        };
        text.draw(
            frame,
            (pad + (12.0 * dpi) as i32) as f32,
            y as f32 + text.ascent(size) + (8.0 * dpi),
            size,
            tr(item.label()),
            color,
        );
        text.draw_clipped(
            frame,
            (pad + (12.0 * dpi) as i32) as f32,
            y as f32 + text.ascent(size) * 2.0 + (12.0 * dpi),
            size * 0.88,
            hint,
            dim,
            (fw - 2 * pad - (18.0 * dpi) as i32) as f32,
        );
        self.hits
            .push((pad, y, fw - 2 * pad, h, Action::Pick(item)));
        y + h + (4.0 * dpi) as i32
    }

    /// Carte d'une signature : son dessin, et ce qu'on peut en faire.
    fn paint_card(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f32,
        pad: i32,
        y: i32,
        fw: i32,
        saved: &Saved,
        index: usize,
        initials: bool,
    ) -> i32 {
        let size = theme.font_size * dpi;
        let width = fw - 2 * pad;
        let h = (72.0 * dpi) as i32;
        let use_action = Action::Use(index, initials);
        let chosen = if initials {
            self.item == Some(Item::Initials)
        } else {
            self.item == Some(Item::Signature) && self.current == index
        };
        let hovered = self.hover == Some(use_action.clone());
        // Le fond de la carte est clair : une signature s'écrit à l'encre
        // sombre, et se verrait mal sur le fond du panneau.
        let radius = 10.0 * dpi;
        round_rect(frame, pad, y, width, h, radius, (0xF4, 0xF5, 0xF8));
        if chosen || hovered {
            let (r, g, b) = if chosen {
                theme.accent
            } else {
                theme.separator
            };
            round_rect_outline(
                frame,
                pad,
                y,
                width,
                h,
                radius,
                (2.0 * dpi).max(1.0),
                (r, g, b),
            );
        }
        let inner = (10.0 * dpi) as i32;
        let (ax, ay) = (pad + inner, y + inner);
        let (aw, ah) = (width - 2 * inner - (54.0 * dpi) as i32, h - 2 * inner);
        match saved {
            Saved::Drawn(strokes) => {
                // Même plume qu'à la pose, mise à l'échelle de la carte.
                let pen = Pen::styled(f64::from(aw), f64::from(ah), self.nib, self.weight);
                let outline = ink::outline(strokes, &pen);
                let (bw, bh) = (
                    (outline.bbox.x1 - outline.bbox.x0).max(0.01),
                    (outline.bbox.y1 - outline.bbox.y0).max(0.01),
                );
                let scale = (f64::from(aw) / bw).min(f64::from(ah) / bh);
                let dx = f64::from(ax) + (f64::from(aw) - bw * scale) / 2.0;
                let dy = f64::from(ay) + f64::midpoint(f64::from(ah), bh * scale);
                let m = Matrix::new(
                    scale,
                    0.0,
                    0.0,
                    -scale,
                    dx - outline.bbox.x0 * scale,
                    dy + outline.bbox.y0 * scale,
                );
                fill_outline(frame, raster, &outline, &m, ink_rgb(self.color));
            }
            Saved::Typed(name) => {
                text.draw_clipped(
                    frame,
                    ax as f32,
                    ay as f32 + f32::midpoint(ah as f32, text.ascent(size * 1.3)),
                    size * 1.3,
                    name,
                    (0x20, 0x24, 0x30),
                    aw as f32,
                );
            }
            Saved::Image(path) => {
                let name = path
                    .file_name()
                    .map_or_else(|| "image".to_string(), |n| n.to_string_lossy().into_owned());
                text.draw_clipped(
                    frame,
                    ax as f32,
                    ay as f32 + f32::midpoint(ah as f32, text.ascent(size)),
                    size,
                    &name,
                    (0x50, 0x55, 0x60),
                    aw as f32,
                );
            }
        }
        self.hits.push((pad, y, width, h, use_action));
        // Refaire / supprimer, à droite de la carte.
        let bw = (46.0 * dpi) as i32;
        let bh = (24.0 * dpi) as i32;
        let bx = pad + width - bw - (8.0 * dpi) as i32;
        for (row, (label, action)) in [
            (tr("Refaire"), Action::Edit(index, initials)),
            (tr("Retirer"), Action::Delete(index, initials)),
        ]
        .into_iter()
        .enumerate()
        {
            let by = y + (8.0 * dpi) as i32 + (bh + (6.0 * dpi) as i32) * row as i32;
            let hovered = self.hover == Some(action.clone());
            let (r, g, b) = if hovered {
                theme.accent
            } else {
                (0xE2, 0xE5, 0xEB)
            };
            round_rect(frame, bx, by, bw, bh, 6.0 * dpi, (r, g, b));
            let color = if hovered {
                (255, 255, 255)
            } else {
                (0x33, 0x38, 0x44)
            };
            let w = text.measure(size * 0.82, label);
            text.draw(
                frame,
                bx as f32 + (bw as f32 - w) / 2.0,
                by as f32 + f32::midpoint(bh as f32, text.ascent(size * 0.82)) - 1.0,
                size * 0.82,
                label,
                color,
            );
            self.hits.push((bx, by, bw, bh, action));
        }
        y + h + (8.0 * dpi) as i32
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, Item, Nib, SignPanel, Weight};

    #[test]
    fn choisir_une_signature_la_rend_courante() {
        let mut p = SignPanel::default();
        p.hits.push((0, 0, 100, 40, Action::Use(2, false)));
        assert_eq!(p.mouse_down(10, 10), Some(Action::Use(2, false)));
        assert_eq!(p.current, 2);
        assert_eq!(p.item, Some(Item::Signature));
    }

    #[test]
    fn lencre_et_la_plume_se_retiennent() {
        let mut p = SignPanel::with_ink(1, Nib::Fountain, Weight::Thin);
        assert_eq!(p.color, 1);
        p.hits.push((0, 0, 10, 10, Action::Ink(2)));
        p.hits
            .push((0, 20, 10, 10, Action::Style(Nib::Marker, Weight::Thick)));
        p.mouse_down(1, 1);
        assert_eq!(p.color, 2);
        p.mouse_down(1, 21);
        assert_eq!(p.nib, Nib::Marker);
        assert_eq!(p.weight, Weight::Thick);
    }

    #[test]
    fn le_defilement_reste_dans_le_contenu() {
        let mut p = SignPanel {
            content: 900,
            ..SignPanel::default()
        };
        p.wheel(-500.0, 400.0);
        assert!((p.scroll - 500.0).abs() < 0.01);
        p.wheel(-500.0, 400.0);
        assert!((p.scroll - 500.0).abs() < 0.01, "on ne dépasse pas le bas");
        p.wheel(9_000.0, 400.0);
        assert!(p.scroll.abs() < 0.01, "ni le haut");
    }

    #[test]
    fn refaire_et_retirer_passent_devant_la_carte() {
        // La carte couvre toute la largeur ; ses deux boutons sont posés
        // dessus. Un clic dessus doit les atteindre, pas la carte.
        let mut p = SignPanel::default();
        p.hits.push((0, 0, 200, 80, Action::Use(0, false)));
        p.hits.push((150, 8, 46, 24, Action::Edit(0, false)));
        p.hits.push((150, 40, 46, 24, Action::Delete(0, false)));
        assert_eq!(p.action_at(160, 16), Some(Action::Edit(0, false)));
        assert_eq!(p.action_at(160, 48), Some(Action::Delete(0, false)));
        assert_eq!(p.action_at(20, 40), Some(Action::Use(0, false)));
    }
}
