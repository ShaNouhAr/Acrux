//! Organisation des pages (voir FONCTIONNALITES_ADOBE_ACROBAT.txt §5) :
//! rotation, suppression, extraction, insertion, remplacement, pages
//! vierges, réordonnancement, duplication, fusion ; le fractionnement d'un
//! document en plusieurs fichiers est dans [`split`].
//!
//! Toutes les opérations travaillent sur l'arbre `/Pages` du catalogue
//! (ISO 32000-2 §7.7.3). Pour rester simples et robustes, elles
//! reconstruisent un arbre plat (un seul nœud `/Pages` racine) : les pages
//! elles-mêmes ne sont pas réécrites, sauf leur `/Parent` et les attributs
//! hérités qui sont rapatriés sur chaque page pour ne rien perdre.
//!
//! # Les références d'une page à une autre
//!
//! Une page en désigne d'autres : un lien `/Dest [12 0 R /Fit]`, le `/P`
//! d'une annotation, un signet. Copier une page d'un document à l'autre en
//! suivant **toutes** les références embarquerait donc, par un simple lien
//! « voir page 40 », la page 40 entière — contenus et images compris —
//! comme objet orphelin : un extrait d'une page pesait autant que le
//! document. L'[`Importer`] sait donc d'avance où vont les pages copiées
//! ([`Importer::map_ref`]) et quelles pages restent derrière
//! ([`Importer::drop_ref`]) : un lien vers une page copiée vise sa copie,
//! un lien vers une page laissée de côté devient `null` (le lien reste, sans
//! destination, comme le fait Acrobat).

pub mod split;

use std::collections::{HashMap, HashSet};

use acrux_core::{Error, Rect, Result};
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef, Page};

/// Attributs héritables à rapatrier sur les pages avant de réécrire l'arbre.
const INHERITABLE: [&str; 4] = ["Resources", "MediaBox", "CropBox", "Rotate"];

/// Ce qui fait le **contenu** d'une page, par opposition à ce qui s'y pose
/// (annotations) et à ce qui la désigne (signets, liens, étiquettes) :
/// c'est ce que [`replace_pages`] remplace.
const CONTENT_KEYS: [&str; 15] = [
    "Contents",
    "Resources",
    "MediaBox",
    "CropBox",
    "BleedBox",
    "TrimBox",
    "ArtBox",
    "Rotate",
    "UserUnit",
    "Group",
    "Thumb",
    "VP",
    "PieceInfo",
    "SeparationInfo",
    "OutputIntents",
];

/// Pages actuelles avec leurs références (les pages directes, sans référence,
/// sont rendues indirectes).
fn page_refs(doc: &Document) -> Result<Vec<(ObjectRef, Page)>> {
    let pages = collect_pages(doc)?;
    let mut out = Vec::with_capacity(pages.len());
    for p in pages {
        let r = match p.reference {
            Some(r) => r,
            None => doc.add(Object::Dict(p.dict.clone())),
        };
        out.push((r, p));
    }
    Ok(out)
}

/// Réécrit l'arbre des pages avec, dans l'ordre, les pages données. Chaque
/// page reçoit ses attributs hérités et un `/Parent` vers la nouvelle racine.
fn rebuild_tree(doc: &Document, pages: &[(ObjectRef, Page)]) -> Result<()> {
    let catalog = doc.catalog()?;
    let root_ref = match catalog.get(&Name::new("Pages")) {
        Some(Object::Reference(r)) => *r,
        _ => doc.allocate(),
    };
    let mut kids = Vec::with_capacity(pages.len());
    for (r, p) in pages {
        let mut d = p.dict.clone();
        for key in INHERITABLE {
            if let Some(v) = p.dict.get(&Name::new(key)) {
                d.insert(Name::new(key), v.clone());
            }
        }
        d.insert(Name::new("Type"), Object::Name(Name::new("Page")));
        d.insert(Name::new("Parent"), Object::Reference(root_ref));
        doc.set(*r, Object::Dict(d));
        kids.push(Object::Reference(*r));
    }
    let mut root = Dict::new();
    root.insert(Name::new("Type"), Object::Name(Name::new("Pages")));
    root.insert(Name::new("Kids"), Object::Array(kids));
    root.insert(
        Name::new("Count"),
        Object::Integer(i64::try_from(pages.len()).unwrap_or(0)),
    );
    doc.set(root_ref, Object::Dict(root));
    let mut catalog = catalog;
    catalog.insert(Name::new("Pages"), Object::Reference(root_ref));
    let cat_ref = acrux_document::xref::trailer_ref(&doc.trailer(), "Root")
        .ok_or_else(|| Error::Corrupt("trailer sans /Root".into()))?;
    doc.set(cat_ref, Object::Dict(catalog));
    Ok(())
}

/// Fait pivoter les pages d'index donnés (0 = première) de `degrees`
/// (multiple de 90, positif = sens horaire), en cumulant avec `/Rotate`.
///
/// # Errors
/// Arbre des pages illisible ou angle invalide.
pub fn rotate_pages(doc: &Document, indices: &[usize], degrees: i32) -> Result<()> {
    if degrees % 90 != 0 {
        return Err(Error::Corrupt("l'angle doit être un multiple de 90".into()));
    }
    let pages = page_refs(doc)?;
    for &i in indices {
        let Some((r, p)) = pages.get(i) else {
            return Err(Error::Corrupt(format!("page {} inexistante", i + 1)));
        };
        let current = p.rotate(doc);
        let mut d = p.dict.clone();
        d.insert(
            Name::new("Rotate"),
            Object::Integer(i64::from((current + degrees).rem_euclid(360))),
        );
        doc.set(*r, Object::Dict(d));
    }
    Ok(())
}

/// Supprime les pages d'index donnés.
///
/// # Errors
/// Index invalide, ou tentative de supprimer toutes les pages.
pub fn delete_pages(doc: &Document, indices: &[usize]) -> Result<()> {
    let pages = page_refs(doc)?;
    for &i in indices {
        if i >= pages.len() {
            return Err(Error::Corrupt(format!("page {} inexistante", i + 1)));
        }
    }
    let kept: Vec<(ObjectRef, Page)> = pages
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !indices.contains(i))
        .map(|(_, p)| p)
        .collect();
    if kept.is_empty() {
        return Err(Error::Corrupt(
            "un document doit garder au moins une page".into(),
        ));
    }
    rebuild_tree(doc, &kept)
}

