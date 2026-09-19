//! Pièces jointes (ISO 32000-2 §7.11 « spécifications de fichier »,
//! §7.11.4 « flux de fichier incorporé » et §12.5.6.15 « annotation
//! `/FileAttachment` » ; inventaire Acrobat §11).
//!
//! Un PDF transporte des fichiers de deux façons, et Acrobat les montre
//! toutes les deux dans le même volet :
//!
//! - **au niveau du document** : l'arbre de noms `/Names /EmbeddedFiles` du
//!   catalogue associe un nom à une spécification de fichier `/Filespec` ;
//! - **au niveau d'une page** : une annotation `/FileAttachment` porte la
//!   même `/Filespec` dans son `/FS`, avec une icône cliquable.
//!
//! Ce module lit les deux, extrait les octets (en vérifiant la somme de
//! contrôle MD5 `/Params /CheckSum` quand elle est là), et sait joindre ou
//! retirer un fichier.
//!
//! ## Pourquoi l'arbre est reconstruit à chaque écriture
//!
//! §7.9.6 exige d'un arbre de noms qu'il soit **trié** par ordre d'octets
//! des clés et **équilibré** : c'est ce qui permet à un lecteur de trouver
//! une entrée par dichotomie sans lire tout l'arbre, et c'est ce que
//! contrôlent les vérificateurs de conformité. Plutôt que d'insérer au bon
//! endroit dans un arbre dont on ne maîtrise pas la forme d'origine, on
//! relit toutes les entrées, on trie, et on reconstruit un arbre neuf dont
//! les feuilles sont de tailles voisines. Le coût est négligeable (quelques
//! dizaines d'entrées au plus dans la vraie vie) et la forme obtenue est
//! toujours conforme.

use std::collections::HashSet;
use std::fmt::Write as _;

use acrux_codecs::flate;
use acrux_core::{Error, Rect, Result};
use acrux_document::crypt::md5::md5;
use acrux_document::text::decode_text_string;
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef};

use crate::annotations::{encode_text, fmt_number, form_xobject, pdf_date_now, Rgb};

/// Entrées par feuille de l'arbre de noms reconstruit. Trente-deux garde des
/// feuilles courtes à lire tout en évitant un arbre profond pour les
/// documents ordinaires (une seule feuille jusqu'à 32 pièces jointes).
const LEAF_SIZE: usize = 32;

/// Enfants par nœud intermédiaire.
const FANOUT: usize = 16;

/// Profondeur maximale explorée : un arbre cyclique ou absurde s'arrête là.
const MAX_DEPTH: usize = 64;

/// Niveau de compression du flux incorporé (compromis taille / temps, comme
/// pour les archives ZIP d'`export`).
const LEVEL: u8 = 6;

/// Côté de l'icône d'une annotation `/FileAttachment`, en points.
const ICON_SIZE: f64 = 20.0;

/// Une pièce jointe du document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// Nom du fichier (`/UF`, sinon `/F`, sinon la clé de l'arbre de noms).
    pub name: String,
    /// `/Desc` de la spécification de fichier.
    pub description: Option<String>,
    /// Page portant l'annotation `/FileAttachment`, si la pièce est posée sur
    /// une page (0 = première). `None` pour une pièce jointe du document.
    pub page: Option<usize>,
    /// `/Params /Size` : taille annoncée des données décompressées.
    pub size: Option<u64>,
    /// `/Subtype` du flux incorporé, interprété comme un type MIME
    /// (`text/plain`, `application/pdf`…).
    pub mime: Option<String>,
    /// `/Params /CreationDate`, brute (`D:AAAAMMJJ…`).
    pub created: Option<String>,
    /// `/Params /ModDate`, brute.
    pub modified: Option<String>,
    /// `/Params /CheckSum` : somme MD5 des données décompressées.
    pub checksum: Option<[u8; 16]>,
    /// Référence du flux incorporé (`/EF /F`), quand il est indirect.
    pub stream: Option<ObjectRef>,
}

/// Options de [`add_attachment`].
#[derive(Debug, Clone)]
pub struct AttachOptions {
    /// `/Desc` : la phrase que montre le volet des pièces jointes.
    pub description: Option<String>,
    /// Type MIME. Déduit de l'extension du nom quand il vaut `None`.
    pub mime: Option<String>,
    /// Page sur laquelle poser une annotation `/FileAttachment` (0 = première).
    /// `None` : la pièce n'existe qu'au niveau du document.
    pub page: Option<usize>,
    /// Coin supérieur gauche de l'icône, en points. Par défaut, en haut à
    /// gauche de la page avec une marge de 24 pt.
    pub position: Option<(f64, f64)>,
    /// Couleur de l'icône.
    pub color: Rgb,
    /// Date à écrire dans `/CreationDate` et `/ModDate`. `None` : maintenant.
    /// Une date fixée rend le fichier produit reproductible (corpus de test).
    pub date: Option<String>,
}

