//! Export DOCX (WordprocessingML, ECMA-376 partie 1 §17).
//!
//! Un `.docx` est une archive ZIP (voir [`crate::zip`]) contenant des parties
//! XML reliées entre elles :
//!
//! ```text
//! [Content_Types].xml          types de chaque partie (§12.2)
//! _rels/.rels                  la partie principale du paquet
//! word/document.xml            le corps du document (§17.2)
//! word/_rels/document.xml.rels styles, numérotation, images
//! word/styles.xml              Normal, Titre 1 à 3, quadrillage de tableau
//! word/numbering.xml           listes à puces et numérotées (§17.9)
//! word/media/imageN.png|jpeg   images incorporées, format d'origine
//! ```
//!
//! Ce qui est conservé depuis `PageText` : paragraphes avec alignement
//! (`w:jc`) et retrait de première ligne (`w:ind`), suites de caractères
//! stylées (`w:rPr` : police, taille, gras, italique, couleur), titres
//! (`w:pStyle` Heading1 à Heading3), listes à puces et numérotées (`w:numPr`),
//! tableaux à bordures (`w:tbl`) et images en ligne (`w:drawing`), insérées à
//! leur place dans l'ordre de lecture. Une page se termine par un saut de
//! page explicite.

// Conversions points → twips (× 20) et → EMU (× 12 700) : valeurs bornées par
// la taille d'une page PDF.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::fmt::Write as _;

use acrux_core::{Rect, Result};
use acrux_document::{collect_pages, Document};

use super::escape_xml;
use super::images::{placed_images, ImageFormat, PlacedImage};
use crate::text::{
    extract_page_text, mark_repeated_headers, Alignment, BlockKind, PageText, Paragraph, Style,
    Table,
};
use crate::zip::ZipWriter;

/// Twips (vingtièmes de point) par point.
const TWIPS: f64 = 20.0;
/// EMU (English Metric Units) par point : 914 400 par pouce, 72 points par pouce.
const EMU: f64 = 12_700.0;
/// Largeur utile d'une page A4 avec marges de 2,54 cm, en twips.
const TEXT_WIDTH_TWIPS: f64 = 9_360.0;

/// Espaces de noms de `document.xml`.
const DOC_NS: &str = concat!(
    " xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"",
    " xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"",
    " xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\"",
    " xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"",
    " xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\""
);

/// En-tête XML commun à toutes les parties.
const XML_HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";

/// Convertit le document en `.docx`.
///
/// # Errors
/// Arbre des pages ou contenu illisible.
pub fn export_docx(doc: &Document) -> Result<Vec<u8>> {
    let pages = collect_pages(doc)?;
    let mut texts = Vec::with_capacity(pages.len());
    for page in &pages {
        texts.push(extract_page_text(doc, page)?);
    }
    mark_repeated_headers(&mut texts);

    let mut media: Vec<(String, ImageFormat, Vec<u8>)> = Vec::new();
    let mut body = String::with_capacity(64 * 1024);
    for (i, (page, text)) in pages.iter().zip(&texts).enumerate() {
        if i > 0 {
            body.push_str("<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>");
        }
        let images = placed_images(doc, page, i);
        write_page(text, &images, &mut media, &mut body);
    }
    // Format de page : celui de la première page.
    let first = pages
        .first()
        .map_or_else(|| Rect::new(0.0, 0.0, 595.0, 842.0), |p| p.crop_box(doc));
    let _ = write!(
        body,
        "<w:sectPr><w:pgSz w:w=\"{}\" w:h=\"{}\"/>\
         <w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" \
         w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/></w:sectPr>",
        (first.width().max(72.0) * TWIPS) as u32,
        (first.height().max(72.0) * TWIPS) as u32
    );

    let mut zip = ZipWriter::new();
    zip.add("[Content_Types].xml", content_types(&media).as_bytes());
    zip.add("_rels/.rels", PACKAGE_RELS.as_bytes());
    zip.add(
        "word/document.xml",
        format!("{XML_HEADER}<w:document{DOC_NS}><w:body>{body}</w:body></w:document>").as_bytes(),
    );
    zip.add(
        "word/_rels/document.xml.rels",
        document_rels(&media).as_bytes(),
    );
    zip.add("word/styles.xml", styles_xml().as_bytes());
    zip.add("word/numbering.xml", numbering_xml().as_bytes());
    for (name, _, data) in &media {
        // Déjà compressées : les stocker évite un travail inutile.
        zip.add_stored(&format!("word/media/{name}"), data);
    }
    Ok(zip.finish())
}

