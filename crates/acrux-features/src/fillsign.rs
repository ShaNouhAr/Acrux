//! Remplir et signer : la signature du quotidien, celle qu'on pose à la main.
//!
//! À ne pas confondre avec le module [`signature`](crate::signature), qui
//! traite des signatures **numériques** — de la cryptographie, un certificat,
//! une preuve. Ici il n'y a aucune preuve de rien : on pose sur la page le
//! dessin de sa signature, son paraphe, une date, une croix dans une case.
//! C'est ce que la plupart des gens appellent « signer un PDF », et c'est
//! l'outil le plus utilisé d'Acrobat.
//!
//! Les deux se complètent : rien n'empêche de poser d'abord une signature
//! manuscrite, puis de sceller le tout avec [`signature::sign`](crate::signature::sign).
//!
//! # Trois façons de signer
//!
//! | Source | Ce qui est produit | Module |
//! |--------|--------------------|--------|
//! | tracée au pointeur | un contour rempli, largeur variable, bouts effilés | [`ink`] |
//! | tapée au clavier | une police manuscrite du système, incorporée en sous-ensemble | [`fontembed`](crate::fontembed) |
//! | importée (photo, capture) | l'image détourée : le papier devient transparent | [`cutout`] |
//!
//! S'y ajoutent les outils de remplissage d'Acrobat : du texte libre et cinq
//! marques ([`Mark`]) — coche, croix, rond, trait, point.
//!
//! Sur un document sans champs, les cases à cocher **dessinées** sont
//! reconnues ([`boxes`]) pour que la marque s'y cale — une suggestion, jamais
//! une pose d'office.
//!
//! # Ce qui est écrit dans le fichier
//!
//! Chaque élément posé est une **annotation** avec son apparence (`/AP /N`),
//! pas un dessin fondu dans la page : elle reste donc déplaçable et
//! supprimable, comme dans Acrobat tant qu'on n'a pas aplati. Le sous-type
//! suit l'usage d'Acrobat — `/Stamp` pour une signature ou une marque,
//! `/FreeText` pour du texte tapé — et une clé privée `/AKFillSign` note de
//! quoi il s'agit, ce qui permet à [`list`], [`remove`] et [`flatten`] de
//! retrouver nos éléments sans toucher aux annotations d'autrui.
//!
//! L'apparence est un XObject de formulaire dont la `/BBox` est exactement la
//! boîte du dessin ; le rectangle de l'annotation reçoit la même proportion,
//! si bien que l'algorithme de §12.5.5 la place sans jamais la déformer.
//!
//! Une chose change à l'aplatissement, et c'est vrai d'Acrobat aussi : le
//! dessin descend **dans le contenu**, donc sous les annotations qui restent.
//! Une signature posée par-dessus un champ de formulaire passera dessous une
//! fois fondue — sauf à aplatir le formulaire d'abord
//! ([`forms::flatten_fields`](crate::forms::flatten_fields)).
//!
//! ```no_run
//! use acrux_document::Document;
//! use acrux_core::Rect;
//! use acrux_features::fillsign::{self, Item, Options};
//! use acrux_features::fillsign::ink::{Pen, Stroke};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let doc = Document::load("contrat.pdf")?;
//! let trait_ = Stroke::from_points(&[(0.0, 0.0), (10.0, 14.0), (22.0, 2.0), (34.0, 16.0)]);
//! fillsign::place(
//!     &doc,
//!     &Options {
//!         item: Item::Drawn { strokes: vec![trait_], pen: Pen::default() },
//!         page: 0,
//!         rect: Rect::new(380.0, 90.0, 520.0, 140.0),
//!         ..Options::default()
//!     },
//! )?;
//! std::fs::write("contrat-signe.pdf", doc.save_incremental()?)?;
//! # Ok(())
//! # }
//! ```

pub mod boxes;
pub mod cutout;
pub mod ink;
pub mod marks;

use std::fmt::Write as _;

use acrux_core::{Error, Matrix, Rect, Result};
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef, Page};

use crate::annotations::{encode_text, pdf_date_now, Rgb};
use crate::fontembed::{embed_preferring, fits_winansi, FontStyle};
use crate::stamp::{encode_win_ansi, pdf_literal, StandardFont};

pub use ink::{InkPoint, Nib, Outline, Pen, Seg, Stroke, Weight};
pub use marks::Mark;

/// Clé privée qui signe nos annotations, et valeur de chaque sorte.
const TAG: &str = "AKFillSign";

