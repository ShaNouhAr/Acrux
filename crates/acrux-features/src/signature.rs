//! Signatures numériques (ISO 32000-2 §12.8).
//!
//! Trois questions, trois réponses séparées :
//!
//! 1. **Qu'y a-t-il dans ce fichier ?** [`list_signatures`] inventorie les
//!    champs `/Sig` du formulaire et lit ce qu'ils déclarent — signataire,
//!    date, motif, lieu, plage d'octets, sous-filtre.
//! 2. **Est-ce que ça tient ?** [`verify_signature`] rend un
//!    [`VerificationReport`] à cinq verdicts distincts. Pas de booléen global :
//!    « la signature cryptographique est bonne » et « le document n'a pas
//!    bougé » sont deux affirmations différentes, et la seconde est celle que
//!    les attaques visent.
//! 3. **Comment en poser une ?** [`sign`] ajoute un champ de signature par
//!    mise à jour incrémentale, réserve son `/Contents`, calcule le
//!    `/ByteRange` **après** écriture, et remplit la réservation sans décaler
//!    un seul octet.
//!
//! Modules : [`x509`] (certificats), [`cms`] (enveloppe `SignedData`),
//! [`verify`] (les cinq verdicts), [`sign`] (pose d'une signature).
//!
//! Limites assumées, détaillées dans `spec-notes/pdf-12.8-signatures.md` :
//! pas d'horodatage RFC 3161, pas de vérification de révocation (OCSP / CRL),
//! pas de validation à long terme (LTV), pas de signature de documents
//! chiffrés, pas de PKCS#12.

pub mod cms;
pub mod sign;
pub mod verify;
pub mod x509;

use acrux_core::{Error, Rect, Result};
use acrux_document::text::decode_text_string;
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef};

pub use sign::{sign, Appearance, SignOptions, SigningKey};
pub use verify::{verify_signature, Status, Verdict, VerificationReport};

/// Sous-filtre d'une signature (`/SubFilter`, §12.8.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubFilter {
    /// `adbe.pkcs7.detached` : CMS détaché, le format courant depuis Acrobat 6.
    AdbePkcs7Detached,
    /// `adbe.pkcs7.sha1` : CMS dont le contenu encapsulé est le condensé SHA-1
    /// du document. Obsolète et interdit en PDF 2.0, mais on sait le lire.
    AdbePkcs7Sha1,
    /// `adbe.x509.rsa_sha1` : signature RSA brute, certificat dans `/Cert`.
    AdbeX509RsaSha1,
    /// `ETSI.CAdES.detached` : CMS détaché au profil PAdES (ETSI EN 319 142).
    EtsiCadesDetached,
    /// `ETSI.RFC3161` : horodatage de document, pas une signature de personne.
    EtsiRfc3161,
    /// Sous-filtre inconnu.
    Other(String),
}

impl SubFilter {
    /// Reconnaît un sous-filtre depuis son nom PDF.
    #[must_use]
    pub fn from_name(name: &str) -> SubFilter {
        match name {
            "adbe.pkcs7.detached" => SubFilter::AdbePkcs7Detached,
            "adbe.pkcs7.sha1" => SubFilter::AdbePkcs7Sha1,
            "adbe.x509.rsa_sha1" => SubFilter::AdbeX509RsaSha1,
            "ETSI.CAdES.detached" => SubFilter::EtsiCadesDetached,
            "ETSI.RFC3161" => SubFilter::EtsiRfc3161,
            other => SubFilter::Other(other.to_string()),
        }
    }

