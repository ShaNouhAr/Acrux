//! Rendu d'une page complète : matrice de base (échelle, retournement,
//! rotation), contenu, puis annotations (§12.5.5).

// `render_page` enchaîne linéairement les étapes de §12.5.5 : la découper
// nuirait à la lecture.
#![allow(clippy::too_many_lines)]

use acrux_core::{Matrix, Rect};
use acrux_document::{Dict, Document, Name, Object, Page};
use acrux_graphics::Bitmap;

use crate::interpreter::{RenderOptions, Renderer};
use crate::state::GraphicsState;

/// Résultat du rendu d'une page.
#[derive(Debug)]
pub struct RenderedPage {
    /// Image RVBA prémultipliée.
    pub bitmap: Bitmap,
    /// Avertissements du moteur.
    pub warnings: Vec<String>,
    /// Matrice espace PDF → pixels utilisée.
    pub base_ctm: Matrix,
}

/// Dimensions en pixels d'une page à une échelle donnée (pixels par point),
/// rotation comprise.
#[must_use]
pub fn page_pixel_size(doc: &Document, page: &Page, scale: f64) -> (u32, u32) {
    page_pixel_size_rotated(doc, page, scale, 0)
}

/// Comme [`page_pixel_size`], la page tournée **en plus** de `extra` degrés
/// dans le sens horaire (un multiple de 90, négatif permis).
///
/// C'est la rotation de la **vue** d'un lecteur (Ctrl+Maj+Plus dans
/// Acrobat) : elle change ce qu'on voit, jamais le document. Elle ne passe
/// pas par [`RenderOptions`], que l'interpréteur construit par littéral
/// complet pour chaque groupe de transparence et chaque motif : une rotation
/// n'y aurait aucun sens, elle ne vaut que pour la page entière.
#[must_use]
pub fn page_pixel_size_rotated(doc: &Document, page: &Page, scale: f64, extra: i32) -> (u32, u32) {
    pixel_size(
        &page.crop_box(doc),
        scale,
        add_rotation(page.rotate(doc), extra),
    )
}

/// Rotation d'une page (`/Rotate`, déjà ramenée entre 0 et 359) augmentée de
/// `extra` degrés, ramenée elle aussi entre 0 et 359.
///
/// La normalisation n'est pas une coquetterie : [`base_matrix`] ne reconnaît
/// que 90, 180 et 270, et une page à `/Rotate 270` tournée de 180 donnerait
/// sinon 450 — dessinée droite, mais à la taille d'une page couchée.
#[must_use]
pub fn add_rotation(rotate: i32, extra: i32) -> i32 {
    (rotate + extra).rem_euclid(360)
}

/// Dimensions en pixels d'une boîte à une échelle et une rotation données.
fn pixel_size(b: &Rect, scale: f64, rotate: i32) -> (u32, u32) {
    let w = (b.width() * scale).round().max(1.0);
    let h = (b.height() * scale).round().max(1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (w, h) = (w.min(65535.0) as u32, h.min(65535.0) as u32);
    match rotate {
        90 | 270 => (h, w),
        _ => (w, h),
    }
}

/// Matrice de base : espace utilisateur PDF → pixels (origine en haut à gauche).
#[must_use]
pub fn base_matrix(bounds: &Rect, scale: f64, rotate: i32, width: u32, height: u32) -> Matrix {
    // Retournement de l'axe Y et mise à l'échelle : (x0, y1) → (0, 0).
    let flip = Matrix::new(
        scale,
        0.0,
        0.0,
        -scale,
        -bounds.x0 * scale,
        bounds.y1 * scale,
    );
    let (w, h) = (f64::from(width), f64::from(height));
    let rot = match rotate {
        90 => Matrix::new(0.0, 1.0, -1.0, 0.0, w, 0.0),
        180 => Matrix::new(-1.0, 0.0, 0.0, -1.0, w, h),
        270 => Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, h),
        _ => Matrix::IDENTITY,
    };
    flip.then(&rot)
}

