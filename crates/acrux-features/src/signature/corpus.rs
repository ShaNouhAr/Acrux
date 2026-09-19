//! Fabrication du matériel de test des signatures et aller-retour complet.
//!
//! Tout est engendré par nos propres implémentations : la clé RSA, le
//! certificat racine, le certificat du signataire, le PDF à signer, la
//! signature elle-même. Rien n'est recopié d'un outil tiers.
//!
//! Le générateur est `#[ignore]` : il écrit dans `tests/corpus/synthese/` et
//! ne se lance qu'à la demande —
//! `cargo test -p acrux-features --lib -- --ignored generate_signature_corpus`.
//! Les autres tests de ce module relisent ce matériel et n'écrivent rien.
//!
//! **Ce matériel n'a aucune valeur de sécurité** : les clés privées sont dans
//! le dépôt et engendrées par un générateur déterministe. Voir le README du
//! dossier.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use acrux_core::Rect;
use acrux_document::asn1::{Oid, Time};
use acrux_document::crypt::rsa::{generate, Hash, PrivateKey, SeededRandom};
use acrux_document::Document;

use super::sign::{sign, Appearance, SignOptions, SigningKey};
use super::verify::{validate_chain, verify_signature, Status};
use super::x509::{issue, Certificate, CertificateTemplate, KeyUsageBit, OID_EKU_DOCUMENT_SIGNING};
use super::{list_signatures, SubFilter};

/// Nom des fichiers versés au corpus.
const ROOT_KEY: &str = "signature-ac-test.key.der";
const ROOT_CERT: &str = "signature-ac-test.cert.der";
const SIGNER_KEY: &str = "signature-signataire-test.key.der";
const SIGNER_CERT: &str = "signature-signataire-test.cert.der";
const UNSIGNED_PDF: &str = "signature-attestation.pdf";
const SIGNED_PDF: &str = "signature-attestation-signee.pdf";

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join("synthese")
}

fn read(name: &str) -> Vec<u8> {
    let path = corpus_dir().join(name);
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{} absent ({e}) : lancer `cargo test -p acrux-features --lib -- --ignored \
             generate_signature_corpus`",
            path.display()
        )
    })
}

fn moment(year: i32, month: u32, day: u32) -> Time {
    Time {
        year,
        month,
        day,
        hour: 12,
        minute: 0,
        second: 0,
    }
}

/// Autorité racine de test : clé et certificat auto-signé.
fn build_root() -> (PrivateKey, Vec<u8>) {
    let mut rng = SeededRandom::new(b"Acrux / autorite de test / 2026");
    let key = generate(2048, &mut rng).expect("clé de l'autorité");
    let dn = vec![
        ("C".to_string(), "FR".to_string()),
        ("O".to_string(), "Acrux".to_string()),
        (
            "CN".to_string(),
            "Acrux — autorité de test (sans valeur)".to_string(),
        ),
    ];
    let der = issue(
        &CertificateTemplate {
            serial: vec![0x01, 0x00, 0x01],
            subject: dn.clone(),
            issuer: dn,
            not_before: moment(2024, 1, 1),
            not_after: moment(2044, 1, 1),
            subject_key: key.public.clone(),
            ca: true,
            path_len: Some(0),
            key_usage: vec![KeyUsageBit::KeyCertSign, KeyUsageBit::CrlSign],
            extended_key_usage: Vec::new(),
            authority_key_id: None,
        },
        &key,
        Hash::Sha256,
    )
    .expect("certificat racine");
    (key, der)
}

/// Certificat de signature, émis par la racine.
fn build_signer(root_key: &PrivateKey, root: &Certificate) -> (PrivateKey, Vec<u8>) {
    let mut rng = SeededRandom::new(b"Acrux / signataire de test / 2026");
    let key = generate(2048, &mut rng).expect("clé du signataire");
    let der = issue(
        &CertificateTemplate {
            serial: vec![0x02, 0x00, 0x02],
            subject: vec![
                ("C".to_string(), "FR".to_string()),
                ("O".to_string(), "Acrux".to_string()),
                ("CN".to_string(), "Camille Test".to_string()),
                ("E".to_string(), "camille@example.invalid".to_string()),
            ],
            issuer: root
                .subject
                .attributes
                .iter()
                .map(|(o, v)| (label_of(o), v.clone()))
                .collect(),
            not_before: moment(2024, 2, 1),
            not_after: moment(2034, 2, 1),
            subject_key: key.public.clone(),
            ca: false,
            path_len: None,
            key_usage: vec![
                KeyUsageBit::DigitalSignature,
                KeyUsageBit::ContentCommitment,
            ],
            extended_key_usage: vec![Oid(OID_EKU_DOCUMENT_SIGNING.to_vec())],
            authority_key_id: root.subject_key_id.clone(),
        },
        root_key,
        Hash::Sha256,
    )
    .expect("certificat du signataire");
    (key, der)
}

