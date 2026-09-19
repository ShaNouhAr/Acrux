//! Certificats X.509 v3 : lecture, vérification et fabrication (RFC 5280).
//!
//! Nous lisons ce dont une signature PDF a besoin — identités, validité,
//! clé publique, `keyUsage`, `extKeyUsage`, `basicConstraints`, identifiants
//! de clé — et nous savons **émettre** un certificat, ce qui sert à fabriquer
//! l'autorité de test du dépôt et, accessoirement, prouve que notre lecteur
//! et notre écrivain DER se répondent.
//!
//! Ce que nous ne faisons pas : politiques de certification, contraintes de
//! noms, `CRLDistributionPoints`, OCSP. Une extension **critique** que nous ne
//! savons pas traiter fait échouer la validation, comme l'exige la RFC 5280
//! §6.1.3 (f) — c'est le seul comportement sûr.

use std::fmt;

use acrux_core::{Error, Result};
use acrux_document::asn1::{self, Oid, Reader, Time, Tlv};
use acrux_document::crypt::rsa::{self, Hash, PrivateKey, PublicKey};
use acrux_document::crypt::sha1;

/// `2.5.29.14` — `subjectKeyIdentifier`.
pub const OID_SUBJECT_KEY_ID: [u32; 4] = [2, 5, 29, 14];
/// `2.5.29.15` — `keyUsage`.
pub const OID_KEY_USAGE: [u32; 4] = [2, 5, 29, 15];
/// `2.5.29.17` — `subjectAltName`.
pub const OID_SUBJECT_ALT_NAME: [u32; 4] = [2, 5, 29, 17];
/// `2.5.29.19` — `basicConstraints`.
pub const OID_BASIC_CONSTRAINTS: [u32; 4] = [2, 5, 29, 19];
/// `2.5.29.35` — `authorityKeyIdentifier`.
pub const OID_AUTHORITY_KEY_ID: [u32; 4] = [2, 5, 29, 35];
/// `2.5.29.37` — `extKeyUsage`.
pub const OID_EXT_KEY_USAGE: [u32; 4] = [2, 5, 29, 37];
/// `1.3.6.1.5.5.7.3.36` — `id-kp-documentSigning` (RFC 9336).
pub const OID_EKU_DOCUMENT_SIGNING: [u32; 9] = [1, 3, 6, 1, 5, 5, 7, 3, 36];
/// `1.3.6.1.5.5.7.3.4` — `id-kp-emailProtection`, historiquement employé par
/// Acrobat pour les certificats de signature.
pub const OID_EKU_EMAIL_PROTECTION: [u32; 9] = [1, 3, 6, 1, 5, 5, 7, 3, 4];
/// `2.5.29.37.0` — `anyExtendedKeyUsage`.
pub const OID_EKU_ANY: [u32; 5] = [2, 5, 29, 37, 0];

/// Bits de l'extension `keyUsage`, dans l'ordre de la RFC 5280 §4.2.1.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyUsageBit {
    /// Signature de données.
    DigitalSignature,
    /// Non-répudiation (nommée `contentCommitment` depuis la RFC 5280).
    ContentCommitment,
    /// Chiffrement de clé.
    KeyEncipherment,
    /// Chiffrement de données.
    DataEncipherment,
    /// Accord de clé.
    KeyAgreement,
    /// Signature de certificats : indispensable à une autorité.
    KeyCertSign,
    /// Signature de listes de révocation.
    CrlSign,
    /// Chiffrement seul dans un accord de clé.
    EncipherOnly,
    /// Déchiffrement seul dans un accord de clé.
    DecipherOnly,
}

impl KeyUsageBit {
    /// Rang du bit dans le `BIT STRING`.
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            KeyUsageBit::DigitalSignature => 0,
            KeyUsageBit::ContentCommitment => 1,
            KeyUsageBit::KeyEncipherment => 2,
            KeyUsageBit::DataEncipherment => 3,
            KeyUsageBit::KeyAgreement => 4,
            KeyUsageBit::KeyCertSign => 5,
            KeyUsageBit::CrlSign => 6,
            KeyUsageBit::EncipherOnly => 7,
            KeyUsageBit::DecipherOnly => 8,
        }
    }

    /// Libellé court en français.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            KeyUsageBit::DigitalSignature => "signature",
            KeyUsageBit::ContentCommitment => "non-répudiation",
            KeyUsageBit::KeyEncipherment => "chiffrement de clé",
            KeyUsageBit::DataEncipherment => "chiffrement de données",
            KeyUsageBit::KeyAgreement => "accord de clé",
            KeyUsageBit::KeyCertSign => "signature de certificats",
            KeyUsageBit::CrlSign => "signature de LCR",
            KeyUsageBit::EncipherOnly => "chiffrement seul",
            KeyUsageBit::DecipherOnly => "déchiffrement seul",
        }
    }
}

