//! Les polices installées sur la machine, retrouvées par leur **nom**.
//!
//! Un PDF qui n'incorpore pas sa police écrit `/BaseFont /Calibri-Bold` et
//! s'en remet au lecteur. Deviner le nom du fichier (`calibrib.ttf`) marche
//! sur Windows et nulle part ailleurs : macOS empile ses polices dans des
//! recueils `.ttc` aux noms arbitraires, les distributions Linux les
//! éparpillent par famille, et personne ne garantit qu'un fichier nommé
//! `arial.ttf` contienne Arial.
//!
//! Alors on fait ce que fait Acrobat : on ouvre les polices une fois, on lit
//! le nom sous lequel chacune se présente, et on s'en sert. C'est la seule
//! méthode qui trouve vraiment ce que le document demande.
//!
//! # Ce que ça coûte
//!
//! Rien, ou presque. On ne lit pas les fichiers : on lit leur **table des
//! tables** — douze octets d'en-tête, seize par table — puis la seule table
//! `name`, au décalage qu'elle indique. Quelques kilo-octets par police au
//! lieu de plusieurs mégaoctets. L'index se construit à la première police
//! absente rencontrée, jamais avant, et sert ensuite tout le document.
//!
//! # L'ordre des recours
//!
//! 1. le nom PostScript exact — c'est la police du document ;
//! 2. le nom complet, puis la famille avec le style demandé ;
//! 3. la famille, quel que soit son style ;
//! 4. une police d'allure équivalente parmi les familles connues ;
//! 5. **n'importe quelle** police de la machine ayant le bon caractère
//!    (à chasse fixe, à empattements) — une vraie police mal choisie reste
//!    préférable à une lettre dessinée par nous ;
//! 6. et seulement là, [`acrux_fonts::fallback`].

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;

use acrux_fonts::names::{self, Names};

/// Nombre maximal de fichiers examinés. Un répertoire de polices en compte
/// quelques centaines ; au-delà, c'est qu'on est tombé sur autre chose, et il
/// ne faut pas y passer la journée.
const MAX_FILES: usize = 4000;
/// Profondeur maximale d'exploration sous un répertoire de polices.
const MAX_DEPTH: usize = 4;
/// Taille maximale d'une table `name` jugée crédible.
const MAX_NAME_TABLE: usize = 1 << 20;

/// Une police installée, telle que l'index la connaît.
#[derive(Debug, Clone)]
pub struct Face {
    /// Fichier qui la contient.
    pub path: PathBuf,
    /// Numéro dans le recueil, 0 pour un fichier simple.
    pub index: u32,
    /// Famille, normalisée.
    pub family: String,
    /// Graisse.
    pub bold: bool,
    /// Italique ou oblique.
    pub italic: bool,
}

/// L'index des polices de la machine.
#[derive(Debug, Default)]
pub struct Index {
    /// Nom PostScript → police. C'est l'entrée la plus sûre : c'est ce nom
    /// que le document écrit dans `/BaseFont`.
    postscript: HashMap<String, Face>,
    /// Nom complet, tel qu'un menu de traitement de texte l'affiche.
    full: HashMap<String, Face>,
    /// Famille → ses graisses et ses italiques.
    family: HashMap<String, Vec<Face>>,
}

impl Index {
    /// Vrai si la machine n'a aucune police utilisable.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.family.is_empty()
    }

    /// Nombre de polices indexées.
    #[must_use]
    pub fn len(&self) -> usize {
        self.family.values().map(Vec::len).sum()
    }

    /// Cherche la police que le document demande.
    #[must_use]
    pub fn lookup(&self, base_font: &str, bold: bool, italic: bool) -> Option<&Face> {
        let name = base_font.rsplit('+').next().unwrap_or(base_font);
        // 1. Le nom PostScript : c'est exactement la police du document.
        if let Some(face) = self.postscript.get(&normalize(name)) {
            return Some(face);
        }
        // 2. Le nom complet. `/BaseFont` sépare souvent par un tiret ou une
        //    virgule là où la police écrit une espace.
        let espaces = name.replace(['-', ','], " ");
        if let Some(face) = self.full.get(&normalize(&espaces)) {
            return Some(face);
        }
        // 3. La famille, avec le style demandé. On essaie le nom tel quel
        //    avant de lui retirer son style : une famille peut s'appeler
        //    « Nimbus Mono PS », et lui ôter son `PS` la rendrait introuvable.
        for key in [normalize(name), family_key(name)] {
            if key.len() < 3 {
                continue;
            }
            if let Some(faces) = self.family.get(&key) {
                if let Some(face) = best_style(faces, bold, italic) {
                    return Some(face);
                }
            }
        }
        None
    }

    /// Cherche une police d'allure voulue, faute de la bonne.
    #[must_use]
    pub fn lookup_generic(&self, kind: Kind, bold: bool, italic: bool) -> Option<&Face> {
        for family in kind.families() {
            if let Some(faces) = self.family.get(&normalize(family)) {
                if let Some(face) = best_style(faces, bold, italic) {
                    return Some(face);
                }
            }
        }
        // Aucune famille connue : n'importe laquelle fera mieux qu'un dessin
        // de notre main. On prend la première dans l'ordre alphabétique, pour
        // qu'une même machine rende toujours la même page.
        let mut familles: Vec<&String> = self.family.keys().collect();
        familles.sort_unstable();
        familles
            .into_iter()
            .find_map(|f| self.family.get(f).and_then(|v| best_style(v, bold, italic)))
    }
}

