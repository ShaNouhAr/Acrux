//! Tests du décodeur DCTDecode.
//!
//! - `encoder` : encodeur JPEG minimal (baseline et progressif) écrit ici,
//!   d'après T.81 annexes A, F.1, G.1 et K, pour produire des images de test
//!   dont on connaît les pixels ;
//! - `vectors` : vrais fichiers JPEG produits par GDI+ et leurs pixels de
//!   référence ;
//! - précision de l'IDCT contre une IDCT naïve O(n⁴) en `f64` ;
//! - robustesse : troncatures, mutations aléatoires, dimensions absurdes,
//!   tables invalides.

// Dans les tests, `unwrap`/`panic` et les conversions numériques directes
// sont acceptés : une panique signale un test raté, pas un fichier hostile.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::struct_excessive_bools,
    clippy::needless_range_loop
)]

use super::*;

mod vectors;

/// Encodeur JPEG minimal de test.
mod encoder {
    use super::super::ZIGZAG;

    /// Table de quantification luminance (T.81 tableau K.1), ordre naturel.
    pub const QT_LUMA: [u32; 64] = [
        16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69,
        56, 14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81,
        104, 113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
    ];
    /// Table de quantification chrominance (tableau K.2).
    pub const QT_CHROMA: [u32; 64] = [
        17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99,
        99, 47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
        99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    ];
    /// Tables de Huffman standard (K.3.3.1, K.3.3.2) : BITS puis HUFFVAL.
    pub const DC_LUMA_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
    pub const DC_CHROMA_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
    pub const DC_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    pub const AC_LUMA_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
    pub const AC_LUMA_VALS: [u8; 162] = [
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61,
        0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52,
        0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25,
        0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45,
        0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64,
        0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
        0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6,
        0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3,
        0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8,
        0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ];
    pub const AC_CHROMA_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
    pub const AC_CHROMA_VALS: [u8; 162] = [
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61,
        0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33,
        0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18,
        0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44,
        0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63,
        0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
        0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
        0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4,
        0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca,
        0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7,
        0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ];
    /// Table « générique » couvrant les 256 symboles (255 codes de 8 bits
    /// et un de 9 bits) : nécessaire aux scans progressifs, dont les symboles
    /// EOBn (0x10, 0x20…) n'existent pas dans les tables standard.
    pub const GENERIC_BITS: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 255, 1, 0, 0, 0, 0, 0, 0, 0];

    pub fn generic_vals() -> Vec<u8> {
        (0..=255u8).collect()
    }

    /// Paramètres d'encodage.
    #[derive(Clone)]
    pub struct Params {
        /// Facteurs d'échantillonnage de la première composante (les autres
        /// sont en 1 × 1) : (2, 2) donne du 4:2:0, (2, 1) du 4:2:2, (4, 1)
        /// du 4:1:1.
        pub hy: usize,
        pub vy: usize,
        /// Qualité 1 à 100 (100 = pas de quantification).
        pub quality: u32,
        pub restart_interval: usize,
        pub progressive: bool,
        /// Segment APP14 Adobe avec ce drapeau `transform`.
        pub adobe: Option<u8>,
        pub jfif: bool,
        /// Identifiants 'R', 'G', 'B' sans transformation de couleur.
        pub rgb_ids: bool,
        /// Tables DQT sur 16 bits.
        pub dqt16: bool,
        /// Précision 8 ou 12 (SOF1).
        pub precision: u8,
        /// Utiliser la table générique plutôt que les tables standard.
        pub generic_tables: bool,
        /// Écrire Y = 0 dans le SOF et un segment DNL après le premier scan.
        pub dnl: bool,
    }

    impl Default for Params {
        fn default() -> Self {
            Params {
                hy: 1,
                vy: 1,
                quality: 95,
                restart_interval: 0,
                progressive: false,
                adobe: None,
                jfif: true,
                rgb_ids: false,
                dqt16: false,
                precision: 8,
                generic_tables: false,
                dnl: false,
            }
        }
    }

    /// Codes de Huffman par symbole (C.2).
    pub struct Codes {
        code: [u16; 256],
        len: [u8; 256],
    }

    pub fn build_codes(bits: &[u8; 16], vals: &[u8]) -> Codes {
        let mut codes = Codes {
            code: [0; 256],
            len: [0; 256],
        };
        let mut code: u32 = 0;
        let mut k = 0;
        for len in 1..=16u8 {
            for _ in 0..bits[usize::from(len) - 1] {
                codes.code[usize::from(vals[k])] = code as u16;
                codes.len[usize::from(vals[k])] = len;
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        codes
    }

    /// Écriture bit à bit avec bourrage `FF 00` (F.1.2.3, B.1.1.5).
    pub struct Writer {
        pub out: Vec<u8>,
        acc: u32,
        n: u32,
    }

    impl Writer {
        pub fn new() -> Self {
            Writer {
                out: Vec::new(),
                acc: 0,
                n: 0,
            }
        }

        pub fn bits(&mut self, value: u32, count: u32) {
            for i in (0..count).rev() {
                self.acc = (self.acc << 1) | ((value >> i) & 1);
                self.n += 1;
                if self.n == 8 {
                    let byte = self.acc as u8;
                    self.out.push(byte);
                    if byte == 0xFF {
                        self.out.push(0);
                    }
                    self.acc = 0;
                    self.n = 0;
                }
            }
        }

        /// Complète l'octet en cours avec des 1 (F.1.2.3).
        pub fn flush(&mut self) {
            while self.n != 0 {
                self.bits(1, 1);
            }
        }

        pub fn marker(&mut self, m: u8) {
            self.flush();
            self.out.push(0xFF);
            self.out.push(m);
        }

        pub fn segment(&mut self, m: u8, body: &[u8]) {
            self.marker(m);
            let len = (body.len() + 2) as u16;
            self.out.extend_from_slice(&len.to_be_bytes());
            self.out.extend_from_slice(body);
        }

        pub fn symbol(&mut self, codes: &Codes, sym: u8) {
            let len = codes.len[usize::from(sym)];
            assert!(len > 0, "symbole {sym:#x} absent de la table");
            self.bits(u32::from(codes.code[usize::from(sym)]), u32::from(len));
        }
    }

    /// Catégorie SSSS d'une valeur (tableau F.1).
    pub fn category(v: i32) -> u32 {
        32 - v.unsigned_abs().leading_zeros()
    }

    /// Bits additionnels d'une valeur (F.1.2.1 / F.1.4) : complément à un
    /// pour les négatifs.
    pub fn value_bits(v: i32, s: u32) -> u32 {
        if s == 0 {
            0
        } else if v < 0 {
            ((v - 1) as u32) & ((1 << s) - 1)
        } else {
            v as u32
        }
    }

    /// Composante prête à écrire.
    pub struct Comp {
        pub id: u8,
        pub h: usize,
        pub v: usize,
        pub tq: u8,
        /// Indice de jeu de tables : 0 luma, 1 chroma.
        pub tables: usize,
        pub cw: usize,
        pub ch: usize,
        pub bw: usize,
        /// Coefficients quantifiés en ordre zigzag.
        pub blocks: Vec<[i32; 64]>,
    }

    /// Événement rencontré en parcourant un scan.
    pub enum Event {
        Block(usize, usize),
        Restart,
        End,
    }

    /// Parcourt les blocs d'un scan dans l'ordre de T.81 A.2 (entrelacé si
    /// plusieurs composantes, sinon ordre des blocs de la composante) et
    /// insère les marqueurs RSTn.
    pub fn run_scan(
        w: &mut Writer,
        scan: &[usize],
        comps: &[Comp],
        mcus: (usize, usize),
        ri: usize,
        mut on: impl FnMut(&mut Writer, Event),
    ) {
        let single = scan.len() == 1;
        let (mx, my) = if single {
            let c = &comps[scan[0]];
            (c.cw.div_ceil(8), c.ch.div_ceil(8))
        } else {
            mcus
        };
        let mut rst = 0u8;
        for mcu in 0..mx * my {
            if ri > 0 && mcu > 0 && mcu % ri == 0 {
                on(w, Event::Restart);
                w.marker(0xD0 + rst);
                rst = (rst + 1) % 8;
            }
            for &ci in scan {
                let c = &comps[ci];
                let (bh, bv) = if single { (1, 1) } else { (c.h, c.v) };
                for v in 0..bv {
                    for h in 0..bh {
                        let by = (mcu / mx) * bv + v;
                        let bx = (mcu % mx) * bh + h;
                        on(w, Event::Block(ci, by * c.bw + bx));
                    }
                }
            }
        }
        on(w, Event::End);
        w.flush();
    }

    /// Bloc séquentiel (F.1.2).
    pub fn baseline_block(w: &mut Writer, zz: &[i32; 64], pred: &mut i32, dc: &Codes, ac: &Codes) {
        let diff = zz[0] - *pred;
        *pred = zz[0];
        let s = category(diff);
        w.symbol(dc, s as u8);
        w.bits(value_bits(diff, s), s);
        let mut run = 0u32;
        for &v in &zz[1..] {
            if v == 0 {
                run += 1;
                continue;
            }
            while run > 15 {
                w.symbol(ac, 0xF0);
                run -= 16;
            }
            let s = category(v);
            w.symbol(ac, ((run << 4) | s) as u8);
            w.bits(value_bits(v, s), s);
            run = 0;
        }
        if run > 0 {
            w.symbol(ac, 0x00);
        }
    }

    /// État des scans AC progressifs : série EOB en attente et bits de
    /// correction à émettre après le code EOBn (G.1.2.2, G.1.2.3).
    #[derive(Default)]
    pub struct AcState {
        eobrun: u32,
        corr: Vec<bool>,
    }

    impl AcState {
        pub fn flush(&mut self, w: &mut Writer, ac: &Codes) {
            if self.eobrun == 0 {
                return;
            }
            let r = self.eobrun.ilog2();
            w.symbol(ac, (r << 4) as u8);
            w.bits(self.eobrun - (1 << r), r);
            for &b in &self.corr {
                w.bits(u32::from(b), 1);
            }
            self.corr.clear();
            self.eobrun = 0;
        }

        fn count_eob(&mut self, w: &mut Writer, ac: &Codes) {
            self.eobrun += 1;
            if self.eobrun == 0x7FFF {
                self.flush(w, ac);
            }
        }
    }

    /// Premier scan AC d'une bande (G.1.2.2).
    pub fn ac_first_block(
        w: &mut Writer,
        zz: &[i32; 64],
        band: (usize, usize),
        al: u32,
        st: &mut AcState,
        ac: &Codes,
    ) {
        let (ss, se) = band;
        let mag = |k: usize| zz[k].unsigned_abs() >> al;
        let Some(last) = (ss..=se).rev().find(|&k| mag(k) != 0) else {
            st.count_eob(w, ac);
            return;
        };
        st.flush(w, ac);
        let mut run = 0u32;
        for k in ss..=last {
            if mag(k) == 0 {
                run += 1;
                continue;
            }
            while run > 15 {
                w.symbol(ac, 0xF0);
                run -= 16;
            }
            let signed = if zz[k] < 0 {
                -(mag(k) as i32)
            } else {
                mag(k) as i32
            };
            let s = category(signed);
            w.symbol(ac, ((run << 4) | s) as u8);
            w.bits(value_bits(signed, s), s);
            run = 0;
        }
        if last < se {
            st.count_eob(w, ac);
        }
    }

    /// Scan d'affinement AC (G.1.2.3) : les coefficients déjà non nuls
    /// reçoivent un bit de correction, les nouveaux un code (RUN, 1) et
    /// leur signe.
    pub fn ac_refine_block(
        w: &mut Writer,
        zz: &[i32; 64],
        band: (usize, usize),
        al: u32,
        st: &mut AcState,
        ac: &Codes,
    ) {
        let (ss, se) = band;
        let mag = |k: usize| zz[k].unsigned_abs() >> al;
        let history = |k: usize| zz[k].unsigned_abs() >> (al + 1);
        let last_new = (ss..=se).rev().find(|&k| mag(k) == 1);
        let trailing_from = |from: usize| -> Vec<bool> {
            (from..=se)
                .filter(|&k| history(k) != 0)
                .map(|k| mag(k) & 1 == 1)
                .collect()
        };
        let Some(last_new) = last_new else {
            let bits = trailing_from(ss);
            st.corr.extend(bits);
            st.count_eob(w, ac);
            return;
        };
        st.flush(w, ac);
        let mut run = 0u32;
        let mut pending: Vec<bool> = Vec::new();
        for k in ss..=last_new {
            if history(k) != 0 {
                pending.push(mag(k) & 1 == 1);
                continue;
            }
            if mag(k) == 0 {
                run += 1;
                if run == 16 {
                    w.symbol(ac, 0xF0);
                    for &b in &pending {
                        w.bits(u32::from(b), 1);
                    }
                    pending.clear();
                    run = 0;
                }
                continue;
            }
            w.symbol(ac, ((run << 4) | 1) as u8);
            w.bits(u32::from(zz[k] > 0), 1);
            for &b in &pending {
                w.bits(u32::from(b), 1);
            }
            pending.clear();
            run = 0;
        }
        if last_new < se {
            let bits = trailing_from(last_new + 1);
            st.corr.extend(bits);
            st.count_eob(w, ac);
        }
    }

    /// Conversion RGB → YCbCr (JFIF 1.02 §7).
    fn rgb_to_ycc(r: f64, g: f64, b: f64) -> [f64; 3] {
        [
            0.299 * r + 0.587 * g + 0.114 * b,
            -0.168_736 * r - 0.331_264 * g + 0.5 * b + 128.0,
            0.5 * r - 0.418_688 * g - 0.081_312 * b + 128.0,
        ]
    }

    /// FDCT 8 × 8 (A.3.3) puis quantification (A.3.4).
    fn fdct_quantize(f: &[f64; 64], qt: &[u32; 64]) -> [i32; 64] {
        let mut cos = [[0f64; 8]; 8];
        for (x, row) in cos.iter_mut().enumerate() {
            for (u, c) in row.iter_mut().enumerate() {
                *c = ((2.0 * x as f64 + 1.0) * u as f64 * std::f64::consts::PI / 16.0).cos();
            }
        }
        let cu = |u: usize| {
            if u == 0 {
                std::f64::consts::FRAC_1_SQRT_2
            } else {
                1.0
            }
        };
        // t[u][y] = Σx cos[x][u] f(x, y)
        let mut t = [[0f64; 8]; 8];
        for u in 0..8 {
            for y in 0..8 {
                t[u][y] = (0..8).map(|x| cos[x][u] * f[y * 8 + x]).sum();
            }
        }
        let mut zz = [0i32; 64];
        for (k, &natural) in ZIGZAG.iter().enumerate() {
            let (v, u) = (usize::from(natural) / 8, usize::from(natural) % 8);
            let coef: f64 = (0..8).map(|y| cos[y][v] * t[u][y]).sum::<f64>() * cu(u) * cu(v) / 4.0;
            zz[k] = (coef / f64::from(qt[usize::from(natural)])).round() as i32;
        }
        zz
    }

    /// Mise à l'échelle des tables par la qualité (formule usuelle de l'IJG).
    pub fn scaled_table(base: &[u32; 64], quality: u32, max: u32) -> [u32; 64] {
        let q = quality.clamp(1, 100);
        let s = if q < 50 { 5000 / q } else { 200 - 2 * q };
        let mut t = [0u32; 64];
        for (o, &b) in t.iter_mut().zip(base) {
            *o = ((b * s + 50) / 100).clamp(1, max);
        }
        t
    }

    /// Encode `pixels` (`ncomp` octets par pixel, ligne par ligne).
    pub fn encode(width: usize, height: usize, ncomp: usize, pixels: &[u8], p: &Params) -> Vec<u8> {
        assert_eq!(pixels.len(), width * height * ncomp);
        let (hmax, vmax) = (p.hy, p.vy);
        let transform =
            (ncomp == 3 && !p.rgb_ids && p.adobe != Some(0)) || (ncomp == 4 && p.adobe == Some(2));
        let scale = f64::from(1u32 << (p.precision - 8));
        let level = f64::from(1u32 << (p.precision - 1));
        // Plans pleine résolution, en unités de la précision demandée.
        let mut full: Vec<Vec<f64>> = vec![vec![0.0; width * height]; ncomp];
        for i in 0..width * height {
            let px = &pixels[i * ncomp..(i + 1) * ncomp];
            let mut vals: Vec<f64> = px.iter().map(|&v| f64::from(v)).collect();
            if transform {
                let ycc = rgb_to_ycc(vals[0], vals[1], vals[2]);
                vals[..3].copy_from_slice(&ycc);
            }
            // Les valeurs YCbCr restent en virgule flottante : l'encodeur de
            // test n'ajoute ainsi pas d'erreur d'arrondi à celle du codec.
            for (plane, v) in full.iter_mut().zip(&vals) {
                plane[i] = v.clamp(0.0, 255.0) * scale;
            }
        }
        let qmax = if p.dqt16 { 65535 } else { 255 };
        let qts = [
            scaled_table(&QT_LUMA, p.quality, qmax),
            scaled_table(&QT_CHROMA, p.quality, qmax),
        ];
        let mcus_x = width.div_ceil(8 * hmax);
        let mcus_y = height.div_ceil(8 * vmax);
        let mut comps = Vec::new();
        for c in 0..ncomp {
            let (h, v) = if c == 0 { (hmax, vmax) } else { (1, 1) };
            let luma = c == 0 || (ncomp == 4 && c == 3);
            let cw = (width * h).div_ceil(hmax);
            let ch = (height * v).div_ceil(vmax);
            let (fx, fy) = (hmax / h, vmax / v);
            // Sous-échantillonnage par moyenne de boîte.
            let mut samples = vec![0f64; cw * ch];
            for sy in 0..ch {
                for sx in 0..cw {
                    let mut sum = 0.0;
                    let mut n = 0.0;
                    for dy in 0..fy {
                        for dx in 0..fx {
                            let (x, y) = (sx * fx + dx, sy * fy + dy);
                            if x < width && y < height {
                                sum += full[c][y * width + x];
                                n += 1.0;
                            }
                        }
                    }
                    samples[sy * cw + sx] = sum / n;
                }
            }
            let (bw, bh) = (mcus_x * h, mcus_y * v);
            let mut blocks = Vec::with_capacity(bw * bh);
            let tq = usize::from(!luma);
            for by in 0..bh {
                for bx in 0..bw {
                    let mut f = [0f64; 64];
                    for y in 0..8 {
                        for x in 0..8 {
                            let sx = (bx * 8 + x).min(cw - 1);
                            let sy = (by * 8 + y).min(ch - 1);
                            f[y * 8 + x] = samples[sy * cw + sx] - level;
                        }
                    }
                    blocks.push(fdct_quantize(&f, &qts[tq]));
                }
            }
            let id = if p.rgb_ids { b"RGB"[c] } else { c as u8 + 1 };
            assert_eq!(blocks.len(), bw * bh);
            comps.push(Comp {
                id,
                h,
                v,
                tq: tq as u8,
                tables: tq,
                cw,
                ch,
                bw,
                blocks,
            });
        }
        write_file(width, height, &comps, &qts, p)
    }

    fn write_file(
        width: usize,
        height: usize,
        comps: &[Comp],
        qts: &[[u32; 64]; 2],
        p: &Params,
    ) -> Vec<u8> {
        let mut w = Writer::new();
        w.marker(0xD8);
        if p.jfif {
            w.segment(0xE0, b"JFIF\0\x01\x01\x00\x00\x01\x00\x01\x00\x00");
        }
        if let Some(t) = p.adobe {
            let mut body = b"Adobe\0\x64\0\0\0\0".to_vec();
            body.push(t);
            w.segment(0xEE, &body);
        }
        // DQT (B.2.4.1), en ordre zigzag.
        let mut dqt = Vec::new();
        for (tq, qt) in qts.iter().enumerate() {
            dqt.push(if p.dqt16 { 0x10 } else { 0 } | tq as u8);
            for &natural in &ZIGZAG {
                let q = qt[usize::from(natural)];
                if p.dqt16 {
                    dqt.extend_from_slice(&(q as u16).to_be_bytes());
                } else {
                    dqt.push(q as u8);
                }
            }
        }
        w.segment(0xDB, &dqt);
        // SOF (B.2.2).
        let sof = if p.progressive {
            0xC2
        } else if p.precision == 12 {
            0xC1
        } else {
            0xC0
        };
        let mut body = vec![p.precision];
        let declared_height = if p.dnl { 0 } else { height as u16 };
        body.extend_from_slice(&declared_height.to_be_bytes());
        body.extend_from_slice(&(width as u16).to_be_bytes());
        body.push(comps.len() as u8);
        for c in comps {
            body.extend_from_slice(&[c.id, ((c.h << 4) | c.v) as u8, c.tq]);
        }
        w.segment(sof, &body);
        // DHT (B.2.4.2).
        let generic = p.generic_tables || p.progressive || p.precision == 12;
        let generic_vals = generic_vals();
        let (dc_codes, ac_codes, dht) = if generic {
            let mut dht = vec![0x00];
            dht.extend_from_slice(&GENERIC_BITS);
            dht.extend_from_slice(&generic_vals);
            dht.push(0x10);
            dht.extend_from_slice(&GENERIC_BITS);
            dht.extend_from_slice(&generic_vals);
            let g = || build_codes(&GENERIC_BITS, &generic_vals);
            (vec![g(), g()], vec![g(), g()], dht)
        } else {
            let mut dht = Vec::new();
            for (class_id, bits, vals) in [
                (0x00u8, &DC_LUMA_BITS, &DC_VALS[..]),
                (0x01, &DC_CHROMA_BITS, &DC_VALS[..]),
                (0x10, &AC_LUMA_BITS, &AC_LUMA_VALS[..]),
                (0x11, &AC_CHROMA_BITS, &AC_CHROMA_VALS[..]),
            ] {
                dht.push(class_id);
                dht.extend_from_slice(bits);
                dht.extend_from_slice(vals);
            }
            (
                vec![
                    build_codes(&DC_LUMA_BITS, &DC_VALS),
                    build_codes(&DC_CHROMA_BITS, &DC_VALS),
                ],
                vec![
                    build_codes(&AC_LUMA_BITS, &AC_LUMA_VALS),
                    build_codes(&AC_CHROMA_BITS, &AC_CHROMA_VALS),
                ],
                dht,
            )
        };
        w.segment(0xC4, &dht);
        if p.restart_interval > 0 {
            w.segment(0xDD, &(p.restart_interval as u16).to_be_bytes());
        }
        let mcus = (
            width.div_ceil(8 * comps[0].h),
            height.div_ceil(8 * comps[0].v),
        );
        let table_of = |ci: usize| if generic { 0 } else { comps[ci].tables };
        let sos = |w: &mut Writer, scan: &[usize], ss: u8, se: u8, ah: u8, al: u8| {
            let mut body = vec![scan.len() as u8];
            for &ci in scan {
                let t = table_of(ci) as u8;
                body.extend_from_slice(&[comps[ci].id, (t << 4) | t]);
            }
            body.extend_from_slice(&[ss, se, (ah << 4) | al]);
            w.segment(0xDA, &body);
        };
        let all: Vec<usize> = (0..comps.len()).collect();
        let ri = p.restart_interval;
        let mut first_scan_done = false;
        let dnl = |w: &mut Writer, done: &mut bool| {
            if p.dnl && !*done {
                w.segment(0xDC, &(height as u16).to_be_bytes());
                *done = true;
            }
        };
        if !p.progressive {
            sos(&mut w, &all, 0, 63, 0, 0);
            let mut preds = [0i32; 4];
            run_scan(&mut w, &all, comps, mcus, ri, |w, e| match e {
                Event::Block(ci, b) => {
                    let t = table_of(ci);
                    baseline_block(
                        w,
                        &comps[ci].blocks[b],
                        &mut preds[ci],
                        &dc_codes[t],
                        &ac_codes[t],
                    );
                }
                Event::Restart => preds = [0; 4],
                Event::End => {}
            });
            dnl(&mut w, &mut first_scan_done);
            w.marker(0xD9);
            return w.out;
        }
        // Progressif : DC (Al = 1), AC 1-5 et 6-63 (Al = 2), affinement AC
        // (2 → 1), affinement DC, affinement AC (1 → 0).
        let dc_scan = |w: &mut Writer, ah: u8, al: u8| {
            sos(w, &all, 0, 0, ah, al);
            let mut preds = [0i32; 4];
            run_scan(w, &all, comps, mcus, ri, |w, e| match e {
                Event::Block(ci, b) => {
                    let v = comps[ci].blocks[b][0] >> al;
                    if ah == 0 {
                        let diff = v - preds[ci];
                        preds[ci] = v;
                        let s = category(diff);
                        w.symbol(&dc_codes[0], s as u8);
                        w.bits(value_bits(diff, s), s);
                    } else {
                        w.bits((v & 1) as u32, 1);
                    }
                }
                Event::Restart => preds = [0; 4],
                Event::End => {}
            });
        };
        let ac_scan = |w: &mut Writer, ci: usize, band: (usize, usize), ah: u32, al: u32| {
            sos(w, &[ci], band.0 as u8, band.1 as u8, ah as u8, al as u8);
            let mut st = AcState::default();
            run_scan(w, &[ci], comps, mcus, ri, |w, e| match e {
                Event::Block(_, b) => {
                    if ah == 0 {
                        ac_first_block(w, &comps[ci].blocks[b], band, al, &mut st, &ac_codes[0]);
                    } else {
                        ac_refine_block(w, &comps[ci].blocks[b], band, al, &mut st, &ac_codes[0]);
                    }
                }
                Event::Restart | Event::End => st.flush(w, &ac_codes[0]),
            });
        };
        dc_scan(&mut w, 0, 1);
        dnl(&mut w, &mut first_scan_done);
        for &ci in &all {
            ac_scan(&mut w, ci, (1, 5), 0, 2);
        }
        for &ci in &all {
            ac_scan(&mut w, ci, (6, 63), 0, 2);
        }
        for &ci in &all {
            ac_scan(&mut w, ci, (1, 63), 2, 1);
        }
        dc_scan(&mut w, 1, 0);
        for &ci in &all {
            ac_scan(&mut w, ci, (1, 63), 1, 0);
        }
        w.marker(0xD9);
        w.out
    }
}

use encoder::Params;

// ---------------------------------------------------------------------------
// Images de test
// ---------------------------------------------------------------------------

/// Générateur pseudo-aléatoire déterministe (LCG 64 bits).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
}

