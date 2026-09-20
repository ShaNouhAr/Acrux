//! Les deux sélecteurs de la barre « Modifier le PDF » : la **liste des
//! polices** et le **nuancier**.
//!
//! # La liste des polices
//!
//! Toutes les familles installées (voir [`acrux_features::sysfonts`]), comme
//! dans un traitement de texte : un champ de recherche en tête — on tape
//! « geo », il reste Georgia —, les polices récentes, puis la liste complète,
//! **chaque nom dessiné dans sa propre police**. Flèches, Entrée, Échap et
//! molette font ce qu'on attend.
//!
//! Dessiner un nom dans sa police demande d'ouvrir le fichier : ce n'est fait
//! que pour les lignes visibles, quelques-unes par image, et l'image obtenue
//! est gardée. Faire défiler trois cents polices ne coûte donc que celles
//! qu'on a regardées.
//!
//! # Le nuancier
//!
//! Trois façons de choisir une couleur, de la plus rapide à la plus précise :
//! les **couleurs du thème** avec leurs teintes claires et foncées, les
//! **couleurs vives**, les **récentes** ; puis un carré saturation-luminosité
//! avec sa réglette de teinte, qui agit **en direct** sur le bloc pendant
//! qu'on glisse ; enfin le code hexadécimal, pour reprendre une couleur
//! exacte. Un clic sur une pastille choisit et referme.

// Coordonnées d'écran entières, et une mise en page lue de haut en bas.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

use std::collections::HashMap;
use std::sync::Mutex;

use acrux_core::{Matrix, Path};
use acrux_features::sysfonts::{self, Family};
use acrux_graphics::{FillRule, Rasterizer};

use crate::platform::{Frame, Key};
use crate::ui::input::{InputAction, TextInput};
use crate::ui::paint::{button, round_rect, round_rect_outline, shadow, ButtonLook, Rgb};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Ce qu'un sélecteur répond à un geste.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome<T> {
    /// Rien à faire : le sélecteur reste ouvert.
    Stay,
    /// Appliquer tout de suite, et rester ouvert (on glisse encore).
    Live(T),
    /// Appliquer et refermer.
    Pick(T),
    /// Refermer sans rien changer.
    Close,
}

/// Rectangle `(x, y, largeur, hauteur)`.
type Rect = (i32, i32, i32, i32);

fn inside(r: Rect, x: i32, y: i32) -> bool {
    x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
}

/// Polices choisies récemment, de la plus récente à la plus ancienne. Elles
/// survivent à la sortie du mode : on retrouve ses polices en y revenant.
static RECENT_FONTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Couleurs choisies récemment.
static RECENT_COLORS: Mutex<Vec<[u8; 3]>> = Mutex::new(Vec::new());

/// Retient une police parmi les récentes.
pub fn remember_font(name: &str) {
    if let Ok(mut recent) = RECENT_FONTS.lock() {
        recent.retain(|n| n != name);
        recent.insert(0, name.to_string());
        recent.truncate(5);
    }
}

/// Retient une couleur parmi les récentes.
pub fn remember_color(rgb: [f64; 3]) {
    let bytes = to_bytes(rgb);
    if let Ok(mut recent) = RECENT_COLORS.lock() {
        recent.retain(|c| *c != bytes);
        recent.insert(0, bytes);
        recent.truncate(10);
    }
}

// ---------------------------------------------------------------------------
// La liste des polices
// ---------------------------------------------------------------------------

/// Une ligne de la liste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// « Police du texte » : rendre au bloc la sienne.
    Keep,
    /// Un intertitre.
    Heading(&'static str),
    /// Une famille, par son rang dans le catalogue.
    Family(usize),
}

/// Le nom d'une famille, dessiné dans sa police.
struct Preview {
    w: u32,
    h: u32,
    coverage: Vec<u8>,
}

/// La liste déroulante des polices.
pub struct FontPicker {
    query: TextInput,
    rows: Vec<Row>,
    /// Défilement de la liste, en pixels.
    scroll: f64,
    /// Ligne mise en avant au clavier ou à la souris.
    highlight: Option<usize>,
    /// Famille du bloc, marquée dans la liste.
    current: Option<usize>,
    previews: HashMap<usize, Option<Preview>>,
    /// La carte et la fenêtre de la liste, relevées au dessin.
    card: Rect,
    view: Rect,
    row_h: i32,
    /// Des noms restent à dessiner dans leur police : il faut repeindre.
    pending: bool,
}

impl FontPicker {
    /// Liste neuve, la famille du bloc marquée.
    #[must_use]
    pub fn new(current: Option<usize>) -> Self {
        let mut query = TextInput::new("Rechercher une police");
        query.focused = true;
        let mut picker = FontPicker {
            query,
            rows: Vec::new(),
            scroll: 0.0,
            highlight: None,
            current,
            previews: HashMap::new(),
            card: (0, 0, 0, 0),
            view: (0, 0, 0, 0),
            row_h: 1,
            pending: false,
        };
        picker.rebuild();
        picker
    }

