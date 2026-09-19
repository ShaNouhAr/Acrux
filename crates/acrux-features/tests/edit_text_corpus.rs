//! Édition de texte in place sur les fichiers réels du corpus
//! (`tests/corpus/reels/`, PDF produits par Chrome/Skia avec des polices
//! Georgia incorporées en sous-ensembles composites `Identity-H`).
//!
//! Ce que vérifient ces tests :
//!
//! - une réécriture sans modification redonne **les mêmes octets** décodés ;
//! - un mot remplacé par un mot de même longueur, plus court ou plus long
//!   (avec accent) se relit bien après ré-extraction ;
//! - **aucune autre ligne ne bouge** (boîtes comparées à 0,01 près) ;
//! - le **rendu hors de la zone éditée est inchangé au pixel près**.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use acrux_core::Rect;
use acrux_document::{collect_pages, Document};
use acrux_features::edit_text::{
    apply_edits_reporting, find_ranges, reflow_paragraph, rewrite_content, ReflowOptions, TextEdit,
};
use acrux_features::text::{extract_page_text, PageText};
use acrux_render::page::page_content;
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
            if p.extension().is_some_and(|x| x == "pdf") && !p.to_string_lossy().contains("mdp-") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Boîtes des lignes, pour comparer une page avant et après édition.
fn boxes(text: &PageText) -> Vec<(String, Rect)> {
    text.lines
        .iter()
        .map(|l| (l.text(), l.bbox))
        .collect::<Vec<_>>()
}

/// Compare deux rendus hors d'une zone (en espace utilisateur), au pixel près.
fn same_pixels_outside(doc: &Document, before: &Document, page_index: usize, zone: Rect) -> usize {
    let render = |d: &Document| {
        let pages = collect_pages(d).unwrap();
        render_page(d, &pages[page_index], 1.0, &RenderOptions::default())
    };
    let a = render(before);
    let b = render(doc);
    assert_eq!(a.bitmap.width(), b.bitmap.width());
    assert_eq!(a.bitmap.height(), b.bitmap.height());
    // Zone éditée en pixels (avec une marge de 2 px pour l'anti-aliasing).
    let m = a.base_ctm;
    let corners = [
        m.apply(acrux_core::Point::new(zone.x0, zone.y0)),
        m.apply(acrux_core::Point::new(zone.x1, zone.y1)),
    ];
    let (x0, x1) = (
        corners[0].x.min(corners[1].x) - 2.0,
        corners[0].x.max(corners[1].x) + 2.0,
    );
    let (y0, y1) = (
        corners[0].y.min(corners[1].y) - 2.0,
        corners[0].y.max(corners[1].y) + 2.0,
    );
    let mut different = 0;
    for y in 0..a.bitmap.height() {
        for x in 0..a.bitmap.width() {
            let (fx, fy) = (f64::from(x), f64::from(y));
            if fx >= x0 && fx <= x1 && fy >= y0 && fy <= y1 {
                continue;
            }
            if a.bitmap.pixel(x, y) != b.bitmap.pixel(x, y) {
                different += 1;
            }
        }
    }
    different
}

/// Tous les glyphes d'une page, dans l'ordre de lecture.
fn glyphs_of(text: &PageText) -> Vec<(String, Rect)> {
    text.lines
        .iter()
        .flat_map(|l| l.words.iter())
        .flat_map(|w| w.glyphs.iter())
        .map(|g| (g.text.clone(), g.bbox))
        .collect()
}

/// Glyphes de `a` sans équivalent dans `b` (même texte, même boîte à 0,01).
fn missing_from(a: &[(String, Rect)], b: &[(String, Rect)]) -> Vec<String> {
    let mut used = vec![false; b.len()];
    let mut out = Vec::new();
    for (text, rect) in a {
        let found = b.iter().enumerate().position(|(i, (t, r))| {
            !used[i]
                && t == text
                && (r.x0 - rect.x0).abs() < 0.01
                && (r.y0 - rect.y0).abs() < 0.01
                && (r.x1 - rect.x1).abs() < 0.01
                && (r.y1 - rect.y1).abs() < 0.01
        });
        match found {
            Some(i) => used[i] = true,
            None => out.push(text.clone()),
        }
    }
    out
}

