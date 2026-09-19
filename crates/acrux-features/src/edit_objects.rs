//! Édition des objets d'une page : déplacer, redimensionner, pivoter,
//! recadrer, réordonner, supprimer, remplacer.
//!
//! C'est le pendant de l'outil « Modifier le PDF » d'Acrobat pour tout ce qui
//! n'est pas du texte — le texte, lui, a son module
//! ([`edit_text`](crate::edit_text)), parce qu'il demande de réencoder des
//! chaînes avec la police en place. Ici on manipule des **choses dessinées** :
//! images, dessins vectoriels, groupes, dégradés, et les blocs de texte pris
//! comme des blocs.
//!
//! # Le principe : la chirurgie, pas la régénération
//!
//! Un éditeur PDF médiocre régénère la page : il relit tout, reconstruit son
//! idée du contenu, et réécrit un flux neuf. Tout bouge, même ce à quoi on
//! n'a pas touché — les nombres changent d'arrondi, les opérateurs d'ordre,
//! les polices de sous-ensemble.
//!
//! Ici, chaque objet est repéré par sa **plage d'octets** dans le flux décodé
//! ([`scan`]). Déplacer un objet, c'est insérer `q … cm` devant sa plage et
//! `Q` derrière. Le supprimer, c'est retirer sa plage. Tout le reste du flux
//! est recopié **octet pour octet** : mêmes nombres, mêmes espaces, mêmes
//! commentaires. Une page dont on déplace une image garde le reste
//! rigoureusement identique, et c'est vérifié au pixel près par les tests.
//!
//! # Comment une transformation est appliquée
//!
//! La matrice demandée est exprimée en **espace page**, comme on la pense :
//! « décale de 10 points vers la droite » s'écrit `Matrix::translate(10, 0)`,
//! quelle que soit la matrice sous laquelle l'objet est dessiné.
//!
//! L'objet, lui, est tracé sous une matrice `C`. Pour qu'un point `p` de son
//! espace arrive à `p · C · M` au lieu de `p · C`, il faut insérer, juste
//! avant lui, une matrice `N` telle que `p · N · C = p · C · M`, donc
//! **`N = C · M · C⁻¹`**. C'est une conjugaison : elle vaut quelle que soit
//! l'imbrication des `q`/`Q` et des `cm` au-dessus.
//!
//! Le bloc inséré est `q N cm … Q` pour tout ce qui ne laisse aucun état
//! derrière lui (une image, un formulaire, un tracé). Pour un bloc de texte,
//! qui règle la police et l'interligne et dont un bloc suivant peut hériter,
//! on écrit plutôt `N cm … N⁻¹ cm` : seule la matrice est touchée, et elle
//! est rendue exactement.
//!
//! ```no_run
//! use acrux_core::Matrix;
//! use acrux_document::{collect_pages, Document};
//! use acrux_features::edit_objects::{self, Edit};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let doc = Document::load("plaquette.pdf")?;
//! let pages = collect_pages(&doc)?;
//! let objects = edit_objects::list(&doc, &pages[0])?;
//! // Le logo remonte de 12 points et grossit d'un dixième.
//! let logo = &objects[0];
//! edit_objects::apply(
//!     &doc,
//!     &pages[0],
//!     &[Edit::Transform {
//!         index: logo.index,
//!         matrix: edit_objects::around(logo.bbox, Matrix::scale(1.1, 1.1))
//!             .then(&Matrix::translate(0.0, 12.0)),
//!     }],
//! )?;
//! std::fs::write("plaquette-modifiee.pdf", doc.save_incremental()?)?;
//! # Ok(())
//! # }
//! ```

pub mod scan;

use std::fmt::Write as _;

use acrux_core::{Error, Matrix, Rect, Result};
use acrux_document::{collect_pages, Dict, Document, Name, Object, Page};

use crate::edit_text::set_page_content;
use acrux_render::page::page_content;

pub use scan::{Kind, PageObject};

