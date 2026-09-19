//! Parcours d'un flux de contenu du point de vue du **balisage**
//! (ISO 32000-2 §14.6) : on ne cherche pas à dessiner la page mais à savoir
//! *quel contenu porte quelle marque*, et *où il se trouve dans les octets du
//! flux* pour pouvoir le re-baliser sans rien déplacer d'autre.
//!
//! Chaque opérateur de texte, chaque image et chaque tracé peint est rattaché
//! à la marque de contenu ouverte la plus proche : `BDC /P <</MCID n>> … EMC`
//! donne le numéro `n` (le lien avec l'arbre de structure, §14.7.4.2) et
//! `BMC /Artifact` ou `BDC /Artifact …` signale un contenu hors du texte
//! (filets, numéros de page, décor, §14.8.2.2). Ce qui n'est ni l'un ni
//! l'autre est du **contenu non balisé**, que le vérificateur PDF/UA signale.
//!
//! Les aplats sont conservés avec leur couleur : ce sont eux qui servent de
//! fond au calcul du contraste (WCAG 2.1 §1.4.3).

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use acrux_core::{Matrix, Point, Rect, Result};
use acrux_document::{Dict, Document, Name, Object, Page};
use acrux_render::content::{ContentLexer, Operation};
use acrux_render::font::LoadedFont;
use acrux_render::page::page_content;

/// Budget d'opérateurs (fichiers hostiles).
const MAX_OPS: usize = 2_000_000;
/// Nombre maximal d'éléments conservés par catégorie.
const MAX_ITEMS: usize = 100_000;
/// Profondeur maximale de récursion dans les XObjects de formulaire.
const MAX_DEPTH: usize = 12;

/// Marque de contenu ouverte (`BMC`/`BDC` … `EMC`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Mark {
    /// `/MCID` de la propriété du `BDC`, s'il y en a un.
    pub mcid: Option<i64>,
    /// La marque (ou une marque englobante) est un `/Artifact`.
    pub artifact: bool,
}

/// Nature d'une tranche d'octets repérée dans le flux de la page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    /// Un opérateur de texte (`Tj`, `TJ`, `'`, `"`) ou un XObject de
    /// formulaire qui contient du texte : du contenu à baliser.
    Text,
    /// Une image (`Do` sur un XObject `/Image`, ou image en ligne).
    Image,
    /// Un tracé peint (remplissage, contour, ombrage) : décor, sauf preuve
    /// du contraire, donc un `/Artifact`.
    Paint,
    /// Un opérateur qui interdit d'englober ce qui le précède et ce qui le
    /// suit dans une même marque (`q`, `Q`, `BT`, `ET`, `BDC /OC`, son `EMC`).
    Structural,
    /// Un opérateur de marque à supprimer lors d'un re-balisage (`BMC`/`BDC`
    /// autres que `/OC`, et leurs `EMC`).
    Drop,
}

/// Une tranche d'octets du flux de contenu de la page.
#[derive(Debug, Clone)]
pub struct Span {
    /// Premier octet (espacement précédant l'opérateur compris).
    pub start: usize,
    /// Octet suivant le dernier de l'opérateur.
    pub end: usize,
    /// Nature.
    pub kind: SpanKind,
    /// Boîte englobante du contenu peint, en espace utilisateur.
    pub bbox: Option<Rect>,
}

/// Une suite de glyphes montrée par un seul opérateur (`Tj`, `TJ`, `'`, `"`).
#[derive(Debug, Clone)]
pub struct TextRun {
    /// Texte Unicode obtenu par `/ToUnicode` ou l'encodage.
    pub text: String,
    /// Boîte englobante en espace utilisateur.
    pub bbox: Rect,
    /// `/MCID` de la marque courante.
    pub mcid: Option<i64>,
    /// Le texte est dans un `/Artifact`.
    pub artifact: bool,
    /// Taille de police effective en points.
    pub size: f64,
    /// Police grasse (nom ou descripteur).
    pub bold: bool,
    /// Couleur de remplissage (ou de contour pour les modes 1 et 5), RVB.
    pub color: [f32; 3],
    /// Rang de l'opérateur dans le flux (ordre de dessin).
    pub order: usize,
}

