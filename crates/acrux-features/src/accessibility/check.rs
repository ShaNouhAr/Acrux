//! Vérificateur d'accessibilité : règles de **PDF/UA-1** (ISO 14289-1) et de
//! **WCAG 2.1** pour le contraste, appliquées au document balisé.
//!
//! Chaque règle porte un identifiant stable (colonne de gauche de [`RULES`])
//! que l'on retrouve dans le rapport, dans la sortie JSON et dans les tests.
//! Une règle ne se déclenche jamais deux fois pour la même cause : les
//! occurrences répétées sur une page sont regroupées avec leur compte.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;

use acrux_core::{Rect, Result};
use acrux_document::text::decode_text_string;
use acrux_document::{collect_pages, Document, Name, Object};

use super::marked::{scan_page, PageMarks};
use super::{read_structure, StructTree};
use crate::text::{extract_page_text, PageText};

/// Sévérité d'un problème.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Information : à vérifier à la main, pas forcément un défaut.
    Info,
    /// Avertissement : conforme au sens strict, mais gênant pour l'utilisateur.
    Warning,
    /// Erreur : non conforme à PDF/UA.
    Error,
}

impl Severity {
    /// Libellé court pour l'affichage.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "avertissement",
            Severity::Error => "erreur",
        }
    }
}

/// Un problème d'accessibilité.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Gravité.
    pub severity: Severity,
    /// Identifiant de règle (voir [`RULES`]).
    pub rule: &'static str,
    /// Message en français, destiné à l'utilisateur.
    pub message: String,
    /// Page concernée (0 = première), si la règle est localisée.
    pub page: Option<usize>,
    /// Élément de structure concerné (`H2 « Introduction »`), si connu.
    pub element: Option<String>,
}

/// Les règles appliquées, avec leur explication. Sert à documenter la sortie
/// et à vérifier dans les tests qu'aucune règle n'a disparu.
pub const RULES: [(&str, &str); 17] = [
    (
        "document-tagged",
        "le document est balisé (/MarkInfo /Marked true et /StructTreeRoot)",
    ),
    (
        "structure-types",
        "les types de structure sont normalisés ou traduits par la /RoleMap",
    ),
    (
        "document-lang",
        "la langue du document est déclarée (/Lang du catalogue)",
    ),
    (
        "passage-lang",
        "un passage dans une autre langue déclare la sienne",
    ),
    ("document-title", "le document a un titre (/Info /Title)"),
    (
        "display-doc-title",
        "le lecteur affiche le titre (/ViewerPreferences /DisplayDocTitle true)",
    ),
    (
        "figure-alt",
        "chaque figure a un texte de remplacement (/Alt ou /ActualText)",
    ),
    (
        "link-alt",
        "chaque lien est balisé et porte un texte de substitution",
    ),
    (
        "reading-order",
        "l'ordre de l'arbre suit l'ordre géométrique du texte",
    ),
    (
        "heading-levels",
        "les titres sont hiérarchisés sans saut de niveau",
    ),
    (
        "table-headers",
        "les tableaux ont des en-têtes TH avec /Scope ou /Headers",
    ),
    (
        "list-structure",
        "les listes sont formées de L > LI > LBody",
    ),
    (
        "contrast",
        "le contraste texte / fond atteint 4,5:1 (3:1 pour le grand texte)",
    ),
    (
        "image-only-text",
        "une page n'est pas une simple image de texte",
    ),
    (
        "font-tounicode",
        "le texte est extractible (/ToUnicode ou encodage connu)",
    ),
    (
        "field-tu",
        "chaque champ de formulaire a une description /TU",
    ),
    (
        "untagged-content",
        "aucun contenu n'est hors du balisage et non marqué /Artifact",
    ),
];

/// Rapport complet.
#[derive(Debug, Clone, Default)]
pub struct AccessibilityReport {
    /// Problèmes, triés par sévérité décroissante puis par page.
    pub issues: Vec<Issue>,
    /// Nombre de pages examinées.
    pub pages: usize,
    /// Le document possède un arbre de structure exploitable.
    pub tagged: bool,
}

impl AccessibilityReport {
    /// Nombre de problèmes d'une sévérité donnée.
    #[must_use]
    pub fn count(&self, severity: Severity) -> usize {
        self.issues
            .iter()
            .filter(|i| i.severity == severity)
            .count()
    }

