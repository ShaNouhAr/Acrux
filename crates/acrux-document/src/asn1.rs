//! ASN.1 en codage DER : lecteur strict et écrivain minimal.
//!
//! Le DER (X.690 §10) est la forme canonique du BER : longueurs **définies**
//! et codées sur le minimum d'octets, booléens valant 0x00 ou 0xFF, éléments
//! d'un `SET OF` triés. Tout ce qui sert aux signatures numériques — X.509
//! (RFC 5280), CMS (RFC 5652), PKCS#1 et PKCS#8 — est spécifié en DER.
//!
//! Le lecteur refuse donc explicitement :
//! - la forme à longueur indéfinie (`0x80`), légale en BER, interdite en DER ;
//! - une longueur codée sur plus d'octets que nécessaire ;
//! - un identifiant de tag à plusieurs octets non minimal ;
//! - des octets résiduels après le dernier élément attendu ([`Reader::end`]).
//!
//! Ce refus n'est pas du purisme : accepter du BER laxiste, c'est accepter
//! que deux analyseurs lisent deux structures différentes dans les mêmes
//! octets, et c'est exactement là que naissent les contournements de
//! signature.

use std::fmt;

use acrux_core::{Error, Result};

// --- Numéros de tag universels (X.680 §8, table 1) -------------------------

/// `BOOLEAN`.
pub const BOOLEAN: u32 = 0x01;
/// `INTEGER`.
pub const INTEGER: u32 = 0x02;
/// `BIT STRING`.
pub const BIT_STRING: u32 = 0x03;
/// `OCTET STRING`.
pub const OCTET_STRING: u32 = 0x04;
/// `NULL`.
pub const NULL: u32 = 0x05;
/// `OBJECT IDENTIFIER`.
pub const OID: u32 = 0x06;
/// `UTF8String`.
pub const UTF8_STRING: u32 = 0x0C;
/// `PrintableString`.
pub const PRINTABLE_STRING: u32 = 0x13;
/// `T61String` (Teletex).
pub const T61_STRING: u32 = 0x14;
/// `IA5String`.
pub const IA5_STRING: u32 = 0x16;
/// `UTCTime`.
pub const UTC_TIME: u32 = 0x17;
/// `GeneralizedTime`.
pub const GENERALIZED_TIME: u32 = 0x18;
/// `BMPString` (UTF-16BE).
pub const BMP_STRING: u32 = 0x1E;
/// `SEQUENCE` / `SEQUENCE OF`.
pub const SEQUENCE: u32 = 0x10;
/// `SET` / `SET OF`.
pub const SET: u32 = 0x11;

/// Classe de tag (X.690 §8.1.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Type défini par la norme ASN.1 elle-même.
    Universal,
    /// Extension propre à une application.
    Application,
    /// `[0]`, `[1]`… : dépend du contexte d'utilisation.
    Context,
    /// Usage privé.
    Private,
}

impl Class {
    fn from_bits(bits: u8) -> Class {
        match bits >> 6 {
            0 => Class::Universal,
            1 => Class::Application,
            2 => Class::Context,
            _ => Class::Private,
        }
    }

    fn bits(self) -> u8 {
        match self {
            Class::Universal => 0x00,
            Class::Application => 0x40,
            Class::Context => 0x80,
            Class::Private => 0xC0,
        }
    }
}

/// Élément DER : tag, forme, contenu, et les octets bruts complets.
#[derive(Debug, Clone, Copy)]
pub struct Tlv<'a> {
    /// Classe du tag.
    pub class: Class,
    /// Vrai pour un type construit (séquence, ensemble, `[0]` explicite).
    pub constructed: bool,
    /// Numéro de tag.
    pub number: u32,
    /// Contenu, sans l'en-tête.
    pub content: &'a [u8],
    /// Élément entier, en-tête comprise. Utile pour re-signer ou re-condenser
    /// une sous-structure sans la réécrire (`tbsCertificate`, attributs signés).
    pub raw: &'a [u8],
    /// Position de l'en-tête dans le tampon d'origine (pour les messages d'erreur).
    pub offset: usize,
}

impl Tlv<'_> {
    /// Vrai si l'élément porte exactement ce tag universel.
    #[must_use]
    pub fn is(&self, number: u32) -> bool {
        self.class == Class::Universal && self.number == number
    }

    /// Vrai si l'élément porte ce tag de classe contexte (`[n]`).
    #[must_use]
    pub fn is_context(&self, number: u32) -> bool {
        self.class == Class::Context && self.number == number
    }
}

/// Identifiant d'objet, suite d'arcs entiers (`1.2.840.113549.1.1.11`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Oid(pub Vec<u32>);

impl Oid {
    /// Construit un OID depuis sa notation pointée.
    ///
    /// # Errors
    /// Notation invalide (arc vide ou non numérique).
    pub fn parse(dotted: &str) -> Result<Oid> {
        let mut arcs = Vec::new();
        for part in dotted.split('.') {
            let v: u32 = part
                .parse()
                .map_err(|_| syntax(0, format!("OID invalide « {dotted} »")))?;
            arcs.push(v);
        }
        if arcs.len() < 2 {
            return Err(syntax(0, format!("OID trop court « {dotted} »")));
        }
        Ok(Oid(arcs))
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, arc) in self.0.iter().enumerate() {
            if i > 0 {
                write!(f, ".")?;
            }
            write!(f, "{arc}")?;
        }
        Ok(())
    }
}

/// Instant lu dans un `UTCTime` ou un `GeneralizedTime`, toujours ramené à UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Time {
    /// Année complète (1950-2049 pour un `UTCTime`, libre sinon).
    pub year: i32,
    /// Mois, 1 à 12.
    pub month: u32,
    /// Jour, 1 à 31.
    pub day: u32,
    /// Heure, 0 à 23.
    pub hour: u32,
    /// Minute, 0 à 59.
    pub minute: u32,
    /// Seconde, 0 à 59 (60 possible en `GeneralizedTime`, ramenée à 59).
    pub second: u32,
}

