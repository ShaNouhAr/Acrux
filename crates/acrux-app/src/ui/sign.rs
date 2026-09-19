//! Outil « remplir et signer », celui de la barre d'Acrobat.
//!
//! Deux morceaux d'interface :
//!
//! 1. une **barre** sous la barre d'outils, qui liste ce qu'on peut poser :
//!    signature, paraphe, texte, et les cinq marques (coche, croix, rond,
//!    trait, point) ;
//! 2. une **fenêtre de capture** pour fabriquer sa signature, avec les trois
//!    façons de faire d'Acrobat : la tracer au pointeur, la taper, ou
//!    importer une image.
//!
//! Le dessin montré dans la barre et dans la fenêtre est **le même contour**
//! que celui qui ira dans le PDF : c'est
//! [`acrux_features::fillsign::ink`] qui le calcule dans les deux cas, si
//! bien que ce qu'on voit est exactement ce qu'on pose.
//!
//! La signature et le paraphe sont conservés entre deux sessions (voir
//! [`crate::ui::prefs`]) : on les dessine une fois, on s'en sert ensuite.

// Coordonnées d'écran entières et couvertures 8 bits.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    clippy::too_many_arguments,
    // Les fonctions de peinture sont des suites d'instructions de dessin :
    // les couper en morceaux rendrait la mise en page illisible.
    clippy::too_many_lines
)]

use std::fmt::Write as _;
use std::path::PathBuf;

use acrux_core::{Matrix, Path, Point};
use acrux_features::fillsign::ink::{self, InkPoint, Outline, Pen, Seg, Stroke};
use acrux_features::fillsign::marks::{outline_of, Mark};
use acrux_graphics::{FillRule, Rasterizer};

use crate::platform::{Frame, Key};
use crate::ui::input::{InputAction, TextInput};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Ce que l'outil s'apprête à poser au prochain clic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    /// La signature enregistrée.
    Signature,
    /// Le paraphe (les initiales).
    Initials,
    /// Du texte tapé au clavier.
    Text,
    /// Une marque.
    Mark(Mark),
}

impl Item {
    /// Libellé du bouton.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Item::Signature => "Signature",
            Item::Initials => "Paraphe",
            Item::Text => "Texte",
            Item::Mark(Mark::Check) => "Coche",
            Item::Mark(Mark::Cross) => "Croix",
            Item::Mark(Mark::Circle) => "Rond",
            Item::Mark(Mark::Line) => "Trait",
            Item::Mark(Mark::Dot) => "Point",
        }
    }

    /// Taille par défaut sur la page, en points PDF (largeur, hauteur).
    #[must_use]
    pub fn default_size(self) -> (f64, f64) {
        match self {
            Item::Signature => (170.0, 48.0),
            Item::Initials => (56.0, 40.0),
            Item::Text => (140.0, 15.0),
            Item::Mark(Mark::Line) => (90.0, 16.0),
            Item::Mark(_) => (18.0, 18.0),
        }
    }
}

/// Une signature enregistrée, quelle que soit la façon dont elle a été faite.
#[derive(Debug, Clone, PartialEq)]
pub enum Saved {
    /// Tracée au pointeur, dans un repère dont l'origine est en bas à gauche.
    Drawn(Vec<Stroke>),
    /// Tapée : le nom, qu'une police manuscrite écrira.
    Typed(String),
    /// Importée : le chemin du fichier image, relu au moment de poser.
    Image(PathBuf),
}

impl Saved {
    /// Forme texte, telle qu'elle est écrite dans les préférences.
    ///
    /// Une ligne lisible et corrigeable à la main, comme le reste du fichier :
    /// `drawn:12,40 18,52;30,10 …`, `typed:Élise Marchand`, `image:C:/…png`.
    #[must_use]
    pub fn encode(&self) -> String {
        match self {
            Saved::Drawn(strokes) => {
                let mut out = String::from("drawn:");
                for (index, stroke) in strokes.iter().enumerate() {
                    if index > 0 {
                        out.push(';');
                    }
                    for (n, point) in stroke.points.iter().enumerate() {
                        if n > 0 {
                            out.push(' ');
                        }
                        let _ = write!(out, "{:.1},{:.1}", point.x, point.y);
                    }
                }
                out
            }
            Saved::Typed(text) => format!("typed:{text}"),
            Saved::Image(path) => format!("image:{}", path.display()),
        }
    }

