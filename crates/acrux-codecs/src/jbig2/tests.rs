//! Tests du décodeur JBIG2Decode.
//!
//! - `encoder` : encodeur JBIG2 de test (codeur MQ de l'annexe E, entiers de
//!   l'annexe A, régions génériques, raffinement, dictionnaires, régions de
//!   texte, motifs, demi-teintes, segments) ;
//! - séquence de test du codeur arithmétique (T.88 §H.2) ;
//! - allers-retours sur des images synthétiques pour chaque type de segment ;
//! - robustesse : troncatures, mutations et octets aléatoires.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::too_many_lines,
    clippy::needless_range_loop,
    clippy::similar_names,
    clippy::too_many_arguments
)]

mod encoder;

use super::huffman::{self, BitReader};
use super::mq::{IntContext, MqDecoder};
use super::*;
use crate::ccitt::tests::encoder::Image;
use crate::ccitt::tests::{checkerboard, glyphs, noise, solid, stripes, Lcg};
use encoder::*;

// ---------------------------------------------------------------------------
// Outils
// ---------------------------------------------------------------------------

/// Sortie attendue du filtre pour une image (1 = noir) : bits inversés.
fn expected(img: &Image) -> Vec<u8> {
    img.packed().iter().map(|b| !b).collect()
}

/// Dessine `src` (OR) en `(x, y)` sur `dst`, en coupant ce qui déborde.
fn draw(dst: &mut Image, src: &Image, x: i32, y: i32) {
    for sy in 0..src.height {
        for sx in 0..src.width {
            let (dx, dy) = (x + sx as i32, y + sy as i32);
            if dx >= 0 && dy >= 0 && (dx as usize) < dst.width && (dy as usize) < dst.height {
                let v = dst.row(dy as usize)[dx as usize] | src.row(sy)[sx];
                dst.set(dx as usize, dy as usize, v);
            }
        }
    }
}

fn end_of_page(number: u32) -> Vec<u8> {
    segment(number, 49, &[], &[])
}

/// Flux de page : informations de page, segments, fin de page.
fn page_stream(w: u32, h: u32, segments: &[Vec<u8>]) -> Vec<u8> {
    let mut out = page_info(w, h, 0);
    for s in segments {
        out.extend_from_slice(s);
    }
    out.extend_from_slice(&end_of_page(99));
    out
}

fn decode_page(data: &[u8], w: usize, h: usize) -> Vec<u8> {
    decode(data, None, w as u32, h as u32).expect("décodage")
}

/// Trois symboles triés par (hauteur, largeur) : point 3 × 3, « A » 5 × 7,
/// « K » 5 × 7 (extraits des glyphes du module CCITT).
fn alphabet() -> Vec<Image> {
    let g = glyphs();
    let mut dot = Image::new(3, 3);
    for y in 0..3 {
        for x in 0..3 {
            dot.set(x, y, u8::from(x != 1 || y != 1));
        }
    }
    let mut a = Image::new(5, 7);
    let mut k = Image::new(5, 7);
    for y in 0..7 {
        for x in 0..5 {
            a.set(x, y, g.row(y + 1)[x + 1]);
            k.set(x, y, g.row(y + 1)[x + 8]);
        }
    }
    vec![dot, a, k]
}

fn sample_instances() -> Vec<Instance> {
    vec![
        Instance {
            x: 1,
            y: 2,
            id: 1,
            refined: None,
        },
        Instance {
            x: 7,
            y: 2,
            id: 2,
            refined: None,
        },
        Instance {
            x: 13,
            y: 6,
            id: 0,
            refined: None,
        },
        Instance {
            x: 2,
            y: 12,
            id: 2,
            refined: None,
        },
        Instance {
            x: 9,
            y: 11,
            id: 1,
            refined: None,
        },
        Instance {
            x: 15,
            y: 13,
            id: 0,
            refined: None,
        },
        Instance {
            x: 0,
            y: 20,
            id: 0,
            refined: None,
        },
    ]
}

fn render_instances(w: usize, h: usize, instances: &[Instance], symbols: &[Image]) -> Image {
    let mut img = Image::new(w, h);
    for inst in instances {
        let sym = inst.refined.as_ref().unwrap_or(&symbols[inst.id]);
        draw(&mut img, sym, inst.x, inst.y);
    }
    img
}

// ---------------------------------------------------------------------------
// Codeur arithmétique
// ---------------------------------------------------------------------------

/// Séquence de test de T.88 §H.2 : 256 bits d'entrée codés avec un seul
/// contexte (état initial 0, MPS 0) et les 30 octets attendus.
const H2_INPUT: [u8; 32] = [
    0x00, 0x02, 0x00, 0x51, 0x00, 0x00, 0x00, 0xC0, 0x03, 0x52, 0x87, 0x2A, 0xAA, 0xAA, 0xAA, 0xAA,
    0x82, 0xC0, 0x20, 0x00, 0xFC, 0xD7, 0x9E, 0xF6, 0xBF, 0x7F, 0xED, 0x90, 0x4F, 0x46, 0xA3, 0xBF,
];
const H2_OUTPUT: [u8; 30] = [
    0x84, 0xC7, 0x3B, 0xFC, 0xE1, 0xA1, 0x43, 0x04, 0x02, 0x20, 0x00, 0x00, 0x41, 0x0D, 0xBB, 0x86,
    0xF4, 0x31, 0x7F, 0xFF, 0x88, 0xFF, 0x37, 0x47, 0x1A, 0xDB, 0x6A, 0xDF, 0xFF, 0xAC,
];

