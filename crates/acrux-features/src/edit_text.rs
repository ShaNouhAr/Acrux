//! Édition de texte **in place** (ISO 32000-2 §9.4) : remplacer des mots dans
//! une page en conservant le fichier, la mise en page et les polices.
//!
//! # Garanties
//!
//! - **Réécriture chirurgicale.** [`rewrite_content`] relit le flux de contenu
//!   de la page en jetons et recopie **octet pour octet** tout ce qui n'est pas
//!   remplacé : mêmes opérateurs, mêmes nombres, mêmes espacements, mêmes
//!   commentaires. Une réécriture sans modification redonne exactement les
//!   octets décodés d'origine (test `rewrite_is_byte_identical`).
//! - **Rien ne bouge autour.** Hors de la ligne éditée, pas un glyphe de la
//!   page ne se déplace : la matrice texte est rétablie à l'identique en fin
//!   d'opération.
//! - **La ligne éditée coule.** Sur la ligne elle-même, la suite du texte se
//!   décale comme dans un traitement de texte : elle avance si le remplacement
//!   est plus large, se rapproche s'il est plus court. Deux exceptions, réglées
//!   par `due()` : un vide d'un cadratin ou plus devant le texte qui suit est
//!   tenu pour une tabulation (colonne, tableau) et sa position est rétablie ;
//!   et si le décalage devait sortir de la `CropBox`, la ligne est resserrée
//!   plutôt que perdue, avec un avertissement invitant à [`reflow_paragraph`].
//!   Jamais un glyphe n'est écrit par-dessus un autre.
//! - **Polices conservées.** Le texte de remplacement est réencodé avec la
//!   police en place (encodage simple ou CMap composite). Si un caractère
//!   manque au sous-ensemble incorporé, le glyphe est ajouté en fusionnant la
//!   police système de la même famille (voir [`encode`]) ; à défaut, le texte
//!   est dessiné avec la police standard la plus proche et un avertissement
//!   est retourné.
//!
//! # Limites connues
//!
//! - Une ligne centrée ou alignée à droite garde son point de départ : elle
//!   n'est pas recentrée après l'édition, et une ligne justifiée ne l'est plus
//!   exactement. Pour recomposer une ligne ou un paragraphe selon son
//!   alignement, utiliser [`reflow_paragraph`].
//! - Le décalage ne se propage pas d'une opération de dessin à la suivante :
//!   si chaque mot est posé par son propre `Tm` (ce que fait Chrome), seul le
//!   mot remplacé change de largeur, les suivants gardent leur place.
//! - Le texte dessiné dans un XObject de formulaire n'est pas modifiable ici
//!   (il peut être partagé entre plusieurs pages).
//! - Une plage doit être contiguë dans le flux et ne pas enjamber d'opérateur
//!   non textuel (image, tracé, `BT`/`ET`, contenu balisé).
//! - Les polices Type 3 ne sont pas réencodées.
//! - Le flux de contenu de la page est réécrit **non compressé** ; `acr
//!   rewrite --compress` le recompresse si besoin.
//!
//! # Exemple
//!
//! ```no_run
//! use acrux_document::{collect_pages, Document};
//! use acrux_features::edit_text::{apply_edits, find_ranges, TextEdit};
//! use acrux_features::text::extract_page_text;
//!
//! # fn main() -> acrux_core::Result<()> {
//! let doc = Document::load("rapport.pdf")?;
//! let page = &collect_pages(&doc)?[0];
//! let text = extract_page_text(&doc, page)?;
//! let target = find_ranges(&text, "brouillon")[0];
//! apply_edits(
//!     &doc,
//!     &[TextEdit { page: 0, target, new_text: "définitif".into(), style: None }],
//! )?;
//! std::fs::write("rapport-2.pdf", doc.save_incremental()?)?;
//! # Ok(())
//! # }
//! ```

mod encode;
mod reflow;
mod scan;

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use acrux_core::{Error, Matrix, Point, Result};
use acrux_document::{collect_pages, Dict, Document, Name, Object, Page};
use acrux_render::content::ContentLexer;
use acrux_render::font::LoadedFont;
use acrux_render::page::page_content;

use crate::text::{extract_page_text, Glyph, PageText};

pub use reflow::{reflow_paragraph, ReflowOptions};

use scan::{Scan, ScannedOp, TextSnapshot};

/// Remplacement d'une opération entière du flux de contenu.
#[derive(Debug, Clone)]
pub struct Replacement {
    /// Indice de l'opération dans le flux de la page (ordre de lecture).
    pub operation: usize,
    /// Octets à écrire à la place (vide : opération supprimée).
    pub bytes: Vec<u8>,
}

/// Plage de glyphes dans le texte extrait d'une page.
///
/// Les indices sont ceux de [`crate::text`] : `line` est l'indice dans
/// `PageText::lines` (ordre de lecture), `start` et `end` comptent les glyphes
/// de la ligne, mots concaténés dans l'ordre, les espaces entre mots exclus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextRange {
    /// Indice de la ligne dans `PageText::lines`.
    pub line: usize,
    /// Premier glyphe (inclus).
    pub start: usize,
    /// Dernier glyphe (exclu).
    pub end: usize,
}

/// Style imposé au texte de remplacement (chaque champ est optionnel : ce qui
/// n'est pas précisé est repris du texte d'origine).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StyleOverride {
    /// Famille de police (une police standard est ajoutée aux ressources).
    pub font: Option<String>,
    /// Taille en points.
    pub size: Option<f64>,
    /// Couleur de remplissage RVB dans [0, 1].
    pub color: Option<[f64; 3]>,
    /// Gras.
    pub bold: Option<bool>,
    /// Italique.
    pub italic: Option<bool>,
}

impl StyleOverride {
    /// Vrai si le style ne demande aucun changement.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Une édition : remplacer une plage de glyphes par un texte.
#[derive(Debug, Clone)]
pub struct TextEdit {
    /// Indice de la page (0 = première).
    pub page: usize,
    /// Plage à remplacer.
    pub target: TextRange,
    /// Texte de remplacement (vide : suppression).
    pub new_text: String,
    /// Style imposé.
    pub style: Option<StyleOverride>,
}

/// Compte rendu d'une édition.
#[derive(Debug, Clone, Default)]
pub struct EditReport {
    /// Nombre d'éditions appliquées.
    pub edits: usize,
    /// Avertissements (police substituée, caractère perdu…).
    pub warnings: Vec<String>,
}

/// Réécrit le flux de contenu d'une page en remplaçant certaines opérations.
///
/// Tout ce qui n'est pas remplacé est recopié **octet pour octet**, espacement
/// compris ; sans remplacement, le résultat est exactement le contenu décodé
/// de la page.
///
/// # Errors
/// Flux de contenu illisible.
pub fn rewrite_content(
    doc: &Document,
    page: &Page,
    replacements: &[Replacement],
) -> Result<Vec<u8>> {
    let content = page_content(doc, page);
    rewrite_bytes(&content, replacements)
}

/// Réécriture d'un flux déjà décodé (voir [`rewrite_content`]).
fn rewrite_bytes(content: &[u8], replacements: &[Replacement]) -> Result<Vec<u8>> {
    let map: HashMap<usize, &[u8]> = replacements
        .iter()
        .map(|r| (r.operation, r.bytes.as_slice()))
        .collect();
    let mut out = Vec::with_capacity(content.len());
    let mut lexer = ContentLexer::new(content);
    let mut last = 0;
    let mut index = 0;
    while lexer.next_operation()?.is_some() {
        let end = lexer.pos();
        match map.get(&index) {
            Some(bytes) => {
                // L'espacement qui précède l'opération est conservé.
                let ws = content[last..end]
                    .iter()
                    .take_while(|b| acrux_document::lexer::is_whitespace(**b))
                    .count();
                out.extend_from_slice(&content[last..last + ws]);
                out.extend_from_slice(bytes);
            }
            None => out.extend_from_slice(&content[last..end]),
        }
        last = end;
        index += 1;
    }
    // Queue du flux (espacement ou octets non lexés).
    out.extend_from_slice(&content[last..]);
    Ok(out)
}

/// Remplace le flux de contenu d'une page par `content` (flux unique, non
/// compressé). Les éventuels flux multiples sont fusionnés.
///
/// # Errors
/// Page non indirecte.
pub fn set_page_content(doc: &Document, page: &Page, content: Vec<u8>) -> Result<()> {
    let page_ref = page
        .reference
        .ok_or_else(|| Error::Corrupt("la page doit être un objet indirect".into()))?;
    let mut dict = Dict::new();
    dict.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(content.len()).unwrap_or(0)),
    );
    let stream = Object::Stream { dict, raw: content };
    let mut page_dict = current_page_dict(doc, page)?;
    match page_dict.get(&Name::new("Contents")) {
        // Flux unique et indirect : remplacé sur place, le reste du
        // dictionnaire de page n'est pas touché.
        Some(Object::Reference(r)) if !is_array(doc, &Object::Reference(*r)) => {
            doc.set(*r, stream);
            return Ok(());
        }
        _ => {}
    }
    let r = doc.add(stream);
    page_dict.insert(Name::new("Contents"), Object::Reference(r));
    doc.set(page_ref, Object::Dict(page_dict));
    Ok(())
}

