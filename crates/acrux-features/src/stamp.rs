//! Apports visuels posés sur un document existant (inventaire Acrobat §4) :
//! filigranes, arrière-plans, en-têtes et pieds de page, numérotation Bates.
//!
//! # Principe
//!
//! Le contenu d'origine des pages n'est **jamais** réécrit. Chaque tampon est
//! un flux de contenu supplémentaire ajouté au tableau `/Contents`
//! (ISO 32000-2 §7.7.3.3), et le contenu existant est encadré par deux
//! minuscules flux `q` et `Q` quand le tampon passe **devant** lui, pour que
//! l'état graphique laissé par la page ne déborde pas sur le tampon :
//!
//! | Position | `/Contents` après pose |
//! |----------|------------------------|
//! | derrière | `[tampon, contenu…]` |
//! | devant   | `[q, contenu…, Q, tampon]` |
//!
//! Les flux d'origine gardent donc leurs octets exacts, ce qui rend la pose
//! **réversible** : [`remove_stamps`] retire les flux ajoutés et rétablit un
//! `/Contents` de la même forme qu'avant. Chaque flux posé est reconnaissable
//! à la clé privée `/AKStamp` de son dictionnaire.
//!
//! # Filigranes et arrière-plans
//!
//! Le dessin lui-même est un **XObject de formulaire unique**, partagé par
//! toutes les pages : seule la matrice `cm` change d'une page à l'autre, si
//! bien qu'un filigrane sur 500 pages ne coûte qu'un objet. Le texte est
//! dessiné avec une police standard (§9.6.2.2) accompagnée d'un `/ToUnicode`,
//! donc reste extractible ; une image PNG ou JPEG devient un XObject image
//! (le JPEG est incorporé tel quel, sans recompression).
//!
//! L'opacité passe par un `/ExtGState` (`/ca`, `/CA`) et le formulaire reçoit
//! un groupe de transparence, pour que les traits d'un même tampon ne se
//! renforcent pas entre eux là où ils se recouvrent.
//!
//! # Placement
//!
//! Tout se place dans l'**espace d'affichage** de la page : l'origine est le
//! coin inférieur gauche de la `CropBox` *après* application de `/Rotate`, si
//! bien qu'un filigrane reste horizontal sur une page pivotée. Neuf ancrages,
//! un décalage libre, une rotation libre et une échelle — absolue, relative à
//! la page (sans déformation) ou étirée — complètent le placement.
//!
//! # Ce qui n'est pas réversible
//!
//! - les ressources ajoutées (`/AKS…` dans `/XObject`, `/ExtGState`,
//!   `/Font`) ne sont retirées que lorsque la page n'a plus aucun tampon ;
//! - une page qui héritait de ses `/Resources` d'un nœud `/Pages` reçoit sa
//!   propre copie ;
//! - les objets devenus inatteignables (formulaire, police) ne disparaissent
//!   qu'à la réécriture complète avec `SaveOptions::drop_unreferenced`.

// `create` incorpore les mêmes images (une page par photo) : le décodeur est
// partagé plutôt que réécrit.
pub(crate) mod image;
pub(crate) mod metrics;
mod numbering;

use std::fmt::Write as _;

use acrux_core::{Error, Matrix, Result};
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef, Page};

pub use crate::annotations::Rgb;
pub use metrics::{encode_win_ansi, pdf_literal, Encoded, StandardFont};
pub use numbering::NumberFormat;

/// Clé privée marquant les flux de contenu posés par ce module.
const MARK: &str = "AKStamp";

/// Préfixe des noms de ressources ajoutés par ce module.
const PREFIX: &str = "AKS";

// --- Types publics ----------------------------------------------------------

/// Nature d'un tampon, telle qu'elle est inscrite dans le flux posé.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampKind {
    /// Filigrane (devant ou derrière le contenu).
    Watermark,
    /// Arrière-plan (toujours derrière le contenu).
    Background,
    /// En-tête et pied de page.
    HeaderFooter,
    /// Numérotation Bates.
    Bates,
}

impl StampKind {
    fn tag(self) -> &'static str {
        match self {
            StampKind::Watermark => "Watermark",
            StampKind::Background => "Background",
            StampKind::HeaderFooter => "HeaderFooter",
            StampKind::Bates => "Bates",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "Watermark" => Some(StampKind::Watermark),
            "Background" => Some(StampKind::Background),
            "HeaderFooter" => Some(StampKind::HeaderFooter),
            "Bates" => Some(StampKind::Bates),
            _ => None,
        }
    }

    /// Nom court accepté en ligne de commande (`filigrane`, `arriere-plan`…).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "filigrane" | "watermark" => Some(StampKind::Watermark),
            "arriere-plan" | "background" => Some(StampKind::Background),
            "entete-pied" | "header-footer" => Some(StampKind::HeaderFooter),
            "bates" => Some(StampKind::Bates),
            _ => None,
        }
    }
}

/// Un des neuf ancrages de la page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Anchor {
    /// Coin supérieur gauche.
    TopLeft,
    /// Bord supérieur, centré.
    TopCenter,
    /// Coin supérieur droit.
    TopRight,
    /// Bord gauche, à mi-hauteur.
    MiddleLeft,
    /// Centre de la page.
    #[default]
    Center,
    /// Bord droit, à mi-hauteur.
    MiddleRight,
    /// Coin inférieur gauche.
    BottomLeft,
    /// Bord inférieur, centré.
    BottomCenter,
    /// Coin inférieur droit.
    BottomRight,
}

impl Anchor {
    /// Ancrage d'après son nom : `haut-gauche`, `centre`, `bas-droite`…
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "haut-gauche" => Some(Anchor::TopLeft),
            "haut-centre" | "haut" => Some(Anchor::TopCenter),
            "haut-droite" => Some(Anchor::TopRight),
            "milieu-gauche" | "gauche" => Some(Anchor::MiddleLeft),
            "centre" => Some(Anchor::Center),
            "milieu-droite" | "droite" => Some(Anchor::MiddleRight),
            "bas-gauche" => Some(Anchor::BottomLeft),
            "bas-centre" | "bas" => Some(Anchor::BottomCenter),
            "bas-droite" => Some(Anchor::BottomRight),
            _ => None,
        }
    }

    /// Fractions horizontale et verticale : 0 (gauche / bas), 0,5 (centre) ou
    /// 1 (droite / haut).
    #[must_use]
    pub fn factors(self) -> (f64, f64) {
        let x = match self {
            Anchor::TopLeft | Anchor::MiddleLeft | Anchor::BottomLeft => 0.0,
            Anchor::TopCenter | Anchor::Center | Anchor::BottomCenter => 0.5,
            _ => 1.0,
        };
        let y = match self {
            Anchor::BottomLeft | Anchor::BottomCenter | Anchor::BottomRight => 0.0,
            Anchor::MiddleLeft | Anchor::Center | Anchor::MiddleRight => 0.5,
            _ => 1.0,
        };
        (x, y)
    }
}

/// Échelle du tampon.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scale {
    /// Facteur absolu appliqué au dessin (1 = taille naturelle : la taille de
    /// police demandée, ou 1 pixel d'image = 1 point).
    Absolute(f64),
    /// Fraction de la page : le tampon est agrandi ou réduit pour occuper au
    /// plus cette fraction de la largeur **et** de la hauteur disponibles,
    /// sans déformation.
    RelativeToPage(f64),
    /// Étirement : le dessin couvre toute la zone disponible, au besoin en
    /// déformant ses proportions. C'est ce qu'il faut pour un aplat de fond.
    Stretch,
}

impl Default for Scale {
    fn default() -> Self {
        Scale::Absolute(1.0)
    }
}

/// Position d'un tampon sur la page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Ancrage dans la page.
    pub anchor: Anchor,
    /// Décalage en points ajouté après l'ancrage (x vers la droite, y vers le
    /// haut, dans l'espace d'affichage).
    pub offset: (f64, f64),
    /// Rotation en degrés, sens trigonométrique, autour du centre du tampon.
    pub rotation: f64,
    /// Échelle.
    pub scale: Scale,
    /// Marge conservée sur les quatre bords, en points.
    pub margin: f64,
}

impl Default for Placement {
    fn default() -> Self {
        Placement {
            anchor: Anchor::Center,
            offset: (0.0, 0.0),
            rotation: 0.0,
            scale: Scale::default(),
            margin: 0.0,
        }
    }
}

/// Ce qui est dessiné par un filigrane ou un arrière-plan.
#[derive(Debug, Clone)]
pub enum StampSource {
    /// Texte, éventuellement sur plusieurs lignes (séparées par `\n`),
    /// centrées les unes par rapport aux autres.
    Text {
        /// Texte à dessiner.
        text: String,
        /// Police standard.
        font: StandardFont,
        /// Corps en points.
        size: f64,
        /// Couleur de remplissage.
        color: Rgb,
    },
    /// Image fournie en octets (PNG, JPEG, BMP, GIF, TIFF). À l'échelle 1,
    /// un pixel vaut un point.
    Image {
        /// Octets du fichier image.
        data: Vec<u8>,
    },
    /// Aplat de couleur unie couvrant toute la boîte du tampon.
    Fill {
        /// Couleur.
        color: Rgb,
    },
}

/// Opacité d'un tampon (`/ca` pour les remplissages, `/CA` pour les traits).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Opacity {
    /// Opacité des remplissages, de 0 (invisible) à 1 (opaque).
    pub fill: f64,
    /// Opacité des traits.
    pub stroke: f64,
}

impl Default for Opacity {
    fn default() -> Self {
        Opacity {
            fill: 1.0,
            stroke: 1.0,
        }
    }
}

impl Opacity {
    fn is_opaque(self) -> bool {
        self.fill >= 1.0 && self.stroke >= 1.0
    }
}

/// Options d'un filigrane.
#[derive(Debug, Clone)]
pub struct WatermarkOptions {
    /// Ce qui est dessiné.
    pub source: StampSource,
    /// Où et comment.
    pub placement: Placement,
    /// Opacité.
    pub opacity: Opacity,
    /// `true` : sous le contenu de la page ; `false` : par-dessus.
    pub behind: bool,
    /// Pages visées (indices à partir de 0) ; vide = toutes.
    pub pages: Vec<usize>,
}

impl Default for WatermarkOptions {
    fn default() -> Self {
        WatermarkOptions {
            source: StampSource::Text {
                text: "BROUILLON".into(),
                font: StandardFont::HelveticaBold,
                size: 48.0,
                color: [0.5, 0.5, 0.5],
            },
            placement: Placement {
                rotation: 45.0,
                scale: Scale::RelativeToPage(0.8),
                ..Placement::default()
            },
            opacity: Opacity {
                fill: 0.25,
                stroke: 0.25,
            },
            behind: false,
            pages: Vec::new(),
        }
    }
}

/// Options d'un arrière-plan : comme un filigrane, mais toujours **derrière**
/// le contenu et, pour un aplat de couleur, étendu à toute la page.
#[derive(Debug, Clone)]
pub struct BackgroundOptions {
    /// Ce qui est dessiné.
    pub source: StampSource,
    /// Où et comment. Ignoré pour [`StampSource::Fill`], qui couvre la page.
    pub placement: Placement,
    /// Opacité.
    pub opacity: Opacity,
    /// Pages visées ; vide = toutes.
    pub pages: Vec<usize>,
}

impl Default for BackgroundOptions {
    fn default() -> Self {
        BackgroundOptions {
            source: StampSource::Fill {
                color: [0.95, 0.95, 0.9],
            },
            placement: Placement::default(),
            opacity: Opacity::default(),
            pages: Vec::new(),
        }
    }
}

/// Marges d'un en-tête ou d'un pied de page, en points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Margins {
    /// Distance entre le haut de la page et le haut du texte d'en-tête.
    pub top: f64,
    /// Distance entre le bas de la page et la ligne de base du pied.
    pub bottom: f64,
    /// Marge gauche.
    pub left: f64,
    /// Marge droite.
    pub right: f64,
}

