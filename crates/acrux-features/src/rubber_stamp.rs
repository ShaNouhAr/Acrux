//! Tampons à la manière d'Acrobat (inventaire Acrobat §8, « Tampons ») :
//! « Approuvé », « Confidentiel », « Brouillon »… posés comme annotations
//! `/Stamp` (ISO 32000-2 §12.5.6.12), avec leur apparence vectorielle.
//!
//! # Trois sortes de tampons
//!
//! - les **douze tampons standard** d'Acrobat ([`StandardStamp`]), chacun
//!   avec son nom PDF (`/Approved`, `/SBRejected`…), sa couleur et son
//!   libellé en français ou en anglais ;
//! - les tampons **dynamiques** : les mêmes, avec une seconde ligne qui dit
//!   qui a tamponné et quand (« par Nina, le 23/09/2026 à 14:05 ») ;
//! - les tampons **image** : une signature scannée, un logo, un cachet.
//!
//! Un texte libre dans une couleur choisie ([`Source::Custom`]) complète la
//! liste, pour la ligne de commande.
//!
//! # L'apparence
//!
//! Le dessin est un XObject de formulaire (§8.10) posé en `/AP /N` :
//! fond teinté à 10 % — le texte dessous reste lisible —, double cadre
//! arrondi (trait de 2,4 pt, filet de 0,8 pt à l'intérieur, l'allure des
//! tampons « Standard Business » d'Acrobat), libellé en capitales grasses
//! centré. Tout est vectoriel : le tampon reste net à tous les zooms et à
//! toutes les tailles qu'on lui donne ensuite.
//!
//! Sur une page tournée (`/Rotate`), le formulaire reçoit la `/Matrix` qui
//! le redresse : l'algorithme de §12.5.5 cale la boîte transformée sur
//! `/Rect`, et le tampon s'affiche droit, comme dans Acrobat.
//!
//! # Rejouer à l'identique
//!
//! Une modification de l'application est appliquée deux fois (au document
//! affiché, à la copie du fil de rendu), puis rejouée à chaque annulation.
//! Tout ce qui dépend du moment est donc tiré **avant**, par celui qui
//! décide de poser le tampon, et voyage dans [`Options`] : la ligne
//! dynamique déjà écrite, l'identité de l'annotation ([`AnnotMeta`]), l'image
//! déjà décodée ([`PreparedImage`]).
//!
//! # Reconnaître nos tampons
//!
//! « Remplir et signer » pose lui aussi des `/Stamp` (clé `/AKFillSign`).
//! Les nôtres portent la clé privée `/AKRubber`, qui dit leur sorte :
//! `/Standard`, `/Dynamic`, `/Custom` ou `/Image`.

use std::fmt::Write as _;
use std::sync::Arc;

use acrux_core::{Error, Point, Rect, Result};
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef};

use crate::annotations::{encode_text, fmt_number as num, AnnotMeta, Rgb};
use crate::stamp::{PreparedImage, StandardFont, TextFont};

/// Clé privée qui marque nos tampons, et dit leur sorte.
pub const TAG: &str = "AKRubber";

/// Hauteur d'un tampon d'une ligne, en points.
const HEIGHT: f64 = 34.0;
/// Hauteur d'un tampon dynamique (deux lignes).
const HEIGHT_DYNAMIC: f64 = 46.0;
/// Corps du libellé.
const LABEL_SIZE: f64 = 18.0;
/// Corps de la ligne dynamique.
const DYNAMIC_SIZE: f64 = 8.0;
/// Espace entre le libellé et la ligne dynamique.
const LINE_GAP: f64 = 5.0;
/// Marge horizontale de part et d'autre du texte.
const PAD: f64 = 14.0;
/// Largeur minimale d'un tampon.
const MIN_WIDTH: f64 = 120.0;
/// Côté maximal d'un tampon image posé à sa taille naturelle.
const MAX_IMAGE_SIDE: f64 = 200.0;
/// Épaisseur du cadre extérieur.
const OUTER: f64 = 2.4;
/// Épaisseur du filet intérieur.
const INNER: f64 = 0.8;
/// Écart entre le cadre et le filet.
const INNER_GAP: f64 = 3.2;
/// Opacité du fond teinté.
const TINT: f64 = 0.10;

/// Langue du libellé d'un tampon.
///
/// C'est du **contenu du document**, pas de l'interface : un tampon posé en
/// français reste « Approuvé » quand on relit le fichier en anglais. La ligne
/// de commande en a besoin autant que l'application, d'où sa place ici.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    /// Français.
    #[default]
    Fr,
    /// Anglais.
    En,
}

impl Language {
    /// Langue d'un code (`fr`, `en`).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "fr" | "francais" | "français" | "french" => Some(Language::Fr),
            "en" | "anglais" | "english" => Some(Language::En),
            _ => None,
        }
    }
}

/// Les douze tampons standard d'Acrobat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StandardStamp {
    /// Approuvé.
    Approved,
    /// Refusé.
    Rejected,
    /// Brouillon.
    Draft,
    /// Confidentiel.
    Confidential,
    /// Final.
    Final,
    /// Pour commentaire.
    ForComment,
    /// Pour information.
    InformationOnly,
    /// Payé.
    Paid,
    /// Reçu.
    Received,
    /// Terminé.
    Completed,
    /// Non approuvé.
    NotApproved,
    /// Nul.
    Void,
}

/// Vert des tampons favorables.
const GREEN: Rgb = [0.16, 0.47, 0.20];
/// Rouge des refus et des mises en garde.
const RED: Rgb = [0.72, 0.11, 0.11];
/// Bleu des étapes de travail.
const BLUE: Rgb = [0.12, 0.29, 0.60];

impl StandardStamp {
    /// Les douze, dans l'ordre du sélecteur d'Acrobat.
    pub const ALL: [StandardStamp; 12] = [
        StandardStamp::Approved,
        StandardStamp::Rejected,
        StandardStamp::Draft,
        StandardStamp::Confidential,
        StandardStamp::Final,
        StandardStamp::Completed,
        StandardStamp::ForComment,
        StandardStamp::InformationOnly,
        StandardStamp::Received,
        StandardStamp::Paid,
        StandardStamp::NotApproved,
        StandardStamp::Void,
    ];

