//! Navigation dans un document : destinations (§12.3.2), destinations
//! nommées (`/Dests` du catalogue et arbre de noms `/Names /Dests`,
//! §7.9.6), actions (§12.6 : `GoTo`, `URI`, `Named`, `Launch`…), liens des
//! pages (§12.5.6.5) et signets (`/Outlines`, §12.3.3).
//!
//! Tout est résolu en indices de page : le visualiseur ne manipule jamais
//! d'objets PDF pour naviguer.

use std::collections::{HashMap, HashSet};

use acrux_core::{Rect, Result};
use acrux_document::text::decode_text_string;
use acrux_document::{Dict, Document, Name, Object, ObjectRef, Page};

use crate::annotations::encode_text;

/// Façon d'afficher la page cible (§12.3.2.2, tableau 149).
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    /// `/XYZ left top zoom` (chaque valeur peut être absente : conserver).
    Xyz {
        /// Abscisse à placer au bord gauche.
        left: Option<f64>,
        /// Ordonnée à placer en haut.
        top: Option<f64>,
        /// Facteur de zoom (0 ou absent = inchangé).
        zoom: Option<f64>,
    },
    /// `/Fit` : page entière.
    Fit,
    /// `/FitH top` ou `/FitBH top` : largeur.
    FitWidth {
        /// Ordonnée à placer en haut.
        top: Option<f64>,
    },
    /// `/FitV left` ou `/FitBV left` : hauteur.
    FitHeight {
        /// Abscisse à placer à gauche.
        left: Option<f64>,
    },
    /// `/FitR` : rectangle.
    FitRect(Rect),
    /// `/FitB` : boîte englobante du contenu.
    FitBox,
}

/// Destination résolue.
#[derive(Debug, Clone, PartialEq)]
pub struct Destination {
    /// Indice de page (0 = première).
    pub page: usize,
    /// Cadrage.
    pub view: View,
}

/// Action d'un lien ou d'un signet.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Aller à une page du document.
    GoTo(Destination),
    /// Ouvrir une adresse.
    Uri(String),
    /// Action prédéfinie (`NextPage`, `PrevPage`, `FirstPage`, `LastPage`…).
    Named(String),
    /// Lancer une application ou un fichier (jamais exécuté automatiquement).
    Launch(String),
    /// Aller dans un autre document (`GoToR`) : chemin du fichier.
    GoToRemote(String),
    /// Type d'action non pris en charge (`/S`).
    Unsupported(String),
}

/// Lien cliquable d'une page.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    /// Position dans `/Annots`.
    pub annotation_index: usize,
    /// Zone active, espace utilisateur de la page.
    pub rect: Rect,
    /// Action.
    pub action: Action,
}

/// Signet (élément de `/Outlines`).
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineItem {
    /// Titre décodé.
    pub title: String,
    /// Action (destination le plus souvent), si elle a pu être résolue.
    pub action: Option<Action>,
    /// Ouvert par défaut (`/Count` positif).
    pub open: bool,
    /// Enfants.
    pub children: Vec<OutlineItem>,
}

/// Table « objet page → indice », construite une fois par document.
#[derive(Debug, Clone, Default)]
pub struct PageIndex {
    by_number: HashMap<u32, usize>,
}

impl PageIndex {
    /// Construit la table depuis la liste des pages.
    #[must_use]
    pub fn new(pages: &[Page]) -> Self {
        let by_number = pages
            .iter()
            .filter_map(|p| p.reference.map(|r| (r.number, p.index)))
            .collect();
        Self { by_number }
    }

    /// Indice d'une page désignée par référence.
    #[must_use]
    pub fn index_of(&self, r: ObjectRef) -> Option<usize> {
        self.by_number.get(&r.number).copied()
    }
}

/// Profondeur maximale des arbres parcourus (noms, signets).
const MAX_DEPTH: usize = 64;

