//! Chiffrement des documents (ISO 32000-2 §7.6) : gestionnaire de sécurité
//! standard, révisions 2 à 6, RC4 et AES.
//!
//! Primitives écrites dans ce module : [`md5`], [`sha2`], [`rc4`], [`aes`],
//! et l'aléa cryptographique ([`random`] : ChaCha20 semé par le système).
//! Voir CHARTE_PROJET.md §1.2 : la décision de conserver ou non une
//! cryptographie maison reste ouverte ; ces implémentations suivent les
//! normes à la lettre et sont testées sur les vecteurs officiels.

#![allow(clippy::many_single_char_names)] // notation de la norme (O, U, P, R, V…)

pub mod aes;
pub mod bigint;
pub mod md5;
pub mod random;
pub mod rc4;
pub mod rsa;
pub mod sha1;
pub mod sha2;

use acrux_core::{Error, Result};

use crate::objects::{Dict, Name, Object, ObjectRef};

/// Constante de remplissage des mots de passe (§7.6.4.3.2, algorithme 2).
const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// Méthode de chiffrement d'un type d'objet (§7.6.5, table 25 : `/CFM`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// `/Identity` : aucun chiffrement.
    None,
    /// `/V2` : RC4.
    Rc4,
    /// `/AESV2` : AES-128 CBC.
    AesV2,
    /// `/AESV3` : AES-256 CBC.
    AesV3,
}

/// Gestionnaire de sécurité standard, prêt à déchiffrer.
#[derive(Debug, Clone)]
pub struct SecurityHandler {
    key: Vec<u8>,
    revision: u32,
    streams: Method,
    strings: Method,
    encrypt_metadata: bool,
    /// Vrai si le mot de passe fourni était celui du propriétaire.
    owner: bool,
    permissions: i32,
    /// `/Perms` ne confirme pas `/P` (révisions 5 et 6, algorithme 13).
    perms_mismatch: bool,
}

/// Résolveur d'objets pour lire le dictionnaire `/Encrypt`.
pub type Resolver<'r> = &'r dyn Fn(&Object) -> Object;

impl SecurityHandler {
    /// Construit le gestionnaire à partir du dictionnaire `/Encrypt`, du premier
    /// élément de `/ID` et d'un mot de passe (vide = mot de passe utilisateur vide,
    /// cas de la grande majorité des PDF « protégés »).
    ///
    /// # Errors
    /// [`Error::Encrypted`] si le mot de passe n'ouvre pas le document,
    /// [`Error::Unsupported`] pour un gestionnaire ou un algorithme inconnu.
    pub fn new(encrypt: &Dict, id0: &[u8], password: &[u8], resolve: Resolver<'_>) -> Result<Self> {
        let get = |k: &str| encrypt.get(&Name::new(k)).map(resolve);
        let filter = get("Filter").and_then(|o| o.as_name().map(|n| n.0.clone()));
        if filter.as_deref() != Some(b"Standard") {
            return Err(Error::Unsupported(format!(
                "gestionnaire de sécurité {}",
                filter
                    .map(|f| String::from_utf8_lossy(&f).into_owned())
                    .unwrap_or_default()
            )));
        }
        let v = get("V").and_then(|o| o.as_i64()).unwrap_or(0);
        let r = get("R").and_then(|o| o.as_i64()).unwrap_or(2);
        let revision = u32::try_from(r).unwrap_or(2);
        let mut length_bits = get("Length").and_then(|o| o.as_i64()).unwrap_or(40);
        let o = string(get("O"));
        let u = string(get("U"));
        #[allow(clippy::cast_possible_truncation)]
        let p = get("P").and_then(|o| o.as_i64()).unwrap_or(-1) as i32;
        let encrypt_metadata = !matches!(get("EncryptMetadata"), Some(Object::Bool(false)));

        // Filtres de chiffrement (V ≥ 4).
        let (mut streams, mut strings) = (Method::Rc4, Method::Rc4);
        if v >= 4 {
            let cf = match get("CF") {
                Some(Object::Dict(d)) => d,
                _ => Dict::new(),
            };
            let method_of = |name: Option<Object>| -> (Method, Option<i64>) {
                let name = name
                    .and_then(|o| o.as_name().map(|n| n.0.clone()))
                    .unwrap_or_else(|| b"Identity".to_vec());
                if name == b"Identity" {
                    return (Method::None, None);
                }
                let Some(Object::Dict(f)) = cf.get(&Name(name)).map(resolve) else {
                    return (Method::None, None);
                };
                let cfm = f
                    .get(&Name::new("CFM"))
                    .and_then(Object::as_name)
                    .map(|n| n.0.clone());
                let len = f.get(&Name::new("Length")).and_then(Object::as_i64);
                let m = match cfm.as_deref() {
                    Some(b"V2") => Method::Rc4,
                    Some(b"AESV2") => Method::AesV2,
                    Some(b"AESV3") => Method::AesV3,
                    _ => Method::None,
                };
                (m, len)
            };
            let (sm, sl) = method_of(get("StmF"));
            let (tm, _) = method_of(get("StrF"));
            streams = sm;
            strings = tm;
            if let Some(l) = sl {
                // /Length dans un filtre est en octets (parfois en bits dans les fichiers réels).
                length_bits = if l <= 40 { l * 8 } else { l };
            }
            if streams == Method::AesV2 || strings == Method::AesV2 {
                length_bits = 128;
            }
        }
        if v >= 5 || streams == Method::AesV3 || strings == Method::AesV3 {
            length_bits = 256;
        }
        let key_len = usize::try_from(length_bits / 8).unwrap_or(5).clamp(5, 32);

        let (key, owner) = if revision >= 5 {
            let oe = string(get("OE"));
            let ue = string(get("UE"));
            Self::key_r6(revision, password, &o, &u, &oe, &ue)?
        } else {
            Self::key_legacy(
                revision,
                key_len,
                password,
                &o,
                &u,
                p,
                id0,
                encrypt_metadata,
            )?
        };
        let (permissions, perms_mismatch) = if revision >= 5 {
            check_perms(&key, &string(get("Perms")), p, owner)
        } else {
            (p, false)
        };
        Ok(Self {
            key,
            revision,
            streams,
            strings,
            encrypt_metadata,
            owner,
            permissions,
            perms_mismatch,
        })
    }

