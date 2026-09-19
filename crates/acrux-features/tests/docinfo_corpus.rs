//! Métadonnées, propriétés d'ouverture, signets et liens sur les fichiers
//! réels du corpus (`tests/corpus/reels/`, `tests/corpus/synthese/`).
//!
//! Ce que vérifient ces tests, sur chaque fichier :
//!
//! - le document **se relit** après écriture ;
//! - la relecture rend **exactement** ce qui a été écrit ;
//! - le **rendu est identique au pixel près** : ni les métadonnées, ni les
//!   propriétés d'ouverture, ni les signets, ni les liens posés par
//!   `autolink` ne dessinent quoi que ce soit ;
//! - le **paquet XMP existant garde ses propriétés inconnues** ;
//! - relancer `autolink` ne crée pas de doublons.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use acrux_document::{collect_pages, Document};
use acrux_features::docinfo::{
    read_metadata, read_view_preferences, set_metadata, set_view_preferences, Date, Duplex,
    Metadata, PageLayout, PageMode, ViewPreferences,
};
use acrux_features::linkedit::autolink;
use acrux_features::navigation::{page_links, Action, Destination, PageIndex, View};
use acrux_features::outline_edit::{flatten, read_outline, set_outline, OutlineNode};
use acrux_render::{render_page, RenderOptions};

fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join(name)
}

/// Fichiers du corpus lisibles sans mot de passe, hors fichiers cassés.
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

/// Enregistre puis relit, comme le ferait un lecteur.
fn reload(doc: &Document) -> Document {
    let bytes = doc
        .save_incremental()
        .or_else(|_| doc.save_full())
        .expect("enregistrement");
    Document::from_bytes(bytes).expect("relecture")
}

/// Nombre de pixels qui diffèrent entre deux versions d'une même page.
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

/// Le rendu de chaque page est rigoureusement inchangé.
fn assert_same_pixels(file: &Path, before: &Document, after: &Document, what: &str) {
    let count = collect_pages(before).unwrap().len();
    assert_eq!(count, collect_pages(after).unwrap().len());
    for page in 0..count {
        let different = different_pixels(before, after, page);
        assert_eq!(
            different,
            0,
            "{} page {} : {different} pixels changés par {what}",
            file.display(),
            page + 1
        );
    }
}

/// Métadonnées à écrire, choisies pour éprouver l'encodage : accents,
/// esperluette, chevrons, apostrophe, date avec décalage horaire.
fn sample_metadata() -> Metadata {
    Metadata {
        title: Some("Rapport « annuel » & <suite>".into()),
        author: Some("Camille Dupont".into()),
        subject: Some("Un sujet éprouvant : l'apostrophe".into()),
        keywords: Some("pdf, métadonnées, test".into()),
        creator: Some("Acrux".into()),
        producer: Some("Acrux".into()),
        created: Date::parse_pdf("D:20240115103045+02'00'"),
        modified: Date::parse_pdf("D:20250919120000Z"),
        ..Metadata::default()
    }
}

#[test]
fn metadata_survive_a_round_trip_without_touching_a_pixel() {
    let files = corpus_files();
    assert!(files.len() > 5, "corpus introuvable");
    for path in files {
        let original = Document::load(&path).unwrap();
        let doc = Document::load(&path).unwrap();
        let before_xmp = acrux_features::docinfo::read_xmp(&doc).unwrap();
        let wanted = sample_metadata();
        set_metadata(&doc, &wanted).unwrap();
        let saved = reload(&doc);
        let back = read_metadata(&saved).unwrap();

        for (label, written, read) in [
            ("titre", &wanted.title, &back.title),
            ("auteur", &wanted.author, &back.author),
            ("sujet", &wanted.subject, &back.subject),
            ("mots-clés", &wanted.keywords, &back.keywords),
            ("créateur", &wanted.creator, &back.creator),
            ("producteur", &wanted.producer, &back.producer),
        ] {
            assert_eq!(read, written, "{} : {label}", path.display());
        }
        assert_eq!(back.created, wanted.created, "{}", path.display());
        assert_eq!(back.modified, wanted.modified, "{}", path.display());
        assert!(back.has_xmp, "{} : XMP absent", path.display());
        assert!(
            back.divergences.is_empty(),
            "{} : /Info et XMP divergent après écriture : {:?}",
            path.display(),
            back.divergences
        );

        // Un paquet XMP d'origine garde ses propriétés inconnues : on
        // vérifie sur `dc:rights` et `xmpMM:DocumentID`, les deux que les
        // générateurs du corpus écrivent le plus souvent.
        if let Some(before) = &before_xmp {
            let after = acrux_features::docinfo::read_xmp(&saved).unwrap().unwrap();
            for property in ["dc:rights", "xmpMM:DocumentID", "xmpMM:InstanceID"] {
                let tag = format!("<{property}");
                if before.contains(&tag) {
                    assert!(
                        after.contains(&tag),
                        "{} : propriété {property} perdue",
                        path.display()
                    );
                }
            }
        }
        assert_same_pixels(&path, &original, &saved, "l'écriture des métadonnées");
    }
}

