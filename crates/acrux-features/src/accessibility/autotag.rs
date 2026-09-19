//! Balisage automatique : construit un `/StructTreeRoot` (ISO 32000-2 §14.7)
//! à partir de la mise en page reconstruite par [`crate::text`], et écrit dans
//! les flux de contenu les marques `BDC /P <</MCID n>> … EMC` qui relient
//! chaque élément de structure au texte qu'il couvre.
//!
//! Le principe, identique à la commande « Baliser automatiquement le
//! document » d'Acrobat :
//!
//! 1. [`crate::text::extract_page_text`] rend les blocs, paragraphes, titres,
//!    listes et tableaux d'une page, dans l'ordre de lecture géométrique ;
//! 2. chaque tranche d'octets du flux (repérée par [`super::marked`]) est
//!    rattachée à l'élément dont la boîte la recouvre le mieux ;
//! 3. le flux est réécrit **octet pour octet** sauf les marques insérées ;
//!    les anciennes marques de balisage sont retirées, `/OC` (contenu
//!    optionnel) est préservé car il pilote la visibilité ;
//! 4. l'arbre, le `/ParentTree`, `/MarkInfo` et `/Lang` sont écrits.
//!
//! Ce que le balisage automatique **ne peut pas** deviner, et qui reste à la
//! charge de l'auteur : le texte de remplacement des figures (`/Alt`), la
//! portée réelle des en-têtes de tableau au-delà de la première ligne, le
//! découpage en sections (`Sect`), les notes et les citations.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use acrux_core::{Rect, Result};
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef, Page};
use acrux_render::page::page_content;

use super::marked::{scan_page, Span, SpanKind};
use crate::text::{BlockKind, PageText};

/// Élément de structure en construction.
struct Node {
    /// Type normalisé (`P`, `H1`, `LI`…).
    kind: &'static str,
    /// Attributs `/A` (propriétaire, portée…).
    attributes: Vec<(&'static str, Object)>,
    /// Indice de la feuille qui porte les `/MCID`, pour les éléments terminaux.
    leaf: Option<usize>,
    /// Objets rattachés par `/OBJR` (annotations, widgets de formulaire).
    objects: Vec<ObjectRef>,
    /// Texte de substitution `/Alt`.
    alt: Option<String>,
    /// Enfants.
    children: Vec<Node>,
}

impl Node {
    fn container(kind: &'static str, children: Vec<Node>) -> Self {
        Self {
            kind,
            attributes: Vec::new(),
            leaf: None,
            objects: Vec::new(),
            alt: None,
            children,
        }
    }

    fn leaf(kind: &'static str, leaf: usize) -> Self {
        Self {
            kind,
            attributes: Vec::new(),
            leaf: Some(leaf),
            objects: Vec::new(),
            alt: None,
            children: Vec::new(),
        }
    }

    /// Élément qui ne couvre pas de contenu marqué mais un objet du document :
    /// une annotation, un widget de formulaire (§14.7.4.3).
    fn object(kind: &'static str, reference: ObjectRef, alt: Option<String>) -> Self {
        Self {
            kind,
            attributes: Vec::new(),
            leaf: None,
            objects: vec![reference],
            alt,
            children: Vec::new(),
        }
    }

    fn with_attribute(mut self, key: &'static str, value: Object) -> Self {
        self.attributes.push((key, value));
        self
    }
}

/// Zone de contenu à laquelle une tranche du flux peut être rattachée.
///
/// Plusieurs rectangles et non un seul : un paragraphe qui se poursuit d'une
/// colonne à l'autre a une boîte englobante qui couvre toute la page, ce qui
/// attraperait le contenu voisin. On garde donc la boîte de chaque ligne.
struct Leaf {
    boxes: Vec<Rect>,
    mcids: Vec<i64>,
}

impl Leaf {
    /// Sommet le plus haut, pour placer les figures dans l'ordre de lecture.
    fn top(&self) -> f64 {
        self.boxes.iter().map(|b| b.y1).fold(f64::MIN, f64::max)
    }
}

/// Destination d'une tranche du flux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Assign {
    /// Contenu d'un élément de structure.
    Leaf(usize),
    /// Décor : `/Artifact`.
    Artifact,
}