    /// Clé stable, en français sans accents : celle de la ligne de commande
    /// et des préférences.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            StandardStamp::Approved => "approuve",
            StandardStamp::Rejected => "refuse",
            StandardStamp::Draft => "brouillon",
            StandardStamp::Confidential => "confidentiel",
            StandardStamp::Final => "final",
            StandardStamp::ForComment => "commentaire",
            StandardStamp::InformationOnly => "information",
            StandardStamp::Paid => "paye",
            StandardStamp::Received => "recu",
            StandardStamp::Completed => "termine",
            StandardStamp::NotApproved => "non-approuve",
            StandardStamp::Void => "nul",
        }
    }

    /// Nom anglais, sans espaces.
    fn english_key(self) -> &'static str {
        match self {
            StandardStamp::Approved => "approved",
            StandardStamp::Rejected => "rejected",
            StandardStamp::Draft => "draft",
            StandardStamp::Confidential => "confidential",
            StandardStamp::Final => "final",
            StandardStamp::ForComment => "for-comment",
            StandardStamp::InformationOnly => "information-only",
            StandardStamp::Paid => "paid",
            StandardStamp::Received => "received",
            StandardStamp::Completed => "completed",
            StandardStamp::NotApproved => "not-approved",
            StandardStamp::Void => "void",
        }
    }

    /// Tampon d'un nom : clé française (`approuve`, `non-approuve`), nom
    /// anglais (`approved`, `for-comment`) ou nom PDF (`SBRejected`), sans
    /// souci de casse, d'accents ni de séparateurs.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let wanted = squash(name);
        if wanted.is_empty() {
            return None;
        }
        StandardStamp::ALL.into_iter().find(|s| {
            [s.key(), s.english_key(), s.pdf_name()]
                .iter()
                .any(|n| squash(n) == wanted)
        })
    }

    /// Nom PDF (`/Name`) : ceux de §12.5.6.12 quand la norme en donne un,
    /// sinon ceux des tampons « Standard Business » d'Acrobat.
    #[must_use]
    pub fn pdf_name(self) -> &'static str {
        match self {
            StandardStamp::Approved => "Approved",
            StandardStamp::Rejected => "SBRejected",
            StandardStamp::Draft => "Draft",
            StandardStamp::Confidential => "Confidential",
            StandardStamp::Final => "Final",
            StandardStamp::ForComment => "ForComment",
            StandardStamp::InformationOnly => "SBInformationOnly",
            StandardStamp::Paid => "Paid",
            StandardStamp::Received => "Received",
            StandardStamp::Completed => "SBCompleted",
            StandardStamp::NotApproved => "NotApproved",
            StandardStamp::Void => "SBVoid",
        }
    }

    /// Libellé dans la langue demandée ; le tampon l'écrit en capitales.
    #[must_use]
    pub fn label(self, lang: Language) -> &'static str {
        match (self, lang) {
            (StandardStamp::Approved, Language::Fr) => "Approuvé",
            (StandardStamp::Rejected, Language::Fr) => "Refusé",
            (StandardStamp::Draft, Language::Fr) => "Brouillon",
            (StandardStamp::Confidential, Language::Fr) => "Confidentiel",
            (StandardStamp::Final, _) => "Final",
            (StandardStamp::ForComment, Language::Fr) => "Pour commentaire",
            (StandardStamp::InformationOnly, Language::Fr) => "Pour information",
            (StandardStamp::Paid, Language::Fr) => "Payé",
            (StandardStamp::Received, Language::Fr) => "Reçu",
            (StandardStamp::Completed, Language::Fr) => "Terminé",
            (StandardStamp::NotApproved, Language::Fr) => "Non approuvé",
            (StandardStamp::Void, Language::Fr) => "Nul",
            (StandardStamp::Approved, Language::En) => "Approved",
            (StandardStamp::Rejected, Language::En) => "Rejected",
            (StandardStamp::Draft, Language::En) => "Draft",
            (StandardStamp::Confidential, Language::En) => "Confidential",
            (StandardStamp::ForComment, Language::En) => "For Comment",
            (StandardStamp::InformationOnly, Language::En) => "Information Only",
            (StandardStamp::Paid, Language::En) => "Paid",
            (StandardStamp::Received, Language::En) => "Received",
            (StandardStamp::Completed, Language::En) => "Completed",
            (StandardStamp::NotApproved, Language::En) => "Not Approved",
            (StandardStamp::Void, Language::En) => "Void",
        }
    }

    /// Couleur : vert pour ce qui est favorable ou réglé, rouge pour les
    /// refus et les mises en garde, bleu pour les étapes de travail.
    #[must_use]
    pub fn color(self) -> Rgb {
        match self {
            StandardStamp::Approved
            | StandardStamp::Final
            | StandardStamp::Paid
            | StandardStamp::Completed => GREEN,
            StandardStamp::Rejected
            | StandardStamp::NotApproved
            | StandardStamp::Void
            | StandardStamp::Confidential => RED,
            StandardStamp::Draft
            | StandardStamp::ForComment
            | StandardStamp::InformationOnly
            | StandardStamp::Received => BLUE,
        }
    }
}

/// Un nom réduit à ses lettres et chiffres ASCII, en minuscules, accents
/// retirés : « Non approuvé », « non_approuve » et « NotApproved » se
/// comparent ainsi sans détour.
fn squash(name: &str) -> String {
    name.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            other => other,
        })
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

/// Ce que montre le tampon.
#[derive(Debug, Clone)]
pub enum Source {
    /// Un des douze tampons standard.
    Standard(StandardStamp),
    /// Un texte libre, dans une couleur choisie, avec le même cadre.
    Custom {
        /// Texte, écrit tel quel.
        text: String,
        /// Couleur du cadre et du texte.
        color: Rgb,
    },
    /// Une image : signature scannée, logo, cachet. Décodée une fois
    /// (voir [`PreparedImage`]), partagée entre les copies de l'opération.
    Image(Arc<PreparedImage>),
}

impl Source {
    /// Sorte inscrite sous `/AKRubber`.
    fn kind(&self, dynamic: bool) -> &'static str {
        match self {
            Source::Standard(_) if dynamic => "Dynamic",
            Source::Standard(_) => "Standard",
            Source::Custom { .. } => "Custom",
            Source::Image(_) => "Image",
        }
    }
}

