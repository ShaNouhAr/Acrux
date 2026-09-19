//! Conversion d'un contour (« stroke », ISO 32000-2 §8.4.3) en chemin de
//! remplissage.
//!
//! # Méthode
//!
//! Le trait est construit dans un « espace de trait » : l'espace utilisateur
//! mis à l'échelle uniformément de façon qu'une unité y vaille au plus un
//! pixel device. Le chemin y est aplati avec la tolérance du rasteriseur, le
//! motif de tirets appliqué (§8.4.3.6), puis chaque segment, chaque joint et
//! chaque extrémité produit un petit polygone orienté dans le sens direct.
//! L'union de ces polygones, remplie en non-zero, est le contour. Le résultat
//! est enfin transformé vers l'espace device par le reste de la matrice, ce
//! qui rend correctement les épaisseurs anisotropes (§8.4.3.2 : la largeur est
//! exprimée dans l'espace utilisateur).
//!
//! Les polygones se recouvrent aux joints ; comme ils ont tous la même
//! orientation, leurs enroulements s'additionnent et non-zero remplit
//! l'union exacte.

use acrux_core::{Matrix, Path, Point};

use crate::raster::FLATTEN_TOLERANCE;

/// Forme des extrémités (§8.4.3.3, tableau 54).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineCap {
    /// Coupée perpendiculairement au point final.
    #[default]
    Butt,
    /// Demi-disque de diamètre la largeur.
    Round,
    /// Prolongée d'une demi-largeur.
    Square,
}

impl LineCap {
    /// Depuis la valeur de l'opérateur `J` (0, 1, 2) ; autre valeur → `Butt`.
    #[must_use]
    pub fn from_pdf(value: i64) -> Self {
        match value {
            1 => LineCap::Round,
            2 => LineCap::Square,
            _ => LineCap::Butt,
        }
    }
}

/// Forme des joints (§8.4.3.4, tableau 55).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineJoin {
    /// Pointe, remplacée par un biseau au-delà de la limite d'onglet.
    #[default]
    Miter,
    /// Arc de cercle.
    Round,
    /// Biseau.
    Bevel,
}

impl LineJoin {
    /// Depuis la valeur de l'opérateur `j` (0, 1, 2) ; autre valeur → `Miter`.
    #[must_use]
    pub fn from_pdf(value: i64) -> Self {
        match value {
            1 => LineJoin::Round,
            2 => LineJoin::Bevel,
            _ => LineJoin::Miter,
        }
    }
}

/// Paramètres de trait de l'état graphique (§8.4.3).
#[derive(Debug, Clone, PartialEq)]
pub struct StrokeStyle {
    /// Largeur en unités utilisateur ; 0 = trait le plus fin possible (§8.4.3.2).
    pub width: f64,
    /// Extrémités.
    pub cap: LineCap,
    /// Joints.
    pub join: LineJoin,
    /// Limite d'onglet (§8.4.3.5), rapport longueur d'onglet / largeur.
    pub miter_limit: f64,
    /// Motif de tirets `(tableau, phase)` en unités utilisateur (§8.4.3.6).
    pub dash: Option<(Vec<f64>, f64)>,
}

impl Default for StrokeStyle {
    fn default() -> Self {
        Self {
            width: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter_limit: 10.0,
            dash: None,
        }
    }
}

impl StrokeStyle {
    /// Style de largeur donnée, autres paramètres par défaut du PDF.
    #[must_use]
    pub fn with_width(width: f64) -> Self {
        Self {
            width,
            ..Self::default()
        }
    }
}

/// Largeur device minimale d'un trait, en pixels : un trait plus fin serait
/// presque invisible une fois anti-aliasé (§8.4.3.2 pour la largeur 0).
pub const MIN_DEVICE_WIDTH: f64 = 1.0;

/// Épsilon de comparaison des longueurs dans l'espace de trait (pixels).
const EPS: f64 = 1e-9;

