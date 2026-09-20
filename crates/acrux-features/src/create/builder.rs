//! Charpente commune à tous les créateurs : un document neuf, des pages, un
//! flux de contenu et un jeu de polices.
//!
//! # Polices
//!
//! Deux chemins, décidés texte par texte :
//!
//! - le texte tient en WinAnsiEncoding ([`crate::fontembed::fits_winansi`]) :
//!   une des quatorze polices standard (§9.6.2.2) est référencée, sans rien
//!   incorporer, et le fichier reste minuscule ;
//! - sinon une police système est incorporée en sous-ensemble `/Type0` par
//!   [`crate::fontembed::embed_text_font`], ce qui permet d'écrire n'importe
//!   quelle écriture tout en gardant le texte extractible grâce au
//!   `/ToUnicode`.
//!
//! Les polices incorporées sont choisies **avant** la mise en page : leurs
//! largeurs décident des coupures de ligne. [`FontCache::prepare`] reçoit donc
//! l'intégralité du texte d'un style avant que la première ligne ne soit
//! composée.
//!
//! # Ressources
//!
//! Toutes les pages partagent le même dictionnaire `/Font` (les entrées
//! inutilisées ne coûtent rien) ; les images, elles, sont déclarées page par
//! page.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use acrux_core::{Error, Rect, Result};
use acrux_document::{Dict, Document, Name, Object, ObjectRef};

use crate::fontembed::{embed_text_font, fits_winansi, EmbeddedFont, FontStyle};
use crate::stamp::metrics::{encode_win_ansi, pdf_literal, StandardFont};
use crate::stamp::Rgb;

/// Nombre écrit court : trois décimales au plus, sans zéros inutiles.
#[must_use]
pub(crate) fn num(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Chaîne de texte PDF (§7.9.2) : littérale en PDFDocEncoding tant que le
/// texte reste latin, UTF-16BE avec marque d'ordre des octets sinon.
#[must_use]
pub(crate) fn text_string(text: &str) -> Object {
    if text.is_ascii() {
        return Object::String(text.as_bytes().to_vec());
    }
    let mut bytes = vec![0xFE, 0xFF];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    Object::String(bytes)
}

/// Police désignée par un fragment de texte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FontRef {
    /// Une des quatorze polices standard, en WinAnsiEncoding.
    Standard(StandardFont),
    /// Une police système incorporée, préparée pour ce style.
    Embedded(FontStyle),
}

/// Indice du style dans la table des polices incorporées.
fn slot(style: FontStyle) -> usize {
    match style {
        FontStyle::Regular => 0,
        FontStyle::Bold => 1,
        FontStyle::Italic => 2,
        FontStyle::BoldItalic => 3,
    }
}

/// Fonte, corps et couleur d'un fragment écrit : les trois vont toujours
/// ensemble, et les passer séparément allongeait toutes les signatures.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pen {
    /// Police.
    pub(crate) font: FontRef,
    /// Corps en points.
    pub(crate) size: f64,
    /// Couleur d'encre.
    pub(crate) color: Rgb,
}

/// Jeu de polices du document en construction.
pub(crate) struct FontCache {
    /// Polices standard déjà écrites, avec leur nom de ressource.
    standard: BTreeMap<StandardFont, (String, ObjectRef)>,
    /// Polices incorporées, une par style ([`slot`] donne l'indice), avec
    /// leur nom de ressource.
    embedded: [Option<(String, EmbeddedFont)>; 4],
    /// Compteur des noms de ressource (`/AKF1`, `/AKF2`…).
    next: usize,
}

impl FontCache {
    /// Jeu vide.
    pub(crate) fn new() -> Self {
        FontCache {
            standard: BTreeMap::new(),
            embedded: [None, None, None, None],
            next: 0,
        }
    }

