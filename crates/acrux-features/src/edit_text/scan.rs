//! Balayage du flux de contenu d'une page pour l'édition : suit l'état texte
//! (ISO 32000-2 §9.4.4 : `Tf Td TD Tm T* TL Tc Tw Tz Ts Tr`) exactement comme
//! le fait l'extraction (`super::super::text::extract`), et retient pour
//! chaque glyphe **où** il a été produit : indice de l'opération dans le flux,
//! élément du tableau `TJ`, octets du code dans la chaîne.
//!
//! C'est ce qui permet de retrouver le `Tj` / `TJ` / `'` / `"` qui a dessiné
//! une plage de glyphes de `PageText` et de le réécrire chirurgicalement.

use std::collections::HashMap;
use std::rc::Rc;

use acrux_core::{Matrix, Point, Result};
use acrux_document::{Dict, Document, Name, Object, ObjectRef, Page};
use acrux_render::content::ContentLexer;
use acrux_render::font::LoadedFont;
use acrux_render::page::page_content;

/// Budget d'opérations (fichiers hostiles), comme l'extraction.
const MAX_OPS: usize = 2_000_000;
/// Profondeur maximale de XObjects de formulaire.
const MAX_DEPTH: usize = 16;

/// État graphique et texte retenu par le balayage.
#[derive(Clone, Debug)]
pub(crate) struct TextSnapshot {
    /// Matrice courante.
    pub ctm: Matrix,
    /// Matrice texte.
    pub tm: Matrix,
    /// Matrice de ligne.
    pub tlm: Matrix,
    /// Nom de ressource de la police courante (`Tf`).
    pub font: Option<Name>,
    /// Taille (`Tf`).
    pub size: f64,
    /// `Tc`.
    pub char_spacing: f64,
    /// `Tw`.
    pub word_spacing: f64,
    /// `Tz` / 100.
    pub hscale: f64,
    /// `TL`.
    pub leading: f64,
    /// `Ts`.
    pub rise: f64,
    /// `Tr`.
    pub render_mode: i64,
    /// Couleur de remplissage courante, sous forme d'octets à rejouer.
    pub fill: FillColor,
}

/// Couleur de remplissage retenue telle qu'elle a été écrite : réémettre ces
/// octets restaure exactement la couleur, quel que soit l'espace
/// colorimétrique (§8.6.8).
#[derive(Clone, Debug, Default)]
pub(crate) struct FillColor {
    /// Dernier `cs` (espace colorimétrique nommé).
    space: Option<Rc<Vec<u8>>>,
    /// Dernier `g` / `rg` / `k` / `sc` / `scn`.
    value: Option<Rc<Vec<u8>>>,
}

impl FillColor {
    /// Note un opérateur de couleur.
    fn record(&mut self, operator: &[u8], raw: &[u8]) {
        match operator {
            b"cs" => {
                self.space = Some(Rc::new(raw.to_vec()));
                self.value = None;
            }
            b"sc" | b"scn" => self.value = Some(Rc::new(raw.to_vec())),
            _ => {
                self.space = None;
                self.value = Some(Rc::new(raw.to_vec()));
            }
        }
    }

    /// Octets à écrire pour rétablir cette couleur (`0 g` par défaut, §8.6.8).
    pub fn restore(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(space) = &self.space {
            out.extend_from_slice(space);
            out.push(b' ');
        }
        match &self.value {
            Some(v) => out.extend_from_slice(v),
            None if out.is_empty() => out.extend_from_slice(b"0 g"),
            None => {}
        }
        out
    }
}

impl TextSnapshot {
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
            fill: FillColor::default(),
        }
    }
}

/// Une opération du flux de la page, avec l'état avant et après.
pub(crate) struct ScannedOp {
    /// Opérateur (`Tj`, `Td`, `re`…).
    pub operator: Vec<u8>,
    /// Opérandes tels que lus.
    pub operands: Vec<Object>,
    /// État à la fin de l'opération.
    pub after: TextSnapshot,
}

