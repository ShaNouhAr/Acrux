//! Étiquettes de page (ISO 32000-2 §12.4.2 ; inventaire Acrobat §1).
//!
//! Le numéro **physique** d'une page (son rang dans l'arbre des pages) n'est
//! pas ce que l'utilisateur lit : dans un livre, la préface est numérotée
//! i, ii, iii et le premier chapitre recommence à 1. C'est à cela que sert
//! `/PageLabels` du catalogue : un **arbre de nombres** (§7.9.7) qui associe
//! à l'indice de la première page d'une plage un dictionnaire décrivant sa
//! numérotation.
//!
//! Sans ce module, l'application afficherait « 3 sur 240 » là où Acrobat
//! affiche « iii sur 240 », et taper « iv » dans le champ de page ne
//! mènerait nulle part.
//!
//! ## Le cas alphabétique
//!
//! Les styles `a` et `A` ne comptent **pas** en base 26. §12.4.2, table 159 :
//! « A à Z pour les 26 premières pages, AA à ZZ pour les 26 suivantes, AAA à
//! ZZZ pour les 26 d'après, et ainsi de suite ». La 27ᵉ page d'une plage
//! alphabétique s'étiquette donc `aa` et la 28ᵉ `bb`, jamais `ab`. C'est
//! contre-intuitif, c'est la norme, et c'est ce que fait Acrobat : produire
//! `ab` ferait diverger notre affichage de celui de tous les autres lecteurs.

use std::collections::HashSet;

use acrux_core::{Error, Result};
use acrux_document::text::decode_text_string;
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef};

use crate::annotations::encode_text;

/// Profondeur maximale explorée dans l'arbre de nombres (anti-cycle).
const MAX_DEPTH: usize = 64;

/// Nombre maximal d'entrées par feuille de l'arbre reconstruit.
const LEAF_SIZE: usize = 32;

/// Style de numérotation d'une plage (`/S`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelStyle {
    /// `/S /D` : 1, 2, 3…
    Decimal,
    /// `/S /r` : i, ii, iii…
    LowerRoman,
    /// `/S /R` : I, II, III…
    UpperRoman,
    /// `/S /a` : a, b, … z, aa, bb… (§12.4.2, table 159).
    LowerLetters,
    /// `/S /A` : A, B, … Z, AA, BB…
    UpperLetters,
}

impl LabelStyle {
    /// Lettre du `/S` correspondant.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            LabelStyle::Decimal => "D",
            LabelStyle::LowerRoman => "r",
            LabelStyle::UpperRoman => "R",
            LabelStyle::LowerLetters => "a",
            LabelStyle::UpperLetters => "A",
        }
    }

    /// Style d'un `/S` lu dans le fichier ; `None` si la valeur est inconnue.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Some(match code {
            "D" => LabelStyle::Decimal,
            "r" => LabelStyle::LowerRoman,
            "R" => LabelStyle::UpperRoman,
            "a" => LabelStyle::LowerLetters,
            "A" => LabelStyle::UpperLetters,
            _ => return None,
        })
    }

    /// Rend `value` (1 pour la première page de la plage) dans ce style.
    #[must_use]
    pub fn render(self, value: u32) -> String {
        match self {
            LabelStyle::Decimal => value.to_string(),
            LabelStyle::LowerRoman => roman(value).to_lowercase(),
            LabelStyle::UpperRoman => roman(value),
            LabelStyle::LowerLetters => letters(value, b'a'),
            LabelStyle::UpperLetters => letters(value, b'A'),
        }
    }
}

/// Une plage d'étiquettes : elle court de `first_page` jusqu'à la plage
/// suivante (ou la fin du document).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelRange {
    /// Première page de la plage (0 = première page du document).
    pub first_page: usize,
    /// Style de numérotation. `None` (`/S` absent) : seul le préfixe
    /// s'affiche, identique sur toutes les pages de la plage.
    pub style: Option<LabelStyle>,
    /// `/P` : texte placé devant le numéro (« Annexe- », « A-»…).
    pub prefix: String,
    /// `/St` : valeur de la première page de la plage (au moins 1).
    pub start: u32,
}

