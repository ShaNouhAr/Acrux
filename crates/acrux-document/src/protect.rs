//! Protection par mot de passe (écriture) : pose ou retire le chiffrement
//! standard d'un document. Le chiffrement produit est toujours AES-256 en
//! révision 6 (ISO 32000-2 §7.6.4.4.9 à 7.6.4.4.12), le plus sûr défini par
//! la norme ; RC4 et AES-128 ne sont conservés qu'en lecture.
//!
//! Principe : tous les objets sont d'abord matérialisés en clair dans les
//! modifications en attente (`edits`), puis le gestionnaire de sécurité est
//! remplacé. Le chargeur ne relit donc jamais le fichier avec la mauvaise
//! clé, et l'enregistrement (`save_full`) chiffre chaque objet avec la
//! nouvelle.
//!
//! Deux mots de passe, comme dans Acrobat : celui **d'ouverture** (dit
//! « utilisateur » par la norme), qui donne accès au document dans la limite
//! des permissions, et celui **des permissions** (« propriétaire »), qui
//! donne tous les droits — dont celui de changer ou de retirer la
//! protection. Un utilisateur qui pourrait retirer la protection rendrait
//! les permissions décoratives : c'est donc le propriétaire seul qui le peut.
//!
//! Tout l'aléa (clé de fichier, sels, `/Perms`, `/ID`, IV) vient d'un
//! générateur cryptographique ([`SystemRandom`]) ; les tests en injectent un
//! reproductible.

use acrux_core::{Error, Result};

use crate::crypt::random::{Random, SystemRandom};
use crate::crypt::{build_r6_entries, SecurityHandler};
use crate::document::Document;
use crate::objects::{Dict, Name, Object, ObjectRef};

/// Permissions utilisateur (§7.6.4.2, table 22). Chaque champ vrai autorise
/// l'action quand le document est ouvert avec le mot de passe utilisateur ;
/// le mot de passe propriétaire autorise tout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // reflète la table 22 : un bit par action
pub struct Permissions {
    /// Imprimer (bit 3).
    pub print: bool,
    /// Modifier le contenu (bit 4).
    pub modify: bool,
    /// Copier ou extraire du texte et des images (bit 5).
    pub copy: bool,
    /// Ajouter ou modifier des annotations et remplir des formulaires (bit 6).
    pub annotate: bool,
    /// Remplir des formulaires même sans le bit 6 (bit 9).
    pub fill_forms: bool,
    /// Extraire pour l'accessibilité (bit 10, obsolète mais toujours lu).
    pub accessibility: bool,
    /// Assembler : insérer, pivoter, supprimer des pages, signets (bit 11).
    pub assemble: bool,
    /// Imprimer en haute qualité (bit 12).
    pub print_high_quality: bool,
}

/// Niveau d'impression permis : les bits 3 et 12 lus ensemble.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintLevel {
    /// Pas d'impression.
    None,
    /// Impression dégradée : le lecteur imprime une image de résolution
    /// modeste, qui ne se prête pas à une copie fidèle.
    Low,
    /// Impression fidèle.
    High,
}

impl Default for Permissions {
    /// Tout autorisé.
    fn default() -> Self {
        Self::all()
    }
}