fn is_array(doc: &Document, obj: &Object) -> bool {
    doc.resolve(obj)
        .is_ok_and(|r| matches!(&*r, Object::Array(_)))
}

/// Dictionnaire de page à jour (il a pu être modifié par une édition
/// précédente).
fn current_page_dict(doc: &Document, page: &Page) -> Result<Dict> {
    match page.reference {
        Some(r) => Ok(doc
            .get(r)?
            .as_dict()
            .cloned()
            .unwrap_or_else(|| page.dict.clone())),
        None => Ok(page.dict.clone()),
    }
}

/// Dictionnaire d'une police de la page, à jour.
fn font_dict_of(doc: &Document, page: &Page, name: &Name) -> Result<Dict> {
    let dict = current_page_dict(doc, page)?;
    let resources = doc
        .dict_get(&dict, "Resources")?
        .and_then(|r| r.as_dict().cloned())
        .unwrap_or_default();
    scan::font_dict(doc, &resources, name)
        .ok_or_else(|| Error::Corrupt(format!("police /{} introuvable", name.as_str())))
}

/// Ajoute une police aux ressources de la page et renvoie son nom de
/// ressource (`/AKF0`, `/AKF1`…).
///
/// # Errors
/// Page non indirecte ou ressources illisibles.
fn add_font_resource(doc: &Document, page: &Page, font: &Dict) -> Result<Name> {
    let mut page_dict = current_page_dict(doc, page)?;
    let resources_entry = page_dict.get(&Name::new("Resources")).cloned();
    let mut resources = match &resources_entry {
        Some(o) => doc.resolve(o)?.as_dict().cloned().unwrap_or_default(),
        None => Dict::new(),
    };
    let mut fonts = doc
        .dict_get(&resources, "Font")?
        .and_then(|f| f.as_dict().cloned())
        .unwrap_or_default();
    // Une police identique déjà ajoutée est réutilisée.
    for (key, value) in &fonts {
        if key.0.starts_with(b"AKF") {
            if let Ok(existing) = doc.resolve(value) {
                if existing.as_dict() == Some(font) {
                    return Ok(key.clone());
                }
            }
        }
    }
    let name = (0..1000)
        .map(|i| Name::new(&format!("AKF{i}")))
        .find(|n| !fonts.contains_key(n))
        .ok_or_else(|| Error::Corrupt("trop de polices ajoutées".into()))?;
    let font_ref = doc.add(Object::Dict(font.clone()));
    fonts.insert(name.clone(), Object::Reference(font_ref));
    resources.insert(Name::new("Font"), Object::Dict(fonts));
    if let Some(Object::Reference(r)) = resources_entry {
        doc.set(r, Object::Dict(resources));
    } else {
        page_dict.insert(Name::new("Resources"), Object::Dict(resources));
        let page_ref = page
            .reference
            .ok_or_else(|| Error::Corrupt("la page doit être un objet indirect".into()))?;
        doc.set(page_ref, Object::Dict(page_dict));
    }
    Ok(name)
}

/// Positions des occurrences de `needle` sous forme de plages éditables
/// (recherche insensible à la casse, comme [`crate::text::find`]).
#[must_use]
pub fn find_ranges(text: &PageText, needle: &str) -> Vec<TextRange> {
    let pattern: Vec<char> = needle
        .chars()
        .filter_map(|c| c.to_lowercase().next())
        .collect();
    if pattern.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (index, line) in text.lines.iter().enumerate() {
        // Caractères de la ligne, avec l'indice du glyphe qui les porte
        // (`None` pour les espaces entre mots).
        let mut chars: Vec<(char, Option<usize>)> = Vec::new();
        let mut glyph = 0;
        for (w, word) in line.words.iter().enumerate() {
            if w > 0 {
                chars.push((' ', None));
            }
            for g in &word.glyphs {
                for c in g.text.chars() {
                    chars.push((c, Some(glyph)));
                }
                glyph += 1;
            }
        }
        let lower: Vec<char> = chars
            .iter()
            .filter_map(|(c, _)| c.to_lowercase().next())
            .collect();
        if lower.len() < pattern.len() {
            continue;
        }
        for start in 0..=lower.len() - pattern.len() {
            if lower[start..start + pattern.len()] != pattern[..] {
                continue;
            }
            let hit = &chars[start..start + pattern.len()];
            let first = hit.iter().find_map(|(_, g)| *g);
            let last = hit.iter().rev().find_map(|(_, g)| *g);
            if let (Some(first), Some(last)) = (first, last) {
                out.push(TextRange {
                    line: index,
                    start: first,
                    end: last + 1,
                });
            }
        }
    }
    out
}

/// Applique des éditions de texte au document.
///
/// Les plages désignent le texte **avant** édition : toutes les éditions d'une
/// page sont résolues sur la même extraction, puis appliquées en une seule
/// réécriture du flux.
///
/// # Errors
/// Plage invalide, texte non modifiable (XObject, plage non contiguë), page
/// non indirecte, ou flux illisible.
pub fn apply_edits(doc: &Document, edits: &[TextEdit]) -> Result<()> {
    apply_edits_reporting(doc, edits).map(|_| ())
}

/// Comme [`apply_edits`], en retournant les avertissements (police
/// substituée, caractère sans glyphe…).
///
/// # Errors
/// Voir [`apply_edits`].
pub fn apply_edits_reporting(doc: &Document, edits: &[TextEdit]) -> Result<EditReport> {
    let pages = collect_pages(doc)?;
    let mut report = EditReport::default();
    let mut by_page: HashMap<usize, Vec<&TextEdit>> = HashMap::new();
    for e in edits {
        by_page.entry(e.page).or_default().push(e);
    }
    let mut indexes: Vec<usize> = by_page.keys().copied().collect();
    indexes.sort_unstable();
    for index in indexes {
        let page = pages
            .get(index)
            .ok_or_else(|| Error::Corrupt(format!("page {} inexistante", index + 1)))?;
        let list = by_page.remove(&index).unwrap_or_default();
        let warnings = apply_page_edits(doc, page, &list)?;
        report.edits += list.len();
        report.warnings.extend(warnings);
    }
    Ok(report)
}

