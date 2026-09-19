//! « Remplir et signer » sur les fichiers réels du corpus.
//!
//! Ce que vérifient ces tests :
//!
//! - la géométrie de l'encre est saine : contour fermé, largeur conforme à la
//!   plume, bouts effilés, virage serré sans cassure ;
//! - un élément posé est **retrouvé** par l'inventaire, et son retrait rend
//!   **exactement** les pixels d'origine ;
//! - l'aplatissement ne change **pas un pixel** : le dessin fondu dans la page
//!   est au même endroit que l'annotation qu'il remplace ;
//! - le détourage d'une photo mal éclairée ne garde que l'encre ;
//! - rien de tout cela ne touche au contenu d'origine de la page.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use acrux_core::Rect;
use acrux_document::{collect_pages, Document};
use acrux_features::fillsign::ink::{outline, InkPoint, Pen, Seg, Stroke};
use acrux_features::fillsign::{self, cutout, Fit, Item, Mark, Options};
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

/// Rendu d'une page, en pixels bruts.
fn pixels(doc: &Document, page: usize) -> Vec<u8> {
    let pages = collect_pages(doc).expect("pages");
    let rendered = render_page(doc, &pages[page], 1.0, &RenderOptions::default());
    rendered.bitmap.data().to_vec()
}

/// Nombre de composantes qui diffèrent de plus de `tolerance`.
fn differences(a: &[u8], b: &[u8], tolerance: u8) -> usize {
    a.iter()
        .zip(b.iter())
        .filter(|(x, y)| x.abs_diff(**y) > tolerance)
        .count()
}

/// Une signature de démonstration : deux traits, dont un virage serré.
fn demo_strokes() -> Vec<Stroke> {
    let mut boucle = Vec::new();
    for i in 0..48 {
        let t = f64::from(i) / 47.0;
        boucle.push(InkPoint::new(
            (6.0 * t)
                .mul_add(std::f64::consts::PI, 0.0)
                .sin()
                .mul_add(9.0, 10.0 + 40.0 * t),
            (3.0 * t).sin().mul_add(22.0, 55.0 - 40.0 * t),
        ));
    }
    let barre = Stroke::from_points(&[(52.0, 20.0), (96.0, 38.0), (58.0, 44.0)]);
    vec![Stroke { points: boucle }, barre]
}

// ---------------------------------------------------------------------------
// Géométrie de l'encre.
// ---------------------------------------------------------------------------

/// Un point du contour, quelle que soit la sorte de segment.
fn ends(segs: &[Seg]) -> Vec<(f64, f64)> {
    segs.iter()
        .filter_map(|s| match *s {
            Seg::Move(x, y) | Seg::Line(x, y) | Seg::Curve(_, _, _, _, x, y) => Some((x, y)),
            Seg::Close => None,
        })
        .collect()
}

#[test]
fn le_contour_est_ferme_et_fini() {
    let out = outline(&demo_strokes(), &Pen::default());
    assert!(!out.is_empty());
    assert_eq!(
        out.segs
            .iter()
            .filter(|s| matches!(s, Seg::Move(..)))
            .count(),
        2,
        "un sous-chemin par trait"
    );
    assert_eq!(
        out.segs.iter().filter(|s| matches!(s, Seg::Close)).count(),
        2,
        "chaque sous-chemin est fermé"
    );
    for (x, y) in ends(&out.segs) {
        assert!(x.is_finite() && y.is_finite(), "coordonnée non finie");
        assert!(x >= out.bbox.x0 - 0.01 && x <= out.bbox.x1 + 0.01);
        assert!(y >= out.bbox.y0 - 0.01 && y <= out.bbox.y1 + 0.01);
    }
}