impl Default for AttachOptions {
    fn default() -> Self {
        Self {
            description: None,
            mime: None,
            page: None,
            position: None,
            // Ambre : la couleur du trombone d'Acrobat, lisible sur du blanc
            // comme sur un fond sombre.
            color: [0.90, 0.62, 0.11],
            date: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Lecture
// ---------------------------------------------------------------------------

/// Référence du catalogue (`/Root` du trailer).
fn catalog_ref(doc: &Document) -> Result<ObjectRef> {
    match doc.trailer().get(&Name::new("Root")) {
        Some(Object::Reference(r)) => Ok(*r),
        _ => Err(Error::Corrupt(
            "catalogue absent ou direct : impossible d'y écrire les pièces jointes".into(),
        )),
    }
}

/// Toutes les entrées de l'arbre `/Names /EmbeddedFiles`, dans l'ordre de
/// parcours, plus la liste des nœuds indirects rencontrés (à supprimer quand
/// l'arbre est reconstruit).
fn read_tree(doc: &Document) -> (Vec<(Vec<u8>, Object)>, Vec<ObjectRef>) {
    let mut entries = Vec::new();
    let mut nodes = Vec::new();
    let Ok(catalog) = doc.catalog() else {
        return (entries, nodes);
    };
    let Some(names) = doc.dict_get(&catalog, "Names").ok().flatten() else {
        return (entries, nodes);
    };
    let Some(names) = names.as_dict() else {
        return (entries, nodes);
    };
    if let Some(Object::Reference(r)) = names.get(&Name::new("EmbeddedFiles")) {
        nodes.push(*r);
    }
    let Some(root) = doc.dict_get(names, "EmbeddedFiles").ok().flatten() else {
        return (entries, nodes);
    };
    let Some(root) = root.as_dict() else {
        return (entries, nodes);
    };
    let mut visited = HashSet::new();
    walk_tree(doc, root, &mut entries, &mut nodes, &mut visited, 0);
    (entries, nodes)
}

/// Parcours en profondeur d'un nœud d'arbre de noms (§7.9.6).
fn walk_tree(
    doc: &Document,
    node: &Dict,
    entries: &mut Vec<(Vec<u8>, Object)>,
    nodes: &mut Vec<ObjectRef>,
    visited: &mut HashSet<u32>,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        return;
    }
    if let Some(list) = doc.dict_get(node, "Names").ok().flatten() {
        if let Some(pairs) = list.as_array() {
            for pair in pairs.chunks_exact(2) {
                let Ok(key) = doc.resolve(&pair[0]) else {
                    continue;
                };
                if let Object::String(s) = &*key {
                    entries.push((s.clone(), pair[1].clone()));
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
            nodes.push(*r);
        }
        let Ok(kid) = doc.resolve(kid) else { continue };
        if let Some(d) = kid.as_dict() {
            walk_tree(doc, d, entries, nodes, visited, depth + 1);
        }
    }
}

/// Chaîne de texte d'un dictionnaire, décodée (PDFDoc ou UTF-16BE).
fn text(doc: &Document, d: &Dict, key: &str) -> Option<String> {
    let o = doc.dict_get(d, key).ok().flatten()?;
    match &*o {
        Object::String(s) => Some(decode_text_string(s)),
        _ => None,
    }
}

/// Flux incorporé d'une spécification de fichier : `/EF /F`, sinon `/UF`,
/// sinon les variantes historiques par système.
fn embedded_stream(doc: &Document, spec: &Dict) -> Option<(Option<ObjectRef>, Dict)> {
    let ef = doc.dict_get(spec, "EF").ok().flatten()?;
    let ef = ef.as_dict()?;
    for key in ["F", "UF", "DOS", "Mac", "Unix"] {
        let Some(entry) = ef.get(&Name::new(key)) else {
            continue;
        };
        let reference = match entry {
            Object::Reference(r) => Some(*r),
            _ => None,
        };
        let Ok(resolved) = doc.resolve(entry) else {
            continue;
        };
        if let Object::Stream { dict, .. } = &*resolved {
            return Some((reference, dict.clone()));
        }
    }
    None
}

/// Construit une [`Attachment`] à partir d'une spécification de fichier.
fn attachment_from_spec(doc: &Document, spec: &Dict, key: Option<&[u8]>) -> Attachment {
    let name = text(doc, spec, "UF")
        .or_else(|| text(doc, spec, "F"))
        .or_else(|| key.map(decode_text_string))
        .unwrap_or_default();
    let mut out = Attachment {
        name,
        description: text(doc, spec, "Desc"),
        page: None,
        size: None,
        mime: None,
        created: None,
        modified: None,
        checksum: None,
        stream: None,
    };
    let Some((reference, stream)) = embedded_stream(doc, spec) else {
        return out;
    };
    out.stream = reference;
    out.mime = doc
        .dict_get(&stream, "Subtype")
        .ok()
        .flatten()
        .and_then(|o| o.as_name().map(Name::as_str));
    if let Some(params) = doc.dict_get(&stream, "Params").ok().flatten() {
        if let Some(params) = params.as_dict() {
            out.size = doc
                .dict_get(params, "Size")
                .ok()
                .flatten()
                .and_then(|o| o.as_i64())
                .and_then(|v| u64::try_from(v).ok());
            out.created = text(doc, params, "CreationDate");
            out.modified = text(doc, params, "ModDate");
            if let Some(o) = doc.dict_get(params, "CheckSum").ok().flatten() {
                if let Object::String(s) = &*o {
                    if let Ok(sum) = <[u8; 16]>::try_from(s.as_slice()) {
                        out.checksum = Some(sum);
                    }
                }
            }
        }
    }
    out
}

/// Inventaire des pièces jointes : d'abord celles du document (arbre de noms
/// `/Names /EmbeddedFiles`, dans l'ordre des clés), puis celles portées par
/// une annotation `/FileAttachment` et absentes de l'arbre.
///
/// Une pièce présente dans l'arbre **et** posée sur une page n'apparaît
/// qu'une fois, avec sa page renseignée : c'est un seul fichier.
///
/// # Errors
/// Arbre des pages illisible.
pub fn list_attachments(doc: &Document) -> Result<Vec<Attachment>> {
    let mut out = Vec::new();
    let (entries, _) = read_tree(doc);
    for (key, value) in &entries {
        let Ok(resolved) = doc.resolve(value) else {
            continue;
        };
        let Some(spec) = resolved.as_dict() else {
            continue;
        };
        out.push(attachment_from_spec(doc, spec, Some(key)));
    }
    for page in collect_pages(doc)? {
        let Some(annots) = page.dict.get(&Name::new("Annots")) else {
            continue;
        };
        let Ok(annots) = doc.resolve(annots) else {
            continue;
        };
        let Some(list) = annots.as_array() else {
            continue;
        };
        for item in list {
            let Ok(annot) = doc.resolve(item) else {
                continue;
            };
            let Some(annot) = annot.as_dict() else {
                continue;
            };
            let is_attachment = annot
                .get(&Name::new("Subtype"))
                .and_then(Object::as_name)
                .is_some_and(|n| n.0 == b"FileAttachment");
            if !is_attachment {
                continue;
            }
            let Some(fs) = doc.dict_get(annot, "FS").ok().flatten() else {
                continue;
            };
            let Some(spec) = fs.as_dict() else { continue };
            let mut found = attachment_from_spec(doc, spec, None);
            found.page = Some(page.index);
            // Même fichier que dans l'arbre : on renseigne seulement sa page.
            match out
                .iter_mut()
                .find(|a| same_file(a, &found) && a.page.is_none())
            {
                Some(existing) => existing.page = found.page,
                None => out.push(found),
            }
        }
    }
    Ok(out)
}

/// Deux descriptions désignent-elles le même fichier ? Le flux incorporé fait
/// foi ; à défaut (flux direct), le nom.
fn same_file(a: &Attachment, b: &Attachment) -> bool {
    match (a.stream, b.stream) {
        (Some(x), Some(y)) => x == y,
        _ => !a.name.is_empty() && a.name == b.name,
    }
}

/// Octets d'une pièce jointe, flux décodé.
///
/// Quand la pièce porte une somme de contrôle `/Params /CheckSum`, elle est
/// vérifiée : un fichier abîmé est signalé plutôt que rendu en silence.
///
/// # Errors
/// Flux introuvable, filtre illisible, ou somme de contrôle fausse.
pub fn read_attachment(doc: &Document, attachment: &Attachment) -> Result<Vec<u8>> {
    let object = match attachment.stream {
        Some(r) => (*doc.get(r)?).clone(),
        None => find_stream_by_name(doc, &attachment.name)?,
    };
    let data = doc.stream_data(&object)?.data;
    if let Some(expected) = attachment.checksum {
        let actual = md5(&data);
        if actual != expected {
            return Err(Error::Corrupt(format!(
                "pièce jointe « {} » : somme de contrôle MD5 fausse",
                attachment.name
            )));
        }
    }
    Ok(data)
}

/// Flux d'une pièce jointe retrouvée par son nom (cas d'un `/EF /F` direct,
/// que l'inventaire ne peut pas référencer).
fn find_stream_by_name(doc: &Document, name: &str) -> Result<Object> {
    let (entries, _) = read_tree(doc);
    for (key, value) in &entries {
        let Ok(resolved) = doc.resolve(value) else {
            continue;
        };
        let Some(spec) = resolved.as_dict() else {
            continue;
        };
        let spec_name = text(doc, spec, "UF")
            .or_else(|| text(doc, spec, "F"))
            .unwrap_or_else(|| decode_text_string(key));
        if spec_name != name {
            continue;
        }
        let Some(ef) = doc.dict_get(spec, "EF").ok().flatten() else {
            continue;
        };
        let Some(ef) = ef.as_dict() else { continue };
        for k in ["F", "UF", "DOS", "Mac", "Unix"] {
            if let Some(entry) = ef.get(&Name::new(k)) {
                if let Ok(resolved) = doc.resolve(entry) {
                    if matches!(&*resolved, Object::Stream { .. }) {
                        return Ok((*resolved).clone());
                    }
                }
            }
        }
    }
    Err(Error::Corrupt(format!(
        "pièce jointe « {name} » introuvable"
    )))
}

// ---------------------------------------------------------------------------
// Écriture
// ---------------------------------------------------------------------------

/// Type MIME déduit de l'extension du nom. La liste couvre ce qu'on joint
/// vraiment à un PDF ; le reste reste générique.
fn guess_mime(name: &str) -> String {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        Some("csv") => "text/csv",
        Some("html" | "htm") => "text/html",
        Some("xml") => "text/xml",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("tif" | "tiff") => "image/tiff",
        Some("zip") => "application/zip",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Joint un fichier au document et retourne la référence de sa `/Filespec`.
///
/// Le flux incorporé est compressé (FlateDecode) et porte les `/Params`
/// attendus par Acrobat : `/Size`, `/CreationDate`, `/ModDate` et la somme
/// `/CheckSum` MD5 des données **décompressées**. Une pièce jointe du même
/// nom est remplacée.
///
/// # Errors
/// Catalogue illisible, ou page demandée inexistante.
pub fn add_attachment(
    doc: &Document,
    name: &str,
    data: &[u8],
    options: &AttachOptions,
) -> Result<ObjectRef> {
    if name.is_empty() {
        return Err(Error::Corrupt(
            "une pièce jointe doit avoir un nom".to_string(),
        ));
    }
    let stamp = options.date.clone().unwrap_or_else(pdf_date_now);
    let mime = options.mime.clone().unwrap_or_else(|| guess_mime(name));

    let mut params = Dict::new();
    params.insert(
        Name::new("Size"),
        Object::Integer(i64::try_from(data.len()).unwrap_or(i64::MAX)),
    );
    params.insert(
        Name::new("CreationDate"),
        Object::String(stamp.clone().into_bytes()),
    );
    params.insert(Name::new("ModDate"), Object::String(stamp.into_bytes()));
    params.insert(Name::new("CheckSum"), Object::String(md5(data).to_vec()));

    let raw = flate::compress(data, LEVEL);
    let mut stream = Dict::new();
    stream.insert(Name::new("Type"), Object::Name(Name::new("EmbeddedFile")));
    stream.insert(Name::new("Subtype"), Object::Name(Name(mime.into_bytes())));
    stream.insert(Name::new("Filter"), Object::Name(Name::new("FlateDecode")));
    stream.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(raw.len()).unwrap_or(i64::MAX)),
    );
    stream.insert(Name::new("Params"), Object::Dict(params));
    let stream_ref = doc.add(Object::Stream { dict: stream, raw });

    let mut spec = Dict::new();
    spec.insert(Name::new("Type"), Object::Name(Name::new("Filespec")));
    // `/F` est la forme historique (PDFDocEncoding), `/UF` la forme Unicode
    // exigée depuis PDF 1.7 : les lecteurs récents lisent `/UF`, les anciens
    // `/F`, et un nom accentué n'est illisible nulle part.
    spec.insert(Name::new("F"), Object::String(encode_text(name)));
    spec.insert(Name::new("UF"), Object::String(utf16(name)));
    if let Some(d) = &options.description {
        spec.insert(Name::new("Desc"), Object::String(encode_text(d)));
    }
    let mut ef = Dict::new();
    ef.insert(Name::new("F"), Object::Reference(stream_ref));
    ef.insert(Name::new("UF"), Object::Reference(stream_ref));
    spec.insert(Name::new("EF"), Object::Dict(ef));
    let spec_ref = doc.add(Object::Dict(spec));

    let key = encode_text(name);
    let (mut entries, nodes) = read_tree(doc);
    // Remplacement d'une pièce du même nom : l'ancienne sort de l'arbre et
    // ses objets sont libérés, sinon le fichier grossirait à chaque mise à jour.
    if let Some(position) = entries
        .iter()
        .position(|(k, _)| decode_text_string(k) == name)
    {
        let (_, old) = entries.remove(position);
        delete_spec(doc, &old);
    }
    entries.push((key, Object::Reference(spec_ref)));
    write_tree(doc, entries, &nodes)?;

    if let Some(page) = options.page {
        add_file_annotation(doc, page, spec_ref, options)?;
    }
    Ok(spec_ref)
}

/// Retire une pièce jointe : son entrée dans l'arbre, ses objets, et les
/// annotations `/FileAttachment` qui la désignent.
///
/// # Errors
/// Aucune pièce jointe de ce nom, ou catalogue illisible.
pub fn remove_attachment(doc: &Document, name: &str) -> Result<()> {
    let (mut entries, nodes) = read_tree(doc);
    let Some(position) = entries
        .iter()
        .position(|(key, value)| entry_is_named(doc, key, value, name))
    else {
        return Err(Error::Corrupt(format!(
            "pièce jointe « {name} » introuvable"
        )));
    };
    let (_, removed) = entries.remove(position);
    let spec_ref = match removed {
        Object::Reference(r) => Some(r),
        _ => None,
    };
    delete_spec(doc, &removed);
    write_tree(doc, entries, &nodes)?;
    remove_file_annotations(doc, spec_ref, name)?;
    Ok(())
}

/// Vrai si l'entrée d'arbre désigne ce nom : la clé de l'arbre d'abord (c'est
/// elle qui fait foi pour la recherche), puis `/UF` ou `/F` de la
/// spécification, qui peuvent en différer dans les fichiers réels.
fn entry_is_named(doc: &Document, key: &[u8], value: &Object, name: &str) -> bool {
    if decode_text_string(key) == name {
        return true;
    }
    let Ok(resolved) = doc.resolve(value) else {
        return false;
    };
    let Some(spec) = resolved.as_dict() else {
        return false;
    };
    text(doc, spec, "UF")
        .or_else(|| text(doc, spec, "F"))
        .is_some_and(|n| n == name)
}

/// Supprime une spécification de fichier et ses flux incorporés.
fn delete_spec(doc: &Document, value: &Object) {
    let Ok(resolved) = doc.resolve(value) else {
        return;
    };
    if let Some(spec) = resolved.as_dict() {
        if let Some(Object::Reference(ef)) = spec.get(&Name::new("EF")) {
            if let Ok(inner) = doc.get(*ef) {
                delete_streams(doc, &inner);
            }
            doc.delete(*ef);
        } else if let Some(ef) = spec.get(&Name::new("EF")) {
            delete_streams(doc, ef);
        }
    }
    if let Object::Reference(r) = value {
        doc.delete(*r);
    }
}

/// Supprime les flux référencés par un dictionnaire `/EF`.
fn delete_streams(doc: &Document, ef: &Object) {
    let Some(d) = ef.as_dict() else { return };
    let mut seen = HashSet::new();
    for key in ["F", "UF", "DOS", "Mac", "Unix"] {
        if let Some(Object::Reference(r)) = d.get(&Name::new(key)) {
            if seen.insert(r.number) {
                doc.delete(*r);
            }
        }
    }
}

/// Chaîne UTF-16BE avec marque d'ordre des octets (`/UF`, §7.9.2.2).
fn utf16(s: &str) -> Vec<u8> {
    let mut out = vec![0xFE, 0xFF];
    for unit in s.encode_utf16() {
        out.extend_from_slice(&unit.to_be_bytes());
    }
    out
}

/// Reconstruit l'arbre `/Names /EmbeddedFiles` : tri par octets des clés,
/// feuilles de tailles voisines, `/Limits` sur tous les nœuds sauf la racine.
fn write_tree(
    doc: &Document,
    mut entries: Vec<(Vec<u8>, Object)>,
    old: &[ObjectRef],
) -> Result<()> {
    let catalog_ref = catalog_ref(doc)?;
    let mut catalog = doc.catalog()?;
    for node in old {
        doc.delete(*node);
    }
    let mut names = doc
        .dict_get(&catalog, "Names")
        .ok()
        .flatten()
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_default();
    if entries.is_empty() {
        names.remove(&Name::new("EmbeddedFiles"));
        if names.is_empty() {
            catalog.remove(&Name::new("Names"));
        } else {
            catalog.insert(Name::new("Names"), Object::Dict(names));
        }
        doc.set(catalog_ref, Object::Dict(catalog));
        return Ok(());
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    // Feuilles : découpage en parts égales à une unité près.
    let mut level: Vec<(Vec<u8>, Vec<u8>, ObjectRef)> = Vec::new();
    for chunk in even_chunks(entries.len(), LEAF_SIZE) {
        let part = &entries[chunk.0..chunk.1];
        let mut pairs = Vec::with_capacity(part.len() * 2);
        for (key, value) in part {
            pairs.push(Object::String(key.clone()));
            pairs.push(value.clone());
        }
        let (low, high) = (part[0].0.clone(), part[part.len() - 1].0.clone());
        let mut node = Dict::new();
        node.insert(Name::new("Names"), Object::Array(pairs));
        node.insert(Name::new("Limits"), limits(&low, &high));
        level.push((low, high, doc.add(Object::Dict(node))));
    }
    // Étages intermédiaires, jusqu'à ce qu'il n'en reste qu'un.
    while level.len() > 1 {
        let mut next = Vec::new();
        for chunk in even_chunks(level.len(), FANOUT) {
            let part = &level[chunk.0..chunk.1];
            let kids: Vec<Object> = part.iter().map(|(_, _, r)| Object::Reference(*r)).collect();
            let (low, high) = (part[0].0.clone(), part[part.len() - 1].1.clone());
            let mut node = Dict::new();
            node.insert(Name::new("Kids"), Object::Array(kids));
            node.insert(Name::new("Limits"), limits(&low, &high));
            next.push((low, high, doc.add(Object::Dict(node))));
        }
        level = next;
    }
    // La racine ne porte pas de `/Limits` (§7.9.6) : on la réécrit sans.
    let (_, _, root) = level.remove(0);
    let mut node = doc.get(root)?.as_dict().cloned().unwrap_or_default();
    node.remove(&Name::new("Limits"));
    doc.set(root, Object::Dict(node));
    names.insert(Name::new("EmbeddedFiles"), Object::Reference(root));
    catalog.insert(Name::new("Names"), Object::Dict(names));
    doc.set(catalog_ref, Object::Dict(catalog));
    Ok(())
}

/// `/Limits [première dernière]`.
fn limits(low: &[u8], high: &[u8]) -> Object {
    Object::Array(vec![
        Object::String(low.to_vec()),
        Object::String(high.to_vec()),
    ])
}

/// Découpe `len` éléments en parts d'au plus `max`, toutes de la même taille
/// à une unité près : c'est ce qui rend l'arbre équilibré plutôt que de
/// laisser une dernière feuille d'un seul élément.
fn even_chunks(len: usize, max: usize) -> Vec<(usize, usize)> {
    if len == 0 {
        return Vec::new();
    }
    let parts = len.div_ceil(max);
    let base = len / parts;
    let extra = len % parts;
    let mut out = Vec::with_capacity(parts);
    let mut start = 0;
    for i in 0..parts {
        let size = base + usize::from(i < extra);
        out.push((start, start + size));
        start += size;
    }
    out
}

/// Pose une annotation `/FileAttachment` sur une page, avec son icône.
fn add_file_annotation(
    doc: &Document,
    page_index: usize,
    spec: ObjectRef,
    options: &AttachOptions,
) -> Result<ObjectRef> {
    let pages = collect_pages(doc)?;
    let page = pages
        .get(page_index)
        .ok_or_else(|| Error::Corrupt(format!("page {} inexistante", page_index + 1)))?;
    let page_ref = page
        .reference
        .ok_or_else(|| Error::Corrupt("la page doit être un objet indirect".into()))?;
    let media = page.crop_box(doc);
    let (x, y) = options
        .position
        .unwrap_or((media.x0 + 24.0, media.y1 - 24.0));
    let rect = Rect::new(x, y - ICON_SIZE, x + ICON_SIZE, y);

    let mut d = Dict::new();
    d.insert(Name::new("Type"), Object::Name(Name::new("Annot")));
    d.insert(
        Name::new("Subtype"),
        Object::Name(Name::new("FileAttachment")),
    );
    d.insert(Name::new("P"), Object::Reference(page_ref));
    // Même date que le flux incorporé : un `AttachOptions::date` fixé rend
    // tout le fichier produit reproductible (fichiers de corpus).
    d.insert(
        Name::new("M"),
        Object::String(
            options
                .date
                .clone()
                .unwrap_or_else(pdf_date_now)
                .into_bytes(),
        ),
    );
    // Imprimable, taille et orientation figées : une icône reste une icône.
    d.insert(Name::new("F"), Object::Integer(4 | 8 | 16));
    d.insert(
        Name::new("Rect"),
        Object::Array(vec![
            Object::Real(rect.x0),
            Object::Real(rect.y0),
            Object::Real(rect.x1),
            Object::Real(rect.y1),
        ]),
    );
    d.insert(Name::new("Name"), Object::Name(Name::new("Paperclip")));
    d.insert(Name::new("FS"), Object::Reference(spec));
    d.insert(
        Name::new("C"),
        Object::Array(options.color.iter().map(|v| Object::Real(*v)).collect()),
    );
    if let Some(text) = &options.description {
        d.insert(Name::new("Contents"), Object::String(encode_text(text)));
    }
    let ap = doc.add(form_xobject(rect, paperclip(rect, options.color)));
    let mut apd = Dict::new();
    apd.insert(Name::new("N"), Object::Reference(ap));
    d.insert(Name::new("AP"), Object::Dict(apd));
    let annot_ref = doc.add(Object::Dict(d));

    let mut page_dict = page.dict.clone();
    let mut annots = match page_dict.get(&Name::new("Annots")) {
        Some(o) => doc
            .resolve(o)?
            .as_array()
            .map(<[Object]>::to_vec)
            .unwrap_or_default(),
        None => Vec::new(),
    };
    annots.push(Object::Reference(annot_ref));
    page_dict.insert(Name::new("Annots"), Object::Array(annots));
    doc.set(page_ref, Object::Dict(page_dict));
    Ok(annot_ref)
}

/// Contenu d'apparence de l'icône : un trombone, tracé deux fois — une passe
/// large en gris foncé qui fait le contour, une passe fine dans la couleur de
/// l'annotation. C'est ce qui le rend lisible sur n'importe quel fond, y
/// compris un aplat de la même couleur.
fn paperclip(rect: Rect, color: Rgb) -> String {
    let (x, y) = (rect.x0, rect.y0);
    let n = fmt_number;
    // Un trombone : montée, capuchon arrondi, descente, boucle basse, retour.
    let path = format!(
        "{} {} m {} {} l {} {} {} {} {} {} c {} {} l {} {} {} {} {} {} c {} {} l",
        n(x + 7.0),
        n(y + 4.0),
        n(x + 7.0),
        n(y + 13.5),
        n(x + 7.0),
        n(y + 17.5),
        n(x + 13.5),
        n(y + 17.5),
        n(x + 13.5),
        n(y + 13.5),
        n(x + 13.5),
        n(y + 7.0),
        n(x + 13.5),
        n(y + 3.5),
        n(x + 10.0),
        n(y + 3.5),
        n(x + 10.0),
        n(y + 7.0),
        n(x + 10.0),
        n(y + 15.5)
    );
    let mut out = String::new();
    let _ = write!(
        out,
        "q 1 J 1 j 0.25 0.25 0.28 RG 2.6 w {path} S {} {} {} RG 1.3 w {path} S Q",
        n(color[0]),
        n(color[1]),
        n(color[2])
    );
    out
}

/// Retire les annotations `/FileAttachment` qui désignent la pièce jointe
/// donnée (par référence de `/Filespec`, sinon par nom).
fn remove_file_annotations(doc: &Document, spec: Option<ObjectRef>, name: &str) -> Result<()> {
    for page in collect_pages(doc)? {
        let Some(page_ref) = page.reference else {
            continue;
        };
        let Some(annots) = page.dict.get(&Name::new("Annots")) else {
            continue;
        };
        let Ok(resolved) = doc.resolve(annots) else {
            continue;
        };
        let Some(list) = resolved.as_array() else {
            continue;
        };
        let mut kept = Vec::with_capacity(list.len());
        let mut dropped = false;
        for item in list {
            if targets_attachment(doc, item, spec, name) {
                if let Object::Reference(r) = item {
                    doc.delete(*r);
                }
                dropped = true;
                continue;
            }
            kept.push(item.clone());
        }
        if dropped {
            let mut page_dict = page.dict.clone();
            page_dict.insert(Name::new("Annots"), Object::Array(kept));
            doc.set(page_ref, Object::Dict(page_dict));
        }
    }
    Ok(())
}

/// Vrai si l'annotation est une pièce jointe désignant ce fichier.
fn targets_attachment(doc: &Document, item: &Object, spec: Option<ObjectRef>, name: &str) -> bool {
    let Ok(annot) = doc.resolve(item) else {
        return false;
    };
    let Some(annot) = annot.as_dict() else {
        return false;
    };
    let is_attachment = annot
        .get(&Name::new("Subtype"))
        .and_then(Object::as_name)
        .is_some_and(|n| n.0 == b"FileAttachment");
    if !is_attachment {
        return false;
    }
    if let (Some(Object::Reference(r)), Some(target)) = (annot.get(&Name::new("FS")), spec) {
        return *r == target;
    }
    let Some(fs) = doc.dict_get(annot, "FS").ok().flatten() else {
        return false;
    };
    let Some(fs) = fs.as_dict() else { return false };
    text(doc, fs, "UF")
        .or_else(|| text(doc, fs, "F"))
        .is_some_and(|n| n == name)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod tests {
    use super::*;

    /// Assemble un PDF de test à partir d'objets numérotés.
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

    /// Document d'une page, sans pièce jointe.
    fn empty_doc() -> Document {
        let bytes = build_pdf(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".into(),
            ),
        ]);
        Document::from_bytes(bytes).unwrap()
    }

    /// Flux incorporé non compressé, écrit à la main.
    fn embedded(content: &str) -> String {
        format!(
            "<< /Type /EmbeddedFile /Subtype /text#2Fplain /Length {} /Params << /Size {} >> >>\nstream\n{content}\nendstream",
            content.len(),
            content.len()
        )
    }

    /// Document dont l'arbre `/Names /EmbeddedFiles` a **deux niveaux** :
    /// une racine à `/Kids`, deux feuilles à `/Names` et `/Limits`.
    fn two_level_doc() -> Document {
        let bytes = build_pdf(&[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles 6 0 R >> >>".into(),
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".into(),
            ),
            (4, embedded("alpha")),
            (5, embedded("beta")),
            (6, "<< /Kids [7 0 R 8 0 R] >>".into()),
            (
                7,
                "<< /Limits [(a.txt) (a.txt)] /Names [(a.txt) 9 0 R] >>".into(),
            ),
            (
                8,
                "<< /Limits [(b.txt) (b.txt)] /Names [(b.txt) 10 0 R] >>".into(),
            ),
            (
                9,
                "<< /Type /Filespec /F (a.txt) /UF (a.txt) /Desc (premier) /EF << /F 4 0 R >> >>"
                    .into(),
            ),
            (
                10,
                "<< /Type /Filespec /F (b.txt) /EF << /F 5 0 R >> >>".into(),
            ),
        ]);
        Document::from_bytes(bytes).unwrap()
    }

    #[test]
    fn multi_level_name_tree_is_read_in_key_order() {
        let doc = two_level_doc();
        let list = list_attachments(&doc).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "a.txt");
        assert_eq!(list[0].description.as_deref(), Some("premier"));
        assert_eq!(list[0].mime.as_deref(), Some("text/plain"));
        assert_eq!(list[0].size, Some(5));
        assert_eq!(list[0].page, None);
        assert_eq!(list[1].name, "b.txt");
        assert_eq!(read_attachment(&doc, &list[0]).unwrap(), b"alpha");
        assert_eq!(read_attachment(&doc, &list[1]).unwrap(), b"beta");
    }

    #[test]
    fn a_document_without_attachments_lists_nothing() {
        let doc = empty_doc();
        assert!(list_attachments(&doc).unwrap().is_empty());
        // Un arbre vide n'est pas une erreur : c'est le cas le plus courant.
        assert!(remove_attachment(&doc, "absent.txt").is_err());
    }

    #[test]
    fn an_empty_tree_node_is_tolerated() {
        let bytes = build_pdf(&[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles << /Names [] >> >> >>"
                    .into(),
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".into(),
            ),
        ]);
        let doc = Document::from_bytes(bytes).unwrap();
        assert!(list_attachments(&doc).unwrap().is_empty());
        add_attachment(&doc, "note.txt", b"bonjour", &AttachOptions::default()).unwrap();
        let list = list_attachments(&doc).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(read_attachment(&doc, &list[0]).unwrap(), b"bonjour");
    }

