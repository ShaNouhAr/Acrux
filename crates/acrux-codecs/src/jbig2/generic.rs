//! Procédures de décodage de région générique (T.88 §6.2) et de région de
//! raffinement (§6.3).
//!
//! Région générique : codage arithmétique avec les gabarits GB 0 à 3
//! (figures 4 à 7), pixels adaptatifs A1 à A4 (§6.2.5.4), prédiction
//! typique TPGDON (§6.2.5.7, contextes 0x9B25, 0x0795, 0x00E5, 0x0195) ;
//! ou codage MMR (§6.2.6) confié au décodeur T.6 de [`crate::ccitt`].
//!
//! Formation du contexte (figure 8) : les pixels du gabarit sont lus ligne
//! par ligne de haut en bas et de gauche à droite, le premier pixel donnant
//! le bit de poids fort et le pixel immédiatement à gauche du pixel courant
//! le bit de poids faible ; les pixels adaptatifs occupent la place de leur
//! position nominale.

use super::bitmap::Bitmap;
use super::mq::{Context, MqDecoder};
use super::{corrupt, MAX_BITMAP_PIXELS};
use crate::ccitt::{paint_changes, MmrDecoder};
use acrux_core::Result;

/// Taille du tableau de contextes d'une région générique (16 bits).
pub(crate) const GB_CONTEXTS: usize = 1 << 16;

/// Taille du tableau de contextes d'une région de raffinement (13 bits).
pub(crate) const GR_CONTEXTS: usize = 1 << 13;

/// Paramètres d'une région générique arithmétique (§6.2.2).
#[derive(Debug, Clone, Copy)]
pub(crate) struct GenericParams {
    /// GBTEMPLATE, 0 à 3.
    pub template: u8,
    /// Pixels adaptatifs A1 à A4 (seul A1 sert pour les gabarits 1 à 3).
    pub at: [(i8, i8); 4],
    /// TPGDON.
    pub tpgdon: bool,
}

/// Description d'un gabarit : fenêtres glissantes sur les lignes y−2, y−1
/// et y (décalage de départ, largeur en bits, position dans le contexte) et
/// emplacements des pixels adaptatifs.
struct Template {
    /// (dx du premier pixel, nombre de pixels, décalage dans le contexte).
    row2: Option<(i64, u32, u32)>,
    row1: (i64, u32, u32),
    /// Nombre de pixels de la ligne courante (à gauche du pixel courant).
    row0: u32,
    /// Pour chaque pixel adaptatif utilisé : (position nominale, bit du contexte).
    at: &'static [((i8, i8), u32)],
    /// Contexte de la décision SLTP (§6.2.5.7).
    tpgd_context: usize,
}

const TEMPLATES: [Template; 4] = [
    // GB 0 (figure 4) : 16 pixels.
    Template {
        row2: Some((-2, 5, 11)),
        row1: (-3, 7, 4),
        row0: 4,
        at: &[((3, -1), 4), ((-3, -1), 10), ((2, -2), 11), ((-2, -2), 15)],
        tpgd_context: 0x9B25,
    },
    // GB 1 (figure 5) : 13 pixels.
    Template {
        row2: Some((-1, 4, 9)),
        row1: (-2, 6, 3),
        row0: 3,
        at: &[((3, -1), 3)],
        tpgd_context: 0x0795,
    },
    // GB 2 (figure 6) : 10 pixels.
    Template {
        row2: Some((-1, 3, 7)),
        row1: (-2, 5, 2),
        row0: 2,
        at: &[((2, -1), 2)],
        tpgd_context: 0x00E5,
    },
    // GB 3 (figure 7) : 10 pixels sur deux lignes.
    Template {
        row2: None,
        row1: (-3, 6, 4),
        row0: 4,
        at: &[((2, -1), 4)],
        tpgd_context: 0x0195,
    },
];

