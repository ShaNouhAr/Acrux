//! Création de documents (inventaire Acrobat §1 : « Créer un PDF » et
//! « Combiner des fichiers »).
//!
//! Quatre sources et un assembleur :
//!
//! | Fonction | Ce qu'elle produit |
//! |----------|--------------------|
//! | [`new_document`] | pages blanches d'un format nommé ou libre |
//! | [`from_images`] | une image par page, ou une grille d'images |
//! | [`from_text`] | du texte brut mis en page, paginé, aligné |
//! | [`from_markdown`] | du Markdown : titres, styles, listes, tableaux, liens |
//! | [`from_3d`] | un modèle 3D (U3D) posé sur une page, avec sa vue |
//! | [`combine`] | plusieurs fichiers réunis — PDF, images, textes —, avec signets, sommaire et formulaires |
//!
//! # Ce que les documents produits garantissent
//!
//! - ils se relisent par `Document::from_bytes` et passent `acr check` ;
//! - ils se rendent sans avertissement du moteur ;
//! - leur texte se réextrait **identique** au texte d'entrée (aux fins de
//!   ligne près, et à la césure près quand elle est activée) ;
//! - un JPEG fourni est incorporé octet pour octet, sans recompression.
//!
//! # Polices
//!
//! Tant que le texte tient en WinAnsiEncoding, une des quatorze polices
//! standard est employée et rien n'est incorporé : les fichiers restent
//! minuscules. Dès qu'un caractère en sort — grec, cyrillique, CJK, symboles
//! — une police système est incorporée en sous-ensemble `/Type0` par
//! [`crate::fontembed`], avec son `/ToUnicode` : le document accepte alors
//! n'importe quelle langue sans cesser d'être extractible.
//!
//! ```no_run
//! use acrux_features::create::{from_text, TextLayout};
//!
//! # fn main() -> acrux_core::Result<()> {
//! let doc = from_text("Bonjour le monde.", &TextLayout::default())?;
//! std::fs::write("bonjour.pdf", doc.save_full()?)?;
//! # Ok(())
//! # }
//! ```

mod builder;
mod combine;
mod images;
mod markdown;
mod paper;
mod text;
mod three_d;
mod wrap;

use acrux_core::Result;
use acrux_document::Document;

pub use crate::stamp::image::{image_format, is_supported as is_image};
pub use combine::{combine, looks_like_pdf, CombineInput, CombineOptions, CombineSource};
pub use images::{from_images, Fit, ImageInput, ImageLayout};
pub use markdown::from_markdown;
pub use paper::{Margins, Orientation, PageSetup, PageSize};
pub use text::{from_text, TextAlign, TextLayout};
pub use three_d::from_3d;

