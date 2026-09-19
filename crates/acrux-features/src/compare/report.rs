//! Rapport de comparaison : un PDF qui met les deux documents côte à côte.
//!
//! Le rapport est un document neuf. Chaque page comparée y est reprise telle
//! quelle, sous forme de **XObject de formulaire** (§8.10) : le flux de
//! contenu d'origine est recopié et ses ressources importées, si bien que la
//! page apparaît exactement comme dans son document, à l'échelle près. Les
//! différences sont ensuite peintes par-dessus : vert pour un ajout, rouge
//! pour une suppression, bleu pour un déplacement (un remplacement se lit
//! donc en rouge à gauche et en vert à droite).
//!
//! Tout est écrit en syntaxe PDF minimale, avec les polices standard
//! Helvetica et Helvetica-Bold (non incorporées, comme le fait Acrobat pour
//! ses propres rapports) : le fichier reste petit et lisible.

use std::fmt::Write as _;

use acrux_core::{Error, Matrix, Rect, Result};
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef, Page, SaveOptions};
use acrux_render::page::page_content;

use crate::compare::{Comparison, DiffKind, Difference, PageDiff};
use crate::pages::Importer;

/// Réglages du rapport PDF.
#[derive(Debug, Clone)]
pub struct ReportOptions {
    /// Nom affiché du document de gauche (« avant »).
    pub old_title: String,
    /// Nom affiché du document de droite (« après »).
    pub new_title: String,
    /// Largeur des pages du rapport en points.
    pub page_width: f64,
    /// Hauteur des pages du rapport en points.
    pub page_height: f64,
    /// Nombre maximal de différences détaillées par page dans le résumé.
    pub extracts_per_page: usize,
    /// Ajouter une planche par paire de pages ; sinon le rapport se limite au
    /// résumé.
    pub include_pages: bool,
    /// N'inclure une planche que pour les pages qui diffèrent.
    pub changed_pages_only: bool,
}

impl Default for ReportOptions {
    /// A4 à l'italienne (842 × 595 pt) : deux pages A4 à la française y
    /// tiennent côte à côte sans être minuscules.
    fn default() -> Self {
        Self {
            old_title: "avant".into(),
            new_title: "après".into(),
            page_width: 842.0,
            page_height: 595.0,
            extracts_per_page: 3,
            include_pages: true,
            changed_pages_only: false,
        }
    }
}

/// Vert des ajouts.
const ADDED: [f64; 3] = [0.09, 0.55, 0.26];
/// Rouge des suppressions.
const REMOVED: [f64; 3] = [0.80, 0.14, 0.14];
/// Bleu des déplacements.
const MOVED: [f64; 3] = [0.13, 0.36, 0.78];
/// Gris des pixels changés (comparaison visuelle).
const PIXELS: [f64; 3] = [0.55, 0.35, 0.05];
/// Noir du texte.
const INK: [f64; 3] = [0.10, 0.10, 0.10];
/// Gris des libellés secondaires.
const MUTED: [f64; 3] = [0.42, 0.42, 0.42];

/// Marge des pages du rapport.
const MARGIN: f64 = 28.0;

/// Largeurs Helvetica (AFM Adobe), codes 32 à 126, pour 1000 unités.
/// Table locale : le rapport n'a besoin que de mesurer ses propres libellés.
const HELVETICA: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

/// Largeurs Helvetica-Bold, codes 32 à 126.
const HELVETICA_BOLD: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, 975, 722, 722, 722, 722, 667,
    611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 333, 278, 333, 584, 556, 333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556,
    278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
];

