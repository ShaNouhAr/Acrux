//! Comparaison visuelle : deux pages rendues, puis les pixels.
//!
//! Le but n'est pas de lister des pixels mais des **zones** : un mot déplacé
//! de deux points produit des milliers de pixels différents dont un humain ne
//! veut voir qu'un rectangle. La méthode :
//!
//! 1. les deux images sont aplaties sur blanc (`to_rgb8_over_white`) puis
//!    comparées composante par composante avec une tolérance, qui absorbe
//!    l'anticrénelage et les arrondis de rastérisation ;
//! 2. les pixels différents sont reportés sur une grille de cellules ;
//! 3. les cellules voisines sont réunies (remplissage par diffusion à huit
//!    voisins, après dilatation d'une cellule) et chaque groupe donne un
//!    rectangle ajusté aux pixels réellement différents.
//!
//! Si le résultat reste trop morcelé, la grille est doublée et l'opération
//! recommencée : on finit toujours par un nombre de zones lisible.

use acrux_core::Rect;
use acrux_graphics::Bitmap;

/// Réglages de la comparaison de pixels.
#[derive(Debug, Clone, Copy)]
pub struct VisualOptions {
    /// Échelle de rendu des pages, en pixels par point (1,0 = 72 dpi).
    pub scale: f64,
    /// Écart toléré par composante RVB (0 à 255) : en dessous, les deux pixels
    /// sont réputés identiques. 0 exige l'égalité stricte ; 16 absorbe
    /// confortablement l'anticrénelage.
    pub tolerance: u8,
    /// Côté en pixels des cellules de regroupement.
    pub cell_size: u32,
    /// Nombre de zones au-delà duquel la grille est doublée.
    pub max_regions: usize,
}

impl Default for VisualOptions {
    fn default() -> Self {
        Self {
            scale: 1.5,
            tolerance: 16,
            cell_size: 8,
            max_regions: 64,
        }
    }
}

/// Résultat de la comparaison de deux images.
#[derive(Debug, Clone, Default)]
pub struct PixelComparison {
    /// Zones changées, en **pixels** de l'image comparée (origine en haut à
    /// gauche), fusionnées et triées de haut en bas.
    pub regions: Vec<Rect>,
    /// Nombre de pixels différents.
    pub changed_pixels: u64,
    /// Nombre de pixels comparés (surface de l'union des deux images).
    pub total_pixels: u64,
}

impl PixelComparison {
    /// Part de pixels différents, de 0 à 1.
    #[must_use]
    pub fn changed_ratio(&self) -> f64 {
        if self.total_pixels == 0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)] // surfaces d'images : loin de 2^53
        let ratio = self.changed_pixels as f64 / self.total_pixels as f64;
        ratio
    }

    /// Vrai si aucun pixel ne diffère.
    #[must_use]
    pub fn is_identical(&self) -> bool {
        self.changed_pixels == 0
    }
}

/// Compare deux images et rend les zones qui changent.
///
/// Les images peuvent avoir des tailles différentes (pages de formats
/// différents) : la comparaison porte sur l'union, et tout ce qui n'existe que
/// dans l'une des deux compte comme changé.
#[must_use]
pub fn compare_bitmaps(a: &Bitmap, b: &Bitmap, options: &VisualOptions) -> PixelComparison {
    let width = a.width().max(b.width());
    let height = a.height().max(b.height());
    if width == 0 || height == 0 {
        return PixelComparison::default();
    }
    let (pa, pb) = (a.to_rgb8_over_white(), b.to_rgb8_over_white());
    let mut changed = vec![false; (width as usize) * (height as usize)];
    let mut changed_pixels = 0u64;
    for y in 0..height {
        for x in 0..width {
            let inside_a = x < a.width() && y < a.height();
            let inside_b = x < b.width() && y < b.height();
            let differs = match (inside_a, inside_b) {
                (true, true) => {
                    let ia = (y as usize * a.width() as usize + x as usize) * 3;
                    let ib = (y as usize * b.width() as usize + x as usize) * 3;
                    (0..3).any(|k| {
                        let (va, vb) = (pa.get(ia + k).copied(), pb.get(ib + k).copied());
                        match (va, vb) {
                            (Some(va), Some(vb)) => va.abs_diff(vb) > options.tolerance,
                            _ => true,
                        }
                    })
                }
                // Hors de l'une des deux images : la zone n'existe que d'un côté.
                _ => true,
            };
            if differs {
                changed[y as usize * width as usize + x as usize] = true;
                changed_pixels += 1;
            }
        }
    }
    let mut cell = options.cell_size.max(1);
    let mut regions = group_regions(&changed, width, height, cell);
    // Trop de zones : grille plus grossière, jusqu'à six fois.
    for _ in 0..6 {
        if regions.len() <= options.max_regions.max(1) {
            break;
        }
        cell = cell.saturating_mul(2);
        regions = group_regions(&changed, width, height, cell);
    }
    PixelComparison {
        regions,
        changed_pixels,
        total_pixels: u64::from(width) * u64::from(height),
    }
}

