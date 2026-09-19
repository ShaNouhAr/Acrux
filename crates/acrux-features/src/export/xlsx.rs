//! Export XLSX (SpreadsheetML, ECMA-376 partie 1 §18) des tableaux détectés.
//!
//! Même conteneur ZIP que le DOCX ([`crate::zip`]) :
//!
//! ```text
//! [Content_Types].xml
//! _rels/.rels                    → xl/workbook.xml
//! xl/workbook.xml                la liste des feuilles (§18.2)
//! xl/_rels/workbook.xml.rels     feuilles, chaînes partagées, styles
//! xl/worksheets/sheetN.xml       une feuille par page contenant un tableau
//! xl/sharedStrings.xml           table des chaînes (§18.4)
//! xl/styles.xml                  deux styles : normal et gras (première ligne)
//! ```
//!
//! Les tableaux viennent de `PageText::tables` (grilles de filets ou colonnes
//! alignées) ; plusieurs tableaux d'une même page sont empilés dans la même
//! feuille, séparés par une ligne vide. Une cellule dont le texte est un
//! nombre est écrite comme un nombre, pas comme une chaîne.

// Indices de lignes et de colonnes : petits entiers bornés par la taille des
// tableaux détectés.
#![allow(clippy::cast_possible_truncation)]

use std::collections::BTreeMap;
use std::fmt::Write as _;

use acrux_core::Result;
use acrux_document::{collect_pages, Document};

use super::escape_xml;
use crate::text::{extract_page_text, Table};
use crate::zip::ZipWriter;

/// En-tête XML commun.
const XML_HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";
/// Espace de noms SpreadsheetML.
const SS_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
/// Espace de noms des relations d'un document Office.
const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// Convertit les tableaux du document en `.xlsx`, une feuille par page.
///
/// Les pages sans tableau ne produisent pas de feuille ; un document qui n'en
/// contient aucun donne un classeur d'une feuille vide (un classeur doit avoir
/// au moins une feuille, §18.2.20).
///
/// # Errors
/// Arbre des pages ou contenu illisible.
pub fn export_tables_xlsx(doc: &Document) -> Result<Vec<u8>> {
    let pages = collect_pages(doc)?;
    // Chaînes partagées : texte → indice, dans l'ordre d'apparition.
    let mut strings: BTreeMap<String, usize> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut sheets: Vec<(String, String)> = Vec::new(); // (nom, XML)
    for (i, page) in pages.iter().enumerate() {
        let text = extract_page_text(doc, page)?;
        if text.tables.is_empty() {
            continue;
        }
        let xml = sheet_xml(&text.tables, &mut strings, &mut order);
        sheets.push((format!("Page {}", i + 1), xml));
    }
    if sheets.is_empty() {
        sheets.push((
            "Page 1".into(),
            format!("{XML_HEADER}<worksheet xmlns=\"{SS_NS}\"><sheetData/></worksheet>"),
        ));
    }

    let mut zip = ZipWriter::new();
    zip.add(
        "[Content_Types].xml",
        content_types(sheets.len()).as_bytes(),
    );
    zip.add("_rels/.rels", package_rels().as_bytes());
    zip.add("xl/workbook.xml", workbook_xml(&sheets).as_bytes());
    zip.add(
        "xl/_rels/workbook.xml.rels",
        workbook_rels(sheets.len()).as_bytes(),
    );
    for (i, (_, xml)) in sheets.iter().enumerate() {
        zip.add(&format!("xl/worksheets/sheet{}.xml", i + 1), xml.as_bytes());
    }
    zip.add("xl/sharedStrings.xml", shared_strings(&order).as_bytes());
    zip.add("xl/styles.xml", STYLES.as_bytes());
    Ok(zip.finish())
}

/// Indice d'une chaîne dans la table partagée, ajoutée si nouvelle.
fn intern(strings: &mut BTreeMap<String, usize>, order: &mut Vec<String>, text: &str) -> usize {
    if let Some(&i) = strings.get(text) {
        return i;
    }
    let i = order.len();
    strings.insert(text.to_string(), i);
    order.push(text.to_string());
    i
}

