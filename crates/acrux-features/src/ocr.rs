//! Lire le texte qui n'est qu'une **image** — un document scanné, une capture
//! d'écran, une page exportée en bitmap — et le rendre modifiable.
//!
//! # Ce que fait ce module
//!
//! 1. Les images d'une page sont décodées, puis séparées en encre et papier
//!    ([`image::Ink`], seuil d'Otsu) ;
//! 2. l'encre est découpée en lignes, puis en caractères ([`segment`]) ;
//! 3. chaque caractère est comparé aux formes des polices installées
//!    ([`shapes`]), et la plus proche l'emporte ;
//! 4. la ligne reconnue est rendue avec sa **place exacte** sur la page, son
//!    corps, sa famille et son encre : de quoi la réécrire comme du vrai
//!    texte.
//!
//! # Ce qu'il ne fait pas
//!
//! Rien n'est appris : les modèles sont les polices du système, pas un
//! réseau entraîné. Un texte net rendu dans une police courante se relit
//! presque toujours exactement ; une écriture manuscrite, une photo de
//! travers ou un fax très abîmé, non. La confiance rendue avec chaque ligne
//! dit ce qu'il en est, et l'application ne propose la modification que
//! lorsqu'elle est bonne.
//!
//! # Modifier
//!
//! Remplacer une ligne, c'est couvrir ses pixels de la couleur du papier
//! ([`mask`]) puis écrire le nouveau texte par-dessus, avec les outils
//! ordinaires de [`crate::edit_text`] — le document garde son image, et
//! porte désormais du vrai texte, sélectionnable et cherchable.
// Coordonnées et comptages de pixels : les conversions y sont partout, et
// toutes bornées par la taille de l'image.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

pub mod image;
pub mod segment;
pub mod shapes;

use std::fmt::Write as _;

use acrux_core::{Matrix, Point, Rect, Result};
use acrux_document::{Document, Page};
use acrux_render::image::decode_image;

use crate::edit_objects::{self, Kind, PageObject};

/// Une ligne de texte lue dans une image.
#[derive(Debug, Clone)]
pub struct TextLine {
    /// Ce qui a été lu.
    pub text: String,
    /// Boîte de la ligne, en espace de page.
    pub bbox: Rect,
    /// Ligne de base, en espace de page.
    pub baseline: f64,
    /// Corps estimé, en points.
    pub size: f64,
    /// Famille la plus probable.
    pub family: String,
    /// Graisse.
    pub bold: bool,
    /// Italique.
    pub italic: bool,
    /// Encre.
    pub color: [f64; 3],
    /// Papier sous la ligne.
    pub background: [f64; 3],
    /// Confiance, de 0 à 1.
    pub confidence: f64,
}

/// Le texte lu dans une image de la page.
#[derive(Debug, Clone)]
pub struct ImageText {
    /// Rang de l'objet dans l'inventaire de la page.
    pub object: usize,
    /// Boîte de l'image, en espace de page.
    pub bbox: Rect,
    /// Lignes lues, de haut en bas.
    pub lines: Vec<TextLine>,
}

/// En deçà de cette confiance, la ligne n'est pas proposée à la modification.
pub const TRUSTED: f64 = 0.55;

/// Lit le texte de toutes les images d'une page.
///
/// # Errors
/// Flux de contenu illisible.
pub fn read_page(doc: &Document, page: &Page) -> Result<Vec<ImageText>> {
    let objects = edit_objects::list(doc, page)?;
    let mut out = Vec::new();
    for object in &objects {
        if !matches!(object.kind, Kind::Image | Kind::InlineImage) {
            continue;
        }
        if let Some(read) = read_object(doc, page, object) {
            if !read.lines.is_empty() {
                out.push(read);
            }
        }
    }
    Ok(out)
}

