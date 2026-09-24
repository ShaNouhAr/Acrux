//! Fusion de formulaires : les champs de pages copiées d'un document dans un
//! autre rejoignent le formulaire de la destination (insertion de pages,
//! fusion, combinaison de fichiers ; voir [`crate::pages::InsertOptions`]).
//!
//! L'[`Importer`] des pages a déjà tout copié : les widgets avec leur
//! `/Parent`, donc leurs champs, et, par les `/Kids` de ces champs, leurs
//! frères. Il reste à dire au formulaire de la destination que ces champs
//! existent, et à régler quatre choses qu'une simple copie ne règle pas :
//!
//! - **les widgets restés derrière** : les annotations des pages non copiées
//!   sont écartées par l'importeur, elles deviennent `null` dans les `/Kids`.
//!   On les retire, et avec eux les sous-champs qui n'ont plus de widget ;
//! - **les homonymes** : deux champs de même nom qualifié partagent leur
//!   valeur. Deux copies d'un même formulaire, combinées, se rempliraient
//!   l'une l'autre sans que personne l'ait voulu : un champ racine dont le
//!   nom existe déjà est renommé `nom_2`, `nom_3`… Seul le `/T` de la racine
//!   change : le nom qualifié de tous ses descendants suit ;
//! - **les signatures** : une signature porte sur les octets du document
//!   signé ; recopiée ailleurs, elle ne prouverait plus rien. Le champ garde
//!   sa place, mais perd sa valeur et l'apparence de la signature, comme
//!   dans Acrobat. Un champ de signature vide reste tel quel ;
//! - **les ressources communes** : les polices de `/DR` que la destination
//!   n'a pas la rejoignent (en cas de conflit de nom, celle de la
//!   destination reste), `/DA` est repris s'il manque, et `/NeedAppearances`
//!   passe à vrai si la source le demandait. L'ordre de calcul `/CO` suit ;
//!   un formulaire XFA, lui, n'est jamais recopié.

use std::collections::btree_map::Entry;
use std::collections::HashSet;

use acrux_core::Result;
use acrux_document::{Dict, Document, Name, Object, ObjectRef};

use super::{acroform, name_of, text_of, write_acroform};
use crate::annotations::encode_text;
use crate::pages::Importer;

/// Profondeur au-delà de laquelle un arbre de champs est tenu pour bouclé.
const MAX_DEPTH: usize = 32;

/// Rattache au formulaire de la destination de `importer` les champs de sa
/// source que la copie des pages a emportés. Rend le nombre de champs
/// racines ajoutés.
///
/// # Errors
/// Catalogue ou formulaire illisible.
pub(crate) fn merge_acroform(importer: &mut Importer<'_>) -> Result<usize> {
    let (src, dst) = (importer.source(), importer.destination());
    let Some((src_form, _)) = acroform(src)? else {
        return Ok(0);
    };
    let (mut form, form_ref) = acroform(dst)?.unwrap_or((Dict::new(), None));
    let mut fields = list_of(dst, &form, "Fields");
    // Les noms déjà pris par les champs racines de la destination.
    let mut taken: HashSet<String> = fields
        .iter()
        .filter_map(|f| dst.resolve(f).ok())
        .filter_map(|f| f.as_dict().and_then(|d| text_of(dst, d, "T")))
        .collect();
    let mut added = 0;
    for root in list_of(src, &src_form, "Fields") {
        // Un champ racine direct (sans objet à lui) ne peut pas avoir été
        // atteint par un widget : il n'a rien à suivre.
        let Object::Reference(r) = root else { continue };
        // Seuls comptent les champs que la copie a emportés : ceux dont un
        // widget est sur une page copiée.
        let Some(copy) = importer.mapped(r) else {
            continue;
        };
        if !prune(dst, copy, 0)? {
            continue;
        }
        let Some(mut dict) = dst.get(copy)?.as_dict().cloned() else {
            continue;
        };
        if let Some(name) = text_of(dst, &dict, "T") {
            let name = if taken.contains(&name) {
                let fresh = (2..u32::MAX)
                    .map(|n| format!("{name}_{n}"))
                    .find(|n| !taken.contains(n))
                    .unwrap_or_else(|| format!("{name}_bis"));
                dict.insert(Name::new("T"), Object::String(encode_text(&fresh)));
                dst.set(copy, Object::Dict(dict));
                fresh
            } else {
                name
            };
            taken.insert(name);
        }
        drop_signatures(dst, copy, false, false, 0)?;
        fields.push(Object::Reference(copy));
        added += 1;
    }
    if added == 0 {
        return Ok(0);
    }
    form.insert(Name::new("Fields"), Object::Array(fields));
    merge_resources(importer, &src_form, &mut form)?;
    // L'ordre de calcul : les champs calculés copiés gardent le leur, à la
    // suite de ceux de la destination.
    let mut order = list_of(dst, &form, "CO");
    let before = order.len();
    for entry in list_of(src, &src_form, "CO") {
        if let Some(copy) = match entry {
            Object::Reference(r) => importer.mapped(r),
            _ => None,
        } {
            order.push(Object::Reference(copy));
        }
    }
    if order.len() > before {
        form.insert(Name::new("CO"), Object::Array(order));
    }
    write_acroform(dst, form, form_ref)?;
    Ok(added)
}