/// Rend une page à `scale` pixels par point (72 dpi = 1.0).
#[must_use]
pub fn render_page(
    doc: &Document,
    page: &Page,
    scale: f64,
    options: &RenderOptions,
) -> RenderedPage {
    render_page_rotated(doc, page, scale, 0, options)
}

/// Comme [`render_page`], la page tournée en plus de `extra` degrés dans le
/// sens horaire : la rotation de la vue (voir [`page_pixel_size_rotated`]).
/// Le document n'est pas touché.
#[must_use]
pub fn render_page_rotated(
    doc: &Document,
    page: &Page,
    scale: f64,
    extra: i32,
    options: &RenderOptions,
) -> RenderedPage {
    let bounds = page.crop_box(doc);
    let rotate = add_rotation(page.rotate(doc), extra);
    let (width, height) = pixel_size(&bounds, scale, rotate);
    let base_ctm = base_matrix(&bounds, scale, rotate, width, height);
    let mut renderer = Renderer::new(doc, width, height, options.clone());
    let resources = doc
        .dict_get(&page.dict, "Resources")
        .ok()
        .flatten()
        .and_then(|r| r.as_dict().cloned())
        .unwrap_or_default();
    let content = page_content(doc, page);
    renderer.run(&content, &resources, GraphicsState::new(base_ctm));
    if options.annotations {
        draw_annotations(doc, page, &mut renderer, base_ctm);
    }
    let warnings = renderer.warnings().to_vec();
    RenderedPage {
        bitmap: renderer.into_bitmap(),
        warnings,
        base_ctm,
    }
}

/// Contenu de la page : concaténation des flux `/Contents` séparés par un
/// espacement (§7.7.3.3 : la coupure peut tomber au milieu d'une opération).
#[must_use]
pub fn page_content(doc: &Document, page: &Page) -> Vec<u8> {
    let mut out = Vec::new();
    let Some(contents) = page.dict.get(&Name::new("Contents")) else {
        return out;
    };
    let Ok(resolved) = doc.resolve(contents) else {
        return out;
    };
    match &*resolved {
        Object::Array(items) => {
            for item in items {
                if let Ok(d) = doc.stream_data(item) {
                    out.extend_from_slice(&d.data);
                    out.push(b'\n');
                }
            }
        }
        Object::Stream { .. } => {
            if let Ok(d) = doc.stream_data(&resolved) {
                out = d.data;
            }
        }
        _ => {}
    }
    out
}

const ANNOT_HIDDEN: i64 = 1 << 1;
const ANNOT_NOVIEW: i64 = 1 << 5;

/// Dessine les apparences normales des annotations (§12.5.5, algorithme 8.1).
fn draw_annotations(doc: &Document, page: &Page, renderer: &mut Renderer<'_>, base_ctm: Matrix) {
    let Some(annots) = page.dict.get(&Name::new("Annots")) else {
        return;
    };
    let Ok(annots) = doc.resolve(annots) else {
        return;
    };
    let Some(list) = annots.as_array() else {
        return;
    };
    for a in list.iter().take(10_000) {
        let Ok(annot) = doc.resolve(a) else { continue };
        let Some(annot) = annot.as_dict() else {
            continue;
        };
        draw_one(doc, annot, renderer, base_ctm);
    }
}

/// Vrai pour une réponse (une note `/Text` qui désigne son commentaire par
/// `/IRT`, §12.5.6.2) : elle partage le rectangle de ce commentaire, et la
/// dessiner poserait une seconde icône sur la première. Les lecteurs la
/// montrent dans le fil du commentaire, pas sur la page.
fn is_reply(annot: &Dict, subtype: Option<&[u8]>) -> bool {
    subtype == Some(b"Text")
        && annot.contains_key(&Name::new("IRT"))
        && annot
            .get(&Name::new("RT"))
            .and_then(Object::as_name)
            .is_none_or(|n| n.0 != b"Group")
}