/// Vrai si `small` est une sous-suite de `big` (comparaison de caractères).
fn is_subsequence(small: &str, big: &str) -> bool {
    let mut it = big.chars();
    small.chars().all(|c| it.any(|b| b == c))
}

/// Remplace la première occurrence de `needle` et vérifie que seule la ligne
/// éditée change : les autres lignes gardent leur position au centième de point
/// près, le rendu hors de la ligne est identique au pixel près, et sur la ligne
/// éditée aucun glyphe n'en recouvre un autre (un remplacement plus large
/// décale la suite de la ligne au lieu de passer dessous). `expect`, s'il est
/// donné, doit apparaître dans le texte réextrait.
#[allow(clippy::too_many_lines)] // une vérification par propriété, à la suite
fn replace_and_check(
    file: &str,
    page_index: usize,
    needle: &str,
    replacement: &str,
    expect: Option<&str>,
) {
    let path = corpus(file);
    let original = Document::load(&path).unwrap();
    let doc = Document::load(&path).unwrap();
    let pages = collect_pages(&doc).unwrap();
    let page = &pages[page_index];
    let before = extract_page_text(&doc, page).unwrap();
    let before_boxes = boxes(&before);
    let before_glyphs = glyphs_of(&before);
    let target = *find_ranges(&before, needle)
        .first()
        .unwrap_or_else(|| panic!("« {needle} » introuvable dans {file}"));
    let line_text = before.lines[target.line].text();
    let line_box = before.lines[target.line].bbox;
    let report = apply_edits_reporting(
        &doc,
        &[TextEdit {
            page: page_index,
            target,
            new_text: replacement.to_string(),
            style: None,
        }],
    )
    .unwrap();
    assert!(
        report.warnings.is_empty(),
        "avertissements inattendus : {:?}",
        report.warnings
    );
    // Relecture après enregistrement incrémental : le document reste valide.
    let saved = Document::from_bytes(doc.save_incremental().unwrap()).unwrap();
    let pages = collect_pages(&saved).unwrap();
    let after = extract_page_text(&saved, &pages[page_index]).unwrap();
    let after_glyphs = glyphs_of(&after);
    // 1. Hors de la ligne éditée, pas un glyphe n'a bougé : le remplacement ne
    // déplace que la suite de sa propre ligne.
    let line_y = line_box.y0;
    let elsewhere = |g: &[(String, Rect)]| -> Vec<(String, Rect)> {
        g.iter()
            .filter(|(_, r)| (r.y0 - line_y).abs() > 1.0)
            .cloned()
            .collect()
    };
    let removed = missing_from(&elsewhere(&before_glyphs), &elsewhere(&after_glyphs)).concat();
    let added = missing_from(&elsewhere(&after_glyphs), &elsewhere(&before_glyphs)).concat();
    assert!(
        is_subsequence(
            &removed.to_lowercase(),
            &needle.to_lowercase().replace(' ', "")
        ),
        "glyphes supprimés inattendus ({file}, « {needle} ») : {removed:?}"
    );
    assert!(
        is_subsequence(&added, &replacement.replace(' ', "")),
        "glyphes ajoutés inattendus ({file}, « {needle} ») : {added:?}"
    );
    // 1 bis. Sur la ligne éditée, aucun glyphe n'en recouvre un autre.
    let mut on_line: Vec<(String, Rect)> = after_glyphs
        .iter()
        .filter(|(_, r)| (r.y0 - line_y).abs() <= 1.0)
        .cloned()
        .collect();
    on_line.sort_by(|a, b| a.1.x0.total_cmp(&b.1.x0));
    for pair in on_line.windows(2) {
        let (left, right) = (&pair[0], &pair[1]);
        let narrower = (left.1.x1 - left.1.x0).min(right.1.x1 - right.1.x0);
        assert!(
            right.1.x0 >= left.1.x1 - narrower * 0.15,
            "« {} » recouvre « {} » ({:?} / {:?})",
            right.0,
            left.0,
            left.1,
            right.1
        );
    }
    let changed_line = before.lines[target.line].text() != after.lines[target.line].text();
    assert!(
        needle == replacement || changed_line || !removed.is_empty() || !added.is_empty(),
        "aucun glyphe changé"
    );
    if let Some(expect) = expect {
        let plain = after.to_plain();
        assert!(plain.contains(expect), "« {expect} » absent de : {plain}");
    }
    // 2. Les autres lignes n'ont pas bougé d'un point.
    let after_boxes = boxes(&after);
    let mut checked = 0;
    for (text, rect) in &before_boxes {
        if *text == line_text {
            continue;
        }
        let found = after_boxes
            .iter()
            .find(|(t, _)| t == text)
            .unwrap_or_else(|| panic!("ligne « {text} » disparue"));
        assert!(
            (found.1.x0 - rect.x0).abs() < 0.01
                && (found.1.y0 - rect.y0).abs() < 0.01
                && (found.1.x1 - rect.x1).abs() < 0.01
                && (found.1.y1 - rect.y1).abs() < 0.01,
            "la ligne « {text} » a bougé : {rect:?} → {:?}",
            found.1
        );
        checked += 1;
    }
    assert!(checked >= 3, "trop peu de lignes vérifiées ({checked})");
    // 3. Le rendu est identique au pixel près hors de la ligne éditée.
    let edited = after
        .lines
        .iter()
        .map(|l| l.bbox)
        .filter(|b| (b.y0 - line_box.y0).abs() < 1.0)
        .fold(line_box, |a, b| a.union(&b));
    let zone = Rect::new(
        edited.x0 - 1.0,
        edited.y0 - 1.0,
        edited.x1 + 1.0,
        edited.y1 + 1.0,
    );
    let different = same_pixels_outside(&saved, &original, page_index, zone);
    assert_eq!(different, 0, "{different} pixels changés hors de la zone");
}