fn number(doc: &Document, o: &Object) -> Option<f64> {
    doc.resolve(o).ok().and_then(|r| r.as_f64())
}

fn text(doc: &Document, d: &Dict, key: &str) -> Option<String> {
    match doc.dict_get(d, key).ok().flatten() {
        Some(r) => match &*r {
            Object::String(s) => Some(decode_text_string(s)),
            Object::Name(n) => Some(n.as_str()),
            _ => None,
        },
        None => None,
    }
}

/// Résout une destination explicite (tableau) ou nommée (nom / chaîne).
#[must_use]
pub fn resolve_destination(doc: &Document, obj: &Object, pages: &PageIndex) -> Option<Destination> {
    resolve_destination_depth(doc, obj, pages, 0)
}

fn resolve_destination_depth(
    doc: &Document,
    obj: &Object,
    pages: &PageIndex,
    depth: usize,
) -> Option<Destination> {
    if depth > 4 {
        return None;
    }
    let resolved = doc.resolve(obj).ok()?;
    match &*resolved {
        Object::Array(a) => explicit_destination(doc, a, pages),
        // Destination nommée : dictionnaire /Dests (PDF 1.1) ou arbre /Names /Dests.
        Object::Name(n) => {
            let target = named_destination(doc, &n.0)?;
            resolve_destination_depth(doc, &target, pages, depth + 1)
        }
        Object::String(s) => {
            let target = named_destination(doc, s)?;
            resolve_destination_depth(doc, &target, pages, depth + 1)
        }
        // Un dictionnaire avec /D (valeur d'un arbre de noms).
        Object::Dict(d) => {
            let inner = d.get(&Name::new("D"))?.clone();
            resolve_destination_depth(doc, &inner, pages, depth + 1)
        }
        _ => None,
    }
}

fn explicit_destination(doc: &Document, a: &[Object], pages: &PageIndex) -> Option<Destination> {
    let first = a.first()?;
    let page = match first {
        Object::Reference(r) => pages.index_of(*r)?,
        // Entier : numéro de page (destinations distantes, tolérées ici).
        Object::Integer(i) => usize::try_from(*i).ok()?,
        _ => return None,
    };
    let kind = a
        .get(1)
        .and_then(Object::as_name)
        .map_or_else(|| "Fit".to_string(), Name::as_str);
    let num = |i: usize| a.get(i).and_then(|o| number(doc, o));
    let view = match kind.as_str() {
        "XYZ" => View::Xyz {
            left: num(2),
            top: num(3),
            zoom: num(4).filter(|z| *z > 0.0),
        },
        "FitH" | "FitBH" => View::FitWidth { top: num(2) },
        "FitV" | "FitBV" => View::FitHeight { left: num(2) },
        "FitR" => match (num(2), num(3), num(4), num(5)) {
            (Some(x0), Some(y0), Some(x1), Some(y1)) => View::FitRect(Rect::new(x0, y0, x1, y1)),
            _ => View::Fit,
        },
        "FitB" => View::FitBox,
        _ => View::Fit,
    };
    Some(Destination { page, view })
}

/// La destination explicite (`[page /XYZ …]`, telle qu'écrite dans `doc`)
/// que désigne une destination **nommée** ; `None` pour une destination déjà
/// explicite, ou un nom que le document ne connaît pas.
///
/// C'est ce qui permet de copier un lien d'un document dans un autre : son
/// nom ne voudrait plus rien dire dans le document d'arrivée, dont l'arbre
/// `/Names /Dests` est un autre — ou désignerait, dans une fusion d'un
/// document avec lui-même, la page de la première copie.
#[must_use]
pub fn named_to_explicit(doc: &Document, dest: &Object) -> Option<Object> {
    let resolved = doc.resolve(dest).ok()?;
    let key = match &*resolved {
        Object::Name(n) => n.0.clone(),
        Object::String(s) => s.clone(),
        _ => return None,
    };
    let mut target = named_destination(doc, &key)?;
    // Un nom mène à un tableau, ou à un dictionnaire dont `/D` est le
    // tableau (§12.3.2.4) ; quelques détours au plus.
    for _ in 0..4 {
        let next = {
            let value = doc.resolve(&target).ok()?;
            match &*value {
                Object::Array(_) => return Some((*value).clone()),
                Object::Dict(d) => d.get(&Name::new("D"))?.clone(),
                _ => return None,
            }
        };
        target = next;
    }
    None
}

