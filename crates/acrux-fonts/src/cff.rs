//! Polices CFF (Compact Font Format, Adobe TN #5176) et charstrings Type 2
//! (Adobe TN #5177).
//!
//! Formes rencontrées dans les PDF (ISO 32000-2 §9.9, tableau 124) :
//! - `FontFile3` avec `/Subtype /Type1C` : CFF nu à un seul programme ;
//! - `FontFile3` avec `/Subtype /CIDFontType0C` : CFF CID-keyed (ROS,
//!   FDArray, FDSelect) ;
//! - `FontFile3` avec `/Subtype /OpenType` : table `CFF ` d'une police
//!   OpenType, extraite par [`crate::truetype::TrueTypeFont`].
//!
//! Structure lue (TN #5176 §2 à §19) : en-tête, Name INDEX, Top DICT INDEX,
//! String INDEX, Global Subr INDEX, charset, Encoding, CharStrings INDEX,
//! Private DICT et Local Subr INDEX, FDArray et FDSelect.
//!
//! Écarts assumés :
//! - l'Expert Encoding prédéfini (TN #5176 annexe B) n'est pas fourni :
//!   `encoding_code_to_gid` renvoie `None` pour ces polices, l'appelant
//!   passe par les noms de glyphes ;
//! - les charstrings Type 1 dans un CFF (`CharstringType 1`) sont
//!   interprétés par [`crate::type1`] ; les charstrings de type inconnu
//!   donnent des glyphes vides ;
//! - la FontMatrix d'un Font DICT de FDArray n'est pas composée avec celle
//!   du Top DICT (cas rarissime).

use std::collections::HashMap;

use acrux_core::{Error, Matrix, Path, Point, Result};

use crate::encodings;
use crate::glyph::GlyphProvider;
use crate::reader::Reader;
use crate::type1::{interpret_type1, Type1Env};

/// Nombre de chaînes standard (TN #5176 annexe A).
pub const STANDARD_STRING_COUNT: u16 = 391;

/// Profondeur maximale d'appel de sous-routines (TN #5177 annexe B : 10).
const MAX_SUBR_DEPTH: u32 = 10;
/// Profondeur maximale de composition `seac`.
const MAX_SEAC_DEPTH: u32 = 4;
/// Taille maximale de la pile d'opérandes (48 selon la spécification ;
/// tolérance pour les polices qui la dépassent légèrement).
const MAX_STACK: usize = 96;
/// Nombre maximal d'opérateurs exécutés pour un glyphe (borne contre les
/// sous-routines qui s'appellent en cascade).
const MAX_OPS_PER_GLYPH: u32 = 1_000_000;
/// Taille du tableau transitoire (`put` / `get`).
const TRANSIENT_SIZE: usize = 32;

/// Plage `[start, end)` dans les données de la police.
type Range = (usize, usize);

/// Dictionnaire CFF : opérateur → opérandes. Les opérateurs échappés
/// (`12 x`) sont codés `1200 + x`.
type Dict = HashMap<u16, Vec<f64>>;

/// Private DICT (TN #5176 §15) et sa Local Subr INDEX.
#[derive(Debug, Clone, Default)]
struct PrivateDict {
    subrs: Vec<Range>,
    default_width_x: f64,
    nominal_width_x: f64,
}

/// Encodage intégré (TN #5176 §12).
#[derive(Debug, Clone)]
enum Encoding {
    Standard,
    Expert,
    /// Code → GID (0 si non défini).
    Custom(Box<[u16; 256]>),
}

/// Police CFF chargée en mémoire.
#[derive(Debug, Clone)]
pub struct CffFont {
    data: Vec<u8>,
    font_name: Option<String>,
    charstrings: Vec<Range>,
    global_subrs: Vec<Range>,
    strings: Vec<Range>,
    /// GID → SID (police nommée) ou CID (police CID-keyed).
    charset: Vec<u16>,
    private: PrivateDict,
    fd_privates: Vec<PrivateDict>,
    fd_select: Vec<u8>,
    is_cid: bool,
    font_matrix: Matrix,
    encoding: Encoding,
    charstring_type: i64,
    name_to_gid: HashMap<String, u32>,
    cid_to_gid: HashMap<u32, u32>,
}

impl CffFont {
    /// Analyse une police CFF nue.
    ///
    /// # Errors
    /// `Error::Corrupt` si l'en-tête, le Top DICT ou l'INDEX CharStrings
    /// est illisible.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let corrupt = |m: &str| Error::Corrupt(format!("police CFF : {m}"));
        let mut r = Reader::new(data);
        // En-tête (§6) : major, minor, hdrSize, offSize.
        let (Some(major), Some(_minor), Some(hdr_size), Some(_off_size)) =
            (r.read_u8(), r.read_u8(), r.read_u8(), r.read_u8())
        else {
            return Err(corrupt("en-tête tronqué"));
        };
        if major != 1 {
            return Err(corrupt(&format!(
                "version majeure {major} non prise en charge"
            )));
        }
        r.seek(usize::from(hdr_size))
            .ok_or_else(|| corrupt("hdrSize hors des données"))?;
        let names = read_index(&mut r).ok_or_else(|| corrupt("Name INDEX illisible"))?;
        let top_dicts = read_index(&mut r).ok_or_else(|| corrupt("Top DICT INDEX illisible"))?;
        let strings = read_index(&mut r).ok_or_else(|| corrupt("String INDEX illisible"))?;
        let global_subrs = read_index(&mut r).unwrap_or_default();

        let top_range = *top_dicts
            .first()
            .ok_or_else(|| corrupt("Top DICT absent"))?;
        let top = parse_dict(slice(data, top_range));