impl Permissions {
    /// Toutes les actions autorisées.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            print: true,
            modify: true,
            copy: true,
            annotate: true,
            fill_forms: true,
            accessibility: true,
            assemble: true,
            print_high_quality: true,
        }
    }

    /// Valeur `/P` : bits 1-2 et 7-8 à zéro, bits 13-32 à un, comme l'exige
    /// la norme ; les bits réservés sont ignorés par les lecteurs.
    #[must_use]
    pub fn to_p(self) -> i32 {
        let mut p: u32 = 0xFFFF_F0C0;
        let mut set = |on: bool, bit: u32| {
            if on {
                p |= 1 << (bit - 1);
            }
        };
        set(self.print, 3);
        set(self.modify, 4);
        set(self.copy, 5);
        set(self.annotate, 6);
        set(self.fill_forms, 9);
        set(self.accessibility, 10);
        set(self.assemble, 11);
        set(self.print_high_quality, 12);
        // Conversion bit à bit voulue : /P est un entier signé 32 bits.
        i32::from_le_bytes(p.to_le_bytes())
    }

    /// Lecture d'une valeur `/P`.
    #[must_use]
    pub fn from_p(p: i32) -> Self {
        let bits = u32::from_le_bytes(p.to_le_bytes());
        let on = |bit: u32| bits & (1 << (bit - 1)) != 0;
        Self {
            print: on(3),
            modify: on(4),
            copy: on(5),
            annotate: on(6),
            fill_forms: on(9),
            accessibility: on(10),
            assemble: on(11),
            print_high_quality: on(12),
        }
    }

    /// Niveau d'impression permis. Le bit 12 seul ne permet rien : sans le
    /// bit 3, on n'imprime pas du tout (§7.6.4.2, table 22).
    #[must_use]
    pub const fn print_level(self) -> PrintLevel {
        match (self.print, self.print_high_quality) {
            (false, _) => PrintLevel::None,
            (true, false) => PrintLevel::Low,
            (true, true) => PrintLevel::High,
        }
    }

    /// Règle le niveau d'impression.
    pub fn set_print(&mut self, level: PrintLevel) {
        self.print = level != PrintLevel::None;
        self.print_high_quality = level == PrintLevel::High;
    }

    /// Permissions rendues cohérentes, comme les lit un lecteur : commenter
    /// comprend remplir les formulaires (bit 6), qui peut copier peut
    /// extraire pour l'accessibilité (bit 10), et la haute qualité ne vaut
    /// rien sans l'impression (bit 12 sans bit 3). Écrire la forme
    /// normalisée, c'est écrire un `/P` que tous les lecteurs lisent de la
    /// même façon.
    #[must_use]
    pub const fn normalized(self) -> Self {
        Self {
            fill_forms: self.fill_forms || self.annotate,
            accessibility: self.accessibility || self.copy,
            print_high_quality: self.print_high_quality && self.print,
            ..self
        }
    }

    /// Vrai si rien n'est interdit.
    #[must_use]
    pub fn is_all(self) -> bool {
        self.normalized() == Self::all()
    }

    /// Ce qui est interdit, en toutes lettres (en français : l'application
    /// les traduit), dans l'ordre du dialogue de protection.
    #[must_use]
    pub fn restrictions(self) -> Vec<&'static str> {
        let p = self.normalized();
        let mut out = Vec::new();
        match p.print_level() {
            PrintLevel::None => out.push("impression interdite"),
            PrintLevel::Low => out.push("impression en basse résolution seulement"),
            PrintLevel::High => {}
        }
        for (allowed, label) in [
            (p.modify, "modification interdite"),
            (p.copy, "copie interdite"),
            (p.annotate, "commentaires interdits"),
            (p.fill_forms, "remplissage des formulaires interdit"),
            (p.accessibility, "extraction pour l'accessibilité interdite"),
            (p.assemble, "assemblage interdit"),
        ] {
            if !allowed {
                out.push(label);
            }
        }
        out
    }
}

/// Force estimée d'un mot de passe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Strength {
    /// Rien de saisi.
    Empty,
    /// Se devine en quelques essais ou quelques heures.
    Weak,
    /// Résiste à un essai naïf, pas à une attaque déterminée.
    Fair,
    /// Correct.
    Good,
    /// Hors de portée d'une recherche exhaustive.
    Strong,
}

impl Strength {
    /// Rang de 0 (vide) à 4 (fort), pour une jauge.
    #[must_use]
    pub const fn level(self) -> u8 {
        match self {
            Strength::Empty => 0,
            Strength::Weak => 1,
            Strength::Fair => 2,
            Strength::Good => 3,
            Strength::Strong => 4,
        }
    }

    /// Libellé (en français : l'application le traduit).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Strength::Empty => "",
            Strength::Weak => "Faible",
            Strength::Fair => "Moyen",
            Strength::Good => "Bon",
            Strength::Strong => "Fort",
        }
    }
}

/// Mots de passe que tout attaquant essaie en premier. Comparés sans casse
/// et sans les chiffres ou la ponctuation de la fin (« Soleil2024! » est
/// « soleil » suivi d'une année).
const COMMON: &[&str] = &[
    "000000",
    "111111",
    "123123",
    "123456",
    "12345678",
    "123456789",
    "acrux",
    "admin",
    "administrateur",
    "azerty",
    "azertyuiop",
    "bonjour",
    "chouchou",
    "doudou",
    "dragon",
    "football",
    "iloveyou",
    "letmein",
    "loulou",
    "marseille",
    "master",
    "monkey",
    "motdepasse",
    "p@ssw0rd",
    "passw0rd",
    "password",
    "princess",
    "qwerty",
    "qwertyuiop",
    "secret",
    "soleil",
    "sunshine",
    "welcome",
];