    /// Relit la forme texte ; `None` si elle est illisible.
    #[must_use]
    pub fn decode(text: &str) -> Option<Saved> {
        let (kind, rest) = text.split_once(':')?;
        match kind {
            "typed" if !rest.trim().is_empty() => Some(Saved::Typed(rest.trim().to_string())),
            "image" if !rest.trim().is_empty() => Some(Saved::Image(PathBuf::from(rest.trim()))),
            "drawn" => {
                let mut strokes = Vec::new();
                for part in rest.split(';') {
                    let mut points = Vec::new();
                    for pair in part.split_whitespace() {
                        let (x, y) = pair.split_once(',')?;
                        points.push(InkPoint::new(x.parse().ok()?, y.parse().ok()?));
                    }
                    if !points.is_empty() {
                        strokes.push(Stroke { points });
                    }
                }
                (!strokes.is_empty()).then_some(Saved::Drawn(strokes))
            }
            _ => None,
        }
    }
}

/// Onglet de la fenêtre de capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    /// Tracer au pointeur.
    #[default]
    Draw,
    /// Taper un nom.
    Type,
    /// Importer une image.
    Import,
}

/// Ce que l'outil demande à l'application de faire.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Rien.
    None,
    /// Redessiner seulement.
    Redraw,
    /// L'élément à poser a changé.
    Pick(Item),
    /// Ouvrir le sélecteur de fichier pour une image (paraphe si `true`).
    Import(bool),
    /// La capture est terminée : enregistrer cette signature (paraphe si `true`).
    Save(bool, Saved),
    /// Quitter l'outil.
    Close,
}

/// Bouton de la fenêtre de capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Button {
    Tab(Tab),
    Clear,
    Import,
    Apply,
    Cancel,
}

/// Fenêtre de capture d'une signature.
#[derive(Debug)]
pub struct Capture {
    /// Vrai si l'on capture le paraphe et non la signature.
    pub initials: bool,
    /// Onglet courant.
    pub tab: Tab,
    /// Traits déjà posés, en coordonnées de la zone de dessin (Y vers le bas).
    strokes: Vec<Stroke>,
    /// Trait en cours de tracé.
    current: Vec<InkPoint>,
    /// Nom tapé.
    name: TextInput,
    /// Image choisie.
    image: Option<PathBuf>,
    /// Zone de dessin à l'écran.
    canvas: (i32, i32, i32, i32),
    /// Boutons cliquables.
    buttons: Vec<(i32, i32, i32, i32, Button)>,
}

impl Capture {
    /// Ouvre une capture vierge.
    #[must_use]
    pub fn new(initials: bool) -> Self {
        Capture {
            initials,
            tab: Tab::Draw,
            strokes: Vec::new(),
            current: Vec::new(),
            name: TextInput::new("Votre nom"),
            image: None,
            canvas: (0, 0, 0, 0),
            buttons: Vec::new(),
        }
    }

