//! Encodeur JPEG 2000 minimal de test, écrit d'après ISO/IEC 15444-1 :
//! encodeur MQ (annexe C, procédures CODEMPS / CODELPS / BYTEOUT / FLUSH),
//! codage EBCOT des trois passes via le moteur partagé de `tier1` (avec les
//! modes bypass, termall, reset, causal et symboles de segmentation),
//! ondelettes directes 5-3 et 9-7 (F.4, informatif), quantification nulle
//! ou scalaire explicite, RCT / ICT directes (G.2, G.3), arbres
//! d'étiquettes d'encodage (B.10.2), en-têtes de paquets (B.10), toutes les
//! progressions et les changements POC, tuiles, tuiles-parties, PPM / PPT,
//! SOP / EPH, ROI Maxshift, marqueurs SIZ / COD / COC / QCD / RGN / POC /
//! COM / SOT / SOD / EOC, et enveloppe JP2 (annexe I).
//!
//! Il n'optimise rien : une couche par défaut (plusieurs seulement avec
//! termall, où chaque passe est un segment), pas de troncature.

use super::super::codestream::{
    CodingStyle, ComponentSiz, PocEntry, Progression, Quant, QuantStyle,
};
use super::super::mq::{Context, QE};
use super::super::structure::{self, Budget, Rect, TileComponent};
use super::super::tier1::{
    self, cbstyle, initial_contexts, pass_is_raw, pass_kind, pass_terminates, BandKind, BitCoder,
    BlockCoder, PassKind, NUM_CONTEXTS,
};

// ---------------------------------------------------------------------------
// Encodeur MQ (C.2)
// ---------------------------------------------------------------------------

/// Encodeur MQ avec ses contextes (procédures des figures C.4 à C.11).
pub struct MqEncoder {
    /// `out[0]` est l'octet fictif précédant le début (BPST).
    out: Vec<u8>,
    bp: usize,
    a: u32,
    c: u32,
    ct: u32,
    pub cx: [Context; NUM_CONTEXTS],
}

impl Default for MqEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl MqEncoder {
    /// INITENC (figure C.10).
    pub fn new() -> Self {
        MqEncoder {
            out: vec![0],
            bp: 0,
            a: 0x8000,
            c: 0,
            ct: 12,
            cx: initial_contexts(),
        }
    }

    /// CODEMPS / CODELPS (figures C.4 et C.5).
    pub fn encode(&mut self, ctx: usize, d: u32) {
        let (index, mps) = (self.cx[ctx].index(), self.cx[ctx].mps());
        let e = QE[index];
        let qe = e.qe;
        if d == mps {
            self.a -= qe;
            if self.a & 0x8000 == 0 {
                if self.a < qe {
                    self.a = qe;
                } else {
                    self.c += qe;
                }
                self.cx[ctx] = Context::new(e.nmps, mps as u8);
                self.renorm();
            } else {
                self.c += qe;
            }
        } else {
            self.a -= qe;
            if self.a < qe {
                self.c += qe;
            } else {
                self.a = qe;
            }
            let new_mps = if e.switch { 1 - mps } else { mps };
            self.cx[ctx] = Context::new(e.nlps, new_mps as u8);
            self.renorm();
        }
    }

    /// RENORME (figure C.6) : contrairement à RENORMD, l'octet sort après
    /// le décalage, quand CT retombe à zéro.
    fn renorm(&mut self) {
        loop {
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            if self.ct == 0 {
                self.byte_out();
            }
            if self.a & 0x8000 != 0 {
                break;
            }
        }
    }

    /// BYTEOUT (figure C.7).
    fn byte_out(&mut self) {
        if self.out[self.bp] == 0xFF {
            self.stuff();
        } else if self.c < 0x800_0000 {
            self.emit_8();
        } else {
            self.out[self.bp] += 1;
            if self.out[self.bp] == 0xFF {
                self.c &= 0x7FF_FFFF;
                self.stuff();
            } else {
                self.emit_8();
            }
        }
    }

    fn emit_8(&mut self) {
        self.out.push(((self.c >> 19) & 0xFF) as u8);
        self.bp += 1;
        self.c &= 0x7_FFFF;
        self.ct = 8;
    }

    fn stuff(&mut self) {
        self.out.push(((self.c >> 20) & 0xFF) as u8);
        self.bp += 1;
        self.c &= 0xF_FFFF;
        self.ct = 7;
    }

    /// FLUSH (figure C.8) avec SETBITS (C.9) ; rend les octets du segment
    /// sans le marqueur final ni un `0xFF` terminal (D.4.2 : un segment ne
    /// se termine jamais par 0xFF).
    pub fn flush(mut self) -> Vec<u8> {
        let tempc = self.c + self.a;
        self.c |= 0xFFFF;
        if self.c >= tempc {
            self.c -= 0x8000;
        }
        self.c <<= self.ct;
        self.byte_out();
        self.c <<= self.ct;
        self.byte_out();
        if self.out[self.bp] != 0xFF {
            self.out.push(0xFF);
            self.bp += 1;
        }
        let mut bytes = self.out[1..=self.bp].to_vec();
        while bytes.last() == Some(&0xFF) {
            bytes.pop();
        }
        bytes
    }
}

impl BitCoder for MqEncoder {
    fn code(&mut self, ctx: usize, bit: u32) -> u32 {
        self.encode(ctx, bit);
        bit
    }

    fn reset_contexts(&mut self) {
        self.cx = initial_contexts();
    }
}

/// Encodeur brut du mode bypass (D.6) : bit de bourrage après 0xFF.
#[derive(Default)]
pub struct RawEncoder {
    out: Vec<u8>,
    cur: u32,
    nbits: u32,
    limit: u32,
}

impl RawEncoder {
    pub fn new() -> Self {
        RawEncoder {
            out: Vec::new(),
            cur: 0,
            nbits: 0,
            limit: 8,
        }
    }

    fn push_bit(&mut self, b: u32) {
        self.cur = (self.cur << 1) | b;
        self.nbits += 1;
        if self.nbits == self.limit {
            self.emit();
        }
    }

    fn emit(&mut self) {
        let byte = self.cur as u8;
        self.out.push(byte);
        self.limit = if byte == 0xFF { 7 } else { 8 };
        self.cur = 0;
        self.nbits = 0;
    }

    /// Termine le segment : bourrage alterné 0,1,0,1… (D.6.2).
    pub fn flush(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            let mut fill = 0;
            while self.nbits > 0 {
                self.push_bit(fill);
                fill ^= 1;
            }
        }
        if self.out.last() == Some(&0xFF) {
            self.out.push(0x2A);
        }
        self.out
    }
}

