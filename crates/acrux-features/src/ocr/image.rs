//! L'image d'une page, ramenée à ce qui compte pour la reconnaissance :
//! une carte d'encre en noir et blanc.
// Coordonnées et comptages de pixels : les conversions y sont partout, et
// toutes bornées par la taille de l'image.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use acrux_core::{Matrix, Rect};

/// Une image binarisée : `ink[y * width + x]` vrai là où il y a de l'encre.
pub struct Ink {
    /// Largeur en pixels.
    pub width: u32,
    /// Hauteur en pixels.
    pub height: u32,
    /// Encre, ligne par ligne depuis le haut.
    pub ink: Vec<bool>,
    /// Couverture d'encre de 0 à 255, ligne par ligne depuis le haut.
    ///
    /// C'est elle qu'on compare aux modèles : le noir et blanc sert à
    /// découper, mais comparer des formes demande les demi-teintes, sans
    /// quoi un « a » et un « u » se ressemblent trop.
    pub coverage: Vec<u8>,
    /// Couleur moyenne de l'encre (RVB 0–1).
    pub color: [f64; 3],
    /// Couleur moyenne du fond (RVB 0–1).
    pub background: [f64; 3],
}

impl Ink {
    /// Sépare l'encre du fond par le seuil d'Otsu (1979) : celui qui rend les
    /// deux groupes de pixels les plus distincts possible.
    ///
    /// Aucun réglage à donner — c'est l'histogramme de l'image qui décide.
    #[must_use]
    pub fn from_rgb(rgb: &[u8], width: u32, height: u32) -> Self {
        let count = (width as usize) * (height as usize);
        let mut grey = Vec::with_capacity(count);
        for i in 0..count {
            let (r, g, b) = (
                u32::from(rgb.get(i * 3).copied().unwrap_or(255)),
                u32::from(rgb.get(i * 3 + 1).copied().unwrap_or(255)),
                u32::from(rgb.get(i * 3 + 2).copied().unwrap_or(255)),
            );
            // Luminance perçue (Rec. 601), en entiers pour rester exact.
            grey.push(((r * 299 + g * 587 + b * 114) / 1000) as u8);
        }
        let threshold = otsu(&grey);
        let mut ink = vec![false; count];
        let (mut dark, mut light) = ([0_u64; 3], [0_u64; 3]);
        let (mut darks, mut lights) = (0_u64, 0_u64);
        for (i, spot) in ink.iter_mut().enumerate() {
            let is_dark = grey[i] <= threshold;
            *spot = is_dark;
            let (sum, n) = if is_dark {
                (&mut dark, &mut darks)
            } else {
                (&mut light, &mut lights)
            };
            for (c, total) in sum.iter_mut().enumerate() {
                *total += u64::from(rgb.get(i * 3 + c).copied().unwrap_or(255));
            }
            *n += 1;
        }
        // Le texte est le groupe minoritaire : une page est surtout du papier.
        // Si c'est l'inverse, l'image est en négatif et l'encre est claire.
        let inverted = darks > lights;
        if inverted {
            for spot in &mut ink {
                *spot = !*spot;
            }
            std::mem::swap(&mut dark, &mut light);
            std::mem::swap(&mut darks, &mut lights);
        }
        // Couverture : 0 sur le papier, 255 au cœur de l'encre, avec les
        // demi-teintes du lissage entre les deux.
        let (ink_level, paper_level) = levels(&grey, &ink);
        let span = f64::from(paper_level) - f64::from(ink_level);
        let coverage = grey
            .iter()
            .map(|g| {
                if span.abs() < 1.0 {
                    return u8::from(ink_level <= *g) * 255;
                }
                let v = (f64::from(paper_level) - f64::from(*g)) / span;
                {
                    (v.clamp(0.0, 1.0) * 255.0) as u8
                }
            })
            .collect();
        Self {
            width,
            height,
            ink,
            coverage,
            color: average(dark, darks),
            background: average(light, lights),
        }
    }

    /// Couverture d'encre d'un pixel, de 0 à 255.
    #[must_use]
    pub fn cover(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.coverage[(y * self.width + x) as usize]
    }

    /// Vrai si le pixel porte de l'encre.
    #[must_use]
    pub fn at(&self, x: u32, y: u32) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        self.ink[(y * self.width + x) as usize]
    }
}

