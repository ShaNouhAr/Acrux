//! « Rechercher et remplacer » sur les fichiers réels du corpus.
//!
//! Le remplacement global d'Acrux n'est pas une réécriture du document : c'est
//! la même édition chirurgicale que la modification de texte, appliquée à
//! toutes les occurrences d'un coup. Ce que vérifient ces tests :
//!
//! - toutes les occurrences trouvées sont bien remplacées, sur toutes les
//!   pages, et le texte cherché ne se relit plus nulle part ;
//! - **le reste de la page ne bouge pas** : les lignes qui ne contenaient pas
//!   le mot gardent leur boîte au centième près ;
//! - un mot absent ne touche à rien et rend zéro.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use acrux_app::render_worker::replace_all;
use acrux_document::{collect_pages, Document};
use acrux_features::text::{extract_page_text, PageText};

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
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
            {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Texte de chaque page, ligne par ligne, avec la boîte de chaque ligne.
fn pages_text(doc: &Document) -> Vec<Vec<(String, acrux_core::Rect)>> {
    let pages = collect_pages(doc).unwrap();
    pages
        .iter()
        .map(|p| {
            extract_page_text(doc, p)
                .map(|t| lines_of(&t))
                .unwrap_or_default()
        })
        .collect()
}

fn lines_of(text: &PageText) -> Vec<(String, acrux_core::Rect)> {
    text.lines
        .iter()
        .map(|l| {
            let s = l
                .words
                .iter()
                .map(|w| w.text.clone())
                .collect::<Vec<_>>()
                .join(" ");
            (s, l.bbox)
        })
        .collect()
}

/// Les mots d'une page, chacun avec sa boîte.
fn words_of(doc: &Document, page: &acrux_document::Page) -> Vec<(String, acrux_core::Rect)> {
    extract_page_text(doc, page).map_or_else(
        |_| Vec::new(),
        |t| {
            t.lines
                .iter()
                .flat_map(|l| l.words.iter().map(|w| (w.text.clone(), w.bbox)))
                .collect()
        },
    )
}

/// Les mots de chaque page.
fn pages_words(doc: &Document) -> Vec<Vec<(String, acrux_core::Rect)>> {
    let pages = collect_pages(doc).unwrap();
    pages.iter().map(|p| words_of(doc, p)).collect()
}

/// Le premier mot d'au moins quatre lettres qui revient dans le document :
/// remplacer un mot unique ne prouverait pas grand-chose.
fn common_word(pages: &[Vec<(String, acrux_core::Rect)>]) -> Option<String> {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for page in pages {
        for (line, _) in page {
            for word in line.split_whitespace() {
                let w: String = word.chars().filter(|c| c.is_alphabetic()).collect();
                if w.chars().count() >= 4 && w.is_ascii() {
                    *seen.entry(w.to_lowercase()).or_default() += 1;
                }
            }
        }
    }
    let mut best: Vec<(String, usize)> = seen.into_iter().filter(|(_, n)| *n >= 2).collect();
    best.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    best.into_iter().next().map(|(w, _)| w)
}

#[test]
fn remplace_toutes_les_occurrences_du_corpus() {
    let files = corpus_files();
    assert!(!files.is_empty(), "corpus introuvable");
    let mut tested = 0;
    for path in files {
        let Ok(doc) = Document::load(&path) else {
            continue;
        };
        let before = pages_text(&doc);
        let before_words = pages_words(&doc);
        let Some(word) = common_word(&before) else {
            continue;
        };
        // Un mot de remplacement qui ne ressemble à rien de ce que le
        // document contient : on peut ainsi affirmer qu'il ne reste plus
        // trace du mot cherché.
        let with = "zoubidou";
        let Ok(count) = replace_all(&doc, &word, with) else {
            continue;
        };
        if count == 0 {
            continue;
        }
        let after = pages_text(&doc);
        let left = after
            .iter()
            .flat_map(|p| p.iter())
            .filter(|(line, _)| line.to_lowercase().contains(&word))
            .count();
        assert_eq!(
            left,
            0,
            "{} : {left} ligne(s) contiennent encore « {word} »",
            path.display()
        );
        // Ce qui n'a pas été touché n'a pas bougé. La comparaison se fait
        // **mot à mot** : un mot de remplacement plus long décale la suite de
        // sa ligne, et peut même faire déborder une colonne sur l'autre — le
        // découpage en lignes de l'extraction n'est donc pas comparable, mais
        // les mots des lignes épargnées, si.
        let after_words = pages_words(&doc);
        for ((b, a), lines) in before_words.iter().zip(&after_words).zip(&before) {
            // Ordonnées des lignes qui contenaient le mot : leurs mots ont le
            // droit d'avoir bougé.
            let touched: Vec<f64> = lines
                .iter()
                .filter(|(l, _)| l.to_lowercase().contains(&word))
                .map(|(_, r)| r.y0)
                .collect();
            for (wb, bb) in b {
                if touched.iter().any(|y| (y - bb.y0).abs() < 1.0) {
                    continue;
                }
                let found = a.iter().any(|(wa, ba)| {
                    wa == wb && (bb.x0 - ba.x0).abs() < 0.01 && (bb.y0 - ba.y0).abs() < 0.01
                });
                assert!(found, "{} : le mot « {wb} » a bougé", path.display());
            }
        }
        tested += 1;
    }
    assert!(tested >= 3, "seulement {tested} document(s) éprouvé(s)");
}

#[test]
fn un_mot_absent_ne_touche_a_rien() {
    let path = corpus_files().into_iter().next().unwrap();
    let doc = Document::load(&path).unwrap();
    let before = pages_text(&doc);
    assert_eq!(replace_all(&doc, "zzzqqqxxx", "rien").unwrap(), 0);
    assert_eq!(replace_all(&doc, "", "rien").unwrap(), 0);
    assert_eq!(before, pages_text(&doc));
}

/// Nombre d'apparitions exactes (casse comprise) d'un mot dans le texte de
/// tout le document.
fn exact_count(pages: &[Vec<(String, acrux_core::Rect)>], word: &str) -> usize {
    pages
        .iter()
        .flat_map(|p| p.iter())
        .map(|(line, _)| line.matches(word).count())
        .sum()
}

/// « Respecter la casse » ne remplace que la forme exacte : le mot en
/// capitale disparaît, le même mot en minuscules reste lisible, et l'on
/// n'en remplace jamais plus que sans l'option.
#[test]
fn respecter_la_casse_ne_remplace_que_la_forme_exacte() {
    use acrux_app::render_worker::replace_all_with;
    use acrux_features::text::SearchOptions;
    let case = SearchOptions {
        match_case: true,
        whole_word: false,
    };
    let mut tested = 0;
    for path in corpus_files() {
        let Ok(doc) = Document::load(&path) else {
            continue;
        };
        let before = pages_text(&doc);
        let Some(word) = common_word(&before) else {
            continue;
        };
        let mut chars = word.chars();
        let Some(first) = chars.next() else { continue };
        let capital: String = first.to_uppercase().chain(chars).collect();
        let lower_before = exact_count(&before, &word);
        let Ok(exact) = replace_all_with(&doc, &capital, "zoubidou", case) else {
            continue;
        };
        let after = pages_text(&doc);
        assert_eq!(
            exact_count(&after, &capital),
            0,
            "{} : « {capital} » reste après remplacement",
            path.display()
        );
        assert_eq!(
            exact_count(&after, &word),
            lower_before,
            "{} : « {word} », en minuscules, ne devait pas être touché",
            path.display()
        );
        // Sans l'option, sur une copie neuve : au moins autant.
        let Ok(fresh) = Document::load(&path) else {
            continue;
        };
        let Ok(any) = replace_all(&fresh, &capital, "zoubidou") else {
            continue;
        };
        assert!(exact <= any, "{} : {exact} > {any}", path.display());
        tested += 1;
    }
    assert!(tested >= 3, "seulement {tested} document(s) éprouvé(s)");
}
