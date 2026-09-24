//! Le sélecteur de tampons : la carte déroulée sous la barre de l'outil
//! « Tamponner », comme le menu des tampons d'Acrobat.
//!
//! Les douze tampons standard en grille de trois colonnes, chacun **dessiné
//! tel qu'il sera posé** : la vignette est un vrai rendu du tampon sur un
//! carré de papier, pas un libellé qui l'imite. Puis les images déjà
//! choisies, la case « Ajouter mon nom et la date » (le tampon dynamique)
//! et le bouton « Depuis une image… ». Flèches, Entrée, Espace et Échap
//! font ce qu'on attend.
//!
//! Le sélecteur ne connaît ni le document ni le moteur : il reçoit ses
//! vignettes toutes faites et rend ce qu'on a choisi ([`Pick`]). C'est le
//! visualiseur qui les calcule, à l'ouverture et une fois pour toutes —
//! jamais par un réveil de la fenêtre.

// Coordonnées d'écran entières, et une mise en page lue de haut en bas
// (x, y, s pour l'échelle, comme dans `pickers.rs`).
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::too_many_arguments,
    clippy::many_single_char_names
)]

use acrux_features::rubber_stamp::StandardStamp;
use acrux_graphics::{Bitmap, Rasterizer};

use crate::platform::{Frame, Key};
use crate::ui::controls::{checkbox, CheckLook};
use crate::ui::icons::{self, Icon};
use crate::ui::lang::tr;
use crate::ui::paint::{button, round_rect, round_rect_outline, shadow, ButtonLook};
use crate::ui::pickers::Outcome;
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Rectangle `(x, y, largeur, hauteur)`.
type Rect = (i32, i32, i32, i32);

/// Colonnes de la grille.
pub const COLUMNS: usize = 3;
/// Images récentes montrées au plus : une rangée.
pub const MAX_IMAGES: usize = 3;
/// Largeur d'une case de tampon, en pixels logiques.
const CELL_W: f32 = 196.0;
/// Hauteur d'une case de tampon.
const CELL_H: f32 = 60.0;
/// Marge du papier dans sa case, où se lit la mise en évidence.
const INSET: f32 = 5.0;
/// Marge de la vignette dans son papier.
const PAPER_PAD: f32 = 6.0;
/// Hauteur d'une case d'image.
const IMAGE_H: f32 = 34.0;
/// Marge intérieure de la carte.
const PAD: f32 = 14.0;
/// Écart entre deux cases.
const GAP: f32 = 6.0;
/// Hauteur d'un intertitre.
const HEADING: f32 = 24.0;
/// Hauteur de la rangée du bas (case à cocher et bouton).
const FOOTER: f32 = 34.0;
/// Papier des vignettes : blanc, dans les deux thèmes, comme la page qui
/// recevra le tampon.
const PAPER: (u8, u8, u8) = (252, 252, 250);

fn inside(r: Rect, x: i32, y: i32) -> bool {
    x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
}

/// Ce qu'on a choisi.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// Un tampon standard.
    Stamp(StandardStamp),
    /// Une image récente, par son rang.
    Image(usize),
    /// « Depuis une image… » : choisir un fichier.
    FromFile,
    /// La case « Ajouter mon nom et la date » vient de changer.
    Dynamic(bool),
}

/// Le sélecteur de tampons.
#[derive(Debug)]
pub struct StampPicker {
    /// Case mise en évidence : les douze tampons, puis les images.
    selected: usize,
    /// Case survolée.
    hover: Option<usize>,
    /// Le pointeur est sur la case à cocher.
    hover_check: bool,
    /// Le pointeur est sur « Depuis une image… ».
    hover_file: bool,
    /// Tampon dynamique : le nom et la date s'ajoutent.
    dynamic: bool,
    /// Noms des images récentes.
    images: Vec<String>,
    /// Zones remplies au dessin.
    card: Rect,
    cells: Vec<Rect>,
    check: Rect,
    file: Rect,
}

impl StampPicker {
    /// Sélecteur neuf ; `current` est la case du choix en cours.
    #[must_use]
    pub fn new(current: usize, dynamic: bool, mut images: Vec<String>) -> Self {
        images.truncate(MAX_IMAGES);
        let count = StandardStamp::ALL.len() + images.len();
        StampPicker {
            selected: current.min(count.saturating_sub(1)),
            hover: None,
            hover_check: false,
            hover_file: false,
            dynamic,
            images,
            card: (0, 0, 0, 0),
            cells: Vec::new(),
            check: (0, 0, 0, 0),
            file: (0, 0, 0, 0),
        }
    }

