//! Analyse de mise en page : des lignes géométriques aux blocs, colonnes et
//! paragraphes dans l'ordre de lecture.
//!
//! Étapes :
//! 1. tableaux à filets (`tables::ruled_grids`) : leurs mots sont retirés du flux ;
//! 2. fragments : chaque ligne est coupée aux grands blancs (> 2 em), pour que
//!    deux colonnes partageant une ligne de base ne restent pas soudées ;
//! 3. tableaux sans filets (`tables::aligned_runs`) : fragments courts alignés
//!    en colonnes sur plusieurs lignes consécutives ;
//! 4. découpe XY récursive des fragments et tableaux : on cherche d'abord une
//!    bande verticale vide (colonnes), sinon la plus grande bande horizontale
//!    vide ; les feuilles sont parcourues haut → bas, gauche → droite ;
//! 5. dans chaque feuille, fusion des fragments d'une même ligne de base, puis
//!    paragraphes (interligne, retraits, taille, ponctuation, marqueurs de liste) ;
//! 6. en-têtes et pieds de page (lignes isolées près des bords, numéros de
//!    page), niveaux de titre (taille relative à la médiane de la page).
//!
//! Seuils (en multiples de la taille de police locale, `size`) :
//! - coupure des fragments : blanc > 2,0 em ;
//! - bande verticale (colonnes) : largeur ≥ 0,8 em (min 5 pt), au moins deux
//!   lignes de base de chaque côté ;
//! - bande horizontale : blanc ≥ 0,7 em (min 3 pt) entre les boîtes ;
//! - nouveau paragraphe : écart de lignes de base > 1,5 × interligne connu
//!   (1,25 em à défaut), ratio de taille hors [0,85 ; 1,18], retrait > 0,8 em,
//!   ligne précédente courte de > 2,5 em et terminée par une ponctuation forte,
//!   marqueur de liste ;
//! - paragraphe continué d'une colonne à l'autre : blocs côte à côte, fin sans
//!   ponctuation forte, reprise en minuscule sans retrait ni marqueur, même taille ;
//! - en-tête / pied : bloc de ≤ 2 lignes dans les 10 % haut/bas de la page,
//!   taille ≤ 1,1 × médiane, isolé (écart ≥ 1,5 interligne) ou ressemblant à un
//!   numéro de page ;
//! - titres : ≤ 2 lignes, ≤ 20 mots, taille ≥ 1,6 / 1,3 / 1,15 × médiane.

// Comptes de lignes et de mots : petits entiers convertis en flottants.
#![allow(clippy::cast_precision_loss)]

use acrux_core::Rect;

use super::tables::{self, Grid};
use super::{
    make_line, median, Alignment, Block, BlockKind, Line, PageText, Paragraph, Table, Word,
};

/// Fragment de ligne : mots contigus sans grand blanc.
pub(super) struct Frag {
    pub words: Vec<Word>,
    pub bbox: Rect,
    pub base: f64,
    pub angle: f64,
    pub size: f64,
}

impl Frag {
    fn from_words(words: Vec<Word>, base: f64, angle: f64) -> Self {
        let line = make_line(words);
        let size = line.size();
        Self {
            words: line.words,
            bbox: line.bbox,
            base,
            angle,
            size,
        }
    }

    pub(super) fn width(&self) -> f64 {
        self.bbox.width()
    }
}

/// Élément de la découpe XY.
struct Item {
    bbox: Rect,
    base: f64,
    size: f64,
    kind: ItemKind,
}

#[derive(Clone, Copy)]
enum ItemKind {
    Frag(usize),
    Table(usize),
}

/// Feuille de la découpe XY : éléments et bornes de la colonne englobante.
struct Leaf {
    items: Vec<usize>,
    col: Rect,
}

/// Table en attente, avec ses lignes de texte.
struct PendingTable {
    table: Table,
    lines: Vec<Line>,
}

