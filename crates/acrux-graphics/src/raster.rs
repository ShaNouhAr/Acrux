//! Rasteriseur scanline anti-aliasé.
//!
//! # Méthode
//!
//! Chaque ligne de pixels est découpée en [`SUBSAMPLES`] sous-lignes
//! horizontales. Pour chaque sous-ligne, on calcule les intersections des
//! arêtes actives avec l'horizontale passant par son centre (liste de
//! croisements triée en `x`, chacun portant sa direction ±1), puis on déduit
//! les intervalles intérieurs selon la règle de remplissage (ISO 32000-2
//! §8.5.3.3.2 non-zero, §8.5.3.3.3 even-odd). Chaque intervalle ajoute à un
//! tampon de couverture une contribution **exacte horizontalement** : les
//! pixels partiellement recouverts aux extrémités reçoivent la fraction
//! exacte, les pixels intérieurs sont marqués par un tampon différentiel
//! (`+w` au début, `−w` à la fin) intégré une fois par ligne.
//!
//! Ce compromis — exact en `x`, [`SUBSAMPLES`] niveaux en `y` — traite les
//! deux règles de remplissage avec le même code et sans approximation autre
//! que l'échantillonnage vertical. Une arête horizontale située à une
//! fraction de pixel reçoit donc une couverture quantifiée au 1/[`SUBSAMPLES`].
//!
//! # Coordonnées
//!
//! Le rasteriseur travaille en pixels, origine en haut à gauche, `y` vers le
//! bas ; le centre du pixel `(i, j)` est `(i + 0.5, j + 0.5)`. C'est à
//! l'appelant de fournir la matrice qui retourne l'axe `y` du PDF.

use acrux_core::{Matrix, Path, Point};

use crate::bitmap::{Bitmap, Mask};
use crate::color::{composite_pixel, to_u8, unit, BlendMode};
use crate::paint::{Paint, PaintSampler};

/// Nombre de sous-lignes par ligne de pixels.
pub const SUBSAMPLES: usize = 16;

/// Tolérance d'aplatissement des courbes, en pixels device.
pub const FLATTEN_TOLERANCE: f64 = 0.1;

/// Au-delà de cette valeur absolue, une coordonnée est considérée comme
/// corrompue et le chemin entier est ignoré.
pub const COORD_LIMIT: f64 = 1.0e9;

/// Règle de remplissage (§8.5.3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillRule {
    /// Règle du nombre d'enroulements non nul (`f`, `B`).
    #[default]
    NonZero,
    /// Règle pair-impair (`f*`, `B*`).
    EvenOdd,
}

impl FillRule {
    #[inline]
    fn is_inside(self, winding: i32) -> bool {
        match self {
            FillRule::NonZero => winding != 0,
            FillRule::EvenOdd => winding & 1 != 0,
        }
    }
}

/// Arête non horizontale, orientée vers le bas (`y0 < y1`).
#[derive(Debug, Clone, Copy)]
struct Edge {
    y0: f64,
    y1: f64,
    /// `x` à `y0`.
    x0: f64,
    /// Variation de `x` par unité de `y`.
    dxdy: f64,
    /// +1 si l'arête d'origine descendait, −1 si elle montait.
    dir: i32,
}

/// Croisement d'une sous-ligne avec une arête.
#[derive(Debug, Clone, Copy)]
struct Crossing {
    x: f64,
    dir: i32,
}

/// Rasteriseur réutilisable : conserve ses tampons entre deux appels pour
/// éviter toute allocation par pixel ou par chemin.
#[derive(Debug, Default)]
pub struct Rasterizer {
    edges: Vec<Edge>,
    active: Vec<usize>,
    crossings: Vec<Crossing>,
    /// Couverture directe des pixels partiels (largeur + 2).
    cover: Vec<f32>,
    /// Tampon différentiel des pixels intérieurs (largeur + 2).
    delta: Vec<f32>,
    /// Couverture intégrée d'une ligne (largeur).
    row: Vec<f32>,
}