    /// Clé de fichier des révisions 2 à 4 (algorithmes 2, 4, 5, 6 et 7).
    #[allow(clippy::too_many_arguments)]
    fn key_legacy(
        revision: u32,
        key_len: usize,
        password: &[u8],
        o: &[u8],
        u: &[u8],
        p: i32,
        id0: &[u8],
        encrypt_metadata: bool,
    ) -> Result<(Vec<u8>, bool)> {
        let pwd = legacy_password(password);
        // 1. Mot de passe utilisateur ?
        let key = compute_key_legacy(revision, key_len, &pwd, o, p, id0, encrypt_metadata);
        if check_user_password_legacy(revision, &key, u, id0) {
            return Ok((key, false));
        }
        // 2. Mot de passe propriétaire ? (algorithme 7) : il donne le mot de passe utilisateur.
        let user_pwd = owner_to_user_legacy(revision, key_len, &pwd, o);
        let key = compute_key_legacy(revision, key_len, &user_pwd, o, p, id0, encrypt_metadata);
        if check_user_password_legacy(revision, &key, u, id0) {
            return Ok((key, true));
        }
        Err(Error::Encrypted)
    }

    /// Clé de fichier des révisions 5 (extension Adobe) et 6 (ISO 32000-2), algorithme 2.A.
    fn key_r6(
        revision: u32,
        password: &[u8],
        o: &[u8],
        u: &[u8],
        oe: &[u8],
        ue: &[u8],
    ) -> Result<(Vec<u8>, bool)> {
        if u.len() < 48 || o.len() < 48 || ue.len() < 32 || oe.len() < 32 {
            return Err(Error::Corrupt(
                "dictionnaire /Encrypt incomplet (R5/R6)".into(),
            ));
        }
        let pwd: Vec<u8> = password.iter().copied().take(127).collect();
        let hash = |salt: &[u8], udata: &[u8]| -> Vec<u8> { hash_2b(revision, &pwd, salt, udata) };
        // Propriétaire d'abord (§7.6.4.4.10 : algorithme 12).
        if hash(&o[32..40], &u[..48]) == o[..32] {
            let ik = hash(&o[40..48], &u[..48]);
            let aes = aes::Aes::new(&ik)?;
            return Ok((aes.cbc_decrypt_no_padding(&[0u8; 16], &oe[..32]), true));
        }
        // Utilisateur (algorithme 11).
        if hash(&u[32..40], &[]) == u[..32] {
            let ik = hash(&u[40..48], &[]);
            let aes = aes::Aes::new(&ik)?;
            return Ok((aes.cbc_decrypt_no_padding(&[0u8; 16], &ue[..32]), false));
        }
        Err(Error::Encrypted)
    }

