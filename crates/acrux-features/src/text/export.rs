//! Sorties d'une page analysée : texte brut, Markdown, HTML et texte
//! positionné (à la `pdftotext -layout`).

// Positions en colonnes de caractères : petits entiers.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::fmt::Write as _;

use super::{median, BlockKind, PageText, Paragraph, Table};

/// Suite de mots de même style dans un paragraphe.
struct Run {
    bold: bool,
    italic: bool,
    text: String,
    space_before: bool,
}

/// Ajoute un morceau de texte à la dernière suite si le style est le même,
/// sinon ouvre une nouvelle suite ; `glue` : pas d'espace avant.
fn push_run(runs: &mut Vec<Run>, bold: bool, italic: bool, text: &str, glue: bool) {
    match runs.last_mut() {
        Some(r) if r.bold == bold && r.italic == italic => {
            if !glue {
                r.text.push(' ');
            }
            r.text.push_str(text);
        }
        _ => {
            let space_before = !glue && !runs.is_empty();
            runs.push(Run {
                bold,
                italic,
                text: text.to_string(),
                space_before,
            });
        }
    }
}

/// Marqueur de liste non numéroté.
fn is_bullet(marker: &str) -> bool {
    marker.chars().count() == 1 && !marker.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Numéro d'un marqueur « 3. » / « 3) » / « (3) ».
fn marker_number(marker: &str) -> Option<u32> {
    marker
        .trim_matches(|c: char| matches!(c, '.' | ')' | '('))
        .parse()
        .ok()
}

impl PageText {
    /// Suites de glyphes de même style (gras / italique) d'un paragraphe,
    /// césures réparées. Le style est suivi au glyphe près : « l'*italique* »
    /// ou « **gras**, » ne mettent en relief que la partie concernée.
    fn runs(&self, p: &Paragraph) -> Vec<Run> {
        let mut runs: Vec<Run> = Vec::new();
        for (li, &line_idx) in p.lines.iter().enumerate() {
            let Some(line) = self.lines.get(line_idx) else {
                continue;
            };
            let skip = usize::from(
                li == 0
                    && line.words.len() >= 2
                    && p.marker.as_deref() == Some(line.words[0].text.as_str()),
            );
            for (wi, w) in line.words.iter().enumerate().skip(skip) {
                // Jonction de lignes : césure à réparer ?
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
                    push_run(&mut runs, w.style.bold, w.style.italic, &w.text, glue);
                    continue;
                }
                for (gi, g) in w.glyphs.iter().enumerate() {
                    push_run(&mut runs, g.bold, g.italic, &g.text, glue || gi > 0);
                }
            }
        }
        runs
    }

    /// Texte brut : un paragraphe par ligne (lignes fusionnées, césures
    /// réparées), une ligne vide entre paragraphes, cellules de tableau
    /// séparées par des tabulations. Sans analyse (`blocks` vide), une ligne
    /// géométrique par ligne.
    #[must_use]
    pub fn to_plain(&self) -> String {
        if self.blocks.is_empty() {
            let mut out = String::new();
            for l in &self.lines {
                out.push_str(&l.text());
                out.push('\n');
            }
            return out;
        }
        // (texte, élément de liste)
        let mut chunks: Vec<(String, bool)> = Vec::new();
        for b in &self.blocks {
            match b.kind {
                BlockKind::Table(i) => {
                    if let Some(t) = self.tables.get(i) {
                        let rows: Vec<String> = t
                            .rows
                            .iter()
                            .map(|r| {
                                r.iter()
                                    .map(|c| c.text.as_str())
                                    .collect::<Vec<_>>()
                                    .join("\t")
                            })
                            .collect();
                        chunks.push((rows.join("\n"), false));
                    }
                }
                _ => {
                    for p in &b.paragraphs {
                        match &p.marker {
                            Some(m) => chunks.push((format!("{m} {}", p.text), true)),
                            None => chunks.push((p.text.clone(), false)),
                        }
                    }
                }
            }
        }
        let mut out = String::new();
        for (i, (text, list)) in chunks.iter().enumerate() {
            if i > 0 {
                out.push('\n');
                if !(*list && chunks[i - 1].1) {
                    out.push('\n');
                }
            }
            out.push_str(text);
        }
        out.push('\n');
        out
    }

    /// Markdown : titres (`#`, `##`, `###`), gras / italique, listes à puces et
    /// numérotées, tableaux.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        let mut prev_list = false;
        for b in &self.blocks {
            match b.kind {
                BlockKind::Table(i) => {
                    if let Some(t) = self.tables.get(i) {
                        if !out.is_empty() {
                            out.push('\n');
                        }
                        markdown_table(&mut out, t);
                        prev_list = false;
                    }
                }
                _ => {
                    for p in &b.paragraphs {
                        let list = p.marker.is_some();
                        if !(out.is_empty() || list && prev_list) {
                            out.push('\n');
                        }
                        if p.heading > 0 {
                            let runs = self.runs(p);
                            let text: Vec<String> = runs
                                .iter()
                                .map(|r| {
                                    format!(
                                        "{}{}",
                                        if r.space_before { " " } else { "" },
                                        escape_md(&r.text)
                                    )
                                })
                                .collect();
                            let _ = writeln!(
                                out,
                                "{} {}",
                                "#".repeat(usize::from(p.heading)),
                                text.concat()
                            );
                        } else {
                            if let Some(m) = &p.marker {
                                if is_bullet(m) {
                                    out.push_str("- ");
                                } else if marker_number(m).is_some() {
                                    out.push_str(m);
                                    out.push(' ');
                                } else {
                                    let _ = write!(out, "- {m} ");
                                }
                            } else {
                                // Évite qu'un paragraphe ordinaire soit lu comme un titre ou une liste.
                                let first = p.text.chars().next();
                                if matches!(first, Some('#' | '>' | '+' | '-' | '*'))
                                    || p.text.split_whitespace().next().is_some_and(|w| {
                                        w.len() >= 2
                                            && w.ends_with('.')
                                            && w[..w.len() - 1].chars().all(|c| c.is_ascii_digit())
                                    })
                                {
                                    out.push('\\');
                                }
                            }
                            self.markdown_inline(&mut out, p);
                            out.push('\n');
                        }
                        prev_list = list;
                    }
                }
            }
        }
        out
    }

    fn markdown_inline(&self, out: &mut String, p: &Paragraph) {
        for r in self.runs(p) {
            if r.space_before {
                out.push(' ');
            }
            let text = escape_md(&r.text);
            let wrap = match (r.bold, r.italic) {
                (true, true) => "***",
                (true, false) => "**",
                (false, true) => "*",
                (false, false) => "",
            };
            // Les délimiteurs ne doivent pas jouxter une espace : on les pose autour du texte élagué.
            let trimmed = text.trim();
            if trimmed.is_empty() {
                out.push_str(&text);
            } else {
                let lead = text.len() - text.trim_start().len();
                let trail = text.len() - text.trim_end().len();
                out.push_str(&text[..lead]);
                out.push_str(wrap);
                out.push_str(trimmed);
                out.push_str(wrap);
                out.push_str(&text[text.len() - trail..]);
            }
        }
    }

    /// Fragment HTML (à insérer dans un `<body>`) : `<h1>`–`<h3>`, `<p>`, `<ul>`/`<ol>`,
    /// `<table>`, `<b>`/`<i>`, `<header>`/`<footer>` ; entités encodées.
    #[must_use]
    pub fn to_html(&self) -> String {
        let mut out = String::new();
        for b in &self.blocks {
            match b.kind {
                BlockKind::Table(i) => {
                    if let Some(t) = self.tables.get(i) {
                        html_table(&mut out, t);
                    }
                }
                kind => {
                    let wrapper = match kind {
                        BlockKind::Header => Some("header"),
                        BlockKind::Footer => Some("footer"),
                        _ => None,
                    };
                    if let Some(w) = wrapper {
                        let _ = writeln!(out, "<{w}>");
                    }
                    self.html_paragraphs(&mut out, &b.paragraphs);
                    if let Some(w) = wrapper {
                        let _ = writeln!(out, "</{w}>");
                    }
                }
            }
        }
        out
    }

    fn html_paragraphs(&self, out: &mut String, paragraphs: &[Paragraph]) {
        // Liste ouverte : `ul` ou `ol`.
        let mut open: Option<&str> = None;
        for p in paragraphs {
            let list_tag = p.marker.as_deref().map(|m| {
                if is_bullet(m) || marker_number(m).is_none() {
                    "ul"
                } else {
                    "ol"
                }
            });
            if open != list_tag {
                if let Some(tag) = open {
                    let _ = writeln!(out, "</{tag}>");
                }
                if let Some(tag) = list_tag {
                    let start = p
                        .marker
                        .as_deref()
                        .and_then(marker_number)
                        .filter(|n| *n != 1 && tag == "ol");
                    match start {
                        Some(n) => {
                            let _ = writeln!(out, "<{tag} start=\"{n}\">");
                        }
                        None => {
                            let _ = writeln!(out, "<{tag}>");
                        }
                    }
                }
                open = list_tag;
            }
            let tag = if p.heading > 0 {
                format!("h{}", p.heading)
            } else if list_tag.is_some() {
                "li".to_string()
            } else {
                "p".to_string()
            };
            let _ = write!(out, "<{tag}>");
            for r in self.runs(p) {
                if r.space_before {
                    out.push(' ');
                }
                let text = escape_html(&r.text);
                match (r.bold && p.heading == 0, r.italic) {
                    (true, true) => {
                        let _ = write!(out, "<b><i>{text}</i></b>");
                    }
                    (true, false) => {
                        let _ = write!(out, "<b>{text}</b>");
                    }
                    (false, true) => {
                        let _ = write!(out, "<i>{text}</i>");
                    }
                    (false, false) => out.push_str(&text),
                }
            }
            let _ = writeln!(out, "</{tag}>");
        }
        if let Some(tag) = open {
            let _ = writeln!(out, "</{tag}>");
        }
    }

    /// Texte positionné : chaque ligne géométrique sur une ligne de sortie,
    /// les mots placés à la colonne correspondant à leur abscisse (une
    /// colonne ≈ une largeur médiane de glyphe), des lignes vides pour les
    /// grands écarts verticaux. Les colonnes d'un tableau ou d'une mise en
    /// page à deux colonnes restent donc alignées.
    #[must_use]
    pub fn to_layout(&self) -> String {
        struct W<'a> {
            x0: f64,
            x1: f64,
            size: f64,
            text: &'a str,
        }
        let mut glyph_widths: Vec<f64> = Vec::new();
        let mut sizes: Vec<f64> = Vec::new();
        // Rangées : (ligne de base, taille, mots).
        let mut rows: Vec<(f64, f64, Vec<W<'_>>)> = Vec::new();
        for line in &self.lines {
            let base = line.baseline();
            let size = line.size();
            for w in &line.words {
                for g in &w.glyphs {
                    let gw = g.along_end - g.along;
                    if gw > 0.0 {
                        glyph_widths.push(gw);
                    }
                    sizes.push(g.size);
                }
                let entry = W {
                    x0: w.bbox.x0,
                    x1: w.bbox.x1,
                    size: if w.style.size > 0.0 {
                        w.style.size
                    } else {
                        size
                    },
                    text: &w.text,
                };
                match rows
                    .iter_mut()
                    .find(|(b, s, _)| (*b - base).abs() <= 0.5 * s.max(size))
                {
                    Some((_, _, ws)) => ws.push(entry),
                    None => rows.push((base, size, vec![entry])),
                }
            }
        }
        if rows.is_empty() {
            return String::new();
        }
        let med = median(&mut sizes).unwrap_or(10.0).max(1.0);
        let cw = median(&mut glyph_widths).unwrap_or(0.5 * med).max(0.1);
        rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let min_x = rows
            .iter()
            .flat_map(|(_, _, ws)| ws.iter().map(|w| w.x0))
            .fold(f64::MAX, f64::min);
        let mut diffs: Vec<f64> = rows
            .windows(2)
            .map(|w| w[0].0 - w[1].0)
            .filter(|d| *d >= 0.5 * med && *d <= 3.0 * med)
            .collect();
        let pitch = median(&mut diffs).unwrap_or(1.2 * med).max(0.8 * med);
        let mut out = String::new();
        let mut prev_base: Option<f64> = None;
        for (base, _, mut ws) in rows {
            if let Some(pb) = prev_base {
                let gap = pb - base;
                if gap > 1.7 * pitch {
                    let extra = ((gap / pitch).round() as usize).saturating_sub(1).min(5);
                    for _ in 0..extra {
                        out.push('\n');
                    }
                }
            }
            ws.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal));
            let mut line = String::new();
            let mut len = 0usize;
            let mut prev_end: Option<f64> = None;
            for w in ws {
                let absolute = ((w.x0 - min_x) / cw).round().max(0.0) as usize;
                let target = match prev_end {
                    None => absolute,
                    Some(end) => {
                        let gap = w.x0 - end;
                        if gap > 2.0 * w.size {
                            // Grand blanc (colonne, cellule) : position absolue, alignée entre rangées.
                            absolute.max(len + 1)
                        } else {
                            // Espace ordinaire : proportionnelle à la taille du texte lui-même.
                            len + ((gap / (0.5 * w.size)).round().max(1.0) as usize)
                        }
                    }
                };
                for _ in len..target {
                    line.push(' ');
                }
                line.push_str(w.text);
                len = target + w.text.chars().count();
                prev_end = Some(w.x1);
            }
            out.push_str(&line);
            out.push('\n');
            prev_base = Some(base);
        }
        out
    }
}

