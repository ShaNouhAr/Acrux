//! Profils ICC : lecture d'un profil incorporé dans un PDF (`/ICCBased`,
//! ISO 32000-2 §8.6.5.5) et conversion des couleurs de l'appareil vers sRGB.
//!
//! Écrit de zéro d'après ICC.1:2022 (profils v4, numérotation des clauses
//! citée ci-dessous) et ICC.1:2001-04 (profils v2). Couvert :
//!
//! - en-tête (§7.2) et table des tags (§7.3), lectures toutes bornées : un
//!   profil tronqué ou aléatoire donne une erreur typée, jamais une panique ;
//! - profils **matriciels/TRC** RVB (§8.3.4 : `rXYZ gXYZ bXYZ rTRC gTRC
//!   bTRC`) avec les types `XYZType`, `curveType` (identité, gamma, table) et
//!   `parametricCurveType` (fonctions 0 à 4) ;
//! - profils **gris** (`kTRC`, §8.3.3) ;
//! - profils **LUT** `A2B0` / `A2B1` / `A2B2` (§9.2.1 à 9.2.3) de types
//!   `lut8Type`, `lut16Type` (matrice, courbes d'entrée, CLUT n-D, courbes de
//!   sortie) et `lutAToBType` (courbes A → CLUT → courbes M → matrice →
//!   courbes B), avec PCS Lab (codages 16 bits hérité v2, 8 bits et v4,
//!   §6.3.4.2 et annexe A) ou PCS XYZ (u1Fixed15, §6.3.4.1) ;
//! - description (`desc` : `textDescriptionType` v2, `multiLocalizedUnicodeType`
//!   v4, `textType`) ;
//! - cache quantifié [`IccCache`] pour convertir des images entières.
//!
//! Chaîne de conversion : appareil → PCS (illuminant D50) → XYZ D65
//! (adaptation de Bradford, annexe E) → sRGB linéaire (IEC 61966-2-1) →
//! sRGB non linéaire. L'intention retenue est celle d'Acrobat : perceptuelle
//! (`A2B0`) quand le profil en a une, sinon colorimétrique relative (profils
//! matriciels/TRC, `A2B1`).
//!
//! Non couvert (voir le README du crate) : profils de couleurs nommées
//! (`nmcl`), `multiProcessElementsType`, tables `B2A`, intention absolue,
//! compensation du point noir.

use std::sync::OnceLock;

use acrux_core::{Error, Result};

use crate::color::to_u8;

/// Nombre maximal de canaux d'un espace ICC (`FCLR`, §7.2.6).
const MAX_CHANNELS: usize = 15;

/// Taille de l'en-tête (§7.2) suivie du compte de tags (§7.3).
const HEADER_LEN: usize = 128;

/// Illuminant du PCS : D50 (§7.2.16).
const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

/// Chromaticité de l'illuminant D65 (IEC 61966-2-1 §5.2).
const D65_XY: (f64, f64) = (0.3127, 0.3290);

/// Chromaticités des primaires sRGB (IEC 61966-2-1 §5.2).
const SRGB_RED_XY: (f64, f64) = (0.64, 0.33);
const SRGB_GREEN_XY: (f64, f64) = (0.30, 0.60);
const SRGB_BLUE_XY: (f64, f64) = (0.15, 0.06);

/// Matrice de Bradford (ICC.1:2022 annexe E, équation E.3).
const BRADFORD: Mat3 = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

/// Matrice 3 × 3, `m[ligne][colonne]`.
type Mat3 = [[f64; 3]; 3];

const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

// ---------------------------------------------------------------------------
// Arithmétique élémentaire
// ---------------------------------------------------------------------------

/// Ramène dans `[0, 1]` ; un NaN devient 0.
#[inline]
fn unit(v: f64) -> f64 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

/// Composante `[0, 1]` en `f32` (la troncature est sans effet : valeur bornée).
#[inline]
#[allow(clippy::cast_possible_truncation)]
fn to_f32(v: f64) -> f32 {
    unit(v) as f32
}

/// Nombre entier (taille de table, index) en flottant.
#[inline]
#[allow(clippy::cast_precision_loss)] // tailles de tables, très inférieures à 2^53
fn count_to_f64(n: usize) -> f64 {
    n as f64
}

/// Partie entière d'une position de grille, bornée à `max`.
#[inline]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // pos ≥ 0 et bornée
fn floor_index(pos: f64, max: usize) -> usize {
    if pos.is_nan() || pos <= 0.0 {
        0
    } else {
        (pos.floor() as usize).min(max)
    }
}

fn mat_mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    out
}

fn mat_vec(m: &Mat3, v: &[f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

fn mat_from_columns(c0: [f64; 3], c1: [f64; 3], c2: [f64; 3]) -> Mat3 {
    [
        [c0[0], c1[0], c2[0]],
        [c0[1], c1[1], c2[1]],
        [c0[2], c1[2], c2[2]],
    ]
}

fn mat_diag(d: [f64; 3]) -> Mat3 {
    [[d[0], 0.0, 0.0], [0.0, d[1], 0.0], [0.0, 0.0, d[2]]]
}

/// Inverse par la comatrice ; `None` si la matrice est singulière ou non finie.
fn mat_inv(m: &Mat3) -> Option<Mat3> {
    let cof = [
        [
            m[1][1] * m[2][2] - m[1][2] * m[2][1],
            -(m[1][0] * m[2][2] - m[1][2] * m[2][0]),
            m[1][0] * m[2][1] - m[1][1] * m[2][0],
        ],
        [
            -(m[0][1] * m[2][2] - m[0][2] * m[2][1]),
            m[0][0] * m[2][2] - m[0][2] * m[2][0],
            -(m[0][0] * m[2][1] - m[0][1] * m[2][0]),
        ],
        [
            m[0][1] * m[1][2] - m[0][2] * m[1][1],
            -(m[0][0] * m[1][2] - m[0][2] * m[1][0]),
            m[0][0] * m[1][1] - m[0][1] * m[1][0],
        ],
    ];
    let det = m[0][0] * cof[0][0] + m[0][1] * cof[0][1] + m[0][2] * cof[0][2];
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = cof[j][i] / det;
        }
    }
    Some(out)
}

/// XYZ (Y = 1) d'une chromaticité `(x, y)`.
fn xy_to_xyz((x, y): (f64, f64)) -> [f64; 3] {
    if y.abs() < 1e-9 {
        return [0.0, 1.0, 0.0];
    }
    [x / y, 1.0, (1.0 - x - y) / y]
}

/// Matrice RVB linéaire → XYZ d'un espace défini par ses chromaticités et son
/// blanc (méthode classique : colonnes des primaires mises à l'échelle pour
/// que (1, 1, 1) donne le blanc).
fn rgb_matrix_from_chromaticities(
    red: (f64, f64),
    green: (f64, f64),
    blue: (f64, f64),
    white: &[f64; 3],
) -> Option<Mat3> {
    let p = mat_from_columns(xy_to_xyz(red), xy_to_xyz(green), xy_to_xyz(blue));
    let s = mat_vec(&mat_inv(&p)?, white);
    Some(mat_mul(&p, &mat_diag(s)))
}

/// Matrice d'adaptation chromatique de Bradford du blanc `src` vers le blanc
/// `dst` (ICC.1:2022 annexe E) : `B⁻¹ · diag(cône(dst) / cône(src)) · B`.
fn bradford_adaptation(src: &[f64; 3], dst: &[f64; 3]) -> Option<Mat3> {
    let cone_src = mat_vec(&BRADFORD, src);
    let cone_dst = mat_vec(&BRADFORD, dst);
    if cone_src.iter().any(|v| !v.is_finite() || v.abs() < 1e-9) {
        return None;
    }
    let scale = mat_diag([
        cone_dst[0] / cone_src[0],
        cone_dst[1] / cone_src[1],
        cone_dst[2] / cone_src[2],
    ]);
    Some(mat_mul(&mat_mul(&mat_inv(&BRADFORD)?, &scale), &BRADFORD))
}

/// Matrices sRGB dérivées une fois des chromaticités de IEC 61966-2-1.
struct Colorimetry {
    /// XYZ (D50, PCS) → sRGB linéaire : inverse sRGB ∘ Bradford D50 → D65.
    xyz_d50_to_linear_srgb: Mat3,
    /// sRGB linéaire → XYZ D50 : colorants sRGB adaptés en D50, tels qu'ils
    /// figurent dans un profil sRGB (`rXYZ gXYZ bXYZ`).
    srgb_colorants_d50: Mat3,
}

fn colorimetry() -> &'static Colorimetry {
    static CELL: OnceLock<Colorimetry> = OnceLock::new();
    CELL.get_or_init(|| {
        let d65 = xy_to_xyz(D65_XY);
        let rgb_to_xyz_d65 =
            rgb_matrix_from_chromaticities(SRGB_RED_XY, SRGB_GREEN_XY, SRGB_BLUE_XY, &d65)
                .unwrap_or(IDENTITY);
        let xyz_d65_to_rgb = mat_inv(&rgb_to_xyz_d65).unwrap_or(IDENTITY);
        let d50_to_d65 = bradford_adaptation(&D50, &d65).unwrap_or(IDENTITY);
        let d65_to_d50 = bradford_adaptation(&d65, &D50).unwrap_or(IDENTITY);
        Colorimetry {
            xyz_d50_to_linear_srgb: mat_mul(&xyz_d65_to_rgb, &d50_to_d65),
            srgb_colorants_d50: mat_mul(&d65_to_d50, &rgb_to_xyz_d65),
        }
    })
}

/// Fonction de transfert sRGB (IEC 61966-2-1 §5.3) : linéaire → codé.
#[must_use]
pub fn linear_to_srgb(v: f64) -> f64 {
    let v = unit(v);
    if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Fonction de transfert sRGB inverse : codé → linéaire.
#[must_use]
pub fn srgb_to_linear(v: f64) -> f64 {
    let v = unit(v);
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// XYZ relatif à l'illuminant D50 (PCS) → sRGB non linéaire, borné à `[0, 1]`.
#[must_use]
pub fn xyz_d50_to_srgb(xyz: [f64; 3]) -> [f32; 3] {
    let linear = mat_vec(&colorimetry().xyz_d50_to_linear_srgb, &xyz);
    linear.map(|v| to_f32(linear_to_srgb(v)))
}

/// CIE L*a*b* (blanc D50) → XYZ D50 (CIE 15, formules inverses ; c'est aussi
/// la définition du PCS Lab, ICC.1:2022 §6.3.4.2).
#[must_use]
pub fn lab_to_xyz_d50(lab: [f64; 3]) -> [f64; 3] {
    const DELTA: f64 = 6.0 / 29.0;
    let finv = |t: f64| {
        if t > DELTA {
            t * t * t
        } else {
            3.0 * DELTA * DELTA * (t - 4.0 / 29.0)
        }
    };
    let finite = |v: f64, lo: f64, hi: f64| if v.is_nan() { 0.0 } else { v.clamp(lo, hi) };
    let l = finite(lab[0], 0.0, 100.0);
    let a = finite(lab[1], -128.0, 127.0);
    let b = finite(lab[2], -128.0, 127.0);
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    [D50[0] * finv(fx), D50[1] * finv(fy), D50[2] * finv(fz)]
}

// ---------------------------------------------------------------------------
// Lecture bornée
// ---------------------------------------------------------------------------

/// Vue sur des octets dont toutes les lectures sont bornées (`None` hors limites).
#[derive(Clone, Copy)]
struct Bytes<'a>(&'a [u8]);

impl<'a> Bytes<'a> {
    fn len(self) -> usize {
        self.0.len()
    }

    fn slice(self, at: usize, len: usize) -> Option<&'a [u8]> {
        self.0.get(at..at.checked_add(len)?)
    }

    fn u8(self, at: usize) -> Option<u8> {
        self.0.get(at).copied()
    }

    fn u16(self, at: usize) -> Option<u16> {
        let b = <[u8; 2]>::try_from(self.slice(at, 2)?).ok()?;
        Some(u16::from_be_bytes(b))
    }

    fn u32(self, at: usize) -> Option<u32> {
        let b = <[u8; 4]>::try_from(self.slice(at, 4)?).ok()?;
        Some(u32::from_be_bytes(b))
    }

    /// Signature de quatre octets.
    fn sig(self, at: usize) -> Option<[u8; 4]> {
        <[u8; 4]>::try_from(self.slice(at, 4)?).ok()
    }

    /// `s15Fixed16Number` (§4.6).
    fn s15f16(self, at: usize) -> Option<f64> {
        let b = <[u8; 4]>::try_from(self.slice(at, 4)?).ok()?;
        Some(f64::from(i32::from_be_bytes(b)) / 65536.0)
    }

    /// `XYZNumber` (§4.14) : trois `s15Fixed16Number`.
    fn xyz_number(self, at: usize) -> Option<[f64; 3]> {
        Some([
            self.s15f16(at)?,
            self.s15f16(at.checked_add(4)?)?,
            self.s15f16(at.checked_add(8)?)?,
        ])
    }

    /// `count` échantillons de 1 ou 2 octets, normalisés dans `[0, 1]`.
    fn samples(self, at: usize, count: usize, wide: bool) -> Option<Vec<f32>> {
        let width = if wide { 2 } else { 1 };
        let raw = self.slice(at, count.checked_mul(width)?)?;
        Some(if wide {
            raw.chunks_exact(2)
                .map(|c| f32::from(u16::from_be_bytes([c[0], c[1]])) / 65535.0)
                .collect()
        } else {
            raw.iter().map(|&v| f32::from(v) / 255.0).collect()
        })
    }
}