/// Familles manuscrites essayées pour une signature tapée, de la plus
/// courante à la moins. Windows installe les quatre premières.
const SCRIPT_FAMILIES: &[&str] = &[
    "segoesc",    // Segoe Script
    "inkfree",    // Ink Free
    "brushsci",   // Brush Script MT
    "lhandw",     // Lucida Handwriting
    "gabriola",   //
    "freescpt",   // Freestyle Script
    "segoepr",    // Segoe Print
    "comic",      // Comic Sans, en dernier recours
    "dancingscr", //
    "caveat",     //
];

/// Hauteur de référence du dessin d'un texte, dans l'espace de l'apparence.
///
/// Sans importance visuelle — la boîte est remise à l'échelle du rectangle
/// demandé — mais assez grande pour que les arrondis n'y paraissent pas.
const TEXT_UNITS: f64 = 100.0;

/// Ce que l'on pose sur la page.
#[derive(Debug, Clone)]
pub enum Item {
    /// Signature ou paraphe tracé au pointeur.
    Drawn {
        /// Traits relevés, un par poser-lever.
        strokes: Vec<Stroke>,
        /// Réglages de la plume.
        pen: Pen,
    },
    /// Signature tapée, écrite avec une police manuscrite du système.
    Typed {
        /// Le nom, tel qu'il sera écrit.
        text: String,
    },
    /// Signature importée : photo ou capture PNG / JPEG.
    Image {
        /// Octets du fichier image.
        data: Vec<u8>,
        /// Détourage du fond, ou image posée telle quelle.
        cutout: Option<cutout::Options>,
    },
    /// Texte de remplissage (nom, date, numéro…).
    Text {
        /// Le texte à écrire.
        text: String,
    },
    /// Marque : coche, croix, rond, trait, point.
    Mark(Mark),
    /// Texte réparti dans un **peigne** — les cases alignées d'un IBAN, d'un
    /// BIC, d'une date : un caractère par case, centré dans la sienne.
    ///
    /// Le rectangle de pose doit être la réunion des cases
    /// ([`boxes::Comb::bounds`]) : les caractères se placent par rapport à
    /// lui.
    Comb {
        /// Le texte ; ce qui dépasse le nombre de cases est ignoré.
        text: String,
        /// Les cases, de gauche à droite, en coordonnées de page.
        cells: Vec<Rect>,
    },
}

impl Item {
    /// Nom de la sorte, écrit dans `/AKFillSign` et rendu par [`list`].
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Item::Drawn { .. } => "drawn",
            Item::Typed { .. } => "typed",
            Item::Image { .. } => "image",
            Item::Text { .. } => "text",
            Item::Mark(_) => "mark",
            Item::Comb { .. } => "comb",
        }
    }

    /// Vrai si l'élément est une signature (par opposition à un remplissage).
    #[must_use]
    pub fn is_signature(&self) -> bool {
        matches!(
            self,
            Item::Drawn { .. } | Item::Typed { .. } | Item::Image { .. }
        )
    }
}

/// Façon d'occuper le rectangle demandé.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fit {
    /// Proportions gardées, dessin centré dans le rectangle. Le rectangle de
    /// l'annotation est **réduit** à ce que le dessin occupe vraiment.
    #[default]
    Contain,
    /// Le dessin est étiré pour remplir exactement le rectangle.
    Stretch,
}

/// Options de pose.
#[derive(Debug, Clone)]
pub struct Options {
    /// Ce qui est posé.
    pub item: Item,
    /// Index de la page, `0` pour la première.
    pub page: usize,
    /// Emplacement voulu, en coordonnées de page.
    pub rect: Rect,
    /// Comment occuper ce rectangle.
    pub fit: Fit,
    /// Couleur de l'encre. Ignorée par une image non recolorée.
    pub color: Rgb,
    /// Auteur (`/T`), celui qui signe.
    pub author: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            item: Item::Mark(Mark::Check),
            page: 0,
            rect: Rect::new(0.0, 0.0, 72.0, 24.0),
            fit: Fit::Contain,
            // Noir : c'est avec quoi l'on signe un papier, et c'est ce qui
            // sort d'une imprimante ensuite. Les autres couleurs sont un
            // choix, pas un défaut.
            color: [0.0, 0.0, 0.0],
            author: None,
        }
    }
}

/// Un élément « remplir et signer » trouvé dans un document.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    /// Index de la page.
    pub page: usize,
    /// Position dans le tableau `/Annots` de la page.
    pub index: usize,
    /// Sorte, telle que [`Item::kind`] la nomme.
    pub kind: String,
    /// Rectangle occupé.
    pub rect: Rect,
    /// Auteur déclaré.
    pub author: Option<String>,
}