/// Abréviation d'un attribut de nom, pour recopier le sujet de la racine.
fn label_of(oid: &Oid) -> String {
    match oid.0.as_slice() {
        [2, 5, 4, 3] => "CN".into(),
        [2, 5, 4, 6] => "C".into(),
        [2, 5, 4, 10] => "O".into(),
        [2, 5, 4, 11] => "OU".into(),
        other => Oid(other.to_vec()).to_string(),
    }
}

/// PDF d'une page écrit à la main, non compressé, lisible dans un éditeur.
fn attestation_pdf() -> Vec<u8> {
    let content = b"BT /F1 16 Tf 40 250 Td (Attestation de test) Tj ET\n\
BT /F1 10 Tf 40 222 Td (Ce document sert a eprouver la pose et la verification) Tj ET\n\
BT /F1 10 Tf 40 208 Td (d'une signature numerique par Acrux.) Tj ET\n\
0.2 0.2 0.5 RG 1 w 40 200 m 380 200 l S\n\
BT /F1 9 Tf 40 176 Td (Article 1 : les octets de ce fichier ne doivent pas bouger.) Tj ET\n\
BT /F1 9 Tf 40 162 Td (Article 2 : la signature est ajoutee par mise a jour incrementale.) Tj ET\n\
BT /F1 9 Tf 40 148 Td (Article 3 : un seul octet modifie doit invalider le condense.) Tj ET\n\
0.5 0.5 0.5 RG 0.5 w 40 60 m 380 60 l S\n\
BT /F1 8 Tf 40 46 Td (Corpus de synthese Acrux - materiel de test sans valeur) Tj ET"
        .to_vec();
    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 420 300] \
/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
            .to_vec(),
        {
            let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
            stream.extend_from_slice(&content);
            stream.extend_from_slice(b"\nendstream");
            stream
        },
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    ];
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /ID [<{}> <{}>] >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len() + 1,
            "41434B5349474E415455524553544553543031",
            "41434B5349474E415455524553544553543031"
        )
        .as_bytes(),
    );
    out
}

/// Écrit dans `tests/corpus/synthese/` l'autorité de test, le certificat du
/// signataire, le document à signer et sa version signée.
#[test]
#[ignore = "écrit le matériel de test dans tests/corpus/synthese/"]
fn generate_signature_corpus() {
    let directory = corpus_dir();
    std::fs::create_dir_all(&directory).expect("dossier de corpus");
    let (root_key, root_der) = build_root();
    let root = Certificate::parse(&root_der).expect("racine relisible");
    let (signer_key, signer_der) = build_signer(&root_key, &root);

    std::fs::write(directory.join(ROOT_KEY), root_key.to_pkcs8_der()).unwrap();
    std::fs::write(directory.join(ROOT_CERT), &root_der).unwrap();
    std::fs::write(directory.join(SIGNER_KEY), signer_key.to_pkcs8_der()).unwrap();
    std::fs::write(directory.join(SIGNER_CERT), &signer_der).unwrap();

    let unsigned = attestation_pdf();
    std::fs::write(directory.join(UNSIGNED_PDF), &unsigned).unwrap();

    let signed = sign_attestation(
        &unsigned,
        &signer_key.to_pkcs8_der(),
        &signer_der,
        &root_der,
    );
    std::fs::write(directory.join(SIGNED_PDF), &signed).unwrap();

    // Contrôle immédiat : ce que l'on vient d'écrire doit se vérifier.
    let doc = Document::from_bytes(signed).expect("document signé relisible");
    let signatures = list_signatures(&doc).expect("inventaire");
    assert_eq!(signatures.len(), 1);
    let report = verify_signature(&doc, signatures.first().unwrap(), &[root]).expect("rapport");
    assert!(
        report.is_fully_valid(),
        "la signature engendrée doit être valide : {report:#?}"
    );
}

