//! Formulaires interactifs AcroForm (ISO 32000-2 §12.7 ; inventaire Acrobat
//! §9) : inventaire des champs, remplissage avec régénération des flux
//! d'apparence des widgets (§12.7.4.3), aplatissement dans le contenu des
//! pages, import / export des données au format FDF (§12.7.8).
//!
//! Les apparences sont écrites en syntaxe PDF minimale et non compressée,
//! comme celles de [`crate::annotations`] : lisibles, déterministes, rendues
//! par notre moteur comme par Acrobat. Les coches et boutons radio sont
//! dessinés en chemins vectoriels (forme choisie d'après `/MK /CA`, comme le
//! ferait ZapfDingbats) : aucune dépendance à une police système.
//!
//! Limites connues : ni JavaScript ni actions de calcul, validation ou
//! formatage (`/AA`) — les valeurs sont écrites telles quelles ; les
//! formulaires XFA sont ignorés (seul l'AcroForm est traité) ; le texte
//! enrichi (`/RV`) est rendu comme du texte brut ; les boutons poussoirs et
//! les champs de signature conservent leur apparence d'origine ; les polices
//! composites (Type 0) de `/DA` sont remplacées par Helvetica.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use acrux_core::{Error, Matrix, Rect, Result};
use acrux_document::text::decode_text_string;
use acrux_document::{collect_pages, writer, Dict, Document, Name, Object, ObjectRef, Parser};

use crate::annotations::encode_text;

// Drapeaux de champ (§12.7.4.1 table 227, §12.7.5.2 table 229, §12.7.5.3
// table 232, §12.7.5.4 table 234). Le bit n vaut 1 << (n - 1).
const FF_READ_ONLY: i64 = 1;
const FF_REQUIRED: i64 = 1 << 1;
const FF_NO_EXPORT: i64 = 1 << 2;
const FF_MULTILINE: i64 = 1 << 12;
const FF_PASSWORD: i64 = 1 << 13;
const FF_NO_TOGGLE_TO_OFF: i64 = 1 << 14;
const FF_RADIO: i64 = 1 << 15;
const FF_PUSHBUTTON: i64 = 1 << 16;
const FF_COMBO: i64 = 1 << 17;
const FF_EDIT: i64 = 1 << 18;
const FF_MULTI_SELECT: i64 = 1 << 21;
const FF_COMB: i64 = 1 << 24;
const FF_RADIOS_IN_UNISON: i64 = 1 << 25;

/// Drapeaux d'annotation (§12.5.3) : masquée / non affichée.
const ANNOT_HIDDEN: i64 = 1 << 1;
const ANNOT_NOVIEW: i64 = 1 << 5;

/// Attributs de champ héritables via `/Parent` (§12.7.4.1).
const INHERITABLE: [&str; 8] = ["FT", "Ff", "V", "DV", "DA", "Q", "Opt", "MaxLen"];

/// Nom de ressource de la police Helvetica ajoutée à `/DR` si nécessaire.
const HELV: &str = "Helv";

/// Type de champ (§12.7.4 : `/FT` combiné aux drapeaux).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    /// Champ de texte (`/Tx`).
    Text,
    /// Case à cocher (`/Btn` sans `Radio` ni `Pushbutton`).
    CheckBox,
    /// Bouton radio (`/Btn` avec `Radio`).
    Radio,
    /// Bouton poussoir (`/Btn` avec `Pushbutton`).
    Button,
    /// Liste déroulante (`/Ch` avec `Combo`).
    ComboBox,
    /// Liste de choix (`/Ch` sans `Combo`).
    ListBox,
    /// Champ de signature (`/Sig`).
    Signature,
    /// `/FT` absent ou inconnu.
    Unknown,
}

impl FieldType {
    /// Libellé court en français.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            FieldType::Text => "texte",
            FieldType::CheckBox => "case",
            FieldType::Radio => "radio",
            FieldType::Button => "bouton",
            FieldType::ComboBox => "combo",
            FieldType::ListBox => "liste",
            FieldType::Signature => "signature",
            FieldType::Unknown => "?",
        }
    }
}

/// Drapeaux utiles de `/Ff` (§12.7.4.1 et suivantes).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // reflet direct des bits de /Ff
pub struct FieldFlags {
    /// Valeur brute de `/Ff`.
    pub raw: i64,
    /// Lecture seule.
    pub read_only: bool,
    /// Obligatoire.
    pub required: bool,
    /// Exclu de l'export.
    pub no_export: bool,
    /// Texte : plusieurs lignes.
    pub multiline: bool,
    /// Texte : mot de passe (affiché en astérisques).
    pub password: bool,
    /// Texte : peigne (`MaxLen` cases de largeur égale).
    pub comb: bool,
    /// Choix : liste déroulante.
    pub combo: bool,
    /// Choix : liste déroulante à saisie libre.
    pub edit: bool,
    /// Choix : sélection multiple.
    pub multi_select: bool,
    /// Bouton : groupe radio.
    pub radio: bool,
    /// Bouton : bouton poussoir.
    pub pushbutton: bool,
    /// Bouton : impossible de tout décocher.
    pub no_toggle_to_off: bool,
    /// Radio : les boutons de même valeur d'export basculent ensemble.
    pub radios_in_unison: bool,
}

impl FieldFlags {
    fn from_raw(raw: i64) -> Self {
        Self {
            raw,
            read_only: raw & FF_READ_ONLY != 0,
            required: raw & FF_REQUIRED != 0,
            no_export: raw & FF_NO_EXPORT != 0,
            multiline: raw & FF_MULTILINE != 0,
            password: raw & FF_PASSWORD != 0,
            comb: raw & FF_COMB != 0,
            combo: raw & FF_COMBO != 0,
            edit: raw & FF_EDIT != 0,
            multi_select: raw & FF_MULTI_SELECT != 0,
            radio: raw & FF_RADIO != 0,
            pushbutton: raw & FF_PUSHBUTTON != 0,
            no_toggle_to_off: raw & FF_NO_TOGGLE_TO_OFF != 0,
            radios_in_unison: raw & FF_RADIOS_IN_UNISON != 0,
        }
    }

    /// Libellés des drapeaux actifs (pour l'affichage).
    #[must_use]
    pub fn labels(&self) -> Vec<&'static str> {
        let table = [
            (self.read_only, "lecture seule"),
            (self.required, "obligatoire"),
            (self.no_export, "non exporté"),
            (self.multiline, "multiligne"),
            (self.password, "mot de passe"),
            (self.comb, "peigne"),
            (self.edit, "saisie libre"),
            (self.multi_select, "sélection multiple"),
            (self.no_toggle_to_off, "toujours un choix"),
            (self.radios_in_unison, "radios à l'unisson"),
        ];
        table
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, l)| *l)
            .collect()
    }
}

/// Option d'un champ de choix (`/Opt`) : valeur d'export et texte affiché
/// (identiques quand `/Opt` ne contient que des chaînes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldOption {
    /// Valeur exportée.
    pub export: String,
    /// Texte affiché.
    pub display: String,
}

/// Widget (annotation `/Widget`) d'un champ.
#[derive(Debug, Clone)]
pub struct Widget {
    /// Index de la page (0 = première), si le widget est référencé par une page.
    pub page: Option<usize>,
    /// `/Rect`.
    pub rect: Rect,
    /// Référence de l'annotation.
    pub reference: Option<ObjectRef>,
    /// État d'apparence courant `/AS`.
    pub state: Option<String>,
    /// États disponibles dans `/AP /N` (`Off` compris).
    pub states: Vec<String>,
    /// État « coché » du widget (premier état différent de `Off`).
    pub on_state: Option<String>,
}

/// Valeur d'un champ, ou valeur à écrire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    /// Texte (champ de texte, liste déroulante à saisie libre).
    Text(String),
    /// Nom d'état d'export d'une case ou d'un bouton radio (`Off` pour décocher).
    State(String),
    /// Case à cocher : coché ou non (l'état « coché » est celui du widget).
    Bool(bool),
    /// Choix : une ou plusieurs valeurs d'export.
    Choice(Vec<String>),
}

impl FieldValue {
    /// Représentation texte (pour l'affichage et le FDF).
    #[must_use]
    pub fn to_display(&self) -> String {
        match self {
            FieldValue::Text(s) | FieldValue::State(s) => s.clone(),
            FieldValue::Bool(b) => if *b { "oui" } else { "non" }.to_string(),
            FieldValue::Choice(v) => v.join(" | "),
        }
    }

    /// Interprète une valeur tapée par l'utilisateur selon le type du champ :
    /// `oui/non/true/false/on/off/1/0` pour une case, nom d'état pour un
    /// radio, valeurs séparées par `|` pour une liste, texte sinon.
    #[must_use]
    pub fn parse(kind: FieldType, text: &str) -> Self {
        match kind {
            FieldType::CheckBox => match text.trim().to_ascii_lowercase().as_str() {
                "1" | "on" | "oui" | "yes" | "true" | "vrai" => FieldValue::Bool(true),
                "0" | "off" | "non" | "no" | "false" | "faux" => FieldValue::Bool(false),
                _ => FieldValue::State(text.trim().to_string()),
            },
            FieldType::Radio => FieldValue::State(text.trim().to_string()),
            FieldType::ComboBox | FieldType::ListBox => FieldValue::Choice(
                text.split('|')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            ),
            _ => FieldValue::Text(text.to_string()),
        }
    }
}

/// Champ terminal d'un formulaire, attributs hérités déjà fusionnés.
#[derive(Debug, Clone)]
pub struct Field {
    /// Nom pleinement qualifié (`parent.enfant`, §12.7.4.2).
    pub name: String,
    /// Nom alternatif `/TU` (info-bulle).
    pub alternate_name: Option<String>,
    /// Référence du dictionnaire de champ terminal.
    pub reference: Option<ObjectRef>,
    /// Type.
    pub kind: FieldType,
    /// Drapeaux.
    pub flags: FieldFlags,
    /// Valeur courante décodée (`/V`).
    pub value: Option<FieldValue>,
    /// Valeur par défaut (`/DV`).
    pub default_value: Option<FieldValue>,
    /// Options (`/Opt`) des champs de choix (et des radios qui en ont).
    pub options: Vec<FieldOption>,
    /// Longueur maximale (`/MaxLen`).
    pub max_len: Option<usize>,
    /// Alignement `/Q` : 0 gauche, 1 centré, 2 droite.
    pub quadding: i64,
    /// Apparence par défaut `/DA` (police, taille, couleur).
    pub default_appearance: Option<String>,
    /// Widgets du champ.
    pub widgets: Vec<Widget>,
}

// ---------------------------------------------------------------------------
// Petits accesseurs tolérants.
// ---------------------------------------------------------------------------

fn name_of(doc: &Document, d: &Dict, key: &str) -> Option<String> {
    doc.dict_get(d, key)
        .ok()
        .flatten()
        .and_then(|o| o.as_name().map(Name::as_str))
}

fn int_of(doc: &Document, d: &Dict, key: &str) -> Option<i64> {
    doc.dict_get(d, key).ok().flatten().and_then(|o| o.as_i64())
}

fn text_of(doc: &Document, d: &Dict, key: &str) -> Option<String> {
    let o = doc.dict_get(d, key).ok().flatten()?;
    match &*o {
        Object::String(s) => Some(decode_text_string(s)),
        _ => None,
    }
}

fn numbers_of(doc: &Document, d: &Dict, key: &str) -> Option<Vec<f64>> {
    let o = doc.dict_get(d, key).ok().flatten()?;
    let a = o.as_array()?;
    Some(
        a.iter()
            .filter_map(|v| doc.resolve(v).ok().and_then(|x| x.as_f64()))
            .collect(),
    )
}

fn rect_of(doc: &Document, d: &Dict) -> Rect {
    match numbers_of(doc, d, "Rect") {
        Some(r) if r.len() == 4 => Rect::new(r[0], r[1], r[2], r[3]),
        _ => Rect::default(),
    }
}

fn dict_of(doc: &Document, d: &Dict, key: &str) -> Option<Dict> {
    doc.dict_get(d, key)
        .ok()
        .flatten()
        .and_then(|o| o.as_dict().cloned())
}

/// Référence du catalogue (`/Root` du trailer).
fn catalog_ref(doc: &Document) -> Result<ObjectRef> {
    match doc.trailer().get(&Name::new("Root")) {
        Some(Object::Reference(r)) => Ok(*r),
        _ => Err(Error::Corrupt("trailer sans /Root indirect".into())),
    }
}

/// Dictionnaire `/AcroForm` et sa référence (si indirect).
fn acroform(doc: &Document) -> Result<Option<(Dict, Option<ObjectRef>)>> {
    let catalog = doc.catalog()?;
    let Some(entry) = catalog.get(&Name::new("AcroForm")) else {
        return Ok(None);
    };
    let reference = match entry {
        Object::Reference(r) => Some(*r),
        _ => None,
    };
    let resolved = doc.resolve(entry)?;
    Ok(resolved.as_dict().map(|d| (d.clone(), reference)))
}

/// Écrit le dictionnaire `/AcroForm` (objet indirect existant ou entrée
/// directe du catalogue).
fn write_acroform(doc: &Document, form: Dict, reference: Option<ObjectRef>) -> Result<()> {
    if let Some(r) = reference {
        doc.set(r, Object::Dict(form));
    } else {
        let root = catalog_ref(doc)?;
        let mut catalog = doc.catalog()?;
        catalog.insert(Name::new("AcroForm"), Object::Dict(form));
        doc.set(root, Object::Dict(catalog));
    }
    Ok(())
}

