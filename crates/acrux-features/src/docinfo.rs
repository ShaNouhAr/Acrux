//! Propriétés du document : métadonnées `/Info` (§14.3.3) et XMP (§14.3.2),
//! propriétés d'ouverture (§12.2) et assainissement avant publication.
//!
//! C'est ce qu'Acrobat range dans « Propriétés du document » : les onglets
//! *Description*, *Vue initiale* et *Avancé*.
//!
//! ## Deux jeux de métadonnées pour un seul document
//!
//! Un PDF porte le plus souvent **deux** descriptions du même document : le
//! dictionnaire `/Info` du trailer, hérité de PDF 1.0, et un paquet XMP
//! (RDF/XML) dans `/Metadata` du catalogue. Rien n'oblige les deux à
//! s'accorder, et de fait ils divergent souvent : un outil met à jour l'un
//! et oublie l'autre. PDF 2.0 donne la priorité au XMP ; Acrobat, lui,
//! affiche `/Info`.
//!
//! Le choix retenu ici : [`read_metadata`] **préfère `/Info`**, complète les
//! champs absents avec le XMP, et **signale chaque divergence** dans
//! [`Metadata::divergences`] au lieu de choisir en silence.
//! [`set_metadata`] écrit **les deux**, d'un seul coup, pour qu'ils
//! s'accordent après coup.
//!
//! ## Ce qui n'est pas réécrit
//!
//! Un paquet XMP existant contient bien plus que les huit champs de
//! `/Info` : droits d'usage, historique des révisions, schémas métier. Il
//! est modifié **élément par élément** (voir [`xmp`]) et non régénéré : ce
//! qu'on ne connaît pas est conservé tel quel.

pub(crate) mod xmp;

use std::fmt::Write as _;

use acrux_core::{Error, Result};
use acrux_document::text::decode_text_string;
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef};

use crate::annotations::encode_text;
use crate::navigation::{
    action_object, action_of, destination_object, Action, Destination, PageIndex, View,
};

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

/// Date d'un document, lisible en syntaxe PDF (§7.9.4) comme en ISO 8601
/// (la forme employée par XMP).
///
/// Les champs absents d'une date partielle (« 2024 » seul) valent 1 pour le
/// mois et le jour, 0 pour l'heure : c'est ce que fait Acrobat, et cela rend
/// les comparaisons possibles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    /// Année.
    pub year: i32,
    /// Mois, de 1 à 12.
    pub month: u8,
    /// Jour, de 1 à 31.
    pub day: u8,
    /// Heure, de 0 à 23.
    pub hour: u8,
    /// Minute.
    pub minute: u8,
    /// Seconde.
    pub second: u8,
    /// Décalage par rapport à UTC en minutes ; `None` si le fichier ne le
    /// précise pas (heure locale d'un fuseau inconnu).
    pub offset_minutes: Option<i32>,
}

impl Default for Date {
    fn default() -> Self {
        Self {
            year: 1970,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            offset_minutes: None,
        }
    }
}

impl Date {
    /// Lit une date écrite dans l'une ou l'autre syntaxe.
    ///
    /// La distinction se joue sur le cinquième caractère : une date ISO
    /// 8601 y porte le tiret de `2024-01-15`, une date PDF un chiffre. Sans
    /// cette règle, `parse_pdf` accepterait « 2024-01-15T10:30 » en n'en
    /// retenant que l'année.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.starts_with("D:") {
            return Self::parse_pdf(text);
        }
        if text.as_bytes().get(4) == Some(&b'-') {
            return Self::parse_iso(text);
        }
        Self::parse_pdf(text).or_else(|| Self::parse_iso(text))
    }

    /// Lit une date PDF `D:YYYYMMDDHHmmSSOHH'mm'` (§7.9.4).
    ///
    /// Le préfixe `D:` est facultatif, comme les apostrophes du décalage :
    /// les deux manquent dans une quantité de fichiers réels.
    #[must_use]
    pub fn parse_pdf(text: &str) -> Option<Self> {
        let text = text.trim();
        let body = text.strip_prefix("D:").unwrap_or(text);
        let digits: String = body.chars().take_while(char::is_ascii_digit).collect();
        if digits.len() < 4 {
            return None;
        }
        let field = |from: usize, len: usize, default: u32| -> u32 {
            digits
                .get(from..from + len)
                .and_then(|s| s.parse().ok())
                .unwrap_or(default)
        };
        let rest = &body[digits.len()..];
        Some(Self {
            year: i32::try_from(field(0, 4, 1970)).unwrap_or(1970),
            month: clamp_u8(field(4, 2, 1), 1, 12),
            day: clamp_u8(field(6, 2, 1), 1, 31),
            hour: clamp_u8(field(8, 2, 0), 0, 23),
            minute: clamp_u8(field(10, 2, 0), 0, 59),
            second: clamp_u8(field(12, 2, 0), 0, 59),
            offset_minutes: parse_offset(rest),
        })
    }

    /// Lit une date ISO 8601 `YYYY-MM-DDTHH:MM:SS±HH:MM` (XMP, §14.3.2).
    ///
    /// Les formes tronquées (`2024`, `2024-06`, `2024-06-01`) sont acceptées,
    /// ainsi qu'une fraction de seconde, qui est ignorée.
    #[must_use]
    pub fn parse_iso(text: &str) -> Option<Self> {
        let text = text.trim();
        let (date, time) = match text.split_once(['T', 't', ' ']) {
            Some((d, t)) => (d, t),
            None => (text, ""),
        };
        let mut parts = date.split('-');
        let year: i32 = parts.next()?.parse().ok()?;
        let month = parts.next().and_then(|m| m.parse().ok()).unwrap_or(1u32);
        let day = parts.next().and_then(|d| d.parse().ok()).unwrap_or(1u32);
        // Le décalage se reconnaît à son signe ou à son « Z » final.
        let (clock, offset) = split_offset(time);
        let mut fields = clock.split(':');
        let hour = fields.next().and_then(|h| h.parse().ok()).unwrap_or(0u32);
        let minute = fields.next().and_then(|m| m.parse().ok()).unwrap_or(0u32);
        let second = fields
            .next()
            .map(|s| s.split(['.', ',']).next().unwrap_or(s))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0u32);
        Some(Self {
            year,
            month: clamp_u8(month, 1, 12),
            day: clamp_u8(day, 1, 31),
            hour: clamp_u8(hour, 0, 23),
            minute: clamp_u8(minute, 0, 59),
            second: clamp_u8(second, 0, 59),
            offset_minutes: offset,
        })
    }

    /// Rend la date en syntaxe PDF (§7.9.4).
    #[must_use]
    pub fn to_pdf(self) -> String {
        let mut out = format!(
            "D:{:04}{:02}{:02}{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        );
        match self.offset_minutes {
            None => {}
            Some(0) => out.push('Z'),
            Some(m) => {
                let sign = if m < 0 { '-' } else { '+' };
                let _ = write!(out, "{sign}{:02}'{:02}'", m.abs() / 60, m.abs() % 60);
            }
        }
        out
    }

    /// Rend la date en ISO 8601, forme employée par XMP.
    #[must_use]
    pub fn to_iso(self) -> String {
        let mut out = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        );
        match self.offset_minutes {
            None => {}
            Some(0) => out.push('Z'),
            Some(m) => {
                let sign = if m < 0 { '-' } else { '+' };
                let _ = write!(out, "{sign}{:02}:{:02}", m.abs() / 60, m.abs() % 60);
            }
        }
        out
    }
}