/// Valeur d'une destination nommée : `/Dests` du catalogue, puis l'arbre
/// de noms `/Names /Dests`.
fn named_destination(doc: &Document, key: &[u8]) -> Option<Object> {
    let catalog = doc.catalog().ok()?;
    if let Some(dests) = doc.dict_get(&catalog, "Dests").ok().flatten() {
        if let Some(d) = dests.as_dict() {
            if let Some(v) = d.get(&Name(key.to_vec())) {
                return Some(v.clone());
            }
        }
    }
    let names = doc.dict_get(&catalog, "Names").ok().flatten()?;
    let names = names.as_dict()?;
    let tree = doc.dict_get(names, "Dests").ok().flatten()?;
    let tree = tree.as_dict()?;
    let mut visited = HashSet::new();
    name_tree_lookup(doc, tree, key, &mut visited, 0)
}

/// Recherche dans un arbre de noms (§7.9.6) : feuilles `/Names [clé valeur …]`,
/// nœuds `/Kids`, avec `/Limits` pour élaguer.
fn name_tree_lookup(
    doc: &Document,
    node: &Dict,
    key: &[u8],
    visited: &mut HashSet<usize>,
    depth: usize,
) -> Option<Object> {
    if depth > MAX_DEPTH {
        return None;
    }
    if let Some(limits) = doc.dict_get(node, "Limits").ok().flatten() {
        if let Some(l) = limits.as_array() {
            if let (Some(Object::String(lo)), Some(Object::String(hi))) = (l.first(), l.get(1)) {
                if key < lo.as_slice() || key > hi.as_slice() {
                    return None;
                }
            }
        }
    }
    if let Some(names) = doc.dict_get(node, "Names").ok().flatten() {
        if let Some(pairs) = names.as_array() {
            for pair in pairs.chunks_exact(2) {
                let k = doc.resolve(&pair[0]).ok()?;
                if let Object::String(s) = &*k {
                    if s.as_slice() == key {
                        return Some(pair[1].clone());
                    }
                }
            }
        }
    }
    if let Some(kids) = doc.dict_get(node, "Kids").ok().flatten() {
        if let Some(list) = kids.as_array() {
            for kid in list {
                if let Object::Reference(r) = kid {
                    if !visited.insert(r.number as usize) {
                        continue;
                    }
                }
                let kid = doc.resolve(kid).ok()?;
                if let Some(d) = kid.as_dict() {
                    if let Some(v) = name_tree_lookup(doc, d, key, visited, depth + 1) {
                        return Some(v);
                    }
                }
            }
        }
    }
    None
}

/// Action d'un dictionnaire portant `/Dest` ou `/A` (lien, signet).
#[must_use]
pub fn action_of(doc: &Document, d: &Dict, pages: &PageIndex) -> Option<Action> {
    if let Some(dest) = d.get(&Name::new("Dest")) {
        return resolve_destination(doc, dest, pages).map(Action::GoTo);
    }
    let a = doc.dict_get(d, "A").ok().flatten()?;
    let a = a.as_dict()?;
    let kind = a.get(&Name::new("S")).and_then(Object::as_name)?.as_str();
    Some(match kind.as_str() {
        "GoTo" => {
            let dest = a.get(&Name::new("D"))?;
            Action::GoTo(resolve_destination(doc, dest, pages)?)
        }
        "URI" => Action::Uri(text(doc, a, "URI")?),
        "Named" => Action::Named(text(doc, a, "N")?),
        "Launch" => Action::Launch(file_spec(doc, a).unwrap_or_default()),
        "GoToR" => Action::GoToRemote(file_spec(doc, a).unwrap_or_default()),
        other => Action::Unsupported(other.to_string()),
    })
}