/// Réordonne les pages : `order` donne, pour chaque position, l'index actuel.
/// Une page absente de `order` est supprimée ; un index répété duplique la
/// page. La copie partage les contenus et les ressources de l'original, mais
/// a ses propres annotations (voir [`duplicate_page_dict`]).
///
/// # Errors
/// Index invalide ou ordre vide.
pub fn reorder_pages(doc: &Document, order: &[usize]) -> Result<()> {
    let pages = page_refs(doc)?;
    if order.is_empty() {
        return Err(Error::Corrupt("ordre vide".into()));
    }
    let mut seen = HashSet::new();
    let mut new_pages = Vec::with_capacity(order.len());
    for &i in order {
        let Some((r, p)) = pages.get(i) else {
            return Err(Error::Corrupt(format!("page {} inexistante", i + 1)));
        };
        if seen.insert(*r) {
            new_pages.push((*r, p.clone()));
        } else {
            // Duplication : un objet distinct pour garder un /Parent unique,
            // et des annotations à elle.
            let copy = doc.allocate();
            let dict = duplicate_page_dict(doc, p, copy)?;
            new_pages.push((
                copy,
                Page {
                    index: 0,
                    reference: Some(copy),
                    dict,
                },
            ));
        }
    }
    rebuild_tree(doc, &new_pages)
}

/// Dictionnaire d'une copie de la page `p`, qui sera rangée sous `new_ref`.
///
/// La copie partage les contenus et les ressources de l'original (on ne les
/// modifie jamais en place : une édition écrit un nouveau flux), mais pas
/// ses annotations. Chacune est recopiée dans un objet neuf dont le `/P`
/// désigne la copie : sans cela, un même commentaire vivrait sur deux pages,
/// et le déplacer sur l'une le déplacerait sur l'autre. Les liens entre
/// annotations de la page — une fenêtre `/Popup` et son `/Parent`, une
/// réponse `/IRT` — sont reportés d'une copie à l'autre. Les widgets de
/// formulaire ne sont pas copiés : un champ ne vit pas sur deux pages sans
/// être renommé, et c'est un choix que l'utilisateur doit faire (Acrobat
/// renomme). La copie perd aussi ses `/StructParents` : ils désigneraient
/// dans l'arbre de structure le contenu balisé de l'original.
///
/// Les copies sont écrites dans `doc` ; le dictionnaire de la page, lui,
/// est rendu à l'appelant, qui le range sous `new_ref`.
///
/// # Errors
/// Annotation illisible.
pub fn duplicate_page_dict(doc: &Document, p: &Page, new_ref: ObjectRef) -> Result<Dict> {
    let mut dict = p.dict.clone();
    dict.remove(&Name::new("StructParents"));
    let annots: Vec<Object> = doc
        .dict_get(&p.dict, "Annots")?
        .and_then(|a| a.as_array().map(<[Object]>::to_vec))
        .unwrap_or_default();
    if annots.is_empty() {
        return Ok(dict);
    }
    // Première passe : une copie par annotation gardée, et la table des
    // anciennes références vers les nouvelles.
    let mut renamed: HashMap<u32, ObjectRef> = HashMap::new();
    let mut copies: Vec<(ObjectRef, Dict)> = Vec::new();
    for entry in &annots {
        let resolved = doc.resolve(entry)?;
        let Some(a) = resolved.as_dict() else {
            continue;
        };
        let widget = a
            .get(&Name::new("Subtype"))
            .and_then(Object::as_name)
            .is_some_and(|n| n.0 == b"Widget");
        if widget {
            continue;
        }
        let mut copy = a.clone();
        copy.insert(Name::new("P"), Object::Reference(new_ref));
        copy.remove(&Name::new("StructParent"));
        let r = doc.allocate();
        if let Object::Reference(old) = entry {
            renamed.insert(old.number, r);
        }
        copies.push((r, copy));
    }
    // Deuxième passe : les liens entre annotations de la page suivent leurs
    // copies.
    let mut list = Vec::with_capacity(copies.len());
    for (r, mut copy) in copies {
        for key in ["Popup", "Parent", "IRT"] {
            let moved = match copy.get(&Name::new(key)) {
                Some(Object::Reference(old)) => renamed.get(&old.number).copied(),
                _ => None,
            };
            if let Some(n) = moved {
                copy.insert(Name::new(key), Object::Reference(n));
            }
        }
        doc.set(r, Object::Dict(copy));
        list.push(Object::Reference(r));
    }
    if list.is_empty() {
        dict.remove(&Name::new("Annots"));
    } else {
        dict.insert(Name::new("Annots"), Object::Array(list));
    }
    Ok(dict)
}

/// Copie profonde d'un objet d'un document vers un autre : les références
/// sont suivies et réallouées, avec mémorisation pour partager les ressources
/// communes (polices, images) et gérer les cycles.
///
/// Les références aux **pages** se règlent à part : voir [`Importer::map_ref`]
/// et [`Importer::drop_ref`], et l'explication en tête du module.
pub struct Importer<'a> {
    src: &'a Document,
    dst: &'a Document,
    map: HashMap<u32, ObjectRef>,
    /// Objets sources à ne pas copier : une référence vers eux devient
    /// `null`.
    dropped: HashSet<u32>,
}

impl<'a> Importer<'a> {
    /// Nouvel importeur de `src` vers `dst`.
    #[must_use]
    pub fn new(src: &'a Document, dst: &'a Document) -> Self {
        Self {
            src,
            dst,
            map: HashMap::new(),
            dropped: HashSet::new(),
        }
    }

    /// Annonce que l'objet source `src` a déjà sa place dans `dst`, sous
    /// `dst_ref` : toute référence vers lui y mènera, sans le recopier.
    /// C'est ainsi qu'un lien d'une page copiée vers une autre page copiée
    /// vise la copie.
    pub fn map_ref(&mut self, src: ObjectRef, dst_ref: ObjectRef) {
        self.map.insert(src.number, dst_ref);
    }

    /// Écarte l'objet source `src` : toute référence vers lui devient
    /// `null` au lieu de le copier. C'est le sort des pages laissées de côté.
    pub fn drop_ref(&mut self, src: ObjectRef) {
        self.dropped.insert(src.number);
    }

    /// Importe un objet (les références indirectes sont copiées récursivement).
    ///
    /// # Errors
    /// Objet source illisible.
    pub fn import(&mut self, obj: &Object) -> Result<Object> {
        self.import_depth(obj, 0)
    }