#[test]
fn mq_encoder_reproduces_h2_test_sequence() {
    let mut mq = MqEncoder::new();
    let mut cx = 0u8;
    for byte in H2_INPUT {
        for i in (0..8).rev() {
            mq.encode(&mut cx, (byte >> i) & 1);
        }
    }
    assert_eq!(mq.finish(), H2_OUTPUT.to_vec());
}

#[test]
fn mq_decoder_decodes_h2_test_sequence() {
    let mut mq = MqDecoder::new(&H2_OUTPUT);
    let mut cx = 0u8;
    for byte in H2_INPUT {
        let mut v = 0u8;
        for _ in 0..8 {
            v = (v << 1) | mq.decode(&mut cx);
        }
        assert_eq!(v, byte);
    }
}

#[test]
fn mq_round_trip_with_many_contexts() {
    let mut rng = Lcg(11);
    let bits: Vec<(usize, u8)> = (0..20_000)
        .map(|i| {
            let ctx = (rng.next() % 64) as usize;
            // Biais variable pour parcourir les états de la table Qe.
            let bit = u8::from(rng.next() % 100 < (i % 97) as u32);
            (ctx, bit)
        })
        .collect();
    let mut mq = MqEncoder::new();
    let mut cx = [0u8; 64];
    for &(c, b) in &bits {
        mq.encode(&mut cx[c], b);
    }
    let data = mq.finish();
    let mut dec = MqDecoder::new(&data);
    let mut cx = [0u8; 64];
    for &(c, b) in &bits {
        assert_eq!(dec.decode(&mut cx[c]), b);
    }
}

#[test]
fn integer_procedures_round_trip() {
    let values: Vec<Option<i32>> = [
        0,
        1,
        2,
        3,
        4,
        5,
        19,
        20,
        21,
        83,
        84,
        85,
        339,
        340,
        341,
        4435,
        4436,
        4437,
        100_000,
        1_000_000_000,
        -1,
        -3,
        -4,
        -20,
        -84,
        -340,
        -4436,
        -123_456,
    ]
    .iter()
    .map(|&v| Some(v))
    .chain([None, None])
    .collect();
    let mut mq = MqEncoder::new();
    let mut enc = IntEncoder::new();
    for &v in &values {
        enc.encode(&mut mq, v);
    }
    let mut iaid = IaidEncoder::new(5);
    for id in [0u32, 1, 17, 31] {
        iaid.encode(&mut mq, id);
    }
    let data = mq.finish();
    let mut dec = MqDecoder::new(&data);
    let mut ctx = IntContext::new();
    for &v in &values {
        assert_eq!(ctx.decode(&mut dec), v);
    }
    let mut iaid = super::mq::IaidContext::new(5);
    for id in [0u32, 1, 17, 31] {
        assert_eq!(iaid.decode(&mut dec), id);
    }
}

// ---------------------------------------------------------------------------
// Régions génériques
// ---------------------------------------------------------------------------

fn generic_images() -> Vec<(&'static str, Image)> {
    vec![
        ("glyphes", glyphs()),
        ("damier", checkerboard(37, 11)),
        ("bruit", noise(70, 23, 9)),
        ("bandes", stripes(40, 12)),
        ("noir", solid(9, 4, true)),
        ("colonne", noise(1, 12, 4)),
    ]
}

#[test]
fn generic_region_all_templates_round_trip() {
    for template in 0..4u8 {
        for (name, img) in generic_images() {
            for tpgdon in [false, true] {
                let data = generic_region_data(&img, 0, 0, template, NOMINAL_AT, tpgdon);
                let stream = page_stream(
                    img.width as u32,
                    img.height as u32,
                    &[segment(1, 38, &[], &data)],
                );
                assert_eq!(
                    decode_page(&stream, img.width, img.height),
                    expected(&img),
                    "{name} gabarit {template} tpgdon {tpgdon}"
                );
            }
        }
    }
}

#[test]
fn generic_region_with_custom_at_pixels() {
    let img = noise(50, 20, 21);
    let ats: [[(i8, i8); 4]; 3] = [
        [(2, -3), (-5, 0), (1, -3), (-6, -1)],
        [(-1, -3), (-3, -1), (2, -2), (-2, -2)],
        [(-128, -1), (127, -2), (0, -127), (-2, -2)],
    ];
    for template in 0..4u8 {
        for at in ats {
            let data = generic_region_data(&img, 0, 0, template, at, true);
            let stream = page_stream(50, 20, &[segment(1, 38, &[], &data)]);
            assert_eq!(decode_page(&stream, 50, 20), expected(&img), "{at:?}");
        }
    }
}

#[test]
fn generic_region_mmr_round_trip() {
    for (name, img) in generic_images() {
        for eofb in [true, false] {
            let data = generic_region_mmr_data(&img, 0, 0, eofb);
            let stream = page_stream(
                img.width as u32,
                img.height as u32,
                &[segment(1, 39, &[], &data)],
            );
            assert_eq!(
                decode_page(&stream, img.width, img.height),
                expected(&img),
                "{name}"
            );
        }
    }
}

