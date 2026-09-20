//! Le rendu d'une scène 3D : projection, tampon de profondeur, éclairage.
//!
//! Tout est fait à la main, comme le reste d'Acrux — il n'y a ni carte
//! graphique ni bibliothèque en dessous, seulement des triangles et un tableau
//! de pixels.
//!
//! # Le chemin d'un triangle
//!
//! 1. Ses sommets passent du repère du modèle au repère du monde (la matrice
//!    du nœud), puis au repère de la caméra (l'inverse de la matrice
//!    caméra→monde).
//! 2. Ce qui est derrière la caméra est **coupé** contre le plan proche : un
//!    triangle à cheval devient un ou deux triangles entiers, sans quoi la
//!    projection enverrait ses sommets à l'infini.
//! 3. La projection donne des coordonnées d'écran et une profondeur.
//! 4. Le triangle est rempli ligne par ligne, chaque pixel n'étant écrit que
//!    s'il est **plus près** que ce qui s'y trouve déjà (tampon de
//!    profondeur) : c'est ce qui fait qu'un objet en cache un autre.
//!
//! # L'éclairage
//!
//! Une lampe frontale, placée à la caméra, comme le fait Acrobat par défaut :
//! une face qui regarde l'observateur est pleinement éclairée, une face de
//! profil s'assombrit. On y ajoute une part d'ambiance pour que rien ne soit
//! tout à fait noir, et un reflet spéculaire discret.

// Un rasteriseur passe son temps à changer d'unité : des pixels entiers aux
// coordonnées flottantes et retour. Les conversions sont bornées par la
// taille de l'image et par `clamp`, et les écrire une à une en `try_from`
// noierait la géométrie sous la plomberie.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use acrux_graphics::{Bitmap, Color};

use super::u3d::{Material, Scene};

/// Un point ou un vecteur de l'espace.
type Vec3 = [f32; 3];

/// Ce que la caméra regarde, et comment.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    /// Centre autour duquel la caméra tourne.
    pub target: Vec3,
    /// Distance à ce centre.
    pub distance: f32,
    /// Rotation autour de l'axe vertical, en radians.
    pub yaw: f32,
    /// Élévation, en radians, bornée pour ne jamais passer par les pôles.
    pub pitch: f32,
    /// Ouverture verticale, en degrés ; zéro pour une projection
    /// orthographique.
    pub fov: f32,
    /// Décalage de la vue, en unités du modèle (le « déplacement » à la
    /// souris).
    pub pan: [f32; 2],
    /// Fond de la vue.
    pub background: Color,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            target: [0.0, 0.0, 0.0],
            distance: 10.0,
            // Trois quarts de face : la vue sous laquelle un objet se lit le
            // mieux, et celle que prennent les visionneuses.
            yaw: 0.6,
            pitch: 0.5,
            fov: 30.0,
            pan: [0.0, 0.0],
            background: Color::rgb(0.95, 0.95, 0.96),
        }
    }
}

impl Camera {
    /// Cadre la scène : centre sur elle, et recule assez pour la voir entière.
    #[must_use]
    pub fn framing(scene: &Scene) -> Self {
        let (min, max) = bounds(scene);
        let target = [
            f32::midpoint(min[0], max[0]),
            f32::midpoint(min[1], max[1]),
            f32::midpoint(min[2], max[2]),
        ];
        // Le rayon de la sphère qui contient la boîte : la demi-diagonale.
        let radius = (0..3)
            .map(|i| (max[i] - min[i]) * (max[i] - min[i]) / 4.0)
            .sum::<f32>()
            .sqrt()
            .max(0.001);
        let default = Camera::default();
        // Distance à laquelle cette sphère tient tout juste dans l'ouverture,
        // avec un peu d'air autour.
        let half = (default.fov.to_radians() / 2.0).tan().max(0.01);
        Camera {
            target,
            distance: radius / half * 1.15,
            ..default
        }
    }

    /// Position de l'œil.
    #[must_use]
    pub fn eye(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        [
            self.target[0] + self.distance * cp * sy,
            self.target[1] + self.distance * sp,
            self.target[2] + self.distance * cp * cy,
        ]
    }

    /// Repère de la caméra : droite, haut, avant.
    #[must_use]
    pub fn basis(&self) -> (Vec3, Vec3, Vec3) {
        let eye = self.eye();
        let forward = normalize(sub(self.target, eye));
        let right = normalize(cross(forward, [0.0, 1.0, 0.0]));
        // Près des pôles, le produit vectoriel s'effondre : on garde alors un
        // repère valable plutôt que des NaN.
        let right = if right.iter().any(|v| !v.is_finite()) {
            [1.0, 0.0, 0.0]
        } else {
            right
        };
        let up = cross(right, forward);
        (right, up, forward)
    }
}

/// Boîte englobante de la scène, dans le repère du monde.
fn bounds(scene: &Scene) -> (Vec3, Vec3) {
    let mut min = [f32::MAX; 3];
    let mut max = [f32::MIN; 3];
    let mut seen = false;
    for item in &scene.items {
        for p in &item.mesh.positions {
            let w = transform(&item.transform, *p);
            for i in 0..3 {
                min[i] = min[i].min(w[i]);
                max[i] = max[i].max(w[i]);
            }
            seen = true;
        }
    }
    if !seen {
        return ([-1.0; 3], [1.0; 3]);
    }
    (min, max)
}