/// Emplacement d'un glyphe dans le flux.
#[derive(Clone, Debug)]
pub(crate) struct Site {
    /// Indice de l'opération dans le flux de la page.
    pub op: usize,
    /// Indice de l'élément chaîne dans un tableau `TJ` (0 pour `Tj`, `'`, `"`).
    pub item: usize,
    /// Octets du code dans la chaîne (`start..end`).
    pub start: usize,
    /// Fin des octets du code.
    pub end: usize,
    /// Matrice texte avant ce glyphe.
    pub tm_before: Matrix,
    /// Le glyphe vient d'un XObject de formulaire : son flux à lui porte le
    /// texte, et c'est celui-là qu'il faudra réécrire.
    pub in_form: bool,
    /// Balise de contenu marqué en vigueur (`/Acrux… BMC`), s'il y en a une.
    /// C'est l'ancre qui permet de retrouver un bloc **écrit par nous**, quoi
    /// qu'en dise l'extraction.
    pub tag: Option<Name>,
    /// Le formulaire d'où il vient : sa référence et la matrice en vigueur au
    /// moment du `Do` (matrice `/Matrix` comprise). De quoi rouvrir ce flux
    /// **comme s'il était une page** et y retrouver les mêmes glyphes.
    pub form: Option<FormSite>,
}

/// Un XObject de formulaire rencontré pendant le balayage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FormSite {
    /// Objet indirect du flux.
    pub reference: ObjectRef,
    /// Matrice courante à l'intérieur du formulaire.
    pub ctm: Matrix,
}

/// Glyphe localisé : la clé géométrique sert à le retrouver dans le
/// `PageText` produit par `extract_page_text` (mêmes calculs, donc mêmes
/// valeurs), le site dit où le réécrire.
pub(crate) struct Located {
    /// Texte Unicode (identique à `text::Glyph::text`).
    pub text: String,
    /// Position projetée sur la direction d'écriture.
    pub along: f64,
    /// Fin de l'avance projetée.
    pub along_end: f64,
    /// Ligne de base projetée.
    pub perp: f64,
    /// Emplacement dans le flux.
    pub site: Site,
}

/// Résultat du balayage.
pub(crate) struct Scan {
    /// Toutes les opérations du flux de la page, dans l'ordre.
    pub ops: Vec<ScannedOp>,
    /// Glyphes dessinés, dans l'ordre du flux.
    pub glyphs: Vec<Located>,
    /// Contenu concaténé de la page (ce que réécrit `rewrite_content`).
    pub content: Vec<u8>,
    /// Ressources du flux balayé : celles de la page, ou celles du XObject.
    /// C'est là qu'il faut chercher les polices citées par ce flux.
    pub resources: Dict,
}

/// Dictionnaire d'une police de ressource.
pub(crate) fn font_dict(doc: &Document, resources: &Dict, name: &Name) -> Option<Dict> {
    let entry = doc
        .dict_get(resources, "Font")
        .ok()
        .flatten()
        .and_then(|f| f.as_dict().and_then(|d| d.get(name).cloned()))?;
    doc.resolve(&entry).ok()?.as_dict().cloned()
}

/// Projections d'un glyphe sur sa direction d'écriture (mêmes formules que
/// `text::projections`, pour produire des clés identiques).
fn projections(origin: Point, end: Point) -> (f64, f64, f64) {
    let angle = (end.y - origin.y).atan2(end.x - origin.x);
    let (s, c) = angle.sin_cos();
    (
        origin.x * c + origin.y * s,
        end.x * c + end.y * s,
        -origin.x * s + origin.y * c,
    )
}

struct Scanner<'a> {
    doc: &'a Document,
    fonts: HashMap<String, Rc<LoadedFont>>,
    ops: Vec<ScannedOp>,
    glyphs: Vec<Located>,
    count: usize,
    depth: usize,
    /// Pile des balises de contenu marqué ouvertes.
    tags: Vec<Name>,
}

