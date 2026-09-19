//! Sources de couleur : couleur unie, dégradés axial et radial (ISO 32000-2
//! §8.7.4.5.3 et §8.7.4.5.4), image échantillonnée et fonction arbitraire.
//!
//! Le rasteriseur ne connaît qu'une opération : « quelle est la couleur au
//! centre du pixel `(x, y)` ? ». Pour éviter de recalculer des inverses de
//! matrices ou des pyramides d'images à chaque pixel, il construit d'abord un
//! [`PaintSampler`] via [`Paint::sampler`], puis appelle
//! [`PaintSampler::color_at`] pour chaque pixel couvert.

use std::fmt;
use std::rc::Rc;

use acrux_core::{Matrix, Point};

use crate::bitmap::Bitmap;
use crate::color::Color;

/// Dégradé axial (type 2 du PDF) ou radial (type 3).
///
/// Pour l'axial, `start` et `end` sont les extrémités de l'axe ; `r0` et `r1`
/// sont ignorés. Pour le radial, `(start, r0)` et `(end, r1)` sont les cercles
/// de départ et d'arrivée (§8.7.4.5.4). `transform` mène de l'espace du
/// dégradé à l'espace device.
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    /// Point de départ (centre du cercle de départ pour un radial).
    pub start: Point,
    /// Point d'arrivée (centre du cercle d'arrivée pour un radial).
    pub end: Point,
    /// Rayon de départ (radial uniquement).
    pub r0: f64,
    /// Rayon d'arrivée (radial uniquement).
    pub r1: f64,
    /// Arrêts `(position dans [0, 1], couleur)`, triés par position croissante.
    pub stops: Vec<(f64, Color)>,
    /// Étendre la première couleur avant `t = 0` (entrée `Extend[0]`).
    pub extend_start: bool,
    /// Étendre la dernière couleur après `t = 1` (entrée `Extend[1]`).
    pub extend_end: bool,
    /// Espace du dégradé → espace device.
    pub transform: Matrix,
}

impl Gradient {
    /// Dégradé axial entre deux points, sans extension, identité.
    #[must_use]
    pub fn linear(start: Point, end: Point, stops: Vec<(f64, Color)>) -> Self {
        Self {
            start,
            end,
            r0: 0.0,
            r1: 0.0,
            stops,
            extend_start: false,
            extend_end: false,
            transform: Matrix::IDENTITY,
        }
    }

    /// Dégradé radial entre deux cercles, sans extension, identité.
    #[must_use]
    pub fn radial(c0: Point, r0: f64, c1: Point, r1: f64, stops: Vec<(f64, Color)>) -> Self {
        Self {
            start: c0,
            end: c1,
            r0,
            r1,
            stops,
            extend_start: false,
            extend_end: false,
            transform: Matrix::IDENTITY,
        }
    }

    /// Couleur à la position `t` (interpolation linéaire entre arrêts, `t`
    /// borné à `[0, 1]`). Sans arrêt, transparent.
    #[must_use]
    pub fn color_at_offset(&self, t: f64) -> Color {
        let Some(first) = self.stops.first() else {
            return Color::TRANSPARENT;
        };
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
        if t <= first.0 {
            return first.1;
        }
        for pair in self.stops.windows(2) {
            let (t0, c0) = pair[0];
            let (t1, c1) = pair[1];
            if t <= t1 {
                let span = t1 - t0;
                if span <= 0.0 {
                    return c1;
                }
                #[allow(clippy::cast_possible_truncation)] // fraction dans [0, 1]
                let f = ((t - t0) / span) as f32;
                return c0.lerp(c1, f);
            }
        }
        self.stops.last().map_or(Color::TRANSPARENT, |s| s.1)
    }

    /// Paramètre `t` d'un point de l'espace du dégradé, ou `None` si le point
    /// n'est pas peint (hors de l'axe sans extension, ou pas de cercle radial).
    fn parameter(&self, p: Point, radial: bool) -> Option<f64> {
        let t = if radial {
            self.radial_parameter(p)?
        } else {
            self.linear_parameter(p)?
        };
        if t < 0.0 && !self.extend_start {
            return None;
        }
        if t > 1.0 && !self.extend_end {
            return None;
        }
        Some(t.clamp(0.0, 1.0))
    }

