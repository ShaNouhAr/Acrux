//! Tampons et images posés sur les fichiers réels du corpus.
//!
//! Ce que vérifient ces tests, fichier par fichier :
//!
//! - une image posée par `edit_objects::add_image` tombe **exactement** où on
//!   la veut, quel que soit le producteur (le `cm` racine jamais refermé de
//!   Chrome, des `q` laissés ouverts, une page tournée) : l'inventaire la
//!   retrouve à sa place et le rendu la montre en son centre ;
//! - hors de l'image, le rendu du contenu ne change **pas d'un pixel** : rien
//!   d'autre n'a été réécrit ;
//! - un tampon posé par `rubber_stamp::place` survit à l'enregistrement, se
//!   voit au rendu de sa couleur, et ne se confond pas avec un élément de
//!   « remplir et signer ».

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use acrux_core::{Point, Rect};
use acrux_document::{collect_pages, Document};
use acrux_features::annotations::AnnotMeta;
use acrux_features::edit_objects::{self, Kind};
use acrux_features::rubber_stamp::{self, Options, Source, StandardStamp};
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

/// Les PDF du corpus qui s'ouvrent sans mot de passe et ne sont pas cassés.
fn corpus_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ["reels", "synthese"] {
        let Ok(entries) = std::fs::read_dir(corpus(dir)) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = p.to_string_lossy().to_string();
            if p.extension().is_some_and(|x| x == "pdf")
                && !name.contains("mdp-")
                && !name.contains("casse")
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn reload(doc: &Document) -> Document {
    let bytes = doc
        .save_incremental()
        .or_else(|_| doc.save_full())
        .expect("enregistrement");
    Document::from_bytes(bytes).expect("relecture")
}

/// Rendu de la première page à une échelle, annotations comprises ou non.
fn render(doc: &Document, annotations: bool) -> acrux_graphics::Bitmap {
    let pages = collect_pages(doc).expect("pages");
    let options = RenderOptions {
        annotations,
        background: Some(acrux_graphics::Color::WHITE),
        ..RenderOptions::default()
    };
    render_page(doc, &pages[0], 1.0, &options).bitmap
}

/// Image d'un rouge franc, 8 × 6 pixels.
fn red() -> acrux_features::stamp::PreparedImage {
    let png = acrux_graphics::encode_png_rgb(8, 6, &[255, 0, 0].repeat(48));
    acrux_features::stamp::prepare_image(&png).unwrap()
}

#[test]
fn une_image_tombe_exactement_ou_on_la_veut_et_rien_dautre_ne_bouge() {
    let mut checked = 0;
    for path in corpus_files() {
        let Ok(doc) = Document::load(&path) else {
            continue;
        };
        if doc.needs_password() {
            continue;
        }
        let name = path.display();
        let Ok(pages) = collect_pages(&doc) else {
            continue;
        };
        let Some(page) = pages.first() else { continue };
        let crop = page.crop_box(&doc);
        let rotate = page.rotate(&doc);
        let before = render(&doc, false);
        let center = Point::new(
            f64::midpoint(crop.x0, crop.x1),
            f64::midpoint(crop.y0, crop.y1),
        );
        let size = (48.0, 36.0);
        let index = edit_objects::add_image(&doc, page, &red(), center, size)
            .unwrap_or_else(|e| panic!("{name} : {e}"));
        let doc = reload(&doc);
        let pages = collect_pages(&doc).unwrap();
        let objects = edit_objects::list(&doc, &pages[0]).unwrap();
        let image = objects
            .get(index)
            .unwrap_or_else(|| panic!("{name} : objet {index} absent"));
        assert_eq!(image.kind, Kind::Image, "{name}");
        // Une page tournée d'un quart de tour échange les côtés.
        let (w, h) = if matches!(rotate, 90 | 270) {
            (size.1, size.0)
        } else {
            size
        };
        let wanted = Rect::new(
            center.x - w / 2.0,
            center.y - h / 2.0,
            center.x + w / 2.0,
            center.y + h / 2.0,
        );
        for (got, want) in [
            (image.bbox.x0, wanted.x0),
            (image.bbox.y0, wanted.y0),
            (image.bbox.x1, wanted.x1),
            (image.bbox.y1, wanted.y1),
        ] {
            assert!((got - want).abs() < 1e-2, "{name} : {:?}", image.bbox);
        }
        // Au rendu : du rouge au centre, et rien de changé ailleurs.
        let after = render(&doc, false);
        assert_eq!(
            (before.width(), before.height()),
            (after.width(), after.height())
        );
        let to_pixels = base_matrix(&crop, 1.0, rotate, after.width(), after.height());
        let middle = to_pixels.apply(center);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let red_px = after.pixel(middle.x as u32, middle.y as u32).unwrap();
        assert!(
            red_px[0] > 200 && red_px[1] < 60 && red_px[2] < 60,
            "{name} : {red_px:?}"
        );
        let zone = to_pixels.transform_rect(&wanted);
        let mut changed = 0usize;
        for y in 0..after.height() {
            for x in 0..after.width() {
                let (fx, fy) = (f64::from(x), f64::from(y));
                if fx >= zone.x0 - 2.0
                    && fx <= zone.x1 + 2.0
                    && fy >= zone.y0 - 2.0
                    && fy <= zone.y1 + 2.0
                {
                    continue;
                }
                let (was, now) = (before.pixel(x, y).unwrap(), after.pixel(x, y).unwrap());
                if was[..3] != now[..3] {
                    changed += 1;
                }
            }
        }
        assert_eq!(changed, 0, "{name} : pixels changés hors de l'image");
        checked += 1;
    }
    assert!(checked >= 10, "trop peu de fichiers éprouvés : {checked}");
}

#[test]
fn un_tampon_se_pose_sur_chaque_fichier_et_se_voit() {
    let mut checked = 0;
    for path in corpus_files() {
        let Ok(doc) = Document::load(&path) else {
            continue;
        };
        if doc.needs_password() || doc.security().is_some() {
            continue;
        }
        let name = path.display();
        let Ok(pages) = collect_pages(&doc) else {
            continue;
        };
        let Some(page) = pages.first() else { continue };
        let crop = page.crop_box(&doc);
        let rotate = page.rotate(&doc);
        let natural = rubber_stamp::natural_size(
            &Source::Standard(StandardStamp::Approved),
            rubber_stamp::Language::Fr,
            None,
        );
        let options = Options {
            meta: AnnotMeta::fresh(Some("Essai")),
            ..Options::new(
                Source::Standard(StandardStamp::Approved),
                0,
                rubber_stamp::top_right(crop, rotate, natural, 36.0),
            )
        };
        rubber_stamp::place(&doc, &options).unwrap_or_else(|e| panic!("{name} : {e}"));
        let doc = reload(&doc);
        let placed = rubber_stamp::list(&doc).unwrap();
        let ours: Vec<_> = placed.iter().filter(|p| p.page == 0).collect();
        assert_eq!(ours.len(), 1, "{name}");
        let annots =
            acrux_features::annotations::list_annotations(&doc, &collect_pages(&doc).unwrap()[0])
                .unwrap();
        let info = annots
            .iter()
            .find(|a| a.index == ours[0].index)
            .unwrap_or_else(|| panic!("{name} : annotation absente"));
        assert_eq!(info.subtype, "Stamp", "{name}");
        assert!(!info.fill_sign, "{name}");
        // Le cadre vert se voit, là où le tampon est posé.
        let bitmap = render(&doc, true);
        let m = base_matrix(&crop, 1.0, rotate, bitmap.width(), bitmap.height());
        let zone = m.transform_rect(&ours[0].rect);
        let mut green = 0usize;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        for y in zone.y0.max(0.0) as u32..(zone.y1 as u32).min(bitmap.height()) {
            for x in zone.x0.max(0.0) as u32..(zone.x1 as u32).min(bitmap.width()) {
                let p = bitmap.pixel(x, y).unwrap().map(i32::from);
                if p[1] > p[0] + 40 && p[1] > p[2] + 40 {
                    green += 1;
                }
            }
        }
        assert!(green > 100, "{name} : {green} pixels verts");
        checked += 1;
    }
    assert!(checked >= 10, "trop peu de fichiers éprouvés : {checked}");
}