#[test]
fn generic_region_with_unknown_length() {
    let img = glyphs();
    // Hauteur de région inconnue, longueur inconnue, nombre de lignes
    // après le marqueur FF AC (§7.2.7).
    let mut data = generic_region_data(&img, 0, 0, 0, NOMINAL_AT, false);
    data[4..8].copy_from_slice(&be32(0xFFFF_FFFF));
    data.extend_from_slice(&be32(img.height as u32));
    let mut stream = page_info(15, 9, 0);
    stream.extend_from_slice(&segment_with_length(1, 38, &[], &data, 0xFFFF_FFFF));
    stream.extend_from_slice(&end_of_page(2));
    assert_eq!(decode_page(&stream, 15, 9), expected(&img));
    // Variante MMR : terminateur 00 00.
    let mut data = generic_region_mmr_data(&img, 0, 0, true);
    data[4..8].copy_from_slice(&be32(0xFFFF_FFFF));
    data.extend_from_slice(&[0, 0]);
    data.extend_from_slice(&be32(img.height as u32));
    let mut stream = page_info(15, 9, 0);
    stream.extend_from_slice(&segment_with_length(1, 38, &[], &data, 0xFFFF_FFFF));
    stream.extend_from_slice(&end_of_page(2));
    assert_eq!(decode_page(&stream, 15, 9), expected(&img));
}

#[test]
fn regions_are_composed_at_their_position_and_clipped() {
    let g = glyphs();
    let d1 = generic_region_data(&g, 3, 2, 0, NOMINAL_AT, false);
    let d2 = generic_region_data(&g, 10, 5, 1, NOMINAL_AT, false);
    // Troisième région entièrement hors page : ignorée.
    let d3 = generic_region_data(&g, 40, 40, 2, NOMINAL_AT, false);
    let stream = page_stream(
        20,
        12,
        &[
            segment(1, 38, &[], &d1),
            segment(2, 38, &[], &d2),
            segment(3, 38, &[], &d3),
        ],
    );
    let mut img = Image::new(20, 12);
    draw(&mut img, &g, 3, 2);
    draw(&mut img, &g, 10, 5);
    assert_eq!(decode_page(&stream, 20, 12), expected(&img));
}

#[test]
fn composition_operators_and_default_pixel() {
    let g = glyphs();
    // Page noire par défaut, région en AND : seuls les pixels noirs du
    // glyphe restent.
    let mut data = generic_region_data(&g, 0, 0, 0, NOMINAL_AT, false);
    data[16] = 1; // AND
    let mut stream = page_info(15, 9, 1);
    stream.extend_from_slice(&segment(1, 38, &[], &data));
    stream.extend_from_slice(&end_of_page(2));
    assert_eq!(decode_page(&stream, 15, 9), expected(&g));
    // XOR de deux fois le même glyphe : page blanche.
    let mut data = generic_region_data(&g, 0, 0, 0, NOMINAL_AT, false);
    data[16] = 2;
    let stream = page_stream(
        15,
        9,
        &[segment(1, 38, &[], &data), segment(2, 38, &[], &data)],
    );
    assert_eq!(decode_page(&stream, 15, 9), vec![0xFF; 18]);
    // Sans informations de page : la page est blanche par défaut.
    let data = generic_region_data(&g, 0, 0, 0, NOMINAL_AT, false);
    let stream = segment(1, 38, &[], &data);
    assert_eq!(decode_page(&stream, 15, 9), expected(&g));
}

#[test]
fn page_height_unknown_with_end_of_stripe() {
    let g = glyphs();
    let mut stream = page_info(15, 0xFFFF_FFFF, 0);
    stream.extend_from_slice(&segment(
        1,
        38,
        &[],
        &generic_region_data(&g, 0, 0, 0, NOMINAL_AT, false),
    ));
    stream.extend_from_slice(&segment(2, 50, &[], &be32(8)));
    stream.extend_from_slice(&segment(
        3,
        38,
        &[],
        &generic_region_data(&g, 0, 9, 0, NOMINAL_AT, false),
    ));
    stream.extend_from_slice(&segment(4, 50, &[], &be32(17)));
    stream.extend_from_slice(&end_of_page(5));
    let mut img = Image::new(15, 18);
    draw(&mut img, &g, 0, 0);
    draw(&mut img, &g, 0, 9);
    assert_eq!(decode_page(&stream, 15, 18), expected(&img));
}

// ---------------------------------------------------------------------------
// Raffinement
// ---------------------------------------------------------------------------

#[test]
fn refinement_region_refines_the_page() {
    let base = noise(30, 14, 5);
    let mut target = base.clone();
    // Quelques pixels changés, dont une ligne entière et une ligne intacte.
    for x in 0..30 {
        target.set(x, 3, u8::from(x % 3 == 0));
    }
    target.set(5, 7, 1 - target.row(7)[5]);
    target.set(20, 10, 1 - target.row(10)[20]);
    for template in 0..2u8 {
        for tpgron in [false, true] {
            let mut data = region_info(30, 14, 0, 0, 0);
            data.push(template | (u8::from(tpgron) << 1));
            if template == 0 {
                data.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
            }
            let mut mq = MqEncoder::new();
            let mut enc = RefinementEncoder::new();
            enc.encode(
                &mut mq,
                &target,
                &base,
                0,
                0,
                template,
                [(-1, -1), (-1, -1)],
                tpgron,
            );
            data.extend_from_slice(&mq.finish());
            let stream = page_stream(
                30,
                14,
                &[
                    segment(
                        1,
                        38,
                        &[],
                        &generic_region_data(&base, 0, 0, 0, NOMINAL_AT, false),
                    ),
                    segment(2, 42, &[], &data),
                ],
            );
            assert_eq!(
                decode_page(&stream, 30, 14),
                expected(&target),
                "gabarit {template} tpgron {tpgron}"
            );
        }
    }
}