/// Crée un document vide : `setup.pages` pages blanches au format demandé.
///
/// Les pages n'ont aucun contenu ; le fichier produit fait moins d'un
/// kilo-octet pour une page.
///
/// # Errors
/// Nombre de pages nul, ou document impossible à refermer.
pub fn new_document(setup: &PageSetup) -> Result<Document> {
    let mut builder = builder::Builder::new()?;
    let (width, height) = setup.page_size();
    for _ in 0..setup.pages.max(1) {
        let page = builder.add_page(width, height);
        // Un flux vide reste un flux : `acr check` et les afficheurs
        // préfèrent une page qui a son `/Contents`.
        builder.content(page).raw("");
    }
    builder.finish()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use acrux_document::collect_pages;

    #[test]
    fn a_blank_document_has_the_requested_pages_and_format() {
        let setup = PageSetup {
            size: PageSize::A5,
            pages: 3,
            ..PageSetup::default()
        };
        let doc = new_document(&setup).unwrap();
        let reread = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        let pages = collect_pages(&reread).unwrap();
        assert_eq!(pages.len(), 3);
        let b = pages[0].media_box(&reread);
        assert_eq!((b.width(), b.height()), (420.0, 595.0));
    }

    #[test]
    fn a_landscape_letter_page_is_wider_than_tall() {
        let setup = PageSetup {
            size: PageSize::Letter,
            orientation: Orientation::Landscape,
            ..PageSetup::default()
        };
        let doc = new_document(&setup).unwrap();
        let reread = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        let b = collect_pages(&reread).unwrap()[0].media_box(&reread);
        assert_eq!((b.width(), b.height()), (792.0, 612.0));
    }

    #[test]
    fn zero_pages_still_gives_one() {
        let setup = PageSetup {
            pages: 0,
            ..PageSetup::default()
        };
        let doc = new_document(&setup).unwrap();
        assert_eq!(collect_pages(&doc).unwrap().len(), 1);
    }
    /// Source Markdown du fichier de corpus. Tout y tient en WinAnsiEncoding :
    /// le document n'incorpore donc **aucune** police système, et son image de
    /// référence est la même sur toutes les machines.
    const CORPUS_MARKDOWN: &str = "\
# Creer un PDF depuis du Markdown

Acrux analyse le Markdown **lui-meme** : aucune bibliotheque, aucun code emprunte.
Le texte peut etre *italique*, **gras**, ou du `code en chasse fixe`.

## Ce qui est reconnu

- les titres de niveau 1 a 6
- les listes a puces, avec un niveau imbrique
  - comme celle-ci
- les citations et les regles horizontales

1. premier point numerote
2. deuxieme point numerote

> Une citation se detache par un filet vertical et par l'italique.

---

| Element | Syntaxe | Largeur |
|---|---|---:|
| titre | `# Titre` | corps decroissant |
| gras | `**gras**` | police grasse |

```rust
let doc = from_markdown(source, &TextLayout::default())?;
```

Tout est decrit sur [le site du projet](https://exemple.test/acrux).
";

    /// Écrit les deux fichiers de corpus de ce module.
    ///
    /// `cargo test -p acrux-features --lib -- --ignored generate_create_corpus`
    ///
    /// Les deux fichiers doivent rester **reproductibles à l'octet près** :
    /// aucune date, aucune police système incorporée (tout le texte tient en
    /// WinAnsiEncoding), et des images engendrées par nos propres encodeurs.
    #[test]
    #[ignore = "génère les fichiers de corpus"]
    fn generate_create_corpus() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("corpus")
            .join("synthese");

        // 1. Markdown : une page A5, toutes les syntaxes reconnues.
        let layout = TextLayout {
            setup: PageSetup {
                size: PageSize::A5,
                margins: Margins {
                    top: 24.0,
                    bottom: 24.0,
                    left: 28.0,
                    right: 28.0,
                },
                ..PageSetup::default()
            },
            size: 8.0,
            ..TextLayout::default()
        };
        let doc = from_markdown(CORPUS_MARKDOWN, &layout).unwrap();
        std::fs::write(
            dir.join("cree-depuis-markdown.pdf"),
            doc.save_full().unwrap(),
        )
        .unwrap();

        // 2. Images : une grille 2 × 2 puis une page seule, JPEG et PNG mêlés.
        let images = vec![
            ImageInput {
                data: corpus_jpeg(96, 64),
                name: "degrade.jpg".into(),
            },
            ImageInput {
                data: corpus_rgb_png(48, 48),
                name: "damier.png".into(),
            },
            ImageInput {
                data: corpus_rgba_png(72, 72),
                name: "disque-alpha.png".into(),
            },
            ImageInput {
                data: corpus_gray_png(64, 32),
                name: "gris.png".into(),
            },
            ImageInput {
                data: corpus_jpeg(160, 40),
                name: "bandeau.jpg".into(),
            },
        ];
        let montage = ImageLayout {
            setup: PageSetup {
                size: PageSize::Custom {
                    width: 400.0,
                    height: 300.0,
                },
                orientation: Orientation::Landscape,
                margins: Margins {
                    top: 16.0,
                    bottom: 16.0,
                    left: 16.0,
                    right: 16.0,
                },
                ..PageSetup::default()
            },
            columns: 2,
            rows: 2,
            gap: 10.0,
            background: Some([0.97, 0.97, 0.99]),
            ..ImageLayout::default()
        };
        let doc = from_images(&images, &montage).unwrap();
        std::fs::write(dir.join("cree-depuis-images.pdf"), doc.save_full().unwrap()).unwrap();
    }

    /// Dégradé JPEG produit par notre encodeur (qualité 85, sous-échantillonné).
    #[allow(clippy::cast_possible_truncation)]
    fn corpus_jpeg(w: u32, h: u32) -> Vec<u8> {
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                rgb.extend_from_slice(&[
                    (x * 255 / w.max(1)) as u8,
                    (y * 255 / h.max(1)) as u8,
                    140,
                ]);
            }
        }
        acrux_codecs::dct::encode::encode(&rgb, w, h, 85, true)
    }

    /// Damier RVB opaque.
    fn corpus_rgb_png(w: u32, h: u32) -> Vec<u8> {
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                let on = ((x / 8) + (y / 8)) % 2 == 0;
                rgb.extend_from_slice(if on { &[30, 90, 180] } else { &[240, 220, 80] });
            }
        }
        acrux_graphics::encode_png_rgb(w, h, &rgb)
    }

    /// Disque rouge sur fond transparent : c'est lui qui exerce le `/SMask`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn corpus_rgba_png(w: u32, h: u32) -> Vec<u8> {
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        let (cx, cy, r) = (f64::from(w) / 2.0, f64::from(h) / 2.0, f64::from(w) * 0.45);
        for y in 0..h {
            for x in 0..w {
                let d = ((f64::from(x) - cx).powi(2) + (f64::from(y) - cy).powi(2)).sqrt();
                let alpha = u8::from(d <= r) * 255;
                pixels.extend_from_slice(&[200, 40, 60, alpha]);
            }
        }
        acrux_graphics::encode_png_with(w, h, acrux_graphics::PixelLayout::Rgba, &pixels, &|d| {
            acrux_codecs::flate::compress(d, 6)
        })
    }

    /// Rampe de gris, en PNG RVB.
    #[allow(clippy::cast_possible_truncation)]
    fn corpus_gray_png(w: u32, h: u32) -> Vec<u8> {
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for _ in 0..h {
            for x in 0..w {
                let v = (x * 255 / w.max(1)) as u8;
                rgb.extend_from_slice(&[v, v, v]);
            }
        }
        acrux_graphics::encode_png_rgb(w, h, &rgb)
    }
}
