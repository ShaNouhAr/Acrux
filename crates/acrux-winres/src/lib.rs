//! Écriture des ressources Windows d'un exécutable, sans `rc.exe`.
//!
//! Le format `.res` est un simple chaînage d'enregistrements ; l'éditeur de liens
//! MSVC accepte un fichier `.res` directement en entrée. Ce module le fabrique
//! à la main, comme tout le reste du projet, pour ne dépendre ni d'une
//! bibliothèque externe ni d'un outil du SDK Windows qui pourrait manquer.
//!
//! Deux ressources nous intéressent :
//!
//! * l'icône du programme ([`Resources::add_icon`]), c'est-à-dire un groupe
//!   `RT_GROUP_ICON` qui référence une image `RT_ICON` par taille ;
//! * le bloc d'informations de version ([`Resources::add_version`]), que
//!   l'Explorateur affiche dans l'onglet « Détails » des propriétés du fichier.
//!
//! ```
//! use acrux_winres::{Resources, VersionInfo};
//!
//! let mut res = Resources::new();
//! res.add_version(&VersionInfo {
//!     file_version: [1, 0, 0, 0],
//!     product_version: [1, 0, 0, 0],
//!     language: 0x040C,
//!     strings: &[("ProductName", "Acrux")],
//! });
//! assert!(!res.into_bytes().is_empty());
//! ```

mod build;
mod icon;
mod version;

pub use build::{emit, BuildError};
pub use icon::{Error, IconGroup};
pub use version::VersionInfo;

/// Identifiant de type `RT_ICON` (une image d'icône).
const RT_ICON: u16 = 3;
/// Identifiant de type `RT_GROUP_ICON` (le répertoire qui les rassemble).
const RT_GROUP_ICON: u16 = 14;
/// Identifiant de type `RT_VERSION`.
const RT_VERSION: u16 = 16;

/// Langue neutre : la ressource convient à toutes les langues d'interface.
const LANG_NEUTRAL: u16 = 0x0000;

/// Une ressource en attente d'écriture.
struct Entry {
    kind: u16,
    id: u16,
    language: u16,
    memory_flags: u16,
    data: Vec<u8>,
}

/// Collection de ressources à sérialiser en un fichier `.res`.
///
/// Les ressources sont écrites dans l'ordre d'ajout. Windows ne s'en soucie
/// pas, mais cela rend le fichier reproductible d'une compilation à l'autre.
pub struct Resources {
    entries: Vec<Entry>,
    /// Prochain identifiant libre pour les images d'icônes.
    next_icon_id: u16,
    /// Prochain identifiant libre pour les groupes d'icônes.
    next_group_id: u16,
}

impl Default for Resources {
    fn default() -> Self {
        Self::new()
    }
}

impl Resources {
    /// Crée une collection vide.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_icon_id: 1,
            next_group_id: 1,
        }
    }

    /// Ajoute le contenu d'un fichier `.ico` comme icône du programme.
    ///
    /// Le premier groupe ajouté porte l'identifiant 1 : c'est celui que
    /// l'Explorateur retient pour représenter l'exécutable.
    ///
    /// # Errors
    ///
    /// Renvoie [`Error`] si le fichier n'est pas une icône Windows lisible
    /// (signature absente, répertoire tronqué, image hors limites).
    pub fn add_icon(&mut self, ico: &[u8]) -> Result<(), Error> {
        let group = IconGroup::parse(ico)?;
        let first = self.next_icon_id;
        for image in group.images() {
            self.entries.push(Entry {
                kind: RT_ICON,
                id: self.next_icon_id,
                language: LANG_NEUTRAL,
                // MOVEABLE | DISCARDABLE, comme ce qu'écrit rc.exe.
                memory_flags: 0x1010,
                data: image.to_vec(),
            });
            self.next_icon_id = self.next_icon_id.saturating_add(1);
        }
        self.entries.push(Entry {
            kind: RT_GROUP_ICON,
            id: self.next_group_id,
            language: LANG_NEUTRAL,
            // MOVEABLE | PURE | DISCARDABLE.
            memory_flags: 0x1030,
            data: group.directory(first),
        });
        self.next_group_id = self.next_group_id.saturating_add(1);
        Ok(())
    }

    /// Ajoute le bloc d'informations de version (identifiant 1).
    pub fn add_version(&mut self, info: &VersionInfo<'_>) {
        self.entries.push(Entry {
            kind: RT_VERSION,
            id: 1,
            language: info.language,
            // MOVEABLE | PURE.
            memory_flags: 0x0030,
            data: info.to_bytes(),
        });
    }

    /// Sérialise la collection au format `.res`.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        let mut out = Vec::new();
        // Tout fichier .res commence par un enregistrement vide : type 0, nom 0.
        push_header(&mut out, 0, 0, 0, 0, 0);
        for entry in &self.entries {
            let size = u32::try_from(entry.data.len()).unwrap_or(u32::MAX);
            push_header(
                &mut out,
                size,
                entry.kind,
                entry.id,
                entry.memory_flags,
                entry.language,
            );
            out.extend_from_slice(&entry.data);
            pad4(&mut out);
        }
        out
    }
}

/// Écrit l'en-tête de 32 octets qui précède les données d'une ressource.
///
/// Le type et le nom sont toujours des ordinaux, notés `0xFFFF` suivi du
/// numéro ; c'est ce qui fixe la taille de l'en-tête à 32 octets.
fn push_header(out: &mut Vec<u8>, size: u32, kind: u16, id: u16, memory_flags: u16, language: u16) {
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&32u32.to_le_bytes());
    out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // DataVersion
    out.extend_from_slice(&memory_flags.to_le_bytes());
    out.extend_from_slice(&language.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // Version
    out.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
}

/// Complète avec des zéros jusqu'à la prochaine frontière de quatre octets.
fn pad4(out: &mut Vec<u8>) {
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

#[cfg(test)]
mod tests {
    use super::{pad4, Resources, VersionInfo};

    #[test]
    fn fichier_vide_reduit_a_len_tete_nul() {
        let bytes = Resources::new().into_bytes();
        assert_eq!(bytes.len(), 32);
        assert_eq!(&bytes[4..8], &32u32.to_le_bytes());
        assert!(bytes[8..].iter().all(|b| *b == 0 || *b == 0xFF));
    }

    #[test]
    fn chaque_ressource_est_alignee() {
        let mut res = Resources::new();
        res.add_version(&VersionInfo {
            file_version: [0, 0, 1, 0],
            product_version: [0, 0, 1, 0],
            language: 0x040C,
            strings: &[("ProductName", "Acrux")],
        });
        let bytes = res.into_bytes();
        assert_eq!(bytes.len() % 4, 0);
    }

    #[test]
    fn pad4_ne_touche_pas_un_tampon_aligne() {
        let mut buffer = vec![0u8; 8];
        pad4(&mut buffer);
        assert_eq!(buffer.len(), 8);
        buffer.push(1);
        pad4(&mut buffer);
        assert_eq!(buffer.len(), 12);
    }
}