/// Pixel `x` d'une ligne, 0 hors de la ligne.
#[inline]
fn px(row: &[u8], x: i64) -> u32 {
    usize::try_from(x)
        .ok()
        .and_then(|i| row.get(i))
        .map_or(0, |&p| u32::from(p))
}

/// Valeur initiale (x = 0) d'une fenêtre couvrant les pixels `x + dx` à
/// `x + dx + len − 1` d'une ligne.
fn window_init(row: &[u8], dx: i64, len: u32) -> u32 {
    let mut w = 0;
    for i in 0..i64::from(len) {
        w = (w << 1) | px(row, dx + i);
    }
    w
}

/// Décode une région générique en codage arithmétique (§6.2.5.7).
///
/// `cx` est le tableau des contextes (persistant entre les symboles d'un
/// dictionnaire) ; `skip`, s'il est présent, désigne les pixels à ne pas
/// décoder (§6.6.5.1, HENSKIP).
///
/// # Errors
///
/// `Error::Corrupt` si l'image dépasse les limites ou si le gabarit est
/// invalide.
pub(crate) fn decode_generic(
    width: usize,
    height: usize,
    params: &GenericParams,
    mq: &mut MqDecoder<'_>,
    cx: &mut [Context],
    skip: Option<&Bitmap>,
) -> Result<Bitmap> {
    let mut bitmap = Bitmap::new(width, height, 0)?;
    if width == 0 || height == 0 {
        return Ok(bitmap);
    }
    let template = TEMPLATES
        .get(usize::from(params.template))
        .ok_or_else(|| corrupt("gabarit de région générique invalide"))?;
    if cx.len() < GB_CONTEXTS {
        return Err(corrupt("contextes de région générique insuffisants"));
    }
    // Pixels adaptatifs hors de leur position nominale (et masque des bits
    // à remplacer). Un pixel adaptatif ne peut désigner que des pixels déjà
    // décodés : dy < 0, ou dy = 0 et dx < 0 (§6.2.5.4).
    let mut at_fix: Vec<((i64, i64), u32)> = Vec::new();
    let mut at_mask: u32 = 0;
    for (i, &(nominal, bit)) in template.at.iter().enumerate() {
        let (dx, dy) = params.at[i];
        if (dx, dy) != nominal {
            if dy > 0 || (dy == 0 && dx >= 0) {
                return Err(corrupt("pixel adaptatif invalide"));
            }
            at_fix.push(((i64::from(dx), i64::from(dy)), bit));
            at_mask |= 1 << bit;
        }
    }
    let empty: Vec<u8> = vec![0; width];
    let mut ltp = false;
    for y in 0..height {
        if params.tpgdon {
            let sltp = mq.decode(&mut cx[template.tpgd_context]);
            if sltp == 1 {
                ltp = !ltp;
            }
            if ltp {
                // Ligne « typique » : copie de la ligne précédente.
                if y > 0 {
                    let (above, cur) = bitmap.data.split_at_mut(y * width);
                    cur[..width].copy_from_slice(&above[(y - 1) * width..]);
                }
                continue;
            }
        }
        let (above, cur) = bitmap.data.split_at_mut(y * width);
        let row = &mut cur[..width];
        let row1: &[u8] = if y >= 1 {
            &above[(y - 1) * width..]
        } else {
            &empty
        };
        let row2: &[u8] = if y >= 2 {
            &above[(y - 2) * width..(y - 1) * width]
        } else {
            &empty
        };
        let skip_row = skip
            .filter(|s| s.width == width && y < s.height)
            .map(|s| s.row(y));
        decode_generic_row(
            template, &at_fix, at_mask, row, row1, row2, above, y, width, mq, cx, skip_row,
        );
    }
    Ok(bitmap)
}