/// Dessine une annotation, son apparence posée dans son rectangle (§12.5.5,
/// algorithme 8.1), ou synthétisée quand elle n'en a pas. `base_ctm` mène
/// de l'espace de la page aux pixels.
fn draw_one(doc: &Document, annot: &Dict, renderer: &mut Renderer<'_>, base_ctm: Matrix) {
    let subtype = annot
        .get(&Name::new("Subtype"))
        .and_then(Object::as_name)
        .map(|n| n.0.clone());
    // Les fenêtres contextuelles ne sont dessinées qu'ouvertes, à l'écran.
    if subtype.as_deref() == Some(b"Popup") || is_reply(annot, subtype.as_deref()) {
        return;
    }
    let flags = doc
        .dict_get(annot, "F")
        .ok()
        .flatten()
        .and_then(|o| o.as_i64())
        .unwrap_or(0);
    if flags & (ANNOT_HIDDEN | ANNOT_NOVIEW) != 0 {
        return;
    }
    let Some(rect) = annot_rect(doc, annot) else {
        return;
    };
    let Some(stream) = normal_appearance(doc, annot) else {
        // Annotation sans flux d'apparence : on en synthétise une comme
        // Acrobat (bordure des liens, formes, marquages de texte, encre).
        match subtype.as_deref() {
            Some(b"Link") => draw_link_border(doc, annot, &rect, renderer, base_ctm),
            Some(kind) => draw_default_appearance(doc, annot, kind, &rect, renderer, base_ctm),
            None => {}
        }
        return;
    };
    let Object::Stream { dict, .. } = &stream else {
        return;
    };
    // Algorithme 8.1 : BBox transformée par Matrix, ajustée dans Rect.
    let bbox = annot_numbers(doc, dict, "BBox").unwrap_or_default();
    let m = annot_numbers(doc, dict, "Matrix").unwrap_or_default();
    let matrix = if m.len() == 6 {
        Matrix::new(m[0], m[1], m[2], m[3], m[4], m[5])
    } else {
        Matrix::IDENTITY
    };
    let a_matrix = if bbox.len() == 4 {
        let tb = matrix.transform_rect(&Rect::new(bbox[0], bbox[1], bbox[2], bbox[3]));
        let sx = if tb.width() > 1e-9 {
            rect.width() / tb.width()
        } else {
            1.0
        };
        let sy = if tb.height() > 1e-9 {
            rect.height() / tb.height()
        } else {
            1.0
        };
        Matrix::new(sx, 0.0, 0.0, sy, rect.x0 - tb.x0 * sx, rect.y0 - tb.y0 * sy)
    } else {
        Matrix::IDENTITY
    };
    let ctm = a_matrix.then(&base_ctm);
    // Le formulaire est dessiné via `Do` sur un dictionnaire de ressources synthétique.
    let mut xobjects = Dict::new();
    xobjects.insert(Name::new("AkAnnot"), stream.clone());
    let mut synthetic = Dict::new();
    synthetic.insert(Name::new("XObject"), Object::Dict(xobjects));
    renderer.run(b"/AkAnnot Do", &synthetic, GraphicsState::new(ctm));
}

/// `/Rect` d'une annotation, coins dans l'ordre ; `None` s'il est illisible.
fn annot_rect(doc: &Document, annot: &Dict) -> Option<Rect> {
    let r = annot_numbers(doc, annot, "Rect").filter(|r| r.len() == 4)?;
    Some(Rect::new(
        r[0].min(r[2]),
        r[1].min(r[3]),
        r[0].max(r[2]),
        r[1].max(r[3]),
    ))
}

