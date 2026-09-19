//! Interprétation du flux de contenu pour l'extraction : opérateurs texte
//! (ISO 32000-2 §9.4), couleurs de remplissage (§8.6.8) pour le style des
//! glyphes, et tracés fins (§8.5) pour repérer les filets de tableaux et les
//! puces dessinées. Les XObjects de formulaire sont parcourus récursivement.

use std::collections::HashMap;
use std::rc::Rc;

use acrux_core::{Matrix, Point, Rect, Result};
use acrux_document::{Dict, Document, Name, Object, Page};
use acrux_render::content::ContentLexer;
use acrux_render::font::LoadedFont;
use acrux_render::page::page_content;

use super::{projections, Glyph};

/// Résultat brut de l'interprétation d'une page.
pub struct Extracted {
    /// Glyphes dans l'ordre du flux.
    pub glyphs: Vec<Glyph>,
    /// Filets fins (voir `PageText::rules`).
    pub rules: Vec<Rect>,
    /// Petites formes pleines (voir `PageText::marks`).
    pub marks: Vec<Rect>,
}

/// Budget d'opérateurs (fichiers hostiles).
const MAX_OPS: usize = 2_000_000;
/// Nombre maximal de filets et de marques conservés.
const MAX_SHAPES: usize = 20_000;
/// Épaisseur maximale d'un filet (points).
const RULE_MAX_THICKNESS: f64 = 2.5;
/// Longueur minimale d'un filet (points).
const RULE_MIN_LENGTH: f64 = 4.0;
/// Taille maximale d'une puce dessinée (points).
const MARK_MAX_SIZE: f64 = 12.0;
/// Taille minimale d'une puce dessinée (points).
const MARK_MIN_SIZE: f64 = 1.0;

/// Police chargée, avec les attributs de style déduits une fois pour toutes.
struct FontInfo {
    font: Rc<LoadedFont>,
    name: String,
    bold: bool,
    italic: bool,
}

/// État graphique et texte pertinent pour l'extraction.
struct State {
    ctm: Matrix,
    tm: Matrix,
    tlm: Matrix,
    font: Option<Rc<FontInfo>>,
    size: f64,
    char_spacing: f64,
    word_spacing: f64,
    hscale: f64,
    leading: f64,
    rise: f64,
    render_mode: i64,
    fill: [f32; 3],
    stroke: [f32; 3],
    /// Nombre de composantes attendu par `sc`/`scn` (espace de remplissage courant).
    fill_components: usize,
    stroke_components: usize,
    line_width: f64,
}

impl State {
    fn new(ctm: Matrix) -> Self {
        Self {
            ctm,
            tm: Matrix::IDENTITY,
            tlm: Matrix::IDENTITY,
            font: None,
            size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            hscale: 1.0,
            leading: 0.0,
            rise: 0.0,
            render_mode: 0,
            fill: [0.0; 3],
            stroke: [0.0; 3],
            fill_components: 1,
            stroke_components: 1,
            line_width: 1.0,
        }
    }

    /// Partie de l'état sauvegardée par `q`.
    fn saved(&self) -> Saved {
        Saved {
            ctm: self.ctm,
            font: self.font.clone(),
            size: self.size,
            fill: self.fill,
            stroke: self.stroke,
            fill_components: self.fill_components,
            stroke_components: self.stroke_components,
            line_width: self.line_width,
        }
    }

    fn restore(&mut self, s: Saved) {
        self.ctm = s.ctm;
        self.font = s.font;
        self.size = s.size;
        self.fill = s.fill;
        self.stroke = s.stroke;
        self.fill_components = s.fill_components;
        self.stroke_components = s.stroke_components;
        self.line_width = s.line_width;
    }

    /// Facteur d'échelle de la CTM (pour les épaisseurs de trait).
    fn scale(&self) -> f64 {
        f64::midpoint(self.ctm.a.hypot(self.ctm.b), self.ctm.c.hypot(self.ctm.d))
    }
}

struct Saved {
    ctm: Matrix,
    font: Option<Rc<FontInfo>>,
    size: f64,
    fill: [f32; 3],
    stroke: [f32; 3],
    fill_components: usize,
    stroke_components: usize,
    line_width: f64,
}

