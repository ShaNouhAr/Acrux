//! Corpus de synthèse des filtres d'image CCITTFaxDecode, JBIG2Decode et
//! JPXDecode : assemblage des PDF et preuve d'exactitude du rendu.
//!
//! Chaîne de production :
//! 1. `cargo test -p acrux-codecs --release -- --ignored generate_corpus` :
//!    les encodeurs de test de chaque codec écrivent dans `target/corpus-gen/`
//!    les octets encodés et l'image source (PNG) de chaque échantillon ;
//! 2. `cargo test -p acrux-render --test synthese_codecs -- --ignored assemble` :
//!    ce fichier assemble ces octets en PDF (une page par image, page de la
//!    taille de l'image en points, image dessinée plein cadre) dans
//!    `tests/corpus/synthese/` et copie les images source comme références
//!    `tests/reference/synthese/<nom>-pNNN.png` ;
//! 3. `codec_pages_render_pixel_exact` (test ordinaire) rend chaque page à
//!    72 dpi et exige l'identité pixel à pixel avec la référence : tous les
//!    codages sont sans perte, et la page JPX alpha est composée sur blanc
//!    avec ⌊(C·α + 255·(255 − α) + 127) / 255⌋.
//!
//! Le test de tolérance `render_corpus` couvre aussi ces fichiers ; celui-ci
//! est strict parce que rien, ici, ne dépend de l'anti-aliasing.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use acrux_document::{collect_pages, Document};
use acrux_graphics::{encode_png_rgb, Color};
use acrux_render::{render_page, RenderOptions};

/// Une page du corpus : fichier de données encodées, image source (PNG),
/// filtre et entrées supplémentaires du dictionnaire d'image (`{globals}`
/// est remplacé par le numéro d'objet du flux `/JBIG2Globals`).
struct PageSpec {
    data: &'static str,
    image: &'static str,
    filter: &'static str,
    dict: &'static str,
    globals: Option<&'static str>,
}

const BILEVEL: &str = "/ColorSpace /DeviceGray /BitsPerComponent 1";

