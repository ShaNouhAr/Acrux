//! Barre d'outils : boutons à icône, séparateurs, champ de page et liste
//! du zoom, disposés de gauche à droite avec un groupe aligné à droite.
//! Tout est dessiné par le toolkit interne ; la barre ne connaît pas le
//! document, elle reçoit un [`ToolbarInfo`] et renvoie des [`ToolAction`].

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    clippy::too_many_arguments,
    clippy::manual_midpoint
)]

use acrux_graphics::Rasterizer;

use crate::platform::{Frame, Key};
use crate::ui::icons::{self, Icon};
use crate::ui::input::{InputAction, TextInput};
use crate::ui::lang::tr;
use crate::ui::paint::{inner_ring, round_rect, round_rect_alpha, round_rect_outline};
use crate::ui::palette::{describe, Command};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Action demandée par l'utilisateur.
// Pas `Copy` : `GoToLabel` porte le texte tapé dans le champ de page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolAction {
    /// Revenir à l'accueil.
    Home,
    /// Ouvrir les paramètres.
    Settings,
    /// Ouvrir un fichier.
    Open,
    /// Page précédente.
    PrevPage,
    /// Page suivante.
    NextPage,
    /// Aller à la page (1 = première).
    GoToPage(usize),
    /// Aller à la page portant cette étiquette (« iv », « Annexe-A »). Émis à
    /// la place de [`ToolAction::GoToPage`] quand le document a un
    /// `/PageLabels` : c'est l'étiquette que l'utilisateur voit, donc celle
    /// qu'il tape. Le visualiseur retombe sur le numéro physique si rien ne
    /// correspond.
    GoToLabel(String),
    /// Zoom arrière.
    ZoomOut,
    /// Zoom avant.
    ZoomIn,
    /// Ajuster à la largeur.
    FitWidth,
    /// Ouvrir la recherche.
    Search,
    /// Basculer le thème.
    ToggleTheme,
    /// Afficher / masquer le panneau latéral.
    TogglePanel,
    /// Afficher / masquer la barre des outils, à droite.
    ToggleTools,
    /// Annuler la dernière modification (Ctrl+Z) : d'abord dans le bloc de
    /// texte en cours de saisie, puis dans l'historique du document.
    Undo,
    /// Rétablir la modification annulée (Ctrl+Y).
    Redo,
    /// Pivoter la page courante de 90° (modification du document).
    RotatePage,
    /// Enregistrer.
    Save,
    /// Imprimer.
    Print,
    /// Changer la disposition des pages.
    CycleViewMode,
    /// Dérouler la liste du zoom sous sa case.
    ZoomMenu,
}

impl ToolAction {
    /// Commande de la palette correspondante, quand il y en a une : c'est
    /// elle qui porte le libellé et le raccourci affichés en info-bulle.
    #[must_use]
    fn command(&self) -> Option<Command> {
        Some(match self {
            ToolAction::Home => Command::Home,
            ToolAction::Settings => Command::Settings,
            ToolAction::Open => Command::Open,
            ToolAction::PrevPage => Command::PrevPage,
            ToolAction::NextPage => Command::NextPage,
            ToolAction::ZoomOut => Command::ZoomOut,
            ToolAction::ZoomIn => Command::ZoomIn,
            ToolAction::FitWidth => Command::FitWidth,
            ToolAction::Search => Command::Search,
            ToolAction::ToggleTheme => Command::ToggleTheme,
            ToolAction::TogglePanel => Command::TogglePanel,
            ToolAction::ToggleTools => Command::ToggleTools,
            ToolAction::Undo => Command::Undo,
            ToolAction::Redo => Command::Redo,
            ToolAction::RotatePage => Command::RotateRight,
            ToolAction::Save => Command::Save,
            ToolAction::Print => Command::Print,
            ToolAction::CycleViewMode => Command::CycleViewMode,
            ToolAction::ZoomMenu => Command::ZoomMenu,
            ToolAction::GoToPage(_) | ToolAction::GoToLabel(_) => return None,
        })
    }
}

/// Ce que la barre affiche.
// Des états indépendants que la barre ne fait que lire : une énumération les
// multiplierait sans rien apprendre de plus.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default)]
pub struct ToolbarInfo {
    /// Page courante (1 = première).
    pub page: usize,
    /// Nombre de pages.
    pub page_count: usize,
    /// Étiquette de la page courante (`/PageLabels`), quand le document en
    /// déclare : c'est elle que la barre affiche, comme Acrobat, et c'est
    /// elle que l'utilisateur peut taper.
    pub page_label: Option<String>,
    /// Zoom en pourcent.
    pub zoom_percent: u32,
    /// Un document est ouvert (sinon les boutons de navigation sont grisés).
    pub has_document: bool,
    /// Il y a quelque chose à annuler : dans l'historique du document, ou
    /// dans le bloc de texte en cours de saisie. Sinon « Annuler » est grisé.
    pub can_undo: bool,
    /// Il y a une modification annulée à rétablir.
    pub can_redo: bool,
    /// Le panneau latéral est ouvert : son bouton est « allumé ».
    pub panel_open: bool,
    /// La colonne des outils est ouverte : son bouton est « allumé ».
    pub tools_open: bool,
    /// La colonne des outils tiendrait dans la fenêtre. Sinon son bouton
    /// est grisé : il ne pourrait rien montrer.
    pub tools_fit: bool,
    /// Le thème en vigueur est sombre : le bouton du thème montre le soleil
    /// (on passera au clair), sinon la lune.
    pub dark_theme: bool,
    /// La liste du zoom est déroulée sous sa case : la case reste enfoncée.
    pub zoom_open: bool,
}

/// Le bouton répond-il ? « Annuler » et « Rétablir » suivent l'historique,
/// « Outils » la place qu'a sa colonne ; les autres, la présence d'un
/// document quand ils en ont besoin.
fn enabled(action: &ToolAction, needs_document: bool, info: &ToolbarInfo) -> bool {
    match action {
        ToolAction::Undo => info.has_document && info.can_undo,
        ToolAction::Redo => info.has_document && info.can_redo,
        ToolAction::ToggleTools => info.has_document && info.tools_fit,
        _ => info.has_document || !needs_document,
    }
}

