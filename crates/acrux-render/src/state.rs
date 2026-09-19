//! État graphique (ISO 32000-2 §8.4) et état texte (§9.3).
//!
//! L'état est empilé par `q`/`Q`. Le chemin en construction et le chemin de
//! découpe en attente (`W`/`W*`) vivent dans l'interpréteur, pas ici.

use std::rc::Rc;

use acrux_core::Matrix;
use acrux_graphics::{BlendMode, LineCap, LineJoin, Mask};

use crate::colorspace::ColorSpace;
use crate::font::LoadedFont;

/// Couleur courante : espace, composantes, et motif éventuel.
#[derive(Debug, Clone)]
pub struct ColorState {
    /// Espace colorimétrique.
    pub space: ColorSpace,
    /// Composantes dans cet espace.
    pub components: Vec<f64>,
    /// Nom du motif (`/Pattern` : `scn /P1`), résolu à l'usage.
    pub pattern: Option<Vec<u8>>,
}

impl ColorState {
    /// Noir dans l'espace gris.
    #[must_use]
    pub fn black() -> Self {
        Self {
            space: ColorSpace::DeviceGray,
            components: vec![0.0],
            pattern: None,
        }
    }

    /// Couleur RVB (0..1) pour le rasteriseur.
    #[must_use]
    pub fn rgb(&self) -> [f32; 3] {
        self.space.to_rgb(&self.components)
    }
}

/// Mode de rendu du texte (§9.3.6, table 104).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextRenderMode {
    /// 0 : remplissage.
    Fill,
    /// 1 : contour.
    Stroke,
    /// 2 : remplissage puis contour.
    FillStroke,
    /// 3 : invisible.
    Invisible,
    /// 4 : remplissage + ajout au chemin de découpe.
    FillClip,
    /// 5 : contour + découpe.
    StrokeClip,
    /// 6 : remplissage + contour + découpe.
    FillStrokeClip,
    /// 7 : découpe seulement.
    Clip,
}

impl TextRenderMode {
    /// Depuis l'opérande de `Tr`.
    #[must_use]
    pub fn from_i64(v: i64) -> Self {
        match v {
            1 => Self::Stroke,
            2 => Self::FillStroke,
            3 => Self::Invisible,
            4 => Self::FillClip,
            5 => Self::StrokeClip,
            6 => Self::FillStrokeClip,
            7 => Self::Clip,
            _ => Self::Fill,
        }
    }

    /// Le mode remplit les glyphes.
    #[must_use]
    pub fn fills(self) -> bool {
        matches!(
            self,
            Self::Fill | Self::FillStroke | Self::FillClip | Self::FillStrokeClip
        )
    }

    /// Le mode trace les contours.
    #[must_use]
    pub fn strokes(self) -> bool {
        matches!(
            self,
            Self::Stroke | Self::FillStroke | Self::StrokeClip | Self::FillStrokeClip
        )
    }

    /// Le mode ajoute les glyphes au chemin de découpe.
    #[must_use]
    pub fn clips(self) -> bool {
        matches!(
            self,
            Self::FillClip | Self::StrokeClip | Self::FillStrokeClip | Self::Clip
        )
    }
}

/// État texte (§9.3.1).
#[derive(Debug, Clone)]
pub struct TextState {
    /// Police courante.
    pub font: Option<Rc<LoadedFont>>,
    /// Taille (`Tf`).
    pub size: f64,
    /// Espacement des caractères `Tc`.
    pub char_spacing: f64,
    /// Espacement des mots `Tw`.
    pub word_spacing: f64,
    /// Échelle horizontale `Tz` (1 = 100 %).
    pub horizontal_scale: f64,
    /// Interligne `TL`.
    pub leading: f64,
    /// Élévation `Ts`.
    pub rise: f64,
    /// Mode de rendu `Tr`.
    pub render_mode: TextRenderMode,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            font: None,
            size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 1.0,
            leading: 0.0,
            rise: 0.0,
            render_mode: TextRenderMode::Fill,
        }
    }
}

/// Masque souple courant (`/SMask` de l'ExtGState), déjà rasterisé en
/// luminosité ou alpha dans l'espace device.
#[derive(Debug, Clone)]
pub struct SoftMask {
    /// Couverture par pixel.
    pub mask: Rc<Mask>,
}

/// État graphique complet.
#[derive(Debug, Clone)]
pub struct GraphicsState {
    /// Matrice de transformation courante (espace utilisateur → device).
    pub ctm: Matrix,
    /// Couleur de tracé.
    pub stroke: ColorState,
    /// Couleur de remplissage.
    pub fill: ColorState,
    /// Épaisseur de trait `w`.
    pub line_width: f64,
    /// Extrémités `J`.
    pub line_cap: LineCap,
    /// Joints `j`.
    pub line_join: LineJoin,
    /// Limite de pointe `M`.
    pub miter_limit: f64,
    /// Tirets `d` : motif et phase.
    pub dash: Option<(Vec<f64>, f64)>,
    /// Alpha constant de tracé `CA`.
    pub stroke_alpha: f64,
    /// Alpha constant de remplissage `ca`.
    pub fill_alpha: f64,
    /// Mode de fusion `BM`.
    pub blend: BlendMode,
    /// Masque souple `SMask`.
    pub soft_mask: Option<SoftMask>,
    /// Chemin de découpe cumulé (`None` = tout est visible).
    pub clip: Option<Rc<Mask>>,
    /// Surimpression `OP`/`op` (informatif pour l'instant).
    pub overprint: bool,
    /// Lissage `SA` (informatif).
    pub stroke_adjust: bool,
    /// État texte.
    pub text: TextState,
}

impl GraphicsState {
    /// État initial pour une page dont la matrice de base est `ctm`.
    #[must_use]
    pub fn new(ctm: Matrix) -> Self {
        Self {
            ctm,
            stroke: ColorState::black(),
            fill: ColorState::black(),
            line_width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash: None,
            stroke_alpha: 1.0,
            fill_alpha: 1.0,
            blend: BlendMode::Normal,
            soft_mask: None,
            clip: None,
            overprint: false,
            stroke_adjust: false,
            text: TextState::default(),
        }
    }

    /// Intersecte le chemin de découpe courant avec un nouveau masque.
    pub fn intersect_clip(&mut self, mask: Mask) {
        self.clip = Some(Rc::new(match &self.clip {
            Some(existing) => existing.intersect(&mask),
            None => mask,
        }));
    }

    /// Masque effectif combinant découpe et masque souple, s'il y en a un.
    #[must_use]
    pub fn effective_clip(&self) -> Option<Rc<Mask>> {
        match (&self.clip, &self.soft_mask) {
            (None, None) => None,
            (Some(c), None) => Some(Rc::clone(c)),
            (None, Some(s)) => Some(Rc::clone(&s.mask)),
            (Some(c), Some(s)) => Some(Rc::new(c.intersect(&s.mask))),
        }
    }
}
