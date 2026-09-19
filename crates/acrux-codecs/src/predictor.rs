//! Prédicteurs des filtres FlateDecode et LZWDecode (ISO 32000-2 §7.4.4.4, tableau 8).
//!
//! Après décompression, un flux peut avoir été « prédit » pour mieux se
//! compresser. Le dictionnaire /DecodeParms indique le prédicteur :
//!
//! | /Predictor | Signification                                            |
//! |------------|----------------------------------------------------------|
//! | 1          | Aucune prédiction (défaut).                              |
//! | 2          | Prédicteur TIFF 2 (différences horizontales, TIFF 6.0 §14). |
//! | 10 à 15    | Prédicteurs PNG (PNG §9 « Filtering »).                  |
//!
//! Pour les prédicteurs PNG, la valeur exacte (10 à 15) n'a d'importance que
//! pour l'encodeur : chaque ligne commence par un octet indiquant le filtre
//! réellement utilisé (0 None, 1 Sub, 2 Up, 3 Average, 4 Paeth), et c'est lui
//! que le décodeur suit.
//!
//! Les paramètres /Colors, /BitsPerComponent et /Columns décrivent la
//! géométrie des lignes. Une dernière ligne incomplète est ignorée, comme le
//! fait Acrobat.

use acrux_core::{Error, Result};

/// Applique l'inverse du prédicteur aux données décompressées.
///
/// - `predictor` : valeur de /Predictor (1, 2 ou 10 à 15) ;
/// - `colors` : composantes par pixel (≥ 1) ;
/// - `bpc` : bits par composante (1, 2, 4, 8 ou 16) ;
/// - `columns` : pixels par ligne (≥ 1).
///
/// # Errors
///
/// `Error::Corrupt` si les paramètres sont hors des valeurs admises par la
/// spécification, ou si une ligne PNG annonce un type de filtre inconnu.
pub fn apply(data: &[u8], predictor: u32, colors: u32, bpc: u32, columns: u32) -> Result<Vec<u8>> {
    match predictor {
        // 0 n'est pas prévu par la spécification mais se rencontre ; on le
        // traite comme 1 (aucune prédiction), ce qui est la seule lecture sensée.
        0 | 1 => Ok(data.to_vec()),
        2 => Ok(tiff(data, &Layout::new(colors, bpc, columns)?)),
        10..=15 => png(data, &Layout::new(colors, bpc, columns)?),
        other => Err(Error::Corrupt(format!(
            "prédicteur {other} inconnu (attendu 1, 2 ou 10-15)"
        ))),
    }
}

/// Géométrie d'une ligne d'échantillons.
struct Layout {
    colors: usize,
    bpc: usize,
    columns: usize,
    /// Octets par pixel, arrondi vers le haut et au moins 1 (PNG §9.2).
    bytes_per_pixel: usize,
    /// Octets par ligne (sans l'octet de filtre PNG).
    row_len: usize,
}

impl Layout {
    fn new(colors: u32, bpc: u32, columns: u32) -> Result<Self> {
        if !matches!(bpc, 1 | 2 | 4 | 8 | 16) {
            return Err(Error::Corrupt(format!(
                "BitsPerComponent {bpc} invalide pour un prédicteur"
            )));
        }
        if colors == 0 || columns == 0 {
            return Err(Error::Corrupt(
                "Colors et Columns doivent être ≥ 1".to_string(),
            ));
        }
        let bits_per_pixel = u64::from(colors) * u64::from(bpc);
        let bits_per_row = bits_per_pixel * u64::from(columns);
        let to_usize = |bits: u64| {
            usize::try_from(bits.div_ceil(8))
                .map_err(|_| Error::Corrupt("ligne de prédicteur trop grande".to_string()))
        };
        Ok(Layout {
            colors: colors as usize,
            bpc: bpc as usize,
            columns: columns as usize,
            bytes_per_pixel: to_usize(bits_per_pixel)?.max(1),
            row_len: to_usize(bits_per_row)?,
        })
    }
}

// ---------------------------------------------------------------------------
// Prédicteurs PNG (PNG §9 « Filtering »)
// ---------------------------------------------------------------------------