    /// Vrai s'il reste des noms à dessiner : la fenêtre doit se repeindre.
    #[must_use]
    pub fn pending(&self) -> bool {
        self.pending
    }

    /// Refait la liste des lignes d'après la recherche.
    fn rebuild(&mut self) {
        let all = sysfonts::families();
        let needle = self.query.value.trim().to_lowercase();
        self.rows.clear();
        if needle.is_empty() {
            self.rows.push(Row::Keep);
            let recent: Vec<usize> = RECENT_FONTS
                .lock()
                .map(|r| {
                    r.iter()
                        .filter_map(|name| all.iter().position(|f| &f.name == name))
                        .collect()
                })
                .unwrap_or_default();
            if !recent.is_empty() {
                self.rows.push(Row::Heading("Récentes"));
                self.rows.extend(recent.into_iter().map(Row::Family));
            }
            self.rows.push(Row::Heading("Toutes les polices"));
            self.rows.extend((0..all.len()).map(Row::Family));
        } else {
            // Ce qui commence par la recherche d'abord, ce qui la contient
            // ensuite : « ari » donne Arial avant Calibri.
            let (mut starts, mut contains) = (Vec::new(), Vec::new());
            for (i, family) in all.iter().enumerate() {
                let name = family.name.to_lowercase();
                if name.starts_with(&needle) {
                    starts.push(Row::Family(i));
                } else if name.contains(&needle) {
                    contains.push(Row::Family(i));
                }
            }
            self.rows.extend(starts);
            self.rows.extend(contains);
        }
        self.scroll = 0.0;
        self.highlight = self.rows.iter().position(|r| matches!(r, Row::Family(_)));
    }

    /// Hauteur totale du contenu.
    fn content(&self) -> i32 {
        self.rows.len() as i32 * self.row_h
    }

    fn clamp_scroll(&mut self) {
        let max = f64::from((self.content() - self.view.3).max(0));
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// Fait venir la ligne mise en avant dans la fenêtre.
    fn reveal(&mut self) {
        let Some(i) = self.highlight else { return };
        let top = f64::from(i as i32 * self.row_h);
        let bottom = top + f64::from(self.row_h);
        if top < self.scroll {
            self.scroll = top;
        } else if bottom > self.scroll + f64::from(self.view.3) {
            self.scroll = bottom - f64::from(self.view.3);
        }
        self.clamp_scroll();
    }

    /// Ligne sous un point de la liste.
    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        if !inside(self.view, x, y) {
            return None;
        }
        let offset = f64::from(y - self.view.1) + self.scroll;
        let index = (offset / f64::from(self.row_h.max(1))) as usize;
        (index < self.rows.len()).then_some(index)
    }

    fn choice(&self, index: usize) -> Outcome<Option<usize>> {
        match self.rows.get(index) {
            Some(Row::Keep) => Outcome::Pick(None),
            Some(Row::Family(i)) => Outcome::Pick(Some(*i)),
            _ => Outcome::Stay,
        }
    }

    /// Clic : une ligne choisit, le champ garde la main, ailleurs referme.
    pub fn mouse_down(&mut self, x: i32, y: i32) -> Outcome<Option<usize>> {
        if !inside(self.card, x, y) {
            return Outcome::Close;
        }
        match self.row_at(x, y) {
            Some(index) => self.choice(index),
            None => Outcome::Stay,
        }
    }