/// Le bouton est-il « allumé » ? Seules les bascules le sont, quand ce
/// qu'elles montrent est affiché : on voit d'un coup d'œil ce qui est ouvert,
/// et donc ce qu'un clic refermera.
fn active(action: &ToolAction, info: &ToolbarInfo) -> bool {
    match action {
        ToolAction::TogglePanel => info.has_document && info.panel_open,
        ToolAction::ToggleTools => info.has_document && info.tools_open,
        _ => false,
    }
}

/// Icône affichée : celle de l'élément, sauf pour le thème, dont le bouton
/// montre **où l'on va** — la lune en thème clair, le soleil en sombre —
/// comme les interrupteurs de thème des navigateurs et des systèmes.
fn shown_icon(icon: Icon, action: &ToolAction, info: &ToolbarInfo) -> Icon {
    match action {
        ToolAction::ToggleTheme if !info.dark_theme => Icon::Moon,
        _ => icon,
    }
}

/// Texte de l'info-bulle d'un bouton : son libellé puis son raccourci, que
/// la palette fournit. Les bascules disent ce que le clic **fera** — « Passer
/// au thème sombre », « Masquer le panneau latéral » — plutôt que le nom
/// neutre de la commande, qui laissait deviner dans quel sens on allait.
fn tip_text(action: &ToolAction, info: &ToolbarInfo) -> Option<String> {
    let (label, shortcut) = describe(action.command()?)?;
    let label = match action {
        ToolAction::ToggleTheme if info.dark_theme => tr("Passer au thème clair"),
        ToolAction::ToggleTheme => tr("Passer au thème sombre"),
        ToolAction::TogglePanel if info.panel_open => tr("Masquer le panneau latéral"),
        ToolAction::TogglePanel => tr("Afficher le panneau latéral"),
        ToolAction::ToggleTools if info.tools_open => tr("Masquer les outils"),
        ToolAction::ToggleTools => tr("Tous les outils"),
        _ => label,
    };
    Some(with_shortcut(label, shortcut))
}

/// Info-bulle de la case du zoom, reprise par le zoom de la barre d'état :
/// ce que fait le clic, et le geste qui zoome sans passer par la liste.
#[must_use]
pub fn zoom_tip() -> String {
    with_shortcut(tr("Choisir le niveau de zoom"), tr("Ctrl+molette"))
}

/// Info-bulle du champ de page, reprise par la page de la barre d'état :
/// on peut y taper un numéro, et le raccourci pour y venir au clavier.
#[must_use]
pub fn page_tip() -> Option<String> {
    let (label, shortcut) = describe(Command::GoToPage)?;
    Some(with_shortcut(label, shortcut))
}

/// Côté du chevron de la case du zoom, en pixels logiques.
const CHEVRON: f32 = 14.0;

/// « libellé  (raccourci) », ou le libellé seul s'il n'y a pas de raccourci.
fn with_shortcut(label: &str, shortcut: &str) -> String {
    if shortcut.is_empty() {
        label.to_string()
    } else {
        format!("{label}  ({shortcut})")
    }
}

/// Boutons qui s'effacent, dans cet ordre, quand la fenêtre est trop étroite
/// pour toute la barre. Sans cela, le groupe de droite (rechercher, outils,
/// paramètres, thème) était repoussé hors de la fenêtre. Chacun de ceux-ci
/// garde un autre accès : un raccourci, la molette, la palette ou le clic
/// droit. Restent jusqu'au bout le panneau, l'ouverture, le champ de page,
/// la case du zoom, Annuler, Rétablir et Enregistrer.
///
/// Les paires partent ensemble : un « + » sans son « − », une page suivante
/// sans la précédente, se liraient comme des oublis.
const DROP_ORDER: [&[ToolAction]; 7] = [
    &[ToolAction::CycleViewMode],
    &[ToolAction::FitWidth],
    &[ToolAction::Print],
    &[ToolAction::RotatePage],
    &[ToolAction::Home],
    &[ToolAction::ZoomOut, ToolAction::ZoomIn],
    &[ToolAction::PrevPage, ToolAction::NextPage],
];

// Pas `Copy` : `Item::Button` porte une `ToolAction` qui ne l'est plus.
#[derive(Debug, Clone)]
enum Item {
    Button {
        icon: Icon,
        action: ToolAction,
        needs_document: bool,
    },
    Separator,
    /// Champ « page / total », éditable au clic.
    PageBox,
    /// Case « 128 % » et son chevron : un clic déroule la liste du zoom.
    ZoomBox,
    /// Espace extensible : ce qui suit est aligné à droite.
    Spacer,
}

/// Barre d'outils.
pub struct Toolbar {
    items: Vec<Item>,
    /// Rectangles `(x, y, w, h)` des éléments, calculés au dernier dessin.
    rects: Vec<(i32, i32, i32, i32)>,
    /// Éléments effacés faute de place au dernier dessin ([`DROP_ORDER`]) :
    /// ni dessinés, ni cliquables, ni atteignables au clavier.
    hidden: Vec<bool>,
    hover: Option<usize>,
    /// Élément tenant le focus clavier, quand la barre l'a. Se déplace aux
    /// flèches, s'active par Entrée ou Espace : tout ce que la souris fait
    /// doit être atteignable sans elle.
    focus: Option<usize>,
    /// Saisie d'un numéro de page en cours.
    page_input: Option<TextInput>,
}

impl Default for Toolbar {
    fn default() -> Self {
        Self::new()
    }
}

