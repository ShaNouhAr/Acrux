//! Aperçu de sortie : séparation d'une page en plaques d'encre et calcul du
//! taux d'encre total.
//!
//! Deux sources, complémentaires :
//!
//! - les **quatre plaques de quadrichromie** viennent du rendu RVB de la page
//!   par `acrux-render` (donc images, ombrages et transparence compris),
//!   reconverti en CMJN avec un retrait de sous-couleur complet
//!   (`K = 1 − max(R, V, B)`, le comportement d'un aperçu de sortie sans
//!   profil de destination) ;
//! - les **tons directs** (`/Separation`, `/DeviceN`) viennent d'un parcours
//!   du flux de contenu : les tracés peints dans une encre nommée sont
//!   rasterisés dans leur propre plaque, avec leur teinte.
//!
//! Limite assumée : un ton direct est aussi rendu, donc compté, dans les
//! plaques de quadrichromie (le moteur le convertit par sa transformation de
//! teinte) ; sa plaque nommée sert à voir *où* l'encre se pose, pas à
//! remplacer les plaques de process. Le texte peint dans un ton direct est
//! rasterisé par la boîte de ses glyphes, pas par leur contour.

use std::collections::BTreeMap;

use acrux_core::{Error, Matrix, Path, Point, Rect, Result};
use acrux_document::{collect_pages, Dict, Document, Name, Object, Page};
use acrux_graphics::{path_coverage, stroke_path, FillRule, StrokeStyle};
use acrux_render::content::ContentLexer;
use acrux_render::font::LoadedFont;
use acrux_render::page::{base_matrix, page_content, page_pixel_size};
use acrux_render::{render_page, RenderOptions};

/// Budget d'opérateurs pour le parcours des tons directs.
const MAX_OPS: usize = 500_000;
/// Profondeur maximale de récursion dans les XObjects de formulaire.
const MAX_DEPTH: usize = 8;
/// Les quatre encres de quadrichromie, dans l'ordre des composantes CMJN.
const PROCESS: [&str; 4] = ["Cyan", "Magenta", "Yellow", "Black"];

/// Une plaque d'encre.
#[derive(Debug, Clone)]
pub struct Separation {
    /// Nom de l'encre : `Cyan`, `Magenta`, `Yellow`, `Black`, ou le nom du
    /// ton direct tel qu'il figure dans `/Separation`.
    pub name: String,
    /// Couverture par pixel, 0 = pas d'encre, 255 = aplat.
    pub coverage: Vec<u8>,
    /// Largeur en pixels.
    pub width: u32,
    /// Hauteur en pixels.
    pub height: u32,
}

impl Separation {
    /// Couverture moyenne de la plaque, en pourcentage.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn average(&self) -> f64 {
        if self.coverage.is_empty() {
            return 0.0;
        }
        let sum: u64 = self.coverage.iter().map(|&v| u64::from(v)).sum();
        sum as f64 / self.coverage.len() as f64 / 255.0 * 100.0
    }

    /// Vrai si la plaque ne porte aucune encre (plaque vide à ne pas graver).
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.coverage.iter().all(|&v| v == 0)
    }
}