fn gradient(w: usize, h: usize, n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * n);
    for y in 0..h {
        for x in 0..w {
            let r = (x * 255 / w.max(2).saturating_sub(1).max(1)) as u8;
            let g = (y * 255 / h.max(2).saturating_sub(1).max(1)) as u8;
            let b = 255 - ((x + y) * 255 / (w + h - 2).max(1)) as u8;
            let k = ((x * y) % 256) as u8;
            out.extend_from_slice(&[r, g, b, k][..n]);
        }
    }
    out
}

/// Dégradé à faible pente (2 niveaux par pixel, chromas très lisses) :
/// le sous-échantillonnage puis le sur-échantillonnage des chromas n'y
/// introduisent qu'une erreur inférieure au niveau, même aux bords où
/// l'interpolation réplique l'échantillon extrême.
fn smooth(w: usize, h: usize, n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * n);
    for y in 0..h {
        for x in 0..w {
            let r = (2 * x).min(255) as u8;
            let g = (2 * y).min(255) as u8;
            let b = (128 + x as i32 - y as i32).clamp(0, 255) as u8;
            let k = (40 + x + y).min(255) as u8;
            out.extend_from_slice(&[r, g, b, k][..n]);
        }
    }
    out
}

fn checker(w: usize, h: usize, n: usize, size: usize) -> Vec<u8> {
    let a = [30u8, 200, 90, 10];
    let b = [220u8, 40, 160, 240];
    let mut out = Vec::with_capacity(w * h * n);
    for y in 0..h {
        for x in 0..w {
            let dark = ((x / size) + (y / size)) % 2 == 0;
            out.extend_from_slice(&(if dark { a } else { b })[..n]);
        }
    }
    out
}

