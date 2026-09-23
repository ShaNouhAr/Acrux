//! Rendu du texte d'interface : une police système (Segoe UI, sinon Arial ou
//! DejaVu Sans) parsée par `acrux-fonts`, glyphes rasterisés par `acrux-graphics`
//! et mis en cache par (glyphe, taille), composés dans le tampon de la
//! fenêtre.
//!
//! Pas de mise en forme complexe (pas de bidi ni de ligatures) : c'est le
//! texte des barres, boutons et panneaux. Le texte des documents passe par
//! le moteur de rendu, pas par ici.

// Composition de pixels : coordonnées entières signées, dimensions non
// signées et produits 8 bits bornés par construction.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::too_many_arguments
)]

use std::collections::HashMap;
use std::path::PathBuf;

use acrux_core::Matrix;
use acrux_fonts::TrueTypeFont;
use acrux_graphics::{Bitmap, BlendMode, Color, FillRule, Paint, Rasterizer};

use crate::platform::Frame;

/// Glyphe rasterisé : couverture 8 bits, origine au point de base.
struct CachedGlyph {
    width: u32,
    height: u32,
    /// Décalage du coin supérieur gauche par rapport à l'origine du glyphe.
    left: i32,
    top: i32,
    /// Avance en pixels (sous-pixel arrondi à l'affichage).
    advance: f32,
    coverage: Vec<u8>,
}

/// Rendu de texte d'interface avec cache de glyphes.
pub struct TextRenderer {
    font: TrueTypeFont,
    cache: HashMap<(u16, u32), CachedGlyph>,
    raster: Rasterizer,
    /// Hauteur d'ascendante (unités/em → fraction de la taille).
    ascent: f32,
    descent: f32,
}

fn candidates() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(w) = std::env::var("WINDIR") {
        dirs.push(PathBuf::from(w).join("Fonts"));
    }
    dirs.push(PathBuf::from("C:/Windows/Fonts"));
    dirs.push(PathBuf::from("/usr/share/fonts/truetype/dejavu"));
    dirs.push(PathBuf::from("/usr/share/fonts/truetype/liberation"));
    dirs.push(PathBuf::from("/System/Library/Fonts"));
    let names = [
        "segoeui.ttf",
        "arial.ttf",
        "DejaVuSans.ttf",
        "LiberationSans-Regular.ttf",
        "Helvetica.ttc",
    ];
    let mut out = Vec::new();
    for d in &dirs {
        for n in names {
            out.push(d.join(n));
        }
    }
    out
}

impl TextRenderer {
    /// Charge la première police système disponible.
    #[must_use]
    pub fn system() -> Option<Self> {
        for path in candidates() {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if let Ok(font) = TrueTypeFont::parse(&bytes) {
                return Some(Self::from_font(font));
            }
        }
        None
    }

    /// Depuis une police déjà parsée.
    #[must_use]
    pub fn from_font(font: TrueTypeFont) -> Self {
        let upem = f32::from(font.units_per_em().max(1));
        let (ascent, descent) = font.hhea().map_or((0.9, 0.25), |h| {
            (f32::from(h.ascender) / upem, -f32::from(h.descender) / upem)
        });
        Self {
            font,
            cache: HashMap::new(),
            raster: Rasterizer::new(),
            ascent,
            descent,
        }
    }

    /// Hauteur de ligne conseillée pour une taille en pixels.
    #[must_use]
    pub fn line_height(&self, size_px: f32) -> f32 {
        (self.ascent + self.descent) * size_px * 1.15
    }

    /// Ascendante en pixels (distance ligne de base → haut).
    #[must_use]
    pub fn ascent(&self, size_px: f32) -> f32 {
        self.ascent * size_px
    }

    fn glyph(&mut self, gid: u16, size_px: f32) -> Option<&CachedGlyph> {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let key = (gid, (size_px * 4.0).round() as u32);
        if !self.cache.contains_key(&key) {
            let upem = f64::from(self.font.units_per_em().max(1));
            let scale = f64::from(size_px) / upem;
            let advance = self.font.advance(gid).map_or(0.0, |a| f64::from(a) * scale);
            let path = self.font.glyph_path(gid).unwrap_or_default();
            // Espace device : y vers le bas, origine au point de base.
            let m = Matrix::new(scale, 0.0, 0.0, -scale, 0.0, 0.0);
            let dev = path.transform(&m);
            let cached = match dev.bounds() {
                Some(b) if !b.is_empty() => {
                    let left = b.x0.floor();
                    let top = b.y0.floor();
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let (w, h) = (
                        (b.x1.ceil() - left) as u32 + 1,
                        (b.y1.ceil() - top) as u32 + 1,
                    );
                    let mut bitmap = Bitmap::new(w, h);
                    self.raster.fill_path(
                        &mut bitmap,
                        &dev,
                        &Matrix::translate(-left, -top),
                        FillRule::NonZero,
                        &Paint::Solid(Color::WHITE),
                        None,
                        1.0,
                        BlendMode::Normal,
                    );
                    let coverage = bitmap.data().chunks_exact(4).map(|px| px[3]).collect();
                    #[allow(clippy::cast_possible_truncation)]
                    CachedGlyph {
                        width: w,
                        height: h,
                        left: left as i32,
                        top: top as i32,
                        advance: advance as f32,
                        coverage,
                    }
                }
                _ => CachedGlyph {
                    width: 0,
                    height: 0,
                    left: 0,
                    top: 0,
                    #[allow(clippy::cast_possible_truncation)]
                    advance: advance as f32,
                    coverage: Vec::new(),
                },
            };
            self.cache.insert(key, cached);
        }
        self.cache.get(&key)
    }

