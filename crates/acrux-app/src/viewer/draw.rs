//! Dessiner et écrire sur la page : rectangle, ellipse, ligne, flèche,
//! crayon, zone de texte et légende — les outils de dessin de la barre des
//! commentaires.
//!
//! # Le geste, puis l'annotation
//!
//! Tant qu'on trace, rien n'est écrit : le geste vit dans un **brouillon**
//! ([`Draft`]), dessiné par-dessus la page à chaque mouvement, avec les
//! formules mêmes du moteur (`annotations::shapes`, `annotations::freetext`)
//! — la flèche qu'on voit en glissant est celle qui sera posée. Au bout du
//! geste, le brouillon devient **une** annotation, par la même
//! modification que les autres (`EditOp::Annotate`) : l'annulation, le
//! fil de rendu et l'enregistrement n'ont rien de nouveau à apprendre.
//!
//! Une forme se pose au relâchement. Le crayon, lui, attend : plusieurs
//! traits font un seul dessin, comme dans Acrobat, jusqu'à Échap ou Entrée.
//! Une zone de texte s'ouvre au relâchement et se tape **en place** ; elle
//! se referme par Échap, Ctrl+Entrée ou un clic ailleurs.
//!
//! Un brouillon ne se perd jamais : enregistrer, annuler, changer d'outil,
//! d'onglet ou de mode le valident d'abord (voir `flush_typing`,
//! `apply_edit`, `close_annot_tools`).
//!
//! # Maj
//!
//! Maj contraint le geste, comme partout : un rectangle devient carré, une
//! ellipse un cercle, une ligne suit un multiple de 45°. Elle se lit à
//! chaque mouvement : on peut l'enfoncer ou la relâcher en plein geste.
//!
//! # Réglages
//!
//! Couleur, remplissage, épaisseur, opacité, et pour le texte la couleur, le
//! corps, la police, le cadre et le fond : un seul jeu ([`DrawStyle`]),
//! réglé dans la barre des commentaires et retenu dans les préférences.
//! Changer un réglage pendant la frappe d'une zone s'y voit aussitôt.

// Coordonnées d'écran entières et couleurs sur 8 bits, calculées depuis des
// grandeurs flottantes bornées.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap
)]

use acrux_core::{Matrix, Path, Point, Rect};
use acrux_features::annotations::freetext::{self, LEADING};
use acrux_features::annotations::shapes::{self, EndShape};
use acrux_features::annotations::{
    AnnotMeta, Callout, LineEnding, NewAnnotation, ShapeStyle, TextAlign, SHAPE_COLOR,
};
use acrux_features::stamp::StandardFont;
use acrux_graphics::{stroke_path, LineCap, LineJoin, StrokeStyle};

use super::{author_name, log_line, AnnotTool, Viewer};
use crate::platform::{Cursor, Event, Frame, Key, Modifiers, MouseButton, WindowHandle};
use crate::render_worker::{AnnotItem, EditOp};
use crate::ui::fieldedit::{FieldEditor, FieldKey};
use crate::ui::lang::{tr, trf};
use crate::ui::menu::Menu;
use crate::ui::modebar::{Setting, Swatch};
use crate::ui::paint::round_rect_alpha;
use crate::ui::pickers::{remember_color, ColorPicker, Outcome};
use crate::ui::sign::fill_path_alpha;
use crate::ui::text::TextRenderer;

/// Côté d'une forme posée d'un simple clic, en points : un clic doit
/// laisser quelque chose de visible, pas une forme d'un point.
const CLICK_SHAPE: f64 = 36.0;

/// Largeur d'une zone de texte posée d'un clic, en points.
const CLICK_TEXT_WIDTH: f64 = 200.0;

/// Largeur de la zone d'une légende, en points.
const CALLOUT_WIDTH: f64 = 150.0;

/// Distance, en pixels d'écran, en deçà de laquelle un geste est un clic.
const CLICK_SLOP: i32 = 3;

/// Distance minimale, en pixels d'écran, entre deux points relevés du
/// crayon : la souris en envoie des centaines par seconde, dont la plupart
/// n'ajoutent rien au trait.
const INK_STEP_PX: f64 = 0.5;

/// Épaisseur du cadre d'une zone de texte, en points.
const TEXT_BORDER: f64 = 1.0;

/// Épaisseurs proposées, en points.
pub(super) const WIDTHS: [f64; 8] = [0.5, 1.0, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0];

/// Opacités proposées.
pub(super) const OPACITIES: [f64; 4] = [1.0, 0.75, 0.5, 0.25];

/// Corps proposés pour le texte, en points.
pub(super) const TEXT_SIZES: [f64; 12] = [
    8.0, 9.0, 10.0, 11.0, 12.0, 14.0, 16.0, 18.0, 20.0, 24.0, 28.0, 36.0,
];

/// Polices proposées : les polices standard, que tout lecteur affiche sans
/// rien incorporer. (Le sélecteur des polices installées ne sert pas ici :
/// le texte d'une zone s'écrit dans une police standard.)
pub(super) const TEXT_FONTS: [(StandardFont, &str); 6] = [
    (StandardFont::Helvetica, "Helvetica"),
    (StandardFont::HelveticaBold, "Helvetica gras"),
    (StandardFont::TimesRoman, "Times"),
    (StandardFont::TimesBold, "Times gras"),
    (StandardFont::Courier, "Courier"),
    (StandardFont::CourierBold, "Courier gras"),
];

/// Réglages des outils de dessin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct DrawStyle {
    /// Couleur du trait des formes et du crayon.
    pub(super) stroke: [f64; 3],
    /// Remplissage des rectangles et ellipses.
    pub(super) fill: Option<[f64; 3]>,
    /// Épaisseur du trait, en points.
    pub(super) width: f64,
    /// Opacité des formes et du crayon.
    pub(super) opacity: f64,
    /// Couleur du texte.
    pub(super) text_color: [f64; 3],
    /// Corps du texte, en points.
    pub(super) text_size: f64,
    /// Police du texte.
    pub(super) font: StandardFont,
    /// Cadre d'une zone de texte.
    pub(super) border: Option<[f64; 3]>,
    /// Fond d'une zone de texte.
    pub(super) text_fill: Option<[f64; 3]>,
}

impl Default for DrawStyle {
    /// Les réglages d'Acrobat : trait rouge de 2 pt, sans fond ; texte noir
    /// en Helvetica 12 dans un cadre rouge.
    fn default() -> Self {
        Self {
            stroke: SHAPE_COLOR,
            fill: None,
            width: 2.0,
            opacity: 1.0,
            text_color: [0.0, 0.0, 0.0],
            text_size: 12.0,
            font: StandardFont::Helvetica,
            border: Some(SHAPE_COLOR),
            text_fill: None,
        }
    }
}

/// Couleur en « RRVVBB ».
fn hex(c: [f64; 3]) -> String {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("{:02X}{:02X}{:02X}", b(c[0]), b(c[1]), b(c[2]))
}

/// Couleur lue en « RRVVBB ».
fn parse_hex(s: &str) -> Option<[f64; 3]> {
    if s.len() != 6 || !s.is_ascii() {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    Some([
        f64::from(channel(0)?) / 255.0,
        f64::from(channel(2)?) / 255.0,
        f64::from(channel(4)?) / 255.0,
    ])
}

/// Couleur facultative : « - » pour aucune ; `current` si la valeur ne se
/// lit pas.
fn parse_optional(s: &str, current: Option<[f64; 3]>) -> Option<[f64; 3]> {
    if s == "-" {
        None
    } else {
        parse_hex(s).or(current)
    }
}

impl DrawStyle {
    /// Forme texte, pour les préférences : `clé=valeur` séparés par des
    /// points-virgules, « - » pour une couleur absente.
    pub(super) fn encode(&self) -> String {
        let opt = |c: Option<[f64; 3]>| c.map_or_else(|| "-".to_string(), hex);
        format!(
            "stroke={};fill={};width={};opacity={};text={};size={};font={};border={};back={}",
            hex(self.stroke),
            opt(self.fill),
            self.width,
            self.opacity,
            hex(self.text_color),
            self.text_size,
            self.font.base_font(),
            opt(self.border),
            opt(self.text_fill)
        )
    }

    /// Relit la forme texte ; une valeur illisible garde celle par défaut,
    /// une clé inconnue est ignorée : un fichier d'une autre version reste
    /// lisible.
    pub(super) fn decode(text: &str) -> Self {
        let mut style = Self::default();
        for part in text.split(';') {
            let Some((key, value)) = part.split_once('=') else {
                continue;
            };
            let value = value.trim();
            let number = value.parse::<f64>().ok().filter(|v| v.is_finite());
            match key.trim() {
                "stroke" => style.stroke = parse_hex(value).unwrap_or(style.stroke),
                "fill" => style.fill = parse_optional(value, style.fill),
                "width" => style.width = number.map_or(style.width, |v| v.clamp(0.25, 72.0)),
                "opacity" => style.opacity = number.map_or(style.opacity, |v| v.clamp(0.05, 1.0)),
                "text" => style.text_color = parse_hex(value).unwrap_or(style.text_color),
                "size" => style.text_size = number.map_or(style.text_size, |v| v.clamp(4.0, 144.0)),
                "font" => {
                    if let Some(f) = StandardFont::from_base_font(value) {
                        style.font = f;
                    }
                }
                "border" => style.border = parse_optional(value, style.border),
                "back" => style.text_fill = parse_optional(value, style.text_fill),
                _ => {}
            }
        }
        style
    }

    /// Aspect de la forme que pose `tool` : le remplissage ne vaut que pour
    /// le rectangle et l'ellipse.
    pub(super) fn shape(&self, tool: AnnotTool) -> ShapeStyle {
        ShapeStyle {
            stroke: Some(self.stroke),
            fill: if tool.fills() { self.fill } else { None },
            width: self.width,
            opacity: self.opacity,
        }
    }

    /// Cadre d'une zone de texte : couleur et épaisseur.
    fn text_border(&self) -> Option<([f64; 3], f64)> {
        self.border.map(|c| (c, TEXT_BORDER))
    }
}

/// Un réglage de la barre des commentaires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DrawSetting {
    /// Couleur du trait.
    Stroke,
    /// Remplissage.
    Fill,
    /// Épaisseur du trait.
    Width,
    /// Opacité.
    Opacity,
    /// Couleur du texte.
    TextColor,
    /// Corps du texte.
    TextSize,
    /// Police.
    Font,
    /// Cadre d'une zone de texte.
    Border,
    /// Fond d'une zone de texte.
    TextFill,
}

