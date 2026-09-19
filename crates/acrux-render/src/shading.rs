//! Ombrages (ISO 32000-2 §8.7.4.5) : types 1 (fonction), 2 (axial),
//! 3 (radial), 4 et 5 (maillages de triangles), 6 et 7 (patches de Coons et
//! de tenseurs). Les types 1 à 3 deviennent des [`Paint`] du rasteriseur ;
//! les maillages deviennent des triangles à interpolation de Gouraud.

// Lecture de flux binaires (valeurs sur au plus 32 bits converties en f64 :
// aucune perte réelle), notation de la spécification à une lettre (x, y, u, v)
// et `Shading::parse` qui suit la table des types 1 à 7.
#![allow(
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    clippy::too_many_lines
)]

use std::rc::Rc;

use acrux_core::{Error, Matrix, Path, Point, Rect, Result};
use acrux_document::{Dict, Document, Name, Object};
use acrux_graphics::{Color, Gradient, Paint};

use crate::colorspace::ColorSpace;
use crate::function::Function;

/// Triangle à couleurs par sommet (Gouraud), en espace de l'ombrage.
#[derive(Debug, Clone)]
pub struct Triangle {
    /// Sommets.
    pub points: [Point; 3],
    /// Couleurs RVB des sommets.
    pub colors: [[f32; 3]; 3],
}

/// Ombrage analysé.
#[derive(Debug, Clone)]
pub struct Shading {
    /// `/ShadingType` 1 à 7.
    pub kind: i64,
    /// Espace colorimétrique des couleurs de l'ombrage.
    pub colorspace: ColorSpace,
    /// Couleur de fond (`/Background`), utilisée seulement par `sh` hors de l'étendue… en pratique ignorée par Acrobat pour `sh`.
    pub background: Option<[f32; 3]>,
    /// Boîte de restriction (`/BBox`) en espace de l'ombrage.
    pub bbox: Option<Rect>,
    /// Fonction(s) de couleur.
    pub function: Option<Rc<Function>>,
    /// `/Coords`.
    pub coords: Vec<f64>,
    /// `/Domain`.
    pub domain: Vec<f64>,
    /// `/Extend`.
    pub extend: (bool, bool),
    /// Matrice du type 1 (`/Matrix`).
    pub matrix: Matrix,
    /// Triangles des types 4 à 7 (déjà décodés).
    pub triangles: Vec<Triangle>,
    /// `/AntiAlias`.
    pub anti_alias: bool,
}

/// Nombre d'arrêts échantillonnés sur la fonction d'un dégradé.
const GRADIENT_SAMPLES: usize = 256;

