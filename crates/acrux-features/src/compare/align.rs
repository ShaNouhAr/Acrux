//! Alignement : le cœur de la comparaison.
//!
//! Deux alignements sont en jeu, et le même schéma sert aux deux :
//!
//! 1. **les mots d'une page** : plus longue sous-suite commune (LCS) exacte
//!    par programmation dynamique, avec un repli linéaire (ancres uniques,
//!    façon « patience diff ») quand la page est trop grosse pour une matrice
//!    n × m ;
//! 2. **les pages d'un document** : alignement global de deux suites
//!    (Needleman-Wunsch) sur une similarité de contenu, qui autorise
//!    l'insertion et la suppression de pages.
//!
//! Les deux rendent la même chose : des **paires d'indices croissantes**
//! `(i, j)`. Tout ce qui n'est pas dans une paire est une différence, ce qui
//! réduit la construction du rapport à une seule fonction ([`changes`]).

use std::collections::HashMap;
use std::ops::Range;

use acrux_core::Rect;

use crate::text::PageText;

/// Profondeur maximale de la découpe par ancres, pour borner la récursion sur
/// des pages pathologiques (le repli ne descend jamais très bas en pratique).
const MAX_ANCHOR_DEPTH: usize = 12;

/// Un mot positionné : l'unité de la comparaison de texte.
#[derive(Debug, Clone)]
pub struct Token {
    /// Texte du mot tel qu'il est écrit dans le document.
    pub text: String,
    /// Texte normalisé, seul utilisé pour décider de l'égalité (voir [`normalize`]).
    pub key: String,
    /// Boîte englobante du mot, en espace utilisateur PDF.
    pub rect: Rect,
    /// Indice de la ligne dans `PageText::lines` (sert à regrouper les
    /// rectangles : une différence qui court sur trois lignes donne trois
    /// rectangles, pas un bloc qui déborde).
    pub line: usize,
}

/// Normalise un mot avant comparaison.
///
/// L'extraction restitue fidèlement ce que le producteur du PDF a écrit :
/// apostrophes typographiques, tirets longs, espaces insécables, ligatures.
/// Deux fichiers issus du même texte mais de deux moteurs différents ne
/// doivent pas se retrouver « tout changé » pour autant, d'où ce repliage.
#[must_use]
pub fn normalize(text: &str, ignore_case: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\u{FB00}' => out.push_str("ff"),
            '\u{FB01}' => out.push_str("fi"),
            '\u{FB02}' => out.push_str("fl"),
            '\u{FB03}' => out.push_str("ffi"),
            '\u{FB04}' => out.push_str("ffl"),
            _ => {
                let c = match c {
                    '\u{2018}' | '\u{2019}' | '\u{201B}' | '\u{02BC}' => '\'',
                    '\u{201C}' | '\u{201D}' | '\u{201E}' => '"',
                    '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
                    '\u{00A0}' | '\u{2007}' | '\u{2009}' | '\u{202F}' => ' ',
                    other => other,
                };
                if ignore_case {
                    out.extend(c.to_lowercase());
                } else {
                    out.push(c);
                }
            }
        }
    }
    out.trim().to_string()
}

/// Mots d'une page dans l'ordre de lecture, prêts à être alignés.
///
/// Les mots dont la clé est vide (ponctuation d'espacement seule) sont
/// écartés : ils n'apportent rien et bruitent l'alignement.
#[must_use]
pub fn tokens(text: &PageText, ignore_case: bool) -> Vec<Token> {
    let mut out = Vec::new();
    for (line, l) in text.lines.iter().enumerate() {
        for w in &l.words {
            let key = normalize(&w.text, ignore_case);
            if key.is_empty() {
                continue;
            }
            out.push(Token {
                text: w.text.clone(),
                key,
                rect: w.bbox,
                line,
            });
        }
    }
    out
}