    /// Titre de la fenêtre.
    #[must_use]
    pub fn title(&self) -> &'static str {
        if self.initials {
            "Votre paraphe"
        } else {
            "Votre signature"
        }
    }

    /// Le fichier image choisi par l'utilisateur.
    pub fn set_image(&mut self, path: PathBuf) {
        self.image = Some(path);
        self.tab = Tab::Import;
    }

    /// Ce que la capture produirait en l'état, ou `None` si elle est vide.
    #[must_use]
    pub fn result(&self) -> Option<Saved> {
        match self.tab {
            Tab::Draw => {
                let strokes = self.normalised();
                (!strokes.is_empty()).then_some(Saved::Drawn(strokes))
            }
            Tab::Type => {
                let text = self.name.value.trim().to_string();
                (!text.is_empty()).then_some(Saved::Typed(text))
            }
            Tab::Import => self.image.clone().map(Saved::Image),
        }
    }

    /// Les traits dans un repère PDF : origine en bas à gauche de la zone,
    /// et non en haut comme à l'écran.
    fn normalised(&self) -> Vec<Stroke> {
        let (_, cy, _, ch) = self.canvas;
        let flip = |p: &InkPoint| InkPoint {
            x: p.x,
            y: f64::from(ch) - (p.y - f64::from(cy)),
            pressure: p.pressure,
        };
        self.strokes
            .iter()
            .map(|s| Stroke {
                points: s.points.iter().map(flip).collect(),
            })
            .filter(|s: &Stroke| !s.points.is_empty())
            .collect()
    }

    /// Clic dans la fenêtre. Rend l'action et si le clic a été consommé.
    pub fn mouse_down(&mut self, x: i32, y: i32) -> (Action, bool) {
        for &(bx, by, bw, bh, button) in &self.buttons {
            if x >= bx && x < bx + bw && y >= by && y < by + bh {
                return (self.press(button), true);
            }
        }
        let (cx, cy, cw, ch) = self.canvas;
        if self.tab == Tab::Draw && x >= cx && x < cx + cw && y >= cy && y < cy + ch {
            self.current = vec![InkPoint::new(f64::from(x), f64::from(y))];
            return (Action::Redraw, true);
        }
        (Action::None, false)
    }

    /// Déplacement du pointeur, bouton enfoncé ou non.
    pub fn mouse_move(&mut self, x: i32, y: i32, dragging: bool) -> Action {
        if !dragging || self.current.is_empty() {
            return Action::None;
        }
        let (cx, cy, cw, ch) = self.canvas;
        // Le tracé reste dans la zone : un geste qui déborde est retenu au
        // bord plutôt qu'interrompu, comme sur un pavé tactile.
        let px = f64::from(x).clamp(f64::from(cx), f64::from(cx + cw));
        let py = f64::from(y).clamp(f64::from(cy), f64::from(cy + ch));
        self.current.push(InkPoint::new(px, py));
        Action::Redraw
    }

    /// Relâchement : le trait en cours est rangé.
    pub fn mouse_up(&mut self) -> Action {
        if self.current.is_empty() {
            return Action::None;
        }
        let points = std::mem::take(&mut self.current);
        self.strokes.push(Stroke { points });
        Action::Redraw
    }

    /// Touche pressée.
    pub fn key(&mut self, key: Key) -> Action {
        match key {
            Key::Escape => Action::Redraw,
            Key::Tab => {
                self.tab = match self.tab {
                    Tab::Draw => Tab::Type,
                    Tab::Type => Tab::Import,
                    Tab::Import => Tab::Draw,
                };
                Action::Redraw
            }
            _ if self.tab == Tab::Type => match self.name.key(key, false) {
                InputAction::Changed => Action::Redraw,
                InputAction::Submit => self.press(Button::Apply),
                InputAction::Cancel => Action::Close,
                InputAction::None => Action::None,
            },
            _ => Action::None,
        }
    }

    /// Caractère tapé.
    pub fn char(&mut self, c: char) -> Action {
        if self.tab == Tab::Type {
            self.name.insert_char(c);
            return Action::Redraw;
        }
        Action::None
    }

    /// Effet d'un bouton.
    fn press(&mut self, button: Button) -> Action {
        match button {
            Button::Tab(tab) => {
                self.tab = tab;
                Action::Redraw
            }
            Button::Clear => {
                self.strokes.clear();
                self.current.clear();
                self.name.set_value("");
                self.image = None;
                Action::Redraw
            }
            Button::Import => Action::Import(self.initials),
            Button::Apply => match self.result() {
                Some(saved) => Action::Save(self.initials, saved),
                None => Action::Redraw,
            },
            Button::Cancel => Action::Close,
        }
    }

    /// Dessine la fenêtre et mémorise les zones cliquables.
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f32,
    ) {
        let size = theme.font_size * dpi;
        let pad = (16.0 * dpi) as i32;
        let width = (520.0 * dpi).min(f32::from(frame.width as u16) * 0.92) as i32;
        let height = (360.0 * dpi).min(f32::from(frame.height as u16) * 0.92) as i32;
        let x = (frame.width as i32 - width) / 2;
        let y = (frame.height as i32 - height) / 2;
        self.buttons.clear();

        // Voile sombre : la fenêtre sort du fond sans qu'on ait d'ombre à
        // dessiner.
        veil(frame);
        frame.fill_rect(
            x - 1,
            y - 1,
            width + 2,
            height + 2,
            theme.separator.0,
            theme.separator.1,
            theme.separator.2,
        );
        frame.fill_rect(x, y, width, height, theme.bar.0, theme.bar.1, theme.bar.2);

        let mut cursor = y + pad;
        text.draw(
            frame,
            (x + pad) as f32,
            cursor as f32 + text.ascent(size * 1.15),
            size * 1.15,
            self.title(),
            theme.text,
        );
        cursor += (size * 1.9) as i32;

        // Onglets.
        let tab_h = (28.0 * dpi) as i32;
        let mut tx = x + pad;
        for (tab, label) in [
            (Tab::Draw, "Tracer"),
            (Tab::Type, "Taper"),
            (Tab::Import, "Importer"),
        ] {
            let w = (text.measure(size, label) + 24.0 * dpi) as i32;
            if tab == self.tab {
                frame.fill_rect(
                    tx,
                    cursor,
                    w,
                    tab_h,
                    theme.hover.0,
                    theme.hover.1,
                    theme.hover.2,
                );
                frame.fill_rect(
                    tx,
                    cursor + tab_h - (2.0 * dpi).max(1.0) as i32,
                    w,
                    (2.0 * dpi).max(1.0) as i32,
                    theme.accent.0,
                    theme.accent.1,
                    theme.accent.2,
                );
            }
            let color = if tab == self.tab {
                theme.text
            } else {
                theme.text_dim
            };
            text.draw(
                frame,
                (tx + (12.0 * dpi) as i32) as f32,
                cursor as f32 + f32::midpoint(tab_h as f32, text.ascent(size)) - 1.0,
                size,
                label,
                color,
            );
            self.buttons.push((tx, cursor, w, tab_h, Button::Tab(tab)));
            tx += w + (4.0 * dpi) as i32;
        }
        cursor += tab_h + (10.0 * dpi) as i32;

        let bottom = y + height - pad - (34.0 * dpi) as i32;
        let area = (x + pad, cursor, width - 2 * pad, bottom - cursor - pad);
        self.paint_body(frame, text, raster, theme, dpi, area);

        // Boutons du bas.
        let bh = (30.0 * dpi) as i32;
        let mut bx = x + width - pad;
        for (label, button) in [("Appliquer", Button::Apply), ("Annuler", Button::Cancel)] {
            let w = (text.measure(size, label) + 28.0 * dpi) as i32;
            bx -= w;
            let strong = button == Button::Apply;
            let (r, g, b) = if strong { theme.accent } else { theme.hover };
            frame.fill_rect(bx, bottom, w, bh, r, g, b);
            text.draw(
                frame,
                (bx + (14.0 * dpi) as i32) as f32,
                bottom as f32 + f32::midpoint(bh as f32, text.ascent(size)) - 1.0,
                size,
                label,
                if strong { (255, 255, 255) } else { theme.text },
            );
            self.buttons.push((bx, bottom, w, bh, button));
            bx -= (8.0 * dpi) as i32;
        }
        let label = "Effacer";
        let w = (text.measure(size, label) + 28.0 * dpi) as i32;
        frame.fill_rect(
            x + pad,
            bottom,
            w,
            bh,
            theme.hover.0,
            theme.hover.1,
            theme.hover.2,
        );
        text.draw(
            frame,
            (x + pad + (14.0 * dpi) as i32) as f32,
            bottom as f32 + f32::midpoint(bh as f32, text.ascent(size)) - 1.0,
            size,
            label,
            theme.text,
        );
        self.buttons.push((x + pad, bottom, w, bh, Button::Clear));
    }

    /// Contenu de l'onglet courant.
    fn paint_body(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f32,
        area: (i32, i32, i32, i32),
    ) {
        let (ax, ay, aw, ah) = area;
        let size = theme.font_size * dpi;
        match self.tab {
            Tab::Draw => {
                self.canvas = (ax, ay, aw, ah);
                frame.fill_rect(ax, ay, aw, ah, 0xFF, 0xFF, 0xFF);
                // Ligne de base, comme sur un bordereau : on sait où signer.
                let line = ay + (ah as f32 * 0.78) as i32;
                frame.fill_rect(
                    ax + (16.0 * dpi) as i32,
                    line,
                    aw - (32.0 * dpi) as i32,
                    1.max((dpi) as i32),
                    0xC8,
                    0xCC,
                    0xD4,
                );
                let mut strokes = self.strokes.clone();
                if !self.current.is_empty() {
                    strokes.push(Stroke {
                        points: self.current.clone(),
                    });
                }
                if strokes.is_empty() {
                    text.draw(
                        frame,
                        (ax + (18.0 * dpi) as i32) as f32,
                        line as f32 + text.ascent(size) + 6.0 * dpi,
                        size,
                        "Tracez votre signature ici, à la souris ou au doigt",
                        (0x9A, 0x9E, 0xA6),
                    );
                } else {
                    // Même plume qu'à la pose : l'aperçu n'est pas une
                    // approximation, c'est le dessin lui-même.
                    let pen = Pen::for_extent(f64::from(aw), f64::from(ah));
                    let outline = ink::outline(&strokes, &pen);
                    fill_outline(
                        frame,
                        raster,
                        &outline,
                        &Matrix::IDENTITY,
                        (0x17, 0x21, 0x70),
                    );
                }
            }
            Tab::Type => {
                let field = (34.0 * dpi) as i32;
                self.name.draw(frame, text, theme, dpi, ax, ay, aw, field);
                text.draw(
                    frame,
                    ax as f32,
                    (ay + field) as f32 + text.ascent(size) + 12.0 * dpi,
                    size,
                    "Votre nom sera écrit dans une police manuscrite du système.",
                    theme.text_dim,
                );
            }
            Tab::Import => {
                let bh = (30.0 * dpi) as i32;
                let label = "Choisir une image…";
                let w = (text.measure(size, label) + 28.0 * dpi) as i32;
                frame.fill_rect(ax, ay, w, bh, theme.hover.0, theme.hover.1, theme.hover.2);
                text.draw(
                    frame,
                    (ax + (14.0 * dpi) as i32) as f32,
                    ay as f32 + f32::midpoint(bh as f32, text.ascent(size)) - 1.0,
                    size,
                    label,
                    theme.text,
                );
                self.buttons.push((ax, ay, w, bh, Button::Import));
                let chosen = match &self.image {
                    Some(path) => path.display().to_string(),
                    None => "Photo ou capture de votre signature, PNG ou JPEG.".into(),
                };
                text.draw_clipped(
                    frame,
                    ax as f32,
                    (ay + bh) as f32 + text.ascent(size) + 12.0 * dpi,
                    size,
                    &chosen,
                    theme.text_dim,
                    aw as f32,
                );
                text.draw(
                    frame,
                    ax as f32,
                    (ay + bh) as f32 + text.ascent(size) * 2.0 + 26.0 * dpi,
                    size,
                    "Le fond du papier sera retiré automatiquement.",
                    theme.text_dim,
                );
            }
        }
    }
}