impl Shading {
    /// Analyse un dictionnaire ou flux d'ombrage.
    ///
    /// # Errors
    /// Type inconnu ou paramètres essentiels absents.
    pub fn parse(doc: &Document, obj: &Object, resources: Option<&Dict>) -> Result<Self> {
        let resolved = doc.resolve(obj)?;
        let dict = resolved
            .as_dict()
            .cloned()
            .ok_or_else(|| Error::Corrupt("ombrage invalide".into()))?;
        let kind = doc
            .dict_get(&dict, "ShadingType")?
            .and_then(|o| o.as_i64())
            .unwrap_or(0);
        let cs_obj = dict
            .get(&Name::new("ColorSpace"))
            .cloned()
            .unwrap_or(Object::Name(Name::new("DeviceRGB")));
        let colorspace =
            ColorSpace::parse(doc, &cs_obj, resources).unwrap_or(ColorSpace::DeviceRgb);
        let function = match dict.get(&Name::new("Function")) {
            Some(f) => Function::parse(doc, f).ok().map(Rc::new),
            None => None,
        };
        let nums = |key: &str| -> Vec<f64> {
            doc.dict_get(&dict, key)
                .ok()
                .flatten()
                .and_then(|o| {
                    o.as_array().map(|a| {
                        a.iter()
                            .map(|v| doc.resolve(v).ok().and_then(|r| r.as_f64()).unwrap_or(0.0))
                            .collect()
                    })
                })
                .unwrap_or_default()
        };
        let background = {
            let b = nums("Background");
            if b.is_empty() {
                None
            } else {
                Some(colorspace.to_rgb(&b))
            }
        };
        let bb = nums("BBox");
        let bbox = if bb.len() == 4 {
            Some(Rect::new(bb[0], bb[1], bb[2], bb[3]))
        } else {
            None
        };
        let ext = doc
            .dict_get(&dict, "Extend")?
            .and_then(|o| o.as_array().map(<[Object]>::to_vec))
            .unwrap_or_default();
        let extend = (
            matches!(ext.first(), Some(Object::Bool(true))),
            matches!(ext.get(1), Some(Object::Bool(true))),
        );
        let m = nums("Matrix");
        let matrix = if m.len() == 6 {
            Matrix::new(m[0], m[1], m[2], m[3], m[4], m[5])
        } else {
            Matrix::IDENTITY
        };
        let anti_alias = matches!(
            doc.dict_get(&dict, "AntiAlias")?.as_deref(),
            Some(Object::Bool(true))
        );
        let mut shading = Self {
            kind,
            colorspace,
            background,
            bbox,
            function,
            coords: nums("Coords"),
            domain: nums("Domain"),
            extend,
            matrix,
            triangles: Vec::new(),
            anti_alias,
        };
        match kind {
            1..=3 => {}
            4..=7 => {
                let Object::Stream { .. } = &*resolved else {
                    return Err(Error::Corrupt("ombrage de maillage sans flux".into()));
                };
                let data = doc.stream_data(&resolved)?.data;
                let bpc = doc
                    .dict_get(&dict, "BitsPerCoordinate")?
                    .and_then(|o| o.as_i64())
                    .unwrap_or(16);
                let bpcomp = doc
                    .dict_get(&dict, "BitsPerComponent")?
                    .and_then(|o| o.as_i64())
                    .unwrap_or(8);
                let bpf = doc
                    .dict_get(&dict, "BitsPerFlag")?
                    .and_then(|o| o.as_i64())
                    .unwrap_or(8);
                let vpr = doc
                    .dict_get(&dict, "VerticesPerRow")?
                    .and_then(|o| o.as_i64())
                    .unwrap_or(2);
                let decode = nums("Decode");
                let mut reader = MeshReader {
                    data: &data,
                    bit: 0,
                    bpc: u32::try_from(bpc).unwrap_or(16).clamp(1, 32),
                    bpcomp: u32::try_from(bpcomp).unwrap_or(8).clamp(1, 16),
                    bpf: u32::try_from(bpf).unwrap_or(8).clamp(1, 8),
                    decode,
                    ncomp: if shading.function.is_some() {
                        1
                    } else {
                        shading.colorspace.components()
                    },
                };
                shading.triangles = match kind {
                    4 => read_type4(&mut reader, &shading),
                    5 => read_type5(
                        &mut reader,
                        &shading,
                        usize::try_from(vpr.max(2)).unwrap_or(2),
                    ),
                    _ => read_patches(&mut reader, &shading, kind == 7),
                };
            }
            other => return Err(Error::Unsupported(format!("ombrage de type {other}"))),
        }
        Ok(shading)
    }

    /// Couleur RVB pour une valeur `t` de la fonction (types 1 à 3 et maillages avec fonction).
    fn color_at(&self, inputs: &[f64]) -> [f32; 3] {
        match &self.function {
            Some(f) => {
                let out = f.eval(inputs);
                self.colorspace.to_rgb(&out)
            }
            None => self.colorspace.to_rgb(inputs),
        }
    }