/// Convertit un chemin en contour de remplissage dans l'espace device.
///
/// Le résultat se remplit avec [`FillRule::NonZero`](crate::FillRule). La
/// largeur est appliquée dans l'espace utilisateur puis transformée, y
/// compris pour une matrice anisotrope. Une matrice singulière, une largeur
/// non finie ou un chemin vide donnent un chemin vide.
#[must_use]
pub fn stroke_path(path: &Path, style: &StrokeStyle, transform: &Matrix) -> Path {
    let mut out = Path::new();
    if path.is_empty() || !style.width.is_finite() || style.width < 0.0 {
        return out;
    }
    // Étirements extrêmes de la matrice (valeurs singulières).
    let sx = (transform.a * transform.a + transform.b * transform.b).sqrt();
    let sy = (transform.c * transform.c + transform.d * transform.d).sqrt();
    let max_stretch = sx.max(sy);
    let det = transform.determinant().abs();
    if !max_stretch.is_finite() || max_stretch <= 0.0 || !det.is_finite() || det <= 0.0 {
        return out;
    }
    let min_stretch = det / max_stretch;
    // Espace de trait : utilisateur × max_stretch.
    let scale = max_stretch;
    let pre = Matrix::scale(scale, scale);
    let post = Matrix::scale(1.0 / scale, 1.0 / scale).then(transform);
    // Largeur dans l'espace de trait, avec le minimum device dans la
    // direction la moins étirée.
    let min_width = MIN_DEVICE_WIDTH * scale / min_stretch;
    let width = (style.width * scale).max(min_width);
    if !width.is_finite() {
        return out;
    }
    let half = width / 2.0;
    let dash = style
        .dash
        .as_ref()
        .and_then(|(pattern, phase)| scaled_dash(pattern, *phase, scale));

    let polygons = path.transform(&pre).flatten(FLATTEN_TOLERANCE);
    let mut stroke = StrokeBuilder {
        out: &mut out,
        half,
        cap: style.cap,
        join: style.join,
        miter_limit: if style.miter_limit.is_finite() {
            style.miter_limit.max(1.0)
        } else {
            10.0
        },
        round_segments: round_segment_count(half),
    };
    for (points, closed) in polygons {
        if points.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()) {
            continue;
        }
        let pts = dedup(&points, closed);
        if pts.len() < 2 {
            // Sous-chemin dégénéré (§8.4.3.3 et §8.5.3.2).
            if let Some(&p) = pts.first() {
                stroke.degenerate(p);
            }
            continue;
        }
        match &dash {
            Some((pattern, phase)) => {
                let mut poly = pts.clone();
                if closed {
                    if let Some(&first) = poly.first() {
                        poly.push(first);
                    }
                }
                for piece in apply_dash(&poly, pattern, *phase) {
                    stroke.polyline(&piece, false);
                }
            }
            None => stroke.polyline(&pts, closed),
        }
    }
    out.transform(&post)
}

/// Motif de tirets validé (§8.4.3.6 : valeurs non négatives, somme non nulle)
/// et mis à l'échelle de l'espace de trait.
fn scaled_dash(pattern: &[f64], phase: f64, scale: f64) -> Option<(Vec<f64>, f64)> {
    if pattern.is_empty() || pattern.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return None;
    }
    let total: f64 = pattern.iter().sum();
    if !total.is_finite() || total <= 0.0 || !phase.is_finite() {
        return None;
    }
    let scaled: Vec<f64> = pattern.iter().map(|v| v * scale).collect();
    Some((scaled, phase.max(0.0) * scale))
}

/// Supprime les points consécutifs confondus (et le retour au départ d'un
/// sous-chemin fermé).
fn dedup(points: &[Point], closed: bool) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::with_capacity(points.len());
    for &p in points {
        if out.last().is_none_or(|q| dist(*q, p) > EPS) {
            out.push(p);
        }
    }
    if closed && out.len() > 1 {
        if let (Some(&first), Some(&last)) = (out.first(), out.last()) {
            if dist(first, last) <= EPS {
                out.pop();
            }
        }
    }
    out
}