    /// Survol ; vrai si l'aspect a changé.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let over = self
            .row_at(x, y)
            .filter(|i| !matches!(self.rows.get(*i), Some(Row::Heading(_))));
        if over.is_some() && over != self.highlight {
            self.highlight = over;
            return true;
        }
        false
    }

    /// La molette fait défiler la liste.
    pub fn wheel(&mut self, delta: f64) {
        self.scroll -= delta * f64::from(self.row_h) * 3.0;
        self.clamp_scroll();
    }

    /// Déplace la mise en avant d'une ligne choisissable à l'autre.
    fn step(&mut self, by: i32) {
        let count = self.rows.len() as i32;
        if count == 0 {
            return;
        }
        let mut i = self.highlight.map_or(-1, |h| h as i32);
        for _ in 0..count {
            i = (i + by).clamp(0, count - 1);
            if !matches!(self.rows.get(i as usize), Some(Row::Heading(_))) {
                self.highlight = Some(i as usize);
                break;
            }
            if i == 0 || i == count - 1 {
                break;
            }
        }
        self.reveal();
    }

    /// Touche : flèches, pages, Entrée, Échap ; le reste va au champ.
    pub fn key(&mut self, key: Key) -> Outcome<Option<usize>> {
        let page = (self.view.3 / self.row_h.max(1)).max(1);
        match key {
            Key::Escape => return Outcome::Close,
            Key::Down => self.step(1),
            Key::Up => self.step(-1),
            Key::PageDown => self.step(page),
            Key::PageUp => self.step(-page),
            Key::Enter => {
                if let Some(index) = self.highlight {
                    return self.choice(index);
                }
            }
            other => {
                if self.query.key(other, false) == InputAction::Changed {
                    self.rebuild();
                }
            }
        }
        Outcome::Stay
    }

    /// Caractère tapé : il filtre la liste.
    pub fn char(&mut self, c: char) {
        if self.query.insert_char(c) == InputAction::Changed {
            self.rebuild();
        }
    }

    /// Dessine la liste sous son bouton.
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f32,
        anchor: Rect,
    ) {
        let s = |v: f32| (v * dpi).round() as i32;
        let size = theme.font_size * dpi;
        let pad = s(10.0);
        let field = s(32.0);
        self.row_h = s(34.0);
        let width = s(340.0).max(anchor.2);
        let room = frame.height as i32 - (anchor.1 + anchor.3) - s(24.0);
        let wanted = (self.content()).min(self.row_h * 11);
        let list_h = wanted.min(room - pad * 3 - field).max(self.row_h * 2);
        let height = pad * 3 + field + list_h;
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
        self.query.draw(
            frame,
            text,
            theme,
            dpi,
            x + pad,
            y + pad,
            width - 2 * pad,
            field,
        );

        self.view = (x + pad / 2, y + pad * 2 + field, width - pad, list_h);
        self.clamp_scroll();
        let (vx, vy, vw, vh) = self.view;
        let first = (self.scroll / f64::from(self.row_h)) as usize;
        let end = ((self.scroll + f64::from(vh)) / f64::from(self.row_h)) as usize + 1;
        let all = sysfonts::families();
        // Les noms des lignes visibles, dessinés dans leur police : quelques
        // fichiers par image, pas plus, pour que l'ouverture reste vive.
        let preview_h = self.row_h as u32;
        let preview_w = (vw - s(44.0)).max(8) as u32;
        let mut budget = 6;
        self.pending = false;
        for row in self.rows.iter().take(end.min(self.rows.len())).skip(first) {
            let Row::Family(i) = row else { continue };
            if self.previews.contains_key(i) {
                continue;
            }
            if budget == 0 {
                self.pending = true;
                break;
            }
            budget -= 1;
            let preview = all
                .get(*i)
                .and_then(|f| draw_name(f, raster, size * 1.12, preview_w, preview_h));
            self.previews.insert(*i, preview);
        }

        // La liste se dessine dans sa fenêtre : ce qui dépasse est rogné.
        let mut list = frame.sub(vx, vy, vw as u32, vh as u32);
        for index in first..end.min(self.rows.len()) {
            let ry = index as i32 * self.row_h - self.scroll as i32;
            let row = self.rows[index];
            if let Row::Heading(label) = row {
                text.draw(
                    &mut list,
                    s(12.0) as f32,
                    (ry + self.row_h / 2) as f32 + text.ascent(size * 0.86) / 2.0 + dpi,
                    size * 0.86,
                    label,
                    theme.text_dim,
                );
                continue;
            }
            if self.highlight == Some(index) {
                round_rect(
                    &mut list,
                    s(4.0),
                    ry + 1,
                    vw - s(14.0),
                    self.row_h - 2,
                    7.0 * dpi,
                    theme.hover,
                );
            }
            let family = match row {
                Row::Family(i) => Some(i),
                _ => None,
            };
            if family == self.current {
                let dot = s(7.0);
                round_rect(
                    &mut list,
                    s(14.0),
                    ry + (self.row_h - dot) / 2,
                    dot,
                    dot,
                    dot as f32 / 2.0,
                    theme.accent,
                );
            }
            let label_x = s(32.0);
            let drawn = family
                .and_then(|i| self.previews.get(&i))
                .and_then(Option::as_ref);
            match (drawn, family.and_then(|i| all.get(i))) {
                (Some(preview), _) => blit_coverage(&mut list, label_x, ry, preview, theme.text),
                (None, Some(f)) => {
                    text.draw_clipped(
                        &mut list,
                        label_x as f32,
                        (ry + self.row_h / 2) as f32 + text.ascent(size) / 2.0,
                        size,
                        &f.name,
                        theme.text,
                        (vw - label_x - s(16.0)) as f32,
                    );
                }
                (None, None) => {
                    text.draw(
                        &mut list,
                        label_x as f32,
                        (ry + self.row_h / 2) as f32 + text.ascent(size) / 2.0,
                        size,
                        "Police du texte",
                        theme.text,
                    );
                }
            }
        }
        // L'ascenseur : il dit où l'on est dans trois cents polices.
        let content = self.content();
        if content > vh {
            let track = vh - s(8.0);
            let thumb = (track * vh / content).max(s(28.0));
            let top =
                s(4.0) + (f64::from(track - thumb) * self.scroll / f64::from(content - vh)) as i32;
            round_rect(
                &mut list,
                vw - s(8.0),
                top,
                s(4.0),
                thumb,
                2.0 * dpi,
                theme.separator,
            );
        }
        if self.rows.is_empty() {
            text.draw(
                frame,
                (x + pad + s(8.0)) as f32,
                (vy + self.row_h / 2) as f32 + text.ascent(size) / 2.0,
                size,
                "Aucune police de ce nom",
                theme.text_dim,
            );
        }
    }
}