/// Applique les éditions d'une page en une réécriture.
fn apply_page_edits(doc: &Document, page: &Page, edits: &[&TextEdit]) -> Result<Vec<String>> {
    let scan = scan::scan(doc, page)?;
    let text = extract_page_text(doc, page)?;
    let index = GlyphIndex::new(&scan);
    let mut warnings = Vec::new();
    // Une coupe par opération touchée.
    let mut cuts: HashMap<usize, Vec<Cut>> = HashMap::new();
    for edit in edits {
        let run = index.resolve(&scan, &text, edit.target)?;
        let name = op_font_name(&scan, run.first_op)?;
        let prepared = encode::prepare(doc, page, &name, &edit.new_text, edit.style.as_ref())?;
        warnings.extend(prepared.warnings.iter().cloned());
        let encoded = prepared.encode(&edit.new_text);
        if !encoded.lost.is_empty() {
            warnings.push(format!(
                "caractères sans glyphe, ignorés : {}",
                encoded.lost.iter().collect::<String>()
            ));
        }
        let insertion = Insertion {
            resource: prepared.resource.clone(),
            bytes: encoded.bytes.clone(),
            widths: encoded.widths.clone(),
            style: edit.style.clone().unwrap_or_default(),
        };
        for (op, cut) in run.cuts(Some(insertion)) {
            cuts.entry(op).or_default().push(cut);
        }
    }
    let mut replacements = Vec::new();
    for (op, mut list) in cuts {
        list.sort_by_key(|c| c.from);
        for pair in list.windows(2) {
            if pair[0].to > pair[1].from {
                return Err(Error::Corrupt(
                    "deux éditions se chevauchent dans la même opération".into(),
                ));
            }
        }
        replacements.push(Replacement {
            operation: op,
            bytes: rewrite_show_op(doc, page, &scan, op, &list, &mut warnings)?,
        });
    }
    let content = rewrite_bytes(&scan.content, &replacements)?;
    set_page_content(doc, page, content)?;
    Ok(warnings)
}

/// Position dans une opération de dessin de texte : indice de l'élément
/// (chaîne ou nombre d'un tableau `TJ`) et octet dans la chaîne.
type Pos = (usize, usize);

/// Élément d'une opération de dessin de texte.
#[derive(Debug, Clone)]
enum Item {
    /// Chaîne à dessiner.
    Str(Vec<u8>),
    /// Ajustement de position (millièmes d'em).
    Num(f64),
}

/// Texte à insérer à la place d'une coupe.
#[derive(Debug, Clone)]
struct Insertion {
    resource: Name,
    bytes: Vec<u8>,
    widths: Vec<(f64, bool)>,
    style: StyleOverride,
}

/// Portion supprimée d'une opération, éventuellement remplacée.
#[derive(Debug, Clone)]
struct Cut {
    /// Début (inclus) ; `None` = début de l'opération.
    from: Option<Pos>,
    /// Fin (exclue) ; `None` = fin de l'opération.
    to: Option<Pos>,
    /// Texte inséré à la place.
    insert: Option<Insertion>,
}

/// Plage résolue dans le flux : opérations et positions.
struct ResolvedRun {
    first_op: usize,
    last_op: usize,
    from: Pos,
    to: Pos,
    /// Opérations de dessin de texte entièrement supprimées (entre les deux).
    middle: Vec<usize>,
}

impl ResolvedRun {
    /// Coupes à appliquer, une par opération touchée.
    fn cuts(&self, insert: Option<Insertion>) -> Vec<(usize, Cut)> {
        let mut out = Vec::new();
        if self.first_op == self.last_op {
            out.push((
                self.first_op,
                Cut {
                    from: Some(self.from),
                    to: Some(self.to),
                    insert,
                },
            ));
            return out;
        }
        out.push((
            self.first_op,
            Cut {
                from: Some(self.from),
                to: None,
                insert,
            },
        ));
        for op in &self.middle {
            out.push((
                *op,
                Cut {
                    from: None,
                    to: None,
                    insert: None,
                },
            ));
        }
        out.push((
            self.last_op,
            Cut {
                from: None,
                to: Some(self.to),
                insert: None,
            },
        ));
        out
    }
}

/// Boîte de chaque glyphe de la page, avec l'**indice de l'opération** qui l'a
/// dessiné.
///
/// Sert à [`edit_objects`](crate::edit_objects) pour rendre à chaque bloc
/// `BT … ET` la boîte de ses propres glyphes : le balayage des objets connaît
/// les plages d'opérations, l'extraction de texte connaît les métriques, et
/// cette fonction fait le pont. Les glyphes que l'extraction ne retrouve pas
/// dans le flux — ils viennent d'un XObject de formulaire — sont omis.
///
/// # Errors
/// Flux de contenu ou texte illisible.
pub(crate) fn glyph_operations(
    doc: &Document,
    page: &Page,
) -> Result<Vec<(acrux_core::Rect, usize)>> {
    let scan = scan::scan(doc, page)?;
    let text = extract_page_text(doc, page)?;
    let index = GlyphIndex::new(&scan);
    let mut out = Vec::new();
    for line in &text.lines {
        for word in &line.words {
            for glyph in &word.glyphs {
                if let Some(found) = index.find(&scan, glyph) {
                    out.push((glyph.bbox, scan.glyphs[found].site.op));
                }
            }
        }
    }
    Ok(out)
}

/// Retrouve un glyphe de `PageText` dans le flux : clé exacte (mêmes calculs
/// des deux côtés), puis repli sur le glyphe le plus proche de même texte.
struct GlyphIndex {
    by_key: HashMap<(u64, u64, u64), usize>,
}

impl GlyphIndex {
    fn new(scan: &Scan) -> Self {
        let mut by_key = HashMap::new();
        for (i, g) in scan.glyphs.iter().enumerate() {
            by_key
                .entry((g.along.to_bits(), g.perp.to_bits(), g.along_end.to_bits()))
                .or_insert(i);
        }
        Self { by_key }
    }

    fn find(&self, scan: &Scan, glyph: &Glyph) -> Option<usize> {
        let key = (
            glyph.along.to_bits(),
            glyph.perp.to_bits(),
            glyph.along_end.to_bits(),
        );
        if let Some(i) = self.by_key.get(&key) {
            return Some(*i);
        }
        // Repli : même texte, position la plus proche (tolérance 0,05 pt).
        scan.glyphs
            .iter()
            .enumerate()
            .filter(|(_, g)| g.text == glyph.text)
            .map(|(i, g)| {
                (
                    i,
                    (g.along - glyph.along).abs() + (g.perp - glyph.perp).abs(),
                )
            })
            .filter(|(_, d)| *d < 0.05)
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
    }

    /// Résout une plage de `PageText` en positions dans le flux.
    fn resolve(&self, scan: &Scan, text: &PageText, range: TextRange) -> Result<ResolvedRun> {
        let line = text
            .lines
            .get(range.line)
            .ok_or_else(|| Error::Corrupt(format!("ligne {} inexistante", range.line)))?;
        let glyphs: Vec<&Glyph> = line.words.iter().flat_map(|w| w.glyphs.iter()).collect();
        if range.start >= range.end || range.end > glyphs.len() {
            return Err(Error::Corrupt(format!(
                "plage de glyphes {}..{} invalide (la ligne en a {})",
                range.start,
                range.end,
                glyphs.len()
            )));
        }
        let mut found = Vec::new();
        for g in &glyphs[range.start..range.end] {
            let i = self.find(scan, g).ok_or_else(|| {
                Error::Corrupt(format!(
                    "glyphe « {} » introuvable dans le flux de contenu",
                    g.text
                ))
            })?;
            found.push(i);
        }
        let (min, max) = (
            *found.iter().min().unwrap_or(&0),
            *found.iter().max().unwrap_or(&0),
        );
        // Les glyphes intermédiaires non sélectionnés doivent être des
        // espaces (qui font partie du texte remplacé).
        let selected: HashSet<usize> = found.iter().copied().collect();
        for i in min..=max {
            if !selected.contains(&i) && scan.glyphs[i].text != " " {
                return Err(Error::Corrupt(
                    "la plage n'est pas contiguë dans le flux de contenu".into(),
                ));
            }
        }
        let first = &scan.glyphs[min].site;
        let last = &scan.glyphs[max].site;
        if first.in_form || last.in_form {
            return Err(Error::Unsupported(
                "texte dessiné dans un XObject de formulaire : non modifiable".into(),
            ));
        }
        // Opérations de texte entièrement consommées entre les deux.
        let mut middle = Vec::new();
        for op in first.op + 1..last.op {
            let scanned = &scan.ops[op];
            if is_show(&scanned.operator) {
                middle.push(op);
            } else if !is_text_state(&scanned.operator) {
                return Err(Error::Unsupported(format!(
                    "opérateur « {} » entre les glyphes : plage non modifiable",
                    String::from_utf8_lossy(&scanned.operator)
                )));
            }
        }
        Ok(ResolvedRun {
            first_op: first.op,
            last_op: last.op,
            from: (first.item, first.start),
            to: (last.item, last.end),
            middle,
        })
    }
}