/// Sépare une page en plaques d'encre à la résolution demandée.
///
/// # Errors
/// Page inexistante, ou document illisible.
pub fn separations(doc: &Document, page: usize, dpi: f64) -> Result<Vec<Separation>> {
    let pages = collect_pages(doc)?;
    let p = pages
        .get(page)
        .ok_or_else(|| Error::Corrupt(format!("page {} inexistante", page + 1)))?;
    let scale = (dpi / 72.0).clamp(0.05, 40.0);
    let rendered = render_page(
        doc,
        p,
        scale,
        &RenderOptions {
            annotations: false,
            ..RenderOptions::default()
        },
    );
    let (width, height) = (rendered.bitmap.width(), rendered.bitmap.height());
    let rgb = rendered.bitmap.to_rgb8_over_white();
    let pixels = (width as usize) * (height as usize);
    let mut plates = vec![vec![0_u8; pixels]; 4];
    for (i, px) in rgb.chunks_exact(3).enumerate().take(pixels) {
        let cmyk = rgb_to_cmyk(px[0], px[1], px[2]);
        for (plate, value) in plates.iter_mut().zip(cmyk) {
            plate[i] = value;
        }
    }
    let mut out: Vec<Separation> = PROCESS
        .iter()
        .zip(plates)
        .map(|(name, coverage)| Separation {
            name: (*name).to_string(),
            coverage,
            width,
            height,
        })
        .collect();
    // Le rendu passe par le RVB : une encre posée en CMJN y perd son taux
    // (un noir de soutien 100/100/100/100 redevient du noir seul). Le parcours
    // du contenu rattrape ces plaques en lisant les composantes d'origine.
    for plate in spot_plates(doc, p, scale, width, height) {
        match out.iter_mut().find(|o| o.name == plate.name) {
            Some(existing) => {
                for (value, direct) in existing.coverage.iter_mut().zip(&plate.coverage) {
                    *value = (*value).max(*direct);
                }
            }
            None => out.push(plate),
        }
    }
    Ok(out)
}

/// Taux d'encre total maximal de la page, en pourcentage (somme des quatre
/// plaques au point le plus chargé). Au-delà de 300 %, les encres ne sèchent
/// pas : c'est la limite usuelle d'un papier couché.
///
/// # Errors
/// Page inexistante, ou document illisible.
pub fn ink_coverage(doc: &Document, page: usize) -> Result<f64> {
    // 72 dpi suffit : on cherche un maximum sur des aplats, pas un détail fin.
    let plates = separations(doc, page, 72.0)?;
    let process: Vec<&Separation> = plates
        .iter()
        .filter(|p| matches!(p.name.as_str(), "Cyan" | "Magenta" | "Yellow" | "Black"))
        .collect();
    let Some(first) = process.first() else {
        return Ok(0.0);
    };
    let mut worst = 0_u32;
    for i in 0..first.coverage.len() {
        let total: u32 = process
            .iter()
            .map(|p| u32::from(p.coverage.get(i).copied().unwrap_or(0)))
            .sum();
        worst = worst.max(total);
    }
    Ok(f64::from(worst) / 255.0 * 100.0)
}

