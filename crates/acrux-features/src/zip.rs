//! Écrivain (et lecteur minimal) d'archives ZIP, écrit de zéro d'après la
//! spécification *.ZIP File Format Specification* de PKWARE (APPNOTE 6.3.x).
//!
//! Il sert aux formats de bureautique d'`export` : un `.docx` comme un `.xlsx`
//! est une archive ZIP contenant des fichiers XML. On n'implémente que ce que
//! ces formats exigent :
//!
//! - en-tête local (§4.3.7), répertoire central (§4.3.12), fin de répertoire
//!   central (§4.3.16) ;
//! - méthodes 0 (stocké) et 8 (deflate, via le compresseur maison
//!   [`acrux_codecs::flate::compress`] débarrassé de son enveloppe zlib) ;
//! - CRC-32 (même polynôme que PNG, réutilisé depuis `acrux_graphics::png`) ;
//! - noms en UTF-8 signalés par le bit 11 des drapeaux généraux (§4.4.4).
//!
//! Non couvert : chiffrement, ZIP64, archives multi-volumes, descripteurs de
//! données. Les fichiers produits restent donc sous les 4 Gio.

use acrux_core::{Error, Result};
use acrux_graphics::png::crc32;

/// Signature d'un en-tête local (§4.3.7).
const LOCAL_SIGNATURE: u32 = 0x0403_4B50;
/// Signature d'une entrée du répertoire central (§4.3.12).
const CENTRAL_SIGNATURE: u32 = 0x0201_4B50;
/// Signature de la fin de répertoire central (§4.3.16).
const END_SIGNATURE: u32 = 0x0605_4B50;
/// Drapeau « nom et commentaire en UTF-8 » (§4.4.4, bit 11).
const FLAG_UTF8: u16 = 1 << 11;
/// Version nécessaire pour lire du deflate (2.0).
const VERSION: u16 = 20;
/// Date MS-DOS du 1ᵉʳ janvier 1980 : la sortie ne dépend pas de l'horloge.
const DOS_DATE: u16 = (1 << 5) | 1;
/// Heure MS-DOS correspondante (minuit).
const DOS_TIME: u16 = 0;
/// Niveau de compression demandé au compresseur DEFLATE maison.
const LEVEL: u8 = 6;

/// Entrée enregistrée pour le répertoire central.
struct Entry {
    name: String,
    method: u16,
    crc: u32,
    compressed: u32,
    uncompressed: u32,
    offset: u32,
}

/// Construit une archive ZIP en mémoire.
///
/// ```
/// use acrux_features::zip::{read, ZipWriter};
///
/// let mut zip = ZipWriter::new();
/// zip.add("mots.txt", b"bonjour bonjour bonjour bonjour");
/// let archive = zip.finish();
/// let entries = read(&archive).unwrap();
/// assert_eq!(entries[0].name, "mots.txt");
/// assert_eq!(entries[0].data, b"bonjour bonjour bonjour bonjour");
/// ```
#[derive(Default)]
pub struct ZipWriter {
    out: Vec<u8>,
    entries: Vec<Entry>,
}

impl ZipWriter {
    /// Archive vide.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ajoute un fichier compressé en deflate (méthode 8), ou stocké si la
    /// compression ne gagne rien.
    pub fn add(&mut self, name: &str, data: &[u8]) {
        // `compress` produit une enveloppe zlib : 2 octets d'en-tête et 4
        // d'Adler-32 que le ZIP n'attend pas (il a son propre CRC-32).
        let zlib = acrux_codecs::flate::compress(data, LEVEL);
        let deflated = zlib
            .get(2..zlib.len().saturating_sub(4))
            .unwrap_or(&[])
            .to_vec();
        if deflated.is_empty() || deflated.len() >= data.len() {
            self.push(name, 0, data, data);
        } else {
            self.push(name, 8, data, &deflated);
        }
    }

    /// Ajoute un fichier sans compression (méthode 0) : utile pour les
    /// contenus déjà compressés (JPEG, PNG).
    pub fn add_stored(&mut self, name: &str, data: &[u8]) {
        self.push(name, 0, data, data);
    }