    /// Projection orthogonale sur l'axe (§8.7.4.5.3, formule de `x'`).
    fn linear_parameter(&self, p: Point) -> Option<f64> {
        let dx = self.end.x - self.start.x;
        let dy = self.end.y - self.start.y;
        let len2 = dx * dx + dy * dy;
        if !len2.is_finite() || len2 <= 0.0 {
            // Axe dégénéré : tout le plan prend la couleur de départ.
            return Some(0.0);
        }
        let t = ((p.x - self.start.x) * dx + (p.y - self.start.y) * dy) / len2;
        t.is_finite().then_some(t)
    }

    /// Résolution de `s` pour un radial (§8.7.4.5.4) : le point `p` est sur le
    /// cercle `c(s), r(s)` avec `c(s) = c0 + s·(c1 − c0)`, `r(s) = r0 + s·(r1 − r0)`.
    /// On garde la plus grande racine telle que `r(s) ≥ 0` et que `s` soit
    /// admissible compte tenu des extensions.
    #[allow(clippy::many_single_char_names)] // coefficients a, b, c de l'équation
    fn radial_parameter(&self, p: Point) -> Option<f64> {
        let cdx = self.end.x - self.start.x;
        let cdy = self.end.y - self.start.y;
        let dr = self.r1 - self.r0;
        let pdx = p.x - self.start.x;
        let pdy = p.y - self.start.y;
        let a = cdx * cdx + cdy * cdy - dr * dr;
        let b = pdx * cdx + pdy * cdy + self.r0 * dr;
        let c = pdx * pdx + pdy * pdy - self.r0 * self.r0;
        let admissible = |s: f64| -> bool {
            s.is_finite()
                && self.r0 + s * dr >= 0.0
                && (s >= 0.0 || self.extend_start)
                && (s <= 1.0 || self.extend_end)
        };
        if a.abs() < 1e-9 {
            // Équation linéaire : 2·b·s = c.
            if b.abs() < 1e-12 {
                return None;
            }
            let s = c / (2.0 * b);
            return admissible(s).then_some(s);
        }
        let disc = b * b - a * c;
        if disc < 0.0 {
            return None;
        }
        let sq = disc.sqrt();
        let s1 = (b + sq) / a;
        let s2 = (b - sq) / a;
        let (hi, lo) = if s1 >= s2 { (s1, s2) } else { (s2, s1) };
        if admissible(hi) {
            Some(hi)
        } else if admissible(lo) {
            Some(lo)
        } else {
            None
        }
    }
}

/// Image utilisée comme source de couleur.
///
/// L'espace image unité `(u, v)` va de `(0, 0)` (coin supérieur gauche, début
/// de la première ligne) à `(1, 1)` (coin inférieur droit). `transform` mène
/// de cet espace à l'espace device ; c'est à l'appelant d'y inclure le
/// retournement vertical du PDF (§8.9.4 : la ligne 0 est en haut du carré unité).
#[derive(Debug, Clone)]
pub struct ImagePaint {
    /// Image source (prémultipliée).
    pub bitmap: Rc<Bitmap>,
    /// Espace image unité → espace device.
    pub transform: Matrix,
    /// Interpolation bilinéaire à l'agrandissement (`/Interpolate`) ; sinon
    /// plus proche voisin. La réduction est toujours filtrée par moyenne.
    pub interpolate: bool,
}

/// Source de couleur d'un remplissage.
pub enum Paint {
    /// Couleur uniforme.
    Solid(Color),
    /// Dégradé axial.
    LinearGradient(Gradient),
    /// Dégradé radial.
    RadialGradient(Gradient),
    /// Image.
    Image(ImagePaint),
    /// Fonction `(x, y) device → couleur`, pour les ombrages exotiques.
    Callback(Box<dyn Fn(f64, f64) -> Color>),
}

