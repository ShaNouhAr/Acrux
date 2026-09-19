//! Export et conversion sur les fichiers réels du corpus (`tests/corpus/`) :
//! images de page (PNG, JPEG), images incorporées, HTML, DOCX, XLSX.
//!
//! Les fichiers bureautiques produits ici ont par ailleurs été ouverts dans
//! Word et Excel (automation COM) pendant le développement ; ces tests
//! vérifient ce qui peut l'être sans Office : structure ZIP relue par notre
//! propre lecteur, XML bien formé, contenu attendu.

// Les noms de parties d'un paquet OOXML sont fixés par la spécification et
// écrits par nous : la comparaison d'extension sensible à la casse est voulue.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::cast_precision_loss,
    clippy::case_sensitive_file_extension_comparisons
)]

use std::path::{Path, PathBuf};

use acrux_document::Document;
use acrux_features::export::{
    export_docx, export_html, export_pages_jpeg, export_pages_png, export_tables_xlsx,
    extract_images, HtmlLayout, HtmlOptions, ImageFormat,
};
use acrux_features::zip::read as read_zip;

fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join("reels")
        .join(name)
}

const IMAGES: &str = "chrome-skia-images-jpeg-png-tirets-opacite-cjk.pdf";
const TABLEAU: &str = "chrome-skia-2pages-texte-tableau-svg.pdf";
const COLONNES: &str = "chrome-skia-deux-colonnes-entete-pied-cesure.pdf";

fn open(name: &str) -> Document {
    Document::load(corpus(name)).unwrap()
}

/// Vérifie qu'un fragment XML est bien formé : balises appariées, pas de
/// `<` ni `&` nu dans le texte. Volontairement minimal, mais suffisant pour
/// repérer une balise oubliée ou un texte mal échappé.
fn check_xml(xml: &str) {
    let mut stack: Vec<&str> = Vec::new();
    let bytes = xml.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'<' => {
                let end = xml[i..]
                    .find('>')
                    .unwrap_or_else(|| panic!("balise non fermée à {i}"))
                    + i;
                let tag = &xml[i + 1..end];
                if !tag.starts_with('?') && !tag.starts_with('!') {
                    if let Some(name) = tag.strip_prefix('/') {
                        let open = stack
                            .pop()
                            .unwrap_or_else(|| panic!("</{name}> sans ouverture"));
                        assert_eq!(open, name, "fermeture incohérente");
                    } else if !tag.ends_with('/') {
                        let name = tag.split([' ', '\t', '\n']).next().unwrap_or("");
                        stack.push(name);
                    }
                }
                i = end + 1;
            }
            b'&' => {
                let rest = &xml[i..];
                assert!(
                    rest.starts_with("&amp;")
                        || rest.starts_with("&lt;")
                        || rest.starts_with("&gt;")
                        || rest.starts_with("&quot;")
                        || rest.starts_with("&apos;")
                        || rest.starts_with("&#"),
                    "esperluette non échappée à {i}"
                );
                i += 1;
            }
            _ => i += 1,
        }
    }
    assert!(stack.is_empty(), "balises non fermées : {stack:?}");
}

#[test]
fn corpus_jpeg_images_come_out_byte_identical() {
    let doc = open(IMAGES);
    let raw = std::fs::read(corpus(IMAGES)).unwrap();
    let images = extract_images(&doc).unwrap();
    assert!(images.len() >= 2, "{images:?}");
    let jpegs: Vec<_> = images
        .iter()
        .filter(|i| i.format == ImageFormat::Jpeg)
        .collect();
    assert!(!jpegs.is_empty(), "le corpus contient un JPEG");
    for img in &jpegs {
        assert_eq!(&img.data[..2], &[0xFF, 0xD8], "SOI");
        // Les octets DCTDecode ressortent tels quels : on les retrouve
        // à l'identique dans le fichier PDF d'origine.
        assert!(
            raw.windows(img.data.len()).any(|w| w == img.data),
            "les octets JPEG de {} ne sont pas ceux du PDF",
            img.name
        );
        // Et ils se relisent avec notre décodeur, aux dimensions annoncées.
        let decoded = acrux_codecs::dct::decode(&img.data).unwrap();
        assert_eq!((decoded.width, decoded.height), (img.width, img.height));
    }
    for img in images.iter().filter(|i| i.format == ImageFormat::Png) {
        assert_eq!(&img.data[..8], &acrux_graphics::png::PNG_SIGNATURE);
        assert!(img.width > 0 && img.height > 0);
        assert!(!img.colorspace.is_empty());
    }
}

