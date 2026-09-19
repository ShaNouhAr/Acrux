//! Encodeur JPEG de base (ITU-T T.81, processus séquentiel DCT à codage de
//! Huffman), écrit de zéro comme le décodeur voisin et comme le compresseur
//! DEFLATE de `flate::compress`.
//!
//! Chaîne d'encodage (T.81 §4.1 et annexe A) :
//! 1. conversion RVB → YCbCr (JFIF, même matrice que la conversion inverse du
//!    décodeur) ;
//! 2. sous-échantillonnage 4:2:0 optionnel des chromas (moyenne de 2 × 2) ;
//! 3. découpe en blocs 8 × 8 avec réplication des bords, décalage de niveau
//!    (−128, §A.3.1) ;
//! 4. DCT directe 8 × 8 séparable (§A.3.3) ;
//! 5. quantification par les tables d'exemple de l'annexe K (K.1 luminance,
//!    K.2 chrominance) mises à l'échelle par la qualité ;
//! 6. codage de Huffman des différences DC et des couples (zéros, amplitude)
//!    AC avec les tables d'exemple K.3 à K.6 (§F.1.2) ;
//! 7. écriture du flux : SOI, APP0 (JFIF), DQT, SOF0, DHT, SOS, données
//!    entropiques avec bourrage des 0xFF (§B.1.1.5), EOI.
//!
//! Le résultat se relit avec [`super::decode`] et avec tout décodeur JPEG.

// Arithmétique d'échantillons et d'indices : toutes les valeurs sont bornées
// par la précision 8 bits, la taille de bloc (64) et les dimensions vérifiées
// en tête d'`encode`.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless
)]

use super::{marker, ZIGZAG};

/// Table de quantification de luminance de l'annexe K.1, en ordre naturel.
const QUANT_LUMA: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, //
    12, 12, 14, 19, 26, 58, 60, 55, //
    14, 13, 16, 24, 40, 57, 69, 56, //
    14, 17, 22, 29, 51, 87, 80, 62, //
    18, 22, 37, 56, 68, 109, 103, 77, //
    24, 35, 55, 64, 81, 104, 113, 92, //
    49, 64, 78, 87, 103, 121, 120, 101, //
    72, 92, 95, 98, 112, 100, 103, 99,
];

/// Table de quantification de chrominance de l'annexe K.2, en ordre naturel.
const QUANT_CHROMA: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, //
    18, 21, 26, 66, 99, 99, 99, 99, //
    24, 26, 56, 99, 99, 99, 99, 99, //
    47, 66, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99,
];

/// Longueurs de codes de la table DC de luminance (annexe K.3, tableau K.3).
const BITS_DC_LUMA: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
/// Longueurs de codes de la table DC de chrominance (tableau K.4).
const BITS_DC_CHROMA: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
/// Valeurs des deux tables DC : les douze catégories d'amplitude.
const VALS_DC: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

/// Longueurs de codes de la table AC de luminance (tableau K.5).
const BITS_AC_LUMA: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7D];
/// Valeurs de la table AC de luminance (tableau K.5), couples (zéros, taille).
const VALS_AC_LUMA: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7,
    0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5,
    0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2,
    0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

/// Longueurs de codes de la table AC de chrominance (tableau K.6).
const BITS_AC_CHROMA: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
/// Valeurs de la table AC de chrominance (tableau K.6).
const VALS_AC_CHROMA: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33, 0x52, 0xF0,
    0x15, 0x62, 0x72, 0xD1, 0x0A, 0x16, 0x24, 0x34, 0xE1, 0x25, 0xF1, 0x17, 0x18, 0x19, 0x1A, 0x26,
    0x27, 0x28, 0x29, 0x2A, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5,
    0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3,
    0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA,
    0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

/// Table de Huffman prête à l'encodage : pour chaque valeur, le code et sa longueur.
struct Encoder {
    /// Longueurs de codes par nombre de bits (BITS, §B.2.4.2).
    bits: [u8; 16],
    /// Valeurs associées, dans l'ordre des codes (HUFFVAL).
    values: Vec<u8>,
    /// Code de chaque valeur (indexé par la valeur elle-même).
    codes: [u16; 256],
    /// Longueur du code de chaque valeur, 0 si la valeur est absente.
    lengths: [u8; 256],
}

