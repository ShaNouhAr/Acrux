//! Images binaires JBIG2 : un octet par pixel (0 ou 1, 1 = noir comme dans
//! T.88 §2.4) et opérateurs de composition (T.88 §6.4.5, tableau 5 ; §7.4.1.5).

use super::{corrupt, MAX_BITMAP_PIXELS};
use acrux_core::Result;

/// Opérateur de combinaison (T.88 §7.4.1.5, §7.4.8.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposeOp {
    Or,
    And,
    Xor,
    Xnor,
    Replace,
}

impl ComposeOp {
    /// Valeur codée sur 3 bits : 0 OR, 1 AND, 2 XOR, 3 XNOR, 4 REPLACE.
    pub(crate) fn from_code(code: u8) -> ComposeOp {
        match code {
            1 => ComposeOp::And,
            2 => ComposeOp::Xor,
            3 => ComposeOp::Xnor,
            4 => ComposeOp::Replace,
            _ => ComposeOp::Or,
        }
    }
}

/// Image binaire, un octet par pixel, lignes consécutives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Bitmap {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl Bitmap {
    /// Image remplie de `value` (0 ou 1).
    ///
    /// # Errors
    ///
    /// `Error::Corrupt` si `width × height` dépasse [`MAX_BITMAP_PIXELS`].
    pub(crate) fn new(width: usize, height: usize, value: u8) -> Result<Bitmap> {
        let pixels = (width as u64).checked_mul(height as u64);
        match pixels {
            Some(p) if p <= MAX_BITMAP_PIXELS => Ok(Bitmap {
                width,
                height,
                data: vec![value; width * height],
            }),
            _ => Err(corrupt("image trop grande")),
        }
    }

    /// Pixel en `(x, y)`, 0 hors de l'image.
    pub(crate) fn get(&self, x: i64, y: i64) -> u8 {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return 0;
        };
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.data[y * self.width + x]
    }

    pub(crate) fn row(&self, y: usize) -> &[u8] {
        &self.data[y * self.width..(y + 1) * self.width]
    }

    /// Dessine `src` avec son coin supérieur gauche en `(x0, y0)`, en
    /// coupant ce qui déborde.
    pub(crate) fn compose(&mut self, src: &Bitmap, x0: i64, y0: i64, op: ComposeOp) {
        let Some((dst_x, src_x, w)) = overlap(x0, src.width, self.width) else {
            return;
        };
        let Some((dst_y, src_y, h)) = overlap(y0, src.height, self.height) else {
            return;
        };
        for dy in 0..h {
            let s = &src.data[(src_y + dy) * src.width + src_x..][..w];
            let d = &mut self.data[(dst_y + dy) * self.width + dst_x..][..w];
            match op {
                ComposeOp::Or => d.iter_mut().zip(s).for_each(|(a, &b)| *a |= b),
                ComposeOp::And => d.iter_mut().zip(s).for_each(|(a, &b)| *a &= b),
                ComposeOp::Xor => d.iter_mut().zip(s).for_each(|(a, &b)| *a ^= b),
                ComposeOp::Xnor => d.iter_mut().zip(s).for_each(|(a, &b)| *a = 1 - (*a ^ b)),
                ComposeOp::Replace => d.copy_from_slice(s),
            }
        }
    }

    /// Copie la fenêtre `(x0, y0, w, h)` de l'image (0 hors de l'image).
    pub(crate) fn window(&self, x0: i64, y0: i64, w: usize, h: usize) -> Result<Bitmap> {
        let mut out = Bitmap::new(w, h, 0)?;
        if let (Some((sx, dx, cw)), Some((sy, dy, ch))) =
            (overlap(x0, w, self.width), overlap(y0, h, self.height))
        {
            for row in 0..ch {
                let s = &self.data[(sy + row) * self.width + sx..][..cw];
                out.data[(dy + row) * w + dx..][..cw].copy_from_slice(s);
            }
        }
        Ok(out)
    }

    /// Lignes tassées 1 bit par pixel, poids fort en tête, 1 = noir.
    pub(crate) fn packed(&self) -> Vec<u8> {
        let stride = self.width.div_ceil(8);
        let mut out = vec![0u8; stride * self.height];
        for y in 0..self.height {
            let row = self.row(y);
            let dst = &mut out[y * stride..][..stride];
            for (x, &p) in row.iter().enumerate() {
                if p != 0 {
                    dst[x / 8] |= 0x80 >> (x % 8);
                }
            }
        }
        out
    }
}

/// Intersection d'un segment `[start, start + len)` avec `[0, limit)` :
/// rend (début dans la destination, début dans la source, longueur).
fn overlap(start: i64, len: usize, limit: usize) -> Option<(usize, usize, usize)> {
    let end = start.checked_add(i64::try_from(len).ok()?)?;
    let limit_i = i64::try_from(limit).ok()?;
    let a = start.max(0);
    let b = end.min(limit_i);
    if a >= b {
        return None;
    }
    Some((
        usize::try_from(a).ok()?,
        usize::try_from(a - start).ok()?,
        usize::try_from(b - a).ok()?,
    ))
}