/// Écrit une page : blocs de texte et images intercalés dans l'ordre de lecture.
fn write_page(
    text: &PageText,
    images: &[PlacedImage],
    media: &mut Vec<(String, ImageFormat, Vec<u8>)>,
    out: &mut String,
) {
    // Ordre de lecture : du haut de la page vers le bas, blocs et images mêlés.
    enum Item<'a> {
        Block(usize),
        Image(&'a PlacedImage),
    }
    let mut items: Vec<(f64, Item<'_>)> = text
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.bbox.y1, Item::Block(i)))
        .collect();
    for placed in images {
        items.push((placed_bbox(placed).y1, Item::Image(placed)));
    }
    items.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    for (_, item) in items {
        match item {
            Item::Block(i) => {
                let Some(block) = text.blocks.get(i) else {
                    continue;
                };
                match block.kind {
                    BlockKind::Table(t) => {
                        if let Some(table) = text.tables.get(t) {
                            write_table(table, out);
                        }
                    }
                    _ => {
                        for p in &block.paragraphs {
                            write_paragraph(text, p, out);
                        }
                    }
                }
            }
            Item::Image(placed) => write_image(placed, media, out),
        }
    }
}

/// Boîte de l'image dans l'espace utilisateur PDF (carré unité transformé).
fn placed_bbox(placed: &PlacedImage) -> Rect {
    let m = placed.ctm;
    let corners = [
        m.apply(acrux_core::Point { x: 0.0, y: 0.0 }),
        m.apply(acrux_core::Point { x: 1.0, y: 0.0 }),
        m.apply(acrux_core::Point { x: 0.0, y: 1.0 }),
        m.apply(acrux_core::Point { x: 1.0, y: 1.0 }),
    ];
    let mut r = Rect::new(corners[0].x, corners[0].y, corners[0].x, corners[0].y);
    for p in &corners[1..] {
        r = r.union(&Rect::new(p.x, p.y, p.x, p.y));
    }
    r
}

/// Suite de caractères de même style dans un paragraphe.
struct Run {
    style: Style,
    text: String,
    space_before: bool,
}

/// Ajoute du texte à la dernière suite si le style est identique.
fn push_run(runs: &mut Vec<Run>, style: &Style, text: &str, glue: bool) {
    let same = runs.last().is_some_and(|r| &r.style == style);
    if same {
        if let Some(r) = runs.last_mut() {
            if !glue {
                r.text.push(' ');
            }
            r.text.push_str(text);
        }
        return;
    }
    let space_before = !glue && !runs.is_empty();
    runs.push(Run {
        style: style.clone(),
        text: text.to_string(),
        space_before,
    });
}