    fn gid(&self, c: char) -> u16 {
        self.font.unicode_to_gid(c).unwrap_or(0)
    }

    /// Largeur d'un texte en pixels.
    pub fn measure(&mut self, size_px: f32, text: &str) -> f32 {
        let mut x = 0.0;
        for c in text.chars() {
            let gid = self.gid(c);
            if let Some(g) = self.glyph(gid, size_px) {
                x += g.advance;
            }
        }
        x
    }

    /// Dessine `text` avec la ligne de base en `(x, baseline_y)`. Retourne
    /// l'abscisse après le dernier glyphe.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        frame: &mut Frame<'_>,
        x: f32,
        baseline_y: f32,
        size_px: f32,
        text: &str,
        color: (u8, u8, u8),
    ) -> f32 {
        let mut pen = x;
        for c in text.chars() {
            pen = self.draw_char(frame, pen, baseline_y, size_px, c, color);
        }
        pen
    }

    /// Dessine un caractère à la plume `pen` et rend la plume avancée.
    fn draw_char(
        &mut self,
        frame: &mut Frame<'_>,
        pen: f32,
        baseline_y: f32,
        size_px: f32,
        c: char,
        color: (u8, u8, u8),
    ) -> f32 {
        let gid = self.gid(c);
        let Some(g) = self.glyph(gid, size_px) else {
            return pen;
        };
        let (gx, gy) = (
            (pen.round() as i32) + g.left,
            (baseline_y.round() as i32) + g.top,
        );
        blend_coverage(frame, gx, gy, g.width, g.height, &g.coverage, color);
        pen + g.advance
    }

    /// Comme [`TextRenderer::draw_clipped`], mais les caractères dont le
    /// rang figure dans `marks` prennent `mark_color` : ce sont les lettres
    /// qu'une recherche a trouvées, que l'œil doit retrouver d'un coup.
    ///
    /// Les rangs se comptent en caractères, pas en octets (« Première » a
    /// neuf caractères et dix octets). Le « … » d'un texte tronqué garde la
    /// couleur ordinaire. Rien n'est alloué : c'est dessiné à chaque image.
    pub fn draw_marked(
        &mut self,
        frame: &mut Frame<'_>,
        x: f32,
        baseline_y: f32,
        size_px: f32,
        text: &str,
        color: (u8, u8, u8),
        mark_color: (u8, u8, u8),
        marks: &[usize],
        max_width: f32,
    ) -> f32 {
        let fits = self.measure(size_px, text) <= max_width;
        let ellipsis = if fits {
            0.0
        } else {
            self.measure(size_px, "…")
        };
        let mut pen = x;
        for (index, c) in text.chars().enumerate() {
            if !fits {
                let gid = self.gid(c);
                let adv = self.glyph(gid, size_px).map_or(0.0, |g| g.advance);
                if pen - x + adv + ellipsis > max_width {
                    return self.draw_char(frame, pen, baseline_y, size_px, '…', color);
                }
            }
            let tint = if marks.contains(&index) {
                mark_color
            } else {
                color
            };
            pen = self.draw_char(frame, pen, baseline_y, size_px, c, tint);
        }
        pen
    }

    /// Dessine un texte tronqué avec « … » s'il dépasse `max_width`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_clipped(
        &mut self,
        frame: &mut Frame<'_>,
        x: f32,
        baseline_y: f32,
        size_px: f32,
        text: &str,
        color: (u8, u8, u8),
        max_width: f32,
    ) -> f32 {
        if self.measure(size_px, text) <= max_width {
            return self.draw(frame, x, baseline_y, size_px, text, color);
        }
        let ellipsis = self.measure(size_px, "…");
        let mut kept = String::new();
        let mut width = 0.0;
        for c in text.chars() {
            let gid = self.gid(c);
            let adv = self.glyph(gid, size_px).map_or(0.0, |g| g.advance);
            if width + adv + ellipsis > max_width {
                break;
            }
            width += adv;
            kept.push(c);
        }
        kept.push('…');
        self.draw(frame, x, baseline_y, size_px, &kept, color)
    }
}

