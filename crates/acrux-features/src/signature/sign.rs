//! Pose d'une signature numérique (ISO 32000-2 §12.8.2).
//!
//! La difficulté n'est pas la cryptographie, c'est la **circularité** : le
//! `/ByteRange` désigne des octets du fichier fini, mais on ne connaît les
//! décalages qu'une fois le fichier écrit — lequel contient le `/ByteRange`.
//! La norme résout cela par une réservation :
//!
//! 1. on écrit la mise à jour incrémentale avec un `/Contents` de `n` octets
//!    nuls et un `/ByteRange` rempli de chiffres de bourrage, tous deux de
//!    **largeur fixe** ;
//! 2. on relit les décalages réels dans les octets produits ;
//! 3. on récrit le `/ByteRange` **à la même place et à la même largeur**, en
//!    complétant par des espaces (une syntaxe que le PDF admet dans un
//!    tableau) ;
//! 4. on condense les octets couverts, on fabrique le CMS, et on le récrit
//!    dans la réservation, complétée par des zéros.
//!
//! Aucune étape ne déplace un octet. C'est la seule façon d'obtenir un
//! `/ByteRange` exact.
//!
//! Ce que nous ne faisons pas : signer un document **chiffré** (la sauvegarde
//! chiffrerait le `/Contents`, qui doit rester en clair), et poser un
//! `/DocMDP` de certification.

use std::fmt::Write as _;

use acrux_core::{Error, Rect, Result};
use acrux_document::asn1::Time;
use acrux_document::crypt::rsa::{Hash, PrivateKey};
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef};

use super::cms::{self, SignParameters};
use super::x509::Certificate;
use crate::annotations::encode_text;
use crate::stamp::{encode_win_ansi, pdf_literal, StandardFont};

/// Taille par défaut de la réservation du `/Contents`, en octets.
///
/// Un CMS avec un certificat RSA 2048 et sa racine pèse environ 2,5 Ko ;
/// 16 Ko laissent de la place à une chaîne complète sans gonfler le fichier.
pub const DEFAULT_RESERVED_BYTES: usize = 16_384;

/// Largeur de bourrage de chaque entier du `/ByteRange` réservé.
/// Dix chiffres couvrent un fichier de 9,9 Go.
const BYTE_RANGE_DIGITS: usize = 10;

/// Clé de signature : la clé privée, son certificat, et la chaîne qui mène à
/// la racine.
///
/// Se charge depuis une clé **PKCS#8 non chiffrée** et un certificat X.509,
/// tous deux en DER. Les conteneurs chiffrés (PKCS#12 / `.pfx`, PKCS#8
/// chiffré) ne sont pas pris en charge : extraire une clé d'un magasin
/// protégé par mot de passe est un travail de gestionnaire de clés, pas de
/// lecteur de PDF.
#[derive(Debug, Clone)]
pub struct SigningKey {
    /// Clé privée RSA.
    pub key: PrivateKey,
    /// Certificat du signataire.
    pub certificate: Certificate,
    /// Certificats intermédiaires et racine, joints à l'enveloppe.
    pub chain: Vec<Certificate>,
}

impl SigningKey {
    /// Charge une clé et son certificat depuis leurs octets DER.
    ///
    /// # Errors
    /// Clé ou certificat illisible, ou clé qui ne correspond pas au certificat.
    pub fn from_der(key_der: &[u8], certificate_der: &[u8]) -> Result<SigningKey> {
        let key = PrivateKey::from_der(key_der)?;
        let certificate = Certificate::parse(certificate_der)?;
        if certificate.public_key != key.public {
            return Err(Error::Corrupt(
                "la clé privée ne correspond pas à la clé publique du certificat".into(),
            ));
        }
        Ok(SigningKey {
            key,
            certificate,
            chain: Vec::new(),
        })
    }

