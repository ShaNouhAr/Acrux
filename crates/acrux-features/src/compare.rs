//! Comparaison de deux documents (inventaire Acrobat §8 « Comparer des
//! fichiers ») : ce qui a été ajouté, supprimé, remplacé ou déplacé, où, et
//! à quoi cela ressemble.
//!
//! Trois étages, indépendants et cumulables :
//!
//! 1. **les pages** sont appariées d'abord ([`align::align_pages`]), parce
//!    qu'une page insérée ou supprimée décalerait tout le reste. L'appariement
//!    se fait sur la similarité de contenu, pas sur le numéro de page, et les
//!    pages seulement déplacées sont reconnues comme telles ;
//! 2. **le texte** de chaque paire de pages est aligné mot à mot (plus longue
//!    sous-suite commune, repli linéaire sur les pages énormes) ; les zones
//!    non appariées deviennent des [`Difference`] situées par leur page et
//!    leurs rectangles dans **chacun** des deux documents ;
//! 3. **les pixels**, en option : les deux pages sont rendues et comparées
//!    avec une tolérance d'anticrénelage, ce qui rattrape tout ce que le texte
//!    ne voit pas (images, traits, couleurs, polices changées).
//!
//! [`write_report`] transforme le résultat en PDF côte à côte, et
//! [`Comparison::summary`] en texte pour un terminal.
//!
//! Limites connues, en toute franchise :
//!
//! - la comparaison de texte repose sur l'extraction ([`crate::text`]), donc
//!   sur un ordre de lecture purement géométrique : deux mises en page très
//!   différentes du même texte peuvent produire des différences de rang ;
//! - les annotations et les champs de formulaire ne sont **pas** comparés en
//!   tant que tels ; la comparaison de pixels les voit si leur apparence
//!   change, mais le rapport PDF ne les redessine pas (il reprend le flux de
//!   contenu des pages, pas leurs annotations) ;
//! - l'appariement des pages est monotone : une permutation de deux pages est
//!   rapportée comme **un** déplacement (l'une reste en place, l'autre bouge) ;
//! - le repli linéaire des pages énormes peut manquer quelques appariements
//!   dans les zones sans aucun mot rare commun ; il ne se déclenche qu'au-delà
//!   de `CompareOptions::max_cells`.

mod align;
mod report;
mod visual;

use std::collections::HashMap;

use acrux_core::{Rect, Result};
use acrux_document::{collect_pages, Document};
use acrux_render::{render_page, RenderOptions};

use crate::text::{extract_page_text, PageText};

pub use align::{normalize, tokens, Token};
pub use report::{write_report, ReportOptions};
pub use visual::{compare_bitmaps, PixelComparison, VisualOptions};

/// Nature d'une différence de texte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    /// Texte présent seulement dans le nouveau document.
    Added,
    /// Texte présent seulement dans l'ancien.
    Removed,
    /// Texte remplacé par un autre au même endroit.
    Replaced,
    /// Même texte, ailleurs (autre endroit de la page ou autre page).
    Moved,
}

impl DiffKind {
    /// Libellé français au singulier.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            DiffKind::Added => "ajout",
            DiffKind::Removed => "suppression",
            DiffKind::Replaced => "remplacement",
            DiffKind::Moved => "déplacement",
        }
    }
}

/// Une différence de texte, située dans les deux documents.
#[derive(Debug, Clone)]
pub struct Difference {
    /// Nature.
    pub kind: DiffKind,
    /// Page de l'ancien document (0 = première), si la différence y existe.
    pub old_page: Option<usize>,
    /// Page du nouveau document.
    pub new_page: Option<usize>,
    /// Texte concerné dans l'ancien document (vide pour un ajout).
    pub old_text: String,
    /// Texte concerné dans le nouveau (vide pour une suppression).
    pub new_text: String,
    /// Rectangles dans l'ancien document, un par ligne touchée.
    pub old_rects: Vec<Rect>,
    /// Rectangles dans le nouveau document, un par ligne touchée.
    pub new_rects: Vec<Rect>,
}

/// Appariement de deux pages.
#[derive(Debug, Clone, Copy)]
pub struct PagePair {
    /// Page de l'ancien document, `None` si la page a été ajoutée.
    pub old_page: Option<usize>,
    /// Page du nouveau document, `None` si la page a été supprimée.
    pub new_page: Option<usize>,
    /// Similarité de contenu, de 0 à 1.
    pub similarity: f64,
    /// La page a changé de rang dans le document.
    pub moved: bool,
}

