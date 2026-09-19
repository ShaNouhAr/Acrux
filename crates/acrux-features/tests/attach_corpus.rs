//! Pièces jointes sur les fichiers réels du corpus (`tests/corpus/reels/`,
//! `tests/corpus/synthese/`).
//!
//! Ce que vérifient ces tests, sur chaque fichier :
//!
//! - le document **se relit** après l'ajout et l'enregistrement ;
//! - la pièce jointe ressort **octet pour octet**, y compris des données
//!   binaires arbitraires et un nom de fichier accentué ;
//! - le **rendu est identique au pixel près** : une pièce jointe au niveau du
//!   document ne dessine rien, elle ne doit donc rien changer à l'image ;
//! - les pièces jointes déjà présentes dans le fichier sont conservées ;
//! - le retrait ramène le document à son inventaire d'origine.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use acrux_document::{collect_pages, Document};
use acrux_features::attach::{
    add_attachment, list_attachments, read_attachment, remove_attachment, AttachOptions,
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

/// Fichiers du corpus lisibles sans mot de passe (les fichiers `casse` sont
/// reconstruits par réparation : ce n'est pas ce qu'on teste ici).
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

/// Nombre de pixels différents entre deux documents, page à page.
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

/// Données d'essai : tous les octets possibles, deux fois, plus du texte.
/// Elles passent par le compresseur Flate et doivent ressortir intactes.
fn payload() -> Vec<u8> {
    let mut data = Vec::with_capacity(600);
    for _ in 0..2 {
        data.extend((0u16..=255).map(|b| u8::try_from(b).unwrap_or(0)));
    }
    data.extend_from_slice("fin — accentué\r\n\0".as_bytes());
    data
}

/// Une pièce jointe au niveau du document : le rendu ne bouge pas d'un pixel
/// et le fichier ressort octet pour octet.
#[test]
fn attaching_a_file_leaves_every_pixel_untouched() {
    let files = corpus_files();
    assert!(files.len() > 5, "corpus introuvable");
    let data = payload();
    for path in files {
        let original = Document::load(&path).unwrap();
        let doc = Document::load(&path).unwrap();
        let before = list_attachments(&doc).unwrap().len();
        let pages = collect_pages(&doc).unwrap().len();
        // Nom accentué, description accentuée : le nom traverse l'arbre de
        // noms en UTF-16BE et doit revenir identique.
        let name = "données-jointes é✓.bin";
        add_attachment(
            &doc,
            name,
            &data,
            &AttachOptions {
                description: Some("Pièce d'essai « corpus »".into()),
                ..AttachOptions::default()
            },
        )
        .unwrap();
        let saved = reload(&doc);
        let list = list_attachments(&saved).unwrap();
        assert_eq!(
            list.len(),
            before + 1,
            "{} : les pièces déjà présentes doivent survivre",
            path.display()
        );
        let found = list
            .iter()
            .find(|a| a.name == name)
            .unwrap_or_else(|| panic!("{} : pièce jointe absente après relecture", path.display()));
        assert_eq!(found.size, Some(data.len() as u64));
        assert_eq!(
            found.description.as_deref(),
            Some("Pièce d'essai « corpus »")
        );
        assert!(found.checksum.is_some(), "somme de contrôle attendue");
        assert_eq!(
            read_attachment(&saved, found).unwrap(),
            data,
            "{} : la pièce jointe n'est pas revenue octet pour octet",
            path.display()
        );
        for index in 0..pages {
            let different = different_pixels(&original, &saved, index);
            assert_eq!(
                different,
                0,
                "{} page {} : {different} pixels changés par une pièce jointe invisible",
                path.display(),
                index + 1
            );
        }
    }
}

/// Poser puis retirer une pièce jointe rend un document qui se dessine
/// exactement comme l'original et dont l'inventaire est revenu au départ.
#[test]
fn removing_an_attachment_restores_the_document() {
    let data = payload();
    for path in corpus_files() {
        let original = Document::load(&path).unwrap();
        let doc = Document::load(&path).unwrap();
        let before: Vec<String> = list_attachments(&doc)
            .unwrap()
            .into_iter()
            .map(|a| a.name)
            .collect();
        add_attachment(&doc, "temporaire.bin", &data, &AttachOptions::default()).unwrap();
        remove_attachment(&doc, "temporaire.bin").unwrap();
        let saved = reload(&doc);
        let after: Vec<String> = list_attachments(&saved)
            .unwrap()
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(after, before, "{}", path.display());
        for index in 0..collect_pages(&saved).unwrap().len() {
            let different = different_pixels(&original, &saved, index);
            assert_eq!(
                different,
                0,
                "{} page {} : {different} pixels n'ont pas retrouvé leur état",
                path.display(),
                index + 1
            );
        }
    }
}

/// Plusieurs pièces jointes de suite : l'arbre reste trié et chacune se relit.
#[test]
fn several_attachments_stay_sorted_and_readable() {
    for path in corpus_files() {
        let doc = Document::load(&path).unwrap();
        let start = list_attachments(&doc).unwrap().len();
        for i in [3u8, 1, 2] {
            add_attachment(
                &doc,
                &format!("piece-{i}.txt"),
                format!("contenu numero {i}").as_bytes(),
                &AttachOptions::default(),
            )
            .unwrap();
        }
        let saved = reload(&doc);
        let list = list_attachments(&saved).unwrap();
        assert_eq!(list.len(), start + 3, "{}", path.display());
        let names: Vec<&str> = list
            .iter()
            .map(|a| a.name.as_str())
            .filter(|n| n.starts_with("piece-"))
            .collect();
        assert_eq!(names, ["piece-1.txt", "piece-2.txt", "piece-3.txt"]);
        for (i, name) in names.iter().enumerate() {
            let found = list.iter().find(|a| a.name == *name).unwrap();
            assert_eq!(
                read_attachment(&saved, found).unwrap(),
                format!("contenu numero {}", i + 1).into_bytes()
            );
        }
    }
}