        let charstrings_offset =
            dict_usize(&top, 17).ok_or_else(|| corrupt("CharStrings absent"))?;
        let charstrings = read_index(&mut Reader::at(data, charstrings_offset))
            .ok_or_else(|| corrupt("CharStrings INDEX illisible"))?;
        let num_glyphs = charstrings.len();
        let is_cid = top.contains_key(&1230);
        let charstring_type = top
            .get(&1206)
            .and_then(|v| v.first())
            .map_or(2, |v| to_i64(*v));
        let font_matrix = top
            .get(&1207)
            .filter(|v| v.len() == 6)
            .map_or(Matrix::new(0.001, 0.0, 0.0, 0.001, 0.0, 0.0), |v| {
                Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5])
            });

        let charset = parse_charset(data, dict_usize(&top, 15).unwrap_or(0), num_glyphs);
        let private = top
            .get(&18)
            .and_then(|v| parse_private(data, v))
            .unwrap_or_default();

        let mut font = CffFont {
            data: data.to_vec(),
            font_name: names
                .first()
                .map(|&r| String::from_utf8_lossy(slice(data, r)).into_owned()),
            charstrings,
            global_subrs,
            strings,
            charset,
            private,
            fd_privates: Vec::new(),
            fd_select: Vec::new(),
            is_cid,
            font_matrix,
            encoding: Encoding::Standard,
            charstring_type,
            name_to_gid: HashMap::new(),
            cid_to_gid: HashMap::new(),
        };

        if is_cid {
            font.parse_cid_structures(&top);
        }
        font.encoding = font.parse_encoding(dict_usize(&top, 16).unwrap_or(0));
        font.build_lookup_maps();
        Ok(font)
    }

    /// FDArray et FDSelect d'une police CID-keyed (§18, §19).
    fn parse_cid_structures(&mut self, top: &Dict) {
        let data = &self.data;
        if let Some(fdarray_offset) = dict_usize(top, 1236) {
            if let Some(fds) = read_index(&mut Reader::at(data, fdarray_offset)) {
                self.fd_privates = fds
                    .iter()
                    .map(|&r| {
                        let fd = parse_dict(slice(data, r));
                        fd.get(&18)
                            .and_then(|v| parse_private(data, v))
                            .unwrap_or_default()
                    })
                    .collect();
            }
        }
        let n = self.charstrings.len();
        self.fd_select = dict_usize(top, 1237)
            .and_then(|off| parse_fd_select(data, off, n))
            .unwrap_or_else(|| vec![0; n]);
    }

    /// Encoding (§12) : 0 standard, 1 expert, sinon offset d'un encodage
    /// personnalisé (formats 0 et 1, avec suppléments).
    fn parse_encoding(&self, offset: usize) -> Encoding {
        match offset {
            0 => return Encoding::Standard,
            1 => return Encoding::Expert,
            _ => {}
        }
        let mut table = Box::new([0u16; 256]);
        let mut r = Reader::at(&self.data, offset);
        let Some(format) = r.read_u8() else {
            return Encoding::Standard;
        };
        match format & 0x7F {
            0 => {
                let n = r.read_u8().unwrap_or(0);
                for gid in 1..=u16::from(n) {
                    let Some(code) = r.read_u8() else {
                        break;
                    };
                    table[usize::from(code)] = gid;
                }
            }
            1 => {
                let n_ranges = r.read_u8().unwrap_or(0);
                let mut gid: u16 = 1;
                for _ in 0..n_ranges {
                    let (Some(first), Some(n_left)) = (r.read_u8(), r.read_u8()) else {
                        break;
                    };
                    for k in 0..=u16::from(n_left) {
                        let code = u16::from(first) + k;
                        if code < 256 && table[usize::from(code)] == 0 {
                            table[usize::from(code)] = gid;
                        }
                        gid = gid.saturating_add(1);
                    }
                }
            }
            _ => return Encoding::Standard,
        }
        if format & 0x80 != 0 {
            let n_sups = r.read_u8().unwrap_or(0);
            for _ in 0..n_sups {
                let (Some(code), Some(sid)) = (r.read_u8(), r.read_u16()) else {
                    break;
                };
                if let Some(gid) = self.charset.iter().position(|&s| s == sid) {
                    table[usize::from(code)] = u16::try_from(gid).unwrap_or(0);
                }
            }
        }
        Encoding::Custom(table)
    }

    /// Tables nom → GID et CID → GID.
    fn build_lookup_maps(&mut self) {
        for (gid, &id) in self.charset.iter().enumerate() {
            let gid = u32::try_from(gid).unwrap_or(u32::MAX);
            if self.is_cid {
                self.cid_to_gid.entry(u32::from(id)).or_insert(gid);
            } else if let Some(name) = self.sid_to_string(id) {
                self.name_to_gid.entry(name).or_insert(gid);
            }
        }
    }

    /// Chaîne d'un SID : standard (< 391) ou String INDEX.
    #[must_use]
    pub fn sid_to_string(&self, sid: u16) -> Option<String> {
        if sid < STANDARD_STRING_COUNT {
            return Some(STANDARD_STRINGS[usize::from(sid)].to_string());
        }
        let r = *self.strings.get(usize::from(sid - STANDARD_STRING_COUNT))?;
        Some(String::from_utf8_lossy(slice(&self.data, r)).into_owned())
    }

    /// Nom de la police (Name INDEX).
    #[must_use]
    pub fn font_name(&self) -> Option<&str> {
        self.font_name.as_deref()
    }

    /// Nombre de glyphes (CharStrings INDEX).
    #[must_use]
    pub fn glyph_count(&self) -> u32 {
        u32::try_from(self.charstrings.len()).unwrap_or(u32::MAX)
    }

    /// FontMatrix (Top DICT), `[0.001 0 0 0.001 0 0]` par défaut.
    #[must_use]
    pub fn font_matrix(&self) -> Matrix {
        self.font_matrix
    }

    /// Vrai pour une police CID-keyed (opérateur ROS présent).
    #[must_use]
    pub fn is_cid(&self) -> bool {
        self.is_cid
    }

    /// SID (police nommée) ou CID (police CID-keyed) du glyphe.
    #[must_use]
    pub fn charset_sid(&self, gid: u32) -> Option<u16> {
        self.charset.get(usize::try_from(gid).ok()?).copied()
    }

    /// Nom du glyphe (police nommée uniquement).
    #[must_use]
    pub fn glyph_name(&self, gid: u32) -> Option<String> {
        if self.is_cid {
            return None;
        }
        self.sid_to_string(self.charset_sid(gid)?)
    }

    /// GID d'un nom de glyphe (police nommée).
    #[must_use]
    pub fn gid_by_name(&self, name: &str) -> Option<u32> {
        self.name_to_gid.get(name).copied()
    }

    /// GID d'un CID via le charset (police CID-keyed) ; pour une police
    /// nommée, le CID est le GID (ISO 32000-2 §9.7.4.2).
    #[must_use]
    pub fn gid_by_cid(&self, cid: u32) -> Option<u32> {
        if self.is_cid {
            self.cid_to_gid.get(&cid).copied()
        } else {
            (cid < self.glyph_count()).then_some(cid)
        }
    }

    /// Alias de [`Self::gid_by_cid`].
    #[must_use]
    pub fn cid_to_gid(&self, cid: u32) -> Option<u32> {
        self.gid_by_cid(cid)
    }

    /// Vrai si la police porte un encodage personnalisé (et non l'encodage
    /// standard implicite).
    #[must_use]
    pub fn has_custom_encoding(&self) -> bool {
        matches!(self.encoding, Encoding::Custom(_))
    }

    /// GID d'un code de l'encodage intégré (§12). `None` si le code n'est
    /// pas mappé.
    #[must_use]
    pub fn encoding_code_to_gid(&self, code: u8) -> Option<u32> {
        match &self.encoding {
            Encoding::Custom(table) => {
                let gid = table[usize::from(code)];
                (gid != 0).then_some(u32::from(gid))
            }
            Encoding::Standard => self.gid_by_name(encodings::standard(code)?),
            Encoding::Expert => None,
        }
    }

    /// Private DICT applicable à un glyphe (FDSelect pour les CID).
    fn private_for(&self, gid: usize) -> &PrivateDict {
        if self.is_cid && !self.fd_privates.is_empty() {
            let fd = self.fd_select.get(gid).copied().unwrap_or(0);
            return self
                .fd_privates
                .get(usize::from(fd))
                .unwrap_or(&self.fd_privates[0]);
        }
        &self.private
    }

    /// Exécute le charstring d'un glyphe.
    fn run_glyph(&self, gid: u32, seac_depth: u32) -> Option<GlyphResult> {
        let index = usize::try_from(gid).ok()?;
        let range = *self.charstrings.get(index)?;
        let code = slice(&self.data, range);
        let private = self.private_for(index);
        if self.charstring_type == 1 {
            let env = CffType1Env {
                font: self,
                subrs: &private.subrs,
            };
            let g = interpret_type1(code, &env, 0);
            return Some(GlyphResult {
                path: g.path,
                width: g.width,
            });
        }
        if self.charstring_type != 2 {
            return Some(GlyphResult {
                path: Path::new(),
                width: private.default_width_x,
            });
        }
        let mut ctx = Type2Context {
            font: self,
            local_subrs: &private.subrs,
            path: Path::new(),
            x: 0.0,
            y: 0.0,
            stack: Vec::with_capacity(48),
            nstems: 0,
            width: None,
            width_parsed: false,
            transient: [0.0; TRANSIENT_SIZE],
            open: false,
            ops: 0,
            seac_depth,
            nominal_width_x: private.nominal_width_x,
        };
        ctx.run(code, 0);
        if ctx.open {
            ctx.path.close();
        }
        Some(GlyphResult {
            path: ctx.path,
            width: ctx.width.unwrap_or(private.default_width_x),
        })
    }

    /// Contour d'un glyphe en unités de charstring (à transformer par
    /// [`Self::font_matrix`]).
    #[must_use]
    pub fn glyph_path(&self, gid: u32) -> Option<Path> {
        self.run_glyph(gid, 0).map(|g| g.path)
    }

    /// Avance horizontale en unités de charstring (largeur du charstring,
    /// sinon `defaultWidthX`).
    #[must_use]
    pub fn advance(&self, gid: u32) -> Option<f64> {
        self.run_glyph(gid, 0).map(|g| g.width)
    }
}

/// Résultat de l'exécution d'un charstring.
struct GlyphResult {
    path: Path,
    width: f64,
}

/// Environnement Type 1 pour les CFF à `CharstringType 1`.
struct CffType1Env<'a> {
    font: &'a CffFont,
    subrs: &'a [Range],
}

impl Type1Env for CffType1Env<'_> {
    fn subr(&self, index: usize) -> Option<&[u8]> {
        self.subrs.get(index).map(|&r| slice(&self.font.data, r))
    }

    fn charstring_by_standard_code(&self, code: u8) -> Option<&[u8]> {
        let gid = self.font.gid_by_name(encodings::standard(code)?)?;
        let range = *self.font.charstrings.get(usize::try_from(gid).ok()?)?;
        Some(slice(&self.font.data, range))
    }
}

/// Tranche bornée (les plages viennent d'INDEX déjà validés, mais on
/// reste défensif).
fn slice(data: &[u8], (start, end): Range) -> &[u8] {
    data.get(start..end).unwrap_or(&[])
}

fn to_i64(v: f64) -> i64 {
    #[allow(clippy::cast_possible_truncation)] // borné par le clamp
    let r = v.clamp(-9.0e15, 9.0e15) as i64;
    r
}

/// Opérande entier positif d'un dictionnaire, converti en `usize`.
fn dict_usize(dict: &Dict, op: u16) -> Option<usize> {
    let v = *dict.get(&op)?.first()?;
    usize::try_from(to_i64(v)).ok()
}

/// INDEX (§5) : renvoie les plages absolues des éléments et place le
/// lecteur après l'INDEX. Les offsets incohérents sont tronqués.
fn read_index(r: &mut Reader<'_>) -> Option<Vec<Range>> {
    let count = usize::from(r.read_u16()?);
    if count == 0 {
        return Some(Vec::new());
    }
    let off_size = r.read_u8()?;
    if !(1..=4).contains(&off_size) {
        return None;
    }
    let mut offsets = Vec::with_capacity(count + 1);
    for _ in 0..=count {
        offsets.push(usize::try_from(r.read_offset(off_size)?).ok()?);
    }
    // Les offsets sont relatifs à l'octet précédant les données (1-based).
    let base = r.pos().checked_sub(1)?;
    let len = r.data().len();
    let mut items = Vec::with_capacity(count);
    for w in offsets.windows(2) {
        let start = base.checked_add(w[0])?.min(len);
        let end = base.checked_add(w[1])?.min(len).max(start);
        items.push((start, end));
    }
    let end = base.checked_add(*offsets.last()?)?.min(len);
    r.seek(end)?;
    Some(items)
}

/// DICT (§4) : suite d'opérandes suivis d'un opérateur.
fn parse_dict(bytes: &[u8]) -> Dict {
    let mut dict = Dict::new();
    let mut operands: Vec<f64> = Vec::new();
    let mut r = Reader::new(bytes);
    while let Some(b0) = r.read_u8() {
        match b0 {
            0..=21 => {
                let op = if b0 == 12 {
                    1200 + u16::from(r.read_u8().unwrap_or(0))
                } else {
                    u16::from(b0)
                };
                dict.insert(op, std::mem::take(&mut operands));
            }
            28 => push_operand(&mut operands, r.read_i16().map(f64::from)),
            29 => push_operand(&mut operands, r.read_i32().map(f64::from)),
            30 => push_operand(&mut operands, read_real(&mut r)),
            32..=246 => push_operand(&mut operands, Some(f64::from(i32::from(b0) - 139))),
            247..=250 => {
                let b1 = r.read_u8().unwrap_or(0);
                let v = (i32::from(b0) - 247) * 256 + i32::from(b1) + 108;
                push_operand(&mut operands, Some(f64::from(v)));
            }
            251..=254 => {
                let b1 = r.read_u8().unwrap_or(0);
                let v = -(i32::from(b0) - 251) * 256 - i32::from(b1) - 108;
                push_operand(&mut operands, Some(f64::from(v)));
            }
            _ => {} // 22-27, 31, 255 : réservés
        }
    }
    dict
}

fn push_operand(operands: &mut Vec<f64>, v: Option<f64>) {
    if let Some(v) = v {
        if operands.len() < 48 {
            operands.push(v);
        }
    }
}

