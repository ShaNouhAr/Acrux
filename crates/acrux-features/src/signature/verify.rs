//! Vérification d'une signature : cinq verdicts indépendants.
//!
//! | Verdict | Question |
//! |---|---|
//! | [`VerificationReport::digest`] | le condensé des octets couverts est-il celui que la signature annonce ? |
//! | [`VerificationReport::signature`] | la signature RSA est-elle valide pour la clé du certificat du signataire ? |
//! | [`VerificationReport::chain`] | le certificat remonte-t-il jusqu'à une racine fournie, avec des dates et des usages corrects ? |
//! | [`VerificationReport::coverage`] | la signature couvre-t-elle **tout** le fichier ? |
//! | [`VerificationReport::modifications`] | le document a-t-il changé après la signature ? |
//!
//! Les deux derniers comptent autant que les trois premiers. Une signature
//! peut être cryptographiquement parfaite et ne couvrir qu'un tiers du
//! fichier : le reste est alors du contenu que personne n'a signé, et c'est
//! ainsi que l'on fabrique un document « signé » qui dit autre chose que ce
//! qui a été signé (attaque dite *Incremental Saving* / *Shadow Attack*).
//! Acrobat affiche un bandeau vert dans plusieurs de ces cas ; nous non.

use std::collections::HashMap;

use acrux_core::Result;
use acrux_document::asn1::Time;
use acrux_document::crypt::rsa::Hash;
use acrux_document::writer::to_string;
use acrux_document::Document;

use super::cms::{SignedData, SignerInfo, OID_DATA};
use super::x509::{Certificate, SignatureAlgorithm};
use super::{SignatureInfo, SubFilter};

/// Issue d'un contrôle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Le contrôle est passé.
    Valid,
    /// Le contrôle a échoué : quelque chose ne va pas.
    Invalid,
    /// Le contrôle n'a pas pu être mené (information manquante, format non
    /// pris en charge). **Ce n'est pas un succès.**
    Unknown,
}

impl Status {
    /// Marqueur d'une ligne de rapport.
    #[must_use]
    pub fn mark(self) -> &'static str {
        match self {
            Status::Valid => "ok",
            Status::Invalid => "ÉCHEC",
            Status::Unknown => "inconnu",
        }
    }
}

/// Un verdict : une issue et son explication.
#[derive(Debug, Clone)]
pub struct Verdict {
    /// Issue du contrôle.
    pub status: Status,
    /// Explication en français, destinée à être affichée telle quelle.
    pub detail: String,
}

impl Verdict {
    /// Verdict positif.
    #[must_use]
    pub fn valid(detail: impl Into<String>) -> Verdict {
        Verdict {
            status: Status::Valid,
            detail: detail.into(),
        }
    }

    /// Verdict négatif.
    #[must_use]
    pub fn invalid(detail: impl Into<String>) -> Verdict {
        Verdict {
            status: Status::Invalid,
            detail: detail.into(),
        }
    }

    /// Verdict indéterminé.
    #[must_use]
    pub fn unknown(detail: impl Into<String>) -> Verdict {
        Verdict {
            status: Status::Unknown,
            detail: detail.into(),
        }
    }
}

/// Rapport complet sur une signature.
#[derive(Debug, Clone)]
pub struct VerificationReport {
    /// Nom du champ.
    pub field_name: String,
    /// Sous-filtre rencontré.
    pub sub_filter: SubFilter,
    /// Résumé du certificat du signataire.
    pub signer: Option<String>,
    /// Nom commun du signataire, extrait du certificat (et non du `/Name`).
    pub signer_common_name: Option<String>,
    /// Date signée par l'attribut `signingTime` du CMS.
    pub signing_time: Option<Time>,
    /// (a) le condensé des octets couverts correspond-il au `messageDigest` ?
    pub digest: Verdict,
    /// (b) la signature RSA est-elle valide ?
    pub signature: Verdict,
    /// (c) la chaîne de certificats se valide-t-elle ?
    pub chain: Verdict,
    /// Chaîne retenue, du signataire à la racine.
    pub chain_path: Vec<String>,
    /// (d) la signature couvre-t-elle tout le document ?
    pub coverage: Verdict,
    /// (e) le document a-t-il été modifié après la signature ?
    pub modifications: Verdict,
    /// Numéros d'objets ajoutés ou remplacés après la signature.
    pub changed_objects: Vec<u32>,
    /// Remarques qui ne sont pas des verdicts (horodatage absent, etc.).
    pub notes: Vec<String>,
}

