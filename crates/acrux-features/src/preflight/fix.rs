//! Correctifs automatiques : tout ce qu'on peut réparer **sans perte** pour
//! rendre un document conforme à un profil.
//!
//! Deux briques y sont écrites de zéro, comme l'exige la charte :
//!
//! - [`xmp_packet`] : un générateur XMP minimal (RDF/XML, XMP Specification
//!   Part 1) qui déclare `pdfaid:part` / `pdfaid:conformance`, `pdfuaid:part`
//!   ou `pdfxid:GTS_PDFXVersion`, et recopie `/Info` dans `dc:` et `xmp:` ;
//! - [`srgb_icc_profile`] : un profil ICC v2 **matriciel/TRC** sRGB construit
//!   ici (primaires adaptées à D50, courbe sRGB échantillonnée), pas un
//!   fichier recopié. Il est relu par notre propre parseur dans les tests.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use acrux_core::{Error, Result};
use acrux_document::text::decode_text_string;
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef};
use acrux_fonts::{encodings, TrueTypeFont};

use super::{check_profile, PreflightIssue, Profile};

/// Ce que [`fix`] a le droit de modifier.
// Une option par correctif : la liste est plate parce que l'appelant coche
// exactement ce qu'il autorise ; un état groupé la rendrait moins lisible.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct FixOptions {
    /// Écrire ou compléter les métadonnées XMP.
    pub xmp: bool,
    /// Ajouter une intention de sortie sRVB avec son profil ICC.
    pub output_intent: bool,
    /// Incorporer les polices manquantes depuis les polices système.
    pub embed_fonts: bool,
    /// Retirer le JavaScript et les actions de lancement.
    pub remove_javascript: bool,
    /// Retirer les fichiers incorporés.
    pub remove_attachments: bool,
    /// Forcer `/ViewerPreferences /DisplayDocTitle true`.
    pub display_doc_title: bool,
    /// Mettre `/Interpolate false` sur toutes les images.
    pub interpolate: bool,
    /// Poser un `/TrimBox` égal à la `/CropBox` sur les pages qui n'en ont pas.
    pub trim_box: bool,
    /// Baliser le document s'il ne l'est pas (profil PDF/UA).
    pub tag: bool,
    /// Titre à écrire si le document n'en a pas.
    pub title: Option<String>,
}

impl Default for FixOptions {
    fn default() -> Self {
        Self {
            xmp: true,
            output_intent: true,
            embed_fonts: true,
            remove_javascript: true,
            remove_attachments: true,
            display_doc_title: true,
            interpolate: true,
            trim_box: true,
            tag: true,
            title: None,
        }
    }
}

/// Ce que [`fix`] a fait, et ce qui reste.
#[derive(Debug, Clone)]
pub struct FixReport {
    /// Corrections appliquées, en français, dans l'ordre.
    pub applied: Vec<String>,
    /// Problèmes encore présents après correction.
    pub remaining: Vec<PreflightIssue>,
}

impl FixReport {
    /// Vrai si le document est désormais conforme.
    #[must_use]
    pub fn is_conforming(&self) -> bool {
        !self
            .remaining
            .iter()
            .any(|i| i.severity == crate::accessibility::Severity::Error)
    }
}

/// Répare ce qui peut l'être, puis re-vérifie le document.
///
/// # Errors
/// Catalogue illisible ou non indirect.
pub fn fix(doc: &Document, profile: Profile, options: &FixOptions) -> Result<FixReport> {
    let mut applied = Vec::new();
    let catalog_ref = catalog_ref(doc)?;

    if options.tag && profile == Profile::PdfUa1 {
        let tree = crate::accessibility::read_structure(doc)?;
        if !tree.is_tagged() {
            let count = crate::accessibility::autotag(doc)?;
            if count > 0 {
                applied.push(format!(
                    "balisage automatique : {count} éléments de structure"
                ));
            }
        }
    }

    if options.remove_javascript {
        let removed = remove_actions(doc)?;
        if removed > 0 {
            applied.push(format!(
                "{removed} action(s) JavaScript ou de lancement retirée(s)"
            ));
        }
    }
    if options.remove_attachments && profile.forbids_embedded_files() && remove_attachments(doc)? {
        applied.push("pièces jointes retirées".to_string());
    }
    if options.embed_fonts {
        let embedded = embed_fonts(doc)?;
        if embedded > 0 {
            applied.push(format!(
                "{embedded} police(s) incorporée(s) depuis les polices système"
            ));
        }
    }
    if options.interpolate {
        let changed = force_no_interpolate(doc);
        if changed > 0 {
            applied.push(format!(
                "{changed} image(s) passée(s) en /Interpolate false"
            ));
        }
    }
    if options.trim_box && profile.is_pdfx() {
        let changed = ensure_trim_box(doc)?;
        if changed > 0 {
            applied.push(format!("{changed} page(s) dotée(s) d'un /TrimBox"));
        }
    }
    // Le titre doit exister avant d'écrire le XMP, qui le recopie.
    if let Some(title) = ensure_title(doc, options) {
        applied.push(format!("titre du document défini : « {title} »"));
    }
    if options.display_doc_title && set_display_doc_title(doc, catalog_ref)? {
        applied.push("/ViewerPreferences /DisplayDocTitle true".to_string());
    }
    if options.output_intent
        && profile.output_intent_subtype().is_some()
        && add_output_intent(doc, profile, catalog_ref)?
    {
        applied.push("intention de sortie sRVB ajoutée (profil ICC incorporé)".to_string());
    }
    if options.xmp {
        write_metadata(doc, profile, catalog_ref)?;
        applied.push("métadonnées XMP écrites".to_string());
    }

    let report = check_profile(doc, profile)?;
    Ok(FixReport {
        applied,
        remaining: report.issues,
    })
}

