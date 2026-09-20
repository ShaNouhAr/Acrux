//! Tests de bout en bout de la création de documents
//! (`acrux_features::create`).
//!
//! Ce que vérifie ce fichier, sur chaque document produit :
//!
//! - il **se relit** par `Document::from_bytes` après enregistrement ;
//! - **tous ses objets sont lisibles** et tous ses flux décodables — ce que
//!   fait `acr check` ;
//! - il **se rend sans le moindre avertissement** du moteur
//!   (`acrux_render::render_page`) ;
//! - son texte se **réextrait identique** au texte d'entrée, aux fins de
//!   ligne près ;
//! - les deux fichiers de corpus du module sont conformes à ce que le
//!   générateur en dit.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use acrux_document::{collect_pages, Document, Object};
use acrux_features::create::{
    combine, from_images, from_markdown, from_text, new_document, CombineInput, CombineOptions,
    CombineSource, Fit, ImageInput, ImageLayout, Margins, PageSetup, PageSize, TextAlign,
    TextLayout,
};
use acrux_features::navigation::{outline, page_links, PageIndex};
use acrux_features::text::{extract_page_text, Line};
use acrux_render::{render_page, RenderOptions};

fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join("synthese")
        .join(name)
}

/// Enregistre puis relit le document, comme le ferait un afficheur.
fn roundtrip(doc: &Document) -> Document {
    let bytes = doc.save_full().unwrap();
    assert!(bytes.starts_with(b"%PDF-"), "en-tête PDF absente");
    Document::from_bytes(bytes).unwrap()
}

/// Ce que fait `acr check` : lire tous les objets et décoder tous les flux.
fn check(doc: &Document, label: &str) {
    for number in doc.object_numbers() {
        let reference = acrux_document::ObjectRef {
            number,
            generation: 0,
        };
        let object = doc
            .get(reference)
            .unwrap_or_else(|e| panic!("{label} : objet {number} illisible ({e})"));
        if matches!(object.as_ref(), Object::Stream { .. }) {
            doc.stream_data(&object)
                .unwrap_or_else(|e| panic!("{label} : flux {number} non décodable ({e})"));
        }
    }
    assert!(doc.warnings().is_empty(), "{label} : {:?}", doc.warnings());
}

/// Rend toutes les pages et exige zéro avertissement du moteur.
fn renders_cleanly(doc: &Document, label: &str) {
    let options = RenderOptions {
        time_budget: Some(std::time::Duration::from_secs(30)),
        ..RenderOptions::default()
    };
    for (i, page) in collect_pages(doc).unwrap().iter().enumerate() {
        let rendered = render_page(doc, page, 1.0, &options);
        assert!(
            rendered.warnings.is_empty(),
            "{label} page {} : {:?}",
            i + 1,
            rendered.warnings
        );
        assert!(rendered.bitmap.width() > 0 && rendered.bitmap.height() > 0);
    }
}

/// Texte du document, une ligne extraite par ligne composée.
fn text_of(doc: &Document) -> Vec<String> {
    let mut out = Vec::new();
    for page in collect_pages(doc).unwrap() {
        for line in extract_page_text(doc, &page).unwrap().lines {
            out.push(Line::text(&line));
        }
    }
    out
}

/// Les trois vérifications communes à tout document produit.
fn sound(doc: &Document, label: &str) -> Document {
    let reread = roundtrip(doc);
    check(&reread, label);
    renders_cleanly(&reread, label);
    reread
}

// --- Documents vierges ------------------------------------------------------

#[test]
fn every_named_format_produces_a_sound_document() {
    for size in [
        PageSize::A6,
        PageSize::A5,
        PageSize::A4,
        PageSize::Letter,
        PageSize::Legal,
        PageSize::EnvelopeDl,
        PageSize::EnvelopeC6,
        PageSize::Custom {
            width: 240.0,
            height: 180.0,
        },
    ] {
        let setup = PageSetup {
            size,
            pages: 2,
            ..PageSetup::default()
        };
        let doc = new_document(&setup).unwrap();
        let reread = sound(&doc, &format!("vierge {}", size.name()));
        let pages = collect_pages(&reread).unwrap();
        assert_eq!(pages.len(), 2);
        let (w, h) = setup.page_size();
        let b = pages[0].media_box(&reread);
        assert!((b.width() - w).abs() < 0.01 && (b.height() - h).abs() < 0.01);
    }
}

// --- Texte ------------------------------------------------------------------