/// Fichiers du corpus et leurs pages.
const FILES: &[(&str, &[PageSpec])] = &[
    (
        "ccitt-g4-g3-1d-2d-blackis1.pdf",
        &[
            PageSpec {
                data: "ccitt-g4.bin",
                image: "ccitt-g4.png",
                filter: "CCITTFaxDecode",
                dict: "/ColorSpace /DeviceGray /BitsPerComponent 1 /DecodeParms << /K -1 /Columns 200 /Rows 120 >>",
                globals: None,
            },
            PageSpec {
                data: "ccitt-g3-1d.bin",
                image: "ccitt-g3-1d.png",
                filter: "CCITTFaxDecode",
                dict: "/ColorSpace /DeviceGray /BitsPerComponent 1 /DecodeParms << /K 0 /Columns 200 /Rows 120 >>",
                globals: None,
            },
            PageSpec {
                data: "ccitt-g3-2d-align.bin",
                image: "ccitt-g3-2d-align.png",
                filter: "CCITTFaxDecode",
                dict: "/ColorSpace /DeviceGray /BitsPerComponent 1 /DecodeParms << /K 4 /Columns 200 /Rows 120 /EncodedByteAlign true /EndOfLine true >>",
                globals: None,
            },
            PageSpec {
                data: "ccitt-g4-blackis1.bin",
                image: "ccitt-g4-blackis1.png",
                filter: "CCITTFaxDecode",
                dict: "/ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [1 0] /DecodeParms << /K -1 /Columns 200 /Rows 120 /BlackIs1 true >>",
                globals: None,
            },
        ],
    ),
    (
        "jbig2-generique-texte-globals.pdf",
        &[
            PageSpec {
                data: "jbig2-generic.bin",
                image: "jbig2-generic.png",
                filter: "JBIG2Decode",
                dict: BILEVEL,
                globals: None,
            },
            PageSpec {
                data: "jbig2-text.bin",
                image: "jbig2-text.png",
                filter: "JBIG2Decode",
                dict: BILEVEL,
                globals: None,
            },
            PageSpec {
                data: "jbig2-globals.bin",
                image: "jbig2-globals.png",
                filter: "JBIG2Decode",
                dict: "/ColorSpace /DeviceGray /BitsPerComponent 1 /DecodeParms << /JBIG2Globals {globals} 0 R >>",
                globals: Some("jbig2-globals.globals.bin"),
            },
        ],
    ),
    (
        "jpx-gris-rvb-alpha-j2k.pdf",
        &[
            // Sans /ColorSpace ni /BitsPerComponent : le conteneur JP2 (colr
            // 17, gris) fait foi.
            PageSpec {
                data: "jpx-gray.jp2",
                image: "jpx-gray.png",
                filter: "JPXDecode",
                dict: "",
                globals: None,
            },
            // Idem, colr 16 (sRGB).
            PageSpec {
                data: "jpx-rgb.jp2",
                image: "jpx-rgb.png",
                filter: "JPXDecode",
                dict: "",
                globals: None,
            },
            // Canal alpha du JP2 (boîte cdef) utilisé comme masque doux.
            PageSpec {
                data: "jpx-rgba.jp2",
                image: "jpx-rgba.png",
                filter: "JPXDecode",
                dict: "/ColorSpace /DeviceRGB /SMaskInData 1",
                globals: None,
            },
            // Flux de code brut : pas de conteneur, /ColorSpace obligatoire.
            PageSpec {
                data: "jpx-rgb.j2k",
                image: "jpx-j2k.png",
                filter: "JPXDecode",
                dict: "/ColorSpace /DeviceRGB /BitsPerComponent 8",
                globals: None,
            },
        ],
    ),
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn corpus_dir() -> PathBuf {
    repo_root().join("tests").join("corpus").join("synthese")
}

fn reference_dir() -> PathBuf {
    repo_root().join("tests").join("reference").join("synthese")
}

fn reference_name(file: &str, page: usize) -> String {
    format!("{}-p{:03}.png", file.trim_end_matches(".pdf"), page + 1)
}

/// Image source d'une page : largeur, hauteur, RVB 8 bits.
type Source = (u32, u32, Vec<u8>);

/// Décodeur PNG minimal (gris, RGB ou RGBA 8 bits, non entrelacé) → RGB.
fn decode_png(data: &[u8]) -> Option<Source> {
    if !data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return None;
    }
    let mut pos = 8;
    let (mut w, mut h, mut color_type) = (0u32, 0u32, 0u8);
    let mut idat = Vec::new();
    while pos + 8 <= data.len() {
        let len =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let kind = &data[pos + 4..pos + 8];
        let body = data.get(pos + 8..pos + 8 + len)?;
        match kind {
            b"IHDR" => {
                w = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                h = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
                if body[8] != 8 || body[12] != 0 {
                    return None;
                }
                color_type = body[9];
            }
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        pos += 12 + len;
    }
    let channels = match color_type {
        2 => 3,
        6 => 4,
        0 => 1,
        _ => return None,
    };
    let raw = acrux_codecs::flate::decode(&idat).ok()?;
    let unfiltered = acrux_codecs::predictor::apply(&raw, 15, channels, 8, w).ok()?;
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for px in unfiltered.chunks_exact(channels as usize) {
        match channels {
            1 => rgb.extend_from_slice(&[px[0], px[0], px[0]]),
            _ => rgb.extend_from_slice(&px[..3]),
        }
    }
    Some((w, h, rgb))
}

/// Écrivain de PDF minimal : objets numérotés dans l'ordre d'ajout, table
/// xref classique. Les objets 1 (catalogue) et 2 (pages) sont réservés.
struct PdfBuilder {
    objects: Vec<Vec<u8>>,
}

impl PdfBuilder {
    fn new() -> Self {
        PdfBuilder {
            objects: vec![Vec::new(), Vec::new()],
        }
    }

    /// Ajoute un objet et rend son numéro.
    fn add(&mut self, body: Vec<u8>) -> usize {
        self.objects.push(body);
        self.objects.len()
    }

    fn add_stream(&mut self, dict: &str, data: &[u8]) -> usize {
        let mut body = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream");
        self.add(body)
    }

    fn set(&mut self, number: usize, body: Vec<u8>) {
        self.objects[number - 1] = body;
    }

    fn finish(&self) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut offsets = Vec::with_capacity(self.objects.len());
        for (i, body) in self.objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        let size = self.objects.len() + 1;
        out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }
}

/// Assemble un fichier du corpus à partir de `target/corpus-gen/` ; rend le
/// PDF et, par page, l'image source.
fn build(pages: &[PageSpec]) -> (Vec<u8>, Vec<Source>) {
    let gen = repo_root().join("target").join("corpus-gen");
    let read = |name: &str| -> Vec<u8> {
        std::fs::read(gen.join(name)).unwrap_or_else(|e| {
            panic!(
                "{} : {e} (lancer d'abord `cargo test -p acrux-codecs --release -- --ignored generate_corpus`)",
                gen.join(name).display()
            )
        })
    };
    let mut pdf = PdfBuilder::new();
    let mut kids = Vec::new();
    let mut sources = Vec::new();
    for p in pages {
        let data = read(p.data);
        let (w, h, rgb) = decode_png(&read(p.image)).expect("PNG source");
        let mut dict = p.dict.to_owned();
        if let Some(g) = p.globals {
            let n = pdf.add_stream("", &read(g));
            dict = dict.replace("{globals}", &n.to_string());
        }
        let image = pdf.add_stream(
            &format!(
                "/Type /XObject /Subtype /Image /Width {w} /Height {h} /Filter /{} {dict}",
                p.filter
            ),
            &data,
        );
        let content = pdf.add_stream("", format!("q {w} 0 0 {h} 0 0 cm /Im1 Do Q").as_bytes());
        let page = pdf.add(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Resources << /XObject << /Im1 {image} 0 R >> >> /Contents {content} 0 R >>"
            )
            .into_bytes(),
        );
        kids.push(page);
        sources.push((w, h, rgb));
    }
    pdf.set(1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    let kids: Vec<String> = kids.iter().map(|k| format!("{k} 0 R")).collect();
    pdf.set(
        2,
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            kids.len()
        )
        .into_bytes(),
    );
    (pdf.finish(), sources)
}