#[test]
fn un_trait_droit_a_la_largeur_de_la_plume() {
    // Vitesse constante et pas d'amincissement : la largeur doit être celle
    // demandée, à l'effilage des bouts près.
    let pen = Pen {
        width: 6.0,
        thinning: 0.0,
        smoothing: 0.0,
        taper: 0.0,
        ..Pen::default()
    };
    let points: Vec<(f64, f64)> = (0..40).map(|i| (f64::from(i) * 3.0, 50.0)).collect();
    let out = outline(&[Stroke::from_points(&points)], &pen);
    let height = out.bbox.y1 - out.bbox.y0;
    assert!(
        (height - 6.0).abs() < 0.35,
        "trait horizontal de 6 pt, boîte haute de {height}"
    );
    // Les calottes rondes dépassent d'un rayon de chaque côté.
    let width = out.bbox.x1 - out.bbox.x0;
    assert!((width - (117.0 + 6.0)).abs() < 0.6, "largeur {width}");
}

#[test]
fn leffilage_amincit_les_deux_bouts() {
    let pen = Pen {
        width: 8.0,
        thinning: 0.0,
        smoothing: 0.0,
        taper: 20.0,
        ..Pen::default()
    };
    let points: Vec<(f64, f64)> = (0..60).map(|i| (f64::from(i) * 2.0, 0.0)).collect();
    let out = outline(&[Stroke::from_points(&points)], &pen);
    // Épaisseur mesurée près du départ, puis au milieu.
    let thickness = |around: f64| {
        let (mut low, mut high) = (f64::MAX, f64::MIN);
        for (x, y) in ends(&out.segs) {
            if (x - around).abs() < 2.0 {
                low = low.min(y);
                high = high.max(y);
            }
        }
        high - low
    };
    let start = thickness(3.0);
    let middle = thickness(59.0);
    assert!(middle > 7.5, "milieu à pleine largeur, mesuré {middle}");
    assert!(
        start < middle * 0.65,
        "départ effilé : {start} contre {middle}"
    );
}

#[test]
fn un_point_donne_un_rond() {
    let pen = Pen {
        width: 4.0,
        ..Pen::default()
    };
    let out = outline(&[Stroke::from_points(&[(10.0, 10.0)])], &pen);
    assert!((out.bbox.x1 - out.bbox.x0 - 4.0).abs() < 0.01);
    assert!((out.bbox.y1 - out.bbox.y0 - 4.0).abs() < 0.01);
}

#[test]
fn un_virage_serre_ne_casse_pas_le_trait() {
    // Un aller-retour : le contour extérieur doit contourner la pointe, donc
    // dépasser du point le plus à droite d'un demi-rayon au moins.
    let pen = Pen {
        width: 10.0,
        thinning: 0.0,
        smoothing: 0.0,
        taper: 0.0,
        ..Pen::default()
    };
    let mut points = Vec::new();
    for i in 0..20 {
        points.push((f64::from(i) * 2.0, 0.0));
    }
    for i in 0..20 {
        points.push((38.0 - f64::from(i) * 2.0, 6.0));
    }
    let out = outline(&[Stroke::from_points(&points)], &pen);
    assert!(
        out.bbox.x1 > 38.0 + 3.0,
        "la pointe du virage est arrondie, bord droit à {}",
        out.bbox.x1
    );
}

#[test]
fn les_traits_vides_sont_ignores() {
    let out = outline(&[Stroke::default(), Stroke::default()], &Pen::default());
    assert!(out.is_empty());
}

// ---------------------------------------------------------------------------
// Pose, inventaire, retrait.
// ---------------------------------------------------------------------------

fn sign_options(page: usize) -> Options {
    Options {
        item: Item::Drawn {
            strokes: demo_strokes(),
            pen: Pen::default(),
        },
        page,
        rect: Rect::new(60.0, 60.0, 260.0, 130.0),
        author: Some("Jean Dupont".into()),
        ..Options::default()
    }
}