/// Assombrit tout le cadre, pour détacher une fenêtre modale.
fn veil(frame: &mut Frame<'_>) {
    for pixel in frame.pixels.chunks_exact_mut(4) {
        pixel[0] = (u32::from(pixel[0]) * 45 / 100) as u8;
        pixel[1] = (u32::from(pixel[1]) * 45 / 100) as u8;
        pixel[2] = (u32::from(pixel[2]) * 45 / 100) as u8;
    }
}

/// Convertit un contour d'encre en chemin `acrux-graphics`.
#[must_use]
pub fn path_of(outline: &Outline) -> Path {
    let mut path = Path::new();
    for seg in &outline.segs {
        match *seg {
            Seg::Move(x, y) => path.move_to(Point::new(x, y)),
            Seg::Line(x, y) => path.line_to(Point::new(x, y)),
            Seg::Curve(x1, y1, x2, y2, x3, y3) => {
                path.curve_to(Point::new(x1, y1), Point::new(x2, y2), Point::new(x3, y3));
            }
            Seg::Close => path.close(),
        }
    }
    path
}

/// Remplit un contour d'encre dans le tampon de la fenêtre.
///
/// La règle est **non nulle**, la même qu'à l'écriture dans le PDF : les
/// boucles des virages serrés se comblent au lieu de percer.
pub fn fill_outline(
    frame: &mut Frame<'_>,
    raster: &mut Rasterizer,
    outline: &Outline,
    matrix: &Matrix,
    color: (u8, u8, u8),
) {
    if outline.is_empty() {
        return;
    }
    let (w, h) = (frame.width, frame.height);
    let mask = raster.path_coverage(&path_of(outline), matrix, FillRule::NonZero, w, h);
    let data = mask.data();
    let bounds = mask.bounds();
    let (x0, y0) = (bounds.0, bounds.1);
    let (x1, y1) = (bounds.2.min(w), bounds.3.min(h));
    for row in y0..y1 {
        for col in x0..x1 {
            let a = u32::from(data[(row * w + col) as usize]);
            if a == 0 {
                continue;
            }
            let i = frame.index(col as usize, row as usize);
            let inv = 255 - a;
            let d = &mut frame.pixels[i..i + 4];
            d[0] = ((u32::from(color.2) * a + u32::from(d[0]) * inv) / 255) as u8;
            d[1] = ((u32::from(color.1) * a + u32::from(d[1]) * inv) / 255) as u8;
            d[2] = ((u32::from(color.0) * a + u32::from(d[2]) * inv) / 255) as u8;
        }
    }
}