    /// Nom PDF du sous-filtre.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            SubFilter::AdbePkcs7Detached => "adbe.pkcs7.detached",
            SubFilter::AdbePkcs7Sha1 => "adbe.pkcs7.sha1",
            SubFilter::AdbeX509RsaSha1 => "adbe.x509.rsa_sha1",
            SubFilter::EtsiCadesDetached => "ETSI.CAdES.detached",
            SubFilter::EtsiRfc3161 => "ETSI.RFC3161",
            SubFilter::Other(s) => s,
        }
    }

    /// Vrai si la valeur du `/Contents` est une enveloppe CMS.
    #[must_use]
    pub fn is_cms(&self) -> bool {
        matches!(
            self,
            SubFilter::AdbePkcs7Detached | SubFilter::AdbePkcs7Sha1 | SubFilter::EtsiCadesDetached
        )
    }
}

/// Un champ de signature et ce que son dictionnaire déclare.
///
/// Tout ce qui figure ici est **déclaratif** : le nom du signataire, la date
/// et le motif sont écrits par celui qui a signé et ne valent rien tant que
/// [`verify_signature`] n'a pas parlé. C'est exactement la confusion sur
/// laquelle jouent les faux documents signés.
#[derive(Debug, Clone)]
pub struct SignatureInfo {
    /// Nom pleinement qualifié du champ.
    pub field_name: String,
    /// Référence du dictionnaire de champ.
    pub field_reference: Option<ObjectRef>,
    /// Référence du dictionnaire de signature (`/V`).
    pub value_reference: Option<ObjectRef>,
    /// `/Filter` : le gestionnaire qui a produit la signature.
    pub filter: String,
    /// `/SubFilter` : le format du `/Contents`.
    pub sub_filter: SubFilter,
    /// `/Name` déclaré par le signataire.
    pub name: Option<String>,
    /// `/M` : date de signature déclarée, au format date PDF.
    pub date: Option<String>,
    /// `/Reason`.
    pub reason: Option<String>,
    /// `/Location`.
    pub location: Option<String>,
    /// `/ContactInfo`.
    pub contact_info: Option<String>,
    /// `/ByteRange` sous forme de couples (décalage, longueur).
    pub byte_range: Vec<(usize, usize)>,
    /// Contenu du `/Contents` (enveloppe CMS ou signature brute).
    pub contents: Vec<u8>,
    /// `/Cert` du sous-filtre `adbe.x509.rsa_sha1`.
    pub embedded_certificates: Vec<Vec<u8>>,
    /// Index de la page portant le widget, si le champ est visible.
    pub page: Option<usize>,
    /// Rectangle du widget (nul pour une signature invisible).
    pub rect: Option<Rect>,
    /// Vrai si la signature certifie le document (`/Reference` avec `/DocMDP`).
    pub certification: bool,
    /// Niveau de `/DocMDP` `/P` : 1 aucune modification, 2 remplissage de
    /// formulaire, 3 remplissage et annotations.
    pub doc_mdp_level: Option<i64>,
    /// Vrai si le champ existe mais n'a pas encore été signé (`/V` absent).
    pub unsigned: bool,
}

impl SignatureInfo {
    /// Fin de la zone couverte par `/ByteRange` (dernier octet + 1).
    #[must_use]
    pub fn covered_end(&self) -> usize {
        self.byte_range
            .iter()
            .map(|(start, len)| start.saturating_add(*len))
            .max()
            .unwrap_or(0)
    }

    /// Octets couverts par la signature, concaténés dans l'ordre déclaré.
    ///
    /// # Errors
    /// Une plage sort du fichier.
    pub fn covered_bytes(&self, data: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for (start, len) in &self.byte_range {
            let end = start
                .checked_add(*len)
                .ok_or_else(|| Error::Corrupt("/ByteRange déborde".into()))?;
            let slice = data.get(*start..end).ok_or_else(|| {
                Error::Corrupt(format!("/ByteRange [{start} {len}] hors fichier"))
            })?;
            out.extend_from_slice(slice);
        }
        Ok(out)
    }

    /// Intervalle laissé hors couverture entre la première et la deuxième
    /// plage : il doit contenir exactement la chaîne `/Contents`.
    #[must_use]
    pub fn gap(&self) -> Option<(usize, usize)> {
        let first = self.byte_range.first()?;
        let second = self.byte_range.get(1)?;
        let start = first.0.checked_add(first.1)?;
        if second.0 < start {
            return None;
        }
        Some((start, second.0))
    }
}