/// Octet WinAnsiEncoding (annexe D) d'un caractère ; `?` si absent.
/// Le rapport n'écrit que ses propres libellés et des extraits de texte : la
/// plage Latin-1 plus la ponctuation courante suffit.
fn win_ansi(c: char) -> u8 {
    match c {
        '\u{20}'..='\u{7E}' | '\u{A0}'..='\u{FF}' => u8::try_from(c as u32).unwrap_or(b'?'),
        '€' => 0x80,
        '\u{2026}' => 0x85,
        'Œ' => 0x8C,
        'œ' => 0x9C,
        '\u{2018}' | '\u{2019}' => 0x92,
        '\u{201C}' => 0x93,
        '\u{201D}' => 0x94,
        '\u{2022}' => 0x95,
        '\u{2013}' => 0x96,
        '\u{2014}' => 0x97,
        _ => b'?',
    }
}

/// Largeur d'un texte en unités de taille de police (à multiplier par la taille).
fn text_width(text: &str, bold: bool) -> f64 {
    let table = if bold { &HELVETICA_BOLD } else { &HELVETICA };
    text.chars()
        .map(|c| {
            let index = usize::from(win_ansi(c)).saturating_sub(32);
            f64::from(table.get(index).copied().unwrap_or(556)) / 1000.0
        })
        .sum()
}

/// Chaîne littérale PDF échappée, texte encodé en WinAnsi.
fn literal(text: &str) -> String {
    let mut out = String::from("(");
    for c in text.chars() {
        let b = win_ansi(c);
        match b {
            b'(' | b')' | b'\\' => {
                out.push('\\');
                out.push(char::from(b));
            }
            0x20..=0x7E => out.push(char::from(b)),
            _ => {
                let _ = write!(out, "\\{b:03o}");
            }
        }
    }
    out.push(')');
    out
}

/// Nombre court (trois décimales, zéros inutiles retirés).
fn num(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Coupe un texte à `max` caractères, avec des points de suspension.
fn ellipsize(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let kept: String = flat.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}\u{2026}")
}

/// Découpe un texte pour qu'il tienne dans `width` points.
fn wrap(text: &str, size: f64, bold: bool, width: f64) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if text_width(&candidate, bold) * size <= width || line.is_empty() {
            line = candidate;
        } else {
            out.push(std::mem::take(&mut line));
            line = word.to_string();
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Une ligne du résumé, avant pagination.
struct Line {
    text: String,
    size: f64,
    bold: bool,
    color: [f64; 3],
    indent: f64,
    space_before: f64,
}

impl Line {
    fn new(text: impl Into<String>, size: f64, color: [f64; 3]) -> Self {
        Self {
            text: text.into(),
            size,
            bold: false,
            color,
            indent: 0.0,
            space_before: 0.0,
        }
    }

    fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    fn indent(mut self, indent: f64) -> Self {
        self.indent = indent;
        self
    }

    fn space(mut self, space: f64) -> Self {
        self.space_before = space;
        self
    }
}

/// Opérateurs de texte pour une ligne posée à (`x`, `y`).
fn show_text(out: &mut String, at: (f64, f64), size: f64, bold: bool, color: [f64; 3], text: &str) {
    let (x, y) = at;
    let font = if bold { "/F2" } else { "/F1" };
    let _ = writeln!(
        out,
        "BT {} {} {} rg {font} {} Tf {} {} Td {} Tj ET",
        num(color[0]),
        num(color[1]),
        num(color[2]),
        num(size),
        num(x),
        num(y),
        literal(text)
    );
}

/// Rectangle en pointillé, sans fond : sert aux zones de pixels, qui se
/// superposent souvent aux surlignages de texte et les rendraient illisibles.
fn outline(out: &mut String, rect: Rect, color: [f64; 3]) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let _ = writeln!(
        out,
        "q {} {} {} RG 0.8 w [2 2] 0 d {} {} {} {} re S Q",
        num(color[0]),
        num(color[1]),
        num(color[2]),
        num(rect.x0 - 1.0),
        num(rect.y0 - 1.0),
        num(rect.width() + 2.0),
        num(rect.height() + 2.0)
    );
}