    /// Révision du gestionnaire.
    #[must_use]
    pub const fn revision(&self) -> u32 {
        self.revision
    }

    /// Méthode de chiffrement des flux.
    #[must_use]
    pub const fn stream_method(&self) -> Method {
        self.streams
    }

    /// Algorithme en toutes lettres, pour l'affichage : « AES-256 (R6) ».
    #[must_use]
    pub fn describe(&self) -> String {
        let method = match self.streams {
            Method::AesV3 => "AES-256".to_string(),
            Method::AesV2 => "AES-128".to_string(),
            Method::Rc4 => format!("RC4 {} bits", self.key.len() * 8),
            Method::None => "aucun (identité)".to_string(),
        };
        format!("{method} (R{})", self.revision)
    }

    /// Le mot de passe fourni était celui du propriétaire.
    #[must_use]
    pub const fn is_owner(&self) -> bool {
        self.owner
    }

    /// Drapeaux de permissions `/P` (§7.6.4.2, table 22).
    #[must_use]
    pub const fn permissions(&self) -> i32 {
        self.permissions
    }

    /// Vrai si `/Perms` contredit `/P` : quelqu'un a retouché les
    /// permissions sans connaître la clé. Les permissions rendues sont alors
    /// les plus restrictives des deux (sauf pour le propriétaire, qui a tous
    /// les droits de toute façon).
    #[must_use]
    pub const fn perms_tampered(&self) -> bool {
        self.perms_mismatch
    }

    /// Vrai si le flux `/Metadata` est chiffré.
    #[must_use]
    pub const fn encrypts_metadata(&self) -> bool {
        self.encrypt_metadata
    }

    /// Clé de fichier (pour les tests et l'écriture).
    #[must_use]
    pub fn file_key(&self) -> &[u8] {
        &self.key
    }

    /// Clé propre à un objet (algorithme 1, §7.6.3.2) ; pour AES-256 la clé
    /// de fichier est utilisée telle quelle (algorithme 1.A).
    fn object_key(&self, r: ObjectRef, method: Method) -> Vec<u8> {
        if method == Method::AesV3 || self.revision >= 5 {
            return self.key.clone();
        }
        let mut h = md5::Md5::new();
        h.update(&self.key);
        h.update(&r.number.to_le_bytes()[..3]);
        h.update(&r.generation.to_le_bytes()[..2]);
        if method == Method::AesV2 {
            h.update(&[0x73, 0x41, 0x6C, 0x54]); // "sAlT"
        }
        let digest = h.finish();
        let n = (self.key.len() + 5).min(16);
        digest[..n].to_vec()
    }

    fn apply(&self, data: &[u8], r: ObjectRef, method: Method) -> Vec<u8> {
        match method {
            Method::None => data.to_vec(),
            Method::Rc4 => rc4::rc4(&self.object_key(r, method), data),
            Method::AesV2 | Method::AesV3 => match aes::Aes::new(&self.object_key(r, method)) {
                Ok(a) => a.cbc_decrypt(data),
                Err(_) => data.to_vec(),
            },
        }
    }

    /// Déchiffre une chaîne appartenant à l'objet `r`.
    #[must_use]
    pub fn decrypt_string(&self, data: &[u8], r: ObjectRef) -> Vec<u8> {
        self.apply(data, r, self.strings)
    }

    /// Déchiffre les données brutes d'un flux appartenant à l'objet `r`.
    #[must_use]
    pub fn decrypt_stream(&self, data: &[u8], r: ObjectRef) -> Vec<u8> {
        self.apply(data, r, self.streams)
    }

    /// Chiffre une chaîne (RC4 symétrique ; AES avec l'IV donné).
    #[must_use]
    pub fn encrypt_string(&self, data: &[u8], r: ObjectRef, iv: &[u8; 16]) -> Vec<u8> {
        self.encrypt(data, r, self.strings, iv)
    }

    /// Chiffre les données d'un flux.
    #[must_use]
    pub fn encrypt_stream(&self, data: &[u8], r: ObjectRef, iv: &[u8; 16]) -> Vec<u8> {
        self.encrypt(data, r, self.streams, iv)
    }

    fn encrypt(&self, data: &[u8], r: ObjectRef, method: Method, iv: &[u8; 16]) -> Vec<u8> {
        match method {
            Method::None => data.to_vec(),
            Method::Rc4 => rc4::rc4(&self.object_key(r, method), data),
            Method::AesV2 | Method::AesV3 => match aes::Aes::new(&self.object_key(r, method)) {
                Ok(a) => a.cbc_encrypt(iv, data),
                Err(_) => data.to_vec(),
            },
        }
    }

