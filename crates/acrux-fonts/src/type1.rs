//! Polices Type 1 (« Adobe Type 1 Font Format », chapitres 2 à 8) :
//! déchiffrement eexec, charstrings Type 1, flex et hint replacement.
//!
//! Formes acceptées par [`Type1Font::parse`] :
//! - PFA : partie claire en texte, puis `eexec` et la partie chiffrée en
//!   binaire ou en hexadécimal (§7.2) ;
//! - PFB : segments `0x80 0x01` (texte) / `0x80 0x02` (binaire) /
//!   `0x80 0x03` (fin), concaténés avant analyse ;
//! - `FontFile` des PDF (ISO 32000-2 §9.9, tableau 124) : identique au
//!   PFA, `Length1`/`Length2` ne sont pas nécessaires (le mot `eexec` est
//!   recherché).
//!
//! Partie claire : `/FontName`, `/FontMatrix`, `/Encoding` (soit
//! `StandardEncoding`, soit `dup <code> /<nom> put`).
//! Partie privée (déchiffrée avec r = 55665, §7.2) : `/lenIV`, `/Subrs`,
//! `/CharStrings` ; chaque charstring est déchiffré avec r = 4330 (§7.3).
//!
//! Charstrings (chapitre 6) : tous les opérateurs, y compris `seac`,
//! `div`, `callothersubr`/`pop` (flex par OtherSubrs 0-2 remplacé par deux
//! courbes de Bézier, hint replacement par OtherSubrs 3 ignoré, contrôle de
//! compteurs 12-13 ignoré), `setcurrentpoint`. Les hints (`hstem`,
//! `vstem`, `hstem3`, `vstem3`, `dotsection`) sont ignorés.

use std::collections::HashMap;

use acrux_core::{Error, Matrix, Path, Point, Result};

use crate::encodings;
use crate::glyph::GlyphProvider;

/// Clé initiale du chiffrement eexec (§7.2).
const EEXEC_R: u16 = 55665;
/// Clé initiale du chiffrement des charstrings (§7.3).
const CHARSTRING_R: u16 = 4330;
const C1: u16 = 52845;
const C2: u16 = 22719;

/// Profondeur maximale d'appel de sous-routines (§6.4 : 10 niveaux, avec
/// une marge pour les polices tolérées).
const MAX_SUBR_DEPTH: u32 = 15;
/// Profondeur maximale de composition `seac`.
const MAX_SEAC_DEPTH: u32 = 4;
/// Nombre maximal d'opérateurs exécutés pour un glyphe.
const MAX_OPS_PER_GLYPH: u32 = 1_000_000;
/// Taille maximale de la pile d'opérandes (24 selon §6.2, tolérance).
const MAX_STACK: usize = 48;
/// Nombre maximal de sous-routines et de glyphes lus.
const MAX_ENTRIES: usize = 65_536;

/// Déchiffrement (§7.2) : `p = c ^ (r >> 8) ; r = (c + r) * c1 + c2`.
#[must_use]
pub fn decrypt(data: &[u8], key: u16, skip: usize) -> Vec<u8> {
    let mut r = key;
    let mut out = Vec::with_capacity(data.len().saturating_sub(skip));
    for (i, &c) in data.iter().enumerate() {
        let p = c ^ (r >> 8) as u8;
        r = (u16::from(c).wrapping_add(r))
            .wrapping_mul(C1)
            .wrapping_add(C2);
        if i >= skip {
            out.push(p);
        }
    }
    out
}

/// Chiffrement (inverse de [`decrypt`]) : `c = p ^ (r >> 8) ;
/// r = (c + r) * c1 + c2`. `lead` octets aléatoires (ici nuls) sont
/// ajoutés en tête (§7.2 : quatre pour eexec, `lenIV` pour les charstrings).
#[must_use]
pub fn encrypt(data: &[u8], key: u16, lead: usize) -> Vec<u8> {
    let mut r = key;
    let mut out = Vec::with_capacity(data.len() + lead);
    for &p in std::iter::repeat_n(&0u8, lead).chain(data.iter()) {
        let c = p ^ (r >> 8) as u8;
        r = (u16::from(c).wrapping_add(r))
            .wrapping_mul(C1)
            .wrapping_add(C2);
        out.push(c);
    }
    out
}

/// Environnement d'exécution d'un charstring Type 1 : sous-routines et
/// accès aux glyphes par code StandardEncoding (pour `seac`).
pub(crate) trait Type1Env {
    /// Sous-routine `index` (déjà déchiffrée).
    fn subr(&self, index: usize) -> Option<&[u8]>;
    /// Charstring du glyphe dont le nom est `StandardEncoding[code]`.
    fn charstring_by_standard_code(&self, code: u8) -> Option<&[u8]>;
}

/// Résultat de l'interprétation d'un charstring Type 1.
pub(crate) struct Type1Outline {
    /// Contour en unités de charstring.
    pub path: Path,
    /// Largeur (`hsbw` / `sbw`).
    pub width: f64,
}

/// État de l'interpréteur (chapitre 6).
struct Interpreter<'a> {
    env: &'a dyn Type1Env,
    path: Path,
    stack: Vec<f64>,
    /// Résultats laissés par `callothersubr` pour `pop` (sommet en fin).
    ps_stack: Vec<f64>,
    x: f64,
    y: f64,
    sbx: f64,
    width: f64,
    open: bool,
    in_flex: bool,
    flex_points: Vec<Point>,
    ops: u32,
    seac_depth: u32,
}

/// Interprète un charstring déchiffré.
pub(crate) fn interpret_type1(code: &[u8], env: &dyn Type1Env, seac_depth: u32) -> Type1Outline {
    let mut it = Interpreter {
        env,
        path: Path::new(),
        stack: Vec::with_capacity(24),
        ps_stack: Vec::new(),
        x: 0.0,
        y: 0.0,
        sbx: 0.0,
        width: 0.0,
        open: false,
        in_flex: false,
        flex_points: Vec::new(),
        ops: 0,
        seac_depth,
    };
    it.run(code, 0);
    if it.open {
        it.path.close();
    }
    Type1Outline {
        path: it.path,
        width: it.width,
    }
}

