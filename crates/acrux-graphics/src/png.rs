//! Encodeur PNG (ISO/IEC 15948, RFC 1950 zlib, RFC 1951 deflate), écrit de
//! zéro. Structure : signature, chunk `IHDR`, un `IDAT` contenant un flux
//! zlib, `IEND` ; le CRC-32 des chunks et l'Adler-32 du flux zlib sont
//! calculés ici.
//!
//! Deux sorties, selon l'usage :
//!
//! - [`encode_png`], [`encode_png_rgb`] : **sans compression** (filtre
//!   « None » et blocs « stored »), pour les images de référence des tests de
//!   rendu, lisibles telles quelles et sans dépendance ;
//! - [`encode_png_with`] : lignes **filtrées** (le filtre le moins coûteux par
//!   ligne, §12.8) et flux zlib confié à l'appelant — `acrux_features::export`
//!   lui passe le compresseur DEFLATE maison, ce qui divise la taille d'une
//!   page rendue par vingt.
//!
//! Ce module ne dépend pas d'`acrux-codecs` (aucun cycle) : la compression entre
//! par ce paramètre.

use crate::bitmap::{unpremultiply, Bitmap};

/// Signature des fichiers PNG (ISO/IEC 15948 §5.2).
pub const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Table du CRC-32 (polynôme 0xEDB88320), calculée à la compilation.
const CRC_TABLE: [u32; 256] = build_crc_table();

const fn build_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        #[allow(clippy::cast_possible_truncation)] // n < 256
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