    fn import_depth(&mut self, obj: &Object, depth: usize) -> Result<Object> {
        if depth > 256 {
            return Err(Error::Corrupt("objets imbriqués trop profondément".into()));
        }
        Ok(match obj {
            Object::Reference(r) => {
                if self.dropped.contains(&r.number) {
                    return Ok(Object::Null);
                }
                if let Some(m) = self.map.get(&r.number) {
                    return Ok(Object::Reference(*m));
                }
                let new_ref = self.dst.allocate();
                self.map.insert(r.number, new_ref);
                let target = self.src.get(*r)?;
                let copied = self.import_depth(&target, depth + 1)?;
                self.dst.set(new_ref, copied);
                Object::Reference(new_ref)
            }
            Object::Array(a) => Object::Array(
                a.iter()
                    .map(|o| self.import_depth(o, depth + 1))
                    .collect::<Result<Vec<_>>>()?,
            ),
            Object::Dict(d) => Object::Dict(self.import_dict(d, depth)?),
            Object::Stream { dict, raw } => {
                // Le flux est copié décodé de tout chiffrement mais avec ses filtres :
                // `raw` est déjà déchiffré par `Document::get`.
                Object::Stream {
                    dict: self.import_dict(dict, depth)?,
                    raw: raw.clone(),
                }
            }
            other => other.clone(),
        })
    }

    fn import_dict(&mut self, d: &Dict, depth: usize) -> Result<Dict> {
        let mut out = Dict::new();
        for (k, v) in d {
            // `/Parent` des pages est réécrit par `rebuild_tree` ; on l'ignore pour
            // ne pas importer tout l'arbre source.
            if k.0 == b"Parent" {
                continue;
            }
            out.insert(k.clone(), self.import_depth(v, depth + 1)?);
        }
        Ok(out)
    }
}

/// Réserve dans `dst` la place des pages `wanted` de `src_pages`, et écarte
/// toutes les autres (voir l'explication en tête du module).
///
/// Rend, pour chaque entrée de `wanted`, la référence réservée à sa
/// **première** occurrence, `None` pour une répétition : une page demandée
/// deux fois devient une copie, avec ses propres annotations.
fn prepare_page_refs(
    importer: &mut Importer<'_>,
    src_pages: &[Page],
    wanted: &[usize],
    dst: &Document,
) -> Vec<Option<ObjectRef>> {
    let mut reserved: HashSet<usize> = HashSet::new();
    let mut out = Vec::with_capacity(wanted.len());
    for &i in wanted {
        if !reserved.insert(i) {
            out.push(None);
            continue;
        }
        let r = dst.allocate();
        if let Some(src_ref) = src_pages.get(i).and_then(|p| p.reference) {
            importer.map_ref(src_ref, r);
        }
        out.push(Some(r));
    }
    for (i, p) in src_pages.iter().enumerate() {
        if let Some(r) = p.reference.filter(|_| !reserved.contains(&i)) {
            importer.drop_ref(r);
        }
    }
    out
}

/// Copie les pages `indices` de la source de `importer` dans sa
/// destination, sans les ranger dans son arbre : rend chaque copie avec sa
/// référence, dans l'ordre demandé. L'importeur garde ensuite ce qu'il a
/// copié : ce qu'on importe après avec lui (les calques) désigne les mêmes
/// objets que les pages.
fn import_pages(importer: &mut Importer<'_>, indices: &[usize]) -> Result<Vec<(ObjectRef, Page)>> {
    let (src, dst) = (importer.src, importer.dst);
    let src_pages = collect_pages(src)?;
    if let Some(&bad) = indices.iter().find(|&&i| i >= src_pages.len()) {
        return Err(Error::Corrupt(format!(
            "page source {} inexistante",
            bad + 1
        )));
    }
    let reserved = prepare_page_refs(importer, &src_pages, indices, dst);
    let mut out = Vec::with_capacity(indices.len());
    for (&i, slot) in indices.iter().zip(reserved) {
        let Some(p) = src_pages.get(i) else {
            continue;
        };
        let Object::Dict(dict) = importer.import(&Object::Dict(p.dict.clone()))? else {
            return Err(Error::Corrupt(format!("page source {} illisible", i + 1)));
        };
        let (r, dict) = if let Some(r) = slot {
            (r, dict)
        } else {
            // Une répétition : les annotations importées sont celles de la
            // première occurrence, la copie en reçoit à elle.
            let r = dst.allocate();
            let first = Page {
                index: 0,
                reference: Some(r),
                dict,
            };
            (r, duplicate_page_dict(dst, &first, r)?)
        };
        dst.set(r, Object::Dict(dict.clone()));
        out.push((
            r,
            Page {
                index: 0,
                reference: Some(r),
                dict,
            },
        ));
    }
    Ok(out)
}

/// Insère dans `dst`, à la position `at` (0 = avant la première page), les
/// pages d'index `indices` de `src`. Ressources, annotations et contenus
/// sont copiés, et les calques qu'elles emploient rejoignent ceux de `dst`
/// ([`merge_layers`]) ; les champs de formulaire (`/AcroForm`) ne sont pas
/// fusionnés.
///
/// # Errors
/// Index ou position invalide, objets illisibles.
pub fn insert_pages_from(
    dst: &Document,
    src: &Document,
    indices: &[usize],
    at: usize,
) -> Result<()> {
    let dst_pages = page_refs(dst)?;
    if at > dst_pages.len() {
        return Err(Error::Corrupt(format!("position {at} hors du document")));
    }
    let mut importer = Importer::new(src, dst);
    let new_pages = import_pages(&mut importer, indices)?;
    let mut all = dst_pages;
    let tail = all.split_off(at);
    all.extend(new_pages);
    all.extend(tail);
    rebuild_tree(dst, &all)?;
    merge_layers(&importer)
}

/// Nouveau document ne contenant que les pages d'index `indices` de `src`,
/// dans cet ordre (extraction). Les métadonnées `/Info` sont copiées, et les
/// calques (`/OCProperties`) : sans eux, un calque masqué par défaut
/// réapparaîtrait sur la page extraite.
///
/// Un document chiffré s'extrait **en clair** : les flux copiés sont ceux
/// que la lecture a déjà déchiffrés.
///
/// # Errors
/// Index invalide ou objets illisibles.
pub fn extract_pages(src: &Document, indices: &[usize]) -> Result<Document> {
    if indices.is_empty() {
        return Err(Error::Corrupt("aucune page à extraire".into()));
    }
    let (major, minor) = src.version();
    let skeleton = format!(
        "%PDF-{major}.{minor}\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\nxref\n0 3\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n0\n%%EOF\n"
    );
    // Les offsets ci-dessus sont approximatifs : le chargeur répare et la
    // sauvegarde complète réécrit tout proprement.
    let dst = Document::from_bytes(skeleton.into_bytes())?;
    if let Some(info) = src.info() {
        let mut importer = Importer::new(src, &dst);
        let copied = importer.import(&Object::Dict(info))?;
        let r = dst.add(copied);
        dst.set_trailer_entry("Info", Object::Reference(r));
    }
    let mut importer = Importer::new(src, &dst);
    let pages = import_pages(&mut importer, indices)?;
    rebuild_tree(&dst, &pages)?;
    copy_layers(&mut importer)?;
    Ok(dst)
}

