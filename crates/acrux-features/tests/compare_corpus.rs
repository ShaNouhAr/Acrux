//! Comparaison de documents sur le corpus réel (`tests/corpus/`).
//!
//! Ce que vérifient ces tests :
//!
//! - un fichier comparé à lui-même ne donne **aucune** différence, texte comme
//!   pixels, sur tout le corpus (y compris les fichiers cassés) ;
//! - le couple de synthèse `comparaison-avant.pdf` / `comparaison-apres.pdf`
//!   rend **exactement** les quatre modifications qui ont servi à le fabriquer
//!   (un mot remplacé, une page supprimée, une page insérée, une page
//!   déplacée) ;
//! - sur un fichier réel produit par Chrome, une édition faite avec
//!   `edit_text` ressort comme la seule différence, à la bonne place ;
//! - une page supprimée et des pages réordonnées dans un fichier réel sont
//!   reconnues comme telles, sans inventer de différences de texte ;
//! - le rapport PDF s'ouvre, se rend sans avertissement du moteur et n'est pas
//!   blanc.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use acrux_document::{collect_pages, Document, SaveOptions};
use acrux_features::compare::{
    compare_documents, write_report, CompareOptions, DiffKind, ReportOptions, VisualOptions,
};
use acrux_features::edit_text::{apply_edits, find_ranges, TextEdit};
use acrux_features::pages::{delete_pages, reorder_pages};
use acrux_features::text::extract_page_text;
use acrux_render::{render_page, RenderOptions};

fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join(name)
}

/// Tous les PDF du corpus, sauf les fichiers chiffrés (mot de passe requis).
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

fn open(path: &Path) -> Document {
    Document::load(path).unwrap_or_else(|e| panic!("{} : {e}", path.display()))
}

/// Recharge un document après modification (les outils de pages travaillent en
/// mémoire ; la comparaison doit voir le fichier tel qu'il sera enregistré).
fn saved(doc: &Document) -> Document {
    let bytes = doc
        .save_full_with(&SaveOptions {
            compress_streams: false,
            ..SaveOptions::default()
        })
        .unwrap();
    Document::from_bytes(bytes).unwrap()
}

#[test]
fn every_corpus_file_equals_itself() {
    for path in corpus_files() {
        let a = open(&path);
        let b = open(&path);
        let options = CompareOptions {
            visual: Some(VisualOptions {
                // 72 dpi : le test porte sur l'égalité, pas sur la finesse.
                scale: 1.0,
                ..VisualOptions::default()
            }),
            ..CompareOptions::default()
        };
        let comparison = compare_documents(&a, &b, &options).unwrap();
        assert!(
            comparison.is_identical(),
            "{} : {}",
            path.display(),
            comparison.summary(3)
        );
        assert_eq!(comparison.counts().total(), 0, "{}", path.display());
        for page in &comparison.pages {
            assert_eq!(page.pair.old_page, page.pair.new_page);
            let visual = page.visual.as_ref().unwrap();
            assert!(
                visual.rects.is_empty() && visual.changed_ratio == 0.0,
                "{} page {:?} : {} % de pixels",
                path.display(),
                page.pair.new_page,
                visual.changed_ratio * 100.0
            );
        }
    }
}