/// Différence visuelle d'une paire de pages.
#[derive(Debug, Clone)]
pub struct VisualDiff {
    /// Zones changées, en espace utilisateur de la page du **nouveau**
    /// document (de l'ancienne page si la page a été supprimée).
    pub rects: Vec<Rect>,
    /// Part de pixels différents, de 0 à 1.
    pub changed_ratio: f64,
}

/// Résultat de la comparaison d'une paire de pages.
#[derive(Debug, Clone)]
pub struct PageDiff {
    /// Appariement.
    pub pair: PagePair,
    /// Différences de texte, dans l'ordre de lecture de la page.
    pub differences: Vec<Difference>,
    /// Comparaison des pixels, si elle a été demandée.
    pub visual: Option<VisualDiff>,
}

impl PageDiff {
    /// Vrai si rien ne distingue les deux pages.
    #[must_use]
    pub fn is_identical(&self) -> bool {
        self.differences.is_empty()
            && !self.pair.moved
            && self.pair.old_page.is_some()
            && self.pair.new_page.is_some()
            && self.visual.as_ref().is_none_or(|v| v.rects.is_empty())
    }
}

/// Comptes d'un rapport de comparaison.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiffCounts {
    /// Ajouts de texte.
    pub added: usize,
    /// Suppressions de texte.
    pub removed: usize,
    /// Remplacements.
    pub replaced: usize,
    /// Déplacements.
    pub moved: usize,
    /// Pages ajoutées.
    pub pages_added: usize,
    /// Pages supprimées.
    pub pages_removed: usize,
    /// Pages déplacées.
    pub pages_moved: usize,
}

impl DiffCounts {
    /// Nombre total de différences de texte.
    #[must_use]
    pub fn total(&self) -> usize {
        self.added + self.removed + self.replaced + self.moved
    }
}

/// Comptes des différences de texte d'une liste.
pub(crate) fn count_differences(differences: &[Difference]) -> DiffCounts {
    let mut c = DiffCounts::default();
    for d in differences {
        match d.kind {
            DiffKind::Added => c.added += 1,
            DiffKind::Removed => c.removed += 1,
            DiffKind::Replaced => c.replaced += 1,
            DiffKind::Moved => c.moved += 1,
        }
    }
    c
}

/// Comparaison complète de deux documents.
#[derive(Debug, Clone)]
pub struct Comparison {
    /// Paires de pages, dans l'ordre du nouveau document (les pages
    /// supprimées prennent la place qu'elles occupaient).
    pub pages: Vec<PageDiff>,
    /// Nombre de pages de l'ancien document.
    pub old_page_count: usize,
    /// Nombre de pages du nouveau.
    pub new_page_count: usize,
    /// Avertissements (page dont le texte n'a pas pu être extrait…).
    pub warnings: Vec<String>,
}

impl Comparison {
    /// Comptes de toutes les différences.
    #[must_use]
    pub fn counts(&self) -> DiffCounts {
        let mut total = DiffCounts::default();
        for page in &self.pages {
            let c = count_differences(&page.differences);
            total.added += c.added;
            total.removed += c.removed;
            total.replaced += c.replaced;
            total.moved += c.moved;
            match (page.pair.old_page, page.pair.new_page) {
                (None, Some(_)) => total.pages_added += 1,
                (Some(_), None) => total.pages_removed += 1,
                _ => {}
            }
            if page.pair.moved {
                total.pages_moved += 1;
            }
        }
        total
    }

    /// Vrai si les deux documents ne diffèrent en rien de ce qui a été comparé.
    #[must_use]
    pub fn is_identical(&self) -> bool {
        self.pages.iter().all(PageDiff::is_identical)
    }

    /// Toutes les différences, pages confondues.
    #[must_use]
    pub fn differences(&self) -> Vec<&Difference> {
        self.pages
            .iter()
            .flat_map(|p| p.differences.iter())
            .collect()
    }