/// Rectangle de surlignage : fond translucide et cadre plein.
fn highlight(out: &mut String, rect: Rect, color: [f64; 3]) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    // Un rectangle très fin (un mot d'une lettre) reste visible : minimum 1 pt.
    let r = Rect::new(
        rect.x0,
        rect.y0,
        rect.x1.max(rect.x0 + 1.0),
        rect.y1.max(rect.y0 + 1.0),
    );
    let _ = write!(
        out,
        "q /GS0 gs {r0} {r1} {r2} rg {x} {y} {w} {h} re f Q\n\
         q {r0} {r1} {r2} RG 0.7 w {x} {y} {w} {h} re S Q\n",
        r0 = num(color[0]),
        r1 = num(color[1]),
        r2 = num(color[2]),
        x = num(r.x0),
        y = num(r.y0),
        w = num(r.width()),
        h = num(r.height())
    );
}

/// Matrice qui place le contenu d'une page dans `target` : rotation
/// d'affichage appliquée, proportions conservées, résultat centré.
fn placement(crop: Rect, rotate: i32, target: Rect) -> Matrix {
    // Redressement : la boîte de rognage, tournée comme l'afficheur le ferait,
    // ramenée à l'origine.
    let (unrotate, width, height) = match rotate {
        90 => (
            Matrix::new(0.0, -1.0, 1.0, 0.0, -crop.y0, crop.x1),
            crop.height(),
            crop.width(),
        ),
        180 => (
            Matrix::new(-1.0, 0.0, 0.0, -1.0, crop.x1, crop.y1),
            crop.width(),
            crop.height(),
        ),
        270 => (
            Matrix::new(0.0, 1.0, -1.0, 0.0, crop.y1, -crop.x0),
            crop.height(),
            crop.width(),
        ),
        _ => (
            Matrix::translate(-crop.x0, -crop.y0),
            crop.width(),
            crop.height(),
        ),
    };
    if width <= 0.0 || height <= 0.0 {
        return unrotate;
    }
    let scale = (target.width() / width).min(target.height() / height);
    let (w, h) = (width * scale, height * scale);
    let tx = target.x0 + (target.width() - w) / 2.0;
    let ty = target.y0 + (target.height() - h) / 2.0;
    unrotate.then(&Matrix::new(scale, 0.0, 0.0, scale, tx, ty))
}

/// Document vierge qui accueillera le rapport.
///
/// Squelette minimal : le chargeur répare les offsets, la sauvegarde complète
/// réécrit tout proprement et `/Root` est remplacé par [`Builder::finish`].
fn blank_document() -> Result<Document> {
    let skeleton = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n0\n%%EOF\n";
    Document::from_bytes(skeleton.to_vec())
}

/// Constructeur du document de rapport.
///
/// Le document est emprunté et non possédé : les [`Importer`] qui copient les
/// pages sources le référencent aussi, et le constructeur doit rester
/// modifiable pendant ce temps.
struct Builder<'a> {
    doc: &'a Document,
    font: ObjectRef,
    font_bold: ObjectRef,
    gstate: ObjectRef,
    pages: Vec<(ObjectRef, Dict)>,
    options: &'a ReportOptions,
}

impl<'a> Builder<'a> {
    fn new(doc: &'a Document, options: &'a ReportOptions) -> Self {
        let simple_font = |base: &str| {
            let mut d = Dict::new();
            d.insert(Name::new("Type"), Object::Name(Name::new("Font")));
            d.insert(Name::new("Subtype"), Object::Name(Name::new("Type1")));
            d.insert(Name::new("BaseFont"), Object::Name(Name::new(base)));
            d.insert(
                Name::new("Encoding"),
                Object::Name(Name::new("WinAnsiEncoding")),
            );
            Object::Dict(d)
        };
        let font = doc.add(simple_font("Helvetica"));
        let font_bold = doc.add(simple_font("Helvetica-Bold"));
        // Opacité des surlignages : 0,28 laisse lire le texte au travers.
        let mut gs = Dict::new();
        gs.insert(Name::new("Type"), Object::Name(Name::new("ExtGState")));
        gs.insert(Name::new("ca"), Object::Real(0.28));
        let gstate = doc.add(Object::Dict(gs));
        Self {
            doc,
            font,
            font_bold,
            gstate,
            pages: Vec::new(),
            options,
        }
    }

