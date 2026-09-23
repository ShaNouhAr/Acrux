//! Document PDF chargé : accès aux objets par référence avec cache,
//! résolution des références, flux d'objets, décodage des flux et
//! réparation automatique quand la table xref est fausse.
//!
//! Le fichier est lu en mémoire en une fois pour l'instant ; le mapping
//! mémoire (prévu par ARCHITECTURE.md) viendra avec la couche `platform`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::path::Path;
use std::rc::Rc;

use acrux_core::{Error, Result};

use crate::crypt::SecurityHandler;
use crate::filters::{self, Decoded};
use crate::lexer::{Lexer, Token};
use crate::objects::{Dict, Name, Object, ObjectRef};
use crate::parser::Parser;
use crate::repair;
use crate::xref::{self, Xref, XrefEntry, XrefKind};

/// Objet obtenu par résolution : soit un emprunt direct, soit un objet
/// indirect partagé depuis le cache.
#[derive(Debug, Clone)]
pub enum Resolved<'a> {
    /// L'objet n'était pas une référence.
    Direct(&'a Object),
    /// L'objet a été chargé depuis le fichier.
    Indirect(Rc<Object>),
}

impl Deref for Resolved<'_> {
    type Target = Object;
    fn deref(&self) -> &Object {
        match self {
            Resolved::Direct(o) => o,
            Resolved::Indirect(o) => o,
        }
    }
}

/// Flux d'objets décodé et indexé (§7.5.7).
struct ObjectStream {
    content: Vec<u8>,
    /// (numéro d'objet, offset dans `content`) dans l'ordre du flux.
    offsets: Vec<(u32, usize)>,
}

/// Document PDF en lecture.
pub struct Document {
    data: Vec<u8>,
    xref: RefCell<Xref>,
    cache: RefCell<HashMap<u32, Rc<Object>>>,
    object_streams: RefCell<HashMap<u32, Rc<ObjectStream>>>,
    /// Objets en cours de chargement (garde contre les références circulaires
    /// du type `/Length` qui pointe sur le flux lui-même).
    loading: RefCell<HashSet<u32>>,
    repaired: Cell<bool>,
    header_version: Option<(u8, u8)>,
    header_offset: usize,
    warnings: RefCell<Vec<String>>,
    /// Gestionnaire de sécurité une fois le mot de passe accepté.
    pub(crate) security: RefCell<Option<SecurityHandler>>,
    /// Référence du dictionnaire `/Encrypt` (jamais déchiffré lui-même).
    pub(crate) encrypt_ref: Cell<Option<ObjectRef>>,
    /// Modifications en attente d'enregistrement : `None` = objet supprimé.
    pub(crate) edits: RefCell<HashMap<u32, Option<Object>>>,
    /// Prochain numéro d'objet libre pour `allocate`.
    pub(crate) next_number: Cell<u32>,
    /// Clés du trailer modifiées (ex. `/Info` remplacé).
    pub(crate) trailer_edits: RefCell<Dict>,
}

