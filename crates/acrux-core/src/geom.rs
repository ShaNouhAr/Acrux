//! Géométrie 2D : points, rectangles et matrices de transformation.
//!
//! Les matrices suivent la convention PDF (ISO 32000-2 §8.3.4) :
//! `[a b c d e f]` transforme `(x, y)` en `(a·x + c·y + e, b·x + d·y + f)`.
//! Les coordonnées PDF sont exprimées en points (1/72 de pouce), origine en
//! bas à gauche, axe Y vers le haut.

/// Point en coordonnées flottantes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    /// Abscisse.
    pub x: f64,
    /// Ordonnée.
    pub y: f64,
}

impl Point {
    /// Crée un point.
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// Rectangle défini par deux coins opposés, toujours normalisé
/// (`x0 <= x1`, `y0 <= y1`) par [`Rect::new`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    /// Bord gauche.
    pub x0: f64,
    /// Bord bas.
    pub y0: f64,
    /// Bord droit.
    pub x1: f64,
    /// Bord haut.
    pub y1: f64,
}

impl Rect {
    /// Crée un rectangle normalisé quel que soit l'ordre des coins
    /// (ISO 32000-2 §7.9.5 : un rectangle PDF peut être donné dans n'importe quel ordre).
    #[must_use]
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    /// Largeur.
    #[must_use]
    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    /// Hauteur.
    #[must_use]
    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }

    /// Vrai si le rectangle n'a pas de surface.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.width() <= 0.0 || self.height() <= 0.0
    }

    /// Intersection de deux rectangles ; rectangle vide s'ils ne se recouvrent pas.
    #[must_use]
    pub fn intersect(&self, other: &Rect) -> Rect {
        let r = Rect {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        if r.is_empty() {
            Rect::default()
        } else {
            r
        }
    }

    /// Plus petit rectangle contenant les deux.
    #[must_use]
    pub fn union(&self, other: &Rect) -> Rect {
        Rect {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    /// Vrai si le point est à l'intérieur (bords inclus).
    #[must_use]
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x0 && p.x <= self.x1 && p.y >= self.y0 && p.y <= self.y1
    }
}

/// Matrice de transformation affine 2D, convention PDF `[a b c d e f]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    /// Composante a.
    pub a: f64,
    /// Composante b.
    pub b: f64,
    /// Composante c.
    pub c: f64,
    /// Composante d.
    pub d: f64,
    /// Translation horizontale.
    pub e: f64,
    /// Translation verticale.
    pub f: f64,
}

impl Default for Matrix {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Matrix {
    /// Matrice identité.
    pub const IDENTITY: Matrix = Matrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// Crée une matrice à partir de ses six composantes.
    #[must_use]
    #[allow(clippy::many_single_char_names)] // noms imposés par la convention PDF [a b c d e f]
    pub const fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// Translation.
    #[must_use]
    pub const fn translate(tx: f64, ty: f64) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    /// Mise à l'échelle.
    #[must_use]
    pub const fn scale(sx: f64, sy: f64) -> Self {
        Self::new(sx, 0.0, 0.0, sy, 0.0, 0.0)
    }

    /// Rotation d'un angle en radians (sens trigonométrique).
    #[must_use]
    pub fn rotate(radians: f64) -> Self {
        let (s, c) = radians.sin_cos();
        Self::new(c, s, -s, c, 0.0, 0.0)
    }

    /// Produit `self × other` : applique d'abord `self`, puis `other`
    /// (ordre du PDF pour l'opérateur `cm` : `CTM' = M × CTM`).
    #[must_use]
    pub fn then(&self, other: &Matrix) -> Matrix {
        Matrix {
            a: self.a * other.a + self.b * other.c,
            b: self.a * other.b + self.b * other.d,
            c: self.c * other.a + self.d * other.c,
            d: self.c * other.b + self.d * other.d,
            e: self.e * other.a + self.f * other.c + other.e,
            f: self.e * other.b + self.f * other.d + other.f,
        }
    }

    /// Applique la transformation à un point.
    #[must_use]
    pub fn apply(&self, p: Point) -> Point {
        Point {
            x: self.a * p.x + self.c * p.y + self.e,
            y: self.b * p.x + self.d * p.y + self.f,
        }
    }

