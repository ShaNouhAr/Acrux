//! Géométrie et apparence des formes dessinées : rectangle, ellipse, ligne,
//! polyligne, polygone, encre (§12.5.6.8 à §12.5.6.13).
//!
//! Tout ici est **pur** : des points et un style en entrée, le contenu d'un
//! flux d'apparence et sa boîte en sortie, sans toucher au document. C'est
//! ce qui permet de régénérer une apparence après un déplacement ou un
//! changement de couleur (les outils de gestion des commentaires), et à
//! l'application de dessiner l'aperçu d'un geste avec **les mêmes**
//! formules — une tête de flèche calculée deux fois finirait par différer.
//!
//! Le contenu est écrit dans l'espace de la page, sans `/Matrix` : la boîte
//! de l'apparence est aussi le `/Rect` de l'annotation, et tout lecteur la
//! pose donc à l'identique.

use std::fmt::Write as _;

use acrux_core::{Point, Rect};

use super::{color_op, fmt, LineEnding, ShapeStyle};

/// Constante de Bézier d'un quart d'ellipse, la même que le rendu.
const KAPPA: f64 = 0.552_284_75;

/// Demi-angle d'ouverture des têtes de flèche : 30°, celui d'Acrobat.
const HEAD_HALF_ANGLE: f64 = std::f64::consts::FRAC_PI_6;

/// Tolérance de la simplification d'un trait d'encre, en points. En deçà
/// d'un demi-pixel à 100 %, un point de plus ne change rien à ce qu'on voit.
pub const INK_TOLERANCE: f64 = 0.35;

/// Plafond de points par trait d'encre, après simplification : un geste
/// aberrant (des heures de griffonnage) ne doit pas produire un flux géant.
const MAX_INK_POINTS: usize = 20_000;

/// Contenu d'un flux d'apparence et sa boîte, dans l'espace de la page.
#[derive(Debug, Clone, PartialEq)]
pub struct Drawn {
    /// Opérateurs de contenu.
    pub content: String,
    /// Boîte qui contient tout le dessin, épaisseur du trait comprise.
    pub bbox: Rect,
}

/// Vrai si le trait se voit : une couleur et une épaisseur.
fn strokes(style: &ShapeStyle) -> bool {
    style.stroke.is_some() && style.width > 0.0
}

/// Début du contenu : état graphique (opacité), couleurs, épaisseur ; bouts
/// et joints ronds si `round` (traits à main levée, lignes).
fn prefix(style: &ShapeStyle, round: bool) -> String {
    let mut out = String::new();
    if style.opacity < 1.0 {
        out.push_str("/GS0 gs ");
    }
    if let Some(c) = style.stroke {
        let _ = write!(out, "{} {} w ", color_op(c, true), fmt(style.width));
    }
    if let Some(f) = style.fill {
        let _ = write!(out, "{} ", color_op(f, false));
    }
    if round {
        out.push_str("1 J 1 j ");
    }
    out
}

/// Opérateur de peinture d'un chemin fermé : trait et fond, l'un ou
/// l'autre, ou rien.
fn paint_closed(style: &ShapeStyle) -> &'static str {
    match (strokes(style), style.fill.is_some()) {
        (true, true) => "B",
        (true, false) => "S",
        (false, true) => "f",
        (false, false) => "n",
    }
}

/// Rectangle réduit de `inset` de chaque côté, sans jamais s'inverser.
fn inset(r: &Rect, by: f64) -> Rect {
    let bx = by.min(r.width() / 2.0);
    let byy = by.min(r.height() / 2.0);
    Rect::new(r.x0 + bx, r.y0 + byy, r.x1 - bx, r.y1 - byy)
}

/// Rectangle (`/Square`) : le trait est rentré d'une demi-épaisseur, pour
/// que la forme ne déborde pas de la zone tracée.
#[must_use]
pub fn rectangle(rect: &Rect, style: &ShapeStyle) -> Drawn {
    let half = if strokes(style) {
        style.width / 2.0
    } else {
        0.0
    };
    let r = inset(rect, half);
    let content = format!(
        "{}{} {} {} {} re {}",
        prefix(style, false),
        fmt(r.x0),
        fmt(r.y0),
        fmt(r.width()),
        fmt(r.height()),
        paint_closed(style)
    );
    Drawn {
        content,
        bbox: *rect,
    }
}