/// Lit le texte d'une image donnée.
#[must_use]
pub fn read_object(doc: &Document, page: &Page, object: &PageObject) -> Option<ImageText> {
    let decoded = decode(doc, page, object)?;
    // Une image minuscule ou démesurée ne porte pas de texte lisible.
    if decoded.width < 24 || decoded.height < 8 {
        return None;
    }
    if u64::from(decoded.width) * u64::from(decoded.height) > 64 * 1024 * 1024 {
        return None;
    }
    let ink = image::Ink::from_rgb(&decoded.rgb, decoded.width, decoded.height);
    let rows = segment::rows(&ink);
    if rows.is_empty() {
        return None;
    }
    let to_page = image::to_page(&object.matrix, decoded.width, decoded.height);
    // La famille se choisit sur toute l'image — un document n'en emploie
    // guère plus d'une — mais la graisse se choisit **ligne par ligne** :
    // un titre en gras et son texte en romain ne se lisent pas avec le même
    // modèle.
    let family = choose_face(&ink, &rows, shapes::faces())?.family.clone();
    let variants: Vec<&shapes::Face> = shapes::faces()
        .iter()
        .filter(|f| f.family == family)
        .collect();
    let mut lines = Vec::new();
    for row in &rows {
        let face = choose_face(&ink, std::slice::from_ref(row), &variants)
            .or_else(|| variants.first().copied());
        let Some(face) = face else { continue };
        if let Some(line) = read_row(&ink, row, face, &to_page) {
            lines.push(line);
        }
    }
    Some(ImageText {
        object: object.index,
        bbox: object.bbox,
        lines,
    })
}

/// Décode les pixels d'une image de la page.
fn decode(
    doc: &Document,
    page: &Page,
    object: &PageObject,
) -> Option<acrux_render::image::DecodedImage> {
    let name = object.name.as_ref()?;
    let resources = edit_objects::resources(doc, page);
    let xobjects = doc
        .dict_get(&resources, "XObject")
        .ok()??
        .as_dict()
        .cloned()?;
    let entry = xobjects.get(&acrux_document::Name::new(name))?;
    let resolved = doc.resolve(entry).ok()?;
    let acrux_document::Object::Stream { dict, .. } = &*resolved else {
        return None;
    };
    let data = doc.stream_data(&resolved).ok()?;
    let filter = data.image_filter.as_ref().map(|(n, p)| (n.as_slice(), p));
    let decoded = decode_image(doc, dict, &data.data, filter, Some(&resources)).ok()?;
    if decoded.rgb.is_empty() {
        return None;
    }
    Some(decoded)
}

/// Choisit la police qui explique le mieux un échantillon de caractères.
///
/// Comparer chaque tache à toutes les polices coûterait dix fois plus cher ;
/// on tranche sur une trentaine de caractères, puis on s'y tient.
fn choose_face<'a, F>(
    ink: &image::Ink,
    rows: &[segment::Row],
    faces: &'a [F],
) -> Option<&'a shapes::Face>
where
    F: std::borrow::Borrow<shapes::Face>,
{
    if faces.is_empty() {
        return None;
    }
    let mut scores = vec![0.0_f64; faces.len()];
    let mut seen = 0;
    // Une vingtaine de lettres suffisent à reconnaître une police ; les
    // comparer toutes à toutes les polices coûterait bien plus cher.
    let wanted = if rows.len() > 1 { 24 } else { 12 };
    for row in rows {
        let Some(metrics) = Metrics::of(row) else {
            continue;
        };
        for glyph in row.glyphs.iter().step_by(2) {
            if seen >= wanted {
                break;
            }
            let Some(probe) = Probe::of(ink, glyph.bbox, &metrics) else {
                continue;
            };
            for (i, face) in faces.iter().enumerate() {
                if let Some(m) = probe.best(face.borrow()) {
                    scores[i] += m.score;
                }
            }
            seen += 1;
        }
        if seen >= wanted {
            break;
        }
    }
    if seen == 0 {
        return None;
    }
    let (best, _) = scores
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.total_cmp(b.1))?;
    faces.get(best).map(std::borrow::Borrow::borrow)
}

/// Mesures d'une ligne : ligne de base et hauteur d'x, en pixels.
struct Metrics {
    /// Ordonnée de la ligne de base, en pixels de l'image.
    baseline: f64,
    /// Hauteur d'x, en pixels.
    x_height: f64,
}