/// Signe l'attestation avec une apparence visible sur la page 1.
fn sign_attestation(
    pdf: &[u8],
    key_der: &[u8],
    certificate_der: &[u8],
    root_der: &[u8],
) -> Vec<u8> {
    let doc = Document::from_bytes(pdf.to_vec()).expect("document à signer");
    let key = SigningKey::from_der(key_der, certificate_der)
        .expect("clé de signature")
        .with_chain(&[root_der.to_vec()])
        .expect("chaîne");
    sign(
        &doc,
        &key,
        &SignOptions {
            reason: Some("Approbation du document de test".into()),
            location: Some("Paris".into()),
            contact_info: Some("camille@example.invalid".into()),
            appearance: Some(Appearance {
                page: 0,
                rect: Rect::new(230.0, 70.0, 390.0, 130.0),
            }),
            signing_time: Some(moment(2026, 5, 20)),
            ..SignOptions::default()
        },
    )
    .expect("signature")
}

/// Le matériel du corpus se relit et la chaîne se valide.
#[test]
fn corpus_certificates_chain_up() {
    let root = Certificate::parse(&read(ROOT_CERT)).unwrap();
    let signer = Certificate::parse(&read(SIGNER_CERT)).unwrap();
    assert!(root.is_self_signed());
    assert!(root.is_certificate_authority());
    assert!(signer.is_signed_by(&root));
    assert!(signer.allows_document_signing());
    assert_eq!(signer.public_key.bits(), 2048);
    let key = PrivateKey::from_pkcs8_der(&read(SIGNER_KEY)).unwrap();
    assert_eq!(key.public, signer.public_key);
    let (verdict, path) = validate_chain(&signer, &[], &[root], moment(2026, 5, 20));
    assert_eq!(verdict.status, Status::Valid, "{}", verdict.detail);
    assert_eq!(path.len(), 2);
}

/// Aller-retour complet : le PDF signé du corpus se vérifie sur ses cinq
/// verdicts.
#[test]
fn signed_corpus_file_verifies() {
    let root = Certificate::parse(&read(ROOT_CERT)).unwrap();
    let doc = Document::from_bytes(read(SIGNED_PDF)).unwrap();
    let signatures = list_signatures(&doc).unwrap();
    assert_eq!(signatures.len(), 1);
    let info = signatures.first().unwrap();
    assert!(!info.unsigned);
    assert_eq!(info.sub_filter, SubFilter::AdbePkcs7Detached);
    assert_eq!(info.filter, "Adobe.PPKLite");
    assert_eq!(info.field_name, "Signature1");
    assert_eq!(info.page, Some(0));
    assert_eq!(
        info.reason.as_deref(),
        Some("Approbation du document de test")
    );
    assert_eq!(info.location.as_deref(), Some("Paris"));
    assert_eq!(info.byte_range.len(), 2);
    assert_eq!(info.covered_end(), doc.bytes().len());

    let report = verify_signature(&doc, info, &[root]).unwrap();
    assert_eq!(
        report.digest.status,
        Status::Valid,
        "{}",
        report.digest.detail
    );
    assert_eq!(
        report.signature.status,
        Status::Valid,
        "{}",
        report.signature.detail
    );
    assert_eq!(
        report.chain.status,
        Status::Valid,
        "{}",
        report.chain.detail
    );
    assert_eq!(
        report.coverage.status,
        Status::Valid,
        "{}",
        report.coverage.detail
    );
    assert_eq!(
        report.modifications.status,
        Status::Valid,
        "{}",
        report.modifications.detail
    );
    assert!(report.is_fully_valid());
    assert_eq!(report.signer_common_name.as_deref(), Some("Camille Test"));
    assert_eq!(report.signing_time, Some(moment(2026, 5, 20)));
    assert_eq!(report.chain_path.len(), 2);
}

/// Sans racine de confiance, la chaîne est « inconnue » — jamais « valide ».
#[test]
fn without_a_trusted_root_the_chain_stays_unknown() {
    let doc = Document::from_bytes(read(SIGNED_PDF)).unwrap();
    let signatures = list_signatures(&doc).unwrap();
    let report = verify_signature(&doc, signatures.first().unwrap(), &[]).unwrap();
    assert_eq!(report.digest.status, Status::Valid);
    assert_eq!(report.signature.status, Status::Valid);
    assert_eq!(report.chain.status, Status::Unknown);
    assert!(!report.is_fully_valid());
}