/// Compose une couverture 8 bits dans le tampon BGRA.
#[allow(clippy::many_single_char_names)] // position, taille, composantes
fn blend_coverage(
    frame: &mut Frame<'_>,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    cov: &[u8],
    color: (u8, u8, u8),
) {
    for row in 0..h {
        let dy = y + i64::from(row) as i32;
        if dy < 0 || dy >= frame.height as i32 {
            continue;
        }
        for col in 0..w {
            let dx = x + col as i32;
            if dx < 0 || dx >= frame.width as i32 {
                continue;
            }
            let a = u32::from(cov[(row * w + col) as usize]);
            if a == 0 {
                continue;
            }
            let i = frame.index(dx as usize, dy as usize);
            let inv = 255 - a;
            let d = &mut frame.pixels[i..i + 4];
            d[0] = ((u32::from(color.2) * a + u32::from(d[0]) * inv) / 255) as u8;
            d[1] = ((u32::from(color.1) * a + u32::from(d[1]) * inv) / 255) as u8;
            d[2] = ((u32::from(color.0) * a + u32::from(d[2]) * inv) / 255) as u8;
            d[3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_something_when_a_system_font_exists() {
        let Some(mut tr) = TextRenderer::system() else {
            return; // machine sans police : rien à tester
        };
        // Une chaîne assez longue pour que la mesure soit parlante.
        let sample = "Acrux lecteur PDF";
        let w = tr.measure(16.0, sample);
        assert!(w > 40.0, "largeur mesurée {w}");
        let mut pixels = vec![0u8; 200 * 40 * 4];
        let mut frame = Frame::new(200, 40, &mut pixels);
        frame.clear(0, 0, 0);
        let end = tr.draw(&mut frame, 4.0, 28.0, 16.0, sample, (255, 255, 255));
        assert!((end - 4.0 - w).abs() < 0.5);
        let lit = frame.pixels.chunks_exact(4).filter(|p| p[1] > 128).count();
        assert!(lit > 100, "pixels allumés : {lit}");
        // Le cache évite de rasteriser deux fois.
        let before = tr.cache.len();
        tr.draw(&mut frame, 4.0, 28.0, 16.0, sample, (255, 255, 255));
        assert_eq!(tr.cache.len(), before);
        let clipped = tr.draw_clipped(
            &mut frame,
            0.0,
            28.0,
            16.0,
            "Un texte beaucoup trop long pour tenir",
            (255, 255, 255),
            60.0,
        );
        assert!(clipped <= 62.0, "{clipped}");
    }

    #[test]
    fn draw_marked_advances_like_draw_and_tints_only_the_marks() {
        let Some(mut tr) = TextRenderer::system() else {
            return; // machine sans police : rien à tester
        };
        let sample = "Première page";
        let mut pixels = vec![0u8; 300 * 40 * 4];
        let mut frame = Frame::new(300, 40, &mut pixels);
        frame.clear(0, 0, 0);
        let plain = tr.draw(&mut frame, 4.0, 28.0, 16.0, sample, (255, 255, 255));
        frame.clear(0, 0, 0);
        // Les huit premiers caractères en rouge pur, le reste en vert pur :
        // « Première » a neuf caractères mais dix octets.
        let marks: Vec<usize> = (0..8).collect();
        let marked = tr.draw_marked(
            &mut frame,
            4.0,
            28.0,
            16.0,
            sample,
            (0, 255, 0),
            (255, 0, 0),
            &marks,
            1000.0,
        );
        assert!((marked - plain).abs() < 0.01, "{marked} contre {plain}");
        // Le tampon est BGRA : l'octet 2 est le rouge, l'octet 1 le vert.
        let red = frame.pixels.chunks_exact(4).filter(|p| p[2] > 128).count();
        let green = frame.pixels.chunks_exact(4).filter(|p| p[1] > 128).count();
        assert!(red > 50 && green > 50, "rouge {red}, vert {green}");
        // Tronqué, il s'arrête avant la largeur permise, « … » compris.
        let clipped = tr.draw_marked(
            &mut frame,
            0.0,
            28.0,
            16.0,
            "Un texte beaucoup trop long pour tenir",
            (255, 255, 255),
            (255, 0, 0),
            &[0, 1],
            60.0,
        );
        assert!(clipped <= 62.0, "{clipped}");
    }
}