#[test]
fn refinement_region_uses_referenced_intermediate_region() {
    let base = glyphs();
    let mut target = base.clone();
    target.set(0, 0, 1);
    target.set(14, 8, 1);
    let mut data = region_info(15, 9, 0, 0, 0);
    data.push(1); // gabarit 1, sans TPGRON
    let mut mq = MqEncoder::new();
    let mut enc = RefinementEncoder::new();
    enc.encode(
        &mut mq,
        &target,
        &base,
        0,
        0,
        1,
        [(-1, -1), (-1, -1)],
        false,
    );
    data.extend_from_slice(&mq.finish());
    let stream = page_stream(
        15,
        9,
        &[
            // Région intermédiaire : jamais composée directement.
            segment(
                1,
                36,
                &[],
                &generic_region_data(&base, 0, 0, 0, NOMINAL_AT, false),
            ),
            segment(2, 42, &[1], &data),
        ],
    );
    assert_eq!(decode_page(&stream, 15, 9), expected(&target));
}

// ---------------------------------------------------------------------------
// Dictionnaire de symboles et région de texte
// ---------------------------------------------------------------------------

fn text_page(
    symbols: &[Image],
    instances: &[Instance],
    layout: Layout,
    refine: bool,
    template: u8,
) -> (Vec<u8>, Image) {
    let (w, h) = (24, 26);
    let dict = symbol_dict_data(symbols, template, 0);
    let mut region = region_info(w, h, 0, 0, 0);
    region.extend_from_slice(&text_region_data(instances, symbols, layout, refine));
    let stream = page_stream(
        w,
        h,
        &[segment(1, 0, &[], &dict), segment(2, 6, &[1], &region)],
    );
    let img = render_instances(w as usize, h as usize, instances, symbols);
    (stream, img)
}

#[test]
fn symbol_dict_and_text_region_arith_round_trip() {
    let symbols = alphabet();
    let instances = sample_instances();
    for template in 0..4u8 {
        for log_strips in [0u32, 2, 3] {
            for ref_corner in 0..4u8 {
                for transposed in [false, true] {
                    let layout = Layout {
                        log_strips,
                        ref_corner,
                        transposed,
                        ds_offset: if log_strips == 2 { -2 } else { 0 },
                        rtemplate: 0,
                    };
                    let (stream, img) = text_page(&symbols, &instances, layout, false, template);
                    assert_eq!(
                        decode_page(&stream, 24, 26),
                        expected(&img),
                        "gabarit {template} bandes {log_strips} coin {ref_corner} transposé {transposed}"
                    );
                }
            }
        }
    }
}

#[test]
fn text_region_with_refinement() {
    let symbols = alphabet();
    let mut instances = sample_instances();
    // Instance raffinée de même taille (un pixel changé) et instance
    // raffinée agrandie (RDW = 2, RDH = 1).
    let mut r1 = symbols[1].clone();
    r1.set(2, 3, 1 - r1.row(3)[2]);
    instances[0].refined = Some(r1);
    let mut r2 = Image::new(7, 8);
    draw(&mut r2, &symbols[2], 1, 0);
    r2.set(0, 7, 1);
    instances[3].refined = Some(r2);
    for rtemplate in 0..2u8 {
        let layout = Layout {
            rtemplate,
            ..Layout::default()
        };
        let (stream, img) = text_page(&symbols, &instances, layout, true, 0);
        assert_eq!(
            decode_page(&stream, 24, 26),
            expected(&img),
            "gabarit {rtemplate}"
        );
    }
}

#[test]
fn symbol_dictionary_in_globals() {
    let symbols = alphabet();
    let instances = sample_instances();
    let globals = segment(1, 0, &[], &symbol_dict_data(&symbols, 0, 0));
    let mut region = region_info(24, 26, 0, 0, 0);
    region.extend_from_slice(&text_region_data(
        &instances,
        &symbols,
        Layout::default(),
        false,
    ));
    let stream = page_stream(24, 26, &[segment(2, 6, &[1], &region)]);
    let img = render_instances(24, 26, &instances, &symbols);
    assert_eq!(
        decode(&stream, Some(&globals), 24, 26).unwrap(),
        expected(&img)
    );
    // Sans les globaux, le dictionnaire manque : région ignorée, page blanche.
    assert_eq!(decode(&stream, None, 24, 26).unwrap(), vec![0xFF; 3 * 26]);
}

#[test]
fn symbol_dictionary_with_input_symbols_from_another_dictionary() {
    let symbols = alphabet();
    // Dictionnaire 1 : le point ; dictionnaire 2 : A et K, référence 1 et
    // exporte tout (entrants compris).
    let d1 = symbol_dict_data(&symbols[..1], 0, 0);
    let d2 = symbol_dict_data(&symbols[1..], 0, 1);
    let instances = sample_instances();
    let mut region = region_info(24, 26, 0, 0, 0);
    region.extend_from_slice(&text_region_data(
        &instances,
        &symbols,
        Layout::default(),
        false,
    ));
    let stream = page_stream(
        24,
        26,
        &[
            segment(1, 0, &[], &d1),
            segment(2, 0, &[1], &d2),
            segment(3, 6, &[2], &region),
        ],
    );
    let img = render_instances(24, 26, &instances, &symbols);
    assert_eq!(decode_page(&stream, 24, 26), expected(&img));
}

