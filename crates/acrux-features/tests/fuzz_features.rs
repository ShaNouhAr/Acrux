//! Fuzzing déterministe des étages hauts : rendu, extraction de texte,
//! annotations, formulaires, calques, export et vérifications.
//!
//! `acrux-document/tests/mutations.rs` couvre déjà l'analyse d'un fichier
//! corrompu ; ici on va plus loin, jusqu'à dessiner la page et à en extraire
//! le texte, parce que c'est ce que fait l'application dès qu'on lui ouvre un
//! document. Un fichier abîmé doit toujours produire une erreur ou un rendu
//! partiel, jamais une panique, une boucle sans fin ou une explosion mémoire.
//!
//! Le nombre de tours se règle avec `ACRUX_FUZZ_ROUNDS` (12 par défaut, de quoi
//! rester sous la minute ; 200 pour une passe sérieuse avant publication).

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::expect_used,
    clippy::unwrap_used
)] // code de test

use std::path::{Path, PathBuf};

use acrux_document::{collect_pages, Document};
use acrux_features::accessibility::check_accessibility;
use acrux_features::annotations::list_annotations;
use acrux_features::compare::{compare_documents, CompareOptions};
use acrux_features::export::{export_markdown, export_text};
use acrux_features::forms::list_fields;
use acrux_features::text::extract_page_text;
use acrux_graphics::Color;
use acrux_render::{layers, render_page, RenderOptions};

/// xorshift64* : la même suite d'une exécution à l'autre, donc un échec est
/// reproductible.
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
            (self.next() % n as u64) as usize
        }
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn corpus_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ["reels", "synthese"] {
        let path = repo_root().join("tests").join("corpus").join(dir);
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "pdf") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Mot de passe porté par le nom du fichier (convention `…-mdp-<mot>.pdf`).
fn password_of(path: &Path) -> Option<Vec<u8>> {
    let name = path.file_stem()?.to_string_lossy().into_owned();
    let at = name.find("-mdp-")?;
    Some(name.as_bytes()[at + 5..].to_vec())
}

/// Une corruption au hasard : octet changé, bloc tronqué, bloc dupliqué,
/// octets insérés, ou un nombre remplacé par une valeur extrême.
fn mutate(data: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut out = data.to_vec();
    if out.is_empty() {
        return out;
    }
    match rng.below(5) {
        0 => {
            let at = rng.below(out.len());
            out[at] = (rng.next() & 0xFF) as u8;
        }
        1 => {
            let keep = rng.below(out.len());
            out.truncate(keep);
        }
        2 => {
            let at = rng.below(out.len());
            let len = rng.below(64).min(out.len() - at);
            let block: Vec<u8> = out[at..at + len].to_vec();
            out.splice(at..at, block);
        }
        3 => {
            let at = rng.below(out.len());
            let junk: Vec<u8> = (0..rng.below(32))
                .map(|_| (rng.next() & 0xFF) as u8)
                .collect();
            out.splice(at..at, junk);
        }
        _ => {
            // Un chiffre remplacé par un autre : les longueurs, les décalages
            // et les tailles de police prennent alors des valeurs absurdes.
            for _ in 0..8 {
                let at = rng.below(out.len());
                if out[at].is_ascii_digit() {
                    out[at] = b'0' + (rng.next() % 10) as u8;
                    break;
                }
            }
        }
    }
    out
}

/// Tout ce que l'application fait d'un document ouvert. Aucun résultat n'est
/// vérifié : seul compte le fait de revenir.
fn exercise(bytes: Vec<u8>, password: Option<&[u8]>) {
    let Ok(doc) = Document::from_bytes(bytes) else {
        return;
    };
    if let Some(pw) = password {
        let _ = doc.authenticate(pw);
    }
    let _ = layers(&doc);
    let _ = list_fields(&doc);
    let _ = check_accessibility(&doc);
    let _ = export_text(&doc);
    let _ = export_markdown(&doc);
    let Ok(pages) = collect_pages(&doc) else {
        return;
    };
    let options = RenderOptions {
        // Un budget court : une page absurde ne doit pas retenir le test.
        time_budget: Some(std::time::Duration::from_secs(5)),
        background: Some(Color::WHITE),
        ..RenderOptions::default()
    };
    for page in pages.iter().take(3) {
        let _ = extract_page_text(&doc, page);
        let _ = list_annotations(&doc, page);
        // 36 dpi : assez pour parcourir tout l'interpréteur, assez petit pour
        // que des dimensions délirantes ne remplissent pas la mémoire.
        let _ = render_page(&doc, page, 0.5, &options);
    }
}

#[test]
fn mutated_documents_never_panic_when_rendered_or_read() {
    let files = corpus_files();
    assert!(!files.is_empty(), "corpus introuvable");
    let rounds: usize = std::env::var("ACRUX_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12);
    let mut rng = Rng(0x0BAD_C0DE_1234_5678);
    for path in &files {
        let data = std::fs::read(path).expect("lecture du corpus");
        let password = password_of(path);
        for _ in 0..rounds {
            let mutated = mutate(&data, &mut rng);
            let mutated = if rng.below(3) == 0 {
                mutate(&mutated, &mut rng)
            } else {
                mutated
            };
            exercise(mutated, password.as_deref());
        }
    }
}

#[test]
fn comparing_a_document_with_its_mutation_never_panics() {
    let files = corpus_files();
    let mut rng = Rng(0xFEED_FACE_0000_0001);
    let options = CompareOptions {
        visual: None,
        ..CompareOptions::default()
    };
    for path in files.iter().take(6) {
        let data = std::fs::read(path).expect("lecture du corpus");
        let Ok(original) = Document::from_bytes(data.clone()) else {
            continue;
        };
        for _ in 0..3 {
            let Ok(other) = Document::from_bytes(mutate(&data, &mut rng)) else {
                continue;
            };
            let _ = compare_documents(&original, &other, &options);
        }
    }
}