/// Recopie les calques (`/OCProperties` du catalogue) de la source de
/// `importer` dans sa destination. C'est l'importeur des pages qui s'en
/// charge : les groupes que désignent les contenus copiés et ceux que
/// masque la configuration par défaut doivent être les mêmes objets.
fn copy_layers(importer: &mut Importer<'_>) -> Result<()> {
    let (src, dst) = (importer.src, importer.dst);
    let catalog = src.catalog()?;
    let Some(layers) = catalog.get(&Name::new("OCProperties")) else {
        return Ok(());
    };
    let copied = importer.import(layers)?;
    set_catalog_entry(dst, "OCProperties", copied)
}

/// Pose une entrée du catalogue de `doc`.
fn set_catalog_entry(doc: &Document, key: &str, value: Object) -> Result<()> {
    let mut catalog = doc.catalog()?;
    catalog.insert(Name::new(key), value);
    let cat_ref = acrux_document::xref::trailer_ref(&doc.trailer(), "Root")
        .ok_or_else(|| Error::Corrupt("trailer sans /Root".into()))?;
    doc.set(cat_ref, Object::Dict(catalog));
    Ok(())
}

/// Les références d'un tableau rangé sous `key` dans `dict` (direct ou
/// indirect) ; vide s'il n'y en a pas.
fn ref_list(doc: &Document, dict: &Dict, key: &str) -> Vec<Object> {
    doc.dict_get(dict, key)
        .ok()
        .flatten()
        .and_then(|a| a.as_array().map(<[Object]>::to_vec))
        .unwrap_or_default()
}

/// Le dictionnaire rangé sous `key` dans `dict` (direct ou indirect).
fn sub_dict(doc: &Document, dict: &Dict, key: &str) -> Option<Dict> {
    doc.dict_get(dict, key)
        .ok()
        .flatten()
        .and_then(|d| d.as_dict().cloned())
}

/// Vrai si la configuration par défaut `d` part de « tout masqué »
/// (`/BaseState /OFF`, ISO 32000-2 §8.11.4.3).
fn base_state_off(doc: &Document, d: &Dict) -> bool {
    matches!(
        doc.dict_get(d, "BaseState").ok().flatten().as_deref(),
        Some(Object::Name(n)) if n.0 == b"OFF"
    )
}

/// Rattache aux calques de la destination de `importer` ceux de sa source
/// que les pages copiées emploient (insertion, remplacement).
///
/// Un groupe de contenu optionnel copié sans cela n'appartient à aucune
/// configuration : les afficheurs le montrent, et un calque masqué par
/// défaut dans la source — un « Brouillon » — réapparaissait sur la page
/// insérée ou remplacée. Seuls les groupes que l'importeur a déjà copiés
/// avec les pages sont rattachés : le panneau des calques ne se remplit pas
/// de ceux des pages laissées de côté. Un groupe masqué par défaut dans la
/// source l'est aussi dans la destination, qu'elle parte de « tout
/// visible » (il entre dans `/OFF`) ou de « tout masqué » (seuls les
/// visibles entrent dans `/ON`).
fn merge_layers(importer: &Importer<'_>) -> Result<()> {
    let (src, dst) = (importer.src, importer.dst);
    let Some(src_props) = sub_dict(src, &src.catalog()?, "OCProperties") else {
        return Ok(());
    };
    let src_d = sub_dict(src, &src_props, "D").unwrap_or_default();
    let src_base_off = base_state_off(src, &src_d);
    let listed =
        |key: &str, r: ObjectRef| ref_list(src, &src_d, key).contains(&Object::Reference(r));
    // Les groupes copiés, avec leur état par défaut dans la source.
    let copied: Vec<(ObjectRef, bool)> = ref_list(src, &src_props, "OCGs")
        .iter()
        .filter_map(|o| match o {
            Object::Reference(r) => importer.map.get(&r.number).map(|&to| {
                let hidden = if src_base_off {
                    !listed("ON", *r)
                } else {
                    listed("OFF", *r)
                };
                (to, hidden)
            }),
            _ => None,
        })
        .collect();
    if copied.is_empty() {
        return Ok(());
    }
    let dst_catalog = dst.catalog()?;
    let existing = sub_dict(dst, &dst_catalog, "OCProperties");
    let had_layers = existing.is_some();
    let mut props = existing.unwrap_or_default();
    let mut d = sub_dict(dst, &props, "D").unwrap_or_default();
    let dst_base_off = base_state_off(dst, &d);
    // Sans `/Order`, les afficheurs listent `/OCGs` : on n'en crée un que
    // pour un document qui n'avait aucun calque.
    let keep_order = !had_layers || d.contains_key(&Name::new("Order"));
    let mut ocgs = ref_list(dst, &props, "OCGs");
    let mut order = ref_list(dst, &d, "Order");
    let mut on = ref_list(dst, &d, "ON");
    let mut off = ref_list(dst, &d, "OFF");
    for (r, hidden) in copied {
        let group = Object::Reference(r);
        if ocgs.contains(&group) {
            continue;
        }
        ocgs.push(group.clone());
        order.push(group.clone());
        if hidden && !dst_base_off {
            off.push(group);
        } else if !hidden && dst_base_off {
            on.push(group);
        }
    }
    for (key, list) in [("ON", on), ("OFF", off)] {
        if !list.is_empty() {
            d.insert(Name::new(key), Object::Array(list));
        }
    }
    if keep_order {
        d.insert(Name::new("Order"), Object::Array(order));
    }
    props.insert(Name::new("OCGs"), Object::Array(ocgs));
    props.insert(Name::new("D"), Object::Dict(d));
    set_catalog_entry(dst, "OCProperties", Object::Dict(props))
}

/// Fusionne plusieurs documents en un nouveau document (toutes leurs pages, dans l'ordre).
///
/// # Errors
/// Documents illisibles.
pub fn merge(docs: &[&Document]) -> Result<Document> {
    let Some(first) = docs.first() else {
        return Err(Error::Corrupt("rien à fusionner".into()));
    };
    let n = collect_pages(first)?.len();
    let all: Vec<usize> = (0..n).collect();
    let out = extract_pages(first, &all)?;
    for d in &docs[1..] {
        let n = collect_pages(d)?.len();
        let all: Vec<usize> = (0..n).collect();
        let at = collect_pages(&out)?.len();
        insert_pages_from(&out, d, &all, at)?;
    }
    Ok(out)
}

