//! Tests du décodeur JPXDecode.
//!
//! - `encoder` : encodeur JPEG 2000 minimal écrit ici d'après 15444-1
//!   (annexes B, C, D, F, G, I) pour produire des flux dont on connaît les
//!   pixels ;
//! - aller-retour sans perte (5-3 + RCT) sur des images synthétiques, avec
//!   tuiles, précincts, modes de bloc, couches, progressions, POC, PPM/PPT,
//!   ROI, COC, sous-échantillonnage, 12 bits, JP2 (palette, cdef, sYCC) ;
//! - aller-retour 9-7 + ICT à ±3 niveaux ;
//! - robustesse : troncatures à toutes les longueurs, mutations aléatoires,
//!   jamais de panique.
//!
//! Aucun outil de la machine ne sait produire un vrai .jp2 (WIC et
//! PowerShell ne le font pas) : la conformité est vérifiée par aller-retour
//! et par les tests de composants (vecteur MQ de l'annexe H.2 de T.88,
//! filtres contre leurs gains nominaux, arbres d'étiquettes, tables D.1).

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
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::if_not_else,
    clippy::manual_let_else,
    clippy::trivially_copy_pass_by_ref
)]

use super::codestream::{PocEntry, Progression};
use super::tier1::cbstyle;
use super::*;
use encoder::{Component, Image, Packed, Params};

mod encoder;

// ---------------------------------------------------------------------------
// Générateurs d'images
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }
}

fn gray_image(w: u32, h: u32, f: impl Fn(u32, u32) -> i32) -> Image {
    let f = &f;
    let data = (0..h).flat_map(|y| (0..w).map(move |x| f(x, y))).collect();
    Image {
        x0: 0,
        y0: 0,
        x1: w,
        y1: h,
        components: vec![Component {
            precision: 8,
            signed: false,
            xr: 1,
            yr: 1,
            data,
        }],
    }
}

fn rgb_image(w: u32, h: u32, f: impl Fn(u32, u32) -> [i32; 3]) -> Image {
    let mut comps = Vec::new();
    let f = &f;
    for c in 0..3 {
        let data = (0..h)
            .flat_map(|y| (0..w).map(move |x| f(x, y)[c]))
            .collect();
        comps.push(Component {
            precision: 8,
            signed: false,
            xr: 1,
            yr: 1,
            data,
        });
    }
    Image {
        x0: 0,
        y0: 0,
        x1: w,
        y1: h,
        components: comps,
    }
}

fn gradient(w: u32, h: u32) -> Image {
    gray_image(w, h, |x, y| ((x * 255 / w.max(1)) + (y * 7)) as i32 % 256)
}

fn checker(w: u32, h: u32, size: u32) -> Image {
    gray_image(w, h, |x, y| {
        if (x / size + y / size) % 2 == 0 {
            30
        } else {
            220
        }
    })
}

fn noise(w: u32, h: u32, seed: u64) -> Image {
    let mut rng = Rng(seed | 1);
    let data: Vec<i32> = (0..w * h).map(|_| (rng.next() % 256) as i32).collect();
    let mut img = gray_image(w, h, |_, _| 0);
    img.components[0].data = data;
    img
}

fn rgb_smooth(w: u32, h: u32) -> Image {
    rgb_image(w, h, |x, y| {
        [
            (x * 255 / w.max(1)) as i32,
            (y * 255 / h.max(1)) as i32,
            (((x + y) * 128 / (w + h).max(1)) + 60) as i32,
        ]
    })
}

fn rgb_noise(w: u32, h: u32, seed: u64) -> Image {
    let mut rng = Rng(seed | 1);
    let mut img = rgb_image(w, h, |_, _| [0; 3]);
    for c in &mut img.components {
        c.data = (0..w * h).map(|_| (rng.next() % 256) as i32).collect();
    }
    img
}

/// Pixels attendus (8 bits entrelacés) d'une image d'entrée.
fn expected_pixels(img: &Image) -> Vec<u8> {
    let w = (img.x1 - img.x0) as usize;
    let h = (img.y1 - img.y0) as usize;
    let n = img.components.len();
    let mut out = vec![0u8; w * h * n];
    for (c, comp) in img.components.iter().enumerate() {
        assert_eq!(comp.xr, 1);
        let p = comp.precision;
        for i in 0..w * h {
            let v = comp.data[i] + if comp.signed { 1 << (p - 1) } else { 0 };
            let v8 = if p >= 8 {
                v >> (p - 8)
            } else {
                (v * 255 + ((1 << p) - 1) / 2) / ((1 << p) - 1)
            };
            out[i * n + c] = v8 as u8;
        }
    }
    out
}

fn max_diff(a: &[u8], b: &[u8]) -> u32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(&x, &y)| u32::from(x.abs_diff(y)))
        .max()
        .unwrap_or(0)
}

fn round_trip(img: &Image, p: &Params, tol: u32) -> JpxImage {
    let bytes = encoder::encode(img, p);
    let decoded = decode(&bytes).expect("décodage");
    assert_eq!(decoded.width, img.x1 - img.x0);
    assert_eq!(decoded.height, img.y1 - img.y0);
    assert_eq!(usize::from(decoded.components), img.components.len());
    let expected = expected_pixels(img);
    let diff = max_diff(&decoded.data, &expected);
    assert!(diff <= tol, "écart maximal {diff} > {tol}");
    decoded
}

fn lossless(img: &Image, p: &Params) -> JpxImage {
    round_trip(img, p, 0)
}