/// Nombre réel codé en quartets (§4, tableau 5).
fn read_real(r: &mut Reader<'_>) -> Option<f64> {
    let mut text = String::new();
    'outer: loop {
        let b = r.read_u8()?;
        for nibble in [b >> 4, b & 0x0F] {
            match nibble {
                0..=9 => text.push(char::from(b'0' + nibble)),
                0xA => text.push('.'),
                0xB => text.push('E'),
                0xC => text.push_str("E-"),
                0xE => text.push('-'),
                0xF => break 'outer,
                _ => {}
            }
            if text.len() > 64 {
                return None;
            }
        }
    }
    text.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Private DICT (§15) depuis les opérandes `[size offset]` du Top DICT.
fn parse_private(data: &[u8], operands: &[f64]) -> Option<PrivateDict> {
    if operands.len() < 2 {
        return None;
    }
    let size = usize::try_from(to_i64(operands[0])).ok()?;
    let offset = usize::try_from(to_i64(operands[1])).ok()?;
    let end = offset.checked_add(size)?.min(data.len());
    let dict = parse_dict(data.get(offset..end)?);
    let subrs = dict_usize(&dict, 19)
        .and_then(|rel| offset.checked_add(rel))
        .and_then(|abs| read_index(&mut Reader::at(data, abs)))
        .unwrap_or_default();
    let first = |op: u16| {
        dict.get(&op)
            .and_then(|v| v.first().copied())
            .unwrap_or(0.0)
    };
    Some(PrivateDict {
        subrs,
        default_width_x: first(20),
        nominal_width_x: first(21),
    })
}

/// Charset (§13) : GID → SID/CID. Offsets 0, 1, 2 : charsets prédéfinis
/// ISOAdobe, Expert, ExpertSubset (annexe C).
fn parse_charset(data: &[u8], offset: usize, num_glyphs: usize) -> Vec<u16> {
    let mut charset = Vec::with_capacity(num_glyphs);
    charset.push(0);
    match offset {
        0 => {
            // ISOAdobe : SID = GID pour les 229 premiers noms standard.
            charset.extend((1..num_glyphs).map(|g| u16::try_from(g).unwrap_or(u16::MAX)));
        }
        1 | 2 => {
            let table: &[u16] = if offset == 1 {
                &EXPERT_CHARSET
            } else {
                &EXPERT_SUBSET_CHARSET
            };
            charset.extend((1..num_glyphs).map(|g| table.get(g).copied().unwrap_or(0)));
        }
        _ => {
            let mut r = Reader::at(data, offset);
            match r.read_u8() {
                Some(0) => {
                    while charset.len() < num_glyphs {
                        let Some(sid) = r.read_u16() else {
                            break;
                        };
                        charset.push(sid);
                    }
                }
                Some(format @ (1 | 2)) => {
                    while charset.len() < num_glyphs {
                        let Some(first) = r.read_u16() else {
                            break;
                        };
                        let Some(n_left) = (if format == 1 {
                            r.read_u8().map(u16::from)
                        } else {
                            r.read_u16()
                        }) else {
                            break;
                        };
                        for k in 0..=n_left {
                            if charset.len() >= num_glyphs {
                                break;
                            }
                            charset.push(first.saturating_add(k));
                        }
                    }
                }
                _ => {}
            }
            // Charset tronqué : les glyphes restants n'ont pas de nom (SID 0).
            charset.resize(num_glyphs, 0);
        }
    }
    charset.truncate(num_glyphs.max(1));
    charset
}

/// FDSelect (§19), formats 0 et 3 : GID → index de Font DICT.
fn parse_fd_select(data: &[u8], offset: usize, num_glyphs: usize) -> Option<Vec<u8>> {
    let mut r = Reader::at(data, offset);
    let mut out = vec![0u8; num_glyphs];
    match r.read_u8()? {
        0 => {
            for slot in &mut out {
                let Some(fd) = r.read_u8() else {
                    break;
                };
                *slot = fd;
            }
        }
        3 => {
            let n_ranges = r.read_u16()?;
            let mut first = usize::from(r.read_u16()?);
            for _ in 0..n_ranges {
                let (Some(fd), Some(next)) = (r.read_u8(), r.read_u16()) else {
                    break;
                };
                let next = usize::from(next);
                for slot in out.iter_mut().take(next.min(num_glyphs)).skip(first) {
                    *slot = fd;
                }
                first = next;
            }
        }
        _ => return None,
    }
    Some(out)
}

/// Biais des numéros de sous-routines (TN #5177 §4.7).
fn subr_bias(count: usize) -> i64 {
    if count < 1240 {
        107
    } else if count < 33900 {
        1131
    } else {
        32768
    }
}

/// Interpréteur de charstrings Type 2 (TN #5177).
struct Type2Context<'a> {
    font: &'a CffFont,
    local_subrs: &'a [Range],
    path: Path,
    x: f64,
    y: f64,
    stack: Vec<f64>,
    nstems: usize,
    width: Option<f64>,
    width_parsed: bool,
    transient: [f64; TRANSIENT_SIZE],
    open: bool,
    ops: u32,
    seac_depth: u32,
    nominal_width_x: f64,
}

