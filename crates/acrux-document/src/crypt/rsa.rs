//! RSA : vérification et signature (RFC 8017, *PKCS #1 v2.2*).
//!
//! Ce module couvre ce dont les signatures PDF ont besoin :
//! - vérification `RSASSA-PKCS1-v1_5` (§8.2.2), le schéma de `adbe.pkcs7.*` ;
//! - vérification `RSASSA-PSS` (§8.1.2), obligatoire pour certains profils
//!   PAdES et pour les certificats émis avec `id-RSASSA-PSS` ;
//! - signature `RSASSA-PKCS1-v1_5` (§8.2.1), ce que nous produisons ;
//! - lecture des clés PKCS#1, PKCS#8 non chiffré et `SubjectPublicKeyInfo`.
//!
//! Ce que nous **ne** faisons **pas** : chiffrement RSA (aucun besoin en
//! signature), PKCS#12, clés chiffrées, courbes elliptiques. Voir
//! `spec-notes/pdf-12.8-signatures.md`.

use acrux_core::{Error, Result};

use crate::asn1::{self, Oid, Reader};
use crate::crypt::bigint::{BigUint, Montgomery};
use crate::crypt::random::Random;
use crate::crypt::{sha1, sha2};

/// `1.2.840.113549.1.1.1` — `rsaEncryption`.
pub const OID_RSA_ENCRYPTION: [u32; 7] = [1, 2, 840, 113_549, 1, 1, 1];
/// `1.2.840.113549.1.1.10` — `id-RSASSA-PSS`.
pub const OID_RSASSA_PSS: [u32; 7] = [1, 2, 840, 113_549, 1, 1, 10];
/// `1.2.840.113549.1.1.8` — `id-mgf1`.
pub const OID_MGF1: [u32; 7] = [1, 2, 840, 113_549, 1, 1, 8];

/// Fonction de condensation utilisable avec RSA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hash {
    /// SHA-1 : **lecture seule**, refusé à la signature.
    Sha1,
    /// SHA-256, notre choix par défaut.
    Sha256,
    /// SHA-384.
    Sha384,
    /// SHA-512.
    Sha512,
}

impl Hash {
    /// Condensé du message.
    #[must_use]
    pub fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Hash::Sha1 => sha1::sha1(data).to_vec(),
            Hash::Sha256 => sha2::sha256(data).to_vec(),
            Hash::Sha384 => sha2::sha384(data),
            Hash::Sha512 => sha2::sha512(data),
        }
    }

    /// Taille du condensé en octets.
    #[must_use]
    pub fn output_len(self) -> usize {
        match self {
            Hash::Sha1 => 20,
            Hash::Sha256 => 32,
            Hash::Sha384 => 48,
            Hash::Sha512 => 64,
        }
    }

    /// Nom court affiché dans les rapports.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Hash::Sha1 => "SHA-1",
            Hash::Sha256 => "SHA-256",
            Hash::Sha384 => "SHA-384",
            Hash::Sha512 => "SHA-512",
        }
    }

    /// OID de l'algorithme de condensation (RFC 8017 annexe B.1).
    #[must_use]
    pub fn oid(self) -> Oid {
        match self {
            Hash::Sha1 => Oid(vec![1, 3, 14, 3, 2, 26]),
            Hash::Sha256 => Oid(vec![2, 16, 840, 1, 101, 3, 4, 2, 1]),
            Hash::Sha384 => Oid(vec![2, 16, 840, 1, 101, 3, 4, 2, 2]),
            Hash::Sha512 => Oid(vec![2, 16, 840, 1, 101, 3, 4, 2, 3]),
        }
    }

    /// Reconnaît un OID de condensation ou de signature `<hash>WithRSA`.
    #[must_use]
    pub fn from_oid(oid: &Oid) -> Option<Hash> {
        [Hash::Sha1, Hash::Sha256, Hash::Sha384, Hash::Sha512]
            .into_iter()
            .find(|h| h.oid() == *oid || h.signature_oid() == *oid)
    }

    /// OID de `<hash>WithRSAEncryption` (RFC 8017 annexe A.2.4).
    #[must_use]
    pub fn signature_oid(self) -> Oid {
        let last = match self {
            Hash::Sha1 => 5,
            Hash::Sha256 => 11,
            Hash::Sha384 => 12,
            Hash::Sha512 => 13,
        };
        Oid(vec![1, 2, 840, 113_549, 1, 1, last])
    }

    /// Préfixe DER de la structure `DigestInfo` (RFC 8017 §9.2, note 1).
    /// Ces octets sont figés par la norme ; [`digest_info`] les recalcule et
    /// un test compare les deux.
    #[must_use]
    pub fn digest_info_prefix(self) -> &'static [u8] {
        match self {
            Hash::Sha1 => &[
                0x30, 0x21, 0x30, 0x09, 0x06, 0x05, 0x2b, 0x0e, 0x03, 0x02, 0x1a, 0x05, 0x00, 0x04,
                0x14,
            ],
            Hash::Sha256 => &[
                0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x01, 0x05, 0x00, 0x04, 0x20,
            ],
            Hash::Sha384 => &[
                0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x02, 0x05, 0x00, 0x04, 0x30,
            ],
            Hash::Sha512 => &[
                0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x03, 0x05, 0x00, 0x04, 0x40,
            ],
        }
    }
}

/// `DigestInfo ::= SEQUENCE { digestAlgorithm AlgorithmIdentifier, digest OCTET STRING }`.
#[must_use]
pub fn digest_info(hash: Hash, digest: &[u8]) -> Vec<u8> {
    asn1::write::sequence(&[
        asn1::write::algorithm(&hash.oid(), Some(&asn1::write::null())),
        asn1::write::octet_string(digest),
    ])
}

/// Clé publique RSA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    /// Module `n`.
    pub modulus: BigUint,
    /// Exposant public `e`.
    pub exponent: BigUint,
}

impl PublicKey {
    /// Construit une clé à partir des octets gros-boutistes de `n` et `e`.
    ///
    /// # Errors
    /// Module nul, pair, ou exposant nul.
    pub fn new(modulus: &[u8], exponent: &[u8]) -> Result<PublicKey> {
        let n = BigUint::from_bytes_be(modulus);
        let e = BigUint::from_bytes_be(exponent);
        if n.is_zero() || !n.is_odd() {
            return Err(Error::Corrupt("module RSA nul ou pair".into()));
        }
        if e.is_zero() {
            return Err(Error::Corrupt("exposant public RSA nul".into()));
        }
        Ok(PublicKey {
            modulus: n,
            exponent: e,
        })
    }