/// Niveaux du **cœur** de l'encre et du papier.
///
/// Ce sont les extrêmes qu'il faut, non les moyennes : prendre la moyenne
/// des pixels sombres ferait passer pour de l'encre pleine tout le halo du
/// lissage, et les lettres s'épaissiraient d'un pixel de chaque côté — assez
/// pour ne plus ressembler à leur modèle.
fn levels(grey: &[u8], ink: &[bool]) -> (u8, u8) {
    let mut dark: Vec<u8> = Vec::new();
    let mut light: Vec<u8> = Vec::new();
    // Un pixel sur sept suffit à situer les niveaux, et coûte sept fois moins.
    for (i, g) in grey.iter().enumerate().step_by(7) {
        if ink.get(i).copied().unwrap_or(false) {
            dark.push(*g);
        } else {
            light.push(*g);
        }
    }
    dark.sort_unstable();
    light.sort_unstable();
    // Le dixième le plus sombre de l'encre, le dixième le plus clair du
    // papier : les deux bouts de l'échelle, sans se laisser prendre au halo.
    let low = dark.get(dark.len() / 10).copied().unwrap_or(0);
    let high = light
        .get(light.len() - light.len() / 10 - 1)
        .copied()
        .unwrap_or(255);
    (low, high.max(low.saturating_add(1)))
}

/// Moyenne d'une somme de composantes.
fn average(sum: [u64; 3], n: u64) -> [f64; 3] {
    if n == 0 {
        return [0.0, 0.0, 0.0];
    }
    [
        sum[0] as f64 / n as f64 / 255.0,
        sum[1] as f64 / n as f64 / 255.0,
        sum[2] as f64 / n as f64 / 255.0,
    ]
}

/// Seuil d'Otsu : celui qui maximise la variance entre les deux groupes.
fn otsu(grey: &[u8]) -> u8 {
    let mut histogram = [0_u64; 256];
    for g in grey {
        histogram[*g as usize] += 1;
    }
    let total: u64 = histogram.iter().sum();
    if total == 0 {
        return 128;
    }
    let total = total as f64;
    let mut sum_all = 0.0;
    for (value, n) in histogram.iter().enumerate() {
        {
            sum_all += value as f64 * *n as f64;
        }
    }
    let (mut weight_low, mut sum_low) = (0.0_f64, 0.0_f64);
    let (mut best, mut best_value) = (0.0_f64, 128_u8);
    for (value, n) in histogram.iter().enumerate() {
        let n = *n as f64;
        weight_low += n;
        if weight_low == 0.0 {
            continue;
        }
        let weight_high = total - weight_low;
        if weight_high == 0.0 {
            break;
        }
        {
            sum_low += value as f64 * n;
        }
        let mean_low = sum_low / weight_low;
        let mean_high = (sum_all - sum_low) / weight_high;
        let between = weight_low * weight_high * (mean_low - mean_high).powi(2);
        if between > best {
            best = between;
            {
                best_value = value as u8;
            }
        }
    }
    best_value
}

/// Matrice qui mène des pixels de l'image à l'espace de page.
///
/// Une image PDF occupe le carré unité transformé par sa matrice courante ;
/// son pixel (0, 0) est **en haut à gauche** (§8.9.5.2).
#[must_use]
pub fn to_page(ctm: &Matrix, width: u32, height: u32) -> Matrix {
    let (w, h) = (f64::from(width.max(1)), f64::from(height.max(1)));
    Matrix::new(1.0 / w, 0.0, 0.0, -1.0 / h, 0.0, 1.0).then(ctm)
}

/// Boîte d'un rectangle de pixels, en espace de page.
#[must_use]
pub fn page_rect(m: &Matrix, x0: u32, y0: u32, x1: u32, y1: u32) -> Rect {
    let a = m.apply(acrux_core::Point::new(f64::from(x0), f64::from(y0)));
    let b = m.apply(acrux_core::Point::new(f64::from(x1), f64::from(y1)));
    Rect::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Une image de deux tons se sépare exactement en deux.
    #[test]
    fn le_seuil_separe_les_deux_tons() {
        let mut rgb = Vec::new();
        for i in 0..100 {
            let v = if i % 5 == 0 { 20 } else { 240 };
            rgb.extend_from_slice(&[v, v, v]);
        }
        let ink = Ink::from_rgb(&rgb, 10, 10);
        assert_eq!(ink.ink.iter().filter(|b| **b).count(), 20);
        assert!(ink.color[0] < 0.2 && ink.background[0] > 0.8);
    }

    /// Un texte clair sur fond sombre est reconnu comme tel.
    #[test]
    fn une_image_en_negatif_se_retourne() {
        let mut rgb = Vec::new();
        for i in 0..100 {
            let v = if i % 5 == 0 { 240 } else { 20 };
            rgb.extend_from_slice(&[v, v, v]);
        }
        let ink = Ink::from_rgb(&rgb, 10, 10);
        assert_eq!(ink.ink.iter().filter(|b| **b).count(), 20);
        assert!(ink.color[0] > 0.8, "l'encre claire doit rester l'encre");
    }
}
