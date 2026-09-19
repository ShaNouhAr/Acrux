//! Décodage entropique EBCOT de niveau 1 (ISO/IEC 15444-1 annexe D) : les
//! trois passes de codage par plan de bits (propagation de signification,
//! affinement de magnitude, nettoyage), les 19 contextes, le mode
//! plage (run-length), les symboles de segmentation et les six modes de
//! bloc du tableau A.19.
//!
//! La modélisation de contexte est écrite une seule fois, de façon
//! générique sur un [`BitCoder`] : le décodeur MQ, le décodeur brut du mode
//! « bypass » et, dans les tests, l'encodeur MQ et l'encodeur brut. Chaque
//! symbole est codé par `coder.code(contexte, valeur_attendue)` : le
//! décodeur ignore la valeur attendue et rend le bit lu, l'encodeur écrit
//! la valeur attendue (déduite des magnitudes déjà connues) et la rend.
//! Ainsi la logique de D.3 (tableaux D.1 à D.4) et de D.4 à D.7 est
//! partagée et ne peut pas diverger entre les deux sens.

use super::mq::{Context, MqDecoder};

/// Contextes (tableau D.1 à D.4) : 0 à 8 codage zéro (ZC), 9 à 13 signe
/// (SC), 14 à 16 affinement (MR), 17 plage (RL), 18 uniforme (UNI).
pub(super) const CX_SC: usize = 9;
pub(super) const CX_MR: usize = 14;
pub(super) const CX_RL: usize = 17;
pub(super) const CX_UNI: usize = 18;
/// Nombre de contextes.
pub(super) const NUM_CONTEXTS: usize = 19;

/// États initiaux des contextes (tableau D.7) : tout à 0 sauf ZC 0 à
/// l'état 4, RL à l'état 3 et UNI à l'état 46.
pub(super) fn initial_contexts() -> [Context; NUM_CONTEXTS] {
    let mut cx = [Context::new(0, 0); NUM_CONTEXTS];
    cx[0] = Context::new(4, 0);
    cx[CX_RL] = Context::new(3, 0);
    cx[CX_UNI] = Context::new(46, 0);
    cx
}

/// Modes du bloc de code (tableau A.19, champ « code-block style »).
pub(super) mod cbstyle {
    /// Contournement sélectif du codeur arithmétique (passes brutes).
    pub const BYPASS: u8 = 0x01;
    /// Remise à zéro des probabilités de contexte après chaque passe.
    pub const RESET: u8 = 0x02;
    /// Terminaison après chaque passe.
    pub const TERMALL: u8 = 0x04;
    /// Contexte verticalement causal.
    pub const CAUSAL: u8 = 0x08;
    // 0x10 : terminaison prédictible, sans effet sur le décodage.
    /// Symboles de segmentation en fin de passe de nettoyage.
    pub const SEGSYM: u8 = 0x20;
}

/// Orientation d'une sous-bande (tableau D.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum BandKind {
    /// Passe-bas dans les deux directions.
    LL,
    /// Passe-haut horizontal (xob = 1, yob = 0).
    HL,
    /// Passe-haut vertical (xob = 0, yob = 1).
    LH,
    /// Passe-haut diagonal.
    HH,
}

/// Type d'une passe de codage (D.3.2 à D.3.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum PassKind {
    /// Propagation de signification.
    SigProp,
    /// Affinement de magnitude.
    MagRef,
    /// Nettoyage.
    Cleanup,
}

/// Type de la passe d'indice `index` : la première est un nettoyage, puis
/// les trois passes se succèdent à chaque plan de bits (D.3.1).
pub(super) fn pass_kind(index: u32) -> PassKind {
    match (index + 2) % 3 {
        0 => PassKind::SigProp,
        1 => PassKind::MagRef,
        _ => PassKind::Cleanup,
    }
}

/// Vrai si la passe `index` est codée en brut (mode bypass, D.6) : à partir
/// de la onzième passe, seules les passes de nettoyage restent codées MQ.
pub(super) fn pass_is_raw(index: u32, style: u8) -> bool {
    style & cbstyle::BYPASS != 0 && index >= 10 && pass_kind(index) != PassKind::Cleanup
}