    /// Ajoute les certificats intermédiaires et la racine.
    ///
    /// # Errors
    /// Un des certificats est illisible.
    pub fn with_chain(mut self, chain_der: &[Vec<u8>]) -> Result<SigningKey> {
        for der in chain_der {
            self.chain.push(Certificate::parse(der)?);
        }
        Ok(self)
    }

    /// Certificat du signataire suivi de sa chaîne.
    #[must_use]
    pub fn certificates(&self) -> Vec<Certificate> {
        let mut out = vec![self.certificate.clone()];
        out.extend(self.chain.iter().cloned());
        out
    }
}

/// Apparence visible d'une signature.
#[derive(Debug, Clone)]
pub struct Appearance {
    /// Index de la page (0 = première).
    pub page: usize,
    /// Rectangle du widget, en coordonnées de page.
    pub rect: Rect,
}

/// Options de signature.
#[derive(Debug, Clone)]
pub struct SignOptions {
    /// Nom du champ. Par défaut `Signature1`, `Signature2`…
    pub field_name: Option<String>,
    /// `/Name` : le nom que le signataire déclare. Par défaut, le `CN` du
    /// certificat.
    pub name: Option<String>,
    /// `/Reason`.
    pub reason: Option<String>,
    /// `/Location`.
    pub location: Option<String>,
    /// `/ContactInfo`.
    pub contact_info: Option<String>,
    /// Fonction de condensation. SHA-256 par défaut ; SHA-1 est refusé.
    pub hash: Hash,
    /// Apparence visible, ou `None` pour une signature invisible.
    pub appearance: Option<Appearance>,
    /// Date déclarée. Par défaut, l'heure courante.
    pub signing_time: Option<Time>,
    /// Taille de la réservation du `/Contents`.
    pub reserved_bytes: usize,
}

impl Default for SignOptions {
    fn default() -> Self {
        SignOptions {
            field_name: None,
            name: None,
            reason: None,
            location: None,
            contact_info: None,
            hash: Hash::Sha256,
            appearance: None,
            signing_time: None,
            reserved_bytes: DEFAULT_RESERVED_BYTES,
        }
    }
}