fn noise(w: usize, h: usize, n: usize, gray: bool) -> Vec<u8> {
    let mut rng = Lcg(42);
    let mut out = Vec::with_capacity(w * h * n);
    for _ in 0..w * h {
        let v = (rng.next() & 255) as u8;
        for _ in 0..n {
            out.push(if gray { v } else { (rng.next() & 255) as u8 });
        }
    }
    out
}

fn max_diff(a: &[u8], b: &[u8]) -> u32 {
    assert_eq!(a.len(), b.len(), "tailles différentes");
    a.iter()
        .zip(b)
        .map(|(&x, &y)| u32::from(x.abs_diff(y)))
        .max()
        .unwrap_or(0)
}

fn round_trip(w: usize, h: usize, n: usize, pixels: &[u8], p: &Params, tol: u32) -> JpegImage {
    let jpeg = encoder::encode(w, h, n, pixels, p);
    let img = decode(&jpeg).expect("décodage");
    assert_eq!(
        (img.width, img.height, img.components),
        (w as u32, h as u32, n as u8)
    );
    assert_eq!(img.progressive, p.progressive);
    let diff = max_diff(&img.data, pixels);
    assert!(
        diff <= tol,
        "écart maximal {diff} > {tol} ({w}×{h}, {n} comp., {}×{})",
        p.hy,
        p.vy
    );
    assert_eq!(read_header(&jpeg), Ok((w as u32, h as u32, n as u8)));
    img
}