/// `/F` : chaîne ou dictionnaire de spécification de fichier (§7.11).
fn file_spec(doc: &Document, a: &Dict) -> Option<String> {
    let f = doc.dict_get(a, "F").ok().flatten()?;
    match &*f {
        Object::String(s) => Some(decode_text_string(s)),
        Object::Dict(d) => text(doc, d, "UF").or_else(|| text(doc, d, "F")),
        _ => None,
    }
}

/// Liens d'une page, dans l'ordre de `/Annots`.
///
/// # Errors
/// Tableau `/Annots` illisible.
pub fn page_links(doc: &Document, page: &Page, pages: &PageIndex) -> Result<Vec<Link>> {
    let mut out = Vec::new();
    let Some(annots) = page.dict.get(&Name::new("Annots")) else {
        return Ok(out);
    };
    let annots = doc.resolve(annots)?;
    let Some(list) = annots.as_array() else {
        return Ok(out);
    };
    for (annotation_index, item) in list.iter().enumerate() {
        let Ok(resolved) = doc.resolve(item) else {
            continue;
        };
        let Some(d) = resolved.as_dict() else {
            continue;
        };
        let is_link = d
            .get(&Name::new("Subtype"))
            .and_then(Object::as_name)
            .is_some_and(|n| n.0 == b"Link");
        if !is_link {
            continue;
        }
        let rect: Vec<f64> = doc
            .dict_get(d, "Rect")
            .ok()
            .flatten()
            .and_then(|o| {
                o.as_array()
                    .map(|a| a.iter().filter_map(|v| number(doc, v)).collect())
            })
            .unwrap_or_default();
        if rect.len() != 4 {
            continue;
        }
        let Some(action) = action_of(doc, d, pages) else {
            continue;
        };
        out.push(Link {
            annotation_index,
            rect: Rect::new(rect[0], rect[1], rect[2], rect[3]),
            action,
        });
    }
    Ok(out)
}

/// Signets du document (arbre `/Outlines`), vide s'il n'y en a pas.
///
/// # Errors
/// Catalogue illisible.
pub fn outline(doc: &Document, pages: &PageIndex) -> Result<Vec<OutlineItem>> {
    let catalog = doc.catalog()?;
    let Some(root) = doc.dict_get(&catalog, "Outlines")? else {
        return Ok(Vec::new());
    };
    let Some(root) = root.as_dict() else {
        return Ok(Vec::new());
    };
    let mut visited = HashSet::new();
    Ok(outline_children(doc, root, pages, &mut visited, 0))
}

fn outline_children(
    doc: &Document,
    parent: &Dict,
    pages: &PageIndex,
    visited: &mut HashSet<u32>,
    depth: usize,
) -> Vec<OutlineItem> {
    let mut out = Vec::new();
    if depth > MAX_DEPTH {
        return out;
    }
    let mut next = parent.get(&Name::new("First")).cloned();
    while let Some(obj) = next {
        if let Object::Reference(r) = obj {
            if !visited.insert(r.number) {
                break;
            }
        }
        let Ok(item) = doc.resolve(&obj) else { break };
        let Some(d) = item.as_dict() else { break };
        let count = doc
            .dict_get(d, "Count")
            .ok()
            .flatten()
            .and_then(|c| c.as_i64())
            .unwrap_or(0);
        out.push(OutlineItem {
            title: text(doc, d, "Title").unwrap_or_default(),
            action: action_of(doc, d, pages),
            open: count > 0,
            children: outline_children(doc, d, pages, visited, depth + 1),
        });
        next = d.get(&Name::new("Next")).cloned();
    }
    out
}

