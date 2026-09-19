//! Extraction de texte structurée sur les fichiers réels du corpus
//! (`tests/corpus/reels/`) : tableau, titre, listes, deux colonnes, en-tête et
//! pied de page, césure, styles, et conservation de tout le texte dans les
//! différentes sorties.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};

use acrux_document::{collect_pages, Document};
use acrux_features::text::{extract_page_text, Alignment, BlockKind, PageText, Paragraph};

fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join("reels")
        .join(name)
}

fn page_text(name: &str, index: usize) -> PageText {
    let doc = Document::load(corpus(name)).unwrap();
    let pages = collect_pages(&doc).unwrap();
    extract_page_text(&doc, &pages[index]).unwrap()
}

fn paragraphs(p: &PageText) -> Vec<&Paragraph> {
    p.blocks.iter().flat_map(|b| b.paragraphs.iter()).collect()
}

/// Caractères alphanumériques triés : compare le contenu de deux textes en
/// ignorant la mise en forme, les espaces, les tirets de césure et les puces.
fn letters(text: &str) -> Vec<char> {
    let mut v: Vec<char> = text.chars().filter(|c| c.is_alphanumeric()).collect();
    v.sort_unstable();
    v
}

/// Retire les balises d'un fragment HTML.
fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[test]
fn chrome_table_title_paragraph_and_bullets() {
    let p1 = page_text("chrome-skia-2pages-texte-tableau-svg.pdf", 0);
    // Tableau 3 × 2 tracé par des filets fins.
    assert_eq!(p1.tables.len(), 1, "{:?}", p1.tables);
    let cells: Vec<Vec<&str>> = p1.tables[0]
        .rows
        .iter()
        .map(|r| r.iter().map(|c| c.text.as_str()).collect())
        .collect();
    assert_eq!(
        cells,
        vec![
            vec!["Colonne A", "Colonne B"],
            vec!["1", "Alpha"],
            vec!["2", "Bêta"]
        ]
    );
    assert!(p1
        .blocks
        .iter()
        .any(|b| matches!(b.kind, BlockKind::Table(0))));
    // Titre : paragraphe d'une ligne en 24 pt (corps 12 pt) → `#`, en gras.
    let paras = paragraphs(&p1);
    let title = paras[0];
    // Texte du fichier de corpus, figé : il garde le nom qu'il portait le jour
    // où Chrome l'a produit.
    assert_eq!(title.text, "Document de test — AcrobatKiller");
    assert_eq!(title.heading, 1);
    assert!(p1.lines[title.lines[0]].words[0].style.bold);
    assert!(p1
        .to_markdown()
        .starts_with("# Document de test — AcrobatKiller\n"));
    // Paragraphe de deux lignes fusionné sur une seule.
    let para = paras
        .iter()
        .find(|p| p.text.starts_with("Ce document"))
        .unwrap();
    assert_eq!(para.lines.len(), 2);
    assert!(
        para.text
            .ends_with("Accents : éàüçß — « guillemets » — ligatures fi fl."),
        "{}",
        para.text
    );
    assert_eq!(para.heading, 0);
    assert!((para.size - 12.0).abs() < 0.5, "{}", para.size);
    // Texte brut : paragraphes séparés par une ligne vide, cellules par des tabulations.
    let plain = p1.to_plain();
    assert!(
        plain.contains("ligatures fi fl.\n\nColonne A\tColonne B\n1\tAlpha\n2\tBêta\n"),
        "{plain:?}"
    );

    // Page 2 : puces dessinées (cercles sous /Lbl), gras, italique, couleur du lien.
    let p2 = page_text("chrome-skia-2pages-texte-tableau-svg.pdf", 1);
    let md = p2.to_markdown();
    assert!(md.contains("- Un\n- Deux\n- Trois\n"), "{md}");
    assert!(
        md.contains("**gras**,") && md.contains("l'*italique*"),
        "{md}"
    );
    let html = p2.to_html();
    assert!(
        html.contains("<ul>\n<li>Un</li>\n<li>Deux</li>\n<li>Trois</li>\n</ul>"),
        "{html}"
    );
    assert!(
        html.contains("<b>gras</b>") && html.contains("<i>italique</i>"),
        "{html}"
    );
    let items: Vec<&Paragraph> = paragraphs(&p2)
        .into_iter()
        .filter(|p| p.marker.is_some())
        .collect();
    assert_eq!(items.len(), 3);
    assert!(items.iter().all(|p| p.marker.as_deref() == Some("•")));
    // « lien. » : le point, dans la même police, reste collé au mot.
    let link = p2
        .words()
        .into_iter()
        .find(|w| w.text.starts_with("lien"))
        .unwrap();
    assert!(
        link.style.color[2] > 0.9 && link.style.color[0] < 0.1,
        "{:?}",
        link.style
    );
    let gras = p2
        .words()
        .into_iter()
        .find(|w| w.text.starts_with("gras"))
        .unwrap();
    assert!(gras.style.bold && !gras.style.italic);
    assert_eq!(
        p2.to_plain().trim_end(),
        "Deuxième page avec du gras, de l'italique et un lien.\n\n• Un\n• Deux\n• Trois"
    );
}