/// Inventaire des objets dessinés par une page, dans l'ordre de tracé.
///
/// Le premier de la liste est le plus au fond, le dernier au premier plan —
/// l'ordre du flux de contenu est l'ordre de superposition.
///
/// # Errors
/// Flux de contenu illisible.
pub fn list(doc: &Document, page: &Page) -> Result<Vec<PageObject>> {
    let content = page_content(doc, page);
    let mut scanned = scan::scan(doc, page, &content)?;
    fill_text_boxes(doc, page, &mut scanned.objects);
    Ok(scanned.objects)
}

/// Complète la boîte des blocs de texte avec celle des glyphes extraits.
///
/// Le balayage des objets connaît la plage d'**opérations** de chaque
/// `BT … ET` ; l'extraction de texte connaît la boîte exacte de chaque glyphe,
/// métriques de police comprises. `edit_text::glyph_operations` fait le pont
/// entre les deux, et chaque glyphe rejoint le bloc qui l'a dessiné — un
/// appariement exact, pas une approximation géométrique.
fn fill_text_boxes(doc: &Document, page: &Page, objects: &mut [PageObject]) {
    if !objects.iter().any(|o| o.kind == Kind::Text) {
        return;
    }
    let Ok(glyphs) = crate::edit_text::glyph_operations(doc, page) else {
        return;
    };
    for object in objects.iter_mut().filter(|o| o.kind == Kind::Text) {
        let (from, to) = object.ops;
        let mut bbox: Option<Rect> = None;
        for (rect, op) in &glyphs {
            if *op >= from && *op < to {
                bbox = Some(match bbox {
                    Some(b) => b.union(rect),
                    None => *rect,
                });
            }
        }
        if let Some(b) = bbox {
            object.bbox = b;
        }
    }
}

/// Place dans l'ordre de tracé.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// Tout devant.
    Front,
    /// Tout derrière.
    Back,
    /// Un cran devant.
    Forward,
    /// Un cran derrière.
    Backward,
}

/// Une modification à appliquer à un objet.
#[derive(Debug, Clone)]
pub enum Edit {
    /// Transformer l'objet, la matrice étant exprimée en espace page.
    Transform {
        /// Index dans l'inventaire.
        index: usize,
        /// Transformation voulue.
        matrix: Matrix,
    },
    /// Retirer l'objet de la page.
    Delete {
        /// Index dans l'inventaire.
        index: usize,
    },
    /// Ne montrer de l'objet que ce qui tombe dans ce rectangle de la page.
    ///
    /// C'est le recadrage d'Acrobat : rien n'est perdu, une découpe est posée.
    Clip {
        /// Index dans l'inventaire.
        index: usize,
        /// Rectangle visible, en coordonnées de page.
        rect: Rect,
    },
    /// Changer la place de l'objet dans l'ordre de superposition.
    Arrange {
        /// Index dans l'inventaire.
        index: usize,
        /// Où l'envoyer.
        to: Order,
    },
    /// Remplacer l'image d'un objet image par une autre (PNG ou JPEG).
    ReplaceImage {
        /// Index dans l'inventaire.
        index: usize,
        /// Octets du fichier image.
        data: Vec<u8>,
    },
}

impl Edit {
    /// Index de l'objet visé.
    #[must_use]
    pub fn index(&self) -> usize {
        match self {
            Edit::Transform { index, .. }
            | Edit::Delete { index }
            | Edit::Clip { index, .. }
            | Edit::Arrange { index, .. }
            | Edit::ReplaceImage { index, .. } => *index,
        }
    }
}

/// Matrice qui applique `m` **autour du centre** d'un rectangle.
///
/// Sans cela, agrandir un objet le fait aussi glisser : la mise à l'échelle se
/// fait depuis l'origine de la page, pas depuis l'objet. C'est l'erreur que
/// font les éditeurs approximatifs, et elle se voit tout de suite.
#[must_use]
pub fn around(rect: Rect, m: Matrix) -> Matrix {
    let (cx, cy) = (
        f64::midpoint(rect.x0, rect.x1),
        f64::midpoint(rect.y0, rect.y1),
    );
    Matrix::translate(-cx, -cy)
        .then(&m)
        .then(&Matrix::translate(cx, cy))
}