/// Numéro d'objet de chaque widget → index de page, d'après `/Annots`.
fn widget_pages(doc: &Document) -> Result<HashMap<u32, usize>> {
    let mut map = HashMap::new();
    for page in collect_pages(doc)? {
        if let Some(r) = page.reference {
            map.insert(r.number, page.index);
        }
        let Some(annots) = page.dict.get(&Name::new("Annots")) else {
            continue;
        };
        if let Some(list) = doc.resolve(annots)?.as_array() {
            for a in list {
                if let Object::Reference(r) = a {
                    map.insert(r.number, page.index);
                }
            }
        }
    }
    Ok(map)
}

// ---------------------------------------------------------------------------
// Inventaire.
// ---------------------------------------------------------------------------

/// Inventaire des champs terminaux du formulaire (`/AcroForm /Fields`,
/// parcours récursif des `/Kids`, héritage des attributs, noms qualifiés).
///
/// # Errors
/// Catalogue ou tableau de champs illisible.
pub fn list_fields(doc: &Document) -> Result<Vec<Field>> {
    let Some((form, _)) = acroform(doc)? else {
        return Ok(Vec::new());
    };
    let page_of = widget_pages(doc)?;
    let mut inherited = Dict::new();
    for key in ["DA", "Q"] {
        if let Some(v) = form.get(&Name::new(key)) {
            inherited.insert(Name::new(key), v.clone());
        }
    }
    let mut out = Vec::new();
    let Some(fields) = doc.dict_get(&form, "Fields")? else {
        return Ok(out);
    };
    let Some(list) = fields.as_array() else {
        return Ok(out);
    };
    let mut visited = HashSet::new();
    for f in list {
        walk_field(doc, f, "", &inherited, &page_of, &mut out, &mut visited, 0);
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // contexte de parcours récursif
fn walk_field(
    doc: &Document,
    node: &Object,
    prefix: &str,
    inherited: &Dict,
    page_of: &HashMap<u32, usize>,
    out: &mut Vec<Field>,
    visited: &mut HashSet<u32>,
    depth: usize,
) {
    if depth > 32 || out.len() > 100_000 {
        return;
    }
    let reference = match node {
        Object::Reference(r) => {
            if !visited.insert(r.number) {
                return;
            }
            Some(*r)
        }
        _ => None,
    };
    let Ok(resolved) = doc.resolve(node) else {
        return;
    };
    let Some(dict) = resolved.as_dict() else {
        return;
    };
    let partial = text_of(doc, dict, "T");
    let name = match (&partial, prefix.is_empty()) {
        (Some(t), true) => t.clone(),
        (Some(t), false) => format!("{prefix}.{t}"),
        (None, _) => prefix.to_string(),
    };
    let mut merged = inherited.clone();
    for key in INHERITABLE {
        if let Some(v) = dict.get(&Name::new(key)) {
            merged.insert(Name::new(key), v.clone());
        }
    }
    let kids: Vec<Object> = doc
        .dict_get(dict, "Kids")
        .ok()
        .flatten()
        .and_then(|k| k.as_array().map(<[Object]>::to_vec))
        .unwrap_or_default();
    let kid_dicts: Vec<Option<Dict>> = kids
        .iter()
        .map(|k| doc.resolve(k).ok().and_then(|o| o.as_dict().cloned()))
        .collect();
    let has_field_kids = kid_dicts
        .iter()
        .flatten()
        .any(|k| k.contains_key(&Name::new("T")));
    if has_field_kids {
        for kid in &kids {
            walk_field(doc, kid, &name, &merged, page_of, out, visited, depth + 1);
        }
        return;
    }
    // Champ terminal : ses widgets sont ses enfants, ou lui-même.
    let mut widgets = Vec::new();
    if kids.is_empty() {
        if dict.contains_key(&Name::new("Rect")) || name_of(doc, dict, "Subtype").is_some() {
            widgets.push(widget_info(doc, dict, reference, page_of));
        }
    } else {
        for (kid, kd) in kids.iter().zip(&kid_dicts) {
            if let Some(kd) = kd {
                let r = match kid {
                    Object::Reference(r) => Some(*r),
                    _ => None,
                };
                widgets.push(widget_info(doc, kd, r, page_of));
            }
        }
    }
    let flags = FieldFlags::from_raw(int_of(doc, &merged, "Ff").unwrap_or(0));
    let kind = match name_of(doc, &merged, "FT").as_deref() {
        Some("Tx") => FieldType::Text,
        Some("Btn") if flags.pushbutton => FieldType::Button,
        Some("Btn") if flags.radio => FieldType::Radio,
        Some("Btn") => FieldType::CheckBox,
        Some("Ch") if flags.combo => FieldType::ComboBox,
        Some("Ch") => FieldType::ListBox,
        Some("Sig") => FieldType::Signature,
        _ => FieldType::Unknown,
    };
    let options = read_options(doc, &merged);
    out.push(Field {
        name,
        alternate_name: text_of(doc, dict, "TU"),
        reference,
        kind,
        flags,
        value: decode_value(doc, &merged, "V", kind),
        default_value: decode_value(doc, &merged, "DV", kind),
        options,
        max_len: int_of(doc, &merged, "MaxLen").and_then(|n| usize::try_from(n).ok()),
        quadding: int_of(doc, &merged, "Q").unwrap_or(0),
        default_appearance: text_of(doc, &merged, "DA"),
        widgets,
    });
}

fn widget_info(
    doc: &Document,
    d: &Dict,
    reference: Option<ObjectRef>,
    page_of: &HashMap<u32, usize>,
) -> Widget {
    let mut states = Vec::new();
    if let Some(ap) = dict_of(doc, d, "AP") {
        if let Some(n) = doc.dict_get(&ap, "N").ok().flatten() {
            if let Object::Dict(map) = &*n {
                states = map.keys().map(Name::as_str).collect();
            }
        }
    }
    let on_state = states.iter().find(|s| *s != "Off").cloned();
    let page = reference
        .and_then(|r| page_of.get(&r.number).copied())
        .or_else(|| match d.get(&Name::new("P")) {
            Some(Object::Reference(p)) => page_of.get(&p.number).copied(),
            _ => None,
        });
    Widget {
        page,
        rect: rect_of(doc, d),
        reference,
        state: name_of(doc, d, "AS"),
        states,
        on_state,
    }
}

/// `/Opt` : chaînes ou paires `[export affichage]` (§12.7.5.4).
fn read_options(doc: &Document, d: &Dict) -> Vec<FieldOption> {
    let Some(opt) = doc.dict_get(d, "Opt").ok().flatten() else {
        return Vec::new();
    };
    let Some(list) = opt.as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|o| {
            let o = doc.resolve(o).ok()?;
            match &*o {
                Object::String(s) => {
                    let t = decode_text_string(s);
                    Some(FieldOption {
                        export: t.clone(),
                        display: t,
                    })
                }
                Object::Array(pair) => {
                    let get = |i: usize| -> Option<String> {
                        match &*doc.resolve(pair.get(i)?).ok()? {
                            Object::String(s) => Some(decode_text_string(s)),
                            _ => None,
                        }
                    };
                    let export = get(0)?;
                    Some(FieldOption {
                        display: get(1).unwrap_or_else(|| export.clone()),
                        export,
                    })
                }
                _ => None,
            }
        })
        .collect()
}

fn decode_value(doc: &Document, d: &Dict, key: &str, kind: FieldType) -> Option<FieldValue> {
    let v = doc.dict_get(d, key).ok().flatten()?;
    let as_text = |o: &Object| -> Option<String> {
        match o {
            Object::String(s) => Some(decode_text_string(s)),
            Object::Name(n) => Some(n.as_str()),
            Object::Stream { .. } => doc.stream_data(o).ok().map(|s| decode_text_string(&s.data)),
            _ => None,
        }
    };
    match kind {
        FieldType::CheckBox | FieldType::Radio => as_text(&v).map(FieldValue::State),
        FieldType::ComboBox | FieldType::ListBox => match &*v {
            Object::Array(items) => Some(FieldValue::Choice(
                items
                    .iter()
                    .filter_map(|i| doc.resolve(i).ok().and_then(|o| as_text(&o)))
                    .collect(),
            )),
            other => as_text(other).map(|s| FieldValue::Choice(vec![s])),
        },
        FieldType::Signature => match &*v {
            Object::Dict(sig) => Some(FieldValue::Text(
                text_of(doc, sig, "Name").unwrap_or_else(|| "signé".into()),
            )),
            _ => None,
        },
        _ => as_text(&v).map(FieldValue::Text),
    }
}

/// Retrouve un champ par son nom qualifié (ou, à défaut, par un suffixe
/// unique `…​.nom`).
fn find_field<'a>(fields: &'a [Field], name: &str) -> Result<&'a Field> {
    if let Some(f) = fields.iter().find(|f| f.name == name) {
        return Ok(f);
    }
    let suffix = format!(".{name}");
    let mut candidates = fields.iter().filter(|f| f.name.ends_with(&suffix));
    match (candidates.next(), candidates.next()) {
        (Some(f), None) => Ok(f),
        (Some(_), Some(_)) => Err(Error::Corrupt(format!(
            "nom de champ ambigu : « {name} » (préciser le nom qualifié)"
        ))),
        _ => Err(Error::Corrupt(format!("champ introuvable : « {name} »"))),
    }
}

// ---------------------------------------------------------------------------
// Remplissage.
// ---------------------------------------------------------------------------