/// Vrai si un segment de mots de code se termine après la passe `index`
/// (D.4.1, tableau D.9) : toujours en mode « termall » ; en mode bypass
/// après la dixième passe, puis après chaque passe brute d'affinement et
/// chaque passe de nettoyage.
pub(super) fn pass_terminates(index: u32, style: u8) -> bool {
    if style & cbstyle::TERMALL != 0 {
        return true;
    }
    if style & cbstyle::BYPASS != 0 && index >= 9 {
        return matches!(pass_kind(index), PassKind::Cleanup | PassKind::MagRef);
    }
    false
}

/// Codeur binaire par contexte (voir la doc du module).
pub(super) trait BitCoder {
    /// Code un symbole dans le contexte `ctx` ; `bit` est la valeur attendue
    /// (utilisée seulement par un encodeur). Rend la valeur codée.
    fn code(&mut self, ctx: usize, bit: u32) -> u32;
    /// Remet les probabilités de contexte à leur état initial (mode RESET).
    fn reset_contexts(&mut self);
}

/// Décodeur MQ avec ses contextes.
pub(super) struct MqBitDecoder<'a, 'c> {
    mq: MqDecoder<'a>,
    cx: &'c mut [Context; NUM_CONTEXTS],
}

impl BitCoder for MqBitDecoder<'_, '_> {
    #[inline]
    fn code(&mut self, ctx: usize, _bit: u32) -> u32 {
        self.mq.decode(&mut self.cx[ctx])
    }

    fn reset_contexts(&mut self) {
        *self.cx = initial_contexts();
    }
}

/// Décodeur brut du mode bypass (D.6) : bits dans l'ordre, poids fort en
/// tête, un bit de bourrage après chaque octet `0xFF`. Au-delà des données
/// il fournit des `0xFF` (même convention que le décodeur MQ).
pub(super) struct RawDecoder<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u32,
    bits: u32,
    prev_ff: bool,
}

impl<'a> RawDecoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        RawDecoder {
            data,
            pos: 0,
            buf: 0,
            bits: 0,
            prev_ff: false,
        }
    }
}

impl BitCoder for RawDecoder<'_> {
    #[inline]
    fn code(&mut self, _ctx: usize, _bit: u32) -> u32 {
        if self.bits == 0 {
            let byte = self.data.get(self.pos).copied().unwrap_or(0xFF);
            self.pos += 1;
            if self.prev_ff {
                self.bits = 7;
                self.buf = u32::from(byte & 0x7F);
            } else {
                self.bits = 8;
                self.buf = u32::from(byte);
            }
            self.prev_ff = byte == 0xFF;
        }
        self.bits -= 1;
        (self.buf >> self.bits) & 1
    }

    fn reset_contexts(&mut self) {}
}

/// Drapeaux d'état d'un échantillon.
const SIG: u8 = 1;
/// Codé dans la passe de propagation du plan courant (« π » de D.3.4).
const VISITED: u8 = 2;
/// A déjà reçu une passe d'affinement.
const REFINED: u8 = 4;
/// Signe négatif.
const NEG: u8 = 8;

/// Voisinage significatif d'un échantillon, empaqueté : ΣH bits 0-1,
/// ΣV bits 2-3, ΣD bits 4-6.
const NBR_H: u8 = 1;
const NBR_V: u8 = 4;
const NBR_D: u8 = 16;

/// Table de contexte ZC (tableau D.1) indexée par le voisinage empaqueté.
const fn zc_table(kind: BandKind) -> [u8; 128] {
    let mut table = [0u8; 128];
    let mut packed = 0usize;
    while packed < 128 {
        let (mut horiz, mut vert) = (packed & 3, (packed >> 2) & 3);
        let diag = packed >> 4;
        if matches!(kind, BandKind::HL) {
            // HL : rôles horizontal et vertical échangés.
            let swapped = horiz;
            horiz = vert;
            vert = swapped;
        }
        let ctx = match kind {
            BandKind::LL | BandKind::LH | BandKind::HL => {
                if horiz == 2 {
                    8
                } else if horiz == 1 {
                    if vert >= 1 {
                        7
                    } else if diag >= 1 {
                        6
                    } else {
                        5
                    }
                } else if vert == 2 {
                    4
                } else if vert == 1 {
                    3
                } else if diag >= 2 {
                    2
                } else if diag == 1 {
                    1
                } else {
                    0
                }
            }
            BandKind::HH => {
                let horiz_vert = horiz + vert;
                if diag >= 3 {
                    8
                } else if diag == 2 {
                    if horiz_vert >= 1 {
                        7
                    } else {
                        6
                    }
                } else if diag == 1 {
                    if horiz_vert >= 2 {
                        5
                    } else if horiz_vert == 1 {
                        4
                    } else {
                        3
                    }
                } else if horiz_vert >= 2 {
                    2
                } else if horiz_vert == 1 {
                    1
                } else {
                    0
                }
            }
        };
        table[packed] = ctx;
        packed += 1;
    }
    table
}

