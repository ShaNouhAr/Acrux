//! CMS `SignedData` détaché (RFC 5652) : lecture et fabrication.
//!
//! C'est le contenu du `/Contents` d'une signature `adbe.pkcs7.detached` ou
//! `ETSI.CAdES.detached` : une enveloppe qui ne contient **pas** les données
//! signées (elles sont dans le PDF, désignées par `/ByteRange`) mais qui porte
//! les certificats, les attributs signés et la signature elle-même.
//!
//! Le point délicat, et celui qui a produit le plus de contournements dans la
//! nature : lorsque des **attributs signés** sont présents, la signature ne
//! porte pas sur le document mais sur ces attributs (RFC 5652 §5.4), et c'est
//! l'attribut `messageDigest` qui fait le lien avec le document. Vérifier la
//! signature sans vérifier ce lien ne prouve rien du tout ; nous vérifions
//! donc toujours les deux, et le `contentType` par-dessus.

use acrux_core::{Error, Result};
use acrux_document::asn1::{self, Oid, Reader, Time};
use acrux_document::crypt::rsa::{Hash, PrivateKey};

use super::x509::{Certificate, DistinguishedName, SignatureAlgorithm};

/// `1.2.840.113549.1.7.1` — `id-data`.
pub const OID_DATA: [u32; 7] = [1, 2, 840, 113_549, 1, 7, 1];
/// `1.2.840.113549.1.7.2` — `id-signedData`.
pub const OID_SIGNED_DATA: [u32; 7] = [1, 2, 840, 113_549, 1, 7, 2];
/// `1.2.840.113549.1.9.3` — attribut `contentType`.
pub const OID_ATTR_CONTENT_TYPE: [u32; 7] = [1, 2, 840, 113_549, 1, 9, 3];
/// `1.2.840.113549.1.9.4` — attribut `messageDigest`.
pub const OID_ATTR_MESSAGE_DIGEST: [u32; 7] = [1, 2, 840, 113_549, 1, 9, 4];
/// `1.2.840.113549.1.9.5` — attribut `signingTime`.
pub const OID_ATTR_SIGNING_TIME: [u32; 7] = [1, 2, 840, 113_549, 1, 9, 5];
/// `1.2.840.113549.1.9.16.2.14` — attribut non signé `id-aa-timeStampToken`.
pub const OID_ATTR_TIMESTAMP: [u32; 9] = [1, 2, 840, 113_549, 1, 9, 16, 2, 14];
/// `1.2.840.113549.1.9.16.2.47` — attribut signé `id-aa-signingCertificateV2`.
pub const OID_ATTR_SIGNING_CERTIFICATE_V2: [u32; 9] = [1, 2, 840, 113_549, 1, 9, 16, 2, 47];

/// Attribut CMS : un OID et ses valeurs, conservées en DER.
#[derive(Debug, Clone)]
pub struct Attribute {
    /// Type de l'attribut.
    pub oid: Oid,
    /// Valeurs, chacune sous forme d'élément DER complet.
    pub values: Vec<Vec<u8>>,
}

/// Un signataire d'un `SignedData`.
#[derive(Debug, Clone)]
pub struct SignerInfo {
    /// Version (1 avec `issuerAndSerialNumber`, 3 avec `subjectKeyIdentifier`).
    pub version: i64,
    /// Émetteur du certificat du signataire.
    pub issuer: Option<DistinguishedName>,
    /// Numéro de série du certificat du signataire.
    pub serial: Option<Vec<u8>>,
    /// Identifiant de clé, quand le signataire est désigné ainsi.
    pub subject_key_id: Option<Vec<u8>>,
    /// Fonction de condensation annoncée.
    pub digest_algorithm: Option<Hash>,
    /// OID de cette fonction, même quand nous ne la connaissons pas.
    pub digest_algorithm_oid: Oid,
    /// Attributs signés, dans l'ordre du fichier.
    pub signed_attributes: Vec<Attribute>,
    /// Les attributs signés re-codés en `SET OF`, forme que la signature
    /// couvre réellement (RFC 5652 §5.4).
    pub signed_attributes_der: Option<Vec<u8>>,
    /// Algorithme de la signature.
    pub signature_algorithm: SignatureAlgorithm,
    /// Valeur de la signature.
    pub signature: Vec<u8>,
    /// Attributs non signés (jeton d'horodatage, par exemple).
    pub unsigned_attributes: Vec<Attribute>,
}

