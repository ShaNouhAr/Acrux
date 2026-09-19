//! Le texte que **nous** écrivons, composé pour de bon.
//!
//! Ce test fabrique un PDF d'une page qui écrit quatre lignes **deux fois** :
//! d'abord glyphe par glyphe (`show` + `Tj`, le comportement historique),
//! puis composée ([`EmbeddedFont::shape_line`] + `TJ`). Les deux lignes se
//! superposeraient si la composition ne servait à rien ; elles diffèrent
//! visiblement :
//!
//! - `fi fl ffi affiche` : les ligatures soudent les `f` aux `i` et aux `l` ;
//! - `AVATAR To Wa` : le crénage resserre les paires `AV`, `VA`, `To`, `Wa` ;
//! - `العربية` : les lettres arabes se lient et la ligne se lit de droite à
//!   gauche ;
//! - `שלום abc` : l'algorithme bidirectionnel place le mot latin à gauche et
//!   l'hébreu de droite à gauche.
//!
//! Le PDF est écrit dans le dossier temporaire de `cargo` ; pour le
//! regarder :
//!
//! ```text
//! cargo test -p acrux-features --test shaping_visual -- --nocapture
//! cargo run -p acrux-cli -- render target/tmp/shaping-demo.pdf --dpi 144 -o demo.png
//! ```

#![allow(clippy::unwrap_used, clippy::print_stdout)]

use std::fmt::Write as _;

use acrux_document::{Dict, Document, Name, Object, ObjectRef};
use acrux_features::fontembed::{embed_text_font, EmbeddedFont, FontStyle};

/// Les lignes écrites, dans cet ordre.
const LINES: &[&str] = &[
    "fi fl ffi affiche",
    "AVATAR To Wa",
    "\u{0627}\u{0644}\u{0639}\u{0631}\u{0628}\u{064A}\u{0629}",
    "\u{05E9}\u{05DC}\u{05D5}\u{05DD} abc",
];

/// Référence de la page du squelette.
const PAGE: ObjectRef = ObjectRef {
    number: 3,
    generation: 0,
};

/// PDF minimal d'une page, à compléter.
fn skeleton() -> Vec<u8> {
    b"%PDF-1.7\n\
      1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
      2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
      3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 560 420] >> endobj\n\
      trailer << /Root 1 0 R /Size 4 >>\n"
        .to_vec()
}

/// Installe Calibri — riche en ligatures, en arabe et en hébreu — sous le nom
/// que le choix de police essaie en premier, pour que la démonstration ne
/// dépende pas de l'ordre des polices de la machine.
fn prefer_calibri() -> Option<std::path::PathBuf> {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("shapingfonts");
    std::fs::create_dir_all(&dir).ok()?;
    let calibri = std::fs::read(r"C:\Windows\Fonts\calibri.ttf").ok()?;
    std::fs::write(dir.join("arial.ttf"), calibri).ok()?;
    std::env::set_var("ACRUX_FONT_DIR", &dir);
    Some(dir)
}

/// Flux de contenu : chaque ligne brute, puis la même composée.
fn content(font: &EmbeddedFont) -> String {
    let mut out = String::new();
    let mut y = 370.0;
    for line in LINES {
        let _ = writeln!(out, "BT /F1 26 Tf 30 {y} Td <{}> Tj ET", font.show(line));
        y -= 40.0;
        let shaped = font
            .shape_line(line)
            .unwrap_or_else(|| format!("<{}> Tj", font.show(line)));
        let _ = writeln!(out, "BT /F1 26 Tf 30 {y} Td {shaped} ET");
        y -= 60.0;
    }
    out
}

/// Page complète : ressources, contenu, boîte.
fn build_page(doc: &Document, font: &EmbeddedFont) {
    let stream = doc.add(Object::Stream {
        dict: Dict::new(),
        raw: content(font).into_bytes(),
    });
    let mut fonts = Dict::new();
    fonts.insert(Name::new("F1"), Object::Reference(font.reference));
    let mut resources = Dict::new();
    resources.insert(Name::new("Font"), Object::Dict(fonts));
    let mut page = Dict::new();
    page.insert(Name::new("Type"), Object::Name(Name::new("Page")));
    page.insert(
        Name::new("Parent"),
        Object::Reference(ObjectRef {
            number: 2,
            generation: 0,
        }),
    );
    page.insert(
        Name::new("MediaBox"),
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(560),
            Object::Integer(420),
        ]),
    );
    page.insert(Name::new("Resources"), Object::Dict(resources));
    page.insert(Name::new("Contents"), Object::Reference(stream));
    doc.set(PAGE, Object::Dict(page));
}

#[test]
fn shaped_lines_differ_from_raw_lines() {
    let Some(_dir) = prefer_calibri() else {
        return; // Machine sans Calibri : rien à démontrer.
    };
    let doc = Document::from_bytes(skeleton()).unwrap();
    let Ok(font) = embed_text_font(&doc, &LINES.join("\n"), FontStyle::Regular) else {
        return; // Machine sans polices système.
    };

    // Les ligatures réduisent le nombre de glyphes.
    let ligatures = font.shape_line(LINES[0]).unwrap();
    let raw_glyphs = font.show(LINES[0]).len() / 4;
    let shaped_glyphs: usize = ligatures
        .split(['<', '>'])
        .filter(|s| s.len() % 4 == 0 && !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|s| s.len() / 4)
        .sum();
    assert!(
        shaped_glyphs < raw_glyphs,
        "les ligatures doivent réduire le nombre de glyphes ({shaped_glyphs} vs {raw_glyphs})"
    );

    // Le crénage resserre la ligne.
    let raw_width = font.width(LINES[1], 26.0);
    let shaped_width = font.shaped_width(LINES[1], 26.0).unwrap();
    assert!(
        shaped_width < raw_width,
        "le crénage doit resserrer « {} » ({shaped_width} vs {raw_width})",
        LINES[1]
    );

    // L'arabe change de glyphes (formes contextuelles) sans changer de
    // nombre de lettres.
    let arabic = font.shape_line(LINES[2]).unwrap();
    assert_ne!(arabic, format!("[<{}>] TJ", font.show(LINES[2])));

    build_page(&doc, &font);
    let bytes = doc.save_full().unwrap();
    let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("shaping-demo.pdf");
    std::fs::write(&out, bytes).unwrap();
    println!("démonstration écrite : {}", out.display());
}
