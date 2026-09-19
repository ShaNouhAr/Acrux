//! Édition des signets (`/Outlines`, §12.3.3) : le panneau *Signets*
//! d'Acrobat.
//!
//! [`crate::navigation::outline`] sait lire l'arbre ; ce module sait
//! l'écrire, l'enrichir (gras, italique, couleur, nœud ouvert ou fermé) et
//! le remanier.
//!
//! ## Pourquoi tout réécrire à chaque fois
//!
//! Un arbre de signets n'est pas rangé comme un arbre ordinaire : chaque
//! nœud connaît son `/Parent`, son `/First` et son `/Last` enfant, son
//! `/Prev` et son `/Next` frère, et porte un `/Count` **signé** dont la
//! valeur absolue compte les descendants qui seraient visibles si le nœud
//! était ouvert. Déplacer un signet d'un parent à l'autre touche donc
//! jusqu'à huit dictionnaires, et une seule erreur de chaînage rend le
//! panneau inutilisable dans tous les lecteurs.
//!
//! [`add_bookmark`], [`remove_bookmark`] et [`move_bookmark`] travaillent
//! pour cette raison sur l'arbre **en mémoire** puis le réécrivent en
//! entier par [`set_outline`] : le chaînage est reconstruit d'un bloc, il
//! est donc juste par construction. Les anciens objets sont libérés.
//!
//! ## Chemins
//!
//! Un signet est désigné par un **chemin** : `[0, 2]` est le troisième
//! enfant du premier signet de premier niveau.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use acrux_core::{Error, Point, Rect, Result};
use acrux_document::text::decode_text_string;
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef, Page};

use crate::annotations::encode_text;
use crate::navigation::{action_object, action_of, Action, Destination, PageIndex, View};

/// Profondeur maximale d'un arbre de signets (anti-cycle et garde-fou).
const MAX_DEPTH: usize = 64;

/// Un signet, avec tout ce que le panneau d'Acrobat sait en montrer.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OutlineNode {
    /// Titre affiché.
    pub title: String,
    /// Titre en gras (`/F` bit 2).
    pub bold: bool,
    /// Titre en italique (`/F` bit 1).
    pub italic: bool,
    /// Couleur du titre, RVB dans [0, 1] (`/C`) ; `None` = noir.
    pub color: Option<[f64; 3]>,
    /// Nœud déplié à l'ouverture du document (`/Count` positif).
    pub open: bool,
    /// Ce que fait un clic : destination, adresse, action nommée…
    pub action: Option<Action>,
    /// Sous-signets.
    pub children: Vec<OutlineNode>,
}

impl OutlineNode {
    /// Signet menant à une page, cadrée en pleine page.
    #[must_use]
    pub fn to_page(title: impl Into<String>, page: usize) -> Self {
        Self {
            title: title.into(),
            action: Some(Action::GoTo(Destination {
                page,
                view: View::Fit,
            })),
            ..Self::default()
        }
    }

    /// Nombre de signets visibles sous ce nœud si son parent le montre :
    /// lui-même exclu, les enfants d'un nœud fermé exclus (§12.3.3).
    #[must_use]
    pub fn visible_descendants(&self) -> usize {
        self.children
            .iter()
            .map(|c| 1 + if c.open { c.visible_descendants() } else { 0 })
            .sum()
    }

    /// Nombre total de signets de la sous-arborescence, nœud compris.
    #[must_use]
    pub fn len(&self) -> usize {
        1 + self.children.iter().map(OutlineNode::len).sum::<usize>()
    }

    /// Toujours faux : un nœud existe, donc il compte au moins pour un.
    /// Présent parce que `len` sans `is_empty` est une faute de style.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }
}

/// Nombre de signets visibles d'une forêt, pour le `/Count` de la racine.
fn visible_of(nodes: &[OutlineNode]) -> usize {
    nodes
        .iter()
        .map(|n| 1 + if n.open { n.visible_descendants() } else { 0 })
        .sum()
}

// ---------------------------------------------------------------------------
// Lecture
// ---------------------------------------------------------------------------

/// Lit l'arbre des signets avec son style : c'est la lecture qu'attend
/// l'éditeur, là où [`crate::navigation::outline`] sert au visualiseur.
///
/// # Errors
/// Catalogue illisible.
pub fn read_outline(doc: &Document) -> Result<Vec<OutlineNode>> {
    let pages = collect_pages(doc)?;
    let index = PageIndex::new(&pages);
    let catalog = doc.catalog()?;
    let Some(root) = doc.dict_get(&catalog, "Outlines")? else {
        return Ok(Vec::new());
    };
    let Some(root) = root.as_dict() else {
        return Ok(Vec::new());
    };
    let mut visited = HashSet::new();
    Ok(read_children(doc, root, &index, &mut visited, 0))
}