/// Un tableau rangé sous `key` dans `dict` (direct ou indirect) ; vide s'il
/// n'y en a pas.
fn list_of(doc: &Document, dict: &Dict, key: &str) -> Vec<Object> {
    doc.dict_get(dict, key)
        .ok()
        .flatten()
        .and_then(|a| a.as_array().map(<[Object]>::to_vec))
        .unwrap_or_default()
}

/// Le dictionnaire rangé sous `key` dans `dict` (direct ou indirect).
fn dict_of(doc: &Document, dict: &Dict, key: &str) -> Option<Dict> {
    doc.dict_get(dict, key)
        .ok()
        .flatten()
        .and_then(|d| d.as_dict().cloned())
}

/// Retire des `/Kids` du champ copié `r` les enfants restés derrière
/// (devenus `null`) et les sous-champs vidés par là. Rend faux si le champ
/// n'a plus rien : ni widget, ni sous-champ.
fn prune(doc: &Document, r: ObjectRef, depth: usize) -> Result<bool> {
    if depth > MAX_DEPTH {
        return Ok(true);
    }
    let Some(mut dict) = doc.get(r)?.as_dict().cloned() else {
        return Ok(false);
    };
    let kids = list_of(doc, &dict, "Kids");
    if kids.is_empty() {
        // Un champ terminal qui est aussi son widget, ou un widget.
        return Ok(true);
    }
    let mut kept = Vec::with_capacity(kids.len());
    for kid in &kids {
        match kid {
            Object::Null => {}
            Object::Reference(k) => {
                if prune(doc, *k, depth + 1)? {
                    kept.push(kid.clone());
                }
            }
            other => kept.push(other.clone()),
        }
    }
    if kept.is_empty() {
        return Ok(false);
    }
    if kept.len() != kids.len() {
        dict.insert(Name::new("Kids"), Object::Array(kept));
        doc.set(r, Object::Dict(dict));
    }
    Ok(true)
}

/// Retire la valeur des champs de signature signés, et l'apparence de leurs
/// widgets (voir l'explication en tête du module). `inherited_sig` dit que
/// le type `/Sig` vient d'un ancêtre, `signed` qu'un ancêtre était signé.
fn drop_signatures(
    doc: &Document,
    r: ObjectRef,
    inherited_sig: bool,
    signed: bool,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let Some(mut dict) = doc.get(r)?.as_dict().cloned() else {
        return Ok(());
    };
    let sig = match name_of(doc, &dict, "FT").as_deref() {
        Some("Sig") => true,
        Some(_) => false,
        None => inherited_sig,
    };
    let mut signed = signed;
    let mut changed = false;
    if sig && dict.remove(&Name::new("V")).is_some() {
        signed = true;
        changed = true;
    }
    let widget = matches!(
        dict.get(&Name::new("Subtype")),
        Some(Object::Name(n)) if n.0 == b"Widget"
    );
    if sig && signed && widget && dict.remove(&Name::new("AP")).is_some() {
        changed = true;
    }
    let kids = list_of(doc, &dict, "Kids");
    if changed {
        doc.set(r, Object::Dict(dict));
    }
    for kid in kids {
        if let Object::Reference(k) = kid {
            drop_signatures(doc, k, sig, signed, depth + 1)?;
        }
    }
    Ok(())
}

/// Les ressources communes du formulaire source que la destination n'a pas :
/// polices de `/DR`, `/DA`, `/NeedAppearances`.
fn merge_resources(importer: &mut Importer<'_>, src_form: &Dict, form: &mut Dict) -> Result<()> {
    let (src, dst) = (importer.source(), importer.destination());
    if let Some(src_fonts) = dict_of(src, src_form, "DR").and_then(|dr| dict_of(src, &dr, "Font")) {
        let mut dr = dict_of(dst, form, "DR").unwrap_or_default();
        let mut fonts = dict_of(dst, &dr, "Font").unwrap_or_default();
        let before = fonts.len();
        for (name, font) in &src_fonts {
            // En cas de conflit de nom, la police de la destination reste :
            // ses propres champs la désignent déjà.
            if !fonts.contains_key(name) {
                fonts.insert(name.clone(), importer.import(font)?);
            }
        }
        if fonts.len() > before {
            dr.insert(Name::new("Font"), Object::Dict(fonts));
            form.insert(Name::new("DR"), Object::Dict(dr));
        }
    }
    if let Entry::Vacant(slot) = form.entry(Name::new("DA")) {
        if let Some(da) = src_form.get(&Name::new("DA")) {
            slot.insert(importer.import(da)?);
        }
    }
    let needs = matches!(
        src.dict_get(src_form, "NeedAppearances")
            .ok()
            .flatten()
            .as_deref(),
        Some(Object::Bool(true))
    );
    if needs {
        form.insert(Name::new("NeedAppearances"), Object::Bool(true));
    }
    Ok(())
}