impl SignerInfo {
    fn attribute(&self, oid: &[u32]) -> Option<&Attribute> {
        self.signed_attributes.iter().find(|a| a.oid.0 == oid)
    }

    /// Valeur de l'attribut signé `messageDigest`.
    #[must_use]
    pub fn message_digest(&self) -> Option<Vec<u8>> {
        let attr = self.attribute(&OID_ATTR_MESSAGE_DIGEST)?;
        let value = attr.values.first()?;
        Reader::new(value).octet_string().ok().map(<[u8]>::to_vec)
    }

    /// Valeur de l'attribut signé `contentType`.
    #[must_use]
    pub fn content_type(&self) -> Option<Oid> {
        let attr = self.attribute(&OID_ATTR_CONTENT_TYPE)?;
        Reader::new(attr.values.first()?).oid().ok()
    }

    /// Valeur de l'attribut signé `signingTime`.
    #[must_use]
    pub fn signing_time(&self) -> Option<Time> {
        let attr = self.attribute(&OID_ATTR_SIGNING_TIME)?;
        Reader::new(attr.values.first()?).time().ok()
    }

    /// Vrai si un jeton d'horodatage RFC 3161 est joint (nous ne le vérifions
    /// pas encore : voir `spec-notes/pdf-12.8-signatures.md`).
    #[must_use]
    pub fn has_timestamp(&self) -> bool {
        self.unsigned_attributes
            .iter()
            .any(|a| a.oid.0 == OID_ATTR_TIMESTAMP)
    }

    /// Vrai si l'attribut `signingCertificateV2` est présent (exigé par les
    /// profils CAdES/PAdES).
    #[must_use]
    pub fn has_signing_certificate_v2(&self) -> bool {
        self.attribute(&OID_ATTR_SIGNING_CERTIFICATE_V2).is_some()
    }
}

/// Contenu d'un `SignedData` lu.
#[derive(Debug, Clone)]
pub struct SignedData {
    /// Version de la structure.
    pub version: i64,
    /// Type du contenu encapsulé (`id-data` pour une signature PDF).
    pub content_type: Oid,
    /// Contenu encapsulé, absent dans une signature détachée.
    pub content: Option<Vec<u8>>,
    /// Certificats joints.
    pub certificates: Vec<Certificate>,
    /// Vrai si des listes de révocation sont jointes (nous ne les lisons pas).
    pub has_crls: bool,
    /// Signataires.
    pub signers: Vec<SignerInfo>,
}

impl SignedData {
    /// Lit un `ContentInfo` contenant un `SignedData`.
    ///
    /// # Errors
    /// Structure invalide, ou type de contenu autre que `id-signedData`.
    pub fn parse(der: &[u8]) -> Result<SignedData> {
        let mut root = Reader::new(der);
        let mut info = root.sequence()?;
        // Le `/Contents` d'un PDF est rempli de zéros jusqu'à la taille
        // réservée : les octets qui suivent la structure DER sont normaux.
        let oid = info.oid()?;
        if oid.0 != OID_SIGNED_DATA {
            return Err(Error::Unsupported(format!(
                "CMS {oid} : seul id-signedData est accepté dans un /Contents"
            )));
        }
        let mut wrapper = info.context(0)?;
        let mut sd = wrapper.sequence()?;
        let version = sd.integer_i64()?;
        // digestAlgorithms : ignoré ici, chaque SignerInfo porte le sien.
        sd.skip()?;
        let mut encap = sd.sequence()?;
        let content_type = encap.oid()?;
        let content = if encap.peek_is_context(0) {
            let mut c = encap.context(0)?;
            Some(c.octet_string()?.to_vec())
        } else {
            None
        };
        let mut certificates = Vec::new();
        if sd.peek_is_context(0) {
            let tlv = sd.read()?;
            let mut list = sd.sub(tlv);
            while !list.is_empty() {
                let item = list.read()?;
                // Un `CertificateSet` peut contenir d'autres choix que des
                // certificats X.509 ; on ne garde que ceux-là.
                if item.is(asn1::SEQUENCE) {
                    certificates.push(Certificate::parse(item.raw)?);
                }
            }
        }
        let has_crls = sd.peek_is_context(1);
        if has_crls {
            sd.skip()?;
        }
        let mut signer_set = sd.set()?;
        let mut signers = Vec::new();
        while !signer_set.is_empty() {
            signers.push(parse_signer(&mut signer_set)?);
        }
        Ok(SignedData {
            version,
            content_type,
            content,
            certificates,
            has_crls,
            signers,
        })
    }

