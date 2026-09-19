//! Tests du décodeur CCITTFaxDecode.
//!
//! - `encoder` : encodeur de test T.4 (1-D et 2-D) et T.6, écrit d'après les
//!   recommandations, réutilisé par les tests JBIG2 pour les régions MMR ;
//! - vecteurs connus des tableaux de T.4 et un flux G4 codé à la main ;
//! - allers-retours sur des images synthétiques ;
//! - robustesse : troncatures à toutes les longueurs, octets aléatoires.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::too_many_lines,
    clippy::needless_range_loop
)]

use super::*;

/// Encodeur de test des codages T.4 et T.6.
pub(crate) mod encoder {
    use super::{BLACK_MAKEUP, BLACK_TERMINAL, EXTENDED_MAKEUP, WHITE_MAKEUP, WHITE_TERMINAL};

    /// Écrivain de bits, poids fort en tête.
    #[derive(Default)]
    pub(crate) struct BitWriter {
        bytes: Vec<u8>,
        /// Bits déjà écrits dans le dernier octet (0 si l'octet est complet).
        used: u32,
    }

    impl BitWriter {
        pub(crate) fn put(&mut self, code: u32, len: u32) {
            for i in (0..len).rev() {
                let bit = ((code >> i) & 1) as u8;
                if self.used == 0 {
                    self.bytes.push(0);
                }
                let last = self.bytes.len() - 1;
                self.bytes[last] |= bit << (7 - self.used);
                self.used = (self.used + 1) % 8;
            }
        }

        pub(crate) fn align(&mut self) {
            self.used = 0;
        }

        pub(crate) fn finish(self) -> Vec<u8> {
            self.bytes
        }
    }