/// Chemin en cours de construction, déjà transformé en espace utilisateur.
#[derive(Default)]
struct PathBuild {
    /// Rectangles `re` (boîtes transformées).
    rects: Vec<Rect>,
    /// Segments droits `m`/`l`/`h`.
    segments: Vec<(Point, Point)>,
    /// Tous les points (boîte englobante des formes quelconques).
    bbox: Option<Rect>,
    current: Option<Point>,
    start: Option<Point>,
    has_curve: bool,
}

impl PathBuild {
    fn clear(&mut self) {
        self.rects.clear();
        self.segments.clear();
        self.bbox = None;
        self.current = None;
        self.start = None;
        self.has_curve = false;
    }

    fn add_point(&mut self, p: Point) {
        let r = Rect::new(p.x, p.y, p.x, p.y);
        self.bbox = Some(self.bbox.map_or(r, |b| b.union(&r)));
    }

    fn move_to(&mut self, p: Point) {
        self.add_point(p);
        self.current = Some(p);
        self.start = Some(p);
    }

    fn line_to(&mut self, p: Point) {
        self.add_point(p);
        if let Some(c) = self.current {
            self.segments.push((c, p));
        }
        self.current = Some(p);
    }

    fn close(&mut self) {
        if let (Some(c), Some(s)) = (self.current, self.start) {
            if (c.x - s.x).abs() > 1e-9 || (c.y - s.y).abs() > 1e-9 {
                self.segments.push((c, s));
            }
            self.current = Some(s);
        }
    }
}

struct Extractor<'a> {
    doc: &'a Document,
    out: Extracted,
    fonts: HashMap<String, Rc<FontInfo>>,
    depth: usize,
    ops: usize,
    path: PathBuild,
    /// Pile des contenus balisés ouverts : `true` pour `/Lbl` (étiquette de liste).
    marked: Vec<bool>,
}

/// Convertit une couleur donnée par ses composantes (gris, RVB ou CMJN) en RVB.
fn to_rgb(components: &[f64]) -> Option<[f32; 3]> {
    #[allow(clippy::cast_possible_truncation)]
    let unit = |v: f64| v.clamp(0.0, 1.0) as f32;
    match components {
        [gray] => Some([unit(*gray); 3]),
        [red, green, blue] => Some([unit(*red), unit(*green), unit(*blue)]),
        [cyan, magenta, yellow, black] => Some([
            unit((1.0 - cyan) * (1.0 - black)),
            unit((1.0 - magenta) * (1.0 - black)),
            unit((1.0 - yellow) * (1.0 - black)),
        ]),
        _ => None,
    }
}

/// Nombre de composantes d'un espace colorimétrique nommé (§8.6), via les
/// ressources pour les noms définis par le document.
fn colorspace_components(doc: &Document, resources: &Dict, name: &Name) -> usize {
    match name.0.as_slice() {
        b"DeviceGray" | b"CalGray" | b"G" | b"Indexed" | b"I" | b"Separation" => 1,
        b"DeviceRGB" | b"CalRGB" | b"RGB" | b"Lab" => 3,
        b"DeviceCMYK" | b"CMYK" => 4,
        b"Pattern" => 0,
        _ => {
            let cs = doc
                .dict_get(resources, "ColorSpace")
                .ok()
                .flatten()
                .and_then(|d| d.as_dict().and_then(|d| d.get(name).cloned()));
            let Some(cs) = cs.and_then(|o| doc.resolve(&o).ok().map(|r| (*r).clone())) else {
                return 1;
            };
            match &cs {
                Object::Name(n) => colorspace_components(doc, &Dict::default(), n),
                Object::Array(items) => match items.first().and_then(Object::as_name) {
                    Some(n) if n.0 == b"ICCBased" => items
                        .get(1)
                        .and_then(|s| doc.resolve(s).ok())
                        .and_then(|s| match &*s {
                            Object::Stream { dict, .. } => {
                                dict.get(&Name::new("N")).and_then(Object::as_i64)
                            }
                            _ => None,
                        })
                        .and_then(|n| usize::try_from(n).ok())
                        .unwrap_or(3),
                    Some(n) if n.0 == b"DeviceN" => items
                        .get(1)
                        .and_then(|a| a.as_array())
                        .map_or(1, <[Object]>::len),
                    Some(n) => colorspace_components(doc, &Dict::default(), n),
                    None => 1,
                },
                _ => 1,
            }
        }
    }
}

