//! Annotations (ISO 32000-2 §12.5 ; inventaire Acrobat §8) : inventaire des
//! annotations d'une page, création d'annotations avec leur flux
//! d'apparence (pour que tout lecteur, Acrobat compris, les affiche à
//! l'identique), suppression.
//!
//! Les apparences sont générées en syntaxe PDF minimale et non compressée :
//! lisibles, déterministes, et rendues par notre moteur comme par les autres.
//!
//! # Balisage du texte
//!
//! Surligner, souligner, barrer et souligner d'un trait ondulé (§12.5.6.10)
//! posent **une** annotation par page, avec un quadrilatère par ligne, comme
//! Acrobat : un passage de dix lignes est un seul commentaire, qu'on relit,
//! qu'on annule et qu'on supprime d'un seul geste. L'insertion est un signe
//! `/Caret` (§12.5.6.11) ; le remplacement, un texte barré et un signe
//! d'insertion **groupés** (§12.5.6.2, `/IRT` et `/RT /Group`) : le signe
//! porte le texte proposé, le barré le suit partout.
//!
//! # Identité
//!
//! Toute annotation créée ici porte un identifiant unique (`/NM`) et sa date
//! de création (`/CreationDate`). L'identité est tirée **une fois**, par
//! celui qui décide de la poser ([`AnnotMeta::fresh`]), puis transmise
//! telle quelle : une modification appliquée à deux copies du document, ou
//! rejouée après une annulation, doit donner la même annotation, sans quoi
//! son identifiant ne désignerait plus rien.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use acrux_core::{Error, Rect, Result};
use acrux_document::text::decode_text_string;
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef, Page};

/// Résumé d'une annotation existante.
#[derive(Debug, Clone)]
pub struct AnnotationInfo {
    /// Position dans `/Annots`.
    pub index: usize,
    /// Référence de l'objet (si indirect).
    pub reference: Option<ObjectRef>,
    /// `/Subtype` (`Text`, `Link`, `Highlight`, `Square`, `Widget`…).
    pub subtype: String,
    /// `/Rect`.
    pub rect: Rect,
    /// `/Contents` décodé.
    pub contents: Option<String>,
    /// `/T` (auteur).
    pub author: Option<String>,
    /// `/M` (date de modification, brute `D:…`).
    pub modified: Option<String>,
    /// `/F` (drapeaux, §12.5.3).
    pub flags: i64,
    /// Possède un flux d'apparence `/AP /N`.
    pub has_appearance: bool,
    /// `/Subtype /Link` : cible URI si `/A << /S /URI >>`.
    pub uri: Option<String>,
    /// `/NM` : identifiant unique de l'annotation dans la page (§12.5.2).
    pub name: Option<String>,
    /// `/IRT` : annotation à laquelle celle-ci répond, ou dont elle fait
    /// partie (§12.5.6.2).
    pub in_reply_to: Option<ObjectRef>,
    /// `/RT` : nature du lien `/IRT` — `R` pour une réponse, `Group` pour un
    /// membre d'un groupe (nom brut).
    pub reply_type: Option<String>,
    /// `/IT` : intention (`Replace`, `StrikeOutTextEdit`…), nom brut.
    pub intent: Option<String>,
}

impl AnnotationInfo {
    /// Vrai si l'annotation n'est qu'un membre d'un groupe (`/RT /Group`) :
    /// son texte, son auteur et sa date sont ceux du membre principal, que
    /// désigne `/IRT` (§12.5.6.2). Une liste de commentaires ne la montre pas
    /// à part.
    #[must_use]
    pub fn is_group_member(&self) -> bool {
        self.in_reply_to.is_some() && self.reply_type.as_deref() == Some("Group")
    }
}

/// Couleur RVB 0..1.
pub type Rgb = [f64; 3];

/// Couleur du signe d'insertion : le bleu des outils de relecture d'Acrobat.
pub const CARET_COLOR: Rgb = [0.0, 0.47, 0.84];

/// Les quatre annotations de balisage du texte (§12.5.6.10) : elles
/// désignent un passage par ses quadrilatères, sans toucher au contenu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkupKind {
    /// Surlignage (`/Highlight`).
    Highlight,
    /// Soulignement (`/Underline`).
    Underline,
    /// Texte barré (`/StrikeOut`).
    StrikeOut,
    /// Soulignement ondulé (`/Squiggly`).
    Squiggly,
}

impl MarkupKind {
    /// Valeur de `/Subtype`.
    #[must_use]
    pub fn subtype(self) -> &'static str {
        match self {
            MarkupKind::Highlight => "Highlight",
            MarkupKind::Underline => "Underline",
            MarkupKind::StrikeOut => "StrikeOut",
            MarkupKind::Squiggly => "Squiggly",
        }
    }

    /// Couleur par défaut. C'est la seule source de ces couleurs : la
    /// ligne de commande et l'application la lisent ici, et un même geste
    /// donne donc partout la même annotation.
    #[must_use]
    pub fn default_color(self) -> Rgb {
        match self {
            MarkupKind::Highlight => [1.0, 1.0, 0.0],
            MarkupKind::Underline => CARET_COLOR,
            MarkupKind::StrikeOut => [0.86, 0.1, 0.1],
            MarkupKind::Squiggly => [0.1, 0.6, 0.2],
        }
    }

    /// Sujet (`/Subj`), que les autres lecteurs affichent comme type du
    /// commentaire.
    #[must_use]
    pub fn subject(self) -> &'static str {
        match self {
            MarkupKind::Highlight => "Surlignage",
            MarkupKind::Underline => "Soulignement",
            MarkupKind::StrikeOut => "Texte barré",
            MarkupKind::Squiggly => "Soulignement ondulé",
        }
    }
}

/// Identité d'une annotation à créer : auteur (`/T`), identifiant (`/NM`)
/// et date (`/M` et `/CreationDate`, forme `D:…`).
///
/// Un champ absent est tiré au moment de la création. Pour qu'une même
/// modification donne la même annotation partout, on la tire **avant**,
/// avec [`AnnotMeta::fresh`], et on la garde avec la modification.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnnotMeta {
    /// Auteur.
    pub author: Option<String>,
    /// Identifiant unique.
    pub name: Option<String>,
    /// Date de création.
    pub date: Option<String>,
}

impl AnnotMeta {
    /// Identité neuve : un identifiant jamais vu et la date de maintenant.
    #[must_use]
    pub fn fresh(author: Option<&str>) -> Self {
        Self {
            author: author.map(ToString::to_string),
            name: Some(new_annotation_id()),
            date: Some(pdf_date_now()),
        }
    }

    /// La même, les champs manquants tirés maintenant.
    fn complete(&self) -> (Option<&str>, String, String) {
        (
            self.author.as_deref(),
            self.name.clone().unwrap_or_else(new_annotation_id),
            self.date.clone().unwrap_or_else(pdf_date_now),
        )
    }
}

