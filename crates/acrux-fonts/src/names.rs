//! Table `name` d'une police OpenType : les noms sous lesquels elle se
//! présente.
//!
//! C'est par là qu'un lecteur retrouve une police installée. Un PDF écrit
//! `/BaseFont /Calibri-Bold` ; le système, lui, range un fichier
//! `calibrib.ttf` dont rien dans le nom de fichier ne garantit le contenu.
//! Deviner le nom du fichier marche sur Windows et nulle part ailleurs — macOS
//! empile ses polices dans des recueils `.ttc` aux noms arbitraires, les
//! distributions Linux les éparpillent par famille. Le seul lien fiable entre
//! ce que demande le document et ce qu'il y a sur le disque est cette table.
//!
//! On lit ici cinq noms (§ « name » d'OpenType) :
//!
//! | ID | Nom | Exemple |
//! |---|---|---|
//! | 1 | famille | `Calibri` |
//! | 2 | sous-famille | `Bold` |
//! | 4 | nom complet | `Calibri Bold` |
//! | 6 | nom PostScript | `Calibri-Bold` |
//! | 16, 17 | famille et sous-famille typographiques | `Calibri`, `Bold` |
//!
//! Les identifiants 16 et 17 priment quand ils existent : ils décrivent la
//! famille telle que le dessinateur la conçoit, là où 1 et 2 sont contraints
//! par le vieux modèle « régulier, gras, italique, gras italique » de Windows.

use crate::reader::Reader;

/// Les noms d'une police.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Names {
    /// Famille (ID 1).
    pub family: Option<String>,
    /// Sous-famille (ID 2) : `Regular`, `Bold`, `Italic`…
    pub subfamily: Option<String>,
    /// Nom complet (ID 4).
    pub full: Option<String>,
    /// Nom PostScript (ID 6) : celui qu'un PDF écrit dans `/BaseFont`.
    pub postscript: Option<String>,
    /// Famille typographique (ID 16), si elle diffère de la famille.
    pub typographic_family: Option<String>,
    /// Sous-famille typographique (ID 17).
    pub typographic_subfamily: Option<String>,
}

impl Names {
    /// Famille à employer : la typographique si elle existe.
    #[must_use]
    pub fn best_family(&self) -> Option<&str> {
        self.typographic_family
            .as_deref()
            .or(self.family.as_deref())
    }

    /// Sous-famille à employer.
    #[must_use]
    pub fn best_subfamily(&self) -> Option<&str> {
        self.typographic_subfamily
            .as_deref()
            .or(self.subfamily.as_deref())
    }
}

/// Lit une table `name` déjà extraite du fichier.
///
/// Ne rend jamais d'erreur : une table tronquée donne simplement moins de
/// noms. Un index de polices se construit sur des centaines de fichiers, dont
/// certains sont abîmés ; en refuser un ne doit pas coûter les autres.
#[must_use]
pub fn parse(table: &[u8]) -> Names {
    let mut r = Reader::new(table);
    let _format = r.read_u16().unwrap_or(0);
    let count = r.read_u16().unwrap_or(0);
    let storage = usize::from(r.read_u16().unwrap_or(0));
    let mut out = Names::default();
    // Meilleure qualité rencontrée pour chaque identifiant, pour ne pas
    // remplacer un nom anglais Unicode par un nom coréen MacRoman.
    let mut quality = [0u8; 18];
    for _ in 0..count {
        let platform = r.read_u16().unwrap_or(0);
        let encoding = r.read_u16().unwrap_or(0);
        let language = r.read_u16().unwrap_or(0);
        let name_id = r.read_u16().unwrap_or(0);
        let length = usize::from(r.read_u16().unwrap_or(0));
        let offset = usize::from(r.read_u16().unwrap_or(0));
        let Some(slot) = usize::from(name_id).checked_add(0) else {
            continue;
        };
        if slot >= quality.len() {
            continue;
        }
        let rank = rank_of(platform, encoding, language);
        if rank == 0 || rank <= quality[slot] {
            continue;
        }
        let Some(start) = storage.checked_add(offset) else {
            continue;
        };
        let Some(end) = start.checked_add(length) else {
            continue;
        };
        let Some(bytes) = table.get(start..end) else {
            continue;
        };
        let Some(text) = decode(platform, encoding, bytes) else {
            continue;
        };
        quality[slot] = rank;
        match name_id {
            1 => out.family = Some(text),
            2 => out.subfamily = Some(text),
            4 => out.full = Some(text),
            6 => out.postscript = Some(text),
            16 => out.typographic_family = Some(text),
            17 => out.typographic_subfamily = Some(text),
            _ => {}
        }
    }
    out
}

/// Qualité d'un enregistrement : 0 = inutilisable, plus c'est haut mieux
/// c'est. L'anglais Unicode passe avant tout le reste.
fn rank_of(platform: u16, encoding: u16, language: u16) -> u8 {
    match platform {
        // Windows, UTF-16 : `0x0409` est l'anglais des États-Unis.
        3 if encoding == 1 || encoding == 0 || encoding == 10 => {
            if language == 0x0409 {
                4
            } else {
                2
            }
        }
        // Unicode pur.
        0 => 3,
        // Macintosh, Roman : `0` est l'anglais.
        1 if encoding == 0 => {
            if language == 0 {
                3
            } else {
                1
            }
        }
        _ => 0,
    }
}