// ---------------------------------------------------------------------------
// Composants : MQ, ondelettes, arbres
// ---------------------------------------------------------------------------

#[test]
fn mq_encoder_reproduces_the_standard_test_vector() {
    // T.88 annexe H.2 (identique dans 15444-1) : 256 bits → 30 octets.
    let input: [u8; 32] = [
        0x00, 0x02, 0x00, 0x51, 0x00, 0x00, 0x00, 0xC0, 0x03, 0x52, 0x87, 0x2A, 0xAA, 0xAA, 0xAA,
        0xAA, 0x82, 0xC0, 0x20, 0x00, 0xFC, 0xD7, 0x9E, 0xF6, 0xBF, 0x7F, 0xED, 0x90, 0x4F, 0x46,
        0xA3, 0xBF,
    ];
    let expected: [u8; 30] = [
        0x84, 0xC7, 0x3B, 0xFC, 0xE1, 0xA1, 0x43, 0x04, 0x02, 0x20, 0x00, 0x00, 0x41, 0x0D, 0xBB,
        0x86, 0xF4, 0x31, 0x7F, 0xFF, 0x88, 0xFF, 0x37, 0x47, 0x1A, 0xDB, 0x6A, 0xDF, 0xFF, 0xAC,
    ];
    let mut enc = encoder::MqEncoder::new();
    enc.cx[0] = super::mq::Context::new(0, 0);
    for byte in input {
        for i in (0..8).rev() {
            enc.encode(0, u32::from((byte >> i) & 1));
        }
    }
    let out = enc.flush();
    // Notre FLUSH omet le marqueur FF AC final.
    assert_eq!(out, &expected[..28]);
    let mut dec = super::mq::MqDecoder::new(&out);
    let mut cx = super::mq::Context::new(0, 0);
    for byte in input {
        for i in (0..8).rev() {
            assert_eq!(dec.decode(&mut cx), u32::from((byte >> i) & 1));
        }
    }
}

#[test]
fn mq_round_trip_random_bits_many_contexts() {
    let mut rng = Rng(7);
    let bits: Vec<(usize, u32)> = (0..5000)
        .map(|_| {
            let ctx = (rng.next() % 19) as usize;
            let bit = u32::from(rng.next() % 7 == 0);
            (ctx, bit)
        })
        .collect();
    let mut enc = encoder::MqEncoder::new();
    for &(ctx, b) in &bits {
        enc.encode(ctx, b);
    }
    let data = enc.flush();
    let mut dec = super::mq::MqDecoder::new(&data);
    let mut cx = super::tier1::initial_contexts();
    for &(ctx, b) in &bits {
        assert_eq!(dec.decode(&mut cx[ctx]), b);
    }
}

#[test]
fn tag_tree_encoder_and_decoder_agree() {
    let mut rng = Rng(99);
    let (w, h) = (5usize, 3usize);
    let leaves: Vec<u32> = (0..w * h).map(|_| rng.next() % 4).collect();
    let mut enc = encoder::TagTreeEncoder::new(w, h, &leaves);
    let mut wr = encoder::BitWriter::new();
    // Comme l'inclusion : seuils croissants, feuille par feuille.
    let mut order = Vec::new();
    for t in 1..=4u32 {
        for y in 0..h {
            for x in 0..w {
                let r = enc.encode(&mut wr, x, y, t);
                order.push(r);
            }
        }
    }
    let bytes = wr.finish();
    let mut dec = super::tier2::TagTree::new(w as u32, h as u32);
    let mut rd = super::tier2::BitReader::new(&bytes, 0);
    let mut k = 0;
    for t in 1..=4u32 {
        for y in 0..h {
            for x in 0..w {
                let r = dec.decode(&mut rd, x as u32, y as u32, t);
                assert_eq!(r, order[k], "t={t} x={x} y={y}");
                if r {
                    assert_eq!(dec.value(x as u32, y as u32), leaves[y * w + x]);
                }
                k += 1;
            }
        }
    }
    assert!(!rd.overrun);
}

// ---------------------------------------------------------------------------
// Aller-retour sans perte (5-3 + RCT)
// ---------------------------------------------------------------------------

#[test]
fn gray_gradient_checker_noise_lossless() {
    let p = Params::default();
    lossless(&gradient(64, 48), &p);
    lossless(&checker(64, 48, 5), &p);
    lossless(&noise(64, 48, 3), &p);
}

#[test]
fn odd_sizes_and_tiny_images_lossless() {
    for (w, h) in [
        (37, 29),
        (1, 1),
        (2, 1),
        (1, 2),
        (3, 3),
        (5, 2),
        (17, 1),
        (1, 9),
    ] {
        for levels in 0..=3u8 {
            let p = Params {
                levels,
                ..Params::default()
            };
            lossless(&noise(w, h, u64::from(w * h + u32::from(levels))), &p);
        }
    }
}

#[test]
fn decomposition_levels_zero_to_five_lossless() {
    for levels in 0..=5u8 {
        let p = Params {
            levels,
            ..Params::default()
        };
        lossless(&gradient(70, 45), &p);
        lossless(&noise(70, 45, 11), &p);
    }
}

#[test]
fn rgb_with_rct_lossless() {
    let p = Params::default();
    lossless(&rgb_smooth(50, 40), &p);
    lossless(&rgb_noise(50, 40, 5), &p);
    let no_mct = Params {
        mct: false,
        ..Params::default()
    };
    lossless(&rgb_noise(33, 21, 6), &no_mct);
}