impl Time {
    /// Nombre de secondes depuis le 1er janvier 1970 UTC (peut être négatif).
    ///
    /// Calendrier grégorien proleptique, sans seconde intercalaire — comme
    /// tout le monde le fait pour comparer des dates de certificats.
    #[must_use]
    pub fn to_unix(self) -> i64 {
        let days = days_from_civil(self.year, self.month, self.day);
        days * 86_400
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
    }

    /// Instant courant, lu sur l'horloge système.
    #[must_use]
    pub fn now() -> Time {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Time::from_unix(i64::try_from(secs).unwrap_or(0))
    }

    /// Reconstruit une date depuis un nombre de secondes Unix.
    #[must_use]
    pub fn from_unix(secs: i64) -> Time {
        let days = secs.div_euclid(86_400);
        let rest = secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        Time {
            year,
            month,
            day,
            hour: u32::try_from(rest / 3600).unwrap_or(0),
            minute: u32::try_from((rest % 3600) / 60).unwrap_or(0),
            second: u32::try_from(rest % 60).unwrap_or(0),
        }
    }

    /// Format lisible `AAAA-MM-JJ hh:mm:ss UTC`.
    #[must_use]
    pub fn to_display(self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_display())
    }
}

/// Jours écoulés depuis l'époque Unix (algorithme de Howard Hinnant,
/// « chrono-Compatible Low-Level Date Algorithms »).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let m = m.clamp(1, 12);
    let d = d.clamp(1, 31);
    let y = i64::from(y) - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Réciproque de [`days_from_civil`].
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        i32::try_from(y + i64::from(m <= 2)).unwrap_or(0),
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

fn syntax(offset: usize, message: String) -> Error {
    Error::Syntax {
        offset: offset as u64,
        message,
    }
}

// --- Lecteur ---------------------------------------------------------------

/// Lecteur DER positionné sur une suite d'éléments.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    /// Décalage du tampon dans le document d'origine, pour les messages.
    base: usize,
}