/// Rend **une seule** annotation de la page — celle de rang `index` dans
/// `/Annots` — sur fond transparent, déplacée et mise à l'échelle de son
/// rectangle vers `to` (coordonnées de page), telle qu'elle apparaîtrait à
/// l'écran à `scale` pixels par point, la page tournée de `rotate` degrés
/// (sa rotation affichée, vue comprise).
///
/// L'image a la taille de `to` à l'écran. C'est l'aperçu d'un geste :
/// l'annotation qu'on déplace ou qu'on agrandit suit le pointeur **telle
/// qu'elle est**, sans que la page entière soit rendue à chaque mouvement.
/// `None` si l'annotation n'existe pas ou n'a pas de rectangle.
#[must_use]
pub fn render_annotation(
    doc: &Document,
    page: &Page,
    index: usize,
    scale: f64,
    rotate: i32,
    to: Rect,
) -> Option<Bitmap> {
    let annots = doc.resolve(page.dict.get(&Name::new("Annots"))?).ok()?;
    let annot = doc.resolve(annots.as_array()?.get(index)?).ok()?;
    let annot = annot.as_dict()?;
    let from = annot_rect(doc, annot)?;
    let rotate = add_rotation(rotate, 0);
    let (width, height) = pixel_size(&to, scale, rotate);
    let base = base_matrix(&to, scale, rotate, width, height);
    // Ancien rectangle → nouveau, par axe, avant de passer aux pixels : les
    // types sans apparence (lignes, encre) suivent comme les autres.
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
    let pre = Matrix::new(sx, 0.0, 0.0, sy, to.x0 - from.x0 * sx, to.y0 - from.y0 * sy);
    let options = RenderOptions {
        annotations: true,
        time_budget: Some(std::time::Duration::from_secs(2)),
        background: None,
        ..RenderOptions::default()
    };
    let mut renderer = Renderer::new(doc, width, height, options);
    draw_one(doc, annot, &mut renderer, pre.then(&base));
    Some(renderer.into_bitmap())
}

/// Nombres d'un tableau de l'annotation (`/C`, `/IC`, `/L`, `/QuadPoints`…).
fn annot_numbers(doc: &Document, annot: &Dict, key: &str) -> Option<Vec<f64>> {
    let o = doc.dict_get(annot, key).ok().flatten()?;
    let a = o.as_array()?;
    Some(
        a.iter()
            .filter_map(|v| doc.resolve(v).ok().and_then(|x| x.as_f64()))
            .collect(),
    )
}

/// Opérateur de couleur (trait `RG`/`G`/`K` ou remplissage `rg`/`g`/`k`)
/// pour un tableau `/C` ou `/IC` ; `None` si vide (transparent) ou absent.
fn color_operator(color: &[f64], stroke: bool) -> Option<String> {
    let op = |s: &str, f: &str| if stroke { s.to_string() } else { f.to_string() };
    match color {
        [g] => Some(format!("{g} {}", op("G", "g"))),
        [r, g, b] => Some(format!("{r} {g} {b} {}", op("RG", "rg"))),
        [c, m, y, k] => Some(format!("{c} {m} {y} {k} {}", op("K", "k"))),
        _ => None,
    }
}

/// Largeur de bordure `/BS /W` (défaut 1) ou `/Border[2]`.
fn border_width(doc: &Document, annot: &Dict) -> f64 {
    if let Some(bs) = doc.dict_get(annot, "BS").ok().flatten() {
        if let Some(d) = bs.as_dict() {
            if let Some(w) = doc.dict_get(d, "W").ok().flatten().and_then(|w| w.as_f64()) {
                return w.max(0.0);
            }
        }
    }
    annot_numbers(doc, annot, "Border")
        .and_then(|b| b.get(2).copied())
        .unwrap_or(1.0)
        .max(0.0)
}

/// Trait ondulé d'un soulignement ondulé sans apparence, de `x0` à `x1`, au
/// bas d'une zone de hauteur `h` : mêmes proportions que celles qu'Acrux
/// écrit dans ses propres annotations (onde de 5 % de la hauteur, demi-pas
/// de 12,5 %), et au plus 4000 segments, pour qu'une zone aberrante ne
/// fabrique pas un chemin géant.
#[allow(clippy::format_push_string)] // un opérateur de contenu par segment
fn squiggle(content: &mut String, stroke: &str, x0: f64, x1: f64, bottom: f64, h: f64) {
    let amplitude = (0.05 * h).max(0.6);
    let half_period = (0.125 * h).max(1.0);
    let center = bottom + 0.06 * h;
    let width = x1 - x0;
    let steps = (width.abs() / half_period).ceil().clamp(1.0, 4000.0);
    let step = width / steps;
    content.push_str(&format!(
        "{stroke} {} w 1 j {x0} {} m ",
        (0.05 * h).max(0.4),
        center - amplitude
    ));
    let mut x = x0;
    let mut up = true;
    let mut done = 0.0;
    while done < steps {
        x += step;
        let y = if up {
            center + amplitude
        } else {
            center - amplitude
        };
        content.push_str(&format!("{x} {y} l "));
        up = !up;
        done += 1.0;
    }
    content.push_str("S ");
}

