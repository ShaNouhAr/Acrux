//! Archive collée à la fin de l'exécutable.
//!
//! L'installateur est un **exécutable auto-extractible** : le programme
//! d'abord, puis l'archive, puis huit octets de longueur et une signature.
//! Il se relit lui-même pour retrouver sa charge, ce qui donne un installateur
//! d'un seul fichier — rien à décompresser, rien à installer pour installer.
//!
//! ```text
//! ┌────────────────────┬──────────┬──────────────┬────────────┐
//! │ programme (stub)   │ archive  │ longueur u64 │ ACRUXPKG   │
//! └────────────────────┴──────────┴──────────────┴────────────┘
//! ```
//!
//! Le format de l'archive est le plus simple qui tienne debout : un compte,
//! puis pour chaque fichier son nom, sa taille décompressée et ses octets
//! compressés en Flate — par notre propre compresseur ([`acrux_codecs`]).

use std::path::Path;

/// Signature des huit derniers octets d'un installateur chargé.
const MAGIC: &[u8; 8] = b"ACRUXPKG";

/// Un fichier de l'archive.
pub struct Entry {
    /// Chemin relatif au dossier d'installation, séparé par des barres.
    pub name: String,
    /// Contenu décompressé.
    pub data: Vec<u8>,
}

/// Assemble une archive depuis des fichiers du disque.
///
/// # Errors
/// Fichier illisible.
pub fn pack(files: &[(String, std::path::PathBuf)]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let count = u32::try_from(files.len()).unwrap_or(0);
    out.extend_from_slice(&count.to_le_bytes());
    for (name, path) in files {
        let data = std::fs::read(path).map_err(|e| format!("{} : {e}", path.display()))?;
        let compressed = acrux_codecs::flate::compress(&data, 9);
        let name_bytes = name.as_bytes();
        out.extend_from_slice(&u16::try_from(name_bytes.len()).unwrap_or(0).to_le_bytes());
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(&u64::try_from(data.len()).unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&u64::try_from(compressed.len()).unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&compressed);
    }
    Ok(out)
}

/// Écrit un installateur chargé : le programme, puis l'archive, puis la
/// signature.
///
/// # Errors
/// Lecture du programme ou écriture de la sortie impossible.
pub fn write_self_extracting(stub: &Path, archive: &[u8], out: &Path) -> Result<(), String> {
    let mut bytes = std::fs::read(stub).map_err(|e| format!("{} : {e}", stub.display()))?;
    // Un programme déjà chargé est d'abord allégé de sa charge : recharger
    // deux fois de suite ne doit pas empiler deux archives.
    if let Some(start) = payload_start(&bytes) {
        bytes.truncate(start);
    }
    bytes.extend_from_slice(archive);
    bytes.extend_from_slice(&u64::try_from(archive.len()).unwrap_or(0).to_le_bytes());
    bytes.extend_from_slice(MAGIC);
    std::fs::write(out, &bytes).map_err(|e| format!("{} : {e}", out.display()))
}

/// Position du début de l'archive dans un exécutable chargé.
fn payload_start(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 16 || &bytes[bytes.len() - 8..] != MAGIC {
        return None;
    }
    let at = bytes.len() - 16;
    let length = u64::from_le_bytes(bytes[at..at + 8].try_into().ok()?);
    let length = usize::try_from(length).ok()?;
    at.checked_sub(length)
}

/// Lit l'archive collée à l'exécutable en cours.
///
/// Rend `None` si le programme n'a pas été chargé — c'est le cas du binaire
/// tel qu'il sort du compilateur, qui sait alors seulement charger.
#[must_use]
pub fn read_self() -> Option<Vec<Entry>> {
    let path = std::env::current_exe().ok()?;
    let bytes = std::fs::read(path).ok()?;
    let start = payload_start(&bytes)?;
    let end = bytes.len() - 16;
    unpack(&bytes[start..end]).ok()
}

/// Décode une archive.
///
/// # Errors
/// Archive tronquée ou flux compressé illisible.
pub fn unpack(archive: &[u8]) -> Result<Vec<Entry>, String> {
    let mut at = 0usize;
    let count = read_u32(archive, &mut at)?;
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name_len = usize::from(read_u16(archive, &mut at)?);
        let name = archive
            .get(at..at + name_len)
            .ok_or_else(|| String::from("archive tronquée"))?;
        at += name_len;
        let plain_len = usize::try_from(read_u64(archive, &mut at)?).unwrap_or(0);
        let packed_len = usize::try_from(read_u64(archive, &mut at)?).unwrap_or(0);
        let packed = archive
            .get(at..at + packed_len)
            .ok_or_else(|| String::from("archive tronquée"))?;
        at += packed_len;
        let data = acrux_codecs::flate::decode(packed)
            .map_err(|e| format!("archive : flux illisible ({e})"))?;
        if data.len() != plain_len {
            return Err(String::from(
                "archive : taille inattendue après décompression",
            ));
        }
        out.push(Entry {
            name: String::from_utf8_lossy(name).into_owned(),
            data,
        });
    }
    Ok(out)
}

fn read_u16(data: &[u8], at: &mut usize) -> Result<u16, String> {
    let slice = data
        .get(*at..*at + 2)
        .ok_or_else(|| String::from("archive tronquée"))?;
    *at += 2;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32(data: &[u8], at: &mut usize) -> Result<u32, String> {
    let slice = data
        .get(*at..*at + 4)
        .ok_or_else(|| String::from("archive tronquée"))?;
    *at += 4;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_u64(data: &[u8], at: &mut usize) -> Result<u64, String> {
    let slice = data
        .get(*at..*at + 8)
        .ok_or_else(|| String::from("archive tronquée"))?;
    *at += 8;
    let array: [u8; 8] = slice.try_into().map_err(|_| String::from("archive"))?;
    Ok(u64::from_le_bytes(array))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{pack, unpack};

    #[test]
    fn une_archive_fait_laller_retour() {
        let dir = std::env::temp_dir().join("acrux-setup-test");
        std::fs::create_dir_all(&dir).expect("dossier");
        let a = dir.join("a.bin");
        let b = dir.join("b.bin");
        std::fs::write(&a, vec![7u8; 5000]).expect("a");
        std::fs::write(&b, b"bonjour").expect("b");
        let archive = pack(&[
            ("acrux.exe".into(), a.clone()),
            ("docs/lisez-moi.txt".into(), b.clone()),
        ])
        .expect("archive");
        // Cinq mille octets identiques se compriment très fort : l'archive
        // doit être bien plus petite que la somme des fichiers.
        assert!(archive.len() < 2000, "archive de {} octets", archive.len());

        let entries = unpack(&archive).expect("relecture");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "acrux.exe");
        assert_eq!(entries[0].data, vec![7u8; 5000]);
        assert_eq!(entries[1].name, "docs/lisez-moi.txt");
        assert_eq!(entries[1].data, b"bonjour");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn une_archive_tronquee_est_refusee() {
        let dir = std::env::temp_dir().join("acrux-setup-test2");
        std::fs::create_dir_all(&dir).expect("dossier");
        let a = dir.join("a.bin");
        std::fs::write(&a, b"contenu").expect("a");
        let archive = pack(&[("a".into(), a)]).expect("archive");
        for cut in [1, archive.len() / 2, archive.len() - 1] {
            assert!(unpack(&archive[..cut]).is_err(), "coupe à {cut}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