impl<'a> Reader<'a> {
    /// Nouveau lecteur sur un tampon complet.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Reader<'a> {
        Reader {
            data,
            pos: 0,
            base: 0,
        }
    }

    /// Vrai s'il ne reste plus d'octet à lire.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pos >= self.data.len()
    }

    /// Octets non encore lus.
    #[must_use]
    pub fn remaining(&self) -> &'a [u8] {
        self.data.get(self.pos..).unwrap_or(&[])
    }

    /// Position absolue courante.
    #[must_use]
    pub fn position(&self) -> usize {
        self.base + self.pos
    }

    /// Exige que tout ait été consommé.
    ///
    /// # Errors
    /// Des octets subsistent après le dernier élément attendu.
    pub fn end(&self) -> Result<()> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(syntax(
                self.position(),
                format!(
                    "{} octets en trop après la structure",
                    self.data.len() - self.pos
                ),
            ))
        }
    }

    /// Lit l'élément suivant sans l'interpréter.
    ///
    /// # Errors
    /// Fin de tampon prématurée, longueur indéfinie, longueur non minimale.
    pub fn read(&mut self) -> Result<Tlv<'a>> {
        let start = self.pos;
        let first = *self
            .data
            .get(self.pos)
            .ok_or_else(|| syntax(self.position(), "élément ASN.1 attendu".into()))?;
        self.pos += 1;
        let class = Class::from_bits(first);
        let constructed = first & 0x20 != 0;
        let mut number = u32::from(first & 0x1F);
        if number == 0x1F {
            // Tag à plusieurs octets (X.690 §8.1.2.4) : 7 bits utiles par octet.
            number = 0;
            let mut first_byte = true;
            loop {
                let b = *self
                    .data
                    .get(self.pos)
                    .ok_or_else(|| syntax(self.position(), "identifiant de tag tronqué".into()))?;
                self.pos += 1;
                if first_byte && b.trailing_zeros() >= 7 {
                    return Err(syntax(
                        self.base + start,
                        "identifiant de tag non minimal".into(),
                    ));
                }
                first_byte = false;
                number = number
                    .checked_mul(128)
                    .and_then(|n| n.checked_add(u32::from(b & 0x7F)))
                    .ok_or_else(|| syntax(self.base + start, "numéro de tag démesuré".into()))?;
                if b & 0x80 == 0 {
                    break;
                }
            }
            if number < 0x1F {
                return Err(syntax(
                    self.base + start,
                    "identifiant de tag non minimal".into(),
                ));
            }
        }
        let len_byte = *self
            .data
            .get(self.pos)
            .ok_or_else(|| syntax(self.position(), "longueur ASN.1 attendue".into()))?;
        self.pos += 1;
        let length = if len_byte & 0x80 == 0 {
            usize::from(len_byte)
        } else {
            let count = usize::from(len_byte & 0x7F);
            if count == 0 {
                // 0x80 : forme indéfinie, réservée au BER.
                return Err(syntax(
                    self.base + start,
                    "longueur indéfinie (BER) refusée : le DER exige une longueur définie".into(),
                ));
            }
            if count > 8 {
                return Err(syntax(self.base + start, "longueur démesurée".into()));
            }
            let bytes = self
                .data
                .get(self.pos..self.pos + count)
                .ok_or_else(|| syntax(self.position(), "longueur tronquée".into()))?;
            if bytes.first() == Some(&0) {
                return Err(syntax(
                    self.base + start,
                    "longueur non minimale (zéro de tête)".into(),
                ));
            }
            let mut v: u64 = 0;
            for &b in bytes {
                v = (v << 8) | u64::from(b);
            }
            if v < 128 {
                return Err(syntax(
                    self.base + start,
                    "longueur courte codée en forme longue".into(),
                ));
            }
            self.pos += count;
            usize::try_from(v)
                .map_err(|_| syntax(self.base + start, "longueur démesurée".into()))?
        };
        let content = self
            .data
            .get(self.pos..self.pos + length)
            .ok_or_else(|| syntax(self.position(), "contenu ASN.1 tronqué".into()))?;
        self.pos += length;
        let raw = self.data.get(start..self.pos).unwrap_or(&[]);
        Ok(Tlv {
            class,
            constructed,
            number,
            content,
            raw,
            offset: self.base + start,
        })
    }

    /// Regarde l'élément suivant sans avancer.
    ///
    /// # Errors
    /// Voir [`Reader::read`].
    pub fn peek(&self) -> Result<Tlv<'a>> {
        let mut copy = self.clone();
        copy.read()
    }

    /// Vrai si l'élément suivant existe et porte ce tag universel.
    #[must_use]
    pub fn peek_is(&self, number: u32) -> bool {
        self.peek().is_ok_and(|t| t.is(number))
    }

    /// Vrai si l'élément suivant existe et porte ce tag de contexte.
    #[must_use]
    pub fn peek_is_context(&self, number: u32) -> bool {
        self.peek().is_ok_and(|t| t.is_context(number))
    }

    /// Lit un élément et exige le tag universel demandé.
    ///
    /// # Errors
    /// Tag inattendu, ou lecture impossible.
    pub fn expect(&mut self, number: u32) -> Result<Tlv<'a>> {
        let tlv = self.read()?;
        if !tlv.is(number) {
            return Err(syntax(
                tlv.offset,
                format!(
                    "tag universel {number} attendu, trouvé {:?} {}",
                    tlv.class, tlv.number
                ),
            ));
        }
        Ok(tlv)
    }

    /// Ouvre une `SEQUENCE` et renvoie un lecteur sur son contenu.
    ///
    /// # Errors
    /// L'élément suivant n'est pas une séquence construite.
    pub fn sequence(&mut self) -> Result<Reader<'a>> {
        let tlv = self.expect(SEQUENCE)?;
        if !tlv.constructed {
            return Err(syntax(tlv.offset, "SEQUENCE primitive".into()));
        }
        Ok(self.sub(tlv))
    }

    /// Ouvre un `SET` et renvoie un lecteur sur son contenu.
    ///
    /// # Errors
    /// L'élément suivant n'est pas un ensemble construit.
    pub fn set(&mut self) -> Result<Reader<'a>> {
        let tlv = self.expect(SET)?;
        if !tlv.constructed {
            return Err(syntax(tlv.offset, "SET primitif".into()));
        }
        Ok(self.sub(tlv))
    }

    /// Ouvre un élément `[n]` explicite et renvoie un lecteur sur son contenu.
    ///
    /// # Errors
    /// L'élément suivant n'est pas `[n]` construit.
    pub fn context(&mut self, number: u32) -> Result<Reader<'a>> {
        let tlv = self.read()?;
        if !tlv.is_context(number) || !tlv.constructed {
            return Err(syntax(tlv.offset, format!("[{number}] explicite attendu")));
        }
        Ok(self.sub(tlv))
    }

    /// Lecteur sur le contenu d'un élément déjà lu.
    #[must_use]
    pub fn sub(&self, tlv: Tlv<'a>) -> Reader<'a> {
        let header = tlv.raw.len() - tlv.content.len();
        Reader {
            data: tlv.content,
            pos: 0,
            base: tlv.offset + header,
        }
    }

    /// Lit un `INTEGER` et renvoie ses octets tels quels (complément à deux,
    /// gros-boutiste), après contrôle de la forme minimale.
    ///
    /// # Errors
    /// Tag inattendu, entier vide, ou codage non minimal.
    pub fn integer_bytes(&mut self) -> Result<&'a [u8]> {
        let tlv = self.expect(INTEGER)?;
        check_integer(tlv)?;
        Ok(tlv.content)
    }

    /// Lit un `INTEGER` positif et renvoie ses octets sans zéro de tête.
    ///
    /// # Errors
    /// Entier négatif, ou voir [`Reader::integer_bytes`].
    pub fn unsigned_bytes(&mut self) -> Result<&'a [u8]> {
        let tlv = self.expect(INTEGER)?;
        check_integer(tlv)?;
        if tlv.content.first().is_some_and(|b| b & 0x80 != 0) {
            return Err(syntax(tlv.offset, "entier négatif inattendu".into()));
        }
        let start = usize::from(tlv.content.first() == Some(&0) && tlv.content.len() > 1);
        Ok(tlv.content.get(start..).unwrap_or(&[]))
    }

    /// Lit un `INTEGER` tenant sur 64 bits signés.
    ///
    /// # Errors
    /// Entier trop grand, ou voir [`Reader::integer_bytes`].
    pub fn integer_i64(&mut self) -> Result<i64> {
        let tlv = self.expect(INTEGER)?;
        check_integer(tlv)?;
        if tlv.content.len() > 8 {
            return Err(syntax(
                tlv.offset,
                "entier hors de portée sur 64 bits".into(),
            ));
        }
        let negative = tlv.content.first().is_some_and(|b| b & 0x80 != 0);
        let mut v: i64 = if negative { -1 } else { 0 };
        for &b in tlv.content {
            v = (v << 8) | i64::from(b);
        }
        Ok(v)
    }

    /// Lit un `OBJECT IDENTIFIER`.
    ///
    /// # Errors
    /// Tag inattendu, contenu vide, arc non minimal ou tronqué.
    pub fn oid(&mut self) -> Result<Oid> {
        let tlv = self.expect(OID)?;
        parse_oid(tlv)
    }

    /// Lit un `OCTET STRING` primitif.
    ///
    /// # Errors
    /// Tag inattendu ou forme construite (interdite en DER).
    pub fn octet_string(&mut self) -> Result<&'a [u8]> {
        let tlv = self.expect(OCTET_STRING)?;
        if tlv.constructed {
            return Err(syntax(
                tlv.offset,
                "OCTET STRING construit refusé en DER".into(),
            ));
        }
        Ok(tlv.content)
    }

    /// Lit un `BIT STRING` et renvoie `(bits inutilisés, octets)`.
    ///
    /// # Errors
    /// Tag inattendu, forme construite, ou compte de bits inutilisés invalide.
    pub fn bit_string(&mut self) -> Result<(u8, &'a [u8])> {
        let tlv = self.expect(BIT_STRING)?;
        if tlv.constructed {
            return Err(syntax(
                tlv.offset,
                "BIT STRING construit refusé en DER".into(),
            ));
        }
        let unused = *tlv
            .content
            .first()
            .ok_or_else(|| syntax(tlv.offset, "BIT STRING vide".into()))?;
        if unused > 7 {
            return Err(syntax(tlv.offset, "bits inutilisés > 7".into()));
        }
        if unused > 0 && tlv.content.len() == 1 {
            return Err(syntax(
                tlv.offset,
                "BIT STRING vide avec bits inutilisés".into(),
            ));
        }
        Ok((unused, tlv.content.get(1..).unwrap_or(&[])))
    }

    /// Lit un `BOOLEAN`.
    ///
    /// # Errors
    /// Tag inattendu, ou valeur autre que 0x00 / 0xFF (le DER n'en admet pas d'autre).
    pub fn boolean(&mut self) -> Result<bool> {
        let tlv = self.expect(BOOLEAN)?;
        match tlv.content {
            [0x00] => Ok(false),
            [0xFF] => Ok(true),
            _ => Err(syntax(
                tlv.offset,
                "BOOLEAN non canonique (le DER impose 0x00 ou 0xFF)".into(),
            )),
        }
    }

    /// Lit un `NULL`.
    ///
    /// # Errors
    /// Tag inattendu ou contenu non vide.
    pub fn null(&mut self) -> Result<()> {
        let tlv = self.expect(NULL)?;
        if tlv.content.is_empty() {
            Ok(())
        } else {
            Err(syntax(tlv.offset, "NULL avec contenu".into()))
        }
    }

    /// Lit un `UTCTime` ou un `GeneralizedTime` (le choix `Time` de X.509).
    ///
    /// # Errors
    /// Tag inattendu ou date mal formée.
    pub fn time(&mut self) -> Result<Time> {
        let tlv = self.read()?;
        parse_time(tlv)
    }

    /// Lit une chaîne de caractères (`UTF8String`, `PrintableString`,
    /// `IA5String`, `T61String` ou `BMPString`) et la rend en `String`.
    ///
    /// # Errors
    /// Tag qui n'est pas un type chaîne connu.
    pub fn string(&mut self) -> Result<String> {
        let tlv = self.read()?;
        decode_string(tlv)
    }

    /// Ignore l'élément suivant.
    ///
    /// # Errors
    /// Voir [`Reader::read`].
    pub fn skip(&mut self) -> Result<()> {
        self.read().map(|_| ())
    }
}

