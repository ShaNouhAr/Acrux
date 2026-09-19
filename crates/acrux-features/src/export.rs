//! Export et conversion : le pendant des « Exporter vers » d'Acrobat.
//!
//! Tout part du même document ouvert et du même moteur que l'affichage :
//!
//! | Sortie | Fonction | Ce qui est conservé |
//! |--------|----------|---------------------|
//! | PNG    | [`export_pages_png`] | rendu exact d'une page à la résolution demandée |
//! | JPEG   | [`export_pages_jpeg`] | idem, encodé par [`acrux_codecs::dct::encode`] |
//! | Images incorporées | [`extract_images`] | les XObjects image dessinés, JPEG à l'octet près |
//! | HTML   | [`export_html`] | texte positionné ou en flux, images, polices incorporées |
//! | DOCX   | [`export_docx`] | paragraphes, styles, titres, listes, tableaux, images |
//! | XLSX   | [`export_tables_xlsx`] | les tableaux détectés, une feuille par page |
//! | Texte / Markdown | [`export_text`], [`export_markdown`] | via `PageText::to_plain` / `to_markdown` |
//!
//! Les conteneurs bureautiques (`.docx`, `.xlsx`) sont des archives ZIP
//! construites par [`crate::zip`] ; leur XML est écrit ici à la main
//! (WordprocessingML ECMA-376 partie 1 §17, SpreadsheetML §18).

mod docx;
mod html;
mod images;
mod xlsx;

use acrux_core::{Error, Result};
use acrux_document::{collect_pages, Document, Page};

pub use docx::export_docx;
pub use html::{export_html, HtmlLayout, HtmlOptions};
pub use images::{extract_images, ExtractedImage, ImageFormat};
pub use xlsx::export_tables_xlsx;

use crate::text::{extract_page_text, mark_repeated_headers, PageText};

/// Résolution minimale et maximale acceptées pour un export d'image.
const DPI_RANGE: (f64, f64) = (1.0, 2400.0);

/// Niveau du compresseur DEFLATE des PNG produits : bon compromis entre
/// taille et temps sur une page rendue.
const PNG_LEVEL: u8 = 6;

/// Flux zlib d'un PNG, par le compresseur maison. Sans lui, l'encodeur de
/// référence d'`acrux-graphics` écrit des blocs « stored » : une page A4 à
/// 150 dpi pèserait 8 Mo au lieu de quelques centaines de kilo-octets.
pub(crate) fn deflate_png(data: &[u8]) -> Vec<u8> {
    acrux_codecs::flate::compress(data, PNG_LEVEL)
}

/// Options de rendu communes aux exports d'image : annotations dessinées,
/// fond blanc (un PDF n'a pas de fond, un PNG ou un JPEG en a un), budget de
/// temps pour ne pas rester bloqué sur un fichier hostile.
fn render_options() -> acrux_render::RenderOptions {
    acrux_render::RenderOptions {
        annotations: true,
        time_budget: Some(std::time::Duration::from_secs(60)),
        background: Some(acrux_graphics::Color::WHITE),
        ..acrux_render::RenderOptions::default()
    }
}

/// Pages demandées, vérifiées ; `indices` vide signifie « toutes les pages ».
fn selected_pages(doc: &Document, indices: &[usize]) -> Result<Vec<(usize, Page)>> {
    let all = collect_pages(doc)?;
    if indices.is_empty() {
        return Ok(all.into_iter().enumerate().collect());
    }
    indices
        .iter()
        .map(|&i| {
            all.get(i)
                .cloned()
                .map(|p| (i, p))
                .ok_or_else(|| Error::Corrupt(format!("page {} absente", i + 1)))
        })
        .collect()
}

/// Texte structuré de toutes les pages, en-têtes et pieds répétés marqués.
///
/// # Errors
/// Ressources de page illisibles.
pub fn page_texts(doc: &Document) -> Result<Vec<PageText>> {
    let pages = collect_pages(doc)?;
    let mut texts = Vec::with_capacity(pages.len());
    for page in &pages {
        texts.push(extract_page_text(doc, page)?);
    }
    mark_repeated_headers(&mut texts);
    Ok(texts)
}

/// Rend les pages `indices` (0 = première ; vide = toutes) en PNG et remet
/// chaque fichier à `sink` avec l'indice de la page.
///
/// Le rendu passe par le moteur d'affichage : une seule et même image pour
/// l'écran, l'impression et l'export (voir `ARCHITECTURE.md` §5).
///
/// # Errors
/// Page absente, arbre des pages illisible, ou erreur remontée par `sink`
/// (écriture du fichier de sortie, par exemple).
pub fn export_pages_png(
    doc: &Document,
    indices: &[usize],
    dpi: f64,
    sink: &mut dyn FnMut(usize, &[u8]) -> Result<()>,
) -> Result<()> {
    let dpi = dpi.clamp(DPI_RANGE.0, DPI_RANGE.1);
    for (i, page) in selected_pages(doc, indices)? {
        let rendered = acrux_render::render_page(doc, &page, dpi / 72.0, &render_options());
        let png = acrux_graphics::encode_png_with(
            rendered.bitmap.width(),
            rendered.bitmap.height(),
            acrux_graphics::PixelLayout::Rgba,
            &rendered.bitmap.to_rgba8_unpremultiplied(),
            &deflate_png,
        );
        sink(i, &png)?;
    }
    Ok(())
}