impl BitCoder for RawEncoder {
    fn code(&mut self, _ctx: usize, bit: u32) -> u32 {
        self.push_bit(bit);
        bit
    }

    fn reset_contexts(&mut self) {}
}

// ---------------------------------------------------------------------------
// Codage d'un bloc (annexe D, sens direct)
// ---------------------------------------------------------------------------

/// Bloc codé : passes, plans nuls, segments (longueur, passes) et données.
pub struct EncodedBlock {
    pub passes: u32,
    pub zero_planes: u32,
    pub segments: Vec<(usize, u32)>,
    pub data: Vec<u8>,
}

/// Codeur courant d'un segment.
enum Coder {
    Mq(MqEncoder),
    Raw(RawEncoder),
}

/// Code un bloc dont les magnitudes sont dans `mags` (ligne par ligne) et
/// les signes dans `negs`, avec `mb` plans de bits disponibles.
pub fn encode_block(
    engine: &mut BlockCoder,
    w: usize,
    h: usize,
    mags: &[u32],
    negs: &[bool],
    band: BandKind,
    style: u8,
    mb: u32,
) -> EncodedBlock {
    let max = mags.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return EncodedBlock {
            passes: 0,
            zero_planes: mb,
            segments: Vec::new(),
            data: Vec::new(),
        };
    }
    let numbps = 32 - max.leading_zeros();
    assert!(numbps <= mb, "magnitude {max} trop grande pour Mb = {mb}");
    engine.reset(w, h, band, style);
    for y in 0..h {
        for x in 0..w {
            engine.preset(x, y, mags[y * w + x], negs[y * w + x]);
        }
    }
    let passes = 3 * numbps - 2;
    let mut segments = Vec::new();
    let mut data = Vec::new();
    let mut contexts = initial_contexts();
    let mut coder: Option<Coder> = None;
    let mut seg_passes = 0u32;
    for pass in 0..passes {
        let plane = numbps - 1 - pass.div_ceil(3);
        let kind = pass_kind(pass);
        if coder.is_none() {
            coder = Some(if pass_is_raw(pass, style) {
                Coder::Raw(RawEncoder::new())
            } else {
                let mut mq = MqEncoder::new();
                mq.cx = contexts;
                Coder::Mq(mq)
            });
        }
        let seg_symbol = style & cbstyle::SEGSYM != 0;
        let run = |c: &mut dyn BitCoder, engine: &mut BlockCoder| match kind {
            PassKind::SigProp => engine.sig_prop_pass(c, plane),
            PassKind::MagRef => engine.mag_ref_pass(c, plane),
            PassKind::Cleanup => engine.cleanup_pass(c, plane, seg_symbol),
        };
        match coder.as_mut().unwrap() {
            Coder::Mq(mq) => {
                run(mq, engine);
                if style & cbstyle::RESET != 0 {
                    mq.reset_contexts();
                }
                contexts = mq.cx;
            }
            Coder::Raw(raw) => run(raw, engine),
        }
        seg_passes += 1;
        if pass_terminates(pass, style) || pass + 1 == passes {
            let bytes = match coder.take().unwrap() {
                Coder::Mq(mq) => mq.flush(),
                Coder::Raw(raw) => raw.flush(),
            };
            segments.push((bytes.len(), seg_passes));
            data.extend_from_slice(&bytes);
            seg_passes = 0;
        }
    }
    EncodedBlock {
        passes,
        zero_planes: mb - numbps,
        segments,
        data,
    }
}

// ---------------------------------------------------------------------------
// Arbres d'étiquettes et écriture de bits d'en-tête (B.10)
// ---------------------------------------------------------------------------

/// Écrivain de bits d'en-tête de paquet avec bourrage après 0xFF (B.10.1).
#[derive(Default)]
pub struct BitWriter {
    pub out: Vec<u8>,
    cur: u32,
    nbits: u32,
    limit: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        BitWriter {
            out: Vec::new(),
            cur: 0,
            nbits: 0,
            limit: 8,
        }
    }

    pub fn bit(&mut self, b: u32) {
        self.cur = (self.cur << 1) | (b & 1);
        self.nbits += 1;
        if self.nbits == self.limit {
            self.emit();
        }
    }

    pub fn bits(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            self.bit((v >> i) & 1);
        }
    }

    fn emit(&mut self) {
        let byte = self.cur as u8;
        self.out.push(byte);
        self.limit = if byte == 0xFF { 7 } else { 8 };
        self.cur = 0;
        self.nbits = 0;
    }

    /// Termine l'en-tête : complète l'octet et, si le dernier octet vaut
    /// 0xFF, émet un octet de bourrage.
    pub fn finish(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            self.cur <<= self.limit - self.nbits;
            self.emit();
        }
        if self.limit == 7 {
            self.out.push(0);
        }
        self.out
    }
}

/// Arbre d'étiquettes côté encodeur (B.10.2).
pub struct TagTreeEncoder {
    levels: Vec<(usize, usize, usize)>,
    value: Vec<u32>,
    low: Vec<u32>,
    known: Vec<bool>,
}

impl TagTreeEncoder {
    pub fn new(w: usize, h: usize, leaves: &[u32]) -> Self {
        let mut levels = Vec::new();
        let (mut lw, mut lh) = (w, h);
        let mut total = 0;
        if w > 0 && h > 0 {
            loop {
                levels.push((lw, lh, total));
                total += lw * lh;
                if lw == 1 && lh == 1 {
                    break;
                }
                lw = lw.div_ceil(2);
                lh = lh.div_ceil(2);
            }
        }
        let mut value = vec![u32::MAX; total];
        if total > 0 {
            value[..w * h].copy_from_slice(leaves);
            for level in 1..levels.len() {
                let (pw, ph, pbase) = levels[level];
                let (cw, ch, cbase) = levels[level - 1];
                for py in 0..ph {
                    for px in 0..pw {
                        let mut m = u32::MAX;
                        for cy in (2 * py)..(2 * py + 2).min(ch) {
                            for cx in (2 * px)..(2 * px + 2).min(cw) {
                                m = m.min(value[cbase + cy * cw + cx]);
                            }
                        }
                        value[pbase + py * pw + px] = m;
                    }
                }
            }
        }
        TagTreeEncoder {
            levels,
            value,
            low: vec![0; total],
            known: vec![false; total],
        }
    }