    /// Déchiffre récursivement toutes les chaînes et le flux d'un objet
    /// indirect `r` chargé depuis le fichier. Les flux `/Type /XRef` ne sont
    /// jamais chiffrés (§7.5.8.2) ; `/Metadata` ne l'est pas si
    /// `/EncryptMetadata false`.
    #[must_use]
    pub fn decrypt_object(&self, obj: Object, r: ObjectRef) -> Object {
        match obj {
            Object::String(s) => Object::String(self.decrypt_string(&s, r)),
            Object::Array(a) => {
                Object::Array(a.into_iter().map(|o| self.decrypt_object(o, r)).collect())
            }
            Object::Dict(d) => Object::Dict(self.decrypt_dict(d, r)),
            Object::Stream { dict, raw } => {
                let kind = dict
                    .get(&Name::new("Type"))
                    .and_then(Object::as_name)
                    .map(|n| n.0.clone());
                let skip = match kind.as_deref() {
                    Some(b"XRef") => true,
                    Some(b"Metadata") => !self.encrypt_metadata,
                    _ => false,
                };
                let raw = if skip {
                    raw
                } else {
                    self.decrypt_stream(&raw, r)
                };
                Object::Stream {
                    dict: self.decrypt_dict(dict, r),
                    raw,
                }
            }
            other => other,
        }
    }

    fn decrypt_dict(&self, d: Dict, r: ObjectRef) -> Dict {
        d.into_iter()
            .map(|(k, v)| (k, self.decrypt_object(v, r)))
            .collect()
    }
}

/// Bits de `/P` qui portent une permission (3 à 6 et 9 à 12) ; les autres
/// sont réservés, et des producteurs les remplissent chacun à sa façon.
const PERMISSION_BITS: i32 = 0b1111_0011_1100;

/// Algorithme 13 (§7.6.4.4.12) : `/Perms` est `/P` chiffré par la clé de
/// fichier, suivi de « adb ». Un `/P` retouché à la main pour s'accorder des
/// droits ne s'y retrouve plus, puisqu'il faudrait la clé pour refaire
/// `/Perms`.
///
/// Rend les permissions à appliquer et vrai en cas d'écart. Jamais d'échec :
/// des fichiers réels ont un `/Perms` faux ou absent, et doivent s'ouvrir.
/// Seul l'utilisateur est dégradé — au plus restrictif des deux valeurs.
fn check_perms(key: &[u8], perms: &[u8], p: i32, owner: bool) -> (i32, bool) {
    let Some(mut block) = perms.get(..16).and_then(|b| <[u8; 16]>::try_from(b).ok()) else {
        return (p, false);
    };
    let Ok(cipher) = aes::Aes::new(key) else {
        return (p, false);
    };
    cipher.decrypt_block(&mut block);
    if &block[9..12] != b"adb" {
        return (p, true);
    }
    let sealed = i32::from_le_bytes([block[0], block[1], block[2], block[3]]);
    if (sealed ^ p) & PERMISSION_BITS == 0 {
        (p, false)
    } else if owner {
        (p, true)
    } else {
        (p & sealed, true)
    }
}

fn string(o: Option<Object>) -> Vec<u8> {
    match o {
        Some(Object::String(s)) => s,
        _ => Vec::new(),
    }
}

/// Mot de passe des révisions ≤ 4 : octets PDFDoc, au plus 32 (§7.6.4.3.2).
/// On accepte de l'UTF-8 en entrée et on rabat les caractères hors Latin-1 sur `?`.
fn legacy_password(password: &[u8]) -> Vec<u8> {
    match std::str::from_utf8(password) {
        Ok(s) => s
            .chars()
            .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
            .take(32)
            .collect(),
        Err(_) => password.iter().copied().take(32).collect(),
    }
}

fn padded(pwd: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let n = pwd.len().min(32);
    out[..n].copy_from_slice(&pwd[..n]);
    out[n..].copy_from_slice(&PAD[..32 - n]);
    out
}

/// Algorithme 2 : clé de chiffrement à partir du mot de passe utilisateur.
#[must_use]
#[allow(clippy::too_many_arguments)] // entrées de l'algorithme 2 de la norme
pub fn compute_key_legacy(
    revision: u32,
    key_len: usize,
    user_pwd: &[u8],
    o: &[u8],
    p: i32,
    id0: &[u8],
    encrypt_metadata: bool,
) -> Vec<u8> {
    let mut h = md5::Md5::new();
    h.update(&padded(user_pwd));
    h.update(&o[..o.len().min(32)]);
    h.update(&p.to_le_bytes());
    h.update(id0);
    if revision >= 4 && !encrypt_metadata {
        h.update(&[0xFF, 0xFF, 0xFF, 0xFF]);
    }
    let mut key = h.finish().to_vec();
    let n = if revision == 2 { 5 } else { key_len.min(16) };
    if revision >= 3 {
        for _ in 0..50 {
            key = md5::md5(&key[..n]).to_vec();
        }
    }
    key.truncate(n);
    key
}

