//! Fenêtre « Combiner des fichiers » : la liste des fichiers à réunir en un
//! seul PDF, leur ordre et deux réglages, comme la fenêtre du même nom
//! d'Acrobat.
//!
//! On y ajoute des fichiers par le dialogue d'ouverture (plusieurs d'un
//! coup) ou en les déposant sur la fenêtre ; on les réordonne en faisant
//! glisser une ligne, par Ctrl+↑ et Ctrl+↓ ou par les boutons « Monter » et
//! « Descendre » ; Suppr ou la croix d'une ligne la retire. Deux cases :
//! un signet par fichier, et garder les champs de formulaire. « Combiner »
//! (ou Entrée) lance la combinaison ; le visualiseur la fait sur un fil à
//! part et ouvre le résultat dans un onglet neuf.
//!
//! L'état est pur : la fenêtre reçoit des touches et des clics et rend un
//! [`CombineOutcome`] ; c'est le visualiseur qui lit les fichiers et
//! combine. Les tests éprouvent donc tout sans rien dessiner. La fenêtre ne
//! rend aucune vignette : du texte, des icônes et des aplats, une peinture
//! qui ne coûte rien.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    // La peinture est une mise en page lue de haut en bas : la découper en
    // morceaux ferait perdre le fil des ordonnées.
    clippy::too_many_lines,
    clippy::too_many_arguments
)]

use std::path::PathBuf;
use std::sync::Arc;

use acrux_graphics::Rasterizer;

use crate::platform::{Frame, Key, Modifiers};
use crate::ui::controls::{self, CheckLook};
use crate::ui::icons::{self, Icon};
use crate::ui::lang::{tr, trf};
use crate::ui::modal::{self, Appear};
use crate::ui::paint::{button, focus_ring, round_rect, round_rect_alpha, veil, ButtonLook};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Hauteur d'une ligne de la liste (px logiques).
const ROW_H: f32 = 46.0;
/// Déplacement au-delà duquel un appui sur une ligne devient un glisser
/// (px logiques).
const DRAG_START: f32 = 5.0;

/// Un fichier de la liste.
#[derive(Clone)]
pub struct CombineItem {
    /// Chemin du fichier.
    pub path: PathBuf,
    /// Nom affiché, qui sera aussi celui de son signet.
    pub name: String,
    /// Ce qu'on en sait : « 12 pages · 1,2 Mo », « Image PNG · 35 Ko ».
    pub detail: String,
    /// C'est une image (l'icône le montre).
    pub image: bool,
    /// Protégé par un mot de passe qu'on n'a pas encore : il sera demandé
    /// au moment de combiner.
    pub locked: bool,
    /// Le mot de passe, une fois donné.
    pub password: Option<Vec<u8>>,
    /// Contenu déjà en mémoire : le document ouvert, avec ses modifications
    /// pas encore enregistrées. `None` : le fichier est lu au moment de
    /// combiner.
    pub data: Option<Arc<Vec<u8>>>,
}

/// Ce que vise un clic, ou ce qui a le focus clavier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// Une ligne de la liste.
    Row(usize),
    /// La croix d'une ligne.
    Remove(usize),
    /// La liste elle-même (le focus clavier des flèches).
    List,
    /// « Ajouter des fichiers… ».
    Add,
    /// « Monter ».
    Up,
    /// « Descendre ».
    Down,
    /// La case « Un signet par fichier ».
    Bookmarks,
    /// La case « Conserver les formulaires ».
    Forms,
    /// « Combiner ».
    Combine,
    /// « Annuler ».
    Cancel,
}

/// Ce que la fenêtre demande au visualiseur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombineOutcome {
    /// Rien.
    None,
    /// Redessiner.
    Redraw,
    /// Ouvrir le dialogue d'ajout de fichiers.
    AddFiles,
    /// Combiner la liste.
    Combine,
    /// Fermer la fenêtre.
    Close,
}

