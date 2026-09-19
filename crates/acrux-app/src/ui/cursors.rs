//! Pointeurs de souris propres à Acrux, dessinés ici puis confiés au
//! système.
//!
//! Comme dans Acrobat, le pointeur dit ce que fera le clic : poser une zone
//! de texte, surligner, poser une note, biffer, déplacer. Le système n'a pas
//! ces formes ; on les dessine donc, comme les icônes, en traits lissés — noir
//! cerné de blanc pour rester lisible sur n'importe quel fond.
//!
//! Les formes sont décrites sur une grille de 32 × 32 et dessinées à la
//! taille demandée par le système (plus grande sur un écran à haute
//! résolution).

// Coordonnées de pixels.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

/// Pointeurs dessinés.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Poser une zone de texte : barre de texte et cadre pointillé.
    AddText,
    /// Surligner : barre de texte et surligneur.
    Highlight,
    /// Poser une note : une note.
    Note,
    /// Biffer : croix de visée et pavé noir.
    Redact,
    /// Déplacer : quatre flèches.
    Move,
}

/// Image d'un pointeur : pixels BGRA (alpha non prémultiplié), côté, et
/// point actif.
#[derive(Debug, Clone)]
pub struct Image {
    /// Pixels BGRA, ligne par ligne, du haut vers le bas.
    pub pixels: Vec<u8>,
    /// Côté en pixels.
    pub size: u32,
    /// Point actif (celui qui clique), en pixels.
    pub hot: (u32, u32),
}

/// Une opération de dessin, sur la grille 32 × 32.
enum Op {
    /// Trait d'une épaisseur donnée, bouts arrondis.
    Line((f32, f32), (f32, f32), f32, [u8; 3]),
    /// Polygone convexe plein.
    Fill(Vec<(f32, f32)>, [u8; 3]),
}

/// Segment, d'un point à l'autre de la grille.
type Segment = ((f32, f32), (f32, f32));

const BLACK: [u8; 3] = [0, 0, 0];
const WHITE: [u8; 3] = [255, 255, 255];

/// Dessine un pointeur à la taille `size`.
#[must_use]
pub fn image(shape: Shape, size: u32) -> Image {
    let size = size.clamp(16, 256);
    let (ops, hot) = describe(shape);
    let k = size as f32 / 32.0;
    let mut rgba = vec![[0.0_f32; 4]; (size * size) as usize];
    for op in &ops {
        paint(&mut rgba, size, k, op);
    }
    let mut pixels = Vec::with_capacity(rgba.len() * 4);
    for [r, g, b, a] in rgba {
        pixels.extend_from_slice(&[
            b.round() as u8,
            g.round() as u8,
            r.round() as u8,
            (a * 255.0).round() as u8,
        ]);
    }
    Image {
        pixels,
        size,
        hot: (
            ((hot.0 * k) as u32).min(size - 1),
            ((hot.1 * k) as u32).min(size - 1),
        ),
    }
}

/// Traits noirs cernés de blanc.
fn outlined(ops: &mut Vec<Op>, lines: &[Segment], width: f32) {
    for &(a, b) in lines {
        ops.push(Op::Line(a, b, width + 2.2, WHITE));
    }
    for &(a, b) in lines {
        ops.push(Op::Line(a, b, width, BLACK));
    }
}

/// Barre de texte (I) centrée en `x`, de `top` à `bottom`.
fn ibeam(ops: &mut Vec<Op>, x: f32, top: f32, bottom: f32) {
    outlined(
        ops,
        &[
            ((x, top + 1.0), (x, bottom - 1.0)),
            ((x - 3.0, top), (x - 0.8, top + 0.8)),
            ((x + 3.0, top), (x + 0.8, top + 0.8)),
            ((x - 3.0, bottom), (x - 0.8, bottom - 0.8)),
            ((x + 3.0, bottom), (x + 0.8, bottom - 0.8)),
        ],
        1.3,
    );
}