/// Vrai si le mot de passe est un mot courant, éventuellement suivi de
/// chiffres ou de ponctuation.
fn is_common(pw: &str) -> bool {
    let lower = pw.to_lowercase();
    let base = lower.trim_end_matches(|c: char| c.is_ascii_digit() || c.is_ascii_punctuation());
    COMMON.contains(&lower.as_str()) || COMMON.contains(&base)
}

/// Estime la force d'un mot de passe.
///
/// L'estimation est celle d'une recherche exhaustive : nombre de caractères
/// fois log₂ de la taille de l'alphabet employé (minuscules 26, majuscules
/// 26, chiffres 10, ponctuation ASCII 33, tout le reste 100). Une répétition
/// ou une suite (« aa », « ab », « 98 ») ne compte que pour un demi
/// caractère : c'est ce qu'on essaie d'abord. Sont faibles d'office : moins
/// de 8 caractères, moins de 3 caractères différents, et les mots de passe
/// courants. Seuils : 36, 50 et 70 bits.
#[must_use]
pub fn password_strength(pw: &str) -> Strength {
    if pw.is_empty() {
        return Strength::Empty;
    }
    let chars: Vec<char> = pw.chars().collect();
    let mut distinct = chars.clone();
    distinct.sort_unstable();
    distinct.dedup();
    if chars.len() < 8 || distinct.len() < 3 || is_common(pw) {
        return Strength::Weak;
    }
    let has = |f: fn(&char) -> bool| chars.iter().any(f);
    let mut alphabet = 0u32;
    for (present, size) in [
        (has(char::is_ascii_lowercase), 26),
        (has(char::is_ascii_uppercase), 26),
        (has(char::is_ascii_digit), 10),
        (has(|c| c.is_ascii() && !c.is_ascii_alphanumeric()), 33),
        (has(|c| !c.is_ascii()), 100),
    ] {
        if present {
            alphabet += size;
        }
    }
    let mut effective = 1.0;
    for pair in chars.windows(2) {
        let (a, b) = (u32::from(pair[0]), u32::from(pair[1]));
        effective += if a.abs_diff(b) <= 1 { 0.5 } else { 1.0 };
    }
    let bits = effective * f64::from(alphabet.max(2)).log2();
    if bits < 36.0 {
        Strength::Weak
    } else if bits < 50.0 {
        Strength::Fair
    } else if bits < 70.0 {
        Strength::Good
    } else {
        Strength::Strong
    }
}

/// Mises en garde sur un mot de passe (en français : l'application les
/// traduit). Elles n'empêchent rien.
#[must_use]
pub fn password_warnings(pw: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    // La révision 6 ne garde que les 127 premiers octets UTF-8 : le reste
    // est ignoré sans bruit (voir `build_r6_entries`).
    if pw.len() > 127 {
        out.push("au-delà de 127 octets, la fin du mot de passe est ignorée");
    }
    // La norme demande de préparer le mot de passe par SASLprep (RFC 4013),
    // qui normalise les caractères accentués ; nous prenons l'UTF-8 tel
    // quel. Un autre lecteur pourrait donc refuser un accent saisi
    // autrement.
    if !pw.is_ascii() {
        out.push("un mot de passe accentué risque d'être refusé par d'autres lecteurs PDF");
    }
    out
}

impl Document {
    /// Charge tous les objets du fichier dans les modifications en attente,
    /// déchiffrés avec le gestionnaire courant. Après cet appel, plus aucune
    /// lecture ne touche le fichier d'origine.
    fn materialize_all(&self) -> Result<()> {
        let skip = self.encrypt_ref();
        for n in self.object_numbers() {
            let r = ObjectRef {
                number: n,
                generation: 0,
            };
            if Some(r) == skip || self.edits.borrow().contains_key(&n) {
                continue;
            }
            let obj = self.get(r)?;
            self.edits.borrow_mut().insert(n, Some((*obj).clone()));
        }
        Ok(())
    }

    /// Vrai si changer ou retirer la protection est permis : document en
    /// clair, ou ouvert avec le mot de passe des permissions.
    #[must_use]
    pub fn can_change_protection(&self) -> bool {
        !self.is_encrypted() || self.security().is_some_and(|h| h.is_owner())
    }

    /// Protège le document : mot de passe utilisateur (demandé à l'ouverture,
    /// vide = ouverture libre avec les permissions), mot de passe propriétaire
    /// (tout autorisé) et permissions. Un document déjà chiffré est rechiffré
    /// avec la nouvelle clé, s'il a été ouvert avec le mot de passe des
    /// permissions. Enregistrer ensuite avec [`Document::save_full`].
    ///
    /// # Errors
    /// Document chiffré dont le mot de passe n'a pas été fourni, ouvert sans
    /// le mot de passe des permissions, ou objet illisible.
    pub fn protect(&self, user: &[u8], owner: &[u8], permissions: Permissions) -> Result<()> {
        self.protect_with(user, owner, permissions, &mut SystemRandom::new())
    }