fn hex(s: &str) -> Vec<u8> {
    let clean: Vec<u8> = s.bytes().filter(u8::is_ascii_hexdigit).collect();
    clean
        .chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}

// ---------------------------------------------------------------------------
// IDCT
// ---------------------------------------------------------------------------

/// IDCT de référence, formule directe de A.3.3 en `f64`, O(n⁴).
fn naive_idct(coeffs: &[i16; 64]) -> [f64; 64] {
    let cu = |u: usize| {
        if u == 0 {
            std::f64::consts::FRAC_1_SQRT_2
        } else {
            1.0
        }
    };
    let mut out = [0f64; 64];
    for y in 0..8 {
        for x in 0..8 {
            let mut s = 0.0;
            for v in 0..8 {
                for u in 0..8 {
                    s += cu(u)
                        * cu(v)
                        * f64::from(coeffs[v * 8 + u])
                        * ((2.0 * x as f64 + 1.0) * u as f64 * std::f64::consts::PI / 16.0).cos()
                        * ((2.0 * y as f64 + 1.0) * v as f64 * std::f64::consts::PI / 16.0).cos();
                }
            }
            out[y * 8 + x] = s / 4.0;
        }
    }
    out
}

#[test]
fn idct_matches_naive_reference_within_one_lsb() {
    let idct = Idct::new(1.0);
    let qt = [1u16; 64];
    let mut rng = Lcg(7);
    let mut worst = 0.0f64;
    for iteration in 0..2000 {
        let mut coeffs = [0i16; 64];
        // Blocs denses, épars et à DC seul, amplitudes réalistes.
        let density = iteration % 4;
        for (i, c) in coeffs.iter_mut().enumerate() {
            let keep = match density {
                0 => true,
                1 => rng.next() % 4 == 0,
                2 => i < 10,
                _ => i == 0,
            };
            if keep {
                let range = if i == 0 { 1024 } else { 300 };
                *c = (rng.next() % (2 * range)) as i16 - range as i16;
            }
        }
        let reference = naive_idct(&coeffs);
        let mut out = [0u8; 64];
        idct.block(&coeffs, &qt, &mut out, 8);
        for (o, r) in out.iter().zip(&reference) {
            let ideal = (r + 128.0).clamp(0.0, 255.0);
            let err = (f64::from(*o) - ideal).abs();
            // L'idéal est arrondi au plus près : l'écart après arrondi vaut
            // au plus 0,5 + l'erreur numérique, donc < 1 LSB.
            worst = worst.max(err);
            assert!(err <= 1.0, "erreur IDCT {err} (bloc {iteration})");
        }
    }
    println!("erreur IDCT maximale avant arrondi : {worst:.4} LSB");
    assert!(worst <= 1.0);
}