/// Une image dessinée (`Do` sur un XObject `/Image`, ou image en ligne).
#[derive(Debug, Clone)]
pub struct ImageDraw {
    /// Boîte englobante en espace utilisateur (le carré unité transformé).
    pub bbox: Rect,
    /// `/MCID` de la marque courante.
    pub mcid: Option<i64>,
    /// L'image est un `/Artifact` (décor).
    pub artifact: bool,
    /// Nom de la ressource (`/Im0`), vide pour une image en ligne.
    pub name: String,
}

/// Un aplat : rectangle rempli d'une couleur unie. Sert de fond au contraste.
#[derive(Debug, Clone, Copy)]
pub struct FillRect {
    /// Boîte remplie.
    pub bbox: Rect,
    /// Couleur RVB.
    pub color: [f32; 3],
    /// Rang dans le flux (ordre de dessin : le dernier dessiné est au-dessus).
    pub order: usize,
}

/// Tout ce qu'une page porte, vu du balisage.
#[derive(Debug, Clone, Default)]
pub struct PageMarks {
    /// Textes montrés, dans l'ordre du flux.
    pub runs: Vec<TextRun>,
    /// Images dessinées.
    pub images: Vec<ImageDraw>,
    /// Aplats (fonds).
    pub fills: Vec<FillRect>,
    /// Tranches d'octets du flux de la page (profondeur 0 uniquement),
    /// dans l'ordre : de quoi réécrire le flux en n'y insérant que des marques.
    pub spans: Vec<Span>,
    /// Plus grand `/MCID` rencontré.
    pub max_mcid: Option<i64>,
    /// Le flux contient au moins un opérateur de marque (`BMC`/`BDC`).
    pub has_marks: bool,
    /// Par police (`/BaseFont`) : nombre de glyphes montrés et nombre de
    /// glyphes sans équivalent Unicode. Une police dont tous les glyphes sont
    /// sans équivalent rend son texte inextractible (§9.10.2).
    pub glyphs_by_font: BTreeMap<String, (usize, usize)>,
}

impl PageMarks {
    /// Texte couvert par un `/MCID`.
    #[must_use]
    pub fn text_of(&self, mcid: i64) -> String {
        let mut out = String::new();
        for r in self.runs.iter().filter(|r| r.mcid == Some(mcid)) {
            out.push_str(&r.text);
        }
        out
    }

    /// Boîte englobante du contenu couvert par un `/MCID` (texte et images).
    #[must_use]
    pub fn bbox_of(&self, mcid: i64) -> Option<Rect> {
        let text = self
            .runs
            .iter()
            .filter(|r| r.mcid == Some(mcid))
            .map(|r| r.bbox);
        let images = self
            .images
            .iter()
            .filter(|i| i.mcid == Some(mcid))
            .map(|i| i.bbox);
        text.chain(images).reduce(|a, b| a.union(&b))
    }