fn dist(a: Point, b: Point) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

/// Nombre de segments pour approcher un cercle de rayon `r` (pixels) avec
/// la tolérance du rasteriseur.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // valeur bornée
fn round_segment_count(r: f64) -> usize {
    if !r.is_finite() || r <= FLATTEN_TOLERANCE {
        return 8;
    }
    let step = 2.0 * (1.0 - FLATTEN_TOLERANCE / r).clamp(-1.0, 1.0).acos();
    if !step.is_finite() || step <= 0.0 {
        return 8;
    }
    (std::f64::consts::TAU / step).ceil().clamp(8.0, 256.0) as usize
}

/// Découpe une polyligne selon le motif de tirets ; retourne les morceaux « encre ».
fn apply_dash(points: &[Point], pattern: &[f64], phase: f64) -> Vec<Vec<Point>> {
    let mut pieces: Vec<Vec<Point>> = Vec::new();
    if points.len() < 2 || pattern.is_empty() {
        return pieces;
    }
    // Position initiale dans le motif d'après la phase.
    let period: f64 = pattern.iter().sum::<f64>() * if pattern.len() % 2 == 1 { 2.0 } else { 1.0 };
    let mut remaining = if period > 0.0 { phase % period } else { 0.0 };
    let mut index = 0usize;
    let mut on = true;
    let mut left = pattern[0];
    while remaining > 0.0 {
        if remaining >= left {
            remaining -= left;
            index = (index + 1) % pattern.len();
            left = pattern[index];
            on = !on;
        } else {
            left -= remaining;
            remaining = 0.0;
        }
    }
    let mut current: Vec<Point> = Vec::new();
    if on {
        current.push(points[0]);
    }
    for seg in points.windows(2) {
        let (a, b) = (seg[0], seg[1]);
        let len = dist(a, b);
        if len <= EPS {
            continue;
        }
        let mut pos = 0.0;
        while pos < len {
            let step = left.min(len - pos);
            pos += step;
            left -= step;
            let t = pos / len;
            let p = Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
            if on {
                current.push(p);
            }
            if left <= EPS {
                // Fin d'un élément du motif.
                if on && current.len() > 1 {
                    pieces.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
                on = !on;
                index = (index + 1) % pattern.len();
                left = pattern[index];
                if on {
                    current.push(p);
                }
                // Éléments de longueur nulle : on les saute sans boucler.
                let mut guard = 0;
                while left <= EPS && guard < pattern.len() {
                    if on {
                        // Tiret de longueur nulle : un point, dessiné par les caps.
                        pieces.push(vec![p, p]);
                    }
                    current.clear();
                    on = !on;
                    index = (index + 1) % pattern.len();
                    left = pattern[index];
                    if on {
                        current.push(p);
                    }
                    guard += 1;
                }
                if left <= EPS {
                    return pieces;
                }
            }
        }
    }
    if on && current.len() > 1 {
        pieces.push(current);
    }
    pieces
}

/// Générateur des polygones du trait dans l'espace de trait.
struct StrokeBuilder<'a> {
    out: &'a mut Path,
    half: f64,
    cap: LineCap,
    join: LineJoin,
    miter_limit: f64,
    round_segments: usize,
}