impl Rasterizer {
    /// Rasteriseur sans tampon alloué.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Remplit un chemin dans `target`. Voir [`fill_path`].
    #[allow(clippy::too_many_arguments)] // signature imposée par l'API publique
    pub fn fill_path(
        &mut self,
        target: &mut Bitmap,
        path: &Path,
        transform: &Matrix,
        rule: FillRule,
        paint: &Paint,
        clip: Option<&Mask>,
        alpha: f32,
        blend: BlendMode,
    ) {
        let alpha = unit(alpha);
        if target.is_empty() || alpha <= 0.0 {
            return;
        }
        let (width, height) = (target.width(), target.height());
        let sampler = paint.sampler();
        let solid = paint.solid_color();
        // Chemin rapide : couleur unie en mode Normal.
        let fast = match (solid, blend) {
            (Some(c), BlendMode::Normal) => Some([c.r * 255.0, c.g * 255.0, c.b * 255.0, c.a]),
            _ => None,
        };
        let mut ctx = FillContext {
            target,
            clip,
            alpha,
            blend,
            sampler: &sampler,
            fast,
        };
        self.rasterize(path, transform, rule, (width, height), |y, x0, cov| {
            ctx.composite_row(y, x0, cov);
        });
    }

    /// Couverture d'un chemin. Voir [`path_coverage`].
    pub fn path_coverage(
        &mut self,
        path: &Path,
        transform: &Matrix,
        rule: FillRule,
        width: u32,
        height: u32,
    ) -> Mask {
        let mut mask = Mask::new(width, height);
        if mask.width() == 0 || mask.height() == 0 {
            return mask;
        }
        let w = width as usize;
        // Boîte des pixels effectivement écrits : elle suit le balayage, ce
        // qui la rend exacte sans coût.
        let (mut bx0, mut by0) = (width, height);
        let (mut bx1, mut by1) = (0u32, 0u32);
        let data = mask.data_mut();
        self.rasterize(path, transform, rule, (width, height), |y, x0, cov| {
            let start = y as usize * w + x0 as usize;
            for (d, &c) in data[start..start + cov.len()].iter_mut().zip(cov) {
                *d = to_u8(c);
            }
            bx0 = bx0.min(x0);
            by0 = by0.min(y);
            #[allow(clippy::cast_possible_truncation)]
            let end = x0 + cov.len() as u32;
            bx1 = bx1.max(end);
            by1 = by1.max(y + 1);
        });
        if bx0 < bx1 && by0 < by1 {
            mask.set_bounds((bx0, by0, bx1, by1));
        }
        mask
    }

    /// Construit les arêtes du chemin transformé et aplati. Retourne `false`
    /// si le chemin est vide, hors image ou contient des coordonnées invalides.
    fn build_edges(&mut self, path: &Path, transform: &Matrix, height: u32) -> Option<(i64, i64)> {
        self.edges.clear();
        if path.is_empty() {
            return None;
        }
        let device = path.transform(transform);
        let polygons = device.flatten(FLATTEN_TOLERANCE);
        let mut y_min = f64::INFINITY;
        let mut y_max = f64::NEG_INFINITY;
        for (points, _closed) in &polygons {
            // Un remplissage ferme toujours implicitement le sous-chemin (§8.5.3.1).
            if points.iter().any(|p| !coord_ok(*p)) {
                self.edges.clear();
                return None;
            }
            let n = points.len();
            if n < 2 {
                continue;
            }
            for i in 0..n {
                let a = points[i];
                let b = points[(i + 1) % n];
                if let Some(e) = make_edge(a, b) {
                    y_min = y_min.min(e.y0);
                    y_max = y_max.max(e.y1);
                    self.edges.push(e);
                }
            }
        }
        if self.edges.is_empty() {
            return None;
        }
        // Lignes de pixels concernées, bornées à l'image.
        let h = i64::from(height);
        #[allow(clippy::cast_possible_truncation)] // bornées par COORD_LIMIT
        let (row_start, row_end) = ((y_min.floor() as i64).max(0), (y_max.ceil() as i64).min(h));
        if row_start >= row_end {
            self.edges.clear();
            return None;
        }
        self.edges
            .sort_unstable_by(|a, b| a.y0.partial_cmp(&b.y0).unwrap_or(std::cmp::Ordering::Equal));
        Some((row_start, row_end))
    }