/// Borne un champ de date lu dans un fichier quelconque.
fn clamp_u8(value: u32, low: u8, high: u8) -> u8 {
    u8::try_from(value).unwrap_or(low).clamp(low, high)
}

/// Décalage horaire d'une date PDF : `Z`, `+HH'mm'`, `-HH'mm'`, `+HH`.
fn parse_offset(rest: &str) -> Option<i32> {
    let mut chars = rest.chars();
    let sign = match chars.next()? {
        'Z' | 'z' => return Some(0),
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let digits: Vec<u32> = chars.filter_map(|c| c.to_digit(10)).collect();
    if digits.len() < 2 {
        return None;
    }
    let hours = i32::try_from(digits[0] * 10 + digits[1]).ok()?;
    let minutes = if digits.len() >= 4 {
        i32::try_from(digits[2] * 10 + digits[3]).ok()?
    } else {
        0
    };
    Some(sign * (hours * 60 + minutes.min(59)))
}

/// Sépare `HH:MM:SS` du décalage d'une heure ISO 8601.
fn split_offset(time: &str) -> (&str, Option<i32>) {
    if let Some(clock) = time.strip_suffix(['Z', 'z']) {
        return (clock, Some(0));
    }
    // Le signe cherché est celui du décalage, pas celui d'une année négative :
    // on ne regarde que la partie horaire.
    if let Some(pos) = time.rfind(['+', '-']) {
        let (clock, offset) = time.split_at(pos);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let digits: Vec<u32> = offset.chars().filter_map(|c| c.to_digit(10)).collect();
        if digits.len() >= 2 {
            let hours = i32::try_from(digits[0] * 10 + digits[1]).unwrap_or(0);
            let minutes = if digits.len() >= 4 {
                i32::try_from(digits[2] * 10 + digits[3]).unwrap_or(0)
            } else {
                0
            };
            return (clock, Some(sign * (hours * 60 + minutes.min(59))));
        }
        return (clock, None);
    }
    (time, None)
}

// ---------------------------------------------------------------------------
// Métadonnées
// ---------------------------------------------------------------------------

/// Champ décrit à la fois par `/Info` et par XMP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// Titre : `/Title`, `dc:title`.
    Title,
    /// Auteur : `/Author`, `dc:creator`.
    Author,
    /// Sujet : `/Subject`, `dc:description`.
    Subject,
    /// Mots-clés : `/Keywords`, `pdf:Keywords`.
    Keywords,
    /// Application d'origine : `/Creator`, `xmp:CreatorTool`.
    Creator,
    /// Convertisseur PDF : `/Producer`, `pdf:Producer`.
    Producer,
    /// Date de création : `/CreationDate`, `xmp:CreateDate`.
    Created,
    /// Date de modification : `/ModDate`, `xmp:ModifyDate`.
    Modified,
}

impl Field {
    /// Tous les champs, dans l'ordre d'affichage d'Acrobat.
    pub const ALL: [Field; 8] = [
        Field::Title,
        Field::Author,
        Field::Subject,
        Field::Keywords,
        Field::Creator,
        Field::Producer,
        Field::Created,
        Field::Modified,
    ];

    /// Clé du dictionnaire `/Info`.
    #[must_use]
    pub fn info_key(self) -> &'static str {
        match self {
            Field::Title => "Title",
            Field::Author => "Author",
            Field::Subject => "Subject",
            Field::Keywords => "Keywords",
            Field::Creator => "Creator",
            Field::Producer => "Producer",
            Field::Created => "CreationDate",
            Field::Modified => "ModDate",
        }
    }

    /// Nom de la propriété XMP et sa forme RDF.
    fn xmp_property(self) -> (&'static str, xmp::Form) {
        match self {
            Field::Title => ("dc:title", xmp::Form::AltText),
            Field::Author => ("dc:creator", xmp::Form::SeqText),
            Field::Subject => ("dc:description", xmp::Form::AltText),
            Field::Keywords => ("pdf:Keywords", xmp::Form::Simple),
            Field::Creator => ("xmp:CreatorTool", xmp::Form::Simple),
            Field::Producer => ("pdf:Producer", xmp::Form::Simple),
            Field::Created => ("xmp:CreateDate", xmp::Form::Simple),
            Field::Modified => ("xmp:ModifyDate", xmp::Form::Simple),
        }
    }

    /// Nom français, tel qu'il s'affiche dans les propriétés du document.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Field::Title => "Titre",
            Field::Author => "Auteur",
            Field::Subject => "Sujet",
            Field::Keywords => "Mots-clés",
            Field::Creator => "Application",
            Field::Producer => "Convertisseur PDF",
            Field::Created => "Créé le",
            Field::Modified => "Modifié le",
        }
    }

    /// Vrai si le champ porte une date.
    #[must_use]
    pub fn is_date(self) -> bool {
        matches!(self, Field::Created | Field::Modified)
    }
}

/// Un champ dont `/Info` et le XMP ne disent pas la même chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// Champ concerné.
    pub field: Field,
    /// Valeur de `/Info` (vide si la clé y manque).
    pub info: String,
    /// Valeur du XMP (vide si la propriété y manque).
    pub xmp: String,
}