#[test]
fn rewrite_is_byte_identical_on_the_whole_corpus() {
    let files = corpus_files();
    assert!(files.len() > 5, "corpus introuvable");
    for path in files {
        let Ok(doc) = Document::load(&path) else {
            continue;
        };
        for page in collect_pages(&doc).unwrap_or_default() {
            let original = page_content(&doc, &page);
            let rewritten = rewrite_content(&doc, &page, &[]).unwrap();
            assert!(
                original == rewritten,
                "réécriture non identique : {} page {}",
                path.display(),
                page.index + 1
            );
        }
    }
}

#[test]
fn replace_word_same_length() {
    replace_and_check(
        "reels/chrome-skia-2pages-texte-tableau-svg.pdf",
        0,
        "Colonne",
        "Colonna",
        Some("Colonna A"),
    );
}

#[test]
fn replace_word_shorter() {
    replace_and_check(
        "reels/chrome-skia-2pages-texte-tableau-svg.pdf",
        0,
        "document",
        "doc",
        Some("doc de test"),
    );
}

#[test]
fn replace_word_longer_with_accent() {
    replace_and_check(
        "reels/chrome-skia-2pages-texte-tableau-svg.pdf",
        0,
        "tester",
        "vérifier très",
        Some("vérifier très le parseur"),
    );
}

#[test]
fn replace_in_justified_two_column_page() {
    replace_and_check(
        "reels/chrome-skia-deux-colonnes-entete-pied-cesure.pdf",
        0,
        "Signé",
        "Écrit",
        Some("Écrit : le comité"),
    );
    replace_and_check(
        "reels/chrome-skia-deux-colonnes-entete-pied-cesure.pdf",
        0,
        "comité",
        "comité de rédaction",
        Some("Signé : le comité de rédaction"),
    );
}

