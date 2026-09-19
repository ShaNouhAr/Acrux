//! Recomposition d'un paragraphe entier dans sa boîte : retour à la ligne aux
//! espaces, interligne, alignement et retrait de première ligne d'origine.
//!
//! Le paragraphe est repéré par son indice dans l'ordre de lecture de la page
//! (`PageText::blocks`, paragraphes mis bout à bout). Toutes les opérations de
//! dessin de texte qui l'ont produit sont supprimées, et le texte recomposé
//! est écrit **à la place de la première d'entre elles** : il hérite donc
//! exactement de la matrice courante, du découpage (`W n`) et de la couleur du
//! texte d'origine, et le reste de la page n'est pas touché.
//!
//! Comportement en cas de débordement : si le nouveau texte demande plus de
//! lignes que la boîte n'en contenait, les lignes supplémentaires sont écrites
//! **en dessous** de la boîte, au même interligne (elles peuvent recouvrir ce
//! qui suit). [`ReflowOptions::keep_font_size`] à `false` réduit d'abord la
//! taille (par pas de 5 %, jusqu'à `min_size`) pour tenir dans le nombre de
//! lignes d'origine.

use std::collections::BTreeMap;

use acrux_core::{Error, Matrix, Result};
use acrux_document::{Document, Page};

use super::{
    encode, fmt, out_str, restore_matrices, rewrite_show_op, scan, write_matrix, write_tj, Cut,
    GlyphIndex, Item, Replacement,
};
use crate::text::{extract_page_text, Alignment, Paragraph};
use std::fmt::Write as _;

/// Options de recomposition.
#[derive(Debug, Clone)]
pub struct ReflowOptions {
    /// Conserver la taille de police d'origine (défaut) : le texte trop long
    /// déborde sous la boîte.
    pub keep_font_size: bool,
    /// Taille minimale (points) si la réduction est autorisée.
    pub min_size: f64,
}

impl Default for ReflowOptions {
    fn default() -> Self {
        Self {
            keep_font_size: true,
            min_size: 6.0,
        }
    }
}