/// Suites stylées d'un paragraphe, marqueur de liste retiré et césures
/// réparées (même découpe que `PageText::to_markdown`, mais le style complet
/// sert de critère : police, taille et couleur aussi).
fn paragraph_runs(text: &PageText, p: &Paragraph) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for (li, &line_idx) in p.lines.iter().enumerate() {
        let Some(line) = text.lines.get(line_idx) else {
            continue;
        };
        let skip = usize::from(
            li == 0
                && line.words.len() >= 2
                && p.marker.as_deref() == Some(line.words[0].text.as_str()),
        );
        for (wi, w) in line.words.iter().enumerate().skip(skip) {
            let mut glue = false;
            if wi == skip && li > 0 {
                if let Some(last) = runs.last_mut() {
                    let lower = w.text.chars().next().is_some_and(char::is_lowercase);
                    let lc = last.text.chars().last();
                    if lc == Some('\u{ad}')
                        || (matches!(lc, Some('-' | '\u{2010}' | '\u{2011}')) && lower)
                    {
                        last.text.pop();
                        glue = true;
                    }
                }
            }
            if w.glyphs.is_empty() {
                push_run(&mut runs, &w.style, &w.text, glue);
                continue;
            }
            for (gi, g) in w.glyphs.iter().enumerate() {
                push_run(&mut runs, &g.style(), &g.text, glue || gi > 0);
            }
        }
    }
    runs
}

/// Écrit un paragraphe complet.
fn write_paragraph(text: &PageText, p: &Paragraph, out: &mut String) {
    let runs = paragraph_runs(text, p);
    if runs.is_empty() {
        return;
    }
    out.push_str("<w:p>");
    write_paragraph_properties(p, out);
    for r in &runs {
        write_run(&r.style, r.space_before, &r.text, p.heading > 0, out);
    }
    out.push_str("</w:p>");
}

/// `w:pPr` : style de titre, numérotation, retrait, alignement — dans l'ordre
/// imposé par le schéma (§17.3.1.26).
fn write_paragraph_properties(p: &Paragraph, out: &mut String) {
    let list = p.marker.as_deref().map(numbering_id);
    let indent = (p.first_line_indent * TWIPS).round();
    let jc = match p.alignment {
        Alignment::Left => None,
        Alignment::Right => Some("right"),
        Alignment::Center => Some("center"),
        Alignment::Justify => Some("both"),
    };
    if p.heading == 0 && list.is_none() && indent < 1.0 && jc.is_none() {
        return;
    }
    out.push_str("<w:pPr>");
    if p.heading > 0 {
        let _ = write!(out, "<w:pStyle w:val=\"Heading{}\"/>", p.heading.min(3));
    }
    if let Some(num) = list {
        let _ = write!(
            out,
            "<w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"{num}\"/></w:numPr>"
        );
    } else if indent >= 1.0 {
        let _ = write!(out, "<w:ind w:firstLine=\"{}\"/>", indent as u32);
    }
    if let Some(a) = jc {
        let _ = write!(out, "<w:jc w:val=\"{a}\"/>");
    }
    out.push_str("</w:pPr>");
}

/// Numéro de liste : 1 pour les puces, 2 pour les listes numérotées (§17.9.18).
fn numbering_id(marker: &str) -> u32 {
    let numbered = marker
        .trim_matches(|c: char| matches!(c, '.' | ')' | '('))
        .parse::<u32>()
        .is_ok();
    u32::from(numbered) + 1
}

/// `w:r` avec ses propriétés (§17.3.2.28 pour l'ordre des éléments de `w:rPr`).
fn write_run(style: &Style, space_before: bool, text: &str, heading: bool, out: &mut String) {
    let size = (style.size * 2.0).round().clamp(2.0, 3_276.0) as u32;
    out.push_str("<w:r><w:rPr>");
    if !style.font.is_empty() {
        let font = escape_xml(&style.font);
        let _ = write!(
            out,
            "<w:rFonts w:ascii=\"{font}\" w:hAnsi=\"{font}\" w:cs=\"{font}\"/>"
        );
    }
    // Un titre est déjà gras par son style : ne pas le redemander.
    if style.bold && !heading {
        out.push_str("<w:b/>");
    }
    if style.italic {
        out.push_str("<w:i/>");
    }
    // Comparaison exacte voulue : la couleur par défaut est le noir exact
    // posé par l'extracteur, pas le résultat d'un calcul.
    #[allow(clippy::float_cmp)]
    let colored = style.color != [0.0; 3];
    if colored {
        let _ = write!(out, "<w:color w:val=\"{}\"/>", hex_color(style.color));
    }
    let _ = write!(out, "<w:sz w:val=\"{size}\"/><w:szCs w:val=\"{size}\"/>");
    out.push_str("</w:rPr><w:t xml:space=\"preserve\">");
    if space_before {
        out.push(' ');
    }
    out.push_str(&escape_xml(text));
    out.push_str("</w:t></w:r>");
}