/// Écrit une valeur dans un champ (`/V`, `/AS` des widgets, `/I` des listes)
/// et régénère les apparences de ses widgets. Met `/NeedAppearances false`.
///
/// # Errors
/// Champ introuvable, en lecture seule, valeur incompatible avec le type ou
/// hors des options, document illisible.
#[allow(clippy::too_many_lines)] // une branche par type de champ
pub fn set_field_value(doc: &Document, name: &str, value: FieldValue) -> Result<()> {
    let fields = list_fields(doc)?;
    let field = find_field(&fields, name)?.clone();
    if field.flags.read_only {
        return Err(Error::Unsupported(format!(
            "le champ « {} » est en lecture seule",
            field.name
        )));
    }
    let field_ref = field.reference.ok_or_else(|| {
        Error::Corrupt(format!(
            "le champ « {} » n'est pas un objet indirect",
            field.name
        ))
    })?;
    let mut field_dict = doc
        .get(field_ref)?
        .as_dict()
        .cloned()
        .ok_or_else(|| Error::Corrupt("dictionnaire de champ attendu".into()))?;
    match field.kind {
        FieldType::Text => {
            let text = match value {
                FieldValue::Text(s) => s,
                FieldValue::Choice(v) => v.join(""),
                FieldValue::State(_) | FieldValue::Bool(_) => {
                    return Err(Error::Unsupported(format!(
                        "« {} » est un champ de texte : donner du texte",
                        field.name
                    )))
                }
            };
            let text = match field.max_len {
                Some(n) if !field.flags.comb => text.chars().take(n).collect(),
                _ => text,
            };
            field_dict.insert(Name::new("V"), Object::String(encode_text(&text)));
            field_dict.remove(&Name::new("RV"));
            doc.set(field_ref, Object::Dict(field_dict));
        }
        FieldType::CheckBox => {
            let state = match value {
                FieldValue::Bool(true) => field
                    .widgets
                    .iter()
                    .find_map(|w| w.on_state.clone())
                    .unwrap_or_else(|| "Yes".into()),
                FieldValue::Bool(false) => "Off".into(),
                FieldValue::State(s) | FieldValue::Text(s) => {
                    match FieldValue::parse(field.kind, &s) {
                        FieldValue::Bool(true) => field
                            .widgets
                            .iter()
                            .find_map(|w| w.on_state.clone())
                            .unwrap_or_else(|| "Yes".into()),
                        FieldValue::Bool(false) => "Off".into(),
                        _ => s,
                    }
                }
                FieldValue::Choice(_) => {
                    return Err(Error::Unsupported(format!(
                        "« {} » est une case à cocher",
                        field.name
                    )))
                }
            };
            set_button_state(doc, &field, field_ref, field_dict, &state)?;
        }
        FieldType::Radio => {
            let export = match value {
                FieldValue::State(s) | FieldValue::Text(s) => s,
                FieldValue::Choice(v) => v.into_iter().next().unwrap_or_default(),
                FieldValue::Bool(false) => "Off".into(),
                FieldValue::Bool(true) => {
                    return Err(Error::Unsupported(format!(
                        "« {} » est un groupe radio : donner la valeur d'export",
                        field.name
                    )))
                }
            };
            let state = radio_state(&field, &export)?;
            set_button_state(doc, &field, field_ref, field_dict, &state)?;
        }
        FieldType::ComboBox | FieldType::ListBox => {
            let mut values = match value {
                FieldValue::Choice(v) => v,
                FieldValue::Text(s) | FieldValue::State(s) => vec![s],
                FieldValue::Bool(_) => {
                    return Err(Error::Unsupported(format!(
                        "« {} » est une liste : donner une valeur",
                        field.name
                    )))
                }
            };
            if !field.flags.multi_select && values.len() > 1 {
                values.truncate(1);
            }
            let mut indices = Vec::new();
            for v in &mut values {
                let found = field
                    .options
                    .iter()
                    .position(|o| o.export == *v)
                    .or_else(|| field.options.iter().position(|o| o.display == *v));
                match found {
                    Some(i) => {
                        v.clone_from(&field.options[i].export);
                        indices.push(i);
                    }
                    None if field.flags.edit || field.options.is_empty() => {}
                    None => {
                        return Err(Error::Unsupported(format!(
                            "« {v} » n'est pas une option de « {} » ({})",
                            field.name,
                            field
                                .options
                                .iter()
                                .map(|o| o.export.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )))
                    }
                }
            }
            let v_obj = if values.len() == 1 {
                Object::String(encode_text(&values[0]))
            } else {
                Object::Array(
                    values
                        .iter()
                        .map(|s| Object::String(encode_text(s)))
                        .collect(),
                )
            };
            field_dict.insert(Name::new("V"), v_obj);
            if field.kind == FieldType::ListBox && !indices.is_empty() {
                indices.sort_unstable();
                field_dict.insert(
                    Name::new("I"),
                    Object::Array(
                        indices
                            .iter()
                            .map(|i| Object::Integer(i64::try_from(*i).unwrap_or(0)))
                            .collect(),
                    ),
                );
            } else {
                field_dict.remove(&Name::new("I"));
            }
            doc.set(field_ref, Object::Dict(field_dict));
        }
        FieldType::Button | FieldType::Signature | FieldType::Unknown => {
            return Err(Error::Unsupported(format!(
                "le champ « {} » ({}) ne se remplit pas",
                field.name,
                field.kind.label()
            )))
        }
    }
    // Relecture (valeur héritée fusionnée) puis apparences.
    let fields = list_fields(doc)?;
    let field = find_field(&fields, &field.name)?.clone();
    regenerate_appearances(doc, &field)?;
    set_need_appearances(doc, false)
}

/// Nom d'état correspondant à une valeur d'export d'un groupe radio :
/// `/Opt[i]` → état « coché » du i-ième widget, sinon l'état lui-même.
fn radio_state(field: &Field, export: &str) -> Result<String> {
    if export == "Off" {
        return Ok("Off".into());
    }
    if let Some(i) = field.options.iter().position(|o| o.export == export) {
        if let Some(on) = field.widgets.get(i).and_then(|w| w.on_state.clone()) {
            return Ok(on);
        }
    }
    if field
        .widgets
        .iter()
        .any(|w| w.on_state.as_deref() == Some(export))
    {
        return Ok(export.to_string());
    }
    let choices: Vec<String> = if field.options.is_empty() {
        field
            .widgets
            .iter()
            .filter_map(|w| w.on_state.clone())
            .collect()
    } else {
        field.options.iter().map(|o| o.export.clone()).collect()
    };
    Err(Error::Unsupported(format!(
        "« {export} » n'est pas un choix de « {} » ({})",
        field.name,
        choices.join(", ")
    )))
}

/// Écrit `/V` sur le champ et `/AS` sur chaque widget d'un bouton.
fn set_button_state(
    doc: &Document,
    field: &Field,
    field_ref: ObjectRef,
    mut field_dict: Dict,
    state: &str,
) -> Result<()> {
    if state != "Off"
        && !field.widgets.is_empty()
        && !field
            .widgets
            .iter()
            .any(|w| w.states.is_empty() || w.on_state.as_deref() == Some(state))
    {
        return Err(Error::Unsupported(format!(
            "« {state} » n'est pas un état de « {} » ({})",
            field.name,
            field
                .widgets
                .iter()
                .filter_map(|w| w.on_state.clone())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    field_dict.insert(Name::new("V"), Object::Name(Name::new(state)));
    let merged_widget = field.widgets.len() == 1 && field.widgets[0].reference == Some(field_ref);
    if merged_widget {
        field_dict.insert(Name::new("AS"), Object::Name(Name::new(state)));
    }
    doc.set(field_ref, Object::Dict(field_dict));
    if merged_widget {
        return Ok(());
    }
    for w in &field.widgets {
        let Some(r) = w.reference else { continue };
        let Some(mut wd) = doc.get(r)?.as_dict().cloned() else {
            continue;
        };
        let on = w.states.is_empty() || w.on_state.as_deref() == Some(state);
        wd.insert(
            Name::new("AS"),
            Object::Name(Name::new(if on { state } else { "Off" })),
        );
        doc.set(r, Object::Dict(wd));
    }
    Ok(())
}

fn set_need_appearances(doc: &Document, value: bool) -> Result<()> {
    if let Some((mut form, reference)) = acroform(doc)? {
        if form.get(&Name::new("NeedAppearances")) == Some(&Object::Bool(value)) {
            return Ok(());
        }
        form.insert(Name::new("NeedAppearances"), Object::Bool(value));
        write_acroform(doc, form, reference)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Apparences (§12.7.4.3).
// ---------------------------------------------------------------------------

/// Apparence par défaut décodée (`/DA`, §12.7.4.3).
#[derive(Debug, Clone)]
struct DefaultAppearance {
    font: String,
    size: f64,
    /// Opérateurs de couleur de remplissage (`0 g`, `r g b rg`, `c m y k k`).
    color: String,
}

fn parse_da(da: &str) -> DefaultAppearance {
    let mut out = DefaultAppearance {
        font: HELV.into(),
        size: 0.0,
        color: "0 g".into(),
    };
    let tokens: Vec<&str> = da.split_whitespace().collect();
    for (i, t) in tokens.iter().enumerate() {
        match *t {
            "Tf" if i >= 2 => {
                out.font = tokens[i - 2].trim_start_matches('/').to_string();
                out.size = tokens[i - 1].parse().unwrap_or(0.0);
            }
            "g" if i >= 1 => out.color = format!("{} g", tokens[i - 1]),
            "rg" if i >= 3 => out.color = tokens[(i - 3)..=i].join(" "),
            "k" if i >= 4 => out.color = tokens[(i - 4)..=i].join(" "),
            _ => {}
        }
    }
    if !out.size.is_finite() || out.size < 0.0 {
        out.size = 0.0;
    }
    out
}

/// Couleur `/MK` (`/BG`, `/BC`) en opérateur de remplissage ou de contour ;
/// tableau vide = transparent (§12.5.6.19 table 189).
fn mk_color(values: &[f64], stroke: bool) -> Option<String> {
    let op = match (values.len(), stroke) {
        (1, false) => "g",
        (1, true) => "G",
        (3, false) => "rg",
        (3, true) => "RG",
        (4, false) => "k",
        (4, true) => "K",
        _ => return None,
    };
    let nums: Vec<String> = values.iter().map(|v| fmt(*v)).collect();
    Some(format!("{} {op}", nums.join(" ")))
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.3}");
    let t = s.trim_end_matches('0').trim_end_matches('.');
    if t.is_empty() || t == "-0" {
        "0".into()
    } else {
        t.to_string()
    }
}

/// Bordure et fond d'un widget.
struct BoxStyle {
    width: f64,
    height: f64,
    /// Matrice de rotation `/MK /R`.
    matrix: Option<[f64; 6]>,
    border_width: f64,
    /// `/BS /S` : S, D, B, I, U.
    border_style: char,
    dash: Vec<f64>,
    background: Option<String>,
    border: Option<String>,
}

fn box_style(doc: &Document, widget: &Dict, rect: Rect) -> BoxStyle {
    let mk = dict_of(doc, widget, "MK").unwrap_or_default();
    let bs = dict_of(doc, widget, "BS").unwrap_or_default();
    let border = numbers_of(doc, &mk, "BC").and_then(|c| mk_color(&c, true));
    let background = numbers_of(doc, &mk, "BG").and_then(|c| mk_color(&c, false));
    let mut border_width = doc
        .dict_get(&bs, "W")
        .ok()
        .flatten()
        .and_then(|w| w.as_f64())
        .unwrap_or(1.0)
        .clamp(0.0, 100.0);
    if border.is_none() {
        border_width = 0.0;
    }
    let border_style = name_of(doc, &bs, "S")
        .and_then(|s| s.chars().next())
        .unwrap_or('S');
    let dash = numbers_of(doc, &bs, "D").unwrap_or_else(|| vec![3.0]);
    let (mut width, mut height) = (rect.width().abs(), rect.height().abs());
    let rotation = int_of(doc, &mk, "R").unwrap_or(0).rem_euclid(360);
    let matrix = match rotation {
        90 => {
            std::mem::swap(&mut width, &mut height);
            Some([0.0, 1.0, -1.0, 0.0, rect.width().abs(), 0.0])
        }
        180 => Some([
            -1.0,
            0.0,
            0.0,
            -1.0,
            rect.width().abs(),
            rect.height().abs(),
        ]),
        270 => {
            std::mem::swap(&mut width, &mut height);
            Some([0.0, -1.0, 1.0, 0.0, 0.0, rect.height().abs()])
        }
        _ => None,
    };
    BoxStyle {
        width,
        height,
        matrix,
        border_width,
        border_style,
        dash,
        background,
        border,
    }
}

impl BoxStyle {
    /// Fond puis bordure (rectangle ou cercle), à la façon d'Acrobat :
    /// biseau / enfoncé avec deux teintes de gris, souligné, tirets.
    #[allow(clippy::many_single_char_names, clippy::too_many_lines)] // coordonnées ; un bloc par style
    fn frame(&self, circle: bool) -> String {
        let (w, h, bw) = (self.width, self.height, self.border_width);
        let mut s = String::new();
        if circle {
            let r = w.min(h) / 2.0;
            let (cx, cy) = (w / 2.0, h / 2.0);
            if let Some(bg) = &self.background {
                let _ = writeln!(s, "{bg} {} f", circle_path(cx, cy, r));
            }
            if let (Some(bc), true) = (&self.border, bw > 0.0) {
                let _ = writeln!(
                    s,
                    "{bc} {} w {} S",
                    fmt(bw),
                    circle_path(cx, cy, r - bw / 2.0)
                );
                if matches!(self.border_style, 'B' | 'I') {
                    let (light, dark) = if self.border_style == 'B' {
                        ("1 G", "0.5 G")
                    } else {
                        ("0.5 G", "0.75 G")
                    };
                    let rr = r - bw * 1.5;
                    let _ = writeln!(
                        s,
                        "{light} {} w {} S {dark} {} S",
                        fmt(bw),
                        arc_path(cx, cy, rr, 45.0, 225.0),
                        arc_path(cx, cy, rr, 225.0, 405.0)
                    );
                }
            }
            return s;
        }
        if let Some(bg) = &self.background {
            let _ = writeln!(s, "{bg} 0 0 {} {} re f", fmt(w), fmt(h));
        }
        let Some(bc) = &self.border else {
            return s;
        };
        if bw <= 0.0 {
            return s;
        }
        match self.border_style {
            'U' => {
                let _ = writeln!(
                    s,
                    "{bc} {} w 0 {} m {} {} l S",
                    fmt(bw),
                    fmt(bw / 2.0),
                    fmt(w),
                    fmt(bw / 2.0)
                );
            }
            'D' => {
                let dash: Vec<String> = self.dash.iter().map(|d| fmt(*d)).collect();
                let _ = writeln!(
                    s,
                    "{bc} {} w [{}] 0 d {} {} {} {} re S",
                    fmt(bw),
                    dash.join(" "),
                    fmt(bw / 2.0),
                    fmt(bw / 2.0),
                    fmt(w - bw),
                    fmt(h - bw)
                );
            }
            style => {
                let _ = writeln!(
                    s,
                    "{bc} {} w {} {} {} {} re S",
                    fmt(bw),
                    fmt(bw / 2.0),
                    fmt(bw / 2.0),
                    fmt(w - bw),
                    fmt(h - bw)
                );
                if matches!(style, 'B' | 'I') {
                    // Bandes intérieures : haut-gauche claire, bas-droite foncée.
                    let (light, dark) = if style == 'B' {
                        ("1 g", "0.5 g")
                    } else {
                        ("0.5 g", "0.75 g")
                    };
                    let (a, b) = (bw, 2.0 * bw);
                    let _ = writeln!(
                        s,
                        "{light} {a} {a} m {a} {hb} l {wb} {hb} l {wb2} {hb2} l {b} {hb2} l {b} {b} l h f",
                        a = fmt(a),
                        b = fmt(b),
                        hb = fmt(h - a),
                        wb = fmt(w - a),
                        wb2 = fmt(w - b),
                        hb2 = fmt(h - b)
                    );
                    let _ = writeln!(
                        s,
                        "{dark} {wb} {hb} m {wb} {a} l {a} {a} l {b} {b} l {wb2} {b} l {wb2} {hb2} l h f",
                        a = fmt(a),
                        b = fmt(b),
                        hb = fmt(h - a),
                        wb = fmt(w - a),
                        wb2 = fmt(w - b),
                        hb2 = fmt(h - b)
                    );
                }
            }
        }
        s
    }

    /// Marge intérieure du texte (bordure + 2 pt, comme Acrobat).
    fn padding(&self) -> f64 {
        self.border_width + 2.0
    }
}

/// Cercle en quatre courbes de Bézier (κ = 0,5523).
fn circle_path(cx: f64, cy: f64, r: f64) -> String {
    let k = 0.5523 * r;
    format!(
        "{} {} m {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c h",
        fmt(cx + r),
        fmt(cy),
        fmt(cx + r),
        fmt(cy + k),
        fmt(cx + k),
        fmt(cy + r),
        fmt(cx),
        fmt(cy + r),
        fmt(cx - k),
        fmt(cy + r),
        fmt(cx - r),
        fmt(cy + k),
        fmt(cx - r),
        fmt(cy),
        fmt(cx - r),
        fmt(cy - k),
        fmt(cx - k),
        fmt(cy - r),
        fmt(cx),
        fmt(cy - r),
        fmt(cx + k),
        fmt(cy - r),
        fmt(cx + r),
        fmt(cy - k),
        fmt(cx + r),
        fmt(cy)
    )
}

/// Arc approché par segments (pour les biseaux circulaires).
fn arc_path(cx: f64, cy: f64, r: f64, from_deg: f64, to_deg: f64) -> String {
    let mut s = String::new();
    let steps = 12;
    for i in 0..=steps {
        let t = from_deg + (to_deg - from_deg) * f64::from(i) / f64::from(steps);
        let (sn, cs) = t.to_radians().sin_cos();
        let _ = write!(
            s,
            "{} {} {}",
            fmt(cx + r * cs),
            fmt(cy + r * sn),
            if i == 0 { "m " } else { "l " }
        );
    }
    s
}

// --- Métriques de texte -----------------------------------------------------

/// Largeurs Helvetica (AFM Adobe), codes 32 à 126.
const HELVETICA_WIDTHS: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

/// Largeurs Helvetica-Bold, codes 32 à 126.
const HELVETICA_BOLD_WIDTHS: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, 975, 722, 722, 722, 722, 667,
    611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 333, 278, 333, 584, 556, 333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556,
    278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
];

/// Largeurs Times-Roman, codes 32 à 126.
const TIMES_WIDTHS: [u16; 95] = [
    250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444, 921, 722, 667, 667, 722, 611,
    556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667, 556, 611, 722, 722, 944, 722,
    722, 611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500,
    278, 778, 500, 500, 500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541,
];

/// Métriques d'une police d'apparence (largeurs pour 1000 unités).
enum Metrics {
    Table(&'static [u16; 95]),
    Fixed(f64),
    /// `/Widths` explicites du dictionnaire de police.
    Explicit {
        first: u32,
        widths: Vec<f64>,
        missing: f64,
    },
}

impl Metrics {
    fn for_font(doc: &Document, font: &Dict) -> Self {
        if let Some(w) = numbers_of(doc, font, "Widths") {
            if !w.is_empty() {
                let missing = dict_of(doc, font, "FontDescriptor")
                    .and_then(|fd| doc.dict_get(&fd, "MissingWidth").ok().flatten()?.as_f64())
                    .unwrap_or(0.0);
                return Metrics::Explicit {
                    first: u32::try_from(int_of(doc, font, "FirstChar").unwrap_or(0)).unwrap_or(0),
                    widths: w,
                    missing,
                };
            }
        }
        let base = name_of(doc, font, "BaseFont")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if base.contains("courier") || base.contains("mono") {
            Metrics::Fixed(600.0)
        } else if base.contains("times") || base.contains("serif") || base.contains("georgia") {
            Metrics::Table(&TIMES_WIDTHS)
        } else if base.contains("bold") {
            Metrics::Table(&HELVETICA_BOLD_WIDTHS)
        } else {
            Metrics::Table(&HELVETICA_WIDTHS)
        }
    }

    /// Largeur d'un caractère en unités texte (déjà divisée par 1000).
    fn width(&self, c: char) -> f64 {
        match self {
            Metrics::Fixed(w) => w / 1000.0,
            Metrics::Table(t) => {
                let b = base_letter(c) as u32;
                let i = b.saturating_sub(32) as usize;
                f64::from(t.get(i).copied().unwrap_or(556)) / 1000.0
            }
            Metrics::Explicit {
                first,
                widths,
                missing,
            } => {
                let code = u32::from(win_ansi_byte(c));
                let w = code
                    .checked_sub(*first)
                    .and_then(|i| widths.get(i as usize).copied())
                    .unwrap_or(*missing);
                w / 1000.0
            }
        }
    }

    fn text_width(&self, s: &str) -> f64 {
        s.chars().map(|c| self.width(c)).sum()
    }
}

/// Lettre de base d'un caractère accentué (pour les tables de largeurs).
fn base_letter(c: char) -> char {
    match c {
        'À'..='Å' => 'A',
        'Ç' => 'C',
        'Æ' | 'È'..='Ë' | '€' => 'E',
        'Ì'..='Ï' => 'I',
        'Ñ' => 'N',
        'Ò'..='Ö' | 'Ø' => 'O',
        'Ù'..='Ü' => 'U',
        'Ý' => 'Y',
        'à'..='å' => 'a',
        'ç' => 'c',
        'è'..='ë' => 'e',
        'ì'..='ï' => 'i',
        'ñ' => 'n',
        'ò'..='ö' | 'ø' | '\u{2022}' => 'o',
        'ù'..='ü' => 'u',
        'ý' | 'ÿ' => 'y',
        'ß' => 'B',
        'æ' | 'œ' | '\u{2026}' => 'm',
        'Œ' => 'M',
        '\u{2018}' | '\u{2019}' => '\'',
        '\u{201C}' | '\u{201D}' => '"',
        '\u{2013}' | '\u{2014}' => '-',
        c if c.is_ascii() => c,
        _ => 'n',
    }
}

/// Octet WinAnsiEncoding (annexe D) d'un caractère ; `?` si absent.
fn win_ansi_byte(c: char) -> u8 {
    match c {
        '\u{20}'..='\u{7E}' | '\u{A0}'..='\u{FF}' => u8::try_from(c as u32).unwrap_or(b'?'),
        '€' => 0x80,
        '\u{201A}' => 0x82,
        'ƒ' => 0x83,
        '\u{201E}' => 0x84,
        '\u{2026}' => 0x85,
        '†' => 0x86,
        '‡' => 0x87,
        'ˆ' => 0x88,
        '‰' => 0x89,
        'Š' => 0x8A,
        '\u{2039}' => 0x8B,
        'Œ' => 0x8C,
        'Ž' => 0x8E,
        '\u{2018}' => 0x91,
        '\u{2019}' => 0x92,
        '\u{201C}' => 0x93,
        '\u{201D}' => 0x94,
        '\u{2022}' => 0x95,
        '\u{2013}' => 0x96,
        '\u{2014}' => 0x97,
        '˜' => 0x98,
        '™' => 0x99,
        'š' => 0x9A,
        '\u{203A}' => 0x9B,
        'œ' => 0x9C,
        'ž' => 0x9E,
        'Ÿ' => 0x9F,
        _ => b'?',
    }
}

/// Chaîne littérale PDF (échappée, octets non ASCII en octal) d'un texte
/// encodé en WinAnsi.
fn pdf_literal(s: &str) -> String {
    let mut out = String::from("(");
    for c in s.chars() {
        let b = win_ansi_byte(c);
        match b {
            b'(' | b')' | b'\\' => {
                out.push('\\');
                out.push(b as char);
            }
            0x20..=0x7E => out.push(b as char),
            _ => {
                let _ = write!(out, "\\{b:03o}");
            }
        }
    }
    out.push(')');
    out
}

/// Police d'apparence : nom de ressource, référence, métriques.
///
/// `embedded` n'est renseigné que si la valeur du champ sort de
/// WinAnsiEncoding : une police système est alors incorporée en sous-ensemble,
/// ce qui permet de remplir un formulaire en japonais ou en russe au lieu
/// d'afficher des « ? ».
struct AppearanceFont {
    resource_name: String,
    reference: ObjectRef,
    metrics: Metrics,
    embedded: Option<Box<crate::fontembed::EmbeddedFont>>,
}

impl AppearanceFont {
    /// Opérande à écrire devant `Tj`.
    fn show(&self, text: &str) -> String {
        match &self.embedded {
            Some(f) => format!("<{}>", f.show(text)),
            None => pdf_literal(text),
        }
    }

    /// Largeur d'un caractère, en em.
    fn width(&self, c: char) -> f64 {
        match &self.embedded {
            Some(f) => f.width(&c.to_string(), 1.0),
            None => self.metrics.width(c),
        }
    }

    /// Largeur d'un texte, en em.
    fn text_width(&self, text: &str) -> f64 {
        match &self.embedded {
            Some(f) => f.width(text, 1.0),
            None => self.metrics.text_width(text),
        }
    }
}

/// Police de `/DA` dans `/AcroForm /DR /Font` ; Helvetica ajoutée si
/// absente ou composite.
fn appearance_font(doc: &Document, da_font: &str, text: &str) -> Result<AppearanceFont> {
    if !crate::fontembed::fits_winansi(text) {
        if let Ok(embedded) =
            crate::fontembed::embed_text_font(doc, text, crate::fontembed::FontStyle::Regular)
        {
            let reference = embedded.reference;
            let name = format!("AKU{}", reference.number);
            register_font(doc, &name, reference)?;
            return Ok(AppearanceFont {
                resource_name: name,
                reference,
                metrics: Metrics::Fixed(500.0),
                embedded: Some(Box::new(embedded)),
            });
        }
    }
    let (mut form, form_ref) = acroform(doc)?.unwrap_or_default();
    let mut dr = dict_of(doc, &form, "DR").unwrap_or_default();
    let mut fonts = dict_of(doc, &dr, "Font").unwrap_or_default();
    if let Some(Object::Reference(r)) = fonts.get(&Name::new(da_font)) {
        if let Some(fd) = doc.get(*r)?.as_dict() {
            if name_of(doc, fd, "Subtype").as_deref() != Some("Type0") {
                return Ok(AppearanceFont {
                    resource_name: da_font.to_string(),
                    reference: *r,
                    metrics: Metrics::for_font(doc, fd),
                    embedded: None,
                });
            }
        }
    }
    // Helvetica ajoutée sous /Helv (ou réutilisée si déjà là).
    let helv_ref = if let Some(Object::Reference(r)) = fonts.get(&Name::new(HELV)) {
        *r
    } else {
        let mut f = Dict::new();
        f.insert(Name::new("Type"), Object::Name(Name::new("Font")));
        f.insert(Name::new("Subtype"), Object::Name(Name::new("Type1")));
        f.insert(Name::new("BaseFont"), Object::Name(Name::new("Helvetica")));
        f.insert(
            Name::new("Encoding"),
            Object::Name(Name::new("WinAnsiEncoding")),
        );
        let r = doc.add(Object::Dict(f));
        fonts.insert(Name::new(HELV), Object::Reference(r));
        dr.insert(Name::new("Font"), Object::Dict(fonts));
        form.insert(Name::new("DR"), Object::Dict(dr));
        write_acroform(doc, form, form_ref)?;
        r
    };
    Ok(AppearanceFont {
        resource_name: HELV.into(),
        reference: helv_ref,
        metrics: Metrics::Table(&HELVETICA_WIDTHS),
        embedded: None,
    })
}

/// Inscrit une police dans `/AcroForm /DR /Font` sous le nom donné.
fn register_font(doc: &Document, name: &str, reference: ObjectRef) -> Result<()> {
    let (mut form, form_ref) = acroform(doc)?.unwrap_or_default();
    let mut dr = dict_of(doc, &form, "DR").unwrap_or_default();
    let mut fonts = dict_of(doc, &dr, "Font").unwrap_or_default();
    if fonts.contains_key(&Name::new(name)) {
        return Ok(());
    }
    fonts.insert(Name::new(name), Object::Reference(reference));
    dr.insert(Name::new("Font"), Object::Dict(fonts));
    form.insert(Name::new("DR"), Object::Dict(dr));
    write_acroform(doc, form, form_ref)
}

/// Flux d'apparence (`/Type /XObject /Subtype /Form`).
fn form_xobject(style: &BoxStyle, content: String, font: Option<&AppearanceFont>) -> Object {
    let mut d = Dict::new();
    d.insert(Name::new("Type"), Object::Name(Name::new("XObject")));
    d.insert(Name::new("Subtype"), Object::Name(Name::new("Form")));
    d.insert(Name::new("FormType"), Object::Integer(1));
    d.insert(
        Name::new("BBox"),
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Real(style.width),
            Object::Real(style.height),
        ]),
    );
    if let Some(m) = style.matrix {
        d.insert(
            Name::new("Matrix"),
            Object::Array(m.iter().map(|v| Object::Real(*v)).collect()),
        );
    }
    if let Some(f) = font {
        let mut fonts = Dict::new();
        fonts.insert(Name::new(&f.resource_name), Object::Reference(f.reference));
        let mut res = Dict::new();
        res.insert(Name::new("Font"), Object::Dict(fonts));
        d.insert(Name::new("Resources"), Object::Dict(res));
    }
    let raw = content.into_bytes();
    d.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
    );
    Object::Stream { dict: d, raw }
}

/// Ligne de texte alignée (`/Q`) dans une largeur.
fn aligned_x(quadding: i64, text_width: f64, x0: f64, avail: f64) -> f64 {
    match quadding {
        1 => x0 + (avail - text_width) / 2.0,
        2 => x0 + avail - text_width,
        _ => x0,
    }
}

/// Coupe un texte en lignes tenant dans `avail` (coupure aux espaces, puis
/// au caractère pour les mots trop longs).
fn wrap_lines(text: &str, metrics: &Metrics, size: f64, avail: f64) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split(['\n', '\r']) {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if metrics.text_width(&candidate) * size <= avail
                || (line.is_empty() && word.is_empty())
            {
                line = candidate;
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            // Mot seul trop long : coupé au caractère.
            let mut chunk = String::new();
            for c in word.chars() {
                let next = format!("{chunk}{c}");
                if metrics.text_width(&next) * size > avail && !chunk.is_empty() {
                    lines.push(std::mem::take(&mut chunk));
                }
                chunk.push(c);
            }
            line = chunk;
        }
        lines.push(line);
    }
    lines
}

/// Hauteur d'ascendante et de descendante (Helvetica : 718 / 207).
const ASCENT: f64 = 0.718;
const DESCENT: f64 = 0.207;

/// Contenu d'un champ de texte ou d'une liste déroulante.
#[allow(
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    clippy::too_many_lines
)] // cases du peigne ; coordonnées ; un bloc par mode (peigne, multiligne, une ligne)
fn text_content(
    style: &BoxStyle,
    font: &AppearanceFont,
    da: &DefaultAppearance,
    field: &Field,
    text: &str,
) -> String {
    let (w, h) = (style.width, style.height);
    let pad = style.padding();
    let (inner_w, inner_h) = ((w - 2.0 * pad).max(1.0), (h - 2.0 * pad).max(1.0));
    let mut s = style.frame(false);
    let comb_cells = if field.flags.comb {
        field.max_len.filter(|n| *n > 0)
    } else {
        None
    };
    if let (Some(cells), Some(bc), true) = (comb_cells, &style.border, style.border_width > 0.0) {
        // Séparateurs de cases du peigne, dans la couleur de bordure (comme Acrobat).
        let cell = w / cells as f64;
        let _ = write!(s, "{bc} {} w [] 0 d ", fmt(style.border_width));
        for i in 1..cells {
            let x = fmt(cell * i as f64);
            let _ = write!(s, "{x} 0 m {x} {} l ", fmt(h));
        }
        s.push_str("S\n");
    }
    let display: String = if field.flags.password {
        text.chars().map(|_| '*').collect()
    } else {
        text.to_string()
    };
    if display.is_empty() {
        return s;
    }
    let _ = write!(
        s,
        "/Tx BMC\nq {} {} {} {} re W n BT {} /{} ",
        fmt(style.border_width),
        fmt(style.border_width),
        fmt(w - 2.0 * style.border_width),
        fmt(h - 2.0 * style.border_width),
        da.color,
        font.resource_name
    );
    if let Some(cells) = comb_cells {
        // Peigne : une case par caractère, centré (§12.7.5.3).
        let cell = w / cells as f64;
        let size = if da.size > 0.0 {
            da.size
        } else {
            (inner_h / 1.35).min(cell / 0.7).max(4.0)
        };
        let y = (h - size * (ASCENT + DESCENT)) / 2.0 + size * DESCENT;
        let _ = write!(s, "{} Tf ", fmt(size));
        let mut prev_x = 0.0;
        let mut first = true;
        for (i, c) in display.chars().take(cells).enumerate() {
            let cw = font.width(c) * size;
            let x = cell * i as f64 + (cell - cw) / 2.0;
            if first {
                let _ = write!(s, "{} {} Td ", fmt(x), fmt(y));
                first = false;
            } else {
                let _ = write!(s, "{} 0 Td ", fmt(x - prev_x));
            }
            let _ = write!(s, "{} Tj ", font.show(&c.to_string()));
            prev_x = x;
        }
        s.push_str("ET Q\nEMC\n");
        return s;
    }
    if field.flags.multiline {
        let size = if da.size > 0.0 { da.size } else { 12.0 };
        let leading = size * 1.15;
        let lines = wrap_lines(&display, &font.metrics, size, inner_w);
        let _ = write!(s, "{} Tf {} TL ", fmt(size), fmt(leading));
        let mut y = h - pad - size * ASCENT;
        let mut prev_x = 0.0;
        let mut first = true;
        for line in lines {
            if y < -size {
                break;
            }
            let tw = font.text_width(&line) * size;
            let x = aligned_x(field.quadding, tw, pad, inner_w);
            if first {
                let _ = write!(s, "{} {} Td ", fmt(x), fmt(y));
                first = false;
            } else {
                let _ = write!(s, "{} {} Td ", fmt(x - prev_x), fmt(-leading));
            }
            let _ = write!(s, "{} Tj ", font.show(&line));
            prev_x = x;
            y -= leading;
        }
        s.push_str("ET Q\nEMC\n");
        return s;
    }
    // Une ligne : taille automatique = hauteur, réduite pour tenir en largeur.
    let natural = font.text_width(&display);
    let size = if da.size > 0.0 {
        da.size
    } else {
        let by_height = inner_h / 1.35;
        let by_width = if natural > 0.0 {
            inner_w / natural
        } else {
            by_height
        };
        by_height.min(by_width).max(4.0)
    };
    let tw = natural * size;
    let x = aligned_x(field.quadding, tw, pad, inner_w);
    let y = (h - size * (ASCENT + DESCENT)) / 2.0 + size * DESCENT;
    let _ = writeln!(
        s,
        "{} Tf {} {} Td {} Tj ET Q\nEMC",
        fmt(size),
        fmt(x),
        fmt(y),
        font.show(&display)
    );
    s
}