impl Default for Margins {
    fn default() -> Self {
        Margins {
            top: 36.0,
            bottom: 36.0,
            left: 54.0,
            right: 54.0,
        }
    }
}

/// Options d'un en-tête et d'un pied de page.
///
/// Les six zones (`header` et `footer`, dans l'ordre gauche, centre, droite)
/// reçoivent un gabarit dont les jetons `{page}`, `{pages}`, `{date}`,
/// `{time}`, `{datetime}` et `{filename}` sont remplacés page par page (voir
/// [`numbering`]). Une zone vide n'écrit rien.
#[derive(Debug, Clone)]
pub struct HeaderFooterOptions {
    /// En-tête : gauche, centre, droite.
    pub header: [String; 3],
    /// Pied de page : gauche, centre, droite.
    pub footer: [String; 3],
    /// Police standard.
    pub font: StandardFont,
    /// Corps en points.
    pub size: f64,
    /// Couleur du texte.
    pub color: Rgb,
    /// Marges.
    pub margins: Margins,
    /// Numéro affiché sur la **première page visée** ; les suivantes
    /// s'incrémentent de 1.
    pub start_number: i64,
    /// Format du numéro de page.
    pub format: NumberFormat,
    /// Valeur du jeton `{filename}`.
    pub file_name: String,
    /// Pages visées ; vide = toutes.
    pub pages: Vec<usize>,
    /// Décalage des jetons `{date}`, `{time}` et `{datetime}` par rapport à
    /// UTC, en minutes (120 pour l'heure d'été d'Europe de l'Ouest).
    pub utc_offset_minutes: i32,
}

impl Default for HeaderFooterOptions {
    fn default() -> Self {
        HeaderFooterOptions {
            header: [String::new(), String::new(), String::new()],
            footer: [String::new(), "{page} / {pages}".into(), String::new()],
            font: StandardFont::Helvetica,
            size: 9.0,
            color: [0.0, 0.0, 0.0],
            margins: Margins::default(),
            start_number: 1,
            format: NumberFormat::Decimal,
            file_name: String::new(),
            pages: Vec::new(),
            utc_offset_minutes: 0,
        }
    }
}

/// Options d'une numérotation Bates.
#[derive(Debug, Clone)]
pub struct BatesOptions {
    /// Texte placé avant le numéro.
    pub prefix: String,
    /// Texte placé après le numéro.
    pub suffix: String,
    /// Nombre de chiffres du numéro (complété par des zéros).
    pub digits: usize,
    /// Premier numéro.
    pub start: u64,
    /// Ancrage du numéro.
    pub anchor: Anchor,
    /// Marge horizontale, en points.
    pub margin_x: f64,
    /// Marge verticale, en points.
    pub margin_y: f64,
    /// Police standard.
    pub font: StandardFont,
    /// Corps en points.
    pub size: f64,
    /// Couleur.
    pub color: Rgb,
    /// Pages visées ; vide = toutes.
    pub pages: Vec<usize>,
}

impl Default for BatesOptions {
    fn default() -> Self {
        BatesOptions {
            prefix: String::new(),
            suffix: String::new(),
            digits: 6,
            start: 1,
            anchor: Anchor::BottomRight,
            margin_x: 36.0,
            margin_y: 24.0,
            font: StandardFont::Helvetica,
            size: 9.0,
            color: [0.0, 0.0, 0.0],
            pages: Vec::new(),
        }
    }
}

/// Résultat de la pose d'un tampon.
#[derive(Debug, Clone, Default)]
pub struct StampReport {
    /// Nombre de pages modifiées.
    pub pages: usize,
    /// Avertissements (caractères non représentables, jetons inconnus…).
    pub warnings: Vec<String>,
}

/// Résultat d'une numérotation Bates.
#[derive(Debug, Clone, Default)]
pub struct BatesReport {
    /// Nombre de pages numérotées.
    pub pages: usize,
    /// Premier numéro posé, si au moins une page l'a été.
    pub first: Option<u64>,
    /// Dernier numéro posé.
    pub last: Option<u64>,
    /// Numéro qui suivrait : c'est lui qu'il faut passer au document suivant
    /// pour que la numérotation continue.
    pub next: u64,
    /// Avertissements.
    pub warnings: Vec<String>,
}

// --- Filigrane et arrière-plan ---------------------------------------------

/// Pose un filigrane sur les pages demandées.
///
/// Le dessin est un XObject de formulaire unique, partagé par toutes les
/// pages ; seule la matrice de placement change d'une page à l'autre.
///
/// # Errors
/// Page non indirecte, image illisible, ou document dont le catalogue est
/// inexploitable.
pub fn add_watermark(doc: &Document, options: &WatermarkOptions) -> Result<StampReport> {
    stamp(
        doc,
        &StampJob {
            kind: StampKind::Watermark,
            source: &options.source,
            placement: options.placement,
            opacity: options.opacity,
            behind: options.behind,
            pages: &options.pages,
        },
    )
}

/// Pose un arrière-plan (toujours sous le contenu) sur les pages demandées.
///
/// Un [`StampSource::Fill`] couvre la page entière ; une image ou un texte
/// suit le placement demandé, exactement comme un filigrane.
///
/// # Errors
/// Mêmes cas que [`add_watermark`].
pub fn add_background(doc: &Document, options: &BackgroundOptions) -> Result<StampReport> {
    let placement = match options.source {
        // Un aplat couvre toute la page : ancrage et échelle n'ont pas de sens.
        StampSource::Fill { .. } => Placement {
            anchor: Anchor::Center,
            offset: (0.0, 0.0),
            rotation: 0.0,
            scale: Scale::Stretch,
            margin: 0.0,
        },
        _ => options.placement,
    };
    stamp(
        doc,
        &StampJob {
            kind: StampKind::Background,
            source: &options.source,
            placement,
            opacity: options.opacity,
            behind: true,
            pages: &options.pages,
        },
    )
}

/// Tout ce qui définit la pose d'un filigrane ou d'un arrière-plan.
struct StampJob<'a> {
    kind: StampKind,
    source: &'a StampSource,
    placement: Placement,
    opacity: Opacity,
    /// `true` : sous le contenu de la page.
    behind: bool,
    /// Indices de pages ; vide = toutes.
    pages: &'a [usize],
}

/// Corps commun au filigrane et à l'arrière-plan.
fn stamp(doc: &Document, job: &StampJob) -> Result<StampReport> {
    let targets = selected_pages(doc, job.pages)?;
    let mut report = StampReport::default();
    if targets.is_empty() {
        return Ok(report);
    }
    let form = build_form(doc, job.source, &mut report.warnings)?;
    let opacity = job.opacity;
    let gs = (!opacity.is_opaque()).then(|| extended_state(doc, opacity));
    let mut writer = Writer::new(doc);
    for page in &targets {
        let name = add_resource(doc, page, "XObject", Object::Reference(form.reference))?;
        let gs_name = gs
            .map(|r| add_resource(doc, page, "ExtGState", Object::Reference(r)))
            .transpose()?;
        let (page_w, page_h, to_user) = display_space(doc, page);
        let matrix =
            placement_matrix(form.width, form.height, page_w, page_h, job.placement).then(&to_user);
        let mut content = String::from("q\n");
        if let Some(g) = &gs_name {
            let _ = writeln!(content, "/{} gs", g.as_str());
        }
        let _ = writeln!(content, "{} cm", matrix_text(&matrix));
        let _ = writeln!(content, "/{} Do", name.as_str());
        content.push_str("Q\n");
        writer.attach(page, job.kind, content.into_bytes(), job.behind)?;
        report.pages += 1;
    }
    Ok(report)
}

// --- En-têtes et pieds de page ---------------------------------------------

/// Pose un en-tête et un pied de page sur les pages demandées.
///
/// Le texte est écrit directement dans un flux ajouté à la page (il change
/// d'une page à l'autre, un formulaire partagé n'aurait pas de sens), mais la
/// police, elle, est un objet unique partagé.
///
/// # Errors
/// Page non indirecte ou ressources illisibles.
pub fn add_header_footer(doc: &Document, options: &HeaderFooterOptions) -> Result<StampReport> {
    let targets = selected_pages(doc, &options.pages)?;
    let mut report = StampReport::default();
    if targets.is_empty() {
        return Ok(report);
    }
    let total = collect_pages(doc)?.len();
    // Tout ce qui pourra être écrit : les gabarits, le nom de fichier et les
    // caractères que produisent les jetons (chiffres, romains, lettres,
    // séparateurs de date). C'est ce qui décide de la police à poser.
    let mut alphabet = String::new();
    for template in options.header.iter().chain(options.footer.iter()) {
        alphabet.push_str(template);
    }
    alphabet.push_str(&options.file_name);
    alphabet.push_str("0123456789ivxlcdmIVXLCDMabcdefghjknopqrstuwyz -:/.");
    let (text_font, font) = TextFont::resolve(doc, &alphabet, options.font, &mut report.warnings);
    let (date, time) = numbering::now_at(options.utc_offset_minutes);
    let mut unknown = Vec::new();
    let mut writer = Writer::new(doc);
    for (rank, page) in targets.iter().enumerate() {
        let number = options
            .start_number
            .saturating_add(i64::try_from(rank).unwrap_or(0));
        let tokens = numbering::Tokens {
            page: options.format.format(number),
            pages: total,
            file_name: options.file_name.clone(),
            bates: None,
            date: date.clone(),
            time: time.clone(),
        };
        let (page_w, page_h, to_user) = display_space(doc, page);
        // Zone verticale : 2 = en-tête (haut), 0 = pied (bas).
        let mut zones = Vec::new();
        for (index, template) in options.header.iter().enumerate() {
            zones.push((2u8, u8::try_from(index).unwrap_or(0), template));
        }
        for (index, template) in options.footer.iter().enumerate() {
            zones.push((0u8, u8::try_from(index).unwrap_or(0), template));
        }
        let mut lines = Vec::new();
        for (v, h, template) in zones {
            if template.is_empty() {
                continue;
            }
            let text = tokens.expand(template, &mut unknown);
            if text.is_empty() {
                continue;
            }
            let width = text_font.width(&text, options.size);
            let x = horizontal_x(
                h,
                page_w,
                width,
                options.margins.left,
                options.margins.right,
            );
            let y = baseline_y(
                v,
                page_h,
                &text_font,
                options.size,
                options.margins.top,
                options.margins.bottom,
            );
            lines.push((x, y, text));
        }
        if lines.is_empty() {
            continue;
        }
        let name = add_resource(doc, page, "Font", Object::Reference(font))?;
        let style = TextStyle {
            font_name: &name,
            font: &text_font,
            size: options.size,
            color: options.color,
        };
        let content = text_block(&to_user, &style, &lines, &mut report.warnings);
        writer.attach(page, StampKind::HeaderFooter, content, false)?;
        report.pages += 1;
    }
    finish_font(doc, font);
    unknown.sort_unstable();
    unknown.dedup();
    for token in unknown {
        report
            .warnings
            .push(format!("jeton inconnu laissé tel quel : {{{token}}}"));
    }
    Ok(report)
}

// --- Numérotation Bates -----------------------------------------------------

