//! Étiquettes de page sur les fichiers réels du corpus.
//!
//! `/PageLabels` ne dessine rien : poser des étiquettes ne doit changer
//! **aucun pixel**. Ces tests le vérifient page par page, contrôlent que les
//! étiquettes se relisent à l'identique après enregistrement, et que la
//! recherche d'une page par son étiquette retrouve la bonne.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use acrux_document::{collect_pages, Document};
use acrux_features::pagelabels::{
    page_for_label, read_label_ranges, read_page_labels, set_page_labels, LabelRange, LabelStyle,
};
use acrux_render::{render_page, RenderOptions};

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
            let name = p.to_string_lossy().to_string();
            if p.extension().is_some_and(|x| x == "pdf")
                && !name.contains("mdp-")
                && !name.contains("casse")
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn reload(doc: &Document) -> Document {
    let bytes = doc
        .save_incremental()
        .or_else(|_| doc.save_full())
        .expect("enregistrement");
    Document::from_bytes(bytes).expect("relecture")
}

fn different_pixels(before: &Document, after: &Document, page: usize) -> usize {
    let render = |d: &Document| {
        let pages = collect_pages(d).unwrap();
        render_page(d, &pages[page], 1.0, &RenderOptions::default())
    };
    let a = render(before);
    let b = render(after);
    assert_eq!(a.bitmap.width(), b.bitmap.width());
    assert_eq!(a.bitmap.height(), b.bitmap.height());
    let mut different = 0;
    for y in 0..a.bitmap.height() {
        for x in 0..a.bitmap.width() {
            if a.bitmap.pixel(x, y) != b.bitmap.pixel(x, y) {
                different += 1;
            }
        }
    }
    different
}

/// Étiquettes mélangées (romaines, décimales, alphabétiques avec préfixe) :
/// elles se relisent à l'identique et le rendu ne bouge pas d'un pixel.
#[test]
fn setting_labels_changes_no_pixel() {
    let files = corpus_files();
    assert!(files.len() > 5, "corpus introuvable");
    for path in files {
        let original = Document::load(&path).unwrap();
        let doc = Document::load(&path).unwrap();
        let count = collect_pages(&doc).unwrap().len();
        // La deuxième plage n'existe que si le document a assez de pages ;
        // le corpus va de 1 à 4 pages.
        let mut ranges = vec![LabelRange {
            first_page: 0,
            style: Some(LabelStyle::LowerRoman),
            prefix: String::new(),
            start: 1,
        }];
        if count > 1 {
            ranges.push(LabelRange {
                first_page: 1,
                style: Some(LabelStyle::UpperLetters),
                prefix: "Annexe-".into(),
                start: 1,
            });
        }
        set_page_labels(&doc, &ranges).unwrap();
        let saved = reload(&doc);
        assert_eq!(
            read_label_ranges(&saved).unwrap(),
            ranges,
            "{}",
            path.display()
        );
        let labels = read_page_labels(&saved).unwrap();
        assert_eq!(labels.len(), count, "{}", path.display());
        assert_eq!(labels[0], "i", "{}", path.display());
        if count > 1 {
            assert_eq!(labels[1], "Annexe-A", "{}", path.display());
            assert_eq!(page_for_label(&labels, "Annexe-A"), Some(1));
        }
        assert_eq!(page_for_label(&labels, "i"), Some(0));
        assert_eq!(page_for_label(&labels, "inexistante"), None);
        for index in 0..count {
            let different = different_pixels(&original, &saved, index);
            assert_eq!(
                different,
                0,
                "{} page {} : {different} pixels changés par des étiquettes",
                path.display(),
                index + 1
            );
        }
    }
}

/// Poser puis retirer les étiquettes rend un document numéroté en décimal
/// (c'est le repli quand `/PageLabels` est absent, y compris pour un fichier
/// qui en avait), et dont le rendu n'a pas bougé d'un pixel.
#[test]
fn clearing_labels_restores_decimal_numbering() {
    for path in corpus_files() {
        let original = Document::load(&path).unwrap();
        let doc = Document::load(&path).unwrap();
        let count = collect_pages(&doc).unwrap().len();
        let decimal: Vec<String> = (1..=count).map(|n| n.to_string()).collect();
        set_page_labels(
            &doc,
            &[LabelRange {
                first_page: 0,
                style: Some(LabelStyle::UpperRoman),
                prefix: String::new(),
                start: 5,
            }],
        )
        .unwrap();
        assert_eq!(read_page_labels(&doc).unwrap()[0], "V");
        set_page_labels(&doc, &[]).unwrap();
        let saved = reload(&doc);
        assert_eq!(
            read_page_labels(&saved).unwrap(),
            decimal,
            "{}",
            path.display()
        );
        for index in 0..count {
            let different = different_pixels(&original, &saved, index);
            assert_eq!(different, 0, "{} page {}", path.display(), index + 1);
        }
    }
}