    /// Incorpore, si ce n'est déjà fait, une police capable d'écrire `text`
    /// dans le style demandé, puis renvoie sa désignation.
    ///
    /// Appeler cette méthode une seule fois par style, avec **tout** le texte
    /// qui sera écrit dans ce style : la police retenue est celle qui couvre
    /// le mieux l'ensemble.
    ///
    /// # Errors
    /// Aucune police système exploitable.
    pub(crate) fn prepare(
        &mut self,
        doc: &Document,
        text: &str,
        style: FontStyle,
    ) -> Result<FontRef> {
        if self.embedded[slot(style)].is_none() {
            let font = embed_text_font(doc, text, style)?;
            self.next += 1;
            let name = format!("AKF{}", self.next);
            self.embedded[slot(style)] = Some((name, font));
        }
        Ok(FontRef::Embedded(style))
    }

    /// Désigne une police standard et l'écrit au besoin.
    pub(crate) fn standard(&mut self, doc: &Document, font: StandardFont) -> FontRef {
        if !self.standard.contains_key(&font) {
            self.next += 1;
            let name = format!("AKF{}", self.next);
            let mut d = Dict::new();
            d.insert(Name::new("Type"), Object::Name(Name::new("Font")));
            d.insert(Name::new("Subtype"), Object::Name(Name::new("Type1")));
            d.insert(
                Name::new("BaseFont"),
                Object::Name(Name::new(font.base_font())),
            );
            d.insert(
                Name::new("Encoding"),
                Object::Name(Name::new("WinAnsiEncoding")),
            );
            let r = doc.add(Object::Dict(d));
            self.standard.insert(font, (name, r));
        }
        FontRef::Standard(font)
    }

    /// Nom de ressource (`AKF1`) d'une police déjà enregistrée.
    pub(crate) fn resource(&self, font: FontRef) -> &str {
        match font {
            FontRef::Standard(s) => self.standard.get(&s).map_or("AKF0", |(n, _)| n.as_str()),
            FontRef::Embedded(s) => self.embedded[slot(s)]
                .as_ref()
                .map_or("AKF0", |(n, _)| n.as_str()),
        }
    }

    /// Largeur d'un texte, en points.
    pub(crate) fn width(&self, font: FontRef, text: &str, size: f64) -> f64 {
        match font {
            FontRef::Standard(s) => s.text_width(text, size),
            FontRef::Embedded(s) => self.embedded[slot(s)]
                .as_ref()
                .map_or(0.0, |(_, f)| f.width(text, size)),
        }
    }

    /// Hampe au-dessus de la ligne de base, en millièmes d'em.
    pub(crate) fn ascent(&self, font: FontRef) -> f64 {
        match font {
            FontRef::Standard(s) => s.ascent(),
            FontRef::Embedded(s) => self.embedded[slot(s)]
                .as_ref()
                .map_or(750.0, |(_, f)| f.ascent),
        }
    }

    /// Jambage sous la ligne de base (négatif), en millièmes d'em.
    pub(crate) fn descent(&self, font: FontRef) -> f64 {
        match font {
            FontRef::Standard(s) => s.descent(),
            FontRef::Embedded(s) => self.embedded[slot(s)]
                .as_ref()
                .map_or(-200.0, |(_, f)| f.descent),
        }
    }

    /// Opérande d'un `Tj` : chaîne littérale pour une police standard,
    /// chaîne hexadécimale d'indices de glyphes pour une police incorporée.
    pub(crate) fn show(&self, font: FontRef, text: &str) -> String {
        match font {
            FontRef::Standard(_) => pdf_literal(&encode_win_ansi(text).bytes),
            FontRef::Embedded(s) => self.embedded[slot(s)]
                .as_ref()
                .map_or_else(|| "<>".to_string(), |(_, f)| format!("<{}>", f.show(text))),
        }
    }

    /// Dictionnaire `/Font` de toutes les polices du document.
    pub(crate) fn resources(&self) -> Dict {
        let mut d = Dict::new();
        for (name, r) in self.standard.values() {
            d.insert(Name::new(name), Object::Reference(*r));
        }
        for (name, font) in self.embedded.iter().flatten() {
            d.insert(Name::new(name), Object::Reference(font.reference));
        }
        d
    }