impl Default for LabelRange {
    fn default() -> Self {
        Self {
            first_page: 0,
            style: Some(LabelStyle::Decimal),
            prefix: String::new(),
            start: 1,
        }
    }
}

/// Chiffres romains d'un entier positif. Au-delà de 3999, les milliers sont
/// écrits en `M` répétés : c'est la convention des lecteurs PDF, faute de
/// notation normalisée pour les grands nombres.
fn roman(value: u32) -> String {
    const TABLE: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    if value == 0 {
        return String::new();
    }
    let mut rest = value;
    let mut out = String::new();
    for (amount, digits) in TABLE {
        while rest >= amount {
            out.push_str(digits);
            rest -= amount;
        }
    }
    out
}

/// Étiquette alphabétique : la lettre `((value - 1) mod 26)` répétée
/// `((value - 1) div 26) + 1` fois (§12.4.2, table 159).
fn letters(value: u32, base: u8) -> String {
    if value == 0 {
        return String::new();
    }
    let index = value - 1;
    let letter = char::from(base + u8::try_from(index % 26).unwrap_or(0));
    let repeat = (index / 26) as usize + 1;
    // Un document à des millions de pages ne doit pas faire exploser la
    // mémoire pour une étiquette : on borne la répétition.
    std::iter::repeat_n(letter, repeat.min(64)).collect()
}

// ---------------------------------------------------------------------------
// Lecture
// ---------------------------------------------------------------------------

/// Plages déclarées par `/PageLabels`, triées par page de départ.
///
/// # Errors
/// Catalogue illisible.
pub fn read_label_ranges(doc: &Document) -> Result<Vec<LabelRange>> {
    let catalog = doc.catalog()?;
    let Some(tree) = doc.dict_get(&catalog, "PageLabels").ok().flatten() else {
        return Ok(Vec::new());
    };
    let Some(tree) = tree.as_dict() else {
        return Ok(Vec::new());
    };
    let mut pairs = Vec::new();
    let mut visited = HashSet::new();
    walk_numbers(doc, tree, &mut pairs, &mut visited, 0);
    let mut out: Vec<LabelRange> = pairs
        .into_iter()
        .filter_map(|(key, value)| range_from(doc, key, &value))
        .collect();
    out.sort_by_key(|r| r.first_page);
    out.dedup_by_key(|r| r.first_page);
    Ok(out)
}

/// Parcours d'un arbre de nombres (§7.9.7) : feuilles `/Nums`, nœuds `/Kids`.
fn walk_numbers(
    doc: &Document,
    node: &Dict,
    out: &mut Vec<(i64, Object)>,
    visited: &mut HashSet<u32>,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        return;
    }
    if let Some(nums) = doc.dict_get(node, "Nums").ok().flatten() {
        if let Some(list) = nums.as_array() {
            for pair in list.chunks_exact(2) {
                let Ok(key) = doc.resolve(&pair[0]) else {
                    continue;
                };
                if let Some(index) = key.as_i64() {
                    out.push((index, pair[1].clone()));
                }
            }
        }
    }
    let Some(kids) = doc.dict_get(node, "Kids").ok().flatten() else {
        return;
    };
    let Some(kids) = kids.as_array() else { return };
    for kid in kids {
        if let Object::Reference(r) = kid {
            if !visited.insert(r.number) {
                continue;
            }
        }
        let Ok(kid) = doc.resolve(kid) else { continue };
        if let Some(d) = kid.as_dict() {
            walk_numbers(doc, d, out, visited, depth + 1);
        }
    }
}

/// Convertit une entrée `clé → dictionnaire` en [`LabelRange`].
fn range_from(doc: &Document, key: i64, value: &Object) -> Option<LabelRange> {
    let first_page = usize::try_from(key).ok()?;
    let resolved = doc.resolve(value).ok()?;
    let d = resolved.as_dict()?;
    let style = doc
        .dict_get(d, "S")
        .ok()
        .flatten()
        .and_then(|o| o.as_name().map(Name::as_str))
        .and_then(|code| LabelStyle::from_code(&code));
    let prefix = match doc.dict_get(d, "P").ok().flatten().as_deref() {
        Some(Object::String(s)) => decode_text_string(s),
        _ => String::new(),
    };
    let start = doc
        .dict_get(d, "St")
        .ok()
        .flatten()
        .and_then(|o| o.as_i64())
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v >= 1)
        .unwrap_or(1);
    Some(LabelRange {
        first_page,
        style,
        prefix,
        start,
    })
}