    /// Arrêts de dégradé échantillonnés sur `[t0, t1]`.
    fn stops(&self) -> Vec<(f64, Color)> {
        let t0 = self.domain.first().copied().unwrap_or(0.0);
        let t1 = self.domain.get(1).copied().unwrap_or(1.0);
        (0..GRADIENT_SAMPLES)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let s = i as f64 / (GRADIENT_SAMPLES - 1) as f64;
                let t = t0 + s * (t1 - t0);
                let c = self.color_at(&[t]);
                (s, Color::rgb(c[0], c[1], c[2]))
            })
            .collect()
    }

    /// Source de peinture pour les types 1 à 3, `ctm` menant de l'espace de
    /// l'ombrage à l'espace device. `None` pour les maillages (voir `triangles`).
    #[must_use]
    pub fn paint(&self, ctm: &Matrix) -> Option<Paint> {
        match self.kind {
            2 if self.coords.len() >= 4 => {
                let mut g = Gradient::linear(
                    Point::new(self.coords[0], self.coords[1]),
                    Point::new(self.coords[2], self.coords[3]),
                    self.stops(),
                );
                g.extend_start = self.extend.0;
                g.extend_end = self.extend.1;
                g.transform = *ctm;
                Some(Paint::LinearGradient(g))
            }
            3 if self.coords.len() >= 6 => {
                let mut g = Gradient::radial(
                    Point::new(self.coords[0], self.coords[1]),
                    self.coords[2],
                    Point::new(self.coords[3], self.coords[4]),
                    self.coords[5],
                    self.stops(),
                );
                g.extend_start = self.extend.0;
                g.extend_end = self.extend.1;
                g.transform = *ctm;
                Some(Paint::RadialGradient(g))
            }
            1 => {
                // Fonction 2-D : device → espace de l'ombrage → espace de la fonction.
                let to_device = self.matrix.then(ctm);
                let inverse = to_device.invert()?;
                let domain = if self.domain.len() >= 4 {
                    self.domain.clone()
                } else {
                    vec![0.0, 1.0, 0.0, 1.0]
                };
                let function = self.function.clone()?;
                let cs = self.colorspace.clone();
                let background = self.background;
                Some(Paint::Callback(Box::new(move |x, y| {
                    let p = inverse.apply(Point::new(x, y));
                    if p.x < domain[0] || p.x > domain[1] || p.y < domain[2] || p.y > domain[3] {
                        return match background {
                            Some(b) => Color::rgb(b[0], b[1], b[2]),
                            None => Color::TRANSPARENT,
                        };
                    }
                    let out = function.eval(&[p.x, p.y]);
                    let c = cs.to_rgb(&out);
                    Color::rgb(c[0], c[1], c[2])
                })))
            }
            _ => None,
        }
    }

    /// Chemin couvrant toute l'étendue d'un dégradé (pour l'opérateur `sh`) :
    /// la boîte de découpe courante est le seul vrai délimiteur, on retourne
    /// donc un très grand rectangle en espace de l'ombrage, ou `/BBox`.
    #[must_use]
    pub fn extent_path(&self) -> Path {
        let mut p = Path::new();
        let r = self.bbox.unwrap_or_else(|| Rect::new(-1e6, -1e6, 1e6, 1e6));
        p.rect(&r);
        p
    }
}

/// Lecteur de données de maillage (§8.7.4.5.5 à §8.7.4.5.8).
struct MeshReader<'a> {
    data: &'a [u8],
    bit: usize,
    bpc: u32,
    bpcomp: u32,
    bpf: u32,
    decode: Vec<f64>,
    ncomp: usize,
}