    /// Choisit, pour un texte donné, la police standard demandée s'il tient
    /// en WinAnsiEncoding, sinon la police incorporée du style demandé.
    ///
    /// C'est l'aiguillage employé partout : il rend les documents latins
    /// légers (aucune police incorporée) sans rien interdire aux autres.
    ///
    /// # Errors
    /// Texte hors WinAnsi et aucune police système exploitable.
    pub(crate) fn pick(
        &mut self,
        doc: &Document,
        text: &str,
        standard: StandardFont,
        style: FontStyle,
    ) -> Result<FontRef> {
        if fits_winansi(text) {
            Ok(self.standard(doc, standard))
        } else {
            self.prepare(doc, text, style)
        }
    }
}

/// Flux de contenu en cours d'écriture, avec les images qu'il référence.
#[derive(Debug, Default)]
pub(crate) struct Content {
    /// Opérateurs déjà écrits.
    ops: String,
    /// Images référencées, nom de ressource → objet.
    xobjects: BTreeMap<String, ObjectRef>,
}

impl Content {
    /// Flux vide.
    pub(crate) fn new() -> Self {
        Content::default()
    }

    /// Ajoute des opérateurs bruts, suivis d'un saut de ligne.
    pub(crate) fn raw(&mut self, ops: &str) {
        self.ops.push_str(ops);
        self.ops.push('\n');
    }

    /// Remplit un rectangle.
    pub(crate) fn fill_rect(&mut self, r: Rect, color: [f64; 3]) {
        if r.width() <= 0.0 || r.height() <= 0.0 {
            return;
        }
        let _ = writeln!(
            self.ops,
            "q {} {} {} rg {} {} {} {} re f Q",
            num(color[0]),
            num(color[1]),
            num(color[2]),
            num(r.x0),
            num(r.y0),
            num(r.width()),
            num(r.height())
        );
    }

    /// Trace un segment horizontal (filet).
    pub(crate) fn rule(&mut self, x0: f64, x1: f64, y: f64, width: f64, color: [f64; 3]) {
        let _ = writeln!(
            self.ops,
            "q {} {} {} RG {} w {} {} m {} {} l S Q",
            num(color[0]),
            num(color[1]),
            num(color[2]),
            num(width),
            num(x0),
            num(y),
            num(x1),
            num(y)
        );
    }

    /// Dessine une image dans le rectangle donné, éventuellement rognée par
    /// `clip` (pour l'ajustement « remplir »).
    pub(crate) fn image(&mut self, name: &str, r: ObjectRef, box_: Rect, clip: Option<Rect>) {
        self.xobjects.insert(name.to_string(), r);
        self.ops.push_str("q\n");
        if let Some(c) = clip {
            let _ = writeln!(
                self.ops,
                "{} {} {} {} re W n",
                num(c.x0),
                num(c.y0),
                num(c.width()),
                num(c.height())
            );
        }
        // L'espace image va de (0,0) à (1,1) : la matrice porte la taille.
        let _ = writeln!(
            self.ops,
            "{} 0 0 {} {} {} cm /{name} Do Q",
            num(box_.width()),
            num(box_.height()),
            num(box_.x0),
            num(box_.y0)
        );
    }

    /// Écrit une ligne de texte d'une seule police, à la position donnée
    /// (`y` est la ligne de base).
    pub(crate) fn text(&mut self, fonts: &FontCache, pen: Pen, (x, y): (f64, f64), text: &str) {
        if text.is_empty() {
            return;
        }
        let _ = writeln!(
            self.ops,
            "BT {} {} {} rg /{} {} Tf 1 0 0 1 {} {} Tm {} Tj ET",
            num(pen.color[0]),
            num(pen.color[1]),
            num(pen.color[2]),
            fonts.resource(pen.font),
            num(pen.size),
            num(x),
            num(y),
            fonts.show(pen.font, text)
        );
    }

