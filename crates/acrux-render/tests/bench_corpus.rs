//! Mesure de performance du rendu sur le corpus, à lancer à la main :
//!
//! ```text
//! cargo test -p acrux-render --release --test bench_corpus -- --ignored --nocapture
//! ```
//!
//! Le test n'échoue que si une page dépasse largement le budget que se donne
//! le projet (une page A4 typique doit se rendre en moins de 100 ms à 150 dpi
//! sur une machine de bureau) : il documente les temps plutôt que de les
//! figer, pour ne pas rendre la suite fragile sur une machine chargée.

// Moyennes affichées : le nombre de pages tient largement dans un f64.
#![allow(clippy::cast_precision_loss)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use acrux_document::{collect_pages, Document};
use acrux_render::{render_page, RenderOptions};

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

/// Mot de passe d'un fichier de corpus (convention `-mdp-<mot de passe>.pdf`).
fn corpus_password(path: &Path) -> Option<Vec<u8>> {
    let stem = path.file_stem()?.to_str()?;
    let (_, pw) = stem.rsplit_once("-mdp-")?;
    Some(pw.as_bytes().to_vec())
}

#[test]
#[ignore = "mesure de performance : à lancer à la main en release"]
fn render_times_per_page() {
    let dpi = 150.0;
    let scale = dpi / 72.0;
    let options = RenderOptions {
        annotations: true,
        time_budget: None,
        background: Some(acrux_graphics::Color::WHITE),
        ..RenderOptions::default()
    };
    let mut slowest: Vec<(f64, String)> = Vec::new();
    let mut total_ms = 0.0;
    let mut pages_done = 0usize;
    for pdf in corpus_files() {
        let Ok(doc) = Document::load(&pdf) else {
            continue;
        };
        if let Some(pw) = corpus_password(&pdf) {
            if doc.authenticate(&pw).is_err() {
                continue;
            }
        }
        let Ok(pages) = collect_pages(&doc) else {
            continue;
        };
        let name = pdf
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut file_ms = 0.0;
        for (i, page) in pages.iter().enumerate() {
            let start = Instant::now();
            let out = render_page(&doc, page, scale, &options);
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            file_ms += ms;
            total_ms += ms;
            pages_done += 1;
            let mega = f64::from(out.bitmap.width()) * f64::from(out.bitmap.height()) / 1e6;
            slowest.push((ms, format!("{name} p{} ({mega:.1} Mpx)", i + 1)));
        }
        println!(
            "{name:<62} {:>3} page(s)  {file_ms:>8.1} ms  ({:.1} ms/page)",
            pages.len(),
            file_ms / pages.len().max(1) as f64
        );
    }
    slowest.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!("\nles plus lentes à {dpi:.0} dpi :");
    for (ms, what) in slowest.iter().take(8) {
        println!("  {ms:>8.1} ms  {what}");
    }
    println!(
        "\n{pages_done} pages en {total_ms:.0} ms ({:.1} ms/page en moyenne)",
        total_ms / pages_done.max(1) as f64
    );
    // Garde-fou très large : signale une régression d'un ordre de grandeur.
    if let Some((ms, what)) = slowest.first() {
        assert!(
            *ms < 3000.0,
            "page beaucoup trop lente : {what} en {ms:.0} ms"
        );
    }
}