    /// Cœur du rasteriseur : appelle `sink(y, x0, couverture)` pour chaque
    /// ligne touchée, avec la couverture `[0, 1]` des pixels `x0..x0 + len`.
    fn rasterize<F: FnMut(u32, u32, &[f32])>(
        &mut self,
        path: &Path,
        transform: &Matrix,
        rule: FillRule,
        (width, height): (u32, u32),
        mut sink: F,
    ) {
        if width == 0 || height == 0 {
            return;
        }
        let Some((row_start, row_end)) = self.build_edges(path, transform, height) else {
            return;
        };
        let w = width as usize;
        self.cover.clear();
        self.cover.resize(w + 2, 0.0);
        self.delta.clear();
        self.delta.resize(w + 2, 0.0);
        self.row.clear();
        self.row.resize(w, 0.0);
        self.active.clear();
        let mut next_edge = 0usize;
        let fw = f64::from(width);
        #[allow(clippy::cast_precision_loss)] // SUBSAMPLES est petit
        let weight = 1.0 / SUBSAMPLES as f32;

        for row in row_start..row_end {
            let mut min_x = usize::MAX;
            let mut max_x = 0usize;
            for sub in 0..SUBSAMPLES {
                #[allow(clippy::cast_precision_loss)] // indices petits
                let sy = row as f64 + (sub as f64 + 0.5) / SUBSAMPLES as f64;
                // Activation des arêtes commençant au-dessus de la sous-ligne.
                while next_edge < self.edges.len() && self.edges[next_edge].y0 <= sy {
                    self.active.push(next_edge);
                    next_edge += 1;
                }
                let edges = &self.edges;
                self.active.retain(|&i| edges[i].y1 > sy);
                if self.active.is_empty() {
                    if next_edge >= self.edges.len() {
                        break;
                    }
                    continue;
                }
                self.crossings.clear();
                for &i in &self.active {
                    let e = &edges[i];
                    let x = e.x0 + (sy - e.y0) * e.dxdy;
                    self.crossings.push(Crossing { x, dir: e.dir });
                }
                self.crossings.sort_unstable_by(|a, b| {
                    a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal)
                });
                let mut winding = 0i32;
                let mut span_start = 0.0f64;
                for c in &self.crossings {
                    let before = rule.is_inside(winding);
                    winding += c.dir;
                    let after = rule.is_inside(winding);
                    if !before && after {
                        span_start = c.x;
                    } else if before && !after {
                        accumulate_span(
                            &mut self.cover,
                            &mut self.delta,
                            (span_start.max(0.0), c.x.min(fw)),
                            weight,
                            (&mut min_x, &mut max_x),
                        );
                    }
                }
            }
            if min_x == usize::MAX {
                if self.active.is_empty() && next_edge >= self.edges.len() {
                    break;
                }
                continue;
            }
            // Intégration de la ligne : somme cumulée du différentiel + partiels.
            let last = max_x.min(w - 1);
            let mut acc = 0.0f32;
            for x in min_x..=last {
                acc += self.delta[x];
                self.row[x] = unit(acc + self.cover[x]);
            }
            // `row` ≥ 0 et `min_x` < largeur ≤ u32::MAX.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            sink(row as u32, min_x as u32, &self.row[min_x..=last]);
            // Remise à zéro de la zone touchée seulement.
            let clear_end = (max_x + 2).min(w + 2);
            self.cover[min_x..clear_end].fill(0.0);
            self.delta[min_x..clear_end].fill(0.0);
        }
        self.active.clear();
        self.edges.clear();
    }
}

/// Vrai si le point est fini et raisonnable.
#[inline]
fn coord_ok(p: Point) -> bool {
    p.x.is_finite() && p.y.is_finite() && p.x.abs() <= COORD_LIMIT && p.y.abs() <= COORD_LIMIT
}

/// Arête orientée vers le bas, `None` si horizontale.
#[allow(clippy::float_cmp)] // une arête est horizontale si ses ordonnées sont exactement égales
fn make_edge(a: Point, b: Point) -> Option<Edge> {
    if a.y == b.y {
        return None;
    }
    let (top, bottom, dir) = if a.y < b.y { (a, b, 1) } else { (b, a, -1) };
    let dxdy = (bottom.x - top.x) / (bottom.y - top.y);
    dxdy.is_finite().then_some(Edge {
        y0: top.y,
        y1: bottom.y,
        x0: top.x,
        dxdy,
        dir,
    })
}