/// Couleur `RRGGBB` majuscule.
fn hex_color(rgb: [f32; 3]) -> String {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "{:02X}{:02X}{:02X}",
        byte(rgb[0]),
        byte(rgb[1]),
        byte(rgb[2])
    )
}

/// Écrit un tableau avec ses bordures (§17.4).
fn write_table(table: &Table, out: &mut String) {
    let cols = table.rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return;
    }
    // Largeurs proportionnelles aux cellules de la ligne la plus fournie.
    let widths = column_widths(table, cols);
    out.push_str(
        "<w:tbl><w:tblPr><w:tblStyle w:val=\"TableGrid\"/>\
         <w:tblW w:w=\"0\" w:type=\"auto\"/><w:tblBorders>",
    );
    for side in ["top", "left", "bottom", "right", "insideH", "insideV"] {
        let _ = write!(
            out,
            "<w:{side} w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>"
        );
    }
    out.push_str("</w:tblBorders></w:tblPr><w:tblGrid>");
    for w in &widths {
        let _ = write!(out, "<w:gridCol w:w=\"{w}\"/>");
    }
    out.push_str("</w:tblGrid>");
    for row in &table.rows {
        out.push_str("<w:tr>");
        for c in 0..cols {
            let width = widths.get(c).copied().unwrap_or(1_000);
            let text = row.get(c).map_or("", |cell| cell.text.as_str());
            let _ = write!(
                out,
                "<w:tc><w:tcPr><w:tcW w:w=\"{width}\" w:type=\"dxa\"/></w:tcPr>"
            );
            for (i, part) in text.split('\n').enumerate() {
                let _ = write!(
                    out,
                    "<w:p><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
                    escape_xml(if i == 0 || !part.is_empty() {
                        part
                    } else {
                        " "
                    })
                );
            }
            out.push_str("</w:tc>");
        }
        out.push_str("</w:tr>");
    }
    // Un tableau doit être suivi d'un paragraphe (§17.2.2).
    out.push_str("</w:tbl><w:p/>");
}

/// Largeurs de colonnes en twips, réparties sur la largeur utile de la page.
fn column_widths(table: &Table, cols: usize) -> Vec<u32> {
    let mut widths = vec![0.0f64; cols];
    for row in &table.rows {
        for (i, cell) in row.iter().enumerate().take(cols) {
            widths[i] = widths[i].max(cell.bbox.width());
        }
    }
    let total: f64 = widths.iter().sum();
    if total <= 0.0 {
        let each = (TEXT_WIDTH_TWIPS / cols as f64) as u32;
        return vec![each.max(1); cols];
    }
    widths
        .iter()
        .map(|w| ((w / total) * TEXT_WIDTH_TWIPS).round().max(1.0) as u32)
        .collect()
}

