//! Décodeur arithmétique MQ (T.88 annexe E, version « conventions
//! logicielles » de la figure E.20 et suivantes) et procédures de décodage
//! d'entiers (T.88 annexe A).

/// Ligne de la table des probabilités Qe (T.88 tableau E.1) : Qe, NMPS,
/// NLPS, SWITCH.
pub(crate) struct QeEntry {
    pub(crate) qe: u32,
    pub(crate) nmps: u8,
    pub(crate) nlps: u8,
    pub(crate) switch: bool,
}

const fn qe(qe: u32, next_state_mps: u8, next_state_after_lps: u8, switch: u8) -> QeEntry {
    QeEntry {
        qe,
        nmps: next_state_mps,
        nlps: next_state_after_lps,
        switch: switch == 1,
    }
}

/// Tableau E.1 : 47 états.
pub(crate) const QE_TABLE: [QeEntry; 47] = [
    qe(0x5601, 1, 1, 1),
    qe(0x3401, 2, 6, 0),
    qe(0x1801, 3, 9, 0),
    qe(0x0AC1, 4, 12, 0),
    qe(0x0521, 5, 29, 0),
    qe(0x0221, 38, 33, 0),
    qe(0x5601, 7, 6, 1),
    qe(0x5401, 8, 14, 0),
    qe(0x4801, 9, 14, 0),
    qe(0x3801, 10, 14, 0),
    qe(0x3001, 11, 17, 0),
    qe(0x2401, 12, 18, 0),
    qe(0x1C01, 13, 20, 0),
    qe(0x1601, 29, 21, 0),
    qe(0x5601, 15, 14, 1),
    qe(0x5401, 16, 14, 0),
    qe(0x5101, 17, 15, 0),
    qe(0x4801, 18, 16, 0),
    qe(0x3801, 19, 17, 0),
    qe(0x3401, 20, 18, 0),
    qe(0x3001, 21, 19, 0),
    qe(0x2801, 22, 19, 0),
    qe(0x2401, 23, 20, 0),
    qe(0x2201, 24, 21, 0),
    qe(0x1C01, 25, 22, 0),
    qe(0x1801, 26, 23, 0),
    qe(0x1601, 27, 24, 0),
    qe(0x1401, 28, 25, 0),
    qe(0x1201, 29, 26, 0),
    qe(0x1101, 30, 27, 0),
    qe(0x0AC1, 31, 28, 0),
    qe(0x09C1, 32, 29, 0),
    qe(0x08A1, 33, 30, 0),
    qe(0x0521, 34, 31, 0),
    qe(0x0441, 35, 32, 0),
    qe(0x02A1, 36, 33, 0),
    qe(0x0221, 37, 34, 0),
    qe(0x0141, 38, 35, 0),
    qe(0x0111, 39, 36, 0),
    qe(0x0085, 40, 37, 0),
    qe(0x0049, 41, 38, 0),
    qe(0x0025, 42, 39, 0),
    qe(0x0015, 43, 40, 0),
    qe(0x0009, 44, 41, 0),
    qe(0x0005, 45, 42, 0),
    qe(0x0001, 45, 43, 0),
    qe(0x5601, 46, 46, 0),
];

/// État d'un contexte : indice dans [`QE_TABLE`] (bits 1 à 6) et MPS (bit 0).
pub(crate) type Context = u8;

/// Décodeur MQ (T.88 §E.3). Au-delà des données, il lit des octets 0xFF,
/// comme le prescrit la convention de marqueur (E.3.4) : un flux tronqué se
/// décode sans panique, en produisant des symboles arbitraires mais bornés
/// par les boucles appelantes.
pub(crate) struct MqDecoder<'a> {
    data: &'a [u8],
    /// BP : indice de l'octet courant.
    bp: usize,
    c: u32,
    a: u32,
    ct: i32,
}

impl<'a> MqDecoder<'a> {
    /// INITDEC (figure E.20).
    pub(crate) fn new(data: &'a [u8]) -> Self {
        let mut d = MqDecoder {
            data,
            bp: 0,
            c: 0,
            a: 0,
            ct: 0,
        };
        d.c = u32::from(d.byte(0)) << 16;
        d.byte_in();
        d.c <<= 7;
        d.ct -= 7;
        d.a = 0x8000;
        d
    }

    fn byte(&self, i: usize) -> u8 {
        self.data.get(i).copied().unwrap_or(0xFF)
    }

    /// BYTEIN (figure E.19).
    fn byte_in(&mut self) {
        if self.byte(self.bp) == 0xFF {
            if self.byte(self.bp + 1) > 0x8F {
                // Marqueur : on ne le dépasse pas.
                self.c = self.c.wrapping_add(0xFF00);
                self.ct = 8;
            } else {
                self.bp += 1;
                self.c = self.c.wrapping_add(u32::from(self.byte(self.bp)) << 9);
                self.ct = 7;
            }
        } else {
            self.bp += 1;
            self.c = self.c.wrapping_add(u32::from(self.byte(self.bp)) << 8);
            self.ct = 8;
        }
    }