    /// Résumé lisible pour un terminal : une ligne par page qui change, puis
    /// au plus `extracts` extraits par page.
    #[must_use]
    pub fn summary(&self, extracts: usize) -> String {
        use std::fmt::Write as _;
        let counts = self.counts();
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{} page(s) avant, {} après",
            self.old_page_count, self.new_page_count
        );
        if self.is_identical() {
            out.push_str("aucune différence\n");
            return out;
        }
        let _ = writeln!(
            out,
            "{} différence(s) : {} ajout(s), {} suppression(s), {} remplacement(s), {} déplacement(s)",
            counts.total(),
            counts.added,
            counts.removed,
            counts.replaced,
            counts.moved
        );
        if counts.pages_added + counts.pages_removed + counts.pages_moved > 0 {
            let _ = writeln!(
                out,
                "pages : {} ajoutée(s), {} supprimée(s), {} déplacée(s)",
                counts.pages_added, counts.pages_removed, counts.pages_moved
            );
        }
        for page in &self.pages {
            if page.is_identical() {
                continue;
            }
            let label =
                |i: Option<usize>| i.map_or_else(|| "-".to_string(), |v| (v + 1).to_string());
            let c = count_differences(&page.differences);
            let mut state = Vec::new();
            if page.pair.old_page.is_none() {
                state.push("page ajoutée".to_string());
            }
            if page.pair.new_page.is_none() {
                state.push("page supprimée".to_string());
            }
            if page.pair.moved {
                state.push("page déplacée".to_string());
            }
            if c.total() > 0 {
                state.push(format!("{} différence(s)", c.total()));
            }
            if let Some(v) = &page.visual {
                if v.changed_ratio > 0.0 {
                    state.push(format!("{:.2} % de pixels", v.changed_ratio * 100.0));
                }
            }
            let _ = writeln!(
                out,
                "\npage {} -> {} : {}",
                label(page.pair.old_page),
                label(page.pair.new_page),
                state.join(", ")
            );
            for d in page.differences.iter().take(extracts) {
                let text = match d.kind {
                    DiffKind::Added => format!("  + {}", short(&d.new_text)),
                    DiffKind::Removed => format!("  - {}", short(&d.old_text)),
                    DiffKind::Replaced => {
                        format!("  ~ {} -> {}", short(&d.old_text), short(&d.new_text))
                    }
                    DiffKind::Moved => format!(
                        "  <> {} (page {} -> {})",
                        short(&d.new_text),
                        label(d.old_page),
                        label(d.new_page)
                    ),
                };
                let _ = writeln!(out, "{text}");
            }
            if page.differences.len() > extracts {
                let _ = writeln!(
                    out,
                    "  ... et {} autre(s)",
                    page.differences.len() - extracts
                );
            }
        }
        out
    }
}

/// Extrait court d'un texte pour l'affichage.
fn short(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 72 {
        return flat;
    }
    let kept: String = flat.chars().take(71).collect();
    format!("{kept}…")
}

/// Réglages de la comparaison.
#[derive(Debug, Clone)]
pub struct CompareOptions {
    /// Ignorer la casse.
    pub ignore_case: bool,
    /// Reconnaître les blocs déplacés (même texte ailleurs) au lieu de les
    /// rapporter comme une suppression puis un ajout.
    pub detect_moves: bool,
    /// Nombre minimal de mots d'un bloc pour qu'un déplacement soit reconnu :
    /// en dessous, deux occurrences du même mot ailleurs dans le document
    /// donneraient de faux déplacements.
    pub move_min_words: usize,
    /// Taille maximale de la matrice de programmation dynamique (nombre de
    /// cellules) : au-delà, l'alignement des mots passe au repli linéaire.
    /// 4 000 000 tient dans 4 Mio et couvre une page de 2 000 mots.
    pub max_cells: usize,
    /// Similarité minimale pour apparier deux pages (0 à 1).
    pub page_match_threshold: f64,
    /// Comparaison des pixels, si demandée.
    pub visual: Option<VisualOptions>,
}

impl Default for CompareOptions {
    fn default() -> Self {
        Self {
            ignore_case: false,
            detect_moves: true,
            move_min_words: 3,
            max_cells: 4_000_000,
            page_match_threshold: 0.5,
            visual: None,
        }
    }
}

/// Différences de texte entre deux pages déjà extraites.
///
/// Sert directement quand on a déjà le texte sous la main (l'application, par
/// exemple) ; [`compare_documents`] l'appelle pour chaque paire de pages.
#[must_use]
pub fn compare_page_text(
    old: &PageText,
    new: &PageText,
    old_page: usize,
    new_page: usize,
    options: &CompareOptions,
) -> Vec<Difference> {
    let old_tokens = tokens(old, options.ignore_case);
    let new_tokens = tokens(new, options.ignore_case);
    let keys_old: Vec<&str> = old_tokens.iter().map(|t| t.key.as_str()).collect();
    let keys_new: Vec<&str> = new_tokens.iter().map(|t| t.key.as_str()).collect();
    let pairs = align::align_words(&keys_old, &keys_new, options.max_cells);
    let changes = align::changes(&pairs, keys_old.len(), keys_new.len());
    changes
        .into_iter()
        .map(|c| {
            let old_slice = &old_tokens[c.old.clone()];
            let new_slice = &new_tokens[c.new.clone()];
            let kind = match (old_slice.is_empty(), new_slice.is_empty()) {
                (true, _) => DiffKind::Added,
                (_, true) => DiffKind::Removed,
                _ => DiffKind::Replaced,
            };
            Difference {
                kind,
                old_page: (!old_slice.is_empty()).then_some(old_page),
                new_page: (!new_slice.is_empty()).then_some(new_page),
                old_text: join(old_slice),
                new_text: join(new_slice),
                old_rects: line_rects(old_slice),
                new_rects: line_rects(new_slice),
            }
        })
        .collect()
}