/// Matrice qui amène `from` exactement sur `to`, proportions non gardées.
#[must_use]
pub fn fit(from: Rect, to: Rect) -> Matrix {
    let sx = if from.width() > 1e-9 {
        to.width() / from.width()
    } else {
        1.0
    };
    let sy = if from.height() > 1e-9 {
        to.height() / from.height()
    } else {
        1.0
    };
    Matrix::translate(-from.x0, -from.y0)
        .then(&Matrix::scale(sx, sy))
        .then(&Matrix::translate(to.x0, to.y0))
}

/// Applique des modifications à une page et rend le nombre d'objets touchés.
///
/// Les modifications portent toutes sur l'inventaire **d'avant** : les index
/// ne se décalent pas au fur et à mesure, on peut donc en enchaîner plusieurs
/// sans recompter.
///
/// # Errors
/// Index inconnu, objet non déplaçable dans l'ordre (parce qu'il est sous une
/// découpe), image de remplacement illisible, ou page non indirecte.
pub fn apply(doc: &Document, page: &Page, edits: &[Edit]) -> Result<usize> {
    if edits.is_empty() {
        return Ok(0);
    }
    let content = page_content(doc, page);
    let mut scanned = scan::scan(doc, page, &content)?;
    fill_text_boxes(doc, page, &mut scanned.objects);
    let objects = scanned.objects;
    let tail = scanned.tail;

    // Une liste de morceaux : les octets d'origine, et ce qu'on insère.
    let mut wraps: Vec<Wrap> = Vec::new();
    let mut moved: Vec<(usize, Order)> = Vec::new();
    let mut touched = 0usize;

    for edit in edits {
        let index = edit.index();
        let object = objects
            .get(index)
            .ok_or_else(|| Error::Corrupt(format!("objet {index} inexistant")))?
            .clone();
        match edit {
            Edit::Transform { matrix, .. } => {
                wraps.push(transform_wrap(&object, *matrix)?);
                touched += 1;
            }
            Edit::Delete { .. } => {
                wraps.push(Wrap {
                    range: object.range,
                    before: Vec::new(),
                    after: Vec::new(),
                    drop: true,
                });
                touched += 1;
            }
            Edit::Clip { rect, .. } => {
                wraps.push(clip_wrap(&object, *rect));
                touched += 1;
            }
            Edit::Arrange { to, .. } => {
                if !object.movable_in_order() {
                    return Err(Error::Unsupported(format!(
                        "l'objet {index} est sous une découpe : le sortir de son groupe \
                         changerait ce qui se voit"
                    )));
                }
                moved.push((index, *to));
                touched += 1;
            }
            Edit::ReplaceImage { data, .. } => {
                replace_image(doc, page, &object, data)?;
                touched += 1;
            }
        }
    }

    let rewritten = rebuild(&content, &objects, tail, &wraps, &moved)?;
    if rewritten != content {
        set_page_content(doc, page, rewritten)?;
    }
    Ok(touched)
}

/// Un encadrement d'octets : ce qu'on met avant, après, et si on jette.
#[derive(Debug, Clone)]
struct Wrap {
    range: (usize, usize),
    before: Vec<u8>,
    after: Vec<u8>,
    drop: bool,
}

/// Encadrement qui applique une transformation d'espace page à un objet.
fn transform_wrap(object: &PageObject, matrix: Matrix) -> Result<Wrap> {
    let inverse = object.matrix.invert().ok_or_else(|| {
        Error::Unsupported(format!(
            "l'objet {} est dessiné sous une matrice dégénérée",
            object.index
        ))
    })?;
    // N = C · M · C⁻¹ : la transformation voulue, vue depuis l'espace de
    // l'objet (voir l'en-tête du module).
    let n = object.matrix.then(&matrix).then(&inverse);
    if object.kind == Kind::Text {
        // Un bloc de texte laisse derrière lui la police et l'interligne :
        // un `Q` les reprendrait. On rend donc la matrice à la main.
        let back = n
            .invert()
            .ok_or_else(|| Error::Unsupported("transformation non inversible".into()))?;
        return Ok(Wrap {
            range: object.range,
            before: format!("{} cm\n", cm(n)).into_bytes(),
            after: format!("\n{} cm", cm(back)).into_bytes(),
            drop: false,
        });
    }
    Ok(Wrap {
        range: object.range,
        before: format!("q {} cm\n", cm(n)).into_bytes(),
        after: b"\nQ".to_vec(),
        drop: false,
    })
}