#[test]
fn view_preferences_survive_a_round_trip_without_touching_a_pixel() {
    for path in corpus_files() {
        let original = Document::load(&path).unwrap();
        let doc = Document::load(&path).unwrap();
        let pages = collect_pages(&doc).unwrap().len();
        let wanted = ViewPreferences {
            page_mode: Some(PageMode::Outlines),
            page_layout: Some(PageLayout::TwoColumnLeft),
            open_action: Some(Action::GoTo(Destination {
                page: pages - 1,
                view: View::FitWidth { top: Some(700.0) },
            })),
            display_document_title: true,
            center_window: true,
            duplex: Some(Duplex::LongEdge),
            print_ranges: vec![(0, pages - 1)],
            copies: Some(2),
            ..ViewPreferences::default()
        };
        set_view_preferences(&doc, &wanted).unwrap();
        let saved = reload(&doc);
        assert_eq!(
            read_view_preferences(&saved).unwrap(),
            wanted,
            "{}",
            path.display()
        );
        assert_same_pixels(&path, &original, &saved, "les propriétés d'ouverture");
    }
}

/// Arbre à trois niveaux construit sur les pages disponibles.
fn sample_outline(pages: usize) -> Vec<OutlineNode> {
    let last = pages - 1;
    vec![
        OutlineNode {
            title: "Première partie".into(),
            bold: true,
            open: true,
            color: Some([0.8, 0.1, 0.1]),
            action: Some(Action::GoTo(Destination {
                page: 0,
                view: View::Fit,
            })),
            children: vec![OutlineNode {
                title: "Chapitre premier".into(),
                italic: true,
                open: false,
                children: vec![
                    OutlineNode::to_page("Détail A", 0),
                    OutlineNode::to_page("Détail B", last),
                ],
                ..OutlineNode::default()
            }],
            ..OutlineNode::default()
        },
        OutlineNode {
            title: "Sur la toile".into(),
            action: Some(Action::Uri("https://exemple.test/doc".into())),
            ..OutlineNode::default()
        },
    ]
}

#[test]
fn bookmarks_survive_a_round_trip_without_touching_a_pixel() {
    for path in corpus_files() {
        let original = Document::load(&path).unwrap();
        let doc = Document::load(&path).unwrap();
        let pages = collect_pages(&doc).unwrap().len();
        let wanted = sample_outline(pages);
        set_outline(&doc, &wanted).unwrap();
        let saved = reload(&doc);
        assert_eq!(read_outline(&saved).unwrap(), wanted, "{}", path.display());
        assert_eq!(flatten(&read_outline(&saved).unwrap()).len(), 5);
        assert_same_pixels(&path, &original, &saved, "l'écriture des signets");

        // Retirer les signets rend un document sans /Outlines, toujours
        // identique au pixel près.
        set_outline(&saved, &[]).unwrap();
        let cleared = reload(&saved);
        assert!(read_outline(&cleared).unwrap().is_empty());
        assert_same_pixels(&path, &original, &cleared, "le retrait des signets");
    }
}

#[test]
fn autolink_adds_invisible_links_and_never_duplicates_them() {
    let mut linked_documents = 0;
    for path in corpus_files() {
        let original = Document::load(&path).unwrap();
        let doc = Document::load(&path).unwrap();
        let report = autolink(&doc).unwrap();
        if report.added == 0 {
            continue;
        }
        linked_documents += 1;
        let saved = reload(&doc);
        assert_same_pixels(&path, &original, &saved, "autolink");

        // Toutes les adresses posées se relisent comme des liens URI.
        let pages = collect_pages(&saved).unwrap();
        let index = PageIndex::new(&pages);
        let mut uris = Vec::new();
        for page in &pages {
            for link in page_links(&saved, page, &index).unwrap() {
                if let Action::Uri(u) = link.action {
                    uris.push(u);
                }
            }
        }
        for found in &report.found {
            assert!(
                uris.contains(&found.uri),
                "{} : {} n'est pas relu",
                path.display(),
                found.uri
            );
        }

        // Relancé, autolink ne pose plus rien : les liens existants sont
        // reconnus.
        let again = autolink(&saved).unwrap();
        assert_eq!(
            again.added,
            0,
            "{} : {} lien(s) en double au second passage",
            path.display(),
            again.added
        );
        assert_eq!(again.already_linked, again.found.len());
    }
    assert!(
        linked_documents > 0,
        "aucun fichier du corpus ne contient d'adresse : le test ne prouve rien"
    );
}

#[test]
fn everything_written_can_be_written_again_over_itself() {
    // Deux passages successifs ne doivent ni accumuler d'objets morts ni
    // changer le résultat : c'est ce qui garantit qu'un utilisateur peut
    // enregistrer plusieurs fois de suite.
    let path = corpus("synthese").join("liens-destinations-signets.pdf");
    let doc = Document::load(&path).unwrap();
    let pages = collect_pages(&doc).unwrap().len();
    set_metadata(&doc, &sample_metadata()).unwrap();
    set_outline(&doc, &sample_outline(pages)).unwrap();
    let first = reload(&doc);
    set_metadata(&first, &sample_metadata()).unwrap();
    set_outline(&first, &sample_outline(pages)).unwrap();
    let second = reload(&first);
    assert_eq!(
        read_metadata(&second).unwrap(),
        read_metadata(&first).unwrap()
    );
    assert_eq!(read_outline(&second).unwrap(), sample_outline(pages));
    assert_same_pixels(&path, &first, &second, "la seconde écriture");
}
