//! Création d'un document à partir de Markdown, avec notre propre analyseur.
//!
//! # Ce qui est couvert
//!
//! | Élément | Syntaxe reconnue |
//! |---------|------------------|
//! | titres | `#` à `######` en début de ligne (ATX), corps décroissant |
//! | gras | `**texte**` et `__texte__` |
//! | italique | `*texte*` et `_texte_` |
//! | code incorporé | `` `texte` `` — chasse fixe, sur fond gris |
//! | bloc de code | clôture par ``` ``` ``` ou `~~~`, ou indentation de quatre espaces |
//! | listes à puces | `-`, `*` ou `+` ; trois niveaux d'imbrication (deux espaces par niveau) |
//! | listes numérotées | `1.`, `2)` ; la numérotation d'origine est respectée |
//! | citations | `>` en début de ligne, filet vertical et italique |
//! | règles horizontales | `---`, `***`, `___` (trois signes ou plus) |
//! | tableaux | tableaux à barres verticales avec leur ligne de séparation `|---|---|` |
//! | liens | `[texte](adresse)` — annotation `/Link` cliquable, texte souligné |
//! | échappement | `\*`, `\_`, `` \` ``, `\[`, `\\` |
//!
//! # Ce qui n'est pas couvert
//!
//! Images, HTML brut, liens de référence (`[a][b]`), notes de bas de page,
//! listes de définitions, titres soulignés (Setext), cases à cocher,
//! citations imbriquées, et les styles à l'intérieur des cellules d'un
//! tableau (leurs marques sont retirées, le texte reste). L'alignement
//! déclaré dans la ligne de séparation d'un tableau (`:---:`) est lu mais
//! seules la gauche et la droite sont appliquées.

use acrux_core::{Rect, Result};
use acrux_document::Document;

use super::builder::{FontRef, Pen};
use super::text::{Flow, TextAlign, TextLayout};
use super::wrap::{wrap, wrap_hard};
use crate::fontembed::FontStyle;
use crate::stamp::metrics::StandardFont;
use crate::stamp::Rgb;

/// Facteurs de corps des six niveaux de titre, du plus grand au plus petit.
const HEADING_SCALE: [f64; 6] = [1.9, 1.55, 1.3, 1.15, 1.05, 1.0];

/// Gris du fond des blocs de code.
const CODE_BACKGROUND: Rgb = [0.955, 0.955, 0.945];
/// Gris des filets (règles, tableaux, citations).
const RULE: Rgb = [0.72, 0.72, 0.72];
/// Gris du texte des citations.
const QUOTE_INK: Rgb = [0.32, 0.32, 0.32];
/// Bleu des liens.
const LINK_INK: Rgb = [0.05, 0.28, 0.65];
/// Retrait d'un niveau de liste, en points.
const INDENT: f64 = 16.0;

/// Style d'un fragment de texte au fil du Markdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Inline {
    bold: bool,
    italic: bool,
    code: bool,
    /// Index du lien dans la table des adresses.
    link: Option<usize>,
}

/// Fragment de texte homogène.
#[derive(Debug, Clone)]
struct Span {
    text: String,
    style: Inline,
}

/// Bloc de niveau supérieur.
#[derive(Debug, Clone)]
enum Block {
    /// Titre de niveau 1 à 6.
    Heading(u8, Vec<Span>),
    /// Paragraphe ordinaire.
    Paragraph(Vec<Span>),
    /// Élément de liste : niveau d'imbrication, puce ou numéro, contenu.
    Item(usize, String, Vec<Span>),
    /// Citation.
    Quote(Vec<Span>),
    /// Bloc de code, ligne par ligne.
    Code(Vec<String>),
    /// Règle horizontale.
    Rule,
    /// Tableau : en-tête, lignes, alignement par colonne.
    Table(Vec<String>, Vec<Vec<String>>, Vec<TextAlign>),
    /// Ligne vide entre deux blocs.
    Gap,
}

/// Document analysé : ses blocs et la table des adresses des liens.
struct Parsed {
    blocks: Vec<Block>,
    links: Vec<String>,
}

// --- Analyse ---------------------------------------------------------------