impl MeshReader<'_> {
    fn bits(&mut self, n: u32) -> Option<u64> {
        let mut v: u64 = 0;
        for _ in 0..n {
            let byte = *self.data.get(self.bit / 8)?;
            let b = (byte >> (7 - (self.bit % 8))) & 1;
            v = (v << 1) | u64::from(b);
            self.bit += 1;
        }
        Some(v)
    }

    fn align(&mut self) {
        self.bit = self.bit.div_ceil(8) * 8;
    }

    fn at_end(&self) -> bool {
        self.bit / 8 >= self.data.len()
    }

    fn decoded(&self, raw: u64, bits: u32, index: usize) -> f64 {
        let max = if bits >= 64 {
            u64::MAX as f64
        } else {
            ((1u64 << bits) - 1) as f64
        };
        let dmin = self.decode.get(2 * index).copied().unwrap_or(0.0);
        let dmax = self.decode.get(2 * index + 1).copied().unwrap_or(1.0);
        dmin + (raw as f64) * (dmax - dmin) / max
    }

    fn flag(&mut self) -> Option<u8> {
        self.bits(self.bpf)
            .map(|v| u8::try_from(v & 0xFF).unwrap_or(0))
    }

    fn point(&mut self) -> Option<Point> {
        let x = self.bits(self.bpc)?;
        let y = self.bits(self.bpc)?;
        Some(Point::new(
            self.decoded(x, self.bpc, 0),
            self.decoded(y, self.bpc, 1),
        ))
    }

    fn color(&mut self, sh: &Shading) -> Option<[f32; 3]> {
        let mut comps = Vec::with_capacity(self.ncomp);
        for i in 0..self.ncomp {
            let raw = self.bits(self.bpcomp)?;
            comps.push(self.decoded(raw, self.bpcomp, 2 + i));
        }
        Some(sh.color_at(&comps))
    }
}

fn read_type4(r: &mut MeshReader<'_>, sh: &Shading) -> Vec<Triangle> {
    let mut tris = Vec::new();
    let mut prev: Vec<(Point, [f32; 3])> = Vec::new();
    while !r.at_end() && tris.len() < 1_000_000 {
        let Some(flag) = r.flag() else { break };
        let (Some(p), Some(c)) = (r.point(), r.color(sh)) else {
            break;
        };
        r.align();
        match flag {
            0 => {
                // Nouveau triangle : deux autres sommets avec drapeaux (ignorés).
                let (Some(_), Some(p2), Some(c2)) = (r.flag(), r.point(), r.color(sh)) else {
                    break;
                };
                r.align();
                let (Some(_), Some(p3), Some(c3)) = (r.flag(), r.point(), r.color(sh)) else {
                    break;
                };
                r.align();
                prev = vec![(p, c), (p2, c2), (p3, c3)];
            }
            1 | 2 if prev.len() == 3 => {
                if flag == 1 {
                    prev = vec![prev[1], prev[2], (p, c)];
                } else {
                    prev = vec![prev[0], prev[2], (p, c)];
                }
            }
            _ => break,
        }
        tris.push(Triangle {
            points: [prev[0].0, prev[1].0, prev[2].0],
            colors: [prev[0].1, prev[1].1, prev[2].1],
        });
    }
    tris
}

fn read_type5(r: &mut MeshReader<'_>, sh: &Shading, per_row: usize) -> Vec<Triangle> {
    let mut rows: Vec<Vec<(Point, [f32; 3])>> = Vec::new();
    while !r.at_end() && rows.len() < 65536 {
        let mut row = Vec::with_capacity(per_row);
        for _ in 0..per_row {
            let (Some(p), Some(c)) = (r.point(), r.color(sh)) else {
                break;
            };
            row.push((p, c));
        }
        if row.len() < per_row {
            break;
        }
        rows.push(row);
    }
    let mut tris = Vec::new();
    for pair in rows.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        for i in 0..per_row - 1 {
            tris.push(Triangle {
                points: [a[i].0, a[i + 1].0, b[i].0],
                colors: [a[i].1, a[i + 1].1, b[i].1],
            });
            tris.push(Triangle {
                points: [a[i + 1].0, b[i + 1].0, b[i].0],
                colors: [a[i + 1].1, b[i + 1].1, b[i].1],
            });
        }
    }
    tris
}