/// Recompose le paragraphe `paragraph_index` de la page avec `new_text`.
///
/// # Errors
/// Paragraphe inexistant, texte non modifiable (XObject, opérateurs non
/// textuels au milieu), texte non horizontal, ou page non indirecte.
// Suite linéaire d'étapes numérotées (repérage, vérifications, géométrie,
// découpe, écriture, réécriture) : la découper nuirait à la lecture.
#[allow(clippy::too_many_lines)]
pub fn reflow_paragraph(
    doc: &Document,
    page: &Page,
    paragraph_index: usize,
    new_text: &str,
    options: &ReflowOptions,
) -> Result<()> {
    let text = extract_page_text(doc, page)?;
    let paragraphs: Vec<&Paragraph> = text
        .blocks
        .iter()
        .flat_map(|b| b.paragraphs.iter())
        .collect();
    let para = paragraphs.get(paragraph_index).ok_or_else(|| {
        Error::Corrupt(format!(
            "paragraphe {paragraph_index} inexistant ({} dans la page)",
            paragraphs.len()
        ))
    })?;
    let scan = scan::scan(doc, page)?;
    let index = GlyphIndex::new(&scan);
    // 1. Tous les glyphes du paragraphe, et les opérations qui les ont produits.
    let mut sites: Vec<usize> = Vec::new();
    for line in &para.lines {
        let Some(line) = text.lines.get(*line) else {
            continue;
        };
        let mut found: Vec<usize> = Vec::new();
        for glyph in line.words.iter().flat_map(|w| w.glyphs.iter()) {
            let i = index.find(&scan, glyph).ok_or_else(|| {
                Error::Corrupt(format!(
                    "glyphe « {} » introuvable dans le flux de contenu",
                    glyph.text
                ))
            })?;
            found.push(i);
        }
        let (Some(min), Some(max)) = (found.iter().min().copied(), found.iter().max().copied())
        else {
            continue;
        };
        // Les espaces entre les mots ne figurent pas dans `PageText` mais
        // font partie de la ligne : ils doivent disparaître avec elle.
        for i in min..=max {
            if !found.contains(&i) && scan.glyphs[i].text != " " {
                return Err(Error::Unsupported(
                    "un autre texte est dessiné au milieu du paragraphe".into(),
                ));
            }
            sites.push(i);
        }
    }
    sites.sort_unstable();
    sites.dedup();
    if sites.is_empty() {
        return Err(Error::Corrupt("paragraphe sans glyphe".into()));
    }
    // 2. Opérations concernées : toutes doivent être entièrement consommées.
    let mut by_op: BTreeMap<usize, usize> = BTreeMap::new();
    for i in &sites {
        let site = &scan.glyphs[*i].site;
        if site.in_form {
            return Err(Error::Unsupported(
                "paragraphe dessiné dans un XObject de formulaire : non modifiable".into(),
            ));
        }
        *by_op.entry(site.op).or_default() += 1;
    }
    let mut total: BTreeMap<usize, usize> = BTreeMap::new();
    for g in &scan.glyphs {
        if by_op.contains_key(&g.site.op) {
            *total.entry(g.site.op).or_default() += 1;
        }
    }
    for (op, count) in &by_op {
        if total.get(op) != Some(count) {
            return Err(Error::Unsupported(
                "une opération dessine à la fois le paragraphe et d'autres textes".into(),
            ));
        }
    }
    let first_op = *by_op.keys().next().unwrap_or(&0);
    let first_site = &scan.glyphs[sites[0]].site;
    let state = &scan.ops[first_op].after;
    let ctm = state.ctm;
    // 3. Géométrie : matrice de rendu du texte de la première ligne.
    let trm = first_site.tm_before.then(&ctm);
    let scale = trm.a.hypot(trm.b);
    if scale < 1e-9 || trm.b.abs() > scale * 0.01 {
        return Err(Error::Unsupported(
            "paragraphe non horizontal : recomposition non prise en charge".into(),
        ));
    }
    let inverse = ctm
        .invert()
        .ok_or_else(|| Error::Corrupt("matrice courante non inversible".into()))?;
    let name = state
        .font
        .clone()
        .ok_or_else(|| Error::Corrupt("aucune police active".into()))?;
    let prepared = encode::prepare(doc, page, &name, new_text, None)?;
    let box_width = para.bbox.width();
    let spacing = if para.line_spacing > 0.1 {
        para.line_spacing
    } else {
        para.size * 1.2
    };
    // 4. Découpe en lignes, avec réduction éventuelle de la taille.
    let mut size = state.size;
    let mut lines = wrap(
        &prepared,
        new_text,
        size * scale,
        box_width,
        para.first_line_indent,
    );
    if !options.keep_font_size {
        while lines.len() > para.lines.len() && size * scale > options.min_size {
            size *= 0.95;
            lines = wrap(
                &prepared,
                new_text,
                size * scale,
                box_width,
                para.first_line_indent,
            );
        }
    }
    // 5. Écriture du bloc à la place de la première opération.
    let mut out = Vec::new();
    let changed = (size - state.size).abs() > 1e-9 || prepared.resource != name;
    if changed {
        let _ = write!(
            out_str(&mut out),
            "/{} {} Tf ",
            prepared.resource.as_str(),
            fmt(size)
        );
    }
    for (i, line) in lines.iter().enumerate() {
        let width = prepared.width(line) * size * scale;
        let indent = if i == 0 { para.first_line_indent } else { 0.0 };
        let last = i + 1 == lines.len();
        let x = match para.alignment {
            Alignment::Right => para.bbox.x1 - width,
            Alignment::Center => para.bbox.x0 + (box_width - width) / 2.0,
            _ => para.bbox.x0 + indent,
        };
        #[allow(clippy::cast_precision_loss)] // au plus quelques milliers de lignes
        let y = trm.f - (i as f64) * spacing;
        let placed = Matrix::new(trm.a, trm.b, trm.c, trm.d, x, y);
        write_matrix(&mut out, &placed.then(&inverse));
        let justify = para.alignment == Alignment::Justify && !last;
        let available = box_width - indent;
        write_tj(
            &mut out,
            &line_items(&prepared, line, justify, available - width, size * scale),
        );
    }
    if changed {
        let _ = write!(
            out_str(&mut out),
            "/{} {} Tf ",
            name.as_str(),
            fmt(state.size)
        );
    }
    restore_matrices(&mut out, state, super::needs_advance(&scan, first_op));
    while out.last() == Some(&b' ') {
        out.pop();
    }
    // 6. Réécriture : le bloc remplace la première opération, les autres
    //    opérations de texte du paragraphe sont vidées (leur avance est
    //    conservée pour ne rien déplacer après elles).
    let mut replacements = vec![Replacement {
        operation: first_op,
        bytes: out,
    }];
    // Ces opérations sont entièrement vidées : aucun texte n'est inséré, donc
    // aucun avertissement de resserrement n'en sortira.
    let mut ignored = Vec::new();
    for op in by_op.keys().skip(1) {
        replacements.push(Replacement {
            operation: *op,
            bytes: rewrite_show_op(
                doc,
                page,
                &scan,
                *op,
                &[Cut {
                    from: None,
                    to: None,
                    insert: None,
                }],
                &mut ignored,
            )?,
        });
    }
    let content = super::rewrite_bytes(&scan.content, &replacements)?;
    super::set_page_content(doc, page, content)
}