/// Regroupe les pixels changés en rectangles via une grille de cellules.
fn group_regions(changed: &[bool], width: u32, height: u32, cell: u32) -> Vec<Rect> {
    let cols = width.div_ceil(cell) as usize;
    let rows = height.div_ceil(cell) as usize;
    if cols == 0 || rows == 0 {
        return Vec::new();
    }
    // Boîte des pixels changés de chaque cellule (x0, y0, x1, y1 en pixels),
    // `None` si la cellule ne contient aucun pixel changé.
    let mut cells: Vec<Option<[u32; 4]>> = vec![None; cols * rows];
    for y in 0..height {
        for x in 0..width {
            if !changed[y as usize * width as usize + x as usize] {
                continue;
            }
            let index = (y / cell) as usize * cols + (x / cell) as usize;
            match &mut cells[index] {
                Some(b) => {
                    b[0] = b[0].min(x);
                    b[1] = b[1].min(y);
                    b[2] = b[2].max(x + 1);
                    b[3] = b[3].max(y + 1);
                }
                slot => *slot = Some([x, y, x + 1, y + 1]),
            }
        }
    }
    // Dilatation d'une cellule : sert uniquement à relier des cellules
    // voisines mais non adjacentes (un mot et son accent, deux traits fins).
    let mut grown = vec![false; cols * rows];
    for r in 0..rows {
        for c in 0..cols {
            if cells[r * cols + c].is_none() {
                continue;
            }
            for nr in r.saturating_sub(1)..=(r + 1).min(rows - 1) {
                for nc in c.saturating_sub(1)..=(c + 1).min(cols - 1) {
                    grown[nr * cols + nc] = true;
                }
            }
        }
    }
    // Composantes connexes par diffusion (huit voisins), boîte ajustée aux
    // pixels réellement changés (les cellules ajoutées par dilatation ne
    // comptent pas dans la boîte).
    let mut seen = vec![false; cols * rows];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..cols * rows {
        if seen[start] || !grown[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let mut bounds: Option<[u32; 4]> = None;
        while let Some(index) = stack.pop() {
            if let Some(b) = cells[index] {
                bounds = Some(match bounds {
                    None => b,
                    Some(p) => [
                        p[0].min(b[0]),
                        p[1].min(b[1]),
                        p[2].max(b[2]),
                        p[3].max(b[3]),
                    ],
                });
            }
            let (r, c) = (index / cols, index % cols);
            for nr in r.saturating_sub(1)..=(r + 1).min(rows - 1) {
                for nc in c.saturating_sub(1)..=(c + 1).min(cols - 1) {
                    let neighbour = nr * cols + nc;
                    if grown[neighbour] && !seen[neighbour] {
                        seen[neighbour] = true;
                        stack.push(neighbour);
                    }
                }
            }
        }
        if let Some(b) = bounds {
            out.push(Rect::new(
                f64::from(b[0]),
                f64::from(b[1]),
                f64::from(b[2]),
                f64::from(b[3]),
            ));
        }
    }
    out.sort_by(|a, b| {
        (a.y0, a.x0)
            .partial_cmp(&(b.y0, b.x0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::float_cmp)]
mod tests {
    use super::*;
    use acrux_graphics::Color;

    fn white(w: u32, h: u32) -> Bitmap {
        Bitmap::new_filled(w, h, Color::WHITE)
    }

    fn paint(bitmap: &mut Bitmap, rect: (u32, u32, u32, u32), value: u8) {
        for y in rect.1..rect.3 {
            for x in rect.0..rect.2 {
                bitmap.set_pixel(x, y, [value, value, value, 255]);
            }
        }
    }

    #[test]
    fn identical_bitmaps_have_no_region() {
        let a = white(40, 40);
        let b = white(40, 40);
        let d = compare_bitmaps(&a, &b, &VisualOptions::default());
        assert!(d.is_identical());
        assert!(d.regions.is_empty());
        assert_eq!(d.changed_ratio(), 0.0);
        assert_eq!(d.total_pixels, 1600);
    }

    #[test]
    fn tolerance_absorbs_antialiasing() {
        let a = white(10, 10);
        let mut b = white(10, 10);
        paint(&mut b, (0, 0, 10, 10), 245); // écart de 10 sur chaque composante
        assert!(compare_bitmaps(&a, &b, &VisualOptions::default()).is_identical());
        let strict = VisualOptions {
            tolerance: 0,
            ..VisualOptions::default()
        };
        assert_eq!(compare_bitmaps(&a, &b, &strict).changed_pixels, 100);
    }

    #[test]
    fn two_distant_blocks_give_two_merged_rectangles() {
        let a = white(200, 200);
        let mut b = white(200, 200);
        paint(&mut b, (10, 10, 30, 20), 0);
        paint(&mut b, (150, 160, 170, 180), 0);
        let d = compare_bitmaps(&a, &b, &VisualOptions::default());
        assert_eq!(d.regions.len(), 2, "{:?}", d.regions);
        assert_eq!(d.regions[0], Rect::new(10.0, 10.0, 30.0, 20.0));
        assert_eq!(d.regions[1], Rect::new(150.0, 160.0, 170.0, 180.0));
        assert_eq!(d.changed_pixels, 20 * 10 + 20 * 20);
    }

    #[test]
    fn different_sizes_count_the_missing_area_as_changed() {
        let a = white(10, 10);
        let b = white(20, 10);
        let d = compare_bitmaps(&a, &b, &VisualOptions::default());
        assert_eq!(d.total_pixels, 200);
        assert_eq!(
            d.changed_pixels, 100,
            "la moitié droite n'existe que dans b"
        );
        assert_eq!(d.regions.len(), 1);
        assert_eq!(d.regions[0], Rect::new(10.0, 0.0, 20.0, 10.0));
    }

    #[test]
    fn a_coarse_grid_is_used_when_the_result_is_too_fragmented() {
        let a = white(200, 200);
        let mut b = white(200, 200);
        // 100 taches isolées : la grille doit grossir jusqu'à tenir la limite.
        for i in 0..10 {
            for j in 0..10 {
                paint(&mut b, (i * 20, j * 20, i * 20 + 2, j * 20 + 2), 0);
            }
        }
        let options = VisualOptions {
            max_regions: 16,
            ..VisualOptions::default()
        };
        let d = compare_bitmaps(&a, &b, &options);
        assert!(d.regions.len() <= 16, "{} zones", d.regions.len());
        assert_eq!(d.changed_pixels, 400);
    }
}