/// Analyse le Markdown en blocs.
#[allow(clippy::too_many_lines)] // un bloc `if` par syntaxe reconnue, à la suite
fn parse(source: &str) -> Parsed {
    let mut links: Vec<String> = Vec::new();
    let mut blocks: Vec<Block> = Vec::new();
    let rows: Vec<&str> = source
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let line = rows[i];
        let trimmed = line.trim();
        // Un paragraphe en cours se referme dès qu'autre chose commence.
        let flush =
            |paragraph: &mut Vec<&str>, blocks: &mut Vec<Block>, links: &mut Vec<String>| {
                if !paragraph.is_empty() {
                    let joined = paragraph.join(" ");
                    blocks.push(Block::Paragraph(parse_inline(&joined, links)));
                    paragraph.clear();
                }
            };
        if let Some(fence) = fence_of(trimmed) {
            flush(&mut paragraph, &mut blocks, &mut links);
            let mut body = Vec::new();
            i += 1;
            while i < rows.len() && fence_of(rows[i].trim()) != Some(fence) {
                body.push(rows[i].to_string());
                i += 1;
            }
            i += 1; // la clôture
            blocks.push(Block::Code(body));
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut paragraph, &mut blocks, &mut links);
            if !matches!(blocks.last(), None | Some(Block::Gap)) {
                blocks.push(Block::Gap);
            }
            i += 1;
            continue;
        }
        if is_rule(trimmed) {
            flush(&mut paragraph, &mut blocks, &mut links);
            blocks.push(Block::Rule);
            i += 1;
            continue;
        }
        if let Some((level, rest)) = heading_of(trimmed) {
            flush(&mut paragraph, &mut blocks, &mut links);
            blocks.push(Block::Heading(level, parse_inline(rest, &mut links)));
            i += 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('>') {
            flush(&mut paragraph, &mut blocks, &mut links);
            let mut body = vec![rest.trim().to_string()];
            i += 1;
            while i < rows.len() {
                let next = rows[i].trim();
                if let Some(more) = next.strip_prefix('>') {
                    body.push(more.trim().to_string());
                    i += 1;
                } else if next.is_empty() {
                    break;
                } else {
                    body.push(next.to_string());
                    i += 1;
                }
            }
            blocks.push(Block::Quote(parse_inline(&body.join(" "), &mut links)));
            continue;
        }
        if let Some((level, marker, rest)) = item_of(line) {
            flush(&mut paragraph, &mut blocks, &mut links);
            blocks.push(Block::Item(level, marker, parse_inline(rest, &mut links)));
            i += 1;
            continue;
        }
        if let Some(table) = table_at(&rows, i) {
            flush(&mut paragraph, &mut blocks, &mut links);
            let (block, consumed) = table;
            blocks.push(block);
            i += consumed;
            continue;
        }
        // Bloc de code indenté de quatre espaces, hors liste.
        if line.starts_with("    ") && paragraph.is_empty() {
            let mut body = Vec::new();
            while i < rows.len() && (rows[i].starts_with("    ") || rows[i].trim().is_empty()) {
                if rows[i].trim().is_empty() && !body.is_empty() {
                    // Une ligne vide ne clôt le bloc que si rien ne suit.
                    let next_indented = rows
                        .get(i + 1)
                        .is_some_and(|l| l.starts_with("    ") && !l.trim().is_empty());
                    if !next_indented {
                        break;
                    }
                }
                body.push(rows[i].get(4..).unwrap_or_default().to_string());
                i += 1;
            }
            blocks.push(Block::Code(body));
            continue;
        }
        paragraph.push(trimmed);
        i += 1;
    }
    if !paragraph.is_empty() {
        let joined = paragraph.join(" ");
        blocks.push(Block::Paragraph(parse_inline(&joined, &mut links)));
    }
    while matches!(blocks.last(), Some(Block::Gap)) {
        blocks.pop();
    }
    Parsed { blocks, links }
}

/// Caractère de clôture d'un bloc de code, si la ligne en est une.
fn fence_of(trimmed: &str) -> Option<char> {
    ['`', '~']
        .into_iter()
        .find(|c| trimmed.starts_with(&c.to_string().repeat(3)))
}

/// Vrai si la ligne est une règle horizontale.
fn is_rule(trimmed: &str) -> bool {
    for c in ['-', '*', '_'] {
        let body: String = trimmed.chars().filter(|x| !x.is_whitespace()).collect();
        if body.len() >= 3 && body.chars().all(|x| x == c) {
            return true;
        }
    }
    false
}

/// Niveau et contenu d'un titre ATX.
fn heading_of(trimmed: &str) -> Option<(u8, &str)> {
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = trimmed.get(hashes..)?;
    if !rest.starts_with(' ') && !rest.is_empty() {
        return None;
    }
    let body = rest.trim().trim_end_matches('#').trim_end();
    u8::try_from(hashes).ok().map(|h| (h, body))
}

/// Niveau, puce et contenu d'un élément de liste.
fn item_of(line: &str) -> Option<(usize, String, &str)> {
    let indent = line.len() - line.trim_start().len();
    let trimmed = line.trim_start();
    let level = (indent / 2).min(2);
    for marker in ['-', '*', '+'] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            if rest.starts_with(' ') {
                // Une règle horizontale (`---`) ressemble à une puce : trancher.
                if is_rule(trimmed) {
                    return None;
                }
                return Some((level, "\u{2022}".into(), rest.trim_start()));
            }
        }
    }
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 9 {
        return None;
    }
    let rest = trimmed.get(digits..)?;
    let sep = rest.chars().next()?;
    if sep != '.' && sep != ')' {
        return None;
    }
    let body = rest.get(1..)?;
    if !body.starts_with(' ') {
        return None;
    }
    let number = trimmed.get(..digits)?;
    Some((level, format!("{number}."), body.trim_start()))
}