    /// Procédure d'encodage pour la feuille (x, y) au seuil donné ; rend
    /// vrai si la valeur est connue du décodeur et inférieure au seuil.
    pub fn encode(&mut self, w: &mut BitWriter, x: usize, y: usize, threshold: u32) -> bool {
        let mut low = 0;
        for level in (0..self.levels.len()).rev() {
            let (lw, _, base) = self.levels[level];
            let n = base + (y >> level) * lw + (x >> level);
            if self.low[n] < low {
                self.low[n] = low;
            }
            while !self.known[n] && self.low[n] < threshold {
                if self.low[n] == self.value[n] {
                    w.bit(1);
                    self.known[n] = true;
                } else {
                    w.bit(0);
                    self.low[n] += 1;
                }
            }
            low = self.low[n];
            if !self.known[n] {
                return false;
            }
        }
        low < threshold
    }
}

// ---------------------------------------------------------------------------
// Ondelettes directes (F.4, informatif) et transformations de couleur
// ---------------------------------------------------------------------------

const EXT: usize = 4;
const ALPHA: f32 = -1.586_134_3;
const BETA: f32 = -0.052_980_12;
const GAMMA: f32 = 0.882_911_1;
const DELTA: f32 = 0.443_506_87;
const K: f32 = 1.230_174_1;

fn pse(i: i64, i0: i64, i1: i64) -> i64 {
    let n = i1 - i0;
    if n <= 1 {
        return i0;
    }
    let period = 2 * (n - 1);
    let mut m = (i - i0).rem_euclid(period);
    if m >= n {
        m = period - m;
    }
    i0 + m
}

fn load<T: Copy>(src: &[T], i0: i64, buf: &mut Vec<T>) {
    let n = src.len();
    let i1 = i0 + n as i64;
    buf.clear();
    for k in 0..n + 2 * EXT {
        let i = i0 - EXT as i64 + k as i64;
        buf.push(src[(pse(i, i0, i1) - i0) as usize]);
    }
}

/// Analyse 5-3 (F.4.8.2.1) d'un signal `[i0, i1)`.
fn analyze_53(x: &mut [i32], i0: i64, buf: &mut Vec<i32>) {
    let n = x.len();
    let i1 = i0 + n as i64;
    if n == 1 {
        if i0.rem_euclid(2) == 1 {
            x[0] *= 2;
        }
        return;
    }
    load(x, i0, buf);
    let at = |i: i64| (i - i0 + EXT as i64) as usize;
    let mut i = i0 - 1 + 1 - (i0 - 1).rem_euclid(2);
    while i < i1 + 1 {
        buf[at(i)] -= (buf[at(i - 1)] + buf[at(i + 1)]) >> 1;
        i += 2;
    }
    let mut i = i0 + i0.rem_euclid(2);
    while i < i1 {
        buf[at(i)] += (buf[at(i - 1)] + buf[at(i + 1)] + 2) >> 2;
        i += 2;
    }
    x.copy_from_slice(&buf[EXT..EXT + n]);
}

/// Analyse 9-7 (F.4.8.2.2) : lifting puis échelle (pairs × 1/K, impairs × K).
fn analyze_97(x: &mut [f32], i0: i64, buf: &mut Vec<f32>) {
    let n = x.len();
    let i1 = i0 + n as i64;
    if n == 1 {
        if i0.rem_euclid(2) == 1 {
            x[0] *= 2.0;
        }
        return;
    }
    load(x, i0, buf);
    let at = |i: i64| (i - i0 + EXT as i64) as usize;
    let first_even = |from: i64| from + from.rem_euclid(2);
    let first_odd = |from: i64| from + 1 - from.rem_euclid(2);
    let mut i = first_odd(i0 - 3);
    while i < i1 + 3 {
        buf[at(i)] += ALPHA * (buf[at(i - 1)] + buf[at(i + 1)]);
        i += 2;
    }
    let mut i = first_even(i0 - 2);
    while i < i1 + 2 {
        buf[at(i)] += BETA * (buf[at(i - 1)] + buf[at(i + 1)]);
        i += 2;
    }
    let mut i = first_odd(i0 - 1);
    while i < i1 + 1 {
        buf[at(i)] += GAMMA * (buf[at(i - 1)] + buf[at(i + 1)]);
        i += 2;
    }
    let mut i = first_even(i0);
    while i < i1 {
        buf[at(i)] += DELTA * (buf[at(i - 1)] + buf[at(i + 1)]);
        buf[at(i)] /= K;
        i += 2;
    }
    let mut i = first_odd(i0);
    while i < i1 {
        buf[at(i)] *= K;
        i += 2;
    }
    x.copy_from_slice(&buf[EXT..EXT + n]);
}

/// Procédure 2D_SD (F.4.8) : colonnes, lignes, puis désentrelacement en
/// (LL, HL, LH, HH).
fn analyze_level<T: Copy + Default>(
    rect: Rect,
    data: &[T],
    filter: fn(&mut [T], i64, &mut Vec<T>),
) -> [Vec<T>; 4] {
    let w = rect.width() as usize;
    let h = rect.height() as usize;
    let mut a = data.to_vec();
    let mut buf = Vec::new();
    let mut col = vec![T::default(); h];
    for x in 0..w {
        for y in 0..h {
            col[y] = a[y * w + x];
        }
        filter(&mut col, i64::from(rect.y0), &mut buf);
        for y in 0..h {
            a[y * w + x] = col[y];
        }
    }
    for y in 0..h {
        filter(&mut a[y * w..(y + 1) * w], i64::from(rect.x0), &mut buf);
    }
    let mut bands: [Vec<T>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for y in 0..h {
        for x in 0..w {
            let ox = (rect.x0 as usize + x) % 2;
            let oy = (rect.y0 as usize + y) % 2;
            bands[ox + 2 * oy].push(a[y * w + x]);
        }
    }
    bands
}

fn forward_rct(c: &mut [Vec<i32>]) {
    let (r, rest) = c.split_at_mut(1);
    let (g, b) = rest.split_at_mut(1);
    for ((r, g), b) in r[0].iter_mut().zip(g[0].iter_mut()).zip(b[0].iter_mut()) {
        let y0 = (*r + 2 * *g + *b) >> 2;
        let y1 = *b - *g;
        let y2 = *r - *g;
        *r = y0;
        *g = y1;
        *b = y2;
    }
}

fn forward_ict(c: &mut [Vec<f32>]) {
    let (r, rest) = c.split_at_mut(1);
    let (g, b) = rest.split_at_mut(1);
    for ((r, g), b) in r[0].iter_mut().zip(g[0].iter_mut()).zip(b[0].iter_mut()) {
        let y = 0.299 * *r + 0.587 * *g + 0.114 * *b;
        let cb = -0.168_75 * *r - 0.331_26 * *g + 0.5 * *b;
        let cr = 0.5 * *r - 0.418_69 * *g - 0.081_31 * *b;
        *r = y;
        *g = cb;
        *b = cr;
    }
}

// ---------------------------------------------------------------------------
// Image et paramètres
// ---------------------------------------------------------------------------

/// Composante d'entrée : échantillons sur sa propre grille.
#[derive(Clone)]
pub struct Component {
    pub precision: u8,
    pub signed: bool,
    pub xr: u32,
    pub yr: u32,
    /// Valeurs (non signées 0..2^p, ou signées) ligne par ligne sur le
    /// rectangle ⌈origine / xr⌉ .. ⌈taille / xr⌉.
    pub data: Vec<i32>,
}

/// Image d'entrée sur la grille de référence.
#[derive(Clone)]
pub struct Image {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    pub components: Vec<Component>,
}

impl Image {
    pub fn comp_rect(&self, c: usize) -> Rect {
        let comp = &self.components[c];
        Rect {
            x0: self.x0.div_ceil(comp.xr),
            y0: self.y0.div_ceil(comp.yr),
            x1: self.x1.div_ceil(comp.xr),
            y1: self.y1.div_ceil(comp.yr),
        }
    }
}

/// Emplacement des en-têtes de paquets.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Packed {
    InLine,
    Ppt,
    Ppm,
}