/// Algorithmes 4 et 5 : valeur `/U` attendue pour une clé donnée.
#[must_use]
pub fn compute_u_legacy(revision: u32, key: &[u8], id0: &[u8]) -> Vec<u8> {
    if revision == 2 {
        return rc4::rc4(key, &PAD);
    }
    let mut h = md5::Md5::new();
    h.update(&PAD);
    h.update(id0);
    let mut x = rc4::rc4(key, &h.finish());
    for i in 1..=19u8 {
        let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
        x = rc4::rc4(&k, &x);
    }
    x.extend_from_slice(&[0u8; 16]); // 16 octets arbitraires (§7.6.4.4.5)
    x
}

fn check_user_password_legacy(revision: u32, key: &[u8], u: &[u8], id0: &[u8]) -> bool {
    let expected = compute_u_legacy(revision, key, id0);
    if revision == 2 {
        u.len() >= 32 && u[..32] == expected[..32]
    } else {
        u.len() >= 16 && u[..16] == expected[..16]
    }
}

/// Clé RC4 dérivée du mot de passe propriétaire (algorithme 3, étapes a-d).
fn owner_rc4_key(revision: u32, key_len: usize, owner_pwd: &[u8]) -> Vec<u8> {
    let mut key = md5::md5(&padded(owner_pwd)).to_vec();
    let n = if revision == 2 { 5 } else { key_len.min(16) };
    if revision >= 3 {
        for _ in 0..50 {
            key = md5::md5(&key).to_vec();
        }
    }
    key.truncate(n);
    key
}

/// Algorithme 3 : valeur `/O` à partir des mots de passe propriétaire et utilisateur.
#[must_use]
pub fn compute_o_legacy(
    revision: u32,
    key_len: usize,
    owner_pwd: &[u8],
    user_pwd: &[u8],
) -> Vec<u8> {
    let owner_pwd = if owner_pwd.is_empty() {
        user_pwd
    } else {
        owner_pwd
    };
    let key = owner_rc4_key(revision, key_len, owner_pwd);
    let mut x = rc4::rc4(&key, &padded(user_pwd));
    if revision >= 3 {
        for i in 1..=19u8 {
            let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
            x = rc4::rc4(&k, &x);
        }
    }
    x
}

/// Algorithme 7 : retrouve le mot de passe utilisateur depuis `/O` et le mot de passe propriétaire.
fn owner_to_user_legacy(revision: u32, key_len: usize, owner_pwd: &[u8], o: &[u8]) -> Vec<u8> {
    let key = owner_rc4_key(revision, key_len, owner_pwd);
    let o = &o[..o.len().min(32)];
    if revision == 2 {
        return rc4::rc4(&key, o);
    }
    let mut x = o.to_vec();
    for i in (0..=19u8).rev() {
        let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
        x = rc4::rc4(&k, &x);
    }
    x
}

/// Algorithme 2.B (§7.6.4.3.4) : hachage SHA-256 puis, en révision 6, itérations
/// AES-128 avec SHA-256/384/512 choisi par le résultat modulo 3.
#[must_use]
pub fn hash_2b(revision: u32, password: &[u8], salt: &[u8], udata: &[u8]) -> Vec<u8> {
    let mut input = Vec::with_capacity(password.len() + salt.len() + udata.len());
    input.extend_from_slice(password);
    input.extend_from_slice(salt);
    input.extend_from_slice(udata);
    let mut k = sha2::sha256(&input).to_vec();
    if revision == 5 {
        return k;
    }
    let mut round = 0usize;
    loop {
        let mut k1 = Vec::with_capacity(64 * (password.len() + k.len() + udata.len()));
        for _ in 0..64 {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(udata);
        }
        let Ok(a) = aes::Aes::new(&k[..16]) else {
            return k;
        };
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&k[16..32]);
        let e = a.cbc_encrypt_no_padding(&iv, &k1);
        let m = e[..16].iter().map(|&b| u32::from(b)).sum::<u32>() % 3;
        k = match m {
            0 => sha2::sha256(&e).to_vec(),
            1 => sha2::sha384(&e),
            _ => sha2::sha512(&e),
        };
        round += 1;
        if round >= 64 && usize::from(*e.last().unwrap_or(&0)) <= round - 32 {
            break;
        }
    }
    k.truncate(32);
    k
}