/// Tableau à barres verticales commençant à la ligne `i` : le bloc et le
/// nombre de lignes consommées.
fn table_at(lines: &[&str], i: usize) -> Option<(Block, usize)> {
    let header = lines.get(i)?.trim();
    let separator = lines.get(i + 1)?.trim();
    if !header.contains('|') || !separator.contains('|') {
        return None;
    }
    let seps = split_row(separator);
    if seps.is_empty()
        || !seps
            .iter()
            .all(|c| !c.is_empty() && c.chars().all(|x| x == '-' || x == ':' || x == ' '))
    {
        return None;
    }
    let aligns: Vec<TextAlign> = seps
        .iter()
        .map(|c| {
            let t = c.trim();
            if t.ends_with(':') && !t.starts_with(':') {
                TextAlign::Right
            } else if t.starts_with(':') && t.ends_with(':') {
                TextAlign::Center
            } else {
                TextAlign::Left
            }
        })
        .collect();
    let head = split_row(header);
    let mut rows = Vec::new();
    let mut consumed = 2;
    while let Some(line) = lines.get(i + consumed) {
        let t = line.trim();
        if t.is_empty() || !t.contains('|') {
            break;
        }
        rows.push(split_row(t));
        consumed += 1;
    }
    Some((Block::Table(head, rows, aligns), consumed))
}

/// Cellules d'une ligne de tableau, barres de bord retirées, marques de
/// style enlevées.
fn split_row(line: &str) -> Vec<String> {
    let body = line.trim().trim_start_matches('|').trim_end_matches('|');
    body.split('|')
        .map(|c| {
            let mut sink = Vec::new();
            parse_inline(c.trim(), &mut sink)
                .into_iter()
                .map(|s| s.text)
                .collect::<String>()
        })
        .collect()
}

/// Analyse les marques de style d'une ligne et rend ses fragments.
fn parse_inline(text: &str, links: &mut Vec<String>) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<Span> = Vec::new();
    let mut current = String::new();
    let mut style = Inline::default();
    let mut i = 0;
    let push = |current: &mut String, spans: &mut Vec<Span>, style: Inline| {
        if !current.is_empty() {
            spans.push(Span {
                text: std::mem::take(current),
                style,
            });
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            current.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '`' && !style.code {
            // Code incorporé : tout est littéral jusqu'au prochain accent grave.
            if let Some(end) = chars[i + 1..].iter().position(|x| *x == '`') {
                push(&mut current, &mut spans, style);
                let body: String = chars[i + 1..=i + end].iter().collect();
                spans.push(Span {
                    text: body,
                    style: Inline {
                        code: true,
                        ..style
                    },
                });
                i += end + 2;
                continue;
            }
        }
        if c == '[' {
            if let Some((label, url, len)) = link_at(&chars, i) {
                push(&mut current, &mut spans, style);
                links.push(url);
                let index = links.len() - 1;
                for span in parse_inline(&label, links) {
                    spans.push(Span {
                        text: span.text,
                        style: Inline {
                            link: Some(index),
                            ..span.style
                        },
                    });
                }
                i += len;
                continue;
            }
        }
        if (c == '*' || c == '_') && chars.get(i + 1) == Some(&c) {
            push(&mut current, &mut spans, style);
            style.bold = !style.bold;
            i += 2;
            continue;
        }
        if c == '*' || c == '_' {
            push(&mut current, &mut spans, style);
            style.italic = !style.italic;
            i += 1;
            continue;
        }
        current.push(c);
        i += 1;
    }
    push(&mut current, &mut spans, style);
    spans
}

/// Lien `[label](url)` commençant en `i` : libellé, adresse et longueur.
fn link_at(chars: &[char], i: usize) -> Option<(String, String, usize)> {
    let close = chars[i + 1..].iter().position(|c| *c == ']')? + i + 1;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = chars[close + 2..].iter().position(|c| *c == ')')? + close + 2;
    let label: String = chars[i + 1..close].iter().collect();
    let url: String = chars[close + 2..end].iter().collect();
    Some((label, url.trim().to_string(), end - i + 1))
}

// --- Composition -----------------------------------------------------------

/// Les cinq fontes employées par le Markdown.
struct Faces {
    regular: FontRef,
    bold: FontRef,
    italic: FontRef,
    bold_italic: FontRef,
    mono: FontRef,
}

impl Faces {
    /// Fonte d'un style incorporé.
    fn of(&self, style: Inline) -> FontRef {
        if style.code {
            return self.mono;
        }
        match (style.bold, style.italic) {
            (true, true) => self.bold_italic,
            (true, false) => self.bold,
            (false, true) => self.italic,
            (false, false) => self.regular,
        }
    }
}

/// Variante grasse ou italique d'une police standard, dans sa famille.
fn variant(base: StandardFont, bold: bool, italic: bool) -> StandardFont {
    let name = base.base_font();
    let family = if name.starts_with("Courier") {
        "courier"
    } else if name.starts_with("Times") {
        "times"
    } else {
        "helvetica"
    };
    let mut key = String::from(family);
    if bold {
        key.push_str(" bold");
    }
    if italic {
        key.push_str(" italic");
    }
    StandardFont::from_name(&key).unwrap_or(base)
}

/// Variante grasse d'une police standard : `combine` compose le titre de son
/// sommaire avec la même famille que le corps du texte.
pub(crate) fn bold_variant(base: StandardFont) -> StandardFont {
    variant(base, true, false)
}