    /// Vrai si aucune erreur n'a été relevée (les avertissements sont tolérés).
    #[must_use]
    pub fn is_conforming(&self) -> bool {
        self.count(Severity::Error) == 0
    }

    /// Vrai si la règle a produit au moins un problème.
    #[must_use]
    pub fn has(&self, rule: &str) -> bool {
        self.issues.iter().any(|i| i.rule == rule)
    }

    /// Résumé « 3 erreurs, 2 avertissements, 1 information ».
    #[must_use]
    pub fn summary(&self) -> String {
        let plural = |n: usize, s: &str| {
            if n > 1 {
                format!("{n} {s}s")
            } else {
                format!("{n} {s}")
            }
        };
        format!(
            "{}, {}, {}",
            plural(self.count(Severity::Error), "erreur"),
            plural(self.count(Severity::Warning), "avertissement"),
            plural(self.count(Severity::Info), "information")
        )
    }

    /// Rapport JSON (écrit à la main : le projet n'a aucune dépendance).
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\n");
        let _ = writeln!(out, "  \"pages\": {},", self.pages);
        let _ = writeln!(out, "  \"tagged\": {},", self.tagged);
        let _ = writeln!(out, "  \"errors\": {},", self.count(Severity::Error));
        let _ = writeln!(out, "  \"warnings\": {},", self.count(Severity::Warning));
        let _ = writeln!(out, "  \"infos\": {},", self.count(Severity::Info));
        out.push_str("  \"issues\": [\n");
        for (i, issue) in self.issues.iter().enumerate() {
            out.push_str("    {");
            let _ = write!(out, "\"severity\": \"{}\", ", issue.severity.label());
            let _ = write!(out, "\"rule\": \"{}\", ", issue.rule);
            let _ = write!(out, "\"message\": \"{}\"", escape_json(&issue.message));
            if let Some(p) = issue.page {
                let _ = write!(out, ", \"page\": {}", p + 1);
            }
            if let Some(e) = &issue.element {
                let _ = write!(out, ", \"element\": \"{}\"", escape_json(e));
            }
            out.push('}');
            if i + 1 < self.issues.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ]\n}\n");
        out
    }
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Accumulateur qui limite le bavardage : au plus [`MAX_PER_RULE`] messages
/// par règle, puis un message de regroupement.
const MAX_PER_RULE: usize = 8;

#[derive(Default)]
struct Collector {
    issues: Vec<Issue>,
    counts: BTreeMap<&'static str, usize>,
    hidden: BTreeMap<&'static str, usize>,
}

impl Collector {
    fn add(
        &mut self,
        severity: Severity,
        rule: &'static str,
        message: impl Into<String>,
        page: Option<usize>,
        element: Option<String>,
    ) {
        let count = self.counts.entry(rule).or_insert(0);
        *count += 1;
        if *count > MAX_PER_RULE {
            *self.hidden.entry(rule).or_insert(0) += 1;
            return;
        }
        self.issues.push(Issue {
            severity,
            rule,
            message: message.into(),
            page,
            element,
        });
    }

    fn finish(mut self) -> Vec<Issue> {
        for (rule, n) in std::mem::take(&mut self.hidden) {
            self.issues.push(Issue {
                severity: Severity::Info,
                rule,
                message: format!("{n} autre(s) occurrence(s) de cette règle, non détaillées"),
                page: None,
                element: None,
            });
        }
        self.issues.sort_by(|a, b| {
            b.severity
                .cmp(&a.severity)
                .then(a.page.cmp(&b.page))
                .then(a.rule.cmp(b.rule))
        });
        self.issues
    }
}

