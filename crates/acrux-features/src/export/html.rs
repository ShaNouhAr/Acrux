//! Export HTML : un document HTML5 complet, une `<section>` par page.
//!
//! Deux mises en page ([`HtmlLayout`]) :
//!
//! - [`HtmlLayout::Absolute`] — chaque mot est un `<span>` positionné en
//!   absolu (`left`, `top`, `font-size` en points, `color`, `transform:
//!   rotate()` pour le texte tourné) et chaque image un `<img>` transformé
//!   par la matrice qui la place dans la page : le résultat **ressemble** au
//!   PDF, page par page, marge par marge ;
//! - [`HtmlLayout::Flow`] — le texte reflué, en réutilisant
//!   `PageText::to_html` (titres, listes, tableaux, gras / italique) : lisible
//!   sur un téléphone, réutilisable dans un CMS.
//!
//! Les images sont incorporées en URI `data:` dans leur format d'origine
//! (JPEG tel quel, PNG sinon) et les polices incorporées du PDF sont
//! converties en WOFF 1.0 (W3C, *WOFF File Format 1.0*) puis déclarées en
//! `@font-face`. Une police que l'on ne sait pas envelopper (CFF nu, Type 1)
//! est remplacée par une pile de polices système choisie d'après le
//! descripteur : à empattements, à chasse fixe ou linéale.

// Coordonnées et tailles de police en points : conversions bornées par la
// taille des pages (ISO 32000-2 §14.11.2 : 14 400 unités au plus).
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::collections::BTreeMap;
use std::fmt::Write as _;

use acrux_core::{Matrix, Rect, Result};
use acrux_document::{collect_pages, Dict, Document, Name, Object, Page};

use super::images::{placed_images, PlacedImage};
use super::{base64, escape_xml};
use crate::text::{extract_page_text, mark_repeated_headers, PageText, Style};

/// Points CSS par point PDF : les deux valent 1/72 de pouce, mais les
/// matrices `transform` de CSS s'expriment en pixels (96 par pouce).
const PX_PER_PT: f64 = 96.0 / 72.0;

/// Hauteur de hampe supposée, en em : distance de la ligne de base au haut de
/// la boîte d'un `<span>` en `line-height: 1`. Moyenne des polices latines.
const ASCENT: f64 = 0.8;

/// Disposition du texte dans le HTML produit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HtmlLayout {
    /// Positionnement absolu, fidèle à la page.
    #[default]
    Absolute,
    /// Texte en flux, reflué par le navigateur (`PageText::to_html`).
    Flow,
}

/// Options de l'export HTML.
#[derive(Debug, Clone)]
pub struct HtmlOptions {
    /// Disposition.
    pub layout: HtmlLayout,
    /// Titre du document (`<title>`).
    pub title: String,
    /// Incorporer les images en URI `data:`.
    pub embed_images: bool,
    /// Incorporer les polices du PDF en `@font-face` (WOFF).
    pub embed_fonts: bool,
    /// Pages à exporter (0 = première) ; vide = toutes.
    pub pages: Vec<usize>,
}

impl Default for HtmlOptions {
    fn default() -> Self {
        HtmlOptions {
            layout: HtmlLayout::Absolute,
            title: "Document".into(),
            embed_images: true,
            embed_fonts: true,
            pages: Vec::new(),
        }
    }
}