/// Prépare les cinq fontes d'après le texte réellement employé par chaque
/// style : une police incorporée n'est choisie qu'en fonction des caractères
/// qu'elle devra écrire.
fn faces(flow: &mut Flow, parsed: &Parsed, layout: &TextLayout) -> Result<Faces> {
    let mut buckets: [String; 5] = Default::default();
    let mut visit = |style: Inline, text: &str| {
        let slot = if style.code {
            4
        } else {
            usize::from(style.bold) + 2 * usize::from(style.italic)
        };
        buckets[slot].push_str(text);
    };
    for block in &parsed.blocks {
        match block {
            Block::Heading(_, spans) => {
                for s in spans {
                    // Un titre est toujours gras.
                    visit(
                        Inline {
                            bold: true,
                            ..s.style
                        },
                        &s.text,
                    );
                }
            }
            Block::Paragraph(spans) | Block::Item(_, _, spans) => {
                for s in spans {
                    visit(s.style, &s.text);
                }
            }
            Block::Quote(spans) => {
                for s in spans {
                    visit(
                        Inline {
                            italic: true,
                            ..s.style
                        },
                        &s.text,
                    );
                }
            }
            Block::Code(body) => {
                for line in body {
                    visit(
                        Inline {
                            code: true,
                            ..Inline::default()
                        },
                        line,
                    );
                }
            }
            Block::Table(head, rows, _) => {
                for cell in head {
                    visit(
                        Inline {
                            bold: true,
                            ..Inline::default()
                        },
                        cell,
                    );
                }
                for row in rows {
                    for cell in row {
                        visit(Inline::default(), cell);
                    }
                }
            }
            Block::Rule | Block::Gap => {}
        }
    }
    // Les puces et les numéros s'écrivent en romain.
    buckets[0].push_str("\u{2022}0123456789. ");
    let mut pick = |text: &str, bold: bool, italic: bool, mono: bool| {
        let standard = if mono {
            variant(StandardFont::Courier, bold, italic)
        } else {
            variant(layout.font, bold, italic)
        };
        let style = match (bold, italic) {
            (true, true) => FontStyle::BoldItalic,
            (true, false) => FontStyle::Bold,
            (false, true) => FontStyle::Italic,
            (false, false) => FontStyle::Regular,
        };
        let doc = &flow.builder.doc;
        flow.builder.fonts.pick(doc, text, standard, style)
    };
    Ok(Faces {
        regular: pick(&buckets[0], false, false, false)?,
        bold: pick(&buckets[1], true, false, false)?,
        italic: pick(&buckets[2], false, true, false)?,
        bold_italic: pick(&buckets[3], true, true, false)?,
        mono: pick(&buckets[4], false, false, true)?,
    })
}

/// Un mot (ou une espace) prêt à être posé, avec son style et sa largeur.
#[derive(Debug, Clone)]
struct Token {
    text: String,
    style: Inline,
    width: f64,
    /// Vrai si une espace séparait ce mot du précédent dans la source. C'est
    /// ce qui empêche `**gras**,` de devenir « gras , » : la virgule est un
    /// fragment à part, mais rien ne la séparait du mot.
    space_before: bool,
}

/// Découpe des fragments en mots mesurés.
fn tokenize(flow: &Flow, faces: &Faces, spans: &[Span], size: f64) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::new();
    // Une espace vue depuis le dernier mot émis, en attente d'être portée par
    // le mot suivant.
    let mut pending = false;
    for span in spans {
        let push = |text: String, out: &mut Vec<Token>, pending: &mut bool| {
            let space_before = *pending && !out.is_empty();
            *pending = false;
            out.push(Token {
                width: flow.builder.fonts.width(faces.of(span.style), &text, size),
                text,
                style: span.style,
                space_before,
            });
        };
        // Le code incorporé garde ses espaces internes : un seul jeton, qui ne
        // se coupe pas.
        if span.style.code {
            push(span.text.clone(), &mut out, &mut pending);
            continue;
        }
        pending |= span.text.starts_with(char::is_whitespace);
        let mut words = span.text.split_whitespace().peekable();
        let had_words = words.peek().is_some();
        for word in words {
            push(word.to_string(), &mut out, &mut pending);
            pending = true;
        }
        if had_words {
            pending = span.text.ends_with(char::is_whitespace);
        } else {
            pending |= !span.text.is_empty();
        }
    }
    out
}