/// Métadonnées d'un document.
///
/// Un champ à `None` est **absent** ; le distinguer d'une chaîne vide
/// compte : [`set_metadata`] retire la clé dans le premier cas et écrit une
/// chaîne vide dans le second.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    /// Titre du document.
    pub title: Option<String>,
    /// Auteur.
    pub author: Option<String>,
    /// Sujet (une phrase de description).
    pub subject: Option<String>,
    /// Mots-clés, séparés par des virgules ou des espaces selon l'usage.
    pub keywords: Option<String>,
    /// Application dans laquelle le document a été créé.
    pub creator: Option<String>,
    /// Outil qui l'a converti en PDF.
    pub producer: Option<String>,
    /// Date de création.
    pub created: Option<Date>,
    /// Date de dernière modification.
    pub modified: Option<Date>,
    /// `/Trapped` : `True`, `False` ou `Unknown` (§14.11.6, recouvrement d'impression).
    pub trapped: Option<String>,
    /// Clés de `/Info` qui ne sont pas des champs normalisés, conservées
    /// telles quelles (Acrobat les affiche dans « Propriétés
    /// personnalisées »).
    pub custom: Vec<(String, String)>,
    /// Divergences relevées entre `/Info` et le XMP à la lecture.
    /// Ignoré par [`set_metadata`].
    pub divergences: Vec<Divergence>,
    /// Vrai si le document porte un paquet XMP `/Metadata`.
    /// Ignoré par [`set_metadata`].
    pub has_xmp: bool,
}

impl Metadata {
    /// Valeur textuelle d'un champ (les dates sont rendues en syntaxe PDF).
    #[must_use]
    pub fn get(&self, field: Field) -> Option<String> {
        match field {
            Field::Title => self.title.clone(),
            Field::Author => self.author.clone(),
            Field::Subject => self.subject.clone(),
            Field::Keywords => self.keywords.clone(),
            Field::Creator => self.creator.clone(),
            Field::Producer => self.producer.clone(),
            Field::Created => self.created.map(Date::to_pdf),
            Field::Modified => self.modified.map(Date::to_pdf),
        }
    }

    /// Remplace un champ. Une date illisible est ignorée pour les champs de
    /// date, plutôt que d'écrire une date fausse dans le fichier.
    pub fn set(&mut self, field: Field, value: Option<String>) {
        let date = value.as_deref().and_then(Date::parse);
        match field {
            Field::Title => self.title = value,
            Field::Author => self.author = value,
            Field::Subject => self.subject = value,
            Field::Keywords => self.keywords = value,
            Field::Creator => self.creator = value,
            Field::Producer => self.producer = value,
            Field::Created => self.created = date,
            Field::Modified => self.modified = date,
        }
    }
}

/// Clés de `/Info` traitées comme des champs normalisés.
const STANDARD_KEYS: [&str; 9] = [
    "Title",
    "Author",
    "Subject",
    "Keywords",
    "Creator",
    "Producer",
    "CreationDate",
    "ModDate",
    "Trapped",
];

/// Dictionnaire `/Info`, modifications en attente comprises.
fn info_dict(doc: &Document) -> Option<Dict> {
    match doc.trailer().get(&Name::new("Info")) {
        Some(Object::Reference(r)) => doc.get(*r).ok()?.as_dict().cloned(),
        Some(Object::Dict(d)) => Some(d.clone()),
        _ => None,
    }
}

/// Paquet XMP du catalogue, décodé en texte.
///
/// # Errors
/// Catalogue illisible.
pub fn read_xmp(doc: &Document) -> Result<Option<String>> {
    let catalog = doc.catalog()?;
    let Some(entry) = catalog.get(&Name::new("Metadata")) else {
        return Ok(None);
    };
    let Ok(stream) = doc.resolve(entry) else {
        return Ok(None);
    };
    let Ok(decoded) = doc.stream_data(&stream) else {
        return Ok(None);
    };
    Ok(Some(String::from_utf8_lossy(&decoded.data).into_owned()))
}

/// Lit les métadonnées : `/Info`, puis le paquet XMP pour compléter les
/// champs absents et relever les divergences.
///
/// # Errors
/// Catalogue illisible.
pub fn read_metadata(doc: &Document) -> Result<Metadata> {
    let info = info_dict(doc).unwrap_or_default();
    let packet = read_xmp(doc)?;
    let mut out = Metadata {
        has_xmp: packet.is_some(),
        ..Metadata::default()
    };
    let from_info = |key: &str| -> Option<String> {
        match doc.dict_get(&info, key).ok().flatten().as_deref() {
            Some(Object::String(s)) => Some(decode_text_string(s)),
            Some(Object::Name(n)) => Some(n.as_str()),
            _ => None,
        }
    };
    for field in Field::ALL {
        let (property, _) = field.xmp_property();
        let in_xmp = packet
            .as_deref()
            .and_then(|p| xmp::read_property(p, property));
        let in_info = from_info(field.info_key());
        // Comparaison sur la forme normalisée : « D:2024… » et
        // « 2024-… » désignent la même date et ne sont pas une divergence.
        let normalize = |v: &str| -> String {
            if field.is_date() {
                Date::parse(v).map_or_else(|| v.trim().to_string(), Date::to_iso)
            } else {
                v.trim().to_string()
            }
        };
        if let (Some(a), Some(b)) = (&in_info, &in_xmp) {
            if normalize(a) != normalize(b) {
                out.divergences.push(Divergence {
                    field,
                    info: a.clone(),
                    xmp: b.clone(),
                });
            }
        }
        out.set(field, in_info.or(in_xmp));
    }
    out.trapped = from_info("Trapped").or_else(|| {
        packet
            .as_deref()
            .and_then(|p| xmp::read_property(p, "pdf:Trapped"))
    });
    for (key, value) in &info {
        let name = key.as_str();
        if STANDARD_KEYS.contains(&name.as_str()) {
            continue;
        }
        if let Ok(resolved) = doc.resolve(value) {
            if let Object::String(s) = &*resolved {
                out.custom.push((name, decode_text_string(s)));
            }
        }
    }
    Ok(out)
}

/// Écrit `/Info` **et** le paquet XMP, d'un seul tenant.
///
/// Le XMP existant est modifié propriété par propriété : les schémas
/// inconnus (droits d'usage, historique, données métier) sont conservés. En
/// l'absence de XMP, un paquet neuf est produit par
/// [`crate::preflight::xmp_packet`], qui relit le `/Info` tout juste écrit.
///
/// Les champs à `None` sont **retirés** des deux côtés.
///
/// # Errors
/// Catalogue absent ou direct (rien à quoi rattacher le paquet).
pub fn set_metadata(doc: &Document, metadata: &Metadata) -> Result<()> {
    write_info(doc, metadata);
    write_xmp(doc, metadata)
}

