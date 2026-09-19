//! Interpréteur de flux de contenu (ISO 32000-2 §8 et §9) : exécute les
//! opérateurs graphiques et texte sur un [`Bitmap`] via le rasteriseur.
//!
//! Principes : jamais de panique (un opérateur mal formé est ignoré et
//! consigné), budgets d'opérations et de profondeur contre les fichiers
//! hostiles, et fidélité à Acrobat pour les cas ambigus (voir les
//! commentaires « Acrobat : … »).

// Code de rasterisation : les conversions `as` entre flottants et entiers sont
// volontaires (coordonnées bornées, indices de LUT), les noms à une lettre
// suivent la notation de la spécification (x, y, w, h, r, g, b) et les grandes
// fonctions reflètent les tables d'opérateurs de §8 et §9.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    clippy::too_many_lines,
    clippy::too_many_arguments
)]

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

use acrux_core::{Matrix, Path, Point, Rect};
use acrux_document::{Dict, Document, Name, Object, ObjectRef};
use acrux_graphics::{
    Bitmap, BlendMode, Color, FillRule, ImagePaint, LineCap, LineJoin, Mask, Paint, Rasterizer,
    StrokeStyle,
};

use crate::colorspace::ColorSpace;
use crate::content::{ContentLexer, InlineImage};
use crate::font::{GlyphRef, LoadedFont};
use crate::function::Function;
use crate::image::{decode_image, DecodedImage};
use crate::shading::{Shading, Triangle};
use crate::state::{ColorState, GraphicsState, SoftMask, TextRenderMode, TextState};

/// Limites de sécurité.
const MAX_OPS: usize = 5_000_000;
const MAX_NESTING: usize = 24;
const MAX_TILES: usize = 16_384;

/// Options de rendu.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Dessiner les annotations (apparences normales).
    pub annotations: bool,
    /// Durée maximale par page ; au-delà le rendu s'arrête proprement.
    pub time_budget: Option<std::time::Duration>,
    /// Couleur de fond (blanc par défaut ; `None` = transparent).
    pub background: Option<Color>,
    /// Visibilité imposée à des groupes de contenu optionnel, par numéro
    /// d'objet : `true` = visible, `false` = masqué. Ce que l'utilisateur
    /// coche dans le panneau des calques passe par là et l'emporte sur la
    /// configuration par défaut du document.
    pub layers: HashMap<u32, bool>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            annotations: true,
            time_budget: None,
            background: Some(Color::WHITE),
            layers: HashMap::new(),
        }
    }
}

/// État propre à l'exécution d'un flux de contenu (page, formulaire, motif, glyphe Type 3).
struct Exec {
    stack: Vec<GraphicsState>,
    gs: GraphicsState,
    /// Matrice de base du flux (espace du motif pour les motifs de ce flux).
    base_ctm: Matrix,
    path: Path,
    pending_clip: Option<FillRule>,
    text_matrix: Matrix,
    line_matrix: Matrix,
    /// Masque de découpe accumulé par les modes de texte avec découpe.
    text_clip: Option<Mask>,
    text_clip_active: bool,
    /// Profondeur de `BDC` cachés (contenu optionnel désactivé).
    hidden_depth: usize,
    /// Compteur d'imbrication BMC/BDC pour retrouver le EMC correspondant.
    marked_depth: Vec<bool>,
    /// Type 3 : `d0`/`d1` vus (pour ignorer les couleurs après `d1`).
    type3_uncolored: bool,
}

impl Exec {
    fn new(gs: GraphicsState) -> Self {
        Self {
            base_ctm: gs.ctm,
            gs,
            stack: Vec::new(),
            path: Path::new(),
            pending_clip: None,
            text_matrix: Matrix::IDENTITY,
            line_matrix: Matrix::IDENTITY,
            text_clip: None,
            text_clip_active: false,
            hidden_depth: 0,
            marked_depth: Vec::new(),
            type3_uncolored: false,
        }
    }
}

/// Moteur de rendu d'un document.
pub struct Renderer<'a> {
    doc: &'a Document,
    target: Bitmap,
    raster: Rasterizer,
    fonts: HashMap<String, Rc<LoadedFont>>,
    images: HashMap<u32, Rc<Bitmap>>,
    hidden_oc: HashSet<u32>,
    ops: usize,
    depth: usize,
    start: Instant,
    options: RenderOptions,
    warnings: Vec<String>,
    /// Formulaires en cours d'exécution (garde contre la récursion `Do` cyclique).
    active_forms: Vec<u32>,
}

impl<'a> Renderer<'a> {
    /// Nouveau moteur sur un bitmap `width × height`.
    #[must_use]
    pub fn new(doc: &'a Document, width: u32, height: u32, options: RenderOptions) -> Self {
        let target = match options.background {
            Some(c) => Bitmap::new_filled(width, height, c),
            None => Bitmap::new(width, height),
        };
        Self {
            doc,
            target,
            raster: Rasterizer::new(),
            fonts: HashMap::new(),
            images: HashMap::new(),
            hidden_oc: hidden_optional_content(doc, &options.layers),
            ops: 0,
            depth: 0,
            start: Instant::now(),
            options,
            warnings: Vec::new(),
            active_forms: Vec::new(),
        }
    }

    /// Bitmap résultat.
    #[must_use]
    pub fn into_bitmap(self) -> Bitmap {
        self.target
    }

    /// Bitmap courant.
    #[must_use]
    pub fn bitmap(&self) -> &Bitmap {
        &self.target
    }

    /// Avertissements accumulés.
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    fn warn(&mut self, msg: impl Into<String>) {
        if self.warnings.len() < 200 {
            self.warnings.push(msg.into());
        }
    }

    fn out_of_budget(&self) -> bool {
        if self.ops > MAX_OPS {
            return true;
        }
        match self.options.time_budget {
            Some(b) => self.start.elapsed() > b,
            None => false,
        }
    }

    /// Exécute un flux de contenu avec ses ressources et un état initial.
    pub fn run(&mut self, content: &[u8], resources: &Dict, initial: GraphicsState) {
        let mut exec = Exec::new(initial);
        self.execute(content, resources, &mut exec);
    }