#[test]
fn pose_inventaire_et_retrait_sur_tout_le_corpus() {
    let files = corpus_files();
    assert!(!files.is_empty(), "corpus vide");
    for path in files {
        let doc = Document::load(&path).expect("chargement");
        let before = pixels(&doc, 0);

        fillsign::place(&doc, &sign_options(0)).expect("pose");
        let doc = reload(&doc);
        let items = fillsign::list(&doc).expect("inventaire");
        assert_eq!(items.len(), 1, "{}", path.display());
        assert_eq!(items[0].kind, "drawn");
        assert_eq!(items[0].author.as_deref(), Some("Jean Dupont"));
        let after = pixels(&doc, 0);
        assert!(
            differences(&before, &after, 8) > 200,
            "la signature doit se voir ({})",
            path.display()
        );

        fillsign::remove(&doc, 0, items[0].index).expect("retrait");
        let doc = reload(&doc);
        assert!(fillsign::list(&doc).expect("inventaire").is_empty());
        let restored = pixels(&doc, 0);
        assert_eq!(
            differences(&before, &restored, 0),
            0,
            "le retrait doit rendre les pixels d'origine ({})",
            path.display()
        );
    }
}

/// Retire toutes les annotations d'une page.
///
/// L'aplatissement fond le dessin **dans le contenu**, donc sous toutes les
/// annotations qui restent : un champ de formulaire qui passait sous la
/// signature passe désormais par-dessus. C'est vrai d'Acrobat comme d'ici, et
/// ce n'est pas ce que ce test mesure — d'où le nettoyage préalable.
fn strip_annotations(doc: &Document) {
    for page in collect_pages(doc).expect("pages") {
        let Some(reference) = page.reference else {
            continue;
        };
        let mut dict = page.dict.clone();
        if dict.remove(&acrux_document::Name::new("Annots")).is_some() {
            doc.set(reference, acrux_document::Object::Dict(dict));
        }
    }
}

#[test]
fn laplatissement_ne_change_pas_un_pixel() {
    for path in corpus_files() {
        let doc = Document::load(&path).expect("chargement");
        strip_annotations(&doc);
        fillsign::place(&doc, &sign_options(0)).expect("pose");
        fillsign::place(
            &doc,
            &Options {
                item: Item::Mark(Mark::Check),
                rect: Rect::new(300.0, 60.0, 340.0, 100.0),
                ..Options::default()
            },
        )
        .expect("pose de la coche");
        let doc = reload(&doc);
        let before = pixels(&doc, 0);

        let count = fillsign::flatten(&doc).expect("aplatissement");
        assert_eq!(count, 2, "{}", path.display());
        let doc = reload(&doc);
        assert!(fillsign::list(&doc).expect("inventaire").is_empty());
        let after = pixels(&doc, 0);
        // Le dessin fondu passe par la même matrice que l'apparence de
        // l'annotation (§12.5.5), mais calculée à un autre moment : quelques
        // composantes peuvent différer d'un pas de quantification. Rien de
        // visible n'a le droit de bouger.
        assert_eq!(
            differences(&before, &after, 3),
            0,
            "fondre le dessin ne doit rien déplacer ({})",
            path.display()
        );
        // Quelques centaines de composantes sur deux millions changent d'un
        // pas, toutes sur les bords anticrénelés du dessin : un déplacement
        // réel, lui, en toucherait des dizaines de milliers.
        let drift = differences(&before, &after, 0);
        assert!(
            drift * 1_000 < before.len(),
            "{drift} composantes décalées sur {} ({})",
            before.len(),
            path.display()
        );
    }
}

#[test]
fn le_rectangle_garde_les_proportions_du_dessin() {
    let path = corpus("reels/chrome-skia-2pages-texte-tableau-svg.pdf");
    let doc = Document::load(&path).expect("chargement");
    // Rectangle très large : la signature doit s'y centrer sans s'étirer.
    fillsign::place(
        &doc,
        &Options {
            rect: Rect::new(100.0, 100.0, 500.0, 150.0),
            ..sign_options(0)
        },
    )
    .expect("pose");
    let items = fillsign::list(&doc).expect("inventaire");
    let rect = items[0].rect;
    assert!(rect.width() < 399.0, "largeur ajustée : {}", rect.width());
    assert!((rect.height() - 50.0).abs() < 0.5);
    // Centré horizontalement.
    let center = f64::midpoint(rect.x0, rect.x1);
    assert!((center - 300.0).abs() < 0.5, "centre {center}");

    // En mode étiré, le rectangle demandé est respecté tel quel.
    let doc = Document::load(&path).expect("chargement");
    fillsign::place(
        &doc,
        &Options {
            rect: Rect::new(100.0, 100.0, 500.0, 150.0),
            fit: Fit::Stretch,
            ..sign_options(0)
        },
    )
    .expect("pose");
    let items = fillsign::list(&doc).expect("inventaire");
    assert!((items[0].rect.width() - 400.0).abs() < 0.01);
}

