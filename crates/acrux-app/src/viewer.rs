//! Visualiseur : affiche les pages d'un document avec défilement continu,
//! zoom, navigation clavier, ouverture par dialogue ou glisser-déposer, et
//! une barre d'état dessinée par le toolkit interne. Les pages sont rendues
//! sur un fil séparé (`render_worker`) : l'interface ne se fige jamais.
//!
//! Fonctions : recherche (Ctrl+F, `viewer/search.rs`), sélection de texte à
//! la souris (simple, double et triple clic), copie (Ctrl+C), tout
//! sélectionner (Ctrl+A), saisie du mot de passe des documents chiffrés.
//!
//! Pas encore de barre d'outils ni de panneaux (voir ROADMAP.md phase 3).

// Coordonnées d'écran entières et dimensions non signées se mélangent comme
// dans toute couche de présentation ; les casts sont bornés par la taille de
// la fenêtre.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::manual_midpoint
)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use acrux_core::{Matrix, Point, Rect};
use acrux_document::{collect_pages, Document, Page};
use acrux_features::annotations::{
    list_annotations, AnnotMeta, MarkupKind, NewAnnotation, CARET_COLOR,
};
use acrux_features::attach::{list_attachments, read_attachment};
use acrux_features::forms::{list_fields, Field, FieldType};
use acrux_features::navigation::{
    flatten_outline, outline, page_links, Action, Destination, Link, PageIndex, View,
};
use acrux_features::redact::RedactionMark;
use acrux_features::text::{extract_page_text, PageText, SearchOptions};
use acrux_graphics::{Bitmap, Color, Rasterizer};
use acrux_render::page::base_matrix;
use acrux_render::{
    add_rotation, page_pixel_size_rotated, render_page, render_page_rotated, RenderOptions,
};

use crate::platform::{
    App, Cursor, Event, Frame, Key, Modifiers, MouseButton, PrintOutcome, PrintSource, Waker,
    WindowHandle,
};
use crate::render_worker::ExportFormat;
use crate::render_worker::{AnnotItem, EditOp, RenderWorker};
use crate::selection::{SelectableText, Selection, TextPos};
use crate::ui::anim::{ease_out, Anim, Clock};
use crate::ui::icons::Icon;
use crate::ui::input::TextInput;
use crate::ui::lang::{self, Lang};
use crate::ui::modal::{PromptAct, PromptCard, PromptContent, PromptFocus};
use crate::ui::objects::{self as objects_ui, Handle, ViewRect};
use crate::ui::paint::{round_rect, round_rect_alpha, round_rect_outline, shadow};
use crate::ui::palette::{Command, Palette, PaletteDown};
use crate::ui::panel::{
    AttachmentRow, CommentRow, OutlineRow, Panel, PanelAction, PanelContent, PanelTab,
};
use crate::ui::prefs::{Fit, Prefs, ViewMode};
use crate::ui::settings::{SettingsAction, SettingsSheet, SettingsState, UpdateLine};
use crate::ui::sign::{self, Capture, Item as SignItem, Saved};
use crate::ui::signpanel::{Action as SignAction, SignPanel};
use crate::ui::tabs::{TabAction, TabInfo, TabPart, Tabs};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;
use crate::ui::toolbar::{ToolAction, Toolbar, ToolbarInfo};
use crate::ui::tools::{ToolsInfo, ToolsPanel};
use crate::ui::video::{self as video_ui, Box2, Hit as VideoHit};
use acrux_features::edit_objects::{self, Edit as ObjectEdit, PageObject};
use acrux_features::fillsign::ink::{InkPoint, Nib, Pen, Stroke, Weight};

mod context;
mod dialogs;
mod editmode;
mod formfill;
mod history;
mod protect;
mod search;
mod status;
mod three_d;
mod zoom;
use crate::ui::editpdf::EditTool;
use crate::ui::formbar::FormBar;
use crate::ui::modebar::ModeBar;
use context::{ContextMenu, Target};
use dialogs::{Asking, Then};
use editmode::EditMode;
use history::{NavHistory, Spot};
use protect::OwnerThen;
use search::{Search, SearchFocus};
use status::StatusSeg;
use zoom::ZoomAnchor;

/// Rectangle semi-transparent (alpha 0..255) composé sur le tampon.
// Position, taille, couleur, alpha : primitive de dessin à coordonnées courtes.
#[allow(clippy::too_many_arguments, clippy::many_single_char_names)]
fn fill_rect_blend(
    frame: &mut Frame<'_>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    color: (u8, u8, u8),
    alpha: u32,
) {
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w).min(frame.width as i32);
    let y1 = (y + h).min(frame.height as i32);
    let inv = 255 - alpha;
    for yy in y0..y1 {
        for xx in x0..x1 {
            let i = frame.index(xx as usize, yy as usize);
            let d = &mut frame.pixels[i..i + 4];
            d[0] = ((u32::from(color.2) * alpha + u32::from(d[0]) * inv) / 255) as u8;
            d[1] = ((u32::from(color.1) * alpha + u32::from(d[1]) * inv) / 255) as u8;
            d[2] = ((u32::from(color.0) * alpha + u32::from(d[2]) * inv) / 255) as u8;
        }
    }
}

/// Journal de débogage : si la variable d'environnement `ACRUX_LOG` désigne un
/// fichier, chaque événement y est ajouté (diagnostic des tests automatisés).
fn log_event(event: &Event) {
    if matches!(event, Event::MouseMove { .. } | Event::Wake) {
        return;
    }
    log_line(&format!("{event:?}"));
}

/// Vrai pour les touches qui commandent la disposition de la fenêtre : elles
/// gardent le même effet quelle que soit la zone qui tient le focus.
///
/// Ctrl+F4 n'en est pas : c'est « fermer l'onglet », comme dans tous les
/// programmes à onglets de Windows. Il ouvrait le panneau latéral.
fn is_global_key(key: Key, m: Modifiers) -> bool {
    match key {
        Key::F(4) => !m.ctrl,
        Key::F(3 | 5 | 6 | 11) => true,
        _ => false,
    }
}

/// Rotation sous laquelle une page est **affichée** : son `/Rotate` plus la
/// rotation de la vue de l'onglet.
///
/// Tout ce qui passe de la page à l'écran ou de l'écran à la page (clics,
/// sélection, surlignages, liens, champs, ajustement) doit la prendre, sans
/// quoi un clic sur une vue tournée viserait à côté. L'impression et la
/// vignette de l'accueil, elles, montrent le document tel qu'il est : elles
/// s'en tiennent à `/Rotate`, comme Acrobat, où la vue tournée ne s'imprime
/// pas.
fn shown_rotation(l: &Loaded, p: &Page) -> i32 {
    add_rotation(p.rotate(&l.doc), l.view_rotation)
}

/// Zone de l'interface qui reçoit les touches. F6 passe de l'une à l'autre,
/// comme le veut l'usage Windows ; à l'intérieur d'une zone, les flèches
/// déplacent et Entrée active. Tout ce que fait la souris doit être
/// atteignable ainsi.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Region {
    /// Le document : défilement, sélection, champs de formulaire.
    Document,
    /// La barre d'outils.
    Toolbar,
    /// Le panneau latéral.
    Panel,
}

/// Info-bulle en attente ou affichée : son texte, le rectangle du bouton
/// survolé et l'instant où le survol a commencé.
type Tip = (String, (i32, i32, i32, i32), std::time::Instant);

/// Délai avant l'apparition d'une info-bulle : assez long pour ne pas gêner
/// quand on traverse la barre, assez court pour répondre à une hésitation.
const TIP_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// Un passage sur une ligne : les blancs, retours à la ligne compris,
/// ramenés à une seule espace. C'est la forme sous laquelle un balisage
/// garde le texte qu'il désigne — la mise en lignes de la page n'a plus de
/// sens dans une note.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Commentaires du document : les annotations qui portent un texte ou une
/// intention de relecture. Les liens et les champs de formulaire n'en sont
/// pas — ils ont déjà leur place ailleurs dans l'interface.
///
/// Un remplacement est un groupe de deux annotations (un signe d'insertion
/// et le texte barré qui le suit, §12.5.6.2) : il ne fait qu'**une** ligne,
/// celle du signe, qui porte le texte proposé. Le type est une clé
/// française, traduite au dessin : changer de langue se voit tout de suite.
fn collect_comments(doc: &Document, pages: &[Page]) -> Vec<CommentRow> {
    let mut out = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let Ok(list) = list_annotations(doc, page) else {
            continue;
        };
        // Les membres principaux des groupes de cette page : un signe
        // d'insertion qui en est un propose de remplacer, pas d'ajouter.
        let primaries: HashSet<acrux_document::ObjectRef> = list
            .iter()
            .filter(|a| a.is_group_member())
            .filter_map(|a| a.in_reply_to)
            .collect();
        for a in list {
            if a.is_group_member() {
                continue;
            }
            let kind = match a.subtype.as_str() {
                "Text" => "Note",
                "Highlight" => "Surlignage",
                "Underline" => "Soulignement",
                "StrikeOut" => "Texte barré",
                "Squiggly" => "Soulignement ondulé",
                "Caret" if a.reference.is_some_and(|r| primaries.contains(&r)) => "Remplacement",
                "Caret" => "Insertion",
                "Square" | "Circle" => "Forme",
                "Line" | "Polygon" | "PolyLine" => "Trait",
                "Ink" => "Dessin",
                "FreeText" => "Texte libre",
                "Stamp" => "Tampon",
                "FileAttachment" => "Pièce jointe",
                "Redact" => "Biffure",
                _ => continue,
            };
            out.push(CommentRow {
                page: index,
                kind,
                author: a.author.clone(),
                contents: a.contents.clone().unwrap_or_default(),
                rect: a.rect,
                index: a.index,
                name: a.name.clone(),
            });
        }
    }
    out
}

/// Pièces jointes du document, mises en forme pour le panneau.
fn collect_attachments(doc: &Document) -> Vec<AttachmentRow> {
    list_attachments(doc)
        .unwrap_or_default()
        .into_iter()
        .map(|a| AttachmentRow {
            // La description dit mieux que le type MIME ce qu'est le
            // fichier ; le type prend le relais quand elle manque.
            detail: a
                .description
                .clone()
                .or_else(|| a.mime.clone())
                .unwrap_or_default(),
            name: a.name,
            size: a.size,
            page: a.page,
        })
        .collect()
}

/// Étiquettes de page du document, ou une liste vide quand il s'en tient à la
/// numérotation décimale : l'interface n'a alors rien de particulier à faire.
fn collect_labels(doc: &Document) -> Vec<String> {
    if acrux_features::pagelabels::read_label_ranges(doc)
        .unwrap_or_default()
        .is_empty()
    {
        return Vec::new();
    }
    acrux_features::pagelabels::read_page_labels(doc).unwrap_or_default()
}

/// Ajoute une ligne au journal `ACRUX_LOG` (voir [`log_event`]).
fn log_line(line: &str) {
    use std::io::Write;
    let Ok(path) = std::env::var("ACRUX_LOG") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Débogage : si `ACRUX_SHOT` désigne un fichier, chaque peinture y écrit le
/// tampon de la fenêtre au format PPM binaire (non compressé : quelques
/// millisecondes, pour ne pas fausser les mesures ni retarder l'affichage).
fn dump_frame(frame: &Frame<'_>) {
    let Ok(path) = std::env::var("ACRUX_SHOT") else {
        return;
    };
    let row = frame.width as usize * 4;
    let mut out = format!(
        "P6
{} {}
255
",
        frame.width, frame.height
    )
    .into_bytes();
    out.reserve(frame.width as usize * frame.height as usize * 3);
    for y in 0..frame.height as usize {
        let start = frame.index(0, y);
        for p in frame.pixels[start..start + row].chunks_exact(4) {
            out.extend_from_slice(&[p[2], p[1], p[0]]);
        }
    }
    let _ = std::fs::write(path, out);
}

/// Place d'une page dans la vue, en pixels, **avant** défilement : `x` part
/// du bord gauche de la zone de document, `y` du haut du document.
#[derive(Debug, Clone, Copy, Default)]
struct PageBox {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    /// Fausse pour les pages qu'un mode « page unique » n'affiche pas.
    visible: bool,
}

/// Regroupement des pages en rangées : une page par rangée, ou deux dans les
/// modes « deux pages » (la couverture restant seule si `cover`, comme le
/// « Show Cover Page » d'Acrobat). Chaque rangée est donnée par sa première
/// et sa dernière page, bornes comprises.
fn page_rows(count: usize, two_up: bool, cover: bool) -> Vec<(usize, usize)> {
    if count == 0 {
        return Vec::new();
    }
    if !two_up {
        return (0..count).map(|i| (i, i)).collect();
    }
    let mut out = Vec::new();
    let mut i = 0;
    if cover {
        out.push((0, 0));
        i = 1;
    }
    while i < count {
        out.push((i, (i + 1).min(count - 1)));
        i += 2;
    }
    out
}

/// Indice de la rangée contenant `page` (0 si elle n'y est pas).
fn row_of(rows: &[(usize, usize)], page: usize) -> usize {
    rows.iter()
        .position(|&(a, b)| page >= a && page <= b)
        .unwrap_or(0)
}

/// Marge entre les pages et autour, en pixels logiques.
const GAP: i32 = 16;
/// Largeur du panneau latéral, en pixels logiques.
const PANEL_WIDTH: u32 = 240;
/// Poids du cache des pages rendues au-delà duquel on l'élague, même s'il
/// compte moins de douze images : à 400 %, une seule page pèse déjà plus de
/// cent mégaoctets.
///
/// Bien moins que le plafond d'une page (256 Mo, voir `viewer/zoom.rs`) :
/// l'élagage ne touche jamais aux pages proches de la vue ni à l'aperçu
/// d'une page qui attend son rendu, il ne jette que les pages lointaines et
/// les échelles passées. À 256 Mo, l'image d'une page vue à 400 % ou au
/// plafond du zoom restait en mémoire, inutile, une fois revenu à 100 % —
/// jusqu'à un quart de gigaoctet gardé pour rien sur une machine modeste.
const CACHE_BYTES: usize = 96 * 1024 * 1024;

/// Documents récents montrés sur l'écran d'accueil.
const MAX_WELCOME: usize = 8;
/// Largeur minimale laissée à la page, en pixels logiques. En deçà, la barre
/// des outils s'efface.
const MIN_PAGE_WIDTH: u32 = 420;
/// Outil d'annotation en cours : on agit directement sur la page, sans
/// sélectionner d'abord.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnnotTool {
    /// Glisser sur du texte le surligne.
    Highlight,
    /// Glisser sur du texte le souligne.
    Underline,
    /// Glisser sur du texte le barre.
    StrikeOut,
    /// Glisser sur du texte le souligne d'un trait ondulé.
    Squiggly,
    /// Un clic dans une ligne de texte y pose un signe d'insertion.
    Insert,
    /// Glisser sur du texte le barre et propose un texte à la place.
    Replace,
    /// Un clic pose une note.
    Note,
    /// Glisser sur du texte le marque pour biffure.
    Redact,
}

/// Les outils de la barre des commentaires, dans l'ordre de ses boutons.
///
/// C'est **la** liste des outils de relecture : ceux qui viendront (formes,
/// crayon, zones de texte, tampons) s'y ajoutent, et la barre les montre
/// sans autre changement. La biffure n'en est pas : elle efface, elle ne
/// commente pas, et reste sous « Protéger ».
const COMMENT_TOOLS: &[AnnotTool] = &[
    AnnotTool::Highlight,
    AnnotTool::Underline,
    AnnotTool::StrikeOut,
    AnnotTool::Squiggly,
    AnnotTool::Insert,
    AnnotTool::Replace,
    AnnotTool::Note,
];

impl AnnotTool {
    /// Nom et consigne affichés dans la barre de l'outil.
    fn describe(self) -> (&'static str, &'static str) {
        match self {
            AnnotTool::Highlight => ("Surligner", "Faites glisser sur le texte à surligner."),
            AnnotTool::Underline => ("Souligner", "Faites glisser sur le texte à souligner."),
            AnnotTool::StrikeOut => ("Barrer", "Faites glisser sur le texte à barrer."),
            AnnotTool::Squiggly => (
                "Souligner d'un trait ondulé",
                "Faites glisser sur le texte à souligner d'un trait ondulé.",
            ),
            AnnotTool::Insert => (
                "Insérer du texte",
                "Cliquez dans le texte, à l'endroit de l'insertion.",
            ),
            AnnotTool::Replace => (
                "Remplacer le texte",
                "Faites glisser sur le texte à remplacer.",
            ),
            AnnotTool::Note => (
                "Poser une note",
                "Cliquez sur la page, à l'endroit de la note.",
            ),
            AnnotTool::Redact => (
                "Biffer",
                "Faites glisser sur le texte à biffer, puis « Appliquer les biffures ».",
            ),
        }
    }

    /// Commande de la palette qui l'allume.
    fn command(self) -> Command {
        match self {
            AnnotTool::Highlight => Command::HighlightTool,
            AnnotTool::Underline => Command::UnderlineTool,
            AnnotTool::StrikeOut => Command::StrikeOutTool,
            AnnotTool::Squiggly => Command::SquigglyTool,
            AnnotTool::Insert => Command::InsertTextTool,
            AnnotTool::Replace => Command::ReplaceTextTool,
            AnnotTool::Note => Command::NoteTool,
            AnnotTool::Redact => Command::RedactTool,
        }
    }

    /// Commande qui fait la même chose **sans** outil, sur la sélection ou
    /// au pointeur : c'est elle qui porte le raccourci (H, U, N) que
    /// l'info-bulle du bouton rappelle.
    fn action(self) -> Command {
        match self {
            AnnotTool::Highlight => Command::Highlight,
            AnnotTool::Underline => Command::Underline,
            AnnotTool::StrikeOut => Command::StrikeOut,
            AnnotTool::Squiggly => Command::Squiggly,
            AnnotTool::Insert => Command::InsertText,
            AnnotTool::Replace => Command::ReplaceText,
            AnnotTool::Note => Command::Note,
            AnnotTool::Redact => Command::MarkRedaction,
        }
    }

    /// Balisage que pose l'outil, s'il en pose un.
    fn markup(self) -> Option<MarkupKind> {
        match self {
            AnnotTool::Highlight => Some(MarkupKind::Highlight),
            AnnotTool::Underline => Some(MarkupKind::Underline),
            AnnotTool::StrikeOut => Some(MarkupKind::StrikeOut),
            AnnotTool::Squiggly => Some(MarkupKind::Squiggly),
            AnnotTool::Insert | AnnotTool::Replace | AnnotTool::Note | AnnotTool::Redact => None,
        }
    }

    /// Icône de l'outil, dans sa barre et sur son bouton.
    fn icon(self) -> Icon {
        match self {
            AnnotTool::Highlight => Icon::Highlight,
            AnnotTool::Underline => Icon::Underline,
            AnnotTool::StrikeOut => Icon::StrikeOut,
            AnnotTool::Squiggly => Icon::Squiggly,
            AnnotTool::Insert => Icon::Insert,
            AnnotTool::Replace => Icon::Replace,
            AnnotTool::Note => Icon::Note,
            AnnotTool::Redact => Icon::Redact,
        }
    }

    /// Vrai pour les outils de la barre des commentaires.
    fn in_comment_bar(self) -> bool {
        COMMENT_TOOLS.contains(&self)
    }
}

/// Élément de « remplir et signer » sélectionné sur la page.
#[derive(Debug, Clone)]
struct PlacedSel {
    /// Page.
    page: usize,
    /// Rang de l'annotation dans la page.
    index: usize,
    /// Rectangle occupé, en coordonnées de page.
    rect: Rect,
    /// Sorte de l'élément (`drawn`, `typed`, `mark:check`…) : de quoi le
    /// redessiner pendant qu'on le déplace.
    kind: String,
    /// Geste en cours : poignée saisie, point de départ, rectangle courant.
    drag: Option<(Option<objects_ui::Handle>, Point, Rect)>,
    /// Poignée survolée.
    hover: Option<objects_ui::Handle>,
}

/// Un trait en train d'être tracé au stylo, sur une page.
#[derive(Debug)]
struct Inking {
    /// Page dessinée.
    page: usize,
    /// Points relevés, en coordonnées de page.
    points: Vec<InkPoint>,
}

// Des drapeaux indépendants (modifié, fichier temporaire, réécriture
// complète, champs à remplir, barre fermée) : autant de faits distincts sur
// un document ouvert.
#[allow(clippy::struct_excessive_bools)]
struct Loaded {
    path: PathBuf,
    doc: Document,
    pages: Vec<Page>,
    /// Bitmaps rendus, par (index de page, échelle en millièmes).
    cache: HashMap<(usize, u32), Bitmap>,
    /// Fil de rendu ; `None` si le document n'a pas pu être rouvert (rendu direct).
    worker: Option<RenderWorker>,
    /// Texte extrait par page, calculé à la demande (recherche, sélection).
    texts: HashMap<usize, (PageText, SelectableText)>,
    /// Liens par page, calculés à la demande.
    links: HashMap<usize, Vec<Link>>,
    /// Table objet page → indice, pour résoudre les destinations.
    page_index: PageIndex,
    /// Signets aplatis pour le panneau.
    outline_rows: Vec<OutlineRow>,
    /// Action de chaque signet (même indice que `outline_rows`).
    outline_actions: Vec<Option<Action>>,
    /// Mot de passe fourni à l'ouverture (nécessaire pour rouvrir le
    /// document lors d'une annulation).
    password: Option<Vec<u8>>,
    /// Modifications appliquées depuis la dernière ouverture ou sauvegarde,
    /// dans l'ordre : c'est l'historique que rejoue l'annulation.
    history: Vec<EditOp>,
    /// Modifications annulées, prêtes à être rétablies.
    redo: Vec<EditOp>,
    /// Le prochain enregistrement doit réécrire le fichier **en entier**.
    ///
    /// Un chiffrement posé ou retiré ne tient pas dans une mise à jour
    /// incrémentale : tout le fichier change de clé.
    full_save: bool,
    /// Modifications non enregistrées.
    modified: bool,
    /// Le document vient d'être **fabriqué** (une image ouverte comme PDF) et
    /// n'a pas encore de fichier à lui : le premier enregistrement demande
    /// donc où le mettre, au lieu d'écrire dans le dossier temporaire.
    temporary: bool,
    /// Champs de formulaire (AcroForm), rechargés après chaque modification
    /// (par [`Loaded::set_fields`], qui tient `fillable` à jour).
    fields: Vec<Field>,
    /// Au moins un champ se remplit : c'est ce qui montre la barre de
    /// formulaire. Tenu à jour avec `fields` : `view_top` le lit à chaque
    /// mise en page, il ne doit pas parcourir tous les champs.
    fillable: bool,
    /// La barre de formulaire a été fermée pour ce document.
    form_bar_closed: bool,
    /// Commentaires (annotations porteuses de texte), pour le panneau.
    comments: Vec<CommentRow>,
    /// Calques du document et leur visibilité courante (numéro, nom, visible).
    layers: Vec<(u32, String, bool)>,
    /// Pièces jointes, pour le panneau, rechargées après chaque modification.
    attachments: Vec<AttachmentRow>,
    /// Vidéos et sons du document, avec leur rectangle : c'est ce qui rend un
    /// clic capable de savoir qu'il tombe sur un média.
    media: Vec<acrux_features::media::Media>,
    /// Modèles 3D du document, avec leur rectangle.
    models: Vec<acrux_features::three_d::Model>,
    /// Cases à cocher **dessinées** de chaque page déjà regardée (voir
    /// [`acrux_features::fillsign::boxes`]), oubliées à chaque modification.
    boxes: HashMap<usize, acrux_features::fillsign::boxes::Found>,
    /// Étiquette de chaque page (`/PageLabels`). Vide quand le document s'en
    /// tient à la numérotation décimale : c'est ce qui distingue « iii sur
    /// 240 » de « 3 sur 240 » dans la barre d'outils et la barre d'état.
    labels: Vec<String>,
    /// Rotation de la **vue**, en degrés dans le sens horaire (0, 90, 180 ou
    /// 270) : Ctrl+Maj+Plus. Elle n'est jamais écrite dans le document et
    /// ne le marque pas modifié ; elle suit l'onglet et part avec lui.
    view_rotation: i32,
    /// Vues précédentes et suivantes (Alt+←, Alt+→). Le nom `history` est
    /// déjà celui des modifications.
    nav: NavHistory,
    /// Vue de l'onglet quand il est passé à l'arrière-plan : y revenir la
    /// retrouve, au lieu de repartir de la première page.
    resume: Option<Spot>,
}

impl Loaded {
    /// Remplace l'inventaire des champs, et ce qu'on en retient.
    fn set_fields(&mut self, fields: Vec<Field>) {
        self.fillable = fields.iter().any(|f| {
            !f.flags.read_only
                && matches!(
                    f.kind,
                    FieldType::Text
                        | FieldType::CheckBox
                        | FieldType::Radio
                        | FieldType::ComboBox
                        | FieldType::ListBox
                )
        });
        self.fields = fields;
    }

    /// Options d'un rendu **à l'écran** fait ici plutôt que par le fil de
    /// rendu (une page redessinée tout de suite après un geste) : celles du
    /// fil, calques compris. Sans les calques, remplir un champ refaisait
    /// apparaître sur sa page le contenu d'un calque masqué.
    fn screen_options(&self, budget: std::time::Duration) -> RenderOptions {
        RenderOptions {
            annotations: true,
            time_budget: Some(budget),
            background: Some(Color::WHITE),
            layers: self
                .layers
                .iter()
                .map(|(n, _, visible)| (*n, *visible))
                .collect(),
        }
    }

    /// Liens d'une page (lus au premier appel).
    fn links(&mut self, page: usize) -> &[Link] {
        let (doc, pages, index) = (&self.doc, &self.pages, &self.page_index);
        self.links.entry(page).or_insert_with(|| {
            pages
                .get(page)
                .and_then(|p| page_links(doc, p, index).ok())
                .unwrap_or_default()
        })
    }

    /// Texte structuré et sélectionnable d'une page (extrait au premier appel).
    fn text(&mut self, page: usize) -> &(PageText, SelectableText) {
        let (doc, pages) = (&self.doc, &self.pages);
        self.texts.entry(page).or_insert_with(|| {
            let t = pages
                .get(page)
                .and_then(|p| extract_page_text(doc, p).ok())
                .unwrap_or_default();
            let sel = SelectableText::from_page_text(&t);
            (t, sel)
        })
    }
}

/// Ce qu'une invite attend de l'utilisateur.
#[allow(clippy::large_enum_variant)] // une seule invite à la fois : la taille est sans effet
enum PromptKind {
    /// Mot de passe d'un document chiffré : le document est déjà analysé
    /// (structure lisible), seuls ses contenus attendent la clé.
    Password {
        path: PathBuf,
        doc: Document,
        pages: Vec<Page>,
    },
    /// Mot de passe des permissions d'un document ouvert avec le seul mot
    /// de passe d'ouverture ; accepté, il donne tous les droits, puis
    /// `then` reprend ce qui l'a demandé.
    OwnerPassword { then: OwnerThen },
    /// Texte d'une note à poser sur `page` au point `(x, y)` (espace page).
    Note { page: usize, x: f64, y: f64 },
    /// Commentaire à joindre à un balisage déjà découpé en zones.
    Markup {
        kind: MarkupKind,
        zones: Vec<(usize, Rect)>,
        /// Texte balisé, page par page (voir `add_markup`).
        excerpts: Vec<(usize, String)>,
    },
    /// Texte proposé à la place d'un passage de `page`, une zone par ligne ;
    /// `passage` est le texte remplacé.
    Replace {
        page: usize,
        quads: Vec<Rect>,
        passage: String,
    },
    /// Texte à insérer sur `page`, en `x`, dans la ligne `line` (espace page).
    Insert { page: usize, x: f64, line: Rect },
    /// Texte à répartir dans un peigne de cases (IBAN, BIC, date) de `page`.
    Comb { page: usize, cells: Vec<Rect> },
    /// Texte de remplissage à poser sur `page`, dans `rect`.
    /// Nouveau texte pour une plage de glyphes d'une ligne.
    EditText {
        page: usize,
        line: usize,
        start: usize,
        end: usize,
    },
}

/// Un média ouvert et sa place dans le document.
struct MediaView {
    /// Page qui le porte.
    page: usize,
    /// Position dans le tableau `/Annots`.
    index: usize,
    /// Rectangle de l'annotation, en coordonnées de page.
    rect: Rect,
    /// Le lecteur, qui tient le décodeur et la sortie audio.
    player: crate::platform::media::Player,
    /// Nom affiché dans la barre d'état.
    title: String,
    /// Vrai tant qu'un fil de réveil bat la mesure pour cette lecture.
    ticking: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Outil « modifier » : ce qu'il sait de la page en cours d'édition.
struct ObjectTool {
    /// Page dont les objets sont chargés.
    page: usize,
    /// Objets de cette page, dans l'ordre de tracé.
    objects: Vec<PageObject>,
    /// Objet sélectionné, par son index dans `objects`.
    selected: Option<usize>,
    /// Objet survolé.
    hover: Option<usize>,
    /// Poignée survolée, quand un objet est sélectionné.
    handle: Option<Handle>,
    /// Geste en cours.
    drag: Option<ObjectDrag>,
}

/// Un geste de manipulation en cours.
#[derive(Clone, Copy)]
struct ObjectDrag {
    /// Poignée saisie.
    handle: Handle,
    /// Point de départ, en coordonnées de page.
    from: Point,
    /// Boîte de l'objet au début du geste.
    start: Rect,
    /// Boîte courante, telle que l'aperçu la montre.
    current: Rect,
    /// Proportions gardées : Maj était enfoncée au début du geste.
    keep_ratio: bool,
}

/// Rectangle qui garde les proportions d'une image dans une boîte.
fn fit_box(area: Box2, width: u32, height: u32) -> Box2 {
    if width == 0 || height == 0 {
        return area;
    }
    let ratio = f64::from(width) / f64::from(height);
    let (mut w, mut h) = (area.w, area.w / ratio);
    if h > area.h {
        h = area.h;
        w = area.h * ratio;
    }
    Box2 {
        x: area.x + (area.w - w) / 2.0,
        y: area.y + (area.h - h) / 2.0,
        w,
        h,
    }
}

/// Jour courant, en jours depuis le 1er janvier 1970.
///
/// Sert uniquement à ne pas chercher les mises à jour plus d'une fois par
/// jour ; une horloge décalée ne fait donc rien de pire qu'une recherche de
/// trop ou de moins.
fn today() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400)
}

/// Invite modale : un titre, un libellé, un champ de saisie.
struct Prompt {
    title: String,
    label: String,
    input: TextInput,
    error: Option<String>,
    kind: PromptKind,
    /// La carte commune : boutons, focus, apparition.
    card: PromptCard,
}

impl Prompt {
    /// Invite neuve, le focus dans le champ.
    fn new(title: &str, label: String, input: TextInput, kind: PromptKind) -> Self {
        Self {
            title: title.to_string(),
            label,
            input,
            error: None,
            kind,
            card: PromptCard::new(),
        }
    }
}

/// Pages du document courant vues par l'impression.
struct PrintPages<'a> {
    doc: &'a Document,
    pages: &'a [Page],
    /// Résolution plafond : un document qui ne permet que l'impression en
    /// basse résolution est rendu à celle-ci, puis agrandi par le pilote.
    max_dpi: Option<f64>,
}

impl PrintSource for PrintPages<'_> {
    fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn page_size(&self, index: usize) -> (f64, f64) {
        let Some(p) = self.pages.get(index) else {
            return (0.0, 0.0);
        };
        let b = p.crop_box(self.doc);
        match p.rotate(self.doc) {
            90 | 270 => (b.height(), b.width()),
            _ => (b.width(), b.height()),
        }
    }

    fn render(&mut self, index: usize, dpi: f64) -> Option<(u32, u32, Vec<u8>)> {
        let page = self.pages.get(index)?;
        let dpi = self.max_dpi.map_or(dpi, |max| dpi.min(max));
        let options = RenderOptions {
            annotations: true,
            time_budget: Some(std::time::Duration::from_secs(60)),
            background: Some(Color::WHITE),
            ..RenderOptions::default()
        };
        let bitmap = render_page(self.doc, page, dpi / 72.0, &options).bitmap;
        Some((bitmap.width(), bitmap.height(), bitmap.to_rgb8_over_white()))
    }
}

/// Nom d'auteur des annotations créées (compte Windows).
fn author_name() -> Option<String> {
    std::env::var("USERNAME").ok().filter(|u| !u.is_empty())
}

/// Délai maximal entre deux clics d'une même série.
const MULTI_CLICK: Duration = Duration::from_millis(500);

/// Application de visualisation.
#[allow(clippy::struct_excessive_bools)] // drapeaux d'état indépendants de la vue
pub struct Viewer {
    loaded: Option<Loaded>,
    error: Option<String>,
    width: u32,
    height: u32,
    /// Échelle DPI de la fenêtre (1.0 = 96 dpi).
    dpi_scale: f64,
    /// La fenêtre est agrandie (bouton « agrandir »).
    window_max: bool,
    /// Zoom logique (1.0 = 100 % à 96 dpi).
    zoom: f64,
    fit: Fit,
    scroll_y: f64,
    scroll_x: f64,
    drag_last: Option<(i32, i32)>,
    /// Documents à ouvrir au premier événement (ligne de commande).
    pending_open: Vec<PathBuf>,
    title_dirty: bool,
    theme: Theme,
    text: Option<TextRenderer>,
    /// Rasteriseur des icônes d'interface.
    raster: Rasterizer,
    /// Barre d'outils.
    toolbar: Toolbar,
    /// Liste du zoom déroulée, et d'où (voir `viewer/zoom.rs`).
    zoom_menu: Option<(crate::ui::zoompicker::ZoomPicker, ZoomAnchor)>,
    /// Fraction de cran de Ctrl+molette pas encore convertie en zoom.
    zoom_wheel: f32,
    /// Indications cliquables de la barre d'état au dernier dessin, en
    /// coordonnées de la fenêtre (voir `viewer/status.rs`).
    status_hits: Vec<(StatusSeg, (i32, i32, i32, i32))>,
    /// Indication de la barre d'état sous le pointeur.
    status_hover: Option<StatusSeg>,
    /// Recherche en cours (Ctrl+F).
    search: Option<Search>,
    /// Dernière recherche fermée, requête et options : F3 la relance, et
    /// Ctrl+F la propose de nouveau.
    last_search: Option<(String, SearchOptions)>,
    /// Sélection de texte.
    selection: Option<Selection>,
    /// Un glisser de sélection est en cours.
    sel_dragging: bool,
    /// Dernier clic gauche : instant, position, nombre de clics de la série.
    last_click: Option<(Instant, i32, i32, u8)>,
    /// Invite modale en cours (mot de passe, texte d'une note).
    prompt: Option<Prompt>,
    /// Dernière position de la souris dans la vue (pour poser une note).
    last_mouse: Option<(i32, i32)>,
    /// Documents ouverts qui ne sont pas à l'écran, dans l'ordre des onglets ;
    /// le document actif (`loaded`) occupe la position `active_tab`.
    others: Vec<Loaded>,
    /// Position de l'onglet actif parmi tous les onglets.
    active_tab: usize,
    /// Barre d'onglets.
    tabs: Tabs,
    /// Palette de commandes ouverte.
    palette: Option<Palette>,
    /// Questions et messages en attente de réponse ; le dernier est affiché.
    dialogs: Vec<Asking>,
    /// Outil « modifier » : objets de la page courante et sélection.
    objects: Option<ObjectTool>,
    /// Média en cours de lecture, s'il y en a un.
    media: Option<MediaView>,
    /// Modèle 3D activé, s'il y en a un.
    three_d: Option<three_d::Active3d>,
    /// Fiche « Paramètres », ouverte par-dessus tout.
    settings: Option<SettingsSheet>,
    /// La barre d'espace vient de presser un bouton de carte : le caractère
    /// qu'elle envoie ensuite n'est plus à personne (voir `handle_event`).
    swallow_space: bool,
    /// Issue de la dernière recherche de mise à jour : `Ok` si Acrux est à
    /// jour, l'erreur sinon (une version trouvée va dans `update_found`).
    update_outcome: Option<Result<(), String>>,
    /// Résultat de la recherche de mise à jour en cours, s'il y en a une.
    update_rx: Option<std::sync::mpsc::Receiver<Result<crate::update::Release, String>>>,
    /// Version plus récente trouvée, en attente que l'utilisateur en décide.
    update_found: Option<crate::update::Release>,
    /// Vrai si la recherche en cours a été demandée par l'utilisateur.
    update_asked: bool,
    /// La recherche du démarrage a déjà été lancée.
    update_started: bool,
    /// Outil « remplir et signer » : la barre est affichée quand il est actif.
    sign_panel: Option<SignPanel>,
    /// Vignettes de la première page des documents récents, calculées une à
    /// une pendant que l'écran d'accueil est affiché. `None` = illisible.
    welcome_thumbs: HashMap<PathBuf, (Option<Bitmap>, Instant)>,
    /// Zone du bouton « Ouvrir un document ».
    welcome_open: Option<(i32, i32, i32, i32)>,
    /// Zone du lien « Vider l'historique » de l'écran d'accueil.
    welcome_clear: Option<(i32, i32, i32, i32)>,
    /// L'accueil est affiché par-dessus le document ouvert, qui reste dans
    /// son onglet : c'est un retour à la base, pas une fermeture.
    home: bool,
    /// Défilement de l'accueil, quand les cartes dépassent.
    welcome_scroll: f64,
    /// Hauteur occupée par l'accueil au dernier dessin.
    welcome_height: f64,
    /// La décoration de la fenêtre a déjà été accordée au thème.
    frame_themed: bool,
    /// Horloge des animations.
    clock: Clock,
    /// Fil qui réveille la fenêtre pendant qu'une animation tourne.
    ticker: Option<Arc<AtomicBool>>,
    /// Vrai tant qu'il y a du mouvement : le fil ne réveille que dans ce cas.
    anim_flag: Arc<AtomicBool>,
    /// Position visée par le défilement en cours, s'il glisse.
    scroll_goal_y: Option<f64>,
    /// Position connue du défilement : ce qui s'en écarte sans passer par le
    /// glissement est un saut demandé ailleurs (aller à une page, zoomer), et
    /// met fin au glissement en cours.
    scroll_seen: f64,
    /// Largeur affichée du panneau de gauche.
    left_anim: Anim,
    /// Largeur affichée de la colonne d'outils, à droite.
    tools_anim: Anim,
    /// Place de la signature qu'on est en train de refaire, s'il y en a une.
    replacing: Option<usize>,
    /// Tracé au stylo en cours, en coordonnées de page.
    inking: Option<Inking>,
    /// Élément posé sélectionné : on peut le déplacer et le redimensionner,
    /// comme n'importe quel objet.
    placed: Option<PlacedSel>,
    /// Une signature est « prise » dans le panneau : la lâcher sur la page
    /// la pose là. C'est l'autre geste attendu, à côté du choix puis du clic.
    carrying: bool,
    /// Page en cours de modification **en direct** (un déplacement).
    ///
    /// Pendant un geste, la page est rendue ici même, à chaque étape. Le fil
    /// de rendu, lui, travaille sur une copie qui a un temps de retard : ses
    /// images montreraient l'élément à sa position précédente, et la page
    /// clignoterait entre les deux. On les écarte donc le temps du geste.
    live_edit: Option<usize>,
    /// Fenêtre de capture d'une signature, ouverte par-dessus tout.
    capture: Option<Capture>,
    /// Fenêtre « Protéger par mot de passe », ouverte par-dessus tout. Elle
    /// vise le document actif : elle se ferme dès qu'il change.
    protect: Option<crate::ui::protect::ProtectDialog>,
    /// Signature enregistrée, conservée entre deux sessions.
    signatures: Vec<Saved>,
    /// Paraphe enregistré.
    initials: Option<Saved>,
    /// Message passager affiché dans la barre d'état (fin d'export…).
    notice: Option<(String, std::time::Instant)>,
    /// Info-bulle du bouton survolé (elle n'apparaît qu'après `TIP_DELAY`).
    tip: Option<Tip>,
    /// Zone qui tient le focus clavier.
    region: Region,
    /// Mode lecture : tout le décor est masqué, il ne reste que les pages.
    reading: bool,
    /// Disposition des pages.
    view_mode: ViewMode,
    /// Première page seule en mode deux pages.
    two_up_cover: bool,
    /// Page de référence dans les modes « une rangée à la fois ».
    anchor: usize,
    /// Réglages persistants (thème, mode, fichiers récents).
    prefs: Prefs,
    /// Rectangles cliquables des fichiers récents au dernier dessin.
    recent_hits: Vec<(i32, i32, i32, i32)>,
    /// Plein écran (barres masquées).
    fullscreen: bool,
    /// Champ de formulaire ayant le focus clavier : rang du champ dans
    /// `fields` et rang du widget dans le champ.
    focus_field: Option<(usize, usize)>,
    /// Champ de formulaire en cours de saisie (voir `viewer/formfill.rs`).
    field_edit: Option<formfill::FieldEdit>,
    /// Liste déroulante d'un champ de formulaire, ouverte.
    field_menu: Option<formfill::FieldMenu>,
    /// Barre d'un document à remplir.
    form_bar: FormBar,
    /// Police de la saisie dans un champ, de mêmes largeurs que celle de
    /// l'apparence, et sa famille.
    field_font: Option<(acrux_features::forms::FontFamily, TextRenderer)>,
    /// Dernière ligne cliquée d'une liste de choix, et son champ : le point
    /// de départ d'un Maj+clic.
    list_anchor: Option<(String, usize)>,
    /// Mode « Modifier le PDF », quand il est actif.
    edit: Option<EditMode>,
    /// Outil d'annotation en cours.
    annot_tool: Option<AnnotTool>,
    /// Barre des commentaires ouverte. Elle survit à l'outil : l'éteindre
    /// ramène à la sélection, et un outil choisi ensuite s'applique à ce
    /// qui est sélectionné, comme dans Acrobat. « Terminer » ou Échap la
    /// ferment.
    comment_bar: bool,
    /// Barre de l'outil d'annotation.
    mode_bar: ModeBar,
    /// Barre des outils, à droite, affichée.
    tools_open: bool,
    /// Barre des outils.
    tools: ToolsPanel,
    /// Panneau latéral affiché.
    panel_open: bool,
    /// Panneau latéral (vignettes, signets).
    panel: Panel,
    /// Poignée de réveil de la fenêtre, pour poursuivre une recherche en
    /// cours sans bloquer la boucle d'événements.
    waker: Option<Box<dyn Waker>>,
    /// Page courante au dernier dessin (pour suivre dans les vignettes).
    last_current: usize,
    /// Clé de cache des vignettes au dernier dessin.
    thumb_key: u32,
    /// Menu du clic droit, quand il est ouvert.
    context_menu: Option<ContextMenu>,
    /// Ce que vise la commande en cours : la page, l'onglet ou le document
    /// récent sur lequel on a fait un clic droit. `Target::Current` hors
    /// d'un menu (voir `run_on`).
    command_target: Target,
}