/// Extension `keyUsage` décodée.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyUsage {
    /// Bits présents, du rang 0 au rang 8.
    pub bits: Vec<bool>,
    /// Vrai si l'extension est marquée critique.
    pub critical: bool,
}

impl KeyUsage {
    /// Vrai si le bit demandé est présent et à 1.
    #[must_use]
    pub fn has(&self, bit: KeyUsageBit) -> bool {
        self.bits.get(bit.index()).copied().unwrap_or(false)
    }

    /// Liste lisible des usages présents.
    #[must_use]
    pub fn labels(&self) -> Vec<&'static str> {
        [
            KeyUsageBit::DigitalSignature,
            KeyUsageBit::ContentCommitment,
            KeyUsageBit::KeyEncipherment,
            KeyUsageBit::DataEncipherment,
            KeyUsageBit::KeyAgreement,
            KeyUsageBit::KeyCertSign,
            KeyUsageBit::CrlSign,
            KeyUsageBit::EncipherOnly,
            KeyUsageBit::DecipherOnly,
        ]
        .into_iter()
        .filter(|b| self.has(*b))
        .map(KeyUsageBit::label)
        .collect()
    }
}

/// Extension `basicConstraints`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BasicConstraints {
    /// Vrai si le certificat est celui d'une autorité.
    pub ca: bool,
    /// Nombre maximal d'autorités intermédiaires en dessous.
    pub path_len: Option<i64>,
    /// Vrai si l'extension est marquée critique.
    pub critical: bool,
}

/// Algorithme de signature d'un certificat ou d'un `SignerInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureAlgorithm {
    /// `RSASSA-PKCS1-v1_5` avec la fonction de condensation donnée.
    RsaPkcs1(Hash),
    /// `RSASSA-PSS` : condensat, fonction de masque, longueur du sel.
    RsaPss {
        /// Fonction de condensation du message.
        hash: Hash,
        /// Fonction de condensation employée par MGF1.
        mgf_hash: Hash,
        /// Longueur du sel en octets.
        salt_len: usize,
    },
    /// Algorithme reconnu syntaxiquement mais non pris en charge.
    Unsupported(Oid),
}

impl SignatureAlgorithm {
    /// Fonction de condensation associée, si elle est connue.
    #[must_use]
    pub fn hash(&self) -> Option<Hash> {
        match self {
            SignatureAlgorithm::RsaPkcs1(h) => Some(*h),
            SignatureAlgorithm::RsaPss { hash, .. } => Some(*hash),
            SignatureAlgorithm::Unsupported(_) => None,
        }
    }

    /// Libellé lisible.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            SignatureAlgorithm::RsaPkcs1(h) => format!("RSA PKCS#1 v1.5 / {}", h.label()),
            SignatureAlgorithm::RsaPss { hash, salt_len, .. } => {
                format!("RSASSA-PSS / {} (sel {salt_len})", hash.label())
            }
            SignatureAlgorithm::Unsupported(o) => format!("non pris en charge ({o})"),
        }
    }

    /// Vérifie une signature sur un message déjà condensé ou non.
    #[must_use]
    pub fn verify(&self, key: &PublicKey, message: &[u8], signature: &[u8]) -> bool {
        match self {
            SignatureAlgorithm::RsaPkcs1(h) => {
                key.verify_pkcs1_v15(*h, &h.digest(message), signature)
            }
            SignatureAlgorithm::RsaPss {
                hash,
                mgf_hash,
                salt_len,
            } => key.verify_pss(
                *hash,
                *mgf_hash,
                Some(*salt_len),
                &hash.digest(message),
                signature,
            ),
            SignatureAlgorithm::Unsupported(_) => false,
        }
    }

    /// Lit un `AlgorithmIdentifier` déjà ouvert.
    ///
    /// # Errors
    /// Structure invalide.
    pub fn parse(reader: &mut Reader<'_>) -> Result<SignatureAlgorithm> {
        let mut alg = reader.sequence()?;
        let oid = alg.oid()?;
        if oid.0 == rsa::OID_RSASSA_PSS {
            return parse_pss_parameters(&mut alg);
        }
        match Hash::from_oid(&oid) {
            Some(h) if oid.0.len() == 7 => Ok(SignatureAlgorithm::RsaPkcs1(h)),
            _ => Ok(SignatureAlgorithm::Unsupported(oid)),
        }
    }
}