    /// Écrit l'en-tête local et les données, puis note l'entrée.
    fn push(&mut self, name: &str, method: u16, raw: &[u8], payload: &[u8]) {
        let offset = u32::try_from(self.out.len()).unwrap_or(u32::MAX);
        let crc = crc32(raw);
        let compressed = u32::try_from(payload.len()).unwrap_or(u32::MAX);
        let uncompressed = u32::try_from(raw.len()).unwrap_or(u32::MAX);
        let name_bytes = name.as_bytes();
        self.out.extend_from_slice(&LOCAL_SIGNATURE.to_le_bytes());
        self.out.extend_from_slice(&VERSION.to_le_bytes());
        self.out.extend_from_slice(&FLAG_UTF8.to_le_bytes());
        self.out.extend_from_slice(&method.to_le_bytes());
        self.out.extend_from_slice(&DOS_TIME.to_le_bytes());
        self.out.extend_from_slice(&DOS_DATE.to_le_bytes());
        self.out.extend_from_slice(&crc.to_le_bytes());
        self.out.extend_from_slice(&compressed.to_le_bytes());
        self.out.extend_from_slice(&uncompressed.to_le_bytes());
        self.out
            .extend_from_slice(&u16::try_from(name_bytes.len()).unwrap_or(0).to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes()); // pas de champ « extra »
        self.out.extend_from_slice(name_bytes);
        self.out.extend_from_slice(payload);
        self.entries.push(Entry {
            name: name.to_string(),
            method,
            crc,
            compressed,
            uncompressed,
            offset,
        });
    }

    /// Termine l'archive : répertoire central puis fin de répertoire.
    #[must_use]
    pub fn finish(mut self) -> Vec<u8> {
        let start = u32::try_from(self.out.len()).unwrap_or(u32::MAX);
        for e in &self.entries {
            let name = e.name.as_bytes();
            self.out.extend_from_slice(&CENTRAL_SIGNATURE.to_le_bytes());
            self.out.extend_from_slice(&VERSION.to_le_bytes()); // version d'écriture
            self.out.extend_from_slice(&VERSION.to_le_bytes()); // version nécessaire
            self.out.extend_from_slice(&FLAG_UTF8.to_le_bytes());
            self.out.extend_from_slice(&e.method.to_le_bytes());
            self.out.extend_from_slice(&DOS_TIME.to_le_bytes());
            self.out.extend_from_slice(&DOS_DATE.to_le_bytes());
            self.out.extend_from_slice(&e.crc.to_le_bytes());
            self.out.extend_from_slice(&e.compressed.to_le_bytes());
            self.out.extend_from_slice(&e.uncompressed.to_le_bytes());
            self.out
                .extend_from_slice(&u16::try_from(name.len()).unwrap_or(0).to_le_bytes());
            self.out.extend_from_slice(&0u16.to_le_bytes()); // extra
            self.out.extend_from_slice(&0u16.to_le_bytes()); // commentaire
            self.out.extend_from_slice(&0u16.to_le_bytes()); // disque de début
            self.out.extend_from_slice(&0u16.to_le_bytes()); // attributs internes
            self.out.extend_from_slice(&0u32.to_le_bytes()); // attributs externes
            self.out.extend_from_slice(&e.offset.to_le_bytes());
            self.out.extend_from_slice(name);
        }
        let end = u32::try_from(self.out.len()).unwrap_or(u32::MAX);
        let count = u16::try_from(self.entries.len()).unwrap_or(u16::MAX);
        self.out.extend_from_slice(&END_SIGNATURE.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes()); // numéro de disque
        self.out.extend_from_slice(&0u16.to_le_bytes()); // disque du répertoire
        self.out.extend_from_slice(&count.to_le_bytes());
        self.out.extend_from_slice(&count.to_le_bytes());
        self.out
            .extend_from_slice(&end.saturating_sub(start).to_le_bytes());
        self.out.extend_from_slice(&start.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes()); // commentaire d'archive
        self.out
    }
}

/// Fichier lu dans une archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    /// Chemin dans l'archive (séparateur `/`).
    pub name: String,
    /// Contenu décompressé.
    pub data: Vec<u8>,
}