    /// Applique la transformation à un vecteur (ignore la translation).
    #[must_use]
    pub fn apply_vector(&self, p: Point) -> Point {
        Point {
            x: self.a * p.x + self.c * p.y,
            y: self.b * p.x + self.d * p.y,
        }
    }

    /// Déterminant de la partie linéaire.
    #[must_use]
    pub fn determinant(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }

    /// Inverse, ou `None` si la matrice est singulière.
    #[must_use]
    pub fn invert(&self) -> Option<Matrix> {
        let det = self.determinant();
        if det.abs() < f64::EPSILON {
            return None;
        }
        let inv = 1.0 / det;
        Some(Matrix {
            a: self.d * inv,
            b: -self.b * inv,
            c: -self.c * inv,
            d: self.a * inv,
            e: (self.c * self.f - self.d * self.e) * inv,
            f: (self.b * self.e - self.a * self.f) * inv,
        })
    }

    /// Boîte englobante d'un rectangle après transformation.
    #[must_use]
    pub fn transform_rect(&self, r: &Rect) -> Rect {
        let pts = [
            self.apply(Point::new(r.x0, r.y0)),
            self.apply(Point::new(r.x1, r.y0)),
            self.apply(Point::new(r.x1, r.y1)),
            self.apply(Point::new(r.x0, r.y1)),
        ];
        let mut out = Rect::new(pts[0].x, pts[0].y, pts[0].x, pts[0].y);
        for p in &pts[1..] {
            out.x0 = out.x0.min(p.x);
            out.y0 = out.y0.min(p.y);
            out.x1 = out.x1.max(p.x);
            out.y1 = out.y1.max(p.y);
        }
        out
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)] // autorisé dans les tests
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn rect_is_normalized() {
        let r = Rect::new(10.0, 20.0, 0.0, 5.0);
        assert_eq!(
            r,
            Rect {
                x0: 0.0,
                y0: 5.0,
                x1: 10.0,
                y1: 20.0
            }
        );
        assert!(close(r.width(), 10.0));
        assert!(close(r.height(), 15.0));
    }

    #[test]
    fn rect_intersection_and_union() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 20.0, 20.0);
        assert_eq!(a.intersect(&b), Rect::new(5.0, 5.0, 10.0, 10.0));
        assert_eq!(a.union(&b), Rect::new(0.0, 0.0, 20.0, 20.0));
        let c = Rect::new(50.0, 50.0, 60.0, 60.0);
        assert!(a.intersect(&c).is_empty());
    }

    #[test]
    fn matrix_translate_then_scale() {
        // Convention PDF : `then` applique self en premier.
        let m = Matrix::translate(10.0, 0.0).then(&Matrix::scale(2.0, 2.0));
        let p = m.apply(Point::new(1.0, 1.0));
        assert!(close(p.x, 22.0) && close(p.y, 2.0));
    }

    #[test]
    fn matrix_inverse_roundtrip() {
        let m = Matrix::new(2.0, 1.0, 0.5, 3.0, 7.0, -4.0);
        let Some(inv) = m.invert() else {
            panic!("la matrice devrait être inversible")
        };
        let id = m.then(&inv);
        assert!(close(id.a, 1.0) && close(id.b, 0.0));
        assert!(close(id.c, 0.0) && close(id.d, 1.0));
        assert!(close(id.e, 0.0) && close(id.f, 0.0));
    }

    #[test]
    fn singular_matrix_has_no_inverse() {
        assert!(Matrix::new(1.0, 2.0, 2.0, 4.0, 0.0, 0.0).invert().is_none());
    }

    #[test]
    fn rotated_rect_bbox() {
        let r = Rect::new(0.0, 0.0, 10.0, 0.0);
        let bb = Matrix::rotate(std::f64::consts::FRAC_PI_2).transform_rect(&r);
        assert!(close(bb.x0, 0.0) && close(bb.x1, 0.0));
        assert!(close(bb.y0, 0.0) && close(bb.y1, 10.0));
    }
}