/// Pose un élément sur une page et rend la référence de l'annotation créée.
///
/// # Errors
/// Page inexistante, police manuscrite introuvable pour une signature tapée,
/// image illisible, ou document dont les pages ne sont pas des objets
/// indirects.
pub fn place(doc: &Document, options: &Options) -> Result<ObjectRef> {
    let pages = collect_pages(doc)?;
    let page = pages
        .get(options.page)
        .ok_or_else(|| Error::Corrupt(format!("page {} inexistante", options.page + 1)))?;
    let form = build(doc, options)?;
    let rect = fitted_rect(options.rect, form.width, form.height, options.fit);
    let appearance = doc.add(form.stream(rect));
    annotate(doc, page, options, rect, appearance)
}

/// Inventaire des éléments posés par ce module.
///
/// # Errors
/// Document illisible.
pub fn list(doc: &Document) -> Result<Vec<Placed>> {
    let mut out = Vec::new();
    for (page_index, page) in collect_pages(doc)?.iter().enumerate() {
        for (index, annot) in annots(doc, page).iter().enumerate() {
            let Some(dict) = resolve_dict(doc, annot) else {
                continue;
            };
            let Some(kind) = tag_of(doc, &dict) else {
                continue;
            };
            out.push(Placed {
                page: page_index,
                index,
                kind,
                rect: rect_of(doc, &dict).unwrap_or_default(),
                author: text_of(doc, &dict, "T"),
            });
        }
    }
    Ok(out)
}

/// Retire un élément posé, désigné par sa page et sa position dans `/Annots`.
///
/// # Errors
/// Page ou index inexistant, ou annotation qui n'est pas de ce module — on ne
/// supprime jamais l'annotation de quelqu'un d'autre par une erreur d'index.
pub fn remove(doc: &Document, page_index: usize, index: usize) -> Result<()> {
    let pages = collect_pages(doc)?;
    let page = pages
        .get(page_index)
        .ok_or_else(|| Error::Corrupt(format!("page {} inexistante", page_index + 1)))?;
    let list = annots(doc, page);
    let target = list
        .get(index)
        .ok_or_else(|| Error::Corrupt(format!("annotation {} inexistante", index + 1)))?;
    let dict =
        resolve_dict(doc, target).ok_or_else(|| Error::Corrupt("annotation illisible".into()))?;
    if tag_of(doc, &dict).is_none() {
        return Err(Error::Corrupt(
            "cette annotation ne vient pas de « remplir et signer »".into(),
        ));
    }
    crate::annotations::remove_annotation(doc, page, index)
}

/// Déplace ou redimensionne un élément déjà posé.
///
/// Seul le rectangle de l'annotation change : son apparence est un XObject de
/// formulaire, que le lecteur remet **à l'échelle du rectangle** (§12.5.5).
/// Une signature agrandie se redessine donc nette, sans perte, et un
/// déplacement ne touche à rien d'autre.
///
/// # Errors
/// Page ou annotation inexistante, ou annotation qui ne vient pas de
/// « remplir et signer ».
pub fn set_rect(doc: &Document, page_index: usize, index: usize, rect: Rect) -> Result<()> {
    let pages = collect_pages(doc)?;
    let page = pages
        .get(page_index)
        .ok_or_else(|| Error::Corrupt(format!("page {} inexistante", page_index + 1)))?;
    let list = annots(doc, page);
    let target = list
        .get(index)
        .ok_or_else(|| Error::Corrupt(format!("annotation {} inexistante", index + 1)))?;
    let mut dict =
        resolve_dict(doc, target).ok_or_else(|| Error::Corrupt("annotation illisible".into()))?;
    if tag_of(doc, &dict).is_none() {
        return Err(Error::Corrupt(
            "cette annotation ne vient pas de « remplir et signer »".into(),
        ));
    }
    let (x0, y0) = (rect.x0.min(rect.x1), rect.y0.min(rect.y1));
    let (x1, y1) = (rect.x0.max(rect.x1), rect.y0.max(rect.y1));
    dict.insert(
        Name::new("Rect"),
        Object::Array(vec![
            Object::Real(x0),
            Object::Real(y0),
            Object::Real(x1),
            Object::Real(y1),
        ]),
    );
    match target {
        Object::Reference(r) => {
            doc.set(*r, Object::Dict(dict));
            Ok(())
        }
        _ => Err(Error::Unsupported(
            "annotation écrite dans la page : non déplaçable".into(),
        )),
    }
}