/// Vrai si ces octets sont ceux d'une image que nous savons lire.
///
/// La signature vaut mieux que l'extension : un fichier mal nommé s'ouvre
/// quand même, et un PDF nommé `.png` reste un PDF.
fn is_image(data: &[u8]) -> bool {
    data.starts_with(&[0x89, b'P', b'N', b'G'])
        || data.starts_with(&[0xFF, 0xD8])
        || data.starts_with(b"BM")
        || data.starts_with(b"GIF8")
        || data.starts_with(b"II* ")
        || data.starts_with(b"MM *")
}

impl Viewer {
    /// Nouveau visualiseur, avec les fichiers à ouvrir au démarrage (un
    /// onglet chacun).
    #[must_use]
    // Un champ par ligne : l'état de départ se lit d'un seul bloc, dans
    // l'ordre de la structure.
    #[allow(clippy::too_many_lines)]
    pub fn new(initial: Vec<PathBuf>) -> Self {
        let prefs = Prefs::load();
        // La langue avant tout le reste : ce qui se construit ensuite peut
        // déjà avoir des libellés.
        lang::apply(Lang::from_key(&prefs.language), system_lang());
        let signatures: Vec<Saved> = prefs
            .signatures
            .iter()
            .filter_map(|s| Saved::decode(s))
            .collect();
        let initials = prefs.initials.as_deref().and_then(Saved::decode);
        Self {
            loaded: None,
            error: None,
            width: 0,
            height: 0,
            dpi_scale: 1.0,
            window_max: prefs.window_max,
            scroll_y: 0.0,
            scroll_x: 0.0,
            drag_last: None,
            pending_open: initial,
            title_dirty: true,
            theme: if prefs.dark_theme && !std::env::var("ACRUX_THEME").is_ok_and(|v| v == "light")
            {
                Theme::dark()
            } else {
                Theme::light()
            },
            zoom: prefs.zoom,
            fit: prefs.fit,
            edit: None,
            annot_tool: None,
            comment_bar: false,
            mode_bar: ModeBar::default(),
            tools_open: prefs.tools_open,
            tools: ToolsPanel::new(),
            panel_open: prefs.panel_open,
            view_mode: prefs.view_mode,
            two_up_cover: prefs.two_up_cover,
            prefs,
            text: TextRenderer::system(),
            raster: Rasterizer::new(),
            toolbar: Toolbar::new(),
            zoom_menu: None,
            zoom_wheel: 0.0,
            status_hits: Vec::new(),
            status_hover: None,
            search: None,
            last_search: None,
            selection: None,
            sel_dragging: false,
            last_click: None,
            prompt: None,
            last_mouse: None,
            others: Vec::new(),
            active_tab: 0,
            tabs: Tabs::new(),
            palette: None,
            dialogs: Vec::new(),
            objects: None,
            media: None,
            three_d: None,
            settings: None,
            swallow_space: false,
            update_outcome: None,
            update_rx: None,
            update_found: None,
            update_asked: false,
            update_started: false,
            sign_panel: None,
            welcome_thumbs: HashMap::new(),
            welcome_open: None,
            welcome_clear: None,
            home: false,
            welcome_scroll: 0.0,
            welcome_height: 0.0,
            frame_themed: false,
            clock: Clock::default(),
            ticker: None,
            anim_flag: Arc::new(AtomicBool::new(false)),
            scroll_goal_y: None,
            scroll_seen: 0.0,
            left_anim: Anim::new(0.0, 0.16),
            tools_anim: Anim::new(0.0, 0.16),
            replacing: None,
            inking: None,
            placed: None,
            carrying: false,
            live_edit: None,
            capture: None,
            protect: None,
            signatures,
            initials,
            notice: None,
            tip: None,
            region: Region::Document,
            reading: false,
            waker: None,
            anchor: 0,
            recent_hits: Vec::new(),
            fullscreen: false,
            focus_field: None,
            field_edit: None,
            field_menu: None,
            form_bar: FormBar::default(),
            field_font: None,
            list_anchor: None,
            panel: Panel::new(),
            last_current: usize::MAX,
            thumb_key: 0,
            context_menu: None,
            command_target: Target::Current,
        }
    }