fn corrupt(message: &str) -> Error {
    Error::Corrupt(format!("profil ICC : {message}"))
}

fn sig_text(sig: [u8; 4]) -> String {
    String::from_utf8_lossy(&sig).trim().to_string()
}

// ---------------------------------------------------------------------------
// Courbes
// ---------------------------------------------------------------------------

/// Courbe monodimensionnelle `[0, 1] → [0, 1]` : `curveType` (§10.5) ou
/// `parametricCurveType` (§10.18), ou table d'un `lut8Type` / `lut16Type`.
#[derive(Debug, Clone)]
enum Curve {
    Identity,
    /// `y = x^γ`.
    Gamma(f64),
    /// Table uniformément échantillonnée, interpolée linéairement.
    Table(Vec<f64>),
    /// Fonction paramétrique ; `p = [g, a, b, c, d, e, f]`.
    Parametric {
        kind: u16,
        p: [f64; 7],
    },
}

impl Curve {
    fn eval(&self, x: f64) -> f64 {
        let x = unit(x);
        let y = match self {
            Curve::Identity => x,
            Curve::Gamma(g) => x.powf(*g),
            Curve::Table(t) => table_lookup(t, x),
            Curve::Parametric { kind, p } => parametric(*kind, p, x),
        };
        unit(y)
    }

    /// Lit une courbe `curv` ou `para` à `at` ; renvoie la courbe et sa taille
    /// en octets (non alignée).
    fn parse(r: Bytes<'_>, at: usize) -> Option<(Curve, usize)> {
        match &r.sig(at)? {
            b"curv" => {
                let count = usize::try_from(r.u32(at.checked_add(8)?)?).ok()?;
                let data_at = at.checked_add(12)?;
                let curve = match count {
                    0 => Curve::Identity,
                    // u8Fixed8Number (§4.7).
                    1 => Curve::Gamma(f64::from(r.u16(data_at)?) / 256.0),
                    _ => {
                        let table = r.samples(data_at, count, true)?;
                        Curve::Table(table.into_iter().map(f64::from).collect())
                    }
                };
                Some((curve, 12 + count.checked_mul(2)?))
            }
            b"para" => {
                let kind = r.u16(at.checked_add(8)?)?;
                let n = match kind {
                    0 => 1,
                    1 => 3,
                    2 => 4,
                    3 => 5,
                    4 => 7,
                    _ => return None,
                };
                let mut p = [0.0; 7];
                for (i, slot) in p.iter_mut().enumerate().take(n) {
                    *slot = r.s15f16(at.checked_add(12 + 4 * i)?)?;
                }
                Some((Curve::Parametric { kind, p }, 12 + 4 * n))
            }
            _ => None,
        }
    }
}

/// Interpolation linéaire dans une table uniforme sur `[0, 1]`.
fn table_lookup(table: &[f64], x: f64) -> f64 {
    match table.len() {
        0 => x,
        1 => table.first().copied().unwrap_or(x),
        len => {
            let pos = x * count_to_f64(len - 1);
            let i0 = floor_index(pos, len - 2);
            let frac = pos - count_to_f64(i0);
            let lo = table.get(i0).copied().unwrap_or(0.0);
            let hi = table.get(i0 + 1).copied().unwrap_or(lo);
            lo + (hi - lo) * frac
        }
    }
}

/// Les cinq fonctions de `parametricCurveType` (§10.18, tableau 70).
fn parametric(kind: u16, params: &[f64; 7], x: f64) -> f64 {
    let (gamma, pa, pb, pc, pd, pe, pf) = (
        params[0], params[1], params[2], params[3], params[4], params[5], params[6],
    );
    let power = |v: f64| v.powf(gamma);
    match kind {
        0 => power(x),
        1 => {
            if pa * x + pb >= 0.0 {
                power(pa * x + pb)
            } else {
                0.0
            }
        }
        2 => {
            if pa * x + pb >= 0.0 {
                power(pa * x + pb) + pc
            } else {
                pc
            }
        }
        3 => {
            if x >= pd {
                power(pa * x + pb)
            } else {
                pc * x
            }
        }
        _ => {
            if x >= pd {
                power(pa * x + pb) + pe
            } else {
                pc * x + pf
            }
        }
    }
}

fn apply_curves(curves: &[Curve], values: &mut [f64]) {
    for (curve, v) in curves.iter().zip(values.iter_mut()) {
        *v = curve.eval(*v);
    }
}

// ---------------------------------------------------------------------------
// Grille n-D (CLUT et cache)
// ---------------------------------------------------------------------------

/// Table de correspondance n-D uniformément échantillonnée sur `[0, 1]ⁿ`.
/// Ordre des données (§10.10, 10.11, 10.12) : le premier canal d'entrée varie
/// le moins vite ; pour chaque nœud, les canaux de sortie se suivent.
#[derive(Debug, Clone)]
struct Grid {
    /// Nombre de nœuds par dimension (≥ 2).
    dims: Vec<usize>,
    out_ch: usize,
    /// Pas de chaque dimension, en éléments de `data`.
    strides: Vec<usize>,
    data: Vec<f32>,
}

impl Grid {
    /// Nombre d'éléments d'une grille, ou `None` en cas de débordement.
    fn element_count(dims: &[usize], out_ch: usize) -> Option<usize> {
        dims.iter().try_fold(out_ch, |acc, &g| acc.checked_mul(g))
    }

    fn new(dims: Vec<usize>, out_ch: usize, data: Vec<f32>) -> Option<Self> {
        if dims.is_empty()
            || dims.len() > MAX_CHANNELS
            || out_ch == 0
            || out_ch > MAX_CHANNELS
            || dims.iter().any(|&g| g < 2)
        {
            return None;
        }
        let mut strides = vec![0; dims.len()];
        let mut stride = out_ch;
        for (slot, &g) in strides.iter_mut().zip(dims.iter()).rev() {
            *slot = stride;
            stride = stride.checked_mul(g)?;
        }
        if data.len() != stride {
            return None;
        }
        Some(Self {
            dims,
            out_ch,
            strides,
            data,
        })
    }

    #[inline]
    fn at(&self, i: usize) -> f64 {
        self.data.get(i).copied().map_or(0.0, f64::from)
    }

    /// Position dans la grille : index de base et fraction par dimension.
    fn locate(&self, input: &[f64]) -> (usize, [f64; MAX_CHANNELS]) {
        let mut base = 0;
        let mut frac = [0.0; MAX_CHANNELS];
        for (i, (&g, &stride)) in self.dims.iter().zip(&self.strides).enumerate() {
            let x = unit(input.get(i).copied().unwrap_or(0.0));
            let pos = x * count_to_f64(g - 1);
            let i0 = floor_index(pos, g - 2);
            base += i0 * stride;
            frac[i] = pos - count_to_f64(i0);
        }
        (base, frac)
    }

    /// Interpolation : tétraédrique sur les trois dernières dimensions,
    /// multilinéaire sur les précédentes (et sur tout si n < 3).
    fn eval(&self, input: &[f64], out: &mut [f64]) {
        let n = self.dims.len();
        for o in out.iter_mut() {
            *o = 0.0;
        }
        let (base, frac) = self.locate(input);
        if n < 3 {
            self.multilinear_from(base, &frac[..n], &self.strides[..n], 1.0, out);
            return;
        }
        let lead = n - 3;
        let tail_frac = [frac[lead], frac[lead + 1], frac[lead + 2]];
        let tail_strides = [
            self.strides[lead],
            self.strides[lead + 1],
            self.strides[lead + 2],
        ];
        for corner in 0..(1usize << lead) {
            let mut weight = 1.0;
            let mut offset = base;
            for (i, (&f, &stride)) in frac.iter().zip(&self.strides).take(lead).enumerate() {
                if (corner >> i) & 1 == 1 {
                    weight *= f;
                    offset += stride;
                } else {
                    weight *= 1.0 - f;
                }
            }
            if weight > 0.0 {
                self.tetrahedral(offset, tail_frac, tail_strides, weight, out);
            }
        }
    }

    /// Interpolation multilinéaire sur toutes les dimensions (référence des tests).
    #[cfg(test)]
    fn eval_multilinear(&self, input: &[f64], out: &mut [f64]) {
        for o in out.iter_mut() {
            *o = 0.0;
        }
        let n = self.dims.len();
        let (base, frac) = self.locate(input);
        self.multilinear_from(base, &frac[..n], &self.strides[..n], 1.0, out);
    }

    fn multilinear_from(
        &self,
        base: usize,
        frac: &[f64],
        strides: &[usize],
        weight: f64,
        out: &mut [f64],
    ) {
        let n = frac.len().min(strides.len());
        for corner in 0..(1usize << n) {
            let mut w = weight;
            let mut offset = base;
            for (i, (&f, &stride)) in frac.iter().zip(strides).enumerate() {
                if (corner >> i) & 1 == 1 {
                    w *= f;
                    offset += stride;
                } else {
                    w *= 1.0 - f;
                }
            }
            if w > 0.0 {
                for (ch, o) in out.iter_mut().enumerate().take(self.out_ch) {
                    *o += w * self.at(offset + ch);
                }
            }
        }
    }

    /// Interpolation tétraédrique 3-D (le cube est découpé en six tétraèdres
    /// selon l'ordre des fractions), pondérée par `weight`, cumulée dans `out`.
    fn tetrahedral(
        &self,
        base: usize,
        frac: [f64; 3],
        strides: [usize; 3],
        weight: f64,
        out: &mut [f64],
    ) {
        // Dimensions triées par fraction décroissante (réseau de tri à 3 entrées).
        let mut order = [0usize, 1, 2];
        if frac[order[0]] < frac[order[1]] {
            order.swap(0, 1);
        }
        if frac[order[1]] < frac[order[2]] {
            order.swap(1, 2);
        }
        if frac[order[0]] < frac[order[1]] {
            order.swap(0, 1);
        }
        let o0 = base;
        let o1 = o0 + strides[order[0]];
        let o2 = o1 + strides[order[1]];
        let o3 = o2 + strides[order[2]];
        let (f1, f2, f3) = (frac[order[0]], frac[order[1]], frac[order[2]]);
        for (ch, o) in out.iter_mut().enumerate().take(self.out_ch) {
            let v0 = self.at(o0 + ch);
            let v1 = self.at(o1 + ch);
            let v2 = self.at(o2 + ch);
            let v3 = self.at(o3 + ch);
            *o += weight * (v0 + f1 * (v1 - v0) + f2 * (v2 - v1) + f3 * (v3 - v2));
        }
    }
}