/// Apparence par défaut des annotations sans `/AP` (§12.5.6) : carré,
/// cercle, ligne, encre, surlignage, soulignement, barré, soulignement
/// ondulé, signe d'insertion, texte libre, polygone / polyligne. Le dessin
/// est exprimé en opérateurs de contenu et rendu dans l'espace de la page,
/// avec l'opacité `/CA` et, pour le surlignage, le mode de fusion
/// `Multiply` comme Acrobat.
// Le flux de contenu est construit par concaténation d'opérateurs : c'est la
// forme la plus lisible pour du code qui écrit du PDF.
#[allow(clippy::too_many_lines, clippy::format_push_string)]
fn draw_default_appearance(
    doc: &Document,
    annot: &Dict,
    kind: &[u8],
    rect: &Rect,
    renderer: &mut Renderer<'_>,
    base_ctm: Matrix,
) {
    let stroke = annot_numbers(doc, annot, "C").and_then(|c| color_operator(&c, true));
    let fill = annot_numbers(doc, annot, "IC").and_then(|c| color_operator(&c, false));
    let width = border_width(doc, annot);
    let alpha = doc
        .dict_get(annot, "CA")
        .ok()
        .flatten()
        .and_then(|a| a.as_f64())
        .unwrap_or(1.0)
        .clamp(0.0, 1.0);
    let mut content = String::new();
    let mut gs = Dict::new();
    gs.insert(Name::new("CA"), Object::Real(alpha));
    gs.insert(Name::new("ca"), Object::Real(alpha));
    let quads = annot_numbers(doc, annot, "QuadPoints").unwrap_or_default();
    match kind {
        b"Square" | b"Circle" => {
            let half = width / 2.0;
            let (x0, y0) = (rect.x0 + half, rect.y0 + half);
            let (w, h) = (
                (rect.width() - width).max(0.0),
                (rect.height() - width).max(0.0),
            );
            if kind == b"Square" {
                content.push_str(&format!("{x0} {y0} {w} {h} re "));
            } else {
                // Ellipse par quatre arcs de Bézier.
                let (rx, ry) = (w / 2.0, h / 2.0);
                let (cx, cy) = (x0 + rx, y0 + ry);
                let k = 0.552_284_75;
                content.push_str(&format!(
                    "{} {cy} m {} {} {} {} {cx} {} c {} {} {} {} {} {cy} c {} {} {} {} {cx} {} c {} {} {} {} {} {cy} c ",
                    cx + rx, cx + rx, cy + ry * k, cx + rx * k, cy + ry, cy + ry,
                    cx - rx * k, cy + ry, cx - rx, cy + ry * k, cx - rx,
                    cx - rx, cy - ry * k, cx - rx * k, cy - ry, cy - ry,
                    cx + rx * k, cy - ry, cx + rx, cy - ry * k, cx + rx
                ));
            }
            let op = match (&stroke, &fill, width > 0.0) {
                (Some(_), Some(_), true) => "B",
                (Some(_), None, true) => "S",
                (_, Some(_), _) => "f",
                _ => "n",
            };
            let mut prefix = String::new();
            if let Some(s) = &stroke {
                prefix.push_str(&format!("{s} {width} w "));
            }
            if let Some(f) = &fill {
                prefix.push_str(&format!("{f} "));
            }
            content = format!("{prefix}{content}{op}");
        }
        b"Line" => {
            let Some(l) = annot_numbers(doc, annot, "L") else {
                return;
            };
            let (Some(s), true) = (&stroke, l.len() >= 4) else {
                return;
            };
            content = format!("{s} {width} w {} {} m {} {} l S", l[0], l[1], l[2], l[3]);
        }
        b"Polygon" | b"PolyLine" => {
            let Some(v) = annot_numbers(doc, annot, "Vertices") else {
                return;
            };
            let Some(s) = &stroke else { return };
            if v.len() < 4 {
                return;
            }
            content.push_str(&format!("{s} {width} w {} {} m ", v[0], v[1]));
            for pair in v[2..].chunks_exact(2) {
                content.push_str(&format!("{} {} l ", pair[0], pair[1]));
            }
            content.push_str(if kind == b"Polygon" { "h S" } else { "S" });
        }
        b"Ink" => {
            let Some(s) = &stroke else { return };
            let Some(ink) = doc.dict_get(annot, "InkList").ok().flatten() else {
                return;
            };
            let Some(paths) = ink.as_array() else { return };
            content.push_str(&format!("{s} {width} w 1 J 1 j "));
            for path in paths {
                let Ok(p) = doc.resolve(path) else { continue };
                let Some(a) = p.as_array() else { continue };
                let pts: Vec<f64> = a
                    .iter()
                    .filter_map(|v| doc.resolve(v).ok().and_then(|x| x.as_f64()))
                    .collect();
                if pts.len() < 4 {
                    continue;
                }
                content.push_str(&format!("{} {} m ", pts[0], pts[1]));
                for pair in pts[2..].chunks_exact(2) {
                    content.push_str(&format!("{} {} l ", pair[0], pair[1]));
                }
                content.push_str("S ");
            }
        }
        b"Highlight" => {
            let Some(f) = annot_numbers(doc, annot, "C").and_then(|c| color_operator(&c, false))
            else {
                return;
            };
            gs.insert(Name::new("BM"), Object::Name(Name::new("Multiply")));
            content.push_str(&format!("{f} "));
            let boxes = if quads.len() >= 8 {
                quads.clone()
            } else {
                vec![
                    rect.x0, rect.y1, rect.x1, rect.y1, rect.x0, rect.y0, rect.x1, rect.y0,
                ]
            };
            for q in boxes.chunks_exact(8) {
                // Quadrilatère : (x1,y1) haut-gauche, (x2,y2) haut-droit, (x3,y3) bas-gauche, (x4,y4) bas-droit.
                content.push_str(&format!(
                    "{} {} m {} {} l {} {} l {} {} l h f ",
                    q[0], q[1], q[2], q[3], q[6], q[7], q[4], q[5]
                ));
            }
        }
        b"Underline" | b"StrikeOut" | b"Squiggly" => {
            let Some(s) = &stroke else { return };
            let boxes = if quads.len() >= 8 {
                quads.clone()
            } else {
                vec![
                    rect.x0, rect.y1, rect.x1, rect.y1, rect.x0, rect.y0, rect.x1, rect.y0,
                ]
            };
            for q in boxes.chunks_exact(8) {
                let (top, bottom) = (q[1].max(q[3]), q[5].min(q[7]));
                let h = top - bottom;
                if kind == b"Squiggly" {
                    squiggle(&mut content, s, q[4], q[6], bottom, h);
                    continue;
                }
                let (lw, y) = match kind {
                    b"StrikeOut" => (h * 0.07, bottom + h * 0.5),
                    _ => (h * 0.07, bottom + h * 0.06),
                };
                content.push_str(&format!(
                    "{s} {} w {} {y} m {} {y} l S ",
                    lw.max(0.5),
                    q[4],
                    q[6]
                ));
            }
        }
        b"Caret" => {
            // Un « ^ » plein qui occupe le rectangle, la pointe en haut et
            // une encoche à la base (§12.5.6.11) : la forme qu'Acrobat donne
            // au signe d'insertion, et celle qu'Acrux écrit dans les siens.
            let Some(f) = annot_numbers(doc, annot, "C").and_then(|c| color_operator(&c, false))
            else {
                return;
            };
            let mid = f64::midpoint(rect.x0, rect.x1);
            let notch = rect.y0 + 0.25 * rect.height();
            content = format!(
                "{f} {} {} m {mid} {} l {} {} l {mid} {notch} l h f",
                rect.x0, rect.y0, rect.y1, rect.x1, rect.y0
            );
        }
        b"FreeText" => {
            // Cadre seul (le texte exige la police de /DA : hors de portée ici).
            let s = stroke.clone().unwrap_or_else(|| "0 G".into());
            let half = width / 2.0;
            content = format!(
                "{s} {width} w {} {} {} {} re S",
                rect.x0 + half,
                rect.y0 + half,
                (rect.width() - width).max(0.0),
                (rect.height() - width).max(0.0)
            );
        }
        _ => return,
    }
    if content.is_empty() {
        return;
    }
    let mut ext = Dict::new();
    ext.insert(Name::new("AkAnnotGS"), Object::Dict(gs));
    let mut resources = Dict::new();
    resources.insert(Name::new("ExtGState"), Object::Dict(ext));
    let program = format!("/AkAnnotGS gs {content}");
    renderer.run(program.as_bytes(), &resources, GraphicsState::new(base_ctm));
}