/// Contrôle la forme minimale d'un `INTEGER` (X.690 §8.3.2).
fn check_integer(tlv: Tlv<'_>) -> Result<()> {
    match tlv.content {
        [] => Err(syntax(tlv.offset, "INTEGER vide".into())),
        [0x00, b, ..] if b & 0x80 == 0 => Err(syntax(
            tlv.offset,
            "INTEGER non minimal (zéro de tête superflu)".into(),
        )),
        [0xFF, b, ..] if b & 0x80 != 0 => Err(syntax(
            tlv.offset,
            "INTEGER non minimal (0xFF de tête superflu)".into(),
        )),
        _ => Ok(()),
    }
}

fn parse_oid(tlv: Tlv<'_>) -> Result<Oid> {
    if tlv.constructed {
        return Err(syntax(tlv.offset, "OID construit".into()));
    }
    if tlv.content.is_empty() {
        return Err(syntax(tlv.offset, "OID vide".into()));
    }
    let mut groups: Vec<u32> = Vec::new();
    let mut value: u32 = 0;
    let mut started = false;
    let mut pending = false;
    for &byte in tlv.content {
        if !started && byte == 0x80 {
            return Err(syntax(tlv.offset, "arc d'OID non minimal".into()));
        }
        started = true;
        value = value
            .checked_mul(128)
            .and_then(|v| v.checked_add(u32::from(byte & 0x7F)))
            .ok_or_else(|| syntax(tlv.offset, "arc d'OID démesuré".into()))?;
        if byte & 0x80 == 0 {
            groups.push(value);
            value = 0;
            started = false;
            pending = false;
        } else {
            pending = true;
        }
    }
    if pending {
        return Err(syntax(tlv.offset, "OID tronqué".into()));
    }
    // Les deux premiers arcs sont codés ensemble dans le premier groupe :
    // 40·arc1 + arc2 (X.690 §8.19.4). Le groupe est lui-même en base 128, donc
    // il faut l'avoir décodé en entier avant de le scinder.
    let first = groups.first().copied().unwrap_or(0);
    let (a, b) = if first < 40 {
        (0, first)
    } else if first < 80 {
        (1, first - 40)
    } else {
        (2, first - 80)
    };
    let mut arcs = vec![a, b];
    arcs.extend(groups.iter().skip(1).copied());
    Ok(Oid(arcs))
}