    /// Image de test : un octet par pixel, 1 = noir.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Image {
        pub width: usize,
        pub height: usize,
        pub pixels: Vec<u8>,
    }

    impl Image {
        pub(crate) fn new(width: usize, height: usize) -> Self {
            Image {
                width,
                height,
                pixels: vec![0; width * height],
            }
        }

        pub(crate) fn row(&self, y: usize) -> &[u8] {
            &self.pixels[y * self.width..(y + 1) * self.width]
        }

        pub(crate) fn set(&mut self, x: usize, y: usize, v: u8) {
            self.pixels[y * self.width + x] = v;
        }

        /// Lignes tassées 1 bit par pixel, 1 = noir.
        pub(crate) fn packed(&self) -> Vec<u8> {
            let stride = self.width.div_ceil(8);
            let mut out = vec![0u8; stride * self.height];
            for y in 0..self.height {
                for x in 0..self.width {
                    if self.pixels[y * self.width + x] != 0 {
                        out[y * stride + x / 8] |= 0x80 >> (x % 8);
                    }
                }
            }
            out
        }
    }

    /// Éléments de changement d'une ligne (le pixel imaginaire avant la
    /// ligne est blanc), suivis de deux sentinelles `width`.
    pub(crate) fn changes(row: &[u8]) -> Vec<u32> {
        let mut out = Vec::new();
        let mut prev = 0u8;
        for (x, &p) in row.iter().enumerate() {
            if p != prev {
                out.push(x as u32);
                prev = p;
            }
        }
        out.push(row.len() as u32);
        out.push(row.len() as u32);
        out
    }

    fn put_code(w: &mut BitWriter, (len, code): (u8, u16)) {
        w.put(u32::from(code), u32::from(len));
    }

    /// Écrit une longueur de plage : codes de composition puis terminaison.
    pub(crate) fn put_run(w: &mut BitWriter, mut run: u32, black: bool) {
        let (terminal, makeup) = if black {
            (&BLACK_TERMINAL, &BLACK_MAKEUP)
        } else {
            (&WHITE_TERMINAL, &WHITE_MAKEUP)
        };
        while run >= 2560 {
            put_code(w, EXTENDED_MAKEUP[12]);
            run -= 2560;
        }
        if run >= 1792 {
            put_code(w, EXTENDED_MAKEUP[((run - 1792) / 64) as usize]);
            run -= (run - 1792) / 64 * 64 + 1792;
        } else if run >= 64 {
            put_code(w, makeup[(run / 64 - 1) as usize]);
            run %= 64;
        }
        put_code(w, terminal[run as usize]);
    }

    /// Ligne en codage unidimensionnel (T.4 §4.1).
    pub(crate) fn put_row_1d(w: &mut BitWriter, row: &[u8]) {
        let mut pos = 0usize;
        let mut black = false;
        while pos < row.len() {
            let mut end = pos;
            while end < row.len() && (row[end] != 0) == black {
                end += 1;
            }
            put_run(w, (end - pos) as u32, black);
            pos = end;
            black = !black;
        }
        if row.is_empty() {
            put_run(w, 0, false);
        }
    }

    /// Ligne en codage bidimensionnel (T.4 §4.2.1.3.2, figure 7/T.4).
    pub(crate) fn put_row_2d(w: &mut BitWriter, row: &[u8], reference: &[u8]) {
        let width = row.len() as u32;
        let cur = changes(row);
        let refc = changes(reference);
        let mut a0: i64 = -1;
        let mut black = false;
        while a0 < i64::from(width) {
            let next = |list: &[u32], from: i64, parity: bool| -> (u32, u32) {
                let mut i = 0;
                while i < list.len() && i64::from(list[i]) <= from {
                    i += 1;
                }
                if (i & 1) != usize::from(parity) {
                    i += 1;
                }
                (
                    list.get(i).copied().unwrap_or(width),
                    list.get(i + 1).copied().unwrap_or(width),
                )
            };
            let (a1, a2) = next(&cur, a0, black);
            let (b1, b2) = next(&refc, a0, black);
            if b2 < a1 {
                w.put(0b0001, 4);
                a0 = i64::from(b2);
            } else {
                let delta = i64::from(a1) - i64::from(b1);
                if delta.abs() <= 3 {
                    let (code, len) = match delta {
                        0 => (0b1, 1),
                        1 => (0b011, 3),
                        2 => (0b00_0011, 6),
                        3 => (0b000_0011, 7),
                        -1 => (0b010, 3),
                        -2 => (0b00_0010, 6),
                        _ => (0b000_0010, 7),
                    };
                    w.put(code, len);
                    a0 = i64::from(a1);
                    black = !black;
                } else {
                    w.put(0b001, 3);
                    let start = a0.max(0) as u32;
                    put_run(w, a1 - start, black);
                    put_run(w, a2 - a1, !black);
                    a0 = i64::from(a2);
                }
            }
        }
    }

    pub(crate) fn put_eol(w: &mut BitWriter) {
        w.put(1, 12);
    }

    /// Codage T.6 (Groupe 4) complet, EOFB facultatif.
    pub(crate) fn encode_g4(img: &Image, eofb: bool) -> Vec<u8> {
        let mut w = BitWriter::default();
        let blank = vec![0u8; img.width];
        for y in 0..img.height {
            let reference = if y == 0 { &blank[..] } else { img.row(y - 1) };
            put_row_2d(&mut w, img.row(y), reference);
        }
        if eofb {
            put_eol(&mut w);
            put_eol(&mut w);
        }
        w.finish()
    }

    /// Options du codage Groupe 3.
    #[derive(Debug, Clone, Copy, Default)]
    pub(crate) struct G3Options {
        /// EOL avant chaque ligne.
        pub eol: bool,
        /// Bits de remplissage pour que chaque ligne commence sur un octet
        /// (EOL compris : l'EOL finit sur un octet).
        pub align: bool,
        /// RTC final.
        pub rtc: bool,
    }

    /// Codage T.4 (Groupe 3) : `k` = 0 pour une dimension, `k` > 0 pour le
    /// mode mixte (une ligne 1-D toutes les `k` lignes).
    pub(crate) fn encode_g3(img: &Image, k: u32, opts: G3Options) -> Vec<u8> {
        let mut w = BitWriter::default();
        let blank = vec![0u8; img.width];
        let put_prologue = |w: &mut BitWriter, one_d: bool| {
            if opts.eol {
                if opts.align {
                    // Remplissage : l'EOL (12 bits) doit finir sur un octet.
                    while (w.used + 12) % 8 != 0 {
                        w.put(0, 1);
                    }
                }
                put_eol(w);
                if k > 0 {
                    w.put(u32::from(one_d), 1);
                }
            } else if opts.align {
                w.align();
                if k > 0 {
                    w.put(u32::from(one_d), 1);
                }
            } else if k > 0 {
                w.put(u32::from(one_d), 1);
            }
        };
        for y in 0..img.height {
            let one_d = k == 0 || y % (k as usize) == 0;
            put_prologue(&mut w, one_d);
            if one_d {
                put_row_1d(&mut w, img.row(y));
            } else {
                let reference = if y == 0 { &blank[..] } else { img.row(y - 1) };
                put_row_2d(&mut w, img.row(y), reference);
            }
        }
        if opts.rtc {
            for _ in 0..6 {
                put_eol(&mut w);
                if k > 0 {
                    w.put(1, 1);
                }
            }
        }
        w.finish()
    }
}