    #[allow(clippy::too_many_lines)]
    fn execute(&mut self, content: &[u8], resources: &Dict, ex: &mut Exec) {
        let mut lexer = ContentLexer::new(content);
        loop {
            let op = match lexer.next_operation() {
                Ok(Some(op)) => op,
                Ok(None) => break,
                Err(e) => {
                    self.warn(format!("flux de contenu illisible : {e}"));
                    break;
                }
            };
            self.ops += 1;
            if self.out_of_budget() {
                self.warn("budget de rendu épuisé : page incomplète");
                break;
            }
            // Contenu optionnel masqué : on ne suit que l'imbrication BDC/EMC.
            if ex.hidden_depth > 0 {
                match op.operator.as_slice() {
                    b"BDC" | b"BMC" => ex.marked_depth.push(false),
                    // La garde dépile toujours ; on ne décrémente que si la
                    // section refermée était celle qui a caché le contenu.
                    b"EMC" if ex.marked_depth.pop() == Some(true) => ex.hidden_depth -= 1,
                    _ => {}
                }
                continue;
            }
            let nums: Vec<f64> = op
                .operands
                .iter()
                .map(|o| o.as_f64().unwrap_or(0.0))
                .collect();
            let n = |i: usize| nums.get(i).copied().unwrap_or(0.0);
            match op.operator.as_slice() {
                // --- État graphique (§8.4.4) ---
                b"q" => {
                    ex.stack.push(ex.gs.clone());
                    if ex.stack.len() > 256 {
                        ex.stack.remove(0);
                    }
                }
                b"Q" => {
                    if let Some(g) = ex.stack.pop() {
                        ex.gs = g;
                    }
                }
                b"cm" if nums.len() >= 6 => {
                    let m = Matrix::new(n(0), n(1), n(2), n(3), n(4), n(5));
                    ex.gs.ctm = m.then(&ex.gs.ctm);
                }
                b"w" => ex.gs.line_width = n(0).abs(),
                b"J" => ex.gs.line_cap = line_cap(n(0)),
                b"j" => ex.gs.line_join = line_join(n(0)),
                b"M" => ex.gs.miter_limit = n(0).max(1.0),
                b"d" => ex.gs.dash = dash_from(&op.operands),
                // Sans effet sur le rendu : intention de rendu et tolérance
                // d'aplatissement (`ri`, `i`, §8.4.4), métriques Type 3 colorées
                // (`d0`, §9.6.5), points et contenu marqués, compatibilité
                // (`MP`, `DP`, `BX`, `EX`, §8.11 et §7.8.2).
                b"ri" | b"i" | b"d0" | b"MP" | b"DP" | b"BX" | b"EX" => {}
                b"gs" => {
                    if let Some(Object::Name(name)) = op.operands.first() {
                        self.apply_ext_gstate(resources, name, ex);
                    }
                }
                // --- Construction de chemins (§8.5.2) ---
                b"m" if nums.len() >= 2 => ex.path.move_to(Point::new(n(0), n(1))),
                b"l" if nums.len() >= 2 => ex.path.line_to(Point::new(n(0), n(1))),
                b"c" if nums.len() >= 6 => {
                    ex.path.curve_to(
                        Point::new(n(0), n(1)),
                        Point::new(n(2), n(3)),
                        Point::new(n(4), n(5)),
                    );
                }
                b"v" if nums.len() >= 4 => {
                    let cur = ex.path.current_point();
                    ex.path
                        .curve_to(cur, Point::new(n(0), n(1)), Point::new(n(2), n(3)));
                }
                b"y" if nums.len() >= 4 => {
                    let end = Point::new(n(2), n(3));
                    ex.path.curve_to(Point::new(n(0), n(1)), end, end);
                }
                b"h" => ex.path.close(),
                b"re" if nums.len() >= 4 => {
                    ex.path
                        .rect(&Rect::new(n(0), n(1), n(0) + n(2), n(1) + n(3)));
                }
                // --- Peinture de chemins (§8.5.3) ---
                b"S" => self.paint_path(ex, resources, false, true, FillRule::NonZero),
                b"s" => {
                    ex.path.close();
                    self.paint_path(ex, resources, false, true, FillRule::NonZero);
                }
                b"f" | b"F" => self.paint_path(ex, resources, true, false, FillRule::NonZero),
                b"f*" => self.paint_path(ex, resources, true, false, FillRule::EvenOdd),
                b"B" => self.paint_path(ex, resources, true, true, FillRule::NonZero),
                b"B*" => self.paint_path(ex, resources, true, true, FillRule::EvenOdd),
                b"b" => {
                    ex.path.close();
                    self.paint_path(ex, resources, true, true, FillRule::NonZero);
                }
                b"b*" => {
                    ex.path.close();
                    self.paint_path(ex, resources, true, true, FillRule::EvenOdd);
                }
                b"n" => self.paint_path(ex, resources, false, false, FillRule::NonZero),
                b"W" => ex.pending_clip = Some(FillRule::NonZero),
                b"W*" => ex.pending_clip = Some(FillRule::EvenOdd),
                // --- Couleur (§8.6.8) ---
                b"CS" | b"cs" => {
                    if ex.type3_uncolored {
                        continue;
                    }
                    let space = match op.operands.first() {
                        Some(o) => {
                            ColorSpace::parse(self.doc, o, Some(resources)).unwrap_or_else(|e| {
                                self.warn(format!("espace colorimétrique : {e}"));
                                ColorSpace::DeviceGray
                            })
                        }
                        None => ColorSpace::DeviceGray,
                    };
                    let cs = ColorState {
                        components: space.initial_color(),
                        space,
                        pattern: None,
                    };
                    if op.operator[0] == b'C' {
                        ex.gs.stroke = cs;
                    } else {
                        ex.gs.fill = cs;
                    }
                }
                b"SC" | b"SCN" | b"sc" | b"scn" => {
                    if ex.type3_uncolored {
                        continue;
                    }
                    let target = if op.operator[0] == b'S' {
                        &mut ex.gs.stroke
                    } else {
                        &mut ex.gs.fill
                    };
                    if target.space.is_pattern() {
                        if let Some(Object::Name(p)) = op.operands.last() {
                            target.pattern = Some(p.0.clone());
                        }
                        // Composantes du motif non coloré (avant le nom).
                        let comps: Vec<f64> =
                            op.operands.iter().filter_map(Object::as_f64).collect();
                        if !comps.is_empty() {
                            target.components = comps;
                        }
                    } else if !nums.is_empty() {
                        target.components.clone_from(&nums);
                    }
                }
                b"G" | b"g" => {
                    set_device_color(ex, op.operator[0] == b'G', ColorSpace::DeviceGray, &nums);
                }
                b"RG" | b"rg" => {
                    set_device_color(ex, op.operator[0] == b'R', ColorSpace::DeviceRgb, &nums);
                }
                b"K" | b"k" => {
                    set_device_color(ex, op.operator[0] == b'K', ColorSpace::DeviceCmyk, &nums);
                }
                // --- Texte (§9.4) ---
                b"BT" => {
                    ex.text_matrix = Matrix::IDENTITY;
                    ex.line_matrix = Matrix::IDENTITY;
                }
                b"ET" => {
                    if ex.text_clip_active {
                        let mask = ex.text_clip.take().unwrap_or_else(|| {
                            Mask::new(self.target.width(), self.target.height())
                        });
                        ex.gs.intersect_clip(mask);
                        // Acrobat : la découpe s'applique aussi aux états empilés depuis BT ? Non : à l'état courant.
                        ex.text_clip_active = false;
                    }
                }
                b"Tc" => ex.gs.text.char_spacing = n(0),
                b"Tw" => ex.gs.text.word_spacing = n(0),
                b"Tz" => ex.gs.text.horizontal_scale = n(0) / 100.0,
                b"TL" => ex.gs.text.leading = n(0),
                b"Ts" => ex.gs.text.rise = n(0),
                b"Tr" => ex.gs.text.render_mode = TextRenderMode::from_i64(n(0) as i64),
                b"Tf" => {
                    ex.gs.text.size = n(1);
                    if let Some(Object::Name(name)) = op.operands.first() {
                        ex.gs.text.font = self.load_font(resources, name);
                    }
                }
                b"Td" => {
                    ex.line_matrix = Matrix::translate(n(0), n(1)).then(&ex.line_matrix);
                    ex.text_matrix = ex.line_matrix;
                }
                b"TD" => {
                    ex.gs.text.leading = -n(1);
                    ex.line_matrix = Matrix::translate(n(0), n(1)).then(&ex.line_matrix);
                    ex.text_matrix = ex.line_matrix;
                }
                b"Tm" if nums.len() >= 6 => {
                    ex.line_matrix = Matrix::new(n(0), n(1), n(2), n(3), n(4), n(5));
                    ex.text_matrix = ex.line_matrix;
                }
                b"T*" => next_line(ex),
                b"Tj" => {
                    if let Some(Object::String(s)) = op.operands.first() {
                        self.show_text(ex, resources, s);
                    }
                }
                b"'" => {
                    next_line(ex);
                    if let Some(Object::String(s)) = op.operands.last() {
                        self.show_text(ex, resources, s);
                    }
                }
                b"\"" => {
                    ex.gs.text.word_spacing = n(0);
                    ex.gs.text.char_spacing = n(1);
                    next_line(ex);
                    if let Some(Object::String(s)) = op.operands.get(2) {
                        self.show_text(ex, resources, s);
                    }
                }
                b"TJ" => {
                    if let Some(Object::Array(items)) = op.operands.last() {
                        for item in items {
                            match item {
                                Object::String(s) => self.show_text(ex, resources, s),
                                o => {
                                    if let Some(adj) = o.as_f64() {
                                        let ts = &ex.gs.text;
                                        let tx = -adj / 1000.0 * ts.size * ts.horizontal_scale;
                                        ex.text_matrix =
                                            Matrix::translate(tx, 0.0).then(&ex.text_matrix);
                                    }
                                }
                            }
                        }
                    }
                }
                // --- Type 3 ---
                b"d1" => ex.type3_uncolored = true,
                // --- XObjects, images, ombrages ---
                b"Do" => {
                    if let Some(Object::Name(name)) = op.operands.first() {
                        self.do_xobject(resources, name, ex);
                    }
                }
                b"BI" => {
                    if let Some(img) = &op.inline_image {
                        self.draw_inline_image(img, resources, ex);
                    }
                }
                b"sh" => {
                    if let Some(Object::Name(name)) = op.operands.first() {
                        self.do_shading(resources, name, ex);
                    }
                }
                // --- Contenu marqué et optionnel (§8.11) ---
                b"BDC" => {
                    let hidden = self.is_hidden_marked_content(resources, &op.operands);
                    ex.marked_depth.push(hidden);
                    if hidden {
                        ex.hidden_depth += 1;
                    }
                }
                b"BMC" => ex.marked_depth.push(false),
                b"EMC" => {
                    ex.marked_depth.pop();
                }
                other => {
                    if self.warnings.len() < 50 {
                        self.warn(format!(
                            "opérateur inconnu `{}`",
                            String::from_utf8_lossy(other)
                        ));
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Ressources
    // ------------------------------------------------------------------

    fn resource(&self, resources: &Dict, category: &str, name: &Name) -> Option<Object> {
        let cat = self.doc.dict_get(resources, category).ok().flatten()?;
        let cat = cat.as_dict()?;
        let entry = cat.get(name)?;
        Some(entry.clone())
    }

    fn load_font(&mut self, resources: &Dict, name: &Name) -> Option<Rc<LoadedFont>> {
        let entry = self.resource(resources, "Font", name);
        let key = match &entry {
            Some(Object::Reference(r)) => format!("ref:{}", r.number),
            Some(Object::Dict(d)) => format!(
                "direct:{}",
                acrux_document::writer::to_string(&Object::Dict(d.clone()))
            ),
            _ => {
                // Police absente des ressources : Acrobat prend Helvetica.
                self.warn(format!("police /{} absente des ressources", name.as_str()));
                "fallback:Helvetica".to_string()
            }
        };
        if let Some(f) = self.fonts.get(&key) {
            return Some(Rc::clone(f));
        }
        let dict = match &entry {
            Some(o) => self.doc.resolve(o).ok().and_then(|r| r.as_dict().cloned()),
            None => None,
        }
        .unwrap_or_else(|| {
            let mut d = Dict::new();
            d.insert(Name::new("BaseFont"), Object::Name(Name::new("Helvetica")));
            d.insert(Name::new("Subtype"), Object::Name(Name::new("Type1")));
            d
        });
        let font = match LoadedFont::load(self.doc, &dict) {
            Ok(f) => Rc::new(f),
            Err(e) => {
                self.warn(format!("police /{} illisible : {e}", name.as_str()));
                return None;
            }
        };
        self.fonts.insert(key, Rc::clone(&font));
        Some(font)
    }

    // ------------------------------------------------------------------
    // ExtGState (§8.4.5)
    // ------------------------------------------------------------------

    fn apply_ext_gstate(&mut self, resources: &Dict, name: &Name, ex: &mut Exec) {
        let Some(gs_obj) = self.resource(resources, "ExtGState", name) else {
            return;
        };
        let Some(gs) = self
            .doc
            .resolve(&gs_obj)
            .ok()
            .and_then(|r| r.as_dict().cloned())
        else {
            return;
        };
        for (k, v) in &gs {
            let v = self
                .doc
                .resolve(v)
                .ok()
                .map_or(Object::Null, |r| (*r).clone());
            match k.0.as_slice() {
                b"LW" => ex.gs.line_width = v.as_f64().unwrap_or(1.0).abs(),
                b"LC" => ex.gs.line_cap = line_cap(v.as_f64().unwrap_or(0.0)),
                b"LJ" => ex.gs.line_join = line_join(v.as_f64().unwrap_or(0.0)),
                b"ML" => ex.gs.miter_limit = v.as_f64().unwrap_or(10.0),
                b"D" => {
                    if let Object::Array(a) = &v {
                        ex.gs.dash = dash_from(a);
                    }
                }
                b"CA" => ex.gs.stroke_alpha = v.as_f64().unwrap_or(1.0).clamp(0.0, 1.0),
                b"ca" => ex.gs.fill_alpha = v.as_f64().unwrap_or(1.0).clamp(0.0, 1.0),
                b"BM" => {
                    let mode_name = match &v {
                        Object::Name(n) => Some(n.as_str()),
                        Object::Array(a) => a.first().and_then(Object::as_name).map(Name::as_str),
                        _ => None,
                    };
                    ex.gs.blend = mode_name
                        .and_then(|m| blend_mode(&m))
                        .unwrap_or(BlendMode::Normal);
                }
                b"SMask" => match &v {
                    Object::Dict(sm) => {
                        let mask = self.build_soft_mask(sm, ex);
                        ex.gs.soft_mask = mask.map(|m| SoftMask { mask: Rc::new(m) });
                    }
                    _ => ex.gs.soft_mask = None,
                },
                b"Font" => {
                    if let Object::Array(a) = &v {
                        if let (Some(fref), Some(size)) =
                            (a.first(), a.get(1).and_then(Object::as_f64))
                        {
                            if let Some(d) = self
                                .doc
                                .resolve(fref)
                                .ok()
                                .and_then(|r| r.as_dict().cloned())
                            {
                                let key = match fref {
                                    Object::Reference(r) => format!("ref:{}", r.number),
                                    _ => format!("gsfont:{}", name.as_str()),
                                };
                                let font = match self.fonts.get(&key) {
                                    Some(f) => Some(Rc::clone(f)),
                                    None => LoadedFont::load(self.doc, &d).ok().map(|f| {
                                        let f = Rc::new(f);
                                        self.fonts.insert(key, Rc::clone(&f));
                                        f
                                    }),
                                };
                                ex.gs.text.font = font;
                                ex.gs.text.size = size;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Masque souple (§11.6.5) : rend le groupe `/G` hors écran et convertit
    /// en couverture par luminosité ou alpha.
    fn build_soft_mask(&mut self, sm: &Dict, ex: &Exec) -> Option<Mask> {
        let g = self.doc.dict_get(sm, "G").ok().flatten()?;
        let Object::Stream { dict: form, .. } = &*g else {
            return None;
        };
        let kind = self
            .doc
            .dict_get(sm, "S")
            .ok()
            .flatten()
            .and_then(|o| o.as_name().map(Name::as_str))
            .unwrap_or_else(|| "Alpha".into());
        let luminosity = kind == "Luminosity";
        let (w, h) = (self.target.width(), self.target.height());
        // Fond : noir pour la luminosité (ou /BC dans l'espace du groupe), transparent pour l'alpha.
        let mut backdrop = Color::TRANSPARENT;
        if luminosity {
            backdrop = Color::BLACK;
            if let Some(bc) = self.doc.dict_get(sm, "BC").ok().flatten() {
                if let Some(a) = bc.as_array() {
                    let comps: Vec<f64> = a.iter().filter_map(Object::as_f64).collect();
                    let group = self
                        .doc
                        .dict_get(form, "Group")
                        .ok()
                        .flatten()
                        .and_then(|g| g.as_dict().cloned());
                    let cs = group
                        .and_then(|g| g.get(&Name::new("CS")).cloned())
                        .and_then(|c| ColorSpace::parse(self.doc, &c, None).ok())
                        .unwrap_or(ColorSpace::DeviceGray);
                    let rgb = cs.to_rgb(&comps);
                    backdrop = Color::rgb(rgb[0], rgb[1], rgb[2]);
                }
            }
        }
        let transfer = self
            .doc
            .dict_get(sm, "TR")
            .ok()
            .flatten()
            .and_then(|tr| match &*tr {
                Object::Name(_) => None,
                o => Function::parse(self.doc, o).ok(),
            });
        // Le groupe ne peint que dans sa `/BBox` : inutile de rendre toute la
        // page hors écran, on se limite à cette boîte en pixels. Le reste du
        // masque vaut le fond, une constante. Sur une page riche en masques,
        // c'est la différence entre huit pages entières redessinées et
        // quelques vignettes.
        let (x0, y0, bw, bh) = form_device_box(self.doc, form, ex.gs.ctm, w, h);
        if bw == 0 || bh == 0 {
            let mut mask = Mask::new(w, h);
            let value = backdrop_level(backdrop, luminosity, transfer.as_ref());
            mask.data_mut().fill(value);
            mask.set_bounds((0, 0, w, h));
            return Some(mask);
        }
        let mut sub = Renderer::new(
            self.doc,
            bw,
            bh,
            RenderOptions {
                annotations: false,
                time_budget: self.options.time_budget,
                background: Some(backdrop),
                layers: self.options.layers.clone(),
            },
        );
        sub.depth = self.depth + 1;
        sub.fonts.clone_from(&self.fonts);
        sub.hidden_oc.clone_from(&self.hidden_oc);
        // La sous-image commence au coin de la boîte : on translate d'autant.
        let shift = Matrix::new(1.0, 0.0, 0.0, 1.0, -f64::from(x0), -f64::from(y0));
        let mut gs = GraphicsState::new(ex.gs.ctm.then(&shift));
        gs.clip = None;
        // Hors de la BBox du groupe, la luminosité vaut celle du fond.
        let form_obj = (*g).clone();
        sub.draw_form(&form_obj, form, &mut Exec::new(gs), true);
        let bitmap = sub.into_bitmap();
        let mut mask = Mask::new(w, h);
        let data = bitmap.data();
        mask.set_bounds((0, 0, w, h));
        let out = mask.data_mut();
        let lut: Vec<u8> = (0..256u32)
            .map(|i| {
                let v = f64::from(i) / 255.0;
                let t = transfer
                    .as_ref()
                    .map_or(v, |f| f.eval(&[v]).first().copied().unwrap_or(v));
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                {
                    (t.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
                }
            })
            .collect();
        // Hors de la boîte, la valeur du fond ; dedans, le rendu.
        out.fill(lut[backdrop_index(backdrop, luminosity)]);
        for (row, line) in data.chunks_exact(bw as usize * 4).enumerate() {
            let y = y0 as usize + row;
            if y >= h as usize {
                break;
            }
            for (col, px) in line.chunks_exact(4).enumerate() {
                let x = x0 as usize + col;
                if x >= w as usize {
                    break;
                }
                let v = if luminosity {
                    // Luminosité du pixel composé (fond déjà appliqué).
                    let (r, g, b) = (f32::from(px[0]), f32::from(px[1]), f32::from(px[2]));
                    (0.3 * r + 0.59 * g + 0.11 * b).round().clamp(0.0, 255.0) as usize
                } else {
                    usize::from(px[3])
                };
                out[y * w as usize + x] = lut[v.min(255)];
            }
        }
        Some(mask)
    }

    // ------------------------------------------------------------------
    // Chemins
    // ------------------------------------------------------------------

    fn paint_path(
        &mut self,
        ex: &mut Exec,
        resources: &Dict,
        fill: bool,
        stroke: bool,
        rule: FillRule,
    ) {
        let path = std::mem::take(&mut ex.path);
        if fill && !path.is_empty() {
            self.fill_with_color_state(ex, resources, &path, rule, false);
        }
        if stroke && !path.is_empty() {
            let style = stroke_style(&ex.gs);
            let outline = acrux_graphics::stroke_path(&path, &style, &ex.gs.ctm);
            self.fill_device_path(ex, resources, &outline, FillRule::NonZero, true);
        }
        if let Some(clip_rule) = ex.pending_clip.take() {
            let (w, h) = (self.target.width(), self.target.height());
            let mask = self
                .raster
                .path_coverage(&path, &ex.gs.ctm, clip_rule, w, h);
            ex.gs.intersect_clip(mask);
        }
    }

    /// Remplit un chemin en espace utilisateur avec la couleur/motif de remplissage (ou de tracé).
    fn fill_with_color_state(
        &mut self,
        ex: &mut Exec,
        resources: &Dict,
        path: &Path,
        rule: FillRule,
        use_stroke: bool,
    ) {
        let device = path.transform(&ex.gs.ctm);
        self.fill_device_path(ex, resources, &device, rule, use_stroke);
    }

    /// Remplit un chemin déjà en espace device.
    fn fill_device_path(
        &mut self,
        ex: &mut Exec,
        resources: &Dict,
        device_path: &Path,
        rule: FillRule,
        use_stroke: bool,
    ) {
        let color_state = if use_stroke {
            ex.gs.stroke.clone()
        } else {
            ex.gs.fill.clone()
        };
        let alpha = if use_stroke {
            ex.gs.stroke_alpha
        } else {
            ex.gs.fill_alpha
        };
        #[allow(clippy::cast_possible_truncation)]
        let alpha = alpha as f32;
        if color_state.space.is_pattern() {
            if let Some(pname) = &color_state.pattern {
                let pname = Name(pname.clone());
                self.paint_pattern(
                    ex,
                    resources,
                    &pname,
                    device_path,
                    rule,
                    alpha,
                    &color_state,
                );
            }
            return;
        }
        let rgb = color_state.rgb();
        let paint = Paint::Solid(Color::rgb(rgb[0], rgb[1], rgb[2]));
        let clip = ex.gs.effective_clip();
        self.raster.fill_path(
            &mut self.target,
            device_path,
            &Matrix::IDENTITY,
            rule,
            &paint,
            clip.as_deref(),
            alpha,
            ex.gs.blend,
        );
    }

    /// Motifs (§8.7.3) : de pavage (contenu répété) ou d'ombrage.
    #[allow(clippy::too_many_arguments)]
    fn paint_pattern(
        &mut self,
        ex: &mut Exec,
        resources: &Dict,
        name: &Name,
        device_path: &Path,
        rule: FillRule,
        alpha: f32,
        color_state: &ColorState,
    ) {
        let Some(pobj) = self.resource(resources, "Pattern", name) else {
            self.warn(format!("motif /{} introuvable", name.as_str()));
            return;
        };
        let Some(pat) = self.doc.resolve(&pobj).ok() else {
            return;
        };
        let Some(pdict) = pat.as_dict().cloned() else {
            return;
        };
        let ptype = self
            .doc
            .dict_get(&pdict, "PatternType")
            .ok()
            .flatten()
            .and_then(|o| o.as_i64())
            .unwrap_or(1);
        let pm: Vec<f64> = self
            .doc
            .dict_get(&pdict, "Matrix")
            .ok()
            .flatten()
            .and_then(|m| {
                m.as_array()
                    .map(|a| a.iter().filter_map(Object::as_f64).collect())
            })
            .unwrap_or_default();
        let pattern_matrix = if pm.len() == 6 {
            Matrix::new(pm[0], pm[1], pm[2], pm[3], pm[4], pm[5])
        } else {
            Matrix::IDENTITY
        };
        // L'espace du motif est celui du flux dans lequel il est une ressource (§8.7.3.1).
        let pattern_ctm = pattern_matrix.then(&ex.base_ctm);
        // Découpe au chemin à peindre.
        let (w, h) = (self.target.width(), self.target.height());
        let path_mask = self
            .raster
            .path_coverage(device_path, &Matrix::IDENTITY, rule, w, h);
        let mut clip = match ex.gs.effective_clip() {
            Some(c) => c.intersect(&path_mask),
            None => path_mask,
        };
        if ptype == 2 {
            let Some(sh_obj) = pdict.get(&Name::new("Shading")) else {
                return;
            };
            let shading = match Shading::parse(self.doc, sh_obj, Some(resources)) {
                Ok(s) => s,
                Err(e) => {
                    self.warn(format!("ombrage du motif : {e}"));
                    return;
                }
            };
            // ExtGState du motif (rare) ignoré.
            self.paint_shading(&shading, &pattern_ctm, &clip, alpha, ex.gs.blend, false);
            return;
        }
        // Motif de pavage : on exécute la cellule pour chaque position couvrant la zone.
        let Object::Stream { .. } = &*pat else {
            return;
        };
        let content = match self.doc.stream_data(&pat) {
            Ok(d) => d.data,
            Err(_) => return,
        };
        let pres = self
            .doc
            .dict_get(&pdict, "Resources")
            .ok()
            .flatten()
            .and_then(|r| r.as_dict().cloned())
            .unwrap_or_default();
        let bbox: Vec<f64> = self
            .doc
            .dict_get(&pdict, "BBox")
            .ok()
            .flatten()
            .and_then(|b| {
                b.as_array()
                    .map(|a| a.iter().filter_map(Object::as_f64).collect())
            })
            .unwrap_or_default();
        if bbox.len() != 4 {
            return;
        }
        let bbox = Rect::new(bbox[0], bbox[1], bbox[2], bbox[3]);
        let xstep = self
            .doc
            .dict_get(&pdict, "XStep")
            .ok()
            .flatten()
            .and_then(|o| o.as_f64())
            .filter(|v| v.abs() > 1e-9)
            .unwrap_or(bbox.width());
        let ystep = self
            .doc
            .dict_get(&pdict, "YStep")
            .ok()
            .flatten()
            .and_then(|o| o.as_f64())
            .filter(|v| v.abs() > 1e-9)
            .unwrap_or(bbox.height());
        let paint_type = self
            .doc
            .dict_get(&pdict, "PaintType")
            .ok()
            .flatten()
            .and_then(|o| o.as_i64())
            .unwrap_or(1);
        if self.depth >= MAX_NESTING {
            return;
        }
        // Zone à couvrir : boîte du chemin en espace device → espace du motif.
        let Some(dev_bounds) = device_path.bounds() else {
            return;
        };
        let dev_bounds = dev_bounds.intersect(&Rect::new(0.0, 0.0, f64::from(w), f64::from(h)));
        if dev_bounds.is_empty() {
            return;
        }
        let Some(inv) = pattern_ctm.invert() else {
            return;
        };
        let pb = inv.transform_rect(&dev_bounds);
        let i0 = ((pb.x0 - bbox.x1) / xstep.abs()).floor();
        let i1 = ((pb.x1 - bbox.x0) / xstep.abs()).ceil();
        let j0 = ((pb.y0 - bbox.y1) / ystep.abs()).floor();
        let j1 = ((pb.y1 - bbox.y0) / ystep.abs()).ceil();
        let count = ((i1 - i0).max(0.0) * (j1 - j0).max(0.0)) as usize;
        if count > MAX_TILES {
            // Trop de cellules (motif très fin) : on peint la couleur moyenne
            // approximative, comme le ferait un aperçu basse résolution.
            self.warn("motif de pavage trop dense : approximation");
            let paint = Paint::Solid(Color::rgba(0.5, 0.5, 0.5, 1.0));
            self.raster.fill_path(
                &mut self.target,
                device_path,
                &Matrix::IDENTITY,
                rule,
                &paint,
                ex.gs.effective_clip().as_deref(),
                alpha,
                ex.gs.blend,
            );
            return;
        }
        // Motif non coloré : la couleur vient des composantes courantes.
        let uncolored_color = if paint_type == 2 {
            let base = match &color_state.space {
                ColorSpace::Pattern { base: Some(b) } => (**b).clone(),
                _ => ColorSpace::DeviceGray,
            };
            Some(ColorState {
                space: base,
                components: color_state.components.clone(),
                pattern: None,
            })
        } else {
            None
        };
        self.depth += 1;
        let clip_rc = Rc::new(std::mem::replace(&mut clip, Mask::new(0, 0)));
        let mut i = i0;
        while i < i1 {
            let mut j = j0;
            while j < j1 {
                let offset = Matrix::translate(i * xstep, j * ystep);
                let cell_ctm = offset.then(&pattern_ctm);
                let mut gs = GraphicsState::new(cell_ctm);
                gs.clip = Some(Rc::clone(&clip_rc));
                gs.fill_alpha = f64::from(alpha);
                gs.stroke_alpha = f64::from(alpha);
                gs.blend = ex.gs.blend;
                if let Some(c) = &uncolored_color {
                    gs.fill = c.clone();
                    gs.stroke = c.clone();
                }
                let mut sub = Exec::new(gs);
                sub.type3_uncolored = paint_type == 2;
                // Découpe à la BBox de la cellule.
                let mut cell_path = Path::new();
                cell_path.rect(&bbox);
                let cell_mask =
                    self.raster
                        .path_coverage(&cell_path, &cell_ctm, FillRule::NonZero, w, h);
                sub.gs.intersect_clip(cell_mask);
                self.execute(&content, &pres, &mut sub);
                if self.out_of_budget() {
                    break;
                }
                j += 1.0;
            }
            i += 1.0;
        }
        self.depth -= 1;
    }

    /// Peint un ombrage à travers un masque (déjà en device).
    fn paint_shading(
        &mut self,
        shading: &Shading,
        ctm: &Matrix,
        clip: &Mask,
        alpha: f32,
        blend: BlendMode,
        respect_bbox: bool,
    ) {
        let (w, h) = (self.target.width(), self.target.height());
        let mut clip = clip.intersect(&Mask::full(w, h));
        if respect_bbox {
            if let Some(bb) = shading.bbox {
                let mut p = Path::new();
                p.rect(&bb);
                let m = self.raster.path_coverage(&p, ctm, FillRule::NonZero, w, h);
                clip = clip.intersect(&m);
            }
        }
        if let Some(paint) = shading.paint(ctm) {
            let mut full = Path::new();
            full.rect(&Rect::new(0.0, 0.0, f64::from(w), f64::from(h)));
            self.raster.fill_path(
                &mut self.target,
                &full,
                &Matrix::IDENTITY,
                FillRule::NonZero,
                &paint,
                Some(&clip),
                alpha,
                blend,
            );
            return;
        }
        for tri in &shading.triangles {
            self.paint_triangle(tri, ctm, &clip, alpha, blend);
        }
    }

    fn paint_triangle(
        &mut self,
        tri: &Triangle,
        ctm: &Matrix,
        clip: &Mask,
        alpha: f32,
        blend: BlendMode,
    ) {
        let p: Vec<Point> = tri.points.iter().map(|q| ctm.apply(*q)).collect();
        let mut path = Path::new();
        path.move_to(p[0]);
        path.line_to(p[1]);
        path.line_to(p[2]);
        path.close();
        // Légère dilatation pour éviter les fentes entre triangles adjacents.
        let colors = tri.colors;
        let (x0, y0, x1, y1, x2, y2) = (p[0].x, p[0].y, p[1].x, p[1].y, p[2].x, p[2].y);
        let det = (y1 - y2) * (x0 - x2) + (x2 - x1) * (y0 - y2);
        let paint = if det.abs() < 1e-9 {
            Paint::Solid(Color::rgb(colors[0][0], colors[0][1], colors[0][2]))
        } else {
            Paint::Callback(Box::new(move |x, y| {
                let l0 = (((y1 - y2) * (x - x2) + (x2 - x1) * (y - y2)) / det).clamp(0.0, 1.0);
                let l1 = (((y2 - y0) * (x - x2) + (x0 - x2) * (y - y2)) / det).clamp(0.0, 1.0);
                let l2 = (1.0 - l0 - l1).clamp(0.0, 1.0);
                let mut c = [0.0f32; 3];
                for k in 0..3 {
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        c[k] = (l0 * f64::from(colors[0][k])
                            + l1 * f64::from(colors[1][k])
                            + l2 * f64::from(colors[2][k])) as f32;
                    }
                }
                Color::rgb(c[0], c[1], c[2])
            }))
        };
        self.raster.fill_path(
            &mut self.target,
            &path,
            &Matrix::IDENTITY,
            FillRule::NonZero,
            &paint,
            Some(clip),
            alpha,
            blend,
        );
    }

    fn do_shading(&mut self, resources: &Dict, name: &Name, ex: &mut Exec) {
        let Some(sh_obj) = self.resource(resources, "Shading", name) else {
            self.warn(format!("ombrage /{} introuvable", name.as_str()));
            return;
        };
        let shading = match Shading::parse(self.doc, &sh_obj, Some(resources)) {
            Ok(s) => s,
            Err(e) => {
                self.warn(format!("ombrage /{} : {e}", name.as_str()));
                return;
            }
        };
        let (w, h) = (self.target.width(), self.target.height());
        let clip = ex
            .gs
            .effective_clip()
            .map_or_else(|| Mask::full(w, h), |c| (*c).clone());
        #[allow(clippy::cast_possible_truncation)]
        let alpha = ex.gs.fill_alpha as f32;
        let ctm = ex.gs.ctm;
        self.paint_shading(&shading, &ctm, &clip, alpha, ex.gs.blend, true);
    }

    // ------------------------------------------------------------------
    // Texte (§9.4.4)
    // ------------------------------------------------------------------

    fn show_text(&mut self, ex: &mut Exec, resources: &Dict, bytes: &[u8]) {
        let Some(font) = ex.gs.text.font.clone() else {
            // Sans police : Acrobat utilise Helvetica.
            ex.gs.text.font = self.load_font(resources, &Name::new("\u{0}missing"));
            if ex.gs.text.font.is_none() {
                return;
            }
            return self.show_text(ex, resources, bytes);
        };
        let ts = ex.gs.text.clone();
        let mode = ts.render_mode;
        if mode.clips() {
            ex.text_clip_active = true;
            if ex.text_clip.is_none() {
                ex.text_clip = Some(Mask::new(self.target.width(), self.target.height()));
            }
        }
        let glyphs = font.decode(bytes);
        for g in glyphs {
            // Matrice de rendu du texte (§9.4.4) : [Tfs×Th 0 0 Tfs 0 Ts] × Tm × CTM.
            let trm = Matrix::new(
                ts.size * ts.horizontal_scale,
                0.0,
                0.0,
                ts.size,
                0.0,
                ts.rise,
            )
            .then(&ex.text_matrix)
            .then(&ex.gs.ctm);
            if mode != TextRenderMode::Invisible || mode.clips() {
                if font.is_type3() {
                    self.draw_type3_glyph(&font, &g, &trm, ex, resources);
                } else if let Some(path) = font.glyph_path_cached(&g) {
                    self.draw_glyph(&path, &trm, mode, ex, resources);
                }
            }
            // Avance (§9.4.4) : tx = (w0 × Tfs + Tc + Tw) × Th.
            let mut w0 = g.width;
            if font.is_type3() {
                // Largeur déjà en espace texte via la matrice de police.
                w0 = g.width;
            }
            let mut tx = w0 * ts.size + ts.char_spacing;
            if g.is_space {
                tx += ts.word_spacing;
            }
            tx *= ts.horizontal_scale;
            ex.text_matrix = Matrix::translate(tx, 0.0).then(&ex.text_matrix);
        }
    }

    fn draw_glyph(
        &mut self,
        path: &Path,
        trm: &Matrix,
        mode: TextRenderMode,
        ex: &mut Exec,
        resources: &Dict,
    ) {
        let device = path.transform(trm);
        if mode.fills() {
            self.fill_device_path(ex, resources, &device, FillRule::NonZero, false);
        }
        if mode.strokes() {
            let style = stroke_style(&ex.gs);
            // Le contour est tracé avec l'épaisseur en espace utilisateur : on
            // ramène le glyphe en espace utilisateur.
            if let Some(inv) = ex.gs.ctm.invert() {
                let user = device.transform(&inv);
                let outline = acrux_graphics::stroke_path(&user, &style, &ex.gs.ctm);
                self.fill_device_path(ex, resources, &outline, FillRule::NonZero, true);
            }
        }
        if mode.clips() {
            let (w, h) = (self.target.width(), self.target.height());
            let m = self
                .raster
                .path_coverage(&device, &Matrix::IDENTITY, FillRule::NonZero, w, h);
            if let Some(acc) = &mut ex.text_clip {
                acc.set_bounds((0, 0, w, h));
                let data = acc.data_mut();
                for (d, s) in data.iter_mut().zip(m.data()) {
                    *d = (*d).max(*s);
                }
            }
        }
    }

    fn draw_type3_glyph(
        &mut self,
        font: &LoadedFont,
        g: &GlyphRef,
        trm: &Matrix,
        ex: &mut Exec,
        resources: &Dict,
    ) {
        if self.depth >= MAX_NESTING {
            return;
        }
        let Some((proc_data, font_res)) = font.type3_proc(self.doc, g.code) else {
            return;
        };
        let ctm = font.font_matrix.then(trm);
        let mut gs = ex.gs.clone();
        gs.ctm = ctm;
        gs.text = TextState::default();
        let mut sub = Exec::new(gs);
        let res = font_res.unwrap_or_else(|| resources.clone());
        self.depth += 1;
        self.execute(&proc_data, &res, &mut sub);
        self.depth -= 1;
    }

    // ------------------------------------------------------------------
    // XObjects (§8.8, §8.9, §8.10)
    // ------------------------------------------------------------------

    fn do_xobject(&mut self, resources: &Dict, name: &Name, ex: &mut Exec) {
        let Some(xobj) = self.resource(resources, "XObject", name) else {
            self.warn(format!("XObject /{} introuvable", name.as_str()));
            return;
        };
        let xref = match &xobj {
            Object::Reference(r) => Some(*r),
            _ => None,
        };
        let Some(resolved) = self.doc.resolve(&xobj).ok() else {
            return;
        };
        let Object::Stream { dict, .. } = &*resolved else {
            return;
        };
        if self.is_hidden_oc(dict) {
            return;
        }
        let subtype = dict
            .get(&Name::new("Subtype"))
            .and_then(Object::as_name)
            .map(|n| n.0.clone());
        match subtype.as_deref() {
            Some(b"Image") => self.draw_image_xobject(&resolved, dict, xref, ex, resources),
            Some(b"Form") | None => {
                if let Some(r) = xref {
                    if self.active_forms.contains(&r.number) {
                        self.warn("formulaire XObject récursif ignoré");
                        return;
                    }
                    self.active_forms.push(r.number);
                }
                let obj = (*resolved).clone();
                let mut sub = Exec::new(ex.gs.clone());
                sub.base_ctm = ex.gs.ctm;
                self.draw_form(&obj, dict, &mut sub, false);
                if xref.is_some() {
                    self.active_forms.pop();
                }
            }
            Some(b"PS") => {}
            Some(other) => self.warn(format!(
                "XObject de type {} ignoré",
                String::from_utf8_lossy(other)
            )),
        }
    }

    /// Dessine un formulaire (§8.10) : matrice, découpe à la BBox, ressources
    /// propres, groupe de transparence hors écran si nécessaire.
    fn draw_form(&mut self, obj: &Object, dict: &Dict, ex: &mut Exec, is_soft_mask_group: bool) {
        if self.depth >= MAX_NESTING {
            self.warn("imbrication de formulaires trop profonde");
            return;
        }
        let content = match self.doc.stream_data(obj) {
            Ok(d) => d.data,
            Err(e) => {
                self.warn(format!("formulaire illisible : {e}"));
                return;
            }
        };
        let m: Vec<f64> = self
            .doc
            .dict_get(dict, "Matrix")
            .ok()
            .flatten()
            .and_then(|m| {
                m.as_array().map(|a| {
                    a.iter()
                        .filter_map(|o| self.doc.resolve(o).ok().and_then(|r| r.as_f64()))
                        .collect()
                })
            })
            .unwrap_or_default();
        if m.len() == 6 {
            ex.gs.ctm = Matrix::new(m[0], m[1], m[2], m[3], m[4], m[5]).then(&ex.gs.ctm);
        }
        ex.base_ctm = ex.gs.ctm;
        let bbox: Vec<f64> = self
            .doc
            .dict_get(dict, "BBox")
            .ok()
            .flatten()
            .and_then(|b| {
                b.as_array().map(|a| {
                    a.iter()
                        .filter_map(|o| self.doc.resolve(o).ok().and_then(|r| r.as_f64()))
                        .collect()
                })
            })
            .unwrap_or_default();
        if bbox.len() == 4 {
            let r = Rect::new(bbox[0], bbox[1], bbox[2], bbox[3]);
            let (w, h) = (self.target.width(), self.target.height());
            // Une boîte qui contient déjà toute la cible ne découpe rien : le
            // masque pleine page qu'elle produirait coûterait plus cher que
            // tout ce que le formulaire dessine.
            if !covers_target(r, ex.gs.ctm, w, h) {
                let mut p = Path::new();
                p.rect(&r);
                let mask = self
                    .raster
                    .path_coverage(&p, &ex.gs.ctm, FillRule::NonZero, w, h);
                ex.gs.intersect_clip(mask);
            }
        }
        let resources = self
            .doc
            .dict_get(dict, "Resources")
            .ok()
            .flatten()
            .and_then(|r| r.as_dict().cloned())
            .unwrap_or_default();
        let is_group = dict.contains_key(&Name::new("Group"));
        let needs_offscreen = !is_soft_mask_group
            && is_group
            && (ex.gs.fill_alpha < 1.0
                || ex.gs.blend != BlendMode::Normal
                || ex.gs.soft_mask.is_some());
        self.depth += 1;
        if needs_offscreen {
            // Groupe de transparence (§11.4) : rendu isolé puis composé avec
            // l'alpha, le mode de fusion et le masque souple du groupe.
            let (w, h) = (self.target.width(), self.target.height());
            // Comme pour les masques souples : le groupe ne peint que dans sa
            // `/BBox`, on ne rend donc hors écran que cette zone.
            let (x0, y0, bw, bh) = if bbox.len() == 4 {
                device_box(
                    Rect::new(bbox[0], bbox[1], bbox[2], bbox[3]),
                    ex.gs.ctm,
                    w,
                    h,
                )
            } else {
                (0, 0, w, h)
            };
            if bw == 0 || bh == 0 {
                self.depth -= 1;
                return;
            }
            let mut sub = Renderer::new(
                self.doc,
                bw,
                bh,
                RenderOptions {
                    annotations: false,
                    time_budget: self.options.time_budget,
                    background: None,
                    layers: self.options.layers.clone(),
                },
            );
            sub.depth = self.depth;
            sub.fonts.clone_from(&self.fonts);
            sub.hidden_oc.clone_from(&self.hidden_oc);
            let shift = Matrix::new(1.0, 0.0, 0.0, 1.0, -f64::from(x0), -f64::from(y0));
            let mut gs = ex.gs.clone();
            gs.fill_alpha = 1.0;
            gs.stroke_alpha = 1.0;
            gs.blend = BlendMode::Normal;
            gs.soft_mask = None;
            gs.ctm = gs.ctm.then(&shift);
            gs.clip = gs
                .clip
                .as_ref()
                .map(|c| Rc::new(c.sub_mask(x0, y0, bw, bh)));
            let mut inner = Exec::new(gs);
            inner.base_ctm = ex.base_ctm.then(&shift);
            sub.execute(&content, &resources, &mut inner);
            self.ops += sub.ops;
            let layer = Rc::new(sub.into_bitmap());
            let paint = Paint::Image(ImagePaint {
                bitmap: layer,
                transform: Matrix::new(
                    f64::from(bw),
                    0.0,
                    0.0,
                    f64::from(bh),
                    f64::from(x0),
                    f64::from(y0),
                ),
                interpolate: false,
            });
            let mut full = Path::new();
            full.rect(&Rect::new(
                f64::from(x0),
                f64::from(y0),
                f64::from(x0 + bw),
                f64::from(y0 + bh),
            ));
            let clip = ex.gs.effective_clip();
            #[allow(clippy::cast_possible_truncation)]
            let alpha = ex.gs.fill_alpha as f32;
            self.raster.fill_path(
                &mut self.target,
                &full,
                &Matrix::IDENTITY,
                FillRule::NonZero,
                &paint,
                clip.as_deref(),
                alpha,
                ex.gs.blend,
            );
        } else {
            self.execute(&content, &resources, ex);
        }
        self.depth -= 1;
    }

    fn draw_image_xobject(
        &mut self,
        obj: &Object,
        dict: &Dict,
        xref: Option<ObjectRef>,
        ex: &mut Exec,
        resources: &Dict,
    ) {
        let cached = xref.and_then(|r| self.images.get(&r.number).cloned());
        let (bitmap, is_stencil, stencil_mask) = if let Some(b) = cached {
            (Some(b), false, None)
        } else {
            let decoded = match self.doc.stream_data(obj) {
                Ok(d) => d,
                Err(e) => {
                    self.warn(format!("image illisible : {e}"));
                    return;
                }
            };
            let filter = decoded
                .image_filter
                .as_ref()
                .map(|(n, p)| (n.as_slice(), p));
            let img = match decode_image(self.doc, dict, &decoded.data, filter, Some(resources)) {
                Ok(i) => i,
                Err(e) => {
                    self.warn(format!("image : {e}"));
                    return;
                }
            };
            for w in &img.warnings {
                self.warn(format!("image : {w}"));
            }
            if img.is_stencil {
                (None, true, Some(img))
            } else {
                let b = Rc::new(bitmap_from_decoded(&img));
                if let Some(r) = xref {
                    if img.width as usize * img.height as usize <= 16 * 1024 * 1024 {
                        self.images.insert(r.number, Rc::clone(&b));
                    }
                }
                (Some(b), false, None)
            }
        };
        if is_stencil {
            if let Some(img) = stencil_mask {
                self.draw_stencil(&img, ex, resources);
            }
            return;
        }
        if let Some(b) = bitmap {
            let interpolate = matches!(
                dict.get(&Name::new("Interpolate")),
                Some(Object::Bool(true))
            );
            self.draw_bitmap(b, interpolate, ex);
        }
    }

    fn draw_inline_image(&mut self, img: &InlineImage, resources: &Dict, ex: &mut Exec) {
        // Filtres standard puis décodage comme un XObject.
        let resolve = |o: &Object| -> Object { o.clone() };
        let decoded = match acrux_document::filters::decode_stream(&img.dict, &img.data, &resolve) {
            Ok(d) => d,
            Err(e) => {
                self.warn(format!("image en ligne : {e}"));
                return;
            }
        };
        let filter = decoded
            .image_filter
            .as_ref()
            .map(|(n, p)| (n.as_slice(), p));
        let decoded_img =
            match decode_image(self.doc, &img.dict, &decoded.data, filter, Some(resources)) {
                Ok(i) => i,
                Err(e) => {
                    self.warn(format!("image en ligne : {e}"));
                    return;
                }
            };
        if decoded_img.is_stencil {
            self.draw_stencil(&decoded_img, ex, resources);
        } else {
            let interpolate = decoded_img.interpolate;
            let b = Rc::new(bitmap_from_decoded(&decoded_img));
            self.draw_bitmap(b, interpolate, ex);
        }
    }

    /// Dessine un bitmap sur le carré unité de l'espace utilisateur (§8.9.4).
    fn draw_bitmap(&mut self, bitmap: Rc<Bitmap>, interpolate: bool, ex: &mut Exec) {
        // Espace image (ligne 0 en haut) → carré unité PDF (origine en bas) → device.
        let flip = Matrix::new(1.0, 0.0, 0.0, -1.0, 0.0, 1.0);
        let transform = flip.then(&ex.gs.ctm);
        let paint = Paint::Image(ImagePaint {
            bitmap,
            transform,
            interpolate,
        });
        let mut unit = Path::new();
        unit.rect(&Rect::new(0.0, 0.0, 1.0, 1.0));
        let clip = ex.gs.effective_clip();
        #[allow(clippy::cast_possible_truncation)]
        let alpha = ex.gs.fill_alpha as f32;
        // Acrobat : une image plus petite qu'un pixel est tout de même dessinée (on laisse le rasteriseur gérer la couverture).
        self.raster.fill_path(
            &mut self.target,
            &unit,
            &ex.gs.ctm,
            FillRule::NonZero,
            &paint,
            clip.as_deref(),
            alpha,
            ex.gs.blend,
        );
    }

    /// Pochoir (`/ImageMask`) : la couleur de remplissage à travers le masque.
    fn draw_stencil(&mut self, img: &DecodedImage, ex: &mut Exec, resources: &Dict) {
        let Some(alpha) = &img.alpha else { return };
        // Bitmap de la couleur de remplissage avec l'alpha du pochoir.
        let rgb = ex.gs.fill.rgb();
        let n = img.width as usize * img.height as usize;
        let mut data = Vec::with_capacity(n * 4);
        for &a in alpha.iter().take(n) {
            let f = f32::from(a) / 255.0;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            data.extend_from_slice(&[
                (rgb[0] * f * 255.0 + 0.5) as u8,
                (rgb[1] * f * 255.0 + 0.5) as u8,
                (rgb[2] * f * 255.0 + 0.5) as u8,
                a,
            ]);
        }
        let Ok(bitmap) = Bitmap::from_premultiplied_rgba8(img.width, img.height, data) else {
            return;
        };
        if ex.gs.fill.space.is_pattern() {
            // Pochoir + motif : on découpe au pochoir puis on peint le motif sur le carré unité.
            let (w, h) = (self.target.width(), self.target.height());
            let flip = Matrix::new(1.0, 0.0, 0.0, -1.0, 0.0, 1.0);
            let paint = Paint::Image(ImagePaint {
                bitmap: Rc::new(bitmap),
                transform: flip.then(&ex.gs.ctm),
                interpolate: img.interpolate,
            });
            let mut layer = Bitmap::new(w, h);
            let mut unit = Path::new();
            unit.rect(&Rect::new(0.0, 0.0, 1.0, 1.0));
            self.raster.fill_path(
                &mut layer,
                &unit,
                &ex.gs.ctm,
                FillRule::NonZero,
                &paint,
                None,
                1.0,
                BlendMode::Normal,
            );
            let mut mask = Mask::new(w, h);
            mask.set_bounds((0, 0, w, h));
            for (d, px) in mask.data_mut().iter_mut().zip(layer.data().chunks_exact(4)) {
                *d = px[3];
            }
            let saved = ex.gs.clip.clone();
            ex.gs.intersect_clip(mask);
            let mut full = Path::new();
            full.rect(&Rect::new(0.0, 0.0, f64::from(w), f64::from(h)));
            self.fill_device_path(ex, resources, &full, FillRule::NonZero, false);
            ex.gs.clip = saved;
            return;
        }
        self.draw_bitmap(Rc::new(bitmap), img.interpolate, ex);
    }

    // ------------------------------------------------------------------
    // Contenu optionnel (§8.11)
    // ------------------------------------------------------------------

    fn is_hidden_oc(&self, dict: &Dict) -> bool {
        match dict.get(&Name::new("OC")) {
            Some(Object::Reference(r)) => self.oc_ref_hidden(*r),
            Some(Object::Dict(d)) => self.ocmd_hidden(d),
            _ => false,
        }
    }

    fn oc_ref_hidden(&self, r: ObjectRef) -> bool {
        if self.hidden_oc.contains(&r.number) {
            return true;
        }
        // OCMD indirect.
        if let Ok(o) = self.doc.get(r) {
            if let Some(d) = o.as_dict() {
                if d.get(&Name::new("Type")).and_then(Object::as_name) == Some(&Name::new("OCMD")) {
                    return self.ocmd_hidden(d);
                }
            }
        }
        false
    }

    /// Dictionnaire d'appartenance (§8.11.2.2) : `/OCGs` avec `/P` (AnyOn par défaut).
    fn ocmd_hidden(&self, d: &Dict) -> bool {
        let ocgs: Vec<u32> = match d.get(&Name::new("OCGs")) {
            Some(Object::Reference(r)) => vec![r.number],
            Some(Object::Array(a)) => a
                .iter()
                .filter_map(|o| match o {
                    Object::Reference(r) => Some(r.number),
                    _ => None,
                })
                .collect(),
            _ => return false,
        };
        if ocgs.is_empty() {
            return false;
        }
        let policy = d
            .get(&Name::new("P"))
            .and_then(Object::as_name)
            .map_or_else(|| "AnyOn".to_string(), Name::as_str);
        let on = |n: &u32| !self.hidden_oc.contains(n);
        let visible = match policy.as_str() {
            "AllOn" => ocgs.iter().all(on),
            "AnyOff" => ocgs.iter().any(|n| !on(n)),
            "AllOff" => ocgs.iter().all(|n| !on(n)),
            _ => ocgs.iter().any(on),
        };
        !visible
    }

    fn is_hidden_marked_content(&self, resources: &Dict, operands: &[Object]) -> bool {
        let Some(Object::Name(tag)) = operands.first() else {
            return false;
        };
        if tag.0 != b"OC" {
            return false;
        }
        match operands.get(1) {
            Some(Object::Name(prop)) => match self.resource(resources, "Properties", prop) {
                Some(Object::Reference(r)) => self.oc_ref_hidden(r),
                Some(Object::Dict(d)) => self.ocmd_hidden(&d),
                _ => false,
            },
            Some(Object::Dict(d)) => self.ocmd_hidden(d),
            _ => false,
        }
    }
}

/// Vrai si le rectangle `r`, transformé par `ctm`, contient toute la cible :
/// la découper serait alors sans effet. On exige une transformation sans
/// rotation ni cisaillement, sinon le contenant n'est pas un rectangle.
fn covers_target(r: Rect, ctm: Matrix, w: u32, h: u32) -> bool {
    if ctm.b.abs() > 1e-9 || ctm.c.abs() > 1e-9 {
        return false;
    }
    let (x0, y0, bw, bh) = device_box(r, ctm, w, h);
    x0 == 0 && y0 == 0 && bw >= w && bh >= h
}

/// Boîte de `r` transformée par `ctm`, en pixels entiers bornés à la cible,
/// avec un pixel de marge pour l'antialiasing des bords.
fn device_box(r: Rect, ctm: Matrix, w: u32, h: u32) -> (u32, u32, u32, u32) {
    let corners = [
        ctm.apply(Point::new(r.x0, r.y0)),
        ctm.apply(Point::new(r.x1, r.y0)),
        ctm.apply(Point::new(r.x0, r.y1)),
        ctm.apply(Point::new(r.x1, r.y1)),
    ];
    let (mut min_x, mut min_y) = (f64::MAX, f64::MAX);
    let (mut max_x, mut max_y) = (f64::MIN, f64::MIN);
    for p in corners {
        if !p.x.is_finite() || !p.y.is_finite() {
            return (0, 0, w, h);
        }
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    let x0 = (min_x.floor() - 1.0).clamp(0.0, f64::from(w));
    let y0 = (min_y.floor() - 1.0).clamp(0.0, f64::from(h));
    let x1 = (max_x.ceil() + 1.0).clamp(0.0, f64::from(w));
    let y1 = (max_y.ceil() + 1.0).clamp(0.0, f64::from(h));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        (
            x0 as u32,
            y0 as u32,
            (x1 - x0).max(0.0) as u32,
            (y1 - y0).max(0.0) as u32,
        )
    }
}

/// Boîte d'un formulaire en pixels du device : `/BBox` transformée par
/// `/Matrix` puis par la matrice courante, arrondie vers l'extérieur et
/// ramenée dans la cible. Sans `/BBox` lisible, c'est toute la cible.
fn form_device_box(
    doc: &Document,
    form: &Dict,
    ctm: Matrix,
    w: u32,
    h: u32,
) -> (u32, u32, u32, u32) {
    let numbers = |key: &str| -> Vec<f64> {
        doc.dict_get(form, key)
            .ok()
            .flatten()
            .and_then(|o| {
                o.as_array().map(|a| {
                    a.iter()
                        .filter_map(|o| doc.resolve(o).ok().and_then(|r| r.as_f64()))
                        .collect()
                })
            })
            .unwrap_or_default()
    };
    let bbox = numbers("BBox");
    if bbox.len() != 4 {
        return (0, 0, w, h);
    }
    let m = numbers("Matrix");
    let full = if m.len() == 6 {
        Matrix::new(m[0], m[1], m[2], m[3], m[4], m[5]).then(&ctm)
    } else {
        ctm
    };
    device_box(Rect::new(bbox[0], bbox[1], bbox[2], bbox[3]), full, w, h)
}

/// Valeur brute (0 à 255) que prend le masque là où seul le fond compte.
fn backdrop_index(backdrop: Color, luminosity: bool) -> usize {
    let rgba = backdrop.to_rgba8();
    if luminosity {
        let (r, g, b) = (f32::from(rgba[0]), f32::from(rgba[1]), f32::from(rgba[2]));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            (0.3 * r + 0.59 * g + 0.11 * b).round().clamp(0.0, 255.0) as usize
        }
    } else {
        usize::from(rgba[3])
    }
}

/// Même valeur, passée par la fonction de transfert.
fn backdrop_level(backdrop: Color, luminosity: bool, transfer: Option<&Function>) -> u8 {
    let raw = backdrop_index(backdrop, luminosity);
    #[allow(clippy::cast_precision_loss)]
    let v = raw as f64 / 255.0;
    let t = transfer.map_or(v, |f| f.eval(&[v]).first().copied().unwrap_or(v));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        (t.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    }
}

/// Groupes de contenu optionnel désactivés par la configuration par défaut
/// (`/OCProperties /D /OFF`, §8.11.4.3), plus ceux dont l'usage `/View` est OFF.
fn hidden_optional_content(doc: &Document, forced: &HashMap<u32, bool>) -> HashSet<u32> {
    let mut hidden = HashSet::new();
    let Ok(catalog) = doc.catalog() else {
        return hidden;
    };
    let Some(ocp) = doc.dict_get(&catalog, "OCProperties").ok().flatten() else {
        return hidden;
    };
    let Some(ocp) = ocp.as_dict() else {
        return hidden;
    };
    let Some(d) = doc.dict_get(ocp, "D").ok().flatten() else {
        return hidden;
    };
    let Some(d) = d.as_dict() else { return hidden };
    let base_off = matches!(
        doc.dict_get(d, "BaseState").ok().flatten().as_deref(),
        Some(Object::Name(n)) if n.0 == b"OFF"
    );
    if base_off {
        if let Some(all) = doc.dict_get(ocp, "OCGs").ok().flatten() {
            if let Some(a) = all.as_array() {
                for o in a {
                    if let Object::Reference(r) = o {
                        hidden.insert(r.number);
                    }
                }
            }
        }
        if let Some(on) = doc.dict_get(d, "ON").ok().flatten() {
            if let Some(a) = on.as_array() {
                for o in a {
                    if let Object::Reference(r) = o {
                        hidden.remove(&r.number);
                    }
                }
            }
        }
    }
    if let Some(off) = doc.dict_get(d, "OFF").ok().flatten() {
        if let Some(a) = off.as_array() {
            for o in a {
                if let Object::Reference(r) = o {
                    hidden.insert(r.number);
                }
            }
        }
    }
    // Le choix de l'utilisateur passe en dernier : il gagne toujours.
    for (&number, &visible) in forced {
        if visible {
            hidden.remove(&number);
        } else {
            hidden.insert(number);
        }
    }
    hidden
}

/// Un calque du document (groupe de contenu optionnel, §8.11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    /// Numéro d'objet du groupe : c'est la clé de `RenderOptions::layers`.
    pub number: u32,
    /// Nom affichable (`/Name`).
    pub name: String,
    /// Visible dans la configuration par défaut du document.
    pub visible: bool,
}

/// Nom `/Name` d'un groupe de contenu optionnel.
fn layer_name(doc: &Document, item: &Object) -> Option<String> {
    let resolved = doc.resolve(item).ok()?;
    let dict = resolved.as_dict()?.clone();
    let name = doc.dict_get(&dict, "Name").ok().flatten()?;
    match &*name {
        Object::String(bytes) => Some(acrux_document::text::decode_text_string(bytes)),
        _ => None,
    }
}

/// Calques du document, dans l'ordre de `/OCProperties /OCGs`.
///
/// L'ordre `/D /Order` d'Acrobat n'est pas repris : il décrit un arbre
/// d'affichage, pas la liste des groupes, et un groupe peut n'y pas figurer.
#[must_use]
pub fn layers(doc: &Document) -> Vec<Layer> {
    let mut out = Vec::new();
    let Ok(catalog) = doc.catalog() else {
        return out;
    };
    let Some(ocp) = doc.dict_get(&catalog, "OCProperties").ok().flatten() else {
        return out;
    };
    let Some(ocp) = ocp.as_dict() else { return out };
    let hidden = hidden_optional_content(doc, &HashMap::new());
    let Some(list) = doc.dict_get(ocp, "OCGs").ok().flatten() else {
        return out;
    };
    let Some(list) = list.as_array() else {
        return out;
    };
    for item in list {
        let Object::Reference(r) = item else { continue };
        let name = layer_name(doc, item).unwrap_or_else(|| format!("Calque {}", r.number));
        if out.iter().any(|l: &Layer| l.number == r.number) {
            continue;
        }
        out.push(Layer {
            number: r.number,
            name,
            visible: !hidden.contains(&r.number),
        });
    }
    out
}

/// Bitmap prémultiplié depuis une image décodée.
fn bitmap_from_decoded(img: &DecodedImage) -> Bitmap {
    let n = img.width as usize * img.height as usize;
    let mut data = Vec::with_capacity(n * 4);
    for i in 0..n {
        let r = img.rgb.get(i * 3).copied().unwrap_or(0);
        let g = img.rgb.get(i * 3 + 1).copied().unwrap_or(0);
        let b = img.rgb.get(i * 3 + 2).copied().unwrap_or(0);
        let a = img
            .alpha
            .as_ref()
            .map_or(255, |al| al.get(i).copied().unwrap_or(255));
        if a == 255 {
            data.extend_from_slice(&[r, g, b, 255]);
        } else {
            let f = u32::from(a);
            #[allow(clippy::cast_possible_truncation)]
            data.extend_from_slice(&[
                ((u32::from(r) * f + 127) / 255) as u8,
                ((u32::from(g) * f + 127) / 255) as u8,
                ((u32::from(b) * f + 127) / 255) as u8,
                a,
            ]);
        }
    }
    Bitmap::from_premultiplied_rgba8(img.width, img.height, data)
        .unwrap_or_else(|_| Bitmap::new(1, 1))
}

fn stroke_style(gs: &GraphicsState) -> StrokeStyle {
    StrokeStyle {
        width: gs.line_width,
        cap: gs.line_cap,
        join: gs.line_join,
        miter_limit: gs.miter_limit,
        dash: gs.dash.clone(),
    }
}

/// Opérateurs `G/g`, `RG/rg`, `K/k` : couleur de trait ou de remplissage dans
/// un espace de périphérique (§8.6.4). Ignoré dans un glyphe Type 3 `d1`.
fn set_device_color(ex: &mut Exec, stroke: bool, space: ColorSpace, nums: &[f64]) {
    if ex.type3_uncolored {
        return;
    }
    let cs = ColorState {
        components: nums.to_vec(),
        space,
        pattern: None,
    };
    if stroke {
        ex.gs.stroke = cs;
    } else {
        ex.gs.fill = cs;
    }
}

/// Opérateur `T*` (§9.4.2) : passe à la ligne suivante selon `TL`.
fn next_line(ex: &mut Exec) {
    ex.line_matrix = Matrix::translate(0.0, -ex.gs.text.leading).then(&ex.line_matrix);
    ex.text_matrix = ex.line_matrix;
}

fn line_cap(v: f64) -> LineCap {
    match v as i64 {
        1 => LineCap::Round,
        2 => LineCap::Square,
        _ => LineCap::Butt,
    }
}

fn line_join(v: f64) -> LineJoin {
    match v as i64 {
        1 => LineJoin::Round,
        2 => LineJoin::Bevel,
        _ => LineJoin::Miter,
    }
}

/// Opérandes de `d` : `[tableau phase]`. Un tableau vide ou tout à zéro = trait plein (§8.4.3.6).
fn dash_from(operands: &[Object]) -> Option<(Vec<f64>, f64)> {
    let arr = operands
        .iter()
        .find_map(|o| o.as_array().map(<[Object]>::to_vec))?;
    let pattern: Vec<f64> = arr
        .iter()
        .filter_map(Object::as_f64)
        .filter(|v| v.is_finite() && *v >= 0.0)
        .collect();
    if pattern.is_empty() || pattern.iter().all(|v| *v == 0.0) {
        return None;
    }
    let phase = operands
        .iter()
        .filter_map(Object::as_f64)
        .next_back()
        .unwrap_or(0.0);
    Some((pattern, phase))
}

fn blend_mode(name: &str) -> Option<BlendMode> {
    Some(match name {
        "Normal" | "Compatible" => BlendMode::Normal,
        "Multiply" => BlendMode::Multiply,
        "Screen" => BlendMode::Screen,
        "Overlay" => BlendMode::Overlay,
        "Darken" => BlendMode::Darken,
        "Lighten" => BlendMode::Lighten,
        "ColorDodge" => BlendMode::ColorDodge,
        "ColorBurn" => BlendMode::ColorBurn,
        "HardLight" => BlendMode::HardLight,
        "SoftLight" => BlendMode::SoftLight,
        "Difference" => BlendMode::Difference,
        "Exclusion" => BlendMode::Exclusion,
        "Hue" => BlendMode::Hue,
        "Saturation" => BlendMode::Saturation,
        "Color" => BlendMode::Color,
        "Luminosity" => BlendMode::Luminosity,
        _ => return None,
    })
}