fn read_children(
    doc: &Document,
    parent: &Dict,
    index: &PageIndex,
    visited: &mut HashSet<u32>,
    depth: usize,
) -> Vec<OutlineNode> {
    let mut out = Vec::new();
    if depth > MAX_DEPTH {
        return out;
    }
    let mut next = parent.get(&Name::new("First")).cloned();
    while let Some(object) = next {
        if let Object::Reference(r) = object {
            if !visited.insert(r.number) {
                break;
            }
        }
        let Ok(item) = doc.resolve(&object) else {
            break;
        };
        let Some(d) = item.as_dict() else { break };
        let flags = doc
            .dict_get(d, "F")
            .ok()
            .flatten()
            .and_then(|f| f.as_i64())
            .unwrap_or(0);
        let count = doc
            .dict_get(d, "Count")
            .ok()
            .flatten()
            .and_then(|c| c.as_i64())
            .unwrap_or(0);
        out.push(OutlineNode {
            title: match doc.dict_get(d, "Title").ok().flatten().as_deref() {
                Some(Object::String(s)) => decode_text_string(s),
                _ => String::new(),
            },
            italic: flags & 1 != 0,
            bold: flags & 2 != 0,
            color: read_color(doc, d),
            open: count > 0,
            action: action_of(doc, d, index),
            children: read_children(doc, d, index, visited, depth + 1),
        });
        next = d.get(&Name::new("Next")).cloned();
    }
    out
}

/// `/C` : trois nombres RVB. Le noir par défaut est rendu `None` pour ne pas
/// écrire une couleur là où le fichier n'en portait pas.
fn read_color(doc: &Document, item: &Dict) -> Option<[f64; 3]> {
    let entry = doc.dict_get(item, "C").ok().flatten()?;
    let values: Vec<f64> = entry
        .as_array()?
        .iter()
        .filter_map(|v| doc.resolve(v).ok().and_then(|r| r.as_f64()))
        .collect();
    let color: [f64; 3] = values.try_into().ok()?;
    color.iter().any(|v| *v != 0.0).then_some(color)
}

/// Aplatit l'arbre : `(chemin, nœud)` dans l'ordre d'affichage.
#[must_use]
pub fn flatten(nodes: &[OutlineNode]) -> Vec<(Vec<usize>, &OutlineNode)> {
    fn walk<'a>(
        nodes: &'a [OutlineNode],
        prefix: &mut Vec<usize>,
        out: &mut Vec<(Vec<usize>, &'a OutlineNode)>,
    ) {
        for (i, node) in nodes.iter().enumerate() {
            prefix.push(i);
            out.push((prefix.clone(), node));
            walk(&node.children, prefix, out);
            prefix.pop();
        }
    }
    let mut out = Vec::new();
    walk(nodes, &mut Vec::new(), &mut out);
    out
}

// ---------------------------------------------------------------------------
// Écriture
// ---------------------------------------------------------------------------

/// Référence du catalogue (`/Root` du trailer).
fn catalog_ref(doc: &Document) -> Result<ObjectRef> {
    match doc.trailer().get(&Name::new("Root")) {
        Some(Object::Reference(r)) => Ok(*r),
        _ => Err(Error::Corrupt(
            "catalogue absent ou direct : impossible d'y écrire les signets".into(),
        )),
    }
}