impl fmt::Debug for Paint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Paint::Solid(c) => f.debug_tuple("Solid").field(c).finish(),
            Paint::LinearGradient(g) => f.debug_tuple("LinearGradient").field(g).finish(),
            Paint::RadialGradient(g) => f.debug_tuple("RadialGradient").field(g).finish(),
            Paint::Image(i) => f.debug_tuple("Image").field(i).finish(),
            Paint::Callback(_) => f.write_str("Callback(..)"),
        }
    }
}

impl From<Color> for Paint {
    fn from(c: Color) -> Self {
        Paint::Solid(c)
    }
}

impl Paint {
    /// Couleur au point device `(x, y)`. Pratique pour les tests ; le
    /// rasteriseur passe par [`Paint::sampler`] pour ne pas refaire les
    /// précalculs à chaque pixel.
    #[must_use]
    pub fn color_at(&self, x: f64, y: f64) -> Color {
        self.sampler().color_at(x, y)
    }

    /// Prépare l'échantillonnage (inverse de matrice, pyramide d'image).
    #[must_use]
    pub fn sampler(&self) -> PaintSampler<'_> {
        let kind = match self {
            Paint::Solid(c) => SamplerKind::Solid(c.clamped()),
            Paint::LinearGradient(g) => SamplerKind::Gradient {
                gradient: g,
                inverse: g.transform.invert(),
                radial: false,
            },
            Paint::RadialGradient(g) => SamplerKind::Gradient {
                gradient: g,
                inverse: g.transform.invert(),
                radial: true,
            },
            Paint::Image(img) => SamplerKind::Image(ImageSampler::new(img)),
            Paint::Callback(f) => SamplerKind::Callback(f.as_ref()),
        };
        PaintSampler { kind }
    }

    /// Vrai si la source est une couleur unie opaque (chemin rapide du rasteriseur).
    #[must_use]
    pub fn solid_color(&self) -> Option<Color> {
        match self {
            Paint::Solid(c) => Some(c.clamped()),
            _ => None,
        }
    }
}

/// Échantillonneur préparé à partir d'un [`Paint`].
pub struct PaintSampler<'a> {
    kind: SamplerKind<'a>,
}

enum SamplerKind<'a> {
    Solid(Color),
    Gradient {
        gradient: &'a Gradient,
        inverse: Option<Matrix>,
        radial: bool,
    },
    Image(ImageSampler<'a>),
    Callback(&'a dyn Fn(f64, f64) -> Color),
}

impl PaintSampler<'_> {
    /// Couleur (non prémultipliée, bornée) au point device `(x, y)`.
    #[must_use]
    pub fn color_at(&self, x: f64, y: f64) -> Color {
        match &self.kind {
            SamplerKind::Solid(c) => *c,
            SamplerKind::Gradient {
                gradient,
                inverse,
                radial,
            } => {
                let Some(inv) = inverse else {
                    return Color::TRANSPARENT;
                };
                let p = inv.apply(Point::new(x, y));
                if !p.x.is_finite() || !p.y.is_finite() {
                    return Color::TRANSPARENT;
                }
                gradient
                    .parameter(p, *radial)
                    .map_or(Color::TRANSPARENT, |t| {
                        gradient.color_at_offset(t).clamped()
                    })
            }
            SamplerKind::Image(s) => s.color_at(x, y),
            SamplerKind::Callback(f) => f(x, y).clamped(),
        }
    }
}

/// Demi-largeur maximale (en pixels du niveau choisi) de la boîte de moyenne.
const MAX_BOX_HALF: f64 = 4.0;