impl DrawSetting {
    /// Nom du réglage, pour son info-bulle.
    pub(super) fn label(self) -> &'static str {
        match self {
            DrawSetting::Stroke => "Couleur du trait",
            DrawSetting::Fill => "Remplissage",
            DrawSetting::Width => "Épaisseur du trait",
            DrawSetting::Opacity => "Opacité",
            DrawSetting::TextColor => "Couleur du texte",
            DrawSetting::TextSize => "Taille du texte",
            DrawSetting::Font => "Police",
            DrawSetting::Border => "Cadre",
            DrawSetting::TextFill => "Fond",
        }
    }

    /// Libellé du choix « aucune couleur », pour les couleurs qui peuvent
    /// manquer.
    fn none_label(self) -> Option<&'static str> {
        match self {
            DrawSetting::Fill => Some("Aucun remplissage"),
            DrawSetting::Border => Some("Aucun cadre"),
            DrawSetting::TextFill => Some("Aucun fond"),
            _ => None,
        }
    }
}

/// Réglages d'un outil, dans l'ordre de la barre.
pub(super) fn settings_for(tool: AnnotTool) -> &'static [DrawSetting] {
    match tool {
        AnnotTool::Rectangle | AnnotTool::Ellipse => &[
            DrawSetting::Stroke,
            DrawSetting::Fill,
            DrawSetting::Width,
            DrawSetting::Opacity,
        ],
        AnnotTool::Line | AnnotTool::Arrow | AnnotTool::Pencil => &[
            DrawSetting::Stroke,
            DrawSetting::Width,
            DrawSetting::Opacity,
        ],
        AnnotTool::TextBox | AnnotTool::Callout => &[
            DrawSetting::TextColor,
            DrawSetting::TextSize,
            DrawSetting::Font,
            DrawSetting::Border,
            DrawSetting::TextFill,
        ],
        _ => &[],
    }
}

/// Valeur d'un réglage choisie dans une liste.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Choice {
    /// Un nombre : épaisseur, opacité ou corps.
    Number(f64),
    /// Une police.
    Font(StandardFont),
}

/// Le nuancier ou la liste d'un réglage, déroulé sous lui.
pub(super) enum DrawPopup {
    /// Nuancier d'une couleur.
    Colors {
        /// Réglage visé.
        target: DrawSetting,
        /// Le nuancier.
        picker: Box<ColorPicker>,
        /// Bouton sous lequel il s'ouvre.
        anchor: (i32, i32, i32, i32),
    },
    /// Liste de valeurs.
    List {
        /// Réglage visé.
        target: DrawSetting,
        /// La liste.
        menu: Menu<Choice>,
    },
}

impl DrawPopup {
    /// Réglage visé.
    pub(super) fn target(&self) -> DrawSetting {
        match self {
            DrawPopup::Colors { target, .. } | DrawPopup::List { target, .. } => *target,
        }
    }
}

/// Zone de texte en cours de frappe.
pub(super) struct TextDraft {
    /// Page.
    pub(super) page: usize,
    /// Zone du texte, dans l'espace de la page. Son haut est fixe ; sa
    /// hauteur suit le texte.
    pub(super) rect: Rect,
    /// Hauteur tracée : le texte agrandit la zone, il ne la rapetisse pas
    /// en deçà.
    pub(super) min_height: f64,
    /// Point que désigne une légende.
    pub(super) anchor: Option<Point>,
    /// Le texte, son curseur et ses étapes d'annulation.
    pub(super) editor: FieldEditor,
    /// Un glisser sélectionne dans la zone.
    pub(super) selecting: bool,
}

/// Ce qu'on est en train de dessiner, pas encore écrit dans le document.
pub(super) enum Draft {
    /// Une forme qu'on trace, de `from` au pointeur.
    Shape {
        /// Outil.
        tool: AnnotTool,
        /// Page.
        page: usize,
        /// Point de départ, dans l'espace de la page.
        from: Point,
        /// Point courant.
        to: Point,
        /// Maj enfoncée.
        shift: bool,
        /// Point de départ à l'écran, pour distinguer un clic d'un geste.
        start: (i32, i32),
        /// Le pointeur s'est assez éloigné pour que ce soit un geste.
        moved: bool,
    },
    /// Le crayon : les traits déjà faits, dont le dernier peut être en
    /// cours.
    Ink {
        /// Page.
        page: usize,
        /// Traits, en points de la page.
        strokes: Vec<Vec<Point>>,
        /// Le bouton est enfoncé : le dernier trait s'allonge.
        drawing: bool,
    },
    /// Une zone de texte qu'on tape.
    Text(Box<TextDraft>),
}

impl Draft {
    /// Page du brouillon.
    pub(super) fn page(&self) -> usize {
        match self {
            Draft::Shape { page, .. } | Draft::Ink { page, .. } => *page,
            Draft::Text(t) => t.page,
        }
    }

    /// Vrai pendant un geste de la souris (bouton enfoncé).
    pub(super) fn gesture(&self) -> bool {
        match self {
            Draft::Shape { .. } => true,
            Draft::Ink { drawing, .. } => *drawing,
            Draft::Text(t) => t.selecting,
        }
    }
}

/// Point d'arrivée contraint par Maj : côtés égaux pour une forme (le plus
/// grand des deux, signes gardés), angle multiple de 45° pour une ligne.
pub(super) fn constrain(from: Point, to: Point, tool: AnnotTool, shift: bool) -> Point {
    if !shift {
        return to;
    }
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    match tool {
        AnnotTool::Line | AnnotTool::Arrow => {
            let len = dx.hypot(dy);
            let step = std::f64::consts::FRAC_PI_4;
            let angle = (dy.atan2(dx) / step).round() * step;
            let (s, c) = angle.sin_cos();
            // Les axes, exactement : un cosinus de 90° vaut 6e-17, pas 0.
            let snap = |v: f64| if v.abs() < 1e-9 { 0.0 } else { v };
            Point::new(from.x + snap(c) * len, from.y + snap(s) * len)
        }
        _ => {
            let side = dx.abs().max(dy.abs());
            let sign = |v: f64| if v < 0.0 { -1.0 } else { 1.0 };
            Point::new(from.x + sign(dx) * side, from.y + sign(dy) * side)
        }
    }
}

/// Zone d'une forme tracée de `from` à `to`, ou, pour un simple clic, un
/// carré de [`CLICK_SHAPE`] points centré sur le clic.
pub(super) fn shape_rect(from: Point, to: Point, moved: bool) -> Rect {
    if moved {
        Rect::new(from.x, from.y, to.x, to.y)
    } else {
        let h = CLICK_SHAPE / 2.0;
        Rect::new(from.x - h, from.y - h, from.x + h, from.y + h)
    }
}

/// Zone de la légende dont on a désigné `anchor` puis relâché en `to` : la
/// zone s'étend depuis `to`, du côté opposé à l'ancre. Un simple clic la
/// pose en haut à droite de l'ancre.
pub(super) fn callout_box(anchor: Point, to: Point, moved: bool, height: f64) -> Rect {
    let corner = if moved {
        to
    } else {
        Point::new(anchor.x + 36.0, anchor.y + 36.0)
    };
    let x0 = if corner.x >= anchor.x {
        corner.x
    } else {
        corner.x - CALLOUT_WIDTH
    };
    let y0 = if corner.y >= anchor.y {
        corner.y
    } else {
        corner.y - height
    };
    Rect::new(x0, y0, x0 + CALLOUT_WIDTH, y0 + height)
}

/// Annotation que donne un brouillon, et sa page ; rien pour un geste qui
/// ne laisse rien (une ligne sans longueur, un dessin sans trait, une zone
/// de texte vide).
///
/// C'est le **seul** endroit qui traduit un outil en annotation.
pub(super) fn draft_annotation(draft: &Draft, style: &DrawStyle) -> Option<(usize, NewAnnotation)> {
    match draft {
        Draft::Shape {
            tool,
            page,
            from,
            to,
            shift,
            moved,
            ..
        } => {
            let to = constrain(*from, *to, *tool, *shift);
            let shape = style.shape(*tool);
            let annotation = match tool {
                AnnotTool::Rectangle => NewAnnotation::Square {
                    rect: shape_rect(*from, to, *moved),
                    style: shape,
                    contents: None,
                },
                AnnotTool::Ellipse => NewAnnotation::Circle {
                    rect: shape_rect(*from, to, *moved),
                    style: shape,
                    contents: None,
                },
                AnnotTool::Line | AnnotTool::Arrow => {
                    if !moved || (to.x - from.x).hypot(to.y - from.y) < 1.0 {
                        return None;
                    }
                    NewAnnotation::Line {
                        from: *from,
                        to,
                        style: shape,
                        start: LineEnding::None,
                        end: if *tool == AnnotTool::Arrow {
                            LineEnding::OpenArrow
                        } else {
                            LineEnding::None
                        },
                        contents: None,
                    }
                }
                _ => return None,
            };
            Some((*page, annotation))
        }
        Draft::Ink { page, strokes, .. } => {
            let strokes: Vec<Vec<Point>> =
                strokes.iter().filter(|s| !s.is_empty()).cloned().collect();
            if strokes.is_empty() {
                return None;
            }
            Some((
                *page,
                NewAnnotation::Ink {
                    strokes,
                    style: style.shape(AnnotTool::Pencil),
                    contents: None,
                },
            ))
        }
        Draft::Text(t) => {
            let text = t.editor.buffer.text.trim_end().to_string();
            if text.trim().is_empty() {
                return None;
            }
            Some((
                t.page,
                NewAnnotation::FreeText {
                    rect: t.rect,
                    text,
                    font: style.font,
                    size: style.text_size,
                    color: style.text_color,
                    border: style.text_border(),
                    fill: style.text_fill,
                    align: TextAlign::Left,
                    callout: t.anchor.map(|anchor| Callout {
                        anchor,
                        knee: None,
                        ending: LineEnding::OpenArrow,
                    }),
                },
            ))
        }
    }
}