/// Construit les entrées `/U`, `/UE`, `/O`, `/OE` et `/Perms` de la révision 6
/// (algorithmes 8, 9 et 10) pour une clé de fichier donnée. Les sels et l'IV
/// doivent être fournis par l'appelant (aléa).
#[must_use]
#[allow(clippy::too_many_arguments)] // entrées des algorithmes 8, 9 et 10
pub fn build_r6_entries(
    file_key: &[u8; 32],
    user_pwd: &[u8],
    owner_pwd: &[u8],
    salts: &[[u8; 8]; 4],
    p: i32,
    encrypt_metadata: bool,
    perms_random: &[u8; 4],
) -> R6Entries {
    let user_pwd: Vec<u8> = user_pwd.iter().copied().take(127).collect();
    let owner_pwd: Vec<u8> = owner_pwd.iter().copied().take(127).collect();
    // /U
    let mut u = hash_2b(6, &user_pwd, &salts[0], &[]);
    u.extend_from_slice(&salts[0]);
    u.extend_from_slice(&salts[1]);
    let ik = hash_2b(6, &user_pwd, &salts[1], &[]);
    let ue = aes::Aes::new(&ik)
        .map(|a| a.cbc_encrypt_no_padding(&[0u8; 16], file_key))
        .unwrap_or_default();
    // /O
    let mut o = hash_2b(6, &owner_pwd, &salts[2], &u);
    o.extend_from_slice(&salts[2]);
    o.extend_from_slice(&salts[3]);
    let ik = hash_2b(6, &owner_pwd, &salts[3], &u);
    let oe = aes::Aes::new(&ik)
        .map(|a| a.cbc_encrypt_no_padding(&[0u8; 16], file_key))
        .unwrap_or_default();
    // /Perms
    let mut perms = [0u8; 16];
    perms[..4].copy_from_slice(&p.to_le_bytes());
    perms[4..8].copy_from_slice(&[0xFF; 4]);
    perms[8] = if encrypt_metadata { b'T' } else { b'F' };
    perms[9..12].copy_from_slice(b"adb");
    perms[12..].copy_from_slice(perms_random);
    if let Ok(a) = aes::Aes::new(file_key) {
        a.encrypt_block(&mut perms);
    }
    R6Entries {
        u,
        ue,
        o,
        oe,
        perms: perms.to_vec(),
    }
}

/// Entrées calculées pour un dictionnaire `/Encrypt` de révision 6.
#[derive(Debug, Clone)]
pub struct R6Entries {
    /// `/U` (48 octets).
    pub u: Vec<u8>,
    /// `/UE` (32 octets).
    pub ue: Vec<u8>,
    /// `/O` (48 octets).
    pub o: Vec<u8>,
    /// `/OE` (32 octets).
    pub oe: Vec<u8>,
    /// `/Perms` (16 octets).
    pub perms: Vec<u8>,
}

#[cfg(test)]
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::unnecessary_wraps,
    clippy::format_collect
)]
pub(crate) mod tests {
    use super::*;

    fn direct(o: &Object) -> Object {
        o.clone()
    }

    /// Dictionnaire /Encrypt de révision 2, 3 ou 4 (RC4 ou AESV2).
    pub(crate) fn legacy_encrypt_dict(
        revision: u32,
        key_bits: usize,
        user: &[u8],
        owner: &[u8],
        id0: &[u8],
        aes: bool,
    ) -> Dict {
        let key_len = key_bits / 8;
        let p: i32 = -1;
        let o = compute_o_legacy(revision, key_len, owner, user);
        let key = compute_key_legacy(revision, key_len, user, &o, p, id0, true);
        let u = compute_u_legacy(revision, &key, id0);
        let mut d = Dict::new();
        d.insert(Name::new("Filter"), Object::Name(Name::new("Standard")));
        d.insert(Name::new("R"), Object::Integer(i64::from(revision)));
        d.insert(Name::new("O"), Object::String(o));
        d.insert(Name::new("U"), Object::String(u));
        d.insert(Name::new("P"), Object::Integer(i64::from(p)));
        d.insert(Name::new("Length"), Object::Integer(key_bits as i64));
        if revision == 4 {
            d.insert(Name::new("V"), Object::Integer(4));
            let mut cf = Dict::new();
            let mut std = Dict::new();
            std.insert(
                Name::new("CFM"),
                Object::Name(Name::new(if aes { "AESV2" } else { "V2" })),
            );
            std.insert(Name::new("Length"), Object::Integer(key_len as i64));
            cf.insert(Name::new("StdCF"), Object::Dict(std));
            d.insert(Name::new("CF"), Object::Dict(cf));
            d.insert(Name::new("StmF"), Object::Name(Name::new("StdCF")));
            d.insert(Name::new("StrF"), Object::Name(Name::new("StdCF")));
        } else {
            d.insert(
                Name::new("V"),
                Object::Integer(if revision == 2 { 1 } else { 2 }),
            );
        }
        d
    }