impl Type2Context<'_> {
    /// Largeur optionnelle en tête de pile (§3.1) : présente si la pile
    /// contient `even + 1` arguments (ou plus de `even` pour les
    /// opérateurs à nombre d'arguments variable).
    fn take_width(&mut self, has_width: bool) {
        if self.width_parsed {
            return;
        }
        self.width_parsed = true;
        if has_width && !self.stack.is_empty() {
            let w = self.stack.remove(0);
            self.width = Some(self.nominal_width_x + w);
        }
    }

    fn move_to(&mut self, x: f64, y: f64) {
        if self.open {
            self.path.close();
        }
        self.x = x;
        self.y = y;
        self.path.move_to(Point::new(x, y));
        self.open = true;
    }

    fn line_to(&mut self, x: f64, y: f64) {
        if !self.open {
            self.path.move_to(Point::new(self.x, self.y));
            self.open = true;
        }
        self.x = x;
        self.y = y;
        self.path.line_to(Point::new(x, y));
    }

    #[allow(clippy::too_many_arguments)] // six coordonnées d'une cubique
    fn curve_to(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, x: f64, y: f64) {
        if !self.open {
            self.path.move_to(Point::new(self.x, self.y));
            self.open = true;
        }
        self.x = x;
        self.y = y;
        self.path
            .curve_to(Point::new(x1, y1), Point::new(x2, y2), Point::new(x, y));
    }

    /// Courbe relative : trois deltas successifs.
    #[allow(clippy::too_many_arguments)] // six deltas d'une cubique
    fn rel_curve(&mut self, dx1: f64, dy1: f64, dx2: f64, dy2: f64, dx3: f64, dy3: f64) {
        let x1 = self.x + dx1;
        let y1 = self.y + dy1;
        let x2 = x1 + dx2;
        let y2 = y1 + dy2;
        self.curve_to(x1, y1, x2, y2, x2 + dx3, y2 + dy3);
    }

    /// Exécute un charstring ; renvoie `false` sur `endchar` (fin du glyphe).
    fn run(&mut self, code: &[u8], depth: u32) -> bool {
        if depth > MAX_SUBR_DEPTH {
            return false;
        }
        let mut r = Reader::new(code);
        while let Some(b0) = r.read_u8() {
            self.ops += 1;
            if self.ops > MAX_OPS_PER_GLYPH {
                return false;
            }
            match b0 {
                28 => self.push(r.read_i16().map(f64::from)),
                32..=246 => self.push(Some(f64::from(i32::from(b0) - 139))),
                247..=250 => {
                    let b1 = r.read_u8().unwrap_or(0);
                    self.push(Some(f64::from(
                        (i32::from(b0) - 247) * 256 + i32::from(b1) + 108,
                    )));
                }
                251..=254 => {
                    let b1 = r.read_u8().unwrap_or(0);
                    self.push(Some(f64::from(
                        -(i32::from(b0) - 251) * 256 - i32::from(b1) - 108,
                    )));
                }
                255 => self.push(r.read_fixed()),
                12 => {
                    let b1 = r.read_u8().unwrap_or(0);
                    if !self.escaped_operator(b1) {
                        return false;
                    }
                }
                19 | 20 => {
                    // hintmask / cntrmask : les vstem implicites précèdent.
                    self.take_width(self.stack.len() % 2 == 1);
                    self.nstems += self.stack.len() / 2;
                    self.stack.clear();
                    let mask_bytes = self.nstems.div_ceil(8);
                    if r.skip(mask_bytes).is_none() {
                        return false;
                    }
                }
                _ => {
                    if !self.operator(b0, depth) {
                        return false;
                    }
                }
            }
        }
        true
    }

    fn push(&mut self, v: Option<f64>) {
        if let Some(v) = v {
            if self.stack.len() < MAX_STACK {
                self.stack.push(v);
            }
        }
    }

    /// Opérateurs à un octet ; renvoie `false` pour arrêter le glyphe.
    fn operator(&mut self, op: u8, depth: u32) -> bool {
        match op {
            1 | 3 | 18 | 23 => {
                // hstem vstem hstemhm vstemhm
                self.take_width(self.stack.len() % 2 == 1);
                self.nstems += self.stack.len() / 2;
                self.stack.clear();
            }
            21 => {
                self.take_width(self.stack.len() > 2);
                let (dx, dy) = (self.arg(0), self.arg(1));
                self.move_to(self.x + dx, self.y + dy);
                self.stack.clear();
            }
            22 => {
                self.take_width(self.stack.len() > 1);
                let dx = self.arg(0);
                self.move_to(self.x + dx, self.y);
                self.stack.clear();
            }
            4 => {
                self.take_width(self.stack.len() > 1);
                let dy = self.arg(0);
                self.move_to(self.x, self.y + dy);
                self.stack.clear();
            }
            5 => {
                let args = std::mem::take(&mut self.stack);
                for pair in args.chunks_exact(2) {
                    self.line_to(self.x + pair[0], self.y + pair[1]);
                }
            }
            6 | 7 => {
                // hlineto / vlineto : alternance horizontale / verticale.
                let args = std::mem::take(&mut self.stack);
                let mut horizontal = op == 6;
                for &d in &args {
                    if horizontal {
                        self.line_to(self.x + d, self.y);
                    } else {
                        self.line_to(self.x, self.y + d);
                    }
                    horizontal = !horizontal;
                }
            }
            8 => {
                let args = std::mem::take(&mut self.stack);
                for c in args.chunks_exact(6) {
                    self.rel_curve(c[0], c[1], c[2], c[3], c[4], c[5]);
                }
            }
            24 => {
                // rcurveline : courbes puis une ligne.
                let args = std::mem::take(&mut self.stack);
                let n_curves = args.len().saturating_sub(2) / 6;
                for c in args[..n_curves * 6].chunks_exact(6) {
                    self.rel_curve(c[0], c[1], c[2], c[3], c[4], c[5]);
                }
                if let Some(l) = args.get(n_curves * 6..n_curves * 6 + 2) {
                    self.line_to(self.x + l[0], self.y + l[1]);
                }
            }
            25 => {
                // rlinecurve : lignes puis une courbe.
                let args = std::mem::take(&mut self.stack);
                let n_lines = args.len().saturating_sub(6) / 2;
                for l in args[..n_lines * 2].chunks_exact(2) {
                    self.line_to(self.x + l[0], self.y + l[1]);
                }
                if let Some(c) = args.get(n_lines * 2..n_lines * 2 + 6) {
                    self.rel_curve(c[0], c[1], c[2], c[3], c[4], c[5]);
                }
            }
            26 | 27 => self.vv_hh_curveto(op == 26),
            30 | 31 => self.vh_hv_curveto(op == 31),
            10 => return self.call_subr(depth, true),
            29 => return self.call_subr(depth, false),
            11 => return true, // return : fin de la sous-routine
            14 => {
                self.endchar();
                return false;
            }
            _ => self.stack.clear(), // opérateurs réservés : ignorés
        }
        true
    }

    /// `n`-ième argument (0 si absent).
    fn arg(&self, i: usize) -> f64 {
        self.stack.get(i).copied().unwrap_or(0.0)
    }

    /// vvcurveto (26) / hhcurveto (27).
    fn vv_hh_curveto(&mut self, vertical: bool) {
        let args = std::mem::take(&mut self.stack);
        let mut d1 = 0.0;
        let mut rest = &args[..];
        if rest.len() % 4 == 1 {
            d1 = rest[0];
            rest = &rest[1..];
        }
        for c in rest.chunks_exact(4) {
            if vertical {
                self.rel_curve(d1, c[0], c[1], c[2], 0.0, c[3]);
            } else {
                self.rel_curve(c[0], d1, c[1], c[2], c[3], 0.0);
            }
            d1 = 0.0;
        }
    }

    /// vhcurveto (30) / hvcurveto (31) : tangentes alternées, dernier
    /// argument optionnel pour la dernière courbe.
    fn vh_hv_curveto(&mut self, start_horizontal: bool) {
        let args = std::mem::take(&mut self.stack);
        let mut horizontal = start_horizontal;
        let mut i = 0;
        while i + 4 <= args.len() {
            let last = i + 8 > args.len();
            let dlast = if last && i + 5 == args.len() {
                args[i + 4]
            } else {
                0.0
            };
            let (d1, d2, d3, d4) = (args[i], args[i + 1], args[i + 2], args[i + 3]);
            if horizontal {
                self.rel_curve(d1, 0.0, d2, d3, dlast, d4);
            } else {
                self.rel_curve(0.0, d1, d2, d3, d4, dlast);
            }
            horizontal = !horizontal;
            i += 4;
        }
    }

    /// callsubr / callgsubr.
    fn call_subr(&mut self, depth: u32, local: bool) -> bool {
        let Some(n) = self.stack.pop() else {
            return true;
        };
        let subrs = if local {
            self.local_subrs
        } else {
            &self.font.global_subrs
        };
        let index = to_i64(n) + subr_bias(subrs.len());
        let Some(range) = usize::try_from(index).ok().and_then(|i| subrs.get(i)) else {
            return true;
        };
        let code = slice(&self.font.data, *range);
        self.run(code, depth + 1)
    }

    /// endchar, avec la forme `seac` à quatre arguments (TN #5177 annexe C).
    fn endchar(&mut self) {
        self.take_width(self.stack.len() == 1 || self.stack.len() == 5);
        if self.open {
            self.path.close();
            self.open = false;
        }
        if self.stack.len() >= 4 && self.seac_depth < MAX_SEAC_DEPTH {
            let n = self.stack.len();
            let (adx, ady) = (self.stack[n - 4], self.stack[n - 3]);
            let bchar = to_i64(self.stack[n - 2]);
            let achar = to_i64(self.stack[n - 1]);
            let font = self.font;
            let gid_of = |code: i64| {
                let name = encodings::standard(u8::try_from(code).ok()?)?;
                font.gid_by_name(name)
            };
            if let (Some(bgid), Some(agid)) = (gid_of(bchar), gid_of(achar)) {
                if let Some(base) = self.font.run_glyph(bgid, self.seac_depth + 1) {
                    self.path.append(&base.path);
                }
                if let Some(accent) = self.font.run_glyph(agid, self.seac_depth + 1) {
                    self.path
                        .append(&accent.path.transform(&Matrix::translate(adx, ady)));
                }
            }
        }
        self.stack.clear();
    }

    /// Opérateurs échappés (`12 x`) : flex (34 à 37) et arithmétique.
    fn escaped_operator(&mut self, op: u8) -> bool {
        if (34..=37).contains(&op) {
            self.flex_operator(op);
        } else {
            self.arithmetic_operator(op);
        }
        true
    }

    /// Opérateurs flex (TN #5177 §4.1) : chaque flex devient deux courbes.
    fn flex_operator(&mut self, op: u8) {
        match op {
            35 => {
                // flex : deux courbes, 13 arguments (fd ignoré).
                let a: Vec<f64> = (0..12).map(|i| self.arg(i)).collect();
                self.rel_curve(a[0], a[1], a[2], a[3], a[4], a[5]);
                self.rel_curve(a[6], a[7], a[8], a[9], a[10], a[11]);
                self.stack.clear();
            }
            34 => {
                // hflex : dx1 dx2 dy2 dx3 dx4 dx5 dx6
                let a: Vec<f64> = (0..7).map(|i| self.arg(i)).collect();
                let y0 = self.y;
                self.rel_curve(a[0], 0.0, a[1], a[2], a[3], 0.0);
                let dy = y0 - self.y;
                self.rel_curve(a[4], 0.0, a[5], dy, a[6], 0.0);
                self.stack.clear();
            }
            36 => {
                // hflex1 : dx1 dy1 dx2 dy2 dx3 dx4 dx5 dy5 dx6
                let a: Vec<f64> = (0..9).map(|i| self.arg(i)).collect();
                let y0 = self.y;
                self.rel_curve(a[0], a[1], a[2], a[3], a[4], 0.0);
                let c2y = self.y + a[7];
                self.rel_curve(a[5], 0.0, a[6], a[7], a[8], y0 - c2y);
                self.stack.clear();
            }
            37 => {
                // flex1 : dx1 dy1 dx2 dy2 dx3 dy3 dx4 dy4 dx5 dy5 d6
                let a: Vec<f64> = (0..11).map(|i| self.arg(i)).collect();
                let (x0, y0) = (self.x, self.y);
                let dx: f64 = a[0] + a[2] + a[4] + a[6] + a[8];
                let dy: f64 = a[1] + a[3] + a[5] + a[7] + a[9];
                self.rel_curve(a[0], a[1], a[2], a[3], a[4], a[5]);
                // Second segment : le point final revient sur l'axe de départ.
                let x1 = self.x + a[6];
                let y1 = self.y + a[7];
                let x2 = x1 + a[8];
                let y2 = y1 + a[9];
                let (ex, ey) = if dx.abs() > dy.abs() {
                    (x2 + a[10], y0)
                } else {
                    (x0, y2 + a[10])
                };
                self.curve_to(x1, y1, x2, y2, ex, ey);
                self.stack.clear();
            }
            _ => {}
        }
    }

    /// Opérateurs arithmétiques et de pile (TN #5177 §4.4, §4.5) ; les
    /// opérateurs inconnus ou obsolètes (`dotsection`) vident la pile.
    fn arithmetic_operator(&mut self, op: u8) {
        match op {
            3 => self.binary(|a, b| f64::from(u8::from(a != 0.0 && b != 0.0))),
            4 => self.binary(|a, b| f64::from(u8::from(a != 0.0 || b != 0.0))),
            5 => self.unary(|a| f64::from(u8::from(a == 0.0))),
            9 => self.unary(f64::abs),
            10 => self.binary(|a, b| a + b),
            11 => self.binary(|a, b| a - b),
            12 => self.binary(|a, b| if b == 0.0 { 0.0 } else { a / b }),
            14 => self.unary(|a| -a),
            #[allow(clippy::float_cmp)] // égalité exacte voulue par `eq`
            15 => self.binary(|a, b| f64::from(u8::from(a == b))),
            18 => {
                self.stack.pop();
            }
            20 => {
                // put : val i
                let i = self.stack.pop().map_or(0, to_i64);
                let v = self.stack.pop().unwrap_or(0.0);
                if let Some(slot) = usize::try_from(i)
                    .ok()
                    .and_then(|i| self.transient.get_mut(i))
                {
                    *slot = v;
                }
            }
            21 => {
                let i = self.stack.pop().map_or(0, to_i64);
                let v = usize::try_from(i)
                    .ok()
                    .and_then(|i| self.transient.get(i))
                    .copied()
                    .unwrap_or(0.0);
                self.push(Some(v));
            }
            22 => {
                // ifelse : s1 s2 v1 v2 → s1 si v1 <= v2 sinon s2
                let v2 = self.stack.pop().unwrap_or(0.0);
                let v1 = self.stack.pop().unwrap_or(0.0);
                let s2 = self.stack.pop().unwrap_or(0.0);
                let s1 = self.stack.pop().unwrap_or(0.0);
                self.push(Some(if v1 <= v2 { s1 } else { s2 }));
            }
            23 => self.push(Some(0.5)), // random : valeur déterministe
            24 => self.binary(|a, b| a * b),
            26 => self.unary(|a| if a < 0.0 { 0.0 } else { a.sqrt() }),
            27 => {
                let v = self.stack.last().copied().unwrap_or(0.0);
                self.push(Some(v));
            }
            28 => {
                let n = self.stack.len();
                if n >= 2 {
                    self.stack.swap(n - 1, n - 2);
                }
            }
            29 => {
                // index : copie l'élément i (0 = sommet) au sommet.
                let i = self.stack.pop().map_or(0, to_i64).max(0);
                let n = self.stack.len();
                let v = usize::try_from(i)
                    .ok()
                    .and_then(|i| n.checked_sub(1 + i))
                    .and_then(|k| self.stack.get(k))
                    .copied()
                    .unwrap_or(0.0);
                self.push(Some(v));
            }
            30 => {
                // roll : n j → fait tourner les n derniers éléments de j.
                let j = self.stack.pop().map_or(0, to_i64);
                let n = self.stack.pop().map_or(0, to_i64);
                if let Ok(n) = usize::try_from(n) {
                    let len = self.stack.len();
                    if n > 0 && n <= len {
                        let shift = j.rem_euclid(i64::try_from(n).unwrap_or(1));
                        let start = len - n;
                        self.stack[start..].rotate_right(usize::try_from(shift).unwrap_or(0));
                    }
                }
            }
            _ => self.stack.clear(),
        }
    }

    fn unary(&mut self, f: impl Fn(f64) -> f64) {
        let a = self.stack.pop().unwrap_or(0.0);
        self.push(Some(f(a)));
    }

    fn binary(&mut self, f: impl Fn(f64, f64) -> f64) {
        let b = self.stack.pop().unwrap_or(0.0);
        let a = self.stack.pop().unwrap_or(0.0);
        self.push(Some(f(a, b)));
    }
}