/// Compteur des identifiants tirés par ce processus.
static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Identifiant d'annotation neuf, de la forme `acrux-<16 hex>-<8 hex>`.
///
/// Il lui suffit d'être **unique**, pas imprévisible : l'heure à la
/// nanoseconde, le numéro du processus et un compteur, brassés (finaliseur
/// de SplitMix64), puis le compteur en clair. Deux appels d'un même
/// processus diffèrent donc toujours, deux processus presque toujours — et
/// un doublon ne ferait que confondre deux commentaires, jamais rien casser.
#[must_use]
pub fn new_annotation_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let low = u64::try_from(nanos % (1_u128 << 64)).unwrap_or(0);
    let count = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut x = low
        ^ u64::from(std::process::id()).rotate_left(32)
        ^ count.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    format!("acrux-{x:016x}-{:08x}", count & 0xFFFF_FFFF)
}

/// Annotation à créer.
#[derive(Debug, Clone)]
pub enum NewAnnotation {
    /// Rectangle (`/Square`).
    Square {
        /// Emplacement.
        rect: Rect,
        /// Couleur de bordure.
        stroke: Rgb,
        /// Épaisseur de bordure en points.
        width: f64,
        /// Couleur de remplissage (aucune si `None`).
        fill: Option<Rgb>,
        /// Commentaire associé (`/Contents`), comme sur n'importe quelle
        /// annotation de balisage : c'est lui que les relecteurs lisent.
        contents: Option<String>,
    },
    /// Surlignage (`/Highlight`) d'une zone rectangulaire. C'est un
    /// [`NewAnnotation::Markup`] à une seule zone, gardé pour les appelants
    /// qui n'en ont qu'une.
    Highlight {
        /// Zone.
        rect: Rect,
        /// Couleur (jaune Acrobat par défaut : 1, 1, 0).
        color: Rgb,
        /// Commentaire associé (`/Contents`).
        contents: Option<String>,
    },
    /// Balisage d'un passage : surlignage, soulignement, texte barré ou
    /// soulignement ondulé, une zone par ligne.
    Markup {
        /// Sorte de balisage.
        kind: MarkupKind,
        /// Zones, une par ligne, dans l'ordre de lecture.
        quads: Vec<Rect>,
        /// Couleur (voir [`MarkupKind::default_color`]).
        color: Rgb,
        /// Commentaire associé (`/Contents`).
        contents: Option<String>,
    },
    /// Signe d'insertion (`/Caret`) : « ajouter ce texte ici ».
    Caret {
        /// Abscisse du point d'insertion.
        x: f64,
        /// Boîte de la ligne au point d'insertion : sa hauteur règle la
        /// taille du signe, son bas le place.
        line: Rect,
        /// Texte à insérer.
        contents: String,
        /// Couleur du signe.
        color: Rgb,
    },
    /// Remplacement : le passage est barré, et un signe d'insertion à sa
    /// fin porte le texte proposé. Les deux annotations forment un groupe
    /// (§12.5.6.2), comme « Remplacer le texte » dans Acrobat.
    Replace {
        /// Zones du passage remplacé, une par ligne, sur une même page.
        quads: Vec<Rect>,
        /// Texte proposé à la place.
        text: String,
        /// Couleur du barré.
        strike: Rgb,
        /// Couleur du signe d'insertion.
        caret: Rgb,
    },
    /// Note (`/Text`, icône « commentaire ») avec contenu.
    Note {
        /// Coin supérieur gauche de l'icône (20 × 20 pt).
        x: f64,
        /// Ordonnée du haut de l'icône.
        y: f64,
        /// Texte de la note.
        contents: String,
        /// Couleur de l'icône.
        color: Rgb,
    },
    /// Lien (`/Link`) vers une URI, sans apparence visible (comme Acrobat).
    Link {
        /// Zone cliquable.
        rect: Rect,
        /// Cible.
        uri: String,
    },
}

fn num(doc: &Document, d: &Dict, key: &str) -> Option<f64> {
    doc.dict_get(d, key).ok().flatten().and_then(|o| o.as_f64())
}

fn string(doc: &Document, d: &Dict, key: &str) -> Option<String> {
    let o = doc.dict_get(d, key).ok().flatten()?;
    match &*o {
        Object::String(s) => Some(decode_text_string(s)),
        _ => None,
    }
}

fn name(doc: &Document, d: &Dict, key: &str) -> Option<String> {
    let o = doc.dict_get(d, key).ok().flatten()?;
    o.as_name().map(Name::as_str)
}

/// Inventaire des annotations d'une page.
///
/// # Errors
/// Tableau `/Annots` illisible.
pub fn list_annotations(doc: &Document, page: &Page) -> Result<Vec<AnnotationInfo>> {
    let mut out = Vec::new();
    let Some(annots) = page.dict.get(&Name::new("Annots")) else {
        return Ok(out);
    };
    let annots = doc.resolve(annots)?;
    let Some(list) = annots.as_array() else {
        return Ok(out);
    };
    for (index, item) in list.iter().enumerate() {
        let reference = match item {
            Object::Reference(r) => Some(*r),
            _ => None,
        };
        let Ok(resolved) = doc.resolve(item) else {
            continue;
        };
        let Some(d) = resolved.as_dict() else {
            continue;
        };
        let subtype = d
            .get(&Name::new("Subtype"))
            .and_then(Object::as_name)
            .map_or_else(|| "?".to_string(), Name::as_str);
        let r: Vec<f64> = doc
            .dict_get(d, "Rect")
            .ok()
            .flatten()
            .and_then(|o| {
                o.as_array().map(|a| {
                    a.iter()
                        .filter_map(|v| doc.resolve(v).ok().and_then(|x| x.as_f64()))
                        .collect()
                })
            })
            .unwrap_or_default();
        let rect = if r.len() == 4 {
            Rect::new(r[0], r[1], r[2], r[3])
        } else {
            Rect::default()
        };
        let has_appearance = doc
            .dict_get(d, "AP")
            .ok()
            .flatten()
            .and_then(|ap| ap.as_dict().map(|a| a.contains_key(&Name::new("N"))))
            .unwrap_or(false);
        let uri = doc.dict_get(d, "A").ok().flatten().and_then(|a| {
            let a = a.as_dict()?;
            if a.get(&Name::new("S")).and_then(Object::as_name)?.0 != b"URI" {
                return None;
            }
            string(doc, a, "URI")
        });
        // `/IRT` n'a de sens que comme référence : c'est l'objet visé.
        let in_reply_to = match d.get(&Name::new("IRT")) {
            Some(Object::Reference(r)) => Some(*r),
            _ => None,
        };
        out.push(AnnotationInfo {
            index,
            reference,
            subtype,
            rect,
            contents: string(doc, d, "Contents"),
            author: string(doc, d, "T"),
            modified: string(doc, d, "M"),
            #[allow(clippy::cast_possible_truncation)]
            flags: num(doc, d, "F").unwrap_or(0.0) as i64,
            has_appearance,
            uri,
            name: string(doc, d, "NM"),
            in_reply_to,
            reply_type: name(doc, d, "RT"),
            intent: name(doc, d, "IT"),
        });
    }
    Ok(out)
}

/// Date PDF `D:AAAAMMJJHHmmSSZ` pour l'instant présent (UTC).
#[must_use]
pub fn pdf_date_now() -> String {
    pdf_date_at(0)
}

