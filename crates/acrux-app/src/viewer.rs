//! Visualiseur : affiche les pages d'un document avec défilement continu,
//! zoom, navigation clavier, ouverture par dialogue ou glisser-déposer, et
//! une barre d'état dessinée par le toolkit interne. Les pages sont rendues
//! sur un fil séparé (`render_worker`) : l'interface ne se fige jamais.
//!
//! Fonctions : recherche (Ctrl+F), sélection de texte à la souris (simple,
//! double et triple clic), copie (Ctrl+C), tout sélectionner (Ctrl+A),
//! saisie du mot de passe des documents chiffrés.
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use acrux_core::{Matrix, Point, Rect};
use acrux_document::{collect_pages, Document, Page};
use acrux_features::annotations::{list_annotations, NewAnnotation};
use acrux_features::attach::{list_attachments, read_attachment};
use acrux_features::forms::{list_fields, Field, FieldType, FieldValue};
use acrux_features::navigation::{
    flatten_outline, outline, page_links, Action, Destination, Link, PageIndex, View,
};
use acrux_features::redact::RedactionMark;
use acrux_features::text::{extract_page_text, find, PageText};
use acrux_graphics::{Bitmap, Color, Rasterizer};
use acrux_render::page::base_matrix;
use acrux_render::{page_pixel_size, render_page, RenderOptions};