/// Exporte le document en un seul fichier HTML autonome.
///
/// # Errors
/// Arbre des pages ou contenu illisible.
pub fn export_html(doc: &Document, options: &HtmlOptions) -> Result<String> {
    let pages = collect_pages(doc)?;
    let indices: Vec<usize> = if options.pages.is_empty() {
        (0..pages.len()).collect()
    } else {
        options
            .pages
            .iter()
            .copied()
            .filter(|&i| i < pages.len())
            .collect()
    };
    let mut texts = Vec::with_capacity(indices.len());
    for &i in &indices {
        texts.push(extract_page_text(doc, &pages[i])?);
    }
    mark_repeated_headers(&mut texts);
    let faces = if options.embed_fonts {
        collect_faces(doc, &pages, &indices)
    } else {
        BTreeMap::new()
    };

    let mut out = String::with_capacity(64 * 1024);
    out.push_str("<!DOCTYPE html>\n<html lang=\"fr\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<meta name=\"generator\" content=\"Acrux\">\n<title>");
    out.push_str(&escape_xml(&options.title));
    out.push_str("</title>\n<style>\n");
    out.push_str(base_css(options.layout));
    for face in faces.values() {
        if let Some(woff) = &face.woff {
            let _ = writeln!(
                out,
                "@font-face{{font-family:'{}';src:url(data:font/woff;base64,{}) format('woff');font-display:block}}",
                face.family,
                base64(woff)
            );
        }
    }
    out.push_str("</style>\n</head>\n<body>\n");

    for (&i, text) in indices.iter().zip(&texts) {
        let page = &pages[i];
        match options.layout {
            HtmlLayout::Flow => {
                let _ = writeln!(out, "<section class=\"page flow\" data-page=\"{}\">", i + 1);
                out.push_str(&text.to_html());
                out.push_str("</section>\n");
            }
            HtmlLayout::Absolute => absolute_page(doc, page, i, text, &faces, options, &mut out),
        }
    }
    out.push_str("</body>\n</html>\n");
    Ok(out)
}

/// Feuille de style commune.
fn base_css(layout: HtmlLayout) -> &'static str {
    match layout {
        HtmlLayout::Absolute => concat!(
            "html{color-scheme:light}\n",
            "body{margin:0;background:#525659;font-family:\"Segoe UI\",Arial,Helvetica,sans-serif}\n",
            ".page{position:relative;margin:12pt auto;background:#fff;overflow:hidden;",
            "box-shadow:0 1pt 6pt rgba(0,0,0,.45)}\n",
            ".page span{position:absolute;white-space:pre;line-height:1;transform-origin:0 0}\n",
            ".page img{position:absolute;left:0;top:0;transform-origin:0 0;",
            "image-rendering:auto;-webkit-user-drag:none}\n",
        ),
        HtmlLayout::Flow => concat!(
            "html{color-scheme:light dark}\n",
            "body{margin:0;background:#fff;color:#111;",
            "font-family:Georgia,\"Times New Roman\",serif;line-height:1.5}\n",
            ".page{max-width:42em;margin:0 auto;padding:2em 1.2em}\n",
            ".page+.page{border-top:1px solid #ddd}\n",
            "table{border-collapse:collapse;margin:1em 0}\n",
            "th,td{border:1px solid #bbb;padding:.3em .6em;text-align:left}\n",
            "header,footer{color:#666;font-size:.85em}\n",
            "@media (prefers-color-scheme:dark){body{background:#16181c;color:#e8e8e8}",
            "th,td{border-color:#555}header,footer{color:#aaa}}\n",
        ),
    }
}

/// Écrit une page en positionnement absolu.
// Une page se décrit par son document, sa position, son texte, ses polices et
// ses options : les regrouper dans une structure éphémère n'aiderait pas.
#[allow(clippy::too_many_arguments)]
fn absolute_page(
    doc: &Document,
    page: &Page,
    index: usize,
    text: &PageText,
    faces: &BTreeMap<String, Face>,
    options: &HtmlOptions,
    out: &mut String,
) {
    let crop = page.crop_box(doc);
    let rotate = page.rotate(doc);
    let (w, h) = match rotate {
        90 | 270 => (crop.height(), crop.width()),
        _ => (crop.width(), crop.height()),
    };
    let to_css = page_matrix(&crop, rotate);
    let _ = writeln!(
        out,
        "<section class=\"page\" data-page=\"{}\" style=\"width:{}pt;height:{}pt\">",
        index + 1,
        num(w),
        num(h)
    );
    if options.embed_images {
        for placed in placed_images(doc, page, index) {
            write_image(&placed, &to_css, out);
        }
    }
    for line in &text.lines {
        for word in &line.words {
            write_word(word, line.size(), &to_css, rotate, faces, out);
        }
    }
    out.push_str("</section>\n");
}