/// Vrai si la matrice texte laissée par l'opération `index` sert à
/// positionner un dessin : une opération de dessin la suit avant tout
/// repositionnement (`Tm`, `Td`, `TD`, `T*`, `BT`).
///
/// Quand ce n'est pas le cas, l'ajustement `TJ` qui rétablirait l'avance
/// d'origine est inutile ; l'écrire ferait apparaître un espace pour les
/// extracteurs de texte (un grand recul se lit comme une espace, §9.4.3).
fn needs_advance(scan: &Scan, index: usize) -> bool {
    for op in scan.ops.iter().skip(index + 1) {
        if is_show(&op.operator) {
            return true;
        }
        if matches!(
            op.operator.as_slice(),
            b"Tm" | b"Td" | b"TD" | b"T*" | b"BT" | b"ET"
        ) {
            return false;
        }
    }
    false
}

/// Vrai pour un opérateur de dessin de texte.
fn is_show(operator: &[u8]) -> bool {
    matches!(operator, b"Tj" | b"TJ" | b"'" | b"\"")
}

/// Vrai pour un opérateur d'état (texte ou couleur) : il peut rester entre
/// deux morceaux d'une plage remplacée sans rien déplacer.
fn is_text_state(operator: &[u8]) -> bool {
    matches!(
        operator,
        b"Tf"
            | b"Tc"
            | b"Tw"
            | b"Tz"
            | b"TL"
            | b"Ts"
            | b"Tr"
            | b"Td"
            | b"TD"
            | b"Tm"
            | b"T*"
            | b"g"
            | b"rg"
            | b"k"
            | b"cs"
            | b"sc"
            | b"scn"
            | b"gs"
    )
}

/// Nom de ressource de la police d'une opération.
fn op_font_name(scan: &Scan, op: usize) -> Result<Name> {
    scan.ops
        .get(op)
        .and_then(|o| o.after.font.clone())
        .ok_or_else(|| Error::Corrupt("aucune police active (Tf manquant)".into()))
}