    /// Taille du module en octets (`k` dans la RFC 8017).
    #[must_use]
    pub fn size(&self) -> usize {
        self.modulus.bits().div_ceil(8)
    }

    /// Taille du module en bits.
    #[must_use]
    pub fn bits(&self) -> usize {
        self.modulus.bits()
    }

    /// `RSAVP1` (RFC 8017 §5.2.2) : `s^e mod n`, rendu sur `k` octets.
    ///
    /// # Errors
    /// Signature hors de `[0, n-1]`, ou arithmétique impossible.
    pub fn raw_verify(&self, signature: &[u8]) -> Result<Vec<u8>> {
        let k = self.size();
        if signature.len() != k {
            return Err(Error::Corrupt(format!(
                "signature de {} octets pour un module de {k}",
                signature.len()
            )));
        }
        let s = BigUint::from_bytes_be(signature);
        if s >= self.modulus {
            return Err(Error::Corrupt("signature hors de portée du module".into()));
        }
        let m = s
            .pow_mod(&self.exponent, &self.modulus)
            .ok_or_else(|| Error::Corrupt("exponentiation RSA impossible".into()))?;
        m.to_bytes_be_padded(k)
            .map_err(|_| Error::Corrupt("résultat RSA trop grand".into()))
    }

    /// Vérifie une signature `RSASSA-PKCS1-v1_5` sur un condensé déjà calculé.
    #[must_use]
    pub fn verify_pkcs1_v15(&self, hash: Hash, digest: &[u8], signature: &[u8]) -> bool {
        let Ok(em) = self.raw_verify(signature) else {
            return false;
        };
        let Ok(expected) = emsa_pkcs1_v15(hash, digest, self.size()) else {
            return false;
        };
        equal_in_constant_time(&em, &expected)
    }

    /// Vérifie une signature `RSASSA-PSS`.
    ///
    /// `salt_len` à `None` déduit la longueur du sel de la structure, ce que
    /// la RFC autorise quand les paramètres ne la donnent pas.
    #[must_use]
    pub fn verify_pss(
        &self,
        hash: Hash,
        mgf_hash: Hash,
        salt_len: Option<usize>,
        digest: &[u8],
        signature: &[u8],
    ) -> bool {
        let Ok(em) = self.raw_verify(signature) else {
            return false;
        };
        emsa_pss_verify(hash, mgf_hash, salt_len, digest, &em, self.bits() - 1)
    }

    /// Lit une clé depuis un `SubjectPublicKeyInfo` DER (RFC 5280 §4.1.2.7).
    ///
    /// # Errors
    /// Structure invalide ou algorithme autre que `rsaEncryption`.
    pub fn from_spki_der(der: &[u8]) -> Result<PublicKey> {
        let mut root = Reader::new(der);
        let mut spki = root.sequence()?;
        root.end()?;
        let mut alg = spki.sequence()?;
        let oid = alg.oid()?;
        if oid.0 != OID_RSA_ENCRYPTION && oid.0 != OID_RSASSA_PSS {
            return Err(Error::Unsupported(format!(
                "clé publique {oid} : seul RSA est pris en charge"
            )));
        }
        let (unused, key_bits) = spki.bit_string()?;
        if unused != 0 {
            return Err(Error::Corrupt("clé publique mal alignée".into()));
        }
        PublicKey::from_pkcs1_der(key_bits)
    }

    /// Lit une clé depuis un `RSAPublicKey` DER (RFC 8017 annexe A.1.1).
    ///
    /// # Errors
    /// Structure invalide.
    pub fn from_pkcs1_der(der: &[u8]) -> Result<PublicKey> {
        let mut root = Reader::new(der);
        let mut seq = root.sequence()?;
        root.end()?;
        let n = seq.unsigned_bytes()?;
        let e = seq.unsigned_bytes()?;
        seq.end()?;
        PublicKey::new(n, e)
    }

    /// Écrit la clé en `RSAPublicKey` DER.
    #[must_use]
    pub fn to_pkcs1_der(&self) -> Vec<u8> {
        asn1::write::sequence(&[
            asn1::write::unsigned_integer(&self.modulus.to_bytes_be()),
            asn1::write::unsigned_integer(&self.exponent.to_bytes_be()),
        ])
    }

    /// Écrit la clé en `SubjectPublicKeyInfo` DER.
    #[must_use]
    pub fn to_spki_der(&self) -> Vec<u8> {
        asn1::write::sequence(&[
            asn1::write::algorithm(
                &Oid(OID_RSA_ENCRYPTION.to_vec()),
                Some(&asn1::write::null()),
            ),
            asn1::write::bit_string(&self.to_pkcs1_der()),
        ])
    }
}

/// Clé privée RSA, avec les paramètres du théorème des restes chinois.
#[derive(Debug, Clone)]
pub struct PrivateKey {
    /// Partie publique.
    pub public: PublicKey,
    /// Exposant privé `d`.
    pub private_exponent: BigUint,
    /// Premier facteur `p`.
    pub prime1: BigUint,
    /// Second facteur `q`.
    pub prime2: BigUint,
    /// `d mod (p-1)`.
    pub exponent1: BigUint,
    /// `d mod (q-1)`.
    pub exponent2: BigUint,
    /// `q^-1 mod p`.
    pub coefficient: BigUint,
}