/// Pose une numérotation Bates sur les pages demandées, en partant de
/// `options.start`.
///
/// # Errors
/// Page non indirecte ou ressources illisibles.
pub fn add_bates(doc: &Document, options: &BatesOptions) -> Result<BatesReport> {
    let targets = selected_pages(doc, &options.pages)?;
    let mut report = BatesReport {
        next: options.start,
        ..BatesReport::default()
    };
    if targets.is_empty() {
        return Ok(report);
    }
    let alphabet = format!("{}{}0123456789", options.prefix, options.suffix);
    let (text_font, font) = TextFont::resolve(doc, &alphabet, options.font, &mut report.warnings);
    let mut writer = Writer::new(doc);
    for (rank, page) in targets.iter().enumerate() {
        let number = options.start.saturating_add(rank as u64);
        let label =
            numbering::bates_label(&options.prefix, number, options.digits, &options.suffix);
        let (page_w, page_h, to_user) = display_space(doc, page);
        let width = text_font.width(&label, options.size);
        let (fx, fy) = options.anchor.factors();
        let x = horizontal_x(
            zone_index(fx),
            page_w,
            width,
            options.margin_x,
            options.margin_x,
        );
        let y = baseline_y(
            zone_index(fy),
            page_h,
            &text_font,
            options.size,
            options.margin_y,
            options.margin_y,
        );
        let name = add_resource(doc, page, "Font", Object::Reference(font))?;
        let style = TextStyle {
            font_name: &name,
            font: &text_font,
            size: options.size,
            color: options.color,
        };
        let content = text_block(&to_user, &style, &[(x, y, label)], &mut report.warnings);
        writer.attach(page, StampKind::Bates, content, false)?;
        report.pages += 1;
        report.first.get_or_insert(number);
        report.last = Some(number);
        report.next = number.saturating_add(1);
    }
    finish_font(doc, font);
    Ok(report)
}

/// Numérote plusieurs documents à la suite : le premier part de
/// `options.start`, chacun des suivants reprend au numéro laissé par le
/// précédent.
///
/// # Errors
/// Mêmes cas que [`add_bates`] ; le premier document en échec interrompt la
/// série (les précédents restent modifiés en mémoire).
pub fn add_bates_sequence(docs: &[&Document], options: &BatesOptions) -> Result<Vec<BatesReport>> {
    let mut out = Vec::with_capacity(docs.len());
    let mut next = options.start;
    for doc in docs {
        let step = BatesOptions {
            start: next,
            ..options.clone()
        };
        let report = add_bates(doc, &step)?;
        next = report.next;
        out.push(report);
    }
    Ok(out)
}

// --- Retrait ----------------------------------------------------------------

/// Retire les tampons des natures demandées de toutes les pages et renvoie le
/// nombre de flux retirés.
///
/// Les flux d'origine n'ayant jamais été réécrits, le `/Contents` retrouve sa
/// forme initiale (une référence simple redevient une référence simple). Les
/// ressources `/AKS…` ne sont retirées que lorsque la page n'a plus aucun
/// tampon ; les objets devenus inatteignables disparaissent à la réécriture
/// complète avec `SaveOptions::drop_unreferenced`.
///
/// # Errors
/// `/Contents` illisible.
pub fn remove_stamps(doc: &Document, kinds: &[StampKind]) -> Result<usize> {
    let mut removed = 0;
    for page in collect_pages(doc)? {
        let Some(page_ref) = page.reference else {
            continue;
        };
        let mut dict = current_dict(doc, &page)?;
        let items = content_items(doc, &dict);
        let mut kept = Vec::new();
        let mut leftover = false;
        for item in items {
            match marker(doc, &item) {
                Some(Marker::Stamp(k)) if kinds.contains(&k) => {
                    if let Object::Reference(r) = item {
                        doc.delete(r);
                    }
                    removed += 1;
                }
                Some(Marker::Stamp(_)) => {
                    leftover = true;
                    kept.push(item);
                }
                _ => kept.push(item),
            }
        }
        if removed == 0 && !leftover {
            continue;
        }
        if !leftover {
            // Plus aucun tampon : les encadrements `q`/`Q` et les ressources
            // ajoutées n'ont plus de raison d'être.
            kept.retain(|item| {
                let drop = matches!(marker(doc, item), Some(Marker::Wrap));
                if drop {
                    if let Object::Reference(r) = item {
                        doc.delete(*r);
                    }
                }
                !drop
            });
            strip_resources(doc, &page, &mut dict);
        }
        match kept.len() {
            0 => {
                dict.remove(&Name::new("Contents"));
            }
            1 if matches!(kept[0], Object::Reference(_)) => {
                dict.insert(Name::new("Contents"), kept.remove(0));
            }
            _ => {
                dict.insert(Name::new("Contents"), Object::Array(kept));
            }
        }
        doc.set(page_ref, Object::Dict(dict));
    }
    Ok(removed)
}

/// Nature d'un flux de contenu de page.
enum Marker {
    Stamp(StampKind),
    Wrap,
}

fn marker(doc: &Document, item: &Object) -> Option<Marker> {
    let resolved = doc.resolve(item).ok()?;
    let tag = resolved.as_dict()?.get(&Name::new(MARK))?.as_name()?;
    match tag.as_str().as_str() {
        "Wrap" => Some(Marker::Wrap),
        other => StampKind::from_tag(other).map(Marker::Stamp),
    }
}

/// Retire du dictionnaire de ressources tout ce que ce module y a mis.
fn strip_resources(doc: &Document, page: &Page, dict: &mut Dict) {
    let (mut resources, reference) = resources_of(doc, page, dict);
    let mut touched = false;
    for category in ["XObject", "ExtGState", "Font"] {
        let key = Name::new(category);
        let Some(sub) = resources.get(&key).and_then(|o| doc.resolve(o).ok()) else {
            continue;
        };
        let Some(sub) = sub.as_dict() else { continue };
        let mut sub = sub.clone();
        let before = sub.len();
        sub.retain(|name, _| !name.as_str().starts_with(PREFIX));
        if sub.len() != before {
            touched = true;
            resources.insert(key, Object::Dict(sub));
        }
    }
    if !touched {
        return;
    }
    if let Some(r) = reference {
        doc.set(r, Object::Dict(resources));
    } else {
        dict.insert(Name::new("Resources"), Object::Dict(resources));
    }
}

// --- Écriture des flux ------------------------------------------------------

/// Écrivain de tampons : garde les deux flux `q` et `Q` partagés par tout le
/// document, pour ne pas en créer une paire par page.
struct Writer<'a> {
    doc: &'a Document,
    wrap: Option<(ObjectRef, ObjectRef)>,
}

impl<'a> Writer<'a> {
    fn new(doc: &'a Document) -> Self {
        Writer { doc, wrap: None }
    }

    /// Flux `q` et `Q` qui encadrent le contenu d'origine.
    fn wrap(&mut self) -> (ObjectRef, ObjectRef) {
        *self.wrap.get_or_insert_with(|| {
            let open = self.doc.add(marked_stream(b"q\n".to_vec(), "Wrap"));
            let close = self.doc.add(marked_stream(b"Q\n".to_vec(), "Wrap"));
            (open, close)
        })
    }

    /// Ajoute un flux de contenu à la page, devant ou derrière l'existant.
    fn attach(
        &mut self,
        page: &Page,
        kind: StampKind,
        content: Vec<u8>,
        behind: bool,
    ) -> Result<()> {
        let page_ref = page
            .reference
            .ok_or_else(|| Error::Corrupt("la page doit être un objet indirect".into()))?;
        let stamp = self.doc.add(marked_stream(content, kind.tag()));
        let mut dict = current_dict(self.doc, page)?;
        let mut items = content_items(self.doc, &dict);
        if behind {
            items.insert(0, Object::Reference(stamp));
        } else {
            let (open, close) = self.wrap();
            items.insert(0, Object::Reference(open));
            items.push(Object::Reference(close));
            items.push(Object::Reference(stamp));
        }
        dict.insert(Name::new("Contents"), Object::Array(items));
        self.doc.set(page_ref, Object::Dict(dict));
        Ok(())
    }
}

/// Flux de contenu portant la clé privée `/AKStamp`.
fn marked_stream(raw: Vec<u8>, tag: &str) -> Object {
    let mut dict = Dict::new();
    dict.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
    );
    dict.insert(Name::new(MARK), Object::Name(Name::new(tag)));
    Object::Stream { dict, raw }
}

/// Éléments de `/Contents`, toujours sous forme de références indirectes.
fn content_items(doc: &Document, dict: &Dict) -> Vec<Object> {
    match dict.get(&Name::new("Contents")) {
        None | Some(Object::Null) => Vec::new(),
        Some(Object::Array(a)) => a.clone(),
        Some(Object::Reference(r)) => match doc.get(*r) {
            Ok(o) => match &*o {
                Object::Array(a) => a.clone(),
                _ => vec![Object::Reference(*r)],
            },
            Err(_) => vec![Object::Reference(*r)],
        },
        // Flux écrit directement dans la page (non conforme) : on le rend
        // indirect pour pouvoir le placer dans un tableau.
        Some(other) => vec![Object::Reference(doc.add(other.clone()))],
    }
}

/// Dictionnaire **propre** de la page, à jour (sans les attributs hérités).
fn current_dict(doc: &Document, page: &Page) -> Result<Dict> {
    match page.reference {
        Some(r) => Ok(doc
            .get(r)?
            .as_dict()
            .cloned()
            .unwrap_or_else(|| page.dict.clone())),
        None => Ok(page.dict.clone()),
    }
}

/// Ressources de la page et, le cas échéant, la référence de l'objet qui les
/// porte. Des ressources héritées sont recopiées (le nœud `/Pages` n'est pas
/// modifié).
fn resources_of(doc: &Document, page: &Page, dict: &Dict) -> (Dict, Option<ObjectRef>) {
    match dict.get(&Name::new("Resources")) {
        Some(Object::Reference(r)) => {
            let d = doc
                .get(*r)
                .ok()
                .and_then(|o| o.as_dict().cloned())
                .unwrap_or_default();
            (d, Some(*r))
        }
        Some(Object::Dict(d)) => (d.clone(), None),
        _ => {
            // Héritées : `page.dict` porte déjà la valeur fusionnée.
            let d = page
                .dict
                .get(&Name::new("Resources"))
                .and_then(|o| doc.resolve(o).ok())
                .and_then(|o| o.as_dict().cloned())
                .unwrap_or_default();
            (d, None)
        }
    }
}

/// Ajoute une ressource à la page et renvoie son nom. Une ressource déjà
/// présente sous le même objet est réutilisée.
fn add_resource(doc: &Document, page: &Page, category: &str, value: Object) -> Result<Name> {
    let page_ref = page
        .reference
        .ok_or_else(|| Error::Corrupt("la page doit être un objet indirect".into()))?;
    let mut dict = current_dict(doc, page)?;
    let (mut resources, reference) = resources_of(doc, page, &dict);
    let key = Name::new(category);
    let mut sub = resources
        .get(&key)
        .and_then(|o| doc.resolve(o).ok())
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_default();
    if let Some((name, _)) = sub
        .iter()
        .find(|(n, v)| *v == &value && n.as_str().starts_with(PREFIX))
    {
        return Ok(name.clone());
    }
    let mut index = 0;
    let name = loop {
        let candidate = Name::new(&format!("{PREFIX}{index}"));
        if !sub.contains_key(&candidate) {
            break candidate;
        }
        index += 1;
    };
    sub.insert(name.clone(), value);
    resources.insert(key, Object::Dict(sub));
    if let Some(r) = reference {
        doc.set(r, Object::Dict(resources));
    } else {
        dict.insert(Name::new("Resources"), Object::Dict(resources));
        doc.set(page_ref, Object::Dict(dict));
    }
    Ok(name)
}

/// Pages visées ; `indices` vide signifie « toutes ».
///
/// Les pages sont renvoyées dans l'ordre demandé, sans doublon : une page
/// citée deux fois ne reçoit pas deux tampons.
fn selected_pages(doc: &Document, indices: &[usize]) -> Result<Vec<Page>> {
    let all = collect_pages(doc)?;
    if indices.is_empty() {
        return Ok(all);
    }
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for &i in indices {
        if seen.contains(&i) {
            continue;
        }
        seen.push(i);
        out.push(
            all.get(i)
                .cloned()
                .ok_or_else(|| Error::Corrupt(format!("page {} absente", i + 1)))?,
        );
    }
    Ok(out)
}

