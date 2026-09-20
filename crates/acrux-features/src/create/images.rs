//! Création d'un document à partir d'images (inventaire Acrobat §1,
//! « Créer un PDF à partir d'un fichier »).
//!
//! Le décodage est celui de [`crate::stamp::image`], partagé avec les
//! filigranes : un **JPEG est incorporé octet pour octet** sous
//! `/Filter /DCTDecode`, sans recompression donc sans perte ; un **PNG est
//! décodé puis recompressé en `FlateDecode`**, son canal alpha devenant un
//! `/SMask`.
//!
//! Chaque page reçoit une grille de `columns × rows` cases (1 × 1 par défaut,
//! soit une image par page) et chaque image s'ajuste dans sa case selon
//! [`Fit`].

use acrux_core::{Error, Rect, Result};
use acrux_document::{Document, ObjectRef};

use super::builder::Builder;
use super::paper::PageSetup;
use crate::stamp::image::decode_all;
use crate::stamp::Rgb;

/// Une image à placer, avec le nom qui la désigne dans les messages et les
/// signets de [`super::combine`].
#[derive(Debug, Clone)]
pub struct ImageInput {
    /// Octets du fichier : PNG, JPEG, BMP, GIF ou TIFF.
    pub data: Vec<u8>,
    /// Nom affiché (celui du fichier, en général).
    pub name: String,
}

impl ImageInput {
    /// Image anonyme depuis ses octets.
    #[must_use]
    pub fn new(data: Vec<u8>) -> Self {
        ImageInput {
            data,
            name: String::new(),
        }
    }

    /// Lit une image sur le disque ; son nom devient celui du fichier.
    ///
    /// # Errors
    /// Fichier illisible.
    pub fn from_path(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let path = path.as_ref();
        let data =
            std::fs::read(path).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        Ok(ImageInput { data, name })
    }
}

/// Ajustement d'une image dans sa case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fit {
    /// Contenir : l'image entière tient dans la case, proportions gardées,
    /// centrée ; c'est le comportement attendu d'un scan.
    #[default]
    Contain,
    /// Remplir : l'image couvre toute la case, proportions gardées, et ce qui
    /// dépasse est rogné.
    Cover,
    /// Taille réelle : un pixel vaut `72 / dpi` points. Une image trop grande
    /// pour sa case est ramenée à « contenir » plutôt que de déborder de la
    /// page.
    Actual,
}

impl Fit {
    /// Ajustement d'après son nom (`contain`, `contenir`, `cover`, `remplir`,
    /// `actual`, `reelle`).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let key: String = name
            .to_ascii_lowercase()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        match key.as_str() {
            "contain" | "contenir" => Some(Fit::Contain),
            "cover" | "remplir" => Some(Fit::Cover),
            "actual" | "reelle" | "taillereelle" => Some(Fit::Actual),
            _ => None,
        }
    }
}

/// Réglages du montage des images.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageLayout {
    /// Format, orientation et marges. Le champ `pages` est ignoré : c'est le
    /// nombre d'images qui décide.
    pub setup: PageSetup,
    /// Colonnes de la grille (1 = une image par page).
    pub columns: usize,
    /// Lignes de la grille.
    pub rows: usize,
    /// Ajustement dans la case.
    pub fit: Fit,
    /// Résolution supposée des images, pour [`Fit::Actual`].
    pub dpi: f64,
    /// Blanc entre deux cases, en points.
    pub gap: f64,
    /// Couleur de fond de la page ; `None` laisse la page transparente.
    pub background: Option<Rgb>,
}

impl Default for ImageLayout {
    /// Une image par page A4, ajustée pour tenir entière, 96 dpi supposés.
    fn default() -> Self {
        ImageLayout {
            setup: PageSetup::default(),
            columns: 1,
            rows: 1,
            fit: Fit::Contain,
            dpi: 96.0,
            gap: 12.0,
            background: None,
        }
    }
}

impl ImageLayout {
    /// Nombre de cases par page (au moins une).
    #[must_use]
    pub fn cells_per_page(&self) -> usize {
        self.columns.max(1) * self.rows.max(1)
    }