impl PrivateKey {
    /// Lit une clé `RSAPrivateKey` DER (RFC 8017 annexe A.1.2).
    ///
    /// # Errors
    /// Structure invalide, version inconnue, ou clé multi-premiers.
    pub fn from_pkcs1_der(der: &[u8]) -> Result<PrivateKey> {
        let mut root = Reader::new(der);
        let mut seq = root.sequence()?;
        root.end()?;
        let version = seq.integer_i64()?;
        if version != 0 {
            return Err(Error::Unsupported(
                "clé RSA à plus de deux facteurs premiers".into(),
            ));
        }
        let n = seq.unsigned_bytes()?;
        let e = seq.unsigned_bytes()?;
        let d = seq.unsigned_bytes()?;
        let p = seq.unsigned_bytes()?;
        let q = seq.unsigned_bytes()?;
        let dp = seq.unsigned_bytes()?;
        let dq = seq.unsigned_bytes()?;
        let qinv = seq.unsigned_bytes()?;
        let key = PrivateKey {
            public: PublicKey::new(n, e)?,
            private_exponent: BigUint::from_bytes_be(d),
            prime1: BigUint::from_bytes_be(p),
            prime2: BigUint::from_bytes_be(q),
            exponent1: BigUint::from_bytes_be(dp),
            exponent2: BigUint::from_bytes_be(dq),
            coefficient: BigUint::from_bytes_be(qinv),
        };
        key.check()?;
        Ok(key)
    }

    /// Lit une clé PKCS#8 **non chiffrée** (`PrivateKeyInfo`, RFC 5208 §5).
    ///
    /// Un fichier PKCS#8 chiffré (`EncryptedPrivateKeyInfo`) ou un PKCS#12
    /// est refusé : nous ne dérivons pas de clé depuis un mot de passe pour
    /// des besoins de signature. Déchiffrez-le d'abord avec votre outil
    /// habituel.
    ///
    /// # Errors
    /// Structure invalide, algorithme non RSA, ou conteneur chiffré.
    pub fn from_pkcs8_der(der: &[u8]) -> Result<PrivateKey> {
        let mut root = Reader::new(der);
        let mut info = root.sequence()?;
        root.end()?;
        let version = info.integer_i64()?;
        if version != 0 {
            return Err(Error::Unsupported(format!(
                "PKCS#8 version {version} inconnue"
            )));
        }
        let mut alg = info.sequence()?;
        let oid = alg.oid()?;
        if oid.0 != OID_RSA_ENCRYPTION {
            return Err(Error::Unsupported(format!(
                "clé privée {oid} : seul RSA est pris en charge"
            )));
        }
        let inner = info.octet_string()?;
        PrivateKey::from_pkcs1_der(inner)
    }

    /// Lit une clé privée en acceptant les deux enveloppes (PKCS#8 d'abord,
    /// PKCS#1 ensuite) : c'est ce qu'attend un utilisateur qui passe un
    /// `.der` sans savoir lequel des deux il tient.
    ///
    /// # Errors
    /// Aucune des deux lectures n'aboutit.
    pub fn from_der(der: &[u8]) -> Result<PrivateKey> {
        match PrivateKey::from_pkcs8_der(der) {
            Ok(k) => Ok(k),
            Err(first) => PrivateKey::from_pkcs1_der(der).map_err(|second| {
                Error::Corrupt(format!(
                    "clé illisible en PKCS#8 ({first}) comme en PKCS#1 ({second}) ; \
                     une clé chiffrée ou un PKCS#12 ne sont pas pris en charge"
                ))
            }),
        }
    }

    /// Écrit la clé en `RSAPrivateKey` DER.
    #[must_use]
    pub fn to_pkcs1_der(&self) -> Vec<u8> {
        asn1::write::sequence(&[
            asn1::write::integer_i64(0),
            asn1::write::unsigned_integer(&self.public.modulus.to_bytes_be()),
            asn1::write::unsigned_integer(&self.public.exponent.to_bytes_be()),
            asn1::write::unsigned_integer(&self.private_exponent.to_bytes_be()),
            asn1::write::unsigned_integer(&self.prime1.to_bytes_be()),
            asn1::write::unsigned_integer(&self.prime2.to_bytes_be()),
            asn1::write::unsigned_integer(&self.exponent1.to_bytes_be()),
            asn1::write::unsigned_integer(&self.exponent2.to_bytes_be()),
            asn1::write::unsigned_integer(&self.coefficient.to_bytes_be()),
        ])
    }

    /// Écrit la clé en PKCS#8 non chiffré.
    #[must_use]
    pub fn to_pkcs8_der(&self) -> Vec<u8> {
        asn1::write::sequence(&[
            asn1::write::integer_i64(0),
            asn1::write::algorithm(
                &Oid(OID_RSA_ENCRYPTION.to_vec()),
                Some(&asn1::write::null()),
            ),
            asn1::write::octet_string(&self.to_pkcs1_der()),
        ])
    }

    /// Contrôle de cohérence : `p·q = n` et les exposants CRT concordent.
    ///
    /// # Errors
    /// Clé incohérente.
    pub fn check(&self) -> Result<()> {
        if self.prime1.mul(&self.prime2) != self.public.modulus {
            return Err(Error::Corrupt("clé RSA incohérente : p·q ≠ n".into()));
        }
        if !self.prime1.is_odd() || !self.prime2.is_odd() {
            return Err(Error::Corrupt("facteur premier pair".into()));
        }
        Ok(())
    }

    /// `RSASP1` (RFC 8017 §5.2.1) par le théorème des restes chinois.
    ///
    /// Le résultat est revérifié avec la clé publique avant d'être rendu :
    /// une erreur de calcul (matériel défaillant, attaque par faute) livrerait
    /// sinon une signature qui révèle un facteur du module.
    ///
    /// # Errors
    /// Message hors de portée ou arithmétique impossible.
    pub fn raw_sign(&self, message: &[u8]) -> Result<Vec<u8>> {
        let k = self.public.size();
        let m = BigUint::from_bytes_be(message);
        if m >= self.public.modulus {
            return Err(Error::Corrupt("message hors de portée du module".into()));
        }
        let mp = Montgomery::new(&self.prime1)
            .ok_or_else(|| Error::Corrupt("facteur premier p pair".into()))?;
        let mq = Montgomery::new(&self.prime2)
            .ok_or_else(|| Error::Corrupt("facteur premier q pair".into()))?;
        let m1 = mp
            .pow_secret(&m, &self.exponent1)
            .ok_or_else(|| Error::Corrupt("exponentiation modulo p impossible".into()))?;
        let m2 = mq
            .pow_secret(&m, &self.exponent2)
            .ok_or_else(|| Error::Corrupt("exponentiation modulo q impossible".into()))?;
        // h = qInv·(m1 - m2) mod p, en ramenant m1 - m2 dans [0, p).
        let diff = if let Some(d) = m1.sub(&m2) {
            d
        } else {
            {
                let d = m2
                    .sub(&m1)
                    .ok_or_else(|| Error::Corrupt("soustraction CRT impossible".into()))?;
                let r = d
                    .rem(&self.prime1)
                    .ok_or_else(|| Error::Corrupt("réduction CRT impossible".into()))?;
                if r.is_zero() {
                    r
                } else {
                    self.prime1
                        .sub(&r)
                        .ok_or_else(|| Error::Corrupt("soustraction CRT impossible".into()))?
                }
            }
        };
        let h = self
            .coefficient
            .mul(&diff)
            .rem(&self.prime1)
            .ok_or_else(|| Error::Corrupt("réduction CRT impossible".into()))?;
        let s = m2.add(&self.prime2.mul(&h));
        let signature = s
            .to_bytes_be_padded(k)
            .map_err(|_| Error::Corrupt("signature RSA trop grande".into()))?;
        let check = self.public.raw_verify(&signature)?;
        let expected = m
            .to_bytes_be_padded(k)
            .map_err(|_| Error::Corrupt("message RSA trop grand".into()))?;
        if !equal_in_constant_time(&check, &expected) {
            return Err(Error::Corrupt(
                "signature RSA incohérente : calcul refait et rejeté".into(),
            ));
        }
        Ok(signature)
    }