/// Vérifie l'accessibilité d'un document au sens de PDF/UA-1.
///
/// # Errors
/// Catalogue ou arbre des pages illisible.
pub fn check_accessibility(doc: &Document) -> Result<AccessibilityReport> {
    let tree = read_structure(doc)?;
    let pages = collect_pages(doc)?;
    let mut marks: Vec<PageMarks> = Vec::with_capacity(pages.len());
    let mut texts: Vec<PageText> = Vec::with_capacity(pages.len());
    for page in &pages {
        marks.push(scan_page(doc, page).unwrap_or_default());
        texts.push(extract_page_text(doc, page).unwrap_or_default());
    }
    let mut c = Collector::default();

    check_tagged(doc, &tree, &mut c);
    check_metadata(doc, &tree, &mut c);
    check_types(&tree, &mut c);
    check_figures(&tree, &mut c);
    check_links(doc, &tree, &pages, &mut c);
    check_headings(&tree, &mut c);
    check_tables(&tree, &mut c);
    check_lists(&tree, &mut c);
    check_reading_order(&tree, &marks, &texts, &mut c);
    check_untagged(&tree, &marks, &mut c);
    check_contrast(&marks, &mut c);
    check_image_only(doc, &pages, &marks, &mut c);
    check_fonts(&marks, &mut c);
    check_fields(doc, &tree, &mut c);
    check_passage_lang(&tree, &mut c);

    Ok(AccessibilityReport {
        issues: c.finish(),
        pages: pages.len(),
        tagged: tree.is_tagged(),
    })
}

// ---------------------------------------------------------------------------
// Règles
// ---------------------------------------------------------------------------

/// Document balisé : `/StructTreeRoot` **et** `/MarkInfo /Marked true` (§14.7.1).
fn check_tagged(doc: &Document, tree: &StructTree, c: &mut Collector) {
    if tree.root.is_none() {
        c.add(
            Severity::Error,
            "document-tagged",
            "le document n'a pas d'arbre de structure (/StructTreeRoot absent) : \
             son contenu n'est pas lisible par une synthèse vocale",
            None,
            None,
        );
        return;
    }
    if tree.children.is_empty() {
        c.add(
            Severity::Error,
            "document-tagged",
            "l'arbre de structure est vide (/StructTreeRoot sans /K)",
            None,
            None,
        );
    }
    if !tree.marked {
        c.add(
            Severity::Error,
            "document-tagged",
            "/MarkInfo /Marked true est absent : le document ne se déclare pas balisé",
            None,
            None,
        );
    }
    if tree.suspects {
        c.add(
            Severity::Warning,
            "document-tagged",
            "/MarkInfo /Suspects true : le producteur déclare son balisage douteux",
            None,
            None,
        );
    }
    if !tree.has_parent_tree {
        c.add(
            Severity::Error,
            "document-tagged",
            "/ParentTree absent : le contenu marqué ne peut pas remonter vers sa structure",
            None,
            None,
        );
    }
    let _ = doc;
}

/// Langue, titre et affichage du titre (ISO 14289-1 §7.2 et §7.1).
fn check_metadata(doc: &Document, tree: &StructTree, c: &mut Collector) {
    match &tree.lang {
        Some(lang) if !lang.trim().is_empty() => {}
        _ => c.add(
            Severity::Error,
            "document-lang",
            "la langue du document n'est pas déclarée (/Lang absent du catalogue)",
            None,
            None,
        ),
    }
    let title = super::info_dict(doc).and_then(|info| {
        match doc.dict_get(&info, "Title").ok().flatten().as_deref() {
            Some(Object::String(s)) => Some(decode_text_string(s)),
            _ => None,
        }
    });
    if title.as_ref().is_none_or(|t| t.trim().is_empty()) {
        c.add(
            Severity::Error,
            "document-title",
            "le document n'a pas de titre (/Info /Title vide ou absent)",
            None,
            None,
        );
    }
    let preferences = doc.catalog().ok().and_then(|cat| {
        doc.dict_get(&cat, "ViewerPreferences")
            .ok()
            .flatten()
            .and_then(|v| v.as_dict().cloned())
    });
    let display = preferences.is_some_and(|v| {
        matches!(
            doc.dict_get(&v, "DisplayDocTitle")
                .ok()
                .flatten()
                .as_deref(),
            Some(Object::Bool(true))
        )
    });
    if !display {
        c.add(
            Severity::Error,
            "display-doc-title",
            "/ViewerPreferences /DisplayDocTitle true est absent : le lecteur affichera \
             le nom du fichier au lieu du titre",
            None,
            None,
        );
    }
}

/// Tous les types de structure doivent être normalisés ou traduits (§14.8.4).
fn check_types(tree: &StructTree, c: &mut Collector) {
    for (e, _) in tree.elements() {
        if !e.is_standard() {
            c.add(
                Severity::Error,
                "structure-types",
                format!(
                    "type de structure « {} » inconnu et absent de la /RoleMap",
                    e.raw_kind
                ),
                e.page,
                Some(e.label()),
            );
        }
    }
}