    /// [`Document::protect`] avec un aléa fourni : clé de fichier, sels,
    /// `/Perms` et `/ID` en sortent. Un aléa reproductible ne sert qu'aux
    /// tests — un document protégé ainsi a une clé que l'on peut refaire.
    ///
    /// # Errors
    /// Voir [`Document::protect`].
    pub fn protect_with(
        &self,
        user: &[u8],
        owner: &[u8],
        permissions: Permissions,
        rng: &mut dyn Random,
    ) -> Result<()> {
        if self.needs_password() {
            return Err(Error::Encrypted);
        }
        if !self.can_change_protection() {
            return Err(Error::Unsupported(
                "seul le mot de passe des permissions permet de changer la protection".into(),
            ));
        }
        self.materialize_all()?;
        if let Some(old) = self.encrypt_ref() {
            self.delete(old);
        }
        let mut file_key = [0u8; 32];
        rng.fill(&mut file_key);
        let mut salts = [[0u8; 8]; 4];
        for s in &mut salts {
            rng.fill(s);
        }
        let mut perms_random = [0u8; 4];
        rng.fill(&mut perms_random);
        // Le mot de passe propriétaire vide vaut le mot de passe utilisateur (§7.6.4.4.9).
        let owner = if owner.is_empty() { user } else { owner };
        let p = permissions.normalized().to_p();
        let e = build_r6_entries(&file_key, user, owner, &salts, p, true, &perms_random);
        file_key.fill(0);
        let mut d = Dict::new();
        d.insert(Name::new("Filter"), Object::Name(Name::new("Standard")));
        d.insert(Name::new("V"), Object::Integer(5));
        d.insert(Name::new("R"), Object::Integer(6));
        d.insert(Name::new("Length"), Object::Integer(256));
        d.insert(Name::new("P"), Object::Integer(i64::from(p)));
        d.insert(Name::new("O"), Object::String(e.o));
        d.insert(Name::new("U"), Object::String(e.u));
        d.insert(Name::new("OE"), Object::String(e.oe));
        d.insert(Name::new("UE"), Object::String(e.ue));
        d.insert(Name::new("Perms"), Object::String(e.perms));
        let mut cf = Dict::new();
        let mut std = Dict::new();
        std.insert(Name::new("CFM"), Object::Name(Name::new("AESV3")));
        std.insert(Name::new("AuthEvent"), Object::Name(Name::new("DocOpen")));
        std.insert(Name::new("Length"), Object::Integer(32));
        cf.insert(Name::new("StdCF"), Object::Dict(std));
        d.insert(Name::new("CF"), Object::Dict(cf));
        d.insert(Name::new("StmF"), Object::Name(Name::new("StdCF")));
        d.insert(Name::new("StrF"), Object::Name(Name::new("StdCF")));
        // Identifiant de fichier : obligatoire avec /Encrypt.
        let id0 = match self.trailer().get(&Name::new("ID")) {
            Some(Object::Array(ids)) if ids.len() == 2 => match &ids[0] {
                Object::String(s) if !s.is_empty() => s.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        let id0 = if id0.is_empty() {
            let mut id = vec![0u8; 16];
            rng.fill(&mut id);
            id
        } else {
            id0
        };
        let mut id1 = vec![0u8; 16];
        rng.fill(&mut id1);
        self.set_trailer_entry(
            "ID",
            Object::Array(vec![Object::String(id0.clone()), Object::String(id1)]),
        );
        let handler = SecurityHandler::new(&d, &id0, owner, &|o: &Object| o.clone())?;
        let r = self.add(Object::Dict(d));
        self.set_trailer_entry("Encrypt", Object::Reference(r));
        self.encrypt_ref.set(Some(r));
        *self.security.borrow_mut() = Some(handler);
        // AES-256 R6 exige PDF 2.0 (ou l'extension Adobe 1.7 niveau 8) :
        // on annonce 2.0 dans le catalogue via /Version.
        self.bump_version((2, 0));
        Ok(())
    }

    /// Retire le chiffrement. Nécessite le mot de passe propriétaire : les
    /// permissions ne valent que si l'utilisateur ne peut pas les retirer,
    /// même quand elles l'autorisent à modifier le contenu.
    /// Enregistrer ensuite avec [`Document::save_full`].
    ///
    /// # Errors
    /// Mot de passe manquant, document ouvert sans le mot de passe des
    /// permissions, ou objet illisible.
    pub fn unprotect(&self) -> Result<()> {
        if self.needs_password() {
            return Err(Error::Encrypted);
        }
        let Some(old) = self.encrypt_ref() else {
            return Ok(());
        };
        if !self.can_change_protection() {
            return Err(Error::Unsupported(
                "seul le mot de passe des permissions permet de retirer la protection".into(),
            ));
        }
        self.materialize_all()?;
        self.delete(old);
        self.set_trailer_entry("Encrypt", Object::Null);
        self.encrypt_ref.set(None);
        *self.security.borrow_mut() = None;
        Ok(())
    }

    /// Relève la version annoncée dans le catalogue si elle est inférieure.
    fn bump_version(&self, min: (u8, u8)) {
        if self.version() >= min {
            return;
        }
        let Ok(cat) = self.catalog() else { return };
        let Some(r) = crate::xref::trailer_ref(&self.trailer(), "Root") else {
            return;
        };
        let mut cat = cat;
        cat.insert(
            Name::new("Version"),
            Object::Name(Name::new(&format!("{}.{}", min.0, min.1))),
        );
        self.set(r, Object::Dict(cat));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)] // tests
mod tests {
    use super::*;
    use crate::crypt::random::ChaCha20Rng;

    fn plain_pdf() -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R >>",
            "<< /Length 44 >>\nstream\nBT /F1 24 Tf 20 40 Td (Bonjour secret) Tj ET\nendstream",
            "<< /Title (Titre en clair) >>",
        ];
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes(),
        );
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R /Info 5 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objs.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    /// Contenu de la page 1, déchiffré.
    fn page_content(doc: &Document) -> Vec<u8> {
        let page = crate::pages::collect_pages(doc).unwrap().remove(0);
        let Some(Object::Reference(c)) = page.dict.get(&Name::new("Contents")) else {
            panic!("contenu attendu");
        };
        doc.stream_data(&doc.get(*c).unwrap()).unwrap().data
    }