/// Contenu d'une liste de choix : options de haut en bas, sélection surlignée.
#[allow(clippy::many_single_char_names)] // coordonnées
fn list_content(
    style: &BoxStyle,
    font: &AppearanceFont,
    da: &DefaultAppearance,
    field: &Field,
    selected: &[String],
    top_index: usize,
) -> String {
    let (w, h) = (style.width, style.height);
    let pad = style.padding();
    let size = if da.size > 0.0 { da.size } else { 12.0 };
    let leading = size * 1.15;
    let mut s = style.frame(false);
    let _ = writeln!(
        s,
        "/Tx BMC\nq {} {} {} {} re W n",
        fmt(style.border_width),
        fmt(style.border_width),
        fmt(w - 2.0 * style.border_width),
        fmt(h - 2.0 * style.border_width)
    );
    let mut top = h - pad;
    let mut text_ops = String::new();
    let mut first = true;
    let mut prev_x = 0.0;
    let mut prev_y = 0.0;
    for opt in field.options.iter().skip(top_index) {
        if top < 0.0 {
            break;
        }
        if selected.contains(&opt.export) {
            // Bleu de sélection d'Acrobat (153, 193, 218).
            let _ = writeln!(
                s,
                "0.6 0.757 0.854 rg {} {} {} {} re f",
                fmt(style.border_width),
                fmt(top - leading),
                fmt(w - 2.0 * style.border_width),
                fmt(leading)
            );
        }
        let tw = font.text_width(&opt.display) * size;
        let x = aligned_x(field.quadding, tw, pad, w - 2.0 * pad);
        let y = top - size * ASCENT - (leading - size) / 2.0;
        if first {
            let _ = write!(text_ops, "{} {} Td ", fmt(x), fmt(y));
            first = false;
        } else {
            let _ = write!(text_ops, "{} {} Td ", fmt(x - prev_x), fmt(y - prev_y));
        }
        let _ = write!(text_ops, "{} Tj ", font.show(&opt.display));
        prev_x = x;
        prev_y = y;
        top -= leading;
    }
    if !first {
        let _ = writeln!(
            s,
            "BT {} /{} {} Tf {text_ops}ET",
            da.color,
            font.resource_name,
            fmt(size)
        );
    }
    s.push_str("Q\nEMC\n");
    s
}