/// Écrit les PDF dans `tests/corpus/synthese/` et les références dans
/// `tests/reference/synthese/`.
#[test]
#[ignore = "assemblage du corpus : cargo test -p acrux-render --test synthese_codecs -- --ignored assemble"]
fn assemble_codec_corpus() {
    std::fs::create_dir_all(reference_dir()).unwrap();
    for (file, pages) in FILES {
        let (bytes, sources) = build(pages);
        let path = corpus_dir().join(file);
        std::fs::write(&path, &bytes).unwrap();
        println!(
            "écrit {} ({} octets, {} pages)",
            path.display(),
            bytes.len(),
            sources.len()
        );
        for (i, (w, h, rgb)) in sources.iter().enumerate() {
            let ref_path = reference_dir().join(reference_name(file, i));
            std::fs::write(&ref_path, encode_png_rgb(*w, *h, rgb)).unwrap();
            println!("écrit {}", ref_path.display());
        }
    }
}

/// Nombre de pixels différents et écart maximal de canal entre deux images RVB.
fn compare(a: &[u8], b: &[u8]) -> (usize, u8) {
    let mut differing = 0;
    let mut max_delta = 0u8;
    for (pa, pb) in a.chunks_exact(3).zip(b.chunks_exact(3)) {
        let d = pa
            .iter()
            .zip(pb)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0);
        if d > 0 {
            differing += 1;
        }
        max_delta = max_delta.max(d);
    }
    (differing, max_delta)
}

/// Chaque page rendue à 72 dpi (1 px = 1 pt) est identique, pixel à pixel,
/// à l'image source encodée.
#[test]
fn codec_pages_render_pixel_exact() {
    let options = RenderOptions {
        annotations: true,
        time_budget: Some(std::time::Duration::from_secs(30)),
        background: Some(Color::WHITE),
        ..RenderOptions::default()
    };
    let actual_dir = repo_root()
        .join("tests")
        .join("reference")
        .join("actual")
        .join("synthese");
    let mut failures = Vec::new();
    for (file, specs) in FILES {
        let path = corpus_dir().join(file);
        let doc = match Document::load(&path) {
            Ok(d) => d,
            Err(e) => {
                failures.push(format!("{file} : ouverture impossible ({e})"));
                continue;
            }
        };
        let pages = collect_pages(&doc).expect("pages");
        if pages.len() != specs.len() {
            failures.push(format!(
                "{file} : {} pages, {} attendues",
                pages.len(),
                specs.len()
            ));
            continue;
        }
        for (i, page) in pages.iter().enumerate() {
            let name = reference_name(file, i);
            let reference = std::fs::read(reference_dir().join(&name))
                .ok()
                .and_then(|d| decode_png(&d))
                .unwrap_or_else(|| panic!("référence {name} absente ou illisible"));
            let rendered = render_page(&doc, page, 1.0, &options);
            for w in &rendered.warnings {
                failures.push(format!("{name} : avertissement « {w} »"));
            }
            let (w, h) = (rendered.bitmap.width(), rendered.bitmap.height());
            if (w, h) != (reference.0, reference.1) {
                failures.push(format!(
                    "{name} : rendu {w}×{h}, source {}×{}",
                    reference.0, reference.1
                ));
                continue;
            }
            let rgb = rendered.bitmap.to_rgb8_over_white();
            let (differing, max_delta) = compare(&rgb, &reference.2);
            println!("{name} : {differing} pixel(s) différent(s), écart max {max_delta}");
            if differing > 0 {
                std::fs::create_dir_all(&actual_dir).unwrap();
                let actual = actual_dir.join(&name);
                std::fs::write(&actual, encode_png_rgb(w, h, &rgb)).unwrap();
                failures.push(format!(
                    "{name} : {differing} pixel(s) différent(s) (écart max {max_delta}) ; rendu écrit dans {}",
                    actual.display()
                ));
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