/// Écrit l'arbre des signets en entier.
///
/// Les anciens nœuds sont libérés, le chaînage (`/First`, `/Last`, `/Prev`,
/// `/Next`, `/Parent`) et les `/Count` signés sont reconstruits. Une forêt
/// vide retire `/Outlines` du catalogue.
///
/// # Errors
/// Catalogue absent ou direct ; destination désignant une page inexistante.
pub fn set_outline(doc: &Document, nodes: &[OutlineNode]) -> Result<()> {
    let catalog_reference = catalog_ref(doc)?;
    let mut catalog = doc.catalog()?;
    let pages = collect_pages(doc)?;
    delete_old_tree(doc, &catalog);
    if nodes.is_empty() {
        catalog.remove(&Name::new("Outlines"));
        // Sans signets, demander le panneau des signets à l'ouverture
        // afficherait un panneau vide.
        if matches!(catalog.get(&Name::new("PageMode")),
            Some(Object::Name(n)) if n.0 == b"UseOutlines")
        {
            catalog.remove(&Name::new("PageMode"));
        }
        doc.set(catalog_reference, Object::Dict(catalog));
        return Ok(());
    }
    let root = doc.allocate();
    let (first, last) = write_level(doc, &pages, nodes, root, 0)?;
    let mut dict = Dict::new();
    dict.insert(Name::new("Type"), Object::Name(Name::new("Outlines")));
    dict.insert(Name::new("First"), Object::Reference(first));
    dict.insert(Name::new("Last"), Object::Reference(last));
    dict.insert(
        Name::new("Count"),
        Object::Integer(i64::try_from(visible_of(nodes)).unwrap_or(0)),
    );
    doc.set(root, Object::Dict(dict));
    catalog.insert(Name::new("Outlines"), Object::Reference(root));
    doc.set(catalog_reference, Object::Dict(catalog));
    Ok(())
}

/// Écrit une fratrie et rend `(premier, dernier)`.
fn write_level(
    doc: &Document,
    pages: &[Page],
    nodes: &[OutlineNode],
    parent: ObjectRef,
    depth: usize,
) -> Result<(ObjectRef, ObjectRef)> {
    if depth > MAX_DEPTH {
        return Err(Error::Corrupt(format!(
            "arbre de signets trop profond (plus de {MAX_DEPTH} niveaux)"
        )));
    }
    // Les références sont réservées d'abord : un nœud cite ses frères.
    let refs: Vec<ObjectRef> = nodes.iter().map(|_| doc.allocate()).collect();
    for (i, node) in nodes.iter().enumerate() {
        let mut d = Dict::new();
        d.insert(Name::new("Title"), Object::String(encode_text(&node.title)));
        d.insert(Name::new("Parent"), Object::Reference(parent));
        if i > 0 {
            d.insert(Name::new("Prev"), Object::Reference(refs[i - 1]));
        }
        if let Some(next) = refs.get(i + 1) {
            d.insert(Name::new("Next"), Object::Reference(*next));
        }
        match &node.action {
            // Une destination s'écrit dans /Dest : plus court qu'une action
            // /GoTo et compris par tous les lecteurs depuis PDF 1.0.
            Some(Action::GoTo(destination)) => {
                let object =
                    crate::navigation::destination_object(pages, destination).ok_or_else(|| {
                        Error::Corrupt(format!(
                            "signet « {} » : page {} inexistante",
                            node.title,
                            destination.page + 1
                        ))
                    })?;
                d.insert(Name::new("Dest"), object);
            }
            Some(action) => {
                if let Some(object) = action_object(pages, action) {
                    d.insert(Name::new("A"), object);
                }
            }
            None => {}
        }
        let flags = i64::from(node.italic) | (i64::from(node.bold) << 1);
        if flags != 0 {
            d.insert(Name::new("F"), Object::Integer(flags));
        }
        if let Some([r, g, b]) = node.color {
            d.insert(
                Name::new("C"),
                Object::Array(vec![Object::Real(r), Object::Real(g), Object::Real(b)]),
            );
        }
        if !node.children.is_empty() {
            let (first, last) = write_level(doc, pages, &node.children, refs[i], depth + 1)?;
            d.insert(Name::new("First"), Object::Reference(first));
            d.insert(Name::new("Last"), Object::Reference(last));
            // §12.3.3 : positif si ouvert, négatif si fermé, la valeur
            // absolue comptant les descendants qui seraient visibles.
            let visible = i64::try_from(node.visible_descendants()).unwrap_or(0);
            d.insert(
                Name::new("Count"),
                Object::Integer(if node.open { visible } else { -visible }),
            );
        }
        doc.set(refs[i], Object::Dict(d));
    }
    // `nodes` n'est jamais vide ici : l'appelant l'a vérifié.
    match (refs.first(), refs.last()) {
        (Some(first), Some(last)) => Ok((*first, *last)),
        _ => Err(Error::Corrupt("fratrie de signets vide".into())),
    }
}

