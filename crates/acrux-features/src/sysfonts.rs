//! Catalogue des polices installées sur la machine.
//!
//! Un traitement de texte propose **toutes** les polices du système, rangées
//! par famille. Celui-ci fait de même : les dossiers de polices sont parcourus
//! une fois, la table `name` de chaque fichier donne sa famille et son style,
//! et l'on obtient une liste alphabétique de familles avec, pour chacune, ses
//! quatre dessins — romain, gras, italique, gras italique — quand ils
//! existent.
//!
//! Le classement suit le modèle de Windows (identifiants 1 et 2 de la table
//! `name`) : « Segoe UI Light » y est une famille à part entière, romaine,
//! et non une graisse de « Segoe UI ». C'est ce que montre Word, et c'est ce
//! qui permet de tout ranger dans quatre cases.
//!
//! Seules les polices à contours TrueType (`glyf`) sont retenues : ce sont
//! celles dont on sait produire un sous-ensemble à incorporer dans le PDF.
//!
//! Le parcours coûte une fraction de seconde — quelques centaines de fichiers
//! à ouvrir — et n'est fait qu'une fois par processus.

use std::path::PathBuf;
use std::sync::OnceLock;

use acrux_fonts::names;
use acrux_fonts::TrueTypeFont;

/// Un fichier de police, et le rang de la police dans une collection `.ttc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceFile {
    /// Chemin du fichier.
    pub path: PathBuf,
    /// Rang dans la collection ; zéro pour un fichier ordinaire.
    pub index: u32,
}

/// Une famille de polices et ses dessins.
#[derive(Debug, Clone, Default)]
pub struct Family {
    /// Nom de la famille, tel que le système le montre.
    pub name: String,
    /// Romain, gras, italique, gras italique.
    faces: [Option<FaceFile>; 4],
}

/// Case d'un style dans [`Family::faces`].
fn slot(bold: bool, italic: bool) -> usize {
    usize::from(bold) + 2 * usize::from(italic)
}

impl Family {
    /// Le fichier du style demandé, ou le plus proche : un gras absent se
    /// rabat sur le romain plutôt que de ne rien rendre.
    #[must_use]
    pub fn face(&self, bold: bool, italic: bool) -> Option<&FaceFile> {
        [
            slot(bold, italic),
            slot(false, italic),
            slot(bold, false),
            slot(false, false),
        ]
        .into_iter()
        .find_map(|i| self.faces[i].as_ref())
        .or_else(|| self.faces.iter().flatten().next())
    }

    /// Vrai si la famille a un vrai dessin pour ce style.
    #[must_use]
    pub fn has(&self, bold: bool, italic: bool) -> bool {
        self.faces[slot(bold, italic)].is_some()
    }
}

/// Lit la police d'un fichier.
#[must_use]
pub fn load(face: &FaceFile) -> Option<TrueTypeFont> {
    let data = std::fs::read(&face.path).ok()?;
    TrueTypeFont::parse_collection_index(&data, face.index).ok()
}

/// Les familles installées, par ordre alphabétique.
#[must_use]
pub fn families() -> &'static [Family] {
    static CATALOGUE: OnceLock<Vec<Family>> = OnceLock::new();
    CATALOGUE.get_or_init(scan)
}

/// La famille de ce nom, sans égard à la casse.
#[must_use]
pub fn find(name: &str) -> Option<&'static Family> {
    let wanted = name.trim();
    families()
        .iter()
        .find(|f| f.name.eq_ignore_ascii_case(wanted))
}

/// Clé de comparaison d'un nom de police : lettres et chiffres seuls, en
/// minuscules, sans les marques de fonderie (`PS`, `MT`) qu'un PDF ajoute.
fn key(name: &str) -> String {
    let mut bare: String = name.chars().filter(char::is_ascii_alphanumeric).collect();
    for mark in ["PSMT", "PS", "MT"] {
        if bare.len() > mark.len() && bare.ends_with(mark) {
            bare.truncate(bare.len() - mark.len());
        }
    }
    bare.to_ascii_lowercase()
}

/// Ce qu'un nom de police de PDF (`ABCDEF+Georgia-Bold`,
/// `TimesNewRomanPS-BoldItalicMT`) veut dire pour un lecteur : la famille,
/// telle que le système l'appelle quand il la connaît, le gras et l'italique.
///
/// C'est ce que la barre « Modifier le PDF » montre pour le bloc ouvert :
/// « Georgia », gras — et non le nom de ressource, ni rien du tout.
#[must_use]
pub fn describe(base_font: &str) -> (String, bool, bool) {
    // Le préfixe de sous-ensemble : six capitales et un « + ».
    let name = match base_font.split_once('+') {
        Some((p, rest)) if p.len() == 6 && p.bytes().all(|b| b.is_ascii_uppercase()) => rest,
        _ => base_font,
    };
    let (family, style) = name
        .split_once(['-', ','])
        .map_or((name, ""), |(f, s)| (f, s));
    let (bold, italic) = names::style_of(style);
    let wanted = key(family);
    if let Some(known) = families().iter().find(|f| key(&f.name) == wanted) {
        return (known.name.clone(), bold, italic);
    }
    // Inconnue du système : on sépare au moins les mots collés.
    let mut spaced = String::new();
    let mut previous = ' ';
    for c in family.chars() {
        if c.is_ascii_uppercase() && previous.is_ascii_lowercase() {
            spaced.push(' ');
        }
        spaced.push(c);
        previous = c;
    }
    (spaced, bold, italic)
}

