//! Dessiner et écrire sur des fichiers réels du corpus : rectangle,
//! ellipse, flèche, polygone, encre, zone de texte et légende.
//!
//! Ce que vérifient ces tests :
//!
//! - les sept sortes se posent sur deux pages, se relisent après un
//!   enregistrement incrémental **et** complet, avec leur apparence ;
//! - les annotations qui étaient là restent intactes ;
//! - le rendu les montre là où elles ont été posées, y compris sur une page
//!   tournée de 90° : l'annotation est en espace de page, c'est le rendu
//!   qui tourne.
//!
//! Les fichiers du corpus sont lus en mémoire et jamais réécrits.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use acrux_core::{Point, Rect};
use acrux_document::{collect_pages, Document, Page};
use acrux_features::annotations::{
    add_annotation, list_annotations, Callout, LineEnding, NewAnnotation, ShapeStyle, TextAlign,
};
use acrux_features::stamp::StandardFont;
use acrux_render::page::base_matrix;
use acrux_render::{render_page, RenderOptions};

fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join(name)
}

fn load(name: &str) -> Document {
    let bytes = std::fs::read(corpus(name)).expect("fichier du corpus");
    Document::from_bytes(bytes).expect("ouverture")
}

fn red() -> ShapeStyle {
    ShapeStyle {
        stroke: Some([1.0, 0.0, 0.0]),
        fill: None,
        width: 3.0,
        opacity: 1.0,
    }
}

/// Les sept sortes, posées autour de `(x, y)`.
fn seven(x: f64, y: f64) -> Vec<NewAnnotation> {
    let r = |dx: f64, dy: f64, w: f64, h: f64| Rect::new(x + dx, y + dy, x + dx + w, y + dy + h);
    vec![
        NewAnnotation::Square {
            rect: r(0.0, 0.0, 80.0, 50.0),
            style: red(),
            contents: Some("À revoir".into()),
        },
        NewAnnotation::Circle {
            rect: r(100.0, 0.0, 60.0, 60.0),
            style: ShapeStyle {
                fill: Some([1.0, 1.0, 0.0]),
                opacity: 0.5,
                ..red()
            },
            contents: None,
        },
        NewAnnotation::Line {
            from: Point::new(x, y - 30.0),
            to: Point::new(x + 150.0, y - 30.0),
            style: red(),
            start: LineEnding::None,
            end: LineEnding::OpenArrow,
            contents: None,
        },
        NewAnnotation::Polygon {
            points: vec![
                Point::new(x + 180.0, y),
                Point::new(x + 220.0, y + 60.0),
                Point::new(x + 260.0, y),
            ],
            style: red(),
            contents: None,
        },
        NewAnnotation::Ink {
            strokes: vec![
                (0..40)
                    .map(|i| {
                        let t = f64::from(i);
                        Point::new(x + 3.0 * t, y - 80.0 + (t * 0.4).sin() * 10.0)
                    })
                    .collect(),
                vec![Point::new(x, y - 110.0), Point::new(x + 60.0, y - 100.0)],
            ],
            style: ShapeStyle {
                stroke: Some([0.0, 0.0, 1.0]),
                ..red()
            },
            contents: None,
        },
        NewAnnotation::FreeText {
            rect: r(140.0, -120.0, 150.0, 40.0),
            text: "Bonjour\nà tous les relecteurs".into(),
            font: StandardFont::Helvetica,
            size: 12.0,
            color: [0.0, 0.0, 0.0],
            border: Some(([1.0, 0.0, 0.0], 1.0)),
            fill: Some([1.0, 1.0, 0.85]),
            align: TextAlign::Left,
            callout: None,
        },
        NewAnnotation::FreeText {
            rect: r(300.0, -120.0, 100.0, 30.0),
            text: "Voir ici".into(),
            font: StandardFont::TimesRoman,
            size: 11.0,
            color: [0.0, 0.0, 1.0],
            border: Some(([0.0, 0.0, 1.0], 1.0)),
            fill: None,
            align: TextAlign::Center,
            callout: Some(Callout {
                anchor: Point::new(x + 250.0, y - 40.0),
                knee: None,
                ending: LineEnding::OpenArrow,
            }),
        },
    ]
}

const KINDS: [&str; 7] = [
    "Square", "Circle", "Line", "Polygon", "Ink", "FreeText", "FreeText",
];

fn subtypes(doc: &Document, page: &Page) -> Vec<String> {
    list_annotations(doc, page)
        .unwrap()
        .into_iter()
        .map(|a| a.subtype)
        .collect()
}