/// Gras et italique déduits du nom de police et du descripteur (§9.8.2).
fn font_style(doc: &Document, dict: &Dict, base_font: &str) -> (bool, bool) {
    let lower = base_font.to_ascii_lowercase();
    let mut bold = [
        "bold",
        "black",
        "heavy",
        "semibold",
        "demibold",
        "extrabold",
    ]
    .iter()
    .any(|k| lower.contains(k));
    let mut italic = lower.contains("italic") || lower.contains("oblique");
    // Descripteur : direct, ou dans la police descendante d'un Type0.
    let descendant = doc
        .dict_get(dict, "DescendantFonts")
        .ok()
        .flatten()
        .and_then(|a| a.as_array().and_then(|a| a.first().cloned()))
        .and_then(|d| doc.resolve(&d).ok().and_then(|r| r.as_dict().cloned()));
    let descriptor = doc
        .dict_get(dict, "FontDescriptor")
        .ok()
        .flatten()
        .and_then(|d| d.as_dict().cloned())
        .or_else(|| {
            descendant.as_ref().and_then(|d| {
                doc.dict_get(d, "FontDescriptor")
                    .ok()
                    .flatten()
                    .and_then(|d| d.as_dict().cloned())
            })
        });
    if let Some(desc) = descriptor {
        let num = |key: &str| {
            doc.dict_get(&desc, key)
                .ok()
                .flatten()
                .and_then(|o| o.as_f64())
        };
        if num("FontWeight").is_some_and(|w| w >= 600.0) {
            bold = true;
        }
        if num("ItalicAngle").is_some_and(|a| a.abs() > 0.01) {
            italic = true;
        }
        if let Some(flags) = num("Flags") {
            // Bit 7 : Italic ; bit 19 : ForceBold (table 123).
            let flags = flags.max(0.0);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let flags = flags as u64;
            italic |= flags & (1 << 6) != 0;
            bold |= flags & (1 << 18) != 0;
        }
    }
    (bold, italic)
}

/// Nom de police sans le préfixe de sous-ensemble (`ABCDEF+`).
fn strip_subset_prefix(name: &str) -> &str {
    match name.split_once('+') {
        Some((prefix, rest))
            if prefix.len() == 6 && prefix.bytes().all(|b| b.is_ascii_uppercase()) =>
        {
            rest
        }
        _ => name,
    }
}