    /// Page sous un point de la fenêtre et coordonnées de ce point dans
    /// l'espace de la page. Hors de toute page, prend la page la plus proche
    /// verticalement (pour prolonger une sélection au-delà des marges).
    #[allow(clippy::many_single_char_names)] // coordonnées et matrices
    fn page_at(&self, x: i32, y: i32) -> Option<(usize, Point)> {
        let l = self.loaded.as_ref()?;
        let layout = self.layout();
        let (vx, vy) = (f64::from(x) + self.scroll_x, f64::from(y) + self.scroll_y);
        // Page dont la boîte est la plus proche du point (distance nulle si
        // le point est dedans) : en deux pages, l'abscisse départage.
        let mut best: Option<(f64, usize)> = None;
        for (i, b) in layout.iter().enumerate() {
            if !b.visible {
                continue;
            }
            let dx = (f64::from(b.x) - vx)
                .max(vx - f64::from(b.x + b.w as i32))
                .max(0.0);
            let dy = (f64::from(b.y) - vy)
                .max(vy - f64::from(b.y + b.h as i32))
                .max(0.0);
            let d = dx.hypot(dy);
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, i));
            }
        }
        let (_, page) = best?;
        let (ox, oy) = self.page_screen(&layout, page)?;
        let PageBox { w, h, .. } = layout[page];
        let p = &l.pages[page];
        let m = base_matrix(
            &p.crop_box(&l.doc),
            self.scale(),
            shown_rotation(l, p),
            w,
            h,
        );
        let inv = m.invert()?;
        let pt = inv.apply(Point::new(f64::from(x) - ox, f64::from(y) - oy));
        Some((page, pt))
    }

    /// Position textuelle sous un point de la fenêtre, et si elle est sur du texte.
    fn text_pos_at(&mut self, x: i32, y: i32) -> Option<(TextPos, bool)> {
        let (page, pt) = self.page_at(x, y)?;
        let l = self.loaded.as_mut()?;
        let hit = l.text(page).1.hit(pt)?;
        Some((
            TextPos {
                page,
                caret: hit.caret,
            },
            hit.on_text,
        ))
    }

    /// Texte de la sélection courante (pages séparées par un saut de ligne).
    fn selected_text(&mut self) -> String {
        let Some(sel) = self.selection else {
            return String::new();
        };
        let Some(l) = &mut self.loaded else {
            return String::new();
        };
        let (s, e) = sel.ordered();
        let mut out = String::new();
        for page in s.page..=e.page.min(l.pages.len().saturating_sub(1)) {
            let text = &l.text(page).1;
            if let Some((a, b)) = sel.range_on_page(page, text.len()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&text.text(a, b));
            }
        }
        out
    }

    /// Sélectionne tout le texte du document.
    fn select_all(&mut self) {
        let Some(l) = &mut self.loaded else { return };
        let last = l.pages.len().saturating_sub(1);
        let len = l.text(last).1.len();
        self.selection = Some(Selection {
            anchor: TextPos { page: 0, caret: 0 },
            focus: TextPos {
                page: last,
                caret: len,
            },
        });
    }

    /// Lien sous un point de la fenêtre (coordonnées de vue).
    fn link_at(&mut self, x: i32, y: i32) -> Option<Link> {
        let (page, pt) = self.page_at(x, y)?;
        let l = self.loaded.as_mut()?;
        l.links(page)
            .iter()
            .find(|link| link.rect.contains(pt))
            .cloned()
    }

    /// Fait défiler jusqu'à une destination.
    #[allow(clippy::many_single_char_names)] // coordonnées et matrices
    fn go_to(&mut self, dest: &Destination) {
        let Some(l) = &self.loaded else { return };
        let Some(page) = l.pages.get(dest.page) else {
            return;
        };
        if matches!(dest.view, View::FitWidth { .. }) {
            self.fit = Fit::Width;
        }
        if self.view_mode.is_paged() {
            self.anchor = dest.page;
        }
        let layout = self.layout();
        let Some(&PageBox { y, w, h, .. }) = layout.get(dest.page) else {
            return;
        };
        // Ordonnée PDF à placer en haut de la vue, si la destination en donne une.
        let top = match &dest.view {
            View::Xyz { top, .. } | View::FitWidth { top } => *top,
            View::FitRect(r) => Some(r.y1),
            _ => None,
        };
        let m = base_matrix(
            &page.crop_box(&l.doc),
            self.scale(),
            shown_rotation(l, page),
            w,
            h,
        );
        let offset = top.map_or(0.0, |t| {
            let p = m.apply(Point::new(0.0, t));
            p.y.clamp(0.0, f64::from(h))
        });
        self.scroll_y = f64::from(y) - f64::from(GAP) + offset;
        self.clamp_scroll();
    }

    /// Exécute l'action d'un lien.
    fn follow(&mut self, action: &Action, window: &mut dyn WindowHandle) {
        log_line(&format!("lien suivi : {action:?}"));
        // Un lien, un signet ou une action nommée est un saut : l'endroit
        // quitté entre dans l'historique de la vue (Alt+← y ramène).
        match action {
            Action::GoTo(d) => self.navigate(|v| v.go_to(d)),
            Action::Uri(u) => window.open_url(u),
            Action::Named(n) => {
                let last = self
                    .loaded
                    .as_ref()
                    .map_or(0, |l| l.pages.len().saturating_sub(1));
                let page = match n.as_str() {
                    "NextPage" => Some(self.current_page() + 1),
                    "PrevPage" => Some(self.current_page().saturating_sub(1)),
                    "FirstPage" => Some(0),
                    "LastPage" => Some(last),
                    _ => None,
                };
                if let Some(page) = page {
                    self.navigate(|v| v.scroll_to_page(page));
                }
            }
            Action::Launch(_) | Action::GoToRemote(_) | Action::Unsupported(_) => {}
        }
    }

    /// Clic gauche dans le document : sélection (texte sous le pointeur) ou
    /// début de déplacement.
    fn mouse_down(&mut self, x: i32, y: i32, clicks: u8, shift: bool) {
        let series = self.click_series(x, y, clicks);
        let Some((pos, on_text)) = self.text_pos_at(x, y) else {
            self.selection = None;
            self.drag_last = Some((x, y));
            return;
        };
        if shift {
            if let Some(sel) = &mut self.selection {
                sel.focus = pos;
                self.sel_dragging = true;
                return;
            }
        }
        if series >= 2 {
            let range = self.loaded.as_mut().map(|l| {
                let t = &l.text(pos.page).1;
                if series == 2 {
                    t.word_at(pos.caret)
                } else {
                    t.line_at(pos.caret)
                }
            });
            if let Some((a, b)) = range {
                self.selection = Some(Selection {
                    anchor: TextPos {
                        page: pos.page,
                        caret: a,
                    },
                    focus: TextPos {
                        page: pos.page,
                        caret: b,
                    },
                });
            }
            return;
        }
        if on_text {
            self.selection = Some(Selection::collapsed(pos));
            self.sel_dragging = true;
        } else {
            self.selection = None;
            self.drag_last = Some((x, y));
        }
    }

    /// Surligne la sélection.
    #[allow(clippy::many_single_char_names)] // coordonnées et matrices
    fn paint_selection(&mut self, frame: &mut Frame<'_>) {
        let Some(sel) = self.selection else { return };
        if sel.is_empty() {
            return;
        }
        let accent = self.theme.accent;
        let layout = self.layout();
        let view_h = self.view_height() as i32;
        let scale = self.scale();
        let (s, e) = sel.ordered();
        let mut boxes: Vec<(i32, i32, i32, i32)> = Vec::new();
        let origins: Vec<Option<(f64, f64)>> = (0..layout.len())
            .map(|i| self.page_screen(&layout, i))
            .collect();
        {
            let Some(l) = &mut self.loaded else { return };
            for page in s.page..=e.page.min(l.pages.len().saturating_sub(1)) {
                let Some(&PageBox { w, h, .. }) = layout.get(page) else {
                    continue;
                };
                let Some(Some((ox, top))) = origins.get(page).copied() else {
                    continue;
                };
                if top + f64::from(h) < 0.0 || top > f64::from(view_h) {
                    continue;
                }
                let p = &l.pages[page];
                let m = base_matrix(&p.crop_box(&l.doc), scale, shown_rotation(l, p), w, h);
                let text = &l.text(page).1;
                let Some((a, b)) = sel.range_on_page(page, text.len()) else {
                    continue;
                };
                for r in text.rects(a, b) {
                    let dev = m.transform_rect(&r);
                    boxes.push((
                        (ox + dev.x0).round() as i32,
                        (top + dev.y0).round() as i32,
                        dev.width().round().max(1.0) as i32,
                        dev.height().round().max(1.0) as i32,
                    ));
                }
            }
        }
        for (x, y, w, h) in boxes {
            if y + h < 0 || y > view_h {
                continue;
            }
            fill_rect_blend(frame, x, y, w, (h).min(view_h - y), accent, 90);
        }
    }

    /// Écran d'accueil : titre, rappel des raccourcis et documents récents
    /// cliquables. Dessiné dans la zone de document quand rien n'est ouvert.
    #[allow(clippy::too_many_lines)] // une mise en page, lue de haut en bas
    fn paint_welcome(&mut self, frame: &mut Frame<'_>) {
        let t = self.theme;
        let dpi = self.dpi_scale as f32;
        let hover = self.last_mouse;
        let recent: Vec<(PathBuf, String, String)> = self
            .prefs
            .recent
            .iter()
            .take(MAX_WELCOME)
            .map(|p| {
                (
                    p.clone(),
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    describe_file(p),
                )
            })
            .collect();
        let thumbs = std::mem::take(&mut self.welcome_thumbs);
        let mut hits: Vec<(i32, i32, i32, i32)> = Vec::new();
        let Some(text) = &mut self.text else {
            self.recent_hits.clear();
            self.welcome_open = None;
            self.welcome_clear = None;
            self.welcome_thumbs = thumbs;
            return;
        };
        let size = t.font_size * dpi;
        let pad = (56.0 * dpi) as i32;
        let x = pad;
        let mut y = (54.0 * dpi) as i32 - self.welcome_scroll as i32;
        let title = size * 2.4;
        text.draw(
            frame,
            x as f32,
            y as f32 + text.ascent(title),
            title,
            "Acrux",
            t.text,
        );
        y += (title * 1.25) as i32;
        text.draw(
            frame,
            x as f32,
            y as f32 + text.ascent(size),
            size,
            lang::tr("Lire, modifier, remplir et signer un PDF."),
            t.text_dim,
        );
        y += (size * 2.6) as i32;
        // Le geste principal : un bouton, pas un raccourci à retenir.
        let label = lang::tr("Ouvrir un document…");
        let bw = (text.measure(size, label) + 40.0 * dpi) as i32;
        let bh = (38.0 * dpi) as i32;
        let over_open =
            hover.is_some_and(|(mx, my)| mx >= x && mx < x + bw && my >= y && my < y + bh);
        let bg = if over_open {
            (
                t.accent.0.saturating_add(18),
                t.accent.1.saturating_add(18),
                t.accent.2.saturating_add(10),
            )
        } else {
            t.accent
        };
        round_rect(frame, x, y, bw, bh, 10.0 * dpi, bg);
        text.draw(
            frame,
            (x + (20.0 * dpi) as i32) as f32,
            y as f32 + f32::midpoint(bh as f32, text.ascent(size)) - 1.0,
            size,
            label,
            (255, 255, 255),
        );
        let open_hit = Some((x, y, bw, bh));
        text.draw(
            frame,
            (x + bw + (18.0 * dpi) as i32) as f32,
            y as f32 + f32::midpoint(bh as f32, text.ascent(size)) - 1.0,
            size,
            lang::tr("ou déposez un PDF sur la fenêtre · Ctrl+Maj+P pour toutes les commandes"),
            t.text_dim,
        );
        y += bh + (34.0 * dpi) as i32;
        if recent.is_empty() {
            self.welcome_clear = None;
            self.recent_hits = hits;
            self.welcome_open = open_hit;
            self.welcome_thumbs = thumbs;
            return;
        }
        text.draw(
            frame,
            x as f32,
            y as f32 + text.ascent(size),
            size,
            lang::tr("Documents récents"),
            t.text_dim,
        );
        // Vider l'historique : un lien discret au bout de l'intertitre, qui
        // ne prend sa couleur qu'au survol — on ne l'actionne pas tous les
        // jours, il n'a pas à concurrencer « Ouvrir un document ».
        {
            let heading_w = text.measure(size, lang::tr("Documents récents")) as i32;
            let label = lang::tr("Vider l'historique");
            let lw = text.measure(size, label) as i32;
            let lx = x + heading_w + (22.0 * dpi) as i32;
            let (hy, hh) = (
                y - (6.0 * dpi) as i32,
                (size * 1.2) as i32 + (12.0 * dpi) as i32,
            );
            let hx = lx - (10.0 * dpi) as i32;
            let hw = lw + (20.0 * dpi) as i32;
            let over =
                hover.is_some_and(|(mx, my)| mx >= hx && mx < hx + hw && my >= hy && my < hy + hh);
            if over {
                round_rect(frame, hx, hy, hw, hh, 7.0 * dpi, t.hover);
            }
            text.draw(
                frame,
                lx as f32,
                y as f32 + text.ascent(size),
                size,
                label,
                if over { t.text } else { t.accent },
            );
            self.welcome_clear = Some((hx, hy, hw, hh));
        }
        y += (size * 2.0) as i32;
        // Une grille de cartes : la vignette dit de quel document il s'agit
        // bien plus vite que son nom de fichier.
        let card_w = (186.0 * dpi) as i32;
        let thumb_h = (148.0 * dpi) as i32;
        let card_h = thumb_h + (58.0 * dpi) as i32;
        let gap = (18.0 * dpi) as i32;
        let columns = ((frame.width as i32 - 2 * pad + gap) / (card_w + gap)).max(1);
        for (index, (path, name, meta)) in recent.iter().enumerate() {
            let col = index as i32 % columns;
            let row = index as i32 / columns;
            let cx = x + col * (card_w + gap);
            let cy = y + row * (card_h + gap);
            if cy + card_h > frame.height as i32 {
                break;
            }
            let over = hover.is_some_and(|(mx, my)| {
                mx >= cx && mx < cx + card_w && my >= cy && my < cy + card_h
            });
            if over {
                round_rect(
                    frame,
                    cx - (8.0 * dpi) as i32,
                    cy - (8.0 * dpi) as i32,
                    card_w + (16.0 * dpi) as i32,
                    card_h + (16.0 * dpi) as i32,
                    12.0 * dpi,
                    t.hover,
                );
            }
            // La page, sur son fond blanc et son ombre, comme dans la vue.
            let radius = 8.0 * dpi;
            // Une ombre large et légère : la carte flotte, elle n'est pas
            // cernée.
            shadow(
                frame,
                cx,
                cy + (5.0 * dpi) as i32,
                card_w,
                thumb_h,
                radius,
                18.0 * dpi,
                if over { 0.32 } else { 0.18 },
            );
            round_rect(frame, cx, cy, card_w, thumb_h, radius, (0xFF, 0xFF, 0xFF));
            match thumbs.get(path) {
                Some((Some(bitmap), since)) => {
                    let bw = bitmap.width() as i32;
                    let bh = bitmap.height() as i32;
                    frame.blit_rgba_premultiplied(
                        cx + (card_w - bw) / 2,
                        cy + (thumb_h - bh) / 2,
                        bitmap.width(),
                        bitmap.height(),
                        bitmap.data(),
                    );
                    // Fondu à l'arrivée : la vignette naît du blanc de la
                    // page plutôt que d'apparaître d'un coup.
                    #[allow(clippy::cast_possible_truncation)]
                    let fade = ease_out(since.elapsed().as_secs_f64() / 0.35) as f32;
                    if fade < 1.0 {
                        round_rect_alpha(
                            frame,
                            cx,
                            cy,
                            card_w,
                            thumb_h,
                            radius,
                            (0xFF, 0xFF, 0xFF),
                            1.0 - fade,
                        );
                    }
                }
                Some((None, _)) => {
                    let label = "illisible";
                    let w = text.measure(size, label);
                    text.draw(
                        frame,
                        cx as f32 + (card_w as f32 - w) / 2.0,
                        cy as f32 + thumb_h as f32 / 2.0,
                        size,
                        label,
                        (0x90, 0x94, 0x9C),
                    );
                }
                None => {
                    // Pas encore calculée : quelques lignes grises, le temps
                    // que la vignette arrive.
                    for line in 0..4 {
                        let ly = cy + (24.0 * dpi) as i32 + line * (16.0 * dpi) as i32;
                        let lw = card_w - (40.0 * dpi) as i32 - line * (10.0 * dpi) as i32;
                        frame.fill_rect(
                            cx + (20.0 * dpi) as i32,
                            ly,
                            lw.max(20),
                            (6.0 * dpi) as i32,
                            0xEC,
                            0xEE,
                            0xF2,
                        );
                    }
                }
            }
            round_rect_outline(
                frame,
                cx,
                cy,
                card_w,
                thumb_h,
                radius,
                dpi.max(1.0),
                t.separator,
            );
            let baseline = (cy + thumb_h) as f32 + text.ascent(size) + 12.0 * dpi;
            text.draw_clipped(
                frame,
                cx as f32,
                baseline,
                size,
                name,
                t.text,
                card_w as f32,
            );
            text.draw_clipped(
                frame,
                cx as f32,
                baseline + text.ascent(size) + 8.0 * dpi,
                size * 0.88,
                meta,
                t.text_dim,
                card_w as f32,
            );
            hits.push((cx, cy, card_w, card_h));
        }
        self.welcome_height = f64::from(y + card_h + 2 * pad) + self.welcome_scroll;
        self.recent_hits = hits;
        self.welcome_open = open_hit;
        self.welcome_thumbs = thumbs;
    }

    /// Vrai s'il reste des vignettes d'accueil à calculer.
    fn welcome_pending(&self) -> bool {
        if !self.showing_home() {
            return false;
        }
        let missing = self
            .prefs
            .recent
            .iter()
            .take(MAX_WELCOME)
            .any(|p| !self.welcome_thumbs.contains_key(p));
        // Un fondu en cours compte aussi : il faut repeindre tant qu'il dure.
        missing
            || self
                .welcome_thumbs
                .values()
                .any(|(_, since)| since.elapsed().as_secs_f64() < 0.4)
    }

    /// Calcule une vignette manquante, s'il en reste une. Rend vrai s'il
    /// faudra repasser : une par réveil, pour que la fenêtre reste vive.
    fn step_welcome_thumbs(&mut self) -> bool {
        if !self.showing_home() {
            return false;
        }
        let Some(path) = self
            .prefs
            .recent
            .iter()
            .take(MAX_WELCOME)
            .find(|p| !self.welcome_thumbs.contains_key(*p))
            .cloned()
        else {
            return false;
        };
        let scale = f64::from(self.dpi_scale as f32);
        let thumb = render_thumbnail(&path, 186.0 * scale, 148.0 * scale);
        self.welcome_thumbs.insert(path, (thumb, Instant::now()));
        true
    }

    /// Fin d'une image : une vignette de l'accueil, s'il en manque, puis
    /// **un** réveil s'il reste du travail par tranches (`searching` : la
    /// tranche de recherche prise avant de peindre n'a pas tout parcouru).
    /// C'est la peinture qui relance, jamais la boucle d'événements (voir la
    /// fin de `handle_event`) : la fenêtre se repeint entre deux pas.
    fn after_paint(&mut self, searching: bool) {
        let thumbs = self.step_welcome_thumbs();
        if searching || thumbs {
            if let Some(w) = &self.waker {
                w.wake();
            }
        }
    }

    /// Onglet qui porte déjà ce fichier, s'il y en a un.
    fn tab_of(&self, path: &Path) -> Option<usize> {
        if self.loaded.as_ref().is_some_and(|l| l.path == path) {
            return Some(self.active_tab);
        }
        let at = self.others.iter().position(|l| l.path == path)?;
        Some(if at >= self.active_tab { at + 1 } else { at })
    }

    /// Ouvre la fiche « Paramètres ».
    fn open_settings(&mut self, window: &mut dyn WindowHandle) {
        self.settings = Some(SettingsSheet::new());
        log_line("paramètres : ouverts");
        window.request_redraw();
    }

    /// Applique ce que la fiche « Paramètres » demande. La langue et le thème
    /// changent sur-le-champ : la fiche, repeinte, les montre aussitôt.
    fn settings_action(&mut self, action: SettingsAction, window: &mut dyn WindowHandle) {
        match action {
            SettingsAction::Close => {
                self.settings = None;
                log_line("paramètres : fermés");
            }
            SettingsAction::Language(choice) => {
                if Lang::from_key(&self.prefs.language) != choice {
                    self.set_language(choice, window);
                }
                log_line(&format!("réglage : langue {}", choice.key()));
            }
            SettingsAction::Dark(dark) => {
                if dark != self.theme.is_dark() {
                    self.toggle_theme(window);
                }
                log_line(if dark {
                    "réglage : thème sombre"
                } else {
                    "réglage : thème clair"
                });
            }
            SettingsAction::AutoUpdates(on) => {
                self.set_auto_updates(on);
                log_line(if on {
                    "réglage : mises à jour au démarrage"
                } else {
                    "réglage : mises à jour jamais"
                });
            }
            SettingsAction::CheckNow => self.check_updates(true, window),
            SettingsAction::Install => {
                // La confirmation passe par-dessus la fiche, qu'on referme :
                // on ne revient pas à des réglages après avoir installé.
                self.settings = None;
                self.install_update(window);
            }
        }
        window.request_redraw();
    }

    /// Recherche des mises à jour au démarrage, ou jamais.
    fn set_auto_updates(&mut self, on: bool) {
        if self.prefs.check_updates == on {
            return;
        }
        self.prefs.check_updates = on;
        self.prefs.save();
        self.set_notice(if on {
            lang::tr("mises à jour : recherche au démarrage").into()
        } else {
            lang::tr("mises à jour : recherche désactivée").into()
        });
    }

    /// Les réglages en vigueur, tels que la fiche les montre. Ils sont lus
    /// champ par champ : la fiche, elle, reste libre d'être empruntée.
    fn settings_state<'a>(
        prefs: &'a Prefs,
        theme: &Theme,
        checking: bool,
        found: Option<&'a crate::update::Release>,
        outcome: Option<&'a Result<(), String>>,
    ) -> SettingsState<'a> {
        let update = if checking {
            UpdateLine::Checking
        } else if let Some(release) = found {
            UpdateLine::Available(&release.version)
        } else {
            match outcome {
                Some(Ok(())) => UpdateLine::UpToDate,
                Some(Err(why)) => UpdateLine::Failed(why),
                None => UpdateLine::Idle,
            }
        };
        SettingsState {
            language: Lang::from_key(&prefs.language),
            system: system_lang(),
            dark: theme.is_dark(),
            auto_updates: prefs.check_updates,
            update,
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    /// Peint la fiche « Paramètres », par-dessus la page.
    fn paint_settings(&mut self, frame: &mut Frame<'_>) {
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        let state = Self::settings_state(
            &self.prefs,
            &self.theme,
            self.update_rx.is_some(),
            self.update_found.as_ref(),
            self.update_outcome.as_ref(),
        );
        let (Some(sheet), Some(text)) = (self.settings.as_mut(), self.text.as_mut()) else {
            return;
        };
        sheet.paint(frame, text, &theme, dpi, &state);
    }

    /// Applique et retient une langue.
    fn set_language(&mut self, choice: Lang, window: &mut dyn WindowHandle) {
        self.prefs.language = choice.key().to_string();
        self.prefs.save();
        lang::apply(choice, system_lang());
        self.title_dirty = true;
        window.request_redraw();
    }

    /// Montre l'accueil, sans rien fermer.
    fn show_home(&mut self, window: &mut dyn WindowHandle) {
        self.home = !self.home || self.loaded.is_none();
        if self.home {
            // Les modes d'édition n'ont pas de sens sur l'accueil.
            self.edit = None;
            self.close_annot_tools();
            self.sign_panel = None;
            self.capture = None;
            self.protect = None;
            self.wake_anim();
            self.set_notice(lang::tr("Accueil").into());
        }
        self.title_dirty = true;
        window.request_redraw();
    }

    /// Vrai si l'accueil prend la place du document.
    fn showing_home(&self) -> bool {
        self.home || self.loaded.is_none()
    }

    /// Quitte l'accueil pour revenir au document ouvert.
    pub(super) fn leave_home(&mut self) {
        // L'info-bulle d'un document récent n'a plus lieu d'être.
        self.tip = None;
        if self.home {
            self.home = false;
            self.wake_anim();
            self.title_dirty = true;
        }
    }

    /// Vide la liste des documents récents — la liste seulement : les
    /// fichiers ne sont pas touchés.
    pub(super) fn clear_recent(&mut self) {
        self.prefs.recent.clear();
        self.prefs.save();
        self.welcome_thumbs.clear();
        self.recent_hits.clear();
        self.welcome_clear = None;
        self.welcome_scroll = 0.0;
        self.set_notice(lang::tr("Historique vidé").to_string());
    }

    /// Ouvre le document récent situé sous `(x, y)`, le cas échéant.
    fn click_recent(&mut self, x: i32, y: i32, window: &mut dyn WindowHandle) -> bool {
        if self
            .welcome_open
            .is_some_and(|(bx, by, bw, bh)| x >= bx && x < bx + bw && y >= by && y < by + bh)
        {
            if let Some(path) = window.open_file_dialog() {
                self.open(&path, window);
            }
            return true;
        }
        if self
            .welcome_clear
            .is_some_and(|(bx, by, bw, bh)| x >= bx && x < bx + bw && y >= by && y < by + bh)
        {
            // On demande avant : la liste ne se retrouve pas.
            self.confirm(
                lang::tr("Vider l'historique"),
                lang::tr(
                    "La liste des documents récents sera effacée. Les fichiers eux-mêmes ne sont pas touchés.",
                ),
                lang::tr("Vider"),
                Then::ClearRecent,
            );
            return true;
        }
        match self.recent_at(x, y) {
            Some(index) => self.open_recent(index, window),
            None => false,
        }
    }

    /// Carte de document récent sous un point de la vue : son rang dans la
    /// liste des récents.
    fn recent_at(&self, x: i32, y: i32) -> Option<usize> {
        self.recent_hits
            .iter()
            .position(|&(rx, ry, rw, rh)| x >= rx && x < rx + rw && y >= ry && y < ry + rh)
    }

    /// Ouvre le document récent de rang `index` ; faux s'il n'y en a pas.
    fn open_recent(&mut self, index: usize, window: &mut dyn WindowHandle) -> bool {
        let Some(path) = self.prefs.recent.get(index).cloned() else {
            return false;
        };
        // Déjà ouvert : on y retourne plutôt que d'en faire un second onglet.
        if let Some(tab) = self.tab_of(&path) {
            self.leave_home();
            self.select_tab(tab);
            window.request_redraw();
            return true;
        }
        if !path.exists() {
            self.prefs.recent.retain(|p| p != &path);
            self.prefs.save();
            self.alert(
                "Fichier introuvable",
                &format!("{}\n\nIl a été retiré de la liste.", path.display()),
            );
            return true;
        }
        self.open(&path, window);
        true
    }

    /// Ouvre la palette de commandes, les dernières employées en tête.
    fn open_palette(&mut self) {
        let recent: Vec<Command> = self
            .prefs
            .recent_commands
            .iter()
            .filter_map(|key| Command::from_key(key))
            .collect();
        // « Vue précédente » au début de l'historique ne ferait que dire
        // qu'il n'y en a pas : comme le clic droit la grise, la palette la
        // tait.
        let mut idle = Vec::new();
        if let Some(l) = &self.loaded {
            if !l.nav.can_back() {
                idle.push(Command::ViewBack);
            }
            if !l.nav.can_forward() {
                idle.push(Command::ViewForward);
            }
            // Effacer ou aplatir un formulaire qui n'existe pas : la palette
            // ne le propose pas.
            if l.fields.is_empty() {
                idle.extend([
                    Command::ToggleFieldHighlight,
                    Command::ResetForm,
                    Command::FlattenForm,
                ]);
            }
        }
        self.palette = Some(Palette::new(self.loaded.is_some(), &recent).without(&idle));
    }

    /// Lance une commande choisie dans la palette, et la retient parmi les
    /// récentes. Elle est retenue **avant** de s'exécuter : « Fermer
    /// l'onglet » ou « Accueil » changent l'état, mais doivent compter aussi.
    /// La colonne d'outils et la barre passent par `run_command` sans ce
    /// détour : un clic sur un outil n'est pas un usage de la palette.
    fn palette_pick(&mut self, command: Command, window: &mut dyn WindowHandle) {
        self.prefs.push_recent_command(command.key());
        self.save_prefs();
        self.run_command(command, window);
    }

    /// Molette, quand quelque chose est ouvert par-dessus le document ; vrai
    /// si elle a été prise. Une palette ou un sélecteur ouvert ne laisse
    /// **jamais** défiler ni zoomer (Ctrl+molette) le document derrière
    /// lui : la molette va à sa liste s'il en a une, sinon elle s'arrête là.
    ///
    /// Les dialogues, la fiche « Paramètres », « Protéger » et les invites
    /// la prennent déjà plus haut (`modal_event`) ; la liste des polices la
    /// fait défiler, le nuancier l'avale (`edit_popup_wheel`). Le menu du
    /// clic droit se referme à la molette, avant d'arriver ici
    /// (`context_menu_event`), et la liste déroulante d'un champ de
    /// formulaire défile avant (`field_menu_event`).
    fn overlay_wheel(&mut self, delta: f32) -> bool {
        if let Some(p) = &mut self.palette {
            p.wheel(delta);
            return true;
        }
        // La fenêtre de capture d'une signature : on y dessine, la page
        // derrière ne doit pas bouger d'un pixel.
        if self.capture.is_some() || !self.dialogs.is_empty() || self.prompt.is_some() {
            return true;
        }
        self.edit_popup_wheel(f64::from(delta))
    }

    /// Dessine la palette de commandes par-dessus tout le reste.
    fn paint_palette(&mut self, frame: &mut Frame<'_>) {
        if self.palette.is_none() {
            return;
        }
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        fill_rect_blend(
            frame,
            0,
            0,
            frame.width as i32,
            frame.height as i32,
            (0, 0, 0),
            110,
        );
        let Some(text) = &mut self.text else { return };
        if let Some(p) = &mut self.palette {
            p.paint(frame, text, &theme, dpi);
        }
    }

    /// Dessine l'invite modale par-dessus la vue, sur la carte commune.
    fn paint_prompt(&mut self, frame: &mut Frame<'_>) {
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        let (Some(p), Some(text)) = (self.prompt.as_mut(), self.text.as_mut()) else {
            return;
        };
        let content = PromptContent {
            title: &p.title,
            label: &p.label,
            input: &p.input,
            error: p.error.as_deref(),
        };
        p.card.paint(frame, text, &theme, dpi, &content);
    }

    /// Touche pendant une invite : Tab passe du champ aux boutons, Entrée
    /// presse celui qui a le focus, Échap annule.
    fn prompt_key(&mut self, key: Key, m: Modifiers, window: &mut dyn WindowHandle) {
        let act = self
            .prompt
            .as_mut()
            .and_then(|p| p.card.key(key, m.shift, &mut p.input, &mut p.error));
        self.prompt_act(act, window);
    }

    /// Ce que l'invite a demandé : valider, annuler, ou rien.
    fn prompt_act(&mut self, act: Option<PromptAct>, window: &mut dyn WindowHandle) {
        match act {
            Some(PromptAct::Submit) => self.prompt_submit(window),
            Some(PromptAct::Cancel) => {
                self.prompt = None;
                log_line("invite : annulée");
            }
            None => {}
        }
    }

    /// Valide l'invite : chaque sorte d'invite fait ce qu'on attend d'elle.
    #[allow(clippy::too_many_lines)] // une branche par sorte d'invite, à la suite
    fn prompt_submit(&mut self, window: &mut dyn WindowHandle) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        log_line("invite : validée");
        let value = prompt.input.value.clone();
        match &prompt.kind {
            PromptKind::Password { doc, .. } => {
                let pw = value.into_bytes();
                if doc.authenticate(&pw).is_ok() && !doc.needs_password() {
                    let Some(Prompt {
                        kind: PromptKind::Password { path, doc, pages },
                        ..
                    }) = self.prompt.take()
                    else {
                        return;
                    };
                    let _ = acrux_features::forms::prepare_display(&doc);
                    self.finish_open(path, doc, pages, Some(pw), window);
                    self.announce_restrictions();
                } else {
                    prompt.error = Some(lang::tr("Mot de passe incorrect").into());
                    prompt.input.clear();
                }
            }
            PromptKind::Markup {
                kind,
                zones,
                excerpts,
            } => {
                let (kind, zones, excerpts) = (*kind, zones.clone(), excerpts.clone());
                self.prompt = None;
                self.add_markup(kind, &zones, &excerpts, Some(&value));
            }
            PromptKind::Replace {
                page,
                quads,
                passage,
            } => {
                let (page, quads, passage) = (*page, quads.clone(), passage.clone());
                self.prompt = None;
                self.add_replace(page, quads, &passage, &value);
            }
            PromptKind::Insert { page, x, line } => {
                let (page, x, line) = (*page, *x, *line);
                self.prompt = None;
                if !value.trim().is_empty() {
                    let meta = AnnotMeta::fresh(author_name().as_deref());
                    self.add_annotations(
                        "Caret",
                        vec![AnnotItem {
                            page,
                            annotation: NewAnnotation::Caret {
                                x,
                                line,
                                contents: value,
                                color: CARET_COLOR,
                            },
                            meta,
                        }],
                    );
                }
            }
            PromptKind::Comb { page, cells } => {
                let (page, cells) = (*page, cells.clone());
                self.prompt = None;
                if !value.trim().is_empty() {
                    let rect = acrux_features::fillsign::boxes::bounds_of(&cells);
                    self.apply_fillsign(
                        page,
                        rect,
                        acrux_features::fillsign::Item::Comb { text: value, cells },
                    );
                    // Le texte posé n'est pas pris en main : on passe
                    // au peigne suivant.
                    self.placed = None;
                }
            }
            PromptKind::EditText {
                page,
                line,
                start,
                end,
            } => {
                let (page, line, start, end) = (*page, *line, *start, *end);
                self.prompt = None;
                if !value.is_empty() {
                    self.apply_edit(EditOp::EditText {
                        page,
                        line,
                        start,
                        end,
                        text: value,
                    });
                }
            }
            PromptKind::OwnerPassword { then } => {
                let then = *then;
                self.owner_password_entered(&value, then, window);
            }
            PromptKind::Note { page, x, y } => {
                let (page, x, y) = (*page, *x, *y);
                self.prompt = None;
                if !value.trim().is_empty() {
                    self.apply_edit(EditOp::annotate(
                        page,
                        NewAnnotation::Note {
                            x,
                            y,
                            contents: value,
                            color: [1.0, 0.85, 0.0],
                        },
                        author_name().as_deref(),
                    ));
                }
            }
        }
        // Une saisie refusée se reprend dans le champ, quel que soit le
        // bouton qui l'a validée.
        if let Some(p) = &mut self.prompt {
            if p.error.is_some() {
                p.card.set_focus(PromptFocus::Field, &mut p.input);
            }
        }
    }

    /// Ouvre l'invite d'édition du texte sélectionné, pré-remplie avec ce
    /// texte. La sélection doit tenir sur une seule ligne : un paragraphe
    /// entier se recompose (`reflow`), ce n'est pas la même opération.
    fn start_text_edit(&mut self, window: &mut dyn WindowHandle) {
        let Some(sel) = self.selection else { return };
        if sel.is_empty() {
            return;
        }
        let (start, end) = sel.ordered();
        if start.page != end.page {
            self.alert(
                "Modification impossible",
                "Sélectionnez du texte sur une seule ligne.",
            );
            return;
        }
        let page = start.page;
        let Some(l) = &mut self.loaded else { return };
        let text = &l.text(page).1;
        let Some((from, to)) = sel.range_on_page(page, text.len()) else {
            return;
        };
        let Some((line, first, last)) = text.line_range(from, to) else {
            self.alert(
                "Modification impossible",
                "Sélectionnez du texte sur une seule ligne.",
            );
            return;
        };
        let current = text.text(from, to);
        let mut input = TextInput::new(lang::tr("Nouveau texte"));
        input.set_value(&current);
        self.prompt = Some(Prompt::new(
            lang::tr("Modifier le texte"),
            lang::tr("Nouveau texte :").into(),
            input,
            PromptKind::EditText {
                page,
                line,
                start: first,
                end: last,
            },
        ));
        window.request_redraw();
    }

    /// Zones (page, rectangle) couvertes par la sélection, une par ligne.
    #[allow(clippy::many_single_char_names)] // bornes de sélection
    fn selection_zones(&mut self) -> Vec<(usize, Rect)> {
        let Some(sel) = self.selection else {
            return Vec::new();
        };
        if sel.is_empty() {
            return Vec::new();
        }
        let mut zones = Vec::new();
        let Some(loaded) = &mut self.loaded else {
            return zones;
        };
        let (start, end) = sel.ordered();
        let last_page = loaded.pages.len().saturating_sub(1);
        for page in start.page..=end.page.min(last_page) {
            let text = &loaded.text(page).1;
            let Some((from, to)) = sel.range_on_page(page, text.len()) else {
                continue;
            };
            for rect in text.rects(from, to) {
                zones.push((page, rect));
            }
        }
        zones
    }

    /// Texte sélectionné, page par page, les blancs et les retours à la
    /// ligne ramenés à une espace : ce que recopie un balisage sans
    /// commentaire (voir `add_markup`).
    fn selection_excerpts(&mut self) -> Vec<(usize, String)> {
        let Some(sel) = self.selection.filter(|s| !s.is_empty()) else {
            return Vec::new();
        };
        let Some(l) = &mut self.loaded else {
            return Vec::new();
        };
        let (s, e) = sel.ordered();
        let mut out = Vec::new();
        for page in s.page..=e.page.min(l.pages.len().saturating_sub(1)) {
            let text = &l.text(page).1;
            if let Some((a, b)) = sel.range_on_page(page, text.len()) {
                let excerpt = one_line(&text.text(a, b));
                if !excerpt.is_empty() {
                    out.push((page, excerpt));
                }
            }
        }
        out
    }

    /// Pose un balisage sur des zones : **une** annotation par page, une
    /// zone par ligne, et une seule modification pour tout le geste. Un
    /// passage de dix lignes est un seul commentaire dans le panneau, et se
    /// défait d'un seul Ctrl+Z. Le commentaire, s'il y en a un, va à la
    /// première page du passage : c'est là que le lecteur commence.
    ///
    /// Sans commentaire, l'annotation reçoit le texte balisé (`excerpts`),
    /// comme dans Acrobat (« copier le texte sélectionné dans les notes des
    /// surlignages, soulignements et barrés », actif par défaut) : le
    /// panneau des commentaires et les autres lecteurs montrent alors *ce
    /// qui* est souligné ou barré, au lieu d'une ligne « (sans texte) » par
    /// annotation.
    fn add_markup(
        &mut self,
        kind: MarkupKind,
        zones: &[(usize, Rect)],
        excerpts: &[(usize, String)],
        comment: Option<&str>,
    ) {
        let mut pages: Vec<(usize, Vec<Rect>)> = Vec::new();
        for &(page, rect) in zones {
            match pages.iter_mut().find(|(p, _)| *p == page) {
                Some((_, quads)) => quads.push(rect),
                None => pages.push((page, vec![rect])),
            }
        }
        let comment = comment
            .filter(|c| !c.trim().is_empty())
            .map(ToString::to_string);
        let author = author_name();
        let items = pages
            .into_iter()
            .enumerate()
            .map(|(i, (page, quads))| AnnotItem {
                page,
                annotation: NewAnnotation::Markup {
                    kind,
                    quads,
                    color: kind.default_color(),
                    contents: match &comment {
                        Some(c) if i == 0 => Some(c.clone()),
                        _ => excerpts
                            .iter()
                            .find(|(p, _)| *p == page)
                            .map(|(_, t)| t.clone()),
                    },
                },
                // Chaque annotation a son identifiant, tiré ici et non à
                // l'application (voir `AnnotItem`).
                meta: AnnotMeta::fresh(author.as_deref()),
            })
            .collect();
        self.add_annotations(kind.subtype(), items);
    }

    /// Pose des annotations en une seule modification, et le note au
    /// journal. Rend faux si rien n'a été posé.
    fn add_annotations(&mut self, what: &str, items: Vec<AnnotItem>) -> bool {
        if items.is_empty() {
            return false;
        }
        let n = items.len();
        let done = self.apply_edit(EditOp::Annotate { items });
        if done {
            log_line(&format!("{n} annotation(s) {what} posée(s)"));
        }
        done
    }

    /// Demande un commentaire, puis balise la sélection avec.
    fn markup_with_comment(&mut self, kind: MarkupKind, window: &mut dyn WindowHandle) {
        let zones = self.selection_zones();
        if zones.is_empty() {
            self.set_notice(lang::tr("sélectionnez d'abord du texte").into());
            return;
        }
        // Un titre entier par balisage : une phrase se traduit, un
        // assemblage de morceaux non.
        let title = match kind {
            MarkupKind::Highlight => "Surligner et commenter",
            MarkupKind::Underline => "Souligner et commenter",
            MarkupKind::StrikeOut => "Barrer et commenter",
            MarkupKind::Squiggly => "Souligner d'un trait ondulé et commenter",
        };
        // Avant la saisie, comme pour « Remplacer » : sur un document qui
        // interdit de commenter, le refus arrivait après la frappe, et le
        // commentaire tapé était perdu.
        if !self.require_right(crate::render_worker::Right::Annotate) {
            window.request_redraw();
            return;
        }
        let excerpts = self.selection_excerpts();
        self.prompt = Some(Prompt::new(
            lang::tr(title),
            lang::tr("Commentaire :").into(),
            TextInput::new(lang::tr("Votre remarque")),
            PromptKind::Markup {
                kind,
                zones,
                excerpts,
            },
        ));
        window.request_redraw();
    }

    /// Balise la sélection courante : une annotation par page, une zone
    /// par ligne.
    fn markup_selection(&mut self, kind: MarkupKind) {
        let zones = self.selection_zones();
        if zones.is_empty() {
            self.set_notice(lang::tr("sélectionnez d'abord du texte").into());
            return;
        }
        let excerpts = self.selection_excerpts();
        self.add_markup(kind, &zones, &excerpts, None);
    }

    /// Ouvre l'invite « Remplacer le texte » pour la sélection.
    ///
    /// Un remplacement tient sur une page : le signe et le barré forment un
    /// groupe, et un groupe ne franchit pas une page (§12.5.6.2).
    fn start_replace(&mut self) {
        let zones = self.selection_zones();
        let Some(&(page, _)) = zones.first() else {
            self.set_notice(lang::tr("sélectionnez d'abord du texte").into());
            return;
        };
        if zones.iter().any(|(p, _)| *p != page) {
            self.alert(
                lang::tr("Remplacement impossible"),
                lang::tr("Sélectionnez du texte sur une seule page."),
            );
            return;
        }
        // Avant la saisie : un refus après la frappe ferait perdre le texte.
        if !self.require_right(crate::render_worker::Right::Annotate) {
            return;
        }
        let quads = zones.into_iter().map(|(_, r)| r).collect();
        // Le passage remplacé, rappelé dans le libellé quand il est court :
        // on sait ce qu'on remplace sans aller le relire derrière la carte.
        let current = self.selected_text();
        let label = if !current.contains('\n') && (1..=40).contains(&current.chars().count()) {
            lang::trf("Remplacer « {} » par :", &[current.trim()])
        } else {
            lang::tr("Remplacer par :").into()
        };
        self.prompt = Some(Prompt::new(
            lang::tr("Remplacer le texte"),
            label,
            TextInput::new(lang::tr("Nouveau texte")),
            PromptKind::Replace {
                page,
                quads,
                passage: one_line(&current),
            },
        ));
    }

    /// Pose un remplacement : le passage barré et, au bout, le texte
    /// proposé. Sans texte, c'est un simple barré — « supprimer ce
    /// passage », en langage de relecture —, qui porte le passage comme
    /// tout balisage sans commentaire (voir `add_markup`).
    fn add_replace(&mut self, page: usize, quads: Vec<Rect>, passage: &str, text: &str) {
        let strike = MarkupKind::StrikeOut.default_color();
        let (what, annotation) = if text.trim().is_empty() {
            (
                "StrikeOut",
                NewAnnotation::Markup {
                    kind: MarkupKind::StrikeOut,
                    quads,
                    color: strike,
                    contents: Some(passage.to_string()).filter(|p| !p.is_empty()),
                },
            )
        } else {
            (
                "Replace",
                NewAnnotation::Replace {
                    quads,
                    text: text.to_string(),
                    strike,
                    caret: CARET_COLOR,
                },
            )
        };
        let meta = AnnotMeta::fresh(author_name().as_deref());
        self.add_annotations(
            what,
            vec![AnnotItem {
                page,
                annotation,
                meta,
            }],
        );
    }

    /// Ouvre l'invite « Insérer du texte » au point `(x, y)` de la vue.
    ///
    /// Le point doit tomber dans une ligne de texte : le signe se pose entre
    /// deux caractères, au plus près du clic, à la hauteur de la ligne.
    fn start_insert(&mut self, x: i32, y: i32) {
        // Une page sans texte (un scan) n'a pas d'interstice où insérer.
        let Some((pos, true)) = self.text_pos_at(x, y) else {
            self.set_notice(lang::tr("cliquez dans une ligne de texte").into());
            return;
        };
        let Some((_, near)) = self.page_at(x, y) else {
            return;
        };
        let Some((cx, line)) = self
            .loaded
            .as_mut()
            .and_then(|l| l.text(pos.page).1.caret_box(pos.caret, near))
        else {
            return;
        };
        if !self.require_right(crate::render_worker::Right::Annotate) {
            return;
        }
        self.selection = None;
        self.prompt = Some(Prompt::new(
            lang::tr("Insérer du texte"),
            lang::trf(
                "Texte à insérer (page {}) :",
                &[&(pos.page + 1).to_string()],
            ),
            TextInput::new(lang::tr("Texte à ajouter")),
            PromptKind::Insert {
                page: pos.page,
                x: cx,
                line,
            },
        ));
    }

    /// Marque la sélection pour biffure (annotations `/Redact`, visibles en
    /// cadre rouge et encore réversibles tant qu'elles ne sont pas appliquées).
    fn mark_redaction(&mut self) {
        let Some(sel) = self.selection else { return };
        if sel.is_empty() {
            return;
        }
        let mut marks = Vec::new();
        {
            let Some(loaded) = &mut self.loaded else {
                return;
            };
            let (start, end) = sel.ordered();
            let last_page = loaded.pages.len().saturating_sub(1);
            for page in start.page..=end.page.min(last_page) {
                let text = &loaded.text(page).1;
                let Some((from, to)) = sel.range_on_page(page, text.len()) else {
                    continue;
                };
                for rect in text.rects(from, to) {
                    marks.push(RedactionMark::new(page, rect));
                }
            }
        }
        if marks.is_empty() {
            return;
        }
        let count = marks.len();
        self.apply_edit(EditOp::Mark { marks });
        self.selection = None;
        log_line(&format!("{count} zone(s) marquée(s) pour biffure"));
    }

    /// Applique définitivement les marques de biffure, après confirmation.
    fn apply_redactions(&mut self) {
        // Le droit se vérifie avant de faire confirmer pour rien.
        if self.loaded.is_none() || !self.require_right(crate::render_worker::Right::Modify) {
            return;
        }
        self.confirm(
            "Appliquer les biffures ?",
            "Le contenu couvert par les marques sera supprimé définitivement du document.",
            "Appliquer",
            Then::ApplyRedactions,
        );
    }

    /// Affiche ou masque un calque : le document n'est pas modifié, seul le
    /// rendu change (c'est une préférence d'affichage, comme dans Acrobat).
    fn toggle_layer(&mut self, number: u32, window: &mut dyn WindowHandle) {
        let Some(l) = &mut self.loaded else { return };
        let mut name = String::new();
        let mut now = false;
        for (n, label, visible) in &mut l.layers {
            if *n == number {
                *visible = !*visible;
                now = *visible;
                name.clone_from(label);
            }
        }
        if name.is_empty() {
            return;
        }
        let map: HashMap<u32, bool> = l
            .layers
            .iter()
            .map(|(n, _, visible)| (*n, *visible))
            .collect();
        l.cache.clear();
        if let Some(worker) = &mut l.worker {
            worker.set_layers(map);
        }
        self.set_notice(if now {
            format!("calque « {name} » affiché")
        } else {
            format!("calque « {name} » masqué")
        });
        window.request_redraw();
    }

    /// Va à la page portant l'étiquette tapée dans le champ de page. Si aucune
    /// ne correspond, le texte est relu comme un numéro de page physique :
    /// taper « 3 » dans un document dont aucune étiquette n'est « 3 » doit
    /// quand même mener quelque part.
    fn go_to_label(&mut self, text: &str) {
        let Some(l) = &self.loaded else { return };
        let count = l.pages.len();
        if count == 0 {
            return;
        }
        let target = acrux_features::pagelabels::page_for_label(&l.labels, text).or_else(|| {
            text.trim()
                .parse::<usize>()
                .ok()
                .filter(|n| *n >= 1)
                .map(|n| (n - 1).min(count - 1))
        });
        match target {
            Some(page) => self.navigate(|v| v.scroll_to_page(page)),
            None => self.set_notice(format!("aucune page « {text} »")),
        }
    }

    /// Enregistre une pièce jointe sur le disque.
    fn save_attachment(&mut self, index: usize, window: &mut dyn WindowHandle) {
        let Some(l) = &self.loaded else { return };
        let Some(row) = l.attachments.get(index) else {
            return;
        };
        let name = row.name.clone();
        // Le dialogue propose le nom d'origine ; les séparateurs de chemin
        // qu'un fichier malveillant pourrait glisser dans le nom sont retirés,
        // sinon la suggestion désignerait un autre dossier.
        let suggested: String = name
            .chars()
            .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
            .collect();
        let Some(path) = window.save_file_dialog(&suggested) else {
            return;
        };
        let Some(l) = &self.loaded else { return };
        let list = match list_attachments(&l.doc) {
            Ok(list) => list,
            Err(e) => {
                self.alert("Extraction impossible", &format!("{e}"));
                return;
            }
        };
        let Some(found) = list.iter().find(|a| a.name == name) else {
            self.alert(
                "Extraction impossible",
                &format!("la pièce jointe « {name} » a disparu du document."),
            );
            return;
        };
        match read_attachment(&l.doc, found).and_then(|data| {
            std::fs::write(&path, &data)?;
            Ok(data.len())
        }) {
            Ok(size) => self.set_notice(format!("« {name} » enregistré ({size} octets)")),
            Err(e) => self.alert("Extraction impossible", &format!("{name}\n\n{e}")),
        }
        window.request_redraw();
    }

    /// Joint un fichier choisi par l'utilisateur au document courant.
    fn add_attachment(&mut self, window: &mut dyn WindowHandle) {
        if self.loaded.is_none() {
            return;
        }
        let Some(path) = window.open_file_dialog() else {
            return;
        };
        let data = match std::fs::read(&path) {
            Ok(data) => data,
            Err(e) => {
                self.alert("Lecture impossible", &format!("{}\n\n{e}", path.display()));
                return;
            }
        };
        let name = path.file_name().map_or_else(
            || "fichier".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let size = data.len();
        if !self.apply_edit(EditOp::Attach {
            name: name.clone(),
            data,
            description: None,
        }) {
            window.request_redraw();
            return;
        }
        self.panel_open = true;
        self.panel.tab = PanelTab::Attachments;
        self.clamp_scroll();
        self.set_notice(format!(
            "« {name} » joint ({size} octets) — Ctrl+S pour enregistrer"
        ));
        window.request_redraw();
    }

    /// Insère, avant la page courante, toutes les pages d'un autre PDF.
    fn insert_pages(&mut self, window: &mut dyn WindowHandle) {
        // Le droit se vérifie avant de faire choisir un fichier pour rien.
        if self.loaded.is_none() || !self.require_right(crate::render_worker::Right::Assemble) {
            return;
        }
        let Some(path) = window.open_file_dialog() else {
            return;
        };
        let at = self.current_page();
        if self.apply_edit(EditOp::Insert {
            path,
            password: None,
            pages: Vec::new(),
            at,
        }) {
            self.set_notice(format!("pages insérées avant la page {}", at + 1));
        }
    }

    /// Duplique la page courante juste après elle. Un index répété dans
    /// l'ordre des pages suffit : `reorder_pages` en fait une copie indirecte
    /// distincte, ce qui marche aussi sur un document déjà modifié.
    fn duplicate_current(&mut self) {
        let Some(l) = &self.loaded else { return };
        let count = l.pages.len();
        let page = self.target_page();
        let mut order: Vec<usize> = (0..count).collect();
        if page >= count {
            return;
        }
        order.insert(page + 1, page);
        if self.apply_edit(EditOp::Reorder { order }) {
            self.set_notice(format!("page {} dupliquée", page + 1));
        }
    }

    /// Écrit la page courante dans un nouveau fichier.
    fn extract_current(&mut self, window: &mut dyn WindowHandle) {
        // La page extraite est écrite en clair, sans les permissions du
        // document : c'est en extraire le contenu, que la permission de copie
        // refuse (Acrobat lie de même l'extraction de pages à la copie).
        if self.loaded.is_some() && !self.rights().copy {
            self.refuse(
                lang::tr("Extraction interdite"),
                lang::tr("Les permissions de ce document interdisent d'en extraire le contenu. Le mot de passe des permissions lève cette restriction."),
            );
            return;
        }
        let Some(l) = &self.loaded else { return };
        let page = self.target_page();
        let stem = l.path.file_stem().map_or_else(
            || "document".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        let Some(target) = window.save_file_dialog(&format!("{stem}-p{}.pdf", page + 1)) else {
            return;
        };
        let Some(l) = &self.loaded else { return };
        let result = acrux_features::pages::extract_pages(&l.doc, &[page])
            .and_then(|d| d.save_full())
            .and_then(|bytes| {
                std::fs::write(&target, bytes)
                    .map_err(|e| acrux_core::Error::Corrupt(format!("écriture : {e}")))
            });
        match result {
            Ok(()) => self.set_notice(format!(
                "page {} écrite dans {}",
                page + 1,
                target.display()
            )),
            Err(e) => self.alert("Extraction impossible", &format!("{e}")),
        }
    }

    /// Convertit le document : le format vient de l'extension choisie dans le
    /// dialogue. La conversion elle-même est faite par le fil de rendu — une
    /// centaine de pages en PNG prendrait plusieurs secondes.
    fn export(&mut self, window: &mut dyn WindowHandle) {
        self.flush_typing();
        // Convertir, c'est extraire le contenu : ce que la permission de
        // copie refuse, l'export le refuse aussi.
        if self.loaded.is_some() && !self.rights().copy {
            self.refuse(
                lang::tr("Export interdit"),
                lang::tr("Les permissions de ce document interdisent d'en extraire le contenu. Le mot de passe des permissions lève cette restriction."),
            );
            return;
        }
        let Some(l) = &self.loaded else { return };
        let stem = l.path.file_stem().map_or_else(
            || "document".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        let types = ExportFormat::all();
        let Some(path) = window.save_file_dialog_as(&format!("{stem}.html"), types) else {
            return;
        };
        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(format) = ExportFormat::from_extension(&extension) else {
            self.alert(
                "Format inconnu",
                "Choisissez une extension parmi html, docx, xlsx, md, txt, png ou jpg.",
            );
            return;
        };
        let Some(l) = &self.loaded else { return };
        let Some(worker) = &l.worker else {
            self.alert(
                "Export impossible",
                "Le document n'est pas ouvert sur le fil de rendu.",
            );
            return;
        };
        worker.export(path.clone(), format);
        self.set_notice(format!("export en cours vers {}…", path.display()));
        window.request_redraw();
    }

    /// Met à jour l'info-bulle après un changement de survol et programme le
    /// réveil qui la fera apparaître (rien ne se repeint tant que la souris
    /// ne bouge pas : sans ce réveil, l'info-bulle n'arriverait jamais).
    fn update_tip(&mut self, window: &mut dyn WindowHandle) {
        let info = self.toolbar_info();
        // La carte de recherche d'abord : elle flotte par-dessus tout le
        // reste, et le survol de la barre d'outils peut être resté levé.
        let tip = self
            .search_tip()
            .or_else(|| self.toolbar.hover_tip(&info))
            .or_else(|| self.mode_bar_tip())
            .or_else(|| self.tab_tip())
            .or_else(|| self.recent_tip())
            .or_else(|| self.status_tip());
        // Même texte ne veut pas dire même élément : les croix des onglets
        // disent toutes « Fermer l'onglet ». Sans le rectangle, passer de
        // l'une à l'autre laissait la bulle sous la première.
        let same = match (&self.tip, &tip) {
            (Some((a, ra, _)), Some((b, rb))) => a == b && ra == rb,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.tip = tip.map(|(text, rect)| (text, rect, std::time::Instant::now()));
        if self.tip.is_some() {
            let waker = window.waker();
            // Un fil jetable : la fenêtre n'a pas de minuterie et l'attente
            // est trop courte pour mériter une infrastructure.
            let _ = std::thread::Builder::new()
                .name("info-bulle".into())
                .spawn(move || {
                    std::thread::sleep(TIP_DELAY);
                    waker.wake();
                });
        }
        window.request_redraw();
    }

    /// Info-bulle d'un bouton de la barre des commentaires : le nom de
    /// l'outil, que l'icône seule ne dit pas, et le raccourci de la même
    /// action sans outil (H pour surligner la sélection).
    fn mode_bar_tip(&self) -> Option<(String, (i32, i32, i32, i32))> {
        if !self.comment_bar || self.mode_bar_height() == 0 {
            return None;
        }
        let (index, rect) = self.mode_bar.hover_tip()?;
        let tool = *COMMENT_TOOLS.get(index)?;
        let label = lang::tr(tool.describe().0);
        let keys = crate::ui::palette::describe(tool.action()).map_or("", |(_, k)| k);
        Some((
            if keys.is_empty() {
                label.to_string()
            } else {
                format!("{label}  ({keys})")
            },
            rect,
        ))
    }

    /// Info-bulle d'un onglet : le **chemin complet** du document, que
    /// l'onglet réduit à son nom (deux « facture.pdf » ne se distinguaient
    /// pas), et s'il reste des modifications à enregistrer. Sur la croix :
    /// ce qu'elle fait.
    fn tab_tip(&self) -> Option<(String, (i32, i32, i32, i32))> {
        if self.tabs_height() == 0 {
            return None;
        }
        let (index, part, (x, y, w, h)) = self.tabs.hovered()?;
        let text = match part {
            TabPart::Close => lang::tr("Fermer l'onglet").to_string(),
            TabPart::Title => {
                let infos = self.tab_infos();
                let tab = infos.get(index)?;
                let name = if tab.path.is_empty() {
                    &tab.title
                } else {
                    &tab.path
                };
                if tab.modified {
                    format!(
                        "{name}   —   {}",
                        lang::tr("modifications non enregistrées")
                    )
                } else {
                    name.clone()
                }
            }
        };
        // La bande des onglets est sous la barre d'outils ; l'info-bulle se
        // place en coordonnées de la fenêtre.
        let top = Toolbar::height(&self.theme, self.dpi_scale as f32);
        Some((text, (x, y + top, w, h)))
    }

    /// Info-bulle d'un document récent de l'accueil : son **chemin complet**,
    /// que la carte tronque, puis sa taille et sa date.
    fn recent_tip(&self) -> Option<(String, (i32, i32, i32, i32))> {
        if !self.showing_home() {
            return None;
        }
        let (mx, my) = self.last_mouse?;
        let index = self.recent_at(mx, my)?;
        let path = self.prefs.recent.get(index)?;
        let (x, y, w, h) = self.recent_hits[index];
        // Taille et date, sans le dossier : le chemin le dit déjà.
        let details = describe_file(path);
        let facts: Vec<&str> = details.split(" · ").collect();
        let facts = facts[..facts.len().saturating_sub(1)].join(" · ");
        let text = if facts.is_empty() {
            path.display().to_string()
        } else {
            format!("{}   —   {facts}", path.display())
        };
        // Les cartes sont en coordonnées de la vue ; l'info-bulle, de la fenêtre.
        let (left, top) = (self.view_left() as i32, self.view_top() as i32);
        Some((text, (x + left, y + top, w, h)))
    }

    /// Vrai si une info-bulle a dépassé son délai d'attente : c'est le seul
    /// cas où un réveil de minuterie doit provoquer une peinture.
    fn tip_due(&self) -> bool {
        self.tip
            .as_ref()
            .is_some_and(|(_, _, since)| since.elapsed() >= TIP_DELAY)
    }

    /// Dessine l'info-bulle sous le bouton survolé, si le délai est écoulé.
    #[allow(clippy::many_single_char_names)] // coordonnées du cadre
    fn paint_tip(&mut self, frame: &mut Frame<'_>) {
        let Some((text, rect, since)) = self.tip.clone() else {
            return;
        };
        if since.elapsed() < TIP_DELAY {
            return;
        }
        let t = self.theme;
        let dpi = self.dpi_scale as f32;
        let size = t.font_size * dpi;
        let Some(renderer) = &mut self.text else {
            return;
        };
        let pad = (6.0 * dpi) as i32;
        // `measure` rend une largeur fractionnaire : arrondir vers le bas
        // rognerait le dernier caractère (remplacé par des points de suite).
        let text_w = renderer.measure(size, &text).ceil();
        let w = text_w as i32 + 2 * pad;
        let h = (renderer.line_height(size)) as i32 + pad;
        // Sous le bouton, recalée dans la fenêtre si elle dépasse à droite ;
        // au-dessus quand la place manque dessous — une indication de la
        // barre d'état : recalée vers le haut, la bulle la couvrirait.
        let x = (rect.0 + rect.2 / 2 - w / 2).clamp(4, (frame.width as i32 - w - 4).max(4));
        let gap = (4.0 * dpi) as i32;
        let below = rect.1 + rect.3 + gap;
        let y = if below + h <= frame.height as i32 - 2 {
            below
        } else {
            (rect.1 - gap - h).max(2)
        };
        shadow(
            frame,
            x,
            y + (3.0 * dpi) as i32,
            w,
            h,
            7.0 * dpi,
            12.0 * dpi,
            0.3,
        );
        round_rect(frame, x, y, w, h, 7.0 * dpi, t.tip_bg);
        round_rect_outline(frame, x, y, w, h, 7.0 * dpi, dpi.max(1.0), t.separator);
        let baseline = y as f32 + f32::midpoint(h as f32, renderer.ascent(size)) - 1.0;
        renderer.draw_clipped(
            frame,
            (x + pad) as f32,
            baseline,
            size,
            &text,
            t.text,
            text_w,
        );
    }

    /// Colle le presse-papiers dans le champ de saisie qui a la main, s'il y
    /// en a un. Rend vrai si le collage le concernait.
    fn paste_into_field(&mut self, window: &mut dyn WindowHandle) -> bool {
        let has_field = self.palette.is_some()
            || self.edit_menu_open()
            || (self.search.is_some() && !self.editing_text());
        if !has_field {
            return false;
        }
        let Some(text) = window.clipboard_text().filter(|t| !t.is_empty()) else {
            return true;
        };
        // La palette recouvre tout : c'est son champ qui reçoit, pas la
        // recherche restée ouverte dessous.
        if let Some(p) = &mut self.palette {
            p.paste(&text);
        } else if self.edit_menu_open() {
            // La recherche d'une police, le code d'une couleur : caractère
            // par caractère, comme une frappe.
            for c in text.chars().filter(|c| !c.is_control()) {
                let _ = self.edit_popup_char(c, window);
            }
        } else if self.search.is_some() {
            self.search_paste(&text);
        }
        window.request_redraw();
        true
    }

    /// Affiche un message passager dans la barre d'état.
    fn set_notice(&mut self, message: String) {
        log_line(&message);
        self.notice = Some((message, std::time::Instant::now()));
    }

    /// Ouvre l'invite de note à la dernière position de la souris.
    fn start_note(&mut self) {
        let Some((mx, my)) = self.last_mouse else {
            return;
        };
        let Some((page, pt)) = self.page_at(mx, my) else {
            return;
        };
        self.prompt = Some(Prompt::new(
            lang::tr("Nouvelle note"),
            lang::trf("Texte de la note (page {}) :", &[&(page + 1).to_string()]),
            TextInput::new(lang::tr("Votre commentaire")),
            PromptKind::Note {
                page,
                x: pt.x,
                y: pt.y,
            },
        ));
    }

    fn open(&mut self, path: &Path, window: &mut dyn WindowHandle) {
        // Ce qui est tapé dans un champ appartient au document d'avant, qui
        // passe à l'arrière-plan : on l'y écrit d'abord.
        self.commit_field();
        self.leave_home();
        // Une image ouverte devient un PDF, comme dans Acrobat : un TIFF de
        // scanner donne une page par feuille.
        if let Some(made) = self.open_as_image(path, window) {
            if made {
                self.title_dirty = true;
                window.request_redraw();
            }
            return;
        }
        match Document::load(path).and_then(|doc| {
            let pages = collect_pages(&doc)?;
            Ok((doc, pages))
        }) {
            Ok((doc, pages)) => {
                if doc.needs_password() {
                    let mut input = TextInput::new(lang::tr("Mot de passe"));
                    input.masked = true;
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.prompt = Some(Prompt::new(
                        lang::tr("Document protégé"),
                        lang::trf("Mot de passe pour « {} » :", &[&name]),
                        input,
                        PromptKind::Password {
                            path: path.to_path_buf(),
                            doc,
                            pages,
                        },
                    ));
                } else {
                    // Les apparences manquantes d'un formulaire, avant tout
                    // le reste (voir `forms::prepare_display`) : le fil de
                    // rendu fait de même, au même moment.
                    let _ = acrux_features::forms::prepare_display(&doc);
                    self.finish_open(path.to_path_buf(), doc, pages, None, window);
                    self.announce_restrictions();
                }
            }
            Err(e) => {
                self.error = Some(format!("{e}"));
                self.alert(
                    "Ouverture impossible",
                    &format!("{}\n\n{e}", path.display()),
                );
            }
        }
        self.title_dirty = true;
        window.request_redraw();
    }

    /// Ouvre un fichier image en le convertissant en PDF.
    ///
    /// Rend `None` si le fichier n'est pas une image — l'ouverture ordinaire
    /// reprend alors la main —, `Some(true)` si le document est ouvert et
    /// `Some(false)` si la conversion a échoué (le message est déjà affiché).
    fn open_as_image(&mut self, path: &Path, window: &mut dyn WindowHandle) -> Option<bool> {
        let data = std::fs::read(path).ok()?;
        if !is_image(&data) {
            return None;
        }
        let name = path
            .file_stem()
            .map_or_else(|| "image".to_string(), |n| n.to_string_lossy().into_owned());
        let made = acrux_features::create::from_images(
            &[acrux_features::create::ImageInput {
                data,
                name: name.clone(),
            }],
            &acrux_features::create::ImageLayout::default(),
        )
        .and_then(|doc| doc.save_full());
        let bytes = match made {
            Ok(b) => b,
            Err(e) => {
                self.alert(
                    "Ouverture impossible",
                    &format!(
                        "{}

{e}",
                        path.display()
                    ),
                );
                return Some(false);
            }
        };
        // Le fil de rendu relit le document sur le disque : le PDF fabriqué a
        // donc besoin d'un fichier, qui va dans le dossier temporaire tant
        // que l'utilisateur n'a pas dit où le ranger.
        let dir = std::env::temp_dir().join("acrux-images");
        let _ = std::fs::create_dir_all(&dir);
        let target = dir.join(format!("{name}.pdf"));
        if let Err(e) = std::fs::write(&target, &bytes) {
            self.alert(
                "Ouverture impossible",
                &format!(
                    "{}

{e}",
                    target.display()
                ),
            );
            return Some(false);
        }
        match Document::load(&target).and_then(|doc| {
            let pages = collect_pages(&doc)?;
            Ok((doc, pages))
        }) {
            Ok((doc, pages)) => {
                let count = pages.len();
                self.finish_open(target, doc, pages, None, window);
                if let Some(l) = &mut self.loaded {
                    l.temporary = true;
                    l.modified = true;
                }
                self.set_notice(crate::ui::lang::trf(
                    "Image convertie en PDF ({} page(s)) — Ctrl+S pour l'enregistrer",
                    &[&count.to_string()],
                ));
                Some(true)
            }
            Err(e) => {
                self.alert(
                    "Ouverture impossible",
                    &format!(
                        "{}

{e}",
                        path.display()
                    ),
                );
                Some(false)
            }
        }
    }

    /// Installe un document analysé (et authentifié) comme document courant.
    fn finish_open(
        &mut self,
        path: PathBuf,
        doc: Document,
        pages: Vec<Page>,
        password: Option<Vec<u8>>,
        window: &mut dyn WindowHandle,
    ) {
        let worker = RenderWorker::start(path.clone(), password.clone(), window.waker());
        // La fenêtre « Protéger » visait le document que celui-ci remplace.
        self.protect = None;
        let page_index = PageIndex::new(&pages);
        let fields = list_fields(&doc).unwrap_or_default();
        let comments = collect_comments(&doc, &pages);
        let layers: Vec<(u32, String, bool)> = acrux_render::layers(&doc)
            .into_iter()
            .map(|l| (l.number, l.name, l.visible))
            .collect();
        let attachments = collect_attachments(&doc);
        let media = acrux_features::media::list(&doc).unwrap_or_default();
        let models = acrux_features::three_d::list(&doc).unwrap_or_default();
        let labels = collect_labels(&doc);
        let items = outline(&doc, &page_index).unwrap_or_default();
        let flat = flatten_outline(&items);
        let outline_rows: Vec<OutlineRow> = flat
            .iter()
            .enumerate()
            .map(|(id, (depth, it))| OutlineRow {
                id,
                depth: *depth,
                title: it.title.clone(),
                has_children: !it.children.is_empty(),
            })
            .collect();
        let outline_actions: Vec<Option<Action>> =
            flat.iter().map(|(_, it)| it.action.clone()).collect();
        let open_ids: Vec<usize> = flat
            .iter()
            .enumerate()
            .filter(|(_, (_, it))| it.open)
            .map(|(id, _)| id)
            .collect();
        self.panel = Panel::new();
        self.panel.set_default_expanded(open_ids);
        // Un document déjà ouvert passe en arrière-plan : nouvel onglet. Il
        // garde sa vue, qu'on retrouvera en y revenant.
        let spot = self.view_spot();
        if let Some(mut previous) = self.loaded.take() {
            previous.resume = spot;
            let at = self.active_tab.min(self.others.len());
            self.others.insert(at, previous);
            self.active_tab = at + 1;
        }
        self.loaded = Some(Loaded {
            path,
            doc,
            pages,
            cache: HashMap::new(),
            worker,
            texts: HashMap::new(),
            links: HashMap::new(),
            page_index,
            outline_rows,
            outline_actions,
            password,
            history: Vec::new(),
            redo: Vec::new(),
            full_save: false,
            modified: false,
            temporary: false,
            fields: Vec::new(),
            fillable: false,
            form_bar_closed: false,
            comments,
            layers,
            attachments,
            media,
            models,
            boxes: HashMap::new(),
            labels,
            view_rotation: 0,
            nav: NavHistory::default(),
            resume: None,
        });
        if let Some(l) = &mut self.loaded {
            l.set_fields(fields);
        }
        self.error = None;
        self.scroll_y = 0.0;
        self.scroll_x = 0.0;
        self.selection = None;
        self.close_search();
        self.focus_field = None;
        self.field_edit = None;
        self.field_menu = None;
        self.anchor = 0;
        self.title_dirty = true;
        if let Some(l) = &self.loaded {
            let path = l.path.clone();
            self.prefs.push_recent(&path);
        }
        self.save_prefs();
        window.request_redraw();
    }

    /// Hauteur de la barre d'état en pixels physiques (0 en plein écran).
    fn status_height(&self) -> u32 {
        if self.fullscreen || self.reading {
            return 0;
        }
        (f64::from(self.theme.status_height) * self.dpi_scale).round() as u32
    }

    /// Haut de la zone de document (sous la barre d'outils), pixels physiques.
    fn view_top(&self) -> u32 {
        if self.fullscreen || self.reading {
            return 0;
        }
        Toolbar::height(&self.theme, self.dpi_scale as f32) as u32
            + self.tabs_height()
            + self.edit_bar_height()
            + self.mode_bar_height()
            + self.form_bar_height()
    }

    /// Hauteur de la barre d'un outil d'annotation, ou de la barre des
    /// commentaires.
    fn mode_bar_height(&self) -> u32 {
        if (self.annot_tool.is_some() || self.comment_bar) && !self.fullscreen && !self.reading {
            ModeBar::height(self.dpi_scale as f32).max(0) as u32
        } else {
            0
        }
    }

    /// Allume un outil d'annotation, ou l'éteint s'il l'était déjà.
    ///
    /// Un outil de commentaire ouvre la barre des commentaires ; l'éteindre
    /// la laisse ouverte, prête pour le suivant. La biffure garde sa barre à
    /// elle.
    fn toggle_annot_tool(&mut self, tool: AnnotTool, window: &mut dyn WindowHandle) {
        if self.loaded.is_none() {
            return;
        }
        self.commit_field();
        if self.annot_tool == Some(tool) {
            self.annot_tool = None;
            log_line(&format!("outil : {tool:?} éteint"));
            window.request_redraw();
            return;
        }
        if !self.require_right(crate::render_worker::Right::Annotate) {
            window.request_redraw();
            return;
        }
        // Un seul outil à la fois.
        self.edit = None;
        self.sign_panel = None;
        self.objects = None;
        self.annot_tool = Some(tool);
        self.comment_bar = tool.in_comment_bar();
        log_line(&format!("outil : {tool:?}"));
        self.clamp_scroll();
        // Du texte déjà sélectionné est traité tout de suite : choisir
        // « surligner » après avoir sélectionné fait ce qu'on attend.
        self.apply_annot_tool();
        window.request_redraw();
    }

    /// Ouvre la barre des commentaires sans outil — on sélectionne, puis on
    /// choisit ce qu'on en fait —, ou la ferme si elle l'est déjà.
    fn toggle_comment_bar(&mut self, window: &mut dyn WindowHandle) {
        if self.comment_bar {
            self.close_annot_tools();
            log_line("barre des commentaires : fermée");
            window.request_redraw();
            return;
        }
        if self.loaded.is_none() || self.showing_home() {
            return;
        }
        self.commit_field();
        if !self.require_right(crate::render_worker::Right::Annotate) {
            window.request_redraw();
            return;
        }
        self.edit = None;
        self.sign_panel = None;
        self.objects = None;
        // La biffure n'est pas un commentaire : elle s'éteint.
        if self.annot_tool.is_some_and(|t| !t.in_comment_bar()) {
            self.annot_tool = None;
        }
        self.comment_bar = true;
        log_line("barre des commentaires : ouverte");
        self.clamp_scroll();
        window.request_redraw();
    }

    /// Éteint l'outil d'annotation et ferme la barre des commentaires :
    /// toute sortie des outils passe par ici, sans quoi la barre resterait
    /// sous celle d'un autre mode.
    fn close_annot_tools(&mut self) {
        self.annot_tool = None;
        self.comment_bar = false;
    }

    /// Échap pour les outils de commentaire ; vrai si la touche a servi.
    ///
    /// Un outil allumé s'éteint d'abord, la barre des commentaires restant
    /// ouverte. La barre seule ne se ferme qu'en dernier : Échap annule
    /// toujours ce qui est le plus proche. Cachée (plein écran, lecture),
    /// elle ne prend pas la touche — elle fermait une barre invisible au
    /// lieu de quitter le plein écran. Une sélection, un champ qui a le
    /// focus, un média qui joue ou le focus dans une barre passent avant
    /// elle : ouverte depuis longtemps, on l'a oubliée, et c'est eux qu'on
    /// voit.
    fn escape_comment_tools(&mut self) -> bool {
        if self.annot_tool.is_some() {
            self.annot_tool = None;
            if !self.comment_bar {
                self.close_annot_tools();
            }
            return true;
        }
        let first = self.selection.is_some_and(|s| !s.is_empty())
            || self.focus_field.is_some()
            || self.media.is_some()
            || self.region != Region::Document;
        if self.comment_bar && self.mode_bar_height() > 0 && !first {
            self.close_annot_tools();
            log_line("barre des commentaires : fermée");
            return true;
        }
        false
    }

    /// Applique l'outil courant à la sélection, s'il y en a une.
    fn apply_annot_tool(&mut self) {
        if self.selection.is_none_or(|s| s.is_empty()) {
            return;
        }
        match self.annot_tool {
            // L'invite s'ouvre sur la sélection, qui reste visible derrière.
            Some(AnnotTool::Replace) => self.start_replace(),
            Some(AnnotTool::Redact) => {
                self.mark_redaction();
                self.selection = None;
            }
            Some(tool) => {
                if let Some(kind) = tool.markup() {
                    self.markup_selection(kind);
                    self.selection = None;
                }
            }
            None => {}
        }
    }

    /// Hauteur de la barre « remplir et signer », nulle quand l'outil dort.
    /// Bascule le plein écran (F11 ; Échap pour sortir).
    fn toggle_fullscreen(&mut self, window: &mut dyn WindowHandle) {
        self.fullscreen = !self.fullscreen;
        self.wake_anim();
        if self.fullscreen {
            self.panel_open = false;
        }
        window.set_fullscreen(self.fullscreen);
        self.clamp_scroll();
    }

    /// Bord gauche de la zone de document (à droite du panneau latéral).
    fn view_left(&self) -> u32 {
        // La largeur affichée suit la largeur voulue en glissant : le panneau
        // vient du bord au lieu d'apparaître d'un coup.
        self.left_anim.value.round().max(0.0) as u32
    }

    /// Largeur que le panneau de gauche devrait avoir.
    fn wanted_left(&self) -> f64 {
        if self.reading {
            return 0.0;
        }
        // L'outil « remplir et signer » a son propre panneau, à la place des
        // vignettes : deux colonnes à gauche ne laisseraient plus voir la page.
        if self.sign_panel.is_some() {
            return f64::from(crate::ui::signpanel::WIDTH) * self.dpi_scale;
        }
        if self.panel_open {
            f64::from(PANEL_WIDTH) * self.dpi_scale
        } else {
            0.0
        }
    }

    /// Vrai si le panneau de gauche est celui de « remplir et signer ».
    fn sign_panel_open(&self) -> bool {
        self.sign_panel.is_some() && !self.reading
    }

    /// Largeur de la barre des outils, à droite.
    ///
    /// Elle s'efface d'elle-même quand la fenêtre devient trop étroite : une
    /// colonne d'outils qui ne laisse plus la place de lire la page ne rend
    /// service à personne, et l'utilisateur n'a pas à aller la fermer pour
    /// pouvoir travailler.
    fn tools_width(&self) -> u32 {
        self.tools_anim.value.round().max(0.0) as u32
    }

    /// Largeur que la colonne d'outils devrait avoir.
    fn wanted_tools_width(&self) -> f64 {
        if !self.tools_open
            || self.reading
            || self.fullscreen
            || self.showing_home()
            || !self.tools_fit()
        {
            return 0.0;
        }
        f64::from(crate::ui::tools::WIDTH) * self.dpi_scale
    }

    /// Vrai si la colonne d'outils tient dans la fenêtre, ouverte ou non.
    ///
    /// La barre d'outils grise son bouton quand elle ne tient pas : il
    /// basculait alors la préférence sans rien montrer, un clic mort.
    fn tools_fit(&self) -> bool {
        let width = f64::from(crate::ui::tools::WIDTH) * self.dpi_scale;
        let reste = f64::from(self.width) - self.wanted_left() - width;
        // Il faut au moins de quoi afficher une page lisible à côté.
        reste >= f64::from(MIN_PAGE_WIDTH) * self.dpi_scale
    }

    /// Fait avancer les animations de `dt` secondes ; rend vrai s'il reste du
    /// mouvement (et donc s'il faut repeindre).
    fn step_anim(&mut self, dt: f64) -> bool {
        let (left, tools) = (self.wanted_left(), self.wanted_tools_width());
        self.left_anim.go_to(left);
        self.tools_anim.go_to(tools);
        let mut moving = self.left_anim.step(dt) | self.tools_anim.step(dt);
        if let Some(goal) = self.scroll_goal_y {
            // Glissement exponentiel : deux coups de molette de suite
            // s'additionnent sans à-coup.
            let k = (-3.0 * dt / 0.15).exp();
            let next = goal - (goal - self.scroll_y) * k;
            let done = (goal - next).abs() < 0.5;
            self.scroll_y = if done { goal } else { next };
            // Ce mouvement-ci est le nôtre : `clamp_scroll` ne doit pas le
            // prendre pour un saut demandé ailleurs. S'il rectifie la valeur
            // (haut ou bas du document), il annulera le glissement, et c'est
            // bien ce qu'on veut.
            self.scroll_seen = self.scroll_y;
            self.clamp_scroll();
            // Arrivé, ou retenu par le haut ou le bas du document : dans les
            // deux cas le glissement n'a plus lieu d'être.
            let stopped = (self.scroll_y - next).abs() > 0.01;
            if done || stopped {
                self.scroll_goal_y = None;
            } else {
                moving = true;
            }
        }
        moving
    }

    /// Fait défiler de `dy` pixels, en douceur.
    fn glide_by(&mut self, dy: f64) {
        let base = self.scroll_goal_y.unwrap_or(self.scroll_y);
        let max = (self.total_height() - f64::from(self.view_height())).max(0.0);
        self.scroll_goal_y = Some((base + dy).clamp(0.0, max));
        self.scroll_seen = self.scroll_y;
        self.wake_anim();
    }

    /// Vrai si quelque chose bouge encore — dont une carte qui apparaît.
    fn animating(&self) -> bool {
        self.scroll_goal_y.is_some()
            || self.left_anim.running()
            || self.tools_anim.running()
            || self.dialog_animating()
            || self
                .prompt
                .as_ref()
                .is_some_and(|p| p.card.appear.animating())
            || self.settings.as_ref().is_some_and(SettingsSheet::animating)
            || self
                .protect
                .as_ref()
                .is_some_and(crate::ui::protect::ProtectDialog::animating)
    }

    /// Arme le fil des animations si l'événement qui s'achève a lancé un
    /// mouvement — typiquement, ouvert une carte. Sans cela, le fil ne le
    /// saurait qu'au prochain événement : la carte, peinte une fois au début
    /// de son fondu, resterait à demi transparente. Le fil existe déjà à ce
    /// stade (`tick_anim` le crée au premier événement), et c'est lui qui
    /// réveille : aucun réveil n'est posté depuis la boucle d'événements.
    ///
    /// On ne fait que lever le drapeau, jamais le baisser : un panneau qui
    /// vient d'être ouvert n'a pas encore de cible (`step_anim` la fixe au
    /// prochain pas) et compte sur `wake_anim` pour être animé.
    fn sync_anim(&self) {
        if self.animating() {
            self.wake_anim();
        }
    }

    /// Met en route (ou arrête) le fil qui réveille la fenêtre pendant les
    /// animations. Un fil plutôt qu'un réveil immédiat : sans attente, la
    /// boucle tournerait à plein régime pour rien.
    fn tick_anim(&mut self, window: &mut dyn WindowHandle) {
        // Le fil bat en permanence mais ne réveille la fenêtre que pendant
        // un mouvement : c'est le seul moyen d'animer sans qu'un événement
        // de l'utilisateur soit là pour relancer la machine.
        if self.ticker.is_none() {
            let alive = Arc::new(AtomicBool::new(true));
            let waker = window.waker();
            let running = Arc::clone(&alive);
            let moving = Arc::clone(&self.anim_flag);
            let _ = std::thread::Builder::new()
                .name("animations".into())
                .spawn(move || {
                    while running.load(Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(16));
                        if moving.load(Ordering::Relaxed) {
                            waker.wake();
                        }
                    }
                });
            self.ticker = Some(alive);
        }
        let dt = self.clock.tick();
        if self.step_anim(dt) {
            window.request_redraw();
        }
        self.anim_flag.store(self.animating(), Ordering::Relaxed);
    }

    /// Signale qu'il y a de nouveau du mouvement à animer.
    fn wake_anim(&self) {
        self.anim_flag.store(true, Ordering::Relaxed);
    }

    /// Largeur de la zone de document.
    fn view_width(&self) -> u32 {
        self.width
            .saturating_sub(self.view_left())
            .saturating_sub(self.tools_width())
    }

    /// Ce que la barre des outils doit savoir.
    fn tools_info(&self) -> ToolsInfo {
        ToolsInfo {
            has_document: self.loaded.is_some() && !self.showing_home(),
            // Les deux outils qui restent ouverts se signalent comme tels :
            // sans cela, rien ne dirait lequel est en cours.
            // La colonne a ses lignes pour surligner, poser une note et
            // biffer ; les autres outils de commentaire s'allument sous
            // « Commenter », qui ouvre leur barre.
            active: if let Some(tool) = self.annot_tool.filter(|t| {
                matches!(
                    t,
                    AnnotTool::Highlight | AnnotTool::Note | AnnotTool::Redact
                )
            }) {
                Some(tool.command())
            } else if self.comment_bar {
                Some(Command::CommentBar)
            } else if let Some(tool) = self.edit_tool() {
                Some(match tool {
                    EditTool::Select => Command::EditPdf,
                    EditTool::AddText => Command::AddTextBox,
                })
            } else if self.sign_panel.is_some() {
                Some(Command::FillSign)
            } else if self.objects.is_some() {
                Some(Command::EditObjects)
            } else {
                None
            },
        }
    }

    /// Hauteur de la zone de document.
    fn view_height(&self) -> u32 {
        self.height
            .saturating_sub(self.status_height())
            .saturating_sub(self.view_top())
    }

    /// Ce que la barre d'outils affiche. Relu à chaque peinture : tout y est
    /// en temps constant, et « Annuler » s'allume dès la première frappe.
    fn toolbar_info(&self) -> ToolbarInfo {
        let page = self.current_page();
        let has_document = self.loaded.is_some() && !self.showing_home();
        // Le bloc de texte en cours de saisie a sa propre pile d'étapes :
        // « Annuler » y défait d'abord, comme Ctrl+Z. Refermer un bloc
        // modifié inscrit une modification au document, ce qui vide la pile
        // « rétablir » de celui-ci : tant qu'on y tape, elle ne compte plus.
        let (block_undo, block_redo) = self.block_history();
        let (history, redo) = self.loaded.as_ref().map_or((false, false), |l| {
            (!l.history.is_empty(), !l.redo.is_empty())
        });
        ToolbarInfo {
            page: page + 1,
            page_count: self.loaded.as_ref().map_or(0, |l| l.pages.len()),
            page_label: self
                .loaded
                .as_ref()
                .and_then(|l| l.labels.get(page))
                .cloned(),
            zoom_percent: (self.effective_zoom() * 100.0).round() as u32,
            has_document,
            can_undo: has_document && (history || block_undo),
            can_redo: has_document && (block_redo || (redo && !block_undo)),
            // « Allumé » veut dire « affiché » : le panneau des vignettes cède
            // la place à celui de « remplir et signer », et la colonne des
            // outils s'efface d'une fenêtre trop étroite pour elle.
            panel_open: self.panel_open && self.sign_panel.is_none() && !self.reading,
            tools_open: self.wanted_tools_width() > 0.0,
            tools_fit: self.tools_fit(),
            dark_theme: self.theme.is_dark(),
            zoom_open: self
                .zoom_menu
                .as_ref()
                .is_some_and(|(_, anchor)| *anchor == ZoomAnchor::Toolbar),
        }
    }

    /// Géométrie des vignettes : échelle, clé de cache et taille par page.
    fn thumb_geometry(&self) -> (f64, u32, Vec<(u32, u32)>) {
        let Some(l) = &self.loaded else {
            return (1.0, 0, Vec::new());
        };
        let inner = (f64::from(PANEL_WIDTH) - 40.0) * self.dpi_scale;
        let max_w = l
            .pages
            .iter()
            .map(|p| {
                let b = p.crop_box(&l.doc);
                match shown_rotation(l, p) {
                    90 | 270 => b.height(),
                    _ => b.width(),
                }
            })
            .fold(1.0_f64, f64::max);
        let scale = (inner / max_w).max(0.01);
        let key = (scale * 1000.0).round() as u32;
        let sizes = l
            .pages
            .iter()
            .map(|p| page_pixel_size_rotated(&l.doc, p, scale, l.view_rotation))
            .collect();
        (scale, key, sizes)
    }

    fn toggle_panel(&mut self) {
        self.panel_open = !self.panel_open;
        self.wake_anim();
        self.save_prefs();
        self.clamp_scroll();
        if self.panel_open {
            let (_, _, sizes) = self.thumb_geometry();
            self.panel
                .reveal_page(self.current_page(), &sizes, self.dpi_scale as f32);
        }
    }

    /// Dessine le panneau latéral dans sa sous-vue et demande les vignettes manquantes.
    fn paint_panel(&mut self, frame: &mut Frame<'_>) {
        let (scale, key, sizes) = self.thumb_geometry();
        let current = self.current_page();
        if current != self.last_current {
            self.last_current = current;
            self.panel
                .reveal_page(current, &sizes, self.dpi_scale as f32);
        }
        let theme = self.theme;
        let dpi = self.dpi_scale as f32;
        let Some(l) = &mut self.loaded else { return };
        let Some(text) = &mut self.text else { return };
        let content = PanelContent {
            page_count: l.pages.len(),
            current,
            thumb_sizes: &sizes,
            bitmaps: &l.cache,
            thumb_key: key,
            outline: &l.outline_rows,
            comments: &l.comments,
            layers: &l.layers,
            attachments: &l.attachments,
        };
        self.panel.paint(frame, text, &theme, dpi, &content);
        if let Some(worker) = &mut l.worker {
            for &page in self.panel.visible_pages() {
                if !l.cache.contains_key(&(page, key)) {
                    worker.request(page, scale, key);
                }
            }
        }
    }

    /// Passe le focus à la zone suivante (ou précédente). Les zones absentes
    /// sont sautées : sans document, la barre d'outils n'a rien d'actif ; sans
    /// panneau ouvert, il n'y a pas de liste où aller.
    fn cycle_region(&mut self, forward: bool, window: &mut dyn WindowHandle) {
        // Le clavier quitte le document : le champ en saisie est validé,
        // sans quoi il garderait la frappe destinée à la barre ou au panneau.
        self.commit_field();
        let info = self.toolbar_info();
        let order = [Region::Document, Region::Toolbar, Region::Panel];
        let at = order.iter().position(|r| *r == self.region).unwrap_or(0);
        for step in 1..=order.len() {
            let next = if forward {
                (at + step) % order.len()
            } else {
                (at + order.len() - step) % order.len()
            };
            let candidate = order[next];
            let taken = match candidate {
                Region::Document => true,
                Region::Toolbar => self.toolbar.focus_edge(!forward, &info),
                Region::Panel => {
                    self.panel_open && self.with_panel_content(|p, c| p.focus_edge(!forward, c))
                }
            };
            if taken {
                self.leave_region(candidate);
                self.region = candidate;
                log_line(&format!("focus clavier : {candidate:?}"));
                window.request_redraw();
                return;
            }
        }
    }

    /// Rend le focus des zones que l'on quitte.
    fn leave_region(&mut self, keep: Region) {
        if keep != Region::Toolbar {
            self.toolbar.clear_focus();
        }
        if keep != Region::Panel {
            self.panel.clear_focus();
        }
    }

    /// Exécute `f` avec le contenu courant du panneau. Le contenu emprunte le
    /// document : ce passage par une fermeture évite de le reconstruire à
    /// quatre endroits.
    fn with_panel_content<T>(&mut self, f: impl FnOnce(&mut Panel, &PanelContent<'_>) -> T) -> T
    where
        T: Default,
    {
        let (_, key, sizes) = self.thumb_geometry();
        let current = self.current_page();
        let Some(l) = &self.loaded else {
            return T::default();
        };
        let content = PanelContent {
            page_count: l.pages.len(),
            current,
            thumb_sizes: &sizes,
            bitmaps: &l.cache,
            thumb_key: key,
            outline: &l.outline_rows,
            comments: &l.comments,
            layers: &l.layers,
            attachments: &l.attachments,
        };
        f(&mut self.panel, &content)
    }

    /// Touches reçues quand la barre d'outils tient le focus clavier.
    fn toolbar_key(&mut self, key: Key, m: Modifiers, window: &mut dyn WindowHandle) {
        let info = self.toolbar_info();
        match key {
            Key::Right | Key::Tab if !m.shift => {
                if !self.toolbar.focus_step(true, &info) {
                    self.region = Region::Document;
                }
            }
            Key::Left | Key::Tab => {
                if !self.toolbar.focus_step(false, &info) {
                    self.region = Region::Document;
                }
            }
            Key::Home => {
                self.toolbar.focus_edge(false, &info);
            }
            Key::End => {
                self.toolbar.focus_edge(true, &info);
            }
            Key::Enter | Key::Space => {
                if let Some(action) = self.toolbar.activate_focus(&info) {
                    self.tool_action(action, window);
                }
            }
            Key::Escape => {
                self.toolbar.clear_focus();
                self.region = Region::Document;
            }
            _ => {}
        }
        window.request_redraw();
    }

    /// Touches reçues quand le panneau tient le focus clavier.
    fn panel_key(&mut self, key: Key, m: Modifiers, window: &mut dyn WindowHandle) {
        match key {
            Key::Down | Key::Tab if !m.shift => {
                if !self.with_panel_content(|p, c| p.focus_step(true, c)) {
                    self.region = Region::Document;
                }
            }
            Key::Up | Key::Tab => {
                if !self.with_panel_content(|p, c| p.focus_step(false, c)) {
                    self.region = Region::Document;
                }
            }
            Key::Home => {
                self.with_panel_content(|p, c| p.focus_edge(false, c));
            }
            Key::End => {
                self.with_panel_content(|p, c| p.focus_edge(true, c));
            }
            Key::Right => {
                self.panel.toggle_focused_branch(true);
            }
            Key::Left => {
                if !self.panel.toggle_focused_branch(false) {
                    self.panel.clear_focus();
                    self.region = Region::Document;
                }
            }
            Key::Enter | Key::Space => {
                let action = self.with_panel_content(Panel::activate_focus);
                self.panel_action(action, window);
            }
            Key::Escape => {
                self.panel.clear_focus();
                self.region = Region::Document;
            }
            _ => {}
        }
        // La vignette ciblée doit rester visible.
        if let Some(page) = self.panel.focused_page() {
            let (_, _, sizes) = self.thumb_geometry();
            let dpi = self.dpi_scale as f32;
            self.panel.reveal_page(page, &sizes, dpi);
        }
        window.request_redraw();
    }

    /// Mode lecture : plus de barres ni de panneau, seulement les pages.
    fn toggle_reading(&mut self, window: &mut dyn WindowHandle) {
        self.reading = !self.reading;
        self.wake_anim();
        if self.reading {
            self.region = Region::Document;
            self.leave_region(Region::Document);
        }
        self.set_notice(if self.reading {
            "mode lecture : Échap ou F5 pour revenir".to_string()
        } else {
            "mode lecture quitté".to_string()
        });
        self.clamp_scroll();
        window.request_redraw();
    }

    /// Exécute une action du panneau.
    fn panel_action(&mut self, action: PanelAction, window: &mut dyn WindowHandle) {
        match action {
            PanelAction::None => {}
            PanelAction::GoToPage(p) => self.navigate(|v| v.scroll_to_page(p)),
            PanelAction::MovePage { from, to } => {
                let count = self.loaded.as_ref().map_or(0, |l| l.pages.len());
                if from >= count || to >= count {
                    return;
                }
                // L'ordre complet décrit le document réordonné : on retire la
                // page déplacée puis on la réinsère à sa nouvelle place.
                let mut order: Vec<usize> = (0..count).collect();
                let page = order.remove(from);
                order.insert(to, page);
                self.apply_edit(EditOp::Reorder { order });
                self.scroll_to_page(to);
            }
            PanelAction::ToggleLayer(number) => self.toggle_layer(number, window),
            PanelAction::SaveAttachment(index) => self.save_attachment(index, window),
            PanelAction::AddAttachment => self.add_attachment(window),
            PanelAction::GoToComment(index) => {
                let target = self
                    .loaded
                    .as_ref()
                    .and_then(|l| l.comments.get(index))
                    .map(|c| (c.page, c.rect, c.index, c.name.clone()));
                if let Some((page, rect, at, name)) = target {
                    log_line(&format!(
                        "panneau : commentaire #{} de la page {} ({})",
                        at + 1,
                        page + 1,
                        name.as_deref().unwrap_or("sans identifiant")
                    ));
                    // Au passage commenté, pas en haut de sa page : sur une
                    // page longue, on ne le trouverait pas.
                    self.navigate(|v| {
                        v.scroll_to_page(page);
                        v.reveal_page_rect(page, rect);
                    });
                }
            }
            PanelAction::Follow(id) => {
                let act = self
                    .loaded
                    .as_ref()
                    .and_then(|l| l.outline_actions.get(id).cloned().flatten());
                if let Some(a) = act {
                    self.follow(&a, window);
                }
            }
        }
    }

    /// Exécute une commande de la palette. Chaque entrée appelle exactement
    /// la même fonction que son raccourci clavier.
    // Une commande par ligne : la liste se lit comme un sommaire.
    #[allow(clippy::too_many_lines)]
    fn run_command(&mut self, command: Command, window: &mut dyn WindowHandle) {
        log_line(&format!("palette : {command:?}"));
        match command {
            Command::Home => self.show_home(window),
            Command::Settings => self.open_settings(window),
            Command::Open => {
                if let Target::Recent(index) = self.command_target {
                    self.open_recent(index, window);
                } else if let Some(p) = window.open_file_dialog() {
                    self.open(&p, window);
                }
            }
            Command::Save => {
                self.save(false, window);
            }
            Command::SaveAs => {
                self.save(true, window);
            }
            Command::Print => self.print(window),
            Command::Export => self.export(window),
            Command::CloseTab => {
                let tab = self.target_tab();
                self.close_tab(tab, window);
            }
            Command::CloseOtherTabs => self.close_other_tabs(),
            Command::CopyPath => self.copy_path(window),
            Command::RevealInFolder => self.show_in_folder(window),
            Command::ForgetRecent => {
                if let Target::Recent(index) = self.command_target {
                    self.forget_recent(index);
                }
            }
            Command::Properties => self.show_properties(),
            Command::NextTab => self.cycle_tab(true),
            Command::PrevPage => self.step_row(false),
            Command::NextPage => self.step_row(true),
            Command::FirstPage => self.navigate(|v| v.scroll_to_page(0)),
            Command::LastPage => {
                let last = self
                    .loaded
                    .as_ref()
                    .map_or(0, |l| l.pages.len().saturating_sub(1));
                self.navigate(|v| v.scroll_to_page(last));
            }
            Command::ViewBack => self.history_step(false),
            Command::ViewForward => self.history_step(true),
            Command::RotateViewRight => self.rotate_view(90),
            Command::RotateViewLeft => self.rotate_view(-90),
            Command::PrevTab => self.cycle_tab(false),
            Command::ZoomIn => self.zoom_step(1),
            Command::ZoomOut => self.zoom_step(-1),
            Command::ZoomReset => self.set_zoom(1.0),
            Command::ZoomMenu => self.open_zoom_menu(ZoomAnchor::Toolbar, window),
            Command::FitWidth => self.set_fit(Fit::Width),
            Command::FitPage => self.set_fit(Fit::Page),
            Command::FitAutomatic => self.set_fit(Fit::Automatic),
            Command::CycleViewMode => self.set_view_mode(self.view_mode.next()),
            Command::Fullscreen => self.toggle_fullscreen(window),
            Command::TogglePanel => self.toggle_panel(),
            Command::ToggleTools => {
                self.tools_open = !self.tools_open;
                self.wake_anim();
                self.clamp_scroll();
                self.save_prefs();
            }
            Command::ShowLayers => {
                self.panel_open = true;
                self.panel.tab = PanelTab::Layers;
                self.clamp_scroll();
            }
            Command::ShowAttachments => {
                self.panel_open = true;
                self.panel.tab = PanelTab::Attachments;
                self.clamp_scroll();
            }
            Command::AddAttachment => self.add_attachment(window),
            Command::GoToPage => {
                let info = self.toolbar_info();
                self.toolbar.focus_page(&info);
            }
            Command::ToggleTheme => self.toggle_theme(window),
            Command::Search => self.open_search_ui(),
            Command::FindNext => self.find_again(true),
            Command::FindPrevious => self.find_again(false),
            Command::Replace => self.open_replace(),
            Command::Copy => self.copy_selection(window),
            Command::SelectAll => self.select_all(),
            Command::RotateRight => self.rotate_current(90),
            Command::RotateLeft => self.rotate_current(-90),
            Command::DeletePage => self.delete_current(),
            Command::InsertPages => self.insert_pages(window),
            Command::DuplicatePage => self.duplicate_current(),
            Command::ExtractPage => self.extract_current(window),
            Command::Undo => self.undo_any(window),
            Command::Redo => self.redo_any(window),
            Command::EditText => self.start_text_edit(window),
            Command::EditPdf => {
                self.close_annot_tools();
                self.enter_edit(EditTool::Select, window);
            }
            Command::HighlightTool => self.toggle_annot_tool(AnnotTool::Highlight, window),
            Command::UnderlineTool => self.toggle_annot_tool(AnnotTool::Underline, window),
            Command::StrikeOutTool => self.toggle_annot_tool(AnnotTool::StrikeOut, window),
            Command::SquigglyTool => self.toggle_annot_tool(AnnotTool::Squiggly, window),
            Command::InsertTextTool => self.toggle_annot_tool(AnnotTool::Insert, window),
            Command::ReplaceTextTool => self.toggle_annot_tool(AnnotTool::Replace, window),
            Command::NoteTool => self.toggle_annot_tool(AnnotTool::Note, window),
            Command::RedactTool => self.toggle_annot_tool(AnnotTool::Redact, window),
            Command::CommentBar => self.toggle_comment_bar(window),
            Command::AddTextBox => {
                self.close_annot_tools();
                self.enter_edit(EditTool::AddText, window);
            }
            Command::Highlight => self.markup_selection(MarkupKind::Highlight),
            Command::Underline => self.markup_selection(MarkupKind::Underline),
            Command::StrikeOut => self.markup_selection(MarkupKind::StrikeOut),
            Command::Squiggly => self.markup_selection(MarkupKind::Squiggly),
            Command::ReplaceText => self.start_replace(),
            Command::InsertText => {
                if let Some((x, y)) = self.last_mouse {
                    self.start_insert(x, y);
                }
            }
            Command::Note => self.start_note(),
            Command::CheckUpdates => self.install_update(window),
            Command::EditObjects => self.toggle_objects(window),
            Command::FillSign => self.toggle_fillsign(window),
            Command::MarkRedaction => self.mark_redaction(),
            Command::ApplyRedactions => self.apply_redactions(),
            Command::Protect => self.protect_command(),
            Command::Unprotect => self.remove_protection(),
            Command::ToggleFieldHighlight => {
                if self.loaded.as_ref().is_some_and(|l| !l.fields.is_empty()) {
                    self.toggle_field_highlight();
                } else {
                    self.set_notice(lang::tr("ce document n'a pas de champ de formulaire").into());
                }
            }
            Command::ResetForm => self.ask_reset_form(),
            Command::FlattenForm => self.ask_flatten_form(),
        }
    }

    /// Applique une modification au document (et à la copie du fil de rendu).
    ///
    /// Rend faux si elle n'a pas eu lieu — refusée par les permissions ou
    /// impossible, ce qui est déjà dit : l'appelant ne doit pas annoncer
    /// comme fait ce qui ne l'est pas.
    fn apply_edit(&mut self, op: EditOp) -> bool {
        // Ce qui est tapé dans un champ passe d'abord : l'historique garde
        // l'ordre des gestes.
        self.commit_field();
        // Un document protégé ne se modifie que dans la limite de ses
        // permissions (le propriétaire les a toutes).
        if self.loaded.is_some() && !self.require_right(op.required_right()) {
            return false;
        }
        // Pivoter une page change sa taille à l'écran, et celle de toutes
        // les suivantes avec l'ajustement à la largeur : le même décalage en
        // pixels montrait ensuite autre chose. On garde la page et la part
        // de page qu'on regardait.
        let keep = if matches!(op, EditOp::Rotate { .. }) {
            self.view_spot()
        } else {
            None
        };
        let Some(l) = &mut self.loaded else {
            return false;
        };
        if let Err(e) = op.apply(&l.doc) {
            self.alert("Modification impossible", &format!("{e}"));
            return false;
        }
        if let Some(w) = &mut l.worker {
            w.edit(op.clone());
        }
        l.history.push(op);
        l.redo.clear();
        let before = l.pages.len();
        match collect_pages(&l.doc) {
            Ok(p) => l.pages = p,
            Err(e) => {
                self.alert("Modification impossible", &format!("{e}"));
                return false;
            }
        }
        // L'historique de la vue suit les pages supprimées, insérées ou
        // déplacées : revenir en arrière mène à la page qu'on avait quittée,
        // pas à celle qui a pris sa place. Toute nouvelle opération sur la
        // structure passe par `history::page_after`.
        let inserted = l.pages.len().saturating_sub(before);
        if let Some(op) = l.history.last() {
            l.nav.remap(|p| history::page_after(op, p, inserted));
        }
        l.page_index = PageIndex::new(&l.pages);
        l.cache.clear();
        l.texts.clear();
        l.links.clear();
        l.boxes.clear();
        l.set_fields(list_fields(&l.doc).unwrap_or_default());
        l.comments = collect_comments(&l.doc, &l.pages);
        l.attachments = collect_attachments(&l.doc);
        l.labels = collect_labels(&l.doc);
        l.modified = true;
        // Le champ qui avait le focus peut avoir disparu (formulaire aplati).
        let fields = &l.fields;
        self.focus_field = self
            .focus_field
            .filter(|&(fi, wi)| fields.get(fi).is_some_and(|f| wi < f.widgets.len()));
        self.selection = None;
        // Le document a changé sous la recherche : ses occurrences ne
        // désignent plus rien. Elle se ferme, sa requête retenue (F3 la
        // relance) ; le remplacement la met de côté avant et la rend après.
        self.close_search();
        // Le modèle activé décrivait un document qui vient de changer.
        self.three_d = None;
        self.title_dirty = true;
        if let Some(s) = keep {
            self.restore_spot(s);
        }
        self.clamp_scroll();
        true
    }

    /// Reconstruit le document depuis le fichier et rejoue `ops`. C'est la
    /// base de l'annulation : les modifications ne sont pas inversées une à
    /// une (elles ne sont pas toutes inversibles), le document est rechargé
    /// puis l'historique conservé est réappliqué.
    fn replay(&mut self, ops: Vec<EditOp>, redo: Vec<EditOp>, window: &mut dyn WindowHandle) {
        // La saisie en cours désigne un état du document qui va disparaître :
        // elle se referme, le mode reste ouvert.
        if let Some(mode) = &mut self.edit {
            mode.active = None;
            mode.units.clear();
        }
        self.field_edit = None;
        self.field_menu = None;
        let Some(l) = &self.loaded else { return };
        let (path, password) = (l.path.clone(), l.password.clone());
        // Le champ qui a le focus, par son nom : les rangs de l'inventaire
        // peuvent changer avec le document.
        let focused = self
            .focus_field
            .and_then(|(fi, wi)| l.fields.get(fi).map(|f| (f.name.clone(), wi)));
        let form_bar_closed = l.form_bar_closed;
        let (scroll_x, scroll_y, anchor) = (self.scroll_x, self.scroll_y, self.anchor);
        let panel_tab = self.panel.tab;
        let doc = match Document::load(&path) {
            Ok(d) => d,
            Err(e) => {
                self.alert("Rechargement impossible", &format!("{e}"));
                return;
            }
        };
        if let Some(pw) = &password {
            let _ = doc.authenticate(pw);
        }
        // Au même moment de la vie du document qu'à l'ouverture : avant de
        // rejouer quoi que ce soit.
        let _ = acrux_features::forms::prepare_display(&doc);
        for op in &ops {
            if let Err(e) = op.apply(&doc) {
                self.alert("Rejeu impossible", &format!("{e}"));
                return;
            }
        }
        let pages = match collect_pages(&doc) {
            Ok(p) => p,
            Err(e) => {
                self.alert("Rechargement impossible", &format!("{e}"));
                return;
            }
        };
        // L'endroit où l'on est, en page et en part de page : un décalage en
        // pixels ne désigne plus le même endroit quand la taille d'une page
        // change (Ctrl+Z après R ramenait ailleurs).
        let spot = self.view_spot();
        // La vue de l'onglet n'est pas une modification : l'annulation ne la
        // défait pas. L'historique de la vue survit.
        let Some((view_rotation, count, mut nav, before)) = self
            .loaded
            .take()
            .map(|l| (l.view_rotation, l.pages.len(), l.nav, l.history))
        else {
            return;
        };
        // L'opération en jeu est la dernière de l'historique le plus long :
        // celui d'avant pour une annulation, le nouveau pour un rétablissement.
        // Seule compte celle qui supprime, insère ou déplace des pages : les
        // vues retenues, et l'endroit où l'on est, la suivent — à l'envers
        // pour une annulation.
        let undoing = before.len() > ops.len();
        let moved = if undoing { before.last() } else { ops.last() }
            .filter(|op| history::moves_pages(op))
            .cloned();
        // Le document rechargé **remplace** celui de l'onglet courant : sans
        // cela, `finish_open` le rangerait en arrière-plan et ouvrirait un
        // onglet de plus à chaque annulation.
        self.finish_open(path, doc, pages, password, window);
        let mut here = None;
        if let Some(l) = &mut self.loaded {
            // Le fil de rendu repart du fichier : on lui rejoue l'historique.
            if let Some(w) = &mut l.worker {
                for op in &ops {
                    w.edit(op.clone());
                }
                if view_rotation != 0 {
                    w.set_view_rotation(view_rotation);
                }
            }
            l.modified = !ops.is_empty();
            l.history = ops;
            l.redo = redo;
            l.view_rotation = view_rotation;
            l.form_bar_closed = form_bar_closed;
            let now = l.pages.len();
            let place = |page: usize| {
                match &moved {
                    None => Some(page),
                    Some(op) if undoing => {
                        history::page_before(op, page, count.saturating_sub(now))
                    }
                    Some(op) => history::page_after(op, page, now.saturating_sub(count)),
                }
                .filter(|&p| p < now)
            };
            nav.remap(place);
            l.nav = nav;
            here = spot.and_then(|s| place(s.page).map(|page| Spot { page, ..s }));
        }
        // L'utilisateur ne doit pas perdre sa place en annulant.
        self.scroll_x = scroll_x;
        if let Some(s) = here {
            self.restore_spot(s);
        } else {
            // La page d'où l'on regardait n'existe plus (une suppression
            // refaite) : on reste au même décalage.
            self.anchor = anchor;
            self.scroll_y = scroll_y;
        }
        self.panel.tab = panel_tab;
        // Le focus revient au champ qui l'avait.
        self.focus_field = focused.and_then(|(name, wi)| {
            let fields = &self.loaded.as_ref()?.fields;
            let fi = fields.iter().position(|f| f.name == name)?;
            (wi < fields[fi].widgets.len()).then_some((fi, wi))
        });
        self.clamp_scroll();
        self.title_dirty = true;
    }

    /// Annule la dernière modification.
    fn undo(&mut self, window: &mut dyn WindowHandle) {
        // La saisie en cours n'est pas encore au document : on l'y porte,
        // sans quoi l'annulation défairait la modification d'avant.
        self.flush_typing();
        let Some(l) = &mut self.loaded else { return };
        let Some(op) = l.history.pop() else { return };
        let ops = l.history.clone();
        let mut redo = l.redo.clone();
        redo.push(op);
        log_line(&format!("annulation : {} restante(s)", ops.len()));
        self.replay(ops, redo, window);
    }

    /// Annule comme Ctrl+Z, d'où qu'on le demande (bouton, palette, clic
    /// droit) : d'abord une étape du bloc de texte en cours de saisie, puis
    /// la dernière modification du document.
    ///
    /// Aller droit à [`Viewer::undo`] refermerait le bloc — ce qui inscrit
    /// la frappe au document — puis annulerait aussitôt cette inscription :
    /// toute la saisie partirait d'un coup.
    fn undo_any(&mut self, window: &mut dyn WindowHandle) {
        if !self.undo_step(window) {
            self.undo(window);
        }
    }

    /// Rétablit comme Ctrl+Y : dans le bloc en cours de saisie d'abord.
    fn redo_any(&mut self, window: &mut dyn WindowHandle) {
        if !self.redo_step(window) {
            self.redo_edit(window);
        }
    }

    /// Rétablit la dernière modification annulée.
    fn redo_edit(&mut self, window: &mut dyn WindowHandle) {
        self.flush_typing();
        let Some(l) = &mut self.loaded else { return };
        let Some(op) = l.redo.pop() else { return };
        let redo = l.redo.clone();
        let mut ops = l.history.clone();
        ops.push(op);
        log_line(&format!("rétablissement : {} modification(s)", ops.len()));
        self.replay(ops, redo, window);
    }

    /// Pivote la page courante, ou celle d'un clic droit.
    fn rotate_current(&mut self, degrees: i32) {
        if self.loaded.is_none() {
            return;
        }
        let page = self.target_page();
        self.apply_edit(EditOp::Rotate {
            pages: vec![page],
            degrees,
        });
    }

    /// Supprime la page courante, ou celle d'un clic droit, après
    /// confirmation. La page est retenue dans la question : la réponse, qui
    /// arrive plus tard, supprime bien celle qui était visée.
    fn delete_current(&mut self) {
        let Some(l) = &self.loaded else { return };
        if l.pages.len() <= 1 {
            self.alert(
                "Suppression impossible",
                "Un document doit garder au moins une page.",
            );
            return;
        }
        // Le droit se vérifie avant de faire confirmer pour rien.
        if !self.require_right(crate::render_worker::Right::Assemble) {
            return;
        }
        let page = self.target_page();
        self.confirm(
            &format!("Supprimer la page {} ?", page + 1),
            "La page sera retirée du document. Ctrl+Z la rétablit ; Ctrl+S enregistre.",
            "Supprimer",
            Then::DeletePage(page),
        );
    }

    /// Enregistre (`save_as` : demande un nouveau chemin et réécrit tout).
    fn save(&mut self, save_as: bool, window: &mut dyn WindowHandle) -> bool {
        // Ce qui est tapé mais pas encore écrit doit l'être avant le fichier.
        self.flush_typing();
        let Some(l) = &self.loaded else { return false };
        // Un document fabriqué à partir d'une image n'a pas de fichier à lui :
        // le premier Ctrl+S demande où le ranger.
        let save_as = save_as || l.temporary;
        let suggested = l.path.file_name().map_or_else(
            || "document.pdf".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let target = if save_as {
            let Some(p) = window.save_file_dialog(&suggested) else {
                return false;
            };
            p
        } else {
            l.path.clone()
        };
        // Enregistrer sous, document réparé ou chiffrement changé :
        // réécriture complète ; sinon ajout incrémental.
        let bytes = if save_as || l.doc.was_repaired() || l.full_save {
            l.doc.save_full()
        } else {
            l.doc.save_incremental()
        };
        let bytes = match bytes {
            Ok(b) => b,
            Err(e) => {
                self.alert("Enregistrement impossible", &format!("{e}"));
                return false;
            }
        };
        // Écriture dans un fichier temporaire puis remplacement : jamais de fichier à moitié écrit.
        let tmp = target.with_extension("pdf.tmp");
        let written = std::fs::write(&tmp, &bytes).and_then(|()| std::fs::rename(&tmp, &target));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            self.alert(
                "Enregistrement impossible",
                &format!("{}\n\n{e}", target.display()),
            );
            return false;
        }
        log_line(&format!(
            "enregistré : {} ({} octets)",
            target.display(),
            bytes.len()
        ));
        if let Some(l) = &mut self.loaded {
            l.path = target;
            l.modified = false;
            // Le document a désormais un fichier à lui.
            l.temporary = false;
            // Le fichier enregistré devient la nouvelle base de l'annulation :
            // rejouer l'historique par-dessus le ferait une deuxième fois.
            l.history.clear();
            l.redo.clear();
        }
        self.title_dirty = true;
        true
    }

    /// Imprime le document (dialogue système).
    fn print(&mut self, window: &mut dyn WindowHandle) {
        if self.loaded.is_none() {
            return;
        }
        // On imprime ce qu'on voit, saisie en cours comprise.
        self.flush_typing();
        let Some(level) = self.print_level_or_refuse() else {
            return;
        };
        let Some(l) = &self.loaded else { return };
        let title = l
            .path
            .file_name()
            .map_or_else(|| "Acrux".to_string(), |n| n.to_string_lossy().into_owned());
        let mut source = PrintPages {
            doc: &l.doc,
            pages: &l.pages,
            max_dpi: (level == acrux_document::protect::PrintLevel::Low)
                .then_some(protect::LOW_PRINT_DPI),
        };
        match window.print(&title, &mut source) {
            PrintOutcome::Printed(n) => log_line(&format!("imprimé : {n} page(s)")),
            PrintOutcome::Cancelled => {}
            PrintOutcome::Failed(m) => self.alert("Impression impossible", &m),
        }
    }

    fn toggle_theme(&mut self, window: &mut dyn WindowHandle) {
        self.theme = if self.theme.is_dark() {
            Theme::light()
        } else {
            Theme::dark()
        };
        self.apply_frame_theme(window);
        self.save_prefs();
        log_line(&format!("thème basculé : canvas {:?}", self.theme.canvas));
    }

    /// Accorde la barre de titre du système au thème.
    fn apply_frame_theme(&self, window: &mut dyn WindowHandle) {
        let dark = self.theme.is_dark();
        // La barre de titre prolonge la barre d'outils : même couleur, même
        // texte. C'est ce qui fait que la fenêtre a l'air d'une seule pièce.
        window.set_frame_theme(dark, self.theme.bar, self.theme.text);
    }

    /// Exécute une action de la barre d'outils.
    fn tool_action(&mut self, action: ToolAction, window: &mut dyn WindowHandle) {
        match action {
            ToolAction::Home => self.show_home(window),
            ToolAction::Settings => self.open_settings(window),
            ToolAction::Open => {
                if let Some(p) = window.open_file_dialog() {
                    self.open(&p, window);
                }
            }
            ToolAction::PrevPage => self.step_row(false),
            ToolAction::NextPage => self.step_row(true),
            ToolAction::GoToPage(n) => {
                let count = self.loaded.as_ref().map_or(0, |l| l.pages.len());
                if count > 0 {
                    self.navigate(|v| v.scroll_to_page((n - 1).min(count - 1)));
                }
            }
            ToolAction::GoToLabel(text) => self.go_to_label(&text),
            ToolAction::ZoomOut => self.zoom_step(-1),
            ToolAction::ZoomIn => self.zoom_step(1),
            // Le bouton parcourt les trois ajustements : automatique,
            // largeur, page. Une liste déroulante pour trois choix serait plus
            // lourde à ouvrir qu'un clic de plus.
            ToolAction::FitWidth => self.set_fit(self.fit.next()),
            ToolAction::Search => self.open_search_ui(),
            ToolAction::ToggleTheme => self.toggle_theme(window),
            ToolAction::TogglePanel => self.toggle_panel(),
            ToolAction::ToggleTools => self.run_command(Command::ToggleTools, window),
            ToolAction::Undo => self.undo_any(window),
            ToolAction::Redo => self.redo_any(window),
            ToolAction::RotatePage => self.rotate_current(90),
            ToolAction::Save => {
                self.save(false, window);
            }
            ToolAction::Print => self.print(window),
            ToolAction::CycleViewMode => self.set_view_mode(self.view_mode.next()),
            ToolAction::ZoomMenu => self.open_zoom_menu(ZoomAnchor::Toolbar, window),
        }
    }

    /// Dessine la barre d'onglets sous la barre d'outils.
    fn paint_tabs(&mut self, frame: &mut Frame<'_>) {
        let height = self.tabs_height();
        if height == 0 {
            return;
        }
        let top = Toolbar::height(&self.theme, self.dpi_scale as f32);
        let infos = self.tab_infos();
        let active = self.active_tab.min(infos.len().saturating_sub(1));
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        let width = frame.width;
        let mut band = frame.sub(0, top, width, height);
        let Some(text) = &mut self.text else { return };
        self.tabs
            .paint(&mut band, text, &theme, dpi, &infos, active);
    }

    fn paint_toolbar(&mut self, frame: &mut Frame<'_>) {
        let info = self.toolbar_info();
        let theme = self.theme;
        let dpi = self.dpi_scale as f32;
        let Some(text) = &mut self.text else { return };
        self.toolbar
            .paint(frame, text, &mut self.raster, &theme, dpi, &info);
    }

    /// Échelle de rendu en pixels par point.
    ///
    /// Toujours bornée par le plafond de mémoire du zoom (voir
    /// `viewer/zoom.rs`) : `zoom_at` le fait respecter à chaque zoom
    /// demandé, mais un zoom fixe peut venir d'ailleurs — les réglages
    /// enregistrés (jusqu'à 1600 %), un autre onglet dont les pages étaient
    /// petites, un écran plus dense — et l'ajustement à la largeur d'une page
    /// très haute grandit sans limite. Sans cette borne, chacun de ces
    /// chemins rendait d'un bloc une page de plusieurs gigaoctets.
    fn scale(&self) -> f64 {
        let cent_pour_cent = self.dpi_scale * (96.0 / 72.0);
        let base = self.zoom * cent_pour_cent;
        let Some(l) = &self.loaded else { return base };
        // La plus grande page décide : autrement le document se remettrait à
        // l'échelle à chaque page tournée. La plus grande en surface fixe le
        // plafond, comme dans `max_zoom`.
        let ((max_w, max_h), largest) = l.pages.iter().take(50).fold(
            ((1.0_f64, 1.0_f64), (1.0_f64, 1.0_f64)),
            |((w, h), big), p| {
                let b = p.crop_box(&l.doc);
                let (bw, bh) = (b.width().abs(), b.height().abs());
                let big = if bw * bh > big.0 * big.1 {
                    (bw, bh)
                } else {
                    big
                };
                let (pw, ph) = match shown_rotation(l, p) {
                    90 | 270 => (b.height(), b.width()),
                    _ => (b.width(), b.height()),
                };
                ((w.max(pw), h.max(ph)), big)
            },
        );
        let ceiling = zoom::scale_ceiling(largest.0, largest.1, self.dpi_scale);
        if self.fit == Fit::Fixed {
            return base.min(ceiling);
        }
        // En deux pages, la largeur disponible se partage entre les deux
        // colonnes et la gouttière qui les sépare.
        let cols = if self.view_mode.is_two_up() { 2.0 } else { 1.0 };
        let gaps = f64::from(GAP) * (cols + 1.0);
        let avail_w = (f64::from(self.view_width()).max(64.0) - gaps).max(32.0);
        let largeur = (avail_w / (max_w * cols)).max(0.05);
        let fitted = match self.fit {
            Fit::Page => {
                let avail_h =
                    (f64::from(self.view_height()).max(64.0) - f64::from(GAP) * 2.0).max(32.0);
                largeur.min(avail_h / max_h).max(0.05)
            }
            // Automatique : la largeur, mais jamais d'agrandissement. Une page
            // plus étroite que la fenêtre s'affiche à sa taille réelle.
            Fit::Automatic => largeur.min(cent_pour_cent),
            Fit::Width | Fit::Fixed => largeur,
        };
        fitted.min(ceiling)
    }

    /// Positions des pages : (y, largeur, hauteur) en pixels.
    /// Regroupement des pages en rangées pour la disposition courante.
    fn rows(&self, count: usize) -> Vec<(usize, usize)> {
        page_rows(count, self.view_mode.is_two_up(), self.two_up_cover)
    }

    /// Rangée contenant la page d'ancrage (modes « une rangée à la fois »).
    fn anchor_row(&self, rows: &[(usize, usize)]) -> usize {
        row_of(rows, self.anchor)
    }

    /// Place de chaque page dans la vue.
    #[allow(clippy::many_single_char_names)] // indices de page et coordonnées
    fn layout(&self) -> Vec<PageBox> {
        let Some(l) = &self.loaded else {
            return Vec::new();
        };
        let scale = self.scale();
        let sizes: Vec<(u32, u32)> = l
            .pages
            .iter()
            .map(|p| page_pixel_size_rotated(&l.doc, p, scale, l.view_rotation))
            .collect();
        let rows = self.rows(sizes.len());
        let paged = self.view_mode.is_paged();
        let shown = self.anchor_row(&rows);
        let view_w = f64::from(self.view_width());
        let mut out = vec![PageBox::default(); sizes.len()];
        let mut y = GAP;
        for (index, &(first, last)) in rows.iter().enumerate() {
            if paged && index != shown {
                continue;
            }
            let in_row: Vec<usize> = (first..=last).collect();
            let row_width: i32 = in_row.iter().map(|&i| sizes[i].0 as i32).sum::<i32>()
                + GAP * (in_row.len() as i32 - 1);
            let row_height = in_row.iter().map(|&i| sizes[i].1).max().unwrap_or(0);
            // Rangée centrée si elle tient, sinon calée à gauche (le
            // défilement horizontal prend le relais).
            let x0 = if f64::from(row_width) + 2.0 * f64::from(GAP) < view_w {
                ((view_w - f64::from(row_width)) / 2.0).round() as i32
            } else {
                GAP
            };
            let mut x = x0;
            for &i in &in_row {
                let (w, h) = sizes[i];
                out[i] = PageBox {
                    x,
                    // Pages d'une même rangée alignées par le haut.
                    y,
                    w,
                    h,
                    visible: true,
                };
                x += w as i32 + GAP;
            }
            y += row_height as i32 + GAP;
        }
        out
    }

    /// Coin supérieur gauche d'une page à l'écran (défilement appliqué).
    fn page_screen(&self, layout: &[PageBox], page: usize) -> Option<(f64, f64)> {
        let b = layout.get(page).filter(|b| b.visible)?;
        Some((
            f64::from(b.x) - self.scroll_x,
            f64::from(b.y) - self.scroll_y,
        ))
    }

    fn total_height(&self) -> f64 {
        self.layout()
            .iter()
            .filter(|b| b.visible)
            .map(|b| f64::from(b.y + b.h as i32 + GAP))
            .fold(0.0, f64::max)
    }

    fn clamp_scroll(&mut self) {
        let max_y = (self.total_height() - f64::from(self.view_height())).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_y);
        // Un saut demandé ailleurs (aller à une page, zoomer) met fin au
        // glissement en cours : deux mouvements à la fois se battraient.
        if (self.scroll_y - self.scroll_seen).abs() > 0.01 {
            self.scroll_goal_y = None;
        }
        if let Some(goal) = &mut self.scroll_goal_y {
            *goal = goal.clamp(0.0, max_y);
        }
        self.scroll_seen = self.scroll_y;
        let max_w = self
            .layout()
            .iter()
            .filter(|b| b.visible)
            .map(|b| f64::from(b.x + b.w as i32 + GAP))
            .fold(0.0, f64::max);
        let max_x = (max_w - f64::from(self.view_width())).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
    }

    /// Page « courante » : celle dont la plus grande hauteur est visible
    /// (à égalité, la première). C'est le choix qui correspond à ce que
    /// l'utilisateur regarde, y compris en deux pages par rangée.
    fn current_page(&self) -> usize {
        if self.view_mode.is_paged() {
            return self.anchor;
        }
        let (top, bottom) = (self.scroll_y, self.scroll_y + f64::from(self.view_height()));
        let layout = self.layout();
        let mut best: Option<(f64, usize)> = None;
        for (i, b) in layout.iter().enumerate() {
            if !b.visible {
                continue;
            }
            let shown = f64::from(b.y + b.h as i32).min(bottom) - f64::from(b.y).max(top);
            if shown > 0.0 && best.is_none_or(|(s, _)| shown > s) {
                best = Some((shown, i));
            }
        }
        best.map_or_else(|| layout.len().saturating_sub(1), |(_, i)| i)
    }

    fn scroll_to_page(&mut self, index: usize) {
        let count = self.loaded.as_ref().map_or(0, |l| l.pages.len());
        if count == 0 {
            return;
        }
        let index = index.min(count - 1);
        if self.view_mode.is_paged() {
            self.anchor = index;
            self.scroll_y = 0.0;
            self.clamp_scroll();
            return;
        }
        if let Some(b) = self.layout().get(index) {
            self.scroll_y = f64::from(b.y - GAP);
            self.clamp_scroll();
        }
    }

    /// Rangée suivante ou précédente (boutons et Ctrl+PgSuiv / PgPréc).
    fn step_row(&mut self, forward: bool) {
        let count = self.loaded.as_ref().map_or(0, |l| l.pages.len());
        if count == 0 {
            return;
        }
        let rows = self.rows(count);
        let current = self.anchor_row(&rows);
        let next = if forward {
            (current + 1).min(rows.len() - 1)
        } else {
            current.saturating_sub(1)
        };
        self.scroll_to_page(rows[next].0);
    }

    // -----------------------------------------------------------------
    // Historique de la vue et rotation de la vue.
    // -----------------------------------------------------------------

    /// La vue actuelle, pour l'historique : la page en haut de l'écran et la
    /// part de sa hauteur déjà passée (voir `viewer/history.rs`).
    ///
    /// C'est la page du **haut** et non la « page courante » (la plus
    /// visible) : la part passée se mesure depuis le bord de la vue, et une
    /// page courante encore sous ce bord la ferait négative.
    fn view_spot(&self) -> Option<Spot> {
        self.loaded.as_ref()?;
        let layout = self.layout();
        let last = layout.len().checked_sub(1)?;
        let page = if self.view_mode.is_paged() {
            self.anchor.min(last)
        } else {
            layout
                .iter()
                .position(|b| b.visible && f64::from(b.y + b.h as i32) > self.scroll_y)
                .unwrap_or(last)
        };
        let b = layout.get(page)?;
        let frac = ((self.scroll_y - f64::from(b.y - GAP)) / f64::from(b.h.max(1))).clamp(0.0, 1.0);
        Some(Spot { page, frac })
    }

    /// Ramène la vue à `s`. Un glissement en cours s'arrête : la vue saute.
    fn restore_spot(&mut self, s: Spot) {
        let count = self.loaded.as_ref().map_or(0, |l| l.pages.len());
        if count == 0 {
            return;
        }
        let page = s.page.min(count - 1);
        self.scroll_to_page(page);
        if let Some(b) = self.layout().get(page) {
            self.scroll_y = f64::from(b.y - GAP) + s.frac * f64::from(b.h);
        }
        self.clamp_scroll();
    }

    /// Fait un **saut** : `jump` déplace la vue, et l'endroit quitté entre
    /// dans l'historique de l'onglet — si la vue a vraiment bougé. Un lien
    /// vers ce qu'on voit déjà n'y laisse rien : revenir en arrière ne
    /// ramènerait nulle part.
    fn navigate(&mut self, jump: impl FnOnce(&mut Self)) {
        let before = self.view_spot();
        let y = self.scroll_y;
        jump(self);
        let (Some(before), Some(after)) = (before, self.view_spot()) else {
            return;
        };
        let moved = after.page != before.page
            || (self.scroll_y - y).abs() > f64::from(self.view_height()) / 4.0;
        if !moved {
            return;
        }
        if let Some(l) = &mut self.loaded {
            l.nav.record(before);
            log_line(&format!(
                "historique : vue retenue (page {}), saut vers la page {}",
                before.page + 1,
                after.page + 1
            ));
        }
    }

    /// Vrai quand rien ne prend tout le clavier et la souris — ni la
    /// palette, ni une invite, ni la capture d'une signature — : la
    /// navigation (Alt+←, Ctrl+Tab, Ctrl+F4, rotation de la vue, boutons
    /// latéraux) a la main. Les questions, la fiche « Paramètres » et la
    /// fenêtre « Protéger » prennent l'événement avant d'en arriver là.
    fn navigation_keys_free(&self) -> bool {
        self.prompt.is_none() && self.palette.is_none() && self.capture.is_none()
    }

    /// Vue précédente (Alt+←, bouton « précédent » de la souris) ou
    /// suivante (Alt+→). Un saut sec, sans glissement : on sait où l'on va.
    pub(super) fn history_step(&mut self, forward: bool) {
        if self.showing_home() {
            return;
        }
        let Some(here) = self.view_spot() else { return };
        let target = self.loaded.as_mut().and_then(|l| {
            if forward {
                l.nav.forward(here)
            } else {
                l.nav.back(here)
            }
        });
        let which = if forward { "suivante" } else { "précédente" };
        let Some(spot) = target else {
            log_line(&format!("historique : pas de vue {which}"));
            self.set_notice(
                lang::tr(if forward {
                    "Aucune vue suivante"
                } else {
                    "Aucune vue précédente"
                })
                .into(),
            );
            return;
        };
        self.restore_spot(spot);
        self.title_dirty = true;
        log_line(&format!(
            "historique : vue {which} → page {}",
            spot.page + 1
        ));
    }

    /// Tourne la **vue** d'un quart de tour (Ctrl+Maj+Plus, Ctrl+Maj+Moins) :
    /// les pages s'affichent tournées, le document ne change pas. Rien à
    /// enregistrer ni à annuler, et l'on garde sa page à l'écran.
    ///
    /// C'est ce qui sert à lire un tableau couché ou un plan scanné de
    /// travers sans toucher au fichier — pivoter la page (R, Maj+R), elle,
    /// écrit `/Rotate`.
    pub(super) fn rotate_view(&mut self, degrees: i32) {
        if self.showing_home() {
            return;
        }
        // La saisie en cours a été mise en page pour l'orientation d'avant :
        // elle part au document, le mode reste ouvert.
        if self.edit.as_ref().is_some_and(|e| e.active.is_some()) {
            self.close_active();
        }
        self.commit_field();
        let spot = self.view_spot();
        let Some(l) = &mut self.loaded else { return };
        l.view_rotation = add_rotation(l.view_rotation, degrees);
        // Les images rendues, vignettes comprises, montrent l'ancienne
        // orientation.
        l.cache.clear();
        if let Some(w) = &mut l.worker {
            w.set_view_rotation(l.view_rotation);
        }
        let now = l.view_rotation;
        if let Some(spot) = spot {
            self.restore_spot(spot);
        }
        self.title_dirty = true;
        log_line(&format!("vue pivotée : {now}°"));
        self.set_notice(if now == 0 {
            lang::tr("Vue remise droite").into()
        } else {
            lang::trf(
                "Vue pivotée de {}° (le document n'est pas modifié)",
                &[&now.to_string()],
            )
        });
    }

    /// Nombre d'onglets ouverts.
    fn tab_count(&self) -> usize {
        self.others.len() + usize::from(self.loaded.is_some())
    }

    /// Document de l'onglet `index`, qu'il soit actif ou non. L'onglet actif
    /// n'est pas dans `others` : les suivants y sont décalés d'un rang.
    fn tab_doc(&self, index: usize) -> Option<&Loaded> {
        if index == self.active_tab {
            return self.loaded.as_ref();
        }
        let at = if index > self.active_tab {
            index - 1
        } else {
            index
        };
        self.others.get(at)
    }

    /// Titres des onglets, dans l'ordre d'affichage.
    fn tab_infos(&self) -> Vec<TabInfo> {
        let name = |l: &Loaded| TabInfo {
            title: l.path.file_name().map_or_else(
                || "document".to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
            // Le chemin d'un document pas encore enregistré serait celui
            // d'un fichier temporaire : il n'apprendrait rien.
            path: if l.temporary {
                String::new()
            } else {
                l.path.display().to_string()
            },
            modified: l.modified,
        };
        let mut out: Vec<TabInfo> = self.others.iter().map(name).collect();
        if let Some(l) = &self.loaded {
            out.insert(self.active_tab.min(out.len()), name(l));
        }
        out
    }

    /// Remet le document actif dans la liste et en active un autre.
    fn select_tab(&mut self, index: usize) {
        // La saisie porte sur **ce** document : on l'y écrit avant d'en
        // changer.
        self.flush_typing();
        self.leave_home();
        self.edit = None;
        self.close_annot_tools();
        if index == self.active_tab || index >= self.tab_count() {
            return;
        }
        let spot = self.view_spot();
        let Some(mut current) = self.loaded.take() else {
            return;
        };
        current.resume = spot;
        let at = self.active_tab.min(self.others.len());
        self.others.insert(at, current);
        // `index` désigne une position dans la liste complète, qui est
        // maintenant exactement `others`.
        self.loaded = Some(self.others.remove(index));
        self.active_tab = index;
        self.reset_view_state();
        self.resume_view();
    }

    /// Retrouve la vue que l'onglet actif avait quand on l'a quitté. Passer
    /// d'un onglet à l'autre ramenait chacun à sa première page, comme si on
    /// venait de l'ouvrir.
    fn resume_view(&mut self) {
        if let Some(spot) = self.loaded.as_mut().and_then(|l| l.resume.take()) {
            self.restore_spot(spot);
        }
    }

    /// Ferme un onglet (en proposant d'enregistrer s'il a des modifications).
    fn close_tab(&mut self, index: usize, window: &mut dyn WindowHandle) {
        self.guard_close_tab(index, window);
    }

    /// Bouton du milieu enfoncé. Sur un onglet, il le ferme, comme dans les
    /// navigateurs ; sur la page, il la saisit pour la faire glisser.
    ///
    /// Dans les barres, il ne saisit jamais la page : il la faisait défiler
    /// depuis un point hors de la vue, et un clic du milieu sur un onglet
    /// aurait fermé l'onglet **et** fait glisser le document suivant.
    fn middle_down(&mut self, x: i32, y: i32, window: &mut dyn WindowHandle) {
        let top = self.view_top() as i32;
        if y >= top {
            self.drag_last = Some((x - self.view_left() as i32, y - top));
            return;
        }
        let bar = Toolbar::height(&self.theme, self.dpi_scale as f32);
        let tabs = self.tabs_height() as i32;
        // Même garde que le clic gauche : une carte, la palette ou une
        // liste ouverte ont la main.
        if y < bar || y >= bar + tabs || self.menu_blocked() {
            return;
        }
        if let TabAction::Close(index) = self.tabs.middle_click(x, y - bar) {
            log_line(&format!(
                "onglet {index} : clic du milieu, fermeture demandée"
            ));
            self.tip = None;
            // Fermer passe par la même garde que la croix : un document
            // modifié demande d'abord s'il faut l'enregistrer.
            self.close_tab(index, window);
            // Les onglets se sont décalés : le survol d'avant désignerait
            // le voisin.
            self.tabs.mouse_leave();
            window.request_redraw();
        }
    }

    /// Ferme un onglet sans rien demander.
    fn close_tab_now(&mut self, index: usize) {
        if index >= self.tab_count() {
            return;
        }
        if index != self.active_tab {
            // Position dans `others` : l'onglet actif n'y est pas encore.
            let at = if index > self.active_tab {
                index - 1
            } else {
                index
            };
            if at >= self.others.len() {
                return;
            }
            self.others.remove(at);
            if index < self.active_tab {
                self.active_tab -= 1;
            }
            self.title_dirty = true;
            return;
        }
        self.edit = None;
        self.close_annot_tools();
        self.loaded = None;
        if self.others.is_empty() {
            self.active_tab = 0;
        } else {
            let next = self.active_tab.min(self.others.len() - 1);
            self.loaded = Some(self.others.remove(next));
            self.active_tab = next;
        }
        self.reset_view_state();
        // L'onglet qui prend la place retrouve sa vue.
        self.resume_view();
        self.title_dirty = true;
    }

    /// Onglet suivant ou précédent (Ctrl+Tab).
    fn cycle_tab(&mut self, forward: bool) {
        let count = self.tab_count();
        if count < 2 {
            return;
        }
        let next = if forward {
            (self.active_tab + 1) % count
        } else {
            (self.active_tab + count - 1) % count
        };
        self.select_tab(next);
    }

    /// Remet à zéro ce qui dépend du document affiché.
    fn reset_view_state(&mut self) {
        // La fenêtre « Protéger » visait l'ancien document actif.
        self.protect = None;
        // Le message passager aussi : « Vue pivotée de 90° » restait affiché
        // sur l'accueil, ou sous le document voisin, qui n'est pas tourné.
        self.notice = None;
        self.scroll_x = 0.0;
        self.scroll_y = 0.0;
        self.anchor = 0;
        self.selection = None;
        self.close_search();
        self.focus_field = None;
        self.field_edit = None;
        self.field_menu = None;
        self.list_anchor = None;
        self.panel = Panel::new();
        self.title_dirty = true;
    }

    /// Hauteur de la barre d'onglets (0 s'il n'y en a qu'un).
    fn tabs_height(&self) -> u32 {
        if self.fullscreen || self.reading || self.tab_count() < 2 {
            return 0;
        }
        Tabs::height(&self.theme, self.dpi_scale as f32) as u32
    }

    /// Vrai si la vue est arrivée en bas du document.
    fn at_bottom(&self) -> bool {
        self.scroll_y + f64::from(self.view_height()) >= self.total_height() - 0.5
    }

    // -----------------------------------------------------------------
    // Vidéos et sons.
    // -----------------------------------------------------------------

    /// Vrai si un média est en train de jouer : c'est ce qui justifie de
    /// repeindre à chaque réveil, et rien d'autre ne le justifierait.
    fn media_playing(&self) -> bool {
        self.media
            .as_ref()
            .is_some_and(|v| v.player.state() == crate::platform::media::State::Playing)
    }

    /// Média sous un point de la vue, s'il y en a un.
    fn media_at(&self, x: i32, y: i32) -> Option<(usize, acrux_features::media::Media)> {
        let loaded = self.loaded.as_ref()?;
        let (page, point) = self.page_at(x, y)?;
        loaded
            .media
            .iter()
            .enumerate()
            .find(|(_, m)| {
                m.page == page
                    && point.x >= m.rect.x0
                    && point.x <= m.rect.x1
                    && point.y >= m.rect.y0
                    && point.y <= m.rect.y1
            })
            .map(|(slot, m)| (slot, m.clone()))
    }

    /// Rectangle d'un média ouvert, en pixels de la vue.
    fn media_box(&self) -> Option<Box2> {
        let view = self.media.as_ref()?;
        let rect = self.page_rect_to_view(view.page, view.rect)?;
        Some(Box2 {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
        })
    }

    /// Ouvre le média cliqué et le démarre.
    ///
    /// Rien n'est joué tout seul : c'est un clic qui déclenche la lecture. Un
    /// média **hors du document** n'est pas ouvert du tout — un fichier PDF
    /// est une donnée venue d'ailleurs, et suivre ce qu'il désigne sans rien
    /// demander reviendrait à exécuter ses instructions.
    fn open_media(&mut self, media: &acrux_features::media::Media, window: &mut dyn WindowHandle) {
        use acrux_features::media::Source;
        match &media.source {
            Some(Source::Embedded { .. } | Source::Samples { .. }) => {}
            Some(Source::External(name)) => {
                self.set_notice(format!(
                    "« {name} » est hors du document : Acrux ne l'ouvre pas de lui-même"
                ));
                return;
            }
            None => {
                self.set_notice("ce média n'a pas de fichier lisible".into());
                return;
            }
        }
        // Déjà ouvert : le clic bascule lecture et pause.
        if self
            .media
            .as_ref()
            .is_some_and(|v| v.page == media.page && v.index == media.index)
        {
            self.toggle_media(window);
            return;
        }
        self.close_media();

        let Some(loaded) = &self.loaded else { return };
        let dir = std::env::temp_dir().join("acrux-medias");
        let path = match acrux_features::media::extract(&loaded.doc, media, &dir) {
            Ok(path) => path,
            Err(e) => {
                self.set_notice(format!("média illisible : {e}"));
                return;
            }
        };
        let mut player = match crate::platform::media::Player::open(&path) {
            Ok(player) => player,
            Err(e) => {
                self.set_notice(format!("lecture impossible : {e}"));
                return;
            }
        };
        player.play();
        let title = media
            .name
            .clone()
            .unwrap_or_else(|| String::from("le média"));
        let ticking = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        self.media = Some(MediaView {
            page: media.page,
            index: media.index,
            rect: media.rect,
            player,
            title: title.clone(),
            ticking: std::sync::Arc::clone(&ticking),
        });
        Self::start_media_ticker(&ticking, window);
        self.set_notice(format!("lecture de {title}"));
    }

    /// Fait battre la mesure : un réveil régulier tant que le média joue.
    ///
    /// La fenêtre n'a pas de minuterie et ne repeint que sur événement ; sans
    /// ce fil, une vidéo s'arrêterait sur sa première image dès que
    /// l'utilisateur cesse de bouger la souris.
    fn start_media_ticker(
        ticking: &std::sync::Arc<std::sync::atomic::AtomicBool>,
        window: &mut dyn WindowHandle,
    ) {
        let waker = window.waker();
        let flag = std::sync::Arc::clone(ticking);
        let _ = std::thread::Builder::new()
            .name("cadence-video".into())
            .spawn(move || {
                // Soixante fois par seconde : assez pour toutes les cadences
                // usuelles, assez peu pour ne pas occuper un cœur.
                while flag.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(16));
                    waker.wake();
                }
            });
    }

    /// Ferme le média en cours, s'il y en a un.
    fn close_media(&mut self) {
        if let Some(view) = self.media.take() {
            view.ticking
                .store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Bascule entre lecture et pause.
    fn toggle_media(&mut self, window: &mut dyn WindowHandle) {
        let Some(view) = &mut self.media else { return };
        view.player.toggle();
        let playing = view.player.state() == crate::platform::media::State::Playing;
        let title = view.title.clone();
        if playing {
            let ticking = std::sync::Arc::clone(&view.ticking);
            if !ticking.swap(true, std::sync::atomic::Ordering::Relaxed) {
                Self::start_media_ticker(&ticking, window);
            }
            self.set_notice(format!("lecture de {title}"));
        } else {
            view.ticking
                .store(false, std::sync::atomic::Ordering::Relaxed);
            self.set_notice(format!("{title} en pause"));
        }
        window.request_redraw();
    }

    /// Clic dans la zone de document quand un média est ouvert. Rend vrai si
    /// le clic a été consommé.
    fn media_mouse_down(&mut self, x: i32, y: i32, window: &mut dyn WindowHandle) -> bool {
        let Some(area) = self.media_box() else {
            return false;
        };
        let Some(hit) = video_ui::hit(area, f64::from(x), f64::from(y), self.dpi_scale) else {
            return false;
        };
        match hit {
            VideoHit::Toggle => self.toggle_media(window),
            VideoHit::Seek(fraction) => {
                if let Some(view) = &mut self.media {
                    let target = view.player.duration() * fraction;
                    view.player.seek(target);
                }
                window.request_redraw();
            }
            VideoHit::Picture => {}
        }
        true
    }

    /// Dessine l'image courante du média et sa barre de commandes.
    fn paint_media(&mut self, view_frame: &mut Frame<'_>) {
        let Some(area) = self.media_box() else { return };
        let (theme, dpi) = (self.theme, self.dpi_scale);
        let Some(view) = &mut self.media else { return };
        let (playing, position, duration) = (
            view.player.state() == crate::platform::media::State::Playing,
            view.player.position(),
            view.player.duration(),
        );
        // Un média sans image — un son — doit quand même se voir : sinon les
        // commandes flottent sur le texte de la page et rien ne dit qu'il y a
        // là quelque chose à écouter.
        let sans_image = view.player.size().is_none();
        if let Some(frame) = view.player.frame() {
            let (w, h) = (frame.width, frame.height);
            // L'image garde ses proportions dans le rectangle de l'annotation,
            // comme le ferait un lecteur vidéo — une vidéo étirée se voit.
            let fitted = fit_box(area, w, h);
            video_ui::paint_frame(view_frame, fitted, &frame.bgra, w, h);
        } else if sans_image {
            video_ui::paint_audio_panel(view_frame, area);
        }
        if let Some(text) = &mut self.text {
            video_ui::paint_controls(
                view_frame, text, &theme, dpi, area, playing, position, duration,
            );
        }
    }

    // -----------------------------------------------------------------
    // Mises à jour.
    // -----------------------------------------------------------------

    /// Lance la recherche d'une version plus récente, sur un fil à part.
    ///
    /// `asked` distingue la recherche demandée par l'utilisateur — qui mérite
    /// une réponse même quand il n'y a rien de neuf — de celle du démarrage,
    /// qui reste silencieuse.
    fn check_updates(&mut self, asked: bool, window: &mut dyn WindowHandle) {
        if self.update_rx.is_some() {
            return;
        }
        if asked {
            self.set_notice("recherche d'une version plus récente…".into());
        }
        self.prefs.last_update_check = today();
        self.prefs.save();
        let (tx, rx) = std::sync::mpsc::channel();
        self.update_rx = Some(rx);
        let waker = window.waker();
        // Un fil jetable : la requête peut prendre plusieurs secondes, et
        // l'interface ne doit pas attendre avec elle.
        let spawned = std::thread::Builder::new()
            .name("mises-a-jour".into())
            .spawn(move || {
                let _ = tx.send(crate::update::latest());
                waker.wake();
            });
        if spawned.is_err() {
            self.update_rx = None;
        }
        self.update_asked = asked;
    }

    /// Relève le résultat de la recherche, s'il est arrivé.
    fn poll_updates(&mut self) -> bool {
        let Some(rx) = &self.update_rx else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.update_rx = None;
                return false;
            }
        };
        self.update_rx = None;
        let asked = self.update_asked;
        self.update_outcome = Some(outcome.as_ref().map(|_| ()).map_err(Clone::clone));
        match outcome {
            Ok(release) if crate::update::newer(&release.version, env!("CARGO_PKG_VERSION")) => {
                self.set_notice(format!(
                    "Acrux {} est disponible — « Installer la mise à jour » dans la palette",
                    release.version
                ));
                self.update_found = Some(release);
            }
            Ok(_) if asked => self.set_notice("Acrux est à jour.".into()),
            Err(message) if asked => self.set_notice(format!("mises à jour : {message}")),
            // Recherche du démarrage : rien de neuf, ou réseau absent. On ne
            // dérange pas l'utilisateur pour cela.
            Ok(_) | Err(_) => {}
        }
        true
    }

    /// Télécharge et lance l'installateur de la version trouvée.
    ///
    /// Rien ne part sans un oui explicite : c'est un programme qu'on va
    /// exécuter, et l'utilisateur doit savoir lequel et d'où il vient.
    fn install_update(&mut self, window: &mut dyn WindowHandle) {
        let Some(release) = self.update_found.clone() else {
            self.check_updates(true, window);
            return;
        };
        let Some(url) = release.installer.clone() else {
            self.set_notice(format!(
                "Acrux {} n'a pas d'installateur : voir {}",
                release.version, release.page
            ));
            return;
        };
        self.confirm(
            &format!("Installer Acrux {} ?", release.version),
            &format!(
                "L'installateur sera téléchargé depuis {url}, puis lancé pour \
                 remplacer la version en place. Acrux devra être relancé ensuite.",
            ),
            "Installer",
            Then::InstallUpdate(url, release.version.clone()),
        );
    }

    /// Télécharge et lance l'installateur, une fois l'accord donné.
    fn install_update_now(&mut self, url: &str, version: &str) {
        self.set_notice("téléchargement de la mise à jour…".into());
        match crate::update::download(url, version) {
            Ok(path) => {
                self.save_prefs();
                match std::process::Command::new(&path).arg("--silent").spawn() {
                    Ok(_) => self.set_notice(format!(
                        "Acrux {version} s'installe — relancez l'application pour en profiter"
                    )),
                    Err(e) => self.set_notice(format!("l'installateur n'a pas démarré : {e}")),
                }
            }
            Err(message) => self.set_notice(format!("mise à jour : {message}")),
        }
    }

    // -----------------------------------------------------------------
    // Modifier les objets de la page.
    // -----------------------------------------------------------------

    /// Ouvre ou ferme l'outil « modifier ».
    fn toggle_objects(&mut self, window: &mut dyn WindowHandle) {
        self.commit_field();
        self.leave_home();
        self.edit = None;
        self.close_annot_tools();
        if self.objects.is_some() {
            self.objects = None;
            self.set_notice("modification des objets : terminé".into());
        } else if self.loaded.is_some() && self.require_right(crate::render_worker::Right::Modify) {
            let page = self.current_page();
            self.load_objects(page);
            let count = self.objects.as_ref().map_or(0, |t| t.objects.len());
            self.set_notice(format!(
                "{count} objet(s) en page {} : cliquez pour sélectionner",
                page + 1
            ));
        }
        window.request_redraw();
    }

    /// Recharge l'inventaire des objets d'une page.
    fn load_objects(&mut self, page: usize) {
        let Some(l) = &self.loaded else { return };
        let Some(target) = l.pages.get(page) else {
            return;
        };
        let objects = edit_objects::list(&l.doc, target).unwrap_or_default();
        self.objects = Some(ObjectTool {
            page,
            objects,
            selected: None,
            hover: None,
            handle: None,
            drag: None,
        });
    }

    /// Rectangle d'une boîte de page dans la vue, ou `None` si la page n'est
    /// pas à l'écran.
    #[allow(clippy::many_single_char_names)] // coordonnées et matrices
    fn page_rect_to_view(&self, page: usize, rect: Rect) -> Option<ViewRect> {
        let l = self.loaded.as_ref()?;
        let layout = self.layout();
        let (ox, oy) = self.page_screen(&layout, page)?;
        let PageBox { w, h, .. } = *layout.get(page)?;
        let p = l.pages.get(page)?;
        let m = base_matrix(
            &p.crop_box(&l.doc),
            self.scale(),
            shown_rotation(l, p),
            w,
            h,
        );
        let a = m.apply(Point::new(rect.x0, rect.y0));
        let b = m.apply(Point::new(rect.x1, rect.y1));
        let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
        let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
        Some(ViewRect {
            x: x0 + ox,
            y: y0 + oy,
            w: x1 - x0,
            h: y1 - y0,
        })
    }

    /// Fait défiler la vue pour qu'un rectangle d'une page se voie en
    /// entier ; s'il est déjà visible, rien ne bouge.
    fn reveal_page_rect(&mut self, page: usize, rect: Rect) {
        if self.view_mode.is_paged() && self.anchor != page {
            self.scroll_to_page(page);
        }
        let Some(r) = self.page_rect_to_view(page, rect) else {
            return;
        };
        let vh = f64::from(self.view_height());
        if r.y < 0.0 || r.y + r.h > vh {
            self.scroll_y = (self.scroll_y + r.y - vh / 3.0).max(0.0);
            self.clamp_scroll();
        }
    }

    /// Objet sous un point de la vue : le plus petit qui le contient, donc le
    /// plus précis — sinon un fond de page avalerait tous les clics.
    fn object_at(&self, x: i32, y: i32) -> Option<usize> {
        let tool = self.objects.as_ref()?;
        let (fx, fy) = (f64::from(x), f64::from(y));
        let mut best: Option<(f64, usize)> = None;
        for (index, object) in tool.objects.iter().enumerate() {
            if object.bbox.width() <= 0.0 || object.bbox.height() <= 0.0 {
                continue;
            }
            let Some(view) = self.page_rect_to_view(tool.page, object.bbox) else {
                continue;
            };
            if view.contains(fx, fy) && best.is_none_or(|(area, _)| view.area() < area) {
                best = Some((view.area(), index));
            }
        }
        best.map(|(_, index)| index)
    }

    /// Clic dans la zone de document quand l'outil est actif. Rend vrai si le
    /// clic a été consommé.
    fn objects_mouse_down(&mut self, x: i32, y: i32, shift: bool) -> bool {
        let Some(tool) = &self.objects else {
            return false;
        };
        let (page, selected) = (tool.page, tool.selected);
        let (fx, fy) = (f64::from(x), f64::from(y));
        // Une poignée de la sélection courante l'emporte sur tout le reste.
        if let Some(index) = selected {
            if let Some(object) = tool.objects.get(index) {
                if let Some(view) = self.page_rect_to_view(page, object.bbox) {
                    if let Some(handle) = objects_ui::handle_at(view, fx, fy, self.dpi_scale) {
                        let bbox = object.bbox;
                        if let Some((_, from)) = self.page_at(x, y) {
                            if let Some(t) = &mut self.objects {
                                t.drag = Some(ObjectDrag {
                                    handle,
                                    from,
                                    start: bbox,
                                    current: bbox,
                                    keep_ratio: shift,
                                });
                            }
                            return true;
                        }
                    }
                }
            }
        }
        let found = self.object_at(x, y);
        if let Some(t) = &mut self.objects {
            t.selected = found;
            t.drag = None;
        }
        found.is_some()
    }

    /// Déplacement du pointeur quand l'outil est actif.
    fn objects_mouse_move(&mut self, x: i32, y: i32, dragging: bool) -> bool {
        if self.objects.is_none() {
            return false;
        }
        let (fx, fy) = (f64::from(x), f64::from(y));
        let page = self.objects.as_ref().map_or(0, |t| t.page);
        let drag = self.objects.as_ref().and_then(|t| t.drag);
        if dragging {
            if let Some(mut d) = drag {
                let Some((_, to)) = self.page_at(x, y) else {
                    return true;
                };
                d.current = objects_ui::resized(
                    d.start,
                    d.handle,
                    to.x - d.from.x,
                    to.y - d.from.y,
                    d.keep_ratio,
                );
                if let Some(t) = &mut self.objects {
                    t.drag = Some(d);
                }
                return true;
            }
        }
        // Survol : quel objet, et quelle poignée de la sélection.
        let hover = self.object_at(x, y);
        let selected = self.objects.as_ref().and_then(|t| t.selected);
        let handle = selected
            .and_then(|index| self.objects.as_ref()?.objects.get(index).map(|o| o.bbox))
            .and_then(|bbox| self.page_rect_to_view(page, bbox))
            .and_then(|view| objects_ui::handle_at(view, fx, fy, self.dpi_scale));
        if let Some(t) = &mut self.objects {
            let changed = t.hover != hover || t.handle != handle;
            t.hover = hover;
            t.handle = handle;
            return changed;
        }
        false
    }

    /// Fin d'un geste : la transformation est envoyée au document.
    fn objects_mouse_up(&mut self) {
        let Some(tool) = &self.objects else { return };
        let (Some(drag), Some(index)) = (tool.drag, tool.selected) else {
            return;
        };
        let page = tool.page;
        if let Some(t) = &mut self.objects {
            t.drag = None;
        }
        // Un geste qui n'a rien bougé ne mérite pas une modification.
        let moved = (drag.current.x0 - drag.start.x0).abs()
            + (drag.current.y0 - drag.start.y0).abs()
            + (drag.current.width() - drag.start.width()).abs()
            + (drag.current.height() - drag.start.height()).abs();
        if moved < 0.05 {
            return;
        }
        let matrix = edit_objects::fit(drag.start, drag.current);
        self.apply_object_edit(page, vec![ObjectEdit::Transform { index, matrix }]);
    }

    /// Envoie des modifications d'objets et recharge l'inventaire.
    fn apply_object_edit(&mut self, page: usize, edits: Vec<ObjectEdit>) {
        if edits.is_empty() {
            return;
        }
        self.apply_edit(EditOp::EditObject { page, edits });
        // L'inventaire d'après l'édition : les plages d'octets ont changé.
        let selected = self.objects.as_ref().and_then(|t| t.selected);
        self.load_objects(page);
        if let (Some(t), Some(index)) = (&mut self.objects, selected) {
            if index < t.objects.len() {
                t.selected = Some(index);
            }
        }
    }

    /// Touche pressée quand l'outil est actif.
    fn objects_key(&mut self, key: Key) -> bool {
        let Some(tool) = &self.objects else {
            return false;
        };
        let (page, Some(index)) = (tool.page, tool.selected) else {
            return false;
        };
        let step = 1.0;
        let edit = match key {
            Key::Delete | Key::Backspace => ObjectEdit::Delete { index },
            Key::Left => ObjectEdit::Transform {
                index,
                matrix: Matrix::translate(-step, 0.0),
            },
            Key::Right => ObjectEdit::Transform {
                index,
                matrix: Matrix::translate(step, 0.0),
            },
            Key::Up => ObjectEdit::Transform {
                index,
                matrix: Matrix::translate(0.0, step),
            },
            Key::Down => ObjectEdit::Transform {
                index,
                matrix: Matrix::translate(0.0, -step),
            },
            _ => return false,
        };
        let removing = matches!(edit, ObjectEdit::Delete { .. });
        self.apply_object_edit(page, vec![edit]);
        if removing {
            if let Some(t) = &mut self.objects {
                t.selected = None;
            }
        }
        true
    }

    /// Caractère tapé quand l'outil est actif : l'ordre de superposition.
    fn objects_char(&mut self, c: char) -> bool {
        let Some(tool) = &self.objects else {
            return false;
        };
        let (page, Some(index)) = (tool.page, tool.selected) else {
            return false;
        };
        let to = match c {
            ']' => edit_objects::Order::Forward,
            '}' => edit_objects::Order::Front,
            '[' => edit_objects::Order::Backward,
            '{' => edit_objects::Order::Back,
            _ => return false,
        };
        self.apply_object_edit(page, vec![ObjectEdit::Arrange { index, to }]);
        true
    }

    /// Dessine le survol, la sélection et l'aperçu du geste en cours.
    fn paint_objects(&mut self, view: &mut Frame<'_>) {
        let Some(tool) = &self.objects else { return };
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        let page = tool.page;
        let hover = tool
            .hover
            .filter(|h| Some(*h) != tool.selected)
            .and_then(|h| tool.objects.get(h))
            .map(|o| o.bbox);
        let selection = match tool.drag {
            Some(d) => Some(d.current),
            None => tool
                .selected
                .and_then(|i| tool.objects.get(i))
                .map(|o| o.bbox),
        };
        let handle = tool.handle;
        if let Some(rect) = hover {
            if let Some(v) = self.page_rect_to_view(page, rect) {
                objects_ui::paint_hover(view, &theme, dpi, v);
            }
        }
        if let Some(rect) = selection {
            if let Some(v) = self.page_rect_to_view(page, rect) {
                objects_ui::paint_selection(view, &theme, dpi, v, handle);
            }
        }
    }

    // -----------------------------------------------------------------
    // Remplir et signer.
    // -----------------------------------------------------------------

    /// Ouvre ou ferme l'outil « remplir et signer ».
    ///
    /// L'outil s'ouvre **sans rien en main** : on y vient autant pour cocher
    /// une case ou taper une date que pour signer, et se voir imposer la
    /// fenêtre de signature — ou une signature collée au pointeur — à chaque
    /// ouverture gênait plus que cela n'aidait. On choisit dans le panneau.
    fn toggle_fillsign(&mut self, window: &mut dyn WindowHandle) {
        self.commit_field();
        self.leave_home();
        if self.loaded.is_none() {
            return;
        }
        self.edit = None;
        self.close_annot_tools();
        if self.sign_panel.is_some() {
            self.sign_panel = None;
            self.capture = None;
            self.wake_anim();
            self.set_notice("remplir et signer : terminé".into());
        } else if self.require_right(crate::render_worker::Right::Annotate) {
            self.sign_panel = Some(self.new_sign_panel());
            self.wake_anim();
            self.pick_sign_item(SignItem::Move);
            self.set_notice("remplir et signer : choisissez quoi poser dans le panneau".into());
        }
        self.clamp_scroll();
        window.request_redraw();
    }

    /// Choisit ce qui sera posé au prochain clic.
    fn pick_sign_item(&mut self, item: SignItem) {
        // Changer d'élément arrête l'écriture en cours sur la page.
        if self.edit_overlay() {
            self.leave_edit();
        }
        let missing = match item {
            SignItem::Signature => self.signatures.is_empty(),
            SignItem::Initials => self.initials.is_none(),
            _ => false,
        };
        if missing {
            self.capture = Some(self.new_capture(item == SignItem::Initials));
            return;
        }
        if let Some(panel) = &mut self.sign_panel {
            panel.item = Some(item);
        }
        self.set_notice(format!("{} : cliquez sur la page", item.label()));
    }

    /// Suite d'une action venue de la barre ou de la fenêtre de capture.
    fn sign_action(&mut self, action: sign::Action, window: &mut dyn WindowHandle) {
        match action {
            sign::Action::None => return,
            sign::Action::Redraw => {}
            sign::Action::Pick(item) => self.pick_sign_item(item),
            sign::Action::Ink(index) => {
                self.prefs.sign_color = u8::try_from(index).unwrap_or(0);
                self.prefs.save();
                self.set_notice(format!(
                    "encre : {}",
                    sign::INKS[index.min(sign::INKS.len() - 1)].0
                ));
            }
            sign::Action::Style(nib, weight) => {
                self.prefs.sign_nib = nib.index();
                self.prefs.sign_weight = weight.index();
                self.prefs.save();
                self.set_notice(format!("{} {}", nib.label(), weight.label().to_lowercase()));
            }
            sign::Action::Import(initials) => {
                let _ = initials;
                if let Some(path) = window.open_file_dialog() {
                    if let Some(c) = &mut self.capture {
                        c.set_image(path);
                    }
                }
            }
            sign::Action::Save(initials, saved) => {
                self.remember_signature(initials, saved);
                self.capture = None;
                let item = if initials {
                    SignItem::Initials
                } else {
                    SignItem::Signature
                };
                if self.sign_panel.is_none() {
                    self.sign_panel = Some(self.new_sign_panel());
                }
                self.pick_sign_item(item);
            }
            sign::Action::Close => {
                if self.capture.is_some() {
                    self.capture = None;
                } else {
                    self.sign_panel = None;
                    self.set_notice("remplir et signer : terminé".into());
                }
                self.clamp_scroll();
            }
        }
        window.request_redraw();
    }

    /// Enregistre une signature et la conserve pour les sessions suivantes.
    fn remember_signature(&mut self, initials: bool, saved: Saved) {
        if initials {
            self.initials = Some(saved);
        } else if let Some(slot) = self.replacing.take().filter(|i| *i < self.signatures.len()) {
            // On refaisait celle-ci : elle garde sa place dans la liste.
            self.signatures[slot] = saved;
            if let Some(panel) = &mut self.sign_panel {
                panel.current = slot;
            }
        } else {
            if self.signatures.len() >= crate::ui::prefs::MAX_SIGNATURES {
                self.signatures.remove(0);
            }
            self.signatures.push(saved);
            if let Some(panel) = &mut self.sign_panel {
                panel.current = self.signatures.len() - 1;
            }
        }
        self.store_signatures();
    }

    /// Recopie les signatures dans les préférences et enregistre.
    fn store_signatures(&mut self) {
        self.prefs.signatures = self.signatures.iter().map(Saved::encode).collect();
        self.prefs.initials = self.initials.as_ref().map(Saved::encode);
        self.prefs.save();
    }

    /// Pose l'élément choisi à l'endroit cliqué dans la page.
    ///
    /// Les coordonnées sont celles de la vue ; le point cliqué devient le coin
    /// **supérieur gauche** de l'élément, comme dans Acrobat, sauf pour les
    /// marques qui se centrent sur le curseur — on vise une case à cocher.
    #[allow(clippy::many_single_char_names)] // coordonnées et dimensions
    fn place_sign(&mut self, x: i32, y: i32, window: &mut dyn WindowHandle) -> bool {
        let Some(item) = self.sign_panel.as_ref().and_then(|p| p.item) else {
            return false;
        };
        let Some((page, point)) = self.page_at(x, y) else {
            return false;
        };
        if let Some(cells) = self.comb_at(item, page, point) {
            // Un peigne : on demande le texte, qui se répartira dans les
            // cases, un caractère par case.
            let count = cells.len();
            self.prompt = Some(Prompt::new(
                lang::tr("Remplir les cases"),
                lang::trf(
                    "Texte à répartir dans les {} cases :",
                    &[&count.to_string()],
                ),
                TextInput::new(lang::tr("un caractère par case")),
                PromptKind::Comb { page, cells },
            ));
            return true;
        }
        if item == SignItem::Move {
            // Le mode déplacement ne pose rien : le clic sert à choisir —
            // sauf dans une case à cocher dessinée, qu'il coche. C'est le
            // geste d'Acrobat : on ouvre l'outil, on clique les cases.
            let Some(rect) = self.snap_box(item, page, point) else {
                // Pas de case : une ligne à remplir, peut-être. On y écrit
                // comme avec l'outil texte, sans avoir eu à le choisir.
                let Some(line) = self.field_line_at(item, page, point) else {
                    return false;
                };
                let color = self.sign_rgb();
                // Le texte part du début de la ligne, où qu'on ait cliqué :
                // c'est là qu'on écrit un nom, une adresse.
                let at = Point::new(line.x0 + 2.0, line.y1 + 3.0);
                self.type_on_page(page, at, color, window);
                return true;
            };
            let check = acrux_features::fillsign::marks::Mark::Check;
            self.apply_fillsign(page, rect, acrux_features::fillsign::Item::Mark(check));
            // La coche posée n'est pas prise en main : on enchaîne les cases.
            self.placed = None;
            if let Some(panel) = &mut self.sign_panel {
                panel.item = Some(SignItem::Move);
            }
            return true;
        }
        if item == SignItem::Draw {
            // Le stylo ne pose rien : il commence un trait, que le
            // relâchement du bouton écrira.
            self.inking = Some(Inking {
                page,
                points: vec![InkPoint::new(point.x, point.y)],
            });
            return true;
        }
        // Ce qu'on pose se **centre sur le pointeur**, comme l'aperçu qui
        // l'accompagne : on vise une ligne ou une case, et c'est là que ça
        // tombe. Poser au coin supérieur gauche obligeait à viser à côté.
        let (w, h) = item.default_size();
        let rect = self.snap_box(item, page, point).unwrap_or_else(|| {
            Rect::new(
                point.x - w / 2.0,
                point.y - h / 2.0,
                point.x + w / 2.0,
                point.y + h / 2.0,
            )
        });
        let source = match item {
            SignItem::Signature => {
                let current = self.sign_panel.as_ref().map_or(0, |p| p.current);
                self.signatures
                    .get(current)
                    .or_else(|| self.signatures.first())
                    .cloned()
            }
            SignItem::Initials => self.initials.clone(),
            _ => None,
        };
        let fill_item = match item {
            SignItem::Mark(mark) => acrux_features::fillsign::Item::Mark(mark),
            SignItem::Text => {
                let color = self.sign_rgb();
                self.type_on_page(page, point, color, window);
                return true;
            }
            _ => match source {
                Some(Saved::Drawn(strokes)) => {
                    // La plume suit la taille du tracé : le même geste donne
                    // le même trait, qu'il ait été fait dans une petite ou
                    // une grande fenêtre. La pointe et l'épaisseur, elles,
                    // sont celles choisies dans la barre.
                    let pen =
                        Pen::styled_for_strokes(&strokes, self.sign_nib(), self.sign_weight());
                    acrux_features::fillsign::Item::Drawn { strokes, pen }
                }
                Some(Saved::Typed(text)) => acrux_features::fillsign::Item::Typed { text },
                Some(Saved::Image(path)) => match std::fs::read(&path) {
                    Ok(data) => acrux_features::fillsign::Item::Image {
                        data,
                        cutout: Some(acrux_features::fillsign::cutout::Options::default()),
                    },
                    Err(e) => {
                        self.set_notice(format!("image de signature illisible : {e}"));
                        return true;
                    }
                },
                None => return false,
            },
        };
        self.apply_fillsign(page, rect, fill_item);
        true
    }

    /// Ce que la page offre à remplir — cases à cocher et peignes dessinés.
    /// L'inventaire d'une page se fait une fois, puis se retient.
    fn drawn_boxes(&mut self, page: usize) -> Option<&acrux_features::fillsign::boxes::Found> {
        let l = self.loaded.as_mut()?;
        if !l.boxes.contains_key(&page) {
            let found = l
                .pages
                .get(page)
                .and_then(|p| acrux_features::fillsign::boxes::scan(&l.doc, p).ok())
                .unwrap_or_default();
            l.boxes.insert(page, found);
        }
        l.boxes.get(&page)
    }

    /// La case à cocher **dessinée** sous un point de la page.
    fn drawn_box_at(&mut self, page: usize, point: Point) -> Option<Rect> {
        self.drawn_boxes(page)?.check_at(point.x, point.y)
    }

    /// La ligne à remplir — « Nom : ______ » — sous un point de la page, pour
    /// qui a la main nue ou l'outil texte.
    fn field_line_at(&mut self, item: SignItem, page: usize, point: Point) -> Option<Rect> {
        if !matches!(item, SignItem::Move | SignItem::Text) {
            return None;
        }
        self.drawn_boxes(page)?.line_at(point.x, point.y)
    }

    /// Le peigne dessiné sous un point de la page, pour qui a la main nue ou
    /// l'outil texte : c'est là qu'on écrit un IBAN, un BIC, une date.
    fn comb_at(&mut self, item: SignItem, page: usize, point: Point) -> Option<Vec<Rect>> {
        if !matches!(item, SignItem::Move | SignItem::Text) {
            return None;
        }
        self.drawn_boxes(page)?
            .comb_at(point.x, point.y)
            .map(|c| c.cells.clone())
    }

    /// Où poser une marque cliquée dans une case dessinée : **dans** la case,
    /// centrée, avec un peu d'air autour — comme Acrobat. Rien hors d'une
    /// case, ni pour ce qui ne se coche pas : la pose reste alors libre.
    fn snap_box(&mut self, item: SignItem, page: usize, point: Point) -> Option<Rect> {
        use acrux_features::fillsign::marks::Mark;
        // La coche, la croix, le point — et la main nue : sans rien choisir,
        // une case dessinée se coche.
        if !matches!(
            item,
            SignItem::Move | SignItem::Mark(Mark::Check | Mark::Cross | Mark::Dot)
        ) {
            return None;
        }
        let found = self.drawn_box_at(page, point)?;
        let air = found.width().min(found.height()) * 0.16;
        Some(Rect::new(
            found.x0 + air,
            found.y0 + air,
            found.x1 - air,
            found.y1 - air,
        ))
    }

    /// Fenêtre de capture, avec l'encre retenue.
    fn new_capture(&self, initials: bool) -> Capture {
        Capture::with_ink(
            initials,
            self.prefs.sign_color as usize,
            self.sign_nib(),
            self.sign_weight(),
        )
    }

    /// Pointe choisie.
    fn sign_nib(&self) -> Nib {
        Nib::from_index(self.prefs.sign_nib)
    }

    /// Épaisseur choisie.
    fn sign_weight(&self) -> Weight {
        Weight::from_index(self.prefs.sign_weight)
    }

    /// Barre de l'outil, avec l'encre retenue de la dernière fois.
    fn new_sign_panel(&self) -> SignPanel {
        SignPanel::with_ink(
            self.prefs.sign_color as usize,
            self.sign_nib(),
            self.sign_weight(),
        )
    }

    /// Couleur d'encre choisie.
    fn sign_rgb(&self) -> [f64; 3] {
        self.sign_panel
            .as_ref()
            .map_or_else(|| sign::INKS[0].1, SignPanel::rgb)
    }

    /// Le trait suit le pointeur.
    fn ink_move(&mut self, x: i32, y: i32) {
        let Some((page, point)) = self.page_at(x, y) else {
            return;
        };
        if let Some(ink) = &mut self.inking {
            // Un geste qui sort de la page continue sur la page commencée :
            // changer de page au milieu d'un trait n'aurait pas de sens.
            if ink.page == page {
                ink.points.push(InkPoint::new(point.x, point.y));
            }
        }
    }

    /// Fin du geste : le trait devient une annotation d'encre.
    fn ink_finish(&mut self) {
        let Some(ink) = self.inking.take() else {
            return;
        };
        if ink.points.len() < 2 {
            // Un simple clic ne laisse pas de trace.
            return;
        }
        let strokes = vec![Stroke { points: ink.points }];
        let pen = Pen::on_page(self.sign_nib(), self.sign_weight(), 1.0);
        let outline = acrux_features::fillsign::ink::outline(&strokes, &pen);
        if outline.is_empty() {
            return;
        }
        let rect = outline.bbox;
        self.apply_fillsign(
            ink.page,
            rect,
            acrux_features::fillsign::Item::Drawn { strokes, pen },
        );
    }

    /// Dessine, sous le pointeur, ce que le prochain clic posera.
    ///
    /// Sans cet aperçu, on pose une signature « à peu près » puis on annule :
    /// la taille et le centrage ne se devinent pas.
    #[allow(clippy::many_single_char_names, clippy::too_many_lines)] // géométrie de la boîte, lue de haut en bas
    fn paint_sign_ghost(&mut self, frame: &mut Frame<'_>) {
        let Some(item) = self.sign_panel.as_ref().and_then(|p| p.item) else {
            return;
        };
        if self.capture.is_some() || item == SignItem::Draw {
            return;
        }
        let Some((mx, my)) = self.last_mouse else {
            return;
        };
        let Some((page, point)) = self.page_at(mx, my) else {
            return;
        };
        // Au-dessus d'un peigne, c'est lui qui s'encadre : un clic y écrira.
        if let Some(cells) = self.comb_at(item, page, point) {
            let bounds = acrux_features::fillsign::boxes::bounds_of(&cells);
            let layout = self.layout();
            if let Some(m) = self.page_to_view(&layout, page) {
                let a = m.apply(Point::new(bounds.x0, bounds.y1));
                let b = m.apply(Point::new(bounds.x1, bounds.y0));
                let ring = (2.0 * self.dpi_scale).round();
                #[allow(clippy::cast_possible_truncation)]
                crate::ui::paint::round_rect_outline(
                    frame,
                    (a.x.min(b.x) - ring) as i32,
                    (a.y.min(b.y) - ring) as i32,
                    ((a.x - b.x).abs() + 2.0 * ring) as i32,
                    ((a.y - b.y).abs() + 2.0 * ring) as i32,
                    3.0 * self.dpi_scale as f32,
                    ring as f32,
                    self.theme.accent,
                );
            }
            return;
        }
        // Au-dessus d'une ligne à remplir, la place du texte s'encadre.
        if self.snap_box(item, page, point).is_none() {
            if let Some(line) = self.field_line_at(item, page, point) {
                let room = acrux_features::fillsign::boxes::LINE_ROOM;
                let layout = self.layout();
                if let Some(m) = self.page_to_view(&layout, page) {
                    let a = m.apply(Point::new(line.x0, line.y1 + room));
                    let b = m.apply(Point::new(line.x1, line.y0));
                    #[allow(clippy::cast_possible_truncation)]
                    crate::ui::paint::round_rect_outline(
                        frame,
                        a.x.min(b.x) as i32,
                        a.y.min(b.y) as i32,
                        (a.x - b.x).abs() as i32,
                        (a.y - b.y).abs() as i32,
                        3.0 * self.dpi_scale as f32,
                        (1.5 * self.dpi_scale).round() as f32,
                        self.theme.accent,
                    );
                }
                return;
            }
        }
        // L'outil texte n'a pas d'autre aperçu que son curseur.
        if item == SignItem::Text {
            return;
        }
        // La main nue ne montre rien — sauf au-dessus d'une case dessinée,
        // où elle propose la coche qu'un clic poserait.
        if item == SignItem::Move && self.snap_box(item, page, point).is_none() {
            return;
        }
        let scale = self.scale();
        let (w, h) = item.default_size();
        let outline = match item {
            SignItem::Mark(mark) => Some(acrux_features::fillsign::marks::outline_of(mark)),
            SignItem::Move => Some(acrux_features::fillsign::marks::outline_of(
                acrux_features::fillsign::marks::Mark::Check,
            )),
            SignItem::Signature | SignItem::Initials => {
                let saved = if item == SignItem::Initials {
                    self.initials.clone()
                } else {
                    let current = self.sign_panel.as_ref().map_or(0, |p| p.current);
                    self.signatures.get(current).cloned()
                };
                match saved {
                    Some(Saved::Drawn(strokes)) => {
                        let pen =
                            Pen::styled_for_strokes(&strokes, self.sign_nib(), self.sign_weight());
                        Some(acrux_features::fillsign::ink::outline(&strokes, &pen))
                    }
                    _ => None,
                }
            }
            SignItem::Draw | SignItem::Text => None,
        };
        // Le point cliqué est le centre : l'aperçu montre exactement la boîte
        // où l'élément ira.
        let (mut cx, mut cy) = (f64::from(mx), f64::from(my));
        let (mut dw, mut dh) = (w * scale, h * scale);
        // Au-dessus d'une case dessinée, elle s'encadre et l'aperçu s'y cale :
        // on voit où la marque ira avant de cliquer.
        if let Some((page, point)) = self.page_at(mx, my) {
            let snapped = self.snap_box(item, page, point);
            let found = snapped.and_then(|_| self.drawn_box_at(page, point));
            let layout = self.layout();
            if let (Some(inner), Some(outer), Some(m)) =
                (snapped, found, self.page_to_view(&layout, page))
            {
                // L'aperçu se peint dans la vue : mêmes coordonnées.
                let corner = |x: f64, y: f64| {
                    let p = m.apply(Point::new(x, y));
                    (p.x, p.y)
                };
                let (ax, ay) = corner(outer.x0, outer.y1);
                let (bx, by) = corner(outer.x1, outer.y0);
                let ring = (2.0 * self.dpi_scale).round();
                #[allow(clippy::cast_possible_truncation)]
                crate::ui::paint::round_rect_outline(
                    frame,
                    (ax.min(bx) - ring) as i32,
                    (ay.min(by) - ring) as i32,
                    ((ax - bx).abs() + 2.0 * ring) as i32,
                    ((ay - by).abs() + 2.0 * ring) as i32,
                    3.0 * self.dpi_scale as f32,
                    ring as f32,
                    self.theme.accent,
                );
                let (ix, iy) = corner(inner.x0, inner.y1);
                let (jx, jy) = corner(inner.x1, inner.y0);
                cx = f64::midpoint(ix, jx);
                cy = f64::midpoint(iy, jy);
                dw = (ix - jx).abs();
                dh = (iy - jy).abs();
            }
        }
        let box_rect = (cx - dw / 2.0, cy - dh / 2.0, dw, dh);
        let [r, g, b] = self.sign_rgb();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ink = ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8);
        // L'encre pâlie : c'est un aperçu, pas encore une trace.
        let faded = (ink.0 / 2 + 110, ink.1 / 2 + 110, ink.2 / 2 + 110);
        if let Some(outline) = outline {
            let (bw, bh) = (
                (outline.bbox.x1 - outline.bbox.x0).max(0.01),
                (outline.bbox.y1 - outline.bbox.y0).max(0.01),
            );
            let k = (dw / bw).min(dh / bh);
            let m = Matrix::new(
                k,
                0.0,
                0.0,
                -k,
                cx - bw * k / 2.0 - outline.bbox.x0 * k,
                cy + bh * k / 2.0 + outline.bbox.y0 * k,
            );
            sign::fill_outline(frame, &mut self.raster, &outline, &m, faded);
        } else {
            // Signature tapée ou importée : on montre au moins la boîte.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let (bx, by, bw, bh) = (
                box_rect.0 as i32,
                box_rect.1 as i32,
                box_rect.2 as i32,
                box_rect.3 as i32,
            );
            let t = (self.dpi_scale.round().max(1.0)) as i32;
            frame.fill_rect(bx, by, bw, t, faded.0, faded.1, faded.2);
            frame.fill_rect(bx, by + bh - t, bw, t, faded.0, faded.1, faded.2);
            frame.fill_rect(bx, by, t, bh, faded.0, faded.1, faded.2);
            frame.fill_rect(bx + bw - t, by, t, bh, faded.0, faded.1, faded.2);
        }
    }

    /// Dessine le trait en cours, tel qu'il sera écrit.
    fn paint_inking(&mut self, frame: &mut Frame<'_>) {
        let Some(ink) = &self.inking else { return };
        if ink.points.len() < 2 {
            return;
        }
        let layout = self.layout();
        let Some((ox, top)) = self.page_screen(&layout, ink.page) else {
            return;
        };
        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        let Some(area) = layout.get(ink.page) else {
            return;
        };
        let Some(page) = loaded.pages.get(ink.page) else {
            return;
        };
        let matrix = base_matrix(
            &page.crop_box(&loaded.doc),
            self.scale(),
            shown_rotation(loaded, page),
            area.w,
            area.h,
        )
        .then(&Matrix::new(1.0, 0.0, 0.0, 1.0, ox, top));
        // Le trait est calculé dans l'espace de la page puis transporté :
        // l'aperçu est le dessin lui-même, à l'échelle de l'affichage.
        let pen = Pen::on_page(self.sign_nib(), self.sign_weight(), 1.0);
        let strokes = [Stroke {
            points: ink.points.clone(),
        }];
        let outline = acrux_features::fillsign::ink::outline(&strokes, &pen);
        let [red, green, blue] = self.sign_rgb();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let rgb = (
            (red * 255.0) as u8,
            (green * 255.0) as u8,
            (blue * 255.0) as u8,
        );
        sign::fill_outline(frame, &mut self.raster, &outline, &matrix, rgb);
    }

    /// Sélectionne le dernier élément posé sur une page.
    ///
    /// Après une pose, Acrobat revient au **déplacement** et laisse l'élément
    /// choisi : on l'ajuste tout de suite, sans changer d'outil. C'est ce que
    /// fait cette méthode, appelée à la fin de chaque pose.
    fn select_last_placed(&mut self, page: usize) {
        let found = self
            .loaded
            .as_ref()
            .and_then(|l| acrux_features::fillsign::list(&l.doc).ok())
            .and_then(|list| {
                list.into_iter()
                    .filter(|p| p.page == page)
                    .max_by_key(|p| p.index)
            });
        if let Some(found) = found {
            self.placed = Some(PlacedSel {
                page: found.page,
                index: found.index,
                rect: found.rect,
                kind: found.kind,
                drag: None,
                hover: None,
            });
        }
        // Retour au déplacement : l'outil ne repose pas la même chose au clic
        // suivant.
        if let Some(panel) = &mut self.sign_panel {
            panel.item = Some(SignItem::Move);
        }
    }

    /// Élément posé sous un point de la page, s'il y en a un.
    fn placed_at(&self, page: usize, point: Point) -> Option<PlacedSel> {
        let list = acrux_features::fillsign::list(&self.loaded.as_ref()?.doc).ok()?;
        list.into_iter()
            .filter(|p| p.page == page && p.rect.contains(point))
            // Le plus petit d'abord : une marque posée sur une signature
            // reste atteignable.
            .min_by(|a, b| {
                (a.rect.width() * a.rect.height()).total_cmp(&(b.rect.width() * b.rect.height()))
            })
            .map(|p| PlacedSel {
                page: p.page,
                index: p.index,
                rect: p.rect,
                kind: p.kind,
                drag: None,
                hover: None,
            })
    }

    /// Rectangle de l'élément sélectionné dans la vue.
    fn placed_view_rect(&self) -> Option<objects_ui::ViewRect> {
        let placed = self.placed.as_ref()?;
        let rect = placed.drag.map_or(placed.rect, |(_, _, r)| r);
        self.page_rect_to_view(placed.page, rect)
    }

    /// Clic sur l'élément sélectionné : poignée ou déplacement. Rend vrai
    /// s'il l'a pris.
    fn placed_mouse_down(&mut self, x: i32, y: i32) -> bool {
        let Some((page, point)) = self.page_at(x, y) else {
            return false;
        };
        let handle = self
            .placed_view_rect()
            .and_then(|v| objects_ui::handle_at(v, f64::from(x), f64::from(y), self.dpi_scale));
        // Rien de sélectionné, ou clic à côté : on prend ce qui est dessous.
        let inside = self
            .placed
            .as_ref()
            .is_some_and(|p| p.page == page && (handle.is_some() || p.rect.contains(point)));
        if !inside {
            let Some(found) = self.placed_at(page, point) else {
                return false;
            };
            self.placed = Some(found);
        }
        if let Some(placed) = &mut self.placed {
            placed.drag = Some((handle, point, placed.rect));
        }
        // L'original s'efface le temps du geste : c'est l'aperçu qui suit le
        // pointeur, et l'on ne voit pas l'élément en double.
        self.live_edit = self.placed.as_ref().map(|p| p.page);
        self.show_placed(false);
        true
    }

    /// Cache ou remontre l'élément sélectionné, et repeint la page.
    fn show_placed(&mut self, visible: bool) {
        let Some(placed) = self.placed.as_ref().map(|p| (p.page, p.index)) else {
            return;
        };
        let scale = self.scale();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let key = (scale * 1000.0).round() as u32;
        let Some(l) = self.loaded.as_mut() else {
            return;
        };
        if acrux_features::fillsign::set_hidden(&l.doc, placed.0, placed.1, !visible).is_err() {
            return;
        }
        l.cache.retain(|(p, _), _| *p != placed.0);
        if let Some(page) = l.pages.get(placed.0) {
            let options = l.screen_options(std::time::Duration::from_secs(5));
            let bitmap = render_page_rotated(&l.doc, page, scale, l.view_rotation, &options).bitmap;
            l.cache.insert((placed.0, key), bitmap);
        }
    }

    /// Suit le geste sur l'élément sélectionné.
    fn placed_mouse_move(&mut self, x: i32, y: i32, dragging: bool) -> bool {
        let Some((_, point)) = self.page_at(x, y) else {
            return false;
        };
        let hover = self
            .placed_view_rect()
            .and_then(|v| objects_ui::handle_at(v, f64::from(x), f64::from(y), self.dpi_scale));
        let Some(placed) = &mut self.placed else {
            return false;
        };
        if !dragging {
            let changed = placed.hover != hover;
            placed.hover = hover;
            return changed;
        }
        let Some((handle, from, _)) = placed.drag else {
            return false;
        };
        let (dx, dy) = (point.x - from.x, point.y - from.y);
        let rect = match handle {
            Some(h) => objects_ui::resized(placed.rect, h, dx, dy, false),
            None => Rect::new(
                placed.rect.x0 + dx,
                placed.rect.y0 + dy,
                placed.rect.x1 + dx,
                placed.rect.y1 + dy,
            ),
        };
        placed.drag = Some((handle, from, rect));
        true
    }

    /// Fin du geste : le nouveau rectangle est écrit dans le document.
    fn placed_mouse_up(&mut self) -> bool {
        let Some(placed) = &mut self.placed else {
            return false;
        };
        let Some((_, _, rect)) = placed.drag.take() else {
            return false;
        };
        let moved = (rect.x0 - placed.rect.x0).abs() > 0.5
            || (rect.y0 - placed.rect.y0).abs() > 0.5
            || (rect.width() - placed.rect.width()).abs() > 0.5;
        let (page, index) = (placed.page, placed.index);
        placed.rect = rect;
        // Remontré d'abord : l'opération qui suit part d'un état propre, et
        // l'annulation retrouve un élément visible.
        self.show_placed(true);
        self.live_edit = None;
        if !moved {
            return true;
        }
        self.apply_edit(EditOp::PlacedRect { page, index, rect });
        true
    }

    /// Dessine l'élément sélectionné, ses poignées, et — pendant un geste —
    /// **le dessin lui-même à sa nouvelle place**.
    ///
    /// Voir la boîte bouger seule ne dit pas où l'on en est : c'est l'élément
    /// qu'on déplace, c'est donc lui qu'il faut voir bouger.
    fn paint_placed(&mut self, frame: &mut Frame<'_>) {
        let Some((page, rect, handle, kind, dragging)) = self.placed.as_ref().map(|p| {
            (
                p.page,
                p.drag.map_or(p.rect, |(_, _, r)| r),
                p.drag.and_then(|(h, _, _)| h).or(p.hover),
                p.kind.clone(),
                p.drag.is_some(),
            )
        }) else {
            return;
        };
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        if dragging {
            self.paint_placed_preview(frame, page, rect, &kind);
        }
        if let Some(view) = self.page_rect_to_view(page, rect) {
            objects_ui::paint_selection(frame, &theme, dpi, view, handle);
        }
    }

    /// Redessine un élément posé dans un rectangle donné.
    #[allow(clippy::many_single_char_names)] // géométrie de la mise à l'échelle
    fn paint_placed_preview(&mut self, frame: &mut Frame<'_>, page: usize, rect: Rect, kind: &str) {
        let Some(view) = self.page_rect_to_view(page, rect) else {
            return;
        };
        let [r, g, b] = self.sign_rgb();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ink = ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8);
        // La marque ou la signature se redessinent par le même code que celui
        // qui les a écrites ; une signature tapée ou importée n'a pas de
        // contour sous la main, et se montre alors par sa boîte.
        let outline = if let Some(mark) = kind.strip_prefix("mark:") {
            acrux_features::fillsign::marks::Mark::from_name(mark)
                .map(acrux_features::fillsign::marks::outline_of)
        } else if kind == "drawn" {
            let current = self.sign_panel.as_ref().map_or(0, |p| p.current);
            let saved = self
                .signatures
                .get(current)
                .cloned()
                .or_else(|| self.initials.clone());
            match saved {
                Some(Saved::Drawn(strokes)) => {
                    let pen =
                        Pen::styled_for_strokes(&strokes, self.sign_nib(), self.sign_weight());
                    Some(acrux_features::fillsign::ink::outline(&strokes, &pen))
                }
                _ => None,
            }
        } else {
            None
        };
        match outline {
            Some(outline) => {
                let (bw, bh) = (
                    (outline.bbox.x1 - outline.bbox.x0).max(0.01),
                    (outline.bbox.y1 - outline.bbox.y0).max(0.01),
                );
                let k = (view.w / bw).min(view.h / bh);
                let m = Matrix::new(
                    k,
                    0.0,
                    0.0,
                    -k,
                    view.x + (view.w - bw * k) / 2.0 - outline.bbox.x0 * k,
                    view.y + (view.h + bh * k) / 2.0 + outline.bbox.y0 * k,
                );
                sign::fill_outline(frame, &mut self.raster, &outline, &m, ink);
            }
            None => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                crate::ui::paint::round_rect_alpha(
                    frame,
                    view.x as i32,
                    view.y as i32,
                    view.w as i32,
                    view.h as i32,
                    2.0,
                    ink,
                    0.35,
                );
            }
        }
    }

    /// Envoie la pose au fil de rendu.
    fn apply_fillsign(&mut self, page: usize, rect: Rect, item: acrux_features::fillsign::Item) {
        let label = item.kind().to_string();
        let options = acrux_features::fillsign::Options {
            item,
            page,
            rect,
            author: None,
            color: self.sign_rgb(),
            ..acrux_features::fillsign::Options::default()
        };
        if self.apply_edit(EditOp::FillSign {
            options: Box::new(options),
        }) {
            self.set_notice(format!("posé : {label} en page {}", page + 1));
        }
        // Une marque reste en main : on coche rarement une seule case, et
        // Acrobat fait de même. Le reste — une signature, un paraphe — se
        // pose une fois, puis se laisse ajuster.
        let sticky = self
            .sign_panel
            .as_ref()
            .is_some_and(|p| matches!(p.item, Some(SignItem::Mark(_))));
        if sticky {
            self.placed = None;
        } else {
            self.select_last_placed(page);
        }
    }

    /// Dessine le panneau de l'outil, à gauche.
    fn paint_sign_panel(&mut self, frame: &mut Frame<'_>) {
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        let signatures = std::mem::take(&mut self.signatures);
        let initials = self.initials.take();
        if let (Some(panel), Some(text)) = (self.sign_panel.as_mut(), self.text.as_mut()) {
            panel.paint(
                frame,
                text,
                &mut self.raster,
                &theme,
                dpi,
                &signatures,
                initials.as_ref(),
                crate::ui::prefs::MAX_SIGNATURES,
            );
        }
        self.signatures = signatures;
        self.initials = initials;
    }

    /// Suite d'une action venue du panneau.
    fn sign_panel_action(&mut self, action: &SignAction, window: &mut dyn WindowHandle) {
        match action {
            SignAction::Pick(item) => self.pick_sign_item(*item),
            SignAction::Use(index, initials) => {
                // Prise en main : si l'on relâche sur la page, on pose là.
                self.carrying = true;
                let item = if *initials {
                    SignItem::Initials
                } else {
                    SignItem::Signature
                };
                let _ = index;
                self.pick_sign_item(item);
            }
            SignAction::Create(initials) => {
                self.replacing = None;
                self.capture = Some(self.new_capture(*initials));
            }
            SignAction::Edit(index, initials) => {
                self.replacing = (!*initials).then_some(*index);
                self.capture = Some(self.new_capture(*initials));
            }
            SignAction::Delete(index, initials) => {
                if *initials {
                    self.initials = None;
                } else if *index < self.signatures.len() {
                    self.signatures.remove(*index);
                }
                if let Some(panel) = &mut self.sign_panel {
                    panel.current = panel.current.min(self.signatures.len().saturating_sub(1));
                    let empty = if *initials {
                        panel.item == Some(SignItem::Initials)
                    } else {
                        self.signatures.is_empty() && panel.item == Some(SignItem::Signature)
                    };
                    if empty {
                        panel.item = None;
                    }
                }
                self.store_signatures();
            }
            SignAction::Ink(index) => {
                self.prefs.sign_color = u8::try_from(*index).unwrap_or(0);
                self.prefs.save();
            }
            SignAction::Style(nib, weight) => {
                self.prefs.sign_nib = nib.index();
                self.prefs.sign_weight = weight.index();
                self.prefs.save();
            }
            SignAction::Close => {
                self.sign_panel = None;
                self.placed = None;
                self.wake_anim();
                self.capture = None;
                if self.edit_overlay() {
                    self.leave_edit();
                }
                self.clamp_scroll();
                self.set_notice("remplir et signer : terminé".into());
            }
        }
        window.request_redraw();
    }

    /// Dessine la fenêtre de capture, par-dessus tout le reste.
    fn paint_capture(&mut self, frame: &mut Frame<'_>) {
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        let Some(text) = &mut self.text else { return };
        if let Some(capture) = &mut self.capture {
            capture.paint(frame, text, &mut self.raster, &theme, dpi);
        }
    }

    /// Recopie l'état courant dans les préférences et les enregistre.
    fn save_prefs(&mut self) {
        self.prefs.dark_theme = self.theme.is_dark();
        self.prefs.view_mode = self.view_mode;
        self.prefs.panel_open = self.panel_open;
        self.prefs.fit = self.fit;
        self.prefs.tools_open = self.tools_open;
        self.prefs.zoom = self.zoom;
        self.prefs.two_up_cover = self.two_up_cover;
        self.prefs.signatures = self.signatures.iter().map(Saved::encode).collect();
        self.prefs.initials = self.initials.as_ref().map(Saved::encode);
        // La taille retenue est celle d'une fenêtre **ordinaire** : celle
        // d'une fenêtre agrandie vaut le bureau entier, et la rouvrir à cette
        // taille sans l'agrandir la ferait déborder de l'écran — on y perdrait
        // le panneau de droite et la barre d'état.
        if self.width > 0 && self.height > 0 && self.dpi_scale > 0.0 && !self.window_max {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let logical = |v: u32| (f64::from(v) / self.dpi_scale).round() as u32;
            self.prefs.window = (logical(self.width), logical(self.height));
        }
        self.prefs.window_max = self.window_max;
        self.prefs.save();
    }

    /// Change la disposition en gardant la page courante à l'écran.
    fn set_view_mode(&mut self, mode: ViewMode) {
        let page = self.current_page();
        self.view_mode = mode;
        self.prefs.view_mode = mode;
        self.anchor = page;
        self.scroll_x = 0.0;
        self.scroll_to_page(page);
        self.title_dirty = true;
        self.save_prefs();
    }

    /// Zoom réel affiché (1.0 = 100 %).
    fn effective_zoom(&self) -> f64 {
        self.scale() / (self.dpi_scale * (96.0 / 72.0))
    }

    fn update_title(&mut self, window: &mut dyn WindowHandle) {
        let title = match &self.loaded {
            Some(l) => {
                let name = l
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let star = if l.modified { " *" } else { "" };
                format!("{name}{star} — Acrux")
            }
            None => "Acrux".to_string(),
        };
        window.set_title(&title);
        self.title_dirty = false;
    }

    /// Récupère les rendus terminés du fil de travail.
    fn collect_results(&mut self) -> bool {
        let Some(l) = &mut self.loaded else {
            return false;
        };
        let Some(worker) = &mut l.worker else {
            return false;
        };
        let mut any = false;
        let live = self.live_edit;
        for r in worker.poll() {
            // La durée de rendu est une mesure de développement : elle va au
            // journal, plus dans la barre d'état.
            log_line(&format!("rendu : page {} en {:.0} ms", r.page + 1, r.ms));
            // Image d'une page qu'on est en train de bouger : elle vient
            // d'un état dépassé, la garder ferait clignoter la page.
            if live == Some(r.page) {
                continue;
            }
            l.cache.insert((r.page, r.scale_key), r.bitmap);
            any = true;
        }
        let mut notices = Vec::new();
        while let Some(n) = worker.take_notice() {
            notices.push(n);
        }
        for n in notices {
            self.set_notice(n);
            any = true;
        }
        any
    }

    #[allow(clippy::too_many_lines, clippy::many_single_char_names)] // une passe : ombre, page ou attente, cache, libellés
    fn paint_document(&mut self, frame: &mut Frame<'_>) {
        let t = self.theme;
        frame.clear(t.canvas.0, t.canvas.1, t.canvas.2);
        let layout = self.layout();
        let scale = self.scale();
        let key_scale = (scale * 1000.0).round() as u32;
        let view_h = self.view_height();
        let (scroll_x, scroll_y) = (self.scroll_x, self.scroll_y);
        let dpi = self.dpi_scale as f32;
        let Some(l) = &mut self.loaded else { return };
        let thumb_key = self.thumb_key;
        if let Some(w) = &mut l.worker {
            w.forget_other_scales(&[key_scale, thumb_key]);
        }
        let view_top = scroll_y;
        let view_bottom = scroll_y + f64::from(view_h);
        let options = RenderOptions {
            annotations: true,
            time_budget: Some(std::time::Duration::from_secs(20)),
            background: Some(Color::WHITE),
            ..RenderOptions::default()
        };
        let mut placeholders: Vec<(i32, i32, u32, u32)> = Vec::new();
        for (i, b) in layout.iter().enumerate() {
            if !b.visible {
                continue;
            }
            let (w, h) = (&b.w, &b.h);
            let top = f64::from(b.y);
            let bottom = top + f64::from(*h);
            if bottom < view_top || top > view_bottom {
                continue;
            }
            let (dx, dy) = (
                (f64::from(b.x) - scroll_x).round() as i32,
                (top - scroll_y).round() as i32,
            );
            let clip_h = view_h as i32;
            // Ombre portée.
            frame.fill_rect(
                dx + 3,
                dy + 3,
                *w as i32,
                (*h as i32).min(clip_h - dy - 3).max(0),
                t.page_shadow.0,
                t.page_shadow.1,
                t.page_shadow.2,
            );
            let key = (i, key_scale);
            if !l.cache.contains_key(&key) {
                if let Some(worker) = &mut l.worker {
                    worker.request(i, scale, key_scale);
                    // En attendant le rendu, l'image de la page à une autre
                    // échelle, étirée : zoomer ne fait plus clignoter la page
                    // en blanc à chaque cran, comme dans Acrobat. La plus
                    // grande échelle disponible, la plus nette ; la vignette
                    // du panneau fait l'affaire à défaut.
                    let stand_in = l
                        .cache
                        .iter()
                        .filter(|((page, s), _)| *page == i && *s != key_scale)
                        .max_by_key(|((_, s), _)| *s);
                    if let Some(((_, from), bitmap)) = stand_in {
                        log_line(&format!(
                            "aperçu étiré : page {} de {from} à {key_scale}",
                            i + 1
                        ));
                        frame.blit_rgba_scaled(
                            (dx, dy, *w, *h),
                            bitmap.width(),
                            bitmap.height(),
                            bitmap.data(),
                        );
                        continue;
                    }
                    // Rien à étirer : page blanche en attendant le rendu.
                    frame.fill_rect(
                        dx,
                        dy,
                        *w as i32,
                        (*h as i32).min(clip_h - dy).max(0),
                        255,
                        255,
                        255,
                    );
                    placeholders.push((dx, dy, *w, *h));
                    continue;
                }
                // Sans fil de rendu : rendu direct.
                let start = std::time::Instant::now();
                let b = render_page_rotated(&l.doc, &l.pages[i], scale, l.view_rotation, &options)
                    .bitmap;
                log_line(&format!(
                    "rendu direct : page {} en {:.0} ms",
                    i + 1,
                    start.elapsed().as_secs_f64() * 1000.0
                ));
                l.cache.insert(key, b);
            }
            if let Some(bitmap) = l.cache.get(&key) {
                let visible_h = (bitmap.height() as i32).min(clip_h - dy).max(0) as u32;
                frame.blit_rgba_premultiplied(dx, dy, bitmap.width(), visible_h, bitmap.data());
            }
        }
        // Le cache ne garde que les pages proches de la vue. Il est aussi
        // borné en octets : à fort zoom, une douzaine d'images d'échelles
        // passées pèserait des centaines de mégaoctets.
        let bytes: usize = l.cache.values().map(|b| b.data().len()).sum();
        if l.cache.len() > 12 || bytes > CACHE_BYTES {
            let near = |i: usize| {
                layout.get(i).is_some_and(|b| {
                    b.visible
                        && f64::from(b.y + b.h as i32) >= view_top - 2000.0
                        && f64::from(b.y) <= view_bottom + 2000.0
                })
            };
            // Une page proche qui attend son rendu garde sa meilleure image
            // d'une autre échelle : c'est son aperçu, sans lui elle
            // repasserait au blanc dès le deuxième cran de zoom.
            let mut stand_ins: HashMap<usize, u32> = HashMap::new();
            for &(i, s) in l.cache.keys() {
                if s != key_scale && near(i) && !l.cache.contains_key(&(i, key_scale)) {
                    let best = stand_ins.entry(i).or_insert(s);
                    *best = (*best).max(s);
                }
            }
            l.cache.retain(|&(i, s), _| {
                s == thumb_key || (s == key_scale && near(i)) || stand_ins.get(&i) == Some(&s)
            });
        }
        // Indication « rendu en cours » sur les pages en attente.
        if let Some(text) = &mut self.text {
            let size = t.font_size * dpi;
            for (dx, dy, w, h) in placeholders {
                let label = lang::tr("Rendu en cours…");
                let tw = text.measure(size, label);
                let cx = dx as f32 + (w as f32 - tw) / 2.0;
                let cy = dy as f32 + (h as f32).min(view_h as f32 - dy as f32) / 2.0;
                text.draw(frame, cx, cy, size, label, t.text_dim);
            }
        }
    }
}