/// Même chose, décalée de `offset_minutes` par rapport à UTC (le suffixe
/// reste `Z` : la valeur écrite est déjà celle du fuseau demandé).
#[must_use]
pub fn pdf_date_at(offset_minutes: i32) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let shifted = i64::try_from(secs).unwrap_or(0) + i64::from(offset_minutes) * 60;
    let secs = u64::try_from(shifted.max(0)).unwrap_or(0);
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Algorithme « civil from days » (Howard Hinnant), sans dépendance.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "D:{y:04}{m:02}{d:02}{:02}{:02}{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    // « -0 » se lit, mais n'a rien à faire dans un flux déterministe.
    if s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// Même formatage, pour les modules voisins qui écrivent eux aussi des
/// apparences à la main (`attach`) : un nombre court et déterministe, sans
/// zéros inutiles, pour que le flux produit ne dépende pas de la plateforme.
pub(crate) fn fmt_number(v: f64) -> String {
    fmt(v)
}

/// XObject de formulaire minimal (`/BBox` + contenu), pour les modules
/// voisins qui posent leur propre annotation.
pub(crate) fn form_xobject(bbox: Rect, content: String) -> Object {
    appearance_stream(bbox, content, &[])
}

/// Flux d'apparence : dictionnaire de formulaire + contenu.
fn appearance_stream(bbox: Rect, content: String, extra: &[(&str, Object)]) -> Object {
    let mut d = Dict::new();
    d.insert(Name::new("Type"), Object::Name(Name::new("XObject")));
    d.insert(Name::new("Subtype"), Object::Name(Name::new("Form")));
    d.insert(Name::new("BBox"), rect_object(bbox));
    for (k, v) in extra {
        d.insert(Name::new(k), v.clone());
    }
    let raw = content.into_bytes();
    d.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
    );
    Object::Stream { dict: d, raw }
}

fn color_array(c: Rgb) -> Object {
    Object::Array(c.iter().map(|v| Object::Real(*v)).collect())
}

/// Opérateur de couleur RVB, de trait (`RG`) ou de remplissage (`rg`).
fn color_op(c: Rgb, stroke: bool) -> String {
    format!(
        "{} {} {} {}",
        fmt(c[0]),
        fmt(c[1]),
        fmt(c[2]),
        if stroke { "RG" } else { "rg" }
    )
}

/// Rectangle PDF `[x0 y0 x1 y1]`.
fn rect_object(r: Rect) -> Object {
    Object::Array(vec![
        Object::Real(r.x0),
        Object::Real(r.y0),
        Object::Real(r.x1),
        Object::Real(r.y1),
    ])
}

/// `/QuadPoints` : huit nombres par zone, haut-gauche, haut-droit,
/// bas-gauche, bas-droit (§12.5.6.10) — l'ordre qu'écrit Acrobat, et que
/// lisent tous les lecteurs.
fn quad_points(quads: &[Rect]) -> Object {
    let mut out = Vec::with_capacity(quads.len() * 8);
    for q in quads {
        out.extend(
            [q.x0, q.y1, q.x1, q.y1, q.x0, q.y0, q.x1, q.y0]
                .into_iter()
                .map(Object::Real),
        );
    }
    Object::Array(out)
}

/// Plus petit rectangle qui contient toutes les zones.
fn union_of(quads: &[Rect]) -> Option<Rect> {
    let (first, rest) = quads.split_first()?;
    Some(rest.iter().fold(*first, |acc, q| acc.union(q)))
}

/// Écrit l'identité d'une annotation : auteur, identifiant, dates.
///
/// `/M` et `/CreationDate` valent la même date : l'annotation naît et n'a
/// pas encore été modifiée.
pub(crate) fn identity(d: &mut Dict, meta: &AnnotMeta) {
    let (author, id, date) = meta.complete();
    if let Some(a) = author {
        d.insert(Name::new("T"), Object::String(encode_text(a)));
    }
    d.insert(Name::new("NM"), Object::String(encode_text(&id)));
    d.insert(Name::new("M"), Object::String(date.clone().into_bytes()));
    d.insert(Name::new("CreationDate"), Object::String(date.into_bytes()));
}

/// Dictionnaire commun à toute annotation posée ici.
fn base_dict(page_ref: ObjectRef, meta: &AnnotMeta) -> Dict {
    let mut d = Dict::new();
    d.insert(Name::new("Type"), Object::Name(Name::new("Annot")));
    d.insert(Name::new("P"), Object::Reference(page_ref));
    d.insert(Name::new("F"), Object::Integer(4)); // Print
    identity(&mut d, meta);
    d
}

/// Pose `/AP << /N … >>` avec un flux d'apparence neuf.
fn set_appearance(doc: &Document, d: &mut Dict, stream: Object) {
    let ap = doc.add(stream);
    let mut apd = Dict::new();
    apd.insert(Name::new("N"), Object::Reference(ap));
    d.insert(Name::new("AP"), Object::Dict(apd));
}

/// Plafond de segments d'un trait ondulé, par zone : une zone aberrante
/// (un million de points de large) ne doit pas produire un flux géant.
const MAX_SQUIGGLE_STEPS: f64 = 4000.0;

/// Trait ondulé sous une zone, ajouté au contenu. Le centre de l'onde est
/// à la hauteur d'un soulignement ; l'amplitude et le pas suivent la
/// hauteur de la ligne, pour garder la même allure à toutes les tailles.
fn squiggle(content: &mut String, zone: &Rect) -> f64 {
    let height = zone.height();
    let amplitude = (0.05 * height).max(0.6);
    let half_period = (0.125 * height).max(1.0);
    let width = (0.05 * height).max(0.4);
    let center = zone.y0 + 0.06 * height;
    let steps = (zone.width() / half_period)
        .ceil()
        .clamp(1.0, MAX_SQUIGGLE_STEPS);
    let step = zone.width() / steps;
    let _ = write!(
        content,
        "{} w {} {} m ",
        fmt(width),
        fmt(zone.x0),
        fmt(center - amplitude)
    );
    // Borné juste au-dessus par MAX_SQUIGGLE_STEPS : la conversion est exacte.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let count = steps as u32;
    for i in 1..=count {
        let y = if i % 2 == 0 {
            center - amplitude
        } else {
            center + amplitude
        };
        let _ = write!(
            content,
            "{} {} l ",
            fmt(zone.x0 + step * f64::from(i)),
            fmt(y)
        );
    }
    content.push_str("S ");
    width + amplitude
}

