//! Barre d'outils : boutons à icône, séparateurs, champ de page et libellé
//! de zoom, disposés de gauche à droite avec un groupe aligné à droite.
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
use crate::ui::paint::round_rect;
use crate::ui::palette::{describe, Command};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Action demandée par l'utilisateur.
// Pas `Copy` : `GoToLabel` porte le texte tapé dans le champ de page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolAction {
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
    /// Pivoter la page courante de 90° (modification du document).
    RotatePage,
    /// Enregistrer.
    Save,
    /// Imprimer.
    Print,
    /// Changer la disposition des pages.
    CycleViewMode,
}

impl ToolAction {
    /// Commande de la palette correspondante, quand il y en a une : c'est
    /// elle qui porte le libellé et le raccourci affichés en info-bulle.
    #[must_use]
    fn command(&self) -> Option<Command> {
        Some(match self {
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
            ToolAction::RotatePage => Command::RotateRight,
            ToolAction::Save => Command::Save,
            ToolAction::Print => Command::Print,
            ToolAction::CycleViewMode => Command::CycleViewMode,
            ToolAction::GoToPage(_) | ToolAction::GoToLabel(_) => return None,
        })
    }
}

/// Ce que la barre affiche.
#[derive(Debug, Clone)]
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
}

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
    /// Libellé « 128 % ».
    ZoomLabel,
    /// Espace extensible : ce qui suit est aligné à droite.
    Spacer,
}