/// Paramètres d'encodage.
#[derive(Clone)]
pub struct Params {
    pub levels: u8,
    pub reversible: bool,
    pub xcb: u8,
    pub ycb: u8,
    pub cbstyle: u8,
    pub mct: bool,
    /// Taille de tuile (0 = une seule tuile).
    pub tile: (u32, u32),
    pub progression: Progression,
    pub layers: u16,
    pub sop: bool,
    pub eph: bool,
    pub precincts: Option<Vec<(u8, u8)>>,
    /// Δb = 2^−quant_shift pour le 9-7.
    pub quant_shift: u8,
    pub guard: u8,
    /// Niveaux de décomposition différents pour la composante 1 (COC).
    pub coc_levels_c1: Option<u8>,
    pub poc: Vec<PocEntry>,
    pub tile_parts: usize,
    pub packed: Packed,
    /// Décalage ROI appliqué aux blocs d'indice pair (RGN sur chaque composante).
    pub roi_shift: u8,
    /// Enveloppe JP2 : espace énuméré et boîtes supplémentaires brutes
    /// (pclr, cmap, cdef) à insérer dans jp2h.
    pub jp2: Option<(u32, Vec<u8>)>,
    pub comment: bool,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            levels: 2,
            reversible: true,
            xcb: 6,
            ycb: 6,
            cbstyle: 0,
            mct: true,
            tile: (0, 0),
            progression: Progression::Lrcp,
            layers: 1,
            sop: false,
            eph: false,
            precincts: None,
            quant_shift: 1,
            guard: 2,
            coc_levels_c1: None,
            poc: Vec::new(),
            tile_parts: 1,
            packed: Packed::InLine,
            roi_shift: 0,
            jp2: None,
            comment: false,
        }
    }
}

fn coding_style(p: &Params, levels: u8) -> CodingStyle {
    let precincts = match &p.precincts {
        Some(v) => (0..=levels)
            .map(|r| v.get(r as usize).copied().unwrap_or((15, 15)))
            .collect(),
        None => vec![(15, 15); levels as usize + 1],
    };
    CodingStyle {
        levels,
        xcb: p.xcb,
        ycb: p.ycb,
        cbstyle: p.cbstyle,
        reversible: p.reversible,
        precincts,
    }
}

/// Exposants / mantisses de quantification par sous-bande (E.1).
fn quant_for(p: &Params, precision: u8, levels: u8, chroma_rct: bool) -> Quant {
    let mut steps = Vec::new();
    let nbands = 3 * usize::from(levels) + 1;
    for i in 0..nbands {
        let gain = if i == 0 {
            0
        } else if (i - 1) % 3 == 2 {
            2
        } else {
            1
        };
        if p.reversible {
            // εb = précision + gain + 1 (+1 pour les chromas RCT).
            steps.push((precision + gain + 1 + u8::from(chroma_rct), 0));
        } else {
            let rb = precision + gain;
            steps.push((rb + p.quant_shift, 0));
        }
    }
    Quant {
        guard: p.guard,
        style: if p.reversible {
            QuantStyle::None
        } else {
            QuantStyle::Expounded
        },
        steps,
    }
}

// ---------------------------------------------------------------------------
// Codage d'une tuile
// ---------------------------------------------------------------------------

/// Bloc codé avec son emplacement (composante, résolution, précinct, bande, index).
struct TileBlocks {
    /// Indexé comme les structures : comps[c].resolutions[r].precincts[p].bands[b].blocks[k].
    blocks: Vec<Vec<Vec<Vec<Vec<EncodedBlock>>>>>,
}

/// Quantification (E.1.1) d'une sous-bande 9-7 : q = signe · ⌊|y| / Δb⌋.
fn quantize(band: &structure::Band, precision: u8, y: &[f32]) -> Vec<i32> {
    let rb = f32::from(precision) + f32::from(band.gain_log2);
    let delta = 2f32.powf(rb - f32::from(band.epsilon)) * (1.0 + f32::from(band.mu) / 2048.0);
    y.iter()
        .map(|&v| {
            let q = (v.abs() / delta).floor() as i32;
            if v < 0.0 {
                -q
            } else {
                q
            }
        })
        .collect()
}

/// Coefficients d'une composante de tuile, par résolution puis sous-bande
/// (LL pour r = 0 ; HL, LH, HH sinon), quantifiés en entiers.
fn transform_component(tc: &TileComponent, samples: Samples) -> Vec<Vec<Vec<i32>>> {
    let levels = tc.style.levels as usize;
    let mut per_res: Vec<Vec<Vec<i32>>> = vec![Vec::new(); levels + 1];
    match samples {
        Samples::Int(mut cur) => {
            for lev in 0..levels {
                let r = levels - lev;
                let rect = tc.resolutions[r].rect;
                let [ll, hl, lh, hh] = analyze_level(rect, &cur, analyze_53);
                per_res[r] = vec![hl, lh, hh];
                cur = ll;
            }
            per_res[0] = vec![cur];
        }
        Samples::Float(mut cur) => {
            let precision = tc.siz.precision;
            for lev in 0..levels {
                let r = levels - lev;
                let res = &tc.resolutions[r];
                let [ll, hl, lh, hh] = analyze_level(res.rect, &cur, analyze_97);
                per_res[r] = vec![
                    quantize(&res.bands[0], precision, &hl),
                    quantize(&res.bands[1], precision, &lh),
                    quantize(&res.bands[2], precision, &hh),
                ];
                cur = ll;
            }
            per_res[0] = vec![quantize(&tc.resolutions[0].bands[0], precision, &cur)];
        }
    }
    per_res
}

enum Samples {
    Int(Vec<i32>),
    Float(Vec<f32>),
}