// --- Géométrie --------------------------------------------------------------

/// Espace d'affichage de la page : largeur, hauteur (rotation comprise) et
/// matrice qui ramène cet espace à l'espace utilisateur du PDF.
///
/// L'origine est le coin inférieur gauche de la page **telle qu'elle
/// s'affiche**, ce qui rend le placement indépendant de `/Rotate`.
fn display_space(doc: &Document, page: &Page) -> (f64, f64, Matrix) {
    let crop = page.crop_box(doc);
    let (w, h) = (crop.width(), crop.height());
    match page.rotate(doc) {
        90 => (h, w, Matrix::new(0.0, 1.0, -1.0, 0.0, crop.x1, crop.y0)),
        180 => (w, h, Matrix::new(-1.0, 0.0, 0.0, -1.0, crop.x1, crop.y1)),
        270 => (h, w, Matrix::new(0.0, -1.0, 1.0, 0.0, crop.x0, crop.y1)),
        _ => (w, h, Matrix::new(1.0, 0.0, 0.0, 1.0, crop.x0, crop.y0)),
    }
}

/// Matrice qui place une boîte `box_w × box_h` (origine en bas à gauche) dans
/// une page de `page_w × page_h`, dans l'espace d'affichage.
fn placement_matrix(
    box_w: f64,
    box_h: f64,
    page_w: f64,
    page_h: f64,
    placement: Placement,
) -> Matrix {
    let theta = placement.rotation.to_radians();
    let (sin, cos) = theta.sin_cos();
    // Boîte englobante du dessin tourné, à l'échelle 1.
    let unit_w = (box_w * cos).abs() + (box_h * sin).abs();
    let unit_h = (box_w * sin).abs() + (box_h * cos).abs();
    let margin = placement.margin;
    let (free_w, free_h) = (page_w - 2.0 * margin, page_h - 2.0 * margin);
    let (sx, sy) = match placement.scale {
        Scale::Absolute(s) => (s, s),
        Scale::RelativeToPage(fraction) => {
            let s = if unit_w <= 0.0 || unit_h <= 0.0 {
                1.0
            } else {
                fraction * (free_w / unit_w).min(free_h / unit_h)
            };
            (s, s)
        }
        Scale::Stretch => (
            if box_w > 0.0 { free_w / box_w } else { 1.0 },
            if box_h > 0.0 { free_h / box_h } else { 1.0 },
        ),
    };
    // Boîte englobante réelle : mise à l'échelle puis rotation.
    let span_w = (box_w * sx * cos).abs() + (box_h * sy * sin).abs();
    let span_h = (box_w * sx * sin).abs() + (box_h * sy * cos).abs();
    let (fx, fy) = placement.anchor.factors();
    let cx = margin + span_w / 2.0 + fx * (free_w - span_w) + placement.offset.0;
    let cy = margin + span_h / 2.0 + fy * (free_h - span_h) + placement.offset.1;
    Matrix::translate(-box_w / 2.0, -box_h / 2.0)
        .then(&Matrix::scale(sx, sy))
        .then(&Matrix::rotate(theta))
        .then(&Matrix::translate(cx, cy))
}

/// Traduit une fraction d'ancrage (0, 0,5 ou 1) en indice de zone.
fn zone_index(factor: f64) -> u8 {
    if factor < 0.25 {
        0
    } else if factor > 0.75 {
        2
    } else {
        1
    }
}

/// Abscisse du début d'un texte selon sa zone horizontale
/// (0 = gauche, 1 = centre, 2 = droite).
fn horizontal_x(zone: u8, page_w: f64, text_w: f64, left: f64, right: f64) -> f64 {
    match zone {
        0 => left,
        1 => (page_w - text_w) / 2.0,
        _ => page_w - right - text_w,
    }
}

/// Ordonnée de la ligne de base selon la zone verticale (0 = bas, 1 = milieu,
/// 2 = haut). Le jambage du bas reste au-dessus de la marge, la hampe du haut
/// reste en dessous.
fn baseline_y(zone: u8, page_h: f64, font: &TextFont, size: f64, top: f64, bottom: f64) -> f64 {
    let (a, d) = font.vertical();
    let ascent = a * size / 1000.0;
    let descent = d * size / 1000.0;
    match zone {
        0 => bottom - descent,
        1 => (page_h - ascent - descent) / 2.0,
        _ => page_h - top - ascent,
    }
}

/// Écrit une matrice avec assez de décimales pour que le placement soit
/// stable au centième de point sur une page A0.
fn matrix_text(m: &Matrix) -> String {
    format!(
        "{} {} {} {} {} {}",
        num(m.a),
        num(m.b),
        num(m.c),
        num(m.d),
        num(m.e),
        num(m.f)
    )
}

fn num(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let s = format!("{v:.5}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" {
        "0".into()
    } else {
        s.to_string()
    }
}

// --- Dessin -----------------------------------------------------------------

/// Formulaire prêt à être posé.
struct BuiltForm {
    reference: ObjectRef,
    width: f64,
    height: f64,
}

/// Construit l'XObject de formulaire d'un filigrane ou d'un arrière-plan.
#[allow(clippy::too_many_lines)] // une branche par source : texte, image, aplat
fn build_form(
    doc: &Document,
    source: &StampSource,
    warnings: &mut Vec<String>,
) -> Result<BuiltForm> {
    let (width, height, content, resources) = match source {
        StampSource::Fill { color } => (
            1.0,
            1.0,
            format!(
                "{} {} {} rg 0 0 1 1 re f\n",
                num(color[0]),
                num(color[1]),
                num(color[2])
            )
            .into_bytes(),
            Dict::new(),
        ),
        StampSource::Image { data } => {
            let decoded = image::decode(data)?;
            let (w, h) = (f64::from(decoded.width), f64::from(decoded.height));
            let reference = decoded.write(doc);
            let name = Name::new(&format!("{PREFIX}0"));
            let mut xobjects = Dict::new();
            xobjects.insert(name.clone(), Object::Reference(reference));
            let mut resources = Dict::new();
            resources.insert(Name::new("XObject"), Object::Dict(xobjects));
            (
                w,
                h,
                format!("{} 0 0 {} 0 0 cm /{} Do\n", num(w), num(h), name.as_str()).into_bytes(),
                resources,
            )
        }
        StampSource::Text {
            text,
            font,
            size,
            color,
        } => {
            let size = size.max(0.01);
            let (text_font, reference) = TextFont::resolve(doc, text, *font, warnings);
            let name = Name::new(&format!("{PREFIX}0"));
            let lines: Vec<&str> = text.split('\n').collect();
            let leading = size * 1.16;
            let (va, vd) = text_font.vertical();
            let ascent = va * size / 1000.0;
            let descent = vd * size / 1000.0;
            let width = lines
                .iter()
                .map(|l| text_font.width(l, size))
                .fold(0.0_f64, f64::max)
                .max(0.01);
            #[allow(clippy::cast_precision_loss)] // au plus quelques dizaines de lignes
            let height = (lines.len() - 1) as f64 * leading + ascent - descent;
            let placed: Vec<(f64, f64, String)> = lines
                .iter()
                .enumerate()
                .map(|(i, line)| {
                    #[allow(clippy::cast_precision_loss)]
                    let y = height - ascent - i as f64 * leading;
                    let x = (width - text_font.width(line, size)) / 2.0;
                    (x, y, (*line).to_string())
                })
                .collect();
            let style = TextStyle {
                font_name: &name,
                font: &text_font,
                size,
                color: *color,
            };
            let content = text_block(&Matrix::IDENTITY, &style, &placed, warnings);
            let mut fonts = Dict::new();
            fonts.insert(name, Object::Reference(reference));
            let mut resources = Dict::new();
            resources.insert(Name::new("Font"), Object::Dict(fonts));
            (width, height, content, resources)
        }
    };
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
    dict.insert(Name::new("Resources"), Object::Dict(resources));
    // Groupe de transparence : l'opacité s'applique au dessin entier et non à
    // chaque trait, sinon les recouvrements se renforcent.
    let mut group = Dict::new();
    group.insert(Name::new("S"), Object::Name(Name::new("Transparency")));
    group.insert(Name::new("CS"), Object::Name(Name::new("DeviceRGB")));
    dict.insert(Name::new("Group"), Object::Dict(group));
    dict.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(content.len()).unwrap_or(0)),
    );
    let reference = doc.add(Object::Stream { dict, raw: content });
    Ok(BuiltForm {
        reference,
        width,
        height,
    })
}

/// Police effective d'un bloc de texte : une des quatorze polices standard
/// tant que le texte tient en WinAnsiEncoding, sinon une police système
/// incorporée en sous-ensemble. C'est ce qui permet de tamponner « 機密 » ou
/// « Конфиденциально » et pas seulement du latin.
enum TextFont {
    /// Police standard, texte écrit en littéral WinAnsi.
    Standard(StandardFont),
    /// Police incorporée, texte écrit en indices de glyphes hexadécimaux.
    Embedded(Box<crate::fontembed::EmbeddedFont>),
}

impl TextFont {
    /// Choisit la police et crée son objet. Le texte donné est l'ensemble de
    /// ce qui sera écrit avec elle : c'est lui qui décide.
    fn resolve(
        doc: &Document,
        text: &str,
        font: StandardFont,
        warnings: &mut Vec<String>,
    ) -> (Self, ObjectRef) {
        if crate::fontembed::fits_winansi(text) {
            let reference = font_object(doc, font);
            finish_font(doc, reference);
            return (TextFont::Standard(font), reference);
        }
        let base = font.base_font();
        let bold = base.contains("Bold");
        let italic = base.contains("Italic") || base.contains("Oblique");
        let style = match (bold, italic) {
            (true, true) => crate::fontembed::FontStyle::BoldItalic,
            (true, false) => crate::fontembed::FontStyle::Bold,
            (false, true) => crate::fontembed::FontStyle::Italic,
            (false, false) => crate::fontembed::FontStyle::Regular,
        };
        match crate::fontembed::embed_text_font(doc, text, style) {
            Ok(embedded) => {
                let reference = embedded.reference;
                if !embedded.covers(text) {
                    warnings.push(format!(
                        "la police système « {} » ne couvre pas tout le texte : les caractères manquants sont dessinés vides",
                        embedded.family
                    ));
                }
                (TextFont::Embedded(Box::new(embedded)), reference)
            }
            Err(e) => {
                warnings.push(format!(
                    "aucune police incorporable pour ce texte ({e}) : les caractères hors WinAnsiEncoding deviennent « ? »"
                ));
                let reference = font_object(doc, font);
                finish_font(doc, reference);
                (TextFont::Standard(font), reference)
            }
        }
    }

    /// Ascendante et descendante en millièmes d'em.
    fn vertical(&self) -> (f64, f64) {
        match self {
            TextFont::Standard(f) => (f.ascent(), f.descent()),
            TextFont::Embedded(f) => (f.ascent, f.descent),
        }
    }

    /// Largeur du texte à la taille donnée, en points.
    fn width(&self, text: &str, size: f64) -> f64 {
        match self {
            TextFont::Standard(f) => f.text_width(text, size),
            TextFont::Embedded(f) => f.width(text, size),
        }
    }

    /// Opérande prête à écrire devant `Tj`.
    fn show(&self, text: &str, replaced: &mut Vec<char>) -> String {
        match self {
            TextFont::Standard(_) => {
                let encoded = metrics::encode_win_ansi(text);
                replaced.extend(encoded.replaced);
                metrics::pdf_literal(&encoded.bytes)
            }
            TextFont::Embedded(f) => format!("<{}>", f.show(text)),
        }
    }
}

/// Aspect d'un bloc de texte posé sur une page.
struct TextStyle<'a> {
    /// Nom de la ressource police, dans les ressources qui portent le flux.
    font_name: &'a Name,
    /// Police effective.
    font: &'a TextFont,
    /// Corps en points.
    size: f64,
    /// Couleur de remplissage.
    color: Rgb,
}