/// Applique une matrice 4×4 rangée en colonnes à un point.
fn transform(m: &[f32; 16], p: Vec3) -> Vec3 {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
    ]
}

/// … et à une direction (sans la translation).
fn rotate(m: &[f32; 16], p: Vec3) -> Vec3 {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2],
    ]
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(v: Vec3) -> Vec3 {
    let n = dot(v, v).sqrt();
    if n <= f32::EPSILON {
        return [0.0, 0.0, 1.0];
    }
    [v[0] / n, v[1] / n, v[2] / n]
}

/// Un sommet prêt à être projeté : sa place dans le repère caméra et sa
/// normale.
#[derive(Clone, Copy)]
struct Vertex {
    view: Vec3,
    normal: Vec3,
}

/// Interpole deux sommets, pour couper un triangle.
fn between(a: &Vertex, b: &Vertex, t: f32) -> Vertex {
    let mix = |x: f32, y: f32| x + (y - x) * t;
    Vertex {
        view: [
            mix(a.view[0], b.view[0]),
            mix(a.view[1], b.view[1]),
            mix(a.view[2], b.view[2]),
        ],
        normal: normalize([
            mix(a.normal[0], b.normal[0]),
            mix(a.normal[1], b.normal[1]),
            mix(a.normal[2], b.normal[2]),
        ]),
    }
}

/// Le tampon d'image et son tampon de profondeur.
struct Target {
    bitmap: Bitmap,
    depth: Vec<f32>,
    width: u32,
    height: u32,
}

/// Dessine une scène et rend l'image obtenue.
///
/// L'image est opaque : elle porte son fond, comme l'aire d'un modèle dans
/// Acrobat.
#[must_use]
pub fn render(scene: &Scene, camera: &Camera, width: u32, height: u32) -> Bitmap {
    let (width, height) = (width.max(1), height.max(1));
    let mut target = Target {
        bitmap: Bitmap::new_filled(width, height, camera.background),
        depth: vec![f32::MAX; (width as usize) * (height as usize)],
        width,
        height,
    };
    let (right, up, forward) = camera.basis();
    let eye = camera.eye();
    // La distance sert d'échelle : un déplacement se mesure en fraction de ce
    // qu'on voit, pour rester naturel de près comme de loin.
    let shift = [
        camera.pan[0] * camera.distance,
        camera.pan[1] * camera.distance,
    ];
    // Le facteur de projection : une ouverture étroite grossit.
    let half = (camera.fov.max(1.0).to_radians() / 2.0).tan().max(0.001);
    // L'ouverture s'applique à la plus petite dimension : le modèle tient
    // dans le cadre, qu'il soit large ou haut.
    let scale = width.min(height) as f32 / 2.0 / half;

    for item in &scene.items {
        let mesh = &item.mesh;
        for face in &mesh.faces {
            let material = item
                .materials
                .get(face.shading as usize)
                .copied()
                .unwrap_or_default();
            let mut corners = [Vertex {
                view: [0.0; 3],
                normal: [0.0, 0.0, 1.0],
            }; 3];
            let mut usable = true;
            for (i, corner) in face.corners.iter().enumerate() {
                let Some(p) = mesh.positions.get(corner.position as usize) else {
                    usable = false;
                    break;
                };
                let world = transform(&item.transform, *p);
                let relative = sub(world, eye);
                corners[i] = Vertex {
                    // Repère caméra : x à droite, y en haut, z vers l'avant.
                    view: [
                        dot(relative, right) - shift[0],
                        dot(relative, up) - shift[1],
                        dot(relative, forward),
                    ],
                    normal: mesh
                        .normals
                        .get(corner.normal as usize)
                        .map_or([0.0, 0.0, 1.0], |n| rotate(&item.transform, *n)),
                };
            }
            if !usable {
                continue;
            }
            // Un maillage sans normales : celle de la face fait l'affaire.
            if mesh.normals.is_empty() {
                let n = normalize(cross(
                    sub(corners[1].view, corners[0].view),
                    sub(corners[2].view, corners[0].view),
                ));
                for c in &mut corners {
                    c.normal = n;
                }
            } else {
                for c in &mut corners {
                    c.normal = normalize([
                        dot(c.normal, right),
                        dot(c.normal, up),
                        dot(c.normal, forward),
                    ]);
                }
            }
            for triangle in clip_near(&corners) {
                raster(&mut target, &triangle, &material, scale);
            }
        }
    }
    target.bitmap
}

/// Distance minimale devant la caméra : au-delà, la division projective
/// s'emballe.
const NEAR: f32 = 0.01;