impl Scanner<'_> {
    fn font(&mut self, resources: &Dict, name: &Name) -> Option<Rc<LoadedFont>> {
        let key = format!("{}:{}", resources.len(), name.as_str());
        if let Some(f) = self.fonts.get(&key) {
            return Some(Rc::clone(f));
        }
        let dict = font_dict(self.doc, resources, name)?;
        let font = Rc::new(LoadedFont::load(self.doc, &dict).ok()?);
        self.fonts.insert(key, Rc::clone(&font));
        Some(font)
    }

    /// Parcourt un flux ; `top` est vrai pour le contenu de la page (les
    /// opérations y sont numérotées et enregistrées).
    #[allow(clippy::too_many_lines)] // un bras par opérateur, comme l'interpréteur
    fn run(&mut self, content: &[u8], resources: &Dict, initial: Matrix, top: bool) {
        let mut st = TextSnapshot::new(initial);
        let mut stack: Vec<TextSnapshot> = Vec::new();
        let mut lexer = ContentLexer::new(content);
        let mut last = 0;
        while let Ok(Some(op)) = lexer.next_operation() {
            let end = lexer.pos();
            let raw = content[last..end].to_vec();
            last = end;
            self.count += 1;
            if self.count > MAX_OPS {
                return;
            }
            let index = if top { self.ops.len() } else { usize::MAX };
            let nums: Vec<f64> = op.operands.iter().filter_map(Object::as_f64).collect();
            let n = |i: usize| nums.get(i).copied().unwrap_or(0.0);
            match op.operator.as_slice() {
                b"BMC" | b"BDC" => {
                    // Une balise à nous ouvre une ancre ; celles des autres
                    // producteurs n'en sont pas moins empilées, pour que les
                    // `EMC` se correspondent.
                    let tag = match op.operands.first() {
                        Some(Object::Name(n)) => Some(n.clone()),
                        _ => None,
                    };
                    self.tags.push(tag.unwrap_or_else(|| Name::new("")));
                }
                b"EMC" => {
                    self.tags.pop();
                }
                b"q" => stack.push(st.clone()),
                b"Q" => {
                    if let Some(s) = stack.pop() {
                        // `q`/`Q` ne sauvegardent pas les matrices texte
                        // (§9.4.1) ; pour le reste on restaure exactement ce
                        // que restaure l'extraction, afin que les positions
                        // calculées ici soient identiques aux siennes.
                        st.ctm = s.ctm;
                        st.font = s.font;
                        st.size = s.size;
                        st.fill = s.fill;
                    }
                }
                b"cm" if nums.len() >= 6 => {
                    st.ctm = Matrix::new(n(0), n(1), n(2), n(3), n(4), n(5)).then(&st.ctm);
                }
                b"g" | b"rg" | b"k" | b"sc" | b"scn" | b"cs" => {
                    st.fill.record(&op.operator, &raw);
                }
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
                        st.font = Some(name.clone());
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
                        self.show(&mut st, resources, s, index, 0, top);
                    }
                }
                b"'" => {
                    st.tlm = Matrix::translate(0.0, -st.leading).then(&st.tlm);
                    st.tm = st.tlm;
                    if let Some(Object::String(s)) = op.operands.last() {
                        self.show(&mut st, resources, s, index, 0, top);
                    }
                }
                b"\"" => {
                    st.word_spacing = n(0);
                    st.char_spacing = n(1);
                    st.tlm = Matrix::translate(0.0, -st.leading).then(&st.tlm);
                    st.tm = st.tlm;
                    if let Some(Object::String(s)) = op.operands.get(2) {
                        self.show(&mut st, resources, s, index, 0, top);
                    }
                }
                b"TJ" => {
                    if let Some(Object::Array(items)) = op.operands.last() {
                        for (item, o) in items.iter().enumerate() {
                            match o {
                                Object::String(s) => {
                                    self.show(&mut st, resources, s, index, item, top);
                                }
                                _ => {
                                    if let Some(adj) = o.as_f64() {
                                        let tx = -adj / 1000.0 * st.size * st.hscale;
                                        st.tm = Matrix::translate(tx, 0.0).then(&st.tm);
                                    }
                                }
                            }
                        }
                    }
                }
                b"Do" => {
                    if let Some(Object::Name(name)) = op.operands.first() {
                        self.do_form(resources, name, &st, index);
                    }
                }
                _ => {}
            }
            if top {
                self.ops.push(ScannedOp {
                    operator: op.operator,
                    operands: op.operands,
                    after: st.clone(),
                });
            }
        }
    }

    /// Glyphes d'une chaîne, avec leur emplacement.
    // Sept paramètres : l'état, les ressources, la chaîne et les trois
    // coordonnées de l'emplacement (opération, élément, niveau).
    #[allow(clippy::too_many_arguments)]
    fn show(
        &mut self,
        st: &mut TextSnapshot,
        resources: &Dict,
        bytes: &[u8],
        op: usize,
        item: usize,
        top: bool,
    ) {
        let Some(name) = st.font.clone() else { return };
        let Some(font) = self.font(resources, &name) else {
            return;
        };
        let mut offset = 0;
        for (g, width) in font.decode_spans(bytes) {
            let trm = Matrix::new(st.size * st.hscale, 0.0, 0.0, st.size, 0.0, st.rise)
                .then(&st.tm)
                .then(&st.ctm);
            let mut tx = g.width * st.size + st.char_spacing;
            if g.is_space {
                tx += st.word_spacing;
            }
            tx *= st.hscale;
            let text = font.to_unicode(&g).unwrap_or_default();
            let origin = trm.apply(Point::new(0.0, 0.0));
            let end = trm.apply(Point::new(g.width.max(0.0), 0.0));
            let (along, along_end, perp) = projections(origin, end);
            let is_space = g.is_space || text.trim().is_empty();
            if !text.is_empty() {
                self.glyphs.push(Located {
                    text: if is_space { " ".into() } else { text },
                    along,
                    along_end,
                    perp,
                    site: Site {
                        op,
                        item,
                        start: offset,
                        end: offset + width,
                        tm_before: st.tm,
                        in_form: !top,
                        tag: self.tags.last().cloned(),
                        form: None,
                    },
                });
            }
            st.tm = Matrix::translate(tx, 0.0).then(&st.tm);
            offset += width;
        }
    }

    #[allow(clippy::too_many_lines)] // une suite de vérifications, lue de haut en bas
    fn do_form(&mut self, resources: &Dict, name: &Name, st: &TextSnapshot, index: usize) {
        if self.depth > MAX_DEPTH {
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
        // La référence sert à réécrire ce flux-là si l'on y modifie du texte.
        let reference = match &xobj {
            Object::Reference(r) => Some(*r),
            _ => None,
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
        let before = self.glyphs.len();
        self.run(&content.data, &res, ctm, false);
        // Les glyphes d'un formulaire renvoient à l'opération `Do` de la page,
        // mais gardent de quoi retrouver leur propre flux.
        for g in &mut self.glyphs[before..] {
            g.site.op = index;
            g.site.in_form = true;
            if g.site.form.is_none() {
                g.site.form = reference.map(|reference| FormSite { reference, ctm });
            }
        }
        self.depth -= 1;
    }
}

/// Balaye le flux de contenu d'une page.
///
/// # Errors
/// Ressources illisibles.
/// Balaye le flux d'un XObject de formulaire **comme s'il était une page**.
///
/// Même code, même état : les glyphes y ont les mêmes coordonnées qu'à
/// l'écran, puisqu'on repart de la matrice en vigueur au moment du `Do`. Ce
/// qui en sort se réécrit exactement comme le flux d'une page.
///
/// # Errors
/// Objet absent, pas un flux, ou flux illisible.
pub(crate) fn scan_form(doc: &Document, site: &FormSite, page: &Page) -> Result<Scan> {
    let object = Object::Reference(site.reference);
    let resolved = doc.resolve(&object)?;
    let Object::Stream { dict, .. } = &*resolved else {
        return Err(acrux_core::Error::Corrupt(
            "le XObject de formulaire n'est pas un flux".into(),
        ));
    };
    let content = doc.stream_data(&resolved)?;
    let resources = doc
        .dict_get(dict, "Resources")
        .ok()
        .flatten()
        .and_then(|r| r.as_dict().cloned())
        .or_else(|| {
            doc.dict_get(&page.dict, "Resources")
                .ok()
                .flatten()
                .and_then(|r| r.as_dict().cloned())
        })
        .unwrap_or_default();
    let mut sc = Scanner {
        doc,
        fonts: HashMap::new(),
        ops: Vec::new(),
        glyphs: Vec::new(),
        count: 0,
        depth: 0,
        tags: Vec::new(),
    };
    sc.run(&content.data, &resources, site.ctm, true);
    Ok(Scan {
        ops: sc.ops,
        glyphs: sc.glyphs,
        content: content.data,
        resources,
    })
}

pub(crate) fn scan(doc: &Document, page: &Page) -> Result<Scan> {
    let resources = doc
        .dict_get(&page.dict, "Resources")?
        .and_then(|r| r.as_dict().cloned())
        .unwrap_or_default();
    let content = page_content(doc, page);
    let mut sc = Scanner {
        doc,
        fonts: HashMap::new(),
        ops: Vec::new(),
        glyphs: Vec::new(),
        count: 0,
        depth: 0,
        tags: Vec::new(),
    };
    sc.run(&content, &resources, Matrix::IDENTITY, true);
    Ok(Scan {
        ops: sc.ops,
        glyphs: sc.glyphs,
        content,
        resources,
    })
}