    /// Ajoute une page au rapport.
    fn add_page(&mut self, content: &str, forms: &[(String, ObjectRef)]) {
        let mut fonts = Dict::new();
        fonts.insert(Name::new("F1"), Object::Reference(self.font));
        fonts.insert(Name::new("F2"), Object::Reference(self.font_bold));
        let mut ext = Dict::new();
        ext.insert(Name::new("GS0"), Object::Reference(self.gstate));
        let mut resources = Dict::new();
        resources.insert(Name::new("Font"), Object::Dict(fonts));
        resources.insert(Name::new("ExtGState"), Object::Dict(ext));
        if !forms.is_empty() {
            let mut xobjects = Dict::new();
            for (name, r) in forms {
                xobjects.insert(Name::new(name), Object::Reference(*r));
            }
            resources.insert(Name::new("XObject"), Object::Dict(xobjects));
        }
        let raw = content.as_bytes().to_vec();
        let mut stream = Dict::new();
        stream.insert(
            Name::new("Length"),
            Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
        );
        let contents = self.doc.add(Object::Stream { dict: stream, raw });
        let mut page = Dict::new();
        page.insert(Name::new("Type"), Object::Name(Name::new("Page")));
        page.insert(
            Name::new("MediaBox"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(self.options.page_width),
                Object::Real(self.options.page_height),
            ]),
        );
        page.insert(Name::new("Resources"), Object::Dict(resources));
        page.insert(Name::new("Contents"), Object::Reference(contents));
        let r = self.doc.allocate();
        self.pages.push((r, page));
    }

    /// Referme le document : arbre des pages, catalogue, écriture.
    fn finish(self) -> Result<Vec<u8>> {
        if self.pages.is_empty() {
            return Err(Error::Corrupt("rapport sans page".into()));
        }
        let root = self.doc.allocate();
        let mut kids = Vec::with_capacity(self.pages.len());
        for (r, mut dict) in self.pages {
            dict.insert(Name::new("Parent"), Object::Reference(root));
            self.doc.set(r, Object::Dict(dict));
            kids.push(Object::Reference(r));
        }
        let count = i64::try_from(kids.len()).unwrap_or(0);
        let mut tree = Dict::new();
        tree.insert(Name::new("Type"), Object::Name(Name::new("Pages")));
        tree.insert(Name::new("Kids"), Object::Array(kids));
        tree.insert(Name::new("Count"), Object::Integer(count));
        self.doc.set(root, Object::Dict(tree));
        let mut catalog = Dict::new();
        catalog.insert(Name::new("Type"), Object::Name(Name::new("Catalog")));
        catalog.insert(Name::new("Pages"), Object::Reference(root));
        let cat = self.doc.add(Object::Dict(catalog));
        self.doc.set_trailer_entry("Root", Object::Reference(cat));
        self.doc.save_full_with(&SaveOptions {
            compress_streams: true,
            drop_unreferenced: true,
            ..SaveOptions::default()
        })
    }
}

/// Copie une page source dans le rapport sous forme de XObject de formulaire.
fn page_form(dst: &Document, importer: &mut Importer, src: &Document, page: &Page) -> ObjectRef {
    let crop = page.crop_box(src);
    let mut dict = Dict::new();
    dict.insert(Name::new("Type"), Object::Name(Name::new("XObject")));
    dict.insert(Name::new("Subtype"), Object::Name(Name::new("Form")));
    dict.insert(
        Name::new("BBox"),
        Object::Array(vec![
            Object::Real(crop.x0),
            Object::Real(crop.y0),
            Object::Real(crop.x1),
            Object::Real(crop.y1),
        ]),
    );
    if let Some(resources) = page.dict.get(&Name::new("Resources")) {
        if let Ok(imported) = importer.import(resources) {
            dict.insert(Name::new("Resources"), imported);
        }
    }
    let raw = page_content(src, page);
    dict.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
    );
    dst.add(Object::Stream { dict, raw })
}