/// Coupe un triangle contre le plan proche.
fn clip_near(corners: &[Vertex; 3]) -> Vec<[Vertex; 3]> {
    let inside: Vec<bool> = corners.iter().map(|c| c.view[2] > NEAR).collect();
    let count = inside.iter().filter(|i| **i).count();
    match count {
        0 => Vec::new(),
        3 => vec![*corners],
        1 => {
            let i = inside.iter().position(|x| *x).unwrap_or(0);
            let (a, b, c) = (corners[i], corners[(i + 1) % 3], corners[(i + 2) % 3]);
            vec![[a, cut(&a, &b), cut(&a, &c)]]
        }
        // Deux sommets devant : le morceau visible est un quadrilatère, donc
        // deux triangles.
        _ => {
            let i = inside.iter().position(|x| !*x).unwrap_or(0);
            let (out, a, b) = (corners[i], corners[(i + 1) % 3], corners[(i + 2) % 3]);
            let (ca, cb) = (cut(&a, &out), cut(&b, &out));
            vec![[a, b, cb], [a, cb, ca]]
        }
    }
}

/// Point où le segment traverse le plan proche.
fn cut(inside: &Vertex, outside: &Vertex) -> Vertex {
    let d = inside.view[2] - outside.view[2];
    let t = if d.abs() < f32::EPSILON {
        0.5
    } else {
        (inside.view[2] - NEAR) / d
    };
    between(inside, outside, t.clamp(0.0, 1.0))
}

/// Remplit un triangle, en respectant le tampon de profondeur.
fn raster(target: &mut Target, triangle: &[Vertex; 3], material: &Material, scale: f32) {
    let half_w = target.width as f32 / 2.0;
    let half_h = target.height as f32 / 2.0;
    // Projection : x et y divisés par la profondeur, l'axe y retourné parce
    // qu'une image se lit du haut vers le bas.
    let project = |v: &Vertex| {
        let z = v.view[2].max(NEAR);
        (
            half_w + v.view[0] * scale / z,
            half_h - v.view[1] * scale / z,
            z,
        )
    };
    let p: Vec<(f32, f32, f32)> = triangle.iter().map(project).collect();
    let area = (p[1].0 - p[0].0) * (p[2].1 - p[0].1) - (p[2].0 - p[0].0) * (p[1].1 - p[0].1);
    if area.abs() < 1e-6 {
        return;
    }
    let min_x = p
        .iter()
        .map(|q| q.0)
        .fold(f32::MAX, f32::min)
        .floor()
        .max(0.0) as u32;
    let max_x = (p.iter().map(|q| q.0).fold(f32::MIN, f32::max).ceil()).min(target.width as f32);
    let min_y = p
        .iter()
        .map(|q| q.1)
        .fold(f32::MAX, f32::min)
        .floor()
        .max(0.0) as u32;
    let max_y = (p.iter().map(|q| q.1).fold(f32::MIN, f32::max).ceil()).min(target.height as f32);
    if max_x <= 0.0 || max_y <= 0.0 {
        return;
    }
    let max_x = max_x as u32;
    let max_y = max_y as u32;

    for y in min_y..max_y {
        for x in min_x..max_x {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // Coordonnées barycentriques : elles disent si le point est dans
            // le triangle, et servent à interpoler.
            let w0 = ((p[1].0 - fx) * (p[2].1 - fy) - (p[2].0 - fx) * (p[1].1 - fy)) / area;
            let w1 = ((p[2].0 - fx) * (p[0].1 - fy) - (p[0].0 - fx) * (p[2].1 - fy)) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let z = w0 * p[0].2 + w1 * p[1].2 + w2 * p[2].2;
            let at = (y as usize) * (target.width as usize) + x as usize;
            let Some(slot) = target.depth.get_mut(at) else {
                continue;
            };
            if z >= *slot {
                continue;
            }
            *slot = z;
            let normal = normalize([
                w0 * triangle[0].normal[0]
                    + w1 * triangle[1].normal[0]
                    + w2 * triangle[2].normal[0],
                w0 * triangle[0].normal[1]
                    + w1 * triangle[1].normal[1]
                    + w2 * triangle[2].normal[1],
                w0 * triangle[0].normal[2]
                    + w1 * triangle[1].normal[2]
                    + w2 * triangle[2].normal[2],
            ]);
            let color = shade(material, normal);
            target.bitmap.set_pixel(x, y, color);
        }
    }
}

/// Couleur d'un pixel : ambiance, lampe frontale, reflet.
fn shade(material: &Material, normal: Vec3) -> [u8; 4] {
    // La lampe est à la caméra : la lumière arrive donc selon -z du repère
    // caméra, et l'éclairement ne dépend que de l'inclinaison de la face.
    let facing = normal[2].abs().clamp(0.0, 1.0);
    let ambient = 0.28;
    let lit = ambient + (1.0 - ambient) * facing;
    // Un reflet étroit et discret : il donne le relief d'une surface sans
    // blanchir la face qu'on regarde de face.
    let specular = facing.powi(32) * 0.12;
    let channel = |c: f32, s: f32| {
        let v = (c * lit + s * specular).clamp(0.0, 1.0);
        (v * 255.0).round() as u8
    };
    [
        channel(material.diffuse[0], material.specular[0]),
        channel(material.diffuse[1], material.specular[1]),
        channel(material.diffuse[2], material.specular[2]),
        255,
    ]
}