/// Signe un document et rend les octets du fichier signé.
///
/// Le document d'origine est conservé octet pour octet : la signature est une
/// mise à jour incrémentale (§7.5.6). Le résultat se relit et se vérifie par
/// [`super::verify_signature`].
///
/// Si [`SignOptions::field_name`] désigne un champ de signature **déjà présent
/// et vide** (un emplacement préparé), ce champ est rempli : son rectangle, sa
/// page et ses drapeaux sont conservés, et aucun second champ du même nom n'est
/// créé. Sinon un champ neuf est ajouté.
///
/// # Errors
/// Document chiffré, page inexistante, réservation trop petite pour
/// l'enveloppe CMS, ou écriture impossible.
#[allow(clippy::too_many_lines)] // décalque la structure d'un dictionnaire /Sig
pub fn sign(doc: &Document, key: &SigningKey, options: &SignOptions) -> Result<Vec<u8>> {
    if doc.is_encrypted() {
        return Err(Error::Unsupported(
            "signer un document chiffré : le /Contents doit rester en clair, \
             or l'enregistrement chiffrerait les chaînes. Retirez d'abord la protection."
                .into(),
        ));
    }
    if options.hash == Hash::Sha1 {
        return Err(Error::Unsupported(
            "SHA-1 n'est plus sûr : signez en SHA-256, SHA-384 ou SHA-512".into(),
        ));
    }
    if options.reserved_bytes < 1024 {
        return Err(Error::Corrupt(
            "réservation du /Contents trop petite (au moins 1024 octets)".into(),
        ));
    }

    // Un champ de signature **préparé mais vide** porte déjà son nom, sa page
    // et son rectangle : on le remplit au lieu d'en créer un second, sans quoi
    // le document se retrouverait avec deux champs du même nom (§12.7.4.2).
    let existing = match &options.field_name {
        Some(name) => super::list_signatures(doc)?
            .into_iter()
            .find(|s| s.unsigned && s.field_name == *name)
            .and_then(|s| s.field_reference),
        None => None,
    };
    let signature_reference = doc.allocate();
    let field_reference = match existing {
        Some(r) => r,
        None => doc.allocate(),
    };
    let field_name = options
        .field_name
        .clone()
        .unwrap_or_else(|| next_field_name(doc));

    // --- Dictionnaire de signature, avec ses deux réservations -------------
    let mut signature = Dict::new();
    signature.insert(Name::new("Type"), Object::Name(Name::new("Sig")));
    signature.insert(
        Name::new("Filter"),
        Object::Name(Name::new("Adobe.PPKLite")),
    );
    signature.insert(
        Name::new("SubFilter"),
        Object::Name(Name::new("adbe.pkcs7.detached")),
    );
    // Bourrage : quatre entiers de dix chiffres, qui fixent la largeur du
    // tableau pour de bon.
    let placeholder = 10i64.pow(u32::try_from(BYTE_RANGE_DIGITS).unwrap_or(10) - 1);
    signature.insert(
        Name::new("ByteRange"),
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(placeholder),
            Object::Integer(placeholder),
            Object::Integer(placeholder),
        ]),
    );
    signature.insert(
        Name::new("Contents"),
        Object::String(vec![0u8; options.reserved_bytes]),
    );
    let signer_name = options
        .name
        .clone()
        .or_else(|| key.certificate.subject.common_name().map(str::to_string));
    if let Some(n) = &signer_name {
        signature.insert(Name::new("Name"), Object::String(encode_text(n)));
    }
    let when = options.signing_time.unwrap_or_else(Time::now);
    // La date du dictionnaire et celle de l'attribut signé `signingTime`
    // doivent coïncider : deux dates différentes dans un même document
    // signé sont un signal d'alerte pour les vérificateurs.
    signature.insert(Name::new("M"), Object::String(pdf_date(when).into_bytes()));
    for (key_name, value) in [
        ("Reason", &options.reason),
        ("Location", &options.location),
        ("ContactInfo", &options.contact_info),
    ] {
        if let Some(v) = value {
            signature.insert(Name::new(key_name), Object::String(encode_text(v)));
        }
    }
    doc.set(signature_reference, Object::Dict(signature));

    // --- Champ de formulaire et son widget --------------------------------
    // On repart du dictionnaire existant quand on remplit un champ préparé :
    // son `/Rect`, son `/P` et ses drapeaux ont été posés par qui l'a préparé.
    let mut field = match existing {
        Some(r) => doc
            .get(r)
            .ok()
            .and_then(|o| o.as_dict().cloned())
            .unwrap_or_default(),
        None => Dict::new(),
    };
    field.insert(Name::new("Type"), Object::Name(Name::new("Annot")));
    field.insert(Name::new("Subtype"), Object::Name(Name::new("Widget")));
    field.insert(Name::new("FT"), Object::Name(Name::new("Sig")));
    field.insert(Name::new("T"), Object::String(encode_text(&field_name)));
    field.insert(Name::new("V"), Object::Reference(signature_reference));
    // Imprimable (4) et verrouillée (128) : une signature ne se déplace pas.
    field.insert(Name::new("F"), Object::Integer(132));
    // Rectangle : celui demandé, celui du champ préparé, ou le rectangle vide
    // d'une signature invisible.
    let rect = match (&options.appearance, existing) {
        (Some(a), _) => Some(a.rect),
        (None, Some(_)) => None,
        (None, None) => Some(Rect::new(0.0, 0.0, 0.0, 0.0)),
    };
    if let Some(r) = rect {
        field.insert(
            Name::new("Rect"),
            Object::Array(vec![
                Object::Real(r.x0),
                Object::Real(r.y0),
                Object::Real(r.x1),
                Object::Real(r.y1),
            ]),
        );
    }

    let pages = collect_pages(doc)?;
    let page_index = options.appearance.as_ref().map_or(0, |a| a.page);
    let page = pages
        .get(page_index)
        .ok_or_else(|| Error::Corrupt(format!("page {} inexistante", page_index + 1)))?;
    let page_reference = page
        .reference
        .ok_or_else(|| Error::Corrupt("la page doit être un objet indirect".into()))?;
    if existing.is_none() || options.appearance.is_some() {
        field.insert(Name::new("P"), Object::Reference(page_reference));
    }

    if options.appearance.is_some() {
        let drawn = rect.unwrap_or_else(|| Rect::new(0.0, 0.0, 0.0, 0.0));
        let stream = appearance_stream(drawn, signer_name.as_deref(), options, when);
        let appearance_reference = doc.add(stream);
        let mut ap = Dict::new();
        ap.insert(Name::new("N"), Object::Reference(appearance_reference));
        field.insert(Name::new("AP"), Object::Dict(ap));
    }
    doc.set(field_reference, Object::Dict(field));

    if existing.is_none() {
        // Le widget doit figurer dans les annotations de la page, visible ou
        // non : c'est ce qui rattache la signature à une page (§12.7.5.5).
        append_to_page_annotations(doc, page_reference, field_reference)?;
        register_in_acroform(doc, Some(field_reference))?;
    } else {
        // Le champ est déjà déclaré : il ne manque que le drapeau qui dit
        // qu'une signature existe désormais.
        register_in_acroform(doc, None)?;
    }

    // --- Écriture puis remplissage des réservations ------------------------
    let written = doc.save_incremental()?;
    fill_reservations(written, signature_reference.number, key, options, when)
}