/// Symbole d'une case cochée ou d'un radio sélectionné, d'après le
/// caractère ZapfDingbats de `/MK /CA` : `4` coche, `l` cercle, `8` croix,
/// `u` losange, `n` carré, `H` étoile (§12.7.5.2.3).
fn mark_content(style: &BoxStyle, symbol: char, color: &str) -> String {
    let (w, h) = (style.width, style.height);
    let side = (w.min(h) - 2.0 * style.padding()).max(1.0);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let mut s = String::from("q ");
    let stroke_color = color
        .replace(" rg", " RG")
        .replace(" g", " G")
        .replace(" k", " K");
    match symbol {
        'l' => {
            let _ = writeln!(s, "{color} {} f", circle_path(cx, cy, side * 0.28));
        }
        '8' => {
            let r = side * 0.32;
            let _ = writeln!(
                s,
                "{stroke_color} {} w 1 J {} {} m {} {} l {} {} m {} {} l S",
                fmt(side * 0.12),
                fmt(cx - r),
                fmt(cy - r),
                fmt(cx + r),
                fmt(cy + r),
                fmt(cx - r),
                fmt(cy + r),
                fmt(cx + r),
                fmt(cy - r)
            );
        }
        'u' => {
            let r = side * 0.36;
            let _ = writeln!(
                s,
                "{color} {} {} m {} {} l {} {} l {} {} l h f",
                fmt(cx),
                fmt(cy + r),
                fmt(cx + r),
                fmt(cy),
                fmt(cx),
                fmt(cy - r),
                fmt(cx - r),
                fmt(cy)
            );
        }
        'n' => {
            let r = side * 0.27;
            let _ = writeln!(
                s,
                "{color} {} {} {} {} re f",
                fmt(cx - r),
                fmt(cy - r),
                fmt(2.0 * r),
                fmt(2.0 * r)
            );
        }
        'H' => {
            let (outer, inner) = (side * 0.4, side * 0.16);
            let _ = write!(s, "{color} ");
            for i in 0..10 {
                let r = if i % 2 == 0 { outer } else { inner };
                let angle = (90.0 + 36.0 * f64::from(i)).to_radians();
                let _ = write!(
                    s,
                    "{} {} {} ",
                    fmt(cx + r * angle.cos()),
                    fmt(cy + r * angle.sin()),
                    if i == 0 { "m" } else { "l" }
                );
            }
            s.push_str("h f");
        }
        _ => {
            // Coche : trait épais aux extrémités rondes.
            let _ = writeln!(
                s,
                "{stroke_color} {} w 1 J 1 j {} {} m {} {} l {} {} l S",
                fmt(side * 0.14),
                fmt(cx - side * 0.32),
                fmt(cy + side * 0.02),
                fmt(cx - side * 0.08),
                fmt(cy - side * 0.25),
                fmt(cx + side * 0.35),
                fmt(cy + side * 0.28)
            );
        }
    }
    s.push_str(" Q\n");
    s
}

/// Régénère `/AP /N` de chaque widget du champ d'après sa valeur courante.
///
/// # Errors
/// Widget ou formulaire illisible.
#[allow(clippy::too_many_lines)] // une branche par type de champ
pub fn regenerate_appearances(doc: &Document, field: &Field) -> Result<()> {
    if matches!(
        field.kind,
        FieldType::Button | FieldType::Signature | FieldType::Unknown
    ) {
        return Ok(());
    }
    let da = parse_da(
        field
            .default_appearance
            .as_deref()
            .unwrap_or("/Helv 0 Tf 0 g"),
    );
    let needs_font = matches!(
        field.kind,
        FieldType::Text | FieldType::ComboBox | FieldType::ListBox
    );
    let font = if needs_font {
        // Tout ce qui pourra être dessiné : la valeur et, pour une liste, les
        // intitulés des options.
        let mut shown = match &field.value {
            Some(FieldValue::Text(t)) => t.clone(),
            Some(FieldValue::Choice(v)) => v.join(" "),
            _ => String::new(),
        };
        for option in &field.options {
            shown.push_str(&option.display);
            shown.push(' ');
        }
        Some(appearance_font(doc, &da.font, &shown)?)
    } else {
        None
    };
    for (i, widget) in field.widgets.iter().enumerate() {
        let Some(r) = widget.reference else { continue };
        let Some(mut wd) = doc.get(r)?.as_dict().cloned() else {
            continue;
        };
        let style = box_style(doc, &wd, widget.rect);
        let mk = dict_of(doc, &wd, "MK").unwrap_or_default();
        let mut ap = Dict::new();
        match field.kind {
            FieldType::Text => {
                let text = match &field.value {
                    Some(FieldValue::Text(t)) => t.clone(),
                    Some(other) => other.to_display(),
                    None => String::new(),
                };
                let f = font
                    .as_ref()
                    .ok_or_else(|| Error::Corrupt("police".into()))?;
                let content = text_content(&style, f, &da, field, &text);
                let stream = doc.add(form_xobject(&style, content, Some(f)));
                ap.insert(Name::new("N"), Object::Reference(stream));
            }
            FieldType::ComboBox => {
                let export = match &field.value {
                    Some(FieldValue::Choice(v)) => v.first().cloned().unwrap_or_default(),
                    Some(other) => other.to_display(),
                    None => String::new(),
                };
                let display = field
                    .options
                    .iter()
                    .find(|o| o.export == export)
                    .map_or(export.clone(), |o| o.display.clone());
                let f = font
                    .as_ref()
                    .ok_or_else(|| Error::Corrupt("police".into()))?;
                let content = text_content(&style, f, &da, field, &display);
                let stream = doc.add(form_xobject(&style, content, Some(f)));
                ap.insert(Name::new("N"), Object::Reference(stream));
            }
            FieldType::ListBox => {
                let selected = match &field.value {
                    Some(FieldValue::Choice(v)) => v.clone(),
                    Some(other) => vec![other.to_display()],
                    None => Vec::new(),
                };
                let top = usize::try_from(int_of(doc, &wd, "TI").unwrap_or(0)).unwrap_or(0);
                let f = font
                    .as_ref()
                    .ok_or_else(|| Error::Corrupt("police".into()))?;
                let content = list_content(&style, f, &da, field, &selected, top);
                let stream = doc.add(form_xobject(&style, content, Some(f)));
                ap.insert(Name::new("N"), Object::Reference(stream));
            }
            FieldType::CheckBox | FieldType::Radio => {
                let is_radio = field.kind == FieldType::Radio;
                let symbol = text_of(doc, &mk, "CA")
                    .and_then(|c| c.chars().next())
                    .unwrap_or(if is_radio { 'l' } else { '4' });
                let circle = is_radio && symbol == 'l';
                let frame = style.frame(circle);
                let on_name = widget.on_state.clone().unwrap_or_else(|| {
                    field
                        .options
                        .get(i)
                        .map_or_else(|| "Yes".to_string(), |o| o.export.clone())
                });
                let off = doc.add(form_xobject(&style, frame.clone(), None));
                let on = doc.add(form_xobject(
                    &style,
                    format!("{frame}{}", mark_content(&style, symbol, &da.color)),
                    None,
                ));
                let mut states = Dict::new();
                states.insert(Name::new(&on_name), Object::Reference(on));
                states.insert(Name::new("Off"), Object::Reference(off));
                ap.insert(Name::new("N"), Object::Dict(states));
                let current = widget.state.clone().unwrap_or_else(|| "Off".into());
                let as_name = if current == on_name {
                    on_name
                } else {
                    "Off".into()
                };
                wd.insert(Name::new("AS"), Object::Name(Name::new(&as_name)));
            }
            FieldType::Button | FieldType::Signature | FieldType::Unknown => {}
        }
        wd.insert(Name::new("AP"), Object::Dict(ap));
        doc.set(r, Object::Dict(wd));
    }
    Ok(())
}