/// Éléments d'une opération de dessin de texte.
fn items_of(op: &ScannedOp) -> Vec<Item> {
    let string = |o: Option<&Object>| match o {
        Some(Object::String(s)) => vec![Item::Str(s.clone())],
        _ => Vec::new(),
    };
    match op.operator.as_slice() {
        b"Tj" | b"'" => string(op.operands.last()),
        b"\"" => string(op.operands.get(2)),
        b"TJ" => match op.operands.last() {
            Some(Object::Array(items)) => items
                .iter()
                .filter_map(|o| match o {
                    Object::String(s) => Some(Item::Str(s.clone())),
                    other => other.as_f64().map(Item::Num),
                })
                .collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Sous-suite d'éléments entre deux positions.
fn slice_items(items: &[Item], a: Pos, b: Pos) -> Vec<Item> {
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if i < a.0 || i > b.0 {
            continue;
        }
        match item {
            Item::Str(s) => {
                let start = if i == a.0 { a.1.min(s.len()) } else { 0 };
                let end = if i == b.0 { b.1.min(s.len()) } else { s.len() };
                if start < end {
                    out.push(Item::Str(s[start..end].to_vec()));
                }
            }
            Item::Num(n) => {
                let after_start = i > a.0 || a.1 == 0;
                if after_start && i < b.0 {
                    out.push(Item::Num(*n));
                }
            }
        }
    }
    out
}

/// Avance (espace texte) d'une suite d'éléments.
fn advance_items(items: &[Item], font: &LoadedFont, st: &TextSnapshot) -> f64 {
    let mut tx = 0.0;
    for item in items {
        match item {
            Item::Str(s) => {
                for (g, _) in font.decode_spans(s) {
                    let mut w = g.width * st.size + st.char_spacing;
                    if g.is_space {
                        w += st.word_spacing;
                    }
                    tx += w * st.hscale;
                }
            }
            Item::Num(n) => tx += -n / 1000.0 * st.size * st.hscale,
        }
    }
    tx
}

/// Réécrit une opération de dessin de texte avec ses coupes.
fn rewrite_show_op(
    doc: &Document,
    page: &Page,
    scan: &Scan,
    index: usize,
    cuts: &[Cut],
    warnings: &mut Vec<String>,
) -> Result<Vec<u8>> {
    let op = scan
        .ops
        .get(index)
        .ok_or_else(|| Error::Corrupt("opération inexistante".into()))?;
    let st = &op.after; // `"` a déjà posé Tw et Tc
    let name = st
        .font
        .clone()
        .ok_or_else(|| Error::Corrupt("aucune police active".into()))?;
    let font = LoadedFont::load(doc, &font_dict_of(doc, page, &name)?)?;
    let items = items_of(op);
    let end: Pos = (items.len(), 0);
    let mut out = Vec::new();
    // Effet propre de l'opérateur (déplacement de ligne, espacements).
    match op.operator.as_slice() {
        b"'" => out.extend_from_slice(b"T* "),
        b"\"" => {
            let _ = write!(
                out_str(&mut out),
                "{} Tw {} Tc T* ",
                fmt(st.word_spacing),
                fmt(st.char_spacing)
            );
        }
        _ => {}
    }
    let room = room_to_right(doc, page, st);
    let mut squeezed = false;
    let mut pos: Pos = (0, 0);
    let mut emitted = 0.0;
    // Écart accumulé entre ce que l'on a écrit et ce que faisait l'original,
    // en millièmes d'em. Négatif = le texte suivant doit avancer pour retrouver
    // sa place ; positif = il faudrait le faire *reculer*, donc le glisser sous
    // le texte inséré. On ne recule jamais : la ligne se décale (cf. `due`).
    let mut pending = 0.0;
    for cut in cuts {
        let from = cut.from.unwrap_or(pos);
        let kept = slice_items(&items, pos, from);
        if !kept.is_empty() {
            // L'ajustement en attente positionne ce qui suit : il est dû.
            let owed = due(std::mem::take(&mut pending), leading_gap(&kept), room);
            squeezed |= owed > leading_gap(&kept) + 1e-6;
            write_adjust(&mut out, owed);
            emitted += advance_items(&kept, &font, st);
            write_tj(&mut out, &kept);
        }
        if let Some(ins) = &cut.insert {
            // Le texte inséré est dessiné tout de suite : aucun espace libre.
            let owed = due(std::mem::take(&mut pending), 0.0, room);
            squeezed |= owed > 1e-6;
            write_adjust(&mut out, owed);
            emitted += write_insertion(&mut out, ins, &name, st);
        }
        pos = cut.to.unwrap_or(end);
        // Ajustement : ramène la matrice texte là où elle était.
        let target = advance_items(&slice_items(&items, (0, 0), pos), &font, st);
        let scale = st.size * st.hscale;
        if scale.abs() > 1e-9 {
            pending += (emitted - target) * 1000.0 / scale;
        }
        emitted = target;
    }
    let rest = slice_items(&items, pos, end);
    if !rest.is_empty() {
        let owed = due(std::mem::take(&mut pending), leading_gap(&rest), room);
        squeezed |= owed > leading_gap(&rest) + 1e-6;
        write_adjust(&mut out, owed);
        write_tj(&mut out, &rest);
    } else if needs_advance(scan, index) {
        // La matrice texte n'est rétablie que si un dessin en dépend :
        // sinon le recul serait inutile (et vu comme un espace à l'extraction).
        let owed = due(pending, 0.0, room);
        squeezed |= owed > 1e-6;
        write_adjust(&mut out, owed);
    }
    while out.last() == Some(&b' ') {
        out.pop();
    }
    if squeezed {
        warnings.push(
            "le texte inséré est plus large que la place disponible sur la ligne :              la suite a été resserrée dessous. Utilisez `reflow_paragraph` pour              recomposer le paragraphe."
                .into(),
        );
    }
    Ok(out)
}

/// Ajustement réellement écrit, `gap` étant l'espace libre devant le texte qui
/// suit. Un `TJ` positif ramène la plume *vers la gauche* : appliqué après un
/// remplacement plus large que l'original, il ferait passer la suite de la ligne
/// sous le texte inséré. On ne recule donc que dans la limite de l'espace
/// disponible, et la ligne se décale vers la droite pour le reste — comme le
/// ferait un traitement de texte. Un ajustement négatif (remplacement plus
/// court) reste dû en entier : il rend au texte suivant sa place exacte.
fn due(pending: f64, gap: f64, room: f64) -> f64 {
    if pending < 0.0 {
        // Remplacement plus court : on referme le trou, comme un traitement de
        // texte. Sauf si ce qui suit est nettement détaché — un cadratin de
        // vide ou plus est une décision de mise en page (tabulation, colonne),
        // pas une conséquence du mot précédent : sa position est rétablie.
        return if gap > COLUMN_GAP { pending } else { 0.0 };
    }
    // On recule le moins possible (`gap`), mais assez pour que le décalage
    // tienne dans la page (`pending - room`), et jamais plus que l'écart réel.
    gap.max(0.0).max(pending - room.max(0.0)).min(pending)
}

/// Vide, en millièmes d'em, à partir duquel un écart entre deux morceaux de
/// texte est tenu pour une tabulation plutôt que pour une espace.
const COLUMN_GAP: f64 = 1000.0;

/// Place restante à droite de la plume, en millièmes d'em du texte courant.
/// Au-delà, décaler la suite de la ligne la ferait sortir de la zone visible :
/// mieux vaut alors resserrer (l'appelant est prévenu) que perdre le texte.
fn room_to_right(doc: &Document, page: &Page, st: &TextSnapshot) -> f64 {
    let m = st.tm.then(&st.ctm);
    let pen = m.apply(Point::new(0.0, 0.0));
    let step = m.apply_vector(Point::new(1.0, 0.0)).x;
    let scale = st.size * st.hscale;
    if step.abs() < 1e-9 || scale.abs() < 1e-9 {
        // Texte vertical, renversé ou de taille nulle : on ne juge pas.
        return f64::INFINITY;
    }
    let box_ = page.crop_box(doc);
    let free = if step > 0.0 {
        box_.x1 - pen.x
    } else {
        pen.x - box_.x0
    };
    (free / step.abs() * 1000.0 / scale).max(0.0)
}

/// Espace libre en tête d'une suite d'éléments, en millièmes d'em : un `TJ`
/// commençant par un nombre négatif éloigne le texte suivant, et ce vide peut
/// absorber tout ou partie du recul sans provoquer de chevauchement.
fn leading_gap(items: &[Item]) -> f64 {
    let mut gap = 0.0;
    for item in items {
        match item {
            Item::Num(n) => gap += -n,
            Item::Str(_) => break,
        }
    }
    gap
}

/// Écrit le texte inséré (police, taille et couleur imposées, puis
/// restauration) et renvoie son avance.
fn write_insertion(out: &mut Vec<u8>, ins: &Insertion, original: &Name, st: &TextSnapshot) -> f64 {
    let size = ins.style.size.unwrap_or(st.size);
    let changed_font = ins.resource != *original || (size - st.size).abs() > 1e-9;
    if let Some(c) = ins.style.color {
        let _ = write!(
            out_str(out),
            "{} {} {} rg ",
            fmt(c[0]),
            fmt(c[1]),
            fmt(c[2])
        );
    }
    if changed_font {
        let _ = write!(out_str(out), "/{} {} Tf ", ins.resource.as_str(), fmt(size));
    }
    if !ins.bytes.is_empty() {
        write_tj(out, &[Item::Str(ins.bytes.clone())]);
    }
    if changed_font {
        let _ = write!(out_str(out), "/{} {} Tf ", original.as_str(), fmt(st.size));
    }
    if ins.style.color.is_some() {
        out.extend_from_slice(&st.fill.restore());
        out.push(b' ');
    }
    // L'avance se calcule avec la taille et les espacements effectifs.
    ins.widths
        .iter()
        .map(|(w, space)| {
            (w * size + st.char_spacing + if *space { st.word_spacing } else { 0.0 }) * st.hscale
        })
        .sum()
}

/// Écrit un ajustement de position `[ … ] TJ`.
///
/// Un grand recul est découpé en plusieurs nombres : au-delà de 180 millièmes
/// d'em, les extracteurs de texte (le nôtre comme ceux d'Acrobat) voient un
/// espace, ce qui découperait le mot réécrit. Le déplacement total est
/// identique (§9.4.3 : les nombres d'un `TJ` s'appliquent l'un après l'autre).
fn write_adjust(out: &mut Vec<u8>, adj: f64) {
    if !adj.is_finite() || adj.abs() <= 1e-6 {
        return;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // borné
    let parts = ((adj.abs() / 160.0).ceil() as usize).clamp(1, 1024);
    #[allow(clippy::cast_precision_loss)] // parts ≤ 64
    let step = adj / parts as f64;
    out.push(b'[');
    for i in 0..parts {
        if i > 0 {
            out.push(b' ');
        }
        let _ = write!(out_str(out), "{}", fmt(step));
    }
    out.extend_from_slice(b"] TJ ");
}

/// Écrit `[ … ] TJ` pour une suite d'éléments.
fn write_tj(out: &mut Vec<u8>, items: &[Item]) {
    out.push(b'[');
    for item in items {
        match item {
            Item::Str(s) => write_hex(out, s),
            Item::Num(n) => {
                let _ = write!(out_str(out), "{}", fmt(*n));
            }
        }
    }
    out.extend_from_slice(b"] TJ ");
}

/// Chaîne hexadécimale `<…>` (toujours sûre, quels que soient les octets).
fn write_hex(out: &mut Vec<u8>, bytes: &[u8]) {
    out.push(b'<');
    for b in bytes {
        let _ = write!(out_str(out), "{b:02X}");
    }
    out.push(b'>');
}

/// Adaptateur `fmt::Write` sur un tampon d'octets (l'écriture ne peut pas
/// échouer : on n'écrit que de l'ASCII).
struct OutStr<'a>(&'a mut Vec<u8>);

impl std::fmt::Write for OutStr<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

fn out_str(out: &mut Vec<u8>) -> OutStr<'_> {
    OutStr(out)
}

/// Nombre PDF : au plus quatre décimales, sans zéros inutiles (§7.3.3).
fn fmt(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".into()
    } else {
        s.to_string()
    }
}