impl StrokeBuilder<'_> {
    /// Ajoute un polygone fermé, orienté dans le sens direct (aire signée positive).
    fn polygon(&mut self, pts: &[Point]) {
        if pts.len() < 3 {
            return;
        }
        let mut area = 0.0;
        for i in 0..pts.len() {
            let a = pts[i];
            let b = pts[(i + 1) % pts.len()];
            area += a.x * b.y - b.x * a.y;
        }
        if area.abs() <= EPS {
            return;
        }
        let push = |out: &mut Path, p: Point, first: bool| {
            if first {
                out.move_to(p);
            } else {
                out.line_to(p);
            }
        };
        if area > 0.0 {
            for (i, &p) in pts.iter().enumerate() {
                push(self.out, p, i == 0);
            }
        } else {
            for (i, &p) in pts.iter().rev().enumerate() {
                push(self.out, p, i == 0);
            }
        }
        self.out.close();
    }

    /// Disque de rayon `half` centré en `c`.
    fn circle(&mut self, c: Point) {
        let n = self.round_segments;
        let pts: Vec<Point> = (0..n)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)] // n ≤ 256
                let angle = std::f64::consts::TAU * i as f64 / n as f64;
                Point::new(c.x + self.half * angle.cos(), c.y + self.half * angle.sin())
            })
            .collect();
        self.polygon(&pts);
    }

    /// Sous-chemin réduit à un point (§8.4.3.3) : disque pour `Round`, carré
    /// pour `Square`, rien pour `Butt`.
    fn degenerate(&mut self, p: Point) {
        match self.cap {
            LineCap::Round => self.circle(p),
            LineCap::Square => {
                let h = self.half;
                self.polygon(&[
                    Point::new(p.x - h, p.y - h),
                    Point::new(p.x + h, p.y - h),
                    Point::new(p.x + h, p.y + h),
                    Point::new(p.x - h, p.y + h),
                ]);
            }
            LineCap::Butt => {}
        }
    }

    /// Contour d'une polyligne sans point double.
    fn polyline(&mut self, pts: &[Point], closed: bool) {
        let n = pts.len();
        if n < 2 {
            if let Some(&p) = pts.first() {
                self.degenerate(p);
            }
            return;
        }
        let closed = closed && n >= 3;
        let seg_count = if closed { n } else { n - 1 };
        // Direction unitaire et normale gauche de chaque segment.
        let dirs: Vec<(Point, Point)> = (0..seg_count)
            .map(|i| {
                let a = pts[i];
                let b = pts[(i + 1) % n];
                let len = dist(a, b).max(EPS);
                let d = Point::new((b.x - a.x) / len, (b.y - a.y) / len);
                (d, Point::new(-d.y, d.x))
            })
            .collect();
        let h = self.half;
        // Corps des segments.
        for (i, &(_, nrm)) in dirs.iter().enumerate() {
            let a = pts[i];
            let b = pts[(i + 1) % n];
            self.polygon(&[
                Point::new(a.x + nrm.x * h, a.y + nrm.y * h),
                Point::new(b.x + nrm.x * h, b.y + nrm.y * h),
                Point::new(b.x - nrm.x * h, b.y - nrm.y * h),
                Point::new(a.x - nrm.x * h, a.y - nrm.y * h),
            ]);
        }
        // Joints.
        let join_range = if closed { 0..n } else { 1..n - 1 };
        for i in join_range {
            let prev = if i == 0 { seg_count - 1 } else { i - 1 };
            let cur = i % seg_count;
            self.join(pts[i], dirs[prev].0, dirs[cur].0);
        }
        // Extrémités.
        if !closed {
            let (d0, _) = dirs[0];
            let (d1, _) = dirs[seg_count - 1];
            self.cap(pts[0], Point::new(-d0.x, -d0.y));
            self.cap(pts[n - 1], d1);
        }
    }

    /// Joint au point `p` entre les directions entrante `d1` et sortante `d2`.
    fn join(&mut self, p: Point, d1: Point, d2: Point) {
        let cross = d1.x * d2.y - d1.y * d2.x;
        let dot = d1.x * d2.x + d1.y * d2.y;
        if cross.abs() <= 1e-12 && dot > 0.0 {
            return; // segments alignés : rien à combler
        }
        if self.join == LineJoin::Round {
            self.circle(p);
            return;
        }
        let half = self.half;
        // Normales du côté extérieur du virage.
        let sign = if cross > 0.0 { -1.0 } else { 1.0 };
        let n1 = Point::new(-d1.y * sign, d1.x * sign);
        let n2 = Point::new(-d2.y * sign, d2.x * sign);
        let outer_in = Point::new(p.x + n1.x * half, p.y + n1.y * half);
        let outer_out = Point::new(p.x + n2.x * half, p.y + n2.y * half);
        if self.join == LineJoin::Miter {
            // Rapport longueur d'onglet / largeur = 1 / sin(φ/2) avec φ l'angle
            // entre les segments ; 1 + cos(θ) = 2·cos²(θ/2) où θ est l'angle de
            // rotation, donc sin(φ/2) = cos(θ/2) (§8.4.3.5).
            let one_plus_cos = 1.0 + dot;
            if one_plus_cos > 1e-12 {
                let ratio = 1.0 / (one_plus_cos / 2.0).sqrt();
                if ratio <= self.miter_limit {
                    let k = half / one_plus_cos;
                    let tip = Point::new(p.x + (n1.x + n2.x) * k, p.y + (n1.y + n2.y) * k);
                    self.polygon(&[p, outer_in, tip, outer_out]);
                    return;
                }
            }
        }
        self.polygon(&[p, outer_in, outer_out]);
    }

    /// Extrémité en `p`, `d` pointant vers l'extérieur du trait.
    fn cap(&mut self, p: Point, d: Point) {
        let h = self.half;
        match self.cap {
            LineCap::Butt => {}
            LineCap::Round => self.circle(p),
            LineCap::Square => {
                let nrm = Point::new(-d.y, d.x);
                let q = Point::new(p.x + d.x * h, p.y + d.y * h);
                self.polygon(&[
                    Point::new(p.x + nrm.x * h, p.y + nrm.y * h),
                    Point::new(q.x + nrm.x * h, q.y + nrm.y * h),
                    Point::new(q.x - nrm.x * h, q.y - nrm.y * h),
                    Point::new(p.x - nrm.x * h, p.y - nrm.y * h),
                ]);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::bitmap::Mask;
    use crate::raster::{path_coverage, FillRule};
    use acrux_core::Rect;

    fn line(a: (f64, f64), b: (f64, f64)) -> Path {
        let mut p = Path::new();
        p.move_to(Point::new(a.0, a.1));
        p.line_to(Point::new(b.0, b.1));
        p
    }

    fn coverage(path: &Path, style: &StrokeStyle, m: &Matrix, w: u32, h: u32) -> Mask {
        let outline = stroke_path(path, style, m);
        path_coverage(&outline, &Matrix::IDENTITY, FillRule::NonZero, w, h)
    }

    fn bbox(mask: &Mask) -> Option<(u32, u32, u32, u32)> {
        let mut r: Option<(u32, u32, u32, u32)> = None;
        for y in 0..mask.height() {
            for x in 0..mask.width() {
                if mask.get(x, y) > 0 {
                    r = Some(r.map_or((x, y, x, y), |(x0, y0, x1, y1)| {
                        (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
                    }));
                }
            }
        }
        r
    }

    #[test]
    fn horizontal_line_width_two_is_two_full_rows() {
        let m = coverage(
            &line((2.0, 5.0), (12.0, 5.0)),
            &StrokeStyle::with_width(2.0),
            &Matrix::IDENTITY,
            16,
            10,
        );
        for x in 2..12 {
            assert_eq!(m.get(x, 4), 255, "x = {x}");
            assert_eq!(m.get(x, 5), 255, "x = {x}");
            assert_eq!(m.get(x, 3), 0);
            assert_eq!(m.get(x, 6), 0);
        }
        // Butt : rien avant 2 ni après 12.
        assert_eq!(m.get(1, 4), 0);
        assert_eq!(m.get(12, 4), 0);
    }

    #[test]
    fn caps_extend_the_line() {
        let base = line((4.0, 5.0), (12.0, 5.0));
        let square = coverage(
            &base,
            &StrokeStyle {
                width: 2.0,
                cap: LineCap::Square,
                ..StrokeStyle::default()
            },
            &Matrix::IDENTITY,
            16,
            10,
        );
        assert_eq!(square.get(3, 4), 255);
        assert_eq!(square.get(12, 5), 255);
        assert_eq!(square.get(2, 4), 0);
        let round = coverage(
            &base,
            &StrokeStyle {
                width: 4.0,
                cap: LineCap::Round,
                ..StrokeStyle::default()
            },
            &Matrix::IDENTITY,
            16,
            10,
        );
        // Le demi-disque couvre le centre de l'extrémité mais pas les coins.
        assert_eq!(round.get(3, 4), 255);
        assert!(round.get(2, 3) < 255);
        assert_eq!(round.get(1, 4), 0);
    }

    #[test]
    fn joins_have_different_bounding_boxes() {
        // Angle aigu vers la droite : l'onglet dépasse loin.
        let mut p = Path::new();
        p.move_to(Point::new(5.0, 30.0));
        p.line_to(Point::new(25.0, 10.0));
        p.line_to(Point::new(45.0, 30.0));
        let mut boxes = Vec::new();
        let mut areas = Vec::new();
        let mut masks = Vec::new();
        for join in [LineJoin::Miter, LineJoin::Round, LineJoin::Bevel] {
            let style = StrokeStyle {
                width: 6.0,
                join,
                ..StrokeStyle::default()
            };
            let m = coverage(&p, &style, &Matrix::IDENTITY, 60, 40);
            boxes.push(bbox(&m).unwrap());
            areas.push(m.data().iter().map(|&v| f64::from(v)).sum::<f64>());
            masks.push(m);
        }
        let (miter, round, bevel) = (boxes[0], boxes[1], boxes[2]);
        assert!(
            miter.1 < round.1 && miter.1 < bevel.1,
            "l'onglet monte plus haut : {miter:?} vs {round:?} et {bevel:?}"
        );
        // Pixel juste au-dessus du sommet : plein pour l'onglet, partiel pour
        // l'arrondi, presque vide pour le biseau ; les aires suivent le même ordre.
        assert_eq!(masks[0].get(25, 7), 255);
        assert!(masks[1].get(25, 7) > masks[2].get(25, 7));
        assert!(areas[0] > areas[1] && areas[1] > areas[2], "{areas:?}");
        // Limite d'onglet dépassée : retour au biseau.
        let limited = StrokeStyle {
            width: 6.0,
            join: LineJoin::Miter,
            miter_limit: 1.0,
            ..StrokeStyle::default()
        };
        let m = coverage(&p, &limited, &Matrix::IDENTITY, 60, 40);
        assert_eq!(bbox(&m).unwrap(), bevel);
        assert_eq!(m, masks[2]);
    }

    #[test]
    fn dash_four_two_on_twelve_pixels_gives_two_dashes() {
        let style = StrokeStyle {
            width: 2.0,
            dash: Some((vec![4.0, 2.0], 0.0)),
            ..StrokeStyle::default()
        };
        let m = coverage(
            &line((0.0, 3.0), (12.0, 3.0)),
            &style,
            &Matrix::IDENTITY,
            14,
            6,
        );
        let row: Vec<bool> = (0..14).map(|x| m.get(x, 2) == 255).collect();
        let runs = row
            .iter()
            .fold((0, false), |(n, prev), &v| (n + usize::from(v && !prev), v))
            .0;
        assert_eq!(runs, 2, "{row:?}");
        assert!(row[0] && row[3] && !row[4] && !row[5] && row[6] && row[9] && !row[10]);
        // Phase de 4 : on commence dans le vide.
        let shifted = StrokeStyle {
            dash: Some((vec![4.0, 2.0], 4.0)),
            ..style.clone()
        };
        let m = coverage(
            &line((0.0, 3.0), (12.0, 3.0)),
            &shifted,
            &Matrix::IDENTITY,
            14,
            6,
        );
        assert_eq!(m.get(0, 2), 0);
        assert_eq!(m.get(3, 2), 255);
        // Motif invalide : ignoré, trait continu.
        let bad = StrokeStyle {
            dash: Some((vec![0.0, 0.0], 0.0)),
            ..style.clone()
        };
        let m = coverage(
            &line((0.0, 3.0), (12.0, 3.0)),
            &bad,
            &Matrix::IDENTITY,
            14,
            6,
        );
        assert!((0..12).all(|x| m.get(x, 2) == 255));
        let odd = StrokeStyle {
            dash: Some((vec![3.0], 0.0)),
            ..style
        };
        let m = coverage(
            &line((0.0, 3.0), (12.0, 3.0)),
            &odd,
            &Matrix::IDENTITY,
            14,
            6,
        );
        assert!(m.get(1, 2) == 255 && m.get(4, 2) == 0 && m.get(7, 2) == 255);
    }

    #[test]
    fn closed_subpath_has_joins_and_no_caps() {
        let mut p = Path::new();
        p.rect(&Rect::new(4.0, 4.0, 14.0, 14.0));
        let style = StrokeStyle {
            width: 2.0,
            join: LineJoin::Miter,
            ..StrokeStyle::default()
        };
        let m = coverage(&p, &style, &Matrix::IDENTITY, 20, 20);
        // Coin d'onglet plein au point de départ, intérieur vide.
        assert_eq!(m.get(3, 3), 255);
        assert_eq!(m.get(14, 14), 255);
        assert_eq!(m.get(9, 9), 0);
        assert_eq!(m.get(2, 2), 0);
    }

    #[test]
    fn zero_width_gives_one_pixel_line_and_anisotropic_scaling() {
        let m = coverage(
            &line((1.0, 3.0), (9.0, 3.0)),
            &StrokeStyle::with_width(0.0),
            &Matrix::IDENTITY,
            12,
            6,
        );
        // Largeur 1 px centrée sur y = 3 : deux demi-lignes à 50 %.
        assert!((i32::from(m.get(5, 2)) - 128).abs() <= 1);
        assert!((i32::from(m.get(5, 3)) - 128).abs() <= 1);
        assert_eq!(m.get(5, 1), 0);
        // Matrice anisotrope : largeur 2 en utilisateur devient 8 px en x et 2 px en y.
        let scale = Matrix::scale(4.0, 1.0);
        let horizontal = coverage(
            &line((1.0, 5.0), (5.0, 5.0)),
            &StrokeStyle::with_width(2.0),
            &scale,
            30,
            12,
        );
        let vertical = coverage(
            &line((3.0, 1.0), (3.0, 9.0)),
            &StrokeStyle::with_width(2.0),
            &scale,
            30,
            12,
        );
        let hb = bbox(&horizontal).unwrap();
        let vb = bbox(&vertical).unwrap();
        assert_eq!(hb.3 - hb.1 + 1, 2, "épaisseur verticale : {hb:?}");
        assert_eq!(vb.2 - vb.0 + 1, 8, "épaisseur horizontale : {vb:?}");
    }

    #[test]
    fn degenerate_subpaths_follow_cap_style() {
        let mut dot = Path::new();
        dot.move_to(Point::new(5.0, 5.0));
        dot.line_to(Point::new(5.0, 5.0));
        for (cap, expect_ink) in [
            (LineCap::Butt, false),
            (LineCap::Round, true),
            (LineCap::Square, true),
        ] {
            let style = StrokeStyle {
                width: 4.0,
                cap,
                ..StrokeStyle::default()
            };
            let m = coverage(&dot, &style, &Matrix::IDENTITY, 10, 10);
            assert_eq!(m.get(5, 5) > 0, expect_ink, "{cap:?}");
        }
        let square = coverage(
            &dot,
            &StrokeStyle {
                width: 4.0,
                cap: LineCap::Square,
                ..StrokeStyle::default()
            },
            &Matrix::IDENTITY,
            10,
            10,
        );
        assert_eq!(square.get(3, 3), 255);
        assert_eq!(square.get(2, 2), 0);
        // Chemin d'un seul `moveTo` : rien, pas de panique.
        let mut single = Path::new();
        single.move_to(Point::new(1.0, 1.0));
        assert!(stroke_path(&single, &StrokeStyle::default(), &Matrix::IDENTITY).is_empty());
    }

    #[test]
    fn invalid_inputs_give_empty_path() {
        let l = line((0.0, 0.0), (10.0, 0.0));
        assert!(stroke_path(&Path::new(), &StrokeStyle::default(), &Matrix::IDENTITY).is_empty());
        assert!(stroke_path(&l, &StrokeStyle::with_width(f64::NAN), &Matrix::IDENTITY).is_empty());
        assert!(stroke_path(&l, &StrokeStyle::with_width(-1.0), &Matrix::IDENTITY).is_empty());
        assert!(stroke_path(&l, &StrokeStyle::default(), &Matrix::scale(0.0, 1.0)).is_empty());
        assert!(stroke_path(
            &l,
            &StrokeStyle::default(),
            &Matrix::scale(f64::INFINITY, 1.0)
        )
        .is_empty());
        let mut nan = Path::new();
        nan.move_to(Point::new(f64::NAN, 0.0));
        nan.line_to(Point::new(5.0, 5.0));
        assert!(stroke_path(&nan, &StrokeStyle::default(), &Matrix::IDENTITY).is_empty());
        let style = StrokeStyle {
            miter_limit: f64::NAN,
            dash: Some((vec![f64::NAN, 1.0], f64::INFINITY)),
            ..StrokeStyle::default()
        };
        assert!(!stroke_path(&l, &style, &Matrix::IDENTITY).is_empty());
        // Segments nuls répétés et courbe.
        let mut p = Path::new();
        p.move_to(Point::new(1.0, 1.0));
        p.line_to(Point::new(1.0, 1.0));
        p.line_to(Point::new(1.0, 1.0));
        p.curve_to(
            Point::new(1.0, 1.0),
            Point::new(1.0, 1.0),
            Point::new(1.0, 1.0),
        );
        p.close();
        let _ = stroke_path(&p, &StrokeStyle::with_width(3.0), &Matrix::IDENTITY);
    }

    #[test]
    fn stroked_circle_is_a_ring() {
        let k = 0.552_284_749_8 * 20.0;
        let mut p = Path::new();
        p.move_to(Point::new(50.0, 30.0));
        p.curve_to(
            Point::new(50.0, 30.0 + k),
            Point::new(30.0 + k, 50.0),
            Point::new(30.0, 50.0),
        );
        p.curve_to(
            Point::new(30.0 - k, 50.0),
            Point::new(10.0, 30.0 + k),
            Point::new(10.0, 30.0),
        );
        p.curve_to(
            Point::new(10.0, 30.0 - k),
            Point::new(30.0 - k, 10.0),
            Point::new(30.0, 10.0),
        );
        p.curve_to(
            Point::new(30.0 + k, 10.0),
            Point::new(50.0, 30.0 - k),
            Point::new(50.0, 30.0),
        );
        p.close();
        let style = StrokeStyle {
            width: 4.0,
            join: LineJoin::Round,
            ..StrokeStyle::default()
        };
        let m = coverage(&p, &style, &Matrix::IDENTITY, 60, 60);
        let area: f64 = m.data().iter().map(|&v| f64::from(v) / 255.0).sum();
        let expected = std::f64::consts::PI * (22.0f64.powi(2) - 18.0f64.powi(2));
        assert!(
            (area - expected).abs() / expected < 0.02,
            "aire {area} attendue {expected}"
        );
        assert_eq!(m.get(30, 30), 0);
        assert_eq!(m.get(30, 10), 255);
    }
}