    #[test]
    fn adding_the_same_name_twice_replaces_the_file() {
        let doc = empty_doc();
        add_attachment(&doc, "note.txt", b"version 1", &AttachOptions::default()).unwrap();
        add_attachment(&doc, "note.txt", b"version 2", &AttachOptions::default()).unwrap();
        let list = list_attachments(&doc).unwrap();
        assert_eq!(list.len(), 1, "le doublon doit remplacer, pas s'ajouter");
        assert_eq!(read_attachment(&doc, &list[0]).unwrap(), b"version 2");
    }

    #[test]
    fn a_non_ascii_name_survives_a_save_and_reload() {
        let doc = empty_doc();
        let name = "rapport-été-2024 ✓.txt";
        add_attachment(
            &doc,
            name,
            "contenu accentué : é à ü".as_bytes(),
            &AttachOptions {
                description: Some("Résumé de l'année".into()),
                ..AttachOptions::default()
            },
        )
        .unwrap();
        let saved = doc.save_full().unwrap();
        let back = Document::from_bytes(saved).unwrap();
        let list = list_attachments(&back).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, name);
        assert_eq!(list[0].description.as_deref(), Some("Résumé de l'année"));
        assert_eq!(
            read_attachment(&back, &list[0]).unwrap(),
            "contenu accentué : é à ü".as_bytes()
        );
    }

    #[test]
    fn a_wrong_checksum_is_refused() {
        let doc = empty_doc();
        add_attachment(&doc, "note.txt", b"bonjour", &AttachOptions::default()).unwrap();
        let mut list = list_attachments(&doc).unwrap();
        assert!(list[0].checksum.is_some());
        list[0].checksum = Some([0; 16]);
        let err = read_attachment(&doc, &list[0]).unwrap_err();
        assert!(
            format!("{err}").contains("somme de contrôle"),
            "message inattendu : {err}"
        );
    }

    #[test]
    fn an_annotation_attachment_reports_its_page() {
        let doc = empty_doc();
        add_attachment(
            &doc,
            "plan.txt",
            b"schema",
            &AttachOptions {
                page: Some(0),
                description: Some("le plan".into()),
                ..AttachOptions::default()
            },
        )
        .unwrap();
        let list = list_attachments(&doc).unwrap();
        assert_eq!(list.len(), 1, "une seule pièce, pas un doublon par page");
        assert_eq!(list[0].page, Some(0));
        // L'annotation est bien posée, avec son apparence.
        let pages = collect_pages(&doc).unwrap();
        let annots = crate::annotations::list_annotations(&doc, &pages[0]).unwrap();
        assert_eq!(annots.len(), 1);
        assert_eq!(annots[0].subtype, "FileAttachment");
        assert!(annots[0].has_appearance);
        remove_attachment(&doc, "plan.txt").unwrap();
        assert!(list_attachments(&doc).unwrap().is_empty());
        let pages = collect_pages(&doc).unwrap();
        assert!(crate::annotations::list_annotations(&doc, &pages[0])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn the_rebuilt_tree_stays_sorted_and_balanced() {
        let doc = empty_doc();
        for i in 0..100 {
            // Ordre d'insertion volontairement mélangé.
            let n = (i * 37) % 100;
            add_attachment(
                &doc,
                &format!("f{n:03}.txt"),
                format!("contenu {n}").as_bytes(),
                &AttachOptions::default(),
            )
            .unwrap();
        }
        let list = list_attachments(&doc).unwrap();
        assert_eq!(list.len(), 100);
        let names: Vec<&str> = list.iter().map(|a| a.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "l'arbre doit être trié");
        // Forme : racine sans /Limits, feuilles de tailles voisines.
        let catalog = doc.catalog().unwrap();
        let names_dict = doc.dict_get(&catalog, "Names").unwrap().unwrap();
        let root = doc
            .dict_get(names_dict.as_dict().unwrap(), "EmbeddedFiles")
            .unwrap()
            .unwrap();
        let root = root.as_dict().unwrap().clone();
        assert!(
            !root.contains_key(&Name::new("Limits")),
            "la racine ne porte pas de /Limits"
        );
        let kids = doc.dict_get(&root, "Kids").unwrap().unwrap();
        let kids = kids.as_array().unwrap().to_vec();
        assert_eq!(kids.len(), 4, "100 entrées → 4 feuilles de 25");
        let mut sizes = Vec::new();
        for kid in &kids {
            let node = doc.resolve(kid).unwrap();
            let node = node.as_dict().unwrap();
            assert!(node.contains_key(&Name::new("Limits")));
            let pairs = doc.dict_get(node, "Names").unwrap().unwrap();
            sizes.push(pairs.as_array().unwrap().len() / 2);
        }
        let (min, max) = (*sizes.iter().min().unwrap(), *sizes.iter().max().unwrap());
        assert!(max - min <= 1, "feuilles déséquilibrées : {sizes:?}");
        // Et tout se relit après enregistrement.
        let back = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        let list = list_attachments(&back).unwrap();
        assert_eq!(list.len(), 100);
        assert_eq!(read_attachment(&back, &list[7]).unwrap(), b"contenu 7");
    }

    #[test]
    fn even_chunks_are_balanced() {
        assert_eq!(even_chunks(0, 32), Vec::new());
        assert_eq!(even_chunks(5, 32), vec![(0, 5)]);
        // 33 entrées : deux feuilles de 17 et 16, pas 32 + 1.
        assert_eq!(even_chunks(33, 32), vec![(0, 17), (17, 33)]);
        assert_eq!(
            even_chunks(100, 32),
            vec![(0, 25), (25, 50), (50, 75), (75, 100)]
        );
    }

    #[test]
    fn removing_from_a_two_level_tree_keeps_the_rest() {
        let doc = two_level_doc();
        remove_attachment(&doc, "a.txt").unwrap();
        let list = list_attachments(&doc).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "b.txt");
        assert_eq!(read_attachment(&doc, &list[0]).unwrap(), b"beta");
        remove_attachment(&doc, "b.txt").unwrap();
        assert!(list_attachments(&doc).unwrap().is_empty());
        // L'entrée /Names du catalogue disparaît quand elle devient vide.
        let catalog = doc.catalog().unwrap();
        assert!(!catalog.contains_key(&Name::new("Names")));
    }

    /// Écrit `tests/corpus/synthese/pieces-jointes-etiquettes.pdf` :
    /// `cargo test -p acrux-features --lib -- --ignored generate_attachments_corpus`.
    ///
    /// Le fichier exerce d'un coup les deux apports de ce lot : **deux pièces
    /// jointes** (l'une posée sur la page 1 avec son trombone, l'autre au
    /// seul niveau du document) et des **étiquettes de page mélangées**
    /// (i, ii, puis 1, 2, puis Annexe-A). Toutes les dates sont figées :
    /// l'image de référence ne doit pas périmer du jour au lendemain.
    #[test]
    #[ignore = "génère le fichier de corpus"]
    #[allow(clippy::too_many_lines)] // le fichier est décrit en entier ici
    fn generate_attachments_corpus() {
        use crate::pagelabels::{set_page_labels, LabelRange, LabelStyle};

        // Cinq pages 300 × 200, chacune annonçant l'étiquette qu'elle porte :
        // l'image de référence documente ainsi ce que le fichier exerce.
        let body = [
            ("Pr\\351face", "\\351tiquette : i"),
            ("Pr\\351face (suite)", "\\351tiquette : ii"),
            ("Chapitre premier", "\\351tiquette : 1"),
            ("Chapitre premier (suite)", "\\351tiquette : 2"),
            ("Annexe", "\\351tiquette : Annexe-A"),
        ];
        let kids: Vec<String> = (0..body.len())
            .map(|i| format!("{} 0 R", 10 + i * 2))
            .collect();
        let mut objects = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (
                2,
                format!(
                    "<< /Type /Pages /Kids [{}] /Count {} >>",
                    kids.join(" "),
                    body.len()
                ),
            ),
            (
                3,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
                    .to_string(),
            ),
            (
                4,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                    .to_string(),
            ),
        ];
        for (index, (title, label)) in body.iter().enumerate() {
            let content = format!(
                "0.12 0.14 0.20 rg BT /F1 16 Tf 56 150 Td ({title}) Tj ET\n\
                 0.55 0.57 0.62 RG 1 w 56 142 m 264 142 l S\n\
                 0.25 0.27 0.32 rg BT /F2 11 Tf 56 118 Td ({label}) Tj ET\n\
                 BT /F2 9 Tf 56 96 Td (Deux pi\\350ces jointes : voir le trombone et le volet.) Tj ET"
            );
            let page = 10 + index * 2;
            objects.push((
                u32::try_from(page).unwrap(),
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents {} 0 R \
                     /Resources << /Font << /F1 3 0 R /F2 4 0 R >> >> >>",
                    page + 1
                ),
            ));
            objects.push((
                u32::try_from(page + 1).unwrap(),
                format!(
                    "<< /Length {} >>\nstream\n{content}\nendstream",
                    content.len()
                ),
            ));
        }
        let doc = Document::from_bytes(build_pdf(&objects)).unwrap();

        // Date figée (15 janvier 2024, midi UTC) pour un fichier reproductible.
        let date = Some("D:20240115120000Z".to_string());
        add_attachment(
            &doc,
            "notes-de-lecture.txt",
            "Relire la preface avant le chapitre premier.\nVerifier les chiffres de l'annexe.\n"
                .as_bytes(),
            &AttachOptions {
                description: Some("Notes du relecteur".into()),
                page: Some(0),
                position: Some((248.0, 176.0)),
                date: date.clone(),
                ..AttachOptions::default()
            },
        )
        .unwrap();
        add_attachment(
            &doc,
            "tableau-annexe.csv",
            b"annee;montant\n2023;1240\n2024;1387\n",
            &AttachOptions {
                description: Some("Chiffres de l'annexe".into()),
                date,
                ..AttachOptions::default()
            },
        )
        .unwrap();
        set_page_labels(
            &doc,
            &[
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
            ],
        )
        .unwrap();
        let saved = doc.save_full().unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/corpus/synthese/pieces-jointes-etiquettes.pdf");
        std::fs::write(&path, saved).unwrap();
        println!("écrit : {}", path.display());
    }

    #[test]
    fn mime_is_guessed_from_the_extension() {
        assert_eq!(guess_mime("a.PDF"), "application/pdf");
        assert_eq!(guess_mime("a.txt"), "text/plain");
        assert_eq!(guess_mime("sans-extension"), "application/octet-stream");
    }
}