/// Nom de l'annotation pour le journal (`/Subtype`).
fn subtype(annotation: &NewAnnotation) -> &'static str {
    match annotation {
        NewAnnotation::Square { .. } => "Square",
        NewAnnotation::Circle { .. } => "Circle",
        NewAnnotation::Line { .. } => "Line",
        NewAnnotation::PolyLine { .. } => "PolyLine",
        NewAnnotation::Polygon { .. } => "Polygon",
        NewAnnotation::Ink { .. } => "Ink",
        NewAnnotation::FreeText {
            callout: Some(_), ..
        } => "FreeText (légende)",
        NewAnnotation::FreeText { .. } => "FreeText",
        _ => "annotation",
    }
}

/// Couleur d'aperçu à l'écran. Toutes les couleurs du document dessinées
/// ici passent par elle : c'est là qu'un mode d'affichage (couleurs
/// inversées pour la nuit) les transformera.
pub(super) fn preview_rgb(c: [f64; 3]) -> (u8, u8, u8) {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    (b(c[0]), b(c[1]), b(c[2]))
}

/// Opacité sur 8 bits.
fn alpha(opacity: f64) -> u8 {
    (opacity.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Ajoute une ellipse inscrite dans `r` à un tracé (quatre arcs de
/// Bézier, les mêmes que le moteur).
fn ellipse_path(path: &mut Path, r: &Rect) {
    const K: f64 = 0.552_284_75;
    let (rx, ry) = (r.width() / 2.0, r.height() / 2.0);
    let (cx, cy) = (r.x0 + rx, r.y0 + ry);
    let (kx, ky) = (K * rx, K * ry);
    path.move_to(Point::new(cx + rx, cy));
    path.curve_to(
        Point::new(cx + rx, cy + ky),
        Point::new(cx + kx, cy + ry),
        Point::new(cx, cy + ry),
    );
    path.curve_to(
        Point::new(cx - kx, cy + ry),
        Point::new(cx - rx, cy + ky),
        Point::new(cx - rx, cy),
    );
    path.curve_to(
        Point::new(cx - rx, cy - ky),
        Point::new(cx - kx, cy - ry),
        Point::new(cx, cy - ry),
    );
    path.curve_to(
        Point::new(cx + kx, cy - ry),
        Point::new(cx + rx, cy - ky),
        Point::new(cx + rx, cy),
    );
    path.close();
}

/// Tracé d'une suite de points.
fn polyline(points: &[Point]) -> Path {
    let mut path = Path::new();
    for (i, p) in points.iter().enumerate() {
        if i == 0 {
            path.move_to(*p);
        } else {
            path.line_to(*p);
        }
    }
    path
}

/// Fichiers d'une police système de mêmes largeurs qu'une police standard :
/// Arial pour Helvetica, Times New Roman pour Times, Courier New pour
/// Courier.
fn font_files(font: StandardFont) -> &'static [&'static str] {
    match font {
        StandardFont::HelveticaBold => &["arialbd.ttf", "LiberationSans-Bold.ttf"],
        StandardFont::HelveticaOblique => &["ariali.ttf", "LiberationSans-Italic.ttf"],
        StandardFont::HelveticaBoldOblique => &["arialbi.ttf", "LiberationSans-BoldItalic.ttf"],
        StandardFont::TimesRoman => &["times.ttf", "LiberationSerif-Regular.ttf"],
        StandardFont::TimesBold => &["timesbd.ttf", "LiberationSerif-Bold.ttf"],
        StandardFont::TimesItalic => &["timesi.ttf", "LiberationSerif-Italic.ttf"],
        StandardFont::TimesBoldItalic => &["timesbi.ttf", "LiberationSerif-BoldItalic.ttf"],
        StandardFont::Courier => &["cour.ttf", "LiberationMono-Regular.ttf"],
        StandardFont::CourierBold => &["courbd.ttf", "LiberationMono-Bold.ttf"],
        StandardFont::CourierOblique => &["couri.ttf", "LiberationMono-Italic.ttf"],
        StandardFont::CourierBoldOblique => &["courbi.ttf", "LiberationMono-BoldItalic.ttf"],
        _ => &["arial.ttf", "LiberationSans-Regular.ttf"],
    }
}

impl Viewer {
    // --- Géométrie ---------------------------------------------------------

    /// Point de la vue exprimé dans l'espace de la page `page`, rotation
    /// comprise — même hors de la page : un geste qui déborde reste sur la
    /// page où il a commencé.
    fn view_to_page(&self, page: usize, x: i32, y: i32) -> Option<Point> {
        let layout = self.layout();
        let m = self.page_to_view(&layout, page)?;
        Some(m.invert()?.apply(Point::new(f64::from(x), f64::from(y))))
    }

    /// Boîte visible d'une page.
    fn crop_of(&self, page: usize) -> Option<Rect> {
        let l = self.loaded.as_ref()?;
        Some(l.pages.get(page)?.crop_box(&l.doc))
    }

    /// Le même point, ramené dans la page.
    fn clamp_to_page(&self, page: usize, p: Point) -> Point {
        match self.crop_of(page) {
            Some(c) => Point::new(p.x.clamp(c.x0, c.x1), p.y.clamp(c.y0, c.y1)),
            None => p,
        }
    }

    /// Page **sous** un point de la vue, et le point dans son espace ; rien
    /// entre deux pages ou dans la marge : une forme commence sur une page.
    fn page_under(&self, x: i32, y: i32) -> Option<(usize, Point)> {
        let (page, p) = self.page_at(x, y)?;
        let crop = self.crop_of(page)?;
        crop.contains(p).then_some((page, p))
    }

    /// Pixels de la vue par point de page, à l'affichage courant.
    fn page_scale(m: &Matrix) -> f64 {
        m.a.hypot(m.b).max(1e-6)
    }

    // --- Souris ------------------------------------------------------------

    /// Clic sur la page avec un outil de dessin ; vrai s'il a servi.
    pub(super) fn draw_mouse_down(&mut self, x: i32, y: i32, shift: bool, clicks: u8) -> bool {
        let Some(tool) = self.annot_tool.filter(|t| t.draws()) else {
            return false;
        };
        // Une zone de texte ouverte : un clic dedans y place le curseur ; un
        // clic ailleurs la termine — il n'en commence pas une autre, comme
        // dans Acrobat.
        if let Some(Draft::Text(t)) = &self.draft {
            let page = t.page;
            let rect = t.rect;
            if let Some(p) = self.view_to_page(page, x, y) {
                let near = Rect::new(rect.x0 - 3.0, rect.y0 - 3.0, rect.x1 + 3.0, rect.y1 + 3.0);
                if near.contains(p) {
                    self.text_click(p, shift, clicks);
                    return true;
                }
            }
            self.commit_draft();
            return true;
        }
        let Some((page, p)) = self.page_under(x, y) else {
            return false;
        };
        if tool == AnnotTool::Pencil {
            // Un trait sur une autre page termine le dessin de la première.
            if self.draft.as_ref().is_some_and(|d| d.page() != page) {
                self.commit_draft();
            }
            match &mut self.draft {
                Some(Draft::Ink {
                    strokes, drawing, ..
                }) => {
                    strokes.push(vec![p]);
                    *drawing = true;
                }
                _ => {
                    self.draft = Some(Draft::Ink {
                        page,
                        strokes: vec![vec![p]],
                        drawing: true,
                    });
                }
            }
            return true;
        }
        self.commit_draft();
        self.draft = Some(Draft::Shape {
            tool,
            page,
            from: p,
            to: p,
            shift,
            start: (x, y),
            moved: false,
        });
        true
    }

    /// Le pointeur bouge, bouton enfoncé, pendant un geste de dessin.
    pub(super) fn draw_mouse_move(&mut self, x: i32, y: i32, shift_now: bool) {
        let Some(page) = self.draft.as_ref().map(Draft::page) else {
            return;
        };
        let Some(raw) = self.view_to_page(page, x, y) else {
            return;
        };
        let p = self.clamp_to_page(page, raw);
        let layout = self.layout();
        let scale = self
            .page_to_view(&layout, page)
            .map_or(1.0, |m| Self::page_scale(&m));
        let mut select_to = None;
        match &mut self.draft {
            Some(Draft::Shape {
                to,
                shift,
                start,
                moved,
                ..
            }) => {
                *to = p;
                *shift = shift_now;
                if (x - start.0).abs() > CLICK_SLOP || (y - start.1).abs() > CLICK_SLOP {
                    *moved = true;
                }
            }
            Some(Draft::Ink {
                strokes,
                drawing: true,
                ..
            }) => {
                if let Some(stroke) = strokes.last_mut() {
                    let far = stroke
                        .last()
                        .is_none_or(|q| (q.x - p.x).hypot(q.y - p.y) * scale >= INK_STEP_PX);
                    if far {
                        stroke.push(p);
                    }
                }
            }
            Some(Draft::Text(t)) if t.selecting => select_to = Some(raw),
            _ => {}
        }
        if let Some(p) = select_to {
            let index = self.text_index_at(p);
            if let Some(Draft::Text(t)) = &mut self.draft {
                t.editor.buffer.move_to(index, true);
            }
        }
    }

    /// Fin d'un geste : une forme se pose, un trait s'ajoute au dessin, une
    /// zone de texte s'ouvre.
    pub(super) fn draw_mouse_up(&mut self) {
        let Some(draft) = self.draft.take() else {
            return;
        };
        match draft {
            Draft::Shape {
                tool: AnnotTool::TextBox,
                page,
                from,
                to,
                shift,
                moved,
                ..
            } => {
                let to = constrain(from, to, AnnotTool::TextBox, shift);
                let traced = Rect::new(from.x, from.y, to.x, to.y);
                // Tracée : la zone est celle du geste. D'un clic : une
                // largeur d'usage à partir du clic, rognée au bord de la
                // page, et la hauteur d'une ligne.
                let (rect, min_height) = if moved && traced.width() >= 12.0 {
                    (traced, traced.height())
                } else {
                    let right = self.crop_of(page).map_or(from.x + CLICK_TEXT_WIDTH, |c| {
                        (from.x + CLICK_TEXT_WIDTH)
                            .min(c.x1 - 4.0)
                            .max(from.x + 40.0)
                    });
                    (Rect::new(from.x, from.y - 10.0, right, from.y), 0.0)
                };
                self.open_text(page, rect, min_height, None);
            }
            Draft::Shape {
                tool: AnnotTool::Callout,
                page,
                from,
                to,
                moved,
                ..
            } => {
                let height = freetext::fit_height(
                    "",
                    self.draw_style.font,
                    self.draw_style.text_size,
                    CALLOUT_WIDTH,
                    TEXT_BORDER,
                );
                let rect = callout_box(from, to, moved, height);
                self.open_text(page, rect, 0.0, Some(from));
            }
            Draft::Shape { .. } => {
                self.draft = Some(draft);
                self.commit_draft();
            }
            Draft::Ink {
                page,
                strokes,
                drawing: _,
            } => {
                self.draft = Some(Draft::Ink {
                    page,
                    strokes,
                    drawing: false,
                });
            }
            Draft::Text(mut t) => {
                t.selecting = false;
                self.draft = Some(Draft::Text(t));
            }
        }
    }

    /// Ouvre une zone de texte à taper.
    fn open_text(&mut self, page: usize, rect: Rect, min_height: f64, anchor: Option<Point>) {
        let mut t = TextDraft {
            page,
            rect,
            min_height,
            anchor,
            editor: FieldEditor::new("", None, true, None, false),
            selecting: false,
        };
        refit(&mut t, &self.draw_style);
        self.draft = Some(Draft::Text(Box::new(t)));
        log_line(&format!(
            "zone de texte ouverte : page {}{}",
            page + 1,
            if anchor.is_some() { " (légende)" } else { "" }
        ));
    }

    /// Rang du caractère sous un point de la page, dans la zone ouverte.
    fn text_index_at(&self, p: Point) -> usize {
        let Some(Draft::Text(t)) = &self.draft else {
            return 0;
        };
        let style = &self.draw_style;
        let pad = freetext::inner_padding(style.border.map_or(0.0, |_| TEXT_BORDER));
        let text = &t.editor.buffer.text;
        let lines = freetext::layout(
            text,
            style.font,
            style.text_size,
            t.rect.width() - 2.0 * pad,
        );
        let pitch = LEADING * style.text_size;
        let row = ((t.rect.y1 - pad - p.y) / pitch).floor().max(0.0) as usize;
        freetext::char_at(
            &lines,
            text,
            style.font,
            style.text_size,
            row,
            p.x - t.rect.x0 - pad,
        )
    }

    /// Clic dans la zone ouverte : le curseur s'y pose (Maj étend la
    /// sélection) ; double clic, le mot ; triple clic, tout.
    fn text_click(&mut self, p: Point, shift: bool, clicks: u8) {
        let index = self.text_index_at(p);
        if let Some(Draft::Text(t)) = &mut self.draft {
            match clicks {
                2 => t.editor.buffer.select_word(index),
                n if n >= 3 => t.editor.buffer.select_all(),
                _ => {
                    t.editor.buffer.move_to(index, shift);
                    t.selecting = true;
                }
            }
        }
    }

    /// Pointeur d'un outil de dessin au-dessus de la page.
    pub(super) fn draw_cursor(&self, x: i32, y: i32) -> Option<Cursor> {
        let tool = self.annot_tool.filter(|t| t.draws())?;
        if let Some(Draft::Text(t)) = &self.draft {
            if self
                .view_to_page(t.page, x, y)
                .is_some_and(|p| t.rect.contains(p))
            {
                return Some(Cursor::IBeam);
            }
        }
        Some(match tool {
            AnnotTool::Pencil => Cursor::Pen,
            AnnotTool::TextBox | AnnotTool::Callout => Cursor::AddText,
            _ => Cursor::Cross,
        })
    }

    // --- Clavier -------------------------------------------------------------

    /// Vrai si une zone de texte reçoit la frappe.
    pub(super) fn draft_typing(&self) -> bool {
        matches!(self.draft, Some(Draft::Text(_))) && self.draft_has_keys()
    }

    /// Vrai si le brouillon reçoit le clavier : aucune invite, palette ni
    /// liste ne passe devant, et le numéro de page de la barre d'outils
    /// n'est pas en saisie — sans quoi « 5 » puis Entrée s'écrivaient dans
    /// la zone ouverte au lieu de mener à la page 5.
    pub(super) fn draft_has_keys(&self) -> bool {
        self.draft.is_some()
            && self.prompt.is_none()
            && self.palette.is_none()
            && self.draw_popup.is_none()
            && !self.toolbar.has_focus()
    }

    /// Une touche pendant un brouillon ; vrai si elle a servi.
    ///
    /// Dans une zone de texte, les touches d'édition vont au texte, Entrée
    /// passe à la ligne, Échap et Ctrl+Entrée la terminent. Le crayon se
    /// termine par Échap ou Entrée ; une forme en plein geste s'abandonne
    /// par Échap.
    pub(super) fn draft_key(&mut self, key: Key, m: Modifiers) -> bool {
        let style = self.draw_style;
        match &mut self.draft {
            None => false,
            Some(Draft::Shape { .. }) => {
                if key == Key::Escape {
                    self.draft = None;
                    log_line("forme abandonnée");
                    true
                } else {
                    false
                }
            }
            Some(Draft::Ink { .. }) => {
                if matches!(key, Key::Escape | Key::Enter) {
                    self.commit_draft();
                    true
                } else {
                    false
                }
            }
            Some(Draft::Text(t)) => {
                if matches!(key, Key::F(_)) || (m.alt && !m.ctrl) {
                    return false;
                }
                let pad = freetext::inner_padding(style.border.map_or(0.0, |_| TEXT_BORDER));
                let width = (t.rect.width() - 2.0 * pad) as f32;
                let (font, size) = (style.font, style.text_size);
                let outcome = t.editor.key(
                    key,
                    m,
                    &mut |s: &str| font.text_width(s, size) as f32,
                    width,
                );
                match outcome {
                    FieldKey::Commit | FieldKey::Cancel => self.commit_draft(),
                    FieldKey::Changed => refit(t, &style),
                    FieldKey::Moved | FieldKey::Next(_) | FieldKey::None => {}
                }
                // Toute autre touche s'arrête là — la page ne défile pas
                // sous une espace —, sauf celles qui ne concernent pas la
                // frappe : PageSuiv reste à la vue, Ctrl+Tab aux onglets.
                let passes = outcome == FieldKey::None
                    && (matches!(key, Key::PageUp | Key::PageDown)
                        || (m.ctrl && key != Key::Space));
                !passes
            }
        }
    }

    /// Un caractère pendant un brouillon ; vrai s'il a servi.
    ///
    /// Dans une zone de texte : la frappe, et Ctrl+A, C, X, V, Z, Y sur le
    /// texte — jamais sur le document : Ctrl+Z défait la frappe, pas
    /// l'annotation d'avant. Pendant un dessin au crayon, Ctrl+Z retire le
    /// dernier trait.
    pub(super) fn draft_char(
        &mut self,
        c: char,
        m: Modifiers,
        window: &mut dyn WindowHandle,
    ) -> bool {
        let style = self.draw_style;
        let copy_allowed = self.rights().copy;
        match &mut self.draft {
            Some(Draft::Ink { .. }) => {
                if m.ctrl && matches!(c, 'z' | 'Z' | '\u{1a}') && !m.shift {
                    return self.draft_undo_step();
                }
                false
            }
            Some(Draft::Text(t)) => {
                if m.ctrl {
                    match c {
                        'a' | 'A' | '\u{1}' => t.editor.buffer.select_all(),
                        'c' | 'C' | '\u{3}' => {
                            if let Some(text) = t.editor.copy_text().filter(|_| copy_allowed) {
                                window.set_clipboard_text(&text);
                            }
                        }
                        'x' | 'X' | '\u{18}' => {
                            if let Some(text) = t.editor.cut() {
                                window.set_clipboard_text(&text);
                            }
                            refit(t, &style);
                        }
                        'v' | 'V' | '\u{16}' => {
                            if let Some(text) = window.clipboard_text() {
                                t.editor.insert(&text);
                                refit(t, &style);
                            }
                        }
                        // Rien à défaire dans la zone : la touche s'arrête
                        // là quand même, l'annotation d'avant reste.
                        'z' | 'Z' | '\u{1a}' if m.shift => {
                            self.draft_redo_step();
                        }
                        'z' | 'Z' | '\u{1a}' => {
                            self.draft_undo_step();
                        }
                        'y' | 'Y' | '\u{19}' => {
                            self.draft_redo_step();
                        }
                        // Les autres raccourcis (enregistrer, imprimer,
                        // rechercher…) valent pour le document : ils
                        // valident d'abord, comme dans un champ de
                        // formulaire.
                        _ => {
                            self.commit_draft();
                            return false;
                        }
                    }
                    return true;
                }
                if !c.is_control() {
                    t.editor.insert(&c.to_string());
                    refit(t, &style);
                }
                true
            }
            _ => false,
        }
    }

    // --- Annulation dans le brouillon ----------------------------------------

    /// Ce qui se défait et se refait **dans** le brouillon, avant le
    /// document : une étape de frappe, un trait de crayon.
    pub(super) fn draft_history(&self) -> (bool, bool) {
        match &self.draft {
            Some(Draft::Text(t)) => (t.editor.can_undo(), t.editor.can_redo()),
            Some(Draft::Ink { strokes, .. }) => (!strokes.is_empty(), false),
            _ => (false, false),
        }
    }

    /// Défait une étape de la frappe, ou retire le dernier trait du crayon ;
    /// faux s'il n'y a rien à défaire ici. Le bouton « Annuler », la palette
    /// et Ctrl+Z passent tous par là : aucun ne défait l'annotation d'avant
    /// pendant qu'on tape.
    pub(super) fn draft_undo_step(&mut self) -> bool {
        let style = self.draw_style;
        match &mut self.draft {
            Some(Draft::Text(t)) => {
                let done = t.editor.undo_step();
                if done {
                    refit(t, &style);
                }
                done
            }
            Some(Draft::Ink { strokes, .. }) => {
                strokes.pop();
                if strokes.is_empty() {
                    self.draft = None;
                }
                log_line("crayon : dernier trait retiré");
                true
            }
            _ => false,
        }
    }

    /// Refait une étape de frappe défaite.
    pub(super) fn draft_redo_step(&mut self) -> bool {
        let style = self.draw_style;
        match &mut self.draft {
            Some(Draft::Text(t)) => {
                let done = t.editor.redo_step();
                if done {
                    refit(t, &style);
                }
                done
            }
            _ => false,
        }
    }

    // --- Validation ----------------------------------------------------------

    /// Écrit le brouillon dans le document, s'il laisse quelque chose.
    ///
    /// Appelé au bout d'un geste, et avant tout ce qui lit le document ou
    /// le quitte : enregistrer, annuler, changer d'outil, de mode ou
    /// d'onglet.
    pub(super) fn commit_draft(&mut self) {
        let Some(draft) = self.draft.take() else {
            return;
        };
        let Some((page, annotation)) = draft_annotation(&draft, &self.draw_style) else {
            if matches!(draft, Draft::Text(_)) {
                log_line("zone de texte vide : rien n'est posé");
            }
            return;
        };
        let unsupported = match &annotation {
            NewAnnotation::FreeText { text, .. } => freetext::unsupported_chars(text),
            _ => Vec::new(),
        };
        let what = subtype(&annotation);
        let author = author_name();
        let item = AnnotItem {
            page,
            annotation,
            meta: AnnotMeta::fresh(author.as_deref()),
        };
        // L'image de la page, avant : elle reste à l'écran le temps que le
        // fil de rendu refasse la page, et le dessin par-dessus. Sans elle,
        // la page passerait au blanc un instant à chaque forme posée.
        let key_scale = (self.scale() * 1000.0).round() as u32;
        let before = self
            .loaded
            .as_mut()
            .and_then(|l| l.cache.remove(&(page, key_scale)));
        if !self.apply_edit(EditOp::Annotate { items: vec![item] }) {
            // Refusée (permissions) : la page n'a pas changé, son image non
            // plus.
            if let (Some(bitmap), Some(l)) = (before, self.loaded.as_mut()) {
                l.cache.insert((page, key_scale), bitmap);
            }
            return;
        }
        log_line(&format!("forme posée : {what} page {}", page + 1));
        if let (Some(bitmap), Some(l)) = (before, self.loaded.as_mut()) {
            l.cache.insert((page, key_scale + 1), bitmap);
            self.draft_ghost = Some((draft, key_scale));
        }
        if !unsupported.is_empty() {
            let list: String = unsupported.iter().collect();
            self.set_notice(trf(
                "Caractères remplacés par « ? » (police standard) : {}",
                &[&list],
            ));
        }
    }

    // --- Réglages ------------------------------------------------------------

    /// Les réglages de l'outil en cours, tels que la barre les montre.
    pub(super) fn draw_settings(&self) -> Vec<Setting> {
        let Some(tool) = self.annot_tool else {
            return Vec::new();
        };
        let s = &self.draw_style;
        let color = |c: Option<[f64; 3]>, swatch| Setting::Color {
            color: c.map(preview_rgb),
            swatch,
        };
        settings_for(tool)
            .iter()
            .map(|setting| match setting {
                DrawSetting::Stroke => color(Some(s.stroke), Swatch::Stroke),
                DrawSetting::Fill => color(s.fill, Swatch::Fill),
                DrawSetting::Width => Setting::Choice(format!("{} pt", number(s.width))),
                DrawSetting::Opacity => {
                    Setting::Choice(format!("{} %", (s.opacity * 100.0).round()))
                }
                DrawSetting::TextColor => color(Some(s.text_color), Swatch::Text),
                DrawSetting::TextSize => Setting::Choice(format!("{} pt", number(s.text_size))),
                DrawSetting::Font => Setting::Choice(
                    tr(TEXT_FONTS
                        .iter()
                        .find(|(f, _)| *f == s.font)
                        .map_or("Helvetica", |(_, name)| *name))
                    .to_string(),
                ),
                DrawSetting::Border => color(s.border, Swatch::Stroke),
                DrawSetting::TextFill => color(s.text_fill, Swatch::Fill),
            })
            .collect()
    }

    /// Rang, dans la barre, du réglage dont la liste est déroulée.
    pub(super) fn open_setting(&self) -> Option<usize> {
        let target = self.draw_popup.as_ref()?.target();
        let tool = self.annot_tool?;
        settings_for(tool).iter().position(|s| *s == target)
    }

    /// Clic sur le réglage de rang `index` de la barre : son nuancier ou
    /// sa liste se déroule dessous.
    #[allow(clippy::too_many_lines)] // un nuancier ou une liste par réglage
    pub(super) fn open_draw_setting(&mut self, index: usize, anchor: (i32, i32, i32, i32)) {
        let Some(tool) = self.annot_tool else {
            return;
        };
        let Some(&target) = settings_for(tool).get(index) else {
            return;
        };
        let s = self.draw_style;
        let colors = |current: Option<[f64; 3]>| {
            // Sans couleur en cours, le carré sur mesure part du blanc.
            let picker = ColorPicker::new(current.unwrap_or([1.0, 1.0, 1.0]));
            match target.none_label() {
                Some(label) => picker.with_none(label, current.is_none()),
                None => picker,
            }
        };
        let list = |items: Vec<(String, Choice)>, current: Choice| {
            let mut menu = Menu::below(anchor);
            for (label, value) in items {
                menu = menu.item(None, &label, "", value, true);
            }
            menu.highlight_where(|v| v == current);
            menu
        };
        let popup = match target {
            DrawSetting::Stroke => DrawPopup::Colors {
                target,
                picker: Box::new(colors(Some(s.stroke))),
                anchor,
            },
            DrawSetting::Fill => DrawPopup::Colors {
                target,
                picker: Box::new(colors(s.fill)),
                anchor,
            },
            DrawSetting::TextColor => DrawPopup::Colors {
                target,
                picker: Box::new(colors(Some(s.text_color))),
                anchor,
            },
            DrawSetting::Border => DrawPopup::Colors {
                target,
                picker: Box::new(colors(s.border)),
                anchor,
            },
            DrawSetting::TextFill => DrawPopup::Colors {
                target,
                picker: Box::new(colors(s.text_fill)),
                anchor,
            },
            DrawSetting::Width => DrawPopup::List {
                target,
                menu: list(
                    WIDTHS
                        .iter()
                        .map(|w| (format!("{} pt", number(*w)), Choice::Number(*w)))
                        .collect(),
                    Choice::Number(s.width),
                ),
            },
            DrawSetting::Opacity => DrawPopup::List {
                target,
                menu: list(
                    OPACITIES
                        .iter()
                        .map(|o| (format!("{} %", (o * 100.0).round()), Choice::Number(*o)))
                        .collect(),
                    Choice::Number(s.opacity),
                ),
            },
            DrawSetting::TextSize => DrawPopup::List {
                target,
                menu: list(
                    TEXT_SIZES
                        .iter()
                        .map(|z| (format!("{} pt", number(*z)), Choice::Number(*z)))
                        .collect(),
                    Choice::Number(s.text_size),
                ),
            },
            DrawSetting::Font => DrawPopup::List {
                target,
                menu: list(
                    TEXT_FONTS
                        .iter()
                        .map(|(f, name)| (tr(name).to_string(), Choice::Font(*f)))
                        .collect(),
                    Choice::Font(s.font),
                ),
            },
        };
        let mut popup = popup;
        if let DrawPopup::List { menu, .. } = &mut popup {
            let (w, h) = (self.width as i32, self.height as i32);
            let (font, dpi) = (self.theme.font_size, self.dpi_scale as f32);
            match self.text.as_mut() {
                Some(text) => {
                    menu.layout(&mut |s: f32, t: &str| text.measure(s, t), font, dpi, w, h);
                }
                None => menu.layout(
                    &mut |s: f32, t: &str| t.chars().count() as f32 * s * 0.55,
                    font,
                    dpi,
                    w,
                    h,
                ),
            }
        }
        log_line(&format!("réglage du dessin : {target:?}"));
        self.tip = None;
        self.draw_popup = Some(popup);
    }

    /// Applique une couleur choisie ; `persist` l'écrit aussi dans le
    /// fichier des préférences (voir `draw_style_changed`).
    fn set_draw_color(&mut self, target: DrawSetting, color: Option<[f64; 3]>, persist: bool) {
        let s = &mut self.draw_style;
        match target {
            DrawSetting::Stroke => s.stroke = color.unwrap_or(s.stroke),
            DrawSetting::Fill => s.fill = color,
            DrawSetting::TextColor => s.text_color = color.unwrap_or(s.text_color),
            DrawSetting::Border => s.border = color,
            DrawSetting::TextFill => s.text_fill = color,
            _ => {}
        }
        self.draw_style_changed(persist);
    }

    /// Applique une valeur choisie dans une liste.
    fn set_draw_choice(&mut self, target: DrawSetting, choice: Choice) {
        let s = &mut self.draw_style;
        match (target, choice) {
            (DrawSetting::Width, Choice::Number(v)) => s.width = v,
            (DrawSetting::Opacity, Choice::Number(v)) => s.opacity = v,
            (DrawSetting::TextSize, Choice::Number(v)) => s.text_size = v,
            (DrawSetting::Font, Choice::Font(f)) => s.font = f,
            _ => {}
        }
        self.draw_style_changed(true);
    }

    /// Un réglage a changé : la zone ouverte le prend aussitôt, les
    /// préférences le retiennent.
    ///
    /// Le fichier n'est écrit que si `persist` : la couleur qui suit le
    /// pointeur dans le carré sur mesure du nuancier change à chaque
    /// mouvement, et l'écrire à chaque fois ferait un accès disque par
    /// mouvement de souris. Elle est écrite quand le nuancier se referme,
    /// ou en quittant (`save_prefs`), qui reprend `prefs.draw_style`.
    fn draw_style_changed(&mut self, persist: bool) {
        let style = self.draw_style;
        if let Some(Draft::Text(t)) = &mut self.draft {
            refit(t, &style);
        }
        self.prefs.draw_style = style.encode();
        if persist {
            self.prefs.save();
        }
    }

    /// Un événement pour le nuancier ou la liste d'un réglage, s'il est
    /// ouvert : il prend tout — la souris, le clavier, la molette — comme
    /// la liste du zoom. Vrai s'il l'a pris.
    pub(super) fn draw_popup_event(
        &mut self,
        event: &Event,
        window: &mut dyn WindowHandle,
    ) -> bool {
        let Some(popup) = &mut self.draw_popup else {
            return false;
        };
        let target = popup.target();
        match popup {
            DrawPopup::Colors { picker, .. } => {
                let outcome = match *event {
                    Event::MouseDown { x, y, .. } => {
                        if picker.none_at(x, y) {
                            self.draw_popup = None;
                            self.set_draw_color(target, None, true);
                            window.request_redraw();
                            return true;
                        }
                        picker.mouse_down(x, y)
                    }
                    Event::MouseMove { x, y, dragging, .. } => {
                        let outcome = picker.mouse_move(x, y, dragging);
                        window.set_cursor(Cursor::Arrow);
                        window.request_redraw();
                        outcome
                    }
                    Event::MouseUp { .. } => {
                        picker.mouse_up();
                        Outcome::Stay
                    }
                    Event::Key(key, _) => picker.key(key),
                    Event::Char(c, m) if !m.ctrl => picker.char(c),
                    Event::Char(..) | Event::Wheel { .. } | Event::Nav { .. } => Outcome::Stay,
                    Event::Resize { .. }
                    | Event::DpiChanged(_)
                    | Event::FileDropped(_)
                    | Event::Close => {
                        self.draw_popup = None;
                        return false;
                    }
                    Event::Wake => return false,
                };
                match outcome {
                    Outcome::Live(c) => self.set_draw_color(target, Some(c), false),
                    Outcome::Pick(c) => {
                        remember_color(c);
                        self.draw_popup = None;
                        self.set_draw_color(target, Some(c), true);
                    }
                    Outcome::Close => {
                        // Refermé sans choix : une couleur suivie en
                        // direct reste, elle est écrite maintenant.
                        self.draw_popup = None;
                        self.draw_style_changed(true);
                    }
                    Outcome::Stay => {}
                }
            }
            DrawPopup::List { menu, .. } => {
                let outcome = match *event {
                    Event::MouseDown { x, y, .. } => menu.mouse_down(x, y),
                    Event::MouseUp { button, x, y } => {
                        if button == MouseButton::Left {
                            menu.mouse_up(x, y)
                        } else {
                            Outcome::Stay
                        }
                    }
                    Event::MouseMove { x, y, .. } => {
                        if menu.mouse_move(x, y) {
                            window.request_redraw();
                        }
                        window.set_cursor(Cursor::Arrow);
                        return true;
                    }
                    Event::Wheel { delta, .. } => {
                        menu.wheel(delta);
                        Outcome::Stay
                    }
                    Event::Key(key, _) => menu.key(key),
                    Event::Char(c, m) if !m.ctrl => menu.char(c),
                    Event::Char(..) | Event::Nav { .. } => Outcome::Close,
                    Event::Resize { .. }
                    | Event::DpiChanged(_)
                    | Event::FileDropped(_)
                    | Event::Close => {
                        self.draw_popup = None;
                        return false;
                    }
                    Event::Wake => return false,
                };
                match outcome {
                    Outcome::Pick(choice) | Outcome::Live(choice) => {
                        self.draw_popup = None;
                        self.set_draw_choice(target, choice);
                    }
                    Outcome::Close => self.draw_popup = None,
                    Outcome::Stay => {}
                }
            }
        }
        window.request_redraw();
        true
    }

    /// Dessine le nuancier ou la liste d'un réglage, par-dessus tout.
    pub(super) fn paint_draw_popup(&mut self, frame: &mut Frame<'_>) {
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        let style = self.draw_style;
        let Some(text) = self.text.as_mut() else {
            return;
        };
        match &mut self.draw_popup {
            Some(DrawPopup::Colors {
                target,
                picker,
                anchor,
            }) => {
                let current = match target {
                    DrawSetting::Stroke => Some(style.stroke),
                    DrawSetting::Fill => style.fill,
                    DrawSetting::TextColor => Some(style.text_color),
                    DrawSetting::Border => style.border,
                    DrawSetting::TextFill => style.text_fill,
                    _ => None,
                };
                // Une couleur absente n'a pas de pastille à entourer : un
                // gris impossible à confondre fait l'affaire.
                picker.paint(
                    frame,
                    text,
                    &theme,
                    dpi,
                    *anchor,
                    current.unwrap_or([-1.0, -1.0, -1.0]),
                );
            }
            Some(DrawPopup::List { menu, .. }) => {
                menu.paint(frame, text, &mut self.raster, &theme);
            }
            None => {}
        }
    }

    // --- Dessin ----------------------------------------------------------------

    /// Dessine le brouillon par-dessus la page, tel qu'il sera posé ; puis,
    /// le temps que la page revienne du fil de rendu, la dernière forme
    /// posée.
    pub(super) fn paint_draft(&mut self, frame: &mut Frame<'_>) {
        if let Some((ghost, key_scale)) = self.draft_ghost.take() {
            let page = ghost.page();
            let arrived = self
                .loaded
                .as_ref()
                .is_none_or(|l| l.cache.contains_key(&(page, key_scale)));
            let current = (self.scale() * 1000.0).round() as u32;
            if arrived || current != key_scale {
                // La page est revenue (ou l'échelle a changé) : l'image
                // d'avant n'a plus rien à remplacer.
                if let Some(l) = self.loaded.as_mut() {
                    l.cache.remove(&(page, key_scale + 1));
                }
            } else {
                self.paint_one(frame, &ghost, false);
                self.draft_ghost = Some((ghost, key_scale));
            }
        }
        if let Some(draft) = self.draft.take() {
            self.paint_one(frame, &draft, true);
            self.draft = Some(draft);
        }
    }

    /// Dessine un brouillon ; `editing` ajoute ce qui ne se voit que
    /// pendant la saisie (le cadre de la zone, le curseur).
    fn paint_one(&mut self, frame: &mut Frame<'_>, draft: &Draft, editing: bool) {
        let layout = self.layout();
        let Some(m) = self.page_to_view(&layout, draft.page()) else {
            return;
        };
        let style = self.draw_style;
        match draft {
            Draft::Shape {
                tool,
                from,
                to,
                shift,
                moved,
                ..
            } => {
                if !moved {
                    return;
                }
                let to = constrain(*from, *to, *tool, *shift);
                self.paint_shape(frame, &m, *tool, [*from, to], &style);
            }
            Draft::Ink { strokes, .. } => {
                let shape = style.shape(AnnotTool::Pencil);
                let mut path = Path::new();
                for s in strokes {
                    match s.as_slice() {
                        [] => {}
                        [p] => {
                            path.move_to(*p);
                            path.line_to(Point::new(p.x + 0.01, p.y));
                        }
                        many => path.append(&polyline(many)),
                    }
                }
                self.stroke_preview(frame, &path, &m, &shape, true);
            }
            Draft::Text(t) => self.paint_text_draft(frame, &m, t, &style, editing),
        }
    }

    /// Trace un chemin de page avec le trait d'une forme.
    fn stroke_preview(
        &mut self,
        frame: &mut Frame<'_>,
        path: &Path,
        m: &Matrix,
        shape: &ShapeStyle,
        round: bool,
    ) {
        let Some(color) = shape.stroke else {
            return;
        };
        if shape.width <= 0.0 || path.is_empty() {
            return;
        }
        let stroke = StrokeStyle {
            width: shape.width,
            cap: if round { LineCap::Round } else { LineCap::Butt },
            join: if round {
                LineJoin::Round
            } else {
                LineJoin::Miter
            },
            miter_limit: 10.0,
            dash: None,
        };
        let outline = stroke_path(path, &stroke, m);
        fill_path_alpha(
            frame,
            &mut self.raster,
            &outline,
            &Matrix::IDENTITY,
            preview_rgb(color),
            alpha(shape.opacity),
        );
    }

    /// Cadre en pointillé, à la couleur d'accent : une zone qu'on trace ou
    /// qu'on tape.
    fn dashed_frame(&mut self, frame: &mut Frame<'_>, m: &Matrix, r: &Rect) {
        let scale = Self::page_scale(m);
        let mut path = Path::new();
        path.rect(r);
        let dash = 4.0 * self.dpi_scale / scale;
        let stroke = StrokeStyle {
            width: self.dpi_scale.max(1.0) / scale,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter_limit: 10.0,
            dash: Some((vec![dash, dash], 0.0)),
        };
        let outline = stroke_path(&path, &stroke, m);
        fill_path_alpha(
            frame,
            &mut self.raster,
            &outline,
            &Matrix::IDENTITY,
            self.theme.accent,
            255,
        );
    }

    /// Aperçu d'une forme en cours de tracé.
    fn paint_shape(
        &mut self,
        frame: &mut Frame<'_>,
        m: &Matrix,
        tool: AnnotTool,
        [from, to]: [Point; 2],
        style: &DrawStyle,
    ) {
        let shape = style.shape(tool);
        match tool {
            AnnotTool::Rectangle | AnnotTool::Ellipse => {
                let r = Rect::new(from.x, from.y, to.x, to.y);
                let half = shape.width / 2.0;
                let inner = Rect::new(
                    r.x0 + half.min(r.width() / 2.0),
                    r.y0 + half.min(r.height() / 2.0),
                    r.x1 - half.min(r.width() / 2.0),
                    r.y1 - half.min(r.height() / 2.0),
                );
                let mut path = Path::new();
                if tool == AnnotTool::Rectangle {
                    path.rect(&inner);
                } else {
                    ellipse_path(&mut path, &inner);
                }
                if let Some(f) = shape.fill {
                    fill_path_alpha(
                        frame,
                        &mut self.raster,
                        &path,
                        m,
                        preview_rgb(f),
                        alpha(shape.opacity),
                    );
                }
                self.stroke_preview(frame, &path, m, &shape, false);
            }
            AnnotTool::Line | AnnotTool::Arrow => {
                let end = if tool == AnnotTool::Arrow {
                    LineEnding::OpenArrow
                } else {
                    LineEnding::None
                };
                let stop = shapes::line_stop(to, from, shape.width, end);
                self.stroke_preview(frame, &polyline(&[from, stop]), m, &shape, true);
                self.paint_ending(frame, m, [to, from], &shape, end);
            }
            AnnotTool::TextBox => {
                self.dashed_frame(frame, m, &Rect::new(from.x, from.y, to.x, to.y));
            }
            AnnotTool::Callout => {
                // L'ancre et la ligne qui mène à la future zone.
                let line = ShapeStyle {
                    stroke: Some(style.border.unwrap_or(style.text_color)),
                    fill: None,
                    width: TEXT_BORDER,
                    opacity: 1.0,
                };
                self.stroke_preview(frame, &polyline(&[from, to]), m, &line, true);
                self.paint_ending(frame, m, [from, to], &line, LineEnding::OpenArrow);
                let height = freetext::fit_height(
                    "",
                    style.font,
                    style.text_size,
                    CALLOUT_WIDTH,
                    TEXT_BORDER,
                );
                self.dashed_frame(frame, m, &callout_box(from, to, true, height));
            }
            _ => {}
        }
    }

    /// Dessine une terminaison de ligne, avec la géométrie du moteur.
    fn paint_ending(
        &mut self,
        frame: &mut Frame<'_>,
        m: &Matrix,
        [tip, from]: [Point; 2],
        shape: &ShapeStyle,
        ending: LineEnding,
    ) {
        let Some(head) = shapes::ending_shape(tip, from, shape.width, ending) else {
            return;
        };
        match head {
            EndShape::Open(points) => {
                self.stroke_preview(frame, &polyline(&points), m, shape, true);
            }
            EndShape::Closed(points) => {
                let mut path = polyline(&points);
                path.close();
                if let Some(c) = shape.fill.or(shape.stroke) {
                    fill_path_alpha(
                        frame,
                        &mut self.raster,
                        &path,
                        m,
                        preview_rgb(c),
                        alpha(shape.opacity),
                    );
                }
                self.stroke_preview(frame, &path, m, shape, true);
            }
            EndShape::Disc(c, r) => {
                let mut path = Path::new();
                ellipse_path(&mut path, &Rect::new(c.x - r, c.y - r, c.x + r, c.y + r));
                if let Some(color) = shape.fill.or(shape.stroke) {
                    fill_path_alpha(
                        frame,
                        &mut self.raster,
                        &path,
                        m,
                        preview_rgb(color),
                        alpha(shape.opacity),
                    );
                }
            }
        }
    }

    /// Police d'écran de mêmes largeurs que la police standard choisie.
    fn draw_font(&mut self, font: StandardFont) -> Option<&mut TextRenderer> {
        // Chargée une fois par police, échec compris : cette fonction est
        // appelée pendant la peinture, à chaque image.
        if self.draw_text_font.as_ref().is_none_or(|(f, _)| *f != font) {
            self.draw_text_font = Some((font, TextRenderer::load(font_files(font))));
        }
        self.draw_text_font.as_mut().and_then(|(_, r)| r.as_mut())
    }

    /// La zone de texte : légende, fond, cadre, texte, et pendant la saisie
    /// la sélection, le curseur et un cadre d'accent.
    #[allow(clippy::too_many_lines)] // une zone, dessinée couche par couche
    fn paint_text_draft(
        &mut self,
        frame: &mut Frame<'_>,
        m: &Matrix,
        t: &TextDraft,
        style: &DrawStyle,
        editing: bool,
    ) {
        let scale = Self::page_scale(m);
        let border = style.text_border();
        if let Some(anchor) = t.anchor {
            let call = Callout {
                anchor,
                knee: None,
                ending: LineEnding::OpenArrow,
            };
            let points = freetext::callout_points(&t.rect, &call);
            let line = ShapeStyle {
                stroke: Some(style.border.unwrap_or(style.text_color)),
                fill: None,
                width: TEXT_BORDER,
                opacity: 1.0,
            };
            self.stroke_preview(frame, &polyline(&points), m, &line, true);
            if let Some(from) = points.get(1) {
                self.paint_ending(frame, m, [anchor, *from], &line, LineEnding::OpenArrow);
            }
        }
        let mut box_path = Path::new();
        box_path.rect(&t.rect);
        if let Some(f) = style.text_fill {
            fill_path_alpha(frame, &mut self.raster, &box_path, m, preview_rgb(f), 255);
        }
        if let Some((c, w)) = border {
            let mut inner = Path::new();
            inner.rect(&Rect::new(
                t.rect.x0 + w / 2.0,
                t.rect.y0 + w / 2.0,
                t.rect.x1 - w / 2.0,
                t.rect.y1 - w / 2.0,
            ));
            let shape = ShapeStyle {
                stroke: Some(c),
                fill: None,
                width: w,
                opacity: 1.0,
            };
            self.stroke_preview(frame, &inner, m, &shape, false);
        }
        if editing {
            let ring = 3.0 * self.dpi_scale / scale;
            self.dashed_frame(
                frame,
                m,
                &Rect::new(
                    t.rect.x0 - ring,
                    t.rect.y0 - ring,
                    t.rect.x1 + ring,
                    t.rect.y1 + ring,
                ),
            );
        }
        let (font, size) = (style.font, style.text_size);
        let pad = freetext::inner_padding(border.map_or(0.0, |(_, w)| w));
        let text = t.editor.buffer.text.clone();
        let lines = freetext::layout(&text, font, size, t.rect.width() - 2.0 * pad);
        let drop = freetext::first_baseline_drop(font, size);
        let pitch = LEADING * size;
        let left = t.rect.x0 + pad;
        let baseline = |i: usize| t.rect.y1 - pad - drop - pitch * i as f64;
        let size_px = (size * scale) as f32;
        let ink = preview_rgb(style.text_color);
        // La sélection, sous le texte.
        if editing && t.editor.buffer.has_selection() {
            let (a, b) = t.editor.buffer.range();
            for (i, line) in lines.iter().enumerate() {
                let (from, to) = (a.max(line.start), b.min(line.end));
                if from >= to && !(a <= line.start && b > line.end) {
                    continue;
                }
                let x_of = |k: usize| -> f64 {
                    text.chars()
                        .skip(line.start)
                        .take(k.saturating_sub(line.start))
                        .map(|c| font.char_width(c) * size / 1000.0)
                        .sum()
                };
                let (x0, x1) = (
                    left + x_of(from),
                    left + x_of(to).max(x_of(from) + size * 0.3),
                );
                let top = m.apply(Point::new(x0, baseline(i) + drop));
                let bottom = m.apply(Point::new(x1, baseline(i) + drop - pitch));
                round_rect_alpha(
                    frame,
                    top.x.min(bottom.x) as i32,
                    top.y.min(bottom.y) as i32,
                    (top.x - bottom.x).abs().ceil() as i32,
                    (top.y - bottom.y).abs().ceil() as i32,
                    0.0,
                    self.theme.accent,
                    0.35,
                );
            }
        }
        if let Some(renderer) = self.draw_font(font) {
            for (i, line) in lines.iter().enumerate() {
                if line.text.is_empty() {
                    continue;
                }
                let p = m.apply(Point::new(left, baseline(i)));
                renderer.draw(frame, p.x as f32, p.y as f32, size_px, &line.text, ink);
            }
        }
        // Le curseur : fixe, sans clignoter — un clignotement demanderait
        // un réveil régulier de la fenêtre, pour rien.
        if editing && !t.editor.buffer.has_selection() {
            let (row, offset) =
                freetext::caret_in(&lines, &text, font, size, t.editor.buffer.caret);
            let low = m.apply(Point::new(left + offset, baseline(row) - size * 0.2));
            let high = m.apply(Point::new(left + offset, baseline(row) + size * 0.85));
            let thick = (self.dpi_scale.round() as i32).max(1);
            let (top, tall) = (low.y.min(high.y), (low.y - high.y).abs());
            frame.fill_rect(
                low.x as i32,
                top as i32,
                thick,
                tall.ceil() as i32,
                ink.0,
                ink.1,
                ink.2,
            );
        }
    }
}

/// Recalcule la hauteur d'une zone de texte d'après son texte : le haut
/// reste où il est, la zone s'allonge vers le bas.
fn refit(t: &mut TextDraft, style: &DrawStyle) {
    let border = style.border.map_or(0.0, |_| TEXT_BORDER);
    let need = freetext::fit_height(
        &t.editor.buffer.text,
        style.font,
        style.text_size,
        t.rect.width(),
        border,
    );
    let height = need.max(t.min_height);
    t.rect = Rect::new(t.rect.x0, t.rect.y1 - height, t.rect.x1, t.rect.y1);
}

/// Un nombre sans décimales inutiles : « 2 », « 0,5 ».
fn number(v: f64) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if crate::ui::lang::english() {
        s.to_string()
    } else {
        s.replace('.', ",")
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::panic)] // valeurs exactes par construction
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn maj_contraint_les_formes_et_les_lignes() {
        let from = p(10.0, 10.0);
        // Rectangle 30 × 10 vers le bas-gauche : un carré de 30, mêmes signes.
        assert_eq!(
            constrain(from, p(-20.0, 0.0), AnnotTool::Rectangle, true),
            p(-20.0, -20.0)
        );
        assert_eq!(
            constrain(from, p(40.0, 20.0), AnnotTool::Ellipse, true),
            p(40.0, 40.0)
        );
        // Sans Maj, rien ne bouge.
        assert_eq!(
            constrain(from, p(40.0, 20.0), AnnotTool::Ellipse, false),
            p(40.0, 20.0)
        );
        // Ligne à 10° : à plat ; à 40° : à 45°.
        let flat = constrain(
            from,
            p(110.0, 10.0 + 100.0 * 10f64.to_radians().tan()),
            AnnotTool::Line,
            true,
        );
        assert_eq!(flat.y, 10.0);
        assert!(flat.x > 100.0);
        let diag = constrain(
            from,
            p(110.0, 10.0 + 100.0 * 40f64.to_radians().tan()),
            AnnotTool::Arrow,
            true,
        );
        assert!(((diag.x - 10.0) - (diag.y - 10.0)).abs() < 1e-9, "{diag:?}");
        let up = constrain(from, p(12.0, 90.0), AnnotTool::Line, true);
        assert_eq!(up.x, 10.0);
    }

    #[test]
    fn un_clic_pose_une_forme_visible() {
        let r = shape_rect(p(100.0, 100.0), p(100.0, 100.0), false);
        assert_eq!(r.width(), CLICK_SHAPE);
        assert!(r.contains(p(100.0, 100.0)));
        // Tracée à l'envers : la zone est remise dans l'ordre.
        let r = shape_rect(p(50.0, 80.0), p(10.0, 20.0), true);
        assert_eq!((r.x0, r.y0, r.x1, r.y1), (10.0, 20.0, 50.0, 80.0));
    }

    fn shape(tool: AnnotTool, moved: bool, to: Point) -> Draft {
        Draft::Shape {
            tool,
            page: 2,
            from: p(10.0, 10.0),
            to,
            shift: false,
            start: (0, 0),
            moved,
        }
    }

    #[test]
    fn le_brouillon_devient_la_bonne_annotation() {
        let style = DrawStyle::default();
        let Some((page, NewAnnotation::Line { end, .. })) =
            draft_annotation(&shape(AnnotTool::Arrow, true, p(90.0, 10.0)), &style)
        else {
            panic!("une flèche est une ligne");
        };
        assert_eq!(page, 2);
        assert_eq!(end, LineEnding::OpenArrow);
        // Une ligne d'un simple clic ne laisse rien.
        assert!(draft_annotation(&shape(AnnotTool::Line, false, p(10.0, 10.0)), &style).is_none());
        // Un rectangle d'un simple clic, si : un carré lisible.
        assert!(matches!(
            draft_annotation(&shape(AnnotTool::Rectangle, false, p(10.0, 10.0)), &style),
            Some((_, NewAnnotation::Square { .. }))
        ));
        // Le remplissage ne vaut que pour les formes fermées.
        let filled = DrawStyle {
            fill: Some([1.0, 1.0, 0.0]),
            ..style
        };
        let Some((_, NewAnnotation::Line { style: s, .. })) =
            draft_annotation(&shape(AnnotTool::Line, true, p(90.0, 10.0)), &filled)
        else {
            panic!("ligne attendue");
        };
        assert_eq!(s.fill, None);
        let empty_ink = Draft::Ink {
            page: 0,
            strokes: vec![Vec::new()],
            drawing: false,
        };
        assert!(draft_annotation(&empty_ink, &style).is_none());
        let text = |s: &str| {
            let mut editor = FieldEditor::new("", None, true, None, false);
            editor.insert(s);
            Draft::Text(Box::new(TextDraft {
                page: 0,
                rect: Rect::new(0.0, 0.0, 100.0, 20.0),
                min_height: 0.0,
                anchor: Some(p(-20.0, -20.0)),
                editor,
                selecting: false,
            }))
        };
        assert!(draft_annotation(&text("  \n "), &style).is_none());
        let Some((
            _,
            NewAnnotation::FreeText {
                text: t, callout, ..
            },
        )) = draft_annotation(&text("Bonjour\nà tous\n"), &style)
        else {
            panic!("zone de texte attendue");
        };
        assert_eq!(t, "Bonjour\nà tous");
        assert!(callout.is_some());
    }

    #[test]
    fn la_legende_s_eloigne_de_son_ancre() {
        let a = p(100.0, 100.0);
        let r = callout_box(a, p(200.0, 150.0), true, 20.0);
        assert_eq!((r.x0, r.y0), (200.0, 150.0));
        let r = callout_box(a, p(50.0, 60.0), true, 20.0);
        assert_eq!((r.x1, r.y1), (50.0, 60.0));
        let r = callout_box(a, a, false, 20.0);
        assert!(r.x0 > a.x && r.y0 > a.y);
    }

    #[test]
    fn reglages_aller_retour() {
        let style = DrawStyle {
            stroke: [0.0, 0.0, 1.0],
            fill: Some([1.0, 1.0, 0.0]),
            width: 4.0,
            opacity: 0.5,
            text_color: [0.2, 0.2, 0.2],
            text_size: 18.0,
            font: StandardFont::TimesBold,
            border: None,
            text_fill: Some([1.0, 1.0, 0.8]),
        };
        let back = DrawStyle::decode(&style.encode());
        assert_eq!(back.fill.map(hex), style.fill.map(hex));
        assert_eq!(back.width, 4.0);
        assert_eq!(back.opacity, 0.5);
        assert_eq!(back.font, StandardFont::TimesBold);
        assert_eq!(back.border, None);
        assert_eq!(hex(back.stroke), "0000FF");
        // Une chaîne abîmée garde les valeurs par défaut.
        let broken = DrawStyle::decode("width=beaucoup;stroke=zz;inconnue=1");
        assert_eq!(broken, DrawStyle::default());
    }

    #[test]
    fn chaque_outil_de_dessin_a_ses_reglages() {
        for tool in [
            AnnotTool::Rectangle,
            AnnotTool::Ellipse,
            AnnotTool::Line,
            AnnotTool::Arrow,
            AnnotTool::Pencil,
            AnnotTool::TextBox,
            AnnotTool::Callout,
        ] {
            assert!(tool.draws());
            assert!(!settings_for(tool).is_empty(), "{tool:?}");
            for s in settings_for(tool) {
                assert_ne!(tr(s.label()), "", "{s:?}");
            }
        }
        assert!(settings_for(AnnotTool::Highlight).is_empty());
        assert!(settings_for(AnnotTool::Rectangle).contains(&DrawSetting::Fill));
        assert!(!settings_for(AnnotTool::Line).contains(&DrawSetting::Fill));
    }
}