#[test]
fn idct_dc_only_block_is_uniform() {
    let idct = Idct::new(1.0);
    let mut coeffs = [0i16; 64];
    coeffs[0] = 80;
    let qt = [2u16; 64];
    let mut out = [0u8; 64];
    idct.block(&coeffs, &qt, &mut out, 8);
    // 80 × 2 / 8 = 20 → 148.
    assert!(out.iter().all(|&v| v == 148));
}

// ---------------------------------------------------------------------------
// Tables de Huffman
// ---------------------------------------------------------------------------

#[test]
fn huffman_rejects_oversubscribed_table() {
    let mut bits = [0u8; 16];
    bits[0] = 3; // trois codes de 1 bit : impossible.
    assert!(HuffTable::build(&bits, &[1, 2, 3]).is_err());
    let mut bits = [0u8; 16];
    bits[0] = 2;
    bits[1] = 1; // 2 × 1/2 + 1/4 > 1.
    assert!(HuffTable::build(&bits, &[1, 2, 3]).is_err());
}

#[test]
fn huffman_accepts_incomplete_table_and_rejects_unknown_code() {
    let mut bits = [0u8; 16];
    bits[1] = 1; // un seul code de 2 bits : « 00 » → 7.
    let table = HuffTable::build(&bits, &[7]).unwrap();
    let data = [0b0000_0000, 0xFF, 0xFF];
    let mut reader = BitReader::new(&data, 0);
    assert_eq!(table.decode(&mut reader), Some(7));
    let data = [0b1100_0000];
    let mut reader = BitReader::new(&data, 0);
    assert_eq!(table.decode(&mut reader), None);
}

#[test]
fn huffman_long_codes_use_slow_path() {
    // Un code de 12 bits et un de 16 bits.
    let mut bits = [0u8; 16];
    bits[11] = 1;
    bits[15] = 1;
    let table = HuffTable::build(&bits, &[0xAB, 0xCD]).unwrap();
    // Code 12 bits = 0000 0000 0000, code 16 bits = 0000 0000 0001 0000,
    // soit 28 bits : 0000 0000 | 0000 0000 | 0000 0001 | 0000 ….
    let data = [0x00, 0x00, 0x01, 0x00, 0x00];
    let mut reader = BitReader::new(&data, 0);
    assert_eq!(table.decode(&mut reader), Some(0xAB));
    assert_eq!(table.decode(&mut reader), Some(0xCD));
}

#[test]
fn bit_reader_unstuffs_ff00_and_stops_at_markers() {
    let data = [0xFF, 0x00, 0xA5, 0xFF, 0xD9];
    let mut reader = BitReader::new(&data, 0);
    assert_eq!(reader.receive(8), 0xFF);
    assert_eq!(reader.receive(8), 0xA5);
    assert!(!reader.overrun());
    assert_eq!(reader.receive(8), 0);
    assert!(reader.overrun());
    assert_eq!(reader.stop, Some(0xD9));
}

// ---------------------------------------------------------------------------
// Allers-retours avec l'encodeur de test
// ---------------------------------------------------------------------------

#[test]
fn gray_gradient_checker_noise_round_trip() {
    let p = Params {
        quality: 100,
        ..Params::default()
    };
    round_trip(32, 24, 1, &gradient(32, 24, 1), &p, 3);
    round_trip(32, 24, 1, &checker(32, 24, 1, 4), &p, 3);
    round_trip(32, 24, 1, &noise(32, 24, 1, true), &p, 3);
    // Qualité moindre : un dégradé reste proche.
    let p = Params {
        quality: 90,
        ..Params::default()
    };
    round_trip(40, 40, 1, &gradient(40, 40, 1), &p, 3);
}

