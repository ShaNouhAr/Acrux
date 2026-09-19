//! # acrux-graphics
//!
//! Rasteriseur 2D générique, indépendant du format PDF, écrit de zéro à
//! partir des spécifications (ISO 32000-2 §8.4, §8.5, §8.7.4, §11.3 ; PNG
//! ISO/IEC 15948 ; RFC 1950/1951). Il ne dépend que d'`acrux-core` et de la
//! bibliothèque standard, n'utilise pas `unsafe` et ne panique sur aucune
//! entrée (chemin vide, NaN, tailles nulles, coordonnées gigantesques).
//!
//! ## Modules
//!
//! | Module   | Rôle |
//! |----------|------|
//! | [`bitmap`] | [`Bitmap`] RGBA 8 bits prémultiplié et [`Mask`] de couverture |
//! | [`color`]  | [`Color`] flottante, 16 [`BlendMode`] PDF, composition |
//! | [`raster`] | Rasteriseur scanline anti-aliasé, [`FillRule`], [`Rasterizer`] |
//! | [`paint`]  | [`Paint`] : uni, dégradés axial/radial, image, fonction |
//! | [`stroke`] | Contours : largeur, extrémités, joints, tirets, onglet |
//! | [`png`]    | Encodeur PNG : sans compression ([`encode_png`], images de référence) ou compressé et filtré ([`encode_png_with`]) |
//! | [`icc`]    | [`IccProfile`] : profils ICC v2/v4 (matrice/TRC, gris, LUT) → sRGB, [`IccCache`] |
//!
//! ## Conventions
//!
//! - Le rasteriseur travaille en **pixels**, origine en haut à gauche, `y`
//!   vers le bas ; le centre du pixel `(i, j)` est `(i + 0.5, j + 0.5)`.
//!   L'appelant fournit la matrice qui retourne l'axe `y` du PDF.
//! - Les chemins ([`acrux_core::Path`]) sont transformés puis aplatis avec une
//!   tolérance de 0,1 px ([`raster::FLATTEN_TOLERANCE`]).
//! - Le [`Bitmap`] est prémultiplié ; les [`Color`] et les [`Paint`] sont non
//!   prémultipliés.
//! - Un contour se dessine en deux temps : [`stroke_path`] produit un chemin
//!   de remplissage dans l'espace device, que [`fill_path`] remplit en
//!   [`FillRule::NonZero`] avec la matrice identité.
//!
//! ## Exemple
//!
//! ```
//! use acrux_core::{Matrix, Path, Rect};
//! use acrux_graphics::{encode_png, fill_path, Bitmap, BlendMode, Color, FillRule, Paint};
//!
//! let mut page = Bitmap::new_filled(100, 100, Color::WHITE);
//! let mut path = Path::new();
//! path.rect(&Rect::new(10.0, 10.0, 60.0, 40.0));
//! // Repère PDF (origine en bas) vers pixels (origine en haut).
//! let flip = Matrix::new(1.0, 0.0, 0.0, -1.0, 0.0, 100.0);
//! fill_path(
//!     &mut page,
//!     &path,
//!     &flip,
//!     FillRule::NonZero,
//!     &Paint::Solid(Color::rgb(0.2, 0.4, 0.8)),
//!     None,
//!     1.0,
//!     BlendMode::Normal,
//! );
//! let png = encode_png(&page);
//! assert_eq!(&png[1..4], b"PNG");
//! ```
//!
//! Tout backend futur (GPU) devra produire un résultat identique au pixel,
//! vérifié par les tests de référence.

pub mod bitmap;
pub mod color;
pub mod icc;
pub mod paint;
pub mod png;
pub mod raster;
pub mod stroke;

pub use bitmap::{Bitmap, Mask};
pub use color::{blend, composite_pixel, BlendMode, Color};
pub use icc::{IccCache, IccProfile};
pub use paint::{Gradient, ImagePaint, Paint, PaintSampler};
pub use png::{encode_png, encode_png_rgb, encode_png_with, PixelLayout};
pub use raster::{fill_path, path_coverage, FillRule, Rasterizer};
pub use stroke::{stroke_path, LineCap, LineJoin, StrokeStyle};

/// Image de démonstration écrite dans `target/acrux-graphics-demo.png` pour un
/// contrôle visuel : `cargo test -p acrux-graphics demo -- --ignored`.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::too_many_lines)] // scène de démonstration
mod demo {
    use std::rc::Rc;

    use acrux_core::{Matrix, Path, Point, Rect};

    use super::*;