#[test]
fn the_synthetic_pair_reports_exactly_what_was_changed() {
    let before = open(&corpus("synthese/comparaison-avant.pdf"));
    let after = open(&corpus("synthese/comparaison-apres.pdf"));
    let comparison = compare_documents(&before, &after, &CompareOptions::default()).unwrap();
    let counts = comparison.counts();
    let summary = comparison.summary(3);
    assert_eq!(
        (counts.added, counts.removed, counts.replaced),
        (1, 1, 1),
        "{summary}"
    );
    assert_eq!(counts.moved, 0, "{summary}");
    assert_eq!(
        (counts.pages_added, counts.pages_removed, counts.pages_moved),
        (1, 1, 1),
        "{summary}"
    );

    // Le mot remplacé sur la première page, et rien d'autre sur cette page.
    let first = comparison
        .pages
        .iter()
        .find(|p| p.pair.old_page == Some(0) && p.pair.new_page == Some(0))
        .unwrap_or_else(|| panic!("{summary}"));
    assert_eq!(first.differences.len(), 1, "{summary}");
    let d = &first.differences[0];
    assert_eq!(d.kind, DiffKind::Replaced);
    assert_eq!(d.old_text, "brouillon");
    assert_eq!(d.new_text, "document definitif");
    assert_eq!(d.old_rects.len(), 1);
    assert!(
        d.old_rects[0].x0 > 100.0 && d.old_rects[0].y0 > 200.0,
        "le mot est sur la deuxième ligne de la page : {:?}",
        d.old_rects[0]
    );

    // La page supprimée, avec son texte.
    let removed = comparison
        .pages
        .iter()
        .find(|p| p.pair.new_page.is_none())
        .unwrap_or_else(|| panic!("{summary}"));
    assert_eq!(removed.pair.old_page, Some(2));
    assert!(
        removed.differences[0].old_text.contains("Chapitre second"),
        "{summary}"
    );
    // La page ajoutée, avec son texte.
    let added = comparison
        .pages
        .iter()
        .find(|p| p.pair.old_page.is_none())
        .unwrap_or_else(|| panic!("{summary}"));
    assert_eq!(added.pair.new_page, Some(1));
    assert!(
        added.differences[0].new_text.contains("Annexe ajoutee"),
        "{summary}"
    );
    // La page déplacée : reconnue, et sans différence de texte. Conclusion et
    // Chapitre premier ont été permutés ; l'alignement, qui est monotone, garde
    // l'un des deux en place et déclare l'autre déplacé — ici le chapitre.
    let moved = comparison
        .pages
        .iter()
        .find(|p| p.pair.moved)
        .unwrap_or_else(|| panic!("{summary}"));
    assert_eq!(
        (moved.pair.old_page, moved.pair.new_page),
        (Some(1), Some(3)),
        "{summary}"
    );
    assert!(moved.differences.is_empty(), "{summary}");
    assert!(moved.pair.similarity > 0.99, "{}", moved.pair.similarity);

    // La conclusion, restée appariée, ne doit rien produire non plus.
    let untouched = comparison
        .pages
        .iter()
        .find(|p| p.pair.old_page == Some(3))
        .unwrap_or_else(|| panic!("{summary}"));
    assert_eq!(untouched.pair.new_page, Some(2), "{summary}");
    assert!(untouched.differences.is_empty(), "{summary}");
}

#[test]
fn an_edit_on_a_real_file_is_the_only_difference() {
    let path = corpus("reels/chrome-skia-deux-colonnes-entete-pied-cesure.pdf");
    let before = open(&path);
    let edited = open(&path);
    let pages = collect_pages(&edited).unwrap();
    let text = extract_page_text(&edited, &pages[0]).unwrap();
    let target = find_ranges(&text, "colonnes")
        .first()
        .copied()
        .expect("le mot « colonnes » est dans ce fichier");
    apply_edits(
        &edited,
        &[TextEdit {
            page: 0,
            target,
            new_text: "rangees".into(),
            style: None,
        }],
    )
    .unwrap();
    let after = saved(&edited);

    let options = CompareOptions {
        visual: Some(VisualOptions {
            scale: 1.0,
            ..VisualOptions::default()
        }),
        ..CompareOptions::default()
    };
    let comparison = compare_documents(&before, &after, &options).unwrap();
    let summary = comparison.summary(5);
    let counts = comparison.counts();
    assert_eq!(counts.total(), 1, "{summary}");
    let d = comparison.differences()[0];
    assert_eq!(d.kind, DiffKind::Replaced, "{summary}");
    assert_eq!(d.old_text, "colonnes");
    assert_eq!(d.new_text, "rangees");
    assert_eq!(d.old_page, Some(0));
    assert_eq!(d.new_page, Some(0));

    // Les pixels changent au même endroit que le texte, et nulle part ailleurs.
    let page = &comparison.pages[0];
    let visual = page.visual.as_ref().unwrap();
    assert!(!visual.rects.is_empty(), "{summary}");
    let word = d.new_rects[0];
    for r in &visual.rects {
        assert!(
            r.intersect(&word).width() > 0.0 || r.y0 >= word.y0 - 4.0 && r.y1 <= word.y1 + 4.0,
            "zone de pixels {r:?} hors de la ligne éditée {word:?}"
        );
    }
    // Les autres pages ne bougent pas du tout.
    for page in &comparison.pages[1..] {
        assert!(page.is_identical(), "{summary}");
    }
}