fn parse_time(tlv: Tlv<'_>) -> Result<Time> {
    let text = std::str::from_utf8(tlv.content)
        .map_err(|_| syntax(tlv.offset, "date non ASCII".into()))?;
    let digits = |s: &str| -> Result<u32> {
        s.parse::<u32>()
            .map_err(|_| syntax(tlv.offset, format!("date invalide « {text} »")))
    };
    // Suffixe de fuseau : « Z » ou « ±hhmm ».
    let (body, offset_minutes) = split_zone(text)
        .ok_or_else(|| syntax(tlv.offset, format!("fuseau absent dans « {text} »")))?;
    let (year, rest) = if tlv.is(UTC_TIME) {
        if body.len() < 10 {
            return Err(syntax(tlv.offset, format!("UTCTime trop court « {text} »")));
        }
        let yy = digits(body.get(..2).unwrap_or(""))?;
        // RFC 5280 §4.1.2.5.1 : 00-49 => 20xx, 50-99 => 19xx.
        let year = if yy < 50 { 2000 + yy } else { 1900 + yy };
        (
            i32::try_from(year).unwrap_or(0),
            body.get(2..).unwrap_or(""),
        )
    } else if tlv.is(GENERALIZED_TIME) {
        if body.len() < 12 {
            return Err(syntax(
                tlv.offset,
                format!("GeneralizedTime trop court « {text} »"),
            ));
        }
        let y = digits(body.get(..4).unwrap_or(""))?;
        (i32::try_from(y).unwrap_or(0), body.get(4..).unwrap_or(""))
    } else {
        return Err(syntax(
            tlv.offset,
            "UTCTime ou GeneralizedTime attendu".into(),
        ));
    };
    if rest.len() < 8 {
        return Err(syntax(tlv.offset, format!("date incomplète « {text} »")));
    }
    let month = digits(rest.get(..2).unwrap_or(""))?;
    let day = digits(rest.get(2..4).unwrap_or(""))?;
    let hour = digits(rest.get(4..6).unwrap_or(""))?;
    let minute = digits(rest.get(6..8).unwrap_or(""))?;
    let second = match rest.get(8..10) {
        Some(s) if s.len() == 2 => digits(s)?,
        _ => 0,
    };
    if month == 0 || month > 12 || day == 0 || day > 31 || hour > 23 || minute > 59 {
        return Err(syntax(tlv.offset, format!("date hors bornes « {text} »")));
    }
    let base = Time {
        year,
        month,
        day,
        hour,
        minute,
        second: second.min(59),
    };
    Ok(Time::from_unix(
        base.to_unix() - i64::from(offset_minutes) * 60,
    ))
}

/// Sépare le corps de la date de son indicateur de fuseau.
fn split_zone(text: &str) -> Option<(&str, i32)> {
    if let Some(body) = text.strip_suffix('Z') {
        return Some((body, 0));
    }
    let bytes = text.as_bytes();
    if text.len() >= 5 {
        let sign_index = text.len() - 5;
        if let Some(&sign_byte @ (b'+' | b'-')) = bytes.get(sign_index) {
            let sign = if sign_byte == b'-' { -1 } else { 1 };
            let zone = text.get(sign_index + 1..)?;
            let h: i32 = zone.get(..2)?.parse().ok()?;
            let m: i32 = zone.get(2..)?.parse().ok()?;
            return Some((text.get(..sign_index)?, sign * (h * 60 + m)));
        }
    }
    None
}

fn decode_string(tlv: Tlv<'_>) -> Result<String> {
    if tlv.class != Class::Universal {
        return Err(syntax(tlv.offset, "chaîne attendue".into()));
    }
    match tlv.number {
        UTF8_STRING => String::from_utf8(tlv.content.to_vec())
            .map_err(|_| syntax(tlv.offset, "UTF8String invalide".into())),
        PRINTABLE_STRING | IA5_STRING | T61_STRING => {
            // T61String est en principe du Teletex ; en pratique les émetteurs
            // y mettent du Latin-1, que l'on décode donc ainsi.
            Ok(tlv.content.iter().map(|&b| char::from(b)).collect())
        }
        BMP_STRING => {
            let mut units = Vec::with_capacity(tlv.content.len() / 2);
            for pair in tlv.content.chunks_exact(2) {
                units.push(u16::from_be_bytes([
                    pair.first().copied().unwrap_or(0),
                    pair.get(1).copied().unwrap_or(0),
                ]));
            }
            Ok(String::from_utf16_lossy(&units))
        }
        _ => Err(syntax(
            tlv.offset,
            format!("type chaîne inconnu (tag {})", tlv.number),
        )),
    }
}

// --- Écrivain --------------------------------------------------------------

/// Écriture DER : chaque fonction rend les octets complets d'un élément.
pub mod write {
    use super::{Class, Oid, Time};

    /// Élément quelconque : classe, forme, numéro de tag, contenu.
    #[must_use]
    pub fn tlv(class: Class, constructed: bool, number: u32, content: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(content.len() + 6);
        let base = class.bits() | if constructed { 0x20 } else { 0 };
        if number < 0x1F {
            out.push(base | u8::try_from(number).unwrap_or(0));
        } else {
            out.push(base | 0x1F);
            let mut arcs = Vec::new();
            let mut v = number;
            loop {
                arcs.push(u8::try_from(v & 0x7F).unwrap_or(0));
                v >>= 7;
                if v == 0 {
                    break;
                }
            }
            for (i, byte) in arcs.iter().rev().enumerate() {
                let last = i + 1 == arcs.len();
                out.push(if last { *byte } else { byte | 0x80 });
            }
        }
        write_length(content.len(), &mut out);
        out.extend_from_slice(content);
        out
    }

    /// Longueur en forme courte si possible, sinon en forme longue minimale.
    fn write_length(len: usize, out: &mut Vec<u8>) {
        if len < 128 {
            out.push(u8::try_from(len).unwrap_or(0));
            return;
        }
        let bytes = len.to_be_bytes();
        let first = bytes.iter().position(|&b| b != 0).unwrap_or(0);
        let significant = bytes.get(first..).unwrap_or(&[]);
        out.push(0x80 | u8::try_from(significant.len()).unwrap_or(0));
        out.extend_from_slice(significant);
    }

    /// Élément universel primitif.
    #[must_use]
    pub fn primitive(number: u32, content: &[u8]) -> Vec<u8> {
        tlv(Class::Universal, false, number, content)
    }

    /// `SEQUENCE` des éléments déjà codés, concaténés.
    #[must_use]
    pub fn sequence(parts: &[Vec<u8>]) -> Vec<u8> {
        tlv(Class::Universal, true, super::SEQUENCE, &concat(parts))
    }

    /// `SET` des éléments déjà codés. Pour un `SET OF`, le DER impose l'ordre
    /// croissant des codages : [`set_of`] s'en charge.
    #[must_use]
    pub fn set(parts: &[Vec<u8>]) -> Vec<u8> {
        tlv(Class::Universal, true, super::SET, &concat(parts))
    }