    /// Ordre géométrique des `/MCID` de la page : de haut en bas, puis de
    /// gauche à droite (approximation d'un ordre de lecture occidental).
    #[must_use]
    pub fn geometric_order(&self) -> Vec<i64> {
        let mut boxes: HashMap<i64, Rect> = HashMap::new();
        let mut add = |id: i64, b: Rect| {
            boxes
                .entry(id)
                .and_modify(|r| *r = r.union(&b))
                .or_insert(b);
        };
        for r in &self.runs {
            if let Some(id) = r.mcid {
                add(id, r.bbox);
            }
        }
        for i in &self.images {
            if let Some(id) = i.mcid {
                add(id, i.bbox);
            }
        }
        let mut items: Vec<(i64, Rect)> = boxes.into_iter().collect();
        items.sort_by(|a, b| {
            // Deux boîtes dont les sommets sont proches sont « sur la même
            // ligne » : on les ordonne alors de gauche à droite.
            let tolerance = a.1.height().min(b.1.height()) * 0.6;
            if (a.1.y1 - b.1.y1).abs() <= tolerance {
                a.1.x0
                    .partial_cmp(&b.1.x0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            } else {
                b.1.y1
                    .partial_cmp(&a.1.y1)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }
        });
        items.into_iter().map(|(id, _)| id).collect()
    }

    /// Couleur de fond dominante derrière une boîte : le dernier aplat
    /// dessiné avant `order` qui la contient, blanc à défaut (le support du
    /// papier, §8.6.8).
    #[must_use]
    pub fn background_behind(&self, bbox: &Rect, order: usize) -> [f32; 3] {
        let mut result = [1.0_f32; 3];
        for f in &self.fills {
            if f.order >= order {
                break;
            }
            if f.bbox.x0 <= bbox.x0 + 0.5
                && f.bbox.y0 <= bbox.y0 + 0.5
                && f.bbox.x1 >= bbox.x1 - 0.5
                && f.bbox.y1 >= bbox.y1 - 0.5
            {
                result = f.color;
            }
        }
        result
    }
}

/// Police chargée une fois par ressource.
struct FontInfo {
    font: Rc<LoadedFont>,
    bold: bool,
    /// Nom affiché dans les messages, préfixe de sous-ensemble retiré.
    name: String,
}

/// État graphique et texte retenu pour le balisage.
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
    fill_components: usize,
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
        }
    }
}

#[derive(Clone)]
struct Saved {
    ctm: Matrix,
    font: Option<Rc<FontInfo>>,
    size: f64,
    fill: [f32; 3],
    stroke: [f32; 3],
    fill_components: usize,
}

/// Convertit des composantes (gris, RVB, CMJN) en RVB.
fn to_rgb(components: &[f64]) -> Option<[f32; 3]> {
    #[allow(clippy::cast_possible_truncation)]
    let unit = |v: f64| v.clamp(0.0, 1.0) as f32;
    match components {
        [gray] => Some([unit(*gray); 3]),
        [r, g, b] => Some([unit(*r), unit(*g), unit(*b)]),
        [c, m, y, k] => Some([
            unit((1.0 - c) * (1.0 - k)),
            unit((1.0 - m) * (1.0 - k)),
            unit((1.0 - y) * (1.0 - k)),
        ]),
        _ => None,
    }
}

/// `sc`/`scn` : une seule composante dans un espace à une teinte est une
/// **teinte** (0 = pas d'encre = blanc), contrairement au gris.
fn tint_to_rgb(values: &[f64], components: usize) -> Option<[f32; 3]> {
    if values.len() == 1 && components == 1 {
        #[allow(clippy::cast_possible_truncation)]
        let v = (1.0 - values[0].clamp(0.0, 1.0)) as f32;
        return Some([v; 3]);
    }
    to_rgb(values)
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

/// Gras déduit du nom de police (suffisant pour le seuil « grand texte »).
fn looks_bold(base_font: &str) -> bool {
    let lower = base_font.to_ascii_lowercase();
    ["bold", "black", "heavy", "semibold", "demibold"]
        .iter()
        .any(|k| lower.contains(k))
}

struct Scanner<'a> {
    doc: &'a Document,
    out: PageMarks,
    fonts: HashMap<String, Rc<FontInfo>>,
    marks: Vec<Mark>,
    /// Pour chaque marque ouverte : faut-il supprimer ses octets au re-balisage ?
    drops: Vec<bool>,
    order: usize,
    ops: usize,
    depth: usize,
    /// Rectangles du chemin en cours, déjà en espace utilisateur.
    rects: Vec<Rect>,
    /// Boîte de tous les points du chemin en cours.
    path_bbox: Option<Rect>,
    /// Position du premier octet du chemin en cours.
    path_start: Option<usize>,
}