/// Cache ou remontre un élément posé, sans le supprimer.
///
/// Sert au déplacement : l'original disparaît le temps du geste, un aperçu
/// suit le pointeur, et il revient à sa nouvelle place au relâchement. Le
/// drapeau est celui du format (§12.5.3, bit 2 de `/F`), donc tous les
/// lecteurs le comprennent.
///
/// # Errors
/// Page ou annotation inexistante, ou annotation qui n'est pas la nôtre.
pub fn set_hidden(doc: &Document, page_index: usize, index: usize, hidden: bool) -> Result<()> {
    let pages = collect_pages(doc)?;
    let page = pages
        .get(page_index)
        .ok_or_else(|| Error::Corrupt(format!("page {} inexistante", page_index + 1)))?;
    let list = annots(doc, page);
    let target = list
        .get(index)
        .ok_or_else(|| Error::Corrupt(format!("annotation {} inexistante", index + 1)))?;
    let mut dict =
        resolve_dict(doc, target).ok_or_else(|| Error::Corrupt("annotation illisible".into()))?;
    if tag_of(doc, &dict).is_none() {
        return Err(Error::Corrupt(
            "cette annotation ne vient pas de « remplir et signer »".into(),
        ));
    }
    let flags = dict
        .get(&Name::new("F"))
        .and_then(Object::as_i64)
        .unwrap_or(0);
    let flags = if hidden { flags | 2 } else { flags & !2 };
    dict.insert(Name::new("F"), Object::Integer(flags));
    match target {
        Object::Reference(r) => {
            doc.set(*r, Object::Dict(dict));
            Ok(())
        }
        _ => Err(Error::Unsupported(
            "annotation écrite dans la page : non modifiable".into(),
        )),
    }
}

/// Fond définitivement les éléments posés dans le contenu des pages.
///
/// Après quoi ils ne se déplacent plus, ne se suppriment plus, et se voient
/// dans n'importe quel lecteur, même un qui ignorerait les annotations. C'est
/// l'équivalent de l'aplatissement d'Acrobat. Rend le nombre d'éléments fondus.
///
/// # Errors
/// Document illisible ou page non indirecte.
pub fn flatten(doc: &Document) -> Result<usize> {
    let mut done = 0usize;
    let mut counter = 0usize;
    for page in collect_pages(doc)? {
        let Some(page_ref) = page.reference else {
            continue;
        };
        let list = annots(doc, &page);
        if list.is_empty() {
            continue;
        }
        let mut kept = Vec::new();
        let mut xobjects = Dict::new();
        let mut drawing = String::new();
        for annot in &list {
            let Some(dict) = resolve_dict(doc, annot) else {
                kept.push(annot.clone());
                continue;
            };
            let (Some(_), Some(rect), Some(ap)) = (
                tag_of(doc, &dict),
                rect_of(doc, &dict),
                normal_appearance(doc, &dict),
            ) else {
                kept.push(annot.clone());
                continue;
            };
            let stream = doc.get(ap)?.as_dict().cloned().unwrap_or_default();
            let matrix = placement_matrix(doc, &stream, rect);
            counter += 1;
            let name = format!("AkSign{counter}");
            xobjects.insert(Name::new(&name), Object::Reference(ap));
            let _ = writeln!(
                drawing,
                "q {} {} {} {} {} {} cm /{name} Do Q",
                fmt(matrix.a),
                fmt(matrix.b),
                fmt(matrix.c),
                fmt(matrix.d),
                fmt(matrix.e),
                fmt(matrix.f)
            );
            if let Object::Reference(r) = annot {
                doc.delete(*r);
            }
            done += 1;
        }
        if kept.len() == list.len() {
            continue;
        }
        let mut page_dict = page.dict.clone();
        if kept.is_empty() {
            page_dict.remove(&Name::new("Annots"));
        } else {
            page_dict.insert(Name::new("Annots"), Object::Array(kept));
        }
        let mut resources = dict_of(doc, &page_dict, "Resources").unwrap_or_default();
        let mut existing = dict_of(doc, &resources, "XObject").unwrap_or_default();
        existing.extend(xobjects);
        resources.insert(Name::new("XObject"), Object::Dict(existing));
        page_dict.insert(Name::new("Resources"), Object::Dict(resources));
        // Le contenu d'origine est encadré par q … Q : l'état graphique qu'il
        // laisse derrière lui ne doit pas déteindre sur ce qu'on ajoute.
        let mut contents = vec![Object::Reference(doc.add(plain_stream(b"q\n")))];
        match page_dict.get(&Name::new("Contents")) {
            Some(Object::Array(items)) => contents.extend(items.iter().cloned()),
            Some(Object::Reference(r)) => match &*doc.get(*r)? {
                Object::Array(items) => contents.extend(items.iter().cloned()),
                _ => contents.push(Object::Reference(*r)),
            },
            _ => {}
        }
        let tail = format!("Q\n{drawing}");
        contents.push(Object::Reference(doc.add(plain_stream(tail.as_bytes()))));
        page_dict.insert(Name::new("Contents"), Object::Array(contents));
        doc.set(page_ref, Object::Dict(page_dict));
    }
    Ok(done)
}

