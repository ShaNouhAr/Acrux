//! Image RGBA 8 bits prémultipliée et masque de couverture.
//!
//! Le [`Bitmap`] est la cible de tout le rendu : octets `R, G, B, A`, ligne
//! par ligne depuis le haut, **prémultipliés** par l'alpha (ce qui rend la
//! composition « source over » linéaire). Le [`Mask`] est un plan de
//! couverture 0..255 utilisé pour le clipping (ISO 32000-2 §8.5.4) et les
//! masques souples (§11.6.5).

use acrux_core::{Error, Matrix, Path, Result};

use crate::color::Color;
use crate::raster::{path_coverage, FillRule};

/// Taille maximale d'un bitmap en octets (1 Gio) : au-delà, les tailles
/// demandées sont considérées comme corrompues.
pub const MAX_BITMAP_BYTES: usize = 1 << 30;

/// Calcule `width × height × bytes_per_pixel` sans dépasser [`MAX_BITMAP_BYTES`].
fn checked_size(width: u32, height: u32, bytes_per_pixel: usize) -> Option<usize> {
    let w = usize::try_from(width).ok()?;
    let h = usize::try_from(height).ok()?;
    let size = w.checked_mul(h)?.checked_mul(bytes_per_pixel)?;
    (size <= MAX_BITMAP_BYTES).then_some(size)
}