/// Opérations et point actif d'un pointeur.
#[allow(clippy::too_many_lines)] // un dessin par pointeur, lus l'un après l'autre
fn describe(shape: Shape) -> (Vec<Op>, (f32, f32)) {
    let mut ops = Vec::new();
    let hot = match shape {
        Shape::AddText => {
            ibeam(&mut ops, 8.0, 3.0, 23.0);
            // Cadre pointillé : la zone qui va naître.
            let (x0, y0, x1, y1) = (14.0_f32, 17.0_f32, 29.0_f32, 29.0_f32);
            let mut dashes = Vec::new();
            let mut x = x0;
            while x < x1 {
                let e = (x + 2.0).min(x1);
                dashes.push(((x, y0), (e, y0)));
                dashes.push(((x, y1), (e, y1)));
                x += 4.0;
            }
            let mut y = y0 + 2.0;
            while y < y1 - 1.0 {
                let e = (y + 2.0).min(y1);
                dashes.push(((x0, y), (x0, e)));
                dashes.push(((x1, y), (x1, e)));
                y += 4.0;
            }
            outlined(&mut ops, &dashes, 1.2);
            (8.0, 13.0)
        }
        Shape::Highlight => {
            ibeam(&mut ops, 8.0, 3.0, 23.0);
            // Surligneur en biais : corps sombre, pointe jaune.
            ops.push(Op::Line((19.0, 29.0), (29.0, 19.0), 7.4, WHITE));
            ops.push(Op::Line((20.0, 28.0), (28.5, 19.5), 5.0, [40, 40, 40]));
            ops.push(Op::Fill(
                vec![(15.0, 30.0), (16.5, 25.0), (20.0, 28.5)],
                [0xF5, 0xC5, 0x18],
            ));
            ops.push(Op::Line((16.8, 26.0), (19.5, 28.7), 1.0, BLACK));
            (8.0, 13.0)
        }
        Shape::Note => {
            // Une note : carré jaune, lignes de texte, pointe de bulle.
            let body = vec![(3.0, 3.0), (25.0, 3.0), (25.0, 20.0), (3.0, 20.0)];
            let tail = vec![(8.0, 19.0), (14.0, 19.0), (6.0, 27.0)];
            let outline = [
                ((3.0, 3.0), (25.0, 3.0)),
                ((25.0, 3.0), (25.0, 20.0)),
                ((25.0, 20.0), (14.0, 20.0)),
                ((14.0, 20.0), (6.0, 27.0)),
                ((6.0, 27.0), (8.0, 20.0)),
                ((8.0, 20.0), (3.0, 20.0)),
                ((3.0, 20.0), (3.0, 3.0)),
            ];
            for &(a, b) in &outline {
                ops.push(Op::Line(a, b, 3.4, WHITE));
            }
            ops.push(Op::Fill(body, [0xFF, 0xD8, 0x4A]));
            ops.push(Op::Fill(tail, [0xFF, 0xD8, 0x4A]));
            for &(a, b) in &outline {
                ops.push(Op::Line(a, b, 1.2, BLACK));
            }
            for y in [8.0, 12.0, 16.0] {
                ops.push(Op::Line((7.0, y), (21.0, y), 1.0, [90, 70, 10]));
            }
            (3.0, 3.0)
        }
        Shape::Redact => {
            outlined(
                &mut ops,
                &[
                    ((12.0, 2.0), (12.0, 9.0)),
                    ((12.0, 15.0), (12.0, 22.0)),
                    ((2.0, 12.0), (9.0, 12.0)),
                    ((15.0, 12.0), (22.0, 12.0)),
                ],
                1.3,
            );
            let pave = vec![(17.0, 20.0), (30.0, 20.0), (30.0, 28.0), (17.0, 28.0)];
            for (a, b) in [
                ((17.0, 20.0), (30.0, 20.0)),
                ((30.0, 20.0), (30.0, 28.0)),
                ((30.0, 28.0), (17.0, 28.0)),
                ((17.0, 28.0), (17.0, 20.0)),
            ] {
                ops.push(Op::Line(a, b, 2.4, WHITE));
            }
            ops.push(Op::Fill(pave, BLACK));
            (12.0, 12.0)
        }
        Shape::Move => {
            let c = 16.0;
            outlined(
                &mut ops,
                &[
                    ((c, 3.0), (c, 29.0)),
                    ((3.0, c), (29.0, c)),
                    ((c, 3.0), (c - 4.0, 7.0)),
                    ((c, 3.0), (c + 4.0, 7.0)),
                    ((c, 29.0), (c - 4.0, 25.0)),
                    ((c, 29.0), (c + 4.0, 25.0)),
                    ((3.0, c), (7.0, c - 4.0)),
                    ((3.0, c), (7.0, c + 4.0)),
                    ((29.0, c), (25.0, c - 4.0)),
                    ((29.0, c), (25.0, c + 4.0)),
                ],
                1.5,
            );
            (c, c)
        }
    };
    (ops, hot)
}

