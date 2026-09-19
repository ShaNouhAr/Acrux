//! Couleur RGBA flottante, modes de fusion (ISO 32000-2 §11.3.5) et
//! composition « source over » (§11.3.8).
//!
//! Les couleurs de ce module sont **non prémultipliées** ; le [`Bitmap`]
//! (voir `bitmap.rs`) stocke au contraire des octets prémultipliés. Les
//! conversions se font par [`Color::to_premultiplied_rgba8`] et
//! [`Color::from_premultiplied_rgba8`].
//!
//! [`Bitmap`]: crate::Bitmap

/// Ramène une composante dans `[0, 1]` ; un NaN devient 0.
#[inline]
#[must_use]
pub(crate) fn unit(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

/// Convertit une composante `[0, 1]` en octet, avec arrondi au plus proche.
#[inline]
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // valeur bornée à [0, 255]
pub(crate) fn to_u8(v: f32) -> u8 {
    (unit(v) * 255.0 + 0.5) as u8
}

/// Couleur RGBA en flottants `[0, 1]`, non prémultipliée.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Color {
    /// Rouge.
    pub r: f32,
    /// Vert.
    pub g: f32,
    /// Bleu.
    pub b: f32,
    /// Opacité (1 = opaque).
    pub a: f32,
}

impl Color {
    /// Noir opaque.
    pub const BLACK: Color = Color::rgb(0.0, 0.0, 0.0);
    /// Blanc opaque.
    pub const WHITE: Color = Color::rgb(1.0, 1.0, 1.0);
    /// Transparent (noir d'opacité nulle).
    pub const TRANSPARENT: Color = Color::rgba(0.0, 0.0, 0.0, 0.0);

    /// Couleur opaque.
    #[must_use]
    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    /// Couleur avec opacité.
    #[must_use]
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// Copie dont chaque composante est ramenée dans `[0, 1]` (NaN → 0).
    #[must_use]
    pub fn clamped(self) -> Self {
        Self {
            r: unit(self.r),
            g: unit(self.g),
            b: unit(self.b),
            a: unit(self.a),
        }
    }

    /// Composantes de couleur seules.
    #[must_use]
    pub const fn rgb_components(self) -> [f32; 3] {
        [self.r, self.g, self.b]
    }

    /// Octets RGBA prémultipliés (format du [`Bitmap`](crate::Bitmap)).
    #[must_use]
    pub fn to_premultiplied_rgba8(self) -> [u8; 4] {
        let c = self.clamped();
        [
            to_u8(c.r * c.a),
            to_u8(c.g * c.a),
            to_u8(c.b * c.a),
            to_u8(c.a),
        ]
    }

    /// Octets RGBA non prémultipliés.
    #[must_use]
    pub fn to_rgba8(self) -> [u8; 4] {
        let c = self.clamped();
        [to_u8(c.r), to_u8(c.g), to_u8(c.b), to_u8(c.a)]
    }

    /// Couleur depuis des octets RGBA prémultipliés. Un alpha nul donne
    /// [`Color::TRANSPARENT`].
    #[must_use]
    pub fn from_premultiplied_rgba8(px: [u8; 4]) -> Self {
        if px[3] == 0 {
            return Self::TRANSPARENT;
        }
        let a = f32::from(px[3]) / 255.0;
        let un = |v: u8| unit(f32::from(v) / 255.0 / a);
        Self {
            r: un(px[0]),
            g: un(px[1]),
            b: un(px[2]),
            a,
        }
    }

    /// Couleur depuis des octets RGBA non prémultipliés.
    #[must_use]
    pub fn from_rgba8(px: [u8; 4]) -> Self {
        Self {
            r: f32::from(px[0]) / 255.0,
            g: f32::from(px[1]) / 255.0,
            b: f32::from(px[2]) / 255.0,
            a: f32::from(px[3]) / 255.0,
        }
    }

