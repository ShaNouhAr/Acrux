//! Dictionnaire de motifs (T.88 §6.7, segment §7.4.5) et région de
//! demi-teintes (§6.6, segment §7.4.5.2) avec l'image en niveaux de gris de
//! l'annexe C.5.

use super::bitmap::{Bitmap, ComposeOp};
use super::generic::{check_size, decode_generic, decode_generic_mmr, GenericParams, GB_CONTEXTS};
use super::mq::MqDecoder;
use super::{corrupt, read_u16, read_u32, MAX_HALFTONE_CELLS, MAX_PATTERNS};
use crate::ccitt::MmrDecoder;
use acrux_core::Result;

/// Décode un segment de dictionnaire de motifs (§7.4.5.1, §6.7.5) : un
/// bitmap collectif de `(GRAYMAX + 1) × HDPW` pixels de large découpé en
/// motifs.
///
/// # Errors
///
/// `Error::Corrupt` si le segment est tronqué ou trop grand.
// Coordonnées en i64, tailles bornées par check_size (≪ i64::MAX).
#[allow(clippy::cast_possible_wrap)]
pub(crate) fn decode_pattern_dict_segment(data: &[u8]) -> Result<Vec<Bitmap>> {
    let flags = *data
        .first()
        .ok_or_else(|| corrupt("dictionnaire de motifs tronqué"))?;
    let mmr = flags & 1 != 0;
    let template = (flags >> 1) & 3;
    let width_byte = *data
        .get(1)
        .ok_or_else(|| corrupt("dictionnaire de motifs tronqué"))?;
    let pattern_w = usize::from(width_byte);
    let pattern_h = usize::from(
        *data
            .get(2)
            .ok_or_else(|| corrupt("dictionnaire de motifs tronqué"))?,
    );
    let graymax = read_u32(data, 3)? as usize;
    if pattern_w == 0 || pattern_h == 0 || graymax >= MAX_PATTERNS {
        return Err(corrupt("dictionnaire de motifs invalide"));
    }
    let collective_w = (graymax + 1) * pattern_w;
    check_size(collective_w as u64, pattern_h as u64)?;
    let body = data.get(7..).unwrap_or(&[]);
    let collective = if mmr {
        let mut decoder = MmrDecoder::new(body, u32::try_from(collective_w).unwrap_or(1));
        decode_generic_mmr(&mut decoder, collective_w, pattern_h)?
    } else {
        // AT1 = (−HDPW, 0) (§6.7.5) ; HDPW > 128 n'est pas représentable.
        let at1 = (i8::try_from(-i32::from(width_byte)).unwrap_or(i8::MIN), 0);
        let params = GenericParams {
            template,
            at: [at1, (-3, -1), (2, -2), (-2, -2)],
            tpgdon: false,
        };
        let mut mq = MqDecoder::new(body);
        let mut cx = vec![0u8; GB_CONTEXTS];
        decode_generic(collective_w, pattern_h, &params, &mut mq, &mut cx, None)?
    };
    (0..=graymax)
        .map(|i| collective.window((i * pattern_w) as i64, 0, pattern_w, pattern_h))
        .collect()
}

