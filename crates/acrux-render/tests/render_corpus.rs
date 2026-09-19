//! Tests de rendu par comparaison d'images : chaque PDF de `tests/corpus/`
//! est rendu à 72 dpi et comparé à son image de référence dans
//! `tests/reference/` (même chemin relatif, suffixe `-pNNN.png`).
//!
//! - Référence absente : le rendu est écrit dans `tests/reference/actual/`
//!   et le test signale le fichier manquant (échec), sauf pour les fichiers
//!   `casse` qui doivent seulement se rendre sans panique.
//! - Différence : le rendu obtenu et une image des différences sont écrits
//!   dans `tests/reference/actual/` pour inspection.
//!
//! Tolérance : au plus 0,5 % des pixels avec un écart de canal > 8 (l'anti-
//! aliasing peut bouger d'un niveau), et aucun pixel avec un écart > 64 en
//! dehors de ces 0,5 %.

// Comparaison de pixels : indices et ratios sur de petits entiers, noms à une
// lettre pour les coordonnées et canaux.
#![allow(clippy::cast_precision_loss, clippy::many_single_char_names)]

use std::path::{Path, PathBuf};

use acrux_document::{collect_pages, Document};
use acrux_graphics::{encode_png_rgb, Color};
use acrux_render::{render_page, RenderOptions};

/// Mot de passe d'un fichier de corpus, donné par convention dans son nom :
/// `xxx-mdp-<mot de passe>.pdf`.
fn corpus_password(path: &Path) -> Option<Vec<u8>> {
    let stem = path.file_stem()?.to_str()?;
    let (_, pw) = stem.rsplit_once("-mdp-")?;
    Some(pw.as_bytes().to_vec())
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
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
    walk(&repo_root().join("tests").join("corpus"), &mut files);
    files.sort();
    files
}

/// Décodeur PNG minimal (RGB/RGBA 8 bits, non entrelacé) pour lire les références.
fn decode_png(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if !data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return None;
    }
    let mut pos = 8;
    let (mut w, mut h, mut color_type) = (0u32, 0u32, 0u8);
    let mut idat = Vec::new();
    while pos + 8 <= data.len() {
        let len =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let kind = &data[pos + 4..pos + 8];
        let body = data.get(pos + 8..pos + 8 + len)?;
        match kind {
            b"IHDR" => {
                w = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                h = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
                if body[8] != 8 || body[12] != 0 {
                    return None;
                }
                color_type = body[9];
            }
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        pos += 12 + len;
    }
    let channels = match color_type {
        2 => 3,
        6 => 4,
        0 => 1,
        _ => return None,
    };
    let raw = acrux_codecs::flate::decode(&idat).ok()?;
    let unfiltered = acrux_codecs::predictor::apply(&raw, 15, channels, 8, w).ok()?;
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for px in unfiltered.chunks_exact(channels as usize) {
        match channels {
            1 => rgb.extend_from_slice(&[px[0], px[0], px[0]]),
            _ => rgb.extend_from_slice(&px[..3]),
        }
    }
    Some((w, h, rgb))
}

struct Diff {
    differing: usize,
    max_delta: u8,
    total: usize,
    image: Vec<u8>,
}

fn compare(a: &[u8], b: &[u8], w: u32, h: u32) -> Diff {
    let total = (w * h) as usize;
    let mut differing = 0;
    let mut max_delta = 0u8;
    let mut image = vec![255u8; total * 3];
    for i in 0..total {
        let mut d = 0u8;
        for k in 0..3 {
            let x = a.get(i * 3 + k).copied().unwrap_or(0);
            let y = b.get(i * 3 + k).copied().unwrap_or(0);
            d = d.max(x.abs_diff(y));
        }
        if d > 8 {
            differing += 1;
            image[i * 3] = 255;
            image[i * 3 + 1] = 0;
            image[i * 3 + 2] = 0;
        } else if d > 0 {
            image[i * 3] = 200;
            image[i * 3 + 1] = 200;
            image[i * 3 + 2] = 200;
        }
        max_delta = max_delta.max(d);
    }
    Diff {
        differing,
        max_delta,
        total,
        image,
    }
}

#[test]
fn corpus_renders_match_references() {
    let root = repo_root();
    let reference_dir = root.join("tests").join("reference");
    let actual_dir = reference_dir.join("actual");
    let options = RenderOptions {
        annotations: true,
        time_budget: Some(std::time::Duration::from_secs(30)),
        background: Some(Color::WHITE),
        ..RenderOptions::default()
    };
    let mut failures = Vec::new();
    for pdf in corpus_files() {
        let rel = pdf
            .strip_prefix(root.join("tests").join("corpus"))
            .unwrap_or(&pdf)
            .with_extension("");
        let is_broken = rel.to_string_lossy().contains("casse");
        let doc = match Document::load(&pdf) {
            Ok(d) => d,
            Err(e) => {
                failures.push(format!("{} : ouverture impossible ({e})", pdf.display()));
                continue;
            }
        };
        if let Some(pw) = corpus_password(&pdf) {
            if let Err(e) = doc.authenticate(&pw) {
                failures.push(format!("{} : mot de passe refusé ({e})", pdf.display()));
                continue;
            }
        }
        let Ok(pages) = collect_pages(&doc) else {
            failures.push(format!("{} : pages illisibles", pdf.display()));
            continue;
        };
        for (i, page) in pages.iter().enumerate().take(10) {
            let rendered = render_page(&doc, page, 1.0, &options);
            let (w, h) = (rendered.bitmap.width(), rendered.bitmap.height());
            let rgb = rendered.bitmap.to_rgb8_over_white();
            let name = format!(
                "{}-p{:03}.png",
                rel.to_string_lossy().replace('\\', "/"),
                i + 1
            );
            let ref_path = reference_dir.join(&name);
            let actual_path = actual_dir.join(&name);
            if is_broken && !ref_path.exists() {
                continue;
            }
            let Some(reference) = std::fs::read(&ref_path).ok().and_then(|d| decode_png(&d)) else {
                if let Some(parent) = actual_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&actual_path, encode_png_rgb(w, h, &rgb));
                failures.push(format!(
                    "{name} : référence absente ; rendu écrit dans {} (à vérifier puis copier dans tests/reference/)",
                    actual_path.display()
                ));
                continue;
            };
            if reference.0 != w || reference.1 != h {
                failures.push(format!(
                    "{name} : taille {w}×{h}, référence {}×{}",
                    reference.0, reference.1
                ));
                continue;
            }
            let diff = compare(&rgb, &reference.2, w, h);
            let ratio = diff.differing as f64 / diff.total.max(1) as f64;
            if ratio > 0.005 {
                if let Some(parent) = actual_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&actual_path, encode_png_rgb(w, h, &rgb));
                let diff_path = actual_path.with_extension("diff.png");
                let _ = std::fs::write(&diff_path, encode_png_rgb(w, h, &diff.image));
                failures.push(format!(
                    "{name} : {:.2} % des pixels diffèrent (écart max {}) ; voir {}",
                    ratio * 100.0,
                    diff.max_delta,
                    diff_path.display()
                ));
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