/// Le texte d'essai : deux paragraphes assez longs pour se couper, plus une
/// ligne vide. Aucun mot n'est plus long qu'une ligne, la césure ne peut donc
/// rien ajouter.
const SAMPLE: &str = "Le petit chat boit du lait tous les matins dans la cuisine ensoleillée \
     de la vieille maison de campagne, puis il dort jusqu'au soir sur le rebord de la fenêtre.\n\
     \n\
     Une deuxième ligne, plus courte, mais qui contient des accents : à é î ô ù, et de la \
     ponctuation — tirets, « guillemets », parenthèses (comme celles-ci).\n\
     Fin.";

/// Les lignes du texte d'entrée, mots normalisés comme le fait la mise en page.
fn words_of(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

#[test]
fn a_text_document_gives_back_exactly_its_words_in_order() {
    for align in [
        TextAlign::Left,
        TextAlign::Right,
        TextAlign::Center,
        TextAlign::Justify,
    ] {
        let layout = TextLayout {
            align,
            hyphenate: false,
            ..TextLayout::default()
        };
        let doc = from_text(SAMPLE, &layout).unwrap();
        let reread = sound(&doc, &format!("texte {align:?}"));
        let extracted: Vec<String> = text_of(&reread)
            .join(" ")
            .split_whitespace()
            .map(str::to_string)
            .collect();
        assert_eq!(extracted, words_of(SAMPLE), "alignement {align:?}");
    }
}

#[test]
fn a_long_text_paginates_and_keeps_every_word() {
    let body: String = (0..120)
        .map(|i| format!("Paragraphe numéro {i}, avec assez de mots pour occuper une ligne entière de la page."))
        .collect::<Vec<_>>()
        .join("\n");
    let layout = TextLayout {
        setup: PageSetup {
            size: PageSize::A6,
            ..PageSetup::default()
        },
        hyphenate: false,
        ..TextLayout::default()
    };
    let doc = from_text(&body, &layout).unwrap();
    let reread = sound(&doc, "texte long");
    assert!(collect_pages(&reread).unwrap().len() > 3, "il faut paginer");
    let extracted: Vec<String> = text_of(&reread)
        .join(" ")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    assert_eq!(extracted, words_of(&body));
}

#[test]
fn margins_are_respected_to_the_point() {
    let margins = Margins {
        top: 40.0,
        bottom: 60.0,
        left: 70.0,
        right: 30.0,
    };
    let layout = TextLayout {
        setup: PageSetup {
            margins,
            ..PageSetup::default()
        },
        hyphenate: false,
        ..TextLayout::default()
    };
    let doc = from_text(SAMPLE, &layout).unwrap();
    let reread = sound(&doc, "marges");
    let area = layout.setup.content_box();
    for page in collect_pages(&reread).unwrap() {
        let text = extract_page_text(&reread, &page).unwrap();
        for line in &text.lines {
            assert!(
                line.bbox.x0 >= area.x0 - 0.5 && line.bbox.x1 <= area.x1 + 0.5,
                "ligne hors des marges latérales : {:?} dans {area:?}",
                line.bbox
            );
            assert!(
                line.bbox.y0 >= area.y0 - 3.0 && line.bbox.y1 <= area.y1 + 0.5,
                "ligne hors des marges verticales : {:?} dans {area:?}",
                line.bbox
            );
        }
    }
}

// --- Images -----------------------------------------------------------------

/// JPEG produit par notre encodeur.
#[allow(clippy::cast_possible_truncation)]
fn jpeg(w: u32, h: u32) -> Vec<u8> {
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            rgb.extend_from_slice(&[(x * 5) as u8, 120, (y * 3) as u8]);
        }
    }
    acrux_codecs::dct::encode::encode(&rgb, w, h, 80, true)
}

/// PNG RVBA à coin transparent.
fn rgba_png(w: u32, h: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let alpha = if x < w / 2 && y < h / 2 { 0 } else { 255 };
            pixels.extend_from_slice(&[10, 200, 120, alpha]);
        }
    }
    acrux_graphics::encode_png_with(w, h, acrux_graphics::PixelLayout::Rgba, &pixels, &|d| {
        acrux_codecs::flate::compress(d, 6)
    })
}