impl App for Viewer {
    fn event(&mut self, event: Event, window: &mut dyn WindowHandle) {
        self.handle_event(event, window);
        self.sync_anim();
    }

    fn paint(&mut self, frame: &mut Frame<'_>) {
        self.paint_all(frame);
    }
}

impl Viewer {
    /// Traite un événement. Toutes ses sorties mènent à `sync_anim` (voir
    /// [`App::event`]), y compris ses nombreux retours anticipés.
    #[allow(clippy::too_many_lines)] // un bras par type d'événement
    fn handle_event(&mut self, event: Event, window: &mut dyn WindowHandle) {
        if !self.pending_open.is_empty() {
            // Chaque fichier de la ligne de commande ouvre un onglet ; le
            // premier reste actif, comme dans les navigateurs.
            for path in std::mem::take(&mut self.pending_open) {
                self.open(&path, window);
            }
            self.select_tab(0);
        }
        // Une fois par lancement, et au plus une fois par jour : la
        // recherche est silencieuse et n'installe rien toute seule.
        if !self.update_started {
            self.update_started = true;
            if self.prefs.check_updates && self.prefs.last_update_check != today() {
                self.check_updates(false, window);
            }
        }
        self.tick_anim(window);
        if !self.frame_themed {
            self.frame_themed = true;
            self.apply_frame_theme(window);
        }
        log_event(&event);
        // Windows envoie la barre d'espace deux fois : en touche, qui presse
        // le bouton d'une carte, puis en caractère. Ce caractère n'a plus de
        // destinataire — la carte est souvent fermée — et irait au document,
        // où l'espace active le champ de formulaire qui a le focus : valider
        // à l'espace l'invite d'un champ la rouvrait aussitôt. Il est donc
        // écarté, sauf quand la touche arrivait dans un champ de saisie, où
        // c'est une espace à écrire.
        if let Event::Char(c, _) = event {
            if std::mem::take(&mut self.swallow_space) && c == ' ' {
                return;
            }
        }
        if matches!(event, Event::Key(..)) {
            self.swallow_space = false;
        }
        let pressing = matches!(event, Event::Key(Key::Space, _)) && !self.modal_typing();
        // Le menu du clic droit ne survit pas à une carte modale ouverte
        // par-dessus (une question arrivée d'un autre fil, par exemple).
        if self.context_menu.is_some() && self.menu_blocked() {
            self.context_menu = None;
        }
        self.drop_blocked_zoom_menu();
        if self.field_menu.is_some() {
            self.drop_blocked_field_menu();
        }
        if self.dialog_event(&event, window)
            || self.protect_event(&event, window)
            || self.modal_event(&event, window)
            || self.zoom_menu_event(&event, window)
            || self.context_menu_event(&event, window)
            || self.field_menu_event(&event, window)
        {
            self.swallow_space = pressing;
            if self.title_dirty {
                self.update_title(window);
            }
            return;
        }
        match event {
            Event::Resize { width, height } => {
                self.width = width;
                self.height = height;
                self.window_max = window.maximised();
                self.clamp_scroll();
                self.title_dirty = true;
            }
            Event::DpiChanged(s) => {
                self.dpi_scale = f64::from(s);
                if let Some(l) = &mut self.loaded {
                    l.cache.clear();
                }
            }
            Event::Wake => {
                // Un réveil sans nouveau rendu, sans recherche en cours, sans
                // info-bulle à faire apparaître et sans média en train de
                // jouer ne mérite pas de repeindre.
                let results = self.collect_results();
                // Le fil des mises à jour réveille une fois, son résultat
                // prêt : la peinture le relève (`poll_updates`) et l'affiche.
                if !results
                    && self.update_rx.is_none()
                    && !self.search_scanning()
                    && !self.tip_due()
                    && !self.media_playing()
                    && !self.edit_on()
                    && self.field_edit.is_none()
                    && !self.welcome_pending()
                    && !self.animating()
                {
                    return;
                }
            }
            Event::FileDropped(path) => self.open(&path, window),
            Event::Wheel {
                delta,
                modifiers,
                x,
                y,
            } => {
                if self.overlay_wheel(delta) {
                    // Une palette ou un sélecteur ouvert a pris la molette :
                    // rien ne défile derrière lui.
                    window.request_redraw();
                } else if self.three_d_wheel(x, y, f64::from(delta), window) {
                    // Le modèle 3D a pris la molette : on s'approche de lui,
                    // la page ne défile pas.
                } else if self.showing_home() {
                    let max = (self.welcome_height - f64::from(self.view_height())).max(0.0);
                    self.welcome_scroll =
                        (self.welcome_scroll - f64::from(delta) * 90.0).clamp(0.0, max);
                    window.request_redraw();
                } else if self.sign_panel_open()
                    && x < self.view_left() as i32
                    && y >= self.view_top() as i32
                {
                    let height = f64::from(self.view_height());
                    if let Some(panel) = &mut self.sign_panel {
                        panel.wheel(f64::from(delta) * 60.0, height);
                    }
                } else if self.panel_open
                    && x < self.view_left() as i32
                    && y >= self.view_top() as i32
                {
                    self.panel.wheel(delta);
                } else if self.tools_width() > 0
                    && x >= self.width.saturating_sub(self.tools_width()) as i32
                    && y >= self.view_top() as i32
                {
                    let (h, dpi) = (f64::from(self.view_height()), self.dpi_scale);
                    self.tools.scroll(f64::from(-delta) * 60.0, h, dpi);
                } else if modifiers.ctrl {
                    self.wheel_zoom(delta, x, y);
                } else if modifiers.shift {
                    self.scroll_x -= f64::from(delta) * 80.0;
                } else {
                    self.glide_by(f64::from(-delta) * 90.0);
                }
                self.clamp_scroll();
            }
            Event::Key(key, m) if self.palette.is_some() => {
                let result = self.palette.as_mut().map(|p| p.key(key, m));
                if let Some((command, close)) = result {
                    if close {
                        self.palette = None;
                    }
                    if let Some(c) = command {
                        self.palette_pick(c, window);
                    }
                }
            }
            // Un champ de formulaire en saisie prend le clavier avant tout ce
            // qui suit — les raccourcis sans modificateur (r, h, s, t…) y
            // sont des lettres, Ctrl+A y sélectionne le champ. Les touches de
            // disposition (F4, F5, F6, F11) restent à la fenêtre.
            Event::Key(key, m)
                if self.field_typing()
                    && !is_global_key(key, m)
                    && self.field_key(key, m, window) => {}
            Event::Char(c, m) if self.field_typing() && self.field_char(c, m, window) => {}
            // Ctrl+V dans un champ de saisie — la recherche, la police ou le
            // code d'une couleur — y colle le presse-papiers ; celui d'une
            // invite (le texte d'un peigne, une note, un mot de passe) passe
            // par `modal_event`. Un IBAN se copie d'ailleurs, il ne se retape
            // pas.
            Event::Char(c, m)
                if m.ctrl && matches!(c, 'v' | 'V' | '\u{16}') && self.paste_into_field(window) => {
            }
            // Les autres raccourcis Ctrl s'arrêtent à la palette, comme à une
            // invite : Ctrl+W fermait l'onglet dessous et la palette proposait
            // encore « Enregistrer » ou « Imprimer » sans document, Ctrl+Z
            // défaisait une modification sous le voile, Ctrl+A sélectionnait
            // tout le document au lieu du champ. Ctrl+Maj+P la referme.
            Event::Char(c, m) if m.ctrl && self.palette.is_some() => {
                if m.shift && matches!(c, 'p' | 'P') {
                    self.palette = None;
                }
            }
            // La navigation vaut quelle que soit la zone qui tient le clavier
            // — la carte de recherche, la barre d'outils, le panneau, un bloc
            // de texte en cours de saisie — : ces raccourcis y étaient
            // avalés, Ctrl+Tab ne changeait pas d'onglet recherche ouverte.
            // Seules les fenêtres qui prennent tout les arrêtent.
            Event::Key(k @ (Key::Left | Key::Right), m)
                if m.alt && !m.ctrl && self.navigation_keys_free() =>
            {
                self.history_step(k == Key::Right);
            }
            Event::Key(k @ (Key::Plus | Key::Minus), m)
                if m.ctrl && m.shift && self.navigation_keys_free() =>
            {
                self.rotate_view(if k == Key::Plus { 90 } else { -90 });
            }
            Event::Key(Key::Tab, m) if m.ctrl && self.navigation_keys_free() => {
                self.cycle_tab(!m.shift);
            }
            // Ctrl+F4 ferme l'onglet, comme Ctrl+W.
            Event::Key(Key::F(4), m) if m.ctrl && self.navigation_keys_free() => {
                let active = self.active_tab;
                self.close_tab(active, window);
            }
            Event::Nav { forward } => {
                if self.navigation_keys_free() {
                    self.history_step(forward);
                }
            }
            // Un sélecteur de la barre « Modifier » déroulé prend le clavier :
            // flèches et Entrée dans la liste des polices, frappe dans sa
            // recherche ou dans le code d'une couleur, Échap pour refermer.
            Event::Key(key, _)
                if self.prompt.is_none()
                    && self.palette.is_none()
                    && self.edit_popup_key(key, window) => {}
            Event::Char(c, m)
                if self.prompt.is_none()
                    && self.palette.is_none()
                    && !m.ctrl
                    && self.edit_popup_char(c, window) => {}
            Event::Key(Key::Escape, _)
                if self.prompt.is_none() && self.palette.is_none() && self.close_3d() =>
            {
                window.request_redraw();
            }
            // Échap éteint l'outil de commentaire d'abord, puis ferme la
            // barre des commentaires : on revient à la sélection avant de
            // quitter la relecture (voir `escape_comment_tools`).
            Event::Key(Key::Escape, _)
                if self.prompt.is_none()
                    && self.palette.is_none()
                    && self.escape_comment_tools() =>
            {
                window.request_redraw();
            }
            Event::Key(key, m)
                if self.edit_on() && self.prompt.is_none() && self.edit_key(key, m, window) => {}
            Event::Char(c, m)
                if self.editing_text() && self.prompt.is_none() && self.edit_char(c, m, window) => {
            }
            Event::Key(key, _)
                if self.objects.is_some() && self.prompt.is_none() && self.objects_key(key) =>
            {
                window.request_redraw();
            }
            Event::Char(c, m) if self.objects.is_some() && !m.ctrl && self.objects_char(c) => {
                window.request_redraw();
            }
            Event::Key(key, _) if self.capture.is_some() => {
                let action = self
                    .capture
                    .as_mut()
                    .map_or(sign::Action::None, |c| c.key(key));
                // Échap ferme la fenêtre quel que soit l'onglet.
                if key == Key::Escape {
                    self.sign_action(sign::Action::Close, window);
                } else {
                    self.sign_action(action, window);
                }
            }
            Event::Char(c, m)
                if self.capture.is_some() && m.ctrl && matches!(c, 'z' | 'Z' | '\u{1a}') =>
            {
                let action = self
                    .capture
                    .as_mut()
                    .map_or(sign::Action::None, Capture::undo);
                self.sign_action(action, window);
            }
            Event::Char(c, m) if self.capture.is_some() && !m.ctrl => {
                let action = self
                    .capture
                    .as_mut()
                    .map_or(sign::Action::None, |cap| cap.char(c));
                self.sign_action(action, window);
            }
            Event::Char(c, m) if self.palette.is_some() && !m.ctrl => {
                if let Some(p) = &mut self.palette {
                    p.char(c);
                }
            }
            // Les touches de disposition valent partout : sinon la zone qui
            // tient le focus les avalerait et on ne pourrait plus en sortir.
            Event::Key(key, m) if is_global_key(key, m) => self.key(key, m, window),
            Event::Key(key, m) if self.region == Region::Toolbar && !self.toolbar.has_focus() => {
                self.toolbar_key(key, m, window);
            }
            Event::Key(key, m) if self.region == Region::Panel => {
                self.panel_key(key, m, window);
            }
            // Une zone autre que le document mange les caractères : sinon la
            // barre d'espace ferait défiler la page sous le bouton visé.
            Event::Char(_, m) if self.region != Region::Document && !m.ctrl => {}
            Event::Key(key, _) if self.toolbar.has_focus() => {
                let info = self.toolbar_info();
                if let Some(a) = self.toolbar.key(key, &info) {
                    self.tool_action(a, window);
                }
            }
            Event::Char(c, m) if self.toolbar.has_focus() && !m.ctrl => {
                let info = self.toolbar_info();
                self.toolbar.char(c, &info);
            }
            // La tabulation parcourt la carte comme n'importe quel formulaire :
            // « rechercher », « remplacer », puis les boutons.
            Event::Key(Key::Tab, m)
                if self.search.as_ref().is_some_and(|s| s.replace.is_some()) =>
            {
                if let Some(s) = &mut self.search {
                    s.tab(m.shift);
                }
                window.request_redraw();
            }
            // Sur un bouton de la carte, Entrée et Espace le pressent.
            Event::Key(key @ (Key::Enter | Key::Space), _)
                if self
                    .search
                    .as_ref()
                    .is_some_and(|s| matches!(s.focus, SearchFocus::Button(_))) =>
            {
                // Le caractère de l'espace ne doit pas finir dans le champ
                // qui reprendrait le focus une fois tout remplacé.
                self.swallow_space = key == Key::Space;
                if self.search.as_ref().map(|s| s.focus) == Some(SearchFocus::Button(0)) {
                    log_line("recherche : remplacer (clavier)");
                    self.replace_current();
                } else {
                    log_line("recherche : tout remplacer (clavier)");
                    self.replace_all();
                }
                window.request_redraw();
            }
            Event::Key(key, m) if self.search.is_some() => self.search_key(key, m, window),
            Event::Char(c, m) if self.search.is_some() && !m.ctrl => {
                self.search_char(c);
                window.request_redraw();
            }
            Event::Key(key, m) => self.key(key, m, window),
            Event::Char(c, m) => {
                if m.ctrl {
                    match c {
                        'o' | 'O' | '\u{f}' => {
                            if let Some(p) = window.open_file_dialog() {
                                self.open(&p, window);
                            }
                        }
                        'h' | 'H' | '\u{8}' => self.open_replace(),
                        's' | 'S' => {
                            self.save(m.shift, window);
                        }
                        'w' | 'W' => {
                            let active = self.active_tab;
                            self.close_tab(active, window);
                        }
                        'z' | 'Z' => {
                            // Ctrl+Z annule, Ctrl+Maj+Z rétablit : la lettre
                            // arrive en minuscule, seul `shift` les distingue.
                            if m.shift {
                                self.redo_edit(window);
                            } else {
                                self.undo(window);
                            }
                        }
                        'y' | 'Y' => self.redo_edit(window),
                        'p' | 'P' => {
                            if m.shift {
                                self.open_palette();
                            } else {
                                self.print(window);
                            }
                        }
                        'e' | 'E' if m.shift => self.enter_edit(EditTool::Select, window),
                        'e' | 'E' => self.export(window),
                        'i' | 'I' => self.insert_pages(window),
                        'c' | 'C' => self.copy_selection(window),
                        // Carte de recherche ouverte, Ctrl+A choisit tout
                        // son champ, pas tout le document.
                        'a' | 'A' => {
                            if !self.search_select_all() {
                                self.select_all();
                            }
                        }
                        'd' | 'D' | '\u{4}' => self.show_properties(),
                        'f' | 'F' | '\u{6}' => self.open_search_ui(),
                        // Ctrl+G : le champ de page prend le focus, prêt à
                        // recevoir un numéro ou une étiquette.
                        'g' | 'G' | '\u{7}' => {
                            let info = self.toolbar_info();
                            self.toolbar.focus_page(&info);
                        }
                        // Avec Maj, c'est la rotation de la vue : la touche
                        // l'a déjà faite (Key::Plus, Key::Minus), son
                        // caractère ne doit pas zoomer en plus.
                        '+' | '=' if !m.shift => self.zoom_step(1),
                        '-' if !m.shift => self.zoom_step(-1),
                        '0' => self.set_fit(Fit::Automatic),
                        _ => {}
                    }
                } else {
                    match c {
                        '+' | '=' => self.zoom_step(1),
                        '-' => self.zoom_step(-1),
                        'f' | 'F' => self.set_fit(Fit::Width),
                        '1' => self.set_zoom(1.0),
                        't' | 'T' => self.toggle_theme(window),
                        'r' => self.rotate_current(90),
                        'R' => self.rotate_current(-90),
                        ' ' => self.activate_focused_field(window),
                        'e' | 'E' => self.start_text_edit(window),
                        'h' => self.markup_selection(MarkupKind::Highlight),
                        'H' => self.markup_with_comment(MarkupKind::Highlight, window),
                        'u' => self.markup_selection(MarkupKind::Underline),
                        'U' => self.markup_with_comment(MarkupKind::Underline, window),
                        'm' => self.mark_redaction(),
                        'M' => self.apply_redactions(),
                        'n' | 'N' => self.start_note(),
                        's' | 'S' => self.toggle_fillsign(window),
                        'o' | 'O' => self.toggle_objects(window),
                        _ => {}
                    }
                }
            }
            Event::MouseDown {
                button: MouseButton::Left,
                x,
                y,
                modifiers,
                clicks,
            } => {
                // Un clic fait taire l'info-bulle : on agit, on ne lit plus.
                self.tip = None;
                if self.capture.is_some() {
                    let action = self
                        .capture
                        .as_mut()
                        .map_or((sign::Action::None, false), |c| c.mouse_down(x, y));
                    // Un clic hors de la fenêtre ne la ferme pas : on est en
                    // train de dessiner, un débordement de geste serait fatal.
                    self.sign_action(action.0, window);
                    return;
                }
                if let Some(p) = &mut self.palette {
                    // Une ligne s'enfonce et partira au relâchement ; un clic
                    // dehors ferme, sans rien exécuter.
                    if p.mouse_down(x, y) == PaletteDown::Outside {
                        self.palette = None;
                    }
                    window.request_redraw();
                    return;
                }
                // Un clic hors du champ en cours de saisie le valide, où
                // qu'il tombe : barre, panneau, autre champ, page.
                if self.field_edit.is_some() && !self.in_field_edit(x, y) {
                    self.commit_field();
                }
                // La liste des polices déroulée prend tous les clics : une
                // ligne choisit, ailleurs referme.
                if self.edit_menu_open() {
                    self.edit_bar_click(x, y, window);
                    window.request_redraw();
                    return;
                }
                // La carte de recherche flotte au-dessus de la page : ses
                // champs et ses boutons se cliquent avant elle, et un clic
                // dans ses marges s'y arrête.
                if self.search_mouse_down(x, y) {
                    window.request_redraw();
                    return;
                }
                let top = self.view_top() as i32;
                let bar = Toolbar::height(&self.theme, self.dpi_scale as f32);
                if y < bar {
                    if self.prompt.is_none() {
                        let info = self.toolbar_info();
                        if let Some(a) = self.toolbar.mouse_down(x, y, &info) {
                            self.tool_action(a, window);
                        }
                    }
                } else if y < bar + self.tabs_height() as i32 {
                    // Bande des onglets.
                    if self.prompt.is_none() {
                        match self.tabs.mouse_down(x, y - bar) {
                            TabAction::Select(i) => self.select_tab(i),
                            TabAction::Close(i) => self.close_tab(i, window),
                            TabAction::None => {}
                        }
                    }
                } else if self.in_status_bar(y) {
                    // La barre d'état passe avant la colonne d'outils et le
                    // panneau, dont les tests ne regardent que l'abscisse :
                    // sans quoi le zoom, sous la colonne, cliquait un outil.
                    if self.prompt.is_none() {
                        self.status_click(x, y, window);
                    }
                } else if self.in_form_bar(y) {
                    if self.prompt.is_none() {
                        self.form_bar_click(x, y, window);
                    }
                } else if y < top
                    && (self.annot_tool.is_some() || self.comment_bar)
                    && !self.edit_on()
                {
                    if self.prompt.is_none() {
                        if self.mode_bar.closes(x, y) {
                            self.close_annot_tools();
                        } else if let Some(&tool) = self
                            .mode_bar
                            .tool_at(x, y)
                            .and_then(|i| COMMENT_TOOLS.get(i))
                        {
                            self.toggle_annot_tool(tool, window);
                        }
                    }
                } else if y < top && self.edit_on() && !self.edit_overlay() {
                    // Barre du mode « Modifier le PDF ».
                    if self.prompt.is_none() {
                        self.edit_bar_click(x, y, window);
                    }
                } else if self.tools_width() > 0
                    && x >= self.width.saturating_sub(self.tools_width()) as i32
                {
                    self.toolbar.blur();
                    if self.prompt.is_none() {
                        let info = self.tools_info();
                        let dpi = self.dpi_scale;
                        if let Some(command) = self.tools.click(f64::from(y - top), dpi, info) {
                            self.run_command(command, window);
                        }
                    }
                } else if self.sign_panel_open() && x < self.view_left() as i32 {
                    self.toolbar.blur();
                    if self.prompt.is_none() {
                        let action = self
                            .sign_panel
                            .as_mut()
                            .and_then(|p| p.mouse_down(x, y - top));
                        if let Some(action) = action {
                            log_line(&format!("remplir et signer : {action:?}"));
                            self.sign_panel_action(&action, window);
                        }
                    }
                } else if self.panel_open && x < self.view_left() as i32 {
                    self.toolbar.blur();
                    if self.prompt.is_none() {
                        let (_, key, sizes) = self.thumb_geometry();
                        let action = match &self.loaded {
                            Some(l) => {
                                let content = PanelContent {
                                    page_count: l.pages.len(),
                                    current: 0,
                                    thumb_sizes: &sizes,
                                    bitmaps: &l.cache,
                                    thumb_key: key,
                                    outline: &l.outline_rows,
                                    comments: &l.comments,
                                    layers: &l.layers,
                                    attachments: &l.attachments,
                                };
                                self.panel.mouse_down(x, y - top, &content)
                            }
                            None => PanelAction::None,
                        };
                        log_line(&format!(
                            "panneau : clic ({x}, {}) → {action:?}, onglet {:?}",
                            y - top,
                            self.panel.tab
                        ));
                        self.panel_action(action, window);
                    }
                } else {
                    self.toolbar.blur();
                    let (x, y) = (x - self.view_left() as i32, y - top);
                    // Un nouveau clic met fin à une prise en main restée
                    // en l'air (relâchée hors de la page, par exemple).
                    self.carrying = false;
                    if self.prompt.is_none() && self.showing_home() {
                        self.click_recent(x, y, window);
                    } else if self.prompt.is_none() && x >= 0 && y < self.view_height() as i32 {
                        if self.edit_on() {
                            self.edit_mouse_down(x, y, clicks, modifiers.shift, window);
                        } else if self.annot_tool == Some(AnnotTool::Note) {
                            self.last_mouse = Some((x, y));
                            self.start_note();
                        } else if self.annot_tool == Some(AnnotTool::Insert) {
                            self.start_insert(x, y);
                        } else if self.three_d_mouse_down(x, y, clicks, modifiers.shift, window) {
                            // Un modèle 3D a pris le clic.
                        } else if self.media_mouse_down(x, y, window) {
                            // Le média a pris le clic.
                        } else if let Some((_, media)) = self.media_at(x, y) {
                            self.open_media(&media, window);
                        } else if self.objects.is_some() {
                            self.objects_mouse_down(x, y, modifiers.shift);
                            window.request_redraw();
                        } else if self
                            .sign_panel
                            .as_ref()
                            .is_some_and(|p| p.item.is_none_or(|i| i == SignItem::Move))
                            && self.placed_mouse_down(x, y)
                        {
                            // Mode déplacement : on saisit ce qui est déjà posé.
                            window.request_redraw();
                        } else if self.place_sign(x, y, window) {
                            // L'outil a posé quelque chose : ni sélection, ni lien.
                        } else if let Some((fi, wi, pt)) = self.widget_at(x, y) {
                            self.click_widget(fi, wi, (x, y), pt, clicks, modifiers, window);
                        } else if let Some(link) = self.link_at(x, y) {
                            self.selection = None;
                            self.focus_field = None;
                            self.follow(&link.action, window);
                        } else {
                            // Un clic hors des champs leur retire le focus :
                            // sans cela, ↓ pour faire défiler la page changeait
                            // encore le bouton radio ou la ligne de liste
                            // cliqués plus tôt, et modifiait le document.
                            self.focus_field = None;
                            self.mouse_down(x, y, clicks, modifiers.shift);
                        }
                    }
                }
            }
            Event::MouseDown {
                button: MouseButton::Middle,
                x,
                y,
                ..
            } => self.middle_down(x, y, window),
            Event::MouseUp { x, y, .. } if self.palette.is_some() => {
                let chosen = self.palette.as_mut().and_then(|p| p.mouse_up(x, y));
                if let Some(command) = chosen {
                    self.palette = None;
                    self.palette_pick(command, window);
                }
            }
            // La palette recouvre tout : ni l'outil en cours, ni le modèle 3D,
            // ni les objets de la page ne voient passer le pointeur — sans
            // quoi tenir l'ascenseur de la palette ferait tourner le modèle.
            Event::MouseMove { x, y, dragging } if self.palette.is_some() => {
                if self
                    .palette
                    .as_mut()
                    .is_some_and(|p| p.mouse_move(x, y, dragging))
                {
                    window.request_redraw();
                }
            }
            // Un bouton de la carte de recherche enfoncé agit au relâchement,
            // pointeur dessus — avant que l'outil en cours ne le prenne.
            Event::MouseUp { x, y, .. } if self.search_pressed() => {
                self.search_mouse_up(x, y);
                window.request_redraw();
            }
            Event::MouseUp { .. } if self.edit_menu_open() => {
                self.edit_popup_up();
                window.request_redraw();
            }
            Event::MouseMove { x, y, dragging } if self.edit_popup_move(x, y, dragging, window) => {
            }
            // Le survol de la carte de recherche ; un bouton enfoncé suit le
            // pointeur même bouton tenu, pour remonter quand on glisse dehors.
            Event::MouseMove { x, y, dragging }
                if self.palette.is_none()
                    && (!dragging || self.search_pressed())
                    && self.search_hover(x, y, window) => {}
            Event::MouseUp { .. } if self.objects.is_some() => {
                self.objects_mouse_up();
                window.request_redraw();
            }
            Event::MouseMove { x, y, dragging } if self.objects.is_some() => {
                let (vx, vy) = (x - self.view_left() as i32, y - self.view_top() as i32);
                if self.objects_mouse_move(vx, vy, dragging) {
                    window.request_redraw();
                }
                let grab = self
                    .objects
                    .as_ref()
                    .is_some_and(|t| t.drag.is_some() || t.hover.is_some() || t.handle.is_some());
                window.set_cursor(if grab { Cursor::Move } else { Cursor::Arrow });
            }
            Event::MouseUp { .. } if self.capture.is_some() => {
                let action = self
                    .capture
                    .as_mut()
                    .map_or(sign::Action::None, Capture::mouse_up);
                self.sign_action(action, window);
            }
            Event::MouseMove { x, y, dragging } if self.capture.is_some() => {
                let action = self
                    .capture
                    .as_mut()
                    .map_or(sign::Action::None, |c| c.mouse_move(x, y, dragging));
                self.sign_action(action, window);
            }
            Event::MouseUp { x, y, .. } if self.carrying => {
                self.carrying = false;
                let (vx, vy) = (x - self.view_left() as i32, y - self.view_top() as i32);
                if vx >= 0 && vy >= 0 && vy < self.view_height() as i32 {
                    self.place_sign(vx, vy, window);
                }
                window.request_redraw();
            }
            Event::MouseUp { .. } if self.placed.as_ref().is_some_and(|p| p.drag.is_some()) => {
                self.placed_mouse_up();
                window.request_redraw();
            }
            Event::MouseUp { .. } if self.inking.is_some() => {
                self.ink_finish();
                window.request_redraw();
            }
            Event::MouseUp { .. } => {
                self.three_d_mouse_up();
                self.edit_mouse_up();
                self.field_mouse_up();
                if self.panel.dragging() {
                    let action = self.panel.mouse_up();
                    self.panel_action(action, window);
                } else {
                    let _ = self.panel.mouse_up();
                }
                self.drag_last = None;
                let was_selecting = self.sel_dragging;
                self.sel_dragging = false;
                if self.selection.is_some_and(|s| s.is_empty()) {
                    self.selection = None;
                }
                if was_selecting {
                    self.apply_annot_tool();
                }
            }
            Event::MouseMove { x, y, dragging } if self.three_d_mouse_move(x, y, window) => {
                let _ = dragging;
            }
            // Un glisser commencé dans le champ en saisie y sélectionne.
            Event::MouseMove {
                x,
                y,
                dragging: true,
            } if self.field_selecting() => {
                let (vx, vy) = (x - self.view_left() as i32, y - self.view_top() as i32);
                self.field_drag(vx, vy);
            }
            Event::MouseMove { x, y, dragging } => {
                let bar = Toolbar::height(&self.theme, self.dpi_scale as f32);
                let mut hover_changed = self.toolbar.mouse_move(x, y);
                if let Some(mode) = &mut self.edit {
                    hover_changed |= mode.bar.mouse_move(x, y, dragging).is_some();
                }
                if self.annot_tool.is_some() || self.comment_bar {
                    hover_changed |= self.mode_bar.mouse_move(x, y);
                }
                hover_changed |= self.form_bar_hover(x, y);
                // Les onglets avant l'info-bulle : sans quoi leur survol ne
                // la déclenchait jamais.
                let tabs = self.tabs_height() as i32;
                if tabs > 0 && y >= bar && y < bar + tabs {
                    hover_changed |= self.tabs.mouse_move(x, y - bar);
                } else {
                    hover_changed |= self.tabs.mouse_leave();
                }
                hover_changed |= self.status_mouse_move(x, y);
                if hover_changed {
                    self.update_tip(window);
                }
                let in_sign_panel = self.sign_panel_open()
                    && x < self.view_left() as i32
                    && y >= self.view_top() as i32;
                let panel_top = self.view_top() as i32;
                if let Some(panel) = self.sign_panel.as_mut() {
                    let top = panel_top;
                    hover_changed |= if in_sign_panel {
                        panel.mouse_move(x, y - top)
                    } else {
                        panel.leave()
                    };
                }
                let in_panel = self.panel_open
                    && !in_sign_panel
                    && x < self.view_left() as i32
                    && y >= self.view_top() as i32;
                if in_panel {
                    hover_changed |= self
                        .panel
                        .mouse_move(x, y - self.view_top() as i32, dragging);
                } else {
                    hover_changed |= self.panel.mouse_leave();
                }
                let in_tools = self.tools_width() > 0
                    && x >= self.width.saturating_sub(self.tools_width()) as i32
                    && y >= self.view_top() as i32;
                if in_tools {
                    let dpi = self.dpi_scale;
                    hover_changed |= self.tools.hover(f64::from(y - self.view_top() as i32), dpi);
                } else {
                    hover_changed |= self.tools.leave();
                }
                let (x, y) = (x - self.view_left() as i32, y - self.view_top() as i32);
                self.last_mouse = (x >= 0 && y >= 0).then_some((x, y));
                // Sur l'accueil, survoler un document récent en dit le chemin.
                // Et une info-bulle affichée se réévalue à chaque mouvement :
                // sans cela, celle d'un document récent survivait à
                // l'ouverture du document et restait plantée sur la page.
                if self.showing_home() || self.tip.is_some() {
                    self.update_tip(window);
                }
                if self.edit_on() && self.prompt.is_none() && self.drag_last.is_none() {
                    self.edit_mouse_move(x, y, dragging, window);
                    if hover_changed {
                        window.request_redraw();
                    }
                    return;
                }
                if self.sign_panel.is_some()
                    && self
                        .placed
                        .as_ref()
                        .is_some_and(|p| p.drag.is_some() || !dragging)
                    && self.placed_mouse_move(x, y, dragging)
                {
                    window.request_redraw();
                    if dragging {
                        return;
                    }
                }
                if self.inking.is_some() && dragging {
                    self.ink_move(x, y);
                    window.request_redraw();
                    return;
                }
                if self.sel_dragging && dragging {
                    if let Some((pos, _)) = self.text_pos_at(x, y) {
                        if let Some(sel) = &mut self.selection {
                            sel.focus = pos;
                        }
                    }
                    // Défilement automatique aux bords pendant la sélection.
                    let vh = self.view_height() as i32;
                    if y < 0 {
                        self.scroll_y -= f64::from(-y).min(40.0);
                    } else if y > vh {
                        self.scroll_y += f64::from(y - vh).min(40.0);
                    }
                    self.clamp_scroll();
                } else if let Some((lx, ly)) = self.drag_last {
                    self.scroll_x -= f64::from(x - lx);
                    self.scroll_y -= f64::from(y - ly);
                    self.clamp_scroll();
                    self.drag_last = Some((x, y));
                } else if !dragging {
                    if self.loaded.is_none() {
                        // La liste d'accueil se surligne au survol.
                        window.request_redraw();
                    }
                    let in_view =
                        self.prompt.is_none() && x >= 0 && y >= 0 && y < self.view_height() as i32;
                    let sign_item = self.sign_panel.as_ref().and_then(|p| p.item);
                    let cursor = if in_view && sign_item == Some(SignItem::Draw) {
                        Cursor::Pen
                    } else if in_view && sign_item == Some(SignItem::Text) {
                        Cursor::AddText
                    } else if in_view && sign_item.is_some() {
                        Cursor::Place
                    } else if in_view && self.annot_tool.is_some() {
                        // Souligner, barrer, remplacer, insérer : on vise du
                        // texte, la barre de texte le dit.
                        match self.annot_tool {
                            Some(AnnotTool::Note) => Cursor::Note,
                            Some(AnnotTool::Redact) => Cursor::Redact,
                            Some(AnnotTool::Highlight) => Cursor::Highlight,
                            _ => Cursor::IBeam,
                        }
                    } else if in_view && self.model_at(x, y).is_some() {
                        // Un modèle 3D se prend en main : la main dit qu'il y
                        // a quelque chose à saisir.
                        Cursor::Move
                    } else if let Some(cursor) = in_view.then(|| self.widget_cursor(x, y)).flatten()
                    {
                        // La barre de texte là où l'on tape, la main sur une
                        // case, un bouton ou une liste.
                        cursor
                    } else if in_view && self.link_at(x, y).is_some() {
                        Cursor::Hand
                    } else if in_view && self.text_pos_at(x, y).is_some_and(|(_, on)| on) {
                        Cursor::IBeam
                    } else {
                        Cursor::Arrow
                    };
                    window.set_cursor(cursor);
                    // L'aperçu de ce qu'on va poser suit le pointeur : il
                    // faut repeindre à chaque mouvement.
                    if hover_changed || sign_item.is_some_and(|i| i != SignItem::Draw) {
                        window.request_redraw();
                    }
                    return;
                }
            }
            Event::MouseDown { .. } => {}
            Event::Close => self.guard_quit(window),
        }
        if self.title_dirty {
            self.update_title(window);
        }
        // La recherche et les vignettes de l'écran d'accueil avancent par
        // petites tranches, **pendant la peinture** (`paint_all`), qui
        // demande un réveil par image tant qu'il en reste. Surtout pas
        // d'ici : cette fonction traite aussi les réveils, et chacun en
        // posterait un autre — la file ne se viderait plus, et Windows, qui
        // ne repeint que file vide, figerait la fenêtre jusqu'à la fin du
        // travail. Il suffit que le réveil existe avant la peinture.
        if self.waker.is_none() && (self.search_scanning() || self.welcome_pending()) {
            self.waker = Some(window.waker());
        }
        window.request_redraw();
    }