/// Libère les objets de l'arbre `/Outlines` existant.
fn delete_old_tree(doc: &Document, catalog: &Dict) {
    let mut stack = match catalog.get(&Name::new("Outlines")) {
        Some(Object::Reference(r)) => vec![*r],
        _ => Vec::new(),
    };
    let mut seen = HashSet::new();
    while let Some(r) = stack.pop() {
        if !seen.insert(r.number) || seen.len() > 100_000 {
            continue;
        }
        if let Ok(object) = doc.get(r) {
            if let Some(d) = object.as_dict() {
                for key in ["First", "Next"] {
                    if let Some(Object::Reference(k)) = d.get(&Name::new(key)) {
                        stack.push(*k);
                    }
                }
            }
        }
        doc.delete(r);
    }
}

// ---------------------------------------------------------------------------
// Remaniements
// ---------------------------------------------------------------------------

/// Fratrie désignée par un chemin (`[]` = premier niveau).
fn siblings_mut<'a>(
    tree: &'a mut Vec<OutlineNode>,
    path: &[usize],
) -> Option<&'a mut Vec<OutlineNode>> {
    let mut current = tree;
    for step in path {
        current = &mut current.get_mut(*step)?.children;
    }
    Some(current)
}

/// Erreur « ce chemin ne mène nulle part ».
fn unknown_path(path: &[usize]) -> Error {
    Error::Corrupt(format!(
        "signet {} inexistant",
        path.iter()
            .map(|i| (i + 1).to_string())
            .collect::<Vec<_>>()
            .join(".")
    ))
}

/// Insère un signet parmi les enfants de `parent`, à la position `index`
/// (au-delà de la fin : ajouté en dernier).
///
/// # Errors
/// Chemin de parent inexistant, ou écriture impossible.
pub fn add_bookmark(
    doc: &Document,
    parent: &[usize],
    index: usize,
    node: OutlineNode,
) -> Result<()> {
    let mut tree = read_outline(doc)?;
    let siblings = siblings_mut(&mut tree, parent).ok_or_else(|| unknown_path(parent))?;
    let at = index.min(siblings.len());
    siblings.insert(at, node);
    set_outline(doc, &tree)
}

/// Supprime le signet désigné **et sa descendance** (c'est ce que fait
/// Acrobat : un signet supprimé emporte ses sous-signets) et le rend, pour
/// qu'un appelant puisse le replacer ailleurs.
///
/// # Errors
/// Chemin inexistant, ou écriture impossible.
pub fn remove_bookmark(doc: &Document, path: &[usize]) -> Result<OutlineNode> {
    let mut tree = read_outline(doc)?;
    let removed = detach(&mut tree, path).ok_or_else(|| unknown_path(path))?;
    set_outline(doc, &tree)?;
    Ok(removed)
}

/// Retire un nœud de l'arbre en mémoire.
fn detach(tree: &mut Vec<OutlineNode>, path: &[usize]) -> Option<OutlineNode> {
    let (last, parent) = path.split_last()?;
    let siblings = siblings_mut(tree, parent)?;
    (*last < siblings.len()).then(|| siblings.remove(*last))
}

/// Déplace un signet : nouveau parent (`[]` = premier niveau) et nouveau
/// rang parmi ses frères.
///
/// Les positions de `to_parent` et de `index` s'entendent dans l'arbre
/// **avant** le déplacement ; elles sont corrigées en interne du décalage
/// que provoque le retrait. Déplacer un signet dans sa propre descendance
/// est refusé.
///
/// # Errors
/// Chemin inexistant, déplacement dans sa propre descendance, ou écriture
/// impossible.
pub fn move_bookmark(
    doc: &Document,
    from: &[usize],
    to_parent: &[usize],
    index: usize,
) -> Result<()> {
    let Some((from_index, from_parent)) = from.split_last() else {
        return Err(Error::Corrupt(
            "chemin de départ vide : aucun signet désigné".into(),
        ));
    };
    if to_parent.len() >= from.len() && to_parent[..from.len()] == *from {
        return Err(Error::Corrupt(format!(
            "un signet ne peut pas être déplacé dans sa propre descendance ({} → {})",
            path_label(from),
            path_label(to_parent)
        )));
    }
    let mut tree = read_outline(doc)?;
    let node = detach(&mut tree, from).ok_or_else(|| unknown_path(from))?;
    // Le retrait décale les frères qui suivaient le nœud déplacé.
    let mut target: Vec<usize> = to_parent.to_vec();
    if target.len() > from_parent.len() && target[..from_parent.len()] == *from_parent {
        let step = &mut target[from_parent.len()];
        if *step > *from_index {
            *step -= 1;
        }
    }
    let mut at = index;
    if target == from_parent && at > *from_index {
        at -= 1;
    }
    let siblings = siblings_mut(&mut tree, &target).ok_or_else(|| unknown_path(to_parent))?;
    let at = at.min(siblings.len());
    siblings.insert(at, node);
    set_outline(doc, &tree)
}