impl Metrics {
    /// Relève la ligne de base et la hauteur d'x d'une ligne.
    ///
    /// La ligne de base est le bas que partagent le plus de caractères — les
    /// jambages de « p » et « q » descendent plus bas, mais ils sont rares.
    /// La hauteur d'x est le haut le plus fréquent, celui des minuscules.
    fn of(row: &segment::Row) -> Option<Self> {
        if row.glyphs.is_empty() {
            return None;
        }
        let mut bottoms: Vec<u32> = row.glyphs.iter().map(|g| g.bbox.y1).collect();
        let mut tops: Vec<u32> = row.glyphs.iter().map(|g| g.bbox.y0).collect();
        bottoms.sort_unstable();
        tops.sort_unstable();
        // Le bas médian, plus stable qu'une moyenne face aux jambages.
        let baseline = f64::from(bottoms[bottoms.len() / 2]);
        // Le haut médian donne les minuscules ; les capitales sont plus
        // hautes et minoritaires dans un texte courant.
        let x_top = f64::from(tops[(tops.len() * 2) / 3]);
        let x_height = (baseline - x_top).max(2.0);
        Some(Self { baseline, x_height })
    }
}

/// Une tache d'encre prête à être comparée.
struct Probe {
    grid: [u8; shapes::CELLS],
    aspect: f64,
    top: f64,
    bottom: f64,
}

impl Probe {
    /// Relève une tache et la rapporte aux mesures de sa ligne.
    fn of(ink: &image::Ink, bbox: segment::Box2, metrics: &Metrics) -> Option<Self> {
        if bbox.width() == 0 || bbox.height() == 0 {
            return None;
        }
        Some(Self {
            grid: shapes::grid_of(ink, bbox),
            aspect: f64::from(bbox.width()) / f64::from(bbox.height()),
            top: (metrics.baseline - f64::from(bbox.y0)) / metrics.x_height,
            bottom: (metrics.baseline - f64::from(bbox.y1)) / metrics.x_height,
        })
    }

    /// Le meilleur caractère de cette police.
    fn best(&self, face: &shapes::Face) -> Option<shapes::Match> {
        shapes::best(face, &self.grid, self.aspect, self.top, self.bottom)
    }
}

/// Lit une ligne entière.
fn read_row(
    ink: &image::Ink,
    row: &segment::Row,
    face: &shapes::Face,
    to_page: &Matrix,
) -> Option<TextLine> {
    let metrics = Metrics::of(row)?;
    let mut text = String::new();
    let mut total = 0.0_f64;
    let mut seen = 0_usize;
    for glyph in &row.glyphs {
        if glyph.space_before {
            text.push(' ');
        }
        let read = read_glyph(ink, glyph.bbox, &metrics, face, 0);
        if read.count == 0 {
            continue;
        }
        text.push_str(&read.text);
        total += read.score;
        seen += read.count;
    }
    let text = settle(&text);
    if seen == 0 || text.trim().is_empty() {
        return None;
    }
    let average = total / seen as f64;
    // Une distance nulle vaut une confiance pleine ; au-delà d'une demie,
    // plus rien de sûr.
    let confidence = (1.0 - average * 2.0).clamp(0.0, 1.0);
    let bbox = image::page_rect(to_page, row.bbox.x0, row.bbox.y0, row.bbox.x1, row.bbox.y1);
    let baseline = to_page
        .apply(Point::new(f64::from(row.bbox.x0), metrics.baseline))
        .y;
    // Le corps se déduit de la hauteur d'x mesurée : c'est la seule mesure
    // que l'image donne sûrement.
    let x_ratio = 0.52;
    let top_left = to_page.apply(Point::new(0.0, 0.0));
    let down = to_page.apply(Point::new(0.0, metrics.x_height));
    let x_height_pt = (down.y - top_left.y).abs();
    let size = (x_height_pt / x_ratio).max(1.0);
    Some(TextLine {
        text,
        bbox,
        baseline,
        size,
        family: face.family.clone(),
        bold: face.bold,
        italic: face.italic,
        color: ink.color,
        background: ink.background,
        confidence,
    })
}