    /// Interpolation linéaire composante par composante (`t` dans `[0, 1]`).
    #[must_use]
    pub fn lerp(self, other: Color, t: f32) -> Color {
        let t = unit(t);
        Color {
            r: self.r + (other.r - self.r) * t,
            g: self.g + (other.g - self.g) * t,
            b: self.b + (other.b - self.b) * t,
            a: self.a + (other.a - self.a) * t,
        }
    }
}

/// Modes de fusion de la norme PDF (ISO 32000-2 §11.3.5, tableaux 134 et 135).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    /// `B(cb, cs) = cs`.
    #[default]
    Normal,
    /// `cb × cs`.
    Multiply,
    /// `cb + cs − cb × cs`.
    Screen,
    /// `HardLight(cs, cb)`.
    Overlay,
    /// `min(cb, cs)`.
    Darken,
    /// `max(cb, cs)`.
    Lighten,
    /// Éclaircit le fond pour refléter la source.
    ColorDodge,
    /// Assombrit le fond pour refléter la source.
    ColorBurn,
    /// Multiply ou Screen selon la source.
    HardLight,
    /// Assombrit ou éclaircit doucement selon la source.
    SoftLight,
    /// `|cb − cs|`.
    Difference,
    /// `cb + cs − 2 × cb × cs`.
    Exclusion,
    /// Teinte de la source, saturation et luminosité du fond.
    Hue,
    /// Saturation de la source, teinte et luminosité du fond.
    Saturation,
    /// Teinte et saturation de la source, luminosité du fond.
    Color,
    /// Luminosité de la source, teinte et saturation du fond.
    Luminosity,
}

impl BlendMode {
    /// Vrai pour les modes séparables (appliqués composante par composante).
    #[must_use]
    pub const fn is_separable(self) -> bool {
        !matches!(
            self,
            BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity
        )
    }

    /// Nom PDF du mode (`/Multiply`, …), sans la barre oblique.
    #[must_use]
    pub const fn pdf_name(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Multiply => "Multiply",
            BlendMode::Screen => "Screen",
            BlendMode::Overlay => "Overlay",
            BlendMode::Darken => "Darken",
            BlendMode::Lighten => "Lighten",
            BlendMode::ColorDodge => "ColorDodge",
            BlendMode::ColorBurn => "ColorBurn",
            BlendMode::HardLight => "HardLight",
            BlendMode::SoftLight => "SoftLight",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
            BlendMode::Hue => "Hue",
            BlendMode::Saturation => "Saturation",
            BlendMode::Color => "Color",
            BlendMode::Luminosity => "Luminosity",
        }
    }

    /// Mode depuis son nom PDF (`Compatible` est traité comme `Normal`, §11.3.5.2).
    #[must_use]
    pub fn from_pdf_name(name: &str) -> Option<Self> {
        Some(match name {
            "Normal" | "Compatible" => BlendMode::Normal,
            "Multiply" => BlendMode::Multiply,
            "Screen" => BlendMode::Screen,
            "Overlay" => BlendMode::Overlay,
            "Darken" => BlendMode::Darken,
            "Lighten" => BlendMode::Lighten,
            "ColorDodge" => BlendMode::ColorDodge,
            "ColorBurn" => BlendMode::ColorBurn,
            "HardLight" => BlendMode::HardLight,
            "SoftLight" => BlendMode::SoftLight,
            "Difference" => BlendMode::Difference,
            "Exclusion" => BlendMode::Exclusion,
            "Hue" => BlendMode::Hue,
            "Saturation" => BlendMode::Saturation,
            "Color" => BlendMode::Color,
            "Luminosity" => BlendMode::Luminosity,
            _ => return None,
        })
    }
}