/// Chemin lisible (`1.3.2`).
fn path_label(path: &[usize]) -> String {
    if path.is_empty() {
        return "racine".to_string();
    }
    path.iter()
        .map(|i| (i + 1).to_string())
        .collect::<Vec<_>>()
        .join(".")
}

// ---------------------------------------------------------------------------
// Signets automatiques
// ---------------------------------------------------------------------------

/// Construit un arbre de signets à partir des titres du document — la
/// commande « Créer des signets automatiquement » d'Acrobat.
///
/// Deux sources, dans cet ordre :
///
/// 1. **la structure balisée** (`H1`…`H6`) quand le document en porte une :
///    c'est la hiérarchie voulue par l'auteur, sans devinette ;
/// 2. à défaut, les **titres déduits de la mise en page** par
///    [`crate::text`] (taille de police dominante dépassée, niveaux 1 à 3).
///
/// Un titre mène à sa propre position dans la page (`/XYZ` sur le coin haut
/// gauche de sa boîte), zoom inchangé : cliquer un signet ne doit pas
/// changer le grossissement choisi par le lecteur.
///
/// Les nœuds rendus sont **ouverts** au premier niveau et fermés en dessous,
/// comme le fait Acrobat sur un document long.
///
/// # Errors
/// Pages ou flux de contenu illisibles.
pub fn outline_from_headings(doc: &Document) -> Result<Vec<OutlineNode>> {
    let mut headings = headings_from_structure(doc)?;
    if headings.is_empty() {
        headings = headings_from_layout(doc)?;
    }
    Ok(nest(&headings))
}

/// Un titre repéré : niveau, texte, destination.
struct Heading {
    level: u8,
    title: String,
    destination: Destination,
}

/// Titres de l'arbre de structure balisée.
fn headings_from_structure(doc: &Document) -> Result<Vec<Heading>> {
    let tree = crate::accessibility::read_structure(doc)?;
    if !tree.is_tagged() {
        return Ok(Vec::new());
    }
    let pages = collect_pages(doc)?;
    // Le texte d'un élément de structure est la suite de ses contenus
    // marqués mis bout à bout : les espaces entre deux `/MCID` n'y sont pas,
    // et un titre composé de plusieurs marques ressortirait collé
    // (« Extractionendeuxcolonnes »). La structure donne donc la hiérarchie
    // et la position ; le texte exact vient de la mise en page, relue dans
    // la boîte de l'élément. Les pages sont extraites à la demande.
    let mut extracted: HashMap<usize, crate::text::PageText> = HashMap::new();
    let mut out = Vec::new();
    for (element, _) in tree.elements() {
        let Some(level) = element.heading_level() else {
            continue;
        };
        let page = element.page.unwrap_or(0);
        let title = match (element.bbox, pages.get(page)) {
            (Some(box_), Some(p)) => {
                let text = match extracted.entry(page) {
                    Entry::Occupied(e) => Some(e.into_mut()),
                    Entry::Vacant(e) => crate::text::extract_page_text(doc, p)
                        .ok()
                        .map(|t| e.insert(t)),
                };
                text.and_then(|t| respaced(t, &box_, &element.text))
                    .unwrap_or_else(|| clean_title(&element.text))
            }
            _ => clean_title(&element.text),
        };
        if title.is_empty() {
            continue;
        }
        out.push(Heading {
            level,
            title,
            destination: Destination {
                page,
                view: View::Xyz {
                    left: element.bbox.map(|b| b.x0),
                    top: element.bbox.map(|b| b.y1),
                    zoom: None,
                },
            },
        });
    }
    Ok(out)
}