/// Échantillonneur d'image : inverse de la transformation, pyramide de
/// niveaux réduits de moitié (mipmap) et taille de la boîte de moyenne.
struct ImageSampler<'a> {
    source: &'a Bitmap,
    /// Niveaux réduits (le niveau 0 est `source`, non dupliqué).
    levels: Vec<Bitmap>,
    /// Niveau retenu (0 = image d'origine).
    level: usize,
    /// Device → pixels du niveau retenu.
    inverse: Option<Matrix>,
    /// Demi-largeur et demi-hauteur de la boîte de moyenne, en pixels du niveau.
    half: (f64, f64),
    /// Plus proche voisin (pas de filtrage).
    nearest: bool,
}

impl<'a> ImageSampler<'a> {
    fn new(img: &'a ImagePaint) -> Self {
        let bmp: &Bitmap = &img.bitmap;
        let w = f64::from(bmp.width());
        let h = f64::from(bmp.height());
        let mut sampler = Self {
            source: bmp,
            levels: Vec::new(),
            level: 0,
            inverse: None,
            half: (0.5, 0.5),
            nearest: !img.interpolate,
        };
        if bmp.is_empty() {
            return sampler;
        }
        // Unité → pixels de l'image, puis → device.
        let unit_to_px = Matrix::scale(1.0 / w, 1.0 / h);
        let px_to_device = unit_to_px.then(&img.transform);
        let Some(inv) = px_to_device.invert() else {
            return sampler;
        };
        // Empreinte d'un pixel device dans l'image, sur chaque axe.
        let fx = (inv.a * inv.a + inv.b * inv.b).sqrt();
        let fy = (inv.c * inv.c + inv.d * inv.d).sqrt();
        if !fx.is_finite() || !fy.is_finite() {
            return sampler;
        }
        let footprint = fx.min(fy);
        // Niveau tel que l'empreinte minimale reste entre 1 et 2 pixels.
        let mut level = 0usize;
        let mut scale = 1.0f64;
        let mut cur_w = bmp.width();
        let mut cur_h = bmp.height();
        while footprint / scale >= 2.0 && cur_w > 1 && cur_h > 1 && level < 16 {
            let next = downsample_half(sampler.levels.last().unwrap_or(bmp));
            cur_w = next.width();
            cur_h = next.height();
            sampler.levels.push(next);
            level += 1;
            scale *= 2.0;
        }
        sampler.level = level;
        sampler.inverse = Some(inv.then(&Matrix::scale(1.0 / scale, 1.0 / scale)));
        let hx = (fx / scale / 2.0).clamp(0.5, MAX_BOX_HALF);
        let hy = (fy / scale / 2.0).clamp(0.5, MAX_BOX_HALF);
        sampler.half = (hx, hy);
        // Plus proche voisin uniquement à l'agrandissement (ou taille égale).
        sampler.nearest = !img.interpolate && fx <= 1.0 + 1e-9 && fy <= 1.0 + 1e-9;
        sampler
    }

    fn bitmap(&self) -> &Bitmap {
        if self.level == 0 {
            self.source
        } else {
            &self.levels[self.level - 1]
        }
    }

    fn color_at(&self, x: f64, y: f64) -> Color {
        let Some(inv) = self.inverse else {
            return Color::TRANSPARENT;
        };
        let p = inv.apply(Point::new(x, y));
        if !p.x.is_finite() || !p.y.is_finite() {
            return Color::TRANSPARENT;
        }
        let bmp = self.bitmap();
        let px = if self.nearest {
            let ix = clamp_index(p.x.floor(), bmp.width());
            let iy = clamp_index(p.y.floor(), bmp.height());
            bmp.pixel(ix, iy).unwrap_or([0; 4]).map(f32::from)
        } else {
            box_average(bmp, p.x, p.y, self.half.0, self.half.1)
        };
        Color::from_premultiplied_rgba8([
            crate::color::to_u8(px[0] / 255.0),
            crate::color::to_u8(px[1] / 255.0),
            crate::color::to_u8(px[2] / 255.0),
            crate::color::to_u8(px[3] / 255.0),
        ])
    }
}

/// Ramène un indice flottant dans `[0, len)` (les bords sont prolongés).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // valeur bornée
fn clamp_index(v: f64, len: u32) -> u32 {
    if len == 0 {
        return 0;
    }
    let v = if v.is_nan() { 0.0 } else { v };
    v.clamp(0.0, f64::from(len - 1)) as u32
}