/// Découpe les jetons en lignes tenant dans `width`, l'espace inter-mots
/// étant mesuré avec la fonte du mot qui précède.
fn wrap_tokens(
    flow: &Flow,
    faces: &Faces,
    tokens: &[Token],
    size: f64,
    width: f64,
) -> Vec<Vec<Token>> {
    let mut lines: Vec<Vec<Token>> = Vec::new();
    let mut current: Vec<Token> = Vec::new();
    let mut used = 0.0;
    for token in tokens {
        let space = if current.is_empty() || !token.space_before {
            0.0
        } else {
            flow.builder.fonts.width(faces.of(token.style), " ", size)
        };
        // Un mot soudé au précédent (une ponctuation, par exemple) ne peut pas
        // ouvrir une ligne à lui seul : il suit son mot.
        if !current.is_empty() && token.space_before && used + space + token.width > width {
            lines.push(std::mem::take(&mut current));
            used = 0.0;
            current.push(Token {
                space_before: false,
                ..token.clone()
            });
            used += token.width;
            continue;
        }
        used += space + token.width;
        current.push(token.clone());
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}

/// Pose une ligne de jetons et rend les rectangles des liens qu'elle porte.
#[allow(clippy::too_many_arguments)] // une ligne composite : tout est nécessaire
fn draw_tokens(
    flow: &mut Flow,
    faces: &Faces,
    tokens: &[Token],
    size: f64,
    (x0, width): (f64, f64),
    baseline: f64,
    align: TextAlign,
    justify: bool,
) -> Vec<(usize, Rect)> {
    if tokens.is_empty() {
        return Vec::new();
    }
    let page = flow.page();
    let spaces = tokens.iter().skip(1).filter(|t| t.space_before).count();
    let words: f64 = tokens.iter().map(|t| t.width).sum();
    let space_width: f64 = tokens
        .iter()
        .skip(1)
        .filter(|t| t.space_before)
        .map(|t| flow.builder.fonts.width(faces.of(t.style), " ", size))
        .sum();
    let natural = words + space_width;
    #[allow(clippy::cast_precision_loss)] // quelques dizaines de mots au plus
    let extra = if justify && align == TextAlign::Justify && spaces > 0 && natural < width {
        (width - natural) / spaces as f64
    } else {
        0.0
    };
    let mut x = match align {
        TextAlign::Right => x0 + width - natural,
        TextAlign::Center => x0 + (width - natural) / 2.0,
        TextAlign::Left | TextAlign::Justify => x0,
    };
    let mut links: Vec<(usize, Rect)> = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        let font = faces.of(token.style);
        if i > 0 && token.space_before {
            x += flow.builder.fonts.width(font, " ", size) + extra;
        }
        let ascent = flow.builder.fonts.ascent(font) * size / 1000.0;
        let descent = flow.builder.fonts.descent(font) * size / 1000.0;
        let ink: Rgb = if token.style.link.is_some() {
            LINK_INK
        } else {
            [0.0, 0.0, 0.0]
        };
        if token.style.code {
            let pad = size * 0.15;
            flow.builder.content(page).fill_rect(
                Rect::new(
                    x - pad,
                    baseline + descent,
                    x + token.width + pad,
                    baseline + ascent,
                ),
                CODE_BACKGROUND,
            );
        }
        let pen = Pen {
            font,
            size,
            color: ink,
        };
        let (fonts, content) = flow.builder.draw(page);
        content.text(fonts, pen, (x, baseline), &token.text);
        if let Some(index) = token.style.link {
            let rect = Rect::new(x, baseline + descent, x + token.width, baseline + ascent);
            match links.last_mut() {
                Some((prev, r)) if *prev == index => *r = r.union(&rect),
                _ => links.push((index, rect)),
            }
        }
        x += token.width;
    }
    // Le soulignement suit la zone entière du lien, espaces comprises : le
    // tracer mot par mot le laisserait pointillé.
    for (_, rect) in &links {
        flow.builder.content(page).rule(
            rect.x0,
            rect.x1,
            baseline - size * 0.09,
            size * 0.05,
            LINK_INK,
        );
    }
    links
}