/// Un nombre PDF : entier quand il l'est, réel sinon.
fn number(v: f64) -> Object {
    if v.fract() == 0.0 && v.abs() < 1e9 {
        // Valeur entière et bornée : la conversion est exacte.
        #[allow(clippy::cast_possible_truncation)]
        Object::Integer(v as i64)
    } else {
        Object::Real(v)
    }
}

/// Un rectangle en tableau PDF `[x0 y0 x1 y1]`.
fn rect_array(r: Rect) -> Object {
    Object::Array(vec![number(r.x0), number(r.y0), number(r.x1), number(r.y1)])
}

/// Insère une page vierge à la position `at` (0 = avant la première page,
/// `len` = après la dernière), du format `media` et tournée de `rotate`
/// degrés : ceux de la page voisine, d'ordinaire, pour qu'une page ajoutée
/// dans un document en paysage soit en paysage.
///
/// La page porte un flux de contenu vide : les afficheurs et `acr check`
/// préfèrent une page qui a son `/Contents`.
///
/// # Errors
/// Position hors du document, format vide ou angle qui n'est pas un
/// multiple de 90.
pub fn insert_blank_page(doc: &Document, at: usize, media: Rect, rotate: i32) -> Result<()> {
    if media.is_empty() {
        return Err(Error::Corrupt("format de page vide".into()));
    }
    if rotate % 90 != 0 {
        return Err(Error::Corrupt("l'angle doit être un multiple de 90".into()));
    }
    let pages = page_refs(doc)?;
    if at > pages.len() {
        return Err(Error::Corrupt(format!(
            "position {} hors du document",
            at + 1
        )));
    }
    let mut stream = Dict::new();
    stream.insert(Name::new("Length"), Object::Integer(0));
    let contents = doc.add(Object::Stream {
        dict: stream,
        raw: Vec::new(),
    });
    let mut dict = Dict::new();
    dict.insert(Name::new("Type"), Object::Name(Name::new("Page")));
    dict.insert(Name::new("MediaBox"), rect_array(media));
    dict.insert(Name::new("Resources"), Object::Dict(Dict::new()));
    dict.insert(Name::new("Contents"), Object::Reference(contents));
    let rotate = rotate.rem_euclid(360);
    if rotate != 0 {
        dict.insert(Name::new("Rotate"), Object::Integer(i64::from(rotate)));
    }
    let r = doc.add(Object::Dict(dict.clone()));
    let mut all = pages;
    all.insert(
        at,
        (
            r,
            Page {
                index: 0,
                reference: Some(r),
                dict,
            },
        ),
    );
    rebuild_tree(doc, &all)
}

/// Remplace le contenu de pages de `dst` par celui de pages de `src` :
/// chaque paire `(cible, source)` donne la page de `dst` à remplacer et la
/// page de `src` qui prend sa place.
///
/// La page cible **garde son objet** : les signets, liens, destinations
/// nommées et étiquettes qui la désignent restent justes, et ses
/// annotations restent (les commentaires faits sur l'ancienne version se
/// retrouvent sur la nouvelle, comme dans Acrobat). Seul ce qui fait son
/// contenu change ([`CONTENT_KEYS`]) : les anciennes boîtes sont retirées
/// avant que les nouvelles soient posées, sinon une `/CropBox` de l'ancienne
/// page recadrerait la nouvelle. Les attributs héritables que la source
/// n'a pas sont posés explicitement, pour que la page ne reprenne pas ceux
/// de ses ancêtres dans l'arbre. `/StructParents` est retiré : il désignait
/// un contenu balisé qui n'existe plus (l'arbre de structure devient
/// incomplet, pas faux). Les calques que le nouveau contenu emploie
/// rejoignent ceux de `dst` ([`merge_layers`]).
///
/// # Errors
/// Paire hors bornes, deux paires sur la même page cible, objets illisibles.
pub fn replace_pages(dst: &Document, src: &Document, pairs: &[(usize, usize)]) -> Result<()> {
    if pairs.is_empty() {
        return Err(Error::Corrupt("aucune page à remplacer".into()));
    }
    let mut pages = page_refs(dst)?;
    let src_pages = collect_pages(src)?;
    let mut seen = HashSet::new();
    for &(target, source) in pairs {
        if target >= pages.len() {
            return Err(Error::Corrupt(format!("page {} inexistante", target + 1)));
        }
        if source >= src_pages.len() {
            return Err(Error::Corrupt(format!(
                "page source {} inexistante",
                source + 1
            )));
        }
        if !seen.insert(target) {
            return Err(Error::Corrupt(format!(
                "la page {} est remplacée deux fois",
                target + 1
            )));
        }
    }
    // Une page directe (sans objet à elle) vient de recevoir un objet dans
    // `page_refs` : l'arbre doit être réécrit pour le désigner.
    let direct = pages.iter().any(|(_, p)| p.reference.is_none());
    let mut importer = Importer::new(src, dst);
    for p in &src_pages {
        if let Some(r) = p.reference {
            importer.drop_ref(r);
        }
    }
    for &(target, source) in pairs {
        let (Some(from), Some((r, page))) = (src_pages.get(source), pages.get_mut(target)) else {
            continue;
        };
        // Seul le contenu de la source est importé : ses annotations
        // seraient autant d'objets orphelins dans `dst`.
        let mut wanted = Dict::new();
        for key in CONTENT_KEYS {
            if let Some(v) = from.dict.get(&Name::new(key)) {
                wanted.insert(Name::new(key), v.clone());
            }
        }
        let Object::Dict(content) = importer.import(&Object::Dict(wanted))? else {
            return Err(Error::Corrupt(format!(
                "page source {} illisible",
                source + 1
            )));
        };
        let mut d = page.dict.clone();
        for key in CONTENT_KEYS {
            d.remove(&Name::new(key));
        }
        d.remove(&Name::new("StructParents"));
        d.extend(content);
        let media = from.media_box(src);
        d.entry(Name::new("MediaBox"))
            .or_insert_with(|| rect_array(media));
        d.entry(Name::new("CropBox"))
            .or_insert_with(|| rect_array(media));
        d.entry(Name::new("Resources"))
            .or_insert_with(|| Object::Dict(Dict::new()));
        d.entry(Name::new("Rotate")).or_insert(Object::Integer(0));
        dst.set(*r, Object::Dict(d.clone()));
        page.dict = d;
    }
    if direct {
        rebuild_tree(dst, &pages)?;
    }
    merge_layers(&importer)
}