/// Décode un segment de région de demi-teintes (§7.4.5.2, §6.6.5) : `data`
/// commence après les informations de région.
///
/// # Errors
///
/// `Error::Corrupt` si le segment est tronqué ou incohérent.
// HGX et HGY sont signés sur 32 bits (§7.4.5.2.1) ; les autres conversions
// portent sur des tailles bornées par check_size.
#[allow(clippy::cast_possible_wrap)]
pub(crate) fn decode_halftone_region_segment(
    data: &[u8],
    width: usize,
    height: usize,
    patterns: &[Bitmap],
) -> Result<Bitmap> {
    let flags = *data
        .first()
        .ok_or_else(|| corrupt("région de demi-teintes tronquée"))?;
    let mmr = flags & 1 != 0;
    let template = (flags >> 1) & 3;
    let enable_skip = flags & 8 != 0;
    let comb_op = ComposeOp::from_code((flags >> 4) & 7);
    let def_pixel = (flags >> 7) & 1;
    let hgw = read_u32(data, 1)? as usize;
    let hgh = read_u32(data, 5)? as usize;
    let hgx = i64::from(read_u32(data, 9)? as i32);
    let hgy = i64::from(read_u32(data, 13)? as i32);
    let hrx = i64::from(read_u16(data, 17)?);
    let hry = i64::from(read_u16(data, 19)?);
    let body = data.get(21..).unwrap_or(&[]);
    let mut region = Bitmap::new(width, height, def_pixel)?;
    let Some(first) = patterns.first() else {
        return Err(corrupt("région de demi-teintes sans motifs"));
    };
    check_size(hgw as u64, hgh as u64)?;
    if (hgw as u64) * (hgh as u64) > MAX_HALFTONE_CELLS {
        return Err(corrupt("grille de demi-teintes trop grande"));
    }
    let (hpw, hph) = (first.width as i64, first.height as i64);
    // Position d'une cellule de la grille (§6.6.5.2).
    let cell = |m: i64, n: i64| -> (i64, i64) {
        let x = (hgx + m * hry + n * hrx) >> 8;
        let y = (hgy + m * hrx - n * hry) >> 8;
        (x, y)
    };
    // HSKIP (§6.6.5.1).
    let skip = if enable_skip {
        let mut skip = Bitmap::new(hgw, hgh, 0)?;
        for m in 0..hgh {
            for n in 0..hgw {
                let (x, y) = cell(m as i64, n as i64);
                if x + hpw <= 0 || x >= width as i64 || y + hph <= 0 || y >= height as i64 {
                    skip.data[m * hgw + n] = 1;
                }
            }
        }
        Some(skip)
    } else {
        None
    };
    // Image en niveaux de gris (annexe C.5) : plans de bits du poids fort
    // au poids faible, code de Gray.
    let bits_per_value = {
        let mut b = 0u32;
        while (1usize << b) < patterns.len() {
            b += 1;
        }
        b.max(1)
    };
    let mut gray: Vec<u32> = vec![0; hgw * hgh];
    let params = GenericParams {
        template,
        at: [
            (if template <= 1 { 3 } else { 2 }, -1),
            (-3, -1),
            (2, -2),
            (-2, -2),
        ],
        tpgdon: false,
    };
    let mut mq = MqDecoder::new(body);
    let mut cx = vec![0u8; GB_CONTEXTS];
    let mut mmr_decoder = MmrDecoder::new(body, u32::try_from(hgw).unwrap_or(1));
    let mut prev_plane: Option<Bitmap> = None;
    for j in (0..bits_per_value).rev() {
        let plane = if mmr {
            decode_generic_mmr(&mut mmr_decoder, hgw, hgh)?
        } else {
            decode_generic(hgw, hgh, &params, &mut mq, &mut cx, skip.as_ref())?
        };
        // GRAYVAL bit j = plane[j] XOR bit j+1.
        let decoded: Vec<u8> = match &prev_plane {
            None => plane.data.clone(),
            Some(prev) => plane
                .data
                .iter()
                .zip(&prev.data)
                .map(|(&p, &q)| p ^ q)
                .collect(),
        };
        for (g, &bit) in gray.iter_mut().zip(&decoded) {
            *g |= u32::from(bit) << j;
        }
        prev_plane = Some(Bitmap {
            width: hgw,
            height: hgh,
            data: decoded,
        });
    }
    // Rendu (§6.6.5.2).
    for m in 0..hgh {
        for n in 0..hgw {
            if skip.as_ref().is_some_and(|s| s.data[m * hgw + n] != 0) {
                continue;
            }
            let (x, y) = cell(m as i64, n as i64);
            let value = (gray[m * hgw + n] as usize).min(patterns.len() - 1);
            region.compose(&patterns[value], x, y, comb_op);
        }
    }
    Ok(region)
}