/// Inventaire des champs de signature du document.
///
/// Les champs non signés (un emplacement réservé, `/V` absent) sont inclus et
/// marqués par [`SignatureInfo::unsigned`] : un document qui en contient peut
/// encore recevoir une signature.
///
/// # Errors
/// Catalogue ou arbre de champs illisible.
pub fn list_signatures(doc: &Document) -> Result<Vec<SignatureInfo>> {
    let pages = collect_pages(doc)?;
    let mut page_of = std::collections::HashMap::new();
    for (index, page) in pages.iter().enumerate() {
        let Some(annots) = page.dict.get(&Name::new("Annots")) else {
            continue;
        };
        let Ok(annots) = doc.resolve(annots) else {
            continue;
        };
        let Some(list) = annots.as_array() else {
            continue;
        };
        for a in list {
            if let Object::Reference(r) = a {
                page_of.insert(r.number, index);
            }
        }
    }
    let mut out = Vec::new();
    for field in crate::forms::list_fields(doc)? {
        if field.kind != crate::forms::FieldType::Signature {
            continue;
        }
        let widget = field.widgets.first();
        let mut info = SignatureInfo {
            field_name: field.name.clone(),
            field_reference: field.reference,
            value_reference: None,
            filter: String::new(),
            sub_filter: SubFilter::Other(String::new()),
            name: None,
            date: None,
            reason: None,
            location: None,
            contact_info: None,
            byte_range: Vec::new(),
            contents: Vec::new(),
            embedded_certificates: Vec::new(),
            page: widget.and_then(|w| w.page).or_else(|| {
                field
                    .reference
                    .and_then(|r| page_of.get(&r.number).copied())
            }),
            rect: widget.map(|w| w.rect),
            certification: false,
            doc_mdp_level: None,
            unsigned: true,
        };
        if let Some(reference) = field.reference {
            if let Ok(object) = doc.get(reference) {
                if let Some(dict) = object.as_dict() {
                    read_value(doc, dict, &mut info);
                }
            }
        }
        out.push(info);
    }
    Ok(out)
}

