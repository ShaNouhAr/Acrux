//! Lecture des images BMP (Windows Bitmap).
//!
//! Le format est celui des `BITMAPFILEHEADER` + `BITMAPINFOHEADER` de
//! Windows. Sont acceptés :
//!
//! - les en-têtes **cœur** (12 octets, palette sur 3 octets) et **info**
//!   (40 octets et au-delà : V4 108, V5 124, palette sur 4 octets) ;
//! - 1, 4, 8 bits **en palette**, 16 bits (555 par défaut, ou les masques
//!   d'un `BI_BITFIELDS`), 24 et 32 bits ;
//! - les compressions `BI_RGB` (aucune), `BI_BITFIELDS`, `BI_RLE8` et
//!   `BI_RLE4` ;
//! - les images **de bas en haut** (le cas ordinaire) comme celles de haut en
//!   bas (hauteur négative).
//!
//! La transparence n'est lue que lorsque le fichier la déclare : un masque
//! alpha non nul dans un en-tête V4 ou V5. Un BMP 32 bits ordinaire garde son
//! quatrième octet « inutilisé », comme le veut la norme — le lire
//! rendrait transparentes quantité d'images qui ne le sont pas.

use acrux_core::{Error, Result};

use super::Raster;

/// Quatre octets, petit-boutiste.
fn u32_at(data: &[u8], at: usize) -> Result<u32> {
    let b = data
        .get(at..at + 4)
        .ok_or_else(|| Error::Corrupt("BMP tronqué".into()))?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Deux octets, petit-boutiste.
fn u16_at(data: &[u8], at: usize) -> Result<u16> {
    let b = data
        .get(at..at + 2)
        .ok_or_else(|| Error::Corrupt("BMP tronqué".into()))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

/// Ce que l'en-tête dit de l'image.
struct Header {
    width: u32,
    height: u32,
    /// Les lignes sont écrites du haut vers le bas (hauteur négative).
    top_down: bool,
    bits: u16,
    compression: u32,
    /// Palette développée en RVB, vide si l'image n'en a pas.
    palette: Vec<[u8; 3]>,
    /// Masques des composantes, pour `BI_BITFIELDS` et les en-têtes V4/V5.
    masks: [u32; 4],
    /// Premier octet des pixels.
    pixels: usize,
}

/// Décode un BMP en pixels.
///
/// # Errors
/// En-tête illisible, profondeur ou compression hors du domaine pris en
/// charge, ou données tronquées.
pub(crate) fn raster(data: &[u8]) -> Result<Raster> {
    let h = header(data)?;
    let pixels = data
        .get(h.pixels..)
        .ok_or_else(|| Error::Corrupt("BMP : pixels absents".into()))?;
    let rows = match h.compression {
        1 => rle8(&h, pixels)?,
        2 => rle4(&h, pixels)?,
        _ => plain(&h, pixels)?,
    };
    Ok(to_raster(&h, rows))
}

/// Lit l'en-tête de fichier puis celui de l'image.
fn header(data: &[u8]) -> Result<Header> {
    if !data.starts_with(b"BM") {
        return Err(Error::Corrupt("BMP : signature absente".into()));
    }
    let start = u32_at(data, 10)? as usize;
    let info = u32_at(data, 14)? as usize;
    // L'en-tête « cœur » d'OS/2 est le seul à tenir en douze octets ; tous les
    // autres commencent par les mêmes champs que le `BITMAPINFOHEADER`.
    let core = info == 12;
    let (width, height, top_down, bits) = if core {
        let w = u32::from(u16_at(data, 18)?);
        let h = u32::from(u16_at(data, 20)?);
        (w, h, false, u16_at(data, 24)?)
    } else {
        if info < 40 {
            return Err(Error::Unsupported(format!(
                "BMP : en-tête de {info} octets inconnu"
            )));
        }
        let w = u32_at(data, 18)?;
        // La hauteur est signée : négative, elle dit que les lignes vont du
        // haut vers le bas.
        let signed = i32::from_ne_bytes(u32_at(data, 22)?.to_ne_bytes());
        // 26 : le nombre de plans, toujours 1 ; 28 : les bits par pixel.
        (w, signed.unsigned_abs(), signed < 0, u16_at(data, 28)?)
    };
    if width == 0 || height == 0 {
        return Err(Error::Corrupt("BMP : dimensions nulles".into()));
    }
    let compression = if core { 0 } else { u32_at(data, 30)? };
    if !matches!(compression, 0 | 1 | 2 | 3 | 6) {
        return Err(Error::Unsupported(format!(
            "BMP : compression {compression} non prise en charge"
        )));
    }
    if !matches!(bits, 1 | 4 | 8 | 16 | 24 | 32) {
        return Err(Error::Unsupported(format!("BMP : {bits} bits par pixel")));
    }
    // Les masques : soit ils suivent l'en-tête (BI_BITFIELDS d'un en-tête de
    // 40 octets), soit ils en font partie (V4 et au-delà).
    let mut masks = default_masks(bits);
    let mut after = 14 + info;
    if !core && matches!(compression, 3 | 6) {
        if info >= 56 {
            for (i, m) in masks.iter_mut().enumerate() {
                *m = u32_at(data, 14 + 40 + i * 4)?;
            }
        } else {
            let count = if compression == 6 { 4 } else { 3 };
            for (i, m) in masks.iter_mut().take(count).enumerate() {
                *m = u32_at(data, after + i * 4)?;
            }
            after += count * 4;
            if count == 3 {
                masks[3] = 0;
            }
        }
    } else if !core && info >= 56 {
        // Un en-tête V4 ou V5 porte toujours ses masques, même sans
        // `BI_BITFIELDS` : ils ne servent alors qu'à décrire l'alpha.
        for (i, m) in masks.iter_mut().enumerate().take(4) {
            let read = u32_at(data, 14 + 40 + i * 4)?;
            if read != 0 && i == 3 {
                *m = read;
            }
        }
    }
    let mut palette = Vec::new();
    if bits <= 8 {
        let used = if core { 0 } else { u32_at(data, 46)? as usize };
        let count = if used == 0 { 1usize << bits } else { used };
        let entry = if core { 3 } else { 4 };
        for i in 0..count {
            let at = after + i * entry;
            let Some(e) = data.get(at..at + 3) else { break };
            palette.push([e[2], e[1], e[0]]);
        }
        if palette.is_empty() {
            return Err(Error::Corrupt("BMP : palette absente".into()));
        }
    }
    let pixels = if start == 0 { after } else { start };
    Ok(Header {
        width,
        height,
        top_down,
        bits,
        compression,
        palette,
        masks,
        pixels,
    })
}

/// Masques employés quand le fichier n'en donne pas.
fn default_masks(bits: u16) -> [u32; 4] {
    match bits {
        // 16 bits sans masques : cinq bits par composante (555).
        16 => [0x7C00, 0x03E0, 0x001F, 0],
        32 => [0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0],
        _ => [0, 0, 0, 0],
    }
}

/// Une ligne de pixels RVBA.
type Row = Vec<[u8; 4]>;

/// Lignes d'une image non compressée, dans l'ordre du fichier.
fn plain(h: &Header, pixels: &[u8]) -> Result<Vec<Row>> {
    let bits = usize::from(h.bits);
    // Chaque ligne est calée sur quatre octets.
    let stride = (h.width as usize * bits).div_ceil(32) * 4;
    let mut rows = Vec::with_capacity(h.height as usize);
    for y in 0..h.height as usize {
        let line = pixels
            .get(y * stride..(y + 1) * stride)
            .ok_or_else(|| Error::Corrupt("BMP tronqué".into()))?;
        let mut row = Vec::with_capacity(h.width as usize);
        for x in 0..h.width as usize {
            row.push(pixel(h, line, x)?);
        }
        rows.push(row);
    }
    Ok(rows)
}

/// Un pixel de la ligne, développé en RVBA.
fn pixel(h: &Header, line: &[u8], x: usize) -> Result<[u8; 4]> {
    let bits = usize::from(h.bits);
    match h.bits {
        1 | 4 | 8 => {
            let per_byte = 8 / bits;
            let byte = *line
                .get(x / per_byte)
                .ok_or_else(|| Error::Corrupt("BMP tronqué".into()))?;
            let shift = 8 - bits * (x % per_byte + 1);
            let mask = u8::try_from((1u16 << bits) - 1).unwrap_or(u8::MAX);
            let index = usize::from((byte >> shift) & mask);
            let c = h.palette.get(index).copied().unwrap_or([0, 0, 0]);
            Ok([c[0], c[1], c[2], 255])
        }
        16 => {
            let at = x * 2;
            let v = u32::from(u16::from_le_bytes([
                *line.get(at).ok_or_else(short)?,
                *line.get(at + 1).ok_or_else(short)?,
            ]));
            Ok(from_masks(h, v))
        }
        24 => {
            let at = x * 3;
            let b = line.get(at..at + 3).ok_or_else(short)?;
            Ok([b[2], b[1], b[0], 255])
        }
        _ => {
            let at = x * 4;
            let b = line.get(at..at + 4).ok_or_else(short)?;
            let v = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            Ok(from_masks(h, v))
        }
    }
}

fn short() -> Error {
    Error::Corrupt("BMP tronqué".into())
}

/// Développe une valeur selon les masques des composantes.
fn from_masks(h: &Header, v: u32) -> [u8; 4] {
    let mut out = [0u8, 0, 0, 255];
    for (i, mask) in h.masks.iter().enumerate() {
        if *mask == 0 {
            continue;
        }
        let shift = mask.trailing_zeros();
        let width = mask.count_ones();
        let raw = (v & mask) >> shift;
        // Étalement sur huit bits : 0b11111 doit donner 255, pas 248.
        let max = (1u32 << width) - 1;
        out[i] = raw
            .checked_mul(255)
            .and_then(|v| v.checked_div(max))
            .and_then(|v| u8::try_from(v).ok())
            .unwrap_or(255);
    }
    out
}

/// Lignes d'une image `BI_RLE8`.
fn rle8(h: &Header, pixels: &[u8]) -> Result<Vec<Row>> {
    let mut rows = blank_rows(h);
    let (mut x, mut y) = (0usize, 0usize);
    let mut at = 0usize;
    while at + 1 < pixels.len() {
        let (count, value) = (pixels[at], pixels[at + 1]);
        at += 2;
        match (count, value) {
            (0, 0) => {
                y += 1;
                x = 0;
            }
            (0, 1) => break,
            (0, 2) => {
                // Saut relatif : deux octets de déplacement.
                x += usize::from(*pixels.get(at).ok_or_else(short)?);
                y += usize::from(*pixels.get(at + 1).ok_or_else(short)?);
                at += 2;
            }
            // Suite de pixels bruts, calée sur deux octets.
            (0, n) => {
                for i in 0..usize::from(n) {
                    let index = usize::from(*pixels.get(at + i).ok_or_else(short)?);
                    put(h, &mut rows, x + i, y, index);
                }
                at += usize::from(n);
                if usize::from(n) % 2 == 1 {
                    at += 1;
                }
                x += usize::from(n);
            }
            // Répétition.
            (n, index) => {
                for i in 0..usize::from(n) {
                    put(h, &mut rows, x + i, y, usize::from(index));
                }
                x += usize::from(n);
            }
        }
    }
    Ok(rows)
}

/// Lignes d'une image `BI_RLE4`.
fn rle4(h: &Header, pixels: &[u8]) -> Result<Vec<Row>> {
    let mut rows = blank_rows(h);
    let (mut x, mut y) = (0usize, 0usize);
    let mut at = 0usize;
    while at + 1 < pixels.len() {
        let (count, value) = (pixels[at], pixels[at + 1]);
        at += 2;
        match (count, value) {
            (0, 0) => {
                y += 1;
                x = 0;
            }
            (0, 1) => break,
            (0, 2) => {
                x += usize::from(*pixels.get(at).ok_or_else(short)?);
                y += usize::from(*pixels.get(at + 1).ok_or_else(short)?);
                at += 2;
            }
            (0, n) => {
                let n = usize::from(n);
                for i in 0..n {
                    let byte = *pixels.get(at + i / 2).ok_or_else(short)?;
                    let index = if i % 2 == 0 { byte >> 4 } else { byte & 0x0F };
                    put(h, &mut rows, x + i, y, usize::from(index));
                }
                // Les données brutes occupent un nombre pair d'octets.
                let bytes = n.div_ceil(2);
                at += bytes + bytes % 2;
                x += n;
            }
            (n, pair) => {
                for i in 0..usize::from(n) {
                    let index = if i % 2 == 0 { pair >> 4 } else { pair & 0x0F };
                    put(h, &mut rows, x + i, y, usize::from(index));
                }
                x += usize::from(n);
            }
        }
    }
    Ok(rows)
}

/// Lignes vides, de la couleur de la première entrée de palette.
fn blank_rows(h: &Header) -> Vec<Row> {
    let c = h.palette.first().copied().unwrap_or([0, 0, 0]);
    vec![vec![[c[0], c[1], c[2], 255]; h.width as usize]; h.height as usize]
}

/// Pose un pixel de palette, en ignorant ce qui déborde.
fn put(header: &Header, rows: &mut [Row], x: usize, y: usize, index: usize) {
    let Some(row) = rows.get_mut(y) else { return };
    let Some(target) = row.get_mut(x) else { return };
    let c = header.palette.get(index).copied().unwrap_or([0, 0, 0]);
    *target = [c[0], c[1], c[2], 255];
}

/// Assemble les lignes dans le sens de lecture d'une image.
fn to_raster(h: &Header, mut rows: Vec<Row>) -> Raster {
    // Un BMP s'écrit de bas en haut, sauf hauteur négative.
    if !h.top_down {
        rows.reverse();
    }
    let count = (h.width as usize) * (h.height as usize);
    let mut data = Vec::with_capacity(count * 3);
    let mut alpha = Vec::with_capacity(count);
    let mut grey = true;
    for row in &rows {
        for p in row {
            data.extend_from_slice(&p[..3]);
            alpha.push(p[3]);
            grey &= p[0] == p[1] && p[1] == p[2];
        }
    }
    // Une image sans couleur s'écrit en niveaux de gris : trois fois moins
    // d'octets pour exactement la même image.
    let (components, data) = if grey {
        (1, data.iter().step_by(3).copied().collect())
    } else {
        (3, data)
    };
    let transparent = h.masks[3] != 0 && alpha.iter().any(|a| *a != 255);
    Raster {
        width: h.width,
        height: h.height,
        components,
        data,
        alpha: transparent.then_some(alpha),
    }
}