use encoder::{encode_g3, encode_g4, G3Options, Image};

// ---------------------------------------------------------------------------
// Images synthétiques
// ---------------------------------------------------------------------------

/// Générateur pseudo-aléatoire déterministe (LCG 64 bits).
pub(crate) struct Lcg(pub u64);

impl Lcg {
    pub(crate) fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
}

/// Bandes horizontales de 3 lignes.
pub(crate) fn stripes(width: usize, height: usize) -> Image {
    let mut img = Image::new(width, height);
    for y in 0..height {
        for x in 0..width {
            img.set(x, y, u8::from((y / 3) % 2 == 1));
        }
    }
    img
}

/// Damier de cases 5 × 5 (largeur volontairement non multiple de 8).
pub(crate) fn checkerboard(width: usize, height: usize) -> Image {
    let mut img = Image::new(width, height);
    for y in 0..height {
        for x in 0..width {
            img.set(x, y, u8::from((x / 5 + y / 5) % 2 == 1));
        }
    }
    img
}

/// « AK » dessiné à la main, 13 × 7, entouré d'une marge blanche d'un pixel.
pub(crate) fn glyphs() -> Image {
    let rows = [
        "..#....#...#.",
        ".#.#...#..#..",
        "#...#..#.#...",
        "#####..##....",
        "#...#..#.#...",
        "#...#..#..#..",
        "#...#..#...#.",
    ];
    let mut img = Image::new(15, 9);
    for (y, r) in rows.iter().enumerate() {
        for (x, c) in r.chars().enumerate() {
            img.set(x + 1, y + 1, u8::from(c == '#'));
        }
    }
    img
}

pub(crate) fn solid(width: usize, height: usize, black: bool) -> Image {
    let mut img = Image::new(width, height);
    img.pixels.iter_mut().for_each(|p| *p = u8::from(black));
    img
}

/// Bruit : chaque pixel a une chance sur `period` d'être noir, avec des
/// plages étendues pour couvrir les codes de composition.
pub(crate) fn noise(width: usize, height: usize, seed: u64) -> Image {
    let mut rng = Lcg(seed);
    let mut img = Image::new(width, height);
    let mut y = 0;
    while y < height {
        let mut x = 0;
        while x < width {
            let run = 1 + (rng.next() % 70) as usize;
            let black = rng.next() % 3 == 0;
            for i in x..(x + run).min(width) {
                img.set(i, y, u8::from(black));
            }
            x += run;
        }
        y += 1;
    }
    img
}

fn all_images() -> Vec<(&'static str, Image)> {
    vec![
        ("bandes", stripes(64, 12)),
        ("bandes larges", stripes(1728, 9)),
        ("damier", checkerboard(37, 11)),
        ("glyphes", glyphs()),
        ("blanc", solid(50, 5, false)),
        ("noir", solid(50, 5, true)),
        ("noir large", solid(3000, 3, true)),
        ("bruit", noise(300, 20, 1)),
        ("bruit étroit", noise(7, 30, 2)),
        ("une colonne", noise(1, 10, 3)),
    ]
}

fn params(k: i32, img: &Image) -> CcittParams {
    CcittParams {
        k,
        columns: img.width as u32,
        rows: img.height as u32,
        black_is_1: true,
        ..CcittParams::default()
    }
}

// ---------------------------------------------------------------------------
// Vecteurs connus
// ---------------------------------------------------------------------------

#[test]
fn tables_form_prefix_codes() {
    let (white, wc) = RunTable::build(&WHITE_TERMINAL, &WHITE_MAKEUP);
    let (black, bc) = RunTable::build(&BLACK_TERMINAL, &BLACK_MAKEUP);
    assert_eq!(wc, 0, "codes blancs en conflit");
    assert_eq!(bc, 0, "codes noirs en conflit");
    // Chaque code se décode vers sa plage.
    for (run, &(len, code)) in (0u16..).zip(WHITE_TERMINAL.iter()) {
        let entry = white.entries[usize::from(code) << (PEEK_BITS - u32::from(len))];
        assert_eq!((entry >> 12, entry & 0xFFF), (u16::from(len), run));
    }
    for (run, &(len, code)) in (0u16..).zip(BLACK_TERMINAL.iter()) {
        let entry = black.entries[usize::from(code) << (PEEK_BITS - u32::from(len))];
        assert_eq!((entry >> 12, entry & 0xFFF), (u16::from(len), run));
    }
}