// ---------------------------------------------------------------------------
// Construction de l'apparence.
// ---------------------------------------------------------------------------

/// Dessin prêt à devenir un XObject de formulaire.
struct Form {
    /// Flux de contenu, dans un repère dont l'origine est le coin bas-gauche.
    content: String,
    /// Largeur naturelle du dessin.
    width: f64,
    /// Hauteur naturelle.
    height: f64,
    /// Ressources (police, image) référencées par le contenu.
    resources: Dict,
}

impl Form {
    /// Objet flux complet, `/BBox` comprise.
    fn stream(self, rect: Rect) -> Object {
        let mut dict = Dict::new();
        dict.insert(Name::new("Type"), Object::Name(Name::new("XObject")));
        dict.insert(Name::new("Subtype"), Object::Name(Name::new("Form")));
        dict.insert(Name::new("FormType"), Object::Integer(1));
        dict.insert(
            Name::new("BBox"),
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(self.width),
                Object::Real(self.height),
            ]),
        );
        dict.insert(Name::new("Resources"), Object::Dict(self.resources));
        let _ = rect;
        let raw = self.content.into_bytes();
        dict.insert(
            Name::new("Length"),
            Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
        );
        Object::Stream { dict, raw }
    }
}

/// Fabrique le dessin correspondant à l'élément demandé.
fn build(doc: &Document, options: &Options) -> Result<Form> {
    match &options.item {
        Item::Drawn { strokes, pen } => Ok(drawn_form(strokes, pen, options.color)),
        Item::Typed { text } => typed_form(doc, text, options.color),
        Item::Image { data, cutout } => cutout::form(doc, data, cutout.as_ref(), options.color),
        Item::Text { text } => Ok(text_form(doc, text, options.color)),
        Item::Mark(mark) => Ok(marks::form(*mark, options.color)),
        Item::Comb { text, cells } => Ok(comb_form(text, cells, options.color)),
    }
}

/// Texte d'un peigne : chaque caractère centré dans sa case, en Helvetica.
///
/// Le repère est celui de la réunion des cases, en points : posé dans ce
/// même rectangle, le dessin n'est ni étiré ni déplacé. Le corps suit la
/// hauteur des cases, et se réduit si un caractère large — un « W » dans une
/// case étroite — ne tenait pas.
fn comb_form(text: &str, cells: &[Rect], color: Rgb) -> Form {
    let standard = StandardFont::Helvetica;
    let bounds = boxes::bounds_of(cells);
    let height = cells.iter().map(Rect::height).fold(f64::MAX, f64::min);
    let narrowest = cells.iter().map(Rect::width).fold(f64::MAX, f64::min);
    let chars: Vec<char> = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| if fits_winansi(&c.to_string()) { c } else { '?' })
        .take(cells.len())
        .collect();
    let mut size = (height * 0.74).max(1.0);
    let widest = chars
        .iter()
        .map(|c| standard.text_width(&c.to_string(), 1.0))
        .fold(0.0, f64::max);
    if widest * size > narrowest * 0.86 {
        size = narrowest * 0.86 / widest;
    }
    // Hauteur d'une capitale d'Helvetica : 718 millièmes du corps.
    let cap = 0.718 * size;
    let mut content = format!(
        "{} {} {} rg\nBT /AkText {} Tf\n",
        fmt(color[0]),
        fmt(color[1]),
        fmt(color[2]),
        fmt(size)
    );
    for (c, cell) in chars.iter().zip(cells) {
        let glyph = c.to_string();
        let width = standard.text_width(&glyph, size);
        let x = cell.x0 - bounds.x0 + (cell.width() - width) / 2.0;
        let y = cell.y0 - bounds.y0 + (cell.height() - cap) / 2.0;
        let _ = writeln!(
            content,
            "1 0 0 1 {} {} Tm {} Tj",
            fmt(x),
            fmt(y),
            pdf_literal(&encode_win_ansi(&glyph).bytes)
        );
    }
    content.push_str("ET\n");
    let mut font_dict = Dict::new();
    font_dict.insert(Name::new("Type"), Object::Name(Name::new("Font")));
    font_dict.insert(Name::new("Subtype"), Object::Name(Name::new("Type1")));
    font_dict.insert(
        Name::new("BaseFont"),
        Object::Name(Name::new(standard.base_font())),
    );
    font_dict.insert(
        Name::new("Encoding"),
        Object::Name(Name::new("WinAnsiEncoding")),
    );
    let mut fonts = Dict::new();
    fonts.insert(Name::new("AkText"), Object::Dict(font_dict));
    let mut resources = Dict::new();
    resources.insert(Name::new("Font"), Object::Dict(fonts));
    Form {
        content,
        width: bounds.width().max(1.0),
        height: bounds.height().max(1.0),
        resources,
    }
}