/// Texte d'une suite de mots.
fn join(tokens: &[Token]) -> String {
    tokens
        .iter()
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Un rectangle par ligne touchée (plutôt qu'un gros bloc qui déborderait sur
/// le texte inchangé autour).
fn line_rects(tokens: &[Token]) -> Vec<Rect> {
    let mut out: Vec<(usize, Rect)> = Vec::new();
    for t in tokens {
        match out.last_mut() {
            Some((line, rect)) if *line == t.line => *rect = rect.union(&t.rect),
            _ => out.push((t.line, t.rect)),
        }
    }
    out.into_iter().map(|(_, r)| r).collect()
}

/// Texte entier d'une page, comme une seule différence (page ajoutée ou
/// supprimée : détailler mot à mot n'apprendrait rien).
fn whole_page(
    text: &PageText,
    kind: DiffKind,
    page: usize,
    options: &CompareOptions,
) -> Vec<Difference> {
    let words = tokens(text, options.ignore_case);
    if words.is_empty() {
        return Vec::new();
    }
    let (content, rects) = (join(&words), line_rects(&words));
    let added = kind == DiffKind::Added;
    vec![Difference {
        kind,
        old_page: (!added).then_some(page),
        new_page: added.then_some(page),
        old_text: if added {
            String::new()
        } else {
            content.clone()
        },
        new_text: if added { content } else { String::new() },
        old_rects: if added { Vec::new() } else { rects.clone() },
        new_rects: if added { rects } else { Vec::new() },
    }]
}

/// Compare deux documents.
///
/// # Errors
/// Arbre des pages illisible. Une page dont le texte ne s'extrait pas n'est
/// pas une erreur : elle est traitée comme vide et signalée dans
/// `Comparison::warnings`.
pub fn compare_documents(
    old: &Document,
    new: &Document,
    options: &CompareOptions,
) -> Result<Comparison> {
    let old_pages = collect_pages(old)?;
    let new_pages = collect_pages(new)?;
    let mut warnings = Vec::new();
    let mut extract = |doc: &Document, pages: &[acrux_document::Page], side: &str| {
        pages
            .iter()
            .map(|p| match extract_page_text(doc, p) {
                Ok(t) => t,
                Err(e) => {
                    warnings.push(format!(
                        "{side} page {} : texte illisible ({e})",
                        p.index + 1
                    ));
                    PageText::default()
                }
            })
            .collect::<Vec<_>>()
    };
    let old_text = extract(old, &old_pages, "avant");
    let new_text = extract(new, &new_pages, "après");

    // 1. Appariement des pages sur la similarité de contenu.
    let old_bags: Vec<HashMap<String, usize>> = old_text
        .iter()
        .map(|t| align::word_bag(t, options.ignore_case))
        .collect();
    let new_bags: Vec<HashMap<String, usize>> = new_text
        .iter()
        .map(|t| align::word_bag(t, options.ignore_case))
        .collect();
    let similarity: Vec<Vec<f64>> = old_bags
        .iter()
        .map(|a| {
            new_bags
                .iter()
                .map(|b| align::bag_similarity(a, b))
                .collect()
        })
        .collect();
    let aligned = align::align_pages(&similarity, options.page_match_threshold);
    let pairs = detect_page_moves(aligned, &similarity, options.page_match_threshold);

    // 2. Différences de texte, paire par paire.
    let mut pages = Vec::with_capacity(pairs.len());
    for pair in pairs {
        let differences = match (pair.old_page, pair.new_page) {
            (Some(a), Some(b)) => match (old_text.get(a), new_text.get(b)) {
                (Some(ta), Some(tb)) => compare_page_text(ta, tb, a, b, options),
                _ => Vec::new(),
            },
            (Some(a), None) => old_text
                .get(a)
                .map(|t| whole_page(t, DiffKind::Removed, a, options))
                .unwrap_or_default(),
            (None, Some(b)) => new_text
                .get(b)
                .map(|t| whole_page(t, DiffKind::Added, b, options))
                .unwrap_or_default(),
            (None, None) => Vec::new(),
        };
        pages.push(PageDiff {
            pair,
            differences,
            visual: None,
        });
    }

    // 3. Déplacements : un bloc supprimé ici et ajouté là n'est qu'un seul
    //    mouvement. Se fait après coup, car un bloc peut changer de page.
    if options.detect_moves {
        detect_text_moves(&mut pages, options.move_min_words);
    }

    // 4. Pixels, si demandé.
    if let Some(visual) = &options.visual {
        for page in &mut pages {
            page.visual = compare_pixels(old, &old_pages, new, &new_pages, page.pair, visual);
        }
    }

    Ok(Comparison {
        pages,
        old_page_count: old_pages.len(),
        new_page_count: new_pages.len(),
        warnings,
    })
}

/// Repère les pages simplement déplacées parmi celles que l'alignement a
/// déclarées supprimées d'un côté et ajoutées de l'autre.
///
/// L'alignement global est monotone par construction : il ne peut pas croiser
/// deux pages. Une page remontée en tête d'un document en ressort donc comme
/// une suppression suivie d'un ajout, que cette passe recolle.
fn detect_page_moves(
    aligned: Vec<(Option<usize>, Option<usize>)>,
    similarity: &[Vec<f64>],
    threshold: f64,
) -> Vec<PagePair> {
    let mut pairs: Vec<PagePair> = aligned
        .into_iter()
        .map(|(old_page, new_page)| PagePair {
            old_page,
            new_page,
            similarity: match (old_page, new_page) {
                (Some(a), Some(b)) => similarity
                    .get(a)
                    .and_then(|row| row.get(b))
                    .copied()
                    .unwrap_or(0.0),
                _ => 0.0,
            },
            moved: false,
        })
        .collect();
    let orphan_old: Vec<usize> = (0..pairs.len())
        .filter(|&i| pairs[i].old_page.is_some() && pairs[i].new_page.is_none())
        .collect();
    let orphan_new: Vec<usize> = (0..pairs.len())
        .filter(|&i| pairs[i].old_page.is_none() && pairs[i].new_page.is_some())
        .collect();
    // Appariement glouton par similarité décroissante : suffisant, les
    // candidats sont peu nombreux et très contrastés (une page déplacée est
    // quasi identique à elle-même).
    let mut candidates: Vec<(f64, usize, usize)> = Vec::new();
    for &i in &orphan_old {
        for &j in &orphan_new {
            let (Some(a), Some(b)) = (pairs[i].old_page, pairs[j].new_page) else {
                continue;
            };
            let s = similarity
                .get(a)
                .and_then(|row| row.get(b))
                .copied()
                .unwrap_or(0.0);
            if s > threshold {
                candidates.push((s, i, j));
            }
        }
    }
    candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut used = vec![false; pairs.len()];
    let mut dropped = vec![false; pairs.len()];
    for (s, i, j) in candidates {
        if used[i] || used[j] {
            continue;
        }
        used[i] = true;
        used[j] = true;
        pairs[j].old_page = pairs[i].old_page;
        pairs[j].similarity = s;
        pairs[j].moved = true;
        dropped[i] = true;
    }
    let mut index = 0;
    pairs.retain(|_| {
        let keep = !dropped[index];
        index += 1;
        keep
    });
    pairs
}

/// Recolle les suppressions et les ajouts de même texte en déplacements.
fn detect_text_moves(pages: &mut [PageDiff], min_words: usize) {
    let mut removed: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    for (p, page) in pages.iter().enumerate() {
        for (d, diff) in page.differences.iter().enumerate() {
            if diff.kind == DiffKind::Removed
                && diff.old_text.split_whitespace().count() >= min_words
            {
                removed
                    .entry(normalize(&diff.old_text, true))
                    .or_default()
                    .push((p, d));
            }
        }
    }
    if removed.is_empty() {
        return;
    }
    // Ajouts à convertir : (page, indice, source du texte supprimé).
    let mut moves: Vec<((usize, usize), (usize, usize))> = Vec::new();
    for (p, page) in pages.iter().enumerate() {
        for (d, diff) in page.differences.iter().enumerate() {
            if diff.kind != DiffKind::Added || diff.new_text.split_whitespace().count() < min_words
            {
                continue;
            }
            let key = normalize(&diff.new_text, true);
            if let Some(sources) = removed.get_mut(&key) {
                if let Some(source) = sources.pop() {
                    moves.push(((p, d), source));
                }
            }
        }
    }
    let mut drop: Vec<(usize, usize)> = Vec::new();
    for ((p, d), (sp, sd)) in moves {
        let source = pages[sp].differences[sd].clone();
        let target = &mut pages[p].differences[d];
        target.kind = DiffKind::Moved;
        target.old_page = source.old_page;
        target.old_text = source.old_text;
        target.old_rects = source.old_rects;
        drop.push((sp, sd));
    }
    for (p, page) in pages.iter_mut().enumerate() {
        let mut index = 0;
        page.differences.retain(|_| {
            let keep = !drop.contains(&(p, index));
            index += 1;
            keep
        });
    }
}

/// Compare les pixels d'une paire de pages.
fn compare_pixels(
    old: &Document,
    old_pages: &[acrux_document::Page],
    new: &Document,
    new_pages: &[acrux_document::Page],
    pair: PagePair,
    options: &VisualOptions,
) -> Option<VisualDiff> {
    let render = |doc: &Document, page: &acrux_document::Page| {
        render_page(doc, page, options.scale, &RenderOptions::default())
    };
    match (
        pair.old_page.and_then(|i| old_pages.get(i)),
        pair.new_page.and_then(|i| new_pages.get(i)),
    ) {
        (Some(a), Some(b)) => {
            let (ra, rb) = (render(old, a), render(new, b));
            let pixels = compare_bitmaps(&ra.bitmap, &rb.bitmap, options);
            let inverse = rb.base_ctm.invert()?;
            Some(VisualDiff {
                rects: pixels
                    .regions
                    .iter()
                    .map(|r| inverse.transform_rect(r))
                    .collect(),
                changed_ratio: pixels.changed_ratio(),
            })
        }
        // Page ajoutée ou supprimée : tout diffère, inutile de la rendre.
        (Some(page), None) => Some(VisualDiff {
            rects: vec![page.crop_box(old)],
            changed_ratio: 1.0,
        }),
        (None, Some(page)) => Some(VisualDiff {
            rects: vec![page.crop_box(new)],
            changed_ratio: 1.0,
        }),
        (None, None) => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::text::test_support::page;

    fn options() -> CompareOptions {
        CompareOptions::default()
    }

    fn kinds(diffs: &[Difference]) -> Vec<(DiffKind, String, String)> {
        diffs
            .iter()
            .map(|d| (d.kind, d.old_text.clone(), d.new_text.clone()))
            .collect()
    }

    #[test]
    fn identical_pages_have_no_difference() {
        let a = page(&[(50.0, 700.0, 12.0, "Bonjour le monde")]);
        let b = page(&[(50.0, 700.0, 12.0, "Bonjour le monde")]);
        assert!(compare_page_text(&a, &b, 0, 0, &options()).is_empty());
    }

    #[test]
    fn an_empty_page_against_text_is_one_addition() {
        let empty = PageText::default();
        let full = page(&[(50.0, 700.0, 12.0, "un deux trois")]);
        let d = compare_page_text(&empty, &full, 0, 0, &options());
        assert_eq!(
            kinds(&d),
            vec![(DiffKind::Added, String::new(), "un deux trois".into())]
        );
        assert_eq!(d[0].new_rects.len(), 1, "une seule ligne touchée");
        assert!(d[0].old_rects.is_empty());
        assert_eq!(d[0].old_page, None);
        assert_eq!(d[0].new_page, Some(0));
        assert!(compare_page_text(&empty, &empty, 0, 0, &options()).is_empty());
    }

    #[test]
    fn a_replaced_word_is_located_on_its_line() {
        let a = page(&[
            (50.0, 700.0, 12.0, "premiere ligne intacte"),
            (50.0, 680.0, 12.0, "seconde ligne fautive"),
        ]);
        let b = page(&[
            (50.0, 700.0, 12.0, "premiere ligne intacte"),
            (50.0, 680.0, 12.0, "seconde ligne corrigee"),
        ]);
        let d = compare_page_text(&a, &b, 0, 0, &options());
        assert_eq!(
            kinds(&d),
            vec![(DiffKind::Replaced, "fautive".into(), "corrigee".into())]
        );
        assert_eq!(d[0].old_rects.len(), 1);
        assert!(
            d[0].old_rects[0].y0 < 700.0 && d[0].old_rects[0].x0 > 100.0,
            "le rectangle est sur la seconde ligne, à droite : {:?}",
            d[0].old_rects[0]
        );
    }

    #[test]
    fn a_difference_over_several_lines_gives_one_rectangle_per_line() {
        let a = page(&[(50.0, 700.0, 12.0, "garde")]);
        let b = page(&[
            (50.0, 700.0, 12.0, "garde ajoute"),
            (50.0, 680.0, 12.0, "encore ajoute"),
        ]);
        let d = compare_page_text(&a, &b, 0, 0, &options());
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].kind, DiffKind::Added);
        assert_eq!(d[0].new_rects.len(), 2, "{:?}", d[0].new_rects);
    }

    #[test]
    fn everything_replaced_is_a_single_difference() {
        let a = page(&[(50.0, 700.0, 12.0, "alpha beta gamma")]);
        let b = page(&[(50.0, 700.0, 12.0, "delta epsilon zeta")]);
        let d = compare_page_text(&a, &b, 0, 0, &options());
        assert_eq!(
            kinds(&d),
            vec![(
                DiffKind::Replaced,
                "alpha beta gamma".into(),
                "delta epsilon zeta".into()
            )]
        );
    }

    #[test]
    fn a_moved_block_is_reported_once() {
        // Le bloc « pomme poire prune » passe de la page 0 à la page 1.
        let mut pages = vec![
            PageDiff {
                pair: PagePair {
                    old_page: Some(0),
                    new_page: Some(0),
                    similarity: 0.8,
                    moved: false,
                },
                differences: compare_page_text(
                    &page(&[(50.0, 700.0, 12.0, "pomme poire prune")]),
                    &PageText::default(),
                    0,
                    0,
                    &options(),
                ),
                visual: None,
            },
            PageDiff {
                pair: PagePair {
                    old_page: Some(1),
                    new_page: Some(1),
                    similarity: 0.8,
                    moved: false,
                },
                differences: compare_page_text(
                    &PageText::default(),
                    &page(&[(50.0, 700.0, 12.0, "pomme poire prune")]),
                    1,
                    1,
                    &options(),
                ),
                visual: None,
            },
        ];
        detect_text_moves(&mut pages, 3);
        assert!(
            pages[0].differences.is_empty(),
            "{:?}",
            pages[0].differences
        );
        assert_eq!(pages[1].differences.len(), 1);
        let moved = &pages[1].differences[0];
        assert_eq!(moved.kind, DiffKind::Moved);
        assert_eq!(moved.old_page, Some(0));
        assert_eq!(moved.new_page, Some(1));
        assert!(!moved.old_rects.is_empty() && !moved.new_rects.is_empty());
    }

    #[test]
    fn a_short_block_is_not_reported_as_a_move() {
        let mut pages = vec![
            PageDiff {
                pair: PagePair {
                    old_page: Some(0),
                    new_page: Some(0),
                    similarity: 1.0,
                    moved: false,
                },
                differences: compare_page_text(
                    &page(&[(50.0, 700.0, 12.0, "le")]),
                    &PageText::default(),
                    0,
                    0,
                    &options(),
                ),
                visual: None,
            },
            PageDiff {
                pair: PagePair {
                    old_page: Some(1),
                    new_page: Some(1),
                    similarity: 1.0,
                    moved: false,
                },
                differences: compare_page_text(
                    &PageText::default(),
                    &page(&[(50.0, 700.0, 12.0, "le")]),
                    1,
                    1,
                    &options(),
                ),
                visual: None,
            },
        ];
        detect_text_moves(&mut pages, 3);
        assert_eq!(pages[0].differences[0].kind, DiffKind::Removed);
        assert_eq!(pages[1].differences[0].kind, DiffKind::Added);
    }

    #[test]
    fn counts_and_summary_hold_together() {
        let comparison = Comparison {
            pages: vec![PageDiff {
                pair: PagePair {
                    old_page: None,
                    new_page: Some(0),
                    similarity: 0.0,
                    moved: false,
                },
                differences: compare_page_text(
                    &PageText::default(),
                    &page(&[(50.0, 700.0, 12.0, "page neuve")]),
                    0,
                    0,
                    &options(),
                ),
                visual: None,
            }],
            old_page_count: 0,
            new_page_count: 1,
            warnings: Vec::new(),
        };
        let counts = comparison.counts();
        assert_eq!((counts.added, counts.pages_added), (1, 1));
        assert!(!comparison.is_identical());
        let summary = comparison.summary(3);
        assert!(summary.contains("page ajoutée"), "{summary}");
        assert!(summary.contains("+ page neuve"), "{summary}");
    }

    // --- Corpus de synthèse -------------------------------------------------

    /// Assemble un PDF à partir des corps d'objets (1 0 obj, 2 0 obj…), avec
    /// une table xref correcte.
    fn build_pdf(objects: &[String]) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    fn stream_object(data: &str) -> String {
        format!("<< /Length {} >>\nstream\n{data}\nendstream", data.len())
    }

    /// Contenu d'une page du corpus : un titre, deux lignes, un filet et un
    /// folio. Texte sans accent : ces fichiers se relisent dans un éditeur.
    fn corpus_page(title: &str, first: &str, second: &str, folio: usize) -> String {
        format!(
            "BT /F2 16 Tf 40 250 Td ({title}) Tj ET\n\
             BT /F1 11 Tf 40 220 Td ({first}) Tj ET\n\
             BT /F1 11 Tf 40 200 Td ({second}) Tj ET\n\
             0.6 G 0.6 w 40 40 m 360 40 l S\n\
             BT /F1 9 Tf 40 25 Td (page {folio}) Tj ET\n"
        )
    }

    /// Document « avant » : quatre pages 400 x 300.
    fn corpus_before() -> Vec<u8> {
        let pages = [
            corpus_page(
                "Introduction",
                "Ce document est un brouillon de comparaison.",
                "Il sert a verifier que les differences sont trouvees.",
                1,
            ),
            corpus_page(
                "Chapitre premier",
                "Le premier chapitre parle des alignements de mots.",
                "La plus longue sous-suite commune y tient le role principal.",
                2,
            ),
            corpus_page(
                "Chapitre second",
                "Le second chapitre parle des pages supprimees.",
                "Ce chapitre entier disparait dans la version suivante.",
                3,
            ),
            corpus_page(
                "Conclusion",
                "La conclusion rassemble les enseignements du document.",
                "Elle change de rang dans la version suivante.",
                4,
            ),
        ];
        let kids: Vec<String> = (0..pages.len())
            .map(|i| format!("{} 0 R", 3 + i * 2))
            .collect();
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!(
                "<< /Type /Pages /Kids [{}] /Count {} >>",
                kids.join(" "),
                pages.len()
            ),
        ];
        for (i, content) in pages.iter().enumerate() {
            let font = 3 + pages.len() * 2;
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] /Contents {} 0 R \
                 /Resources << /Font << /F1 {font} 0 R /F2 {} 0 R >> >> >>",
                4 + i * 2,
                font + 1
            ));
            objects.push(stream_object(content));
        }
        objects.push(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_string(),
        );
        objects.push(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
                .to_string(),
        );
        build_pdf(&objects)
    }

    /// Document d'une page, source de la page insérée au milieu.
    fn corpus_insert() -> Vec<u8> {
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> >>"
                .to_string(),
            stream_object(&corpus_page(
                "Annexe ajoutee",
                "Cette page est entierement nouvelle.",
                "Elle n a aucun equivalent dans la version precedente.",
                9,
            )),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
                .to_string(),
        ];
        build_pdf(&objects)
    }

    /// Écrit `comparaison-avant.pdf` et `comparaison-apres.pdf` dans
    /// `tests/corpus/synthese/`, le second obtenu du premier par les outils du
    /// projet (voir le README de ce dossier).
    ///
    /// `cargo test -p acrux-features --lib -- --ignored generate_compare_corpus`.
    #[test]
    #[ignore = "génère les fichiers de corpus"]
    fn generate_compare_corpus() {
        use crate::edit_text::{apply_edits, find_ranges, TextEdit};
        use crate::pages::{delete_pages, insert_pages_from, reorder_pages};
        use acrux_document::SaveOptions;

        let before = corpus_before();
        let doc = Document::from_bytes(before.clone()).unwrap();
        // 1. Un mot remplacé sur la première page.
        let pages = collect_pages(&doc).unwrap();
        let text = extract_page_text(&doc, &pages[0]).unwrap();
        let target = find_ranges(&text, "brouillon")[0];
        apply_edits(
            &doc,
            &[TextEdit {
                page: 0,
                target,
                new_text: "document definitif".into(),
                style: None,
            }],
        )
        .unwrap();
        // 2. « Chapitre second » supprimé.
        delete_pages(&doc, &[2]).unwrap();
        // 3. Une page neuve insérée en deuxième position.
        let extra = Document::from_bytes(corpus_insert()).unwrap();
        insert_pages_from(&doc, &extra, &[0], 1).unwrap();
        // 4. La conclusion remonte avant le premier chapitre.
        reorder_pages(&doc, &[0, 1, 3, 2]).unwrap();
        let after = doc
            .save_full_with(&SaveOptions {
                compress_streams: false,
                ..SaveOptions::default()
            })
            .unwrap();
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/synthese");
        for (name, bytes) in [
            ("comparaison-avant.pdf", &before),
            ("comparaison-apres.pdf", &after),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            println!("écrit : {}", path.display());
        }
    }
}