#[test]
#[allow(clippy::too_many_lines)] // une page réelle vérifiée sous tous ses aspects
fn two_columns_header_footer_hyphenation_and_lists() {
    let p = page_text("chrome-skia-deux-colonnes-entete-pied-cesure.pdf", 0);
    // En-tête (deux fragments sur une ligne dans la marge haute) et pied « Page 1 ».
    let first = p.blocks.first().unwrap();
    assert_eq!(first.kind, BlockKind::Header, "{first:?}");
    assert_eq!(
        first.paragraphs[0].text,
        "Rapport de mise en page AcrobatKiller"
    );
    let last = p.blocks.last().unwrap();
    assert_eq!(last.kind, BlockKind::Footer, "{last:?}");
    assert_eq!(last.paragraphs[0].text, "Page 1");
    // Ordre de lecture : colonne de gauche entière avant la colonne de droite.
    let plain = p.to_plain();
    let pos = |needle: &str| {
        plain
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} absent de {plain}"))
    };
    let order = [
        "Extraction en deux colonnes",
        "Cette page sert",
        "Un mot coupé",
        "Sous-titre de section",
        "Le texte justifié",
        "Premier point",
        "Troisième point",
        "Le dernier paragraphe",
        "Signé : le comité",
        "Fin du document",
        "Page 1",
    ];
    for w in order.windows(2) {
        assert!(pos(w[0]) < pos(w[1]), "{} avant {} :\n{plain}", w[0], w[1]);
    }
    // Césure réparée et paragraphe continué d'une colonne à l'autre.
    assert!(
        plain.contains("comme dans la documentation technique."),
        "{plain}"
    );
    assert!(
        plain.contains("la dernière ligne de chaque paragraphe qui reste alignée à gauche."),
        "{plain}"
    );
    // Retrait de première ligne, justification, interligne.
    let paras = paragraphs(&p);
    let intro = paras
        .iter()
        .find(|p| p.text.starts_with("Cette page"))
        .unwrap();
    assert!(intro.lines.len() >= 5, "{:?}", intro.lines);
    assert!(
        intro.first_line_indent > 8.0 && intro.first_line_indent < 20.0,
        "{}",
        intro.first_line_indent
    );
    assert_eq!(intro.alignment, Alignment::Justify);
    assert!(
        intro.line_spacing > 12.0 && intro.line_spacing < 16.0,
        "{}",
        intro.line_spacing
    );
    // Liste numérotée (étiquettes textuelles « 1. »).
    let items: Vec<&&Paragraph> = paras.iter().filter(|p| p.marker.is_some()).collect();
    let markers: Vec<&str> = items.iter().map(|p| p.marker.as_deref().unwrap()).collect();
    assert_eq!(markers, vec!["1.", "2.", "3."]);
    assert_eq!(items[0].text, "Premier point numéroté.");
    // Alignements des lignes isolées et titres.
    let signed = paras.iter().find(|p| p.text.starts_with("Signé")).unwrap();
    assert_eq!(signed.alignment, Alignment::Right);
    let end = paras.iter().find(|p| p.text == "Fin du document").unwrap();
    assert_eq!(end.alignment, Alignment::Center);
    assert!(p.lines[end.lines[0]].words[0].style.italic);
    let title = paras
        .iter()
        .find(|p| p.text == "Extraction en deux colonnes")
        .unwrap();
    assert_eq!(title.heading, 1);
    let sub = paras
        .iter()
        .find(|p| p.text == "Sous-titre de section")
        .unwrap();
    assert!(sub.heading >= 2, "{}", sub.heading);
    // Markdown et HTML.
    let md = p.to_markdown();
    assert!(md.contains("# Extraction en deux colonnes\n"), "{md}");
    assert!(md.contains("1. Premier point numéroté.\n2. Deuxième point numéroté.\n3. Troisième point numéroté.\n"), "{md}");
    assert!(
        md.contains("**Jean-Pierre**") && md.contains("*arc-en-ciel*"),
        "{md}"
    );
    let html = p.to_html();
    assert!(html.starts_with("<header>\n<p>Rapport de mise en page AcrobatKiller</p>\n</header>\n<h1>Extraction en deux colonnes</h1>"), "{html}");
    assert!(
        html.contains("<ol>\n<li>Premier point numéroté.</li>"),
        "{html}"
    );
    assert!(
        html.ends_with("<footer>\n<p>Page 1</p>\n</footer>\n"),
        "{html}"
    );
    // Texte positionné : les deux colonnes restent côte à côte sur une même ligne.
    let layout = p.to_layout();
    let line = layout
        .lines()
        .find(|l| l.contains("Cette page sert"))
        .unwrap();
    assert!(
        line.contains("paragraphe") && line.find("paragraphe").unwrap() > 40,
        "{line}"
    );
}

