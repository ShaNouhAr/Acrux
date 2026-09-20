//! Frappe en direct : ce que l'aperçu dessine doit tomber **exactement** là
//! où l'écriture mettra le texte.
//!
//! L'application ne réécrit plus le document à chaque lettre : elle met le
//! texte en page en mémoire et le dessine elle-même. Tout repose donc sur une
//! seule promesse — la mise en page de l'aperçu est celle qui écrit. Ces
//! tests la vérifient sur de vrais fichiers : pour chaque bloc, l'aperçu est
//! comparé au texte réellement écrit, ligne par ligne.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use acrux_document::{collect_pages, Document};
use acrux_features::edit_text::{
    line_at, line_unit, open_paragraph, open_unit, set_paragraph_text, text_frame_at, LiveText,
    NewTextStyle, OpenedParagraph,
};
use acrux_features::text::extract_page_text;

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus")
        .join(name)
}

/// Tous les fichiers du corpus.
fn all_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ["reels", "synthese"] {
        let Ok(entries) = std::fs::read_dir(corpus(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "pdf") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Un bloc ouvrable de la première page.
fn a_block(doc: &Document) -> Option<OpenedParagraph> {
    let pages = collect_pages(doc).ok()?;
    let page = pages.first()?;
    let text = extract_page_text(doc, page).ok()?;
    let count = text
        .blocks
        .iter()
        .map(|b| b.paragraphs.len())
        .sum::<usize>();
    for i in 0..count {
        if let Ok(opened) = open_paragraph(doc, page, &text, i) {
            if opened.text.split_whitespace().count() >= 3 {
                return Some(opened);
            }
        }
    }
    // À défaut, la première ligne qui s'ouvre.
    for i in 0..text.lines.len() {
        if let Some(unit) = line_unit(&text, i) {
            if let Ok(opened) = open_unit(doc, page, &text, &unit) {
                if !opened.text.trim().is_empty() {
                    return Some(opened);
                }
            }
        }
    }
    None
}

#[test]
fn lapercu_place_le_texte_ou_lecriture_le_mettra() {
    let mut checked = 0;
    for file in all_files() {
        let Ok(doc) = Document::load(&file) else {
            continue;
        };
        let Some(opened) = a_block(&doc) else {
            continue;
        };
        let pages = collect_pages(&doc).unwrap();
        let Ok(live) = LiveText::open(&doc, &pages[0], &opened.frame) else {
            continue;
        };
        let typed = format!("{} modifie", opened.text.trim());
        if !live.covers(&typed) {
            continue;
        }
        // Un bloc en biais se met en page dans son repère : l'aperçu ne sait
        // pas encore le dessiner tourné, et l'application le laisse donc au
        // document. Rien à comparer ici.
        if opened.frame.rotation.abs() > 1e-4 {
            continue;
        }
        // 1. L'aperçu, sans rien écrire.
        let laid = live.lay(&opened.frame, &typed);
        assert!(!laid.lines.is_empty(), "{}", file.display());
        // 2. L'écriture, pour de vrai.
        let map = set_paragraph_text(&doc, &pages[0], &opened.frame, &opened.drawn, &typed)
            .unwrap_or_else(|e| panic!("{} : {e}", file.display()));
        // 3. Les deux doivent coïncider, frontière par frontière.
        assert_eq!(
            laid.caret.len(),
            map.len(),
            "{} : nombre de frontières",
            file.display()
        );
        for i in 0..map.len() {
            let (a, b) = (laid.caret.caret_rect(i), map.caret_rect(i));
            assert!(
                (a.x0 - b.x0).abs() < 0.01 && (a.y0 - b.y0).abs() < 0.01,
                "{} : frontière {i} en ({}, {}) contre ({}, {})",
                file.display(),
                a.x0,
                a.y0,
                b.x0,
                b.y0
            );
        }
        checked += 1;
    }
    assert!(checked >= 5, "seulement {checked} fichier(s) vérifié(s)");
}

#[test]
fn une_zone_neuve_saffiche_avant_detre_ecrite() {
    let file = corpus("reels/chrome-skia-formulaire-lignes-cases.pdf");
    let doc = Document::load(&file).unwrap();
    let pages = collect_pages(&doc).unwrap();
    let text = extract_page_text(&doc, &pages[0]).unwrap();
    let crop = pages[0].crop_box(&doc);
    let frame = text_frame_at(
        &doc,
        &pages[0],
        &text,
        crop.x0 + 80.0,
        f64::midpoint(crop.y0, crop.y1),
        NewTextStyle::default(),
    );
    // La zone neuve n'a pas encore de police dans la page : l'aperçu doit
    // quand même savoir la mesurer, sans rien ajouter au document.
    let live = LiveText::open(&doc, &pages[0], &frame).expect("aperçu d'une zone neuve");
    let laid = live.lay(&frame, "Jean Dupont");
    assert_eq!(laid.lines.len(), 1);
    assert!(!live.glyphs(&laid, frame.size).is_empty());
    let before = doc.save_incremental().unwrap().len();
    let after = doc.save_incremental().unwrap().len();
    assert_eq!(before, after, "l'aperçu a modifié le document");
}

/// Une police incorporée est presque toujours sous-ensemblée : elle ne porte
/// que les lettres déjà sur la page. Taper les autres doit **quand même** se
/// voir — c'est la police système qui les prête le temps de la frappe.
#[test]
fn les_lettres_absentes_du_sous_ensemble_se_dessinent_quand_meme() {
    let mut secourus = 0;
    for file in all_files() {
        let Ok(doc) = Document::load(&file) else {
            continue;
        };
        let Some(opened) = a_block(&doc) else {
            continue;
        };
        let pages = collect_pages(&doc).unwrap();
        let Ok(live) = LiveText::open(&doc, &pages[0], &opened.frame) else {
            continue;
        };
        let typed = "Wagon dhiver 42";
        if live.covers(typed) {
            continue;
        }
        // Le document ne sait pas tout écrire, mais on doit tout voir.
        assert!(
            live.can_draw(typed),
            "{} : « {typed} » ne se dessine pas",
            file.display()
        );
        let laid = live.lay(&opened.frame, typed);
        let attendus = typed.chars().filter(|c| *c != ' ').count();
        assert_eq!(
            live.glyphs(&laid, opened.frame.size).len(),
            attendus,
            "{} : glyphes manquants",
            file.display()
        );
        secourus += 1;
    }
    assert!(secourus >= 1, "aucun sous-ensemble incomplet rencontré");
}

#[test]
fn les_lignes_de_lapercu_portent_le_texte_tape() {
    let file = corpus("reels/chrome-skia-deux-colonnes-entete-pied-cesure.pdf");
    let doc = Document::load(&file).unwrap();
    let pages = collect_pages(&doc).unwrap();
    let opened = a_block(&doc).expect("un bloc");
    let live = LiveText::open(&doc, &pages[0], &opened.frame).expect("police du bloc");
    let typed = "Un texte assez long pour couler sur plusieurs lignes dans la boite du bloc.";
    let laid = live.lay(&opened.frame, typed);
    let rendu: String = laid
        .lines
        .iter()
        .map(|l| l.text.trim())
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        rendu.split_whitespace().collect::<Vec<_>>(),
        typed.split_whitespace().collect::<Vec<_>>()
    );
    // Chaque ligne descend, et chacune part de la boîte.
    for pair in laid.lines.windows(2) {
        assert!(pair[1].baseline < pair[0].baseline);
    }
    for line in &laid.lines {
        assert!(line.x >= opened.frame.x0 - 0.01);
    }
}

#[test]
fn lapercu_connait_lencre_du_bloc() {
    for file in all_files() {
        let Ok(doc) = Document::load(&file) else {
            continue;
        };
        let Some(opened) = a_block(&doc) else {
            continue;
        };
        let pages = collect_pages(&doc).unwrap();
        let Ok(live) = LiveText::open(&doc, &pages[0], &opened.frame) else {
            continue;
        };
        let [r, g, b] = live.color();
        assert!(
            (0.0..=1.0).contains(&r) && (0.0..=1.0).contains(&g) && (0.0..=1.0).contains(&b),
            "{} : encre hors bornes",
            file.display()
        );
    }
}

/// La ligne cliquée s'ouvre aussi en direct : c'est le repli quand un bloc
/// entier refuse, et il doit rester aussi vif.
#[test]
fn une_ligne_seule_sapercoit_aussi() {
    let file = corpus("reels/chrome-skia-2pages-texte-tableau-svg.pdf");
    let doc = Document::load(&file).unwrap();
    let pages = collect_pages(&doc).unwrap();
    let text = extract_page_text(&doc, &pages[0]).unwrap();
    let line = text.lines.first().expect("une ligne");
    let index = line_at(
        &text,
        f64::midpoint(line.bbox.x0, line.bbox.x1),
        f64::midpoint(line.bbox.y0, line.bbox.y1),
    )
    .expect("ligne sous le point");
    let unit = line_unit(&text, index).expect("unité de ligne");
    let opened = open_unit(&doc, &pages[0], &text, &unit).expect("ouverture");
    let apercu = LiveText::open(&doc, &pages[0], &opened.frame).expect("police");
    let laid = apercu.lay(&opened.frame, "Ligne remplacee");
    assert_eq!(laid.lines.len(), 1);
    assert!((laid.lines[0].baseline - opened.frame.baseline).abs() < 0.01);
}
