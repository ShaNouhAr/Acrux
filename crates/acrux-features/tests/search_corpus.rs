//! La recherche sur les fichiers du corpus (`tests/corpus/reels` et
//! `synthese`), en lecture seule.
//!
//! Ce que vérifient ces tests, page par page et sur quelques mots tirés du
//! texte de chaque page :
//!
//! - chaque morceau d'occurrence a une boîte véritable, posée sur la page ;
//! - respecter la casse ou chercher le mot entier ne trouve jamais **plus**
//!   que la recherche par défaut ;
//! - les plages éditables (`find_ranges`) sont exactement les occurrences
//!   d'une ligne de la recherche, dans le même ordre ;
//! - les deux mots qui se suivent de part et d'autre d'une fin de ligne,
//!   dans un paragraphe, se trouvent ensemble : la jonction des lignes marche
//!   sur du vrai texte, pas seulement sur des pages fabriquées ;
//! - rien ne panique, sur aucun fichier.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use acrux_document::{collect_pages, Document};
use acrux_features::edit_text::{find_ranges, find_ranges_with, TextRange};
use acrux_features::text::{extract_page_text, find_matches, PageText, SearchOptions, TextMatch};

fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join(name)
}

fn corpus_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ["reels", "synthese"] {
        let Ok(entries) = std::fs::read_dir(corpus(dir)) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "pdf") && !p.to_string_lossy().contains("mdp-") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Les pages lisibles d'un fichier, texte extrait.
fn pages(path: &Path) -> Vec<PageText> {
    let Ok(doc) = Document::load(path) else {
        return Vec::new();
    };
    let Ok(pages) = collect_pages(&doc) else {
        return Vec::new();
    };
    pages
        .iter()
        .filter_map(|p| extract_page_text(&doc, p).ok())
        .collect()
}

/// Quelques mots d'une page : ceux d'au moins quatre lettres, un sur trois,
/// au plus huit.
fn sample_words(text: &PageText) -> Vec<String> {
    text.lines
        .iter()
        .flat_map(|l| l.words.iter())
        .map(|w| {
            w.text
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_string()
        })
        .filter(|w| w.chars().count() >= 4 && w.chars().all(char::is_alphanumeric))
        .step_by(3)
        .take(8)
        .collect()
}

fn single_ranges(matches: &[TextMatch]) -> Vec<TextRange> {
    matches
        .iter()
        .filter_map(TextMatch::single_line)
        .map(|p| TextRange {
            line: p.line,
            start: p.start,
            end: p.end,
        })
        .collect()
}

#[test]
fn chaque_occurrence_est_sur_sa_page_et_les_options_restreignent() {
    let files = corpus_files();
    assert!(!files.is_empty(), "corpus introuvable");
    let case = SearchOptions {
        match_case: true,
        whole_word: false,
    };
    let word = SearchOptions {
        match_case: false,
        whole_word: true,
    };
    let mut searched = 0;
    for path in files {
        for text in pages(&path) {
            let page = text.page_box;
            for w in sample_words(&text) {
                let found = find_matches(&text, &w, SearchOptions::default());
                assert!(
                    !found.is_empty(),
                    "{} : « {w} », tiré de la page, introuvable",
                    path.display()
                );
                for piece in found.iter().flat_map(|m| m.pieces.iter()) {
                    let r = piece.rect;
                    assert!(
                        r.width() > 0.0 && r.height() > 0.0 && r.x0.is_finite() && r.y1.is_finite(),
                        "{} : « {w} », boîte vide {r:?}",
                        path.display()
                    );
                    assert!(
                        piece.start < piece.end,
                        "{} : « {w} », plage vide",
                        path.display()
                    );
                    let line = &text.lines[piece.line];
                    let glyphs: usize = line.words.iter().map(|w| w.glyphs.len()).sum();
                    assert!(
                        piece.end <= glyphs,
                        "{} : plage hors de la ligne",
                        path.display()
                    );
                    if !page.is_empty() {
                        // Posée sur la page, à un point près : la boîte d'un
                        // glyphe est approchée (avance × corps).
                        let margin = acrux_core::Rect::new(
                            page.x0 - 1.0,
                            page.y0 - 1.0,
                            page.x1 + 1.0,
                            page.y1 + 1.0,
                        );
                        assert!(
                            !r.intersect(&margin).is_empty(),
                            "{} : « {w} » hors de la page {r:?} / {page:?}",
                            path.display()
                        );
                    }
                }
                let n = found.len();
                assert!(find_matches(&text, &w, case).len() <= n);
                assert!(find_matches(&text, &w, word).len() <= n);
                assert_eq!(
                    find_ranges(&text, &w),
                    single_ranges(&found),
                    "{} : « {w} »",
                    path.display()
                );
                assert_eq!(
                    find_ranges_with(&text, &w, case),
                    single_ranges(&find_matches(&text, &w, case)),
                );
                searched += 1;
            }
        }
    }
    assert!(searched >= 100, "seulement {searched} recherche(s)");
}

#[test]
fn deux_mots_de_part_et_d_autre_d_une_fin_de_ligne_se_trouvent() {
    let mut joined = 0;
    for path in corpus_files() {
        for text in pages(&path) {
            for paragraph in text.blocks.iter().flat_map(|b| b.paragraphs.iter()) {
                for pair in paragraph.lines.windows(2) {
                    let (Some(a), Some(b)) = (text.lines.get(pair[0]), text.lines.get(pair[1]))
                    else {
                        continue;
                    };
                    let (Some(last), Some(first)) = (a.words.last(), b.words.first()) else {
                        continue;
                    };
                    // Un mot qui finit par un tiret se joint à la suite
                    // sans espace : il est vérifié par les tests unitaires.
                    if last.text.ends_with(['-', '\u{ad}', '\u{2010}', '\u{2011}'])
                        || last.text.chars().any(char::is_whitespace)
                        || first.text.chars().any(char::is_whitespace)
                    {
                        continue;
                    }
                    let needle = format!("{} {}", last.text, first.text);
                    let found = find_matches(&text, &needle, SearchOptions::default());
                    assert!(
                        found.iter().any(|m| m.pieces.len() == 2),
                        "{} : « {needle} » introuvable à cheval sur deux lignes",
                        path.display()
                    );
                    joined += 1;
                }
            }
        }
    }
    assert!(
        joined >= 20,
        "seulement {joined} fin(s) de ligne éprouvée(s)"
    );
}
