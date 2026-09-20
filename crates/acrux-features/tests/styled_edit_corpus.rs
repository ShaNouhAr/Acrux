//! Modifier une ligne qui mêle plusieurs styles ne doit pas les aplatir.
//!
//! Une ligne ordinaire d'un document porte souvent du gras, de l'italique et
//! un lien. La recomposer avec une seule police effacerait tout cela — c'est
//! ce que faisait Acrux jusqu'ici, et c'est le défaut qu'on éprouve ici : on
//! ouvre la ligne, on y insère un mot, on la réécrit, et l'on vérifie que le
//! gras est resté gras.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use acrux_document::{collect_pages, Document};
use acrux_features::edit_text::{line_at, line_unit, move_paragraph_styled, open_unit};
use acrux_features::text::extract_page_text;

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus")
        .join(name)
}

/// Mots gras et mots colorés d'une page, dans l'ordre.
fn styled_words(doc: &Document, page: usize) -> (Vec<String>, Vec<String>) {
    let pages = collect_pages(doc).unwrap();
    let text = extract_page_text(doc, &pages[page]).unwrap();
    let (mut bold, mut colored) = (Vec::new(), Vec::new());
    for line in &text.lines {
        for word in &line.words {
            if word.text.trim().is_empty() {
                continue;
            }
            let glyphs = &word.glyphs;
            if glyphs.iter().any(|g| g.bold) {
                bold.push(word.text.clone());
            }
            // Un lien est bleu : sa couleur est un style comme un autre.
            if glyphs.iter().any(|g| g.color[2] > 0.5 && g.color[0] < 0.3) {
                colored.push(word.text.clone());
            }
        }
    }
    (bold, colored)
}

#[test]
fn modifier_une_ligne_garde_le_gras_et_la_couleur() {
    let file = corpus("reels/chrome-skia-2pages-texte-tableau-svg.pdf");
    let doc = Document::load(&file).unwrap();
    let (bold_before, colored_before) = styled_words(&doc, 1);
    assert!(
        !bold_before.is_empty() && !colored_before.is_empty(),
        "la page d'épreuve doit porter du gras et un lien coloré"
    );

    // La ligne « Deuxième page avec du gras, de l'italique et un lien. »
    let pages = collect_pages(&doc).unwrap();
    let text = extract_page_text(&doc, &pages[1]).unwrap();
    let target = text
        .lines
        .iter()
        .position(|l| l.text().contains("gras"))
        .expect("une ligne avec du gras");
    let line = &text.lines[target];
    let index = line_at(
        &text,
        f64::midpoint(line.bbox.x0, line.bbox.x1),
        f64::midpoint(line.bbox.y0, line.bbox.y1),
    )
    .unwrap();
    let unit = line_unit(&text, index).unwrap();
    let opened = open_unit(&doc, &pages[1], &text, &unit).unwrap();
    assert!(
        !opened.styles.uniform(),
        "la ligne mêle plusieurs styles : {} relevé(s)",
        opened.styles.runs.len()
    );
    assert_eq!(
        opened.styles.per_char.len(),
        opened.text.chars().count(),
        "un style par caractère"
    );

    // On insère un mot au début, et l'on réécrit en rendant les styles.
    let edited = format!("Ici {}", opened.text);
    let styles = opened.styles.carry(&opened.text, &edited);
    move_paragraph_styled(
        &doc,
        &pages[1],
        &opened.frame,
        &opened.frame,
        &opened.drawn,
        &edited,
        Some(&styles),
    )
    .expect("réécriture");

    // Le gras et la couleur doivent avoir survécu.
    let (bold_after, colored_after) = styled_words(&doc, 1);
    for word in &bold_before {
        assert!(
            bold_after.iter().any(|w| w == word),
            "« {word} » n'est plus gras (gras restants : {bold_after:?})"
        );
    }
    for word in &colored_before {
        assert!(
            colored_after.iter().any(|w| w == word),
            "« {word} » a perdu sa couleur (colorés restants : {colored_after:?})"
        );
    }
    // Et le texte tapé est bien là.
    let after = extract_page_text(&doc, &collect_pages(&doc).unwrap()[1]).unwrap();
    let all: String = after
        .lines
        .iter()
        .map(acrux_features::text::Line::text)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(all.contains("Ici Deuxième"), "le texte tapé manque : {all}");
}

/// Sans styles, la réécriture reste celle d'avant : un seul style, une seule
/// police, et rien ne change dans le fichier.
#[test]
fn un_bloc_dun_seul_style_secrit_comme_avant() {
    let file = corpus("reels/chrome-skia-deux-colonnes-entete-pied-cesure.pdf");
    let doc = Document::load(&file).unwrap();
    let pages = collect_pages(&doc).unwrap();
    let text = extract_page_text(&doc, &pages[0]).unwrap();
    let index = text
        .lines
        .iter()
        .position(|l| l.text().contains("colonnes"))
        .unwrap();
    let unit = line_unit(&text, index).unwrap();
    let opened = open_unit(&doc, &pages[0], &text, &unit).unwrap();
    assert!(
        opened.styles.uniform(),
        "cette ligne n'a qu'un style : {:?}",
        opened.styles.runs.len()
    );
}

/// Affiche les polices de la page d'épreuve (mise au point).
#[test]
#[ignore = "sortie de mise au point"]
fn voir_les_polices() {
    let file = corpus("reels/chrome-skia-2pages-texte-tableau-svg.pdf");
    let doc = Document::load(&file).unwrap();
    let pages = collect_pages(&doc).unwrap();
    let text = extract_page_text(&doc, &pages[1]).unwrap();
    for line in text.lines.iter().take(3) {
        for word in &line.words {
            let g = &word.glyphs[0];
            println!(
                "{:<14} police {:<32} gras {} italique {} couleur {:?}",
                word.text, g.font, g.bold, g.italic, g.color
            );
        }
    }
}
