//! Lecture d'un fichier `.ico` et conversion en ressources Windows.
//!
//! Un fichier `.ico` et la ressource `RT_GROUP_ICON` décrivent la même chose —
//! un répertoire d'images — avec une seule différence : dans le fichier, chaque
//! entrée se termine par la position de l'image sur 32 bits ; dans la ressource,
//! par l'identifiant sur 16 bits de la ressource `RT_ICON` correspondante.

use core::fmt;

/// Signature des images PNG, admises dans les icônes depuis Windows Vista.
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];

/// Raison pour laquelle un fichier `.ico` n'a pas pu être lu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Les six premiers octets ne sont pas ceux d'une icône Windows.
    Signature,
    /// Le répertoire ne contient aucune image.
    Empty,
    /// Le fichier s'arrête avant la fin du répertoire.
    Truncated,
    /// Une entrée désigne une image en dehors du fichier.
    Image,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let texte = match self {
            Self::Signature => "ce fichier n'est pas une icône Windows",
            Self::Empty => "l'icône ne contient aucune image",
            Self::Truncated => "le répertoire de l'icône est tronqué",
            Self::Image => "une image de l'icône déborde du fichier",
        };
        f.write_str(texte)
    }
}

impl std::error::Error for Error {}

/// Description d'une image telle qu'elle apparaît dans le répertoire.
struct Meta {
    /// Largeur en pixels, `0` valant 256 (la valeur ne tient pas sur un octet).
    width: u8,
    /// Hauteur en pixels, avec la même convention.
    height: u8,
    /// Nombre de couleurs de la palette, `0` si l'image n'en a pas.
    colors: u8,
    /// Nombre de plans, toujours 1 en pratique.
    planes: u16,
    /// Nombre de bits par pixel.
    bits: u16,
}

/// Répertoire d'icône lu depuis un fichier `.ico`.
pub struct IconGroup<'a> {
    meta: Vec<Meta>,
    images: Vec<&'a [u8]>,
}

impl<'a> IconGroup<'a> {
    /// Lit le répertoire et les images d'un fichier `.ico`.
    ///
    /// # Errors
    ///
    /// Voir [`Error`] : signature absente, répertoire vide ou tronqué, image
    /// désignée en dehors du fichier.
    pub fn parse(ico: &'a [u8]) -> Result<Self, Error> {
        if ico.len() < 6 {
            return Err(Error::Signature);
        }
        if read_u16(ico, 0) != 0 || read_u16(ico, 2) != 1 {
            return Err(Error::Signature);
        }
        let count = usize::from(read_u16(ico, 4));
        if count == 0 {
            return Err(Error::Empty);
        }
        let directory_end = 6 + 16 * count;
        if ico.len() < directory_end {
            return Err(Error::Truncated);
        }

        let mut meta = Vec::with_capacity(count);
        let mut images = Vec::with_capacity(count);
        for index in 0..count {
            let base = 6 + 16 * index;
            let size = read_u32(ico, base + 8) as usize;
            let offset = read_u32(ico, base + 12) as usize;
            let end = offset.checked_add(size).ok_or(Error::Image)?;
            if offset < directory_end || end > ico.len() {
                return Err(Error::Image);
            }
            let image = &ico[offset..end];
            let (planes, bits) = geometry(image)
                .unwrap_or_else(|| (read_u16(ico, base + 4), read_u16(ico, base + 6)));
            meta.push(Meta {
                width: ico[base],
                height: ico[base + 1],
                colors: ico[base + 2],
                planes,
                bits,
            });
            images.push(image);
        }
        Ok(Self { meta, images })
    }

    /// Les images, dans l'ordre du répertoire.
    #[must_use]
    pub fn images(&self) -> &[&'a [u8]] {
        &self.images
    }

    /// Construit la ressource `RT_GROUP_ICON`.
    ///
    /// Les images reçoivent les identifiants `first_id`, `first_id + 1`, etc.,
    /// dans l'ordre où [`Self::images`] les rend.
    #[must_use]
    pub fn directory(&self, first_id: u16) -> Vec<u8> {
        let count = u16::try_from(self.meta.len()).unwrap_or(u16::MAX);
        let mut out = Vec::with_capacity(6 + 14 * self.meta.len());
        out.extend_from_slice(&0u16.to_le_bytes()); // réservé
        out.extend_from_slice(&1u16.to_le_bytes()); // type : icône
        out.extend_from_slice(&count.to_le_bytes());
        for (index, entry) in self.meta.iter().enumerate() {
            let size = u32::try_from(self.images[index].len()).unwrap_or(u32::MAX);
            let id = first_id.saturating_add(u16::try_from(index).unwrap_or(u16::MAX));
            out.push(entry.width);
            out.push(entry.height);
            out.push(entry.colors);
            out.push(0); // réservé
            out.extend_from_slice(&entry.planes.to_le_bytes());
            out.extend_from_slice(&entry.bits.to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&id.to_le_bytes());
        }
        out
    }
}