impl Interpreter<'_> {
    fn push(&mut self, v: f64) {
        if self.stack.len() < MAX_STACK {
            self.stack.push(v);
        }
    }

    fn arg(&self, i: usize) -> f64 {
        self.stack.get(i).copied().unwrap_or(0.0)
    }

    fn move_to(&mut self, x: f64, y: f64) {
        self.x = x;
        self.y = y;
        if self.in_flex {
            self.flex_points.push(Point::new(x, y));
            return;
        }
        if self.open {
            self.path.close();
        }
        self.path.move_to(Point::new(x, y));
        self.open = true;
    }

    fn ensure_open(&mut self) {
        if !self.open {
            self.path.move_to(Point::new(self.x, self.y));
            self.open = true;
        }
    }

    fn line_to(&mut self, x: f64, y: f64) {
        self.ensure_open();
        self.x = x;
        self.y = y;
        self.path.line_to(Point::new(x, y));
    }

    #[allow(clippy::too_many_arguments)] // six deltas d'une cubique
    fn rel_curve(&mut self, dx1: f64, dy1: f64, dx2: f64, dy2: f64, dx3: f64, dy3: f64) {
        self.ensure_open();
        let x1 = self.x + dx1;
        let y1 = self.y + dy1;
        let x2 = x1 + dx2;
        let y2 = y1 + dy2;
        self.x = x2 + dx3;
        self.y = y2 + dy3;
        self.path.curve_to(
            Point::new(x1, y1),
            Point::new(x2, y2),
            Point::new(self.x, self.y),
        );
    }

    /// Exécute `code` ; renvoie `false` quand le glyphe est terminé.
    fn run(&mut self, code: &[u8], depth: u32) -> bool {
        if depth > MAX_SUBR_DEPTH {
            return false;
        }
        let mut i = 0usize;
        while i < code.len() {
            self.ops += 1;
            if self.ops > MAX_OPS_PER_GLYPH {
                return false;
            }
            let v = code[i];
            i += 1;
            match v {
                32..=246 => self.push(f64::from(i32::from(v) - 139)),
                247..=250 => {
                    let w = code.get(i).copied().unwrap_or(0);
                    i += 1;
                    self.push(f64::from((i32::from(v) - 247) * 256 + i32::from(w) + 108));
                }
                251..=254 => {
                    let w = code.get(i).copied().unwrap_or(0);
                    i += 1;
                    self.push(f64::from(-(i32::from(v) - 251) * 256 - i32::from(w) - 108));
                }
                255 => {
                    let Some(b) = code.get(i..i + 4) else {
                        return false;
                    };
                    i += 4;
                    self.push(f64::from(i32::from_be_bytes([b[0], b[1], b[2], b[3]])));
                }
                12 => {
                    let op = code.get(i).copied().unwrap_or(0);
                    i += 1;
                    if !self.escaped_operator(op) {
                        return false;
                    }
                }
                _ => {
                    if !self.operator(v, depth) {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Opérateurs à un octet (§6.4).
    fn operator(&mut self, op: u8, depth: u32) -> bool {
        match op {
            13 => {
                // hsbw : sbx wx
                self.sbx = self.arg(0);
                self.width = self.arg(1);
                self.x = self.sbx;
                self.y = 0.0;
                self.stack.clear();
            }
            9 => {
                // closepath : ferme sans changer le point courant.
                if self.open {
                    self.path.close();
                    self.open = false;
                }
                self.stack.clear();
            }
            21 => {
                let (dx, dy) = (self.arg(0), self.arg(1));
                self.move_to(self.x + dx, self.y + dy);
                self.stack.clear();
            }
            22 => {
                let dx = self.arg(0);
                self.move_to(self.x + dx, self.y);
                self.stack.clear();
            }
            4 => {
                let dy = self.arg(0);
                self.move_to(self.x, self.y + dy);
                self.stack.clear();
            }
            5 => {
                let (dx, dy) = (self.arg(0), self.arg(1));
                self.line_to(self.x + dx, self.y + dy);
                self.stack.clear();
            }
            6 => {
                let dx = self.arg(0);
                self.line_to(self.x + dx, self.y);
                self.stack.clear();
            }
            7 => {
                let dy = self.arg(0);
                self.line_to(self.x, self.y + dy);
                self.stack.clear();
            }
            8 => {
                let a: Vec<f64> = (0..6).map(|i| self.arg(i)).collect();
                self.rel_curve(a[0], a[1], a[2], a[3], a[4], a[5]);
                self.stack.clear();
            }
            30 => {
                // vhcurveto : dy1 dx2 dy2 dx3
                let a: Vec<f64> = (0..4).map(|i| self.arg(i)).collect();
                self.rel_curve(0.0, a[0], a[1], a[2], a[3], 0.0);
                self.stack.clear();
            }
            31 => {
                // hvcurveto : dx1 dx2 dy2 dy3
                let a: Vec<f64> = (0..4).map(|i| self.arg(i)).collect();
                self.rel_curve(a[0], 0.0, a[1], a[2], 0.0, a[3]);
                self.stack.clear();
            }
            10 => {
                // callsubr : le numéro est au sommet, les autres arguments
                // restent disponibles pour la sous-routine.
                let Some(n) = self.stack.pop() else {
                    return true;
                };
                let Some(index) = usize::try_from(to_i64(n)).ok() else {
                    return true;
                };
                let Some(code) = self.env.subr(index) else {
                    return true;
                };
                return self.run(code, depth + 1);
            }
            11 => return true, // return
            14 => {
                if self.open {
                    self.path.close();
                    self.open = false;
                }
                return false;
            }
            _ => self.stack.clear(), // hstem, vstem et opérateurs réservés
        }
        true
    }

    /// Opérateurs échappés `12 x` (§6.4).
    fn escaped_operator(&mut self, op: u8) -> bool {
        match op {
            6 => {
                self.seac();
                return false;
            }
            7 => {
                // sbw : sbx sby wx wy
                self.sbx = self.arg(0);
                self.x = self.sbx;
                self.y = self.arg(1);
                self.width = self.arg(2);
                self.stack.clear();
            }
            12 => {
                // div
                let b = self.stack.pop().unwrap_or(1.0);
                let a = self.stack.pop().unwrap_or(0.0);
                self.push(if b == 0.0 { 0.0 } else { a / b });
            }
            16 => self.call_other_subr(),
            17 => {
                // pop : récupère un résultat d'OtherSubr.
                let v = self.ps_stack.pop().unwrap_or(0.0);
                self.push(v);
            }
            33 => {
                // setcurrentpoint : x y
                self.x = self.arg(0);
                self.y = self.arg(1);
                self.stack.clear();
            }
            _ => self.stack.clear(), // dotsection, vstem3, hstem3, réservés
        }
        true
    }

    /// callothersubr : `arg1 … argn n othersubr#` (§8.3, §8.4).
    fn call_other_subr(&mut self) {
        let index = self.stack.pop().map_or(-1, to_i64);
        let n = self
            .stack
            .pop()
            .map_or(0, |v| usize::try_from(to_i64(v).max(0)).unwrap_or(0))
            .min(self.stack.len());
        let args: Vec<f64> = self.stack.split_off(self.stack.len() - n);
        self.ps_stack.clear();
        match index {
            0 => {
                // Fin du flex : 7 points collectés (référence + 6 points
                // de contrôle) → deux courbes ; laisse (x, y) pour les
                // deux `pop` qui suivent.
                self.in_flex = false;
                let pts = std::mem::take(&mut self.flex_points);
                if pts.len() >= 7 {
                    self.ensure_open();
                    self.path.curve_to(pts[1], pts[2], pts[3]);
                    self.path.curve_to(pts[4], pts[5], pts[6]);
                    self.x = pts[6].x;
                    self.y = pts[6].y;
                } else if let Some(last) = pts.last() {
                    self.line_to(last.x, last.y);
                }
                self.ps_stack = vec![self.y, self.x];
            }
            1 => {
                self.in_flex = true;
                self.flex_points.clear();
            }
            3 => self.ps_stack = vec![3.0], // hint replacement : ignoré
            2 | 12 | 13 => {}               // suite du flex, contrôle de compteurs : ignorés
            _ => {
                // OtherSubr inconnu : les arguments sont rendus dans l'ordre
                // aux `pop` suivants.
                self.ps_stack = args.iter().rev().copied().collect();
            }
        }
    }

    /// seac : `asb adx ady bchar achar` (§6.4). L'accent est placé de
    /// sorte que son point de départ soit en `(sbx - asb + adx, ady)`.
    fn seac(&mut self) {
        if self.seac_depth >= MAX_SEAC_DEPTH {
            self.stack.clear();
            return;
        }
        let asb = self.arg(0);
        let adx = self.arg(1);
        let ady = self.arg(2);
        let bchar = u8::try_from(to_i64(self.arg(3))).ok();
        let achar = u8::try_from(to_i64(self.arg(4))).ok();
        self.stack.clear();
        if self.open {
            self.path.close();
            self.open = false;
        }
        let env = self.env;
        let depth = self.seac_depth + 1;
        let base = bchar
            .and_then(|c| env.charstring_by_standard_code(c))
            .map(|cs| interpret_type1(cs, env, depth));
        if let Some(b) = base {
            self.path.append(&b.path);
        }
        let accent = achar
            .and_then(|c| env.charstring_by_standard_code(c))
            .map(|cs| interpret_type1(cs, env, depth));
        if let Some(a) = accent {
            let m = Matrix::translate(self.sbx - asb + adx, ady);
            self.path.append(&a.path.transform(&m));
        }
    }
}

fn to_i64(v: f64) -> i64 {
    #[allow(clippy::cast_possible_truncation)] // borné par le clamp
    let r = v.clamp(-9.0e15, 9.0e15) as i64;
    r
}

/// Police Type 1 chargée en mémoire (charstrings déchiffrés).
#[derive(Debug, Clone)]
pub struct Type1Font {
    font_name: Option<String>,
    font_matrix: Matrix,
    encoding: Box<[Option<String>; 256]>,
    standard_encoding: bool,
    subrs: Vec<Vec<u8>>,
    /// Glyphes dans l'ordre du dictionnaire CharStrings (GID = position).
    glyphs: Vec<(String, Vec<u8>)>,
    name_to_gid: HashMap<String, usize>,
}

impl Type1Env for Type1Font {
    fn subr(&self, index: usize) -> Option<&[u8]> {
        self.subrs.get(index).map(Vec::as_slice)
    }

    fn charstring_by_standard_code(&self, code: u8) -> Option<&[u8]> {
        let name = encodings::standard(code)?;
        let gid = *self.name_to_gid.get(name)?;
        self.glyphs.get(gid).map(|(_, cs)| cs.as_slice())
    }
}

impl Type1Font {
    /// Analyse une police Type 1 (PFA, PFB ou FontFile PDF).
    ///
    /// # Errors
    /// `Error::Corrupt` si la section `eexec` est introuvable ou si aucun
    /// charstring n'est lisible.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let corrupt = |m: &str| Error::Corrupt(format!("police Type 1 : {m}"));
        let joined;
        let data = if data.first() == Some(&0x80) {
            joined = join_pfb_segments(data);
            joined.as_slice()
        } else {
            data
        };
        let eexec_pos = find(data, b"eexec").ok_or_else(|| corrupt("mot-clé eexec absent"))?;
        let clear = &data[..eexec_pos];
        let private = decrypt_eexec_section(&data[eexec_pos + 5..])
            .ok_or_else(|| corrupt("section eexec illisible"))?;

        let font_name = find(clear, b"/FontName")
            .and_then(|p| Scanner::new(clear, p + 9).next_token())
            .and_then(|t| t.strip_prefix('/').map(str::to_string));
        let font_matrix =
            parse_font_matrix(clear).unwrap_or(Matrix::new(0.001, 0.0, 0.0, 0.001, 0.0, 0.0));
        let (encoding, standard_encoding) = parse_encoding(clear);

        let len_iv = find(&private, b"/lenIV")
            .and_then(|p| Scanner::new(&private, p + 6).next_number())
            .map_or(4i64, to_i64);
        let subrs = parse_subrs(&private, len_iv);
        let glyphs = parse_charstrings(&private, len_iv);
        if glyphs.is_empty() {
            return Err(corrupt("aucun charstring"));
        }
        let mut name_to_gid = HashMap::with_capacity(glyphs.len());
        for (i, (name, _)) in glyphs.iter().enumerate() {
            name_to_gid.entry(name.clone()).or_insert(i);
        }
        Ok(Type1Font {
            font_name,
            font_matrix,
            encoding,
            standard_encoding,
            subrs,
            glyphs,
            name_to_gid,
        })
    }

    /// `/FontName`.
    #[must_use]
    pub fn font_name(&self) -> Option<&str> {
        self.font_name.as_deref()
    }

    /// `/FontMatrix`, `[0.001 0 0 0.001 0 0]` par défaut.
    #[must_use]
    pub fn font_matrix(&self) -> Matrix {
        self.font_matrix
    }

    /// Encodage intégré (`/Encoding`) : nom de glyphe par code.
    #[must_use]
    pub fn builtin_encoding(&self) -> &[Option<String>; 256] {
        &self.encoding
    }

    /// Vrai si la police déclare `/Encoding StandardEncoding`.
    #[must_use]
    pub fn uses_standard_encoding(&self) -> bool {
        self.standard_encoding
    }

    /// Noms des glyphes dans l'ordre des GID.
    #[must_use]
    pub fn glyph_names(&self) -> Vec<&str> {
        self.glyphs.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// Nom du glyphe `gid`.
    #[must_use]
    pub fn glyph_name(&self, gid: u32) -> Option<&str> {
        self.glyphs
            .get(usize::try_from(gid).ok()?)
            .map(|(n, _)| n.as_str())
    }

    /// GID (position dans CharStrings) d'un nom.
    #[must_use]
    pub fn gid_by_name(&self, name: &str) -> Option<u32> {
        self.name_to_gid
            .get(name)
            .and_then(|&i| u32::try_from(i).ok())
    }

    fn outline(&self, gid: usize) -> Option<Type1Outline> {
        let (_, cs) = self.glyphs.get(gid)?;
        Some(interpret_type1(cs, self, 0))
    }

    /// Contour d'un glyphe par nom, en unités de charstring (à transformer
    /// par [`Self::font_matrix`]).
    #[must_use]
    pub fn glyph_path_by_name(&self, name: &str) -> Option<Path> {
        self.outline(*self.name_to_gid.get(name)?).map(|o| o.path)
    }

    /// Largeur (`hsbw`/`sbw`) d'un glyphe par nom.
    #[must_use]
    pub fn advance_by_name(&self, name: &str) -> Option<f64> {
        self.outline(*self.name_to_gid.get(name)?).map(|o| o.width)
    }
}

impl GlyphProvider for Type1Font {
    fn glyph_path(&self, gid: u32) -> Option<Path> {
        self.outline(usize::try_from(gid).ok()?).map(|o| o.path)
    }

    fn advance(&self, gid: u32) -> Option<f64> {
        self.outline(usize::try_from(gid).ok()?).map(|o| o.width)
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
        u32::try_from(self.glyphs.len()).unwrap_or(u32::MAX)
    }
}

/// Position de `needle` dans `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Concatène les segments d'un fichier PFB (§7.1 de la note technique
/// « PFB », en-tête `0x80 type len32-LE`).
fn join_pfb_segments(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0usize;
    while i + 6 <= data.len() && data[i] == 0x80 {
        let kind = data[i + 1];
        if kind == 3 {
            break;
        }
        let len = u32::from_le_bytes([data[i + 2], data[i + 3], data[i + 4], data[i + 5]]);
        let len = usize::try_from(len).unwrap_or(usize::MAX);
        let start = i + 6;
        let end = start.saturating_add(len).min(data.len());
        out.extend_from_slice(&data[start..end]);
        i = end;
    }
    if out.is_empty() {
        data.to_vec()
    } else {
        out
    }
}

fn is_ps_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0C' | b'\0')
}

fn is_hex_digit(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

/// Déchiffre la section qui suit le mot `eexec` : détection binaire /
/// hexadécimal, puis essai de plusieurs débuts possibles (le nombre
/// d'espaces après `eexec` varie selon les générateurs) jusqu'à obtenir
/// un texte contenant `/CharStrings` ou `/Subrs`.
fn decrypt_eexec_section(after: &[u8]) -> Option<Vec<u8>> {
    let first_non_space = after
        .iter()
        .position(|&b| !is_ps_space(b))
        .unwrap_or(after.len());
    let probe = after.get(first_non_space..first_non_space + 4)?;
    if probe.iter().all(|&b| is_hex_digit(b)) {
        let mut bytes = Vec::with_capacity(after.len() / 2);
        let mut nibble: Option<u8> = None;
        for &b in &after[first_non_space..] {
            let Some(v) = hex_value(b) else {
                if is_ps_space(b) {
                    continue;
                }
                break;
            };
            match nibble.take() {
                Some(hi) => bytes.push((hi << 4) | v),
                None => nibble = Some(v),
            }
        }
        return Some(decrypt(&bytes, EEXEC_R, 4));
    }
    let mut best: Option<Vec<u8>> = None;
    for skip in 0..=first_non_space.min(4) {
        let plain = decrypt(&after[skip..], EEXEC_R, 4);
        if find(&plain, b"/CharStrings").is_some() || find(&plain, b"/Subrs").is_some() {
            return Some(plain);
        }
        if best.is_none() {
            best = Some(plain);
        }
    }
    best
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Analyseur lexical minimal PostScript (espaces, commentaires, mots).
struct Scanner<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Scanner<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Self {
            data,
            pos: pos.min(data.len()),
        }
    }

    fn skip_space(&mut self) {
        while self.pos < self.data.len() {
            let b = self.data[self.pos];
            if is_ps_space(b) {
                self.pos += 1;
            } else if b == b'%' {
                while self.pos < self.data.len()
                    && self.data[self.pos] != b'\n'
                    && self.data[self.pos] != b'\r'
                {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// Jeton suivant : mot, nom (`/x`), ou délimiteur isolé.
    fn next_token(&mut self) -> Option<&'a str> {
        self.skip_space();
        let start = self.pos;
        if start >= self.data.len() {
            return None;
        }
        let b = self.data[start];
        let is_delim = |c: u8| matches!(c, b'[' | b']' | b'{' | b'}' | b'(' | b')' | b'<' | b'>');
        if is_delim(b) {
            self.pos += 1;
        } else {
            self.pos += 1;
            while self.pos < self.data.len() {
                let c = self.data[self.pos];
                if is_ps_space(c) || is_delim(c) || c == b'/' || c == b'%' {
                    break;
                }
                self.pos += 1;
            }
        }
        std::str::from_utf8(&self.data[start..self.pos]).ok()
    }

    fn next_number(&mut self) -> Option<f64> {
        self.next_token()?
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
    }

    /// Lit `len` octets binaires après le jeton `RD` et l'espace unique
    /// qui le suit (§7.3 : « RD » puis un espace puis les données).
    fn binary(&mut self, len: usize) -> Option<&'a [u8]> {
        // Jeton RD / -| (quel que soit son nom).
        self.next_token()?;
        self.pos += 1; // l'espace de séparation
        let end = self.pos.checked_add(len)?;
        let bytes = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(bytes)
    }
}

/// `/FontMatrix [a b c d e f]`.
fn parse_font_matrix(clear: &[u8]) -> Option<Matrix> {
    let p = find(clear, b"/FontMatrix")?;
    let mut s = Scanner::new(clear, p + 11);
    if s.next_token()? != "[" {
        return None;
    }
    let mut v = [0.0; 6];
    for slot in &mut v {
        *slot = s.next_number()?;
    }
    Some(Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5]))
}

/// `/Encoding StandardEncoding def` ou `dup <code> /<nom> put …`.
fn parse_encoding(clear: &[u8]) -> (Box<[Option<String>; 256]>, bool) {
    let mut table: Box<[Option<String>; 256]> = Box::new(std::array::from_fn(|_| None));
    let Some(p) = find(clear, b"/Encoding") else {
        return (table, false);
    };
    let mut s = Scanner::new(clear, p + 9);
    if s.next_token() == Some("StandardEncoding") {
        for (code, slot) in table.iter_mut().enumerate() {
            *slot = encodings::standard(u8::try_from(code).unwrap_or(0)).map(str::to_string);
        }
        return (table, true);
    }
    // `dup <code> /<nom> put` jusqu'à `readonly def` ou `def`.
    let mut s = Scanner::new(clear, p + 9);
    let mut guard = 0usize;
    while let Some(tok) = s.next_token() {
        guard += 1;
        if guard > MAX_ENTRIES * 4 {
            break;
        }
        match tok {
            "dup" => {
                let Some(code) = s.next_number() else {
                    continue;
                };
                let Some(name) = s.next_token() else {
                    break;
                };
                if let (Ok(code), Some(name)) = (u8::try_from(to_i64(code)), name.strip_prefix('/'))
                {
                    if s.next_token() == Some("put") {
                        table[usize::from(code)] = Some(name.to_string());
                    }
                }
            }
            "readonly" | "def" => break,
            _ => {}
        }
    }
    (table, false)
}

/// Déchiffre un charstring selon `lenIV` (`-1` : non chiffré).
fn decrypt_charstring(bytes: &[u8], len_iv: i64) -> Vec<u8> {
    match usize::try_from(len_iv) {
        Ok(skip) => decrypt(bytes, CHARSTRING_R, skip),
        Err(_) => bytes.to_vec(),
    }
}

/// `/Subrs n array` puis `dup <i> <len> RD <bin> NP`.
fn parse_subrs(private: &[u8], len_iv: i64) -> Vec<Vec<u8>> {
    let Some(p) = find(private, b"/Subrs") else {
        return Vec::new();
    };
    let mut s = Scanner::new(private, p + 6);
    let count = s
        .next_number()
        .map_or(0, |v| usize::try_from(to_i64(v).max(0)).unwrap_or(0))
        .min(MAX_ENTRIES);
    let mut subrs: Vec<Vec<u8>> = Vec::new();
    let mut read = 0usize;
    while read < count {
        let Some(tok) = s.next_token() else {
            break;
        };
        match tok {
            "dup" => {
                let (Some(index), Some(len)) = (s.next_number(), s.next_number()) else {
                    break;
                };
                let index = usize::try_from(to_i64(index).max(0)).unwrap_or(0);
                let len = usize::try_from(to_i64(len).max(0)).unwrap_or(0);
                let Some(bytes) = s.binary(len) else {
                    break;
                };
                if index < MAX_ENTRIES {
                    if subrs.len() <= index {
                        subrs.resize(index + 1, Vec::new());
                    }
                    subrs[index] = decrypt_charstring(bytes, len_iv);
                }
                read += 1;
            }
            "/CharStrings" | "ND" | "|-" | "noaccess" | "end" => break,
            _ => {}
        }
    }
    subrs
}

/// `/CharStrings n dict dup begin` puis `/<nom> <len> RD <bin> ND`.
fn parse_charstrings(private: &[u8], len_iv: i64) -> Vec<(String, Vec<u8>)> {
    let Some(p) = find(private, b"/CharStrings") else {
        return Vec::new();
    };
    let mut s = Scanner::new(private, p + 12);
    // Jusqu'à `begin`.
    let mut guard = 0;
    while let Some(tok) = s.next_token() {
        guard += 1;
        if tok == "begin" || guard > 16 {
            break;
        }
    }
    let mut glyphs = Vec::new();
    while glyphs.len() < MAX_ENTRIES {
        let Some(tok) = s.next_token() else {
            break;
        };
        if tok == "end" {
            break;
        }
        let Some(name) = tok.strip_prefix('/') else {
            continue;
        };
        let Some(len) = s.next_number() else {
            break;
        };
        let len = usize::try_from(to_i64(len).max(0)).unwrap_or(0);
        let Some(bytes) = s.binary(len) else {
            break;
        };
        glyphs.push((name.to_string(), decrypt_charstring(bytes, len_iv)));
    }
    glyphs
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp, clippy::too_many_lines)]
pub(crate) mod tests {
    use super::*;
    use acrux_core::PathCommand;

    /// Nombre en opérande de charstring Type 1.
    fn num(v: i32) -> Vec<u8> {
        if (-107..=107).contains(&v) {
            vec![u8::try_from(v + 139).unwrap()]
        } else if (108..=1131).contains(&v) {
            let w = v - 108;
            vec![
                u8::try_from(w / 256 + 247).unwrap(),
                u8::try_from(w % 256).unwrap(),
            ]
        } else if (-1131..=-108).contains(&v) {
            let w = -v - 108;
            vec![
                u8::try_from(w / 256 + 251).unwrap(),
                u8::try_from(w % 256).unwrap(),
            ]
        } else {
            let mut out = vec![255];
            out.extend_from_slice(&v.to_be_bytes());
            out
        }
    }

    enum T {
        N(i32),
        Op(u8),
        Esc(u8),
    }
    use T::{Esc, Op, N};

    fn cs(tokens: &[T]) -> Vec<u8> {
        let mut out = Vec::new();
        for t in tokens {
            match t {
                T::N(v) => out.extend(num(*v)),
                T::Op(o) => out.push(*o),
                T::Esc(o) => out.extend_from_slice(&[12, *o]),
            }
        }
        out
    }

    /// Section privée en clair (avant chiffrement eexec).
    fn private_section(len_iv: usize) -> Vec<u8> {
        let subrs: Vec<Vec<u8>> = vec![
            // 0 : 3 0 callothersubr pop pop setcurrentpoint return
            cs(&[N(3), N(0), Esc(16), Esc(17), Esc(17), Esc(33), Op(11)]),
            cs(&[N(0), N(1), Esc(16), Op(11)]),
            cs(&[N(0), N(2), Esc(16), Op(11)]),
            cs(&[Op(11)]),
            // 4 : hint subr (vstem) — jamais exécuté.
            cs(&[N(30), N(40), Op(3), Op(11)]),
            // 5 : trace une ligne de 25.
            cs(&[N(25), Op(6), Op(11)]),
        ];
        let glyphs: Vec<(&str, Vec<u8>)> = vec![
            (".notdef", cs(&[N(0), N(250), Op(13), Op(14)])),
            (
                "A",
                cs(&[
                    N(50),
                    N(600),
                    Op(13),
                    N(0),
                    N(0),
                    Op(21),
                    N(100),
                    Op(6),
                    N(100),
                    Op(7),
                    N(-100),
                    Op(6),
                    Op(9),
                    Op(14),
                ]),
            ),
            (
                "quoteright",
                cs(&[
                    N(20),
                    N(300),
                    Op(13),
                    N(0),
                    N(0),
                    Op(21),
                    N(10),
                    Op(7),
                    Op(9),
                    Op(14),
                ]),
            ),
            (
                "flexy",
                cs(&[
                    N(0),
                    N(500),
                    Op(13),
                    N(0),
                    N(0),
                    Op(21),
                    N(1),
                    Op(10),
                    N(50),
                    N(0),
                    Op(21),
                    N(2),
                    Op(10),
                    N(-40),
                    N(10),
                    Op(21),
                    N(2),
                    Op(10),
                    N(20),
                    N(10),
                    Op(21),
                    N(2),
                    Op(10),
                    N(20),
                    N(0),
                    Op(21),
                    N(2),
                    Op(10),
                    N(20),
                    N(0),
                    Op(21),
                    N(2),
                    Op(10),
                    N(20),
                    N(-10),
                    Op(21),
                    N(2),
                    Op(10),
                    N(10),
                    N(-10),
                    Op(21),
                    N(2),
                    Op(10),
                    N(50),
                    N(100),
                    N(0),
                    N(0),
                    Op(10),
                    N(200),
                    N(2),
                    Esc(12),
                    Op(6),
                    Op(14),
                ]),
            ),
            (
                "Aacute",
                cs(&[
                    N(50),
                    N(600),
                    Op(13),
                    N(20),
                    N(100),
                    N(200),
                    N(65),
                    N(39),
                    Esc(6),
                ]),
            ),
            (
                "hinted",
                cs(&[
                    N(0),
                    N(500),
                    Op(13),
                    N(10),
                    N(20),
                    Op(1),
                    N(4),
                    N(1),
                    N(3),
                    Esc(16),
                    Esc(17),
                    Op(10),
                    N(0),
                    N(0),
                    Op(21),
                    N(5),
                    Op(10),
                    N(2000),
                    Op(7),
                    N(1),
                    N(2),
                    N(3),
                    N(3),
                    N(99),
                    Esc(16),
                    Esc(17),
                    Esc(17),
                    Op(5),
                    Op(14),
                ]),
            ),
            (
                "curvy",
                cs(&[
                    N(0),
                    N(500),
                    Op(13),
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
                    Op(30),
                    N(10),
                    N(20),
                    N(30),
                    N(40),
                    Op(31),
                    Op(14),
                ]),
            ),
            (
                "sbw",
                cs(&[
                    N(10),
                    N(20),
                    N(700),
                    N(0),
                    Esc(7),
                    N(5),
                    Op(22),
                    N(5),
                    Op(4),
                    Op(14),
                ]),
            ),
        ];
        let mut out = Vec::new();
        out.extend_from_slice(b"dup /Private 8 dict dup begin\n/RD {string currentfile exch readstring pop} executeonly def\n/ND {noaccess def} executeonly def\n/NP {noaccess put} executeonly def\n");
        out.extend_from_slice(format!("/lenIV {len_iv} def\n").as_bytes());
        out.extend_from_slice(format!("/Subrs {} array\n", subrs.len()).as_bytes());
        for (i, s) in subrs.iter().enumerate() {
            let enc = encrypt(s, CHARSTRING_R, len_iv);
            out.extend_from_slice(format!("dup {i} {} RD ", enc.len()).as_bytes());
            out.extend_from_slice(&enc);
            out.extend_from_slice(b" NP\n");
        }
        out.extend_from_slice(b"ND\nend\n");
        out.extend_from_slice(format!("/CharStrings {} dict dup begin\n", glyphs.len()).as_bytes());
        for (name, g) in &glyphs {
            let enc = encrypt(g, CHARSTRING_R, len_iv);
            out.extend_from_slice(format!("/{name} {} RD ", enc.len()).as_bytes());
            out.extend_from_slice(&enc);
            out.extend_from_slice(b" ND\n");
        }
        out.extend_from_slice(b"end\nend\nmark currentfile closefile\n");
        out
    }

    fn clear_section(standard: bool) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"%!PS-AdobeFont-1.0: TestFont 001.000\n/FontName /TestFont def\n/PaintType 0 def\n/FontMatrix [0.001 0 0 0.001 0 0] readonly def\n");
        if standard {
            out.extend_from_slice(b"/Encoding StandardEncoding def\n");
        } else {
            out.extend_from_slice(b"/Encoding 256 array\n0 1 255 {1 index exch /.notdef put} for\ndup 65 /A put\ndup 66 /flexy put\ndup 200 /Aacute put\nreadonly def\n");
        }
        out.extend_from_slice(b"currentdict end\ncurrentfile eexec\n");
        out
    }

    /// PFA binaire.
    pub(crate) fn sample_pfa() -> Vec<u8> {
        let mut out = clear_section(false);
        out.extend(encrypt(&private_section(4), EEXEC_R, 4));
        out.extend_from_slice(b"\n");
        out.extend_from_slice(&[b'0'; 64]);
        out.extend_from_slice(b"\ncleartomark\n");
        out
    }

    fn sample_pfa_hex() -> Vec<u8> {
        let mut out = clear_section(true);
        let enc = encrypt(&private_section(1), EEXEC_R, 4);
        for (i, b) in enc.iter().enumerate() {
            out.extend_from_slice(format!("{b:02X}").as_bytes());
            if i % 32 == 31 {
                out.push(b'\n');
            }
        }
        out.extend_from_slice(b"\n0000000000000000\ncleartomark\n");
        out
    }

    fn sample_pfb() -> Vec<u8> {
        let clear = clear_section(false);
        let bin = encrypt(&private_section(4), EEXEC_R, 4);
        let trailer = b"0000000000000000\ncleartomark\n".to_vec();
        let mut out = Vec::new();
        for (kind, seg) in [(1u8, clear), (2, bin), (1, trailer)] {
            out.push(0x80);
            out.push(kind);
            out.extend_from_slice(&u32::try_from(seg.len()).unwrap().to_le_bytes());
            out.extend_from_slice(&seg);
        }
        out.extend_from_slice(&[0x80, 0x03]);
        out
    }

    #[test]
    fn encryption_roundtrip() {
        let plain = b"hello charstring";
        let enc = encrypt(plain, CHARSTRING_R, 4);
        assert_eq!(decrypt(&enc, CHARSTRING_R, 4), plain);
        let enc = encrypt(plain, EEXEC_R, 4);
        assert_eq!(decrypt(&enc, EEXEC_R, 4), plain);
    }

    #[test]
    fn parses_pfa_metadata_and_encoding() {
        let font = Type1Font::parse(&sample_pfa()).unwrap();
        assert_eq!(font.font_name(), Some("TestFont"));
        assert_eq!(
            font.font_matrix(),
            Matrix::new(0.001, 0.0, 0.0, 0.001, 0.0, 0.0)
        );
        assert!(!font.uses_standard_encoding());
        let enc = font.builtin_encoding();
        assert_eq!(enc[65].as_deref(), Some("A"));
        assert_eq!(enc[66].as_deref(), Some("flexy"));
        assert_eq!(enc[200].as_deref(), Some("Aacute"));
        assert_eq!(enc[67], None);
        assert_eq!(
            font.glyph_names(),
            vec![
                ".notdef",
                "A",
                "quoteright",
                "flexy",
                "Aacute",
                "hinted",
                "curvy",
                "sbw"
            ]
        );
        assert_eq!(font.gid_by_name("flexy"), Some(3));
        assert_eq!(font.glyph_name(3), Some("flexy"));
        assert_eq!(font.glyph_name(99), None);
    }

    #[test]
    fn square_glyph_and_width() {
        let font = Type1Font::parse(&sample_pfa()).unwrap();
        let path = font.glyph_path_by_name("A").unwrap();
        assert_eq!(
            path.commands(),
            &[
                PathCommand::MoveTo(Point::new(50.0, 0.0)),
                PathCommand::LineTo(Point::new(150.0, 0.0)),
                PathCommand::LineTo(Point::new(150.0, 100.0)),
                PathCommand::LineTo(Point::new(50.0, 100.0)),
                PathCommand::Close,
            ]
        );
        assert_eq!(font.advance_by_name("A"), Some(600.0));
        assert_eq!(font.advance_by_name(".notdef"), Some(250.0));
        assert!(font.glyph_path_by_name(".notdef").unwrap().is_empty());
        assert!(font.glyph_path_by_name("zzz").is_none());
        // sbw : point de départ (10, 20), largeur 700.
        let sbw = font.glyph_path_by_name("sbw").unwrap();
        assert_eq!(
            sbw.commands()[0],
            PathCommand::MoveTo(Point::new(15.0, 20.0))
        );
        assert_eq!(font.advance_by_name("sbw"), Some(700.0));
    }

    #[test]
    fn flex_becomes_two_curves() {
        let font = Type1Font::parse(&sample_pfa()).unwrap();
        let path = font.glyph_path_by_name("flexy").unwrap();
        assert_eq!(
            path.commands(),
            &[
                PathCommand::MoveTo(Point::new(0.0, 0.0)),
                PathCommand::CurveTo(
                    Point::new(10.0, 10.0),
                    Point::new(30.0, 20.0),
                    Point::new(50.0, 20.0)
                ),
                PathCommand::CurveTo(
                    Point::new(70.0, 20.0),
                    Point::new(90.0, 10.0),
                    Point::new(100.0, 0.0)
                ),
                // 200 2 div hlineto après setcurrentpoint (100, 0).
                PathCommand::LineTo(Point::new(200.0, 0.0)),
                PathCommand::Close,
            ]
        );
    }

    #[test]
    fn seac_composes_base_and_accent() {
        let font = Type1Font::parse(&sample_pfa()).unwrap();
        let path = font.glyph_path_by_name("Aacute").unwrap();
        let cmds = path.commands();
        assert_eq!(cmds.len(), 8);
        assert_eq!(cmds[0], PathCommand::MoveTo(Point::new(50.0, 0.0)));
        // Accent : sbx 50 - asb 20 + adx 100 = 130 ; son propre sb 20 → 150.
        assert_eq!(cmds[5], PathCommand::MoveTo(Point::new(150.0, 200.0)));
        assert_eq!(cmds[6], PathCommand::LineTo(Point::new(150.0, 210.0)));
        assert_eq!(font.advance_by_name("Aacute"), Some(600.0));
    }

    #[test]
    fn hint_replacement_and_unknown_othersubr() {
        let font = Type1Font::parse(&sample_pfa()).unwrap();
        let path = font.glyph_path_by_name("hinted").unwrap();
        assert_eq!(
            path.commands(),
            &[
                PathCommand::MoveTo(Point::new(0.0, 0.0)),
                PathCommand::LineTo(Point::new(25.0, 0.0)),
                PathCommand::LineTo(Point::new(25.0, 2000.0)),
                // OtherSubr 99 inconnu : `1 2 3 3 99 callothersubr pop pop`
                // rend 1 puis 2 → rlineto 1 2.
                PathCommand::LineTo(Point::new(26.0, 2002.0)),
                PathCommand::Close,
            ]
        );
    }

    #[test]
    fn curve_operators() {
        let font = Type1Font::parse(&sample_pfa()).unwrap();
        let path = font.glyph_path_by_name("curvy").unwrap();
        let cmds = path.commands();
        assert_eq!(
            cmds[1],
            PathCommand::CurveTo(
                Point::new(10.0, 20.0),
                Point::new(40.0, 60.0),
                Point::new(90.0, 120.0)
            )
        );
        // vhcurveto 10 20 30 40 : (90,130) (110,160) (150,160)
        assert_eq!(
            cmds[2],
            PathCommand::CurveTo(
                Point::new(90.0, 130.0),
                Point::new(110.0, 160.0),
                Point::new(150.0, 160.0)
            )
        );
        // hvcurveto 10 20 30 40 : (160,160) (180,190) (180,230)
        assert_eq!(
            cmds[3],
            PathCommand::CurveTo(
                Point::new(160.0, 160.0),
                Point::new(180.0, 190.0),
                Point::new(180.0, 230.0)
            )
        );
    }

    #[test]
    fn hex_pfa_with_standard_encoding_and_leniv_1() {
        let font = Type1Font::parse(&sample_pfa_hex()).unwrap();
        assert!(font.uses_standard_encoding());
        assert_eq!(font.builtin_encoding()[65].as_deref(), Some("A"));
        assert_eq!(font.builtin_encoding()[39].as_deref(), Some("quoteright"));
        assert_eq!(font.advance_by_name("A"), Some(600.0));
        assert_eq!(font.glyph_path_by_name("A").unwrap().commands().len(), 5);
    }

    #[test]
    fn pfb_segments() {
        let font = Type1Font::parse(&sample_pfb()).unwrap();
        assert_eq!(font.glyph_names().len(), 8);
        assert_eq!(font.advance_by_name("flexy"), Some(500.0));
    }

    #[test]
    fn glyph_provider_trait() {
        let font = Type1Font::parse(&sample_pfa()).unwrap();
        let p: &dyn GlyphProvider = &font;
        assert_eq!(p.glyph_count(), 8);
        assert_eq!(p.units_per_em(), 1000.0);
        assert_eq!(p.advance(1), Some(600.0));
        assert_eq!(p.glyph_path(1).unwrap().commands().len(), 5);
        assert!(p.glyph_path(8).is_none());
    }

    #[test]
    fn eexec_with_crlf_and_extra_spaces() {
        let mut out = clear_section(false);
        out.pop(); // retire le '\n' après eexec
        out.extend_from_slice(b"\r\n  ");
        out.extend(encrypt(&private_section(4), EEXEC_R, 4));
        let font = Type1Font::parse(&out).unwrap();
        assert_eq!(font.glyph_names().len(), 8);
    }

    #[test]
    fn errors_and_truncation() {
        assert!(Type1Font::parse(b"").is_err());
        assert!(Type1Font::parse(b"%!PS no eexec here").is_err());
        assert!(Type1Font::parse(b"%!PS eexec\n").is_err());
        for data in [sample_pfa(), sample_pfb(), sample_pfa_hex()] {
            for n in (0..data.len()).step_by(7) {
                if let Ok(font) = Type1Font::parse(&data[..n]) {
                    for gid in 0..=font.glyph_count() {
                        let _ = font.glyph_path(gid);
                        let _ = font.advance(gid);
                    }
                    let _ = font.glyph_path_by_name("Aacute");
                }
            }
        }
    }

    #[test]
    fn random_bytes_never_panic() {
        let mut seed: u32 = 0x0BAD_F00D;
        let base = sample_pfa();
        for round in 0..100 {
            let mut data = base.clone();
            // Corrompt 20 octets au hasard.
            for _ in 0..20 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let i = usize::try_from(seed).unwrap_or(0) % data.len();
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                data[i] = u8::try_from(seed >> 24).unwrap_or(0);
            }
            if round % 2 == 0 {
                data.truncate(data.len() / 2 + round);
            }
            if let Ok(font) = Type1Font::parse(&data) {
                for gid in 0..font.glyph_count() {
                    let _ = font.glyph_path(gid);
                }
            }
        }
    }
}