/// Encode les blocs d'une tuile.
fn encode_tile_blocks(img: &Image, comps: &[TileComponent], p: &Params) -> TileBlocks {
    let ncomp = comps.len();
    // Échantillons de chaque composante de tuile, décalés (DC) et MCT.
    let mut planes: Vec<Vec<i32>> = Vec::with_capacity(ncomp);
    for (c, tc) in comps.iter().enumerate() {
        let comp = &img.components[c];
        let full = img.comp_rect(c);
        let fw = full.width() as usize;
        let mut v = Vec::with_capacity(tc.rect.area() as usize);
        let offset = if comp.signed {
            0
        } else {
            1 << (comp.precision - 1)
        };
        for y in tc.rect.y0..tc.rect.y1 {
            for x in tc.rect.x0..tc.rect.x1 {
                let sx = (x - full.x0) as usize;
                let sy = (y - full.y0) as usize;
                v.push(comp.data[sy * fw + sx] - offset);
            }
        }
        planes.push(v);
    }
    let mut samples: Vec<Samples> = Vec::with_capacity(ncomp);
    if p.reversible {
        if p.mct && ncomp >= 3 {
            forward_rct(&mut planes[..3]);
        }
        samples.extend(planes.into_iter().map(Samples::Int));
    } else {
        let mut f: Vec<Vec<f32>> = planes
            .into_iter()
            .map(|v| v.into_iter().map(|x| x as f32).collect())
            .collect();
        if p.mct && ncomp >= 3 {
            forward_ict(&mut f[..3]);
        }
        samples.extend(f.into_iter().map(Samples::Float));
    }
    let mut engine = BlockCoder::new();
    let mut blocks = Vec::with_capacity(ncomp);
    for (c, tc) in comps.iter().enumerate() {
        let per_res = transform_component(
            tc,
            std::mem::replace(&mut samples[c], Samples::Int(Vec::new())),
        );
        let mut res_blocks = Vec::new();
        let roi = p.roi_shift;
        let mut block_counter = 0usize;
        for (r, res) in tc.resolutions.iter().enumerate() {
            let mut prec_blocks = Vec::new();
            for precinct in &res.precincts {
                let mut band_blocks = Vec::new();
                for (b, pb) in precinct.bands.iter().enumerate() {
                    let band = &res.bands[b];
                    let coeffs = &per_res[r][b];
                    let bw = band.rect.width() as usize;
                    let mb =
                        u32::from(tc.quant.guard) + u32::from(band.epsilon) - 1 + u32::from(roi);
                    let mut encoded = Vec::new();
                    for cb in &pb.blocks {
                        let w = cb.rect.width() as usize;
                        let h = cb.rect.height() as usize;
                        let mut mags = Vec::with_capacity(w * h);
                        let mut negs = Vec::with_capacity(w * h);
                        let shift = if roi > 0 && block_counter % 2 == 0 {
                            roi
                        } else {
                            0
                        };
                        for y in 0..h {
                            for x in 0..w {
                                let v = coeffs[((cb.rect.y0 - band.rect.y0) as usize + y) * bw
                                    + (cb.rect.x0 - band.rect.x0) as usize
                                    + x];
                                mags.push(v.unsigned_abs() << shift);
                                negs.push(v < 0);
                            }
                        }
                        block_counter += 1;
                        encoded.push(encode_block(
                            &mut engine,
                            w,
                            h,
                            &mags,
                            &negs,
                            band.kind,
                            p.cbstyle,
                            mb,
                        ));
                    }
                    band_blocks.push(encoded);
                }
                prec_blocks.push(band_blocks);
            }
            res_blocks.push(prec_blocks);
        }
        blocks.push(res_blocks);
    }
    TileBlocks { blocks }
}

/// Un paquet écrit : en-tête (avec EPH éventuel) et corps (avec SOP éventuel).
struct Packet {
    header: Vec<u8>,
    body: Vec<u8>,
}

/// État d'inclusion d'un bloc pendant l'écriture des paquets.
#[derive(Clone, Copy)]
struct BlockState {
    included: bool,
    lblock: u32,
    /// Passes déjà émises.
    sent: u32,
}

/// Nombre de passes du bloc envoyées jusqu'à la couche `l` incluse (bornes
/// cumulées) : avec plusieurs couches, la moitié des passes (arrondie vers
/// le bas) dans la première couche pour les blocs pairs, tout dans la
/// dernière pour les blocs impairs.
fn passes_through_layer(total: u32, layers: u16, l: u16, block_index: usize) -> u32 {
    if layers <= 1 || l + 1 >= layers {
        return total;
    }
    if block_index % 2 == 1 {
        return 0;
    }
    total * u32::from(l + 1) / u32::from(layers)
}

fn write_pass_count(w: &mut BitWriter, n: u32) {
    match n {
        1 => w.bit(0),
        2 => w.bits(0b10, 2),
        3..=5 => {
            w.bits(0b11, 2);
            w.bits(n - 3, 2);
        }
        6..=36 => {
            w.bits(0b1111, 4);
            w.bits(n - 6, 5);
        }
        _ => {
            w.bits(0b1111_11111, 9);
            w.bits(n - 37, 7);
        }
    }
}

struct PacketWriter<'a> {
    comps: &'a [TileComponent],
    blocks: &'a TileBlocks,
    states: Vec<Vec<Vec<Vec<Vec<BlockState>>>>>,
    inclusion: Vec<Vec<Vec<Vec<TagTreeEncoder>>>>,
    zero: Vec<Vec<Vec<Vec<TagTreeEncoder>>>>,
    layers: u16,
    sop: bool,
    eph: bool,
    nsop: u16,
}