impl Encoder {
    /// Construit les codes canoniques depuis BITS et HUFFVAL (procédures
    /// `Generate_size_table` et `Generate_code_table`, figures C.1 et C.2).
    fn new(bits: &[u8; 16], values: &[u8]) -> Self {
        let mut codes = [0u16; 256];
        let mut lengths = [0u8; 256];
        let mut code: u16 = 0;
        let mut k = 0usize;
        for (i, &count) in bits.iter().enumerate() {
            let length = (i + 1) as u8;
            for _ in 0..count {
                if let Some(&v) = values.get(k) {
                    codes[v as usize] = code;
                    lengths[v as usize] = length;
                }
                code = code.wrapping_add(1);
                k += 1;
            }
            code <<= 1;
        }
        Encoder {
            bits: *bits,
            values: values.to_vec(),
            codes,
            lengths,
        }
    }
}

/// Écrivain de bits avec bourrage des octets 0xFF (§B.1.1.5 : tout 0xFF des
/// données entropiques est suivi d'un 0x00).
struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    count: u32,
}

impl BitWriter {
    fn new(capacity: usize) -> Self {
        BitWriter {
            out: Vec::with_capacity(capacity),
            acc: 0,
            count: 0,
        }
    }

    /// Écrit les `length` bits de poids faible de `value`, du plus significatif
    /// au moins significatif.
    fn push(&mut self, value: u16, length: u8) {
        if length == 0 {
            return;
        }
        let length = u32::from(length.min(16));
        let masked = u32::from(value) & ((1u32 << length) - 1);
        self.acc = (self.acc << length) | masked;
        self.count += length;
        while self.count >= 8 {
            self.count -= 8;
            let byte = ((self.acc >> self.count) & 0xFF) as u8;
            self.out.push(byte);
            if byte == 0xFF {
                self.out.push(0x00);
            }
        }
    }

    /// Complète l'octet en cours avec des 1 (§F.1.2.1) et rend le flux.
    fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            let pad = 8 - self.count;
            self.push(u16::MAX >> (16 - pad), pad as u8);
        }
        self.out
    }
}

/// Table de cosinus de la DCT 8 points : `cos[u][x] = cos((2x+1)uπ/16)`.
struct Cosines([[f64; 8]; 8]);

impl Cosines {
    fn new() -> Self {
        let mut t = [[0.0f64; 8]; 8];
        for (u, row) in t.iter_mut().enumerate() {
            for (x, cell) in row.iter_mut().enumerate() {
                *cell = (((2 * x + 1) * u) as f64 * std::f64::consts::PI / 16.0).cos();
            }
        }
        Cosines(t)
    }

    /// DCT directe 8 × 8 séparable (§A.3.3, équation A.3) appliquée en place.
    fn forward(&self, block: &mut [f64; 64]) {
        const INV_SQRT2: f64 = std::f64::consts::FRAC_1_SQRT_2;
        let mut tmp = [0.0f64; 64];
        // Lignes.
        for y in 0..8 {
            for u in 0..8 {
                let mut sum = 0.0;
                for x in 0..8 {
                    sum += block[y * 8 + x] * self.0[u][x];
                }
                let cu = if u == 0 { INV_SQRT2 } else { 1.0 };
                tmp[y * 8 + u] = 0.5 * cu * sum;
            }
        }
        // Colonnes.
        for x in 0..8 {
            for v in 0..8 {
                let mut sum = 0.0;
                for y in 0..8 {
                    sum += tmp[y * 8 + x] * self.0[v][y];
                }
                let cv = if v == 0 { INV_SQRT2 } else { 1.0 };
                block[v * 8 + x] = 0.5 * cv * sum;
            }
        }
    }
}