#[test]
fn reflow_paragraph_rewrites_the_whole_box() {
    let path = corpus("reels/chrome-skia-deux-colonnes-entete-pied-cesure.pdf");
    let doc = Document::load(&path).unwrap();
    let pages = collect_pages(&doc).unwrap();
    let before = extract_page_text(&doc, &pages[0]).unwrap();
    // Le premier paragraphe justifié de la colonne de gauche.
    let paragraphs: Vec<_> = before
        .blocks
        .iter()
        .flat_map(|b| b.paragraphs.iter())
        .collect();
    let index = paragraphs
        .iter()
        .position(|p| p.lines.len() >= 3)
        .expect("un paragraphe de plusieurs lignes");
    let box_before = paragraphs[index].bbox;
    let new_text = "Ce paragraphe a été entièrement recomposé par le moteur d'édition : \
        les mots sont replacés dans la boîte d'origine, avec le même interligne, \
        le même alignement et le même retrait de première ligne.";
    reflow_paragraph(&doc, &pages[0], index, new_text, &ReflowOptions::default()).unwrap();
    let saved = Document::from_bytes(doc.save_incremental().unwrap()).unwrap();
    let pages = collect_pages(&saved).unwrap();
    let after = extract_page_text(&saved, &pages[0]).unwrap();
    let paragraphs: Vec<_> = after
        .blocks
        .iter()
        .flat_map(|b| b.paragraphs.iter())
        .collect();
    let rewritten = paragraphs
        .iter()
        .find(|p| p.text.starts_with("Ce paragraphe a été"))
        .unwrap_or_else(|| {
            panic!(
                "paragraphe recomposé introuvable : {:?}",
                paragraphs
                    .iter()
                    .map(|p| p.text.as_str())
                    .collect::<Vec<_>>()
            )
        });
    assert!(
        rewritten.text.contains("retrait de première ligne"),
        "texte incomplet : {}",
        rewritten.text
    );
    // Il tient dans la largeur de la boîte d'origine.
    assert!(
        rewritten.bbox.x0 >= box_before.x0 - 0.5 && rewritten.bbox.x1 <= box_before.x1 + 0.5,
        "hors de la boîte : {:?} pour {:?}",
        rewritten.bbox,
        box_before
    );
    assert!((rewritten.bbox.y1 - box_before.y1).abs() < 0.5);
}

#[test]
fn missing_glyphs_are_added_from_the_system_font() {
    // « Z », « % » et « Ω » ne sont pas dans le sous-ensemble Georgia
    // incorporé par Chrome : ils doivent être ajoutés depuis georgia.ttf.
    let path = corpus("reels/chrome-skia-2pages-texte-tableau-svg.pdf");
    let doc = Document::load(&path).unwrap();
    let pages = collect_pages(&doc).unwrap();
    let before = extract_page_text(&doc, &pages[0]).unwrap();
    let target = find_ranges(&before, "Bêta")[0];
    let report = apply_edits_reporting(
        &doc,
        &[TextEdit {
            page: 0,
            target,
            new_text: "Zêta % Ωmega".into(),
            style: None,
        }],
    )
    .unwrap();
    let saved = Document::from_bytes(doc.save_incremental().unwrap()).unwrap();
    let pages = collect_pages(&saved).unwrap();
    let after = extract_page_text(&saved, &pages[0]).unwrap();
    let plain = after.to_plain();
    if !report.warnings.is_empty() {
        // Machine sans la police système : le repli doit être signalé.
        assert!(
            report.warnings.iter().any(|w| w.contains("standard")),
            "{:?}",
            report.warnings
        );
        return;
    }
    assert!(plain.contains("Zêta % Ωmega"), "{plain}");
    // Les nouveaux glyphes sont bien dessinés (de l'encre dans leur boîte).
    let omega = after
        .words()
        .into_iter()
        .find(|w| w.text.starts_with('Ω'))
        .expect("le mot « Ωmega » est extrait");
    let rendered = render_page(&saved, &pages[0], 2.0, &RenderOptions::default());
    let m = rendered.base_ctm;
    let a = m.apply(acrux_core::Point::new(omega.bbox.x0, omega.bbox.y0));
    let b = m.apply(acrux_core::Point::new(omega.bbox.x1, omega.bbox.y1));
    let mut ink = 0;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    for y in (a.y.min(b.y).max(0.0) as u32)..(a.y.max(b.y) as u32) {
        for x in (a.x.min(b.x).max(0.0) as u32)..(a.x.max(b.x) as u32) {
            if rendered.bitmap.pixel(x, y).is_some_and(|p| p[0] < 128) {
                ink += 1;
            }
        }
    }
    assert!(
        ink > 20,
        "les nouveaux glyphes ne sont pas dessinés ({ink} px)"
    );
}