/// Écrit `a b c d e f Tm`.
fn write_matrix(out: &mut Vec<u8>, m: &Matrix) {
    let _ = write!(
        out_str(out),
        "{} {} {} {} {} {} Tm ",
        fmt(m.a),
        fmt(m.b),
        fmt(m.c),
        fmt(m.d),
        fmt(m.e),
        fmt(m.f)
    );
}

/// Rétablit exactement les matrices texte d'un état : `Tm` pose la matrice de
/// ligne, un ajustement `TJ` replace la matrice texte si elle en différait
/// (ce que seul un déplacement horizontal peut faire après un `Tj`).
fn restore_matrices(out: &mut Vec<u8>, st: &TextSnapshot, advance: bool) -> Option<String> {
    let (m, n) = (st.tlm, st.tm);
    let same_linear = (m.a - n.a).abs() < 1e-12
        && (m.b - n.b).abs() < 1e-12
        && (m.c - n.c).abs() < 1e-12
        && (m.d - n.d).abs() < 1e-12;
    if !same_linear {
        // Cas très rare (matrice texte modifiée autrement qu'en avançant) :
        // on rétablit la matrice texte, la matrice de ligne est perdue.
        write_matrix(out, &n);
        return Some("matrice de ligne non rétablie après réécriture".into());
    }
    write_matrix(out, &m);
    let det = m.a.mul_add(m.d, -(m.b * m.c));
    if det.abs() < 1e-12 {
        return None;
    }
    let (dx, dy) = (n.e - m.e, n.f - m.f);
    let tx = dx.mul_add(m.d, -(dy * m.c)) / det;
    let ty = m.a.mul_add(dy, -(m.b * dx)) / det;
    let scale = st.size * st.hscale;
    if ty.abs() > 1e-9 || scale.abs() < 1e-9 || !advance {
        return None;
    }
    write_adjust(out, -tx * 1000.0 / scale);
    None
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp
)]
mod tests {
    use super::*;
    use crate::text::find;

    /// PDF minimal avec une police standard et le contenu donné.
    fn pdf(content: &str) -> Document {
        let src = format!(
            "%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 600 300] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >> endobj\n4 0 obj << /Length {} >>\nstream\n{content}\nendstream\nendobj\n5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >> endobj\n",
            content.len()
        );
        Document::from_bytes(src.into_bytes()).unwrap()
    }

    fn page_of(doc: &Document) -> Page {
        collect_pages(doc).unwrap().remove(0)
    }

    #[test]
    fn rewrite_without_change_is_byte_identical() {
        let doc = pdf("BT /F1 12 Tf 20 250 Td (Bonjour)Tj\n0 -20 Td[(Deux) -600 (mots)]TJ ET\n% commentaire\n");
        let page = page_of(&doc);
        let original = page_content(&doc, &page);
        let rewritten = rewrite_content(&doc, &page, &[]).unwrap();
        assert_eq!(original, rewritten);
    }

    #[test]
    fn replace_same_length_keeps_following_text() {
        let doc = pdf("BT /F1 12 Tf 20 250 Td (Bonjour le monde) Tj ET");
        let page = page_of(&doc);
        let text = extract_page_text(&doc, &page).unwrap();
        let target = find_ranges(&text, "monde")[0];
        apply_edits(
            &doc,
            &[TextEdit {
                page: 0,
                target,
                new_text: "MONDE".into(),
                style: None,
            }],
        )
        .unwrap();
        let page = page_of(&doc);
        let after = extract_page_text(&doc, &page).unwrap();
        assert_eq!(after.lines[0].text(), "Bonjour le MONDE");
    }

    #[test]
    fn a_longer_replacement_pushes_the_rest_of_the_line_instead_of_covering_it() {
        // Sans crénage, « ici » est collé au mot remplacé : il n'y a aucune
        // place libre, donc la ligne doit se décaler au lieu de se superposer.
        let doc = pdf("BT /F1 12 Tf 20 250 Td [(un mot )(ici)] TJ ET");
        let page = page_of(&doc);
        let before = extract_page_text(&doc, &page).unwrap();
        let x_ici = before.lines[0].words[2].bbox.x0;
        let target = find_ranges(&before, "mot")[0];
        apply_edits(
            &doc,
            &[TextEdit {
                page: 0,
                target,
                new_text: "mot beaucoup plus long".into(),
                style: None,
            }],
        )
        .unwrap();
        let page = page_of(&doc);
        let after = extract_page_text(&doc, &page).unwrap();
        let words = &after.lines[0].words;
        assert_eq!(words.last().map(|w| w.text.as_str()), Some("ici"));
        let last = words.last().unwrap();
        let previous = &words[words.len() - 2];
        assert!(
            last.bbox.x0 >= previous.bbox.x1 - 0.01,
            "« ici » chevauche le texte inséré : {} < {}",
            last.bbox.x0,
            previous.bbox.x1
        );
        assert!(
            last.bbox.x0 > x_ici,
            "« ici » aurait dû être poussé vers la droite : {x_ici} → {}",
            last.bbox.x0
        );
    }

    #[test]
    fn a_shorter_replacement_closes_the_hole_it_would_leave() {
        // Texte courant : la suite de la ligne se rapproche.
        let doc = pdf("BT /F1 12 Tf 20 250 Td [(un mot )(ici)] TJ ET");
        let page = page_of(&doc);
        let before = extract_page_text(&doc, &page).unwrap();
        let x_ici = before.lines[0].words[2].bbox.x0;
        let target = find_ranges(&before, "mot")[0];
        apply_edits(
            &doc,
            &[TextEdit {
                page: 0,
                target,
                new_text: "m".into(),
                style: None,
            }],
        )
        .unwrap();
        let page = page_of(&doc);
        let after = extract_page_text(&doc, &page).unwrap();
        let last = after.lines[0].words.last().unwrap();
        assert_eq!(last.text, "ici");
        assert!(
            last.bbox.x0 < x_ici - 0.5,
            "le trou n'a pas été refermé : {x_ici} → {}",
            last.bbox.x0
        );
    }

    #[test]
    fn replace_longer_and_shorter_does_not_move_the_rest() {
        // « ici » est dessiné par la même opération, 240 pt plus loin.
        for replacement in ["mot beaucoup plus long", "m", "mêlé"] {
            let doc = pdf("BT /F1 12 Tf 20 250 Td [(un mot) -20000 (ici)] TJ ET");
            let page = page_of(&doc);
            let before = extract_page_text(&doc, &page).unwrap();
            let x_ici = before.lines[0].words[2].bbox.x0;
            let target = find_ranges(&before, "mot")[0];
            apply_edits(
                &doc,
                &[TextEdit {
                    page: 0,
                    target,
                    new_text: replacement.into(),
                    style: None,
                }],
            )
            .unwrap();
            let page = page_of(&doc);
            let after = extract_page_text(&doc, &page).unwrap();
            let words: Vec<&str> = after.lines[0]
                .words
                .iter()
                .map(|w| w.text.as_str())
                .collect();
            assert_eq!(words.first(), Some(&"un"), "{replacement}");
            assert_eq!(words.last(), Some(&"ici"), "{replacement}");
            let x_after = after.lines[0].words.last().unwrap().bbox.x0;
            assert!(
                (x_after - x_ici).abs() < 0.01,
                "« ici » a bougé ({replacement}) : {x_ici} → {x_after}"
            );
        }
    }