/// Ordre complet des pages après le déplacement d'un bloc : le glisser de
/// plusieurs vignettes, dans l'application comme dans `acr reorder`.
///
/// `block` est l'ensemble des pages déplacées (dans n'importe quel ordre,
/// doublons permis) ; `at` est la position d'insertion **dans l'ordre
/// d'origine**, de 0 (avant la première page) à `count` (après la
/// dernière) : c'est ce que désigne le trait d'insertion entre deux
/// vignettes. Le bloc garde son ordre interne et s'insère là, les autres
/// pages gardent le leur. Déposer un bloc à l'intérieur de lui-même rend
/// l'ordre d'origine.
#[must_use]
pub fn move_block(count: usize, block: &[usize], at: usize) -> Vec<usize> {
    let mut block: Vec<usize> = block.iter().copied().filter(|&b| b < count).collect();
    block.sort_unstable();
    block.dedup();
    let at = at.min(count);
    let before = block.iter().filter(|&&b| b < at).count();
    let mut order: Vec<usize> = (0..count)
        .filter(|p| block.binary_search(p).is_err())
        .collect();
    let tail = order.split_off(at - before);
    order.extend(block);
    order.extend(tail);
    order
}

/// Ordre complet des pages après la duplication d'un bloc : les copies
/// suivent, ensemble et dans leur ordre, la dernière page du bloc. C'est
/// l'ordre qu'attend [`reorder_pages`], où un indice répété est une copie.
#[must_use]
pub fn duplicate_block(count: usize, block: &[usize]) -> Vec<usize> {
    let mut block: Vec<usize> = block.iter().copied().filter(|&b| b < count).collect();
    block.sort_unstable();
    block.dedup();
    let Some(&last) = block.last() else {
        return (0..count).collect();
    };
    let mut order: Vec<usize> = (0..=last).collect();
    order.extend(&block);
    order.extend(last + 1..count);
    order
}