/// Bloc de texte : une matrice d'entrée, puis une ligne par position.
fn text_block(
    to_user: &Matrix,
    style: &TextStyle,
    lines: &[(f64, f64, String)],
    warnings: &mut Vec<String>,
) -> Vec<u8> {
    let mut out = String::from("q\n");
    if *to_user != Matrix::IDENTITY {
        let _ = writeln!(out, "{} cm", matrix_text(to_user));
    }
    let _ = writeln!(
        out,
        "BT\n/{} {} Tf\n{} {} {} rg",
        style.font_name.as_str(),
        num(style.size),
        num(style.color[0]),
        num(style.color[1]),
        num(style.color[2])
    );
    let mut replaced = Vec::new();
    for (x, y, text) in lines {
        let shown = style.font.show(text, &mut replaced);
        let _ = writeln!(out, "1 0 0 1 {} {} Tm {shown} Tj", num(*x), num(*y));
    }
    out.push_str("ET\nQ\n");
    replaced.sort_unstable();
    replaced.dedup();
    if !replaced.is_empty() {
        let list: String = replaced.iter().collect();
        warnings.push(format!(
            "caractères hors de WinAnsiEncoding remplacés par « ? » : {list}"
        ));
    }
    out.into_bytes()
}

/// Dictionnaire d'état graphique étendu portant l'opacité.
fn extended_state(doc: &Document, opacity: Opacity) -> ObjectRef {
    let mut dict = Dict::new();
    dict.insert(Name::new("Type"), Object::Name(Name::new("ExtGState")));
    dict.insert(Name::new("BM"), Object::Name(Name::new("Normal")));
    dict.insert(Name::new("ca"), Object::Real(opacity.fill.clamp(0.0, 1.0)));
    dict.insert(
        Name::new("CA"),
        Object::Real(opacity.stroke.clamp(0.0, 1.0)),
    );
    doc.add(Object::Dict(dict))
}

// --- Polices ----------------------------------------------------------------

/// Crée (ou retrouve) l'objet police d'une police standard.
///
/// Le `/ToUnicode` est posé par [`finish_font`] : pour une police standard en
/// WinAnsiEncoding il fait double emploi avec l'encodage, mais il garantit
/// l'extraction du texte par un lecteur qui ne connaîtrait pas les tables de
/// l'annexe D.
fn font_object(doc: &Document, font: StandardFont) -> ObjectRef {
    let mut dict = Dict::new();
    dict.insert(Name::new("Type"), Object::Name(Name::new("Font")));
    dict.insert(Name::new("Subtype"), Object::Name(Name::new("Type1")));
    dict.insert(
        Name::new("BaseFont"),
        Object::Name(Name::new(font.base_font())),
    );
    dict.insert(
        Name::new("Encoding"),
        Object::Name(Name::new("WinAnsiEncoding")),
    );
    doc.add(Object::Dict(dict))
}

/// Ajoute à une police posée le `/ToUnicode` couvrant tout WinAnsiEncoding.
fn finish_font(doc: &Document, reference: ObjectRef) {
    let Ok(existing) = doc.get(reference) else {
        return;
    };
    let Some(dict) = existing.as_dict() else {
        return;
    };
    if dict.contains_key(&Name::new("ToUnicode")) {
        return;
    }
    let mut pairs = Vec::new();
    for code in 32..=255u8 {
        let Some(name) = acrux_fonts::encodings::win_ansi(code) else {
            continue;
        };
        if let Some(c) = acrux_fonts::encodings::glyph_name_to_unicode(name) {
            pairs.push((code, c));
        }
    }
    let raw = to_unicode_cmap(&pairs);
    let mut stream_dict = Dict::new();
    stream_dict.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
    );
    let cmap = doc.add(Object::Stream {
        dict: stream_dict,
        raw,
    });
    let mut dict = dict.clone();
    dict.insert(Name::new("ToUnicode"), Object::Reference(cmap));
    doc.set(reference, Object::Dict(dict));
}

