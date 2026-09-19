//! Accessibilité : arbre de structure logique (ISO 32000-2 §14.7), vérificateur
//! **PDF/UA-1** (ISO 14289-1) et **balisage automatique**.
//!
//! Trois services, dans l'ordre où on s'en sert :
//!
//! 1. [`read_structure`] lit le `/StructTreeRoot` et rend un arbre
//!    d'[`StructElement`] où chaque élément connaît son type (`/S`), son texte
//!    (retrouvé par les `/MCID` des marques `BDC /P <</MCID n>> … EMC` du flux
//!    de contenu), sa page, sa langue et ses attributs (`/Alt`, `/ActualText`,
//!    `/Scope`, `/Headers`…). C'est l'ordre de lecture voulu par l'auteur.
//! 2. [`check_accessibility`] applique les règles de PDF/UA et rend un rapport
//!    d'[`Issue`] classées par sévérité (voir `check::RULES` pour la liste).
//! 3. [`autotag`] construit un arbre de structure à partir de la mise en page
//!    détectée par [`crate::text`] (titres, paragraphes, listes, tableaux) et
//!    écrit les marques correspondantes dans les flux de contenu. C'est la
//!    fonction « Baliser automatiquement le document » d'Acrobat.
//!
//! Limites assumées, détaillées dans `spec-notes/pdf-14.7-structure-et-balisage.md` :
//! le balisage automatique déduit la structure de la géométrie, il ne devine ni
//! le texte de remplacement des images ni la portée réelle des en-têtes de
//! tableau ; le contraste est calculé contre les aplats du contenu, pas contre
//! les images ni les dégradés.

mod autotag;
mod check;
mod marked;

use std::collections::{BTreeMap, HashMap, HashSet};

use acrux_core::{Rect, Result};
use acrux_document::text::decode_text_string;
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef};

pub use autotag::{autotag, detect_language};
pub use check::{check_accessibility, contrast_ratio, AccessibilityReport, Issue, Severity, RULES};
pub use marked::{scan_page, ImageDraw, PageMarks, Span, SpanKind, TextRun};

/// Profondeur maximale de l'arbre de structure (fichiers hostiles ou cycliques).
const MAX_DEPTH: usize = 64;
/// Nombre maximal d'éléments lus.
const MAX_ELEMENTS: usize = 500_000;

/// Types de structure normalisés (§14.8.4, tableaux 344 à 349). Un `/S` absent
/// de cette liste est passé par la `/RoleMap` ; s'il n'y est pas non plus, il
/// reste tel quel et le vérificateur le signale.
pub const STANDARD_TYPES: [&str; 49] = [
    "Document",
    "Part",
    "Art",
    "Sect",
    "Div",
    "BlockQuote",
    "Caption",
    "TOC",
    "TOCI",
    "Index",
    "NonStruct",
    "Private",
    "P",
    "H",
    "H1",
    "H2",
    "H3",
    "H4",
    "H5",
    "H6",
    "L",
    "LI",
    "Lbl",
    "LBody",
    "Table",
    "TR",
    "TH",
    "TD",
    "THead",
    "TBody",
    "TFoot",
    "Span",
    "Quote",
    "Note",
    "Reference",
    "BibEntry",
    "Code",
    "Link",
    "Annot",
    "Ruby",
    "RB",
    "RT",
    "RP",
    "Warichu",
    "WT",
    "WP",
    "Figure",
    "Formula",
    "Form",
];

/// Un élément de l'arbre de structure (§14.7.2, tableau 355).
#[derive(Debug, Clone, Default)]
pub struct StructElement {
    /// Type de structure après application de la `/RoleMap` (`P`, `H2`, `TD`…).
    pub kind: String,
    /// Type déclaré tel quel dans `/S` (utile quand la `/RoleMap` l'a traduit).
    pub raw_kind: String,
    /// Référence de l'objet, si l'élément est indirect.
    pub reference: Option<ObjectRef>,
    /// Titre lisible `/T`.
    pub title: Option<String>,
    /// Langue `/Lang` propre à l'élément.
    pub lang: Option<String>,
    /// Texte de remplacement `/Alt` (obligatoire sur les figures en PDF/UA).
    pub alt: Option<String>,
    /// Texte réel `/ActualText` (remplace le contenu pour la lecture).
    pub actual_text: Option<String>,
    /// Développement d'une abréviation `/E`.
    pub expansion: Option<String>,
    /// Page de rattachement (`/Pg`, héritée de l'ancêtre à défaut).
    pub page: Option<usize>,
    /// Contenus marqués couverts : `(page, MCID)` dans l'ordre de l'arbre.
    pub mcids: Vec<(usize, i64)>,
    /// Objets référencés (`/OBJR` : annotations, widgets de formulaire).
    pub objects: Vec<ObjectRef>,
    /// Attributs `/A` aplatis (`Scope`, `Headers`, `ColSpan`, `ListNumbering`…).
    pub attributes: BTreeMap<String, String>,
    /// Texte couvert par les seuls `/MCID` de cet élément (hors enfants).
    pub own_text: String,
    /// Texte couvert, celui de l'élément puis celui de ses enfants.
    pub text: String,
    /// Boîte englobante du contenu couvert, en espace utilisateur.
    pub bbox: Option<Rect>,
    /// Enfants dans l'ordre de lecture.
    pub children: Vec<StructElement>,
}

impl StructElement {
    /// Niveau de titre (1 à 6) pour `H1`…`H6`, `None` sinon. `H` (titre sans
    /// niveau, PDF 2.0) rend `None` : son niveau vient de l'imbrication.
    #[must_use]
    pub fn heading_level(&self) -> Option<u8> {
        let rest = self.kind.strip_prefix('H')?;
        let level: u8 = rest.parse().ok()?;
        (1..=6).contains(&level).then_some(level)
    }

    /// Vrai si le type est l'un des types normalisés.
    #[must_use]
    pub fn is_standard(&self) -> bool {
        STANDARD_TYPES.contains(&self.kind.as_str())
    }