/// Barre d'outils.
pub struct Toolbar {
    items: Vec<Item>,
    /// Rectangles `(x, y, w, h)` des éléments, calculés au dernier dessin.
    rects: Vec<(i32, i32, i32, i32)>,
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
    pub fn new() -> Self {
        use Item::{Button, PageBox, Separator, Spacer, ZoomLabel};
        let items = vec![
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
            ZoomLabel,
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
            Button {
                icon: Icon::Tools,
                action: ToolAction::ToggleTools,
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
        let h = Self::height(theme, dpi);
        let button = (32.0 * dpi).round() as i32;
        let gap = (2.0 * dpi).round() as i32;
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
        let widths: Vec<i32> = self
            .items
            .iter()
            .map(|it| match it {
                Item::Button { .. } => button,
                Item::Separator => (9.0 * dpi).round() as i32,
                Item::PageBox => page_box,
                Item::ZoomLabel => text.measure(size, "1000 %").round() as i32 + 2 * pad,
                Item::Spacer => 0,
            })
            .collect();
        let after_spacer: i32 = self
            .items
            .iter()
            .zip(&widths)
            .skip_while(|(it, _)| !matches!(it, Item::Spacer))
            .map(|(_, w)| w + gap)
            .sum();
        let mut x = pad;
        let y = (h - button) / 2;
        for (i, (it, w)) in self.items.iter().zip(&widths).enumerate() {
            if matches!(it, Item::Spacer) {
                x = (width - pad - after_spacer).max(x);
                self.rects[i] = (x, y, 0, button);
                continue;
            }
            self.rects[i] = (x, y, *w, button);
            x += w + gap;
        }
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
        let size = t.font_size * dpi;
        let pad = (6.0 * dpi).round() as i32;
        let icon_px = (20.0 * dpi).round();
        for (i, it) in self.items.iter().enumerate() {
            let (x, y, w, bh) = self.rects[i];
            match it {
                Item::Button {
                    icon,
                    needs_document,
                    ..
                } => {
                    let enabled = info.has_document || !needs_document;
                    if enabled && self.hover == Some(i) {
                        round_rect(frame, x, y, w, bh, 7.0 * dpi, t.hover);
                    }
                    if self.focus == Some(i) {
                        focus_ring(frame, t, x, y, w, bh);
                    }
                    let color = if enabled { t.text } else { t.text_dim };
                    let ix = x + (w - icon_px as i32) / 2;
                    let iy = y + (bh - icon_px as i32) / 2;
                    icons::draw(frame, raster, *icon, ix, iy, icon_px, color);
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
                Item::PageBox => {
                    if let Some(input) = &self.page_input {
                        input.draw(frame, text, t, dpi, x, y + 3, w, bh - 6);
                    } else {
                        let label = Self::page_box_label(info);
                        if info.has_document && self.hover == Some(i) {
                            round_rect(frame, x, y, w, bh, 7.0 * dpi, t.hover);
                        }
                        let room = (w - pad) as f32;
                        let tw = text.measure(size, &label).min(room);
                        let baseline = y as f32 + (bh as f32 + text.ascent(size)) / 2.0 - 1.0;
                        let color = if info.has_document {
                            t.text
                        } else {
                            t.text_dim
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
                }
                Item::ZoomLabel => {
                    let label = if info.has_document {
                        format!("{} %", info.zoom_percent)
                    } else {
                        String::new()
                    };
                    let tw = text.measure(size, &label);
                    let baseline = y as f32 + (bh as f32 + text.ascent(size)) / 2.0 - 1.0;
                    text.draw(
                        frame,
                        x as f32 + (w as f32 - tw) / 2.0,
                        baseline,
                        size,
                        &label,
                        t.text,
                    );
                }
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
            .filter(|&i| {
                !matches!(
                    self.items[i],
                    Item::Separator | Item::Spacer | Item::ZoomLabel
                )
            })
    }

    /// Déplacement de la souris : vrai si l'aspect a changé.
    /// Texte de l'info-bulle du bouton survolé et rectangle de ce bouton.
    #[must_use]
    pub fn hover_tip(&self) -> Option<(String, (i32, i32, i32, i32))> {
        let index = self.hover?;
        let Item::Button { action, .. } = self.items.get(index)? else {
            return None;
        };
        let (label, shortcut) = describe(action.command()?)?;
        let text = if shortcut.is_empty() {
            label.to_string()
        } else {
            format!("{label}  ({shortcut})")
        };
        Some((text, *self.rects.get(index)?))
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
                if *needs_document && !info.has_document {
                    return None;
                }
                self.page_input = None;
                Some(action.clone())
            }
            Item::PageBox if info.has_document => {
                self.focus_page(info);
                None
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

    /// Indices des éléments atteignables au clavier, dans l'ordre visuel.
    fn focusable(&self, info: &ToolbarInfo) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, it)| match it {
                Item::Button { needs_document, .. } => info.has_document || !*needs_document,
                Item::PageBox => info.has_document,
                _ => false,
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
            return self.focus_edge(!forward, info);
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

    /// Active l'élément sous le focus (Entrée ou Espace).
    pub fn activate_focus(&mut self, info: &ToolbarInfo) -> Option<ToolAction> {
        let index = self.focus?;
        match self.items.get(index)? {
            Item::Button { action, .. } => Some(action.clone()),
            Item::PageBox => {
                self.focus_page(info);
                None
            }
            _ => None,
        }
    }
}

/// Anneau de focus : deux cadres, l'un clair l'autre accentué, pour rester
/// visible sur un fond sombre comme sur un fond clair.
fn focus_ring(frame: &mut Frame<'_>, t: &Theme, x: i32, y: i32, w: i32, h: i32) {
    let a = t.accent;
    for (inset, color) in [(0, t.bar), (1, a), (2, a)] {
        let (x, y, w, h) = (x + inset, y + inset, w - 2 * inset, h - 2 * inset);
        if w <= 0 || h <= 0 {
            continue;
        }
        frame.fill_rect(x, y, w, 1, color.0, color.1, color.2);
        frame.fill_rect(x, y + h - 1, w, 1, color.0, color.1, color.2);
        frame.fill_rect(x, y, 1, h, color.0, color.1, color.2);
        frame.fill_rect(x + w - 1, y, 1, h, color.0, color.1, color.2);
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
            page_label: None,
            zoom_percent: 100,
            has_document: true,
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
        tb.rects[1] = (0, 0, 32, 32);
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
}