const ZC_LL: [u8; 128] = zc_table(BandKind::LL);
const ZC_HL: [u8; 128] = zc_table(BandKind::HL);
const ZC_HH: [u8; 128] = zc_table(BandKind::HH);

/// Segment de données d'un bloc de code reçu dans un paquet : plage
/// d'octets dans le corps de la tuile et nombre de passes qu'elle couvre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Chunk {
    pub start: usize,
    pub end: usize,
    pub passes: u32,
}

/// Paramètres de décodage d'un bloc.
#[derive(Clone, Copy, Debug)]
pub(super) struct BlockParams {
    /// Modes du bloc (tableau A.19).
    pub style: u8,
    /// Nombre de plans de bits à décoder (Mb − P).
    pub planes: u32,
    /// Nombre total de passes reçues.
    pub passes: u32,
}

/// Résultat du décodage d'un bloc.
#[derive(Clone, Copy, Debug)]
pub(super) struct BlockOutcome {
    /// Indice du plan de bits le plus bas effectivement décodé (0 = tout).
    pub lowest_plane: u32,
}

/// Moteur de modélisation de contexte pour un bloc, réutilisé d'un bloc à
/// l'autre (aucune allocation par échantillon). Les tableaux ont une
/// bordure d'un échantillon pour lire les voisins sans test de limite.
pub(super) struct BlockCoder {
    pub w: usize,
    pub h: usize,
    stride: usize,
    /// Drapeaux SIG, VISITED, REFINED, NEG.
    pub flags: Vec<u8>,
    /// Voisinage significatif empaqueté.
    nbr: Vec<u8>,
    /// Magnitudes (bits accumulés plan par plan).
    pub mag: Vec<u32>,
    causal: bool,
    zc: &'static [u8; 128],
    /// Tampon de concaténation des segments répartis sur plusieurs couches.
    scratch: Vec<u8>,
}

impl BlockCoder {
    pub fn new() -> Self {
        BlockCoder {
            w: 0,
            h: 0,
            stride: 2,
            flags: Vec::new(),
            nbr: Vec::new(),
            mag: Vec::new(),
            causal: false,
            zc: &ZC_LL,
            scratch: Vec::new(),
        }
    }

    /// Prépare le moteur pour un bloc `w × h` (tout à zéro).
    pub fn reset(&mut self, w: usize, h: usize, band: BandKind, style: u8) {
        self.w = w;
        self.h = h;
        self.stride = w + 2;
        let size = (w + 2) * (h + 2);
        self.flags.clear();
        self.flags.resize(size, 0);
        self.nbr.clear();
        self.nbr.resize(size, 0);
        self.mag.clear();
        self.mag.resize(size, 0);
        self.causal = style & cbstyle::CAUSAL != 0;
        self.zc = match band {
            BandKind::LL | BandKind::LH => &ZC_LL,
            BandKind::HL => &ZC_HL,
            BandKind::HH => &ZC_HH,
        };
    }

    /// Indice (avec bordure) de l'échantillon (x, y).
    #[inline]
    pub fn index(&self, x: usize, y: usize) -> usize {
        (y + 1) * self.stride + x + 1
    }