    /// Écrit une ligne de texte **justifiée** : chaque espace reçoit
    /// `extra` points de plus, portés par un `TJ` plutôt que par `Tw`, qui
    /// resterait sans effet sur une police composite `/Type0`.
    pub(crate) fn justified_text(
        &mut self,
        fonts: &FontCache,
        pen: Pen,
        (x, y): (f64, f64),
        text: &str,
        extra: f64,
    ) {
        if text.is_empty() {
            return;
        }
        if extra.abs() < 0.001 || !text.contains(' ') {
            self.text(fonts, pen, (x, y), text);
            return;
        }
        // Un déplacement de `TJ` est en millièmes de l'unité de texte, compté
        // **en sens inverse** de l'écriture : pour élargir, il est négatif.
        let shift = -extra / pen.size * 1000.0;
        let mut array = String::new();
        // Le séparateur reste dans la chaîne : les glyphes d'espace sont
        // nécessaires pour que l'extraction retrouve les mots.
        let pieces: Vec<&str> = text.split_inclusive(' ').collect();
        let last = pieces.len().saturating_sub(1);
        for (i, piece) in pieces.iter().enumerate() {
            array.push_str(&fonts.show(pen.font, piece));
            if i != last && piece.ends_with(' ') {
                let _ = write!(array, " {} ", num(shift));
            }
        }
        let _ = writeln!(
            self.ops,
            "BT {} {} {} rg /{} {} Tf 1 0 0 1 {} {} Tm [{array}] TJ ET",
            num(pen.color[0]),
            num(pen.color[1]),
            num(pen.color[2]),
            fonts.resource(pen.font),
            num(pen.size),
            num(x),
            num(y)
        );
    }
}

/// Page en cours de construction.
struct PageDraft {
    /// Référence réservée dès la création : les destinations des liens et des
    /// signets la citent avant que la page ne soit écrite.
    reference: ObjectRef,
    width: f64,
    height: f64,
    content: Content,
    annotations: Vec<Object>,
}

/// Document neuf en cours de construction.
pub(crate) struct Builder {
    /// Le document porté ; récupéré par [`Builder::finish`].
    pub(crate) doc: Document,
    /// Jeu de polices partagé par toutes les pages.
    pub(crate) fonts: FontCache,
    pages: Vec<PageDraft>,
    /// Signets à écrire, en une seule profondeur : (titre, page).
    bookmarks: Vec<(String, usize)>,
}

/// Squelette minimal : le chargeur répare les décalages et l'objet 1 devient
/// le catalogue définitif, si bien qu'aucun objet mort ne subsiste.
const SKELETON: &[u8] = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n0\n%%EOF\n";

/// Référence du catalogue dans le squelette.
const CATALOG: ObjectRef = ObjectRef {
    number: 1,
    generation: 0,
};

impl Builder {
    /// Document vierge, sans page.
    ///
    /// # Errors
    /// Squelette illisible (ne devrait jamais arriver).
    pub(crate) fn new() -> Result<Self> {
        Ok(Builder {
            doc: Document::from_bytes(SKELETON.to_vec())?,
            fonts: FontCache::new(),
            pages: Vec::new(),
            bookmarks: Vec::new(),
        })
    }

    /// Nombre de pages déjà ouvertes.
    pub(crate) fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Ouvre une page et renvoie son index.
    pub(crate) fn add_page(&mut self, width: f64, height: f64) -> usize {
        let reference = self.doc.allocate();
        self.pages.push(PageDraft {
            reference,
            width,
            height,
            content: Content::new(),
            annotations: Vec::new(),
        });
        self.pages.len() - 1
    }

    /// Flux de contenu de la page d'index donné.
    pub(crate) fn content(&mut self, page: usize) -> &mut Content {
        debug_assert!(page < self.pages.len(), "page inexistante");
        let index = page.min(self.pages.len().saturating_sub(1));
        &mut self.pages[index].content
    }