/// Référence du catalogue.
fn catalog_ref(doc: &Document) -> Result<ObjectRef> {
    match doc.trailer().get(&Name::new("Root")) {
        Some(Object::Reference(r)) => Ok(*r),
        _ => Err(Error::Corrupt(
            "catalogue absent ou direct : impossible de le modifier".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Correctifs simples
// ---------------------------------------------------------------------------

/// Retire le JavaScript (arbre de noms, `/OpenAction`, `/AA`) et les actions
/// `/Launch` des annotations.
fn remove_actions(doc: &Document) -> Result<usize> {
    let mut removed = 0usize;
    let catalog_ref = catalog_ref(doc)?;
    let mut catalog = doc.catalog()?;
    if let Some(names) = doc
        .dict_get(&catalog, "Names")
        .ok()
        .flatten()
        .and_then(|n| n.as_dict().cloned())
    {
        if names.contains_key(&Name::new("JavaScript")) {
            let mut names = names;
            names.remove(&Name::new("JavaScript"));
            removed += 1;
            if names.is_empty() {
                catalog.remove(&Name::new("Names"));
            } else {
                catalog.insert(Name::new("Names"), Object::Dict(names));
            }
        }
    }
    if let Some(open) = doc.dict_get(&catalog, "OpenAction").ok().flatten() {
        let is_script = open.as_dict().is_some_and(|d| {
            d.get(&Name::new("S"))
                .and_then(Object::as_name)
                .is_some_and(|n| n.0 == b"JavaScript" || n.0 == b"Launch")
        });
        if is_script {
            catalog.remove(&Name::new("OpenAction"));
            removed += 1;
        }
    }
    if catalog.remove(&Name::new("AA")).is_some() {
        removed += 1;
    }
    doc.set(catalog_ref, Object::Dict(catalog));

    for page in collect_pages(doc)? {
        let Some(page_ref) = page.reference else {
            continue;
        };
        let Ok(obj) = doc.get(page_ref) else { continue };
        let Some(mut dict) = obj.as_dict().cloned() else {
            continue;
        };
        let mut changed = dict.remove(&Name::new("AA")).is_some();
        if changed {
            removed += 1;
        }
        let annots = doc
            .dict_get(&dict, "Annots")
            .ok()
            .flatten()
            .and_then(|a| a.as_array().map(<[Object]>::to_vec))
            .unwrap_or_default();
        for item in &annots {
            let Object::Reference(r) = item else { continue };
            let Ok(annot) = doc.get(*r) else { continue };
            let Some(mut annot) = annot.as_dict().cloned() else {
                continue;
            };
            let mut touched = annot.remove(&Name::new("AA")).is_some();
            let dangerous = doc
                .dict_get(&annot, "A")
                .ok()
                .flatten()
                .and_then(|a| a.as_dict().cloned())
                .is_some_and(|d| {
                    d.get(&Name::new("S"))
                        .and_then(Object::as_name)
                        .is_some_and(|n| n.0 == b"JavaScript" || n.0 == b"Launch")
                });
            if dangerous {
                annot.remove(&Name::new("A"));
                touched = true;
            }
            if touched {
                removed += 1;
                doc.set(*r, Object::Dict(annot));
            }
        }
        if changed {
            doc.set(page_ref, Object::Dict(dict.clone()));
        }
        changed = false;
        let _ = changed;
    }
    Ok(removed)
}

/// Retire l'arbre `/Names /EmbeddedFiles` et les annotations de pièce jointe.
fn remove_attachments(doc: &Document) -> Result<bool> {
    let mut changed = false;
    let catalog_ref = catalog_ref(doc)?;
    let mut catalog = doc.catalog()?;
    if let Some(mut names) = doc
        .dict_get(&catalog, "Names")
        .ok()
        .flatten()
        .and_then(|n| n.as_dict().cloned())
    {
        if names.remove(&Name::new("EmbeddedFiles")).is_some() {
            changed = true;
            if names.is_empty() {
                catalog.remove(&Name::new("Names"));
            } else {
                catalog.insert(Name::new("Names"), Object::Dict(names));
            }
        }
    }
    if changed {
        doc.set(catalog_ref, Object::Dict(catalog));
    }
    for page in collect_pages(doc)? {
        let Some(page_ref) = page.reference else {
            continue;
        };
        let Ok(obj) = doc.get(page_ref) else { continue };
        let Some(mut dict) = obj.as_dict().cloned() else {
            continue;
        };
        let Some(annots) = doc
            .dict_get(&dict, "Annots")
            .ok()
            .flatten()
            .and_then(|a| a.as_array().map(<[Object]>::to_vec))
        else {
            continue;
        };
        let kept: Vec<Object> = annots
            .iter()
            .filter(|item| {
                doc.resolve(item)
                    .ok()
                    .and_then(|a| a.as_dict().cloned())
                    .and_then(|d| {
                        d.get(&Name::new("Subtype"))
                            .and_then(Object::as_name)
                            .cloned()
                    })
                    .is_none_or(|n| n.0 != b"FileAttachment")
            })
            .cloned()
            .collect();
        if kept.len() != annots.len() {
            changed = true;
            if kept.is_empty() {
                dict.remove(&Name::new("Annots"));
            } else {
                dict.insert(Name::new("Annots"), Object::Array(kept));
            }
            doc.set(page_ref, Object::Dict(dict));
        }
    }
    Ok(changed)
}

/// `/Interpolate false` sur toutes les images du document.
fn force_no_interpolate(doc: &Document) -> usize {
    let mut changed = 0usize;
    for number in doc.object_numbers() {
        let r = ObjectRef {
            number,
            generation: 0,
        };
        let Ok(obj) = doc.get(r) else { continue };
        let Object::Stream { dict, raw } = &*obj else {
            continue;
        };
        let is_image = dict
            .get(&Name::new("Subtype"))
            .and_then(Object::as_name)
            .is_some_and(|n| n.0 == b"Image");
        if !is_image {
            continue;
        }
        if matches!(
            dict.get(&Name::new("Interpolate")),
            Some(Object::Bool(true))
        ) {
            let mut dict = dict.clone();
            dict.insert(Name::new("Interpolate"), Object::Bool(false));
            doc.set(
                r,
                Object::Stream {
                    dict,
                    raw: raw.clone(),
                },
            );
            changed += 1;
        }
    }
    changed
}

/// `/TrimBox` égal à la `/CropBox` sur les pages qui n'ont ni `/TrimBox` ni
/// `/ArtBox` : la zone à rogner devient explicite sans rien déplacer.
fn ensure_trim_box(doc: &Document) -> Result<usize> {
    let mut changed = 0usize;
    for page in collect_pages(doc)? {
        let Some(page_ref) = page.reference else {
            continue;
        };
        if page.dict.contains_key(&Name::new("TrimBox"))
            || page.dict.contains_key(&Name::new("ArtBox"))
        {
            continue;
        }
        let b = page.crop_box(doc);
        let Ok(obj) = doc.get(page_ref) else { continue };
        let Some(mut dict) = obj.as_dict().cloned() else {
            continue;
        };
        dict.insert(
            Name::new("TrimBox"),
            Object::Array(vec![
                Object::Real(b.x0),
                Object::Real(b.y0),
                Object::Real(b.x1),
                Object::Real(b.y1),
            ]),
        );
        doc.set(page_ref, Object::Dict(dict));
        changed += 1;
    }
    Ok(changed)
}

/// `/ViewerPreferences /DisplayDocTitle true` (ISO 14289-1 §7.1).
fn set_display_doc_title(doc: &Document, catalog_ref: ObjectRef) -> Result<bool> {
    let mut catalog = doc.catalog()?;
    let mut prefs = doc
        .dict_get(&catalog, "ViewerPreferences")
        .ok()
        .flatten()
        .and_then(|v| v.as_dict().cloned())
        .unwrap_or_default();
    if matches!(
        prefs.get(&Name::new("DisplayDocTitle")),
        Some(Object::Bool(true))
    ) {
        return Ok(false);
    }
    prefs.insert(Name::new("DisplayDocTitle"), Object::Bool(true));
    catalog.insert(Name::new("ViewerPreferences"), Object::Dict(prefs));
    doc.set(catalog_ref, Object::Dict(catalog));
    Ok(true)
}

/// Titre du document : celui fourni, sinon celui déjà présent, sinon la
/// première ligne de texte de la première page (comme le fait un traitement
/// de texte qui devine un titre).
fn ensure_title(doc: &Document, options: &FixOptions) -> Option<String> {
    let existing = document_title(doc);
    if options.title.is_none() && existing.as_ref().is_some_and(|t| !t.trim().is_empty()) {
        return None;
    }
    let title = match &options.title {
        Some(t) => t.clone(),
        None => guess_title(doc).unwrap_or_else(|| "Document sans titre".to_string()),
    };
    let info_ref = if let Some(Object::Reference(r)) = doc.trailer().get(&Name::new("Info")) {
        *r
    } else {
        let r = doc.add(Object::Dict(Dict::new()));
        doc.set_trailer_entry("Info", Object::Reference(r));
        r
    };
    let mut info = doc
        .get(info_ref)
        .ok()
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_default();
    info.insert(
        Name::new("Title"),
        Object::String(encode_text_string(&title)),
    );
    doc.set(info_ref, Object::Dict(info));
    Some(title)
}

/// `/Info /Title` décodé.
fn document_title(doc: &Document) -> Option<String> {
    let info = crate::accessibility::info_dict(doc)?;
    match doc.dict_get(&info, "Title").ok().flatten().as_deref() {
        Some(Object::String(s)) => Some(decode_text_string(s)),
        _ => None,
    }
}

/// Première ligne de texte de la première page, tronquée.
fn guess_title(doc: &Document) -> Option<String> {
    let pages = collect_pages(doc).ok()?;
    let page = pages.first()?;
    let text = crate::text::extract_page_text(doc, page).ok()?;
    let line = text
        .lines
        .iter()
        .map(crate::text::Line::text)
        .find(|t| t.trim().chars().count() >= 3)?;
    let trimmed: String = line.trim().chars().take(80).collect();
    (!trimmed.is_empty()).then_some(trimmed)
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

// ---------------------------------------------------------------------------
// Intention de sortie et profil ICC sRVB
// ---------------------------------------------------------------------------

/// Ajoute une intention de sortie sRVB si le profil attendu manque.
fn add_output_intent(doc: &Document, profile: Profile, catalog_ref: ObjectRef) -> Result<bool> {
    let Some(subtype) = profile.output_intent_subtype() else {
        return Ok(false);
    };
    let mut catalog = doc.catalog()?;
    let existing = doc
        .dict_get(&catalog, "OutputIntents")
        .ok()
        .flatten()
        .and_then(|o| o.as_array().map(<[Object]>::to_vec))
        .unwrap_or_default();
    let already = existing.iter().any(|item| {
        doc.resolve(item)
            .ok()
            .and_then(|r| r.as_dict().cloned())
            .is_some_and(|d| {
                d.get(&Name::new("S"))
                    .and_then(Object::as_name)
                    .is_some_and(|n| n.as_str() == subtype)
                    && d.contains_key(&Name::new("DestOutputProfile"))
            })
    });
    if already {
        return Ok(false);
    }
    let icc = srgb_icc_profile();
    let mut stream_dict = Dict::new();
    stream_dict.insert(Name::new("N"), Object::Integer(3));
    stream_dict.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(icc.len()).unwrap_or(0)),
    );
    let profile_ref = doc.add(Object::Stream {
        dict: stream_dict,
        raw: icc,
    });

    let mut intent = Dict::new();
    intent.insert(Name::new("Type"), Object::Name(Name::new("OutputIntent")));
    intent.insert(Name::new("S"), Object::Name(Name::new(subtype)));
    intent.insert(
        Name::new("OutputConditionIdentifier"),
        Object::String(b"sRGB IEC61966-2.1".to_vec()),
    );
    intent.insert(
        Name::new("OutputCondition"),
        Object::String(b"sRGB IEC61966-2.1".to_vec()),
    );
    intent.insert(
        Name::new("Info"),
        Object::String(b"sRGB IEC61966-2.1 (profil ICC engendre par Acrux)".to_vec()),
    );
    intent.insert(
        Name::new("RegistryName"),
        Object::String(b"http://www.color.org".to_vec()),
    );
    intent.insert(
        Name::new("DestOutputProfile"),
        Object::Reference(profile_ref),
    );
    let intent_ref = doc.add(Object::Dict(intent));
    let mut intents = existing;
    intents.push(Object::Reference(intent_ref));
    catalog.insert(Name::new("OutputIntents"), Object::Array(intents));
    doc.set(catalog_ref, Object::Dict(catalog));
    Ok(true)
}

/// Illuminant D50 du PCS (ICC.1:2022 §7.2.16).
const D50: [f64; 3] = [0.964_2, 1.0, 0.824_9];
/// Primaires sRVB adaptées à D50 par la matrice de Bradford (IEC 61966-2-1
/// combinée à ICC.1:2022 annexe E) : colonnes R, V, B du profil.
const SRGB_PRIMARIES: [[f64; 3]; 3] = [
    [0.436_065, 0.222_488, 0.013_916],
    [0.385_147, 0.716_873, 0.097_076],
    [0.143_066, 0.060_608, 0.713_913],
];

/// Construit un profil ICC v2 matriciel/TRC décrivant sRGB.
///
/// Écrit ici, octet par octet, d'après ICC.1:2001-04 : en-tête de 128 octets,
/// table de tags, puis `desc`, `wtpt`, `rXYZ`/`gXYZ`/`bXYZ` (`XYZType`),
/// `rTRC`/`gTRC`/`bTRC` (`curveType` échantillonné sur 1024 points) et
/// `cprt`. Les trois courbes partagent les mêmes octets, ce que la norme
/// autorise. Notre propre parseur (`acrux_graphics::IccProfile`) le relit.
#[must_use]
pub fn srgb_icc_profile() -> Vec<u8> {
    let curve = srgb_curve_tag();
    let tags: Vec<([u8; 4], Vec<u8>)> = vec![
        (*b"desc", text_description("sRGB IEC61966-2.1")),
        (*b"wtpt", xyz_tag(D50)),
        (*b"rXYZ", xyz_tag(SRGB_PRIMARIES[0])),
        (*b"gXYZ", xyz_tag(SRGB_PRIMARIES[1])),
        (*b"bXYZ", xyz_tag(SRGB_PRIMARIES[2])),
        (*b"rTRC", curve.clone()),
        (*b"gTRC", curve.clone()),
        (*b"bTRC", curve),
        (*b"cprt", text_tag("Domaine public - engendre par Acrux")),
    ];

    // Table des tags, en partageant les données identiques.
    let header_size = 128;
    let table_size = 4 + tags.len() * 12;
    let mut data = Vec::new();
    let mut offsets: BTreeMap<Vec<u8>, (u32, u32)> = BTreeMap::new();
    let mut entries: Vec<([u8; 4], u32, u32)> = Vec::new();
    for (sig, body) in &tags {
        if let Some((offset, size)) = offsets.get(body) {
            entries.push((*sig, *offset, *size));
            continue;
        }
        let offset = u32::try_from(header_size + table_size + data.len()).unwrap_or(0);
        let size = u32::try_from(body.len()).unwrap_or(0);
        data.extend_from_slice(body);
        while data.len() % 4 != 0 {
            data.push(0);
        }
        offsets.insert(body.clone(), (offset, size));
        entries.push((*sig, offset, size));
    }

    let total = header_size + table_size + data.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&u32::try_from(total).unwrap_or(0).to_be_bytes());
    out.extend_from_slice(&[0; 4]); // CMM
    out.extend_from_slice(&[0x02, 0x10, 0x00, 0x00]); // version 2.1
    out.extend_from_slice(b"mntr");
    out.extend_from_slice(b"RGB ");
    out.extend_from_slice(b"XYZ ");
    // Date et heure figées : le profil doit être reproductible octet pour octet.
    for v in [2026_u16, 1, 1, 0, 0, 0] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(b"acsp");
    out.extend_from_slice(&[0; 4]); // plate-forme
    out.extend_from_slice(&[0; 4]); // drapeaux
    out.extend_from_slice(&[0; 4]); // fabricant
    out.extend_from_slice(&[0; 4]); // modèle
    out.extend_from_slice(&[0; 8]); // attributs
    out.extend_from_slice(&[0; 4]); // intention de rendu : perceptuelle
    for v in D50 {
        out.extend_from_slice(&s15_fixed16(v).to_be_bytes());
    }
    out.extend_from_slice(&[0; 4]); // créateur
    out.extend_from_slice(&[0; 16]); // identifiant du profil
    out.extend_from_slice(&[0; 28]); // réservé
    debug_assert_eq!(out.len(), header_size);

    out.extend_from_slice(&u32::try_from(entries.len()).unwrap_or(0).to_be_bytes());
    for (sig, offset, size) in &entries {
        out.extend_from_slice(sig);
        out.extend_from_slice(&offset.to_be_bytes());
        out.extend_from_slice(&size.to_be_bytes());
    }
    out.extend_from_slice(&data);
    out
}

/// Nombre `s15Fixed16` (ICC.1:2022 §4.6).
#[allow(clippy::cast_possible_truncation)]
fn s15_fixed16(v: f64) -> i32 {
    (v * 65536.0)
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// `XYZType` (§10.31).
fn xyz_tag(xyz: [f64; 3]) -> Vec<u8> {
    let mut t = b"XYZ \0\0\0\0".to_vec();
    for v in xyz {
        t.extend_from_slice(&s15_fixed16(v).to_be_bytes());
    }
    t
}

/// `curveType` (§10.6) échantillonnant la fonction de transfert sRGB
/// (IEC 61966-2-1) sur 1024 points en 16 bits.
fn srgb_curve_tag() -> Vec<u8> {
    const POINTS: usize = 1024;
    let mut t = b"curv\0\0\0\0".to_vec();
    t.extend_from_slice(&u32::try_from(POINTS).unwrap_or(0).to_be_bytes());
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    for i in 0..POINTS {
        let x = i as f64 / (POINTS - 1) as f64;
        let y = acrux_graphics::icc::srgb_to_linear(x);
        let v = (y.clamp(0.0, 1.0) * 65535.0).round() as u16;
        t.extend_from_slice(&v.to_be_bytes());
    }
    t
}

/// `textDescriptionType` v2 (§6.5.17) : compte ASCII, chaîne, puis les zones
/// Unicode et ScriptCode laissées vides.
fn text_description(text: &str) -> Vec<u8> {
    let mut t = b"desc\0\0\0\0".to_vec();
    t.extend_from_slice(&u32::try_from(text.len() + 1).unwrap_or(0).to_be_bytes());
    t.extend_from_slice(text.as_bytes());
    t.push(0);
    t.extend_from_slice(&[0; 8]); // code de langue et compte Unicode
    t.extend_from_slice(&[0; 3]); // ScriptCode et compte Macintosh
    t.extend_from_slice(&[0; 67]); // description Macintosh
    t
}

/// `textType` (§10.24).
fn text_tag(text: &str) -> Vec<u8> {
    let mut t = b"text\0\0\0\0".to_vec();
    t.extend_from_slice(text.as_bytes());
    t.push(0);
    t
}

// ---------------------------------------------------------------------------
// Métadonnées XMP
// ---------------------------------------------------------------------------

/// Écrit (ou remplace) le flux `/Metadata` du catalogue.
fn write_metadata(doc: &Document, profile: Profile, catalog_ref: ObjectRef) -> Result<()> {
    let packet = xmp_packet(doc, Some(profile));
    let raw = packet.into_bytes();
    let mut dict = Dict::new();
    dict.insert(Name::new("Type"), Object::Name(Name::new("Metadata")));
    dict.insert(Name::new("Subtype"), Object::Name(Name::new("XML")));
    dict.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
    );
    let mut catalog = doc.catalog()?;
    // Un flux XMP n'est jamais chiffré ni filtré (PDF/A-1 §6.7.3).
    let reference = match catalog.get(&Name::new("Metadata")) {
        Some(Object::Reference(r)) => *r,
        _ => doc.allocate(),
    };
    doc.set(reference, Object::Stream { dict, raw });
    catalog.insert(Name::new("Metadata"), Object::Reference(reference));
    doc.set(catalog_ref, Object::Dict(catalog));
    Ok(())
}

/// Construit un paquet XMP minimal mais complet pour le profil demandé.
///
/// Reprend `/Info` (`Title`, `Author`, `Subject`, `Keywords`, `Creator`,
/// `Producer`, `CreationDate`, `ModDate`) dans les espaces de noms `dc`,
/// `xmp` et `pdf`, puis ajoute l'identification du profil. `profile` à
/// `None` produit un paquet sans identification de conformité : c'est ce
/// dont [`crate::docinfo::set_metadata`] a besoin pour un document ordinaire.
#[must_use]
#[allow(clippy::too_many_lines)] // un bloc RDF par espace de noms, lus de haut en bas
pub fn xmp_packet(doc: &Document, profile: Option<Profile>) -> String {
    let info = crate::accessibility::info_dict(doc).unwrap_or_default();
    let text = |key: &str| match doc.dict_get(&info, key).ok().flatten().as_deref() {
        Some(Object::String(s)) => Some(decode_text_string(s)),
        _ => None,
    };
    let title = text("Title").unwrap_or_default();
    let author = text("Author").unwrap_or_default();
    let subject = text("Subject").unwrap_or_default();
    let keywords = text("Keywords").unwrap_or_default();
    let creator = text("Creator").unwrap_or_default();
    let producer = text("Producer").unwrap_or_else(|| "Acrux".to_string());
    let created = text("CreationDate").and_then(|d| iso_date(&d));
    let modified = text("ModDate").and_then(|d| iso_date(&d));

    let mut out = String::with_capacity(2048);
    out.push_str("<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n");
    out.push_str("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"Acrux\">\n");
    out.push_str("  <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n");

    out.push_str(
        "    <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n",
    );
    out.push_str("      <dc:format>application/pdf</dc:format>\n");
    if !title.is_empty() {
        let _ = writeln!(
            out,
            "      <dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:title>",
            xml_escape(&title)
        );
    }
    if !author.is_empty() {
        let _ = writeln!(
            out,
            "      <dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>",
            xml_escape(&author)
        );
    }
    if !subject.is_empty() {
        let _ = writeln!(
            out,
            "      <dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:description>",
            xml_escape(&subject)
        );
    }
    out.push_str("    </rdf:Description>\n");

    out.push_str(
        "    <rdf:Description rdf:about=\"\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\">\n",
    );
    if !creator.is_empty() {
        let _ = writeln!(
            out,
            "      <xmp:CreatorTool>{}</xmp:CreatorTool>",
            xml_escape(&creator)
        );
    }
    if let Some(d) = &created {
        let _ = writeln!(out, "      <xmp:CreateDate>{d}</xmp:CreateDate>");
    }
    if let Some(d) = &modified {
        let _ = writeln!(out, "      <xmp:ModifyDate>{d}</xmp:ModifyDate>");
    }
    out.push_str("    </rdf:Description>\n");

    out.push_str(
        "    <rdf:Description rdf:about=\"\" xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\">\n",
    );
    let _ = writeln!(
        out,
        "      <pdf:Producer>{}</pdf:Producer>",
        xml_escape(&producer)
    );
    if !keywords.is_empty() {
        let _ = writeln!(
            out,
            "      <pdf:Keywords>{}</pdf:Keywords>",
            xml_escape(&keywords)
        );
    }
    out.push_str("    </rdf:Description>\n");

    if let Some((prefix, part, conformance)) = profile.and_then(Profile::xmp_identification) {
        let namespace = match prefix {
            "pdfaid" => "http://www.aiim.org/pdfa/ns/id/",
            "pdfuaid" => "http://www.aiim.org/pdfua/ns/id/",
            _ => "http://www.npes.org/pdfx/ns/id/",
        };
        let _ = writeln!(
            out,
            "    <rdf:Description rdf:about=\"\" xmlns:{prefix}=\"{namespace}\">"
        );
        let _ = writeln!(out, "      <{prefix}:part>{part}</{prefix}:part>");
        if prefix == "pdfaid" && !conformance.is_empty() {
            let _ = writeln!(
                out,
                "      <{prefix}:conformance>{conformance}</{prefix}:conformance>"
            );
        }
        if prefix == "pdfxid" {
            let _ = writeln!(
                out,
                "      <pdfxid:GTS_PDFXVersion>{conformance}</pdfxid:GTS_PDFXVersion>"
            );
        }
        out.push_str("    </rdf:Description>\n");
    }

    out.push_str("  </rdf:RDF>\n</x:xmpmeta>\n");
    // Espace de remplissage recommandé pour une mise à jour en place.
    for _ in 0..8 {
        out.push_str("                                                                       \n");
    }
    out.push_str("<?xpacket end=\"w\"?>\n");
    out
}

/// Convertit une date PDF `D:YYYYMMDDHHmmSSOHH'mm'` en date XMP ISO 8601.
fn iso_date(pdf: &str) -> Option<String> {
    let d = pdf.strip_prefix("D:").unwrap_or(pdf);
    let digits: String = d.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() < 8 {
        return None;
    }
    let part = |from: usize, len: usize| digits.get(from..from + len).unwrap_or("00");
    let mut out = format!("{}-{}-{}", part(0, 4), part(4, 2), part(6, 2));
    if digits.len() >= 14 {
        let _ = write!(out, "T{}:{}:{}", part(8, 2), part(10, 2), part(12, 2));
        // Décalage horaire, s'il est présent après les chiffres.
        let rest: String = d.chars().skip(digits.len()).collect();
        match rest.chars().next() {
            Some('Z') => out.push('Z'),
            Some(sign @ ('+' | '-')) => {
                let tz: String = rest.chars().skip(1).filter(char::is_ascii_digit).collect();
                if tz.len() >= 4 {
                    let _ = write!(out, "{sign}{}:{}", &tz[0..2], &tz[2..4]);
                }
            }
            _ => {}
        }
    }
    Some(out)
}

/// Échappe le texte pour du XML.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Incorporation des polices
// ---------------------------------------------------------------------------

/// Incorpore les polices simples non incorporées en puisant dans les polices
/// système. Rend le nombre de polices traitées.
fn embed_fonts(doc: &Document) -> Result<usize> {
    let mut done = 0usize;
    let mut seen = std::collections::HashSet::new();
    for page in collect_pages(doc)? {
        let resources = doc
            .dict_get(&page.dict, "Resources")
            .ok()
            .flatten()
            .and_then(|r| r.as_dict().cloned())
            .unwrap_or_default();
        let Some(fonts) = doc
            .dict_get(&resources, "Font")
            .ok()
            .flatten()
            .and_then(|f| f.as_dict().cloned())
        else {
            continue;
        };
        for value in fonts.values() {
            let Object::Reference(r) = value else {
                continue;
            };
            if !seen.insert(r.number) {
                continue;
            }
            if embed_one_font(doc, *r) {
                done += 1;
            }
        }
    }
    Ok(done)
}

/// Incorpore une police simple ; rend `true` si le dictionnaire a changé.
#[allow(clippy::too_many_lines)] // le descripteur a beaucoup d'entrées obligatoires
fn embed_one_font(doc: &Document, reference: ObjectRef) -> bool {
    let Ok(obj) = doc.get(reference) else {
        return false;
    };
    let Some(mut dict) = obj.as_dict().cloned() else {
        return false;
    };
    let subtype = dict
        .get(&Name::new("Subtype"))
        .and_then(Object::as_name)
        .map(Name::as_str)
        .unwrap_or_default();
    if !matches!(subtype.as_str(), "Type1" | "TrueType" | "MMType1") {
        return false;
    }
    let base_font = dict
        .get(&Name::new("BaseFont"))
        .and_then(Object::as_name)
        .map(Name::as_str)
        .unwrap_or_default();
    let descriptor = doc
        .dict_get(&dict, "FontDescriptor")
        .ok()
        .flatten()
        .and_then(|d| d.as_dict().cloned());
    let already = descriptor.as_ref().is_some_and(|d| {
        ["FontFile", "FontFile2", "FontFile3"]
            .iter()
            .any(|k| d.contains_key(&Name::new(k)))
    });
    if already {
        return false;
    }
    let Some(bytes) = find_system_font(&base_font) else {
        return false;
    };
    let Ok(font) = TrueTypeFont::parse(&bytes) else {
        return false;
    };
    let units = f64::from(font.units_per_em().max(1));
    let scale = 1000.0 / units;

    // Le programme incorporé.
    let mut file_dict = Dict::new();
    file_dict.insert(
        Name::new("Length1"),
        Object::Integer(i64::try_from(bytes.len()).unwrap_or(0)),
    );
    file_dict.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(bytes.len()).unwrap_or(0)),
    );
    let file_ref = doc.add(Object::Stream {
        dict: file_dict,
        raw: bytes,
    });

    // Descripteur : complété ou créé de toutes pièces.
    let mut descriptor = descriptor.unwrap_or_default();
    let bbox = font.bbox();
    let lower = base_font.to_ascii_lowercase();
    let serif = lower.contains("times") || lower.contains("serif") || lower.contains("roman");
    let fixed = lower.contains("courier") || lower.contains("mono");
    // Drapeaux §9.8.2 table 123 : bit 1 fixe, bit 2 serif, bit 6 non symbolique.
    let mut flags = 1 << 5;
    if fixed {
        flags |= 1;
    }
    if serif {
        flags |= 1 << 1;
    }
    let italic = lower.contains("italic") || lower.contains("oblique");
    if italic {
        flags |= 1 << 6;
    }
    let (ascent, descent) = font.hhea().map_or((750.0, -250.0), |h| {
        (
            f64::from(h.ascender) * scale,
            f64::from(h.descender) * scale,
        )
    });
    let entry = |value: f64| Object::Integer(round_i64(value));
    descriptor.insert(Name::new("Type"), Object::Name(Name::new("FontDescriptor")));
    descriptor.insert(Name::new("FontName"), Object::Name(Name::new(&base_font)));
    descriptor.insert(Name::new("Flags"), Object::Integer(flags));
    descriptor.insert(
        Name::new("FontBBox"),
        Object::Array(vec![
            entry(bbox.x0 * scale),
            entry(bbox.y0 * scale),
            entry(bbox.x1 * scale),
            entry(bbox.y1 * scale),
        ]),
    );
    descriptor.insert(
        Name::new("ItalicAngle"),
        Object::Integer(if italic { -12 } else { 0 }),
    );
    descriptor.insert(Name::new("Ascent"), entry(ascent));
    descriptor.insert(Name::new("Descent"), entry(descent));
    descriptor.insert(Name::new("CapHeight"), entry(ascent * 0.9));
    descriptor.insert(Name::new("StemV"), Object::Integer(80));
    descriptor.insert(Name::new("FontFile2"), Object::Reference(file_ref));
    let descriptor_ref = doc.add(Object::Dict(descriptor));

    // La police devient une TrueType incorporée, encodée en WinAnsi.
    dict.insert(Name::new("Subtype"), Object::Name(Name::new("TrueType")));
    dict.insert(
        Name::new("FontDescriptor"),
        Object::Reference(descriptor_ref),
    );
    dict.entry(Name::new("Encoding"))
        .or_insert_with(|| Object::Name(Name::new("WinAnsiEncoding")));
    if !dict.contains_key(&Name::new("Widths")) {
        let mut widths = Vec::with_capacity(224);
        for code in 32_u32..=255 {
            #[allow(clippy::cast_possible_truncation)]
            let name = encodings::win_ansi(code as u8);
            let gid = name
                .and_then(encodings::glyph_name_to_unicode)
                .and_then(|c| font.unicode_to_gid(c));
            let advance = gid
                .and_then(|g| font.advance(g))
                .map_or(0.0, |w| f64::from(w) * scale);
            widths.push(Object::Integer(round_i64(advance)));
        }
        dict.insert(Name::new("FirstChar"), Object::Integer(32));
        dict.insert(Name::new("LastChar"), Object::Integer(255));
        dict.insert(Name::new("Widths"), Object::Array(widths));
    }
    doc.set(reference, Object::Dict(dict));
    true
}