    /// Certificat correspondant à un signataire, cherché par
    /// `issuerAndSerialNumber` puis par identifiant de clé.
    #[must_use]
    pub fn certificate_of<'a>(&'a self, signer: &SignerInfo) -> Option<&'a Certificate> {
        if let (Some(issuer), Some(serial)) = (&signer.issuer, &signer.serial) {
            // Comparaison binaire du nom : voir DistinguishedName.
            if let Some(c) = self
                .certificates
                .iter()
                .find(|c| c.issuer.der == issuer.der && c.serial == *serial)
            {
                return Some(c);
            }
        }
        if let Some(id) = &signer.subject_key_id {
            return self
                .certificates
                .iter()
                .find(|c| c.subject_key_id.as_ref() == Some(id) || c.computed_key_id() == *id);
        }
        None
    }
}

fn parse_signer(set: &mut Reader<'_>) -> Result<SignerInfo> {
    let mut si = set.sequence()?;
    let version = si.integer_i64()?;
    let (issuer, serial, subject_key_id) = if si.peek_is_context(0) {
        // [0] IMPLICIT SubjectKeyIdentifier (OCTET STRING retagué).
        let tlv = si.read()?;
        (None, None, Some(tlv.content.to_vec()))
    } else {
        let mut ias = si.sequence()?;
        let issuer = parse_issuer_name(&mut ias)?;
        let serial = ias.integer_bytes()?.to_vec();
        ias.end()?;
        (Some(issuer), Some(serial), None)
    };
    let digest_algorithm_oid = {
        let mut alg = si.sequence()?;
        alg.oid()?
    };
    let digest_algorithm = Hash::from_oid(&digest_algorithm_oid);
    let (signed_attributes, signed_attributes_der) = if si.peek_is_context(0) {
        let tlv = si.read()?;
        let mut attrs = si.sub(tlv);
        let mut list = Vec::new();
        while !attrs.is_empty() {
            list.push(parse_attribute(&mut attrs)?);
        }
        // Le condensé porte sur la forme `SET OF` (tag 0x31) et non sur le
        // `[0] IMPLICIT` (tag 0xA0) tel qu'il apparaît dans le fichier.
        let mut retagged = tlv.raw.to_vec();
        if let Some(first) = retagged.first_mut() {
            *first = 0x31;
        }
        (list, Some(retagged))
    } else {
        (Vec::new(), None)
    };
    let signature_algorithm = SignatureAlgorithm::parse(&mut si)?;
    let signature = si.octet_string()?.to_vec();
    let mut unsigned_attributes = Vec::new();
    if si.peek_is_context(1) {
        let tlv = si.read()?;
        let mut attrs = si.sub(tlv);
        while !attrs.is_empty() {
            unsigned_attributes.push(parse_attribute(&mut attrs)?);
        }
    }
    Ok(SignerInfo {
        version,
        issuer,
        serial,
        subject_key_id,
        digest_algorithm,
        digest_algorithm_oid,
        signed_attributes,
        signed_attributes_der,
        signature_algorithm,
        signature,
        unsigned_attributes,
    })
}

/// Lit un `Name` en ne retenant que ce dont le CMS a besoin : ses octets.
fn parse_issuer_name(reader: &mut Reader<'_>) -> Result<DistinguishedName> {
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

fn parse_attribute(reader: &mut Reader<'_>) -> Result<Attribute> {
    let mut seq = reader.sequence()?;
    let oid = seq.oid()?;
    let mut values = Vec::new();
    let mut set = seq.set()?;
    while !set.is_empty() {
        let tlv = set.read()?;
        values.push(tlv.raw.to_vec());
    }
    Ok(Attribute { oid, values })
}

// --- Fabrication -----------------------------------------------------------

/// Ce qu'il faut pour produire un `SignedData` détaché.
#[derive(Debug, Clone)]
pub struct SignParameters<'a> {
    /// Condensé des données signées (les octets désignés par `/ByteRange`).
    pub content_digest: &'a [u8],
    /// Fonction de condensation employée.
    pub hash: Hash,
    /// Certificat du signataire, puis le reste de la chaîne.
    pub certificates: &'a [Certificate],
    /// Date déclarée dans l'attribut `signingTime`.
    pub signing_time: Time,
}