// ---------------------------------------------------------------------------
// Pipeline LUT
// ---------------------------------------------------------------------------

/// Codage des trois valeurs de sortie (normalisées `[0, 1]`) d'une LUT vers le PCS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PcsEncoding {
    /// Lab 16 bits hérité de v2 (`lut16Type`, annexe A) : L* 0…100 ↔ 0…0xFF00,
    /// a*/b* −128…127 ↔ 0…0xFF00.
    LabLegacy16,
    /// Lab 8 bits ou 16 bits v4 (§6.3.4.2) : L* 0…100 ↔ 0…max, a*/b* −128…127 ↔ 0…max.
    Lab16,
    /// XYZ 16 bits `u1Fixed15Number` (§6.3.4.1) : 1,0 ↔ 0x8000.
    Xyz16,
}

impl PcsEncoding {
    fn to_xyz_d50(self, v: [f64; 3]) -> [f64; 3] {
        match self {
            PcsEncoding::Xyz16 => v.map(|c| c * (65535.0 / 32768.0)),
            PcsEncoding::Lab16 => {
                lab_to_xyz_d50([v[0] * 100.0, v[1] * 255.0 - 128.0, v[2] * 255.0 - 128.0])
            }
            PcsEncoding::LabLegacy16 => lab_to_xyz_d50([
                v[0] * (65535.0 / 652.80),
                v[1] * (65535.0 / 256.0) - 128.0,
                v[2] * (65535.0 / 256.0) - 128.0,
            ]),
        }
    }
}

/// Pipeline d'une table `A2Bx` (§10.10 à §10.12), dans l'ordre d'application :
/// matrice héritée (seulement si l'entrée est XYZ) → courbes A (ou tables
/// d'entrée) → CLUT → courbes M → matrice → courbes B (ou tables de sortie).
#[derive(Debug, Clone)]
struct LutPipeline {
    in_ch: usize,
    pre_matrix: Option<Mat3>,
    a_curves: Vec<Curve>,
    clut: Option<Grid>,
    m_curves: Vec<Curve>,
    matrix: Option<(Mat3, [f64; 3])>,
    b_curves: Vec<Curve>,
    encoding: PcsEncoding,
}

impl LutPipeline {
    fn to_xyz_d50(&self, input: &[f64]) -> [f64; 3] {
        let mut v = [0.0; MAX_CHANNELS];
        for (dst, src) in v.iter_mut().zip(input) {
            *dst = unit(*src);
        }
        if let Some(m) = &self.pre_matrix {
            let t = mat_vec(m, &[v[0], v[1], v[2]]);
            v[..3].copy_from_slice(&t);
        }
        let n = self.in_ch.min(MAX_CHANNELS);
        apply_curves(&self.a_curves, &mut v[..n]);
        let mut out = [v[0], v[1], v[2]];
        if let Some(grid) = &self.clut {
            grid.eval(&v[..n], &mut out);
        }
        apply_curves(&self.m_curves, &mut out);
        if let Some((m, offset)) = &self.matrix {
            let t = mat_vec(m, &out);
            for (i, o) in out.iter_mut().enumerate() {
                *o = unit(t[i] + offset[i]);
            }
        }
        apply_curves(&self.b_curves, &mut out);
        self.encoding.to_xyz_d50(out.map(unit))
    }
}

/// Lit `count` courbes consécutives (`curv` / `para`) alignées sur 4 octets
/// (§10.12.2 à 10.12.5).
fn parse_curve_sequence(tag: Bytes<'_>, at: usize, count: usize) -> Option<Vec<Curve>> {
    let mut curves = Vec::with_capacity(count);
    let mut at = at;
    for _ in 0..count {
        let (curve, size) = Curve::parse(tag, at)?;
        curves.push(curve);
        at = at.checked_add(size)?.checked_add(3)? & !3;
    }
    Some(curves)
}

/// `lut8Type` (§10.11) et `lut16Type` (§10.10).
fn parse_lut8_16(
    tag: Bytes<'_>,
    wide: bool,
    in_ch: usize,
    pcs: [u8; 4],
    data_space: [u8; 4],
) -> Result<LutPipeline> {
    let err = || corrupt("table lut8/lut16 tronquée");
    let declared_in = usize::from(tag.u8(8).ok_or_else(err)?);
    let out_ch = usize::from(tag.u8(9).ok_or_else(err)?);
    let grid = usize::from(tag.u8(10).ok_or_else(err)?);
    if declared_in != in_ch {
        return Err(corrupt("nombre de canaux d'entrée de la LUT incohérent"));
    }
    if out_ch != 3 {
        return Err(Error::Unsupported(format!(
            "LUT ICC à {out_ch} canaux de sortie"
        )));
    }
    if grid < 2 {
        return Err(corrupt("CLUT sans nœuds"));
    }
    let mut matrix = [[0.0; 3]; 3];
    for (i, row) in matrix.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = tag.s15f16(12 + 4 * (3 * i + j)).ok_or_else(err)?;
        }
    }
    let (in_entries, out_entries, mut at) = if wide {
        let ie = usize::from(tag.u16(48).ok_or_else(err)?);
        let oe = usize::from(tag.u16(50).ok_or_else(err)?);
        (ie, oe, 52)
    } else {
        (256, 256, 48)
    };
    if !(2..=4096).contains(&in_entries) || !(2..=4096).contains(&out_entries) {
        return Err(corrupt("nombre d'entrées de table invalide"));
    }
    let width = if wide { 2 } else { 1 };
    let a_curves = read_lut_tables(tag, &mut at, in_ch, in_entries, wide)?;
    let dims = vec![grid; in_ch];
    let count = Grid::element_count(&dims, out_ch).ok_or_else(|| corrupt("CLUT trop grande"))?;
    let data = tag.samples(at, count, wide).ok_or_else(err)?;
    at += count * width;
    let clut = Grid::new(dims, out_ch, data).ok_or_else(|| corrupt("CLUT invalide"))?;
    let b_curves = read_lut_tables(tag, &mut at, out_ch, out_entries, wide)?;
    let encoding = match (&pcs, wide) {
        (b"Lab ", true) => PcsEncoding::LabLegacy16,
        (b"Lab ", false) => PcsEncoding::Lab16,
        _ => PcsEncoding::Xyz16,
    };
    Ok(LutPipeline {
        in_ch,
        // §10.10 : la matrice n'est appliquée que si l'espace d'entrée est XYZ.
        pre_matrix: (&data_space == b"XYZ ").then_some(matrix),
        a_curves,
        clut: Some(clut),
        m_curves: Vec::new(),
        matrix: None,
        b_curves,
        encoding,
    })
}

/// `count` tables d'entrée ou de sortie d'un `lut8Type` / `lut16Type`, lues à
/// `*at` (avancé d'autant).
fn read_lut_tables(
    tag: Bytes<'_>,
    at: &mut usize,
    count: usize,
    entries: usize,
    wide: bool,
) -> Result<Vec<Curve>> {
    let mut tables = Vec::with_capacity(count);
    for _ in 0..count {
        let samples = tag
            .samples(*at, entries, wide)
            .ok_or_else(|| corrupt("table lut8/lut16 tronquée"))?;
        *at += entries * if wide { 2 } else { 1 };
        tables.push(Curve::Table(samples.into_iter().map(f64::from).collect()));
    }
    Ok(tables)
}

/// CLUT d'un `lutAToBType` (§10.12.3).
fn parse_mab_clut(tag: Bytes<'_>, at: usize, in_ch: usize, out_ch: usize) -> Option<Grid> {
    let mut dims = Vec::with_capacity(in_ch);
    for i in 0..in_ch {
        dims.push(usize::from(tag.u8(at.checked_add(i)?)?));
    }
    let precision = tag.u8(at.checked_add(16)?)?;
    let wide = match precision {
        1 => false,
        2 => true,
        _ => return None,
    };
    let count = Grid::element_count(&dims, out_ch)?;
    let data = tag.samples(at.checked_add(20)?, count, wide)?;
    Grid::new(dims, out_ch, data)
}

/// `lutAToBType` (§10.12).
fn parse_mab(tag: Bytes<'_>, in_ch: usize, pcs: [u8; 4]) -> Result<LutPipeline> {
    let err = || corrupt("table lutAToB tronquée");
    let declared_in = usize::from(tag.u8(8).ok_or_else(err)?);
    let out_ch = usize::from(tag.u8(9).ok_or_else(err)?);
    if declared_in != in_ch {
        return Err(corrupt("nombre de canaux d'entrée de la LUT incohérent"));
    }
    if out_ch != 3 {
        return Err(Error::Unsupported(format!(
            "LUT ICC à {out_ch} canaux de sortie"
        )));
    }
    let offset = |at: usize| -> Result<usize> {
        usize::try_from(tag.u32(at).ok_or_else(err)?).map_err(|_| err())
    };
    let (off_b, off_matrix, off_m, off_clut, off_a) = (
        offset(12)?,
        offset(16)?,
        offset(20)?,
        offset(24)?,
        offset(28)?,
    );
    if off_b == 0 {
        return Err(corrupt("lutAToB sans courbes B"));
    }
    let b_curves = parse_curve_sequence(tag, off_b, out_ch).ok_or_else(err)?;
    let clut = if off_clut == 0 {
        None
    } else {
        Some(parse_mab_clut(tag, off_clut, in_ch, out_ch).ok_or_else(|| corrupt("CLUT invalide"))?)
    };
    let a_curves = if off_a == 0 || clut.is_none() {
        Vec::new()
    } else {
        parse_curve_sequence(tag, off_a, in_ch).ok_or_else(err)?
    };
    if clut.is_none() && in_ch != out_ch {
        return Err(corrupt(
            "lutAToB sans CLUT avec des nombres de canaux différents",
        ));
    }
    let m_curves = if off_m == 0 {
        Vec::new()
    } else {
        parse_curve_sequence(tag, off_m, out_ch).ok_or_else(err)?
    };
    let matrix = if off_matrix == 0 {
        None
    } else {
        let mut m = [[0.0; 3]; 3];
        for (i, row) in m.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = tag
                    .s15f16(off_matrix.checked_add(4 * (3 * i + j)).ok_or_else(err)?)
                    .ok_or_else(err)?;
            }
        }
        let mut offset = [0.0; 3];
        for (i, o) in offset.iter_mut().enumerate() {
            *o = tag
                .s15f16(off_matrix.checked_add(36 + 4 * i).ok_or_else(err)?)
                .ok_or_else(err)?;
        }
        Some((m, offset))
    };
    let encoding = if &pcs == b"Lab " {
        PcsEncoding::Lab16
    } else {
        PcsEncoding::Xyz16
    };
    Ok(LutPipeline {
        in_ch,
        pre_matrix: None,
        a_curves,
        clut,
        m_curves,
        matrix,
        b_curves,
        encoding,
    })
}

// ---------------------------------------------------------------------------
// Description
// ---------------------------------------------------------------------------

fn trim_text(s: &str) -> Option<String> {
    let t = s.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    (!t.is_empty()).then(|| t.to_string())
}

/// `textDescriptionType` (ICC.1:2001-04 §6.5.17) : compte ASCII puis chaîne.
fn parse_text_description(tag: Bytes<'_>) -> Option<String> {
    let count = usize::try_from(tag.u32(8)?).ok()?;
    let raw = tag.slice(12, count).or_else(|| tag.0.get(12..))?;
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    trim_text(
        &raw[..end]
            .iter()
            .map(|&b| char::from(b))
            .collect::<String>(),
    )
}