/// Ajoute l'intervalle `[xa, xb)` (déjà borné à `[0, largeur]`) d'une
/// sous-ligne de poids `weight` aux tampons de couverture.
#[inline]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn accumulate_span(
    cover: &mut [f32],
    delta: &mut [f32],
    (xa, xb): (f64, f64),
    weight: f32,
    (min_x, max_x): (&mut usize, &mut usize),
) {
    if xb.partial_cmp(&xa) != Some(std::cmp::Ordering::Greater) {
        return;
    }
    let ia = xa.floor() as usize;
    let ib = xb.floor() as usize;
    // `ib` peut valoir `largeur` quand xb == largeur : les tampons ont deux
    // cases de marge.
    if ib + 1 >= cover.len() {
        return;
    }
    if ia == ib {
        cover[ia] += ((xb - xa) as f32) * weight;
    } else {
        cover[ia] += ((ia as f64 + 1.0 - xa) as f32) * weight;
        cover[ib] += ((xb - ib as f64) as f32) * weight;
        delta[ia + 1] += weight;
        delta[ib] -= weight;
    }
    *min_x = (*min_x).min(ia);
    *max_x = (*max_x).max(ib);
}

/// État de composition d'un remplissage.
struct FillContext<'a> {
    target: &'a mut Bitmap,
    clip: Option<&'a Mask>,
    alpha: f32,
    blend: BlendMode,
    sampler: &'a PaintSampler<'a>,
    /// Couleur unie en mode Normal : `[r×255, g×255, b×255, a]`.
    fast: Option<[f32; 4]>,
}

impl FillContext<'_> {
    fn composite_row(&mut self, y: u32, x0: u32, cov: &[f32]) {
        let clip_row = self.clip.and_then(|m| m.row(y));
        let clip_width = self.clip.map_or(0, Mask::width);
        let alpha = self.alpha;
        let Some(row) = self.target.row_mut(y) else {
            return;
        };
        for (i, &c) in cov.iter().enumerate() {
            let x = x0 as usize + i;
            let mut coverage = c * alpha;
            if let Some(clip) = clip_row {
                // Hors du masque de clip : exclu.
                #[allow(clippy::cast_possible_truncation)] // x < largeur du bitmap
                let cx = x as u32;
                let m = if cx < clip_width { clip[x] } else { 0 };
                coverage *= f32::from(m) / 255.0;
            } else if self.clip.is_some() {
                // Le masque n'a pas cette ligne : exclu.
                return;
            }
            if coverage <= 1.0 / 1024.0 {
                continue;
            }
            let px = &mut row[x * 4..x * 4 + 4];
            if let Some(fast) = self.fast {
                let a_s = fast[3] * coverage;
                if a_s >= 0.999 {
                    px[0] = to_u8(fast[0] / 255.0);
                    px[1] = to_u8(fast[1] / 255.0);
                    px[2] = to_u8(fast[2] / 255.0);
                    px[3] = 255;
                } else {
                    let inv = 1.0 - a_s;
                    for k in 0..3 {
                        px[k] = to_u8((f32::from(px[k]) * inv + fast[k] * a_s) / 255.0);
                    }
                    px[3] = to_u8((f32::from(px[3]) * inv) / 255.0 + a_s);
                }
            } else {
                #[allow(clippy::cast_precision_loss)] // indices de pixels
                let color = self.sampler.color_at(x as f64 + 0.5, f64::from(y) + 0.5);
                let mut dst = [px[0], px[1], px[2], px[3]];
                composite_pixel(&mut dst, color, coverage, self.blend);
                px.copy_from_slice(&dst);
            }
        }
    }
}

/// Remplit un chemin dans `target`.
///
/// - `transform` : espace du chemin → pixels (origine en haut à gauche).
/// - `rule` : règle de remplissage.
/// - `paint` : source de couleur, échantillonnée au centre de chaque pixel.
/// - `clip` : masque de couverture multiplié à la couverture du chemin ; les
///   pixels hors du masque sont exclus.
/// - `alpha` : alpha constant (`CA`/`ca`, §11.6.4.4).
/// - `blend` : mode de fusion (§11.3.5).
///
/// Un chemin vide, hors image ou contenant des coordonnées non finies ou
/// gigantesques ne dessine rien. Pour des appels répétés, préférer un
/// [`Rasterizer`] réutilisé.
#[allow(clippy::too_many_arguments)] // signature imposée
pub fn fill_path(
    target: &mut Bitmap,
    path: &Path,
    transform: &Matrix,
    rule: FillRule,
    paint: &Paint,
    clip: Option<&Mask>,
    alpha: f32,
    blend: BlendMode,
) {
    Rasterizer::new().fill_path(target, path, transform, rule, paint, clip, alpha, blend);
}

