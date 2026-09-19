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
use std::path::PathBuf;
use std::rc::Rc;

use acrux_core::{Matrix, Path, Result};
use acrux_document::{Dict, Document, Name, Object};
use acrux_fonts::cmap::CMap;
use acrux_fonts::encodings::{self, glyph_name_to_unicode};
use acrux_fonts::{CffFont, TrueTypeFont, Type1Font};

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
    /// Aucun programme (police absente et substitution impossible).
    None,
}

impl std::fmt::Debug for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Program::TrueType(_) => write!(f, "TrueType"),
            Program::Cff(_) => write!(f, "CFF"),
            Program::Type1(_) => write!(f, "Type1"),
            Program::Type3 { .. } => write!(f, "Type3"),
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
        let flags = descriptor
            .as_ref()
            .and_then(|d| doc.dict_get(d, "Flags").ok().flatten())
            .and_then(|o| o.as_i64())
            .and_then(|v| u32::try_from(v).ok())
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
        if substituted && symbolic && builtin.is_none() {
            // Police symbolique non incorporée : on ne sait rien de son encodage.
            encoding_names = base_table(encodings::BaseEncoding::Standard);
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
            // Pas de /Widths (polices standard) : largeur du programme.
            if let Some(w) = self.program_advance(code) {
                return w;
            }
        }
        self.default_width
    }

    /// Largeur d'avance depuis le programme de police (unités texte).
    fn program_advance(&self, code: u32) -> Option<f64> {
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
                Program::Type1(_) | Program::Type3 { .. } | Program::None => Some(cid),
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
            Program::Type3 { .. } | Program::None => None,
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
        let gid = self.gid_for(glyph.cid)?;
        let (path, upem) = match &self.program {
            Program::TrueType(tt) => (
                tt.glyph_path(u16::try_from(gid).ok()?)?,
                f64::from(tt.units_per_em().max(1)),
            ),
            Program::Cff(cff) => {
                let p = cff.glyph_path(gid)?;
                let m = cff.font_matrix();
                // FontMatrix non standard : on l'applique et on ramène à 1 unité.
                return Some(p.transform(&m));
            }
            Program::Type1(t1) => {
                let p = t1.glyph_path_by_name(t1.glyph_name(gid)?)?;
                return Some(p.transform(&t1.font_matrix()));
            }
            _ => return None,
        };
        let scale = 1.0 / upem.max(1.0);
        Some(path.transform(&Matrix::scale(scale, scale)))
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

/// Répertoires de polices système, dans l'ordre de recherche.
fn system_font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(custom) = std::env::var("ACRUX_FONT_DIR") {
        dirs.push(PathBuf::from(custom));
    }
    if let Ok(windir) = std::env::var("WINDIR") {
        dirs.push(PathBuf::from(windir).join("Fonts"));
    }
    dirs.push(PathBuf::from("C:/Windows/Fonts"));
    dirs.push(PathBuf::from("/usr/share/fonts/truetype/liberation"));
    dirs.push(PathBuf::from("/usr/share/fonts/truetype/dejavu"));
    dirs.push(PathBuf::from("/usr/share/fonts"));
    dirs.push(PathBuf::from("/System/Library/Fonts"));
    dirs.push(PathBuf::from("/Library/Fonts"));
    dirs
}

thread_local! {
    static SUBSTITUTE_CACHE: std::cell::RefCell<HashMap<String, Option<Rc<Vec<u8>>>>> = std::cell::RefCell::new(HashMap::new());
}

/// Choisit et charge une police système pour remplacer une police absente
/// (§9.6.2.2 : les 14 polices standard ; sinon d'après le nom et les drapeaux).
fn substitute(base_font: &str, flags: u32) -> Program {
    let lower = base_font.to_ascii_lowercase();
    let bold = lower.contains("bold")
        || lower.contains("black")
        || lower.contains("heavy")
        || lower.contains("semibold")
        || flags & FLAG_FORCE_BOLD != 0;
    let italic = lower.contains("italic") || lower.contains("oblique") || flags & FLAG_ITALIC != 0;
    let fixed = lower.contains("courier") || lower.contains("mono") || flags & FLAG_FIXED != 0;
    let serif = lower.contains("times")
        || lower.contains("georgia")
        || lower.contains("book")
        || lower.contains("garamond")
        || lower.contains("roman")
        || lower.contains("serif") && !lower.contains("sans");
    let candidates: Vec<&str> = if lower.contains("symbol") {
        vec!["symbol.ttf"]
    } else if lower.contains("dingbat") || lower.contains("wingding") {
        vec!["wingding.ttf", "symbol.ttf"]
    } else if fixed {
        match (bold, italic) {
            (true, true) => vec![
                "courbi.ttf",
                "LiberationMono-BoldItalic.ttf",
                "DejaVuSansMono-BoldOblique.ttf",
            ],
            (true, false) => vec![
                "courbd.ttf",
                "LiberationMono-Bold.ttf",
                "DejaVuSansMono-Bold.ttf",
            ],
            (false, true) => vec![
                "couri.ttf",
                "LiberationMono-Italic.ttf",
                "DejaVuSansMono-Oblique.ttf",
            ],
            (false, false) => vec![
                "cour.ttf",
                "LiberationMono-Regular.ttf",
                "DejaVuSansMono.ttf",
            ],
        }
    } else if serif
        || (flags & FLAG_SERIF != 0 && !lower.contains("arial") && !lower.contains("helvetica"))
    {
        match (bold, italic) {
            (true, true) => vec![
                "timesbi.ttf",
                "LiberationSerif-BoldItalic.ttf",
                "DejaVuSerif-BoldItalic.ttf",
            ],
            (true, false) => vec![
                "timesbd.ttf",
                "LiberationSerif-Bold.ttf",
                "DejaVuSerif-Bold.ttf",
            ],
            (false, true) => vec![
                "timesi.ttf",
                "LiberationSerif-Italic.ttf",
                "DejaVuSerif-Italic.ttf",
            ],
            (false, false) => vec![
                "times.ttf",
                "LiberationSerif-Regular.ttf",
                "DejaVuSerif.ttf",
            ],
        }
    } else {
        match (bold, italic) {
            (true, true) => vec![
                "arialbi.ttf",
                "LiberationSans-BoldItalic.ttf",
                "DejaVuSans-BoldOblique.ttf",
            ],
            (true, false) => vec![
                "arialbd.ttf",
                "LiberationSans-Bold.ttf",
                "DejaVuSans-Bold.ttf",
            ],
            (false, true) => vec![
                "ariali.ttf",
                "LiberationSans-Italic.ttf",
                "DejaVuSans-Oblique.ttf",
            ],
            (false, false) => vec!["arial.ttf", "LiberationSans-Regular.ttf", "DejaVuSans.ttf"],
        }
    };
    for file in candidates {
        let key = file.to_string();
        let cached = SUBSTITUTE_CACHE.with(|c| c.borrow().get(&key).cloned());
        let bytes = if let Some(b) = cached {
            b
        } else {
            let found = system_font_dirs()
                .into_iter()
                .map(|d| d.join(file))
                .find(|p| p.is_file())
                .and_then(|p| std::fs::read(p).ok())
                .map(Rc::new);
            SUBSTITUTE_CACHE.with(|c| c.borrow_mut().insert(key, found.clone()));
            found
        };
        if let Some(bytes) = bytes {
            if let Ok(tt) = TrueTypeFont::parse(&bytes) {
                return Program::TrueType(tt);
            }
        }
    }
    Program::None
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