/// Analyse une spécification de pages « 1,3-5,8 » (1 = première) en indices 0-based.
///
/// # Errors
/// Syntaxe invalide ou page hors document.
pub fn parse_page_spec(spec: &str, page_count: usize) -> Result<Vec<usize>> {
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (part, part),
        };
        let a: usize = a
            .parse()
            .map_err(|_| Error::Corrupt(format!("page invalide : {part}")))?;
        let b: usize = if b.is_empty() {
            page_count
        } else {
            b.parse()
                .map_err(|_| Error::Corrupt(format!("page invalide : {part}")))?
        };
        if a == 0 || b == 0 || a > page_count || b > page_count {
            return Err(Error::Corrupt(format!(
                "page hors document : {part} (1 à {page_count})"
            )));
        }
        if a <= b {
            out.extend(a - 1..b);
        } else {
            out.extend((b - 1..a).rev());
        }
    }
    if out.is_empty() {
        return Err(Error::Corrupt("aucune page indiquée".into()));
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn pdf_with_pages(n: usize) -> Document {
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            String::new(),
        ];
        let mut kids = Vec::new();
        for i in 0..n {
            let num = 3 + i;
            kids.push(format!("{num} 0 R"));
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} 100] >>",
                100 + i
            ));
        }
        objects[1] = format!(
            "<< /Type /Pages /Kids [{}] /Count {n} /Rotate 90 >>",
            kids.join(" ")
        );
        from_objects(&objects)
    }

    /// Un document dont l'objet `i + 1` est `objects[i]`.
    fn from_objects(objects: &[String]) -> Document {
        let mut src = b"%PDF-1.4\n".to_vec();
        for (i, body) in objects.iter().enumerate() {
            src.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        Document::from_bytes(src).unwrap()
    }

    fn widths(doc: &Document) -> Vec<f64> {
        collect_pages(doc)
            .unwrap()
            .iter()
            .map(|p| p.media_box(doc).width())
            .collect()
    }

    fn roundtrip(doc: &Document) -> Document {
        Document::from_bytes(doc.save_full().unwrap()).unwrap()
    }

    /// Nombre d'objets `/Type /Page` du fichier, qu'ils soient dans l'arbre
    /// ou orphelins.
    fn page_objects(doc: &Document) -> usize {
        doc.object_numbers()
            .into_iter()
            .filter(|&number| {
                let obj = doc.get(ObjectRef {
                    number,
                    generation: 0,
                });
                obj.is_ok_and(|o| {
                    o.as_dict()
                        .and_then(|d| d.get(&Name::new("Type")))
                        .and_then(Object::as_name)
                        .is_some_and(|n| n.0 == b"Page")
                })
            })
            .count()
    }

    #[test]
    fn rotate_and_delete() {
        let doc = pdf_with_pages(3);
        rotate_pages(&doc, &[0], 90).unwrap();
        rotate_pages(&doc, &[1], -90).unwrap();
        let d = roundtrip(&doc);
        let pages = collect_pages(&d).unwrap();
        assert_eq!(pages[0].rotate(&d), 180, "90 hérité + 90");
        assert_eq!(pages[1].rotate(&d), 0);
        assert_eq!(pages[2].rotate(&d), 90);
        delete_pages(&d, &[1]).unwrap();
        let d = roundtrip(&d);
        assert_eq!(widths(&d), vec![100.0, 102.0]);
        assert_eq!(
            collect_pages(&d).unwrap()[1].rotate(&d),
            90,
            "attribut hérité conservé après réécriture"
        );
        assert!(delete_pages(&d, &[0, 1]).is_err());
        assert!(rotate_pages(&d, &[0], 45).is_err());
    }

    #[test]
    fn reorder_with_duplicate() {
        let doc = pdf_with_pages(3);
        reorder_pages(&doc, &[2, 0, 2]).unwrap();
        let d = roundtrip(&doc);
        assert_eq!(widths(&d), vec![102.0, 100.0, 102.0]);
        assert!(reorder_pages(&d, &[5]).is_err());
    }

    #[test]
    fn extract_insert_and_merge() {
        let a = pdf_with_pages(2);
        let b = pdf_with_pages(3);
        let ex = extract_pages(&b, &[2, 0]).unwrap();
        let ex = roundtrip(&ex);
        assert_eq!(widths(&ex), vec![102.0, 100.0]);
        insert_pages_from(&a, &b, &[1], 1).unwrap();
        let a = roundtrip(&a);
        assert_eq!(widths(&a), vec![100.0, 101.0, 101.0]);
        let m = merge(&[&a, &b]).unwrap();
        let m = roundtrip(&m);
        assert_eq!(widths(&m), vec![100.0, 101.0, 101.0, 100.0, 101.0, 102.0]);
        assert!(!m.was_repaired());
    }

    #[test]
    fn shared_resources_are_imported_once() {
        // Deux pages partagent la même ressource indirecte.
        let src = b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /Resources 5 0 R >> endobj\n4 0 obj << /Type /Page /Parent 2 0 R /Resources 5 0 R >> endobj\n5 0 obj << /Font << >> >> endobj\n".to_vec();
        let src = Document::from_bytes(src).unwrap();
        let out = extract_pages(&src, &[0, 1]).unwrap();
        let out = roundtrip(&out);
        let pages = collect_pages(&out).unwrap();
        let r0 = pages[0].dict.get(&Name::new("Resources")).unwrap();
        let r1 = pages[1].dict.get(&Name::new("Resources")).unwrap();
        assert_eq!(r0, r1, "ressource partagée importée une seule fois");
    }

    /// Trois pages ; la première porte un lien vers la troisième. Extraire
    /// la première seule ne doit pas embarquer la troisième : le lien reste,
    /// sans destination. Extraire la première et la troisième : le lien
    /// vise la copie de la troisième.
    #[test]
    fn a_link_does_not_drag_its_target_page_along() {
        let doc = from_objects(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Annots [6 0 R] >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 101 100] >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 102 100] /Contents 7 0 R >>".into(),
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /P 3 0 R /Dest [5 0 R /Fit] >>"
                .into(),
            "<< /Length 8 >>\nstream\n0 0 m S\nendstream".into(),
        ]);
        let link_dest = |d: &Document| -> Object {
            let pages = collect_pages(d).unwrap();
            let annots = d.dict_get(&pages[0].dict, "Annots").unwrap().unwrap();
            let link = d.resolve(&annots.as_array().unwrap()[0]).unwrap();
            let dest = d
                .dict_get(link.as_dict().unwrap(), "Dest")
                .unwrap()
                .unwrap();
            dest.as_array().unwrap()[0].clone()
        };
        let alone = roundtrip(&extract_pages(&doc, &[0]).unwrap());
        assert_eq!(collect_pages(&alone).unwrap().len(), 1);
        assert_eq!(page_objects(&alone), 1, "aucune page orpheline");
        assert_eq!(link_dest(&alone), Object::Null);
        let both = roundtrip(&extract_pages(&doc, &[0, 2]).unwrap());
        let pages = collect_pages(&both).unwrap();
        assert_eq!(page_objects(&both), 2);
        assert_eq!(
            link_dest(&both),
            Object::Reference(pages[1].reference.unwrap()),
            "le lien vise la nouvelle page"
        );
        // Le /P de l'annotation désigne sa propre page, pas une copie.
        let annots = both.dict_get(&pages[0].dict, "Annots").unwrap().unwrap();
        let link = both.resolve(&annots.as_array().unwrap()[0]).unwrap();
        assert_eq!(
            link.as_dict().unwrap().get(&Name::new("P")),
            Some(&Object::Reference(pages[0].reference.unwrap()))
        );
        // Même règle pour l'insertion dans un autre document.
        let target = pdf_with_pages(2);
        insert_pages_from(&target, &doc, &[0], 2).unwrap();
        let target = roundtrip(&target);
        assert_eq!(page_objects(&target), 3);
    }

    /// Dupliquer une page annotée : chaque page a ses propres annotations,
    /// le `/P` de la copie désigne la copie, et un widget n'est pas copié.
    #[test]
    fn a_duplicate_owns_its_annotations() {
        let doc = from_objects(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Annots [4 0 R 5 0 R 6 0 R] >>"
                .into(),
            "<< /Type /Annot /Subtype /Text /Rect [0 0 10 10] /P 3 0 R /Contents (note) /Popup 5 0 R >>"
                .into(),
            "<< /Type /Annot /Subtype /Popup /Rect [0 0 50 50] /P 3 0 R /Parent 4 0 R >>".into(),
            "<< /Type /Annot /Subtype /Widget /Rect [0 0 10 10] /P 3 0 R /FT /Tx /T (champ) >>"
                .into(),
        ]);
        reorder_pages(&doc, &[0, 0]).unwrap();
        let d = roundtrip(&doc);
        let pages = collect_pages(&d).unwrap();
        let refs = |p: &Page| -> Vec<ObjectRef> {
            d.dict_get(&p.dict, "Annots")
                .unwrap()
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|o| match o {
                    Object::Reference(r) => *r,
                    _ => panic!("annotation directe"),
                })
                .collect()
        };
        let (a, b) = (refs(&pages[0]), refs(&pages[1]));
        assert_eq!(a.len(), 3);
        assert_eq!(b.len(), 2, "le widget n'est pas copié");
        assert!(b.iter().all(|r| !a.contains(r)), "objets distincts");
        let copy_ref = pages[1].reference.unwrap();
        let note = d.get(b[0]).unwrap();
        let note = note.as_dict().unwrap();
        assert_eq!(
            note.get(&Name::new("P")),
            Some(&Object::Reference(copy_ref))
        );
        assert_eq!(
            note.get(&Name::new("Popup")),
            Some(&Object::Reference(b[1])),
            "la note désigne la fenêtre copiée"
        );
        let popup = d.get(b[1]).unwrap();
        assert_eq!(
            popup.as_dict().unwrap().get(&Name::new("Parent")),
            Some(&Object::Reference(b[0]))
        );
    }

    /// Un calque masqué par défaut dans la source le reste sur la page
    /// insérée ou remplacée : le groupe copié rejoint les calques de la
    /// destination, dans `/OFF`. Les groupes des pages laissées de côté
    /// n'y entrent pas.
    #[test]
    fn hidden_layers_stay_hidden_on_inserted_and_replaced_pages() {
        let src = from_objects(&[
            "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R 6 0 R 7 0 R] /D << /OFF [6 0 R] >> >> >>"
                .into(),
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Properties << /V 5 0 R /B 6 0 R >> >> >>"
                .into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Properties << /A 7 0 R >> >> >>"
                .into(),
            "<< /Type /OCG /Name (Visible) >>".into(),
            "<< /Type /OCG /Name (Brouillon) >>".into(),
            "<< /Type /OCG /Name (Autre) >>".into(),
        ]);
        // Les noms des groupes d'une liste de la configuration.
        let names = |d: &Document, list: &[Object]| -> Vec<String> {
            list.iter()
                .map(|o| {
                    let g = d.resolve(o).unwrap();
                    match d.dict_get(g.as_dict().unwrap(), "Name").unwrap().as_deref() {
                        Some(Object::String(s)) => String::from_utf8_lossy(s).into_owned(),
                        other => panic!("nom de calque : {other:?}"),
                    }
                })
                .collect()
        };
        let layers = |d: &Document| -> (Vec<String>, Vec<String>) {
            let props = sub_dict(d, &d.catalog().unwrap(), "OCProperties").unwrap();
            let config = sub_dict(d, &props, "D").unwrap();
            (
                names(d, &ref_list(d, &props, "OCGs")),
                names(d, &ref_list(d, &config, "OFF")),
            )
        };
        let target = pdf_with_pages(2);
        insert_pages_from(&target, &src, &[0], 1).unwrap();
        let (all, off) = layers(&roundtrip(&target));
        assert_eq!(all, ["Visible", "Brouillon"], "pas le calque de la page 2");
        assert_eq!(off, ["Brouillon"]);
        // Une destination qui a déjà ses calques, partie de « tout masqué » :
        // le groupe visible entre dans /ON, le masqué n'y entre pas.
        let dst = from_objects(&[
            "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [4 0 R] /D << /BaseState /OFF /ON [4 0 R] /Order [4 0 R] >> >> >>"
                .into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>".into(),
            "<< /Type /OCG /Name (Existant) >>".into(),
        ]);
        replace_pages(&dst, &src, &[(0, 0)]).unwrap();
        let d = roundtrip(&dst);
        let props = sub_dict(&d, &d.catalog().unwrap(), "OCProperties").unwrap();
        let config = sub_dict(&d, &props, "D").unwrap();
        assert_eq!(
            names(&d, &ref_list(&d, &props, "OCGs")),
            ["Existant", "Visible", "Brouillon"]
        );
        assert_eq!(
            names(&d, &ref_list(&d, &config, "ON")),
            ["Existant", "Visible"]
        );
        assert_eq!(names(&d, &ref_list(&d, &config, "Order")).len(), 3);
        assert!(ref_list(&d, &config, "OFF").is_empty());
    }

    #[test]
    fn blocks_move_and_duplicate() {
        assert_eq!(move_block(5, &[1, 2], 0), vec![1, 2, 0, 3, 4]);
        assert_eq!(move_block(5, &[1, 2], 5), vec![0, 3, 4, 1, 2]);
        assert_eq!(
            move_block(5, &[2, 1], 2),
            vec![0, 1, 2, 3, 4],
            "dans lui-même"
        );
        assert_eq!(
            move_block(5, &[1, 2], 3),
            vec![0, 1, 2, 3, 4],
            "juste après lui"
        );
        assert_eq!(move_block(5, &[0, 3], 2), vec![1, 0, 3, 2, 4]);
        assert_eq!(move_block(3, &[0], 3), vec![1, 2, 0]);
        assert_eq!(move_block(3, &[2], 0), vec![2, 0, 1]);
        assert_eq!(move_block(3, &[7], 1), vec![0, 1, 2], "hors bornes ignoré");
        assert_eq!(duplicate_block(4, &[1, 2]), vec![0, 1, 2, 1, 2, 3]);
        assert_eq!(duplicate_block(3, &[2]), vec![0, 1, 2, 2]);
        assert_eq!(duplicate_block(3, &[]), vec![0, 1, 2]);
    }

    #[test]
    fn blank_pages_take_the_given_format() {
        let doc = pdf_with_pages(3);
        let pages = collect_pages(&doc).unwrap();
        let (media, rotate) = (pages[1].media_box(&doc), pages[1].rotate(&doc));
        assert_eq!(rotate, 90, "hérité de l'arbre");
        insert_blank_page(&doc, 0, media, rotate).unwrap();
        insert_blank_page(&doc, 2, Rect::new(0.0, 0.0, 595.0, 842.0), 0).unwrap();
        let len = collect_pages(&doc).unwrap().len();
        insert_blank_page(&doc, len, media, 0).unwrap();
        assert!(insert_blank_page(&doc, 99, media, 0).is_err());
        assert!(insert_blank_page(&doc, 0, Rect::default(), 0).is_err());
        assert!(insert_blank_page(&doc, 0, media, 45).is_err());
        let d = roundtrip(&doc);
        assert!(!d.was_repaired());
        assert_eq!(
            widths(&d),
            vec![101.0, 100.0, 595.0, 101.0, 102.0, 101.0],
            "vierges en 1, 3 et 6"
        );
        let pages = collect_pages(&d).unwrap();
        assert_eq!(pages[0].rotate(&d), 90, "comme la voisine");
        assert_eq!(pages[2].rotate(&d), 0);
        assert_eq!(pages[1].rotate(&d), 90, "les autres gardent leur rotation");
        assert!(pages[0].dict.contains_key(&Name::new("Contents")));
    }

    #[test]
    fn replaced_pages_keep_their_object_and_annotations() {
        let dst = from_objects(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /CropBox [0 0 50 50] >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Annots [5 0 R] /StructParents 0 >>"
                .into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>".into(),
            "<< /Type /Annot /Subtype /Text /Rect [0 0 10 10] /P 3 0 R >>".into(),
        ]);
        let src = pdf_with_pages(3);
        let before = collect_pages(&dst).unwrap()[0].reference;
        replace_pages(&dst, &src, &[(0, 2)]).unwrap();
        let d = roundtrip(&dst);
        let pages = collect_pages(&d).unwrap();
        assert_eq!(pages[0].reference, before, "même objet");
        let width = |r: Rect| r.width().round();
        assert_eq!(width(pages[0].media_box(&d)).to_string(), "102");
        assert_eq!(
            width(pages[0].crop_box(&d)).to_string(),
            "102",
            "la CropBox héritée ne recadre pas la nouvelle page"
        );
        assert_eq!(pages[0].rotate(&d), 90, "rotation de la source");
        assert!(pages[0].dict.contains_key(&Name::new("Annots")));
        assert!(!pages[0].dict.contains_key(&Name::new("StructParents")));
        assert_eq!(
            width(pages[1].crop_box(&d)).to_string(),
            "50",
            "l'autre page n'a pas changé"
        );
        assert_eq!(page_objects(&d), 2, "rien d'autre n'est importé");
        assert!(replace_pages(&dst, &src, &[(0, 1), (0, 2)]).is_err());
        assert!(replace_pages(&dst, &src, &[(5, 0)]).is_err());
        assert!(replace_pages(&dst, &src, &[(0, 9)]).is_err());
        assert!(replace_pages(&dst, &src, &[]).is_err());
    }

    #[test]
    fn page_spec() {
        assert_eq!(parse_page_spec("1,3-5,8", 10).unwrap(), vec![0, 2, 3, 4, 7]);
        assert_eq!(parse_page_spec("5-3", 10).unwrap(), vec![4, 3, 2]);
        assert_eq!(parse_page_spec("9-", 10).unwrap(), vec![8, 9]);
        assert!(parse_page_spec("0", 10).is_err());
        assert!(parse_page_spec("11", 10).is_err());
        assert!(parse_page_spec("a", 10).is_err());
        assert!(parse_page_spec("", 10).is_err());
    }
}