/// Remplit le `/ByteRange` puis le `/Contents` sans déplacer d'octet.
fn fill_reservations(
    mut data: Vec<u8>,
    signature_number: u32,
    key: &SigningKey,
    options: &SignOptions,
    when: Time,
) -> Result<Vec<u8>> {
    let header = format!("\n{signature_number} 0 obj");
    let object_start = find_last(&data, header.as_bytes()).ok_or_else(|| {
        Error::Corrupt(format!(
            "objet de signature {signature_number} introuvable dans le fichier écrit"
        ))
    })?;
    let object_end = find_from(&data, b"endobj", object_start)
        .ok_or_else(|| Error::Corrupt("fin de l'objet de signature introuvable".into()))?;

    // Emplacement de la chaîne hexadécimale du /Contents.
    let contents_key = find_between(&data, b"/Contents", object_start, object_end)
        .ok_or_else(|| Error::Corrupt("/Contents introuvable".into()))?;
    let hex_open = find_between(&data, b"<", contents_key, object_end)
        .ok_or_else(|| Error::Corrupt("chaîne hexadécimale du /Contents introuvable".into()))?;
    let hex_close = find_between(&data, b">", hex_open, object_end)
        .ok_or_else(|| Error::Corrupt("fin du /Contents introuvable".into()))?;
    let gap_start = hex_open;
    let gap_end = hex_close + 1;

    // /ByteRange réel : tout sauf l'intervalle du /Contents.
    let tail = data.len() - gap_end;
    let range_key = find_between(&data, b"/ByteRange", object_start, object_end)
        .ok_or_else(|| Error::Corrupt("/ByteRange introuvable".into()))?;
    let open = find_between(&data, b"[", range_key, object_end)
        .ok_or_else(|| Error::Corrupt("tableau /ByteRange introuvable".into()))?;
    let close = find_between(&data, b"]", open, object_end)
        .ok_or_else(|| Error::Corrupt("fin du /ByteRange introuvable".into()))?;
    let width = close - open - 1;
    let value = format!("0 {gap_start} {gap_end} {tail}");
    if value.len() > width {
        return Err(Error::Corrupt(
            "fichier trop grand pour la réservation du /ByteRange".into(),
        ));
    }
    let padded = format!("{value:<width$}");
    if let Some(slot) = data.get_mut(open + 1..close) {
        slot.copy_from_slice(padded.as_bytes());
    }

    // Condensé des octets couverts, puis enveloppe CMS.
    let mut covered = Vec::with_capacity(gap_start + tail);
    covered.extend_from_slice(data.get(..gap_start).unwrap_or(&[]));
    covered.extend_from_slice(data.get(gap_end..).unwrap_or(&[]));
    let digest = options.hash.digest(&covered);
    let certificates = key.certificates();
    let envelope = cms::build_detached(
        &SignParameters {
            content_digest: &digest,
            hash: options.hash,
            certificates: &certificates,
            signing_time: when,
        },
        &key.key,
    )?;
    let room = hex_close - hex_open - 1;
    if envelope.len() * 2 > room {
        return Err(Error::Corrupt(format!(
            "réservation de {} octets trop petite pour une enveloppe CMS de {} octets : \
             augmentez SignOptions::reserved_bytes",
            room / 2,
            envelope.len()
        )));
    }
    let mut hex = String::with_capacity(room);
    for byte in &envelope {
        let _ = write!(hex, "{byte:02X}");
    }
    // Le reste de la réservation reste à zéro : la longueur DER de l'enveloppe
    // en délimite la fin, les octets suivants sont ignorés par tout lecteur.
    while hex.len() < room {
        hex.push('0');
    }
    if let Some(slot) = data.get_mut(hex_open + 1..hex_close) {
        slot.copy_from_slice(hex.as_bytes());
    }
    Ok(data)
}