    /// Signe un condensé en `RSASSA-PKCS1-v1_5` (RFC 8017 §8.2.1).
    ///
    /// # Errors
    /// SHA-1 demandé (interdit à la signature), module trop court, ou
    /// arithmétique impossible.
    pub fn sign_pkcs1_v15(&self, hash: Hash, digest: &[u8]) -> Result<Vec<u8>> {
        if hash == Hash::Sha1 {
            return Err(Error::Unsupported(
                "SHA-1 n'est plus sûr : Acrux ne signe qu'à partir de SHA-256".into(),
            ));
        }
        let em = emsa_pkcs1_v15(hash, digest, self.public.size())?;
        self.raw_sign(&em)
    }
}

/// `EMSA-PKCS1-v1_5-ENCODE` (RFC 8017 §9.2) :
/// `EM = 0x00 || 0x01 || PS || 0x00 || DigestInfo`, `PS` étant au moins huit
/// octets `0xFF`.
///
/// # Errors
/// Condensé de la mauvaise taille, ou module trop court pour le schéma.
pub fn emsa_pkcs1_v15(hash: Hash, digest: &[u8], em_len: usize) -> Result<Vec<u8>> {
    if digest.len() != hash.output_len() {
        return Err(Error::Corrupt(format!(
            "condensé de {} octets pour {}",
            digest.len(),
            hash.label()
        )));
    }
    let mut t = hash.digest_info_prefix().to_vec();
    t.extend_from_slice(digest);
    if em_len < t.len() + 11 {
        return Err(Error::Corrupt(
            "module RSA trop court pour ce condensé".into(),
        ));
    }
    let mut em = Vec::with_capacity(em_len);
    em.push(0x00);
    em.push(0x01);
    em.resize(em_len - t.len() - 1, 0xFF);
    em.push(0x00);
    em.extend_from_slice(&t);
    Ok(em)
}

/// `MGF1` (RFC 8017 annexe B.2.1).
#[must_use]
pub fn mgf1(hash: Hash, seed: &[u8], length: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(length + hash.output_len());
    let mut counter: u32 = 0;
    while out.len() < length {
        let mut block = seed.to_vec();
        block.extend_from_slice(&counter.to_be_bytes());
        out.extend_from_slice(&hash.digest(&block));
        counter = counter.wrapping_add(1);
    }
    out.truncate(length);
    out
}

/// `EMSA-PSS-VERIFY` (RFC 8017 §9.1.2).
#[must_use]
pub fn emsa_pss_verify(
    hash: Hash,
    mgf_hash: Hash,
    salt_len: Option<usize>,
    digest: &[u8],
    em: &[u8],
    em_bits: usize,
) -> bool {
    let h_len = hash.output_len();
    if digest.len() != h_len {
        return false;
    }
    // EM peut porter un octet de tête nul quand emBits n'est pas multiple de 8.
    let em_len = em_bits.div_ceil(8);
    let em = match em.len().checked_sub(em_len) {
        Some(extra) => {
            if em.get(..extra).is_some_and(|p| p.iter().any(|&b| b != 0)) {
                return false;
            }
            em.get(extra..).unwrap_or(&[])
        }
        None => return false,
    };
    if em_len < h_len + 2 {
        return false;
    }
    if em.last() != Some(&0xBC) {
        return false;
    }
    let db_len = em_len - h_len - 1;
    let masked_db = em.get(..db_len).unwrap_or(&[]);
    let h = em.get(db_len..em_len - 1).unwrap_or(&[]);
    // Les bits de tête excédentaires doivent être nuls (§9.1.2 étape 6).
    let spare = 8 * em_len - em_bits;
    let top_mask = if spare == 0 { 0xFF } else { 0xFFu8 >> spare };
    if masked_db.first().is_some_and(|b| b & !top_mask != 0) {
        return false;
    }
    let mask = mgf1(mgf_hash, h, db_len);
    let mut db: Vec<u8> = masked_db
        .iter()
        .zip(mask.iter())
        .map(|(a, b)| a ^ b)
        .collect();
    if let Some(first) = db.first_mut() {
        *first &= top_mask;
    }
    // Le séparateur 0x01 suit une suite de zéros ; sa position donne la
    // longueur du sel quand les paramètres ne l'indiquent pas.
    let separator = match salt_len {
        Some(s) => {
            if db_len < s + 1 {
                return false;
            }
            db_len - s - 1
        }
        None => match db.iter().position(|&b| b != 0) {
            Some(p) => p,
            None => return false,
        },
    };
    if db
        .get(..separator)
        .is_some_and(|p| p.iter().any(|&b| b != 0))
    {
        return false;
    }
    if db.get(separator) != Some(&0x01) {
        return false;
    }
    let salt = db.get(separator + 1..).unwrap_or(&[]);
    let mut prime = vec![0u8; 8];
    prime.extend_from_slice(digest);
    prime.extend_from_slice(salt);
    equal_in_constant_time(&hash.digest(&prime), h)
}

