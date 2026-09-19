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

use acrux_core::{Error, Result};

use crate::crypt::{build_r6_entries, SecurityHandler};
use crate::document::Document;
use crate::edit::Rng;
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

    /// Protège le document : mot de passe utilisateur (demandé à l'ouverture,
    /// vide = ouverture libre avec les permissions), mot de passe propriétaire
    /// (tout autorisé) et permissions. Un document déjà chiffré est rechiffré
    /// avec la nouvelle clé. Enregistrer ensuite avec [`Document::save_full`].
    ///
    /// # Errors
    /// Document chiffré dont le mot de passe n'a pas été fourni, ou objet
    /// illisible.
    pub fn protect(&self, user: &[u8], owner: &[u8], permissions: Permissions) -> Result<()> {
        if self.needs_password() {
            return Err(Error::Encrypted);
        }
        self.materialize_all()?;
        if let Some(old) = self.encrypt_ref() {
            self.delete(old);
        }
        let mut rng = Rng::new(self.bytes().len() as u64 ^ 0xC0FF_EE11);
        let mut file_key = [0u8; 32];
        for chunk in file_key.chunks_exact_mut(8) {
            chunk.copy_from_slice(&rng.next().to_le_bytes());
        }
        let mut salts = [[0u8; 8]; 4];
        for s in &mut salts {
            s.copy_from_slice(&rng.next().to_le_bytes());
        }
        let mut perms_random = [0u8; 4];
        perms_random.copy_from_slice(&rng.next().to_le_bytes()[..4]);
        // Le mot de passe propriétaire vide vaut le mot de passe utilisateur (§7.6.4.4.9).
        let owner = if owner.is_empty() { user } else { owner };
        let p = permissions.to_p();
        let e = build_r6_entries(&file_key, user, owner, &salts, p, true, &perms_random);
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
            let mut id = Vec::with_capacity(16);
            id.extend_from_slice(&rng.next().to_le_bytes());
            id.extend_from_slice(&rng.next().to_le_bytes());
            id
        } else {
            id0
        };
        let mut id1 = Vec::with_capacity(16);
        id1.extend_from_slice(&rng.next().to_le_bytes());
        id1.extend_from_slice(&rng.next().to_le_bytes());
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

    /// Retire le chiffrement. Nécessite le mot de passe propriétaire, ou le
    /// mot de passe utilisateur si les permissions autorisent la modification.
    /// Enregistrer ensuite avec [`Document::save_full`].
    ///
    /// # Errors
    /// Mot de passe manquant, droits insuffisants, ou objet illisible.
    pub fn unprotect(&self) -> Result<()> {
        if self.needs_password() {
            return Err(Error::Encrypted);
        }
        let Some(old) = self.encrypt_ref() else {
            return Ok(());
        };
        let allowed = self
            .security()
            .is_some_and(|h| h.is_owner() || Permissions::from_p(h.permissions()).modify);
        if !allowed {
            return Err(Error::Unsupported(
                "les permissions du document interdisent de retirer sa protection".into(),
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
        assert!(!Permissions::from_p(h.permissions()).copy);
        assert_eq!(
            locked.info().unwrap().get(&Name::new("Title")),
            Some(&Object::String(b"Titre en clair".to_vec()))
        );
        let page = crate::pages::collect_pages(&locked).unwrap().remove(0);
        let Some(Object::Reference(c)) = page.dict.get(&Name::new("Contents")) else {
            panic!("contenu attendu");
        };
        let content = locked.stream_data(&locked.get(*c).unwrap()).unwrap().data;
        assert!(content.windows(6).any(|w| w == b"secret"));

        // Mot de passe propriétaire.
        let owner = Document::from_bytes(saved).unwrap();
        owner.authenticate(b"chef").unwrap();
        assert!(owner.security().unwrap().is_owner());
        assert_eq!(owner.version(), (2, 0));
    }

    #[test]
    fn unprotect_restores_plain_text() {
        let doc = Document::from_bytes(plain_pdf()).unwrap();
        doc.protect(b"", b"chef", Permissions::all()).unwrap();
        let saved = doc.save_full().unwrap();
        // Mot de passe utilisateur vide : ouverture libre.
        let d2 = Document::from_bytes(saved).unwrap();
        assert!(d2.is_encrypted() && !d2.needs_password());
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
}