/// Écrit le dictionnaire `/Info` (créé au besoin).
fn write_info(doc: &Document, metadata: &Metadata) {
    let mut info = info_dict(doc).unwrap_or_default();
    for field in Field::ALL {
        let key = Name::new(field.info_key());
        match metadata.get(field) {
            Some(value) => {
                info.insert(key, Object::String(encode_text(&value)));
            }
            None => {
                info.remove(&key);
            }
        }
    }
    match &metadata.trapped {
        // §14.11.6 : /Trapped est un nom, pas une chaîne.
        Some(v) => info.insert(Name::new("Trapped"), Object::Name(Name::new(v))),
        None => info.remove(&Name::new("Trapped")),
    };
    // Les clés personnalisées absentes de la liste sont retirées : c'est ce
    // que veut dire « écrire ces métadonnées-là ».
    let keep: Vec<String> = metadata.custom.iter().map(|(k, _)| k.clone()).collect();
    let stale: Vec<Name> = info
        .keys()
        .filter(|k| {
            let name = k.as_str();
            !STANDARD_KEYS.contains(&name.as_str()) && !keep.contains(&name)
        })
        .cloned()
        .collect();
    for key in stale {
        info.remove(&key);
    }
    for (key, value) in &metadata.custom {
        info.insert(Name::new(key), Object::String(encode_text(value)));
    }
    let reference = match doc.trailer().get(&Name::new("Info")) {
        Some(Object::Reference(r)) => *r,
        _ => doc.allocate(),
    };
    doc.set(reference, Object::Dict(info));
    doc.set_trailer_entry("Info", Object::Reference(reference));
}

/// Met le paquet XMP en accord avec `metadata`.
fn write_xmp(doc: &Document, metadata: &Metadata) -> Result<()> {
    let existing = read_xmp(doc)?;
    // Faute de paquet existant, on en engendre un neuf ; dans les deux cas
    // les propriétés connues sont ensuite posées une à une, de sorte que le
    // XMP dise exactement ce que dit `/Info` — y compris pour retirer le
    // `pdf:Producer` que le générateur met d'office.
    let mut packet = match existing {
        Some(packet) => packet,
        None => crate::preflight::xmp_packet(doc, None),
    };
    for field in Field::ALL {
        let (property, form) = field.xmp_property();
        let value = if field.is_date() {
            match field {
                Field::Created => metadata.created.map(Date::to_iso),
                _ => metadata.modified.map(Date::to_iso),
            }
        } else {
            metadata.get(field)
        };
        packet = xmp::set_property(&packet, property, value.as_deref(), form);
    }
    packet = xmp::set_property(
        &packet,
        "pdf:Trapped",
        metadata.trapped.as_deref(),
        xmp::Form::Simple,
    );
    write_metadata_stream(doc, packet.into_bytes())
}

/// Remplace (ou crée) le flux `/Metadata` du catalogue.
///
/// Le flux n'est ni filtré ni chiffré : un paquet XMP doit rester lisible
/// par un outil qui ne sait pas ouvrir le PDF (PDF/A-1 §6.7.3).
fn write_metadata_stream(doc: &Document, raw: Vec<u8>) -> Result<()> {
    let catalog_reference = catalog_ref(doc)?;
    let mut catalog = doc.catalog()?;
    let mut dict = Dict::new();
    dict.insert(Name::new("Type"), Object::Name(Name::new("Metadata")));
    dict.insert(Name::new("Subtype"), Object::Name(Name::new("XML")));
    dict.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
    );
    let reference = match catalog.get(&Name::new("Metadata")) {
        Some(Object::Reference(r)) => *r,
        _ => doc.allocate(),
    };
    doc.set(reference, Object::Stream { dict, raw });
    catalog.insert(Name::new("Metadata"), Object::Reference(reference));
    doc.set(catalog_reference, Object::Dict(catalog));
    Ok(())
}

/// Retire toutes les métadonnées avant publication — ce qu'Acrobat appelle
/// « assainir ».
///
/// Sont retirés : le dictionnaire `/Info`, le paquet XMP du catalogue, les
/// paquets XMP attachés aux pages, et les dictionnaires `/PieceInfo` (données
/// privées laissées par le logiciel de création, souvent bavardes).
///
/// Rend la liste de ce qui a disparu, en français, pour pouvoir l'afficher.
///
/// # Errors
/// Catalogue absent ou direct.
pub fn remove_all_metadata(doc: &Document) -> Result<Vec<String>> {
    let catalog_reference = catalog_ref(doc)?;
    let mut catalog = doc.catalog()?;
    let mut removed = Vec::new();

    if let Some(info) = info_dict(doc) {
        let present: Vec<String> = info.keys().map(Name::as_str).collect();
        if !present.is_empty() {
            removed.push(format!("/Info ({})", present.join(", ")));
        }
        if let Some(Object::Reference(r)) = doc.trailer().get(&Name::new("Info")) {
            doc.delete(*r);
        }
        doc.set_trailer_entry("Info", Object::Null);
    }
    if let Some(Object::Reference(r)) = catalog.get(&Name::new("Metadata")) {
        doc.delete(*r);
    }
    if catalog.remove(&Name::new("Metadata")).is_some() {
        removed.push("paquet XMP du catalogue".to_string());
    }
    if let Some(Object::Reference(r)) = catalog.get(&Name::new("PieceInfo")) {
        doc.delete(*r);
    }
    if catalog.remove(&Name::new("PieceInfo")).is_some() {
        removed.push("données privées /PieceInfo du catalogue".to_string());
    }
    doc.set(catalog_reference, Object::Dict(catalog));

    let mut pages_touched = 0usize;
    for page in collect_pages(doc)? {
        let Some(page_ref) = page.reference else {
            continue;
        };
        let mut dict = page.dict.clone();
        let mut changed = false;
        for key in ["Metadata", "PieceInfo"] {
            if let Some(Object::Reference(r)) = dict.get(&Name::new(key)) {
                doc.delete(*r);
            }
            changed |= dict.remove(&Name::new(key)).is_some();
        }
        if changed {
            doc.set(page_ref, Object::Dict(dict));
            pages_touched += 1;
        }
    }
    if pages_touched > 0 {
        removed.push(format!("métadonnées de {pages_touched} page(s)"));
    }
    Ok(removed)
}