/// Construit un `ContentInfo` / `SignedData` détaché signé par `key`.
///
/// Les trois attributs signés obligatoires d'une signature PDF sont posés :
/// `contentType`, `signingTime` et `messageDigest` (ISO 32000-2 §12.8.3.3).
/// Le `SET OF` des attributs est trié par l'écrivain DER, comme l'exige
/// X.690 §11.6 — c'est ce tri qui rend la structure reproductible.
///
/// # Errors
/// Aucun certificat fourni, ou signature RSA impossible.
pub fn build_detached(parameters: &SignParameters<'_>, key: &PrivateKey) -> Result<Vec<u8>> {
    let signer_cert = parameters
        .certificates
        .first()
        .ok_or_else(|| Error::Corrupt("aucun certificat de signataire".into()))?;
    let hash = parameters.hash;
    let digest_algorithm = asn1::write::algorithm(&hash.oid(), Some(&asn1::write::null()));

    let attributes = vec![
        attribute(
            &Oid(OID_ATTR_CONTENT_TYPE.to_vec()),
            &[asn1::write::oid(&Oid(OID_DATA.to_vec()))],
        ),
        attribute(
            &Oid(OID_ATTR_SIGNING_TIME.to_vec()),
            &[asn1::write::x509_time(parameters.signing_time)],
        ),
        attribute(
            &Oid(OID_ATTR_MESSAGE_DIGEST.to_vec()),
            &[asn1::write::octet_string(parameters.content_digest)],
        ),
    ];
    // Forme signée : SET OF trié.
    let signed_attributes = asn1::write::set_of(&attributes);
    let signature = key.sign_pkcs1_v15(hash, &hash.digest(&signed_attributes))?;
    // Forme écrite : le même contenu retagué en [0] IMPLICIT.
    let mut implicit = signed_attributes.clone();
    if let Some(first) = implicit.first_mut() {
        *first = 0xA0;
    }

    let signer_info = asn1::write::sequence(&[
        asn1::write::integer_i64(1),
        asn1::write::sequence(&[
            signer_cert.issuer.der.clone(),
            asn1::write::unsigned_integer(&signer_cert.serial),
        ]),
        digest_algorithm.clone(),
        implicit,
        asn1::write::algorithm(
            &Oid(acrux_document::crypt::rsa::OID_RSA_ENCRYPTION.to_vec()),
            Some(&asn1::write::null()),
        ),
        asn1::write::octet_string(&signature),
    ]);

    let certificates: Vec<Vec<u8>> = parameters
        .certificates
        .iter()
        .map(|c| c.der.clone())
        .collect();
    let mut certificate_set = Vec::new();
    for c in &certificates {
        certificate_set.extend_from_slice(c);
    }

    let signed_data = asn1::write::sequence(&[
        asn1::write::integer_i64(1),
        asn1::write::set_of(&[digest_algorithm]),
        // encapContentInfo sans eContent : c'est ce qui fait la signature « détachée ».
        asn1::write::sequence(&[asn1::write::oid(&Oid(OID_DATA.to_vec()))]),
        asn1::write::context_constructed(0, &certificate_set),
        asn1::write::set_of(&[signer_info]),
    ]);
    Ok(asn1::write::sequence(&[
        asn1::write::oid(&Oid(OID_SIGNED_DATA.to_vec())),
        asn1::write::context(0, &signed_data),
    ]))
}