    /// Dictionnaire /Encrypt de révision 6 (AES-256).
    pub(crate) fn r6_encrypt_dict(file_key: &[u8; 32], user: &[u8], owner: &[u8]) -> Dict {
        let salts = [[1u8; 8], [2u8; 8], [3u8; 8], [4u8; 8]];
        let e = build_r6_entries(file_key, user, owner, &salts, -1, true, &[9, 9, 9, 9]);
        let mut d = Dict::new();
        d.insert(Name::new("Filter"), Object::Name(Name::new("Standard")));
        d.insert(Name::new("V"), Object::Integer(5));
        d.insert(Name::new("R"), Object::Integer(6));
        d.insert(Name::new("Length"), Object::Integer(256));
        d.insert(Name::new("P"), Object::Integer(-1));
        d.insert(Name::new("O"), Object::String(e.o));
        d.insert(Name::new("U"), Object::String(e.u));
        d.insert(Name::new("OE"), Object::String(e.oe));
        d.insert(Name::new("UE"), Object::String(e.ue));
        d.insert(Name::new("Perms"), Object::String(e.perms));
        let mut cf = Dict::new();
        let mut std = Dict::new();
        std.insert(Name::new("CFM"), Object::Name(Name::new("AESV3")));
        std.insert(Name::new("Length"), Object::Integer(32));
        cf.insert(Name::new("StdCF"), Object::Dict(std));
        d.insert(Name::new("CF"), Object::Dict(cf));
        d.insert(Name::new("StmF"), Object::Name(Name::new("StdCF")));
        d.insert(Name::new("StrF"), Object::Name(Name::new("StdCF")));
        d
    }

    const ID0: &[u8] = b"0123456789abcdef";
    const R: ObjectRef = ObjectRef {
        number: 7,
        generation: 0,
    };

    #[test]
    fn rc4_40_bits_revision_2_empty_user_password() {
        let d = legacy_encrypt_dict(2, 40, b"", b"owner", ID0, false);
        let h = SecurityHandler::new(&d, ID0, b"", &direct).unwrap();
        assert!(!h.is_owner());
        assert_eq!(h.file_key().len(), 5);
        let c = h.encrypt_string(b"secret", R, &[0; 16]);
        assert_ne!(c, b"secret");
        assert_eq!(h.decrypt_string(&c, R), b"secret");
        // Mot de passe propriétaire.
        let h2 = SecurityHandler::new(&d, ID0, b"owner", &direct).unwrap();
        assert!(h2.is_owner());
        assert_eq!(h2.file_key(), h.file_key());
        // Mauvais mot de passe.
        assert_eq!(
            SecurityHandler::new(&d, ID0, b"nope", &direct).err(),
            Some(Error::Encrypted)
        );
    }

    #[test]
    fn rc4_128_bits_revision_3_with_user_password() {
        let d = legacy_encrypt_dict(3, 128, b"user", b"owner", ID0, false);
        assert_eq!(
            SecurityHandler::new(&d, ID0, b"", &direct).err(),
            Some(Error::Encrypted)
        );
        let h = SecurityHandler::new(&d, ID0, b"user", &direct).unwrap();
        assert_eq!(h.file_key().len(), 16);
        let h2 = SecurityHandler::new(&d, ID0, b"owner", &direct).unwrap();
        assert!(h2.is_owner());
        assert_eq!(h2.file_key(), h.file_key());
        let c = h.encrypt_stream(b"stream data", R, &[0; 16]);
        assert_eq!(h.decrypt_stream(&c, R), b"stream data");
    }