/// Met une table de quantification à l'échelle de la qualité, selon la
/// convention usuelle (facteur 5000/Q en dessous de 50, 200 − 2Q au-dessus),
/// chaque valeur restant dans 1..=255 (précision 8 bits, §B.2.4.1).
fn scale_quant(base: &[u16; 64], quality: u8) -> [u16; 64] {
    let q = u32::from(quality.clamp(1, 100));
    let scale = if q < 50 { 5000 / q } else { 200 - 2 * q };
    let mut out = [0u16; 64];
    for (o, &b) in out.iter_mut().zip(base.iter()) {
        let v = (u32::from(b) * scale + 50) / 100;
        *o = v.clamp(1, 255) as u16;
    }
    out
}

/// Catégorie d'amplitude d'une valeur (SSSS, tableau F.1) : nombre de bits
/// nécessaires à sa magnitude.
fn magnitude_category(value: i32) -> u8 {
    let mut m = value.unsigned_abs();
    let mut s = 0u8;
    while m > 0 {
        m >>= 1;
        s += 1;
    }
    s
}

/// Bits d'amplitude d'une valeur pour sa catégorie (§F.1.2.1.1).
fn magnitude_bits(value: i32, size: u8) -> u16 {
    let v = if value >= 0 {
        value
    } else {
        value + (1 << size) - 1
    };
    (v & 0xFFFF) as u16
}

/// Plan d'échantillons d'une composante.
struct Plane {
    width: usize,
    height: usize,
    data: Vec<u8>,
}

impl Plane {
    /// Échantillon avec réplication des bords (blocs débordant de l'image).
    fn at(&self, x: usize, y: usize) -> u8 {
        if self.width == 0 || self.height == 0 {
            return 0;
        }
        let x = x.min(self.width - 1);
        let y = y.min(self.height - 1);
        self.data.get(y * self.width + x).copied().unwrap_or(0)
    }
}

/// Convertit l'image RVB en trois plans YCbCr (JFIF §7), les chromas
/// éventuellement sous-échantillonnés 2 × 2 par moyenne.
// `r`, `g`, `b`, `y`, `cb`, `cr` : noms de la formule JFIF, plus clairs que
// des paraphrases.
#[allow(clippy::similar_names, clippy::many_single_char_names)]
fn to_planes(rgb: &[u8], width: usize, height: usize, subsample: bool) -> [Plane; 3] {
    let n = width * height;
    let mut y = vec![0u8; n];
    let mut cb_full = vec![0f32; n];
    let mut cr_full = vec![0f32; n];
    for i in 0..n {
        let r = f32::from(rgb.get(i * 3).copied().unwrap_or(0));
        let g = f32::from(rgb.get(i * 3 + 1).copied().unwrap_or(0));
        let b = f32::from(rgb.get(i * 3 + 2).copied().unwrap_or(0));
        y[i] = (0.299 * r + 0.587 * g + 0.114 * b)
            .round()
            .clamp(0.0, 255.0) as u8;
        cb_full[i] = 128.0 - 0.168_736 * r - 0.331_264 * g + 0.5 * b;
        cr_full[i] = 128.0 + 0.5 * r - 0.418_688 * g - 0.081_312 * b;
    }
    let luma = Plane {
        width,
        height,
        data: y,
    };
    if !subsample {
        let pack = |src: &[f32]| Plane {
            width,
            height,
            data: src
                .iter()
                .map(|v| v.round().clamp(0.0, 255.0) as u8)
                .collect(),
        };
        return [luma, pack(&cb_full), pack(&cr_full)];
    }
    let cw = width.div_ceil(2).max(1);
    let ch = height.div_ceil(2).max(1);
    let down = |src: &[f32]| {
        let mut out = vec![0u8; cw * ch];
        for by in 0..ch {
            for bx in 0..cw {
                let mut sum = 0.0f32;
                let mut count = 0f32;
                for dy in 0..2 {
                    for dx in 0..2 {
                        let (x, py) = (bx * 2 + dx, by * 2 + dy);
                        if x < width && py < height {
                            sum += src[py * width + x];
                            count += 1.0;
                        }
                    }
                }
                let v = if count > 0.0 { sum / count } else { 128.0 };
                out[by * cw + bx] = v.round().clamp(0.0, 255.0) as u8;
            }
        }
        Plane {
            width: cw,
            height: ch,
            data: out,
        }
    };
    let cb = down(&cb_full);
    let cr = down(&cr_full);
    [luma, cb, cr]
}