/// `multiLocalizedUnicodeType` (§10.15) : enregistrement « en » de préférence.
fn parse_mluc(tag: Bytes<'_>) -> Option<String> {
    let count = usize::try_from(tag.u32(8)?).ok()?;
    let record_size = usize::try_from(tag.u32(12)?).ok()?.max(12);
    let max_records = tag.len().saturating_sub(16) / record_size;
    let mut chosen: Option<(usize, usize)> = None;
    for i in 0..count.min(max_records) {
        let at = 16 + i * record_size;
        let lang = tag.slice(at, 2)?;
        let len = usize::try_from(tag.u32(at + 4)?).ok()?;
        let off = usize::try_from(tag.u32(at + 8)?).ok()?;
        if chosen.is_none() || lang == b"en" {
            chosen = Some((off, len));
            if lang == b"en" {
                break;
            }
        }
    }
    let (off, len) = chosen?;
    let raw = tag.slice(off, len & !1)?;
    let units = raw
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]));
    let text: String = char::decode_utf16(units)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect();
    trim_text(&text)
}

/// `textType` (§10.24) : ASCII terminé par un octet nul.
fn parse_text(tag: Bytes<'_>) -> Option<String> {
    let raw = tag.0.get(8..)?;
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    trim_text(
        &raw[..end]
            .iter()
            .map(|&b| char::from(b))
            .collect::<String>(),
    )
}

fn parse_description(tag: Bytes<'_>) -> Option<String> {
    match &tag.sig(0)? {
        b"desc" => parse_text_description(tag),
        b"mluc" => parse_mluc(tag),
        b"text" => parse_text(tag),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Profil
// ---------------------------------------------------------------------------

/// Nombre de canaux d'un espace de données (§7.2.6, tableau 19).
fn channels_of(space: [u8; 4]) -> Option<usize> {
    match &space {
        b"GRAY" => Some(1),
        b"XYZ " | b"Lab " | b"Luv " | b"YCbr" | b"Yxy " | b"RGB " | b"HSV " | b"HLS " | b"CMY " => {
            Some(3)
        }
        b"CMYK" => Some(4),
        [d, b'C', b'L', b'R'] => {
            let n = usize::try_from(char::from(*d).to_digit(16)?).ok()?;
            (2..=MAX_CHANNELS).contains(&n).then_some(n)
        }
        _ => None,
    }
}

/// Transformation appareil → PCS retenue pour le profil.
#[derive(Debug, Clone)]
enum Transform {
    /// Courbes `rTRC gTRC bTRC` puis matrice RVB linéaire → XYZ D50.
    MatrixTrc { trc: [Curve; 3], to_xyz: Mat3 },
    /// Courbe `kTRC` : luminance relative Y.
    GrayTrc(Curve),
    /// Table `A2Bx`.
    Lut(LutPipeline),
    /// Espace de données Lab sans table : entrées au codage Lab v4.
    LabEncoded,
}

/// Table des tags d'un profil, avec accès borné à chaque tag.
struct Parser<'a> {
    bytes: Bytes<'a>,
    tags: Vec<([u8; 4], usize, usize)>,
}

impl<'a> Parser<'a> {
    fn new(data: &'a [u8]) -> Result<Self> {
        let bytes = Bytes(data);
        if data.len() < HEADER_LEN + 4 {
            return Err(corrupt("en-tête tronqué"));
        }
        if bytes.slice(36, 4) != Some(b"acsp") {
            return Err(corrupt("signature « acsp » absente"));
        }
        let declared = usize::try_from(bytes.u32(HEADER_LEN).unwrap_or(0)).unwrap_or(0);
        let count = declared.min((data.len() - HEADER_LEN - 4) / 12);
        let mut tags = Vec::with_capacity(count);
        for i in 0..count {
            let at = HEADER_LEN + 4 + 12 * i;
            let (Some(sig), Some(off), Some(size)) =
                (bytes.sig(at), bytes.u32(at + 4), bytes.u32(at + 8))
            else {
                break;
            };
            let (Ok(off), Ok(size)) = (usize::try_from(off), usize::try_from(size)) else {
                continue;
            };
            // Un tag hors du fichier est ignoré, pas fatal (§7.3 : offset + size ≤ taille).
            if size >= 8 && off.checked_add(size).is_some_and(|end| end <= data.len()) {
                tags.push((sig, off, size));
            }
        }
        Ok(Self { bytes, tags })
    }

    fn tag(&self, sig: [u8; 4]) -> Option<Bytes<'a>> {
        let &(_, off, size) = self.tags.iter().find(|(s, _, _)| *s == sig)?;
        self.bytes.slice(off, size).map(Bytes)
    }

    /// Tag de type `XYZType` (§10.31).
    fn xyz(&self, sig: [u8; 4]) -> Option<[f64; 3]> {
        let t = self.tag(sig)?;
        (&t.sig(0)? == b"XYZ ").then(|| t.xyz_number(8))?
    }

    fn curve(&self, sig: [u8; 4]) -> Option<Curve> {
        Curve::parse(self.tag(sig)?, 0).map(|(c, _)| c)
    }

    /// Tag de type `s15Fixed16ArrayType` (§10.22) contenant une matrice 3 × 3.
    fn matrix(&self, sig: [u8; 4]) -> Option<Mat3> {
        let t = self.tag(sig)?;
        if &t.sig(0)? != b"sf32" {
            return None;
        }
        let mut m = [[0.0; 3]; 3];
        for (i, row) in m.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = t.s15f16(8 + 4 * (3 * i + j))?;
            }
        }
        Some(m)
    }

    fn lut(
        &self,
        sig: [u8; 4],
        in_ch: usize,
        pcs: [u8; 4],
        space: [u8; 4],
    ) -> Option<Result<LutPipeline>> {
        let t = self.tag(sig)?;
        Some(match &t.sig(0).unwrap_or([0; 4]) {
            b"mft1" => parse_lut8_16(t, false, in_ch, pcs, space),
            b"mft2" => parse_lut8_16(t, true, in_ch, pcs, space),
            b"mAB " => parse_mab(t, in_ch, pcs),
            other => Err(Error::Unsupported(format!(
                "type de LUT ICC « {} »",
                sig_text(*other)
            ))),
        })
    }

    /// Matrice/TRC (§8.3.4). Les colorants doivent sommer au blanc D50 ; si
    /// un profil v2 les a laissés sous son illuminant natif, ils sont adaptés
    /// par `chad` (§9.2.15) ou, à défaut, par Bradford depuis leur somme.
    fn matrix_trc(&self) -> Option<Transform> {
        let r = self.xyz(*b"rXYZ")?;
        let g = self.xyz(*b"gXYZ")?;
        let b = self.xyz(*b"bXYZ")?;
        let trc = [
            self.curve(*b"rTRC")?,
            self.curve(*b"gTRC")?,
            self.curve(*b"bTRC")?,
        ];
        let mut to_xyz = mat_from_columns(r, g, b);
        let sum = [r[0] + g[0] + b[0], r[1] + g[1] + b[1], r[2] + g[2] + b[2]];
        let adapted = sum.iter().zip(&D50).all(|(s, d)| (s - d).abs() <= 0.01);
        if !adapted {
            if let Some(adapt) = self
                .matrix(*b"chad")
                .or_else(|| bradford_adaptation(&sum, &D50))
            {
                to_xyz = mat_mul(&adapt, &to_xyz);
            }
        }
        Some(Transform::MatrixTrc { trc, to_xyz })
    }

    /// Choisit la transformation : `A2B0` (perceptuelle) puis `A2B1`, `A2B2`,
    /// puis matrice/TRC ou `kTRC`, puis Lab brut.
    fn transform(&self, components: usize, pcs: [u8; 4], space: [u8; 4]) -> Result<Transform> {
        let mut first_error = None;
        for sig in [b"A2B0", b"A2B1", b"A2B2"] {
            match self.lut(*sig, components, pcs, space) {
                Some(Ok(lut)) => return Ok(Transform::Lut(lut)),
                Some(Err(e)) => first_error.get_or_insert(e),
                None => continue,
            };
        }
        if components == 1 {
            if let Some(k) = self.curve(*b"kTRC") {
                return Ok(Transform::GrayTrc(k));
            }
        }
        if components == 3 {
            if let Some(t) = self.matrix_trc() {
                return Ok(t);
            }
            if &space == b"Lab " {
                return Ok(Transform::LabEncoded);
            }
        }
        Err(first_error.unwrap_or_else(|| {
            corrupt("aucune transformation utilisable (A2B0, matrice/TRC ou kTRC)")
        }))
    }
}

/// Profil ICC analysé, prêt à convertir des couleurs vers sRGB.
#[derive(Debug, Clone)]
pub struct IccProfile {
    version: (u8, u8),
    class: [u8; 4],
    data_space: [u8; 4],
    pcs: [u8; 4],
    white_point: [f64; 3],
    components: usize,
    description: Option<String>,
    transform: Transform,
}