/// `RSASSA-PSS-params` (RFC 8017 annexe A.2.3), valeurs par défaut comprises.
fn parse_pss_parameters(alg: &mut Reader<'_>) -> Result<SignatureAlgorithm> {
    let mut hash = Hash::Sha1;
    let mut mgf_hash = Hash::Sha1;
    let mut salt_len = 20usize;
    if alg.is_empty() {
        return Ok(SignatureAlgorithm::RsaPss {
            hash,
            mgf_hash,
            salt_len,
        });
    }
    let mut params = alg.sequence()?;
    if params.peek_is_context(0) {
        let mut inner = params.context(0)?;
        let mut a = inner.sequence()?;
        hash = Hash::from_oid(&a.oid()?).unwrap_or(Hash::Sha1);
    }
    if params.peek_is_context(1) {
        let mut inner = params.context(1)?;
        let mut a = inner.sequence()?;
        let mgf = a.oid()?;
        if mgf.0 != rsa::OID_MGF1 {
            return Ok(SignatureAlgorithm::Unsupported(mgf));
        }
        let mut h = a.sequence()?;
        mgf_hash = Hash::from_oid(&h.oid()?).unwrap_or(Hash::Sha1);
    }
    if params.peek_is_context(2) {
        let mut inner = params.context(2)?;
        salt_len = usize::try_from(inner.integer_i64()?).unwrap_or(20);
    }
    Ok(SignatureAlgorithm::RsaPss {
        hash,
        mgf_hash,
        salt_len,
    })
}

/// Nom distinctif : la suite d'attributs, plus les octets DER d'origine.
///
/// Les octets sont conservés parce que l'appariement d'un `SignerInfo` avec
/// son certificat se fait par **comparaison binaire** du nom de l'émetteur
/// (RFC 5652 §5.3) : re-normaliser un nom, c'est risquer de faire correspondre
/// deux identités différentes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistinguishedName {
    /// Attributs dans l'ordre du certificat.
    pub attributes: Vec<(Oid, String)>,
    /// Codage DER complet du `Name`.
    pub der: Vec<u8>,
}

impl DistinguishedName {
    /// Valeur du premier attribut portant cet OID.
    #[must_use]
    pub fn get(&self, oid: &[u32]) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(o, _)| o.0 == oid)
            .map(|(_, v)| v.as_str())
    }

    /// Nom commun (`CN`).
    #[must_use]
    pub fn common_name(&self) -> Option<&str> {
        self.get(&[2, 5, 4, 3])
    }

    /// Rendu lisible, du plus spécifique au plus général (style RFC 4514).
    #[must_use]
    pub fn to_display(&self) -> String {
        let parts: Vec<String> = self
            .attributes
            .iter()
            .rev()
            .map(|(o, v)| format!("{}={v}", attribute_label(o)))
            .collect();
        if parts.is_empty() {
            "(nom vide)".to_string()
        } else {
            parts.join(", ")
        }
    }
}

impl fmt::Display for DistinguishedName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_display())
    }
}

/// Abréviation usuelle d'un attribut de nom distinctif.
fn attribute_label(oid: &Oid) -> String {
    match oid.0.as_slice() {
        [2, 5, 4, 3] => "CN".into(),
        [2, 5, 4, 4] => "SN".into(),
        [2, 5, 4, 5] => "serialNumber".into(),
        [2, 5, 4, 6] => "C".into(),
        [2, 5, 4, 7] => "L".into(),
        [2, 5, 4, 8] => "ST".into(),
        [2, 5, 4, 10] => "O".into(),
        [2, 5, 4, 11] => "OU".into(),
        [2, 5, 4, 42] => "GN".into(),
        [1, 2, 840, 113_549, 1, 9, 1] => "E".into(),
        [0, 9, 2342, 19_200_300, 100, 1, 25] => "DC".into(),
        _ => oid.to_string(),
    }
}

/// OID d'un attribut de nom depuis son abréviation.
///
/// # Errors
/// Abréviation inconnue.
pub fn attribute_oid(label: &str) -> Result<Oid> {
    let arcs: &[u32] = match label {
        "CN" => &[2, 5, 4, 3],
        "SN" => &[2, 5, 4, 4],
        "serialNumber" => &[2, 5, 4, 5],
        "C" => &[2, 5, 4, 6],
        "L" => &[2, 5, 4, 7],
        "ST" => &[2, 5, 4, 8],
        "O" => &[2, 5, 4, 10],
        "OU" => &[2, 5, 4, 11],
        "GN" => &[2, 5, 4, 42],
        "E" => &[1, 2, 840, 113_549, 1, 9, 1],
        "DC" => &[0, 9, 2342, 19_200_300, 100, 1, 25],
        other => return Oid::parse(other),
    };
    Ok(Oid(arcs.to_vec()))
}