#[test]
fn code_block_sizes_32_and_64_and_small() {
    for (xcb, ycb) in [(5, 5), (6, 6), (4, 4), (2, 2), (3, 6), (6, 3)] {
        let p = Params {
            xcb,
            ycb,
            levels: 2,
            ..Params::default()
        };
        lossless(&noise(90, 70, 21), &p);
    }
}

#[test]
fn multiple_tiles_lossless() {
    let p = Params {
        tile: (32, 32),
        levels: 3,
        ..Params::default()
    };
    lossless(&rgb_noise(100, 75, 8), &p);
    let p2 = Params {
        tile: (16, 24),
        levels: 1,
        ..Params::default()
    };
    lossless(&gradient(50, 50), &p2);
}

#[test]
fn nonzero_image_origin_exercises_odd_parities() {
    for (x0, y0) in [(1, 0), (0, 1), (3, 2), (5, 7)] {
        let mut img = noise(40, 30, 13);
        img.x0 = x0;
        img.y0 = y0;
        img.x1 = 40 + x0;
        img.y1 = 30 + y0;
        let p = Params {
            levels: 3,
            tile: (32, 32),
            ..Params::default()
        };
        lossless(&img, &p);
    }
}

#[test]
fn twelve_bit_precision_is_reduced_to_eight_bits() {
    let mut rng = Rng(77);
    let mut img = gray_image(40, 30, |_, _| 0);
    img.components[0].precision = 12;
    img.components[0].data = (0..1200).map(|_| (rng.next() % 4096) as i32).collect();
    lossless(&img, &Params::default());
    let mut img16 = img.clone();
    img16.components[0].precision = 16;
    img16.components[0].data = (0..1200).map(|_| (rng.next() % 65536) as i32).collect();
    lossless(&img16, &Params::default());
}

#[test]
fn low_precision_and_signed_components() {
    let mut rng = Rng(5);
    let mut img = gray_image(30, 20, |_, _| 0);
    img.components[0].precision = 4;
    img.components[0].data = (0..600).map(|_| (rng.next() % 16) as i32).collect();
    lossless(&img, &Params::default());
    img.components[0].precision = 1;
    img.components[0].data = (0..600).map(|_| (rng.next() % 2) as i32).collect();
    lossless(&img, &Params::default());
    let mut signed = gray_image(30, 20, |_, _| 0);
    signed.components[0].signed = true;
    signed.components[0].data = (0..600).map(|_| (rng.next() % 256) as i32 - 128).collect();
    lossless(&signed, &Params::default());
}

#[test]
fn block_coding_modes_lossless() {
    let modes = [
        cbstyle::BYPASS,
        cbstyle::RESET,
        cbstyle::TERMALL,
        cbstyle::CAUSAL,
        cbstyle::SEGSYM,
        cbstyle::BYPASS | cbstyle::TERMALL,
        cbstyle::BYPASS | cbstyle::RESET | cbstyle::CAUSAL | cbstyle::SEGSYM,
        cbstyle::BYPASS
            | cbstyle::RESET
            | cbstyle::TERMALL
            | cbstyle::CAUSAL
            | 0x10
            | cbstyle::SEGSYM,
    ];
    for style in modes {
        let p = Params {
            cbstyle: style,
            levels: 2,
            ..Params::default()
        };
        lossless(&noise(70, 50, 31 + u64::from(style)), &p);
        lossless(&rgb_smooth(41, 37), &p);
    }
}

#[test]
fn custom_precincts_and_all_progressions() {
    for progression in [
        Progression::Lrcp,
        Progression::Rlcp,
        Progression::Rpcl,
        Progression::Pcrl,
        Progression::Cprl,
    ] {
        let p = Params {
            progression,
            levels: 3,
            precincts: Some(vec![(3, 3), (4, 4), (5, 4), (5, 5)]),
            xcb: 4,
            ycb: 4,
            ..Params::default()
        };
        lossless(&rgb_noise(75, 60, 9), &p);
        let p2 = Params {
            progression,
            levels: 2,
            tile: (40, 40),
            ..Params::default()
        };
        lossless(&rgb_smooth(90, 70), &p2);
    }
}

#[test]
fn multiple_layers_with_termall_lossless() {
    for layers in [2u16, 3, 5] {
        let p = Params {
            layers,
            cbstyle: cbstyle::TERMALL,
            levels: 2,
            ..Params::default()
        };
        lossless(&noise(60, 45, 17), &p);
        let p_bypass = Params {
            layers,
            cbstyle: cbstyle::TERMALL | cbstyle::BYPASS,
            progression: Progression::Rlcp,
            ..Params::default()
        };
        lossless(&rgb_noise(45, 45, 18), &p_bypass);
    }
}

#[test]
fn sop_eph_tile_parts_comment_and_plt_are_handled() {
    let p = Params {
        sop: true,
        eph: true,
        tile_parts: 3,
        comment: true,
        levels: 2,
        tile: (48, 48),
        ..Params::default()
    };
    lossless(&rgb_noise(80, 60, 41), &p);
}

#[test]
fn packed_packet_headers_ppt_and_ppm() {
    for packed in [Packed::Ppt, Packed::Ppm] {
        let p = Params {
            packed,
            eph: true,
            tile: (40, 40),
            tile_parts: 2,
            levels: 2,
            ..Params::default()
        };
        lossless(&rgb_noise(90, 70, 42), &p);
        let p2 = Params {
            packed,
            layers: 3,
            cbstyle: cbstyle::TERMALL,
            ..Params::default()
        };
        lossless(&gradient(50, 33), &p2);
    }
}