fn attribute(oid: &Oid, values: &[Vec<u8>]) -> Vec<u8> {
    asn1::write::sequence(&[asn1::write::oid(oid), asn1::write::set_of(values)])
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::signature::x509::{issue, CertificateTemplate, KeyUsageBit};
    use acrux_document::crypt::rsa::{generate, SeededRandom};

    fn material() -> (PrivateKey, Certificate) {
        let mut rng = SeededRandom::new(b"cms tests 512");
        let key = generate(512, &mut rng).unwrap();
        let dn = vec![
            ("C".to_string(), "FR".to_string()),
            ("CN".to_string(), "Signataire CMS".to_string()),
        ];
        let der = issue(
            &CertificateTemplate {
                serial: vec![0x2A],
                subject: dn.clone(),
                issuer: dn,
                not_before: Time {
                    year: 2020,
                    month: 1,
                    day: 1,
                    hour: 0,
                    minute: 0,
                    second: 0,
                },
                not_after: Time {
                    year: 2040,
                    month: 1,
                    day: 1,
                    hour: 0,
                    minute: 0,
                    second: 0,
                },
                subject_key: key.public.clone(),
                ca: false,
                path_len: None,
                key_usage: vec![KeyUsageBit::DigitalSignature],
                extended_key_usage: Vec::new(),
                authority_key_id: None,
            },
            &key,
            Hash::Sha256,
        )
        .unwrap();
        (key, Certificate::parse(&der).unwrap())
    }

    #[test]
    fn detached_signed_data_roundtrip() {
        let (key, cert) = material();
        let content = b"les octets que designe /ByteRange";
        let digest = Hash::Sha256.digest(content);
        let when = Time {
            year: 2026,
            month: 3,
            day: 4,
            hour: 5,
            minute: 6,
            second: 7,
        };
        let der = build_detached(
            &SignParameters {
                content_digest: &digest,
                hash: Hash::Sha256,
                certificates: std::slice::from_ref(&cert),
                signing_time: when,
            },
            &key,
        )
        .unwrap();
        let sd = SignedData::parse(&der).unwrap();
        assert_eq!(sd.version, 1);
        assert_eq!(sd.content_type.0, OID_DATA);
        assert!(sd.content.is_none(), "signature détachée");
        assert_eq!(sd.certificates.len(), 1);
        assert_eq!(sd.signers.len(), 1);
        let signer = sd.signers.first().unwrap();
        assert_eq!(signer.version, 1);
        assert_eq!(signer.digest_algorithm, Some(Hash::Sha256));
        assert_eq!(signer.message_digest(), Some(digest.clone()));
        assert_eq!(signer.content_type().map(|o| o.0), Some(OID_DATA.to_vec()));
        assert_eq!(signer.signing_time(), Some(when));
        assert!(!signer.has_timestamp());
        assert_eq!(
            sd.certificate_of(signer).map(|c| c.serial.clone()),
            Some(vec![0x2A])
        );
        // La signature couvre bien les attributs sous leur forme SET OF.
        let attrs = signer.signed_attributes_der.as_ref().unwrap();
        assert_eq!(attrs.first(), Some(&0x31));
        let algorithm = crate::signature::verify::effective_algorithm(signer).unwrap();
        assert_eq!(
            algorithm,
            crate::signature::x509::SignatureAlgorithm::RsaPkcs1(Hash::Sha256)
        );
        assert!(algorithm.verify(&cert.public_key, attrs, &signer.signature));
        // Un octet changé dans les attributs casse tout.
        let mut broken = attrs.clone();
        if let Some(b) = broken.last_mut() {
            *b ^= 0x01;
        }
        assert!(!algorithm.verify(&cert.public_key, &broken, &signer.signature));
    }

    #[test]
    fn trailing_zero_padding_is_tolerated() {
        // Dans un PDF, le /Contents est plus grand que la structure CMS : les
        // octets de bourrage ne doivent pas gêner la lecture.
        let (key, cert) = material();
        let digest = Hash::Sha256.digest(b"x");
        let mut der = build_detached(
            &SignParameters {
                content_digest: &digest,
                hash: Hash::Sha256,
                certificates: std::slice::from_ref(&cert),
                signing_time: Time::from_unix(0),
            },
            &key,
        )
        .unwrap();
        der.extend_from_slice(&[0u8; 512]);
        let sd = SignedData::parse(&der).unwrap();
        assert_eq!(sd.signers.len(), 1);
    }

    #[test]
    fn refuses_other_content_types() {
        let bad = asn1::write::sequence(&[
            asn1::write::oid(&Oid(OID_DATA.to_vec())),
            asn1::write::context(0, &asn1::write::octet_string(b"x")),
        ]);
        assert!(SignedData::parse(&bad).is_err());
        assert!(SignedData::parse(b"pas du DER").is_err());
    }
}