/// Patches de Coons (type 6, 12 points) et de tenseurs (type 7, 16 points),
/// subdivisés en une grille de triangles avec interpolation bilinéaire des
/// couleurs d'angle (§8.7.4.5.7).
fn read_patches(r: &mut MeshReader<'_>, sh: &Shading, tensor: bool) -> Vec<Triangle> {
    let mut tris = Vec::new();
    let mut prev_points: [Point; 12] = [Point::default(); 12];
    let mut prev_colors: [[f32; 3]; 4] = [[0.0; 3]; 4];
    let mut have_prev = false;
    while !r.at_end() && tris.len() < 2_000_000 {
        let Some(flag) = r.flag() else { break };
        let n_points = if flag == 0 { 12 } else { 8 };
        let n_colors = if flag == 0 { 4 } else { 2 };
        let mut pts = Vec::with_capacity(16);
        for _ in 0..n_points {
            let Some(p) = r.point() else { return tris };
            pts.push(p);
        }
        if tensor {
            for _ in 0..4 {
                let Some(p) = r.point() else { return tris };
                pts.push(p); // points internes : ignorés dans l'approximation de Coons
            }
        }
        let mut cols = Vec::with_capacity(4);
        for _ in 0..n_colors {
            let Some(c) = r.color(sh) else { return tris };
            cols.push(c);
        }
        r.align();
        // Reconstitue les 12 points de contrôle de bord et 4 couleurs (table 85).
        let (points, colors): ([Point; 12], [[f32; 3]; 4]) = if flag == 0 {
            let mut p = [Point::default(); 12];
            p.copy_from_slice(&pts[..12]);
            (p, [cols[0], cols[1], cols[2], cols[3]])
        } else {
            if !have_prev {
                break;
            }
            // Bord partagé avec le patch précédent selon le drapeau (§8.7.4.5.7, table 85).
            let pp = prev_points;
            let pc = prev_colors;
            let (edge, c0, c1): ([Point; 4], [f32; 3], [f32; 3]) = match flag {
                1 => ([pp[3], pp[4], pp[5], pp[6]], pc[1], pc[2]),
                2 => ([pp[6], pp[7], pp[8], pp[9]], pc[2], pc[3]),
                _ => ([pp[9], pp[10], pp[11], pp[0]], pc[3], pc[0]),
            };
            let mut p = [Point::default(); 12];
            p[..4].copy_from_slice(&edge);
            p[4..12].copy_from_slice(&pts[..8]);
            (p, [c0, c1, cols[0], cols[1]])
        };
        prev_points = points;
        prev_colors = colors;
        have_prev = true;
        subdivide_coons(&points, &colors, &mut tris);
    }
    tris
}

/// Point d'une cubique de Bézier.
fn bezier(p0: Point, p1: Point, p2: Point, p3: Point, t: f64) -> Point {
    let mt = 1.0 - t;
    let a = mt * mt * mt;
    let b = 3.0 * mt * mt * t;
    let c = 3.0 * mt * t * t;
    let d = t * t * t;
    Point::new(
        a * p0.x + b * p1.x + c * p2.x + d * p3.x,
        a * p0.y + b * p1.y + c * p2.y + d * p3.y,
    )
}