    /// Marque l'échantillon comme significatif et met à jour le voisinage
    /// des huit voisins. En mode causal vertical, la première ligne d'une
    /// bande de quatre n'est pas vue par la ligne du dessus (qui appartient
    /// à la bande précédente et ne doit pas regarder « en avant », D.7).
    #[inline]
    fn set_significant(&mut self, idx: usize, y: usize, negative: bool) {
        self.flags[idx] |= SIG | if negative { NEG } else { 0 };
        let s = self.stride;
        self.nbr[idx - 1] += NBR_H;
        self.nbr[idx + 1] += NBR_H;
        self.nbr[idx + s] += NBR_V;
        self.nbr[idx + s - 1] += NBR_D;
        self.nbr[idx + s + 1] += NBR_D;
        if !(self.causal && y % 4 == 0) {
            self.nbr[idx - s] += NBR_V;
            self.nbr[idx - s - 1] += NBR_D;
            self.nbr[idx - s + 1] += NBR_D;
        }
    }

    /// Contexte de signe et bit d'inversion (tableau D.3) d'après les
    /// contributions horizontale et verticale (tableau D.2).
    #[inline]
    fn sign_context(&self, idx: usize, y: usize) -> (usize, u32) {
        let contrib = |f: u8| -> i32 {
            if f & SIG == 0 {
                0
            } else if f & NEG != 0 {
                -1
            } else {
                1
            }
        };
        let s = self.stride;
        let h = (contrib(self.flags[idx - 1]) + contrib(self.flags[idx + 1])).clamp(-1, 1);
        let down = if self.causal && y % 4 == 3 {
            0
        } else {
            self.flags[idx + s]
        };
        let v = (contrib(self.flags[idx - s]) + contrib(down)).clamp(-1, 1);
        match (h, v) {
            (0, 0) => (CX_SC, 0),
            (0, 1) => (CX_SC + 1, 0),
            (0, _) => (CX_SC + 1, 1),
            (1, 0) => (CX_SC + 3, 0),
            (1, 1) => (CX_SC + 4, 0),
            (1, _) => (CX_SC + 2, 0),
            (_, 0) => (CX_SC + 3, 1),
            (_, 1) => (CX_SC + 2, 1),
            (_, _) => (CX_SC + 4, 1),
        }
    }

    /// Code le signe d'un échantillon qui devient significatif (D.3.2).
    #[inline]
    fn code_sign<C: BitCoder + ?Sized>(&mut self, coder: &mut C, idx: usize, y: usize) {
        let (ctx, xor) = self.sign_context(idx, y);
        let expected = u32::from(self.flags[idx] & NEG != 0) ^ xor;
        let bit = coder.code(ctx, expected);
        self.set_significant(idx, y, (bit ^ xor) != 0);
    }

    /// Passe de propagation de signification (D.3.1) sur le plan `plane`.
    pub fn sig_prop_pass<C: BitCoder + ?Sized>(&mut self, coder: &mut C, plane: u32) {
        for y0 in (0..self.h).step_by(4) {
            let y1 = (y0 + 4).min(self.h);
            for x in 0..self.w {
                for y in y0..y1 {
                    let idx = self.index(x, y);
                    if self.flags[idx] & SIG != 0 {
                        continue;
                    }
                    let n = self.nbr[idx];
                    if n == 0 {
                        continue;
                    }
                    let ctx = usize::from(self.zc[usize::from(n & 0x7F)]);
                    let expected = (self.mag[idx] >> plane) & 1;
                    if coder.code(ctx, expected) != 0 {
                        self.mag[idx] |= 1 << plane;
                        self.code_sign(coder, idx, y);
                    }
                    self.flags[idx] |= VISITED;
                }
            }
        }
    }

    /// Passe d'affinement de magnitude (D.3.3, tableau D.4).
    pub fn mag_ref_pass<C: BitCoder + ?Sized>(&mut self, coder: &mut C, plane: u32) {
        for y0 in (0..self.h).step_by(4) {
            let y1 = (y0 + 4).min(self.h);
            for x in 0..self.w {
                for y in y0..y1 {
                    let idx = self.index(x, y);
                    let f = self.flags[idx];
                    if f & (SIG | VISITED) != SIG {
                        continue;
                    }
                    let ctx = if f & REFINED != 0 {
                        CX_MR + 2
                    } else if self.nbr[idx] != 0 {
                        CX_MR + 1
                    } else {
                        CX_MR
                    };
                    let expected = (self.mag[idx] >> plane) & 1;
                    let bit = coder.code(ctx, expected);
                    self.mag[idx] |= bit << plane;
                    self.flags[idx] |= REFINED;
                }
            }
        }
    }