    #[test]
    fn replace_across_operations() {
        // Chaque mot est dessiné par sa propre opération, comme le fait Chrome.
        let doc = pdf("BT /F1 12 Tf 20 250 Td (un) Tj 20 0 Td (deux) Tj 40 0 Td (trois) Tj ET");
        let page = page_of(&doc);
        let before = extract_page_text(&doc, &page).unwrap();
        let x_trois = before.lines[0].words[2].bbox.x0;
        let target = find_ranges(&before, "deux")[0];
        apply_edits(
            &doc,
            &[TextEdit {
                page: 0,
                target,
                new_text: "II".into(),
                style: None,
            }],
        )
        .unwrap();
        let page = page_of(&doc);
        let after = extract_page_text(&doc, &page).unwrap();
        assert_eq!(after.lines[0].text(), "un II trois");
        assert!((after.lines[0].words[2].bbox.x0 - x_trois).abs() < 0.01);
    }

    #[test]
    fn style_override_changes_color_and_size() {
        let doc = pdf("BT /F1 12 Tf 1 0 0 rg 20 250 Td (rouge et petit) Tj ET");
        let page = page_of(&doc);
        let text = extract_page_text(&doc, &page).unwrap();
        let target = find_ranges(&text, "petit")[0];
        apply_edits(
            &doc,
            &[TextEdit {
                page: 0,
                target,
                new_text: "GRAND".into(),
                style: Some(StyleOverride {
                    size: Some(20.0),
                    color: Some([0.0, 0.0, 1.0]),
                    ..StyleOverride::default()
                }),
            }],
        )
        .unwrap();
        let page = page_of(&doc);
        let after = extract_page_text(&doc, &page).unwrap();
        let word = after
            .words()
            .into_iter()
            .find(|w| w.text == "GRAND")
            .expect("mot remplacé");
        assert_eq!(word.style.color, [0.0, 0.0, 1.0]);
        assert!((word.style.size - 20.0).abs() < 0.01, "{:?}", word.style);
        // Le texte qui précède garde sa couleur et sa taille.
        let first = after.words()[0].style.clone();
        assert_eq!(first.color, [1.0, 0.0, 0.0]);
        assert!((first.size - 12.0).abs() < 0.01);
    }

    #[test]
    fn delete_text_and_find_ranges() {
        let doc = pdf("BT /F1 12 Tf 20 250 Td (alpha beta gamma) Tj ET");
        let page = page_of(&doc);
        let text = extract_page_text(&doc, &page).unwrap();
        assert_eq!(find_ranges(&text, "beta").len(), 1);
        assert_eq!(find_ranges(&text, "absent").len(), 0);
        assert_eq!(find(&text, "beta").len(), 1);
        let target = find_ranges(&text, "beta ")[0];
        apply_edits(
            &doc,
            &[TextEdit {
                page: 0,
                target,
                new_text: String::new(),
                style: None,
            }],
        )
        .unwrap();
        let page = page_of(&doc);
        let after = extract_page_text(&doc, &page).unwrap();
        assert_eq!(after.lines[0].text(), "alpha gamma");
    }

    #[test]
    fn several_content_streams_are_merged_and_edited() {
        // `/Contents` en tableau : les flux sont concaténés pour l'édition,
        // puis remplacés par un flux unique (§7.8.2).
        let (a, b) = (
            "BT /F1 12 Tf 20 250 Td (premier flux) Tj ET",
            "BT /F1 12 Tf 20 200 Td (second flux) Tj ET",
        );
        let src = format!(
            "%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 600 300] /Contents [4 0 R 6 0 R] /Resources << /Font << /F1 5 0 R >> >> >> endobj\n4 0 obj << /Length {} >>\nstream\n{a}\nendstream\nendobj\n5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >> endobj\n6 0 obj << /Length {} >>\nstream\n{b}\nendstream\nendobj\n",
            a.len(),
            b.len()
        );
        let doc = Document::from_bytes(src.into_bytes()).unwrap();
        let page = page_of(&doc);
        let before = extract_page_text(&doc, &page).unwrap();
        assert_eq!(before.lines.len(), 2);
        let target = find_ranges(&before, "second")[0];
        apply_edits(
            &doc,
            &[TextEdit {
                page: 0,
                target,
                new_text: "petit".into(),
                style: None,
            }],
        )
        .unwrap();
        let page = page_of(&doc);
        let after = extract_page_text(&doc, &page).unwrap();
        assert_eq!(after.lines[0].text(), "premier flux");
        assert_eq!(after.lines[1].text(), "petit flux");
        assert!(
            (after.lines[0].bbox.x0 - before.lines[0].bbox.x0).abs() < 0.01,
            "le premier flux n'a pas bougé"
        );
    }

    #[test]
    fn quote_operators_keep_their_line_moves() {
        // `'` et `"` déplacent la ligne avant de dessiner (§9.4.3) : la
        // réécriture doit reproduire ce déplacement.
        let doc = pdf(
            "BT /F1 12 Tf 14 TL 20 250 Td (premiere) Tj (deuxieme ligne) ' 2 1 (troisieme ligne) \" ET",
        );
        let page = page_of(&doc);
        let before = extract_page_text(&doc, &page).unwrap();
        assert_eq!(before.lines.len(), 3, "{:?}", before.to_plain());
        let baselines: Vec<f64> = before
            .lines
            .iter()
            .map(crate::text::Line::baseline)
            .collect();
        let target = find_ranges(&before, "deuxieme")[0];
        let second = find_ranges(&before, "troisieme")[0];
        apply_edits(
            &doc,
            &[
                TextEdit {
                    page: 0,
                    target,
                    new_text: "DEUX".into(),
                    style: None,
                },
                TextEdit {
                    page: 0,
                    target: second,
                    new_text: "TROIS".into(),
                    style: None,
                },
            ],
        )
        .unwrap();
        let page = page_of(&doc);
        let after = extract_page_text(&doc, &page).unwrap();
        assert_eq!(after.lines.len(), 3, "{:?}", after.to_plain());
        assert_eq!(after.lines[1].text(), "DEUX ligne");
        assert_eq!(after.lines[2].text(), "TROIS ligne");
        // Les lignes sont restées à leur hauteur : l'interligne des opérateurs
        // `'` et `"` a bien été réémis.
        for (line, baseline) in after.lines.iter().zip(&baselines) {
            assert!((line.baseline() - baseline).abs() < 0.01);
        }
    }

    #[test]
    fn character_and_word_spacing_are_compensated() {
        // Tc, Tw et Tz entrent dans le calcul de l'avance : sans eux la
        // correction `TJ` serait fausse et « fin » bougerait.
        let doc = pdf("BT /F1 12 Tf 0.5 Tc 3 Tw 120 Tz 20 250 Td [(un mot) -20000 (fin)] TJ ET");
        let page = page_of(&doc);
        let before = extract_page_text(&doc, &page).unwrap();
        let x_fin = before.lines[0].words[2].bbox.x0;
        let target = find_ranges(&before, "mot")[0];
        apply_edits(
            &doc,
            &[TextEdit {
                page: 0,
                target,
                new_text: "mots plus longs".into(),
                style: None,
            }],
        )
        .unwrap();
        let page = page_of(&doc);
        let after = extract_page_text(&doc, &page).unwrap();
        let fin = after.lines[0]
            .words
            .last()
            .expect("la ligne garde des mots");
        assert_eq!(fin.text, "fin");
        assert!(
            (fin.bbox.x0 - x_fin).abs() < 0.01,
            "« fin » a bougé : {x_fin} → {}",
            fin.bbox.x0
        );
    }

    #[test]
    fn invalid_ranges_are_refused() {
        let doc = pdf("BT /F1 12 Tf 20 250 Td (abc) Tj ET");
        let bad = TextEdit {
            page: 0,
            target: TextRange {
                line: 9,
                start: 0,
                end: 1,
            },
            new_text: "x".into(),
            style: None,
        };
        assert!(apply_edits(&doc, &[bad]).is_err());
        let bad = TextEdit {
            page: 0,
            target: TextRange {
                line: 0,
                start: 0,
                end: 99,
            },
            new_text: "x".into(),
            style: None,
        };
        assert!(apply_edits(&doc, &[bad]).is_err());
        let bad = TextEdit {
            page: 7,
            target: TextRange {
                line: 0,
                start: 0,
                end: 1,
            },
            new_text: "x".into(),
            style: None,
        };
        assert!(apply_edits(&doc, &[bad]).is_err());
    }