impl GlyphProvider for CffFont {
    fn glyph_path(&self, gid: u32) -> Option<Path> {
        CffFont::glyph_path(self, gid)
    }

    fn advance(&self, gid: u32) -> Option<f64> {
        CffFont::advance(self, gid)
    }

    fn units_per_em(&self) -> f64 {
        let a = self.font_matrix.a;
        if a > 0.0 {
            1.0 / a
        } else {
            1000.0
        }
    }

    fn glyph_count(&self) -> u32 {
        CffFont::glyph_count(self)
    }
}

/// Charset Expert prédéfini (TN #5176 annexe C), SID par GID à partir de 0.
static EXPERT_CHARSET: [u16; 166] = [
    0, 1, 229, 230, 231, 232, 233, 234, 235, 236, 237, 238, 13, 14, 15, 99, 239, 240, 241, 242,
    243, 244, 245, 246, 247, 248, 27, 28, 249, 250, 251, 252, 253, 254, 255, 256, 257, 258, 259,
    260, 261, 262, 263, 264, 265, 266, 109, 110, 267, 268, 269, 270, 271, 272, 273, 274, 275, 276,
    277, 278, 279, 280, 281, 282, 283, 284, 285, 286, 287, 288, 289, 290, 291, 292, 293, 294, 295,
    296, 297, 298, 299, 300, 301, 302, 303, 304, 305, 306, 307, 308, 309, 310, 311, 312, 313, 314,
    315, 316, 317, 318, 158, 155, 163, 319, 320, 321, 322, 323, 324, 325, 326, 150, 164, 169, 327,
    328, 329, 330, 331, 332, 333, 334, 335, 336, 337, 338, 339, 340, 341, 342, 343, 344, 345, 346,
    347, 348, 349, 350, 351, 352, 353, 354, 355, 356, 357, 358, 359, 360, 361, 362, 363, 364, 365,
    366, 367, 368, 369, 370, 371, 372, 373, 374, 375, 376, 377, 378,
];

/// Charset ExpertSubset prédéfini (TN #5176 annexe C).
static EXPERT_SUBSET_CHARSET: [u16; 87] = [
    0, 1, 231, 232, 235, 236, 237, 238, 13, 14, 15, 99, 239, 240, 241, 242, 243, 244, 245, 246,
    247, 248, 27, 28, 249, 250, 251, 253, 254, 255, 256, 257, 258, 259, 260, 261, 262, 263, 264,
    265, 266, 109, 110, 267, 268, 269, 270, 272, 300, 301, 302, 305, 314, 315, 158, 155, 163, 320,
    321, 322, 323, 324, 325, 326, 150, 164, 169, 327, 328, 329, 330, 331, 332, 333, 334, 335, 336,
    337, 338, 339, 340, 341, 342, 343, 344, 345, 346,
];

/// Les 391 chaînes standard (TN #5176 annexe A).
pub static STANDARD_STRINGS: [&str; 391] = [
    ".notdef",
    "space",
    "exclam",
    "quotedbl",
    "numbersign",
    "dollar",
    "percent",
    "ampersand",
    "quoteright",
    "parenleft",
    "parenright",
    "asterisk",
    "plus",
    "comma",
    "hyphen",
    "period",
    "slash",
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "colon",
    "semicolon",
    "less",
    "equal",
    "greater",
    "question",
    "at",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "bracketleft",
    "backslash",
    "bracketright",
    "asciicircum",
    "underscore",
    "quoteleft",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "braceleft",
    "bar",
    "braceright",
    "asciitilde",
    "exclamdown",
    "cent",
    "sterling",
    "fraction",
    "yen",
    "florin",
    "section",
    "currency",
    "quotesingle",
    "quotedblleft",
    "guillemotleft",
    "guilsinglleft",
    "guilsinglright",
    "fi",
    "fl",
    "endash",
    "dagger",
    "daggerdbl",
    "periodcentered",
    "paragraph",
    "bullet",
    "quotesinglbase",
    "quotedblbase",
    "quotedblright",
    "guillemotright",
    "ellipsis",
    "perthousand",
    "questiondown",
    "grave",
    "acute",
    "circumflex",
    "tilde",
    "macron",
    "breve",
    "dotaccent",
    "dieresis",
    "ring",
    "cedilla",
    "hungarumlaut",
    "ogonek",
    "caron",
    "emdash",
    "AE",
    "ordfeminine",
    "Lslash",
    "Oslash",
    "OE",
    "ordmasculine",
    "ae",
    "dotlessi",
    "lslash",
    "oslash",
    "oe",
    "germandbls",
    "onesuperior",
    "logicalnot",
    "mu",
    "trademark",
    "Eth",
    "onehalf",
    "plusminus",
    "Thorn",
    "onequarter",
    "divide",
    "brokenbar",
    "degree",
    "thorn",
    "threequarters",
    "twosuperior",
    "registered",
    "minus",
    "eth",
    "multiply",
    "threesuperior",
    "copyright",
    "Aacute",
    "Acircumflex",
    "Adieresis",
    "Agrave",
    "Aring",
    "Atilde",
    "Ccedilla",
    "Eacute",
    "Ecircumflex",
    "Edieresis",
    "Egrave",
    "Iacute",
    "Icircumflex",
    "Idieresis",
    "Igrave",
    "Ntilde",
    "Oacute",
    "Ocircumflex",
    "Odieresis",
    "Ograve",
    "Otilde",
    "Scaron",
    "Uacute",
    "Ucircumflex",
    "Udieresis",
    "Ugrave",
    "Yacute",
    "Ydieresis",
    "Zcaron",
    "aacute",
    "acircumflex",
    "adieresis",
    "agrave",
    "aring",
    "atilde",
    "ccedilla",
    "eacute",
    "ecircumflex",
    "edieresis",
    "egrave",
    "iacute",
    "icircumflex",
    "idieresis",
    "igrave",
    "ntilde",
    "oacute",
    "ocircumflex",
    "odieresis",
    "ograve",
    "otilde",
    "scaron",
    "uacute",
    "ucircumflex",
    "udieresis",
    "ugrave",
    "yacute",
    "ydieresis",
    "zcaron",
    "exclamsmall",
    "Hungarumlautsmall",
    "dollaroldstyle",
    "dollarsuperior",
    "ampersandsmall",
    "Acutesmall",
    "parenleftsuperior",
    "parenrightsuperior",
    "twodotenleader",
    "onedotenleader",
    "zerooldstyle",
    "oneoldstyle",
    "twooldstyle",
    "threeoldstyle",
    "fouroldstyle",
    "fiveoldstyle",
    "sixoldstyle",
    "sevenoldstyle",
    "eightoldstyle",
    "nineoldstyle",
    "commasuperior",
    "threequartersemdash",
    "periodsuperior",
    "questionsmall",
    "asuperior",
    "bsuperior",
    "centsuperior",
    "dsuperior",
    "esuperior",
    "isuperior",
    "lsuperior",
    "msuperior",
    "nsuperior",
    "osuperior",
    "rsuperior",
    "ssuperior",
    "tsuperior",
    "ff",
    "ffi",
    "ffl",
    "parenleftinferior",
    "parenrightinferior",
    "Circumflexsmall",
    "hyphensuperior",
    "Gravesmall",
    "Asmall",
    "Bsmall",
    "Csmall",
    "Dsmall",
    "Esmall",
    "Fsmall",
    "Gsmall",
    "Hsmall",
    "Ismall",
    "Jsmall",
    "Ksmall",
    "Lsmall",
    "Msmall",
    "Nsmall",
    "Osmall",
    "Psmall",
    "Qsmall",
    "Rsmall",
    "Ssmall",
    "Tsmall",
    "Usmall",
    "Vsmall",
    "Wsmall",
    "Xsmall",
    "Ysmall",
    "Zsmall",
    "colonmonetary",
    "onefitted",
    "rupiah",
    "Tildesmall",
    "exclamdownsmall",
    "centoldstyle",
    "Lslashsmall",
    "Scaronsmall",
    "Zcaronsmall",
    "Dieresissmall",
    "Brevesmall",
    "Caronsmall",
    "Dotaccentsmall",
    "Macronsmall",
    "figuredash",
    "hypheninferior",
    "Ogoneksmall",
    "Ringsmall",
    "Cedillasmall",
    "questiondownsmall",
    "oneeighth",
    "threeeighths",
    "fiveeighths",
    "seveneighths",
    "onethird",
    "twothirds",
    "zerosuperior",
    "foursuperior",
    "fivesuperior",
    "sixsuperior",
    "sevensuperior",
    "eightsuperior",
    "ninesuperior",
    "zeroinferior",
    "oneinferior",
    "twoinferior",
    "threeinferior",
    "fourinferior",
    "fiveinferior",
    "sixinferior",
    "seveninferior",
    "eightinferior",
    "nineinferior",
    "centinferior",
    "dollarinferior",
    "periodinferior",
    "commainferior",
    "Agravesmall",
    "Aacutesmall",
    "Acircumflexsmall",
    "Atildesmall",
    "Adieresissmall",
    "Aringsmall",
    "AEsmall",
    "Ccedillasmall",
    "Egravesmall",
    "Eacutesmall",
    "Ecircumflexsmall",
    "Edieresissmall",
    "Igravesmall",
    "Iacutesmall",
    "Icircumflexsmall",
    "Idieresissmall",
    "Ethsmall",
    "Ntildesmall",
    "Ogravesmall",
    "Oacutesmall",
    "Ocircumflexsmall",
    "Otildesmall",
    "Odieresissmall",
    "OEsmall",
    "Oslashsmall",
    "Ugravesmall",
    "Uacutesmall",
    "Ucircumflexsmall",
    "Udieresissmall",
    "Yacutesmall",
    "Thornsmall",
    "Ydieresissmall",
    "001.000",
    "001.001",
    "001.002",
    "001.003",
    "Black",
    "Bold",
    "Book",
    "Light",
    "Medium",
    "Regular",
    "Roman",
    "Semibold",
];

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]
pub(crate) mod tests {
    use super::*;
    use acrux_core::PathCommand;

