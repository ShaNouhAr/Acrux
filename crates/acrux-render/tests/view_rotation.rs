//! Rotation de la vue : la page se dessine tournée **sans que le document
//! change**. C'est ce que fait Ctrl+Maj+Plus dans l'application, comme dans
//! Acrobat, par opposition à « pivoter la page », qui écrit `/Rotate`.
//!
//! Le fichier de corpus `synthese/liens-destinations-signets.pdf` a trois
//! pages de 300 × 200 points sans `/Rotate` : une page couchée, où l'échange
//! de la largeur et de la hauteur se voit tout de suite. Il est lu en place,
//! jamais modifié.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use acrux_document::{collect_pages, Document};
use acrux_render::{
    add_rotation, page_pixel_size, page_pixel_size_rotated, render_page, render_page_rotated,
    RenderOptions,
};

fn corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join("synthese")
        .join("liens-destinations-signets.pdf")
}

fn document() -> Document {
    Document::load(corpus()).expect("corpus des liens")
}

#[test]
fn la_rotation_ajoutee_est_ramenee_entre_0_et_359() {
    assert_eq!(add_rotation(0, 90), 90);
    assert_eq!(add_rotation(0, -90), 270, "un quart de tour à gauche");
    assert_eq!(add_rotation(270, 180), 90);
    assert_eq!(add_rotation(90, 270), 0, "un tour complet revient à droit");
    assert_eq!(add_rotation(180, -540), 0);
}

#[test]
fn un_quart_de_tour_echange_largeur_et_hauteur() {
    let doc = document();
    let pages = collect_pages(&doc).unwrap();
    let page = &pages[0];
    let straight = page_pixel_size(&doc, page, 1.0);
    assert_eq!(straight, (300, 200));
    assert_eq!(page_pixel_size_rotated(&doc, page, 1.0, 0), straight);
    assert_eq!(page_pixel_size_rotated(&doc, page, 1.0, 90), (200, 300));
    assert_eq!(page_pixel_size_rotated(&doc, page, 1.0, 180), (300, 200));
    assert_eq!(page_pixel_size_rotated(&doc, page, 1.0, 270), (200, 300));
    assert_eq!(
        page_pixel_size_rotated(&doc, page, 1.0, -90),
        (200, 300),
        "-90 vaut 270"
    );
}

#[test]
fn le_rendu_tourne_montre_la_page_couchee_debout() {
    let doc = document();
    let pages = collect_pages(&doc).unwrap();
    let page = &pages[0];
    let options = RenderOptions::default();
    let plain = render_page(&doc, page, 1.0, &options);
    let same = render_page_rotated(&doc, page, 1.0, 0, &options);
    assert_eq!(
        (plain.bitmap.width(), plain.bitmap.height()),
        (same.bitmap.width(), same.bitmap.height())
    );
    assert_eq!(
        plain.bitmap.data(),
        same.bitmap.data(),
        "sans rotation, le rendu est celui de toujours"
    );

    let turned = render_page_rotated(&doc, page, 1.0, 90, &options);
    assert_eq!((turned.bitmap.width(), turned.bitmap.height()), (200, 300));
    // Tourner d'un quart dans le sens horaire amène le haut de la page à
    // droite : le point PDF (0, 200), coin haut gauche, arrive en (200, 0).
    let corner = turned.base_ctm.apply(acrux_core::Point::new(0.0, 200.0));
    assert!((corner.x - 200.0).abs() < 1e-9 && corner.y.abs() < 1e-9);
    // La même encre, disposée autrement. L'anticrénelage n'échantillonne
    // pas les lignes et les colonnes de la même façon : le compte des
    // pixels touchés est à quelques pour cent près, pas au pixel.
    let ink = |data: &[u8]| data.chunks_exact(4).filter(|p| p[3] > 0).count();
    let (a, b) = (ink(plain.bitmap.data()), ink(turned.bitmap.data()));
    assert!(a > 500, "la page porte du texte et des cadres ({a} pixels)");
    assert!(
        a.abs_diff(b) * 100 < a * 5,
        "{a} pixels droits, {b} tournés"
    );
}