/// Défait les filtres PNG ligne par ligne. Chaque ligne du flux est précédée
/// d'un octet de type de filtre.
fn png(data: &[u8], layout: &Layout) -> Result<Vec<u8>> {
    let stride = layout.row_len + 1;
    let rows = data.len() / stride;
    if rows == 0 {
        // Aucune ligne complète : on n'alloue rien (une ligne peut être immense
        // si /Columns est aberrant).
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(rows * layout.row_len);
    let mut previous = vec![0u8; layout.row_len];
    let mut current = vec![0u8; layout.row_len];

    for (row_index, chunk) in data.chunks_exact(stride).enumerate() {
        let filter = chunk[0];
        current.copy_from_slice(&chunk[1..]);
        if !unfilter_row(filter, &mut current, &previous, layout.bytes_per_pixel) {
            return Err(Error::Corrupt(format!(
                "prédicteur PNG : type de filtre {filter} inconnu à la ligne {row_index}"
            )));
        }
        out.extend_from_slice(&current);
        std::mem::swap(&mut previous, &mut current);
    }
    Ok(out)
}

/// Reconstruit une ligne en place selon son type de filtre (PNG §9.2).
/// Renvoie `false` si le type est inconnu.
fn unfilter_row(filter: u8, row: &mut [u8], previous: &[u8], bpp: usize) -> bool {
    match filter {
        0 => {}
        1 => {
            for i in bpp..row.len() {
                row[i] = row[i].wrapping_add(row[i - bpp]);
            }
        }
        2 => {
            for (byte, &up) in row.iter_mut().zip(previous) {
                *byte = byte.wrapping_add(up);
            }
        }
        3 => {
            for i in 0..row.len() {
                let left = if i >= bpp { row[i - bpp] } else { 0 };
                let up = previous[i];
                // Moyenne entière arrondie vers le bas (PNG, §6.5 : floor((a + b) / 2)).
                let average = left.midpoint(up);
                row[i] = row[i].wrapping_add(average);
            }
        }
        4 => {
            for i in 0..row.len() {
                let (left, up_left) = if i >= bpp {
                    (row[i - bpp], previous[i - bpp])
                } else {
                    (0, 0)
                };
                row[i] = row[i].wrapping_add(paeth(left, previous[i], up_left));
            }
        }
        _ => return false,
    }
    true
}

/// Prédicteur de Paeth (PNG §9.4) : choisit parmi gauche (`a`), haut (`b`)
/// et haut-gauche (`c`) la valeur la plus proche de `a + b - c`.
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let pa = (p - i16::from(a)).abs();
    let pb = (p - i16::from(b)).abs();
    let pc = (p - i16::from(c)).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

// ---------------------------------------------------------------------------
// Prédicteur TIFF 2 (TIFF 6.0 §14 « Differencing Predictor »)
// ---------------------------------------------------------------------------

/// Défait les différences horizontales : chaque échantillon est stocké comme
/// différence avec l'échantillon de même composante du pixel précédent.
fn tiff(data: &[u8], layout: &Layout) -> Vec<u8> {
    let rows = data.len() / layout.row_len;
    let mut out = data[..rows * layout.row_len].to_vec();
    for row in out.chunks_exact_mut(layout.row_len) {
        match layout.bpc {
            8 => tiff_row_8(row, layout.colors),
            16 => tiff_row_16(row, layout.colors),
            _ => tiff_row_sub_byte(row, layout),
        }
    }
    out
}

/// Différences horizontales sur des échantillons de 8 bits.
fn tiff_row_8(row: &mut [u8], colors: usize) {
    for i in colors..row.len() {
        row[i] = row[i].wrapping_add(row[i - colors]);
    }
}

/// Différences horizontales sur des échantillons de 16 bits grand-boutistes
/// (ISO 32000-2 §7.4.4.4 : les échantillons de 16 bits sont en big-endian).
fn tiff_row_16(row: &mut [u8], colors: usize) {
    let samples = row.len() / 2;
    for i in colors..samples {
        let previous = u16::from_be_bytes([row[2 * (i - colors)], row[2 * (i - colors) + 1]]);
        let current = u16::from_be_bytes([row[2 * i], row[2 * i + 1]]);
        row[2 * i..2 * i + 2].copy_from_slice(&current.wrapping_add(previous).to_be_bytes());
    }
}

/// Différences horizontales sur des échantillons de 1, 2 ou 4 bits, rangés
/// du bit de poids fort au bit de poids faible dans chaque octet.
fn tiff_row_sub_byte(row: &mut [u8], layout: &Layout) {
    let bpc = layout.bpc;
    let per_byte = 8 / bpc;
    let mask = (1u8 << bpc) - 1;
    let total = layout.colors * layout.columns;
    let mut previous = vec![0u8; layout.colors];

    for index in 0..total {
        let byte_index = index / per_byte;
        let shift = 8 - bpc * (index % per_byte + 1);
        let component = index % layout.colors;
        let raw = (row[byte_index] >> shift) & mask;
        let value = raw.wrapping_add(previous[component]) & mask;
        row[byte_index] = (row[byte_index] & !(mask << shift)) | (value << shift);
        previous[component] = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predictor_one_is_identity() {
        assert_eq!(apply(b"abc", 1, 1, 8, 1), Ok(b"abc".to_vec()));
        assert_eq!(apply(b"abc", 0, 1, 8, 1), Ok(b"abc".to_vec()));
    }

    #[test]
    fn rejects_invalid_parameters() {
        assert!(matches!(apply(b"", 7, 1, 8, 1), Err(Error::Corrupt(_))));
        assert!(matches!(apply(b"", 12, 1, 3, 1), Err(Error::Corrupt(_))));
        assert!(matches!(apply(b"", 12, 0, 8, 1), Err(Error::Corrupt(_))));
        assert!(matches!(apply(b"", 2, 1, 8, 0), Err(Error::Corrupt(_))));
    }

    #[test]
    fn png_none_and_sub() {
        // Deux lignes de 3 octets : None puis Sub.
        let data = [0, 1, 2, 3, 1, 10, 1, 1];
        assert_eq!(apply(&data, 12, 1, 8, 3), Ok(vec![1, 2, 3, 10, 11, 12]));
    }

    #[test]
    fn png_up() {
        let data = [0, 1, 2, 3, 2, 1, 1, 1];
        assert_eq!(apply(&data, 12, 1, 8, 3), Ok(vec![1, 2, 3, 2, 3, 4]));
    }

    #[test]
    fn png_average() {
        // Ligne 1 : None → 2 4 6. Ligne 2 : Average avec gauche/haut.
        // Pixel 0 : gauche 0, haut 2 → moyenne 1 ; 5 + 1 = 6.
        // Pixel 1 : gauche 6, haut 4 → moyenne 5 ; 1 + 5 = 6.
        // Pixel 2 : gauche 6, haut 6 → moyenne 6 ; 0 + 6 = 6.
        let data = [0, 2, 4, 6, 3, 5, 1, 0];
        assert_eq!(apply(&data, 13, 1, 8, 3), Ok(vec![2, 4, 6, 6, 6, 6]));
    }

    #[test]
    fn png_paeth() {
        // Ligne 1 : 10 20 30. Ligne 2 : Paeth.
        // Pixel 0 : a=0 b=10 c=0 → p=10 → pa=10 pb=0 pc=10 → b=10 ; 1+10 = 11.
        // Pixel 1 : a=11 b=20 c=10 → p=21 → pa=10 pb=1 pc=11 → b=20 ; 2+20 = 22.
        // Pixel 2 : a=22 b=30 c=20 → p=32 → pa=10 pb=2 pc=12 → b=30 ; 3+30 = 33.
        let data = [0, 10, 20, 30, 4, 1, 2, 3];
        assert_eq!(apply(&data, 15, 1, 8, 3), Ok(vec![10, 20, 30, 11, 22, 33]));
    }

    #[test]
    fn paeth_prefers_left_then_up_then_up_left() {
        assert_eq!(paeth(5, 5, 5), 5);
        assert_eq!(paeth(100, 0, 0), 100);
        assert_eq!(paeth(0, 100, 0), 100);
        assert_eq!(paeth(50, 50, 0), 50);
        assert_eq!(paeth(10, 20, 15), 15);
    }

    #[test]
    fn png_sub_with_three_colors() {
        // Un pixel RVB de 3 octets : Sub ajoute le pixel précédent composante par composante.
        let data = [1, 1, 2, 3, 1, 1, 1];
        assert_eq!(apply(&data, 11, 3, 8, 2), Ok(vec![1, 2, 3, 2, 3, 4]));
    }

    #[test]
    fn png_sub_with_16_bit_samples() {
        // bpp = 2 : l'octet i s'ajoute à l'octet i-2.
        let data = [1, 0x01, 0x02, 0x00, 0x01];
        assert_eq!(apply(&data, 11, 1, 16, 2), Ok(vec![0x01, 0x02, 0x01, 0x03]));
    }

    #[test]
    fn png_ignores_incomplete_last_row() {
        let data = [0, 1, 2, 3, 0, 9];
        assert_eq!(apply(&data, 12, 1, 8, 3), Ok(vec![1, 2, 3]));
    }

    #[test]
    fn png_rejects_unknown_filter_type() {
        assert!(matches!(
            apply(&[7, 1, 2, 3], 12, 1, 8, 3),
            Err(Error::Corrupt(_))
        ));
    }

    #[test]
    fn tiff_8_bit_two_colors() {
        // Deux composantes : chaque composante s'accumule séparément.
        let data = [10, 20, 1, 2, 1, 2];
        assert_eq!(apply(&data, 2, 2, 8, 3), Ok(vec![10, 20, 11, 22, 12, 24]));
    }

    #[test]
    fn tiff_8_bit_wraps_modulo_256() {
        assert_eq!(apply(&[250, 10], 2, 1, 8, 2), Ok(vec![250, 4]));
    }

    #[test]
    fn tiff_16_bit() {
        let data = [0x01, 0x00, 0x00, 0xFF, 0x00, 0x02];
        assert_eq!(
            apply(&data, 2, 1, 16, 3),
            Ok(vec![0x01, 0x00, 0x01, 0xFF, 0x02, 0x01])
        );
    }

    #[test]
    fn tiff_4_bit() {
        // Échantillons 4 bits : 1, 1, 1, 1 → 1, 2, 3, 4.
        assert_eq!(apply(&[0x11, 0x11], 2, 1, 4, 4), Ok(vec![0x12, 0x34]));
    }

    #[test]
    fn tiff_2_bit_wraps_modulo_4() {
        // Échantillons 2 bits : 3, 3, 3, 3 → 3, 2, 1, 0 (accumulation modulo 4).
        assert_eq!(apply(&[0xFF], 2, 1, 2, 4), Ok(vec![0b1110_0100]));
    }

    #[test]
    fn tiff_1_bit_is_xor_accumulation() {
        // 1 bit : 1,0,0,0,1,0,0,0 → 1,1,1,1,0,0,0,0.
        assert_eq!(apply(&[0b1000_1000], 2, 1, 1, 8), Ok(vec![0b1111_0000]));
    }

    #[test]
    fn tiff_1_bit_two_rows_with_padding() {
        // 3 colonnes de 1 bit : chaque ligne tient sur un octet avec 5 bits de bourrage.
        assert_eq!(
            apply(&[0b1010_0000, 0b1100_0000], 2, 1, 1, 3),
            Ok(vec![0b1100_0000, 0b1000_0000])
        );
    }

    #[test]
    fn tiff_ignores_incomplete_last_row() {
        assert_eq!(apply(&[1, 1, 1, 9], 2, 1, 8, 3), Ok(vec![1, 2, 3]));
    }

    #[test]
    fn huge_columns_yield_no_row() {
        assert_eq!(apply(&[0, 1, 2], 12, 4, 16, u32::MAX), Ok(Vec::new()));
        assert_eq!(apply(&[0, 1, 2], 2, 4, 16, u32::MAX), Ok(Vec::new()));
    }
}