#[test]
fn every_fit_produces_a_sound_document_and_keeps_the_jpeg_intact() {
    let original = jpeg(64, 48);
    for fit in [Fit::Contain, Fit::Cover, Fit::Actual] {
        let images = vec![
            ImageInput::new(original.clone()),
            ImageInput::new(rgba_png(32, 32)),
        ];
        let layout = ImageLayout {
            fit,
            ..ImageLayout::default()
        };
        let doc = from_images(&images, &layout).unwrap();
        let reread = sound(&doc, &format!("images {fit:?}"));
        assert_eq!(collect_pages(&reread).unwrap().len(), 2);
        // Le JPEG doit ressortir octet pour octet.
        let mut seen = false;
        for number in reread.object_numbers() {
            let object = reread
                .get(acrux_document::ObjectRef {
                    number,
                    generation: 0,
                })
                .unwrap();
            if let Object::Stream { dict, raw } = object.as_ref() {
                if dict.get(&acrux_document::Name::new("Filter"))
                    == Some(&Object::Name(acrux_document::Name::new("DCTDecode")))
                {
                    assert_eq!(raw, &original, "le JPEG a été recompressé");
                    seen = true;
                }
            }
        }
        assert!(seen, "aucun flux DCTDecode");
    }
}

// --- Markdown ---------------------------------------------------------------

const MARKDOWN: &str = "# Titre principal\n\n\
     Un paragraphe avec du **gras**, de l'*italique* et du `code`.\n\n\
     ## Sous-titre\n\n\
     - premier point\n\
     - deuxième point\n\n\
     1. un\n\
     2. deux\n\n\
     > Une citation.\n\n\
     ---\n\n\
     | Colonne A | Colonne B |\n\
     |---|---|\n\
     | 1 | 2 |\n\n\
     ```\nlet x = 1;\n```\n\n\
     Un lien vers [le site](https://exemple.test/page).\n";

#[test]
fn a_markdown_document_is_sound_and_carries_its_link_and_bookmarks() {
    let doc = from_markdown(MARKDOWN, &TextLayout::default()).unwrap();
    let reread = sound(&doc, "markdown");
    let flat = text_of(&reread).join("\n");
    for expected in [
        "Titre principal",
        "gras",
        "Sous-titre",
        "premier point",
        "deuxième point",
        "Une citation.",
        "Colonne A",
        "let x = 1;",
        "le site",
    ] {
        assert!(flat.contains(expected), "« {expected} » absent de : {flat}");
    }
    // Aucune marque Markdown ne subsiste dans le texte rendu.
    for mark in ["**", "##", "```", "]("] {
        assert!(!flat.contains(mark), "marque « {mark} » laissée : {flat}");
    }
    let pages = collect_pages(&reread).unwrap();
    let index = PageIndex::new(&pages);
    let links = page_links(&reread, &pages[0], &index).unwrap();
    assert_eq!(links.len(), 1, "{links:?}");
    let items = outline(&reread, &index).unwrap();
    let titles: Vec<&str> = items.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["Titre principal", "Sous-titre"]);
}

// --- Combinaison ------------------------------------------------------------

#[test]
fn combining_every_kind_of_input_gives_a_sound_document() {
    let pdf = new_document(&PageSetup {
        pages: 2,
        ..PageSetup::default()
    })
    .unwrap()
    .save_full()
    .unwrap();
    let inputs = vec![
        CombineInput {
            name: "vierge.pdf".into(),
            source: CombineSource::Pdf(pdf),
        },
        CombineInput {
            name: "photo.jpg".into(),
            source: CombineSource::Image(jpeg(40, 30)),
        },
        CombineInput {
            name: "notes.md".into(),
            source: CombineSource::Markdown(MARKDOWN.into()),
        },
        CombineInput {
            name: "brut.txt".into(),
            source: CombineSource::Text(SAMPLE.into()),
        },
    ];
    let options = CombineOptions {
        bookmarks: true,
        table_of_contents: true,
        page_numbers: true,
        ..CombineOptions::default()
    };
    let doc = combine(&inputs, &options).unwrap();
    let reread = sound(&doc, "combinaison");
    let pages = collect_pages(&reread).unwrap();
    let index = PageIndex::new(&pages);
    // Un signet par fichier, dans l'ordre.
    let items = outline(&reread, &index).unwrap();
    let titles: Vec<&str> = items.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["vierge.pdf", "photo.jpg", "notes.md", "brut.txt"]);
    // Le sommaire ouvre le document et pointe sur chaque fichier.
    assert_eq!(page_links(&reread, &pages[0], &index).unwrap().len(), 4);
    // La numérotation va jusqu'au bout.
    let last = pages.len();
    let footer = extract_page_text(&reread, &pages[last - 1])
        .unwrap()
        .lines
        .iter()
        .map(Line::text)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(footer.contains(&format!("{last} / {last}")), "{footer}");
}

// --- Fichiers de corpus -----------------------------------------------------