impl VerificationReport {
    /// Vrai si les cinq verdicts sont positifs. Un seul `Unknown` suffit à
    /// répondre non : l'ignorance n'est pas une validation.
    #[must_use]
    pub fn is_fully_valid(&self) -> bool {
        [
            &self.digest,
            &self.signature,
            &self.chain,
            &self.coverage,
            &self.modifications,
        ]
        .into_iter()
        .all(|v| v.status == Status::Valid)
    }

    /// Résumé d'une ligne.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.is_fully_valid() {
            format!("{} : signature valide", self.field_name)
        } else {
            let failed = [
                ("condensé", &self.digest),
                ("signature", &self.signature),
                ("chaîne", &self.chain),
                ("couverture", &self.coverage),
                ("modifications", &self.modifications),
            ]
            .into_iter()
            .filter(|(_, v)| v.status != Status::Valid)
            .map(|(n, v)| format!("{n} ({})", v.status.mark()))
            .collect::<Vec<_>>()
            .join(", ");
            format!("{} : à problème — {failed}", self.field_name)
        }
    }
}

/// Vérifie une signature.
///
/// `roots` est l'ensemble des certificats racines de confiance. Il peut être
/// vide : la chaîne rendra alors un verdict `Unknown`, jamais `Valid`. Nous
/// n'embarquons **aucun** magasin de confiance : la liste des autorités
/// auxquelles se fier est une décision de l'utilisateur, pas du logiciel.
///
/// # Errors
/// Document illisible pendant la comparaison des révisions.
#[allow(clippy::too_many_lines)] // un verdict après l'autre, dans l'ordre du rapport
pub fn verify_signature(
    doc: &Document,
    info: &SignatureInfo,
    roots: &[Certificate],
) -> Result<VerificationReport> {
    let data = doc.bytes();
    let mut report = VerificationReport {
        field_name: info.field_name.clone(),
        sub_filter: info.sub_filter.clone(),
        signer: None,
        signer_common_name: None,
        signing_time: None,
        digest: Verdict::unknown("non évalué"),
        signature: Verdict::unknown("non évalué"),
        chain: Verdict::unknown("non évaluée"),
        chain_path: Vec::new(),
        coverage: Verdict::unknown("non évaluée"),
        modifications: Verdict::unknown("non évaluées"),
        changed_objects: Vec::new(),
        notes: Vec::new(),
    };
    if info.unsigned {
        let detail = "champ de signature vide : aucune signature à vérifier";
        report.digest = Verdict::unknown(detail);
        report.signature = Verdict::unknown(detail);
        report.chain = Verdict::unknown(detail);
        report.coverage = Verdict::unknown(detail);
        report.modifications = Verdict::unknown(detail);
        return Ok(report);
    }

    check_coverage(data, info, &mut report);
    check_modifications(doc, info, &mut report);

    match &info.sub_filter {
        f if f.is_cms() => verify_cms(data, info, roots, &mut report),
        SubFilter::AdbeX509RsaSha1 => verify_x509_rsa_sha1(data, info, roots, &mut report),
        SubFilter::EtsiRfc3161 => {
            let detail = "horodatage de document (ETSI.RFC3161) : non pris en charge";
            report.digest = Verdict::unknown(detail);
            report.signature = Verdict::unknown(detail);
            report.chain = Verdict::unknown(detail);
        }
        other => {
            let detail = format!("sous-filtre « {} » inconnu", other.name());
            report.digest = Verdict::unknown(detail.clone());
            report.signature = Verdict::unknown(detail.clone());
            report.chain = Verdict::unknown(detail);
        }
    }
    Ok(report)
}

