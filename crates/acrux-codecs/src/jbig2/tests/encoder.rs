//! Encodeur JBIG2 de test, écrit d'après T.88 : codeur arithmétique MQ
//! (annexe E, procédures CODELPS/CODEMPS/BYTEOUT/FLUSH), procédures de
//! codage d'entiers (annexe A, à rebours du décodage), région générique
//! (§6.2, contexte formé pixel par pixel d'après les figures 4 à 8, sans
//! partager le code du décodeur), raffinement (§6.3), dictionnaire de
//! symboles et région de texte (§6.5, §6.4) arithmétiques et Huffman,
//! dictionnaire de motifs et demi-teintes (§6.7, §6.6), en-têtes de segment
//! (§7.2).

use super::super::huffman::{self, Line, LineKind, Table};
use super::super::mq::QE_TABLE;
use crate::ccitt::tests::encoder::{encode_g4, BitWriter, Image};

// ---------------------------------------------------------------------------
// Codeur MQ (annexe E)
// ---------------------------------------------------------------------------

pub struct MqEncoder {
    out: Vec<u8>,
    c: u32,
    a: u32,
    ct: u32,
}

impl Default for MqEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl MqEncoder {
    /// INITENC (figure E.3) : B désigne l'octet fictif précédant le début.
    pub fn new() -> Self {
        MqEncoder {
            out: vec![0x00],
            c: 0,
            a: 0x8000,
            ct: 12,
        }
    }

    fn b(&self) -> u8 {
        *self.out.last().unwrap()
    }

    fn set_b(&mut self, v: u8) {
        let i = self.out.len() - 1;
        self.out[i] = v;
    }

    /// CODEMPS / CODELPS (figures E.5, E.6).
    pub fn encode(&mut self, cx: &mut u8, d: u8) {
        let index = usize::from(*cx >> 1);
        let mps = *cx & 1;
        let e = &QE_TABLE[index];
        let qe = e.qe;
        if d == mps {
            self.a -= qe;
            if self.a & 0x8000 == 0 {
                if self.a < qe {
                    self.a = qe;
                } else {
                    self.c += qe;
                }
                *cx = (e.nmps << 1) | mps;
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
            *cx = (e.nlps << 1) | new_mps;
            self.renorm();
        }
    }

    /// RENORME (figure E.8) : décalage, puis BYTEOUT dès que CT atteint 0
    /// (l'ordre inverse du décodeur, figure E.21).
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

    /// BYTEOUT (figure E.9).
    fn byte_out(&mut self) {
        if self.b() == 0xFF {
            self.stuff();
        } else if self.c < 0x800_0000 {
            self.emit();
        } else {
            let b = self.b() + 1;
            self.set_b(b);
            if b == 0xFF {
                self.c &= 0x7FF_FFFF;
                self.stuff();
            } else {
                self.emit();
            }
        }
    }

    fn stuff(&mut self) {
        self.out.push((self.c >> 20) as u8);
        self.c &= 0xF_FFFF;
        self.ct = 7;
    }

    fn emit(&mut self) {
        self.out.push((self.c >> 19) as u8);
        self.c &= 0x7_FFFF;
        self.ct = 8;
    }

    /// FLUSH (figure E.11) avec SETBITS, puis marqueur FF AC.
    pub fn finish(mut self) -> Vec<u8> {
        let tempc = self.c + self.a;
        self.c |= 0xFFFF;
        if self.c >= tempc {
            self.c -= 0x8000;
        }
        self.c <<= self.ct;
        self.byte_out();
        self.c <<= self.ct;
        self.byte_out();
        if self.b() != 0xFF {
            self.out.push(0xFF);
        }
        self.out.push(0xAC);
        assert_eq!(self.out[0], 0, "retenue propagée dans l'octet fictif");
        self.out.remove(0);
        self.out
    }
}

// ---------------------------------------------------------------------------
// Entiers (annexe A)
// ---------------------------------------------------------------------------

pub struct IntEncoder {
    cx: [u8; 512],
}

impl Default for IntEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl IntEncoder {
    pub fn new() -> Self {
        IntEncoder { cx: [0; 512] }
    }

    fn bit(&mut self, mq: &mut MqEncoder, prev: &mut usize, d: u32) {
        mq.encode(&mut self.cx[*prev], d as u8);
        *prev = if *prev < 256 {
            (*prev << 1) | d as usize
        } else {
            (((*prev << 1) | d as usize) & 511) | 256
        };
    }