/// Dessin d'une signature tracée.
fn drawn_form(strokes: &[Stroke], pen: &Pen, color: Rgb) -> Form {
    let outline = ink::outline(strokes, pen);
    let bbox = outline.bbox;
    let width = (bbox.x1 - bbox.x0).max(0.01);
    let height = (bbox.y1 - bbox.y0).max(0.01);
    let mut content = format!("{} {} {} rg\n", fmt(color[0]), fmt(color[1]), fmt(color[2]));
    write_path(&mut content, &outline, -bbox.x0, -bbox.y0);
    // Règle non nulle : les boucles du contour intérieur, dans les virages
    // très serrés, se remplissent au lieu de percer.
    content.push_str("f\n");
    Form {
        content,
        width,
        height,
        resources: Dict::new(),
    }
}

/// Écrit un contour dans un flux de contenu, décalé de `dx`, `dy`.
fn write_path(out: &mut String, outline: &Outline, dx: f64, dy: f64) {
    for seg in &outline.segs {
        match *seg {
            Seg::Move(x, y) => {
                let _ = writeln!(out, "{} {} m", fmt(x + dx), fmt(y + dy));
            }
            Seg::Line(x, y) => {
                let _ = writeln!(out, "{} {} l", fmt(x + dx), fmt(y + dy));
            }
            Seg::Curve(x1, y1, x2, y2, x3, y3) => {
                let _ = writeln!(
                    out,
                    "{} {} {} {} {} {} c",
                    fmt(x1 + dx),
                    fmt(y1 + dy),
                    fmt(x2 + dx),
                    fmt(y2 + dy),
                    fmt(x3 + dx),
                    fmt(y3 + dy)
                );
            }
            Seg::Close => out.push_str("h\n"),
        }
    }
}

/// Signature tapée : une police manuscrite du système, incorporée.
fn typed_form(doc: &Document, text: &str, color: Rgb) -> Result<Form> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::Corrupt("signature tapée : le texte est vide".into()));
    }
    let font = embed_preferring(doc, text, FontStyle::Italic, SCRIPT_FAMILIES)?;
    let width = font
        .shaped_width(text, TEXT_UNITS)
        .unwrap_or_else(|| font.width(text, TEXT_UNITS))
        .max(1.0);
    let ascent = font.ascent / 1000.0 * TEXT_UNITS;
    let descent = font.descent / 1000.0 * TEXT_UNITS;
    let height = (ascent - descent).max(1.0);
    let show = font
        .shape_line(text)
        .unwrap_or_else(|| format!("<{}> Tj", font.show(text)));
    let content = format!(
        "{} {} {} rg\nBT /AkScript {} Tf 1 0 0 1 0 {} Tm {show} ET\n",
        fmt(color[0]),
        fmt(color[1]),
        fmt(color[2]),
        fmt(TEXT_UNITS),
        fmt(-descent)
    );
    let mut fonts = Dict::new();
    fonts.insert(Name::new("AkScript"), Object::Reference(font.reference));
    let mut resources = Dict::new();
    resources.insert(Name::new("Font"), Object::Dict(fonts));
    Ok(Form {
        content,
        width,
        height,
        resources,
    })
}