    /// Jeu de polices et flux de contenu d'une page, empruntés ensemble :
    /// écrire du texte demande de lire les largeurs tout en écrivant les
    /// opérateurs, et ce sont deux champs distincts.
    pub(crate) fn draw(&mut self, page: usize) -> (&FontCache, &mut Content) {
        debug_assert!(page < self.pages.len(), "page inexistante");
        let index = page.min(self.pages.len().saturating_sub(1));
        (&self.fonts, &mut self.pages[index].content)
    }

    /// Pose un lien externe (`/URI`) sur une page.
    pub(crate) fn add_uri_link(&mut self, page: usize, rect: Rect, uri: &str) {
        let mut action = Dict::new();
        action.insert(Name::new("S"), Object::Name(Name::new("URI")));
        action.insert(Name::new("URI"), Object::String(uri.as_bytes().to_vec()));
        self.add_link(page, rect, Object::Dict(action));
    }

    /// Pose une annotation déjà construite sur une page.
    pub(crate) fn add_annotation(&mut self, page: usize, annot: Dict) {
        if let Some(draft) = self.pages.get_mut(page) {
            draft.annotations.push(Object::Dict(annot));
        }
    }

    /// Annotation `/Link` sans bordure visible, portant l'action donnée.
    fn add_link(&mut self, page: usize, rect: Rect, action: Object) {
        let Some(draft) = self.pages.get_mut(page) else {
            return;
        };
        let mut annot = Dict::new();
        annot.insert(Name::new("Type"), Object::Name(Name::new("Annot")));
        annot.insert(Name::new("Subtype"), Object::Name(Name::new("Link")));
        annot.insert(
            Name::new("Rect"),
            Object::Array(vec![
                Object::Real(rect.x0),
                Object::Real(rect.y0),
                Object::Real(rect.x1),
                Object::Real(rect.y1),
            ]),
        );
        annot.insert(
            Name::new("Border"),
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(0),
            ]),
        );
        // Sans `/H /N`, un afficheur inverse la zone au clic ; les liens de
        // texte doivent rester discrets.
        annot.insert(Name::new("H"), Object::Name(Name::new("N")));
        annot.insert(Name::new("A"), action);
        draft.annotations.push(Object::Dict(annot));
    }

    /// Enregistre un signet de premier niveau vers une page.
    pub(crate) fn add_bookmark(&mut self, title: &str, page: usize) {
        self.bookmarks.push((title.to_string(), page));
    }

    /// Referme le document : flux de contenu, arbre des pages, signets,
    /// catalogue. Le document rendu n'est pas encore sérialisé ; appeler
    /// `save_full` pour obtenir des octets.
    ///
    /// # Errors
    /// Document sans aucune page.
    pub(crate) fn finish(self) -> Result<Document> {
        if self.pages.is_empty() {
            return Err(Error::Corrupt(
                "un document doit contenir au moins une page".into(),
            ));
        }
        let doc = self.doc;
        let root = doc.allocate();
        let fonts = self.fonts.resources();
        let mut kids = Vec::with_capacity(self.pages.len());
        let mut page_refs = Vec::with_capacity(self.pages.len());
        for draft in self.pages {
            page_refs.push(draft.reference);
            let raw = draft.content.ops.into_bytes();
            let mut stream = Dict::new();
            stream.insert(
                Name::new("Length"),
                Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
            );
            let contents = doc.add(Object::Stream { dict: stream, raw });
            let mut resources = Dict::new();
            resources.insert(Name::new("Font"), Object::Dict(fonts.clone()));
            if !draft.content.xobjects.is_empty() {
                let mut x = Dict::new();
                for (name, r) in &draft.content.xobjects {
                    x.insert(Name::new(name), Object::Reference(*r));
                }
                resources.insert(Name::new("XObject"), Object::Dict(x));
            }
            resources.insert(
                Name::new("ProcSet"),
                Object::Array(vec![
                    Object::Name(Name::new("PDF")),
                    Object::Name(Name::new("Text")),
                    Object::Name(Name::new("ImageC")),
                    Object::Name(Name::new("ImageB")),
                ]),
            );
            let mut page = Dict::new();
            page.insert(Name::new("Type"), Object::Name(Name::new("Page")));
            page.insert(Name::new("Parent"), Object::Reference(root));
            page.insert(
                Name::new("MediaBox"),
                Object::Array(vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Real(draft.width),
                    Object::Real(draft.height),
                ]),
            );
            page.insert(Name::new("Resources"), Object::Dict(resources));
            page.insert(Name::new("Contents"), Object::Reference(contents));
            if !draft.annotations.is_empty() {
                let refs: Vec<Object> = draft
                    .annotations
                    .into_iter()
                    .map(|a| Object::Reference(doc.add(a)))
                    .collect();
                page.insert(Name::new("Annots"), Object::Array(refs));
            }
            doc.set(draft.reference, Object::Dict(page));
            kids.push(Object::Reference(draft.reference));
        }
        let count = i64::try_from(kids.len()).unwrap_or(0);
        let mut tree = Dict::new();
        tree.insert(Name::new("Type"), Object::Name(Name::new("Pages")));
        tree.insert(Name::new("Kids"), Object::Array(kids));
        tree.insert(Name::new("Count"), Object::Integer(count));
        doc.set(root, Object::Dict(tree));

        let mut catalog = Dict::new();
        catalog.insert(Name::new("Type"), Object::Name(Name::new("Catalog")));
        catalog.insert(Name::new("Pages"), Object::Reference(root));
        if let Some(o) = write_outlines(&doc, &self.bookmarks, &page_refs) {
            catalog.insert(Name::new("Outlines"), Object::Reference(o));
            catalog.insert(
                Name::new("PageMode"),
                Object::Name(Name::new("UseOutlines")),
            );
        }
        doc.set(CATALOG, Object::Dict(catalog));
        doc.set_trailer_entry("Root", Object::Reference(CATALOG));
        Ok(doc)
    }
}