/// Parcourt une page et rend son contenu vu du balisage.
///
/// # Errors
/// Ressources illisibles (un contenu vide n'est pas une erreur).
pub fn scan_page(doc: &Document, page: &Page) -> Result<PageMarks> {
    let resources = doc
        .dict_get(&page.dict, "Resources")
        .ok()
        .flatten()
        .and_then(|r| r.as_dict().cloned())
        .unwrap_or_default();
    let content = page_content(doc, page);
    let mut scanner = Scanner {
        doc,
        out: PageMarks::default(),
        fonts: HashMap::new(),
        marks: Vec::new(),
        drops: Vec::new(),
        order: 0,
        ops: 0,
        depth: 0,
        rects: Vec::new(),
        path_bbox: None,
        path_start: None,
    };
    scanner.run(&content, &resources, Matrix::IDENTITY);
    Ok(scanner.out)
}

impl Scanner<'_> {
    /// Marque courante (la plus intérieure qui porte un `/MCID`, et le
    /// drapeau artéfact hérité de n'importe quel niveau).
    fn current(&self) -> Mark {
        let mcid = self.marks.iter().rev().find_map(|m| m.mcid);
        let artifact = self.marks.iter().any(|m| m.artifact);
        Mark { mcid, artifact }
    }

    fn span(&mut self, start: usize, end: usize, kind: SpanKind, bbox: Option<Rect>) {
        if self.depth == 0 && self.out.spans.len() < MAX_ITEMS {
            self.out.spans.push(Span {
                start,
                end,
                kind,
                bbox,
            });
        }
    }

    fn font(&mut self, resources: &Dict, name: &Name) -> Option<Rc<FontInfo>> {
        let entry = self
            .doc
            .dict_get(resources, "Font")
            .ok()
            .flatten()
            .and_then(|f| f.as_dict().and_then(|d| d.get(name).cloned()))?;
        let key = match &entry {
            Object::Reference(r) => format!("ref:{}", r.number),
            o => format!("direct:{}", acrux_document::writer::to_string(o)),
        };
        if let Some(f) = self.fonts.get(&key) {
            return Some(Rc::clone(f));
        }
        let dict = self.doc.resolve(&entry).ok()?.as_dict().cloned()?;
        let font = Rc::new(LoadedFont::load(self.doc, &dict).ok()?);
        let bold = looks_bold(&font.base_font);
        let name = strip_subset_prefix(&font.base_font).to_string();
        let info = Rc::new(FontInfo { font, bold, name });
        self.fonts.insert(key, Rc::clone(&info));
        Some(info)
    }

    #[allow(clippy::too_many_lines)] // un bras par opérateur, comme l'interpréteur
    fn run(&mut self, content: &[u8], resources: &Dict, initial_ctm: Matrix) {
        let mut st = State::new(initial_ctm);
        let mut stack: Vec<Saved> = Vec::new();
        let mut lexer = ContentLexer::new(content);
        loop {
            let start = lexer.pos();
            let Ok(Some(op)) = lexer.next_operation() else {
                break;
            };
            let end = lexer.pos();
            self.ops += 1;
            if self.ops > MAX_OPS {
                return;
            }
            self.order += 1;
            let nums: Vec<f64> = op.operands.iter().filter_map(Object::as_f64).collect();
            let n = |i: usize| nums.get(i).copied().unwrap_or(0.0);
            match op.operator.as_slice() {
                b"q" => {
                    stack.push(Saved {
                        ctm: st.ctm,
                        font: st.font.clone(),
                        size: st.size,
                        fill: st.fill,
                        stroke: st.stroke,
                        fill_components: st.fill_components,
                    });
                    self.span(start, end, SpanKind::Structural, None);
                }
                b"Q" => {
                    if let Some(s) = stack.pop() {
                        st.ctm = s.ctm;
                        st.font = s.font;
                        st.size = s.size;
                        st.fill = s.fill;
                        st.stroke = s.stroke;
                        st.fill_components = s.fill_components;
                    }
                    self.span(start, end, SpanKind::Structural, None);
                }
                b"cm" if nums.len() >= 6 => {
                    st.ctm = Matrix::new(n(0), n(1), n(2), n(3), n(4), n(5)).then(&st.ctm);
                }
                b"g" | b"rg" | b"k" => {
                    if let Some(c) = to_rgb(&nums) {
                        st.fill = c;
                        st.fill_components = nums.len();
                    }
                }
                b"G" | b"RG" | b"K" | b"SC" | b"SCN" => {
                    if let Some(c) = to_rgb(&nums) {
                        st.stroke = c;
                    }
                }
                b"cs" => {
                    st.fill_components = 1;
                    st.fill = [0.0; 3];
                    if let Some(Object::Name(name)) = op.operands.first() {
                        st.fill_components = components_of(self.doc, resources, name);
                    }
                }
                b"sc" | b"scn" => {
                    if let Some(c) = tint_to_rgb(&nums, st.fill_components) {
                        st.fill = c;
                    }
                }
                // Chemins : seuls les rectangles et la boîte englobante servent.
                b"m" | b"l" if nums.len() >= 2 => {
                    self.begin_path(start);
                    self.add_point(st.ctm.apply(Point::new(n(0), n(1))));
                }
                b"c" if nums.len() >= 6 => {
                    self.begin_path(start);
                    for i in 0..3 {
                        self.add_point(st.ctm.apply(Point::new(n(2 * i), n(2 * i + 1))));
                    }
                }
                b"v" | b"y" if nums.len() >= 4 => {
                    self.begin_path(start);
                    for i in 0..2 {
                        self.add_point(st.ctm.apply(Point::new(n(2 * i), n(2 * i + 1))));
                    }
                }
                b"re" if nums.len() >= 4 => {
                    self.begin_path(start);
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
                    self.rects.push(r);
                    self.add_point(corners[0]);
                    self.add_point(corners[2]);
                }
                b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" => {
                    let path_start = self.path_start.unwrap_or(start);
                    let bbox = self.path_bbox;
                    self.fill_path(&st);
                    self.span(path_start, end, SpanKind::Paint, bbox);
                }
                b"S" | b"s" => {
                    let path_start = self.path_start.unwrap_or(start);
                    let bbox = self.path_bbox;
                    self.clear_path();
                    self.span(path_start, end, SpanKind::Paint, bbox);
                }
                b"sh" => self.span(start, end, SpanKind::Paint, None),
                // `W n` : découpe, pas de peinture ; rien à baliser.
                b"n" => {
                    let path_start = self.path_start.unwrap_or(start);
                    self.clear_path();
                    self.span(path_start, end, SpanKind::Structural, None);
                }
                // Contenu balisé (§14.6).
                b"BMC" | b"BDC" => {
                    self.out.has_marks = true;
                    let tag = op.operands.first().and_then(Object::as_name).cloned();
                    let artifact = tag.as_ref().is_some_and(|t| t.0 == b"Artifact");
                    // `/OC` pilote la visibilité : on ne le supprime jamais.
                    let optional = tag.as_ref().is_some_and(|t| t.0 == b"OC");
                    let mcid = self.mcid_of(&op.operands, resources);
                    if self.marks.len() < 256 {
                        self.marks.push(Mark { mcid, artifact });
                        self.drops.push(!optional);
                    }
                    if let Some(id) = mcid {
                        self.out.max_mcid = Some(self.out.max_mcid.map_or(id, |m: i64| m.max(id)));
                    }
                    let kind = if optional {
                        SpanKind::Structural
                    } else {
                        SpanKind::Drop
                    };
                    self.span(start, end, kind, None);
                }
                b"EMC" => {
                    self.marks.pop();
                    let kind = if self.drops.pop().unwrap_or(true) {
                        SpanKind::Drop
                    } else {
                        SpanKind::Structural
                    };
                    self.span(start, end, kind, None);
                }
                // Texte (§9.4).
                b"BT" => {
                    st.tm = Matrix::IDENTITY;
                    st.tlm = Matrix::IDENTITY;
                    self.span(start, end, SpanKind::Structural, None);
                }
                b"ET" => self.span(start, end, SpanKind::Structural, None),
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
                b"Tj" | b"'" | b"\"" | b"TJ" => {
                    let bbox = self.show_operator(&mut st, &op);
                    if bbox.is_some() {
                        self.span(start, end, SpanKind::Text, bbox);
                    }
                }
                b"Do" => {
                    if let Some(Object::Name(name)) = op.operands.first() {
                        let (kind, bbox) = self.do_xobject(&st, resources, name);
                        if let Some(kind) = kind {
                            self.span(start, end, kind, bbox);
                        }
                    }
                }
                b"BI" if op.inline_image.is_some() => {
                    let mark = self.current();
                    let bbox = unit_square(st.ctm);
                    if self.out.images.len() < MAX_ITEMS {
                        self.out.images.push(ImageDraw {
                            bbox,
                            mcid: mark.mcid,
                            artifact: mark.artifact,
                            name: String::new(),
                        });
                    }
                    self.span(start, end, SpanKind::Image, Some(bbox));
                }
                _ => {}
            }
        }
    }

    /// `/MCID` de la propriété d'un `BDC` : dictionnaire direct ou nom à
    /// résoudre dans `/Properties` des ressources (§14.6.2).
    fn mcid_of(&self, operands: &[Object], resources: &Dict) -> Option<i64> {
        let prop = operands.get(1)?;
        let dict = match prop {
            Object::Dict(d) => d.clone(),
            Object::Name(name) => self
                .doc
                .dict_get(resources, "Properties")
                .ok()
                .flatten()
                .and_then(|p| p.as_dict().and_then(|d| d.get(name).cloned()))
                .and_then(|o| self.doc.resolve(&o).ok().and_then(|r| r.as_dict().cloned()))?,
            Object::Reference(_) => self
                .doc
                .resolve(prop)
                .ok()
                .and_then(|r| r.as_dict().cloned())?,
            _ => return None,
        };
        self.doc
            .dict_get(&dict, "MCID")
            .ok()
            .flatten()
            .and_then(|o| o.as_i64())
    }

    fn begin_path(&mut self, start: usize) {
        if self.path_start.is_none() {
            self.path_start = Some(start);
        }
    }

    fn add_point(&mut self, p: Point) {
        let r = Rect::new(p.x, p.y, p.x, p.y);
        self.path_bbox = Some(self.path_bbox.map_or(r, |b| b.union(&r)));
    }

    fn clear_path(&mut self) {
        self.rects.clear();
        self.path_bbox = None;
        self.path_start = None;
    }

    /// Un remplissage : chaque rectangle (ou, à défaut, la boîte du chemin)
    /// devient un aplat candidat comme fond.
    fn fill_path(&mut self, st: &State) {
        let order = self.order;
        let color = st.fill;
        let rects = std::mem::take(&mut self.rects);
        if self.out.fills.len() < MAX_ITEMS {
            if rects.is_empty() {
                if let Some(b) = self.path_bbox {
                    self.out.fills.push(FillRect {
                        bbox: b,
                        color,
                        order,
                    });
                }
            } else {
                for r in rects {
                    self.out.fills.push(FillRect {
                        bbox: r,
                        color,
                        order,
                    });
                }
            }
        }
        self.clear_path();
    }

    /// Un opérateur de texte : accumule tous ses glyphes en une seule
    /// exécution (c'est la granularité du balisage).
    fn show_operator(&mut self, st: &mut State, op: &Operation) -> Option<Rect> {
        let mut text = String::new();
        let mut bbox: Option<Rect> = None;
        match op.operator.as_slice() {
            b"Tj" => {
                if let Some(Object::String(s)) = op.operands.first() {
                    self.show(st, s, &mut text, &mut bbox);
                }
            }
            b"'" => {
                st.tlm = Matrix::translate(0.0, -st.leading).then(&st.tlm);
                st.tm = st.tlm;
                if let Some(Object::String(s)) = op.operands.last() {
                    self.show(st, s, &mut text, &mut bbox);
                }
            }
            b"\"" => {
                st.word_spacing = op.operands.first().and_then(Object::as_f64).unwrap_or(0.0);
                st.char_spacing = op.operands.get(1).and_then(Object::as_f64).unwrap_or(0.0);
                st.tlm = Matrix::translate(0.0, -st.leading).then(&st.tlm);
                st.tm = st.tlm;
                if let Some(Object::String(s)) = op.operands.get(2) {
                    self.show(st, s, &mut text, &mut bbox);
                }
            }
            _ => {
                if let Some(Object::Array(items)) = op.operands.last() {
                    let items = items.clone();
                    for item in &items {
                        match item {
                            Object::String(s) => self.show(st, s, &mut text, &mut bbox),
                            o => {
                                if let Some(adj) = o.as_f64() {
                                    if adj < -180.0 && !text.ends_with(' ') {
                                        text.push(' ');
                                    }
                                    let tx = -adj / 1000.0 * st.size * st.hscale;
                                    st.tm = Matrix::translate(tx, 0.0).then(&st.tm);
                                }
                            }
                        }
                    }
                }
            }
        }
        let bbox = bbox?;
        if text.trim().is_empty() || self.out.runs.len() >= MAX_ITEMS {
            return Some(bbox);
        }
        let mark = self.current();
        let color = if matches!(st.render_mode, 1 | 5) {
            st.stroke
        } else {
            st.fill
        };
        let size = (st.size * st.tm.a.hypot(st.tm.b) * st.ctm.a.hypot(st.ctm.b)).abs();
        self.out.runs.push(TextRun {
            text,
            bbox,
            mcid: mark.mcid,
            artifact: mark.artifact,
            size,
            bold: st.font.as_ref().is_some_and(|f| f.bold),
            color,
            order: self.order,
        });
        Some(bbox)
    }

    fn show(&mut self, st: &mut State, bytes: &[u8], text: &mut String, bbox: &mut Option<Rect>) {
        let Some(info) = st.font.clone() else { return };
        let font = &info.font;
        for g in font.decode(bytes) {
            let trm = Matrix::new(st.size * st.hscale, 0.0, 0.0, st.size, 0.0, st.rise)
                .then(&st.tm)
                .then(&st.ctm);
            let p0 = trm.apply(Point::new(0.0, -0.2));
            let p1 = trm.apply(Point::new(g.width.max(0.0), 0.8));
            let r = Rect::new(p0.x, p0.y, p1.x, p1.y);
            *bbox = Some(bbox.map_or(r, |b| b.union(&r)));
            let unicode = font.to_unicode(&g).unwrap_or_default();
            let counters = self
                .out
                .glyphs_by_font
                .entry(info.name.clone())
                .or_insert((0, 0));
            counters.0 += 1;
            if unicode.is_empty() && !g.is_space {
                counters.1 += 1;
            }
            if g.is_space && unicode.is_empty() {
                text.push(' ');
            } else {
                text.push_str(&unicode);
            }
            let mut tx = g.width * st.size + st.char_spacing;
            if g.is_space {
                tx += st.word_spacing;
            }
            tx *= st.hscale;
            st.tm = Matrix::translate(tx, 0.0).then(&st.tm);
        }
    }

    /// Dessine un XObject. Rend la nature de la tranche à enregistrer et sa
    /// boîte : une image est une figure, un formulaire vaut ce qu'il contient.
    fn do_xobject(
        &mut self,
        st: &State,
        resources: &Dict,
        name: &Name,
    ) -> (Option<SpanKind>, Option<Rect>) {
        let Some(entry) = self
            .doc
            .dict_get(resources, "XObject")
            .ok()
            .flatten()
            .and_then(|x| x.as_dict().and_then(|d| d.get(name).cloned()))
        else {
            return (None, None);
        };
        let Ok(obj) = self.doc.resolve(&entry) else {
            return (None, None);
        };
        let Some(dict) = obj.as_dict().cloned() else {
            return (None, None);
        };
        let subtype = self
            .doc
            .dict_get(&dict, "Subtype")
            .ok()
            .flatten()
            .and_then(|o| o.as_name().map(Name::as_str))
            .unwrap_or_default();
        if subtype == "Image" {
            let mark = self.current();
            let bbox = unit_square(st.ctm);
            if self.out.images.len() < MAX_ITEMS {
                self.out.images.push(ImageDraw {
                    bbox,
                    mcid: mark.mcid,
                    artifact: mark.artifact,
                    name: name.as_str(),
                });
            }
            return (Some(SpanKind::Image), Some(bbox));
        }
        if subtype != "Form" || self.depth >= MAX_DEPTH {
            return (None, None);
        }
        let Ok(data) = self.doc.stream_data(&obj) else {
            return (None, None);
        };
        let matrix = self
            .doc
            .dict_get(&dict, "Matrix")
            .ok()
            .flatten()
            .and_then(|m| {
                let a = m.as_array()?;
                let v: Vec<f64> = a.iter().filter_map(Object::as_f64).collect();
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
        let (runs_before, images_before) = (self.out.runs.len(), self.out.images.len());
        self.depth += 1;
        let saved_marks = self.marks.len();
        self.run(&data.data, &inner, matrix.then(&st.ctm));
        self.marks.truncate(saved_marks);
        self.drops.truncate(saved_marks);
        self.depth -= 1;
        let mut bbox: Option<Rect> = None;
        for r in &self.out.runs[runs_before..] {
            bbox = Some(bbox.map_or(r.bbox, |b| b.union(&r.bbox)));
        }
        let has_text = self.out.runs.len() > runs_before;
        for i in &self.out.images[images_before..] {
            bbox = Some(bbox.map_or(i.bbox, |b| b.union(&i.bbox)));
        }
        let kind = if has_text {
            SpanKind::Text
        } else if self.out.images.len() > images_before {
            SpanKind::Image
        } else {
            SpanKind::Paint
        };
        (Some(kind), bbox)
    }
}

/// Carré unité transformé par la CTM : la boîte d'une image (§8.9.5.2).
fn unit_square(ctm: Matrix) -> Rect {
    let corners = [
        ctm.apply(Point::new(0.0, 0.0)),
        ctm.apply(Point::new(1.0, 0.0)),
        ctm.apply(Point::new(1.0, 1.0)),
        ctm.apply(Point::new(0.0, 1.0)),
    ];
    corners.iter().skip(1).fold(
        Rect::new(corners[0].x, corners[0].y, corners[0].x, corners[0].y),
        |r, p| r.union(&Rect::new(p.x, p.y, p.x, p.y)),
    )
}

/// Nombre de composantes d'un espace colorimétrique nommé, pour savoir si
/// `scn` reçoit une teinte ou des composantes.
fn components_of(doc: &Document, resources: &Dict, name: &Name) -> usize {
    match name.0.as_slice() {
        b"DeviceGray" | b"CalGray" | b"G" | b"Indexed" | b"I" | b"Separation" => 1,
        b"DeviceRGB" | b"CalRGB" | b"RGB" | b"Lab" => 3,
        b"DeviceCMYK" | b"CMYK" => 4,
        _ => {
            let cs = doc
                .dict_get(resources, "ColorSpace")
                .ok()
                .flatten()
                .and_then(|d| d.as_dict().and_then(|d| d.get(name).cloned()))
                .and_then(|o| doc.resolve(&o).ok().map(|r| (*r).clone()));
            match cs {
                Some(Object::Name(n)) => components_of(doc, &Dict::default(), &n),
                Some(Object::Array(items)) => match items.first().and_then(Object::as_name) {
                    Some(n) if n.0 == b"ICCBased" => items
                        .get(1)
                        .and_then(|s| doc.resolve(s).ok())
                        .and_then(|s| s.as_dict().and_then(|d| d.get(&Name::new("N")).cloned()))
                        .and_then(|o| o.as_i64())
                        .and_then(|n| usize::try_from(n).ok())
                        .unwrap_or(3),
                    Some(n) if n.0 == b"DeviceN" => items
                        .get(1)
                        .and_then(|a| a.as_array())
                        .map_or(1, <[Object]>::len),
                    Some(n) => components_of(doc, &Dict::default(), n),
                    None => 1,
                },
                _ => 1,
            }
        }
    }
}