/// Texte de remplissage : Helvetica quand elle suffit, une police système
/// incorporée sinon (accents rares, alphabets non latins).
fn text_form(doc: &Document, text: &str, color: Rgb) -> Form {
    let text = if text.is_empty() { " " } else { text };
    if !fits_winansi(text) {
        if let Ok(font) = embed_preferring(doc, text, FontStyle::Regular, &[]) {
            let width = font.width(text, TEXT_UNITS).max(1.0);
            let ascent = font.ascent / 1000.0 * TEXT_UNITS;
            let descent = font.descent / 1000.0 * TEXT_UNITS;
            let mut fonts = Dict::new();
            fonts.insert(Name::new("AkText"), Object::Reference(font.reference));
            let mut resources = Dict::new();
            resources.insert(Name::new("Font"), Object::Dict(fonts));
            return Form {
                content: format!(
                    "{} {} {} rg\nBT /AkText {} Tf 1 0 0 1 0 {} Tm <{}> Tj ET\n",
                    fmt(color[0]),
                    fmt(color[1]),
                    fmt(color[2]),
                    fmt(TEXT_UNITS),
                    fmt(-descent),
                    font.show(text)
                ),
                width,
                height: (ascent - descent).max(1.0),
                resources,
            };
        }
    }
    let standard = StandardFont::Helvetica;
    let width = standard.text_width(text, TEXT_UNITS).max(1.0);
    let ascent = standard.ascent() / 1000.0 * TEXT_UNITS;
    let descent = standard.descent() / 1000.0 * TEXT_UNITS;
    let encoded = encode_win_ansi(text);
    let mut font_dict = Dict::new();
    font_dict.insert(Name::new("Type"), Object::Name(Name::new("Font")));
    font_dict.insert(Name::new("Subtype"), Object::Name(Name::new("Type1")));
    font_dict.insert(
        Name::new("BaseFont"),
        Object::Name(Name::new(standard.base_font())),
    );
    font_dict.insert(
        Name::new("Encoding"),
        Object::Name(Name::new("WinAnsiEncoding")),
    );
    let mut fonts = Dict::new();
    fonts.insert(Name::new("AkText"), Object::Dict(font_dict));
    let mut resources = Dict::new();
    resources.insert(Name::new("Font"), Object::Dict(fonts));
    Form {
        content: format!(
            "{} {} {} rg\nBT /AkText {} Tf 1 0 0 1 0 {} Tm {} Tj ET\n",
            fmt(color[0]),
            fmt(color[1]),
            fmt(color[2]),
            fmt(TEXT_UNITS),
            fmt(-descent),
            pdf_literal(&encoded.bytes)
        ),
        width,
        height: (ascent - descent).max(1.0),
        resources,
    }
}

// ---------------------------------------------------------------------------
// Pose de l'annotation.
// ---------------------------------------------------------------------------

/// Ajuste le rectangle demandé aux proportions du dessin.
fn fitted_rect(rect: Rect, width: f64, height: f64, fit: Fit) -> Rect {
    if fit == Fit::Stretch || width <= 0.0 || height <= 0.0 {
        return rect;
    }
    let (rw, rh) = (rect.width(), rect.height());
    if rw <= 0.0 || rh <= 0.0 {
        return rect;
    }
    let scale = (rw / width).min(rh / height);
    let (w, h) = (width * scale, height * scale);
    let x = (rw - w) / 2.0 + rect.x0;
    let y = (rh - h) / 2.0 + rect.y0;
    Rect::new(x, y, x + w, y + h)
}

/// Écrit l'annotation et l'ajoute au tableau `/Annots` de la page.
fn annotate(
    doc: &Document,
    page: &Page,
    options: &Options,
    rect: Rect,
    appearance: ObjectRef,
) -> Result<ObjectRef> {
    let page_ref = page
        .reference
        .ok_or_else(|| Error::Corrupt("la page doit être un objet indirect".into()))?;
    let mut d = Dict::new();
    d.insert(Name::new("Type"), Object::Name(Name::new("Annot")));
    // Acrobat pose un /Stamp pour une signature ou une marque, un /FreeText
    // pour du texte tapé : on garde ses sous-types, pour que ses propres
    // outils reconnaissent ce que nous posons.
    let subtype = if matches!(options.item, Item::Text { .. }) {
        "FreeText"
    } else {
        "Stamp"
    };
    d.insert(Name::new("Subtype"), Object::Name(Name::new(subtype)));
    d.insert(Name::new("P"), Object::Reference(page_ref));
    d.insert(
        Name::new("Rect"),
        Object::Array(vec![
            Object::Real(rect.x0),
            Object::Real(rect.y0),
            Object::Real(rect.x1),
            Object::Real(rect.y1),
        ]),
    );
    d.insert(Name::new("M"), Object::String(pdf_date_now().into_bytes()));
    // 4 = à imprimer. Une signature qui ne sortirait pas à l'impression
    // n'aurait aucun sens.
    d.insert(Name::new("F"), Object::Integer(4));
    d.insert(Name::new(TAG), Object::Name(Name::new(options.item.kind())));
    if let Some(author) = &options.author {
        d.insert(Name::new("T"), Object::String(encode_text(author)));
    }
    if let Item::Typed { text } | Item::Text { text } | Item::Comb { text, .. } = &options.item {
        d.insert(Name::new("Contents"), Object::String(encode_text(text)));
    }
    if matches!(options.item, Item::Text { .. }) {
        // Un /FreeText doit porter un /DA, même quand son apparence est déjà
        // écrite : les lecteurs qui régénèrent l'apparence s'y raccrochent.
        d.insert(
            Name::new("DA"),
            Object::String(
                format!(
                    "{} {} {} rg /Helv 12 Tf",
                    fmt(options.color[0]),
                    fmt(options.color[1]),
                    fmt(options.color[2])
                )
                .into_bytes(),
            ),
        );
        d.insert(Name::new("Q"), Object::Integer(0));
    } else {
        d.insert(Name::new("Name"), Object::Name(Name::new("AcruxSignature")));
    }
    let mut ap = Dict::new();
    ap.insert(Name::new("N"), Object::Reference(appearance));
    d.insert(Name::new("AP"), Object::Dict(ap));

    let annot_ref = doc.add(Object::Dict(d));
    let mut page_dict = page.dict.clone();
    let mut list = annots(doc, page);
    list.push(Object::Reference(annot_ref));
    page_dict.insert(Name::new("Annots"), Object::Array(list));
    doc.set(page_ref, Object::Dict(page_dict));
    Ok(annot_ref)
}