#[allow(clippy::cast_possible_truncation)]
fn round_i64(v: f64) -> i64 {
    v.round().clamp(-1e9, 1e9) as i64
}

/// Cherche un fichier de police système correspondant au nom de base.
/// Les mêmes répertoires que la substitution de `acrux-render`, et la variable
/// `ACRUX_FONT_DIR` pour les tests et les environnements sans police système.
fn find_system_font(base_font: &str) -> Option<Vec<u8>> {
    let lower = base_font.to_ascii_lowercase();
    let bold = ["bold", "black", "heavy", "semibold"]
        .iter()
        .any(|k| lower.contains(k));
    let italic = lower.contains("italic") || lower.contains("oblique");
    let fixed = lower.contains("courier") || lower.contains("mono");
    let serif = lower.contains("times") || lower.contains("serif") || lower.contains("roman");
    let candidates: Vec<&str> = match (fixed, serif, bold, italic) {
        (true, _, true, true) => vec!["courbi.ttf", "LiberationMono-BoldItalic.ttf"],
        (true, _, true, false) => vec!["courbd.ttf", "LiberationMono-Bold.ttf"],
        (true, _, false, true) => vec!["couri.ttf", "LiberationMono-Italic.ttf"],
        (true, _, false, false) => vec!["cour.ttf", "LiberationMono-Regular.ttf"],
        (_, true, true, true) => vec!["timesbi.ttf", "LiberationSerif-BoldItalic.ttf"],
        (_, true, true, false) => vec!["timesbd.ttf", "LiberationSerif-Bold.ttf"],
        (_, true, false, true) => vec!["timesi.ttf", "LiberationSerif-Italic.ttf"],
        (_, true, false, false) => vec!["times.ttf", "LiberationSerif-Regular.ttf"],
        (_, _, true, true) => vec!["arialbi.ttf", "LiberationSans-BoldItalic.ttf"],
        (_, _, true, false) => vec!["arialbd.ttf", "LiberationSans-Bold.ttf"],
        (_, _, false, true) => vec!["ariali.ttf", "LiberationSans-Italic.ttf"],
        (_, _, false, false) => vec!["arial.ttf", "LiberationSans-Regular.ttf"],
    };
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(custom) = std::env::var("ACRUX_FONT_DIR") {
        dirs.push(std::path::PathBuf::from(custom));
    }
    if let Ok(windir) = std::env::var("WINDIR") {
        dirs.push(std::path::PathBuf::from(windir).join("Fonts"));
    }
    for d in [
        "C:/Windows/Fonts",
        "/usr/share/fonts/truetype/liberation",
        "/usr/share/fonts/truetype/dejavu",
        "/usr/share/fonts",
        "/System/Library/Fonts",
        "/Library/Fonts",
    ] {
        dirs.push(std::path::PathBuf::from(d));
    }
    for file in candidates {
        for dir in &dirs {
            let path = dir.join(file);
            if path.is_file() {
                if let Ok(bytes) = std::fs::read(&path) {
                    return Some(bytes);
                }
            }
        }
    }
    None
}