/// Un tampon à poser.
#[derive(Debug, Clone)]
pub struct Options {
    /// Ce que montre le tampon.
    pub source: Source,
    /// Page (à partir de 0).
    pub page: usize,
    /// Centre du tampon, en coordonnées de page.
    pub center: Point,
    /// Largeur et hauteur **telles qu'on les voit** (rotation de la page
    /// comprise) ; `None` : sa taille naturelle.
    pub size: Option<(f64, f64)>,
    /// Langue du libellé d'un tampon standard.
    pub label_lang: Language,
    /// Seconde ligne d'un tampon dynamique, **déjà écrite** (voir
    /// [`dynamic_line`]) : tirée au moment de la décision, elle ne change
    /// pas quand l'opération est rejouée.
    pub dynamic: Option<String>,
    /// Auteur, identifiant et date de l'annotation, tirés une fois.
    pub meta: AnnotMeta,
    /// Opacité, de 0 à 1.
    pub opacity: f64,
}

impl Options {
    /// Tampon standard, à sa taille naturelle, opaque, en français.
    #[must_use]
    pub fn new(source: Source, page: usize, center: Point) -> Self {
        Options {
            source,
            page,
            center,
            size: None,
            label_lang: Language::Fr,
            dynamic: None,
            meta: AnnotMeta::default(),
            opacity: 1.0,
        }
    }

    /// Libellé du tampon, avant mise en capitales ; vide pour une image.
    fn label(&self) -> String {
        match &self.source {
            Source::Standard(s) => s.label(self.label_lang).to_string(),
            Source::Custom { text, .. } => text.clone(),
            Source::Image(_) => String::new(),
        }
    }

    /// Couleur du cadre et du texte.
    fn color(&self) -> Rgb {
        match &self.source {
            Source::Standard(s) => s.color(),
            Source::Custom { color, .. } => *color,
            Source::Image(_) => [0.0, 0.0, 0.0],
        }
    }
}

/// Un tampon posé par ce module, tel que l'inventaire le montre.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    /// Page (à partir de 0).
    pub page: usize,
    /// Rang dans `/Annots`.
    pub index: usize,
    /// Sorte : `Standard`, `Dynamic`, `Custom` ou `Image`.
    pub kind: String,
    /// `/Name`.
    pub name: Option<String>,
    /// `/Rect`.
    pub rect: Rect,
    /// `/Contents` : le libellé, et la ligne dynamique.
    pub contents: Option<String>,
}

// --- Ligne dynamique ----------------------------------------------------------

/// Seconde ligne d'un tampon dynamique, pour l'heure présente décalée de
/// `offset_minutes` par rapport à UTC (le fuseau de la personne).
#[must_use]
pub fn dynamic_line(author: &str, lang: Language, offset_minutes: i32) -> String {
    dynamic_line_at(
        author,
        lang,
        &crate::annotations::pdf_date_at(offset_minutes),
    )
}

/// Même chose pour une date PDF donnée (`D:AAAAMMJJHHmm…`) : « par Nina, le
/// 23/09/2026 à 14:05 », ou « By Nina at 2:05 PM, Sep 23, 2026 ».
#[must_use]
pub fn dynamic_line_at(author: &str, lang: Language, pdf_date: &str) -> String {
    let digits: Vec<u32> = pdf_date
        .trim_start_matches("D:")
        .chars()
        .take(12)
        .filter_map(|c| c.to_digit(10))
        .collect();
    let field = |from: usize, len: usize| -> u32 {
        digits
            .get(from..from + len)
            .map_or(0, |d| d.iter().fold(0, |acc, v| acc * 10 + v))
    };
    let (year, month, day) = (field(0, 4), field(4, 2), field(6, 2));
    let (hour, minute) = (field(8, 2), field(10, 2));
    let author = author.trim();
    match lang {
        Language::Fr => {
            let when = format!("le {day:02}/{month:02}/{year:04} à {hour:02}:{minute:02}");
            if author.is_empty() {
                when
            } else {
                format!("par {author}, {when}")
            }
        }
        Language::En => {
            const MONTHS: [&str; 12] = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            let month_name = MONTHS
                .get(usize::try_from(month.max(1) - 1).unwrap_or(0))
                .copied()
                .unwrap_or("Jan");
            let (h12, half) = match hour {
                0 => (12, "AM"),
                1..=11 => (hour, "AM"),
                12 => (12, "PM"),
                _ => (hour - 12, "PM"),
            };
            let when = format!("at {h12}:{minute:02} {half}, {month_name} {day}, {year:04}");
            if author.is_empty() {
                when
            } else {
                format!("By {author} {when}")
            }
        }
    }
}

// --- Taille ---------------------------------------------------------------------

/// Taille naturelle d'un tampon, en points : un libellé en capitales avec
/// sa marge (120 pt au moins), 34 pt de haut, 46 avec la ligne dynamique ;
/// une image à 96 ppp, bornée à 200 pt de côté.
///
/// Les largeurs sont celles des polices standard : un auteur en cyrillique,
/// écrit avec une police système incorporée, peut donner un tampon un peu
/// différent — [`place`] mesure alors avec la vraie police.
#[must_use]
pub fn natural_size(source: &Source, lang: Language, dynamic: Option<&str>) -> (f64, f64) {
    let label = match source {
        Source::Standard(s) => s.label(lang).to_string(),
        Source::Custom { text, .. } => text.clone(),
        Source::Image(image) => return image.natural_size(Some(MAX_IMAGE_SIDE)),
    };
    let label_w = StandardFont::HelveticaBold.text_width(&label.to_uppercase(), LABEL_SIZE);
    let dynamic_w = dynamic.map_or(0.0, |d| StandardFont::Helvetica.text_width(d, DYNAMIC_SIZE));
    frame_size(label_w, dynamic_w, dynamic.is_some())
}

/// Taille d'un tampon de texte d'après la largeur de ses lignes.
fn frame_size(label_w: f64, dynamic_w: f64, two_lines: bool) -> (f64, f64) {
    let width = (label_w.max(dynamic_w) + 2.0 * PAD).max(MIN_WIDTH);
    let height = if two_lines { HEIGHT_DYNAMIC } else { HEIGHT };
    (width, height)
}

/// Rectangle d'un tampon centré sur `center`, de taille `size` telle qu'on
/// la voit, sur une page de boîte `crop` tournée de `rotate` degrés : les
/// côtés s'échangent à 90° et 270°, et le tampon est ramené dans la page
/// quand on l'a posé trop près du bord.
#[must_use]
pub fn stamp_rect(center: Point, size: (f64, f64), crop: Rect, rotate: i32) -> Rect {
    let (w, h) = if matches!(rotate.rem_euclid(360), 90 | 270) {
        (size.1, size.0)
    } else {
        size
    };
    let along = |c: f64, len: f64, lo: f64, hi: f64| -> f64 {
        let start = c - len / 2.0;
        if len >= hi - lo {
            // Plus grand que la page : centré sur elle.
            return f64::midpoint(lo, hi) - len / 2.0;
        }
        start.clamp(lo, hi - len)
    };
    let x0 = along(center.x, w, crop.x0.min(crop.x1), crop.x0.max(crop.x1));
    let y0 = along(center.y, h, crop.y0.min(crop.y1), crop.y0.max(crop.y1));
    Rect::new(x0, y0, x0 + w, y0 + h)
}