#[test]
fn page_jpeg_matches_the_png_rendering() {
    let doc = open(TABLEAU);
    let mut png = Vec::new();
    export_pages_png(&doc, &[0], 72.0, &mut |_, data| {
        png = data.to_vec();
        Ok(())
    })
    .unwrap();
    assert_eq!(&png[..8], &acrux_graphics::png::PNG_SIGNATURE);

    // Référence : le même rendu, en RVB.
    let pages = acrux_document::collect_pages(&doc).unwrap();
    let rendered = acrux_render::render_page(
        &doc,
        &pages[0],
        1.0,
        &acrux_render::RenderOptions {
            annotations: true,
            time_budget: Some(std::time::Duration::from_secs(60)),
            background: Some(acrux_graphics::Color::WHITE),
            ..acrux_render::RenderOptions::default()
        },
    );
    let reference = rendered.bitmap.to_rgb8_over_white();

    for (quality, limit) in [(90u8, 3.0f64), (50, 8.0)] {
        let mut jpeg = Vec::new();
        export_pages_jpeg(&doc, &[0], 72.0, quality, false, &mut |_, data| {
            jpeg = data.to_vec();
            Ok(())
        })
        .unwrap();
        let img = acrux_codecs::dct::decode(&jpeg).unwrap();
        assert_eq!(
            (img.width, img.height),
            (rendered.bitmap.width(), rendered.bitmap.height())
        );
        assert_eq!(img.data.len(), reference.len());
        let sum: f64 = reference
            .iter()
            .zip(&img.data)
            .map(|(&a, &b)| f64::from(i32::from(a) - i32::from(b)).abs())
            .sum();
        let mean = sum / reference.len() as f64;
        assert!(mean < limit, "écart moyen à qualité {quality} : {mean}");
    }
}

#[test]
fn absolute_html_looks_like_the_page() {
    let doc = open(IMAGES);
    let html = export_html(&doc, &HtmlOptions::default()).unwrap();
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.trim_end().ends_with("</html>"));
    for tag in ["span", "section", "html", "head", "body", "style", "title"] {
        assert_eq!(
            html.matches(&format!("<{tag}")).count(),
            html.matches(&format!("</{tag}>")).count(),
            "balises <{tag}> déséquilibrées"
        );
    }
    // Texte de la page, positionné en points.
    assert!(
        html.contains(">transparence</span>"),
        "texte attendu absent"
    );
    assert!(html.contains("pt;font-size:"));
    // Images incorporées dans leur format d'origine.
    assert!(
        html.contains("src=\"data:image/jpeg;base64,"),
        "JPEG incorporé"
    );
    assert!(
        html.contains("src=\"data:image/png;base64,"),
        "PNG incorporé"
    );
    // Polices incorporées : les TrueType du PDF, enveloppées en WOFF.
    assert!(
        html.contains("@font-face{font-family:'akf"),
        "police incorporée"
    );
    assert!(html.contains("format('woff')"));
    // Le texte tourné garde son angle.
    assert!(html.contains("transform:rotate("), "texte tourné");
    // Aucun guillemet double dans un attribut style (les familles de polices
    // sont citées avec des apostrophes) : sinon l'attribut se refermerait.
    for line in html.lines().filter(|l| l.starts_with("<span style=\"")) {
        let attr = &line["<span style=\"".len()..];
        let end = attr.find('"').unwrap();
        assert!(!attr[..end].contains('"'));
    }
}

#[test]
fn flow_html_reuses_the_structured_text() {
    let doc = open(COLONNES);
    let options = HtmlOptions {
        layout: HtmlLayout::Flow,
        title: "Deux colonnes".into(),
        ..HtmlOptions::default()
    };
    let html = export_html(&doc, &options).unwrap();
    assert!(html.contains("<title>Deux colonnes</title>"));
    assert!(
        html.contains("<h1>"),
        "titre reconstruit : {}",
        &html[..400]
    );
    assert!(html.contains("<p>"));
    assert!(!html.contains("position:absolute"));
    // Les en-têtes et pieds répétés sont balisés.
    assert!(html.contains("<footer>") || html.contains("<header>"));
}