#[test]
fn known_codes_from_t4_tables() {
    // Tableau 2/T.4 : blanc 2 = 0111, noir 2 = 11 ; tableau 3a/T.4 : blanc 64 = 11011.
    assert_eq!(WHITE_TERMINAL[2], (4, 0b0111));
    assert_eq!(BLACK_TERMINAL[2], (2, 0b11));
    assert_eq!(WHITE_MAKEUP[0], (5, 0b11011));
    // Blanc 0 = 00110101, noir 0 = 0000110111, blanc 1728 = 010011011.
    assert_eq!(WHITE_TERMINAL[0], (8, 0b0011_0101));
    assert_eq!(BLACK_TERMINAL[0], (10, 0b00_0011_0111));
    assert_eq!(WHITE_MAKEUP[26], (9, 0b0_1001_1011));
}

#[test]
fn one_d_blank_line_1728() {
    // Ligne blanche de 1728 pixels : composition 1728 (010011011) puis
    // terminaison 0 (00110101) → 0100 1101 1001 1010 1(000 0000).
    let data = [0x4D, 0x9A, 0x80];
    let params = CcittParams {
        rows: 1,
        black_is_1: true,
        ..CcittParams::default()
    };
    let out = decode(&data, &params).unwrap();
    assert_eq!(out, vec![0u8; 216]);
    let params = CcittParams {
        rows: 1,
        ..CcittParams::default()
    };
    assert_eq!(decode(&data, &params).unwrap(), vec![0xFF; 216]);
}

#[test]
fn hand_encoded_g4_vector() {
    // Image 8 × 2, lignes « ..###... » : ligne 1 en mode horizontal
    // (001 + blanc 2 « 0111 » + noir 3 « 10 ») puis V0 ; ligne 2 = trois V0 ;
    // EOFB. Bits : 0010111101 111 000000000001 000000000001.
    let data = [0x2F, 0x78, 0x00, 0x80, 0x08];
    let params = CcittParams {
        k: -1,
        columns: 8,
        rows: 2,
        black_is_1: true,
        ..CcittParams::default()
    };
    assert_eq!(
        decode(&data, &params).unwrap(),
        vec![0b0011_1000, 0b0011_1000]
    );
    // L'encodeur de test produit exactement ce flux.
    let mut img = Image::new(8, 2);
    for y in 0..2 {
        for x in 2..5 {
            img.set(x, y, 1);
        }
    }
    assert_eq!(encode_g4(&img, true), data.to_vec());
}

#[test]
fn default_params_follow_table_11() {
    let p = CcittParams::default();
    assert_eq!(p.k, 0);
    assert_eq!(p.columns, 1728);
    assert_eq!(p.rows, 0);
    assert!(!p.black_is_1);
    assert!(!p.byte_align);
    assert!(!p.end_of_line);
    assert!(p.end_of_block);
}

// ---------------------------------------------------------------------------
// Allers-retours
// ---------------------------------------------------------------------------

#[test]
fn g4_round_trips() {
    for (name, img) in all_images() {
        for eofb in [true, false] {
            let data = encode_g4(&img, eofb);
            let out = decode(&data, &params(-1, &img)).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(out, img.packed(), "{name} (eofb {eofb})");
        }
    }
}

#[test]
fn g4_byte_aligned_rows() {
    // EncodedByteAlign avec K < 0 : chaque ligne commence sur un octet.
    for (name, img) in all_images() {
        let mut w = encoder::BitWriter::default();
        let blank = vec![0u8; img.width];
        for y in 0..img.height {
            w.align();
            let reference = if y == 0 { &blank[..] } else { img.row(y - 1) };
            encoder::put_row_2d(&mut w, img.row(y), reference);
        }
        let data = w.finish();
        let p = CcittParams {
            byte_align: true,
            ..params(-1, &img)
        };
        assert_eq!(decode(&data, &p).unwrap(), img.packed(), "{name}");
    }
}

#[test]
fn g3_1d_round_trips_with_all_option_combinations() {
    for (name, img) in all_images() {
        for eol in [false, true] {
            for align in [false, true] {
                for rtc in [false, true] {
                    let opts = G3Options { eol, align, rtc };
                    let data = encode_g3(&img, 0, opts);
                    let p = CcittParams {
                        byte_align: align,
                        end_of_line: eol,
                        ..params(0, &img)
                    };
                    assert_eq!(decode(&data, &p).unwrap(), img.packed(), "{name} {opts:?}");
                }
            }
        }
    }
}

#[test]
fn g3_2d_round_trips() {
    for (name, img) in all_images() {
        for k in [1u32, 2, 4] {
            for align in [false, true] {
                let opts = G3Options {
                    eol: true,
                    align,
                    rtc: true,
                };
                let data = encode_g3(&img, k, opts);
                let p = CcittParams {
                    byte_align: align,
                    end_of_line: true,
                    ..params(k as i32, &img)
                };
                assert_eq!(
                    decode(&data, &p).unwrap(),
                    img.packed(),
                    "{name} K={k} {opts:?}"
                );
            }
        }
    }
}