/// Certificat X.509 lu.
#[derive(Debug, Clone)]
pub struct Certificate {
    /// Codage DER complet.
    pub der: Vec<u8>,
    /// Octets du `tbsCertificate`, ceux que couvre la signature.
    pub tbs: Vec<u8>,
    /// Version (1, 2 ou 3).
    pub version: i64,
    /// Numéro de série, tel quel (il peut dépasser 64 bits).
    pub serial: Vec<u8>,
    /// Émetteur.
    pub issuer: DistinguishedName,
    /// Sujet.
    pub subject: DistinguishedName,
    /// Début de validité.
    pub not_before: Time,
    /// Fin de validité.
    pub not_after: Time,
    /// Clé publique du sujet.
    pub public_key: PublicKey,
    /// Algorithme de la signature apposée par l'émetteur.
    pub signature_algorithm: SignatureAlgorithm,
    /// Valeur de la signature.
    pub signature: Vec<u8>,
    /// `keyUsage`, si présent.
    pub key_usage: Option<KeyUsage>,
    /// `extKeyUsage`, si présent.
    pub extended_key_usage: Vec<Oid>,
    /// `basicConstraints`, si présent.
    pub basic_constraints: Option<BasicConstraints>,
    /// `subjectKeyIdentifier`.
    pub subject_key_id: Option<Vec<u8>>,
    /// `authorityKeyIdentifier` (partie `keyIdentifier`).
    pub authority_key_id: Option<Vec<u8>>,
    /// Extensions critiques que nous ne savons pas traiter.
    pub unknown_critical: Vec<Oid>,
}

impl Certificate {
    /// Lit un certificat DER.
    ///
    /// # Errors
    /// Structure invalide, ou clé publique non RSA.
    #[allow(clippy::too_many_lines)] // décalque la structure TBSCertificate
    pub fn parse(der: &[u8]) -> Result<Certificate> {
        let mut root = Reader::new(der);
        let mut cert = root.sequence()?;
        root.end()?;
        let tbs_tlv = cert.peek()?;
        let mut tbs = cert.sub(tbs_tlv);
        cert.skip()?;
        if !tbs_tlv.is(asn1::SEQUENCE) {
            return Err(Error::Corrupt("tbsCertificate absent".into()));
        }
        let signature_algorithm = SignatureAlgorithm::parse(&mut cert)?;
        let (unused, signature) = cert.bit_string()?;
        if unused != 0 {
            return Err(Error::Corrupt("signature de certificat mal alignée".into()));
        }
        cert.end()?;

        let version = if tbs.peek_is_context(0) {
            let mut v = tbs.context(0)?;
            let n = v.integer_i64()?;
            v.end()?;
            n + 1
        } else {
            1
        };
        let serial = tbs.integer_bytes()?.to_vec();
        let inner_algorithm = SignatureAlgorithm::parse(&mut tbs)?;
        if inner_algorithm != signature_algorithm {
            return Err(Error::Corrupt(
                "algorithme de signature incohérent entre le certificat et son tbsCertificate"
                    .into(),
            ));
        }
        let issuer = parse_name(&mut tbs)?;
        let mut validity = tbs.sequence()?;
        let not_before = validity.time()?;
        let not_after = validity.time()?;
        validity.end()?;
        let subject = parse_name(&mut tbs)?;
        let spki_tlv = tbs.peek()?;
        tbs.skip()?;
        let public_key = PublicKey::from_spki_der(spki_tlv.raw)?;

        // [1] et [2] : identifiants uniques de la version 2, ignorés.
        if tbs.peek_is_context(1) {
            tbs.skip()?;
        }
        if tbs.peek_is_context(2) {
            tbs.skip()?;
        }
        let mut certificate = Certificate {
            der: der.to_vec(),
            tbs: tbs_tlv.raw.to_vec(),
            version,
            serial,
            issuer,
            subject,
            not_before,
            not_after,
            public_key,
            signature_algorithm,
            signature: signature.to_vec(),
            key_usage: None,
            extended_key_usage: Vec::new(),
            basic_constraints: None,
            subject_key_id: None,
            authority_key_id: None,
            unknown_critical: Vec::new(),
        };
        if tbs.peek_is_context(3) {
            let mut wrapper = tbs.context(3)?;
            let mut list = wrapper.sequence()?;
            wrapper.end()?;
            while !list.is_empty() {
                let mut ext = list.sequence()?;
                let oid = ext.oid()?;
                let critical = if ext.peek_is(asn1::BOOLEAN) {
                    ext.boolean()?
                } else {
                    false
                };
                let value = ext.octet_string()?;
                ext.end()?;
                certificate.absorb_extension(&oid, critical, value)?;
            }
        }
        tbs.end()?;
        Ok(certificate)
    }