#[test]
fn the_markdown_corpus_file_is_what_the_generator_claims() {
    let doc = Document::load(corpus("cree-depuis-markdown.pdf")).unwrap();
    check(&doc, "cree-depuis-markdown.pdf");
    renders_cleanly(&doc, "cree-depuis-markdown.pdf");
    let pages = collect_pages(&doc).unwrap();
    assert_eq!(pages.len(), 1);
    let b = pages[0].media_box(&doc);
    assert!((b.width() - 420.0).abs() < 0.01 && (b.height() - 595.0).abs() < 0.01);
    // Aucune police incorporée : l'image de référence doit valoir sur toute
    // machine, sans dépendre des polices installées.
    for number in doc.object_numbers() {
        let object = doc
            .get(acrux_document::ObjectRef {
                number,
                generation: 0,
            })
            .unwrap();
        if let Some(dict) = object.as_dict() {
            for key in ["FontFile", "FontFile2", "FontFile3"] {
                assert!(
                    !dict.contains_key(&acrux_document::Name::new(key)),
                    "objet {number} incorpore une police"
                );
            }
        }
    }
    let index = PageIndex::new(&pages);
    assert_eq!(page_links(&doc, &pages[0], &index).unwrap().len(), 1);
    assert_eq!(outline(&doc, &index).unwrap().len(), 2);
}

#[test]
fn the_images_corpus_file_is_what_the_generator_claims() {
    let doc = Document::load(corpus("cree-depuis-images.pdf")).unwrap();
    check(&doc, "cree-depuis-images.pdf");
    renders_cleanly(&doc, "cree-depuis-images.pdf");
    let pages = collect_pages(&doc).unwrap();
    assert_eq!(pages.len(), 2, "quatre images puis une");
    let b = pages[0].media_box(&doc);
    assert!((b.width() - 400.0).abs() < 0.01 && (b.height() - 300.0).abs() < 0.01);
    let mut jpegs = 0;
    let mut masks = 0;
    let mut flate_images = 0;
    for number in doc.object_numbers() {
        let object = doc
            .get(acrux_document::ObjectRef {
                number,
                generation: 0,
            })
            .unwrap();
        let Some(dict) = object.as_dict() else {
            continue;
        };
        if dict.get(&acrux_document::Name::new("Subtype"))
            != Some(&Object::Name(acrux_document::Name::new("Image")))
        {
            continue;
        }
        match dict.get(&acrux_document::Name::new("Filter")) {
            Some(Object::Name(n)) if n.0 == b"DCTDecode" => jpegs += 1,
            Some(Object::Name(n)) if n.0 == b"FlateDecode" => flate_images += 1,
            _ => {}
        }
        if dict.contains_key(&acrux_document::Name::new("SMask")) {
            masks += 1;
        }
    }
    assert_eq!(jpegs, 2, "deux JPEG incorporés tels quels");
    assert!(flate_images >= 3, "les PNG sont recompressés en Flate");
    assert_eq!(masks, 1, "une seule image transparente");
}

// --- Images d'autres formats ------------------------------------------------

/// Un fichier du corpus d'images, écrit par l'encodeur de Windows.
fn image(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join("images")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{} : {e}", path.display()))
}

/// Un scan TIFF de deux pages doit donner un PDF de deux pages : c'est tout
/// l'intérêt du format pour un document numérisé.
#[test]
fn un_tiff_multipage_donne_autant_de_pages() {
    let doc = from_images(
        &[ImageInput {
            data: image("deux-pages.tif"),
            name: "deux-pages.tif".into(),
        }],
        &ImageLayout::default(),
    )
    .unwrap();
    assert_eq!(collect_pages(&doc).unwrap().len(), 2);
    let doc = roundtrip(&doc);
    check(&doc, "tiff multipage");
    renders_cleanly(&doc, "tiff multipage");
}

/// Les autres formats font un document sain, comme le PNG et le JPEG.
#[test]
fn bmp_gif_et_tiff_donnent_un_document_sain() {
    for name in [
        "motif-24.bmp",
        "motif.gif",
        "motif-lzw.tif",
        "damier-g4.tif",
    ] {
        let doc = from_images(
            &[ImageInput {
                data: image(name),
                name: name.into(),
            }],
            &ImageLayout::default(),
        )
        .unwrap_or_else(|e| panic!("{name} : {e}"));
        assert_eq!(collect_pages(&doc).unwrap().len(), 1, "{name}");
        let doc = roundtrip(&doc);
        check(&doc, name);
        renders_cleanly(&doc, name);
    }
}