use crate::platform::{
    App, Cursor, Event, Frame, Key, Modifiers, MouseButton, PrintOutcome, PrintSource, Waker,
    WindowHandle,
};
use crate::render_worker::ExportFormat;
use crate::render_worker::{EditOp, RenderWorker};
use crate::selection::{SelectableText, Selection, TextPos};
use crate::ui::input::{InputAction, TextInput};
use crate::ui::objects::{self as objects_ui, Handle, ViewRect};
use crate::ui::palette::{Command, Palette};
use crate::ui::panel::{
    AttachmentRow, CommentRow, OutlineRow, Panel, PanelAction, PanelContent, PanelTab,
};
use crate::ui::prefs::{Prefs, ViewMode};
use crate::ui::sign::{self, Bar as SignBar, Capture, Item as SignItem, Saved};
use crate::ui::tabs::{TabAction, TabInfo, Tabs};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;
use crate::ui::toolbar::{ToolAction, Toolbar, ToolbarInfo};
use crate::ui::video::{self as video_ui, Box2, Hit as VideoHit};
use acrux_features::edit_objects::{self, Edit as ObjectEdit, PageObject};

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
fn is_global_key(key: Key) -> bool {
    matches!(key, Key::F(4 | 5 | 6 | 11))
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

/// Commentaires du document : les annotations qui portent un texte ou une
/// intention de relecture. Les liens et les champs de formulaire n'en sont
/// pas — ils ont déjà leur place ailleurs dans l'interface.
fn collect_comments(doc: &Document, pages: &[Page]) -> Vec<CommentRow> {
    let mut out = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let Ok(list) = list_annotations(doc, page) else {
            continue;
        };
        for a in list {
            let kind = match a.subtype.as_str() {
                "Text" => "Note",
                "Highlight" => "Surlignage",
                "Underline" => "Soulignement",
                "StrikeOut" => "Texte barré",
                "Squiggly" => "Soulignement ondulé",
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
                kind: kind.to_string(),
                author: a.author.clone(),
                contents: a.contents.clone().unwrap_or_default(),
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
/// Paliers de zoom.
const ZOOM_STEPS: [f64; 16] = [
    0.25, 0.33, 0.5, 0.67, 0.75, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 6.0,
];

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
    /// Modifications non enregistrées.
    modified: bool,
    /// Champs de formulaire (AcroForm), rechargés après chaque modification.
    fields: Vec<Field>,
    /// Commentaires (annotations porteuses de texte), pour le panneau.
    comments: Vec<CommentRow>,
    /// Calques du document et leur visibilité courante (numéro, nom, visible).
    layers: Vec<(u32, String, bool)>,
    /// Pièces jointes, pour le panneau, rechargées après chaque modification.
    attachments: Vec<AttachmentRow>,
    /// Vidéos et sons du document, avec leur rectangle : c'est ce qui rend un
    /// clic capable de savoir qu'il tombe sur un média.
    media: Vec<acrux_features::media::Media>,
    /// Étiquette de chaque page (`/PageLabels`). Vide quand le document s'en
    /// tient à la numérotation décimale : c'est ce qui distingue « iii sur
    /// 240 » de « 3 sur 240 » dans la barre d'outils et la barre d'état.
    labels: Vec<String>,
}

impl Loaded {
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
    /// Texte d'une note à poser sur `page` au point `(x, y)` (espace page).
    Note { page: usize, x: f64, y: f64 },
    /// Valeur d'un champ de formulaire.
    Field { name: String, kind: FieldType },
    /// Commentaire à joindre à un surlignage déjà découpé en zones.
    Highlight { zones: Vec<(usize, Rect)> },
    /// Texte de remplissage à poser sur `page`, dans `rect`.
    SignText { page: usize, rect: Rect },
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
}

/// Pages du document courant vues par l'impression.
struct PrintPages<'a> {
    doc: &'a Document,
    pages: &'a [Page],
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
    /// Zoom logique (1.0 = 100 % à 96 dpi).
    zoom: f64,
    fit_width: bool,
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
    /// Dernier temps de rendu d'une page (ms), pour la barre d'état.
    last_render_ms: f64,
    /// Recherche en cours (Ctrl+F).
    search: Option<Search>,
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
    /// Outil « modifier » : objets de la page courante et sélection.
    objects: Option<ObjectTool>,
    /// Média en cours de lecture, s'il y en a un.
    media: Option<MediaView>,
    /// Résultat de la recherche de mise à jour en cours, s'il y en a une.
    update_rx: Option<std::sync::mpsc::Receiver<Result<crate::update::Release, String>>>,
    /// Version plus récente trouvée, en attente que l'utilisateur en décide.
    update_found: Option<crate::update::Release>,
    /// Vrai si la recherche en cours a été demandée par l'utilisateur.
    update_asked: bool,
    /// La recherche du démarrage a déjà été lancée.
    update_started: bool,
    /// Outil « remplir et signer » : la barre est affichée quand il est actif.
    sign_bar: Option<SignBar>,
    /// Fenêtre de capture d'une signature, ouverte par-dessus tout.
    capture: Option<Capture>,
    /// Signature enregistrée, conservée entre deux sessions.
    signature: Option<Saved>,
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
    /// Champ de formulaire ayant le focus clavier (indice dans `fields`).
    focus_field: Option<usize>,
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
}

/// État de la recherche dans le document.
struct Search {
    input: TextInput,
    /// Occurrences : (page, boîte en espace PDF).
    hits: Vec<(usize, Rect)>,
    /// Occurrence courante.
    current: usize,
    /// Dernière requête évaluée.
    last_query: String,
    /// Nombre de pages déjà parcourues : la recherche avance par tranches
    /// pour ne jamais figer l'interface sur un gros document.
    scanned: usize,
}

impl Viewer {
    /// Nouveau visualiseur, avec les fichiers à ouvrir au démarrage (un
    /// onglet chacun).
    #[must_use]
    pub fn new(initial: Vec<PathBuf>) -> Self {
        let prefs = Prefs::load();
        let signature = prefs.signature.as_deref().and_then(Saved::decode);
        let initials = prefs.initials.as_deref().and_then(Saved::decode);
        Self {
            loaded: None,
            error: None,
            width: 0,
            height: 0,
            dpi_scale: 1.0,
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
            fit_width: prefs.fit_width,
            panel_open: prefs.panel_open,
            view_mode: prefs.view_mode,
            two_up_cover: prefs.two_up_cover,
            prefs,
            text: TextRenderer::system(),
            raster: Rasterizer::new(),
            toolbar: Toolbar::new(),
            last_render_ms: 0.0,
            search: None,
            selection: None,
            sel_dragging: false,
            last_click: None,
            prompt: None,
            last_mouse: None,
            others: Vec::new(),
            active_tab: 0,
            tabs: Tabs::new(),
            palette: None,
            objects: None,
            media: None,
            update_rx: None,
            update_found: None,
            update_asked: false,
            update_started: false,
            sign_bar: None,
            capture: None,
            signature,
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
            panel: Panel::new(),
            last_current: usize::MAX,
            thumb_key: 0,
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
        let m = base_matrix(&p.crop_box(&l.doc), self.scale(), p.rotate(&l.doc), w, h);
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

    /// Champ de formulaire sous un point de la fenêtre : (indice du champ,
    /// indice du widget).
    fn widget_at(&mut self, x: i32, y: i32) -> Option<(usize, usize)> {
        let (page, pt) = self.page_at(x, y)?;
        let l = self.loaded.as_ref()?;
        for (fi, f) in l.fields.iter().enumerate() {
            for (wi, w) in f.widgets.iter().enumerate() {
                if w.page == Some(page) && w.rect.contains(pt) {
                    return Some((fi, wi));
                }
            }
        }
        None
    }

    /// Clic sur un widget de formulaire : bascule, sélection ou saisie.
    fn click_widget(&mut self, fi: usize, wi: usize, window: &mut dyn WindowHandle) {
        let Some(l) = &self.loaded else { return };
        let Some(field) = l.fields.get(fi) else {
            return;
        };
        if field.flags.read_only {
            return;
        }
        let name = field.name.clone();
        let kind = field.kind;
        let current = field
            .value
            .as_ref()
            .map(FieldValue::to_display)
            .unwrap_or_default();
        match kind {
            FieldType::CheckBox => {
                let checked = matches!(&field.value, Some(FieldValue::State(s)) if s != "Off")
                    || matches!(&field.value, Some(FieldValue::Bool(true)));
                self.apply_edit(
                    EditOp::SetField {
                        name,
                        value: FieldValue::Bool(!checked),
                    },
                    window,
                );
            }
            FieldType::Radio => {
                let Some(state) = field.widgets.get(wi).and_then(|w| w.on_state.clone()) else {
                    return;
                };
                self.apply_edit(
                    EditOp::SetField {
                        name,
                        value: FieldValue::State(state),
                    },
                    window,
                );
            }
            FieldType::Text | FieldType::ComboBox | FieldType::ListBox => {
                let label = if kind == FieldType::Text {
                    format!("Valeur du champ « {name} » :")
                } else {
                    let opts: Vec<String> =
                        field.options.iter().map(|o| o.export.clone()).collect();
                    format!(
                        "« {name} » — options : {}{} :",
                        opts.join(", "),
                        if field.flags.multi_select {
                            " (séparer par |)"
                        } else {
                            ""
                        }
                    )
                };
                let mut input = TextInput::new("Valeur");
                input.value = current;
                input.caret = input.value.chars().count();
                self.prompt = Some(Prompt {
                    title: "Champ de formulaire".into(),
                    label,
                    input,
                    error: None,
                    kind: PromptKind::Field { name, kind },
                });
            }
            FieldType::Button | FieldType::Signature | FieldType::Unknown => {}
        }
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
            self.fit_width = true;
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
            page.rotate(&l.doc),
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
        match action {
            Action::GoTo(d) => self.go_to(d),
            Action::Uri(u) => window.open_url(u),
            Action::Named(n) => match n.as_str() {
                "NextPage" => self.scroll_to_page(self.current_page() + 1),
                "PrevPage" => self.scroll_to_page(self.current_page().saturating_sub(1)),
                "FirstPage" => self.scroll_to_page(0),
                "LastPage" => {
                    let last = self
                        .loaded
                        .as_ref()
                        .map_or(0, |l| l.pages.len().saturating_sub(1));
                    self.scroll_to_page(last);
                }
                _ => {}
            },
            Action::Launch(_) | Action::GoToRemote(_) | Action::Unsupported(_) => {}
        }
    }

    /// Clic gauche dans le document : sélection (texte sous le pointeur) ou
    /// début de déplacement.
    fn mouse_down(&mut self, x: i32, y: i32, clicks: u8, shift: bool) {
        let now = Instant::now();
        let series = match self.last_click {
            Some((t, lx, ly, n))
                if now.duration_since(t) < MULTI_CLICK
                    && (x - lx).abs() < 4
                    && (y - ly).abs() < 4 =>
            {
                if clicks >= 2 {
                    2
                } else {
                    n + 1
                }
            }
            _ => 1,
        };
        self.last_click = Some((now, x, y, series));
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
                let m = base_matrix(&p.crop_box(&l.doc), scale, p.rotate(&l.doc), w, h);
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
    fn paint_welcome(&mut self, frame: &mut Frame<'_>) {
        let t = self.theme;
        let dpi = self.dpi_scale as f32;
        let hover = self.last_mouse;
        let recent: Vec<(String, String)> = self
            .prefs
            .recent
            .iter()
            .map(|p| {
                (
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    p.parent()
                        .map(|d| d.display().to_string())
                        .unwrap_or_default(),
                )
            })
            .collect();
        let mut hits: Vec<(i32, i32, i32, i32)> = Vec::new();
        let Some(text) = &mut self.text else {
            self.recent_hits.clear();
            return;
        };
        let size = t.font_size * dpi;
        let x = (48.0 * dpi) as i32;
        let mut y = (56.0 * dpi) as i32;
        let title = size * 2.0;
        text.draw(
            frame,
            x as f32,
            y as f32 + text.ascent(title),
            title,
            "Acrux",
            t.text,
        );
        y += (title * 1.8) as i32;
        for line in [
            "Ctrl+O pour ouvrir un document, ou déposez un PDF sur la fenêtre.",
            "Ctrl+Maj+P ouvre la palette : toutes les commandes et leurs raccourcis.",
        ] {
            text.draw(
                frame,
                x as f32,
                y as f32 + text.ascent(size),
                size,
                line,
                t.text_dim,
            );
            y += (size * 1.9) as i32;
        }
        if !recent.is_empty() {
            y += (size * 1.4) as i32;
            text.draw(
                frame,
                x as f32,
                y as f32 + text.ascent(size),
                size,
                "Documents récents",
                t.text_dim,
            );
            y += (size * 2.2) as i32;
            let row = (size * 2.4) as i32;
            let width = (frame.width as i32 - 2 * x).max(120);
            for (name, dir) in &recent {
                if y + row > frame.height as i32 {
                    break;
                }
                let over = hover
                    .is_some_and(|(mx, my)| mx >= x && mx < x + width && my >= y && my < y + row);
                if over {
                    frame.fill_rect(x - 8, y, width + 16, row, t.hover.0, t.hover.1, t.hover.2);
                }
                let baseline = y as f32 + (row as f32 + text.ascent(size)) / 2.0 - 1.0;
                let used = text.draw(frame, x as f32, baseline, size, name, t.text);
                text.draw_clipped(
                    frame,
                    used + size,
                    baseline,
                    size * 0.92,
                    dir,
                    t.text_dim,
                    (x + width) as f32 - used - size,
                );
                hits.push((x - 8, y, width + 16, row));
                y += row;
            }
        }
        self.recent_hits = hits;
    }

    /// Ouvre le document récent situé sous `(x, y)`, le cas échéant.
    fn click_recent(&mut self, x: i32, y: i32, window: &mut dyn WindowHandle) -> bool {
        let Some(index) = self
            .recent_hits
            .iter()
            .position(|&(rx, ry, rw, rh)| x >= rx && x < rx + rw && y >= ry && y < ry + rh)
        else {
            return false;
        };
        let Some(path) = self.prefs.recent.get(index).cloned() else {
            return false;
        };
        if !path.exists() {
            self.prefs.recent.retain(|p| p != &path);
            self.prefs.save();
            window.show_error(
                "Fichier introuvable",
                &format!("{}\n\nIl a été retiré de la liste.", path.display()),
            );
            return true;
        }
        self.open(&path, window);
        true
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

    /// Dessine l'invite modale par-dessus la vue.
    fn paint_prompt(&mut self, frame: &mut Frame<'_>) {
        let Some(prompt) = &self.prompt else { return };
        let t = self.theme;
        let dpi = self.dpi_scale as f32;
        fill_rect_blend(
            frame,
            0,
            0,
            self.width as i32,
            self.height as i32,
            (0, 0, 0),
            120,
        );
        let panel_w = (420.0 * dpi) as i32;
        let panel_h = (150.0 * dpi) as i32;
        let x = (self.width as i32 - panel_w) / 2;
        let y = self.view_top() as i32 + (self.view_height() as i32 - panel_h) / 2;
        frame.fill_rect(
            x - 1,
            y - 1,
            panel_w + 2,
            panel_h + 2,
            t.separator.0,
            t.separator.1,
            t.separator.2,
        );
        frame.fill_rect(x, y, panel_w, panel_h, t.bar.0, t.bar.1, t.bar.2);
        let Some(text) = &mut self.text else { return };
        let size = t.font_size * dpi;
        let pad = (16.0 * dpi) as i32;
        text.draw(
            frame,
            (x + pad) as f32,
            (y + pad) as f32 + text.ascent(size * 1.15),
            size * 1.15,
            &prompt.title,
            t.text,
        );
        text.draw_clipped(
            frame,
            (x + pad) as f32,
            (y + pad) as f32 + size * 1.15 + (8.0 * dpi) + text.ascent(size),
            size,
            &prompt.label,
            t.text_dim,
            (panel_w - 2 * pad) as f32,
        );
        let box_h = (30.0 * dpi) as i32;
        let box_y = y + pad + (size * 1.15 + size + 20.0 * dpi) as i32;
        prompt.input.draw(
            frame,
            text,
            &t,
            dpi,
            x + pad,
            box_y,
            panel_w - 2 * pad,
            box_h,
        );
        let hint_y = (box_y + box_h) as f32 + (8.0 * dpi) + text.ascent(size);
        let (hint, color) = match &prompt.error {
            Some(e) => (e.as_str(), (0xE5, 0x53, 0x53)),
            None => ("Entrée pour valider, Échap pour annuler", t.text_dim),
        };
        text.draw(frame, (x + pad) as f32, hint_y, size, hint, color);
    }

    /// Événement clavier pendant une invite modale.
    #[allow(clippy::too_many_lines)] // une branche par sorte d'invite, à la suite
    fn prompt_key(&mut self, key: Key, window: &mut dyn WindowHandle) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        match prompt.input.key(key, false) {
            InputAction::Cancel => self.prompt = None,
            InputAction::Submit => {
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
                            self.finish_open(path, doc, pages, Some(pw), window);
                        } else {
                            prompt.error = Some("Mot de passe incorrect".to_string());
                            prompt.input.clear();
                        }
                    }
                    PromptKind::Field { name, kind } => {
                        let (name, kind) = (name.clone(), *kind);
                        self.prompt = None;
                        self.apply_edit(
                            EditOp::SetField {
                                name,
                                value: FieldValue::parse(kind, &value),
                            },
                            window,
                        );
                    }
                    PromptKind::SignText { page, rect } => {
                        let (page, rect) = (*page, *rect);
                        self.prompt = None;
                        if !value.trim().is_empty() {
                            // La hauteur du rectangle fixe le corps du texte ;
                            // la largeur s'ajuste au texte, proportions gardées.
                            let wide = Rect::new(
                                rect.x0,
                                rect.y0,
                                rect.x0 + rect.height() * 60.0,
                                rect.y1,
                            );
                            self.apply_fillsign(
                                page,
                                wide,
                                acrux_features::fillsign::Item::Text { text: value },
                                window,
                            );
                        }
                    }
                    PromptKind::Highlight { zones } => {
                        let zones = zones.clone();
                        self.prompt = None;
                        self.add_highlights(&zones, Some(&value), window);
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
                            self.apply_edit(
                                EditOp::EditText {
                                    page,
                                    line,
                                    start,
                                    end,
                                    text: value,
                                },
                                window,
                            );
                        }
                    }
                    PromptKind::Note { page, x, y } => {
                        let (page, x, y) = (*page, *x, *y);
                        self.prompt = None;
                        if !value.trim().is_empty() {
                            self.apply_edit(
                                EditOp::Annotate {
                                    page,
                                    annotation: NewAnnotation::Note {
                                        x,
                                        y,
                                        contents: value,
                                        color: [1.0, 0.85, 0.0],
                                    },
                                    author: author_name(),
                                },
                                window,
                            );
                        }
                    }
                }
            }
            InputAction::Changed | InputAction::None => {}
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
            window.show_error(
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
            window.show_error(
                "Modification impossible",
                "Sélectionnez du texte sur une seule ligne.",
            );
            return;
        };
        let current = text.text(from, to);
        let mut input = TextInput::new("texte");
        input.set_value(&current);
        self.prompt = Some(Prompt {
            title: "Modifier le texte".to_string(),
            label: "Nouveau texte :".to_string(),
            input,
            error: None,
            kind: PromptKind::EditText {
                page,
                line,
                start: first,
                end: last,
            },
        });
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

    /// Pose les surlignages ; le commentaire, s'il y en a un, va sur la
    /// première zone (c'est là que le lecteur clique).
    fn add_highlights(
        &mut self,
        zones: &[(usize, Rect)],
        comment: Option<&str>,
        window: &mut dyn WindowHandle,
    ) {
        for (index, (page, rect)) in zones.iter().enumerate() {
            let contents = if index == 0 {
                comment
                    .map(ToString::to_string)
                    .filter(|c| !c.trim().is_empty())
            } else {
                None
            };
            self.apply_edit(
                EditOp::Annotate {
                    page: *page,
                    annotation: NewAnnotation::Highlight {
                        rect: *rect,
                        color: [1.0, 1.0, 0.0],
                        contents,
                    },
                    author: author_name(),
                },
                window,
            );
        }
    }

    /// Demande un commentaire, puis surligne la sélection avec.
    fn highlight_with_comment(&mut self, window: &mut dyn WindowHandle) {
        let zones = self.selection_zones();
        if zones.is_empty() {
            return;
        }
        self.prompt = Some(Prompt {
            title: "Surligner et commenter".to_string(),
            label: "Commentaire :".to_string(),
            input: TextInput::new("votre remarque"),
            error: None,
            kind: PromptKind::Highlight { zones },
        });
        window.request_redraw();
    }

    /// Surligne la sélection courante (annotations `/Highlight`, une par ligne).
    fn highlight_selection(&mut self, window: &mut dyn WindowHandle) {
        let zones = self.selection_zones();
        self.add_highlights(&zones, None, window);
    }

    /// Marque la sélection pour biffure (annotations `/Redact`, visibles en
    /// cadre rouge et encore réversibles tant qu'elles ne sont pas appliquées).
    fn mark_redaction(&mut self, window: &mut dyn WindowHandle) {
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
        self.apply_edit(EditOp::Mark { marks }, window);
        self.selection = None;
        log_line(&format!("{count} zone(s) marquée(s) pour biffure"));
    }

    /// Applique définitivement les marques de biffure, après confirmation.
    fn apply_redactions(&mut self, window: &mut dyn WindowHandle) {
        if self.loaded.is_none() {
            return;
        }
        if !window.confirm(
            "Appliquer les biffures",
            "Le contenu couvert par les marques sera supprimé définitivement du document. Continuer ?",
        ) {
            return;
        }
        self.apply_edit(EditOp::ApplyRedactions, window);
        log_line("biffures appliquées");
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
            Some(page) => self.scroll_to_page(page),
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
                window.show_error("Extraction impossible", &format!("{e}"));
                return;
            }
        };
        let Some(found) = list.iter().find(|a| a.name == name) else {
            window.show_error(
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
            Err(e) => window.show_error("Extraction impossible", &format!("{name}\n\n{e}")),
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
                window.show_error("Lecture impossible", &format!("{}\n\n{e}", path.display()));
                return;
            }
        };
        let name = path.file_name().map_or_else(
            || "fichier".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let size = data.len();
        self.apply_edit(
            EditOp::Attach {
                name: name.clone(),
                data,
                description: None,
            },
            window,
        );
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
        if self.loaded.is_none() {
            return;
        }
        let Some(path) = window.open_file_dialog() else {
            return;
        };
        let at = self.current_page();
        self.apply_edit(
            EditOp::Insert {
                path,
                password: None,
                pages: Vec::new(),
                at,
            },
            window,
        );
        self.set_notice(format!("pages insérées avant la page {}", at + 1));
    }

    /// Duplique la page courante juste après elle. Un index répété dans
    /// l'ordre des pages suffit : `reorder_pages` en fait une copie indirecte
    /// distincte, ce qui marche aussi sur un document déjà modifié.
    fn duplicate_current(&mut self, window: &mut dyn WindowHandle) {
        let Some(l) = &self.loaded else { return };
        let count = l.pages.len();
        let page = self.current_page();
        let mut order: Vec<usize> = (0..count).collect();
        if page >= count {
            return;
        }
        order.insert(page + 1, page);
        self.apply_edit(EditOp::Reorder { order }, window);
        self.set_notice(format!("page {} dupliquée", page + 1));
    }

    /// Écrit la page courante dans un nouveau fichier.
    fn extract_current(&mut self, window: &mut dyn WindowHandle) {
        let Some(l) = &self.loaded else { return };
        let page = self.current_page();
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
            Err(e) => window.show_error("Extraction impossible", &format!("{e}")),
        }
    }

    /// Convertit le document : le format vient de l'extension choisie dans le
    /// dialogue. La conversion elle-même est faite par le fil de rendu — une
    /// centaine de pages en PNG prendrait plusieurs secondes.
    fn export(&mut self, window: &mut dyn WindowHandle) {
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
            window.show_error(
                "Format inconnu",
                "Choisissez une extension parmi html, docx, xlsx, md, txt, png ou jpg.",
            );
            return;
        };
        let Some(l) = &self.loaded else { return };
        let Some(worker) = &l.worker else {
            window.show_error(
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
        let tip = self.toolbar.hover_tip();
        let same = match (&self.tip, &tip) {
            (Some((a, _, _)), Some((b, _))) => a == b,
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
        // Sous le bouton, recalée dans la fenêtre si elle dépasse à droite.
        let x = (rect.0 + rect.2 / 2 - w / 2).clamp(4, (frame.width as i32 - w - 4).max(4));
        let y = (rect.1 + rect.3 + (4.0 * dpi) as i32).min(frame.height as i32 - h - 2);
        frame.fill_rect(x, y, w, h, t.tip_bg.0, t.tip_bg.1, t.tip_bg.2);
        frame.fill_rect(x, y, w, 1, t.separator.0, t.separator.1, t.separator.2);
        frame.fill_rect(
            x,
            y + h - 1,
            w,
            1,
            t.separator.0,
            t.separator.1,
            t.separator.2,
        );
        frame.fill_rect(x, y, 1, h, t.separator.0, t.separator.1, t.separator.2);
        frame.fill_rect(
            x + w - 1,
            y,
            1,
            h,
            t.separator.0,
            t.separator.1,
            t.separator.2,
        );
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
        self.prompt = Some(Prompt {
            title: "Nouvelle note".into(),
            label: format!("Texte de la note (page {}) :", page + 1),
            input: TextInput::new("Votre commentaire"),
            error: None,
            kind: PromptKind::Note {
                page,
                x: pt.x,
                y: pt.y,
            },
        });
    }

    /// Recalcule les occurrences si la requête a changé.
    fn update_search(&mut self) {
        let Some(s) = &mut self.search else { return };
        if s.input.value == s.last_query {
            return;
        }
        // Nouvelle requête : on repart de la première page.
        s.last_query.clone_from(&s.input.value);
        s.hits.clear();
        s.current = 0;
        s.scanned = 0;
        self.step_search();
    }

    /// Vrai s'il reste des pages à parcourir pour la recherche en cours.
    fn search_scanning(&self) -> bool {
        let count = self.loaded.as_ref().map_or(0, |l| l.pages.len());
        self.search.as_ref().is_some_and(|s| s.scanned < count)
    }

    /// Parcourt quelques pages de plus (budget de temps court, pour rendre la
    /// main à la boucle d'événements) ; vrai s'il en reste.
    fn step_search(&mut self) -> bool {
        let count = self.loaded.as_ref().map_or(0, |l| l.pages.len());
        {
            let Some(s) = &mut self.search else {
                return false;
            };
            if s.scanned >= count {
                return false;
            }
            if s.input.value.trim().is_empty() {
                s.scanned = count;
                return false;
            }
        }
        let had_hits = self.search.as_ref().is_some_and(|s| !s.hits.is_empty());
        let deadline = Instant::now() + Duration::from_millis(12);
        {
            let Some(l) = &mut self.loaded else {
                return false;
            };
            let Some(s) = &mut self.search else {
                return false;
            };
            while s.scanned < count {
                let page = s.scanned;
                let text = &l.text(page).0;
                for r in find(text, &s.input.value) {
                    s.hits.push((page, r));
                }
                s.scanned += 1;
                if Instant::now() >= deadline {
                    break;
                }
            }
        }
        // Dès que la première occurrence apparaît, on s'y rend.
        if !had_hits && self.search.as_ref().is_some_and(|s| !s.hits.is_empty()) {
            self.scroll_to_hit();
        }
        self.search_scanning()
    }

    /// Fait défiler jusqu'à l'occurrence courante.
    #[allow(clippy::many_single_char_names)] // coordonnées et matrices
    fn scroll_to_hit(&mut self) {
        let Some(s) = &self.search else { return };
        let Some(&(page, rect)) = s.hits.get(s.current) else {
            return;
        };
        if self.view_mode.is_paged() {
            self.anchor = page;
        }
        let layout = self.layout();
        let Some(&PageBox { y, w, h, .. }) = layout.get(page) else {
            return;
        };
        let Some(l) = &self.loaded else { return };
        let p = &l.pages[page];
        let m = base_matrix(&p.crop_box(&l.doc), self.scale(), p.rotate(&l.doc), w, h);
        let dev = m.transform_rect(&rect);
        let target = f64::from(y) + dev.y0 - f64::from(self.view_height()) / 2.0;
        self.scroll_y = target;
        self.clamp_scroll();
    }

    /// Dessine les surlignages des occurrences et le champ de recherche.
    #[allow(clippy::many_single_char_names)] // coordonnées et matrices
    fn paint_search(&mut self, frame: &mut Frame<'_>) {
        let Some(s) = &self.search else { return };
        let t = self.theme;
        let dpi = self.dpi_scale as f32;
        let layout = self.layout();
        let view_h = self.view_height() as i32;
        if let Some(l) = &self.loaded {
            let scale = self.scale();
            for (idx, (page, rect)) in s.hits.iter().enumerate() {
                let Some(&PageBox { w, h, .. }) = layout.get(*page) else {
                    continue;
                };
                let Some((x0, top)) = self.page_screen(&layout, *page) else {
                    continue;
                };
                let p = &l.pages[*page];
                let m = base_matrix(&p.crop_box(&l.doc), scale, p.rotate(&l.doc), w, h);
                let dev = m.transform_rect(rect);
                let sx = (x0 + dev.x0).round() as i32;
                let sy = (top + dev.y0).round() as i32;
                let sw = dev.width().round().max(2.0) as i32;
                let sh = dev.height().round().max(2.0) as i32;
                if sy + sh < 0 || sy > view_h {
                    continue;
                }
                let color = if idx == s.current {
                    (255, 140, 0)
                } else {
                    (255, 230, 0)
                };
                fill_rect_blend(frame, sx, sy, sw, sh.min(view_h - sy), color, 110);
            }
        }
        // Champ en haut à droite, avec le compteur d'occurrences.
        let Some(text) = &mut self.text else { return };
        let box_w = (320.0 * dpi) as i32;
        let box_h = (30.0 * dpi) as i32;
        let margin = (12.0 * dpi) as i32;
        let x = frame.width as i32 - box_w - margin;
        let y = margin;
        frame.fill_rect(
            x - 2,
            y - 2,
            box_w + 4,
            box_h + 4,
            t.bar.0,
            t.bar.1,
            t.bar.2,
        );
        s.input.draw(frame, text, &t, dpi, x, y, box_w, box_h);
        let scanning = s.scanned < self.loaded.as_ref().map_or(0, |l| l.pages.len());
        let counter = if s.input.value.is_empty() {
            String::new()
        } else if scanning {
            format!("{} trouvée(s), recherche…", s.hits.len())
        } else if s.hits.is_empty() {
            "aucun résultat".to_string()
        } else {
            format!("{} / {}", s.current + 1, s.hits.len())
        };
        if !counter.is_empty() {
            let size = t.font_size * dpi;
            let cw = text.measure(size, &counter);
            let baseline = y as f32 + (box_h as f32 + text.ascent(size)) / 2.0 - 1.0;
            frame.fill_rect(
                x - 2,
                y + box_h + 2,
                box_w + 4,
                box_h,
                t.bar.0,
                t.bar.1,
                t.bar.2,
            );
            text.draw(
                frame,
                x as f32 + box_w as f32 - cw - 8.0 * dpi,
                baseline + box_h as f32 + 2.0,
                size,
                &counter,
                t.text_dim,
            );
        }
    }

    fn open(&mut self, path: &Path, window: &mut dyn WindowHandle) {
        match Document::load(path).and_then(|doc| {
            let pages = collect_pages(&doc)?;
            Ok((doc, pages))
        }) {
            Ok((doc, pages)) => {
                if doc.needs_password() {
                    let mut input = TextInput::new("Mot de passe");
                    input.masked = true;
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.prompt = Some(Prompt {
                        title: "Document protégé".into(),
                        label: format!("Mot de passe pour « {name} » :"),
                        input,
                        error: None,
                        kind: PromptKind::Password {
                            path: path.to_path_buf(),
                            doc,
                            pages,
                        },
                    });
                } else {
                    self.finish_open(path.to_path_buf(), doc, pages, None, window);
                }
            }
            Err(e) => {
                self.error = Some(format!("{e}"));
                window.show_error(
                    "Ouverture impossible",
                    &format!("{}\n\n{e}", path.display()),
                );
            }
        }
        self.title_dirty = true;
        window.request_redraw();
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
        let page_index = PageIndex::new(&pages);
        let fields = list_fields(&doc).unwrap_or_default();
        let comments = collect_comments(&doc, &pages);
        let layers: Vec<(u32, String, bool)> = acrux_render::layers(&doc)
            .into_iter()
            .map(|l| (l.number, l.name, l.visible))
            .collect();
        let attachments = collect_attachments(&doc);
        let media = acrux_features::media::list(&doc).unwrap_or_default();
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
        // Un document déjà ouvert passe en arrière-plan : nouvel onglet.
        if let Some(previous) = self.loaded.take() {
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
            modified: false,
            fields,
            comments,
            layers,
            attachments,
            media,
            labels,
        });
        self.error = None;
        self.scroll_y = 0.0;
        self.scroll_x = 0.0;
        self.selection = None;
        self.search = None;
        self.focus_field = None;
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
            + self.sign_height()
    }

    /// Hauteur de la barre « remplir et signer », nulle quand l'outil dort.
    fn sign_height(&self) -> u32 {
        if self.sign_bar.is_some() && !self.fullscreen && !self.reading {
            SignBar::height(self.dpi_scale as f32).max(0) as u32
        } else {
            0
        }
    }

    /// Bascule le plein écran (F11 ; Échap pour sortir).
    fn toggle_fullscreen(&mut self, window: &mut dyn WindowHandle) {
        self.fullscreen = !self.fullscreen;
        if self.fullscreen {
            self.panel_open = false;
        }
        window.set_fullscreen(self.fullscreen);
        self.clamp_scroll();
    }

    /// Champs de formulaire dans l'ordre de tabulation (page, haut → bas, gauche → droite).
    fn tab_order(&self) -> Vec<usize> {
        let Some(l) = &self.loaded else {
            return Vec::new();
        };
        let mut order: Vec<(usize, i64, i64, usize)> = l
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                !f.flags.read_only
                    && !matches!(
                        f.kind,
                        FieldType::Button | FieldType::Signature | FieldType::Unknown
                    )
            })
            .filter_map(|(i, f)| {
                let w = f.widgets.first()?;
                let page = w.page?;
                Some((
                    page,
                    -(w.rect.y1 * 10.0) as i64,
                    (w.rect.x0 * 10.0) as i64,
                    i,
                ))
            })
            .collect();
        order.sort_unstable();
        order.into_iter().map(|(_, _, _, i)| i).collect()
    }

    /// Donne le focus au champ suivant (`forward`) ou précédent, et le montre.
    fn focus_next_field(&mut self, forward: bool) {
        let order = self.tab_order();
        if order.is_empty() {
            return;
        }
        let pos = self
            .focus_field
            .and_then(|f| order.iter().position(|&i| i == f));
        let next = match (pos, forward) {
            (None, true) => 0,
            (None, false) => order.len() - 1,
            (Some(p), true) => (p + 1) % order.len(),
            (Some(p), false) => (p + order.len() - 1) % order.len(),
        };
        let fi = order[next];
        self.focus_field = Some(fi);
        let target = self.loaded.as_ref().and_then(|l| {
            let w = l.fields.get(fi)?.widgets.first()?;
            Some((w.page?, w.rect))
        });
        if let Some((page, rect)) = target {
            // Montre le champ si son haut ou son bas sort de la vue.
            let layout = self.layout();
            if let (Some(&PageBox { y, w, h, .. }), Some(l)) = (layout.get(page), &self.loaded) {
                let p = &l.pages[page];
                let m = base_matrix(&p.crop_box(&l.doc), self.scale(), p.rotate(&l.doc), w, h);
                let dev = m.transform_rect(&rect);
                let top = f64::from(y) + dev.y0;
                let bottom = f64::from(y) + dev.y1;
                let vh = f64::from(self.view_height());
                if top < self.scroll_y || bottom > self.scroll_y + vh {
                    self.scroll_y = (top - vh / 3.0).max(0.0);
                    self.clamp_scroll();
                }
            }
        }
    }

    /// Active le champ ayant le focus (Entrée / Espace).
    fn activate_focused_field(&mut self, window: &mut dyn WindowHandle) {
        if let Some(fi) = self.focus_field {
            self.click_widget(fi, 0, window);
        }
    }

    /// Cadre le champ de formulaire ayant le focus.
    #[allow(clippy::many_single_char_names)] // coordonnées et matrices
    fn paint_field_focus(&mut self, frame: &mut Frame<'_>) {
        let Some(fi) = self.focus_field else { return };
        let accent = self.theme.accent;
        let layout = self.layout();
        let scale = self.scale();
        let origins: Vec<Option<(f64, f64)>> = (0..layout.len())
            .map(|i| self.page_screen(&layout, i))
            .collect();
        let Some(l) = &self.loaded else { return };
        let Some(field) = l.fields.get(fi) else {
            return;
        };
        for w in &field.widgets {
            let Some(page) = w.page else { continue };
            let Some(&PageBox { w: pw, h: ph, .. }) = layout.get(page) else {
                continue;
            };
            let Some(Some((ox, top))) = origins.get(page).copied() else {
                continue;
            };
            let p = &l.pages[page];
            let m = base_matrix(&p.crop_box(&l.doc), scale, p.rotate(&l.doc), pw, ph);
            let dev = m.transform_rect(&w.rect);
            let x0 = (ox + dev.x0).round() as i32 - 2;
            let y0 = (top + dev.y0).round() as i32 - 2;
            let bw = dev.width().round() as i32 + 4;
            let bh = dev.height().round() as i32 + 4;
            let t = (2.0 * self.dpi_scale).round().max(1.0) as i32;
            frame.fill_rect(x0, y0, bw, t, accent.0, accent.1, accent.2);
            frame.fill_rect(x0, y0 + bh - t, bw, t, accent.0, accent.1, accent.2);
            frame.fill_rect(x0, y0, t, bh, accent.0, accent.1, accent.2);
            frame.fill_rect(x0 + bw - t, y0, t, bh, accent.0, accent.1, accent.2);
        }
    }

    /// Bord gauche de la zone de document (à droite du panneau latéral).
    fn view_left(&self) -> u32 {
        if self.panel_open && !self.reading {
            (f64::from(PANEL_WIDTH) * self.dpi_scale).round() as u32
        } else {
            0
        }
    }

    /// Largeur de la zone de document.
    fn view_width(&self) -> u32 {
        self.width.saturating_sub(self.view_left())
    }

    /// Hauteur de la zone de document.
    fn view_height(&self) -> u32 {
        self.height
            .saturating_sub(self.status_height())
            .saturating_sub(self.view_top())
    }

    /// Ce que la barre d'outils affiche.
    fn toolbar_info(&self) -> ToolbarInfo {
        let page = self.current_page();
        ToolbarInfo {
            page: page + 1,
            page_count: self.loaded.as_ref().map_or(0, |l| l.pages.len()),
            page_label: self
                .loaded
                .as_ref()
                .and_then(|l| l.labels.get(page))
                .cloned(),
            zoom_percent: (self.effective_zoom() * 100.0).round() as u32,
            has_document: self.loaded.is_some(),
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
                match p.rotate(&l.doc) {
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
            .map(|p| page_pixel_size(&l.doc, p, scale))
            .collect();
        (scale, key, sizes)
    }

    fn toggle_panel(&mut self) {
        self.panel_open = !self.panel_open;
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
            PanelAction::GoToPage(p) => self.scroll_to_page(p),
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
                self.apply_edit(EditOp::Reorder { order }, window);
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
                    .map(|c| c.page);
                if let Some(page) = target {
                    self.scroll_to_page(page);
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
    fn run_command(&mut self, command: Command, window: &mut dyn WindowHandle) {
        log_line(&format!("palette : {command:?}"));
        match command {
            Command::Open => {
                if self.confirm_discard(window) {
                    if let Some(p) = window.open_file_dialog() {
                        self.open(&p, window);
                    }
                }
            }
            Command::Save => self.save(false, window),
            Command::SaveAs => self.save(true, window),
            Command::Print => self.print(window),
            Command::Export => self.export(window),
            Command::CloseTab => {
                let active = self.active_tab;
                self.close_tab(active, window);
            }
            Command::NextTab => self.cycle_tab(true),
            Command::PrevPage => self.step_row(false),
            Command::NextPage => self.step_row(true),
            Command::FirstPage => self.scroll_to_page(0),
            Command::LastPage => {
                let last = self
                    .loaded
                    .as_ref()
                    .map_or(0, |l| l.pages.len().saturating_sub(1));
                self.scroll_to_page(last);
            }
            Command::ZoomIn => self.zoom_step(1),
            Command::ZoomOut => self.zoom_step(-1),
            Command::ZoomReset => self.set_zoom(1.0),
            Command::FitWidth => {
                self.fit_width = true;
                self.clamp_scroll();
            }
            Command::CycleViewMode => self.set_view_mode(self.view_mode.next()),
            Command::Fullscreen => self.toggle_fullscreen(window),
            Command::TogglePanel => self.toggle_panel(),
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
            Command::ToggleTheme => self.toggle_theme(),
            Command::Search => self.open_search(),
            Command::Copy => {
                let text = self.selected_text();
                if !text.is_empty() {
                    window.set_clipboard_text(&text);
                }
            }
            Command::SelectAll => self.select_all(),
            Command::RotateRight => self.rotate_current(90, window),
            Command::RotateLeft => self.rotate_current(-90, window),
            Command::DeletePage => self.delete_current(window),
            Command::InsertPages => self.insert_pages(window),
            Command::DuplicatePage => self.duplicate_current(window),
            Command::ExtractPage => self.extract_current(window),
            Command::Undo => self.undo(window),
            Command::Redo => self.redo_edit(window),
            Command::EditText => self.start_text_edit(window),
            Command::Highlight => self.highlight_selection(window),
            Command::Note => self.start_note(),
            Command::CheckUpdates => self.install_update(window),
            Command::EditObjects => self.toggle_objects(window),
            Command::FillSign => self.toggle_fillsign(window),
            Command::MarkRedaction => self.mark_redaction(window),
            Command::ApplyRedactions => self.apply_redactions(window),
        }
    }

    /// Applique une modification au document (et à la copie du fil de rendu).
    fn apply_edit(&mut self, op: EditOp, window: &mut dyn WindowHandle) {
        let Some(l) = &mut self.loaded else { return };
        if let Err(e) = op.apply(&l.doc) {
            window.show_error("Modification impossible", &format!("{e}"));
            return;
        }
        if let Some(w) = &mut l.worker {
            w.edit(op.clone());
        }
        l.history.push(op);
        l.redo.clear();
        match collect_pages(&l.doc) {
            Ok(p) => l.pages = p,
            Err(e) => {
                window.show_error("Modification impossible", &format!("{e}"));
                return;
            }
        }
        l.page_index = PageIndex::new(&l.pages);
        l.cache.clear();
        l.texts.clear();
        l.links.clear();
        l.fields = list_fields(&l.doc).unwrap_or_default();
        l.comments = collect_comments(&l.doc, &l.pages);
        l.attachments = collect_attachments(&l.doc);
        l.labels = collect_labels(&l.doc);
        l.modified = true;
        self.selection = None;
        self.search = None;
        self.title_dirty = true;
        self.clamp_scroll();
    }

    /// Reconstruit le document depuis le fichier et rejoue `ops`. C'est la
    /// base de l'annulation : les modifications ne sont pas inversées une à
    /// une (elles ne sont pas toutes inversibles), le document est rechargé
    /// puis l'historique conservé est réappliqué.
    fn replay(&mut self, ops: Vec<EditOp>, redo: Vec<EditOp>, window: &mut dyn WindowHandle) {
        let Some(l) = &self.loaded else { return };
        let (path, password) = (l.path.clone(), l.password.clone());
        let (scroll_x, scroll_y, anchor) = (self.scroll_x, self.scroll_y, self.anchor);
        let panel_tab = self.panel.tab;
        let doc = match Document::load(&path) {
            Ok(d) => d,
            Err(e) => {
                window.show_error("Rechargement impossible", &format!("{e}"));
                return;
            }
        };
        if let Some(pw) = &password {
            let _ = doc.authenticate(pw);
        }
        for op in &ops {
            if let Err(e) = op.apply(&doc) {
                window.show_error("Rejeu impossible", &format!("{e}"));
                return;
            }
        }
        let pages = match collect_pages(&doc) {
            Ok(p) => p,
            Err(e) => {
                window.show_error("Rechargement impossible", &format!("{e}"));
                return;
            }
        };
        // Le document rechargé **remplace** celui de l'onglet courant : sans
        // cela, `finish_open` le rangerait en arrière-plan et ouvrirait un
        // onglet de plus à chaque annulation.
        self.loaded = None;
        self.finish_open(path, doc, pages, password, window);
        if let Some(l) = &mut self.loaded {
            // Le fil de rendu repart du fichier : on lui rejoue l'historique.
            if let Some(w) = &mut l.worker {
                for op in &ops {
                    w.edit(op.clone());
                }
            }
            l.modified = !ops.is_empty();
            l.history = ops;
            l.redo = redo;
        }
        // L'utilisateur ne doit pas perdre sa place en annulant.
        self.anchor = anchor;
        self.scroll_x = scroll_x;
        self.scroll_y = scroll_y;
        self.panel.tab = panel_tab;
        self.clamp_scroll();
        self.title_dirty = true;
    }

    /// Annule la dernière modification.
    fn undo(&mut self, window: &mut dyn WindowHandle) {
        let Some(l) = &mut self.loaded else { return };
        let Some(op) = l.history.pop() else { return };
        let ops = l.history.clone();
        let mut redo = l.redo.clone();
        redo.push(op);
        log_line(&format!("annulation : {} restante(s)", ops.len()));
        self.replay(ops, redo, window);
    }

    /// Rétablit la dernière modification annulée.
    fn redo_edit(&mut self, window: &mut dyn WindowHandle) {
        let Some(l) = &mut self.loaded else { return };
        let Some(op) = l.redo.pop() else { return };
        let redo = l.redo.clone();
        let mut ops = l.history.clone();
        ops.push(op);
        log_line(&format!("rétablissement : {} modification(s)", ops.len()));
        self.replay(ops, redo, window);
    }

    /// Pivote la page courante.
    fn rotate_current(&mut self, degrees: i32, window: &mut dyn WindowHandle) {
        if self.loaded.is_none() {
            return;
        }
        let page = self.current_page();
        self.apply_edit(
            EditOp::Rotate {
                pages: vec![page],
                degrees,
            },
            window,
        );
    }

    /// Supprime la page courante après confirmation.
    fn delete_current(&mut self, window: &mut dyn WindowHandle) {
        let Some(l) = &self.loaded else { return };
        if l.pages.len() <= 1 {
            window.show_error(
                "Suppression impossible",
                "Un document doit garder au moins une page.",
            );
            return;
        }
        let page = self.current_page();
        if !window.confirm(
            "Supprimer la page",
            &format!(
                "Supprimer la page {} ? (Ctrl+S pour enregistrer ensuite)",
                page + 1
            ),
        ) {
            return;
        }
        self.apply_edit(EditOp::Delete { pages: vec![page] }, window);
    }

    /// Enregistre (`save_as` : demande un nouveau chemin et réécrit tout).
    fn save(&mut self, save_as: bool, window: &mut dyn WindowHandle) {
        let Some(l) = &self.loaded else { return };
        let suggested = l.path.file_name().map_or_else(
            || "document.pdf".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let target = if save_as {
            let Some(p) = window.save_file_dialog(&suggested) else {
                return;
            };
            p
        } else {
            l.path.clone()
        };
        // Enregistrer sous ou document réparé : réécriture complète ; sinon ajout incrémental.
        let bytes = if save_as || l.doc.was_repaired() {
            l.doc.save_full()
        } else {
            l.doc.save_incremental()
        };
        let bytes = match bytes {
            Ok(b) => b,
            Err(e) => {
                window.show_error("Enregistrement impossible", &format!("{e}"));
                return;
            }
        };
        // Écriture dans un fichier temporaire puis remplacement : jamais de fichier à moitié écrit.
        let tmp = target.with_extension("pdf.tmp");
        let written = std::fs::write(&tmp, &bytes).and_then(|()| std::fs::rename(&tmp, &target));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            window.show_error(
                "Enregistrement impossible",
                &format!("{}\n\n{e}", target.display()),
            );
            return;
        }
        log_line(&format!(
            "enregistré : {} ({} octets)",
            target.display(),
            bytes.len()
        ));
        if let Some(l) = &mut self.loaded {
            l.path = target;
            l.modified = false;
            // Le fichier enregistré devient la nouvelle base de l'annulation :
            // rejouer l'historique par-dessus le ferait une deuxième fois.
            l.history.clear();
            l.redo.clear();
        }
        self.title_dirty = true;
    }

    /// Imprime le document (dialogue système).
    fn print(&mut self, window: &mut dyn WindowHandle) {
        let Some(l) = &self.loaded else { return };
        let title = l
            .path
            .file_name()
            .map_or_else(|| "Acrux".to_string(), |n| n.to_string_lossy().into_owned());
        let mut source = PrintPages {
            doc: &l.doc,
            pages: &l.pages,
        };
        match window.print(&title, &mut source) {
            PrintOutcome::Printed(n) => log_line(&format!("imprimé : {n} page(s)")),
            PrintOutcome::Cancelled => {}
            PrintOutcome::Failed(m) => window.show_error("Impression impossible", &m),
        }
    }

    /// Vrai s'il n'y a rien à perdre, ou si l'utilisateur accepte de perdre
    /// les modifications non enregistrées.
    fn confirm_discard(&mut self, window: &mut dyn WindowHandle) -> bool {
        match &self.loaded {
            Some(l) if l.modified => window.confirm(
                "Modifications non enregistrées",
                "Le document a été modifié. Abandonner les modifications ?",
            ),
            _ => true,
        }
    }

    /// Comme [`Self::confirm_discard`], mais pour **tous** les onglets : à la
    /// fermeture de la fenêtre, un onglet modifié en arrière-plan ne doit pas
    /// disparaître en silence.
    fn confirm_discard_all(&mut self, window: &mut dyn WindowHandle) -> bool {
        let others = self.others.iter().filter(|l| l.modified).count();
        let active = self.loaded.as_ref().is_some_and(|l| l.modified);
        if others == 0 {
            return !active || self.confirm_discard(window);
        }
        let total = others + usize::from(active);
        window.confirm(
            "Modifications non enregistrées",
            &format!("{total} document(s) ouverts ont été modifiés. Quitter sans enregistrer ?"),
        )
    }

    /// Ouvre le champ de recherche.
    fn open_search(&mut self) {
        if self.loaded.is_some() {
            self.search = Some(Search {
                input: TextInput::new("Rechercher dans le document"),
                hits: Vec::new(),
                current: 0,
                last_query: String::new(),
                scanned: 0,
            });
        }
    }

    fn toggle_theme(&mut self) {
        self.theme = if self.theme.canvas == Theme::dark().canvas {
            Theme::light()
        } else {
            Theme::dark()
        };
        self.save_prefs();
        log_line(&format!("thème basculé : canvas {:?}", self.theme.canvas));
    }

    /// Exécute une action de la barre d'outils.
    fn tool_action(&mut self, action: ToolAction, window: &mut dyn WindowHandle) {
        match action {
            ToolAction::Open => {
                if self.confirm_discard(window) {
                    if let Some(p) = window.open_file_dialog() {
                        self.open(&p, window);
                    }
                }
            }
            ToolAction::PrevPage => self.step_row(false),
            ToolAction::NextPage => self.step_row(true),
            ToolAction::GoToPage(n) => {
                let count = self.loaded.as_ref().map_or(0, |l| l.pages.len());
                if count > 0 {
                    self.scroll_to_page((n - 1).min(count - 1));
                }
            }
            ToolAction::GoToLabel(text) => self.go_to_label(&text),
            ToolAction::ZoomOut => self.zoom_step(-1),
            ToolAction::ZoomIn => self.zoom_step(1),
            ToolAction::FitWidth => {
                self.fit_width = true;
                self.clamp_scroll();
            }
            ToolAction::Search => self.open_search(),
            ToolAction::ToggleTheme => self.toggle_theme(),
            ToolAction::TogglePanel => self.toggle_panel(),
            ToolAction::RotatePage => self.rotate_current(90, window),
            ToolAction::Save => self.save(false, window),
            ToolAction::Print => self.print(window),
            ToolAction::CycleViewMode => self.set_view_mode(self.view_mode.next()),
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
    fn scale(&self) -> f64 {
        let base = self.zoom * self.dpi_scale * (96.0 / 72.0);
        if self.fit_width {
            if let Some(l) = &self.loaded {
                let max_w = l
                    .pages
                    .iter()
                    .take(50)
                    .map(|p| {
                        let b = p.crop_box(&l.doc);
                        match p.rotate(&l.doc) {
                            90 | 270 => b.height(),
                            _ => b.width(),
                        }
                    })
                    .fold(1.0_f64, f64::max);
                // En deux pages, la largeur disponible se partage entre les
                // deux colonnes et la gouttière qui les sépare.
                let cols = if self.view_mode.is_two_up() { 2.0 } else { 1.0 };
                let gaps = f64::from(GAP) * (cols + 1.0);
                let avail = (f64::from(self.view_width()).max(64.0) - gaps).max(32.0);
                return (avail / (max_w * cols)).max(0.05);
            }
        }
        base
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
            .map(|p| page_pixel_size(&l.doc, p, scale))
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

    /// Nombre d'onglets ouverts.
    fn tab_count(&self) -> usize {
        self.others.len() + usize::from(self.loaded.is_some())
    }

    /// Titres des onglets, dans l'ordre d'affichage.
    fn tab_infos(&self) -> Vec<TabInfo> {
        let name = |l: &Loaded| TabInfo {
            title: l.path.file_name().map_or_else(
                || "document".to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
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
        if index == self.active_tab || index >= self.tab_count() {
            return;
        }
        let Some(current) = self.loaded.take() else {
            return;
        };
        let at = self.active_tab.min(self.others.len());
        self.others.insert(at, current);
        // `index` désigne une position dans la liste complète, qui est
        // maintenant exactement `others`.
        self.loaded = Some(self.others.remove(index));
        self.active_tab = index;
        self.reset_view_state();
    }

    /// Ferme un onglet (avec confirmation s'il a des modifications).
    fn close_tab(&mut self, index: usize, window: &mut dyn WindowHandle) {
        if index >= self.tab_count() {
            return;
        }
        if index == self.active_tab {
            if !self.confirm_discard(window) {
                return;
            }
            self.loaded = None;
            if self.others.is_empty() {
                self.active_tab = 0;
                self.reset_view_state();
                self.title_dirty = true;
                return;
            }
            let next = self.active_tab.min(self.others.len() - 1);
            self.loaded = Some(self.others.remove(next));
            self.active_tab = next;
        } else {
            // Position dans `others` : l'onglet actif n'y est pas encore.
            let at = if index > self.active_tab {
                index - 1
            } else {
                index
            };
            if at >= self.others.len() {
                return;
            }
            if self.others[at].modified
                && !window.confirm(
                    "Modifications non enregistrées",
                    "Cet onglet a été modifié. Le fermer sans enregistrer ?",
                )
            {
                return;
            }
            self.others.remove(at);
            if index < self.active_tab {
                self.active_tab -= 1;
            }
            self.title_dirty = true;
            return;
        }
        self.reset_view_state();
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
        self.scroll_x = 0.0;
        self.scroll_y = 0.0;
        self.anchor = 0;
        self.selection = None;
        self.search = None;
        self.focus_field = None;
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
            Some(Source::Embedded { .. }) => {}
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
        if let Some(frame) = view.player.frame() {
            let (w, h) = (frame.width, frame.height);
            // L'image garde ses proportions dans le rectangle de l'annotation,
            // comme le ferait un lecteur vidéo — une vidéo étirée se voit.
            let fitted = fit_box(area, w, h);
            video_ui::paint_frame(view_frame, fitted, &frame.bgra, w, h);
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
        if !window.confirm(
            &format!("Installer Acrux {} ?", release.version),
            &format!(
                "L'installateur sera téléchargé depuis :\n{url}\n\n\
                 Puis lancé pour remplacer la version en place. Acrux devra \
                 être relancé ensuite.",
            ),
        ) {
            return;
        }
        self.set_notice("téléchargement de la mise à jour…".into());
        match crate::update::download(&url, &release.version) {
            Ok(path) => {
                self.save_prefs();
                match std::process::Command::new(&path).arg("--silent").spawn() {
                    Ok(_) => self.set_notice(format!(
                        "Acrux {} s'installe — relancez l'application pour en profiter",
                        release.version
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
        if self.objects.is_some() {
            self.objects = None;
            self.set_notice("modification des objets : terminé".into());
        } else if self.loaded.is_some() {
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
        let m = base_matrix(&p.crop_box(&l.doc), self.scale(), p.rotate(&l.doc), w, h);
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
    fn objects_mouse_up(&mut self, window: &mut dyn WindowHandle) {
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
        self.apply_object_edit(page, vec![ObjectEdit::Transform { index, matrix }], window);
    }

    /// Envoie des modifications d'objets et recharge l'inventaire.
    fn apply_object_edit(
        &mut self,
        page: usize,
        edits: Vec<ObjectEdit>,
        window: &mut dyn WindowHandle,
    ) {
        if edits.is_empty() {
            return;
        }
        self.apply_edit(EditOp::EditObject { page, edits }, window);
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
    fn objects_key(&mut self, key: Key, window: &mut dyn WindowHandle) -> bool {
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
        self.apply_object_edit(page, vec![edit], window);
        if removing {
            if let Some(t) = &mut self.objects {
                t.selected = None;
            }
        }
        true
    }

    /// Caractère tapé quand l'outil est actif : l'ordre de superposition.
    fn objects_char(&mut self, c: char, window: &mut dyn WindowHandle) -> bool {
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
        self.apply_object_edit(page, vec![ObjectEdit::Arrange { index, to }], window);
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
    /// À la première ouverture, si aucune signature n'est enregistrée, la
    /// fenêtre de capture s'ouvre aussitôt : c'est le geste attendu, on ne
    /// choisit pas « Signature » avant d'en avoir une.
    fn toggle_fillsign(&mut self, window: &mut dyn WindowHandle) {
        if self.loaded.is_none() {
            return;
        }
        if self.sign_bar.is_some() {
            self.sign_bar = None;
            self.capture = None;
            self.set_notice("remplir et signer : terminé".into());
        } else {
            self.sign_bar = Some(SignBar::default());
            if self.signature.is_none() {
                self.capture = Some(Capture::new(false));
            } else {
                self.pick_sign_item(SignItem::Signature);
            }
        }
        self.clamp_scroll();
        window.request_redraw();
    }

    /// Choisit ce qui sera posé au prochain clic.
    fn pick_sign_item(&mut self, item: SignItem) {
        let missing = match item {
            SignItem::Signature => self.signature.is_none(),
            SignItem::Initials => self.initials.is_none(),
            _ => false,
        };
        if missing {
            self.capture = Some(Capture::new(item == SignItem::Initials));
            return;
        }
        if let Some(bar) = &mut self.sign_bar {
            bar.item = Some(item);
        }
        self.set_notice(format!("{} : cliquez sur la page", item.label()));
    }

    /// Suite d'une action venue de la barre ou de la fenêtre de capture.
    fn sign_action(&mut self, action: sign::Action, window: &mut dyn WindowHandle) {
        match action {
            sign::Action::None => return,
            sign::Action::Redraw => {}
            sign::Action::Pick(item) => self.pick_sign_item(item),
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
                if self.sign_bar.is_none() {
                    self.sign_bar = Some(SignBar::default());
                }
                self.pick_sign_item(item);
            }
            sign::Action::Close => {
                if self.capture.is_some() {
                    self.capture = None;
                } else {
                    self.sign_bar = None;
                    self.set_notice("remplir et signer : terminé".into());
                }
                self.clamp_scroll();
            }
        }
        window.request_redraw();
    }

    /// Enregistre une signature et la conserve pour les sessions suivantes.
    fn remember_signature(&mut self, initials: bool, saved: Saved) {
        let encoded = saved.encode();
        if initials {
            self.initials = Some(saved);
            self.prefs.initials = Some(encoded);
        } else {
            self.signature = Some(saved);
            self.prefs.signature = Some(encoded);
        }
        self.prefs.save();
    }

    /// Pose l'élément choisi à l'endroit cliqué dans la page.
    ///
    /// Les coordonnées sont celles de la vue ; le point cliqué devient le coin
    /// **supérieur gauche** de l'élément, comme dans Acrobat, sauf pour les
    /// marques qui se centrent sur le curseur — on vise une case à cocher.
    #[allow(clippy::many_single_char_names)] // coordonnées et dimensions
    fn place_sign(&mut self, x: i32, y: i32, window: &mut dyn WindowHandle) -> bool {
        let Some(item) = self.sign_bar.as_ref().and_then(|b| b.item) else {
            return false;
        };
        let Some((page, point)) = self.page_at(x, y) else {
            return false;
        };
        let (w, h) = item.default_size();
        let rect = if matches!(item, SignItem::Mark(_)) {
            Rect::new(
                point.x - w / 2.0,
                point.y - h / 2.0,
                point.x + w / 2.0,
                point.y + h / 2.0,
            )
        } else {
            Rect::new(point.x, point.y - h, point.x + w, point.y)
        };
        let source = match item {
            SignItem::Signature => self.signature.clone(),
            SignItem::Initials => self.initials.clone(),
            _ => None,
        };
        let fill_item = match item {
            SignItem::Mark(mark) => acrux_features::fillsign::Item::Mark(mark),
            SignItem::Text => {
                self.start_sign_text(page, rect);
                return true;
            }
            _ => match source {
                Some(Saved::Drawn(strokes)) => {
                    // La plume suit la taille du tracé : le même geste donne
                    // le même trait, qu'il ait été fait dans une petite ou
                    // une grande fenêtre.
                    let pen = acrux_features::fillsign::Pen::for_strokes(&strokes);
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
        self.apply_fillsign(page, rect, fill_item, window);
        true
    }

    /// Envoie la pose au fil de rendu.
    fn apply_fillsign(
        &mut self,
        page: usize,
        rect: Rect,
        item: acrux_features::fillsign::Item,
        window: &mut dyn WindowHandle,
    ) {
        let label = item.kind().to_string();
        let options = acrux_features::fillsign::Options {
            item,
            page,
            rect,
            author: None,
            ..acrux_features::fillsign::Options::default()
        };
        self.apply_edit(
            EditOp::FillSign {
                options: Box::new(options),
            },
            window,
        );
        self.set_notice(format!("posé : {label} en page {}", page + 1));
    }

    /// Demande le texte à écrire, puis le pose là où l'on a cliqué.
    fn start_sign_text(&mut self, page: usize, rect: Rect) {
        self.prompt = Some(Prompt {
            title: "Texte".into(),
            label: format!("À écrire en page {} :", page + 1),
            input: TextInput::new("Nom, date, numéro…"),
            error: None,
            kind: PromptKind::SignText { page, rect },
        });
    }

    /// Dessine la barre de l'outil, sous la barre d'onglets.
    fn paint_sign_bar(&mut self, frame: &mut Frame<'_>) {
        if self.sign_height() == 0 {
            return;
        }
        let y = Toolbar::height(&self.theme, self.dpi_scale as f32) + self.tabs_height() as i32;
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        let Some(text) = &mut self.text else { return };
        if let Some(bar) = &mut self.sign_bar {
            bar.paint(frame, text, &mut self.raster, &theme, dpi, y);
        }
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
        self.prefs.dark_theme = self.theme.canvas == Theme::dark().canvas;
        self.prefs.view_mode = self.view_mode;
        self.prefs.panel_open = self.panel_open;
        self.prefs.fit_width = self.fit_width;
        self.prefs.zoom = self.zoom;
        self.prefs.two_up_cover = self.two_up_cover;
        self.prefs.signature = self.signature.as_ref().map(Saved::encode);
        self.prefs.initials = self.initials.as_ref().map(Saved::encode);
        if self.width > 0 && self.height > 0 && self.dpi_scale > 0.0 {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let logical = |v: u32| (f64::from(v) / self.dpi_scale).round() as u32;
            self.prefs.window = (logical(self.width), logical(self.height));
        }
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

    fn set_zoom(&mut self, zoom: f64) {
        // Conserve le point du document au centre de la vue.
        let old_scale = self.scale();
        let center = (self.scroll_y + f64::from(self.view_height()) / 2.0) / old_scale;
        self.fit_width = false;
        self.zoom = zoom.clamp(0.1, 16.0);
        let new_scale = self.scale();
        self.scroll_y = center * new_scale - f64::from(self.view_height()) / 2.0;
        self.clamp_scroll();
        self.title_dirty = true;
    }

    fn zoom_step(&mut self, direction: i32) {
        let real = self.effective_zoom();
        let next = if direction > 0 {
            ZOOM_STEPS
                .iter()
                .copied()
                .find(|z| *z > real + 1e-6)
                .unwrap_or(real * 1.25)
        } else {
            ZOOM_STEPS
                .iter()
                .rev()
                .copied()
                .find(|z| *z < real - 1e-6)
                .unwrap_or(real / 1.25)
        };
        self.set_zoom(next);
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
        for r in worker.poll() {
            self.last_render_ms = r.ms;
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
        let mut last_ms = None;
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
                    // Page blanche en attendant le rendu.
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
                let b = render_page(&l.doc, &l.pages[i], scale, &options).bitmap;
                last_ms = Some(start.elapsed().as_secs_f64() * 1000.0);
                l.cache.insert(key, b);
            }
            if let Some(bitmap) = l.cache.get(&key) {
                let visible_h = (bitmap.height() as i32).min(clip_h - dy).max(0) as u32;
                frame.blit_rgba_premultiplied(dx, dy, bitmap.width(), visible_h, bitmap.data());
            }
        }
        if let Some(ms) = last_ms {
            self.last_render_ms = ms;
        }
        // Le cache ne garde que les pages proches de la vue.
        if l.cache.len() > 12 {
            let keep: Vec<(usize, u32)> = l
                .cache
                .keys()
                .filter(|(i, s)| {
                    *s == thumb_key
                        || (*s == key_scale
                            && layout.get(*i).is_some_and(|b| {
                                b.visible
                                    && f64::from(b.y + b.h as i32) >= view_top - 2000.0
                                    && f64::from(b.y) <= view_bottom + 2000.0
                            }))
                })
                .copied()
                .collect();
            l.cache.retain(|k, _| keep.contains(k));
        }
        // Indication « rendu en cours » sur les pages en attente.
        if let Some(text) = &mut self.text {
            let size = t.font_size * dpi;
            for (dx, dy, w, h) in placeholders {
                let label = "Rendu en cours…";
                let tw = text.measure(size, label);
                let cx = dx as f32 + (w as f32 - tw) / 2.0;
                let cy = dy as f32 + (h as f32).min(view_h as f32 - dy as f32) / 2.0;
                text.draw(frame, cx, cy, size, label, t.text_dim);
            }
        }
    }

    fn paint_status(&mut self, frame: &mut Frame<'_>) {
        let t = self.theme;
        let h = self.status_height() as i32;
        let top = self.height as i32 - h;
        frame.fill_rect(0, top, self.width as i32, h, t.bar.0, t.bar.1, t.bar.2);
        frame.fill_rect(
            0,
            top,
            self.width as i32,
            1,
            t.separator.0,
            t.separator.1,
            t.separator.2,
        );
        let page = self.current_page() + 1;
        let zoom = self.effective_zoom();
        // Un message passager (fin d'export…) prend la place du nom de fichier
        // pendant quelques secondes.
        let notice = self.notice.as_ref().and_then(|(m, at)| {
            (at.elapsed() < std::time::Duration::from_secs(8)).then(|| m.clone())
        });
        let (left, right) = match &self.loaded {
            Some(l) => {
                let name = l
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                // Document étiqueté : l'étiquette d'abord, le rang physique
                // entre parenthèses — « page iii (3 / 240) », comme Acrobat.
                let position = match l.labels.get(page - 1) {
                    Some(label) if *label != page.to_string() => {
                        format!("page {label} ({page} / {})", l.pages.len())
                    }
                    _ => format!("page {page} / {}", l.pages.len()),
                };
                let right = format!(
                    "{position}   {:.0} %{}   {}   {:.0} ms",
                    zoom * 100.0,
                    if self.fit_width { " (largeur)" } else { "" },
                    self.view_mode.label(),
                    self.last_render_ms
                );
                (notice.unwrap_or(name), right)
            }
            None => (
                "Aucun document — Ctrl+O pour ouvrir, ou déposez un PDF ici".to_string(),
                String::new(),
            ),
        };
        let Some(text) = &mut self.text else { return };
        let size = t.font_size * self.dpi_scale as f32;
        let baseline = top as f32 + (h as f32 + text.ascent(size)) / 2.0 - 1.0;
        let pad = 10.0 * self.dpi_scale as f32;
        let right_w = text.measure(size, &right);
        let right_x = self.width as f32 - pad - right_w;
        text.draw(frame, right_x, baseline, size, &right, t.text_dim);
        let max_left = (right_x - 2.0 * pad).max(40.0);
        text.draw_clipped(frame, pad, baseline, size, &left, t.text, max_left);
    }
}

impl App for Viewer {
    #[allow(clippy::too_many_lines)] // un bras par type d'événement
    fn event(&mut self, event: Event, window: &mut dyn WindowHandle) {
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
        log_event(&event);
        match event {
            Event::Resize { width, height } => {
                self.width = width;
                self.height = height;
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
                if !results && !self.search_scanning() && !self.tip_due() && !self.media_playing() {
                    return;
                }
            }
            Event::FileDropped(path) => {
                if self.confirm_discard(window) {
                    self.open(&path, window);
                }
            }
            Event::Wheel {
                delta,
                modifiers,
                x,
                y,
            } => {
                if self.panel_open && x < self.view_left() as i32 && y >= self.view_top() as i32 {
                    self.panel.wheel(delta);
                } else if modifiers.ctrl {
                    self.zoom_step(if delta > 0.0 { 1 } else { -1 });
                } else if modifiers.shift {
                    self.scroll_x -= f64::from(delta) * 80.0;
                } else {
                    self.scroll_y -= f64::from(delta) * 80.0;
                }
                self.clamp_scroll();
            }
            Event::Key(key, m) if self.palette.is_some() => {
                let result = self.palette.as_mut().map(|p| p.key(key, m.shift));
                if let Some((command, close)) = result {
                    if close {
                        self.palette = None;
                    }
                    if let Some(c) = command {
                        self.run_command(c, window);
                    }
                }
            }
            Event::Key(key, _)
                if self.objects.is_some()
                    && self.prompt.is_none()
                    && self.objects_key(key, window) =>
            {
                window.request_redraw();
            }
            Event::Char(c, m)
                if self.objects.is_some() && !m.ctrl && self.objects_char(c, window) =>
            {
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
            Event::Key(key, m) if is_global_key(key) => self.key(key, m, window),
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
            Event::Key(key, _) if self.prompt.is_some() => self.prompt_key(key, window),
            Event::Char(c, m) if self.prompt.is_some() && !m.ctrl => {
                if let Some(p) = &mut self.prompt {
                    p.input.insert_char(c);
                    p.error = None;
                }
            }
            Event::Key(key, m) if self.search.is_some() => {
                let action = self
                    .search
                    .as_mut()
                    .map_or(InputAction::None, |s| s.input.key(key, m.shift));
                match action {
                    InputAction::Cancel => self.search = None,
                    InputAction::Submit => {
                        self.update_search();
                        if let Some(s) = &mut self.search {
                            if !s.hits.is_empty() {
                                let n = s.hits.len();
                                s.current = if m.shift {
                                    (s.current + n - 1) % n
                                } else {
                                    (s.current + 1) % n
                                };
                            }
                        }
                        self.scroll_to_hit();
                    }
                    InputAction::Changed => {
                        self.update_search();
                        self.scroll_to_hit();
                    }
                    InputAction::None => {}
                }
            }
            Event::Char(c, m) if self.search.is_some() && !m.ctrl => {
                if let Some(s) = &mut self.search {
                    if s.input.insert_char(c) == InputAction::Changed {
                        self.update_search();
                        self.scroll_to_hit();
                    }
                }
            }
            Event::Key(key, m) => self.key(key, m, window),
            Event::Char(c, m) => {
                if m.ctrl {
                    match c {
                        'o' | 'O' | '\u{f}' => {
                            if self.confirm_discard(window) {
                                if let Some(p) = window.open_file_dialog() {
                                    self.open(&p, window);
                                }
                            }
                        }
                        's' | 'S' => self.save(m.shift, window),
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
                                self.palette = Some(Palette::new(self.loaded.is_some()));
                            } else {
                                self.print(window);
                            }
                        }
                        'e' | 'E' => self.export(window),
                        'i' | 'I' => self.insert_pages(window),
                        'c' | 'C' => {
                            let text = self.selected_text();
                            if !text.is_empty() {
                                window.set_clipboard_text(&text);
                            }
                        }
                        'a' | 'A' => self.select_all(),
                        'f' | 'F' | '\u{6}' => self.open_search(),
                        // Ctrl+G : le champ de page prend le focus, prêt à
                        // recevoir un numéro ou une étiquette.
                        'g' | 'G' | '\u{7}' => {
                            let info = self.toolbar_info();
                            self.toolbar.focus_page(&info);
                        }
                        '+' | '=' => self.zoom_step(1),
                        '-' => self.zoom_step(-1),
                        '0' => {
                            self.fit_width = true;
                            self.clamp_scroll();
                        }
                        _ => {}
                    }
                } else {
                    match c {
                        '+' | '=' => self.zoom_step(1),
                        '-' => self.zoom_step(-1),
                        'f' | 'F' => {
                            self.fit_width = true;
                            self.clamp_scroll();
                        }
                        '1' => self.set_zoom(1.0),
                        't' | 'T' => self.toggle_theme(),
                        'r' => self.rotate_current(90, window),
                        'R' => self.rotate_current(-90, window),
                        ' ' => self.activate_focused_field(window),
                        'e' | 'E' => self.start_text_edit(window),
                        'h' => self.highlight_selection(window),
                        'H' => self.highlight_with_comment(window),
                        'm' => self.mark_redaction(window),
                        'M' => self.apply_redactions(window),
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
                if self.palette.is_some() {
                    let chosen = self.palette.as_ref().and_then(|p| p.mouse_down(x, y));
                    if let Some(c) = chosen {
                        self.palette = None;
                        self.run_command(c, window);
                    } else {
                        self.palette = None;
                    }
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
                } else if y < top {
                    // Barre « remplir et signer ».
                    if self.prompt.is_none() {
                        if let Some(action) =
                            self.sign_bar.as_ref().and_then(|b| b.mouse_down(x, y))
                        {
                            self.sign_action(action, window);
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
                    if self.prompt.is_none() && self.loaded.is_none() {
                        self.click_recent(x, y, window);
                    } else if self.prompt.is_none() && x >= 0 && y < self.view_height() as i32 {
                        if self.media_mouse_down(x, y, window) {
                            // Le média a pris le clic.
                        } else if let Some((_, media)) = self.media_at(x, y) {
                            self.open_media(&media, window);
                        } else if self.objects.is_some() {
                            self.objects_mouse_down(x, y, modifiers.shift);
                            window.request_redraw();
                        } else if self.place_sign(x, y, window) {
                            // L'outil a posé quelque chose : ni sélection, ni lien.
                        } else if let Some((fi, wi)) = self.widget_at(x, y) {
                            self.selection = None;
                            self.focus_field = Some(fi);
                            self.click_widget(fi, wi, window);
                        } else if let Some(link) = self.link_at(x, y) {
                            self.selection = None;
                            self.follow(&link.action, window);
                        } else {
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
            } => self.drag_last = Some((x - self.view_left() as i32, y - self.view_top() as i32)),
            Event::MouseUp { .. } if self.objects.is_some() => {
                self.objects_mouse_up(window);
                window.request_redraw();
            }
            Event::MouseMove { x, y, dragging } if self.objects.is_some() => {
                let (vx, vy) = (x - self.view_left() as i32, y - self.view_top() as i32);
                if self.objects_mouse_move(vx, vy, dragging) {
                    window.request_redraw();
                }
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
            Event::MouseUp { .. } => {
                if self.panel.dragging() {
                    let action = self.panel.mouse_up();
                    self.panel_action(action, window);
                } else {
                    let _ = self.panel.mouse_up();
                }
                self.drag_last = None;
                self.sel_dragging = false;
                if self.selection.is_some_and(|s| s.is_empty()) {
                    self.selection = None;
                }
            }
            Event::MouseMove { x, y, dragging } => {
                if let Some(p) = &mut self.palette {
                    if p.mouse_move(x, y) {
                        window.request_redraw();
                    }
                    return;
                }
                let bar = Toolbar::height(&self.theme, self.dpi_scale as f32);
                let mut hover_changed = self.toolbar.mouse_move(x, y);
                if hover_changed {
                    self.update_tip(window);
                }
                if self.tabs_height() > 0 && y >= bar && y < self.view_top() as i32 {
                    hover_changed |= self.tabs.mouse_move(x, y - bar);
                } else {
                    hover_changed |= self.tabs.mouse_leave();
                }
                let in_panel =
                    self.panel_open && x < self.view_left() as i32 && y >= self.view_top() as i32;
                if in_panel {
                    hover_changed |= self
                        .panel
                        .mouse_move(x, y - self.view_top() as i32, dragging);
                } else {
                    hover_changed |= self.panel.mouse_leave();
                }
                let (x, y) = (x - self.view_left() as i32, y - self.view_top() as i32);
                self.last_mouse = (x >= 0 && y >= 0).then_some((x, y));
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
                    let cursor = if in_view
                        && (self.widget_at(x, y).is_some() || self.link_at(x, y).is_some())
                    {
                        Cursor::Hand
                    } else if in_view && self.text_pos_at(x, y).is_some_and(|(_, on)| on) {
                        Cursor::IBeam
                    } else {
                        Cursor::Arrow
                    };
                    window.set_cursor(cursor);
                    if hover_changed {
                        window.request_redraw();
                    }
                    return;
                }
            }
            Event::MouseDown { .. } => {}
            Event::Close => {
                if self.confirm_discard_all(window) {
                    self.save_prefs();
                    window.close();
                }
            }
        }
        if self.title_dirty {
            self.update_title(window);
        }
        // La recherche avance par petites tranches : chaque réveil en traite
        // une, ce qui laisse passer les événements de l'utilisateur entre-temps.
        if self.step_search() {
            if self.waker.is_none() {
                self.waker = Some(window.waker());
            }
            if let Some(w) = &self.waker {
                w.wake();
            }
        }
        window.request_redraw();
    }

    fn paint(&mut self, frame: &mut Frame<'_>) {
        self.collect_results();
        self.poll_updates();
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
            self.paint_document(&mut view);
            if self.loaded.is_none() {
                self.paint_welcome(&mut view);
            }
            self.paint_selection(&mut view);
            self.paint_objects(&mut view);
            self.paint_media(&mut view);
            self.paint_field_focus(&mut view);
            self.paint_search(&mut view);
        }
        if self.panel_open && left > 0 && vh > 0 {
            let mut side = frame.sub(0, top, left as u32, vh);
            self.paint_panel(&mut side);
        }
        if !self.fullscreen && !self.reading {
            self.paint_toolbar(frame);
            self.paint_tabs(frame);
            self.paint_sign_bar(frame);
            self.paint_status(frame);
        }
        self.paint_capture(frame);
        self.paint_prompt(frame);
        self.paint_tip(frame);
        self.paint_palette(frame);
        dump_frame(frame);
    }
}

impl Viewer {
    fn key(&mut self, key: Key, m: Modifiers, window: &mut dyn WindowHandle) {
        let page_h = f64::from(self.view_height());
        match key {
            Key::F(4) => self.toggle_panel(),
            Key::F(5) => self.toggle_reading(window),
            Key::F(6) => self.cycle_region(!m.shift, window),
            Key::F(11) => self.toggle_fullscreen(window),
            Key::Tab if m.ctrl => self.cycle_tab(!m.shift),
            Key::Tab => self.focus_next_field(!m.shift),
            Key::Enter => self.activate_focused_field(window),
            Key::Delete if m.ctrl => self.delete_current(window),
            Key::Down => self.scroll_y += 60.0,
            Key::Up => self.scroll_y -= 60.0,
            Key::Right => self.scroll_x += 60.0,
            Key::Left => self.scroll_x -= 60.0,
            Key::PageDown => {
                // En mode « une rangée à la fois », la page est l'unité de
                // défilement dès qu'on est en bas de la rangée affichée.
                if m.ctrl || (self.view_mode.is_paged() && self.at_bottom()) {
                    self.step_row(true);
                } else {
                    self.scroll_y += page_h * 0.9;
                }
            }
            Key::PageUp => {
                if m.ctrl || (self.view_mode.is_paged() && self.scroll_y <= 0.5) {
                    self.step_row(false);
                } else {
                    self.scroll_y -= page_h * 0.9;
                }
            }
            Key::Home => {
                if self.view_mode.is_paged() {
                    self.scroll_to_page(0);
                } else {
                    self.scroll_y = 0.0;
                }
            }
            Key::End => {
                let last = self
                    .loaded
                    .as_ref()
                    .map_or(0, |l| l.pages.len().saturating_sub(1));
                if self.view_mode.is_paged() {
                    self.scroll_to_page(last);
                } else {
                    self.scroll_y = f64::MAX / 4.0;
                }
            }
            Key::Escape => {
                if self.media.is_some() {
                    self.close_media();
                    self.set_notice("lecture arrêtée".into());
                } else if self.objects.is_some() {
                    self.toggle_objects(window);
                } else if self.sign_bar.is_some() {
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

#[cfg(test)]
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