    #[test]
    fn number_formatting() {
        assert_eq!(fmt(1.0), "1");
        assert_eq!(fmt(-0.000_01), "0");
        assert_eq!(fmt(12.345_67), "12.3457");
        assert_eq!(fmt(f64::NAN), "0");
    }

    /// Source du fichier de corpus : une page qui exerce tous les chemins de
    /// l'édition (remplacement dans une chaîne, sur plusieurs opérations,
    /// suppression, changement de style, recomposition d'un paragraphe).
    fn sample_source() -> Vec<u8> {
        let mut content = String::new();
        let line = |content: &mut String, x: f64, y: f64, font: &str, size: f64, text: &str| {
            let _ = writeln!(content, "BT /{font} {size} Tf {x} {y} Td ({text}) Tj ET");
        };
        line(
            &mut content,
            24.0,
            268.0,
            "F2",
            15.0,
            "\\311dition de texte in place",
        );
        line(
            &mut content,
            24.0,
            244.0,
            "F1",
            11.0,
            "1. M\\352me longueur : AVANT | rep\\350re",
        );
        line(
            &mut content,
            24.0,
            226.0,
            "F1",
            11.0,
            "2. Plus court : remplacement | rep\\350re",
        );
        // Lignes en deux morceaux : le second est posé par un `Td`, pas par des
        // espaces, pour que le repère reste net après un remplacement plus long.
        let split = |content: &mut String, y: f64, size: f64, left: &str, gap: f64, right: &str| {
            let _ = writeln!(
                content,
                "BT /F1 {size} Tf 24 {y} Td ({left}) Tj {gap} 0 Td ({right}) Tj ET"
            );
        };
        split(
            &mut content,
            208.0,
            11.0,
            "3. Plus long : eau",
            130.0,
            "| rep\\350re",
        );
        split(
            &mut content,
            190.0,
            11.0,
            "4. Style : normal",
            120.0,
            "et discret",
        );
        line(
            &mut content,
            24.0,
            172.0,
            "F1",
            11.0,
            "5. Suppression : mot-inutile texte conserv\\351",
        );
        // 6. Une opération de dessin par lettre (comme les moteurs Chrome/Skia).
        content.push_str("BT /F3 11 Tf 24 154 Td ");
        let mut first = true;
        // Les « @ » avancent sans rien dessiner : le repère reste lisible même
        // après un remplacement plus long.
        for c in "6. Une lettre par Tj : cible@@@@| repere".chars() {
            let prefix = if first { "" } else { "6.6 0 Td " };
            first = false;
            if c == '@' {
                content.push_str(prefix);
            } else {
                let _ = write!(content, "{prefix}({c}) Tj ");
            }
        }
        content.push_str("ET\n");
        // 7. Paragraphe justifié en Courier : chaque ligne fait exactement 68
        //    caractères, les deux marges sont donc atteintes.
        let text = "Ce paragraphe justifie sera entierement recompose par le moteur d'edition, \
            qui replace les mots dans la meme boite, avec le meme interligne et le meme \
            alignement que l'original.";
        let mut y = 124.0;
        for justified in justify_fixed(text, 68) {
            line(&mut content, 24.0, y, "F3", 9.0, &justified);
            y -= 12.0;
        }
        let src = format!(
            "%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
             2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
             3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 460 300] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R /F2 6 0 R /F3 7 0 R >> >> >> endobj\n\
             4 0 obj << /Length {} >>\nstream\n{content}endstream\nendobj\n\
             5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >> endobj\n\
             6 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >> endobj\n\
             7 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding >> endobj\n",
            content.len()
        );
        src.into_bytes()
    }

    /// Justifie un texte à la largeur `width` en caractères (police à chasse
    /// fixe) : les espaces supplémentaires sont répartis entre les mots.
    fn justify_fixed(text: &str, width: usize) -> Vec<String> {
        let mut lines: Vec<Vec<&str>> = vec![Vec::new()];
        let mut len = 0;
        for word in text.split_whitespace() {
            let extra = if len == 0 { word.len() } else { word.len() + 1 };
            if len + extra > width && len > 0 {
                lines.push(Vec::new());
                len = 0;
            }
            len += if len == 0 { word.len() } else { word.len() + 1 };
            if let Some(last) = lines.last_mut() {
                last.push(word);
            }
        }
        let count = lines.len();
        lines
            .into_iter()
            .enumerate()
            .map(|(i, words)| {
                let text = words.join(" ");
                if i + 1 == count || words.len() < 2 {
                    return text;
                }
                // Répartition des espaces manquants sur les intervalles.
                let missing = width.saturating_sub(text.len());
                let gaps = words.len() - 1;
                let mut out = String::new();
                for (k, word) in words.iter().enumerate() {
                    out.push_str(word);
                    if k < gaps {
                        let share = missing / gaps + usize::from(k < missing % gaps);
                        out.push(' ');
                        for _ in 0..share {
                            out.push(' ');
                        }
                    }
                }
                out
            })
            .collect()
    }

    /// Génère `tests/corpus/synthese/texte-edite-remplacements.pdf`. Lancer
    /// avec `cargo test -p acrux-features -- --ignored generate_edit_text_corpus`.
    #[test]
    #[ignore = "génère le fichier de corpus"]
    fn generate_edit_text_corpus() {
        let doc = Document::from_bytes(sample_source()).unwrap();
        let page = page_of(&doc);
        let text = extract_page_text(&doc, &page).unwrap();
        let range = |needle: &str| find_ranges(&text, needle)[0];
        let red = StyleOverride {
            size: Some(15.0),
            color: Some([0.8, 0.1, 0.1]),
            ..StyleOverride::default()
        };
        let edits = vec![
            TextEdit {
                page: 0,
                target: range("AVANT"),
                new_text: "APRÈS".into(),
                style: None,
            },
            TextEdit {
                page: 0,
                target: range("remplacement"),
                new_text: "court".into(),
                style: None,
            },
            TextEdit {
                page: 0,
                target: range("eau"),
                new_text: "château".into(),
                style: None,
            },
            TextEdit {
                page: 0,
                target: range("normal"),
                new_text: "colorée".into(),
                style: Some(red),
            },
            TextEdit {
                page: 0,
                target: range("mot-inutile "),
                new_text: String::new(),
                style: None,
            },
            TextEdit {
                page: 0,
                target: range("cible"),
                new_text: "ATTEINT".into(),
                style: None,
            },
        ];
        let report = apply_edits_reporting(&doc, &edits).unwrap();
        assert_eq!(report.edits, 6, "{:?}", report.warnings);
        // Recomposition du paragraphe justifié (le dernier de la page).
        let page = page_of(&doc);
        let text = extract_page_text(&doc, &page).unwrap();
        let index = text
            .blocks
            .iter()
            .flat_map(|b| b.paragraphs.iter())
            .position(|p| p.text.starts_with("Ce paragraphe"))
            .unwrap();
        reflow_paragraph(
            &doc,
            &page,
            index,
            "Ce paragraphe a été entièrement recomposé dans sa boîte d'origine : \
             les mots sont replacés à la même taille, avec le même interligne et \
             la même justification qu'avant l'édition.",
            &ReflowOptions::default(),
        )
        .unwrap();
        let saved = doc.save_full().unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/corpus/synthese/texte-edite-remplacements.pdf");
        std::fs::write(&path, saved).unwrap();
        println!("écrit : {}", path.display());
    }
}