impl Extractor<'_> {
    fn font(&mut self, resources: &Dict, name: &Name) -> Option<Rc<FontInfo>> {
        let entry = self
            .doc
            .dict_get(resources, "Font")
            .ok()
            .flatten()
            .and_then(|f| f.as_dict().and_then(|d| d.get(name).cloned()));
        let key = match &entry {
            Some(Object::Reference(r)) => format!("ref:{}", r.number),
            Some(o) => format!("direct:{}", acrux_document::writer::to_string(o)),
            None => return None,
        };
        if let Some(f) = self.fonts.get(&key) {
            return Some(Rc::clone(f));
        }
        let dict = self.doc.resolve(entry.as_ref()?).ok()?.as_dict().cloned()?;
        let font = Rc::new(LoadedFont::load(self.doc, &dict).ok()?);
        let (bold, italic) = font_style(self.doc, &dict, &font.base_font);
        let info = Rc::new(FontInfo {
            name: strip_subset_prefix(&font.base_font).to_string(),
            font,
            bold,
            italic,
        });
        self.fonts.insert(key, Rc::clone(&info));
        Some(info)
    }

    #[allow(clippy::too_many_lines)] // un bras par opérateur, comme l'interpréteur de rendu
    fn run(&mut self, content: &[u8], resources: &Dict, initial_ctm: Matrix) {
        let mut st = State::new(initial_ctm);
        let mut stack: Vec<Saved> = Vec::new();
        let mut lexer = ContentLexer::new(content);
        while let Ok(Some(op)) = lexer.next_operation() {
            self.ops += 1;
            if self.ops > MAX_OPS {
                return;
            }
            let nums: Vec<f64> = op.operands.iter().filter_map(Object::as_f64).collect();
            let n = |i: usize| nums.get(i).copied().unwrap_or(0.0);
            match op.operator.as_slice() {
                b"q" => stack.push(st.saved()),
                b"Q" => {
                    if let Some(s) = stack.pop() {
                        st.restore(s);
                    }
                }
                b"cm" if nums.len() >= 6 => {
                    st.ctm = Matrix::new(n(0), n(1), n(2), n(3), n(4), n(5)).then(&st.ctm);
                }
                b"w" => st.line_width = n(0),
                // Couleurs (§8.6.8) : seules les composantes numériques sont interprétées.
                b"g" | b"rg" | b"k" => {
                    if let Some(c) = to_rgb(&nums) {
                        st.fill = c;
                        st.fill_components = nums.len();
                    }
                }
                b"G" | b"RG" | b"K" => {
                    if let Some(c) = to_rgb(&nums) {
                        st.stroke = c;
                        st.stroke_components = nums.len();
                    }
                }
                b"cs" | b"CS" => {
                    if let Some(Object::Name(name)) = op.operands.first() {
                        let count = colorspace_components(self.doc, resources, name);
                        // Couleur initiale : noir (§8.6.3), sauf motif (inchangé).
                        if op.operator[0] == b'c' {
                            st.fill_components = count;
                            if count > 0 {
                                st.fill = [0.0; 3];
                            }
                        } else {
                            st.stroke_components = count;
                            if count > 0 {
                                st.stroke = [0.0; 3];
                            }
                        }
                    }
                }
                b"sc" | b"scn" => {
                    if let Some(c) = tint_to_rgb(&nums, st.fill_components) {
                        st.fill = c;
                    }
                }
                b"SC" | b"SCN" => {
                    if let Some(c) = tint_to_rgb(&nums, st.stroke_components) {
                        st.stroke = c;
                    }
                }
                // Chemins (§8.5.2) : construction en espace utilisateur.
                b"m" if nums.len() >= 2 => self.path.move_to(st.ctm.apply(Point::new(n(0), n(1)))),
                b"l" if nums.len() >= 2 => self.path.line_to(st.ctm.apply(Point::new(n(0), n(1)))),
                b"c" if nums.len() >= 6 => {
                    for i in 0..3 {
                        self.path
                            .add_point(st.ctm.apply(Point::new(n(2 * i), n(2 * i + 1))));
                    }
                    self.path.current = Some(st.ctm.apply(Point::new(n(4), n(5))));
                    self.path.has_curve = true;
                }
                b"v" | b"y" if nums.len() >= 4 => {
                    for i in 0..2 {
                        self.path
                            .add_point(st.ctm.apply(Point::new(n(2 * i), n(2 * i + 1))));
                    }
                    self.path.current = Some(st.ctm.apply(Point::new(n(2), n(3))));
                    self.path.has_curve = true;
                }
                b"h" => self.path.close(),
                b"re" if nums.len() >= 4 => {
                    let m = st.ctm;
                    let corners = [
                        m.apply(Point::new(n(0), n(1))),
                        m.apply(Point::new(n(0) + n(2), n(1))),
                        m.apply(Point::new(n(0) + n(2), n(1) + n(3))),
                        m.apply(Point::new(n(0), n(1) + n(3))),
                    ];
                    let r = corners.iter().skip(1).fold(
                        Rect::new(corners[0].x, corners[0].y, corners[0].x, corners[0].y),
                        |r, p| r.union(&Rect::new(p.x, p.y, p.x, p.y)),
                    );
                    self.path.rects.push(r);
                    self.path.add_point(corners[0]);
                    self.path.add_point(corners[2]);
                    self.path.current = Some(corners[0]);
                    self.path.start = Some(corners[0]);
                }
                b"S" | b"s" => {
                    if op.operator[0] == b's' {
                        self.path.close();
                    }
                    self.paint(&st, false, true);
                }
                b"f" | b"F" | b"f*" => self.paint(&st, true, false),
                b"B" | b"B*" | b"b" | b"b*" => {
                    if op.operator[0] == b'b' {
                        self.path.close();
                    }
                    self.paint(&st, true, true);
                }
                b"n" => self.path.clear(),
                // Contenu balisé (§14.6) : `/Lbl` signale une étiquette de liste.
                b"BDC" | b"BMC" => {
                    let is_label =
                        matches!(op.operands.first(), Some(Object::Name(n)) if n.0 == b"Lbl");
                    if self.marked.len() < 256 {
                        self.marked.push(is_label);
                    }
                }
                b"EMC" => {
                    self.marked.pop();
                }
                // Texte (§9.4).
                b"BT" => {
                    st.tm = Matrix::IDENTITY;
                    st.tlm = Matrix::IDENTITY;
                }
                b"Tc" => st.char_spacing = n(0),
                b"Tw" => st.word_spacing = n(0),
                b"Tz" => st.hscale = n(0) / 100.0,
                b"TL" => st.leading = n(0),
                b"Ts" => st.rise = n(0),
                #[allow(clippy::cast_possible_truncation)]
                b"Tr" => st.render_mode = n(0) as i64,
                b"Tf" => {
                    st.size = n(0);
                    if let Some(Object::Name(name)) = op.operands.first() {
                        st.font = self.font(resources, name);
                    }
                }
                b"Td" => {
                    st.tlm = Matrix::translate(n(0), n(1)).then(&st.tlm);
                    st.tm = st.tlm;
                }
                b"TD" => {
                    st.leading = -n(1);
                    st.tlm = Matrix::translate(n(0), n(1)).then(&st.tlm);
                    st.tm = st.tlm;
                }
                b"Tm" if nums.len() >= 6 => {
                    st.tlm = Matrix::new(n(0), n(1), n(2), n(3), n(4), n(5));
                    st.tm = st.tlm;
                }
                b"T*" => {
                    st.tlm = Matrix::translate(0.0, -st.leading).then(&st.tlm);
                    st.tm = st.tlm;
                }
                b"Tj" => {
                    if let Some(Object::String(s)) = op.operands.first() {
                        self.show(&mut st, s);
                    }
                }
                b"'" => {
                    st.tlm = Matrix::translate(0.0, -st.leading).then(&st.tlm);
                    st.tm = st.tlm;
                    if let Some(Object::String(s)) = op.operands.last() {
                        self.show(&mut st, s);
                    }
                }
                b"\"" => {
                    st.word_spacing = n(0);
                    st.char_spacing = n(1);
                    st.tlm = Matrix::translate(0.0, -st.leading).then(&st.tlm);
                    st.tm = st.tlm;
                    if let Some(Object::String(s)) = op.operands.get(2) {
                        self.show(&mut st, s);
                    }
                }
                b"TJ" => {
                    if let Some(Object::Array(items)) = op.operands.last() {
                        for item in items {
                            match item {
                                Object::String(s) => self.show(&mut st, s),
                                o => {
                                    if let Some(adj) = o.as_f64() {
                                        let tx = -adj / 1000.0 * st.size * st.hscale;
                                        // Un grand recul/avance équivaut à un espace.
                                        if adj < -180.0 {
                                            self.push_space(&st);
                                        }
                                        st.tm = Matrix::translate(tx, 0.0).then(&st.tm);
                                    }
                                }
                            }
                        }
                    }
                }
                b"Do" => {
                    if let Some(Object::Name(name)) = op.operands.first() {
                        self.do_form(resources, name, &st);
                    }
                }
                _ => {}
            }
        }
    }

    /// Fin de chemin : classe les formes en filets et marques, puis vide le chemin.
    fn paint(&mut self, st: &State, fill: bool, stroke: bool) {
        let in_label = self.marked.last().copied().unwrap_or(false);
        let path = std::mem::take(&mut self.path);
        if self.out.rules.len() + self.out.marks.len() >= MAX_SHAPES {
            return;
        }
        let lw = st.line_width * st.scale();
        let thin = |r: &Rect| {
            r.width().min(r.height()) <= RULE_MAX_THICKNESS
                && r.width().max(r.height()) >= RULE_MIN_LENGTH
        };
        if fill {
            for r in &path.rects {
                if thin(r) {
                    self.out.rules.push(*r);
                } else if is_mark(r) {
                    self.out.marks.push(*r);
                }
            }
            if path.rects.is_empty() {
                if let Some(b) = path.bbox {
                    if is_mark(&b) || (in_label && b.width() <= MARK_MAX_SIZE * 2.0) {
                        self.out.marks.push(b);
                    } else if !path.has_curve {
                        // Polygone plein très plat : filet dessiné comme un chemin.
                        if thin(&b) {
                            self.out.rules.push(b);
                        }
                    }
                }
            }
        }
        if stroke && lw <= RULE_MAX_THICKNESS {
            let half = (lw / 2.0).max(0.25);
            for r in &path.rects {
                if r.width() >= RULE_MIN_LENGTH || r.height() >= RULE_MIN_LENGTH {
                    // Quatre côtés d'un rectangle tracé.
                    self.out
                        .rules
                        .push(Rect::new(r.x0, r.y0 - half, r.x1, r.y0 + half));
                    self.out
                        .rules
                        .push(Rect::new(r.x0, r.y1 - half, r.x1, r.y1 + half));
                    self.out
                        .rules
                        .push(Rect::new(r.x0 - half, r.y0, r.x0 + half, r.y1));
                    self.out
                        .rules
                        .push(Rect::new(r.x1 - half, r.y0, r.x1 + half, r.y1));
                }
            }
            for (a, b) in &path.segments {
                let horizontal = (a.y - b.y).abs() <= 0.5 && (a.x - b.x).abs() >= RULE_MIN_LENGTH;
                let vertical = (a.x - b.x).abs() <= 0.5 && (a.y - b.y).abs() >= RULE_MIN_LENGTH;
                if horizontal {
                    let y = f64::midpoint(a.y, b.y);
                    self.out.rules.push(Rect::new(a.x, y - half, b.x, y + half));
                } else if vertical {
                    let x = f64::midpoint(a.x, b.x);
                    self.out.rules.push(Rect::new(x - half, a.y, x + half, b.y));
                }
            }
        }
    }

    fn push_space(&mut self, st: &State) {
        let trm = st.tm.then(&st.ctm);
        let p = trm.apply(Point::new(0.0, 0.0));
        let e = trm.apply(Point::new(1.0, 0.0));
        let (angle, along, _, perp) = projections(p, e);
        self.out.glyphs.push(Glyph {
            text: " ".into(),
            bbox: Rect::new(p.x, p.y, p.x, p.y),
            size: st.size,
            font: st.font.as_ref().map(|f| f.name.clone()).unwrap_or_default(),
            is_space: true,
            angle,
            along,
            along_end: along,
            perp,
            bold: false,
            italic: false,
            color: st.fill,
        });
    }

    fn show(&mut self, st: &mut State, bytes: &[u8]) {
        let Some(info) = st.font.clone() else { return };
        let font = &info.font;
        // Modes 1 et 5 : texte tracé au contour, couleur de trait.
        let color = if matches!(st.render_mode, 1 | 5) {
            st.stroke
        } else {
            st.fill
        };
        for g in font.decode(bytes) {
            let trm = Matrix::new(st.size * st.hscale, 0.0, 0.0, st.size, 0.0, st.rise)
                .then(&st.tm)
                .then(&st.ctm);
            let mut tx = g.width * st.size + st.char_spacing;
            if g.is_space {
                tx += st.word_spacing;
            }
            tx *= st.hscale;
            // Tous les modes de rendu sont extraits, y compris le texte invisible
            // (mode 3) des couches OCR, comme le fait Acrobat.
            let text = font.to_unicode(&g).unwrap_or_default();
            // Boîte : de l'origine à l'avance, hauteur ≈ taille (descente 0,2, montée 0,8).
            let p0 = trm.apply(Point::new(0.0, -0.2));
            let p1 = trm.apply(Point::new(g.width.max(0.0), 0.8));
            let bbox = Rect::new(p0.x, p0.y, p1.x, p1.y);
            let origin = trm.apply(Point::new(0.0, 0.0));
            let end = trm.apply(Point::new(g.width.max(0.0), 0.0));
            let (angle, along, along_end, perp) = projections(origin, end);
            let is_space = g.is_space || text.trim().is_empty();
            if !text.is_empty() {
                self.out.glyphs.push(Glyph {
                    text: if is_space { " ".into() } else { text },
                    bbox,
                    size: (st.size * st.tm.a.hypot(st.tm.b) * st.ctm.a.hypot(st.ctm.b)).abs(),
                    font: info.name.clone(),
                    is_space,
                    angle,
                    along,
                    along_end,
                    perp,
                    bold: info.bold,
                    italic: info.italic,
                    color,
                });
            }
            st.tm = Matrix::translate(tx, 0.0).then(&st.tm);
        }
    }

    fn do_form(&mut self, resources: &Dict, name: &Name, st: &State) {
        if self.depth > 16 {
            return;
        }
        let Some(xobj) = self
            .doc
            .dict_get(resources, "XObject")
            .ok()
            .flatten()
            .and_then(|x| x.as_dict().and_then(|d| d.get(name).cloned()))
        else {
            return;
        };
        let Ok(resolved) = self.doc.resolve(&xobj) else {
            return;
        };
        let Object::Stream { dict, .. } = &*resolved else {
            return;
        };
        if dict.get(&Name::new("Subtype")).and_then(Object::as_name) != Some(&Name::new("Form")) {
            return;
        }
        let Ok(content) = self.doc.stream_data(&resolved) else {
            return;
        };
        let m: Vec<f64> = dict
            .get(&Name::new("Matrix"))
            .and_then(|m| m.as_array())
            .map(|a| a.iter().filter_map(Object::as_f64).collect())
            .unwrap_or_default();
        let mut ctm = st.ctm;
        if m.len() == 6 {
            ctm = Matrix::new(m[0], m[1], m[2], m[3], m[4], m[5]).then(&ctm);
        }
        let res = self
            .doc
            .dict_get(dict, "Resources")
            .ok()
            .flatten()
            .and_then(|r| r.as_dict().cloned())
            .unwrap_or_else(|| resources.clone());
        self.depth += 1;
        let saved_path = std::mem::take(&mut self.path);
        self.run(&content.data, &res, ctm);
        self.path = saved_path;
        self.depth -= 1;
    }
}