/// Couverture anti-aliasée d'un chemin dans une image `width × height`,
/// utilisée pour construire les masques de clip (§8.5.4).
#[must_use]
pub fn path_coverage(
    path: &Path,
    transform: &Matrix,
    rule: FillRule,
    width: u32,
    height: u32,
) -> Mask {
    Rasterizer::new().path_coverage(path, transform, rule, width, height)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::cast_precision_loss
)]
mod tests {
    use super::*;
    use crate::color::Color;
    use acrux_core::Rect;

    fn rect_path(x0: f64, y0: f64, x1: f64, y1: f64) -> Path {
        let mut p = Path::new();
        p.rect(&Rect::new(x0, y0, x1, y1));
        p
    }

    /// Cercle de centre `(cx, cy)` et rayon `r` en quatre cubiques.
    fn circle_path(cx: f64, cy: f64, r: f64) -> Path {
        let k = 0.552_284_749_8 * r;
        let mut p = Path::new();
        p.move_to(Point::new(cx + r, cy));
        p.curve_to(
            Point::new(cx + r, cy + k),
            Point::new(cx + k, cy + r),
            Point::new(cx, cy + r),
        );
        p.curve_to(
            Point::new(cx - k, cy + r),
            Point::new(cx - r, cy + k),
            Point::new(cx - r, cy),
        );
        p.curve_to(
            Point::new(cx - r, cy - k),
            Point::new(cx - k, cy - r),
            Point::new(cx, cy - r),
        );
        p.curve_to(
            Point::new(cx + k, cy - r),
            Point::new(cx + r, cy - k),
            Point::new(cx + r, cy),
        );
        p.close();
        p
    }

    fn fill_black(target: &mut Bitmap, path: &Path, rule: FillRule) {
        fill_path(
            target,
            path,
            &Matrix::IDENTITY,
            rule,
            &Paint::Solid(Color::BLACK),
            None,
            1.0,
            BlendMode::Normal,
        );
    }

    fn coverage_sum(mask: &Mask) -> f64 {
        mask.data().iter().map(|&v| f64::from(v) / 255.0).sum()
    }

    #[test]
    fn aligned_rectangle_is_pixel_exact() {
        let mut b = Bitmap::new(8, 8);
        fill_black(&mut b, &rect_path(2.0, 3.0, 6.0, 5.0), FillRule::NonZero);
        for y in 0..8 {
            for x in 0..8 {
                let inside = (2..6).contains(&x) && (3..5).contains(&y);
                let expected = if inside { [0, 0, 0, 255] } else { [0; 4] };
                assert_eq!(b.pixel(x, y), Some(expected), "pixel ({x}, {y})");
            }
        }
    }

    #[test]
    fn half_pixel_rectangle_has_half_coverage_on_edges() {
        let m = path_coverage(
            &rect_path(2.5, 1.5, 6.5, 4.5),
            &Matrix::IDENTITY,
            FillRule::NonZero,
            10,
            10,
        );
        // Bords verticaux : couverture horizontale exacte.
        assert!((i32::from(m.get(2, 3)) - 128).abs() <= 1);
        assert!((i32::from(m.get(6, 3)) - 128).abs() <= 1);
        assert_eq!(m.get(4, 3), 255);
        // Bords horizontaux : 8 sous-lignes sur 16.
        assert!((i32::from(m.get(4, 1)) - 128).abs() <= 1);
        assert!((i32::from(m.get(4, 4)) - 128).abs() <= 1);
        // Coin : 50 % × 50 %.
        assert!((i32::from(m.get(2, 1)) - 64).abs() <= 1);
        assert_eq!(m.get(1, 3), 0);
        assert_eq!(m.get(7, 3), 0);
        // Aire totale = 4 × 3.
        assert!((coverage_sum(&m) - 12.0).abs() < 0.05);
    }