/// Régénère les apparences de tous les champs remplissables (utile pour un
/// formulaire livré avec `/NeedAppearances true`).
///
/// # Errors
/// Formulaire illisible.
pub fn regenerate_all(doc: &Document) -> Result<usize> {
    let fields = list_fields(doc)?;
    let mut n = 0;
    for f in &fields {
        if matches!(
            f.kind,
            FieldType::Text
                | FieldType::CheckBox
                | FieldType::Radio
                | FieldType::ComboBox
                | FieldType::ListBox
        ) {
            regenerate_appearances(doc, f)?;
            n += 1;
        }
    }
    if n > 0 {
        set_need_appearances(doc, false)?;
    }
    Ok(n)
}

// ---------------------------------------------------------------------------
// Aplatissement.
// ---------------------------------------------------------------------------

/// Apparence normale d'un widget selon `/AS` (référence ou flux direct).
fn normal_appearance(doc: &Document, annot: &Dict) -> Option<Object> {
    let ap = dict_of(doc, annot, "AP")?;
    let n = ap.get(&Name::new("N"))?;
    match (*doc.resolve(n).ok()?).clone() {
        Object::Stream { .. } => Some(n.clone()),
        Object::Dict(states) => {
            let state = name_of(doc, annot, "AS");
            let chosen = match state {
                Some(s) => states.get(&Name::new(&s)).cloned(),
                None if states.len() == 1 => states.values().next().cloned(),
                None => None,
            }?;
            matches!(&*doc.resolve(&chosen).ok()?, Object::Stream { .. }).then_some(chosen)
        }
        _ => None,
    }
}

/// Matrice plaçant le flux d'apparence dans `/Rect` (§12.5.5, algorithme 8.1).
fn placement_matrix(doc: &Document, stream: &Dict, rect: Rect) -> Matrix {
    let bbox = numbers_of(doc, stream, "BBox").unwrap_or_default();
    let m = numbers_of(doc, stream, "Matrix").unwrap_or_default();
    let matrix = if m.len() == 6 {
        Matrix::new(m[0], m[1], m[2], m[3], m[4], m[5])
    } else {
        Matrix::IDENTITY
    };
    if bbox.len() != 4 {
        return Matrix::IDENTITY;
    }
    let tb = matrix.transform_rect(&Rect::new(bbox[0], bbox[1], bbox[2], bbox[3]));
    let sx = if tb.width() > 1e-9 {
        rect.width() / tb.width()
    } else {
        1.0
    };
    let sy = if tb.height() > 1e-9 {
        rect.height() / tb.height()
    } else {
        1.0
    };
    Matrix::new(sx, 0.0, 0.0, sy, rect.x0 - tb.x0 * sx, rect.y0 - tb.y0 * sy)
}

fn plain_stream(content: &[u8]) -> Object {
    let mut d = Dict::new();
    d.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(content.len()).unwrap_or(0)),
    );
    Object::Stream {
        dict: d,
        raw: content.to_vec(),
    }
}

/// Aplatit le formulaire : l'apparence de chaque widget est dessinée dans le
/// contenu de sa page (XObject `Do`), les widgets sont retirés de `/Annots`
/// et `/AcroForm` est supprimé du catalogue. Retourne le nombre de widgets
/// dessinés.
///
/// # Errors
/// Pages ou catalogue illisibles.
pub fn flatten_fields(doc: &Document) -> Result<usize> {
    let mut drawn = 0;
    let mut counter = 0usize;
    for page in collect_pages(doc)? {
        let Some(page_ref) = page.reference else {
            continue;
        };
        let Some(annots) = page.dict.get(&Name::new("Annots")) else {
            continue;
        };
        let list = doc
            .resolve(annots)?
            .as_array()
            .map(<[Object]>::to_vec)
            .unwrap_or_default();
        let mut kept = Vec::new();
        let mut xobjects = Dict::new();
        let mut drawing = String::new();
        for a in &list {
            let Ok(resolved) = doc.resolve(a) else {
                kept.push(a.clone());
                continue;
            };
            let Some(annot) = resolved.as_dict() else {
                kept.push(a.clone());
                continue;
            };
            if name_of(doc, annot, "Subtype").as_deref() != Some("Widget") {
                kept.push(a.clone());
                continue;
            }
            let flags = int_of(doc, annot, "F").unwrap_or(0);
            if flags & (ANNOT_HIDDEN | ANNOT_NOVIEW) == 0 {
                if let Some(ap) = normal_appearance(doc, annot) {
                    let ap_ref = match ap {
                        Object::Reference(r) => r,
                        stream => doc.add(stream),
                    };
                    let stream_dict = doc.get(ap_ref)?.as_dict().cloned().unwrap_or_default();
                    let m = placement_matrix(doc, &stream_dict, rect_of(doc, annot));
                    counter += 1;
                    let name = format!("AkForm{counter}");
                    xobjects.insert(Name::new(&name), Object::Reference(ap_ref));
                    let _ = writeln!(
                        drawing,
                        "q {} {} {} {} {} {} cm /{name} Do Q",
                        fmt(m.a),
                        fmt(m.b),
                        fmt(m.c),
                        fmt(m.d),
                        fmt(m.e),
                        fmt(m.f)
                    );
                    drawn += 1;
                }
            }
            if let Object::Reference(r) = a {
                doc.delete(*r);
            }
        }
        if kept.len() == list.len() {
            continue;
        }
        let mut page_dict = page.dict.clone();
        if kept.is_empty() {
            page_dict.remove(&Name::new("Annots"));
        } else {
            page_dict.insert(Name::new("Annots"), Object::Array(kept));
        }
        if !xobjects.is_empty() {
            let mut resources = dict_of(doc, &page_dict, "Resources").unwrap_or_default();
            let mut existing = dict_of(doc, &resources, "XObject").unwrap_or_default();
            existing.extend(xobjects);
            resources.insert(Name::new("XObject"), Object::Dict(existing));
            page_dict.insert(Name::new("Resources"), Object::Dict(resources));
            // Contenu existant encadré par q … Q, puis les widgets par-dessus.
            let mut contents = vec![Object::Reference(doc.add(plain_stream(b"q\n")))];
            match page_dict.get(&Name::new("Contents")) {
                Some(Object::Array(items)) => contents.extend(items.iter().cloned()),
                Some(Object::Reference(r)) => match &*doc.get(*r)? {
                    Object::Array(items) => contents.extend(items.iter().cloned()),
                    _ => contents.push(Object::Reference(*r)),
                },
                _ => {}
            }
            let tail = format!("Q\n{drawing}");
            contents.push(Object::Reference(doc.add(plain_stream(tail.as_bytes()))));
            page_dict.insert(Name::new("Contents"), Object::Array(contents));
        }
        doc.set(page_ref, Object::Dict(page_dict));
    }
    // Plus de formulaire.
    let mut catalog = doc.catalog()?;
    if catalog.remove(&Name::new("AcroForm")).is_some() {
        doc.set(catalog_ref(doc)?, Object::Dict(catalog));
    }
    Ok(drawn)
}

// ---------------------------------------------------------------------------
// FDF (§12.7.8).
// ---------------------------------------------------------------------------

fn value_object(field: &Field) -> Option<Object> {
    match field.value.as_ref()? {
        FieldValue::Text(s) => Some(Object::String(encode_text(s))),
        FieldValue::State(s) => Some(Object::Name(Name::new(s))),
        FieldValue::Bool(b) => Some(Object::Name(Name::new(if *b { "Yes" } else { "Off" }))),
        FieldValue::Choice(v) if v.len() == 1 => Some(Object::String(encode_text(&v[0]))),
        FieldValue::Choice(v) => Some(Object::Array(
            v.iter().map(|s| Object::String(encode_text(s))).collect(),
        )),
    }
}

/// Exporte les valeurs des champs (sauf `NoExport`, boutons et signatures)
/// en FDF : `/FDF << /Fields [ << /T (nom qualifié) /V valeur >> … ] >>`.
#[must_use]
pub fn export_fdf(doc: &Document) -> Vec<u8> {
    let fields = list_fields(doc).unwrap_or_default();
    let mut entries = Vec::new();
    for f in &fields {
        if f.flags.no_export || matches!(f.kind, FieldType::Button | FieldType::Signature) {
            continue;
        }
        let Some(v) = value_object(f) else { continue };
        let mut d = Dict::new();
        d.insert(Name::new("T"), Object::String(encode_text(&f.name)));
        d.insert(Name::new("V"), v);
        entries.push(Object::Dict(d));
    }
    let mut fdf = Dict::new();
    fdf.insert(Name::new("Fields"), Object::Array(entries));
    let mut root = Dict::new();
    root.insert(Name::new("FDF"), Object::Dict(fdf));
    let mut out = b"%FDF-1.2\n%\xE2\xE3\xCF\xD3\n1 0 obj".to_vec();
    writer::write_object(&Object::Dict(root), &mut out);
    out.extend_from_slice(b"\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF");
    out
}

/// Objets indirects d'un fichier FDF, et son trailer.
fn parse_fdf(data: &[u8]) -> Result<(HashMap<u32, Object>, Dict)> {
    let mut objects = HashMap::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if &data[i..i + 3] == b"obj" && data.get(i + 3).is_none_or(|b| !b.is_ascii_alphanumeric()) {
            // Recule sur « n g » : espaces, génération, espaces, numéro.
            let mut j = i;
            let skip_ws = |j: &mut usize| {
                while *j > 0 && data[*j - 1].is_ascii_whitespace() {
                    *j -= 1;
                }
            };
            let skip_digits = |j: &mut usize| -> bool {
                let start = *j;
                while *j > 0 && data[*j - 1].is_ascii_digit() {
                    *j -= 1;
                }
                *j < start
            };
            skip_ws(&mut j);
            let ok = j < i && skip_digits(&mut j) && {
                let before = j;
                skip_ws(&mut j);
                j < before && skip_digits(&mut j)
            };
            if ok && (j == 0 || !data[j - 1].is_ascii_alphanumeric()) {
                let mut parser = Parser::at(data, j);
                if let Ok((r, obj)) = parser.parse_indirect(&|_| None) {
                    objects.insert(r.number, obj);
                    i = parser.pos().max(i + 3);
                    continue;
                }
            }
        }
        i += 1;
    }
    let trailer = data
        .windows(7)
        .position(|w| w == b"trailer")
        .and_then(|p| Parser::at(data, p + 7).parse_object().ok())
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_default();
    if objects.is_empty() {
        return Err(Error::Corrupt("aucun objet FDF".into()));
    }
    Ok((objects, trailer))
}

/// Importe les valeurs d'un fichier FDF (champs plats `/T` qualifiés ou
/// hiérarchie `/Kids`). Retourne le nombre de champs mis à jour ; les noms
/// inconnus, les champs en lecture seule et les valeurs refusées sont ignorés.
///
/// # Errors
/// FDF illisible ou sans `/FDF /Fields`, document illisible.
pub fn import_fdf(doc: &Document, data: &[u8]) -> Result<usize> {
    let (objects, trailer) = parse_fdf(data)?;
    let resolve = |o: &Object| -> Object {
        match o {
            Object::Reference(r) => objects.get(&r.number).cloned().unwrap_or(Object::Null),
            other => other.clone(),
        }
    };
    let root = match trailer.get(&Name::new("Root")) {
        Some(r) => resolve(r),
        None => objects
            .values()
            .find(|o| {
                o.as_dict()
                    .is_some_and(|d| d.contains_key(&Name::new("FDF")))
            })
            .cloned()
            .unwrap_or(Object::Null),
    };
    let fields = root
        .as_dict()
        .and_then(|d| resolve(d.get(&Name::new("FDF"))?).as_dict().cloned())
        .and_then(|fdf| {
            resolve(fdf.get(&Name::new("Fields"))?)
                .as_array()
                .map(<[Object]>::to_vec)
        })
        .ok_or_else(|| Error::Corrupt("FDF sans /FDF /Fields".into()))?;
    let mut values: Vec<(String, Object)> = Vec::new();
    collect_fdf_fields(&fields, "", &resolve, &mut values, 0);
    let existing = list_fields(doc)?;
    let mut count = 0;
    for (name, v) in values {
        let Ok(field) = find_field(&existing, &name) else {
            continue;
        };
        let as_text = |o: &Object| -> Option<String> {
            match o {
                Object::String(s) => Some(decode_text_string(s)),
                Object::Name(n) => Some(n.as_str()),
                _ => None,
            }
        };
        let value = match (field.kind, &v) {
            (FieldType::CheckBox | FieldType::Radio, o) => as_text(o).map(FieldValue::State),
            (FieldType::ComboBox | FieldType::ListBox, Object::Array(items)) => Some(
                FieldValue::Choice(items.iter().filter_map(|i| as_text(&resolve(i))).collect()),
            ),
            (FieldType::ComboBox | FieldType::ListBox, o) => {
                as_text(o).map(|s| FieldValue::Choice(vec![s]))
            }
            (_, o) => as_text(o).map(FieldValue::Text),
        };
        let Some(value) = value else { continue };
        if set_field_value(doc, &field.name, value).is_ok() {
            count += 1;
        }
    }
    Ok(count)
}