/// Référence de cellule en notation A1 (§18.17.2.3).
fn cell_ref(col: usize, row: usize) -> String {
    let mut letters = Vec::new();
    let mut c = col;
    loop {
        letters.push(b'A' + (c % 26) as u8);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    letters.reverse();
    format!("{}{}", String::from_utf8_lossy(&letters), row + 1)
}

/// Reconnaît un nombre écrit à la française ou à l'anglaise (espaces de
/// milliers, virgule ou point décimal), pour l'écrire comme un nombre.
fn as_number(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.len() > 24 {
        return None;
    }
    let cleaned: String = trimmed
        .chars()
        .filter(|c| !matches!(c, ' ' | '\u{a0}' | '\u{202f}' | '\''))
        .map(|c| if c == ',' { '.' } else { c })
        .collect();
    if !cleaned
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
    {
        return None;
    }
    cleaned.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// XML d'une feuille : les tableaux de la page, empilés.
fn sheet_xml(
    tables: &[Table],
    strings: &mut BTreeMap<String, usize>,
    order: &mut Vec<String>,
) -> String {
    let mut out = String::from(XML_HEADER);
    let _ = write!(out, "<worksheet xmlns=\"{SS_NS}\"><sheetData>");
    let mut row = 0usize;
    for (ti, table) in tables.iter().enumerate() {
        if ti > 0 {
            row += 1; // ligne vide entre deux tableaux
        }
        for (ri, cells) in table.rows.iter().enumerate() {
            let _ = write!(out, "<row r=\"{}\">", row + 1);
            for (ci, cell) in cells.iter().enumerate() {
                let text = cell.text.trim();
                if text.is_empty() {
                    continue;
                }
                let reference = cell_ref(ci, row);
                // Première ligne du tableau : en-tête, en gras (style 1).
                let style = if ri == 0 { " s=\"1\"" } else { "" };
                if let Some(n) = as_number(text) {
                    let _ = write!(out, "<c r=\"{reference}\"{style}><v>{n}</v></c>");
                } else {
                    let i = intern(strings, order, text);
                    let _ = write!(out, "<c r=\"{reference}\"{style} t=\"s\"><v>{i}</v></c>");
                }
            }
            out.push_str("</row>");
            row += 1;
        }
    }
    out.push_str("</sheetData></worksheet>");
    out
}

/// Nom de feuille accepté par Excel : 31 caractères, sans `[]:*?/\`.
fn sheet_name(name: &str, index: usize) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !matches!(c, '[' | ']' | ':' | '*' | '?' | '/' | '\\'))
        .take(31)
        .collect();
    if cleaned.trim().is_empty() {
        format!("Feuille{}", index + 1)
    } else {
        cleaned
    }
}

/// `xl/workbook.xml`.
fn workbook_xml(sheets: &[(String, String)]) -> String {
    let mut out = String::from(XML_HEADER);
    let _ = write!(
        out,
        "<workbook xmlns=\"{SS_NS}\" xmlns:r=\"{REL_NS}\"><sheets>"
    );
    for (i, (name, _)) in sheets.iter().enumerate() {
        let _ = write!(
            out,
            "<sheet name=\"{}\" sheetId=\"{}\" r:id=\"rId{}\"/>",
            escape_xml(&sheet_name(name, i)),
            i + 1,
            i + 1
        );
    }
    out.push_str("</sheets></workbook>");
    out
}

/// `_rels/.rels`.
fn package_rels() -> String {
    format!(
        "{XML_HEADER}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Id=\"rId1\" Type=\"{REL_NS}/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>"
    )
}