fn markdown_table(out: &mut String, t: &Table) {
    let cols = t.rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return;
    }
    for (ri, row) in t.rows.iter().enumerate() {
        out.push('|');
        for c in 0..cols {
            let text = row.get(c).map_or("", |c| c.text.as_str());
            let _ = write!(
                out,
                " {} |",
                escape_md(text).replace('|', "\\|").replace('\n', " ")
            );
        }
        out.push('\n');
        if ri == 0 {
            out.push('|');
            for _ in 0..cols {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
    }
}

fn html_table(out: &mut String, t: &Table) {
    out.push_str("<table>\n");
    for (ri, row) in t.rows.iter().enumerate() {
        out.push_str("<tr>");
        let tag = if ri == 0 { "th" } else { "td" };
        for c in row {
            let _ = write!(out, "<{tag}>{}</{tag}>", escape_html(&c.text));
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</table>\n");
}

/// Échappe les caractères de mise en forme Markdown les plus courants.
fn escape_md(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '*' | '_' | '`' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Encode les entités HTML.
fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Fonction partagée avec `layout` (réexport pour les tests).
#[cfg(test)]
pub(super) fn join(a: &str, b: &str) -> String {
    let mut s = a.to_string();
    super::layout::join_lines(&mut s, b);
    s
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::super::test_support::page;
    use super::*;

    #[test]
    fn hyphen_repair() {
        assert_eq!(join("docu-", "mentation"), "documentation");
        assert_eq!(join("Jean-", "Pierre"), "Jean- Pierre");
        assert_eq!(join("mot\u{ad}", "Suite"), "motSuite");
        assert_eq!(join("fin.", "Debut"), "fin. Debut");
    }

    #[test]
    fn markdown_and_html_styles_and_escapes() {
        let mut p = page(&[(50.0, 700.0, 12.0, "Texte *gras* et a_b <ok>")]);
        p.analyze();
        assert_eq!(p.to_markdown(), "Texte **gras** et a\\_b <ok>\n");
        assert_eq!(p.to_html(), "<p>Texte <b>gras</b> et a_b &lt;ok&gt;</p>\n");
    }

    #[test]
    fn style_runs_follow_glyphs_inside_words() {
        use super::super::{lines_from_glyphs, test_support::line_glyphs};
        // « gras » gras suivi d'une virgule normale dans le même mot ; « l' » normal puis « italique » italique.
        let mut glyphs = line_glyphs(50.0, 700.0, 12.0, "du *gras*, de l'italique");
        // Marque italique les glyphes de « italique » (les huit derniers).
        let n = glyphs.len();
        for g in &mut glyphs[n - 8..] {
            g.italic = true;
        }
        let mut p = lines_from_glyphs(glyphs);
        p.analyze();
        assert_eq!(p.to_markdown(), "du **gras**, de l'*italique*\n");
        assert_eq!(p.to_html(), "<p>du <b>gras</b>, de l'<i>italique</i></p>\n");
    }

    #[test]
    fn layout_keeps_columns_aligned() {
        let mut p = page(&[
            (50.0, 700.0, 10.0, "Gauche un"),
            (300.0, 700.0, 10.0, "Droite un"),
            (50.0, 688.0, 10.0, "Gauche deux"),
            (300.0, 688.0, 10.0, "Droite deux"),
            (50.0, 600.0, 10.0, "Bas"),
        ]);
        p.analyze();
        let layout = p.to_layout();
        let lines: Vec<&str> = layout.lines().collect();
        assert!(lines[0].starts_with("Gauche un") && lines[1].starts_with("Gauche deux"));
        assert_eq!(lines[0].find("Droite"), lines[1].find("Droite"), "{layout}");
        assert!(lines[2].is_empty(), "grand écart → ligne vide : {layout}");
        assert!(lines.iter().any(|l| l.starts_with("Bas")));
    }

    #[test]
    fn plain_without_analysis_lists_lines() {
        let p = page(&[(50.0, 700.0, 10.0, "a b"), (50.0, 688.0, 10.0, "c")]);
        assert_eq!(p.to_plain(), "a b\nc\n");
    }
}