#[test]
fn ycbcr_444_round_trip() {
    let p = Params {
        quality: 100,
        ..Params::default()
    };
    round_trip(24, 16, 3, &gradient(24, 16, 3), &p, 3);
    round_trip(24, 16, 3, &checker(24, 16, 3, 8), &p, 3);
    round_trip(16, 16, 3, &noise(16, 16, 3, false), &p, 3);
}

#[test]
fn ycbcr_420_422_411_round_trip() {
    // Chromas lisses (dégradé) ou constantes (bruit gris encodé en RGB) :
    // le sous-échantillonnage n'introduit alors pas d'erreur significative.
    for (hy, vy) in [
        (2, 2),
        (2, 1),
        (1, 2),
        (4, 1),
        (4, 2),
        (2, 4),
        (3, 1),
        (3, 3),
    ] {
        let p = Params {
            hy,
            vy,
            quality: 100,
            ..Params::default()
        };
        // Rapports 2:1 : interpolation triangulaire, erreur < 1 niveau de
        // chroma. Autres rapports : réplication de la moyenne de boîte, dont
        // les pixels extrêmes s'écartent de (f − 1) / 2 pas du dégradé, soit
        // jusqu'à 1,5 pas de chroma (× 1,77 en RGB) pour f = 4.
        let tol = if hy <= 2 && vy <= 2 {
            3
        } else {
            2 + hy.max(vy) as u32
        };
        round_trip(37, 29, 3, &smooth(37, 29, 3), &p, tol);
        round_trip(33, 17, 3, &noise(33, 17, 3, true), &p, 3);
    }
}

#[test]
fn restart_intervals_round_trip() {
    for ri in [1, 2, 5] {
        for (hy, vy) in [(1, 1), (2, 2)] {
            let p = Params {
                hy,
                vy,
                quality: 100,
                restart_interval: ri,
                ..Params::default()
            };
            round_trip(45, 21, 3, &smooth(45, 21, 3), &p, 3);
            round_trip(45, 21, 1, &checker(45, 21, 1, 3), &p, 3);
        }
    }
}

#[test]
fn tiny_and_odd_dimensions_round_trip() {
    let p = Params {
        quality: 100,
        ..Params::default()
    };
    round_trip(1, 1, 1, &[77], &p, 3);
    round_trip(1, 1, 3, &[10, 200, 30], &p, 3);
    round_trip(9, 1, 3, &gradient(9, 1, 3), &p, 3);
    round_trip(1, 17, 1, &gradient(1, 17, 1), &p, 3);
    let p = Params {
        hy: 2,
        vy: 2,
        quality: 100,
        ..Params::default()
    };
    round_trip(1, 1, 3, &[10, 200, 30], &p, 3);
    round_trip(17, 9, 3, &smooth(17, 9, 3), &p, 3);
}

#[test]
fn generic_huffman_table_and_16_bit_dqt_round_trip() {
    let p = Params {
        quality: 100,
        generic_tables: true,
        dqt16: true,
        ..Params::default()
    };
    round_trip(24, 24, 3, &checker(24, 24, 3, 6), &p, 3);
}

#[test]
fn twelve_bit_precision_is_reduced_to_eight_bits() {
    let p = Params {
        quality: 100,
        precision: 12,
        ..Params::default()
    };
    round_trip(24, 16, 1, &gradient(24, 16, 1), &p, 3);
    round_trip(24, 16, 3, &gradient(24, 16, 3), &p, 3);
}

#[test]
fn rgb_component_ids_disable_color_transform() {
    let pixels = checker(16, 16, 3, 4);
    let p = Params {
        quality: 100,
        rgb_ids: true,
        jfif: false,
        ..Params::default()
    };
    round_trip(16, 16, 3, &pixels, &p, 3);
}

#[test]
fn adobe_transform_flag_controls_color_conversion() {
    let pixels = gradient(16, 16, 3);
    // APP14 transform = 0 : composantes brutes.
    let p = Params {
        quality: 100,
        adobe: Some(0),
        ..Params::default()
    };
    let img = round_trip(16, 16, 3, &pixels, &p, 3);
    assert!(!img.adobe_inverted);
    // APP14 transform = 1 : YCbCr.
    let p = Params {
        quality: 100,
        adobe: Some(1),
        ..Params::default()
    };
    round_trip(16, 16, 3, &pixels, &p, 3);
}

#[test]
fn cmyk_and_ycck_are_returned_uninverted_with_flag() {
    let pixels = smooth(16, 16, 4);
    let p = Params {
        quality: 100,
        adobe: Some(0),
        ..Params::default()
    };
    let img = round_trip(16, 16, 4, &pixels, &p, 3);
    assert!(img.adobe_inverted);
    let p = Params {
        quality: 100,
        adobe: Some(2),
        hy: 2,
        vy: 2,
        ..Params::default()
    };
    let img = round_trip(16, 16, 4, &pixels, &p, 3);
    assert!(img.adobe_inverted);
    // Sans APP14 : CMYK brut, pas d'inversion signalée.
    let p = Params {
        quality: 100,
        adobe: None,
        ..Params::default()
    };
    let img = round_trip(16, 16, 4, &pixels, &p, 3);
    assert!(!img.adobe_inverted);
}

#[test]
fn dnl_defines_height_when_sof_has_zero_lines() {
    let pixels = gradient(20, 12, 1);
    let p = Params {
        quality: 100,
        dnl: true,
        ..Params::default()
    };
    round_trip(20, 12, 1, &pixels, &p, 3);
    let p = Params {
        quality: 100,
        dnl: true,
        progressive: true,
        ..Params::default()
    };
    round_trip(20, 12, 1, &pixels, &p, 3);
}

// ---------------------------------------------------------------------------
// Progressif
// ---------------------------------------------------------------------------

#[test]
fn progressive_round_trip_gray_and_color() {
    for (hy, vy, ri) in [(1, 1, 0), (2, 2, 0), (2, 1, 3), (1, 1, 1), (2, 2, 4)] {
        for quality in [100, 75] {
            let p = Params {
                hy,
                vy,
                quality,
                restart_interval: ri,
                progressive: true,
                ..Params::default()
            };
            let baseline = Params {
                progressive: false,
                ..p.clone()
            };
            for (w, h) in [(37, 29), (16, 16), (8, 8)] {
                for (n, pixels) in [
                    (1, gradient(w, h, 1)),
                    (3, smooth(w, h, 3)),
                    (3, checker(w, h, 3, 5)),
                    (1, noise(w, h, 1, true)),
                ] {
                    let progressive = encoder::encode(w, h, n, &pixels, &p);
                    let sequential = encoder::encode(w, h, n, &pixels, &baseline);
                    let a = decode(&progressive).expect("progressif");
                    let b = decode(&sequential).expect("séquentiel");
                    assert!(a.progressive);
                    assert!(!b.progressive);
                    // Mêmes coefficients ⇒ mêmes pixels, au bit près.
                    assert_eq!(
                        a.data, b.data,
                        "{w}×{h} {n} comp. {hy}×{vy} ri={ri} q={quality}"
                    );
                    // Le damier couleur sous-échantillonné lisse ses bords
                    // de chroma : on ne le compare pas aux pixels d'origine.
                    let subsampled_checker = hy * vy > 1 && n == 3 && pixels[0] == 30;
                    if quality == 100 && !subsampled_checker {
                        assert!(
                            max_diff(&a.data, &pixels) <= 3,
                            "{w}×{h} {n} comp. {hy}×{vy}"
                        );
                    }
                    assert_eq!(read_header(&progressive), Ok((w as u32, h as u32, n as u8)));
                }
            }
        }
    }
}