impl IccProfile {
    /// Analyse un profil ICC (v2 ou v4) tel qu'incorporé dans un flux `/ICCBased`.
    ///
    /// # Errors
    ///
    /// [`Error::Corrupt`] si l'en-tête, la table des tags ou un tag
    /// nécessaire est invalide ou tronqué ; [`Error::Unsupported`] pour un
    /// espace de données ou un type de LUT non pris en charge. Aucune entrée
    /// ne provoque de panique.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let parser = Parser::new(data)?;
        let h = parser.bytes;
        let field = |v: Option<[u8; 4]>| v.ok_or_else(|| corrupt("en-tête tronqué"));
        let version = (h.u8(8).unwrap_or(2), h.u8(9).unwrap_or(0) >> 4);
        let class = field(h.sig(12))?;
        let data_space = field(h.sig(16))?;
        let pcs = field(h.sig(20))?;
        if &pcs != b"XYZ " && &pcs != b"Lab " {
            return Err(corrupt(&format!("PCS « {} » inconnu", sig_text(pcs))));
        }
        let components = channels_of(data_space).ok_or_else(|| {
            Error::Unsupported(format!(
                "espace de données ICC « {} »",
                sig_text(data_space)
            ))
        })?;
        let white_point = parser.xyz(*b"wtpt").unwrap_or(D50);
        let description = parser.tag(*b"desc").and_then(parse_description);
        let transform = parser.transform(components, pcs, data_space)?;
        Ok(Self {
            version,
            class,
            data_space,
            pcs,
            white_point,
            components,
            description,
            transform,
        })
    }

    /// Version `(majeure, mineure)` de l'en-tête (§7.2.4), par ex. `(2, 1)` ou `(4, 3)`.
    #[must_use]
    pub fn version(&self) -> (u8, u8) {
        self.version
    }

    /// Classe du profil (§7.2.5) : `mntr`, `scnr`, `prtr`, `spac`…
    #[must_use]
    pub fn profile_class(&self) -> [u8; 4] {
        self.class
    }

    /// Espace de données (§7.2.6) : `RGB `, `GRAY`, `CMYK`, `Lab `…
    #[must_use]
    pub fn data_space(&self) -> [u8; 4] {
        self.data_space
    }

    /// Espace de connexion (§7.2.7) : `XYZ ` ou `Lab `.
    #[must_use]
    pub fn pcs(&self) -> [u8; 4] {
        self.pcs
    }

    /// Point blanc du support (`wtpt`, §9.2.48), D50 s'il est absent.
    #[must_use]
    pub fn white_point(&self) -> [f64; 3] {
        self.white_point
    }

    /// Nombre de composantes d'entrée (1, 3 ou 4 pour les profils courants).
    #[must_use]
    pub fn components(&self) -> usize {
        self.components
    }

    /// Vrai si la conversion passe par les tags matrice/TRC (`rXYZ`… `bTRC`).
    #[must_use]
    pub fn is_matrix_trc(&self) -> bool {
        matches!(self.transform, Transform::MatrixTrc { .. })
    }

    /// Vrai si la conversion passe par une table `A2Bx`.
    #[must_use]
    pub fn is_lut(&self) -> bool {
        matches!(self.transform, Transform::Lut(_))
    }

    /// Description du profil (tag `desc`), si présente et lisible.
    #[must_use]
    pub fn description(&self) -> Option<String> {
        self.description.clone()
    }

    /// Détection rapide des profils sRGB courants, pour court-circuiter la
    /// conversion : description contenant « sRGB », ou profil matriciel dont
    /// les colorants et les courbes sont à moins de 1 % des valeurs sRGB.
    #[must_use]
    pub fn is_srgb_like(&self) -> bool {
        if self
            .description
            .as_deref()
            .is_some_and(|d| d.to_ascii_lowercase().contains("srgb"))
        {
            return true;
        }
        let Transform::MatrixTrc { trc, to_xyz } = &self.transform else {
            return false;
        };
        let reference = &colorimetry().srgb_colorants_d50;
        let colorants_close = to_xyz
            .iter()
            .flatten()
            .zip(reference.iter().flatten())
            .all(|(a, b)| (a - b).abs() <= 0.01);
        let curves_close = trc.iter().all(|c| {
            (0..=20).all(|i| {
                let x = f64::from(i) / 20.0;
                (c.eval(x) - srgb_to_linear(x)).abs() <= 0.01
            })
        });
        colorants_close && curves_close
    }

    /// Convertit une couleur de l'appareil (composantes dans `[0, 1]`, au
    /// nombre de [`components`](Self::components) ; les composantes manquantes
    /// valent 0) en sRGB non linéaire borné à `[0, 1]`.
    #[must_use]
    pub fn to_srgb(&self, input: &[f64]) -> [f32; 3] {
        let get = |i: usize| unit(input.get(i).copied().unwrap_or(0.0));
        let xyz = match &self.transform {
            Transform::MatrixTrc { trc, to_xyz } => {
                let linear = [
                    trc[0].eval(get(0)),
                    trc[1].eval(get(1)),
                    trc[2].eval(get(2)),
                ];
                mat_vec(to_xyz, &linear)
            }
            Transform::GrayTrc(curve) => {
                let v = to_f32(linear_to_srgb(curve.eval(get(0))));
                return [v; 3];
            }
            Transform::Lut(pipeline) => pipeline.to_xyz_d50(input),
            Transform::LabEncoded => lab_to_xyz_d50([
                get(0) * 100.0,
                get(1) * 255.0 - 128.0,
                get(2) * 255.0 - 128.0,
            ]),
        };
        xyz_d50_to_srgb(xyz)
    }
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/// Nombre de niveaux par composante du cache selon l'arité, borné pour que la
/// table reste petite (≤ 2²⁰ nœuds).
fn default_levels(components: usize) -> usize {
    match components {
        1 => 256,
        2 => 64,
        3 => 33,
        4 => 17,
        n => (2..=9usize)
            .rev()
            .find(|&levels| {
                levels
                    .checked_pow(u32::try_from(n).unwrap_or(u32::MAX))
                    .is_some_and(|nodes| nodes <= 1 << 20)
            })
            .unwrap_or(2),
    }
}

/// Cache de conversion : table n-D quantifiée (par défaut 256, 33 ou 17
/// niveaux pour 1, 3 ou 4 composantes) construite paresseusement au premier
/// appel, puis interpolée. Destiné aux images (scans CMJN de plusieurs
/// millions de pixels) ; la conversion exacte reste [`IccProfile::to_srgb`].
#[derive(Debug)]
pub struct IccCache {
    profile: IccProfile,
    levels: usize,
    grid: OnceLock<Grid>,
}

impl IccCache {
    /// Cache avec le nombre de niveaux par défaut pour l'arité du profil.
    #[must_use]
    pub fn new(profile: IccProfile) -> Self {
        let levels = default_levels(profile.components());
        Self::with_levels(profile, levels)
    }

    /// Cache avec `levels` niveaux par composante (ramené dans `[2, 256]` et
    /// réduit si la table dépasserait 2²⁴ nœuds).
    #[must_use]
    pub fn with_levels(profile: IccProfile, levels: usize) -> Self {
        let n = profile.components();
        let mut levels = levels.clamp(2, 256);
        while levels > 2
            && u32::try_from(n)
                .ok()
                .and_then(|n| levels.checked_pow(n))
                .is_none_or(|nodes| nodes > 1 << 24)
        {
            levels -= 1;
        }
        Self {
            profile,
            levels,
            grid: OnceLock::new(),
        }
    }

    /// Profil sous-jacent.
    #[must_use]
    pub fn profile(&self) -> &IccProfile {
        &self.profile
    }

    /// Rend le profil.
    #[must_use]
    pub fn into_profile(self) -> IccProfile {
        self.profile
    }

    /// Niveaux par composante.
    #[must_use]
    pub fn levels(&self) -> usize {
        self.levels
    }

    /// Vrai une fois la table construite.
    #[must_use]
    pub fn is_built(&self) -> bool {
        self.grid.get().is_some()
    }

    fn build(&self) -> Grid {
        let n = self.profile.components().clamp(1, MAX_CHANNELS);
        let dims = vec![self.levels; n];
        let count = Grid::element_count(&dims, 3).unwrap_or(0);
        let mut data = Vec::with_capacity(count);
        let mut index = vec![0usize; n];
        let mut input = vec![0.0f64; n];
        let step = count_to_f64(self.levels - 1);
        loop {
            for (v, &i) in input.iter_mut().zip(&index) {
                *v = count_to_f64(i) / step;
            }
            data.extend(self.profile.to_srgb(&input));
            // Incrément : le dernier canal varie le plus vite (ordre des CLUT).
            let mut carry = true;
            for i in index.iter_mut().rev() {
                *i += 1;
                if *i < self.levels {
                    carry = false;
                    break;
                }
                *i = 0;
            }
            if carry {
                break;
            }
        }
        Grid::new(dims, 3, data).unwrap_or_else(|| {
            // Inatteignable (tailles vérifiées dans `with_levels`) ; grille noire de secours.
            let dims = vec![2; n];
            let count = Grid::element_count(&dims, 3).unwrap_or(0);
            Grid::new(dims, 3, vec![0.0; count]).unwrap_or(Grid {
                dims: vec![2],
                out_ch: 3,
                strides: vec![3],
                data: vec![0.0; 6],
            })
        })
    }

    fn grid(&self) -> &Grid {
        self.grid.get_or_init(|| self.build())
    }

    /// Conversion interpolée dans le cache (mêmes conventions que
    /// [`IccProfile::to_srgb`]).
    #[must_use]
    pub fn to_srgb(&self, input: &[f64]) -> [f32; 3] {
        let mut out = [0.0; 3];
        self.grid().eval(input, &mut out);
        out.map(to_f32)
    }

    /// Conversion d'un pixel en octets (une composante par octet) vers des
    /// octets sRGB.
    #[must_use]
    pub fn to_srgb_u8(&self, input: &[u8]) -> [u8; 3] {
        let mut values = [0.0; MAX_CHANNELS];
        for (v, &b) in values.iter_mut().zip(input) {
            *v = f64::from(b) / 255.0;
        }
        let n = self.profile.components().clamp(1, MAX_CHANNELS);
        self.to_srgb(&values[..n]).map(to_u8)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::too_many_lines,
    clippy::too_many_arguments
)]
mod tests {
    use super::*;

    // --- Fabrique de profils binaires -------------------------------------

    fn be16(v: u16) -> [u8; 2] {
        v.to_be_bytes()
    }

    fn be32(v: u32) -> [u8; 4] {
        v.to_be_bytes()
    }

    fn s15f16(v: f64) -> [u8; 4] {
        ((v * 65536.0).round() as i32).to_be_bytes()
    }

    struct Builder {
        major: u8,
        class: [u8; 4],
        space: [u8; 4],
        pcs: [u8; 4],
        tags: Vec<([u8; 4], Vec<u8>)>,
    }

    impl Builder {
        fn new(major: u8, class: [u8; 4], space: [u8; 4], pcs: [u8; 4]) -> Self {
            Self {
                major,
                class,
                space,
                pcs,
                tags: Vec::new(),
            }
        }

        fn tag(mut self, sig: [u8; 4], data: Vec<u8>) -> Self {
            self.tags.push((sig, data));
            self
        }

        fn build(&self) -> Vec<u8> {
            let mut out = vec![0u8; HEADER_LEN];
            out[8] = self.major;
            out[9] = if self.major >= 4 { 0x30 } else { 0x10 };
            out[12..16].copy_from_slice(&self.class);
            out[16..20].copy_from_slice(&self.space);
            out[20..24].copy_from_slice(&self.pcs);
            out[36..40].copy_from_slice(b"acsp");
            out[68..72].copy_from_slice(&s15f16(D50[0]));
            out[72..76].copy_from_slice(&s15f16(D50[1]));
            out[76..80].copy_from_slice(&s15f16(D50[2]));
            out.extend_from_slice(&be32(self.tags.len() as u32));
            let table_at = out.len();
            out.resize(table_at + 12 * self.tags.len(), 0);
            for (i, (sig, data)) in self.tags.iter().enumerate() {
                while out.len() % 4 != 0 {
                    out.push(0);
                }
                let off = out.len();
                out.extend_from_slice(data);
                let entry = table_at + 12 * i;
                out[entry..entry + 4].copy_from_slice(sig);
                out[entry + 4..entry + 8].copy_from_slice(&be32(off as u32));
                out[entry + 8..entry + 12].copy_from_slice(&be32(data.len() as u32));
            }
            let size = out.len() as u32;
            out[0..4].copy_from_slice(&be32(size));
            out
        }
    }

    fn xyz_tag(v: [f64; 3]) -> Vec<u8> {
        let mut t = b"XYZ \0\0\0\0".to_vec();
        for c in v {
            t.extend_from_slice(&s15f16(c));
        }
        t
    }

    fn curv_tag(values: &[u16]) -> Vec<u8> {
        let mut t = b"curv\0\0\0\0".to_vec();
        t.extend_from_slice(&be32(values.len() as u32));
        for &v in values {
            t.extend_from_slice(&be16(v));
        }
        t
    }

    fn para_tag(kind: u16, params: &[f64]) -> Vec<u8> {
        let mut t = b"para\0\0\0\0".to_vec();
        t.extend_from_slice(&be16(kind));
        t.extend_from_slice(&[0, 0]);
        for &p in params {
            t.extend_from_slice(&s15f16(p));
        }
        t
    }

    /// Courbe sRGB en `parametricCurveType` de type 3.
    fn srgb_para() -> Vec<u8> {
        para_tag(3, &[2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045])
    }

    fn desc_v2(text: &str) -> Vec<u8> {
        let mut t = b"desc\0\0\0\0".to_vec();
        t.extend_from_slice(&be32(text.len() as u32 + 1));
        t.extend_from_slice(text.as_bytes());
        t.push(0);
        t.extend_from_slice(&[0; 8]); // code et compte Unicode
        t.extend_from_slice(&[0; 3]); // ScriptCode
        t.extend_from_slice(&[0; 67]);
        t
    }

    fn mluc_tag(records: &[(&[u8; 4], &str)]) -> Vec<u8> {
        let mut t = b"mluc\0\0\0\0".to_vec();
        t.extend_from_slice(&be32(records.len() as u32));
        t.extend_from_slice(&be32(12));
        let mut strings = Vec::new();
        let mut table = Vec::new();
        let base = 16 + 12 * records.len();
        for (lang, text) in records {
            let utf16: Vec<u8> = text.encode_utf16().flat_map(be16).collect();
            table.extend_from_slice(&lang[..]);
            table.extend_from_slice(&be32(utf16.len() as u32));
            table.extend_from_slice(&be32((base + strings.len()) as u32));
            strings.extend_from_slice(&utf16);
        }
        t.extend_from_slice(&table);
        t.extend_from_slice(&strings);
        t
    }