    /// `SET OF` trié selon l'ordre lexicographique des codages (X.690 §11.6).
    #[must_use]
    pub fn set_of(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut sorted = parts.to_vec();
        sorted.sort();
        set(&sorted)
    }

    /// `[n]` explicite (le contenu est un élément complet).
    #[must_use]
    pub fn context(number: u32, content: &[u8]) -> Vec<u8> {
        tlv(Class::Context, true, number, content)
    }

    /// `[n]` implicite primitif (le contenu remplace celui du type de base).
    #[must_use]
    pub fn context_primitive(number: u32, content: &[u8]) -> Vec<u8> {
        tlv(Class::Context, false, number, content)
    }

    /// `[n]` implicite construit (une `SEQUENCE` ou un `SET` retagué).
    #[must_use]
    pub fn context_constructed(number: u32, content: &[u8]) -> Vec<u8> {
        tlv(Class::Context, true, number, content)
    }

    /// `INTEGER` non signé à partir de ses octets gros-boutistes : les zéros
    /// de tête sont retirés et un `0x00` est ajouté si le bit de poids fort
    /// est à 1, pour que le nombre reste positif en complément à deux.
    #[must_use]
    pub fn unsigned_integer(bytes: &[u8]) -> Vec<u8> {
        let first = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
        let trimmed = bytes.get(first..).unwrap_or(&[]);
        if trimmed.is_empty() {
            return primitive(super::INTEGER, &[0]);
        }
        let mut content = Vec::with_capacity(trimmed.len() + 1);
        if trimmed.first().is_some_and(|b| b & 0x80 != 0) {
            content.push(0);
        }
        content.extend_from_slice(trimmed);
        primitive(super::INTEGER, &content)
    }

    /// `INTEGER` signé sur 64 bits, en forme minimale.
    #[must_use]
    pub fn integer_i64(value: i64) -> Vec<u8> {
        let bytes = value.to_be_bytes();
        let mut start = 0;
        while start + 1 < bytes.len() {
            let b = bytes.get(start).copied().unwrap_or(0);
            let next = bytes.get(start + 1).copied().unwrap_or(0);
            let redundant = (b == 0x00 && next & 0x80 == 0) || (b == 0xFF && next & 0x80 != 0);
            if !redundant {
                break;
            }
            start += 1;
        }
        primitive(super::INTEGER, bytes.get(start..).unwrap_or(&[]))
    }

    /// `OBJECT IDENTIFIER`.
    #[must_use]
    pub fn oid(value: &Oid) -> Vec<u8> {
        let mut content = Vec::new();
        let a = value.0.first().copied().unwrap_or(0);
        let b = value.0.get(1).copied().unwrap_or(0);
        push_base128(&mut content, a * 40 + b);
        for arc in value.0.iter().skip(2) {
            push_base128(&mut content, *arc);
        }
        primitive(super::OID, &content)
    }

    fn push_base128(out: &mut Vec<u8>, mut value: u32) {
        let mut chunks = Vec::new();
        loop {
            chunks.push(u8::try_from(value & 0x7F).unwrap_or(0));
            value >>= 7;
            if value == 0 {
                break;
            }
        }
        for (i, byte) in chunks.iter().rev().enumerate() {
            let last = i + 1 == chunks.len();
            out.push(if last { *byte } else { byte | 0x80 });
        }
    }

    /// `OCTET STRING`.
    #[must_use]
    pub fn octet_string(data: &[u8]) -> Vec<u8> {
        primitive(super::OCTET_STRING, data)
    }

    /// `BIT STRING` sans bit inutilisé.
    #[must_use]
    pub fn bit_string(data: &[u8]) -> Vec<u8> {
        let mut content = Vec::with_capacity(data.len() + 1);
        content.push(0);
        content.extend_from_slice(data);
        primitive(super::BIT_STRING, &content)
    }

    /// `BIT STRING` de drapeaux : `bits` du plus significatif au moins
    /// significatif, zéros de queue retirés comme l'exige le DER.
    #[must_use]
    pub fn named_bits(bits: &[bool]) -> Vec<u8> {
        let last = bits.iter().rposition(|&b| b);
        let Some(last) = last else {
            return primitive(super::BIT_STRING, &[0]);
        };
        let used = last + 1;
        let bytes = used.div_ceil(8);
        let mut content = vec![0u8; bytes + 1];
        if let Some(slot) = content.first_mut() {
            *slot = u8::try_from(bytes * 8 - used).unwrap_or(0);
        }
        for (i, &bit) in bits.iter().take(used).enumerate() {
            if bit {
                if let Some(slot) = content.get_mut(1 + i / 8) {
                    *slot |= 0x80 >> (i % 8);
                }
            }
        }
        primitive(super::BIT_STRING, &content)
    }

    /// `NULL`.
    #[must_use]
    pub fn null() -> Vec<u8> {
        primitive(super::NULL, &[])
    }

    /// `BOOLEAN` canonique (0x00 ou 0xFF).
    #[must_use]
    pub fn boolean(value: bool) -> Vec<u8> {
        primitive(super::BOOLEAN, &[if value { 0xFF } else { 0x00 }])
    }

    /// `UTF8String`.
    #[must_use]
    pub fn utf8_string(text: &str) -> Vec<u8> {
        primitive(super::UTF8_STRING, text.as_bytes())
    }