/// Fonction de fusion séparable `B(cb, cs)` pour une composante (§11.3.5.2).
#[must_use]
fn blend_separable(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    match mode {
        // Les modes non séparables ne passent jamais ici (voir `blend`).
        BlendMode::Normal
        | BlendMode::Hue
        | BlendMode::Saturation
        | BlendMode::Color
        | BlendMode::Luminosity => cs,
        BlendMode::Multiply => cb * cs,
        BlendMode::Screen => cb + cs - cb * cs,
        BlendMode::Overlay => blend_separable(BlendMode::HardLight, cs, cb),
        BlendMode::Darken => cb.min(cs),
        BlendMode::Lighten => cb.max(cs),
        BlendMode::ColorDodge => {
            if cb <= 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        BlendMode::ColorBurn => {
            if cb >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        }
        BlendMode::HardLight => {
            if cs <= 0.5 {
                blend_separable(BlendMode::Multiply, cb, 2.0 * cs)
            } else {
                blend_separable(BlendMode::Screen, cb, 2.0 * cs - 1.0)
            }
        }
        BlendMode::SoftLight => {
            if cs <= 0.5 {
                cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
            } else {
                let d = if cb <= 0.25 {
                    ((16.0 * cb - 12.0) * cb + 4.0) * cb
                } else {
                    cb.sqrt()
                };
                cb + (2.0 * cs - 1.0) * (d - cb)
            }
        }
        BlendMode::Difference => (cb - cs).abs(),
        BlendMode::Exclusion => cb + cs - 2.0 * cb * cs,
    }
}

/// Luminosité d'une couleur (§11.3.5.3).
#[must_use]
fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

/// Ramène une couleur dans la gamme en préservant la luminosité (§11.3.5.3, ClipColor).
#[must_use]
fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let luminosity = lum(c);
    let lowest = c[0].min(c[1]).min(c[2]);
    let highest = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if lowest < 0.0 {
        let range = luminosity - lowest;
        if range > 0.0 {
            for v in &mut out {
                *v = luminosity + (*v - luminosity) * luminosity / range;
            }
        }
    }
    if highest > 1.0 {
        let range = highest - luminosity;
        if range > 0.0 {
            for v in &mut out {
                *v = luminosity + (*v - luminosity) * (1.0 - luminosity) / range;
            }
        }
    }
    out
}

/// Impose une luminosité à une couleur (§11.3.5.3, SetLum).
#[must_use]
fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

/// Saturation d'une couleur (§11.3.5.3, Sat).
#[must_use]
fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

/// Impose une saturation à une couleur (§11.3.5.3, SetSat).
#[must_use]
fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    // Indices des composantes max, mid et min.
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|&i, &j| c[i].partial_cmp(&c[j]).unwrap_or(std::cmp::Ordering::Equal));
    let (lo, mid, hi) = (idx[0], idx[1], idx[2]);
    let mut out = [0.0f32; 3];
    let range = c[hi] - c[lo];
    if range > 0.0 {
        out[mid] = (c[mid] - c[lo]) * s / range;
        out[hi] = s;
    }
    out
}

/// Fusion de deux couleurs RGB non prémultipliées selon le mode (§11.3.5).
///
/// `backdrop` est la couleur déjà présente, `source` la couleur peinte.
/// Les composantes sont attendues dans `[0, 1]` ; le résultat y est ramené.
#[must_use]
pub fn blend(mode: BlendMode, backdrop: [f32; 3], source: [f32; 3]) -> [f32; 3] {
    let cb = backdrop.map(unit);
    let cs = source.map(unit);
    let out = match mode {
        BlendMode::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        BlendMode::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        BlendMode::Color => set_lum(cs, lum(cb)),
        BlendMode::Luminosity => set_lum(cb, lum(cs)),
        separable => [
            blend_separable(separable, cb[0], cs[0]),
            blend_separable(separable, cb[1], cs[1]),
            blend_separable(separable, cb[2], cs[2]),
        ],
    };
    out.map(unit)
}