/// Rend le texte de la mise en page qui correspond exactement à `wanted`,
/// aux blancs près, parmi les lignes dont le centre tombe dans `box_`.
///
/// La condition d'égalité une fois les blancs retirés est ce qui rend le
/// procédé sûr : on ne fait que **rendre ses espaces** au texte de la
/// structure, jamais lui ajouter du texte voisin. Une boîte de titre qui
/// déborde sur d'autres lignes — cela arrive sur un bloc pivoté — rend donc
/// `None`, et l'appelant s'en tient au texte de la structure.
fn respaced(text: &crate::text::PageText, box_: &Rect, wanted: &str) -> Option<String> {
    let strip = |s: &str| -> String { s.split_whitespace().collect() };
    let target = strip(wanted);
    if target.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    let mut seen = String::new();
    for line in &text.lines {
        let center = Point::new(
            f64::midpoint(line.bbox.x0, line.bbox.x1),
            f64::midpoint(line.bbox.y0, line.bbox.y1),
        );
        if !box_.contains(center) {
            continue;
        }
        parts.push(line.text());
        seen.push_str(&strip(&line.text()));
        if seen == target {
            return Some(clean_title(&parts.join(" ")));
        }
        if seen.len() >= target.len() {
            return None;
        }
    }
    None
}

/// Titres déduits de la mise en page, page par page.
fn headings_from_layout(doc: &Document) -> Result<Vec<Heading>> {
    let mut out = Vec::new();
    for page in collect_pages(doc)? {
        let Ok(text) = crate::text::extract_page_text(doc, &page) else {
            continue;
        };
        for block in &text.blocks {
            if block.kind != crate::text::BlockKind::Body {
                continue;
            }
            for paragraph in &block.paragraphs {
                if paragraph.heading == 0 {
                    continue;
                }
                let title = clean_title(&paragraph.text);
                if title.is_empty() {
                    continue;
                }
                out.push(Heading {
                    level: paragraph.heading,
                    title,
                    destination: Destination {
                        page: page.index,
                        view: View::Xyz {
                            left: Some(paragraph.bbox.x0),
                            top: Some(paragraph.bbox.y1),
                            zoom: None,
                        },
                    },
                });
            }
        }
    }
    Ok(out)
}

/// Longueur maximale d'un titre de signet engendré.
const MAX_TITLE: usize = 120;

/// Nettoie un titre : espaces normalisés, coupé à [`MAX_TITLE`] caractères.
fn clean_title(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let joined = words.join(" ");
    if joined.chars().count() <= MAX_TITLE {
        return joined;
    }
    let cut: String = joined.chars().take(MAX_TITLE).collect();
    // Coupe au dernier mot entier plutôt qu'au milieu.
    match cut.rfind(' ') {
        Some(space) if space > MAX_TITLE / 2 => format!("{}…", &cut[..space]),
        _ => format!("{cut}…"),
    }
}