/// Chemin d'une ellipse inscrite dans `r` : quatre arcs de Bézier, les
/// mêmes que le rendu de secours d'une annotation sans apparence.
#[must_use]
pub fn ellipse_ops(r: &Rect) -> String {
    let (rx, ry) = (r.width() / 2.0, r.height() / 2.0);
    let (cx, cy) = (r.x0 + rx, r.y0 + ry);
    let (kx, ky) = (KAPPA * rx, KAPPA * ry);
    let p = |x: f64, y: f64| format!("{} {}", fmt(x), fmt(y));
    format!(
        "{} m {} {} {} c {} {} {} c {} {} {} c {} {} {} c h",
        p(cx + rx, cy),
        p(cx + rx, cy + ky),
        p(cx + kx, cy + ry),
        p(cx, cy + ry),
        p(cx - kx, cy + ry),
        p(cx - rx, cy + ky),
        p(cx - rx, cy),
        p(cx - rx, cy - ky),
        p(cx - kx, cy - ry),
        p(cx, cy - ry),
        p(cx + kx, cy - ry),
        p(cx + rx, cy - ky),
        p(cx + rx, cy),
    )
}

/// Ellipse (`/Circle`) inscrite dans la zone, trait rentré comme le
/// rectangle.
#[must_use]
pub fn ellipse(rect: &Rect, style: &ShapeStyle) -> Drawn {
    let half = if strokes(style) {
        style.width / 2.0
    } else {
        0.0
    };
    let r = inset(rect, half);
    let content = format!(
        "{}{} {}",
        prefix(style, false),
        ellipse_ops(&r),
        paint_closed(style)
    );
    Drawn {
        content,
        bbox: *rect,
    }
}

/// Longueur d'une tête de flèche pour une épaisseur de trait : quatre fois
/// l'épaisseur, jamais moins de 6 pt — une flèche d'un trait fin doit
/// encore se voir.
#[must_use]
pub fn head_length(width: f64) -> f64 {
    (4.0 * width).max(6.0)
}

/// Vecteur unitaire de `from` vers `to` ; `None` si les points se
/// confondent (aucune direction à donner à une tête).
fn unit(from: Point, to: Point) -> Option<Point> {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let len = dx.hypot(dy);
    (len > 1e-9).then(|| Point::new(dx / len, dy / len))
}

/// Forme d'une terminaison de ligne, dans l'espace de la page.
#[derive(Debug, Clone, PartialEq)]
pub enum EndShape {
    /// Trait ouvert : une aile, la pointe, l'autre aile (flèche ouverte),
    /// ou un segment (butée).
    Open(Vec<Point>),
    /// Polygone fermé, rempli : triangle d'une flèche fermée, carré.
    Closed(Vec<Point>),
    /// Disque : centre et rayon.
    Disc(Point, f64),
}

/// Terminaison `ending` posée à l'extrémité `tip` d'une ligne qui vient de
/// `from`, pour un trait d'épaisseur `width`. `None` pour
/// [`LineEnding::None`] ou une ligne de longueur nulle.
///
/// C'est la seule source de cette géométrie : l'aperçu de l'application
/// l'appelle aussi.
#[must_use]
pub fn ending_shape(tip: Point, from: Point, width: f64, ending: LineEnding) -> Option<EndShape> {
    let u = unit(from, tip)?;
    let n = Point::new(-u.y, u.x);
    let len = head_length(width);
    let half = len * HEAD_HALF_ANGLE.tan();
    let wing = |side: f64| {
        Point::new(
            tip.x - len * u.x + side * half * n.x,
            tip.y - len * u.y + side * half * n.y,
        )
    };
    let small = (1.5 * width).max(3.0);
    match ending {
        LineEnding::None => None,
        LineEnding::OpenArrow => Some(EndShape::Open(vec![wing(1.0), tip, wing(-1.0)])),
        LineEnding::ClosedArrow => Some(EndShape::Closed(vec![wing(1.0), tip, wing(-1.0)])),
        LineEnding::Circle => Some(EndShape::Disc(tip, small)),
        LineEnding::Square => {
            let (a, b) = (
                Point::new(small * u.x, small * u.y),
                Point::new(small * n.x, small * n.y),
            );
            Some(EndShape::Closed(vec![
                Point::new(tip.x + a.x + b.x, tip.y + a.y + b.y),
                Point::new(tip.x - a.x + b.x, tip.y - a.y + b.y),
                Point::new(tip.x - a.x - b.x, tip.y - a.y - b.y),
                Point::new(tip.x + a.x - b.x, tip.y + a.y - b.y),
            ]))
        }
        LineEnding::Butt => Some(EndShape::Open(vec![
            Point::new(tip.x + half * n.x, tip.y + half * n.y),
            Point::new(tip.x - half * n.x, tip.y - half * n.y),
        ])),
    }
}