/// Balise automatiquement le document et rend le nombre d'éléments de
/// structure créés.
///
/// Écrit `/StructTreeRoot`, `/ParentTree`, `/MarkInfo /Marked true`, `/Lang`
/// (déduit du texte), le `/StructParents` de chaque page et les marques de
/// contenu. Un arbre de structure déjà présent est remplacé.
///
/// # Errors
/// Catalogue illisible, ou page qui n'est pas un objet indirect (le balisage
/// doit pouvoir écrire son `/StructParents`).
#[allow(clippy::too_many_lines)] // une étape par entrée à écrire, lues de haut en bas
pub fn autotag(doc: &Document) -> Result<usize> {
    let pages = collect_pages(doc)?;
    let catalog_ref = catalog_ref(doc)?;
    let mut catalog = doc.catalog()?;

    // Langue : déduite de tout le texte du document.
    let mut sample = String::new();
    let mut page_texts: Vec<PageText> = Vec::with_capacity(pages.len());
    for page in &pages {
        let text = crate::text::extract_page_text(doc, page).unwrap_or_default();
        if sample.len() < 20_000 {
            for line in &text.lines {
                sample.push_str(&line.text());
                sample.push(' ');
            }
        }
        page_texts.push(text);
    }
    let lang = detect_language(&sample).unwrap_or("fr");

    // Les niveaux de titre de `text` sont des rangs de taille : deux tailles
    // peuvent donner 1 et 3. PDF/UA interdit les sauts, on les renumérote sur
    // tout le document en H1, H2, H3… dans l'ordre des tailles.
    let mut levels: BTreeSet<u8> = BTreeSet::new();
    for text in &page_texts {
        for block in &text.blocks {
            for paragraph in &block.paragraphs {
                if paragraph.heading > 0 {
                    levels.insert(paragraph.heading);
                }
            }
        }
    }
    let heading_map: BTreeMap<u8, u8> = levels
        .into_iter()
        .enumerate()
        .map(|(i, level)| (level, u8::try_from(i + 1).unwrap_or(6).min(6)))
        .collect();

    let root_ref = doc.allocate();
    let document_ref = doc.allocate();
    let mut page_nodes: Vec<Node> = Vec::new();
    let mut page_leaves: Vec<Vec<Leaf>> = Vec::new();
    let mut page_refs: Vec<ObjectRef> = Vec::new();
    let mut parent_tree: Vec<Vec<Option<ObjectRef>>> = Vec::new();

    for (index, page) in pages.iter().enumerate() {
        let Some(page_ref) = page.reference else {
            continue;
        };
        let marks = scan_page(doc, page)?;
        let content = page_content(doc, page);
        let text = &page_texts[index];
        let (mut nodes, mut leaves, image_leaves) =
            build_page_tree(text, &marks.spans, &heading_map);
        let assignment = assign_spans(&marks.spans, &leaves, &image_leaves);
        let mut next_mcid = 0_i64;
        let new_content = rewrite(
            &content,
            &marks.spans,
            &assignment,
            &mut leaves,
            &mut next_mcid,
        );
        write_content(doc, page, page_ref, new_content);

        // Un élément dont aucune feuille n'a reçu de contenu n'a rien à dire.
        prune(&mut nodes, &leaves);
        if nodes.is_empty() {
            continue;
        }
        for node in annotation_nodes(doc, page, &marks) {
            nodes.push(node);
        }
        page_nodes.push(Node::container("Part", nodes));
        page_leaves.push(leaves);
        page_refs.push(page_ref);
        parent_tree.push(vec![None; usize::try_from(next_mcid).unwrap_or(0)]);
    }

    if page_nodes.is_empty() {
        return Ok(0);
    }

    // Émission des objets : chaque élément connaît son parent et sa page.
    let mut count = 0usize;
    let mut kids = Vec::new();
    let mut annotations: Vec<(ObjectRef, ObjectRef)> = Vec::new();
    for (i, node) in page_nodes.iter().enumerate() {
        let mut emitter = Emitter {
            doc,
            page_ref: page_refs[i],
            leaves: &page_leaves[i],
            nums: &mut parent_tree[i],
            annotations: &mut annotations,
            count: &mut count,
        };
        let r = emitter.emit(node, document_ref);
        kids.push(Object::Reference(r));
    }

    let mut document = Dict::new();
    document.insert(Name::new("Type"), Object::Name(Name::new("StructElem")));
    document.insert(Name::new("S"), Object::Name(Name::new("Document")));
    document.insert(Name::new("P"), Object::Reference(root_ref));
    document.insert(Name::new("K"), Object::Array(kids));
    doc.set(document_ref, Object::Dict(document));
    count += 1;

    // `/ParentTree` : numéro de `/StructParents` → élément de chaque MCID.
    let mut nums = Vec::new();
    for (i, refs) in parent_tree.iter().enumerate() {
        let key = i64::try_from(i).unwrap_or(0);
        if let Ok(obj) = doc.get(page_refs[i]) {
            if let Some(d) = obj.as_dict() {
                let mut d = d.clone();
                d.insert(Name::new("StructParents"), Object::Integer(key));
                doc.set(page_refs[i], Object::Dict(d));
            }
        }
        nums.push(Object::Integer(key));
        nums.push(Object::Array(
            refs.iter()
                .map(|r| r.map_or(Object::Null, Object::Reference))
                .collect(),
        ));
    }
    // Les annotations rattachées par `/OBJR` ont leur propre clé, singulière
    // (`/StructParent`), qui pointe directement sur leur élément (§14.7.4.4).
    let mut next_key = i64::try_from(parent_tree.len()).unwrap_or(0);
    for (annotation, element) in &annotations {
        if let Ok(obj) = doc.get(*annotation) {
            if let Some(d) = obj.as_dict() {
                let mut d = d.clone();
                d.insert(Name::new("StructParent"), Object::Integer(next_key));
                doc.set(*annotation, Object::Dict(d));
            }
        }
        nums.push(Object::Integer(next_key));
        nums.push(Object::Reference(*element));
        next_key += 1;
    }
    let mut parent_tree_dict = Dict::new();
    parent_tree_dict.insert(Name::new("Nums"), Object::Array(nums));

    let mut root = Dict::new();
    root.insert(Name::new("Type"), Object::Name(Name::new("StructTreeRoot")));
    root.insert(
        Name::new("K"),
        Object::Array(vec![Object::Reference(document_ref)]),
    );
    root.insert(Name::new("ParentTree"), Object::Dict(parent_tree_dict));
    root.insert(Name::new("ParentTreeNextKey"), Object::Integer(next_key));
    doc.set(root_ref, Object::Dict(root));

    let mut mark_info = Dict::new();
    mark_info.insert(Name::new("Marked"), Object::Bool(true));
    catalog.insert(Name::new("MarkInfo"), Object::Dict(mark_info));
    catalog.insert(Name::new("StructTreeRoot"), Object::Reference(root_ref));
    catalog.insert(Name::new("Lang"), Object::String(lang.as_bytes().to_vec()));
    doc.set(catalog_ref, Object::Dict(catalog));
    Ok(count)
}