#[test]
fn progression_order_changes_poc() {
    let p = Params {
        levels: 3,
        layers: 2,
        cbstyle: cbstyle::TERMALL,
        poc: vec![
            PocEntry {
                res_start: 0,
                comp_start: 0,
                layer_end: 1,
                res_end: 2,
                comp_end: 3,
                progression: Progression::Rpcl,
            },
            PocEntry {
                res_start: 0,
                comp_start: 0,
                layer_end: 2,
                res_end: 4,
                comp_end: 2,
                progression: Progression::Cprl,
            },
        ],
        progression: Progression::Lrcp,
        ..Params::default()
    };
    lossless(&rgb_noise(60, 50, 43), &p);
}

#[test]
fn roi_maxshift_is_undone() {
    let p = Params {
        roi_shift: 11,
        levels: 2,
        ..Params::default()
    };
    lossless(&noise(50, 40, 44), &p);
    lossless(&rgb_smooth(50, 40), &p);
}

#[test]
fn coc_gives_component_one_a_different_decomposition() {
    for lv in [0u8, 1, 4] {
        let p = Params {
            levels: 2,
            coc_levels_c1: Some(lv),
            mct: false,
            ..Params::default()
        };
        lossless(&rgb_noise(40, 36, 45), &p);
    }
}

#[test]
fn subsampled_chroma_components_are_replicated() {
    // Composante 0 pleine résolution, composantes 1 et 2 à XRsiz = YRsiz = 2.
    let (w, h) = (41u32, 27u32);
    let mut rng = Rng(46);
    let mut img = gray_image(w, h, |_, _| 0);
    img.components[0].data = (0..w * h).map(|_| (rng.next() % 256) as i32).collect();
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    for _ in 0..2 {
        img.components.push(Component {
            precision: 8,
            signed: false,
            xr: 2,
            yr: 2,
            data: (0..cw * ch).map(|_| (rng.next() % 256) as i32).collect(),
        });
    }
    let p = Params {
        mct: false,
        levels: 2,
        ..Params::default()
    };
    let decoded = decode(&encoder::encode(&img, &p)).unwrap();
    assert_eq!(
        (decoded.width, decoded.height, decoded.components),
        (w, h, 3)
    );
    for y in 0..h {
        for x in 0..w {
            let px = &decoded.data[((y * w + x) * 3) as usize..][..3];
            assert_eq!(px[0], img.components[0].data[(y * w + x) as usize] as u8);
            let sx = (x as usize * cw as usize) / w as usize;
            let sy = (y as usize * ch as usize) / h as usize;
            assert_eq!(px[1], img.components[1].data[sy * cw as usize + sx] as u8);
            assert_eq!(px[2], img.components[2].data[sy * cw as usize + sx] as u8);
        }
    }
    assert_eq!(read_header(&encoder::encode(&img, &p)).unwrap(), (w, h, 3));
}

// ---------------------------------------------------------------------------
// 9-7 irréversible + ICT
// ---------------------------------------------------------------------------

#[test]
fn irreversible_97_with_ict_is_close() {
    for levels in [1u8, 2, 3, 5] {
        let p = Params {
            reversible: false,
            levels,
            quant_shift: 2,
            ..Params::default()
        };
        round_trip(&rgb_smooth(64, 48), &p, 3);
        round_trip(&gradient(37, 29), &p, 3);
    }
    let p = Params {
        reversible: false,
        levels: 2,
        quant_shift: 3,
        cbstyle: cbstyle::BYPASS | cbstyle::TERMALL,
        tile: (32, 32),
        ..Params::default()
    };
    round_trip(&rgb_noise(50, 40, 47), &p, 3);
}

#[test]
fn coarse_quantization_stays_within_step_bound() {
    // Δ = 2 → erreur de reconstruction bornée mais visible.
    let p = Params {
        reversible: false,
        levels: 2,
        quant_shift: 0,
        mct: false,
        ..Params::default()
    };
    let bytes = encoder::encode(&gradient(48, 40), &p);
    let expected = expected_pixels(&gradient(48, 40));
    let decoded = decode(&bytes).unwrap();
    assert!(max_diff(&decoded.data, &expected) <= 8);
}

// ---------------------------------------------------------------------------
// Options et conteneur JP2
// ---------------------------------------------------------------------------

#[test]
fn reduced_resolution_decoding_halves_dimensions() {
    // Image lisse (sans rebouclage) : le passe-bas 5-3 en garde la moyenne locale.
    let img = gray_image(64, 48, |x, y| (x * 2 + y * 2) as i32);
    let p = Params {
        levels: 3,
        ..Params::default()
    };
    let bytes = encoder::encode(&img, &p);
    for reduce in 1..=3u8 {
        let r = decode_with_options(
            &bytes,
            &JpxOptions {
                max_resolution_reduction: reduce,
                smask_in_data: false,
            },
        )
        .unwrap();
        assert_eq!(r.width, 64 >> reduce);
        assert_eq!(r.height, 48 >> reduce);
        // Le passe-bas 5-3 conserve la moyenne locale : proche de l'original
        // sous-échantillonné.
        let expected = expected_pixels(&img);
        let mut worst = 0u32;
        for y in 0..r.height {
            for x in 0..r.width {
                let o = expected[((y << reduce) * 64 + (x << reduce)) as usize];
                let d = r.data[(y * r.width + x) as usize];
                worst = worst.max(u32::from(o.abs_diff(d)));
            }
        }
        assert!(worst <= 8, "réduction {reduce} : écart {worst}");
    }
    // Une réduction supérieure au nombre de niveaux est bornée.
    let r = decode_with_options(
        &bytes,
        &JpxOptions {
            max_resolution_reduction: 9,
            smask_in_data: false,
        },
    )
    .unwrap();
    assert_eq!((r.width, r.height), (8, 6));
}