/// Page (1-based) lisible, ou « — » pour une page absente.
fn page_label(index: Option<usize>) -> String {
    index.map_or_else(|| "\u{2014}".to_string(), |i| format!("p. {}", i + 1))
}

/// Résumé d'une paire de pages en une ligne.
fn page_headline(diff: &PageDiff) -> String {
    let counts = crate::compare::count_differences(&diff.differences);
    let mut parts = Vec::new();
    if diff.pair.old_page.is_none() {
        parts.push("page ajoutée".to_string());
    }
    if diff.pair.new_page.is_none() {
        parts.push("page supprimée".to_string());
    }
    if diff.pair.moved {
        parts.push("page déplacée".to_string());
    }
    for (n, one, many) in [
        (counts.added, "ajout", "ajouts"),
        (counts.removed, "suppression", "suppressions"),
        (counts.replaced, "remplacement", "remplacements"),
        (counts.moved, "déplacement", "déplacements"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {}", if n > 1 { many } else { one }));
        }
    }
    if let Some(v) = &diff.visual {
        if v.changed_ratio > 0.0 {
            parts.push(format!("{:.2} % de pixels", v.changed_ratio * 100.0));
        }
    }
    if parts.is_empty() {
        "identique".to_string()
    } else {
        parts.join(", ")
    }
}

/// Extrait lisible d'une différence, avec son signe.
fn extract(d: &Difference) -> (String, [f64; 3]) {
    match d.kind {
        DiffKind::Added => (format!("+ {}", ellipsize(&d.new_text, 110)), ADDED),
        DiffKind::Removed => (format!("- {}", ellipsize(&d.old_text, 110)), REMOVED),
        DiffKind::Replaced => (
            format!(
                "~ {} -> {}",
                ellipsize(&d.old_text, 52),
                ellipsize(&d.new_text, 52)
            ),
            MOVED,
        ),
        DiffKind::Moved => (
            format!(
                "<-> {} ({} -> {})",
                ellipsize(&d.new_text, 70),
                page_label(d.old_page),
                page_label(d.new_page)
            ),
            MOVED,
        ),
    }
}

/// Lignes du résumé.
#[allow(clippy::too_many_lines)] // liste de libelles : la decouper n'apporterait rien
fn summary_lines(comparison: &Comparison, options: &ReportOptions) -> Vec<Line> {
    let counts = comparison.counts();
    let mut lines = vec![
        Line::new("Rapport de comparaison", 20.0, INK).bold(),
        Line::new(
            format!(
                "Avant : {} ({} page{})",
                options.old_title,
                comparison.old_page_count,
                if comparison.old_page_count > 1 {
                    "s"
                } else {
                    ""
                }
            ),
            10.0,
            MUTED,
        )
        .space(10.0),
        Line::new(
            format!(
                "Après : {} ({} page{})",
                options.new_title,
                comparison.new_page_count,
                if comparison.new_page_count > 1 {
                    "s"
                } else {
                    ""
                }
            ),
            10.0,
            MUTED,
        ),
    ];
    if comparison.is_identical() {
        lines.push(
            Line::new("Aucune différence.", 13.0, INK)
                .bold()
                .space(18.0),
        );
        return lines;
    }
    lines.push(
        Line::new(
            format!(
                "{} différence{} de texte : {} ajout(s), {} suppression(s), {} remplacement(s), {} déplacement(s)",
                counts.total(),
                if counts.total() > 1 { "s" } else { "" },
                counts.added,
                counts.removed,
                counts.replaced,
                counts.moved
            ),
            12.0,
            INK,
        )
        .bold()
        .space(18.0),
    );
    if counts.pages_added + counts.pages_removed + counts.pages_moved > 0 {
        lines.push(
            Line::new(
                format!(
                    "Pages : {} ajoutée(s), {} supprimée(s), {} déplacée(s)",
                    counts.pages_added, counts.pages_removed, counts.pages_moved
                ),
                11.0,
                INK,
            )
            .space(4.0),
        );
    }
    lines.push(Line::new("Détail par page", 12.0, INK).bold().space(16.0));
    for diff in &comparison.pages {
        if diff.is_identical() {
            continue;
        }
        lines.push(
            Line::new(
                format!(
                    "{} -> {} : {}",
                    page_label(diff.pair.old_page),
                    page_label(diff.pair.new_page),
                    page_headline(diff)
                ),
                10.0,
                INK,
            )
            .bold()
            .space(8.0),
        );
        for d in diff.differences.iter().take(options.extracts_per_page) {
            let (text, color) = extract(d);
            lines.push(Line::new(text, 9.0, color).indent(14.0).space(2.0));
        }
        if diff.differences.len() > options.extracts_per_page {
            lines.push(
                Line::new(
                    format!(
                        "\u{2026} et {} autre(s)",
                        diff.differences.len() - options.extracts_per_page
                    ),
                    9.0,
                    MUTED,
                )
                .indent(14.0)
                .space(2.0),
            );
        }
    }
    lines
}