    fn sf32_tag(m: &Mat3) -> Vec<u8> {
        let mut t = b"sf32\0\0\0\0".to_vec();
        for row in m {
            for &v in row {
                t.extend_from_slice(&s15f16(v));
            }
        }
        t
    }

    fn lut16_tag(
        in_ch: u8,
        out_ch: u8,
        grid: u8,
        in_tables: &[Vec<u16>],
        clut: &[u16],
        out_tables: &[Vec<u16>],
    ) -> Vec<u8> {
        let mut t = b"mft2\0\0\0\0".to_vec();
        t.extend_from_slice(&[in_ch, out_ch, grid, 0]);
        for v in IDENTITY.iter().flatten() {
            t.extend_from_slice(&s15f16(*v));
        }
        t.extend_from_slice(&be16(in_tables[0].len() as u16));
        t.extend_from_slice(&be16(out_tables[0].len() as u16));
        for table in in_tables
            .iter()
            .chain([clut.to_vec()].iter())
            .chain(out_tables)
        {
            for &v in table {
                t.extend_from_slice(&be16(v));
            }
        }
        t
    }

    fn lut8_tag(in_ch: u8, out_ch: u8, grid: u8, clut: &[u8]) -> Vec<u8> {
        let mut t = b"mft1\0\0\0\0".to_vec();
        t.extend_from_slice(&[in_ch, out_ch, grid, 0]);
        for v in IDENTITY.iter().flatten() {
            t.extend_from_slice(&s15f16(*v));
        }
        let identity: Vec<u8> = (0..=255).collect();
        for _ in 0..in_ch {
            t.extend_from_slice(&identity);
        }
        t.extend_from_slice(clut);
        for _ in 0..out_ch {
            t.extend_from_slice(&identity);
        }
        t
    }

    fn pad4(v: &mut Vec<u8>) {
        while v.len() % 4 != 0 {
            v.push(0);
        }
    }

    /// `lutAToBType` ; `clut = (grille par dimension, précision, données)`.
    fn mab_tag(
        in_ch: u8,
        out_ch: u8,
        a_curves: Option<&[Vec<u8>]>,
        clut: Option<(&[u8], u8, &[u8])>,
        m_curves: Option<&[Vec<u8>]>,
        matrix: Option<&[f64; 12]>,
        b_curves: &[Vec<u8>],
    ) -> Vec<u8> {
        let mut t = b"mAB \0\0\0\0".to_vec();
        t.extend_from_slice(&[in_ch, out_ch, 0, 0]);
        t.extend_from_slice(&[0; 20]);
        let mut offsets = [0u32; 5];
        let push_curves = |t: &mut Vec<u8>, curves: &[Vec<u8>]| {
            pad4(t);
            let at = t.len() as u32;
            for c in curves {
                t.extend_from_slice(c);
                pad4(t);
            }
            at
        };
        offsets[0] = push_curves(&mut t, b_curves);
        if let Some(mx) = matrix {
            pad4(&mut t);
            offsets[1] = t.len() as u32;
            for &v in mx {
                t.extend_from_slice(&s15f16(v));
            }
        }
        if let Some(mc) = m_curves {
            offsets[2] = push_curves(&mut t, mc);
        }
        if let Some((grid, precision, data)) = clut {
            pad4(&mut t);
            offsets[3] = t.len() as u32;
            let mut g = [0u8; 16];
            g[..grid.len()].copy_from_slice(grid);
            t.extend_from_slice(&g);
            t.extend_from_slice(&[precision, 0, 0, 0]);
            t.extend_from_slice(data);
        }
        if let Some(ac) = a_curves {
            offsets[4] = push_curves(&mut t, ac);
        }
        for (i, off) in offsets.iter().enumerate() {
            t[12 + 4 * i..16 + 4 * i].copy_from_slice(&be32(*off));
        }
        t
    }

    // --- Colorimétrie de référence pour les tests ---------------------------

    const SRGB_R: [f64; 3] = [0.4361, 0.2225, 0.0139];
    const SRGB_G: [f64; 3] = [0.3851, 0.7169, 0.0971];
    const SRGB_B: [f64; 3] = [0.1431, 0.0606, 0.7141];

    fn srgb_to_xyz_d50(rgb: [f64; 3]) -> [f64; 3] {
        let linear = rgb.map(srgb_to_linear);
        mat_vec(&colorimetry().srgb_colorants_d50, &linear)
    }