/// Compose un document à partir de Markdown.
///
/// La mise en page (format, marges, corps, interligne, en-tête et pied)
/// vient de `layout` ; `layout.align` s'applique aux paragraphes, jamais aux
/// titres, aux listes ni aux tableaux.
///
/// # Errors
/// Texte hors WinAnsiEncoding sans police système exploitable, ou document
/// impossible à refermer.
#[allow(clippy::too_many_lines)] // un bloc `match` par élément Markdown
pub fn from_markdown(source: &str, layout: &TextLayout) -> Result<Document> {
    let parsed = parse(source);
    let mut flow = Flow::new(&layout.setup)?;
    let faces = faces(&mut flow, &parsed, layout)?;
    let area = flow.area();
    let base = layout.size;
    let leading = layout.leading();
    for block in &parsed.blocks {
        match block {
            Block::Gap => flow.skip(layout.paragraph_spacing),
            Block::Rule => {
                let top = flow.reserve(leading);
                let y = top - leading / 2.0;
                let page = flow.page();
                flow.builder
                    .content(page)
                    .rule(area.x0, area.x1, y, 0.7, RULE);
            }
            Block::Heading(level, spans) => {
                let size = base * HEADING_SCALE[usize::from(*level - 1).min(5)];
                let bold: Vec<Span> = spans
                    .iter()
                    .map(|s| Span {
                        text: s.text.clone(),
                        style: Inline {
                            bold: true,
                            ..s.style
                        },
                    })
                    .collect();
                flow.skip(size * 0.5);
                let tokens = tokenize(&flow, &faces, &bold, size);
                let mut titled = false;
                let lines = wrap_tokens(&flow, &faces, &tokens, size, area.width());
                for line in &lines {
                    let head = size * layout.line_height;
                    let top = flow.reserve(head);
                    let baseline = Flow::baseline(&flow.builder.fonts, faces.bold, size, top, head);
                    if !titled && layout.bookmark_headings {
                        titled = true;
                        let title: String =
                            spans.iter().map(|s| s.text.as_str()).collect::<String>();
                        let page = flow.page();
                        flow.builder.add_bookmark(title.trim(), page);
                    }
                    let anchors = draw_tokens(
                        &mut flow,
                        &faces,
                        line,
                        size,
                        (area.x0, area.width()),
                        baseline,
                        TextAlign::Left,
                        false,
                    );
                    attach(&mut flow, &parsed.links, &anchors);
                }
                flow.skip(size * 0.25);
            }
            Block::Paragraph(spans) => {
                let tokens = tokenize(&flow, &faces, spans, base);
                let lines = wrap_tokens(&flow, &faces, &tokens, base, area.width());
                let last = lines.len().saturating_sub(1);
                for (k, line) in lines.iter().enumerate() {
                    let top = flow.reserve(leading);
                    let baseline =
                        Flow::baseline(&flow.builder.fonts, faces.regular, base, top, leading);
                    let anchors = draw_tokens(
                        &mut flow,
                        &faces,
                        line,
                        base,
                        (area.x0, area.width()),
                        baseline,
                        layout.align,
                        k != last,
                    );
                    attach(&mut flow, &parsed.links, &anchors);
                }
            }
            Block::Item(level, marker, spans) => {
                #[allow(clippy::cast_precision_loss)] // trois niveaux au plus
                let indent = INDENT * (*level as f64 + 1.0);
                let x0 = area.x0 + indent;
                let tokens = tokenize(&flow, &faces, spans, base);
                let lines = wrap_tokens(&flow, &faces, &tokens, base, area.x1 - x0);
                for (k, line) in lines.iter().enumerate() {
                    let top = flow.reserve(leading);
                    let baseline =
                        Flow::baseline(&flow.builder.fonts, faces.regular, base, top, leading);
                    if k == 0 {
                        let width = flow.builder.fonts.width(faces.regular, marker, base);
                        let page = flow.page();
                        let pen = Pen {
                            font: faces.regular,
                            size: base,
                            color: [0.0, 0.0, 0.0],
                        };
                        let (fonts, content) = flow.builder.draw(page);
                        content.text(fonts, pen, (x0 - width - base * 0.35, baseline), marker);
                    }
                    let anchors = draw_tokens(
                        &mut flow,
                        &faces,
                        line,
                        base,
                        (x0, area.x1 - x0),
                        baseline,
                        TextAlign::Left,
                        false,
                    );
                    attach(&mut flow, &parsed.links, &anchors);
                }
            }
            Block::Quote(spans) => {
                let italic: Vec<Span> = spans
                    .iter()
                    .map(|s| Span {
                        text: s.text.clone(),
                        style: Inline {
                            italic: true,
                            ..s.style
                        },
                    })
                    .collect();
                let x0 = area.x0 + INDENT;
                let tokens = tokenize(&flow, &faces, &italic, base);
                let lines = wrap_tokens(&flow, &faces, &tokens, base, area.x1 - x0);
                for line in &lines {
                    let top = flow.reserve(leading);
                    let baseline =
                        Flow::baseline(&flow.builder.fonts, faces.italic, base, top, leading);
                    let page = flow.page();
                    // Filet vertical : un rectangle plein de 2 pt.
                    flow.builder
                        .content(page)
                        .fill_rect(Rect::new(area.x0, top - leading, area.x0 + 2.0, top), RULE);
                    let mut x = x0;
                    for (i, token) in line.iter().enumerate() {
                        let font = faces.of(token.style);
                        if i > 0 && token.space_before {
                            x += flow.builder.fonts.width(font, " ", base);
                        }
                        let pen = Pen {
                            font,
                            size: base,
                            color: QUOTE_INK,
                        };
                        let (fonts, content) = flow.builder.draw(page);
                        content.text(fonts, pen, (x, baseline), &token.text);
                        x += token.width;
                    }
                }
            }
            Block::Code(body) => {
                let size = base * 0.92;
                let code_leading = size * 1.3;
                let measure = |s: &str| flow.builder.fonts.width(faces.mono, s, size);
                let mut rendered: Vec<String> = Vec::new();
                for line in body {
                    rendered.extend(wrap_hard(line, area.width() - 12.0, &measure));
                }
                for line in &rendered {
                    let top = flow.reserve(code_leading);
                    let page = flow.page();
                    flow.builder.content(page).fill_rect(
                        Rect::new(area.x0, top - code_leading, area.x1, top),
                        CODE_BACKGROUND,
                    );
                    let baseline =
                        Flow::baseline(&flow.builder.fonts, faces.mono, size, top, code_leading);
                    let pen = Pen {
                        font: faces.mono,
                        size,
                        color: [0.1, 0.1, 0.15],
                    };
                    let (fonts, content) = flow.builder.draw(page);
                    content.text(fonts, pen, (area.x0 + 6.0, baseline), line);
                }
            }
            Block::Table(head, rows, aligns) => {
                draw_table(&mut flow, &faces, layout, (head, rows, aligns));
            }
        }
    }
    let mut builder = flow.builder;
    super::text::running_titles(&mut builder, layout)?;
    builder.finish()
}

/// Pose les annotations `/Link` d'une ligne.
fn attach(flow: &mut Flow, urls: &[String], links: &[(usize, Rect)]) {
    let page = flow.page();
    for (index, rect) in links {
        if let Some(url) = urls.get(*index) {
            flow.builder.add_uri_link(page, *rect, url);
        }
    }
}