/// Lit une archive ZIP en parcourant son répertoire central.
///
/// Sert aux tests et à la relecture des documents produits ; les entrées
/// chiffrées, ZIP64 ou compressées autrement qu'en méthode 0 ou 8 sont
/// signalées comme non prises en charge.
///
/// # Errors
///
/// `Error::Corrupt` si la fin de répertoire central est absente ou si une
/// entrée est tronquée ; `Error::Unsupported` pour une méthode inconnue.
pub fn read(zip: &[u8]) -> Result<Vec<ZipEntry>> {
    let u16_at = |p: usize| -> u16 {
        u16::from_le_bytes([
            zip.get(p).copied().unwrap_or(0),
            zip.get(p + 1).copied().unwrap_or(0),
        ])
    };
    let u32_at = |p: usize| -> u32 {
        u32::from_le_bytes([
            zip.get(p).copied().unwrap_or(0),
            zip.get(p + 1).copied().unwrap_or(0),
            zip.get(p + 2).copied().unwrap_or(0),
            zip.get(p + 3).copied().unwrap_or(0),
        ])
    };
    // Fin de répertoire central : recherchée depuis la fin (§4.3.16).
    let end = (0..zip.len().saturating_sub(21))
        .rev()
        .find(|&p| u32_at(p) == END_SIGNATURE)
        .ok_or_else(|| Error::Corrupt("ZIP : fin de répertoire central absente".into()))?;
    let count = usize::from(u16_at(end + 10));
    let mut pos = u32_at(end + 16) as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if u32_at(pos) != CENTRAL_SIGNATURE {
            return Err(Error::Corrupt("ZIP : répertoire central tronqué".into()));
        }
        let method = u16_at(pos + 10);
        let compressed = u32_at(pos + 20) as usize;
        let name_len = usize::from(u16_at(pos + 28));
        let extra_len = usize::from(u16_at(pos + 30));
        let comment_len = usize::from(u16_at(pos + 32));
        let local = u32_at(pos + 42) as usize;
        let name = String::from_utf8_lossy(
            zip.get(pos + 46..pos + 46 + name_len)
                .ok_or_else(|| Error::Corrupt("ZIP : nom tronqué".into()))?,
        )
        .into_owned();
        // L'en-tête local redonne les longueurs de nom et d'extra (§4.3.7).
        if u32_at(local) != LOCAL_SIGNATURE {
            return Err(Error::Corrupt(format!("ZIP : en-tête local de {name}")));
        }
        let data_start =
            local + 30 + usize::from(u16_at(local + 26)) + usize::from(u16_at(local + 28));
        let payload = zip
            .get(data_start..data_start + compressed)
            .ok_or_else(|| Error::Corrupt(format!("ZIP : données de {name} tronquées")))?;
        let data = match method {
            0 => payload.to_vec(),
            8 => acrux_codecs::flate::decode(payload)?,
            other => {
                return Err(Error::Unsupported(format!(
                    "ZIP : méthode de compression {other}"
                )))
            }
        };
        if crc32(&data) != u32_at(pos + 16) {
            return Err(Error::Corrupt(format!("ZIP : CRC-32 de {name}")));
        }
        out.push(ZipEntry { name, data });
        pos += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_stored_and_deflated() {
        let mut zip = ZipWriter::new();
        let repetitif = "abcabcabc".repeat(500);
        zip.add("mimetype", b"x"); // trop court : stocké
        zip.add("word/document.xml", repetitif.as_bytes());
        zip.add_stored("word/media/image1.png", &[0x89, b'P', b'N', b'G']);
        zip.add("dossier/vide.txt", b"");
        let archive = zip.finish();
        let entries = read(&archive).unwrap();
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].name, "mimetype");
        assert_eq!(entries[0].data, b"x");
        assert_eq!(entries[1].name, "word/document.xml");
        assert_eq!(entries[1].data, repetitif.as_bytes());
        assert_eq!(entries[2].data, vec![0x89, b'P', b'N', b'G']);
        assert!(entries[3].data.is_empty());
        // Le long contenu répétitif doit avoir été réellement compressé.
        assert!(
            archive.len() < repetitif.len() / 2,
            "archive de {} octets pour {} de données",
            archive.len(),
            repetitif.len()
        );
    }

    #[test]
    fn utf8_names_and_signatures() {
        let mut zip = ZipWriter::new();
        zip.add("dossier/été à Noël.txt", "caractères accentués".as_bytes());
        let archive = zip.finish();
        assert_eq!(&archive[..4], &LOCAL_SIGNATURE.to_le_bytes());
        assert_eq!(
            u16::from_le_bytes([archive[6], archive[7]]) & FLAG_UTF8,
            FLAG_UTF8,
            "bit 11 : nom UTF-8"
        );
        let entries = read(&archive).unwrap();
        assert_eq!(entries[0].name, "dossier/été à Noël.txt");
    }

    #[test]
    fn empty_archive_is_valid() {
        let archive = ZipWriter::new().finish();
        assert_eq!(archive.len(), 22);
        assert!(read(&archive).unwrap().is_empty());
    }

    #[test]
    fn corrupt_archives_are_reported_not_panicking() {
        assert!(read(&[]).is_err());
        assert!(read(b"PK\x05\x06 pas assez d'octets").is_err());
        let mut zip = ZipWriter::new();
        zip.add(
            "a.txt",
            b"contenu quelconque assez long pour compresser un peu",
        );
        let mut archive = zip.finish();
        // Altère un octet de données : le CRC doit le signaler.
        archive[40] ^= 0xFF;
        assert!(read(&archive).is_err());
    }
}