    /// Code `value`, ou OOB pour `None` (A.2 à rebours).
    pub fn encode(&mut self, mq: &mut MqEncoder, value: Option<i32>) {
        let mut prev = 1usize;
        let (s, a) = match value {
            None => (1, 0u32),
            Some(v) => (u32::from(v < 0), v.unsigned_abs()),
        };
        self.bit(mq, &mut prev, s);
        let (prefix, nbits, offset): (&[u32], u32, u32) = if a <= 3 {
            (&[0], 2, 0)
        } else if a <= 19 {
            (&[1, 0], 4, 4)
        } else if a <= 83 {
            (&[1, 1, 0], 6, 20)
        } else if a <= 339 {
            (&[1, 1, 1, 0], 8, 84)
        } else if a <= 4435 {
            (&[1, 1, 1, 1, 0], 12, 340)
        } else {
            (&[1, 1, 1, 1, 1], 32, 4436)
        };
        for &p in prefix {
            self.bit(mq, &mut prev, p);
        }
        let v = a - offset;
        for i in (0..nbits).rev() {
            self.bit(mq, &mut prev, (v >> i) & 1);
        }
    }
}

pub struct IaidEncoder {
    cx: Vec<u8>,
    code_len: u32,
}

impl IaidEncoder {
    pub fn new(code_len: u32) -> Self {
        IaidEncoder {
            cx: vec![0; 1 << (code_len + 1)],
            code_len,
        }
    }

    pub fn encode(&mut self, mq: &mut MqEncoder, id: u32) {
        let mut prev = 1usize;
        for i in (0..self.code_len).rev() {
            let bit = ((id >> i) & 1) as u8;
            mq.encode(&mut self.cx[prev], bit);
            prev = (prev << 1) | usize::from(bit);
        }
    }
}

// ---------------------------------------------------------------------------
// Région générique (§6.2) : contexte formé d'après les figures 4 à 8
// ---------------------------------------------------------------------------

/// Pixels du gabarit dans l'ordre du contexte (poids fort en premier), les
/// pixels adaptatifs étant désignés par `At(n)`.
#[derive(Clone, Copy)]
enum Pix {
    Fixed(i32, i32),
    At(usize),
}

fn template_pixels(template: u8) -> Vec<Pix> {
    use Pix::{At, Fixed};
    match template {
        0 => vec![
            At(3),
            Fixed(-1, -2),
            Fixed(0, -2),
            Fixed(1, -2),
            At(2),
            At(1),
            Fixed(-2, -1),
            Fixed(-1, -1),
            Fixed(0, -1),
            Fixed(1, -1),
            Fixed(2, -1),
            At(0),
            Fixed(-4, 0),
            Fixed(-3, 0),
            Fixed(-2, 0),
            Fixed(-1, 0),
        ],
        1 => vec![
            Fixed(-1, -2),
            Fixed(0, -2),
            Fixed(1, -2),
            Fixed(2, -2),
            Fixed(-2, -1),
            Fixed(-1, -1),
            Fixed(0, -1),
            Fixed(1, -1),
            Fixed(2, -1),
            At(0),
            Fixed(-3, 0),
            Fixed(-2, 0),
            Fixed(-1, 0),
        ],
        2 => vec![
            Fixed(-1, -2),
            Fixed(0, -2),
            Fixed(1, -2),
            Fixed(-2, -1),
            Fixed(-1, -1),
            Fixed(0, -1),
            Fixed(1, -1),
            At(0),
            Fixed(-2, 0),
            Fixed(-1, 0),
        ],
        _ => vec![
            Fixed(-3, -1),
            Fixed(-2, -1),
            Fixed(-1, -1),
            Fixed(0, -1),
            Fixed(1, -1),
            At(0),
            Fixed(-4, 0),
            Fixed(-3, 0),
            Fixed(-2, 0),
            Fixed(-1, 0),
        ],
    }
}

pub const NOMINAL_AT: [(i8, i8); 4] = [(3, -1), (-3, -1), (2, -2), (-2, -2)];

fn pixel(img: &Image, x: i32, y: i32) -> u32 {
    if x < 0 || y < 0 || x as usize >= img.width || y as usize >= img.height {
        0
    } else {
        u32::from(img.pixels[y as usize * img.width + x as usize])
    }
}

pub struct GenericEncoder {
    pub cx: Vec<u8>,
}

impl Default for GenericEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl GenericEncoder {
    pub fn new() -> Self {
        GenericEncoder {
            cx: vec![0; 1 << 16],
        }
    }

    /// Code `img` avec le gabarit et les pixels adaptatifs donnés.
    pub fn encode(
        &mut self,
        mq: &mut MqEncoder,
        img: &Image,
        template: u8,
        at: [(i8, i8); 4],
        tpgdon: bool,
    ) {
        self.encode_with_skip(mq, img, template, at, tpgdon, None);
    }

