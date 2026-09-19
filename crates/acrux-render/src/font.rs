//! Polices telles que vues par le PDF (ISO 32000-2 §9) : polices simples
//! (Type1, TrueType, Type3), composites (Type0 + CIDFont), encodages,
//! largeurs, correspondance code → glyphe, et substitution par une police
//! système quand la police n'est pas incorporée.
//!
//! Le parsing des fichiers de polices est dans `acrux-fonts` ; ici on assemble
//! ce que dit le dictionnaire de police avec ce que contient le fichier.

// `Program` embarque directement les programmes de police (tailles très
// différentes selon le format, mais chaque police en porte un seul) ; le
// chargement suit la longue liste d'entrées du dictionnaire de police (§9.6).
#![allow(clippy::large_enum_variant, clippy::too_many_lines)]

use std::collections::HashMap;

use acrux_core::{Matrix, Path, Result};
use acrux_document::{Dict, Document, Name, Object};
use acrux_fonts::cmap::CMap;
use acrux_fonts::encodings::{self, glyph_name_to_unicode};
use acrux_fonts::fallback;
use acrux_fonts::standard::Standard;
use acrux_fonts::{CffFont, TrueTypeFont, Type1Font};

pub mod system;

/// Famille de police PDF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontKind {
    /// `/Type1` (et `/MMType1`).
    Type1,
    /// `/TrueType`.
    TrueType,
    /// `/Type3` : glyphes définis par des flux de contenu.
    Type3,
    /// `/Type0` : police composite.
    Type0,
}

/// Programme de police chargé.
pub enum Program {
    /// Contours TrueType (ou OpenType CFF via `TrueTypeFont::cff`).
    TrueType(TrueTypeFont),
    /// CFF nu (`/FontFile3`).
    Cff(CffFont),
    /// Type 1 (`/FontFile`).
    Type1(Type1Font),
    /// Type 3 : procédures de glyphes.
    Type3 {
        /// `/CharProcs` : nom de glyphe → flux de contenu.
        char_procs: Dict,
        /// `/Resources` propres à la police.
        resources: Option<Dict>,
    },
    /// Notre propre police de secours : aucune police du système n'est
    /// utilisable. Voir [`acrux_fonts::fallback`].
    Fallback,
    /// Aucun programme (ni police du système, ni glyphe de secours pour ce
    /// nom : les écritures que notre police de secours ne couvre pas).
    None,
}

impl std::fmt::Debug for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Program::TrueType(_) => write!(f, "TrueType"),
            Program::Cff(_) => write!(f, "CFF"),
            Program::Type1(_) => write!(f, "Type1"),
            Program::Type3 { .. } => write!(f, "Type3"),
            Program::Fallback => write!(f, "Fallback"),
            Program::None => write!(f, "None"),
        }
    }
}

/// Un glyphe à dessiner : code source, identifiant dans le programme,
/// largeur d'avance en espace texte (déjà divisée par 1000) et indicateur
/// « caractère code 32 sur un octet » pour l'espacement des mots (§9.3.3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphRef {
    /// Code tel que lu dans la chaîne.
    pub code: u32,
    /// CID (composites) ou code (simples).
    pub cid: u32,
    /// Largeur d'avance horizontale en unités texte (glyph space / 1000).
    pub width: f64,
    /// Vrai si le code est l'octet 32 d'une police à un octet.
    pub is_space: bool,
}

/// Police prête à l'emploi.
pub struct LoadedFont {
    /// Famille.
    pub kind: FontKind,
    /// Nom de base (`/BaseFont`).
    pub base_font: String,
    /// Programme de glyphes.
    pub program: Program,
    /// Vrai si le programme vient d'une substitution (police non incorporée).
    pub substituted: bool,
    /// Police standard reconnue (§9.6.2.2), si `/BaseFont` en désigne une :
    /// c'est elle qui donne les largeurs quand le document n'en fournit pas.
    pub standard: Option<Standard>,
    /// Matrice glyphe → texte (`/FontMatrix` pour Type3, sinon 1/1000 ou
    /// 1/unitsPerEm est appliqué via `units_per_em`).
    pub font_matrix: Matrix,
    /// Drapeaux du descripteur (§9.8.2, table 123).
    pub flags: u32,
    /// Largeur par défaut (`/MissingWidth` ou `/DW`).
    default_width: f64,
    /// Largeurs des polices simples, par code.
    simple_widths: HashMap<u32, f64>,
    /// Largeurs des polices composites, par CID.
    cid_widths: HashMap<u32, f64>,
    /// Encodage des polices simples : code → nom de glyphe.
    encoding_names: Vec<Option<String>>,
    /// Vrai si le dictionnaire fournit un `/Encoding` explicite.
    has_encoding: bool,
    /// CMap d'encodage (composites).
    cmap: Option<CMap>,
    /// CID → GID (composites CIDFontType2) : `None` = identité.
    cid_to_gid: Option<Vec<u16>>,
    /// Cache code → GID.
    gid_cache: std::cell::RefCell<HashMap<u32, Option<u32>>>,
    /// Cache CID → contour, en espace texte unitaire. Une page de texte
    /// redessine les mêmes lettres des centaines de fois : sans ce cache, le
    /// programme de la police est réinterprété à chaque occurrence.
    path_cache: std::cell::RefCell<HashMap<u32, Option<std::rc::Rc<Path>>>>,
    /// `/ToUnicode` pour l'extraction de texte.
    pub to_unicode: Option<CMap>,
}

impl std::fmt::Debug for LoadedFont {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedFont")
            .field("kind", &self.kind)
            .field("base_font", &self.base_font)
            .field("program", &self.program)
            .field("substituted", &self.substituted)
            .finish_non_exhaustive()
    }
}

const FLAG_SYMBOLIC: u32 = 1 << 2;
const FLAG_SERIF: u32 = 1 << 1;
const FLAG_FIXED: u32 = 1;
const FLAG_ITALIC: u32 = 1 << 6;
const FLAG_FORCE_BOLD: u32 = 1 << 18;