    /// `PrintableString` (les caractères hors du jeu autorisé sont remplacés
    /// par `?` : à l'appelant de choisir `utf8_string` si besoin).
    #[must_use]
    pub fn printable_string(text: &str) -> Vec<u8> {
        let content: Vec<u8> = text
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || " '()+,-./:=?".contains(c) {
                    u8::try_from(u32::from(c)).unwrap_or(b'?')
                } else {
                    b'?'
                }
            })
            .collect();
        primitive(super::PRINTABLE_STRING, &content)
    }

    /// `IA5String`.
    #[must_use]
    pub fn ia5_string(text: &str) -> Vec<u8> {
        let content: Vec<u8> = text
            .chars()
            .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
            .collect();
        primitive(super::IA5_STRING, &content)
    }

    /// `UTCTime` `AAMMJJhhmmssZ` (valide de 1950 à 2049, RFC 5280 §4.1.2.5.1).
    #[must_use]
    pub fn utc_time(t: Time) -> Vec<u8> {
        let yy = t.year.rem_euclid(100);
        let text = format!(
            "{:02}{:02}{:02}{:02}{:02}{:02}Z",
            yy, t.month, t.day, t.hour, t.minute, t.second
        );
        primitive(super::UTC_TIME, text.as_bytes())
    }

    /// `GeneralizedTime` `AAAAMMJJhhmmssZ`.
    #[must_use]
    pub fn generalized_time(t: Time) -> Vec<u8> {
        let text = format!(
            "{:04}{:02}{:02}{:02}{:02}{:02}Z",
            t.year, t.month, t.day, t.hour, t.minute, t.second
        );
        primitive(super::GENERALIZED_TIME, text.as_bytes())
    }

    /// Date au format qu'impose RFC 5280 §4.1.2.5 : `UTCTime` jusqu'en 2049,
    /// `GeneralizedTime` au-delà.
    #[must_use]
    pub fn x509_time(t: Time) -> Vec<u8> {
        if (1950..=2049).contains(&t.year) {
            utc_time(t)
        } else {
            generalized_time(t)
        }
    }

    /// `AlgorithmIdentifier` : un OID et des paramètres facultatifs.
    #[must_use]
    pub fn algorithm(id: &Oid, parameters: Option<&[u8]>) -> Vec<u8> {
        match parameters {
            Some(p) => sequence(&[oid(id), p.to_vec()]),
            None => sequence(&[oid(id)]),
        }
    }

    fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::with_capacity(parts.iter().map(Vec::len).sum());
        for p in parts {
            out.extend_from_slice(p);
        }
        out
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn short_and_long_lengths() {
        // 130 octets : forme longue sur un octet (0x81 0x82).
        let data = vec![0xAAu8; 130];
        let encoded = write::octet_string(&data);
        assert_eq!(encoded.get(..3), Some([0x04, 0x81, 0x82].as_slice()));
        let mut r = Reader::new(&encoded);
        assert_eq!(r.octet_string().unwrap(), data.as_slice());
        r.end().unwrap();
        // 300 octets : forme longue sur deux octets.
        let data = vec![0xBBu8; 300];
        let encoded = write::octet_string(&data);
        assert_eq!(encoded.get(..4), Some([0x04, 0x82, 0x01, 0x2C].as_slice()));
        assert_eq!(Reader::new(&encoded).octet_string().unwrap().len(), 300);
    }

    #[test]
    fn indefinite_length_is_refused() {
        // SEQUENCE en longueur indéfinie, terminée par 00 00 : légal en BER.
        let ber = [0x30u8, 0x80, 0x02, 0x01, 0x01, 0x00, 0x00];
        let err = Reader::new(&ber).read().unwrap_err();
        assert!(format!("{err}").contains("indéfinie"), "{err}");
    }

    #[test]
    fn non_minimal_length_is_refused() {
        // Longueur 1 codée en forme longue.
        let bad = [0x02u8, 0x81, 0x01, 0x05];
        assert!(Reader::new(&bad).read().is_err());
        // Longueur avec zéro de tête.
        let bad = [0x02u8, 0x82, 0x00, 0x01, 0x05];
        assert!(Reader::new(&bad).read().is_err());
    }

    #[test]
    fn integers_minimal_and_signed() {
        assert_eq!(write::integer_i64(0), vec![0x02, 0x01, 0x00]);
        assert_eq!(write::integer_i64(127), vec![0x02, 0x01, 0x7F]);
        assert_eq!(write::integer_i64(128), vec![0x02, 0x02, 0x00, 0x80]);
        assert_eq!(write::integer_i64(-1), vec![0x02, 0x01, 0xFF]);
        assert_eq!(write::integer_i64(-128), vec![0x02, 0x01, 0x80]);
        assert_eq!(write::integer_i64(-129), vec![0x02, 0x02, 0xFF, 0x7F]);
        for v in [
            0i64,
            1,
            -1,
            127,
            128,
            -128,
            -129,
            i64::MAX,
            i64::MIN,
            65_535,
        ] {
            let encoded = write::integer_i64(v);
            assert_eq!(Reader::new(&encoded).integer_i64().unwrap(), v, "{v}");
        }
        // Entier non minimal refusé.
        assert!(Reader::new(&[0x02u8, 0x02, 0x00, 0x01])
            .integer_i64()
            .is_err());
        assert!(Reader::new(&[0x02u8, 0x02, 0xFF, 0xFF])
            .integer_i64()
            .is_err());
        assert!(Reader::new(&[0x02u8, 0x00]).integer_i64().is_err());
    }

    #[test]
    fn unsigned_integer_adds_the_sign_byte() {
        let encoded = write::unsigned_integer(&[0xFF, 0x01]);
        assert_eq!(encoded, vec![0x02, 0x03, 0x00, 0xFF, 0x01]);
        assert_eq!(
            Reader::new(&encoded).unsigned_bytes().unwrap(),
            [0xFF, 0x01].as_slice()
        );
        assert_eq!(write::unsigned_integer(&[0, 0, 0]), vec![0x02, 0x01, 0x00]);
        // Un entier négatif n'est pas un module RSA.
        assert!(Reader::new(&[0x02u8, 0x01, 0x80]).unsigned_bytes().is_err());
    }

    #[test]
    fn oid_multi_byte_arcs() {
        // sha256WithRSAEncryption = 1.2.840.113549.1.1.11 : les arcs 840 et
        // 113549 tiennent sur plusieurs octets.
        let oid = Oid::parse("1.2.840.113549.1.1.11").unwrap();
        let encoded = write::oid(&oid);
        assert_eq!(
            encoded,
            vec![0x06, 0x09, 0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0B]
        );
        assert_eq!(Reader::new(&encoded).oid().unwrap(), oid);
        assert_eq!(format!("{oid}"), "1.2.840.113549.1.1.11");
        // Arc de tête 2.100.3 (cas du §8.19.4 où le premier octet dépasse 80).
        let big = Oid::parse("2.100.3").unwrap();
        let encoded = write::oid(&big);
        assert_eq!(encoded, vec![0x06, 0x03, 0x81, 0x34, 0x03]);
        assert_eq!(Reader::new(&encoded).oid().unwrap(), big);
        // Arc non minimal (0x80 de tête) refusé.
        assert!(Reader::new(&[0x06u8, 0x03, 0x2A, 0x80, 0x01])
            .oid()
            .is_err());
    }

    #[test]
    fn nested_structures() {
        let inner = write::sequence(&[write::integer_i64(1), write::utf8_string("é")]);
        let outer = write::sequence(&[
            inner.clone(),
            write::set_of(&[write::integer_i64(3), write::integer_i64(2)]),
            write::context(0, &write::boolean(true)),
        ]);
        let mut r = Reader::new(&outer);
        let mut seq = r.sequence().unwrap();
        r.end().unwrap();
        let mut first = seq.sequence().unwrap();
        assert_eq!(first.integer_i64().unwrap(), 1);
        assert_eq!(first.string().unwrap(), "é");
        first.end().unwrap();
        let mut set = seq.set().unwrap();
        // Le SET OF a été trié : 2 avant 3.
        assert_eq!(set.integer_i64().unwrap(), 2);
        assert_eq!(set.integer_i64().unwrap(), 3);
        let mut ctx = seq.context(0).unwrap();
        assert!(ctx.boolean().unwrap());
        seq.end().unwrap();
    }

    #[test]
    fn high_tag_numbers() {
        let encoded = write::tlv(Class::Context, false, 200, &[1, 2, 3]);
        assert_eq!(encoded.get(..3), Some([0x9F, 0x81, 0x48].as_slice()));
        let tlv = Reader::new(&encoded).read().unwrap();
        assert_eq!(tlv.number, 200);
        assert!(tlv.is_context(200));
        // Identifiant de tag non minimal.
        assert!(Reader::new(&[0xBFu8, 0x80, 0x01, 0x00]).read().is_err());
    }

    #[test]
    fn bit_strings() {
        let encoded = write::bit_string(&[0xDE, 0xAD]);
        let (unused, data) = Reader::new(&encoded).bit_string().unwrap();
        assert_eq!((unused, data), (0, [0xDE, 0xAD].as_slice()));
        // digitalSignature + keyCertSign (bits 0 et 5) = 0b10000100, 2 bits utiles.
        let bits = [true, false, false, false, false, true];
        let encoded = write::named_bits(&bits);
        assert_eq!(encoded, vec![0x03, 0x02, 0x02, 0x84]);
        assert!(Reader::new(&[0x03u8, 0x01, 0x03]).bit_string().is_err());
        assert!(Reader::new(&[0x03u8, 0x02, 0x08, 0x00])
            .bit_string()
            .is_err());
    }

    #[test]
    fn booleans_must_be_canonical() {
        assert!(Reader::new(&write::boolean(true)).boolean().unwrap());
        assert!(!Reader::new(&write::boolean(false)).boolean().unwrap());
        assert!(Reader::new(&[0x01u8, 0x01, 0x01]).boolean().is_err());
    }

    #[test]
    fn times() {
        let mut r = Reader::new(b"\x17\x0d990101000000Z");
        let t = r.time().unwrap();
        assert_eq!((t.year, t.month, t.day), (1999, 1, 1));
        let mut r = Reader::new(b"\x17\x0d250630123456Z");
        let t = r.time().unwrap();
        assert_eq!(t.to_display(), "2025-06-30 12:34:56 UTC");
        let mut r = Reader::new(b"\x18\x0f20991231235959Z");
        let t = r.time().unwrap();
        assert_eq!(t.to_display(), "2099-12-31 23:59:59 UTC");
        // Décalage horaire ramené à UTC.
        let mut r = Reader::new(b"\x17\x0f2506301234+0200");
        let t = r.time().unwrap();
        assert_eq!(t.to_display(), "2025-06-30 10:34:00 UTC");
        // Aller-retour par l'écrivain.
        let t = Time {
            year: 2030,
            month: 2,
            day: 28,
            hour: 1,
            minute: 2,
            second: 3,
        };
        assert_eq!(Reader::new(&write::x509_time(t)).time().unwrap(), t);
        let far = Time { year: 2100, ..t };
        assert_eq!(write::x509_time(far).first(), Some(&0x18));
        assert_eq!(Reader::new(&write::x509_time(far)).time().unwrap(), far);
        // Sans fuseau : refusé.
        assert!(Reader::new(b"\x17\x0c250630123456").time().is_err());
    }

    #[test]
    fn unix_epoch_roundtrip() {
        for secs in [0i64, 1, 86_399, 86_400, 1_700_000_000, -1, -86_400] {
            let t = Time::from_unix(secs);
            assert_eq!(t.to_unix(), secs, "{secs}");
        }
        assert_eq!(Time::from_unix(0).to_display(), "1970-01-01 00:00:00 UTC");
    }

    #[test]
    fn trailing_bytes_are_refused() {
        let mut encoded = write::integer_i64(1);
        encoded.push(0x00);
        let mut r = Reader::new(&encoded);
        r.integer_i64().unwrap();
        assert!(r.end().is_err());
    }

    #[test]
    fn truncated_content_is_refused() {
        assert!(Reader::new(&[0x04u8, 0x05, 0x01, 0x02]).read().is_err());
        assert!(Reader::new(&[0x30u8]).read().is_err());
        assert!(Reader::new(&[]).read().is_err());
    }
}