    fn circle(cx: f64, cy: f64, r: f64) -> Path {
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

    #[test]
    #[ignore = "écrit target/acrux-graphics-demo.png pour inspection visuelle"]
    fn write_demo_png() {
        let mut page = Bitmap::new_filled(400, 300, Color::WHITE);
        let mut r = Rasterizer::new();
        let id = Matrix::IDENTITY;
        let fill = |r: &mut Rasterizer, page: &mut Bitmap, p: &Path, paint: &Paint, rule| {
            r.fill_path(page, p, &id, rule, paint, None, 1.0, BlendMode::Normal);
        };
        // Cercle rouge anti-aliasé, puis anneau even-odd bleu.
        fill(
            &mut r,
            &mut page,
            &circle(70.0, 70.0, 50.0),
            &Paint::Solid(Color::rgb(0.9, 0.2, 0.2)),
            FillRule::NonZero,
        );
        let mut ring = circle(190.0, 70.0, 50.0);
        ring.append(&circle(190.0, 70.0, 25.0));
        fill(
            &mut r,
            &mut page,
            &ring,
            &Paint::Solid(Color::rgba(0.2, 0.3, 0.9, 0.8)),
            FillRule::EvenOdd,
        );
        // Dégradés axial et radial.
        let mut g = Gradient::linear(
            Point::new(260.0, 0.0),
            Point::new(380.0, 0.0),
            vec![(0.0, Color::BLACK), (1.0, Color::rgb(1.0, 0.8, 0.0))],
        );
        g.extend_start = true;
        g.extend_end = true;
        let mut rect = Path::new();
        rect.rect(&Rect::new(260.0, 20.0, 380.0, 60.0));
        fill(
            &mut r,
            &mut page,
            &rect,
            &Paint::LinearGradient(g),
            FillRule::NonZero,
        );
        let mut rg = Gradient::radial(
            Point::new(310.0, 95.0),
            0.0,
            Point::new(320.0, 100.0),
            30.0,
            vec![(0.0, Color::WHITE), (1.0, Color::rgb(0.0, 0.5, 0.2))],
        );
        rg.extend_end = true;
        fill(
            &mut r,
            &mut page,
            &circle(320.0, 100.0, 30.0),
            &Paint::RadialGradient(rg),
            FillRule::NonZero,
        );
        // Traits : joints, extrémités, tirets, trait fin.
        let mut zig = Path::new();
        zig.move_to(Point::new(20.0, 200.0));
        zig.line_to(Point::new(60.0, 150.0));
        zig.line_to(Point::new(100.0, 200.0));
        zig.line_to(Point::new(140.0, 150.0));
        for (i, (join, cap)) in [
            (LineJoin::Miter, LineCap::Butt),
            (LineJoin::Round, LineCap::Round),
            (LineJoin::Bevel, LineCap::Square),
        ]
        .into_iter()
        .enumerate()
        {
            let style = StrokeStyle {
                width: 10.0,
                join,
                cap,
                ..StrokeStyle::default()
            };
            let m = Matrix::translate(0.0, f64::from(u32::try_from(i).unwrap()) * 35.0);
            let outline = stroke_path(&zig, &style, &m);
            fill(
                &mut r,
                &mut page,
                &outline,
                &Paint::Solid(Color::rgba(0.1, 0.1, 0.1, 0.7)),
                FillRule::NonZero,
            );
        }
        let mut line = Path::new();
        line.move_to(Point::new(160.0, 160.0));
        line.curve_to(
            Point::new(200.0, 120.0),
            Point::new(220.0, 240.0),
            Point::new(260.0, 200.0),
        );
        let dashed = StrokeStyle {
            width: 3.0,
            cap: LineCap::Round,
            dash: Some((vec![8.0, 6.0], 0.0)),
            ..StrokeStyle::default()
        };
        let outline = stroke_path(&line, &dashed, &id);
        fill(
            &mut r,
            &mut page,
            &outline,
            &Paint::Solid(Color::BLACK),
            FillRule::NonZero,
        );
        let hair = stroke_path(
            &line,
            &StrokeStyle::with_width(0.0),
            &Matrix::translate(0.0, 60.0),
        );
        fill(
            &mut r,
            &mut page,
            &hair,
            &Paint::Solid(Color::BLACK),
            FillRule::NonZero,
        );
        // Image : damier 64×64 agrandi (net) puis réduit (moyenne).
        let mut checker = Bitmap::new(64, 64);
        for y in 0..64 {
            for x in 0..64 {
                let on = ((x / 4) + (y / 4)) % 2 == 0;
                let px = if on {
                    [30, 30, 30, 255]
                } else {
                    [220, 220, 220, 255]
                };
                checker.set_pixel(x, y, px);
            }
        }
        let checker = Rc::new(checker);
        for (transform, interpolate) in [
            (
                Matrix::scale(96.0, 96.0).then(&Matrix::translate(280.0, 150.0)),
                false,
            ),
            (
                Matrix::scale(16.0, 16.0).then(&Matrix::translate(280.0, 260.0)),
                true,
            ),
            (
                Matrix::scale(16.0, 16.0).then(&Matrix::translate(310.0, 260.0)),
                false,
            ),
        ] {
            let paint = Paint::Image(ImagePaint {
                bitmap: Rc::clone(&checker),
                transform,
                interpolate,
            });
            let mut unit = Path::new();
            unit.rect(&Rect::new(0.0, 0.0, 1.0, 1.0));
            r.fill_path(
                &mut page,
                &unit,
                &transform,
                FillRule::NonZero,
                &paint,
                None,
                1.0,
                BlendMode::Normal,
            );
        }
        // Clip circulaire + fusion Multiply.
        let clip = Mask::from_path(&circle(80.0, 260.0, 30.0), &id, FillRule::NonZero, 400, 300);
        let mut band = Path::new();
        band.rect(&Rect::new(40.0, 240.0, 240.0, 280.0));
        r.fill_path(
            &mut page,
            &band,
            &id,
            FillRule::NonZero,
            &Paint::Solid(Color::rgb(0.2, 0.6, 0.9)),
            Some(&clip),
            1.0,
            BlendMode::Multiply,
        );
        let png = encode_png(&page);
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/acrux-graphics-demo.png");
        std::fs::write(&out, png).unwrap();
        println!("image écrite : {}", out.display());
    }
}