/// Matrice espace utilisateur PDF → espace CSS de la page (origine en haut à
/// gauche, y vers le bas), rotation `/Rotate` comprise. Même construction que
/// `acrux_render::page::base_matrix` à l'échelle 1.
fn page_matrix(crop: &Rect, rotate: i32) -> Matrix {
    let flip = Matrix::new(1.0, 0.0, 0.0, -1.0, -crop.x0, crop.y1);
    let rot = match rotate {
        90 => Matrix::new(0.0, 1.0, -1.0, 0.0, crop.height(), 0.0),
        180 => Matrix::new(-1.0, 0.0, 0.0, -1.0, crop.width(), crop.height()),
        270 => Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, crop.width()),
        _ => Matrix::IDENTITY,
    };
    flip.then(&rot)
}

/// Écrit une image placée par sa matrice.
///
/// L'espace image est le carré unité, ligne 0 **en haut** (§8.9.4) : la
/// matrice CSS est donc celle du PDF avec l'axe vertical retourné et
/// l'origine déplacée au coin haut-gauche de l'image.
fn write_image(placed: &PlacedImage, to_css: &Matrix, out: &mut String) {
    let (iw, ih) = (placed.image.width, placed.image.height);
    if iw == 0 || ih == 0 {
        return;
    }
    let a = placed.ctm.then(to_css);
    let (fw, fh) = (f64::from(iw), f64::from(ih));
    let k = PX_PER_PT;
    let _ = writeln!(
        out,
        "<img width=\"{iw}\" height=\"{ih}\" alt=\"\" style=\"transform:matrix({},{},{},{},{},{})\" src=\"data:{};base64,{}\">\n",
        num(a.a * k / fw),
        num(a.b * k / fw),
        num(-a.c * k / fh),
        num(-a.d * k / fh),
        num((a.c + a.e) * k),
        num((a.d + a.f) * k),
        placed.image.format.media_type(),
        base64(&placed.image.data)
    );
}

/// Écrit un mot en `<span>` positionné sur sa ligne de base.
fn write_word(
    word: &crate::text::Word,
    line_size: f64,
    to_css: &Matrix,
    rotate: i32,
    faces: &BTreeMap<String, Face>,
    out: &mut String,
) {
    if word.text.trim().is_empty() {
        return;
    }
    let style = &word.style;
    let size = if style.size > 0.1 {
        style.size
    } else {
        line_size
    };
    // Origine de la ligne de base : projections inverses du premier glyphe.
    let (origin, angle) = match word.glyphs.first() {
        Some(g) => {
            let (s, c) = g.angle.sin_cos();
            (
                acrux_core::Point {
                    x: g.along * c - g.perp * s,
                    y: g.along * s + g.perp * c,
                },
                g.angle,
            )
        }
        None => (
            acrux_core::Point {
                x: word.bbox.x0,
                y: word.bbox.y0,
            },
            0.0,
        ),
    };
    let p = to_css.apply(origin);
    // L'axe y du CSS descend : une rotation trigonométrique du PDF devient une
    // rotation horaire, à laquelle s'ajoute la rotation de la page.
    let degrees = -angle.to_degrees() + f64::from(rotate);
    let face = faces.get(style.font.as_str());
    let embedded = face.is_some_and(|f| f.woff.is_some());
    out.push_str("<span style=\"left:");
    out.push_str(&num(p.x));
    out.push_str("pt;top:");
    out.push_str(&num(p.y));
    out.push_str("pt;font-size:");
    out.push_str(&num(size));
    out.push_str("pt;color:");
    out.push_str(&css_color(style.color));
    out.push_str(";font-family:");
    out.push_str(&font_family(face, style));
    if !embedded && style.bold {
        out.push_str(";font-weight:700");
    }
    if !embedded && style.italic {
        out.push_str(";font-style:italic");
    }
    out.push_str(";transform:");
    if degrees.abs() > 0.01 {
        let _ = write!(out, "rotate({}deg) ", num(degrees));
    }
    let _ = write!(out, "translateY({}em)", num(-ASCENT));
    out.push_str("\">");
    out.push_str(&escape_xml(&visible_text(word)));
    out.push_str("</span>\n");
}