#[test]
fn g3_2d_without_eol_still_decodes() {
    let img = glyphs();
    let data = encode_g3(&img, 4, G3Options::default());
    assert_eq!(decode(&data, &params(4, &img)).unwrap(), img.packed());
}

#[test]
fn rows_unknown_yields_decoded_height() {
    let img = checkerboard(37, 11);
    let data = encode_g4(&img, true);
    let p = CcittParams {
        rows: 0,
        ..params(-1, &img)
    };
    assert_eq!(decode(&data, &p).unwrap(), img.packed());
    // Sans EOFB, la fin des données termine aussi l'image.
    let data = encode_g4(&img, false);
    assert_eq!(decode(&data, &p).unwrap(), img.packed());
}

#[test]
fn black_is_1_false_inverts_output() {
    let img = glyphs();
    let data = encode_g4(&img, true);
    let p = CcittParams {
        black_is_1: false,
        ..params(-1, &img)
    };
    let expected: Vec<u8> = img.packed().iter().map(|b| !b).collect();
    assert_eq!(decode(&data, &p).unwrap(), expected);
}

#[test]
fn rows_larger_than_stream_pads_with_white() {
    let img = glyphs();
    let data = encode_g4(&img, true);
    let p = CcittParams {
        rows: 12,
        ..params(-1, &img)
    };
    let out = decode(&data, &p).unwrap();
    let stride = 2;
    assert_eq!(out.len(), 12 * stride);
    assert_eq!(&out[..9 * stride], &img.packed()[..]);
    assert!(out[9 * stride..].iter().all(|&b| b == 0));
}

#[test]
fn rows_smaller_than_stream_stops_early() {
    let img = stripes(64, 12);
    let data = encode_g4(&img, true);
    let p = CcittParams {
        rows: 5,
        ..params(-1, &img)
    };
    assert_eq!(decode(&data, &p).unwrap(), &img.packed()[..5 * 8]);
}

#[test]
fn runs_beyond_2560_use_repeated_makeups() {
    let img = solid(6000, 2, true);
    let data = encode_g3(&img, 0, G3Options::default());
    assert_eq!(decode(&data, &params(0, &img)).unwrap(), img.packed());
    let img = noise(9000, 3, 5);
    let data = encode_g4(&img, true);
    assert_eq!(decode(&data, &params(-1, &img)).unwrap(), img.packed());
}

// ---------------------------------------------------------------------------
// Tolérance et robustesse
// ---------------------------------------------------------------------------

#[test]
fn truncated_stream_returns_decoded_rows() {
    let img = stripes(64, 12);
    let data = encode_g4(&img, true);
    let p = params(-1, &img);
    // Coupé au milieu : les premières lignes sont intactes, le reste blanc.
    let out = decode(&data[..data.len() / 2], &p).unwrap();
    assert_eq!(out.len(), 12 * 8);
    let packed = img.packed();
    let intact = out
        .chunks(8)
        .zip(packed.chunks(8))
        .take_while(|(a, b)| a == b)
        .count();
    assert!(intact >= 2, "{intact} lignes intactes");
}