/// Décode les octets d'un nom selon sa plateforme.
fn decode(platform: u16, encoding: u16, bytes: &[u8]) -> Option<String> {
    let text = if platform == 1 && encoding == 0 {
        // MacRoman : l'ASCII suffit pour un nom de police ; au-delà, on
        // préfère renoncer plutôt que de rendre des caractères faux.
        if bytes.iter().any(|b| *b >= 0x80) {
            return None;
        }
        bytes.iter().map(|b| char::from(*b)).collect()
    } else {
        // UTF-16BE, paires de substitution comprises.
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from(c[0]) << 8 | u16::from(c[1]))
            .collect();
        String::from_utf16(&units).ok()?
    };
    let text = text.trim().to_string();
    // Un nom vide ou démesuré n'en est pas un.
    if text.is_empty() || text.len() > 200 {
        return None;
    }
    Some(text)
}

/// Décrit le style que porte une sous-famille.
///
/// `Bold`, `Italic`, `Oblique`, `Bold Italic`, et les variantes des fonderies
/// (`Demi`, `Heavy`, `Book`). Renvoie `(gras, italique)`.
#[must_use]
pub fn style_of(subfamily: &str) -> (bool, bool) {
    let key = subfamily.to_ascii_lowercase();
    let bold = key.contains("bold")
        || key.contains("black")
        || key.contains("heavy")
        || key.contains("semibold")
        || key.contains("demibold");
    let italic = key.contains("italic") || key.contains("oblique");
    (bold, italic)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fabrique une table `name` minimale.
    fn table(records: &[(u16, u16, u16, u16, &str)]) -> Vec<u8> {
        let mut head = Vec::new();
        let mut storage = Vec::new();
        head.extend_from_slice(&0u16.to_be_bytes());
        head.extend_from_slice(&u16::try_from(records.len()).unwrap_or(0).to_be_bytes());
        let storage_offset = 6 + 12 * records.len();
        head.extend_from_slice(&u16::try_from(storage_offset).unwrap_or(0).to_be_bytes());
        for (platform, encoding, language, name_id, text) in records {
            let bytes: Vec<u8> = if *platform == 1 {
                text.bytes().collect()
            } else {
                text.encode_utf16().flat_map(u16::to_be_bytes).collect()
            };
            head.extend_from_slice(&platform.to_be_bytes());
            head.extend_from_slice(&encoding.to_be_bytes());
            head.extend_from_slice(&language.to_be_bytes());
            head.extend_from_slice(&name_id.to_be_bytes());
            head.extend_from_slice(&u16::try_from(bytes.len()).unwrap_or(0).to_be_bytes());
            head.extend_from_slice(&u16::try_from(storage.len()).unwrap_or(0).to_be_bytes());
            storage.extend_from_slice(&bytes);
        }
        head.extend_from_slice(&storage);
        head
    }

    #[test]
    fn lit_les_noms_windows() {
        let data = table(&[
            (3, 1, 0x0409, 1, "Calibri"),
            (3, 1, 0x0409, 2, "Bold"),
            (3, 1, 0x0409, 4, "Calibri Bold"),
            (3, 1, 0x0409, 6, "Calibri-Bold"),
        ]);
        let n = parse(&data);
        assert_eq!(n.family.as_deref(), Some("Calibri"));
        assert_eq!(n.subfamily.as_deref(), Some("Bold"));
        assert_eq!(n.full.as_deref(), Some("Calibri Bold"));
        assert_eq!(n.postscript.as_deref(), Some("Calibri-Bold"));
        assert_eq!(n.best_family(), Some("Calibri"));
    }

    #[test]
    fn langlais_prime_sur_les_autres_langues() {
        // Le nom japonais arrive d'abord ; l'anglais doit gagner quand même.
        let data = table(&[
            (3, 1, 0x0411, 1, "\u{30e1}\u{30a4}\u{30ea}\u{30aa}"),
            (3, 1, 0x0409, 1, "Meiryo"),
        ]);
        assert_eq!(parse(&data).family.as_deref(), Some("Meiryo"));
    }

    #[test]
    fn la_famille_typographique_prime() {
        let data = table(&[
            (3, 1, 0x0409, 1, "Segoe UI Semibold"),
            (3, 1, 0x0409, 2, "Regular"),
            (3, 1, 0x0409, 16, "Segoe UI"),
            (3, 1, 0x0409, 17, "Semibold"),
        ]);
        let n = parse(&data);
        assert_eq!(n.best_family(), Some("Segoe UI"));
        assert_eq!(n.best_subfamily(), Some("Semibold"));
    }

    #[test]
    fn une_table_abimee_ne_fait_pas_tomber_lindex() {
        assert_eq!(parse(&[]), Names::default());
        assert_eq!(parse(&[0, 0, 0, 5]), Names::default());
        // Décalage de stockage hors des données.
        let mut data = table(&[(3, 1, 0x0409, 1, "Arial")]);
        data.truncate(data.len() - 3);
        let _ = parse(&data);
    }

    #[test]
    fn les_styles_se_lisent_dans_la_sous_famille() {
        assert_eq!(style_of("Regular"), (false, false));
        assert_eq!(style_of("Bold"), (true, false));
        assert_eq!(style_of("Italic"), (false, true));
        assert_eq!(style_of("Bold Italic"), (true, true));
        assert_eq!(style_of("Oblique"), (false, true));
        assert_eq!(style_of("Semibold"), (true, false));
    }
}