// ---------------------------------------------------------------------------
// Écriture : destinations et actions
// ---------------------------------------------------------------------------

/// Construit le tableau d'une destination explicite (§12.3.2.2).
///
/// La page est désignée par sa **référence indirecte**, comme l'exige la
/// norme pour une destination interne ; une page directe (rare, mais des
/// générateurs en produisent) n'est pas adressable et rend `None`.
#[must_use]
pub fn destination_object(pages: &[Page], destination: &Destination) -> Option<Object> {
    let page = pages.get(destination.page)?.reference?;
    let mut a = vec![Object::Reference(page)];
    let num = |v: f64| Object::Real(v);
    match &destination.view {
        View::Xyz { left, top, zoom } => {
            a.push(Object::Name(Name::new("XYZ")));
            a.push(left.map_or(Object::Null, num));
            a.push(top.map_or(Object::Null, num));
            // Un zoom nul ou absent veut dire « garder celui en cours ».
            a.push(zoom.filter(|z| *z > 0.0).map_or(Object::Null, num));
        }
        View::Fit => a.push(Object::Name(Name::new("Fit"))),
        View::FitWidth { top } => {
            a.push(Object::Name(Name::new("FitH")));
            a.push(top.map_or(Object::Null, num));
        }
        View::FitHeight { left } => {
            a.push(Object::Name(Name::new("FitV")));
            a.push(left.map_or(Object::Null, num));
        }
        View::FitRect(r) => {
            a.push(Object::Name(Name::new("FitR")));
            a.extend([num(r.x0), num(r.y0), num(r.x1), num(r.y1)]);
        }
        View::FitBox => a.push(Object::Name(Name::new("FitB"))),
    }
    Some(Object::Array(a))
}

/// Construit le dictionnaire d'action correspondant (§12.6).
///
/// [`Action::Unsupported`] rend `None` : on ne réécrit pas une action dont on
/// n'a pas su lire le contenu, ce serait la remplacer par une coquille vide.
#[must_use]
pub fn action_object(pages: &[Page], action: &Action) -> Option<Object> {
    let mut d = Dict::new();
    let kind = |d: &mut Dict, name: &str| d.insert(Name::new("S"), Object::Name(Name::new(name)));
    match action {
        Action::GoTo(destination) => {
            kind(&mut d, "GoTo");
            d.insert(Name::new("D"), destination_object(pages, destination)?);
        }
        Action::Uri(uri) => {
            kind(&mut d, "URI");
            // §12.6.4.7 : l'URI est une chaîne d'octets codée en ASCII sept bits.
            d.insert(Name::new("URI"), Object::String(uri.as_bytes().to_vec()));
        }
        Action::Named(name) => {
            kind(&mut d, "Named");
            d.insert(Name::new("N"), Object::Name(Name::new(name)));
        }
        Action::Launch(file) => {
            kind(&mut d, "Launch");
            d.insert(Name::new("F"), Object::String(encode_text(file)));
        }
        Action::GoToRemote(file) => {
            kind(&mut d, "GoToR");
            d.insert(Name::new("F"), Object::String(encode_text(file)));
            // Sans /D, un lecteur ouvre le document distant à sa première page.
            d.insert(
                Name::new("D"),
                Object::Array(vec![Object::Integer(0), Object::Name(Name::new("Fit"))]),
            );
        }
        Action::Unsupported(_) => return None,
    }
    Some(Object::Dict(d))
}