    /// Description courte pour les messages (`H2 « Introduction »`).
    #[must_use]
    pub fn label(&self) -> String {
        let extract: String = self.text.chars().take(40).collect();
        let extract = extract.trim();
        if extract.is_empty() {
            self.kind.clone()
        } else {
            format!("{} « {extract} »", self.kind)
        }
    }

    /// Parcours en profondeur, préfixe (l'élément puis ses descendants).
    pub fn walk<'a>(&'a self, visit: &mut impl FnMut(&'a StructElement, usize)) {
        self.walk_depth(visit, 0);
    }

    fn walk_depth<'a>(&'a self, visit: &mut impl FnMut(&'a StructElement, usize), depth: usize) {
        visit(self, depth);
        for c in &self.children {
            c.walk_depth(visit, depth + 1);
        }
    }
}

/// Arbre de structure d'un document.
#[derive(Debug, Clone, Default)]
pub struct StructTree {
    /// `/MarkInfo /Marked true` : le document se déclare balisé (§14.7.1).
    pub marked: bool,
    /// `/MarkInfo /Suspects true` : le balisage est déclaré douteux.
    pub suspects: bool,
    /// Langue par défaut du document (`/Lang` du catalogue).
    pub lang: Option<String>,
    /// `/RoleMap` : type propriétaire → type normalisé.
    pub role_map: BTreeMap<String, String>,
    /// Référence du `/StructTreeRoot`.
    pub root: Option<ObjectRef>,
    /// `/ParentTree` présent (indispensable pour relier contenu et structure).
    pub has_parent_tree: bool,
    /// Racines de l'arbre (le plus souvent un seul `Document`).
    pub children: Vec<StructElement>,
}

impl StructTree {
    /// Vrai si le document possède un arbre de structure exploitable.
    #[must_use]
    pub fn is_tagged(&self) -> bool {
        self.root.is_some() && !self.children.is_empty()
    }

    /// Tous les éléments, dans l'ordre de l'arbre, avec leur profondeur.
    #[must_use]
    pub fn elements(&self) -> Vec<(&StructElement, usize)> {
        let mut out = Vec::new();
        for c in &self.children {
            c.walk(&mut |e, d| out.push((e, d)));
        }
        out
    }

    /// Texte du document dans l'ordre de l'arbre : l'ordre de lecture voulu
    /// par l'auteur, à préférer à l'ordre géométrique quand il existe.
    #[must_use]
    pub fn reading_order_text(&self) -> String {
        let mut out = String::new();
        for c in &self.children {
            c.walk(&mut |e, _| {
                let own: String = e.actual_text.clone().unwrap_or_else(|| e.own_text.clone());
                if !own.trim().is_empty() {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(own.trim());
                }
            });
        }
        out
    }
}

/// Lit l'arbre de structure du document.
///
/// # Errors
/// Catalogue illisible. Un document sans balisage n'est pas une erreur :
/// l'arbre rendu est vide et [`StructTree::is_tagged`] vaut `false`.
pub fn read_structure(doc: &Document) -> Result<StructTree> {
    let catalog = doc.catalog()?;
    let mut tree = StructTree {
        lang: text_entry(doc, &catalog, "Lang"),
        ..StructTree::default()
    };
    if let Some(mark_info) = doc
        .dict_get(&catalog, "MarkInfo")
        .ok()
        .flatten()
        .and_then(|m| m.as_dict().cloned())
    {
        tree.marked = bool_entry(doc, &mark_info, "Marked");
        tree.suspects = bool_entry(doc, &mark_info, "Suspects");
    }
    let Some(root_obj) = catalog.get(&Name::new("StructTreeRoot")) else {
        return Ok(tree);
    };
    if let Object::Reference(r) = root_obj {
        tree.root = Some(*r);
    }
    let Ok(resolved) = doc.resolve(root_obj) else {
        return Ok(tree);
    };
    let Some(root) = resolved.as_dict().cloned() else {
        return Ok(tree);
    };
    if tree.root.is_none() {
        // `/StructTreeRoot` direct : rare mais légal, on garde l'arbre.
        tree.root = Some(ObjectRef {
            number: 0,
            generation: 0,
        });
    }
    tree.has_parent_tree = root.contains_key(&Name::new("ParentTree"));
    if let Some(map) = doc
        .dict_get(&root, "RoleMap")
        .ok()
        .flatten()
        .and_then(|m| m.as_dict().cloned())
    {
        for (k, v) in &map {
            if let Some(target) = v.as_name() {
                tree.role_map.insert(k.as_str(), target.as_str());
            }
        }
    }

    // Texte marqué de chaque page, indexé par MCID.
    let pages = collect_pages(doc)?;
    let page_numbers: HashMap<u32, usize> = pages
        .iter()
        .filter_map(|p| p.reference.map(|r| (r.number, p.index)))
        .collect();
    let mut marks: Vec<PageMarks> = Vec::with_capacity(pages.len());
    for page in &pages {
        marks.push(scan_page(doc, page).unwrap_or_default());
    }

    let mut reader = Reader {
        doc,
        role_map: &tree.role_map,
        page_numbers: &page_numbers,
        marks: &marks,
        visited: HashSet::new(),
        count: 0,
    };
    if let Some(kids) = root.get(&Name::new("K")) {
        tree.children = reader.read_kids(kids, None, 0);
    }
    Ok(tree)
}

struct Reader<'a> {
    doc: &'a Document,
    role_map: &'a BTreeMap<String, String>,
    page_numbers: &'a HashMap<u32, usize>,
    marks: &'a [PageMarks],
    visited: HashSet<u32>,
    count: usize,
}