/// Référence du catalogue (`/Root` du trailer).
fn catalog_ref(doc: &Document) -> Result<ObjectRef> {
    match doc.trailer().get(&Name::new("Root")) {
        Some(Object::Reference(r)) => Ok(*r),
        _ => Err(Error::Corrupt(
            "catalogue absent ou direct : impossible d'y écrire les métadonnées".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Propriétés d'ouverture (§12.2)
// ---------------------------------------------------------------------------

/// Panneau ouvert à l'affichage du document (`/PageMode`, §7.7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageMode {
    /// Aucun panneau.
    #[default]
    None,
    /// Panneau des signets.
    Outlines,
    /// Panneau des vignettes.
    Thumbnails,
    /// Plein écran, sans menus ni fenêtre.
    FullScreen,
    /// Panneau des calques (contenu optionnel).
    OptionalContent,
    /// Panneau des pièces jointes.
    Attachments,
}

impl PageMode {
    /// Nom PDF correspondant.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            PageMode::None => "UseNone",
            PageMode::Outlines => "UseOutlines",
            PageMode::Thumbnails => "UseThumbs",
            PageMode::FullScreen => "FullScreen",
            PageMode::OptionalContent => "UseOC",
            PageMode::Attachments => "UseAttachments",
        }
    }

    /// Mode d'un nom PDF ou d'un mot-clé de la ligne de commande.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.trim() {
            "UseNone" | "aucun" => PageMode::None,
            "UseOutlines" | "signets" => PageMode::Outlines,
            "UseThumbs" | "vignettes" => PageMode::Thumbnails,
            "FullScreen" | "plein-ecran" => PageMode::FullScreen,
            "UseOC" | "calques" => PageMode::OptionalContent,
            "UseAttachments" | "pieces-jointes" => PageMode::Attachments,
            _ => return None,
        })
    }

    /// Nom français.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            PageMode::None => "aucun panneau",
            PageMode::Outlines => "panneau des signets",
            PageMode::Thumbnails => "panneau des vignettes",
            PageMode::FullScreen => "plein écran",
            PageMode::OptionalContent => "panneau des calques",
            PageMode::Attachments => "panneau des pièces jointes",
        }
    }
}

/// Disposition des pages (`/PageLayout`, §7.7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageLayout {
    /// Une page à la fois.
    #[default]
    SinglePage,
    /// Défilement continu, une colonne.
    OneColumn,
    /// Deux pages côte à côte, impaires à gauche.
    TwoColumnLeft,
    /// Deux pages côte à côte, impaires à droite.
    TwoColumnRight,
    /// Deux pages à la fois, impaires à gauche.
    TwoPageLeft,
    /// Deux pages à la fois, impaires à droite.
    TwoPageRight,
}

impl PageLayout {
    /// Nom PDF correspondant.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            PageLayout::SinglePage => "SinglePage",
            PageLayout::OneColumn => "OneColumn",
            PageLayout::TwoColumnLeft => "TwoColumnLeft",
            PageLayout::TwoColumnRight => "TwoColumnRight",
            PageLayout::TwoPageLeft => "TwoPageLeft",
            PageLayout::TwoPageRight => "TwoPageRight",
        }
    }

    /// Disposition d'un nom PDF ou d'un mot-clé de la ligne de commande.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.trim() {
            "SinglePage" | "une-page" => PageLayout::SinglePage,
            "OneColumn" | "continu" => PageLayout::OneColumn,
            "TwoColumnLeft" | "deux-colonnes" => PageLayout::TwoColumnLeft,
            "TwoColumnRight" | "deux-colonnes-droite" => PageLayout::TwoColumnRight,
            "TwoPageLeft" | "deux-pages" => PageLayout::TwoPageLeft,
            "TwoPageRight" | "deux-pages-droite" => PageLayout::TwoPageRight,
            _ => return None,
        })
    }

    /// Nom français.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            PageLayout::SinglePage => "une seule page",
            PageLayout::OneColumn => "défilement continu",
            PageLayout::TwoColumnLeft => "deux colonnes, impaires à gauche",
            PageLayout::TwoColumnRight => "deux colonnes, impaires à droite",
            PageLayout::TwoPageLeft => "deux pages, impaires à gauche",
            PageLayout::TwoPageRight => "deux pages, impaires à droite",
        }
    }
}

/// Mode d'impression recto verso conseillé (`/Duplex`, §12.2 table 147).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Duplex {
    /// Recto seul.
    Simplex,
    /// Recto verso, reliure sur le petit côté.
    ShortEdge,
    /// Recto verso, reliure sur le grand côté.
    LongEdge,
}

impl Duplex {
    /// Nom PDF correspondant.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Duplex::Simplex => "Simplex",
            Duplex::ShortEdge => "DuplexFlipShortEdge",
            Duplex::LongEdge => "DuplexFlipLongEdge",
        }
    }

    /// Mode d'un nom PDF ou d'un mot-clé de la ligne de commande.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.trim() {
            "Simplex" | "recto" => Duplex::Simplex,
            "DuplexFlipShortEdge" | "recto-verso-court" => Duplex::ShortEdge,
            "DuplexFlipLongEdge" | "recto-verso-long" => Duplex::LongEdge,
            _ => return None,
        })
    }
}

/// Propriétés d'ouverture : ce que le lecteur doit faire quand le document
/// s'ouvre, et ce qu'il doit proposer à l'impression (§12.2).
// Six booléens indépendants, parce que §12.2 en définit six : les réunir
// dans un état composite ne ferait que masquer le tableau de la norme.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ViewPreferences {
    /// Panneau ouvert (`/PageMode`).
    pub page_mode: Option<PageMode>,
    /// Disposition des pages (`/PageLayout`).
    pub page_layout: Option<PageLayout>,
    /// Action d'ouverture (`/OpenAction`) : page de départ, zoom, destination.
    pub open_action: Option<Action>,
    /// Afficher le titre du document dans la barre de fenêtre plutôt que le
    /// nom de fichier (`/DisplayDocTitle`).
    pub display_document_title: bool,
    /// Masquer la barre d'outils (`/HideToolbar`).
    pub hide_toolbar: bool,
    /// Masquer la barre de menus (`/HideMenubar`).
    pub hide_menubar: bool,
    /// Masquer tout le reste de l'interface (`/HideWindowUI`).
    pub hide_window_ui: bool,
    /// Ajuster la fenêtre à la première page (`/FitWindow`).
    pub fit_window: bool,
    /// Centrer la fenêtre sur l'écran (`/CenterWindow`).
    pub center_window: bool,
    /// Mode de page hors plein écran (`/NonFullScreenPageMode`).
    pub non_full_screen_page_mode: Option<PageMode>,
    /// Impression recto verso conseillée (`/Duplex`).
    pub duplex: Option<Duplex>,
    /// Plages de pages proposées à l'impression (`/PrintPageRange`), bornes
    /// comprises, comptées à partir de 0.
    pub print_ranges: Vec<(usize, usize)>,
    /// Nombre de copies proposé (`/NumCopies`).
    pub copies: Option<i64>,
}