#[test]
fn jp2_enumerated_colourspaces_are_reported() {
    for (cs, expected) in [
        (16, JpxColorSpace::Srgb),
        (17, JpxColorSpace::Gray),
        (12, JpxColorSpace::Cmyk),
        (20, JpxColorSpace::Unknown),
    ] {
        let p = Params {
            jp2: Some((cs, Vec::new())),
            ..Params::default()
        };
        let d = lossless(&noise(20, 12, 48), &p);
        assert_eq!(d.colorspace, expected);
        assert!(!d.has_alpha);
        assert!(!d.palette_applied);
    }
    let raw = lossless(&noise(20, 12, 48), &Params::default());
    assert_eq!(raw.colorspace, JpxColorSpace::Unknown);
}

#[test]
fn jp2_sycc_is_converted_to_rgb() {
    let img = rgb_image(24, 16, |x, y| [128, (x * 10) as i32, (y * 15) as i32]);
    let p = Params {
        jp2: Some((18, Vec::new())),
        mct: false,
        ..Params::default()
    };
    let d = decode(&encoder::encode(&img, &p)).unwrap();
    assert_eq!(d.colorspace, JpxColorSpace::Sycc);
    let px = &d.data[..3];
    // Y = 128, Cb = 0, Cr = 0 → R = 128 + 1.402·(−128) ≈ 0 saturé.
    assert_eq!(px[0], 0);
    assert!(px[1] > 250);
    assert_eq!(px[2], 0);
}

#[test]
fn jp2_icc_profile_bytes_are_exposed() {
    let mut colr = vec![2, 0, 0];
    colr.extend_from_slice(b"fake-icc-profile");
    let extra = encoder::make_box(b"colr", &colr);
    let img = noise(16, 16, 49);
    let p = Params {
        jp2: Some((16, extra)),
        ..Params::default()
    };
    // La première boîte colr (énumérée 16) gagne : on encode donc avec la
    // boîte ICC placée via une seconde image sans colr énuméré.
    let bytes = encoder::encode(&img, &p);
    let d = decode(&bytes).unwrap();
    assert_eq!(d.colorspace, JpxColorSpace::Srgb);
    // Fichier construit à la main avec colr ICC seul.
    let cs_start = bytes.windows(4).position(|w| w == b"jp2c").unwrap() - 4;
    let codestream = &bytes[cs_start + 8..];
    let mut file = bytes[..12].to_vec();
    file.extend(encoder::make_box(b"ftyp", b"jp2 \0\0\0\0jp2 "));
    let mut jp2h = encoder::make_box(b"ihdr", &[0; 14]);
    jp2h.extend(encoder::make_box(b"colr", &colr));
    file.extend(encoder::make_box(b"jp2h", &jp2h));
    file.extend(encoder::make_box(b"jp2c", codestream));
    let d2 = decode(&file).unwrap();
    assert_eq!(
        d2.colorspace,
        JpxColorSpace::Icc(b"fake-icc-profile".to_vec())
    );
    assert_eq!(d2.data, d.data);
}

#[test]
fn jp2_palette_is_applied_through_cmap() {
    // Image d'indices 0..3 → palette RVB 8 bits.
    let img = gray_image(20, 10, |x, y| ((x + y) % 4) as i32);
    let mut pclr = vec![0, 4, 3, 7, 7, 7];
    let table: [[u8; 3]; 4] = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [10, 20, 30]];
    for e in table {
        pclr.extend_from_slice(&e);
    }
    let cmap = [0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 2];
    let mut extra = encoder::make_box(b"pclr", &pclr);
    extra.extend(encoder::make_box(b"cmap", &cmap));
    let p = Params {
        jp2: Some((16, extra)),
        ..Params::default()
    };
    let bytes = encoder::encode(&img, &p);
    assert_eq!(read_header(&bytes).unwrap(), (20, 10, 3));
    let d = decode(&bytes).unwrap();
    assert!(d.palette_applied);
    assert_eq!(d.components, 3);
    for y in 0..10u32 {
        for x in 0..20u32 {
            let i = ((x + y) % 4) as usize;
            assert_eq!(&d.data[((y * 20 + x) * 3) as usize..][..3], &table[i]);
        }
    }
}