/// Tranche par le contexte les confusions que la forme ne sait pas trancher.
///
/// « l », « 1 » et « I » ont presque la même forme ; c'est leur voisinage
/// qui dit ce qu'ils sont. Un chiffre isolé au milieu d'un mot n'existe
/// pratiquement pas, et une lettre au milieu d'un nombre non plus.
fn settle(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, c) in chars.iter().enumerate() {
        let before = i.checked_sub(1).and_then(|j| chars.get(j)).copied();
        let after = chars.get(i + 1).copied();
        let letter = |c: Option<char>| c.is_some_and(char::is_alphabetic);
        let digit = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit());
        let swapped = match c {
            '1' | 'I' if letter(before) || letter(after) => Some('l'),
            '0' if letter(before) || letter(after) => Some('o'),
            'l' | 'I' if digit(before) || digit(after) => Some('1'),
            'o' | 'O' if digit(before) && digit(after) => Some('0'),
            _ => None,
        };
        out.push(swapped.unwrap_or(*c));
    }
    out
}

/// Ce qu'une tache d'encre a donné à lire.
struct Read {
    /// Caractères lus.
    text: String,
    /// Somme des écarts.
    score: f64,
    /// Nombre de caractères.
    count: usize,
}

impl Read {
    /// Ce que vaut un verdict de la comparaison, ligatures épelées.
    fn of(found: shapes::Match) -> Self {
        let spelled = shapes::spelled(found.c);
        let text = if spelled.is_empty() {
            found.c.to_string()
        } else {
            spelled.to_string()
        };
        let count = text.chars().count();
        Self {
            score: found.score * count as f64,
            text,
            count,
        }
    }
}

/// Lit une tache d'encre — et, si elle se lit mal, ses deux moitiés.
///
/// C'est la reconnaissance qui décide du découpage, et non l'inverse : deux
/// lettres qui se touchent ne font qu'une tache, mais une tache qui se lit
/// mal et dont les moitiés se lisent bien **était** deux lettres. Aucune
/// mesure géométrique ne sait faire cette différence — un « m » a lui aussi
/// un creux au milieu, et il se lit très bien entier.
fn read_glyph(
    ink: &image::Ink,
    bbox: segment::Box2,
    metrics: &Metrics,
    face: &shapes::Face,
    depth: u32,
) -> Read {
    let whole = Probe::of(ink, bbox, metrics).and_then(|p| p.best(face));
    let Some(whole) = whole else {
        return Read {
            text: String::new(),
            score: 0.0,
            count: 0,
        };
    };
    // Une tache qui se lit bien est une lettre : on n'y touche pas.
    if depth >= 2 || whole.score < 0.22 {
        return Read::of(whole);
    }
    let Some((left, right)) = segment::split_at(ink, bbox) else {
        return Read::of(whole);
    };
    let a = read_glyph(ink, left, metrics, face, depth + 1);
    let b = read_glyph(ink, right, metrics, face, depth + 1);
    let count = a.count + b.count;
    if count == 0 {
        return Read::of(whole);
    }
    let split_score = (a.score + b.score) / count as f64;
    // Les moitiés doivent se lire nettement mieux que l'ensemble : sans
    // cette marge, on couperait les lettres larges en deux.
    if split_score < whole.score * 0.75 {
        Read {
            text: format!("{}{}", a.text, b.text),
            score: a.score + b.score,
            count,
        }
    } else {
        Read::of(whole)
    }
}

/// Ligne lue sous un point de la page, si elle est sûre.
#[must_use]
pub fn line_at(images: &[ImageText], x: f64, y: f64) -> Option<&TextLine> {
    images
        .iter()
        .flat_map(|i| i.lines.iter())
        .filter(|l| l.confidence >= TRUSTED)
        .find(|l| x >= l.bbox.x0 && x <= l.bbox.x1 && y >= l.bbox.y0 && y <= l.bbox.y1)
}