/// Moyenne pondérée par l'aire des pixels recouverts par la boîte
/// `[cx − hx, cx + hx] × [cy − hy, cy + hy]` (coordonnées pixel, coins sur les
/// entiers). Une boîte de demi-largeur 0,5 équivaut à une interpolation
/// bilinéaire ; une boîte plus large réalise la moyenne de réduction. Les
/// pixels au-delà des bords prolongent le bord.
fn box_average(bmp: &Bitmap, cx: f64, cy: f64, hx: f64, hy: f64) -> [f32; 4] {
    let (w, h) = (bmp.width(), bmp.height());
    if w == 0 || h == 0 {
        return [0.0; 4];
    }
    let x0 = cx - hx;
    let x1 = cx + hx;
    let y0 = cy - hy;
    let y1 = cy + hy;
    let mut acc = [0.0f32; 4];
    let mut total = 0.0f32;
    let mut fy = y0.floor();
    while fy < y1 {
        let wy = (y1.min(fy + 1.0) - y0.max(fy)).max(0.0);
        let iy = clamp_index(fy, h);
        let mut fx = x0.floor();
        while fx < x1 {
            let wx = (x1.min(fx + 1.0) - x0.max(fx)).max(0.0);
            let ix = clamp_index(fx, w);
            #[allow(clippy::cast_possible_truncation)] // poids dans [0, 1]
            let weight = (wx * wy) as f32;
            if let Some(px) = bmp.pixel(ix, iy) {
                for (a, v) in acc.iter_mut().zip(px) {
                    *a += f32::from(v) * weight;
                }
            }
            total += weight;
            fx += 1.0;
        }
        fy += 1.0;
    }
    if total <= 0.0 {
        return [0.0; 4];
    }
    acc.map(|v| v / total)
}

/// Réduction de moitié par moyenne de blocs 2×2 (les dimensions impaires
/// gardent une dernière ligne/colonne moyennée sur les pixels disponibles).
#[must_use]
pub(crate) fn downsample_half(src: &Bitmap) -> Bitmap {
    let w = src.width().div_ceil(2).max(1);
    let h = src.height().div_ceil(2).max(1);
    let mut out = Bitmap::new(w, h);
    if src.is_empty() {
        return out;
    }
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for dy in 0..2 {
                for dx in 0..2 {
                    if let Some(px) = src.pixel(x * 2 + dx, y * 2 + dy) {
                        for (a, v) in acc.iter_mut().zip(px) {
                            *a += u32::from(v);
                        }
                        n += 1;
                    }
                }
            }
            let n = n.max(1);
            let px = acc.map(|v| u8::try_from((v + n / 2) / n).unwrap_or(255));
            out.set_pixel(x, y, px);
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::float_cmp)]
mod tests {
    use super::*;

    fn bw_stops() -> Vec<(f64, Color)> {
        vec![(0.0, Color::BLACK), (1.0, Color::WHITE)]
    }

    #[test]
    fn linear_gradient_increases_along_axis() {
        let g = Gradient::linear(Point::new(0.0, 0.0), Point::new(10.0, 0.0), bw_stops());
        let paint = Paint::LinearGradient(g);
        let mut prev = -1.0;
        for x in 0..10 {
            let c = paint.color_at(f64::from(x) + 0.5, 3.0);
            assert!(c.r > prev, "x = {x} : {} <= {prev}", c.r);
            assert_eq!(c.r, c.g);
            prev = c.r;
        }
        // Hors de l'axe sans extension : transparent ; avec extension : bornes.
        assert_eq!(paint.color_at(-1.0, 0.0), Color::TRANSPARENT);
        assert_eq!(paint.color_at(11.0, 0.0), Color::TRANSPARENT);
        let mut g = Gradient::linear(Point::new(0.0, 0.0), Point::new(10.0, 0.0), bw_stops());
        g.extend_start = true;
        g.extend_end = true;
        let paint = Paint::LinearGradient(g);
        assert_eq!(paint.color_at(-5.0, 0.0), Color::BLACK);
        assert_eq!(paint.color_at(50.0, 0.0), Color::WHITE);
    }

