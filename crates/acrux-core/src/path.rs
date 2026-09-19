//! Chemin vectoriel partagé par les polices (contours de glyphes), le
//! rasteriseur et l'interpréteur de contenu (opérateurs `m l c v y h re`).
//!
//! Les courbes quadratiques (TrueType) sont converties en cubiques à la
//! construction : le rasteriseur ne connaît que segments et cubiques.

use crate::geom::{Matrix, Point, Rect};

/// Commande de chemin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathCommand {
    /// Début d'un sous-chemin.
    MoveTo(Point),
    /// Segment de droite.
    LineTo(Point),
    /// Courbe de Bézier cubique : deux points de contrôle puis point final.
    CurveTo(Point, Point, Point),
    /// Fermeture du sous-chemin courant.
    Close,
}

/// Chemin : suite de sous-chemins.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Path {
    commands: Vec<PathCommand>,
    /// Point de départ du sous-chemin courant (pour `Close` et les quadratiques).
    start: Point,
    current: Point,
}

impl Path {
    /// Chemin vide.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Commandes.
    #[must_use]
    pub fn commands(&self) -> &[PathCommand] {
        &self.commands
    }

    /// Vrai si le chemin ne contient aucune commande.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Point courant.
    #[must_use]
    pub const fn current_point(&self) -> Point {
        self.current
    }

    /// Nouveau sous-chemin.
    pub fn move_to(&mut self, p: Point) {
        self.commands.push(PathCommand::MoveTo(p));
        self.start = p;
        self.current = p;
    }

    /// Segment.
    pub fn line_to(&mut self, p: Point) {
        if self.commands.is_empty() {
            self.move_to(p);
            return;
        }
        self.commands.push(PathCommand::LineTo(p));
        self.current = p;
    }

    /// Cubique.
    pub fn curve_to(&mut self, c1: Point, c2: Point, p: Point) {
        if self.commands.is_empty() {
            self.move_to(c1);
        }
        self.commands.push(PathCommand::CurveTo(c1, c2, p));
        self.current = p;
    }

    /// Quadratique (TrueType), élevée en cubique.
    pub fn quad_to(&mut self, c: Point, p: Point) {
        let p0 = self.current;
        let c1 = Point::new(
            p0.x + 2.0 / 3.0 * (c.x - p0.x),
            p0.y + 2.0 / 3.0 * (c.y - p0.y),
        );
        let c2 = Point::new(p.x + 2.0 / 3.0 * (c.x - p.x), p.y + 2.0 / 3.0 * (c.y - p.y));
        self.curve_to(c1, c2, p);
    }

    /// Ferme le sous-chemin courant.
    pub fn close(&mut self) {
        if matches!(self.commands.last(), Some(PathCommand::Close) | None) {
            return;
        }
        self.commands.push(PathCommand::Close);
        self.current = self.start;
    }

    /// Rectangle (opérateur `re`, §8.5.2.1) : sous-chemin fermé.
    pub fn rect(&mut self, r: &Rect) {
        self.move_to(Point::new(r.x0, r.y0));
        self.line_to(Point::new(r.x1, r.y0));
        self.line_to(Point::new(r.x1, r.y1));
        self.line_to(Point::new(r.x0, r.y1));
        self.close();
    }

    /// Ajoute les commandes d'un autre chemin.
    pub fn append(&mut self, other: &Path) {
        self.commands.extend_from_slice(&other.commands);
        self.start = other.start;
        self.current = other.current;
    }

    /// Chemin transformé par une matrice.
    #[must_use]
    pub fn transform(&self, m: &Matrix) -> Path {
        let commands = self
            .commands
            .iter()
            .map(|c| match *c {
                PathCommand::MoveTo(p) => PathCommand::MoveTo(m.apply(p)),
                PathCommand::LineTo(p) => PathCommand::LineTo(m.apply(p)),
                PathCommand::CurveTo(a, b, p) => {
                    PathCommand::CurveTo(m.apply(a), m.apply(b), m.apply(p))
                }
                PathCommand::Close => PathCommand::Close,
            })
            .collect();
        Path {
            commands,
            start: m.apply(self.start),
            current: m.apply(self.current),
        }
    }

    /// Boîte englobante des points de contrôle (majorant de la boîte réelle),
    /// `None` si le chemin est vide.
    #[must_use]
    pub fn bounds(&self) -> Option<Rect> {
        let mut pts = self.commands.iter().flat_map(|c| match *c {
            PathCommand::MoveTo(p) | PathCommand::LineTo(p) => vec![p],
            PathCommand::CurveTo(a, b, p) => vec![a, b, p],
            PathCommand::Close => vec![],
        });
        let first = pts.next()?;
        let mut r = Rect::new(first.x, first.y, first.x, first.y);
        for p in pts {
            r.x0 = r.x0.min(p.x);
            r.y0 = r.y0.min(p.y);
            r.x1 = r.x1.max(p.x);
            r.y1 = r.y1.max(p.y);
        }
        Some(r)
    }