/// Contenu et rectangle d'une annotation de balisage.
///
/// Les proportions des traits sont celles du rendu de secours
/// (`acrux-render`, pour une annotation sans apparence) : épaisseur 7 % de
/// la hauteur de ligne, soulignement à 6 % du bas, barré à mi-hauteur. Une
/// annotation a ainsi le même aspect, qu'elle vienne d'ici ou d'ailleurs.
fn markup_appearance(kind: MarkupKind, quads: &[Rect], color: Rgb) -> (Rect, String) {
    let bounds = union_of(quads).unwrap_or_default();
    let mut content = String::new();
    let mut margin: f64 = 0.0;
    match kind {
        MarkupKind::Highlight => {
            let _ = write!(content, "/GS0 gs {} ", color_op(color, false));
            for q in quads {
                let _ = write!(
                    content,
                    "{} {} {} {} re ",
                    fmt(q.x0),
                    fmt(q.y0),
                    fmt(q.width()),
                    fmt(q.height())
                );
            }
            content.push('f');
        }
        MarkupKind::Underline | MarkupKind::StrikeOut => {
            let _ = write!(content, "{} 0 J ", color_op(color, true));
            for q in quads {
                let h = q.height();
                let lw = (0.07 * h).max(0.5);
                let y = if kind == MarkupKind::StrikeOut {
                    q.y0 + 0.5 * h
                } else {
                    q.y0 + 0.06 * h
                };
                let _ = write!(
                    content,
                    "{} w {} {y} m {} {y} l S ",
                    fmt(lw),
                    fmt(q.x0),
                    fmt(q.x1),
                    y = fmt(y)
                );
                margin = margin.max(lw);
            }
        }
        MarkupKind::Squiggly => {
            let _ = write!(content, "{} 1 j ", color_op(color, true));
            for q in quads {
                margin = margin.max(squiggle(&mut content, q));
            }
        }
    }
    let rect = Rect::new(
        bounds.x0 - margin,
        bounds.y0 - margin,
        bounds.x1 + margin,
        bounds.y1 + margin,
    );
    (rect, content.trim_end().to_string())
}

/// Annotation de balisage complète : type, zones, couleur, apparence.
fn markup_dict(
    doc: &Document,
    d: &mut Dict,
    kind: MarkupKind,
    quads: &[Rect],
    color: Rgb,
    contents: Option<&str>,
) -> Result<()> {
    if quads.is_empty() {
        return Err(Error::Corrupt("aucune zone de texte à baliser".into()));
    }
    let (rect, content) = markup_appearance(kind, quads, color);
    if let Some(c) = contents.filter(|c| !c.is_empty()) {
        d.insert(Name::new("Contents"), Object::String(encode_text(c)));
    }
    d.insert(
        Name::new("Subtype"),
        Object::Name(Name::new(kind.subtype())),
    );
    d.insert(Name::new("Rect"), rect_object(rect));
    d.insert(Name::new("C"), color_array(color));
    d.insert(Name::new("QuadPoints"), quad_points(quads));
    d.insert(
        Name::new("Subj"),
        Object::String(encode_text(kind.subject())),
    );
    let stream = if kind == MarkupKind::Highlight {
        // Le surligneur fonce le texte au lieu de le couvrir : fusion
        // « produit », dans un groupe de transparence (comme Acrobat).
        let mut gs = Dict::new();
        gs.insert(Name::new("BM"), Object::Name(Name::new("Multiply")));
        let mut ext = Dict::new();
        ext.insert(Name::new("GS0"), Object::Dict(gs));
        let mut res = Dict::new();
        res.insert(Name::new("ExtGState"), Object::Dict(ext));
        let mut group = Dict::new();
        group.insert(Name::new("S"), Object::Name(Name::new("Transparency")));
        appearance_stream(
            rect,
            content,
            &[
                ("Resources", Object::Dict(res)),
                ("Group", Object::Dict(group)),
            ],
        )
    } else {
        appearance_stream(rect, content, &[])
    };
    set_appearance(doc, d, stream);
    Ok(())
}

/// Rectangle et contenu du signe d'insertion : un « ^ » plein, à encoche,
/// centré sur `x`, la pointe juste au-dessus de la ligne de base, le corps
/// dessous — là où Acrobat le pose : il désigne l'interstice sans cacher
/// les lettres voisines. (Le bas de la boîte d'un glyphe est celui des
/// jambages ; la ligne de base est à un cinquième de sa hauteur environ.)
fn caret_appearance(x: f64, line: &Rect, color: Rgb) -> (Rect, String) {
    let h = line.height().max(1.0);
    let w = 0.45 * h;
    let (bottom, top) = (line.y0 - 0.12 * h, line.y0 + 0.33 * h);
    let rect = Rect::new(x - w / 2.0, bottom, x + w / 2.0, top);
    let notch = bottom + 0.25 * (top - bottom);
    let content = format!(
        "{} {x0} {b} m {x} {t} l {x1} {b} l {x} {n} l h f",
        color_op(color, false),
        x0 = fmt(rect.x0),
        x1 = fmt(rect.x1),
        x = fmt(x),
        b = fmt(bottom),
        t = fmt(top),
        n = fmt(notch)
    );
    (rect, content)
}

/// Annotation `/Caret` complète.
fn caret_dict(doc: &Document, d: &mut Dict, x: f64, line: &Rect, contents: &str, color: Rgb) {
    let (rect, content) = caret_appearance(x, line, color);
    d.insert(Name::new("Subtype"), Object::Name(Name::new("Caret")));
    d.insert(Name::new("Rect"), rect_object(rect));
    d.insert(Name::new("C"), color_array(color));
    // Pas de paragraphe (« ¶ ») : le signe seul (§12.5.6.11, table 184).
    d.insert(Name::new("Sy"), Object::Name(Name::new("None")));
    d.insert(Name::new("Contents"), Object::String(encode_text(contents)));
    d.insert(
        Name::new("Subj"),
        Object::String(encode_text("Insertion de texte")),
    );
    set_appearance(doc, d, appearance_stream(rect, content, &[]));
}

/// Page telle qu'elle est **maintenant** dans le document, et sa référence.
///
/// On relit l'objet plutôt que `page.dict` : une page collectée avant une
/// première annotation ne la connaît pas, et y ajouter la seconde perdrait
/// la première. L'objet relu est aussi celui du fichier, sans les attributs
/// hérités que `collect_pages` y recopie — ce qui n'est pas touché ne
/// change pas.
fn current_page(doc: &Document, page: &Page) -> Result<(ObjectRef, Dict)> {
    let page_ref = page
        .reference
        .ok_or_else(|| Error::Corrupt("la page doit être un objet indirect".into()))?;
    let dict = doc
        .get(page_ref)
        .ok()
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_else(|| page.dict.clone());
    Ok((page_ref, dict))
}

/// `/Annots` d'une page, en tableau direct (un tableau indirect est recopié).
fn annots_of(doc: &Document, page_dict: &Dict) -> Result<Vec<Object>> {
    Ok(match page_dict.get(&Name::new("Annots")) {
        Some(o) => doc
            .resolve(o)?
            .as_array()
            .map(<[Object]>::to_vec)
            .unwrap_or_default(),
        None => Vec::new(),
    })
}

/// Ajoute des annotations à `/Annots` (créé si absent), en une seule
/// réécriture de la page.
fn push_to_annots(doc: &Document, page: &Page, refs: &[ObjectRef]) -> Result<()> {
    let (page_ref, mut page_dict) = current_page(doc, page)?;
    let mut annots = annots_of(doc, &page_dict)?;
    annots.extend(refs.iter().map(|r| Object::Reference(*r)));
    page_dict.insert(Name::new("Annots"), Object::Array(annots));
    doc.set(page_ref, Object::Dict(page_dict));
    Ok(())
}