    /// Dessine la barre des commentaires, ou celle de l'outil d'annotation
    /// en cours, s'il y en a une.
    fn paint_mode_bar(&mut self, frame: &mut Frame<'_>) {
        if self.mode_bar_height() == 0 {
            return;
        }
        let y = Toolbar::height(&self.theme, self.dpi_scale as f32)
            + self.tabs_height() as i32
            + self.edit_bar_height() as i32;
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        let Some(text) = self.text.as_mut() else {
            return;
        };
        if self.comment_bar {
            let icons: Vec<Icon> = COMMENT_TOOLS.iter().map(|t| t.icon()).collect();
            let active = self
                .annot_tool
                .and_then(|t| COMMENT_TOOLS.iter().position(|c| *c == t));
            let hint = self
                .annot_tool
                .map_or("Sélectionnez du texte, puis choisissez un outil.", |t| {
                    t.describe().1
                });
            self.mode_bar.paint_tools(
                frame,
                text,
                &mut self.raster,
                &theme,
                dpi,
                y,
                &icons,
                active,
                hint,
            );
        } else if let Some(tool) = self.annot_tool {
            let (title, hint) = tool.describe();
            self.mode_bar.paint(
                frame,
                text,
                &mut self.raster,
                &theme,
                dpi,
                y,
                tool.icon(),
                title,
                hint,
            );
        }
    }