    /// Case d'index `index` (0 = en haut à gauche, puis de gauche à droite
    /// et de haut en bas) dans la boîte utile.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // quelques colonnes, jamais 2^53
    pub fn cell(&self, index: usize) -> Rect {
        let (cols, rows) = (self.columns.max(1), self.rows.max(1));
        let area = self.setup.content_box();
        let cell_w = (area.width() - self.gap * (cols - 1) as f64) / cols as f64;
        let cell_h = (area.height() - self.gap * (rows - 1) as f64) / rows as f64;
        let i = index % (cols * rows);
        let (col, row) = (i % cols, i / cols);
        let x0 = area.x0 + (cell_w + self.gap) * col as f64;
        let y1 = area.y1 - (cell_h + self.gap) * row as f64;
        Rect::new(x0, y1 - cell_h.max(0.0), x0 + cell_w.max(0.0), y1)
    }
}

/// Place une image de `pixels` pixels dans `cell` et renvoie son rectangle,
/// plus le rognage éventuel.
#[must_use]
#[allow(clippy::cast_precision_loss)] // dimensions d'image : bien en deçà de 2^53
pub(crate) fn place(cell: Rect, pixels: (u32, u32), fit: Fit, dpi: f64) -> (Rect, Option<Rect>) {
    let (iw, ih) = (f64::from(pixels.0).max(1.0), f64::from(pixels.1).max(1.0));
    let (cw, ch) = (cell.width().max(0.0), cell.height().max(0.0));
    if cw <= 0.0 || ch <= 0.0 {
        return (Rect::new(cell.x0, cell.y0, cell.x0, cell.y0), None);
    }
    let contain = (cw / iw).min(ch / ih);
    let scale = match fit {
        Fit::Contain => contain,
        Fit::Cover => (cw / iw).max(ch / ih),
        // Un pixel vaut 72/dpi points, sans jamais dépasser la case.
        Fit::Actual => (72.0 / dpi.max(1.0)).min(contain),
    };
    let (w, h) = (iw * scale, ih * scale);
    let x = cell.x0 + (cw - w) / 2.0;
    let y = cell.y0 + (ch - h) / 2.0;
    let drawn = Rect::new(x, y, x + w, y + h);
    // Seul « remplir » déborde de sa case : c'est là qu'il faut rogner.
    let clip = (fit == Fit::Cover && (w > cw + 0.001 || h > ch + 0.001)).then_some(cell);
    (drawn, clip)
}

/// Compose un document à partir d'images (PNG, JPEG, BMP, GIF, TIFF).
///
/// Un **TIFF multipage** — ce que produit un scanner — donne autant d'images
/// qu'il porte de pages, dans l'ordre : un fichier suffit donc à faire un
/// document de plusieurs feuilles.
///
/// # Errors
/// Liste vide, image d'un format inconnu ou d'en-tête illisible.
pub fn from_images(images: &[ImageInput], layout: &ImageLayout) -> Result<Document> {
    if images.is_empty() {
        return Err(Error::Corrupt("aucune image à mettre en page".into()));
    }
    let mut builder = Builder::new()?;
    let per_page = layout.cells_per_page();
    let (width, height) = layout.setup.page_size();
    let mut page = usize::MAX;
    let mut decoded_all = Vec::with_capacity(images.len());
    for input in images {
        decoded_all.extend(decode_all(&input.data).map_err(|e| annotate(&input.name, &e))?);
    }
    for (index, decoded) in decoded_all.into_iter().enumerate() {
        let pixels = (decoded.width, decoded.height);
        let object: ObjectRef = decoded.write(&builder.doc);
        if index % per_page == 0 {
            page = builder.add_page(width, height);
            if let Some(color) = layout.background {
                builder
                    .content(page)
                    .fill_rect(Rect::new(0.0, 0.0, width, height), color);
            }
        }
        let (drawn, clip) = place(
            layout.cell(index % per_page),
            pixels,
            layout.fit,
            layout.dpi,
        );
        let name = format!("AKI{}", index + 1);
        builder.content(page).image(&name, object, drawn, clip);
    }
    builder.finish()
}