/// Écrit une image en ligne (`w:drawing`, §20.4) et enregistre sa partie.
fn write_image(
    placed: &PlacedImage,
    media: &mut Vec<(String, ImageFormat, Vec<u8>)>,
    out: &mut String,
) {
    let bbox = placed_bbox(placed);
    if bbox.width() < 1.0 || bbox.height() < 1.0 || placed.image.data.is_empty() {
        return;
    }
    let index = media.len() + 1;
    let name = format!("image{index}.{}", placed.image.format.extension());
    // rId1 : styles, rId2 : numérotation, puis une relation par image.
    let rel = index + 2;
    media.push((name, placed.image.format, placed.image.data.clone()));
    let max_width = TEXT_WIDTH_TWIPS / TWIPS; // en points
    let scale = (max_width / bbox.width()).min(1.0);
    let cx = (bbox.width() * scale * EMU).round().max(1.0) as u64;
    let cy = (bbox.height() * scale * EMU).round().max(1.0) as u64;
    let _ = write!(
        out,
        "<w:p><w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\">\
         <wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/>\
         <wp:docPr id=\"{index}\" name=\"Image {index}\"/>\
         <wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect=\"1\"/></wp:cNvGraphicFramePr>\
         <a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
         <pic:pic><pic:nvPicPr><pic:cNvPr id=\"{index}\" name=\"Image {index}\"/><pic:cNvPicPr/></pic:nvPicPr>\
         <pic:blipFill><a:blip r:embed=\"rId{rel}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
         <pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm>\
         <a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic>\
         </a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"
    );
}

// ---------------------------------------------------------------------------
// Parties fixes du paquet
// ---------------------------------------------------------------------------

/// `_rels/.rels` : la partie principale du paquet (§12.3.2).
const PACKAGE_RELS: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>",
    "</Relationships>"
);