// ---------------------------------------------------------------------------
// Petits utilitaires partagés.
// ---------------------------------------------------------------------------

/// Nombre écrit court : quatre décimales au plus, sans zéros inutiles.
pub(crate) fn fmt(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let mut s = format!("{v:.4}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        s = "0".into();
    }
    s
}

/// Tableau `/Annots` d'une page, résolu.
fn annots(doc: &Document, page: &Page) -> Vec<Object> {
    match page.dict.get(&Name::new("Annots")) {
        Some(o) => doc
            .resolve(o)
            .ok()
            .and_then(|r| r.as_array().map(<[Object]>::to_vec))
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

fn resolve_dict(doc: &Document, object: &Object) -> Option<Dict> {
    doc.resolve(object).ok()?.as_dict().cloned()
}

/// Valeur de la clé privée, si l'annotation vient de ce module.
fn tag_of(doc: &Document, dict: &Dict) -> Option<String> {
    match &*doc.dict_get(dict, TAG).ok()?? {
        Object::Name(n) => Some(n.as_str()),
        _ => None,
    }
}

fn rect_of(doc: &Document, dict: &Dict) -> Option<Rect> {
    let values = doc.dict_get(dict, "Rect").ok()??;
    let array = values.as_array()?;
    if array.len() != 4 {
        return None;
    }
    let n: Vec<f64> = array.iter().filter_map(Object::as_f64).collect();
    (n.len() == 4).then(|| Rect::new(n[0], n[1], n[2], n[3]))
}

fn text_of(doc: &Document, dict: &Dict, key: &str) -> Option<String> {
    match &*doc.dict_get(dict, key).ok()?? {
        Object::String(s) => Some(acrux_document::text::decode_text_string(s)),
        _ => None,
    }
}

fn dict_of(doc: &Document, dict: &Dict, key: &str) -> Option<Dict> {
    doc.dict_get(dict, key)
        .ok()?
        .and_then(|o| o.as_dict().cloned())
}

/// Référence de l'apparence normale.
fn normal_appearance(doc: &Document, dict: &Dict) -> Option<ObjectRef> {
    let ap = doc.dict_get(dict, "AP").ok()??;
    let ap = ap.as_dict()?;
    match ap.get(&Name::new("N"))? {
        Object::Reference(r) => Some(*r),
        _ => None,
    }
}

/// Matrice qui amène la `/BBox` du formulaire dans le rectangle (§12.5.5).
fn placement_matrix(doc: &Document, stream: &Dict, rect: Rect) -> Matrix {
    let numbers = |key: &str| -> Vec<f64> {
        doc.dict_get(stream, key)
            .ok()
            .flatten()
            .and_then(|o| {
                o.as_array()
                    .map(|a| a.iter().filter_map(Object::as_f64).collect())
            })
            .unwrap_or_default()
    };
    let bbox = numbers("BBox");
    if bbox.len() != 4 {
        return Matrix::IDENTITY;
    }
    let m = numbers("Matrix");
    let matrix = if m.len() == 6 {
        Matrix::new(m[0], m[1], m[2], m[3], m[4], m[5])
    } else {
        Matrix::IDENTITY
    };
    let transformed = matrix.transform_rect(&Rect::new(bbox[0], bbox[1], bbox[2], bbox[3]));
    let sx = if transformed.width() > 1e-9 {
        rect.width() / transformed.width()
    } else {
        1.0
    };
    let sy = if transformed.height() > 1e-9 {
        rect.height() / transformed.height()
    } else {
        1.0
    };
    Matrix::new(
        sx,
        0.0,
        0.0,
        sy,
        rect.x0 - transformed.x0 * sx,
        rect.y0 - transformed.y0 * sy,
    )
}

fn plain_stream(content: &[u8]) -> Object {
    let mut d = Dict::new();
    d.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(content.len()).unwrap_or(0)),
    );
    Object::Stream {
        dict: d,
        raw: content.to_vec(),
    }
}