/// Nombre de pixels qui diffèrent entre deux rendus d'une même page.
fn changed(a: &Document, b: &Document, index: usize) -> usize {
    let pa = collect_pages(a).unwrap();
    let pb = collect_pages(b).unwrap();
    let ra = render_page(a, &pa[index], 1.0, &RenderOptions::default()).bitmap;
    let rb = render_page(b, &pb[index], 1.0, &RenderOptions::default()).bitmap;
    ra.data()
        .chunks_exact(4)
        .zip(rb.data().chunks_exact(4))
        .filter(|(x, y)| x != y)
        .count()
}

#[test]
fn sept_formes_sur_deux_pages_incremental_et_complet() {
    let name = "reels/chrome-skia-2pages-texte-tableau-svg.pdf";
    let original = load(name);
    let doc = load(name);
    let pages = collect_pages(&doc).unwrap();
    let before: Vec<Vec<String>> = pages.iter().map(|p| subtypes(&doc, p)).collect();
    for (index, page) in pages.iter().enumerate().take(2) {
        for annot in seven(60.0, 500.0) {
            add_annotation(&doc, page, &annot, Some("Corpus")).unwrap_or_else(|e| {
                panic!("page {index} : {annot:?} refusée : {e}");
            });
        }
    }
    let incremental = doc.save_incremental().expect("incrémental");
    let full = doc.save_full().expect("complet");
    for bytes in [incremental, full] {
        let d2 = Document::from_bytes(bytes).expect("relecture");
        let pages2 = collect_pages(&d2).unwrap();
        for (index, page) in pages2.iter().enumerate().take(2) {
            let list = list_annotations(&d2, page).unwrap();
            let kinds: Vec<&str> = list.iter().map(|a| a.subtype.as_str()).collect();
            // Ce qui était là d'abord, dans le même ordre ; puis les sept.
            assert_eq!(
                &kinds[..before[index].len()],
                before[index].iter().map(String::as_str).collect::<Vec<_>>(),
                "page {index} : annotations d'origine touchées"
            );
            assert_eq!(&kinds[before[index].len()..], KINDS, "page {index}");
            assert!(
                list[before[index].len()..].iter().all(|a| a.has_appearance),
                "page {index} : une annotation sans apparence"
            );
            assert!(
                changed(&original, &d2, index) > 2000,
                "page {index} : le rendu ne montre rien"
            );
        }
    }
}

#[test]
fn page_tournee_la_forme_est_au_bon_endroit() {
    let doc = load("reels/chrome-skia-rotate90-mise-a-jour-incrementale.pdf");
    let pages = collect_pages(&doc).unwrap();
    let page = &pages[0];
    let crop = page.crop_box(&doc);
    // Un disque plein au premier quart de la page.
    let center = Point::new(
        crop.x0 + crop.width() * 0.25,
        crop.y0 + crop.height() * 0.25,
    );
    add_annotation(
        &doc,
        page,
        &NewAnnotation::Circle {
            rect: Rect::new(
                center.x - 20.0,
                center.y - 20.0,
                center.x + 20.0,
                center.y + 20.0,
            ),
            style: ShapeStyle {
                stroke: None,
                fill: Some([0.0, 0.8, 0.0]),
                width: 0.0,
                opacity: 1.0,
            },
            contents: None,
        },
        None,
    )
    .unwrap();
    let d2 = Document::from_bytes(doc.save_full().unwrap()).unwrap();
    let pages2 = collect_pages(&d2).unwrap();
    let rendered = render_page(&d2, &pages2[0], 1.0, &RenderOptions::default());
    let bmp = rendered.bitmap;
    let m = base_matrix(
        &pages2[0].crop_box(&d2),
        1.0,
        pages2[0].rotate(&d2),
        bmp.width(),
        bmp.height(),
    );
    let at = m.apply(center);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let px = bmp.pixel(at.x as u32, at.y as u32).unwrap();
    assert!(
        px[1] > 150 && px[0] < 80 && px[2] < 80,
        "disque vert attendu en {at:?} : {px:?}"
    );
}

#[test]
fn ajout_sans_toucher_aux_annotations_sans_apparence() {
    let doc = load("synthese/annotations-sans-apparence.pdf");
    let pages = collect_pages(&doc).unwrap();
    let before: Vec<(String, bool)> = list_annotations(&doc, &pages[0])
        .unwrap()
        .into_iter()
        .map(|a| (a.subtype, a.has_appearance))
        .collect();
    assert!(!before.is_empty());
    for annot in seven(50.0, 300.0) {
        add_annotation(&doc, &pages[0], &annot, None).unwrap();
    }
    let d2 = Document::from_bytes(doc.save_full().unwrap()).unwrap();
    let after: Vec<(String, bool)> = list_annotations(&d2, &collect_pages(&d2).unwrap()[0])
        .unwrap()
        .into_iter()
        .map(|a| (a.subtype, a.has_appearance))
        .collect();
    assert_eq!(after.len(), before.len() + 7);
    assert_eq!(&after[..before.len()], &before[..]);
}