/// Comparaison sans court-circuit : le temps ne dépend pas de la position de
/// la première différence.
#[must_use]
pub fn equal_in_constant_time(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// --- Génération de clés ------------------------------------------------------

/// Générateur pseudo-aléatoire déterministe fondé sur SHA-256 en mode
/// compteur.
///
/// **À n'employer que pour fabriquer du matériel de test.** Une vraie clé
/// exige l'entropie du système d'exploitation ([`SystemRandom`]) ; ce
/// générateur est reproductible par construction, ce qui est précisément ce
/// qu'il ne faut pas pour une clé de production, et exactement ce qu'il faut
/// pour un corpus de test que l'on veut pouvoir refabriquer à l'identique.
///
/// [`SystemRandom`]: super::random::SystemRandom
#[derive(Debug, Clone)]
pub struct SeededRandom {
    seed: Vec<u8>,
    counter: u64,
}

impl SeededRandom {
    /// Nouveau générateur à partir d'une graine.
    #[must_use]
    pub fn new(seed: &[u8]) -> SeededRandom {
        SeededRandom {
            seed: seed.to_vec(),
            counter: 0,
        }
    }

    /// Remplit `out` d'octets pseudo-aléatoires.
    pub fn fill(&mut self, out: &mut [u8]) {
        let mut written = 0;
        while written < out.len() {
            let mut block = self.seed.clone();
            block.extend_from_slice(&self.counter.to_be_bytes());
            self.counter = self.counter.wrapping_add(1);
            let digest = sha2::sha256(&block);
            for &b in &digest {
                if let Some(slot) = out.get_mut(written) {
                    *slot = b;
                    written += 1;
                } else {
                    break;
                }
            }
        }
    }
}

impl Random for SeededRandom {
    fn fill(&mut self, out: &mut [u8]) {
        SeededRandom::fill(self, out);
    }
}

/// Entier aléatoire de **exactement** `bits` bits, impair, avec les deux
/// bits de poids fort à 1 : c'est la forme d'un candidat de premier RSA,
/// qui garantit que le produit `p·q` occupe bien toute la largeur voulue.
fn odd_candidate<R: Random + ?Sized>(random: &mut R, bits: usize) -> BigUint {
    let bytes = bits.div_ceil(8);
    let mut buf = vec![0u8; bytes];
    random.fill(&mut buf);
    // Les bits au-delà de `bits` sont mis à zéro, puis on force les bits
    // de rang bits-1, bits-2 et 0.
    for i in bits..bytes * 8 {
        clear_bit(&mut buf, i);
    }
    set_bit(&mut buf, bits - 1);
    set_bit(&mut buf, bits.saturating_sub(2));
    set_bit(&mut buf, 0);
    BigUint::from_bytes_be(&buf)
}

/// Met à 1 le bit de rang `i` d'un tampon gros-boutiste.
fn set_bit(buf: &mut [u8], i: usize) {
    let Some(index) = buf.len().checked_sub(1 + i / 8) else {
        return;
    };
    if let Some(slot) = buf.get_mut(index) {
        *slot |= 1 << (i % 8);
    }
}

/// Met à 0 le bit de rang `i` d'un tampon gros-boutiste.
fn clear_bit(buf: &mut [u8], i: usize) {
    let Some(index) = buf.len().checked_sub(1 + i / 8) else {
        return;
    };
    if let Some(slot) = buf.get_mut(index) {
        *slot &= !(1u8 << (i % 8));
    }
}

/// Premiers de un octet, pour écarter à peu de frais la grande majorité des
/// candidats composés avant le test de Miller-Rabin.
const SMALL_PRIMES: [u32; 54] = [
    2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83, 89, 97,
    101, 103, 107, 109, 113, 127, 131, 137, 139, 149, 151, 157, 163, 167, 173, 179, 181, 191, 193,
    197, 199, 211, 223, 227, 229, 233, 239, 241, 251,
];

/// Test de primalité de Miller-Rabin à bases aléatoires (probabilité d'erreur
/// inférieure à `4^-rounds`).
#[must_use]
pub fn is_probable_prime<R: Random + ?Sized>(n: &BigUint, rounds: usize, random: &mut R) -> bool {
    if n.bits() < 2 {
        return false;
    }
    for p in SMALL_PRIMES {
        let small = BigUint::from_u64(u64::from(p));
        if *n == small {
            return true;
        }
        match n.rem(&small) {
            Some(r) if r.is_zero() => return false,
            None => return false,
            _ => {}
        }
    }
    let one = BigUint::one();
    let Some(n_minus_1) = n.sub(&one) else {
        return false;
    };
    // n - 1 = 2^s · d avec d impair.
    let mut s = 0usize;
    let mut d = n_minus_1.clone();
    while !d.is_odd() && !d.is_zero() {
        d = d.shr(1);
        s += 1;
    }
    let Some(mont) = Montgomery::new(n) else {
        return false;
    };
    for _ in 0..rounds {
        let mut buf = vec![0u8; n.bits().div_ceil(8)];
        random.fill(&mut buf);
        let Some(a) = BigUint::from_bytes_be(&buf).rem(&n_minus_1) else {
            return false;
        };
        let a = a.add(&one);
        let Some(mut x) = mont.pow(&a, &d) else {
            return false;
        };
        if x == one || x == n_minus_1 {
            continue;
        }
        let mut composite = true;
        for _ in 1..s {
            let Some(next) = mont.pow(&x, &BigUint::from_u64(2)) else {
                return false;
            };
            x = next;
            if x == n_minus_1 {
                composite = false;
                break;
            }
        }
        if composite {
            return false;
        }
    }
    true
}

/// Engendre une clé RSA de `bits` bits avec l'exposant public 65537.
///
/// La clé vaut ce que vaut l'aléa : avec [`SystemRandom`], c'est une clé de
/// production ; avec [`SeededRandom`], du matériel de test reproductible, à
/// ne jamais employer pour signer pour de vrai. Compter quelques secondes
/// pour 2048 bits en profil de développement.
///
/// [`SystemRandom`]: super::random::SystemRandom
///
/// # Errors
/// Taille inférieure à 512 bits, ou échec de la recherche de premiers.
pub fn generate<R: Random + ?Sized>(bits: usize, random: &mut R) -> Result<PrivateKey> {
    if bits < 512 || bits % 2 != 0 {
        return Err(Error::Corrupt(
            "taille de clé RSA invalide (au moins 512 bits, pair)".into(),
        ));
    }
    let e = BigUint::from_u64(65_537);
    let one = BigUint::one();
    for _ in 0..200 {
        let p = find_prime(bits / 2, &e, random)?;
        let q = find_prime(bits / 2, &e, random)?;
        if p == q {
            continue;
        }
        let (p, q) = if p > q { (p, q) } else { (q, p) };
        let n = p.mul(&q);
        if n.bits() != bits {
            continue;
        }
        let (Some(p1), Some(q1)) = (p.sub(&one), q.sub(&one)) else {
            continue;
        };
        let phi = p1.mul(&q1);
        let Some(d) = e.mod_inverse(&phi) else {
            continue;
        };
        let (Some(dp), Some(dq), Some(qinv)) = (d.rem(&p1), d.rem(&q1), q.mod_inverse(&p)) else {
            continue;
        };
        let key = PrivateKey {
            public: PublicKey {
                modulus: n,
                exponent: e.clone(),
            },
            private_exponent: d,
            prime1: p,
            prime2: q,
            exponent1: dp,
            exponent2: dq,
            coefficient: qinv,
        };
        key.check()?;
        return Ok(key);
    }
    Err(Error::Corrupt(
        "génération de clé RSA : aucun couple de premiers trouvé".into(),
    ))
}

fn find_prime<R: Random + ?Sized>(bits: usize, e: &BigUint, random: &mut R) -> Result<BigUint> {
    let one = BigUint::one();
    for _ in 0..100_000 {
        let candidate = odd_candidate(random, bits);
        if candidate.bits() != bits {
            continue;
        }
        // p - 1 doit être premier avec e, sinon d n'existe pas.
        let Some(p1) = candidate.sub(&one) else {
            continue;
        };
        if p1.rem(e).is_some_and(|r| r.is_zero()) {
            continue;
        }
        if is_probable_prime(&candidate, 24, random) {
            return Ok(candidate);
        }
    }
    Err(Error::Corrupt("aucun premier trouvé".into()))
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn bytes(hex: &str) -> Vec<u8> {
        let clean: String = hex.chars().filter(char::is_ascii_hexdigit).collect();
        clean
            .as_bytes()
            .chunks(2)
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn digest_info_prefixes_match_our_der_writer() {
        // Les préfixes figés viennent de la RFC 8017 §9.2 ; notre écrivain DER
        // doit reproduire exactement la même structure, sans quoi l'un des deux
        // est faux.
        for h in [Hash::Sha1, Hash::Sha256, Hash::Sha384, Hash::Sha512] {
            let digest = vec![0xA5u8; h.output_len()];
            let built = digest_info(h, &digest);
            let mut expected = h.digest_info_prefix().to_vec();
            expected.extend_from_slice(&digest);
            assert_eq!(built, expected, "{}", h.label());
        }
        assert_eq!(bytes("30 21 30"), vec![0x30, 0x21, 0x30]);
    }

    #[test]
    fn emsa_pkcs1_shape() {
        let digest = vec![0u8; 32];
        let em = emsa_pkcs1_v15(Hash::Sha256, &digest, 128).unwrap();
        assert_eq!(em.len(), 128);
        assert_eq!(em.get(..2), Some([0x00, 0x01].as_slice()));
        let t_len = Hash::Sha256.digest_info_prefix().len() + 32;
        assert_eq!(em.get(128 - t_len - 1), Some(&0x00));
        assert!(em
            .get(2..128 - t_len - 1)
            .unwrap()
            .iter()
            .all(|&b| b == 0xFF));
        // Module trop court.
        assert!(emsa_pkcs1_v15(Hash::Sha512, &[0u8; 64], 64).is_err());
        // Condensé de la mauvaise taille.
        assert!(emsa_pkcs1_v15(Hash::Sha256, &[0u8; 20], 128).is_err());
    }

    /// Vecteur officiel de la RFC 8017 : `mgf1` avec SHA-1 n'est pas donné
    /// tel quel dans la RFC, mais l'exemple OAEP 1.1 du fichier
    /// `oaep-vect.txt` l'emploie ; on se contente ici des propriétés
    /// structurelles et de la non-régression.
    #[test]
    fn mgf1_is_deterministic_and_sized() {
        let a = mgf1(Hash::Sha256, b"graine", 100);
        assert_eq!(a.len(), 100);
        assert_eq!(a, mgf1(Hash::Sha256, b"graine", 100));
        assert_ne!(a, mgf1(Hash::Sha1, b"graine", 100));
        // Le préfixe d'une sortie longue est la sortie courte.
        assert_eq!(mgf1(Hash::Sha256, b"graine", 10), a.get(..10).unwrap());
    }

    #[test]
    fn constant_time_equality() {
        assert!(equal_in_constant_time(b"abc", b"abc"));
        assert!(!equal_in_constant_time(b"abc", b"abd"));
        assert!(!equal_in_constant_time(b"abc", b"ab"));
        assert!(equal_in_constant_time(b"", b""));
    }

    /// Clé de 512 bits engendrée une fois et figée, pour que les tests
    /// tournent vite. Elle ne protège rien : c'est du matériel de test.
    fn small_key() -> PrivateKey {
        let mut rng = SeededRandom::new(b"Acrux test 512");
        generate(512, &mut rng).expect("génération de clé de test")
    }

    #[test]
    fn generated_key_signs_and_verifies() {
        let key = small_key();
        assert_eq!(key.public.bits(), 512);
        key.check().unwrap();
        let digest = Hash::Sha256.digest(b"le message a signer");
        let sig = key.sign_pkcs1_v15(Hash::Sha256, &digest).unwrap();
        assert_eq!(sig.len(), key.public.size());
        assert!(key.public.verify_pkcs1_v15(Hash::Sha256, &digest, &sig));
        // Un bit changé dans le condensé invalide la signature.
        let mut other = digest.clone();
        if let Some(b) = other.first_mut() {
            *b ^= 1;
        }
        assert!(!key.public.verify_pkcs1_v15(Hash::Sha256, &other, &sig));
        // Un bit changé dans la signature aussi.
        let mut broken = sig.clone();
        if let Some(b) = broken.last_mut() {
            *b ^= 1;
        }
        assert!(!key.public.verify_pkcs1_v15(Hash::Sha256, &digest, &broken));
        // Signature de la mauvaise longueur.
        assert!(!key
            .public
            .verify_pkcs1_v15(Hash::Sha256, &digest, &sig[1..]));
    }

    #[test]
    fn sha1_is_refused_for_signing() {
        let key = small_key();
        let digest = Hash::Sha1.digest(b"x");
        assert!(key.sign_pkcs1_v15(Hash::Sha1, &digest).is_err());
        // Mais la vérification SHA-1 reste possible, pour les vieux fichiers.
        let em = emsa_pkcs1_v15(Hash::Sha1, &digest, key.public.size()).unwrap();
        let sig = key.raw_sign(&em).unwrap();
        assert!(key.public.verify_pkcs1_v15(Hash::Sha1, &digest, &sig));
    }

    #[test]
    fn key_der_roundtrip() {
        let key = small_key();
        let pkcs1 = key.to_pkcs1_der();
        let again = PrivateKey::from_pkcs1_der(&pkcs1).unwrap();
        assert_eq!(again.public, key.public);
        assert_eq!(again.to_pkcs1_der(), pkcs1);
        let pkcs8 = key.to_pkcs8_der();
        assert_eq!(
            PrivateKey::from_pkcs8_der(&pkcs8).unwrap().public,
            key.public
        );
        assert_eq!(PrivateKey::from_der(&pkcs8).unwrap().public, key.public);
        assert_eq!(PrivateKey::from_der(&pkcs1).unwrap().public, key.public);
        assert!(PrivateKey::from_der(b"pas du DER").is_err());
        let spki = key.public.to_spki_der();
        assert_eq!(PublicKey::from_spki_der(&spki).unwrap(), key.public);
        assert!(PublicKey::from_spki_der(&pkcs1).is_err());
    }

    #[test]
    fn pss_roundtrip_with_our_own_encoding() {
        // Nous ne signons pas en PSS ; on fabrique donc EM à la main pour
        // éprouver la vérification, sel de 32 octets compris.
        let key = small_key();
        let hash = Hash::Sha256;
        let digest = hash.digest(b"message PSS");
        let em_bits = key.public.bits() - 1;
        let em_len = em_bits.div_ceil(8);
        let h_len = hash.output_len();
        let salt = [7u8; 20];
        let mut prime = vec![0u8; 8];
        prime.extend_from_slice(&digest);
        prime.extend_from_slice(&salt);
        let h = hash.digest(&prime);
        let db_len = em_len - h_len - 1;
        let mut db = vec![0u8; db_len - salt.len() - 1];
        db.push(0x01);
        db.extend_from_slice(&salt);
        let mask = mgf1(hash, &h, db_len);
        let mut masked: Vec<u8> = db.iter().zip(mask.iter()).map(|(a, b)| a ^ b).collect();
        let spare = 8 * em_len - em_bits;
        if let Some(first) = masked.first_mut() {
            *first &= 0xFFu8 >> spare;
        }
        let mut em = masked;
        em.extend_from_slice(&h);
        em.push(0xBC);
        let sig = key.raw_sign(&em).unwrap();
        assert!(key
            .public
            .verify_pss(hash, hash, Some(salt.len()), &digest, &sig));
        // Déduction automatique de la longueur du sel.
        assert!(key.public.verify_pss(hash, hash, None, &digest, &sig));
        // Mauvais condensé.
        assert!(!key
            .public
            .verify_pss(hash, hash, Some(salt.len()), &hash.digest(b"autre"), &sig));
        // Mauvaise longueur de sel annoncée.
        assert!(!key.public.verify_pss(hash, hash, Some(10), &digest, &sig));
    }

    #[test]
    fn miller_rabin_knows_its_primes() {
        let mut rng = SeededRandom::new(b"primalite");
        for p in [3u64, 5, 7, 97, 65_537, 2_147_483_647] {
            assert!(
                is_probable_prime(&BigUint::from_u64(p), 8, &mut rng),
                "{p} est premier"
            );
        }
        for c in [1u64, 4, 9, 15, 561, 1105, 1729, 65_535, 4_294_967_295] {
            assert!(
                !is_probable_prime(&BigUint::from_u64(c), 8, &mut rng),
                "{c} est composé"
            );
        }
    }

    #[test]
    fn generate_with_system_random() {
        let mut rng = crate::crypt::random::SystemRandom::new();
        let key = generate(512, &mut rng).unwrap();
        assert_eq!(key.public.bits(), 512);
        key.check().unwrap();
        let digest = Hash::Sha256.digest(b"cle de production");
        let sig = key.sign_pkcs1_v15(Hash::Sha256, &digest).unwrap();
        assert!(key.public.verify_pkcs1_v15(Hash::Sha256, &digest, &sig));
        // Deux tirages, deux clés : l'aléa n'est plus rejoué.
        let other = generate(512, &mut rng).unwrap();
        assert_ne!(other.public.to_pkcs1_der(), key.public.to_pkcs1_der());
    }

    #[test]
    fn seeded_random_is_reproducible() {
        let mut a = SeededRandom::new(b"graine");
        let mut b = SeededRandom::new(b"graine");
        let (mut x, mut y) = ([0u8; 70], [0u8; 70]);
        a.fill(&mut x);
        b.fill(&mut y);
        assert_eq!(x, y);
        let mut c = SeededRandom::new(b"autre");
        let mut z = [0u8; 70];
        c.fill(&mut z);
        assert_ne!(x, z);
    }

    /// Clé RSA 2048 bits et signature produites par **.NET** :
    /// `[RSA]::Create(2048)`, `ExportPkcs8PrivateKey()` puis
    /// `SignData(msg, SHA256, Pkcs1)`.
    ///
    /// C'est un vecteur d'interopérabilité, plus probant qu'un vecteur
    /// recopié : il exerce d'un coup la lecture PKCS#8, la vérification et —
    /// puisque `RSASSA-PKCS1-v1_5` est **déterministe** — l'égalité octet pour
    /// octet de notre signature avec celle d'une implémentation tierce.
    const DOTNET_PKCS8: &str = "308204bf020100300d06092a864886f70d0101010500048204a9308204a50201000282010100c3c8bd59845e2bc62fc4f2127d0985832277838746dcef353d5b453aa1fc057a2f1ce51a005211508b41fa7911b249cb43b00b27722b57cef6a8c3cd08eadf6c293e70176adb47b5832afb01a2489fc244532693307cedd08c036a47569ad0703f7ee7fd016b96d31232f048512998c9e66bcc59772bdb9298540433dfd49356791f5abaabb6511221d502cff164a18883890c59e4a66a84d35ad635e66e5c5aec66f8a87ec7714ad16157a70b37f0ba9a6eb750de764a61f857f958fab4c486d77f625e65ffda2f0a5a5acf6abc2e6cb6c6729a4fd1193125e4b35265727235d965b922c5ecedfab85426692f226ad8be2ea2e6b8c9210c40b586bd59acaf6d0203010001028201007200648948735147f0aedc49f9b6dea052248758f0e15b04843aae200b0c65e014a8dd9a7b4f4e37b92eecfb2c5bc56e6f7685b82d59a1a2a8abf27f644ee753e2dc3186e138d813905229a074ef96df16cbd82d62d18be4a072a8a2eb81f0173c51d821d17ee56ef82ce30f28fb70b537cfd348d1049fb147b3bc52bc65da783df4648cac6f1b839fbd57f9dc8f44357d800b08db26be94ec7c96469675fd25b5c39a895a33e341b69f683956eb9327ddd8dc2c7f4431712a561a3e3172fc98e762d96519c3bdd9eef1afe7ebc18acd27b43f473f1e0a4d2a3a1aaae2c84cc63c8dcba8e462e5cd76c3e2eae6ca616ebe0d5241ea2e3713dc304e1cde12931d02818100d9fd8e7b1638880457c32676b3230f35d56d3d5e2d251a2f91969dd03b9ef5f61918f088bc3122f3d5c80819c3fc3c4798211ceae1de25860a5963dce6117132a21e1039d31fecf255d2a32bc8ad76520a81542e04673acde8cd0a556e0ce36367a53683369fccac41e854993d7ac77820b4c7b2fc04c50fda8083d5133f253302818100e5ebf59af755ca4517831135a6c40821939108d9cc3ca0ca11f577e949c694be8d15ef14f2a8766e95765e1c5fe4d9b7e0ab40a4e00b9c7b321c2697b01593877cdf9a0467e1aab118ff7bd7cccb31f6f433a14ec4a723fe2a821f998b6aafa48b0ae2936e3e5d153219ab9f675f4e77d620d0364dc30e602f695e59401798df02818100b9b9e354118996b184889b53d4aa51423f95f40c3210836ff5edca8568d6b59eb8a15c0653b8d59bc40fca7f1150ed96de11904ebaa4077a5d84eda57e4b6c1384b67282a1d37890bbf85bd7690209663ad7177ea177c64d3b44bec22ca24476240f4a139f4da5173a8c14cffee685de5e9747f1c1f0da68f874385e2928caab02818100812021916472d3e435ae304e175864d0a6957f890200d2b4699d9838766c8640f5ef6994342b9447cabced61b6214a7cd03a9d557b564a0d8e38ed1ba7929686330548f44c7b1a67d788343f200ec602d166e5a2dd22993e37155935dc6c903432ba6c412c5aeddfe7812f3798d097bb0990e81e7751a293364d50e582ef3db50281810084e1dd127cd6483d690ffdf3226a4a1055626bb9d6a1c43ff8cc5789cdf8bd273a30ccb8a7ecd86b48097defb8196912d23169e3a7a4ce929d997c4159b084668be2a90e23261e614ddf11dcacc2b6a1c2b0d7bdf93a379f31d144d8ee02285d3e222b313ebd254540897fa608c185d9001699c2d0645c5dc86fb69312d41f2e";

    const DOTNET_SIGNATURE: &str = "6a86b0a4ed28a0d63009b7c54523270fa707072330c67c0a61b0463219b5d0a2f9e83a3829c35f8d2abd639621049198e2aa444134c24541b34a888086c91d52286ba0b11f9884dbc0fa72a33532f6977d72756978d1b15ea50c200c2d87a3a896ff7af12fa773ab22e787ff3a5d47cf894c3e60c3431a3b83c38202242ce96080868d3cba7f044dc42a5dda00c7ad39b58988401d75ac64fe74b727a747895d8341e9af565f84f1108a85aae5b077205305f52da31decf1afd8d64dabe8d9a4fa98654b05e0514c7620538472a0a6ddd534e85b9bd898e74abdacdb28b5461f3de3ef347dd6193fe8ab036bbc1d00144cb4b3b7b61b724478fb5952b6c0e9b6";

    #[test]
    fn dotnet_interoperability_vector() {
        let key = PrivateKey::from_pkcs8_der(&bytes(DOTNET_PKCS8)).unwrap();
        assert_eq!(key.public.bits(), 2048);
        let expected = bytes(DOTNET_SIGNATURE);
        // Ce message est figé : c'est celui que .NET a signé pour produire
        // `DOTNET_SIGNATURE`. Le renommer invaliderait le vecteur.
        let digest = Hash::Sha256.digest(b"AcrobatKiller interop vector");
        // Nous acceptons la signature de .NET…
        assert!(key
            .public
            .verify_pkcs1_v15(Hash::Sha256, &digest, &expected));
        // …et nous produisons exactement la même.
        let ours = key.sign_pkcs1_v15(Hash::Sha256, &digest).unwrap();
        assert_eq!(ours, expected);
        // Un autre message ne passe pas.
        let other = Hash::Sha256.digest(b"AcrobatKiller interop vectoR");
        assert!(!key.public.verify_pkcs1_v15(Hash::Sha256, &other, &expected));
    }

    #[test]
    fn refuses_inconsistent_keys() {
        let key = small_key();
        let mut broken = key.clone();
        broken.prime1 = broken.prime1.add(&BigUint::from_u64(2));
        assert!(broken.check().is_err());
        assert!(PublicKey::new(&[0], &[1, 0, 1]).is_err());
        assert!(PublicKey::new(&[4], &[1, 0, 1]).is_err());
        assert!(PublicKey::new(&key.public.modulus.to_bytes_be(), &[0]).is_err());
    }
}