/// Texte d'un mot, glyphes superposés retirés.
///
/// Un texte tracé deux fois — remplissage puis contour (`Tr 2`, §9.3.6), ou
/// faux gras par double frappe décalée — donne deux glyphes au même endroit :
/// à l'écran ils se confondent, mais en HTML ils s'écriraient l'un après
/// l'autre (« CCoonnttoouurr »). Deux glyphes de même texte distants de moins
/// d'un vingtième de cadratin ne sont donc écrits qu'une fois.
fn visible_text(word: &crate::text::Word) -> String {
    if word.glyphs.len() < 2 {
        return word.text.clone();
    }
    let mut out = String::with_capacity(word.text.len());
    let mut previous: Option<(&str, f64, f64)> = None; // (texte, position, taille)
    for g in &word.glyphs {
        if let Some((text, along, size)) = previous {
            if text == g.text && (g.along - along).abs() <= size.max(g.size) * 0.05 {
                continue;
            }
        }
        out.push_str(&g.text);
        previous = Some((&g.text, g.along, g.size));
    }
    out
}

/// Couleur CSS `#rrggbb`.
fn css_color(rgb: [f32; 3]) -> String {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        byte(rgb[0]),
        byte(rgb[1]),
        byte(rgb[2])
    )
}

/// Déclaration `font-family` : la police incorporée puis sa pile de secours.
fn font_family(face: Option<&Face>, style: &Style) -> String {
    match face {
        Some(f) if f.woff.is_some() => format!("'{}',{}", f.family, f.stack),
        Some(f) => f.stack.to_string(),
        None => fallback_stack(&style.font, 0).to_string(),
    }
}

/// Nombre CSS compact : deux décimales au plus, sans zéros inutiles.
fn num(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    // `-0.0` s'écrirait « -0 » : le zéro négatif n'a pas de sens en CSS.
    let v = if v == 0.0 { 0.0 } else { v };
    let s = format!("{v:.2}");
    let s = if s == "-0.00" { "0.00".to_string() } else { s };
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" {
        "0".into()
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------------------
// Polices
// ---------------------------------------------------------------------------

/// Police du document préparée pour le HTML.
struct Face {
    /// Nom de famille CSS unique.
    family: String,
    /// Octets WOFF, si la police incorporée a pu être enveloppée.
    woff: Option<Vec<u8>>,
    /// Pile de polices système de secours.
    stack: &'static str,
}

/// Piles de secours documentées, choisies d'après `/Flags` (tableau 123) et le
/// nom de la police.
fn fallback_stack(name: &str, flags: u64) -> &'static str {
    const SERIF: &str = "Georgia,'Times New Roman',Times,serif";
    const MONO: &str = "'Cascadia Mono','Courier New',Courier,monospace";
    const SANS: &str = "'Segoe UI',Arial,Helvetica,sans-serif";
    let lower = name.to_ascii_lowercase();
    if flags & 1 != 0 || lower.contains("mono") || lower.contains("courier") {
        return MONO;
    }
    if flags & (1 << 1) != 0
        || lower.contains("times")
        || lower.contains("serif")
        || lower.contains("georgia")
        || lower.contains("garamond")
        || lower.contains("roman")
        || lower.contains("minion")
    {
        return SERIF;
    }
    SANS
}

/// Nom de police sans le préfixe de sous-ensemble `ABCDEF+` (§9.6.4).
fn strip_subset_prefix(name: &str) -> &str {
    match name.split_once('+') {
        Some((prefix, rest))
            if prefix.len() == 6 && prefix.bytes().all(|b| b.is_ascii_uppercase()) =>
        {
            rest
        }
        _ => name,
    }
}