fn collect_fdf_fields(
    items: &[Object],
    prefix: &str,
    resolve: &dyn Fn(&Object) -> Object,
    out: &mut Vec<(String, Object)>,
    depth: usize,
) {
    if depth > 32 {
        return;
    }
    for item in items {
        let Some(d) = resolve(item).as_dict().cloned() else {
            continue;
        };
        let t = match d.get(&Name::new("T")).map(resolve) {
            Some(Object::String(s)) => decode_text_string(&s),
            _ => String::new(),
        };
        let name = if prefix.is_empty() {
            t
        } else if t.is_empty() {
            prefix.to_string()
        } else {
            format!("{prefix}.{t}")
        };
        if let Some(Object::Array(kids)) = d.get(&Name::new("Kids")).map(resolve) {
            collect_fdf_fields(&kids, &name, resolve, out, depth + 1);
        }
        if let Some(v) = d.get(&Name::new("V")) {
            out.push((name, resolve(v)));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::too_many_lines)]
mod tests {
    use super::*;
    use acrux_render::{render_page, RenderOptions};

    /// PDF de synthèse : page 420 × 400 avec étiquettes, et un formulaire
    /// exerçant chaque type de champ.
    fn build_pdf(objects: &[(u32, String)]) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3".to_vec();
        let mut offsets: Vec<(u32, usize)> = Vec::new();
        for (n, body) in objects {
            offsets.push((*n, out.len()));
            out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj").as_bytes());
        }
        let max = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f ", max + 1).as_bytes());
        for n in 1..=max {
            match offsets.iter().find(|(m, _)| *m == n) {
                Some((_, o)) => out.extend_from_slice(format!("{o:010} 00000 n ").as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f "),
            }
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF",
                max + 1
            )
            .as_bytes(),
        );
        out
    }

    fn stream(dict: &str, content: &str) -> String {
        format!(
            "<< {dict} /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        )
    }

    fn sample_pdf() -> Vec<u8> {
        let labels = [
            (30.0, 346.0, "Nom :"),
            (30.0, 306.0, "Code postal :"),
            (30.0, 266.0, "Mot de passe :"),
            (30.0, 236.0, "Adresse :"),
            (30.0, 156.0, "Ville :"),
            (30.0, 114.0, "Abonn\\351 :"),
            (30.0, 84.0, "Taille :"),
            (172.0, 84.0, "S"),
            (222.0, 84.0, "M"),
            (272.0, 84.0, "L"),
            (30.0, 46.0, "Pays :"),
            (310.0, 134.0, "Langues :"),
        ];
        let mut content = String::from("BT /F1 10 Tf 0 g\n");
        let mut prev = (0.0, 0.0);
        for (x, y, text) in labels {
            let _ = writeln!(content, "{} {} Td ({text}) Tj", x - prev.0, y - prev.1);
            prev = (x, y);
        }
        content.push_str("ET\n0.5 G 0.5 w 20 20 m 400 20 l S");
        let widget = |ft: &str, t: &str, rect: &str, extra: &str| -> String {
            format!("<< /Type /Annot /Subtype /Widget /F 4 /P 3 0 R /FT /{ft} /T ({t}) /Rect [{rect}] {extra} >>")
        };
        let objects: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 10 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 420 400] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> /Annots [11 0 R 12 0 R 14 0 R 15 0 R 25 0 R 16 0 R 17 0 R 18 0 R 20 0 R 21 0 R 22 0 R 23 0 R] >>".into(),
            ),
            (4, stream("", &content)),
            (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into()),
            (
                10,
                "<< /Fields [11 0 R 12 0 R 13 0 R 16 0 R 17 0 R 19 0 R 21 0 R 22 0 R 24 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 5 0 R >> >> /NeedAppearances true >>".into(),
            ),
            // Texte simple, taille automatique, fond et bordure.
            (
                11,
                widget("Tx", "nom", "150 340 390 362", "/TU (Nom de famille) /V (Dupont) /DA (/Helv 0 Tf 0 0 0.5 rg) /MaxLen 40 /MK << /BG [0.9 0.92 1] /BC [0 0 0.5] >> /BS << /W 1 /S /S >>"),
            ),
            // Case à cocher (coche), apparences vides à régénérer.
            (
                12,
                widget("Btn", "abonne", "150 108 168 126", "/V /Off /AS /Off /MK << /CA (4) /BC [0] /BG [1] >> /AP << /N << /Oui 30 0 R /Off 31 0 R >> >>"),
            ),
            // Groupe radio S / M / L (style cercle) ; M coché.
            (
                13,
                "<< /FT /Btn /Ff 32768 /T (taille) /V /M /Kids [14 0 R 15 0 R 25 0 R] /DA (/Helv 0 Tf 0 g) >>".into(),
            ),
            (
                14,
                "<< /Type /Annot /Subtype /Widget /F 4 /P 3 0 R /Parent 13 0 R /Rect [150 78 166 94] /AS /Off /MK << /CA (l) /BC [0] /BG [1] >> /AP << /N << /S 32 0 R /Off 33 0 R >> >> >>".into(),
            ),
            (
                15,
                "<< /Type /Annot /Subtype /Widget /F 4 /P 3 0 R /Parent 13 0 R /Rect [200 78 216 94] /AS /M /MK << /CA (l) /BC [0] /BG [1] >> /AP << /N << /M 32 0 R /Off 33 0 R >> >> >>".into(),
            ),
            (
                25,
                "<< /Type /Annot /Subtype /Widget /F 4 /P 3 0 R /Parent 13 0 R /Rect [250 78 266 94] /AS /Off /MK << /CA (l) /BC [0] /BG [1] >> /AP << /N << /L 32 0 R /Off 33 0 R >> >> >>".into(),
            ),
            // Liste déroulante avec paire export / affichage, bordure biseautée.
            (
                16,
                widget("Ch", "pays", "150 36 300 58", "/Ff 131072 /Opt [(France) (Belgique) [(CH) (Suisse)]] /V (France) /MK << /BG [0.85] /BC [0.4] >> /BS << /W 1 /S /B >>"),
            ),
            // Liste à sélection multiple.
            (
                17,
                widget("Ch", "langues", "310 36 390 126", "/Ff 2097152 /Opt [(Fran\\347ais) (Anglais) (Espagnol) (Allemand)] /V [(Fran\\347ais)] /I [0] /DA (/Helv 9 Tf 0 g) /MK << /BC [0.4] /BG [1] >>"),
            ),
            // Champ hérité : parent « adresse » multiligne, enfants rue et ville.
            (
                19,
                "<< /T (adresse) /FT /Tx /Ff 4096 /DA (/Helv 9 Tf 0 g) /Kids [18 0 R 20 0 R] >>".into(),
            ),
            (
                18,
                "<< /Type /Annot /Subtype /Widget /F 4 /P 3 0 R /Parent 19 0 R /T (rue) /Rect [150 176 390 246] /MK << /BC [0.4] /BG [1] >> >>".into(),
            ),
            (
                20,
                "<< /Type /Annot /Subtype /Widget /F 4 /P 3 0 R /Parent 19 0 R /T (ville) /Ff 0 /Rect [150 146 390 168] /MK << /BC [0.4] /BG [1] >> >>".into(),
            ),
            // Peigne à 5 cases, bordure en tirets.
            (
                21,
                widget("Tx", "code", "150 296 250 318", "/Ff 16777216 /MaxLen 5 /Q 1 /MK << /BC [0] /BG [1] >> /BS << /W 1 /S /D /D [2 2] >>"),
            ),
            // Mot de passe.
            (
                22,
                widget("Tx", "secret", "150 256 390 278", "/Ff 8192 /MK << /BC [0.4] /BG [1] >>"),
            ),
            // Champ en lecture seule.
            (
                23,
                widget("Tx", "fige", "150 380 390 396", "/Ff 1 /V (non modifiable) /Parent 24 0 R"),
            ),
            (24, "<< /T (systeme) /Kids [23 0 R] >>".into()),
            (30, stream("/Type /XObject /Subtype /Form /BBox [0 0 18 18]", "")),
            (31, stream("/Type /XObject /Subtype /Form /BBox [0 0 18 18]", "")),
            (32, stream("/Type /XObject /Subtype /Form /BBox [0 0 16 16]", "")),
            (33, stream("/Type /XObject /Subtype /Form /BBox [0 0 16 16]", "")),
        ];
        build_pdf(&objects)
    }

    fn field<'a>(fields: &'a [Field], name: &str) -> &'a Field {
        fields.iter().find(|f| f.name == name).unwrap()
    }

    fn fill_sample(doc: &Document) {
        set_field_value(doc, "nom", FieldValue::Text("Dupont Élodie".into())).unwrap();
        set_field_value(doc, "code", FieldValue::Text("75011".into())).unwrap();
        set_field_value(doc, "secret", FieldValue::Text("secret".into())).unwrap();
        set_field_value(
            doc,
            "adresse.rue",
            FieldValue::Text(
                "12 rue des Lilas\nBâtiment B, escalier 3, troisième étage à gauche, deuxième porte à droite"
                    .into(),
            ),
        )
        .unwrap();
        set_field_value(doc, "adresse.ville", FieldValue::Text("Paris".into())).unwrap();
        set_field_value(doc, "abonne", FieldValue::Bool(true)).unwrap();
        set_field_value(doc, "taille", FieldValue::State("M".into())).unwrap();
        set_field_value(doc, "pays", FieldValue::Choice(vec!["Suisse".into()])).unwrap();
        set_field_value(
            doc,
            "langues",
            FieldValue::Choice(vec!["Français".into(), "Espagnol".into()]),
        )
        .unwrap();
    }

    fn appearance_text(doc: &Document, widget: &Widget) -> String {
        let w = doc.get(widget.reference.unwrap()).unwrap();
        let ap = dict_of(doc, w.as_dict().unwrap(), "AP").unwrap();
        let n = ap.get(&Name::new("N")).unwrap();
        let stream = match (*doc.resolve(n).unwrap()).clone() {
            Object::Dict(states) => {
                let s = widget.state.clone().unwrap();
                states.get(&Name::new(&s)).cloned().unwrap()
            }
            _ => n.clone(),
        };
        String::from_utf8_lossy(&doc.stream_data(&stream).unwrap().data).into_owned()
    }

    #[test]
    fn inventory_lists_every_field_with_inheritance() {
        let doc = Document::from_bytes(sample_pdf()).unwrap();
        let fields = list_fields(&doc).unwrap();
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "nom",
                "abonne",
                "taille",
                "pays",
                "langues",
                "adresse.rue",
                "adresse.ville",
                "code",
                "secret",
                "systeme.fige"
            ]
        );
        let nom = field(&fields, "nom");
        assert_eq!(nom.kind, FieldType::Text);
        assert_eq!(nom.value, Some(FieldValue::Text("Dupont".into())));
        assert_eq!(nom.max_len, Some(40));
        assert_eq!(nom.alternate_name.as_deref(), Some("Nom de famille"));
        assert_eq!(nom.widgets.len(), 1);
        assert_eq!(nom.widgets[0].page, Some(0));
        assert_eq!(nom.widgets[0].rect, Rect::new(150.0, 340.0, 390.0, 362.0));

        let case = field(&fields, "abonne");
        assert_eq!(case.kind, FieldType::CheckBox);
        assert_eq!(
            case.widgets[0].states,
            vec!["Off".to_string(), "Oui".into()]
        );
        assert_eq!(case.widgets[0].on_state.as_deref(), Some("Oui"));
        assert_eq!(case.widgets[0].state.as_deref(), Some("Off"));

        let taille = field(&fields, "taille");
        assert_eq!(taille.kind, FieldType::Radio);
        assert!(taille.flags.radio);
        assert_eq!(taille.widgets.len(), 3);
        assert_eq!(taille.value, Some(FieldValue::State("M".into())));
        assert_eq!(taille.widgets[1].on_state.as_deref(), Some("M"));

        let pays = field(&fields, "pays");
        assert_eq!(pays.kind, FieldType::ComboBox);
        assert_eq!(pays.options.len(), 3);
        assert_eq!(pays.options[2].export, "CH");
        assert_eq!(pays.options[2].display, "Suisse");

        let langues = field(&fields, "langues");
        assert_eq!(langues.kind, FieldType::ListBox);
        assert!(langues.flags.multi_select);
        assert_eq!(
            langues.value,
            Some(FieldValue::Choice(vec!["Français".into()]))
        );

        let rue = field(&fields, "adresse.rue");
        assert_eq!(rue.kind, FieldType::Text, "type hérité du parent");
        assert!(rue.flags.multiline, "drapeaux hérités");
        assert_eq!(rue.default_appearance.as_deref(), Some("/Helv 9 Tf 0 g"));
        let ville = field(&fields, "adresse.ville");
        assert!(!ville.flags.multiline, "/Ff local prioritaire");

        let code = field(&fields, "code");
        assert!(code.flags.comb);
        assert_eq!(code.quadding, 1);
        assert!(field(&fields, "secret").flags.password);
        let fige = field(&fields, "systeme.fige");
        assert!(fige.flags.read_only);
        assert_eq!(fige.flags.labels(), vec!["lecture seule"]);
    }

    #[test]
    fn filling_writes_values_states_and_appearances() {
        let doc = Document::from_bytes(sample_pdf()).unwrap();
        fill_sample(&doc);
        let fields = list_fields(&doc).unwrap();

        let nom = field(&fields, "nom");
        assert_eq!(nom.value, Some(FieldValue::Text("Dupont Élodie".into())));
        let ap = appearance_text(&doc, &nom.widgets[0]);
        assert!(ap.contains("/Tx BMC"), "{ap}");
        assert!(ap.contains("(Dupont \\311lodie) Tj"), "{ap}");
        assert!(ap.contains("0 0 0.5 rg"), "couleur de /DA : {ap}");
        assert!(
            ap.contains("0.9 0.92 1 rg 0 0 240 22 re f"),
            "fond /MK /BG : {ap}"
        );
        assert!(ap.contains("0 0 0.5 RG 1 w"), "bordure /MK /BC : {ap}");

        let secret = field(&fields, "secret");
        let ap = appearance_text(&doc, &secret.widgets[0]);
        assert!(ap.contains("(******) Tj"), "{ap}");

        let code = field(&fields, "code");
        let ap = appearance_text(&doc, &code.widgets[0]);
        assert_eq!(ap.matches(" Tj").count(), 5, "un Tj par case : {ap}");
        assert!(ap.contains("[2 2] 0 d"), "tirets : {ap}");

        let rue = field(&fields, "adresse.rue");
        let ap = appearance_text(&doc, &rue.widgets[0]);
        assert!(ap.matches(" Tj").count() >= 3, "retours à la ligne : {ap}");
        assert!(ap.contains("(12 rue des Lilas) Tj"), "{ap}");

        let case = field(&fields, "abonne");
        assert_eq!(case.value, Some(FieldValue::State("Oui".into())));
        assert_eq!(case.widgets[0].state.as_deref(), Some("Oui"));
        let ap = appearance_text(&doc, &case.widgets[0]);
        assert!(ap.contains("1 J 1 j"), "coche vectorielle : {ap}");

        let taille = field(&fields, "taille");
        assert_eq!(taille.value, Some(FieldValue::State("M".into())));
        let states: Vec<Option<&str>> = taille.widgets.iter().map(|w| w.state.as_deref()).collect();
        assert_eq!(states, [Some("Off"), Some("M"), Some("Off")]);
        set_field_value(&doc, "taille", FieldValue::State("L".into())).unwrap();
        let fields = list_fields(&doc).unwrap();
        let taille = field(&fields, "taille");
        let states: Vec<Option<&str>> = taille.widgets.iter().map(|w| w.state.as_deref()).collect();
        assert_eq!(states, [Some("Off"), Some("Off"), Some("L")]);
        assert!(set_field_value(&doc, "taille", FieldValue::State("XL".into())).is_err());

        let pays = field(&fields, "pays");
        assert_eq!(pays.value, Some(FieldValue::Choice(vec!["CH".into()])));
        let ap = appearance_text(&doc, &pays.widgets[0]);
        assert!(ap.contains("(Suisse) Tj"), "texte affiché : {ap}");
        assert!(ap.contains("1 g "), "biseau : {ap}");
        assert!(set_field_value(&doc, "pays", FieldValue::Choice(vec!["Mars".into()])).is_err());

        let langues = field(&fields, "langues");
        assert_eq!(
            langues.value,
            Some(FieldValue::Choice(vec![
                "Français".into(),
                "Espagnol".into()
            ]))
        );
        let d = doc.get(langues.reference.unwrap()).unwrap();
        assert_eq!(
            d.as_dict().unwrap().get(&Name::new("I")),
            Some(&Object::Array(vec![Object::Integer(0), Object::Integer(2)]))
        );
        let ap = appearance_text(&doc, &langues.widgets[0]);
        assert_eq!(ap.matches("0.6 0.757 0.854 rg").count(), 2, "{ap}");

        // Lecture seule et champ inconnu.
        let err = set_field_value(&doc, "systeme.fige", FieldValue::Text("x".into())).unwrap_err();
        assert!(err.to_string().contains("lecture seule"), "{err}");
        assert!(set_field_value(&doc, "inconnu", FieldValue::Text("x".into())).is_err());
        // Suffixe unique accepté.
        set_field_value(&doc, "ville", FieldValue::Text("Lyon".into())).unwrap();

        // /NeedAppearances repassé à false, police conservée dans /DR.
        let (form, _) = acroform(&doc).unwrap().unwrap();
        assert_eq!(
            form.get(&Name::new("NeedAppearances")),
            Some(&Object::Bool(false))
        );

        // Enregistrement puis relecture : tout persiste et se rend.
        let saved = doc.save_incremental().unwrap();
        let doc2 = Document::from_bytes(saved).unwrap();
        let fields = list_fields(&doc2).unwrap();
        assert_eq!(
            field(&fields, "adresse.ville").value,
            Some(FieldValue::Text("Lyon".into()))
        );
        let page = collect_pages(&doc2).unwrap().remove(0);
        let rendered = render_page(&doc2, &page, 1.0, &RenderOptions::default());
        // Fond bleu pâle du champ « nom » (coin intérieur, loin du texte).
        let px = rendered.bitmap.pixel(385, 400 - 345).unwrap();
        assert!(px[2] > 240 && px[0] < 240, "fond du champ nom : {px:?}");
        // Coche noire dans la case (centre-droit du trait).
        // Coche noire à l'intérieur de la case (bordure exclue).
        let dark = (153..166)
            .flat_map(|x| (111..124).map(move |y| (x, y)))
            .filter(|(x, y)| rendered.bitmap.pixel(*x, 400 - *y).unwrap()[0] < 100)
            .count();
        assert!(dark > 8, "coche visible : {dark} pixels sombres");
    }

    #[test]
    fn a_value_outside_winansi_embeds_a_font_in_the_appearance() {
        let doc = Document::from_bytes(sample_pdf()).unwrap();
        set_field_value(&doc, "nom", FieldValue::Text("田中 花子".into())).unwrap();
        // Soit une police composite a été incorporée et l'apparence écrit des
        // indices de glyphes, soit la machine n'a aucune police utilisable et
        // le repli reste le comportement d'avant.
        let mut has_type0 = false;
        let mut appearance = String::new();
        for number in doc.object_numbers() {
            let Ok(object) = doc.get(ObjectRef {
                number,
                generation: 0,
            }) else {
                continue;
            };
            if let Some(dict) = object.as_dict() {
                if matches!(dict.get(&Name::new("Subtype")), Some(Object::Name(n)) if n.0 == b"Type0")
                {
                    has_type0 = true;
                }
            }
            if matches!(&*object, Object::Stream { .. }) {
                if let Ok(data) = doc.stream_data(&object) {
                    let text = String::from_utf8_lossy(&data.data);
                    if text.contains("/Tx BMC") {
                        appearance = text.into_owned();
                    }
                }
            }
        }
        if has_type0 {
            assert!(
                appearance.contains("Tj") && appearance.contains('<'),
                "l'apparence devrait écrire des indices de glyphes : {appearance}"
            );
            assert!(
                !appearance.contains("(??"),
                "plus aucun caractère remplacé : {appearance}"
            );
        }
    }

    #[test]
    fn flatten_draws_widgets_and_removes_form() {
        let doc = Document::from_bytes(sample_pdf()).unwrap();
        fill_sample(&doc);
        let drawn = flatten_fields(&doc).unwrap();
        assert_eq!(
            drawn, 11,
            "un widget dessiné par annotation (y compris le champ figé sans apparence : ignoré)"
        );
        assert!(list_fields(&doc).unwrap().is_empty());
        assert!(!doc.catalog().unwrap().contains_key(&Name::new("AcroForm")));
        let saved = doc.save_full().unwrap();
        let doc2 = Document::from_bytes(saved).unwrap();
        let page = collect_pages(&doc2).unwrap().remove(0);
        assert!(!page.dict.contains_key(&Name::new("Annots")));
        let content = acrux_render::page::page_content(&doc2, &page);
        let text = String::from_utf8_lossy(&content);
        assert!(text.starts_with("q\n"), "contenu d'origine encadré");
        assert!(text.contains("/AkForm1 Do"), "{text}");
        assert_eq!(text.matches(" Do").count(), 11);
        let rendered = render_page(&doc2, &page, 1.0, &RenderOptions::default());
        let px = rendered.bitmap.pixel(385, 400 - 345).unwrap();
        assert!(
            px[2] > 240 && px[0] < 240,
            "fond du champ nom aplati : {px:?}"
        );
    }

    #[test]
    fn fdf_roundtrip() {
        let doc = Document::from_bytes(sample_pdf()).unwrap();
        fill_sample(&doc);
        let fdf = export_fdf(&doc);
        let text = String::from_utf8_lossy(&fdf);
        assert!(text.starts_with("%FDF-1.2"), "{text}");
        assert!(text.contains("/T (adresse.rue)"), "{text}");
        assert!(text.contains("/T (abonne) /V /Oui"), "{text}");
        assert!(text.contains("/T (langues) /V [<FEFF"), "{text}");
        assert!(text.contains("> (Espagnol)]"), "{text}");
        assert!(text.contains("/T (pays) /V (CH)"), "{text}");
        assert!(
            text.contains("/T (systeme.fige)"),
            "les champs figés s'exportent aussi"
        );

        // Import dans un formulaire vierge.
        let fresh = Document::from_bytes(sample_pdf()).unwrap();
        let n = import_fdf(&fresh, &fdf).unwrap();
        assert_eq!(n, 9, "tous sauf le champ en lecture seule");
        let fields = list_fields(&fresh).unwrap();
        assert_eq!(
            field(&fields, "nom").value,
            Some(FieldValue::Text("Dupont Élodie".into()))
        );
        assert_eq!(
            field(&fields, "abonne").value,
            Some(FieldValue::State("Oui".into()))
        );
        assert_eq!(
            field(&fields, "taille").widgets[1].state.as_deref(),
            Some("M")
        );
        assert_eq!(
            field(&fields, "langues").value,
            Some(FieldValue::Choice(vec![
                "Français".into(),
                "Espagnol".into()
            ]))
        );

        // FDF hiérarchique écrit à la main (avec /Kids et référence indirecte).
        let manual = b"%FDF-1.2\n1 0 obj << /FDF << /Fields [ << /T (adresse) /Kids [ << /T (ville) /V (Nantes) >> ] >> 2 0 R ] >> >> endobj\n2 0 obj << /T (nom) /V (Martin) >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF\n";
        assert_eq!(import_fdf(&fresh, manual).unwrap(), 2);
        let fields = list_fields(&fresh).unwrap();
        assert_eq!(
            field(&fields, "adresse.ville").value,
            Some(FieldValue::Text("Nantes".into()))
        );
        assert_eq!(
            field(&fields, "nom").value,
            Some(FieldValue::Text("Martin".into()))
        );
        assert!(import_fdf(&fresh, b"rien").is_err());
    }

    #[test]
    fn helpers_parse_da_wrap_and_encode() {
        let da = parse_da("/Helv 12 Tf 1 0 0 rg");
        assert_eq!(da.font, "Helv");
        assert!((da.size - 12.0).abs() < 1e-9);
        assert_eq!(da.color, "1 0 0 rg");
        let da = parse_da("0.5 g /TiRo 0 Tf");
        assert_eq!(da.font, "TiRo");
        assert_eq!(da.color, "0.5 g");
        assert_eq!(parse_da("").font, "Helv");

        let m = Metrics::Table(&HELVETICA_WIDTHS);
        assert!((m.text_width("Hello") - 2.278).abs() < 1e-9);
        let lines = wrap_lines("un deux trois quatre", &m, 10.0, 50.0);
        assert_eq!(lines, ["un deux", "trois", "quatre"]);
        let lines = wrap_lines("abcdefghijklmnop", &m, 10.0, 30.0);
        assert!(lines.len() > 2 && lines.concat() == "abcdefghijklmnop");
        assert_eq!(pdf_literal("a(b)\\ é€"), "(a\\(b\\)\\\\ \\351\\200)");
        assert_eq!(
            FieldValue::parse(FieldType::CheckBox, "oui"),
            FieldValue::Bool(true)
        );
        assert_eq!(
            FieldValue::parse(FieldType::ListBox, "a | b"),
            FieldValue::Choice(vec!["a".into(), "b".into()])
        );
        assert_eq!(mk_color(&[0.5], true).as_deref(), Some("0.5 G"));
        assert_eq!(mk_color(&[], false), None);
    }

    /// Génère le fichier de corpus `tests/corpus/synthese/formulaire-acroform-champs.pdf`
    /// (apparences produites par ce module). Lancer avec
    /// `cargo test -p acrux-features -- --ignored generate_forms_corpus`.
    #[test]
    #[ignore = "génère le fichier de corpus"]
    fn generate_forms_corpus() {
        let doc = Document::from_bytes(sample_pdf()).unwrap();
        fill_sample(&doc);
        let saved = doc.save_full().unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/corpus/synthese/formulaire-acroform-champs.pdf");
        std::fs::write(&path, saved).unwrap();
        println!("écrit : {}", path.display());
    }
}