// Les coordonnées sont manipulées en i64 (décalages négatifs des pixels
// adaptatifs) ; les tailles sont bornées par MAX_BITMAP_PIXELS, bien en deçà
// de i64::MAX.
#[allow(clippy::too_many_arguments, clippy::cast_possible_wrap)]
fn decode_generic_row(
    template: &Template,
    at_fix: &[((i64, i64), u32)],
    at_mask: u32,
    row: &mut [u8],
    row1: &[u8],
    row2: &[u8],
    above: &[u8],
    y: usize,
    width: usize,
    mq: &mut MqDecoder<'_>,
    cx: &mut [Context],
    skip_row: Option<&[u8]>,
) {
    let (r2_dx, r2_len, r2_shift) = template.row2.unwrap_or((0, 0, 0));
    let (r1_dx, r1_len, r1_shift) = template.row1;
    let r0_len = template.row0;
    let mask2 = (1u32 << r2_len) - 1;
    let mask1 = (1u32 << r1_len) - 1;
    let mask0 = (1u32 << r0_len) - 1;
    let mut w2 = if r2_len > 0 {
        window_init(row2, r2_dx, r2_len)
    } else {
        0
    };
    let mut w1 = window_init(row1, r1_dx, r1_len);
    let mut w0: u32 = 0;
    // Décalage du prochain pixel entrant dans chaque fenêtre.
    let next2 = r2_dx + i64::from(r2_len);
    let next1 = r1_dx + i64::from(r1_len);
    let yi = y as i64;
    for x in 0..width {
        let xi = x as i64;
        let mut context = (w2 << r2_shift) | (w1 << r1_shift) | w0;
        if at_mask != 0 {
            context &= !at_mask;
            for &((dx, dy), bit) in at_fix {
                let v = if dy == 0 {
                    px(row, xi + dx)
                } else {
                    match usize::try_from(yi + dy) {
                        Ok(yy) => px(&above[yy * width..][..width], xi + dx),
                        Err(_) => 0,
                    }
                };
                context |= v << bit;
            }
        }
        let pixel = match skip_row {
            Some(s) if s[x] != 0 => 0,
            _ => mq.decode(&mut cx[context as usize]),
        };
        row[x] = pixel;
        w2 = ((w2 << 1) | px(row2, xi + next2)) & mask2;
        w1 = ((w1 << 1) | px(row1, xi + next1)) & mask1;
        w0 = ((w0 << 1) | u32::from(pixel)) & mask0;
    }
}

/// Décode une région générique en codage MMR (§6.2.6) : `height` lignes
/// lues sur le décodeur T.6 fourni, qui peut être partagé entre plusieurs
/// plans (§C.5). Les lignes manquantes restent blanches.
///
/// # Errors
///
/// `Error::Corrupt` si l'image dépasse les limites.
pub(crate) fn decode_generic_mmr(
    mmr: &mut MmrDecoder<'_>,
    width: usize,
    height: usize,
) -> Result<Bitmap> {
    let mut bitmap = Bitmap::new(width, height, 0)?;
    for y in 0..height {
        let Some(changes) = mmr.next_row() else {
            break;
        };
        paint_changes(&mut bitmap.data[y * width..(y + 1) * width], changes);
    }
    Ok(bitmap)
}

/// Paramètres d'une région de raffinement (§6.3.2).
#[derive(Debug, Clone, Copy)]
pub(crate) struct RefinementParams {
    /// GRTEMPLATE, 0 ou 1.
    pub template: u8,
    /// Pixels adaptatifs A1 (image courante) et A2 (référence), gabarit 0.
    pub at: [(i8, i8); 2],
    /// TPGRON.
    pub tpgron: bool,
}