/// Ajoute une annotation à la page et retourne sa référence, avec une
/// identité neuve (voir [`add_annotation_with`] pour la fixer d'avance).
///
/// # Errors
/// La page n'est pas un objet indirect, `/Annots` est illisible, ou
/// l'annotation n'a aucune zone.
pub fn add_annotation(
    doc: &Document,
    page: &Page,
    annot: &NewAnnotation,
    author: Option<&str>,
) -> Result<ObjectRef> {
    add_annotation_with(doc, page, annot, &AnnotMeta::fresh(author))
}

/// Ajoute une annotation à la page, avec l'identité donnée, et retourne sa
/// référence — pour un remplacement, celle du signe d'insertion, membre
/// principal du groupe.
///
/// Même document, même annotation et même identité donnent le même
/// résultat : c'est ce qui permet de rejouer la modification.
///
/// # Errors
/// La page n'est pas un objet indirect, `/Annots` est illisible, ou
/// l'annotation n'a aucune zone.
#[allow(clippy::too_many_lines)] // une branche par type d'annotation
pub fn add_annotation_with(
    doc: &Document,
    page: &Page,
    annot: &NewAnnotation,
    meta: &AnnotMeta,
) -> Result<ObjectRef> {
    let (page_ref, _) = current_page(doc, page)?;
    // L'identité est complétée une fois pour toutes : un remplacement en
    // tire deux annotations, qui doivent partager la date et dériver leurs
    // identifiants du même nom.
    let (author, id, date) = meta.complete();
    let meta = AnnotMeta {
        author: author.map(ToString::to_string),
        name: Some(id.clone()),
        date: Some(date.clone()),
    };
    let mut d = base_dict(page_ref, &meta);
    match annot {
        NewAnnotation::Square {
            rect,
            stroke,
            width,
            fill,
            contents,
        } => {
            if let Some(c) = contents {
                d.insert(Name::new("Contents"), Object::String(encode_text(c)));
            }
            d.insert(Name::new("Subtype"), Object::Name(Name::new("Square")));
            d.insert(Name::new("Rect"), rect_object(*rect));
            d.insert(Name::new("C"), color_array(*stroke));
            if let Some(f) = fill {
                d.insert(Name::new("IC"), color_array(*f));
            }
            let mut bs = Dict::new();
            bs.insert(Name::new("W"), Object::Real(*width));
            d.insert(Name::new("BS"), Object::Dict(bs));
            let half = width / 2.0;
            let mut content = format!("{} {} w ", color_op(*stroke, true), fmt(*width));
            let op = if let Some(f) = fill {
                let _ = write!(content, "{} ", color_op(*f, false));
                "B"
            } else {
                "S"
            };
            let _ = write!(
                content,
                "{} {} {} {} re {op}",
                fmt(rect.x0 + half),
                fmt(rect.y0 + half),
                fmt(rect.width() - width),
                fmt(rect.height() - width)
            );
            set_appearance(doc, &mut d, appearance_stream(*rect, content, &[]));
        }
        NewAnnotation::Highlight {
            rect,
            color,
            contents,
        } => markup_dict(
            doc,
            &mut d,
            MarkupKind::Highlight,
            std::slice::from_ref(rect),
            *color,
            contents.as_deref(),
        )?,
        NewAnnotation::Markup {
            kind,
            quads,
            color,
            contents,
        } => markup_dict(doc, &mut d, *kind, quads, *color, contents.as_deref())?,
        NewAnnotation::Caret {
            x,
            line,
            contents,
            color,
        } => caret_dict(doc, &mut d, *x, line, contents, *color),
        NewAnnotation::Replace {
            quads,
            text,
            strike,
            caret,
        } => {
            // Le signe, au bout de la dernière ligne barrée : c'est le
            // membre principal, qui porte le texte proposé (§12.5.6.2).
            let last = quads
                .last()
                .ok_or_else(|| Error::Corrupt("aucune zone de texte à remplacer".into()))?;
            caret_dict(doc, &mut d, last.x1, last, text, *caret);
            d.insert(
                Name::new("Subj"),
                Object::String(encode_text("Remplacer le texte")),
            );
            d.insert(Name::new("IT"), Object::Name(Name::new("Replace")));
            let caret_ref = doc.add(Object::Dict(d));
            // Le barré suit le signe : même auteur, même date, un nom dérivé,
            // pas de texte à lui.
            let member = AnnotMeta {
                author: meta.author.clone(),
                name: Some(format!("{id}-1")),
                date: Some(date),
            };
            let mut s = base_dict(page_ref, &member);
            markup_dict(doc, &mut s, MarkupKind::StrikeOut, quads, *strike, None)?;
            s.insert(Name::new("IRT"), Object::Reference(caret_ref));
            s.insert(Name::new("RT"), Object::Name(Name::new("Group")));
            s.insert(
                Name::new("IT"),
                Object::Name(Name::new("StrikeOutTextEdit")),
            );
            let strike_ref = doc.add(Object::Dict(s));
            push_to_annots(doc, page, &[caret_ref, strike_ref])?;
            return Ok(caret_ref);
        }
        NewAnnotation::Note {
            x,
            y,
            contents,
            color,
        } => {
            let rect = Rect::new(*x, *y - 20.0, *x + 20.0, *y);
            d.insert(Name::new("Subtype"), Object::Name(Name::new("Text")));
            d.insert(Name::new("Rect"), rect_object(rect));
            d.insert(Name::new("Name"), Object::Name(Name::new("Comment")));
            d.insert(Name::new("Contents"), Object::String(encode_text(contents)));
            d.insert(Name::new("C"), color_array(*color));
            d.insert(Name::new("F"), Object::Integer(4 | 8 | 16)); // Print, NoZoom, NoRotate
                                                                   // Icône : bulle arrondie remplie avec trois lignes de « texte ».
            let (x0, y0) = (rect.x0, rect.y0);
            let content = format!(
                "{} 0.25 0.25 0.25 RG 0.8 w {x0} {y0} m {} {y0} l {} {} l {} {} l {x0} {} l h B 1 g 0.4 w {} {} m {} {} l S {} {} m {} {} l S {} {} m {} {} l S",
                color_op(*color, false),
                fmt(x0 + 20.0),
                fmt(x0 + 20.0),
                fmt(y0 + 14.0),
                fmt(x0 + 10.0),
                fmt(y0 + 20.0),
                fmt(y0 + 14.0),
                fmt(x0 + 4.0),
                fmt(y0 + 11.0),
                fmt(x0 + 16.0),
                fmt(y0 + 11.0),
                fmt(x0 + 4.0),
                fmt(y0 + 8.0),
                fmt(x0 + 16.0),
                fmt(y0 + 8.0),
                fmt(x0 + 4.0),
                fmt(y0 + 5.0),
                fmt(x0 + 12.0),
                fmt(y0 + 5.0),
                x0 = fmt(x0),
                y0 = fmt(y0)
            );
            set_appearance(doc, &mut d, appearance_stream(rect, content, &[]));
        }
        NewAnnotation::Link { rect, uri } => {
            d.insert(Name::new("Subtype"), Object::Name(Name::new("Link")));
            d.insert(Name::new("Rect"), rect_object(*rect));
            d.insert(
                Name::new("Border"),
                Object::Array(vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Integer(0),
                ]),
            );
            let mut a = Dict::new();
            a.insert(Name::new("S"), Object::Name(Name::new("URI")));
            a.insert(Name::new("URI"), Object::String(uri.clone().into_bytes()));
            d.insert(Name::new("A"), Object::Dict(a));
        }
    }
    let annot_ref = doc.add(Object::Dict(d));
    push_to_annots(doc, page, &[annot_ref])?;
    Ok(annot_ref)
}