    /// Dictionnaire /Encrypt d'un fichier enregistré.
    fn encrypt_dict(saved: &[u8]) -> Dict {
        let doc = Document::from_bytes(saved.to_vec()).unwrap();
        let r = doc.encrypt_ref().unwrap();
        doc.get(r).unwrap().as_dict().unwrap().clone()
    }

    fn string_of(d: &Dict, key: &str) -> Vec<u8> {
        match d.get(&Name::new(key)) {
            Some(Object::String(s)) => s.clone(),
            other => panic!("/{key} : {other:?}"),
        }
    }

    fn second_id(saved: &[u8]) -> Vec<u8> {
        let doc = Document::from_bytes(saved.to_vec()).unwrap();
        match doc.trailer().get(&Name::new("ID")) {
            Some(Object::Array(ids)) => match ids.get(1) {
                Some(Object::String(s)) => s.clone(),
                other => panic!("/ID[1] : {other:?}"),
            },
            other => panic!("/ID : {other:?}"),
        }
    }

    #[test]
    fn permissions_bits_roundtrip() {
        let all = Permissions::all();
        // Bits 1-2 à zéro, tout le reste à un.
        assert_eq!(all.to_p(), -4);
        let mut p = all;
        p.print = false;
        p.copy = false;
        assert_eq!(Permissions::from_p(p.to_p()), p);
        assert_eq!(Permissions::from_p(-1), Permissions::all());
    }

    /// Les trois niveaux d'impression et les 64 combinaisons des six cases
    /// passent par `/P` sans rien perdre, une fois rendues cohérentes.
    #[test]
    fn permissions_roundtrip_all_combinations() {
        for level in [PrintLevel::None, PrintLevel::Low, PrintLevel::High] {
            for mask in 0u8..64 {
                let bit = |i: u8| mask & (1 << i) != 0;
                let mut p = Permissions {
                    print: false,
                    modify: bit(0),
                    copy: bit(1),
                    annotate: bit(2),
                    fill_forms: bit(3),
                    accessibility: bit(4),
                    assemble: bit(5),
                    print_high_quality: false,
                };
                p.set_print(level);
                let n = p.normalized();
                assert_eq!(Permissions::from_p(n.to_p()), n, "{level:?} {mask:06b}");
                assert_eq!(n.print_level(), level);
                assert_eq!(n.normalized(), n, "normalisation idempotente");
            }
        }
    }