/// Surface de Coons définie par ses quatre bords cubiques (points 0..12 dans
/// l'ordre de la spec : p1 = points[0], bords D1 (0→3), C2 (3→6), D2 (6→9), C1 (9→0)).
fn subdivide_coons(p: &[Point; 12], colors: &[[f32; 3]; 4], out: &mut Vec<Triangle>) {
    const N: usize = 6;
    // Bords dans le paramétrage (u, v) de la spec : coin (0,0) = p[0], (0,1) = p[3], (1,1) = p[6], (1,0) = p[9].
    let c_left = |v: f64| bezier(p[0], p[1], p[2], p[3], v); // u = 0
    let c_top = |u: f64| bezier(p[3], p[4], p[5], p[6], u); // v = 1
    let c_right = |v: f64| bezier(p[9], p[8], p[7], p[6], v); // u = 1
    let c_bottom = |u: f64| bezier(p[0], p[11], p[10], p[9], u); // v = 0
    let surface = |u: f64, v: f64| -> Point {
        let l = c_left(v);
        let rr = c_right(v);
        let b = c_bottom(u);
        let t = c_top(u);
        let x = (1.0 - u) * l.x + u * rr.x + (1.0 - v) * b.x + v * t.x
            - ((1.0 - u) * (1.0 - v) * p[0].x
                + (1.0 - u) * v * p[3].x
                + u * v * p[6].x
                + u * (1.0 - v) * p[9].x);
        let y = (1.0 - u) * l.y + u * rr.y + (1.0 - v) * b.y + v * t.y
            - ((1.0 - u) * (1.0 - v) * p[0].y
                + (1.0 - u) * v * p[3].y
                + u * v * p[6].y
                + u * (1.0 - v) * p[9].y);
        Point::new(x, y)
    };
    let color = |u: f64, v: f64| -> [f32; 3] {
        // c0 en (0,0), c1 en (0,1), c2 en (1,1), c3 en (1,0).
        let mut c = [0.0f32; 3];
        for k in 0..3 {
            #[allow(clippy::cast_possible_truncation)]
            {
                c[k] = ((1.0 - u) * (1.0 - v) * f64::from(colors[0][k])
                    + (1.0 - u) * v * f64::from(colors[1][k])
                    + u * v * f64::from(colors[2][k])
                    + u * (1.0 - v) * f64::from(colors[3][k])) as f32;
            }
        }
        c
    };
    #[allow(clippy::cast_precision_loss)]
    for i in 0..N {
        for j in 0..N {
            let (u0, u1) = (i as f64 / N as f64, (i + 1) as f64 / N as f64);
            let (v0, v1) = (j as f64 / N as f64, (j + 1) as f64 / N as f64);
            let p00 = surface(u0, v0);
            let p10 = surface(u1, v0);
            let p01 = surface(u0, v1);
            let p11 = surface(u1, v1);
            out.push(Triangle {
                points: [p00, p10, p11],
                colors: [color(u0, v0), color(u1, v0), color(u1, v1)],
            });
            out.push(Triangle {
                points: [p00, p11, p01],
                colors: [color(u0, v0), color(u1, v1), color(u0, v1)],
            });
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use std::fmt::Write;

    use super::*;

    /// Encode des octets en hexadécimal (pour `/ASCIIHexDecode`).
    fn hex_string(data: &[u8]) -> String {
        data.iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }
    use acrux_document::Parser;

    fn doc() -> Document {
        Document::from_bytes(b"%PDF-1.7\n1 0 obj << /Type /Catalog >> endobj\n".to_vec()).unwrap()
    }

    fn shading(src: &str) -> Shading {
        let o = Parser::new(src.as_bytes()).parse_object().unwrap();
        Shading::parse(&doc(), &o, None).unwrap()
    }

    #[test]
    fn axial_gradient_paint() {
        let s = shading("<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> /Extend [true false] >>");
        let paint = s.paint(&Matrix::IDENTITY).unwrap();
        let Paint::LinearGradient(g) = &paint else {
            panic!("axial attendu")
        };
        assert_eq!(g.stops.len(), GRADIENT_SAMPLES);
        assert!(g.extend_start && !g.extend_end);
        let first = g.stops[0].1;
        let last = g.stops[GRADIENT_SAMPLES - 1].1;
        assert!(first.r > 0.99 && first.b < 0.01);
        assert!(last.b > 0.99 && last.r < 0.01);
    }

    #[test]
    fn radial_and_function_based() {
        let s = shading("<< /ShadingType 3 /ColorSpace /DeviceGray /Coords [50 50 0 50 50 40] /Function << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >> >>");
        assert!(matches!(
            s.paint(&Matrix::IDENTITY),
            Some(Paint::RadialGradient(_))
        ));
        let s = shading("<< /ShadingType 1 /ColorSpace /DeviceRGB /Domain [0 1 0 1] /Matrix [100 0 0 100 0 0] /Function << /FunctionType 2 /Domain [0 1] /C0 [0 0 0] /C1 [1 1 1] /N 1 >> >>");
        let paint = s.paint(&Matrix::IDENTITY).unwrap();
        let inside = paint.color_at(50.0, 50.0);
        assert!(inside.r > 0.49 && inside.r < 0.51, "{inside:?}");
        let outside = paint.color_at(150.0, 50.0);
        assert!(outside.a < 0.01, "hors domaine : transparent");
    }

    #[test]
    fn type4_free_form_triangles() {
        // 8 bits partout, Decode [0 100 0 100 0 1 0 1 0 1] : un triangle puis un raccord (flag 1).
        let mut data = Vec::new();
        for (flag, x, y, rgb) in [
            (0u8, 0u8, 0u8, [255u8, 0, 0]),
            (0, 100, 0, [0, 255, 0]),
            (0, 0, 100, [0, 0, 255]),
            (1, 100, 100, [255, 255, 255]),
        ] {
            data.push(flag);
            data.push(x);
            data.push(y);
            data.extend_from_slice(&rgb);
        }
        let hex = hex_string(&data);
        // parse_indirect attend "n g obj" : on encapsule.
        let src = format!("9 0 obj << /ShadingType 4 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 255 0 255 0 1 0 1 0 1] /Filter /ASCIIHexDecode /Length {} >>\nstream\n{hex}>\nendstream endobj", hex.len() + 1);
        let (_, o) = Parser::new(src.as_bytes())
            .parse_indirect(&|_| None)
            .unwrap();
        let s = Shading::parse(&doc(), &o, None).unwrap();
        assert_eq!(s.triangles.len(), 2);
        assert_eq!(s.triangles[0].points[1], Point::new(100.0, 0.0));
        assert_eq!(s.triangles[1].points[2], Point::new(100.0, 100.0));
        assert!((s.triangles[0].colors[0][0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn type6_coons_patch_is_subdivided() {
        // Un patch carré 0..100 avec des bords droits, 4 couleurs.
        let mut data = vec![0u8]; // flag
        let corners = [
            (0u8, 0u8),
            (0, 33),
            (0, 66),
            (0, 100),
            (33, 100),
            (66, 100),
            (100, 100),
            (100, 66),
            (100, 33),
            (100, 0),
            (66, 0),
            (33, 0),
        ];
        for (x, y) in corners {
            data.push(x);
            data.push(y);
        }
        for c in [[255u8, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0]] {
            data.extend_from_slice(&c);
        }
        let hex = hex_string(&data);
        let src = format!("9 0 obj << /ShadingType 6 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 255 0 255 0 1 0 1 0 1] /Filter /ASCIIHexDecode /Length {} >>\nstream\n{hex}>\nendstream endobj", hex.len() + 1);
        let (_, o) = Parser::new(src.as_bytes())
            .parse_indirect(&|_| None)
            .unwrap();
        let s = Shading::parse(&doc(), &o, None).unwrap();
        assert_eq!(s.triangles.len(), 72);
        // Tous les points restent dans le carré.
        for t in &s.triangles {
            for p in &t.points {
                assert!(
                    p.x >= -1e-6 && p.x <= 100.0 + 1e-6 && p.y >= -1e-6 && p.y <= 100.0 + 1e-6,
                    "{p:?}"
                );
            }
        }
        // Le coin (0,0) est rouge.
        let t0 = &s.triangles[0];
        assert!(t0.colors[0][0] > 0.99 && t0.colors[0][1] < 0.01);
    }

    #[test]
    fn unsupported_and_invalid() {
        let o = Parser::new(b"<< /ShadingType 9 >>").parse_object().unwrap();
        assert!(Shading::parse(&doc(), &o, None).is_err());
        let o = Parser::new(b"<< /ShadingType 4 /ColorSpace /DeviceRGB >>")
            .parse_object()
            .unwrap();
        assert!(
            Shading::parse(&doc(), &o, None).is_err(),
            "maillage sans flux"
        );
    }
}