/// CMap `ToUnicode` (§9.10.3) pour une police simple à un octet.
fn to_unicode_cmap(pairs: &[(u8, char)]) -> Vec<u8> {
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo\n<< /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<00> <FF>\nendcodespacerange\n",
    );
    for chunk in pairs.chunks(100) {
        let _ = writeln!(out, "{} beginbfchar", chunk.len());
        for (code, c) in chunk {
            let mut dst = String::new();
            let mut buffer = [0u16; 2];
            for unit in c.encode_utf16(&mut buffer) {
                let _ = write!(dst, "{unit:04X}");
            }
            let _ = writeln!(out, "<{code:02X}> <{dst}>");
        }
        out.push_str("endbfchar\n");
    }
    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out.into_bytes()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use acrux_core::{Point, Rect};
    use acrux_render::{render_page, RenderOptions};

    /// Document de travail : `pages` pages de 400 × 300 avec une ligne de
    /// texte Helvetica chacune, plus, si `rotate` n'est pas nul, un `/Rotate`
    /// sur chaque page.
    fn sample(pages: usize, rotate: i32) -> Document {
        let mut out = String::from("%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n");
        let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 3 + i * 2)).collect();
        let _ = writeln!(
            out,
            "2 0 obj << /Type /Pages /Kids [{}] /Count {pages} >> endobj",
            kids.join(" ")
        );
        let rotation = if rotate == 0 {
            String::new()
        } else {
            format!(" /Rotate {rotate}")
        };
        for i in 0..pages {
            let page = 3 + i * 2;
            let content = format!("BT /F1 12 Tf 20 150 Td (Origine {}) Tj ET\n", i + 1);
            let _ = writeln!(
                out,
                "{page} 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300]{rotation} \
                 /Contents {} 0 R /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 \
                 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >> >> >> >> endobj",
                page + 1
            );
            let _ = writeln!(
                out,
                "{} 0 obj << /Length {} >>\nstream\n{content}endstream\nendobj",
                page + 1,
                content.len()
            );
        }
        Document::from_bytes(out.into_bytes()).unwrap()
    }

    /// Page sans `/Contents` du tout.
    fn blank() -> Document {
        Document::from_bytes(
            b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
              2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 200] >> endobj\n\
              3 0 obj << /Type /Page /Parent 2 0 R >> endobj\n"
                .to_vec(),
        )
        .unwrap()
    }

    /// Relit le document après enregistrement, comme le ferait un lecteur.
    /// Les documents de test sont écrits sans table de références croisées :
    /// ils sont réparés à l'ouverture, donc réenregistrés en entier.
    fn reload(doc: &Document) -> Document {
        Document::from_bytes(doc.save_full().unwrap()).unwrap()
    }

    /// Concaténation des flux de contenu d'une page.
    fn content_of(doc: &Document, index: usize) -> String {
        let pages = collect_pages(doc).unwrap();
        String::from_utf8_lossy(&acrux_render::page::page_content(doc, &pages[index])).into_owned()
    }

    /// Boîte occupée par une boîte `w × h` placée dans une page `pw × ph`.
    fn placed(placement: Placement, w: f64, h: f64, pw: f64, ph: f64) -> Rect {
        placement_matrix(w, h, pw, ph, placement).transform_rect(&Rect::new(0.0, 0.0, w, h))
    }

    fn at(anchor: Anchor, margin: f64) -> Placement {
        Placement {
            anchor,
            margin,
            ..Placement::default()
        }
    }

    #[test]
    fn nine_anchors_place_the_box_in_their_corner() {
        let (w, h, pw, ph) = (100.0, 40.0, 600.0, 400.0);
        let m = 10.0;
        let cases = [
            (Anchor::BottomLeft, 10.0, 10.0),
            (Anchor::BottomCenter, 250.0, 10.0),
            (Anchor::BottomRight, 490.0, 10.0),
            (Anchor::MiddleLeft, 10.0, 180.0),
            (Anchor::Center, 250.0, 180.0),
            (Anchor::MiddleRight, 490.0, 180.0),
            (Anchor::TopLeft, 10.0, 350.0),
            (Anchor::TopCenter, 250.0, 350.0),
            (Anchor::TopRight, 490.0, 350.0),
        ];
        for (anchor, x0, y0) in cases {
            let r = placed(at(anchor, m), w, h, pw, ph);
            assert!(
                (r.x0 - x0).abs() < 1e-9 && (r.y0 - y0).abs() < 1e-9,
                "{anchor:?} : {r:?}, attendu ({x0}, {y0})"
            );
            assert!((r.width() - w).abs() < 1e-9 && (r.height() - h).abs() < 1e-9);
            // La marge est respectée sur les quatre bords.
            assert!(r.x0 >= m - 1e-9 && r.y0 >= m - 1e-9);
            assert!(r.x1 <= pw - m + 1e-9 && r.y1 <= ph - m + 1e-9);
        }
    }

    #[test]
    fn offset_moves_the_box_after_anchoring() {
        let base = placed(at(Anchor::BottomLeft, 0.0), 100.0, 40.0, 600.0, 400.0);
        let moved = placed(
            Placement {
                offset: (25.0, -7.5),
                ..at(Anchor::BottomLeft, 0.0)
            },
            100.0,
            40.0,
            600.0,
            400.0,
        );
        assert!((moved.x0 - base.x0 - 25.0).abs() < 1e-9);
        assert!((moved.y0 - base.y0 + 7.5).abs() < 1e-9);
    }

    #[test]
    fn rotation_turns_the_box_around_its_centre() {
        let p = Placement {
            rotation: 90.0,
            ..Placement::default()
        };
        let r = placed(p, 100.0, 40.0, 600.0, 400.0);
        // À 90°, la boîte 100 × 40 occupe 40 × 100, toujours centrée.
        assert!((r.width() - 40.0).abs() < 1e-6, "{r:?}");
        assert!((r.height() - 100.0).abs() < 1e-6, "{r:?}");
        assert!((r.x0 + r.x1 - 600.0).abs() < 1e-6);
        assert!((r.y0 + r.y1 - 400.0).abs() < 1e-6);
        // À 45°, la boîte englobante grandit dans les deux sens.
        let d = placed(
            Placement {
                rotation: 45.0,
                ..Placement::default()
            },
            100.0,
            40.0,
            600.0,
            400.0,
        );
        let expected = (100.0 + 40.0) * std::f64::consts::FRAC_1_SQRT_2;
        assert!((d.width() - expected).abs() < 1e-6, "{d:?}");
        assert!((d.height() - expected).abs() < 1e-6, "{d:?}");
        // 360° revient au point de départ.
        let full = placed(
            Placement {
                rotation: 360.0,
                ..Placement::default()
            },
            100.0,
            40.0,
            600.0,
            400.0,
        );
        let none = placed(Placement::default(), 100.0, 40.0, 600.0, 400.0);
        assert!((full.x0 - none.x0).abs() < 1e-6 && (full.y0 - none.y0).abs() < 1e-6);
    }

    #[test]
    fn relative_scale_fits_the_page() {
        let p = Placement {
            scale: Scale::RelativeToPage(1.0),
            ..Placement::default()
        };
        // Boîte 100 × 40 dans 600 × 400 : c'est la largeur qui limite (×6).
        let r = placed(p, 100.0, 40.0, 600.0, 400.0);
        assert!((r.width() - 600.0).abs() < 1e-6, "{r:?}");
        assert!((r.height() - 240.0).abs() < 1e-6, "{r:?}");
        // La moitié de la page, marges comprises.
        let half = placed(
            Placement {
                scale: Scale::RelativeToPage(0.5),
                margin: 50.0,
                ..Placement::default()
            },
            100.0,
            40.0,
            600.0,
            400.0,
        );
        assert!((half.width() - 250.0).abs() < 1e-6, "{half:?}");
        // Une échelle relative sur un dessin tourné tient aussi dans la page.
        let turned = placed(
            Placement {
                rotation: 45.0,
                scale: Scale::RelativeToPage(1.0),
                ..Placement::default()
            },
            100.0,
            40.0,
            600.0,
            400.0,
        );
        assert!(
            turned.x0 >= -1e-6 && turned.x1 <= 600.0 + 1e-6,
            "{turned:?}"
        );
        assert!(
            turned.y0 >= -1e-6 && turned.y1 <= 400.0 + 1e-6,
            "{turned:?}"
        );
    }

    /// L'espace d'affichage doit coïncider avec celui du moteur de rendu :
    /// le point (0, 0) d'affichage tombe en bas à gauche de l'image, quelle
    /// que soit la rotation de la page.
    #[test]
    fn display_space_matches_the_renderer() {
        for rotate in [0, 90, 180, 270] {
            let doc = sample(1, rotate);
            let pages = collect_pages(&doc).unwrap();
            let (w, h, to_user) = display_space(&doc, &pages[0]);
            let rendered = render_page(&doc, &pages[0], 1.0, &RenderOptions::default());
            assert!(
                (w - f64::from(rendered.bitmap.width())).abs() < 1.0
                    && (h - f64::from(rendered.bitmap.height())).abs() < 1.0,
                "rotation {rotate} : {w}×{h} contre {}×{}",
                rendered.bitmap.width(),
                rendered.bitmap.height()
            );
            let full = to_user.then(&rendered.base_ctm);
            // Bas gauche de l'affichage → bas gauche de l'image.
            let origin = full.apply(Point::new(0.0, 0.0));
            assert!(
                origin.x.abs() < 1e-6 && (origin.y - h).abs() < 1e-6,
                "{origin:?} (rotation {rotate})"
            );
            // Haut droit de l'affichage → haut droit de l'image.
            let corner = full.apply(Point::new(w, h));
            assert!(
                (corner.x - w).abs() < 1e-6 && corner.y.abs() < 1e-6,
                "{corner:?} (rotation {rotate})"
            );
        }
    }

    #[test]
    fn watermark_uses_one_shared_form_for_every_page() {
        let doc = sample(3, 0);
        let report = add_watermark(&doc, &WatermarkOptions::default()).unwrap();
        assert_eq!(report.pages, 3);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let doc = reload(&doc);
        // Un seul XObject de formulaire dans tout le document.
        let forms: Vec<u32> = doc
            .object_numbers()
            .into_iter()
            .filter(|n| {
                let r = ObjectRef {
                    number: *n,
                    generation: 0,
                };
                doc.get(r)
                    .ok()
                    .and_then(|o| o.as_dict().cloned())
                    .is_some_and(|d| {
                        d.get(&Name::new("Subtype")) == Some(&Object::Name(Name::new("Form")))
                    })
            })
            .collect();
        assert_eq!(forms.len(), 1, "un seul formulaire attendu : {forms:?}");
        // Chaque page le dessine et garde son texte d'origine.
        for i in 0..3 {
            let content = content_of(&doc, i);
            assert!(content.contains(" Do"), "page {i} : {content}");
            assert!(
                content.contains(&format!("(Origine {})", i + 1)),
                "page {i}"
            );
        }
    }

    #[test]
    fn original_content_is_never_rewritten() {
        let source = sample(1, 0);
        let before = content_of(&source, 0);
        add_watermark(&source, &WatermarkOptions::default()).unwrap();
        let after = content_of(&source, 0);
        // Le contenu d'origine est toujours là, octet pour octet, encadré.
        assert!(after.contains(before.trim_end()), "{after}");
        // Et il est bien encadré par q / Q, le tampon venant après.
        let q = after.find("q\n").unwrap();
        let origin = after.find("(Origine 1)").unwrap();
        let stamp = after.rfind(" Do").unwrap();
        assert!(q < origin && origin < stamp, "ordre inattendu : {after}");
    }

    #[test]
    fn behind_puts_the_stamp_first() {
        let doc = sample(1, 0);
        add_watermark(
            &doc,
            &WatermarkOptions {
                behind: true,
                ..WatermarkOptions::default()
            },
        )
        .unwrap();
        let content = content_of(&doc, 0);
        assert!(
            content.find(" Do").unwrap() < content.find("(Origine 1)").unwrap(),
            "{content}"
        );
    }

    #[test]
    fn opacity_writes_an_extended_graphics_state() {
        let doc = sample(1, 0);
        add_watermark(
            &doc,
            &WatermarkOptions {
                opacity: Opacity {
                    fill: 0.3,
                    stroke: 0.4,
                },
                ..WatermarkOptions::default()
            },
        )
        .unwrap();
        assert!(content_of(&doc, 0).contains(" gs"), "pas de `gs`");
        let doc = reload(&doc);
        let states: Vec<Dict> = doc
            .object_numbers()
            .into_iter()
            .filter_map(|n| {
                let d = doc
                    .get(ObjectRef {
                        number: n,
                        generation: 0,
                    })
                    .ok()?
                    .as_dict()
                    .cloned()?;
                (d.get(&Name::new("Type")) == Some(&Object::Name(Name::new("ExtGState"))))
                    .then_some(d)
            })
            .collect();
        assert_eq!(states.len(), 1);
        let value = |key: &str| states[0].get(&Name::new(key)).and_then(Object::as_f64);
        assert!(
            (value("ca").unwrap() - 0.3).abs() < 1e-9,
            "{:?}",
            value("ca")
        );
        assert!(
            (value("CA").unwrap() - 0.4).abs() < 1e-9,
            "{:?}",
            value("CA")
        );
        // Opacité pleine : aucun ExtGState, donc aucun `gs`.
        let plain = sample(1, 0);
        add_watermark(
            &plain,
            &WatermarkOptions {
                opacity: Opacity::default(),
                ..WatermarkOptions::default()
            },
        )
        .unwrap();
        assert!(!content_of(&plain, 0).contains(" gs"));
    }

    #[test]
    fn page_ranges_are_honoured() {
        let doc = sample(4, 0);
        let report = add_watermark(
            &doc,
            &WatermarkOptions {
                pages: vec![0, 2, 2],
                ..WatermarkOptions::default()
            },
        )
        .unwrap();
        // La page 3 citée deux fois ne reçoit qu'un tampon.
        assert_eq!(report.pages, 2);
        for (index, expected) in [(0, true), (1, false), (2, true), (3, false)] {
            assert_eq!(
                content_of(&doc, index).contains(" Do"),
                expected,
                "page {}",
                index + 1
            );
        }
        // Page hors document : erreur claire.
        assert!(add_watermark(
            &doc,
            &WatermarkOptions {
                pages: vec![9],
                ..WatermarkOptions::default()
            }
        )
        .is_err());
    }

    #[test]
    fn single_page_and_empty_page_are_handled() {
        // Document d'une seule page.
        let one = sample(1, 0);
        assert_eq!(
            add_watermark(&one, &WatermarkOptions::default())
                .unwrap()
                .pages,
            1
        );
        // Page sans `/Contents` : le tampon devient le seul contenu.
        let doc = blank();
        let report = add_watermark(&doc, &WatermarkOptions::default()).unwrap();
        assert_eq!(report.pages, 1);
        let doc = reload(&doc);
        let content = content_of(&doc, 0);
        assert!(content.contains(" Do"), "{content}");
        let pages = collect_pages(&doc).unwrap();
        let rendered = render_page(&doc, &pages[0], 1.0, &RenderOptions::default());
        assert_eq!(rendered.bitmap.width(), 200);
    }

    /// De l'encre dans la moitié demandée de la page rendue.
    fn ink_ratio(doc: &Document, index: usize, zone: Rect) -> f64 {
        let pages = collect_pages(doc).unwrap();
        let rendered = render_page(
            doc,
            &pages[index],
            1.0,
            &RenderOptions {
                background: Some(acrux_graphics::Color::WHITE),
                ..RenderOptions::default()
            },
        );
        let mut ink = 0u32;
        let mut total = 0u32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        for y in (zone.y0 as u32)..(zone.y1 as u32).min(rendered.bitmap.height()) {
            for x in (zone.x0 as u32)..(zone.x1 as u32).min(rendered.bitmap.width()) {
                total += 1;
                if rendered
                    .bitmap
                    .pixel(x, y)
                    .is_some_and(|p| p[..3].iter().any(|c| *c < 250))
                {
                    ink += 1;
                }
            }
        }
        f64::from(ink) / f64::from(total.max(1))
    }

    #[test]
    fn a_watermark_is_actually_drawn_and_its_text_extractable() {
        let doc = sample(1, 0);
        add_watermark(
            &doc,
            &WatermarkOptions {
                source: StampSource::Text {
                    text: "CONFIDENTIEL".into(),
                    font: StandardFont::HelveticaBold,
                    size: 40.0,
                    color: [1.0, 0.0, 0.0],
                },
                placement: Placement {
                    rotation: 30.0,
                    scale: Scale::RelativeToPage(0.9),
                    ..Placement::default()
                },
                opacity: Opacity {
                    fill: 0.3,
                    stroke: 0.3,
                },
                behind: false,
                pages: Vec::new(),
            },
        )
        .unwrap();
        let doc = reload(&doc);
        // Le texte du filigrane est extrait, et celui d'origine est intact.
        let pages = collect_pages(&doc).unwrap();
        let text = crate::text::extract_page_text(&doc, &pages[0])
            .unwrap()
            .to_plain();
        assert!(text.contains("CONFIDENTIEL"), "{text}");
        assert!(text.contains("Origine 1"), "{text}");
        // Et il y a de l'encre au centre de la page.
        assert!(
            ink_ratio(&doc, 0, Rect::new(150.0, 120.0, 250.0, 180.0)) > 0.05,
            "filigrane invisible"
        );
    }

    #[test]
    fn opacity_lightens_the_drawing() {
        let opaque = sample(1, 0);
        let faint = sample(1, 0);
        let options = |fill: f64| WatermarkOptions {
            source: StampSource::Fill {
                color: [0.0, 0.0, 0.0],
            },
            placement: Placement {
                scale: Scale::RelativeToPage(1.0),
                ..Placement::default()
            },
            opacity: Opacity { fill, stroke: fill },
            behind: false,
            pages: Vec::new(),
        };
        add_watermark(&opaque, &options(1.0)).unwrap();
        add_watermark(&faint, &options(0.25)).unwrap();
        let pixel = |doc: &Document| {
            let pages = collect_pages(doc).unwrap();
            let rendered = render_page(
                doc,
                &pages[0],
                1.0,
                &RenderOptions {
                    background: Some(acrux_graphics::Color::WHITE),
                    ..RenderOptions::default()
                },
            );
            u32::from(rendered.bitmap.pixel(200, 150).unwrap()[0])
        };
        assert!(pixel(&opaque) < 10, "aplat opaque : {}", pixel(&opaque));
        // 25 % de noir sur blanc : environ 191.
        let value = pixel(&faint);
        assert!((186..=196).contains(&value), "aplat à 25 % : {value}");
    }

    #[test]
    fn background_is_always_behind_and_fills_the_page() {
        let doc = sample(1, 0);
        add_background(&doc, &BackgroundOptions::default()).unwrap();
        let content = content_of(&doc, 0);
        assert!(
            content.find(" Do").unwrap() < content.find("(Origine 1)").unwrap(),
            "{content}"
        );
        // La couleur couvre les coins de la page.
        let doc = reload(&doc);
        let pages = collect_pages(&doc).unwrap();
        let rendered = render_page(&doc, &pages[0], 1.0, &RenderOptions::default());
        for (x, y) in [(1, 1), (398, 1), (1, 298), (398, 298)] {
            let p = rendered.bitmap.pixel(x, y).unwrap();
            assert!(p[2] < p[0], "coin ({x}, {y}) non couvert : {p:?}");
        }
    }

    #[test]
    fn header_and_footer_tokens_are_replaced_and_aligned() {
        let doc = sample(3, 0);
        let report = add_header_footer(
            &doc,
            &HeaderFooterOptions {
                header: ["{filename}".into(), "Rapport".into(), "{date}".into()],
                footer: [
                    "gauche".into(),
                    "{page} / {pages}".into(),
                    "{inconnu}".into(),
                ],
                margins: Margins {
                    top: 20.0,
                    bottom: 20.0,
                    left: 30.0,
                    right: 30.0,
                },
                start_number: 1,
                format: NumberFormat::Decimal,
                file_name: "essai.pdf".into(),
                ..HeaderFooterOptions::default()
            },
        )
        .unwrap();
        assert_eq!(report.pages, 3);
        assert!(
            report.warnings.iter().any(|w| w.contains("{inconnu}")),
            "{:?}",
            report.warnings
        );
        let doc = reload(&doc);
        let pages = collect_pages(&doc).unwrap();
        for (index, page) in pages.iter().enumerate() {
            let text = crate::text::extract_page_text(&doc, page).unwrap();
            let plain = text.to_plain();
            assert!(plain.contains("essai.pdf"), "page {index} : {plain}");
            assert!(
                plain.contains(&format!("{} / 3", index + 1)),
                "page {index} : {plain}"
            );
            // Alignement : « essai.pdf » à gauche, la date à droite, le tout
            // en haut ; le numéro de page centré en bas.
            let word = |needle: &str| {
                text.words()
                    .into_iter()
                    .find(|w| w.text.contains(needle))
                    .unwrap_or_else(|| panic!("mot « {needle} » absent de : {plain}"))
                    .bbox
            };
            let line = |needle: &str| {
                text.lines
                    .iter()
                    .find(|l| l.text().contains(needle))
                    .unwrap_or_else(|| panic!("ligne « {needle} » absente de : {plain}"))
                    .bbox
            };
            let left = word("essai.pdf");
            let right = word("-"); // la date, « 2026-09-19 »
            assert!(left.x0 < right.x0, "{left:?} / {right:?}");
            assert!((left.x0 - 30.0).abs() < 1.0, "marge gauche : {left:?}");
            assert!((right.x1 - 370.0).abs() < 1.5, "marge droite : {right:?}");
            assert!(left.y0 > 250.0, "en-tête trop bas : {left:?}");
            let centre = line("/ 3");
            assert!(centre.y1 < 40.0, "pied trop haut : {centre:?}");
            assert!(
                (f64::midpoint(centre.x0, centre.x1) - 200.0).abs() < 2.0,
                "pied mal centré : {centre:?}"
            );
        }
    }

    #[test]
    fn page_numbering_offset_and_format() {
        let doc = sample(3, 0);
        add_header_footer(
            &doc,
            &HeaderFooterOptions {
                footer: [String::new(), "- {page} -".into(), String::new()],
                start_number: 4,
                format: NumberFormat::LowerRoman,
                ..HeaderFooterOptions::default()
            },
        )
        .unwrap();
        let doc = reload(&doc);
        let pages = collect_pages(&doc).unwrap();
        let plain: Vec<String> = pages
            .iter()
            .map(|p| crate::text::extract_page_text(&doc, p).unwrap().to_plain())
            .collect();
        assert!(plain[0].contains("- iv -"), "{}", plain[0]);
        assert!(plain[1].contains("- v -"), "{}", plain[1]);
        assert!(plain[2].contains("- vi -"), "{}", plain[2]);
    }

    #[test]
    fn empty_zones_write_nothing() {
        let doc = sample(1, 0);
        let report = add_header_footer(
            &doc,
            &HeaderFooterOptions {
                header: [String::new(), String::new(), String::new()],
                footer: [String::new(), String::new(), String::new()],
                ..HeaderFooterOptions::default()
            },
        )
        .unwrap();
        assert_eq!(report.pages, 0);
        assert_eq!(content_of(&doc, 0), content_of(&sample(1, 0), 0));
    }

    #[test]
    fn bates_numbering_is_continuous_across_documents() {
        let first = sample(2, 0);
        let second = sample(3, 0);
        let options = BatesOptions {
            prefix: "ACME-".into(),
            suffix: "-P".into(),
            digits: 5,
            start: 98,
            ..BatesOptions::default()
        };
        let reports = add_bates_sequence(&[&first, &second], &options).unwrap();
        assert_eq!(reports[0].first, Some(98));
        assert_eq!(reports[0].last, Some(99));
        assert_eq!(reports[1].first, Some(100));
        assert_eq!(reports[1].last, Some(102));
        assert_eq!(reports[1].next, 103);
        let labels = |doc: &Document| -> Vec<String> {
            let doc = reload(doc);
            collect_pages(&doc)
                .unwrap()
                .iter()
                .map(|p| crate::text::extract_page_text(&doc, p).unwrap().to_plain())
                .collect()
        };
        let a = labels(&first);
        assert!(a[0].contains("ACME-00098-P"), "{}", a[0]);
        assert!(a[1].contains("ACME-00099-P"), "{}", a[1]);
        let b = labels(&second);
        assert!(b[0].contains("ACME-00100-P"), "{}", b[0]);
        assert!(b[2].contains("ACME-00102-P"), "{}", b[2]);
    }

    #[test]
    fn bates_anchor_places_the_number() {
        let doc = sample(1, 0);
        add_bates(
            &doc,
            &BatesOptions {
                anchor: Anchor::BottomRight,
                margin_x: 20.0,
                margin_y: 15.0,
                digits: 3,
                start: 7,
                ..BatesOptions::default()
            },
        )
        .unwrap();
        let doc = reload(&doc);
        let pages = collect_pages(&doc).unwrap();
        let text = crate::text::extract_page_text(&doc, &pages[0]).unwrap();
        let word = text
            .words()
            .into_iter()
            .find(|w| w.text.contains("007"))
            .expect("numéro Bates extrait");
        assert!((word.bbox.x1 - 380.0).abs() < 1.0, "{:?}", word.bbox);
        assert!(
            word.bbox.y0 > 12.0 && word.bbox.y0 < 20.0,
            "{:?}",
            word.bbox
        );
    }

    /// Un tampon posé puis retiré rend exactement les mêmes pixels
    /// qu'auparavant.
    #[test]
    fn stamps_can_be_removed_without_trace() {
        let original = sample(2, 0);
        let doc = sample(2, 0);
        add_watermark(&doc, &WatermarkOptions::default()).unwrap();
        add_header_footer(&doc, &HeaderFooterOptions::default()).unwrap();
        add_bates(&doc, &BatesOptions::default()).unwrap();
        let removed = remove_stamps(
            &doc,
            &[
                StampKind::Watermark,
                StampKind::Background,
                StampKind::HeaderFooter,
                StampKind::Bates,
            ],
        )
        .unwrap();
        assert_eq!(removed, 6, "3 tampons × 2 pages");
        let doc = reload(&doc);
        for index in 0..2 {
            let before = content_of(&original, index);
            let after = content_of(&doc, index);
            assert_eq!(after.trim(), before.trim(), "page {index}");
            let pages_a = collect_pages(&original).unwrap();
            let pages_b = collect_pages(&doc).unwrap();
            let a = render_page(&original, &pages_a[index], 1.0, &RenderOptions::default());
            let b = render_page(&doc, &pages_b[index], 1.0, &RenderOptions::default());
            for y in 0..a.bitmap.height() {
                for x in 0..a.bitmap.width() {
                    assert_eq!(
                        a.bitmap.pixel(x, y),
                        b.bitmap.pixel(x, y),
                        "pixel ({x}, {y}) de la page {index}"
                    );
                }
            }
        }
    }

    #[test]
    fn removal_can_target_one_kind() {
        let doc = sample(1, 0);
        add_watermark(&doc, &WatermarkOptions::default()).unwrap();
        add_bates(&doc, &BatesOptions::default()).unwrap();
        assert_eq!(remove_stamps(&doc, &[StampKind::Bates]).unwrap(), 1);
        let content = content_of(&doc, 0);
        assert!(content.contains(" Do"), "le filigrane doit rester");
        assert!(
            !content.contains("000001"),
            "le numéro doit partir : {content}"
        );
        assert_eq!(remove_stamps(&doc, &[StampKind::Bates]).unwrap(), 0);
    }

    #[test]
    fn stamps_follow_the_page_rotation() {
        // Sur une page pivotée de 90°, un pied de page reste en bas de la
        // page telle qu'elle s'affiche.
        let doc = sample(1, 90);
        add_bates(
            &doc,
            &BatesOptions {
                anchor: Anchor::BottomCenter,
                digits: 2,
                start: 5,
                ..BatesOptions::default()
            },
        )
        .unwrap();
        let doc = reload(&doc);
        let pages = collect_pages(&doc).unwrap();
        let rendered = render_page(
            &doc,
            &pages[0],
            1.0,
            &RenderOptions {
                background: Some(acrux_graphics::Color::WHITE),
                ..RenderOptions::default()
            },
        );
        // Image pivotée : 300 de large, 400 de haut. L'encre du numéro doit
        // se trouver en bas au centre de cette image.
        assert_eq!(
            (rendered.bitmap.width(), rendered.bitmap.height()),
            (300, 400)
        );
        let mut ink = 0;
        for y in 360..395 {
            for x in 120..180 {
                if rendered.bitmap.pixel(x, y).is_some_and(|p| p[0] < 200) {
                    ink += 1;
                }
            }
        }
        assert!(
            ink > 10,
            "numéro absent du bas de la page pivotée ({ink} px)"
        );
    }

    #[test]
    fn an_image_watermark_is_embedded() {
        let doc = sample(1, 0);
        let rgb = vec![255u8, 0, 0, 0, 0, 255, 0, 255, 0, 255, 255, 0];
        let png = acrux_graphics::encode_png_rgb(2, 2, &rgb);
        add_watermark(
            &doc,
            &WatermarkOptions {
                source: StampSource::Image { data: png },
                placement: Placement {
                    anchor: Anchor::TopRight,
                    margin: 10.0,
                    scale: Scale::Absolute(20.0),
                    ..Placement::default()
                },
                opacity: Opacity::default(),
                behind: false,
                pages: Vec::new(),
            },
        )
        .unwrap();
        let doc = reload(&doc);
        let pages = collect_pages(&doc).unwrap();
        let rendered = render_page(&doc, &pages[0], 1.0, &RenderOptions::default());
        // Coin supérieur droit : le quart haut-gauche de l'image est rouge.
        let p = rendered.bitmap.pixel(360, 20).unwrap();
        assert!(p[0] > 200 && p[1] < 60, "coin rouge attendu : {p:?}");
        // Une image illisible est refusée proprement.
        assert!(add_watermark(
            &sample(1, 0),
            &WatermarkOptions {
                source: StampSource::Image {
                    data: b"pas une image".to_vec()
                },
                ..WatermarkOptions::default()
            }
        )
        .is_err());
    }

    #[test]
    fn text_outside_winansi_embeds_a_composite_font() {
        let doc = sample(1, 0);
        let report = add_watermark(
            &doc,
            &WatermarkOptions {
                source: StampSource::Text {
                    text: "秘密".into(),
                    font: StandardFont::Helvetica,
                    size: 20.0,
                    color: [0.0, 0.0, 0.0],
                },
                ..WatermarkOptions::default()
            },
        )
        .unwrap();
        assert_eq!(report.pages, 1);
        // Une police composite a été posée : c'est elle qui sait écrire ces
        // caractères, là où les quatorze polices standard les remplaçaient
        // par « ? ».
        let mut type0 = 0;
        let mut files = 0;
        for number in doc.object_numbers() {
            let Ok(object) = doc.get(acrux_document::ObjectRef {
                number,
                generation: 0,
            }) else {
                continue;
            };
            let Some(dict) = object.as_dict() else {
                continue;
            };
            if matches!(dict.get(&Name::new("Subtype")), Some(Object::Name(n)) if n.0 == b"Type0") {
                type0 += 1;
            }
            if dict.contains_key(&Name::new("FontFile2")) {
                files += 1;
            }
        }
        // Sur une machine sans police système, le repli reste licite : on
        // vérifie alors que l'utilisateur est prévenu.
        if type0 == 0 {
            assert!(
                report.warnings.iter().any(|w| w.contains("WinAnsi")),
                "ni police incorporée ni avertissement : {:?}",
                report.warnings
            );
        } else {
            assert_eq!(type0, 1, "une seule police composite");
            assert_eq!(files, 1, "le programme de police est incorporé");
            assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        }
    }

    #[test]
    fn multiline_watermarks_are_centred() {
        let doc = sample(1, 0);
        add_watermark(
            &doc,
            &WatermarkOptions {
                source: StampSource::Text {
                    text: "NE PAS\nDIFFUSER".into(),
                    font: StandardFont::HelveticaBold,
                    size: 24.0,
                    color: [0.4, 0.4, 0.4],
                },
                placement: Placement::default(),
                opacity: Opacity::default(),
                behind: false,
                pages: Vec::new(),
            },
        )
        .unwrap();
        let doc = reload(&doc);
        let pages = collect_pages(&doc).unwrap();
        let text = crate::text::extract_page_text(&doc, &pages[0]).unwrap();
        let plain = text.to_plain();
        assert!(
            plain.contains("NE PAS") && plain.contains("DIFFUSER"),
            "{plain}"
        );
        let centre = |needle: &str| {
            let b = text
                .words()
                .into_iter()
                .find(|w| w.text.contains(needle))
                .unwrap_or_else(|| panic!("« {needle} » absent"))
                .bbox;
            f64::midpoint(b.x0, b.x1)
        };
        assert!(
            (centre("DIFFUSER") - centre("PAS")).abs() < 30.0,
            "lignes non centrées l'une sur l'autre"
        );
    }

    #[test]
    fn a_standard_font_carries_its_to_unicode() {
        let doc = sample(1, 0);
        add_bates(&doc, &BatesOptions::default()).unwrap();
        let doc = reload(&doc);
        let fonts: Vec<Dict> = doc
            .object_numbers()
            .into_iter()
            .filter_map(|n| {
                let d = doc
                    .get(ObjectRef {
                        number: n,
                        generation: 0,
                    })
                    .ok()?
                    .as_dict()
                    .cloned()?;
                (d.get(&Name::new("BaseFont")) == Some(&Object::Name(Name::new("Helvetica"))))
                    .then_some(d)
            })
            .collect();
        let added = fonts
            .iter()
            .find(|d| d.contains_key(&Name::new("ToUnicode")))
            .expect("police posée avec /ToUnicode");
        let cmap = doc
            .resolve(added.get(&Name::new("ToUnicode")).unwrap())
            .unwrap();
        let data = doc.stream_data(&cmap).unwrap();
        let text = String::from_utf8_lossy(&data.data);
        assert!(text.contains("beginbfchar"), "{text}");
        assert!(text.contains("<E9> <00E9>"), "é manquant du ToUnicode");
    }
    /// PNG RVBA de `size` × `size` : disque bleu dégradé sur fond
    /// transparent, pour exercer le canal alpha (`/SMask`).
    fn disc_png(size: u32) -> Vec<u8> {
        let mut pixels = Vec::with_capacity((size * size * 4) as usize);
        let radius = f64::from(size) / 2.0;
        for y in 0..size {
            for x in 0..size {
                let (dx, dy) = (f64::from(x) - radius + 0.5, f64::from(y) - radius + 0.5);
                let d = dx.hypot(dy) / radius;
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let alpha = if d >= 1.0 {
                    0
                } else {
                    (255.0 * (1.0 - d * d)) as u8
                };
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let blue = (120.0 + 135.0 * (1.0 - d.min(1.0))) as u8;
                pixels.extend_from_slice(&[20, 60, blue, alpha]);
            }
        }
        acrux_graphics::encode_png_with(
            size,
            size,
            acrux_graphics::PixelLayout::Rgba,
            &pixels,
            &|data| acrux_codecs::flate::compress(data, 6),
        )
    }

    #[test]
    fn a_png_with_alpha_becomes_a_soft_mask() {
        let doc = sample(1, 0);
        add_watermark(
            &doc,
            &WatermarkOptions {
                source: StampSource::Image { data: disc_png(24) },
                placement: Placement {
                    scale: Scale::Absolute(4.0),
                    ..Placement::default()
                },
                opacity: Opacity::default(),
                behind: false,
                pages: Vec::new(),
            },
        )
        .unwrap();
        let doc = reload(&doc);
        // L'image porte bien un /SMask.
        let has_smask = doc.object_numbers().into_iter().any(|n| {
            doc.get(ObjectRef {
                number: n,
                generation: 0,
            })
            .ok()
            .and_then(|o| o.as_dict().cloned())
            .is_some_and(|d| {
                d.get(&Name::new("Subtype")) == Some(&Object::Name(Name::new("Image")))
                    && d.contains_key(&Name::new("SMask"))
            })
        });
        assert!(has_smask, "/SMask attendu pour un PNG avec alpha");
        // Au rendu : bleu au centre, papier blanc dans les coins du carré.
        let pages = collect_pages(&doc).unwrap();
        let rendered = render_page(
            &doc,
            &pages[0],
            1.0,
            &RenderOptions {
                background: Some(acrux_graphics::Color::WHITE),
                ..RenderOptions::default()
            },
        );
        let centre = rendered.bitmap.pixel(200, 150).unwrap();
        assert!(centre[2] > centre[0] + 60, "centre non bleu : {centre:?}");
        let corner = rendered.bitmap.pixel(155, 105).unwrap();
        assert!(
            corner.iter().take(3).all(|c| *c > 240),
            "coin transparent attendu : {corner:?}"
        );
    }

    /// Document de départ du fichier de corpus : deux pages A5 paysage avec
    /// un titre, un paragraphe, un filet et un tableau simple.
    fn corpus_source() -> Vec<u8> {
        let mut objects = String::from(
            "%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
             2 0 obj << /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >> endobj\n",
        );
        let body = [
            (
                "Rapport trimestriel",
                [
                    "Ce document de synth\\350se sert de t\\351moin : son contenu d'origine",
                    "ne doit jamais bouger quand on lui pose un filigrane, un en-t\\352te,",
                    "un pied de page ou une num\\351rotation Bates.",
                ],
            ),
            (
                "Annexe : chiffres",
                [
                    "Chaque rep\\350re | ci-dessous marque la fin d'une ligne : il permet",
                    "de v\\351rifier d'un coup d'oeil qu'aucun glyphe n'a \\351t\\351 d\\351plac\\351",
                    "par les tampons pos\\351s sur la page.",
                ],
            ),
        ];
        for (index, (title, paragraph)) in body.iter().enumerate() {
            let page = 3 + index * 2;
            let mut content = String::new();
            let _ = writeln!(
                content,
                "0.15 0.18 0.25 rg BT /F2 18 Tf 40 240 Td ({title}) Tj ET"
            );
            let _ = writeln!(content, "0.6 0.6 0.65 RG 1 w 40 232 m 380 232 l S");
            let _ = writeln!(content, "0 g BT /F1 10 Tf 40 210 Td 14 TL");
            for line in paragraph {
                let _ = writeln!(content, "({line} |) Tj T*");
            }
            content.push_str("ET\n");
            let _ = writeln!(
                content,
                "0.85 0.88 0.95 rg 40 60 160 40 re f 0 g BT /F2 11 Tf 52 76 Td (Total : {} k\\200) Tj ET",
                (index + 1) * 137
            );
            let _ = writeln!(
                objects,
                "{page} 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 420 297] \
                 /Contents {} 0 R /Resources << /Font << \
                 /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >> \
                 /F2 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >> \
                 >> >> >> endobj",
                page + 1
            );
            let _ = writeln!(
                objects,
                "{} 0 obj << /Length {} >>\nstream\n{content}endstream\nendobj",
                page + 1,
                content.len()
            );
        }
        objects.into_bytes()
    }

    /// Génère `tests/corpus/synthese/filigrane-entete-bates.pdf`, qui montre
    /// d'un coup les quatre apports visuels du module. Lancer avec
    /// `cargo test -p acrux-features --lib -- --ignored generate_stamp_corpus`.
    ///
    /// Le fichier ne doit contenir **aucune date** : son image de référence
    /// serait périmée le lendemain.
    #[test]
    #[ignore = "génère le fichier de corpus"]
    #[allow(clippy::too_many_lines)] // un bloc commenté par tampon posé
    fn generate_stamp_corpus() {
        let doc = Document::from_bytes(corpus_source()).unwrap();
        // 1. Arrière-plan image : le disque dégradé et son canal alpha, en
        // bas à droite. Chaque arrière-plan se glisse **devant** le
        // précédent : le disque est donc posé avant l'aplat qui doit rester
        // dessous.
        add_background(
            &doc,
            &BackgroundOptions {
                source: StampSource::Image { data: disc_png(48) },
                placement: Placement {
                    anchor: Anchor::BottomRight,
                    margin: 18.0,
                    scale: Scale::Absolute(2.0),
                    ..Placement::default()
                },
                opacity: Opacity::default(),
                pages: Vec::new(),
            },
        )
        .unwrap();
        // 2. Aplat crème sur toute la page, sous tout le reste.
        add_background(
            &doc,
            &BackgroundOptions {
                source: StampSource::Fill {
                    color: [0.99, 0.975, 0.93],
                },
                ..BackgroundOptions::default()
            },
        )
        .unwrap();
        // 3. Filigrane texte sur deux lignes, en diagonale, à 22 % d'opacité,
        // devant le contenu.
        add_watermark(
            &doc,
            &WatermarkOptions {
                source: StampSource::Text {
                    text: "BROUILLON\nNE PAS DIFFUSER".into(),
                    font: StandardFont::HelveticaBold,
                    size: 36.0,
                    color: [0.8, 0.1, 0.1],
                },
                placement: Placement {
                    anchor: Anchor::Center,
                    rotation: 28.0,
                    scale: Scale::RelativeToPage(0.85),
                    margin: 12.0,
                    ..Placement::default()
                },
                opacity: Opacity {
                    fill: 0.22,
                    stroke: 0.22,
                },
                behind: false,
                pages: Vec::new(),
            },
        )
        .unwrap();
        // 4. En-tête et pied de page, six zones, numéro en chiffres romains.
        add_header_footer(
            &doc,
            &HeaderFooterOptions {
                header: [
                    "{filename}".into(),
                    "Rapport trimestriel".into(),
                    "Diffusion restreinte".into(),
                ],
                footer: [
                    "Acrux".into(),
                    "page {page} sur {pages}".into(),
                    String::new(),
                ],
                font: StandardFont::Helvetica,
                size: 8.0,
                color: [0.25, 0.25, 0.3],
                margins: Margins {
                    top: 18.0,
                    bottom: 18.0,
                    left: 28.0,
                    right: 28.0,
                },
                start_number: 1,
                format: NumberFormat::LowerRoman,
                file_name: "rapport.pdf".into(),
                pages: Vec::new(),
                utc_offset_minutes: 0,
            },
        )
        .unwrap();
        // 5. Numérotation Bates en bas à droite.
        add_bates(
            &doc,
            &BatesOptions {
                prefix: "ACME-".into(),
                suffix: String::new(),
                digits: 6,
                start: 1,
                anchor: Anchor::BottomRight,
                margin_x: 28.0,
                margin_y: 30.0,
                font: StandardFont::Courier,
                size: 8.0,
                color: [0.5, 0.1, 0.1],
                pages: Vec::new(),
            },
        )
        .unwrap();
        let saved = doc.save_full().unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/corpus/synthese/filigrane-entete-bates.pdf");
        std::fs::write(&path, saved).unwrap();
        println!("écrit : {}", path.display());
    }
}