/// `[Content_Types].xml` (§12.2.2).
fn content_types(media: &[(String, ImageFormat, Vec<u8>)]) -> String {
    let mut out = String::from(XML_HEADER);
    out.push_str("<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">");
    out.push_str("<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>");
    out.push_str("<Default Extension=\"xml\" ContentType=\"application/xml\"/>");
    if media.iter().any(|(_, f, _)| *f == ImageFormat::Png) {
        out.push_str("<Default Extension=\"png\" ContentType=\"image/png\"/>");
    }
    if media.iter().any(|(_, f, _)| *f == ImageFormat::Jpeg) {
        out.push_str("<Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>");
    }
    for (part, kind) in [
        ("/word/document.xml", "document.main"),
        ("/word/styles.xml", "styles"),
        ("/word/numbering.xml", "numbering"),
    ] {
        let _ = write!(
            out,
            "<Override PartName=\"{part}\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.{kind}+xml\"/>"
        );
    }
    out.push_str("</Types>");
    out
}

/// `word/_rels/document.xml.rels`.
fn document_rels(media: &[(String, ImageFormat, Vec<u8>)]) -> String {
    const NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let mut out = String::from(XML_HEADER);
    let _ = write!(
        out,
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Id=\"rId1\" Type=\"{NS}/styles\" Target=\"styles.xml\"/>\
         <Relationship Id=\"rId2\" Type=\"{NS}/numbering\" Target=\"numbering.xml\"/>"
    );
    for (i, (name, _, _)) in media.iter().enumerate() {
        let _ = write!(
            out,
            "<Relationship Id=\"rId{}\" Type=\"{NS}/image\" Target=\"media/{name}\"/>",
            i + 3
        );
    }
    out.push_str("</Relationships>");
    out
}

/// `word/styles.xml` : Normal, Heading1 à 3, quadrillage de tableau.
fn styles_xml() -> String {
    let mut out = String::from(XML_HEADER);
    out.push_str(
        "<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
    );
    out.push_str(
        "<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\">\
         <w:name w:val=\"Normal\"/><w:qFormat/></w:style>",
    );
    for (level, size) in [(1u8, 32u32), (2, 26), (3, 24)] {
        let _ = write!(
            out,
            "<w:style w:type=\"paragraph\" w:styleId=\"Heading{level}\">\
             <w:name w:val=\"heading {level}\"/><w:basedOn w:val=\"Normal\"/><w:qFormat/>\
             <w:pPr><w:spacing w:before=\"240\" w:after=\"120\"/><w:outlineLvl w:val=\"{}\"/></w:pPr>\
             <w:rPr><w:b/><w:sz w:val=\"{size}\"/><w:szCs w:val=\"{size}\"/></w:rPr></w:style>",
            level - 1
        );
    }
    out.push_str("<w:style w:type=\"table\" w:styleId=\"TableGrid\"><w:name w:val=\"Table Grid\"/><w:tblPr><w:tblBorders>");
    for side in ["top", "left", "bottom", "right", "insideH", "insideV"] {
        let _ = write!(
            out,
            "<w:{side} w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>"
        );
    }
    out.push_str("</w:tblBorders></w:tblPr></w:style></w:styles>");
    out
}

/// `word/numbering.xml` : une liste à puces (numId 1), une numérotée (numId 2).
fn numbering_xml() -> String {
    let mut out = String::from(XML_HEADER);
    out.push_str(
        "<w:numbering xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
    );
    for (id, format, text) in [(0u32, "bullet", "\u{2022}"), (1, "decimal", "%1.")] {
        let _ = write!(
            out,
            "<w:abstractNum w:abstractNumId=\"{id}\"><w:multiLevelType w:val=\"singleLevel\"/>\
             <w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/><w:numFmt w:val=\"{format}\"/>\
             <w:lvlText w:val=\"{text}\"/><w:lvlJc w:val=\"left\"/>\
             <w:pPr><w:ind w:left=\"720\" w:hanging=\"360\"/></w:pPr></w:lvl></w:abstractNum>"
        );
    }
    out.push_str(
        "<w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num>\
         <w:num w:numId=\"2\"><w:abstractNumId w:val=\"1\"/></w:num></w:numbering>",
    );
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::zip::read;

    fn part(archive: &[u8], name: &str) -> String {
        let entries = read(archive).unwrap();
        let entry = entries.iter().find(|e| e.name == name).unwrap_or_else(|| {
            panic!(
                "partie {name} absente de {:?}",
                entries.iter().map(|e| &e.name).collect::<Vec<_>>()
            )
        });
        String::from_utf8(entry.data.clone()).unwrap()
    }

    #[test]
    fn package_contains_the_required_parts() {
        let doc = super::super::tests::pdf("BT /F1 12 Tf 30 250 Td (Bonjour) Tj ET");
        let archive = export_docx(&doc).unwrap();
        let names: Vec<String> = read(&archive)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        for required in [
            "[Content_Types].xml",
            "_rels/.rels",
            "word/document.xml",
            "word/_rels/document.xml.rels",
            "word/styles.xml",
            "word/numbering.xml",
        ] {
            assert!(
                names.contains(&required.to_string()),
                "{required} manque : {names:?}"
            );
        }
        let document = part(&archive, "word/document.xml");
        assert!(document.starts_with("<?xml version=\"1.0\""));
        assert!(document.contains("Bonjour"));
        assert!(document.ends_with("</w:document>"));
        assert!(document.contains("<w:sectPr>"));
    }

    #[test]
    fn styles_headings_and_alignment_are_written() {
        let doc = super::super::tests::pdf(
            "BT /F1 24 Tf 30 270 Td (Grand titre) Tj /F1 10 Tf 0 -50 Td 1 0 0 rg (Texte rouge) Tj              0 -14 Td 0 g (Une deuxieme ligne de corps de texte ordinaire) Tj              0 -14 Td (et une troisieme, pour que la taille mediane soit celle du corps) Tj ET",
        );
        let document = part(&export_docx(&doc).unwrap(), "word/document.xml");
        assert!(
            document.contains("<w:pStyle w:val=\"Heading1\"/>"),
            "{document}"
        );
        assert!(
            document.contains("<w:color w:val=\"FF0000\"/>"),
            "{document}"
        );
        assert!(
            document.contains("<w:sz w:val=\"20\"/>"),
            "10 pt = 20 demi-points"
        );
        assert!(document.contains("w:rFonts w:ascii=\"Helvetica\""));
    }

    #[test]
    fn bullets_and_tables_produce_numbering_and_borders() {
        // Une puce dessinée, un élément de liste, puis une grille de filets.
        let doc = super::super::tests::pdf(
            "50 250 4 4 re f BT /F1 10 Tf 60 250 Td (Premier point) Tj ET \
             20 100 200 0.5 re f 20 80 200 0.5 re f 20 60 200 0.5 re f \
             20 60 0.5 40 re f 120 60 0.5 40 re f 220 60 0.5 40 re f \
             BT /F1 8 Tf 30 88 Td (A) Tj 100 0 Td (B) Tj -100 -20 Td (C) Tj 100 0 Td (D) Tj ET",
        );
        let document = part(&export_docx(&doc).unwrap(), "word/document.xml");
        assert!(document.contains("<w:numId w:val=\"1\"/>"), "{document}");
        assert!(document.contains("<w:tbl>"), "{document}");
        assert!(document.contains("w:val=\"single\""), "bordures");
        assert!(
            document.contains("</w:tbl><w:p/>"),
            "paragraphe après tableau"
        );
        let numbering = part(&export_docx(&doc).unwrap(), "word/numbering.xml");
        assert!(numbering.contains("w:numFmt w:val=\"bullet\""));
        assert!(numbering.contains("w:numFmt w:val=\"decimal\""));
    }

    #[test]
    fn images_become_drawings_with_a_relation() {
        let src = "%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
             2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
             3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
             /Resources << /XObject << /Im0 5 0 R >> >> >> endobj\n\
             4 0 obj << /Length 31 >>\nstream\nq 80 0 0 40 10 20 cm /Im0 Do Q\nendstream\nendobj\n\
             5 0 obj << /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB \
             /BitsPerComponent 8 /Length 12 >>\nstream\n\x7f\x01\x01\x01\x7f\x01\x01\x01\x7f\x7f\x7f\x01\nendstream\nendobj\n";
        let doc = Document::from_bytes(src.as_bytes().to_vec()).unwrap();
        let archive = export_docx(&doc).unwrap();
        let document = part(&archive, "word/document.xml");
        assert!(document.contains("<w:drawing>"), "{document}");
        assert!(document.contains("r:embed=\"rId3\""));
        // 80 pt × 12 700 = 1 016 000 EMU.
        assert!(
            document.contains("cx=\"1016000\" cy=\"508000\""),
            "{document}"
        );
        let rels = part(&archive, "word/_rels/document.xml.rels");
        assert!(rels.contains("Target=\"media/image1.png\""));
        let types = part(&archive, "[Content_Types].xml");
        assert!(types.contains("Extension=\"png\""));
        assert!(read(&archive)
            .unwrap()
            .iter()
            .any(|e| e.name == "word/media/image1.png"));
    }

    #[test]
    fn escaping_and_page_breaks() {
        let doc = super::super::tests::pdf("BT /F1 12 Tf 30 250 Td (a < b & c > d) Tj ET");
        let document = part(&export_docx(&doc).unwrap(), "word/document.xml");
        assert!(document.contains("a &lt; b &amp; c &gt; d"), "{document}");
        assert!(!document.contains("w:br"), "une seule page : pas de saut");
    }

    #[test]
    fn column_widths_are_proportional() {
        let mut table = Table::default();
        table.rows.push(vec![
            crate::text::Cell {
                bbox: Rect::new(0.0, 0.0, 30.0, 10.0),
                text: "a".into(),
            },
            crate::text::Cell {
                bbox: Rect::new(30.0, 0.0, 120.0, 10.0),
                text: "b".into(),
            },
        ]);
        let widths = column_widths(&table, 2);
        assert_eq!(widths.len(), 2);
        assert!(widths[1] > widths[0] * 2, "{widths:?}");
        assert!(widths.iter().sum::<u32>() <= TEXT_WIDTH_TWIPS as u32 + 2);
        assert_eq!(column_widths(&Table::default(), 3).len(), 3);
    }
}