/// Encadrement qui découpe un objet à un rectangle de la page.
///
/// La découpe est exprimée **dans l'espace de l'objet**, pas en espace page :
/// on transforme les quatre coins du rectangle par l'inverse de la matrice du
/// tracé, et on les pose tels quels. Faire l'inverse — remettre la matrice à
/// l'identité, poser le rectangle, puis rétablir la matrice — reviendrait à
/// écrire `C⁻¹` puis `C` avec six décimales chacun : le produit ne serait plus
/// exactement `C`, l'image serait rééchantillonnée d'un demi-millième de point,
/// et **tous** ses pixels changeraient. Ici la matrice de l'objet n'est jamais
/// touchée, et ce qui reste visible est identique au bit près.
///
/// Les quatre coins sont posés en polygone et non en `re` : une matrice qui
/// tourne ou cisaille fait d'un rectangle un parallélogramme.
fn clip_wrap(object: &PageObject, rect: Rect) -> Wrap {
    let Some(inverse) = object.matrix.invert() else {
        // Matrice dégénérée : l'objet ne dessine rien, la découpe non plus.
        return Wrap {
            range: object.range,
            before: Vec::new(),
            after: Vec::new(),
            drop: false,
        };
    };
    let corners = [
        (rect.x0, rect.y0),
        (rect.x1, rect.y0),
        (rect.x1, rect.y1),
        (rect.x0, rect.y1),
    ];
    let mut before = String::from("q ");
    for (index, (x, y)) in corners.into_iter().enumerate() {
        let p = inverse.apply(acrux_core::Point::new(x, y));
        let _ = write!(
            before,
            "{} {} {} ",
            num(p.x),
            num(p.y),
            if index == 0 { "m" } else { "l" }
        );
    }
    before.push_str(
        "h W n
",
    );
    Wrap {
        range: object.range,
        before: before.into_bytes(),
        after: b"
Q"
        .to_vec(),
        drop: false,
    }
}

/// Reconstruit le flux : encadrements, suppressions, puis déplacements.
fn rebuild(
    content: &[u8],
    objects: &[PageObject],
    tail: Matrix,
    wraps: &[Wrap],
    moved: &[(usize, Order)],
) -> Result<Vec<u8>> {
    // Les objets déplacés sont retirés de leur place puis réémis ailleurs,
    // avec de quoi se redessiner à l'identique.
    let mut drops: Vec<(usize, usize)> = Vec::new();
    let mut inserts: Vec<(usize, Vec<u8>)> = Vec::new();
    for (index, order) in moved {
        let object = &objects[*index];
        drops.push(object.range);
        let (at, ambient) = insert_site(content, objects, tail, *index, *order);
        inserts.push((at, standalone(content, object, ambient)));
    }

    let mut out = Vec::with_capacity(content.len() + 64);
    let mut cursor = 0usize;
    // Les bornes à traiter, dans l'ordre du flux.
    let mut marks: Vec<usize> = wraps
        .iter()
        .map(|w| w.range.0)
        .chain(drops.iter().map(|d| d.0))
        .chain(inserts.iter().map(|(at, _)| *at))
        .collect();
    marks.sort_unstable();
    marks.dedup();

    for mark in marks {
        if mark < cursor {
            return Err(Error::Unsupported(
                "deux modifications se recouvrent : les appliquer une par une".into(),
            ));
        }
        out.extend_from_slice(&content[cursor..mark]);
        cursor = mark;
        for (_, piece) in inserts.iter().filter(|(at, _)| *at == mark) {
            out.extend_from_slice(piece);
        }
        if let Some(range) = drops.iter().find(|d| d.0 == mark) {
            cursor = range.1;
            continue;
        }
        if let Some(wrap) = wraps.iter().find(|w| w.range.0 == mark) {
            out.extend_from_slice(&wrap.before);
            if !wrap.drop {
                out.extend_from_slice(&content[wrap.range.0..wrap.range.1]);
            }
            out.extend_from_slice(&wrap.after);
            cursor = wrap.range.1;
        }
    }
    out.extend_from_slice(&content[cursor..]);
    Ok(out)
}