/// Un seul octet modifié dans la zone couverte doit faire échouer le condensé.
#[test]
fn one_changed_byte_breaks_the_digest() {
    let root = Certificate::parse(&read(ROOT_CERT)).unwrap();
    let original = read(SIGNED_PDF);
    // On vise un octet du flux de contenu de la page, bien à l'intérieur de
    // la première plage couverte.
    let position = find(&original, b"Attestation de test").expect("texte de la page") + 2;
    let mut mutated = original.clone();
    if let Some(byte) = mutated.get_mut(position) {
        *byte = b'X';
    }
    assert_ne!(mutated, original);
    let doc = Document::from_bytes(mutated).unwrap();
    let signatures = list_signatures(&doc).unwrap();
    let report = verify_signature(&doc, signatures.first().unwrap(), &[root]).unwrap();
    assert_eq!(
        report.digest.status,
        Status::Invalid,
        "un octet changé doit invalider le condensé"
    );
    // La signature CMS, elle, reste bonne : c'est le lien avec le document qui
    // est rompu. Distinguer les deux est tout l'intérêt des verdicts séparés.
    assert_eq!(report.signature.status, Status::Valid);
    assert!(!report.is_fully_valid());
}

/// Une mise à jour incrémentale postérieure est détectée : la signature ne
/// couvre plus tout le fichier et des objets ont changé.
#[test]
fn an_incremental_update_after_signing_is_detected() {
    let root = Certificate::parse(&read(ROOT_CERT)).unwrap();
    let doc = Document::from_bytes(read(SIGNED_PDF)).unwrap();
    // On remplace le contenu de la page : exactement ce que fait une attaque
    // par sauvegarde incrémentale.
    let pages = acrux_document::collect_pages(&doc).unwrap();
    let page = pages.first().unwrap();
    let contents = page
        .dict
        .get(&acrux_document::Name::new("Contents"))
        .cloned();
    let Some(acrux_document::Object::Reference(stream_reference)) = contents else {
        panic!("flux de contenu attendu par référence");
    };
    let replacement = b"BT /F1 20 Tf 40 250 Td (TOUT AUTRE CHOSE) Tj ET".to_vec();
    let mut dict = acrux_document::Dict::new();
    dict.insert(
        acrux_document::Name::new("Length"),
        acrux_document::Object::Integer(i64::try_from(replacement.len()).unwrap()),
    );
    doc.set(
        stream_reference,
        acrux_document::Object::Stream {
            dict,
            raw: replacement,
        },
    );
    let tampered = doc.save_incremental().unwrap();

    let doc = Document::from_bytes(tampered).unwrap();
    let signatures = list_signatures(&doc).unwrap();
    let report = verify_signature(&doc, signatures.first().unwrap(), &[root]).unwrap();
    // Le condensé et la signature restent valides : ils portent sur la
    // révision signée, qui n'a pas bougé.
    assert_eq!(report.digest.status, Status::Valid);
    assert_eq!(report.signature.status, Status::Valid);
    // Mais la couverture et les modifications dénoncent l'ajout.
    assert_eq!(report.coverage.status, Status::Invalid);
    assert_eq!(report.modifications.status, Status::Invalid);
    assert!(
        report.changed_objects.contains(&stream_reference.number),
        "l'objet réécrit doit être signalé : {:?}",
        report.changed_objects
    );
    assert!(!report.is_fully_valid());
}

/// Signer deux fois : la seconde signature est valide et couvre tout, la
/// première voit le document modifié après elle.
#[test]
fn signing_twice_keeps_both_fields() {
    let root_der = read(ROOT_CERT);
    let signed = read(SIGNED_PDF);
    let twice = sign_attestation(&signed, &read(SIGNER_KEY), &read(SIGNER_CERT), &root_der);
    let doc = Document::from_bytes(twice).unwrap();
    let signatures = list_signatures(&doc).unwrap();
    assert_eq!(signatures.len(), 2);
    let names: Vec<&str> = signatures.iter().map(|s| s.field_name.as_str()).collect();
    assert_eq!(names, vec!["Signature1", "Signature2"]);
    let root = Certificate::parse(&root_der).unwrap();
    let second = signatures
        .iter()
        .find(|s| s.field_name == "Signature2")
        .unwrap();
    let report = verify_signature(&doc, second, std::slice::from_ref(&root)).unwrap();
    assert!(report.is_fully_valid(), "{report:#?}");
    let first = signatures
        .iter()
        .find(|s| s.field_name == "Signature1")
        .unwrap();
    let report = verify_signature(&doc, first, &[root]).unwrap();
    assert_eq!(report.digest.status, Status::Valid);
    assert_eq!(report.coverage.status, Status::Invalid);
}

