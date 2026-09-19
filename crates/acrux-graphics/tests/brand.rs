//! Génération de la marque du logiciel, dessinée par notre propre rasteriseur.
//!
//! Le logo n'est pas une image que l'on traîne : c'est une figure géométrique,
//! définie ici une fois, et rendue à toutes les tailles utiles. Le même
//! contour sert aux SVG de `branding/` (écrits à la main, mêmes coordonnées)
//! et aux PNG produits ici.
//!
//! Pour régénérer les fichiers :
//! `cargo test -p acrux-graphics --test brand -- --ignored generate`

// Code de génération d'images : les conversions et les comparaisons exactes y
// sont voulues (coordonnées entières d'une grille, couleurs fixées à la main).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp
)]

use std::path::PathBuf;

use acrux_core::{Matrix, Path, Point, Rect};
use acrux_graphics::{encode_png, Bitmap, BlendMode, Color, FillRule, Gradient, Paint, Rasterizer};

/// Côté de la grille de dessin. Toutes les coordonnées ci-dessous sont dans
/// cette grille ; le rendu se fait à l'échelle voulue.
const GRID: f64 = 128.0;

/// Rayon des coins de la tuile.
const RADIUS: f64 = 30.0;

/// Rouge braise, en haut de la tuile.
fn ember() -> Color {
    Color::rgb(0.894, 0.275, 0.184)
}

/// Rouge profond, en bas de la tuile.
fn deep() -> Color {
    Color::rgb(0.706, 0.063, 0.125)
}

/// Aplat de remplacement sous 64 pixels, où le dégradé ne se voit plus.
fn flat_red() -> Color {
    Color::rgb(0.8, 0.169, 0.157)
}

