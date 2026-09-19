//! Contenu optionnel (§8.11) : liste des calques d'un document et rendu selon
//! la visibilité imposée par l'appelant.
//!
//! Le fichier de corpus `synthese/calques-contenu-optionnel.pdf` déclare trois
//! groupes — « Plan » (bloc bleu), « Cotes » (ligne de cote orange) et
//! « Brouillon » (gros texte gris), ce dernier masqué par la configuration par
//! défaut du document. Chaque test compte des pixels d'une couleur dans la
//! zone du calque concerné : c'est la seule preuve qui compte, le reste du
//! rendu étant couvert par `render_corpus`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::path::PathBuf;

use acrux_document::{collect_pages, Document};
use acrux_render::{layers, render_page, Layer, RenderOptions};

/// Racine du dépôt, depuis le dossier de la crate.
fn corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join("synthese")
        .join("calques-contenu-optionnel.pdf")
}

fn document() -> Document {
    Document::load(corpus()).expect("corpus des calques")
}

/// Nombre de pixels de la page qui ne sont pas blancs, dans la bande
/// horizontale `y0..y1` (en pixels, origine en haut).
fn ink_in_band(doc: &Document, forced: &HashMap<u32, bool>, y0: u32, y1: u32) -> usize {
    let pages = collect_pages(doc).unwrap();
    let options = RenderOptions {
        layers: forced.clone(),
        ..RenderOptions::default()
    };
    let rendered = render_page(doc, &pages[0], 1.0, &options);
    let bitmap = &rendered.bitmap;
    let rgba = bitmap.to_rgba8_unpremultiplied();
    let mut count = 0;
    for y in y0..y1.min(bitmap.height()) {
        for x in 0..bitmap.width() {
            let i = (y as usize * bitmap.width() as usize + x as usize) * 4;
            let (r, g, b) = (rgba[i], rgba[i + 1], rgba[i + 2]);
            if r < 240 || g < 240 || b < 240 {
                count += 1;
            }
        }
    }
    count
}

/// Numéro d'objet du calque portant ce nom.
fn number_of(list: &[Layer], name: &str) -> u32 {
    list.iter()
        .find(|l| l.name == name)
        .unwrap_or_else(|| panic!("calque « {name} » absent"))
        .number
}

#[test]
fn layers_are_listed_with_their_default_visibility() {
    let doc = document();
    let list = layers(&doc);
    let names: Vec<&str> = list.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Plan", "Cotes", "Brouillon"]);
    assert!(list[0].visible, "« Plan » est visible par défaut");
    assert!(list[1].visible, "« Cotes » est visible par défaut");
    assert!(
        !list[2].visible,
        "« Brouillon » est dans /OFF : masqué par défaut"
    );
}

#[test]
fn a_hidden_layer_appears_when_the_caller_asks_for_it() {
    let doc = document();
    let list = layers(&doc);
    // Le mot « BROUILLON » est dessiné vers y = 300 pt depuis le bas d'une
    // page de 842 pt, soit la bande 500..560 en pixels depuis le haut.
    let (y0, y1) = (500, 560);
    let hidden = ink_in_band(&doc, &HashMap::new(), y0, y1);
    assert_eq!(hidden, 0, "le brouillon ne doit pas être dessiné");
    let mut forced = HashMap::new();
    forced.insert(number_of(&list, "Brouillon"), true);
    let shown = ink_in_band(&doc, &forced, y0, y1);
    assert!(shown > 500, "le brouillon devrait apparaître ({shown} px)");
}

#[test]
fn a_visible_layer_disappears_when_the_caller_hides_it() {
    let doc = document();
    let list = layers(&doc);
    // Le bloc bleu occupe 60..260 pt en largeur et 520..680 pt en hauteur,
    // soit la bande 162..322 en pixels depuis le haut.
    let (y0, y1) = (180, 300);
    let shown = ink_in_band(&doc, &HashMap::new(), y0, y1);
    assert!(shown > 10_000, "le bloc bleu est dessiné ({shown} px)");
    let mut forced = HashMap::new();
    forced.insert(number_of(&list, "Plan"), false);
    let hidden = ink_in_band(&doc, &forced, y0, y1);
    assert!(
        hidden * 20 < shown,
        "le bloc bleu devrait disparaître ({hidden} px restants sur {shown})"
    );
}

#[test]
fn a_document_without_optional_content_has_no_layers() {
    let doc = Document::from_bytes(
        b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >> endobj\n".to_vec(),
    )
    .unwrap();
    assert!(layers(&doc).is_empty());
}