/// Référence du catalogue (`/Root` du trailer).
fn catalog_ref(doc: &Document) -> Result<ObjectRef> {
    match doc.trailer().get(&Name::new("Root")) {
        Some(Object::Reference(r)) => Ok(*r),
        _ => Err(acrux_core::Error::Corrupt(
            "catalogue absent ou direct : impossible d'y écrire le balisage".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Construction de l'arbre d'une page
// ---------------------------------------------------------------------------

/// Construit les éléments d'une page et les zones de contenu correspondantes.
/// Rend `(nœuds, feuilles, feuille de chaque tranche d'image)`.
fn build_page_tree(
    text: &PageText,
    spans: &[Span],
    heading_map: &BTreeMap<u8, u8>,
) -> (Vec<Node>, Vec<Leaf>, BTreeMap<usize, usize>) {
    let mut leaves: Vec<Leaf> = Vec::new();
    let mut nodes: Vec<Node> = Vec::new();
    let new_leaf = |leaves: &mut Vec<Leaf>, boxes: Vec<Rect>| {
        leaves.push(Leaf {
            boxes,
            mcids: Vec::new(),
        });
        leaves.len() - 1
    };
    // Boîtes des lignes d'un paragraphe : la granularité du rattachement.
    let paragraph_boxes = |p: &crate::text::Paragraph| -> Vec<Rect> {
        let boxes: Vec<Rect> = p
            .lines
            .iter()
            .filter_map(|i| text.lines.get(*i).map(|l| l.bbox))
            .collect();
        if boxes.is_empty() {
            vec![p.bbox]
        } else {
            boxes
        }
    };

    for block in &text.blocks {
        match block.kind {
            // En-têtes et pieds de page courants sont des artéfacts (§14.8.2.2) :
            // ils ne sont pas rattachés à l'arbre, donc aucune feuille.
            BlockKind::Header | BlockKind::Footer => {}
            BlockKind::Table(index) => {
                let Some(table) = text.tables.get(index) else {
                    continue;
                };
                let mut rows = Vec::new();
                for (r, row) in table.rows.iter().enumerate() {
                    let mut cells = Vec::new();
                    for cell in row {
                        // Première ligne : en-tête de colonne, la convention la
                        // plus courante et celle qu'Acrobat applique aussi.
                        let header = r == 0;
                        let leaf = new_leaf(&mut leaves, vec![cell.bbox]);
                        let node = if header {
                            Node::leaf("TH", leaf)
                                .with_attribute("O", Object::Name(Name::new("Table")))
                                .with_attribute("Scope", Object::Name(Name::new("Column")))
                        } else {
                            Node::leaf("TD", leaf)
                        };
                        cells.push(node);
                    }
                    if !cells.is_empty() {
                        rows.push(Node::container("TR", cells));
                    }
                }
                if !rows.is_empty() {
                    nodes.push(Node::container("Table", rows));
                }
            }
            BlockKind::Body => {
                let mut list_items: Vec<Node> = Vec::new();
                let mut numbered = false;
                for paragraph in &block.paragraphs {
                    if let Some(marker) = &paragraph.marker {
                        numbered |= marker.chars().next().is_some_and(char::is_numeric);
                        let leaf = new_leaf(&mut leaves, paragraph_boxes(paragraph));
                        list_items.push(Node::container("LI", vec![Node::leaf("LBody", leaf)]));
                        continue;
                    }
                    if !list_items.is_empty() {
                        nodes.push(finish_list(std::mem::take(&mut list_items), numbered));
                        numbered = false;
                    }
                    let leaf = new_leaf(&mut leaves, paragraph_boxes(paragraph));
                    let level = heading_map
                        .get(&paragraph.heading)
                        .copied()
                        .unwrap_or(paragraph.heading);
                    nodes.push(Node::leaf(heading_kind(level), leaf));
                }
                if !list_items.is_empty() {
                    nodes.push(finish_list(list_items, numbered));
                }
            }
        }
    }

    // Figures : une par image dessinée, insérée dans l'ordre de lecture.
    let mut image_leaves = BTreeMap::new();
    for (i, span) in spans.iter().enumerate() {
        if span.kind != SpanKind::Image {
            continue;
        }
        let Some(bbox) = span.bbox else { continue };
        let leaf = new_leaf(&mut leaves, vec![bbox]);
        image_leaves.insert(i, leaf);
        let node = Node::leaf("Figure", leaf);
        let position = nodes
            .iter()
            .position(|n| node_top(n, &leaves) < bbox.y1)
            .unwrap_or(nodes.len());
        nodes.insert(position, node);
    }
    (nodes, leaves, image_leaves)
}

/// Sommet de la première feuille d'un nœud (pour placer les figures).
fn node_top(node: &Node, leaves: &[Leaf]) -> f64 {
    if let Some(i) = node.leaf {
        return leaves.get(i).map_or(f64::MIN, Leaf::top);
    }
    node.children
        .iter()
        .map(|c| node_top(c, leaves))
        .fold(f64::MIN, f64::max)
}

/// Enveloppe des `LI` dans une liste, en indiquant la numérotation.
fn finish_list(items: Vec<Node>, numbered: bool) -> Node {
    let numbering = if numbered { "Decimal" } else { "Disc" };
    Node::container("L", items)
        .with_attribute("O", Object::Name(Name::new("List")))
        .with_attribute("ListNumbering", Object::Name(Name::new(numbering)))
}

/// Niveau de titre détecté par la mise en page → type de structure.
fn heading_kind(level: u8) -> &'static str {
    match level {
        1 => "H1",
        2 => "H2",
        3 => "H3",
        4 => "H4",
        5 => "H5",
        6 => "H6",
        _ => "P",
    }
}

/// Retire les éléments dont aucune feuille n'a reçu de contenu.
fn prune(nodes: &mut Vec<Node>, leaves: &[Leaf]) {
    nodes.retain_mut(|node| {
        if !node.objects.is_empty() {
            return true;
        }
        if let Some(i) = node.leaf {
            return leaves.get(i).is_some_and(|l| !l.mcids.is_empty());
        }
        prune(&mut node.children, leaves);
        !node.children.is_empty()
    });
}

// ---------------------------------------------------------------------------
// Rattachement des tranches du flux
// ---------------------------------------------------------------------------

/// Pour chaque tranche du flux, l'élément (ou l'artéfact) auquel elle revient.
fn assign_spans(
    spans: &[Span],
    leaves: &[Leaf],
    image_leaves: &BTreeMap<usize, usize>,
) -> Vec<Option<Assign>> {
    spans
        .iter()
        .enumerate()
        .map(|(i, span)| match span.kind {
            SpanKind::Image => image_leaves.get(&i).map(|l| Assign::Leaf(*l)),
            SpanKind::Paint => Some(Assign::Artifact),
            SpanKind::Text => {
                let bbox = span.bbox?;
                Some(best_leaf(&bbox, leaves).map_or(Assign::Artifact, Assign::Leaf))
            }
            SpanKind::Structural | SpanKind::Drop => None,
        })
        .collect()
}

/// Feuille dont la boîte recouvre le mieux celle de la tranche.
fn best_leaf(bbox: &Rect, leaves: &[Leaf]) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, leaf) in leaves.iter().enumerate() {
        let mut area = 0.0_f64;
        for b in &leaf.boxes {
            // Tolérance d'un point : les boîtes de glyphes et celles des
            // lignes ne sont pas calculées avec les mêmes arrondis.
            let grown = Rect::new(b.x0 - 1.0, b.y0 - 1.0, b.x1 + 1.0, b.y1 + 1.0);
            let inter = grown.intersect(bbox);
            area = area.max(inter.width() * inter.height());
        }
        if area <= 0.0 {
            continue;
        }
        if best.is_none_or(|(_, b)| area > b) {
            best = Some((i, area));
        }
    }
    best.map(|(i, _)| i)
}

// ---------------------------------------------------------------------------
// Réécriture du flux
// ---------------------------------------------------------------------------

/// Réécrit le flux en n'y changeant que les marques : le reste est recopié
/// octet pour octet.
fn rewrite(
    content: &[u8],
    spans: &[Span],
    assignment: &[Option<Assign>],
    leaves: &mut [Leaf],
    next_mcid: &mut i64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len() + 256);
    let mut cursor = 0usize;
    let mut open: Option<Assign> = None;
    let mut last_end = 0usize;

    for (i, span) in spans.iter().enumerate() {
        let assign = assignment.get(i).copied().flatten();
        match span.kind {
            SpanKind::Drop => {
                close(&mut out, content, &mut cursor, &mut open, last_end);
                if span.start >= cursor {
                    out.extend_from_slice(&content[cursor..span.start]);
                }
                cursor = span.end.max(cursor);
            }
            SpanKind::Structural => {
                close(&mut out, content, &mut cursor, &mut open, last_end);
            }
            SpanKind::Text | SpanKind::Image | SpanKind::Paint => {
                let Some(assign) = assign else { continue };
                if open != Some(assign) {
                    close(&mut out, content, &mut cursor, &mut open, last_end);
                    let start = span.start.max(cursor);
                    out.extend_from_slice(&content[cursor..start]);
                    cursor = start;
                    match assign {
                        Assign::Artifact => out.extend_from_slice(b"\n/Artifact BMC\n"),
                        Assign::Leaf(leaf) => {
                            let mcid = *next_mcid;
                            *next_mcid += 1;
                            if let Some(l) = leaves.get_mut(leaf) {
                                l.mcids.push(mcid);
                            }
                            let mut marker = String::new();
                            let _ = write!(marker, "\n/P <</MCID {mcid}>> BDC\n");
                            out.extend_from_slice(marker.as_bytes());
                        }
                    }
                    open = Some(assign);
                }
                last_end = span.end;
            }
        }
    }
    close(&mut out, content, &mut cursor, &mut open, last_end);
    if cursor < content.len() {
        out.extend_from_slice(&content[cursor..]);
    }
    out
}