/// Vrai si l'annotation `o` est un membre du groupe dont `primary` est le
/// membre principal (`/IRT` vers lui, `/RT /Group`).
fn is_member_of(doc: &Document, o: &Object, primary: ObjectRef) -> bool {
    let Ok(resolved) = doc.resolve(o) else {
        return false;
    };
    let Some(d) = resolved.as_dict() else {
        return false;
    };
    matches!(d.get(&Name::new("IRT")), Some(Object::Reference(r)) if *r == primary)
        && d.get(&Name::new("RT"))
            .and_then(Object::as_name)
            .is_some_and(|n| n.0 == b"Group")
}

/// Supprime l'annotation d'index donné (position dans `/Annots`) ; son popup
/// éventuel est supprimé aussi, et, si elle est le membre principal d'un
/// groupe (un remplacement), les autres membres avec elle : un texte barré
/// sans le signe qui disait par quoi le remplacer ne voudrait plus rien dire.
///
/// Les index des annotations suivantes peuvent donc reculer de plus d'un.
///
/// # Errors
/// Index invalide ou page non indirecte.
pub fn remove_annotation(doc: &Document, page: &Page, index: usize) -> Result<()> {
    let (page_ref, mut page_dict) = current_page(doc, page)?;
    let mut annots = annots_of(doc, &page_dict)?;
    if index >= annots.len() {
        return Err(Error::Corrupt(format!(
            "annotation {} inexistante",
            index + 1
        )));
    }
    let removed = annots.remove(index);
    if let Object::Reference(r) = removed {
        if let Ok(o) = doc.get(r) {
            if let Some(d) = o.as_dict() {
                if let Some(Object::Reference(popup)) = d.get(&Name::new("Popup")) {
                    annots.retain(|a| !matches!(a, Object::Reference(p) if p == popup));
                    doc.delete(*popup);
                }
            }
        }
        let mut kept = Vec::with_capacity(annots.len());
        for a in annots {
            if is_member_of(doc, &a, r) {
                if let Object::Reference(member) = a {
                    doc.delete(member);
                }
            } else {
                kept.push(a);
            }
        }
        annots = kept;
        doc.delete(r);
    }
    page_dict.insert(Name::new("Annots"), Object::Array(annots));
    doc.set(page_ref, Object::Dict(page_dict));
    Ok(())
}

/// Chaîne de texte PDF : PDFDoc si possible, sinon UTF-16BE avec BOM.
#[must_use]
pub fn encode_text(s: &str) -> Vec<u8> {
    if s.chars().all(|c| (c as u32) < 128) {
        return s.as_bytes().to_vec();
    }
    let mut out = vec![0xFE, 0xFF];
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_be_bytes());
    }
    out
}