/// Figures : `/Alt` ou `/ActualText` obligatoire (ISO 14289-1 §7.3).
fn check_figures(tree: &StructTree, c: &mut Collector) {
    for (e, _) in tree.elements() {
        if e.kind != "Figure" && e.kind != "Formula" {
            continue;
        }
        let alt = e.alt.as_ref().is_some_and(|a| !a.trim().is_empty());
        let actual = e.actual_text.as_ref().is_some_and(|a| !a.trim().is_empty());
        if !alt && !actual {
            c.add(
                Severity::Error,
                "figure-alt",
                format!("{} sans texte de remplacement (/Alt)", e.kind),
                e.page,
                Some(e.label()),
            );
        }
    }
}

/// Liens : élément `Link` porteur de texte, et annotations `/Link` balisées
/// avec une description (ISO 14289-1 §7.18).
fn check_links(
    doc: &Document,
    tree: &StructTree,
    pages: &[acrux_document::Page],
    c: &mut Collector,
) {
    let mut referenced: HashSet<u32> = HashSet::new();
    for (e, _) in tree.elements() {
        for r in &e.objects {
            referenced.insert(r.number);
        }
        if e.kind != "Link" {
            continue;
        }
        let has_alt = e.alt.as_ref().is_some_and(|a| !a.trim().is_empty());
        if !has_alt && e.text.trim().is_empty() {
            c.add(
                Severity::Error,
                "link-alt",
                "lien sans texte visible ni texte de substitution (/Alt)",
                e.page,
                Some(e.label()),
            );
        }
    }
    for page in pages {
        let Some(annots) = doc
            .dict_get(&page.dict, "Annots")
            .ok()
            .flatten()
            .and_then(|a| a.as_array().map(<[Object]>::to_vec))
        else {
            continue;
        };
        for a in &annots {
            let number = match a {
                Object::Reference(r) => Some(r.number),
                _ => None,
            };
            let Ok(resolved) = doc.resolve(a) else {
                continue;
            };
            let Some(d) = resolved.as_dict() else {
                continue;
            };
            let subtype = d
                .get(&Name::new("Subtype"))
                .and_then(Object::as_name)
                .map(Name::as_str)
                .unwrap_or_default();
            if subtype != "Link" {
                continue;
            }
            let contents = match doc.dict_get(d, "Contents").ok().flatten().as_deref() {
                Some(Object::String(s)) => decode_text_string(s),
                _ => String::new(),
            };
            let tagged = number.is_some_and(|n| referenced.contains(&n));
            if tree.is_tagged() && !tagged {
                c.add(
                    Severity::Error,
                    "link-alt",
                    "annotation /Link absente de l'arbre de structure (aucun /OBJR ne la référence)",
                    Some(page.index),
                    None,
                );
            } else if contents.trim().is_empty() && !tagged {
                c.add(
                    Severity::Warning,
                    "link-alt",
                    "annotation /Link sans description /Contents",
                    Some(page.index),
                    None,
                );
            }
        }
    }
}

/// Titres : premier niveau H1, aucun saut de niveau (H1 → H3).
fn check_headings(tree: &StructTree, c: &mut Collector) {
    let mut previous: Option<u8> = None;
    let mut seen_any = false;
    for (e, _) in tree.elements() {
        let Some(level) = e.heading_level() else {
            continue;
        };
        if !seen_any {
            seen_any = true;
            if level != 1 {
                c.add(
                    Severity::Warning,
                    "heading-levels",
                    format!("le premier titre du document est un H{level} et non un H1"),
                    e.page,
                    Some(e.label()),
                );
            }
        }
        if let Some(prev) = previous {
            if level > prev + 1 {
                c.add(
                    Severity::Error,
                    "heading-levels",
                    format!("saut de niveau de titre : H{prev} suivi de H{level}"),
                    e.page,
                    Some(e.label()),
                );
            }
        }
        previous = Some(level);
    }
}