/// Dessine le nom d'une famille dans sa propre police.
///
/// Rend `None` si la police ne sait pas écrire son nom — une police de
/// symboles : la liste l'écrit alors dans la police de l'interface, sans quoi
/// on lirait une ligne de pictogrammes.
fn draw_name(family: &Family, raster: &mut Rasterizer, px: f32, w: u32, h: u32) -> Option<Preview> {
    let font = sysfonts::load(family.face(false, false)?)?;
    let upem = f64::from(font.units_per_em().max(1));
    let mut path = Path::new();
    let mut pen = 0.0f64;
    for c in family.name.chars() {
        let gid = font.unicode_to_gid(c).filter(|g| *g != 0)?;
        if let Some(glyph) = font.glyph_path(gid) {
            path.append(&glyph.transform(&Matrix::new(1.0, 0.0, 0.0, 1.0, pen, 0.0)));
        }
        pen += f64::from(font.advance(gid).unwrap_or(0));
    }
    let scale = f64::from(px) / upem;
    // Les contours ont l'axe des y vers le haut : on le retourne, la ligne de
    // base un peu sous le milieu de la ligne.
    let baseline = f64::from(h) / 2.0 + f64::from(px) * 0.34;
    let m = Matrix::new(scale, 0.0, 0.0, -scale, 0.0, baseline);
    let mask = raster.path_coverage(&path, &m, FillRule::NonZero, w, h);
    Some(Preview {
        w,
        h,
        coverage: mask.data().to_vec(),
    })
}

/// Pose une image de couverture, teintée, sur le cadre.
fn blit_coverage(frame: &mut Frame<'_>, x: i32, y: i32, preview: &Preview, ink: Rgb) {
    for row in 0..preview.h as i32 {
        let dy = y + row;
        if dy < 0 || dy >= frame.height as i32 {
            continue;
        }
        for col in 0..preview.w as i32 {
            let dx = x + col;
            if dx < 0 || dx >= frame.width as i32 {
                continue;
            }
            let a = u32::from(preview.coverage[(row as u32 * preview.w + col as u32) as usize]);
            if a == 0 {
                continue;
            }
            let i = frame.index(dx as usize, dy as usize);
            let inv = 255 - a;
            let d = &mut frame.pixels[i..i + 4];
            d[0] = ((u32::from(ink.2) * a + u32::from(d[0]) * inv) / 255) as u8;
            d[1] = ((u32::from(ink.1) * a + u32::from(d[1]) * inv) / 255) as u8;
            d[2] = ((u32::from(ink.0) * a + u32::from(d[2]) * inv) / 255) as u8;
        }
    }
}

// ---------------------------------------------------------------------------
// Le nuancier
// ---------------------------------------------------------------------------

/// Les dix couleurs du thème, en tête de colonne.
const THEME_COLORS: [[u8; 3]; 10] = [
    [0xFF, 0xFF, 0xFF],
    [0x00, 0x00, 0x00],
    [0x44, 0x54, 0x6A],
    [0x1F, 0x4E, 0x99],
    [0x2E, 0x86, 0xC1],
    [0x17, 0xA5, 0x89],
    [0x3D, 0x9A, 0x40],
    [0xE6, 0x9A, 0x17],
    [0xD3, 0x54, 0x00],
    [0xC0, 0x39, 0x2B],
];

/// Les couleurs vives, celles qu'on veut d'un clic.
const VIVID_COLORS: [[u8; 3]; 10] = [
    [0xC0, 0x00, 0x00],
    [0xFF, 0x00, 0x00],
    [0xFF, 0x8C, 0x00],
    [0xFF, 0xD4, 0x00],
    [0x92, 0xD0, 0x50],
    [0x00, 0xA8, 0x50],
    [0x00, 0xB0, 0xF0],
    [0x00, 0x70, 0xC0],
    [0x00, 0x20, 0x60],
    [0x70, 0x30, 0xA0],
];

/// Teintes d'une couleur du thème : elle-même, trois plus claires, deux plus
/// foncées. Le blanc et le noir donnent une échelle de gris.
fn shades(base: [u8; 3]) -> [[u8; 3]; 6] {
    let mix = |to: u8, t: f32| {
        let blend = |c: u8| (f32::from(c) + (f32::from(to) - f32::from(c)) * t).round() as u8;
        [blend(base[0]), blend(base[1]), blend(base[2])]
    };
    if base == [0xFF, 0xFF, 0xFF] {
        return [
            base,
            mix(0, 0.05),
            mix(0, 0.15),
            mix(0, 0.25),
            mix(0, 0.35),
            mix(0, 0.5),
        ];
    }
    if base == [0, 0, 0] {
        return [
            base,
            mix(255, 0.5),
            mix(255, 0.35),
            mix(255, 0.25),
            mix(255, 0.15),
            mix(255, 0.05),
        ];
    }
    [
        base,
        mix(255, 0.8),
        mix(255, 0.6),
        mix(255, 0.4),
        mix(0, 0.25),
        mix(0, 0.5),
    ]
}