/// `xl/_rels/workbook.xml.rels` : une feuille par relation, puis les chaînes
/// partagées et les styles.
fn workbook_rels(sheets: usize) -> String {
    let mut out = String::from(XML_HEADER);
    out.push_str(
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    for i in 0..sheets {
        let _ = write!(
            out,
            "<Relationship Id=\"rId{}\" Type=\"{REL_NS}/worksheet\" Target=\"worksheets/sheet{}.xml\"/>",
            i + 1,
            i + 1
        );
    }
    let _ = write!(
        out,
        "<Relationship Id=\"rId{}\" Type=\"{REL_NS}/sharedStrings\" Target=\"sharedStrings.xml\"/>\
         <Relationship Id=\"rId{}\" Type=\"{REL_NS}/styles\" Target=\"styles.xml\"/></Relationships>",
        sheets + 1,
        sheets + 2
    );
    out
}

/// `xl/sharedStrings.xml`.
fn shared_strings(order: &[String]) -> String {
    let mut out = String::from(XML_HEADER);
    let _ = write!(
        out,
        "<sst xmlns=\"{SS_NS}\" count=\"{}\" uniqueCount=\"{}\">",
        order.len(),
        order.len()
    );
    for s in order {
        let _ = write!(
            out,
            "<si><t xml:space=\"preserve\">{}</t></si>",
            escape_xml(s)
        );
    }
    out.push_str("</sst>");
    out
}

/// `[Content_Types].xml`.
fn content_types(sheets: usize) -> String {
    const OFFICE: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml";
    let mut out = String::from(XML_HEADER);
    out.push_str("<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">");
    out.push_str("<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>");
    out.push_str("<Default Extension=\"xml\" ContentType=\"application/xml\"/>");
    let _ = write!(
        out,
        "<Override PartName=\"/xl/workbook.xml\" ContentType=\"{OFFICE}.sheet.main+xml\"/>"
    );
    for i in 0..sheets {
        let _ = write!(
            out,
            "<Override PartName=\"/xl/worksheets/sheet{}.xml\" ContentType=\"{OFFICE}.worksheet+xml\"/>",
            i + 1
        );
    }
    let _ = write!(
        out,
        "<Override PartName=\"/xl/sharedStrings.xml\" ContentType=\"{OFFICE}.sharedStrings+xml\"/>\
         <Override PartName=\"/xl/styles.xml\" ContentType=\"{OFFICE}.styles+xml\"/></Types>"
    );
    out
}

/// `xl/styles.xml` : le strict minimum exigé par Excel, plus un style gras
/// (indice 1) pour la ligne d'en-tête.
const STYLES: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    "<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
    "<fonts count=\"2\"><font><sz val=\"11\"/><name val=\"Calibri\"/></font>",
    "<font><b/><sz val=\"11\"/><name val=\"Calibri\"/></font></fonts>",
    "<fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill>",
    "<fill><patternFill patternType=\"gray125\"/></fill></fills>",
    "<borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders>",
    "<cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>",
    "<cellXfs count=\"2\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/>",
    "<xf numFmtId=\"0\" fontId=\"1\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyFont=\"1\"/></cellXfs>",
    "<cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles>",
    "</styleSheet>"
);

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::zip::read;

    fn part(archive: &[u8], name: &str) -> String {
        let entries = read(archive).unwrap();
        let entry = entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("partie {name} absente"));
        String::from_utf8(entry.data.clone()).unwrap()
    }

    /// PDF d'une page avec une grille de filets 3 × 2 et du texte dans chaque case.
    fn pdf_with_table() -> Document {
        super::super::tests::pdf(
            "20 100 200 0.5 re f 20 80 200 0.5 re f 20 60 200 0.5 re f \
             20 60 0.5 40 re f 120 60 0.5 40 re f 220 60 0.5 40 re f \
             BT /F1 8 Tf 30 88 Td (Article) Tj 100 0 Td (Prix) Tj -100 -20 Td (Stylo) Tj 100 0 Td (1,50) Tj ET",
        )
    }

    #[test]
    fn workbook_structure_and_shared_strings() {
        let archive = export_tables_xlsx(&pdf_with_table()).unwrap();
        let names: Vec<String> = read(&archive)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        for required in [
            "[Content_Types].xml",
            "_rels/.rels",
            "xl/workbook.xml",
            "xl/_rels/workbook.xml.rels",
            "xl/worksheets/sheet1.xml",
            "xl/sharedStrings.xml",
            "xl/styles.xml",
        ] {
            assert!(
                names.contains(&required.to_string()),
                "{required} manque : {names:?}"
            );
        }
        let workbook = part(&archive, "xl/workbook.xml");
        assert!(workbook.contains("<sheet name=\"Page 1\" sheetId=\"1\" r:id=\"rId1\"/>"));
        let sheet = part(&archive, "xl/worksheets/sheet1.xml");
        assert!(sheet.contains("<c r=\"A1\" s=\"1\" t=\"s\">"), "{sheet}");
        // « 1,50 » est reconnu comme un nombre.
        assert!(sheet.contains("<v>1.5</v>"), "{sheet}");
        let sst = part(&archive, "xl/sharedStrings.xml");
        assert!(sst.contains("Article") && sst.contains("Stylo"), "{sst}");
    }

    #[test]
    fn document_without_tables_still_has_one_sheet() {
        let doc = super::super::tests::pdf("BT /F1 12 Tf 30 250 Td (Rien qu'un paragraphe) Tj ET");
        let archive = export_tables_xlsx(&doc).unwrap();
        let sheet = part(&archive, "xl/worksheets/sheet1.xml");
        assert!(sheet.contains("<sheetData/>"), "{sheet}");
        assert!(part(&archive, "xl/workbook.xml").contains("sheetId=\"1\""));
    }

    #[test]
    fn cell_references_follow_a1_notation() {
        assert_eq!(cell_ref(0, 0), "A1");
        assert_eq!(cell_ref(1, 9), "B10");
        assert_eq!(cell_ref(25, 0), "Z1");
        assert_eq!(cell_ref(26, 0), "AA1");
        assert_eq!(cell_ref(27, 1), "AB2");
        assert_eq!(cell_ref(701, 0), "ZZ1");
        assert_eq!(cell_ref(702, 0), "AAA1");
    }

    #[test]
    fn numbers_are_recognised_but_not_over_eagerly() {
        assert_eq!(as_number("42"), Some(42.0));
        assert_eq!(as_number(" -3,5 "), Some(-3.5));
        assert_eq!(as_number("1 234,5"), Some(1234.5));
        assert_eq!(as_number("1.5e3"), Some(1500.0));
        assert_eq!(as_number(""), None);
        assert_eq!(as_number("12 €"), None);
        assert_eq!(as_number("Article"), None);
        assert_eq!(as_number("2026-09-18"), None);
    }

    #[test]
    fn sheet_names_are_sanitised() {
        assert_eq!(sheet_name("Page 1", 0), "Page 1");
        assert_eq!(sheet_name("a/b[c]:d*e?f\\g", 0), "abcdefg");
        assert_eq!(sheet_name("  ", 2), "Feuille3");
        assert_eq!(sheet_name(&"x".repeat(50), 0).chars().count(), 31);
    }
}