    /// Aplatit les courbes en segments avec une tolérance en unités du chemin.
    /// Retourne une liste de polygones (un par sous-chemin) avec un drapeau
    /// « fermé ».
    #[must_use]
    pub fn flatten(&self, tolerance: f64) -> Vec<(Vec<Point>, bool)> {
        let tolerance = tolerance.max(1e-4);
        let mut out: Vec<(Vec<Point>, bool)> = Vec::new();
        let mut cur: Vec<Point> = Vec::new();
        let mut last = Point::default();
        let flush = |cur: &mut Vec<Point>, out: &mut Vec<(Vec<Point>, bool)>, closed: bool| {
            if cur.len() > 1 || (closed && !cur.is_empty()) {
                out.push((std::mem::take(cur), closed));
            } else {
                cur.clear();
            }
        };
        for c in &self.commands {
            match *c {
                PathCommand::MoveTo(p) => {
                    flush(&mut cur, &mut out, false);
                    cur.push(p);
                    last = p;
                }
                PathCommand::LineTo(p) => {
                    cur.push(p);
                    last = p;
                }
                PathCommand::CurveTo(a, b, p) => {
                    flatten_cubic(last, a, b, p, tolerance, &mut cur);
                    last = p;
                }
                PathCommand::Close => {
                    let start = cur.first().copied();
                    flush(&mut cur, &mut out, true);
                    if let Some(s) = start {
                        last = s;
                        // Un tracé qui continue après `h` repart du point de départ.
                        cur.push(s);
                    }
                }
            }
        }
        flush(&mut cur, &mut out, false);
        out
    }
}

/// Subdivision adaptative d'une cubique en segments (critère de planéité).
#[allow(clippy::many_single_char_names, clippy::cast_precision_loss)] // notation de Bernstein
fn flatten_cubic(p0: Point, p1: Point, p2: Point, p3: Point, tolerance: f64, out: &mut Vec<Point>) {
    // Nombre de segments d'après la dérivée seconde maximale (borne de Wang).
    let ddx = (p0.x - 2.0 * p1.x + p2.x)
        .abs()
        .max((p1.x - 2.0 * p2.x + p3.x).abs());
    let ddy = (p0.y - 2.0 * p1.y + p2.y)
        .abs()
        .max((p1.y - 2.0 * p2.y + p3.y).abs());
    let dd = (ddx * ddx + ddy * ddy).sqrt();
    let n = ((dd / tolerance * 0.75).sqrt().ceil()).clamp(1.0, 256.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = n as usize;
    for i in 1..=n {
        let t = i as f64 / n as f64;
        let mt = 1.0 - t;
        let a = mt * mt * mt;
        let b = 3.0 * mt * mt * t;
        let c = 3.0 * mt * t * t;
        let d = t * t * t;
        out.push(Point::new(
            a * p0.x + b * p1.x + c * p2.x + d * p3.x,
            a * p0.y + b * p1.y + c * p2.y + d * p3.y,
        ));
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn rect_and_bounds() {
        let mut p = Path::new();
        p.rect(&Rect::new(1.0, 2.0, 4.0, 6.0));
        assert_eq!(p.commands().len(), 5);
        assert_eq!(p.bounds(), Some(Rect::new(1.0, 2.0, 4.0, 6.0)));
        assert_eq!(p.current_point(), Point::new(1.0, 2.0));
    }

    #[test]
    fn quad_is_elevated_to_cubic() {
        let mut p = Path::new();
        p.move_to(Point::new(0.0, 0.0));
        p.quad_to(Point::new(3.0, 0.0), Point::new(3.0, 3.0));
        match p.commands()[1] {
            PathCommand::CurveTo(a, b, e) => {
                assert_eq!(a, Point::new(2.0, 0.0));
                assert_eq!(b, Point::new(3.0, 1.0));
                assert_eq!(e, Point::new(3.0, 3.0));
            }
            ref other => panic!("cubique attendue, obtenu {other:?}"),
        }
    }

    #[test]
    fn flatten_line_and_curve() {
        let mut p = Path::new();
        p.move_to(Point::new(0.0, 0.0));
        p.line_to(Point::new(10.0, 0.0));
        p.curve_to(
            Point::new(10.0, 5.0),
            Point::new(5.0, 10.0),
            Point::new(0.0, 10.0),
        );
        p.close();
        let polys = p.flatten(0.1);
        assert_eq!(polys.len(), 1);
        let (pts, closed) = &polys[0];
        assert!(*closed);
        assert!(pts.len() > 4, "la courbe doit être subdivisée");
        assert_eq!(pts[0], Point::new(0.0, 0.0));
        assert_eq!(*pts.last().unwrap(), Point::new(0.0, 10.0));
        // Tous les points de la courbe restent dans la boîte englobante.
        for q in pts {
            assert!(q.x >= -1e-9 && q.x <= 10.0 + 1e-9 && q.y >= -1e-9 && q.y <= 10.0 + 1e-9);
        }
    }

    #[test]
    fn transform_and_append() {
        let mut p = Path::new();
        p.move_to(Point::new(1.0, 1.0));
        p.line_to(Point::new(2.0, 1.0));
        let t = p.transform(&Matrix::scale(2.0, 3.0));
        assert_eq!(t.commands()[1], PathCommand::LineTo(Point::new(4.0, 3.0)));
        let mut q = Path::new();
        q.append(&t);
        q.append(&p);
        assert_eq!(q.commands().len(), 4);
        assert_eq!(q.current_point(), Point::new(2.0, 1.0));
    }

    #[test]
    fn close_is_idempotent_and_empty_path_ignores_close() {
        let mut p = Path::new();
        p.close();
        assert!(p.is_empty());
        p.move_to(Point::new(0.0, 0.0));
        p.line_to(Point::new(1.0, 0.0));
        p.close();
        p.close();
        assert_eq!(p.commands().len(), 3);
    }
}