#[test]
fn progressive_truncated_after_first_scans_is_a_coarse_image() {
    let pixels = gradient(32, 32, 1);
    let p = Params {
        quality: 100,
        progressive: true,
        ..Params::default()
    };
    let jpeg = encoder::encode(32, 32, 1, &pixels, &p);
    // Coupe après le deuxième SOS : DC et basses fréquences seulement.
    let mut sos_positions = Vec::new();
    for i in 0..jpeg.len() - 1 {
        if jpeg[i] == 0xFF && jpeg[i + 1] == 0xDA {
            sos_positions.push(i);
        }
    }
    assert!(sos_positions.len() >= 3);
    let cut = &jpeg[..sos_positions[2]];
    let img = decode(cut).expect("image partielle");
    assert_eq!((img.width, img.height), (32, 32));
    // Approximation grossière mais corrélée (DC à 1 bit près sur 8 × 8).
    assert!(max_diff(&img.data, &pixels) <= 40);
}

// ---------------------------------------------------------------------------
// Vecteurs réels (GDI+)
// ---------------------------------------------------------------------------

fn check_vector(name: &str, jpeg_hex: &str, expected: &[u8], w: u32, h: u32, tol: u32) {
    let jpeg = hex(jpeg_hex);
    assert_eq!(read_header(&jpeg), Ok((w, h, 3)), "{name}");
    let img = decode(&jpeg).expect(name);
    assert_eq!((img.width, img.height, img.components), (w, h, 3));
    assert!(!img.progressive && !img.adobe_inverted);
    let diff = max_diff(&img.data, expected);
    let mean: f64 = img
        .data
        .iter()
        .zip(expected)
        .map(|(&a, &b)| f64::from(a.abs_diff(b)))
        .sum::<f64>()
        / expected.len() as f64;
    println!("{name} : écart max {diff}, écart moyen {mean:.3}");
    assert!(diff <= tol, "{name} : écart maximal {diff} > {tol}");
}

#[test]
fn real_gdiplus_jpeg_grad16() {
    check_vector(
        "grad16",
        vectors::GRAD16_JPEG,
        vectors::GRAD16_RGB,
        16,
        16,
        2,
    );
}

#[test]
fn real_gdiplus_jpeg_gray16() {
    check_vector(
        "gray16",
        vectors::GRAY16_JPEG,
        vectors::GRAY16_RGB,
        16,
        16,
        2,
    );
}

#[test]
fn real_gdiplus_jpeg_grad37x29() {
    check_vector(
        "grad37x29",
        vectors::GRAD37X29_JPEG,
        vectors::GRAD37X29_RGB,
        37,
        29,
        2,
    );
}

// ---------------------------------------------------------------------------
// Tolérance et robustesse
// ---------------------------------------------------------------------------

#[test]
fn garbage_before_soi_and_missing_eoi_are_tolerated() {
    let pixels = gradient(16, 16, 1);
    let p = Params {
        quality: 100,
        ..Params::default()
    };
    let jpeg = encoder::encode(16, 16, 1, &pixels, &p);
    let mut padded = b"\r\n%%garbage\xFF\x00".to_vec();
    padded.extend_from_slice(&jpeg[..jpeg.len() - 2]); // sans EOI
    let img = decode(&padded).expect("décodage");
    assert!(max_diff(&img.data, &pixels) <= 3);
    assert_eq!(read_header(&padded), Ok((16, 16, 1)));
}

#[test]
fn unknown_app_and_comment_segments_are_skipped() {
    let pixels = gradient(16, 16, 3);
    let p = Params {
        quality: 100,
        ..Params::default()
    };
    let jpeg = encoder::encode(16, 16, 3, &pixels, &p);
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1, 0x00, 0x05, b'E', b'x', b'f']); // APP1
    out.extend_from_slice(&[0xFF, 0xFE, 0x00, 0x08, b'c', b'o', b'm', b'm', b'e', b'n']); // COM
    out.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xED, 0x00, 0x02]); // remplissage + APP13 vide
    out.extend_from_slice(&jpeg[2..]);
    let img = decode(&out).expect("décodage");
    assert!(max_diff(&img.data, &pixels) <= 3);
}

#[test]
fn truncated_scan_yields_gray_bottom_rows() {
    let pixels = gradient(64, 64, 1);
    let p = Params {
        quality: 100,
        ..Params::default()
    };
    let jpeg = encoder::encode(64, 64, 1, &pixels, &p);
    let sos = jpeg.windows(2).position(|w| w == [0xFF, 0xDA]).unwrap();
    let cut = sos + (jpeg.len() - sos) / 2;
    let img = decode(&jpeg[..cut]).expect("image partielle");
    assert_eq!((img.width, img.height), (64, 64));
    // Première ligne intacte, dernière ligne grise.
    assert!(max_diff(&img.data[..64], &pixels[..64]) <= 3);
    assert!(img.data[63 * 64..].iter().all(|&v| v == 128));
}

#[test]
fn every_truncation_length_never_panics() {
    let pixels = smooth(24, 20, 3);
    for progressive in [false, true] {
        let p = Params {
            hy: 2,
            vy: 2,
            quality: 90,
            restart_interval: 2,
            progressive,
            ..Params::default()
        };
        let jpeg = encoder::encode(24, 20, 3, &pixels, &p);
        for len in 0..jpeg.len() {
            let _ = decode(&jpeg[..len]);
            let _ = read_header(&jpeg[..len]);
        }
        let full = decode(&jpeg).unwrap();
        assert!(max_diff(&full.data, &pixels) <= 12);
    }
}

#[test]
fn random_mutations_never_panic() {
    let pixels = gradient(24, 20, 3);
    let files = [
        encoder::encode(
            24,
            20,
            3,
            &pixels,
            &Params {
                hy: 2,
                vy: 2,
                restart_interval: 3,
                ..Params::default()
            },
        ),
        encoder::encode(
            24,
            20,
            3,
            &pixels,
            &Params {
                progressive: true,
                ..Params::default()
            },
        ),
    ];
    let mut rng = Lcg(2024);
    for file in &files {
        for _ in 0..1500 {
            let mut data = file.clone();
            let flips = 1 + rng.next() % 8;
            for _ in 0..flips {
                let i = rng.next() as usize % data.len();
                data[i] = (rng.next() & 255) as u8;
            }
            let _ = decode(&data);
            let _ = read_header(&data);
        }
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Lcg(99);
    for len in [0usize, 1, 2, 3, 4, 16, 64, 300, 4096] {
        for _ in 0..40 {
            let data: Vec<u8> = (0..len).map(|_| (rng.next() & 255) as u8).collect();
            let _ = decode(&data);
            let _ = read_header(&data);
            // Variante avec SOI valide devant.
            let mut with_soi = vec![0xFF, 0xD8, 0xFF];
            with_soi.extend_from_slice(&data);
            let _ = decode(&with_soi);
        }
    }
}

fn frame_only(sof: u8, precision: u8, height: u16, width: u16, comps: &[(u8, u8, u8)]) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8, 0xFF, sof];
    let len = 8 + 3 * comps.len();
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.push(precision);
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.push(comps.len() as u8);
    for &(id, hv, tq) in comps {
        out.extend_from_slice(&[id, hv, tq]);
    }
    out
}