/// Rattache le nom du fichier au message d'erreur d'une image.
fn annotate(name: &str, error: &Error) -> Error {
    if name.is_empty() {
        return error.clone();
    }
    Error::Corrupt(format!("{name} : {error}"))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation
)]
mod tests {
    use super::*;
    use crate::create::paper::{Margins, PageSize};
    use acrux_document::{collect_pages, Name, Object};

    /// PNG RVBA minimal : un damier `w × h` dont le coin supérieur gauche est
    /// transparent, écrit par notre propre encodeur.
    fn rgba_png(w: u32, h: u32) -> Vec<u8> {
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let on = (x + y) % 2 == 0;
                let alpha = if x == 0 && y == 0 { 0 } else { 255 };
                pixels.extend_from_slice(&[
                    if on { 220 } else { 40 },
                    if on { 60 } else { 180 },
                    90,
                    alpha,
                ]);
            }
        }
        acrux_graphics::encode_png_with(w, h, acrux_graphics::PixelLayout::Rgba, &pixels, &|d| {
            acrux_codecs::flate::compress(d, 6)
        })
    }

    /// PNG opaque (RVB).
    fn rgb_png(w: u32, h: u32) -> Vec<u8> {
        let mut pixels = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                pixels.extend_from_slice(&[(x * 7) as u8, (y * 11) as u8, 128]);
            }
        }
        acrux_graphics::encode_png_rgb(w, h, &pixels)
    }

    /// JPEG produit par notre encodeur.
    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                rgb.extend_from_slice(&[(x * 3) as u8, 90, (y * 5) as u8]);
            }
        }
        acrux_codecs::dct::encode::encode(&rgb, w, h, 85, true)
    }

    #[test]
    fn one_image_per_page_by_default() {
        let images = vec![
            ImageInput::new(rgb_png(8, 6)),
            ImageInput::new(jpeg(16, 16)),
            ImageInput::new(rgba_png(4, 4)),
        ];
        let doc = from_images(&images, &ImageLayout::default()).unwrap();
        let reread = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        assert_eq!(collect_pages(&reread).unwrap().len(), 3);
    }

    #[test]
    fn a_grid_packs_several_images_on_one_page() {
        let images: Vec<ImageInput> = (0..7).map(|_| ImageInput::new(rgb_png(8, 8))).collect();
        let layout = ImageLayout {
            columns: 2,
            rows: 3,
            ..ImageLayout::default()
        };
        assert_eq!(layout.cells_per_page(), 6);
        let doc = from_images(&images, &layout).unwrap();
        assert_eq!(collect_pages(&doc).unwrap().len(), 2, "6 puis 1");
        // Les six cases d'une page ne se chevauchent pas et restent dans la
        // boîte utile.
        let area = layout.setup.content_box();
        let cells: Vec<Rect> = (0..6).map(|i| layout.cell(i)).collect();
        for (i, a) in cells.iter().enumerate() {
            assert!(a.x0 >= area.x0 - 0.01 && a.x1 <= area.x1 + 0.01, "{a:?}");
            assert!(a.y0 >= area.y0 - 0.01 && a.y1 <= area.y1 + 0.01, "{a:?}");
            for b in &cells[i + 1..] {
                let inter = a.intersect(b);
                assert!(
                    inter.width() <= 0.01 || inter.height() <= 0.01,
                    "{a:?} {b:?}"
                );
            }
        }
    }

    #[test]
    fn a_jpeg_is_embedded_byte_for_byte() {
        let bytes = jpeg(24, 12);
        let doc = from_images(&[ImageInput::new(bytes.clone())], &ImageLayout::default()).unwrap();
        let reread = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        let mut found = false;
        for n in reread.object_numbers() {
            let Ok(obj) = reread.get(acrux_document::ObjectRef {
                number: n,
                generation: 0,
            }) else {
                continue;
            };
            let Object::Stream { dict, raw } = obj.as_ref() else {
                continue;
            };
            if dict.get(&Name::new("Filter")) == Some(&Object::Name(Name::new("DCTDecode"))) {
                assert_eq!(raw, &bytes, "le JPEG doit sortir tel quel");
                assert_eq!(dict.get(&Name::new("Width")), Some(&Object::Integer(24)));
                found = true;
            }
        }
        assert!(found, "aucun flux DCTDecode dans le document");
    }

    #[test]
    fn a_png_with_alpha_gets_an_smask() {
        let doc = from_images(&[ImageInput::new(rgba_png(6, 6))], &ImageLayout::default()).unwrap();
        let reread = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        let mut masks = 0;
        for n in reread.object_numbers() {
            let Ok(obj) = reread.get(acrux_document::ObjectRef {
                number: n,
                generation: 0,
            }) else {
                continue;
            };
            if let Some(d) = obj.as_dict() {
                if d.contains_key(&Name::new("SMask")) {
                    masks += 1;
                }
            }
        }
        assert_eq!(masks, 1, "une image transparente, un masque");
    }

    #[test]
    fn an_image_larger_than_the_page_is_scaled_down() {
        let cell = Rect::new(0.0, 0.0, 100.0, 50.0);
        let (drawn, clip) = place(cell, (4000, 3000), Fit::Contain, 96.0);
        assert!(
            drawn.width() <= 100.001 && drawn.height() <= 50.001,
            "{drawn:?}"
        );
        assert!(clip.is_none());
        // Proportions gardées : 4/3.
        assert!((drawn.width() / drawn.height() - 4.0 / 3.0).abs() < 0.001);
        // Taille réelle : une image énorme est ramenée à « contenir ».
        let (actual, _) = place(cell, (4000, 3000), Fit::Actual, 96.0);
        assert!(
            actual.width() <= 100.001 && actual.height() <= 50.001,
            "{actual:?}"
        );
        // Petite image en taille réelle : 96 pixels font bien 72 points.
        let (small, _) = place(cell, (96, 48), Fit::Actual, 96.0);
        assert!((small.width() - 72.0).abs() < 0.001, "{small:?}");
    }

    #[test]
    fn cover_fills_the_cell_and_clips() {
        let cell = Rect::new(10.0, 10.0, 110.0, 60.0);
        let (drawn, clip) = place(cell, (100, 100), Fit::Cover, 96.0);
        assert!(
            drawn.width() >= 99.999 && drawn.height() >= 49.999,
            "{drawn:?}"
        );
        assert_eq!(clip, Some(cell), "ce qui dépasse doit être rogné");
        // Centrage : le débordement est partagé en haut et en bas.
        assert!((drawn.y0 + drawn.y1 - (cell.y0 + cell.y1)).abs() < 0.001);
    }

    #[test]
    fn an_empty_list_is_an_error_and_a_bad_format_names_the_file() {
        assert!(from_images(&[], &ImageLayout::default()).is_err());
        let bad = ImageInput {
            data: b"GIF89a".to_vec(),
            name: "photo.gif".into(),
        };
        let e = from_images(&[bad], &ImageLayout::default()).unwrap_err();
        assert!(e.to_string().contains("photo.gif"), "{e}");
    }

    #[test]
    fn the_background_paints_the_whole_page() {
        let layout = ImageLayout {
            setup: PageSetup {
                size: PageSize::Custom {
                    width: 100.0,
                    height: 100.0,
                },
                margins: Margins {
                    top: 10.0,
                    bottom: 10.0,
                    left: 10.0,
                    right: 10.0,
                },
                ..PageSetup::default()
            },
            background: Some([0.2, 0.4, 0.6]),
            ..ImageLayout::default()
        };
        let doc = from_images(&[ImageInput::new(rgb_png(4, 4))], &layout).unwrap();
        let reread = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        let pages = collect_pages(&reread).unwrap();
        let content = acrux_render::page::page_content(&reread, &pages[0]);
        let ops = String::from_utf8_lossy(&content);
        assert!(ops.contains("0 0 100 100 re f"), "{ops}");
    }
}