/// Étiquette de chaque page du document, dans l'ordre.
///
/// Sans `/PageLabels` — le cas de la grande majorité des fichiers — chaque
/// page reçoit son numéro décimal, de sorte que l'appelant n'a jamais de cas
/// particulier à traiter.
///
/// # Errors
/// Arbre des pages ou catalogue illisible.
pub fn read_page_labels(doc: &Document) -> Result<Vec<String>> {
    let count = collect_pages(doc)?.len();
    let ranges = read_label_ranges(doc)?;
    Ok(labels_for(&ranges, count))
}

/// Étiquettes engendrées par des plages pour un document de `count` pages.
/// Les pages situées avant la première plage retombent sur leur numéro
/// décimal, comme le fait Acrobat devant un `/PageLabels` incomplet.
#[must_use]
pub fn labels_for(ranges: &[LabelRange], count: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(count);
    for page in 0..count {
        let range = ranges
            .iter()
            .rev()
            .find(|r| r.first_page <= page && r.first_page < count);
        out.push(match range {
            Some(r) => {
                let offset = u32::try_from(page - r.first_page).unwrap_or(0);
                let number = r.style.map(|s| s.render(r.start.saturating_add(offset)));
                match number {
                    Some(n) => format!("{}{n}", r.prefix),
                    None => r.prefix.clone(),
                }
            }
            None => (page + 1).to_string(),
        });
    }
    out
}

/// Page portant l'étiquette donnée (0 = première page).
///
/// La comparaison ignore les espaces de bordure et la casse : « IV », « iv »
/// et « Iv » désignent la même page, comme dans le champ de page d'Acrobat.
/// En cas d'étiquettes répétées (un `/P` sans `/S`), la première l'emporte.
#[must_use]
pub fn page_for_label(labels: &[String], label: &str) -> Option<usize> {
    let wanted = label.trim();
    if let Some(i) = labels.iter().position(|l| l == wanted) {
        return Some(i);
    }
    let folded = wanted.to_lowercase();
    labels.iter().position(|l| l.to_lowercase() == folded)
}

// ---------------------------------------------------------------------------
// Écriture
// ---------------------------------------------------------------------------

/// Référence du catalogue (`/Root` du trailer).
fn catalog_ref(doc: &Document) -> Result<ObjectRef> {
    match doc.trailer().get(&Name::new("Root")) {
        Some(Object::Reference(r)) => Ok(*r),
        _ => Err(Error::Corrupt(
            "catalogue absent ou direct : impossible d'y écrire les étiquettes".into(),
        )),
    }
}

/// Écrit `/PageLabels`. Une liste vide retire l'entrée : le document revient
/// à la numérotation décimale implicite.
///
/// Les plages sont triées et dédoublonnées. §12.4.2 exige qu'une plage
/// commence à la page 0 ; si l'appelant ne l'a pas fait, une plage décimale
/// est ajoutée d'office plutôt que de produire un fichier non conforme.
///
/// # Errors
/// Catalogue illisible.
pub fn set_page_labels(doc: &Document, ranges: &[LabelRange]) -> Result<()> {
    let catalog_ref = catalog_ref(doc)?;
    let mut catalog = doc.catalog()?;
    // L'ancien arbre est libéré : sans cela, chaque enregistrement laisserait
    // derrière lui les nœuds de la version précédente.
    delete_old_tree(doc, &catalog);
    if ranges.is_empty() {
        catalog.remove(&Name::new("PageLabels"));
        doc.set(catalog_ref, Object::Dict(catalog));
        return Ok(());
    }
    let mut sorted = ranges.to_vec();
    sorted.sort_by_key(|r| r.first_page);
    sorted.dedup_by_key(|r| r.first_page);
    if sorted.first().is_some_and(|r| r.first_page != 0) {
        sorted.insert(0, LabelRange::default());
    }
    let entries: Vec<(i64, Object)> = sorted
        .iter()
        .map(|r| (i64::try_from(r.first_page).unwrap_or(0), range_dict(r)))
        .collect();
    let root = build_number_tree(doc, &entries);
    catalog.insert(Name::new("PageLabels"), Object::Reference(root));
    doc.set(catalog_ref, Object::Dict(catalog));
    Ok(())
}