impl Toolbar {
    /// Barre standard du visualiseur.
    #[must_use]
    #[allow(clippy::too_many_lines)] // un élément par bouton, dans l'ordre de la barre
    pub fn new() -> Self {
        use Item::{Button, PageBox, Separator, Spacer, ZoomBox};
        let items = vec![
            // L'accueil d'abord, tout à gauche : c'est le retour à la base,
            // et on le cherche là.
            Button {
                icon: Icon::Home,
                action: ToolAction::Home,
                needs_document: false,
            },
            Button {
                icon: Icon::Sidebar,
                action: ToolAction::TogglePanel,
                needs_document: true,
            },
            Button {
                icon: Icon::Open,
                action: ToolAction::Open,
                needs_document: false,
            },
            Separator,
            Button {
                icon: Icon::Prev,
                action: ToolAction::PrevPage,
                needs_document: true,
            },
            PageBox,
            Button {
                icon: Icon::Next,
                action: ToolAction::NextPage,
                needs_document: true,
            },
            Separator,
            Button {
                icon: Icon::ZoomOut,
                action: ToolAction::ZoomOut,
                needs_document: true,
            },
            ZoomBox,
            Button {
                icon: Icon::ZoomIn,
                action: ToolAction::ZoomIn,
                needs_document: true,
            },
            Button {
                icon: Icon::FitWidth,
                action: ToolAction::FitWidth,
                needs_document: true,
            },
            Button {
                icon: Icon::ViewMode,
                action: ToolAction::CycleViewMode,
                needs_document: true,
            },
            Separator,
            // Annuler et Rétablir ouvrent le groupe des modifications : on
            // les cherche là, et à côté de ce qu'ils défont. Pas de
            // séparateur de plus : la barre est déjà large.
            Button {
                icon: Icon::Undo,
                action: ToolAction::Undo,
                needs_document: true,
            },
            Button {
                icon: Icon::Redo,
                action: ToolAction::Redo,
                needs_document: true,
            },
            Button {
                icon: Icon::Rotate,
                action: ToolAction::RotatePage,
                needs_document: true,
            },
            Button {
                icon: Icon::Save,
                action: ToolAction::Save,
                needs_document: true,
            },
            Button {
                icon: Icon::Print,
                action: ToolAction::Print,
                needs_document: true,
            },
            Spacer,
            Button {
                icon: Icon::Search,
                action: ToolAction::Search,
                needs_document: true,
            },
            // La colonne des outils ne s'affiche pas sur l'accueil : sans
            // document, le bouton n'aurait rien montré, il est donc grisé.
            Button {
                icon: Icon::Tools,
                action: ToolAction::ToggleTools,
                needs_document: true,
            },
            Button {
                icon: Icon::Settings,
                action: ToolAction::Settings,
                needs_document: false,
            },
            Button {
                icon: Icon::Theme,
                action: ToolAction::ToggleTheme,
                needs_document: false,
            },
        ];
        Self {
            rects: vec![(0, 0, 0, 0); items.len()],
            hidden: vec![false; items.len()],
            items,
            hover: None,
            focus: None,
            page_input: None,
        }
    }

    /// Hauteur en pixels physiques.
    #[must_use]
    pub fn height(theme: &Theme, dpi: f32) -> i32 {
        (theme.toolbar_height as f32 * dpi).round() as i32
    }

    /// Le champ de page a le focus clavier.
    #[must_use]
    pub fn has_focus(&self) -> bool {
        self.page_input.is_some()
    }

    /// Texte du champ de page : l'étiquette du document quand il en a, sinon
    /// le numéro physique.
    fn page_box_label(info: &ToolbarInfo) -> String {
        if !info.has_document {
            return "– / –".to_string();
        }
        match &info.page_label {
            Some(label) => format!("{label} / {}", info.page_count),
            None => format!("{} / {}", info.page, info.page_count),
        }
    }

    /// Calcule la position des éléments pour une largeur de fenêtre.
    fn layout(
        &mut self,
        width: i32,
        dpi: f32,
        text: &mut TextRenderer,
        theme: &Theme,
        info: &ToolbarInfo,
    ) {
        let pad = (6.0 * dpi).round() as i32;
        let size = theme.font_size * dpi;
        // Le champ de page suit l'étiquette courante, sans descendre sous la
        // largeur d'un numéro à quatre chiffres ni dépasser une borne : une
        // étiquette bavarde ne doit pas repousser les boutons voisins.
        let page_box = {
            let floor = text.measure(size, "9999 / 9999");
            let ceiling = text.measure(size, "MMMMMMMMMMMMMM / 9999");
            let wanted = text.measure(size, &Self::page_box_label(info));
            wanted.clamp(floor, ceiling).round() as i32 + 2 * pad
        };
        // La case du zoom garde la largeur de « 1000 % » : elle ne bouge pas
        // d'un zoom à l'autre, ni à l'ouverture d'un document.
        let chevron = (CHEVRON * dpi).round() as i32;
        let zoom_box = text.measure(size, "1000 %").round() as i32 + chevron + 2 * pad;
        self.place(width, dpi, Self::height(theme, dpi), page_box, zoom_box);
    }

    /// Place les éléments de gauche à droite, une fois les textes mesurés :
    /// la part de la disposition qui ne dépend pas de la police, et qu'une
    /// épreuve peut donc exercer seule.
    fn place(&mut self, width: i32, dpi: f32, h: i32, page_box: i32, zoom_box: i32) {
        let button = (32.0 * dpi).round() as i32;
        let gap = (2.0 * dpi).round() as i32;
        let pad = (6.0 * dpi).round() as i32;
        let widths: Vec<i32> = self
            .items
            .iter()
            .map(|it| match it {
                Item::Button { .. } => button,
                Item::Separator => (9.0 * dpi).round() as i32,
                Item::PageBox => page_box,
                Item::ZoomBox => zoom_box,
                Item::Spacer => 0,
            })
            .collect();
        // Fenêtre étroite : les boutons les moins utiles s'effacent un à un
        // jusqu'à ce que la barre tienne.
        let mut hidden = vec![false; self.items.len()];
        let span = |hidden: &[bool]| -> i32 {
            let used: i32 = widths
                .iter()
                .zip(hidden)
                .filter(|(_, gone)| !**gone)
                .map(|(w, _)| w + gap)
                .sum();
            used + 2 * pad
        };
        for victims in DROP_ORDER {
            if span(&hidden) <= width {
                break;
            }
            for (i, it) in self.items.iter().enumerate() {
                if matches!(it, Item::Button { action, .. } if victims.contains(action)) {
                    hidden[i] = true;
                }
            }
        }
        let after_spacer: i32 = self
            .items
            .iter()
            .zip(&widths)
            .zip(&hidden)
            .skip_while(|((it, _), _)| !matches!(it, Item::Spacer))
            .filter(|(_, gone)| !**gone)
            .map(|((_, w), _)| w + gap)
            .sum();
        let mut x = pad;
        let y = (h - button) / 2;
        for (i, (it, w)) in self.items.iter().zip(&widths).enumerate() {
            if matches!(it, Item::Spacer) {
                x = (width - pad - after_spacer).max(x);
                self.rects[i] = (x, y, 0, button);
                continue;
            }
            if hidden[i] {
                self.rects[i] = (x, y, 0, button);
                continue;
            }
            self.rects[i] = (x, y, *w, button);
            x += w + gap;
        }
        self.hidden = hidden;
    }