impl Document {
    /// Charge un fichier.
    ///
    /// # Errors
    /// Lecture impossible, fichier qui n'est pas un PDF, ou structure
    /// irréparable (aucun catalogue trouvé même après balayage).
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let data = std::fs::read(path)?;
        Self::from_bytes(data)
    }

    /// Charge depuis des octets en mémoire.
    ///
    /// # Errors
    /// Voir [`Document::load`].
    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        let (header_offset, header_version) = find_header(&data);
        if header_version.is_none() && !looks_like_pdf(&data) {
            return Err(Error::NotAPdf);
        }
        let mut warnings = Vec::new();
        let decode = |d: &Dict, raw: &[u8]| -> Result<Vec<u8>> {
            filters::decode_stream(d, raw, &|o| o.clone()).map(|x| x.data)
        };
        let (xref, repaired) = match xref::read(&data, &decode) {
            Ok(x) => (x, false),
            Err(e) => {
                warnings.push(format!(
                    "table xref illisible ({e}) : reconstruction par balayage"
                ));
                let x = repair::reconstruct(&data, &decode);
                if !x.trailer.contains_key(&Name::new("Root")) {
                    return Err(Error::Corrupt(
                        "aucun catalogue trouvé, même après balayage".into(),
                    ));
                }
                (x, true)
            }
        };
        warnings.extend(xref.warnings.iter().cloned());
        let encrypt_ref = xref::trailer_ref(&xref.trailer, "Encrypt");
        let size = xref
            .trailer
            .get(&Name::new("Size"))
            .and_then(Object::as_i64)
            .and_then(|s| u32::try_from(s).ok())
            .unwrap_or(0);
        let max_known = xref.entries.keys().max().map_or(0, |m| m + 1);
        let next_number = size.max(max_known).max(1);
        let doc = Self {
            data,
            xref: RefCell::new(xref),
            cache: RefCell::new(HashMap::new()),
            object_streams: RefCell::new(HashMap::new()),
            loading: RefCell::new(HashSet::new()),
            repaired: Cell::new(repaired),
            header_version,
            header_offset,
            warnings: RefCell::new(warnings),
            security: RefCell::new(None),
            encrypt_ref: Cell::new(encrypt_ref),
            edits: RefCell::new(HashMap::new()),
            next_number: Cell::new(next_number),
            trailer_edits: RefCell::new(Dict::new()),
        };
        // Document chiffré : on tente le mot de passe utilisateur vide (cas le
        // plus fréquent). En cas d'échec le document reste ouvert mais ses
        // chaînes et flux sont illisibles jusqu'à `authenticate`.
        if doc.is_encrypted() {
            match doc.authenticate(b"") {
                Ok(()) => {}
                Err(Error::Encrypted) => doc.warn("mot de passe requis".into()),
                Err(e) => doc.warn(format!("chiffrement non pris en charge : {e}")),
            }
        }
        // Le catalogue doit être lisible ; sinon on tente la réparation tout de suite.
        if doc.catalog().is_err() && !doc.repaired.get() {
            doc.repair("catalogue illisible");
            doc.catalog()?;
        }
        Ok(doc)
    }

    /// Tente d'ouvrir un document chiffré avec un mot de passe (utilisateur ou
    /// propriétaire). Les objets déjà chargés sont rechargés.
    ///
    /// # Errors
    /// [`Error::Encrypted`] si le mot de passe est refusé ; [`Error::Unsupported`]
    /// si le gestionnaire de sécurité n'est pas le gestionnaire standard.
    pub fn authenticate(&self, password: &[u8]) -> Result<()> {
        let Some(handler) = self.handler_for(password)? else {
            return Ok(());
        };
        *self.security.borrow_mut() = Some(handler);
        self.cache.borrow_mut().clear();
        self.object_streams.borrow_mut().clear();
        Ok(())
    }

    /// Vrai si le document s'ouvre sans mot de passe : en clair, ou chiffré
    /// avec un mot de passe d'ouverture vide (ce qui laisse les permissions
    /// s'appliquer). Rien n'est changé au gestionnaire en place.
    #[must_use]
    pub fn opens_without_password(&self) -> bool {
        self.handler_for(b"").is_ok()
    }

    /// Gestionnaire de sécurité qu'ouvrirait `password`, sans l'installer ;
    /// `None` pour un document en clair.
    fn handler_for(&self, password: &[u8]) -> Result<Option<SecurityHandler>> {
        let trailer = self.trailer();
        let Some(enc) = trailer.get(&Name::new("Encrypt")) else {
            return Ok(None);
        };
        let enc = self.resolve(enc)?;
        let Some(enc) = enc.as_dict() else {
            return Err(Error::Corrupt("/Encrypt n'est pas un dictionnaire".into()));
        };
        let id0 = match trailer.get(&Name::new("ID")) {
            Some(Object::Array(ids)) => match ids.first() {
                Some(Object::String(s)) => s.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        let resolve = |o: &Object| -> Object {
            match self.resolve(o) {
                Ok(r) => (*r).clone(),
                Err(_) => Object::Null,
            }
        };
        SecurityHandler::new(enc, &id0, password, &resolve).map(Some)
    }

    /// Vrai si le document est chiffré et qu'aucun mot de passe valide n'a été fourni.
    #[must_use]
    pub fn needs_password(&self) -> bool {
        self.is_encrypted() && self.security.borrow().is_none()
    }

    /// Gestionnaire de sécurité actif, le cas échéant.
    #[must_use]
    pub fn security(&self) -> Option<SecurityHandler> {
        self.security.borrow().clone()
    }

    /// Octets bruts du fichier.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// Version de l'en-tête `%PDF-x.y`, ou celle du catalogue (`/Version`) si
    /// elle est plus récente (§7.7.2).
    #[must_use]
    pub fn version(&self) -> (u8, u8) {
        let header = self.header_version.unwrap_or((1, 0));
        let from_catalog = self
            .catalog()
            .ok()
            .and_then(|c| c.get(&Name::new("Version")).cloned())
            .and_then(|v| {
                self.resolve(&v)
                    .ok()
                    .and_then(|r| r.as_name().map(Name::as_str))
            })
            .and_then(|s| parse_version(&s));
        match from_catalog {
            Some(v) if v > header => v,
            _ => header,
        }
    }

    /// Dictionnaire trailer fusionné (modifications en attente incluses).
    #[must_use]
    pub fn trailer(&self) -> Dict {
        let mut t = self.xref.borrow().trailer.clone();
        for (k, v) in self.trailer_edits.borrow().iter() {
            if *v == Object::Null {
                t.remove(k);
            } else {
                t.insert(k.clone(), v.clone());
            }
        }
        t
    }

    /// Référence du dictionnaire `/Encrypt`, s'il existe.
    #[must_use]
    pub fn encrypt_ref(&self) -> Option<ObjectRef> {
        self.encrypt_ref.get()
    }

    /// Entrée de la table xref d'un objet (sans les modifications en attente).
    #[must_use]
    pub fn xref_entry(&self, number: u32) -> Option<XrefEntry> {
        self.xref.borrow().get(number)
    }

    /// Origine de la table xref.
    #[must_use]
    pub fn xref_kind(&self) -> XrefKind {
        self.xref.borrow().kind
    }

    /// Nombre d'entrées connues dans la table xref.
    #[must_use]
    pub fn xref_len(&self) -> usize {
        self.xref.borrow().entries.len()
    }

    /// Numéros d'objets connus, triés (modifications en attente incluses).
    #[must_use]
    pub fn object_numbers(&self) -> Vec<u32> {
        let edits = self.edits.borrow();
        let mut v: Vec<u32> = self
            .xref
            .borrow()
            .entries
            .iter()
            .filter(|(n, e)| !matches!(e, XrefEntry::Free) && !matches!(edits.get(n), Some(None)))
            .map(|(n, _)| *n)
            .collect();
        for (n, e) in edits.iter() {
            if e.is_some() && !v.contains(n) {
                v.push(*n);
            }
        }
        v.sort_unstable();
        v
    }

    /// Vrai si le trailer déclare un dictionnaire `/Encrypt`.
    #[must_use]
    pub fn is_encrypted(&self) -> bool {
        self.trailer().contains_key(&Name::new("Encrypt"))
    }

    /// Vrai si la table xref a dû être reconstruite.
    #[must_use]
    pub fn was_repaired(&self) -> bool {
        self.repaired.get()
    }

    /// Avertissements accumulés (structure réparée, objets illisibles…).
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        self.warnings.borrow().clone()
    }

    fn warn(&self, msg: String) {
        let mut w = self.warnings.borrow_mut();
        if w.len() < 1000 {
            w.push(msg);
        }
    }

    /// Catalogue du document (`/Root`).
    ///
    /// # Errors
    /// Trailer sans `/Root` ou catalogue qui n'est pas un dictionnaire.
    pub fn catalog(&self) -> Result<Dict> {
        let root = xref::trailer_ref(&self.xref.borrow().trailer, "Root")
            .ok_or_else(|| Error::Corrupt("trailer sans /Root".into()))?;
        match self.get(root)?.as_dict() {
            Some(d) => Ok(d.clone()),
            None => Err(Error::Corrupt(
                "le catalogue n'est pas un dictionnaire".into(),
            )),
        }
    }

    /// Dictionnaire `/Info` du trailer, s'il existe.
    #[must_use]
    pub fn info(&self) -> Option<Dict> {
        let r = xref::trailer_ref(&self.xref.borrow().trailer, "Info")?;
        self.get(r).ok()?.as_dict().cloned()
    }

    /// Résout une référence éventuelle.
    ///
    /// # Errors
    /// Objet illisible.
    pub fn resolve<'a>(&self, obj: &'a Object) -> Result<Resolved<'a>> {
        match obj {
            Object::Reference(r) => Ok(Resolved::Indirect(self.get(*r)?)),
            other => Ok(Resolved::Direct(other)),
        }
    }

    /// Valeur d'une clé de dictionnaire, référence résolue. `None` si absente.
    ///
    /// # Errors
    /// Objet référencé illisible.
    pub fn dict_get<'a>(&self, dict: &'a Dict, key: &str) -> Result<Option<Resolved<'a>>> {
        match dict.get(&Name::new(key)) {
            None => Ok(None),
            Some(o) => self.resolve(o).map(Some),
        }
    }

    /// Charge un objet indirect (mis en cache). Une référence vers un objet
    /// inexistant vaut `null` (§7.3.10), après une tentative de réparation.
    ///
    /// # Errors
    /// Objet présent mais illisible.
    pub fn get(&self, r: ObjectRef) -> Result<Rc<Object>> {
        if let Some(edit) = self.edits.borrow().get(&r.number) {
            return Ok(Rc::new(edit.clone().unwrap_or(Object::Null)));
        }
        if let Some(o) = self.cache.borrow().get(&r.number) {
            return Ok(Rc::clone(o));
        }
        if !self.loading.borrow_mut().insert(r.number) {
            // Référence circulaire pendant le chargement (ex. /Length → soi-même).
            return Ok(Rc::new(Object::Null));
        }
        let result = self.load_object(r);
        self.loading.borrow_mut().remove(&r.number);
        let obj = match result {
            Ok(o) => o,
            Err(e) => {
                if self.repaired.get() {
                    return Err(e);
                }
                self.repair(&format!("objet {} illisible : {e}", r.number));
                // Après réparation, le cache a été vidé : on recharge.
                self.loading.borrow_mut().insert(r.number);
                let again = self.load_object(r);
                self.loading.borrow_mut().remove(&r.number);
                again?
            }
        };
        let rc = Rc::new(obj);
        self.cache.borrow_mut().insert(r.number, Rc::clone(&rc));
        Ok(rc)
    }

    /// Reconstruit la table xref et vide les caches.
    fn repair(&self, reason: &str) {
        self.warn(format!("réparation : {reason}"));
        let decode = |d: &Dict, raw: &[u8]| -> Result<Vec<u8>> {
            filters::decode_stream(d, raw, &|o| o.clone()).map(|x| x.data)
        };
        let mut rebuilt = repair::reconstruct(&self.data, &decode);
        // On garde les clés du trailer d'origine que le balayage n'a pas retrouvées.
        let old = self.xref.borrow().trailer.clone();
        for (k, v) in old {
            rebuilt.trailer.entry(k).or_insert(v);
        }
        *self.xref.borrow_mut() = rebuilt;
        self.cache.borrow_mut().clear();
        self.object_streams.borrow_mut().clear();
        self.repaired.set(true);
    }

    fn load_object(&self, r: ObjectRef) -> Result<Object> {
        let entry = self.xref.borrow().get(r.number);
        match entry {
            // Entrée libre : l'objet a été supprimé, la référence vaut `null` (§7.3.10).
            Some(XrefEntry::Free) => Ok(Object::Null),
            // Numéro inconnu : peut-être une table incomplète, on tentera la réparation.
            None => {
                if self.repaired.get() {
                    Ok(Object::Null)
                } else {
                    Err(Error::MissingObject {
                        number: r.number,
                        generation: r.generation,
                    })
                }
            }
            Some(XrefEntry::Offset { offset, .. }) => {
                let offset = usize::try_from(offset).unwrap_or(usize::MAX);
                // Les offsets sont relatifs à `%PDF` chez Acrobat quand le fichier
                // commence par des octets parasites : on essaie les deux.
                let parsed = match self.parse_at(offset, r.number) {
                    Ok(o) => Ok(o),
                    Err(e) if self.header_offset > 0 => self
                        .parse_at(offset.saturating_add(self.header_offset), r.number)
                        .map_err(|_| e),
                    Err(e) => Err(e),
                };
                // Déchiffrement : seuls les objets lus directement dans le fichier
                // sont chiffrés (ceux d'un flux d'objets le sont via leur flux) ;
                // le dictionnaire /Encrypt lui-même ne l'est jamais (§7.6.2).
                match parsed {
                    Ok(o) if self.encrypt_ref.get().is_none_or(|e| e.number != r.number) => {
                        match &*self.security.borrow() {
                            Some(h) => Ok(h.decrypt_object(o, r)),
                            None => Ok(o),
                        }
                    }
                    other => other,
                }
            }
            Some(XrefEntry::InStream {
                stream_number,
                index,
            }) => self.load_from_object_stream(stream_number, index, r.number),
        }
    }

    /// Parse l'objet indirect à `offset` et vérifie son numéro.
    fn parse_at(&self, offset: usize, expected: u32) -> Result<Object> {
        if offset >= self.data.len() {
            return Err(Error::Corrupt(format!("offset {offset} hors du fichier")));
        }
        let mut p = Parser::at(&self.data, offset);
        let resolve_length = |lr: ObjectRef| -> Option<i64> {
            if lr.number == expected {
                return None;
            }
            self.get(lr).ok()?.as_i64()
        };
        let (found, obj) = p.parse_indirect(&resolve_length)?;
        if found.number != expected {
            return Err(Error::Corrupt(format!(
                "offset {offset} : objet {} trouvé, {expected} attendu",
                found.number
            )));
        }
        Ok(obj)
    }

    fn load_from_object_stream(
        &self,
        stream_number: u32,
        index: u32,
        expected: u32,
    ) -> Result<Object> {
        let stm = self.object_stream(stream_number)?;
        // On cherche par numéro plutôt que par index : les index des tables
        // xref sont parfois faux, les numéros dans l'en-tête du flux rarement.
        let by_number = stm.offsets.iter().find(|(n, _)| *n == expected);
        let by_index = stm
            .offsets
            .get(index as usize)
            .filter(|(n, _)| *n == expected);
        let Some(&(_, off)) = by_index.or(by_number) else {
            return Err(Error::Corrupt(format!(
                "objet {expected} absent du flux d'objets {stream_number}"
            )));
        };
        Parser::at(&stm.content, off).parse_object()
    }

    fn object_stream(&self, number: u32) -> Result<Rc<ObjectStream>> {
        if let Some(s) = self.object_streams.borrow().get(&number) {
            return Ok(Rc::clone(s));
        }
        let obj = self.get(ObjectRef {
            number,
            generation: 0,
        })?;
        let Object::Stream { dict, raw } = &*obj else {
            return Err(Error::Corrupt(format!(
                "l'objet {number} n'est pas un flux d'objets"
            )));
        };
        let content = self.decode_raw(dict, raw)?.data;
        let n = dict
            .get(&Name::new("N"))
            .and_then(Object::as_i64)
            .unwrap_or(0);
        let first = dict
            .get(&Name::new("First"))
            .and_then(Object::as_i64)
            .and_then(|f| usize::try_from(f).ok())
            .unwrap_or(0);
        let mut lx = Lexer::new(&content);
        let mut offsets = Vec::new();
        for _ in 0..n.max(0) {
            let (Ok(Token::Integer(on)), Ok(Token::Integer(off))) =
                (lx.next_token(), lx.next_token())
            else {
                break;
            };
            if let (Ok(on), Ok(off)) = (u32::try_from(on), usize::try_from(off)) {
                offsets.push((on, first.saturating_add(off)));
            }
        }
        let stm = Rc::new(ObjectStream { content, offsets });
        self.object_streams
            .borrow_mut()
            .insert(number, Rc::clone(&stm));
        Ok(stm)
    }

    /// Données décodées d'un flux (filtres standard appliqués ; un filtre
    /// d'image éventuel est signalé dans le résultat).
    ///
    /// # Errors
    /// L'objet n'est pas un flux, ou un filtre a échoué.
    pub fn stream_data(&self, obj: &Object) -> Result<Decoded> {
        match obj {
            Object::Stream { dict, raw } => self.decode_raw(dict, raw),
            Object::Reference(r) => {
                let target = self.get(*r)?;
                self.stream_data(&target)
            }
            _ => Err(Error::Corrupt("flux attendu".into())),
        }
    }

    fn decode_raw(&self, dict: &Dict, raw: &[u8]) -> Result<Decoded> {
        let resolve = |o: &Object| -> Object {
            match self.resolve(o) {
                Ok(r) => (*r).clone(),
                Err(_) => Object::Null,
            }
        };
        filters::decode_stream(dict, raw, &resolve)
    }
}