#[test]
fn deleted_and_reordered_pages_on_a_real_file_are_recognised() {
    let path = corpus("reels/chrome-skia-2pages-texte-tableau-svg.pdf");
    let before = open(&path);
    // Deux pages : on les échange. Aucun texte ne change, seulement le rang.
    let swapped = open(&path);
    reorder_pages(&swapped, &[1, 0]).unwrap();
    let after = saved(&swapped);
    let comparison = compare_documents(&before, &after, &CompareOptions::default()).unwrap();
    let summary = comparison.summary(3);
    assert_eq!(comparison.counts().total(), 0, "{summary}");
    // Une permutation de deux pages, c'est un seul déplacement : l'alignement
    // en garde une en place et déclare l'autre déplacée.
    assert_eq!(comparison.counts().pages_moved, 1, "{summary}");
    let mut seen: Vec<(Option<usize>, Option<usize>)> = comparison
        .pages
        .iter()
        .map(|p| (p.pair.old_page, p.pair.new_page))
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        vec![(Some(0), Some(1)), (Some(1), Some(0))],
        "{summary}"
    );
    for page in &comparison.pages {
        assert!(page.differences.is_empty(), "{summary}");
    }

    // Une page supprimée : l'autre reste appariée sans différence.
    let shortened = open(&path);
    delete_pages(&shortened, &[0]).unwrap();
    let after = saved(&shortened);
    let comparison = compare_documents(&before, &after, &CompareOptions::default()).unwrap();
    let summary = comparison.summary(3);
    assert_eq!(comparison.counts().pages_removed, 1, "{summary}");
    assert_eq!(comparison.counts().pages_added, 0, "{summary}");
    let kept = comparison
        .pages
        .iter()
        .find(|p| p.pair.new_page == Some(0))
        .unwrap_or_else(|| panic!("{summary}"));
    assert_eq!(kept.pair.old_page, Some(1), "{summary}");
    assert!(kept.differences.is_empty(), "{summary}");
}

#[test]
fn the_report_is_a_readable_pdf() {
    let before = open(&corpus("synthese/comparaison-avant.pdf"));
    let after = open(&corpus("synthese/comparaison-apres.pdf"));
    let options = CompareOptions {
        visual: Some(VisualOptions::default()),
        ..CompareOptions::default()
    };
    let comparison = compare_documents(&before, &after, &options).unwrap();
    let bytes = write_report(
        &before,
        &after,
        &comparison,
        &ReportOptions {
            old_title: "comparaison-avant.pdf".into(),
            new_title: "comparaison-apres.pdf".into(),
            ..ReportOptions::default()
        },
    )
    .unwrap();
    let report = Document::from_bytes(bytes).unwrap();
    let pages = collect_pages(&report).unwrap();
    // Un résumé, puis une planche par paire de pages.
    assert_eq!(pages.len(), 1 + comparison.pages.len());
    for page in &pages {
        let rendered = render_page(&report, page, 1.0, &RenderOptions::default());
        assert!(
            rendered.warnings.is_empty(),
            "page {} : {:?}",
            page.index + 1,
            rendered.warnings
        );
        let ink = rendered
            .bitmap
            .to_rgb8_over_white()
            .chunks_exact(3)
            .filter(|p| p[0] < 250 || p[1] < 250 || p[2] < 250)
            .count();
        assert!(
            ink > 500,
            "page {} du rapport presque blanche ({ink} pixels encrés)",
            page.index + 1
        );
    }
    // Le texte du rapport se relit : titres, libellés et extraits.
    let summary_text = extract_page_text(&report, &pages[0]).unwrap().to_plain();
    for needle in [
        "Rapport de comparaison",
        "comparaison-avant.pdf",
        "brouillon",
        "document definitif",
        "Annexe ajoutee",
    ] {
        assert!(
            summary_text.contains(needle),
            "« {needle} » absent : {summary_text}"
        );
    }
}