    #[test]
    fn circle_area_matches_pi_r_squared() {
        let r = 40.0;
        let m = path_coverage(
            &circle_path(50.0, 50.0, r),
            &Matrix::IDENTITY,
            FillRule::NonZero,
            100,
            100,
        );
        let area = coverage_sum(&m);
        let expected = std::f64::consts::PI * r * r;
        assert!(
            (area - expected).abs() / expected < 0.01,
            "aire {area} attendue {expected}"
        );
        // Le centre est plein, l'extérieur vide, le bord partiel.
        assert_eq!(m.get(50, 50), 255);
        assert_eq!(m.get(2, 2), 0);
        // À 45°, le bord passe par x = y = 50 + 40·cos(45°) ≈ 78,3.
        let edge = m.get(78, 78);
        assert!(edge > 0 && edge < 255, "bord : {edge}");
    }

    #[test]
    fn nested_squares_same_orientation_rules_differ() {
        let mut p = rect_path(1.0, 1.0, 9.0, 9.0);
        p.rect(&Rect::new(3.0, 3.0, 7.0, 7.0));
        let nz = path_coverage(&p, &Matrix::IDENTITY, FillRule::NonZero, 10, 10);
        let eo = path_coverage(&p, &Matrix::IDENTITY, FillRule::EvenOdd, 10, 10);
        assert_eq!(nz.get(5, 5), 255, "non-zero remplit le centre");
        assert_eq!(eo.get(5, 5), 0, "even-odd creuse le centre");
        assert_eq!(nz.get(2, 2), 255);
        assert_eq!(eo.get(2, 2), 255);
        assert_eq!(nz.get(0, 0), 0);
        assert_eq!(eo.get(0, 0), 0);
    }

    #[test]
    fn nested_squares_opposite_orientation_both_rules_hollow() {
        let mut p = rect_path(1.0, 1.0, 9.0, 9.0);
        // Carré intérieur parcouru en sens inverse.
        p.move_to(Point::new(3.0, 3.0));
        p.line_to(Point::new(3.0, 7.0));
        p.line_to(Point::new(7.0, 7.0));
        p.line_to(Point::new(7.0, 3.0));
        p.close();
        let nz = path_coverage(&p, &Matrix::IDENTITY, FillRule::NonZero, 10, 10);
        let eo = path_coverage(&p, &Matrix::IDENTITY, FillRule::EvenOdd, 10, 10);
        assert_eq!(nz.get(5, 5), 0);
        assert_eq!(eo.get(5, 5), 0);
        assert_eq!(nz.get(2, 5), 255);
        assert_eq!(eo.get(2, 5), 255);
    }

    #[test]
    fn transform_flips_y_axis() {
        // Rectangle PDF (origine en bas) rendu dans une image de 10 px de haut.
        let flip = Matrix::new(1.0, 0.0, 0.0, -1.0, 0.0, 10.0);
        let m = path_coverage(
            &rect_path(0.0, 0.0, 4.0, 2.0),
            &flip,
            FillRule::NonZero,
            10,
            10,
        );
        assert_eq!(m.get(1, 9), 255);
        assert_eq!(m.get(1, 8), 255);
        assert_eq!(m.get(1, 7), 0);
    }

    #[test]
    fn clip_mask_limits_fill_to_half() {
        let mut clip = Mask::new(10, 10);
        for y in 0..10 {
            for x in 0..5 {
                clip.data_mut()[y * 10 + x] = 255;
            }
        }
        let mut b = Bitmap::new(10, 10);
        fill_path(
            &mut b,
            &rect_path(0.0, 0.0, 10.0, 10.0),
            &Matrix::IDENTITY,
            FillRule::NonZero,
            &Paint::Solid(Color::BLACK),
            Some(&clip),
            1.0,
            BlendMode::Normal,
        );
        for y in 0..10 {
            assert_eq!(b.pixel(2, y), Some([0, 0, 0, 255]));
            assert_eq!(b.pixel(7, y), Some([0; 4]));
        }
        // Masque plus petit que le bitmap : le reste est exclu.
        let small = Mask::full(3, 3);
        let mut b = Bitmap::new(10, 10);
        fill_path(
            &mut b,
            &rect_path(0.0, 0.0, 10.0, 10.0),
            &Matrix::IDENTITY,
            FillRule::NonZero,
            &Paint::Solid(Color::BLACK),
            Some(&small),
            1.0,
            BlendMode::Normal,
        );
        assert_eq!(b.pixel(1, 1), Some([0, 0, 0, 255]));
        assert_eq!(b.pixel(5, 1), Some([0; 4]));
        assert_eq!(b.pixel(1, 5), Some([0; 4]));
    }