/// Pages avec, pour chacune, ses annotations (utilitaire pour l'outil en ligne de commande).
///
/// # Errors
/// Document illisible.
pub fn list_all(doc: &Document) -> Result<Vec<(usize, Vec<AnnotationInfo>)>> {
    let pages = collect_pages(doc)?;
    let mut out = Vec::new();
    for p in &pages {
        out.push((p.index, list_annotations(doc, p)?));
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn doc() -> Document {
        Document::from_bytes(
            b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >> endobj\n".to_vec(),
        )
        .unwrap()
    }

    fn page0(d: &Document) -> Page {
        collect_pages(d).unwrap().remove(0)
    }

    fn reloaded(d: &Document) -> Document {
        Document::from_bytes(d.save_full().unwrap()).unwrap()
    }

    fn raw(d: &Document, info: &AnnotationInfo) -> Dict {
        d.get(info.reference.unwrap())
            .unwrap()
            .as_dict()
            .unwrap()
            .clone()
    }

    fn numbers(d: &Document, dict: &Dict, key: &str) -> Vec<f64> {
        d.dict_get(dict, key)
            .unwrap()
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Object::as_f64)
            .collect()
    }

    #[test]
    fn add_list_remove_roundtrip() {
        let d = doc();
        let page = collect_pages(&d).unwrap().remove(0);
        add_annotation(
            &d,
            &page,
            &NewAnnotation::Square {
                rect: Rect::new(10.0, 10.0, 60.0, 40.0),
                stroke: [1.0, 0.0, 0.0],
                width: 2.0,
                fill: Some([1.0, 1.0, 0.0]),
                contents: Some("À revoir".into()),
            },
            Some("Élodie"),
        )
        .unwrap();
        let page = collect_pages(&d).unwrap().remove(0);
        add_annotation(
            &d,
            &page,
            &NewAnnotation::Note {
                x: 100.0,
                y: 150.0,
                contents: "Bonjour à tous".into(),
                color: [1.0, 0.8, 0.0],
            },
            None,
        )
        .unwrap();
        let page = collect_pages(&d).unwrap().remove(0);
        add_annotation(
            &d,
            &page,
            &NewAnnotation::Highlight {
                rect: Rect::new(0.0, 0.0, 50.0, 10.0),
                color: [1.0, 1.0, 0.0],
                contents: None,
            },
            None,
        )
        .unwrap();
        let page = collect_pages(&d).unwrap().remove(0);
        add_annotation(
            &d,
            &page,
            &NewAnnotation::Link {
                rect: Rect::new(0.0, 0.0, 50.0, 10.0),
                uri: "https://example.org".into(),
            },
            None,
        )
        .unwrap();
        // Enregistrement complet puis relecture.
        let saved = d.save_full().unwrap();
        let d2 = Document::from_bytes(saved).unwrap();
        let page = collect_pages(&d2).unwrap().remove(0);
        let list = list_annotations(&d2, &page).unwrap();
        assert_eq!(list.len(), 4);
        assert_eq!(list[0].subtype, "Square");
        assert_eq!(list[0].author.as_deref(), Some("Élodie"));
        assert!(list[0].has_appearance);
        assert!(list[0].modified.as_deref().unwrap().starts_with("D:20"));
        assert_eq!(list[1].subtype, "Text");
        assert_eq!(list[1].contents.as_deref(), Some("Bonjour à tous"));
        assert_eq!(list[2].subtype, "Highlight");
        assert_eq!(list[3].uri.as_deref(), Some("https://example.org"));
        assert!(!list[3].has_appearance);
        // Le rendu prend en compte les apparences (le carré rouge/jaune est visible).
        let rendered =
            acrux_render::render_page(&d2, &page, 1.0, &acrux_render::RenderOptions::default());
        let px = rendered.bitmap.pixel(35, 200 - 25).unwrap();
        assert!(
            px[0] > 200 && px[1] > 200 && px[2] < 60,
            "remplissage jaune attendu, obtenu {px:?}"
        );
        // Suppression.
        remove_annotation(&d2, &page, 1).unwrap();
        let page = collect_pages(&d2).unwrap().remove(0);
        let list = list_annotations(&d2, &page).unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[1].subtype, "Highlight");
        assert!(remove_annotation(&d2, &page, 9).is_err());
    }

    #[test]
    fn date_and_text_encoding() {
        let d = pdf_date_now();
        assert!(
            d.starts_with("D:20") && d.ends_with('Z') && d.len() == 17,
            "{d}"
        );
        assert_eq!(encode_text("abc"), b"abc");
        assert_eq!(encode_text("é"), vec![0xFE, 0xFF, 0x00, 0xE9]);
    }

    fn markup(kind: MarkupKind, quads: Vec<Rect>) -> NewAnnotation {
        NewAnnotation::Markup {
            kind,
            quads,
            color: kind.default_color(),
            contents: None,
        }
    }

    /// Les quatre balisages : une annotation chacun, un quadrilatère par
    /// ligne, une identité propre, relus à l'identique après
    /// enregistrement.
    #[test]
    fn markup_quatre_types_aller_retour() {
        let d = doc();
        let two = vec![
            Rect::new(10.0, 150.0, 120.0, 162.0),
            Rect::new(10.0, 135.0, 80.0, 147.0),
        ];
        let page = page0(&d);
        add_annotation(&d, &page, &markup(MarkupKind::Underline, two.clone()), None).unwrap();
        // La même `page`, périmée : la première annotation ne doit pas se
        // perdre (la page est relue avant chaque ajout).
        add_annotation(
            &d,
            &page,
            &markup(
                MarkupKind::StrikeOut,
                vec![Rect::new(10.0, 100.0, 120.0, 112.0)],
            ),
            None,
        )
        .unwrap();
        add_annotation(
            &d,
            &page,
            &markup(
                MarkupKind::Squiggly,
                vec![Rect::new(10.0, 60.0, 120.0, 72.0)],
            ),
            None,
        )
        .unwrap();
        add_annotation(
            &d,
            &page,
            &NewAnnotation::Markup {
                kind: MarkupKind::Highlight,
                quads: vec![
                    Rect::new(10.0, 30.0, 120.0, 42.0),
                    Rect::new(10.0, 15.0, 60.0, 27.0),
                ],
                color: [1.0, 1.0, 0.0],
                contents: Some("À vérifier".into()),
            },
            Some("Zoé"),
        )
        .unwrap();
        let d2 = reloaded(&d);
        let list = list_annotations(&d2, &page0(&d2)).unwrap();
        let kinds: Vec<&str> = list.iter().map(|a| a.subtype.as_str()).collect();
        assert_eq!(kinds, ["Underline", "StrikeOut", "Squiggly", "Highlight"]);
        let mut names: Vec<&str> = list.iter().filter_map(|a| a.name.as_deref()).collect();
        assert_eq!(names.len(), 4, "chaque annotation a son /NM");
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 4, "des /NM tous différents");
        for a in &list {
            assert!(a.has_appearance, "{} sans apparence", a.subtype);
            let dict = raw(&d2, a);
            let created = string(&d2, &dict, "CreationDate").unwrap();
            assert!(created.starts_with("D:20"), "{created}");
            assert_eq!(a.modified.as_deref(), Some(created.as_str()));
        }
        let underline = raw(&d2, &list[0]);
        assert_eq!(numbers(&d2, &underline, "QuadPoints").len(), 16);
        let color = numbers(&d2, &underline, "C");
        let expected = MarkupKind::Underline.default_color();
        assert_eq!(color.len(), 3);
        for (got, want) in color.iter().zip(expected) {
            assert!((got - want).abs() < 1e-6, "{color:?}");
        }
        assert_eq!(numbers(&d2, &raw(&d2, &list[3]), "QuadPoints").len(), 16);
        assert_eq!(list[3].contents.as_deref(), Some("À vérifier"));
        assert_eq!(list[3].author.as_deref(), Some("Zoé"));
    }

    /// Les traits sont là où on les attend : sous le texte souligné, au
    /// milieu du texte barré, en bas pour l'ondulé — et le haut des lignes
    /// soulignées reste blanc.
    #[test]
    fn rendu_des_traits() {
        let d = doc();
        let page = page0(&d);
        let under = Rect::new(20.0, 150.0, 180.0, 170.0);
        let strike = Rect::new(20.0, 100.0, 180.0, 120.0);
        let wave = Rect::new(20.0, 50.0, 180.0, 70.0);
        for (kind, q) in [
            (MarkupKind::Underline, under),
            (MarkupKind::StrikeOut, strike),
            (MarkupKind::Squiggly, wave),
        ] {
            add_annotation(&d, &page, &markup(kind, vec![q]), None).unwrap();
        }
        let d2 = reloaded(&d);
        let bmp = acrux_render::render_page(
            &d2,
            &page0(&d2),
            2.0,
            &acrux_render::RenderOptions::default(),
        )
        .bitmap;
        // Pixel (x, y) en points de page, origine en bas.
        let at = |x: f64, y: f64| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let (px, py) = ((x * 2.0) as u32, ((200.0 - y) * 2.0) as u32);
            bmp.pixel(px, py).unwrap()
        };
        let blue = at(100.0, 151.2);
        assert!(
            blue[2] > 150 && blue[0] < 80,
            "soulignement bleu : {blue:?}"
        );
        let red = at(100.0, 110.0);
        assert!(red[0] > 150 && red[1] < 80, "barré rouge : {red:?}");
        // L'onde passe par toute la bande basse : on y cherche du vert.
        let green = (0..24)
            .map(|i| at(40.0 + f64::from(i) * 0.5, 51.2))
            .chain((0..24).map(|i| at(40.0 + f64::from(i) * 0.5, 52.0)))
            .any(|p| p[1] > 100 && p[0] < 100 && p[2] < 100);
        assert!(green, "soulignement ondulé vert introuvable");
        for y in [165.0, 65.0] {
            let top = at(100.0, y);
            assert!(
                top.iter().take(3).all(|&c| c > 240),
                "haut touché : {top:?}"
            );
        }
    }

    /// Sans leur apparence — un fichier d'un autre logiciel qui ne l'écrit
    /// pas —, l'onde et le signe d'insertion se dessinent quand même, au
    /// même endroit.
    #[test]
    fn rendu_de_secours_sans_apparence() {
        let d = doc();
        let wave = Rect::new(20.0, 50.0, 180.0, 70.0);
        add_annotation(
            &d,
            &page0(&d),
            &markup(MarkupKind::Squiggly, vec![wave]),
            None,
        )
        .unwrap();
        add_annotation(
            &d,
            &page0(&d),
            &NewAnnotation::Caret {
                x: 100.0,
                line: Rect::new(40.0, 100.0, 46.0, 120.0),
                contents: "ajout".into(),
                color: CARET_COLOR,
            },
            None,
        )
        .unwrap();
        for a in list_annotations(&d, &page0(&d)).unwrap() {
            let mut dict = raw(&d, &a);
            dict.remove(&Name::new("AP"));
            d.set(a.reference.unwrap(), Object::Dict(dict));
        }
        let d2 = reloaded(&d);
        assert!(list_annotations(&d2, &page0(&d2))
            .unwrap()
            .iter()
            .all(|a| !a.has_appearance));
        let bmp = acrux_render::render_page(
            &d2,
            &page0(&d2),
            2.0,
            &acrux_render::RenderOptions::default(),
        )
        .bitmap;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let at = |x: f64, y: f64| {
            bmp.pixel((x * 2.0) as u32, ((200.0 - y) * 2.0) as u32)
                .unwrap()
        };
        let green = (0..24)
            .flat_map(|i| {
                [
                    at(40.0 + f64::from(i) * 0.5, 51.2),
                    at(40.0 + f64::from(i) * 0.5, 52.0),
                ]
            })
            .any(|p| p[1] > 100 && p[0] < 100 && p[2] < 100);
        assert!(green, "onde de secours introuvable");
        let px = at(100.0, 104.0);
        assert!(
            px[2] > 150 && px[0] < 80,
            "signe de secours bleu attendu : {px:?}"
        );
    }

    /// Un remplacement : un signe qui porte le texte, un barré groupé avec
    /// lui ; supprimer le signe emporte le barré.
    #[test]
    fn remplacer_forme_un_groupe() {
        let d = doc();
        let meta = AnnotMeta {
            author: Some("Relecteur".into()),
            name: Some("acrux-essai".into()),
            date: Some("D:20260924120000Z".into()),
        };
        let quads = vec![
            Rect::new(10.0, 150.0, 120.0, 162.0),
            Rect::new(10.0, 135.0, 60.0, 147.0),
        ];
        let caret = add_annotation_with(
            &d,
            &page0(&d),
            &NewAnnotation::Replace {
                quads,
                text: "nouveau".into(),
                strike: MarkupKind::StrikeOut.default_color(),
                caret: CARET_COLOR,
            },
            &meta,
        )
        .unwrap();
        let d2 = reloaded(&d);
        let list = list_annotations(&d2, &page0(&d2)).unwrap();
        assert_eq!(list.len(), 2);
        let (c, s) = (&list[0], &list[1]);
        assert_eq!(c.subtype, "Caret");
        assert_eq!(c.contents.as_deref(), Some("nouveau"));
        assert_eq!(c.name.as_deref(), Some("acrux-essai"));
        assert_eq!(c.intent.as_deref(), Some("Replace"));
        assert!(!c.is_group_member());
        assert_eq!(s.subtype, "StrikeOut");
        assert!(s.is_group_member());
        assert_eq!(s.in_reply_to, c.reference);
        assert_eq!(s.reply_type.as_deref(), Some("Group"));
        assert_eq!(s.intent.as_deref(), Some("StrikeOutTextEdit"));
        assert_eq!(s.contents, None);
        assert_eq!(s.name.as_deref(), Some("acrux-essai-1"));
        assert_eq!(s.author.as_deref(), Some("Relecteur"));
        assert_eq!(s.modified.as_deref(), Some("D:20260924120000Z"));
        assert_eq!(c.reference.map(|r| r.number), Some(caret.number));
        // Le signe se pose au bout de la dernière ligne barrée.
        assert!((f64::midpoint(c.rect.x0, c.rect.x1) - 60.0).abs() < 1e-6);
        remove_annotation(&d2, &page0(&d2), 0).unwrap();
        assert!(list_annotations(&d2, &page0(&d2)).unwrap().is_empty());
    }

    /// Le signe d'insertion est centré sur le point d'insertion et se voit.
    #[test]
    fn caret_apparence() {
        let d = doc();
        let line = Rect::new(40.0, 100.0, 46.0, 120.0);
        add_annotation(
            &d,
            &page0(&d),
            &NewAnnotation::Caret {
                x: 100.0,
                line,
                contents: "ajout".into(),
                color: CARET_COLOR,
            },
            None,
        )
        .unwrap();
        let d2 = reloaded(&d);
        let list = list_annotations(&d2, &page0(&d2)).unwrap();
        assert_eq!(list[0].subtype, "Caret");
        let r = list[0].rect;
        assert!((f64::midpoint(r.x0, r.x1) - 100.0).abs() < 1e-6, "{r:?}");
        assert!(r.y0 < 100.0 && r.y1 < 110.0, "{r:?}");
        let bmp = acrux_render::render_page(
            &d2,
            &page0(&d2),
            2.0,
            &acrux_render::RenderOptions::default(),
        )
        .bitmap;
        // Au centre du signe, au-dessus de l'encoche.
        let px = bmp.pixel(200, 2 * (200 - 104)).unwrap();
        assert!(px[2] > 150 && px[0] < 80, "signe bleu attendu : {px:?}");
    }

    /// La même modification, avec la même identité, donne la même
    /// annotation sur deux copies ; sans identité fournie, un /NM est tiré.
    #[test]
    fn meta_deterministe() {
        let meta = AnnotMeta::fresh(Some("A"));
        let annot = markup(MarkupKind::Underline, vec![Rect::new(1.0, 1.0, 50.0, 10.0)]);
        let (a, b) = (doc(), doc());
        add_annotation_with(&a, &page0(&a), &annot, &meta).unwrap();
        add_annotation_with(&b, &page0(&b), &annot, &meta).unwrap();
        let la = list_annotations(&a, &page0(&a)).unwrap();
        let lb = list_annotations(&b, &page0(&b)).unwrap();
        assert_eq!(la[0].name, lb[0].name);
        assert_eq!(la[0].modified, lb[0].modified);
        assert_eq!(la[0].name, meta.name);
        let c = doc();
        add_annotation(&c, &page0(&c), &annot, None).unwrap();
        let lc = list_annotations(&c, &page0(&c)).unwrap();
        assert!(lc[0]
            .name
            .as_deref()
            .is_some_and(|n| n.starts_with("acrux-")));
    }

    #[test]
    fn identifiants_uniques() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..10_000 {
            let id = new_annotation_id();
            assert!(id.starts_with("acrux-"), "{id}");
            assert!(seen.insert(id), "identifiant en double");
        }
    }

    /// Une zone aberrante ne fait pas exploser le flux de l'onde.
    #[test]
    fn squiggly_zone_immense() {
        let (_, content) = markup_appearance(
            MarkupKind::Squiggly,
            &[Rect::new(0.0, 0.0, 1.0e6, 12.0)],
            [0.0, 0.0, 0.0],
        );
        assert!(content.len() < 200_000, "{} octets", content.len());
    }

    /// Une zone vide n'est pas une annotation : l'erreur le dit.
    #[test]
    fn balisage_sans_zone_refuse() {
        let d = doc();
        assert!(
            add_annotation(&d, &page0(&d), &markup(MarkupKind::StrikeOut, vec![]), None).is_err()
        );
        let replace = NewAnnotation::Replace {
            quads: Vec::new(),
            text: "x".into(),
            strike: [1.0, 0.0, 0.0],
            caret: CARET_COLOR,
        };
        assert!(add_annotation(&d, &page0(&d), &replace, None).is_err());
        assert!(list_annotations(&d, &page0(&d)).unwrap().is_empty());
    }
}