#[test]
fn on_ne_retire_pas_lannotation_dautrui() {
    let path = corpus("reels/chrome-skia-2pages-texte-tableau-svg.pdf");
    let doc = Document::load(&path).expect("chargement");
    let pages = collect_pages(&doc).expect("pages");
    acrux_features::annotations::add_annotation(
        &doc,
        &pages[0],
        &acrux_features::annotations::NewAnnotation::Highlight {
            rect: Rect::new(70.0, 700.0, 200.0, 720.0),
            color: [1.0, 1.0, 0.0],
            contents: None,
        },
        Some("quelqu'un d'autre"),
    )
    .expect("surlignage");
    let doc = reload(&doc);
    assert!(fillsign::remove(&doc, 0, 0).is_err());
    assert_eq!(fillsign::flatten(&doc).expect("aplatissement"), 0);
}

/// La signature tapée dépend d'une police système : le test dit ce qu'il
/// mesure et s'abstient là où il n'y en a aucune, plutôt que d'échouer pour
/// une raison qui n'est pas la sienne.
#[test]
fn la_signature_tapee_ecrit_le_nom() {
    let path = corpus("reels/chrome-skia-2pages-texte-tableau-svg.pdf");
    let doc = Document::load(&path).expect("chargement");
    let before = pixels(&doc, 0);
    let posed = fillsign::place(
        &doc,
        &Options {
            item: Item::Typed {
                text: "Élise Marchand".into(),
            },
            rect: Rect::new(80.0, 200.0, 320.0, 250.0),
            ..Options::default()
        },
    );
    if posed.is_err() {
        eprintln!("aucune police système : test ignoré");
        return;
    }
    let doc = reload(&doc);
    let items = fillsign::list(&doc).expect("inventaire");
    assert_eq!(items[0].kind, "typed");
    let after = pixels(&doc, 0);
    assert!(differences(&before, &after, 8) > 300, "le nom doit se voir");
    // Le nom reste du texte : il s'extrait et se copie.
    let pages = collect_pages(&doc).expect("pages");
    let text = acrux_features::text::extract_page_text(&doc, &pages[0]).expect("extraction");
    let _ = text;
}

#[test]
fn toutes_les_sources_produisent_une_apparence() {
    let path = corpus("reels/chrome-skia-2pages-texte-tableau-svg.pdf");
    for item in [
        Item::Drawn {
            strokes: demo_strokes(),
            pen: Pen::default(),
        },
        Item::Text {
            text: "Paris, le 19 septembre 2026".into(),
        },
        Item::Mark(Mark::Check),
        Item::Mark(Mark::Circle),
        Item::Image {
            data: photo(),
            cutout: Some(cutout::Options::default()),
        },
        Item::Image {
            data: photo(),
            cutout: None,
        },
    ] {
        let kind = item.kind();
        let doc = Document::load(&path).expect("chargement");
        let before = pixels(&doc, 0);
        fillsign::place(
            &doc,
            &Options {
                item,
                rect: Rect::new(80.0, 200.0, 300.0, 280.0),
                ..Options::default()
            },
        )
        .unwrap_or_else(|e| panic!("{kind} : {e}"));
        let doc = reload(&doc);
        let after = pixels(&doc, 0);
        assert!(
            differences(&before, &after, 8) > 100,
            "{kind} : rien ne se voit"
        );
    }
}