/// Ajoute un segment `FF <code> <longueur> <charge>`.
fn push_segment(out: &mut Vec<u8>, code: u8, payload: &[u8]) {
    out.push(0xFF);
    out.push(code);
    let len = (payload.len() + 2).min(0xFFFF) as u16;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
}

/// Écrit les deux tables de quantification (DQT, §B.2.4.1), en ordre zigzag.
fn push_dqt(out: &mut Vec<u8>, luma: &[u16; 64], chroma: &[u16; 64]) {
    let mut payload = Vec::with_capacity(2 * 65);
    for (id, table) in [(0u8, luma), (1u8, chroma)] {
        payload.push(id); // précision 8 bits (Pq = 0), identifiant Tq
        for &z in &ZIGZAG {
            payload.push(table[z as usize] as u8);
        }
    }
    push_segment(out, marker::DQT, &payload);
}

/// Écrit une table de Huffman (DHT, §B.2.4.2). `class` : 0 = DC, 1 = AC.
fn push_dht(out: &mut Vec<u8>, class: u8, id: u8, table: &Encoder) {
    let mut payload = Vec::with_capacity(17 + table.values.len());
    payload.push((class << 4) | id);
    payload.extend_from_slice(&table.bits);
    payload.extend_from_slice(&table.values);
    push_segment(out, marker::DHT, &payload);
}

/// Encode une image RVB 8 bits (3 octets par pixel, ligne par ligne depuis le
/// haut) en JPEG de base.
///
/// - `quality` : 1 (très compressé) à 100 (quasi sans perte), borné à cet
///   intervalle ; 75 est la valeur usuelle, 90 conserve les dégradés fins.
/// - `subsample` : sous-échantillonnage 4:2:0 des chromas (fichier ~40 % plus
///   petit, chromas deux fois moins définis) ; `false` pour du 4:4:4.
///
/// Des données trop courtes sont complétées par des zéros ; une image de
/// dimension nulle donne un vecteur vide.
#[must_use]
pub fn encode(rgb: &[u8], width: u32, height: u32, quality: u8, subsample: bool) -> Vec<u8> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let (w, h) = (width as usize, height as usize);
    // Borne de sécurité identique au décodeur.
    if u64::from(width) * u64::from(height) > super::MAX_PIXELS {
        return Vec::new();
    }
    let planes = to_planes(rgb, w, h, subsample);
    let q_luma = scale_quant(&QUANT_LUMA, quality);
    let q_chroma = scale_quant(&QUANT_CHROMA, quality);
    let dc_luma = Encoder::new(&BITS_DC_LUMA, &VALS_DC);
    let dc_chroma = Encoder::new(&BITS_DC_CHROMA, &VALS_DC);
    let ac_luma = Encoder::new(&BITS_AC_LUMA, &VALS_AC_LUMA);
    let ac_chroma = Encoder::new(&BITS_AC_CHROMA, &VALS_AC_CHROMA);

    let mut out = Vec::with_capacity(w * h / 2 + 1024);
    out.extend_from_slice(&[0xFF, marker::SOI]);
    // APP0 JFIF : version 1.1, unités en points par pouce, 72 dpi, pas de vignette.
    push_segment(
        &mut out,
        0xE0,
        &[b'J', b'F', b'I', b'F', 0, 1, 1, 1, 0, 72, 0, 72, 0, 0],
    );
    push_dqt(&mut out, &q_luma, &q_chroma);
    // SOF0 : précision 8, dimensions, trois composantes (§B.2.2).
    let (hmax, vmax) = if subsample { (2u8, 2u8) } else { (1u8, 1u8) };
    let mut sof = Vec::with_capacity(15);
    sof.push(8);
    sof.extend_from_slice(&(height.min(65535) as u16).to_be_bytes());
    sof.extend_from_slice(&(width.min(65535) as u16).to_be_bytes());
    sof.push(3);
    for (id, (hi, vi), tq) in [(1u8, (hmax, vmax), 0u8), (2, (1, 1), 1), (3, (1, 1), 1)] {
        sof.push(id);
        sof.push((hi << 4) | vi);
        sof.push(tq);
    }
    push_segment(&mut out, marker::SOF0, &sof);
    push_dht(&mut out, 0, 0, &dc_luma);
    push_dht(&mut out, 1, 0, &ac_luma);
    push_dht(&mut out, 0, 1, &dc_chroma);
    push_dht(&mut out, 1, 1, &ac_chroma);
    // SOS : les trois composantes, sélecteurs de tables, Ss=0 Se=63 Ah=Al=0 (§B.2.3).
    let sos = [3u8, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0x00];
    push_segment(&mut out, marker::SOS, &sos);

    let cos = Cosines::new();
    let mut writer = BitWriter::new(w * h / 2 + 256);
    let mut pred = [0i32; 3];
    let mcu_w = 8 * hmax as usize;
    let mcu_h = 8 * vmax as usize;
    let mcus_x = w.div_ceil(mcu_w);
    let mcus_y = h.div_ceil(mcu_h);
    let mut block = [0f64; 64];
    for my in 0..mcus_y {
        for mx in 0..mcus_x {
            for (ci, plane) in planes.iter().enumerate() {
                let (hi, vi) = if ci == 0 {
                    (hmax as usize, vmax as usize)
                } else {
                    (1, 1)
                };
                let (quant, dc, ac) = if ci == 0 {
                    (&q_luma, &dc_luma, &ac_luma)
                } else {
                    (&q_chroma, &dc_chroma, &ac_chroma)
                };
                for by in 0..vi {
                    for bx in 0..hi {
                        let ox = (mx * hi + bx) * 8;
                        let oy = (my * vi + by) * 8;
                        for y in 0..8 {
                            for x in 0..8 {
                                block[y * 8 + x] = f64::from(plane.at(ox + x, oy + y)) - 128.0;
                            }
                        }
                        cos.forward(&mut block);
                        encode_block(&mut writer, &block, quant, dc, ac, &mut pred[ci]);
                    }
                }
            }
        }
    }
    out.extend_from_slice(&writer.finish());
    out.extend_from_slice(&[0xFF, marker::EOI]);
    out
}