/// Ajoute le widget au tableau `/Annots` de la page.
fn append_to_page_annotations(
    doc: &Document,
    page_reference: ObjectRef,
    widget: ObjectRef,
) -> Result<()> {
    let page_object = doc.get(page_reference)?;
    let Some(page_dict) = page_object.as_dict() else {
        return Err(Error::Corrupt("page illisible".into()));
    };
    let mut page_dict = page_dict.clone();
    match page_dict.get(&Name::new("Annots")).cloned() {
        Some(Object::Reference(r)) => {
            // Tableau partagé : on le remplace en place, la page ne bouge pas.
            let existing = doc.get(r)?;
            let mut list = existing.as_array().unwrap_or(&[]).to_vec();
            list.push(Object::Reference(widget));
            doc.set(r, Object::Array(list));
            return Ok(());
        }
        Some(Object::Array(mut list)) => {
            list.push(Object::Reference(widget));
            page_dict.insert(Name::new("Annots"), Object::Array(list));
        }
        _ => {
            page_dict.insert(
                Name::new("Annots"),
                Object::Array(vec![Object::Reference(widget)]),
            );
        }
    }
    doc.set(page_reference, Object::Dict(page_dict));
    Ok(())
}

/// Inscrit le champ dans `/AcroForm /Fields` et pose `/SigFlags 3`.
///
/// `/SigFlags` bit 1 (`SignaturesExist`) et bit 2 (`AppendOnly`) : le second
/// dit au lecteur que le fichier ne doit être modifié que par ajout, ce qui
/// est précisément ce qui préserve les signatures.
fn register_in_acroform(doc: &Document, field: Option<ObjectRef>) -> Result<()> {
    let catalog_reference = match doc.trailer().get(&Name::new("Root")) {
        Some(Object::Reference(r)) => *r,
        _ => return Err(Error::Corrupt("catalogue absent du trailer".into())),
    };
    let catalog = doc.catalog()?;
    match catalog.get(&Name::new("AcroForm")).cloned() {
        Some(Object::Reference(r)) => {
            let form_object = doc.get(r)?;
            let mut form = form_object.as_dict().cloned().unwrap_or_default();
            if let Some(field) = field {
                add_field(doc, &mut form, field)?;
            }
            form.insert(Name::new("SigFlags"), Object::Integer(3));
            doc.set(r, Object::Dict(form));
        }
        Some(Object::Dict(mut form)) => {
            if let Some(field) = field {
                add_field(doc, &mut form, field)?;
            }
            form.insert(Name::new("SigFlags"), Object::Integer(3));
            let mut catalog = catalog;
            catalog.insert(Name::new("AcroForm"), Object::Dict(form));
            doc.set(catalog_reference, Object::Dict(catalog));
        }
        _ => {
            let mut form = Dict::new();
            form.insert(
                Name::new("Fields"),
                Object::Array(field.map(Object::Reference).into_iter().collect()),
            );
            form.insert(Name::new("SigFlags"), Object::Integer(3));
            let form_reference = doc.add(Object::Dict(form));
            let mut catalog = catalog;
            catalog.insert(Name::new("AcroForm"), Object::Reference(form_reference));
            doc.set(catalog_reference, Object::Dict(catalog));
        }
    }
    Ok(())
}