/// Les octets qui redessinent un objet **n'importe où**, au premier niveau.
///
/// L'objet ne peut pas simplement être recopié ailleurs : il compte sur la
/// matrice et sur les couleurs en vigueur à sa place d'origine. On les rend
/// donc explicites, dans un `q … Q` qui ne déborde sur rien.
///
/// `ambient` est la matrice déjà en vigueur là où l'on réémet. La matrice
/// écrite est donc `matrice de l'objet × ambiante⁻¹` : beaucoup de
/// producteurs — Chrome le premier — posent au tout début du flux un `cm` de
/// mise à l'échelle qu'ils ne referment jamais, et réémettre la matrice
/// absolue l'appliquerait **deux fois**, envoyant l'objet hors de la page.
fn standalone(content: &[u8], object: &PageObject, ambient: Matrix) -> Vec<u8> {
    let relative = match ambient.invert() {
        Some(back) => object.matrix.then(&back),
        None => object.matrix,
    };
    let mut out = Vec::new();
    out.extend_from_slice(b"\nq ");
    out.extend_from_slice(cm(relative).as_bytes());
    out.extend_from_slice(b" cm\n");
    out.extend_from_slice(&object.state);
    out.extend_from_slice(&content[object.range.0..object.range.1]);
    out.extend_from_slice(b"\nQ\n");
    out
}

/// Où réémettre un objet déplacé dans l'ordre de tracé, et sous quelle
/// matrice ambiante.
fn insert_site(
    content: &[u8],
    objects: &[PageObject],
    tail: Matrix,
    index: usize,
    order: Order,
) -> (usize, Matrix) {
    // Les voisins de même niveau, ceux entre lesquels l'objet peut se glisser.
    let siblings: Vec<&PageObject> = objects.iter().filter(|o| o.depth == 0).collect();
    let position = siblings.iter().position(|o| o.index == index);
    match order {
        // Au tout début du flux, rien n'a encore été posé : la matrice y est
        // l'identité.
        Order::Back => (0, Matrix::IDENTITY),
        Order::Front => (content.len(), tail),
        Order::Forward => match position.and_then(|p| siblings.get(p + 1)) {
            Some(next) => (next.range.1, next.outer),
            None => (content.len(), tail),
        },
        Order::Backward => match position.and_then(|p| p.checked_sub(1)).map(|p| siblings[p]) {
            Some(previous) => (previous.range.0, previous.outer),
            None => (0, Matrix::IDENTITY),
        },
    }
}

/// Remplace l'image d'un objet image.
fn replace_image(doc: &Document, page: &Page, object: &PageObject, data: &[u8]) -> Result<()> {
    if object.kind != Kind::Image {
        return Err(Error::Unsupported(format!(
            "l'objet {} n'est pas une image",
            object.index
        )));
    }
    let Some(name) = &object.name else {
        return Err(Error::Corrupt("image sans nom de ressource".into()));
    };
    let image = crate::stamp::image::decode(data)?;
    let reference = image.write(doc);
    let page_ref = page
        .reference
        .ok_or_else(|| Error::Corrupt("la page doit être un objet indirect".into()))?;
    let mut page_dict = page.dict.clone();
    let mut resources = doc
        .dict_get(&page_dict, "Resources")
        .ok()
        .flatten()
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_default();
    let mut xobjects = doc
        .dict_get(&resources, "XObject")
        .ok()
        .flatten()
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_default();
    xobjects.insert(Name::new(name), Object::Reference(reference));
    resources.insert(Name::new("XObject"), Object::Dict(xobjects));
    page_dict.insert(Name::new("Resources"), Object::Dict(resources));
    doc.set(page_ref, Object::Dict(page_dict));
    Ok(())
}

