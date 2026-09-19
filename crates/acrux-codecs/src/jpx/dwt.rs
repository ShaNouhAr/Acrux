//! Transformée en ondelettes discrète inverse (ISO/IEC 15444-1 annexe F) :
//! filtre 5-3 réversible en entiers (F.3.8.2) et filtre 9-7 irréversible
//! en virgule flottante par étapes de « lifting » (F.3.8.6), extension
//! périodique symétrique (F.3.7), procédure 2D_SR (F.3.2 à F.3.5) sur une
//! résolution : entrelacement 2D (F.3.3) puis filtrage des lignes (HOR_SR)
//! et des colonnes (VER_SR).
//!
//! Les signaux sont manipulés avec leurs coordonnées absolues `[i0, i1)`
//! sur la grille de la résolution, car la parité de `i0` détermine quels
//! échantillons sont passe-bas (pairs) et passe-haut (impairs).
//!
//! Constantes du filtre 9-7 (tableau F.4). Les facteurs d'échelle des
//! étapes 1 et 2 sont choisis pour que la synthèse soit l'inverse exacte
//! d'une analyse dont le passe-bas a un gain continu de 1 et le passe-haut
//! un gain de Nyquist de 2 (gains nominaux du tableau E.1) : les
//! coefficients pairs sont multipliés par K et les impairs par 1/K.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use super::structure::Rect;

/// Échantillons d'extension de chaque côté du signal (le 9-7 en lit 4).
const EXT: usize = 4;

/// Paramètres de lifting du filtre 9-7 irréversible (tableau F.4).
const ALPHA: f32 = -1.586_134_3;
const BETA: f32 = -0.052_980_12;
const GAMMA: f32 = 0.882_911_1;
const DELTA: f32 = 0.443_506_87;
const K: f32 = 1.230_174_1;

/// Extension périodique symétrique (F.3.7, équation F-4) : indice source
/// dans `[i0, i1)` de la position `i`, par réflexion autour des bords.
fn pse(i: i64, i0: i64, i1: i64) -> i64 {
    let n = i1 - i0;
    if n <= 1 {
        return i0;
    }
    let period = 2 * (n - 1);
    let mut m = (i - i0).rem_euclid(period);
    if m >= n {
        m = period - m;
    }
    i0 + m
}

/// Charge le signal de longueur `n` commençant en `i0` dans `buf`
/// (longueur `n + 2·EXT`) avec ses extensions.
fn load<T: Copy>(src: impl Fn(usize) -> T, n: usize, i0: i64, buf: &mut [T]) {
    #[allow(clippy::cast_possible_wrap)]
    let i1 = i0 + n as i64;
    for (k, slot) in buf.iter_mut().enumerate().take(n + 2 * EXT) {
        #[allow(clippy::cast_possible_wrap)]
        let i = i0 - EXT as i64 + k as i64;
        #[allow(clippy::cast_sign_loss)]
        let j = (pse(i, i0, i1) - i0) as usize;
        *slot = src(j);
    }
}

/// Filtre 5-3 réversible inverse (F.3.8.2, équations F-5 et F-6) sur un
/// tampon étendu : `buf[k]` correspond à la position `i0 − EXT + k`.
pub(super) fn filter_53(buf: &mut [i32], i0: i64, i1: i64) {
    if i1 - i0 == 1 {
        // F.3.7 : un seul échantillon, X = Y si i0 pair, Y/2 sinon.
        if i0.rem_euclid(2) == 1 {
            buf[EXT] /= 2;
        }
        return;
    }
    #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
    let at = |i: i64| (i - i0 + EXT as i64) as usize;
    // STEP1 : X(2n) = Y(2n) − ⌊(Y(2n−1) + Y(2n+1) + 2) / 4⌋, pairs de [i0−1, i1+1).
    let mut i = i0 - 1 + (i0 - 1).rem_euclid(2);
    while i < i1 + 1 {
        buf[at(i)] -= (buf[at(i - 1)] + buf[at(i + 1)] + 2) >> 2;
        i += 2;
    }
    // STEP2 : X(2n+1) = Y(2n+1) + ⌊(X(2n) + X(2n+2)) / 2⌋, impairs de [i0, i1).
    let mut i = i0 + 1 - i0.rem_euclid(2);
    while i < i1 {
        buf[at(i)] += (buf[at(i - 1)] + buf[at(i + 1)]) >> 1;
        i += 2;
    }
}