impl<'a> PacketWriter<'a> {
    fn new(comps: &'a [TileComponent], blocks: &'a TileBlocks, p: &Params) -> Self {
        let mut states = Vec::new();
        let mut inclusion = Vec::new();
        let mut zero = Vec::new();
        for (c, tc) in comps.iter().enumerate() {
            let mut sr = Vec::new();
            let mut ir = Vec::new();
            let mut zr = Vec::new();
            for (r, res) in tc.resolutions.iter().enumerate() {
                let mut sp = Vec::new();
                let mut ip = Vec::new();
                let mut zp = Vec::new();
                for (pi, precinct) in res.precincts.iter().enumerate() {
                    let mut sb = Vec::new();
                    let mut ib = Vec::new();
                    let mut zb = Vec::new();
                    for (b, pb) in precinct.bands.iter().enumerate() {
                        let enc = &blocks.blocks[c][r][pi][b];
                        let bw = pb.blocks_wide as usize;
                        let bh = pb.blocks.len().checked_div(bw).unwrap_or(0);
                        // Couche de première inclusion : première couche avec des passes.
                        let first_layer: Vec<u32> = enc
                            .iter()
                            .enumerate()
                            .map(|(k, e)| {
                                (0..p.layers)
                                    .find(|&l| passes_through_layer(e.passes, p.layers, l, k) > 0)
                                    .map_or(u32::from(p.layers) + 1, u32::from)
                            })
                            .collect();
                        let zeros: Vec<u32> = enc.iter().map(|e| e.zero_planes).collect();
                        sb.push(vec![
                            BlockState {
                                included: false,
                                lblock: 3,
                                sent: 0
                            };
                            enc.len()
                        ]);
                        ib.push(TagTreeEncoder::new(bw, bh, &first_layer));
                        zb.push(TagTreeEncoder::new(bw, bh, &zeros));
                    }
                    sp.push(sb);
                    ip.push(ib);
                    zp.push(zb);
                }
                sr.push(sp);
                ir.push(ip);
                zr.push(zp);
            }
            states.push(sr);
            inclusion.push(ir);
            zero.push(zr);
        }
        PacketWriter {
            comps,
            blocks,
            states,
            inclusion,
            zero,
            layers: p.layers,
            sop: p.sop,
            eph: p.eph,
            nsop: 0,
        }
    }

    /// Écrit le paquet (l, r, c, p) (B.10).
    fn packet(&mut self, l: u16, r: usize, c: usize, pi: usize) -> Packet {
        let precinct = &self.comps[c].resolutions[r].precincts[pi];
        let style = self.comps[c].style.cbstyle;
        let mut w = BitWriter::new();
        let mut body = Vec::new();
        let mut any = false;
        for (b, pb) in precinct.bands.iter().enumerate() {
            for k in 0..pb.blocks.len() {
                let e = &self.blocks.blocks[c][r][pi][b][k];
                let st = self.states[c][r][pi][b][k];
                if passes_through_layer(e.passes, self.layers, l, k) > st.sent {
                    any = true;
                }
            }
        }
        if !any {
            w.bit(0);
        } else {
            w.bit(1);
            for (b, pb) in precinct.bands.iter().enumerate() {
                let bw = pb.blocks_wide as usize;
                for k in 0..pb.blocks.len() {
                    let (bx, by) = (k % bw.max(1), k / bw.max(1));
                    let e = &self.blocks.blocks[c][r][pi][b][k];
                    let st = &mut self.states[c][r][pi][b][k];
                    let target = passes_through_layer(e.passes, self.layers, l, k);
                    let new_passes = target - st.sent;
                    if st.included {
                        w.bit(u32::from(new_passes > 0));
                        if new_passes == 0 {
                            continue;
                        }
                    } else {
                        let inc =
                            self.inclusion[c][r][pi][b].encode(&mut w, bx, by, u32::from(l) + 1);
                        if !inc || new_passes == 0 {
                            continue;
                        }
                        st.included = true;
                        let mut t = 1;
                        while !self.zero[c][r][pi][b].encode(&mut w, bx, by, t) {
                            t += 1;
                        }
                        st.lblock = 3;
                    }
                    write_pass_count(&mut w, new_passes);
                    // Segments couverts par ces passes.
                    let mut segs: Vec<(usize, u32)> = Vec::new();
                    let mut pass_at = 0u32;
                    let mut byte_at = 0usize;
                    let mut range_start = 0usize;
                    let mut found_start = false;
                    for &(len, np) in &e.segments {
                        let seg_start = pass_at;
                        let seg_end = pass_at + np;
                        if seg_end > st.sent && seg_start < target {
                            if !found_start {
                                range_start = byte_at;
                                found_start = true;
                            }
                            let lo = seg_start.max(st.sent);
                            let hi = seg_end.min(target);
                            segs.push((len, hi - lo));
                        }
                        pass_at = seg_end;
                        byte_at += len;
                    }
                    let mut total_len = 0;
                    for (len, np) in &segs {
                        while st.lblock + np.ilog2() < bits_needed(*len) {
                            w.bit(1);
                            st.lblock += 1;
                        }
                        total_len += len;
                    }
                    w.bit(0);
                    for (len, np) in &segs {
                        w.bits(*len as u32, st.lblock + np.ilog2());
                    }
                    body.extend_from_slice(&e.data[range_start..range_start + total_len]);
                    st.sent = target;
                }
            }
        }
        let _ = style;
        let mut header = w.finish();
        if self.eph {
            header.extend_from_slice(&[0xFF, 0x92]);
        }
        let mut full_body = Vec::new();
        if self.sop {
            full_body.extend_from_slice(&[0xFF, 0x91, 0, 4]);
            full_body.extend_from_slice(&self.nsop.to_be_bytes());
            self.nsop = self.nsop.wrapping_add(1);
        }
        full_body.extend_from_slice(&body);
        Packet {
            header,
            body: full_body,
        }
    }
}

fn bits_needed(v: usize) -> u32 {
    if v == 0 {
        1
    } else {
        usize::BITS - v.leading_zeros()
    }
}