/// Dictionnaire d'une plage (`/S`, `/P`, `/St`).
fn range_dict(range: &LabelRange) -> Object {
    let mut d = Dict::new();
    if let Some(style) = range.style {
        d.insert(Name::new("S"), Object::Name(Name::new(style.code())));
    }
    if !range.prefix.is_empty() {
        d.insert(Name::new("P"), Object::String(encode_text(&range.prefix)));
    }
    if range.start > 1 {
        d.insert(Name::new("St"), Object::Integer(i64::from(range.start)));
    }
    Object::Dict(d)
}

/// Supprime les nœuds indirects de l'arbre `/PageLabels` existant.
fn delete_old_tree(doc: &Document, catalog: &Dict) {
    let mut stack = match catalog.get(&Name::new("PageLabels")) {
        Some(Object::Reference(r)) => vec![*r],
        _ => Vec::new(),
    };
    let mut seen = HashSet::new();
    let mut depth = 0;
    while let Some(r) = stack.pop() {
        depth += 1;
        if depth > 1024 || !seen.insert(r.number) {
            continue;
        }
        if let Ok(obj) = doc.get(r) {
            if let Some(d) = obj.as_dict() {
                if let Some(Object::Array(kids)) = d.get(&Name::new("Kids")) {
                    for kid in kids {
                        if let Object::Reference(k) = kid {
                            stack.push(*k);
                        }
                    }
                }
            }
        }
        doc.delete(r);
    }
}

/// Construit un arbre de nombres équilibré et retourne sa racine.
fn build_number_tree(doc: &Document, entries: &[(i64, Object)]) -> ObjectRef {
    if entries.len() <= LEAF_SIZE {
        let mut node = Dict::new();
        node.insert(Name::new("Nums"), nums_array(entries));
        return doc.add(Object::Dict(node));
    }
    let parts = entries.len().div_ceil(LEAF_SIZE);
    let base = entries.len() / parts;
    let extra = entries.len() % parts;
    let mut kids = Vec::with_capacity(parts);
    let mut start = 0;
    for i in 0..parts {
        let size = base + usize::from(i < extra);
        let part = &entries[start..start + size];
        start += size;
        let mut node = Dict::new();
        node.insert(Name::new("Nums"), nums_array(part));
        node.insert(
            Name::new("Limits"),
            Object::Array(vec![
                Object::Integer(part[0].0),
                Object::Integer(part[part.len() - 1].0),
            ]),
        );
        kids.push(Object::Reference(doc.add(Object::Dict(node))));
    }
    let mut root = Dict::new();
    root.insert(Name::new("Kids"), Object::Array(kids));
    doc.add(Object::Dict(root))
}