/// Filtre 9-7 irréversible inverse (F.3.8.6, équations F-7 à F-12).
pub(super) fn filter_97(buf: &mut [f32], i0: i64, i1: i64) {
    if i1 - i0 == 1 {
        if i0.rem_euclid(2) == 1 {
            buf[EXT] /= 2.0;
        }
        return;
    }
    #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
    let at = |i: i64| (i - i0 + EXT as i64) as usize;
    let first_even = |from: i64| from + from.rem_euclid(2);
    let first_odd = |from: i64| from + 1 - from.rem_euclid(2);
    // STEP1 : pairs de [i0−3, i1+3) multipliés par K.
    let mut i = first_even(i0 - 3);
    while i < i1 + 3 {
        buf[at(i)] *= K;
        i += 2;
    }
    // STEP2 : impairs de [i0−4, i1+4) multipliés par 1/K.
    let mut i = first_odd(i0 - 4);
    while i < i1 + 4 {
        buf[at(i)] /= K;
        i += 2;
    }
    // STEP3 : X(2n) −= δ (X(2n−1) + X(2n+1)), pairs de [i0−3, i1+3).
    let mut i = first_even(i0 - 3);
    while i < i1 + 3 {
        buf[at(i)] -= DELTA * (buf[at(i - 1)] + buf[at(i + 1)]);
        i += 2;
    }
    // STEP4 : X(2n+1) −= γ (X(2n) + X(2n+2)), impairs de [i0−2, i1+2).
    let mut i = first_odd(i0 - 2);
    while i < i1 + 2 {
        buf[at(i)] -= GAMMA * (buf[at(i - 1)] + buf[at(i + 1)]);
        i += 2;
    }
    // STEP5 : X(2n) −= β (X(2n−1) + X(2n+1)), pairs de [i0−1, i1+1).
    let mut i = first_even(i0 - 1);
    while i < i1 + 1 {
        buf[at(i)] -= BETA * (buf[at(i - 1)] + buf[at(i + 1)]);
        i += 2;
    }
    // STEP6 : X(2n+1) −= α (X(2n) + X(2n+2)), impairs de [i0, i1).
    let mut i = first_odd(i0);
    while i < i1 {
        buf[at(i)] -= ALPHA * (buf[at(i - 1)] + buf[at(i + 1)]);
        i += 2;
    }
}

/// Sous-bandes d'une résolution, chacune stockée ligne par ligne sur son
/// propre rectangle (B.5, équation B-15). Une sous-bande absente ou de
/// taille inattendue est lue comme nulle.
pub(super) struct Subbands<'a, T> {
    pub ll: &'a [T],
    pub hl: &'a [T],
    pub lh: &'a [T],
    pub hh: &'a [T],
}