    /// Vrai si l'élément a été effacé faute de place.
    fn is_hidden(&self, index: usize) -> bool {
        self.hidden.get(index).copied().unwrap_or(false)
    }

    /// Dessine le champ « page / total », d'indice `i` dans la barre.
    fn paint_page_box(
        &self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        t: &Theme,
        dpi: f32,
        i: usize,
        info: &ToolbarInfo,
    ) {
        let (x, y, w, bh) = self.rects[i];
        let radius = 7.0 * dpi;
        // Même case que le champ de saisie qui la remplace au clic
        // (`TextInput::draw`, en y + 3 et h − 6) : rien ne saute quand on se
        // met à taper.
        let (fy, fh) = (y + 3, bh - 6);
        if let Some(input) = &self.page_input {
            input.draw(frame, text, t, dpi, x, fy, w, fh);
            return;
        }
        // Au repos, le champ est **encadré** : on doit voir qu'on peut y
        // taper un numéro. Le survol durcit le cadre au lieu de peindre un
        // bouton.
        if info.has_document {
            round_rect(frame, x, fy, w, fh, radius, t.canvas);
            let edge = if self.hover == Some(i) {
                t.text_dim
            } else {
                t.separator
            };
            round_rect_outline(frame, x, fy, w, fh, radius, dpi.max(1.0), edge);
        }
        // Le champ atteint au clavier (F6 puis les flèches) porte l'anneau
        // comme les boutons : il n'en avait aucun, et l'on ne savait plus où
        // était le focus.
        if self.focus == Some(i) {
            inner_ring(frame, x, fy, w, fh, radius, dpi, t.accent);
        }
        let size = t.font_size * dpi;
        let pad = (6.0 * dpi).round() as i32;
        let label = Self::page_box_label(info);
        let room = (w - pad) as f32;
        let tw = text.measure(size, &label).min(room);
        let baseline = y as f32 + (bh as f32 + text.ascent(size)) / 2.0 - 1.0;
        let color = if info.has_document {
            t.text
        } else {
            t.disabled()
        };
        text.draw_clipped(
            frame,
            x as f32 + (w as f32 - tw) / 2.0,
            baseline,
            size,
            &label,
            color,
            room,
        );
    }

    /// Dessine la case du zoom, d'indice `i` dans la barre : le niveau, et
    /// un chevron qui dit qu'elle se déroule. Elle est encadrée comme le
    /// champ de page, et reste enfoncée tant que sa liste est ouverte. Sans
    /// document, « – % » grisé tient la même place — comme « – / – » pour le
    /// champ de page : pas de trou dans la barre, et rien ne bouge quand un
    /// document s'ouvre.
    fn paint_zoom_box(
        &self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        t: &Theme,
        dpi: f32,
        i: usize,
        info: &ToolbarInfo,
    ) {
        let (x, y, w, bh) = self.rects[i];
        let radius = 7.0 * dpi;
        // Même case que le champ de page, pour que les deux se lisent comme
        // des champs de la même famille.
        let (fy, fh) = (y + 3, bh - 6);
        let on = info.has_document;
        let hovered = on && self.hover == Some(i);
        if on {
            let fill = if info.zoom_open { t.hover } else { t.canvas };
            round_rect(frame, x, fy, w, fh, radius, fill);
            let edge = if info.zoom_open {
                t.accent
            } else if hovered {
                t.text_dim
            } else {
                t.separator
            };
            round_rect_outline(frame, x, fy, w, fh, radius, dpi.max(1.0), edge);
        }
        if self.focus == Some(i) {
            inner_ring(frame, x, fy, w, fh, radius, dpi, t.accent);
        }
        let pad = (6.0 * dpi).round() as i32;
        let chevron = (CHEVRON * dpi).round() as i32;
        let ix = x + w - pad / 2 - chevron;
        let color = if on { t.text } else { t.disabled() };
        icons::draw(
            frame,
            raster,
            Icon::ChevronDown,
            ix,
            y + (bh - chevron) / 2,
            chevron as f32,
            if on { t.text_dim } else { color },
        );
        let size = t.font_size * dpi;
        let label = if on {
            format!("{} %", info.zoom_percent)
        } else {
            "– %".to_string()
        };
        // Le niveau est centré dans ce qui reste à gauche du chevron.
        let room = (ix - x - pad / 2).max(0) as f32;
        let tw = text.measure(size, &label).min(room);
        let baseline = y as f32 + (bh as f32 + text.ascent(size)) / 2.0 - 1.0;
        text.draw_clipped(
            frame,
            (x + pad / 2) as f32 + (room - tw) / 2.0,
            baseline,
            size,
            &label,
            color,
            room,
        );
    }