/// Compose une couleur source sur un pixel RGBA prémultiplié (§11.3.8,
/// formule de composition de base) :
///
/// ```text
/// αr = αb + αs − αb·αs
/// αr·Cr = (1 − αs)·αb·Cb + αs·((1 − αb)·Cs + αb·B(Cb, Cs))
/// ```
///
/// `source` est non prémultipliée ; `alpha` combine l'alpha constant, la
/// couverture anti-aliasée et le masque de clip (`[0, 1]`). Avec le mode
/// `Normal` c'est exactement « source over ».
pub fn composite_pixel(dst: &mut [u8; 4], source: Color, alpha: f32, mode: BlendMode) {
    let src = source.clamped();
    let a_s = unit(src.a * unit(alpha));
    if a_s <= 0.0 {
        return;
    }
    let a_b = f32::from(dst[3]) / 255.0;
    let cb_p = [
        f32::from(dst[0]) / 255.0,
        f32::from(dst[1]) / 255.0,
        f32::from(dst[2]) / 255.0,
    ];
    let cs = src.rgb_components();
    let mixed = if mode == BlendMode::Normal || a_b <= 0.0 {
        cs
    } else {
        // Couleur de fond non prémultipliée pour la fonction de fusion.
        let cb = cb_p.map(|v| unit(v / a_b));
        let b = blend(mode, cb, cs);
        [
            (1.0 - a_b) * cs[0] + a_b * b[0],
            (1.0 - a_b) * cs[1] + a_b * b[1],
            (1.0 - a_b) * cs[2] + a_b * b[2],
        ]
    };
    for i in 0..3 {
        dst[i] = to_u8((1.0 - a_s) * cb_p[i] + a_s * mixed[i]);
    }
    dst[3] = to_u8(a_b + a_s - a_b * a_s);
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn premultiplied_conversion_roundtrip() {
        let c = Color::rgba(1.0, 0.5, 0.0, 0.5);
        let px = c.to_premultiplied_rgba8();
        assert_eq!(px, [128, 64, 0, 128]);
        let back = Color::from_premultiplied_rgba8(px);
        assert!(close(back.r, 1.0));
        assert!((back.g - 0.5).abs() < 0.01);
        assert!(close(back.b, 0.0));
        assert!((back.a - 0.5).abs() < 0.01);
        assert_eq!(
            Color::from_premultiplied_rgba8([10, 20, 30, 0]),
            Color::TRANSPARENT
        );
    }

    #[test]
    fn nan_and_out_of_range_are_clamped() {
        let c = Color::rgba(f32::NAN, 2.0, -1.0, f32::INFINITY).clamped();
        assert_eq!(c, Color::rgba(0.0, 1.0, 0.0, 1.0));
        assert_eq!(
            Color::rgba(f32::NAN, f32::NAN, f32::NAN, f32::NAN).to_rgba8(),
            [0; 4]
        );
    }

    #[test]
    fn multiply_half_by_half_is_quarter() {
        let out = blend(BlendMode::Multiply, [0.5; 3], [0.5; 3]);
        assert!(out.iter().all(|&v| close(v, 0.25)));
    }

    #[test]
    fn separable_modes_match_spec_formulas() {
        let cb = [0.25, 0.5, 0.75];
        let cs = [0.6, 0.6, 0.6];
        let screen = blend(BlendMode::Screen, cb, cs);
        assert!(close(screen[0], 0.25 + 0.6 - 0.15));
        let darken = blend(BlendMode::Darken, cb, cs);
        assert_eq!(darken, [0.25, 0.5, 0.6]);
        let lighten = blend(BlendMode::Lighten, cb, cs);
        assert_eq!(lighten, [0.6, 0.6, 0.75]);
        let diff = blend(BlendMode::Difference, cb, cs);
        assert!(close(diff[0], 0.35) && close(diff[2], 0.15));
        let excl = blend(BlendMode::Exclusion, cb, cs);
        assert!(close(excl[1], 0.5 + 0.6 - 0.6));
        // Overlay(cb, cs) = HardLight(cs, cb).
        assert_eq!(
            blend(BlendMode::Overlay, cb, cs),
            blend(BlendMode::HardLight, cs, cb)
        );
        // ColorDodge et ColorBurn : bornes de la spécification.
        assert_eq!(blend(BlendMode::ColorDodge, [0.0; 3], [0.5; 3]), [0.0; 3]);
        assert_eq!(blend(BlendMode::ColorDodge, [0.5; 3], [1.0; 3]), [1.0; 3]);
        assert_eq!(blend(BlendMode::ColorBurn, [1.0; 3], [0.5; 3]), [1.0; 3]);
        assert_eq!(blend(BlendMode::ColorBurn, [0.5; 3], [0.0; 3]), [0.0; 3]);
        // SoftLight avec cs = 0,5 ne change pas le fond.
        let soft = blend(BlendMode::SoftLight, cb, [0.5; 3]);
        assert!(close(soft[0], 0.25) && close(soft[2], 0.75));
    }

    #[test]
    fn non_separable_modes_preserve_expected_components() {
        let backdrop = [0.2, 0.4, 0.6];
        let source = [0.9, 0.1, 0.3];
        let lum_out = blend(BlendMode::Luminosity, backdrop, source);
        assert!(close(lum(lum_out), lum(source)));
        let col_out = blend(BlendMode::Color, backdrop, source);
        assert!(close(lum(col_out), lum(backdrop)));
        let hue_out = blend(BlendMode::Hue, backdrop, source);
        assert!(close(lum(hue_out), lum(backdrop)));
        assert!((sat(hue_out) - sat(backdrop)).abs() < 1e-4);
        let sat_out = blend(BlendMode::Saturation, backdrop, source);
        assert!(close(lum(sat_out), lum(backdrop)));
        assert!((sat(sat_out) - sat(source)).abs() < 1e-4);
        // Un gris n'a pas de teinte : Hue ne change que la saturation nulle.
        let g = blend(BlendMode::Hue, [0.5; 3], source);
        assert!(g.iter().all(|&v| close(v, 0.5)));
    }

    #[test]
    fn composite_source_over() {
        let mut dst = [0, 0, 0, 0];
        composite_pixel(&mut dst, Color::rgb(1.0, 0.0, 0.0), 1.0, BlendMode::Normal);
        assert_eq!(dst, [255, 0, 0, 255]);
        composite_pixel(&mut dst, Color::rgb(0.0, 0.0, 1.0), 0.5, BlendMode::Normal);
        assert_eq!(dst, [128, 0, 128, 255]);
        let mut dst = [0, 0, 0, 0];
        composite_pixel(&mut dst, Color::WHITE, 0.5, BlendMode::Normal);
        assert_eq!(dst, [128, 128, 128, 128]);
        let mut untouched = [1, 2, 3, 4];
        composite_pixel(&mut untouched, Color::WHITE, 0.0, BlendMode::Multiply);
        assert_eq!(untouched, [1, 2, 3, 4]);
    }

    #[test]
    fn composite_multiply_on_opaque_backdrop() {
        let mut dst = Color::rgb(0.5, 0.5, 0.5).to_premultiplied_rgba8();
        composite_pixel(
            &mut dst,
            Color::rgb(0.5, 0.5, 0.5),
            1.0,
            BlendMode::Multiply,
        );
        assert_eq!(dst, [64, 64, 64, 255]);
        // Sur fond transparent, le mode n'a aucun effet (αb = 0).
        let mut dst = [0; 4];
        composite_pixel(
            &mut dst,
            Color::rgb(0.5, 0.5, 0.5),
            1.0,
            BlendMode::Multiply,
        );
        assert_eq!(dst, [128, 128, 128, 255]);
    }

    #[test]
    fn pdf_names_roundtrip() {
        let all = [
            BlendMode::Normal,
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::Overlay,
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::ColorDodge,
            BlendMode::ColorBurn,
            BlendMode::HardLight,
            BlendMode::SoftLight,
            BlendMode::Difference,
            BlendMode::Exclusion,
            BlendMode::Hue,
            BlendMode::Saturation,
            BlendMode::Color,
            BlendMode::Luminosity,
        ];
        assert_eq!(all.len(), 16);
        for m in all {
            assert_eq!(BlendMode::from_pdf_name(m.pdf_name()), Some(m));
            assert_eq!(
                m.is_separable(),
                all.iter().position(|&x| x == m).unwrap() < 12
            );
        }
        assert_eq!(
            BlendMode::from_pdf_name("Compatible"),
            Some(BlendMode::Normal)
        );
        assert_eq!(BlendMode::from_pdf_name("Inconnu"), None);
    }
}