    fn absorb_extension(&mut self, oid: &Oid, critical: bool, value: &[u8]) -> Result<()> {
        let mut r = Reader::new(value);
        match oid.0.as_slice() {
            s if s == OID_KEY_USAGE => {
                let (unused, bytes) = r.bit_string()?;
                let total = bytes.len() * 8 - usize::from(unused);
                let mut bits = Vec::with_capacity(total);
                for i in 0..total {
                    let byte = bytes.get(i / 8).copied().unwrap_or(0);
                    bits.push(byte & (0x80 >> (i % 8)) != 0);
                }
                self.key_usage = Some(KeyUsage { bits, critical });
            }
            s if s == OID_BASIC_CONSTRAINTS => {
                let mut seq = r.sequence()?;
                let ca = if seq.peek_is(asn1::BOOLEAN) {
                    seq.boolean()?
                } else {
                    false
                };
                let path_len = if seq.peek_is(asn1::INTEGER) {
                    Some(seq.integer_i64()?)
                } else {
                    None
                };
                self.basic_constraints = Some(BasicConstraints {
                    ca,
                    path_len,
                    critical,
                });
            }
            s if s == OID_EXT_KEY_USAGE => {
                let mut seq = r.sequence()?;
                while !seq.is_empty() {
                    self.extended_key_usage.push(seq.oid()?);
                }
            }
            s if s == OID_SUBJECT_KEY_ID => {
                self.subject_key_id = Some(r.octet_string()?.to_vec());
            }
            s if s == OID_AUTHORITY_KEY_ID => {
                let mut seq = r.sequence()?;
                if seq.peek_is_context(0) {
                    let tlv = seq.read()?;
                    self.authority_key_id = Some(tlv.content.to_vec());
                }
            }
            s if s == OID_SUBJECT_ALT_NAME => {}
            _ => {
                if critical {
                    self.unknown_critical.push(oid.clone());
                }
            }
        }
        Ok(())
    }

    /// Vrai si l'instant `at` tombe dans la période de validité.
    #[must_use]
    pub fn is_valid_at(&self, at: Time) -> bool {
        at >= self.not_before && at <= self.not_after
    }

    /// Vérifie que ce certificat a bien été signé par `issuer`.
    #[must_use]
    pub fn is_signed_by(&self, issuer: &Certificate) -> bool {
        if self.issuer.der != issuer.subject.der {
            return false;
        }
        self.signature_algorithm
            .verify(&issuer.public_key, &self.tbs, &self.signature)
    }

    /// Vrai si le certificat se signe lui-même (racine, ou certificat isolé).
    #[must_use]
    pub fn is_self_signed(&self) -> bool {
        self.is_signed_by(self)
    }

    /// Vrai si le certificat peut émettre d'autres certificats.
    #[must_use]
    pub fn is_certificate_authority(&self) -> bool {
        let ca = self.basic_constraints.is_some_and(|b| b.ca);
        let usage_ok = self
            .key_usage
            .as_ref()
            .is_none_or(|u| u.has(KeyUsageBit::KeyCertSign));
        ca && usage_ok
    }

    /// Vrai si le certificat autorise la signature de documents : `keyUsage`
    /// absent, ou portant `digitalSignature` ou `contentCommitment`.
    #[must_use]
    pub fn allows_document_signing(&self) -> bool {
        self.key_usage.as_ref().is_none_or(|u| {
            u.has(KeyUsageBit::DigitalSignature) || u.has(KeyUsageBit::ContentCommitment)
        })
    }

    /// Vrai si `extKeyUsage` est absent ou compatible avec la signature de
    /// documents.
    #[must_use]
    pub fn extended_usage_allows_signing(&self) -> bool {
        if self.extended_key_usage.is_empty() {
            return true;
        }
        self.extended_key_usage.iter().any(|o| {
            o.0 == OID_EKU_ANY || o.0 == OID_EKU_DOCUMENT_SIGNING || o.0 == OID_EKU_EMAIL_PROTECTION
        })
    }

    /// Identifiant de clé calculé selon la méthode 1 de la RFC 5280 §4.2.1.2 :
    /// SHA-1 des octets de la clé publique. SHA-1 ne sert ici que
    /// d'identifiant, jamais de preuve.
    #[must_use]
    pub fn computed_key_id(&self) -> Vec<u8> {
        sha1::sha1(&self.public_key.to_pkcs1_der()).to_vec()
    }

    /// Résumé d'une ligne, pour les rapports.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} (émis par {}, valide du {} au {}, RSA {} bits)",
            self.subject,
            self.issuer,
            self.not_before.to_display(),
            self.not_after.to_display(),
            self.public_key.bits()
        )
    }
}

/// Lit un `Name` (RDNSequence).
fn parse_name(reader: &mut Reader<'_>) -> Result<DistinguishedName> {
    let tlv = reader.peek()?;
    let mut seq = reader.sequence()?;
    let mut attributes = Vec::new();
    while !seq.is_empty() {
        let mut rdn = seq.set()?;
        while !rdn.is_empty() {
            let mut pair = rdn.sequence()?;
            let oid = pair.oid()?;
            let value = pair
                .string()
                .unwrap_or_else(|_| "(valeur illisible)".into());
            attributes.push((oid, value));
        }
    }
    Ok(DistinguishedName {
        attributes,
        der: tlv.raw.to_vec(),
    })
}