// ---------------------------------------------------------------------------
// Huffman
// ---------------------------------------------------------------------------

fn bits(s: &str) -> Vec<u8> {
    let mut w = crate::ccitt::tests::encoder::BitWriter::default();
    for c in s.chars().filter(|c| *c == '0' || *c == '1') {
        w.put(u32::from(c == '1'), 1);
    }
    w.finish()
}

#[test]
fn standard_tables_known_codes() {
    // B.1 : 0..15 → « 0 » + 4 bits ; 16..271 → « 10 » + 8 bits.
    let t = huffman::standard(1).unwrap();
    let data = bits("0 0101  10 00000011");
    let mut r = BitReader::new(&data);
    assert_eq!(t.decode(&mut r).unwrap(), Some(5));
    assert_eq!(t.decode(&mut r).unwrap(), Some(19));
    // B.2 : 2 → « 110 », OOB → « 111111 », 75 → « 111110 » + 32 bits.
    let t = huffman::standard(2).unwrap();
    let data = bits("110 111111 111110 00000000000000000000000000000010");
    let mut r = BitReader::new(&data);
    assert_eq!(t.decode(&mut r).unwrap(), Some(2));
    assert_eq!(t.decode(&mut r).unwrap(), None);
    assert_eq!(t.decode(&mut r).unwrap(), Some(77));
    // B.3 : −1 → « 11111110 » + 8 bits (255) ; OOB → « 111110 ».
    let t = huffman::standard(3).unwrap();
    let data = bits("11111110 11111111 111110");
    let mut r = BitReader::new(&data);
    assert_eq!(t.decode(&mut r).unwrap(), Some(-1));
    assert_eq!(t.decode(&mut r).unwrap(), None);
    // B.4 : 1 → « 0 », 4 → « 1110 000 ».
    let t = huffman::standard(4).unwrap();
    let data = bits("0 1110 000");
    let mut r = BitReader::new(&data);
    assert_eq!(t.decode(&mut r).unwrap(), Some(1));
    assert_eq!(t.decode(&mut r).unwrap(), Some(4));
    // B.11 : 0 → « 0 », 2 → « 10 » + « 1 ».
    let t = huffman::standard(11).unwrap();
    let data = bits("0 10 1");
    let mut r = BitReader::new(&data);
    assert_eq!(t.decode(&mut r).unwrap(), Some(0));
    assert_eq!(t.decode(&mut r).unwrap(), Some(2));
    // B.8 : 0 → « 00 », OOB → « 01 ».
    let t = huffman::standard(8).unwrap();
    let data = bits("00 0 01");
    let mut r = BitReader::new(&data);
    assert_eq!(t.decode(&mut r).unwrap(), Some(0));
    assert_eq!(t.decode(&mut r).unwrap(), None);
}

#[test]
fn all_standard_tables_round_trip() {
    for n in 1..=15u32 {
        let lines = huffman::standard_lines(n).unwrap();
        let enc = HuffEncoder::standard(n);
        let table = huffman::standard(n).unwrap();
        // Pour chaque ligne : valeur basse, milieu et haute de la plage.
        let mut values: Vec<Option<i32>> = Vec::new();
        for l in lines {
            match l.kind {
                huffman::LineKind::Oob => values.push(None),
                huffman::LineKind::Lower => {
                    values.push(Some(l.low));
                    values.push(Some(l.low - 1000));
                }
                huffman::LineKind::Normal => {
                    values.push(Some(l.low));
                    if l.range_len == 32 {
                        values.push(Some(l.low + 100_000));
                    } else if l.range_len > 0 {
                        values.push(Some(l.low + (1 << l.range_len) - 1));
                        values.push(Some(l.low + (1 << (l.range_len - 1))));
                    }
                }
            }
        }
        let mut w = crate::ccitt::tests::encoder::BitWriter::default();
        for &v in &values {
            enc.encode(&mut w, v);
        }
        let data = w.finish();
        let mut r = BitReader::new(&data);
        for &v in &values {
            assert_eq!(table.decode(&mut r).unwrap(), v, "table B.{n}");
        }
    }
}

#[test]
fn custom_table_segment_matches_b2() {
    // Lignes de B.2 en table personnalisée : 0, 1, 2 (plage 0), 3..10
    // (plage 3), 11..74 (plage 6), haute 75, basse −1, OOB.
    let seg = custom_table_segment(
        5,
        0,
        75,
        &[(1, 0), (2, 0), (3, 0), (4, 3), (5, 6)],
        0,
        6,
        Some(6),
    );
    // Les données suivent l'en-tête de segment (11 octets).
    let table = huffman::parse_custom(&seg[11..]).unwrap();
    let enc = HuffEncoder::standard(2);
    let values = [
        Some(0),
        Some(2),
        Some(7),
        Some(50),
        Some(75),
        Some(1000),
        None,
    ];
    let mut w = crate::ccitt::tests::encoder::BitWriter::default();
    for &v in &values {
        enc.encode(&mut w, v);
    }
    let data = w.finish();
    let mut r = BitReader::new(&data);
    for &v in &values {
        assert_eq!(table.decode(&mut r).unwrap(), v);
    }
}