/// Pose une opération sur l'image (RVB 0–255, alpha 0–1, non prémultiplié).
#[allow(clippy::many_single_char_names)] // géométrie de pixel
fn paint(rgba: &mut [[f32; 4]], size: u32, k: f32, op: &Op) {
    for py in 0..size {
        for px in 0..size {
            // Centre du pixel, ramené sur la grille 32 × 32.
            let (x, y) = ((px as f32 + 0.5) / k, (py as f32 + 0.5) / k);
            let (coverage, color) = match op {
                Op::Line(a, b, w, color) => {
                    let d = segment_distance((x, y), *a, *b);
                    // Bord lissé sur un pixel de l'image.
                    (((w / 2.0 - d) * k + 0.5).clamp(0.0, 1.0), color)
                }
                Op::Fill(points, color) => (polygon_coverage(points, px, py, k), color),
            };
            if coverage <= 0.0 {
                continue;
            }
            let dst = &mut rgba[(py * size + px) as usize];
            let a = coverage + dst[3] * (1.0 - coverage);
            if a <= 0.0 {
                continue;
            }
            for c in 0..3 {
                dst[c] = (f32::from(color[c]) * coverage + dst[c] * dst[3] * (1.0 - coverage)) / a;
            }
            dst[3] = a;
        }
    }
}

/// Distance d'un point à un segment.
fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx * dx + dy * dy;
    let t = if len <= f32::EPSILON {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len).clamp(0.0, 1.0)
    };
    (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy)
}

/// Part d'un pixel couverte par un polygone convexe (4 × 4 échantillons).
fn polygon_coverage(points: &[(f32, f32)], px: u32, py: u32, k: f32) -> f32 {
    let mut inside = 0;
    for sy in 0..4 {
        for sx in 0..4 {
            let x = (px as f32 + (sx as f32 + 0.5) / 4.0) / k;
            let y = (py as f32 + (sy as f32 + 0.5) / 4.0) / k;
            if contains(points, (x, y)) {
                inside += 1;
            }
        }
    }
    inside as f32 / 16.0
}

/// Point dans un polygone convexe, quel que soit son sens.
fn contains(points: &[(f32, f32)], p: (f32, f32)) -> bool {
    let n = points.len();
    let (mut pos, mut neg) = (false, false);
    for i in 0..n {
        let (a, b) = (points[i], points[(i + 1) % n]);
        let cross = (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
        if cross > 0.0 {
            pos = true;
        } else if cross < 0.0 {
            neg = true;
        }
    }
    !(pos && neg)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Shape; 5] = [
        Shape::AddText,
        Shape::Highlight,
        Shape::Note,
        Shape::Redact,
        Shape::Move,
    ];

    #[test]
    fn chaque_pointeur_a_une_forme_opaque_et_un_fond_transparent() {
        for shape in ALL {
            let img = image(shape, 32);
            assert_eq!(img.pixels.len(), 32 * 32 * 4);
            let opaque = img.pixels.chunks_exact(4).filter(|p| p[3] > 200).count();
            let clear = img.pixels.chunks_exact(4).filter(|p| p[3] == 0).count();
            assert!(opaque > 30, "{shape:?} : {opaque} pixels opaques");
            assert!(clear > 32 * 32 / 3, "{shape:?} : fond trop plein ({clear})");
        }
    }

    #[test]
    fn le_point_actif_suit_la_taille() {
        let small = image(Shape::Move, 32);
        let big = image(Shape::Move, 64);
        assert_eq!(small.hot, (16, 16));
        assert_eq!(big.hot, (32, 32));
        assert_eq!(big.size, 64);
    }

    #[test]
    fn le_point_actif_de_la_note_est_sur_la_note() {
        let img = image(Shape::Note, 32);
        let (x, y) = img.hot;
        // Le pixel voisin du point actif, vers l'intérieur, est dessiné.
        let i = (((y + 1) * 32 + x + 1) * 4) as usize;
        assert!(img.pixels[i + 3] > 128);
    }
}