/// Les six nombres d'un `cm`.
fn cm(m: Matrix) -> String {
    let mut out = String::with_capacity(48);
    for (index, v) in [m.a, m.b, m.c, m.d, m.e, m.f].into_iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{}", num(v));
    }
    out
}

/// Nombre écrit court, six décimales au plus.
fn num(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let mut s = format!("{v:.6}");
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

// ---------------------------------------------------------------------------
// Raccourcis de haut niveau.
// ---------------------------------------------------------------------------

/// Déplace un objet de `dx`, `dy` points.
///
/// # Errors
/// Voir [`apply`].
pub fn translate(doc: &Document, page: &Page, index: usize, dx: f64, dy: f64) -> Result<usize> {
    apply(
        doc,
        page,
        &[Edit::Transform {
            index,
            matrix: Matrix::translate(dx, dy),
        }],
    )
}

/// Met un objet à l'échelle autour de son centre.
///
/// # Errors
/// Voir [`apply`].
pub fn scale(doc: &Document, page: &Page, index: usize, sx: f64, sy: f64) -> Result<usize> {
    let objects = list(doc, page)?;
    let object = objects
        .get(index)
        .ok_or_else(|| Error::Corrupt(format!("objet {index} inexistant")))?;
    let matrix = around(object.bbox, Matrix::scale(sx, sy));
    apply(doc, page, &[Edit::Transform { index, matrix }])
}

/// Pivote un objet autour de son centre, en degrés, sens direct.
///
/// # Errors
/// Voir [`apply`].
pub fn rotate(doc: &Document, page: &Page, index: usize, degrees: f64) -> Result<usize> {
    let objects = list(doc, page)?;
    let object = objects
        .get(index)
        .ok_or_else(|| Error::Corrupt(format!("objet {index} inexistant")))?;
    let matrix = around(object.bbox, Matrix::rotate(degrees.to_radians()));
    apply(doc, page, &[Edit::Transform { index, matrix }])
}

/// Amène un objet exactement dans un rectangle.
///
/// # Errors
/// Voir [`apply`].
pub fn place(doc: &Document, page: &Page, index: usize, rect: Rect) -> Result<usize> {
    let objects = list(doc, page)?;
    let object = objects
        .get(index)
        .ok_or_else(|| Error::Corrupt(format!("objet {index} inexistant")))?;
    let matrix = fit(object.bbox, rect);
    apply(doc, page, &[Edit::Transform { index, matrix }])
}

/// Façon d'aligner plusieurs objets entre eux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// Bords gauches.
    Left,
    /// Centres, horizontalement.
    CenterX,
    /// Bords droits.
    Right,
    /// Bords hauts.
    Top,
    /// Centres, verticalement.
    CenterY,
    /// Bords bas.
    Bottom,
}

/// Aligne des objets entre eux, sur l'enveloppe de leur ensemble.
///
/// C'est le « Aligner » d'Acrobat : chaque objet ne fait que **glisser**, il
/// ne change ni de taille ni de forme.
///
/// # Errors
/// Index inconnu, ou voir [`apply`].
pub fn align(doc: &Document, page: &Page, indices: &[usize], how: Align) -> Result<usize> {
    let objects = list(doc, page)?;
    let mut chosen = Vec::new();
    for &index in indices {
        chosen.push(
            objects
                .get(index)
                .ok_or_else(|| Error::Corrupt(format!("objet {index} inexistant")))?,
        );
    }
    if chosen.len() < 2 {
        return Ok(0);
    }
    let envelope = chosen
        .iter()
        .skip(1)
        .fold(chosen[0].bbox, |acc, o| acc.union(&o.bbox));
    let edits: Vec<Edit> = chosen
        .iter()
        .map(|o| {
            let (dx, dy) = match how {
                Align::Left => (envelope.x0 - o.bbox.x0, 0.0),
                Align::Right => (envelope.x1 - o.bbox.x1, 0.0),
                Align::CenterX => (
                    f64::midpoint(envelope.x0, envelope.x1) - f64::midpoint(o.bbox.x0, o.bbox.x1),
                    0.0,
                ),
                Align::Bottom => (0.0, envelope.y0 - o.bbox.y0),
                Align::Top => (0.0, envelope.y1 - o.bbox.y1),
                Align::CenterY => (
                    0.0,
                    f64::midpoint(envelope.y0, envelope.y1) - f64::midpoint(o.bbox.y0, o.bbox.y1),
                ),
            };
            Edit::Transform {
                index: o.index,
                matrix: Matrix::translate(dx, dy),
            }
        })
        .collect();
    apply(doc, page, &edits)
}

