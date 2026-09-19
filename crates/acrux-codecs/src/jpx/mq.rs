//! Décodeur arithmétique MQ (ISO/IEC 15444-1 annexe C).
//!
//! C'est le même codeur que celui de JBIG2 (ITU-T T.88 annexe E) ; il est
//! réimplémenté ici indépendamment, d'après les procédures DECODE, BYTEIN,
//! INITDEC et RENORMD (C.3, figures C.15 à C.20), dans la version où le
//! registre C est comparé par sa moitié haute `Chigh` à la probabilité Qe.
//!
//! Les 47 états de la table C.2 sont codés dans [`QE`] ; un contexte est un
//! octet `indice << 1 | MPS` (voir [`Context`]).

/// Une ligne de la table C.2 : Qe, NMPS, NLPS, SWITCH.
#[derive(Clone, Copy)]
pub(super) struct QeEntry {
    /// Estimation de probabilité du symbole le moins probable.
    pub qe: u32,
    /// État suivant après un MPS.
    pub nmps: u8,
    /// État suivant après un LPS.
    pub nlps: u8,
    /// Vrai si un LPS inverse le sens du MPS.
    pub switch: bool,
}

const fn e(qe: u32, state_after_mps: u8, state_on_lps: u8, switch: u8) -> QeEntry {
    QeEntry {
        qe,
        nmps: state_after_mps,
        nlps: state_on_lps,
        switch: switch == 1,
    }
}

/// Table C.2 – valeurs de Qe et transitions d'état.
pub(super) const QE: [QeEntry; 47] = [
    e(0x5601, 1, 1, 1),
    e(0x3401, 2, 6, 0),
    e(0x1801, 3, 9, 0),
    e(0x0AC1, 4, 12, 0),
    e(0x0521, 5, 29, 0),
    e(0x0221, 38, 33, 0),
    e(0x5601, 7, 6, 1),
    e(0x5401, 8, 14, 0),
    e(0x4801, 9, 14, 0),
    e(0x3801, 10, 14, 0),
    e(0x3001, 11, 17, 0),
    e(0x2401, 12, 18, 0),
    e(0x1C01, 13, 20, 0),
    e(0x1601, 29, 21, 0),
    e(0x5601, 15, 14, 1),
    e(0x5401, 16, 14, 0),
    e(0x5101, 17, 15, 0),
    e(0x4801, 18, 16, 0),
    e(0x3801, 19, 17, 0),
    e(0x3401, 20, 18, 0),
    e(0x3001, 21, 19, 0),
    e(0x2801, 22, 19, 0),
    e(0x2401, 23, 20, 0),
    e(0x2201, 24, 21, 0),
    e(0x1C01, 25, 22, 0),
    e(0x1801, 26, 23, 0),
    e(0x1601, 27, 24, 0),
    e(0x1401, 28, 25, 0),
    e(0x1201, 29, 26, 0),
    e(0x1101, 30, 27, 0),
    e(0x0AC1, 31, 28, 0),
    e(0x09C1, 32, 29, 0),
    e(0x08A1, 33, 30, 0),
    e(0x0521, 34, 31, 0),
    e(0x0441, 35, 32, 0),
    e(0x02A1, 36, 33, 0),
    e(0x0221, 37, 34, 0),
    e(0x0141, 38, 35, 0),
    e(0x0111, 39, 36, 0),
    e(0x0085, 40, 37, 0),
    e(0x0049, 41, 38, 0),
    e(0x0025, 42, 39, 0),
    e(0x0015, 43, 40, 0),
    e(0x0009, 44, 41, 0),
    e(0x0005, 45, 42, 0),
    e(0x0001, 45, 43, 0),
    e(0x5601, 46, 46, 0),
];

/// État d'un contexte : indice dans [`QE`] (bits 1 à 6) et MPS (bit 0).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Context(u8);

impl Context {
    /// Contexte dans l'état `index` avec le MPS `mps`.
    pub const fn new(index: u8, mps: u8) -> Self {
        Context((index << 1) | (mps & 1))
    }

    /// Indice d'état dans [`QE`].
    #[inline]
    pub fn index(self) -> usize {
        usize::from(self.0 >> 1)
    }

    /// Symbole le plus probable (0 ou 1).
    #[inline]
    pub fn mps(self) -> u32 {
        u32::from(self.0 & 1)
    }
}

/// Décodeur MQ sur un segment de mots de code. Au-delà de la fin des données
/// le décodeur lit des octets `0xFF` (C.3.4 : un marqueur est traité comme
/// une suite infinie de 1), ce qui garantit une terminaison propre même sur
/// un segment tronqué.
pub(super) struct MqDecoder<'a> {
    data: &'a [u8],
    /// BP : position de l'octet courant.
    bp: usize,
    c: u32,
    a: u32,
    ct: u32,
}