/// Quantifie, parcourt en zigzag et code un bloc (§F.1.2).
fn encode_block(
    writer: &mut BitWriter,
    block: &[f64; 64],
    quant: &[u16; 64],
    dc_table: &Encoder,
    ac_table: &Encoder,
    pred: &mut i32,
) {
    let mut zz = [0i32; 64];
    for (k, &nat) in ZIGZAG.iter().enumerate() {
        let n = nat as usize;
        let q = f64::from(quant[n]).max(1.0);
        // Arrondi au plus proche, borné à l'intervalle 11 bits du mode de base.
        let v = (block[n] / q).round().clamp(-2048.0, 2047.0) as i32;
        zz[k] = v;
    }
    // DC : différence avec le bloc précédent de la même composante.
    let diff = zz[0] - *pred;
    *pred = zz[0];
    let size = magnitude_category(diff);
    writer.push(
        dc_table.codes[size as usize],
        dc_table.lengths[size as usize],
    );
    if size > 0 {
        writer.push(magnitude_bits(diff, size), size);
    }
    // AC : couples (zéros, taille) ; ZRL (0xF0) tous les seize zéros, EOB à la fin.
    let last = zz.iter().rposition(|&v| v != 0).unwrap_or(0);
    let mut run = 0u8;
    for &v in zz.get(1..=last).unwrap_or(&[]) {
        if v == 0 {
            run += 1;
            if run == 16 {
                writer.push(ac_table.codes[0xF0], ac_table.lengths[0xF0]);
                run = 0;
            }
            continue;
        }
        let size = magnitude_category(v);
        let symbol = ((run << 4) | size.min(15)) as usize;
        writer.push(ac_table.codes[symbol], ac_table.lengths[symbol]);
        writer.push(magnitude_bits(v, size), size);
        run = 0;
    }
    if last < 63 {
        writer.push(ac_table.codes[0], ac_table.lengths[0]); // EOB
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp
)]
mod tests {
    use super::*;