/// Ferme la marque ouverte : recopie jusqu'à la fin du dernier contenu marqué
/// puis écrit `EMC`.
fn close(
    out: &mut Vec<u8>,
    content: &[u8],
    cursor: &mut usize,
    open: &mut Option<Assign>,
    last_end: usize,
) {
    if open.take().is_none() {
        return;
    }
    let end = last_end.max(*cursor).min(content.len());
    out.extend_from_slice(&content[*cursor..end]);
    *cursor = end;
    out.extend_from_slice(b"\nEMC\n");
}

/// Remplace le contenu de la page par un flux unique non compressé.
fn write_content(doc: &Document, page: &Page, page_ref: ObjectRef, data: Vec<u8>) {
    for r in content_refs(doc, page) {
        doc.delete(r);
    }
    let mut stream_dict = Dict::new();
    stream_dict.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(data.len()).unwrap_or(0)),
    );
    let stream = doc.add(Object::Stream {
        dict: stream_dict,
        raw: data,
    });
    let mut dict = page.dict.clone();
    dict.insert(Name::new("Contents"), Object::Reference(stream));
    doc.set(page_ref, Object::Dict(dict));
}

/// Références des flux de contenu d'une page.
fn content_refs(doc: &Document, page: &Page) -> Vec<ObjectRef> {
    let Some(contents) = page.dict.get(&Name::new("Contents")) else {
        return Vec::new();
    };
    match contents {
        Object::Reference(r) => match doc.get(*r).map(|o| matches!(&*o, Object::Array(_))) {
            Ok(true) => doc
                .get(*r)
                .ok()
                .and_then(|o| o.as_array().map(<[Object]>::to_vec))
                .unwrap_or_default()
                .iter()
                .filter_map(|o| match o {
                    Object::Reference(r) => Some(*r),
                    _ => None,
                })
                .chain(std::iter::once(*r))
                .collect(),
            _ => vec![*r],
        },
        Object::Array(items) => items
            .iter()
            .filter_map(|o| match o {
                Object::Reference(r) => Some(*r),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Émission des objets de structure
// ---------------------------------------------------------------------------

/// Écrit les objets de structure d'une page.
struct Emitter<'a> {
    doc: &'a Document,
    page_ref: ObjectRef,
    leaves: &'a [Leaf],
    /// Tableau du `/ParentTree` de la page : MCID → élément.
    nums: &'a mut [Option<ObjectRef>],
    /// Couples (annotation, élément) à relier par `/StructParent`.
    annotations: &'a mut Vec<(ObjectRef, ObjectRef)>,
    count: &'a mut usize,
}

impl Emitter<'_> {
    fn emit(&mut self, node: &Node, parent: ObjectRef) -> ObjectRef {
        let reference = self.doc.allocate();
        *self.count += 1;
        let mut dict = Dict::new();
        dict.insert(Name::new("Type"), Object::Name(Name::new("StructElem")));
        dict.insert(Name::new("S"), Object::Name(Name::new(node.kind)));
        dict.insert(Name::new("P"), Object::Reference(parent));
        dict.insert(Name::new("Pg"), Object::Reference(self.page_ref));
        if let Some(alt) = &node.alt {
            dict.insert(Name::new("Alt"), Object::String(encode_text_string(alt)));
        }
        if !node.attributes.is_empty() {
            let mut attrs = Dict::new();
            for (k, v) in &node.attributes {
                attrs.insert(Name::new(k), v.clone());
            }
            dict.insert(Name::new("A"), Object::Dict(attrs));
        }
        let mut kids = Vec::new();
        if let Some(i) = node.leaf {
            if let Some(leaf) = self.leaves.get(i) {
                for mcid in &leaf.mcids {
                    kids.push(Object::Integer(*mcid));
                    if let Ok(index) = usize::try_from(*mcid) {
                        if index < self.nums.len() {
                            self.nums[index] = Some(reference);
                        }
                    }
                }
            }
        }
        for object in &node.objects {
            let mut objr = Dict::new();
            objr.insert(Name::new("Type"), Object::Name(Name::new("OBJR")));
            objr.insert(Name::new("Obj"), Object::Reference(*object));
            objr.insert(Name::new("Pg"), Object::Reference(self.page_ref));
            kids.push(Object::Dict(objr));
            self.annotations.push((*object, reference));
        }
        for child in &node.children {
            let r = self.emit(child, reference);
            kids.push(Object::Reference(r));
        }
        dict.insert(Name::new("K"), Object::Array(kids));
        self.doc.set(reference, Object::Dict(dict));
        reference
    }
}

/// Encode une chaîne de texte PDF : ASCII tel quel, sinon UTF-16BE avec BOM
/// (§7.9.2.2).
fn encode_text_string(s: &str) -> Vec<u8> {
    if s.is_ascii() {
        return s.as_bytes().to_vec();
    }
    let mut out = vec![0xFE, 0xFF];
    for unit in s.encode_utf16() {
        out.extend_from_slice(&unit.to_be_bytes());
    }
    out
}

/// Éléments `Link` et `Form` des annotations de la page : PDF/UA exige que
/// chaque lien et chaque champ de formulaire soit atteignable depuis l'arbre
/// (§7.18). Le texte couvert par le rectangle de l'annotation devient son
/// `/Alt` : c'est ce qu'une synthèse vocale annonce.
fn annotation_nodes(doc: &Document, page: &Page, marks: &super::PageMarks) -> Vec<Node> {
    let Some(annots) = doc
        .dict_get(&page.dict, "Annots")
        .ok()
        .flatten()
        .and_then(|a| a.as_array().map(<[Object]>::to_vec))
    else {
        return Vec::new();
    };
    let mut out: Vec<(f64, Node)> = Vec::new();
    for item in &annots {
        let Object::Reference(reference) = item else {
            continue;
        };
        let Ok(resolved) = doc.resolve(item) else {
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
        let kind = match subtype.as_str() {
            "Link" => "Link",
            "Widget" => "Form",
            _ => continue,
        };
        let rect = doc
            .dict_get(d, "Rect")
            .ok()
            .flatten()
            .and_then(|r| {
                let v: Vec<f64> = r.as_array()?.iter().filter_map(Object::as_f64).collect();
                (v.len() == 4).then(|| Rect::new(v[0], v[1], v[2], v[3]))
            })
            .unwrap_or_default();
        // `/Alt` : le texte que le rectangle recouvre, à défaut le `/Contents`.
        let mut covered = String::new();
        for run in &marks.runs {
            if !run.bbox.intersect(&rect).is_empty() {
                if !covered.is_empty() {
                    covered.push(' ');
                }
                covered.push_str(run.text.trim());
            }
        }
        let alt = if covered.trim().is_empty() {
            None
        } else {
            Some(covered.trim().to_string())
        };
        out.push((rect.y1, Node::object(kind, *reference, alt)));
    }
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    out.into_iter().map(|(_, n)| n).collect()
}

// ---------------------------------------------------------------------------
// Détection de langue
// ---------------------------------------------------------------------------

/// Mots outils du français, choisis parce qu'ils sont fréquents, courts et
/// absents de l'anglais.
const FRENCH_WORDS: [&str; 24] = [
    "le", "la", "les", "des", "une", "est", "dans", "pour", "que", "qui", "pas", "sur", "avec",
    "aux", "cette", "sont", "être", "plus", "nous", "vous", "leur", "ses", "ou", "mais",
];

/// Mots outils de l'anglais, mêmes critères.
const ENGLISH_WORDS: [&str; 24] = [
    "the", "of", "and", "to", "in", "is", "that", "for", "it", "with", "as", "was", "on", "are",
    "this", "be", "by", "from", "at", "which", "have", "not", "they", "but",
];

/// Devine la langue d'un texte (« fr » ou « en ») en comptant les mots outils.
///
/// Méthode volontairement simple et documentée : on compte les occurrences de
/// deux listes de mots très fréquents, et on ne tranche que si l'une domine
/// nettement l'autre (au moins 3 occurrences et 50 % d'avance). Sinon la
/// fonction rend `None` plutôt qu'une langue fausse — déclarer une mauvaise
/// langue est pire pour une synthèse vocale que de n'en déclarer aucune.
#[must_use]
pub fn detect_language(text: &str) -> Option<&'static str> {
    let mut french = 0usize;
    let mut english = 0usize;
    for word in text.split(|c: char| !c.is_alphabetic() && c != '\'') {
        let word = word.trim_matches('\'').to_lowercase();
        if word.is_empty() {
            continue;
        }
        if FRENCH_WORDS.contains(&word.as_str()) {
            french += 1;
        }
        if ENGLISH_WORDS.contains(&word.as_str()) {
            english += 1;
        }
    }
    let (winner, top, other) = if french >= english {
        ("fr", french, english)
    } else {
        ("en", english, french)
    };
    if top >= 3 && top * 2 >= other * 3 {
        Some(winner)
    } else {
        None
    }
}