/// Barre de l'outil : ce qu'on peut poser, et le bouton pour en sortir.
#[derive(Debug, Default)]
pub struct Bar {
    /// Élément choisi.
    pub item: Option<Item>,
    /// Zones cliquables, remplies au dessin.
    hits: Vec<(i32, i32, i32, i32, Option<Item>)>,
}

/// Les éléments de la barre, dans l'ordre d'Acrobat.
pub const ITEMS: [Item; 8] = [
    Item::Signature,
    Item::Initials,
    Item::Text,
    Item::Mark(Mark::Check),
    Item::Mark(Mark::Cross),
    Item::Mark(Mark::Circle),
    Item::Mark(Mark::Line),
    Item::Mark(Mark::Dot),
];

impl Bar {
    /// Hauteur de la barre en pixels.
    #[must_use]
    pub fn height(dpi: f32) -> i32 {
        (34.0 * dpi) as i32
    }

    /// Clic dans la barre : `None` s'il tombe à côté.
    #[must_use]
    pub fn mouse_down(&self, x: i32, y: i32) -> Option<Action> {
        for &(bx, by, bw, bh, item) in &self.hits {
            if x >= bx && x < bx + bw && y >= by && y < by + bh {
                return Some(match item {
                    Some(i) => Action::Pick(i),
                    None => Action::Close,
                });
            }
        }
        None
    }