/// Dessine un tableau : colonnes de largeur proportionnelle au contenu,
/// en-tête gras sur fond gris, filets fins entre les lignes.
#[allow(clippy::cast_precision_loss)] // colonnes et lignes : jamais 2^53
fn draw_table(
    flow: &mut Flow,
    faces: &Faces,
    layout: &TextLayout,
    (head, rows, aligns): (&[String], &[Vec<String>], &[TextAlign]),
) {
    let area = flow.area();
    let base = layout.size * 0.95;
    let leading = base * 1.35;
    let columns = head.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
    if columns == 0 {
        return;
    }
    let padding = 5.0;
    // Largeur naturelle de chaque colonne, ramenée à la largeur utile.
    let mut natural = vec![0.0f64; columns];
    for (c, cell) in head.iter().enumerate() {
        natural[c] = natural[c].max(flow.builder.fonts.width(faces.bold, cell, base));
    }
    for row in rows {
        for (c, cell) in row.iter().enumerate().take(columns) {
            natural[c] = natural[c].max(flow.builder.fonts.width(faces.regular, cell, base));
        }
    }
    let total: f64 = natural.iter().sum::<f64>() + padding * 2.0 * columns as f64;
    let available = area.width();
    let widths: Vec<f64> = if total <= available {
        // Le reste est réparti également : le tableau occupe toute la largeur.
        natural
            .iter()
            .map(|w| w + padding * 2.0 + (available - total) / columns as f64)
            .collect()
    } else {
        natural
            .iter()
            .map(|w| (w + padding * 2.0) * available / total)
            .collect()
    };
    let mut all: Vec<(&[String], bool)> = vec![(head, true)];
    all.extend(rows.iter().map(|r| (r.as_slice(), false)));
    for (cells, header) in all {
        // Hauteur de la ligne : la cellule la plus haute une fois repliée.
        let mut folded: Vec<Vec<String>> = Vec::with_capacity(columns);
        for (c, width) in widths.iter().enumerate() {
            let cell = cells.get(c).map_or("", String::as_str);
            let font = if header { faces.bold } else { faces.regular };
            let measure = |s: &str| flow.builder.fonts.width(font, s, base);
            folded.push(wrap(
                cell,
                (width - padding * 2.0).max(1.0),
                false,
                &measure,
            ));
        }
        let lines = folded.iter().map(Vec::len).max().unwrap_or(1);
        let height = leading * lines as f64;
        let top = flow.reserve(height);
        let page = flow.page();
        if header {
            flow.builder.content(page).fill_rect(
                Rect::new(area.x0, top - height, area.x1, top),
                CODE_BACKGROUND,
            );
        }
        flow.builder
            .content(page)
            .rule(area.x0, area.x1, top - height, 0.5, RULE);
        let mut x = area.x0;
        for (c, (cell_lines, column)) in folded.iter().zip(&widths).enumerate() {
            let font = if header { faces.bold } else { faces.regular };
            for (k, line) in cell_lines.iter().enumerate() {
                let band = top - leading * k as f64;
                let baseline = Flow::baseline(&flow.builder.fonts, font, base, band, leading);
                let text_width = flow.builder.fonts.width(font, line, base);
                let inner = column - padding * 2.0;
                let dx = match aligns.get(c).copied().unwrap_or(TextAlign::Left) {
                    TextAlign::Right => inner - text_width,
                    TextAlign::Center => (inner - text_width) / 2.0,
                    TextAlign::Left | TextAlign::Justify => 0.0,
                };
                let pen = Pen {
                    font,
                    size: base,
                    color: [0.0, 0.0, 0.0],
                };
                let (fonts, content) = flow.builder.draw(page);
                content.text(fonts, pen, (x + padding + dx, baseline), line);
            }
            x += column;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use acrux_document::collect_pages;

    fn page_text(doc: &Document, index: usize) -> String {
        let pages = collect_pages(doc).unwrap();
        crate::text::extract_page_text(doc, &pages[index])
            .unwrap()
            .lines
            .iter()
            .map(crate::text::Line::text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn roundtrip(doc: &Document) -> Document {
        Document::from_bytes(doc.save_full().unwrap()).unwrap()
    }

    #[test]
    fn headings_are_recognised_at_every_level() {
        for level in 1..=6u8 {
            let source = format!("{} Titre", "#".repeat(usize::from(level)));
            let blocks = parse(&source).blocks;
            assert!(
                matches!(&blocks[0], Block::Heading(l, s) if *l == level && s[0].text == "Titre"),
                "{blocks:?}"
            );
        }
        // Sept dièses ne sont plus un titre.
        assert!(matches!(parse("####### x").blocks[0], Block::Paragraph(_)));
        // Un dièse collé au texte non plus.
        assert!(matches!(
            parse("#pas-un-titre").blocks[0],
            Block::Paragraph(_)
        ));
    }

    #[test]
    fn inline_styles_split_the_text() {
        let mut links = Vec::new();
        let spans = parse_inline("du **gras** et de l'*italique* et du `code`", &mut links);
        let bold = spans.iter().find(|s| s.style.bold).unwrap();
        assert_eq!(bold.text, "gras");
        let italic = spans.iter().find(|s| s.style.italic).unwrap();
        assert_eq!(italic.text, "italique");
        let code = spans.iter().find(|s| s.style.code).unwrap();
        assert_eq!(code.text, "code");
        // Le texte reconstitué n'a plus aucune marque.
        let flat: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(flat, "du gras et de l'italique et du code");
    }

    #[test]
    fn escapes_keep_the_marks_literal() {
        let mut links = Vec::new();
        let spans = parse_inline(r"un \*etoile\* et un \\ antislash", &mut links);
        let flat: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(flat, r"un *etoile* et un \ antislash");
        assert!(spans.iter().all(|s| !s.style.italic));
    }

    #[test]
    fn links_are_collected_with_their_labels() {
        let mut links = Vec::new();
        let spans = parse_inline("voir [le site](https://exemple.fr/a) ici", &mut links);
        assert_eq!(links, vec!["https://exemple.fr/a"]);
        let linked: String = spans
            .iter()
            .filter(|s| s.style.link == Some(0))
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(linked, "le site");
    }

    #[test]
    fn lists_keep_their_markers_and_levels() {
        let blocks = parse("- un\n- deux\n  - imbriqué\n\n1. premier\n2) second").blocks;
        let items: Vec<&Block> = blocks
            .iter()
            .filter(|b| matches!(b, Block::Item(..)))
            .collect();
        assert_eq!(items.len(), 5);
        assert!(matches!(items[0], Block::Item(0, m, _) if m.as_str() == "\u{2022}"));
        assert!(matches!(items[2], Block::Item(1, _, _)), "{:?}", items[2]);
        assert!(matches!(items[3], Block::Item(0, m, _) if m.as_str() == "1."));
        assert!(matches!(items[4], Block::Item(0, m, _) if m.as_str() == "2."));
    }

    #[test]
    fn rules_are_not_confused_with_bullets() {
        assert!(matches!(parse("---").blocks[0], Block::Rule));
        assert!(matches!(parse("***").blocks[0], Block::Rule));
        assert!(matches!(parse("___").blocks[0], Block::Rule));
        assert!(matches!(parse("- - -").blocks[0], Block::Rule));
        assert!(matches!(parse("- texte").blocks[0], Block::Item(..)));
    }

    #[test]
    fn fenced_code_is_taken_literally() {
        let blocks = parse("```rust\nlet x = **pas du gras**;\n```").blocks;
        assert!(
            matches!(&blocks[0], Block::Code(b) if b == &["let x = **pas du gras**;".to_string()]),
            "{blocks:?}"
        );
    }

    #[test]
    fn a_pipe_table_needs_its_separator_row() {
        let blocks = parse("| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |").blocks;
        let Block::Table(head, rows, aligns) = &blocks[0] else {
            panic!("tableau attendu : {blocks:?}");
        };
        assert_eq!(head, &["a".to_string(), "b".to_string()]);
        assert_eq!(rows.len(), 2);
        assert_eq!(aligns.len(), 2);
        // Sans ligne de séparation, ce n'est qu'un paragraphe.
        assert!(matches!(
            parse("| a | b |\n| 1 | 2 |").blocks[0],
            Block::Paragraph(_)
        ));
    }

    #[test]
    fn table_alignment_is_read_from_the_separator() {
        let blocks = parse("| a | b | c |\n|:--|:-:|--:|\n| 1 | 2 | 3 |").blocks;
        let Block::Table(_, _, aligns) = &blocks[0] else {
            panic!("tableau attendu");
        };
        assert_eq!(
            aligns,
            &[TextAlign::Left, TextAlign::Center, TextAlign::Right]
        );
    }

    #[test]
    fn a_quote_gathers_its_lines() {
        let blocks = parse("> une citation\n> sur deux lignes\n\nsuite").blocks;
        let Block::Quote(spans) = &blocks[0] else {
            panic!("citation attendue : {blocks:?}");
        };
        let flat: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(flat, "une citation sur deux lignes");
    }

    #[test]
    fn a_whole_document_renders_and_reads_back() {
        let source = "# Titre\n\nUn paragraphe avec du **gras**.\n\n- un\n- deux\n\n\
                      > citation\n\n---\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n\
                      ```\ncode\n```\n\nVoir [le site](https://exemple.fr).";
        let doc = from_markdown(source, &TextLayout::default()).unwrap();
        let reread = roundtrip(&doc);
        let text = page_text(&reread, 0);
        for expected in [
            "Titre", "gras", "un", "deux", "citation", "1", "2", "code", "le site",
        ] {
            assert!(text.contains(expected), "« {expected} » absent de : {text}");
        }
        // Le lien est bien une annotation cliquable.
        let pages = collect_pages(&reread).unwrap();
        let index = crate::navigation::PageIndex::new(&pages);
        let links = crate::navigation::page_links(&reread, &pages[0], &index).unwrap();
        assert_eq!(links.len(), 1, "{links:?}");
    }

    #[test]
    fn headings_are_bigger_than_the_body() {
        let doc = roundtrip(&from_markdown("# Grand\n\npetit", &TextLayout::default()).unwrap());
        let pages = collect_pages(&doc).unwrap();
        let text = crate::text::extract_page_text(&doc, &pages[0]).unwrap();
        let sizes: Vec<f64> = text.lines.iter().map(crate::text::Line::size).collect();
        assert!(sizes[0] > sizes[1] * 1.5, "{sizes:?}");
    }

    #[test]
    fn an_empty_document_still_gives_a_page() {
        let doc = from_markdown("", &TextLayout::default()).unwrap();
        assert_eq!(collect_pages(&doc).unwrap().len(), 1);
    }
}