impl LoadedFont {
    /// Charge une police depuis son dictionnaire.
    ///
    /// # Errors
    /// Dictionnaire illisible. Une police non incorporée ou au programme
    /// corrompu n'est pas une erreur : on substitue ou on rend des glyphes vides.
    pub fn load(doc: &Document, dict: &Dict) -> Result<Self> {
        let subtype = name_of(doc, dict, "Subtype").unwrap_or_default();
        let base_font = name_of(doc, dict, "BaseFont").unwrap_or_default();
        let kind = match subtype.as_str() {
            "Type0" => FontKind::Type0,
            "TrueType" => FontKind::TrueType,
            "Type3" => FontKind::Type3,
            _ => FontKind::Type1,
        };
        match kind {
            FontKind::Type0 => Self::load_type0(doc, dict, base_font),
            FontKind::Type3 => Self::load_type3(doc, dict, base_font),
            _ => Self::load_simple(doc, dict, kind, base_font),
        }
    }

    fn load_simple(doc: &Document, dict: &Dict, kind: FontKind, base_font: String) -> Result<Self> {
        let descriptor = doc
            .dict_get(dict, "FontDescriptor")?
            .and_then(|d| d.as_dict().cloned());
        // Les quatorze polices standard peuvent se passer de descripteur et de
        // largeurs (§9.6.2.2) : c'est au lecteur de les connaître.
        let standard = if kind == FontKind::Type3 {
            None
        } else {
            Standard::from_base_font(&base_font)
        };
        let flags = descriptor
            .as_ref()
            .and_then(|d| doc.dict_get(d, "Flags").ok().flatten())
            .and_then(|o| o.as_i64())
            .and_then(|v| u32::try_from(v).ok())
            .or_else(|| standard.map(|s| s.metrics().flags))
            .unwrap_or(0);
        let missing_width = descriptor
            .as_ref()
            .and_then(|d| doc.dict_get(d, "MissingWidth").ok().flatten())
            .and_then(|o| o.as_f64())
            .unwrap_or(0.0);
        let (program, substituted) = match descriptor.as_ref().and_then(|d| load_program(doc, d)) {
            Some(p) => (p, false),
            None => (substitute(&base_font, flags), true),
        };
        // Largeurs.
        let mut simple_widths = HashMap::new();
        let first_char = doc
            .dict_get(dict, "FirstChar")?
            .and_then(|o| o.as_i64())
            .unwrap_or(0);
        if let Some(widths) = doc.dict_get(dict, "Widths")? {
            if let Some(arr) = widths.as_array() {
                for (i, w) in arr.iter().enumerate() {
                    if let Some(w) = doc.resolve(w).ok().and_then(|o| o.as_f64()) {
                        let code = first_char + i64::try_from(i).unwrap_or(0);
                        if let Ok(code) = u32::try_from(code) {
                            simple_widths.insert(code, w / 1000.0);
                        }
                    }
                }
            }
        }
        // Encodage (§9.6.5) : base selon la police, puis /Differences.
        let symbolic = flags & FLAG_SYMBOLIC != 0 && flags & (1 << 5) == 0;
        let builtin: Option<Vec<Option<String>>> = match &program {
            Program::Type1(t1) => Some(t1.builtin_encoding().to_vec()),
            Program::Cff(cff) if !cff.is_cid() => Some(
                (0..=255u8)
                    .map(|c| cff.encoding_code_to_gid(c).and_then(|g| cff.glyph_name(g)))
                    .collect(),
            ),
            _ => None,
        };
        let mut encoding_names: Vec<Option<String>> = match &builtin {
            Some(b) if symbolic || !substituted => b.clone(),
            // Type 3 : aucun encodage implicite, tout vient de /Differences (§9.6.6.3).
            _ if kind == FontKind::Type3 => vec![None; 256],
            _ => base_table(encodings::BaseEncoding::Standard),
        };
        if builtin.is_none() {
            if let Some(s) = standard.filter(|s| s.has_builtin_encoding()) {
                // Symbol et ZapfDingbats portent leur propre encodage
                // (§9.6.6.1). Leur appliquer StandardEncoding mettrait « A »
                // là où le document veut « Alpha » : ni le bon glyphe, ni la
                // bonne largeur.
                encoding_names = base_table(if s == Standard::ZapfDingbats {
                    encodings::BaseEncoding::ZapfDingbats
                } else {
                    encodings::BaseEncoding::Symbol
                });
            }
        }
        let mut has_encoding = false;
        if let Some(enc) = doc.dict_get(dict, "Encoding")? {
            has_encoding = true;
            let apply_base = |names: &mut Vec<Option<String>>, base: &str| {
                let table = match base {
                    "WinAnsiEncoding" => Some(encodings::BaseEncoding::WinAnsi),
                    "MacRomanEncoding" => Some(encodings::BaseEncoding::MacRoman),
                    "MacExpertEncoding" => Some(encodings::BaseEncoding::MacExpert),
                    "StandardEncoding" => Some(encodings::BaseEncoding::Standard),
                    _ => None,
                };
                if let Some(t) = table {
                    *names = base_table(t);
                }
            };
            match &*enc {
                Object::Name(n) => apply_base(&mut encoding_names, &n.as_str()),
                Object::Dict(d) => {
                    if let Some(Object::Name(b)) = doc.dict_get(d, "BaseEncoding")?.as_deref() {
                        apply_base(&mut encoding_names, &b.as_str());
                    } else if builtin.is_none() && !symbolic && kind != FontKind::Type3 {
                        apply_base(&mut encoding_names, "StandardEncoding");
                    }
                    if let Some(diff) = doc.dict_get(d, "Differences")? {
                        if let Some(items) = diff.as_array() {
                            let mut code: i64 = 0;
                            for item in items {
                                match doc.resolve(item).ok().as_deref() {
                                    Some(Object::Integer(i)) => code = *i,
                                    Some(Object::Real(r)) => {
                                        #[allow(clippy::cast_possible_truncation)]
                                        {
                                            code = *r as i64;
                                        }
                                    }
                                    Some(Object::Name(n)) => {
                                        if let Ok(c) = usize::try_from(code) {
                                            if c < 256 {
                                                encoding_names[c] = Some(n.as_str());
                                            }
                                        }
                                        code += 1;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        encoding_names.resize(256, None);
        let to_unicode = load_to_unicode(doc, dict);
        Ok(Self {
            kind,
            base_font,
            program,
            substituted,
            standard,
            font_matrix: Matrix::IDENTITY,
            flags,
            default_width: missing_width / 1000.0,
            simple_widths,
            cid_widths: HashMap::new(),
            encoding_names,
            has_encoding,
            cmap: None,
            cid_to_gid: None,
            gid_cache: std::cell::RefCell::new(HashMap::new()),
            path_cache: std::cell::RefCell::new(HashMap::new()),
            to_unicode,
        })
    }

    fn load_type3(doc: &Document, dict: &Dict, base_font: String) -> Result<Self> {
        let matrix = doc
            .dict_get(dict, "FontMatrix")?
            .and_then(|m| m.as_array().map(|a| matrix_from(doc, a)))
            .unwrap_or(Matrix::new(0.001, 0.0, 0.0, 0.001, 0.0, 0.0));
        let char_procs = doc
            .dict_get(dict, "CharProcs")?
            .and_then(|d| d.as_dict().cloned())
            .unwrap_or_default();
        let resources = doc
            .dict_get(dict, "Resources")?
            .and_then(|d| d.as_dict().cloned());
        let mut font = Self::load_simple(doc, dict, FontKind::Type3, base_font)?;
        font.program = Program::Type3 {
            char_procs,
            resources,
        };
        font.substituted = false;
        font.font_matrix = matrix;
        // Les largeurs Type3 sont en espace glyphe : on les garde brutes (×matrice à l'usage).
        let first_char = doc
            .dict_get(dict, "FirstChar")?
            .and_then(|o| o.as_i64())
            .unwrap_or(0);
        font.simple_widths.clear();
        if let Some(widths) = doc.dict_get(dict, "Widths")? {
            if let Some(arr) = widths.as_array() {
                for (i, w) in arr.iter().enumerate() {
                    if let Some(w) = doc.resolve(w).ok().and_then(|o| o.as_f64()) {
                        if let Ok(code) = u32::try_from(first_char + i64::try_from(i).unwrap_or(0))
                        {
                            // Largeur transformée par la matrice de police (composante x).
                            let p = matrix.apply_vector(acrux_core::Point::new(w, 0.0));
                            font.simple_widths.insert(code, p.x);
                        }
                    }
                }
            }
        }
        // Type3 : encodage obligatoire via /Differences ; pas de base implicite.
        if !font.has_encoding {
            font.encoding_names = vec![None; 256];
        }
        Ok(font)
    }

    fn load_type0(doc: &Document, dict: &Dict, base_font: String) -> Result<Self> {
        let descendant = doc
            .dict_get(dict, "DescendantFonts")?
            .and_then(|d| d.as_array().and_then(|a| a.first().cloned()))
            .and_then(|o| doc.resolve(&o).ok().and_then(|r| r.as_dict().cloned()))
            .unwrap_or_default();
        let descriptor = doc
            .dict_get(&descendant, "FontDescriptor")?
            .and_then(|d| d.as_dict().cloned());
        let flags = descriptor
            .as_ref()
            .and_then(|d| doc.dict_get(d, "Flags").ok().flatten())
            .and_then(|o| o.as_i64())
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        let (program, substituted) = match descriptor.as_ref().and_then(|d| load_program(doc, d)) {
            Some(p) => (p, false),
            None => (substitute(&base_font, flags), true),
        };
        // CMap d'encodage.
        let cmap = match doc.dict_get(dict, "Encoding")? {
            Some(e) => match &*e {
                Object::Name(n) => CMap::predefined(&n.as_str()),
                Object::Stream { .. } => doc
                    .stream_data(&e)
                    .ok()
                    .and_then(|d| CMap::parse(&d.data).ok()),
                _ => None,
            },
            None => None,
        }
        .or_else(|| CMap::predefined("Identity-H"));
        // Largeurs /W et /DW (§9.7.4.3).
        let default_width = doc
            .dict_get(&descendant, "DW")?
            .and_then(|o| o.as_f64())
            .unwrap_or(1000.0)
            / 1000.0;
        let mut cid_widths = HashMap::new();
        if let Some(w) = doc.dict_get(&descendant, "W")? {
            if let Some(items) = w.as_array() {
                let vals: Vec<Object> = items
                    .iter()
                    .filter_map(|o| doc.resolve(o).ok().map(|r| (*r).clone()))
                    .collect();
                let mut i = 0;
                while i < vals.len() {
                    let Some(first) = vals[i].as_f64() else {
                        i += 1;
                        continue;
                    };
                    if let Some(Object::Array(ws)) = vals.get(i + 1) {
                        for (k, wv) in ws.iter().enumerate() {
                            if let Some(wv) = doc.resolve(wv).ok().and_then(|o| o.as_f64()) {
                                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                                let cid = first.max(0.0) as u32 + u32::try_from(k).unwrap_or(0);
                                cid_widths.insert(cid, wv / 1000.0);
                            }
                        }
                        i += 2;
                    } else if let (Some(last), Some(wv)) = (
                        vals.get(i + 1).and_then(Object::as_f64),
                        vals.get(i + 2).and_then(Object::as_f64),
                    ) {
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let (a, b) = (first.max(0.0) as u32, last.max(0.0) as u32);
                        if b >= a && b - a < 65536 {
                            for cid in a..=b {
                                cid_widths.insert(cid, wv / 1000.0);
                            }
                        }
                        i += 3;
                    } else {
                        i += 1;
                    }
                }
            }
        }
        // CIDToGIDMap.
        let cid_to_gid = match doc.dict_get(&descendant, "CIDToGIDMap")? {
            Some(m) if matches!(&*m, Object::Stream { .. }) => doc.stream_data(&m).ok().map(|d| {
                d.data
                    .chunks(2)
                    .map(|c| u16::from(c[0]) << 8 | u16::from(*c.get(1).unwrap_or(&0)))
                    .collect()
            }),
            _ => None,
        };
        let to_unicode = load_to_unicode(doc, dict);
        Ok(Self {
            kind: FontKind::Type0,
            base_font,
            program,
            substituted,
            // Aucune des quatorze n'est composite : leurs tables ne
            // s'appliquent pas ici, où les largeurs viennent de /W et /DW.
            standard: None,
            font_matrix: Matrix::IDENTITY,
            flags,
            default_width,
            simple_widths: HashMap::new(),
            cid_widths,
            encoding_names: Vec::new(),
            has_encoding: true,
            cmap,
            cid_to_gid,
            gid_cache: std::cell::RefCell::new(HashMap::new()),
            path_cache: std::cell::RefCell::new(HashMap::new()),
            to_unicode,
        })
    }

    /// Vrai pour une police composite.
    #[must_use]
    pub fn is_composite(&self) -> bool {
        self.kind == FontKind::Type0
    }

    /// Vrai pour une police Type 3.
    #[must_use]
    pub fn is_type3(&self) -> bool {
        self.kind == FontKind::Type3
    }

    /// Découpe une chaîne en glyphes avec leurs largeurs.
    #[must_use]
    pub fn decode(&self, bytes: &[u8]) -> Vec<GlyphRef> {
        self.decode_spans(bytes)
            .into_iter()
            .map(|(g, _)| g)
            .collect()
    }

    /// Comme [`Self::decode`], en indiquant aussi le nombre d'octets
    /// consommés par chaque code (1 pour une police simple, 1 à 4 pour une
    /// composite selon sa CMap).
    ///
    /// Nécessaire pour retrouver les octets exacts d'un glyphe dans une
    /// chaîne, donc pour la réécriture chirurgicale des flux de contenu
    /// (`acrux_features::edit_text`).
    #[must_use]
    pub fn decode_spans(&self, bytes: &[u8]) -> Vec<(GlyphRef, usize)> {
        if let Some(cmap) = &self.cmap {
            return cmap
                .decode(bytes)
                .into_iter()
                .map(|(code, cid, nbytes)| {
                    (
                        GlyphRef {
                            code,
                            cid,
                            width: self
                                .cid_widths
                                .get(&cid)
                                .copied()
                                .unwrap_or(self.default_width),
                            is_space: nbytes == 1 && code == 32,
                        },
                        nbytes,
                    )
                })
                .collect();
        }
        bytes
            .iter()
            .map(|&b| {
                let code = u32::from(b);
                (
                    GlyphRef {
                        code,
                        cid: code,
                        width: self.width_of_code(code),
                        is_space: code == 32,
                    },
                    1,
                )
            })
            .collect()
    }

    /// Largeur d'avance (espace texte, taille 1) d'un code ou d'un CID.
    ///
    /// Utilisée par l'édition de texte pour mesurer une chaîne qu'elle vient
    /// d'encoder, sans repasser par [`Self::decode`].
    #[must_use]
    pub fn width_of(&self, code_or_cid: u32) -> f64 {
        if self.is_composite() {
            return self
                .cid_widths
                .get(&code_or_cid)
                .copied()
                .unwrap_or(self.default_width);
        }
        self.width_of_code(code_or_cid)
    }

    fn width_of_code(&self, code: u32) -> f64 {
        if let Some(w) = self.simple_widths.get(&code) {
            return *w;
        }
        if self.simple_widths.is_empty() {
            // Pas de /Widths. Un programme incorporé fait foi : c'est la
            // police même du document.
            if !self.substituted {
                if let Some(w) = self.program_advance(code) {
                    return w;
                }
            }
            // Sinon, si c'est une des quatorze polices standard, sa table
            // AFM fait foi — et elle ne dépend d'aucune police installée.
            if let Some(w) = self.standard_width(code) {
                return w;
            }
            // En dernier ressort, ce que mesure la police de substitution.
            if let Some(w) = self.program_advance(code) {
                return w;
            }
        }
        self.default_width
    }

    /// Largeur d'un code d'après la table de la police standard, en unités
    /// texte. `None` si la police n'en est pas une, ou si le glyphe lui est
    /// inconnu.
    fn standard_width(&self, code: u32) -> Option<f64> {
        let standard = self.standard?;
        let name = self.glyph_name(code)?;
        standard.width_by_name(name).map(|w| w / 1000.0)
    }

    /// Largeur d'avance depuis le programme de police (unités texte).
    fn program_advance(&self, code: u32) -> Option<f64> {
        if matches!(self.program, Program::Fallback) {
            let name = self.glyph_name(code)?;
            return fallback::advance(name).map(|a| a / fallback::units_per_em());
        }
        let gid = self.gid_for(code)?;
        match &self.program {
            Program::TrueType(tt) => {
                let adv = f64::from(tt.advance(u16::try_from(gid).ok()?)?);
                Some(adv / f64::from(tt.units_per_em().max(1)))
            }
            Program::Cff(cff) => {
                let adv = cff.advance(gid)?;
                Some(
                    cff.font_matrix()
                        .apply_vector(acrux_core::Point::new(adv, 0.0))
                        .x,
                )
            }
            Program::Type1(t1) => {
                let name = self.encoding_names.get(code as usize).cloned().flatten()?;
                let adv = t1.advance_by_name(&name)?;
                Some(
                    t1.font_matrix()
                        .apply_vector(acrux_core::Point::new(adv, 0.0))
                        .x,
                )
            }
            _ => None,
        }
    }

    /// Nom de glyphe d'un code (polices simples).
    #[must_use]
    pub fn glyph_name(&self, code: u32) -> Option<&str> {
        self.encoding_names
            .get(code as usize)
            .and_then(|n| n.as_deref())
    }

    /// Identifiant de glyphe dans le programme pour un code (simples) ou un CID (composites).
    #[must_use]
    pub fn gid_for(&self, code_or_cid: u32) -> Option<u32> {
        if let Some(g) = self.gid_cache.borrow().get(&code_or_cid) {
            return *g;
        }
        let g = self.compute_gid(code_or_cid);
        self.gid_cache.borrow_mut().insert(code_or_cid, g);
        g
    }

    #[allow(clippy::too_many_lines)]
    fn compute_gid(&self, code: u32) -> Option<u32> {
        if self.is_composite() {
            let cid = code;
            return match &self.program {
                Program::Cff(cff) => {
                    if cff.is_cid() {
                        cff.gid_by_cid(cid)
                    } else {
                        Some(cid)
                    }
                }
                Program::TrueType(tt) => {
                    if let Some(cff) = tt.cff() {
                        if cff.is_cid() {
                            return cff.gid_by_cid(cid);
                        }
                    }
                    match &self.cid_to_gid {
                        Some(map) => map.get(cid as usize).map(|g| u32::from(*g)),
                        None => Some(cid),
                    }
                }
                Program::Type1(_) | Program::Type3 { .. } | Program::Fallback | Program::None => {
                    Some(cid)
                }
            };
        }
        let name = self.glyph_name(code);
        let symbolic = self.flags & FLAG_SYMBOLIC != 0;
        match &self.program {
            Program::Type1(t1) => {
                // Les Type 1 sont adressés par nom ; le « gid » est l'index du nom.
                let name = name?;
                t1.gid_by_name(name).or_else(|| {
                    glyph_name_to_unicode(name)
                        .and_then(|u| t1.gid_by_name(&format!("uni{:04X}", u32::from(u))))
                })
            }
            Program::Cff(cff) => {
                if let Some(n) = name {
                    if let Some(g) = cff.gid_by_name(n) {
                        return Some(g);
                    }
                    if let Some(u) = glyph_name_to_unicode(n) {
                        if let Some(g) = cff.gid_by_name(&format!("uni{:04X}", u32::from(u))) {
                            return Some(g);
                        }
                    }
                }
                u8::try_from(code)
                    .ok()
                    .and_then(|c| cff.encoding_code_to_gid(c))
            }
            Program::TrueType(tt) => {
                // §9.6.6.4 : ordre de recherche selon le caractère symbolique.
                let via_unicode = |n: &str| {
                    glyph_name_to_unicode(n)
                        .and_then(|u| tt.unicode_to_gid(u))
                        .map(u32::from)
                };
                let via_30 = |c: u32| {
                    tt.cmap_lookup(3, 0, c)
                        .or_else(|| tt.cmap_lookup(3, 0, 0xF000 + (c & 0xFF)))
                        .or_else(|| tt.cmap_lookup(3, 0, 0xF100 + (c & 0xFF)))
                        .or_else(|| tt.cmap_lookup(3, 0, 0xF200 + (c & 0xFF)))
                        .map(u32::from)
                };
                let via_10 = |c: u32| tt.cmap_lookup(1, 0, c).map(u32::from);
                let by_name = |n: &str| tt.gid_by_name(n).map(u32::from);
                let gname_index = |n: &str| -> Option<u32> {
                    // Noms de la forme gXX / glyphXX / index XX.
                    let digits = n
                        .strip_prefix("glyph")
                        .or_else(|| n.strip_prefix("index"))
                        .or_else(|| n.strip_prefix('g'))?;
                    digits.parse::<u32>().ok()
                };
                let result = if symbolic && !self.has_encoding {
                    via_30(code)
                        .or_else(|| via_10(code))
                        .or_else(|| name.and_then(via_unicode))
                } else {
                    name.and_then(via_unicode)
                        .or_else(|| name.and_then(by_name))
                        .or_else(|| via_30(code))
                        .or_else(|| via_10(code))
                        .or_else(|| name.and_then(gname_index))
                };
                let result = result.or_else(|| {
                    if !tt.has_cmap(3, 1) && !tt.has_cmap(1, 0) && !tt.has_cmap(3, 0) {
                        // Sans cmap : le code est l'indice de glyphe (§9.6.6.4).
                        Some(code)
                    } else {
                        None
                    }
                });
                result.filter(|g| *g != 0 || code == 0)
            }
            // La police de secours s'adresse par nom, jamais par indice.
            Program::Type3 { .. } | Program::Fallback | Program::None => None,
        }
    }

    /// Contour d'un glyphe, mémorisé : c'est la forme qu'utilise le rendu.
    /// Le contour ne dépend que du CID, jamais de la position ni de la taille.
    #[must_use]
    pub fn glyph_path_cached(&self, glyph: &GlyphRef) -> Option<std::rc::Rc<Path>> {
        if let Some(p) = self.path_cache.borrow().get(&glyph.cid) {
            return p.clone();
        }
        let path = self.glyph_path(glyph).map(std::rc::Rc::new);
        self.path_cache.borrow_mut().insert(glyph.cid, path.clone());
        path
    }

    /// Contour d'un glyphe dans l'espace texte unitaire (1 = taille de police 1).
    #[must_use]
    pub fn glyph_path(&self, glyph: &GlyphRef) -> Option<Path> {
        if matches!(self.program, Program::Fallback) {
            let name = self.fallback_name(glyph)?;
            let unit = 1.0 / fallback::units_per_em();
            let path = fallback::glyph(&name)?
                .path
                .transform(&Matrix::scale(unit, unit));
            return match self.substitution_fit(glyph) {
                Some(fit) => Some(path.transform(&Matrix::scale(fit, 1.0))),
                None => Some(path),
            };
        }
        let gid = self.gid_for(glyph.cid)?;
        let path = match &self.program {
            Program::TrueType(tt) => {
                let p = tt.glyph_path(u16::try_from(gid).ok()?)?;
                let scale = 1.0 / f64::from(tt.units_per_em().max(1));
                p.transform(&Matrix::scale(scale, scale))
            }
            Program::Cff(cff) => {
                // FontMatrix non standard : on l'applique et on ramène à 1 unité.
                cff.glyph_path(gid)?.transform(&cff.font_matrix())
            }
            Program::Type1(t1) => t1
                .glyph_path_by_name(t1.glyph_name(gid)?)?
                .transform(&t1.font_matrix()),
            _ => return None,
        };
        match self.substitution_fit(glyph) {
            Some(fit) => Some(path.transform(&Matrix::scale(fit, 1.0))),
            None => Some(path),
        }
    }

    /// Nom du glyphe à demander à la police de secours.
    ///
    /// Pour une police simple c'est son nom d'encodage. Pour une composite, le
    /// code n'est qu'un identifiant interne au document : seul le
    /// `/ToUnicode` dit quelle lettre il désigne.
    fn fallback_name(&self, glyph: &GlyphRef) -> Option<String> {
        if !self.is_composite() {
            if let Some(name) = self.glyph_name(glyph.cid) {
                if fallback::has(name) {
                    return Some(name.to_string());
                }
            }
        }
        let text = self.to_unicode(glyph)?;
        fallback::name_for_char(text.chars().next()?).map(str::to_string)
    }

    /// Étirement horizontal à appliquer au contour d'un glyphe de
    /// substitution, pour qu'il occupe l'avance que le document déclare.
    ///
    /// Une police de remplacement n'a pas les chasses de la police absente.
    /// Dessinée telle quelle, chaque lettre est trop étroite ou trop large
    /// pour la place que le document lui a réservée : le texte se décolle de
    /// ses blancs, déborde de sa colonne, et deux lecteurs affichent deux
    /// pages différentes. Acrobat s'en tire avec des polices à axes variables
    /// qu'il instancie à la chasse voulue ; faute de pouvoir dessiner une
    /// chasse nouvelle, on étire le contour. La lettre est un peu condensée ou
    /// dilatée, mais elle **tombe au bon endroit**, et c'est cela qui décide
    /// si une page ressemble à son original.
    ///
    /// Ne s'applique qu'aux polices substituées : un programme incorporé est
    /// la police du document, on n'y touche pas.
    fn substitution_fit(&self, glyph: &GlyphRef) -> Option<f64> {
        if !self.substituted || glyph.width <= 0.0 {
            return None;
        }
        let natural = if matches!(self.program, Program::Fallback) {
            let name = self.fallback_name(glyph)?;
            fallback::advance(&name)? / fallback::units_per_em()
        } else {
            self.program_advance(glyph.cid)?
        };
        if natural <= 0.0 {
            return None;
        }
        let ratio = glyph.width / natural;
        // Hors de ces bornes, ce n'est plus un ajustement : le glyphe trouvé
        // n'a rien à voir avec celui que le document voulait. L'étirer le
        // rendrait illisible sans rien corriger.
        if (0.2..=5.0).contains(&ratio) {
            Some(ratio)
        } else {
            None
        }
    }

    /// Procédure de glyphe Type 3 pour un code.
    #[must_use]
    pub fn type3_proc(&self, doc: &Document, code: u32) -> Option<(Vec<u8>, Option<Dict>)> {
        let Program::Type3 {
            char_procs,
            resources,
        } = &self.program
        else {
            return None;
        };
        let name = self.glyph_name(code)?;
        let proc_obj = char_procs.get(&Name::new(name))?;
        let data = doc.stream_data(proc_obj).ok()?.data;
        Some((data, resources.clone()))
    }

    /// Texte Unicode d'un glyphe pour l'extraction (ToUnicode, puis nom de glyphe, puis code).
    #[must_use]
    pub fn to_unicode(&self, glyph: &GlyphRef) -> Option<String> {
        if let Some(tu) = &self.to_unicode {
            if let Some(s) = tu.to_unicode(glyph.code) {
                return Some(s);
            }
        }
        if !self.is_composite() {
            if let Some(n) = self.glyph_name(glyph.code) {
                if let Some(c) = glyph_name_to_unicode(n) {
                    return Some(c.to_string());
                }
            }
            if (32..127).contains(&glyph.code) {
                return char::from_u32(glyph.code).map(|c| c.to_string());
            }
        }
        None
    }
}

fn name_of(doc: &Document, dict: &Dict, key: &str) -> Option<String> {
    let o = doc.dict_get(dict, key).ok().flatten()?;
    o.as_name().map(Name::as_str)
}

fn matrix_from(doc: &Document, a: &[Object]) -> Matrix {
    let v: Vec<f64> = a
        .iter()
        .map(|o| doc.resolve(o).ok().and_then(|r| r.as_f64()).unwrap_or(0.0))
        .collect();
    if v.len() >= 6 {
        Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5])
    } else {
        Matrix::new(0.001, 0.0, 0.0, 0.001, 0.0, 0.0)
    }
}

fn load_to_unicode(doc: &Document, dict: &Dict) -> Option<CMap> {
    let tu = doc.dict_get(dict, "ToUnicode").ok().flatten()?;
    let data = doc.stream_data(&tu).ok()?.data;
    CMap::parse(&data).ok()
}

/// Charge le programme incorporé (`/FontFile`, `/FontFile2`, `/FontFile3`).
fn load_program(doc: &Document, descriptor: &Dict) -> Option<Program> {
    for key in ["FontFile2", "FontFile3", "FontFile"] {
        let Some(ff) = doc.dict_get(descriptor, key).ok().flatten() else {
            continue;
        };
        let Ok(data) = doc.stream_data(&ff) else {
            continue;
        };
        let subtype = ff
            .as_dict()
            .and_then(|d| d.get(&Name::new("Subtype")))
            .and_then(Object::as_name)
            .map(Name::as_str);
        let bytes = data.data;
        if bytes.is_empty() {
            continue;
        }
        // On se fie au contenu plus qu'à la clé : les fichiers réels se trompent souvent.
        if let Some(p) = program_from_bytes(&bytes, key, subtype.as_deref()) {
            return Some(p);
        }
    }
    None
}

fn program_from_bytes(bytes: &[u8], key: &str, subtype: Option<&str>) -> Option<Program> {
    let head = bytes.get(..4)?;
    if head == b"OTTO" || head == [0, 1, 0, 0] || head == b"true" || head == b"ttcf" {
        return TrueTypeFont::parse(bytes).ok().map(Program::TrueType);
    }
    if head[0] == b'%' || head[0] == 0x80 || bytes.starts_with(b"%!") {
        return Type1Font::parse(bytes).ok().map(Program::Type1);
    }
    if head[0] == 1 && head[1] == 0 {
        return CffFont::parse(bytes).ok().map(Program::Cff);
    }
    match (key, subtype) {
        ("FontFile2", _) | ("FontFile3", Some("OpenType")) => {
            TrueTypeFont::parse(bytes).ok().map(Program::TrueType)
        }
        ("FontFile3", _) => CffFont::parse(bytes).ok().map(Program::Cff),
        _ => Type1Font::parse(bytes).ok().map(Program::Type1),
    }
}

/// Choisit une police du système pour remplacer une police absente.
///
/// Trois recours, dans cet ordre : la police **exactement demandée**, si elle
/// est installée ; à défaut une police de même allure ; à défaut la nôtre.
/// Voir [`system`] pour le détail de la recherche par nom.
fn substitute(base_font: &str, flags: u32) -> Program {
    let lower = base_font.to_ascii_lowercase();
    let bold = lower.contains("bold")
        || lower.contains("black")
        || lower.contains("heavy")
        || lower.contains("semibold")
        || flags & FLAG_FORCE_BOLD != 0;
    let italic = lower.contains("italic") || lower.contains("oblique") || flags & FLAG_ITALIC != 0;
    let index = system::index();

    // 1. La police du document, retrouvée par son nom. Ce n'est alors pas une
    //    substitution du tout : c'est la bonne police.
    if let Some(face) = index.lookup(base_font, bold, italic) {
        if let Some(program) = from_face(face) {
            return program;
        }
    }

    // 2. Une police de même allure. Les drapeaux du descripteur (§9.8.2) sont
    //    plus fiables que le nom, mais beaucoup de fichiers ne les remplissent
    //    pas : on lit les deux.
    let kind = if lower.contains("dingbat") || lower.contains("wingding") {
        system::Kind::Dingbat
    } else if lower.contains("symbol") {
        system::Kind::Symbol
    } else if lower.contains("courier") || lower.contains("mono") || flags & FLAG_FIXED != 0 {
        system::Kind::Mono
    } else if lower.contains("times")
        || lower.contains("georgia")
        || lower.contains("garamond")
        || lower.contains("roman")
        || lower.contains("book")
        || (lower.contains("serif") && !lower.contains("sans"))
        || (flags & FLAG_SERIF != 0 && !lower.contains("arial") && !lower.contains("helvetica"))
    {
        system::Kind::Serif
    } else {
        system::Kind::Sans
    };
    if let Some(face) = index.lookup_generic(kind, bold, italic) {
        if let Some(program) = from_face(face) {
            return program;
        }
    }

    // 3. Aucune police sur la machine : la nôtre. Un fichier valide doit
    //    s'afficher, même là où il n'y a rien d'installé.
    Program::Fallback
}

/// Charge une police de l'index.
fn from_face(face: &system::Face) -> Option<Program> {
    let bytes = system::load(face)?;
    TrueTypeFont::parse_collection_index(&bytes, face.index)
        .ok()
        .map(Program::TrueType)
}

/// Table complète (256 codes) d'un encodage prédéfini.
fn base_table(base: encodings::BaseEncoding) -> Vec<Option<String>> {
    let f: fn(u8) -> Option<&'static str> = match base {
        encodings::BaseEncoding::Standard => encodings::standard,
        encodings::BaseEncoding::WinAnsi => encodings::win_ansi,
        encodings::BaseEncoding::MacRoman => encodings::mac_roman,
        encodings::BaseEncoding::MacExpert => encodings::mac_expert,
        encodings::BaseEncoding::PdfDoc => encodings::pdf_doc,
        encodings::BaseEncoding::Symbol => encodings::symbol,
        encodings::BaseEncoding::ZapfDingbats => encodings::zapf_dingbats,
    };
    (0..=255u8).map(|c| f(c).map(str::to_string)).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use acrux_document::Parser;

    fn doc() -> Document {
        Document::from_bytes(b"%PDF-1.7\n1 0 obj << /Type /Catalog >> endobj\n".to_vec()).unwrap()
    }

    fn font(src: &str) -> LoadedFont {
        let d = Parser::new(src.as_bytes()).parse_object().unwrap();
        LoadedFont::load(&doc(), d.as_dict().unwrap()).unwrap()
    }

    /// Largeur en x de l'encre d'un contour.
    fn ink_width(path: &Path) -> f64 {
        path.bounds().map_or(0.0, |b| b.x1 - b.x0)
    }

    #[test]
    fn la_police_exacte_prime_sur_toute_substitution() {
        let index = system::index();
        if index.is_empty() {
            return; // Machine sans polices : rien à trouver.
        }
        // Georgia n'est pas une des quatorze : la trouver, c'est l'avoir
        // cherchée par son nom dans les polices installées.
        let Some(face) = index.lookup("Georgia", false, false) else {
            return; // Georgia n'est pas installée ici.
        };
        assert_eq!(face.family, "georgia");
        let gras = index.lookup("Georgia-Bold", true, false);
        assert!(gras.is_some_and(|f| f.family == "georgia" && f.bold));
        // Un nom PostScript avec préfixe de sous-ensemble et suffixe de
        // fonderie doit retomber sur la même famille.
        let abrege = index.lookup("ABCDEF+Georgia-BoldItalic", true, true);
        assert!(abrege.is_some_and(|f| f.family == "georgia"));
    }

    #[test]
    fn une_machine_pourvue_nemploie_jamais_notre_police() {
        if system::index().is_empty() {
            return; // Machine sans polices : c'est justement là qu'elle sert.
        }
        // Une police installée, une absente, une qui n'existe nulle part, et
        // deux symboliques : aucune ne doit finir sur un dessin de notre main
        // tant que la machine a de vraies polices.
        for base in [
            "Calibri",
            "Palatino-Roman",
            "CettePoliceNexistePas",
            "Wingdings",
            "Symbol",
        ] {
            let f = font(&format!(
                "<< /Type /Font /Subtype /TrueType /BaseFont /{base} >>"
            ));
            assert!(
                !matches!(f.program, Program::Fallback),
                "{base} est tombée sur la police de secours"
            );
        }
    }

    #[test]
    fn les_polices_standard_ont_leurs_largeurs_sans_police_systeme() {
        // §9.6.2.2 : ni /Widths, ni /FontDescriptor, et c'est légal.
        let helvetica = font("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        assert_eq!(helvetica.standard, Some(Standard::Helvetica));
        let glyphes = helvetica.decode(b"A i ");
        // Helvetica : A = 667, espace = 278, i = 222 millièmes.
        assert!(
            (glyphes[0].width - 0.667).abs() < 1e-9,
            "{:?}",
            glyphes[0].width
        );
        assert!((glyphes[1].width - 0.278).abs() < 1e-9);
        assert!((glyphes[2].width - 0.222).abs() < 1e-9);
        // Times et Courier de même, sous leurs noms courants.
        let times = font("<< /Type /Font /Subtype /TrueType /BaseFont /TimesNewRomanPSMT >>");
        assert!((times.decode(b"m")[0].width - 0.778).abs() < 1e-9);
        let courier = font("<< /Type /Font /Subtype /TrueType /BaseFont /CourierNew >>");
        assert!((courier.decode(b"i")[0].width - 0.600).abs() < 1e-9);
        // Un /Widths explicite reste le maître : c'est le document qui parle.
        let declaree = font(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 65 /Widths [900] >>",
        );
        assert!((declaree.decode(b"A")[0].width - 0.9).abs() < 1e-9);
    }

    #[test]
    fn symbol_garde_son_propre_encodage() {
        let f = font("<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>");
        assert_eq!(f.standard, Some(Standard::Symbol));
        // Le code 0x61 est « alpha » dans Symbol, pas « a ».
        assert_eq!(f.glyph_name(0x61), Some("alpha"));
        assert!((f.decode(b"a")[0].width - 0.631).abs() < 1e-9);
        // ZapfDingbats de même : des noms de fleurons, jamais « a ».
        let z = font("<< /Type /Font /Subtype /Type1 /BaseFont /ZapfDingbats >>");
        let nom = z.glyph_name(0x61).unwrap_or_default();
        assert!(
            nom.starts_with('a') && nom.len() > 1 && nom[1..].chars().all(|c| c.is_ascii_digit()),
            "{nom}"
        );
    }

    #[test]
    fn un_glyphe_substitue_occupe_la_largeur_declaree() {
        // Deux fois la même lettre, deux largeurs déclarées différentes : le
        // contour dessiné doit suivre, sans quoi le texte se décolle de sa
        // place.
        let etroit =
            font("<< /Type /Font /Subtype /TrueType /BaseFont /Helvetica /FirstChar 65 /Widths [500] /FontDescriptor << /Flags 32 >> >>");
        let large =
            font("<< /Type /Font /Subtype /TrueType /BaseFont /Helvetica /FirstChar 65 /Widths [1000] /FontDescriptor << /Flags 32 >> >>");
        assert!(etroit.substituted && large.substituted);
        let (Some(a), Some(b)) = (
            etroit.glyph_path(&etroit.decode(b"A")[0]),
            large.glyph_path(&large.decode(b"A")[0]),
        ) else {
            return; // Aucune police de substitution sur cette machine.
        };
        let (wa, wb) = (ink_width(&a), ink_width(&b));
        assert!(wa > 0.0 && wb > 0.0);
        // Rapport des largeurs déclarées : 1000 / 500.
        assert!(
            (wb / wa - 2.0).abs() < 0.02,
            "rapport {} attendu 2",
            wb / wa
        );
    }

    #[test]
    fn simple_font_widths_and_encoding() {
        let f = font("<< /Type /Font /Subtype /TrueType /BaseFont /Arial /FirstChar 65 /Widths [700 650] /Encoding << /BaseEncoding /WinAnsiEncoding /Differences [66 /bullet] >> /FontDescriptor << /Flags 32 /MissingWidth 250 >> >>");
        assert_eq!(f.kind, FontKind::TrueType);
        assert!(f.substituted);
        let g = f.decode(b"AB ");
        assert_eq!(g.len(), 3);
        assert!((g[0].width - 0.7).abs() < 1e-9);
        assert!((g[1].width - 0.65).abs() < 1e-9);
        assert!(g[2].is_space);
        assert!((g[2].width - 0.25).abs() < 1e-9, "MissingWidth");
        assert_eq!(f.glyph_name(65), Some("A"));
        assert_eq!(f.glyph_name(66), Some("bullet"));
        assert_eq!(f.glyph_name(0xE9), Some("eacute"));
        assert_eq!(f.to_unicode(&g[0]), Some("A".into()));
    }

    #[test]
    fn type0_widths_ranges_and_arrays() {
        let f = font("<< /Type /Font /Subtype /Type0 /BaseFont /Foo /Encoding /Identity-H /DescendantFonts [ << /Type /Font /Subtype /CIDFontType2 /DW 500 /W [ 1 [100 200] 5 10 300 ] >> ] >>");
        assert!(f.is_composite());
        let g = f.decode(&[0, 1, 0, 2, 0, 7, 0, 99, 0, 32]);
        assert_eq!(g.len(), 5);
        assert_eq!(g[0].cid, 1);
        assert!((g[0].width - 0.1).abs() < 1e-9);
        assert!((g[1].width - 0.2).abs() < 1e-9);
        assert!((g[2].width - 0.3).abs() < 1e-9);
        assert!((g[3].width - 0.5).abs() < 1e-9, "DW");
        assert!(
            !g[4].is_space,
            "code 32 sur deux octets : pas d'espacement de mot"
        );
    }

    #[test]
    fn type3_font_matrix_widths() {
        let f = font("<< /Type /Font /Subtype /Type3 /FontMatrix [0.01 0 0 0.01 0 0] /CharProcs << >> /Encoding << /Differences [97 /square] >> /FirstChar 97 /Widths [50] >>");
        assert!(f.is_type3());
        let g = f.decode(b"a");
        assert!((g[0].width - 0.5).abs() < 1e-9);
        assert_eq!(f.glyph_name(97), Some("square"));
        assert_eq!(f.glyph_name(98), None);
    }

    #[test]
    fn substitution_picks_system_font_when_available() {
        let f = font("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>");
        assert!(f.substituted);
        // Sur une machine sans polices système, le programme est None : pas d'erreur.
        if matches!(f.program, Program::TrueType(_)) {
            let g = f.decode(b"H");
            assert!(
                g[0].width > 0.3,
                "largeur tirée du programme : {}",
                g[0].width
            );
            assert!(f.glyph_path(&g[0]).is_some());
        }
    }
}