    /// Image de test : dégradés croisés et aplats, dimensions non multiples de 16.
    fn sample(width: usize, height: usize) -> Vec<u8> {
        let mut rgb = vec![0u8; width * height * 3];
        for y in 0..height {
            for x in 0..width {
                let i = (y * width + x) * 3;
                rgb[i] = ((x * 255) / width.max(1)) as u8;
                rgb[i + 1] = ((y * 255) / height.max(1)) as u8;
                rgb[i + 2] = if (x / 8 + y / 8) % 2 == 0 { 40 } else { 200 };
            }
        }
        rgb
    }

    /// Écart moyen par canal entre deux images RVB de même taille.
    fn mean_error(a: &[u8], b: &[u8]) -> f64 {
        assert_eq!(a.len(), b.len());
        let sum: f64 = a
            .iter()
            .zip(b)
            .map(|(&x, &y)| f64::from(i32::from(x) - i32::from(y)).abs())
            .sum();
        sum / a.len() as f64
    }

    #[test]
    fn roundtrip_quality_90_is_almost_lossless() {
        let (w, h) = (61usize, 37usize);
        let rgb = sample(w, h);
        let jpeg = encode(&rgb, w as u32, h as u32, 90, false);
        let img = super::super::decode(&jpeg).unwrap();
        assert_eq!((img.width, img.height), (w as u32, h as u32));
        assert_eq!(img.components, 3);
        let err = mean_error(&rgb, &img.data);
        assert!(err < 3.0, "écart moyen à qualité 90 : {err}");
    }

    #[test]
    fn roundtrip_quality_50_stays_reasonable() {
        let (w, h) = (64usize, 48usize);
        let rgb = sample(w, h);
        let jpeg = encode(&rgb, w as u32, h as u32, 50, false);
        let img = super::super::decode(&jpeg).unwrap();
        let err = mean_error(&rgb, &img.data);
        assert!(err < 8.0, "écart moyen à qualité 50 : {err}");
    }