    #[test]
    fn normalized_links() {
        let mut p = Permissions::all();
        p.fill_forms = false;
        assert!(p.normalized().fill_forms, "commenter comprend remplir");
        p.annotate = false;
        assert!(!p.normalized().fill_forms);
        let mut p = Permissions::all();
        p.accessibility = false;
        assert!(
            p.normalized().accessibility,
            "copier comprend l'accessibilité"
        );
        p.copy = false;
        assert!(!p.normalized().accessibility);
        let mut p = Permissions::all();
        p.print = false;
        let n = p.normalized();
        assert!(
            !n.print && !n.print_high_quality,
            "haute qualité sans impression"
        );
        assert_eq!(p.print_level(), PrintLevel::None);
        assert!(!p.is_all());
        assert!(Permissions::all().is_all());
        assert!(Permissions::all().restrictions().is_empty());
        let mut p = Permissions::all();
        p.set_print(PrintLevel::Low);
        p.copy = false;
        p.accessibility = false;
        assert_eq!(
            p.restrictions(),
            vec![
                "impression en basse résolution seulement",
                "copie interdite",
                "extraction pour l'accessibilité interdite"
            ]
        );
    }

    #[test]
    fn protect_then_open_with_user_and_owner_passwords() {
        let doc = Document::from_bytes(plain_pdf()).unwrap();
        let mut perms = Permissions::all();
        perms.copy = false;
        doc.protect(b"utilisateur", b"chef", perms).unwrap();
        assert!(doc.is_encrypted());
        let saved = doc.save_full().unwrap();
        assert!(!saved.windows(6).any(|w| w == b"secret"));
        assert!(!saved.windows(8).any(|w| w == b"en clair"));
        assert!(saved.windows(6).any(|w| w == b"/AESV3"));

        // Sans mot de passe : structure lisible, contenus verrouillés.
        let locked = Document::from_bytes(saved.clone()).unwrap();
        assert!(locked.needs_password());
        assert!(locked.authenticate(b"faux").is_err());
        assert!(locked.needs_password());

        // Mot de passe utilisateur : permissions limitées.
        locked.authenticate(b"utilisateur").unwrap();
        assert!(!locked.needs_password());
        let h = locked.security().unwrap();
        assert!(!h.is_owner());
        assert!(!h.perms_tampered());
        assert!(!Permissions::from_p(h.permissions()).copy);
        assert_eq!(
            locked.info().unwrap().get(&Name::new("Title")),
            Some(&Object::String(b"Titre en clair".to_vec()))
        );
        assert!(page_content(&locked).windows(6).any(|w| w == b"secret"));

        // Mot de passe propriétaire.
        let owner = Document::from_bytes(saved).unwrap();
        owner.authenticate(b"chef").unwrap();
        assert!(owner.security().unwrap().is_owner());
        assert_eq!(owner.version(), (2, 0));
    }

    /// Deux protections du même fichier, avec les mêmes mots de passe, ne
    /// partagent rien : ni clé, ni sels, ni `/Perms`, ni identifiant.
    #[test]
    fn two_protections_give_different_keys() {
        let (a, b) = (
            Document::from_bytes(plain_pdf()).unwrap(),
            Document::from_bytes(plain_pdf()).unwrap(),
        );
        a.protect(b"u", b"o", Permissions::all()).unwrap();
        b.protect(b"u", b"o", Permissions::all()).unwrap();
        let (ka, kb) = (a.security().unwrap(), b.security().unwrap());
        assert_ne!(ka.file_key(), kb.file_key());
        assert_eq!(ka.file_key().len(), 32);
        let (sa, sb) = (a.save_full().unwrap(), b.save_full().unwrap());
        let (da, db) = (encrypt_dict(&sa), encrypt_dict(&sb));
        for key in ["U", "O", "UE", "OE", "Perms"] {
            assert_ne!(string_of(&da, key), string_of(&db, key), "/{key}");
        }
        assert_ne!(second_id(&sa), second_id(&sb));
        // Au-delà de l'en-tête, les deux fichiers divergent.
        let header = sa.iter().position(|&c| c == b'\n').unwrap() + 1;
        assert_ne!(sa[header..], sb[header..]);
        // Et chacun s'ouvre avec les deux mots de passe.
        for saved in [sa, sb] {
            let d = Document::from_bytes(saved).unwrap();
            d.authenticate(b"u").unwrap();
            assert!(page_content(&d).windows(6).any(|w| w == b"secret"));
        }
    }

