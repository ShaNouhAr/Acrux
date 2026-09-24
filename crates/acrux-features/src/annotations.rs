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

use acrux_core::{Error, Point, Rect, Result};
use acrux_document::text::decode_text_string;
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef, Page};

use crate::stamp::StandardFont;

pub mod appearance;
mod edit;
pub mod freetext;
pub mod review;
pub mod shapes;

pub use edit::{set_annotation_properties, set_hidden, AnnotChanges};

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
    /// Couleur principale, ramenée en RVB : `/C` d'une forme (son trait),
    /// d'un balisage (sa teinte), d'une note (son icône) ; pour une zone de
    /// texte, la couleur **du texte**, lue dans `/DA` — son `/C` est son
    /// fond. C'est la couleur que la barre de propriétés montre et change.
    pub color: Option<Rgb>,
    /// Couleur de fond : `/IC` d'une forme, `/C` d'une zone de texte.
    pub fill: Option<Rgb>,
    /// Opacité `/CA`, 1 par défaut.
    pub opacity: f64,
    /// Épaisseur du trait : `/BS /W`, sinon le troisième nombre de
    /// `/Border` ; `None` quand l'annotation n'en dit rien.
    pub border_width: Option<f64>,
    /// `/Popup` : la fenêtre contextuelle de l'annotation.
    pub popup: Option<ObjectRef>,
    /// Zones d'un balisage (`/QuadPoints`), chacune ramenée au rectangle
    /// qui la contient ; vide pour les autres annotations.
    pub quads: Vec<Rect>,
    /// `/State` : état posé par une annotation d'état (`Accepted`,
    /// `Marked`…), §12.5.6.3.
    pub state: Option<String>,
    /// `/StateModel` : `Review` ou `Marked`.
    pub state_model: Option<String>,
    /// Posée par « remplir et signer » : elle se gère là, pas ici.
    pub fill_sign: bool,
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

    /// Vrai pour une annotation d'état (`/IRT` et `/State`) : elle donne un
    /// statut à celle qu'elle vise, elle n'est pas un commentaire.
    #[must_use]
    pub fn is_state(&self) -> bool {
        self.in_reply_to.is_some() && self.state.as_deref().is_some_and(|s| !s.is_empty())
    }

    /// Vrai pour une réponse : `/IRT` sans état ni groupe.
    #[must_use]
    pub fn is_reply(&self) -> bool {
        self.in_reply_to.is_some() && !self.is_group_member() && !self.is_state()
    }

    /// Vrai si l'annotation est cachée (bit 2 de `/F`).
    #[must_use]
    pub fn hidden(&self) -> bool {
        self.flags & 2 != 0
    }
}

/// Couleur RVB 0..1.
pub type Rgb = [f64; 3];

/// Couleur du signe d'insertion : le bleu des outils de relecture d'Acrobat.
pub const CARET_COLOR: Rgb = [0.0, 0.47, 0.84];

/// Rouge des formes, par défaut : celui des outils de dessin d'Acrobat.
pub const SHAPE_COLOR: Rgb = [0.9, 0.13, 0.13];

/// Aspect d'une forme : couleur du trait, couleur de fond, épaisseur du
/// trait (`/BS /W`) et opacité (`/CA`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapeStyle {
    /// Couleur du trait (`/C`) ; aucun trait si `None`.
    pub stroke: Option<Rgb>,
    /// Couleur de fond (`/IC`) ; transparent si `None`.
    pub fill: Option<Rgb>,
    /// Épaisseur du trait, en points.
    pub width: f64,
    /// Opacité, de 0 (invisible) à 1 (opaque).
    pub opacity: f64,
}

impl Default for ShapeStyle {
    /// Trait rouge de 2 pt, sans fond, opaque.
    fn default() -> Self {
        Self {
            stroke: Some(SHAPE_COLOR),
            fill: None,
            width: 2.0,
            opacity: 1.0,
        }
    }
}

impl ShapeStyle {
    /// Le même, ramené dans des bornes raisonnables : épaisseur de 0 à
    /// 72 pt, opacité de 0 à 1, composantes de 0 à 1 ; une valeur qui n'est
    /// pas un nombre reprend celle par défaut. Ce qui vient d'une ligne de
    /// commande ou d'un fichier ne doit pas produire un flux absurde.
    #[must_use]
    pub fn clamped(&self) -> Self {
        let d = Self::default();
        let rgb = |c: Rgb| {
            c.map(|v| {
                if v.is_finite() {
                    v.clamp(0.0, 1.0)
                } else {
                    0.0
                }
            })
        };
        Self {
            stroke: self.stroke.map(rgb),
            fill: self.fill.map(rgb),
            width: if self.width.is_finite() {
                self.width.clamp(0.0, 72.0)
            } else {
                d.width
            },
            opacity: if self.opacity.is_finite() {
                self.opacity.clamp(0.0, 1.0)
            } else {
                d.opacity
            },
        }
    }
}

/// Terminaison d'une ligne (`/LE`, §12.5.6.7, table 179).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    /// Aucune.
    #[default]
    None,
    /// Flèche ouverte : deux traits.
    OpenArrow,
    /// Flèche fermée : un triangle plein.
    ClosedArrow,
    /// Disque.
    Circle,
    /// Carré.
    Square,
    /// Butée : un trait perpendiculaire.
    Butt,
}

impl LineEnding {
    /// Toutes, dans l'ordre de la table.
    pub const ALL: [LineEnding; 6] = [
        LineEnding::None,
        LineEnding::OpenArrow,
        LineEnding::ClosedArrow,
        LineEnding::Circle,
        LineEnding::Square,
        LineEnding::Butt,
    ];

    /// Nom PDF.
    #[must_use]
    pub fn pdf_name(self) -> &'static str {
        match self {
            LineEnding::None => "None",
            LineEnding::OpenArrow => "OpenArrow",
            LineEnding::ClosedArrow => "ClosedArrow",
            LineEnding::Circle => "Circle",
            LineEnding::Square => "Square",
            LineEnding::Butt => "Butt",
        }
    }

    /// Depuis le nom PDF ; un nom inconnu (`Diamond`, `Slash`…) vaut
    /// `None` plutôt qu'une erreur : la ligne se dessine quand même.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|e| e.pdf_name() == name)
            .unwrap_or_default()
    }
}

/// Ligne d'ancrage d'une légende : du point désigné, par un coude
/// facultatif, jusqu'au bord de la zone de texte.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Callout {
    /// Point désigné, où se pose la flèche.
    pub anchor: Point,
    /// Coude ; calculé s'il n'est pas donné (voir
    /// [`freetext::callout_points`]).
    pub knee: Option<Point>,
    /// Terminaison côté ancre.
    pub ending: LineEnding,
}

/// Alignement des lignes d'une zone de texte (`/Q`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlign {
    /// À gauche.
    #[default]
    Left,
    /// Centré.
    Center,
    /// À droite.
    Right,
}

impl TextAlign {
    /// Valeur de `/Q`.
    #[must_use]
    pub fn q(self) -> i64 {
        match self {
            TextAlign::Left => 0,
            TextAlign::Center => 1,
            TextAlign::Right => 2,
        }
    }
}

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
        /// Trait, remplissage, épaisseur, opacité.
        style: ShapeStyle,
        /// Commentaire associé (`/Contents`), comme sur n'importe quelle
        /// annotation de balisage : c'est lui que les relecteurs lisent.
        contents: Option<String>,
    },
    /// Ellipse (`/Circle`) inscrite dans une zone.
    Circle {
        /// Zone.
        rect: Rect,
        /// Trait, remplissage, épaisseur, opacité.
        style: ShapeStyle,
        /// Commentaire associé.
        contents: Option<String>,
    },
    /// Ligne droite (`/Line`), avec ses terminaisons : une flèche est une
    /// ligne dont le bout est une tête.
    Line {
        /// Origine.
        from: Point,
        /// Extrémité.
        to: Point,
        /// Trait (le remplissage colore l'intérieur des têtes fermées).
        style: ShapeStyle,
        /// Terminaison à l'origine.
        start: LineEnding,
        /// Terminaison à l'extrémité.
        end: LineEnding,
        /// Commentaire associé.
        contents: Option<String>,
    },
    /// Ligne brisée ouverte (`/PolyLine`).
    PolyLine {
        /// Sommets, au moins deux.
        points: Vec<Point>,
        /// Trait.
        style: ShapeStyle,
        /// Terminaison au premier sommet.
        start: LineEnding,
        /// Terminaison au dernier.
        end: LineEnding,
        /// Commentaire associé.
        contents: Option<String>,
    },
    /// Polygone fermé (`/Polygon`).
    Polygon {
        /// Sommets, au moins trois.
        points: Vec<Point>,
        /// Trait et remplissage.
        style: ShapeStyle,
        /// Commentaire associé.
        contents: Option<String>,
    },
    /// Dessin à main levée (`/Ink`) : un ou plusieurs traits, qui forment
    /// **une** annotation, comme le crayon d'Acrobat.
    Ink {
        /// Traits, chacun une suite de points relevés sous le pointeur ;
        /// ils sont simplifiés à l'écriture.
        strokes: Vec<Vec<Point>>,
        /// Trait.
        style: ShapeStyle,
        /// Commentaire associé.
        contents: Option<String>,
    },
    /// Zone de texte (`/FreeText`) : un texte écrit sur la page, dans une
    /// police standard, avec cadre et fond facultatifs ; avec `callout`,
    /// une légende reliée à un point par une ligne fléchée.
    FreeText {
        /// Zone du texte.
        rect: Rect,
        /// Texte, sauts de ligne compris.
        text: String,
        /// Police standard.
        font: StandardFont,
        /// Corps en points.
        size: f64,
        /// Couleur du texte.
        color: Rgb,
        /// Cadre : couleur et épaisseur ; aucun si `None`.
        border: Option<(Rgb, f64)>,
        /// Fond ; transparent si `None`.
        fill: Option<Rgb>,
        /// Alignement des lignes.
        align: TextAlign,
        /// Ligne d'ancrage d'une légende.
        callout: Option<Callout>,
        /// Rotation de la page **à l'affichage**, en degrés (sens horaire,
        /// comme `/Rotate`) : le texte s'écrit droit pour qui la regarde
        /// ainsi (voir [`freetext::upright`]). 0 pour une page droite.
        rotation: i32,
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
        let popup = match d.get(&Name::new("Popup")) {
            Some(Object::Reference(r)) => Some(*r),
            _ => None,
        };
        let free_text = subtype == "FreeText";
        let color = if free_text {
            string(doc, d, "DA").and_then(|da| appearance::parse_da(&da).fill)
        } else {
            numbers(doc, d, "C").and_then(|c| color_from(&c))
        };
        let fill = numbers(doc, d, if free_text { "C" } else { "IC" }).and_then(|c| color_from(&c));
        let quads = numbers(doc, d, "QuadPoints")
            .map(|q| quads_from(&q))
            .unwrap_or_default();
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
            color,
            fill,
            opacity: num(doc, d, "CA").map_or(1.0, |a| a.clamp(0.0, 1.0)),
            border_width: border_width(doc, d),
            popup,
            quads,
            state: string_or_name(doc, d, "State"),
            state_model: string_or_name(doc, d, "StateModel"),
            fill_sign: d.contains_key(&Name::new(crate::fillsign::TAG)),
        });
    }
    Ok(out)
}