/// Imbrique une suite plate de titres selon leur niveau.
///
/// Un titre dont le niveau saute (un `H3` juste après un `H1`) est rattaché
/// au dernier titre de niveau inférieur : mieux vaut un arbre légèrement
/// aplati qu'un signet perdu.
fn nest(headings: &[Heading]) -> Vec<OutlineNode> {
    let mut roots: Vec<OutlineNode> = Vec::new();
    // Pile des niveaux ouverts : (niveau, chemin dans l'arbre).
    let mut stack: Vec<(u8, Vec<usize>)> = Vec::new();
    for heading in headings {
        while stack
            .last()
            .is_some_and(|(level, _)| *level >= heading.level)
        {
            stack.pop();
        }
        let node = OutlineNode {
            title: heading.title.clone(),
            // Le premier niveau est déplié, le reste replié.
            open: stack.is_empty(),
            action: Some(Action::GoTo(heading.destination.clone())),
            ..OutlineNode::default()
        };
        let path = match stack.last() {
            None => {
                roots.push(node);
                vec![roots.len() - 1]
            }
            Some((_, parent)) => {
                let Some(siblings) = siblings_mut(&mut roots, parent) else {
                    continue;
                };
                siblings.push(node);
                let mut path = parent.clone();
                path.push(siblings.len() - 1);
                path
            }
        };
        if stack.len() < MAX_DEPTH {
            stack.push((heading.level, path));
        }
    }
    roots
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::docinfo::tests::doc_with;

    fn reload(doc: &Document) -> Document {
        let bytes = doc
            .save_incremental()
            .or_else(|_| doc.save_full())
            .expect("enregistrement");
        Document::from_bytes(bytes).expect("relecture")
    }

    /// Arbre à trois niveaux : 1 (ouvert) > 1.1 (fermé) > 1.1.1, puis 2.
    fn sample() -> Vec<OutlineNode> {
        vec![
            OutlineNode {
                title: "Partie I".into(),
                bold: true,
                open: true,
                color: Some([1.0, 0.0, 0.0]),
                action: Some(Action::GoTo(Destination {
                    page: 0,
                    view: View::Fit,
                })),
                children: vec![OutlineNode {
                    title: "Chapitre 1".into(),
                    italic: true,
                    open: false,
                    children: vec![
                        OutlineNode::to_page("Section 1.1", 1),
                        OutlineNode::to_page("Section 1.2", 2),
                    ],
                    ..OutlineNode::default()
                }],
                ..OutlineNode::default()
            },
            OutlineNode {
                title: "Partie II".into(),
                action: Some(Action::Uri("https://exemple.test/".into())),
                ..OutlineNode::default()
            },
        ]
    }

    /// Dictionnaire d'un signet atteint par son chemin, via /First et /Next.
    fn item_dict(doc: &Document, path: &[usize]) -> Dict {
        let catalog = doc.catalog().unwrap();
        let mut current = doc
            .dict_get(&catalog, "Outlines")
            .unwrap()
            .unwrap()
            .as_dict()
            .cloned()
            .unwrap();
        for step in path {
            let first = current.get(&Name::new("First")).cloned().unwrap();
            let mut node = doc.resolve(&first).unwrap().as_dict().cloned().unwrap();
            for _ in 0..*step {
                let next = node.get(&Name::new("Next")).cloned().unwrap();
                node = doc.resolve(&next).unwrap().as_dict().cloned().unwrap();
            }
            current = node;
        }
        current
    }

    fn integer(d: &Dict, key: &str) -> Option<i64> {
        d.get(&Name::new(key)).and_then(Object::as_i64)
    }

    #[test]
    fn a_three_level_tree_round_trips() {
        let doc = doc_with("", &[], "");
        set_outline(&doc, &sample()).unwrap();
        let saved = reload(&doc);
        assert_eq!(read_outline(&saved).unwrap(), sample());
    }

    #[test]
    fn chaining_and_signed_counts_follow_the_spec() {
        let doc = doc_with("", &[], "");
        set_outline(&doc, &sample()).unwrap();
        let saved = reload(&doc);
        let catalog = saved.catalog().unwrap();
        let root = saved
            .dict_get(&catalog, "Outlines")
            .unwrap()
            .unwrap()
            .as_dict()
            .cloned()
            .unwrap();
        // Visibles : Partie I, Chapitre 1 (Partie I est ouverte),
        // Partie II. Les sections sont sous un nœud fermé.
        assert_eq!(integer(&root, "Count"), Some(3));

        let one = item_dict(&saved, &[0]);
        assert_eq!(integer(&one, "Count"), Some(1));
        assert!(!one.contains_key(&Name::new("Prev")));
        assert!(one.contains_key(&Name::new("Next")));
        assert_eq!(integer(&one, "F"), Some(2)); // gras

        let chapter = item_dict(&saved, &[0, 0]);
        // Fermé : négatif, et |Count| compte les deux sections.
        assert_eq!(integer(&chapter, "Count"), Some(-2));
        assert_eq!(integer(&chapter, "F"), Some(1)); // italique

        let two = item_dict(&saved, &[1]);
        assert!(!two.contains_key(&Name::new("Next")));
        assert!(!two.contains_key(&Name::new("Count")));
        // /Prev de Partie II pointe bien sur Partie I, et /Next de Partie I
        // sur Partie II : le chaînage est cohérent dans les deux sens.
        let first = root.get(&Name::new("First")).unwrap();
        let last = root.get(&Name::new("Last")).unwrap();
        assert_eq!(two.get(&Name::new("Prev")), Some(first));
        assert_eq!(one.get(&Name::new("Next")), Some(last));
        // Chaque enfant désigne bien son parent.
        let section = item_dict(&saved, &[0, 0, 1]);
        assert_eq!(
            section.get(&Name::new("Parent")),
            one.get(&Name::new("First"))
        );
    }

    #[test]
    fn a_bookmark_can_be_added_anywhere() {
        let doc = doc_with("", &[], "");
        set_outline(&doc, &sample()).unwrap();
        add_bookmark(&doc, &[0, 0], 1, OutlineNode::to_page("Intercalée", 0)).unwrap();
        add_bookmark(&doc, &[], 99, OutlineNode::to_page("Fin", 2)).unwrap();
        let tree = read_outline(&reload(&doc)).unwrap();
        let titles: Vec<&str> = tree[0].children[0]
            .children
            .iter()
            .map(|n| n.title.as_str())
            .collect();
        assert_eq!(titles, ["Section 1.1", "Intercalée", "Section 1.2"]);
        assert_eq!(tree.len(), 3);
        assert_eq!(tree[2].title, "Fin");
    }

    #[test]
    fn removing_a_node_removes_its_children() {
        let doc = doc_with("", &[], "");
        set_outline(&doc, &sample()).unwrap();
        let removed = remove_bookmark(&doc, &[0, 0]).unwrap();
        assert_eq!(removed.title, "Chapitre 1");
        assert_eq!(removed.children.len(), 2);
        let saved = reload(&doc);
        let tree = read_outline(&saved).unwrap();
        assert_eq!(flatten(&tree).len(), 2);
        assert!(tree[0].children.is_empty());
        // Partie I n'a plus d'enfants : plus de /Count ni de /First.
        let one = item_dict(&saved, &[0]);
        assert!(!one.contains_key(&Name::new("Count")));
        assert!(!one.contains_key(&Name::new("First")));
    }

    #[test]
    fn a_bookmark_changes_parent_and_rank() {
        let doc = doc_with("", &[], "");
        set_outline(&doc, &sample()).unwrap();
        // Section 1.2 devient le premier signet de premier niveau.
        move_bookmark(&doc, &[0, 0, 1], &[], 0).unwrap();
        let tree = read_outline(&reload(&doc)).unwrap();
        assert_eq!(tree[0].title, "Section 1.2");
        assert_eq!(tree.len(), 3);
        assert_eq!(tree[1].children[0].children.len(), 1);
        assert_eq!(tree[1].children[0].children[0].title, "Section 1.1");
    }

    #[test]
    fn moving_within_the_same_parent_shifts_correctly() {
        let doc = doc_with("", &[], "");
        set_outline(&doc, &sample()).unwrap();
        // Section 1.1 passe après Section 1.2.
        move_bookmark(&doc, &[0, 0, 0], &[0, 0], 2).unwrap();
        let tree = read_outline(&reload(&doc)).unwrap();
        let titles: Vec<&str> = tree[0].children[0]
            .children
            .iter()
            .map(|n| n.title.as_str())
            .collect();
        assert_eq!(titles, ["Section 1.2", "Section 1.1"]);
    }

    #[test]
    fn a_bookmark_cannot_be_moved_into_itself() {
        let doc = doc_with("", &[], "");
        set_outline(&doc, &sample()).unwrap();
        assert!(move_bookmark(&doc, &[0], &[0, 0], 0).is_err());
        assert!(move_bookmark(&doc, &[0], &[0], 0).is_err());
        assert!(move_bookmark(&doc, &[7], &[], 0).is_err());
    }

    #[test]
    fn an_empty_tree_removes_the_entry() {
        let doc = doc_with("/PageMode /UseOutlines", &[], "");
        set_outline(&doc, &sample()).unwrap();
        set_outline(&doc, &[]).unwrap();
        let saved = reload(&doc);
        assert!(read_outline(&saved).unwrap().is_empty());
        let catalog = saved.catalog().unwrap();
        assert!(!catalog.contains_key(&Name::new("Outlines")));
        assert!(!catalog.contains_key(&Name::new("PageMode")));
    }

    #[test]
    fn levels_that_skip_are_still_nested() {
        let headings = [
            Heading {
                level: 1,
                title: "A".into(),
                destination: Destination {
                    page: 0,
                    view: View::Fit,
                },
            },
            Heading {
                level: 3,
                title: "A.1".into(),
                destination: Destination {
                    page: 0,
                    view: View::Fit,
                },
            },
            Heading {
                level: 2,
                title: "A.2".into(),
                destination: Destination {
                    page: 0,
                    view: View::Fit,
                },
            },
            Heading {
                level: 1,
                title: "B".into(),
                destination: Destination {
                    page: 1,
                    view: View::Fit,
                },
            },
        ];
        let tree = nest(&headings);
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].title, "A");
        assert!(tree[0].open);
        let children: Vec<&str> = tree[0].children.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(children, ["A.1", "A.2"]);
        assert!(!tree[0].children[0].open);
    }

    #[test]
    fn long_titles_are_cut_on_a_word() {
        let long = "mot ".repeat(60);
        let cut = clean_title(&long);
        assert!(cut.chars().count() <= MAX_TITLE + 1);
        assert!(cut.ends_with('…'));
        assert_eq!(clean_title("  deux   espaces  "), "deux espaces");
    }
}