    /// L'aléa s'injecte : avec la même graine, les mêmes entrées.
    #[test]
    fn protect_with_seeded_is_reproducible() {
        let run = || {
            let doc = Document::from_bytes(plain_pdf()).unwrap();
            let mut rng = ChaCha20Rng::from_seed([42; 32]);
            doc.protect_with(b"u", b"o", Permissions::all(), &mut rng)
                .unwrap();
            let file_key = doc.security().unwrap().file_key().to_vec();
            (encrypt_dict(&doc.save_full().unwrap()), file_key)
        };
        let ((a, ka), (b, kb)) = (run(), run());
        assert_eq!(ka, kb);
        for key in ["U", "O", "UE", "OE", "Perms"] {
            assert_eq!(string_of(&a, key), string_of(&b, key), "/{key}");
        }
    }

    #[test]
    fn open_with_either_password() {
        let doc = Document::from_bytes(plain_pdf()).unwrap();
        let mut perms = Permissions::all();
        perms.set_print(PrintLevel::Low);
        perms.copy = false;
        perms.fill_forms = false;
        perms.annotate = false;
        perms.assemble = false;
        doc.protect(b"lecture", b"chef", perms).unwrap();
        let saved = doc.save_full().unwrap();

        let reader = Document::from_bytes(saved.clone()).unwrap();
        assert!(reader.needs_password());
        reader.authenticate(b"lecture").unwrap();
        let h = reader.security().unwrap();
        assert!(!h.is_owner());
        let read = Permissions::from_p(h.permissions());
        assert_eq!(read, perms.normalized());
        assert_eq!(read.print_level(), PrintLevel::Low);
        assert!(!read.copy && !read.fill_forms && !read.assemble && read.modify);
        assert!(page_content(&reader).windows(6).any(|w| w == b"secret"));
        assert!(!reader.can_change_protection());

        let chef = Document::from_bytes(saved.clone()).unwrap();
        chef.authenticate(b"chef").unwrap();
        assert!(chef.security().unwrap().is_owner());
        assert!(chef.can_change_protection());
        assert!(page_content(&chef).windows(6).any(|w| w == b"secret"));

        let wrong = Document::from_bytes(saved).unwrap();
        assert_eq!(wrong.authenticate(b"x"), Err(Error::Encrypted));
    }

    /// Un utilisateur ne change pas la protection : sans le mot de passe des
    /// permissions, il retirerait les restrictions en reprotégeant.
    #[test]
    fn reprotect_requires_owner() {
        let doc = Document::from_bytes(plain_pdf()).unwrap();
        let mut perms = Permissions::all();
        perms.copy = false;
        doc.protect(b"lecture", b"chef", perms).unwrap();
        let saved = doc.save_full().unwrap();

        let d = Document::from_bytes(saved).unwrap();
        d.authenticate(b"lecture").unwrap();
        assert!(matches!(
            d.protect(b"autre", b"", Permissions::all()),
            Err(Error::Unsupported(_))
        ));
        // Le mot de passe des permissions, saisi après coup, suffit.
        d.authenticate(b"chef").unwrap();
        d.protect(b"nouveau", b"patron", Permissions::all())
            .unwrap();
        let again = Document::from_bytes(d.save_full().unwrap()).unwrap();
        assert!(again.authenticate(b"lecture").is_err());
        again.authenticate(b"nouveau").unwrap();
        assert!(page_content(&again).windows(6).any(|w| w == b"secret"));
    }

    #[test]
    fn unprotect_restores_plain_text() {
        let doc = Document::from_bytes(plain_pdf()).unwrap();
        doc.protect(b"", b"chef", Permissions::all()).unwrap();
        let saved = doc.save_full().unwrap();
        // Mot de passe utilisateur vide : ouverture libre, mais le retrait
        // reste au propriétaire.
        let d2 = Document::from_bytes(saved).unwrap();
        assert!(d2.is_encrypted() && !d2.needs_password());
        assert!(d2.unprotect().is_err());
        d2.authenticate(b"chef").unwrap();
        d2.unprotect().unwrap();
        let plain = d2.save_full().unwrap();
        assert!(plain.windows(6).any(|w| w == b"secret"));
        assert!(!plain.windows(8).any(|w| w == b"/Encrypt"));
        let d3 = Document::from_bytes(plain).unwrap();
        assert!(!d3.is_encrypted());
        assert_eq!(
            d3.info().unwrap().get(&Name::new("Title")),
            Some(&Object::String(b"Titre en clair".to_vec()))
        );
    }

    #[test]
    fn unprotect_refused_without_rights() {
        let doc = Document::from_bytes(plain_pdf()).unwrap();
        let mut perms = Permissions::all();
        perms.modify = false;
        doc.protect(b"", b"chef", perms).unwrap();
        let d2 = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        assert!(d2.unprotect().is_err());
        d2.authenticate(b"chef").unwrap();
        d2.unprotect().unwrap();
    }