#[test]
fn jp2_cdef_alpha_is_kept_only_with_smask_in_data() {
    // 4 composantes : R, V, B, alpha (cdef : canal 3 = opacité).
    let mut img = rgb_noise(20, 12, 50);
    img.components.push(Component {
        precision: 8,
        signed: false,
        xr: 1,
        yr: 1,
        data: (0..240i32).map(|i| i * 255 / 239).collect(),
    });
    let cdef = [
        0, 4, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 2, 0, 2, 0, 0, 0, 3, 0, 3, 0, 1, 0, 0,
    ];
    let extra = encoder::make_box(b"cdef", &cdef);
    let p = Params {
        jp2: Some((16, extra)),
        ..Params::default()
    };
    let bytes = encoder::encode(&img, &p);
    assert_eq!(read_header(&bytes).unwrap(), (20, 12, 3));
    let without = decode(&bytes).unwrap();
    assert_eq!(without.components, 3);
    assert!(!without.has_alpha);
    let with = decode_with_options(
        &bytes,
        &JpxOptions {
            max_resolution_reduction: 0,
            smask_in_data: true,
        },
    )
    .unwrap();
    assert_eq!(with.components, 4);
    assert!(with.has_alpha);
    let expected = expected_pixels(&img);
    assert_eq!(with.data, expected);
    let rgb: Vec<u8> = expected.chunks(4).flat_map(|px| px[..3].to_vec()).collect();
    assert_eq!(without.data, rgb);
    // cdef avec canaux de couleur dans le désordre : canal 0 → position 2,
    // canal 1 → position 3, canal 2 → position 1, soit la sortie B, R, V.
    let cdef2 = [0, 3, 0, 0, 0, 0, 0, 2, 0, 1, 0, 0, 0, 3, 0, 2, 0, 0, 0, 1];
    let img3 = rgb_noise(10, 8, 51);
    let p3 = Params {
        jp2: Some((16, encoder::make_box(b"cdef", &cdef2))),
        mct: false,
        ..Params::default()
    };
    let d3 = decode(&encoder::encode(&img3, &p3)).unwrap();
    let e3 = expected_pixels(&img3);
    for (dst, src) in d3.data.chunks(3).zip(e3.chunks(3)) {
        assert_eq!(dst, &[src[2], src[0], src[1]]);
    }
}

#[test]
fn read_header_reports_dimensions_without_decoding() {
    let img = rgb_smooth(37, 29);
    let bytes = encoder::encode(&img, &Params::default());
    assert_eq!(read_header(&bytes).unwrap(), (37, 29, 3));
    assert!(matches!(read_header(&[1, 2, 3]), Err(Error::Corrupt(_))));
    assert!(matches!(read_header(&bytes[..10]), Err(Error::Corrupt(_))));
}

// ---------------------------------------------------------------------------
// Robustesse
// ---------------------------------------------------------------------------

#[test]
fn truncation_at_every_length_never_panics() {
    let img = rgb_noise(24, 20, 52);
    let p = Params {
        levels: 2,
        tile: (16, 16),
        sop: true,
        eph: true,
        cbstyle: cbstyle::BYPASS | cbstyle::TERMALL,
        layers: 2,
        jp2: Some((16, Vec::new())),
        ..Params::default()
    };
    let bytes = encoder::encode(&img, &p);
    for len in 0..bytes.len() {
        let _ = decode(&bytes[..len]);
        let _ = read_header(&bytes[..len]);
    }
    let full = decode(&bytes).unwrap();
    assert_eq!(full.data, expected_pixels(&img));
}

#[test]
fn random_mutations_never_panic() {
    let img = rgb_noise(30, 22, 53);
    let p = Params {
        levels: 3,
        precincts: Some(vec![(2, 2), (3, 3), (4, 4), (5, 5)]),
        xcb: 3,
        ycb: 3,
        ..Params::default()
    };
    let bytes = encoder::encode(&img, &p);
    let mut rng = Rng(54);
    for round in 0..400 {
        let mut m = bytes.clone();
        let flips = 1 + round % 6;
        for _ in 0..flips {
            let at = (rng.next() as usize) % m.len();
            m[at] = (rng.next() & 0xFF) as u8;
        }
        let _ = decode(&m);
        let _ = decode_with_options(
            &m,
            &JpxOptions {
                max_resolution_reduction: 1,
                smask_in_data: true,
            },
        );
    }
    // Octets aléatoires après un en-tête valide.
    let sot = bytes.windows(2).position(|w| w == [0xFF, 0x90]).unwrap();
    for round in 0..50 {
        let mut m = bytes[..sot + 14].to_vec();
        for _ in 0..(50 + round * 7) {
            m.push((rng.next() & 0xFF) as u8);
        }
        let _ = decode(&m);
    }
    // Octets entièrement aléatoires.
    for _ in 0..200 {
        let n = 1 + (rng.next() as usize) % 200;
        let m: Vec<u8> = (0..n).map(|_| (rng.next() & 0xFF) as u8).collect();
        let _ = decode(&m);
    }
}

#[test]
fn absurd_headers_are_rejected_quickly() {
    // SIZ annonçant une image gigantesque.
    let mut siz = vec![0xFF, 0x4F, 0xFF, 0x51, 0, 41, 0, 0];
    for v in [u32::MAX, u32::MAX, 0, 0, u32::MAX, u32::MAX, 0, 0] {
        siz.extend_from_slice(&v.to_be_bytes());
    }
    siz.extend_from_slice(&[0, 1, 7, 1, 1]);
    assert!(matches!(read_header(&siz), Ok((u32::MAX, u32::MAX, 1))));
    // COD + QCD + SOT minimaux : l'allocation est refusée.
    siz.extend_from_slice(&[0xFF, 0x52, 0, 12, 0, 0, 0, 1, 0, 5, 4, 4, 0, 1]);
    siz.extend_from_slice(&[0xFF, 0x5C, 0, 4, 0x40, 0x40]);
    siz.extend_from_slice(&[0xFF, 0x90, 0, 10, 0, 0, 0, 0, 0, 0, 0, 1, 0xFF, 0x93]);
    assert!(matches!(decode(&siz), Err(Error::Corrupt(_))));
    // Précincts minuscules sur une grande image : trop de précincts.
    let mut small = vec![0xFF, 0x4F, 0xFF, 0x51, 0, 41, 0, 0];
    for v in [20000, 20000, 0, 0, 20000, 20000, 0, 0] {
        siz.clear();
        small.extend_from_slice(&u32::to_be_bytes(v));
    }
    small.extend_from_slice(&[0, 1, 7, 1, 1]);
    small.extend_from_slice(&[0xFF, 0x52, 0, 14, 1, 0, 0, 1, 0, 1, 4, 4, 0, 1, 0x11, 0x11]);
    small.extend_from_slice(&[0xFF, 0x5C, 0, 5, 0x40, 0x40, 0x40]);
    small.extend_from_slice(&[0xFF, 0x90, 0, 10, 0, 0, 0, 0, 0, 0, 0, 1, 0xFF, 0x93]);
    assert!(matches!(decode(&small), Err(Error::Corrupt(_))));
}