/// Point d'entrée : voir la documentation du module.
pub(super) fn analyze(
    lines: Vec<Line>,
    rules: &[Rect],
    marks: &[Rect],
    page_box: Rect,
) -> (Vec<Line>, Vec<Table>, Vec<Block>) {
    if lines.is_empty() {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let med = {
        let mut sizes: Vec<f64> = lines
            .iter()
            .flat_map(|l| l.words.iter())
            .flat_map(|w| w.glyphs.iter().map(|g| g.size))
            .collect();
        median(&mut sizes).unwrap_or(10.0).max(1.0)
    };
    let page_pitch = page_pitch(&lines, med);

    // 1. Tableaux à filets.
    let (mut pending, free_lines) = extract_ruled_tables(lines, rules, med);

    // 2. Fragments.
    let mut frags: Vec<Frag> = Vec::new();
    for line in free_lines {
        split_fragments(line, &mut frags);
    }

    // 3. Tableaux sans filets.
    for run in tables::aligned_runs(&frags, med) {
        pending.push(aligned_table(&mut frags, &run));
    }
    let frags: Vec<Frag> = frags.into_iter().filter(|f| !f.words.is_empty()).collect();

    // 4. Découpe XY.
    let mut items: Vec<Item> = frags
        .iter()
        .enumerate()
        .map(|(i, f)| Item {
            bbox: f.bbox,
            base: f.base,
            size: f.size,
            kind: ItemKind::Frag(i),
        })
        .collect();
    for (i, t) in pending.iter().enumerate() {
        items.push(Item {
            bbox: t.table.bbox,
            base: t.table.bbox.y1,
            size: t.lines.first().map_or(med, Line::size),
            kind: ItemKind::Table(i),
        });
    }
    let root = items
        .iter()
        .skip(1)
        .fold(items[0].bbox, |r, it| r.union(&it.bbox));
    let mut leaves = Vec::new();
    xy_cut(&items, (0..items.len()).collect(), root, 0, &mut leaves);

    // 5. Feuilles → lignes, paragraphes, blocs.
    let (mut out_lines, out_tables, mut blocks) =
        leaves_to_blocks(leaves, &items, frags, pending, page_pitch, marks);
    // 6. Paragraphes coupés par un changement de colonne, en-têtes, pieds, titres.
    merge_continuations(&mut blocks);
    mark_headers_footers(&mut blocks, page_box, med, page_pitch);
    for b in &mut blocks {
        if matches!(b.kind, BlockKind::Body) {
            for p in &mut b.paragraphs {
                p.heading = heading_level(p, med);
            }
        }
    }
    out_lines.shrink_to_fit();
    (out_lines, out_tables, blocks)
}

/// Parcourt les feuilles de la découpe dans l'ordre de lecture : les fragments
/// deviennent des lignes puis des paragraphes, les tableaux des blocs `Table`.
fn leaves_to_blocks(
    leaves: Vec<Leaf>,
    items: &[Item],
    frags: Vec<Frag>,
    pending: Vec<PendingTable>,
    page_pitch: f64,
    marks: &[Rect],
) -> (Vec<Line>, Vec<Table>, Vec<Block>) {
    let mut frags: Vec<Option<Frag>> = frags.into_iter().map(Some).collect();
    let mut pending: Vec<Option<PendingTable>> = pending.into_iter().map(Some).collect();
    let mut out_lines: Vec<Line> = Vec::new();
    let mut out_tables: Vec<Table> = Vec::new();
    let mut blocks: Vec<Block> = Vec::new();
    for leaf in leaves {
        let mut idx = leaf.items;
        idx.sort_by(|a, b| {
            items[*b]
                .bbox
                .y1
                .partial_cmp(&items[*a].bbox.y1)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut group: Vec<Frag> = Vec::new();
        for i in idx {
            match items[i].kind {
                ItemKind::Frag(f) => {
                    if let Some(f) = frags[f].take() {
                        group.push(f);
                    }
                }
                ItemKind::Table(t) => {
                    if !group.is_empty() {
                        blocks.push(text_block(
                            std::mem::take(&mut group),
                            leaf.col,
                            page_pitch,
                            marks,
                            &mut out_lines,
                        ));
                    }
                    if let Some(mut p) = pending[t].take() {
                        for line in p.lines {
                            p.table.lines.push(out_lines.len());
                            out_lines.push(line);
                        }
                        blocks.push(Block {
                            kind: BlockKind::Table(out_tables.len()),
                            bbox: p.table.bbox,
                            paragraphs: Vec::new(),
                        });
                        out_tables.push(p.table);
                    }
                }
            }
        }
        if !group.is_empty() {
            blocks.push(text_block(
                group,
                leaf.col,
                page_pitch,
                marks,
                &mut out_lines,
            ));
        }
    }

    (out_lines, out_tables, blocks)
}

/// Ponctuation qui termine une phrase ou un paragraphe.
fn ends_sentence(text: &str) -> bool {
    text.chars()
        .last()
        .is_some_and(|c| matches!(c, '.' | '!' | '?' | ':' | ';' | '»' | '”' | '"' | ')'))
}

/// Un paragraphe coupé par un changement de colonne (dernier paragraphe d'un
/// bloc sans ponctuation forte, premier paragraphe du bloc suivant commençant
/// par une minuscule, sans retrait ni marqueur, même taille) est recollé au
/// paragraphe qui le précède dans l'ordre de lecture.
fn merge_continuations(blocks: &mut Vec<Block>) {
    let mut i = 1;
    while i < blocks.len() {
        let continues = {
            let (before, after) = (&blocks[i - 1], &blocks[i]);
            match (before.paragraphs.last(), after.paragraphs.first()) {
                (Some(last), Some(first))
                    if matches!(before.kind, BlockKind::Body)
                        && matches!(after.kind, BlockKind::Body) =>
                {
                    // Blocs côte à côte (colonnes), pas empilés dans la même colonne.
                    let side_by_side = after.bbox.x0 >= before.bbox.x1 - 0.5 * first.size
                        || before.bbox.x0 >= after.bbox.x1 - 0.5 * first.size;
                    side_by_side
                        && last.marker.is_none()
                        && first.marker.is_none()
                        && !ends_sentence(&last.text)
                        && first.text.chars().next().is_some_and(char::is_lowercase)
                        && first.first_line_indent <= 0.5 * first.size
                        && (0.85..=1.18).contains(&(first.size / last.size.max(0.1)))
                }
                _ => false,
            }
        };
        if continues {
            let moved = blocks[i].paragraphs.remove(0);
            if let Some(target) = blocks[i - 1].paragraphs.last_mut() {
                join_lines(&mut target.text, &moved.text);
                target.lines.extend(moved.lines);
                target.bbox = target.bbox.union(&moved.bbox);
                if target.line_spacing == 0.0 {
                    target.line_spacing = moved.line_spacing;
                }
            }
            if blocks[i].paragraphs.is_empty() {
                blocks.remove(i);
                continue;
            }
            let rest = &mut blocks[i];
            rest.bbox = rest
                .paragraphs
                .iter()
                .skip(1)
                .fold(rest.paragraphs[0].bbox, |r, p| r.union(&p.bbox));
        }
        i += 1;
    }
}

/// Interligne médian de la page (écarts de lignes de base consécutives ≤ 3 em).
fn page_pitch(lines: &[Line], med: f64) -> f64 {
    let mut bases: Vec<f64> = lines.iter().map(Line::baseline).collect();
    bases.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let mut diffs: Vec<f64> = bases
        .windows(2)
        .map(|w| w[0] - w[1])
        .filter(|d| *d > 0.5 * med && *d <= 3.0 * med)
        .collect();
    median(&mut diffs).unwrap_or(1.2 * med)
}

/// Retire des lignes les mots situés dans une grille de filets et construit
/// les tableaux correspondants.
fn extract_ruled_tables(
    lines: Vec<Line>,
    rules: &[Rect],
    med: f64,
) -> (Vec<PendingTable>, Vec<Line>) {
    if rules.len() < 3 {
        return (Vec::new(), lines);
    }
    let word_boxes: Vec<Rect> = lines
        .iter()
        .flat_map(|l| l.words.iter().map(|w| w.bbox))
        .collect();
    let grids: Vec<Grid> = tables::ruled_grids(rules, &word_boxes, med);
    if grids.is_empty() {
        return (Vec::new(), lines);
    }
    let mut per_grid: Vec<(Vec<Line>, Vec<Word>)> =
        grids.iter().map(|_| (Vec::new(), Vec::new())).collect();
    let mut free = Vec::with_capacity(lines.len());
    for line in lines {
        let mut rest: Vec<Word> = Vec::new();
        let mut in_grid: Vec<Vec<Word>> = grids.iter().map(|_| Vec::new()).collect();
        for w in line.words {
            let c = center(&w.bbox);
            match grids.iter().position(|g| g.bbox.contains(c)) {
                Some(gi) => in_grid[gi].push(w),
                None => rest.push(w),
            }
        }
        for (gi, ws) in in_grid.into_iter().enumerate() {
            if !ws.is_empty() {
                per_grid[gi].1.extend(ws.iter().cloned());
                let mut l = make_line(ws);
                l.words.sort_by(|a, b| {
                    a.bbox
                        .x0
                        .partial_cmp(&b.bbox.x0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                per_grid[gi].0.push(l);
            }
        }
        if !rest.is_empty() {
            free.push(make_line(rest));
        }
    }
    let mut pending = Vec::new();
    for (g, (lines, words)) in grids.iter().zip(per_grid) {
        if let Some(table) = tables::grid_table(g, &words) {
            pending.push(PendingTable { table, lines });
        } else {
            // Grille rejetée a posteriori : ses lignes redeviennent libres.
            free.extend(lines);
        }
    }
    // Les lignes libres doivent rester triées de haut en bas.
    free.sort_by(|a, b| {
        b.bbox
            .y1
            .partial_cmp(&a.bbox.y1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    (pending, free)
}

fn center(r: &Rect) -> acrux_core::Point {
    acrux_core::Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1))
}

/// Coupe une ligne aux blancs de plus de 2 em.
fn split_fragments(line: Line, out: &mut Vec<Frag>) {
    let base = line.baseline();
    let angle = line.angle();
    let mut current: Vec<Word> = Vec::new();
    let mut last_end: Option<f64> = None;
    for w in line.words {
        let start = w.glyphs.first().map_or(w.bbox.x0, |g| g.along);
        let end = w.glyphs.last().map_or(w.bbox.x1, |g| g.along_end);
        let size = w.style.size.max(1.0);
        if let Some(le) = last_end {
            if start - le > (2.0 * size).max(6.0) && !current.is_empty() {
                out.push(Frag::from_words(std::mem::take(&mut current), base, angle));
            }
        }
        last_end = Some(end.max(last_end.unwrap_or(end)));
        current.push(w);
    }
    if !current.is_empty() {
        out.push(Frag::from_words(current, base, angle));
    }
}

/// Construit un tableau sans filets à partir de lignes de fragments alignés.
fn aligned_table(frags: &mut [Frag], run: &[Vec<usize>]) -> PendingTable {
    let mut rows = Vec::with_capacity(run.len());
    let mut lines = Vec::with_capacity(run.len());
    let mut bbox: Option<Rect> = None;
    for row in run {
        let mut cells = Vec::with_capacity(row.len());
        let mut row_words: Vec<Word> = Vec::new();
        for &fi in row {
            let f = &mut frags[fi];
            bbox = Some(bbox.map_or(f.bbox, |b| b.union(&f.bbox)));
            let text: Vec<&str> = f.words.iter().map(|w| w.text.as_str()).collect();
            cells.push(super::Cell {
                bbox: f.bbox,
                text: text.join(" "),
            });
            row_words.extend(std::mem::take(&mut f.words));
        }
        lines.push(make_line(row_words));
        rows.push(cells);
    }
    PendingTable {
        table: Table {
            bbox: bbox.unwrap_or_default(),
            rows,
            lines: Vec::new(),
        },
        lines,
    }
}

/// Nombre de lignes de base distinctes (à 1 pt près) parmi des éléments ;
/// un tableau compte pour deux.
fn distinct_baselines(items: &[Item], idx: &[usize]) -> usize {
    let mut bases: Vec<f64> = Vec::with_capacity(idx.len());
    let mut count = 0;
    for &i in idx {
        match items[i].kind {
            ItemKind::Table(_) => count += 2,
            ItemKind::Frag(_) => bases.push(items[i].base),
        }
    }
    bases.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut last: Option<f64> = None;
    for b in bases {
        if last.is_none_or(|l| (b - l).abs() > 1.0) {
            count += 1;
            last = Some(b);
        }
    }
    count
}

/// Découpe XY récursive (voir la documentation du module).
fn xy_cut(items: &[Item], idx: Vec<usize>, col: Rect, depth: usize, out: &mut Vec<Leaf>) {
    if idx.len() <= 1 || depth > 48 {
        out.push(Leaf { items: idx, col });
        return;
    }
    let mut sizes: Vec<f64> = idx.iter().map(|&i| items[i].size).collect();
    let size = median(&mut sizes).unwrap_or(10.0).max(1.0);

    // Bande verticale la plus large.
    let min_gap_x = (0.8 * size).max(5.0);
    let mut xs: Vec<(f64, f64)> = idx
        .iter()
        .map(|&i| (items[i].bbox.x0, items[i].bbox.x1))
        .collect();
    xs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut best: Option<(f64, f64)> = None;
    let mut cur_end = xs[0].1;
    for &(x0, x1) in &xs[1..] {
        if x0 - cur_end >= min_gap_x {
            let left: Vec<usize> = idx
                .iter()
                .copied()
                .filter(|&i| items[i].bbox.x1 <= cur_end + 1e-6)
                .collect();
            let right: Vec<usize> = idx
                .iter()
                .copied()
                .filter(|&i| items[i].bbox.x1 > cur_end + 1e-6)
                .collect();
            let valid =
                distinct_baselines(items, &left) >= 2 && distinct_baselines(items, &right) >= 2;
            if valid && best.is_none_or(|(a, b)| x0 - cur_end > b - a) {
                best = Some((cur_end, x0));
            }
        }
        cur_end = cur_end.max(x1);
    }
    if let Some((gx0, _)) = best {
        let (left, right): (Vec<usize>, Vec<usize>) =
            idx.iter().partition(|&&i| items[i].bbox.x1 <= gx0 + 1e-6);
        let bbox_of = |v: &[usize]| {
            v.iter()
                .skip(1)
                .fold(items[v[0]].bbox, |r, &i| r.union(&items[i].bbox))
        };
        let (lc, rc) = (bbox_of(&left), bbox_of(&right));
        xy_cut(items, left, lc, depth + 1, out);
        xy_cut(items, right, rc, depth + 1, out);
        return;
    }

    // Bande horizontale la plus haute.
    let min_gap_y = (0.7 * size).max(3.0);
    let mut ys: Vec<(f64, f64)> = idx
        .iter()
        .map(|&i| (items[i].bbox.y0, items[i].bbox.y1))
        .collect();
    ys.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut best: Option<(f64, f64)> = None;
    let mut cur_bottom = ys[0].0;
    for &(y0, y1) in &ys[1..] {
        let gap = cur_bottom - y1;
        if gap >= min_gap_y && best.is_none_or(|(g, _)| gap > g) {
            best = Some((gap, f64::midpoint(cur_bottom, y1)));
        }
        cur_bottom = cur_bottom.min(y0);
    }
    if let Some((_, cut)) = best {
        let (top, bottom): (Vec<usize>, Vec<usize>) =
            idx.iter().partition(|&&i| items[i].bbox.y0 >= cut);
        if !top.is_empty() && !bottom.is_empty() {
            xy_cut(items, top, col, depth + 1, out);
            xy_cut(items, bottom, col, depth + 1, out);
            return;
        }
    }
    out.push(Leaf { items: idx, col });
}

/// Ligne enrichie pour la construction des paragraphes.
struct LineInfo {
    index: usize,
    x0: f64,
    x1: f64,
    base: f64,
    size: f64,
    angle: f64,
    bbox: Rect,
    marker: Option<String>,
    /// Début du texte après le marqueur (ou `x0`).
    text_start: f64,
    /// Ligne sans le marqueur.
    text: String,
    starts_lower: bool,
}

/// Marqueurs de liste à un caractère.
const BULLETS: &[char] = &[
    '•', '·', '◦', '▪', '■', '●', '○', '‣', '⁃', '-', '–', '—', '*', '»', '>',
];

/// Marqueur de liste textuel : puce ou numérotation (« 1. », « a) », « (2) », « iv. »).
fn text_marker(word: &str) -> bool {
    let mut chars = word.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return BULLETS.contains(&c);
    }
    let inner = word.strip_prefix('(').unwrap_or(word);
    let Some(body) = inner.strip_suffix(['.', ')']) else {
        return false;
    };
    if body.is_empty() || body.len() > 4 {
        return false;
    }
    let digits = body.chars().all(|c| c.is_ascii_digit());
    let letter = body.chars().count() == 1 && body.chars().all(|c| c.is_ascii_alphabetic());
    let roman = body.chars().all(|c| "ivxlcIVXLC".contains(c));
    digits || letter || roman
}

fn line_info(index: usize, line: &Line, marks: &[Rect]) -> LineInfo {
    let size = line.size();
    let base = line.baseline();
    let mut marker = None;
    let mut first_word = 0;
    if line.words.len() >= 2 && text_marker(&line.words[0].text) {
        marker = Some(line.words[0].text.clone());
        first_word = 1;
    } else {
        // Puce dessinée juste à gauche de la ligne.
        let hit = marks.iter().any(|m| {
            let cy = f64::midpoint(m.y0, m.y1);
            cy >= line.bbox.y0
                && cy <= line.bbox.y1
                && m.x1 <= line.bbox.x0 + 0.1 * size
                && line.bbox.x0 - m.x1 <= 2.0 * size
        });
        if hit {
            marker = Some("•".into());
        }
    }
    let body = &line.words[first_word..];
    let text: Vec<&str> = body.iter().map(|w| w.text.as_str()).collect();
    let text = text.join(" ");
    LineInfo {
        index,
        x0: line.bbox.x0,
        x1: line.bbox.x1,
        base,
        size,
        angle: line.angle(),
        bbox: line.bbox,
        marker,
        text_start: body.first().map_or(line.bbox.x0, |w| w.bbox.x0),
        starts_lower: text.chars().next().is_some_and(char::is_lowercase),
        text,
    }
}

/// Fusionne les fragments d'une feuille par ligne de base, en lignes triées de haut en bas.
fn merge_lines(group: Vec<Frag>) -> Vec<Line> {
    let mut merged: Vec<(f64, f64, f64, Vec<Word>)> = Vec::new(); // (base, angle, size, mots)
    for f in group {
        let tol = (0.5 * f.size).max(1.0);
        match merged
            .iter_mut()
            .find(|(b, a, _, _)| (*b - f.base).abs() <= tol && (*a - f.angle).abs() < 0.035)
        {
            Some((_, _, _, ws)) => ws.extend(f.words),
            None => merged.push((f.base, f.angle, f.size, f.words)),
        }
    }
    let mut lines: Vec<Line> = merged
        .into_iter()
        .map(|(_, _, _, mut ws)| {
            ws.sort_by(|a, b| {
                let ka = a.glyphs.first().map_or(a.bbox.x0, |g| g.along);
                let kb = b.glyphs.first().map_or(b.bbox.x0, |g| g.along);
                ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
            });
            make_line(ws)
        })
        .collect();
    lines.sort_by(|a, b| {
        b.bbox
            .y1
            .partial_cmp(&a.bbox.y1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    lines
}

/// Paragraphe en construction.
struct Building {
    lines: Vec<usize>, // indices dans `infos`
    marker: Option<String>,
    text_start: f64,
    size: f64,
}

impl Building {
    fn pitch(&self, infos: &[LineInfo]) -> Option<f64> {
        if self.lines.len() < 2 {
            return None;
        }
        let mut d: Vec<f64> = self
            .lines
            .windows(2)
            .map(|w| infos[w[0]].base - infos[w[1]].base)
            .collect();
        median(&mut d)
    }

    fn body_left(&self, infos: &[LineInfo]) -> f64 {
        if self.lines.len() >= 2 {
            self.lines[1..]
                .iter()
                .map(|&i| infos[i].x0)
                .fold(f64::MAX, f64::min)
        } else {
            infos[self.lines[0]].x0
        }
    }
}

/// Faut-il ouvrir un nouveau paragraphe avant `cur` ?
fn breaks_paragraph(
    para: &Building,
    infos: &[LineInfo],
    cur: &LineInfo,
    leaf_pitch: Option<f64>,
    col_right: f64,
) -> bool {
    let prev = &infos[para.lines[para.lines.len() - 1]];
    if cur.marker.is_some() || (cur.angle - prev.angle).abs() > 0.05 {
        return true;
    }
    let ratio = cur.size / para.size.max(0.1);
    if !(0.85..=1.18).contains(&ratio) {
        return true;
    }
    let size = para.size.max(1.0);
    let reference = para.pitch(infos).or(leaf_pitch).unwrap_or(1.25 * size);
    if prev.base - cur.base > 1.5 * reference + 0.05 * size {
        return true;
    }
    if para.marker.is_some() {
        // Élément de liste : la suite s'aligne sur le texte après le marqueur.
        return (cur.x0 - para.text_start).abs() > 0.6 * size;
    }
    let body_left = para.body_left(infos);
    if cur.x0 - body_left > 0.8 * size {
        return true;
    }
    if para.lines.len() >= 2 && body_left - cur.x0 > 0.8 * size {
        return true;
    }
    col_right - prev.x1 > 2.5 * size && ends_sentence(&prev.text) && !cur.starts_lower
}

/// Vrai si une ligne qui finit par `last` et la suivante, qui commence par
/// `next_first`, sont les deux moitiés d'un mot coupé en fin de ligne : le
/// tiret disparaît à la jonction.
///
/// Un trait d'union conditionnel (U+00AD) n'est là que pour la coupure : il
/// part toujours. Un tiret ordinaire ne part que si la suite commence par une
/// minuscule — « docu- / mentation » est une césure, « Jean- / Pierre » un
/// prénom composé. La recherche (`search`) applique cette même règle au
/// texte qu'elle parcourt : on trouve ce qu'on lit.
pub(super) fn is_hyphen_break(last: char, next_first: Option<char>) -> bool {
    last == '\u{ad}'
        || (matches!(last, '-' | '\u{2010}' | '\u{2011}')
            && next_first.is_some_and(char::is_lowercase))
}

/// Joint deux lignes en réparant une césure (voir [`is_hyphen_break`]).
pub(super) fn join_lines(acc: &mut String, next: &str) {
    if acc.is_empty() {
        acc.push_str(next);
        return;
    }
    if acc
        .chars()
        .last()
        .is_some_and(|last| is_hyphen_break(last, next.chars().next()))
    {
        acc.pop();
        acc.push_str(next);
    } else {
        acc.push(' ');
        acc.push_str(next);
    }
}

fn finish_paragraph(b: &Building, infos: &[LineInfo], col: Rect) -> Paragraph {
    let lines: Vec<&LineInfo> = b.lines.iter().map(|&i| &infos[i]).collect();
    let mut text = String::new();
    for l in &lines {
        join_lines(&mut text, &l.text);
    }
    let bbox = lines
        .iter()
        .skip(1)
        .fold(lines[0].bbox, |r, l| r.union(&l.bbox));
    let size = b.size;
    let tol = 0.35 * size;
    let n = lines.len();
    let lefts: Vec<f64> = lines.iter().map(|l| l.x0 - col.x0).collect();
    let rights: Vec<f64> = lines.iter().map(|l| col.x1 - l.x1).collect();
    let alignment = if n == 1 {
        let (l, r) = (lefts[0], rights[0]);
        if l < tol {
            Alignment::Left
        } else if r < tol {
            Alignment::Right
        } else if (l - r).abs() < (1.0 * size).max(0.1 * col.width()) {
            Alignment::Center
        } else {
            Alignment::Left
        }
    } else {
        let left_ok = lefts[1..].iter().all(|l| *l < tol);
        let right_ok = rights[..n - 1].iter().all(|r| *r < tol);
        let centered = lefts
            .iter()
            .zip(&rights)
            .all(|(l, r)| (l - r).abs() < 1.0 * size);
        // Deux lignes suffisent : la première touche le bord droit de la
        // colonne à un tiers de cadratin près, ce qu'un texte en drapeau ne
        // fait que par hasard — et alors la justifier ne la change pas.
        if left_ok && right_ok {
            Alignment::Justify
        } else if left_ok {
            Alignment::Left
        } else if right_ok {
            Alignment::Right
        } else if centered {
            Alignment::Center
        } else {
            Alignment::Left
        }
    };
    let first_line_indent = if n >= 2 {
        lines[0].x0 - lines[1..].iter().map(|l| l.x0).fold(f64::MAX, f64::min)
    } else {
        0.0
    };
    Paragraph {
        bbox,
        lines: lines.iter().map(|l| l.index).collect(),
        text,
        alignment,
        first_line_indent,
        line_spacing: b.pitch(infos).unwrap_or(0.0),
        size,
        marker: b.marker.clone(),
        heading: 0,
    }
}

/// Fragments d'une feuille → lignes ajoutées à `out_lines`, et bloc de paragraphes.
fn text_block(
    group: Vec<Frag>,
    col: Rect,
    page_pitch: f64,
    marks: &[Rect],
    out_lines: &mut Vec<Line>,
) -> Block {
    let lines = merge_lines(group);
    let first = out_lines.len();
    let infos: Vec<LineInfo> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| line_info(first + i, l, marks))
        .collect();
    out_lines.extend(lines);
    let leaf_bbox = infos
        .iter()
        .skip(1)
        .fold(infos[0].bbox, |r, l| r.union(&l.bbox));
    // Bornes de colonne : la feuille elle-même si elle est assez fournie, sinon
    // la colonne héritée de la découpe.
    let col = if infos.len() >= 3 { leaf_bbox } else { col };
    let leaf_pitch = {
        let mut d: Vec<f64> = infos
            .windows(2)
            .map(|w| w[0].base - w[1].base)
            .filter(|d| *d > 0.0 && *d <= 3.0 * w_size(&infos))
            .collect();
        if d.len() >= 2 {
            median(&mut d).filter(|p| *p <= 2.5 * w_size(&infos))
        } else {
            Some(page_pitch).filter(|p| *p <= 2.5 * w_size(&infos))
        }
    };
    let mut paragraphs = Vec::new();
    let mut current: Option<Building> = None;
    for (i, info) in infos.iter().enumerate() {
        let start_new = match &current {
            None => true,
            Some(b) => breaks_paragraph(b, &infos, info, leaf_pitch, col.x1),
        };
        if start_new {
            if let Some(b) = current.take() {
                paragraphs.push(finish_paragraph(&b, &infos, col));
            }
            current = Some(Building {
                lines: vec![i],
                marker: info.marker.clone(),
                text_start: info.text_start,
                size: info.size,
            });
        } else if let Some(b) = &mut current {
            b.lines.push(i);
            let mut sizes: Vec<f64> = b.lines.iter().map(|&j| infos[j].size).collect();
            b.size = median(&mut sizes).unwrap_or(info.size);
        }
    }
    if let Some(b) = current.take() {
        paragraphs.push(finish_paragraph(&b, &infos, col));
    }
    Block {
        kind: BlockKind::Body,
        bbox: leaf_bbox,
        paragraphs,
    }
}

/// Taille médiane des lignes d'une feuille.
fn w_size(infos: &[LineInfo]) -> f64 {
    let mut s: Vec<f64> = infos.iter().map(|l| l.size).collect();
    median(&mut s).unwrap_or(10.0).max(1.0)
}

/// Niveau de titre d'un paragraphe (0 = corps).
fn heading_level(p: &Paragraph, med: f64) -> u8 {
    if med <= 0.0
        || p.lines.len() > 2
        || p.marker.is_some()
        || p.text.split_whitespace().count() > 20
    {
        return 0;
    }
    let ratio = p.size / med;
    if ratio >= 1.6 {
        1
    } else if ratio >= 1.3 {
        2
    } else if ratio >= 1.15 {
        3
    } else {
        0
    }
}

/// « 3 », « Page 3 », « 3 / 10 », « - 3 - », « p. 3 »…
pub(super) fn looks_like_page_number(text: &str) -> bool {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.is_empty() || tokens.len() > 4 {
        return false;
    }
    let mut digits = false;
    for t in tokens {
        let t = t.trim_matches(|c: char| matches!(c, '-' | '–' | '—' | '.' | ',' | '/' | '|'));
        if t.is_empty() {
            continue;
        }
        if t.len() <= 4 && t.chars().all(|c| c.is_ascii_digit()) {
            digits = true;
            continue;
        }
        let lower = t.to_lowercase();
        if !matches!(
            lower.as_str(),
            "page" | "p" | "pg" | "of" | "sur" | "de" | "seite"
        ) {
            return false;
        }
    }
    digits
}

/// Nombre de lignes et taille médiane d'un bloc de texte.
fn block_lines_and_size(b: &Block) -> (usize, f64) {
    let n = b.paragraphs.iter().map(|p| p.lines.len()).sum();
    let mut s: Vec<f64> = b.paragraphs.iter().map(|p| p.size).collect();
    (n, median(&mut s).unwrap_or(0.0))
}

fn block_text(b: &Block) -> String {
    let parts: Vec<&str> = b.paragraphs.iter().map(|p| p.text.as_str()).collect();
    parts.join(" ")
}

/// Marque les en-têtes et pieds de page d'une page isolée.
fn mark_headers_footers(blocks: &mut [Block], page: Rect, med: f64, pitch: f64) {
    if page.is_empty() || blocks.len() < 2 {
        return;
    }
    let zone = 0.10 * page.height();
    let candidate = |b: &Block| {
        let (n, size) = block_lines_and_size(b);
        matches!(b.kind, BlockKind::Body) && n > 0 && n <= 2 && size <= 1.1 * med
    };
    // En-tête : premier(s) bloc(s) dans la zone haute.
    for i in 0..blocks.len() {
        let b = &blocks[i];
        if !candidate(b) || b.bbox.y1 < page.y1 - zone {
            break;
        }
        let isolated = blocks
            .get(i + 1)
            .is_none_or(|next| b.bbox.y0 - next.bbox.y1 >= 1.5 * pitch);
        if isolated || looks_like_page_number(&block_text(b)) {
            blocks[i].kind = BlockKind::Header;
        } else {
            break;
        }
    }
    for i in (0..blocks.len()).rev() {
        let b = &blocks[i];
        if !candidate(b) || b.bbox.y0 > page.y0 + zone {
            break;
        }
        let isolated = i == 0 || blocks[i - 1].bbox.y0 - b.bbox.y1 >= 1.5 * pitch;
        if isolated || looks_like_page_number(&block_text(b)) {
            blocks[i].kind = BlockKind::Footer;
        } else {
            break;
        }
    }
}

/// Texte normalisé pour comparer des en-têtes d'une page à l'autre (chiffres → `#`).
fn normalized(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            out.push('#');
        } else if c.is_whitespace() {
            if !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out.trim().to_string()
}

/// Marque `Header`/`Footer` les blocs courts (≤ 2 lignes) situés dans les 20 %
/// haut ou bas de la page et répétés, au numéro près, sur au moins deux pages.
pub fn mark_repeated_headers(pages: &mut [PageText]) {
    if pages.len() < 2 {
        return;
    }
    let mut counts: std::collections::HashMap<(bool, String), usize> =
        std::collections::HashMap::new();
    let keys_of = |p: &PageText| -> Vec<(usize, (bool, String))> {
        if p.page_box.is_empty() {
            return Vec::new();
        }
        let zone = 0.2 * p.page_box.height();
        p.blocks
            .iter()
            .enumerate()
            .filter_map(|(i, b)| {
                let (n, _) = block_lines_and_size(b);
                if n == 0 || n > 2 || matches!(b.kind, BlockKind::Table(_)) {
                    return None;
                }
                let top = b.bbox.y1 >= p.page_box.y1 - zone;
                let bottom = b.bbox.y0 <= p.page_box.y0 + zone;
                if top == bottom {
                    return None;
                }
                let text = normalized(&block_text(b));
                (!text.is_empty() && text.len() <= 160).then_some((i, (top, text)))
            })
            .collect()
    };
    for p in pages.iter() {
        let mut seen = std::collections::HashSet::new();
        for (_, key) in keys_of(p) {
            if seen.insert(key.clone()) {
                *counts.entry(key).or_insert(0) += 1;
            }
        }
    }
    for p in pages.iter_mut() {
        for (i, key) in keys_of(p) {
            if counts.get(&key).copied().unwrap_or(0) >= 2 {
                p.blocks[i].kind = if key.0 {
                    BlockKind::Header
                } else {
                    BlockKind::Footer
                };
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::super::test_support::page;
    use super::*;

    fn texts(p: &PageText) -> Vec<String> {
        p.blocks
            .iter()
            .flat_map(|b| b.paragraphs.iter().map(|p| p.text.clone()))
            .collect()
    }

    #[test]
    fn paragraphs_spacing_indent_and_hyphen() {
        // Deux paragraphes de 12 pt, interligne 14 : le second commence par un
        // retrait et le premier se termine par une césure « docu-mentation ».
        let mut p = page(&[
            (
                50.0,
                700.0,
                12.0,
                "Premiere ligne du premier paragraphe docu-",
            ),
            (50.0, 686.0, 12.0, "mentation suite et fin du paragraphe."),
            (
                70.0,
                660.0,
                12.0,
                "Second paragraphe avec retrait de premiere",
            ),
            (
                50.0,
                646.0,
                12.0,
                "ligne qui continue ici sans s'arreter du tout.",
            ),
            (50.0, 632.0, 12.0, "Et encore une ligne."),
        ]);
        p.analyze();
        let t = texts(&p);
        assert_eq!(t.len(), 2, "{t:?}");
        assert_eq!(
            t[0],
            "Premiere ligne du premier paragraphe documentation suite et fin du paragraphe."
        );
        assert!(
            t[1].starts_with("Second paragraphe") && t[1].ends_with("Et encore une ligne."),
            "{t:?}"
        );
        let para = p
            .blocks
            .iter()
            .flat_map(|b| b.paragraphs.iter())
            .nth(1)
            .unwrap();
        assert!((para.first_line_indent - 20.0).abs() < 0.01, "{para:?}");
        assert!((para.line_spacing - 14.0).abs() < 0.01);
        assert_eq!(para.lines.len(), 3);
        let plain = p.to_plain();
        assert!(plain.contains("paragraphe.\n\nSecond"), "{plain:?}");
    }

    #[test]
    fn font_size_change_starts_paragraph_and_heading() {
        let mut p = page(&[
            (50.0, 700.0, 24.0, "Titre"),
            (50.0, 680.0, 12.0, "Corps de texte premiere ligne"),
            (50.0, 666.0, 12.0, "corps de texte seconde ligne."),
        ]);
        p.analyze();
        let t = texts(&p);
        assert_eq!(t.len(), 2, "{t:?}");
        assert_eq!(p.blocks[0].paragraphs[0].heading, 1);
        assert!(
            p.to_markdown().starts_with("# Titre\n"),
            "{}",
            p.to_markdown()
        );
    }

    #[test]
    fn two_columns_read_left_then_right() {
        let mut p = page(&[
            (50.0, 700.0, 10.0, "Gauche un deux trois"),
            (300.0, 700.0, 10.0, "Droite un deux trois"),
            (50.0, 688.0, 10.0, "gauche quatre cinq six"),
            (300.0, 688.0, 10.0, "droite quatre cinq six"),
            (50.0, 676.0, 10.0, "gauche sept huit."),
            (300.0, 676.0, 10.0, "droite sept huit."),
        ]);
        p.analyze();
        let t = texts(&p);
        assert_eq!(t.len(), 2, "{t:?}");
        assert_eq!(
            t[0],
            "Gauche un deux trois gauche quatre cinq six gauche sept huit."
        );
        assert_eq!(
            t[1],
            "Droite un deux trois droite quatre cinq six droite sept huit."
        );
        assert_eq!(p.lines.len(), 6);
        assert!(p.lines[0].text().starts_with("Gauche") && p.lines[3].text().starts_with("Droite"));
        assert_eq!(p.blocks[0].paragraphs[0].alignment, Alignment::Left);
    }

    #[test]
    fn paragraph_continues_across_columns() {
        let mut p = page(&[
            (50.0, 700.0, 10.0, "Gauche un deux trois quatre"),
            (300.0, 700.0, 10.0, "suite de la phrase a droite."),
            (50.0, 688.0, 10.0, "gauche cinq six sept huit et"),
            (300.0, 664.0, 10.0, "Nouveau paragraphe a droite."),
        ]);
        p.analyze();
        let t = texts(&p);
        assert_eq!(t.len(), 2, "{t:?}");
        assert_eq!(
            t[0],
            "Gauche un deux trois quatre gauche cinq six sept huit et suite de la phrase a droite."
        );
        assert_eq!(t[1], "Nouveau paragraphe a droite.");
    }

    #[test]
    fn header_footer_and_page_number() {
        let mut p = page(&[
            (50.0, 780.0, 9.0, "Rapport annuel"),
            (50.0, 600.0, 12.0, "Corps de la page premiere ligne"),
            (50.0, 586.0, 12.0, "corps seconde ligne."),
            (280.0, 20.0, 9.0, "Page 3"),
        ]);
        p.analyze();
        let kinds: Vec<BlockKind> = p.blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            vec![BlockKind::Header, BlockKind::Body, BlockKind::Footer],
            "{kinds:?}"
        );
        assert!(looks_like_page_number("- 12 -") && looks_like_page_number("3 / 10"));
        assert!(!looks_like_page_number("Chapitre 3 les origines"));
    }

    #[test]
    fn repeated_headers_across_pages() {
        let mut pages = vec![
            page(&[
                (50.0, 780.0, 10.0, "Manuel v1"),
                (50.0, 600.0, 12.0, "Texte page un."),
                (50.0, 30.0, 10.0, "1"),
            ]),
            page(&[
                (50.0, 780.0, 10.0, "Manuel v1"),
                (50.0, 600.0, 12.0, "Texte page deux."),
                (50.0, 30.0, 10.0, "2"),
            ]),
        ];
        for p in &mut pages {
            p.analyze();
            // Sans isolement (une seule page), on force Body pour tester la répétition.
            for b in &mut p.blocks {
                b.kind = BlockKind::Body;
            }
        }
        mark_repeated_headers(&mut pages);
        for p in &pages {
            let kinds: Vec<BlockKind> = p.blocks.iter().map(|b| b.kind).collect();
            assert_eq!(
                kinds,
                vec![BlockKind::Header, BlockKind::Body, BlockKind::Footer],
                "{kinds:?}"
            );
        }
    }

    #[test]
    fn bullets_and_numbered_lists() {
        let mut p = page(&[
            (50.0, 700.0, 12.0, "Liste :"),
            (50.0, 686.0, 12.0, "• premier element sur une ligne"),
            (50.0, 672.0, 12.0, "• second element qui est long et"),
            (62.0, 658.0, 12.0, "continue sur la ligne suivante"),
            (50.0, 644.0, 12.0, "1. numero un"),
            (50.0, 630.0, 12.0, "2. numero deux"),
        ]);
        p.analyze();
        let paras: Vec<&Paragraph> = p.blocks.iter().flat_map(|b| b.paragraphs.iter()).collect();
        assert_eq!(paras.len(), 5, "{:?}", texts(&p));
        assert_eq!(paras[1].marker.as_deref(), Some("•"));
        assert_eq!(
            paras[2].text,
            "second element qui est long et continue sur la ligne suivante"
        );
        assert_eq!(paras[3].marker.as_deref(), Some("1."));
        let md = p.to_markdown();
        assert!(
            md.contains("- premier element sur une ligne\n- second element"),
            "{md}"
        );
        assert!(md.contains("1. numero un\n2. numero deux"), "{md}");
    }

    #[test]
    fn alignment_detection() {
        let mut p = page(&[
            (
                50.0,
                700.0,
                10.0,
                "aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa",
            ),
            (
                50.0,
                688.0,
                10.0,
                "aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa",
            ),
            (
                50.0,
                676.0,
                10.0,
                "aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa",
            ),
            (50.0, 664.0, 10.0, "aaaa aaaa."),
            (241.0, 630.0, 10.0, "Centre"),
            (300.0, 600.0, 10.0, "Droite droite droite droite droite"),
        ]);
        // Trois lignes pleines puis une courte : justifié ; « Centre » centré ; dernière à droite.
        p.analyze();
        let paras: Vec<&Paragraph> = p.blocks.iter().flat_map(|b| b.paragraphs.iter()).collect();
        assert_eq!(paras[0].alignment, Alignment::Justify, "{:?}", paras[0]);
        let centre = paras.iter().find(|p| p.text == "Centre").unwrap();
        assert_eq!(centre.alignment, Alignment::Center);
        let droite = paras.iter().find(|p| p.text.starts_with("Droite")).unwrap();
        assert_eq!(droite.alignment, Alignment::Right);
    }
}