fn add_field(doc: &Document, form: &mut Dict, field: ObjectRef) -> Result<()> {
    match form.get(&Name::new("Fields")).cloned() {
        Some(Object::Reference(r)) => {
            let existing = doc.get(r)?;
            let mut list = existing.as_array().unwrap_or(&[]).to_vec();
            list.push(Object::Reference(field));
            doc.set(r, Object::Array(list));
        }
        Some(Object::Array(mut list)) => {
            list.push(Object::Reference(field));
            form.insert(Name::new("Fields"), Object::Array(list));
        }
        _ => {
            form.insert(
                Name::new("Fields"),
                Object::Array(vec![Object::Reference(field)]),
            );
        }
    }
    Ok(())
}

/// Premier nom de champ libre : `Signature1`, `Signature2`…
fn next_field_name(doc: &Document) -> String {
    let existing: Vec<String> = crate::forms::list_fields(doc)
        .map(|fields| fields.into_iter().map(|f| f.name).collect())
        .unwrap_or_default();
    for n in 1..10_000 {
        let candidate = format!("Signature{n}");
        if !existing.contains(&candidate) {
            return candidate;
        }
    }
    "Signature".to_string()
}

/// Apparence : un cadre, un fond très clair et quelques lignes de texte en
/// Helvetica. Volontairement sobre — une apparence n'est pas une preuve, et
/// une apparence trop convaincante est un piège.
fn appearance_stream(
    rect: Rect,
    signer: Option<&str>,
    options: &SignOptions,
    when: Time,
) -> Object {
    let width = (rect.x1 - rect.x0).abs().max(1.0);
    let height = (rect.y1 - rect.y0).abs().max(1.0);
    let font = StandardFont::Helvetica;
    let mut lines = Vec::new();
    if let Some(name) = signer {
        lines.push(format!("Signé par : {name}"));
    }
    if let Some(reason) = &options.reason {
        lines.push(format!("Motif : {reason}"));
    }
    if let Some(location) = &options.location {
        lines.push(format!("Lieu : {location}"));
    }
    lines.push(format!("Date : {}", when.to_display()));
    // Corps qui tient en hauteur, puis réduit s'il déborde en largeur.
    let count = u32::try_from(lines.len()).unwrap_or(1).max(1);
    let mut size = (height / (f64::from(count) * 1.35)).clamp(4.0, 11.0);
    let widest = lines
        .iter()
        .map(|l| font.text_width(l, 1.0))
        .fold(0.0f64, f64::max);
    if widest > 0.0 {
        size = size.min((width - 10.0).max(1.0) / widest);
    }
    let leading = size * 1.35;
    let mut content = String::new();
    let _ = write!(
        content,
        "q 0.96 0.96 0.99 rg 0 0 {} {} re f 0.16 0.20 0.42 RG 1 w 0.5 0.5 {} {} re S \
         BT /Helv {} Tf 0.10 0.12 0.30 rg ",
        fmt(width),
        fmt(height),
        fmt(width - 1.0),
        fmt(height - 1.0),
        fmt(size)
    );
    let mut y = height - 4.0 - size;
    for (index, line) in lines.iter().enumerate() {
        let encoded = encode_win_ansi(line);
        if index == 0 {
            let _ = write!(content, "1 0 0 1 {} {} Tm ", fmt(5.0), fmt(y));
        } else {
            let _ = write!(content, "0 {} Td ", fmt(-leading));
        }
        let _ = write!(content, "{} Tj ", pdf_literal(&encoded.bytes));
        y -= leading;
    }
    content.push_str("ET Q");

    let mut font_dict = Dict::new();
    font_dict.insert(Name::new("Type"), Object::Name(Name::new("Font")));
    font_dict.insert(Name::new("Subtype"), Object::Name(Name::new("Type1")));
    font_dict.insert(
        Name::new("BaseFont"),
        Object::Name(Name::new(font.base_font())),
    );
    font_dict.insert(
        Name::new("Encoding"),
        Object::Name(Name::new("WinAnsiEncoding")),
    );
    let mut fonts = Dict::new();
    fonts.insert(Name::new("Helv"), Object::Dict(font_dict));
    let mut resources = Dict::new();
    resources.insert(Name::new("Font"), Object::Dict(fonts));

    let mut dict = Dict::new();
    dict.insert(Name::new("Type"), Object::Name(Name::new("XObject")));
    dict.insert(Name::new("Subtype"), Object::Name(Name::new("Form")));
    dict.insert(
        Name::new("BBox"),
        Object::Array(vec![
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(width),
            Object::Real(height),
        ]),
    );
    dict.insert(Name::new("Resources"), Object::Dict(resources));
    let raw = content.into_bytes();
    dict.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
    );
    Object::Stream { dict, raw }
}