/// Écrit les pages de résumé (autant que nécessaire).
fn write_summary(builder: &mut Builder, comparison: &Comparison) {
    let options = builder.options;
    let (width, height) = (options.page_width, options.page_height);
    let available = width - 2.0 * MARGIN;
    let mut content = String::new();
    let mut y = height - MARGIN - 18.0;
    for line in summary_lines(comparison, options) {
        for (i, part) in wrap(&line.text, line.size, line.bold, available - line.indent)
            .into_iter()
            .enumerate()
        {
            let step = line.size * 1.32 + if i == 0 { line.space_before } else { 0.0 };
            if y - step < MARGIN + 12.0 {
                builder.add_page(&content, &[]);
                content = String::new();
                y = height - MARGIN - 18.0;
            }
            y -= step;
            show_text(
                &mut content,
                (MARGIN + line.indent, y),
                line.size,
                line.bold,
                line.color,
                &part,
            );
        }
    }
    builder.add_page(&content, &[]);
}

/// Légende du bas de page.
fn write_legend(content: &mut String, width: f64) {
    let mut x = MARGIN;
    for (color, label) in [
        (ADDED, "ajout"),
        (REMOVED, "suppression"),
        (MOVED, "déplacement ou remplacement"),
        (PIXELS, "pixels différents"),
    ] {
        let box_rect = Rect::new(x, 20.0, x + 8.0, 28.0);
        if color == PIXELS {
            outline(content, box_rect, color);
        } else {
            highlight(content, box_rect, color);
        }
        show_text(content, (x + 12.0, 21.0), 8.0, false, MUTED, label);
        x += 12.0 + text_width(label, false) * 8.0 + 16.0;
        if x > width - MARGIN {
            break;
        }
    }
}

