//! Combien de texte se laisse modifier, et pourquoi le reste refuse.
//!
//! Ce test parcourt le corpus, essaie d'ouvrir **chaque bloc** puis, quand le
//! bloc refuse, **chaque ligne** de ce bloc. Il échoue si la couverture
//! tombe : c'est la mesure de « je peux modifier ce que je vois ».
//!
//! Lancé avec `-- --nocapture`, il imprime le détail des refus, ce qui sert à
//! savoir quoi attaquer ensuite.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use acrux_document::{collect_pages, Document};
use acrux_features::edit_text::{line_unit, normalized, open_unit, set_paragraph_text, text_units};
use acrux_features::text::extract_page_text;

/// Tous les PDF du corpus, sans les fichiers volontairement corrompus.
fn corpus_files() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus");
    let mut out = Vec::new();
    for dir in ["reels", "synthese"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if path.extension().is_some_and(|e| e == "pdf") && !name.contains("casse") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Première phrase d'un message d'erreur, pour regrouper les refus.
fn reason(message: &str) -> String {
    let cut = message.find('«').unwrap_or(message.len());
    message[..cut].trim().trim_end_matches(':').to_string()
}

#[test]
fn presque_tout_le_texte_se_laisse_modifier() {
    let mut blocks = (0_usize, 0_usize);
    let mut lines = (0_usize, 0_usize);
    let mut refus: BTreeMap<String, usize> = BTreeMap::new();
    for path in corpus_files() {
        let Ok(doc) = Document::load(&path) else {
            continue;
        };
        let Ok(pages) = collect_pages(&doc) else {
            continue;
        };
        for page in pages.iter().take(2) {
            let Ok(text) = extract_page_text(&doc, page) else {
                continue;
            };
            for unit in text_units(&text) {
                blocks.1 += 1;
                match open_unit(&doc, page, &text, &unit) {
                    Ok(_) => blocks.0 += 1,
                    Err(e) => {
                        *refus.entry(reason(&e.to_string())).or_default() += 1;
                        // Le bloc refuse : ses lignes, une par une ?
                        for piece in &unit.pieces {
                            lines.1 += 1;
                            let ok = line_unit(&text, piece.line)
                                .is_some_and(|u| open_unit(&doc, page, &text, &u).is_ok());
                            if ok {
                                lines.0 += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    println!("blocs modifiables : {} / {}", blocks.0, blocks.1);
    println!(
        "lignes rattrapées dans les blocs refusés : {} / {}",
        lines.0, lines.1
    );
    for (cause, count) in &refus {
        println!("  {count:>4} × {cause}");
    }
    assert!(blocks.1 > 50, "corpus trop maigre : {} blocs", blocks.1);
    // Ce que l'on tient aujourd'hui ; ce seuil ne doit pas redescendre.
    // Le reste : des blocs en biais (filigranes) et quelques opérations qui
    // dessinent deux textes à la fois.
    let couverture = blocks.0 * 100 / blocks.1;
    assert!(
        couverture >= 93,
        "seulement {couverture} % des blocs se modifient"
    );
}

/// Taper dans un bloc, lettre après lettre, ne doit jamais être refusé en
/// cours de route : c'est exactement ce que fait l'éditeur.
#[test]
fn chaque_bloc_ouvert_se_tape_jusquau_bout() {
    let mut tries = 0;
    let mut refus: Vec<String> = Vec::new();
    for path in corpus_files() {
        let Ok(first) = Document::load(&path) else {
            continue;
        };
        let Ok(pages) = collect_pages(&first) else {
            continue;
        };
        let Ok(page_text) = extract_page_text(&first, &pages[0]) else {
            continue;
        };
        // Deux blocs par fichier suffisent : le test doit rester rapide.
        for unit in text_units(&page_text).into_iter().take(2) {
            // Chaque bloc part du fichier d'origine : un bloc recomposé peut
            // déborder sur son voisin, et l'on ne mesure pas cela ici.
            let Ok(doc) = Document::load(&path) else {
                continue;
            };
            let Ok(pages) = collect_pages(&doc) else {
                continue;
            };
            let Ok(text) = extract_page_text(&doc, &pages[0]) else {
                continue;
            };
            let Ok(opened) = open_unit(&doc, &pages[0], &text, &unit) else {
                continue;
            };
            tries += 1;
            let mut drawn = opened.drawn.clone();
            let mut typed = opened.text.clone();
            for c in " Xyz".chars() {
                typed.push(c);
                let Ok(pages) = collect_pages(&doc) else {
                    break;
                };
                match set_paragraph_text(&doc, &pages[0], &opened.frame, &drawn, &typed) {
                    Ok(_) => drawn = normalized(&typed),
                    Err(e) => {
                        refus.push(format!(
                            "{} : « {} » refusé après « {} » — {e}",
                            path.file_name().unwrap_or_default().to_string_lossy(),
                            c,
                            opened.text.chars().take(24).collect::<String>()
                        ));
                        break;
                    }
                }
            }
        }
    }
    println!("blocs tapés jusqu'au bout : {tries} essais");
    for r in &refus {
        println!("  {r}");
    }
    assert!(tries > 20, "corpus trop maigre : {tries} essais");
    assert!(
        refus.is_empty(),
        "{} frappes refusées en cours de saisie",
        refus.len()
    );
}