/// La fenêtre « Combiner des fichiers ».
// Deux réglages, la combinaison en cours, la sélection à montrer, la
// navigation au clavier : autant de faits indépendants.
#[allow(clippy::struct_excessive_bools)]
pub struct CombineWindow {
    /// Les fichiers, dans l'ordre de la combinaison.
    pub items: Vec<CombineItem>,
    /// Un signet par fichier.
    pub bookmarks: bool,
    /// Garder les champs de formulaire.
    pub keep_forms: bool,
    /// Ligne sélectionnée.
    selected: Option<usize>,
    /// Élément qui a le focus clavier.
    focus: Hit,
    /// Élément survolé.
    hover: Option<Hit>,
    /// Bouton ou case enfoncé : il n'agira qu'au relâchement, pointeur
    /// dessus.
    pressed: Option<Hit>,
    /// Appui sur une ligne : la ligne et l'ordonnée de l'appui.
    press_row: Option<(usize, i32)>,
    /// Glisser en cours : la ligne saisie et la position d'insertion visée
    /// (0 = avant la première ligne).
    drag: Option<(usize, usize)>,
    /// Première ligne montrée.
    scroll: usize,
    /// La sélection vient de changer au clavier : la peinture la montre.
    reveal: bool,
    /// On navigue au clavier (Tab) : les anneaux de focus se montrent.
    keyboard: bool,
    /// Zones cliquables, relevées au dernier dessin.
    hits: Vec<((i32, i32, i32, i32), Hit)>,
    /// Échelle du dernier dessin (seuil du glisser).
    dpi: f32,
    /// La combinaison est en cours : les gestes sont ignorés.
    busy: bool,
    /// Un mot sur le dernier ajout (fichiers refusés), en toutes lettres.
    note: Option<String>,
    /// Apparition, comme toutes les cartes.
    appear: Appear,
}