/// Aplatit les signets : (profondeur, élément), en ordre d'affichage.
#[must_use]
pub fn flatten_outline(items: &[OutlineItem]) -> Vec<(usize, &OutlineItem)> {
    fn walk<'a>(items: &'a [OutlineItem], depth: usize, out: &mut Vec<(usize, &'a OutlineItem)>) {
        for it in items {
            out.push((depth, it));
            walk(&it.children, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    walk(items, 0, &mut out);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)] // tests
mod tests {
    use super::*;
    use acrux_document::collect_pages;

    /// PDF minimal : 2 pages, un lien explicite, un lien nommé via l'arbre
    /// de noms, un lien URI, des signets imbriqués.
    fn pdf() -> Vec<u8> {
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R /Outlines 8 0 R /Names << /Dests 11 0 R >> /Dests << /ancien [4 0 R /Fit] >> >>",
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Annots [5 0 R 6 0 R 7 0 R 13 0 R] >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] >>",
            "<< /Type /Annot /Subtype /Link /Rect [10 10 50 20] /Dest [4 0 R /XYZ 5 95 2] >>",
            "<< /Type /Annot /Subtype /Link /Rect [60 10 90 20] /A << /S /GoTo /D (chap2) >> >>",
            "<< /Type /Annot /Subtype /Link /Rect [100 10 150 20] /A << /S /URI /URI (https://example.org) >> >>",
            "<< /Type /Outlines /First 9 0 R /Last 9 0 R /Count 2 >>",
            "<< /Title (Chapitre 1) /Parent 8 0 R /First 10 0 R /Last 10 0 R /Count 1 /Dest [3 0 R /Fit] /Next 9 0 R >>",
            "<< /Title <FEFF00C9007400E9> /Parent 9 0 R /A << /S /GoTo /D /ancien >> >>",
            "<< /Kids [12 0 R] >>",
            "<< /Limits [(a) (z)] /Names [(chap2) << /D [4 0 R /FitH 80] >>] >>",
            "<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /A << /S /Named /N /NextPage >> >>",
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes(),
        );
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objs.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn links_explicit_named_uri_and_named_action() {
        let doc = Document::from_bytes(pdf()).unwrap();
        let pages = collect_pages(&doc).unwrap();
        let index = PageIndex::new(&pages);
        let links = page_links(&doc, &pages[0], &index).unwrap();
        assert_eq!(links.len(), 4);
        assert_eq!(
            links[0].action,
            Action::GoTo(Destination {
                page: 1,
                view: View::Xyz {
                    left: Some(5.0),
                    top: Some(95.0),
                    zoom: Some(2.0)
                }
            })
        );
        assert_eq!(links[0].rect, Rect::new(10.0, 10.0, 50.0, 20.0));
        assert_eq!(
            links[1].action,
            Action::GoTo(Destination {
                page: 1,
                view: View::FitWidth { top: Some(80.0) }
            })
        );
        assert_eq!(links[2].action, Action::Uri("https://example.org".into()));
        assert_eq!(links[3].action, Action::Named("NextPage".into()));
        assert!(page_links(&doc, &pages[1], &index).unwrap().is_empty());
    }

    #[test]
    fn outline_tree_with_old_style_named_destination() {
        let doc = Document::from_bytes(pdf()).unwrap();
        let pages = collect_pages(&doc).unwrap();
        let index = PageIndex::new(&pages);
        let items = outline(&doc, &index).unwrap();
        // Le /Next auto-référent de l'item 9 est coupé par le garde-fou.
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Chapitre 1");
        assert!(items[0].open);
        assert_eq!(
            items[0].action,
            Some(Action::GoTo(Destination {
                page: 0,
                view: View::Fit
            }))
        );
        assert_eq!(items[0].children.len(), 1);
        assert_eq!(items[0].children[0].title, "Été");
        assert_eq!(
            items[0].children[0].action,
            Some(Action::GoTo(Destination {
                page: 1,
                view: View::Fit
            }))
        );
        let flat = flatten_outline(&items);
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[1].0, 1);
    }

    #[test]
    fn unknown_named_destination_is_none() {
        let doc = Document::from_bytes(pdf()).unwrap();
        let pages = collect_pages(&doc).unwrap();
        let index = PageIndex::new(&pages);
        assert!(resolve_destination(&doc, &Object::String(b"nulle".to_vec()), &index).is_none());
    }
}