/// Date au format PDF (§7.9.4) `D:AAAAMMJJhhmmssZ`, en UTC.
fn pdf_date(t: Time) -> String {
    format!(
        "D:{:04}{:02}{:02}{:02}{:02}{:02}Z",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}

/// Nombre sans zéros inutiles, pour les flux de contenu.
fn fmt(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round())
    } else {
        format!("{v:.3}")
    }
}

fn find_last(data: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || data.len() < needle.len() {
        return None;
    }
    (0..=data.len() - needle.len())
        .rev()
        .find(|&i| data.get(i..i + needle.len()) == Some(needle))
}

fn find_from(data: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || data.len() < needle.len() {
        return None;
    }
    (from..=data.len() - needle.len()).find(|&i| data.get(i..i + needle.len()) == Some(needle))
}

fn find_between(data: &[u8], needle: &[u8], from: usize, to: usize) -> Option<usize> {
    let found = find_from(data, needle, from)?;
    if found < to {
        Some(found)
    } else {
        None
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn needle_search() {
        let data = b"aXbXcX";
        assert_eq!(find_last(data, b"X"), Some(5));
        assert_eq!(find_from(data, b"X", 2), Some(3));
        assert_eq!(find_between(data, b"X", 0, 2), Some(1));
        assert_eq!(find_between(data, b"X", 2, 3), None);
        assert_eq!(find_last(data, b"zzzzzzzzzz"), None);
        assert_eq!(find_from(b"", b"x", 0), None);
    }

    #[test]
    fn number_formatting() {
        assert_eq!(fmt(3.0), "3");
        assert_eq!(fmt(-1.0), "-1");
        assert_eq!(fmt(2.5), "2.500");
    }

    #[test]
    fn default_options_are_sane() {
        let o = SignOptions::default();
        assert_eq!(o.hash, Hash::Sha256);
        assert_eq!(o.reserved_bytes, DEFAULT_RESERVED_BYTES);
        assert!(o.appearance.is_none());
    }
}