    #[test]
    fn constant_alpha_and_blend_modes_apply() {
        let mut b = Bitmap::new_filled(4, 4, Color::WHITE);
        fill_path(
            &mut b,
            &rect_path(0.0, 0.0, 4.0, 4.0),
            &Matrix::IDENTITY,
            FillRule::NonZero,
            &Paint::Solid(Color::BLACK),
            None,
            0.5,
            BlendMode::Normal,
        );
        assert_eq!(b.pixel(1, 1), Some([128, 128, 128, 255]));
        let mut b = Bitmap::new_filled(4, 4, Color::rgb(0.5, 0.5, 0.5));
        fill_path(
            &mut b,
            &rect_path(0.0, 0.0, 4.0, 4.0),
            &Matrix::IDENTITY,
            FillRule::NonZero,
            &Paint::Solid(Color::rgb(0.5, 0.5, 0.5)),
            None,
            1.0,
            BlendMode::Multiply,
        );
        assert_eq!(b.pixel(1, 1), Some([64, 64, 64, 255]));
        // Alpha nul : rien ne change.
        let mut b = Bitmap::new(2, 2);
        fill_path(
            &mut b,
            &rect_path(0.0, 0.0, 2.0, 2.0),
            &Matrix::IDENTITY,
            FillRule::NonZero,
            &Paint::Solid(Color::BLACK),
            None,
            0.0,
            BlendMode::Normal,
        );
        assert_eq!(b.pixel(0, 0), Some([0; 4]));
    }

    #[test]
    fn gradient_paint_is_sampled_per_pixel() {
        use crate::paint::Gradient;
        let g = Gradient::linear(
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            vec![(0.0, Color::BLACK), (1.0, Color::WHITE)],
        );
        let mut b = Bitmap::new(10, 1);
        fill_path(
            &mut b,
            &rect_path(0.0, 0.0, 10.0, 1.0),
            &Matrix::IDENTITY,
            FillRule::NonZero,
            &Paint::LinearGradient(g),
            None,
            1.0,
            BlendMode::Normal,
        );
        let values: Vec<u8> = (0..10).map(|x| b.pixel(x, 0).unwrap()[0]).collect();
        assert!(values.windows(2).all(|w| w[1] > w[0]), "{values:?}");
        assert!(values.iter().all(|&v| v > 0 && v < 255));
    }

    #[test]
    fn paths_outside_or_partially_outside_are_clipped() {
        let mut b = Bitmap::new(4, 4);
        fill_black(
            &mut b,
            &rect_path(100.0, 100.0, 200.0, 200.0),
            FillRule::NonZero,
        );
        assert!(b.data().iter().all(|&v| v == 0));
        fill_black(
            &mut b,
            &rect_path(-50.0, -50.0, 2.0, 50.0),
            FillRule::NonZero,
        );
        assert_eq!(b.pixel(1, 3), Some([0, 0, 0, 255]));
        assert_eq!(b.pixel(2, 3), Some([0; 4]));
        // Rectangle englobant toute l'image et bien au-delà.
        let mut b = Bitmap::new(4, 4);
        fill_black(&mut b, &rect_path(-1e6, -1e6, 1e6, 1e6), FillRule::NonZero);
        assert!(b.data().chunks_exact(4).all(|p| p == [0, 0, 0, 255]));
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        let mut zero = Bitmap::new(0, 0);
        fill_black(&mut zero, &rect_path(0.0, 0.0, 1.0, 1.0), FillRule::NonZero);
        let mut b = Bitmap::new(4, 4);
        fill_black(&mut b, &Path::new(), FillRule::EvenOdd);
        let mut nan = Path::new();
        nan.move_to(Point::new(f64::NAN, 0.0));
        nan.line_to(Point::new(3.0, 3.0));
        nan.line_to(Point::new(0.0, 3.0));
        fill_black(&mut b, &nan, FillRule::NonZero);
        let mut huge = Path::new();
        huge.move_to(Point::new(1e300, 0.0));
        huge.line_to(Point::new(3.0, 3.0));
        huge.line_to(Point::new(0.0, -1e300));
        fill_black(&mut b, &huge, FillRule::NonZero);
        let mut inf = Path::new();
        inf.move_to(Point::new(f64::INFINITY, f64::NEG_INFINITY));
        inf.line_to(Point::new(1.0, 1.0));
        inf.line_to(Point::new(0.0, 1.0));
        fill_black(&mut b, &inf, FillRule::NonZero);
        assert!(b.data().iter().all(|&v| v == 0));
        // Matrice singulière et sous-chemin d'un seul point.
        fill_path(
            &mut b,
            &rect_path(0.0, 0.0, 4.0, 4.0),
            &Matrix::scale(0.0, 0.0),
            FillRule::NonZero,
            &Paint::Solid(Color::BLACK),
            None,
            1.0,
            BlendMode::Normal,
        );
        let mut dot = Path::new();
        dot.move_to(Point::new(1.0, 1.0));
        fill_black(&mut b, &dot, FillRule::NonZero);
        assert!(b.data().iter().all(|&v| v == 0));
        assert_eq!(
            path_coverage(&Path::new(), &Matrix::IDENTITY, FillRule::NonZero, 0, 0).width(),
            0
        );
        let _ = path_coverage(
            &nan,
            &Matrix::IDENTITY,
            FillRule::NonZero,
            u32::MAX,
            u32::MAX,
        );
    }