impl Reader<'_> {
    /// Lit `/K` : entier (MCID), dictionnaire (élément, `/MCR`, `/OBJR`),
    /// tableau, ou référence vers l'un de ces cas (§14.7.2).
    fn read_kids(
        &mut self,
        kids: &Object,
        page: Option<usize>,
        depth: usize,
    ) -> Vec<StructElement> {
        let mut out = Vec::new();
        if depth > MAX_DEPTH || self.count > MAX_ELEMENTS {
            return out;
        }
        let Ok(resolved) = self.doc.resolve(kids) else {
            return out;
        };
        match &*resolved {
            Object::Array(items) => {
                for item in items {
                    out.extend(self.read_kids(item, page, depth + 1));
                }
            }
            Object::Dict(d) if d.contains_key(&Name::new("S")) => {
                let reference = match kids {
                    Object::Reference(r) => {
                        if !self.visited.insert(r.number) {
                            return out;
                        }
                        Some(*r)
                    }
                    _ => None,
                };
                if let Some(e) = self.read_element(d, reference, page, depth) {
                    out.push(e);
                }
            }
            _ => {}
        }
        out
    }

    /// Contenus directement couverts par `/K` : MCID entiers et `/MCR`, plus
    /// les `/OBJR`. Les éléments enfants sont lus par `read_kids`.
    fn collect_content(
        &mut self,
        kids: &Object,
        page: Option<usize>,
        mcids: &mut Vec<(usize, i64)>,
        objects: &mut Vec<ObjectRef>,
        depth: usize,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        let Ok(resolved) = self.doc.resolve(kids) else {
            return;
        };
        match &*resolved {
            Object::Integer(mcid) => {
                if let Some(p) = page {
                    mcids.push((p, *mcid));
                }
            }
            Object::Array(items) => {
                for item in items {
                    self.collect_content(item, page, mcids, objects, depth + 1);
                }
            }
            Object::Dict(d) if !d.contains_key(&Name::new("S")) => {
                let kind = d
                    .get(&Name::new("Type"))
                    .and_then(Object::as_name)
                    .map(Name::as_str)
                    .unwrap_or_default();
                let own_page = d
                    .get(&Name::new("Pg"))
                    .and_then(|o| match o {
                        Object::Reference(r) => self.page_numbers.get(&r.number).copied(),
                        _ => None,
                    })
                    .or(page);
                if kind == "OBJR" {
                    if let Some(Object::Reference(r)) = d.get(&Name::new("Obj")) {
                        objects.push(*r);
                    }
                } else if let Some(mcid) = d.get(&Name::new("MCID")).and_then(Object::as_i64) {
                    if let Some(p) = own_page {
                        mcids.push((p, mcid));
                    }
                }
            }
            _ => {}
        }
    }

    fn read_element(
        &mut self,
        d: &Dict,
        reference: Option<ObjectRef>,
        inherited_page: Option<usize>,
        depth: usize,
    ) -> Option<StructElement> {
        self.count += 1;
        if self.count > MAX_ELEMENTS {
            return None;
        }
        let raw_kind = d.get(&Name::new("S")).and_then(Object::as_name)?.as_str();
        let kind = self
            .role_map
            .get(&raw_kind)
            .cloned()
            .unwrap_or_else(|| raw_kind.clone());
        let page = d
            .get(&Name::new("Pg"))
            .and_then(|o| match o {
                Object::Reference(r) => self.page_numbers.get(&r.number).copied(),
                _ => None,
            })
            .or(inherited_page);
        let mut mcids = Vec::new();
        let mut objects = Vec::new();
        let mut children = Vec::new();
        if let Some(kids) = d.get(&Name::new("K")) {
            self.collect_content(kids, page, &mut mcids, &mut objects, 0);
            children = self.read_kids(kids, page, depth + 1);
        }
        let mut own_text = String::new();
        let mut bbox: Option<Rect> = None;
        for (p, mcid) in &mcids {
            if let Some(m) = self.marks.get(*p) {
                own_text.push_str(&m.text_of(*mcid));
                if let Some(b) = m.bbox_of(*mcid) {
                    bbox = Some(bbox.map_or(b, |a| a.union(&b)));
                }
            }
        }
        let mut text = own_text.clone();
        for c in &children {
            if !text.is_empty() && !text.ends_with(' ') && !c.text.is_empty() {
                text.push(' ');
            }
            text.push_str(&c.text);
            if let Some(b) = c.bbox {
                bbox = Some(bbox.map_or(b, |a| a.union(&b)));
            }
        }
        Some(StructElement {
            kind,
            raw_kind,
            reference,
            title: text_entry(self.doc, d, "T"),
            lang: text_entry(self.doc, d, "Lang"),
            alt: text_entry(self.doc, d, "Alt"),
            actual_text: text_entry(self.doc, d, "ActualText"),
            expansion: text_entry(self.doc, d, "E"),
            page,
            mcids,
            objects,
            attributes: read_attributes(self.doc, d),
            own_text,
            text,
            bbox,
            children,
        })
    }
}