    #[test]
    fn subsampling_is_smaller_and_still_close() {
        let (w, h) = (80usize, 80usize);
        // Dégradé lisse : le gain du 4:2:0 vient des chromas, pas du damier.
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                rgb[i] = ((x * 255) / w) as u8;
                rgb[i + 1] = ((y * 255) / h) as u8;
                rgb[i + 2] = (((x + y) * 255) / (w + h)) as u8;
            }
        }
        let full = encode(&rgb, w as u32, h as u32, 85, false);
        let half = encode(&rgb, w as u32, h as u32, 85, true);
        assert!(
            half.len() < full.len(),
            "4:2:0 doit être plus petit : {} vs {}",
            half.len(),
            full.len()
        );
        let img = super::super::decode(&half).unwrap();
        assert_eq!((img.width, img.height), (w as u32, h as u32));
        // Les chromas sont moitié moins définis : on compare le seul canal Y.
        let luma = |d: &[u8]| -> Vec<u8> {
            d.chunks_exact(3)
                .map(|p| {
                    (0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2]))
                        .round() as u8
                })
                .collect()
        };
        let err = mean_error(&luma(&rgb), &luma(&img.data));
        assert!(err < 6.0, "écart moyen de luminance en 4:2:0 : {err}");
    }

    #[test]
    fn flat_color_is_exact_and_tiny() {
        let (w, h) = (32usize, 16usize);
        let rgb: Vec<u8> = [17u8, 200, 90]
            .iter()
            .copied()
            .cycle()
            .take(w * h * 3)
            .collect();
        let jpeg = encode(&rgb, w as u32, h as u32, 95, false);
        let img = super::super::decode(&jpeg).unwrap();
        let err = mean_error(&rgb, &img.data);
        assert!(err < 2.0, "aplat : {err}");
        assert!(jpeg.len() < 1024, "aplat compact : {} octets", jpeg.len());
    }

    #[test]
    fn markers_are_well_formed() {
        let jpeg = encode(&sample(20, 20), 20, 20, 75, true);
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "SOI");
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9], "EOI");
        // En-tête relu par le décodeur : dimensions et composantes.
        let (w, h, comps) = super::super::read_header(&jpeg).unwrap();
        assert_eq!((w, h, comps), (20, 20, 3));
        // Aucun 0xFF des données entropiques n'est laissé sans bourrage : le
        // seul marqueur après SOS est EOI.
        let sos = jpeg
            .windows(2)
            .position(|w| w == [0xFF, 0xDA])
            .expect("SOS présent");
        let len = usize::from(u16::from_be_bytes([jpeg[sos + 2], jpeg[sos + 3]]));
        let mut i = sos + 2 + len;
        while i + 1 < jpeg.len() - 2 {
            if jpeg[i] == 0xFF {
                assert_eq!(jpeg[i + 1], 0x00, "0xFF non bourré à {i}");
                i += 2;
            } else {
                i += 1;
            }
        }
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        assert!(encode(&[], 0, 0, 75, false).is_empty());
        assert!(encode(&[], 10, 0, 75, true).is_empty());
        // Données trop courtes : complétées par des zéros, image décodable.
        let jpeg = encode(&[255, 0, 0], 4, 4, 75, false);
        let img = super::super::decode(&jpeg).unwrap();
        assert_eq!((img.width, img.height), (4, 4));
        // Qualités extrêmes bornées à 1..=100.
        for q in [0u8, 1, 100, 255] {
            let jpeg = encode(&sample(9, 9), 9, 9, q, q % 2 == 0);
            assert!(super::super::decode(&jpeg).is_ok(), "qualité {q}");
        }
    }

    #[test]
    fn quant_scaling_follows_quality() {
        let low = scale_quant(&QUANT_LUMA, 10);
        let high = scale_quant(&QUANT_LUMA, 95);
        assert!(low[0] > high[0], "{} vs {}", low[0], high[0]);
        assert!(low.iter().all(|&v| (1..=255).contains(&v)));
        assert!(high.iter().all(|&v| (1..=255).contains(&v)));
        // Qualité 50 : la table de l'annexe K telle quelle.
        assert_eq!(scale_quant(&QUANT_LUMA, 50), QUANT_LUMA);
        assert_eq!(scale_quant(&QUANT_CHROMA, 50), QUANT_CHROMA);
    }

    #[test]
    fn huffman_codes_are_canonical_and_prefix_free() {
        for (bits, vals) in [
            (&BITS_DC_LUMA, &VALS_DC[..]),
            (&BITS_DC_CHROMA, &VALS_DC[..]),
            (&BITS_AC_LUMA, &VALS_AC_LUMA[..]),
            (&BITS_AC_CHROMA, &VALS_AC_CHROMA[..]),
        ] {
            let table = Encoder::new(bits, vals);
            assert_eq!(
                usize::from(bits.iter().map(|&b| u16::from(b)).sum::<u16>()),
                vals.len()
            );
            let mut seen: Vec<(u16, u8)> = Vec::new();
            for &v in vals {
                let (c, l) = (table.codes[v as usize], table.lengths[v as usize]);
                assert!(l > 0 && l <= 16);
                for &(oc, ol) in &seen {
                    let shift = l.max(ol) - l.min(ol);
                    let (long, short) = if l >= ol { (c, oc) } else { (oc, c) };
                    assert_ne!(long >> shift, short, "code préfixe de {v:#x}");
                }
                seen.push((c, l));
            }
        }
    }

    #[test]
    fn magnitude_categories_match_table_f1() {
        assert_eq!(magnitude_category(0), 0);
        assert_eq!(magnitude_category(1), 1);
        assert_eq!(magnitude_category(-1), 1);
        assert_eq!(magnitude_category(2), 2);
        assert_eq!(magnitude_category(-3), 2);
        assert_eq!(magnitude_category(255), 8);
        assert_eq!(magnitude_bits(1, 1), 1);
        assert_eq!(magnitude_bits(-1, 1), 0);
        assert_eq!(magnitude_bits(-2, 2), 1);
        assert_eq!(magnitude_bits(3, 2), 3);
    }

    #[test]
    fn forward_dct_matches_the_definition() {
        let cos = Cosines::new();
        // Bloc constant : seul le coefficient DC est non nul, égal à 8 × valeur.
        let mut block = [7.0f64; 64];
        cos.forward(&mut block);
        assert!((block[0] - 56.0).abs() < 1e-9, "DC = 8 × 7 : {}", block[0]);
        assert!(block[1..].iter().all(|v| v.abs() < 1e-9));
    }
}