/// (d) Couverture : la première plage part-elle de zéro, l'intervalle laissé
/// libre contient-il exactement le `/Contents`, et la dernière plage va-t-elle
/// jusqu'au bout du fichier ?
fn check_coverage(data: &[u8], info: &SignatureInfo, report: &mut VerificationReport) {
    if info.byte_range.len() != 2 {
        report.coverage = Verdict::invalid(format!(
            "/ByteRange devrait décrire deux plages, il en décrit {}",
            info.byte_range.len()
        ));
        return;
    }
    let mut problems = Vec::new();
    if info.byte_range.first().map(|r| r.0) != Some(0) {
        problems.push("la première plage ne commence pas à l'octet 0".to_string());
    }
    let end = info.covered_end();
    if end < data.len() {
        problems.push(format!(
            "{} octet(s) du fichier ne sont pas couverts (le fichier fait {} octets, \
             la signature s'arrête à {end})",
            data.len() - end,
            data.len()
        ));
    } else if end > data.len() {
        problems.push("/ByteRange dépasse la fin du fichier".to_string());
    }
    // L'intervalle non couvert doit être la chaîne /Contents et rien d'autre.
    match info.gap() {
        None => problems.push("/ByteRange ne laisse pas d'intervalle exploitable".to_string()),
        Some((start, stop)) => match data.get(start..stop) {
            None => problems.push("l'intervalle non couvert sort du fichier".to_string()),
            Some(gap) => {
                let trimmed: Vec<u8> = gap
                    .iter()
                    .copied()
                    .filter(|b| !b.is_ascii_whitespace())
                    .collect();
                if trimmed.first() != Some(&b'<') || trimmed.last() != Some(&b'>') {
                    problems.push(
                        "l'intervalle non couvert n'est pas une simple chaîne hexadécimale : \
                         il pourrait cacher n'importe quoi"
                            .to_string(),
                    );
                } else {
                    let hex = trimmed.get(1..trimmed.len() - 1).unwrap_or(&[]);
                    if hex.iter().any(|b| !b.is_ascii_hexdigit()) {
                        problems.push(
                            "l'intervalle non couvert contient autre chose que de l'hexadécimal"
                                .to_string(),
                        );
                    } else if hex.len() / 2 != info.contents.len() {
                        problems.push(format!(
                            "l'intervalle non couvert ({} octets utiles) ne correspond pas au \
                             /Contents lu ({} octets)",
                            hex.len() / 2,
                            info.contents.len()
                        ));
                    }
                }
            }
        },
    }
    report.coverage = if problems.is_empty() {
        Verdict::valid("la signature couvre tout le fichier, à l'exception de sa propre valeur")
    } else {
        Verdict::invalid(problems.join(" ; "))
    };
}