/// Centre d'un tampon de taille `size` (telle qu'on la voit) posé dans le
/// coin supérieur droit **de la page affichée**, à `margin` points des
/// bords : la place d'un tampon qu'on ne pose pas à la souris.
#[must_use]
pub fn top_right(crop: Rect, rotate: i32, size: (f64, f64), margin: f64) -> Point {
    let (w, h) = (crop.width().abs(), crop.height().abs());
    let (x0, y0) = (crop.x0.min(crop.x1), crop.y0.min(crop.y1));
    let (x1, y1) = (x0 + w, y0 + h);
    // Espace de la page affichée vers espace de la page (voir
    // `stamp::display_space`) : l'origine est le coin inférieur gauche de
    // ce qu'on voit.
    let (shown_w, shown_h, m) = match rotate.rem_euclid(360) {
        90 => (h, w, acrux_core::Matrix::new(0.0, 1.0, -1.0, 0.0, x1, y0)),
        180 => (w, h, acrux_core::Matrix::new(-1.0, 0.0, 0.0, -1.0, x1, y1)),
        270 => (h, w, acrux_core::Matrix::new(0.0, -1.0, 1.0, 0.0, x0, y1)),
        _ => (w, h, acrux_core::Matrix::new(1.0, 0.0, 0.0, 1.0, x0, y0)),
    };
    m.apply(Point::new(
        shown_w - margin - size.0 / 2.0,
        shown_h - margin - size.1 / 2.0,
    ))
}

// --- Pose -------------------------------------------------------------------------

/// Pose un tampon et rend la référence de l'annotation, ajoutée **à la fin**
/// de `/Annots` (son rang est donc le dernier).
///
/// Mêmes options, même document : même résultat — c'est ce qui permet de
/// rejouer la pose à chaque annulation.
///
/// # Errors
/// Page absente ou non indirecte, `/Annots` illisible, taille nulle.
pub fn place(doc: &Document, options: &Options) -> Result<ObjectRef> {
    let pages = collect_pages(doc)?;
    let page = pages
        .get(options.page)
        .ok_or_else(|| Error::Corrupt(format!("page {} inexistante", options.page + 1)))?;
    let page_ref = page
        .reference
        .ok_or_else(|| Error::Corrupt("la page doit être un objet indirect".into()))?;
    let rotate = page.rotate(doc);
    let mut warnings = Vec::new();
    let form = build_form(doc, options, rotate, &mut warnings);
    let size = options.size.unwrap_or((form.width, form.height));
    if !(size.0.is_finite() && size.1.is_finite() && size.0 > 0.0 && size.1 > 0.0) {
        return Err(Error::Unsupported("tampon de taille nulle".into()));
    }
    let rect = stamp_rect(options.center, size, page.crop_box(doc), rotate);

    let mut d = Dict::new();
    d.insert(Name::new("Type"), Object::Name(Name::new("Annot")));
    d.insert(Name::new("Subtype"), Object::Name(Name::new("Stamp")));
    d.insert(Name::new("P"), Object::Reference(page_ref));
    d.insert(Name::new("F"), Object::Integer(4)); // Imprimer
    d.insert(
        Name::new("Rect"),
        Object::Array(
            [rect.x0, rect.y0, rect.x1, rect.y1]
                .into_iter()
                .map(Object::Real)
                .collect(),
        ),
    );
    let name = match &options.source {
        Source::Standard(s) => s.pdf_name(),
        // Le nom par défaut de §12.5.6.12 pour ce qui n'est pas standard.
        Source::Custom { .. } | Source::Image(_) => "Draft",
    };
    d.insert(Name::new("Name"), Object::Name(Name::new(name)));
    if !matches!(options.source, Source::Image(_)) {
        d.insert(
            Name::new("C"),
            Object::Array(options.color().into_iter().map(Object::Real).collect()),
        );
    }
    let mut contents = options.label();
    if let Some(line) = options.dynamic.as_deref().filter(|l| !l.is_empty()) {
        if !contents.is_empty() {
            contents.push('\n');
        }
        contents.push_str(line);
    }
    if !contents.is_empty() {
        d.insert(
            Name::new("Contents"),
            Object::String(encode_text(&contents)),
        );
    }
    let opacity = opacity_of(options);
    if opacity < 1.0 {
        // Écrite aussi dans l'apparence, comme le fait Acrobat.
        d.insert(Name::new("CA"), Object::Real(opacity));
    }
    crate::annotations::identity(&mut d, &options.meta);
    d.insert(
        Name::new(TAG),
        Object::Name(Name::new(options.source.kind(options.dynamic.is_some()))),
    );
    let mut ap = Dict::new();
    ap.insert(Name::new("N"), Object::Reference(form.reference));
    d.insert(Name::new("AP"), Object::Dict(ap));
    let reference = doc.add(Object::Dict(d));
    crate::annotations::push_to_annots(doc, page, &[reference])?;
    Ok(reference)
}