#[test]
fn absurd_dimensions_are_refused_without_allocating() {
    // 65535 × 65535 = 4,3 milliards de pixels > 2³¹.
    let data = frame_only(0xC0, 8, 65535, 65535, &[(1, 0x11, 0)]);
    assert!(matches!(decode(&data), Err(Error::Corrupt(_))));
    assert!(matches!(read_header(&data), Err(Error::Corrupt(_))));
    // 2³¹ pixels mais 4 composantes : tampon > 1 Go.
    let data = frame_only(
        0xC0,
        8,
        32768,
        65535,
        &[(1, 0x11, 0), (2, 0x11, 0), (3, 0x11, 0), (4, 0x11, 0)],
    );
    assert!(matches!(read_header(&data), Err(Error::Corrupt(_))));
    // Dimensions nulles sans DNL.
    let data = frame_only(0xC0, 8, 0, 10, &[(1, 0x11, 0)]);
    assert!(matches!(read_header(&data), Err(Error::Corrupt(_))));
    let data = frame_only(0xC0, 8, 10, 0, &[(1, 0x11, 0)]);
    assert!(matches!(read_header(&data), Err(Error::Corrupt(_))));
    // Facteurs d'échantillonnage nuls ou > 4.
    let data = frame_only(0xC0, 8, 8, 8, &[(1, 0x01, 0)]);
    assert!(matches!(decode(&data), Err(Error::Corrupt(_))));
    let data = frame_only(0xC0, 8, 8, 8, &[(1, 0x51, 0)]);
    assert!(matches!(decode(&data), Err(Error::Corrupt(_))));
    // Trame acceptable mais aucun scan.
    let data = frame_only(0xC0, 8, 8, 8, &[(1, 0x11, 0)]);
    assert!(matches!(decode(&data), Err(Error::Corrupt(_))));
    assert_eq!(read_header(&data), Ok((8, 8, 1)));
}

#[test]
fn unsupported_processes_are_reported() {
    for sof in [0xC3u8, 0xC5, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCF] {
        let data = frame_only(sof, 8, 8, 8, &[(1, 0x11, 0)]);
        assert!(
            matches!(decode(&data), Err(Error::Unsupported(_))),
            "SOF {sof:#x}"
        );
        assert!(matches!(read_header(&data), Err(Error::Unsupported(_))));
    }
    // Précision 16 bits : inexistante.
    let data = frame_only(0xC1, 16, 8, 8, &[(1, 0x11, 0)]);
    assert!(matches!(read_header(&data), Err(Error::Unsupported(_))));
    // 12 bits en baseline (SOF0) : interdit par B.2.2.
    let data = frame_only(0xC0, 12, 8, 8, &[(1, 0x11, 0)]);
    assert!(matches!(read_header(&data), Err(Error::Unsupported(_))));
}

#[test]
fn invalid_huffman_table_before_first_scan_is_an_error() {
    let mut data = frame_only(0xC0, 8, 8, 8, &[(1, 0x11, 0)]);
    data.extend_from_slice(&[0xFF, 0xC4, 0x00, 0x16, 0x00]);
    data.extend_from_slice(&[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    data.extend_from_slice(&[0, 1, 2]);
    assert!(matches!(decode(&data), Err(Error::Corrupt(_))));
}

#[test]
fn no_soi_is_corrupt() {
    assert!(matches!(decode(b""), Err(Error::Corrupt(_))));
    assert!(matches!(decode(b"%PDF-1.7"), Err(Error::Corrupt(_))));
    assert!(matches!(read_header(b"\xFF\xD9"), Err(Error::Corrupt(_))));
}

#[test]
fn missing_restart_marker_resynchronises() {
    let pixels = gradient(48, 16, 1);
    let p = Params {
        quality: 100,
        restart_interval: 2,
        ..Params::default()
    };
    let jpeg = encoder::encode(48, 16, 1, &pixels, &p);
    // Supprime le premier marqueur RST0 : l'intervalle suivant est perdu
    // mais le décodeur se recale sur RST1.
    let rst = jpeg.windows(2).position(|w| w == [0xFF, 0xD0]).unwrap();
    let mut broken = jpeg[..rst].to_vec();
    broken.extend_from_slice(&jpeg[rst + 2..]);
    let img = decode(&broken).expect("image partielle");
    assert_eq!((img.width, img.height), (48, 16));
    // Le numéro du marqueur RST1 indique que deux intervalles ont été
    // consommés : les MCU 2 et 3 (colonnes 16 à 31 de la première rangée)
    // restent gris, tout le reste est intact.
    let first_row = &img.data[..48];
    assert!(max_diff(&first_row[..16], &pixels[..16]) <= 3);
    assert!(first_row[16..32].iter().all(|&v| v == 128));
    assert!(max_diff(&first_row[32..], &pixels[32..48]) <= 3);
    let last_row = 15 * 48;
    assert!(max_diff(&img.data[last_row..], &pixels[last_row..]) <= 3);
}

// ---------------------------------------------------------------------------
// Performance (à lancer en release : cargo test --release -p acrux-codecs -- --ignored dct)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "mesure de performance, à lancer en release"]
fn a4_300dpi_420_decodes_under_one_second() {
    let (w, h) = (2480, 3508);
    // Dégradé texturé (bruit ± 24 sur chaque canal) : presque tous les blocs
    // portent des coefficients AC, comme une page scannée.
    let mut pixels = gradient(w, h, 3);
    let mut rng = Lcg(3);
    for v in &mut pixels {
        let n = (rng.next() % 49) as i32 - 24;
        *v = (i32::from(*v) + n).clamp(0, 255) as u8;
    }
    let p = Params {
        hy: 2,
        vy: 2,
        quality: 85,
        restart_interval: 155,
        ..Params::default()
    };
    let jpeg = encoder::encode(w, h, 3, &pixels, &p);
    let start = std::time::Instant::now();
    let img = decode(&jpeg).expect("décodage");
    let elapsed = start.elapsed();
    println!(
        "A4 300 dpi 4:2:0 : {} octets JPEG décodés en {:?}",
        jpeg.len(),
        elapsed
    );
    assert_eq!((img.width, img.height), (w as u32, h as u32));
    let mean: f64 = img
        .data
        .iter()
        .zip(&pixels)
        .map(|(&a, &b)| f64::from(a.abs_diff(b)))
        .sum::<f64>()
        / pixels.len() as f64;
    println!("écart moyen après compression à 85 % : {mean:.2}");
    // Bruit ± 24 sur les chromas sous-échantillonnés : écart moyen ≈ 10.
    assert!(mean < 16.0);
    assert!(elapsed.as_secs_f64() < 1.0, "{elapsed:?}");
}