/// Rend les pages `indices` (0 = première ; vide = toutes) en JPEG.
///
/// `quality` va de 1 à 100 (75 par défaut chez Acrobat) ; `subsample` active
/// le sous-échantillonnage 4:2:0 des chromas. L'encodeur est
/// [`acrux_codecs::dct::encode::encode`], écrit ici même.
///
/// # Errors
/// Comme [`export_pages_png`].
pub fn export_pages_jpeg(
    doc: &Document,
    indices: &[usize],
    dpi: f64,
    quality: u8,
    subsample: bool,
    sink: &mut dyn FnMut(usize, &[u8]) -> Result<()>,
) -> Result<()> {
    let dpi = dpi.clamp(DPI_RANGE.0, DPI_RANGE.1);
    for (i, page) in selected_pages(doc, indices)? {
        let rendered = acrux_render::render_page(doc, &page, dpi / 72.0, &render_options());
        let rgb = rendered.bitmap.to_rgb8_over_white();
        let jpeg = acrux_codecs::dct::encode::encode(
            &rgb,
            rendered.bitmap.width(),
            rendered.bitmap.height(),
            quality,
            subsample,
        );
        sink(i, &jpeg)?;
    }
    Ok(())
}

/// Texte brut du document : `PageText::to_plain` page par page, séparé par
/// des sauts de page (`\u{c}`).
///
/// # Errors
/// Pages illisibles.
pub fn export_text(doc: &Document) -> Result<String> {
    let texts = page_texts(doc)?;
    let mut out = String::new();
    for (i, t) in texts.iter().enumerate() {
        if i > 0 {
            out.push('\u{c}');
        }
        out.push_str(&t.to_plain());
    }
    Ok(out)
}

/// Markdown du document : `PageText::to_markdown` page par page, séparé par
/// une règle horizontale.
///
/// # Errors
/// Pages illisibles.
pub fn export_markdown(doc: &Document) -> Result<String> {
    let texts = page_texts(doc)?;
    let mut out = String::new();
    for (i, t) in texts.iter().enumerate() {
        if i > 0 {
            out.push_str("\n---\n\n");
        }
        out.push_str(&t.to_markdown());
    }
    Ok(out)
}

/// Échappe le texte d'un nœud ou d'un attribut XML (XML 1.0 §2.4) et retire
/// les caractères de contrôle interdits, qu'aucun lecteur n'accepte.
pub(crate) fn escape_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || (0xD800..=0xDFFF).contains(&(c as u32)) => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Alphabet Base64 standard (RFC 4648 §4).
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode en Base64 (RFC 4648), sans retour à la ligne : pour les URI `data:`.
pub(crate) fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).map_or(0, |&b| u32::from(b));
        let b2 = chunk.get(2).map_or(0, |&b| u32::from(b));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(char::from(BASE64[(n >> 18) as usize & 63]));
        out.push(char::from(BASE64[(n >> 12) as usize & 63]));
        out.push(if chunk.len() > 1 {
            char::from(BASE64[(n >> 6) as usize & 63])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(BASE64[n as usize & 63])
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    /// PDF minimal d'une page avec le contenu donné.
    pub(super) fn pdf(content: &str) -> Document {
        let src = format!(
            "%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >> >> >> >> endobj\n4 0 obj << /Length {} >>\nstream\n{content}\nendstream\nendobj\n",
            content.len()
        );
        Document::from_bytes(src.into_bytes()).unwrap()
    }

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xFF, 0xFE, 0xFD]), "//79");
    }

    #[test]
    fn xml_escaping_covers_entities_and_control_characters() {
        assert_eq!(escape_xml("a<b>&\"'"), "a&lt;b&gt;&amp;&quot;&apos;");
        assert_eq!(escape_xml("avant\u{1}après"), "avant après");
        assert_eq!(escape_xml("garde\tles\nblancs"), "garde\tles\nblancs");
    }

    #[test]
    fn png_and_jpeg_pages_are_produced() {
        let doc = pdf("1 0 0 RG 0 0 1 rg 20 20 260 260 re f BT /F1 24 Tf 40 150 Td (Export) Tj ET");
        let mut png = Vec::new();
        export_pages_png(&doc, &[], 72.0, &mut |i, data| {
            png.push((i, data.to_vec()));
            Ok(())
        })
        .unwrap();
        assert_eq!(png.len(), 1);
        assert_eq!(png[0].0, 0);
        assert_eq!(&png[0].1[..8], &acrux_graphics::png::PNG_SIGNATURE);
        let mut jpeg = Vec::new();
        export_pages_jpeg(&doc, &[0], 72.0, 85, true, &mut |_, data| {
            jpeg = data.to_vec();
            Ok(())
        })
        .unwrap();
        let img = acrux_codecs::dct::decode(&jpeg).unwrap();
        assert_eq!((img.width, img.height), (300, 300));
        // Le carré bleu occupe le centre de la page.
        let center = ((150 * 300) + 150) * 3;
        assert!(
            img.data[center + 2] > 180,
            "centre bleu : {:?}",
            &img.data[center..center + 3]
        );
    }

    #[test]
    fn missing_page_is_reported() {
        let doc = pdf("BT ET");
        let err = export_pages_png(&doc, &[7], 72.0, &mut |_, _| Ok(()));
        assert!(err.is_err());
        // Une erreur du puits remonte telle quelle.
        let err = export_pages_png(&doc, &[0], 72.0, &mut |_, _| {
            Err(Error::Corrupt("disque plein".into()))
        });
        assert!(matches!(err, Err(Error::Corrupt(m)) if m == "disque plein"));
    }

    #[test]
    fn text_and_markdown_reuse_page_text() {
        let doc = pdf("BT /F1 20 Tf 30 250 Td (Titre du document) Tj 0 -60 Td /F1 10 Tf (Un paragraphe ordinaire.) Tj ET");
        let plain = export_text(&doc).unwrap();
        assert!(plain.contains("Titre du document"));
        assert!(plain.contains("Un paragraphe ordinaire."));
        let md = export_markdown(&doc).unwrap();
        assert!(md.contains("# Titre du document"), "{md}");
    }
}