/// Un champ de signature **préparé mais vide** est rempli, pas doublé.
#[test]
fn a_prepared_empty_field_is_filled_in_place() {
    use acrux_document::{Dict, Name, Object};

    // On prépare un champ /Sig vide sur l'attestation, comme le ferait
    // « préparer un formulaire » dans Acrobat.
    let doc = Document::from_bytes(read(UNSIGNED_PDF)).unwrap();
    let pages = acrux_document::collect_pages(&doc).unwrap();
    let page_reference = pages.first().unwrap().reference.unwrap();
    let field_reference = doc.allocate();
    let mut field = Dict::new();
    field.insert(Name::new("Type"), Object::Name(Name::new("Annot")));
    field.insert(Name::new("Subtype"), Object::Name(Name::new("Widget")));
    field.insert(Name::new("FT"), Object::Name(Name::new("Sig")));
    field.insert(
        Name::new("T"),
        Object::String(crate::annotations::encode_text("ChampPrepare")),
    );
    field.insert(Name::new("P"), Object::Reference(page_reference));
    field.insert(Name::new("F"), Object::Integer(4));
    field.insert(
        Name::new("Rect"),
        Object::Array(vec![
            Object::Real(240.0),
            Object::Real(80.0),
            Object::Real(380.0),
            Object::Real(120.0),
        ]),
    );
    doc.set(field_reference, Object::Dict(field));
    let mut page = pages.first().unwrap().dict.clone();
    page.insert(
        Name::new("Annots"),
        Object::Array(vec![Object::Reference(field_reference)]),
    );
    doc.set(page_reference, Object::Dict(page));
    let mut form = Dict::new();
    form.insert(
        Name::new("Fields"),
        Object::Array(vec![Object::Reference(field_reference)]),
    );
    form.insert(Name::new("SigFlags"), Object::Integer(1));
    let form_reference = doc.add(Object::Dict(form));
    let mut catalog = doc.catalog().unwrap();
    catalog.insert(Name::new("AcroForm"), Object::Reference(form_reference));
    let Some(Object::Reference(catalog_reference)) = doc.trailer().get(&Name::new("Root")).cloned()
    else {
        panic!("catalogue indirect attendu");
    };
    doc.set(catalog_reference, Object::Dict(catalog));
    let prepared = doc.save_incremental().unwrap();

    let doc = Document::from_bytes(prepared).unwrap();
    let signatures = list_signatures(&doc).unwrap();
    assert_eq!(signatures.len(), 1);
    assert!(signatures.first().unwrap().unsigned);

    // On signe en nommant ce champ : il doit être rempli, pas dupliqué.
    let key = SigningKey::from_der(&read(SIGNER_KEY), &read(SIGNER_CERT))
        .unwrap()
        .with_chain(&[read(ROOT_CERT)])
        .unwrap();
    let signed = sign(
        &doc,
        &key,
        &SignOptions {
            field_name: Some("ChampPrepare".into()),
            signing_time: Some(moment(2026, 5, 20)),
            ..SignOptions::default()
        },
    )
    .unwrap();

    let doc = Document::from_bytes(signed).unwrap();
    let signatures = list_signatures(&doc).unwrap();
    assert_eq!(
        signatures.len(),
        1,
        "le champ préparé ne doit pas être doublé"
    );
    let info = signatures.first().unwrap();
    assert!(!info.unsigned);
    assert_eq!(info.field_name, "ChampPrepare");
    // Le rectangle préparé est conservé.
    let rect = info.rect.unwrap();
    assert!((rect.x0 - 240.0).abs() < 0.01 && (rect.y1 - 120.0).abs() < 0.01);
    let root = Certificate::parse(&read(ROOT_CERT)).unwrap();
    let report = verify_signature(&doc, info, &[root]).unwrap();
    assert!(report.is_fully_valid(), "{report:#?}");
}

/// Le fichier non signé du corpus expose bien zéro signature.
#[test]
fn the_unsigned_corpus_file_has_no_signature() {
    let doc = Document::from_bytes(read(UNSIGNED_PDF)).unwrap();
    assert!(list_signatures(&doc).unwrap().is_empty());
}

fn find(data: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || data.len() < needle.len() {
        return None;
    }
    (0..=data.len() - needle.len()).find(|&i| data.get(i..i + needle.len()) == Some(needle))
}