    /// Peint toute la fenêtre.
    fn paint_all(&mut self, frame: &mut Frame<'_>) {
        self.collect_results();
        self.poll_updates();
        // Avant de peindre : ce que trouve la recherche, et le défilement
        // vers sa première occurrence, se voient dans cette image-ci.
        let searching = self.search_scanning() && self.step_search();
        let t = self.theme;
        log_line(&format!("peinture : canvas {:?}", t.canvas));
        frame.clear(t.canvas.0, t.canvas.1, t.canvas.2);
        // La zone de document est peinte dans une sous-vue (les lignes de
        // pixels entre la barre d'outils et la barre d'état) : le code de
        // rendu des pages travaille en coordonnées de vue, sans décalage.
        let (left, top) = (self.view_left() as i32, self.view_top() as i32);
        let (vw, vh) = (self.view_width(), self.view_height());
        self.thumb_key = if self.panel_open {
            self.thumb_geometry().1
        } else {
            0
        };
        if vw > 0 && vh > 0 {
            let mut view = frame.sub(left, top, vw, vh);
            if self.showing_home() {
                view.clear(t.canvas.0, t.canvas.1, t.canvas.2);
                self.paint_welcome(&mut view);
            } else {
                self.paint_document(&mut view);
                self.paint_fields(&mut view);
            }
            self.paint_3d(&mut view);
            self.paint_selection(&mut view);
            self.paint_edit(&mut view);
            self.paint_objects(&mut view);
            self.paint_inking(&mut view);
            self.paint_sign_ghost(&mut view);
            if self.sign_panel.is_some() {
                self.paint_placed(&mut view);
            }
            self.paint_media(&mut view);
            self.paint_field_focus(&mut view);
            self.paint_search(&mut view);
        }
        if self.sign_panel_open() && left > 0 && vh > 0 {
            let mut side = frame.sub(0, top, left as u32, vh);
            self.paint_sign_panel(&mut side);
        } else if self.panel_open && left > 0 && vh > 0 {
            let mut side = frame.sub(0, top, left as u32, vh);
            self.paint_panel(&mut side);
        }
        let tools_w = self.tools_width();
        if tools_w > 0 && vh > 0 {
            let x = self.width.saturating_sub(tools_w) as i32;
            let mut side = frame.sub(x, top, tools_w, vh);
            let info = self.tools_info();
            let (theme, dpi) = (self.theme, self.dpi_scale);
            if let Some(text) = self.text.as_mut() {
                self.tools
                    .paint(&mut side, text, &mut self.raster, &theme, dpi, info);
            }
        }
        if !self.fullscreen && !self.reading {
            self.paint_toolbar(frame);
            self.paint_tabs(frame);
            if self.edit_on() {
                self.paint_edit_bar(frame);
            }
            self.paint_mode_bar(frame);
            self.paint_form_bar(frame);
            self.paint_status(frame);
        } else {
            // Plein écran, lecture : pas de barre d'état, rien à y cliquer.
            self.status_hits.clear();
            self.status_hover = None;
        }
        // L'info-bulle parle d'un bouton de la barre : elle passe sous les
        // cartes qui s'ouvrent par-dessus, avec lui.
        self.paint_tip(frame);
        self.paint_zoom_menu(frame);
        self.paint_field_menu(frame);
        self.paint_capture(frame);
        self.paint_protect(frame);
        self.paint_prompt(frame);
        self.paint_settings(frame);
        self.paint_palette(frame);
        self.paint_context_menu(frame);
        self.paint_dialog(frame);
        dump_frame(frame);
        self.after_paint(searching);
    }
}