/// (e) Modifications : on relit le document tronqué à la fin de la zone
/// couverte et on compare objet par objet avec le document complet.
fn check_modifications(doc: &Document, info: &SignatureInfo, report: &mut VerificationReport) {
    let data = doc.bytes();
    let end = info.covered_end();
    if end == 0 || end > data.len() {
        report.modifications = Verdict::unknown("/ByteRange inexploitable");
        return;
    }
    if end == data.len() {
        report.modifications =
            Verdict::valid("aucun octet n'a été ajouté après la révision signée");
        return;
    }
    let signed_bytes = data.get(..end).unwrap_or(&[]).to_vec();
    let Ok(signed_doc) = Document::from_bytes(signed_bytes) else {
        report.modifications = Verdict::invalid(format!(
            "{} octet(s) ont été ajoutés après la signature, et la révision signée \
             n'est plus relisible seule",
            data.len() - end
        ));
        return;
    };
    let mut changed = Vec::new();
    let mut before: HashMap<u32, String> = HashMap::new();
    for number in signed_doc.object_numbers() {
        if let Ok(object) = signed_doc.get(acrux_document::ObjectRef {
            number,
            generation: 0,
        }) {
            before.insert(number, to_string(&object));
        }
    }
    for number in doc.object_numbers() {
        let Ok(object) = doc.get(acrux_document::ObjectRef {
            number,
            generation: 0,
        }) else {
            continue;
        };
        let after = to_string(&object);
        match before.get(&number) {
            Some(previous) if *previous == after => {}
            _ => changed.push(number),
        }
    }
    changed.sort_unstable();
    report.changed_objects.clone_from(&changed);
    let count = data.len() - end;
    report.modifications = if changed.is_empty() {
        Verdict::invalid(format!(
            "{count} octet(s) ajoutés après la signature ; aucun objet ne semble avoir \
             changé, mais ces octets ne sont pas couverts"
        ))
    } else {
        Verdict::invalid(format!(
            "le document a été modifié après la signature : {count} octet(s) ajoutés, \
             {} objet(s) ajoutés ou remplacés ({})",
            changed.len(),
            changed
                .iter()
                .take(12)
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    };
}

/// (a), (b) et (c) pour les sous-filtres à enveloppe CMS.
#[allow(clippy::too_many_lines)] // les trois verdicts cryptographiques, à la file
fn verify_cms(
    data: &[u8],
    info: &SignatureInfo,
    roots: &[Certificate],
    report: &mut VerificationReport,
) {
    let signed_data = match SignedData::parse(&info.contents) {
        Ok(s) => s,
        Err(e) => {
            let detail = format!("enveloppe CMS illisible : {e}");
            report.digest = Verdict::unknown(detail.clone());
            report.signature = Verdict::invalid(detail.clone());
            report.chain = Verdict::unknown(detail);
            return;
        }
    };
    let Some(signer) = signed_data.signers.first() else {
        let detail = "l'enveloppe CMS ne contient aucun signataire";
        report.digest = Verdict::unknown(detail);
        report.signature = Verdict::invalid(detail);
        report.chain = Verdict::unknown(detail);
        return;
    };
    if signed_data.signers.len() > 1 {
        report.notes.push(format!(
            "{} signataires dans l'enveloppe : seul le premier est vérifié",
            signed_data.signers.len()
        ));
    }
    report.signing_time = signer.signing_time();
    if signer.has_timestamp() {
        report
            .notes
            .push("un jeton d'horodatage RFC 3161 est joint mais n'est pas vérifié".into());
    } else if info.sub_filter == SubFilter::EtsiCadesDetached {
        report
            .notes
            .push("profil PAdES sans jeton d'horodatage : la date n'est qu'une déclaration".into());
    }
    if info.sub_filter == SubFilter::EtsiCadesDetached && !signer.has_signing_certificate_v2() {
        report.notes.push(
            "profil PAdES sans attribut signingCertificateV2 : le lien entre la signature \
             et le certificat n'est pas scellé"
                .into(),
        );
    }

    let Some(hash) = signer.digest_algorithm else {
        let detail = format!(
            "fonction de condensation inconnue ({})",
            signer.digest_algorithm_oid
        );
        report.digest = Verdict::unknown(detail.clone());
        report.signature = Verdict::unknown(detail);
        return;
    };
    if hash == Hash::Sha1 {
        report.notes.push(
            "condensé SHA-1 : algorithme obsolète, la signature ne prouve plus grand-chose".into(),
        );
    }

    // (a) le condensé des octets couverts.
    let covered = match info.covered_bytes(data) {
        Ok(c) => c,
        Err(e) => {
            report.digest = Verdict::invalid(format!("octets couverts illisibles : {e}"));
            return;
        }
    };
    let computed = if info.sub_filter == SubFilter::AdbePkcs7Sha1 {
        // adbe.pkcs7.sha1 : le contenu encapsulé est le condensé SHA-1 des
        // octets couverts, et c'est lui que `messageDigest` couvre.
        Hash::Sha1.digest(&covered)
    } else {
        hash.digest(&covered)
    };
    match signer.message_digest() {
        None if signer.signed_attributes_der.is_none() => {
            report.digest = Verdict::valid(
                "signature sans attributs signés : le condensé est vérifié avec la signature",
            );
        }
        None => {
            report.digest = Verdict::invalid("attributs signés présents mais sans messageDigest");
        }
        Some(declared) => {
            if info.sub_filter == SubFilter::AdbePkcs7Sha1 {
                // Le messageDigest porte sur le contenu encapsulé.
                let encapsulated = signed_data.content.clone().unwrap_or_default();
                let matches_content = hash.digest(&encapsulated) == declared;
                let matches_file = encapsulated == computed;
                report.digest = if matches_content && matches_file {
                    Verdict::valid(format!(
                        "condensé {} du contenu encapsulé conforme, et ce contenu est bien \
                         le SHA-1 des octets couverts",
                        hash.label()
                    ))
                } else {
                    Verdict::invalid("le condensé encapsulé ne correspond pas aux octets couverts")
                };
            } else if declared == computed {
                report.digest = Verdict::valid(format!(
                    "condensé {} des {} octets couverts conforme au messageDigest signé",
                    hash.label(),
                    covered.len()
                ));
            } else {
                report.digest = Verdict::invalid(
                    "le condensé des octets couverts ne correspond pas au messageDigest signé : \
                     le contenu signé a changé",
                );
            }
            // contentType doit valoir id-data, sinon les attributs signés
            // pourraient avoir été empruntés à un autre message.
            match signer.content_type() {
                Some(ct) if ct.0 == OID_DATA => {}
                Some(ct) => report.notes.push(format!(
                    "attribut contentType inattendu ({ct}) : id-data était attendu"
                )),
                None => report
                    .notes
                    .push("attribut signé contentType absent (exigé par la RFC 5652)".into()),
            }
        }
    }

    // (b) la signature elle-même.
    let certificate = signed_data.certificate_of(signer).cloned();
    match &certificate {
        None => {
            report.signature =
                Verdict::unknown("le certificat du signataire n'est pas joint à l'enveloppe");
        }
        Some(cert) => {
            report.signer = Some(cert.summary());
            report.signer_common_name = cert.subject.common_name().map(str::to_string);
            report.signature = check_signature(signer, cert, &covered);
        }
    }

    // (c) la chaîne.
    match &certificate {
        None => report.chain = Verdict::unknown("aucun certificat de signataire"),
        Some(cert) => {
            let at = report.signing_time.unwrap_or_else(Time::now);
            let (verdict, path) = validate_chain(cert, &signed_data.certificates, roots, at);
            report.chain = verdict;
            report.chain_path = path;
        }
    }
}

/// Algorithme réellement employé par un `SignerInfo`.
///
/// Le CMS écrit presque toujours `rsaEncryption` comme `signatureAlgorithm`
/// (RFC 5652 §5.3) : la fonction de condensation est alors celle du
/// `digestAlgorithm` du même `SignerInfo`, et le schéma est PKCS#1 v1.5.
#[must_use]
pub fn effective_algorithm(signer: &SignerInfo) -> Option<SignatureAlgorithm> {
    match &signer.signature_algorithm {
        SignatureAlgorithm::Unsupported(oid)
            if oid.0 == acrux_document::crypt::rsa::OID_RSA_ENCRYPTION =>
        {
            signer.digest_algorithm.map(SignatureAlgorithm::RsaPkcs1)
        }
        other => Some(other.clone()),
    }
}

/// Vérifie la signature d'un `SignerInfo` : sur les attributs signés s'il y en
/// a, sur le contenu sinon.
fn check_signature(signer: &SignerInfo, cert: &Certificate, covered: &[u8]) -> Verdict {
    let message = match &signer.signed_attributes_der {
        Some(attributes) => attributes.clone(),
        None => covered.to_vec(),
    };
    let Some(algorithm) = effective_algorithm(signer) else {
        return Verdict::unknown("algorithme de signature indéterminé");
    };
    if let SignatureAlgorithm::Unsupported(oid) = &algorithm {
        return Verdict::unknown(format!(
            "algorithme de signature non pris en charge ({oid})"
        ));
    }
    if algorithm.verify(&cert.public_key, &message, &signer.signature) {
        Verdict::valid(format!(
            "signature {} valide pour la clé du certificat ({} bits)",
            algorithm.label(),
            cert.public_key.bits()
        ))
    } else {
        Verdict::invalid(format!(
            "la signature {} ne correspond pas à la clé publique du certificat",
            algorithm.label()
        ))
    }
}

/// `adbe.x509.rsa_sha1` (§12.8.3.2) : `/Contents` est la signature RSA brute
/// du SHA-1 des octets couverts, le certificat est dans `/Cert`.
fn verify_x509_rsa_sha1(
    data: &[u8],
    info: &SignatureInfo,
    roots: &[Certificate],
    report: &mut VerificationReport,
) {
    report.notes.push(
        "sous-filtre adbe.x509.rsa_sha1 : format hérité, fondé sur SHA-1, interdit en PDF 2.0"
            .into(),
    );
    let certificates: Vec<Certificate> = info
        .embedded_certificates
        .iter()
        .filter_map(|der| Certificate::parse(der).ok())
        .collect();
    let Some(cert) = certificates.first() else {
        let detail = "aucun certificat lisible dans /Cert";
        report.signature = Verdict::invalid(detail);
        report.chain = Verdict::unknown(detail);
        report.digest = Verdict::unknown(detail);
        return;
    };
    report.signer = Some(cert.summary());
    report.signer_common_name = cert.subject.common_name().map(str::to_string);
    let covered = match info.covered_bytes(data) {
        Ok(c) => c,
        Err(e) => {
            report.digest = Verdict::invalid(format!("octets couverts illisibles : {e}"));
            return;
        }
    };
    // Le /Contents est un OCTET STRING DER enveloppant la signature, ou la
    // signature brute selon les producteurs : on accepte les deux.
    let signature = acrux_document::asn1::Reader::new(&info.contents)
        .octet_string()
        .map_or_else(|_| info.contents.clone(), <[u8]>::to_vec);
    let digest = Hash::Sha1.digest(&covered);
    if cert
        .public_key
        .verify_pkcs1_v15(Hash::Sha1, &digest, &signature)
    {
        report.digest = Verdict::valid(format!(
            "SHA-1 des {} octets couverts conforme",
            covered.len()
        ));
        report.signature = Verdict::valid("signature RSA / SHA-1 valide pour le certificat /Cert");
    } else {
        report.digest =
            Verdict::invalid("la signature ne correspond pas au SHA-1 des octets couverts");
        report.signature = Verdict::invalid("signature RSA / SHA-1 invalide");
    }
    let (verdict, path) = validate_chain(cert, &certificates, roots, Time::now());
    report.chain = verdict;
    report.chain_path = path;
}

/// Construit et valide la chaîne du signataire jusqu'à une racine de confiance.
///
/// Règles appliquées (RFC 5280 §6.1, simplifiées) : dates de validité,
/// `basicConstraints` avec `pathLenConstraint`, `keyUsage` (`keyCertSign` pour
/// les autorités, `digitalSignature` ou `contentCommitment` pour la feuille),
/// `extKeyUsage`, et refus de toute extension critique inconnue.
#[must_use]
#[allow(clippy::too_many_lines)] // les règles de la RFC 5280 §6.1, une par une
pub fn validate_chain(
    leaf: &Certificate,
    intermediates: &[Certificate],
    roots: &[Certificate],
    at: Time,
) -> (Verdict, Vec<String>) {
    let mut path = vec![leaf.summary()];
    let mut problems = Vec::new();
    if !leaf.is_valid_at(at) {
        problems.push(format!(
            "le certificat du signataire n'était pas valide le {} (validité du {} au {})",
            at.to_display(),
            leaf.not_before.to_display(),
            leaf.not_after.to_display()
        ));
    }
    if !leaf.allows_document_signing() {
        problems.push(format!(
            "keyUsage du signataire sans digitalSignature ni contentCommitment ({})",
            leaf.key_usage
                .as_ref()
                .map(|u| u.labels().join(", "))
                .unwrap_or_default()
        ));
    }
    if !leaf.extended_usage_allows_signing() {
        problems.push("extKeyUsage du signataire n'autorise pas la signature de documents".into());
    }
    for oid in &leaf.unknown_critical {
        problems.push(format!(
            "extension critique inconnue dans le signataire ({oid})"
        ));
    }

    let mut current = leaf.clone();
    let mut depth = 0usize;
    let mut trusted = false;
    loop {
        if roots
            .iter()
            .any(|r| r.subject.der == current.subject.der && r.public_key == current.public_key)
        {
            trusted = true;
            break;
        }
        if depth >= 10 {
            problems.push("chaîne de certificats trop longue (plus de dix maillons)".into());
            break;
        }
        // On cherche d'abord dans les racines : une racine de confiance ferme
        // la chaîne même si un intermédiaire du même nom est joint au fichier.
        let issuer = roots
            .iter()
            .chain(intermediates.iter())
            .find(|c| current.is_signed_by(c))
            .cloned();
        let Some(issuer) = issuer else {
            if current.is_self_signed() {
                problems.push(format!(
                    "la chaîne se termine sur le certificat auto-signé « {} », qui ne figure \
                     pas parmi les racines de confiance fournies",
                    current.subject
                ));
            } else {
                problems.push(format!(
                    "aucun certificat fourni ne signe « {} » (émetteur déclaré : {})",
                    current.subject, current.issuer
                ));
            }
            break;
        };
        if issuer.subject.der == current.subject.der && issuer.public_key == current.public_key {
            // Auto-signé : il ne peut clore la chaîne que s'il est de confiance.
            problems.push(format!(
                "le certificat auto-signé « {} » n'est pas une racine de confiance",
                issuer.subject
            ));
            break;
        }
        path.push(issuer.summary());
        if !issuer.is_valid_at(at) {
            problems.push(format!(
                "l'autorité « {} » n'était pas valide le {}",
                issuer.subject,
                at.to_display()
            ));
        }
        if !issuer.is_certificate_authority() {
            problems.push(format!(
                "« {} » signe un certificat sans être une autorité (basicConstraints / keyUsage)",
                issuer.subject
            ));
        }
        if let Some(max) = issuer.basic_constraints.and_then(|b| b.path_len) {
            if i64::try_from(depth).unwrap_or(i64::MAX) > max {
                problems.push(format!(
                    "pathLenConstraint de « {} » dépassé",
                    issuer.subject
                ));
            }
        }
        for oid in &issuer.unknown_critical {
            problems.push(format!(
                "extension critique inconnue dans « {} » ({oid})",
                issuer.subject
            ));
        }
        current = issuer;
        depth += 1;
    }

    let verdict = if !problems.is_empty() {
        if trusted {
            Verdict::invalid(problems.join(" ; "))
        } else {
            Verdict::unknown(problems.join(" ; "))
        }
    } else if trusted {
        Verdict::valid(format!(
            "chaîne de {} certificat(s) valide jusqu'à une racine de confiance",
            path.len()
        ))
    } else {
        Verdict::unknown("aucune racine de confiance n'a été fournie")
    };
    (verdict, path)
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn status_marks() {
        assert_eq!(Status::Valid.mark(), "ok");
        assert_eq!(Status::Invalid.mark(), "ÉCHEC");
        assert_eq!(Status::Unknown.mark(), "inconnu");
    }

    #[test]
    fn a_report_with_an_unknown_is_not_valid() {
        let report = VerificationReport {
            field_name: "S".into(),
            sub_filter: SubFilter::AdbePkcs7Detached,
            signer: None,
            signer_common_name: None,
            signing_time: None,
            digest: Verdict::valid("x"),
            signature: Verdict::valid("x"),
            chain: Verdict::unknown("aucune racine"),
            chain_path: Vec::new(),
            coverage: Verdict::valid("x"),
            modifications: Verdict::valid("x"),
            changed_objects: Vec::new(),
            notes: Vec::new(),
        };
        assert!(!report.is_fully_valid());
        assert!(report.summary().contains("chaîne"));
        let ok = VerificationReport {
            chain: Verdict::valid("x"),
            ..report
        };
        assert!(ok.is_fully_valid());
        assert!(ok.summary().contains("valide"));
    }
}