/// Tableaux : au moins un `TH`, et une portée exploitable (§14.8.4.3.4).
fn check_tables(tree: &StructTree, c: &mut Collector) {
    for (table, _) in tree.elements() {
        if table.kind != "Table" {
            continue;
        }
        let mut headers = 0usize;
        let mut cells = 0usize;
        let mut headers_without_scope = 0usize;
        let mut cells_with_headers = 0usize;
        table.walk(&mut |e, _| match e.kind.as_str() {
            "TH" => {
                headers += 1;
                if !e.attributes.contains_key("Scope") {
                    headers_without_scope += 1;
                }
            }
            "TD" => {
                cells += 1;
                if e.attributes.contains_key("Headers") {
                    cells_with_headers += 1;
                }
            }
            _ => {}
        });
        if headers == 0 {
            c.add(
                Severity::Error,
                "table-headers",
                "tableau sans cellule d'en-tête TH : les lecteurs d'écran ne peuvent pas \
                 annoncer la colonne ni la ligne d'une cellule",
                table.page,
                Some(table.label()),
            );
        } else if headers_without_scope > 0 && cells_with_headers < cells {
            c.add(
                Severity::Warning,
                "table-headers",
                format!(
                    "{headers_without_scope} en-tête(s) TH sans /Scope, et les cellules ne \
                     portent pas de /Headers : la portée des en-têtes est ambiguë"
                ),
                table.page,
                Some(table.label()),
            );
        }
    }
}

/// Listes : `L` ne contient que des `LI` (et une `Caption`), chaque `LI`
/// contient un `LBody` (§14.8.4.3.3).
fn check_lists(tree: &StructTree, c: &mut Collector) {
    for (list, _) in tree.elements() {
        if list.kind != "L" {
            continue;
        }
        for child in &list.children {
            if child.kind != "LI" && child.kind != "Caption" {
                c.add(
                    Severity::Error,
                    "list-structure",
                    format!(
                        "la liste L contient un élément {} au lieu d'un LI",
                        child.kind
                    ),
                    child.page,
                    Some(child.label()),
                );
                continue;
            }
            if child.kind == "LI" && !child.children.iter().any(|g| g.kind == "LBody") {
                c.add(
                    Severity::Warning,
                    "list-structure",
                    "élément de liste LI sans LBody",
                    child.page,
                    Some(child.label()),
                );
            }
        }
    }
}

/// Ordre de lecture : l'ordre de l'arbre doit suivre celui que la mise en page
/// impose à l'œil. L'ordre de référence est celui que `crate::text` reconstruit
/// par découpe XY — il comprend les colonnes, contrairement à un simple
/// « de haut en bas ».
fn check_reading_order(
    tree: &StructTree,
    marks: &[PageMarks],
    texts: &[PageText],
    c: &mut Collector,
) {
    if !tree.is_tagged() {
        return;
    }
    // MCID par page, dans l'ordre de l'arbre.
    let mut by_page: BTreeMap<usize, Vec<i64>> = BTreeMap::new();
    for (e, _) in tree.elements() {
        for (page, mcid) in &e.mcids {
            let list = by_page.entry(*page).or_default();
            if list.last() != Some(mcid) {
                list.push(*mcid);
            }
        }
    }
    for (page, order) in by_page {
        let Some(m) = marks.get(page) else { continue };
        let present: HashSet<i64> = order.iter().copied().collect();
        let Some(geometric) = layout_order(m, texts.get(page), &present) else {
            continue; // contenu partiellement absent du flux : rien à comparer
        };
        if geometric.len() != order.len() {
            continue;
        }
        if let Some(i) = order.iter().zip(&geometric).position(|(a, b)| a != b) {
            let text = m.text_of(geometric[i]);
            let extract: String = text.chars().take(40).collect();
            c.add(
                Severity::Warning,
                "reading-order",
                format!(
                    "l'ordre de l'arbre s'écarte de l'ordre géométrique : « {} » vient \
                     visuellement avant, mais l'arbre le place plus loin",
                    extract.trim()
                ),
                Some(page),
                None,
            );
        }
    }
}

/// Ordre de lecture attendu d'une page : les `/MCID` rangés dans l'ordre des
/// lignes reconstruites par la mise en page. `None` si un `/MCID` n'a pas pu
/// être situé, auquel cas la comparaison n'aurait pas de sens.
fn layout_order(
    marks: &PageMarks,
    text: Option<&PageText>,
    wanted: &HashSet<i64>,
) -> Option<Vec<i64>> {
    let text = text?;
    if text.lines.is_empty() {
        return None;
    }
    let mut keyed: Vec<(usize, f64, i64)> = Vec::with_capacity(wanted.len());
    for id in wanted {
        let bbox = marks.bbox_of(*id)?;
        let line = text
            .lines
            .iter()
            .position(|l| !l.bbox.intersect(&bbox).is_empty())?;
        keyed.push((line, bbox.x0, *id));
    }
    keyed.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    });
    Some(keyed.into_iter().map(|(_, _, id)| id).collect())
}