/// `/Nums [clé valeur …]`.
fn nums_array(entries: &[(i64, Object)]) -> Object {
    let mut out = Vec::with_capacity(entries.len() * 2);
    for (key, value) in entries {
        out.push(Object::Integer(*key));
        out.push(value.clone());
    }
    Object::Array(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod tests {
    use super::*;

    fn build_pdf(objects: &[(u32, String)]) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3".to_vec();
        let mut offsets: Vec<(u32, usize)> = Vec::new();
        for (n, body) in objects {
            offsets.push((*n, out.len()));
            out.extend_from_slice(format!("\n{n} 0 obj\n{body}\nendobj").as_bytes());
        }
        let max = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
        let xref = out.len();
        out.extend_from_slice(format!("\nxref\n0 {}\n0000000000 65535 f ", max + 1).as_bytes());
        for n in 1..=max {
            match offsets.iter().find(|(m, _)| *m == n) {
                Some((_, o)) => out.extend_from_slice(format!("{:010} 00000 n ", o + 1).as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f "),
            }
        }
        out.extend_from_slice(
            format!(
                "\ntrailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
                max + 1,
                xref + 1
            )
            .as_bytes(),
        );
        out
    }

    /// Document de `count` pages, avec l'entrée `/PageLabels` donnée (ou aucune).
    fn doc_with(count: u32, page_labels: &str) -> Document {
        let kids: Vec<String> = (0..count).map(|i| format!("{} 0 R", 3 + i)).collect();
        let mut objects = vec![
            (
                1,
                format!("<< /Type /Catalog /Pages 2 0 R {page_labels} >>"),
            ),
            (
                2,
                format!(
                    "<< /Type /Pages /Kids [{}] /Count {count} >>",
                    kids.join(" ")
                ),
            ),
        ];
        for i in 0..count {
            objects.push((
                3 + i,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".into(),
            ));
        }
        Document::from_bytes(build_pdf(&objects)).unwrap()
    }

    #[test]
    fn roman_numerals_follow_the_usual_rules() {
        assert_eq!(roman(1), "I");
        assert_eq!(roman(4), "IV");
        assert_eq!(roman(9), "IX");
        assert_eq!(roman(14), "XIV");
        assert_eq!(roman(40), "XL");
        assert_eq!(roman(1987), "MCMLXXXVII");
        assert_eq!(roman(4000), "MMMM");
        assert_eq!(roman(0), "");
        assert_eq!(LabelStyle::LowerRoman.render(4), "iv");
    }

    /// §12.4.2, table 159 : au-delà de z, la lettre est **répétée**.
    #[test]
    fn letters_repeat_past_z() {
        let a = LabelStyle::LowerLetters;
        assert_eq!(a.render(1), "a");
        assert_eq!(a.render(26), "z");
        assert_eq!(a.render(27), "aa");
        assert_eq!(a.render(28), "bb");
        assert_eq!(a.render(52), "zz");
        assert_eq!(a.render(53), "aaa");
        assert_eq!(LabelStyle::UpperLetters.render(27), "AA");
        assert_eq!(LabelStyle::UpperLetters.render(2), "B");
    }

    #[test]
    fn a_document_without_page_labels_falls_back_to_decimal() {
        let doc = doc_with(3, "");
        assert_eq!(read_page_labels(&doc).unwrap(), ["1", "2", "3"]);
        assert!(read_label_ranges(&doc).unwrap().is_empty());
    }

    #[test]
    fn mixed_ranges_are_rendered() {
        // i, ii, puis 1, 2, puis Annexe-A.
        let doc = doc_with(
            5,
            "/PageLabels << /Nums [0 << /S /r >> 2 << /S /D >> 4 << /S /A /P (Annexe-) >>] >>",
        );
        assert_eq!(
            read_page_labels(&doc).unwrap(),
            ["i", "ii", "1", "2", "Annexe-A"]
        );
        let labels = read_page_labels(&doc).unwrap();
        assert_eq!(page_for_label(&labels, "ii"), Some(1));
        assert_eq!(page_for_label(&labels, "II"), Some(1));
        assert_eq!(page_for_label(&labels, " 2 "), Some(3));
        assert_eq!(page_for_label(&labels, "Annexe-A"), Some(4));
        assert_eq!(page_for_label(&labels, "zzz"), None);
    }

    #[test]
    fn a_range_without_a_style_shows_only_its_prefix() {
        let doc = doc_with(
            4,
            "/PageLabels << /Nums [0 << /S /D >> 2 << /P (Couverture) >>] >>",
        );
        assert_eq!(
            read_page_labels(&doc).unwrap(),
            ["1", "2", "Couverture", "Couverture"]
        );
        let ranges = read_label_ranges(&doc).unwrap();
        assert_eq!(ranges[1].style, None);
        assert_eq!(ranges[1].prefix, "Couverture");
    }

    #[test]
    fn a_two_level_number_tree_is_read() {
        let doc = doc_with(4, "/PageLabels 20 0 R");
        // Le document ci-dessus n'a pas l'objet 20 : on le construit à la main.
        let objects = vec![
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /PageLabels 20 0 R >>".to_string(),
            ),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R] /Count 4 >>".to_string(),
            ),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 9 9] >>".into(),
            ),
            (
                4,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 9 9] >>".into(),
            ),
            (
                5,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 9 9] >>".into(),
            ),
            (
                6,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 9 9] >>".into(),
            ),
            (20, "<< /Kids [21 0 R 22 0 R] >>".into()),
            (21, "<< /Limits [0 0] /Nums [0 << /S /R /St 7 >>] >>".into()),
            (
                22,
                "<< /Limits [2 2] /Nums [2 << /S /D /P (B-) >>] >>".into(),
            ),
        ];
        drop(doc);
        let doc = Document::from_bytes(build_pdf(&objects)).unwrap();
        assert_eq!(
            read_page_labels(&doc).unwrap(),
            ["VII", "VIII", "B-1", "B-2"]
        );
    }

    #[test]
    fn writing_then_reading_round_trips() {
        let doc = doc_with(6, "");
        let ranges = vec![
            LabelRange {
                first_page: 0,
                style: Some(LabelStyle::LowerRoman),
                prefix: String::new(),
                start: 1,
            },
            LabelRange {
                first_page: 2,
                style: Some(LabelStyle::Decimal),
                prefix: String::new(),
                start: 1,
            },
            LabelRange {
                first_page: 4,
                style: Some(LabelStyle::UpperLetters),
                prefix: "Annexe-".into(),
                start: 1,
            },
        ];
        set_page_labels(&doc, &ranges).unwrap();
        let back = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        assert_eq!(
            read_page_labels(&back).unwrap(),
            ["i", "ii", "1", "2", "Annexe-A", "Annexe-B"]
        );
        assert_eq!(read_label_ranges(&back).unwrap(), ranges);
    }

    #[test]
    fn a_first_range_that_does_not_start_at_zero_is_completed() {
        let doc = doc_with(4, "");
        set_page_labels(
            &doc,
            &[LabelRange {
                first_page: 2,
                style: Some(LabelStyle::UpperRoman),
                prefix: String::new(),
                start: 1,
            }],
        )
        .unwrap();
        // Une plage décimale est ajoutée à la page 0, comme l'exige §12.4.2.
        assert_eq!(read_page_labels(&doc).unwrap(), ["1", "2", "I", "II"]);
        assert_eq!(read_label_ranges(&doc).unwrap()[0].first_page, 0);
    }

    #[test]
    fn an_empty_list_removes_the_entry() {
        let doc = doc_with(3, "/PageLabels << /Nums [0 << /S /r >>] >>");
        assert_eq!(read_page_labels(&doc).unwrap(), ["i", "ii", "iii"]);
        set_page_labels(&doc, &[]).unwrap();
        assert_eq!(read_page_labels(&doc).unwrap(), ["1", "2", "3"]);
        assert!(!doc
            .catalog()
            .unwrap()
            .contains_key(&Name::new("PageLabels")));
    }

    #[test]
    fn a_start_number_shifts_the_range() {
        let doc = doc_with(3, "/PageLabels << /Nums [0 << /S /D /St 42 >>] >>");
        assert_eq!(read_page_labels(&doc).unwrap(), ["42", "43", "44"]);
    }

    #[test]
    fn labels_before_the_first_range_stay_decimal() {
        // Fichier fautif : la première plage commence à la page 2.
        let doc = doc_with(4, "/PageLabels << /Nums [2 << /S /r >>] >>");
        assert_eq!(read_page_labels(&doc).unwrap(), ["1", "2", "i", "ii"]);
    }

    #[test]
    fn many_ranges_build_a_two_level_tree() {
        let doc = doc_with(1, "");
        let ranges: Vec<LabelRange> = (0..100)
            .map(|i| LabelRange {
                first_page: i,
                style: Some(LabelStyle::Decimal),
                prefix: format!("{i}-"),
                start: 1,
            })
            .collect();
        set_page_labels(&doc, &ranges).unwrap();
        let read = read_label_ranges(&doc).unwrap();
        assert_eq!(read.len(), 100);
        assert_eq!(read[99].prefix, "99-");
        let catalog = doc.catalog().unwrap();
        let tree = doc.dict_get(&catalog, "PageLabels").unwrap().unwrap();
        let tree = tree.as_dict().unwrap();
        assert!(
            tree.contains_key(&Name::new("Kids")),
            "arbre à deux niveaux"
        );
    }
}