/// Décode une région de raffinement (§6.3.5.6) de taille `width × height`
/// à partir de `reference`, décalée de `(dx, dy)` (GRREFERENCEDX/DY).
///
/// Gabarit 0 (figure 12) : 13 pixels — image courante (0, −1), (1, −1),
/// (−1, 0) et A1 ; référence les neuf pixels centrés sur le pixel
/// correspondant, A2 remplaçant (−1, −1). Gabarit 1 (figure 13) : 10 pixels
/// — image courante (−1, −1), (0, −1), (1, −1), (−1, 0) ; référence (0, −1),
/// (−1, 0), (0, 0), (1, 0), (0, 1), (1, 1). Contextes SLTP : 0x0008 et 0x0080.
///
/// # Errors
///
/// `Error::Corrupt` si l'image dépasse les limites ou si le gabarit est
/// invalide.
// Huit paramètres : ceux de la procédure de la norme (tableau 6) ; les
// coordonnées sont en i64 avec des tailles bornées par MAX_BITMAP_PIXELS.
#[allow(clippy::too_many_arguments, clippy::cast_possible_wrap)]
pub(crate) fn decode_refinement(
    width: usize,
    height: usize,
    params: RefinementParams,
    reference: &Bitmap,
    dx: i64,
    dy: i64,
    mq: &mut MqDecoder<'_>,
    cx: &mut [Context],
) -> Result<Bitmap> {
    let mut bitmap = Bitmap::new(width, height, 0)?;
    if params.template > 1 {
        return Err(corrupt("gabarit de raffinement invalide"));
    }
    if cx.len() < GR_CONTEXTS {
        return Err(corrupt("contextes de raffinement insuffisants"));
    }
    let (a1, a2) = (
        (i64::from(params.at[0].0), i64::from(params.at[0].1)),
        (i64::from(params.at[1].0), i64::from(params.at[1].1)),
    );
    let mut ltp = false;
    for y in 0..height {
        if params.tpgron {
            let context = if params.template == 0 { 0x0008 } else { 0x0080 };
            if mq.decode(&mut cx[context]) == 1 {
                ltp = !ltp;
            }
        }
        for x in 0..width {
            let (xi, yi) = (x as i64, y as i64);
            let (rx, ry) = (xi - dx, yi - dy);
            if ltp {
                // Prédiction typique : voisinage 3 × 3 uniforme dans la référence.
                let mut sum = 0u32;
                for j in -1..=1 {
                    for i in -1..=1 {
                        sum += u32::from(reference.get(rx + i, ry + j));
                    }
                }
                if sum == 0 {
                    continue;
                }
                if sum == 9 {
                    bitmap.data[y * width + x] = 1;
                    continue;
                }
            }
            let cur = |ddx: i64, ddy: i64| u32::from(bitmap.get(xi + ddx, yi + ddy));
            let rf = |ddx: i64, ddy: i64| u32::from(reference.get(rx + ddx, ry + ddy));
            let context = if params.template == 0 {
                (cur(0, -1) << 12)
                    | (cur(1, -1) << 11)
                    | (cur(-1, 0) << 10)
                    | (cur(a1.0, a1.1) << 9)
                    | (rf(0, -1) << 8)
                    | (rf(1, -1) << 7)
                    | (rf(-1, 0) << 6)
                    | (rf(0, 0) << 5)
                    | (rf(1, 0) << 4)
                    | (rf(-1, 1) << 3)
                    | (rf(0, 1) << 2)
                    | (rf(1, 1) << 1)
                    | rf(a2.0, a2.1)
            } else {
                (cur(-1, -1) << 9)
                    | (cur(0, -1) << 8)
                    | (cur(1, -1) << 7)
                    | (cur(-1, 0) << 6)
                    | (rf(0, -1) << 5)
                    | (rf(-1, 0) << 4)
                    | (rf(0, 0) << 3)
                    | (rf(1, 0) << 2)
                    | (rf(0, 1) << 1)
                    | rf(1, 1)
            };
            let pixel = mq.decode(&mut cx[context as usize]);
            bitmap.data[y * width + x] = pixel;
        }
    }
    Ok(bitmap)
}

/// Vérifie qu'une taille de région est raisonnable avant toute allocation.
pub(crate) fn check_size(width: u64, height: u64) -> Result<()> {
    match width.checked_mul(height) {
        Some(p) if p <= MAX_BITMAP_PIXELS => Ok(()),
        _ => Err(corrupt("région trop grande")),
    }
}