#[test]
fn truncated_body_gives_partial_image_not_error() {
    let img = gradient(64, 64);
    let p = Params {
        levels: 2,
        progression: Progression::Rlcp,
        ..Params::default()
    };
    let bytes = encoder::encode(&img, &p);
    let cut = bytes.len() * 2 / 3;
    let d = decode(&bytes[..cut]).unwrap();
    assert_eq!((d.width, d.height), (64, 64));
    // Les basses résolutions sont là : l'image reste ressemblante.
    let expected = expected_pixels(&img);
    let mean: u32 = d
        .data
        .iter()
        .zip(&expected)
        .map(|(&a, &b)| u32::from(a.abs_diff(b)))
        .sum::<u32>()
        / 4096;
    assert!(mean < 40, "écart moyen {mean}");
}

// ---------------------------------------------------------------------------
// Performance
// ---------------------------------------------------------------------------

/// `cargo test --release -p acrux-codecs a4_300dpi -- --ignored --nocapture`
#[test]
#[ignore = "mesure de performance : cargo test --release -p acrux-codecs a4_300dpi -- --ignored --nocapture"]
fn a4_300dpi_colour_decodes_in_under_three_seconds() {
    let (w, h) = (2480u32, 3508u32);
    let img = rgb_image(w, h, |x, y| {
        let t = (x * 7 + y * 3) % 256;
        [
            ((x * 255) / w) as i32,
            ((y * 255) / h) as i32,
            ((t as i32 - 128) * ((x % 5) as i32) / 5 + 128).clamp(0, 255),
        ]
    });
    let p = Params {
        levels: 5,
        ..Params::default()
    };
    let bytes = encoder::encode(&img, &p);
    let start = std::time::Instant::now();
    let d = decode(&bytes).unwrap();
    let elapsed = start.elapsed();
    println!("A4 300 dpi couleur ({} octets) : {elapsed:?}", bytes.len());
    assert_eq!(d.data, expected_pixels(&img));
    assert!(elapsed.as_secs_f64() < 3.0, "trop lent : {elapsed:?}");
}

// ---------------------------------------------------------------------------
// Échantillons de corpus
// (cargo test -p acrux-codecs --release -- --ignored generate_corpus)
// ---------------------------------------------------------------------------

/// Masque bilevel 200 × 120 portant `label` (glyphes 15 × 21 en haut).
fn corpus_label(label: &str) -> crate::ccitt::tests::encoder::Image {
    use crate::ccitt::tests::corpus;
    let mut m =
        crate::ccitt::tests::encoder::Image::new(corpus::SAMPLE_WIDTH, corpus::SAMPLE_HEIGHT);
    corpus::draw_text(&mut m, 8, 8, label, 3);
    m
}

/// Formes du corpus JPX : disque rouge, triangle vert, carré bleu.
fn corpus_shape(x: u32, y: u32) -> Option<[i32; 3]> {
    let (fx, fy) = (i64::from(x), i64::from(y));
    if (fx - 50).pow(2) + (fy - 80).pow(2) <= 26 * 26 {
        Some([220, 30, 30])
    } else if (44..=108).contains(&fy) && (fx - 100).abs() * 64 <= (fy - 44) * 30 {
        Some([30, 170, 50])
    } else if (140..190).contains(&fx) && (54..106).contains(&fy) {
        Some([30, 60, 220])
    } else {
        None
    }
}

/// Image RVB 200 × 120 du corpus : dégradé, formes, étiquette `label` en `ink`.
fn corpus_rgb(label: &str, ink: [i32; 3]) -> Image {
    use crate::ccitt::tests::corpus::{SAMPLE_HEIGHT, SAMPLE_WIDTH};
    let (w, h) = (SAMPLE_WIDTH as u32, SAMPLE_HEIGHT as u32);
    let mask = corpus_label(label);
    rgb_image(w, h, |x, y| {
        if mask.row(y as usize)[x as usize] != 0 {
            ink
        } else if let Some(c) = corpus_shape(x, y) {
            c
        } else {
            let gx = (x * 255 / (w - 1)) as i32;
            let gy = (y * 255 / (h - 1)) as i32;
            [gx, gy, 255 - gx]
        }
    })
}