/// Suite des paquets (l, r, c, p) d'une tuile dans l'ordre de progression,
/// avec les volumes POC puis le volume complet (B.12).
fn packet_sequence(
    comps: &[TileComponent],
    tile: Rect,
    p: &Params,
) -> Vec<(u16, usize, usize, usize)> {
    struct Vol {
        rs: usize,
        re: usize,
        cs: usize,
        ce: usize,
        le: u16,
        order: Progression,
    }
    let max_res = comps.iter().map(|c| c.resolutions.len()).max().unwrap();
    let mut vols: Vec<Vol> = p
        .poc
        .iter()
        .map(|e| Vol {
            rs: e.res_start as usize,
            re: (e.res_end as usize).min(max_res),
            cs: e.comp_start as usize,
            ce: (e.comp_end as usize).min(comps.len()),
            le: e.layer_end.min(p.layers),
            order: e.progression,
        })
        .collect();
    vols.push(Vol {
        rs: 0,
        re: max_res,
        cs: 0,
        ce: comps.len(),
        le: p.layers,
        order: p.progression,
    });
    let mut done: Vec<Vec<Vec<u16>>> = comps
        .iter()
        .map(|c| {
            c.resolutions
                .iter()
                .map(|r| vec![0u16; r.precinct_count() as usize])
                .collect()
        })
        .collect();
    let mut seq = Vec::new();
    let anchor = |c: usize, r: usize, pi: usize| -> (u64, u64) {
        let comp = &comps[c];
        let res = &comp.resolutions[r];
        let pw = u64::from(res.precincts_wide.max(1));
        let (px, py) = (pi as u64 % pw, pi as u64 / pw);
        let shift = u32::from(comp.style.levels) - r as u32;
        let rx = ((u64::from(res.rect.x0 >> res.ppx) + px) << res.ppx) << shift;
        let ry = ((u64::from(res.rect.y0 >> res.ppy) + py) << res.ppy) << shift;
        (
            (rx * u64::from(comp.siz.xr)).max(u64::from(tile.x0)),
            (ry * u64::from(comp.siz.yr)).max(u64::from(tile.y0)),
        )
    };
    for v in &vols {
        let mut emit = |l: u16, r: usize, c: usize, pi: usize, seq: &mut Vec<_>| {
            if done[c][r][pi] == l {
                done[c][r][pi] += 1;
                seq.push((l, r, c, pi));
            }
        };
        match v.order {
            Progression::Lrcp => {
                for l in 0..v.le {
                    for r in v.rs..v.re {
                        for c in v.cs..v.ce {
                            if r < comps[c].resolutions.len() {
                                for pi in 0..comps[c].resolutions[r].precinct_count() as usize {
                                    emit(l, r, c, pi, &mut seq);
                                }
                            }
                        }
                    }
                }
            }
            Progression::Rlcp => {
                for r in v.rs..v.re {
                    for l in 0..v.le {
                        for c in v.cs..v.ce {
                            if r < comps[c].resolutions.len() {
                                for pi in 0..comps[c].resolutions[r].precinct_count() as usize {
                                    emit(l, r, c, pi, &mut seq);
                                }
                            }
                        }
                    }
                }
            }
            _ => {
                let mut keys = Vec::new();
                for c in v.cs..v.ce {
                    for r in v.rs..v.re.min(comps[c].resolutions.len()) {
                        for pi in 0..comps[c].resolutions[r].precinct_count() as usize {
                            let (x, y) = anchor(c, r, pi);
                            keys.push((y, x, c, r, pi));
                        }
                    }
                }
                match v.order {
                    Progression::Rpcl => keys.sort_by_key(|&(y, x, c, r, pi)| (r, y, x, c, pi)),
                    Progression::Pcrl => keys.sort_by_key(|&(y, x, c, r, pi)| (y, x, c, r, pi)),
                    _ => keys.sort_by_key(|&(y, x, c, r, pi)| (c, y, x, r, pi)),
                }
                for (_, _, c, r, pi) in keys {
                    for l in 0..v.le {
                        emit(l, r, c, pi, &mut seq);
                    }
                }
            }
        }
    }
    seq
}

// ---------------------------------------------------------------------------
// Flux de code et fichier JP2
// ---------------------------------------------------------------------------

fn segment(out: &mut Vec<u8>, marker: u16, body: &[u8]) {
    out.extend_from_slice(&marker.to_be_bytes());
    out.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(body);
}

fn spcod(style: &CodingStyle, custom: bool) -> Vec<u8> {
    let mut b = vec![
        style.levels,
        style.xcb - 2,
        style.ycb - 2,
        style.cbstyle,
        u8::from(style.reversible),
    ];
    if custom {
        for &(px, py) in &style.precincts {
            b.push(px | (py << 4));
        }
    }
    b
}

fn qcd_body(q: &Quant) -> Vec<u8> {
    let mut b = vec![
        (q.guard << 5)
            | match q.style {
                QuantStyle::None => 0,
                QuantStyle::Derived => 1,
                QuantStyle::Expounded => 2,
            },
    ];
    for &(e, mu) in &q.steps {
        match q.style {
            QuantStyle::None => b.push(e << 3),
            _ => b.extend_from_slice(&((u16::from(e) << 11) | mu).to_be_bytes()),
        }
    }
    b
}