/// RVB → CMJN avec retrait de sous-couleur complet (§10.4 : la conversion
/// d'un aperçu sans profil de destination).
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn rgb_to_cmyk(r: u8, g: u8, b: u8) -> [u8; 4] {
    let (r, g, b) = (
        f64::from(r) / 255.0,
        f64::from(g) / 255.0,
        f64::from(b) / 255.0,
    );
    let k = 1.0 - r.max(g).max(b);
    if k >= 1.0 - 1e-9 {
        return [0, 0, 0, 255];
    }
    let denominator = 1.0 - k;
    let convert = |v: f64| (((1.0 - v - k) / denominator).clamp(0.0, 1.0) * 255.0).round() as u8;
    [
        convert(r),
        convert(g),
        convert(b),
        (k.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

// ---------------------------------------------------------------------------
// Tons directs
// ---------------------------------------------------------------------------

/// Encre nommée employée par l'état graphique courant, avec sa teinte.
#[derive(Debug, Clone, Default)]
struct Ink {
    names: Vec<String>,
    tints: Vec<f64>,
}

impl Ink {
    fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

/// Encre de quadrichromie posée par `k` ou `K`.
fn process_ink(components: &[f64]) -> Ink {
    if components.len() < 4 {
        return Ink::default();
    }
    Ink {
        names: PROCESS.iter().map(|n| (*n).to_string()).collect(),
        tints: components[..4].to_vec(),
    }
}

struct SpotScanner<'a> {
    doc: &'a Document,
    base: Matrix,
    width: u32,
    height: u32,
    plates: BTreeMap<String, Vec<f32>>,
    ops: usize,
    depth: usize,
}

/// Plaques des tons directs employés par la page.
fn spot_plates(
    doc: &Document,
    page: &Page,
    scale: f64,
    width: u32,
    height: u32,
) -> Vec<Separation> {
    let bounds = page.crop_box(doc);
    let base = base_matrix(&bounds, scale, page.rotate(doc), width, height);
    let resources = doc
        .dict_get(&page.dict, "Resources")
        .ok()
        .flatten()
        .and_then(|r| r.as_dict().cloned())
        .unwrap_or_default();
    let mut scanner = SpotScanner {
        doc,
        base,
        width,
        height,
        plates: BTreeMap::new(),
        ops: 0,
        depth: 0,
    };
    let content = page_content(doc, page);
    scanner.run(&content, &resources, Matrix::IDENTITY);
    scanner
        .plates
        .into_iter()
        .map(|(name, values)| Separation {
            name,
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            coverage: values
                .iter()
                .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
                .collect(),
            width,
            height,
        })
        .collect()
}

impl SpotScanner<'_> {
    #[allow(clippy::too_many_lines)] // un bras par opérateur
    fn run(&mut self, content: &[u8], resources: &Dict, initial_ctm: Matrix) {
        let mut ctm = initial_ctm;
        let mut fill = Ink::default();
        let mut stroke = Ink::default();
        let mut line_width = 1.0_f64;
        let mut path = Path::new();
        let mut current = Point::new(0.0, 0.0);
        let mut start = current;
        let mut stack: Vec<(Matrix, Ink, Ink, f64)> = Vec::new();
        let mut text: TextState = TextState::default();
        let mut lexer = ContentLexer::new(content);
        while let Ok(Some(op)) = lexer.next_operation() {
            self.ops += 1;
            if self.ops > MAX_OPS {
                return;
            }
            let nums: Vec<f64> = op.operands.iter().filter_map(Object::as_f64).collect();
            let n = |i: usize| nums.get(i).copied().unwrap_or(0.0);
            match op.operator.as_slice() {
                b"q" => stack.push((ctm, fill.clone(), stroke.clone(), line_width)),
                b"Q" => {
                    if let Some((c, f, s, w)) = stack.pop() {
                        ctm = c;
                        fill = f;
                        stroke = s;
                        line_width = w;
                    }
                }
                b"cm" if nums.len() >= 6 => {
                    ctm = Matrix::new(n(0), n(1), n(2), n(3), n(4), n(5)).then(&ctm);
                }
                b"w" => line_width = n(0),
                b"g" | b"rg" => fill = Ink::default(),
                b"G" | b"RG" => stroke = Ink::default(),
                // `k` et `K` posent directement les quatre encres.
                b"k" => fill = process_ink(&nums),
                b"K" => stroke = process_ink(&nums),
                b"cs" | b"CS" => {
                    let names = op
                        .operands
                        .first()
                        .and_then(Object::as_name)
                        .map(|name| self.ink_names(resources, name))
                        .unwrap_or_default();
                    let ink = Ink {
                        names,
                        tints: Vec::new(),
                    };
                    if op.operator[0] == b'c' {
                        fill = ink;
                    } else {
                        stroke = ink;
                    }
                }
                b"sc" | b"scn" => fill.tints.clone_from(&nums),
                b"SC" | b"SCN" => stroke.tints.clone_from(&nums),
                b"m" if nums.len() >= 2 => {
                    current = ctm.apply(Point::new(n(0), n(1)));
                    start = current;
                    path.move_to(current);
                }
                b"l" if nums.len() >= 2 => {
                    current = ctm.apply(Point::new(n(0), n(1)));
                    path.line_to(current);
                }
                b"c" if nums.len() >= 6 => {
                    let c1 = ctm.apply(Point::new(n(0), n(1)));
                    let c2 = ctm.apply(Point::new(n(2), n(3)));
                    current = ctm.apply(Point::new(n(4), n(5)));
                    path.curve_to(c1, c2, current);
                }
                b"v" if nums.len() >= 4 => {
                    let c2 = ctm.apply(Point::new(n(0), n(1)));
                    let end = ctm.apply(Point::new(n(2), n(3)));
                    path.curve_to(current, c2, end);
                    current = end;
                }
                b"y" if nums.len() >= 4 => {
                    let c1 = ctm.apply(Point::new(n(0), n(1)));
                    let end = ctm.apply(Point::new(n(2), n(3)));
                    path.curve_to(c1, end, end);
                    current = end;
                }
                b"h" => {
                    path.close();
                    current = start;
                }
                b"re" if nums.len() >= 4 => {
                    let corners = [
                        ctm.apply(Point::new(n(0), n(1))),
                        ctm.apply(Point::new(n(0) + n(2), n(1))),
                        ctm.apply(Point::new(n(0) + n(2), n(1) + n(3))),
                        ctm.apply(Point::new(n(0), n(1) + n(3))),
                    ];
                    path.move_to(corners[0]);
                    for c in &corners[1..] {
                        path.line_to(*c);
                    }
                    path.close();
                    current = corners[0];
                    start = corners[0];
                }
                b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" => {
                    let rule = if op.operator.ends_with(b"*") {
                        FillRule::EvenOdd
                    } else {
                        FillRule::NonZero
                    };
                    self.paint(&path, &fill, rule);
                    if op.operator[0] == b'B' || op.operator[0] == b'b' {
                        self.paint_stroke(&path, &stroke, line_width, ctm);
                    }
                    path = Path::new();
                }
                b"S" | b"s" => {
                    self.paint_stroke(&path, &stroke, line_width, ctm);
                    path = Path::new();
                }
                b"n" => path = Path::new(),
                b"BT" => {
                    text.matrix = Matrix::IDENTITY;
                    text.line_matrix = Matrix::IDENTITY;
                }
                b"Tf" => {
                    text.size = n(0);
                    text.font = op
                        .operands
                        .first()
                        .and_then(Object::as_name)
                        .and_then(|name| self.load_font(resources, name));
                }
                b"TL" => text.leading = n(0),
                b"Tz" => text.hscale = n(0) / 100.0,
                b"Tc" => text.char_spacing = n(0),
                b"Tw" => text.word_spacing = n(0),
                b"Td" | b"TD" => {
                    if op.operator == b"TD" {
                        text.leading = -n(1);
                    }
                    text.line_matrix = Matrix::translate(n(0), n(1)).then(&text.line_matrix);
                    text.matrix = text.line_matrix;
                }
                b"Tm" if nums.len() >= 6 => {
                    text.line_matrix = Matrix::new(n(0), n(1), n(2), n(3), n(4), n(5));
                    text.matrix = text.line_matrix;
                }
                b"T*" => {
                    text.line_matrix =
                        Matrix::translate(0.0, -text.leading).then(&text.line_matrix);
                    text.matrix = text.line_matrix;
                }
                b"Tj" | b"'" | b"\"" | b"TJ" => {
                    if fill.is_empty() {
                        // Les glyphes suivent quand même l'avance du texte.
                        self.advance_text(&mut text, &op, ctm, None);
                    } else {
                        let ink = fill.clone();
                        self.advance_text(&mut text, &op, ctm, Some(&ink));
                    }
                }
                b"Do" => {
                    if let Some(name) = op.operands.first().and_then(Object::as_name) {
                        self.do_form(resources, name, ctm);
                    }
                }
                _ => {}
            }
        }
    }

    /// Noms d'encre d'un espace colorimétrique nommé, vide s'il n'y en a pas.
    fn ink_names(&self, resources: &Dict, name: &Name) -> Vec<String> {
        if name.0 == b"DeviceCMYK" {
            return PROCESS.iter().map(|n| (*n).to_string()).collect();
        }
        let Some(entry) = self
            .doc
            .dict_get(resources, "ColorSpace")
            .ok()
            .flatten()
            .and_then(|d| d.as_dict().and_then(|d| d.get(name).cloned()))
        else {
            return Vec::new();
        };
        let Ok(resolved) = self.doc.resolve(&entry) else {
            return Vec::new();
        };
        let Some(items) = resolved.as_array() else {
            return Vec::new();
        };
        match items.first().and_then(Object::as_name).map(Name::as_str) {
            Some(family) if family == "Separation" => items
                .get(1)
                .and_then(Object::as_name)
                .map(|n| vec![n.as_str()])
                .unwrap_or_default(),
            Some(family) if family == "DeviceN" => items
                .get(1)
                .and_then(Object::as_array)
                .map(|names| {
                    names
                        .iter()
                        .filter_map(|o| o.as_name().map(Name::as_str))
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn plate(&mut self, name: &str) -> &mut Vec<f32> {
        let pixels = (self.width as usize) * (self.height as usize);
        self.plates
            .entry(name.to_string())
            .or_insert_with(|| vec![0.0; pixels])
    }

    /// Ajoute la couverture d'un chemin aux plaques des encres de `ink`.
    fn paint(&mut self, path: &Path, ink: &Ink, rule: FillRule) {
        if ink.is_empty() || path.is_empty() {
            return;
        }
        let mask = path_coverage(path, &self.base, rule, self.width, self.height);
        for (i, name) in ink.names.clone().into_iter().enumerate() {
            let tint = ink.tints.get(i).copied().unwrap_or(1.0).clamp(0.0, 1.0);
            if tint <= 0.0 {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            let tint = tint as f32;
            let plate = self.plate(&name);
            for (value, coverage) in plate.iter_mut().zip(mask.data()) {
                let added = f32::from(*coverage) / 255.0 * tint;
                *value = value.max(added);
            }
        }
    }

    fn paint_stroke(&mut self, path: &Path, ink: &Ink, line_width: f64, ctm: Matrix) {
        if ink.is_empty() || path.is_empty() {
            return;
        }
        let scale = f64::midpoint(ctm.a.hypot(ctm.b), ctm.c.hypot(ctm.d)).max(1e-6);
        let style = StrokeStyle {
            width: (line_width * scale).max(0.2),
            ..StrokeStyle::default()
        };
        let outline = stroke_path(path, &style, &self.base);
        self.paint(&outline, ink, FillRule::NonZero);
    }

    fn load_font(&self, resources: &Dict, name: &Name) -> Option<std::rc::Rc<LoadedFont>> {
        let entry = self
            .doc
            .dict_get(resources, "Font")
            .ok()
            .flatten()
            .and_then(|f| f.as_dict().and_then(|d| d.get(name).cloned()))?;
        let dict = self.doc.resolve(&entry).ok()?.as_dict().cloned()?;
        LoadedFont::load(self.doc, &dict).ok().map(std::rc::Rc::new)
    }

    /// Fait avancer la matrice de texte et, si une encre nommée est active,
    /// rasterise la boîte de chaque glyphe.
    fn advance_text(
        &mut self,
        text: &mut TextState,
        op: &acrux_render::content::Operation,
        ctm: Matrix,
        ink: Option<&Ink>,
    ) {
        let strings: Vec<Object> = match op.operator.as_slice() {
            b"TJ" => match op.operands.last() {
                Some(Object::Array(items)) => items.clone(),
                _ => Vec::new(),
            },
            b"\"" => op.operands.get(2).cloned().into_iter().collect(),
            _ => op
                .operands
                .last()
                .filter(|o| matches!(o, Object::String(_)))
                .cloned()
                .into_iter()
                .collect(),
        };
        if matches!(op.operator.as_slice(), b"'" | b"\"") {
            text.line_matrix = Matrix::translate(0.0, -text.leading).then(&text.line_matrix);
            text.matrix = text.line_matrix;
        }
        let Some(font) = text.font.clone() else {
            return;
        };
        let mut boxes = Path::new();
        for item in &strings {
            match item {
                Object::String(bytes) => {
                    for g in font.decode(bytes) {
                        let trm =
                            Matrix::new(text.size * text.hscale, 0.0, 0.0, text.size, 0.0, 0.0)
                                .then(&text.matrix)
                                .then(&ctm);
                        if ink.is_some() && !g.is_space {
                            let p0 = trm.apply(Point::new(0.0, -0.2));
                            let p1 = trm.apply(Point::new(g.width.max(0.0), 0.75));
                            boxes.rect(&Rect::new(p0.x, p0.y, p1.x, p1.y));
                        }
                        let mut tx = g.width * text.size + text.char_spacing;
                        if g.is_space {
                            tx += text.word_spacing;
                        }
                        tx *= text.hscale;
                        text.matrix = Matrix::translate(tx, 0.0).then(&text.matrix);
                    }
                }
                other => {
                    if let Some(adj) = other.as_f64() {
                        let tx = -adj / 1000.0 * text.size * text.hscale;
                        text.matrix = Matrix::translate(tx, 0.0).then(&text.matrix);
                    }
                }
            }
        }
        if let Some(ink) = ink {
            self.paint(&boxes, ink, FillRule::NonZero);
        }
    }

    fn do_form(&mut self, resources: &Dict, name: &Name, ctm: Matrix) {
        if self.depth >= MAX_DEPTH {
            return;
        }
        let Some(entry) = self
            .doc
            .dict_get(resources, "XObject")
            .ok()
            .flatten()
            .and_then(|x| x.as_dict().and_then(|d| d.get(name).cloned()))
        else {
            return;
        };
        let Ok(obj) = self.doc.resolve(&entry) else {
            return;
        };
        let Some(dict) = obj.as_dict().cloned() else {
            return;
        };
        let is_form = dict
            .get(&Name::new("Subtype"))
            .and_then(Object::as_name)
            .is_some_and(|n| n.0 == b"Form");
        if !is_form {
            return;
        }
        let Ok(data) = self.doc.stream_data(&obj) else {
            return;
        };
        let matrix = self
            .doc
            .dict_get(&dict, "Matrix")
            .ok()
            .flatten()
            .and_then(|m| {
                let v: Vec<f64> = m.as_array()?.iter().filter_map(Object::as_f64).collect();
                (v.len() == 6).then(|| Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5]))
            })
            .unwrap_or(Matrix::IDENTITY);
        let inner = self
            .doc
            .dict_get(&dict, "Resources")
            .ok()
            .flatten()
            .and_then(|r| r.as_dict().cloned())
            .unwrap_or_else(|| resources.clone());
        self.depth += 1;
        self.run(&data.data, &inner, matrix.then(&ctm));
        self.depth -= 1;
    }
}

/// État de texte minimal, suffisant pour placer les boîtes des glyphes.
struct TextState {
    matrix: Matrix,
    line_matrix: Matrix,
    font: Option<std::rc::Rc<LoadedFont>>,
    size: f64,
    leading: f64,
    hscale: f64,
    char_spacing: f64,
    word_spacing: f64,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            matrix: Matrix::IDENTITY,
            line_matrix: Matrix::IDENTITY,
            font: None,
            size: 0.0,
            leading: 0.0,
            hscale: 1.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
        }
    }
}

/// Dimensions en pixels d'une page, pour dimensionner l'aperçu avant rendu.
///
/// # Errors
/// Page inexistante.
pub fn preview_size(doc: &Document, page: usize, dpi: f64) -> Result<(u32, u32)> {
    let pages = collect_pages(doc)?;
    let p = pages
        .get(page)
        .ok_or_else(|| Error::Corrupt(format!("page {} inexistante", page + 1)))?;
    Ok(page_pixel_size(doc, p, (dpi / 72.0).clamp(0.05, 40.0)))
}