/// Bordure d'un lien sans flux d'apparence : `/Border [h v w [tirets]]`
/// (largeur 1 par défaut) dans la couleur `/C` (gris, RVB ou CMJN ; tableau
/// vide = pas de bordure). Tracée à l'intérieur de `/Rect`.
fn draw_link_border(
    doc: &Document,
    annot: &Dict,
    rect: &Rect,
    renderer: &mut Renderer<'_>,
    base_ctm: Matrix,
) {
    let numbers = |key: &str| -> Option<Vec<f64>> {
        let o = doc.dict_get(annot, key).ok().flatten()?;
        let a = o.as_array()?;
        Some(
            a.iter()
                .filter_map(|v| doc.resolve(v).ok().and_then(|x| x.as_f64()))
                .collect(),
        )
    };
    let Some(color) = numbers("C") else { return };
    let color_op = match color.as_slice() {
        [g] => format!("{g} G"),
        [r, g, b] => format!("{r} {g} {b} RG"),
        [c, m, y, k] => format!("{c} {m} {y} {k} K"),
        _ => return,
    };
    let border = numbers("Border").unwrap_or_default();
    let width = border.get(2).copied().unwrap_or(1.0);
    if width <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let dash = match doc.dict_get(annot, "Border").ok().flatten() {
        Some(b) => b
            .as_array()
            .and_then(|a| a.get(3))
            .and_then(|d| doc.resolve(d).ok())
            .and_then(|d| {
                d.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|v| doc.resolve(v).ok().and_then(|x| x.as_f64()))
                        .map(|v| format!("{v}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
            })
            .filter(|d| !d.is_empty())
            .map_or(String::new(), |d| format!("[{d}] 0 d ")),
        None => String::new(),
    };
    let half = width / 2.0;
    let content = format!(
        "{color_op} {width} w {dash}{} {} {} {} re S",
        rect.x0 + half,
        rect.y0 + half,
        (rect.width() - width).max(0.0),
        (rect.height() - width).max(0.0)
    );
    renderer.run(
        content.as_bytes(),
        &Dict::new(),
        GraphicsState::new(base_ctm),
    );
}

/// Flux d'apparence normale (`/AP /N`, avec `/AS` pour les états).
fn normal_appearance(doc: &Document, annot: &Dict) -> Option<Object> {
    let ap = doc.dict_get(annot, "AP").ok().flatten()?;
    let ap = ap.as_dict()?;
    let n = doc.dict_get(ap, "N").ok().flatten()?;
    match &*n {
        Object::Stream { .. } => Some((*n).clone()),
        Object::Dict(states) => {
            let state = doc
                .dict_get(annot, "AS")
                .ok()
                .flatten()
                .and_then(|s| s.as_name().cloned());
            let chosen = match state {
                Some(s) => states.get(&s).cloned(),
                None if states.len() == 1 => states.values().next().cloned(),
                None => None,
            }?;
            let resolved = doc.resolve(&chosen).ok()?;
            match &*resolved {
                Object::Stream { .. } => Some((*resolved).clone()),
                _ => None,
            }
        }
        _ => None,
    }
}