/// Opacité demandée, bornée ; une valeur illisible vaut « opaque ».
fn opacity_of(options: &Options) -> f64 {
    if options.opacity.is_finite() {
        options.opacity.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

/// Inventaire des tampons posés par ce module (ceux qui portent `/AKRubber`).
///
/// # Errors
/// Document illisible.
pub fn list(doc: &Document) -> Result<Vec<Placed>> {
    let mut out = Vec::new();
    for (page_index, page) in collect_pages(doc)?.iter().enumerate() {
        for info in crate::annotations::list_annotations(doc, page)? {
            let Some(r) = info.reference else { continue };
            let Some(dict) = doc.get(r).ok().and_then(|o| o.as_dict().cloned()) else {
                continue;
            };
            let Some(kind) = dict.get(&Name::new(TAG)).and_then(Object::as_name) else {
                continue;
            };
            out.push(Placed {
                page: page_index,
                index: info.index,
                kind: kind.as_str(),
                name: dict
                    .get(&Name::new("Name"))
                    .and_then(Object::as_name)
                    .map(Name::as_str),
                rect: info.rect,
                contents: info.contents.clone(),
            });
        }
    }
    Ok(out)
}

// --- Apparence ---------------------------------------------------------------------

/// Formulaire d'apparence prêt à être posé.
struct Form {
    reference: ObjectRef,
    width: f64,
    height: f64,
}

/// Rectangle aux coins arrondis, en quatre segments et quatre quarts de
/// cercle approchés par des Bézier (k = 0,5523) ; le rayon est ramené à la
/// moitié du plus petit côté.
#[must_use]
pub fn rounded_rect(left: f64, bottom: f64, width: f64, height: f64, radius: f64) -> String {
    let radius = radius.clamp(0.0, width.min(height) / 2.0);
    let kappa = radius * 0.552_284_75;
    let (right, top) = (left + width, bottom + height);
    // Chaque côté, puis le quart de cercle qui mène au suivant, dans le sens
    // direct depuis le bas à gauche.
    let points: [(f64, f64); 17] = [
        (left + radius, bottom),
        (right - radius, bottom),
        (right - radius + kappa, bottom),
        (right, bottom + radius - kappa),
        (right, bottom + radius),
        (right, top - radius),
        (right, top - radius + kappa),
        (right - radius + kappa, top),
        (right - radius, top),
        (left + radius, top),
        (left + radius - kappa, top),
        (left, top - radius + kappa),
        (left, top - radius),
        (left, bottom + radius),
        (left, bottom + radius - kappa),
        (left + radius - kappa, bottom),
        (left + radius, bottom),
    ];
    let mut out = String::new();
    let pt = |out: &mut String, (x, y): (f64, f64)| {
        let _ = write!(out, "{} {} ", num(x), num(y));
    };
    pt(&mut out, points[0]);
    out.push_str("m ");
    for side in 0..4 {
        let at = 1 + side * 4;
        pt(&mut out, points[at]);
        out.push_str("l ");
        for p in &points[at + 1..at + 4] {
            pt(&mut out, *p);
        }
        out.push_str("c ");
    }
    out.push_str("h ");
    out
}

/// Opérateur de couleur RVB, `rg` ou `RG`.
fn rgb(c: Rgb, op: &str) -> String {
    format!("{} {} {} {op}", num(c[0]), num(c[1]), num(c[2]))
}

/// État graphique étendu d'une opacité de remplissage et de trait.
fn opacity_state(opacity: f64) -> Object {
    let mut gs = Dict::new();
    gs.insert(Name::new("Type"), Object::Name(Name::new("ExtGState")));
    gs.insert(Name::new("ca"), Object::Real(opacity));
    gs.insert(Name::new("CA"), Object::Real(opacity));
    Object::Dict(gs)
}

/// Construit le formulaire d'apparence, dans le sens de lecture (la
/// rotation de la page passe par sa `/Matrix`).
fn build_form(doc: &Document, options: &Options, rotate: i32, warnings: &mut Vec<String>) -> Form {
    let mut parts = Parts::default();
    let opacity = opacity_of(options);
    if opacity < 1.0 {
        parts
            .states
            .insert(Name::new("GS0"), opacity_state(opacity));
        parts.content.push_str("/GS0 gs\n");
    }
    let (width, height) = match &options.source {
        Source::Image(image) => {
            let (w, h) = image.natural_size(Some(MAX_IMAGE_SIDE));
            let reference = image.write(doc);
            let mut xobjects = Dict::new();
            xobjects.insert(Name::new("Im0"), Object::Reference(reference));
            parts
                .resources
                .insert(Name::new("XObject"), Object::Dict(xobjects));
            let _ = writeln!(
                parts.content,
                "q {} 0 0 {} 0 0 cm /Im0 Do Q",
                num(w),
                num(h)
            );
            (w, h)
        }
        Source::Standard(_) | Source::Custom { .. } => {
            text_stamp(doc, options, &mut parts, warnings)
        }
    };
    let Parts {
        mut resources,
        states,
        content,
    } = parts;
    if !states.is_empty() {
        resources.insert(Name::new("ExtGState"), Object::Dict(states));
    }
    let mut dict = Dict::new();
    dict.insert(Name::new("Type"), Object::Name(Name::new("XObject")));
    dict.insert(Name::new("Subtype"), Object::Name(Name::new("Form")));
    dict.insert(
        Name::new("BBox"),
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Real(width),
            Object::Real(height),
        ]),
    );
    // Redressé sur une page tournée : l'algorithme de §12.5.5 cale la boîte
    // transformée sur /Rect, dont les côtés sont échangés d'autant.
    let m = crate::edit_objects::upright(rotate);
    if m != acrux_core::Matrix::IDENTITY {
        dict.insert(
            Name::new("Matrix"),
            Object::Array(
                [m.a, m.b, m.c, m.d, m.e, m.f]
                    .into_iter()
                    .map(Object::Real)
                    .collect(),
            ),
        );
    }
    dict.insert(Name::new("Resources"), Object::Dict(resources));
    // Groupe de transparence : l'opacité s'applique au tampon entier, et non
    // à chaque trait qui se recouvre.
    let mut group = Dict::new();
    group.insert(Name::new("S"), Object::Name(Name::new("Transparency")));
    group.insert(Name::new("CS"), Object::Name(Name::new("DeviceRGB")));
    dict.insert(Name::new("Group"), Object::Dict(group));
    let raw = content.into_bytes();
    dict.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
    );
    let reference = doc.add(Object::Stream { dict, raw });
    Form {
        reference,
        width,
        height,
    }
}

/// Ce que le formulaire accumule : ressources, états graphiques, contenu.
#[derive(Default)]
struct Parts {
    resources: Dict,
    states: Dict,
    content: String,
}