#[test]
fn docx_package_opens_and_is_valid_xml() {
    let doc = open(TABLEAU);
    let archive = export_docx(&doc).unwrap();
    let entries = read_zip(&archive).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    for required in [
        "[Content_Types].xml",
        "_rels/.rels",
        "word/document.xml",
        "word/_rels/document.xml.rels",
        "word/styles.xml",
        "word/numbering.xml",
    ] {
        assert!(names.contains(&required), "{required} manque : {names:?}");
    }
    for e in &entries {
        if e.name.ends_with(".xml") || e.name.ends_with(".rels") {
            let xml = std::str::from_utf8(&e.data).unwrap();
            check_xml(xml);
        }
    }
    let document = entries
        .iter()
        .find(|e| e.name == "word/document.xml")
        .map(|e| String::from_utf8(e.data.clone()).unwrap())
        .unwrap();
    // Fragment du fichier de corpus lui-même, pas le nom du produit : ce PDF
    // est figé et ne sera jamais renommé.
    assert!(document.contains("Document de test"), "texte du PDF repris");
    assert!(document.contains("<w:tbl>"), "tableau converti");
    assert!(document.contains("<w:br w:type=\"page\"/>"), "saut de page");
    assert!(document.contains("<w:pStyle w:val=\"Heading1\"/>"), "titre");
}

#[test]
fn docx_carries_the_images_of_the_page() {
    let doc = open(IMAGES);
    let entries = read_zip(&export_docx(&doc).unwrap()).unwrap();
    let media: Vec<&str> = entries
        .iter()
        .map(|e| e.name.as_str())
        .filter(|n| n.starts_with("word/media/"))
        .collect();
    assert!(!media.is_empty(), "images incorporées : {media:?}");
    let rels = entries
        .iter()
        .find(|e| e.name == "word/_rels/document.xml.rels")
        .map(|e| String::from_utf8(e.data.clone()).unwrap())
        .unwrap();
    let document = entries
        .iter()
        .find(|e| e.name == "word/document.xml")
        .map(|e| String::from_utf8(e.data.clone()).unwrap())
        .unwrap();
    for (i, _) in media.iter().enumerate() {
        let id = format!("rId{}", i + 3);
        assert!(rels.contains(&format!("Id=\"{id}\"")), "relation {id}");
        assert!(
            document.contains(&format!("r:embed=\"{id}\"")),
            "dessin {id}"
        );
    }
    // Le JPEG du corpus reste un JPEG dans le document Word.
    assert!(media.iter().any(|n| n.ends_with(".jpeg")), "{media:?}");
}

#[test]
fn xlsx_carries_the_detected_tables() {
    let doc = open(TABLEAU);
    let entries = read_zip(&export_tables_xlsx(&doc).unwrap()).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    for required in [
        "[Content_Types].xml",
        "_rels/.rels",
        "xl/workbook.xml",
        "xl/_rels/workbook.xml.rels",
        "xl/worksheets/sheet1.xml",
        "xl/sharedStrings.xml",
        "xl/styles.xml",
    ] {
        assert!(names.contains(&required), "{required} manque : {names:?}");
    }
    for e in &entries {
        check_xml(std::str::from_utf8(&e.data).unwrap());
    }
    let sheet = entries
        .iter()
        .find(|e| e.name == "xl/worksheets/sheet1.xml")
        .map(|e| String::from_utf8(e.data.clone()).unwrap())
        .unwrap();
    assert!(sheet.contains("<c r=\"A1\" s=\"1\" t=\"s\">"), "{sheet}");
    assert!(sheet.contains("<c r=\"B2\""), "deuxième colonne : {sheet}");
    let sst = entries
        .iter()
        .find(|e| e.name == "xl/sharedStrings.xml")
        .map(|e| String::from_utf8(e.data.clone()).unwrap())
        .unwrap();
    assert!(sst.contains("Colonne A"), "en-tête du tableau : {sst}");
}

#[test]
fn zip_container_is_re_readable_and_deflated() {
    let doc = open(COLONNES);
    let archive = export_docx(&doc).unwrap();
    // Signature ZIP et fin de répertoire central.
    assert_eq!(&archive[..4], b"PK\x03\x04");
    assert!(archive.windows(4).any(|w| w == b"PK\x05\x06"));
    let entries = read_zip(&archive).unwrap();
    let total: usize = entries.iter().map(|e| e.data.len()).sum();
    assert!(
        archive.len() < total,
        "le conteneur ({}) doit être plus petit que son contenu ({total})",
        archive.len()
    );
    // Un octet altéré est détecté par le CRC-32.
    let mut corrupt = archive.clone();
    let i = corrupt.len() / 2;
    corrupt[i] ^= 0xFF;
    assert!(read_zip(&corrupt).is_err());
}