/// Allure demandée, quand la police exacte est introuvable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Linéale.
    Sans,
    /// À empattements.
    Serif,
    /// À chasse fixe.
    Mono,
    /// Symboles mathématiques.
    Symbol,
    /// Fleurons et pictogrammes.
    Dingbat,
}

impl Kind {
    /// Familles à essayer, dans l'ordre. Les premières sont métriquement
    /// compatibles avec les polices standard de PDF, les suivantes sont ce
    /// qu'on trouve couramment sur chaque système.
    fn families(self) -> &'static [&'static str] {
        match self {
            Kind::Sans => &[
                "Arial",
                "Liberation Sans",
                "Helvetica",
                "Helvetica Neue",
                "Nimbus Sans",
                "DejaVu Sans",
                "Noto Sans",
                "Segoe UI",
                "Verdana",
                "Tahoma",
                "Roboto",
                "Cantarell",
                "Ubuntu",
            ],
            Kind::Serif => &[
                "Times New Roman",
                "Liberation Serif",
                "Times",
                "Nimbus Roman",
                "DejaVu Serif",
                "Noto Serif",
                "Georgia",
                "Cambria",
                "Garamond",
                "Palatino",
            ],
            Kind::Mono => &[
                "Courier New",
                "Liberation Mono",
                "Courier",
                "Nimbus Mono PS",
                "DejaVu Sans Mono",
                "Noto Sans Mono",
                "Consolas",
                "Menlo",
                "Monaco",
                "Ubuntu Mono",
            ],
            Kind::Symbol => &["Symbol", "OpenSymbol", "Standard Symbols PS", "DejaVu Sans"],
            Kind::Dingbat => &[
                "Wingdings",
                "Zapf Dingbats",
                "Dingbats",
                "D050000L",
                "Noto Sans Symbols",
            ],
        }
    }
}

/// Parmi les polices d'une famille, celle qui colle le mieux au style demandé.
fn best_style(faces: &[Face], bold: bool, italic: bool) -> Option<&Face> {
    faces
        .iter()
        .min_by_key(|f| usize::from(f.bold != bold) * 2 + usize::from(f.italic != italic))
}

/// Réduit un nom à sa forme comparable : lettres et chiffres, en minuscules.
fn normalize(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Radical de famille d'un `/BaseFont` : le nom, débarrassé de son style et
/// des suffixes de nommage des fonderies.
///
/// Le style se retire **en fin de nom seulement**. L'ôter partout casserait
/// « Times New Roman », dont le `Roman` est un morceau du nom de famille et
/// non une graisse — la police s'appelle ainsi depuis 1932.
fn family_key(name: &str) -> String {
    let name = name.rsplit('+').next().unwrap_or(name);
    // En nommage PostScript, le style suit un tiret : `Calibri-BoldItalic`.
    let base = name.split(['-', ',']).next().unwrap_or(name);
    let mut key = normalize(base);
    // Suffixes de nommage : `ArialMT`, `TimesNewRomanPSMT`.
    for suffixe in ["psmt", "psmc", "mt", "ps"] {
        if let Some(cut) = key.strip_suffix(suffixe) {
            if cut.len() >= 3 {
                key = cut.to_string();
                break;
            }
        }
    }
    // Style collé au nom, sans séparateur : `ArialBold`.
    for style in [
        "boldoblique",
        "bolditalic",
        "semibold",
        "demibold",
        "oblique",
        "italic",
        "regular",
        "bold",
        "light",
        "black",
        "heavy",
    ] {
        if let Some(cut) = key.strip_suffix(style) {
            if cut.len() >= 3 {
                key = cut.to_string();
                break;
            }
        }
    }
    key
}

/// L'index, construit au premier besoin.
///
/// `Rc` et non `Arc` : le rendu d'une page est mono-fil, et l'index vit aussi
/// longtemps que le programme.
pub fn index() -> &'static Index {
    static INDEX: OnceLock<Index> = OnceLock::new();
    INDEX.get_or_init(build)
}