/// Dessin d'un tampon de texte — fond teinté, double cadre, libellé et
/// ligne dynamique — ajouté à `parts` ; rend sa largeur et sa hauteur.
fn text_stamp(
    doc: &Document,
    options: &Options,
    parts: &mut Parts,
    warnings: &mut Vec<String>,
) -> (f64, f64) {
    let label = options.label().to_uppercase();
    let (label_font, label_ref) =
        TextFont::resolve(doc, &label, StandardFont::HelveticaBold, warnings);
    let mut fonts = Dict::new();
    fonts.insert(Name::new("F0"), Object::Reference(label_ref));
    let second = options
        .dynamic
        .as_deref()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let (font, reference) = TextFont::resolve(doc, line, StandardFont::Helvetica, warnings);
            fonts.insert(Name::new("F1"), Object::Reference(reference));
            (line, font)
        });
    parts
        .resources
        .insert(Name::new("Font"), Object::Dict(fonts));
    let label_w = label_font.width(&label, LABEL_SIZE);
    let dynamic_w = second
        .as_ref()
        .map_or(0.0, |(line, font)| font.width(line, DYNAMIC_SIZE));
    let (w, h) = frame_size(label_w, dynamic_w, second.is_some());
    let mut tint = Dict::new();
    tint.insert(Name::new("Type"), Object::Name(Name::new("ExtGState")));
    tint.insert(Name::new("ca"), Object::Real(TINT));
    parts.states.insert(Name::new("GS1"), Object::Dict(tint));
    let color = options.color();
    let content = &mut parts.content;
    // Fond teinté, transparent : le texte de la page reste lisible sous le
    // tampon.
    let half = OUTER / 2.0;
    let radius = 0.2 * h;
    let outline = rounded_rect(half, half, w - OUTER, h - OUTER, radius);
    let _ = writeln!(content, "q /GS1 gs {} {outline}f Q", rgb(color, "rg"));
    // Double cadre : le trait, puis le filet à l'intérieur.
    let inset = half + INNER_GAP;
    let _ = writeln!(
        content,
        "{} 1 j {} w {outline}S {} w {}S",
        rgb(color, "RG"),
        num(OUTER),
        num(INNER),
        rounded_rect(
            inset,
            inset,
            w - 2.0 * inset,
            h - 2.0 * inset,
            (radius - INNER_GAP).max(1.0)
        )
    );
    // Le texte, centré : le libellé en capitales grasses, la ligne
    // dynamique dessous. Des capitales se centrent sur leur hauteur, pas sur
    // celle des jambages qu'elles n'ont pas.
    let cap = |size: f64, font: &TextFont| font.vertical().0.min(718.0) * size / 1000.0;
    let label_cap = cap(LABEL_SIZE, &label_font);
    let (label_y, second_y) = match &second {
        Some((_, font)) => {
            let small = cap(DYNAMIC_SIZE, font);
            let bottom = (h - (label_cap + LINE_GAP + small)) / 2.0;
            (bottom + small + LINE_GAP, bottom)
        }
        None => ((h - label_cap) / 2.0, 0.0),
    };
    let mut replaced = Vec::new();
    let _ = writeln!(
        content,
        "BT {} /F0 {} Tf 1 0 0 1 {} {} Tm {} Tj ET",
        rgb(color, "rg"),
        num(LABEL_SIZE),
        num((w - label_w) / 2.0),
        num(label_y),
        label_font.show(&label, &mut replaced)
    );
    if let Some((line, font)) = &second {
        let _ = writeln!(
            content,
            "BT {} /F1 {} Tf 1 0 0 1 {} {} Tm {} Tj ET",
            rgb(color, "rg"),
            num(DYNAMIC_SIZE),
            num((w - dynamic_w) / 2.0),
            num(second_y),
            font.show(line, &mut replaced)
        );
    }
    replaced.sort_unstable();
    replaced.dedup();
    if !replaced.is_empty() {
        let list: String = replaced.iter().collect();
        warnings.push(format!(
            "caractères hors de WinAnsiEncoding remplacés par « ? » : {list}"
        ));
    }
    (w, h)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::create::{new_document, Orientation, PageSetup, PageSize};
    use acrux_render::{render_page, RenderOptions};

    /// Page vierge de 400 × 300, relue comme un vrai fichier.
    fn blank(rotate: i32) -> Document {
        let doc = new_document(&PageSetup {
            size: PageSize::Custom {
                width: 400.0,
                height: 300.0,
            },
            orientation: Orientation::Landscape,
            ..PageSetup::default()
        })
        .unwrap();
        if rotate != 0 {
            crate::pages::rotate_pages(&doc, &[0], rotate).unwrap();
        }
        reload(&doc)
    }

    fn reload(doc: &Document) -> Document {
        Document::from_bytes(doc.save_full().unwrap()).unwrap()
    }

    fn options(stamp: StandardStamp) -> Options {
        Options {
            meta: AnnotMeta {
                author: Some("Nina".into()),
                name: Some("essai".into()),
                date: Some("D:20260923140500Z".into()),
            },
            ..Options::new(Source::Standard(stamp), 0, Point::new(200.0, 150.0))
        }
    }

    /// Dictionnaire de l'annotation posée.
    fn dict_of(doc: &Document, r: ObjectRef) -> Dict {
        doc.get(r).unwrap().as_dict().cloned().unwrap()
    }

    fn numbers(doc: &Document, d: &Dict, key: &str) -> Vec<f64> {
        crate::annotations::numbers(doc, d, key).unwrap()
    }

    /// Formulaire d'apparence et son contenu décodé.
    fn appearance(doc: &Document, d: &Dict) -> (Dict, String) {
        let ap = doc
            .dict_get(d, "AP")
            .unwrap()
            .unwrap()
            .as_dict()
            .cloned()
            .unwrap();
        let Some(Object::Reference(r)) = ap.get(&Name::new("N")) else {
            panic!("apparence absente");
        };
        let stream = doc.get(*r).unwrap();
        let data = doc.stream_data(&stream).unwrap().data;
        let Object::Stream { dict, .. } = &*stream else {
            panic!("flux attendu");
        };
        (dict.clone(), String::from_utf8_lossy(&data).into_owned())
    }

    #[test]
    fn les_douze_tampons_se_posent_avec_leur_nom_et_leur_libelle() {
        assert_eq!(StandardStamp::ALL.len(), 12);
        for stamp in StandardStamp::ALL {
            let doc = blank(0);
            let r = place(&doc, &options(stamp)).unwrap();
            let d = dict_of(&doc, r);
            assert_eq!(
                d.get(&Name::new("Subtype")),
                Some(&Object::Name(Name::new("Stamp")))
            );
            assert_eq!(
                d.get(&Name::new("Name")),
                Some(&Object::Name(Name::new(stamp.pdf_name())))
            );
            assert_eq!(
                d.get(&Name::new(TAG)),
                Some(&Object::Name(Name::new("Standard")))
            );
            let placed = list(&doc).unwrap();
            assert_eq!(placed.len(), 1);
            assert_eq!(
                placed[0].contents.as_deref(),
                Some(stamp.label(Language::Fr))
            );
            // Le rectangle a les proportions du dessin.
            let rect = numbers(&doc, &d, "Rect");
            let (bbox, content) = appearance(&doc, &d);
            let bbox = numbers(&doc, &bbox, "BBox");
            let ratio = (rect[2] - rect[0]) / (rect[3] - rect[1]);
            assert!((ratio - bbox[2] / bbox[3]).abs() < 1e-6, "{stamp:?}");
            assert!(content.contains(" c h "), "cadre arrondi : {content}");
        }
    }

    #[test]
    fn les_noms_se_reconnaissent_en_francais_et_en_anglais() {
        for (name, expected) in [
            ("approuve", StandardStamp::Approved),
            ("Approuvé", StandardStamp::Approved),
            ("APPROVED", StandardStamp::Approved),
            ("non-approuve", StandardStamp::NotApproved),
            ("Non approuvé", StandardStamp::NotApproved),
            ("NotApproved", StandardStamp::NotApproved),
            ("SBRejected", StandardStamp::Rejected),
            ("reçu", StandardStamp::Received),
            ("for comment", StandardStamp::ForComment),
            ("information", StandardStamp::InformationOnly),
            ("nul", StandardStamp::Void),
        ] {
            assert_eq!(StandardStamp::from_name(name), Some(expected), "{name}");
        }
        assert_eq!(StandardStamp::from_name("urgent"), None);
        assert_eq!(StandardStamp::from_name(""), None);
        let mut keys: Vec<&str> = StandardStamp::ALL.iter().map(|s| s.key()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), 12, "douze clés distinctes");
        for stamp in StandardStamp::ALL {
            assert_eq!(StandardStamp::from_name(stamp.key()), Some(stamp));
        }
    }

    #[test]
    fn le_tampon_se_voit_de_sa_couleur_et_laisse_le_reste_blanc() {
        let doc = blank(0);
        place(&doc, &options(StandardStamp::Approved)).unwrap();
        let doc = reload(&doc);
        let pages = collect_pages(&doc).unwrap();
        let rendered = render_page(
            &doc,
            &pages[0],
            2.0,
            &RenderOptions {
                annotations: true,
                ..RenderOptions::default()
            },
        )
        .bitmap;
        let r = list(&doc).unwrap()[0].rect;
        let rect = [r.x0, r.y0, r.x1, r.y1];
        // Espace de page vers pixels : échelle 2, y retourné.
        let px = |x: f64, y: f64| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let (x, y) = ((x * 2.0) as u32, ((300.0 - y) * 2.0) as u32);
            rendered.pixel(x, y).unwrap().map(i32::from)
        };
        // Sur le cadre, à mi-hauteur du bord gauche : le vert du tampon.
        let edge = px(rect[0] + 1.2, f64::midpoint(rect[1], rect[3]));
        assert!(
            edge[1] > edge[0] + 40 && edge[1] > edge[2] + 40,
            "vert attendu sur le cadre : {edge:?}"
        );
        // Dans le fond, entre le cadre et le texte : teinté, mais clair.
        let tint = px(rect[0] + 8.0, rect[1] + 6.0);
        assert!(
            tint[0] < 255 && tint[0] > 200,
            "fond teinté clair : {tint:?}"
        );
        // Hors du tampon : blanc.
        assert_eq!(px(rect[0] - 10.0, rect[1] - 10.0)[..3], [255, 255, 255]);
    }

    #[test]
    fn la_ligne_dynamique_a_sa_forme_et_grandit_le_tampon() {
        let date = "D:20260923140500Z";
        assert_eq!(
            dynamic_line_at("Nina", Language::Fr, date),
            "par Nina, le 23/09/2026 à 14:05"
        );
        assert_eq!(
            dynamic_line_at("Nina", Language::En, date),
            "By Nina at 2:05 PM, Sep 23, 2026"
        );
        assert_eq!(
            dynamic_line_at("", Language::Fr, "D:20260101000900Z"),
            "le 01/01/2026 à 00:09"
        );
        assert_eq!(
            dynamic_line_at("", Language::En, "D:20260101000900Z"),
            "at 12:09 AM, Jan 1, 2026"
        );
        // La forme, pour l'heure présente.
        let now = dynamic_line("Nina", Language::Fr, 120);
        assert!(
            now.starts_with("par Nina, le ") && now.contains(" à "),
            "{now}"
        );

        let doc = blank(0);
        let mut o = options(StandardStamp::Confidential);
        o.dynamic = Some(dynamic_line_at("Nina", Language::Fr, date));
        let r = place(&doc, &o).unwrap();
        let d = dict_of(&doc, r);
        let rect = numbers(&doc, &d, "Rect");
        assert!((rect[3] - rect[1] - HEIGHT_DYNAMIC).abs() < 1e-6);
        assert_eq!(
            d.get(&Name::new(TAG)),
            Some(&Object::Name(Name::new("Dynamic")))
        );
        let placed = list(&doc).unwrap();
        assert_eq!(
            placed[0].contents.as_deref(),
            Some("Confidentiel\npar Nina, le 23/09/2026 à 14:05")
        );
        // Mêmes options, même apparence : la pose se rejoue à l'identique.
        let again = blank(0);
        let r2 = place(&again, &o).unwrap();
        assert_eq!(
            appearance(&doc, &d).1,
            appearance(&again, &dict_of(&again, r2)).1
        );
        assert_eq!(
            dict_of(&doc, r).get(&Name::new("NM")),
            dict_of(&again, r2).get(&Name::new("NM"))
        );
    }

    #[test]
    fn un_auteur_hors_winansi_ne_devient_pas_des_points_dinterrogation() {
        let doc = blank(0);
        let mut o = options(StandardStamp::Approved);
        o.dynamic = Some(dynamic_line_at("Иван", Language::Fr, "D:20260923140500Z"));
        let mut warnings = Vec::new();
        let form = build_form(&doc, &o, 0, &mut warnings);
        let stream = doc.get(form.reference).unwrap();
        let content = String::from_utf8_lossy(&doc.stream_data(&stream).unwrap().data).into_owned();
        // Soit une police système couvre le cyrillique, et rien n'est
        // remplacé ; soit aucune ne le couvre, et c'est dit.
        assert!(
            !content.contains("(par ?") || !warnings.is_empty(),
            "{content} {warnings:?}"
        );
    }

    #[test]
    fn un_tampon_image_garde_ses_proportions_et_sa_transparence() {
        let data = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/corpus/images/motif.png"),
        )
        .unwrap();
        let image = Arc::new(crate::stamp::prepare_image(&data).unwrap());
        let (px_w, px_h) = (f64::from(image.width()), f64::from(image.height()));
        let doc = blank(0);
        let mut stamp = options(StandardStamp::Approved);
        stamp.source = Source::Image(Arc::clone(&image));
        let annot = dict_of(&doc, place(&doc, &stamp).unwrap());
        assert_eq!(
            annot.get(&Name::new(TAG)),
            Some(&Object::Name(Name::new("Image")))
        );
        let rect = numbers(&doc, &annot, "Rect");
        let natural = (px_w * 0.75, px_h * 0.75);
        let shrink = (MAX_IMAGE_SIDE / natural.0.max(natural.1)).min(1.0);
        assert!((rect[2] - rect[0] - natural.0 * shrink).abs() < 1e-6);
        assert!((rect[3] - rect[1] - natural.1 * shrink).abs() < 1e-6);
        let (form, content) = appearance(&doc, &annot);
        assert!(content.contains("/Im0 Do"), "{content}");
        let resources = doc.dict_get(&form, "Resources").unwrap().unwrap();
        let xobjects = doc
            .dict_get(resources.as_dict().unwrap(), "XObject")
            .unwrap()
            .unwrap()
            .as_dict()
            .cloned()
            .unwrap();
        let Some(Object::Reference(im)) = xobjects.get(&Name::new("Im0")) else {
            panic!("image absente");
        };
        let im = doc.get(*im).unwrap();
        let Object::Stream { dict, .. } = &*im else {
            panic!("flux attendu");
        };
        assert_eq!(
            dict.get(&Name::new("Subtype")),
            Some(&Object::Name(Name::new("Image")))
        );
        assert_eq!(
            dict.contains_key(&Name::new("SMask")),
            image.has_alpha(),
            "le masque suit la transparence"
        );
    }

    #[test]
    fn sur_une_page_tournee_le_tampon_est_redresse() {
        let doc = blank(90);
        let r = place(&doc, &options(StandardStamp::Approved)).unwrap();
        let d = dict_of(&doc, r);
        let (form, _) = appearance(&doc, &d);
        assert_eq!(
            numbers(&doc, &form, "Matrix"),
            [0.0, 1.0, -1.0, 0.0, 0.0, 0.0]
        );
        let rect = numbers(&doc, &d, "Rect");
        assert!(
            rect[3] - rect[1] > rect[2] - rect[0],
            "plus haut que large dans la page : {rect:?}"
        );
        // Rendu tourné : à l'écran, le tampon est plus large que haut.
        let doc = reload(&doc);
        let pages = collect_pages(&doc).unwrap();
        let bitmap = render_page(
            &doc,
            &pages[0],
            1.0,
            &RenderOptions {
                annotations: true,
                ..RenderOptions::default()
            },
        )
        .bitmap;
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for y in 0..bitmap.height() {
            for x in 0..bitmap.width() {
                let p = bitmap.pixel(x, y).unwrap().map(i32::from);
                if p[1] > p[0] + 60 {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        assert!(x1 > x0 && y1 > y0, "rien de vert à l'écran");
        assert!(x1 - x0 > y1 - y0, "plus large que haut à l'écran");
    }

    #[test]
    fn le_coin_superieur_droit_est_celui_quon_voit() {
        let crop = Rect::new(0.0, 0.0, 400.0, 300.0);
        let size = (120.0, 34.0);
        let p = top_right(crop, 0, size, 36.0);
        assert!(
            (p.x - 304.0).abs() < 1e-9 && (p.y - 247.0).abs() < 1e-9,
            "{p:?}"
        );
        // Page tournée d'un quart de tour à droite : le coin supérieur droit
        // de l'écran est le coin supérieur gauche de la page.
        let r = stamp_rect(top_right(crop, 90, size, 36.0), size, crop, 90);
        assert!((r.x0 - 36.0).abs() < 1e-9, "{r:?}");
        assert!((r.y1 - 264.0).abs() < 1e-9, "{r:?}");
    }

    #[test]
    fn le_tampon_reste_dans_la_page() {
        let crop = Rect::new(0.0, 0.0, 400.0, 300.0);
        // Posé au bord : ramené dedans.
        let r = stamp_rect(Point::new(395.0, 5.0), (120.0, 34.0), crop, 0);
        assert!((r.x1 - 400.0).abs() < 1e-9 && r.y0.abs() < 1e-9, "{r:?}");
        // À 90°, les côtés s'échangent.
        let r = stamp_rect(Point::new(200.0, 150.0), (120.0, 34.0), crop, 90);
        assert!((r.width() - 34.0).abs() < 1e-9 && (r.height() - 120.0).abs() < 1e-9);
        // Plus grand que la page : centré sur elle.
        let r = stamp_rect(Point::new(10.0, 10.0), (500.0, 34.0), crop, 0);
        assert!((r.x0 + 50.0).abs() < 1e-9, "{r:?}");
    }

    #[test]
    fn apres_relecture_le_tampon_est_la_et_survit_aux_filigranes() {
        let doc = blank(0);
        place(&doc, &options(StandardStamp::Paid)).unwrap();
        let doc = reload(&doc);
        let pages = collect_pages(&doc).unwrap();
        let annots = crate::annotations::list_annotations(&doc, &pages[0]).unwrap();
        assert_eq!(annots.len(), 1);
        assert_eq!(annots[0].subtype, "Stamp");
        assert!(!annots[0].fill_sign, "pas un élément de remplir et signer");
        crate::stamp::remove_stamps(
            &doc,
            &[
                crate::stamp::StampKind::Watermark,
                crate::stamp::StampKind::Background,
                crate::stamp::StampKind::HeaderFooter,
                crate::stamp::StampKind::Bates,
            ],
        )
        .unwrap();
        assert_eq!(list(&doc).unwrap().len(), 1);
    }

    #[test]
    fn le_tampon_se_deplace_et_sagrandit_comme_un_commentaire() {
        let doc = blank(0);
        place(&doc, &options(StandardStamp::Draft)).unwrap();
        let pages = collect_pages(&doc).unwrap();
        let before = list(&doc).unwrap()[0].rect;
        let bigger = Rect::new(
            before.x0,
            before.y0,
            before.x0 + before.width() * 2.0,
            before.y0 + before.height() * 2.0,
        );
        crate::annotations::set_annotation_properties(
            &doc,
            &pages[0],
            0,
            &crate::annotations::AnnotChanges {
                rect: Some(bigger),
                ..crate::annotations::AnnotChanges::default()
            },
        )
        .unwrap();
        assert_eq!(list(&doc).unwrap()[0].rect, bigger);
    }
}
