//! Icônes vectorielles de l'interface, décrites comme des chemins dans une
//! boîte de 24 × 24 unités (y vers le bas) et rasterisées par `acrux-graphics`
//! à la taille demandée : nettes à toutes les échelles DPI, sans fichier
//! d'image, et colorées par le thème.
//!
//! Les icônes « au trait » sont des lignes médianes converties en contours
//! par [`stroke_path`] ; les autres sont des surfaces pleines.

// Coordonnées d'écran entières et couvertures 8 bits.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    clippy::too_many_arguments
)]

use acrux_core::{Matrix, Path, Point};
use acrux_graphics::{stroke_path, FillRule, LineCap, LineJoin, Rasterizer, StrokeStyle};

use crate::platform::Frame;

/// Icônes disponibles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    /// Dossier (ouvrir).
    Open,
    /// Chevron gauche (page précédente).
    Prev,
    /// Chevron droit (page suivante).
    Next,
    /// Loupe avec un moins.
    ZoomOut,
    /// Loupe avec un plus.
    ZoomIn,
    /// Flèche double entre deux bornes (ajuster à la largeur).
    FitWidth,
    /// Loupe (rechercher).
    Search,
    /// Soleil (thème clair / sombre).
    Theme,
    /// Panneau latéral (rectangle avec une colonne à gauche).
    Sidebar,
    /// Flèche en arc (pivoter).
    Rotate,
    /// Flèche vers un bac (enregistrer).
    Save,
    /// Imprimante.
    Print,
    /// Deux rectangles côte à côte (disposition des pages).
    ViewMode,
    /// Quatre carrés (ouvrir la barre des outils).
    Tools,
    /// Crayon (modifier le texte).
    EditText,
    /// Rectangle à poignées (modifier les objets).
    Objects,
    /// Paraphe sur une ligne (remplir et signer).
    Sign,
    /// Pointe de surligneur sur une bande (surligner).
    Highlight,
    /// Bulle (poser une note).
    Note,
    /// Page avec un plus (insérer des pages).
    PageInsert,
    /// Deux pages superposées (dupliquer).
    PageDuplicate,
    /// Page avec une flèche sortante (extraire).
    PageExtract,
    /// Page avec une croix (supprimer).
    PageDelete,
    /// Bande pleine sur une page (biffure).
    Redact,
    /// Bande pleine et coche (appliquer les biffures).
    RedactApply,
    /// Page et flèche vers la droite (exporter).
    Export,
    /// Trombone (joindre un fichier).
    Attach,
}

/// Contour d'une page, motif commun à beaucoup d'icônes.
fn page(path: &mut Path, x: f64, y: f64, w: f64, h: f64) {
    polyline(
        path,
        &[(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)],
    );
}

/// Rectangle plein.
fn bar(path: &mut Path, x: f64, y: f64, w: f64, h: f64) {
    polyline(
        path,
        &[(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)],
    );
    path.close();
}

/// Constante de Bézier pour un quart de cercle.
const KAPPA: f64 = 0.552_284_75;

fn circle(path: &mut Path, cx: f64, cy: f64, r: f64) {
    let k = KAPPA * r;
    path.move_to(Point::new(cx + r, cy));
    path.curve_to(
        Point::new(cx + r, cy + k),
        Point::new(cx + k, cy + r),
        Point::new(cx, cy + r),
    );
    path.curve_to(
        Point::new(cx - k, cy + r),
        Point::new(cx - r, cy + k),
        Point::new(cx - r, cy),
    );
    path.curve_to(
        Point::new(cx - r, cy - k),
        Point::new(cx - k, cy - r),
        Point::new(cx, cy - r),
    );
    path.curve_to(
        Point::new(cx + k, cy - r),
        Point::new(cx + r, cy - k),
        Point::new(cx + r, cy),
    );
    path.close();
}

fn polyline(path: &mut Path, pts: &[(f64, f64)]) {
    let mut it = pts.iter();
    if let Some(&(x, y)) = it.next() {
        path.move_to(Point::new(x, y));
    }
    for &(x, y) in it {
        path.line_to(Point::new(x, y));
    }
}

fn magnifier(path: &mut Path) {
    circle(path, 10.5, 10.5, 6.0);
    polyline(path, &[(15.0, 15.0), (20.5, 20.5)]);
}