    /// Encodage d'un entier en opérande de DICT sur 5 octets (taille fixe,
    /// pratique pour calculer les offsets à l'avance).
    fn dict_int(v: i32) -> Vec<u8> {
        let mut out = vec![29];
        out.extend_from_slice(&v.to_be_bytes());
        out
    }

    /// Encodage d'un réel en quartets (§4).
    fn dict_real(text: &str) -> Vec<u8> {
        let mut nibbles: Vec<u8> = Vec::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '0'..='9' => nibbles.push(c as u8 - b'0'),
                '.' => nibbles.push(0xA),
                'E' => {
                    if chars.peek() == Some(&'-') {
                        chars.next();
                        nibbles.push(0xC);
                    } else {
                        nibbles.push(0xB);
                    }
                }
                '-' => nibbles.push(0xE),
                _ => {}
            }
        }
        nibbles.push(0xF);
        if nibbles.len() % 2 == 1 {
            nibbles.push(0xF);
        }
        let mut out = vec![30];
        for pair in nibbles.chunks(2) {
            out.push((pair[0] << 4) | pair[1]);
        }
        out
    }

    fn dict_op(op: u16) -> Vec<u8> {
        if op >= 1200 {
            vec![12, (op - 1200) as u8]
        } else {
            vec![op as u8]
        }
    }

    /// INDEX avec offsets sur 4 octets.
    pub(crate) fn index(items: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(items.len() as u16).to_be_bytes());
        if items.is_empty() {
            return out;
        }
        out.push(4);
        let mut off = 1u32;
        out.extend_from_slice(&off.to_be_bytes());
        for it in items {
            off += it.len() as u32;
            out.extend_from_slice(&off.to_be_bytes());
        }
        for it in items {
            out.extend_from_slice(it);
        }
        out
    }

    /// Nombre en opérande de charstring Type 2.
    pub(crate) fn cs_num(v: i32) -> Vec<u8> {
        if (-107..=107).contains(&v) {
            vec![(v + 139) as u8]
        } else if (108..=1131).contains(&v) {
            let w = v - 108;
            vec![(w / 256 + 247) as u8, (w % 256) as u8]
        } else if (-1131..=-108).contains(&v) {
            let w = -v - 108;
            vec![(w / 256 + 251) as u8, (w % 256) as u8]
        } else {
            let mut out = vec![28];
            out.extend_from_slice(&(v as i16).to_be_bytes());
            out
        }
    }

    /// Assemble un charstring à partir de nombres et d'opérateurs
    /// (`Op(n)` = opérateur ; `Esc(n)` = opérateur échappé).
    pub(crate) enum T {
        N(i32),
        Op(u8),
        Esc(u8),
        Raw(u8),
    }

    pub(crate) fn charstring(tokens: &[T]) -> Vec<u8> {
        let mut out = Vec::new();
        for t in tokens {
            match t {
                T::N(v) => out.extend_from_slice(&cs_num(*v)),
                T::Op(o) => out.push(*o),
                T::Esc(o) => out.extend_from_slice(&[12, *o]),
                T::Raw(b) => out.push(*b),
            }
        }
        out
    }

    use T::{Esc, Op, Raw, N};

    /// Police nommée de test :
    /// gid 0 `.notdef` vide ; 1 `square` (rmoveto hlineto vlineto, largeur) ;
    /// 2 `curve` (rrcurveto + hvcurveto) ; 3 `viasubr` (callsubr local puis
    /// callgsubr) ; 4 `hinted` (hstem vstem hintmask) ; 5 `flexed` (hflex) ;
    /// 6 `arith` (add mul → carré de 60) ; 7 `Aacute` par seac.
    pub(crate) fn sample_font() -> Vec<u8> {
        // Sous-routine locale 0 (biais 107 → appelée avec -107) : carré.
        let local_subr = charstring(&[N(100), Op(6), N(100), Op(7), N(-100), Op(6), Op(11)]);
        // Sous-routine globale 0 : ligne verticale puis return.
        let global_subr = charstring(&[N(50), Op(7), Op(11)]);
        let glyphs = vec![
            charstring(&[Op(14)]),
            // Largeur 500 - nominal 100 = 400 encodé, puis carré.
            charstring(&[
                N(400),
                N(10),
                N(20),
                Op(21),
                N(100),
                Op(6),
                N(100),
                Op(7),
                N(-100),
                Op(6),
                Op(14),
            ]),
            charstring(&[
                N(0),
                N(0),
                Op(21),
                N(10),
                N(20),
                N(30),
                N(40),
                N(50),
                N(60),
                Op(8),
                N(10),
                N(20),
                N(30),
                N(40),
                Op(31),
                Op(14),
            ]),
            charstring(&[N(0), N(0), Op(21), N(-107), Op(10), N(-107), Op(29), Op(14)]),
            charstring(&[
                N(10),
                N(20),
                Op(1),
                N(30),
                N(40),
                Op(3),
                Op(19),
                Raw(0xC0),
                N(5),
                N(5),
                Op(21),
                N(10),
                Op(6),
                Op(14),
            ]),
            charstring(&[
                N(0),
                N(0),
                Op(21),
                N(10),
                N(20),
                N(30),
                N(40),
                N(50),
                N(60),
                N(70),
                Esc(34),
                Op(14),
            ]),
            charstring(&[
                N(0),
                N(0),
                Op(21),
                N(20),
                N(40),
                Esc(10), // 60
                Op(6),
                N(30),
                N(2),
                Esc(24), // 60
                Op(7),
                Op(14),
            ]),
            // seac : adx=100 ady=200 bchar=65 (A → square) achar=39 (quoteright → curve)
            charstring(&[N(100), N(200), N(65), N(39), Op(14)]),
        ];
        // String INDEX : SID 391.. = square, curve, viasubr, hinted, flexed, arith.
        let names = ["square", "curve", "viasubr", "hinted", "flexed", "arith"];
        let strings: Vec<Vec<u8>> = names.iter().map(|n| n.as_bytes().to_vec()).collect();
        // charset format 0 : gid 1 = « A » (SID 34) et gid 2 = « quoteright »
        // (SID 8) pour que le seac du glyphe 7 les retrouve par leur code
        // StandardEncoding ; gid 3..6 = noms personnalisés ; gid 7 = Aacute (171).
        let mut charset = vec![0u8];
        for sid in [34u16, 8, 393, 394, 395, 396, 171] {
            charset.extend_from_slice(&sid.to_be_bytes());
        }

        // Encodage personnalisé format 0 : codes 65 → gid 1, 66 → gid 2, 67 → 3
        // avec un supplément (0x80) : code 200 → SID 171 (gid 7).
        let encoding = vec![0x80, 3, 65, 66, 67, 1, 200, 0, 171];

        let private_dict = {
            let mut d = Vec::new();
            d.extend(dict_int(300));
            d.extend(dict_op(20)); // defaultWidthX
            d.extend(dict_int(100));
            d.extend(dict_op(21)); // nominalWidthX
            d
        };
        // Subrs suit immédiatement le Private DICT : offset relatif = taille
        // du Private DICT + 6 (opérande 5 octets + opérateur).
        let private_len = private_dict.len() + 6;
        let mut private = private_dict;
        private.extend(dict_int(private_len as i32));
        private.extend(dict_op(19));
        let local_subrs = index(&[local_subr]);

        build_cff(
            b"TestFont",
            &strings,
            &[global_subr],
            &charset,
            Some(&encoding),
            &glyphs,
            &private,
            &local_subrs,
            None,
        )
    }

    /// Assemble un CFF complet. Ordre : en-tête, Name, Top DICT, String,
    /// GSubr, charset, encoding, CharStrings, Private (+Subrs), [FDArray,
    /// FDSelect].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_cff(
        name: &[u8],
        strings: &[Vec<u8>],
        global_subrs: &[Vec<u8>],
        charset: &[u8],
        encoding: Option<&[u8]>,
        glyphs: &[Vec<u8>],
        private: &[u8],
        local_subrs: &[u8],
        cid: Option<(&[Vec<u8>], &[u8])>,
    ) -> Vec<u8> {
        let header = vec![1, 0, 4, 4];
        let name_index = index(&[name.to_vec()]);
        let string_index = index(strings);
        let gsubr_index = index(global_subrs);
        let charstrings_index = index(glyphs);

        // Taille du Top DICT (opérandes fixes de 5 octets) :
        // charset(15) encoding(16) CharStrings(17) Private(18: 2 opérandes)
        // FontMatrix (6 réels) ; CID : ROS (3 opérandes), FDArray, FDSelect.
        let font_matrix: Vec<u8> = ["0.001", "0", "0", "0.001", "0", "0"]
            .iter()
            .flat_map(|s| dict_real(s))
            .chain(dict_op(1207))
            .collect();
        let top_size = font_matrix.len()
            + (5 + 1) * 2
            + (5 + 5 + 1)
            + cid.map_or(0, |_| (5 * 3 + 2) + (5 + 2) * 2)
            + encoding.map_or(0, |_| 5 + 1);
        let top_index_size = 2 + 1 + 4 * 2 + top_size;
        let mut offset = header.len()
            + name_index.len()
            + top_index_size
            + string_index.len()
            + gsubr_index.len();
        let charset_offset = offset;
        offset += charset.len();
        let encoding_offset = offset;
        offset += encoding.map_or(0, <[u8]>::len);
        let charstrings_offset = offset;
        offset += charstrings_index.len();
        let private_offset = offset;
        offset += private.len() + local_subrs.len();
        let fdarray_offset = offset;

        let mut top = Vec::new();
        if let Some((fds, _)) = cid {
            top.extend(dict_int(0));
            top.extend(dict_int(0));
            top.extend(dict_int(0));
            top.extend(dict_op(1230)); // ROS
            let fdarray_index = index(fds);
            top.extend(dict_int(fdarray_offset as i32));
            top.extend(dict_op(1236));
            top.extend(dict_int((fdarray_offset + fdarray_index.len()) as i32));
            top.extend(dict_op(1237));
        }
        top.extend(font_matrix);
        top.extend(dict_int(charset_offset as i32));
        top.extend(dict_op(15));
        if encoding.is_some() {
            top.extend(dict_int(encoding_offset as i32));
            top.extend(dict_op(16));
        }
        top.extend(dict_int(charstrings_offset as i32));
        top.extend(dict_op(17));
        top.extend(dict_int(private.len() as i32));
        top.extend(dict_int(private_offset as i32));
        top.extend(dict_op(18));
        assert_eq!(top.len(), top_size, "taille du Top DICT mal calculée");
        let top_index = index(&[top]);
        assert_eq!(top_index.len(), top_index_size);

        let mut out = header;
        out.extend(name_index);
        out.extend(top_index);
        out.extend(string_index);
        out.extend(gsubr_index);
        out.extend_from_slice(charset);
        if let Some(e) = encoding {
            out.extend_from_slice(e);
        }
        assert_eq!(out.len(), charstrings_offset);
        out.extend(charstrings_index);
        out.extend_from_slice(private);
        out.extend_from_slice(local_subrs);
        if let Some((fds, fdselect)) = cid {
            out.extend(index(fds));
            out.extend_from_slice(fdselect);
        }
        out
    }

    /// Police CID-keyed : 4 glyphes, CID 0, 100, 101, 5000 (charset format
    /// 2), deux Font DICT (FDSelect format 3 : gid 0-1 → FD 0, gid 2-3 →
    /// FD 1), nominalWidthX différents pour vérifier la sélection.
    pub(crate) fn sample_cid_font() -> Vec<u8> {
        let glyphs = vec![
            charstring(&[Op(14)]),
            charstring(&[
                N(0),
                N(0),
                N(0),
                Op(21),
                N(100),
                Op(6),
                N(100),
                Op(7),
                Op(14),
            ]),
            charstring(&[N(0), N(0), N(0), Op(21), N(50), Op(6), Op(14)]),
            charstring(&[N(0), N(0), Op(21), N(-107), Op(10), Op(14)]),
        ];
        let mut charset = vec![2u8];
        charset.extend_from_slice(&100u16.to_be_bytes());
        charset.extend_from_slice(&1u16.to_be_bytes()); // 100, 101
        charset.extend_from_slice(&5000u16.to_be_bytes());
        charset.extend_from_slice(&0u16.to_be_bytes());

        // Private DICT du Top DICT (inutilisé pour les CID mais présent).
        let private = {
            let mut d = Vec::new();
            d.extend(dict_int(1));
            d.extend(dict_op(20));
            d
        };
        // Les Private DICT des FD sont placés dans la zone « private » du
        // constructeur : on les concatène et on calcule leurs offsets.
        let fd0_private = {
            let mut d = Vec::new();
            d.extend(dict_int(1000));
            d.extend(dict_op(21)); // nominalWidthX 1000
            d
        };
        let fd1_private_dict = {
            let mut d = Vec::new();
            d.extend(dict_int(2000));
            d.extend(dict_op(21)); // nominalWidthX 2000
            d.extend(dict_int(700));
            d.extend(dict_op(20)); // defaultWidthX 700
            d
        };
        let fd1_len = fd1_private_dict.len() + 6;
        let mut fd1_private = fd1_private_dict;
        fd1_private.extend(dict_int(fd1_len as i32));
        fd1_private.extend(dict_op(19));
        let fd1_subrs = index(&[charstring(&[N(30), Op(7), Op(11)])]);

        // Zone privée : [private][fd0_private][fd1_private][fd1_subrs]
        let mut private_zone = private.clone();
        let fd0_off_rel = private_zone.len();
        private_zone.extend_from_slice(&fd0_private);
        let fd1_off_rel = private_zone.len();
        private_zone.extend_from_slice(&fd1_private);
        private_zone.extend_from_slice(&fd1_subrs);

        // Pour connaître l'offset absolu de la zone privée, on construit une
        // première fois avec des FD vides puis on lit l'offset Private.
        let probe = build_cff(
            b"CIDTest",
            &[],
            &[],
            &charset,
            None,
            &glyphs,
            &private_zone,
            &[],
            Some((&[Vec::new(), Vec::new()], &[3, 0, 0, 0, 0, 0, 0])),
        );
        let top = {
            let mut r = Reader::at(&probe, 4);
            let _ = read_index(&mut r).unwrap();
            let t = read_index(&mut r).unwrap();
            parse_dict(slice(&probe, t[0]))
        };
        let private_abs = to_i64(top.get(&18).unwrap()[1]) as usize;
        let fd = |off: usize, len: usize| {
            let mut d = Vec::new();
            d.extend(dict_int(len as i32));
            d.extend(dict_int((private_abs + off) as i32));
            d.extend(dict_op(18));
            d
        };
        let fds = vec![
            fd(fd0_off_rel, fd0_private.len()),
            fd(fd1_off_rel, fd1_private.len()),
        ];
        // FDSelect format 3 : 2 plages, sentinelle 4.
        let fdselect = vec![3, 0, 2, 0, 0, 0, 0, 2, 1, 0, 4];
        build_cff(
            b"CIDTest",
            &[],
            &[],
            &charset,
            None,
            &glyphs,
            &private_zone,
            &[],
            Some((&fds, &fdselect)),
        )
    }

    fn square_commands(x0: f64, y0: f64, s: f64) -> Vec<PathCommand> {
        vec![
            PathCommand::MoveTo(Point::new(x0, y0)),
            PathCommand::LineTo(Point::new(x0 + s, y0)),
            PathCommand::LineTo(Point::new(x0 + s, y0 + s)),
            PathCommand::LineTo(Point::new(x0, y0 + s)),
            PathCommand::Close,
        ]
    }

    #[test]
    fn standard_strings_and_charsets_have_expected_sizes() {
        assert_eq!(STANDARD_STRINGS.len(), 391);
        assert_eq!(STANDARD_STRINGS[0], ".notdef");
        assert_eq!(STANDARD_STRINGS[1], "space");
        assert_eq!(STANDARD_STRINGS[34], "A");
        assert_eq!(STANDARD_STRINGS[66], "a");
        assert_eq!(STANDARD_STRINGS[228], "zcaron");
        assert_eq!(STANDARD_STRINGS[229], "exclamsmall");
        assert_eq!(STANDARD_STRINGS[378], "Ydieresissmall");
        assert_eq!(STANDARD_STRINGS[390], "Semibold");
        assert_eq!(EXPERT_CHARSET.len(), 166);
        assert_eq!(EXPERT_SUBSET_CHARSET.len(), 87);
        // L'encodage standard correspond aux SID 1..95 pour les codes 32..126.
        for code in 32u8..=126 {
            let name = encodings::standard(code).unwrap();
            assert_eq!(STANDARD_STRINGS[usize::from(code - 31)], name);
        }
    }

    #[test]
    fn header_dicts_and_names() {
        let font = CffFont::parse(&sample_font()).unwrap();
        assert_eq!(font.font_name(), Some("TestFont"));
        assert_eq!(font.glyph_count(), 8);
        assert!(!font.is_cid());
        assert_eq!(
            font.font_matrix(),
            Matrix::new(0.001, 0.0, 0.0, 0.001, 0.0, 0.0)
        );
        assert_eq!(font.glyph_name(1).as_deref(), Some("A"));
        assert_eq!(font.glyph_name(3).as_deref(), Some("viasubr"));
        assert_eq!(font.glyph_name(7).as_deref(), Some("Aacute"));
        assert_eq!(font.charset_sid(3), Some(393));
        assert_eq!(font.gid_by_name("hinted"), Some(4));
        assert_eq!(font.gid_by_name("A"), Some(1));
        assert_eq!(font.gid_by_name("nope"), None);
        assert_eq!(font.gid_by_cid(3), Some(3));
        assert_eq!(font.gid_by_cid(99), None);
        assert_eq!(font.sid_to_string(391).as_deref(), Some("square"));
        assert_eq!(font.sid_to_string(1000), None);
    }

    #[test]
    fn custom_encoding_with_supplement() {
        let font = CffFont::parse(&sample_font()).unwrap();
        assert!(font.has_custom_encoding());
        assert_eq!(font.encoding_code_to_gid(65), Some(1));
        assert_eq!(font.encoding_code_to_gid(67), Some(3));
        assert_eq!(font.encoding_code_to_gid(200), Some(7));
        assert_eq!(font.encoding_code_to_gid(68), None);
    }

    #[test]
    fn square_with_width() {
        let font = CffFont::parse(&sample_font()).unwrap();
        let path = font.glyph_path(1).unwrap();
        assert_eq!(path.commands(), square_commands(10.0, 20.0, 100.0));
        assert_eq!(font.advance(1), Some(500.0));
        // Pas de largeur dans le charstring : defaultWidthX.
        assert_eq!(font.advance(2), Some(300.0));
        assert!(font.glyph_path(0).unwrap().is_empty());
        assert!(font.glyph_path(8).is_none());
    }

    #[test]
    fn curves() {
        let font = CffFont::parse(&sample_font()).unwrap();
        let path = font.glyph_path(2).unwrap();
        let cmds = path.commands();
        assert_eq!(cmds[0], PathCommand::MoveTo(Point::new(0.0, 0.0)));
        assert_eq!(
            cmds[1],
            PathCommand::CurveTo(
                Point::new(10.0, 20.0),
                Point::new(40.0, 60.0),
                Point::new(90.0, 120.0)
            )
        );
        // hvcurveto 10 20 30 40 : c1 = (100,120), c2 = (120,150), fin = (120,190).
        assert_eq!(
            cmds[2],
            PathCommand::CurveTo(
                Point::new(100.0, 120.0),
                Point::new(120.0, 150.0),
                Point::new(120.0, 190.0)
            )
        );
        assert_eq!(cmds[3], PathCommand::Close);
    }

    #[test]
    fn local_and_global_subroutines() {
        let font = CffFont::parse(&sample_font()).unwrap();
        let path = font.glyph_path(3).unwrap();
        let cmds = path.commands();
        assert_eq!(cmds[0], PathCommand::MoveTo(Point::new(0.0, 0.0)));
        assert_eq!(cmds[1], PathCommand::LineTo(Point::new(100.0, 0.0)));
        assert_eq!(cmds[2], PathCommand::LineTo(Point::new(100.0, 100.0)));
        assert_eq!(cmds[3], PathCommand::LineTo(Point::new(0.0, 100.0)));
        assert_eq!(cmds[4], PathCommand::LineTo(Point::new(0.0, 150.0)));
        assert_eq!(cmds[5], PathCommand::Close);
    }

    #[test]
    fn hintmask_skips_mask_bytes() {
        let font = CffFont::parse(&sample_font()).unwrap();
        let path = font.glyph_path(4).unwrap();
        assert_eq!(
            path.commands(),
            &[
                PathCommand::MoveTo(Point::new(5.0, 5.0)),
                PathCommand::LineTo(Point::new(15.0, 5.0)),
                PathCommand::Close,
            ]
        );
        // Deux stems déclarés, pas de largeur (nombre pair d'arguments).
        assert_eq!(font.advance(4), Some(300.0));
    }

    #[test]
    fn hflex_is_two_curves_back_to_baseline() {
        let font = CffFont::parse(&sample_font()).unwrap();
        let path = font.glyph_path(5).unwrap();
        let cmds = path.commands();
        assert_eq!(
            cmds[1],
            PathCommand::CurveTo(
                Point::new(10.0, 0.0),
                Point::new(30.0, 30.0),
                Point::new(70.0, 30.0)
            )
        );
        assert_eq!(
            cmds[2],
            PathCommand::CurveTo(
                Point::new(120.0, 30.0),
                Point::new(180.0, 0.0),
                Point::new(250.0, 0.0)
            )
        );
    }

    #[test]
    fn arithmetic_operators() {
        let font = CffFont::parse(&sample_font()).unwrap();
        let path = font.glyph_path(6).unwrap();
        assert_eq!(
            path.commands()[1],
            PathCommand::LineTo(Point::new(60.0, 0.0))
        );
        assert_eq!(
            path.commands()[2],
            PathCommand::LineTo(Point::new(60.0, 60.0))
        );
    }

    #[test]
    fn seac_through_endchar() {
        let font = CffFont::parse(&sample_font()).unwrap();
        let path = font.glyph_path(7).unwrap();
        let cmds = path.commands();
        // Base « A » = carré en (10,20), accent « quoteright » = courbes
        // translatées de (100, 200).
        assert_eq!(&cmds[..5], &square_commands(10.0, 20.0, 100.0)[..]);
        assert_eq!(cmds[5], PathCommand::MoveTo(Point::new(100.0, 200.0)));
        assert_eq!(cmds.len(), 9);
    }

    #[test]
    fn cid_keyed_font() {
        let font = CffFont::parse(&sample_cid_font()).unwrap();
        assert!(font.is_cid());
        assert_eq!(font.glyph_count(), 4);
        assert_eq!(font.gid_by_cid(100), Some(1));
        assert_eq!(font.gid_by_cid(101), Some(2));
        assert_eq!(font.gid_by_cid(5000), Some(3));
        assert_eq!(font.cid_to_gid(5000), Some(3));
        assert_eq!(font.gid_by_cid(7), None);
        assert_eq!(font.charset_sid(3), Some(5000));
        assert_eq!(font.glyph_name(1), None);
        // FD 0 : nominalWidthX 1000, largeur codée 0 → 1000.
        assert_eq!(font.advance(1), Some(1000.0));
        // FD 1 : nominalWidthX 2000.
        assert_eq!(font.advance(2), Some(2000.0));
        // FD 1 sans largeur : defaultWidthX 700, et Subrs locales du FD 1.
        assert_eq!(font.advance(3), Some(700.0));
        let path = font.glyph_path(3).unwrap();
        assert_eq!(
            path.commands()[1],
            PathCommand::LineTo(Point::new(0.0, 30.0))
        );
        assert_eq!(font.encoding_code_to_gid(65), None);
    }

    #[test]
    fn predefined_charsets() {
        let glyphs = vec![charstring(&[Op(14)]); 5];
        let private = Vec::new();
        for (offset, expect_gid4) in [(0u8, 4u16), (1, 231), (2, 235)] {
            // charset = 1 octet inutilisé, on force l'offset via un Top
            // DICT manuel : plus simple de reconstruire ici.
            let data = build_cff(b"X", &[], &[], &[0], None, &glyphs, &private, &[], None);
            // Remplace l'opérande charset (offset absolu) par la valeur prédéfinie.
            let mut patched = data.clone();
            let mut r = Reader::at(&patched, 4);
            let _ = read_index(&mut r).unwrap();
            let top = read_index(&mut r).unwrap()[0];
            let dict = &data[top.0..top.1];
            let pos = dict
                .windows(6)
                .position(|w| w[5] == 15 && w[0] == 29)
                .unwrap();
            patched[top.0 + pos..top.0 + pos + 5].copy_from_slice(&[29, 0, 0, 0, offset]);
            let font = CffFont::parse(&patched).unwrap();
            assert_eq!(font.charset_sid(4), Some(expect_gid4), "charset {offset}");
        }
    }

    #[test]
    fn glyph_provider_trait() {
        let font = CffFont::parse(&sample_font()).unwrap();
        let p: &dyn GlyphProvider = &font;
        assert_eq!(p.glyph_count(), 8);
        assert_eq!(p.units_per_em(), 1000.0);
        assert_eq!(p.advance(1), Some(500.0));
        assert_eq!(p.glyph_path(1).unwrap().commands().len(), 5);
    }

    #[test]
    fn recursive_subroutine_terminates() {
        // Sous-routine locale qui s'appelle elle-même.
        let local = charstring(&[N(10), Op(6), N(-107), Op(10), Op(11)]);
        let glyphs = vec![charstring(&[N(0), N(0), Op(21), N(-107), Op(10), Op(14)])];
        let mut private = Vec::new();
        private.extend(dict_int(6));
        private.extend(dict_op(19));
        let data = build_cff(
            b"R",
            &[],
            &[],
            &[0],
            None,
            &glyphs,
            &private,
            &index(&[local]),
            None,
        );
        let font = CffFont::parse(&data).unwrap();
        let path = font.glyph_path(0).unwrap();
        assert!(!path.is_empty());
        assert!(path.commands().len() <= 20);
    }

    #[test]
    fn truncation_never_panics() {
        for data in [sample_font(), sample_cid_font()] {
            for n in 0..data.len() {
                if let Ok(font) = CffFont::parse(&data[..n]) {
                    for gid in 0..=font.glyph_count() {
                        let _ = font.glyph_path(gid);
                        let _ = font.advance(gid);
                        let _ = font.glyph_name(gid);
                    }
                    let _ = font.gid_by_name("A");
                    let _ = font.gid_by_cid(100);
                    let _ = font.encoding_code_to_gid(65);
                }
            }
        }
    }

    #[test]
    fn random_bytes_never_panic() {
        let mut seed: u32 = 0xDEAD_BEEF;
        for round in 0..300 {
            let len = (round * 5) % 200;
            let mut data = vec![0u8; len];
            for b in &mut data {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *b = (seed >> 24) as u8;
            }
            if data.len() >= 4 {
                data[..4].copy_from_slice(&[1, 0, 4, 4]);
            }
            if let Ok(font) = CffFont::parse(&data) {
                for gid in 0..font.glyph_count().min(32) {
                    let _ = font.glyph_path(gid);
                }
            }
        }
    }

    #[test]
    fn dict_real_roundtrip() {
        let bytes = dict_real("-0.00123E-2");
        let mut r = Reader::at(&bytes, 1);
        let v = read_real(&mut r).unwrap();
        assert!((v - (-0.00123e-2)).abs() < 1e-12);
        let d = parse_dict(&[dict_real("1.5"), dict_op(1207)].concat());
        assert_eq!(d.get(&1207), Some(&vec![1.5]));
        let d = parse_dict(&[cs_num(-1000), dict_int(70000), dict_op(5)].concat());
        assert_eq!(d.get(&5), Some(&vec![-1000.0, 70000.0]));
    }
}