impl Default for CombineWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl CombineWindow {
    /// Fenêtre vide : un signet par fichier et les formulaires gardés, les
    /// réglages d'Acrobat.
    #[must_use]
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            bookmarks: true,
            keep_forms: true,
            selected: None,
            focus: Hit::List,
            hover: None,
            pressed: None,
            press_row: None,
            drag: None,
            scroll: 0,
            reveal: false,
            keyboard: false,
            hits: Vec::new(),
            dpi: 1.0,
            busy: false,
            note: None,
            appear: Appear::new(),
        }
    }

    /// Vrai tant que l'apparition a une image à peindre.
    #[must_use]
    pub fn animating(&self) -> bool {
        self.appear.animating()
    }

    /// Ligne sélectionnée.
    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// La combinaison est en cours.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.busy
    }

    /// Marque la combinaison en cours, ou finie (en échec : la fenêtre
    /// reste ouverte, sa liste intacte).
    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
        self.pressed = None;
        self.press_row = None;
        self.drag = None;
    }

    /// Un mot affiché en bas de la fenêtre (fichiers refusés…), ou rien.
    pub fn set_note(&mut self, note: Option<String>) {
        self.note = note;
    }

    /// Ajoute des fichiers à la fin de la liste ; un fichier déjà présent
    /// n'est pas ajouté deux fois. Le dernier ajouté est sélectionné. Rend
    /// le nombre de fichiers ajoutés.
    pub fn add(&mut self, items: Vec<CombineItem>) -> usize {
        let mut added = 0;
        for item in items {
            if self.items.iter().any(|i| i.path == item.path) {
                continue;
            }
            self.items.push(item);
            added += 1;
        }
        if added > 0 {
            self.selected = Some(self.items.len() - 1);
            self.reveal = true;
        }
        added
    }

    /// Retire la ligne `index` ; la sélection passe à sa voisine.
    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.items.len() {
            return false;
        }
        self.items.remove(index);
        self.selected = if self.items.is_empty() {
            None
        } else {
            Some(index.min(self.items.len() - 1))
        };
        self.scroll = self.scroll.min(self.items.len().saturating_sub(1));
        self.reveal = true;
        true
    }

    /// Déplace la ligne `from` à la place `to` (index final) ; la sélection
    /// la suit. Rend faux si rien ne bouge.
    pub fn move_item(&mut self, from: usize, to: usize) -> bool {
        let n = self.items.len();
        if from >= n || to >= n || from == to {
            return false;
        }
        let item = self.items.remove(from);
        self.items.insert(to, item);
        self.selected = Some(to);
        self.reveal = true;
        true
    }

    /// Touche pressée.
    pub fn key(&mut self, key: Key, m: Modifiers) -> CombineOutcome {
        if self.busy {
            return CombineOutcome::None;
        }
        match key {
            Key::Escape => CombineOutcome::Close,
            Key::Tab => {
                self.keyboard = true;
                self.move_focus(!m.shift);
                CombineOutcome::Redraw
            }
            Key::Enter => match self.focus {
                Hit::Cancel => CombineOutcome::Close,
                Hit::Add => CombineOutcome::AddFiles,
                Hit::Up | Hit::Down | Hit::Bookmarks | Hit::Forms => self.press(self.focus),
                _ => self.press(Hit::Combine),
            },
            Key::Space => match self.focus {
                Hit::List | Hit::Row(_) | Hit::Remove(_) => CombineOutcome::None,
                other => self.press(other),
            },
            Key::Up | Key::Down if m.ctrl => {
                self.focus = Hit::List;
                self.press(if key == Key::Up { Hit::Up } else { Hit::Down })
            }
            Key::Up | Key::Down | Key::Home | Key::End => {
                self.focus = Hit::List;
                let Some(last) = self.items.len().checked_sub(1) else {
                    return CombineOutcome::Redraw;
                };
                self.selected = Some(match (key, self.selected) {
                    (Key::Home, _) | (Key::Down, None) => 0,
                    (Key::Up, Some(s)) => s.saturating_sub(1),
                    (Key::Down, Some(s)) => (s + 1).min(last),
                    // Fin, ou ↑ sans sélection : la dernière ligne.
                    _ => last,
                });
                self.reveal = true;
                CombineOutcome::Redraw
            }
            Key::Delete | Key::Backspace => match self.selected {
                Some(s) => {
                    self.focus = Hit::List;
                    self.remove(s);
                    CombineOutcome::Redraw
                }
                None => CombineOutcome::None,
            },
            _ => CombineOutcome::None,
        }
    }

    /// Arrêts de tabulation, dans l'ordre de la fenêtre.
    const STOPS: [Hit; 8] = [
        Hit::List,
        Hit::Add,
        Hit::Up,
        Hit::Down,
        Hit::Bookmarks,
        Hit::Forms,
        Hit::Combine,
        Hit::Cancel,
    ];

    /// Passe le focus à l'arrêt suivant (ou précédent).
    fn move_focus(&mut self, forward: bool) {
        let n = Self::STOPS.len();
        let at = Self::STOPS
            .iter()
            .position(|s| *s == self.focus)
            .unwrap_or(0);
        let next = if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
        };
        self.focus = Self::STOPS[next];
    }

    /// Effet d'un élément, cliqué ou pressé au clavier.
    fn press(&mut self, hit: Hit) -> CombineOutcome {
        match hit {
            Hit::Row(i) => {
                self.selected = Some(i);
                self.focus = Hit::List;
                CombineOutcome::Redraw
            }
            Hit::Remove(i) => {
                self.remove(i);
                CombineOutcome::Redraw
            }
            Hit::List => {
                self.focus = Hit::List;
                CombineOutcome::Redraw
            }
            Hit::Add => {
                self.focus = Hit::Add;
                CombineOutcome::AddFiles
            }
            Hit::Up | Hit::Down => {
                if !matches!(self.focus, Hit::List) {
                    self.focus = hit;
                }
                if let Some(s) = self.selected {
                    let to = if hit == Hit::Up {
                        s.checked_sub(1)
                    } else {
                        Some(s + 1)
                    };
                    if let Some(to) = to {
                        self.move_item(s, to);
                    }
                }
                CombineOutcome::Redraw
            }
            Hit::Bookmarks => {
                self.bookmarks = !self.bookmarks;
                self.focus = hit;
                CombineOutcome::Redraw
            }
            Hit::Forms => {
                self.keep_forms = !self.keep_forms;
                self.focus = hit;
                CombineOutcome::Redraw
            }
            Hit::Combine => {
                if self.items.is_empty() {
                    CombineOutcome::None
                } else {
                    CombineOutcome::Combine
                }
            }
            Hit::Cancel => CombineOutcome::Close,
        }
    }

    /// Élément sous un point, d'après le dernier dessin. La croix d'une
    /// ligne passe avant la ligne.
    #[must_use]
    pub fn target_at(&self, x: i32, y: i32) -> Option<Hit> {
        self.hits
            .iter()
            .find(|((hx, hy, hw, hh), _)| x >= *hx && x < hx + hw && y >= *hy && y < hy + hh)
            .map(|(_, hit)| *hit)
    }

    /// Appui. Une ligne se sélectionne tout de suite (et pourra glisser) ;
    /// un bouton ou une case s'enfonce seulement, et n'agira qu'au
    /// relâchement — on se ravise en glissant hors de lui.
    pub fn mouse_down(&mut self, x: i32, y: i32) -> CombineOutcome {
        if self.busy {
            return CombineOutcome::None;
        }
        self.pressed = None;
        self.press_row = None;
        match self.target_at(x, y) {
            Some(Hit::Row(i)) => {
                self.press_row = Some((i, y));
                self.press(Hit::Row(i))
            }
            Some(Hit::List) => self.press(Hit::List),
            Some(hit) => {
                self.pressed = Some(hit);
                self.hover = Some(hit);
                CombineOutcome::Redraw
            }
            None => CombineOutcome::None,
        }
    }

    /// Mouvement ; rend vrai si l'affichage doit changer. Bouton enfoncé
    /// sur une ligne, au-delà d'un petit seuil, c'est un glisser : le trait
    /// d'insertion suit le pointeur.
    pub fn mouse_move(&mut self, x: i32, y: i32, dragging: bool) -> bool {
        if self.busy {
            return false;
        }
        if dragging {
            if let Some((from, y0)) = self.press_row {
                let threshold = (DRAG_START * self.dpi) as i32;
                if self.drag.is_some() || (y - y0).abs() > threshold {
                    let at = self.drop_index(y).unwrap_or(from);
                    let changed = self.drag != Some((from, at));
                    self.drag = Some((from, at));
                    return changed;
                }
            }
        }
        let over = self.target_at(x, y);
        let changed = over != self.hover;
        self.hover = over;
        changed
    }

    /// Relâchement : un glisser dépose la ligne à la place visée ; un
    /// bouton enfoncé agit si le pointeur est resté dessus.
    pub fn mouse_up(&mut self, x: i32, y: i32) -> CombineOutcome {
        self.press_row = None;
        if let Some((from, at)) = self.drag.take() {
            // `at` compte les lignes d'avant le déplacement : au-delà de la
            // ligne saisie, elle-même ne compte plus.
            let to = if at > from { at - 1 } else { at };
            self.move_item(from, to);
            return CombineOutcome::Redraw;
        }
        let Some(pressed) = self.pressed.take() else {
            return CombineOutcome::None;
        };
        if self.target_at(x, y) == Some(pressed) {
            self.press(pressed)
        } else {
            CombineOutcome::Redraw
        }
    }

    /// Molette sur la fenêtre : la liste défile, deux lignes par cran.
    pub fn wheel(&mut self, delta: f32) -> bool {
        let before = self.scroll;
        let steps = (delta.abs() * 2.0).round().max(1.0) as usize;
        self.scroll = if delta > 0.0 {
            self.scroll.saturating_sub(steps)
        } else {
            (self.scroll + steps).min(self.items.len().saturating_sub(1))
        };
        self.scroll != before
    }

    /// Position d'insertion visée à l'ordonnée `y`, comme entre deux
    /// vignettes du panneau : moitié haute d'une ligne, avant elle ; moitié
    /// basse, après.
    fn drop_index(&self, y: i32) -> Option<usize> {
        let mut best: Option<(i32, usize)> = None;
        for ((_, ry, _, rh), hit) in &self.hits {
            let Hit::Row(i) = hit else { continue };
            let (d, index) = if y < ry + rh / 2 {
                ((y - ry).abs(), *i)
            } else {
                ((y - ry - rh).abs(), i + 1)
            };
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, index));
            }
        }
        best.map(|(_, i)| i)
    }

    /// Dessine la fenêtre au centre du cadre et relève les zones cliquables.
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f32,
    ) {
        let s = |v: f32| (v * dpi).round() as i32;
        self.dpi = dpi;
        let size = theme.font_size * dpi;
        let small = size * 0.9;
        let (fw, fh) = (frame.width as i32, frame.height as i32);
        let width = s(600.0).min(fw - s(24.0)).max(s(340.0));
        let height = s(560.0).min(fh - s(24.0)).max(s(360.0));
        let pad = s(24.0);
        let inner = width - 2 * pad;
        self.hits.clear();

        let progress = self.appear.progress();
        veil(frame, progress);
        let (x, y) = modal::modal_rect(fw, fh, width, height, dpi, progress);
        let y = y.min((fh - height).max(s(8.0)));
        let left = x + pad;
        modal::card(frame, theme, dpi, x, y, width, height, progress);

        // Le titre et la consigne.
        let mut cy = y + pad;
        let title_size = modal::title_size(theme, dpi);
        modal::title(
            frame,
            text,
            theme,
            dpi,
            left,
            cy + text.ascent(title_size) as i32,
            tr("Combiner des fichiers"),
        );
        cy += s(30.0);
        text.draw_clipped(
            frame,
            left as f32,
            cy as f32 + text.ascent(small),
            small,
            tr("Les fichiers sont réunis dans l'ordre de la liste : glissez une ligne, ou Ctrl+↑ et Ctrl+↓, pour le changer."),
            theme.text_dim,
            inner as f32,
        );
        cy += s(28.0);

        // Du bas vers le haut : boutons, cases, rangée d'outils ; la liste
        // prend ce qui reste.
        let bh = s(modal::BUTTON_H);
        let by = y + height - pad - bh;
        let checks_y = by - s(18.0) - s(22.0);
        let tools_h = s(30.0);
        let tools_y = checks_y - s(14.0) - tools_h;
        let list_top = cy;
        let list_h = (tools_y - s(10.0) - list_top).max(s(ROW_H));
        let row_h = s(ROW_H);
        let hover = self.hover;
        let hovered = |h: Hit| hover == Some(h);

        // La liste, dans son creux.
        controls::well(frame, left, list_top, inner, list_h, 9.0 * dpi, theme);
        // L'anneau de focus ne se montre qu'à qui navigue au clavier : la
        // liste a le focus dès l'ouverture, un anneau permanent pèserait.
        if self.keyboard && self.focus == Hit::List {
            focus_ring(
                frame,
                left,
                list_top,
                inner,
                list_h,
                9.0 * dpi,
                dpi,
                theme.accent,
            );
        }
        let visible = ((list_h - s(8.0)) / row_h).max(1) as usize;
        let count = self.items.len();
        // La sélection faite au clavier reste visible ; la molette, elle,
        // défile librement.
        if self.reveal {
            if let Some(sel) = self.selected {
                if sel < self.scroll {
                    self.scroll = sel;
                } else if sel >= self.scroll + visible {
                    self.scroll = sel + 1 - visible;
                }
            }
            self.reveal = false;
        }
        self.scroll = self.scroll.min(count.saturating_sub(visible));
        if count == 0 {
            let message =
                tr("Aucun fichier. Ajoutez-en, ou déposez-les ici : PDF, images, textes.");
            let mw = text
                .measure(size, message)
                .min(inner as f32 - s(24.0) as f32);
            text.draw_clipped(
                frame,
                left as f32 + (inner as f32 - mw) / 2.0,
                (list_top + list_h / 2) as f32 + text.ascent(size) / 2.0,
                size,
                message,
                theme.text_dim,
                inner as f32 - s(24.0) as f32,
            );
        }
        let row_x = left + s(4.0);
        let row_w = inner - s(8.0);
        let overflow = count > visible;
        let row_w = if overflow { row_w - s(8.0) } else { row_w };
        let mut ry = list_top + s(4.0);
        for index in self.scroll..(self.scroll + visible).min(count) {
            let Some(item) = self.items.get(index) else {
                break;
            };
            let selected = self.selected == Some(index);
            let dragged = self.drag.is_some_and(|(from, _)| from == index);
            if selected {
                round_rect_alpha(
                    frame,
                    row_x,
                    ry,
                    row_w,
                    row_h - s(2.0),
                    7.0 * dpi,
                    theme.accent,
                    0.22,
                );
            } else if hovered(Hit::Row(index)) || hovered(Hit::Remove(index)) {
                round_rect(
                    frame,
                    row_x,
                    ry,
                    row_w,
                    row_h - s(2.0),
                    7.0 * dpi,
                    theme.hover,
                );
            }
            let ink = if dragged { theme.text_dim } else { theme.text };
            let icon = if item.image {
                Icon::Image
            } else {
                Icon::Document
            };
            let isz = s(22.0);
            icons::draw(
                frame,
                raster,
                icon,
                row_x + s(10.0),
                ry + (row_h - isz) / 2,
                isz as f32,
                if item.locked {
                    theme.warning
                } else {
                    theme.text_dim
                },
            );
            let tx = row_x + s(42.0);
            let tw = (row_x + row_w - s(44.0) - tx) as f32;
            text.draw_clipped(
                frame,
                tx as f32,
                ry as f32 + s(18.0) as f32 + text.ascent(size) / 2.0 - s(2.0) as f32,
                size,
                &item.name,
                ink,
                tw,
            );
            let detail = if item.locked && item.password.is_none() {
                trf("{} · protégé : mot de passe demandé", &[&item.detail])
            } else {
                item.detail.clone()
            };
            text.draw_clipped(
                frame,
                tx as f32,
                ry as f32 + s(34.0) as f32 + text.ascent(small) / 2.0,
                small,
                &detail,
                if item.locked && item.password.is_none() {
                    theme.warning
                } else {
                    theme.text_dim
                },
                tw,
            );
            // La croix : discrète, elle s'affirme au survol.
            let cs = s(26.0);
            let (cx, ccy) = (row_x + row_w - cs - s(8.0), ry + (row_h - cs) / 2);
            if hovered(Hit::Remove(index)) {
                round_rect(frame, cx, ccy, cs, cs, 6.0 * dpi, theme.button_hover);
            }
            let csz = s(16.0);
            icons::draw(
                frame,
                raster,
                Icon::Close,
                cx + (cs - csz) / 2,
                ccy + (cs - csz) / 2,
                csz as f32,
                if hovered(Hit::Remove(index)) {
                    theme.text
                } else {
                    theme.text_dim
                },
            );
            self.hits.push(((cx, ccy, cs, cs), Hit::Remove(index)));
            self.hits.push(((row_x, ry, row_w, row_h), Hit::Row(index)));
            ry += row_h;
        }
        // Le trait d'insertion d'un glisser.
        if let Some((_, at)) = self.drag {
            let line_y = if at <= self.scroll {
                list_top + s(4.0)
            } else {
                list_top + s(4.0) + (at - self.scroll).min(visible) as i32 * row_h
            } - s(1.0);
            round_rect(
                frame,
                row_x,
                line_y,
                row_w,
                s(3.0).max(2),
                1.5 * dpi,
                theme.accent,
            );
        }
        // L'ascenseur, quand la liste dépasse.
        if overflow {
            let track = list_h - s(12.0);
            let thumb = (track * visible as i32 / count as i32).max(s(24.0));
            let range = (count - visible).max(1) as i32;
            let ty = list_top + s(6.0) + (track - thumb) * self.scroll as i32 / range;
            round_rect(
                frame,
                left + inner - s(9.0),
                ty,
                s(4.0),
                thumb,
                2.0 * dpi,
                theme.text_dim,
            );
        }

        // Les outils de la liste.
        let can_up = self.selected.is_some_and(|s| s > 0);
        let can_down = self.selected.is_some_and(|s| s + 1 < count);
        let mut bx = left;
        for (label, hit, enabled) in [
            (tr("Ajouter des fichiers…"), Hit::Add, true),
            (tr("Monter"), Hit::Up, can_up),
            (tr("Descendre"), Hit::Down, can_down),
        ] {
            let bw = (text.measure(size, label) as i32 + s(28.0)).max(s(72.0));
            self.push_button(
                frame,
                text,
                theme,
                dpi,
                (bx, tools_y, bw, tools_h),
                label,
                hit,
                false,
                !enabled || self.busy,
            );
            bx += bw + s(8.0);
        }

        // Les deux cases.
        for (i, (label, hit, on)) in [
            (tr("Un signet par fichier"), Hit::Bookmarks, self.bookmarks),
            (tr("Conserver les formulaires"), Hit::Forms, self.keep_forms),
        ]
        .into_iter()
        .enumerate()
        {
            let rect = controls::checkbox(
                frame,
                text,
                theme,
                dpi,
                left + i as i32 * (inner / 2),
                checks_y,
                label,
                CheckLook {
                    on,
                    enabled: !self.busy,
                    hovered: hovered(hit),
                    focused: self.keyboard && self.focus == hit,
                },
            );
            self.hits.push((rect, hit));
        }

        // L'état, à gauche des boutons : en cours, un mot sur le dernier
        // ajout, ou le compte.
        let (status, tone) = if self.busy {
            (tr("Combinaison en cours…").to_string(), theme.accent)
        } else if let Some(note) = &self.note {
            (note.clone(), theme.warning)
        } else if count > 0 {
            (trf("{} fichier(s)", &[&count.to_string()]), theme.text_dim)
        } else {
            (String::new(), theme.text_dim)
        };

        // Les boutons, groupés à droite dans l'ordre de Windows :
        // « Combiner », puis « Annuler » — posés de droite à gauche.
        let mut bx = x + width - pad;
        for (label, hit, primary, disabled) in [
            (tr("Annuler"), Hit::Cancel, false, self.busy),
            (tr("Combiner"), Hit::Combine, true, count == 0 || self.busy),
        ] {
            let bw = (text.measure(size, label) as i32 + s(32.0)).max(s(modal::BUTTON_MIN_W));
            bx -= bw;
            self.push_button(
                frame,
                text,
                theme,
                dpi,
                (bx, by, bw, bh),
                label,
                hit,
                primary,
                disabled,
            );
            bx -= s(8.0);
        }
        if !status.is_empty() {
            text.draw_clipped(
                frame,
                left as f32,
                (by + bh / 2) as f32 + text.ascent(small) / 2.0,
                small,
                &status,
                tone,
                (bx - left - s(8.0)) as f32,
            );
        }
        // La liste elle-même en dernier : ses lignes et leurs croix passent
        // avant elle.
        self.hits.push(((left, list_top, inner, list_h), Hit::List));
    }

    /// Un bouton, son libellé et sa zone cliquable (sauf grisé).
    fn push_button(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
        (bx, by, bw, bh): (i32, i32, i32, i32),
        label: &str,
        hit: Hit,
        primary: bool,
        disabled: bool,
    ) {
        let size = theme.font_size * dpi;
        let hovered = self.hover == Some(hit);
        let ink = button(
            frame,
            bx,
            by,
            bw,
            bh,
            dpi,
            theme,
            ButtonLook {
                primary,
                hovered: hovered && !disabled,
                focused: self.keyboard && self.focus == hit,
                disabled,
                pressed: hovered && self.pressed == Some(hit),
            },
        );
        let lw = text.measure(size, label);
        text.draw(
            frame,
            bx as f32 + (bw as f32 - lw) / 2.0,
            by as f32 + f32::midpoint(bh as f32, text.ascent(size)) - 1.0,
            size,
            label,
            ink,
        );
        if !disabled {
            self.hits.push(((bx, by, bw, bh), hit));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests
mod tests {
    use super::*;

    fn item(name: &str) -> CombineItem {
        CombineItem {
            path: PathBuf::from(format!("C:/essai/{name}")),
            name: name.into(),
            detail: String::new(),
            image: false,
            locked: false,
            password: None,
            data: None,
        }
    }

    fn names(w: &CombineWindow) -> Vec<&str> {
        w.items.iter().map(|i| i.name.as_str()).collect()
    }

    fn window(list: &[&str]) -> CombineWindow {
        let mut w = CombineWindow::new();
        w.add(list.iter().map(|n| item(n)).collect());
        w
    }

    const PLAIN: Modifiers = Modifiers {
        ctrl: false,
        shift: false,
        alt: false,
    };
    const CTRL: Modifiers = Modifiers {
        ctrl: true,
        shift: false,
        alt: false,
    };

    #[test]
    fn un_fichier_n_est_ajoute_qu_une_fois() {
        let mut w = window(&["a.pdf", "b.png"]);
        assert_eq!(w.add(vec![item("b.png"), item("c.tif")]), 1);
        assert_eq!(names(&w), ["a.pdf", "b.png", "c.tif"]);
        assert_eq!(w.selected(), Some(2), "le dernier ajouté est sélectionné");
        assert_eq!(w.add(vec![item("a.pdf")]), 0);
    }

    #[test]
    fn retirer_et_deplacer_restent_dans_les_bornes() {
        let mut w = window(&["a", "b", "c"]);
        assert!(w.move_item(0, 2));
        assert_eq!(names(&w), ["b", "c", "a"]);
        assert_eq!(w.selected(), Some(2));
        assert!(!w.move_item(0, 3), "au-delà de la fin");
        assert!(!w.move_item(5, 0));
        assert!(!w.move_item(1, 1));
        assert!(w.remove(2));
        assert_eq!(names(&w), ["b", "c"]);
        assert_eq!(w.selected(), Some(1), "la voisine prend la sélection");
        assert!(!w.remove(9));
        w.remove(0);
        w.remove(0);
        assert_eq!(w.selected(), None);
    }

    #[test]
    fn le_clavier_selectionne_deplace_retire_combine_et_ferme() {
        let mut w = window(&["a", "b", "c"]);
        // Le dernier ajouté est sélectionné ; Ctrl+↑ le fait monter.
        assert_eq!(w.key(Key::Up, CTRL), CombineOutcome::Redraw);
        assert_eq!(names(&w), ["a", "c", "b"]);
        assert_eq!(w.selected(), Some(1));
        w.key(Key::Up, PLAIN);
        assert_eq!(w.selected(), Some(0));
        w.key(Key::Up, CTRL);
        assert_eq!(names(&w), ["a", "c", "b"], "déjà en tête");
        w.key(Key::Down, CTRL);
        assert_eq!(names(&w), ["c", "a", "b"]);
        assert_eq!(w.key(Key::Delete, PLAIN), CombineOutcome::Redraw);
        assert_eq!(names(&w), ["c", "b"]);
        assert_eq!(w.key(Key::Enter, PLAIN), CombineOutcome::Combine);
        assert_eq!(w.key(Key::Escape, PLAIN), CombineOutcome::Close);
        // Une liste vide ne se combine pas.
        let mut empty = CombineWindow::new();
        assert_eq!(empty.key(Key::Enter, PLAIN), CombineOutcome::None);
        assert_eq!(empty.key(Key::Delete, PLAIN), CombineOutcome::None);
    }

    #[test]
    fn tab_parcourt_la_fenetre_et_espace_coche() {
        let mut w = window(&["a"]);
        let mut seen = Vec::new();
        for _ in 0..8 {
            w.key(Key::Tab, PLAIN);
            seen.push(w.focus);
        }
        assert_eq!(w.focus, Hit::List, "le tour revient à la liste");
        assert!(seen.contains(&Hit::Bookmarks) && seen.contains(&Hit::Cancel));
        w.focus = Hit::Forms;
        assert_eq!(w.key(Key::Space, PLAIN), CombineOutcome::Redraw);
        assert!(!w.keep_forms);
        w.focus = Hit::Add;
        assert_eq!(w.key(Key::Enter, PLAIN), CombineOutcome::AddFiles);
        w.focus = Hit::Cancel;
        assert_eq!(w.key(Key::Enter, PLAIN), CombineOutcome::Close);
    }

    /// Les zones d'une liste de trois lignes de 40 pixels, comme le dessin
    /// les relève.
    fn with_rows(w: &mut CombineWindow) {
        w.hits = vec![
            ((200, 0, 20, 20), Hit::Remove(0)),
            ((0, 0, 240, 40), Hit::Row(0)),
            ((0, 40, 240, 40), Hit::Row(1)),
            ((0, 80, 240, 40), Hit::Row(2)),
            ((0, 200, 80, 30), Hit::Combine),
        ];
    }

    #[test]
    fn glisser_une_ligne_la_deplace() {
        let mut w = window(&["a", "b", "c"]);
        with_rows(&mut w);
        // La première ligne, déposée dans la moitié basse de la dernière.
        w.mouse_down(10, 10);
        assert_eq!(w.selected(), Some(0));
        assert!(w.mouse_move(10, 60, true));
        assert!(w.mouse_move(10, 110, true));
        assert_eq!(w.drag, Some((0, 3)));
        assert_eq!(w.mouse_up(10, 110), CombineOutcome::Redraw);
        assert_eq!(names(&w), ["b", "c", "a"]);
        // La dernière, remontée dans la moitié haute de la première.
        w.mouse_down(10, 90);
        w.mouse_move(10, 30, true);
        w.mouse_move(10, 5, true);
        w.mouse_up(10, 5);
        assert_eq!(names(&w), ["a", "b", "c"]);
        // Un appui sans glisser ne déplace rien.
        w.mouse_down(10, 50);
        w.mouse_move(10, 52, true);
        assert_eq!(w.mouse_up(10, 52), CombineOutcome::None);
        assert_eq!(names(&w), ["a", "b", "c"]);
        assert_eq!(w.selected(), Some(1));
    }

    #[test]
    fn les_boutons_agissent_au_relachement_et_la_croix_retire() {
        let mut w = window(&["a", "b"]);
        with_rows(&mut w);
        assert_eq!(w.mouse_down(10, 210), CombineOutcome::Redraw);
        assert_eq!(w.mouse_up(10, 212), CombineOutcome::Combine);
        // Glissé hors du bouton : on s'est ravisé.
        w.mouse_down(10, 210);
        assert_eq!(w.mouse_up(10, 150), CombineOutcome::Redraw);
        // La croix passe avant la ligne qui la porte.
        w.mouse_down(205, 5);
        assert_eq!(w.mouse_up(205, 5), CombineOutcome::Redraw);
        assert_eq!(names(&w), ["b"]);
    }

    #[test]
    fn le_survol_ne_repeint_que_s_il_change() {
        let mut w = window(&["a", "b"]);
        with_rows(&mut w);
        assert!(w.mouse_move(10, 10, false));
        assert!(!w.mouse_move(12, 14, false), "même ligne");
        assert!(w.mouse_move(10, 50, false));
        assert!(w.mouse_move(500, 500, false), "hors de tout");
        assert!(!w.mouse_move(501, 500, false));
    }

    #[test]
    fn pendant_la_combinaison_les_gestes_sont_ignores() {
        let mut w = window(&["a", "b"]);
        with_rows(&mut w);
        w.set_busy(true);
        assert_eq!(w.key(Key::Escape, PLAIN), CombineOutcome::None);
        assert_eq!(w.key(Key::Delete, PLAIN), CombineOutcome::None);
        assert_eq!(w.mouse_down(10, 210), CombineOutcome::None);
        assert_eq!(w.mouse_up(10, 210), CombineOutcome::None);
        assert!(!w.mouse_move(10, 50, false));
        assert_eq!(names(&w), ["a", "b"]);
        w.set_busy(false);
        assert_eq!(w.key(Key::Escape, PLAIN), CombineOutcome::Close);
    }
}