/// Contenu non balisé : du texte qui n'est ni dans un `/MCID` ni dans un
/// `/Artifact` (ISO 14289-1 §7.1, Matterhorn 01-003).
fn check_untagged(tree: &StructTree, marks: &[PageMarks], c: &mut Collector) {
    if !tree.is_tagged() {
        return;
    }
    for (page, m) in marks.iter().enumerate() {
        let orphans: Vec<&str> = m
            .runs
            .iter()
            .filter(|r| r.mcid.is_none() && !r.artifact)
            .map(|r| r.text.as_str())
            .collect();
        if orphans.is_empty() {
            continue;
        }
        let extract: String = orphans.join(" ").chars().take(40).collect();
        c.add(
            Severity::Error,
            "untagged-content",
            format!(
                "{} bloc(s) de texte hors de tout balisage et non marqués /Artifact : « {} »",
                orphans.len(),
                extract.trim()
            ),
            Some(page),
            None,
        );
        let images = m
            .images
            .iter()
            .filter(|i| i.mcid.is_none() && !i.artifact)
            .count();
        if images > 0 {
            c.add(
                Severity::Error,
                "untagged-content",
                format!("{images} image(s) hors de tout balisage et non marquées /Artifact"),
                Some(page),
                None,
            );
        }
    }
}

/// Luminance relative d'une couleur sRVB (WCAG 2.1, définition « relative luminance »).
fn relative_luminance(color: [f32; 3]) -> f64 {
    let channel = |v: f32| {
        let v = f64::from(v).clamp(0.0, 1.0);
        if v <= 0.039_28 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color[0]) + 0.7152 * channel(color[1]) + 0.0722 * channel(color[2])
}

/// Rapport de contraste WCAG entre deux couleurs sRVB : de 1:1 à 21:1.
#[must_use]
pub fn contrast_ratio(a: [f32; 3], b: [f32; 3]) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Seuil applicable : 3:1 pour le grand texte (≥ 18 pt, ou ≥ 14 pt en gras),
/// 4,5:1 sinon (WCAG 2.1 §1.4.3).
fn contrast_threshold(size: f64, bold: bool) -> f64 {
    if size >= 18.0 || (bold && size >= 14.0) {
        3.0
    } else {
        4.5
    }
}