impl Viewer {
    fn key(&mut self, key: Key, m: Modifiers, window: &mut dyn WindowHandle) {
        if key == Key::Escape && self.home && self.loaded.is_some() {
            self.leave_home();
            window.request_redraw();
            return;
        }
        // Un champ qui a le focus prend ses flèches : la ligne d'une liste,
        // le bouton d'un groupe radio, Alt+↓ pour une liste déroulante.
        if self.focus_field.is_some() && self.field_nav_key(key, m, window) {
            return;
        }
        let page_h = f64::from(self.view_height());
        match key {
            // F3 est la touche « suivante » de Windows ; la colonne des
            // outils passe à Maj+F4, le raccourci du volet Outils d'Acrobat.
            Key::F(3) => self.find_again(!m.shift),
            Key::F(4) if m.shift => {
                self.tools_open = !self.tools_open;
                self.wake_anim();
                self.clamp_scroll();
                self.save_prefs();
            }
            Key::F(4) => self.toggle_panel(),
            Key::F(5) => self.toggle_reading(window),
            Key::F(6) => self.cycle_region(!m.shift, window),
            Key::F(11) => self.toggle_fullscreen(window),
            // Ctrl+Tab change d'onglet ; il est pris plus tôt, quelle que
            // soit la zone (voir `handle_event`).
            Key::Tab if !m.ctrl => {
                // Comme dans Acrobat : Tab entre dans le champ suivant, son
                // texte sélectionné, prêt à être remplacé.
                self.focus_next_field(!m.shift);
                self.enter_focused_field(window);
            }
            Key::Enter => self.activate_focused_field(window),
            Key::Delete if m.ctrl => self.delete_current(),
            Key::Down => self.glide_by(70.0),
            Key::Up => self.glide_by(-70.0),
            Key::Right => self.scroll_x += 60.0,
            Key::Left => self.scroll_x -= 60.0,
            Key::PageDown => {
                // En mode « une rangée à la fois », la page est l'unité de
                // défilement dès qu'on est en bas de la rangée affichée.
                if m.ctrl || (self.view_mode.is_paged() && self.at_bottom()) {
                    self.step_row(true);
                } else {
                    self.glide_by(page_h * 0.9);
                }
            }
            Key::PageUp => {
                if m.ctrl || (self.view_mode.is_paged() && self.scroll_y <= 0.5) {
                    self.step_row(false);
                } else {
                    self.glide_by(-page_h * 0.9);
                }
            }
            // Origine et Fin sont des sauts, comme dans Acrobat : Alt+← ramène
            // là où l'on était.
            Key::Home => self.navigate(|v| {
                if v.view_mode.is_paged() {
                    v.scroll_to_page(0);
                } else {
                    v.scroll_y = 0.0;
                    v.clamp_scroll();
                }
            }),
            Key::End => {
                let last = self
                    .loaded
                    .as_ref()
                    .map_or(0, |l| l.pages.len().saturating_sub(1));
                self.navigate(|v| {
                    if v.view_mode.is_paged() {
                        v.scroll_to_page(last);
                    } else {
                        v.scroll_y = f64::MAX / 4.0;
                        v.clamp_scroll();
                    }
                });
            }
            Key::Escape => {
                if self.media.is_some() {
                    self.close_media();
                    self.set_notice("lecture arrêtée".into());
                } else if self.objects.is_some() {
                    self.toggle_objects(window);
                } else if self.sign_panel.is_some() {
                    self.sign_action(sign::Action::Close, window);
                } else if self.reading {
                    self.toggle_reading(window);
                } else if self.fullscreen {
                    self.toggle_fullscreen(window);
                } else if self.focus_field.is_some() {
                    self.focus_field = None;
                } else {
                    self.selection = None;
                }
            }
            _ => {}
        }
        self.clamp_scroll();
    }
}