/// Écrit une planche : la page d'avant à gauche, celle d'après à droite.
#[allow(clippy::too_many_lines)] // une planche s'ecrit de haut en bas ; la decouper la rendrait moins lisible
fn write_pair<'a>(
    builder: &mut Builder,
    diff: &PageDiff,
    old: (&Document, &[Page]),
    new: (&Document, &[Page]),
    old_importer: &mut Importer<'a>,
    new_importer: &mut Importer<'a>,
) {
    let (width, height) = (builder.options.page_width, builder.options.page_height);
    let panel_width = (width - 3.0 * MARGIN) / 2.0;
    let (panel_bottom, panel_top) = (MARGIN + 22.0, height - MARGIN - 44.0);
    let left = Rect::new(MARGIN, panel_bottom, MARGIN + panel_width, panel_top);
    let right = Rect::new(
        width - MARGIN - panel_width,
        panel_bottom,
        width - MARGIN,
        panel_top,
    );
    let mut content = String::new();
    show_text(
        &mut content,
        (MARGIN, height - MARGIN - 12.0),
        14.0,
        true,
        INK,
        &format!(
            "{} -> {}",
            page_label(diff.pair.old_page),
            page_label(diff.pair.new_page)
        ),
    );
    show_text(
        &mut content,
        (MARGIN + 110.0, height - MARGIN - 12.0),
        10.0,
        false,
        MUTED,
        &page_headline(diff),
    );
    let mut forms = Vec::new();
    for (side, panel, index, docs, label) in [
        (
            0usize,
            left,
            diff.pair.old_page,
            old,
            builder.options.old_title.clone(),
        ),
        (
            1usize,
            right,
            diff.pair.new_page,
            new,
            builder.options.new_title.clone(),
        ),
    ] {
        show_text(
            &mut content,
            (panel.x0, panel_top + 6.0),
            9.0,
            false,
            MUTED,
            &format!("{label} \u{2014} {}", page_label(index)),
        );
        let _ = writeln!(
            content,
            "q 1 1 1 rg {x} {y} {w} {h} re f 0.75 0.75 0.75 RG 0.5 w {x} {y} {w} {h} re S Q",
            x = num(panel.x0),
            y = num(panel.y0),
            w = num(panel.width()),
            h = num(panel.height())
        );
        let Some(page) = index.and_then(|i| docs.1.get(i)) else {
            show_text(
                &mut content,
                (panel.x0 + 12.0, panel.y0 + panel.height() / 2.0),
                11.0,
                false,
                MUTED,
                "page absente de ce document",
            );
            continue;
        };
        let name = format!("Fm{side}");
        let importer: &mut Importer<'a> = if side == 0 {
            &mut *old_importer
        } else {
            &mut *new_importer
        };
        let form = page_form(builder.doc, importer, docs.0, page);
        forms.push((name.clone(), form));
        let target = Rect::new(
            panel.x0 + 5.0,
            panel.y0 + 5.0,
            panel.x1 - 5.0,
            panel.y1 - 5.0,
        );
        let matrix = placement(page.crop_box(docs.0), page.rotate(docs.0), target);
        let _ = writeln!(
            content,
            "q {x} {y} {w} {h} re W n {a} {b} {c} {d} {e} {f} cm /{name} Do Q",
            x = num(panel.x0),
            y = num(panel.y0),
            w = num(panel.width()),
            h = num(panel.height()),
            a = num(matrix.a),
            b = num(matrix.b),
            c = num(matrix.c),
            d = num(matrix.d),
            e = num(matrix.e),
            f = num(matrix.f)
        );
        // Surlignages : rectangles de la page, amenés dans le repère du rapport.
        for d in &diff.differences {
            let (rects, color) = match (side, d.kind) {
                (0, DiffKind::Added) | (1, DiffKind::Removed) => (&[][..], INK),
                (0, _) => (&d.old_rects[..], diff_color(d.kind, true)),
                (_, _) => (&d.new_rects[..], diff_color(d.kind, false)),
            };
            for r in rects {
                highlight(&mut content, matrix.transform_rect(r), color);
            }
        }
        if let Some(v) = &diff.visual {
            // Les zones visuelles sont données dans le repère de la page de
            // droite ; à gauche elles ne sont dessinées que si la page manque
            // à droite (page supprimée).
            let draw = if side == 0 {
                diff.pair.new_page.is_none()
            } else {
                true
            };
            if draw {
                for r in &v.rects {
                    outline(&mut content, matrix.transform_rect(r), PIXELS);
                }
            }
        }
    }
    write_legend(&mut content, width);
    builder.add_page(&content, &forms);
}