#[test]
fn symbol_dict_and_text_region_huffman_round_trip() {
    let symbols = alphabet();
    let instances = sample_instances();
    for mmr in [false, true] {
        for log_strips in [0u32, 1, 3] {
            for custom_dw in [false, true] {
                let dict = symbol_dict_huffman_data(&symbols, mmr, custom_dw);
                let mut region = region_info(24, 26, 0, 0, 0);
                region
                    .extend_from_slice(&text_region_huffman_data(&instances, &symbols, log_strips));
                let table = custom_table_segment(
                    7,
                    0,
                    75,
                    &[(1, 0), (2, 0), (3, 0), (4, 3), (5, 6)],
                    0,
                    6,
                    Some(6),
                );
                let refs: &[u32] = if custom_dw { &[7] } else { &[] };
                let stream = page_stream(
                    24,
                    26,
                    &[
                        table,
                        segment(1, 0, refs, &dict),
                        segment(2, 6, &[1], &region),
                    ],
                );
                let img = render_instances(24, 26, &instances, &symbols);
                assert_eq!(
                    decode_page(&stream, 24, 26),
                    expected(&img),
                    "mmr {mmr} bandes {log_strips} table personnalisée {custom_dw}"
                );
            }
        }
    }
}

#[test]
fn huffman_dictionary_with_refinement_is_unsupported() {
    let flags: u16 = 0b11; // SDHUFF + SDREFAGG
    let mut data = flags.to_be_bytes().to_vec();
    data.extend_from_slice(&[0, 0, 0, 0]); // rAT (gabarit 0)
    data.extend_from_slice(&be32(1));
    data.extend_from_slice(&be32(1));
    assert!(matches!(
        symbol::decode_symbol_dict_segment(&data, &[], &[], None),
        Err(Error::Unsupported(_))
    ));
}

// ---------------------------------------------------------------------------
// Motifs et demi-teintes
// ---------------------------------------------------------------------------

fn patterns() -> Vec<Image> {
    // Quatre motifs 4 × 4 de plus en plus noirs.
    (0..4)
        .map(|level| {
            let mut p = Image::new(4, 4);
            for y in 0..4 {
                for x in 0..4 {
                    p.set(x, y, u8::from((x + 4 * y) % 5 < level));
                }
            }
            p
        })
        .collect()
}

#[test]
fn pattern_dict_and_halftone_region_round_trip() {
    let pats = patterns();
    let (gw, gh) = (5usize, 4usize);
    let values: Vec<u32> = (0..gw * gh).map(|i| (i * 7 % 4) as u32).collect();
    for mmr in [false, true] {
        for template in 0..4u8 {
            let dict = pattern_dict_data(&pats, template, mmr);
            let mut region = region_info(20, 16, 0, 0, 0);
            region.extend_from_slice(&halftone_region_data(
                &values,
                HalftoneParams {
                    gw,
                    gh,
                    hgx: 0,
                    hgy: 0,
                    hrx: 4 << 8,
                    hry: 0,
                    region: (20, 16),
                    pattern: (4, 4),
                    num_patterns: pats.len(),
                    template,
                    mmr,
                    enable_skip: false,
                },
            ));
            let stream = page_stream(
                20,
                16,
                &[segment(1, 16, &[], &dict), segment(2, 22, &[1], &region)],
            );
            let mut img = Image::new(20, 16);
            for m in 0..gh {
                for n in 0..gw {
                    draw(
                        &mut img,
                        &pats[values[m * gw + n] as usize],
                        (n * 4) as i32,
                        (m * 4) as i32,
                    );
                }
            }
            assert_eq!(
                decode_page(&stream, 20, 16),
                expected(&img),
                "mmr {mmr} gabarit {template}"
            );
        }
    }
}

#[test]
fn halftone_region_with_rotated_grid_and_skip() {
    let pats = patterns();
    let (gw, gh) = (6usize, 6usize);
    let values: Vec<u32> = (0..gw * gh).map(|i| (i * 5 % 4) as u32).collect();
    // Grille tournée : vecteur (3, 2) en pixels, origine décalée, certaines
    // cellules hors région (HENSKIP).
    let (hgx, hgy, hrx, hry) = (-2 << 8, 1 << 8, 3u16 << 8, 2u16 << 8);
    for enable_skip in [false, true] {
        let dict = pattern_dict_data(&pats, 0, false);
        let mut region = region_info(16, 16, 0, 0, 0);
        region.extend_from_slice(&halftone_region_data(
            &values,
            HalftoneParams {
                gw,
                gh,
                hgx,
                hgy,
                hrx,
                hry,
                region: (16, 16),
                pattern: (4, 4),
                num_patterns: pats.len(),
                template: 0,
                mmr: false,
                enable_skip,
            },
        ));
        let stream = page_stream(
            16,
            16,
            &[segment(1, 16, &[], &dict), segment(2, 22, &[1], &region)],
        );
        let mut img = Image::new(16, 16);
        for m in 0..gh as i32 {
            for n in 0..gw as i32 {
                let x = (hgx + m * i32::from(hry) + n * i32::from(hrx)) >> 8;
                let y = (hgy + m * i32::from(hrx) - n * i32::from(hry)) >> 8;
                draw(
                    &mut img,
                    &pats[values[(m as usize) * gw + n as usize] as usize],
                    x,
                    y,
                );
            }
        }
        assert_eq!(
            decode_page(&stream, 16, 16),
            expected(&img),
            "skip {enable_skip}"
        );
    }
}