/// Vignette de la première page d'un fichier, au plus grand format qui tient
/// dans `max_w` × `max_h`. `None` si le fichier ne s'ouvre pas.
///
/// C'est un rendu complet, comme celui de la vue : une vignette fidèle vaut
/// mieux qu'une icône générique pour reconnaître un document.
fn render_thumbnail(path: &Path, max_w: f64, max_h: f64) -> Option<Bitmap> {
    let doc = Document::load(path).ok()?;
    let pages = collect_pages(&doc).ok()?;
    let page = pages.first()?;
    let crop = page.crop_box(&doc);
    let (pw, ph) = match page.rotate(&doc) {
        90 | 270 => (crop.height(), crop.width()),
        _ => (crop.width(), crop.height()),
    };
    if pw < 1.0 || ph < 1.0 {
        return None;
    }
    let scale = (max_w / pw).min(max_h / ph).clamp(0.01, 4.0);
    let options = RenderOptions {
        annotations: true,
        // Une vignette n'a pas le droit de figer l'écran d'accueil.
        time_budget: Some(std::time::Duration::from_millis(400)),
        ..RenderOptions::default()
    };
    Some(render_page(&doc, page, scale, &options).bitmap)
}

/// Dossier, taille et date d'un fichier, en une ligne.
fn describe_file(path: &Path) -> String {
    let dir = path
        .parent()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    let Ok(meta) = std::fs::metadata(path) else {
        return dir;
    };
    let human = context::size_text(meta.len(), lang::english());
    let day = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| day_label(d.as_secs()))
        .unwrap_or_default();
    if day.is_empty() {
        format!("{human} · {dir}")
    } else {
        format!("{human} · {day} · {dir}")
    }
}

/// Date d'un horodatage Unix, en jours, sous la forme « 19/09/2026 ».
fn day_label(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    // Algorithme civil depuis 1970 (Howard Hinnant), sans dépendance.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{d:02}/{m:02}/{y}")
}

/// Langue de l'interface du système, telle que la comprend Acrux.
fn system_lang() -> Lang {
    match crate::platform::system_language().as_str() {
        "fr" => Lang::French,
        _ => Lang::English,
    }
}

#[cfg(test)]
#[allow(clippy::panic)] // tests
mod tests {
    use super::*;

    #[test]
    fn rows_one_up_are_single_pages() {
        assert_eq!(page_rows(0, false, false), Vec::new());
        assert_eq!(page_rows(3, false, false), vec![(0, 0), (1, 1), (2, 2)]);
        // L'option « couverture » n'a pas de sens sur une colonne.
        assert_eq!(page_rows(3, false, true), vec![(0, 0), (1, 1), (2, 2)]);
    }

    #[test]
    fn rows_two_up_pair_pages_and_handle_the_cover() {
        assert_eq!(page_rows(4, true, false), vec![(0, 1), (2, 3)]);
        // Nombre impair : la dernière rangée n'a qu'une page.
        assert_eq!(page_rows(5, true, false), vec![(0, 1), (2, 3), (4, 4)]);
        assert_eq!(page_rows(1, true, false), vec![(0, 0)]);
        // Avec couverture : page 1 seule, puis des paires.
        assert_eq!(page_rows(5, true, true), vec![(0, 0), (1, 2), (3, 4)]);
        assert_eq!(page_rows(2, true, true), vec![(0, 0), (1, 1)]);
    }

    /// Un document qui ne permet que l'impression en basse résolution est
    /// rendu à 150 ppp au plus, quelle que soit l'imprimante.
    #[test]
    fn low_resolution_printing_is_capped() {
        let Ok(doc) =
            acrux_features::create::new_document(&acrux_features::create::PageSetup::default())
        else {
            panic!("document A4");
        };
        let Ok(pages) = collect_pages(&doc) else {
            panic!("pages");
        };
        let width_at = |max_dpi: Option<f64>| {
            let mut source = PrintPages {
                doc: &doc,
                pages: &pages,
                max_dpi,
            };
            source.render(0, 600.0).map_or(0, |(w, _, _)| w)
        };
        // A4 : 595 pt, soit 8,26 pouces.
        let free = width_at(None);
        let capped = width_at(Some(protect::LOW_PRINT_DPI));
        assert!((4900..=5000).contains(&free), "{free}");
        assert!((1230..=1250).contains(&capped), "{capped}");
    }

    /// Le panneau montre un remplacement une seule fois, sous son nom, et
    /// distingue l'insertion seule ; un lien n'est pas un commentaire.
    #[test]
    fn comments_name_and_group_the_review_marks() {
        use acrux_features::annotations::add_annotation;
        let Ok(doc) =
            acrux_features::create::new_document(&acrux_features::create::PageSetup::default())
        else {
            panic!("document A4");
        };
        let page = || collect_pages(&doc).map(|mut p| p.remove(0));
        let line = Rect::new(72.0, 700.0, 200.0, 712.0);
        let annots = [
            NewAnnotation::Replace {
                quads: vec![line],
                text: "nouveau".into(),
                strike: MarkupKind::StrikeOut.default_color(),
                caret: CARET_COLOR,
            },
            NewAnnotation::Caret {
                x: 100.0,
                line: Rect::new(72.0, 650.0, 80.0, 662.0),
                contents: "ajout".into(),
                color: CARET_COLOR,
            },
            NewAnnotation::Markup {
                kind: MarkupKind::Underline,
                quads: vec![Rect::new(72.0, 600.0, 200.0, 612.0)],
                color: MarkupKind::Underline.default_color(),
                contents: None,
            },
            NewAnnotation::Link {
                rect: line,
                uri: "https://example.org".into(),
            },
        ];
        for a in &annots {
            let Ok(p) = page() else { panic!("page") };
            assert!(
                add_annotation(&doc, &p, a, Some("Zoé")).is_ok(),
                "annotation {a:?}"
            );
        }
        let Ok(pages) = collect_pages(&doc) else {
            panic!("pages")
        };
        let rows = collect_comments(&doc, &pages);
        let kinds: Vec<&str> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, ["Remplacement", "Insertion", "Soulignement"]);
        assert_eq!(rows[0].contents, "nouveau");
        assert_eq!(rows[0].index, 0);
        // Le barré groupé est sauté : l'insertion vient juste après lui.
        assert_eq!(rows[1].index, 2);
        assert!(rows.iter().all(|r| r.name.is_some()));
    }

    /// Le passage recopié dans un balisage tient sur une ligne, sans blancs
    /// en trop : les retours à la ligne de la page n'y ont plus de sens.
    #[test]
    fn one_line_flattens_the_passage() {
        assert_eq!(
            one_line("  Ce document\na été\t généré \r\n par Edge "),
            "Ce document a été généré par Edge"
        );
        assert_eq!(one_line(" \n "), "");
    }

    /// Chaque outil de la barre des commentaires a son nom, sa consigne,
    /// son icône et des commandes que la palette connaît.
    #[test]
    fn every_comment_tool_is_described() {
        for tool in COMMENT_TOOLS {
            let (name, hint) = tool.describe();
            assert!(!name.is_empty() && !hint.is_empty(), "{tool:?}");
            assert!(
                crate::ui::palette::describe(tool.command()).is_some(),
                "{tool:?} : outil absent de la palette"
            );
            assert!(
                crate::ui::palette::describe(tool.action()).is_some(),
                "{tool:?} : action absente de la palette"
            );
            assert!(tool.in_comment_bar());
        }
        assert!(!AnnotTool::Redact.in_comment_bar());
        // Les quatre balisages et eux seuls posent un balisage.
        let markups = COMMENT_TOOLS
            .iter()
            .filter(|t| t.markup().is_some())
            .count();
        assert_eq!(markups, 4);
    }

    #[test]
    fn row_of_finds_the_containing_row() {
        let rows = page_rows(5, true, true);
        assert_eq!(row_of(&rows, 0), 0);
        assert_eq!(row_of(&rows, 1), 1);
        assert_eq!(row_of(&rows, 2), 1);
        assert_eq!(row_of(&rows, 4), 2);
        // Page hors du document : première rangée, jamais de panique.
        assert_eq!(row_of(&rows, 99), 0);
        assert_eq!(row_of(&[], 3), 0);
    }
}