/// Écrit dans `target/corpus-gen/` les flux JPEG 2000 du corpus de synthèse
/// (tous sans perte : 5-3, RCT pour le RVB) et leurs images source ; voir
/// `crates/acrux-render/tests/synthese_codecs.rs`.
#[test]
#[ignore = "génération du corpus : cargo test -p acrux-codecs --release -- --ignored generate_corpus"]
fn generate_corpus_samples() {
    use crate::ccitt::tests::corpus::{self, SAMPLE_HEIGHT, SAMPLE_WIDTH};
    let (w, h) = (SAMPLE_WIDTH as u32, SAMPLE_HEIGHT as u32);
    let lossless = Params {
        levels: 3,
        reversible: true,
        ..Params::default()
    };

    // 1. Gris 8 bits, conteneur JP2 (colr 17) : dégradé diagonal, disque
    //    sombre, carré clair, étiquette noire. Sans /ColorSpace dans le PDF.
    let mask = corpus_label("GRIS");
    let gray = gray_image(w, h, |x, y| {
        let (fx, fy) = (i64::from(x), i64::from(y));
        if mask.row(y as usize)[x as usize] != 0 {
            0
        } else if (fx - 60).pow(2) + (fy - 80).pow(2) <= 26 * 26 {
            30
        } else if (120..180).contains(&fx) && (54..106).contains(&fy) {
            235
        } else {
            u32::midpoint(x * 255 / (w - 1), y * 255 / (h - 1)) as i32
        }
    });
    let p = Params {
        mct: false,
        jp2: Some((17, Vec::new())),
        ..lossless.clone()
    };
    let bytes = encoder::encode(&gray, &p);
    let d = decode(&bytes).unwrap();
    assert_eq!(
        (d.width, d.height, d.components, d.colorspace.clone()),
        (w, h, 1, JpxColorSpace::Gray)
    );
    let expected = expected_pixels(&gray);
    assert_eq!(d.data, expected, "gris");
    corpus::write("jpx-gray.jp2", &bytes);
    let rgb: Vec<u8> = expected.iter().flat_map(|&v| [v, v, v]).collect();
    corpus::write_png_rgb("jpx-gray.png", w, h, &rgb);

    // 2. RVB (5-3 + RCT), conteneur JP2 (colr 16), étiquette blanche.
    let rgb = corpus_rgb("RVB", [255, 255, 255]);
    let p = Params {
        jp2: Some((16, Vec::new())),
        ..lossless.clone()
    };
    let bytes = encoder::encode(&rgb, &p);
    let d = decode(&bytes).unwrap();
    assert_eq!(
        (d.components, d.colorspace.clone()),
        (3, JpxColorSpace::Srgb)
    );
    let expected = expected_pixels(&rgb);
    assert_eq!(d.data, expected, "rvb");
    corpus::write("jpx-rgb.jp2", &bytes);
    corpus::write_png_rgb("jpx-rgb.png", w, h, &expected);

    // 3. RVB + alpha (cdef : canal 3 = opacité) pour /SMaskInData 1 : alpha
    //    en rampe horizontale, disque et étiquette (noire) opaques, carré
    //    entièrement transparent.
    let mut rgba = corpus_rgb("ALPHA", [0, 0, 0]);
    let mask = corpus_label("ALPHA");
    let alpha: Vec<i32> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| {
            let (fx, fy) = (i64::from(x), i64::from(y));
            let opaque = mask.row(y as usize)[x as usize] != 0
                || (fx - 50).pow(2) + (fy - 80).pow(2) <= 26 * 26;
            if opaque {
                255
            } else if (140..190).contains(&fx) && (54..106).contains(&fy) {
                0
            } else {
                (x * 255 / (w - 1)) as i32
            }
        })
        .collect();
    rgba.components.push(Component {
        precision: 8,
        signed: false,
        xr: 1,
        yr: 1,
        data: alpha,
    });
    let cdef = [
        0, 4, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 2, 0, 2, 0, 0, 0, 3, 0, 3, 0, 1, 0, 0,
    ];
    let p = Params {
        jp2: Some((16, encoder::make_box(b"cdef", &cdef))),
        ..lossless.clone()
    };
    let bytes = encoder::encode(&rgba, &p);
    let d = decode_with_options(
        &bytes,
        &JpxOptions {
            max_resolution_reduction: 0,
            smask_in_data: true,
        },
    )
    .unwrap();
    assert!(d.has_alpha && d.components == 4);
    let expected = expected_pixels(&rgba);
    assert_eq!(d.data, expected, "rvb + alpha");
    // Image source de la page : composée sur fond blanc (C·α + 1·(1 − α)),
    // arrondie au plus proche.
    let over_white: Vec<u8> = expected
        .chunks(4)
        .flat_map(|px| {
            let a = u32::from(px[3]);
            [0, 1, 2].map(|k| ((u32::from(px[k]) * a + 255 * (255 - a) + 127) / 255) as u8)
        })
        .collect();
    let alpha_png: Vec<u8> = expected.chunks(4).flat_map(|px| [px[3]; 3]).collect();
    corpus::write("jpx-rgba.jp2", &bytes);
    corpus::write_png_rgb("jpx-rgba.png", w, h, &over_white);
    corpus::write_png_rgb("jpx-rgba-alpha.png", w, h, &alpha_png);

    // 4. Flux de code J2K brut (sans conteneur) en RVB : le PDF fournit
    //    /ColorSpace.
    let j2k = corpus_rgb("J2K", [255, 255, 255]);
    let bytes = encoder::encode(&j2k, &lossless);
    let d = decode(&bytes).unwrap();
    assert_eq!(d.colorspace, JpxColorSpace::Unknown);
    let expected = expected_pixels(&j2k);
    assert_eq!(d.data, expected, "j2k");
    corpus::write("jpx-rgb.j2k", &bytes);
    corpus::write_png_rgb("jpx-j2k.png", w, h, &expected);
}