fn to_bytes(rgb: [f64; 3]) -> [u8; 3] {
    let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [byte(rgb[0]), byte(rgb[1]), byte(rgb[2])]
}

fn to_unit(rgb: [u8; 3]) -> [f64; 3] {
    [
        f64::from(rgb[0]) / 255.0,
        f64::from(rgb[1]) / 255.0,
        f64::from(rgb[2]) / 255.0,
    ]
}

/// Teinte (0–360), saturation et luminosité (0–1) d'une couleur.
fn to_hsv(rgb: [u8; 3]) -> (f32, f32, f32) {
    let (r, g, b) = (
        f32::from(rgb[0]) / 255.0,
        f32::from(rgb[1]) / 255.0,
        f32::from(rgb[2]) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let hue = if delta <= f32::EPSILON {
        0.0
    } else if (max - r).abs() <= f32::EPSILON {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if (max - g).abs() <= f32::EPSILON {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let sat = if max <= f32::EPSILON {
        0.0
    } else {
        delta / max
    };
    (hue, sat, max)
}

/// Couleur d'une teinte, d'une saturation et d'une luminosité.
fn from_hsv(hue: f32, sat: f32, val: f32) -> [u8; 3] {
    let c = val * sat;
    let sector = (hue.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match sector as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = val - c;
    let byte = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    [byte(r), byte(g), byte(b)]
}

/// Lit « #1F4E99 », « 1f4e99 » ou « #abc ».
fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let hex = text.trim().trim_start_matches('#');
    let nibble = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let bytes = hex.as_bytes();
    match bytes.len() {
        6 => Some([
            nibble(bytes[0])? * 16 + nibble(bytes[1])?,
            nibble(bytes[2])? * 16 + nibble(bytes[3])?,
            nibble(bytes[4])? * 16 + nibble(bytes[5])?,
        ]),
        3 => Some([
            nibble(bytes[0])? * 17,
            nibble(bytes[1])? * 17,
            nibble(bytes[2])? * 17,
        ]),
        _ => None,
    }
}

/// Ce qu'on glisse dans le nuancier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drag {
    Square,
    Hue,
}

/// Le nuancier.
pub struct ColorPicker {
    hue: f32,
    sat: f32,
    val: f32,
    hex: TextInput,
    drag: Option<Drag>,
    /// Pastille survolée, rang dans `cells`.
    hover: Option<usize>,
    /// Le pointeur est sur « OK ».
    hover_ok: bool,
    card: Rect,
    /// Marge de clic autour d'une pastille : la moitié de l'interstice.
    slack: i32,
    cells: Vec<(Rect, [u8; 3])>,
    square: Rect,
    strip: Rect,
    field: Rect,
    ok: Rect,
}

impl ColorPicker {
    /// Nuancier neuf, réglé sur la couleur du bloc.
    #[must_use]
    pub fn new(current: [f64; 3]) -> Self {
        let bytes = to_bytes(current);
        let (hue, sat, val) = to_hsv(bytes);
        let mut hex = TextInput::new("RRVVBB");
        hex.set_value(&format!("{:02X}{:02X}{:02X}", bytes[0], bytes[1], bytes[2]));
        ColorPicker {
            hue,
            sat,
            val,
            hex,
            drag: None,
            hover: None,
            hover_ok: false,
            card: (0, 0, 0, 0),
            slack: 0,
            cells: Vec::new(),
            square: (0, 0, 0, 0),
            strip: (0, 0, 0, 0),
            field: (0, 0, 0, 0),
            ok: (0, 0, 0, 0),
        }
    }

    fn rgb(&self) -> [u8; 3] {
        from_hsv(self.hue, self.sat, self.val)
    }

    fn sync_hex(&mut self) {
        let c = self.rgb();
        self.hex
            .set_value(&format!("{:02X}{:02X}{:02X}", c[0], c[1], c[2]));
    }

    /// Règle le carré ou la réglette d'après le pointeur.
    fn track(&mut self, drag: Drag, x: i32, y: i32) {
        match drag {
            Drag::Square => {
                let (sx, sy, sw, sh) = self.square;
                self.sat = ((x - sx) as f32 / (sw - 1).max(1) as f32).clamp(0.0, 1.0);
                self.val = 1.0 - ((y - sy) as f32 / (sh - 1).max(1) as f32).clamp(0.0, 1.0);
            }
            Drag::Hue => {
                let (hx, _, hw, _) = self.strip;
                self.hue = ((x - hx) as f32 / (hw - 1).max(1) as f32).clamp(0.0, 1.0) * 359.99;
            }
        }
        self.sync_hex();
    }

    /// Clic : une pastille choisit, le carré et la réglette règlent en
    /// direct, « OK » valide, ailleurs referme.
    pub fn mouse_down(&mut self, x: i32, y: i32) -> Outcome<[f64; 3]> {
        if !inside(self.card, x, y) {
            return Outcome::Close;
        }
        // Les interstices entre pastilles ne sont pas des trous : chaque
        // pastille répond un peu au-delà de son bord.
        let slack = self.slack;
        let near = |r: Rect| {
            inside(
                (r.0 - slack, r.1 - slack, r.2 + 2 * slack, r.3 + 2 * slack),
                x,
                y,
            )
        };
        if let Some((_, color)) = self.cells.iter().find(|(r, _)| near(*r)) {
            return Outcome::Pick(to_unit(*color));
        }
        self.hex.focused = inside(self.field, x, y);
        for (rect, drag) in [(self.square, Drag::Square), (self.strip, Drag::Hue)] {
            if inside(rect, x, y) {
                self.drag = Some(drag);
                self.track(drag, x, y);
                return Outcome::Live(to_unit(self.rgb()));
            }
        }
        if inside(self.ok, x, y) {
            return Outcome::Pick(to_unit(self.rgb()));
        }
        Outcome::Stay
    }

    /// Déplacement : on glisse dans le carré ou la réglette, ou l'on survole.
    pub fn mouse_move(&mut self, x: i32, y: i32, dragging: bool) -> Outcome<[f64; 3]> {
        if let (Some(drag), true) = (self.drag, dragging) {
            self.track(drag, x, y);
            return Outcome::Live(to_unit(self.rgb()));
        }
        self.hover = self.cells.iter().position(|(r, _)| inside(*r, x, y));
        self.hover_ok = inside(self.ok, x, y);
        Outcome::Stay
    }

    /// Relâchement : le glissement est fini.
    pub fn mouse_up(&mut self) {
        self.drag = None;
    }

    /// Touche : Échap referme, Entrée valide le code tapé.
    pub fn key(&mut self, key: Key) -> Outcome<[f64; 3]> {
        match key {
            Key::Escape => Outcome::Close,
            Key::Enter => match parse_hex(&self.hex.value) {
                Some(color) => Outcome::Pick(to_unit(color)),
                None => Outcome::Pick(to_unit(self.rgb())),
            },
            other => {
                if self.hex.key(other, false) == InputAction::Changed {
                    return self.typed();
                }
                Outcome::Stay
            }
        }
    }

    /// Caractère tapé dans le code hexadécimal.
    pub fn char(&mut self, c: char) -> Outcome<[f64; 3]> {
        if !self.hex.focused || !(c.is_ascii_hexdigit() || c == '#') {
            return Outcome::Stay;
        }
        if self.hex.insert_char(c) == InputAction::Changed {
            return self.typed();
        }
        Outcome::Stay
    }

    /// Le code a changé : s'il se lit, le nuancier le suit en direct.
    fn typed(&mut self) -> Outcome<[f64; 3]> {
        match parse_hex(&self.hex.value) {
            Some(color) if self.hex.value.trim_start_matches('#').len() == 6 => {
                (self.hue, self.sat, self.val) = to_hsv(color);
                Outcome::Live(to_unit(color))
            }
            _ => Outcome::Stay,
        }
    }

    /// Dessine le nuancier sous son bouton.
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
        anchor: Rect,
        current: [f64; 3],
    ) {
        let s = |v: f32| (v * dpi).round() as i32;
        let size = theme.font_size * dpi;
        let pad = s(14.0);
        let cell = s(22.0);
        let gap = s(4.0);
        let grid_w = cell * 10 + gap * 9;
        self.slack = gap / 2;
        let width = grid_w + pad * 2;
        let recent: Vec<[u8; 3]> = RECENT_COLORS.lock().map(|r| r.clone()).unwrap_or_default();
        let heading = s(22.0);
        let square_h = s(120.0);
        let strip_h = s(14.0);
        let field_h = s(32.0);
        let recent_h = if recent.is_empty() {
            0
        } else {
            heading + cell + s(8.0)
        };
        let height = pad
            + heading
            + cell * 6
            + gap * 5
            + s(10.0)
            + heading
            + cell
            + s(8.0)
            + recent_h
            + heading
            + square_h
            + s(8.0)
            + strip_h
            + s(12.0)
            + field_h
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

        let chosen = to_bytes(current);
        self.cells.clear();
        let gx = x + pad;
        let mut cy = y + pad;
        let title = |frame: &mut Frame<'_>, text: &mut TextRenderer, label: &str, at: i32| {
            text.draw(
                frame,
                gx as f32,
                at as f32 + text.ascent(size * 0.86) + dpi,
                size * 0.86,
                label,
                theme.text_dim,
            );
        };

        // Les couleurs du thème, et leurs teintes.
        title(frame, text, "Couleurs du thème", cy);
        cy += heading;
        for (col, base) in THEME_COLORS.iter().enumerate() {
            for (row, color) in shades(*base).iter().enumerate() {
                let rect = (
                    gx + col as i32 * (cell + gap),
                    cy + row as i32 * (cell + gap),
                    cell,
                    cell,
                );
                self.cells.push((rect, *color));
            }
        }
        cy += cell * 6 + gap * 5 + s(10.0);
        // Les couleurs vives.
        title(frame, text, "Couleurs vives", cy);
        cy += heading;
        for (col, color) in VIVID_COLORS.iter().enumerate() {
            self.cells
                .push(((gx + col as i32 * (cell + gap), cy, cell, cell), *color));
        }
        cy += cell + s(8.0);
        // Les récentes.
        if !recent.is_empty() {
            title(frame, text, "Récentes", cy);
            cy += heading;
            for (col, color) in recent.iter().take(10).enumerate() {
                self.cells
                    .push(((gx + col as i32 * (cell + gap), cy, cell, cell), *color));
            }
            cy += cell + s(8.0);
        }
        for (index, (rect, color)) in self.cells.iter().enumerate() {
            let (rx, ry, rw, rh) = *rect;
            let ring = s(2.0);
            if *color == chosen {
                round_rect(
                    frame,
                    rx - ring,
                    ry - ring,
                    rw + 2 * ring,
                    rh + 2 * ring,
                    6.0 * dpi,
                    theme.accent,
                );
            } else if self.hover == Some(index) {
                round_rect(
                    frame,
                    rx - ring,
                    ry - ring,
                    rw + 2 * ring,
                    rh + 2 * ring,
                    6.0 * dpi,
                    theme.text_dim,
                );
            }
            round_rect(
                frame,
                rx,
                ry,
                rw,
                rh,
                4.0 * dpi,
                (color[0], color[1], color[2]),
            );
            // Une pastille presque de la couleur du fond se perdrait : un
            // liseré la détache.
            let near = |a: u8, b: u8| a.abs_diff(b) < 28;
            if near(color[0], theme.bar.0)
                && near(color[1], theme.bar.1)
                && near(color[2], theme.bar.2)
                || *color == [0xFF, 0xFF, 0xFF]
            {
                round_rect_outline(
                    frame,
                    rx,
                    ry,
                    rw,
                    rh,
                    4.0 * dpi,
                    dpi.max(1.0),
                    theme.separator,
                );
            }
        }

        // La couleur sur mesure : carré, réglette, code.
        title(frame, text, "Personnalisée", cy);
        cy += heading;
        self.square = (gx, cy, grid_w, square_h);
        for row in 0..square_h {
            let val = 1.0 - row as f32 / (square_h - 1).max(1) as f32;
            for col in 0..grid_w {
                let sat = col as f32 / (grid_w - 1).max(1) as f32;
                let c = from_hsv(self.hue, sat, val);
                frame.fill_rect(gx + col, cy + row, 1, 1, c[0], c[1], c[2]);
            }
        }
        round_rect_outline(
            frame,
            gx,
            cy,
            grid_w,
            square_h,
            2.0 * dpi,
            dpi.max(1.0),
            theme.separator,
        );
        // Le repère : un anneau blanc cerné de noir, lisible sur toute couleur.
        let mx = gx + (self.sat * (grid_w - 1) as f32) as i32;
        let my = cy + ((1.0 - self.val) * (square_h - 1) as f32) as i32;
        let knob = s(7.0);
        round_rect_outline(
            frame,
            mx - knob,
            my - knob,
            knob * 2,
            knob * 2,
            knob as f32,
            3.0 * dpi,
            (0, 0, 0),
        );
        round_rect_outline(
            frame,
            mx - knob,
            my - knob,
            knob * 2,
            knob * 2,
            knob as f32,
            1.5 * dpi,
            (255, 255, 255),
        );
        cy += square_h + s(8.0);

        self.strip = (gx, cy, grid_w, strip_h);
        for col in 0..grid_w {
            let hue = col as f32 / (grid_w - 1).max(1) as f32 * 359.99;
            let c = from_hsv(hue, 1.0, 1.0);
            frame.fill_rect(gx + col, cy, 1, strip_h, c[0], c[1], c[2]);
        }
        let hx = gx + (self.hue / 359.99 * (grid_w - 1) as f32) as i32;
        round_rect(
            frame,
            hx - s(3.0),
            cy - s(3.0),
            s(6.0),
            strip_h + s(6.0),
            3.0 * dpi,
            (255, 255, 255),
        );
        round_rect_outline(
            frame,
            hx - s(3.0),
            cy - s(3.0),
            s(6.0),
            strip_h + s(6.0),
            3.0 * dpi,
            dpi.max(1.0),
            (0, 0, 0),
        );
        cy += strip_h + s(12.0);

        // La pastille de la couleur réglée, son code, et « OK ».
        let custom = self.rgb();
        round_rect(
            frame,
            gx,
            cy,
            field_h,
            field_h,
            6.0 * dpi,
            (custom[0], custom[1], custom[2]),
        );
        round_rect_outline(
            frame,
            gx,
            cy,
            field_h,
            field_h,
            6.0 * dpi,
            dpi.max(1.0),
            theme.separator,
        );
        text.draw(
            frame,
            (gx + field_h + s(10.0)) as f32,
            (cy + field_h / 2) as f32 + text.ascent(size) / 2.0,
            size,
            "#",
            theme.text_dim,
        );
        let ok_w = s(64.0);
        self.field = (
            gx + field_h + s(24.0),
            cy,
            grid_w - field_h - s(24.0) - ok_w - s(8.0),
            field_h,
        );
        self.hex.draw(
            frame,
            text,
            theme,
            dpi,
            self.field.0,
            self.field.1,
            self.field.2,
            self.field.3,
        );
        self.ok = (gx + grid_w - ok_w, cy, ok_w, field_h);
        let ink = button(
            frame,
            self.ok.0,
            self.ok.1,
            ok_w,
            field_h,
            dpi,
            theme,
            ButtonLook {
                primary: true,
                hovered: self.hover_ok,
                focused: false,
                disabled: false,
            },
        );
        let lw = text.measure(size, "OK");
        text.draw(
            frame,
            self.ok.0 as f32 + (ok_w as f32 - lw) / 2.0,
            (cy + field_h / 2) as f32 + text.ascent(size) / 2.0,
            size,
            "OK",
            ink,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Une couleur passée en teinte-saturation-luminosité et ramenée reste la
    /// même : le carré montre bien la couleur qu'on lui a donnée.
    #[test]
    fn la_conversion_de_couleur_fait_laller_retour() {
        for color in [
            [0x1F, 0x4E, 0x99],
            [0xC0, 0x39, 0x2B],
            [0, 0, 0],
            [255, 255, 255],
            [18, 200, 77],
        ] {
            let (h, s, v) = to_hsv(color);
            let back = from_hsv(h, s, v);
            for i in 0..3 {
                assert!(
                    color[i].abs_diff(back[i]) <= 1,
                    "{color:?} devient {back:?}"
                );
            }
        }
    }

    #[test]
    fn le_code_hexadecimal_se_lit_sous_ses_formes_courantes() {
        assert_eq!(parse_hex("#1F4E99"), Some([0x1F, 0x4E, 0x99]));
        assert_eq!(parse_hex("1f4e99"), Some([0x1F, 0x4E, 0x99]));
        assert_eq!(parse_hex("#abc"), Some([0xAA, 0xBB, 0xCC]));
        assert_eq!(parse_hex("#12"), None);
        assert_eq!(parse_hex("zzzzzz"), None);
    }

    /// Chaque couleur du thème a six teintes distinctes, du clair au foncé.
    #[test]
    fn chaque_couleur_du_theme_a_six_teintes() {
        for base in THEME_COLORS {
            let all = shades(base);
            for (i, a) in all.iter().enumerate() {
                for b in &all[i + 1..] {
                    assert_ne!(a, b, "teintes de {base:?}");
                }
            }
        }
    }

    /// La recherche met en tête ce qui **commence** par le texte tapé.
    #[test]
    fn la_recherche_met_les_debuts_en_tete() {
        let mut picker = FontPicker::new(None);
        let Some(first) = sysfonts::families().first() else {
            return;
        };
        let needle: String = first.name.chars().take(3).collect();
        for c in needle.chars() {
            picker.char(c);
        }
        let top = match picker.rows.first() {
            Some(Row::Family(top)) => *top,
            other => {
                assert!(other.is_some(), "la recherche « {needle} » n'a rien trouvé");
                return;
            }
        };
        assert!(sysfonts::families()[top]
            .name
            .to_lowercase()
            .starts_with(&needle.to_lowercase()));
    }

    /// Toutes les polices de la machine se laissent dessiner : aucune ne fait
    /// tomber la liste, quelle que soit la bizarrerie de son fichier.
    #[test]
    fn toutes_les_polices_du_systeme_se_dessinent() {
        let mut raster = Rasterizer::new();
        for family in sysfonts::families() {
            let _ = draw_name(family, &mut raster, 22.0, 440, 51);
        }
    }

    /// Un clic hors de la carte referme sans rien choisir.
    #[test]
    fn un_clic_dehors_referme() {
        let mut fonts = FontPicker::new(None);
        fonts.card = (100, 100, 200, 300);
        assert_eq!(fonts.mouse_down(10, 10), Outcome::Close);
        let mut colors = ColorPicker::new([0.0, 0.0, 0.0]);
        colors.card = (100, 100, 200, 300);
        assert_eq!(colors.mouse_down(10, 10), Outcome::Close);
    }
}