    /// Variante avec pixels sautés (§6.6.5.1, HSKIP) : ces pixels ne sont
    /// pas codés (et valent 0 dans `img`).
    pub fn encode_with_skip(
        &mut self,
        mq: &mut MqEncoder,
        img: &Image,
        template: u8,
        at: [(i8, i8); 4],
        tpgdon: bool,
        skip: Option<&Image>,
    ) {
        let pixels = template_pixels(template);
        let sltp_ctx = [0x9B25usize, 0x0795, 0x00E5, 0x0195][usize::from(template)];
        let mut ltp = false;
        for y in 0..img.height {
            if tpgdon {
                let typical = if y == 0 {
                    img.row(0).iter().all(|&p| p == 0)
                } else {
                    img.row(y) == img.row(y - 1)
                };
                mq.encode(&mut self.cx[sltp_ctx], u8::from(typical != ltp));
                ltp = typical;
                if ltp {
                    continue;
                }
            }
            for x in 0..img.width {
                if skip.is_some_and(|s| s.row(y)[x] != 0) {
                    assert_eq!(img.row(y)[x], 0, "pixel sauté non nul");
                    continue;
                }
                let mut ctx = 0usize;
                for p in &pixels {
                    let (dx, dy) = match *p {
                        Pix::Fixed(dx, dy) => (dx, dy),
                        Pix::At(i) => (i32::from(at[i].0), i32::from(at[i].1)),
                    };
                    ctx = (ctx << 1) | pixel(img, x as i32 + dx, y as i32 + dy) as usize;
                }
                mq.encode(&mut self.cx[ctx], img.pixels[y * img.width + x]);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Raffinement (§6.3)
// ---------------------------------------------------------------------------

pub struct RefinementEncoder {
    pub cx: Vec<u8>,
}

impl Default for RefinementEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl RefinementEncoder {
    pub fn new() -> Self {
        RefinementEncoder {
            cx: vec![0; 1 << 13],
        }
    }

    /// Code `img` par raffinement de `reference` décalée de `(dx, dy)`
    /// (GRREFERENCEDX/DY : le pixel (x, y) correspond à (x − dx, y − dy)).
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        mq: &mut MqEncoder,
        img: &Image,
        reference: &Image,
        dx: i32,
        dy: i32,
        template: u8,
        at: [(i8, i8); 2],
        tpgron: bool,
    ) {
        let mut ltp = false;
        for y in 0..img.height {
            if tpgron {
                // La ligne est « typique » si chaque pixel dont le voisinage
                // de référence est uniforme vaut cette valeur uniforme.
                let typical = (0..img.width).all(|x| {
                    let (rx, ry) = (x as i32 - dx, y as i32 - dy);
                    let sum: u32 = (-1..=1)
                        .flat_map(|j| (-1..=1).map(move |i| (i, j)))
                        .map(|(i, j)| pixel(reference, rx + i, ry + j))
                        .sum();
                    let p = u32::from(img.pixels[y * img.width + x]);
                    (sum != 0 || p == 0) && (sum != 9 || p == 1)
                });
                let ctx = if template == 0 { 0x0008 } else { 0x0080 };
                mq.encode(&mut self.cx[ctx], u8::from(typical != ltp));
                ltp = typical;
            }
            for x in 0..img.width {
                let (xi, yi) = (x as i32, y as i32);
                let (rx, ry) = (xi - dx, yi - dy);
                if ltp {
                    let sum: u32 = (-1..=1)
                        .flat_map(|j| (-1..=1).map(move |i| (i, j)))
                        .map(|(i, j)| pixel(reference, rx + i, ry + j))
                        .sum();
                    if sum == 0 || sum == 9 {
                        continue;
                    }
                }
                let cur = |ddx: i32, ddy: i32| pixel(img, xi + ddx, yi + ddy) as usize;
                let rf = |ddx: i32, ddy: i32| pixel(reference, rx + ddx, ry + ddy) as usize;
                let ctx = if template == 0 {
                    (cur(0, -1) << 12)
                        | (cur(1, -1) << 11)
                        | (cur(-1, 0) << 10)
                        | (cur(i32::from(at[0].0), i32::from(at[0].1)) << 9)
                        | (rf(0, -1) << 8)
                        | (rf(1, -1) << 7)
                        | (rf(-1, 0) << 6)
                        | (rf(0, 0) << 5)
                        | (rf(1, 0) << 4)
                        | (rf(-1, 1) << 3)
                        | (rf(0, 1) << 2)
                        | (rf(1, 1) << 1)
                        | rf(i32::from(at[1].0), i32::from(at[1].1))
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
                mq.encode(&mut self.cx[ctx], img.pixels[y * img.width + x]);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Segments (§7.2) et régions (§7.4.1)
// ---------------------------------------------------------------------------

pub fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// En-tête de segment (forme courte) suivi de ses données.
pub fn segment(number: u32, kind: u8, referred: &[u32], data: &[u8]) -> Vec<u8> {
    segment_with_length(number, kind, referred, data, data.len() as u32)
}

pub fn segment_with_length(
    number: u32,
    kind: u8,
    referred: &[u32],
    data: &[u8],
    length: u32,
) -> Vec<u8> {
    assert!(referred.len() <= 4);
    let mut out = Vec::new();
    out.extend_from_slice(&be32(number));
    out.push(kind);
    out.push((referred.len() as u8) << 5);
    for &r in referred {
        if number <= 256 {
            out.push(r as u8);
        } else if number <= 65536 {
            out.extend_from_slice(&(r as u16).to_be_bytes());
        } else {
            out.extend_from_slice(&be32(r));
        }
    }
    out.push(1); // page 1
    out.extend_from_slice(&be32(length));
    out.extend_from_slice(data);
    out
}

pub fn region_info(w: u32, h: u32, x: u32, y: u32, op: u8) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&be32(w));
    out.extend_from_slice(&be32(h));
    out.extend_from_slice(&be32(x));
    out.extend_from_slice(&be32(y));
    out.push(op);
    out
}

/// Segment d'informations de page (§7.4.8).
pub fn page_info(w: u32, h: u32, default_pixel: u8) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&be32(w));
    data.extend_from_slice(&be32(h));
    data.extend_from_slice(&be32(0));
    data.extend_from_slice(&be32(0));
    data.push(default_pixel << 2);
    data.extend_from_slice(&[0, 0]);
    segment(0, 48, &[], &data)
}

/// Données d'un segment de région générique (§7.4.6) en codage arithmétique.
pub fn generic_region_data(
    img: &Image,
    x: u32,
    y: u32,
    template: u8,
    at: [(i8, i8); 4],
    tpgdon: bool,
) -> Vec<u8> {
    let mut data = region_info(img.width as u32, img.height as u32, x, y, 0);
    data.push((template << 1) | (u8::from(tpgdon) << 3));
    let n = if template == 0 { 4 } else { 1 };
    for a in at.iter().take(n) {
        data.push(a.0 as u8);
        data.push(a.1 as u8);
    }
    let mut mq = MqEncoder::new();
    let mut enc = GenericEncoder::new();
    enc.encode(&mut mq, img, template, at, tpgdon);
    data.extend_from_slice(&mq.finish());
    data
}

/// Données d'un segment de région générique en codage MMR.
pub fn generic_region_mmr_data(img: &Image, x: u32, y: u32, eofb: bool) -> Vec<u8> {
    let mut data = region_info(img.width as u32, img.height as u32, x, y, 0);
    data.push(1);
    data.extend_from_slice(&encode_g4(img, eofb));
    data
}

// ---------------------------------------------------------------------------
// Dictionnaire de symboles et région de texte arithmétiques
// ---------------------------------------------------------------------------

pub fn symbol_code_len(n: usize) -> u32 {
    let mut len = 0;
    while (1usize << len) < n {
        len += 1;
    }
    len.max(1)
}

/// Données d'un segment de dictionnaire de symboles arithmétique (§7.4.3)
/// : les symboles doivent être triés par (hauteur, largeur) ; les `input`
/// symboles entrants puis les nouveaux sont tous exportés, dans cet ordre.
pub fn symbol_dict_data(symbols: &[Image], template: u8, input: usize) -> Vec<u8> {
    let mut data = Vec::new();
    let flags: u16 = u16::from(template) << 10;
    data.extend_from_slice(&flags.to_be_bytes());
    let n = if template == 0 { 4 } else { 1 };
    for a in NOMINAL_AT.iter().take(n) {
        data.push(a.0 as u8);
        data.push(a.1 as u8);
    }
    data.extend_from_slice(&be32((input + symbols.len()) as u32));
    data.extend_from_slice(&be32(symbols.len() as u32));
    let mut mq = MqEncoder::new();
    let mut gen = GenericEncoder::new();
    let mut iadh = IntEncoder::new();
    let mut iadw = IntEncoder::new();
    let mut iaex = IntEncoder::new();
    let mut prev_h = 0i32;
    let mut i = 0;
    while i < symbols.len() {
        let h = symbols[i].height as i32;
        iadh.encode(&mut mq, Some(h - prev_h));
        prev_h = h;
        let mut prev_w = 0i32;
        while i < symbols.len() && symbols[i].height as i32 == h {
            let w = symbols[i].width as i32;
            iadw.encode(&mut mq, Some(w - prev_w));
            prev_w = w;
            gen.encode(&mut mq, &symbols[i], template, NOMINAL_AT, false);
            i += 1;
        }
        iadw.encode(&mut mq, None);
    }
    // Exportation : tous les symboles, entrants compris.
    iaex.encode(&mut mq, Some(0));
    iaex.encode(&mut mq, Some((input + symbols.len()) as i32));
    data.extend_from_slice(&mq.finish());
    data
}

/// Instance d'une région de texte : coin supérieur gauche, identifiant et
/// bitmap raffiné éventuel (dimensions identiques au symbole).
#[derive(Clone)]
pub struct Instance {
    pub x: i32,
    pub y: i32,
    pub id: usize,
    pub refined: Option<Image>,
}

/// Paramètres de mise en page d'une région de texte.
#[derive(Clone, Copy)]
pub struct Layout {
    pub log_strips: u32,
    /// 0 BOTTOMLEFT, 1 TOPLEFT, 2 BOTTOMRIGHT, 3 TOPRIGHT.
    pub ref_corner: u8,
    pub transposed: bool,
    pub ds_offset: i32,
    pub rtemplate: u8,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            log_strips: 0,
            ref_corner: 1,
            transposed: false,
            ds_offset: 0,
            rtemplate: 0,
        }
    }
}

/// Regroupe les instances en bandes : rend, par bande, (T de la bande,
/// instances triées par S) avec S/T selon TRANSPOSED.
fn strips(instances: &[Instance], symbols: &[Image], layout: Layout) -> Vec<(i32, Vec<Instance>)> {
    let strips = 1i32 << layout.log_strips;
    let mut items: Vec<(i32, i32, Instance)> = instances
        .iter()
        .map(|inst| {
            let (w, h) = (
                symbols[inst.id].width as i32,
                symbols[inst.id].height as i32,
            );
            let (s, t) = if layout.transposed {
                let t = match layout.ref_corner {
                    0 | 1 => inst.x,
                    _ => inst.x + w - 1,
                };
                (inst.y, t)
            } else {
                let t = match layout.ref_corner {
                    1 | 3 => inst.y,
                    _ => inst.y + h - 1,
                };
                (inst.x, t)
            };
            (t.div_euclid(strips) * strips, s, inst.clone())
        })
        .collect();
    items.sort_by_key(|(strip, s, _)| (*strip, *s));
    let mut out: Vec<(i32, Vec<Instance>)> = Vec::new();
    for (strip, _, inst) in items {
        match out.last_mut() {
            Some((t, list)) if *t == strip => list.push(inst),
            _ => out.push((strip, vec![inst])),
        }
    }
    out
}

fn instance_t(inst: &Instance, symbols: &[Image], layout: Layout) -> i32 {
    let (w, h) = (
        symbols[inst.id].width as i32,
        symbols[inst.id].height as i32,
    );
    if layout.transposed {
        match layout.ref_corner {
            0 | 1 => inst.x,
            _ => inst.x + w - 1,
        }
    } else {
        match layout.ref_corner {
            1 | 3 => inst.y,
            _ => inst.y + h - 1,
        }
    }
}

/// Données d'un segment de région de texte arithmétique (§7.4.4), après
/// les informations de région.
pub fn text_region_data(
    instances: &[Instance],
    symbols: &[Image],
    layout: Layout,
    refine: bool,
) -> Vec<u8> {
    let mut data = Vec::new();
    let flags: u16 = u16::from(refine) << 1
        | (layout.log_strips as u16) << 2
        | u16::from(layout.ref_corner) << 4
        | u16::from(layout.transposed) << 6
        | ((layout.ds_offset & 0x1F) as u16) << 10
        | u16::from(layout.rtemplate) << 15;
    data.extend_from_slice(&flags.to_be_bytes());
    if refine && layout.rtemplate == 0 {
        data.extend_from_slice(&[0xFFu8, 0xFF, 0xFF, 0xFF]); // A1 = A2 = (−1, −1)
    }
    data.extend_from_slice(&be32(instances.len() as u32));
    let code_len = symbol_code_len(symbols.len());
    let mut mq = MqEncoder::new();
    let mut iadt = IntEncoder::new();
    let mut iafs = IntEncoder::new();
    let mut iads = IntEncoder::new();
    let mut iait = IntEncoder::new();
    let mut iari = IntEncoder::new();
    let mut iardw = IntEncoder::new();
    let mut iardh = IntEncoder::new();
    let mut iardx = IntEncoder::new();
    let mut iardy = IntEncoder::new();
    let mut iaid = IaidEncoder::new(code_len);
    let mut refiner = RefinementEncoder::new();
    let strips_n = 1i32 << layout.log_strips;
    iadt.encode(&mut mq, Some(0));
    let mut stript = 0i32;
    let mut firsts = 0i32;
    for (strip_t, list) in strips(instances, symbols, layout) {
        iadt.encode(&mut mq, Some((strip_t - stript) / strips_n));
        stript = strip_t;
        let mut curs = 0i32;
        for (k, inst) in list.iter().enumerate() {
            let sym = &symbols[inst.id];
            let s = if layout.transposed { inst.y } else { inst.x };
            if k == 0 {
                iafs.encode(&mut mq, Some(s - firsts));
                firsts = s;
            } else {
                iads.encode(&mut mq, Some(s - curs - layout.ds_offset));
            }
            curs = s;
            if strips_n > 1 {
                iait.encode(&mut mq, Some(instance_t(inst, symbols, layout) - stript));
            }
            iaid.encode(&mut mq, inst.id as u32);
            if refine {
                match &inst.refined {
                    None => iari.encode(&mut mq, Some(0)),
                    Some(r) => {
                        iari.encode(&mut mq, Some(1));
                        let rdw = r.width as i32 - sym.width as i32;
                        let rdh = r.height as i32 - sym.height as i32;
                        iardw.encode(&mut mq, Some(rdw));
                        iardh.encode(&mut mq, Some(rdh));
                        iardx.encode(&mut mq, Some(0));
                        iardy.encode(&mut mq, Some(0));
                        let dx = rdw.div_euclid(2);
                        let dy = rdh.div_euclid(2);
                        refiner.encode(
                            &mut mq,
                            r,
                            sym,
                            dx,
                            dy,
                            layout.rtemplate,
                            [(-1, -1), (-1, -1)],
                            false,
                        );
                    }
                }
            }
            let advance = match (&inst.refined, layout.transposed) {
                (Some(r), true) => r.height,
                (Some(r), false) => r.width,
                (None, true) => sym.height,
                (None, false) => sym.width,
            } as i32;
            curs += advance - 1;
        }
        iads.encode(&mut mq, None);
    }
    data.extend_from_slice(&mq.finish());
    data
}

// ---------------------------------------------------------------------------
// Codage de Huffman
// ---------------------------------------------------------------------------

/// Encodeur d'après une table : écrit le préfixe puis les bits de plage.
pub struct HuffEncoder {
    lines: Vec<Line>,
    table: Table,
}

impl HuffEncoder {
    pub fn standard(n: u32) -> Self {
        let lines = huffman::standard_lines(n).unwrap().to_vec();
        HuffEncoder {
            table: Table::new(&lines).unwrap(),
            lines,
        }
    }

    pub fn encode(&self, w: &mut BitWriter, value: Option<i32>) {
        for (i, l) in self.lines.iter().enumerate() {
            if l.prefix_len == 0 {
                continue;
            }
            let ok = match (l.kind, value) {
                (LineKind::Oob, None) => true,
                (LineKind::Lower, Some(v)) => v <= l.low,
                (LineKind::Normal, Some(v)) => {
                    let span = if l.range_len == 32 {
                        i64::from(i32::MAX)
                    } else {
                        1i64 << l.range_len
                    };
                    i64::from(v) >= i64::from(l.low) && i64::from(v) < i64::from(l.low) + span
                }
                _ => false,
            };
            if ok {
                w.put(self.table.code_of(i), u32::from(l.prefix_len));
                match l.kind {
                    LineKind::Oob => {}
                    LineKind::Lower => w.put((l.low - value.unwrap()) as u32, 32),
                    LineKind::Normal => {
                        w.put((value.unwrap() - l.low) as u32, u32::from(l.range_len));
                    }
                }
                return;
            }
        }
        panic!("valeur {value:?} hors table");
    }
}

/// Segment de table personnalisée (§7.4.13, B.2) équivalent aux lignes
/// données (plages normales croissantes et contiguës de `low` à `high`).
pub fn custom_table_segment(
    number: u32,
    low: i32,
    high: i32,
    lines: &[(u8, u8)],
    lower_len: u8,
    upper_len: u8,
    oob_len: Option<u8>,
) -> Vec<u8> {
    let prefix_size = 4u8;
    let range_size = 6u8;
    let mut data =
        vec![u8::from(oob_len.is_some()) | ((prefix_size - 1) << 1) | ((range_size - 1) << 4)];
    data.extend_from_slice(&be32(low as u32));
    data.extend_from_slice(&be32(high as u32));
    let mut w = BitWriter::default();
    for &(p, r) in lines {
        w.put(u32::from(p), u32::from(prefix_size));
        w.put(u32::from(r), u32::from(range_size));
    }
    w.put(u32::from(lower_len), u32::from(prefix_size));
    w.put(u32::from(upper_len), u32::from(prefix_size));
    if let Some(o) = oob_len {
        w.put(u32::from(o), u32::from(prefix_size));
    }
    data.extend_from_slice(&w.finish());
    segment(number, 53, &[], &data)
}

/// Dictionnaire de symboles Huffman (SDHUFF = 1, tables B.4, B.2, B.1),
/// bitmaps collectifs non compressés (`mmr` faux) ou MMR.
pub fn symbol_dict_huffman_data(symbols: &[Image], mmr: bool, custom_dw: bool) -> Vec<u8> {
    let mut data = Vec::new();
    let flags: u16 = 1 | if custom_dw { 3 << 4 } else { 0 };
    data.extend_from_slice(&flags.to_be_bytes());
    data.extend_from_slice(&be32(symbols.len() as u32));
    data.extend_from_slice(&be32(symbols.len() as u32));
    let dh = HuffEncoder::standard(4);
    // La table personnalisée de test reprend exactement les lignes de B.2.
    let dw = HuffEncoder::standard(2);
    let bm = HuffEncoder::standard(1);
    let ex = HuffEncoder::standard(1);
    let mut w = BitWriter::default();
    let mut prev_h = 0i32;
    let mut i = 0;
    while i < symbols.len() {
        let h = symbols[i].height;
        dh.encode(&mut w, Some(h as i32 - prev_h));
        prev_h = h as i32;
        let mut prev_w = 0i32;
        let first = i;
        while i < symbols.len() && symbols[i].height == h {
            let wd = symbols[i].width as i32;
            dw.encode(&mut w, Some(wd - prev_w));
            prev_w = wd;
            i += 1;
        }
        dw.encode(&mut w, None);
        // Bitmap collectif de la classe.
        let totwidth: usize = symbols[first..i].iter().map(|s| s.width).sum();
        let mut collective = Image::new(totwidth, h);
        let mut x0 = 0;
        for s in &symbols[first..i] {
            for y in 0..h {
                for x in 0..s.width {
                    collective.set(x0 + x, y, s.row(y)[x]);
                }
            }
            x0 += s.width;
        }
        if mmr {
            let coded = encode_g4(&collective, true);
            bm.encode(&mut w, Some(coded.len() as i32));
            w.align();
            for b in coded {
                w.put(u32::from(b), 8);
            }
        } else {
            bm.encode(&mut w, Some(0));
            w.align();
            for b in collective.packed() {
                w.put(u32::from(b), 8);
            }
        }
        w.align();
    }
    ex.encode(&mut w, Some(0));
    ex.encode(&mut w, Some(symbols.len() as i32));
    data.extend_from_slice(&w.finish());
    data
}

/// Région de texte Huffman (SBHUFF = 1, tables B.6, B.8, B.11, B.14, B.1),
/// sans raffinement, coin TOPLEFT.
pub fn text_region_huffman_data(
    instances: &[Instance],
    symbols: &[Image],
    log_strips: u32,
) -> Vec<u8> {
    let layout = Layout {
        log_strips,
        ..Layout::default()
    };
    let mut data = Vec::new();
    let flags: u16 = 1 | (log_strips as u16) << 2 | 1 << 4;
    data.extend_from_slice(&flags.to_be_bytes());
    let huff_flags: u16 = 0; // FS B.6, DS B.8, DT B.11, RD* B.14, RSIZE B.1
    data.extend_from_slice(&huff_flags.to_be_bytes());
    data.extend_from_slice(&be32(instances.len() as u32));
    let mut w = BitWriter::default();
    // Codes des symboles (§7.4.3.1.7) : tous de longueur `code_len`, un
    // seul code de plage utilisé (longueur 1, code « 0 »).
    let code_len = symbol_code_len(symbols.len());
    for i in 0..35u32 {
        w.put(u32::from(i == code_len), 4);
    }
    for _ in symbols {
        w.put(0, 1);
    }
    w.align();
    let fs = HuffEncoder::standard(6);
    let ds = HuffEncoder::standard(8);
    let dt = HuffEncoder::standard(11);
    let strips_n = 1i32 << log_strips;
    dt.encode(&mut w, Some(0));
    let mut stript = 0i32;
    let mut firsts = 0i32;
    for (strip_t, list) in strips(instances, symbols, layout) {
        dt.encode(&mut w, Some((strip_t - stript) / strips_n));
        stript = strip_t;
        let mut curs = 0i32;
        for (k, inst) in list.iter().enumerate() {
            let sym = &symbols[inst.id];
            if k == 0 {
                fs.encode(&mut w, Some(inst.x - firsts));
                firsts = inst.x;
            } else {
                ds.encode(&mut w, Some(inst.x - curs));
            }
            curs = inst.x;
            if strips_n > 1 {
                w.put((inst.y - stript) as u32, log_strips);
            }
            w.put(inst.id as u32, code_len);
            curs += sym.width as i32 - 1;
        }
        ds.encode(&mut w, None);
    }
    data.extend_from_slice(&w.finish());
    data
}

// ---------------------------------------------------------------------------
// Motifs et demi-teintes (§6.7, §6.6)
// ---------------------------------------------------------------------------

/// Données d'un dictionnaire de motifs (§7.4.5.1) : motifs de même taille.
pub fn pattern_dict_data(patterns: &[Image], template: u8, mmr: bool) -> Vec<u8> {
    let (pw, ph) = (patterns[0].width, patterns[0].height);
    let mut collective = Image::new(pw * patterns.len(), ph);
    for (i, p) in patterns.iter().enumerate() {
        for y in 0..ph {
            for x in 0..pw {
                collective.set(i * pw + x, y, p.row(y)[x]);
            }
        }
    }
    let mut data = vec![u8::from(mmr) | (template << 1), pw as u8, ph as u8];
    data.extend_from_slice(&be32(patterns.len() as u32 - 1));
    if mmr {
        data.extend_from_slice(&encode_g4(&collective, true));
    } else {
        let at = [(-(pw as i8), 0), (-3, -1), (2, -2), (-2, -2)];
        let mut mq = MqEncoder::new();
        let mut gen = GenericEncoder::new();
        gen.encode(&mut mq, &collective, template, at, false);
        data.extend_from_slice(&mq.finish());
    }
    data
}

/// Paramètres d'une région de demi-teintes de test.
#[derive(Clone, Copy)]
pub struct HalftoneParams {
    /// Grille `gw × gh`, cellules de `(hrx, hry)` en 1/256 de pixel à
    /// partir de `(hgx, hgy)`.
    pub gw: usize,
    pub gh: usize,
    pub hgx: i32,
    pub hgy: i32,
    pub hrx: u16,
    pub hry: u16,
    /// Taille de la région et des motifs (pour HSKIP).
    pub region: (usize, usize),
    pub pattern: (usize, usize),
    pub num_patterns: usize,
    pub template: u8,
    pub mmr: bool,
    pub enable_skip: bool,
}

/// Données d'une région de demi-teintes (§7.4.5.2) après les informations
/// de région.
pub fn halftone_region_data(values: &[u32], p: HalftoneParams) -> Vec<u8> {
    let HalftoneParams {
        gw,
        gh,
        hgx,
        hgy,
        hrx,
        hry,
        region: (region_w, region_h),
        pattern: (pattern_w, pattern_h),
        num_patterns,
        template,
        mmr,
        enable_skip,
    } = p;
    let mut data = vec![u8::from(mmr) | (template << 1) | (u8::from(enable_skip) << 3)];
    data.extend_from_slice(&be32(gw as u32));
    data.extend_from_slice(&be32(gh as u32));
    data.extend_from_slice(&be32(hgx as u32));
    data.extend_from_slice(&be32(hgy as u32));
    data.extend_from_slice(&hrx.to_be_bytes());
    data.extend_from_slice(&hry.to_be_bytes());
    let bits = symbol_code_len(num_patterns);
    // HSKIP (§6.6.5.1) : cellules dont le motif tombe hors de la région.
    let skip = enable_skip.then(|| {
        let mut skip = Image::new(gw, gh);
        for m in 0..gh {
            for n in 0..gw {
                let x = (hgx + m as i32 * i32::from(hry) + n as i32 * i32::from(hrx)) >> 8;
                let y = (hgy + m as i32 * i32::from(hrx) - n as i32 * i32::from(hry)) >> 8;
                let out = x + pattern_w as i32 <= 0
                    || x >= region_w as i32
                    || y + pattern_h as i32 <= 0
                    || y >= region_h as i32;
                skip.set(n, m, u8::from(out));
            }
        }
        skip
    });
    // Plans en code de Gray (C.5), du poids fort au poids faible.
    let mut planes: Vec<Image> = Vec::new();
    for j in (0..bits).rev() {
        let mut plane = Image::new(gw, gh);
        for (i, &v) in values.iter().enumerate() {
            let bit = (v >> j) & 1;
            let next = if j + 1 < bits { (v >> (j + 1)) & 1 } else { 0 };
            plane.pixels[i] = (bit ^ next) as u8;
            if skip.as_ref().is_some_and(|s| s.pixels[i] != 0) {
                plane.pixels[i] = 0;
            }
        }
        planes.push(plane);
    }
    if mmr {
        // Un seul flux MMR pour tous les plans, EOFB à la fin.
        let mut w = BitWriter::default();
        let blank = vec![0u8; gw];
        let mut prev: Option<Vec<u8>> = None;
        for plane in &planes {
            for y in 0..gh {
                let reference = prev.as_deref().unwrap_or(&blank);
                crate::ccitt::tests::encoder::put_row_2d(&mut w, plane.row(y), reference);
                prev = Some(plane.row(y).to_vec());
            }
        }
        crate::ccitt::tests::encoder::put_eol(&mut w);
        crate::ccitt::tests::encoder::put_eol(&mut w);
        data.extend_from_slice(&w.finish());
    } else {
        let at = [
            (if template <= 1 { 3 } else { 2 }, -1),
            (-3, -1),
            (2, -2),
            (-2, -2),
        ];
        let mut mq = MqEncoder::new();
        let mut gen = GenericEncoder::new();
        for plane in &planes {
            gen.encode_with_skip(&mut mq, plane, template, at, false, skip.as_ref());
        }
        data.extend_from_slice(&mq.finish());
    }
    data
}