    /// DECODE (figure E.17) avec MPSEXCHANGE / LPSEXCHANGE (E.18, E.16)
    /// et RENORMD (E.21).
    pub(crate) fn decode(&mut self, cx: &mut Context) -> u8 {
        let index = usize::from(*cx >> 1);
        let mps = *cx & 1;
        let entry = &QE_TABLE[index.min(46)];
        let qe = entry.qe;
        self.a = self.a.wrapping_sub(qe);
        let chigh = (self.c >> 16) & 0xFFFF;
        let d;
        if chigh < qe {
            // LPSEXCHANGE
            if self.a < qe {
                self.a = qe;
                d = mps;
                *cx = (entry.nmps << 1) | mps;
            } else {
                self.a = qe;
                d = 1 - mps;
                let new_mps = if entry.switch { 1 - mps } else { mps };
                *cx = (entry.nlps << 1) | new_mps;
            }
            self.renorm();
        } else {
            self.c = self.c.wrapping_sub(qe << 16);
            if self.a & 0x8000 == 0 {
                // MPSEXCHANGE
                if self.a < qe {
                    d = 1 - mps;
                    let new_mps = if entry.switch { 1 - mps } else { mps };
                    *cx = (entry.nlps << 1) | new_mps;
                } else {
                    d = mps;
                    *cx = (entry.nmps << 1) | mps;
                }
                self.renorm();
            } else {
                d = mps;
            }
        }
        d
    }

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

/// Contextes d'une procédure de décodage d'entier (annexe A) : 512 états.
pub(crate) struct IntContext {
    cx: [Context; 512],
}

impl IntContext {
    pub(crate) fn new() -> Self {
        IntContext { cx: [0; 512] }
    }

    /// Procédure de décodage d'entier (A.2, figure A.1). Rend `None` pour OOB.
    pub(crate) fn decode(&mut self, mq: &mut MqDecoder<'_>) -> Option<i32> {
        let mut prev: usize = 1;
        let mut bit = |mq: &mut MqDecoder<'_>, prev: &mut usize| -> u32 {
            let d = mq.decode(&mut self.cx[*prev]);
            *prev = if *prev < 256 {
                (*prev << 1) | usize::from(d)
            } else {
                (((*prev << 1) | usize::from(d)) & 511) | 256
            };
            u32::from(d)
        };
        let s = bit(mq, &mut prev);
        // Préfixe : jusqu'à cinq 1 consécutifs choisissent la classe de valeur.
        let mut ones = 0usize;
        while ones < 5 && bit(mq, &mut prev) == 1 {
            ones += 1;
        }
        let (nbits, offset) = [(2, 0), (4, 4), (6, 20), (8, 84), (12, 340), (32, 4436)][ones];
        let mut v: u32 = 0;
        for _ in 0..nbits {
            v = (v << 1) | bit(mq, &mut prev);
        }
        let v = v.saturating_add(offset);
        if s == 1 && v == 0 {
            return None;
        }
        let v = i32::try_from(v).unwrap_or(i32::MAX);
        Some(if s == 1 { -v } else { v })
    }
}

/// Contextes de la procédure IAID (A.3) : arbre de `2^(codeLen + 1)` états.
pub(crate) struct IaidContext {
    cx: Vec<Context>,
    code_len: u32,
}

impl IaidContext {
    pub(crate) fn new(code_len: u32) -> Self {
        let code_len = code_len.min(24);
        IaidContext {
            cx: vec![0; 1 << (code_len + 1)],
            code_len,
        }
    }

    /// Décode un identifiant de symbole (figure A.2).
    pub(crate) fn decode(&mut self, mq: &mut MqDecoder<'_>) -> u32 {
        let mut prev: usize = 1;
        for _ in 0..self.code_len {
            let d = mq.decode(&mut self.cx[prev]);
            prev = (prev << 1) | usize::from(d);
        }
        #[allow(clippy::cast_possible_truncation)]
        let id = (prev - (1usize << self.code_len)) as u32;
        id
    }
}

/// Jeu complet des contextes entiers d'une région de texte ou d'un
/// dictionnaire de symboles (§6.4.6, §6.5.8.2.3).
pub(crate) struct IntContexts {
    pub iadh: IntContext,
    pub iadw: IntContext,
    pub iaex: IntContext,
    pub iaai: IntContext,
    pub iadt: IntContext,
    pub iafs: IntContext,
    pub iads: IntContext,
    pub iait: IntContext,
    pub iari: IntContext,
    pub iardw: IntContext,
    pub iardh: IntContext,
    pub iardx: IntContext,
    pub iardy: IntContext,
    pub iaid: IaidContext,
}

impl IntContexts {
    pub(crate) fn new(symbol_code_len: u32) -> Self {
        IntContexts {
            iadh: IntContext::new(),
            iadw: IntContext::new(),
            iaex: IntContext::new(),
            iaai: IntContext::new(),
            iadt: IntContext::new(),
            iafs: IntContext::new(),
            iads: IntContext::new(),
            iait: IntContext::new(),
            iari: IntContext::new(),
            iardw: IntContext::new(),
            iardh: IntContext::new(),
            iardx: IntContext::new(),
            iardy: IntContext::new(),
            iaid: IaidContext::new(symbol_code_len),
        }
    }
}