/// Procédure 2D_SR (F.3.2) : reconstruit la résolution `res` à partir de
/// la résolution inférieure (LL) et des trois sous-bandes. `out` reçoit
/// `res.width() × res.height()` échantillons ; `scratch` est un tampon de
/// travail réutilisé entre les appels.
pub(super) fn synthesize_level<T: Copy + Default>(
    res: Rect,
    bands: &Subbands<'_, T>,
    out: &mut Vec<T>,
    scratch: &mut Vec<T>,
    filter: fn(&mut [T], i64, i64),
) {
    let (u0, u1, v0, v1) = (
        u64::from(res.x0),
        u64::from(res.x1),
        u64::from(res.y0),
        u64::from(res.y1),
    );
    let w = usize::try_from(u1.saturating_sub(u0)).unwrap_or(0);
    let h = usize::try_from(v1.saturating_sub(v0)).unwrap_or(0);
    out.clear();
    out.resize(w * h, T::default());
    if w == 0 || h == 0 {
        return;
    }
    // 2D_INTERLEAVE (F.3.3) : les sous-bandes occupent les sous-réseaux
    // pair/impair de la résolution.
    let low_w = usize::try_from(u1.div_ceil(2) - u0.div_ceil(2)).unwrap_or(0);
    let high_w = usize::try_from(u1 / 2 - u0 / 2).unwrap_or(0);
    let x_low0 = u0.div_ceil(2);
    let x_high0 = u0 / 2;
    let y_low0 = v0.div_ceil(2);
    let y_high0 = v0 / 2;
    for y in v0..v1 {
        let row = &mut out[usize::try_from(y - v0).unwrap_or(0) * w..][..w];
        let odd_row = y % 2 == 1;
        let by = usize::try_from(if odd_row {
            y / 2 - y_high0
        } else {
            y / 2 - y_low0
        })
        .unwrap_or(0);
        for (k, slot) in row.iter_mut().enumerate() {
            let x = u0 + k as u64;
            let odd_col = x % 2 == 1;
            let bx = usize::try_from(if odd_col {
                x / 2 - x_high0
            } else {
                x / 2 - x_low0
            })
            .unwrap_or(0);
            let (band, bw): (&[T], usize) = match (odd_col, odd_row) {
                (false, false) => (bands.ll, low_w),
                (true, false) => (bands.hl, high_w),
                (false, true) => (bands.lh, low_w),
                (true, true) => (bands.hh, high_w),
            };
            *slot = band.get(by * bw + bx).copied().unwrap_or_default();
        }
    }
    #[allow(clippy::cast_possible_wrap)]
    let (i0, i1, j0, j1) = (u0 as i64, u1 as i64, v0 as i64, v1 as i64);
    // HOR_SR (F.3.4) : chaque ligne.
    scratch.clear();
    scratch.resize(w.max(h) + 2 * EXT, T::default());
    for y in 0..h {
        let row = &mut out[y * w..(y + 1) * w];
        load(|k| row[k], w, i0, scratch);
        filter(&mut scratch[..w + 2 * EXT], i0, i1);
        row.copy_from_slice(&scratch[EXT..EXT + w]);
    }
    // VER_SR (F.3.5) : chaque colonne.
    for x in 0..w {
        load(|k| out[k * w + x], h, j0, scratch);
        filter(&mut scratch[..h + 2 * EXT], j0, j1);
        for (k, v) in scratch[EXT..EXT + h].iter().enumerate() {
            out[k * w + x] = *v;
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]
mod tests {
    use super::*;

    #[test]
    fn symmetric_extension_reflects_around_edges() {
        // Signal [3, 7) = {3,4,5,6} ; période 6.
        assert_eq!(pse(2, 3, 7), 4);
        assert_eq!(pse(1, 3, 7), 5);
        assert_eq!(pse(7, 3, 7), 5);
        assert_eq!(pse(8, 3, 7), 4);
        assert_eq!(pse(9, 3, 7), 3);
        assert_eq!(pse(10, 3, 7), 4);
        assert_eq!(pse(0, 5, 6), 5);
    }

    /// Analyse 5-3 directe (F.4.8.2, informatif) pour vérifier l'inverse.
    fn analyze_53(x: &[i32], i0: i64) -> Vec<i32> {
        let n = x.len();
        #[allow(clippy::cast_possible_wrap)]
        let i1 = i0 + n as i64;
        if n == 1 {
            return vec![if i0.rem_euclid(2) == 1 {
                x[0] * 2
            } else {
                x[0]
            }];
        }
        let mut buf = vec![0i32; n + 2 * EXT];
        load(|k| x[k], n, i0, &mut buf);
        #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
        let at = |i: i64| (i - i0 + EXT as i64) as usize;
        // Y(2n+1) = X(2n+1) − ⌊(X(2n) + X(2n+2)) / 2⌋ sur les impairs de [i0−1, i1+1).
        let mut i = i0 - 1 + 1 - (i0 - 1).rem_euclid(2);
        while i < i1 + 1 {
            buf[at(i)] -= (buf[at(i - 1)] + buf[at(i + 1)]) >> 1;
            i += 2;
        }
        // Y(2n) = X(2n) + ⌊(Y(2n−1) + Y(2n+1) + 2) / 4⌋ sur les pairs de [i0, i1).
        let mut i = i0 + i0.rem_euclid(2);
        while i < i1 {
            buf[at(i)] += (buf[at(i - 1)] + buf[at(i + 1)] + 2) >> 2;
            i += 2;
        }
        buf[EXT..EXT + n].to_vec()
    }

    #[test]
    fn filter_53_inverts_analysis_for_all_parities_and_lengths() {
        for i0 in 0..4i64 {
            for n in 1..12usize {
                let x: Vec<i32> = (0..n).map(|k| ((k * 37 + 11) % 23) as i32 - 11).collect();
                let y = analyze_53(&x, i0);
                let mut buf = vec![0i32; n + 2 * EXT];
                load(|k| y[k], n, i0, &mut buf);
                #[allow(clippy::cast_possible_wrap)]
                filter_53(&mut buf, i0, i0 + n as i64);
                assert_eq!(&buf[EXT..EXT + n], &x[..], "i0={i0} n={n}");
            }
        }
    }

    #[test]
    fn filter_97_reconstructs_a_constant_from_its_low_band() {
        // Un signal constant c n'a que des coefficients passe-bas égaux à c
        // (gain continu 1) : la synthèse doit rendre c partout.
        let n = 16usize;
        let y: Vec<f32> = (0..n)
            .map(|k| if k % 2 == 0 { 100.0 } else { 0.0 })
            .collect();
        let mut buf = vec![0f32; n + 2 * EXT];
        load(|k| y[k], n, 0, &mut buf);
        #[allow(clippy::cast_possible_wrap)]
        filter_97(&mut buf, 0, n as i64);
        for v in &buf[EXT..EXT + n] {
            assert!((v - 100.0).abs() < 1e-3, "{v}");
        }
    }

    #[test]
    fn filter_97_reconstructs_nyquist_from_its_high_band() {
        // (−1)^i a un passe-haut de −2 aux positions impaires (gain 2).
        let n = 16usize;
        let y: Vec<f32> = (0..n)
            .map(|k| if k % 2 == 1 { -2.0 } else { 0.0 })
            .collect();
        let mut buf = vec![0f32; n + 2 * EXT];
        load(|k| y[k], n, 0, &mut buf);
        #[allow(clippy::cast_possible_wrap)]
        filter_97(&mut buf, 0, n as i64);
        for (k, v) in buf[EXT..EXT + n].iter().enumerate() {
            let expected = if k % 2 == 0 { 1.0 } else { -1.0 };
            assert!((v - expected).abs() < 1e-3, "k={k} {v}");
        }
    }

    #[test]
    fn synthesize_level_interleaves_and_filters_a_constant_image() {
        let res = Rect {
            x0: 1,
            y0: 0,
            x1: 6,
            y1: 5,
        };
        // Largeurs : pairs de [1,6) → x ∈ {2,4} (2), impairs {1,3,5} (3).
        let ll = vec![50i32; 2 * 3];
        let hl = vec![0i32; 3 * 3];
        let lh = vec![0i32; 2 * 2];
        let hh = vec![0i32; 3 * 2];
        let mut out = Vec::new();
        let mut scratch = Vec::new();
        synthesize_level(
            res,
            &Subbands {
                ll: &ll,
                hl: &hl,
                lh: &lh,
                hh: &hh,
            },
            &mut out,
            &mut scratch,
            filter_53,
        );
        assert_eq!(out.len(), 25);
        assert!(out.iter().all(|&v| v == 50), "{out:?}");
    }
}