/// Lit une suite de certificats DER concaténés (le format d'un dossier de
/// racines aplati, et de `[0] IMPLICIT CertificateSet` du CMS).
///
/// # Errors
/// Un des certificats est illisible.
pub fn parse_many(der: &[u8]) -> Result<Vec<Certificate>> {
    let mut out = Vec::new();
    let mut reader = Reader::new(der);
    while !reader.is_empty() {
        let tlv: Tlv<'_> = reader.read()?;
        out.push(Certificate::parse(tlv.raw)?);
    }
    Ok(out)
}

// --- Émission --------------------------------------------------------------

/// Gabarit d'un certificat à émettre.
#[derive(Debug, Clone)]
pub struct CertificateTemplate {
    /// Numéro de série (gros-boutiste, non nul).
    pub serial: Vec<u8>,
    /// Attributs du sujet, du plus général au plus spécifique (`C`, `O`, `CN`).
    pub subject: Vec<(String, String)>,
    /// Attributs de l'émetteur (identiques au sujet pour un auto-signé).
    pub issuer: Vec<(String, String)>,
    /// Début de validité.
    pub not_before: Time,
    /// Fin de validité.
    pub not_after: Time,
    /// Clé publique du sujet.
    pub subject_key: PublicKey,
    /// Vrai pour une autorité de certification.
    pub ca: bool,
    /// `pathLenConstraint` éventuel.
    pub path_len: Option<i64>,
    /// Bits de `keyUsage` à poser.
    pub key_usage: Vec<KeyUsageBit>,
    /// OID d'`extKeyUsage` à poser.
    pub extended_key_usage: Vec<Oid>,
    /// Identifiant de clé de l'émetteur, pour `authorityKeyIdentifier`.
    pub authority_key_id: Option<Vec<u8>>,
}

/// Code un `Name` depuis des couples `(abréviation, valeur)`.
///
/// # Errors
/// Abréviation d'attribut inconnue.
pub fn encode_name(attributes: &[(String, String)]) -> Result<Vec<u8>> {
    let mut rdns = Vec::new();
    for (label, value) in attributes {
        let oid = attribute_oid(label)?;
        // UTF8String partout : PrintableString perdrait les accents, et tous
        // les lecteurs modernes acceptent l'UTF-8 (RFC 5280 §4.1.2.4).
        let pair =
            asn1::write::sequence(&[asn1::write::oid(&oid), asn1::write::utf8_string(value)]);
        rdns.push(asn1::write::set_of(&[pair]));
    }
    Ok(asn1::write::sequence(&rdns))
}

/// Émet un certificat X.509 v3 signé par `issuer_key`.
///
/// # Errors
/// Attribut de nom inconnu, ou signature impossible.
pub fn issue(
    template: &CertificateTemplate,
    issuer_key: &PrivateKey,
    hash: Hash,
) -> Result<Vec<u8>> {
    let algorithm = asn1::write::algorithm(&hash.signature_oid(), Some(&asn1::write::null()));
    let mut extensions = Vec::new();
    extensions.push(extension(&Oid(OID_BASIC_CONSTRAINTS.to_vec()), true, &{
        let mut parts = Vec::new();
        if template.ca {
            parts.push(asn1::write::boolean(true));
        }
        if let Some(p) = template.path_len {
            parts.push(asn1::write::integer_i64(p));
        }
        asn1::write::sequence(&parts)
    }));
    if !template.key_usage.is_empty() {
        let mut bits = vec![false; 9];
        for u in &template.key_usage {
            if let Some(slot) = bits.get_mut(u.index()) {
                *slot = true;
            }
        }
        extensions.push(extension(
            &Oid(OID_KEY_USAGE.to_vec()),
            true,
            &asn1::write::named_bits(&bits),
        ));
    }
    if !template.extended_key_usage.is_empty() {
        let list: Vec<Vec<u8>> = template
            .extended_key_usage
            .iter()
            .map(asn1::write::oid)
            .collect();
        extensions.push(extension(
            &Oid(OID_EXT_KEY_USAGE.to_vec()),
            false,
            &asn1::write::sequence(&list),
        ));
    }
    let ski = sha1::sha1(&template.subject_key.to_pkcs1_der()).to_vec();
    extensions.push(extension(
        &Oid(OID_SUBJECT_KEY_ID.to_vec()),
        false,
        &asn1::write::octet_string(&ski),
    ));
    if let Some(akid) = &template.authority_key_id {
        extensions.push(extension(
            &Oid(OID_AUTHORITY_KEY_ID.to_vec()),
            false,
            &asn1::write::sequence(&[asn1::write::context_primitive(0, akid)]),
        ));
    }
    let tbs = asn1::write::sequence(&[
        // version [0] EXPLICIT : 2 = v3.
        asn1::write::context(0, &asn1::write::integer_i64(2)),
        asn1::write::unsigned_integer(&template.serial),
        algorithm.clone(),
        encode_name(&template.issuer)?,
        asn1::write::sequence(&[
            asn1::write::x509_time(template.not_before),
            asn1::write::x509_time(template.not_after),
        ]),
        encode_name(&template.subject)?,
        template.subject_key.to_spki_der(),
        asn1::write::context(3, &asn1::write::sequence(&extensions)),
    ]);
    let signature = issuer_key.sign_pkcs1_v15(hash, &hash.digest(&tbs))?;
    Ok(asn1::write::sequence(&[
        tbs,
        algorithm,
        asn1::write::bit_string(&signature),
    ]))
}