// ---------------------------------------------------------------------------
// Robustesse
// ---------------------------------------------------------------------------

fn composite_stream() -> Vec<u8> {
    let symbols = alphabet();
    let instances = sample_instances();
    let dict = symbol_dict_data(&symbols, 0, 0);
    let mut region = region_info(24, 26, 0, 0, 0);
    region.extend_from_slice(&text_region_data(
        &instances,
        &symbols,
        Layout::default(),
        false,
    ));
    let generic = generic_region_data(&glyphs(), 5, 15, 0, NOMINAL_AT, true);
    let pats = patterns();
    let mut halftone = region_info(20, 16, 0, 0, 0);
    let values: Vec<u32> = (0..20).map(|i| (i % 4) as u32).collect();
    halftone.extend_from_slice(&halftone_region_data(
        &values,
        HalftoneParams {
            gw: 5,
            gh: 4,
            hgx: 0,
            hgy: 0,
            hrx: 4 << 8,
            hry: 0,
            region: (20, 16),
            pattern: (4, 4),
            num_patterns: 4,
            template: 0,
            mmr: false,
            enable_skip: false,
        },
    ));
    page_stream(
        24,
        26,
        &[
            segment(1, 0, &[], &dict),
            segment(2, 6, &[1], &region),
            segment(3, 38, &[], &generic),
            segment(4, 16, &[], &pattern_dict_data(&pats, 0, false)),
            segment(5, 22, &[4], &halftone),
            segment(6, 39, &[], &generic_region_mmr_data(&glyphs(), 0, 0, true)),
        ],
    )
}

#[test]
fn every_truncation_length_never_panics() {
    let stream = composite_stream();
    for len in 0..stream.len() {
        if let Ok(out) = decode(&stream[..len], None, 24, 26) {
            assert_eq!(out.len(), 3 * 26);
        }
    }
}