/// Répertoires où chercher les polices du système.
///
/// `ACRUX_FONT_DIR` les **remplace** au lieu de s'y ajouter : c'est ce qui
/// permet de vérifier ce que donne un fichier sur une machine dépourvue de
/// polices, qui est le cas que l'on redoute.
fn font_dirs() -> Vec<PathBuf> {
    if let Ok(custom) = std::env::var("ACRUX_FONT_DIR") {
        return vec![PathBuf::from(custom)];
    }
    let mut dirs = Vec::new();
    if let Ok(windir) = std::env::var("WINDIR") {
        dirs.push(PathBuf::from(windir).join("Fonts"));
    }
    // Polices installées par l'utilisateur, sans droits d'administrateur.
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Microsoft/Windows/Fonts"));
    }
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".fonts"));
        dirs.push(home.join(".local/share/fonts"));
        dirs.push(home.join("Library/Fonts"));
    }
    dirs.push(PathBuf::from("/usr/share/fonts"));
    dirs.push(PathBuf::from("/usr/local/share/fonts"));
    dirs.push(PathBuf::from("/System/Library/Fonts"));
    dirs.push(PathBuf::from("/Library/Fonts"));
    dirs
}

/// Parcourt les répertoires de polices et lit le nom de chacune.
fn build() -> Index {
    let mut index = Index::default();
    let mut budget = MAX_FILES;
    for dir in font_dirs() {
        walk(&dir, 0, &mut budget, &mut index);
        if budget == 0 {
            break;
        }
    }
    index
}

/// Descend un répertoire, sans suivre les liens et sans y passer la journée.
fn walk(dir: &Path, depth: usize, budget: &mut usize, index: &mut Index) {
    if depth > MAX_DEPTH || *budget == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if *budget == 0 {
            return;
        }
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk(&path, depth + 1, budget, index);
            continue;
        }
        if !kind.is_file() || !is_font_file(&path) {
            continue;
        }
        *budget -= 1;
        for (number, names) in read_names(&path) {
            insert(index, &path, number, &names);
        }
    }
}

/// Vrai pour les extensions qui contiennent des contours lisibles par nous.
fn is_font_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "ttf" | "otf" | "ttc" | "otc"
    )
}

/// Range une police dans l'index.
fn insert(index: &mut Index, path: &Path, number: u32, names: &Names) {
    let Some(family) = names.best_family() else {
        return;
    };
    let (bold, italic) = names
        .best_subfamily()
        .map_or((false, false), names::style_of);
    let face = Face {
        path: path.to_path_buf(),
        index: number,
        family: normalize(family),
        bold,
        italic,
    };
    if let Some(ps) = &names.postscript {
        index
            .postscript
            .entry(normalize(ps))
            .or_insert_with(|| face.clone());
    }
    if let Some(full) = &names.full {
        index
            .full
            .entry(normalize(full))
            .or_insert_with(|| face.clone());
    }
    index
        .family
        .entry(face.family.clone())
        .or_default()
        .push(face);
}