/// Inventaire des objets de toutes les pages (utilitaire pour la ligne de
/// commande).
///
/// # Errors
/// Document illisible.
pub fn list_all(doc: &Document) -> Result<Vec<(usize, Vec<PageObject>)>> {
    let mut out = Vec::new();
    for (index, page) in collect_pages(doc)?.iter().enumerate() {
        out.push((index, list(doc, page)?));
    }
    Ok(out)
}

/// Dictionnaire des ressources d'une page, résolu.
#[must_use]
pub fn resources(doc: &Document, page: &Page) -> Dict {
    doc.dict_get(&page.dict, "Resources")
        .ok()
        .flatten()
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::float_cmp)]
mod tests {
    use super::{around, cm, fit, num};
    use acrux_core::{Matrix, Point, Rect};

    #[test]
    fn les_nombres_sont_ecrits_court() {
        assert_eq!(num(1.0), "1");
        assert_eq!(num(-0.0), "0");
        assert_eq!(num(1.500_000), "1.5");
        assert_eq!(num(0.123_456_7), "0.123457");
        assert_eq!(num(f64::NAN), "0");
    }

    #[test]
    fn un_cm_a_six_nombres() {
        assert_eq!(cm(Matrix::IDENTITY), "1 0 0 1 0 0");
        assert_eq!(cm(Matrix::translate(10.0, -4.5)), "1 0 0 1 10 -4.5");
    }

    #[test]
    fn autour_du_centre_ne_deplace_pas() {
        let r = Rect::new(100.0, 200.0, 300.0, 240.0);
        let m = around(r, Matrix::scale(2.0, 2.0));
        let center = Point::new(200.0, 220.0);
        let moved = m.apply(center);
        assert!((moved.x - center.x).abs() < 1e-9);
        assert!((moved.y - center.y).abs() < 1e-9);
        // Le rectangle double bien de taille.
        let grown = m.transform_rect(&r);
        assert!((grown.width() - 400.0).abs() < 1e-9);
        assert!((grown.height() - 80.0).abs() < 1e-9);
    }

    #[test]
    fn placer_amene_exactement_sur_la_cible() {
        let from = Rect::new(10.0, 10.0, 30.0, 20.0);
        let to = Rect::new(100.0, 400.0, 200.0, 450.0);
        let got = fit(from, to).transform_rect(&from);
        for (a, b) in [
            (got.x0, to.x0),
            (got.y0, to.y0),
            (got.x1, to.x1),
            (got.y1, to.y1),
        ] {
            assert!((a - b).abs() < 1e-9, "{a} contre {b}");
        }
    }

    #[test]
    fn la_conjugaison_applique_bien_en_espace_page() {
        // Un objet dessiné sous une matrice quelconque.
        let c = Matrix::new(2.0, 0.3, -0.4, 1.5, 30.0, 70.0);
        let m = Matrix::translate(12.0, -5.0);
        let n = c.then(&m).then(&c.invert().unwrap());
        // Le point p de l'espace de l'objet doit arriver à p·C·M.
        let p = Point::new(0.25, 0.75);
        let expected = m.apply(c.apply(p));
        let got = c.apply(n.apply(p));
        assert!((got.x - expected.x).abs() < 1e-9, "{got:?} {expected:?}");
        assert!((got.y - expected.y).abs() < 1e-9);
    }
}