/// Nom de famille CSS sûr, dérivé du nom PDF.
fn css_family(name: &str, index: usize) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("akf{index}-{}", cleaned.trim_matches('-'))
}

/// Inventorie les polices des pages demandées et convertit en WOFF ce qui
/// peut l'être.
fn collect_faces(doc: &Document, pages: &[Page], indices: &[usize]) -> BTreeMap<String, Face> {
    let mut faces: BTreeMap<String, Face> = BTreeMap::new();
    for &i in indices {
        let Some(page) = pages.get(i) else { continue };
        let Some(fonts) = doc
            .dict_get(&page.dict, "Resources")
            .ok()
            .flatten()
            .and_then(|r| r.as_dict().cloned())
            .and_then(|r| {
                doc.dict_get(&r, "Font")
                    .ok()
                    .flatten()
                    .map(|f| (*f).clone())
            })
            .and_then(|f| f.as_dict().cloned())
        else {
            continue;
        };
        for entry in fonts.values() {
            let Ok(resolved) = doc.resolve(entry) else {
                continue;
            };
            let Some(dict) = resolved.as_dict().cloned() else {
                continue;
            };
            let Some((name, face)) = face_from_font(doc, &dict, faces.len()) else {
                continue;
            };
            faces.entry(name).or_insert(face);
        }
    }
    faces
}

/// Construit la `Face` d'un dictionnaire de police (§9.6, §9.7).
fn face_from_font(doc: &Document, dict: &Dict, index: usize) -> Option<(String, Face)> {
    let base = dict
        .get(&Name::new("BaseFont"))
        .and_then(Object::as_name)
        .map(acrux_document::Name::as_str)
        .unwrap_or_default();
    let name = strip_subset_prefix(&base).to_string();
    if name.is_empty() {
        return None;
    }
    // Type 0 : le descripteur est dans la police descendante (§9.7.3).
    let descriptor = doc
        .dict_get(dict, "FontDescriptor")
        .ok()
        .flatten()
        .and_then(|d| d.as_dict().cloned())
        .or_else(|| {
            let descendants = doc.dict_get(dict, "DescendantFonts").ok().flatten()?;
            let first = descendants.as_array()?.first()?.clone();
            let cid = doc.resolve(&first).ok()?.as_dict().cloned()?;
            doc.dict_get(&cid, "FontDescriptor")
                .ok()
                .flatten()
                .and_then(|d| d.as_dict().cloned())
        });
    let flags = descriptor
        .as_ref()
        .and_then(|d| d.get(&Name::new("Flags")).and_then(Object::as_i64))
        .unwrap_or(0)
        .max(0) as u64;
    let woff = descriptor.as_ref().and_then(|d| embedded_woff(doc, d));
    Some((
        name.clone(),
        Face {
            family: css_family(&name, index),
            woff,
            stack: fallback_stack(&name, flags),
        },
    ))
}

/// Octets WOFF de la police incorporée, si elle est au format sfnt
/// (`/FontFile2`, ou `/FontFile3` de sous-type `/OpenType`). Un CFF nu ou un
/// Type 1 n'est pas enveloppable tel quel : on renvoie `None` et l'appelant
/// substitue une police système.
fn embedded_woff(doc: &Document, descriptor: &Dict) -> Option<Vec<u8>> {
    for key in ["FontFile2", "FontFile3"] {
        let Some(ff) = doc.dict_get(descriptor, key).ok().flatten() else {
            continue;
        };
        let Ok(data) = doc.stream_data(&ff) else {
            continue;
        };
        if let Some(woff) = sfnt_to_woff(&data.data) {
            return Some(woff);
        }
    }
    None
}

/// Signature WOFF 1.0 (`wOFF`).
const WOFF_SIGNATURE: u32 = 0x774F_4646;