#[test]
fn every_truncation_length_never_panics() {
    let img = noise(300, 20, 1);
    let streams = [
        (encode_g4(&img, true), params(-1, &img)),
        (
            encode_g3(
                &img,
                0,
                G3Options {
                    eol: true,
                    align: false,
                    rtc: true,
                },
            ),
            params(0, &img),
        ),
        (
            encode_g3(
                &img,
                4,
                G3Options {
                    eol: true,
                    align: true,
                    rtc: true,
                },
            ),
            params(4, &img),
        ),
    ];
    for (data, p) in &streams {
        for len in 0..data.len() {
            if let Ok(out) = decode(&data[..len], p) {
                assert_eq!(out.len(), 20 * 38);
            }
        }
        let unknown_rows = CcittParams {
            rows: 0,
            ..p.clone()
        };
        for len in 0..data.len() {
            if let Ok(out) = decode(&data[..len], &unknown_rows) {
                assert_eq!(out.len() % 38, 0);
            }
        }
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Lcg(99);
    for i in 0..400 {
        let len = (rng.next() % 200) as usize;
        let data: Vec<u8> = (0..len).map(|_| (rng.next() & 0xFF) as u8).collect();
        let p = CcittParams {
            k: [-1, 0, 4][i % 3],
            columns: 1 + rng.next() % 300,
            rows: rng.next() % 40,
            byte_align: i % 5 == 0,
            ..CcittParams::default()
        };
        if let Ok(out) = decode(&data, &p) {
            let stride = (p.columns as usize).div_ceil(8);
            assert_eq!(out.len() % stride, 0);
            if p.rows > 0 {
                assert_eq!(out.len(), p.rows as usize * stride);
            }
        }
    }
}

#[test]
fn random_mutations_never_panic() {
    let img = noise(200, 16, 4);
    let base = encode_g4(&img, true);
    let p = params(-1, &img);
    let mut rng = Lcg(2024);
    for _ in 0..300 {
        let mut data = base.clone();
        for _ in 0..=(rng.next() % 4) {
            let i = (rng.next() as usize) % data.len();
            data[i] ^= 1 << (rng.next() % 8);
        }
        if let Ok(out) = decode(&data, &p) {
            assert_eq!(out.len(), 16 * 25);
        }
    }
}

#[test]
fn empty_and_invalid_inputs_are_errors() {
    assert!(matches!(
        decode(&[], &CcittParams::default()),
        Err(Error::Corrupt(_))
    ));
    let zero_columns = CcittParams {
        columns: 0,
        ..CcittParams::default()
    };
    assert!(matches!(
        decode(&[0xFF], &zero_columns),
        Err(Error::Corrupt(_))
    ));
    let huge = CcittParams {
        columns: MAX_COLUMNS,
        rows: u32::MAX,
        ..CcittParams::default()
    };
    assert!(matches!(decode(&[0xFF], &huge), Err(Error::Corrupt(_))));
}

#[test]
fn oversized_runs_are_clamped_to_width() {
    // Ligne 1-D de 8 pixels annonçant 64 + 5 blancs puis 3 noirs.
    let mut w = encoder::BitWriter::default();
    encoder::put_run(&mut w, 69, false);
    encoder::put_run(&mut w, 3, true);
    let data = w.finish();
    let p = CcittParams {
        columns: 8,
        rows: 1,
        black_is_1: true,
        ..CcittParams::default()
    };
    assert_eq!(decode(&data, &p).unwrap(), vec![0]);
}

#[test]
fn mmr_decoder_streams_rows_and_reports_consumption() {
    let img = glyphs();
    let data = encode_g4(&img, true);
    let mut mmr = MmrDecoder::new(&data, img.width as u32);
    for y in 0..img.height {
        let changes = mmr.next_row().expect("ligne");
        let mut row = vec![0u8; img.width];
        paint_changes(&mut row, changes);
        assert_eq!(row, img.row(y), "ligne {y}");
    }
    mmr.skip_eofb();
    assert_eq!(mmr.bytes_consumed(), data.len());
    assert!(mmr.next_row().is_none());
}

// ---------------------------------------------------------------------------
// Performance (à lancer en release : cargo test --release -p acrux-codecs -- --ignored ccitt)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "mesure de performance, à lancer en release"]
fn a4_300dpi_g4_decodes_quickly() {
    let (w, h) = (2480, 3508);
    // Texte simulé : glyphes « AK » répétés sur toute la page.
    let glyph = glyphs();
    let mut img = Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (gx, gy) = (x % 20, y % 14);
            if gx < glyph.width && gy < glyph.height {
                img.set(x, y, glyph.row(gy)[gx]);
            }
        }
    }
    let data = encode_g4(&img, true);
    let p = params(-1, &img);
    let start = std::time::Instant::now();
    let out = decode(&data, &p).unwrap();
    let elapsed = start.elapsed();
    println!(
        "A4 300 dpi G4 : {} octets décodés en {elapsed:?}",
        data.len()
    );
    assert_eq!(out, img.packed());
    let data = encode_g3(&img, 0, G3Options::default());
    let start = std::time::Instant::now();
    let out = decode(&data, &params(0, &img)).unwrap();
    println!(
        "A4 300 dpi G3 1-D : {} octets décodés en {:?}",
        data.len(),
        start.elapsed()
    );
    assert_eq!(out, img.packed());
    assert!(elapsed.as_secs_f64() < 1.0, "{elapsed:?}");
}

// ---------------------------------------------------------------------------
// Échantillons de corpus
// (cargo test -p acrux-codecs --release -- --ignored generate_corpus)
// ---------------------------------------------------------------------------