    #[test]
    fn rasterizer_is_reusable() {
        let mut r = Rasterizer::new();
        let mut b = Bitmap::new(6, 6);
        for i in 0..3 {
            let f = f64::from(i);
            r.fill_path(
                &mut b,
                &rect_path(f, f, f + 2.0, f + 2.0),
                &Matrix::IDENTITY,
                FillRule::NonZero,
                &Paint::Solid(Color::BLACK),
                None,
                1.0,
                BlendMode::Normal,
            );
        }
        assert_eq!(b.pixel(0, 0), Some([0, 0, 0, 255]));
        assert_eq!(b.pixel(3, 3), Some([0, 0, 0, 255]));
        assert_eq!(b.pixel(5, 5), Some([0; 4]));
        let m = r.path_coverage(
            &rect_path(0.0, 0.0, 1.0, 1.0),
            &Matrix::IDENTITY,
            FillRule::NonZero,
            2,
            2,
        );
        assert_eq!(m.get(0, 0), 255);
    }

    #[test]
    fn thin_shapes_get_partial_coverage() {
        // Trait de 0,25 px de large : couverture 25 %.
        let m = path_coverage(
            &rect_path(2.0, 0.0, 2.25, 4.0),
            &Matrix::IDENTITY,
            FillRule::NonZero,
            5,
            4,
        );
        assert!((i32::from(m.get(2, 1)) - 64).abs() <= 1);
        // Triangle fin oblique : aire préservée.
        let mut tri = Path::new();
        tri.move_to(Point::new(0.0, 0.0));
        tri.line_to(Point::new(20.0, 0.0));
        tri.line_to(Point::new(0.0, 20.0));
        tri.close();
        let m = path_coverage(&tri, &Matrix::IDENTITY, FillRule::EvenOdd, 20, 20);
        assert!((coverage_sum(&m) - 200.0).abs() < 1.0);
    }

    #[test]
    #[ignore = "benchmark : cargo test -p acrux-graphics -- --ignored --nocapture"]
    fn bench_a4_150dpi_rectangle_fill() {
        let (w, h) = (1240, 1754);
        let mut b = Bitmap::new(w, h);
        let mut r = Rasterizer::new();
        let path = rect_path(0.0, 0.0, f64::from(w), f64::from(h));
        let paint = Paint::Solid(Color::rgb(0.2, 0.4, 0.8));
        let iterations = 20;
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            r.fill_path(
                &mut b,
                &path,
                &Matrix::IDENTITY,
                FillRule::NonZero,
                &paint,
                None,
                1.0,
                BlendMode::Normal,
            );
        }
        let per = start.elapsed() / iterations;
        println!("remplissage A4 150 dpi ({w}×{h}) : {per:?} par page");
        let circle = circle_path(620.0, 877.0, 500.0);
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            r.fill_path(
                &mut b,
                &circle,
                &Matrix::IDENTITY,
                FillRule::NonZero,
                &paint,
                None,
                0.5,
                BlendMode::Normal,
            );
        }
        let per = start.elapsed() / iterations;
        println!("cercle r = 500 px avec alpha 0,5 : {per:?}");
        assert_eq!(b.pixel(0, 0).map(|p| p[3]), Some(255));
    }
}