/// Point où le trait de la ligne s'arrête devant une terminaison : à la
/// base d'une flèche fermée — sans quoi le bout carré du trait dépasserait
/// de la pointe —, au bout sinon.
#[must_use]
pub fn line_stop(tip: Point, from: Point, width: f64, ending: LineEnding) -> Point {
    match (ending, unit(from, tip)) {
        (LineEnding::ClosedArrow, Some(u)) => {
            let back = head_length(width) * 0.9;
            Point::new(tip.x - back * u.x, tip.y - back * u.y)
        }
        _ => tip,
    }
}

/// Points qui bornent une terminaison (pour la boîte de l'annotation).
fn end_points(shape: &EndShape) -> Vec<Point> {
    match shape {
        EndShape::Open(p) | EndShape::Closed(p) => p.clone(),
        EndShape::Disc(c, r) => vec![Point::new(c.x - r, c.y - r), Point::new(c.x + r, c.y + r)],
    }
}

/// Plus petite boîte qui contient les points, gonflée de `margin`.
#[must_use]
pub fn bounds_of(points: &[Point], margin: f64) -> Rect {
    let mut it = points.iter();
    let Some(first) = it.next() else {
        return Rect::default();
    };
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
    for p in it {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    Rect::new(x0 - margin, y0 - margin, x1 + margin, y1 + margin)
}

/// Écrit une terminaison. Les formes fermées se remplissent de la couleur
/// intérieure (`/IC`) s'il y en a une, sinon de celle du trait : une
/// flèche fermée creuse sur une ligne rouge se lit mal.
fn write_end(out: &mut String, shape: &EndShape, style: &ShapeStyle) {
    let Some(stroke) = style.stroke else {
        return;
    };
    let fill = style.fill.unwrap_or(stroke);
    match shape {
        EndShape::Open(points) => {
            write_polyline(out, points);
            out.push_str("S ");
        }
        EndShape::Closed(points) => {
            let _ = write!(out, "{} ", color_op(fill, false));
            write_polyline(out, points);
            out.push_str("b ");
        }
        EndShape::Disc(c, r) => {
            let _ = write!(out, "{} ", color_op(fill, false));
            out.push_str(&ellipse_ops(&Rect::new(c.x - r, c.y - r, c.x + r, c.y + r)));
            out.push_str(" b ");
        }
    }
}

/// `x y m x y l …` pour une suite de points.
fn write_polyline(out: &mut String, points: &[Point]) {
    for (i, p) in points.iter().enumerate() {
        let _ = write!(
            out,
            "{} {} {} ",
            fmt(p.x),
            fmt(p.y),
            if i == 0 { "m" } else { "l" }
        );
    }
}

/// Ligne ouverte (`/Line` à deux points, `/PolyLine` au-delà), avec ses
/// terminaisons au premier et au dernier point.
///
/// Rend `None` s'il y a moins de deux points.
#[must_use]
pub fn open_path(
    points: &[Point],
    style: &ShapeStyle,
    start: LineEnding,
    end: LineEnding,
) -> Option<Drawn> {
    let (&first, &last) = (points.first()?, points.last()?);
    if points.len() < 2 {
        return None;
    }
    let (second, before_last) = (points[1], points[points.len() - 2]);
    let w = style.width;
    let heads: Vec<EndShape> = [
        ending_shape(first, second, w, start),
        ending_shape(last, before_last, w, end),
    ]
    .into_iter()
    .flatten()
    .collect();
    let mut body = points.to_vec();
    body[0] = line_stop(first, second, w, start);
    let n = body.len();
    body[n - 1] = line_stop(last, before_last, w, end);
    let mut content = prefix(
        &ShapeStyle {
            fill: None,
            ..*style
        },
        true,
    );
    write_polyline(&mut content, &body);
    content.push_str("S ");
    let mut extent: Vec<Point> = points.to_vec();
    for h in &heads {
        write_end(&mut content, h, style);
        extent.extend(end_points(h));
    }
    Some(Drawn {
        content: content.trim_end().to_string(),
        bbox: bounds_of(&extent, w / 2.0 + 1.0),
    })
}

/// Polygone fermé (`/Polygon`), rempli s'il a une couleur intérieure.
///
/// Rend `None` s'il y a moins de trois sommets.
#[must_use]
pub fn polygon(points: &[Point], style: &ShapeStyle) -> Option<Drawn> {
    if points.len() < 3 {
        return None;
    }
    let mut content = prefix(style, false);
    write_polyline(&mut content, points);
    let _ = write!(content, "h {}", paint_closed(style));
    // Un sommet en pointe dépasse, joint en onglet, d'au plus dix
    // demi-épaisseurs (la limite d'onglet par défaut) : on laisse la place.
    let margin = if strokes(style) {
        style.width * 5.0
    } else {
        0.0
    };
    Some(Drawn {
        content,
        bbox: bounds_of(points, margin + 1.0),
    })
}

/// Distance d'un point au segment `a`–`b`.
fn segment_distance(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    if len2 <= 1e-18 {
        return (p.x - a.x).hypot(p.y - a.y);
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0);
    (p.x - (a.x + t * dx)).hypot(p.y - (a.y + t * dy))
}

/// Simplifie un trait d'encre (Ramer-Douglas-Peucker) : ne garde que les
/// points qui s'écartent de plus de `tolerance` de la corde. Les deux
/// extrémités restent toujours ; un point isolé aussi — un simple clic du
/// crayon laisse une trace.
///
/// La pile explicite évite la récursion : un trait de cent mille points
/// ne doit pas épuiser la pile d'appel.
#[must_use]
pub fn simplify(points: &[Point], tolerance: f64) -> Vec<Point> {
    let finite: Vec<Point> = points
        .iter()
        .copied()
        .filter(|p| p.x.is_finite() && p.y.is_finite())
        .collect();
    if finite.len() <= 2 {
        return finite;
    }
    let mut keep = vec![false; finite.len()];
    keep[0] = true;
    keep[finite.len() - 1] = true;
    let mut stack = vec![(0, finite.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (mut far, mut index) = (0.0, a);
        for (i, p) in finite.iter().enumerate().take(b).skip(a + 1) {
            let d = segment_distance(*p, finite[a], finite[b]);
            if d > far {
                far = d;
                index = i;
            }
        }
        if far > tolerance {
            keep[index] = true;
            stack.push((a, index));
            stack.push((index, b));
        }
    }
    let mut out: Vec<Point> = finite
        .into_iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(p))
        .collect();
    if out.len() > MAX_INK_POINTS {
        let step = out.len().div_ceil(MAX_INK_POINTS);
        let last = out[out.len() - 1];
        out = out.into_iter().step_by(step).collect();
        if out.last() != Some(&last) {
            out.push(last);
        }
    }
    out
}

/// Courbe lisse qui passe par les points d'un trait : chaque segment est un
/// arc de Bézier dont les tangentes suivent les voisins (Catmull-Rom). Rend
/// les triplets `(c1, c2, point)` qui suivent le premier point.
#[must_use]
pub fn smooth(points: &[Point]) -> Vec<(Point, Point, Point)> {
    let n = points.len();
    let mut out = Vec::with_capacity(n.saturating_sub(1));
    for i in 0..n.saturating_sub(1) {
        let p0 = points[i.saturating_sub(1)];
        let p1 = points[i];
        let p2 = points[i + 1];
        let p3 = points[(i + 2).min(n - 1)];
        let c1 = Point::new(p1.x + (p2.x - p0.x) / 6.0, p1.y + (p2.y - p0.y) / 6.0);
        let c2 = Point::new(p2.x - (p3.x - p1.x) / 6.0, p2.y - (p3.y - p1.y) / 6.0);
        out.push((c1, c2, p2));
    }
    out
}

/// Encre (`/Ink`) : chaque trait, déjà simplifié, devient une courbe lisse
/// à bouts ronds. Un trait d'un seul point est un point.
///
/// Rend `None` sans aucun point.
#[must_use]
pub fn ink(strokes_in: &[Vec<Point>], style: &ShapeStyle) -> Option<Drawn> {
    let mut content = prefix(
        &ShapeStyle {
            fill: None,
            ..*style
        },
        true,
    );
    let mut extent = Vec::new();
    for stroke in strokes_in {
        let Some(&first) = stroke.first() else {
            continue;
        };
        let _ = write!(content, "{} {} m ", fmt(first.x), fmt(first.y));
        extent.push(first);
        if stroke.len() == 1 {
            // Un point : un segment minuscule, que le bout rond arrondit.
            let _ = write!(content, "{} {} l ", fmt(first.x + 0.01), fmt(first.y));
        }
        for (c1, c2, p) in smooth(stroke) {
            let _ = write!(
                content,
                "{} {} {} {} {} {} c ",
                fmt(c1.x),
                fmt(c1.y),
                fmt(c2.x),
                fmt(c2.y),
                fmt(p.x),
                fmt(p.y)
            );
            // Une courbe de Bézier tient dans l'enveloppe de ses points de
            // contrôle : la boîte les compte.
            extent.extend([c1, c2, p]);
        }
        content.push_str("S ");
    }
    if extent.is_empty() {
        return None;
    }
    Some(Drawn {
        content: content.trim_end().to_string(),
        bbox: bounds_of(&extent, style.width / 2.0 + 1.0),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp, clippy::panic)]
mod tests {
    use super::*;

    fn red() -> ShapeStyle {
        ShapeStyle {
            stroke: Some([1.0, 0.0, 0.0]),
            fill: None,
            width: 2.0,
            opacity: 1.0,
        }
    }

    #[test]
    fn rectangle_rentre_le_trait() {
        let d = rectangle(&Rect::new(10.0, 10.0, 60.0, 40.0), &red());
        assert!(d.content.contains("11 11 48 28 re S"), "{}", d.content);
        assert_eq!(d.bbox, Rect::new(10.0, 10.0, 60.0, 40.0));
        let filled = ShapeStyle {
            fill: Some([1.0, 1.0, 0.0]),
            opacity: 0.5,
            ..red()
        };
        let d = rectangle(&Rect::new(10.0, 10.0, 60.0, 40.0), &filled);
        assert!(d.content.starts_with("/GS0 gs "), "{}", d.content);
        assert!(d.content.ends_with(" B"), "{}", d.content);
    }

    #[test]
    fn fleche_ouverte_et_fermee() {
        let (from, tip) = (Point::new(0.0, 0.0), Point::new(100.0, 0.0));
        let Some(EndShape::Open(p)) = ending_shape(tip, from, 2.0, LineEnding::OpenArrow) else {
            panic!("flèche ouverte attendue");
        };
        assert_eq!(p[1], tip);
        assert!((p[0].x - 92.0).abs() < 1e-9 && p[0].y > 0.0, "{p:?}");
        assert!((p[2].y + p[0].y).abs() < 1e-9, "ailes symétriques");
        assert!(ending_shape(tip, tip, 2.0, LineEnding::OpenArrow).is_none());
        let stop = line_stop(tip, from, 2.0, LineEnding::ClosedArrow);
        assert!(stop.x < 100.0 && stop.x > 90.0, "{stop:?}");
        assert_eq!(line_stop(tip, from, 2.0, LineEnding::OpenArrow), tip);
        let d = open_path(
            &[from, tip],
            &red(),
            LineEnding::None,
            LineEnding::ClosedArrow,
        )
        .unwrap();
        assert!(d.content.contains(" b"), "{}", d.content);
        assert!(d.bbox.x1 >= 101.0 && d.bbox.y1 > 4.0, "{:?}", d.bbox);
    }

    #[test]
    fn simplification_garde_les_bouts() {
        let points: Vec<Point> = (0..500)
            .map(|i| Point::new(f64::from(i), (f64::from(i) * 0.3).sin() * 0.05))
            .collect();
        let s = simplify(&points, INK_TOLERANCE);
        assert!(s.len() < 20, "{}", s.len());
        assert_eq!(s[0], points[0]);
        assert_eq!(*s.last().unwrap(), points[499]);
        // Un coude franc est gardé.
        let elbow = [
            Point::new(0.0, 0.0),
            Point::new(5.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 5.0),
            Point::new(10.0, 10.0),
        ];
        assert_eq!(simplify(&elbow, INK_TOLERANCE).len(), 3);
        assert_eq!(simplify(&[Point::new(1.0, 1.0)], 0.35).len(), 1);
    }

    #[test]
    fn encre_point_isole_visible() {
        let d = ink(&[vec![Point::new(50.0, 50.0)]], &red()).unwrap();
        assert!(d.content.contains("1 J 1 j"), "{}", d.content);
        assert!(d.content.contains("50.01 50 l"), "{}", d.content);
        assert!(d.bbox.contains(Point::new(50.0, 50.0)));
        assert!(ink(&[], &red()).is_none());
    }

    #[test]
    fn polygone_ferme() {
        let pts = [
            Point::new(10.0, 10.0),
            Point::new(50.0, 90.0),
            Point::new(90.0, 10.0),
        ];
        let d = polygon(&pts, &red()).unwrap();
        assert!(d.content.ends_with("h S"), "{}", d.content);
        assert!(polygon(&pts[..2], &red()).is_none());
    }
}