/// Encode l'image en flux de code J2K (ou fichier JP2 si demandé).
pub fn encode(img: &Image, p: &Params) -> Vec<u8> {
    let ncomp = img.components.len();
    let mut cs = Vec::new();
    cs.extend_from_slice(&[0xFF, 0x4F]);
    // SIZ.
    let mut siz = vec![0, 0];
    let (tw, th) = if p.tile == (0, 0) {
        (img.x1, img.y1)
    } else {
        p.tile
    };
    for v in [img.x1, img.y1, img.x0, img.y0, tw, th, 0, 0] {
        siz.extend_from_slice(&v.to_be_bytes());
    }
    siz.extend_from_slice(&(ncomp as u16).to_be_bytes());
    for c in &img.components {
        siz.push((c.precision - 1) | if c.signed { 0x80 } else { 0 });
        siz.push(c.xr as u8);
        siz.push(c.yr as u8);
    }
    segment(&mut cs, 0xFF51, &siz);
    // COD.
    let style0 = coding_style(p, p.levels);
    let custom = p.precincts.is_some();
    let mut cod = vec![
        u8::from(custom) | (u8::from(p.sop) << 1) | (u8::from(p.eph) << 2),
        match p.progression {
            Progression::Lrcp => 0,
            Progression::Rlcp => 1,
            Progression::Rpcl => 2,
            Progression::Pcrl => 3,
            Progression::Cprl => 4,
        },
    ];
    cod.extend_from_slice(&p.layers.to_be_bytes());
    cod.push(u8::from(p.mct));
    cod.extend(spcod(&style0, custom));
    segment(&mut cs, 0xFF52, &cod);
    let mut styles: Vec<CodingStyle> = vec![style0.clone(); ncomp];
    if let Some(lv) = p.coc_levels_c1 {
        let style1 = coding_style(p, lv);
        let mut coc = vec![1, u8::from(custom)];
        coc.extend(spcod(&style1, custom));
        segment(&mut cs, 0xFF53, &coc);
        styles[1] = style1;
    }
    // QCD (composante 0) puis QCC pour les autres si nécessaire.
    let quants: Vec<Quant> = (0..ncomp)
        .map(|c| {
            let chroma = p.reversible && p.mct && ncomp >= 3 && (c == 1 || c == 2);
            quant_for(p, img.components[c].precision, styles[c].levels, chroma)
        })
        .collect();
    segment(&mut cs, 0xFF5C, &qcd_body(&quants[0]));
    for c in 1..ncomp {
        if quants[c] != quants[0] {
            let mut qcc = vec![c as u8];
            qcc.extend(qcd_body(&quants[c]));
            segment(&mut cs, 0xFF5D, &qcc);
        }
    }
    if p.roi_shift > 0 {
        for c in 0..ncomp {
            segment(&mut cs, 0xFF5E, &[c as u8, 0, p.roi_shift]);
        }
    }
    if !p.poc.is_empty() {
        let mut poc = Vec::new();
        for e in &p.poc {
            poc.push(e.res_start);
            poc.push(e.comp_start as u8);
            poc.extend_from_slice(&e.layer_end.to_be_bytes());
            poc.push(e.res_end);
            poc.push(e.comp_end as u8);
            poc.push(match e.progression {
                Progression::Lrcp => 0,
                Progression::Rlcp => 1,
                Progression::Rpcl => 2,
                Progression::Pcrl => 3,
                Progression::Cprl => 4,
            });
        }
        segment(&mut cs, 0xFF5F, &poc);
    }
    if p.comment {
        let mut com = vec![0, 1];
        com.extend_from_slice(b"Acrux test encoder");
        segment(&mut cs, 0xFF64, &com);
        // TLM factice (ignoré par le décodeur).
        segment(&mut cs, 0xFF55, &[0, 0x40, 0, 0, 0, 0]);
    }
    // Tuiles.
    let tiles_wide = img.x1.div_ceil(tw);
    let tiles_high = img.y1.div_ceil(th);
    let mut ppm: Vec<u8> = Vec::new();
    let mut tile_streams: Vec<Vec<u8>> = Vec::new();
    for ty in 0..tiles_high {
        for tx in 0..tiles_wide {
            let tile = Rect {
                x0: (tx * tw).max(img.x0),
                y0: (ty * th).max(img.y0),
                x1: ((tx + 1) * tw).min(img.x1),
                y1: ((ty + 1) * th).min(img.y1),
            };
            let mut budget = Budget::default();
            let comps: Vec<TileComponent> = (0..ncomp)
                .map(|c| {
                    structure::build_tile_component(
                        tile,
                        ComponentSiz {
                            precision: img.components[c].precision,
                            signed: img.components[c].signed,
                            xr: img.components[c].xr,
                            yr: img.components[c].yr,
                        },
                        &styles[c],
                        &quants[c],
                        p.roi_shift,
                        &mut budget,
                    )
                    .unwrap()
                })
                .collect();
            let blocks = encode_tile_blocks(img, &comps, p);
            let mut writer = PacketWriter::new(&comps, &blocks, p);
            let seq = packet_sequence(&comps, tile, p);
            let packets: Vec<Packet> = seq
                .iter()
                .map(|&(l, r, c, pi)| writer.packet(l, r, c, pi))
                .collect();
            let tile_index = (ty * tiles_wide + tx) as u16;
            let parts = p.tile_parts.max(1).min(packets.len().max(1));
            let per_part = packets.len().div_ceil(parts);
            for (tp, chunk) in packets.chunks(per_part.max(1)).enumerate() {
                let mut headers = Vec::new();
                let mut body = Vec::new();
                for pk in chunk {
                    match p.packed {
                        Packed::InLine => {
                            body.extend_from_slice(&pk.body[..pk.body.len() - pk.body.len()]);
                            // SOP précède l'en-tête ; le corps vient après.
                            if p.sop {
                                body.extend_from_slice(&pk.body[..6]);
                                body.extend_from_slice(&pk.header);
                                body.extend_from_slice(&pk.body[6..]);
                            } else {
                                body.extend_from_slice(&pk.header);
                                body.extend_from_slice(&pk.body);
                            }
                        }
                        Packed::Ppt | Packed::Ppm => {
                            headers.extend_from_slice(&pk.header);
                            body.extend_from_slice(&pk.body);
                        }
                    }
                }
                let mut th_seg = Vec::new();
                if p.packed == Packed::Ppt {
                    let mut ppt = vec![0u8];
                    ppt.extend_from_slice(&headers);
                    segment(&mut th_seg, 0xFF61, &ppt);
                } else if p.packed == Packed::Ppm {
                    ppm.extend_from_slice(&(headers.len() as u32).to_be_bytes());
                    ppm.extend_from_slice(&headers);
                }
                if p.comment {
                    segment(&mut th_seg, 0xFF58, &[0, 0x80]);
                }
                let mut tile_stream = Vec::new();
                let psot = 12 + th_seg.len() + 2 + body.len();
                let mut sot = Vec::new();
                sot.extend_from_slice(&tile_index.to_be_bytes());
                sot.extend_from_slice(&(psot as u32).to_be_bytes());
                sot.push(tp as u8);
                sot.push(parts as u8);
                segment(&mut tile_stream, 0xFF90, &sot);
                tile_stream.extend_from_slice(&th_seg);
                tile_stream.extend_from_slice(&[0xFF, 0x93]);
                tile_stream.extend_from_slice(&body);
                tile_streams.push(tile_stream);
            }
        }
    }
    if p.packed == Packed::Ppm {
        // Découpé en deux segments PPM pour tester la concaténation Zppm.
        let mid = ppm.len() / 2;
        let mut s0 = vec![0u8];
        s0.extend_from_slice(&ppm[..mid]);
        segment(&mut cs, 0xFF60, &s0);
        let mut s1 = vec![1u8];
        s1.extend_from_slice(&ppm[mid..]);
        segment(&mut cs, 0xFF60, &s1);
    }
    for t in tile_streams {
        cs.extend_from_slice(&t);
    }
    cs.extend_from_slice(&[0xFF, 0xD9]);
    match &p.jp2 {
        None => cs,
        Some((enum_cs, extra)) => wrap_jp2(&cs, img, *enum_cs, extra),
    }
}

pub fn make_box(t: &[u8; 4], content: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&((content.len() + 8) as u32).to_be_bytes());
    b.extend_from_slice(t);
    b.extend_from_slice(content);
    b
}

fn wrap_jp2(cs: &[u8], img: &Image, enum_cs: u32, extra: &[u8]) -> Vec<u8> {
    let mut file = vec![
        0x00, 0x00, 0x00, 0x0C, 0x6A, 0x50, 0x20, 0x20, 0x0D, 0x0A, 0x87, 0x0A,
    ];
    file.extend(make_box(b"ftyp", b"jp2 \0\0\0\0jp2 "));
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(img.y1 - img.y0).to_be_bytes());
    ihdr.extend_from_slice(&(img.x1 - img.x0).to_be_bytes());
    ihdr.extend_from_slice(&(img.components.len() as u16).to_be_bytes());
    ihdr.extend_from_slice(&[img.components[0].precision - 1, 7, 0, 0]);
    let mut colr = vec![1, 0, 0];
    colr.extend_from_slice(&enum_cs.to_be_bytes());
    let mut jp2h = make_box(b"ihdr", &ihdr);
    jp2h.extend(make_box(b"colr", &colr));
    jp2h.extend_from_slice(extra);
    file.extend(make_box(b"jp2h", &jp2h));
    file.extend(make_box(b"jp2c", cs));
    file
}

/// Vérifie l'inclusion du module tier1 (évite un avertissement d'import).
#[allow(dead_code)]
fn _uses(_: tier1::PassKind) {}