/// Chemin de l'icône dans la boîte 24 × 24 : lignes médianes (à épaissir)
/// et surfaces pleines, retournés séparément.
#[must_use]
#[allow(clippy::too_many_lines)] // une branche par icône
pub fn geometry(icon: Icon) -> (Path, Path) {
    let mut lines = Path::new();
    let mut fills = Path::new();
    match icon {
        Icon::Open => {
            polyline(
                &mut lines,
                &[
                    (3.0, 7.0),
                    (9.0, 7.0),
                    (11.0, 9.5),
                    (21.0, 9.5),
                    (21.0, 19.0),
                    (3.0, 19.0),
                    (3.0, 7.0),
                ],
            );
        }
        Icon::Prev => polyline(&mut lines, &[(14.5, 6.0), (8.5, 12.0), (14.5, 18.0)]),
        Icon::Next => polyline(&mut lines, &[(9.5, 6.0), (15.5, 12.0), (9.5, 18.0)]),
        Icon::ZoomOut => {
            magnifier(&mut lines);
            polyline(&mut lines, &[(7.5, 10.5), (13.5, 10.5)]);
        }
        Icon::ZoomIn => {
            magnifier(&mut lines);
            polyline(&mut lines, &[(7.5, 10.5), (13.5, 10.5)]);
            polyline(&mut lines, &[(10.5, 7.5), (10.5, 13.5)]);
        }
        Icon::FitWidth => {
            polyline(&mut lines, &[(4.0, 5.0), (4.0, 19.0)]);
            polyline(&mut lines, &[(20.0, 5.0), (20.0, 19.0)]);
            polyline(&mut lines, &[(7.0, 12.0), (17.0, 12.0)]);
            polyline(&mut lines, &[(10.0, 9.0), (7.0, 12.0), (10.0, 15.0)]);
            polyline(&mut lines, &[(14.0, 9.0), (17.0, 12.0), (14.0, 15.0)]);
        }
        Icon::Search => magnifier(&mut lines),
        Icon::Rotate => {
            // Arc horaire de 135° à 405° (rayon 7), pointe de flèche à l'arrivée.
            let pts: Vec<(f64, f64)> = (0..=12)
                .map(|i| {
                    let a = (135.0 + 270.0 * f64::from(i) / 12.0).to_radians();
                    (12.0 + 7.0 * a.cos(), 12.5 + 7.0 * a.sin())
                })
                .collect();
            polyline(&mut lines, &pts);
            polyline(&mut lines, &[(13.5, 3.5), (18.0, 7.5), (13.0, 10.5)]);
        }
        Icon::Save => {
            polyline(
                &mut lines,
                &[(4.0, 14.0), (4.0, 19.5), (20.0, 19.5), (20.0, 14.0)],
            );
            polyline(&mut lines, &[(12.0, 4.0), (12.0, 15.0)]);
            polyline(&mut lines, &[(8.0, 11.0), (12.0, 15.0), (16.0, 11.0)]);
        }
        Icon::Print => {
            polyline(
                &mut lines,
                &[(7.0, 9.0), (7.0, 4.0), (17.0, 4.0), (17.0, 9.0)],
            );
            polyline(
                &mut lines,
                &[
                    (7.0, 15.0),
                    (4.0, 15.0),
                    (4.0, 9.0),
                    (20.0, 9.0),
                    (20.0, 15.0),
                    (17.0, 15.0),
                ],
            );
            polyline(
                &mut lines,
                &[
                    (7.0, 12.5),
                    (7.0, 20.0),
                    (17.0, 20.0),
                    (17.0, 12.5),
                    (7.0, 12.5),
                ],
            );
        }
        Icon::Tools => {
            for (x, y) in [(4.5, 4.5), (13.5, 4.5), (4.5, 13.5), (13.5, 13.5)] {
                bar(&mut fills, x, y, 6.0, 6.0);
            }
        }
        Icon::EditText => {
            // Crayon en diagonale, pointe en bas à gauche.
            polyline(
                &mut lines,
                &[
                    (4.5, 19.5),
                    (4.5, 16.0),
                    (16.0, 4.5),
                    (19.5, 8.0),
                    (8.0, 19.5),
                    (4.5, 19.5),
                ],
            );
            polyline(&mut lines, &[(13.5, 7.0), (17.0, 10.5)]);
        }
        Icon::Objects => {
            page(&mut lines, 6.0, 6.5, 12.0, 11.0);
            for (x, y) in [(4.0, 4.5), (16.0, 4.5), (4.0, 15.5), (16.0, 15.5)] {
                bar(&mut fills, x, y, 4.0, 4.0);
            }
        }
        Icon::Sign => {
            // Un paraphe : trois boucles enlevées, puis la ligne de signature.
            polyline(
                &mut lines,
                &[
                    (4.0, 14.5),
                    (7.0, 8.0),
                    (8.5, 14.0),
                    (11.0, 6.5),
                    (12.5, 14.0),
                    (15.0, 9.5),
                    (17.0, 13.5),
                    (20.0, 11.0),
                ],
            );
            polyline(&mut lines, &[(4.0, 19.0), (20.0, 19.0)]);
        }
        Icon::Highlight => {
            polyline(
                &mut lines,
                &[
                    (8.0, 13.0),
                    (15.5, 5.5),
                    (19.0, 9.0),
                    (11.5, 16.5),
                    (8.0, 16.5),
                    (8.0, 13.0),
                ],
            );
            bar(&mut fills, 4.0, 18.5, 16.0, 2.5);
        }
        Icon::Note => {
            polyline(
                &mut lines,
                &[
                    (4.0, 5.0),
                    (20.0, 5.0),
                    (20.0, 15.5),
                    (11.0, 15.5),
                    (7.0, 19.5),
                    (7.0, 15.5),
                    (4.0, 15.5),
                    (4.0, 5.0),
                ],
            );
            polyline(&mut lines, &[(8.0, 9.0), (16.0, 9.0)]);
            polyline(&mut lines, &[(8.0, 12.0), (13.0, 12.0)]);
        }
        Icon::PageInsert => {
            page(&mut lines, 5.0, 3.5, 14.0, 17.0);
            polyline(&mut lines, &[(12.0, 8.0), (12.0, 16.0)]);
            polyline(&mut lines, &[(8.0, 12.0), (16.0, 12.0)]);
        }
        Icon::PageDuplicate => {
            page(&mut lines, 4.0, 3.5, 12.0, 14.0);
            page(&mut lines, 8.0, 6.5, 12.0, 14.0);
        }
        Icon::PageExtract => {
            polyline(
                &mut lines,
                &[
                    (13.0, 3.5),
                    (5.0, 3.5),
                    (5.0, 20.5),
                    (17.0, 20.5),
                    (17.0, 14.0),
                ],
            );
            polyline(&mut lines, &[(11.0, 10.0), (20.5, 10.0)]);
            polyline(&mut lines, &[(17.0, 6.5), (20.5, 10.0), (17.0, 13.5)]);
        }
        Icon::PageDelete => {
            page(&mut lines, 5.0, 3.5, 14.0, 17.0);
            polyline(&mut lines, &[(9.0, 9.0), (15.0, 15.0)]);
            polyline(&mut lines, &[(15.0, 9.0), (9.0, 15.0)]);
        }
        Icon::Redact => {
            page(&mut lines, 5.0, 3.5, 14.0, 17.0);
            bar(&mut fills, 7.5, 9.5, 9.0, 5.0);
        }
        Icon::RedactApply => {
            bar(&mut fills, 3.5, 5.0, 11.0, 5.0);
            polyline(&mut lines, &[(3.5, 14.5), (11.5, 14.5)]);
            polyline(&mut lines, &[(12.5, 17.0), (15.5, 20.0), (21.0, 12.0)]);
        }
        Icon::Export => {
            polyline(
                &mut lines,
                &[
                    (13.0, 3.5),
                    (5.0, 3.5),
                    (5.0, 20.5),
                    (17.0, 20.5),
                    (17.0, 12.0),
                ],
            );
            polyline(&mut lines, &[(12.0, 12.5), (20.5, 4.5)]);
            polyline(&mut lines, &[(14.5, 4.5), (20.5, 4.5), (20.5, 10.5)]);
        }
        Icon::Attach => {
            // Trombone d'un seul trait. Le dessiner à deux traits parallèles,
            // comme un vrai trombone, les ferait se toucher à dix-huit pixels
            // et l'icône deviendrait une tache.
            polyline(
                &mut lines,
                &[
                    (17.5, 7.0),
                    (17.5, 15.5),
                    (16.8, 18.2),
                    (14.0, 19.5),
                    (11.2, 18.2),
                    (10.5, 15.5),
                    (10.5, 7.5),
                    (11.0, 5.5),
                    (12.8, 4.5),
                    (14.6, 5.5),
                    (15.1, 7.5),
                    (15.1, 16.0),
                ],
            );
        }
        Icon::ViewMode => {
            polyline(
                &mut lines,
                &[
                    (3.5, 5.0),
                    (11.0, 5.0),
                    (11.0, 19.0),
                    (3.5, 19.0),
                    (3.5, 5.0),
                ],
            );
            polyline(
                &mut lines,
                &[
                    (13.0, 5.0),
                    (20.5, 5.0),
                    (20.5, 19.0),
                    (13.0, 19.0),
                    (13.0, 5.0),
                ],
            );
        }
        Icon::Sidebar => {
            polyline(
                &mut lines,
                &[
                    (4.0, 5.0),
                    (20.0, 5.0),
                    (20.0, 19.0),
                    (4.0, 19.0),
                    (4.0, 5.0),
                ],
            );
            polyline(&mut lines, &[(9.5, 5.0), (9.5, 19.0)]);
        }
        Icon::Theme => {
            circle(&mut fills, 12.0, 12.0, 4.0);
            for i in 0..8 {
                let a = f64::from(i) * std::f64::consts::FRAC_PI_4;
                let (s, c) = a.sin_cos();
                polyline(
                    &mut lines,
                    &[
                        (12.0 + 6.5 * c, 12.0 + 6.5 * s),
                        (12.0 + 9.0 * c, 12.0 + 9.0 * s),
                    ],
                );
            }
        }
    }
    (lines, fills)
}