    #[test]
    fn aesv2_revision_4() {
        let d = legacy_encrypt_dict(4, 128, b"", b"o", ID0, true);
        let h = SecurityHandler::new(&d, ID0, b"", &direct).unwrap();
        assert_eq!(h.streams, Method::AesV2);
        let iv = [5u8; 16];
        let c = h.encrypt_string(b"hello aes", R, &iv);
        assert_eq!(c.len(), 32);
        assert_eq!(h.decrypt_string(&c, R), b"hello aes");
        // La clé d'objet dépend du numéro : un autre objet ne déchiffre pas.
        let other = ObjectRef {
            number: 8,
            generation: 0,
        };
        assert_ne!(h.decrypt_string(&c, other), b"hello aes");
    }

    #[test]
    fn aesv3_revision_6() {
        let key = [0x42u8; 32];
        let d = r6_encrypt_dict(&key, b"utilisateur", b"proprio");
        assert_eq!(
            SecurityHandler::new(&d, ID0, b"", &direct).err(),
            Some(Error::Encrypted)
        );
        let h = SecurityHandler::new(&d, ID0, b"utilisateur", &direct).unwrap();
        assert_eq!(h.file_key(), key);
        assert!(!h.is_owner());
        let h2 = SecurityHandler::new(&d, ID0, b"proprio", &direct).unwrap();
        assert!(h2.is_owner());
        assert_eq!(h2.file_key(), key);
        let c = h.encrypt_stream(b"aes 256 payload", R, &[1; 16]);
        assert_eq!(h.decrypt_stream(&c, R), b"aes 256 payload");
        assert_eq!(h.describe(), "AES-256 (R6)");
        assert!(!h.perms_tampered());
    }

    /// Algorithme 13 : `/P` retouché pour s'accorder la copie, `/Perms`
    /// scellé sans elle. L'utilisateur garde l'interdiction, le propriétaire
    /// voit l'écart sans rien perdre.
    #[test]
    fn perms_tamper_detected() {
        let key = [0x24u8; 32];
        let sealed = -4 & !(1 << 4); // tout sauf la copie (bit 5)
        let salts = [[5u8; 8], [6u8; 8], [7u8; 8], [8u8; 8]];
        let e = build_r6_entries(&key, b"u", b"o", &salts, sealed, true, &[1, 2, 3, 4]);
        let mut d = r6_encrypt_dict(&key, b"u", b"o");
        for (k, v) in [
            ("O", e.o),
            ("U", e.u),
            ("OE", e.oe),
            ("UE", e.ue),
            ("Perms", e.perms),
        ] {
            d.insert(Name::new(k), Object::String(v));
        }
        d.insert(Name::new("P"), Object::Integer(-4));
        let user = SecurityHandler::new(&d, ID0, b"u", &direct).unwrap();
        assert!(user.perms_tampered());
        assert_eq!(user.permissions() & (1 << 4), 0, "la copie reste interdite");
        let owner = SecurityHandler::new(&d, ID0, b"o", &direct).unwrap();
        assert!(owner.perms_tampered() && owner.is_owner());
        // Un /Perms illisible est signalé, sans empêcher l'ouverture.
        d.insert(Name::new("Perms"), Object::String(vec![0; 16]));
        let h = SecurityHandler::new(&d, ID0, b"u", &direct).unwrap();
        assert!(h.perms_tampered());
        // Absent : rien à vérifier.
        d.remove(&Name::new("Perms"));
        let h = SecurityHandler::new(&d, ID0, b"u", &direct).unwrap();
        assert!(!h.perms_tampered());
    }

    #[test]
    fn decrypt_object_recurses_and_skips_xref_streams() {
        let d = legacy_encrypt_dict(3, 128, b"", b"", ID0, false);
        let h = SecurityHandler::new(&d, ID0, b"", &direct).unwrap();
        let s = h.encrypt_string(b"x", R, &[0; 16]);
        let obj = Object::Array(vec![Object::String(s), Object::Integer(1)]);
        assert_eq!(
            h.decrypt_object(obj, R),
            Object::Array(vec![Object::String(b"x".to_vec()), Object::Integer(1)])
        );
        let mut xd = Dict::new();
        xd.insert(Name::new("Type"), Object::Name(Name::new("XRef")));
        let raw = b"not encrypted".to_vec();
        match h.decrypt_object(
            Object::Stream {
                dict: xd,
                raw: raw.clone(),
            },
            R,
        ) {
            Object::Stream { raw: r, .. } => assert_eq!(r, raw),
            _ => panic!(),
        }
    }

    #[test]
    fn unsupported_handler() {
        let mut d = Dict::new();
        d.insert(Name::new("Filter"), Object::Name(Name::new("Adobe.PubSec")));
        assert!(matches!(
            SecurityHandler::new(&d, ID0, b"", &direct),
            Err(Error::Unsupported(_))
        ));
    }
}