    /// Place offerte à une vignette, en pixels, pour une échelle d'écran :
    /// le visualiseur rend les tampons à cette taille.
    #[must_use]
    pub fn thumb_box(dpi: f32) -> (f64, f64) {
        let inner = |side: f32| f64::from((side - 2.0 * (INSET + PAPER_PAD)) * dpi);
        (inner(CELL_W), inner(CELL_H))
    }

    /// Vrai si le nom et la date s'ajoutent.
    #[must_use]
    pub fn dynamic(&self) -> bool {
        self.dynamic
    }

    /// Nombre de cases.
    fn count(&self) -> usize {
        StandardStamp::ALL.len() + self.images.len()
    }

    /// Ce que choisit une case.
    fn item(index: usize) -> Pick {
        match StandardStamp::ALL.get(index) {
            Some(stamp) => Pick::Stamp(*stamp),
            None => Pick::Image(index - StandardStamp::ALL.len()),
        }
    }

    /// Clic : une case choisit, la case à cocher bascule, le bouton ouvre le
    /// choix d'un fichier, dehors referme.
    pub fn mouse_down(&mut self, x: i32, y: i32) -> Outcome<Pick> {
        if !inside(self.card, x, y) {
            return Outcome::Close;
        }
        if let Some(index) = self.cells.iter().position(|r| inside(*r, x, y)) {
            self.selected = index;
            return Outcome::Pick(Self::item(index));
        }
        if inside(self.check, x, y) {
            self.dynamic = !self.dynamic;
            return Outcome::Live(Pick::Dynamic(self.dynamic));
        }
        if inside(self.file, x, y) {
            return Outcome::Pick(Pick::FromFile);
        }
        Outcome::Stay
    }