/// Contraste du texte sur le fond dominant derrière lui.
fn check_contrast(marks: &[PageMarks], c: &mut Collector) {
    for (page, m) in marks.iter().enumerate() {
        // Un message par couple (couleur de texte, couleur de fond) et par page.
        let mut seen: HashSet<(u32, u32)> = HashSet::new();
        for run in &m.runs {
            if run.artifact || run.text.trim().is_empty() {
                continue;
            }
            let background = m.background_behind(&run.bbox, run.order);
            let ratio = contrast_ratio(run.color, background);
            let threshold = contrast_threshold(run.size, run.bold);
            if ratio >= threshold {
                continue;
            }
            if !seen.insert((pack(run.color), pack(background))) {
                continue;
            }
            let extract: String = run.text.chars().take(30).collect();
            c.add(
                Severity::Warning,
                "contrast",
                format!(
                    "contraste {ratio:.1}:1 pour {} sur {} (seuil {threshold:.1}:1) : « {} »",
                    hex(run.color),
                    hex(background),
                    extract.trim()
                ),
                Some(page),
                None,
            );
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn pack(c: [f32; 3]) -> u32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (q(c[0]) << 16) | (q(c[1]) << 8) | q(c[2])
}

fn hex(c: [f32; 3]) -> String {
    format!("#{:06X}", pack(c))
}

/// Texte en image : une page couverte par une grande image et sans texte
/// extractible est un scan non reconnu (ISO 14289-1 §7.3, Matterhorn 13-004).
fn check_image_only(
    doc: &Document,
    pages: &[acrux_document::Page],
    marks: &[PageMarks],
    c: &mut Collector,
) {
    for (page, m) in marks.iter().enumerate() {
        let Some(p) = pages.get(page) else { continue };
        let characters: usize = m.runs.iter().map(|r| r.text.trim().chars().count()).sum();
        if characters >= 20 {
            continue;
        }
        let box_ = p.crop_box(doc);
        let area = box_.width() * box_.height();
        if area <= 0.0 {
            continue;
        }
        let covered = m
            .images
            .iter()
            .map(|i| {
                let r: Rect = i.bbox.intersect(&box_);
                r.width() * r.height()
            })
            .fold(0.0_f64, f64::max);
        if covered / area >= 0.4 {
            c.add(
                Severity::Error,
                "image-only-text",
                format!(
                    "page sans texte extractible mais couverte à {:.0} % par une image : \
                     probable image de texte à reconnaître (OCR)",
                    covered / area * 100.0
                ),
                Some(page),
                None,
            );
        }
    }
}

/// Polices dont les glyphes n'ont pas d'équivalent Unicode : le texte affiché
/// ne peut être ni lu à voix haute, ni copié, ni recherché (§9.10.2).
fn check_fonts(marks: &[PageMarks], c: &mut Collector) {
    let mut totals: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for m in marks {
        for (font, (shown, unmapped)) in &m.glyphs_by_font {
            let e = totals.entry(font.clone()).or_insert((0, 0));
            e.0 += shown;
            e.1 += unmapped;
        }
    }
    for (font, (shown, unmapped)) in totals {
        if unmapped == 0 {
            continue;
        }
        let name = if font.is_empty() { "(sans nom)" } else { &font };
        c.add(
            Severity::Error,
            "font-tounicode",
            format!(
                "police {name} : {unmapped} caractère(s) sur {shown} sans équivalent Unicode \
                 (/ToUnicode absent ou encodage non standard)"
            ),
            None,
            None,
        );
    }
}

/// Champs de formulaire : `/TU` obligatoire (ISO 14289-1 §7.18.7), et chaque
/// widget doit être relié à un élément `Form` de la structure.
fn check_fields(doc: &Document, tree: &StructTree, c: &mut Collector) {
    let Ok(fields) = crate::forms::list_fields(doc) else {
        return;
    };
    let mut referenced: HashSet<u32> = HashSet::new();
    for (e, _) in tree.elements() {
        for r in &e.objects {
            referenced.insert(r.number);
        }
    }
    for field in &fields {
        let page = field.widgets.first().and_then(|w| w.page);
        if field
            .alternate_name
            .as_ref()
            .is_none_or(|t| t.trim().is_empty())
        {
            c.add(
                Severity::Error,
                "field-tu",
                format!(
                    "champ de formulaire « {} » sans description /TU (info-bulle)",
                    field.name
                ),
                page,
                None,
            );
        }
        if tree.is_tagged()
            && !field
                .widgets
                .iter()
                .any(|w| w.reference.is_some_and(|r| referenced.contains(&r.number)))
        {
            c.add(
                Severity::Error,
                "field-tu",
                format!(
                    "champ « {} » absent de l'arbre de structure (aucun élément Form ne le \
                     référence par /OBJR)",
                    field.name
                ),
                page,
                None,
            );
        }
    }
}

/// Passages dans une autre langue que celle du document : ils doivent porter
/// leur propre `/Lang` (ISO 14289-1 §7.2).
fn check_passage_lang(tree: &StructTree, c: &mut Collector) {
    let Some(document_lang) = tree.lang.as_ref().map(|l| short_lang(l)) else {
        return;
    };
    for (e, _) in tree.elements() {
        if e.lang.is_some() || !e.children.is_empty() {
            continue;
        }
        let text = e.text.trim();
        if text.chars().count() < 40 {
            continue;
        }
        let Some(detected) = super::detect_language(text) else {
            continue;
        };
        if detected != document_lang {
            c.add(
                Severity::Warning,
                "passage-lang",
                format!(
                    "passage détecté en « {detected} » alors que le document est en \
                     « {document_lang} » et que l'élément n'a pas de /Lang"
                ),
                e.page,
                Some(e.label()),
            );
        }
    }
}

/// Code de langue réduit à sa partie principale (`fr-FR` → `fr`).
fn short_lang(lang: &str) -> String {
    lang.split(['-', '_'])
        .next()
        .unwrap_or(lang)
        .to_ascii_lowercase()
}