/// Lit les propriétés d'ouverture.
///
/// # Errors
/// Catalogue illisible.
pub fn read_view_preferences(doc: &Document) -> Result<ViewPreferences> {
    let catalog = doc.catalog()?;
    let pages = collect_pages(doc)?;
    let index = PageIndex::new(&pages);
    let mut out = ViewPreferences {
        page_mode: name_of(doc, &catalog, "PageMode").and_then(|n| PageMode::parse(&n)),
        page_layout: name_of(doc, &catalog, "PageLayout").and_then(|n| PageLayout::parse(&n)),
        ..ViewPreferences::default()
    };
    // /OpenAction est soit une destination (tableau), soit une action.
    if let Some(entry) = catalog.get(&Name::new("OpenAction")) {
        out.open_action = match doc.resolve(entry).ok().as_deref() {
            Some(Object::Dict(d)) => action_of(doc, d, &index),
            Some(other) => {
                crate::navigation::resolve_destination(doc, other, &index).map(Action::GoTo)
            }
            None => None,
        };
    }
    let Some(prefs) = doc
        .dict_get(&catalog, "ViewerPreferences")
        .ok()
        .flatten()
        .and_then(|p| p.as_dict().cloned())
    else {
        return Ok(out);
    };
    out.display_document_title = flag(doc, &prefs, "DisplayDocTitle");
    out.hide_toolbar = flag(doc, &prefs, "HideToolbar");
    out.hide_menubar = flag(doc, &prefs, "HideMenubar");
    out.hide_window_ui = flag(doc, &prefs, "HideWindowUI");
    out.fit_window = flag(doc, &prefs, "FitWindow");
    out.center_window = flag(doc, &prefs, "CenterWindow");
    out.non_full_screen_page_mode =
        name_of(doc, &prefs, "NonFullScreenPageMode").and_then(|n| PageMode::parse(&n));
    out.duplex = name_of(doc, &prefs, "Duplex").and_then(|n| Duplex::parse(&n));
    out.copies = doc
        .dict_get(&prefs, "NumCopies")
        .ok()
        .flatten()
        .and_then(|c| c.as_i64())
        .filter(|c| *c >= 1);
    if let Some(ranges) = doc.dict_get(&prefs, "PrintPageRange").ok().flatten() {
        if let Some(list) = ranges.as_array() {
            // Paires « première, dernière », comptées à partir de 1 (§12.2).
            for pair in list.chunks_exact(2) {
                let first = doc.resolve(&pair[0]).ok().and_then(|o| o.as_i64());
                let last = doc.resolve(&pair[1]).ok().and_then(|o| o.as_i64());
                if let (Some(a), Some(b)) = (first, last) {
                    if a >= 1 && b >= a {
                        let to_index = |v: i64| usize::try_from(v - 1).unwrap_or(0);
                        out.print_ranges.push((to_index(a), to_index(b)));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Nom d'une entrée de dictionnaire.
fn name_of(doc: &Document, dict: &Dict, key: &str) -> Option<String> {
    match doc.dict_get(dict, key).ok().flatten().as_deref() {
        Some(Object::Name(n)) => Some(n.as_str()),
        _ => None,
    }
}

/// Booléen d'une entrée de dictionnaire (absent = faux).
fn flag(doc: &Document, dict: &Dict, key: &str) -> bool {
    matches!(
        doc.dict_get(dict, key).ok().flatten().as_deref(),
        Some(Object::Bool(true))
    )
}

/// Écrit les propriétés d'ouverture.
///
/// Les booléens faux ne sont **pas** écrits : c'est leur valeur par défaut,
/// et les inscrire alourdirait le catalogue sans rien changer. Un
/// `/ViewerPreferences` qui se retrouve vide est retiré.
///
/// # Errors
/// Catalogue absent ou direct ; destination d'ouverture désignant une page
/// inexistante.
pub fn set_view_preferences(doc: &Document, preferences: &ViewPreferences) -> Result<()> {
    let catalog_reference = catalog_ref(doc)?;
    let mut catalog = doc.catalog()?;
    let pages = collect_pages(doc)?;

    set_name(
        &mut catalog,
        "PageMode",
        preferences.page_mode.map(PageMode::code),
    );
    set_name(
        &mut catalog,
        "PageLayout",
        preferences.page_layout.map(PageLayout::code),
    );
    match &preferences.open_action {
        None => {
            catalog.remove(&Name::new("OpenAction"));
        }
        // Une destination simple s'écrit en tableau : c'est la forme
        // qu'attendent les lecteurs et celle qu'écrit Acrobat.
        Some(Action::GoTo(destination)) => {
            let object = destination_object(&pages, destination).ok_or_else(|| {
                Error::Corrupt(format!(
                    "page {} inexistante pour l'action d'ouverture",
                    destination.page + 1
                ))
            })?;
            catalog.insert(Name::new("OpenAction"), object);
        }
        Some(action) => {
            let object = action_object(&pages, action)
                .ok_or_else(|| Error::Corrupt("action d'ouverture non prise en charge".into()))?;
            catalog.insert(Name::new("OpenAction"), object);
        }
    }

    let mut prefs = doc
        .dict_get(&catalog, "ViewerPreferences")
        .ok()
        .flatten()
        .and_then(|p| p.as_dict().cloned())
        .unwrap_or_default();
    for (key, value) in [
        ("DisplayDocTitle", preferences.display_document_title),
        ("HideToolbar", preferences.hide_toolbar),
        ("HideMenubar", preferences.hide_menubar),
        ("HideWindowUI", preferences.hide_window_ui),
        ("FitWindow", preferences.fit_window),
        ("CenterWindow", preferences.center_window),
    ] {
        if value {
            prefs.insert(Name::new(key), Object::Bool(true));
        } else {
            prefs.remove(&Name::new(key));
        }
    }
    set_name(
        &mut prefs,
        "NonFullScreenPageMode",
        preferences.non_full_screen_page_mode.map(PageMode::code),
    );
    set_name(&mut prefs, "Duplex", preferences.duplex.map(Duplex::code));
    match preferences.copies {
        Some(n) if n >= 1 => prefs.insert(Name::new("NumCopies"), Object::Integer(n)),
        _ => prefs.remove(&Name::new("NumCopies")),
    };
    if preferences.print_ranges.is_empty() {
        prefs.remove(&Name::new("PrintPageRange"));
    } else {
        let mut list = Vec::with_capacity(preferences.print_ranges.len() * 2);
        for (first, last) in &preferences.print_ranges {
            let number = |v: usize| Object::Integer(i64::try_from(v + 1).unwrap_or(1));
            list.push(number(*first));
            list.push(number(*last.max(first)));
        }
        prefs.insert(Name::new("PrintPageRange"), Object::Array(list));
    }
    if prefs.is_empty() {
        if let Some(Object::Reference(r)) = catalog.get(&Name::new("ViewerPreferences")) {
            doc.delete(*r);
        }
        catalog.remove(&Name::new("ViewerPreferences"));
    } else {
        catalog.insert(Name::new("ViewerPreferences"), Object::Dict(prefs));
    }
    doc.set(catalog_reference, Object::Dict(catalog));
    Ok(())
}

/// Écrit un nom, ou retire la clé si la valeur est absente.
fn set_name(dict: &mut Dict, key: &str, value: Option<&str>) {
    match value {
        Some(v) => dict.insert(Name::new(key), Object::Name(Name::new(v))),
        None => dict.remove(&Name::new(key)),
    };
}

/// Destination d'ouverture décrite en français, pour l'affichage.
#[must_use]
pub fn describe_open_action(action: &Action) -> String {
    match action {
        Action::GoTo(Destination { page, view }) => {
            let view = match view {
                View::Xyz { zoom: Some(z), .. } => format!(", zoom {:.0} %", z * 100.0),
                View::Xyz { .. } => String::new(),
                View::Fit => ", page entière".to_string(),
                View::FitWidth { .. } => ", pleine largeur".to_string(),
                View::FitHeight { .. } => ", pleine hauteur".to_string(),
                View::FitRect(_) => ", sur un rectangle".to_string(),
                View::FitBox => ", sur le contenu".to_string(),
            };
            format!("page {}{view}", page + 1)
        }
        Action::Uri(u) => format!("ouvre {u}"),
        Action::Named(n) => format!("action {n}"),
        Action::Launch(f) => format!("lance {f}"),
        Action::GoToRemote(f) => format!("ouvre {f}"),
        Action::Unsupported(s) => format!("action /{s}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
pub(crate) mod tests {
    use super::*;

    /// Assemble un PDF à partir des corps d'objets numérotés.
    pub(crate) fn build_pdf(objects: &[(u32, String)], extra_trailer: &str) -> Vec<u8> {
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
                "\ntrailer\n<< /Size {} /Root 1 0 R {extra_trailer} >>\nstartxref\n{}\n%%EOF",
                max + 1,
                xref + 1
            )
            .as_bytes(),
        );
        out
    }

    /// Document de trois pages, avec les entrées de catalogue données.
    pub(crate) fn doc_with(catalog_extra: &str, more: &[(u32, String)], trailer: &str) -> Document {
        let mut objects = vec![
            (
                1,
                format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
            ),
            (
                2,
                "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>".to_string(),
            ),
        ];
        for n in 3..=5 {
            objects.push((
                n,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".to_string(),
            ));
        }
        objects.extend_from_slice(more);
        Document::from_bytes(build_pdf(&objects, trailer)).unwrap()
    }

    fn reload(doc: &Document) -> Document {
        let bytes = doc
            .save_incremental()
            .or_else(|_| doc.save_full())
            .expect("enregistrement");
        Document::from_bytes(bytes).expect("relecture")
    }

    #[test]
    fn pdf_dates_round_trip_both_ways() {
        let d = Date::parse_pdf("D:20240115103045+02'00'").unwrap();
        assert_eq!(
            (d.year, d.month, d.day, d.hour, d.minute, d.second),
            (2024, 1, 15, 10, 30, 45)
        );
        assert_eq!(d.offset_minutes, Some(120));
        assert_eq!(d.to_pdf(), "D:20240115103045+02'00'");
        assert_eq!(d.to_iso(), "2024-01-15T10:30:45+02:00");
        assert_eq!(Date::parse_iso(&d.to_iso()), Some(d));
        assert_eq!(Date::parse_pdf(&d.to_pdf()), Some(d));
    }

    #[test]
    fn utc_and_partial_dates_are_understood() {
        let z = Date::parse_pdf("D:20240115103045Z").unwrap();
        assert_eq!(z.offset_minutes, Some(0));
        assert_eq!(z.to_pdf(), "D:20240115103045Z");
        assert_eq!(z.to_iso(), "2024-01-15T10:30:45Z");

        let year = Date::parse_pdf("D:2024").unwrap();
        assert_eq!((year.month, year.day, year.hour), (1, 1, 0));
        assert_eq!(year.offset_minutes, None);
        assert_eq!(year.to_pdf(), "D:20240101000000");

        // Sans « D: », sans apostrophes, décalage négatif.
        let loose = Date::parse_pdf("20231231235959-0530").unwrap();
        assert_eq!(loose.offset_minutes, Some(-330));
        assert_eq!(loose.to_iso(), "2023-12-31T23:59:59-05:30");

        let iso = Date::parse_iso("2024-06-01T08:09:10.250Z").unwrap();
        assert_eq!((iso.hour, iso.minute, iso.second), (8, 9, 10));
        assert_eq!(iso.to_pdf(), "D:20240601080910Z");
        assert_eq!(Date::parse_iso("2024-06"), Date::parse_iso("2024-06-01"));
        assert_eq!(Date::parse_pdf("bavardage"), None);
    }

    #[test]
    fn info_and_xmp_are_both_read_and_divergences_reported() {
        let packet = "<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
             <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
             <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
             <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n\
             <dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Titre XMP</rdf:li></rdf:Alt></dc:title>\n\
             <dc:creator><rdf:Seq><rdf:li>Camille</rdf:li></rdf:Seq></dc:creator>\n\
             <dc:rights><rdf:Alt><rdf:li xml:lang=\"x-default\">Tous droits</rdf:li></rdf:Alt></dc:rights>\n\
             </rdf:Description>\n</rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>";
        let doc = doc_with(
            "/Metadata 6 0 R",
            &[
                (
                    6,
                    format!(
                        "<< /Type /Metadata /Subtype /XML /Length {} >>\nstream\n{packet}\nendstream",
                        packet.len()
                    ),
                ),
                (
                    7,
                    "<< /Title (Titre Info) /Producer (Outil) /CreationDate (D:20200102030405Z) /Personnel (valeur) >>".to_string(),
                ),
            ],
            "/Info 7 0 R",
        );
        let meta = read_metadata(&doc).unwrap();
        assert!(meta.has_xmp);
        // /Info l'emporte, le XMP complète.
        assert_eq!(meta.title.as_deref(), Some("Titre Info"));
        assert_eq!(meta.author.as_deref(), Some("Camille"));
        assert_eq!(meta.producer.as_deref(), Some("Outil"));
        assert_eq!(
            meta.created.map(Date::to_iso).as_deref(),
            Some("2020-01-02T03:04:05Z")
        );
        assert_eq!(
            meta.custom,
            [("Personnel".to_string(), "valeur".to_string())]
        );
        assert_eq!(meta.divergences.len(), 1);
        assert_eq!(meta.divergences[0].field, Field::Title);
        assert_eq!(meta.divergences[0].xmp, "Titre XMP");
    }

    #[test]
    fn writing_metadata_keeps_unknown_xmp_properties() {
        let packet = "<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
             <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
             <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
             <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n\
             <dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Ancien</rdf:li></rdf:Alt></dc:title>\n\
             <dc:rights><rdf:Alt><rdf:li xml:lang=\"x-default\">© 2019 Quelqu'un</rdf:li></rdf:Alt></dc:rights>\n\
             </rdf:Description>\n\
             <rdf:Description rdf:about=\"\" xmlns:metier=\"http://exemple.test/ns/\">\n\
             <metier:dossier>A-42</metier:dossier>\n\
             </rdf:Description>\n</rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>";
        let doc = doc_with(
            "/Metadata 6 0 R",
            &[(
                6,
                format!(
                    "<< /Type /Metadata /Subtype /XML /Length {} >>\nstream\n{packet}\nendstream",
                    packet.len()
                ),
            )],
            "",
        );
        let meta = Metadata {
            title: Some("Rapport 2024".into()),
            author: Some("Camille & Cie <équipe>".into()),
            created: Date::parse_pdf("D:20240115103045+02'00'"),
            ..Metadata::default()
        };
        set_metadata(&doc, &meta).unwrap();
        let saved = reload(&doc);
        let back = read_metadata(&saved).unwrap();
        assert_eq!(back.title.as_deref(), Some("Rapport 2024"));
        assert_eq!(back.author.as_deref(), Some("Camille & Cie <équipe>"));
        assert_eq!(back.created, meta.created);
        assert!(back.divergences.is_empty(), "{:?}", back.divergences);
        // Les propriétés qu'on ne connaît pas sont toujours là.
        let packet = read_xmp(&saved).unwrap().unwrap();
        assert!(packet.contains("© 2019 Quelqu'un"), "{packet}");
        assert!(packet.contains("A-42"), "{packet}");
        assert!(packet.contains("xmlns:metier="), "{packet}");
    }

    #[test]
    fn a_document_without_xmp_gets_a_consistent_packet() {
        let doc = doc_with("", &[], "");
        let meta = Metadata {
            title: Some("Sans XMP".into()),
            subject: Some("Un sujet".into()),
            ..Metadata::default()
        };
        set_metadata(&doc, &meta).unwrap();
        let saved = reload(&doc);
        let back = read_metadata(&saved).unwrap();
        assert!(back.has_xmp);
        assert_eq!(back.title.as_deref(), Some("Sans XMP"));
        assert_eq!(back.subject.as_deref(), Some("Un sujet"));
        assert!(back.divergences.is_empty(), "{:?}", back.divergences);
    }

    #[test]
    fn removing_a_field_removes_it_everywhere() {
        let doc = doc_with("", &[], "");
        set_metadata(
            &doc,
            &Metadata {
                title: Some("À effacer".into()),
                ..Metadata::default()
            },
        )
        .unwrap();
        let doc = reload(&doc);
        set_metadata(&doc, &Metadata::default()).unwrap();
        let saved = reload(&doc);
        let back = read_metadata(&saved).unwrap();
        assert_eq!(back.title, None);
        let packet = read_xmp(&saved).unwrap().unwrap();
        assert!(!packet.contains("À effacer"), "{packet}");
    }

    #[test]
    fn sanitizing_removes_info_and_xmp() {
        let packet = "<?xpacket begin=\"\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"></x:xmpmeta>";
        let doc = doc_with(
            "/Metadata 6 0 R",
            &[
                (
                    6,
                    format!(
                        "<< /Type /Metadata /Subtype /XML /Length {} >>\nstream\n{packet}\nendstream",
                        packet.len()
                    ),
                ),
                (7, "<< /Title (Secret) /Author (Interne) >>".to_string()),
            ],
            "/Info 7 0 R",
        );
        let removed = remove_all_metadata(&doc).unwrap();
        assert_eq!(removed.len(), 2, "{removed:?}");
        let saved = reload(&doc);
        let back = read_metadata(&saved).unwrap();
        assert_eq!(back, Metadata::default());
        assert!(!saved
            .catalog()
            .unwrap()
            .contains_key(&Name::new("Metadata")));
    }

    #[test]
    fn view_preferences_round_trip() {
        let doc = doc_with("", &[], "");
        let prefs = ViewPreferences {
            page_mode: Some(PageMode::Outlines),
            page_layout: Some(PageLayout::TwoPageLeft),
            open_action: Some(Action::GoTo(Destination {
                page: 2,
                view: View::Xyz {
                    left: Some(0.0),
                    top: Some(200.0),
                    zoom: Some(1.5),
                },
            })),
            display_document_title: true,
            hide_toolbar: true,
            center_window: true,
            duplex: Some(Duplex::LongEdge),
            print_ranges: vec![(0, 1)],
            copies: Some(2),
            ..ViewPreferences::default()
        };
        set_view_preferences(&doc, &prefs).unwrap();
        let saved = reload(&doc);
        let back = read_view_preferences(&saved).unwrap();
        assert_eq!(back, prefs);
    }

    #[test]
    fn view_preferences_can_be_cleared() {
        let doc = doc_with(
            "/PageMode /UseThumbs /PageLayout /OneColumn /ViewerPreferences << /HideToolbar true >>",
            &[],
            "",
        );
        let before = read_view_preferences(&doc).unwrap();
        assert_eq!(before.page_mode, Some(PageMode::Thumbnails));
        assert!(before.hide_toolbar);
        set_view_preferences(&doc, &ViewPreferences::default()).unwrap();
        let saved = reload(&doc);
        assert_eq!(
            read_view_preferences(&saved).unwrap(),
            ViewPreferences::default()
        );
        let catalog = saved.catalog().unwrap();
        assert!(!catalog.contains_key(&Name::new("ViewerPreferences")));
        assert!(!catalog.contains_key(&Name::new("PageMode")));
    }

    #[test]
    fn an_open_action_on_a_missing_page_is_refused() {
        let doc = doc_with("", &[], "");
        let prefs = ViewPreferences {
            open_action: Some(Action::GoTo(Destination {
                page: 99,
                view: View::Fit,
            })),
            ..ViewPreferences::default()
        };
        assert!(set_view_preferences(&doc, &prefs).is_err());
    }
}