/// Encre du mot-symbole.
fn ink() -> Color {
    Color::rgb(0.09, 0.09, 0.11)
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Polygone fermé à partir d'une liste de points.
fn polygon(points: &[(f64, f64)]) -> Path {
    let mut p = Path::new();
    let Some(first) = points.first() else {
        return p;
    };
    p.move_to(Point::new(first.0, first.1));
    for (x, y) in &points[1..] {
        p.line_to(Point::new(*x, *y));
    }
    p.close();
    p
}

/// Rectangle plein.
fn bar(x0: f64, y0: f64, x1: f64, y1: f64) -> Path {
    let mut p = Path::new();
    p.rect(&Rect::new(x0, y0, x1, y1));
    p
}

/// Carré aux coins arrondis couvrant toute la grille.
#[allow(clippy::many_single_char_names)] // coordonnées et rayons
fn tile() -> Path {
    let k = RADIUS * 0.5523; // approximation de Bézier d'un quart de cercle
    let (a, b) = (0.0, GRID);
    let r = RADIUS;
    let mut p = Path::new();
    p.move_to(Point::new(a + r, a));
    p.line_to(Point::new(b - r, a));
    p.curve_to(
        Point::new(b - r + k, a),
        Point::new(b, a + r - k),
        Point::new(b, a + r),
    );
    p.line_to(Point::new(b, b - r));
    p.curve_to(
        Point::new(b, b - r + k),
        Point::new(b - r + k, b),
        Point::new(b - r, b),
    );
    p.line_to(Point::new(a + r, b));
    p.curve_to(
        Point::new(a + r - k, b),
        Point::new(a, b - r + k),
        Point::new(a, b - r),
    );
    p.line_to(Point::new(a, a + r));
    p.curve_to(
        Point::new(a, a + r - k),
        Point::new(a + r - k, a),
        Point::new(a + r, a),
    );
    p.close();
    p
}

/// Le « A » d'Acrux, dans sa boîte de 68 sur 64, origine en haut à gauche.
///
/// Silhouette pleine, contre-forme triangulaire et ouverture du bas : trois
/// contours remplis en pair-impair. Le sommet n'est pas une pointe mais une
/// **facette coupée en biais**, signature de la marque — la même coupe revient
/// sur les extrémités du mot-symbole.
fn letter_a() -> Path {
    let mut p = polygon(&[(27.0, 6.0), (41.0, 0.0), (68.0, 64.0), (0.0, 64.0)]);
    // Contre-forme : le triangle fermé par la barre.
    p.append(&polygon(&[(35.0, 23.0), (27.0, 40.0), (42.0, 40.0)]));
    // Ouverture entre les jambes, sous la barre.
    p.append(&polygon(&[
        (21.0, 54.0),
        (48.0, 54.0),
        (52.0, 64.0),
        (16.0, 64.0),
    ]));
    p
}

/// Le « A » placé dans la tuile.
fn tile_letter() -> Path {
    letter_a().transform(&Matrix::new(1.0, 0.0, 0.0, 1.0, 30.0, 32.0))
}

/// « C » : barre haute, montant, barre basse, extrémités coupées en biais.
fn letter_c() -> Path {
    polygon(&[
        (0.0, 0.0),
        (60.0, 0.0),
        (50.0, 16.0),
        (16.0, 16.0),
        (16.0, 48.0),
        (50.0, 48.0),
        (60.0, 64.0),
        (0.0, 64.0),
    ])
}

/// « R » : fût, panse et jambe oblique.
fn letter_r() -> Path {
    let mut p = bar(0.0, 0.0, 16.0, 64.0);
    p.append(&polygon(&[
        (16.0, 0.0),
        (48.0, 0.0),
        (58.0, 12.0),
        (58.0, 44.0),
        (42.0, 44.0),
        (42.0, 16.0),
        (16.0, 16.0),
    ]));
    p.append(&bar(16.0, 28.0, 48.0, 44.0));
    p.append(&polygon(&[
        (28.0, 44.0),
        (44.0, 44.0),
        (58.0, 64.0),
        (42.0, 64.0),
    ]));
    p
}

/// « U » : deux montants, une base aux angles coupés.
fn letter_u() -> Path {
    polygon(&[
        (0.0, 0.0),
        (16.0, 0.0),
        (16.0, 48.0),
        (44.0, 48.0),
        (44.0, 0.0),
        (60.0, 0.0),
        (60.0, 54.0),
        (50.0, 64.0),
        (10.0, 64.0),
        (0.0, 54.0),
    ])
}

/// « X » : deux obliques croisées.
fn letter_x() -> Path {
    let mut p = polygon(&[(0.0, 0.0), (16.0, 0.0), (60.0, 64.0), (44.0, 64.0)]);
    p.append(&polygon(&[
        (44.0, 0.0),
        (60.0, 0.0),
        (16.0, 64.0),
        (0.0, 64.0),
    ]));
    p
}

/// Une lettre composée : son contour et la règle qui la remplit.
type Glyph = (Path, FillRule);

/// Une lettre du mot-symbole dans la table de composition : son tracé, sa
/// largeur d'avance et sa règle de remplissage.
type Entry = (fn() -> Path, f64, FillRule);

/// Mot-symbole « ACRUX » : hauteur de capitale 64, graisse 16, les mêmes
/// proportions que la lettre du logo. Aucune police n'est nécessaire, tout est
/// en contours.
///
/// Chaque lettre porte sa règle de remplissage : le « A » a des contre-formes,
/// donc pair-impair ; le « R » et le « X » sont faits de morceaux qui se
/// recouvrent, donc non-nul, sinon les recouvrements se perceraient.
fn wordmark() -> (Vec<Glyph>, f64) {
    let letters: [Entry; 5] = [
        (letter_a, 68.0, FillRule::EvenOdd),
        (letter_c, 60.0, FillRule::NonZero),
        (letter_r, 58.0, FillRule::NonZero),
        (letter_u, 60.0, FillRule::NonZero),
        (letter_x, 60.0, FillRule::NonZero),
    ];
    let tracking = 14.0;
    let mut out = Vec::new();
    let mut x = 0.0;
    for (shape, width, rule) in letters {
        out.push((
            shape().transform(&Matrix::new(1.0, 0.0, 0.0, 1.0, x, 0.0)),
            rule,
        ));
        x += width + tracking;
    }
    (out, x - tracking)
}

/// Dessine le logo à la taille demandée. `flat` remplace le dégradé par un
/// aplat (pour les petites tailles, où le dégradé ne se voit pas et brouille
/// l'anticrénelage).
fn draw(size: u32, flat: bool) -> Bitmap {
    let mut bitmap = Bitmap::new(size, size);
    let mut raster = Rasterizer::new();
    let scale = f64::from(size) / GRID;
    let m = Matrix::new(scale, 0.0, 0.0, scale, 0.0, 0.0);
    let mut paint = if flat {
        Paint::Solid(flat_red())
    } else {
        Paint::LinearGradient(Gradient::linear(
            Point::new(0.0, 0.0),
            Point::new(0.0, GRID),
            vec![(0.0, ember()), (1.0, deep())],
        ))
    };
    if let Paint::LinearGradient(g) = &mut paint {
        g.transform = m;
    }
    raster.fill_path(
        &mut bitmap,
        &tile(),
        &m,
        FillRule::NonZero,
        &paint,
        None,
        1.0,
        BlendMode::Normal,
    );
    raster.fill_path(
        &mut bitmap,
        &tile_letter(),
        &m,
        FillRule::EvenOdd,
        &Paint::Solid(Color::WHITE),
        None,
        1.0,
        BlendMode::Normal,
    );
    bitmap
}

/// Version monochrome : la lettre seule, sans tuile, pour les usages en une
/// encre (tampon, gravure, documentation imprimée).
fn draw_mono(size: u32) -> Bitmap {
    let mut bitmap = Bitmap::new(size, size);
    let mut raster = Rasterizer::new();
    let scale = f64::from(size) / GRID;
    raster.fill_path(
        &mut bitmap,
        &tile_letter(),
        &Matrix::new(scale, 0.0, 0.0, scale, 0.0, 0.0),
        FillRule::EvenOdd,
        &Paint::Solid(ink()),
        None,
        1.0,
        BlendMode::Normal,
    );
    bitmap
}

/// Dessine le mot-symbole sur fond transparent, en une seule encre.
fn draw_wordmark(height: u32, color: Color) -> Bitmap {
    let (letters, width) = wordmark();
    let scale = f64::from(height) / 64.0;
    let pad = 8.0 * scale;
    let mut bitmap = Bitmap::new(
        (width * scale + 2.0 * pad).ceil() as u32,
        (f64::from(height) + 2.0 * pad).ceil() as u32,
    );
    let mut raster = Rasterizer::new();
    let m = Matrix::new(scale, 0.0, 0.0, scale, pad, pad);
    for (path, rule) in &letters {
        raster.fill_path(
            &mut bitmap,
            path,
            &m,
            *rule,
            &Paint::Solid(color),
            None,
            1.0,
            BlendMode::Normal,
        );
    }
    bitmap
}

#[test]
fn the_letter_stays_inside_the_tile() {
    let bounds = tile_letter().bounds().expect("contour non vide");
    assert!(bounds.x0 >= 28.0 && bounds.x1 <= 100.0, "{bounds:?}");
    assert!(bounds.y0 >= 28.0 && bounds.y1 <= 100.0, "{bounds:?}");
}

#[test]
fn the_tile_covers_the_whole_grid() {
    let bounds = tile().bounds().expect("contour non vide");
    assert_eq!(
        (bounds.x0, bounds.y0, bounds.x1, bounds.y1),
        (0.0, 0.0, GRID, GRID)
    );
}

#[test]
fn the_icon_is_white_on_the_left_leg_and_red_in_the_counter() {
    let bitmap = draw(128, true);
    // Milieu de la jambe gauche du « A ».
    let leg = bitmap.pixel(42, 84).unwrap();
    assert!(leg[0] > 240 && leg[1] > 240 && leg[2] > 240, "{leg:?}");
    // Contre-forme triangulaire : le fond doit s'y voir.
    let counter = bitmap.pixel(64, 68).unwrap();
    assert!(
        counter[0] > 150 && counter[1] < 90 && counter[2] < 90,
        "{counter:?}"
    );
    // Coin extérieur : transparent, la tuile est arrondie.
    assert_eq!(bitmap.pixel(1, 1).unwrap()[3], 0);
}

#[test]
fn the_wordmark_has_five_letters_in_a_row() {
    let (letters, width) = wordmark();
    assert_eq!(letters.len(), 5);
    let mut previous = -1.0_f64;
    for (path, _) in &letters {
        let bounds = path.bounds().expect("contour non vide");
        assert_eq!(bounds.y0, 0.0);
        assert_eq!(bounds.y1, 64.0, "toutes les lettres ont la même hauteur");
        assert!(bounds.x0 > previous, "les lettres se suivent sans reculer");
        previous = bounds.x0;
    }
    // Cinq lettres et quatre approches : la largeur ne peut pas être n'importe quoi.
    assert!(width > 300.0 && width < 380.0, "{width}");
}

#[test]
fn the_letters_that_overlap_themselves_are_filled_non_zero() {
    // Le « R » et le « X » sont faits de morceaux qui se recouvrent : rempli
    // en pair-impair, le croisement du X se perce. Ce test garde la règle.
    let (letters, _) = wordmark();
    let rules: Vec<FillRule> = letters.iter().map(|(_, r)| *r).collect();
    assert_eq!(rules[0], FillRule::EvenOdd, "le A a des contre-formes");
    assert_eq!(rules[2], FillRule::NonZero, "le R");
    assert_eq!(rules[4], FillRule::NonZero, "le X");
    // Preuve par le pixel : au centre du X, l'encre doit être pleine.
    let mark = draw_wordmark(64, Color::BLACK);
    let centre = mark.pixel(mark.width() - 38, 40).unwrap();
    assert!(centre[3] > 200, "croisement du X percé : {centre:?}");
}

#[test]
#[ignore = "écrit les fichiers de marque dans branding/"]
fn generate() {
    let dir = repo_root().join("branding");
    std::fs::create_dir_all(&dir).expect("dossier branding");
    for size in [512u32, 256, 128, 64, 48, 32, 24, 16] {
        let bitmap = draw(size, size < 64);
        let png = encode_png(&bitmap);
        std::fs::write(dir.join(format!("acrux-icon-{size}.png")), png).expect("écriture");
    }
    std::fs::write(
        dir.join("acrux-icon-mono-512.png"),
        encode_png(&draw_mono(512)),
    )
    .expect("écriture");
    for (name, color) in [
        ("acrux-mot.png", ink()),
        ("acrux-mot-blanc.png", Color::WHITE),
    ] {
        std::fs::write(dir.join(name), encode_png(&draw_wordmark(96, color))).expect("écriture");
    }
    // Planche de contrôle : toutes les tailles sur blanc, plus le mot-symbole.
    let mut sheet = Bitmap::new_filled(1100, 800, Color::WHITE);
    let mut x = 32.0;
    for size in [512u32, 256, 128] {
        blit(
            &mut sheet,
            &draw(size, false),
            x as u32,
            32 + (512 - size) / 2,
        );
        x += f64::from(size) + 32.0;
    }
    x = 32.0;
    for size in [64u32, 48, 32, 24, 16] {
        blit(
            &mut sheet,
            &draw(size, size < 64),
            x as u32,
            580 + (64 - size) / 2,
        );
        x += f64::from(size) + 32.0;
    }
    blit(&mut sheet, &draw_wordmark(64, ink()), 400, 578);
    std::fs::write(dir.join("acrux-planche.png"), encode_png(&sheet)).expect("écriture");
    println!("marque écrite dans {}", dir.display());
}

/// Recopie `src` dans `dst` à la position donnée, en composant sur le fond.
fn blit(dst: &mut Bitmap, src: &Bitmap, at_x: u32, at_y: u32) {
    for y in 0..src.height() {
        for x in 0..src.width() {
            let Some(px) = src.pixel(x, y) else { continue };
            let (dx, dy) = (at_x + x, at_y + y);
            if dx >= dst.width() || dy >= dst.height() {
                continue;
            }
            let a = f64::from(px[3]) / 255.0;
            let Some(under) = dst.pixel(dx, dy) else {
                continue;
            };
            let mix = |s: u8, d: u8| {
                // `src` est prémultiplié : la composante porte déjà l'alpha.
                (f64::from(s) + f64::from(d) * (1.0 - a))
                    .round()
                    .clamp(0.0, 255.0) as u8
            };
            dst.set_pixel(
                dx,
                dy,
                [
                    mix(px[0], under[0]),
                    mix(px[1], under[1]),
                    mix(px[2], under[2]),
                    255,
                ],
            );
        }
    }
}

#[test]
fn the_grid_is_square() {
    let r = Rect::new(0.0, 0.0, GRID, GRID);
    assert_eq!(r.width(), r.height());
}