/// Couleur d'une différence selon le côté où elle est dessinée.
fn diff_color(kind: DiffKind, old_side: bool) -> [f64; 3] {
    match kind {
        DiffKind::Added => ADDED,
        DiffKind::Removed => REMOVED,
        DiffKind::Moved => MOVED,
        // Un remplacement se lit comme une suppression à gauche et un ajout à droite.
        DiffKind::Replaced => {
            if old_side {
                REMOVED
            } else {
                ADDED
            }
        }
    }
}

/// Construit le PDF de comparaison : page de résumé, puis une planche par
/// paire de pages avec les deux documents côte à côte et les différences
/// surlignées.
///
/// # Errors
/// Documents illisibles, ou comparaison sans aucune page.
pub fn write_report(
    old: &Document,
    new: &Document,
    comparison: &Comparison,
    options: &ReportOptions,
) -> Result<Vec<u8>> {
    let old_pages = collect_pages(old)?;
    let new_pages = collect_pages(new)?;
    let doc = blank_document()?;
    let mut builder = Builder::new(&doc, options);
    write_summary(&mut builder, comparison);
    if options.include_pages {
        // Un importeur par document source : les ressources communes à
        // plusieurs pages (polices, images) ne sont copiées qu'une fois.
        let mut old_importer = Importer::new(old, &doc);
        let mut new_importer = Importer::new(new, &doc);
        for diff in &comparison.pages {
            if options.changed_pages_only && diff.is_identical() {
                continue;
            }
            write_pair(
                &mut builder,
                diff,
                (old, &old_pages),
                (new, &new_pages),
                &mut old_importer,
                &mut new_importer,
            );
        }
    }
    builder.finish()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::float_cmp)]
mod tests {
    use super::*;
    use acrux_core::Point;

    #[test]
    fn literals_escape_and_encode() {
        assert_eq!(literal("a(b)c\\"), "(a\\(b\\)c\\\\)");
        assert_eq!(literal("été"), "(\\351t\\351)");
        assert_eq!(literal("\u{2192}"), "(?)");
    }

    #[test]
    fn wrapping_respects_the_width() {
        let lines = wrap("un deux trois quatre cinq six", 10.0, false, 60.0);
        assert!(lines.len() > 1, "{lines:?}");
        for l in &lines {
            assert!(
                text_width(l, false) * 10.0 <= 60.0 || !l.contains(' '),
                "{l:?}"
            );
        }
        assert_eq!(wrap("", 10.0, false, 60.0), vec![String::new()]);
    }

    #[test]
    fn ellipsize_keeps_short_texts() {
        assert_eq!(ellipsize("un  deux\ntrois", 40), "un deux trois");
        assert_eq!(ellipsize("abcdefghij", 5), "abcd\u{2026}");
    }

    #[test]
    fn placement_fits_and_centres_a_page() {
        let crop = Rect::new(0.0, 0.0, 200.0, 400.0);
        let target = Rect::new(0.0, 0.0, 200.0, 200.0);
        let m = placement(crop, 0, target);
        let placed = m.transform_rect(&crop);
        assert_eq!(placed.height(), 200.0);
        assert_eq!(placed.width(), 100.0);
        assert_eq!(placed.x0, 50.0, "centré horizontalement");
    }

    #[test]
    fn placement_applies_the_display_rotation() {
        // Page 200 × 400 tournée de 90° : elle s'affiche en 400 × 200.
        let crop = Rect::new(0.0, 0.0, 200.0, 400.0);
        let target = Rect::new(0.0, 0.0, 400.0, 400.0);
        let m = placement(crop, 90, target);
        let placed = m.transform_rect(&crop);
        assert_eq!((placed.width(), placed.height()), (400.0, 200.0));
        // Le coin haut-gauche d'origine part à droite (rotation horaire).
        let corner = m.apply(Point::new(0.0, 400.0));
        assert_eq!((corner.x, corner.y), (400.0, 300.0));
    }
}