impl<'a> MqDecoder<'a> {
    /// Procédure INITDEC (figure C.20).
    pub fn new(data: &'a [u8]) -> Self {
        let mut d = MqDecoder {
            data,
            bp: 0,
            c: 0,
            a: 0,
            ct: 0,
        };
        d.c = u32::from(d.byte_at(0)) << 16;
        d.byte_in();
        d.c <<= 7;
        d.ct = d.ct.wrapping_sub(7);
        d.a = 0x8000;
        d
    }

    #[inline]
    fn byte_at(&self, pos: usize) -> u8 {
        self.data.get(pos).copied().unwrap_or(0xFF)
    }

    /// Procédure BYTEIN (figure C.19) : lit l'octet suivant en tenant compte
    /// du bourrage après `0xFF`.
    #[inline]
    fn byte_in(&mut self) {
        if self.byte_at(self.bp) == 0xFF {
            if self.byte_at(self.bp + 1) > 0x8F {
                // Marqueur : on n'avance plus et on fournit des 1.
                self.c = self.c.wrapping_add(0xFF00);
                self.ct = 8;
            } else {
                self.bp += 1;
                self.c = self.c.wrapping_add(u32::from(self.byte_at(self.bp)) << 9);
                self.ct = 7;
            }
        } else {
            self.bp += 1;
            self.c = self.c.wrapping_add(u32::from(self.byte_at(self.bp)) << 8);
            self.ct = 8;
        }
    }

    /// Procédure DECODE (figure C.15) avec LPSEXCHANGE / MPSEXCHANGE
    /// (figures C.16, C.17) et RENORMD (C.18).
    // Les conversions `as u8` portent sur un MPS qui vaut 0 ou 1.
    #[allow(clippy::cast_possible_truncation)]
    #[inline]
    pub fn decode(&mut self, cx: &mut Context) -> u32 {
        let entry = QE[cx.index()];
        let qe = entry.qe;
        let mps = cx.mps();
        self.a = self.a.wrapping_sub(qe);
        let d;
        if ((self.c >> 16) & 0xFFFF) < qe {
            // Chigh < Qe : LPSEXCHANGE.
            if self.a < qe {
                self.a = qe;
                d = mps;
                *cx = Context::new(entry.nmps, mps as u8);
            } else {
                self.a = qe;
                d = 1 - mps;
                let new_mps = if entry.switch { 1 - mps } else { mps };
                *cx = Context::new(entry.nlps, new_mps as u8);
            }
            self.renorm();
        } else {
            self.c = self.c.wrapping_sub(qe << 16);
            if self.a & 0x8000 == 0 {
                // MPSEXCHANGE.
                if self.a < qe {
                    d = 1 - mps;
                    let new_mps = if entry.switch { 1 - mps } else { mps };
                    *cx = Context::new(entry.nlps, new_mps as u8);
                } else {
                    d = mps;
                    *cx = Context::new(entry.nmps, mps as u8);
                }
                self.renorm();
            } else {
                d = mps;
            }
        }
        d
    }

    /// Procédure RENORMD (figure C.18).
    #[inline]
    fn renorm(&mut self) {
        loop {
            if self.ct == 0 {
                self.byte_in();
            }
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            if self.a & 0x8000 != 0 {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vecteur de test de l'annexe H.2 de T.88 (identique à celui de
    /// 15444-1) : 256 octets de données codées avec un seul contexte, dont
    /// on connaît les 256 octets décodés.
    const ENCODED: [u8; 30] = [
        0x84, 0xC7, 0x3B, 0xFC, 0xE1, 0xA1, 0x43, 0x04, 0x02, 0x20, 0x00, 0x00, 0x41, 0x0D, 0xBB,
        0x86, 0xF4, 0x31, 0x7F, 0xFF, 0x88, 0xFF, 0x37, 0x47, 0x1A, 0xDB, 0x6A, 0xDF, 0xFF, 0xAC,
    ];
    const DECODED: [u8; 32] = [
        0x00, 0x02, 0x00, 0x51, 0x00, 0x00, 0x00, 0xC0, 0x03, 0x52, 0x87, 0x2A, 0xAA, 0xAA, 0xAA,
        0xAA, 0x82, 0xC0, 0x20, 0x00, 0xFC, 0xD7, 0x9E, 0xF6, 0xBF, 0x7F, 0xED, 0x90, 0x4F, 0x46,
        0xA3, 0xBF,
    ];

    #[test]
    #[allow(clippy::unwrap_used)]
    fn decodes_the_standard_test_vector() {
        let mut dec = MqDecoder::new(&ENCODED);
        let mut cx = Context::default();
        let mut out = Vec::new();
        for _ in 0..32 {
            let mut byte = 0u8;
            for _ in 0..8 {
                byte = (byte << 1) | u8::try_from(dec.decode(&mut cx)).unwrap();
            }
            out.push(byte);
        }
        assert_eq!(out, DECODED);
    }

    #[test]
    fn empty_input_decodes_without_panicking() {
        let mut dec = MqDecoder::new(&[]);
        let mut cx = Context::new(46, 0);
        for _ in 0..1000 {
            let _ = dec.decode(&mut cx);
        }
    }
}