/// Enveloppe un fichier sfnt (TrueType `00010000` / `true`, OpenType `OTTO`,
/// premier fichier d'une collection `ttcf`) dans un conteneur WOFF 1.0.
///
/// Chaque table est compressée en zlib par le compresseur maison quand cela
/// gagne de la place, sinon stockée telle quelle (WOFF §4 : `compLength ==
/// origLength` signale une table non compressée).
fn sfnt_to_woff(bytes: &[u8]) -> Option<Vec<u8>> {
    let be32 = |p: usize| -> Option<u32> {
        Some(u32::from_be_bytes(bytes.get(p..p + 4)?.try_into().ok()?))
    };
    let be16 = |p: usize| -> Option<u16> {
        Some(u16::from_be_bytes(bytes.get(p..p + 2)?.try_into().ok()?))
    };
    let base = if bytes.starts_with(b"ttcf") {
        be32(12)? as usize
    } else {
        0
    };
    let flavor = be32(base)?;
    if !matches!(flavor, 0x0001_0000 | 0x4F54_544F | 0x7472_7565) {
        return None;
    }
    let num_tables = be16(base + 4)?;
    if num_tables == 0 || usize::from(num_tables) > 512 {
        return None;
    }
    // Répertoire sfnt : tag, somme de contrôle, position, longueur.
    let mut tables: Vec<(u32, u32, &[u8])> = Vec::with_capacity(num_tables.into());
    for i in 0..usize::from(num_tables) {
        let p = base + 12 + 16 * i;
        let tag = be32(p)?;
        let checksum = be32(p + 4)?;
        let offset = be32(p + 8)? as usize;
        let length = be32(p + 12)? as usize;
        let data = bytes.get(offset..offset.checked_add(length)?)?;
        tables.push((tag, checksum, data));
    }
    tables.sort_by_key(|(tag, _, _)| *tag);
    let round4 = |n: usize| n.div_ceil(4) * 4;
    let total_sfnt = 12
        + 16 * tables.len()
        + tables
            .iter()
            .map(|(_, _, d)| round4(d.len()))
            .sum::<usize>();

    // Corps : données de table, chacune alignée sur 4 octets.
    let mut body = Vec::with_capacity(bytes.len());
    let directory_size = 44 + 20 * tables.len();
    let mut directory = Vec::with_capacity(directory_size);
    for (tag, checksum, data) in &tables {
        let zlib = acrux_codecs::flate::compress(data, 9);
        let payload: &[u8] = if zlib.len() < data.len() { &zlib } else { data };
        let offset = u32::try_from(directory_size + body.len()).ok()?;
        directory.extend_from_slice(&tag.to_be_bytes());
        directory.extend_from_slice(&offset.to_be_bytes());
        directory.extend_from_slice(&u32::try_from(payload.len()).ok()?.to_be_bytes());
        directory.extend_from_slice(&u32::try_from(data.len()).ok()?.to_be_bytes());
        directory.extend_from_slice(&checksum.to_be_bytes());
        body.extend_from_slice(payload);
        body.resize(round4(body.len()), 0);
    }
    let length = u32::try_from(directory_size + body.len()).ok()?;
    let mut out = Vec::with_capacity(length as usize);
    out.extend_from_slice(&WOFF_SIGNATURE.to_be_bytes());
    out.extend_from_slice(&flavor.to_be_bytes());
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&num_tables_be(tables.len())?);
    out.extend_from_slice(&0u16.to_be_bytes()); // réservé
    out.extend_from_slice(&u32::try_from(total_sfnt).ok()?.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // version majeure
    out.extend_from_slice(&0u16.to_be_bytes()); // version mineure
    out.extend_from_slice(&[0u8; 20]); // métadonnées et données privées absentes
    out.extend_from_slice(&directory);
    out.extend_from_slice(&body);
    Some(out)
}