#[test]
fn random_mutations_never_panic() {
    let stream = composite_stream();
    let mut rng = Lcg(77);
    for _ in 0..400 {
        let mut data = stream.clone();
        for _ in 0..=(rng.next() % 5) {
            let i = (rng.next() as usize) % data.len();
            data[i] ^= 1 << (rng.next() % 8);
        }
        if let Ok(out) = decode(&data, None, 24, 26) {
            assert_eq!(out.len(), 3 * 26);
        }
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Lcg(5);
    for _ in 0..300 {
        let len = (rng.next() % 300) as usize;
        let data: Vec<u8> = (0..len).map(|_| (rng.next() & 0xFF) as u8).collect();
        let _ = decode(&data, None, 1 + rng.next() % 50, 1 + rng.next() % 50);
        let _ = decode(&data, Some(&data), 8, 8);
    }
}

#[test]
fn invalid_dimensions_and_empty_input() {
    assert!(matches!(decode(&[], None, 0, 5), Err(Error::Corrupt(_))));
    assert!(matches!(decode(&[], None, 5, 5), Err(Error::Corrupt(_))));
    assert!(matches!(
        decode(&[0; 32], None, 1 << 20, 1 << 20),
        Err(Error::Corrupt(_))
    ));
}

#[test]
fn oversized_region_is_rejected_without_allocation() {
    // Région de 2^30 × 2^20 sur une page de 8 × 8 : la hauteur est coupée à
    // la page (8 lignes) mais 2^30 × 8 dépasse la limite → refusée, page
    // intacte.
    let mut data = region_info(1 << 30, 1 << 20, 0, 0, 0);
    data.push(0);
    data.extend_from_slice(&[3, 0xFF, 0xFD, 0xFF, 2, 0xFE, 0xFE, 0xFE]);
    data.extend_from_slice(&[0xFF; 16]);
    let stream = page_stream(8, 8, &[segment(1, 38, &[], &data)]);
    assert_eq!(decode_page(&stream, 8, 8), vec![0xFF; 8]);
}

// ---------------------------------------------------------------------------
// Performance (à lancer en release : cargo test --release -p acrux-codecs -- --ignored jbig2)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "mesure de performance, à lancer en release"]
fn a4_300dpi_generic_and_text_regions_decode_quickly() {
    let (w, h) = (2480usize, 3508usize);
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
    let data = generic_region_data(&img, 0, 0, 0, NOMINAL_AT, true);
    let stream = page_stream(w as u32, h as u32, &[segment(1, 38, &[], &data)]);
    let start = std::time::Instant::now();
    let out = decode_page(&stream, w, h);
    println!(
        "A4 300 dpi région générique (gabarit 0, TPGDON) : {} octets décodés en {:?}",
        stream.len(),
        start.elapsed()
    );
    assert_eq!(out, expected(&img));

    // Page de texte : dictionnaire de 3 symboles, ~20 000 instances.
    let symbols = alphabet();
    let mut instances = Vec::new();
    let mut rng = Lcg(8);
    for row in 0..(h / 14) {
        let mut x = 5i32;
        while x < w as i32 - 10 {
            let id = (rng.next() % 3) as usize;
            instances.push(Instance {
                x,
                y: (row * 14) as i32 + 2,
                id,
                refined: None,
            });
            x += symbols[id].width as i32 + 1;
        }
    }
    let dict = symbol_dict_data(&symbols, 0, 0);
    let mut region = region_info(w as u32, h as u32, 0, 0, 0);
    region.extend_from_slice(&text_region_data(
        &instances,
        &symbols,
        Layout::default(),
        false,
    ));
    let stream = page_stream(
        w as u32,
        h as u32,
        &[segment(1, 0, &[], &dict), segment(2, 6, &[1], &region)],
    );
    let start = std::time::Instant::now();
    let out = decode_page(&stream, w, h);
    println!(
        "A4 300 dpi région de texte ({} instances) : {} octets décodés en {:?}",
        instances.len(),
        stream.len(),
        start.elapsed()
    );
    let img = render_instances(w, h, &instances, &symbols);
    assert_eq!(out, expected(&img));
}

// ---------------------------------------------------------------------------
// Échantillons de corpus
// (cargo test -p acrux-codecs --release -- --ignored generate_corpus)
// ---------------------------------------------------------------------------

/// Texte pour une région de texte : un symbole (glyphe 15 × 21) par lettre
/// distincte de `words`, et une instance par lettre, un mot par ligne.
fn corpus_text(words: &[&str]) -> (Vec<Image>, Vec<Instance>) {
    use crate::ccitt::tests::corpus;
    let scale = 3;
    let mut letters: Vec<char> = words.iter().flat_map(|w| w.chars()).collect();
    letters.sort_unstable();
    letters.dedup();
    // Tous les symboles ont la même taille : l'ordre (hauteur, largeur) du
    // dictionnaire est respecté quel que soit l'ordre des lettres.
    let symbols: Vec<Image> = letters
        .iter()
        .map(|&c| {
            let mut s = Image::new(5 * scale, 7 * scale);
            corpus::draw_text(&mut s, 0, 0, &c.to_string(), scale);
            s
        })
        .collect();
    let mut instances = Vec::new();
    for (line, word) in words.iter().enumerate() {
        for (i, c) in word.chars().enumerate() {
            instances.push(Instance {
                x: (10 + i * 6 * scale) as i32,
                y: (10 + line * 36) as i32,
                id: letters.iter().position(|&l| l == c).unwrap(),
                refined: None,
            });
        }
    }
    (symbols, instances)
}

/// Écrit dans `target/corpus-gen/` les flux JBIG2 embarqués (ISO 32000-2
/// §7.4.7 : suite de segments sans en-tête de fichier) du corpus de synthèse
/// et leurs images source ; voir `crates/acrux-render/tests/synthese_codecs.rs`.
#[test]
#[ignore = "génération du corpus : cargo test -p acrux-codecs --release -- --ignored generate_corpus"]
fn generate_corpus_samples() {
    use crate::ccitt::tests::corpus::{self, SAMPLE_HEIGHT, SAMPLE_WIDTH};
    let (w, h) = (SAMPLE_WIDTH as u32, SAMPLE_HEIGHT as u32);

    // 1. Région générique immédiate, codage arithmétique, gabarit 0, TPGDON.
    let img = corpus::bilevel_sample("JBIG2 GEN");
    let data = generic_region_data(&img, 0, 0, 0, NOMINAL_AT, true);
    let stream = page_stream(w, h, &[segment(1, 38, &[], &data)]);
    assert_eq!(
        decode_page(&stream, SAMPLE_WIDTH, SAMPLE_HEIGHT),
        expected(&img)
    );
    corpus::write("jbig2-generic.bin", &stream);
    corpus::write_png_bilevel("jbig2-generic.png", &img);

    // 2. Dictionnaire de symboles puis région de texte dans le même flux.
    let (symbols, instances) = corpus_text(&["JBIG2", "TEXT", "REGION"]);
    let mut region = region_info(w, h, 0, 0, 0);
    region.extend_from_slice(&text_region_data(
        &instances,
        &symbols,
        Layout::default(),
        false,
    ));
    let stream = page_stream(
        w,
        h,
        &[
            segment(1, 0, &[], &symbol_dict_data(&symbols, 0, 0)),
            segment(2, 6, &[1], &region),
        ],
    );
    let img = render_instances(SAMPLE_WIDTH, SAMPLE_HEIGHT, &instances, &symbols);
    assert_eq!(
        decode_page(&stream, SAMPLE_WIDTH, SAMPLE_HEIGHT),
        expected(&img)
    );
    corpus::write("jbig2-text.bin", &stream);
    corpus::write_png_bilevel("jbig2-text.png", &img);

    // 3. Dictionnaire dans un flux /JBIG2Globals séparé, région dans la page.
    let (symbols, instances) = corpus_text(&["GLOBALS", "DICT", "JBIG2"]);
    let globals = segment(1, 0, &[], &symbol_dict_data(&symbols, 0, 0));
    let mut region = region_info(w, h, 0, 0, 0);
    region.extend_from_slice(&text_region_data(
        &instances,
        &symbols,
        Layout::default(),
        false,
    ));
    let stream = page_stream(w, h, &[segment(2, 6, &[1], &region)]);
    let img = render_instances(SAMPLE_WIDTH, SAMPLE_HEIGHT, &instances, &symbols);
    assert_eq!(
        decode(&stream, Some(&globals), w, h).unwrap(),
        expected(&img)
    );
    corpus::write("jbig2-globals.bin", &stream);
    corpus::write("jbig2-globals.globals.bin", &globals);
    corpus::write_png_bilevel("jbig2-globals.png", &img);
}