    #[test]
    fn gradient_transform_and_stops() {
        let mut g = Gradient::linear(Point::new(0.0, 0.0), Point::new(1.0, 0.0), bw_stops());
        g.transform = Matrix::scale(100.0, 1.0);
        let paint = Paint::LinearGradient(g);
        assert!((paint.color_at(50.0, 0.0).r - 0.5).abs() < 0.02);
        // Arrêts multiples et dégénérés.
        let g = Gradient::linear(
            Point::new(0.0, 0.0),
            Point::new(1.0, 0.0),
            vec![
                (0.0, Color::BLACK),
                (0.5, Color::rgb(1.0, 0.0, 0.0)),
                (0.5, Color::rgb(0.0, 0.0, 1.0)),
                (1.0, Color::WHITE),
            ],
        );
        assert!((g.color_at_offset(0.25).r - 0.5).abs() < 1e-6);
        assert_eq!(g.color_at_offset(0.75).b, 1.0);
        assert_eq!(g.color_at_offset(f64::NAN), Color::BLACK);
        let empty = Gradient::linear(Point::new(0.0, 0.0), Point::new(1.0, 0.0), vec![]);
        assert_eq!(empty.color_at_offset(0.5), Color::TRANSPARENT);
        // Axe dégénéré : couleur de départ partout, pas de panique.
        let degenerate = Gradient::linear(Point::new(1.0, 1.0), Point::new(1.0, 1.0), bw_stops());
        assert_eq!(
            Paint::LinearGradient(degenerate).color_at(7.0, 7.0),
            Color::BLACK
        );
    }

    #[test]
    fn radial_gradient_follows_pdf_definition() {
        // Cercles concentriques : t est proportionnel à la distance au centre.
        let g = Gradient::radial(
            Point::new(0.0, 0.0),
            0.0,
            Point::new(0.0, 0.0),
            10.0,
            bw_stops(),
        );
        let paint = Paint::RadialGradient(g.clone());
        assert!((paint.color_at(5.0, 0.0).r - 0.5).abs() < 0.01);
        assert!((paint.color_at(0.0, 0.0).r).abs() < 0.01);
        assert_eq!(paint.color_at(20.0, 0.0), Color::TRANSPARENT);
        let mut ext = g;
        ext.extend_end = true;
        assert_eq!(Paint::RadialGradient(ext).color_at(20.0, 0.0), Color::WHITE);
        // Cercles décalés (cône) : le point sur le cercle final a t = 1.
        let g = Gradient::radial(
            Point::new(0.0, 0.0),
            1.0,
            Point::new(10.0, 0.0),
            3.0,
            bw_stops(),
        );
        let paint = Paint::RadialGradient(g);
        assert!((paint.color_at(13.0, 0.0).r - 1.0).abs() < 0.01);
        assert!((paint.color_at(-1.0, 0.0).r).abs() < 0.01);
        // Cas linéaire (a ≈ 0) : cercles de même rayon translatés de |dr| = 0.
        let g = Gradient::radial(
            Point::new(0.0, 0.0),
            5.0,
            Point::new(0.0, 0.0),
            5.0,
            bw_stops(),
        );
        let _ = Paint::RadialGradient(g).color_at(1.0, 1.0);
        // Coordonnées absurdes : pas de panique.
        let g = Gradient::radial(
            Point::new(0.0, 0.0),
            0.0,
            Point::new(0.0, 0.0),
            10.0,
            bw_stops(),
        );
        let _ = Paint::RadialGradient(g).color_at(f64::NAN, f64::INFINITY);
    }