/// Lit `/V` (en remontant les `/Parent` si besoin) et remplit `info`.
#[allow(clippy::too_many_lines)] // une clé du dictionnaire /Sig après l'autre
fn read_value(doc: &Document, field: &Dict, info: &mut SignatureInfo) {
    let mut current = field.clone();
    let mut value = None;
    for _ in 0..16 {
        if let Some(v) = current.get(&Name::new("V")) {
            if let Object::Reference(r) = v {
                info.value_reference = Some(*r);
            }
            value = doc.resolve(v).ok().and_then(|o| o.as_dict().cloned());
            break;
        }
        let Some(parent) = doc.dict_get(&current, "Parent").ok().flatten() else {
            break;
        };
        let Some(parent) = parent.as_dict() else {
            break;
        };
        current = parent.clone();
    }
    let Some(signature) = value else { return };
    info.unsigned = false;
    let name_of = |key: &str| -> Option<String> {
        doc.dict_get(&signature, key)
            .ok()
            .flatten()
            .and_then(|o| o.as_name().map(Name::as_str))
    };
    let text_of = |key: &str| -> Option<String> {
        match &*doc.dict_get(&signature, key).ok().flatten()? {
            Object::String(s) => Some(decode_text_string(s)),
            _ => None,
        }
    };
    info.filter = name_of("Filter").unwrap_or_default();
    info.sub_filter = SubFilter::from_name(&name_of("SubFilter").unwrap_or_default());
    info.name = text_of("Name");
    info.date = text_of("M");
    info.reason = text_of("Reason");
    info.location = text_of("Location");
    info.contact_info = text_of("ContactInfo");
    if let Some(Object::String(s)) = doc
        .dict_get(&signature, "Contents")
        .ok()
        .flatten()
        .map(|o| (*o).clone())
    {
        info.contents = s;
    }
    if let Some(range) = doc.dict_get(&signature, "ByteRange").ok().flatten() {
        if let Some(array) = range.as_array() {
            let numbers: Vec<i64> = array
                .iter()
                .filter_map(|o| doc.resolve(o).ok().and_then(|v| v.as_i64()))
                .collect();
            for pair in numbers.chunks_exact(2) {
                let (Some(start), Some(len)) = (pair.first(), pair.get(1)) else {
                    continue;
                };
                if let (Ok(s), Ok(l)) = (usize::try_from(*start), usize::try_from(*len)) {
                    info.byte_range.push((s, l));
                }
            }
        }
    }
    // /Cert : une chaîne, ou un tableau de chaînes (le signataire d'abord).
    if let Some(cert) = doc.dict_get(&signature, "Cert").ok().flatten() {
        match &*cert {
            Object::String(s) => info.embedded_certificates.push(s.clone()),
            Object::Array(list) => {
                for item in list {
                    if let Ok(resolved) = doc.resolve(item) {
                        if let Object::String(s) = &*resolved {
                            info.embedded_certificates.push(s.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    // /Reference [ << /TransformMethod /DocMDP /TransformParams << /P n >> >> ]
    if let Some(reference) = doc.dict_get(&signature, "Reference").ok().flatten() {
        if let Some(list) = reference.as_array() {
            for item in list {
                let Ok(resolved) = doc.resolve(item) else {
                    continue;
                };
                let Some(dict) = resolved.as_dict() else {
                    continue;
                };
                let method = doc
                    .dict_get(dict, "TransformMethod")
                    .ok()
                    .flatten()
                    .and_then(|o| o.as_name().map(Name::as_str));
                if method.as_deref() != Some("DocMDP") {
                    continue;
                }
                info.certification = true;
                if let Some(params) = doc.dict_get(dict, "TransformParams").ok().flatten() {
                    if let Some(p) = params.as_dict() {
                        info.doc_mdp_level =
                            doc.dict_get(p, "P").ok().flatten().and_then(|o| o.as_i64());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod corpus;

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn sub_filter_names_roundtrip() {
        for name in [
            "adbe.pkcs7.detached",
            "adbe.pkcs7.sha1",
            "adbe.x509.rsa_sha1",
            "ETSI.CAdES.detached",
            "ETSI.RFC3161",
            "quelque.chose",
        ] {
            assert_eq!(SubFilter::from_name(name).name(), name);
        }
        assert!(SubFilter::AdbePkcs7Detached.is_cms());
        assert!(SubFilter::EtsiCadesDetached.is_cms());
        assert!(!SubFilter::AdbeX509RsaSha1.is_cms());
        assert!(!SubFilter::EtsiRfc3161.is_cms());
    }

    #[test]
    fn byte_range_arithmetic() {
        let info = SignatureInfo {
            field_name: "Signature1".into(),
            field_reference: None,
            value_reference: None,
            filter: "Adobe.PPKLite".into(),
            sub_filter: SubFilter::AdbePkcs7Detached,
            name: None,
            date: None,
            reason: None,
            location: None,
            contact_info: None,
            byte_range: vec![(0, 10), (30, 20)],
            contents: Vec::new(),
            embedded_certificates: Vec::new(),
            page: None,
            rect: None,
            certification: false,
            doc_mdp_level: None,
            unsigned: false,
        };
        assert_eq!(info.covered_end(), 50);
        assert_eq!(info.gap(), Some((10, 30)));
        let data: Vec<u8> = (0u8..50).collect();
        let covered = info.covered_bytes(&data).unwrap();
        assert_eq!(covered.len(), 30);
        assert_eq!(covered.first(), Some(&0));
        assert_eq!(covered.get(10), Some(&30));
        // Plage hors fichier.
        assert!(info.covered_bytes(&data[..40]).is_err());
    }
}