/// Contour plein de l'icône (traits épaissis + surfaces), dans la boîte 24 × 24.
#[must_use]
pub fn outline(icon: Icon) -> Path {
    let (lines, fills) = geometry(icon);
    let style = StrokeStyle {
        width: 1.9,
        cap: LineCap::Round,
        join: LineJoin::Round,
        miter_limit: 10.0,
        dash: None,
    };
    let mut out = stroke_path(&lines, &style, &Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0));
    out.append(&fills);
    out
}

/// Dessine l'icône avec son coin supérieur gauche en `(x, y)` et une boîte
/// de `size` pixels de côté.
pub fn draw(
    frame: &mut Frame<'_>,
    raster: &mut Rasterizer,
    icon: Icon,
    x: i32,
    y: i32,
    size: f32,
    color: (u8, u8, u8),
) {
    let px = size.round().max(1.0) as u32;
    let s = f64::from(px) / 24.0;
    let m = Matrix::new(s, 0.0, 0.0, s, 0.0, 0.0);
    let mask = raster.path_coverage(&outline(icon), &m, FillRule::NonZero, px, px);
    let cov = mask.data();
    for row in 0..px {
        let dy = y + row as i32;
        if dy < 0 || dy >= frame.height as i32 {
            continue;
        }
        for col in 0..px {
            let dx = x + col as i32;
            if dx < 0 || dx >= frame.width as i32 {
                continue;
            }
            let a = u32::from(cov[(row * px + col) as usize]);
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
    fn every_icon_covers_some_pixels_inside_the_box() {
        let icons = [
            Icon::Open,
            Icon::Prev,
            Icon::Next,
            Icon::ZoomOut,
            Icon::ZoomIn,
            Icon::FitWidth,
            Icon::Search,
            Icon::Theme,
            Icon::Sidebar,
            Icon::Rotate,
            Icon::Save,
            Icon::Print,
            Icon::ViewMode,
        ];
        let mut raster = Rasterizer::new();
        for icon in icons {
            let m = Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
            let mask = raster.path_coverage(&outline(icon), &m, FillRule::NonZero, 24, 24);
            let covered = mask.data().iter().filter(|&&c| c > 128).count();
            assert!(covered > 20 && covered < 400, "{icon:?} : {covered} pixels");
            let b = outline(icon).bounds().unwrap_or_default();
            assert!(
                b.x0 >= 0.0 && b.y0 >= 0.0 && b.x1 <= 24.0 && b.y1 <= 24.0,
                "{icon:?} déborde"
            );
        }
    }

    #[test]
    fn draw_blends_into_a_frame() {
        let mut pixels = vec![0u8; 32 * 32 * 4];
        let mut frame = Frame::new(32, 32, &mut pixels);
        let mut raster = Rasterizer::new();
        draw(
            &mut frame,
            &mut raster,
            Icon::Search,
            4,
            4,
            24.0,
            (255, 255, 255),
        );
        assert!(pixels.chunks_exact(4).any(|p| p[0] > 200));
    }
}