/// CRC-32 tel que défini par PNG (ISO/IEC 15948 §5.5).
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = CRC_TABLE[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

/// Adler-32 (RFC 1950 §8.2).
#[must_use]
pub fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let mut a = 1u32;
    let mut b = 0u32;
    // Par blocs pour éviter tout débordement de `b` avant réduction.
    for chunk in data.chunks(5_000) {
        for &v in chunk {
            a += u32::from(v);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

/// Flux zlib sans compression : en-tête `78 01`, blocs stored de 65 535
/// octets au plus (RFC 1951 §3.2.4), Adler-32 final.
#[must_use]
pub fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 65_535 * 5 + 11);
    out.extend_from_slice(&[0x78, 0x01]);
    let mut blocks = data.chunks(65_535).peekable();
    if blocks.peek().is_none() {
        // Un flux vide contient tout de même un bloc final vide.
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
    }
    while let Some(block) = blocks.next() {
        let last = blocks.peek().is_none();
        out.push(u8::from(last)); // BFINAL, BTYPE = 00
        #[allow(clippy::cast_possible_truncation)] // ≤ 65 535
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Ajoute un chunk `type + données + CRC` (ISO/IEC 15948 §5.3).
fn push_chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    let start = out.len();
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Disposition des octets fournis à [`encode_png_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelLayout {
    /// 3 octets par pixel (type de couleur 2).
    Rgb,
    /// 4 octets par pixel, opacité **non prémultipliée** (type de couleur 6).
    Rgba,
}

impl PixelLayout {
    /// Octets par pixel.
    #[must_use]
    pub fn channels(self) -> usize {
        match self {
            PixelLayout::Rgb => 3,
            PixelLayout::Rgba => 4,
        }
    }

    /// Type de couleur PNG (ISO/IEC 15948 §11.2.2, tableau 11.1).
    #[must_use]
    pub fn color_type(self) -> u8 {
        match self {
            PixelLayout::Rgb => 2,
            PixelLayout::Rgba => 6,
        }
    }
}

/// Une ligne de `stride` octets, complétée par des zéros si les données
/// fournies sont trop courtes.
fn row_of(pixels: &[u8], y: usize, stride: usize, out: &mut Vec<u8>) {
    out.clear();
    let start = y * stride;
    let end = start + stride;
    let avail = pixels
        .get(start.min(pixels.len())..pixels.len().min(end))
        .unwrap_or(&[]);
    out.extend_from_slice(avail);
    out.resize(stride, 0);
}

/// Prédicteur de Paeth (ISO/IEC 15948 §9.4).
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (i32::from(a), i32::from(b), i32::from(c));
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // valeurs 0..255
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

/// Filtre une ligne avec le type `kind` (§9.2), `prev` étant la ligne du dessus.
fn filter_row(kind: u8, row: &[u8], prev: &[u8], bpp: usize, out: &mut Vec<u8>) {
    out.clear();
    for i in 0..row.len() {
        let x = row[i];
        let a = if i >= bpp { row[i - bpp] } else { 0 };
        let b = prev.get(i).copied().unwrap_or(0);
        let c = if i >= bpp {
            prev.get(i - bpp).copied().unwrap_or(0)
        } else {
            0
        };
        out.push(match kind {
            1 => x.wrapping_sub(a),
            2 => x.wrapping_sub(b),
            3 => x.wrapping_sub(u8::midpoint(a, b)),
            4 => x.wrapping_sub(paeth(a, b, c)),
            _ => x,
        });
    }
}

/// Somme des valeurs absolues signées : heuristique de choix de filtre
/// recommandée par la spécification (§12.8).
fn filter_score(row: &[u8]) -> u64 {
    // Valeur absolue de l'octet lu comme un entier signé, sans conversion :
    // `min(b, 256 - b)`, et 0 pour 0.
    row.iter()
        .map(|&b| u64::from(b.min(b.wrapping_neg())))
        .sum()
}

/// Assemble le fichier PNG à partir des lignes brutes déjà filtrées.
fn assemble(width: u32, height: u32, color_type: u8, zlib: &[u8]) -> Vec<u8> {
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, color_type, 0, 0, 0]); // 8 bits, compression, filtre, entrelacement
    let mut out = Vec::with_capacity(zlib.len() + 64);
    out.extend_from_slice(&PNG_SIGNATURE);
    push_chunk(&mut out, *b"IHDR", &ihdr);
    push_chunk(&mut out, *b"IDAT", zlib);
    push_chunk(&mut out, *b"IEND", &[]);
    out
}

/// Encode des lignes déjà non prémultipliées (`channels` octets par pixel),
/// sans compression (filtre None et blocs stored).
fn encode_raw(width: u32, height: u32, channels: usize, color_type: u8, pixels: &[u8]) -> Vec<u8> {
    let stride = width as usize * channels;
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    let mut row = Vec::with_capacity(stride);
    for y in 0..height as usize {
        raw.push(0); // filtre None
        row_of(pixels, y, stride, &mut row);
        raw.extend_from_slice(&row);
    }
    assemble(width, height, color_type, &zlib_stored(&raw))
}

/// Encode un PNG **compressé** : chaque ligne reçoit le filtre qui minimise
/// la somme des valeurs absolues signées (§12.8), puis `deflate` produit le
/// flux zlib du chunk `IDAT`.
///
/// `deflate` reçoit les lignes filtrées et doit rendre un flux zlib complet
/// (RFC 1950) : passer `acrux_codecs::flate::compress` pour obtenir des fichiers
/// dix à trente fois plus petits que [`encode_png`], qui n'est pas compressé.
/// Des données trop courtes sont complétées par des zéros.
#[must_use]
pub fn encode_png_with(
    width: u32,
    height: u32,
    layout: PixelLayout,
    pixels: &[u8],
    deflate: &dyn Fn(&[u8]) -> Vec<u8>,
) -> Vec<u8> {
    let bpp = layout.channels();
    let stride = width as usize * bpp;
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    let mut row = Vec::with_capacity(stride);
    let mut prev = vec![0u8; stride];
    let mut candidate = Vec::with_capacity(stride);
    let mut best = Vec::with_capacity(stride);
    for y in 0..height as usize {
        row_of(pixels, y, stride, &mut row);
        let mut best_kind = 0u8;
        let mut best_score = u64::MAX;
        for kind in 0..5u8 {
            filter_row(kind, &row, &prev, bpp, &mut candidate);
            let score = filter_score(&candidate);
            if score < best_score {
                best_score = score;
                best_kind = kind;
                std::mem::swap(&mut best, &mut candidate);
            }
        }
        raw.push(best_kind);
        raw.extend_from_slice(&best);
        std::mem::swap(&mut prev, &mut row);
    }
    assemble(width, height, layout.color_type(), &deflate(&raw))
}

/// Encode un bitmap en PNG RGBA 8 bits (couleurs non prémultipliées en sortie).
#[must_use]
pub fn encode_png(bitmap: &Bitmap) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(bitmap.data().len());
    for px in bitmap.data().chunks_exact(4) {
        pixels.extend_from_slice(&unpremultiply(px));
    }
    encode_raw(bitmap.width(), bitmap.height(), 4, 6, &pixels)
}

/// Encode des octets RGB 8 bits (3 par pixel, ligne par ligne) en PNG.
/// Des données trop courtes sont complétées par des zéros.
#[must_use]
pub fn encode_png_rgb(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    encode_raw(width, height, 3, 2, rgb)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::color::Color;

    /// Décodeur de structure minimal : vérifie la signature, le CRC de chaque
    /// chunk et renvoie `(type, données)`.
    fn walk_chunks(png: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        assert_eq!(&png[..8], &PNG_SIGNATURE);
        let mut pos = 8;
        let mut chunks = Vec::new();
        while pos < png.len() {
            let len = u32::from_be_bytes(png[pos..pos + 4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = png[pos + 4..pos + 8].try_into().unwrap();
            let data = &png[pos + 8..pos + 8 + len];
            let crc = u32::from_be_bytes(png[pos + 8 + len..pos + 12 + len].try_into().unwrap());
            assert_eq!(
                crc,
                crc32(&png[pos + 4..pos + 8 + len]),
                "CRC du chunk {kind:?}"
            );
            chunks.push((kind, data.to_vec()));
            pos += 12 + len;
        }
        assert_eq!(pos, png.len(), "taille cohérente");
        chunks
    }

    /// Décodeur zlib stored minimal : inverse de `zlib_stored`.
    fn inflate_stored(z: &[u8]) -> Vec<u8> {
        assert_eq!(&z[..2], &[0x78, 0x01]);
        let mut pos = 2;
        let mut out = Vec::new();
        loop {
            let header = z[pos];
            assert_eq!(header & 0x06, 0, "BTYPE = 00");
            let len = u16::from_le_bytes([z[pos + 1], z[pos + 2]]) as usize;
            let nlen = u16::from_le_bytes([z[pos + 3], z[pos + 4]]);
            assert_eq!(nlen, !(len as u16));
            out.extend_from_slice(&z[pos + 5..pos + 5 + len]);
            pos += 5 + len;
            if header & 1 == 1 {
                break;
            }
        }
        assert_eq!(
            u32::from_be_bytes(z[pos..pos + 4].try_into().unwrap()),
            adler32(&out)
        );
        assert_eq!(pos + 4, z.len());
        out
    }

    #[test]
    fn crc32_and_adler32_known_values() {
        // Valeurs de référence classiques.
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        let big = vec![0xFFu8; 100_000];
        let _ = adler32(&big); // pas de débordement
    }

    #[test]
    fn png_structure_and_pixels_roundtrip() {
        let mut b = Bitmap::new(3, 2);
        b.set_pixel(0, 0, [255, 0, 0, 255]);
        b.set_pixel(1, 0, [0, 128, 0, 128]); // vert à 50 %
        b.set_pixel(2, 1, [0, 0, 0, 0]);
        let png = encode_png(&b);
        let chunks = walk_chunks(&png);
        assert_eq!(chunks.len(), 3);
        assert_eq!(&chunks[0].0, b"IHDR");
        assert_eq!(&chunks[1].0, b"IDAT");
        assert_eq!(&chunks[2].0, b"IEND");
        let ihdr = &chunks[0].1;
        assert_eq!(ihdr.len(), 13);
        assert_eq!(u32::from_be_bytes(ihdr[..4].try_into().unwrap()), 3);
        assert_eq!(u32::from_be_bytes(ihdr[4..8].try_into().unwrap()), 2);
        assert_eq!(&ihdr[8..], &[8, 6, 0, 0, 0]);
        let raw = inflate_stored(&chunks[1].1);
        assert_eq!(raw.len(), 2 * (1 + 3 * 4));
        assert_eq!(raw[0], 0);
        assert_eq!(&raw[1..5], &[255, 0, 0, 255]);
        assert_eq!(&raw[5..9], &[0, 255, 0, 128], "dé-prémultiplié");
        assert_eq!(raw[13], 0);
        assert_eq!(&raw[22..26], &[0, 0, 0, 0]);
        assert!(chunks[2].1.is_empty());
    }

    #[test]
    fn rgb_encoder_and_zero_size() {
        let png = encode_png_rgb(2, 1, &[1, 2, 3, 4, 5, 6]);
        let chunks = walk_chunks(&png);
        assert_eq!(chunks[0].1[9], 2, "type de couleur RGB");
        let raw = inflate_stored(&chunks[1].1);
        assert_eq!(raw, vec![0, 1, 2, 3, 4, 5, 6]);
        // Données trop courtes : complétées, pas de panique.
        let short = encode_png_rgb(2, 2, &[9, 9, 9]);
        let raw = inflate_stored(&walk_chunks(&short)[1].1);
        assert_eq!(raw, vec![0, 9, 9, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        // Bitmap 0×0 : structure valide, flux zlib vide.
        let empty = encode_png(&Bitmap::new(0, 0));
        let chunks = walk_chunks(&empty);
        assert!(inflate_stored(&chunks[1].1).is_empty());
    }

    #[test]
    fn large_image_uses_multiple_stored_blocks() {
        // 300×100 RGBA = 120 300 octets de données brutes : 2 blocs.
        let b = Bitmap::new_filled(300, 100, Color::rgb(0.2, 0.4, 0.6));
        let png = encode_png(&b);
        let chunks = walk_chunks(&png);
        let idat = &chunks[1].1;
        assert_eq!(idat[2] & 1, 0, "premier bloc non final");
        let raw = inflate_stored(idat);
        assert_eq!(raw.len(), 100 * (1 + 300 * 4));
        assert_eq!(&raw[1..5], &[51, 102, 153, 255]);
        // Taille attendue : signature + IHDR + IDAT (2 en-têtes de bloc) + IEND.
        let expected = 8 + (12 + 13) + (12 + 2 + 2 * 5 + raw.len() + 4) + 12;
        assert_eq!(png.len(), expected);
    }

    /// Défiltre les lignes d'un PNG (inverse de `filter_row`).
    fn unfilter(raw: &[u8], stride: usize, bpp: usize) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::with_capacity(raw.len());
        let mut prev = vec![0u8; stride];
        for line in raw.chunks(stride + 1) {
            let kind = line[0];
            let mut row = vec![0u8; stride];
            for i in 0..stride {
                let x = line[1 + i];
                let a = if i >= bpp { row[i - bpp] } else { 0 };
                let b = prev[i];
                let c = if i >= bpp { prev[i - bpp] } else { 0 };
                row[i] = match kind {
                    1 => x.wrapping_add(a),
                    2 => x.wrapping_add(b),
                    3 => x.wrapping_add(u8::midpoint(a, b)),
                    4 => x.wrapping_add(paeth(a, b, c)),
                    _ => x,
                };
            }
            out.extend_from_slice(&row);
            prev = row;
        }
        out
    }

    #[test]
    fn filtered_png_roundtrips_and_shrinks() {
        // Dégradé horizontal : le filtre « Sub » le réduit à des constantes.
        let (w, h) = (64usize, 40usize);
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                rgb[i] = (x * 4) as u8;
                rgb[i + 1] = (y * 6) as u8;
                rgb[i + 2] = 128;
            }
        }
        // Compresseur factice : on ne teste ici que le filtrage.
        let png = encode_png_with(w as u32, h as u32, PixelLayout::Rgb, &rgb, &|raw| {
            zlib_stored(raw)
        });
        let chunks = walk_chunks(&png);
        assert_eq!(chunks[0].1[9], 2, "type de couleur RGB");
        let raw = inflate_stored(&chunks[1].1);
        assert_eq!(raw.len(), h * (1 + w * 3));
        assert!(
            raw.chunks(w * 3 + 1).any(|l| l[0] != 0),
            "au moins une ligne filtrée"
        );
        assert_eq!(unfilter(&raw, w * 3, 3), rgb, "aller-retour du filtrage");
        // Les octets filtrés d'un dégradé sont presque tous nuls : compressés,
        // ils tiennent dans une fraction de la taille brute.
        let zeros = raw.iter().fold(0usize, |n, &b| n + usize::from(b == 0));
        assert!(zeros > raw.len() / 2, "{zeros} zéros sur {}", raw.len());
    }

    #[test]
    fn filtered_png_handles_alpha_and_short_data() {
        let rgba = [255u8, 0, 0, 255, 0, 255, 0, 128];
        let png = encode_png_with(2, 1, PixelLayout::Rgba, &rgba, &|r| zlib_stored(r));
        let chunks = walk_chunks(&png);
        assert_eq!(chunks[0].1[9], 6, "type de couleur RGBA");
        let raw = inflate_stored(&chunks[1].1);
        assert_eq!(unfilter(&raw, 8, 4), rgba);
        // Données trop courtes : complétées par des zéros, pas de panique.
        let png = encode_png_with(2, 2, PixelLayout::Rgb, &[9, 9, 9], &|r| zlib_stored(r));
        let raw = inflate_stored(&walk_chunks(&png)[1].1);
        assert_eq!(
            unfilter(&raw, 6, 3),
            vec![9, 9, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        // Image vide : structure valide.
        let png = encode_png_with(0, 0, PixelLayout::Rgba, &[], &|r| zlib_stored(r));
        assert!(inflate_stored(&walk_chunks(&png)[1].1).is_empty());
    }

    #[test]
    fn paeth_matches_the_specification() {
        assert_eq!(paeth(0, 0, 0), 0);
        assert_eq!(paeth(10, 20, 30), 10, "p = 0 : a est le plus proche");
        assert_eq!(paeth(10, 20, 15), 15, "p = 15 : c est exact");
        assert_eq!(paeth(200, 10, 5), 200);
        assert_eq!(paeth(10, 200, 5), 200);
        assert_eq!(paeth(100, 100, 250), 100);
    }
}