    /// Passe de nettoyage (D.3.4) avec le mode plage et, si demandé, le
    /// symbole de segmentation `1010` (D.5).
    pub fn cleanup_pass<C: BitCoder + ?Sized>(
        &mut self,
        coder: &mut C,
        plane: u32,
        seg_symbol: bool,
    ) {
        let s = self.stride;
        for y0 in (0..self.h).step_by(4) {
            let y1 = (y0 + 4).min(self.h);
            for x in 0..self.w {
                let mut y = y0;
                if y1 - y0 == 4 {
                    let idx0 = self.index(x, y0);
                    let all_clear = (0..4).all(|k| {
                        let i = idx0 + k * s;
                        self.flags[i] & (SIG | VISITED) == 0 && self.nbr[i] == 0
                    });
                    if all_clear {
                        let first = (0..4u32)
                            .find(|&k| (self.mag[idx0 + k as usize * s] >> plane) & 1 != 0);
                        if coder.code(CX_RL, u32::from(first.is_some())) == 0 {
                            continue;
                        }
                        let expected = first.unwrap_or(0);
                        let hi = coder.code(CX_UNI, (expected >> 1) & 1);
                        let lo = coder.code(CX_UNI, expected & 1);
                        let k = ((hi << 1) | lo) as usize;
                        let idx = idx0 + k * s;
                        self.mag[idx] |= 1 << plane;
                        self.code_sign(coder, idx, y0 + k);
                        y = y0 + k + 1;
                    }
                }
                while y < y1 {
                    let idx = self.index(x, y);
                    let f = self.flags[idx];
                    if f & VISITED != 0 {
                        self.flags[idx] = f & !VISITED;
                    } else if f & SIG == 0 {
                        let ctx = usize::from(self.zc[usize::from(self.nbr[idx] & 0x7F)]);
                        let expected = (self.mag[idx] >> plane) & 1;
                        if coder.code(ctx, expected) != 0 {
                            self.mag[idx] |= 1 << plane;
                            self.code_sign(coder, idx, y);
                        }
                    }
                    y += 1;
                }
            }
        }
        if seg_symbol {
            for expected in [1, 0, 1, 0] {
                // Un symbole erroné signale une corruption ; on continue
                // quand même (comportement tolérant).
                let _ = coder.code(CX_UNI, expected);
            }
        }
    }

    /// Exécute une passe du type voulu.
    fn run_pass<C: BitCoder>(&mut self, coder: &mut C, kind: PassKind, plane: u32, style: u8) {
        match kind {
            PassKind::SigProp => self.sig_prop_pass(coder, plane),
            PassKind::MagRef => self.mag_ref_pass(coder, plane),
            PassKind::Cleanup => {
                self.cleanup_pass(coder, plane, style & cbstyle::SEGSYM != 0);
            }
        }
        if style & cbstyle::RESET != 0 {
            coder.reset_contexts();
        }
    }