    /// Dessine la barre en haut de la fenêtre.
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f32,
        info: &ToolbarInfo,
    ) {
        let t = theme;
        let h = Self::height(t, dpi);
        let width = frame.width as i32;
        frame.fill_rect(0, 0, width, h, t.bar.0, t.bar.1, t.bar.2);
        frame.fill_rect(
            0,
            h - 1,
            width,
            1,
            t.separator.0,
            t.separator.1,
            t.separator.2,
        );
        self.layout(width, dpi, text, t, info);
        let icon_px = (20.0 * dpi).round();
        let radius = 7.0 * dpi;
        for (i, it) in self.items.iter().enumerate() {
            let (x, y, w, bh) = self.rects[i];
            if self.is_hidden(i) {
                continue;
            }
            match it {
                Item::Button {
                    icon,
                    action,
                    needs_document,
                } => {
                    let on = enabled(action, *needs_document, info);
                    let lit = active(action, info);
                    let hovered = on && self.hover == Some(i);
                    // Une bascule allumée porte un fond d'accent léger, un peu
                    // plus soutenu au survol ; les autres boutons n'ont de
                    // fond qu'au survol.
                    if lit {
                        let alpha = if hovered { 0.28 } else { 0.18 };
                        round_rect_alpha(frame, x, y, w, bh, radius, t.accent, alpha);
                    } else if hovered {
                        round_rect(frame, x, y, w, bh, radius, t.hover);
                    }
                    if self.focus == Some(i) {
                        inner_ring(frame, x, y, w, bh, radius, dpi, t.accent);
                    }
                    let color = if !on {
                        t.disabled()
                    } else if lit {
                        t.accent
                    } else {
                        t.text
                    };
                    let ix = x + (w - icon_px as i32) / 2;
                    let iy = y + (bh - icon_px as i32) / 2;
                    let shown = shown_icon(*icon, action, info);
                    icons::draw(frame, raster, shown, ix, iy, icon_px, color);
                }
                Item::Separator => {
                    let sx = x + w / 2;
                    frame.fill_rect(
                        sx,
                        y + bh / 6,
                        1,
                        bh * 2 / 3,
                        t.separator.0,
                        t.separator.1,
                        t.separator.2,
                    );
                }
                Item::PageBox => self.paint_page_box(frame, text, t, dpi, i, info),
                Item::ZoomBox => self.paint_zoom_box(frame, text, raster, t, dpi, i, info),
                Item::Spacer => {}
            }
        }
    }

    fn item_at(&self, x: i32, y: i32) -> Option<usize> {
        self.rects
            .iter()
            .position(|&(rx, ry, rw, rh)| {
                rw > 0 && x >= rx && x < rx + rw && y >= ry && y < ry + rh
            })
            .filter(|&i| !matches!(self.items[i], Item::Separator | Item::Spacer))
    }

    /// Texte de l'info-bulle de l'élément survolé et rectangle de cet
    /// élément.
    ///
    /// Un bouton grisé n'en a pas : l'info-bulle promettait une action que le
    /// clic refusait ensuite. Le champ de page en a une, qui dit qu'on peut y
    /// taper un numéro — et le raccourci pour y venir au clavier.
    #[must_use]
    pub fn hover_tip(&self, info: &ToolbarInfo) -> Option<(String, (i32, i32, i32, i32))> {
        let index = self.hover.filter(|&i| !self.is_hidden(i))?;
        let text = match self.items.get(index)? {
            Item::Button {
                action,
                needs_document,
                ..
            } => {
                if !enabled(action, *needs_document, info) {
                    return None;
                }
                tip_text(action, info)?
            }
            Item::PageBox if info.has_document && self.page_input.is_none() => page_tip()?,
            // Liste déroulée : l'info-bulle se tairait sous elle.
            Item::ZoomBox if info.has_document && !info.zoom_open => zoom_tip(),
            _ => return None,
        };
        Some((text, *self.rects.get(index)?))
    }

    /// Rectangle de la case du zoom au dernier dessin, pour y accrocher sa
    /// liste ; `None` si elle n'a pas encore été dessinée.
    #[must_use]
    pub fn zoom_rect(&self) -> Option<(i32, i32, i32, i32)> {
        let i = self
            .items
            .iter()
            .position(|it| matches!(it, Item::ZoomBox))?;
        self.rects.get(i).copied().filter(|r| r.2 > 0)
    }

    /// Survol : vrai si l'élément sous la souris a changé (il faut repeindre).
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let h = self.rects.iter().map(|r| r.1 + r.3).max().unwrap_or(0);
        let hover = if y < h { self.item_at(x, y) } else { None };
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    /// Clic gauche dans la barre.
    pub fn mouse_down(&mut self, x: i32, y: i32, info: &ToolbarInfo) -> Option<ToolAction> {
        let i = self.item_at(x, y)?;
        match &self.items[i] {
            Item::Button {
                action,
                needs_document,
                ..
            } => {
                if !enabled(action, *needs_document, info) {
                    return None;
                }
                self.page_input = None;
                Some(action.clone())
            }
            Item::PageBox if info.has_document => {
                self.focus_page(info);
                None
            }
            Item::ZoomBox if info.has_document => {
                self.page_input = None;
                Some(ToolAction::ZoomMenu)
            }
            _ => None,
        }
    }

    /// Donne le focus au champ de page, pré-rempli avec la position courante
    /// (son étiquette si le document en a). Appelé par le clic sur le champ et
    /// par la commande « Aller à une page » de la palette.
    pub fn focus_page(&mut self, info: &ToolbarInfo) {
        if !info.has_document {
            return;
        }
        let mut input = TextInput::new("page");
        input.value = info
            .page_label
            .clone()
            .unwrap_or_else(|| info.page.to_string());
        input.caret = input.value.chars().count();
        self.page_input = Some(input);
    }

    /// Touche pendant la saisie du numéro de page.
    pub fn key(&mut self, key: Key, info: &ToolbarInfo) -> Option<ToolAction> {
        let input = self.page_input.as_mut()?;
        match input.key(key, false) {
            InputAction::Submit => {
                let value = input.value.trim().to_string();
                self.page_input = None;
                if value.is_empty() {
                    return None;
                }
                // Document étiqueté : ce qui est tapé est d'abord une
                // étiquette (« iv », « Annexe-A ») ; le visualiseur retombe
                // sur le numéro physique si elle n'existe pas.
                if info.page_label.is_some() {
                    return Some(ToolAction::GoToLabel(value));
                }
                value
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n >= 1)
                    .map(ToolAction::GoToPage)
            }
            InputAction::Cancel => {
                self.page_input = None;
                None
            }
            InputAction::Changed | InputAction::None => None,
        }
    }

    /// Caractère pendant la saisie du champ de page : des chiffres seulement,
    /// sauf quand le document a des étiquettes — une étiquette peut être
    /// « xii », « A-4 » ou « Annexe B ».
    pub fn char(&mut self, c: char, info: &ToolbarInfo) {
        let accepted = if info.page_label.is_some() {
            !c.is_control()
        } else {
            c.is_ascii_digit()
        };
        if let Some(input) = &mut self.page_input {
            if accepted {
                input.insert_char(c);
            }
        }
    }

    /// Abandonne la saisie de page (clic ailleurs).
    pub fn blur(&mut self) {
        self.page_input = None;
    }

    // --- Focus clavier ---------------------------------------------------

    /// Indices des éléments atteignables au clavier, dans l'ordre visuel :
    /// ceux qui répondent et qui sont affichés.
    fn focusable(&self, info: &ToolbarInfo) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(i, it)| {
                !self.is_hidden(*i)
                    && match it {
                        Item::Button {
                            action,
                            needs_document,
                            ..
                        } => enabled(action, *needs_document, info),
                        Item::PageBox | Item::ZoomBox => info.has_document,
                        _ => false,
                    }
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Place le focus sur le premier élément (ou le dernier si `last`).
    /// Renvoie faux si la barre n'a rien à cibler.
    pub fn focus_edge(&mut self, last: bool, info: &ToolbarInfo) -> bool {
        let targets = self.focusable(info);
        self.focus = if last {
            targets.last().copied()
        } else {
            targets.first().copied()
        };
        self.focus.is_some()
    }

    /// Déplace le focus d'un cran. Renvoie faux quand on sort de la barre :
    /// au visualiseur alors de passer à la zone suivante.
    pub fn focus_step(&mut self, forward: bool, info: &ToolbarInfo) -> bool {
        let targets = self.focusable(info);
        let Some(current) = self.focus else {
            return self.focus_edge(!forward, info);
        };
        let Some(at) = targets.iter().position(|i| *i == current) else {
            // L'élément qui avait le focus ne répond plus — « Annuler », une
            // fois tout annulé. On repart de sa place, pas d'un bord de la
            // barre : la flèche droite mène à son voisin de droite.
            let next = if forward {
                targets.iter().find(|&&i| i > current)
            } else {
                targets.iter().rev().find(|&&i| i < current)
            };
            self.focus = next.copied();
            return self.focus.is_some();
        };
        let next = if forward {
            Some(at + 1)
        } else {
            at.checked_sub(1)
        };
        if let Some(target) = next.and_then(|n| targets.get(n)) {
            self.focus = Some(*target);
            true
        } else {
            self.focus = None;
            false
        }
    }

    /// Abandonne le focus clavier.
    pub fn clear_focus(&mut self) {
        self.focus = None;
    }

    /// Indice de l'élément qui a le focus (diagnostic).
    #[must_use]
    pub fn focus_index(&self) -> Option<usize> {
        self.focus
    }

    /// Vrai si la barre tient le focus clavier.
    #[must_use]
    pub fn focused(&self) -> bool {
        self.focus.is_some()
    }

    /// Active l'élément sous le focus (Entrée ou Espace). Rien, s'il ne
    /// répond pas : le focus peut rester sur « Annuler » après la dernière
    /// annulation, et Entrée n'a alors plus rien à défaire.
    pub fn activate_focus(&mut self, info: &ToolbarInfo) -> Option<ToolAction> {
        let index = self.focus?;
        if self.is_hidden(index) {
            return None;
        }
        match self.items.get(index)? {
            Item::Button {
                action,
                needs_document,
                ..
            } => enabled(action, *needs_document, info).then(|| action.clone()),
            Item::PageBox => {
                self.focus_page(info);
                None
            }
            Item::ZoomBox => info.has_document.then_some(ToolAction::ZoomMenu),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_box_edits_and_submits() {
        let mut tb = Toolbar::new();
        // Sans disposition calculée, rien n'est cliquable.
        let info = ToolbarInfo {
            page: 3,
            page_count: 10,
            zoom_percent: 100,
            has_document: true,
            ..ToolbarInfo::default()
        };
        assert_eq!(tb.mouse_down(5, 5, &info), None);
        // On simule une disposition : le champ de page occupe (100, 0, 80, 32).
        let idx = tb
            .items
            .iter()
            .position(|it| matches!(it, Item::PageBox))
            .unwrap_or(0);
        tb.rects[idx] = (100, 0, 80, 32);
        assert_eq!(tb.mouse_down(120, 10, &info), None);
        assert!(tb.has_focus());
        tb.key(Key::End, &info);
        tb.key(Key::Backspace, &info);
        tb.char('7', &info);
        // Sans étiquettes, le champ n'accepte que des chiffres.
        tb.char('x', &info);
        assert_eq!(tb.key(Key::Enter, &info), Some(ToolAction::GoToPage(7)));
        assert!(!tb.has_focus());
        // Le bouton « ouvrir » marche sans document, « suivant » non.
        let open = tb
            .items
            .iter()
            .position(|it| {
                matches!(
                    it,
                    Item::Button {
                        action: ToolAction::Open,
                        ..
                    }
                )
            })
            .unwrap_or_default();
        tb.rects[open] = (0, 0, 32, 32);
        let no_doc = ToolbarInfo {
            has_document: false,
            ..info.clone()
        };
        assert_eq!(tb.mouse_down(5, 5, &no_doc), Some(ToolAction::Open));
        let next = tb
            .items
            .iter()
            .position(|it| {
                matches!(
                    it,
                    Item::Button {
                        action: ToolAction::NextPage,
                        ..
                    }
                )
            })
            .unwrap_or(0);
        tb.rects[next] = (200, 0, 32, 32);
        assert_eq!(tb.mouse_down(210, 5, &no_doc), None);
        assert_eq!(tb.mouse_down(210, 5, &info), Some(ToolAction::NextPage));
        assert!(tb.mouse_move(210, 5));
        assert!(!tb.mouse_move(212, 6));
    }

    /// Document étiqueté : le champ montre l'étiquette, accepte des lettres
    /// et renvoie une étiquette à résoudre plutôt qu'un numéro de page.
    #[test]
    fn page_box_shows_and_accepts_a_label() {
        let mut tb = Toolbar::new();
        let info = ToolbarInfo {
            page: 3,
            page_count: 240,
            page_label: Some("iii".into()),
            zoom_percent: 100,
            has_document: true,
            ..ToolbarInfo::default()
        };
        assert_eq!(Toolbar::page_box_label(&info), "iii / 240");
        tb.focus_page(&info);
        assert!(tb.has_focus());
        // Le champ est pré-rempli avec l'étiquette, pas avec « 3 ».
        for _ in 0..3 {
            tb.key(Key::Backspace, &info);
        }
        for c in "Annexe-A".chars() {
            tb.char(c, &info);
        }
        assert_eq!(
            tb.key(Key::Enter, &info),
            Some(ToolAction::GoToLabel("Annexe-A".into()))
        );
        // Sans document, le champ ne prend pas le focus.
        let none = ToolbarInfo {
            has_document: false,
            ..info
        };
        tb.focus_page(&none);
        assert!(!tb.has_focus());
        assert_eq!(Toolbar::page_box_label(&none), "– / –");
    }

    /// Indice du bouton qui porte cette action.
    fn button(tb: &Toolbar, wanted: &ToolAction) -> usize {
        tb.items
            .iter()
            .position(|it| matches!(it, Item::Button { action, .. } if action == wanted))
            .unwrap_or(usize::MAX)
    }

    /// Un document ouvert, rien d'autre.
    fn with_document() -> ToolbarInfo {
        ToolbarInfo {
            page: 1,
            page_count: 4,
            zoom_percent: 100,
            has_document: true,
            ..ToolbarInfo::default()
        }
    }

    #[test]
    fn undo_redo_follow_history() {
        let mut tb = Toolbar::new();
        let (undo, redo) = (
            button(&tb, &ToolAction::Undo),
            button(&tb, &ToolAction::Redo),
        );
        // Annuler et Rétablir ouvrent le groupe des modifications, avant
        // Pivoter.
        assert_eq!(redo, undo + 1);
        assert_eq!(button(&tb, &ToolAction::RotatePage), redo + 1);
        tb.rects[undo] = (0, 0, 32, 32);
        tb.rects[redo] = (40, 0, 32, 32);
        let rien = with_document();
        assert_eq!(tb.mouse_down(10, 10, &rien), None);
        assert_eq!(tb.mouse_down(50, 10, &rien), None);
        let annulable = ToolbarInfo {
            can_undo: true,
            ..with_document()
        };
        assert_eq!(tb.mouse_down(10, 10, &annulable), Some(ToolAction::Undo));
        assert_eq!(tb.mouse_down(50, 10, &annulable), None);
        let retablissable = ToolbarInfo {
            can_redo: true,
            ..with_document()
        };
        assert_eq!(
            tb.mouse_down(50, 10, &retablissable),
            Some(ToolAction::Redo)
        );
        // Sans document, rien à annuler, quoi que disent les drapeaux.
        let sans = ToolbarInfo {
            has_document: false,
            can_undo: true,
            can_redo: true,
            ..ToolbarInfo::default()
        };
        assert_eq!(tb.mouse_down(10, 10, &sans), None);
        assert_eq!(tb.mouse_down(50, 10, &sans), None);
    }

    #[test]
    fn disabled_buttons_have_no_tip() {
        let mut tb = Toolbar::new();
        let undo = button(&tb, &ToolAction::Undo);
        tb.rects[undo] = (0, 0, 32, 32);
        assert!(tb.mouse_move(10, 10));
        assert_eq!(
            tb.hover_tip(&with_document()),
            None,
            "grisé : pas d'info-bulle"
        );
        let annulable = ToolbarInfo {
            can_undo: true,
            ..with_document()
        };
        let (tip, rect) = tb.hover_tip(&annulable).unwrap_or_default();
        assert!(tip.contains("Ctrl+Z"), "{tip}");
        assert!(
            tip.starts_with("Annuler") || tip.starts_with("Undo"),
            "{tip}"
        );
        assert_eq!(rect, (0, 0, 32, 32));
    }

    #[test]
    fn page_box_tip_says_it_can_be_typed_into() {
        let mut tb = Toolbar::new();
        let idx = tb
            .items
            .iter()
            .position(|it| matches!(it, Item::PageBox))
            .unwrap_or(0);
        tb.rects[idx] = (100, 0, 80, 32);
        tb.mouse_move(120, 10);
        let (tip, _) = tb.hover_tip(&with_document()).unwrap_or_default();
        assert!(tip.contains("Ctrl+G"), "{tip}");
        // Sans document, le champ est éteint : pas d'info-bulle.
        assert_eq!(tb.hover_tip(&ToolbarInfo::default()), None);
    }

    /// La case du zoom déroule sa liste — à la souris comme au clavier —
    /// dès qu'un document est ouvert, et dit ce qu'elle fait au survol.
    #[test]
    fn zoom_box_opens_its_list() {
        let mut tb = Toolbar::new();
        let zoom = tb
            .items
            .iter()
            .position(|it| matches!(it, Item::ZoomBox))
            .unwrap_or(usize::MAX);
        assert_eq!(tb.zoom_rect(), None, "pas encore dessinée");
        tb.rects[zoom] = (300, 4, 90, 32);
        assert_eq!(tb.zoom_rect(), Some((300, 4, 90, 32)));
        let doc = with_document();
        assert_eq!(tb.mouse_down(320, 10, &doc), Some(ToolAction::ZoomMenu));
        assert_eq!(tb.mouse_down(320, 10, &ToolbarInfo::default()), None);
        // Au clavier : atteinte par les flèches, activée par Entrée.
        assert!(tb.focusable(&doc).contains(&zoom));
        assert!(!tb.focusable(&ToolbarInfo::default()).contains(&zoom));
        tb.focus = Some(zoom);
        assert_eq!(tb.activate_focus(&doc), Some(ToolAction::ZoomMenu));
        assert_eq!(tb.activate_focus(&ToolbarInfo::default()), None);
        // L'info-bulle, sauf liste déroulée ou sans document.
        assert!(tb.mouse_move(320, 10));
        let (tip, rect) = tb.hover_tip(&doc).unwrap_or_default();
        assert!(tip.contains("Ctrl+"), "{tip}");
        assert_eq!(rect, (300, 4, 90, 32));
        let open = ToolbarInfo {
            zoom_open: true,
            ..with_document()
        };
        assert_eq!(tb.hover_tip(&open), None);
        assert_eq!(tb.hover_tip(&ToolbarInfo::default()), None);
    }

    #[test]
    fn theme_button_shows_where_it_goes() {
        let clair = ToolbarInfo {
            dark_theme: false,
            ..ToolbarInfo::default()
        };
        let sombre = ToolbarInfo {
            dark_theme: true,
            ..ToolbarInfo::default()
        };
        let theme = ToolAction::ToggleTheme;
        assert_eq!(shown_icon(Icon::Theme, &theme, &clair), Icon::Moon);
        assert_eq!(shown_icon(Icon::Theme, &theme, &sombre), Icon::Theme);
        // Les autres boutons gardent leur icône.
        assert_eq!(
            shown_icon(Icon::Save, &ToolAction::Save, &clair),
            Icon::Save
        );
        let vers_sombre = tip_text(&theme, &clair).unwrap_or_default();
        let vers_clair = tip_text(&theme, &sombre).unwrap_or_default();
        assert_ne!(vers_sombre, vers_clair);
        assert!(vers_sombre.ends_with("(T)"), "{vers_sombre}");
        assert!(vers_clair.ends_with("(T)"), "{vers_clair}");
    }

    #[test]
    fn toggles_report_their_state() {
        let panneau = ToolAction::TogglePanel;
        let outils = ToolAction::ToggleTools;
        let ouvert = ToolbarInfo {
            panel_open: true,
            tools_open: true,
            ..with_document()
        };
        assert!(active(&panneau, &ouvert));
        assert!(active(&outils, &ouvert));
        assert!(!active(&panneau, &with_document()));
        assert!(!active(&outils, &with_document()));
        // Sans document, rien n'est affiché : rien n'est allumé.
        let accueil = ToolbarInfo {
            has_document: false,
            ..ouvert.clone()
        };
        assert!(!active(&panneau, &accueil));
        assert!(!active(&outils, &accueil));
        // Les autres boutons ne s'allument jamais.
        assert!(!active(&ToolAction::Save, &ouvert));
        // Fenêtre trop étroite pour la colonne : le bouton ne pourrait rien
        // montrer, il est grisé plutôt que de basculer la préférence en vain.
        assert!(!enabled(&outils, true, &with_document()));
        let place = ToolbarInfo {
            tools_fit: true,
            ..with_document()
        };
        assert!(enabled(&outils, true, &place));
        // L'info-bulle dit ce que le clic fera, et le raccourci.
        let masquer = tip_text(&panneau, &ouvert).unwrap_or_default();
        let afficher = tip_text(&panneau, &with_document()).unwrap_or_default();
        assert_ne!(masquer, afficher);
        assert!(afficher.ends_with("(F4)"), "{afficher}");
        assert!(tip_text(&outils, &with_document())
            .unwrap_or_default()
            .ends_with("(Maj+F4)"));
    }

    #[test]
    fn keyboard_skips_and_ignores_disabled() {
        let mut tb = Toolbar::new();
        let undo = button(&tb, &ToolAction::Undo);
        let redo = button(&tb, &ToolAction::Redo);
        let rotate = button(&tb, &ToolAction::RotatePage);
        let info = with_document();
        assert!(!tb.focusable(&info).contains(&undo));
        let annulable = ToolbarInfo {
            can_undo: true,
            ..with_document()
        };
        assert!(tb.focusable(&annulable).contains(&undo));
        // Le focus resté sur Annuler, devenu grisé : Entrée ne fait rien…
        tb.focus = Some(undo);
        assert_eq!(tb.activate_focus(&info), None);
        assert_eq!(tb.activate_focus(&annulable), Some(ToolAction::Undo));
        // … et la flèche droite mène au voisin qui répond (Rétablir est
        // grisé lui aussi), pas au bord de la barre.
        assert!(tb.focus_step(true, &info));
        assert_eq!(tb.focus, Some(rotate));
        tb.focus = Some(redo);
        assert!(tb.focus_step(false, &info));
        assert!(tb.focus.is_some_and(|i| i < undo));
    }

    /// Fenêtre étroite : les boutons secondaires s'effacent, le groupe de
    /// droite reste dans la fenêtre.
    #[test]
    fn narrow_window_drops_secondary_buttons() {
        let mut tb = Toolbar::new();
        let info = with_document();
        // Sans police : le champ de page et le zoom prennent des largeurs
        // typiques à 100 %.
        tb.place(2000, 1.0, 40, 87, 57);
        assert!(!tb.hidden.iter().any(|h| *h), "large : tout tient");
        let theme = button(&tb, &ToolAction::ToggleTheme);
        tb.place(700, 1.0, 40, 87, 57);
        let (x, _, w, _) = tb.rects[theme];
        assert!(
            x + w <= 700,
            "le bouton du thème sort de la fenêtre : {x} + {w}"
        );
        let view = button(&tb, &ToolAction::CycleViewMode);
        assert!(tb.is_hidden(view), "la disposition s'efface la première");
        assert!(!tb.is_hidden(button(&tb, &ToolAction::Undo)));
        // Un bouton effacé ne se clique ni ne se cible.
        assert_eq!(tb.rects[view].2, 0);
        assert!(!tb.focusable(&info).contains(&view));
        // Plus étroit encore : les boutons du zoom et des pages partent, par
        // paires, mais les modifications et le groupe de droite restent.
        tb.place(560, 1.0, 40, 87, 57);
        let (x, _, w, _) = tb.rects[theme];
        assert!(
            x + w <= 560,
            "le bouton du thème sort de la fenêtre : {x} + {w}"
        );
        assert!(tb.is_hidden(button(&tb, &ToolAction::ZoomIn)));
        assert!(tb.is_hidden(button(&tb, &ToolAction::PrevPage)));
        for kept in [ToolAction::Undo, ToolAction::Redo, ToolAction::Save] {
            assert!(!tb.is_hidden(button(&tb, &kept)), "{kept:?}");
        }
        // La place revenue, tout revient.
        tb.place(2000, 1.0, 40, 87, 57);
        assert!(!tb.hidden.iter().any(|h| *h));
    }
}
