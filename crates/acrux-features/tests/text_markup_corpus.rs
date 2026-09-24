//! Annoter le texte d'un fichier réel du corpus : souligner un mot, en
//! remplacer un autre.
//!
//! Ce que vérifient ces tests :
//!
//! - les annotations se posent sur les boîtes que donne l'extraction du
//!   texte, et se relisent après un enregistrement complet ;
//! - un remplacement forme un groupe (signe d'insertion et texte barré) ;
//! - le rendu les montre, et **rien** du contenu de la page ne change : le
//!   texte extrait est le même avant et après.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use acrux_core::Rect;
use acrux_document::{collect_pages, Document};
use acrux_features::annotations::{
    add_annotation, list_annotations, MarkupKind, NewAnnotation, CARET_COLOR,
};
use acrux_features::text::{extract_page_text, Line};
use acrux_render::{render_page, RenderOptions};

fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join(name)
}

#[test]
fn souligner_puis_remplacer_sur_un_vrai_document() {
    // Lu en mémoire : le fichier du corpus n'est jamais réécrit.
    let bytes = std::fs::read(corpus("reels/chrome-skia-2pages-texte-tableau-svg.pdf"))
        .expect("fichier du corpus");
    let doc = Document::from_bytes(bytes).expect("ouverture");
    let pages = collect_pages(&doc).expect("pages");
    let text = extract_page_text(&doc, &pages[0]).expect("texte");
    let before: Vec<String> = text.lines.iter().map(Line::text).collect();
    let words: Vec<Rect> = text
        .words()
        .iter()
        .filter(|w| w.text.chars().count() >= 4)
        .map(|w| w.bbox)
        .take(2)
        .collect();
    assert_eq!(words.len(), 2, "deux mots d'au moins quatre lettres");
    add_annotation(
        &doc,
        &pages[0],
        &NewAnnotation::Markup {
            kind: MarkupKind::Underline,
            quads: vec![words[0]],
            color: MarkupKind::Underline.default_color(),
            contents: Some("à préciser".into()),
        },
        Some("Relecture"),
    )
    .expect("soulignement");
    add_annotation(
        &doc,
        &pages[0],
        &NewAnnotation::Replace {
            quads: vec![words[1]],
            text: "nouveau".into(),
            strike: MarkupKind::StrikeOut.default_color(),
            caret: CARET_COLOR,
        },
        Some("Relecture"),
    )
    .expect("remplacement");

    let saved = doc.save_full().expect("enregistrement");
    let doc = Document::from_bytes(saved).expect("relecture");
    let pages = collect_pages(&doc).expect("pages");
    let list = list_annotations(&doc, &pages[0]).expect("inventaire");
    let ours: Vec<_> = list
        .iter()
        .filter(|a| a.author.as_deref() == Some("Relecture"))
        .collect();
    let kinds: Vec<&str> = ours.iter().map(|a| a.subtype.as_str()).collect();
    assert_eq!(kinds, ["Underline", "Caret", "StrikeOut"]);
    assert_eq!(ours[0].contents.as_deref(), Some("à préciser"));
    assert_eq!(ours[1].contents.as_deref(), Some("nouveau"));
    assert!(ours[2].is_group_member());
    assert_eq!(ours[2].in_reply_to, ours[1].reference);
    assert!(ours.iter().all(|a| a.has_appearance && a.name.is_some()));

    // Le rendu passe, et le soulignement se voit sous le premier mot.
    let scale = 2.0;
    let bmp = render_page(&doc, &pages[0], scale, &RenderOptions::default()).bitmap;
    let media = pages[0].media_box(&doc);
    let w = words[0];
    let y = w.y0 + 0.06 * w.height();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let px = bmp
        .pixel(
            (f64::midpoint(w.x0, w.x1) * scale) as u32,
            ((media.y1 - y) * scale) as u32,
        )
        .expect("pixel dans la page");
    assert!(
        px[2] > px[0] + 60,
        "trait bleu attendu sous le mot souligné, obtenu {px:?}"
    );

    // Le balisage ne touche pas au contenu de la page.
    let after = extract_page_text(&doc, &pages[0]).expect("texte");
    let after: Vec<String> = after.lines.iter().map(Line::text).collect();
    assert_eq!(before, after);
}