/// Plus longue sous-suite commune exacte, par programmation dynamique.
///
/// Mémoire : un octet de direction par cellule (`n × m`), plus deux lignes de
/// compteurs. C'est pour cette mémoire que l'appelant borne `n × m`.
fn lcs_pairs(left: &[&str], right: &[&str]) -> Vec<(usize, usize)> {
    let (rows, cols) = (left.len(), right.len());
    if rows == 0 || cols == 0 {
        return Vec::new();
    }
    // 0 = mot commun (diagonale), 1 = avancer à gauche, 2 = avancer à droite.
    let mut dir = vec![0u8; rows * cols];
    let mut prev = vec![0u32; cols + 1];
    let mut cur = vec![0u32; cols + 1];
    for i in 0..rows {
        cur[0] = 0;
        for j in 0..cols {
            if left[i] == right[j] {
                cur[j + 1] = prev[j] + 1;
                dir[i * cols + j] = 0;
            } else if prev[j + 1] >= cur[j] {
                cur[j + 1] = prev[j + 1];
                dir[i * cols + j] = 1;
            } else {
                cur[j + 1] = cur[j];
                dir[i * cols + j] = 2;
            }
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (rows, cols);
    while i > 0 && j > 0 {
        match dir[(i - 1) * cols + (j - 1)] {
            0 => {
                out.push((i - 1, j - 1));
                i -= 1;
                j -= 1;
            }
            1 => i -= 1,
            _ => j -= 1,
        }
    }
    out.reverse();
    out
}

/// Mots présents **exactement une fois** de chaque côté : ce sont des points
/// d'ancrage sûrs (un mot rare qui ne se répète pas ne peut guère être
/// apparié autrement).
fn unique_pairs(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let mut counts: HashMap<&str, (usize, usize, usize, usize)> = HashMap::new();
    for (i, k) in a.iter().enumerate() {
        let e = counts.entry(k).or_insert((0, 0, 0, 0));
        e.0 += 1;
        e.1 = i;
    }
    for (j, k) in b.iter().enumerate() {
        let e = counts.entry(k).or_insert((0, 0, 0, 0));
        e.2 += 1;
        e.3 = j;
    }
    let mut out: Vec<(usize, usize)> = counts
        .values()
        .filter(|(na, _, nb, _)| *na == 1 && *nb == 1)
        .map(|(_, i, _, j)| (*i, *j))
        .collect();
    out.sort_unstable();
    out
}

/// Plus longue sous-suite d'ancres dont les deux indices croissent ensemble
/// (les ancres sont déjà triées sur `i`, il reste à rendre `j` croissant).
/// Tri par patiences : `O(k log k)`.
fn longest_increasing(pairs: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut tails: Vec<usize> = Vec::new();
    let mut previous = vec![usize::MAX; pairs.len()];
    for (idx, &(_, j)) in pairs.iter().enumerate() {
        let pos = tails.partition_point(|&t| pairs[t].1 < j);
        if pos > 0 {
            previous[idx] = tails[pos - 1];
        }
        if pos == tails.len() {
            tails.push(idx);
        } else {
            tails[pos] = idx;
        }
    }
    let mut out = Vec::new();
    let mut cursor = tails.last().copied();
    while let Some(i) = cursor {
        out.push(pairs[i]);
        cursor = (previous[i] != usize::MAX).then_some(previous[i]);
    }
    out.reverse();
    out
}

/// Apparie les mots de `a` et de `b` : paires `(i, j)` croissantes.
///
/// Tant que `a.len() × b.len()` tient dans `max_cells`, la LCS exacte est
/// employée. Au-delà, la zone est découpée sur les mots uniques communs puis
/// chaque morceau est traité récursivement : le coût redevient quasi linéaire
/// sur une page immense, au prix de quelques appariements manqués là où le
/// texte ne contient aucun mot rare.
#[must_use]
pub fn align_words(a: &[&str], b: &[&str], max_cells: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    align_range(a, b, max_cells, 0, (0, 0), &mut out);
    out
}

fn align_range(
    a: &[&str],
    b: &[&str],
    max_cells: usize,
    depth: usize,
    offset: (usize, usize),
    out: &mut Vec<(usize, usize)>,
) {
    // Préfixe et suffixe communs : gratuits, et ils suffisent souvent à ramener
    // le reste sous le seuil de la LCS exacte.
    let mut head = 0;
    while head < a.len() && head < b.len() && a[head] == b[head] {
        out.push((offset.0 + head, offset.1 + head));
        head += 1;
    }
    let mut tail = 0;
    while tail < a.len() - head
        && tail < b.len() - head
        && a[a.len() - 1 - tail] == b[b.len() - 1 - tail]
    {
        tail += 1;
    }
    let (a, b) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    let offset = (offset.0 + head, offset.1 + head);
    let push_tail = |out: &mut Vec<(usize, usize)>| {
        for t in 0..tail {
            out.push((offset.0 + a.len() + t, offset.1 + b.len() + t));
        }
    };
    if a.is_empty() || b.is_empty() {
        push_tail(out);
        return;
    }
    if a.len().saturating_mul(b.len()) <= max_cells || depth >= MAX_ANCHOR_DEPTH {
        for (i, j) in lcs_pairs(a, b) {
            out.push((offset.0 + i, offset.1 + j));
        }
        push_tail(out);
        return;
    }
    let anchors = longest_increasing(&unique_pairs(a, b));
    if anchors.is_empty() {
        // Aucun mot rare commun : le bloc entier est un remplacement.
        push_tail(out);
        return;
    }
    let (mut pa, mut pb) = (0, 0);
    for (i, j) in anchors {
        align_range(
            &a[pa..i],
            &b[pb..j],
            max_cells,
            depth + 1,
            (offset.0 + pa, offset.1 + pb),
            out,
        );
        out.push((offset.0 + i, offset.1 + j));
        (pa, pb) = (i + 1, j + 1);
    }
    align_range(
        &a[pa..],
        &b[pb..],
        max_cells,
        depth + 1,
        (offset.0 + pa, offset.1 + pb),
        out,
    );
    push_tail(out);
}

/// Une zone qui diffère : la plage de `a` remplacée par la plage de `b`.
/// L'une des deux peut être vide (ajout ou suppression pur).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Indices dans la suite de gauche.
    pub old: Range<usize>,
    /// Indices dans la suite de droite.
    pub new: Range<usize>,
}

/// Zones différentes déduites des paires appariées.
#[must_use]
pub fn changes(pairs: &[(usize, usize)], n: usize, m: usize) -> Vec<Change> {
    let mut out = Vec::new();
    let (mut a, mut b) = (0, 0);
    let push = |old: Range<usize>, new: Range<usize>, out: &mut Vec<Change>| {
        if !old.is_empty() || !new.is_empty() {
            out.push(Change { old, new });
        }
    };
    for &(i, j) in pairs {
        push(a..i, b..j, &mut out);
        (a, b) = (i + 1, j + 1);
    }
    push(a..n, b..m, &mut out);
    out
}

/// Sac de mots d'une page : chaque mot normalisé avec son nombre d'occurrences.
#[must_use]
pub fn word_bag(text: &PageText, ignore_case: bool) -> HashMap<String, usize> {
    let mut bag = HashMap::new();
    for line in &text.lines {
        for w in &line.words {
            let key = normalize(&w.text, ignore_case);
            if !key.is_empty() {
                *bag.entry(key).or_insert(0) += 1;
            }
        }
    }
    bag
}

/// Similarité de deux sacs de mots (coefficient de Sørensen-Dice sur les
/// multi-ensembles) : 1 = même contenu, 0 = rien en commun.
///
/// Deux pages vides sont réputées identiques : une page blanche insérée entre
/// deux pages blanches n'a pas de raison d'être signalée comme un changement
/// de texte (le mode visuel, lui, la verra).
#[must_use]
pub fn bag_similarity(a: &HashMap<String, usize>, b: &HashMap<String, usize>) -> f64 {
    let total: usize = a.values().sum::<usize>() + b.values().sum::<usize>();
    if total == 0 {
        return 1.0;
    }
    let common: usize = a
        .iter()
        .map(|(k, n)| b.get(k).map_or(0, |m| *n.min(m)))
        .sum();
    #[allow(clippy::cast_precision_loss)] // compteurs de mots : bien en deçà de 2^53
    let ratio = (2 * common) as f64 / total as f64;
    ratio
}

/// Alignement global de deux suites de pages sur leur similarité
/// (Needleman-Wunsch, pénalité de trou nulle) : rend, dans l'ordre, des
/// couples `(page de gauche, page de droite)` dont l'un des deux membres est
/// `None` quand la page n'a pas d'équivalent.
///
/// `similarity[i][j]` est la similarité de la page `i` de gauche avec la page
/// `j` de droite. Apparier deux pages rapporte `similarité − seuil` : deux
/// pages ne sont donc appariées que si elles se ressemblent plus que le seuil,
/// sinon il vaut mieux les déclarer supprimée et ajoutée.
#[must_use]
pub fn align_pages(similarity: &[Vec<f64>], threshold: f64) -> Vec<(Option<usize>, Option<usize>)> {
    let rows = similarity.len();
    let cols = similarity.first().map_or(0, Vec::len);
    let stride = cols + 1;
    let mut score = vec![0.0f64; (rows + 1) * stride];
    // 0 = appariement, 1 = page de gauche seule, 2 = page de droite seule.
    let mut dir = vec![1u8; (rows + 1) * stride];
    dir[..stride].fill(2);
    for i in 1..=rows {
        for j in 1..=cols {
            let diagonal = score[(i - 1) * stride + j - 1] + similarity[i - 1][j - 1] - threshold;
            let up = score[(i - 1) * stride + j];
            let left = score[i * stride + j - 1];
            // À score égal, la page de gauche seule (suppression) passe avant
            // la page de droite seule : la remontée se faisant à l'envers,
            // c'est le chemin « left » qu'il faut préférer ici.
            let (best, step) = if diagonal >= up && diagonal >= left {
                (diagonal, 0)
            } else if left >= up {
                (left, 2)
            } else {
                (up, 1)
            };
            score[i * stride + j] = best;
            dir[i * stride + j] = step;
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (rows, cols);
    while i > 0 || j > 0 {
        let step = if i == 0 {
            2
        } else if j == 0 {
            1
        } else {
            dir[i * stride + j]
        };
        match step {
            0 => {
                out.push((Some(i - 1), Some(j - 1)));
                i -= 1;
                j -= 1;
            }
            1 => {
                out.push((Some(i - 1), None));
                i -= 1;
            }
            _ => {
                out.push((None, Some(j - 1)));
                j -= 1;
            }
        }
    }
    out.reverse();
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::float_cmp)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(ToString::to_string).collect()
    }

    fn align(a: &str, b: &str, max_cells: usize) -> Vec<Change> {
        let (wa, wb) = (words(a), words(b));
        let ra: Vec<&str> = wa.iter().map(String::as_str).collect();
        let rb: Vec<&str> = wb.iter().map(String::as_str).collect();
        let pairs = align_words(&ra, &rb, max_cells);
        // L'alignement doit toujours être strictement croissant des deux côtés.
        for w in pairs.windows(2) {
            assert!(
                w[0].0 < w[1].0 && w[0].1 < w[1].1,
                "paires non croissantes : {pairs:?}"
            );
        }
        changes(&pairs, ra.len(), rb.len())
    }

    /// Rend les changements sous forme lisible « -supprimé +ajouté ».
    fn render(a: &str, b: &str, max_cells: usize) -> String {
        let (wa, wb) = (words(a), words(b));
        align(a, b, max_cells)
            .iter()
            .map(|c| {
                format!(
                    "[-{} +{}]",
                    wa[c.old.clone()].join(" "),
                    wb[c.new.clone()].join(" ")
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn identical_sequences_have_no_change() {
        assert!(align("un deux trois", "un deux trois", 1 << 20).is_empty());
        assert!(align("", "", 1 << 20).is_empty());
    }

    #[test]
    fn empty_side_is_one_change() {
        assert_eq!(render("un deux", "", 1 << 20), "[-un deux +]");
        assert_eq!(render("", "un deux", 1 << 20), "[- +un deux]");
    }

    #[test]
    fn insertion_deletion_and_replacement() {
        assert_eq!(render("a b d", "a b c d", 1 << 20), "[- +c]");
        assert_eq!(render("a b c d", "a b d", 1 << 20), "[-c +]");
        assert_eq!(render("a b c d", "a x d", 1 << 20), "[-b c +x]");
        assert_eq!(
            render("tout autre chose", "rien de commun", 1 << 20),
            "[-tout autre chose +rien de commun]"
        );
    }

    #[test]
    fn linear_fallback_finds_the_same_changes() {
        // `max_cells` à 0 force le repli par ancres sur chaque sous-bloc.
        let a = "le chat dort sur le tapis rouge du salon principal";
        let b = "le chat dort sur le grand tapis rouge du salon";
        assert_eq!(render(a, b, 1 << 20), "[- +grand] [-principal +]");
        assert_eq!(render(a, b, 0), "[- +grand] [-principal +]");
    }

    #[test]
    fn fallback_on_a_long_document_stays_exact_where_it_can() {
        // 5 000 mots dont un seul change : le repli doit trouver ce seul mot.
        let mut a: Vec<String> = (0..5000).map(|i| format!("mot{i}")).collect();
        let b = a.clone();
        a[2500] = "intrus".into();
        let ra: Vec<&str> = a.iter().map(String::as_str).collect();
        let rb: Vec<&str> = b.iter().map(String::as_str).collect();
        let ch = changes(&align_words(&ra, &rb, 0), ra.len(), rb.len());
        assert_eq!(ch.len(), 1, "{ch:?}");
        assert_eq!(ch[0].old, 2500..2501);
        assert_eq!(ch[0].new, 2500..2501);
    }

    #[test]
    fn normalisation_folds_typography() {
        assert_eq!(normalize("l\u{2019}été", false), "l'été");
        assert_eq!(normalize("\u{FB01}n", false), "fin");
        assert_eq!(normalize("Été", true), "été");
        assert_eq!(normalize("  mot\u{A0} ", false), "mot");
    }

    fn sim(pages: &[&[&str]], other: &[&[&str]]) -> Vec<Vec<f64>> {
        let bag = |w: &[&str]| {
            let mut b: HashMap<String, usize> = HashMap::new();
            for k in w {
                *b.entry((*k).to_string()).or_insert(0) += 1;
            }
            b
        };
        pages
            .iter()
            .map(|p| {
                other
                    .iter()
                    .map(|q| bag_similarity(&bag(p), &bag(q)))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn page_alignment_detects_an_inserted_page() {
        let old: Vec<&[&str]> = vec![&["a", "a"], &["b", "b"], &["c", "c"]];
        let new: Vec<&[&str]> = vec![&["a", "a"], &["x", "x"], &["b", "b"], &["c", "c"]];
        let pairs = align_pages(&sim(&old, &new), 0.5);
        assert_eq!(
            pairs,
            vec![
                (Some(0), Some(0)),
                (None, Some(1)),
                (Some(1), Some(2)),
                (Some(2), Some(3)),
            ]
        );
    }

    #[test]
    fn page_alignment_detects_a_deleted_page_and_empty_documents() {
        let old: Vec<&[&str]> = vec![&["a"], &["b"], &["c"]];
        let new: Vec<&[&str]> = vec![&["a"], &["c"]];
        assert_eq!(
            align_pages(&sim(&old, &new), 0.5),
            vec![(Some(0), Some(0)), (Some(1), None), (Some(2), Some(1))]
        );
        assert!(align_pages(&[], 0.5).is_empty());
        let only_old: Vec<&[&str]> = vec![&["a"]];
        let none: Vec<&[&str]> = vec![];
        assert_eq!(
            align_pages(&sim(&only_old, &none), 0.5),
            vec![(Some(0), None)]
        );
    }

    #[test]
    fn dissimilar_pages_are_not_paired() {
        let old: Vec<&[&str]> = vec![&["a", "b", "c", "d"]];
        let new: Vec<&[&str]> = vec![&["w", "x", "y", "z"]];
        assert_eq!(
            align_pages(&sim(&old, &new), 0.5),
            vec![(Some(0), None), (None, Some(0))],
            "rien en commun : une suppression et un ajout, pas un appariement"
        );
    }
}