/// Image RGBA 8 bits prémultipliée, origine en haut à gauche.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl Bitmap {
    /// Bitmap transparent. Une taille impossible (dépassement ou plus de
    /// [`MAX_BITMAP_BYTES`]) donne un bitmap 0×0 ; voir [`Bitmap::try_new`]
    /// pour obtenir une erreur à la place.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self::try_new(width, height).unwrap_or_else(|_| Self::empty())
    }

    /// Bitmap transparent, ou `Error::Unsupported` si la taille est impossible.
    ///
    /// # Errors
    /// Si `width × height × 4` dépasse [`MAX_BITMAP_BYTES`].
    pub fn try_new(width: u32, height: u32) -> Result<Self> {
        let size = checked_size(width, height, 4)
            .ok_or_else(|| Error::Unsupported(format!("bitmap {width}×{height} trop grand")))?;
        Ok(Self {
            width,
            height,
            data: vec![0; size],
        })
    }

    /// Bitmap 0×0.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            width: 0,
            height: 0,
            data: Vec::new(),
        }
    }

    /// Bitmap rempli d'une couleur uniforme.
    #[must_use]
    pub fn new_filled(width: u32, height: u32, color: Color) -> Self {
        let mut b = Self::new(width, height);
        b.fill(color);
        b
    }

    /// Bitmap depuis des octets RGBA prémultipliés (`width × height × 4`).
    ///
    /// # Errors
    /// Si la longueur des données ne correspond pas aux dimensions.
    pub fn from_premultiplied_rgba8(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        let expected = checked_size(width, height, 4)
            .ok_or_else(|| Error::Unsupported(format!("bitmap {width}×{height} trop grand")))?;
        if data.len() != expected {
            return Err(Error::Corrupt(format!(
                "bitmap {width}×{height} : {} octets reçus, {expected} attendus",
                data.len()
            )));
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }

    /// Largeur en pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Hauteur en pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Vrai si le bitmap n'a aucun pixel.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Octets RGBA prémultipliés, ligne par ligne.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Accès en écriture aux octets.
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Longueur d'une ligne en octets.
    #[must_use]
    pub fn stride(&self) -> usize {
        self.width as usize * 4
    }

    /// Indice du premier octet du pixel, `None` hors image.
    #[must_use]
    pub fn pixel_index(&self, x: u32, y: u32) -> Option<usize> {
        (x < self.width && y < self.height)
            .then(|| (y as usize * self.width as usize + x as usize) * 4)
    }

    /// Pixel RGBA prémultiplié, `None` hors image.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        let i = self.pixel_index(x, y)?;
        Some([
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ])
    }

    /// Couleur non prémultipliée d'un pixel, `None` hors image.
    #[must_use]
    pub fn color_at(&self, x: u32, y: u32) -> Option<Color> {
        self.pixel(x, y).map(Color::from_premultiplied_rgba8)
    }

    /// Écrit un pixel RGBA prémultiplié ; ignoré hors image.
    pub fn set_pixel(&mut self, x: u32, y: u32, px: [u8; 4]) {
        if let Some(i) = self.pixel_index(x, y) {
            self.data[i..i + 4].copy_from_slice(&px);
        }
    }

    /// Ligne `y` (octets RGBA), `None` hors image.
    #[must_use]
    pub fn row(&self, y: u32) -> Option<&[u8]> {
        let stride = self.stride();
        (y < self.height).then(|| &self.data[y as usize * stride..(y as usize + 1) * stride])
    }

    /// Ligne `y` en écriture, `None` hors image.
    pub fn row_mut(&mut self, y: u32) -> Option<&mut [u8]> {
        let stride = self.stride();
        (y < self.height).then(|| &mut self.data[y as usize * stride..(y as usize + 1) * stride])
    }

    /// Remplit tout le bitmap d'une couleur.
    pub fn fill(&mut self, color: Color) {
        let px = color.to_premultiplied_rgba8();
        for chunk in self.data.chunks_exact_mut(4) {
            chunk.copy_from_slice(&px);
        }
    }

    /// Copie des octets RGBA prémultipliés dans la ligne `y` à partir de la
    /// colonne `x`. Les pixels qui déborderaient à droite sont ignorés, ainsi
    /// que les octets incomplets en fin de `rgba`.
    pub fn blit_row(&mut self, x: u32, y: u32, rgba: &[u8]) {
        let Some(row) = self.row_mut(y) else {
            return;
        };
        let start = x as usize * 4;
        if start >= row.len() {
            return;
        }
        let pixels = rgba.len() / 4 * 4;
        let n = pixels.min(row.len() - start);
        row[start..start + n].copy_from_slice(&rgba[..n]);
    }

    /// Octets RGB 8 bits (3 par pixel) après composition sur fond blanc.
    #[must_use]
    pub fn to_rgb8_over_white(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.width as usize * self.height as usize * 3);
        for px in self.data.chunks_exact(4) {
            // Prémultiplié sur blanc : C + (1 − α) × 1.
            let inv = 255 - px[3];
            out.push(px[0].saturating_add(inv));
            out.push(px[1].saturating_add(inv));
            out.push(px[2].saturating_add(inv));
        }
        out
    }

    /// Octets RGBA non prémultipliés (4 par pixel).
    #[must_use]
    pub fn to_rgba8_unpremultiplied(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len());
        for px in self.data.chunks_exact(4) {
            out.extend_from_slice(&unpremultiply(px));
        }
        out
    }
}

/// Dé-prémultiplie un pixel RGBA (alpha nul → pixel entièrement nul).
#[must_use]
pub(crate) fn unpremultiply(px: &[u8]) -> [u8; 4] {
    let a = u32::from(px[3]);
    if a == 0 {
        return [0; 4];
    }
    // (c × 255 / a) arrondi, borné à 255 si les données sont incohérentes.
    let un = |c: u8| -> u8 {
        let v = (u32::from(c) * 255 + a / 2) / a;
        u8::try_from(v).unwrap_or(255)
    };
    [un(px[0]), un(px[1]), un(px[2]), px[3]]
}

/// Masque de couverture : un octet par pixel, 0 = exclu, 255 = entièrement couvert.
///
/// Le masque retient aussi la **boîte des pixels qui peuvent être non nuls**
/// (`bounds`, en `x0, y0, x1, y1` demi-ouvert). C'est une majoration, jamais
/// une minoration : tout ce qui est en dehors est garanti nul. Les découpes
/// d'un PDF sont presque toujours de petits rectangles dans une grande page,
/// et cette boîte évite de parcourir la page entière à chaque intersection —
/// sur une page riche en découpes, elle fait le gros de la différence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mask {
    width: u32,
    height: u32,
    data: Vec<u8>,
    bounds: (u32, u32, u32, u32),
}

