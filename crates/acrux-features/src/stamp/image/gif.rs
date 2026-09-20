//! Lecture des images GIF (87a et 89a).
//!
//! Seule la **première image** est lue : un GIF animé devient dans un PDF ce
//! qu'il est dans Acrobat, une image fixe. Sont pris en charge la palette
//! globale et la palette locale, l'entrelacement (les quatre passes), et la
//! couleur transparente déclarée par un bloc de contrôle graphique — elle
//! devient le `/SMask` de l'image.
//!
//! La compression est le LZW de la norme GIF : codes de longueur variable
//! **rangés bit de poids faible en tête**, sans le décalage d'une unité du
//! LZW de PostScript. C'est pourquoi il est écrit ici plutôt qu'emprunté à
//! `acrux-codecs`.

use acrux_core::{Error, Result};

use super::Raster;

fn corrupt(what: &str) -> Error {
    Error::Corrupt(format!("GIF : {what}"))
}

/// Décode la première image d'un GIF.
///
/// # Errors
/// Signature absente, fichier tronqué, ou flux LZW incohérent.
pub(crate) fn raster(data: &[u8]) -> Result<Raster> {
    if !data.starts_with(b"GIF87a") && !data.starts_with(b"GIF89a") {
        return Err(corrupt("signature absente"));
    }
    let packed = *data.get(10).ok_or_else(|| corrupt("en-tête tronqué"))?;
    let mut at = 13;
    let mut global = Vec::new();
    if packed & 0x80 != 0 {
        let count = 2usize << (packed & 0x07);
        global = palette(data, at, count)?;
        at += count * 3;
    }
    // Les blocs se suivent jusqu'à la première image.
    let mut transparent: Option<u8> = None;
    loop {
        match *data.get(at).ok_or_else(|| corrupt("fin prématurée"))? {
            0x3B => return Err(corrupt("aucune image")),
            0x21 => {
                let label = *data.get(at + 1).ok_or_else(|| corrupt("extension"))?;
                let start = at + 2;
                if label == 0xF9 {
                    // Contrôle graphique : le drapeau de transparence et
                    // l'indice de la couleur transparente.
                    let flags = *data.get(start + 1).ok_or_else(|| corrupt("extension"))?;
                    if flags & 0x01 != 0 {
                        transparent =
                            Some(*data.get(start + 4).ok_or_else(|| corrupt("extension"))?);
                    }
                }
                at = skip_blocks(data, start)?;
            }
            0x2C => break,
            other => return Err(corrupt(&format!("bloc inconnu 0x{other:02X}"))),
        }
    }
    // Descripteur d'image : position, taille, palette locale, entrelacement.
    let width = u16_at(data, at + 5)?;
    let height = u16_at(data, at + 7)?;
    let flags = *data.get(at + 9).ok_or_else(|| corrupt("descripteur"))?;
    let interlaced = flags & 0x40 != 0;
    let mut table = global;
    let mut pos = at + 10;
    if flags & 0x80 != 0 {
        let count = 2usize << (flags & 0x07);
        table = palette(data, pos, count)?;
        pos += count * 3;
    }
    if table.is_empty() {
        return Err(corrupt("aucune palette"));
    }
    if width == 0 || height == 0 {
        return Err(corrupt("dimensions nulles"));
    }
    let min_code = *data.get(pos).ok_or_else(|| corrupt("données absentes"))?;
    let body = gather(data, pos + 1)?;
    let indexes = lzw(&body, min_code, width as usize * height as usize)?;
    Ok(assemble(
        &indexes,
        &table,
        width,
        height,
        interlaced,
        transparent,
    ))
}

/// Deux octets, petit-boutiste.
fn u16_at(data: &[u8], at: usize) -> Result<u16> {
    let b = data
        .get(at..at + 2)
        .ok_or_else(|| corrupt("en-tête tronqué"))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

/// Lit une table de couleurs.
fn palette(data: &[u8], at: usize, count: usize) -> Result<Vec<[u8; 3]>> {
    let bytes = data
        .get(at..at + count * 3)
        .ok_or_else(|| corrupt("palette tronquée"))?;
    Ok(bytes.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect())
}

/// Saute une suite de sous-blocs et rend la position qui suit.
fn skip_blocks(data: &[u8], mut at: usize) -> Result<usize> {
    loop {
        let len = usize::from(*data.get(at).ok_or_else(|| corrupt("sous-bloc"))?);
        at += 1 + len;
        if len == 0 {
            return Ok(at);
        }
    }
}

/// Rassemble les sous-blocs de données en un seul flux.
fn gather(data: &[u8], mut at: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let len = usize::from(*data.get(at).ok_or_else(|| corrupt("sous-bloc"))?);
        if len == 0 {
            return Ok(out);
        }
        out.extend_from_slice(
            data.get(at + 1..at + 1 + len)
                .ok_or_else(|| corrupt("sous-bloc tronqué"))?,
        );
        at += 1 + len;
    }
}

