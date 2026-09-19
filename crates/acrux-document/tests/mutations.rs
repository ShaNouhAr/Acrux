//! Fuzzing léger et déterministe : chaque PDF du corpus est corrompu de
//! nombreuses façons (octets modifiés, tronqués, dupliqués, insérés) et le
//! moteur doit toujours répondre sans paniquer, sans boucler et sans
//! exploser en mémoire. Un vrai fuzzer guidé viendra en complément
//! (voir tests/fuzz/README.md), mais ce test tourne partout, sans outil.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::range_plus_one,
    clippy::expect_used
)] // code de test

use std::path::{Path, PathBuf};

use acrux_document::{collect_pages, Document, Lexer, Object, ObjectRef, Parser, Token};

/// xorshift64* : reproductible d'une exécution à l'autre.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            usize::try_from(self.next() % n as u64).unwrap_or(0)
        }
    }
}

fn corpus_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == "private") {
                    continue;
                }
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pdf")) {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("corpus"),
        &mut files,
    );
    files
}

/// Applique une mutation aléatoire.
fn mutate(data: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut d = data.to_vec();
    if d.is_empty() {
        return d;
    }
    match rng.below(8) {
        0 => {
            // Quelques octets remplacés.
            for _ in 0..1 + rng.below(16) {
                let i = rng.below(d.len());
                d[i] = (rng.next() & 0xFF) as u8;
            }
        }
        1 => {
            // Troncature.
            let at = rng.below(d.len());
            d.truncate(at);
        }
        2 => {
            // Suppression d'un bloc.
            let a = rng.below(d.len());
            let len = rng.below(64.min(d.len() - a));
            d.drain(a..a + len);
        }
        3 => {
            // Duplication d'un bloc.
            let a = rng.below(d.len());
            let len = rng.below(256.min(d.len() - a));
            let chunk = d[a..a + len].to_vec();
            let at = rng.below(d.len());
            d.splice(at..at, chunk);
        }
        4 => {
            // Insertion d'octets aléatoires.
            let at = rng.below(d.len());
            let junk: Vec<u8> = (0..rng.below(32))
                .map(|_| (rng.next() & 0xFF) as u8)
                .collect();
            d.splice(at..at, junk);
        }
        5 => {
            // Chiffres d'un offset xref changés.
            for _ in 0..4 {
                let i = rng.below(d.len());
                if d[i].is_ascii_digit() {
                    d[i] = b'0' + (rng.next() % 10) as u8;
                }
            }
        }
        6 => {
            // Mots-clés structurels effacés.
            for kw in [
                &b"endobj"[..],
                b"endstream",
                b"xref",
                b"trailer",
                b"startxref",
                b" obj",
            ] {
                if rng.below(3) == 0 {
                    if let Some(pos) = d.windows(kw.len()).position(|w| w == kw) {
                        for b in &mut d[pos..pos + kw.len()] {
                            *b = b' ';
                        }
                    }
                }
            }
        }
        _ => {
            // Bits inversés.
            for _ in 0..1 + rng.below(8) {
                let i = rng.below(d.len());
                d[i] ^= 1 << rng.below(8);
            }
        }
    }
    d
}

/// Exerce tout ce que l'application ferait à l'ouverture.
fn exercise(bytes: Vec<u8>) {
    let Ok(doc) = Document::from_bytes(bytes) else {
        return;
    };
    let _ = doc.version();
    let _ = doc.catalog();
    let _ = doc.info();
    if let Ok(pages) = collect_pages(&doc) {
        for p in pages.iter().take(50) {
            let _ = p.media_box(&doc);
            let _ = p.crop_box(&doc);
            let _ = p.rotate(&doc);
        }
    }
    for n in doc.object_numbers().into_iter().take(500) {
        let r = ObjectRef {
            number: n,
            generation: 0,
        };
        if let Ok(o) = doc.get(r) {
            if matches!(&*o, Object::Stream { .. }) {
                let _ = doc.stream_data(&o);
            }
        }
    }
    let _ = doc.save_full();
    let _ = doc.save_incremental();
}

#[test]
fn mutated_corpus_never_panics() {
    let files = corpus_files();
    assert!(!files.is_empty());
    let rounds: usize = std::env::var("ACRUX_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    for path in &files {
        let data = std::fs::read(path).expect("lecture du corpus");
        for _ in 0..rounds {
            let mutated = mutate(&data, &mut rng);
            // Deux mutations successives une fois sur trois.
            let mutated = if rng.below(3) == 0 {
                mutate(&mutated, &mut rng)
            } else {
                mutated
            };
            exercise(mutated);
        }
    }
}

#[test]
fn random_bytes_never_panic_lexer_and_parser() {
    let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
    for _ in 0..2000 {
        let len = rng.below(200);
        let alphabet = b"0123456789.-+ ()<>[]{}/%\\#\n\r\tobjRstreamendtrufalsn\x00\xff";
        let data: Vec<u8> = (0..len)
            .map(|_| {
                if rng.below(4) == 0 {
                    (rng.next() & 0xFF) as u8
                } else {
                    alphabet[rng.below(alphabet.len())]
                }
            })
            .collect();
        let mut lx = Lexer::new(&data);
        for _ in 0..1000 {
            match lx.next_token() {
                Ok(Token::Eof) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let mut p = Parser::new(&data);
        for _ in 0..100 {
            if p.parse_object().is_err() && p.pos() >= data.len() {
                break;
            }
        }
        let mut p = Parser::new(&data);
        let _ = p.parse_indirect(&|_| None);
        let _ = Document::from_bytes(data);
    }
}