/// Outils partagés par les tests `generate_corpus_samples` des trois codecs
/// d'image (CCITT, JBIG2, JPX) : police 5 × 7, image bilevel reconnaissable,
/// répertoire de sortie `target/corpus-gen/` et écriture PNG (acrux-graphics,
/// dépendance de développement). Les octets produits sont assemblés en PDF
/// par `crates/acrux-render/tests/synthese_codecs.rs`.
pub(crate) mod corpus {
    use super::encoder::Image;
    use std::path::PathBuf;

    /// Largeur des échantillons, en pixels.
    pub(crate) const SAMPLE_WIDTH: usize = 200;
    /// Hauteur des échantillons, en pixels.
    pub(crate) const SAMPLE_HEIGHT: usize = 120;

    /// Glyphe 5 × 7 d'un caractère (majuscules, chiffres, tiret ; tout
    /// autre caractère est une espace).
    pub(crate) fn glyph(c: char) -> [&'static str; 7] {
        match c {
            'A' => [
                ".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#",
            ],
            'B' => [
                "####.", "#...#", "#...#", "####.", "#...#", "#...#", "####.",
            ],
            'C' => [
                ".####", "#....", "#....", "#....", "#....", "#....", ".####",
            ],
            'D' => [
                "####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####.",
            ],
            'E' => [
                "#####", "#....", "#....", "####.", "#....", "#....", "#####",
            ],
            'F' => [
                "#####", "#....", "#....", "####.", "#....", "#....", "#....",
            ],
            'G' => [
                ".####", "#....", "#....", "#.###", "#...#", "#...#", ".####",
            ],
            'H' => [
                "#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#",
            ],
            'I' => [
                "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "#####",
            ],
            'J' => [
                "..###", "...#.", "...#.", "...#.", "...#.", "#..#.", ".##..",
            ],
            'K' => [
                "#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#",
            ],
            'L' => [
                "#....", "#....", "#....", "#....", "#....", "#....", "#####",
            ],
            'M' => [
                "#...#", "##.##", "#.#.#", "#...#", "#...#", "#...#", "#...#",
            ],
            'N' => [
                "#...#", "##..#", "#.#.#", "#..##", "#...#", "#...#", "#...#",
            ],
            'O' => [
                ".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
            ],
            'P' => [
                "####.", "#...#", "#...#", "####.", "#....", "#....", "#....",
            ],
            'R' => [
                "####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#",
            ],
            'S' => [
                ".####", "#....", "#....", ".###.", "....#", "....#", "####.",
            ],
            'T' => [
                "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#..",
            ],
            'U' => [
                "#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
            ],
            'V' => [
                "#...#", "#...#", "#...#", "#...#", "#...#", ".#.#.", "..#..",
            ],
            'X' => [
                "#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#",
            ],
            'Y' => [
                "#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#..",
            ],
            '0' => [
                ".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###.",
            ],
            '1' => [
                "..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###.",
            ],
            '2' => [
                ".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####",
            ],
            '3' => [
                "#####", "...#.", "..#..", "...#.", "....#", "#...#", ".###.",
            ],
            '4' => [
                "...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#.",
            ],
            '5' => [
                "#####", "#....", "####.", "....#", "....#", "#...#", ".###.",
            ],
            '-' => [
                ".....", ".....", ".....", "#####", ".....", ".....", ".....",
            ],
            _ => ["....."; 7],
        }
    }