    /// Survol ; vrai s'il y a quelque chose à redessiner.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let hover = self.cells.iter().position(|r| inside(*r, x, y));
        let check = inside(self.check, x, y);
        let file = inside(self.file, x, y);
        let changed = hover != self.hover || check != self.hover_check || file != self.hover_file;
        self.hover = hover;
        self.hover_check = check;
        self.hover_file = file;
        changed
    }

    /// Touche : les flèches parcourent la grille, Entrée choisit, Espace
    /// coche ou décoche le nom et la date, Échap referme.
    pub fn key(&mut self, key: Key) -> Outcome<Pick> {
        let count = self.count();
        let last = count.saturating_sub(1);
        match key {
            Key::Escape => return Outcome::Close,
            Key::Enter => return Outcome::Pick(Self::item(self.selected)),
            Key::Space => {
                self.dynamic = !self.dynamic;
                return Outcome::Live(Pick::Dynamic(self.dynamic));
            }
            Key::Left => self.selected = self.selected.saturating_sub(1),
            Key::Right => self.selected = (self.selected + 1).min(last),
            Key::Up => {
                if self.selected >= COLUMNS {
                    self.selected -= COLUMNS;
                }
            }
            Key::Down => {
                if self.selected + COLUMNS < count {
                    self.selected += COLUMNS;
                } else if self.selected < StandardStamp::ALL.len()
                    && count > StandardStamp::ALL.len()
                {
                    // De la dernière rangée des tampons aux images, même
                    // quand la colonne n'en a pas.
                    self.selected = StandardStamp::ALL.len();
                }
            }
            Key::Home => self.selected = 0,
            Key::End => self.selected = last,
            _ => {}
        }
        Outcome::Stay
    }

    /// Dessine la carte sous `anchor` (le bouton qui l'ouvre). `thumbs`
    /// porte la vignette de chaque tampon standard, dans l'ordre de
    /// [`StandardStamp::ALL`] ; une vignette absente laisse le libellé.
    #[allow(clippy::too_many_lines)] // une carte, lue de haut en bas
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f32,
        anchor: Rect,
        thumbs: &[Option<Bitmap>],
    ) {
        let s = |v: f32| (v * dpi).round() as i32;
        let size = theme.font_size * dpi;
        let (pad, gap) = (s(PAD), s(GAP));
        let (cell_w, cell_h) = (s(CELL_W), s(CELL_H));
        let grid_w = cell_w * COLUMNS as i32 + gap * (COLUMNS as i32 - 1);
        let stamp_rows = StandardStamp::ALL.len().div_ceil(COLUMNS) as i32;
        let images_h = if self.images.is_empty() {
            0
        } else {
            s(HEADING) + s(IMAGE_H) + s(8.0)
        };
        let width = grid_w + 2 * pad;
        let height = pad
            + s(HEADING)
            + stamp_rows * cell_h
            + (stamp_rows - 1) * gap
            + s(10.0)
            + images_h
            + s(FOOTER)
            + pad;
        let x = anchor
            .0
            .min(frame.width as i32 - width - s(8.0))
            .max(s(8.0));
        let y = anchor.1 + anchor.3 + s(4.0);
        self.card = (x, y, width, height);
        let radius = 12.0 * dpi;
        shadow(frame, x, y, width, height, radius, 26.0 * dpi, 0.45);
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
        let gx = x + pad;
        let mut cy = y + pad;
        let heading = |frame: &mut Frame<'_>, text: &mut TextRenderer, label: &str, at: i32| {
            text.draw(
                frame,
                gx as f32,
                at as f32 + text.ascent(size * 0.86) + dpi,
                size * 0.86,
                label,
                theme.text_dim,
            );
        };

        // Les tampons standard, chacun sur son papier.
        heading(frame, text, tr("Tampons"), cy);
        cy += s(HEADING);
        self.cells.clear();
        for (index, stamp) in StandardStamp::ALL.iter().enumerate() {
            let (col, row) = ((index % COLUMNS) as i32, (index / COLUMNS) as i32);
            let cell = (
                gx + col * (cell_w + gap),
                cy + row * (cell_h + gap),
                cell_w,
                cell_h,
            );
            self.cells.push(cell);
            let lit = self.hover == Some(index);
            if lit {
                round_rect(
                    frame,
                    cell.0,
                    cell.1,
                    cell.2,
                    cell.3,
                    8.0 * dpi,
                    theme.hover,
                );
            }
            let inset = s(INSET);
            let paper = (
                cell.0 + inset,
                cell.1 + inset,
                cell.2 - 2 * inset,
                cell.3 - 2 * inset,
            );
            round_rect(frame, paper.0, paper.1, paper.2, paper.3, 5.0 * dpi, PAPER);
            // Un filet : sur la carte claire, le papier s'y fondrait.
            round_rect_outline(
                frame,
                paper.0,
                paper.1,
                paper.2,
                paper.3,
                5.0 * dpi,
                dpi.max(1.0) * 0.75,
                theme.separator,
            );
            if let Some(b) = thumbs.get(index).and_then(Option::as_ref) {
                frame.blit_rgba_premultiplied(
                    paper.0 + (paper.2 - b.width() as i32) / 2,
                    paper.1 + (paper.3 - b.height() as i32) / 2,
                    b.width(),
                    b.height(),
                    b.data(),
                );
            } else {
                {
                    let label = stamp.label(if crate::ui::lang::english() {
                        acrux_features::rubber_stamp::Language::En
                    } else {
                        acrux_features::rubber_stamp::Language::Fr
                    });
                    let c = stamp.color();
                    let ink = (
                        (c[0] * 255.0) as u8,
                        (c[1] * 255.0) as u8,
                        (c[2] * 255.0) as u8,
                    );
                    let w = text.measure(size, label);
                    text.draw(
                        frame,
                        paper.0 as f32 + (paper.2 as f32 - w) / 2.0,
                        (paper.1 + paper.3 / 2) as f32 + text.ascent(size) / 2.0,
                        size,
                        label,
                        ink,
                    );
                }
            }
            if self.selected == index {
                round_rect_outline(
                    frame,
                    cell.0,
                    cell.1,
                    cell.2,
                    cell.3,
                    8.0 * dpi,
                    (2.0 * dpi).max(1.0),
                    theme.accent,
                );
            }
        }
        cy += stamp_rows * cell_h + (stamp_rows - 1) * gap + s(10.0);

        // Les images déjà choisies : leur nom, derrière l'icône d'image.
        if !self.images.is_empty() {
            heading(frame, text, tr("Images"), cy);
            cy += s(HEADING);
            let row_h = s(IMAGE_H);
            for (i, name) in self.images.iter().enumerate() {
                let index = StandardStamp::ALL.len() + i;
                let cell = (gx + i as i32 * (cell_w + gap), cy, cell_w, row_h);
                self.cells.push(cell);
                let bg = if self.hover == Some(index) {
                    theme.button_hover
                } else {
                    theme.hover
                };
                round_rect(frame, cell.0, cell.1, cell.2, cell.3, 8.0 * dpi, bg);
                let icon = 18.0 * dpi;
                icons::draw(
                    frame,
                    raster,
                    Icon::Image,
                    cell.0 + s(8.0),
                    cell.1 + (row_h - icon.round() as i32) / 2,
                    icon,
                    theme.text,
                );
                let tx = cell.0 + s(8.0) + icon.round() as i32 + s(8.0);
                text.draw_clipped(
                    frame,
                    tx as f32,
                    (cell.1 + row_h / 2) as f32 + text.ascent(size) / 2.0,
                    size,
                    name,
                    theme.text,
                    (cell.0 + cell.2 - tx - s(8.0)) as f32,
                );
                if self.selected == index {
                    round_rect_outline(
                        frame,
                        cell.0,
                        cell.1,
                        cell.2,
                        cell.3,
                        8.0 * dpi,
                        (2.0 * dpi).max(1.0),
                        theme.accent,
                    );
                }
            }
            cy += row_h + s(8.0);
        }

        // La rangée du bas : le nom et la date, puis le choix d'un fichier.
        let footer = s(FOOTER);
        self.check = checkbox(
            frame,
            text,
            theme,
            dpi,
            gx,
            cy + (footer - s(22.0)) / 2,
            tr("Ajouter mon nom et la date"),
            CheckLook {
                on: self.dynamic,
                enabled: true,
                hovered: self.hover_check,
                focused: false,
            },
        );
        let label = tr("Depuis une image…");
        let bw = text.measure(size, label).ceil() as i32 + s(28.0);
        let file = (gx + grid_w - bw, cy, bw, footer);
        self.file = file;
        let ink = button(
            frame,
            file.0,
            file.1,
            file.2,
            file.3,
            dpi,
            theme,
            ButtonLook {
                hovered: self.hover_file,
                ..ButtonLook::default()
            },
        );
        text.draw(
            frame,
            (file.0 + s(14.0)) as f32,
            (file.1 + footer / 2) as f32 + text.ascent(size) / 2.0,
            size,
            label,
            ink,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_fleches_parcourent_la_grille_de_trois_colonnes() {
        let mut p = StampPicker::new(0, false, Vec::new());
        assert_eq!(p.key(Key::Right), Outcome::Stay);
        assert_eq!(p.key(Key::Down), Outcome::Stay);
        // Deuxième colonne, deuxième rangée.
        assert_eq!(
            p.key(Key::Enter),
            Outcome::Pick(Pick::Stamp(StandardStamp::ALL[4]))
        );
        p.key(Key::Up);
        p.key(Key::Up);
        assert_eq!(
            p.key(Key::Enter),
            Outcome::Pick(Pick::Stamp(StandardStamp::ALL[1])),
            "on ne sort pas par le haut"
        );
        p.key(Key::End);
        p.key(Key::Right);
        assert_eq!(
            p.key(Key::Enter),
            Outcome::Pick(Pick::Stamp(StandardStamp::ALL[11])),
            "ni par la fin"
        );
        p.key(Key::Home);
        p.key(Key::Left);
        assert_eq!(
            p.key(Key::Enter),
            Outcome::Pick(Pick::Stamp(StandardStamp::ALL[0]))
        );
    }

    #[test]
    fn les_images_suivent_les_tampons() {
        let mut p = StampPicker::new(10, false, vec!["cachet.png".into(), "logo.png".into()]);
        // La colonne se garde : sous « Non approuvé », la deuxième image.
        p.key(Key::Down);
        assert_eq!(p.key(Key::Enter), Outcome::Pick(Pick::Image(1)));
        p.key(Key::Left);
        assert_eq!(p.key(Key::Enter), Outcome::Pick(Pick::Image(0)));
        // Au-delà de la dernière image, on reste.
        p.key(Key::Right);
        p.key(Key::Right);
        assert_eq!(p.key(Key::Enter), Outcome::Pick(Pick::Image(1)));
        // Sous la troisième colonne, sans image : la première image.
        let mut p = StampPicker::new(11, false, vec!["cachet.png".into()]);
        p.key(Key::Down);
        assert_eq!(p.key(Key::Enter), Outcome::Pick(Pick::Image(0)));
    }

    #[test]
    fn espace_coche_et_echap_referme() {
        let mut p = StampPicker::new(0, false, Vec::new());
        assert_eq!(p.key(Key::Space), Outcome::Live(Pick::Dynamic(true)));
        assert!(p.dynamic());
        assert_eq!(p.key(Key::Space), Outcome::Live(Pick::Dynamic(false)));
        assert_eq!(p.key(Key::Escape), Outcome::Close);
    }

    #[test]
    fn un_clic_dehors_referme_et_le_choix_en_cours_est_borne() {
        let mut p = StampPicker::new(99, true, Vec::new());
        assert_eq!(
            p.key(Key::Enter),
            Outcome::Pick(Pick::Stamp(StandardStamp::Void))
        );
        // Jamais dessiné : sa carte est vide, tout clic est dehors.
        assert_eq!(p.mouse_down(10, 10), Outcome::Close);
        let (w, h) = StampPicker::thumb_box(1.5);
        assert!(w > 200.0 && h > 40.0 && w > 3.0 * h, "{w} × {h}");
    }
}