/// Retrouve le nombre de plans et de bits par pixel dans l'image elle-même.
///
/// Le répertoire du fichier `.ico` ment souvent — beaucoup d'outils y laissent
/// des zéros —, alors que l'image, elle, est toujours exacte. `rc.exe` fait
/// la même relecture.
fn geometry(image: &[u8]) -> Option<(u16, u16)> {
    if image.len() >= 26 && image[..8] == PNG_MAGIC {
        // En-tête IHDR : profondeur à l'octet 24, type de couleur à l'octet 25.
        let depth = u16::from(image[24]);
        let channels = match image[25] {
            0 | 3 => 1, // niveaux de gris, ou palette
            2 => 3,     // RVB
            4 => 2,     // niveaux de gris + alpha
            6 => 4,     // RVB + alpha
            _ => return None,
        };
        return Some((1, depth.saturating_mul(channels)));
    }
    if image.len() >= 16 && read_u32(image, 0) == 40 {
        // BITMAPINFOHEADER : biPlanes puis biBitCount.
        return Some((read_u16(image, 12), read_u16(image, 14)));
    }
    None
}

/// Lit un entier 16 bits en petit-boutiste, `0` si la position déborde.
fn read_u16(data: &[u8], at: usize) -> u16 {
    match data.get(at..at + 2) {
        Some([low, high]) => u16::from_le_bytes([*low, *high]),
        _ => 0,
    }
}

/// Lit un entier 32 bits en petit-boutiste, `0` si la position déborde.
fn read_u32(data: &[u8], at: usize) -> u32 {
    match data.get(at..at + 4) {
        Some([a, b, c, d]) => u32::from_le_bytes([*a, *b, *c, *d]),
        _ => 0,
    }
}

#[cfg(test)]
#[allow(clippy::panic)] // un échec de lecture est un échec de test
mod tests {
    use super::{Error, IconGroup};

    /// Fabrique une icône à deux images DIB factices de tailles différentes.
    fn ico_factice() -> Vec<u8> {
        let mut image_a = vec![0u8; 40];
        image_a[0] = 40; // biSize
        image_a[12] = 1; // biPlanes
        image_a[14] = 32; // biBitCount
        let mut image_b = image_a.clone();
        image_b[14] = 8;

        let mut out = vec![0, 0, 1, 0, 2, 0];
        let first = 6 + 32;
        for (index, (side, image)) in [(16u8, &image_a), (32u8, &image_b)].iter().enumerate() {
            out.push(*side);
            out.push(*side);
            out.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // couleurs, réservé, plans, bits
            out.extend_from_slice(&u32::try_from(image.len()).unwrap_or(0).to_le_bytes());
            let offset = first + index * image_a.len();
            out.extend_from_slice(&u32::try_from(offset).unwrap_or(0).to_le_bytes());
        }
        out.extend_from_slice(&image_a);
        out.extend_from_slice(&image_b);
        out
    }

    #[test]
    fn lit_les_deux_images() {
        let ico = ico_factice();
        let Ok(group) = IconGroup::parse(&ico) else {
            panic!("icône factice illisible")
        };
        assert_eq!(group.images().len(), 2);
        assert_eq!(group.images()[0].len(), 40);
    }

    #[test]
    fn le_repertoire_reprend_la_geometrie_de_limage() {
        let ico = ico_factice();
        let Ok(group) = IconGroup::parse(&ico) else {
            panic!("icône factice illisible")
        };
        let dir = group.directory(1);
        assert_eq!(dir.len(), 6 + 14 * 2);
        assert_eq!(&dir[..6], &[0, 0, 1, 0, 2, 0]);
        // Les bits par pixel viennent du BITMAPINFOHEADER, pas des zéros du répertoire.
        assert_eq!(u16::from_le_bytes([dir[6 + 6], dir[6 + 7]]), 32);
        assert_eq!(u16::from_le_bytes([dir[20 + 6], dir[20 + 7]]), 8);
        // Identifiants attribués dans l'ordre.
        assert_eq!(u16::from_le_bytes([dir[6 + 12], dir[6 + 13]]), 1);
        assert_eq!(u16::from_le_bytes([dir[20 + 12], dir[20 + 13]]), 2);
    }

    #[test]
    fn les_png_sont_reconnus() {
        let mut image = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];
        image.resize(26, 0);
        image[24] = 8; // profondeur
        image[25] = 6; // RVB + alpha
        let mut out = vec![0, 0, 1, 0, 1, 0];
        out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&u32::try_from(image.len()).unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&22u32.to_le_bytes());
        out.extend_from_slice(&image);
        let Ok(group) = IconGroup::parse(&out) else {
            panic!("icône PNG illisible")
        };
        let dir = group.directory(1);
        assert_eq!(u16::from_le_bytes([dir[6 + 6], dir[6 + 7]]), 32);
    }

    #[test]
    fn refuse_les_fichiers_invalides() {
        assert_eq!(IconGroup::parse(&[]).err(), Some(Error::Signature));
        assert_eq!(
            IconGroup::parse(&[1, 0, 1, 0, 1, 0]).err(),
            Some(Error::Signature)
        );
        assert_eq!(
            IconGroup::parse(&[0, 0, 1, 0, 0, 0]).err(),
            Some(Error::Empty)
        );
        assert_eq!(
            IconGroup::parse(&[0, 0, 1, 0, 1, 0]).err(),
            Some(Error::Truncated)
        );
        let mut tronque = ico_factice();
        tronque.truncate(6 + 32 + 10);
        assert_eq!(IconGroup::parse(&tronque).err(), Some(Error::Image));
    }
}