/// Parcourt les dossiers de polices.
fn scan() -> Vec<Family> {
    let mut found: Vec<Family> = Vec::new();
    for dir in crate::fontembed::font_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase());
            if !matches!(ext.as_deref(), Some("ttf" | "otf" | "ttc")) {
                continue;
            }
            let Ok(data) = std::fs::read(&path) else {
                continue;
            };
            for index in 0..TrueTypeFont::collection_count(&data).max(1) {
                record(&mut found, &data, &path, index);
            }
        }
    }
    found.sort_by_key(|f| f.name.to_lowercase());
    found
}

/// Range une police dans le catalogue, si elle est exploitable.
fn record(found: &mut Vec<Family>, data: &[u8], path: &std::path::Path, index: u32) {
    let Ok(font) = TrueTypeFont::parse_collection_index(data, index) else {
        return;
    };
    // Le sous-ensemble recopie la table `glyf` : une police CFF pure ne
    // s'incorpore pas ainsi.
    if !font.has_glyf() {
        return;
    }
    let Some(table) = font.table_bytes(*b"name") else {
        return;
    };
    let read = names::parse(table);
    let Some(name) = read.family.as_deref().or(read.best_family()) else {
        return;
    };
    let name = name.trim();
    // Les familles « cachées » du système commencent par un point.
    if name.is_empty() || name.starts_with('.') {
        return;
    }
    let (bold, italic) = names::style_of(
        read.subfamily
            .as_deref()
            .or(read.best_subfamily())
            .unwrap_or("Regular"),
    );
    let position = found
        .iter()
        .position(|f| f.name.eq_ignore_ascii_case(name))
        .unwrap_or_else(|| {
            found.push(Family {
                name: name.to_string(),
                faces: Default::default(),
            });
            found.len() - 1
        });
    let face = &mut found[position].faces[slot(bold, italic)];
    // Le premier fichier trouvé garde sa place : les doublons d'un même
    // dessin (une police installée deux fois) ne se disputent pas la case.
    if face.is_none() {
        *face = Some(FaceFile {
            path: path.to_path_buf(),
            index,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sur une machine qui a des polices, le catalogue n'est pas vide, il est
    /// trié, et chaque famille rend au moins un fichier lisible.
    #[test]
    fn le_catalogue_est_trie_et_lisible() {
        let all = families();
        if all.is_empty() {
            // Un conteneur sans aucune police : rien à vérifier.
            return;
        }
        for pair in all.windows(2) {
            assert!(
                pair[0].name.to_lowercase() <= pair[1].name.to_lowercase(),
                "{} avant {}",
                pair[0].name,
                pair[1].name
            );
        }
        let first = &all[0];
        let face = first.face(false, false);
        assert!(face.is_some(), "{} n'a aucun dessin", first.name);
        assert!(face.and_then(load).is_some(), "{} illisible", first.name);
    }

    /// Un style absent se rabat sur un dessin existant plutôt que sur rien.
    #[test]
    fn un_style_absent_se_rabat_sur_un_autre() {
        let family = Family {
            name: "Essai".into(),
            faces: [
                Some(FaceFile {
                    path: PathBuf::from("essai.ttf"),
                    index: 0,
                }),
                None,
                None,
                None,
            ],
        };
        assert!(!family.has(true, true));
        assert_eq!(
            family.face(true, true).map(|f| f.path.clone()),
            Some(PathBuf::from("essai.ttf"))
        );
    }

    /// Un nom de police de PDF se lit comme un lecteur le dirait.
    #[test]
    fn un_nom_de_police_de_pdf_se_lit() {
        let (family, bold, italic) = describe("ABCDEF+Zzyzx-BoldItalic");
        assert_eq!((family.as_str(), bold, italic), ("Zzyzx", true, true));
        let (family, bold, italic) = describe("MaPoliceInconnue");
        assert_eq!(
            (family.as_str(), bold, italic),
            ("Ma Police Inconnue", false, false)
        );
        assert_eq!(key("TimesNewRomanPSMT"), "timesnewroman");
        assert_eq!(key("Times New Roman"), "timesnewroman");
    }

    /// La recherche ignore la casse et les blancs autour.
    #[test]
    fn la_recherche_ignore_la_casse() {
        let Some(first) = families().first() else {
            return;
        };
        let shouted = format!("  {}  ", first.name.to_uppercase());
        assert_eq!(
            find(&shouted).map(|f| f.name.as_str()),
            Some(first.name.as_str())
        );
    }
}