/// Couvre une zone de la page d'un rectangle plein.
///
/// C'est ce qui efface le texte d'origine d'une image avant d'écrire le
/// nouveau par-dessus — l'image elle-même n'est pas touchée.
///
/// # Errors
/// Flux de contenu illisible, ou page non indirecte.
pub fn mask(doc: &Document, page: &Page, rect: Rect, color: [f64; 3]) -> Result<()> {
    let content = acrux_render::page::page_content(doc, page);
    let mut out = Vec::with_capacity(content.len() + 128);
    out.extend_from_slice(b"q\n");
    out.extend_from_slice(&content);
    let [r, g, b] = color;
    let _ = write!(
        StrOut(&mut out),
        "\nQ\nq {} {} {} rg {} {} {} {} re f Q\n",
        fmt(r),
        fmt(g),
        fmt(b),
        fmt(rect.x0),
        fmt(rect.y0),
        fmt(rect.width()),
        fmt(rect.height()),
    );
    crate::edit_text::set_page_content(doc, page, out)
}

/// Écrit des octets dans un vecteur.
struct StrOut<'a>(&'a mut Vec<u8>);

impl std::fmt::Write for StrOut<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

/// Un nombre, au plus court.
fn fmt(v: f64) -> String {
    let mut s = format!("{v:.4}");
    while s.contains('.') && (s.ends_with('0') || s.ends_with('.')) {
        s.pop();
    }
    if s.is_empty() || s == "-" {
        "0".to_string()
    } else {
        s
    }
}

/// Grilles des premières taches d'une image, pour la mise au point.
///
/// Rendues telles qu'elles sont comparées aux modèles : c'est le moyen de
/// voir ce que la reconnaissance voit vraiment.
#[must_use]
pub fn probe_grids(doc: &Document, page: &Page, count: usize) -> Vec<(String, String)> {
    let Ok(objects) = edit_objects::list(doc, page) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for object in &objects {
        if !matches!(object.kind, Kind::Image | Kind::InlineImage) {
            continue;
        }
        let Some(decoded) = decode(doc, page, object) else {
            continue;
        };
        let ink = image::Ink::from_rgb(&decoded.rgb, decoded.width, decoded.height);
        let rows = segment::rows(&ink);
        let face = choose_face(&ink, &rows, shapes::faces());
        for row in rows.iter().skip(1) {
            let Some(metrics) = Metrics::of(row) else {
                continue;
            };
            for glyph in &row.glyphs {
                if out.len() >= count {
                    return out;
                }
                let Some(probe) = Probe::of(&ink, glyph.bbox, &metrics) else {
                    continue;
                };
                let found = face.and_then(|f| probe.best(f));
                let label = found.map_or_else(
                    || "?".to_string(),
                    |m| format!("« {} » ({:.3})", m.c, m.score),
                );
                out.push((label, shapes::ascii(&probe.grid)));
            }
        }
    }
    out
}

/// Candidats des premières taches, pour la mise au point.
#[must_use]
pub fn probe_ranking(doc: &Document, page: &Page, skip: usize, count: usize) -> Vec<String> {
    let Ok(objects) = edit_objects::list(doc, page) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for object in &objects {
        if !matches!(object.kind, Kind::Image | Kind::InlineImage) {
            continue;
        }
        let Some(decoded) = decode(doc, page, object) else {
            continue;
        };
        let ink = image::Ink::from_rgb(&decoded.rgb, decoded.width, decoded.height);
        let rows = segment::rows(&ink);
        let Some(face) = choose_face(&ink, &rows, shapes::faces()) else {
            continue;
        };
        for row in rows.iter().skip(1) {
            let Some(metrics) = Metrics::of(row) else {
                continue;
            };
            for glyph in row.glyphs.iter().skip(skip) {
                if out.len() >= count {
                    return out;
                }
                let Some(probe) = Probe::of(&ink, glyph.bbox, &metrics) else {
                    continue;
                };
                let ranked =
                    shapes::ranked(face, &probe.grid, probe.aspect, probe.top, probe.bottom);
                let mut line = format!(
                    "boite {}x{} prop {:.2} haut {:.2} bas {:.2} :",
                    glyph.bbox.width(),
                    glyph.bbox.height(),
                    probe.aspect,
                    probe.top,
                    probe.bottom
                );
                for (c, score, shape, proportion, place) in ranked {
                    let _ = std::fmt::Write::write_fmt(
                        &mut line,
                        format_args!(" {c}={score:.3}(f{shape:.2} p{proportion:.2} l{place:.2})"),
                    );
                }
                out.push(line);
            }
            return out;
        }
    }
    out
}