/// Découpe un texte en lignes tenant dans la largeur (coupure aux espaces,
/// puis au caractère pour un mot trop long).
fn wrap(
    prepared: &encode::Prepared,
    text: &str,
    size: f64,
    width: f64,
    first_indent: f64,
) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let available = |lines: &Vec<String>| {
        if lines.is_empty() {
            (width - first_indent).max(1.0)
        } else {
            width.max(1.0)
        }
    };
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if prepared.width(&candidate) * size <= available(&lines) || current.is_empty() {
            if prepared.width(&candidate) * size <= available(&lines) {
                current = candidate;
                continue;
            }
            // Mot seul trop long : coupé au caractère.
            let mut chunk = String::new();
            for c in word.chars() {
                let next = format!("{chunk}{c}");
                if prepared.width(&next) * size > available(&lines) && !chunk.is_empty() {
                    lines.push(std::mem::take(&mut chunk));
                }
                chunk.push(c);
            }
            current = chunk;
            continue;
        }
        lines.push(std::mem::take(&mut current));
        current = word.to_string();
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Éléments d'une ligne : texte, et ajustements entre les mots pour la
/// justification (§9.4.3 : un nombre de `TJ` déplace de −n/1000 × taille).
fn line_items(
    prepared: &encode::Prepared,
    line: &str,
    justify: bool,
    extra: f64,
    size: f64,
) -> Vec<Item> {
    let words: Vec<&str> = line.split(' ').collect();
    if !justify || words.len() < 2 || extra <= 0.0 || size <= 0.0 {
        return vec![Item::Str(prepared.encode(line).bytes)];
    }
    #[allow(clippy::cast_precision_loss)] // au plus quelques dizaines de mots
    let gaps = (words.len() - 1) as f64;
    // Espace supplémentaire par blanc, en millièmes d'em de la taille utilisée.
    let adjust = -extra / gaps / size * 1000.0;
    let mut items = Vec::new();
    for (i, word) in words.iter().enumerate() {
        let piece = if i + 1 == words.len() {
            (*word).to_string()
        } else {
            format!("{word} ")
        };
        items.push(Item::Str(prepared.encode(&piece).bytes));
        if i + 1 < words.len() {
            items.push(Item::Num(adjust));
        }
    }
    items
}