/// Lit la table `name` d'un fichier de police, sans le charger en entier.
///
/// Rend un couple par police : un `.ttc` en contient plusieurs.
fn read_names(path: &Path) -> Vec<(u32, Names)> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let mut head = [0u8; 12];
    if file.read_exact(&mut head).is_err() {
        return Vec::new();
    }
    let mut directories = Vec::new();
    if &head[..4] == b"ttcf" {
        let count = u32::from_be_bytes([head[8], head[9], head[10], head[11]]).min(64);
        let mut offsets = vec![0u8; count as usize * 4];
        if file.read_exact(&mut offsets).is_err() {
            return Vec::new();
        }
        for chunk in offsets.chunks_exact(4) {
            directories.push(u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
    } else {
        directories.push(0);
    }
    let mut out = Vec::new();
    for (number, offset) in directories.into_iter().enumerate() {
        let Some(table) = read_name_table(&mut file, u64::from(offset)) else {
            continue;
        };
        let names = names::parse(&table);
        if names.best_family().is_some() {
            out.push((u32::try_from(number).unwrap_or(0), names));
        }
    }
    out
}

/// Trouve la table `name` dans une table des tables et la lit.
fn read_name_table(file: &mut File, directory: u64) -> Option<Vec<u8>> {
    file.seek(SeekFrom::Start(directory)).ok()?;
    let mut head = [0u8; 12];
    file.read_exact(&mut head).ok()?;
    let count = usize::from(u16::from_be_bytes([head[4], head[5]])).min(512);
    let mut records = vec![0u8; count * 16];
    file.read_exact(&mut records).ok()?;
    for record in records.chunks_exact(16) {
        if &record[..4] != b"name" {
            continue;
        }
        let offset = u32::from_be_bytes([record[8], record[9], record[10], record[11]]);
        let length = u32::from_be_bytes([record[12], record[13], record[14], record[15]]);
        let length = usize::try_from(length).ok()?;
        if length == 0 || length > MAX_NAME_TABLE {
            return None;
        }
        file.seek(SeekFrom::Start(u64::from(offset))).ok()?;
        let mut table = vec![0u8; length];
        file.read_exact(&mut table).ok()?;
        return Some(table);
    }
    None
}

thread_local! {
    /// Octets des polices déjà chargées, par chemin : une même police sert
    /// souvent plusieurs fois dans un document.
    static LOADED: std::cell::RefCell<HashMap<PathBuf, Option<Rc<Vec<u8>>>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Charge les octets d'une police de l'index.
#[must_use]
pub fn load(face: &Face) -> Option<Rc<Vec<u8>>> {
    if let Some(bytes) = LOADED.with(|c| c.borrow().get(&face.path).cloned()) {
        return bytes;
    }
    let bytes = std::fs::read(&face.path).ok().map(Rc::new);
    LOADED.with(|c| c.borrow_mut().insert(face.path.clone(), bytes.clone()));
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_noms_se_normalisent() {
        assert_eq!(normalize("Times New Roman"), "timesnewroman");
        assert_eq!(normalize("Helvetica-BoldOblique"), "helveticaboldoblique");
        assert_eq!(family_key("ABCDEF+Calibri-BoldItalic"), "calibri");
        // Le « Roman » de Times New Roman n'est pas une graisse.
        assert_eq!(family_key("TimesNewRomanPSMT"), "timesnewroman");
        assert_eq!(family_key("Arial-BoldMT"), "arial");
        assert_eq!(family_key("ArialMT"), "arial");
        assert_eq!(family_key("ArialBold"), "arial");
        assert_eq!(family_key("SegoeUI"), "segoeui");
        assert_eq!(family_key("ComicSansMS"), "comicsansms");
        assert_eq!(family_key("Times-Roman"), "times");
    }

    #[test]
    fn le_style_le_plus_proche_est_choisi() {
        let face = |bold, italic| Face {
            path: PathBuf::from("x"),
            index: 0,
            family: "x".into(),
            bold,
            italic,
        };
        let faces = vec![face(false, false), face(true, false), face(false, true)];
        assert!(best_style(&faces, true, false).is_some_and(|f| f.bold && !f.italic));
        assert!(best_style(&faces, false, true).is_some_and(|f| !f.bold && f.italic));
        // Gras italique absent : on prend celui qui se trompe le moins.
        let choisi = best_style(&faces, true, true).unwrap_or(&faces[0]);
        assert!(choisi.bold || choisi.italic);
        assert!(best_style(&[], false, false).is_none());
    }

    #[test]
    fn lindex_de_la_machine_trouve_ses_polices() {
        let index = index();
        if index.is_empty() {
            return; // Machine sans polices : rien à vérifier.
        }
        // Une famille au moins doit répondre à son propre nom.
        let une = index.family.keys().next().cloned().unwrap_or_default();
        assert!(index.family.contains_key(&une));
        // L'allure demandée doit toujours donner quelque chose.
        for kind in [Kind::Sans, Kind::Serif, Kind::Mono] {
            assert!(index.lookup_generic(kind, false, false).is_some());
        }
    }
}
