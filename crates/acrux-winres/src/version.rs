//! Construction du bloc `VS_VERSIONINFO`.
//!
//! C'est ce bloc que l'Explorateur lit pour remplir l'onglet « Détails » des
//! propriétés d'un fichier, et que le gestionnaire des tâches affiche à côté
//! d'un processus. Sa forme est une petite structure récursive : un nœud porte
//! une clé, une valeur facultative, puis ses enfants, chacun aligné sur quatre
//! octets.

/// Page de codes des chaînes : 1200, c'est-à-dire UTF-16.
const CODEPAGE_UNICODE: u16 = 0x04B0;

/// Informations de version d'un exécutable.
pub struct VersionInfo<'a> {
    /// Version du fichier, en quatre nombres (majeur, mineur, correctif, build).
    pub file_version: [u16; 4],
    /// Version du produit, dans le même format.
    pub product_version: [u16; 4],
    /// Identifiant de langue des chaînes, par exemple `0x040C` pour le français.
    pub language: u16,
    /// Paires clé/valeur affichées par l'Explorateur, dans l'ordre d'affichage.
    ///
    /// Les clés reconnues par Windows sont notamment `CompanyName`,
    /// `FileDescription`, `FileVersion`, `InternalName`, `LegalCopyright`,
    /// `OriginalFilename`, `ProductName` et `ProductVersion`.
    pub strings: &'a [(&'a str, &'a str)],
}

impl VersionInfo<'_> {
    /// Sérialise le bloc tel qu'il doit figurer dans la ressource `RT_VERSION`.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let table_key = format!("{:04x}{CODEPAGE_UNICODE:04x}", self.language);
        let entries: Vec<Vec<u8>> = self
            .strings
            .iter()
            .map(|(key, value)| {
                let text = utf16z(value);
                // Pour une valeur textuelle, la longueur s'exprime en caractères.
                let chars = u16::try_from(text.len() / 2).unwrap_or(u16::MAX);
                node(key, &text, chars, 1, &[])
            })
            .collect();
        let table = node(&table_key, &[], 0, 1, &entries);
        let string_file_info = node("StringFileInfo", &[], 0, 1, &[table]);

        let mut translation = Vec::with_capacity(4);
        translation.extend_from_slice(&self.language.to_le_bytes());
        translation.extend_from_slice(&CODEPAGE_UNICODE.to_le_bytes());
        let var = node("Translation", &translation, 4, 0, &[]);
        let var_file_info = node("VarFileInfo", &[], 0, 1, &[var]);

        let fixed = self.fixed_info();
        let len = u16::try_from(fixed.len()).unwrap_or(0);
        node(
            "VS_VERSION_INFO",
            &fixed,
            len,
            0,
            &[string_file_info, var_file_info],
        )
    }

    /// Construit la structure `VS_FIXEDFILEINFO` de 52 octets.
    fn fixed_info(&self) -> Vec<u8> {
        let pack = |v: [u16; 4]| {
            (
                (u32::from(v[0]) << 16) | u32::from(v[1]),
                (u32::from(v[2]) << 16) | u32::from(v[3]),
            )
        };
        let (file_high, file_low) = pack(self.file_version);
        let (product_high, product_low) = pack(self.product_version);
        let fields: [u32; 13] = [
            0xFEEF_04BD, // signature
            0x0001_0000, // version de la structure
            file_high,
            file_low,
            product_high,
            product_low,
            0x0000_003F, // drapeaux significatifs
            0,           // drapeaux (ni debug, ni préversion)
            0x0000_0004, // VOS__WINDOWS32
            0x0000_0001, // VFT_APP
            0,           // pas de sous-type
            0,           // date, moitié haute
            0,           // date, moitié basse
        ];
        let mut out = Vec::with_capacity(52);
        for field in fields {
            out.extend_from_slice(&field.to_le_bytes());
        }
        out
    }
}

/// Assemble un nœud du bloc de version.
///
/// `value_len` vaut le nombre de caractères pour une valeur textuelle
/// (`kind` = 1) et le nombre d'octets pour une valeur binaire (`kind` = 0) ;
/// c'est la convention, surprenante mais bien réelle, du format.
fn node(key: &str, value: &[u8], value_len: u16, kind: u16, children: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + value.len());
    out.extend_from_slice(&0u16.to_le_bytes()); // longueur, écrite à la fin
    out.extend_from_slice(&value_len.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&utf16z(key));
    pad4(&mut out);
    out.extend_from_slice(value);
    for child in children {
        pad4(&mut out);
        out.extend_from_slice(child);
    }
    // La longueur couvre le nœud et ses enfants, mais pas le remplissage que
    // le parent ajoutera derrière.
    let len = u16::try_from(out.len()).unwrap_or(u16::MAX);
    out[0..2].copy_from_slice(&len.to_le_bytes());
    out
}

/// Encode une chaîne en UTF-16 petit-boutiste, terminée par un zéro.
fn utf16z(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 2 + 2);
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Complète avec des zéros jusqu'à la prochaine frontière de quatre octets.
fn pad4(out: &mut Vec<u8>) {
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

#[cfg(test)]
mod tests {
    use super::{node, utf16z, VersionInfo};

    fn exemple() -> VersionInfo<'static> {
        VersionInfo {
            file_version: [1, 2, 3, 4],
            product_version: [1, 2, 0, 0],
            language: 0x040C,
            strings: &[("ProductName", "Acrux"), ("FileVersion", "1.2.3.4")],
        }
    }

    #[test]
    fn la_longueur_annoncee_est_la_longueur_reelle() {
        let bytes = exemple().to_bytes();
        let annonce = usize::from(u16::from_le_bytes([bytes[0], bytes[1]]));
        assert_eq!(annonce, bytes.len());
    }

    #[test]
    fn len_tete_decrit_la_structure_fixe() {
        let bytes = exemple().to_bytes();
        // Valeur binaire de 52 octets, type 0, clé « VS_VERSION_INFO ».
        assert_eq!(u16::from_le_bytes([bytes[2], bytes[3]]), 52);
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 0);
        assert_eq!(&bytes[6..6 + 32], &utf16z("VS_VERSION_INFO")[..]);
        // La signature suit, une fois aligné sur quatre octets.
        let at = 6 + 32 + 2;
        assert_eq!(
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]),
            0xFEEF_04BD
        );
    }

    #[test]
    fn les_versions_sont_empaquetees_par_moities() {
        let bytes = exemple().to_bytes();
        let at = 6 + 32 + 2 + 8;
        assert_eq!(
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]),
            0x0001_0002
        );
        assert_eq!(
            u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]]),
            0x0003_0004
        );
    }

    #[test]
    fn la_table_porte_la_langue_et_la_page_de_codes() {
        let bytes = exemple().to_bytes();
        let cle = utf16z("040c04b0");
        assert!(bytes.windows(cle.len()).any(|w| w == cle));
        let nom = utf16z("Acrux");
        assert!(bytes.windows(nom.len()).any(|w| w == nom));
    }

    #[test]
    fn un_noeud_sans_enfant_tient_dans_sa_len_tete() {
        let feuille = node("A", &[], 0, 1, &[]);
        // 6 octets d'en-tete, 4 pour la cle, puis 2 de remplissage :
        // le format aligne la valeur sur quatre octets, meme vide.
        assert_eq!(feuille.len(), 12);
        assert_eq!(u16::from_le_bytes([feuille[0], feuille[1]]), 12);
    }
}