/// Écrit un arbre de signets plat (§12.3.3) et renvoie la racine `/Outlines`.
///
/// `pages` donne les références des pages du document, dans l'ordre ; chaque
/// signet cite l'une d'elles par son index. [`super::combine`] s'en sert sur
/// un document déjà assemblé.
pub(crate) fn write_outlines(
    doc: &Document,
    bookmarks: &[(String, usize)],
    pages: &[ObjectRef],
) -> Option<ObjectRef> {
    if bookmarks.is_empty() || pages.is_empty() {
        return None;
    }
    let root = doc.allocate();
    let refs: Vec<ObjectRef> = bookmarks.iter().map(|_| doc.allocate()).collect();
    for (i, (title, page)) in bookmarks.iter().enumerate() {
        let target = *pages.get(*page).or_else(|| pages.first())?;
        let mut item = Dict::new();
        item.insert(Name::new("Title"), text_string(title));
        item.insert(Name::new("Parent"), Object::Reference(root));
        if i > 0 {
            item.insert(Name::new("Prev"), Object::Reference(refs[i - 1]));
        }
        if let Some(next) = refs.get(i + 1) {
            item.insert(Name::new("Next"), Object::Reference(*next));
        }
        item.insert(
            Name::new("Dest"),
            Object::Array(vec![
                Object::Reference(target),
                Object::Name(Name::new("Fit")),
            ]),
        );
        doc.set(refs[i], Object::Dict(item));
    }
    let mut outlines = Dict::new();
    outlines.insert(Name::new("Type"), Object::Name(Name::new("Outlines")));
    outlines.insert(Name::new("First"), Object::Reference(refs[0]));
    outlines.insert(
        Name::new("Last"),
        Object::Reference(*refs.last().unwrap_or(&refs[0])),
    );
    outlines.insert(
        Name::new("Count"),
        Object::Integer(i64::try_from(refs.len()).unwrap_or(0)),
    );
    doc.set(root, Object::Dict(outlines));
    Some(root)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use acrux_document::collect_pages;

    #[test]
    fn text_strings_switch_to_utf16_only_when_needed() {
        assert_eq!(text_string("Plan"), Object::String(b"Plan".to_vec()));
        let accented = text_string("Résumé");
        let Object::String(bytes) = accented else {
            panic!("chaîne attendue");
        };
        assert_eq!(&bytes[..2], &[0xFE, 0xFF], "marque d'ordre des octets");
    }

    #[test]
    fn numbers_stay_short() {
        assert_eq!(num(1.0), "1");
        assert_eq!(num(1.5), "1.5");
        assert_eq!(num(1.0 / 3.0), "0.333");
        assert_eq!(num(-0.0), "-0");
    }

    #[test]
    fn an_empty_builder_refuses_to_finish() {
        let b = Builder::new().unwrap();
        assert!(b.finish().is_err());
    }

    #[test]
    fn a_one_page_document_reloads_with_its_page() {
        let mut b = Builder::new().unwrap();
        let p = b.add_page(200.0, 100.0);
        let font = b.fonts.standard(&b.doc, StandardFont::Helvetica);
        let (fonts, content) = b.draw(p);
        let pen = Pen {
            font,
            size: 12.0,
            color: [0.0; 3],
        };
        content.text(fonts, pen, (10.0, 50.0), "Bonjour");
        let doc = b.finish().unwrap();
        let bytes = doc.save_full().unwrap();
        let reread = Document::from_bytes(bytes).unwrap();
        let pages = collect_pages(&reread).unwrap();
        assert_eq!(pages.len(), 1);
        assert!((pages[0].media_box(&reread).width() - 200.0).abs() < 0.001);
        let text = crate::text::extract_page_text(&reread, &pages[0]).unwrap();
        assert_eq!(text.lines.len(), 1);
        assert_eq!(text.lines[0].text(), "Bonjour");
    }

    #[test]
    fn justification_uses_tj_offsets_and_keeps_the_spaces() {
        let mut b = Builder::new().unwrap();
        let font = b.fonts.standard(&b.doc, StandardFont::Helvetica);
        let pen = Pen {
            font,
            size: 12.0,
            color: [0.0; 3],
        };
        let mut content = Content::new();
        content.justified_text(&b.fonts, pen, (10.0, 50.0), "un deux", 4.0);
        assert!(content.ops.contains("TJ"), "{}", content.ops);
        assert!(
            content.ops.contains("(un )"),
            "l'espace reste dans la chaîne : {}",
            content.ops
        );
        // Sans élargissement, un simple `Tj` suffit.
        let mut plain = Content::new();
        plain.justified_text(&b.fonts, pen, (10.0, 50.0), "un deux", 0.0);
        assert!(plain.ops.contains("Tj"), "{}", plain.ops);
        assert!(!plain.ops.contains("TJ"), "{}", plain.ops);
    }

    #[test]
    fn bookmarks_are_written_as_a_flat_outline() {
        let mut b = Builder::new().unwrap();
        b.add_page(200.0, 100.0);
        b.add_page(200.0, 100.0);
        b.content(0).raw("q Q");
        b.content(1).raw("q Q");
        b.add_bookmark("Premier", 0);
        b.add_bookmark("Second", 1);
        let doc = b.finish().unwrap();
        let bytes = doc.save_full().unwrap();
        let reread = Document::from_bytes(bytes).unwrap();
        let pages = collect_pages(&reread).unwrap();
        let index = crate::navigation::PageIndex::new(&pages);
        let items = crate::navigation::outline(&reread, &index).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Premier");
        assert_eq!(items[1].title, "Second");
    }
}