/// `sc`/`scn` : composantes selon l'espace courant ; un nom de motif ou un
/// nombre de composantes inattendu laisse la couleur inchangée. Pour les
/// espaces à une composante autres que le gris (Separation, Indexed…), la
/// teinte est approximée par un gris inversé (1 = encre pleine).
fn tint_to_rgb(nums: &[f64], components: usize) -> Option<[f32; 3]> {
    if components == 0 || nums.len() != components {
        return None;
    }
    to_rgb(nums)
}

/// Petite forme pleine, à peu près carrée : puce, case, coche.
fn is_mark(r: &Rect) -> bool {
    let (w, h) = (r.width(), r.height());
    (MARK_MIN_SIZE..=MARK_MAX_SIZE).contains(&w)
        && (MARK_MIN_SIZE..=MARK_MAX_SIZE).contains(&h)
        && w / h <= 2.5
        && h / w <= 2.5
}

/// Interprète le contenu d'une page.
///
/// # Errors
/// Ressources illisibles.
pub fn extract(doc: &Document, page: &Page) -> Result<Extracted> {
    let resources = doc
        .dict_get(&page.dict, "Resources")?
        .and_then(|r| r.as_dict().cloned())
        .unwrap_or_default();
    let content = page_content(doc, page);
    let mut ex = Extractor {
        doc,
        out: Extracted {
            glyphs: Vec::new(),
            rules: Vec::new(),
            marks: Vec::new(),
        },
        fonts: HashMap::new(),
        depth: 0,
        ops: 0,
        path: PathBuild::default(),
        marked: Vec::new(),
    };
    // Espace utilisateur non tourné : la rotation de page n'affecte pas l'ordre de lecture.
    ex.run(&content, &resources, Matrix::IDENTITY);
    Ok(ex.out)
}