    /// Même avec le droit de modifier, l'utilisateur ne retire pas la
    /// protection : l'ancienne règle le permettait.
    #[test]
    fn unprotect_refused_to_user_even_with_modify() {
        let doc = Document::from_bytes(plain_pdf()).unwrap();
        let mut perms = Permissions::all();
        perms.copy = false;
        doc.protect(b"lecture", b"chef", perms).unwrap();
        let d2 = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        d2.authenticate(b"lecture").unwrap();
        assert!(Permissions::from_p(d2.security().unwrap().permissions()).modify);
        assert!(matches!(d2.unprotect(), Err(Error::Unsupported(_))));
    }

    /// Les fichiers des versions précédentes avaient le même mot de passe
    /// pour l'ouverture et les permissions : ils s'ouvrent en propriétaire
    /// (celui-ci est essayé d'abord), et se déprotègent donc toujours.
    #[test]
    fn same_password_opens_as_owner() {
        let doc = Document::from_bytes(plain_pdf()).unwrap();
        doc.protect(b"meme", b"meme", Permissions::all()).unwrap();
        let d2 = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        d2.authenticate(b"meme").unwrap();
        assert!(d2.security().unwrap().is_owner());
        d2.unprotect().unwrap();
    }

    /// Un `/P` retouché pour tout s'accorder ne trompe pas notre lecteur :
    /// `/Perms`, scellé par la clé, dit la vérité.
    #[test]
    fn perms_tamper_detected() {
        let doc = Document::from_bytes(plain_pdf()).unwrap();
        let mut perms = Permissions::all();
        perms.copy = false;
        perms.accessibility = false;
        doc.protect(b"lecture", b"chef", perms).unwrap();
        let mut saved = doc.save_full().unwrap();
        let p = format!("/P {}", perms.to_p());
        let at = saved
            .windows(p.len())
            .position(|w| w == p.as_bytes())
            .unwrap();
        // Même longueur : les décalages de la table xref restent justes.
        let forged = format!("{:<width$}", "/P -4", width = p.len());
        saved[at..at + p.len()].copy_from_slice(forged.as_bytes());

        let user = Document::from_bytes(saved.clone()).unwrap();
        user.authenticate(b"lecture").unwrap();
        let h = user.security().unwrap();
        assert!(h.perms_tampered());
        assert!(!Permissions::from_p(h.permissions()).copy);
        assert!(page_content(&user).windows(6).any(|w| w == b"secret"));

        let owner = Document::from_bytes(saved).unwrap();
        owner.authenticate(b"chef").unwrap();
        let h = owner.security().unwrap();
        assert!(h.perms_tampered());
        assert!(h.is_owner());
    }

    #[test]
    fn opens_without_password() {
        let doc = Document::from_bytes(plain_pdf()).unwrap();
        assert!(doc.opens_without_password());
        doc.protect(b"", b"chef", Permissions::all()).unwrap();
        assert!(doc.opens_without_password());
        let d = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        assert!(d.opens_without_password());
        d.authenticate(b"chef").unwrap();
        d.protect(b"lecture", b"chef", Permissions::all()).unwrap();
        assert!(!d.opens_without_password());
    }

    #[test]
    fn password_strength_levels() {
        assert_eq!(password_strength(""), Strength::Empty);
        for weak in [
            "123456",
            "azerty",
            "aaaaaaaa",
            "abcdefgh",
            "lecture",
            "Soleil2024",
            "Password1234!",
            "12345678901",
        ] {
            assert_eq!(password_strength(weak), Strength::Weak, "{weak}");
        }
        assert_eq!(password_strength("jardinet"), Strength::Fair);
        assert_eq!(password_strength("Printemps42"), Strength::Good);
        assert_eq!(password_strength("c7#Vq!e9Lp2@xZ"), Strength::Strong);
        assert_eq!(password_strength("Chef#2026!xQ"), Strength::Strong);
        assert!(password_strength("Été-à-Noirmoutier") >= Strength::Good);
        assert_eq!(Strength::Strong.level(), 4);
        assert_eq!(Strength::Weak.label(), "Faible");
    }

    #[test]
    fn password_warnings_long_and_non_ascii() {
        assert!(password_warnings("simple").is_empty());
        assert_eq!(password_warnings(&"x".repeat(128)).len(), 1);
        assert!(password_warnings(&"x".repeat(127)).is_empty());
        assert_eq!(password_warnings("été").len(), 1);
        assert_eq!(password_warnings(&"é".repeat(64)).len(), 2);
    }
}