    /// Décode un bloc entier à partir de ses segments (D.4) : chaque
    /// segment terminé réinitialise le décodeur MQ ou brut ; les contextes
    /// ne sont réinitialisés qu'en mode RESET. Le moteur doit avoir été
    /// préparé par [`BlockCoder::reset`].
    pub fn decode(&mut self, body: &[u8], chunks: &[Chunk], params: BlockParams) -> BlockOutcome {
        let planes = params.planes;
        if planes == 0 || planes > 31 {
            return BlockOutcome {
                lowest_plane: planes,
            };
        }
        let max_passes = 3 * (planes - 1) + 1;
        let total = params.passes.min(max_passes);
        let mut cx = initial_contexts();
        let mut lowest_plane = planes;
        let mut pass = 0u32;
        let mut chunk_iter = chunks.iter();
        let mut scratch = std::mem::take(&mut self.scratch);
        while pass < total {
            // Rassemble les morceaux (un par paquet) jusqu'au prochain point
            // de terminaison : ils forment un seul segment de mots de code.
            let mut seg_passes = 0u32;
            let mut first: Option<(usize, usize)> = None;
            let mut multi = false;
            for chunk in chunk_iter.by_ref() {
                if chunk.passes == 0 {
                    continue;
                }
                let start = chunk.start.min(body.len());
                let end = chunk.end.clamp(start, body.len());
                match first {
                    None => first = Some((start, end)),
                    Some((fs, fe)) => {
                        if !multi {
                            scratch.clear();
                            scratch.extend_from_slice(&body[fs..fe]);
                            multi = true;
                        }
                        scratch.extend_from_slice(&body[start..end]);
                    }
                }
                seg_passes += chunk.passes;
                let last = pass + seg_passes;
                if pass_terminates(last - 1, params.style) || last >= total {
                    break;
                }
            }
            let Some((fs, fe)) = first else {
                break;
            };
            let data: &[u8] = if multi { &scratch } else { &body[fs..fe] };
            let end = (pass + seg_passes).min(total);
            if pass_is_raw(pass, params.style) {
                let mut coder = RawDecoder::new(data);
                while pass < end {
                    let plane = planes - 1 - pass.div_ceil(3);
                    self.run_pass(&mut coder, pass_kind(pass), plane, params.style);
                    lowest_plane = plane;
                    pass += 1;
                }
            } else {
                let mut coder = MqBitDecoder {
                    mq: MqDecoder::new(data),
                    cx: &mut cx,
                };
                while pass < end {
                    let plane = planes - 1 - pass.div_ceil(3);
                    self.run_pass(&mut coder, pass_kind(pass), plane, params.style);
                    lowest_plane = plane;
                    pass += 1;
                }
            }
        }
        self.scratch = scratch;
        BlockOutcome { lowest_plane }
    }

    /// Vrai si l'échantillon d'indice `idx` est négatif.
    #[inline]
    pub fn is_negative(&self, idx: usize) -> bool {
        self.flags[idx] & NEG != 0
    }

    /// Charge la valeur connue d'un échantillon (encodeur de test).
    #[cfg(test)]
    pub fn preset(&mut self, x: usize, y: usize, magnitude: u32, negative: bool) {
        let idx = self.index(x, y);
        self.mag[idx] = magnitude;
        if negative {
            self.flags[idx] |= NEG;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pass_kinds_follow_cleanup_then_triplets() {
        assert_eq!(pass_kind(0), PassKind::Cleanup);
        assert_eq!(pass_kind(1), PassKind::SigProp);
        assert_eq!(pass_kind(2), PassKind::MagRef);
        assert_eq!(pass_kind(3), PassKind::Cleanup);
        assert_eq!(pass_kind(10), PassKind::SigProp);
    }

    #[test]
    fn bypass_terminations_match_table_d9() {
        let s = cbstyle::BYPASS;
        let terminated: Vec<u32> = (0..16).filter(|&i| pass_terminates(i, s)).collect();
        assert_eq!(terminated, vec![9, 11, 12, 14, 15]);
        let raw: Vec<u32> = (0..16).filter(|&i| pass_is_raw(i, s)).collect();
        assert_eq!(raw, vec![10, 11, 13, 14]);
        assert!((0..16).all(|i| pass_terminates(i, cbstyle::TERMALL)));
        assert!(!(0..16).any(|i| pass_terminates(i, 0)));
    }

    #[test]
    fn zero_coding_tables_match_table_d1() {
        // LL : deux voisins horizontaux → 8 ; un vertical seul → 3.
        assert_eq!(ZC_LL[2], 8);
        assert_eq!(ZC_LL[4], 3);
        assert_eq!(ZC_LL[16], 1);
        assert_eq!(ZC_LL[0], 0);
        // HL : rôles de H et V échangés.
        assert_eq!(ZC_HL[8], 8);
        assert_eq!(ZC_HL[1], 3);
        // HH : trois diagonaux → 8 ; un diagonal et un H → 4.
        assert_eq!(ZC_HH[48], 8);
        assert_eq!(ZC_HH[17], 4);
        assert_eq!(ZC_HH[16], 3);
    }

    #[test]
    fn raw_decoder_skips_stuffed_bit_after_ff() {
        let mut raw = RawDecoder::new(&[0xFF, 0x7F, 0x80]);
        let bits: Vec<u32> = (0..23).map(|_| raw.code(0, 0)).collect();
        let mut expected = vec![1u32; 8];
        expected.extend([1; 7]);
        expected.extend([1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(bits, expected);
    }
}