    fn xyz_d50_to_lab(xyz: [f64; 3]) -> [f64; 3] {
        const DELTA: f64 = 6.0 / 29.0;
        let f = |t: f64| {
            if t > DELTA * DELTA * DELTA {
                t.cbrt()
            } else {
                t / (3.0 * DELTA * DELTA) + 4.0 / 29.0
            }
        };
        let fx = f(xyz[0] / D50[0]);
        let fy = f(xyz[1] / D50[1]);
        let fz = f(xyz[2] / D50[2]);
        [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
    }

    fn lab_legacy16(lab: [f64; 3]) -> [u16; 3] {
        let q = |v: f64| v.round().clamp(0.0, 65535.0) as u16;
        [
            q(lab[0] * 652.80),
            q((lab[1] + 128.0) * 256.0),
            q((lab[2] + 128.0) * 256.0),
        ]
    }

    fn lab_v4_8(lab: [f64; 3]) -> [u8; 3] {
        let q = |v: f64| v.round().clamp(0.0, 255.0) as u8;
        [q(lab[0] * 2.55), q(lab[1] + 128.0), q(lab[2] + 128.0)]
    }

    /// sRGB attendu d'une couleur passée par le codage Lab 8 bits (aller-retour).
    fn srgb_after_lab8(rgb: [f64; 3]) -> [f64; 3] {
        let q = lab_v4_8(xyz_d50_to_lab(srgb_to_xyz_d50(rgb)));
        let lab = [
            f64::from(q[0]) / 2.55,
            f64::from(q[1]) - 128.0,
            f64::from(q[2]) - 128.0,
        ];
        xyz_d50_to_srgb(lab_to_xyz_d50(lab)).map(f64::from)
    }

    fn max_abs_diff(a: [f32; 3], b: [f64; 3]) -> f64 {
        a.iter()
            .zip(&b)
            .map(|(x, y)| (f64::from(*x) - y).abs())
            .fold(0.0, f64::max)
    }

    // --- Profils synthétiques ---------------------------------------------

    fn srgb_profile() -> Vec<u8> {
        Builder::new(2, *b"mntr", *b"RGB ", *b"XYZ ")
            .tag(*b"desc", desc_v2("Synthetic RGB display"))
            .tag(*b"wtpt", xyz_tag(D50))
            .tag(*b"rXYZ", xyz_tag(SRGB_R))
            .tag(*b"gXYZ", xyz_tag(SRGB_G))
            .tag(*b"bXYZ", xyz_tag(SRGB_B))
            .tag(*b"rTRC", srgb_para())
            .tag(*b"gTRC", srgb_para())
            .tag(*b"bTRC", srgb_para())
            .build()
    }

    fn gray_profile() -> Vec<u8> {
        Builder::new(2, *b"mntr", *b"GRAY", *b"XYZ ")
            .tag(*b"wtpt", xyz_tag(D50))
            .tag(*b"kTRC", curv_tag(&[563])) // 2,2 en u8Fixed8 (563 / 256)
            .build()
    }

    /// CMJN → Lab par `lut16Type`, CLUT 3⁴ reproduisant (1−c)(1−k)…
    fn cmyk_lut16_profile() -> Vec<u8> {
        let mut clut = Vec::new();
        for ci in 0..3 {
            for mi in 0..3 {
                for yi in 0..3 {
                    for ki in 0..3 {
                        let (c, m, y, k) = (
                            f64::from(ci) / 2.0,
                            f64::from(mi) / 2.0,
                            f64::from(yi) / 2.0,
                            f64::from(ki) / 2.0,
                        );
                        let rgb = [
                            (1.0 - c) * (1.0 - k),
                            (1.0 - m) * (1.0 - k),
                            (1.0 - y) * (1.0 - k),
                        ];
                        clut.extend_from_slice(&lab_legacy16(xyz_d50_to_lab(srgb_to_xyz_d50(rgb))));
                    }
                }
            }
        }
        let identity = vec![0u16, 65535];
        Builder::new(2, *b"prtr", *b"CMYK", *b"Lab ")
            .tag(*b"desc", desc_v2("Synthetic CMYK press"))
            .tag(*b"wtpt", xyz_tag(D50))
            .tag(
                *b"A2B0",
                lut16_tag(
                    4,
                    3,
                    3,
                    &vec![identity.clone(); 4],
                    &clut,
                    &vec![identity; 3],
                ),
            )
            .build()
    }

    /// RVB → Lab par `lutAToBType` v4, CLUT 8 bits 3³, courbes A et B identité.
    fn rgb_mab_profile() -> Vec<u8> {
        let mut clut = Vec::new();
        for ri in 0..3 {
            for gi in 0..3 {
                for bi in 0..3 {
                    let rgb = [
                        f64::from(ri) / 2.0,
                        f64::from(gi) / 2.0,
                        f64::from(bi) / 2.0,
                    ];
                    clut.extend_from_slice(&lab_v4_8(xyz_d50_to_lab(srgb_to_xyz_d50(rgb))));
                }
            }
        }
        let identity = vec![curv_tag(&[]); 3];
        Builder::new(4, *b"mntr", *b"RGB ", *b"Lab ")
            .tag(*b"desc", mluc_tag(&[(b"enUS", "Synthetic v4 LUT")]))
            .tag(*b"wtpt", xyz_tag(D50))
            .tag(
                *b"A2B0",
                mab_tag(
                    3,
                    3,
                    Some(&identity),
                    Some((&[3, 3, 3], 1, &clut)),
                    None,
                    None,
                    &identity,
                ),
            )
            .build()
    }

    /// sRGB exprimé en `lutAToBType` sans CLUT : courbes M → matrice → courbes B, PCS XYZ.
    fn srgb_mab_matrix_profile() -> Vec<u8> {
        let scale = 32768.0 / 65535.0;
        let c = &colorimetry().srgb_colorants_d50;
        let mut matrix = [0.0; 12];
        for i in 0..3 {
            for j in 0..3 {
                matrix[3 * i + j] = c[i][j] * scale;
            }
        }
        let m_curves = vec![srgb_para(); 3];
        let b_curves = vec![curv_tag(&[]); 3];
        Builder::new(4, *b"mntr", *b"RGB ", *b"XYZ ")
            .tag(*b"wtpt", xyz_tag(D50))
            .tag(
                *b"A2B0",
                mab_tag(3, 3, None, None, Some(&m_curves), Some(&matrix), &b_curves),
            )
            .build()
    }

    /// RVB → Lab 8 bits par `lut8Type`, CLUT 2³.
    fn rgb_lut8_profile() -> Vec<u8> {
        let mut clut = Vec::new();
        for ri in 0..2 {
            for gi in 0..2 {
                for bi in 0..2 {
                    let rgb = [f64::from(ri), f64::from(gi), f64::from(bi)];
                    clut.extend_from_slice(&lab_v4_8(xyz_d50_to_lab(srgb_to_xyz_d50(rgb))));
                }
            }
        }
        Builder::new(2, *b"mntr", *b"RGB ", *b"Lab ")
            .tag(*b"A2B1", lut8_tag(3, 3, 2, &clut))
            .build()
    }

    // --- Tests --------------------------------------------------------------

    #[test]
    fn header_fields_are_read() {
        let p = IccProfile::parse(&srgb_profile()).unwrap();
        assert_eq!(p.version(), (2, 1));
        assert_eq!(&p.profile_class(), b"mntr");
        assert_eq!(&p.data_space(), b"RGB ");
        assert_eq!(&p.pcs(), b"XYZ ");
        assert_eq!(p.components(), 3);
        assert!(p.is_matrix_trc());
        assert!(!p.is_lut());
        assert!((p.white_point()[0] - D50[0]).abs() < 1e-4);
        assert_eq!(p.description().as_deref(), Some("Synthetic RGB display"));
    }

    #[test]
    fn synthetic_srgb_matrix_profile_is_identity() {
        let p = IccProfile::parse(&srgb_profile()).unwrap();
        let mut worst = 0.0f64;
        for r in 0..=8 {
            for g in 0..=8 {
                for b in 0..=8 {
                    let rgb = [f64::from(r) / 8.0, f64::from(g) / 8.0, f64::from(b) / 8.0];
                    let out = p.to_srgb(&rgb);
                    worst = worst.max(max_abs_diff(out, rgb));
                }
            }
        }
        println!(
            "profil sRGB synthétique : écart maximal {:.5} (soit {:.2}/255)",
            worst,
            worst * 255.0
        );
        assert!(worst <= 1.0 / 255.0, "écart {worst}");
        assert!(p.is_srgb_like());
    }

    #[test]
    fn gray_gamma_profile_maps_luminance() {
        let p = IccProfile::parse(&gray_profile()).unwrap();
        assert_eq!(p.components(), 1);
        assert!(!p.is_matrix_trc());
        assert_eq!(p.to_srgb(&[0.0]), [0.0; 3]);
        assert_eq!(p.to_srgb(&[1.0]), [1.0; 3]);
        let out = p.to_srgb(&[0.5]);
        let expected = linear_to_srgb(0.5f64.powf(563.0 / 256.0));
        assert!(out
            .iter()
            .all(|&v| (f64::from(v) - expected).abs() < 1.0 / 255.0));
        assert!(!p.is_srgb_like());
    }

    #[test]
    fn cmyk_lut16_profile_reproduces_primaries() {
        let p = IccProfile::parse(&cmyk_lut16_profile()).unwrap();
        assert_eq!(p.components(), 4);
        assert!(p.is_lut());
        assert!(!p.is_matrix_trc());
        let tol = 2.0 / 255.0;
        assert!(max_abs_diff(p.to_srgb(&[0.0, 0.0, 0.0, 0.0]), [1.0, 1.0, 1.0]) < tol);
        assert!(max_abs_diff(p.to_srgb(&[0.0, 0.0, 0.0, 1.0]), [0.0, 0.0, 0.0]) < tol);
        assert!(max_abs_diff(p.to_srgb(&[1.0, 0.0, 0.0, 0.0]), [0.0, 1.0, 1.0]) < tol);
        assert!(max_abs_diff(p.to_srgb(&[0.0, 1.0, 0.0, 0.0]), [1.0, 0.0, 1.0]) < tol);
        assert!(max_abs_diff(p.to_srgb(&[0.0, 0.0, 1.0, 0.0]), [1.0, 1.0, 0.0]) < tol);
        // Nœud intermédiaire de la CLUT : exact aussi.
        assert!(max_abs_diff(p.to_srgb(&[0.5, 0.0, 0.0, 0.0]), [0.5, 1.0, 1.0]) < tol);
        // Point intérieur : proche de la formule multiplicative.
        let out = p.to_srgb(&[0.25, 0.1, 0.0, 0.3]);
        assert!(
            max_abs_diff(out, [0.75 * 0.7, 0.9 * 0.7, 0.7]) < 0.08,
            "{out:?}"
        );
    }

    #[test]
    fn v4_lut_atob_with_8bit_clut() {
        let p = IccProfile::parse(&rgb_mab_profile()).unwrap();
        assert_eq!(p.version(), (4, 3));
        assert!(p.is_lut());
        assert_eq!(p.description().as_deref(), Some("Synthetic v4 LUT"));
        for r in 0..3 {
            for g in 0..3 {
                for b in 0..3 {
                    let rgb = [f64::from(r) / 2.0, f64::from(g) / 2.0, f64::from(b) / 2.0];
                    let out = p.to_srgb(&rgb);
                    // Exact au regard du codage Lab 8 bits ; l'écart à la couleur
                    // idéale (jusqu'à 0,07 sur un canal nul d'une couleur saturée)
                    // vient de la quantification de a*/b* à l'unité.
                    assert!(
                        max_abs_diff(out, srgb_after_lab8(rgb)) < 1.0 / 255.0,
                        "{rgb:?} → {out:?}"
                    );
                    assert!(max_abs_diff(out, rgb) < 0.1, "{rgb:?} → {out:?}");
                }
            }
        }
        // Sur les gris (a* = b* = 0 exacts), seule la quantification de L* joue.
        let out = p.to_srgb(&[0.5, 0.5, 0.5]);
        assert!(max_abs_diff(out, [0.5; 3]) < 2.0 / 255.0, "{out:?}");
    }

    #[test]
    fn v4_lut_atob_matrix_only_is_identity() {
        let p = IccProfile::parse(&srgb_mab_matrix_profile()).unwrap();
        assert!(p.is_lut());
        for r in 0..=4 {
            for g in 0..=4 {
                for b in 0..=4 {
                    let rgb = [f64::from(r) / 4.0, f64::from(g) / 4.0, f64::from(b) / 4.0];
                    let out = p.to_srgb(&rgb);
                    assert!(max_abs_diff(out, rgb) < 1.0 / 255.0, "{rgb:?} → {out:?}");
                }
            }
        }
    }

    #[test]
    fn lut8_profile_with_8bit_lab() {
        let p = IccProfile::parse(&rgb_lut8_profile()).unwrap();
        assert!(p.is_lut());
        for r in 0..2 {
            for g in 0..2 {
                for b in 0..2 {
                    let rgb = [f64::from(r), f64::from(g), f64::from(b)];
                    let out = p.to_srgb(&rgb);
                    assert!(
                        max_abs_diff(out, srgb_after_lab8(rgb)) < 1.0 / 255.0,
                        "{rgb:?} → {out:?}"
                    );
                    assert!(max_abs_diff(out, rgb) < 0.1);
                }
            }
        }
    }

    #[test]
    fn lab_data_space_without_lut_uses_v4_encoding() {
        let p = IccProfile::parse(&Builder::new(2, *b"spac", *b"Lab ", *b"Lab ").build()).unwrap();
        assert_eq!(p.components(), 3);
        let white = p.to_srgb(&[1.0, 128.0 / 255.0, 128.0 / 255.0]);
        assert!(max_abs_diff(white, [1.0, 1.0, 1.0]) < 2.0 / 255.0);
        assert_eq!(p.to_srgb(&[0.0, 128.0 / 255.0, 128.0 / 255.0]), [0.0; 3]);
    }

    #[test]
    fn unadapted_v2_colorants_are_adapted_with_chad_or_bradford() {
        let d65 = xy_to_xyz(D65_XY);
        let m =
            rgb_matrix_from_chromaticities(SRGB_RED_XY, SRGB_GREEN_XY, SRGB_BLUE_XY, &d65).unwrap();
        let col = |j: usize| [m[0][j], m[1][j], m[2][j]];
        let base = || {
            Builder::new(2, *b"mntr", *b"RGB ", *b"XYZ ")
                .tag(*b"wtpt", xyz_tag(d65))
                .tag(*b"rXYZ", xyz_tag(col(0)))
                .tag(*b"gXYZ", xyz_tag(col(1)))
                .tag(*b"bXYZ", xyz_tag(col(2)))
                .tag(*b"rTRC", srgb_para())
                .tag(*b"gTRC", srgb_para())
                .tag(*b"bTRC", srgb_para())
        };
        let with_chad = base()
            .tag(
                *b"chad",
                sf32_tag(&bradford_adaptation(&d65, &D50).unwrap()),
            )
            .build();
        for bytes in [base().build(), with_chad] {
            let p = IccProfile::parse(&bytes).unwrap();
            for rgb in [[1.0, 1.0, 1.0], [0.5, 0.2, 0.8], [0.0, 1.0, 0.0]] {
                let out = p.to_srgb(&rgb);
                assert!(max_abs_diff(out, rgb) < 1.5 / 255.0, "{rgb:?} → {out:?}");
            }
        }
    }

    #[test]
    fn description_v2_and_v4() {
        let v2 = Builder::new(2, *b"mntr", *b"GRAY", *b"XYZ ")
            .tag(*b"desc", desc_v2("Gris v2"))
            .tag(*b"kTRC", curv_tag(&[]))
            .build();
        assert_eq!(
            IccProfile::parse(&v2).unwrap().description().as_deref(),
            Some("Gris v2")
        );
        let v4 = Builder::new(4, *b"mntr", *b"GRAY", *b"XYZ ")
            .tag(
                *b"desc",
                mluc_tag(&[(b"frFR", "Gris étalonné"), (b"enUS", "Calibrated gray")]),
            )
            .tag(*b"kTRC", curv_tag(&[]))
            .build();
        assert_eq!(
            IccProfile::parse(&v4).unwrap().description().as_deref(),
            Some("Calibrated gray")
        );
        let only_fr = Builder::new(4, *b"mntr", *b"GRAY", *b"XYZ ")
            .tag(*b"desc", mluc_tag(&[(b"frFR", "Gris étalonné")]))
            .tag(*b"kTRC", curv_tag(&[]))
            .build();
        assert_eq!(
            IccProfile::parse(&only_fr)
                .unwrap()
                .description()
                .as_deref(),
            Some("Gris étalonné")
        );
        let none = Builder::new(2, *b"mntr", *b"GRAY", *b"XYZ ")
            .tag(*b"kTRC", curv_tag(&[]))
            .build();
        assert_eq!(IccProfile::parse(&none).unwrap().description(), None);
    }

    #[test]
    fn srgb_detection_by_description_and_by_values() {
        let by_desc = Builder::new(2, *b"mntr", *b"GRAY", *b"XYZ ")
            .tag(*b"desc", desc_v2("sRGB IEC61966-2.1"))
            .tag(*b"kTRC", curv_tag(&[]))
            .build();
        assert!(IccProfile::parse(&by_desc).unwrap().is_srgb_like());
        // Adobe RGB (1998) : primaires différentes, gamma 2,2.
        let adobe = Builder::new(2, *b"mntr", *b"RGB ", *b"XYZ ")
            .tag(*b"rXYZ", xyz_tag([0.6097, 0.3111, 0.0195]))
            .tag(*b"gXYZ", xyz_tag([0.2052, 0.6257, 0.0609]))
            .tag(*b"bXYZ", xyz_tag([0.1492, 0.0632, 0.7446]))
            .tag(*b"rTRC", curv_tag(&[563]))
            .tag(*b"gTRC", curv_tag(&[563]))
            .tag(*b"bTRC", curv_tag(&[563]))
            .build();
        let p = IccProfile::parse(&adobe).unwrap();
        assert!(!p.is_srgb_like());
        // Le vert Adobe RGB saturé sort du gamut sRGB : borné, pas d'identité.
        let g = p.to_srgb(&[0.0, 1.0, 0.0]);
        assert!(g[1] >= 0.99 && g[0] <= 0.01);
    }

    #[test]
    fn parametric_curves_follow_the_spec_formulas() {
        let p = |kind: u16, params: &[f64]| {
            let tag = para_tag(kind, params);
            Curve::parse(Bytes(&tag), 0).unwrap().0
        };
        let c0 = p(0, &[2.0]);
        assert!((c0.eval(0.5) - 0.25).abs() < 1e-4);
        let c1 = p(1, &[1.0, 2.0, -0.5]);
        assert!((c1.eval(0.5) - 0.5).abs() < 1e-4 && c1.eval(0.1) == 0.0);
        let c2 = p(2, &[1.0, 1.0, -0.5, 0.25]);
        assert!((c2.eval(0.1) - 0.25).abs() < 1e-4 && (c2.eval(1.0) - 0.75).abs() < 1e-4);
        let c3 = p(3, &[2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045]);
        for i in 0..=10 {
            let x = f64::from(i) / 10.0;
            assert!((c3.eval(x) - srgb_to_linear(x)).abs() < 1e-4);
        }
        let c4 = p(4, &[1.0, 1.0, 0.0, 0.5, 0.5, 0.25, 0.1]);
        assert!((c4.eval(0.2) - 0.2).abs() < 1e-4 && (c4.eval(0.6) - 0.85).abs() < 1e-4);
        // Type inconnu : refusé.
        assert!(Curve::parse(Bytes(&para_tag(7, &[1.0])), 0).is_none());
        // Table `curv` interpolée, et gamma.
        let table = curv_tag(&[0, 32768, 65535]);
        let t = Curve::parse(Bytes(&table), 0).unwrap().0;
        assert!((t.eval(0.25) - 0.25).abs() < 1e-3);
        let gamma = Curve::parse(Bytes(&curv_tag(&[256])), 0).unwrap().0;
        assert_eq!(gamma.eval(0.3), 0.3);
    }

    #[test]
    fn tetrahedral_and_multilinear_agree_on_linear_data() {
        // f(x, y, z) = 0,2x + 0,3y + 0,5z sur une grille 3 × 4 × 2.
        let dims = vec![3usize, 4, 2];
        let mut data = Vec::new();
        for i in 0..3 {
            for j in 0..4 {
                for k in 0..2 {
                    data.push((0.2 * i as f32 / 2.0) + (0.3 * j as f32 / 3.0) + 0.5 * k as f32);
                }
            }
        }
        let grid = Grid::new(dims, 1, data).unwrap();
        let mut seed = 7u64;
        for _ in 0..200 {
            let mut input = [0.0; 3];
            for v in &mut input {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                *v = (seed >> 11) as f64 / (1u64 << 53) as f64;
            }
            let expected = 0.2 * input[0] + 0.3 * input[1] + 0.5 * input[2];
            let (mut a, mut b) = ([0.0], [0.0]);
            grid.eval(&input, &mut a);
            grid.eval_multilinear(&input, &mut b);
            assert!((a[0] - expected).abs() < 1e-5, "{a:?} vs {expected}");
            assert!((b[0] - expected).abs() < 1e-5);
        }
        // 4-D : multilinéaire sur le premier axe, tétraédrique sur les trois autres.
        let dims4 = vec![2usize, 2, 2, 2];
        let mut data4 = Vec::new();
        for idx in 0..16u32 {
            let bits = (0..4).map(|b| f32::from(u8::from((idx >> (3 - b)) & 1 == 1)));
            data4.push(
                bits.zip([0.1f32, 0.2, 0.3, 0.4])
                    .map(|(b, w)| b * w)
                    .sum::<f32>(),
            );
        }
        let grid4 = Grid::new(dims4, 1, data4).unwrap();
        let input = [0.3, 0.9, 0.5, 0.1];
        let mut out = [0.0];
        grid4.eval(&input, &mut out);
        assert!((out[0] - (0.03 + 0.18 + 0.15 + 0.04)).abs() < 1e-5);
        // Grille invalide : un seul nœud, ou données de mauvaise taille.
        assert!(Grid::new(vec![1, 2], 1, vec![0.0; 2]).is_none());
        assert!(Grid::new(vec![2, 2], 1, vec![0.0; 3]).is_none());
    }

    #[test]
    fn cache_matches_direct_conversion() {
        let cache = IccCache::new(IccProfile::parse(&srgb_profile()).unwrap());
        assert_eq!(cache.levels(), 33);
        assert!(!cache.is_built());
        for rgb in [
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.31, 0.77, 0.12],
            [0.98, 0.02, 0.5],
        ] {
            let direct = cache.profile().to_srgb(&rgb);
            let cached = cache.to_srgb(&rgb);
            assert!(max_abs_diff(cached, direct.map(f64::from)) < 1.0 / 255.0);
        }
        assert!(cache.is_built());
        assert_eq!(cache.to_srgb_u8(&[255, 0, 128]), [255, 0, 128]);

        let cmyk = IccCache::new(IccProfile::parse(&cmyk_lut16_profile()).unwrap());
        assert_eq!(cmyk.levels(), 17);
        for c in [
            [0.0; 4],
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 0.0],
            [0.2, 0.4, 0.6, 0.1],
        ] {
            let direct = cmyk.profile().to_srgb(&c);
            let cached = cmyk.to_srgb(&c);
            assert!(
                max_abs_diff(cached, direct.map(f64::from)) < 3.0 / 255.0,
                "{c:?}"
            );
        }
        let gray = IccCache::new(IccProfile::parse(&gray_profile()).unwrap());
        assert_eq!(gray.levels(), 256);
        assert_eq!(
            gray.to_srgb_u8(&[200]),
            gray.profile().to_srgb(&[200.0 / 255.0]).map(to_u8)
        );
        let small = IccCache::with_levels(gray.into_profile(), 1);
        assert_eq!(small.levels(), 2);
        assert_eq!(default_levels(6), 9);
        assert_eq!(default_levels(7), 7);
        assert_eq!(default_levels(15), 2);
    }

    #[test]
    fn invalid_headers_are_rejected() {
        assert!(IccProfile::parse(&[]).is_err());
        assert!(IccProfile::parse(&[0; 200]).is_err());
        let mut no_acsp = srgb_profile();
        no_acsp[36] = b'x';
        assert!(matches!(
            IccProfile::parse(&no_acsp),
            Err(Error::Corrupt(_))
        ));
        let bad_pcs = Builder::new(2, *b"mntr", *b"RGB ", *b"Luv ").build();
        assert!(IccProfile::parse(&bad_pcs).is_err());
        let unknown_space = Builder::new(2, *b"mntr", *b"ZZZZ", *b"XYZ ").build();
        assert!(matches!(
            IccProfile::parse(&unknown_space),
            Err(Error::Unsupported(_))
        ));
        let no_tags = Builder::new(2, *b"mntr", *b"RGB ", *b"XYZ ").build();
        assert!(IccProfile::parse(&no_tags).is_err());
        // Tag LUT inexploitable mais matrice/TRC présente : on retombe dessus.
        let mut with_bad_lut = Builder::new(2, *b"mntr", *b"RGB ", *b"XYZ ");
        with_bad_lut = with_bad_lut.tag(*b"A2B0", b"mft2\0\0\0\0\x03\x03\x00\x00".to_vec());
        let bytes = with_bad_lut
            .tag(*b"rXYZ", xyz_tag(SRGB_R))
            .tag(*b"gXYZ", xyz_tag(SRGB_G))
            .tag(*b"bXYZ", xyz_tag(SRGB_B))
            .tag(*b"rTRC", srgb_para())
            .tag(*b"gTRC", srgb_para())
            .tag(*b"bTRC", srgb_para())
            .build();
        assert!(IccProfile::parse(&bytes).unwrap().is_matrix_trc());
    }

    fn check_no_panic(bytes: &[u8]) {
        if let Ok(p) = IccProfile::parse(bytes) {
            for input in [
                [0.0; 4],
                [1.0; 4],
                [0.3, 0.6, 0.9, 0.2],
                [f64::NAN, 2.0, -1.0, 0.5],
            ] {
                let out = p.to_srgb(&input[..p.components().min(4)]);
                assert!(out.iter().all(|v| (0.0..=1.0).contains(v)), "{out:?}");
            }
            let _ = p.description();
            let _ = p.is_srgb_like();
            let cache = IccCache::with_levels(p, 3);
            let out = cache.to_srgb(&[0.5; 4]);
            assert!(out.iter().all(|v| (0.0..=1.0).contains(v)));
        }
    }

    #[test]
    fn truncated_and_random_profiles_never_panic() {
        let profiles = [
            srgb_profile(),
            gray_profile(),
            cmyk_lut16_profile(),
            rgb_mab_profile(),
            srgb_mab_matrix_profile(),
            rgb_lut8_profile(),
        ];
        for bytes in &profiles {
            for len in 0..bytes.len() {
                check_no_panic(&bytes[..len]);
            }
        }
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 56) as u8
        };
        for _ in 0..300 {
            let len = usize::from(next()) * 4;
            let buf: Vec<u8> = (0..len).map(|_| next()).collect();
            check_no_panic(&buf);
            let mut with_sig = buf.clone();
            if with_sig.len() >= 40 {
                with_sig[36..40].copy_from_slice(b"acsp");
                with_sig[20..24].copy_from_slice(b"Lab ");
            }
            check_no_panic(&with_sig);
        }
        // Mutations d'octets sur des profils valides (après l'en-tête).
        for bytes in &profiles {
            for _ in 0..300 {
                let mut mutated = bytes.clone();
                let flips = 1 + usize::from(next()) % 8;
                for _ in 0..flips {
                    let at = HEADER_LEN
                        + (usize::from(next()) * 256 + usize::from(next()))
                            % (mutated.len() - HEADER_LEN);
                    mutated[at] = next();
                }
                check_no_panic(&mutated);
            }
        }
    }

    #[test]
    fn colorimetric_constants_are_consistent() {
        // Les colorants sRGB dérivés des chromaticités retrouvent les valeurs publiées.
        let c = &colorimetry().srgb_colorants_d50;
        for (j, published) in [SRGB_R, SRGB_G, SRGB_B].iter().enumerate() {
            for i in 0..3 {
                assert!(
                    (c[i][j] - published[i]).abs() < 2e-4,
                    "colorant {j} composante {i}"
                );
            }
        }
        // Blanc D50 du PCS → sRGB blanc ; Lab (100, 0, 0) → blanc ; Lab noir → noir.
        assert!(max_abs_diff(xyz_d50_to_srgb(D50), [1.0; 3]) < 1e-3);
        assert!(max_abs_diff(xyz_d50_to_srgb(lab_to_xyz_d50([100.0, 0.0, 0.0])), [1.0; 3]) < 1e-3);
        assert_eq!(xyz_d50_to_srgb(lab_to_xyz_d50([0.0, 0.0, 0.0])), [0.0; 3]);
        assert!((srgb_to_linear(linear_to_srgb(0.2)) - 0.2).abs() < 1e-9);
        assert_eq!(linear_to_srgb(f64::NAN), 0.0);
        assert!(mat_inv(&[[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [0.0, 0.0, 1.0]]).is_none());
    }

    #[test]
    #[ignore = "mesure de performance : cargo test -p acrux-graphics --release -- --ignored --nocapture"]
    fn bench_one_million_cmyk_conversions_with_cache() {
        let cache = IccCache::new(IccProfile::parse(&cmyk_lut16_profile()).unwrap());
        let build = std::time::Instant::now();
        let _ = cache.to_srgb(&[0.0; 4]);
        let build_time = build.elapsed();
        let start = std::time::Instant::now();
        let mut acc = 0.0f32;
        let mut seed = 1u32;
        for _ in 0..1_000_000 {
            let mut px = [0u8; 4];
            for b in &mut px {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *b = (seed >> 24) as u8;
            }
            let out = cache.to_srgb_u8(&px);
            acc += f32::from(out[0]) + f32::from(out[1]) + f32::from(out[2]);
        }
        let elapsed = start.elapsed();
        println!(
            "cache CMJN : construction {build_time:?}, 1 M de conversions en {elapsed:?} (somme {acc})"
        );
        if !cfg!(debug_assertions) {
            assert!(elapsed.as_secs_f64() < 1.0, "trop lent : {elapsed:?}");
        }
    }
}