/// Attributs `/A` (§14.7.6) : dictionnaire, tableau de dictionnaires, ou
/// tableau alternant dictionnaires et numéros de révision. Les clés sont
/// aplaties en texte ; `/O` (propriétaire) est conservé sous la clé `O`.
fn read_attributes(doc: &Document, d: &Dict) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(attr) = doc.dict_get(d, "A").ok().flatten() else {
        return out;
    };
    let mut add = |dict: &Dict| {
        for (k, v) in dict {
            let value = match doc.resolve(v) {
                Ok(r) => acrux_document::writer::to_string(&r),
                Err(_) => acrux_document::writer::to_string(v),
            };
            out.insert(k.as_str(), value.trim().to_string());
        }
    };
    match &*attr {
        Object::Dict(dict) => add(dict),
        Object::Array(items) => {
            for item in items {
                if let Ok(r) = doc.resolve(item) {
                    if let Some(dict) = r.as_dict() {
                        add(dict);
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// Chaîne de texte ou nom d'une entrée de dictionnaire.
fn text_entry(doc: &Document, d: &Dict, key: &str) -> Option<String> {
    let o = doc.dict_get(d, key).ok().flatten()?;
    match &*o {
        Object::String(s) => Some(decode_text_string(s)),
        Object::Name(n) => Some(n.as_str()),
        _ => None,
    }
}

/// Dictionnaire `/Info` du trailer, **modifications en attente comprises**.
/// `Document::info` ne lit que le trailer d'origine ; ici on veut voir le
/// titre qu'un correctif vient d'écrire.
pub(crate) fn info_dict(doc: &Document) -> Option<Dict> {
    match doc.trailer().get(&Name::new("Info")) {
        Some(Object::Reference(r)) => doc.get(*r).ok()?.as_dict().cloned(),
        Some(Object::Dict(d)) => Some(d.clone()),
        _ => None,
    }
}

/// Booléen d'une entrée de dictionnaire (absent = faux).
fn bool_entry(doc: &Document, d: &Dict, key: &str) -> bool {
    matches!(
        doc.dict_get(d, key).ok().flatten().as_deref(),
        Some(Object::Bool(true))
    )
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines
)]
pub(crate) mod tests {
    use super::*;
    use acrux_document::SaveOptions;

    /// Assemble un PDF à partir des corps d'objets (1 0 obj, 2 0 obj…), avec
    /// une table xref correcte. Même style que `acrux_document::protect`.
    pub fn build_pdf(objects: &[String], trailer_extra: &str) -> Vec<u8> {
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
                "trailer\n<< /Size {} /Root 1 0 R {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    pub fn stream_object(dict: &str, data: &str) -> String {
        format!(
            "<< {dict} /Length {} >>\nstream\n{data}\nendstream",
            data.len()
        )
    }

    /// Contenu d'une page balisée : un titre et deux paragraphes, chacun dans
    /// sa marque `BDC /P <</MCID n>> … EMC`.
    fn tagged_content() -> String {
        concat!(
            "/P <</MCID 0>> BDC BT /F2 18 Tf 40 250 Td (Rapport annuel) Tj ET EMC\n",
            "/P <</MCID 1>> BDC BT /F1 11 Tf 40 220 Td (Le premier paragraphe du rapport.) Tj ET EMC\n",
            "/P <</MCID 2>> BDC BT /F1 11 Tf 40 200 Td (Le second paragraphe du rapport.) Tj ET EMC\n",
            "/Artifact BMC 0.5 G 0.5 w 40 40 m 260 40 l S EMC\n"
        )
        .to_string()
    }

    /// Document correctement balisé : titre, langue, /MarkInfo, /ParentTree.
    pub fn tagged_pdf() -> Vec<u8> {
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R /Lang (fr-FR) /MarkInfo << /Marked true >> \
             /StructTreeRoot 6 0 R /ViewerPreferences << /DisplayDocTitle true >> >>"
                .to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R \
             /StructParents 0 /Resources << /Font << /F1 5 0 R /F2 10 0 R >> >> >>"
                .to_string(),
            stream_object("", &tagged_content()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_string(),
            // 6 : racine de l'arbre de structure.
            "<< /Type /StructTreeRoot /K [7 0 R] /ParentTree << /Nums [0 [8 0 R 9 0 R 11 0 R]] >> \
             /ParentTreeNextKey 1 >>"
                .to_string(),
            // 7 : Document
            "<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R 9 0 R 11 0 R] >>".to_string(),
            "<< /Type /StructElem /S /H1 /P 7 0 R /Pg 3 0 R /K [0] >>".to_string(),
            "<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K [1] >>".to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
                .to_string(),
            "<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K [2] >>".to_string(),
        ];
        build_pdf(&objects, "")
    }

    /// Le même document, mais sans la moindre balise.
    pub fn untagged_pdf() -> Vec<u8> {
        let content = concat!(
            "BT /F2 18 Tf 40 250 Td (Rapport annuel) Tj ET\n",
            "BT /F1 11 Tf 40 220 Td (Le premier paragraphe du rapport.) Tj ET\n",
            "BT /F1 11 Tf 40 200 Td (Le second paragraphe est la suite du texte.) Tj ET\n",
            "0.5 G 0.5 w 40 40 m 260 40 l S\n"
        );
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R /ViewerPreferences << /DisplayDocTitle true >> >>"
                .to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> >>"
                .to_string(),
            stream_object("", content),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
                .to_string(),
        ];
        build_pdf(&objects, "")
    }

    fn open(data: Vec<u8>) -> Document {
        Document::from_bytes(data).unwrap()
    }

    #[test]
    fn structure_tree_is_read_with_text_and_pages() {
        let doc = open(with_info(tagged_pdf(), "(Rapport annuel)"));
        let tree = read_structure(&doc).unwrap();
        assert!(tree.is_tagged());
        assert!(tree.marked);
        assert_eq!(tree.lang.as_deref(), Some("fr-FR"));
        let elements = tree.elements();
        let kinds: Vec<&str> = elements.iter().map(|(e, _)| e.kind.as_str()).collect();
        assert_eq!(kinds, vec!["Document", "H1", "P", "P"], "{kinds:?}");
        let heading = elements[1].0;
        assert_eq!(heading.heading_level(), Some(1));
        assert_eq!(heading.text.trim(), "Rapport annuel");
        assert_eq!(heading.page, Some(0));
        assert!(heading.bbox.is_some_and(|b| b.y0 > 240.0));
        assert!(elements[2].0.text.contains("premier paragraphe"));
        assert!(tree
            .reading_order_text()
            .starts_with("Rapport annuel\nLe premier"));
    }

    /// Ajoute un dictionnaire `/Info` au PDF de test.
    fn with_info(data: Vec<u8>, title: &str) -> Vec<u8> {
        let doc = Document::from_bytes(data).unwrap();
        let info = doc.add(Object::Dict(Dict::new()));
        let mut d = Dict::new();
        d.insert(
            Name::new("Title"),
            Object::String(title.trim_matches(['(', ')']).as_bytes().to_vec()),
        );
        doc.set(info, Object::Dict(d));
        doc.set_trailer_entry("Info", Object::Reference(info));
        doc.save_full_with(&SaveOptions {
            compress_streams: false,
            ..SaveOptions::default()
        })
        .unwrap()
    }

    #[test]
    fn tagged_document_passes_the_checker() {
        let doc = open(with_info(tagged_pdf(), "(Rapport annuel)"));
        let report = check_accessibility(&doc).unwrap();
        assert!(
            report.is_conforming(),
            "{:#?}",
            report
                .issues
                .iter()
                .filter(|i| i.severity == Severity::Error)
                .collect::<Vec<_>>()
        );
        assert!(report.tagged);
        assert_eq!(report.pages, 1);
    }

    #[test]
    fn untagged_document_reports_the_expected_rules() {
        let doc = open(untagged_pdf());
        let report = check_accessibility(&doc).unwrap();
        assert!(!report.tagged);
        assert!(report.has("document-tagged"));
        assert!(report.has("document-lang"));
        assert!(report.has("document-title"));
        assert!(!report.is_conforming());
        // Le contenu non balisé n'est pas signalé deux fois : la règle
        // « document non balisé » suffit.
        assert!(!report.has("untagged-content"));
        let json = report.to_json();
        assert!(json.contains("\"rule\": \"document-tagged\""), "{json}");
        assert!(json.contains("\"tagged\": false"));
    }

    #[test]
    fn contrast_ratio_matches_wcag_reference_values() {
        let white = [1.0, 1.0, 1.0];
        let black = [0.0, 0.0, 0.0];
        assert!((contrast_ratio(black, white) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(white, white) - 1.0).abs() < 0.001);
        // #767676 sur blanc : exactement 4,54:1 dans la table WCAG.
        let gray = [0.462_745, 0.462_745, 0.462_745];
        let ratio = contrast_ratio(gray, white);
        assert!((4.4..4.7).contains(&ratio), "{ratio}");
    }

    #[test]
    fn low_contrast_text_is_reported() {
        let content =
            "/P <</MCID 0>> BDC BT /F1 11 Tf 0.75 g 40 220 Td (Texte gris clair) Tj ET EMC\n";
        let doc = open(with_info(one_paragraph_pdf(content), "(Essai)"));
        let report = check_accessibility(&doc).unwrap();
        assert!(report.has("contrast"), "{:#?}", report.issues);
        let issue = report.issues.iter().find(|i| i.rule == "contrast").unwrap();
        assert_eq!(issue.severity, Severity::Warning);
        assert!(issue.message.contains("#BFBFBF"), "{}", issue.message);
    }

    #[test]
    fn dark_text_on_dark_background_is_reported() {
        // Un aplat sombre dessiné avant le texte : c'est lui le fond.
        let content = concat!(
            "0.1 0.1 0.1 rg 20 200 260 40 re f\n",
            "/P <</MCID 0>> BDC BT /F1 11 Tf 0.2 g 40 220 Td (Texte sombre sur fond sombre) Tj ET EMC\n",
        );
        let doc = open(with_info(one_paragraph_pdf(content), "(Essai)"));
        let report = check_accessibility(&doc).unwrap();
        let issue = report
            .issues
            .iter()
            .find(|i| i.rule == "contrast")
            .expect("contraste attendu");
        assert!(issue.message.contains("sur #1A1A1A"), "{}", issue.message);
    }

    /// Page balisée d'un seul paragraphe, contenu fourni par l'appelant.
    fn one_paragraph_pdf(content: &str) -> Vec<u8> {
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R /Lang (fr) /MarkInfo << /Marked true >> \
             /StructTreeRoot 6 0 R /ViewerPreferences << /DisplayDocTitle true >> >>"
                .to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R \
             /StructParents 0 /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            stream_object("", content),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_string(),
            "<< /Type /StructTreeRoot /K [7 0 R] /ParentTree << /Nums [0 [8 0 R]] >> >>"
                .to_string(),
            "<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R] >>".to_string(),
            "<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K [0] >>".to_string(),
        ];
        build_pdf(&objects, "")
    }

    #[test]
    fn figure_without_alt_is_an_error() {
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R /Lang (fr) /MarkInfo << /Marked true >> \
             /StructTreeRoot 6 0 R /ViewerPreferences << /DisplayDocTitle true >> >>"
                .to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R \
             /StructParents 0 /Resources << /Font << /F1 5 0 R >> /XObject << /Im0 9 0 R >> >> >>"
                .to_string(),
            stream_object(
                "",
                "/Figure <</MCID 0>> BDC q 40 0 0 40 40 200 cm /Im0 Do Q EMC\n",
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_string(),
            "<< /Type /StructTreeRoot /K [7 0 R] /ParentTree << /Nums [0 [8 0 R]] >> >>"
                .to_string(),
            "<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R] >>".to_string(),
            "<< /Type /StructElem /S /Figure /P 7 0 R /Pg 3 0 R /K [0] >>".to_string(),
            stream_object(
                "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray \
                 /BitsPerComponent 8 /Filter /ASCIIHexDecode",
                "00ff00ff>",
            ),
        ];
        let doc = open(with_info(build_pdf(&objects, ""), "(Essai)"));
        let report = check_accessibility(&doc).unwrap();
        let issue = report
            .issues
            .iter()
            .find(|i| i.rule == "figure-alt")
            .expect("figure sans /Alt attendue");
        assert_eq!(issue.severity, Severity::Error);
        assert_eq!(issue.page, Some(0));
    }

    #[test]
    fn heading_level_skip_is_an_error() {
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R /Lang (fr) /MarkInfo << /Marked true >> \
             /StructTreeRoot 6 0 R /ViewerPreferences << /DisplayDocTitle true >> >>"
                .to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R \
             /StructParents 0 /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            stream_object(
                "",
                "/P <</MCID 0>> BDC BT /F1 18 Tf 40 250 Td (Titre) Tj ET EMC\n\
                 /P <</MCID 1>> BDC BT /F1 13 Tf 40 210 Td (Sous-sous-titre) Tj ET EMC\n",
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_string(),
            "<< /Type /StructTreeRoot /K [7 0 R] /ParentTree << /Nums [0 [8 0 R 9 0 R]] >> >>"
                .to_string(),
            "<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R 9 0 R] >>".to_string(),
            "<< /Type /StructElem /S /H1 /P 7 0 R /Pg 3 0 R /K [0] >>".to_string(),
            "<< /Type /StructElem /S /H3 /P 7 0 R /Pg 3 0 R /K [1] >>".to_string(),
        ];
        let doc = open(with_info(build_pdf(&objects, ""), "(Essai)"));
        let report = check_accessibility(&doc).unwrap();
        let issue = report
            .issues
            .iter()
            .find(|i| i.rule == "heading-levels")
            .expect("saut de niveau attendu");
        assert_eq!(issue.severity, Severity::Error);
        assert!(
            issue.message.contains("H1 suivi de H3"),
            "{}",
            issue.message
        );
    }

    #[test]
    fn table_without_header_cells_is_an_error() {
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R /Lang (fr) /MarkInfo << /Marked true >> \
             /StructTreeRoot 6 0 R /ViewerPreferences << /DisplayDocTitle true >> >>"
                .to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R \
             /StructParents 0 /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            stream_object(
                "",
                "/P <</MCID 0>> BDC BT /F1 11 Tf 40 250 Td (Paris) Tj ET EMC\n",
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_string(),
            "<< /Type /StructTreeRoot /K [7 0 R] /ParentTree << /Nums [0 [10 0 R]] >> >>"
                .to_string(),
            "<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R] >>".to_string(),
            "<< /Type /StructElem /S /Table /P 7 0 R /Pg 3 0 R /K [9 0 R] >>".to_string(),
            "<< /Type /StructElem /S /TR /P 8 0 R /Pg 3 0 R /K [10 0 R] >>".to_string(),
            "<< /Type /StructElem /S /TD /P 9 0 R /Pg 3 0 R /K [0] >>".to_string(),
        ];
        let doc = open(with_info(build_pdf(&objects, ""), "(Essai)"));
        let report = check_accessibility(&doc).unwrap();
        let issue = report
            .issues
            .iter()
            .find(|i| i.rule == "table-headers")
            .expect("tableau sans TH attendu");
        assert_eq!(issue.severity, Severity::Error);
    }

    #[test]
    fn list_must_contain_list_items() {
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R /Lang (fr) /MarkInfo << /Marked true >> \
             /StructTreeRoot 6 0 R /ViewerPreferences << /DisplayDocTitle true >> >>"
                .to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R \
             /StructParents 0 /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            stream_object(
                "",
                "/P <</MCID 0>> BDC BT /F1 11 Tf 40 250 Td (Premier point) Tj ET EMC\n",
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_string(),
            "<< /Type /StructTreeRoot /K [7 0 R] /ParentTree << /Nums [0 [9 0 R]] >> >>"
                .to_string(),
            "<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R] >>".to_string(),
            "<< /Type /StructElem /S /L /P 7 0 R /Pg 3 0 R /K [9 0 R] >>".to_string(),
            "<< /Type /StructElem /S /P /P 8 0 R /Pg 3 0 R /K [0] >>".to_string(),
        ];
        let doc = open(with_info(build_pdf(&objects, ""), "(Essai)"));
        let report = check_accessibility(&doc).unwrap();
        let issue = report
            .issues
            .iter()
            .find(|i| i.rule == "list-structure")
            .expect("liste mal formée attendue");
        assert!(
            issue.message.contains("au lieu d'un LI"),
            "{}",
            issue.message
        );
    }

    #[test]
    fn font_without_unicode_mapping_is_an_error() {
        // Type0 Identity-H sans /ToUnicode : aucun glyphe n'a d'équivalent.
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R /Lang (fr) /MarkInfo << /Marked true >> \
             /StructTreeRoot 6 0 R /ViewerPreferences << /DisplayDocTitle true >> >>"
                .to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R \
             /StructParents 0 /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            stream_object(
                "",
                "/P <</MCID 0>> BDC BT /F1 11 Tf 40 250 Td <00240025> Tj ET EMC\n",
            ),
            "<< /Type /Font /Subtype /Type0 /BaseFont /Inconnue /Encoding /Identity-H \
             /DescendantFonts [9 0 R] >>"
                .to_string(),
            "<< /Type /StructTreeRoot /K [7 0 R] /ParentTree << /Nums [0 [8 0 R]] >> >>"
                .to_string(),
            "<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R] >>".to_string(),
            "<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K [0] >>".to_string(),
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Inconnue \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /DW 500 >>"
                .to_string(),
        ];
        let doc = open(with_info(build_pdf(&objects, ""), "(Essai)"));
        let report = check_accessibility(&doc).unwrap();
        let issue = report
            .issues
            .iter()
            .find(|i| i.rule == "font-tounicode")
            .expect("police sans /ToUnicode attendue");
        assert_eq!(issue.severity, Severity::Error);
        assert!(issue.message.contains("Inconnue"), "{}", issue.message);
    }

    #[test]
    fn detect_language_recognises_french_and_english() {
        assert_eq!(
            detect_language("Le rapport présente les résultats de la société pour cette année."),
            Some("fr")
        );
        assert_eq!(
            detect_language("The report presents the results of the company for this year."),
            Some("en")
        );
        assert_eq!(detect_language("Xyz 42 ***"), None);
    }

    #[test]
    fn autotag_produces_a_tree_the_checker_accepts() {
        let doc = open(with_info(untagged_pdf(), "(Rapport annuel)"));
        assert!(!check_accessibility(&doc).unwrap().tagged);
        let created = autotag(&doc).unwrap();
        assert!(created >= 4, "{created} éléments créés");

        // Relecture depuis les octets écrits : le balisage doit survivre à
        // l'enregistrement.
        let saved = doc
            .save_full_with(&SaveOptions {
                compress_streams: false,
                ..SaveOptions::default()
            })
            .unwrap();
        let doc = open(saved);
        let tree = read_structure(&doc).unwrap();
        assert!(tree.is_tagged());
        assert!(tree.marked);
        assert_eq!(tree.lang.as_deref(), Some("fr"));
        let kinds: Vec<String> = tree
            .elements()
            .iter()
            .map(|(e, _)| e.kind.clone())
            .collect();
        assert!(kinds.contains(&"Document".to_string()), "{kinds:?}");
        assert!(
            kinds.iter().any(|k| k.starts_with('H')),
            "un titre est attendu : {kinds:?}"
        );
        assert!(kinds.contains(&"P".to_string()), "{kinds:?}");
        let text = tree.reading_order_text();
        assert!(text.contains("Rapport annuel"), "{text:?}");
        assert!(text.contains("premier paragraphe"), "{text:?}");

        let report = check_accessibility(&doc).unwrap();
        assert!(
            report.is_conforming(),
            "aller-retour : {:#?}",
            report
                .issues
                .iter()
                .filter(|i| i.severity == Severity::Error)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn autotag_marks_decorations_as_artifacts() {
        let doc = open(with_info(untagged_pdf(), "(Rapport annuel)"));
        autotag(&doc).unwrap();
        let pages = acrux_document::collect_pages(&doc).unwrap();
        let marks = scan_page(&doc, &pages[0]).unwrap();
        assert!(
            marks.runs.iter().all(|r| r.mcid.is_some() || r.artifact),
            "aucun texte ne doit rester hors balisage"
        );
        let content = acrux_render::page::page_content(&doc, &pages[0]);
        let text = String::from_utf8_lossy(&content);
        assert!(text.contains("/Artifact BMC"), "{text}");
        assert!(text.contains("/P <</MCID 0>> BDC"), "{text}");
        // Le texte d'origine est recopié tel quel.
        assert!(text.contains("(Rapport annuel) Tj"), "{text}");
    }

    #[test]
    fn autotag_is_idempotent() {
        let doc = open(with_info(untagged_pdf(), "(Rapport annuel)"));
        let first = autotag(&doc).unwrap();
        let second = autotag(&doc).unwrap();
        assert_eq!(
            first, second,
            "re-baliser un document déjà balisé doit donner le même arbre"
        );
        let report = check_accessibility(&doc).unwrap();
        assert!(report.is_conforming(), "{:#?}", report.issues);
    }

    #[test]
    fn rules_list_has_no_duplicate_identifier() {
        let mut ids: Vec<&str> = RULES.iter().map(|(id, _)| *id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "identifiants de règle en double");
    }

    /// Page 420 × 300 truffée de défauts d'accessibilité volontaires,
    /// documentée dans `tests/corpus/synthese/README.md`.
    fn accessibility_corpus_pdf() -> Vec<u8> {
        let content = concat!(
            "0.97 0.97 0.97 rg 0 0 420 300 re f\n",
            "/P <</MCID 0>> BDC BT /F2 16 Tf 0 g 24 262 Td (Rapport sans balisage complet) Tj ET EMC\n",
            "/P <</MCID 1>> BDC BT /F2 11 Tf 24 236 Td (Sous-titre qui saute un niveau) Tj ET EMC\n",
            "/P <</MCID 2>> BDC BT /F1 10 Tf 24 214 Td (Ce paragraphe est correctement balise et lisible.) Tj ET EMC\n",
            "/P <</MCID 3>> BDC BT /F1 10 Tf 0.78 g 24 196 Td (Ce texte gris clair n'a pas assez de contraste.) Tj ET EMC\n",
            "BT /F1 10 Tf 0 g 24 178 Td (Cette ligne n'est dans aucune balise.) Tj ET\n",
            "/Figure <</MCID 4>> BDC q 84 0 0 56 24 96 cm /Im0 Do Q EMC\n",
            "/Artifact BMC 0.35 G 0.6 w 140 96 m 396 96 l S 140 124 m 396 124 l S ",
            "140 152 m 396 152 l S 140 96 m 140 152 l S 268 96 m 268 152 l S 396 96 m 396 152 l S EMC\n",
            "/P <</MCID 5>> BDC BT /F2 10 Tf 0 g 148 132 Td (Ville) Tj ET EMC\n",
            "/P <</MCID 6>> BDC BT /F2 10 Tf 276 132 Td (Habitants) Tj ET EMC\n",
            "/P <</MCID 7>> BDC BT /F1 10 Tf 148 104 Td (Lyon) Tj ET EMC\n",
            "/P <</MCID 8>> BDC BT /F1 10 Tf 276 104 Td (522 250) Tj ET EMC\n",
            "/Artifact BMC BT /F1 9 Tf 0.3 g 24 62 Td (Nom :) Tj ET EMC\n",
            "/Artifact BMC BT /F1 8 Tf 0.35 g 24 26 Td ",
            "(Fichier de synthese : les defauts d'accessibilite sont volontaires.) Tj ET EMC\n",
        );
        let objects = vec![
            // 1 : pas de /Lang, pas de /ViewerPreferences → deux erreurs.
            "<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 8 0 R \
             /AcroForm << /Fields [23 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 5 0 R >> >> \
             /NeedAppearances false >> >>"
                .to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 420 300] /Contents 4 0 R \
             /StructParents 0 /Annots [22 0 R 23 0 R] \
             /Resources << /Font << /F1 5 0 R /F2 6 0 R >> /XObject << /Im0 7 0 R >> >> >>"
                .to_string(),
            stream_object("", content),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
                .to_string(),
            stream_object(
                "/Type /XObject /Subtype /Image /Width 4 /Height 3 /ColorSpace /DeviceRGB \
                 /BitsPerComponent 8 /Filter /ASCIIHexDecode",
                "3060a04080c060a0e080c0e0 4080c060a0e080c0e03060a0 80c0e03060a04080c060a0e0>",
            ),
            // 8 : racine de structure et arbre des parents.
            "<< /Type /StructTreeRoot /K [9 0 R] /ParentTree << /Nums [0 [10 0 R 11 0 R 12 0 R \
             13 0 R 14 0 R 18 0 R 19 0 R 20 0 R 21 0 R]] >> /ParentTreeNextKey 1 >>"
                .to_string(),
            "<< /Type /StructElem /S /Document /P 8 0 R \
             /K [10 0 R 11 0 R 12 0 R 13 0 R 14 0 R 15 0 R] >>"
                .to_string(),
            "<< /Type /StructElem /S /H1 /P 9 0 R /Pg 3 0 R /K [0] >>".to_string(),
            // 11 : H3 juste après un H1 → saut de niveau.
            "<< /Type /StructElem /S /H3 /P 9 0 R /Pg 3 0 R /K [1] >>".to_string(),
            "<< /Type /StructElem /S /P /P 9 0 R /Pg 3 0 R /K [2] >>".to_string(),
            "<< /Type /StructElem /S /P /P 9 0 R /Pg 3 0 R /K [3] >>".to_string(),
            // 14 : figure sans /Alt.
            "<< /Type /StructElem /S /Figure /P 9 0 R /Pg 3 0 R /K [4] >>".to_string(),
            // 15 : tableau sans la moindre cellule d'en-tête TH.
            "<< /Type /StructElem /S /Table /P 9 0 R /Pg 3 0 R /K [16 0 R 17 0 R] >>".to_string(),
            "<< /Type /StructElem /S /TR /P 15 0 R /Pg 3 0 R /K [18 0 R 19 0 R] >>".to_string(),
            "<< /Type /StructElem /S /TR /P 15 0 R /Pg 3 0 R /K [20 0 R 21 0 R] >>".to_string(),
            "<< /Type /StructElem /S /TD /P 16 0 R /Pg 3 0 R /K [5] >>".to_string(),
            "<< /Type /StructElem /S /TD /P 16 0 R /Pg 3 0 R /K [6] >>".to_string(),
            "<< /Type /StructElem /S /TD /P 17 0 R /Pg 3 0 R /K [7] >>".to_string(),
            "<< /Type /StructElem /S /TD /P 17 0 R /Pg 3 0 R /K [8] >>".to_string(),
            // 22 : lien sans /Contents et absent de l'arbre de structure.
            "<< /Type /Annot /Subtype /Link /Rect [24 208 268 226] /Border [0 0 1] /C [0 0 0.8] \
             /A << /S /URI /URI (https://example.org/rapport) >> >>"
                .to_string(),
            // 23 : champ de formulaire sans /TU.
            "<< /Type /Annot /Subtype /Widget /F 4 /P 3 0 R /FT /Tx /T (nom) /V (Dupont) \
             /Rect [60 54 200 74] /MK << /BC [0.2] /BG [1] >> /AP << /N 24 0 R >> >>"
                .to_string(),
            stream_object(
                "/Type /XObject /Subtype /Form /BBox [0 0 140 20] \
                 /Resources << /Font << /Helv 5 0 R >> >>",
                "/Tx BMC q 1 g 0 0 140 20 re f 0.2 G 1 w 0.5 0.5 139 19 re S \
                 BT /Helv 10 Tf 0 g 4 6 Td (Dupont) Tj ET Q EMC",
            ),
        ];
        build_pdf(&objects, "")
    }

    #[test]
    fn autotag_attaches_links_and_form_fields_by_objr() {
        let doc = open(accessibility_corpus_pdf());
        assert!(check_accessibility(&doc).unwrap().has("link-alt"));
        autotag(&doc).unwrap();
        let tree = read_structure(&doc).unwrap();
        let elements = tree.elements();
        let link = elements
            .iter()
            .find(|(e, _)| e.kind == "Link")
            .expect("élément Link attendu")
            .0;
        assert_eq!(link.objects.len(), 1, "l'annotation doit être en /OBJR");
        assert!(
            link.alt
                .as_deref()
                .is_some_and(|a| a.contains("paragraphe")),
            "le texte couvert devient le /Alt : {:?}",
            link.alt
        );
        let form = elements
            .iter()
            .find(|(e, _)| e.kind == "Form")
            .expect("élément Form attendu")
            .0;
        assert_eq!(form.objects.len(), 1);
        let report = check_accessibility(&doc).unwrap();
        assert!(!report.has("link-alt"), "{:#?}", report.issues);
        assert!(!report.has("structure-types"), "{:#?}", report.issues);
        // Ce que le balisage automatique ne peut pas inventer reste signalé.
        assert!(report.has("figure-alt"));
        assert!(report.has("field-tu"));
    }

    /// Écrit `tests/corpus/synthese/accessibilite-problemes.pdf`.
    /// `cargo test -p acrux-features --lib -- --ignored generate_accessibility_corpus`.
    #[test]
    #[ignore = "génère le fichier de corpus"]
    fn generate_accessibility_corpus() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/corpus/synthese/accessibilite-problemes.pdf");
        std::fs::write(&path, accessibility_corpus_pdf()).unwrap();
        println!("écrit : {}", path.display());
    }

    /// Le fichier de corpus doit continuer à exercer les règles annoncées.
    #[test]
    fn accessibility_corpus_triggers_its_documented_rules() {
        let doc = open(accessibility_corpus_pdf());
        let report = check_accessibility(&doc).unwrap();
        for rule in [
            "document-lang",
            "document-title",
            "display-doc-title",
            "figure-alt",
            "heading-levels",
            "table-headers",
            "contrast",
            "untagged-content",
            "link-alt",
            "field-tu",
        ] {
            assert!(
                report.has(rule),
                "règle {rule} attendue : {:#?}",
                report.issues
            );
        }
        assert!(report.tagged, "le document est balisé, mais mal");
    }
}