/// Décompresse le LZW de GIF en indices de palette.
fn lzw(data: &[u8], min_code: u8, expected: usize) -> Result<Vec<u8>> {
    if !(2..=11).contains(&min_code) {
        return Err(corrupt("taille de code hors norme"));
    }
    let clear = 1u16 << min_code;
    let end = clear + 1;
    // Le dictionnaire : chaque entrée est un préfixe et la lettre qui le
    // suit. Les premières entrées sont les octets eux-mêmes, sans préfixe.
    let mut prefix = vec![NONE; 4096];
    let mut suffix = vec![0u8; 4096];
    for (i, byte) in suffix.iter_mut().enumerate().take(usize::from(clear)) {
        *byte = u8::try_from(i).unwrap_or(0);
    }
    let mut next = end + 1;
    let mut size = u32::from(min_code) + 1;
    let mut out = Vec::with_capacity(expected);
    let mut previous: Option<u16> = None;
    let (mut bits, mut acc) = (0u32, 0u32);
    let mut stack = Vec::with_capacity(4096);
    for byte in data {
        // Les codes sont rangés bit de poids faible en tête.
        acc |= u32::from(*byte) << bits;
        bits += 8;
        while bits >= size {
            let code = u16::try_from(acc & ((1 << size) - 1)).unwrap_or(0);
            acc >>= size;
            bits -= size;
            if code == clear {
                next = end + 1;
                size = u32::from(min_code) + 1;
                previous = None;
                continue;
            }
            if code == end {
                return Ok(out);
            }
            // Le cas « code encore inconnu » : la suite est celle du code
            // précédent, suivie de sa propre première lettre.
            let first = if code < next {
                walk(code, &prefix, &suffix, &mut stack, &mut out)?
            } else if let Some(p) = previous {
                let f = walk(p, &prefix, &suffix, &mut stack, &mut out)?;
                out.push(f);
                f
            } else {
                return Err(corrupt("code inattendu"));
            };
            if let Some(p) = previous {
                if next < 4096 {
                    prefix[usize::from(next)] = p;
                    suffix[usize::from(next)] = first;
                    next += 1;
                    if u32::from(next) == 1 << size && size < 12 {
                        size += 1;
                    }
                }
            }
            previous = Some(code);
            if out.len() > expected * 2 + 4096 {
                return Err(corrupt("flux incohérent"));
            }
        }
    }
    Ok(out)
}

/// Entrée sans préfixe : les 256 premières, et celles qui n'existent pas.
const NONE: u16 = u16::MAX;

/// Écrit la suite désignée par `code` et rend sa première lettre.
///
/// La suite se remonte du dernier caractère au premier, préfixe après
/// préfixe ; elle ressort donc de la pile à l'endroit.
fn walk(
    code: u16,
    prefix: &[u16],
    suffix: &[u8],
    stack: &mut Vec<u8>,
    out: &mut Vec<u8>,
) -> Result<u8> {
    stack.clear();
    let mut c = code;
    loop {
        let i = usize::from(c);
        if i >= suffix.len() || stack.len() > 4096 {
            return Err(corrupt("chaîne incohérente"));
        }
        stack.push(suffix[i]);
        if prefix[i] == NONE {
            break;
        }
        c = prefix[i];
    }
    let first = *stack.last().unwrap_or(&0);
    out.extend(stack.iter().rev());
    Ok(first)
}

/// Développe les indices en pixels, en remettant les lignes entrelacées dans
/// l'ordre.
fn assemble(
    indexes: &[u8],
    table: &[[u8; 3]],
    width: u16,
    height: u16,
    interlaced: bool,
    transparent: Option<u8>,
) -> Raster {
    let (w, h) = (usize::from(width), usize::from(height));
    let order = if interlaced {
        // Les quatre passes de GIF : une ligne sur huit, puis sur huit à
        // partir de la quatrième, sur quatre, enfin toutes les impaires.
        let mut rows = Vec::with_capacity(h);
        for (start, step) in [(0, 8), (4, 8), (2, 4), (1, 2)] {
            let mut y = start;
            while y < h {
                rows.push(y);
                y += step;
            }
        }
        rows
    } else {
        (0..h).collect()
    };
    let mut data = vec![0u8; w * h * 3];
    let mut alpha = vec![255u8; w * h];
    let mut grey = true;
    for (source, target) in order.into_iter().enumerate() {
        for x in 0..w {
            let Some(index) = indexes.get(source * w + x) else {
                continue;
            };
            let c = table.get(usize::from(*index)).copied().unwrap_or([0, 0, 0]);
            let at = target * w + x;
            data[at * 3] = c[0];
            data[at * 3 + 1] = c[1];
            data[at * 3 + 2] = c[2];
            grey &= c[0] == c[1] && c[1] == c[2];
            if transparent == Some(*index) {
                alpha[at] = 0;
            }
        }
    }
    let (components, data) = if grey {
        (1, data.iter().step_by(3).copied().collect())
    } else {
        (3, data)
    };
    let sheer = alpha.iter().any(|a| *a != 255);
    Raster {
        width: u32::from(width),
        height: u32::from(height),
        components,
        data,
        alpha: sheer.then_some(alpha),
    }
}