    /// Dessine `text` (1 = noir) avec le coin supérieur gauche en `(x, y)`,
    /// chaque pixel de glyphe agrandi `scale` fois, une colonne de glyphe
    /// d'espacement entre les caractères (avance de 6 × `scale`).
    pub(crate) fn draw_text(img: &mut Image, x: usize, y: usize, text: &str, scale: usize) {
        for (i, c) in text.chars().enumerate() {
            for (gy, row) in glyph(c).iter().enumerate() {
                for (gx, ch) in row.chars().enumerate() {
                    if ch != '#' {
                        continue;
                    }
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let px = x + (i * 6 + gx) * scale + dx;
                            let py = y + gy * scale + dy;
                            if px < img.width && py < img.height {
                                img.set(px, py, 1);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Image bilevel 200 × 120 reconnaissable (1 = noir) : cadre de deux
    /// pixels, `label` (au plus dix caractères, glyphes 15 × 21) en haut,
    /// puis un disque plein, un triangle plein et un anneau.
    pub(crate) fn bilevel_sample(label: &str) -> Image {
        let mut img = Image::new(SAMPLE_WIDTH, SAMPLE_HEIGHT);
        for y in 0..SAMPLE_HEIGHT {
            for x in 0..SAMPLE_WIDTH {
                let frame = x < 2 || y < 2 || x >= SAMPLE_WIDTH - 2 || y >= SAMPLE_HEIGHT - 2;
                let (fx, fy) = (x as i64, y as i64);
                let disc = (fx - 40).pow(2) + (fy - 80).pow(2) <= 26 * 26;
                let ring = {
                    let d = (fx - 160).pow(2) + (fy - 80).pow(2);
                    (14 * 14..=26 * 26).contains(&d)
                };
                // Sommet (100, 44), base y = 108 de demi-largeur 30.
                let triangle = (44..=108).contains(&fy) && (fx - 100).abs() * 64 <= (fy - 44) * 30;
                if frame || disc || ring || triangle {
                    img.set(x, y, 1);
                }
            }
        }
        draw_text(&mut img, 8, 8, label, 3);
        img
    }

    /// `target/corpus-gen/` à la racine du dépôt (créé au besoin).
    pub(crate) fn output_dir() -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("corpus-gen");
        std::fs::create_dir_all(&dir).expect("création de target/corpus-gen");
        dir
    }

    /// Écrit un fichier dans le répertoire de sortie.
    pub(crate) fn write(name: &str, bytes: &[u8]) {
        let path = output_dir().join(name);
        std::fs::write(&path, bytes).unwrap_or_else(|e| panic!("{} : {e}", path.display()));
        println!("écrit {} ({} octets)", path.display(), bytes.len());
    }

    /// Écrit une image RVB 8 bits en PNG.
    pub(crate) fn write_png_rgb(name: &str, width: u32, height: u32, rgb: &[u8]) {
        write(name, &acrux_graphics::encode_png_rgb(width, height, rgb));
    }

    /// Écrit une image bilevel en PNG (1 = noir → 0, 0 = blanc → 255).
    pub(crate) fn write_png_bilevel(name: &str, img: &Image) {
        let rgb: Vec<u8> = img
            .pixels
            .iter()
            .flat_map(|&p| {
                let v = if p == 0 { 255 } else { 0 };
                [v, v, v]
            })
            .collect();
        write_png_rgb(name, img.width as u32, img.height as u32, &rgb);
    }
}

/// Écrit dans `target/corpus-gen/` les flux CCITT du corpus de synthèse et
/// leurs images source ; les paramètres `/DecodeParms` correspondants sont
/// déclarés dans `crates/acrux-render/tests/synthese_codecs.rs`.
#[test]
#[ignore = "génération du corpus : cargo test -p acrux-codecs --release -- --ignored generate_corpus"]
fn generate_corpus_samples() {
    let g4 = corpus::bilevel_sample("G4");
    let g3_1d = corpus::bilevel_sample("G3 1D");
    let g3_2d = corpus::bilevel_sample("G3 2D");
    let black = corpus::bilevel_sample("BLACK IS 1");
    // (nom, image, flux, paramètres avec lesquels le PDF sera lu)
    let samples: [(&str, &Image, Vec<u8>, CcittParams); 4] = [
        (
            "ccitt-g4",
            &g4,
            encode_g4(&g4, true),
            CcittParams {
                black_is_1: false,
                ..params(-1, &g4)
            },
        ),
        (
            // 1-D pur : ni EOL, ni RTC, lignes non alignées.
            "ccitt-g3-1d",
            &g3_1d,
            encode_g3(&g3_1d, 0, G3Options::default()),
            CcittParams {
                black_is_1: false,
                ..params(0, &g3_1d)
            },
        ),
        (
            // Mixte K = 4 : EOL + bit de mode, bits de remplissage avant
            // chaque EOL (EncodedByteAlign), RTC final.
            "ccitt-g3-2d-align",
            &g3_2d,
            encode_g3(
                &g3_2d,
                4,
                G3Options {
                    eol: true,
                    align: true,
                    rtc: true,
                },
            ),
            CcittParams {
                black_is_1: false,
                byte_align: true,
                end_of_line: true,
                ..params(4, &g3_2d)
            },
        ),
        (
            // BlackIs1 : le filtre livre 1 = noir, le PDF ajoute /Decode [1 0].
            "ccitt-g4-blackis1",
            &black,
            encode_g4(&black, true),
            params(-1, &black),
        ),
    ];
    for (name, img, data, p) in &samples {
        // Le flux doit se décoder exactement avec les paramètres déclarés.
        let decoded = decode(data, p).unwrap_or_else(|e| panic!("{name} : {e}"));
        let expected: Vec<u8> = if p.black_is_1 {
            img.packed()
        } else {
            img.packed().iter().map(|b| !b).collect()
        };
        assert_eq!(decoded, expected, "{name}");
        corpus::write(&format!("{name}.bin"), data);
        corpus::write_png_bilevel(&format!("{name}.png"), img);
    }
}