    #[test]
    fn image_nearest_magnified_gives_sharp_blocks() {
        let mut src = Bitmap::new(2, 2);
        src.set_pixel(0, 0, [255, 0, 0, 255]);
        src.set_pixel(1, 0, [0, 255, 0, 255]);
        src.set_pixel(0, 1, [0, 0, 255, 255]);
        src.set_pixel(1, 1, [255, 255, 255, 255]);
        let paint = Paint::Image(ImagePaint {
            bitmap: Rc::new(src),
            transform: Matrix::scale(20.0, 20.0),
            interpolate: false,
        });
        let sampler = paint.sampler();
        for y in 0..20 {
            for x in 0..20 {
                let c = sampler.color_at(f64::from(x) + 0.5, f64::from(y) + 0.5);
                let expected = match (x < 10, y < 10) {
                    (true, true) => Color::rgb(1.0, 0.0, 0.0),
                    (false, true) => Color::rgb(0.0, 1.0, 0.0),
                    (true, false) => Color::rgb(0.0, 0.0, 1.0),
                    (false, false) => Color::WHITE,
                };
                assert_eq!(c, expected, "pixel ({x}, {y})");
            }
        }
        // Avec interpolation, le centre est un mélange.
        let Paint::Image(img) = paint else { panic!() };
        let smooth = Paint::Image(ImagePaint {
            interpolate: true,
            ..img
        });
        let c = smooth.color_at(10.0, 10.0);
        assert!(c.r > 0.2 && c.r < 0.8 && c.g > 0.2 && c.g < 0.8);
    }

    #[test]
    fn image_minified_is_averaged() {
        // Damier 100×100 noir/blanc réduit en 10×10 : gris moyen partout.
        let mut src = Bitmap::new(100, 100);
        for y in 0..100 {
            for x in 0..100 {
                let v = if (x + y) % 2 == 0 { 255 } else { 0 };
                src.set_pixel(x, y, [v, v, v, 255]);
            }
        }
        for interpolate in [false, true] {
            let paint = Paint::Image(ImagePaint {
                bitmap: Rc::new(src.clone()),
                transform: Matrix::scale(10.0, 10.0),
                interpolate,
            });
            let sampler = paint.sampler();
            for y in 0..10 {
                for x in 0..10 {
                    let c = sampler.color_at(f64::from(x) + 0.5, f64::from(y) + 0.5);
                    assert!((c.r - 0.5).abs() < 0.03, "({x}, {y}) : {}", c.r);
                    assert!((c.a - 1.0).abs() < 1e-6);
                }
            }
        }
    }

    #[test]
    fn downsample_half_handles_odd_sizes() {
        let mut src = Bitmap::new(3, 3);
        src.fill(Color::WHITE);
        let half = downsample_half(&src);
        assert_eq!((half.width(), half.height()), (2, 2));
        assert_eq!(half.pixel(1, 1), Some([255; 4]));
        assert_eq!(downsample_half(&Bitmap::new(1, 1)).width(), 1);
        assert_eq!(downsample_half(&Bitmap::new(0, 0)).width(), 1);
    }

    #[test]
    fn degenerate_images_and_callback_do_not_panic() {
        let paint = Paint::Image(ImagePaint {
            bitmap: Rc::new(Bitmap::new(0, 0)),
            transform: Matrix::IDENTITY,
            interpolate: true,
        });
        assert_eq!(paint.color_at(1.0, 1.0), Color::TRANSPARENT);
        let singular = Paint::Image(ImagePaint {
            bitmap: Rc::new(Bitmap::new(4, 4)),
            transform: Matrix::scale(0.0, 0.0),
            interpolate: true,
        });
        assert_eq!(singular.color_at(1.0, 1.0), Color::TRANSPARENT);
        #[allow(clippy::cast_possible_truncation)] // test
        let cb = Paint::Callback(Box::new(|x, _| Color::rgb(x as f32, 5.0, f32::NAN)));
        assert_eq!(cb.color_at(0.5, 0.0), Color::rgb(0.5, 1.0, 0.0));
        assert!(format!("{cb:?}").contains("Callback"));
        assert_eq!(Paint::from(Color::WHITE).solid_color(), Some(Color::WHITE));
    }
}