    /// Dessine la barre sur toute la largeur, à l'ordonnée `y`.
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f32,
        y: i32,
    ) {
        let h = Self::height(dpi);
        let size = theme.font_size * dpi;
        frame.fill_rect(
            0,
            y,
            frame.width as i32,
            h,
            theme.bar.0,
            theme.bar.1,
            theme.bar.2,
        );
        frame.fill_rect(
            0,
            y + h - 1,
            frame.width as i32,
            1,
            theme.separator.0,
            theme.separator.1,
            theme.separator.2,
        );
        self.hits.clear();
        let pad = (10.0 * dpi) as i32;
        let mut x = pad;
        for item in ITEMS {
            let label = item.label();
            let glyph = (18.0 * dpi) as i32;
            let is_mark = matches!(item, Item::Mark(_));
            let w = (text.measure(size, label) + 22.0 * dpi) as i32
                + if is_mark {
                    glyph + (4.0 * dpi) as i32
                } else {
                    0
                };
            let selected = self.item == Some(item);
            if selected {
                frame.fill_rect(
                    x,
                    y + (3.0 * dpi) as i32,
                    w,
                    h - (7.0 * dpi) as i32,
                    theme.accent.0,
                    theme.accent.1,
                    theme.accent.2,
                );
            }
            let color = if selected {
                (255, 255, 255)
            } else {
                theme.text
            };
            let mut tx = x + (11.0 * dpi) as i32;
            if let Item::Mark(mark) = item {
                // Le bouton montre la marque elle-même, dessinée par le même
                // code que celui qui l'écrira dans le PDF.
                let outline = outline_of(mark);
                let (bw, bh) = (
                    (outline.bbox.x1 - outline.bbox.x0).max(0.01),
                    (outline.bbox.y1 - outline.bbox.y0).max(0.01),
                );
                let s = (f64::from(glyph) / bw.max(bh)).min(f64::from(glyph) / bh);
                let top = f64::from(y) + f64::from(h) / 2.0 + bh * s / 2.0;
                // Y vers le bas à l'écran : la matrice retourne le dessin.
                let m = Matrix::new(
                    s,
                    0.0,
                    0.0,
                    -s,
                    f64::from(tx) - outline.bbox.x0 * s,
                    top + outline.bbox.y0 * s,
                );
                fill_outline(frame, raster, &outline, &m, color);
                tx += glyph + (4.0 * dpi) as i32;
            }
            text.draw(
                frame,
                tx as f32,
                y as f32 + f32::midpoint(h as f32, text.ascent(size)) - 1.0,
                size,
                label,
                color,
            );
            self.hits.push((x, y, w, h, Some(item)));
            x += w + (4.0 * dpi) as i32;
        }
        let label = "Terminer";
        let w = (text.measure(size, label) + 24.0 * dpi) as i32;
        let bx = frame.width as i32 - w - pad;
        if bx > x {
            text.draw(
                frame,
                (bx + (12.0 * dpi) as i32) as f32,
                y as f32 + f32::midpoint(h as f32, text.ascent(size)) - 1.0,
                size,
                label,
                theme.text_dim,
            );
            self.hits.push((bx, y, w, h, None));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{Action, Capture, Item, Saved, Tab, ITEMS};
    use acrux_features::fillsign::marks::Mark;

    #[test]
    fn un_trace_donne_une_signature_dessinee() {
        let mut c = Capture::new(false);
        c.canvas = (0, 0, 200, 100);
        c.mouse_down(10, 60);
        c.mouse_move(40, 30, true);
        c.mouse_move(80, 70, true);
        c.mouse_up();
        match c.result() {
            Some(Saved::Drawn(strokes)) => {
                assert_eq!(strokes.len(), 1);
                assert_eq!(strokes[0].points.len(), 3);
                // L'ordonnée est retournée : le repère du PDF monte.
                assert!((strokes[0].points[0].y - 40.0).abs() < 0.01);
            }
            other => panic!("attendu un tracé, reçu {other:?}"),
        }
    }

    #[test]
    fn un_nom_tape_donne_une_signature_tapee() {
        let mut c = Capture::new(false);
        c.tab = Tab::Type;
        for ch in "Élise".chars() {
            c.char(ch);
        }
        assert_eq!(c.result(), Some(Saved::Typed("Élise".into())));
    }

    #[test]
    fn une_capture_vide_ne_produit_rien() {
        let c = Capture::new(true);
        assert_eq!(c.result(), None);
        let mut c = Capture::new(true);
        c.tab = Tab::Type;
        for ch in "   ".chars() {
            c.char(ch);
        }
        assert_eq!(c.result(), None);
    }

    #[test]
    fn effacer_vide_les_trois_onglets() {
        let mut c = Capture::new(false);
        c.canvas = (0, 0, 200, 100);
        c.mouse_down(10, 60);
        c.mouse_up();
        c.tab = Tab::Type;
        c.char('A');
        assert!(c.result().is_some());
        c.press(super::Button::Clear);
        assert_eq!(c.result(), None);
        c.tab = Tab::Draw;
        assert_eq!(c.result(), None);
    }

    #[test]
    fn le_tracé_reste_dans_la_zone() {
        let mut c = Capture::new(false);
        c.canvas = (10, 10, 100, 50);
        c.mouse_down(20, 20);
        c.mouse_move(500, -80, true);
        c.mouse_up();
        let Some(Saved::Drawn(strokes)) = c.result() else {
            panic!("tracé attendu")
        };
        assert!(strokes[0].points.iter().all(|p| p.x <= 110.01));
    }

    #[test]
    fn les_signatures_font_laller_retour_par_les_preferences() {
        use acrux_features::fillsign::ink::{InkPoint, Stroke};
        for saved in [
            Saved::Typed("Élise Marchand".into()),
            Saved::Image(std::path::PathBuf::from("C:/sig.png")),
            Saved::Drawn(vec![
                Stroke {
                    points: vec![InkPoint::new(1.0, 2.0), InkPoint::new(3.5, 4.5)],
                },
                Stroke {
                    points: vec![InkPoint::new(9.0, 8.0)],
                },
            ]),
        ] {
            let text = saved.encode();
            assert_eq!(Saved::decode(&text), Some(saved), "« {text} »");
        }
        assert_eq!(Saved::decode("drawn:"), None);
        assert_eq!(Saved::decode("typed:   "), None);
        assert_eq!(Saved::decode("n'importe quoi"), None);
    }

    #[test]
    fn chaque_element_a_un_libelle_et_une_taille() {
        for item in ITEMS {
            assert!(!item.label().is_empty());
            let (w, h) = item.default_size();
            assert!(w > 0.0 && h > 0.0, "{item:?}");
        }
        assert_eq!(Item::Mark(Mark::Check).label(), "Coche");
    }

    #[test]
    fn valider_rend_la_signature() {
        let mut c = Capture::new(true);
        c.tab = Tab::Type;
        c.char('J');
        c.char('D');
        assert_eq!(
            c.press(super::Button::Apply),
            Action::Save(true, Saved::Typed("JD".into()))
        );
    }
}
