//! Organisation des pages sur les fichiers réels du corpus : fractionner,
//! recombiner, déplacer un bloc de pages.
//!
//! Tout se fait sur des copies en mémoire : rien n'est écrit dans
//! `tests/corpus`. Le critère est le rendu : une page fractionnée puis
//! recombinée, ou déplacée puis remise à sa place, doit se dessiner **au
//! pixel près** comme l'originale. C'est ce qui prouve que l'extraction —
//! et son tri des références entre pages — ne perd rien de visible.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use acrux_document::{collect_pages, Document, Name, Object, ObjectRef, SaveOptions};
use acrux_features::pages::split::{plan_parts, write_parts, SplitPlan};
use acrux_features::pages::{merge, move_block, reorder_pages};
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

/// Un document lu depuis une copie en mémoire du fichier.
fn load(path: &Path) -> Document {
    Document::from_bytes(std::fs::read(path).unwrap()).unwrap()
}

/// Pixels qui diffèrent entre la page `a` de `left` et la page `b` de
/// `right`, rendues à la même échelle.
fn different_pixels(left: &Document, a: usize, right: &Document, b: usize) -> usize {
    let render = |d: &Document, i: usize| {
        let pages = collect_pages(d).unwrap();
        render_page(d, &pages[i], 1.0, &RenderOptions::default())
    };
    let (x, y) = (render(left, a), render(right, b));
    assert_eq!(x.bitmap.width(), y.bitmap.width());
    assert_eq!(x.bitmap.height(), y.bitmap.height());
    let mut different = 0;
    for row in 0..x.bitmap.height() {
        for col in 0..x.bitmap.width() {
            if x.bitmap.pixel(col, row) != y.bitmap.pixel(col, row) {
                different += 1;
            }
        }
    }
    different
}

/// Les parties d'un fractionnement, relues depuis leurs octets.
fn split(doc: &Document, plan: &SplitPlan) -> Vec<(Vec<usize>, Vec<u8>)> {
    let options = SaveOptions::default();
    let parts = plan_parts(doc, plan, &options).unwrap();
    let mut out = Vec::new();
    write_parts(doc, &parts.parts, &options, &mut |_, part, bytes| {
        out.push((part.pages.clone(), bytes));
        Ok(())
    })
    .unwrap();
    out
}

/// Nombre d'objets `/Type /Page` d'un fichier, dans l'arbre ou orphelins.
fn page_objects(doc: &Document) -> usize {
    doc.object_numbers()
        .into_iter()
        .filter(|&number| {
            doc.get(ObjectRef {
                number,
                generation: 0,
            })
            .is_ok_and(|o| {
                o.as_dict()
                    .and_then(|d| d.get(&Name::new("Type")))
                    .and_then(Object::as_name)
                    .is_some_and(|n| n.0 == b"Page")
            })
        })
        .count()
}

/// Une page par fichier, puis tous les fichiers recombinés : même nombre de
/// pages, et chaque page se dessine comme l'originale. Aucune partie ne
/// contient de page orpheline, et chacune pèse moins que le document.
#[test]
fn splitting_then_merging_changes_no_pixel() {
    let files = corpus_files();
    assert!(files.len() > 5, "corpus introuvable");
    for path in files {
        let doc = load(&path);
        let count = collect_pages(&doc).unwrap().len();
        let whole = doc
            .save_full_with(&SaveOptions {
                drop_unreferenced: true,
                ..SaveOptions::default()
            })
            .unwrap()
            .len();
        let parts = split(&doc, &SplitPlan::EveryN(1));
        assert_eq!(parts.len(), count, "{}", path.display());
        let docs: Vec<Document> = parts
            .into_iter()
            .map(|(pages, bytes)| {
                if count > 1 {
                    assert!(
                        bytes.len() < whole,
                        "{} page {} : {} octets pour {whole}",
                        path.display(),
                        pages[0] + 1,
                        bytes.len()
                    );
                }
                let part = Document::from_bytes(bytes).unwrap();
                assert_eq!(collect_pages(&part).unwrap().len(), 1);
                assert_eq!(
                    page_objects(&part),
                    1,
                    "{} : page orpheline",
                    path.display()
                );
                part
            })
            .collect();
        let refs: Vec<&Document> = docs.iter().collect();
        let merged = merge(&refs).unwrap();
        let merged = Document::from_bytes(merged.save_full().unwrap()).unwrap();
        assert_eq!(collect_pages(&merged).unwrap().len(), count);
        for page in 0..count {
            let different = different_pixels(&doc, page, &merged, page);
            assert_eq!(
                different,
                0,
                "{} page {} : {different} pixels changés",
                path.display(),
                page + 1
            );
        }
    }
}