fn extension(oid: &Oid, critical: bool, value: &[u8]) -> Vec<u8> {
    let mut parts = vec![asn1::write::oid(oid)];
    if critical {
        parts.push(asn1::write::boolean(true));
    }
    parts.push(asn1::write::octet_string(value));
    asn1::write::sequence(&parts)
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use acrux_document::crypt::rsa::{generate, SeededRandom};

    fn test_key() -> PrivateKey {
        let mut rng = SeededRandom::new(b"x509 tests 512");
        generate(512, &mut rng).unwrap()
    }

    fn dn(cn: &str) -> Vec<(String, String)> {
        vec![
            ("C".into(), "FR".into()),
            ("O".into(), "Acrux".into()),
            ("CN".into(), cn.into()),
        ]
    }

    fn moment(year: i32) -> Time {
        Time {
            year,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
        }
    }

    #[test]
    fn self_signed_roundtrip() {
        let key = test_key();
        let template = CertificateTemplate {
            serial: vec![0x01, 0x02, 0x03],
            subject: dn("Racine de test"),
            issuer: dn("Racine de test"),
            not_before: moment(2020),
            not_after: moment(2040),
            subject_key: key.public.clone(),
            ca: true,
            path_len: Some(1),
            key_usage: vec![KeyUsageBit::KeyCertSign, KeyUsageBit::CrlSign],
            extended_key_usage: Vec::new(),
            authority_key_id: None,
        };
        let der = issue(&template, &key, Hash::Sha256).unwrap();
        let cert = Certificate::parse(&der).unwrap();
        assert_eq!(cert.version, 3);
        assert_eq!(cert.serial, vec![0x01, 0x02, 0x03]);
        assert_eq!(cert.subject.common_name(), Some("Racine de test"));
        assert_eq!(
            cert.subject.to_display(),
            "CN=Racine de test, O=Acrux, C=FR"
        );
        assert_eq!(cert.issuer.der, cert.subject.der);
        assert_eq!(cert.public_key, key.public);
        assert!(cert.is_self_signed());
        assert!(cert.is_certificate_authority());
        assert_eq!(
            cert.basic_constraints,
            Some(BasicConstraints {
                ca: true,
                path_len: Some(1),
                critical: true
            })
        );
        assert!(cert
            .key_usage
            .as_ref()
            .unwrap()
            .has(KeyUsageBit::KeyCertSign));
        assert!(!cert
            .key_usage
            .as_ref()
            .unwrap()
            .has(KeyUsageBit::DigitalSignature));
        assert_eq!(cert.subject_key_id, Some(cert.computed_key_id()));
        assert!(cert.unknown_critical.is_empty());
        assert!(cert.is_valid_at(moment(2030)));
        assert!(!cert.is_valid_at(moment(2019)));
        assert!(!cert.is_valid_at(moment(2041)));
        assert_eq!(
            cert.signature_algorithm,
            SignatureAlgorithm::RsaPkcs1(Hash::Sha256)
        );
        // Le certificat lu doit rendre les mêmes octets.
        assert_eq!(cert.der, der);
    }

    #[test]
    fn issued_certificate_chains_to_its_root() {
        let root_key = test_key();
        let mut rng = SeededRandom::new(b"feuille 512");
        let leaf_key = generate(512, &mut rng).unwrap();
        let root_der = issue(
            &CertificateTemplate {
                serial: vec![1],
                subject: dn("Racine"),
                issuer: dn("Racine"),
                not_before: moment(2020),
                not_after: moment(2040),
                subject_key: root_key.public.clone(),
                ca: true,
                path_len: None,
                key_usage: vec![KeyUsageBit::KeyCertSign],
                extended_key_usage: Vec::new(),
                authority_key_id: None,
            },
            &root_key,
            Hash::Sha256,
        )
        .unwrap();
        let root = Certificate::parse(&root_der).unwrap();
        let leaf_der = issue(
            &CertificateTemplate {
                serial: vec![2],
                subject: dn("Signataire"),
                issuer: dn("Racine"),
                not_before: moment(2021),
                not_after: moment(2035),
                subject_key: leaf_key.public.clone(),
                ca: false,
                path_len: None,
                key_usage: vec![
                    KeyUsageBit::DigitalSignature,
                    KeyUsageBit::ContentCommitment,
                ],
                extended_key_usage: vec![Oid(OID_EKU_DOCUMENT_SIGNING.to_vec())],
                authority_key_id: root.subject_key_id.clone(),
            },
            &root_key,
            Hash::Sha256,
        )
        .unwrap();
        let leaf = Certificate::parse(&leaf_der).unwrap();
        assert!(leaf.is_signed_by(&root));
        assert!(!leaf.is_self_signed());
        assert!(!root.is_signed_by(&leaf));
        assert!(leaf.allows_document_signing());
        assert!(leaf.extended_usage_allows_signing());
        assert!(!leaf.is_certificate_authority());
        assert_eq!(leaf.authority_key_id, root.subject_key_id);
        // Un octet modifié dans le tbsCertificate casse la vérification.
        let mut broken = leaf_der.clone();
        let pos = broken.len() / 4;
        if let Some(b) = broken.get_mut(pos) {
            *b ^= 0x01;
        }
        // Si la mutation a cassé la structure DER, la lecture échoue — tout
        // aussi bon ; sinon la vérification doit refuser.
        if let Ok(c) = Certificate::parse(&broken) {
            assert!(!c.is_signed_by(&root));
        }
        // Lecture d'une concaténation.
        let mut both = root_der.clone();
        both.extend_from_slice(&leaf_der);
        let list = parse_many(&both).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list.first().unwrap().subject.common_name(), Some("Racine"));
    }

    #[test]
    fn unknown_critical_extension_is_recorded() {
        let key = test_key();
        // On fabrique le certificat à la main pour y glisser une extension
        // critique inconnue.
        let algorithm =
            asn1::write::algorithm(&Hash::Sha256.signature_oid(), Some(&asn1::write::null()));
        let ext = extension(
            &Oid::parse("1.3.6.1.4.1.99999.1").unwrap(),
            true,
            &[0x05, 0x00],
        );
        let tbs = asn1::write::sequence(&[
            asn1::write::context(0, &asn1::write::integer_i64(2)),
            asn1::write::unsigned_integer(&[9]),
            algorithm.clone(),
            encode_name(&dn("Émetteur")).unwrap(),
            asn1::write::sequence(&[
                asn1::write::x509_time(moment(2020)),
                asn1::write::x509_time(moment(2040)),
            ]),
            encode_name(&dn("Sujet")).unwrap(),
            key.public.to_spki_der(),
            asn1::write::context(3, &asn1::write::sequence(&[ext])),
        ]);
        let sig = key
            .sign_pkcs1_v15(Hash::Sha256, &Hash::Sha256.digest(&tbs))
            .unwrap();
        let der = asn1::write::sequence(&[tbs, algorithm, asn1::write::bit_string(&sig)]);
        let cert = Certificate::parse(&der).unwrap();
        assert_eq!(cert.unknown_critical.len(), 1);
        assert_eq!(
            cert.unknown_critical.first().unwrap().to_string(),
            "1.3.6.1.4.1.99999.1"
        );
    }

    #[test]
    fn pss_parameters_are_read() {
        // AlgorithmIdentifier id-RSASSA-PSS avec SHA-256 et un sel de 32.
        let sha256 = asn1::write::algorithm(&Hash::Sha256.oid(), Some(&asn1::write::null()));
        let mgf = asn1::write::algorithm(&Oid(rsa::OID_MGF1.to_vec()), Some(&sha256));
        let params = asn1::write::sequence(&[
            asn1::write::context(0, &sha256),
            asn1::write::context(1, &mgf),
            asn1::write::context(2, &asn1::write::integer_i64(32)),
        ]);
        let alg = asn1::write::algorithm(&Oid(rsa::OID_RSASSA_PSS.to_vec()), Some(&params));
        let mut r = Reader::new(&alg);
        assert_eq!(
            SignatureAlgorithm::parse(&mut r).unwrap(),
            SignatureAlgorithm::RsaPss {
                hash: Hash::Sha256,
                mgf_hash: Hash::Sha256,
                salt_len: 32
            }
        );
        // Sans paramètres : les valeurs par défaut de la RFC 8017 (SHA-1).
        let bare = asn1::write::algorithm(&Oid(rsa::OID_RSASSA_PSS.to_vec()), None);
        let mut r = Reader::new(&bare);
        assert_eq!(
            SignatureAlgorithm::parse(&mut r).unwrap(),
            SignatureAlgorithm::RsaPss {
                hash: Hash::Sha1,
                mgf_hash: Hash::Sha1,
                salt_len: 20
            }
        );
    }

    #[test]
    fn attribute_labels_roundtrip() {
        for label in ["CN", "O", "OU", "C", "L", "ST", "E", "DC", "SN", "GN"] {
            let oid = attribute_oid(label).unwrap();
            assert_eq!(attribute_label(&oid), label);
        }
        assert!(attribute_oid("1.2.3.4").is_ok());
        assert!(attribute_oid("inconnu").is_err());
    }
}