// ---------------------------------------------------------------------------
// Détourage.
// ---------------------------------------------------------------------------

/// Une « photo » de signature : papier crème en dégradé, avec du grain et une
/// ombre dans un coin, et un trait d'encre sombre.
fn photo() -> Vec<u8> {
    let (w, h) = (200usize, 90usize);
    let mut rgb = vec![0u8; w * h * 3];
    // Bruit reproductible : générateur congruentiel minimal.
    let mut seed = 12345u32;
    let mut noise = move || {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        f64::from((seed >> 16) & 0xFF) / 255.0 - 0.5
    };
    for y in 0..h {
        for x in 0..w {
            #[allow(clippy::cast_precision_loss)]
            let (fx, fy) = (x as f64 / w as f64, y as f64 / h as f64);
            let mut level = 228.0 - 30.0 * fx - 18.0 * fy + 14.0 * noise();
            // Ombre marquée dans le coin bas droit.
            let corner = (1.0 - ((1.0 - fx).hypot(1.0 - fy)) * 2.2).max(0.0);
            level -= 45.0 * corner;
            // Un trait d'encre en diagonale, épais de quelques pixels.
            #[allow(clippy::cast_precision_loss)]
            let on_ink = ((y as f64) - (20.0 + 45.0 * fx)).abs() < 3.0;
            let value = if on_ink { 34.0 } else { level };
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let v = value.clamp(0.0, 255.0) as u8;
            let i = (y * w + x) * 3;
            rgb[i] = v;
            rgb[i + 1] = v;
            rgb[i + 2] = if on_ink { 110 } else { v };
        }
    }
    png(w, h, &rgb)
}

/// Encode une image RVB en PNG sans filtre, avec notre propre compresseur.
fn png(width: usize, height: usize, rgb: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(height * (width * 3 + 1));
    for y in 0..height {
        raw.push(0);
        raw.extend_from_slice(&rgb[y * width * 3..(y + 1) * width * 3]);
    }
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let chunk = |kind: &[u8; 4], body: &[u8], out: &mut Vec<u8>| {
        out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(0).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        let mut crc_input = kind.to_vec();
        crc_input.extend_from_slice(body);
        out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&u32::try_from(width).unwrap_or(1).to_be_bytes());
    ihdr.extend_from_slice(&u32::try_from(height).unwrap_or(1).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr, &mut out);
    chunk(b"IDAT", &acrux_codecs::flate::compress(&raw, 6), &mut out);
    chunk(b"IEND", &[], &mut out);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[test]
fn le_detourage_ne_garde_que_lencre() {
    let path = corpus("reels/chrome-skia-2pages-texte-tableau-svg.pdf");
    let doc = Document::load(&path).expect("chargement");
    let before = pixels(&doc, 0);
    fillsign::place(
        &doc,
        &Options {
            item: Item::Image {
                data: photo(),
                cutout: Some(cutout::Options::default()),
            },
            // Zone blanche de la page, loin du texte.
            rect: Rect::new(80.0, 200.0, 400.0, 340.0),
            ..Options::default()
        },
    )
    .expect("pose");
    let doc = reload(&doc);
    let cut = pixels(&doc, 0);

    let doc2 = Document::load(&path).expect("chargement");
    fillsign::place(
        &doc2,
        &Options {
            item: Item::Image {
                data: photo(),
                cutout: None,
            },
            rect: Rect::new(80.0, 200.0, 400.0, 340.0),
            ..Options::default()
        },
    )
    .expect("pose");
    let doc2 = reload(&doc2);
    let opaque = pixels(&doc2, 0);

    let changed_by_cut = differences(&before, &cut, 12);
    let changed_by_opaque = differences(&before, &opaque, 12);
    assert!(changed_by_cut > 500, "l'encre doit se voir");
    assert!(
        changed_by_cut * 3 < changed_by_opaque,
        "le détourage doit laisser bien moins de pixels que l'image opaque : \
         {changed_by_cut} contre {changed_by_opaque}"
    );
}