/// Nombres d'un tableau de l'annotation (`/C`, `/QuadPoints`, `/L`…) ;
/// `None` si la clé manque ou n'est pas un tableau.
pub(crate) fn numbers(doc: &Document, d: &Dict, key: &str) -> Option<Vec<f64>> {
    let o = doc.dict_get(d, key).ok().flatten()?;
    let a = o.as_array()?;
    Some(
        a.iter()
            .filter_map(|v| doc.resolve(v).ok().and_then(|x| x.as_f64()))
            .collect(),
    )
}

/// Couleur d'un tableau `/C` ou `/IC` ramenée en RVB : gris, RVB ou CMJN
/// (§12.5.2, table 166). Un tableau vide veut dire « transparent ».
pub(crate) fn color_from(c: &[f64]) -> Option<Rgb> {
    let unit = |v: f64| {
        if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    match c {
        [g] => Some([unit(*g); 3]),
        [r, g, b] => Some([unit(*r), unit(*g), unit(*b)]),
        [c, m, y, k] => {
            let k = unit(*k);
            Some([
                (1.0 - unit(*c)) * (1.0 - k),
                (1.0 - unit(*m)) * (1.0 - k),
                (1.0 - unit(*y)) * (1.0 - k),
            ])
        }
        _ => None,
    }
}

/// `/QuadPoints` en rectangles : huit nombres par zone, dont on garde le
/// rectangle qui les contient (un quadrilatère penché s'y inscrit).
pub(crate) fn quads_from(q: &[f64]) -> Vec<Rect> {
    q.chunks_exact(8)
        .map(|c| {
            let xs = [c[0], c[2], c[4], c[6]];
            let ys = [c[1], c[3], c[5], c[7]];
            Rect::new(
                xs.iter().copied().fold(f64::INFINITY, f64::min),
                ys.iter().copied().fold(f64::INFINITY, f64::min),
                xs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                ys.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            )
        })
        .collect()
}

/// Épaisseur du trait : `/BS /W`, sinon le troisième nombre de `/Border`.
pub(crate) fn border_width(doc: &Document, d: &Dict) -> Option<f64> {
    let bs = doc
        .dict_get(d, "BS")
        .ok()
        .flatten()
        .and_then(|bs| bs.as_dict().and_then(|b| num(doc, b, "W")));
    bs.or_else(|| numbers(doc, d, "Border").and_then(|b| b.get(2).copied()))
        .filter(|w| w.is_finite())
        .map(|w| w.max(0.0))
}

/// Chaîne de texte ou nom : `/State` et `/StateModel` sont des chaînes
/// (§12.5.6.3, table 175), mais des logiciels les écrivent en noms.
fn string_or_name(doc: &Document, d: &Dict, key: &str) -> Option<String> {
    string(doc, d, key).or_else(|| name(doc, d, key))
}

/// Date PDF `D:AAAAMMJJHHmmSS…` rendue lisible (« 2024-03-12 10:30 ») ; la
/// chaîne est rendue telle quelle si elle n'a pas cette forme — les fichiers
/// réels en contiennent de toutes sortes.
///
/// La ligne de commande et l'application la partagent : une même date se lit
/// pareil dans `acr annots`, dans le panneau des commentaires et dans la
/// bulle d'une note.
#[must_use]
pub fn readable_date(raw: &str) -> String {
    let digits: Vec<char> = raw.trim_start_matches("D:").chars().collect();
    if digits.len() < 8 || !digits[..8].iter().all(char::is_ascii_digit) {
        return raw.to_string();
    }
    let part = |a: usize, b: usize| -> String { digits[a..b.min(digits.len())].iter().collect() };
    let date = format!("{}-{}-{}", part(0, 4), part(4, 6), part(6, 8));
    if digits.len() >= 12 && digits[8..12].iter().all(char::is_ascii_digit) {
        return format!("{date} {}:{}", part(8, 10), part(10, 12));
    }
    date
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

/// Icône d'une note (`/Text`) dans son rectangle : une bulle de la couleur
/// de la note, cernée de gris, et trois lignes de « texte ».
///
/// Le dessin est pensé pour 20 × 20 points, la taille qu'Acrux donne à ses
/// notes, et suit le rectangle quand il est autre : une note d'un autre
/// logiciel dont on change la couleur garde sa taille.
pub(crate) fn note_icon(rect: Rect, color: Rgb) -> String {
    let (x0, y0) = (rect.x0, rect.y0);
    let (u, v) = (rect.width() / 20.0, rect.height() / 20.0);
    let x = |t: f64| fmt(x0 + t * u);
    let y = |t: f64| fmt(y0 + t * v);
    format!(
        "{} 0.25 0.25 0.25 RG 0.8 w {} {} m {} {} l {} {} l {} {} l {} {} l h B 1 g 0.4 w {} {} m {} {} l S {} {} m {} {} l S {} {} m {} {} l S",
        color_op(color, false),
        x(0.0),
        y(0.0),
        x(20.0),
        y(0.0),
        x(20.0),
        y(14.0),
        x(10.0),
        y(20.0),
        x(0.0),
        y(14.0),
        x(4.0),
        y(11.0),
        x(16.0),
        y(11.0),
        x(4.0),
        y(8.0),
        x(16.0),
        y(8.0),
        x(4.0),
        y(5.0),
        x(12.0),
        y(5.0),
    )
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

/// Tableau de nombres `[x y x y …]` pour une suite de points (`/Vertices`,
/// un trait de `/InkList`).
fn points_array(points: &[Point]) -> Object {
    Object::Array(
        points
            .iter()
            .flat_map(|p| [Object::Real(p.x), Object::Real(p.y)])
            .collect(),
    )
}

/// `/LE [/début /fin]`.
fn endings_array(start: LineEnding, end: LineEnding) -> Object {
    Object::Array(vec![
        Object::Name(Name::new(start.pdf_name())),
        Object::Name(Name::new(end.pdf_name())),
    ])
}

/// Flux d'apparence d'une forme : son contenu, et l'état graphique `GS0`
/// qui porte l'opacité quand elle n'est pas pleine.
fn shape_stream(bbox: Rect, content: String, opacity: f64) -> Object {
    if opacity >= 1.0 {
        return appearance_stream(bbox, content, &[]);
    }
    let mut gs = Dict::new();
    gs.insert(Name::new("CA"), Object::Real(opacity));
    gs.insert(Name::new("ca"), Object::Real(opacity));
    let mut ext = Dict::new();
    ext.insert(Name::new("GS0"), Object::Dict(gs));
    let mut res = Dict::new();
    res.insert(Name::new("ExtGState"), Object::Dict(ext));
    appearance_stream(bbox, content, &[("Resources", Object::Dict(res))])
}

/// Dictionnaire commun aux formes : type, sujet, boîte, couleurs (`/C` du
/// trait, `/IC` du fond), épaisseur (`/BS`), opacité (`/CA`) et apparence.
///
/// Les clés sémantiques sont écrites en plus de l'apparence : un lecteur
/// qui la régénère (après un changement de couleur, par exemple) les lit.
fn shape_dict(
    doc: &Document,
    d: &mut Dict,
    (subtype, subject): (&str, &str),
    style: &ShapeStyle,
    contents: Option<&str>,
    drawn: shapes::Drawn,
) {
    if let Some(c) = contents.filter(|c| !c.is_empty()) {
        d.insert(Name::new("Contents"), Object::String(encode_text(c)));
    }
    d.insert(Name::new("Subtype"), Object::Name(Name::new(subtype)));
    d.insert(Name::new("Subj"), Object::String(encode_text(subject)));
    d.insert(Name::new("Rect"), rect_object(drawn.bbox));
    // Sans trait, un tableau vide : « transparent » (§12.5.2, table 166).
    d.insert(
        Name::new("C"),
        style
            .stroke
            .map_or_else(|| Object::Array(Vec::new()), color_array),
    );
    if let Some(f) = style.fill {
        d.insert(Name::new("IC"), color_array(f));
    }
    let mut bs = Dict::new();
    bs.insert(Name::new("W"), Object::Real(style.width));
    bs.insert(Name::new("S"), Object::Name(Name::new("S")));
    d.insert(Name::new("BS"), Object::Dict(bs));
    if style.opacity < 1.0 {
        d.insert(Name::new("CA"), Object::Real(style.opacity));
    }
    let stream = shape_stream(drawn.bbox, drawn.content, style.opacity);
    set_appearance(doc, d, stream);
}

/// Annotation `/Line` complète : `/L`, `/LE`, et l'intention « flèche »
/// quand un bout porte une tête.
fn line_dict(
    doc: &Document,
    d: &mut Dict,
    [from, to]: [Point; 2],
    style: &ShapeStyle,
    (start, end): (LineEnding, LineEnding),
    contents: Option<&str>,
) -> Result<()> {
    if (to.x - from.x).hypot(to.y - from.y) < 1e-6 {
        return Err(Error::Corrupt(
            "une ligne demande deux points distincts".into(),
        ));
    }
    let style = style.clamped();
    let drawn = shapes::open_path(&[from, to], &style, start, end)
        .ok_or_else(|| Error::Corrupt("ligne sans longueur".into()))?;
    let arrow = [start, end]
        .iter()
        .any(|e| matches!(e, LineEnding::OpenArrow | LineEnding::ClosedArrow));
    let subject = if arrow { "Flèche" } else { "Ligne" };
    shape_dict(doc, d, ("Line", subject), &style, contents, drawn);
    d.insert(Name::new("L"), points_array(&[from, to]));
    d.insert(Name::new("LE"), endings_array(start, end));
    if arrow {
        d.insert(Name::new("IT"), Object::Name(Name::new("LineArrow")));
    }
    Ok(())
}

/// Annotation `/Ink` complète : les traits sont simplifiés une fois, et
/// `/InkList` garde ces points — c'est ce que redessinent les lecteurs qui
/// n'utilisent pas l'apparence.
fn ink_dict(
    doc: &Document,
    d: &mut Dict,
    strokes: &[Vec<Point>],
    style: &ShapeStyle,
    contents: Option<&str>,
) -> Result<()> {
    let style = style.clamped();
    let simplified: Vec<Vec<Point>> = strokes
        .iter()
        .map(|s| shapes::simplify(s, shapes::INK_TOLERANCE))
        .filter(|s| !s.is_empty())
        .collect();
    let drawn = shapes::ink(&simplified, &style)
        .ok_or_else(|| Error::Corrupt("un dessin demande au moins un point".into()))?;
    shape_dict(doc, d, ("Ink", "Crayon"), &style, contents, drawn);
    d.insert(
        Name::new("InkList"),
        Object::Array(simplified.iter().map(|s| points_array(s)).collect()),
    );
    Ok(())
}

/// Annotation `/FreeText` complète : texte (`/Contents`), style (`/DA`,
/// `/DS`, `/Q`), cadre (`/BS`), fond (`/C`, la couleur de fond pour une
/// zone de texte, comme chez Acrobat), et pour une légende la ligne
/// d'ancrage (`/CL`, `/LE`, `/RD`).
fn freetext_dict(
    doc: &Document,
    d: &mut Dict,
    tb: &freetext::TextBox<'_>,
    callout: Option<&Callout>,
    rotation: i32,
) {
    let rotation = freetext::quarter_turns(rotation);
    let (content, bbox) = freetext::appearance(tb, callout, rotation);
    d.insert(Name::new("Subtype"), Object::Name(Name::new("FreeText")));
    d.insert(Name::new("Rect"), rect_object(bbox));
    d.insert(Name::new("Contents"), Object::String(encode_text(tb.text)));
    d.insert(
        Name::new("DA"),
        Object::String(freetext::default_appearance(tb).into_bytes()),
    );
    d.insert(
        Name::new("DS"),
        Object::String(encode_text(&freetext::default_style(tb))),
    );
    d.insert(Name::new("Q"), Object::Integer(tb.align.q()));
    // Le sens du texte, pour les lecteurs qui refont l'apparence d'après
    // `/DA` et `/Contents` (Acrobat écrit la même clé).
    if rotation != 0 {
        d.insert(Name::new("Rotate"), Object::Integer(i64::from(rotation)));
    }
    let mut bs = Dict::new();
    bs.insert(Name::new("W"), Object::Real(tb.border_width()));
    d.insert(Name::new("BS"), Object::Dict(bs));
    if let Some(f) = tb.fill {
        d.insert(Name::new("C"), color_array(f));
    }
    if let Some(call) = callout {
        d.insert(Name::new("IT"), Object::Name(Name::new("FreeTextCallout")));
        d.insert(Name::new("Subj"), Object::String(encode_text("Légende")));
        d.insert(
            Name::new("CL"),
            points_array(&freetext::callout_points_turned(&tb.rect, call, rotation)),
        );
        d.insert(
            Name::new("LE"),
            Object::Name(Name::new(call.ending.pdf_name())),
        );
        d.insert(
            Name::new("RD"),
            Object::Array(
                freetext::rect_differences(&bbox, &tb.rect)
                    .into_iter()
                    .map(Object::Real)
                    .collect(),
            ),
        );
    } else {
        d.insert(Name::new("IT"), Object::Name(Name::new("FreeText")));
        d.insert(
            Name::new("Subj"),
            Object::String(encode_text("Zone de texte")),
        );
    }
    let mut res = Dict::new();
    res.insert(
        Name::new("Font"),
        Object::Dict(freetext::font_resources(tb.font)),
    );
    set_appearance(
        doc,
        d,
        appearance_stream(bbox, content, &[("Resources", Object::Dict(res))]),
    );
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
            style,
            contents,
        } => {
            let style = style.clamped();
            let drawn = shapes::rectangle(rect, &style);
            shape_dict(
                doc,
                &mut d,
                ("Square", "Rectangle"),
                &style,
                contents.as_deref(),
                drawn,
            );
        }
        NewAnnotation::Circle {
            rect,
            style,
            contents,
        } => {
            let style = style.clamped();
            let drawn = shapes::ellipse(rect, &style);
            shape_dict(
                doc,
                &mut d,
                ("Circle", "Ellipse"),
                &style,
                contents.as_deref(),
                drawn,
            );
        }
        NewAnnotation::Line {
            from,
            to,
            style,
            start,
            end,
            contents,
        } => line_dict(
            doc,
            &mut d,
            [*from, *to],
            style,
            (*start, *end),
            contents.as_deref(),
        )?,
        NewAnnotation::PolyLine {
            points,
            style,
            start,
            end,
            contents,
        } => {
            let style = style.clamped();
            let drawn = shapes::open_path(points, &style, *start, *end).ok_or_else(|| {
                Error::Corrupt("une ligne brisée demande au moins deux points".into())
            })?;
            shape_dict(
                doc,
                &mut d,
                ("PolyLine", "Ligne brisée"),
                &style,
                contents.as_deref(),
                drawn,
            );
            d.insert(Name::new("Vertices"), points_array(points));
            d.insert(Name::new("LE"), endings_array(*start, *end));
        }
        NewAnnotation::Polygon {
            points,
            style,
            contents,
        } => {
            let style = style.clamped();
            let drawn = shapes::polygon(points, &style).ok_or_else(|| {
                Error::Corrupt("un polygone demande au moins trois sommets".into())
            })?;
            shape_dict(
                doc,
                &mut d,
                ("Polygon", "Polygone"),
                &style,
                contents.as_deref(),
                drawn,
            );
            d.insert(Name::new("Vertices"), points_array(points));
        }
        NewAnnotation::Ink {
            strokes,
            style,
            contents,
        } => ink_dict(doc, &mut d, strokes, style, contents.as_deref())?,
        NewAnnotation::FreeText {
            rect,
            text,
            font,
            size,
            color,
            border,
            fill,
            align,
            callout,
            rotation,
        } => {
            let size = if size.is_finite() {
                size.clamp(1.0, 144.0)
            } else {
                12.0
            };
            let tb = freetext::TextBox {
                rect: *rect,
                text,
                font: *font,
                size,
                color: *color,
                border: *border,
                fill: *fill,
                align: *align,
            };
            freetext_dict(doc, &mut d, &tb, callout.as_ref(), *rotation);
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
            set_appearance(
                doc,
                &mut d,
                appearance_stream(rect, note_icon(rect, *color), &[]),
            );
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

/// Références vers lesquelles pointe une annotation de `/Annots` : celle
/// qu'elle vise par `/IRT` (réponse, état, membre de groupe) et, pour une
/// fenêtre contextuelle, son annotation mère (`/Parent`).
fn attached_to(doc: &Document, o: &Object) -> Vec<ObjectRef> {
    let Ok(resolved) = doc.resolve(o) else {
        return Vec::new();
    };
    let Some(d) = resolved.as_dict() else {
        return Vec::new();
    };
    ["IRT", "Parent"]
        .iter()
        .filter_map(|key| match d.get(&Name::new(key)) {
            Some(Object::Reference(r)) => Some(*r),
            _ => None,
        })
        .collect()
}

/// Supprime l'annotation d'index donné (position dans `/Annots`), et tout ce
/// qui n'a de sens qu'avec elle : sa fenêtre contextuelle, les autres
/// membres de son groupe (un texte barré sans le signe qui disait par quoi
/// le remplacer ne voudrait plus rien dire), ses réponses et leurs réponses,
/// ses annotations d'état — comme Acrobat, où supprimer un commentaire
/// emporte son fil.
///
/// Les index des annotations suivantes peuvent donc reculer de plus d'un.
///
/// # Errors
/// Index invalide ou page non indirecte.
pub fn remove_annotation(doc: &Document, page: &Page, index: usize) -> Result<()> {
    let (page_ref, mut page_dict) = current_page(doc, page)?;
    let annots = annots_of(doc, &page_dict)?;
    let Some(removed) = annots.get(index) else {
        return Err(Error::Corrupt(format!(
            "annotation {} inexistante",
            index + 1
        )));
    };
    let mut kept: Vec<Object> = annots
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != index)
        .map(|(_, a)| a.clone())
        .collect();
    if let Object::Reference(r) = removed {
        // Tout ce qui s'y rattache, de proche en proche : on collecte
        // d'abord, on filtre `/Annots` une seule fois ensuite. L'ensemble
        // grandit à chaque tour ou la boucle s'arrête : un cycle d'`/IRT`
        // ne la fait pas tourner sans fin.
        let mut gone: std::collections::HashSet<ObjectRef> = std::iter::once(*r).collect();
        if let Some(Object::Reference(popup)) = doc.get(*r).ok().and_then(|o| {
            o.as_dict()
                .and_then(|d| d.get(&Name::new("Popup")).cloned())
        }) {
            gone.insert(popup);
        }
        loop {
            let before = gone.len();
            for a in &kept {
                let Object::Reference(me) = a else { continue };
                if gone.contains(me) {
                    continue;
                }
                if attached_to(doc, a).iter().any(|t| gone.contains(t)) {
                    gone.insert(*me);
                    if let Some(Object::Reference(popup)) = doc.get(*me).ok().and_then(|o| {
                        o.as_dict()
                            .and_then(|d| d.get(&Name::new("Popup")).cloned())
                    }) {
                        gone.insert(popup);
                    }
                }
            }
            if gone.len() == before {
                break;
            }
        }
        kept.retain(|a| !matches!(a, Object::Reference(x) if gone.contains(x)));
        for x in gone {
            doc.delete(x);
        }
    }
    page_dict.insert(Name::new("Annots"), Object::Array(kept));
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
#[allow(clippy::unwrap_used, clippy::panic, clippy::float_cmp)]
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
                style: ShapeStyle {
                    stroke: Some([1.0, 0.0, 0.0]),
                    fill: Some([1.0, 1.0, 0.0]),
                    width: 2.0,
                    opacity: 1.0,
                },
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

    /// Rendu d'une page à 2 px par point, et lecture d'un pixel en points
    /// de page (origine en bas, page de 200 × 200).
    fn rendered(d: &Document) -> impl Fn(f64, f64) -> [u8; 4] {
        let d2 = reloaded(d);
        let bmp = acrux_render::render_page(
            &d2,
            &page0(&d2),
            2.0,
            &acrux_render::RenderOptions::default(),
        )
        .bitmap;
        move |x: f64, y: f64| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let (px, py) = ((x * 2.0) as u32, ((200.0 - y) * 2.0) as u32);
            let p = bmp.pixel(px, py).unwrap();
            [p[0], p[1], p[2], p[3]]
        }
    }

    fn style(stroke: Rgb) -> ShapeStyle {
        ShapeStyle {
            stroke: Some(stroke),
            fill: None,
            width: 2.0,
            opacity: 1.0,
        }
    }

    fn is_red(p: [u8; 4]) -> bool {
        p[0] > 180 && p[1] < 90 && p[2] < 90
    }

    fn is_white(p: [u8; 4]) -> bool {
        p.iter().take(3).all(|&c| c > 240)
    }

    /// Une ellipse rouge sans fond : le bord est rouge, le centre blanc, et
    /// tout se relit après enregistrement.
    #[test]
    fn circle_aller_retour() {
        let d = doc();
        let rect = Rect::new(40.0, 60.0, 160.0, 140.0);
        add_annotation(
            &d,
            &page0(&d),
            &NewAnnotation::Circle {
                rect,
                style: style([1.0, 0.0, 0.0]),
                contents: Some("Cercle".into()),
            },
            Some("Zoé"),
        )
        .unwrap();
        let d2 = reloaded(&d);
        let list = list_annotations(&d2, &page0(&d2)).unwrap();
        assert_eq!(list[0].subtype, "Circle");
        assert!(list[0].has_appearance);
        assert_eq!(list[0].rect, rect);
        assert_eq!(list[0].contents.as_deref(), Some("Cercle"));
        let at = rendered(&d);
        assert!(
            is_red(at(40.8, 100.0)),
            "bord gauche : {:?}",
            at(40.8, 100.0)
        );
        assert!(
            is_white(at(100.0, 100.0)),
            "centre : {:?}",
            at(100.0, 100.0)
        );
        // Le coin de la zone est hors de l'ellipse.
        assert!(is_white(at(42.0, 62.0)), "coin : {:?}", at(42.0, 62.0));
    }

    /// Fond jaune à moitié transparent : le blanc de la page transparaît,
    /// et l'état graphique de l'apparence porte l'opacité.
    #[test]
    fn opacite_du_fond() {
        let d = doc();
        add_annotation(
            &d,
            &page0(&d),
            &NewAnnotation::Square {
                rect: Rect::new(20.0, 20.0, 180.0, 180.0),
                style: ShapeStyle {
                    stroke: Some([1.0, 0.0, 0.0]),
                    fill: Some([1.0, 1.0, 0.0]),
                    width: 2.0,
                    opacity: 0.5,
                },
                contents: None,
            },
            None,
        )
        .unwrap();
        let d2 = reloaded(&d);
        let info = &list_annotations(&d2, &page0(&d2)).unwrap()[0];
        let dict = raw(&d2, info);
        assert_eq!(num(&d2, &dict, "CA"), Some(0.5));
        let ap = d2.dict_get(&dict, "AP").unwrap().unwrap();
        let normal = d2.dict_get(ap.as_dict().unwrap(), "N").unwrap().unwrap();
        let Object::Stream { dict: form, .. } = &*normal else {
            panic!("flux attendu");
        };
        let res = d2.dict_get(form, "Resources").unwrap().unwrap();
        let ext = d2
            .dict_get(res.as_dict().unwrap(), "ExtGState")
            .unwrap()
            .unwrap();
        let gs = d2.dict_get(ext.as_dict().unwrap(), "GS0").unwrap().unwrap();
        assert_eq!(num(&d2, gs.as_dict().unwrap(), "ca"), Some(0.5));
        let at = rendered(&d);
        let px = at(100.0, 100.0);
        assert!(
            px[0] > 240 && px[1] > 240 && (115..=140).contains(&px[2]),
            "jaune à demi transparent : {px:?}"
        );
    }

    /// Une flèche : `/L`, `/LE`, une tête visible sur ses deux ailes, et
    /// une boîte qui la contient.
    #[test]
    fn fleche_ouverte() {
        let d = doc();
        add_annotation(
            &d,
            &page0(&d),
            &NewAnnotation::Line {
                from: Point::new(20.0, 100.0),
                to: Point::new(180.0, 100.0),
                style: style([1.0, 0.0, 0.0]),
                start: LineEnding::None,
                end: LineEnding::OpenArrow,
                contents: None,
            },
            None,
        )
        .unwrap();
        let d2 = reloaded(&d);
        let info = &list_annotations(&d2, &page0(&d2)).unwrap()[0];
        assert_eq!(info.subtype, "Line");
        assert_eq!(info.intent.as_deref(), Some("LineArrow"));
        let dict = raw(&d2, info);
        assert_eq!(numbers(&d2, &dict, "L"), [20.0, 100.0, 180.0, 100.0]);
        let le = d2.dict_get(&dict, "LE").unwrap().unwrap();
        let names: Vec<String> = le
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|o| o.as_name().map(Name::as_str))
            .collect();
        assert_eq!(names, ["None", "OpenArrow"]);
        // La tête dépasse la ligne en hauteur.
        assert!(
            info.rect.y1 > 104.0 && info.rect.y0 < 96.0,
            "{:?}",
            info.rect
        );
        let at = rendered(&d);
        assert!(is_red(at(100.0, 100.0)), "ligne : {:?}", at(100.0, 100.0));
        // Aile haute de la tête : 8 pt en arrière de la pointe, 30°.
        let wing = (0..12)
            .map(|i| at(174.0 + f64::from(i) * 0.25, 101.0 + f64::from(i) * 0.25))
            .any(is_red);
        assert!(wing, "aile de la flèche introuvable");
        assert!(is_white(at(100.0, 110.0)));
    }

    /// Une flèche fermée est un triangle plein, de la couleur de fond s'il y
    /// en a une.
    #[test]
    fn fleche_fermee_remplie() {
        let d = doc();
        add_annotation(
            &d,
            &page0(&d),
            &NewAnnotation::Line {
                from: Point::new(20.0, 100.0),
                to: Point::new(180.0, 100.0),
                style: ShapeStyle {
                    stroke: Some([1.0, 0.0, 0.0]),
                    fill: Some([0.0, 0.0, 1.0]),
                    width: 4.0,
                    opacity: 1.0,
                },
                start: LineEnding::ClosedArrow,
                end: LineEnding::ClosedArrow,
                contents: None,
            },
            None,
        )
        .unwrap();
        let at = rendered(&d);
        // Dans le triangle de droite (longueur 16 pt), près de la base.
        let inside = at(168.0, 101.5);
        assert!(
            inside[2] > 180 && inside[0] < 90,
            "intérieur bleu attendu : {inside:?}"
        );
        let inside = at(32.0, 98.5);
        assert!(
            inside[2] > 180 && inside[0] < 90,
            "tête de départ : {inside:?}"
        );
    }

    /// Polygone fermé et ligne brisée : `/Vertices` de 2n nombres ; seul le
    /// polygone trace le segment qui revient au départ.
    #[test]
    fn polygone_et_ligne_brisee() {
        let tri = vec![
            Point::new(20.0, 20.0),
            Point::new(100.0, 180.0),
            Point::new(180.0, 20.0),
        ];
        for closed in [true, false] {
            let d = doc();
            let annot = if closed {
                NewAnnotation::Polygon {
                    points: tri.clone(),
                    style: style([1.0, 0.0, 0.0]),
                    contents: None,
                }
            } else {
                NewAnnotation::PolyLine {
                    points: tri.clone(),
                    style: style([1.0, 0.0, 0.0]),
                    start: LineEnding::None,
                    end: LineEnding::None,
                    contents: None,
                }
            };
            add_annotation(&d, &page0(&d), &annot, None).unwrap();
            let d2 = reloaded(&d);
            let info = &list_annotations(&d2, &page0(&d2)).unwrap()[0];
            assert_eq!(info.subtype, if closed { "Polygon" } else { "PolyLine" });
            assert_eq!(numbers(&d2, &raw(&d2, info), "Vertices").len(), 6);
            let at = rendered(&d);
            // Le bas du triangle : le segment de fermeture.
            assert_eq!(is_red(at(100.0, 20.5)), closed, "fermeture {closed}");
            assert!(is_red(at(60.0, 100.0)), "côté gauche");
        }
        let d = doc();
        assert!(add_annotation(
            &d,
            &page0(&d),
            &NewAnnotation::Polygon {
                points: tri[..2].to_vec(),
                style: style([1.0, 0.0, 0.0]),
                contents: None,
            },
            None
        )
        .is_err());
    }

    /// Un trait de 500 points presque droit est simplifié, ses bouts
    /// gardés ; deux traits font une seule annotation à deux tableaux ; un
    /// point seul laisse une trace.
    #[test]
    fn encre_simplifiee() {
        let d = doc();
        let wobbly: Vec<Point> = (0..500)
            .map(|i| {
                let t = f64::from(i) / 499.0;
                Point::new(20.0 + 160.0 * t, 60.0 + (t * 40.0).sin() * 0.05)
            })
            .collect();
        add_annotation(
            &d,
            &page0(&d),
            &NewAnnotation::Ink {
                strokes: vec![wobbly.clone(), vec![Point::new(100.0, 150.0)]],
                style: style([0.0, 0.0, 1.0]),
                contents: None,
            },
            None,
        )
        .unwrap();
        let d2 = reloaded(&d);
        let info = &list_annotations(&d2, &page0(&d2)).unwrap()[0];
        assert_eq!(info.subtype, "Ink");
        let dict = raw(&d2, info);
        let ink = d2.dict_get(&dict, "InkList").unwrap().unwrap();
        let strokes = ink.as_array().unwrap();
        assert_eq!(strokes.len(), 2);
        let first: Vec<f64> = strokes[0]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Object::as_f64)
            .collect();
        assert!(first.len() < 100, "{} nombres", first.len());
        assert_eq!(first[0], 20.0);
        assert!((first[first.len() - 2] - 180.0).abs() < 1e-9);
        let at = rendered(&d);
        let blue = |p: [u8; 4]| p[2] > 180 && p[0] < 90;
        assert!(blue(at(100.0, 60.0)), "trait : {:?}", at(100.0, 60.0));
        assert!(blue(at(100.0, 150.0)), "point : {:?}", at(100.0, 150.0));
    }

    fn free_text(
        text: &str,
        border: Option<(Rgb, f64)>,
        callout: Option<Callout>,
    ) -> NewAnnotation {
        NewAnnotation::FreeText {
            rect: Rect::new(40.0, 100.0, 160.0, 140.0),
            text: text.into(),
            font: StandardFont::Helvetica,
            size: 12.0,
            color: [0.0, 0.0, 1.0],
            border,
            fill: None,
            align: TextAlign::Left,
            callout,
            rotation: 0,
        }
    }

    /// Une zone de texte : `/DA`, `/Q`, `/DS`, le texte relu (accents
    /// compris), des pixels bleus dans la zone, le cadre rouge.
    #[test]
    fn zone_de_texte() {
        let d = doc();
        add_annotation(
            &d,
            &page0(&d),
            &free_text("Élan à revoir\nWWWW", Some(([1.0, 0.0, 0.0], 1.0)), None),
            None,
        )
        .unwrap();
        let d2 = reloaded(&d);
        let info = &list_annotations(&d2, &page0(&d2)).unwrap()[0];
        assert_eq!(info.subtype, "FreeText");
        assert_eq!(info.intent.as_deref(), Some("FreeText"));
        assert_eq!(info.contents.as_deref(), Some("Élan à revoir\nWWWW"));
        let dict = raw(&d2, info);
        assert_eq!(
            string(&d2, &dict, "DA").as_deref(),
            Some("0 0 1 rg /Helv 12 Tf 1 0 0 RG")
        );
        assert_eq!(num(&d2, &dict, "Q"), Some(0.0));
        assert!(string(&d2, &dict, "DS").is_some_and(|s| s.contains("Helvetica 12pt")));
        let at = rendered(&d);
        assert!(is_red(at(40.5, 120.0)), "cadre : {:?}", at(40.5, 120.0));
        // Deuxième ligne : des W, bien pleins, sous la première.
        let ink = (0..60)
            .flat_map(|i| (0..8).map(move |j| (44.0 + f64::from(i) * 0.5, 114.0 + f64::from(j))))
            .map(|(x, y)| at(x, y))
            .filter(|p| p[2] > 150 && p[0] < 100)
            .count();
        assert!(ink > 20, "texte bleu introuvable ({ink} pixels)");
    }

    /// Une légende : l'intention, `/CL` qui part de l'ancre, `/RD`, la
    /// flèche ouverte, et la ligne visible à mi-chemin.
    #[test]
    fn legende() {
        let d = doc();
        let call = Callout {
            anchor: Point::new(100.0, 40.0),
            knee: None,
            ending: LineEnding::OpenArrow,
        };
        add_annotation(
            &d,
            &page0(&d),
            &free_text("Voir ici", None, Some(call)),
            None,
        )
        .unwrap();
        let d2 = reloaded(&d);
        let info = &list_annotations(&d2, &page0(&d2)).unwrap()[0];
        assert_eq!(info.intent.as_deref(), Some("FreeTextCallout"));
        let dict = raw(&d2, info);
        let cl = numbers(&d2, &dict, "CL");
        assert_eq!(cl.len(), 6, "{cl:?}");
        assert_eq!(&cl[..2], [100.0, 40.0]);
        // Jonction au milieu du bas de la zone, coude 12 pt dessous.
        assert_eq!(&cl[4..], [100.0, 100.0]);
        assert_eq!(&cl[2..4], [100.0, 88.0]);
        let rd = numbers(&d2, &dict, "RD");
        assert_eq!(rd.len(), 4);
        assert!(rd.iter().all(|v| *v >= 0.0) && rd[1] > 50.0, "{rd:?}");
        assert!(info.rect.y0 < 40.0, "{:?}", info.rect);
        let at = rendered(&d);
        let blue = |p: [u8; 4]| p[2] > 150 && p[0] < 100;
        assert!(
            (0..6).any(|i| blue(at(99.5 + f64::from(i) * 0.2, 70.0))),
            "ligne d'ancrage : {:?}",
            at(100.0, 70.0)
        );
    }

    /// Un caractère hors de WinAnsi devient « ? » sans erreur, et il est
    /// nommé d'avance ; le texte relu, lui, reste entier.
    #[test]
    fn zone_de_texte_hors_winansi() {
        assert_eq!(freetext::unsupported_chars("Ω = 1"), vec!['Ω']);
        let d = doc();
        add_annotation(&d, &page0(&d), &free_text("Ω = 1", None, None), None).unwrap();
        let d2 = reloaded(&d);
        let info = &list_annotations(&d2, &page0(&d2)).unwrap()[0];
        assert_eq!(info.contents.as_deref(), Some("Ω = 1"));
    }

    #[test]
    fn noms_des_terminaisons() {
        for e in LineEnding::ALL {
            assert_eq!(LineEnding::from_name(e.pdf_name()), e);
        }
        assert_eq!(LineEnding::from_name("Diamond"), LineEnding::None);
    }

    /// Une ligne sans longueur ou un dessin sans point sont refusés, et le
    /// document reste intact.
    #[test]
    fn formes_vides_refusees() {
        let d = doc();
        let p = Point::new(10.0, 10.0);
        for annot in [
            NewAnnotation::Line {
                from: p,
                to: p,
                style: ShapeStyle::default(),
                start: LineEnding::None,
                end: LineEnding::OpenArrow,
                contents: None,
            },
            NewAnnotation::Ink {
                strokes: vec![Vec::new()],
                style: ShapeStyle::default(),
                contents: None,
            },
        ] {
            assert!(add_annotation(&d, &page0(&d), &annot, None).is_err());
        }
        assert!(list_annotations(&d, &page0(&d)).unwrap().is_empty());
        // Des valeurs aberrantes sont ramenées dans leurs bornes.
        let wild = ShapeStyle {
            stroke: Some([2.0, -1.0, f64::NAN]),
            fill: None,
            width: 1e9,
            opacity: f64::NAN,
        }
        .clamped();
        assert_eq!(wild.stroke, Some([1.0, 0.0, 0.0]));
        assert_eq!(wild.width, 72.0);
        assert_eq!(wild.opacity, 1.0);
    }

    // --- Gérer les commentaires : modifier, répondre, statuer, supprimer ---

    /// Un carré rouge de 2 pt posé en (40, 40)-(80, 80).
    fn square(d: &Document) -> usize {
        add_annotation(
            d,
            &page0(d),
            &NewAnnotation::Square {
                rect: Rect::new(40.0, 40.0, 80.0, 80.0),
                style: style([1.0, 0.0, 0.0]),
                contents: Some("Carré".into()),
            },
            Some("Zoé"),
        )
        .unwrap();
        list_annotations(d, &page0(d)).unwrap().len() - 1
    }

    /// Ajoute une annotation écrite à la main (sans apparence) et rend son
    /// rang.
    fn raw_annotation(d: &Document, entries: &[(&str, Object)]) -> usize {
        let mut dict = Dict::new();
        dict.insert(Name::new("Type"), Object::Name(Name::new("Annot")));
        for (k, v) in entries {
            dict.insert(Name::new(k), v.clone());
        }
        let r = d.add(Object::Dict(dict));
        push_to_annots(d, &page0(d), &[r]).unwrap();
        list_annotations(d, &page0(d)).unwrap().len() - 1
    }

    fn reals(values: &[f64]) -> Object {
        Object::Array(values.iter().map(|v| Object::Real(*v)).collect())
    }

    fn name_obj(n: &str) -> Object {
        Object::Name(Name::new(n))
    }

    /// Flux `/AP /N` d'une annotation.
    fn normal_stream(d: &Document, a: &AnnotationInfo) -> (ObjectRef, Dict, Vec<u8>) {
        let ap = d
            .dict_get(&raw(d, a), "AP")
            .unwrap()
            .unwrap()
            .as_dict()
            .unwrap()
            .clone();
        let Some(Object::Reference(n)) = ap.get(&Name::new("N")).cloned() else {
            panic!("apparence indirecte attendue")
        };
        let o = d.get(n).unwrap();
        let Object::Stream { dict, .. } = &*o else {
            panic!("flux attendu")
        };
        let data = d.stream_data(&o).unwrap().data;
        (n, dict.clone(), data)
    }

    /// Déplacer un carré le déplace, l'agrandir le redessine : le trait
    /// garde son épaisseur, l'ancienne place redevient blanche, et tout
    /// tient après enregistrement.
    #[test]
    fn un_carre_se_deplace_et_s_agrandit() {
        let d = doc();
        let i = square(&d);
        let changes = AnnotChanges {
            rect: Some(Rect::new(100.0, 100.0, 140.0, 140.0)),
            date: Some("D:20260924120000Z".into()),
            ..AnnotChanges::default()
        };
        set_annotation_properties(&d, &page0(&d), i, &changes).unwrap();
        let a = &list_annotations(&d, &page0(&d)).unwrap()[i];
        assert_eq!(a.rect, Rect::new(100.0, 100.0, 140.0, 140.0));
        assert_eq!(a.modified.as_deref(), Some("D:20260924120000Z"));
        let at = rendered(&d);
        assert!(
            is_red(at(100.5, 120.0)),
            "bord gauche : {:?}",
            at(100.5, 120.0)
        );
        assert!(
            is_white(at(40.5, 60.0)),
            "ancienne place : {:?}",
            at(40.5, 60.0)
        );
        // Agrandi : l'apparence est refaite, le trait reste de 2 pt.
        let bigger = AnnotChanges {
            rect: Some(Rect::new(100.0, 100.0, 180.0, 180.0)),
            ..AnnotChanges::default()
        };
        set_annotation_properties(&d, &page0(&d), i, &bigger).unwrap();
        let d2 = reloaded(&d);
        let a = &list_annotations(&d2, &page0(&d2)).unwrap()[i];
        assert_eq!(a.rect, Rect::new(100.0, 100.0, 180.0, 180.0));
        let (_, dict, _) = normal_stream(&d2, a);
        assert_eq!(
            numbers(&d2, &dict, "BBox"),
            vec![100.0, 100.0, 180.0, 180.0]
        );
        let at = rendered(&d2);
        assert!(is_red(at(100.8, 140.0)), "{:?}", at(100.8, 140.0));
        assert!(
            is_white(at(103.5, 140.0)),
            "trait resté fin : {:?}",
            at(103.5, 140.0)
        );
    }

    /// Une encre, une ligne et un polygone d'un autre logiciel (sans
    /// apparence) : leur géométrie suit le rectangle, en translation comme
    /// en échelle.
    #[test]
    fn la_geometrie_suit_le_rectangle() {
        let d = doc();
        let ink = raw_annotation(
            &d,
            &[
                ("Subtype", name_obj("Ink")),
                ("Rect", reals(&[10.0, 10.0, 30.0, 30.0])),
                (
                    "InkList",
                    Object::Array(vec![reals(&[10.0, 10.0, 30.0, 30.0])]),
                ),
                ("C", reals(&[0.0, 0.0, 1.0])),
            ],
        );
        let line = raw_annotation(
            &d,
            &[
                ("Subtype", name_obj("Line")),
                ("Rect", reals(&[10.0, 50.0, 30.0, 70.0])),
                ("L", reals(&[10.0, 50.0, 30.0, 70.0])),
                ("C", reals(&[0.0, 0.0, 0.0])),
            ],
        );
        let poly = raw_annotation(
            &d,
            &[
                ("Subtype", name_obj("Polygon")),
                ("Rect", reals(&[10.0, 100.0, 30.0, 120.0])),
                ("Vertices", reals(&[10.0, 100.0, 30.0, 100.0, 20.0, 120.0])),
            ],
        );
        // Encre : translation de (+50, +5).
        let moved = AnnotChanges {
            rect: Some(Rect::new(60.0, 15.0, 80.0, 35.0)),
            ..AnnotChanges::default()
        };
        set_annotation_properties(&d, &page0(&d), ink, &moved).unwrap();
        let list = list_annotations(&d, &page0(&d)).unwrap();
        let dict = raw(&d, &list[ink]);
        let strokes = d.dict_get(&dict, "InkList").unwrap().unwrap();
        let first: Vec<f64> = strokes.as_array().unwrap()[0]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Object::as_f64)
            .collect();
        assert_eq!(first, vec![60.0, 15.0, 80.0, 35.0]);
        assert!(!list[ink].has_appearance, "un déplacement ne redessine pas");
        // Ligne : échelle ×2 autour de son coin bas-gauche.
        let doubled = AnnotChanges {
            rect: Some(Rect::new(10.0, 50.0, 50.0, 90.0)),
            ..AnnotChanges::default()
        };
        set_annotation_properties(&d, &page0(&d), line, &doubled).unwrap();
        let list = list_annotations(&d, &page0(&d)).unwrap();
        assert_eq!(
            numbers(&d, &raw(&d, &list[line]), "L"),
            vec![10.0, 50.0, 50.0, 90.0]
        );
        // L'apparence refaite couvre la ligne entière, trait compris.
        assert!(list[line].has_appearance);
        assert!(list[line].rect.x1 >= 50.0 && list[line].rect.y1 >= 90.0);
        // Polygone : translation.
        let shifted = AnnotChanges {
            rect: Some(Rect::new(110.0, 100.0, 130.0, 120.0)),
            ..AnnotChanges::default()
        };
        set_annotation_properties(&d, &page0(&d), poly, &shifted).unwrap();
        let list = list_annotations(&d, &page0(&d)).unwrap();
        assert_eq!(
            numbers(&d, &raw(&d, &list[poly]), "Vertices"),
            vec![110.0, 100.0, 130.0, 100.0, 120.0, 120.0]
        );
    }

    /// Un surlignage ne se déplace pas — il suit le texte — mais change de
    /// couleur, son apparence refaite avec son groupe de transparence.
    #[test]
    fn un_marquage_change_de_couleur_sans_bouger() {
        let d = doc();
        add_annotation(
            &d,
            &page0(&d),
            &markup(
                MarkupKind::Highlight,
                vec![Rect::new(20.0, 100.0, 120.0, 112.0)],
            ),
            None,
        )
        .unwrap();
        let moved = AnnotChanges {
            rect: Some(Rect::new(0.0, 0.0, 10.0, 10.0)),
            ..AnnotChanges::default()
        };
        assert!(set_annotation_properties(&d, &page0(&d), 0, &moved).is_err());
        let green = AnnotChanges {
            color: Some([0.0, 1.0, 0.0]),
            ..AnnotChanges::default()
        };
        set_annotation_properties(&d, &page0(&d), 0, &green).unwrap();
        let d2 = reloaded(&d);
        let a = &list_annotations(&d2, &page0(&d2)).unwrap()[0];
        assert_eq!(a.color, Some([0.0, 1.0, 0.0]));
        assert_eq!(a.quads.len(), 1);
        let (_, dict, _) = normal_stream(&d2, a);
        assert!(dict.contains_key(&Name::new("Group")));
        let at = rendered(&d2);
        let p = at(60.0, 106.0);
        assert!(p[1] > 200 && p[0] < 80 && p[2] < 80, "vert attendu : {p:?}");
    }

    /// L'opacité est écrite dans `/CA` **et** dans l'apparence ; pour un
    /// tampon qu'on ne sait pas redessiner, l'apparence d'origine est
    /// enveloppée, une seule fois même si l'on recommence.
    #[test]
    fn l_opacite_est_dans_l_apparence() {
        let d = doc();
        let i = square(&d);
        let half = AnnotChanges {
            opacity: Some(0.5),
            ..AnnotChanges::default()
        };
        set_annotation_properties(&d, &page0(&d), i, &half).unwrap();
        let a = &list_annotations(&d, &page0(&d)).unwrap()[i];
        assert_eq!(a.opacity, 0.5);
        let (_, _, content) = normal_stream(&d, a);
        assert!(content.starts_with(b"/GS0 gs"), "{content:?}");
        let at = rendered(&d);
        let p = at(40.8, 60.0);
        assert!(
            p[0] > 200 && p[1] > 90 && p[1] < 170,
            "rouge atténué : {p:?}"
        );
        // Un tampon : un carré bleu plein en apparence.
        let form = d.add(appearance_stream(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            "0 0 1 rg 0 0 10 10 re f".into(),
            &[],
        ));
        let mut ap = Dict::new();
        ap.insert(Name::new("N"), Object::Reference(form));
        let stamp = raw_annotation(
            &d,
            &[
                ("Subtype", name_obj("Stamp")),
                ("Rect", reals(&[120.0, 120.0, 160.0, 160.0])),
                ("AP", Object::Dict(ap)),
            ],
        );
        let blue = AnnotChanges {
            color: Some([0.0, 0.0, 1.0]),
            ..AnnotChanges::default()
        };
        assert!(set_annotation_properties(&d, &page0(&d), stamp, &blue).is_err());
        for o in [0.5, 0.25] {
            let fade = AnnotChanges {
                opacity: Some(o),
                ..AnnotChanges::default()
            };
            set_annotation_properties(&d, &page0(&d), stamp, &fade).unwrap();
        }
        let list = list_annotations(&d, &page0(&d)).unwrap();
        let (_, dict, content) = normal_stream(&d, &list[stamp]);
        assert_eq!(content.as_slice(), b"/GS0 gs /AkOriginal Do");
        let res = d.dict_get(&dict, "Resources").unwrap().unwrap();
        let xobj = d
            .dict_get(res.as_dict().unwrap(), "XObject")
            .unwrap()
            .unwrap();
        assert_eq!(
            xobj.as_dict().unwrap().get(&Name::new("AkOriginal")),
            Some(&Object::Reference(form)),
            "l'enveloppe vise l'original, pas l'enveloppe précédente"
        );
        let at = rendered(&d);
        let p = at(140.0, 140.0);
        assert!(p[2] > 200 && p[0] > 150, "bleu au quart : {p:?}");
    }

    /// Un lien, un champ et un élément de « remplir et signer » se gèrent
    /// ailleurs : refusés, le document intact.
    #[test]
    fn liens_champs_et_remplir_et_signer_refuses() {
        let d = doc();
        let link = raw_annotation(
            &d,
            &[
                ("Subtype", name_obj("Link")),
                ("Rect", reals(&[0.0, 0.0, 10.0, 10.0])),
            ],
        );
        let widget = raw_annotation(
            &d,
            &[
                ("Subtype", name_obj("Widget")),
                ("Rect", reals(&[0.0, 0.0, 10.0, 10.0])),
            ],
        );
        let signed = raw_annotation(
            &d,
            &[
                ("Subtype", name_obj("Stamp")),
                ("Rect", reals(&[0.0, 0.0, 10.0, 10.0])),
                (crate::fillsign::TAG, name_obj("drawn")),
            ],
        );
        // Le fichier, sans sa fin (l'identifiant `/ID` change à chaque
        // enregistrement).
        let body = |d: &Document| {
            let bytes = d.save_full().unwrap();
            let end = bytes.windows(7).position(|w| w == b"trailer").unwrap();
            bytes[..end].to_vec()
        };
        let before = body(&d);
        for i in [link, widget, signed] {
            let c = AnnotChanges {
                rect: Some(Rect::new(1.0, 1.0, 11.0, 11.0)),
                ..AnnotChanges::default()
            };
            assert!(set_annotation_properties(&d, &page0(&d), i, &c).is_err());
        }
        for i in [link, widget] {
            assert!(review::add_reply(&d, &page0(&d), i, "x", &AnnotMeta::default()).is_err());
        }
        assert!(set_annotation_properties(&d, &page0(&d), 9, &AnnotChanges::default()).is_err());
        assert_eq!(body(&d), before);
        let list = list_annotations(&d, &page0(&d)).unwrap();
        assert!(list[signed].fill_sign);
    }

    /// Une modification refusée ne laisse rien derrière elle : la fenêtre
    /// contextuelle d'un tampon qu'on voulait déplacer **et** recolorer ne
    /// bouge pas seule. Sinon le document affiché et la copie du fil de
    /// rendu, où la même modification échoue autrement, divergeraient.
    #[test]
    fn une_modification_refusee_ne_touche_a_rien() {
        let d = doc();
        let form = d.add(appearance_stream(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            "0 0 1 rg 0 0 10 10 re f".into(),
            &[],
        ));
        let mut ap = Dict::new();
        ap.insert(Name::new("N"), Object::Reference(form));
        let popup = raw_annotation(
            &d,
            &[
                ("Subtype", name_obj("Popup")),
                ("Rect", reals(&[200.0, 200.0, 300.0, 260.0])),
            ],
        );
        let popup_ref = list_annotations(&d, &page0(&d)).unwrap()[popup]
            .reference
            .unwrap();
        let stamp = raw_annotation(
            &d,
            &[
                ("Subtype", name_obj("Stamp")),
                ("Rect", reals(&[120.0, 120.0, 160.0, 160.0])),
                ("AP", Object::Dict(ap)),
                ("Popup", Object::Reference(popup_ref)),
            ],
        );
        let body = |d: &Document| {
            let bytes = d.save_full().unwrap();
            let end = bytes.windows(7).position(|w| w == b"trailer").unwrap();
            bytes[..end].to_vec()
        };
        let before = body(&d);
        let both = AnnotChanges {
            rect: Some(Rect::new(20.0, 20.0, 60.0, 60.0)),
            color: Some([1.0, 0.0, 0.0]),
            ..AnnotChanges::default()
        };
        assert!(set_annotation_properties(&d, &page0(&d), stamp, &both).is_err());
        assert_eq!(body(&d), before, "rien n'a bougé, pas même la fenêtre");
        // Le déplacement seul passe, et la fenêtre suit.
        let moved = AnnotChanges {
            rect: Some(Rect::new(20.0, 20.0, 60.0, 60.0)),
            ..AnnotChanges::default()
        };
        set_annotation_properties(&d, &page0(&d), stamp, &moved).unwrap();
        let list = list_annotations(&d, &page0(&d)).unwrap();
        assert_eq!(list[popup].rect, Rect::new(100.0, 100.0, 200.0, 160.0));
    }

    fn meta(author: &str, date: &str) -> AnnotMeta {
        AnnotMeta {
            author: Some(author.into()),
            name: Some(format!("{author}-{date}")),
            date: Some(date.into()),
        }
    }

    /// Une note, deux réponses dont une imbriquée, deux statuts (le plus
    /// récent fait foi), une case : un seul fil, bien rangé.
    #[test]
    fn reponses_et_statuts_forment_un_fil() {
        use review::{comment_threads, ReviewState, StateChange};
        let d = doc();
        add_annotation_with(
            &d,
            &page0(&d),
            &NewAnnotation::Note {
                x: 20.0,
                y: 180.0,
                contents: "À relire".into(),
                color: [1.0, 0.8, 0.0],
            },
            &meta("Alice", "D:20240101100000Z"),
        )
        .unwrap();
        let p = page0(&d);
        review::add_reply(&d, &p, 0, "D'accord", &meta("Bruno", "D:20240102100000Z")).unwrap();
        review::add_reply(&d, &p, 1, "Merci", &meta("Alice", "D:20240103100000Z")).unwrap();
        // L'ordre d'écriture ne compte pas : la date fait foi.
        review::set_state(
            &d,
            &p,
            0,
            StateChange::Review(ReviewState::Rejected),
            &meta("Bruno", "D:20240105100000Z"),
        )
        .unwrap();
        review::set_state(
            &d,
            &p,
            0,
            StateChange::Review(ReviewState::Accepted),
            &meta("Alice", "D:20240104100000Z"),
        )
        .unwrap();
        review::set_state(
            &d,
            &p,
            0,
            StateChange::Marked(true),
            &meta("Alice", "D:20240104"),
        )
        .unwrap();
        let d2 = reloaded(&d);
        let pages = collect_pages(&d2).unwrap();
        let threads = comment_threads(&d2, &pages);
        assert_eq!(threads.len(), 1, "{threads:?}");
        let t = &threads[0];
        assert_eq!(t.author.as_deref(), Some("Alice"));
        assert_eq!(t.contents, "À relire");
        let replies: Vec<(&str, usize)> = t
            .replies
            .iter()
            .map(|r| (r.contents.as_str(), r.depth))
            .collect();
        assert_eq!(replies, [("D'accord", 1), ("Merci", 2)]);
        assert_eq!(
            t.review,
            Some((ReviewState::Rejected, Some("Bruno".into())))
        );
        assert!(t.marked);
        let list = list_annotations(&d2, &pages[0]).unwrap();
        assert!(list[1].is_reply() && list[3].is_state() && list[3].hidden());
        assert_eq!(list[3].contents.as_deref(), Some("Rejected set by Bruno"));
        // Les réponses ne se dessinent pas sur la note : une réponse rouge
        // posée au même endroit ne laisse aucun pixel rouge.
        let d3 = doc();
        add_annotation(
            &d3,
            &page0(&d3),
            &NewAnnotation::Note {
                x: 20.0,
                y: 180.0,
                contents: "Mère".into(),
                color: [1.0, 1.0, 0.0],
            },
            None,
        )
        .unwrap();
        review::add_reply(&d3, &page0(&d3), 0, "Fille", &AnnotMeta::default()).unwrap();
        let reply = list_annotations(&d3, &page0(&d3)).unwrap()[1].clone();
        let form = d3.add(appearance_stream(
            Rect::new(20.0, 160.0, 40.0, 180.0),
            "1 0 0 rg 20 160 20 20 re f".into(),
            &[],
        ));
        let mut ap = Dict::new();
        ap.insert(Name::new("N"), Object::Reference(form));
        let mut dict = raw(&d3, &reply);
        dict.insert(Name::new("AP"), Object::Dict(ap));
        d3.set(reply.reference.unwrap(), Object::Dict(dict));
        let at = rendered(&d3);
        assert!(
            !is_red(at(30.0, 170.0)),
            "réponse dessinée : {:?}",
            at(30.0, 170.0)
        );
    }

    /// Les cas limites d'un fil : un barré groupé n'est pas listé, une
    /// réponse orpheline devient un commentaire, un cycle ne boucle pas.
    #[test]
    fn fils_aux_cas_limites() {
        let d = doc();
        add_annotation(
            &d,
            &page0(&d),
            &NewAnnotation::Replace {
                quads: vec![Rect::new(10.0, 150.0, 60.0, 162.0)],
                text: "neuf".into(),
                strike: MarkupKind::StrikeOut.default_color(),
                caret: CARET_COLOR,
            },
            None,
        )
        .unwrap();
        // Une réponse à un objet absent.
        raw_annotation(
            &d,
            &[
                ("Subtype", name_obj("Text")),
                ("Rect", reals(&[0.0, 0.0, 20.0, 20.0])),
                (
                    "IRT",
                    Object::Reference(ObjectRef {
                        number: 999,
                        generation: 0,
                    }),
                ),
                ("Contents", Object::String(b"orpheline".to_vec())),
            ],
        );
        // Deux réponses qui se répondent l'une l'autre.
        let a = d.add(Object::Null);
        let b = d.add(Object::Null);
        for (me, other) in [(a, b), (b, a)] {
            let mut dict = Dict::new();
            dict.insert(Name::new("Subtype"), name_obj("Text"));
            dict.insert(Name::new("Rect"), reals(&[0.0, 0.0, 5.0, 5.0]));
            dict.insert(Name::new("IRT"), Object::Reference(other));
            d.set(me, Object::Dict(dict));
        }
        push_to_annots(&d, &page0(&d), &[a, b]).unwrap();
        let threads = review::comment_threads(&d, &collect_pages(&d).unwrap());
        let summary: Vec<(&str, bool)> = threads
            .iter()
            .map(|t| (t.subtype.as_str(), t.grouped))
            .collect();
        assert_eq!(summary, [("Caret", true), ("Text", false)]);
        assert_eq!(threads[1].contents, "orpheline");
    }

    /// Supprimer un commentaire emporte ses réponses, ses états et sa
    /// fenêtre ; les autres annotations restent, dans leur ordre.
    #[test]
    fn supprimer_emporte_le_fil() {
        let d = doc();
        let first = square(&d);
        let note_ref = list_annotations(&d, &page0(&d)).unwrap()[first]
            .reference
            .unwrap();
        let popup = d.add(Object::Null);
        let mut note = raw(&d, &list_annotations(&d, &page0(&d)).unwrap()[first]);
        note.insert(Name::new("Popup"), Object::Reference(popup));
        d.set(note_ref, Object::Dict(note));
        let mut pd = Dict::new();
        pd.insert(Name::new("Subtype"), name_obj("Popup"));
        pd.insert(Name::new("Parent"), Object::Reference(note_ref));
        pd.insert(Name::new("Rect"), reals(&[90.0, 90.0, 180.0, 150.0]));
        d.set(popup, Object::Dict(pd));
        push_to_annots(&d, &page0(&d), &[popup]).unwrap();
        let other = square(&d);
        assert_eq!(other, 2);
        let p = page0(&d);
        review::add_reply(&d, &p, first, "réponse", &AnnotMeta::default()).unwrap();
        review::add_reply(&d, &p, 3, "sous-réponse", &AnnotMeta::default()).unwrap();
        review::set_state(
            &d,
            &p,
            first,
            review::StateChange::Marked(true),
            &AnnotMeta::default(),
        )
        .unwrap();
        assert_eq!(list_annotations(&d, &page0(&d)).unwrap().len(), 6);
        remove_annotation(&d, &page0(&d), first).unwrap();
        let left = list_annotations(&d, &page0(&d)).unwrap();
        assert_eq!(left.len(), 1, "{left:?}");
        assert_eq!(left[0].subtype, "Square");
        assert_eq!(left[0].index, 0);
    }

    #[test]
    fn dates_lisibles() {
        assert_eq!(readable_date("D:20240312103015+01'00'"), "2024-03-12 10:30");
        assert_eq!(readable_date("D:20240312"), "2024-03-12");
        assert_eq!(readable_date("hier soir"), "hier soir");
        assert_eq!(readable_date("D:2024"), "D:2024");
    }

    /// Cacher une annotation qui n'est pas de « remplir et signer » : le
    /// bit 2 de `/F` se pose et se retire, les autres drapeaux restent.
    #[test]
    fn cacher_une_annotation() {
        let d = doc();
        let i = square(&d);
        set_hidden(&d, &page0(&d), i, true).unwrap();
        let a = &list_annotations(&d, &page0(&d)).unwrap()[i];
        assert!(a.hidden());
        assert_eq!(a.flags & 4, 4);
        assert!(is_white(rendered(&d)(40.8, 60.0)));
        set_hidden(&d, &page0(&d), i, false).unwrap();
        assert!(!list_annotations(&d, &page0(&d)).unwrap()[i].hidden());
    }

    /// Une zone de texte change de couleur de texte et de texte : sa police
    /// et son cadre restent, son apparence dit le nouveau texte.
    #[test]
    fn une_zone_de_texte_se_modifie() {
        let d = doc();
        add_annotation(
            &d,
            &page0(&d),
            &NewAnnotation::FreeText {
                rect: Rect::new(20.0, 100.0, 180.0, 140.0),
                text: "Bonjour".into(),
                font: StandardFont::Courier,
                size: 14.0,
                color: [0.0, 0.0, 0.0],
                border: Some(([1.0, 0.0, 0.0], 1.0)),
                fill: None,
                align: TextAlign::Left,
                callout: None,
                rotation: 0,
            },
            None,
        )
        .unwrap();
        let changes = AnnotChanges {
            color: Some([0.0, 0.0, 1.0]),
            contents: Some("Au revoir".into()),
            ..AnnotChanges::default()
        };
        set_annotation_properties(&d, &page0(&d), 0, &changes).unwrap();
        let d2 = reloaded(&d);
        let a = &list_annotations(&d2, &page0(&d2)).unwrap()[0];
        assert_eq!(a.color, Some([0.0, 0.0, 1.0]));
        assert_eq!(a.contents.as_deref(), Some("Au revoir"));
        let da = string(&d2, &raw(&d2, a), "DA").unwrap();
        let parsed = appearance::parse_da(&da);
        assert_eq!(parsed.font.as_deref(), Some("Cour"));
        assert_eq!(parsed.stroke, Some([1.0, 0.0, 0.0]));
        let (_, _, content) = normal_stream(&d2, a);
        assert!(
            String::from_utf8_lossy(&content).contains("Au revoir"),
            "le nouveau texte est dans l'apparence"
        );
    }

    /// Une annotation rendue seule : de la taille de sa zone d'arrivée,
    /// transparente hors du trait.
    #[test]
    fn une_annotation_se_rend_seule() {
        let d = doc();
        let i = square(&d);
        let img = acrux_render::render_annotation(
            &d,
            &page0(&d),
            i,
            2.0,
            0,
            Rect::new(100.0, 100.0, 180.0, 140.0),
        )
        .unwrap();
        assert_eq!((img.width(), img.height()), (160, 80));
        let edge = img.pixel(1, 40).unwrap();
        assert!(edge[3] > 200 && edge[0] > 200, "trait : {edge:?}");
        assert_eq!(img.pixel(80, 40).unwrap()[3], 0, "intérieur transparent");
        assert!(
            acrux_render::render_annotation(&d, &page0(&d), 9, 1.0, 0, Rect::default()).is_none()
        );
    }
}