#[test]
fn nothing_is_lost_between_lines_and_outputs() {
    for name in [
        "chrome-skia-2pages-texte-tableau-svg.pdf",
        "chrome-skia-deux-colonnes-entete-pied-cesure.pdf",
        "chrome-skia-images-jpeg-png-tirets-opacite-cjk.pdf",
    ] {
        let doc = Document::load(corpus(name)).unwrap();
        let pages = collect_pages(&doc).unwrap();
        for (i, page) in pages.iter().enumerate() {
            let p = extract_page_text(&doc, page).unwrap();
            let from_lines: String = p
                .lines
                .iter()
                .map(acrux_features::text::Line::text)
                .collect::<Vec<_>>()
                .join("\n");
            let expected = letters(&from_lines);
            assert!(!expected.is_empty(), "{name} page {} : aucun texte", i + 1);
            assert_eq!(
                letters(&p.to_plain()),
                expected,
                "{name} page {} : texte brut",
                i + 1
            );
            assert_eq!(
                letters(&p.to_markdown()),
                expected,
                "{name} page {} : markdown",
                i + 1
            );
            // En HTML, les numéros des listes ordonnées sont portés par `<ol>` :
            // on compare au texte des paragraphes (sans marqueurs) et des cellules.
            let mut structured: Vec<String> = Vec::new();
            for b in &p.blocks {
                structured.extend(b.paragraphs.iter().map(|q| q.text.clone()));
            }
            for t in &p.tables {
                structured.extend(t.rows.iter().flatten().map(|c| c.text.clone()));
            }
            assert_eq!(
                letters(&strip_tags(&p.to_html())),
                letters(&structured.join("\n")),
                "{name} page {} : html",
                i + 1
            );
            assert_eq!(
                letters(&p.to_layout()),
                expected,
                "{name} page {} : layout",
                i + 1
            );
            // Chaque ligne est référencée par exactement un paragraphe ou un tableau.
            let mut refs = vec![0usize; p.lines.len()];
            for b in &p.blocks {
                for q in &b.paragraphs {
                    for &l in &q.lines {
                        refs[l] += 1;
                    }
                }
            }
            for t in &p.tables {
                for &l in &t.lines {
                    refs[l] += 1;
                }
            }
            assert!(
                refs.iter().all(|&r| r == 1),
                "{name} page {} : lignes référencées {refs:?}",
                i + 1
            );
        }
    }
}