/// Cherche `%PDF-x.y` dans les 1024 premiers octets (§7.5.2 ; Acrobat tolère
/// des octets parasites avant l'en-tête).
fn find_header(data: &[u8]) -> (usize, Option<(u8, u8)>) {
    let window = &data[..data.len().min(1024)];
    let Some(pos) = window.windows(5).position(|w| w == b"%PDF-") else {
        return (0, None);
    };
    let rest = &data[pos + 5..];
    let end = rest
        .iter()
        .position(|&b| !(b.is_ascii_digit() || b == b'.'))
        .unwrap_or(rest.len());
    let version = std::str::from_utf8(&rest[..end])
        .ok()
        .and_then(parse_version);
    (pos, version)
}

fn parse_version(s: &str) -> Option<(u8, u8)> {
    let mut it = s.trim().split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
}

/// Heuristique quand l'en-tête manque : présence d'un objet ou d'un trailer.
fn looks_like_pdf(data: &[u8]) -> bool {
    let lx = Lexer::new(data);
    lx.find(b" obj").is_some() || lx.find(b"trailer").is_some() || lx.find(b"startxref").is_some()
}

impl std::fmt::Debug for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document")
            .field("bytes", &self.data.len())
            .field("version", &self.header_version)
            .field("xref_kind", &self.xref.borrow().kind)
            .field("objects", &self.xref.borrow().entries.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod tests {
    use super::*;

    /// Assemble un PDF avec une table xref exacte à partir d'une liste
    /// d'objets `(numéro, corps)`. Le corps est la syntaxe entre `obj` et `endobj`.
    pub(crate) fn build_pdf(
        version: &str,
        objects: &[(u32, &str)],
        trailer_extra: &str,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("%PDF-{version}\n%\u{E2}\u{E3}\u{CF}\u{D3}\n").as_bytes());
        let mut offsets: Vec<(u32, usize)> = Vec::new();
        for (n, body) in objects {
            offsets.push((*n, out.len()));
            out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let max = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max + 1).as_bytes());
        for n in 1..=max {
            match offsets.iter().find(|(m, _)| *m == n) {
                Some((_, o)) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n",
                max + 1
            )
            .as_bytes(),
        );
        out
    }

    pub(crate) fn simple_pdf() -> Vec<u8> {
        build_pdf(
            "1.4",
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (
                    2,
                    "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
                ),
                (3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>"),
                (4, "<< /Length 5 0 R >>\nstream\nBT ET\nendstream"),
                (5, "5"),
                (6, "<< /Title (Test) /Author <FEFF00C9006D0069006C0065> >>"),
            ],
            "/Info 6 0 R",
        )
    }

    #[test]
    fn loads_simple_document() {
        let doc = Document::from_bytes(simple_pdf()).unwrap();
        assert_eq!(doc.version(), (1, 4));
        assert_eq!(doc.xref_kind(), XrefKind::Table);
        assert!(!doc.was_repaired());
        assert!(!doc.is_encrypted());
        let cat = doc.catalog().unwrap();
        assert_eq!(
            cat.get(&Name::new("Type"))
                .unwrap()
                .as_name()
                .unwrap()
                .as_str(),
            "Catalog"
        );
        let info = doc.info().unwrap();
        assert_eq!(
            info.get(&Name::new("Title")),
            Some(&Object::String(b"Test".to_vec()))
        );
        assert_eq!(doc.object_numbers(), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn indirect_length_and_stream_data() {
        let doc = Document::from_bytes(simple_pdf()).unwrap();
        let c = doc
            .get(ObjectRef {
                number: 4,
                generation: 0,
            })
            .unwrap();
        assert_eq!(doc.stream_data(&c).unwrap().data, b"BT ET");
    }

    #[test]
    fn missing_object_is_null_after_repair_attempt() {
        let doc = Document::from_bytes(simple_pdf()).unwrap();
        let o = doc
            .get(ObjectRef {
                number: 99,
                generation: 0,
            })
            .unwrap();
        assert_eq!(*o, Object::Null);
        assert!(doc.was_repaired());
    }

    #[test]
    fn broken_offsets_trigger_repair() {
        let mut pdf = simple_pdf();
        // Sabote la table xref : tous les offsets à 0.
        let x = Lexer::new(&pdf).find(b"xref\n").unwrap();
        let t = Lexer::new(&pdf).find(b"trailer").unwrap();
        for b in &mut pdf[x + 5..t] {
            if b.is_ascii_digit() {
                *b = b'0';
            }
        }
        let doc = Document::from_bytes(pdf).unwrap();
        assert!(doc.was_repaired());
        assert_eq!(doc.xref_kind(), XrefKind::Reconstructed);
        assert_eq!(doc.catalog().unwrap().len(), 2);
    }

    #[test]
    fn no_xref_at_all() {
        let pdf = b"%PDF-1.7\n1 0 obj << /Type /Catalog >> endobj\n".to_vec();
        let doc = Document::from_bytes(pdf).unwrap();
        assert!(doc.was_repaired());
        assert_eq!(doc.version(), (1, 7));
    }

    #[test]
    fn junk_before_header_shifts_offsets() {
        let mut pdf = b"JUNKJUNKJUNK\n".to_vec();
        pdf.extend_from_slice(&simple_pdf());
        let doc = Document::from_bytes(pdf).unwrap();
        // Les offsets de la table sont décalés de 13 octets : résolus sans réparation.
        assert!(!doc.was_repaired());
        assert_eq!(doc.catalog().unwrap().len(), 2);
    }

    #[test]
    fn not_a_pdf() {
        assert_eq!(
            Document::from_bytes(b"hello world".to_vec()).err(),
            Some(Error::NotAPdf)
        );
        assert!(Document::from_bytes(Vec::new()).is_err());
    }

    #[test]
    fn catalog_version_overrides_header() {
        let pdf = build_pdf("1.4", &[(1, "<< /Type /Catalog /Version /1.6 >>")], "");
        assert_eq!(Document::from_bytes(pdf).unwrap().version(), (1, 6));
        let pdf = build_pdf("1.7", &[(1, "<< /Type /Catalog /Version /1.3 >>")], "");
        assert_eq!(Document::from_bytes(pdf).unwrap().version(), (1, 7));
    }

    #[test]
    fn objects_in_object_streams() {
        let inner = "3 0 4 18 << /Type /Page >> << /X 1 >>";
        let objstm = format!(
            "<< /Type /ObjStm /N 2 /First 9 /Length {} >>\nstream\n{inner}\nendstream",
            inner.len()
        );
        // Pas de table xref : la reconstruction découvre le flux d'objets et son contenu.
        let pdf = format!(
            "%PDF-1.5\n1 0 obj << /Type /Catalog /Pages 4 0 R >> endobj\n2 0 obj {objstm} endobj\n"
        );
        let doc = Document::from_bytes(pdf.into_bytes()).unwrap();
        let o3 = doc
            .get(ObjectRef {
                number: 3,
                generation: 0,
            })
            .unwrap();
        assert_eq!(
            o3.as_dict()
                .unwrap()
                .get(&Name::new("Type"))
                .unwrap()
                .as_name()
                .unwrap()
                .as_str(),
            "Page"
        );
        let o4 = doc
            .get(ObjectRef {
                number: 4,
                generation: 0,
            })
            .unwrap();
        assert_eq!(
            o4.as_dict().unwrap().get(&Name::new("X")),
            Some(&Object::Integer(1))
        );
    }

    #[test]
    fn self_referencing_length_does_not_loop() {
        let pdf = build_pdf(
            "1.4",
            &[
                (1, "<< /Type /Catalog >>"),
                (2, "<< /Length 2 0 R >>\nstream\nabc\nendstream"),
            ],
            "",
        );
        let doc = Document::from_bytes(pdf).unwrap();
        let s = doc
            .get(ObjectRef {
                number: 2,
                generation: 0,
            })
            .unwrap();
        assert_eq!(doc.stream_data(&s).unwrap().data, b"abc");
    }
}