/// Des pages aux images distinctes : chaque page écrite seule ne garde que
/// les siennes, et pèse bien moins que le document.
#[test]
fn a_part_keeps_only_its_own_images() {
    for name in [
        "synthese/cree-depuis-images.pdf",
        "synthese/jpx-gris-rvb-alpha-j2k.pdf",
    ] {
        let doc = load(&corpus(name));
        let whole = usize::try_from(std::fs::metadata(corpus(name)).unwrap().len()).unwrap();
        for (pages, bytes) in split(&doc, &SplitPlan::EveryN(1)) {
            assert!(
                bytes.len() * 10 < whole * 8,
                "{name} page {} : {} octets pour {whole}",
                pages[0] + 1,
                bytes.len()
            );
        }
    }
}

/// Les signets de premier niveau du document de liens : une partie par
/// page de départ distincte, aucune page orpheline, et les liens vers une
/// page restée dans une autre partie deviennent `null` au lieu de
/// l'embarquer.
#[test]
fn splitting_by_bookmarks_follows_the_top_level() {
    let path = corpus("synthese/liens-destinations-signets.pdf");
    let doc = load(&path);
    let pages = collect_pages(&doc).unwrap();
    let index = acrux_features::navigation::PageIndex::new(&pages);
    let tops = acrux_features::navigation::outline(&doc, &index).unwrap();
    let mut starts: Vec<usize> = tops
        .iter()
        .filter_map(|t| match &t.action {
            Some(acrux_features::navigation::Action::GoTo(d)) => Some(d.page),
            _ => None,
        })
        .collect();
    starts.sort_unstable();
    starts.dedup();
    assert!(!starts.is_empty(), "le document a des signets");
    let parts = split(&doc, &SplitPlan::TopBookmarks);
    let expected = starts.len() + usize::from(starts[0] > 0);
    assert_eq!(parts.len(), expected);
    let mut seen = Vec::new();
    for (list, bytes) in parts {
        let part = Document::from_bytes(bytes).unwrap();
        assert_eq!(page_objects(&part), list.len(), "aucune page orpheline");
        seen.extend(list);
    }
    assert_eq!(seen, (0..pages.len()).collect::<Vec<_>>());
}

/// Déplacer un bloc de pages puis le remettre : chaque page se dessine
/// comme avant.
#[test]
fn moving_a_block_and_back_changes_no_pixel() {
    for path in corpus_files() {
        let original = load(&path);
        let count = collect_pages(&original).unwrap().len();
        if count < 3 {
            continue;
        }
        let doc = load(&path);
        let block = [0, 1];
        let order = move_block(count, &block, count);
        reorder_pages(&doc, &order).unwrap();
        // L'inverse : la page qui est maintenant en position `i` venait de
        // `order[i]`.
        let mut back = vec![0; count];
        for (i, &from) in order.iter().enumerate() {
            back[from] = i;
        }
        reorder_pages(&doc, &back).unwrap();
        let doc = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        for page in 0..count {
            assert_eq!(
                different_pixels(&original, page, &doc, page),
                0,
                "{} page {}",
                path.display(),
                page + 1
            );
        }
    }
}