impl Mask {
    /// Masque entièrement vide (tout exclu). Taille impossible → masque 0×0.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        checked_size(width, height, 1).map_or_else(
            || Self {
                width: 0,
                height: 0,
                data: Vec::new(),
                bounds: (0, 0, 0, 0),
            },
            |size| Self {
                width,
                height,
                data: vec![0; size],
                bounds: (0, 0, 0, 0),
            },
        )
    }

    /// Boîte des pixels susceptibles d'être non nuls (`x0, y0, x1, y1`).
    #[must_use]
    pub fn bounds(&self) -> (u32, u32, u32, u32) {
        self.bounds
    }

    /// Déclare une boîte plus large (jamais plus étroite) : utilisé par les
    /// constructions qui remplissent les données à la main.
    pub fn set_bounds(&mut self, bounds: (u32, u32, u32, u32)) {
        self.bounds = (
            bounds.0.min(self.width),
            bounds.1.min(self.height),
            bounds.2.min(self.width),
            bounds.3.min(self.height),
        );
    }

    /// Vrai si le masque n'a aucun pixel couvert.
    #[must_use]
    pub fn is_empty_box(&self) -> bool {
        self.bounds.0 >= self.bounds.2 || self.bounds.1 >= self.bounds.3
    }

    /// Masque entièrement couvert.
    #[must_use]
    pub fn full(width: u32, height: u32) -> Self {
        let mut m = Self::new(width, height);
        m.data.fill(255);
        m.bounds = (0, 0, m.width, m.height);
        m
    }

    /// Masque depuis des octets de couverture (`width × height`).
    ///
    /// # Errors
    /// Si la longueur des données ne correspond pas aux dimensions.
    pub fn from_data(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        let expected = checked_size(width, height, 1)
            .ok_or_else(|| Error::Unsupported(format!("masque {width}×{height} trop grand")))?;
        if data.len() != expected {
            return Err(Error::Corrupt(format!(
                "masque {width}×{height} : {} octets reçus, {expected} attendus",
                data.len()
            )));
        }
        Ok(Self {
            width,
            height,
            data,
            bounds: (0, 0, width, height),
        })
    }

    /// Couverture d'un chemin rempli (voir [`path_coverage`]).
    #[must_use]
    pub fn from_path(
        path: &Path,
        transform: &Matrix,
        rule: FillRule,
        width: u32,
        height: u32,
    ) -> Self {
        path_coverage(path, transform, rule, width, height)
    }

    /// Largeur.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Hauteur.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Octets de couverture, ligne par ligne.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Accès en écriture aux octets de couverture.
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Couverture d'un pixel ; 0 hors masque.
    #[must_use]
    pub fn get(&self, x: u32, y: u32) -> u8 {
        if x < self.width && y < self.height {
            self.data[y as usize * self.width as usize + x as usize]
        } else {
            0
        }
    }

    /// Ligne `y`, `None` hors masque.
    #[must_use]
    pub fn row(&self, y: u32) -> Option<&[u8]> {
        let w = self.width as usize;
        (y < self.height).then(|| &self.data[y as usize * w..(y as usize + 1) * w])
    }

    /// Découpe rectangulaire du masque : la fenêtre `(x, y, width, height)`
    /// vue comme un masque à part entière. Sert aux rendus hors écran limités
    /// à une boîte, qui doivent hériter de la découpe déjà en place.
    #[must_use]
    pub fn sub_mask(&self, x: u32, y: u32, width: u32, height: u32) -> Mask {
        let mut out = Mask::new(width, height);
        if out.width() != width || out.height() != height {
            return out;
        }
        out.bounds = (
            self.bounds.0.saturating_sub(x).min(width),
            self.bounds.1.saturating_sub(y).min(height),
            self.bounds.2.saturating_sub(x).min(width),
            self.bounds.3.saturating_sub(y).min(height),
        );
        let data = out.data_mut();
        for row in 0..height {
            let Some(src) = self.row(y + row) else {
                continue;
            };
            for col in 0..width {
                let Some(v) = src.get((x + col) as usize) else {
                    break;
                };
                data[(row * width + col) as usize] = *v;
            }
        }
        out
    }

    /// Intersection : produit des couvertures pixel à pixel (§8.5.4 : le
    /// clip courant est l'intersection des clips successifs). Si les tailles
    /// diffèrent, le résultat a les dimensions minimales communes.
    #[must_use]
    pub fn intersect(&self, other: &Mask) -> Mask {
        let w = self.width.min(other.width);
        let h = self.height.min(other.height);
        let mut out = Mask::new(w, h);
        // Hors de l'intersection des deux boîtes, l'un des deux masques est
        // nul, donc le produit aussi : le masque neuf est déjà à zéro là.
        let x0 = self.bounds.0.max(other.bounds.0).min(w);
        let y0 = self.bounds.1.max(other.bounds.1).min(h);
        let x1 = self.bounds.2.min(other.bounds.2).min(w);
        let y1 = self.bounds.3.min(other.bounds.3).min(h);
        out.bounds = (x0, y0, x1.max(x0), y1.max(y0));
        if x0 >= x1 || y0 >= y1 {
            return out;
        }
        let dst_w = out.width as usize;
        let (a0, b0) = (x0 as usize, x1 as usize);
        for y in y0..y1 {
            let (Some(a), Some(b)) = (self.row(y), other.row(y)) else {
                break;
            };
            let dst = &mut out.data[y as usize * dst_w + a0..y as usize * dst_w + b0];
            let (sa, sb) = (&a[a0..b0], &b[a0..b0]);
            for (d, (&pa, &pb)) in dst.iter_mut().zip(sa.iter().zip(sb.iter())) {
                let prod = u32::from(pa) * u32::from(pb);
                *d = u8::try_from((prod + 127) / 255).unwrap_or(255);
            }
        }
        out
    }

    /// Inverse la couverture (255 − c) ; utile pour les masques souples.
    #[must_use]
    pub fn inverted(&self) -> Mask {
        Mask {
            width: self.width,
            height: self.height,
            data: self.data.iter().map(|&c| 255 - c).collect(),
            // L'inverse d'un masque borné couvre tout le reste.
            bounds: (0, 0, self.width, self.height),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use acrux_core::Rect;

    #[test]
    fn new_is_transparent_and_sized() {
        let b = Bitmap::new(3, 2);
        assert_eq!(b.width(), 3);
        assert_eq!(b.height(), 2);
        assert_eq!(b.data().len(), 24);
        assert!(b.data().iter().all(|&v| v == 0));
        assert_eq!(b.pixel(2, 1), Some([0; 4]));
        assert_eq!(b.pixel(3, 0), None);
        assert_eq!(b.pixel(0, 2), None);
    }

    #[test]
    fn zero_and_huge_sizes_do_not_panic() {
        let z = Bitmap::new(0, 0);
        assert!(z.is_empty());
        assert_eq!(z.pixel(0, 0), None);
        assert!(z.to_rgb8_over_white().is_empty());
        let mut z = Bitmap::new(0, 5);
        z.set_pixel(0, 0, [1; 4]);
        z.blit_row(0, 0, &[1; 8]);
        z.fill(Color::WHITE);
        let huge = Bitmap::new(u32::MAX, u32::MAX);
        assert!(huge.is_empty());
        assert!(Bitmap::try_new(u32::MAX, 2).is_err());
        assert!(Mask::new(u32::MAX, u32::MAX).data().is_empty());
        assert_eq!(Mask::full(0, 0).get(0, 0), 0);
    }

    #[test]
    fn filled_and_pixel_access() {
        let mut b = Bitmap::new_filled(2, 2, Color::rgba(1.0, 0.0, 0.0, 0.5));
        assert_eq!(b.pixel(1, 1), Some([128, 0, 0, 128]));
        b.set_pixel(0, 0, [0, 255, 0, 255]);
        assert_eq!(b.pixel(0, 0), Some([0, 255, 0, 255]));
        b.set_pixel(9, 9, [7; 4]); // ignoré
        assert_eq!(b.row(0).unwrap().len(), 8);
        assert!(b.row(2).is_none());
        let c = b.color_at(1, 1).unwrap();
        assert!((c.r - 1.0).abs() < 0.01 && (c.a - 0.5).abs() < 0.01);
    }

    #[test]
    fn blit_row_clips_to_the_right_edge() {
        let mut b = Bitmap::new(3, 1);
        b.blit_row(1, 0, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 99]);
        assert_eq!(b.pixel(0, 0), Some([0; 4]));
        assert_eq!(b.pixel(1, 0), Some([1, 2, 3, 4]));
        assert_eq!(b.pixel(2, 0), Some([5, 6, 7, 8]));
        b.blit_row(5, 0, &[1; 4]); // hors image
        b.blit_row(0, 3, &[1; 4]); // hors image
        assert_eq!(b.pixel(0, 0), Some([0; 4]));
    }

    #[test]
    fn over_white_and_unpremultiply() {
        let mut b = Bitmap::new(2, 1);
        b.set_pixel(0, 0, [128, 0, 0, 128]); // rouge à 50 %
        b.set_pixel(1, 0, [0, 0, 0, 0]); // transparent
        assert_eq!(b.to_rgb8_over_white(), vec![255, 127, 127, 255, 255, 255]);
        assert_eq!(
            b.to_rgba8_unpremultiplied(),
            vec![255, 0, 0, 128, 0, 0, 0, 0]
        );
        // Données incohérentes (couleur > alpha) : borné à 255, pas de panique.
        assert_eq!(unpremultiply(&[200, 10, 0, 100]), [255, 26, 0, 100]);
    }

    #[test]
    fn from_data_checks_length() {
        assert!(Bitmap::from_premultiplied_rgba8(2, 2, vec![0; 16]).is_ok());
        assert!(Bitmap::from_premultiplied_rgba8(2, 2, vec![0; 15]).is_err());
        assert!(Mask::from_data(2, 2, vec![0; 4]).is_ok());
        assert!(Mask::from_data(2, 2, vec![0; 5]).is_err());
    }

    #[test]
    fn mask_intersect_multiplies_coverage() {
        let a = Mask::from_data(2, 1, vec![255, 128]).unwrap();
        let b = Mask::from_data(2, 1, vec![128, 128]).unwrap();
        let m = a.intersect(&b);
        assert_eq!(m.data(), &[128, 64]);
        assert_eq!(Mask::full(2, 1).intersect(&a), a);
        // Tailles différentes : dimensions minimales communes.
        let big = Mask::full(4, 3);
        let m = big.intersect(&a);
        assert_eq!((m.width(), m.height()), (2, 1));
        assert_eq!(m.data(), &[255, 128]);
        assert_eq!(a.inverted().data(), &[0, 127]);
        assert_eq!(a.get(5, 5), 0);
    }

    #[test]
    fn mask_from_path_covers_rectangle() {
        let mut p = Path::new();
        p.rect(&Rect::new(1.0, 1.0, 3.0, 3.0));
        let m = Mask::from_path(&p, &Matrix::IDENTITY, FillRule::NonZero, 4, 4);
        assert_eq!(m.get(0, 0), 0);
        assert_eq!(m.get(1, 1), 255);
        assert_eq!(m.get(2, 2), 255);
        assert_eq!(m.get(3, 3), 0);
    }
}