/// Nombre de tables sur deux octets gros-boutiens.
fn num_tables_be(n: usize) -> Option<[u8; 2]> {
    Some(u16::try_from(n).ok()?.to_be_bytes())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn pdf(content: &str) -> Document {
        super::super::tests::pdf(content)
    }

    #[test]
    fn absolute_html_places_words_and_is_well_formed() {
        let doc = pdf("BT /F1 12 Tf 30 250 Td 1 0 0 rg (Bonjour le monde) Tj ET");
        let html = export_html(&doc, &HtmlOptions::default()).unwrap();
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.trim_end().ends_with("</html>"));
        assert!(html.contains(
            "<section class=\"page\" data-page=\"1\" style=\"width:300pt;height:300pt\">"
        ));
        assert!(html.contains(">Bonjour</span>"), "{html}");
        assert!(html.contains("color:#ff0000"), "couleur reprise : {html}");
        // Ligne de base à y = 250 dans une page de 300 : top = 50 pt.
        assert!(html.contains("top:50pt"), "{html}");
        assert!(html.contains("font-size:12pt"));
        // Autant de balises ouvrantes que de fermantes.
        assert_eq!(
            html.matches("<span").count(),
            html.matches("</span>").count()
        );
        assert_eq!(
            html.matches("<section").count(),
            html.matches("</section>").count()
        );
    }

    #[test]
    fn flow_html_reuses_page_text_to_html() {
        let doc = pdf(
            "BT /F1 20 Tf 30 250 Td (Un titre) Tj /F1 10 Tf 0 -60 Td (Du corps de texte.) Tj ET",
        );
        let options = HtmlOptions {
            layout: HtmlLayout::Flow,
            ..HtmlOptions::default()
        };
        let html = export_html(&doc, &options).unwrap();
        assert!(html.contains("<h1>Un titre</h1>"), "{html}");
        assert!(html.contains("<p>Du corps de texte.</p>"), "{html}");
        assert!(!html.contains("position:absolute"));
    }

    #[test]
    fn rotated_text_gets_a_css_rotation() {
        let doc = pdf("BT /F1 12 Tf 0.866 0.5 -0.5 0.866 50 50 Tm (Tourne) Tj ET");
        let html = export_html(&doc, &HtmlOptions::default()).unwrap();
        assert!(html.contains("rotate(-30deg)"), "{html}");
    }

    #[test]
    fn images_are_embedded_as_data_uris() {
        let src = "%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
             2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
             3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R \
             /Resources << /XObject << /Im0 5 0 R >> >> >> endobj\n\
             4 0 obj << /Length 31 >>\nstream\nq 80 0 0 40 10 20 cm /Im0 Do Q\nendstream\nendobj\n\
             5 0 obj << /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB \
             /BitsPerComponent 8 /Length 12 >>\nstream\n\x7f\x01\x01\x01\x7f\x01\x01\x01\x7f\x7f\x7f\x01\nendstream\nendobj\n";
        let doc = Document::from_bytes(src.as_bytes().to_vec()).unwrap();
        let html = export_html(&doc, &HtmlOptions::default()).unwrap();
        assert!(html.contains("src=\"data:image/png;base64,"), "{html}");
        // 80 pt de large et 40 de haut sur 2 px : 80·(96/72)/2 = 53,33 et 26,67 ;
        // l'origine est le coin haut-gauche de l'image, à (10 ; 100−20−40) pt.
        assert!(
            html.contains("matrix(53.33,0,0,26.67,13.33,53.33)"),
            "{html}"
        );
        // Sans incorporation, aucune image.
        let options = HtmlOptions {
            embed_images: false,
            ..HtmlOptions::default()
        };
        assert!(!export_html(&doc, &options).unwrap().contains("<img"));
    }

    #[test]
    fn woff_wraps_an_sfnt_and_refuses_the_rest() {
        // Fichier sfnt minimal : deux tables de contenu quelconque.
        let mut sfnt = Vec::new();
        sfnt.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        sfnt.extend_from_slice(&2u16.to_be_bytes());
        sfnt.extend_from_slice(&[0; 6]); // searchRange, entrySelector, rangeShift
        let head: Vec<u8> = (0..64u8).collect();
        let cmap: Vec<u8> = vec![7; 300];
        let head_off = 12 + 32;
        let cmap_off = head_off + head.len();
        for (tag, off, len) in [
            (*b"head", head_off, head.len()),
            (*b"cmap", cmap_off, cmap.len()),
        ] {
            sfnt.extend_from_slice(&tag);
            sfnt.extend_from_slice(&0u32.to_be_bytes());
            sfnt.extend_from_slice(&u32::try_from(off).unwrap().to_be_bytes());
            sfnt.extend_from_slice(&u32::try_from(len).unwrap().to_be_bytes());
        }
        sfnt.extend_from_slice(&head);
        sfnt.extend_from_slice(&cmap);
        let woff = sfnt_to_woff(&sfnt).unwrap();
        assert_eq!(&woff[..4], b"wOFF");
        assert_eq!(
            u32::from_be_bytes(woff[4..8].try_into().unwrap()),
            0x0001_0000
        );
        assert_eq!(
            u32::from_be_bytes(woff[8..12].try_into().unwrap()) as usize,
            woff.len()
        );
        assert_eq!(u16::from_be_bytes(woff[12..14].try_into().unwrap()), 2);
        // La table de 300 octets identiques doit avoir été compressée.
        let comp = u32::from_be_bytes(woff[44 + 8..44 + 12].try_into().unwrap());
        let orig = u32::from_be_bytes(woff[44 + 12..44 + 16].try_into().unwrap());
        assert!(comp < orig, "{comp} < {orig}");
        // Formats non sfnt refusés.
        assert!(sfnt_to_woff(b"%!PS-AdobeFont-1.0").is_none());
        assert!(sfnt_to_woff(&[1, 0, 4, 2]).is_none());
        assert!(sfnt_to_woff(&[]).is_none());
    }

    #[test]
    fn fallback_stacks_follow_flags_and_names() {
        assert!(fallback_stack("Courier", 0).contains("monospace"));
        assert!(fallback_stack("Inconnue", 1).contains("monospace"));
        assert!(fallback_stack("Times-Roman", 0).contains("serif"));
        assert!(fallback_stack("Inconnue", 2).contains("serif"));
        assert!(fallback_stack("Helvetica", 0).contains("sans-serif"));
        assert_eq!(css_family("ABCDEF+Arial,Bold", 3), "akf3-ABCDEF-Arial-Bold");
        assert_eq!(strip_subset_prefix("ABCDEF+Arial"), "Arial");
        assert_eq!(strip_subset_prefix("Arial"), "Arial");
    }

    #[test]
    fn overstruck_glyphs_are_written_once() {
        use crate::text::{lines_from_glyphs, test_support::line_glyphs};
        // « Tr 2 » : le même mot tracé deux fois, remplissage puis contour.
        let mut glyphs = line_glyphs(50.0, 700.0, 12.0, "Contour");
        glyphs.extend(line_glyphs(50.0, 700.0, 12.0, "Contour"));
        let text = lines_from_glyphs(glyphs);
        let word = &text.lines[0].words[0];
        assert_eq!(
            word.text, "CCoonnttoouurr",
            "l'extraction voit bien deux passes"
        );
        assert_eq!(visible_text(word), "Contour");
        // Les vraies lettres doublées sont conservées.
        let text = lines_from_glyphs(line_glyphs(50.0, 700.0, 12.0, "appelle"));
        assert_eq!(visible_text(&text.lines[0].words[0]), "appelle");
    }

    #[test]
    fn numbers_are_compact() {
        assert_eq!(num(1.0), "1");
        assert_eq!(num(1.5), "1.5");
        assert_eq!(num(1.005), "1"); // arrondi à deux décimales puis élagage
        assert_eq!(num(-0.0), "0");
        assert_eq!(num(-0.001), "0");
        assert_eq!(num(f64::NAN), "0");
    }
}
