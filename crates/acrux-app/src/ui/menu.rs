//! Menu contextuel : la liste qui s'ouvre au clic droit, sous le pointeur.
//!
//! C'est la primitive commune à tous les menus de l'application — la page,
//! une vignette, un onglet, un document récent de l'accueil aujourd'hui ; un
//! commentaire ou un champ de formulaire demain. Elle ne sait rien de ce
//! qu'elle propose : chaque élément porte une action `T`, que le menu rend
//! telle quelle quand on la choisit (voir [`Outcome`]). Exécuter revient à
//! l'appelant — qui passe par la même commande que la palette et les
//! raccourcis, pour qu'une action ne se comporte jamais de deux façons.
//!
//! Il se conduit comme un menu de Windows, parce que c'est ce que la main
//! attend :
//!
//! - il s'ouvre **au pointeur**, vers la droite et le bas, et bascule à
//!   gauche ou en haut quand la place manque : il ne sort jamais de la
//!   fenêtre ;
//! - un élément grisé se lit mais ne se choisit pas ; les flèches le
//!   sautent, comme les séparateurs ;
//! - on choisit **au relâchement** : on peut enfoncer le bouton droit,
//!   glisser jusqu'à l'élément et lâcher. Un clic dehors referme sans rien
//!   faire ;
//! - au clavier : flèches (en boucle), Origine, Fin, Entrée ou Espace, Échap.
//!   L'initiale d'un élément le choisit ; quand plusieurs commencent par la
//!   même lettre, elle passe de l'un à l'autre.
//!
//! Il apparaît d'un coup, sans fondu : un menu qu'on attend, même un dixième
//! de seconde, paraît lent. Rien à animer, donc rien à réveiller.

// Coordonnées d'écran entières, mesures de texte fractionnaires, et une
// géométrie de rectangles (x, y, w, h) lue à plat.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]

use acrux_graphics::Rasterizer;

use crate::platform::{Frame, Key};
use crate::ui::icons::{self, Icon};
use crate::ui::paint::{round_rect, round_rect_outline, shadow, Rgb};
use crate::ui::palette::fold_char;
pub use crate::ui::pickers::Outcome;
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Rectangle `(x, y, largeur, hauteur)`.
type Rect = (i32, i32, i32, i32);

// Dimensions, en pixels logiques (multipliées par l'échelle de l'écran).

/// Marge entre le bord de la carte et la première ligne.
const PAD: f32 = 5.0;
/// Hauteur d'un élément.
const ROW: f32 = 30.0;
/// Hauteur d'un séparateur : un trait fin au milieu.
const SEPARATOR: f32 = 9.0;
/// Retrait du contenu d'une ligne par rapport au bord de la carte.
const INSET: f32 = 14.0;
/// Colonne des icônes, réservée dès qu'un élément en a une : les libellés
/// restent alignés, qu'ils aient une icône ou non.
const ICON_COLUMN: f32 = 28.0;
/// Côté d'une icône.
const ICON: f32 = 16.0;
/// Écart minimal entre le plus long libellé et la colonne des raccourcis.
const GAP: f32 = 32.0;
/// Largeur minimale de la carte : un menu de trois mots courts ne doit pas
/// ressembler à une info-bulle.
const MIN_WIDTH: f32 = 200.0;
/// Distance gardée aux bords de la fenêtre.
const MARGIN: f32 = 8.0;
/// Décalage de la carte par rapport au pointeur : la pointe de la flèche
/// ne cache pas le premier élément.
const OFFSET: f32 = 2.0;
/// Déplacement au-delà duquel le pointeur a « bougé » depuis l'ouverture :
/// un relâchement ne choisit qu'après un vrai geste.
const MOVE: f32 = 4.0;
/// Rayon des coins de la carte.
const RADIUS: f32 = 8.0;
/// Taille du raccourci, relative à celle du libellé.
const SHORTCUT_SCALE: f32 = 0.92;

/// Un élément choisissable.
struct Item<T> {
    icon: Option<Icon>,
    label: String,
    shortcut: String,
    action: T,
    enabled: bool,
}

/// Une ligne du menu.
enum Entry<T> {
    Item(Item<T>),
    Separator,
}

/// Un menu contextuel ouvert.
pub struct Menu<T: Copy> {
    entries: Vec<Entry<T>>,
    /// Point d'ouverture (le pointeur au clic droit), en coordonnées de la
    /// fenêtre.
    origin: (i32, i32),
    /// La carte, calculée par [`Menu::layout`].
    card: Rect,
    /// Rectangle de chaque ligne, dans l'ordre de `entries`.
    rows: Vec<Rect>,
    /// Élément mis en avant, au clavier ou sous le pointeur.
    highlight: Option<usize>,
    /// Le pointeur s'est éloigné du point d'ouverture.
    moved: bool,
    /// Un bouton a été enfoncé sur la carte : son relâchement choisit.
    pressed: bool,
    /// Taille du texte (pixels physiques) et échelle, relevées par `layout`.
    size: f32,
    dpi: f32,
    /// Abscisse des libellés, relative au bord gauche de la carte.
    label_x: i32,
}

impl<T: Copy> Menu<T> {
    /// Menu vide, qui s'ouvrira au point `(x, y)` de la fenêtre.
    #[must_use]
    pub fn new(x: i32, y: i32) -> Self {
        Self {
            entries: Vec::new(),
            origin: (x, y),
            card: (x, y, 0, 0),
            rows: Vec::new(),
            highlight: None,
            moved: false,
            pressed: false,
            size: 13.0,
            dpi: 1.0,
            label_x: 0,
        }
    }

    /// Ajoute un élément : icône facultative, libellé, raccourci (vide s'il
    /// n'y en a pas), action rendue quand on le choisit, et s'il est
    /// choisissable.
    #[must_use]
    pub fn item(
        mut self,
        icon: Option<Icon>,
        label: &str,
        shortcut: &str,
        action: T,
        enabled: bool,
    ) -> Self {
        self.entries.push(Entry::Item(Item {
            icon,
            label: label.to_string(),
            shortcut: shortcut.to_string(),
            action,
            enabled,
        }));
        self
    }

    /// Ajoute un séparateur. Il n'en apparaît jamais deux de suite, ni en
    /// tête, ni en queue : un menu construit par morceaux, dont certains
    /// sont vides selon le contexte, reste ainsi toujours propre.
    #[must_use]
    pub fn separator(mut self) -> Self {
        if matches!(self.entries.last(), Some(Entry::Item(_))) {
            self.entries.push(Entry::Separator);
        }
        self
    }

    /// Vrai si le menu n'a aucun élément : il n'y a rien à ouvrir.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.entries.iter().any(|e| matches!(e, Entry::Item(_)))
    }

    /// Point d'ouverture, en coordonnées de la fenêtre.
    #[must_use]
    pub fn origin(&self) -> (i32, i32) {
        self.origin
    }

    /// Met en avant le premier élément choisissable : c'est ainsi que
    /// s'ouvre un menu appelé au clavier, prêt pour Entrée.
    pub fn select_first(&mut self) {
        self.highlight = self.first_active();
    }

    /// Calcule la carte et ses lignes pour une fenêtre `win_w` × `win_h`.
    ///
    /// La mesure du texte est passée en fermeture (taille en pixels, texte)
    /// pour que la géométrie se vérifie sans police système. À rappeler
    /// quand la fenêtre change de taille : la carte s'y recale.
    pub fn layout(
        &mut self,
        measure: &mut dyn FnMut(f32, &str) -> f32,
        font: f32,
        dpi: f32,
        win_w: i32,
        win_h: i32,
    ) {
        while matches!(self.entries.last(), Some(Entry::Separator)) {
            self.entries.pop();
        }
        let s = |v: f32| (v * dpi).round() as i32;
        let size = font * dpi;
        let icons = self
            .entries
            .iter()
            .any(|e| matches!(e, Entry::Item(i) if i.icon.is_some()));
        let (mut label_w, mut shortcut_w) = (0.0_f32, 0.0_f32);
        for entry in &self.entries {
            if let Entry::Item(item) = entry {
                label_w = label_w.max(measure(size, &item.label));
                if !item.shortcut.is_empty() {
                    shortcut_w = shortcut_w.max(measure(size * SHORTCUT_SCALE, &item.shortcut));
                }
            }
        }
        let label_x = s(INSET) + if icons { s(ICON_COLUMN) } else { 0 };
        let shortcuts = if shortcut_w > 0.0 {
            s(GAP) + shortcut_w.ceil() as i32
        } else {
            0
        };
        let width = (label_x + label_w.ceil() as i32 + shortcuts + s(INSET)).max(s(MIN_WIDTH));
        let heights: Vec<i32> = self
            .entries
            .iter()
            .map(|e| match e {
                Entry::Item(_) => s(ROW),
                Entry::Separator => s(SEPARATOR),
            })
            .collect();
        let height = 2 * s(PAD) + heights.iter().sum::<i32>();
        let x = place(self.origin.0, width, win_w, s(OFFSET), s(MARGIN));
        let y = place(self.origin.1, height, win_h, s(OFFSET), s(MARGIN));
        self.card = (x, y, width, height);
        self.rows.clear();
        let mut ry = y + s(PAD);
        for h in heights {
            self.rows.push((x + s(4.0), ry, width - s(8.0), h));
            ry += h;
        }
        self.size = size;
        self.dpi = dpi;
        self.label_x = label_x;
    }

    /// Vrai si le point est sur la carte.
    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        inside(self.card, x, y)
    }

    /// Ligne sous un point, séparateurs compris.
    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        self.rows.iter().position(|&r| inside(r, x, y))
    }

    /// Action de l'élément `index`, s'il est choisissable.
    fn pick(&self, index: usize) -> Option<T> {
        match self.entries.get(index) {
            Some(Entry::Item(item)) if item.enabled => Some(item.action),
            _ => None,
        }
    }

    fn active(&self, index: usize) -> bool {
        self.pick(index).is_some()
    }

    fn first_active(&self) -> Option<usize> {
        (0..self.entries.len()).find(|&i| self.active(i))
    }

    fn last_active(&self) -> Option<usize> {
        (0..self.entries.len()).rev().find(|&i| self.active(i))
    }

    /// Bouton enfoncé : dehors, le menu se referme ; dessus, il attend le
    /// relâchement.
    pub fn mouse_down(&mut self, x: i32, y: i32) -> Outcome<T> {
        if !self.contains(x, y) {
            return Outcome::Close;
        }
        self.pressed = true;
        if let Some(i) = self.row_at(x, y).filter(|&i| self.active(i)) {
            self.highlight = Some(i);
        }
        Outcome::Stay
    }

    /// Bouton relâché : choisit l'élément sous le pointeur, pourvu qu'il y
    /// ait eu un geste — un appui sur la carte, ou un déplacement depuis
    /// l'ouverture. Le relâchement du clic droit qui vient d'ouvrir le menu
    /// ne choisit donc jamais rien, même si la carte, recalée au bord de la
    /// fenêtre, se trouve sous le pointeur.
    pub fn mouse_up(&mut self, x: i32, y: i32) -> Outcome<T> {
        let armed = std::mem::take(&mut self.pressed) || self.moved;
        if !armed {
            return Outcome::Stay;
        }
        self.row_at(x, y)
            .and_then(|i| self.pick(i))
            .map_or(Outcome::Stay, Outcome::Pick)
    }

    /// Déplacement du pointeur ; vrai si l'aspect a changé (il faut alors
    /// repeindre, et seulement alors).
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let reach = (MOVE * self.dpi.max(1.0)).round() as i32;
        let (ox, oy) = self.origin;
        if (x - ox).abs() > reach || (y - oy).abs() > reach {
            self.moved = true;
        }
        // Sur la carte, la mise en avant suit le pointeur (rien sur un
        // séparateur ni sur un élément grisé) ; dehors, celle du clavier
        // reste où elle est.
        if !self.contains(x, y) {
            return false;
        }
        let over = self.row_at(x, y).filter(|&i| self.active(i));
        let changed = over != self.highlight;
        self.highlight = over;
        changed
    }

    /// Touche : flèches, Origine, Fin, Entrée, Espace, Échap. Le reste
    /// n'a pas d'effet — mais n'atteint pas non plus ce qui est dessous.
    pub fn key(&mut self, key: Key) -> Outcome<T> {
        match key {
            Key::Escape | Key::ContextMenu => return Outcome::Close,
            Key::Down => self.step(true),
            Key::Up => self.step(false),
            Key::Home | Key::PageUp => self.highlight = self.first_active(),
            Key::End | Key::PageDown => self.highlight = self.last_active(),
            Key::Enter | Key::Space => {
                return self
                    .highlight
                    .and_then(|i| self.pick(i))
                    .map_or(Outcome::Stay, Outcome::Pick);
            }
            _ => {}
        }
        Outcome::Stay
    }

    /// Caractère tapé : l'initiale d'un élément. Seul à la porter, il est
    /// choisi ; à plusieurs, la mise en avant passe au suivant.
    pub fn char(&mut self, c: char) -> Outcome<T> {
        let want = fold_char(c);
        if !want.is_alphanumeric() {
            return Outcome::Stay;
        }
        let matching: Vec<usize> = (0..self.entries.len())
            .filter(|&i| self.active(i) && self.initial(i) == Some(want))
            .collect();
        match matching.as_slice() {
            [] => Outcome::Stay,
            [only] => self.pick(*only).map_or(Outcome::Stay, Outcome::Pick),
            several => {
                let next = several
                    .iter()
                    .copied()
                    .find(|&i| Some(i) > self.highlight)
                    .or_else(|| several.first().copied());
                self.highlight = next;
                Outcome::Stay
            }
        }
    }

    /// Première lettre d'un libellé, pliée (minuscule, sans accent).
    fn initial(&self, index: usize) -> Option<char> {
        match self.entries.get(index) {
            Some(Entry::Item(item)) => item.label.chars().next().map(fold_char),
            _ => None,
        }
    }

    /// Passe à l'élément choisissable suivant ou précédent, en boucle.
    fn step(&mut self, forward: bool) {
        let active: Vec<usize> = (0..self.entries.len())
            .filter(|&i| self.active(i))
            .collect();
        let at = self
            .highlight
            .and_then(|h| active.iter().position(|&i| i == h));
        let len = active.len();
        self.highlight = match at {
            _ if len == 0 => None,
            None if forward => active.first().copied(),
            None => active.last().copied(),
            Some(p) if forward => active.get((p + 1) % len).copied(),
            Some(p) => active.get((p + len - 1) % len).copied(),
        };
    }

    /// Dessine le menu, par-dessus tout ce qui est déjà peint. `layout`
    /// doit avoir été appelé pour la taille courante de la fenêtre.
    pub fn paint(
        &self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
    ) {
        let (x, y, w, h) = self.card;
        if w <= 0 || h <= 0 {
            return;
        }
        let dpi = self.dpi;
        let s = |v: f32| (v * dpi).round() as i32;
        let radius = RADIUS * dpi;
        shadow(frame, x, y + s(3.0), w, h, radius, 18.0 * dpi, 0.4);
        round_rect(frame, x, y, w, h, radius, theme.bar);
        round_rect_outline(frame, x, y, w, h, radius, dpi.max(1.0), theme.separator);
        // Un élément grisé : entre le texte secondaire et le fond, lisible
        // mais visiblement hors d'usage.
        let faint = mix(theme.text_dim, theme.bar);
        let size = self.size;
        let small = size * SHORTCUT_SCALE;
        let right = (x + w - s(INSET)) as f32;
        for (i, (entry, &(rx, ry, rw, rh))) in self.entries.iter().zip(&self.rows).enumerate() {
            let item = match entry {
                Entry::Separator => {
                    let (r, g, b) = theme.separator;
                    frame.fill_rect(x + s(10.0), ry + rh / 2, w - s(20.0), 1, r, g, b);
                    continue;
                }
                Entry::Item(item) => item,
            };
            if self.highlight == Some(i) {
                round_rect(frame, rx, ry, rw, rh, 5.0 * dpi, theme.hover);
            }
            let color = if item.enabled { theme.text } else { faint };
            if let Some(icon) = item.icon {
                let icon_px = s(ICON);
                icons::draw(
                    frame,
                    raster,
                    icon,
                    x + s(INSET),
                    ry + (rh - icon_px) / 2,
                    ICON * dpi,
                    color,
                );
            }
            let baseline = ry as f32 + f32::midpoint(rh as f32, text.ascent(size)) - 1.0;
            let shortcut_w = if item.shortcut.is_empty() {
                0.0
            } else {
                text.measure(small, &item.shortcut)
            };
            let label_x = (x + self.label_x) as f32;
            let room = if shortcut_w > 0.0 {
                right - shortcut_w - GAP * dpi - label_x
            } else {
                right - label_x
            };
            text.draw_clipped(frame, label_x, baseline, size, &item.label, color, room);
            if shortcut_w > 0.0 {
                let dim = if item.enabled { theme.text_dim } else { faint };
                text.draw(
                    frame,
                    right - shortcut_w,
                    baseline,
                    small,
                    &item.shortcut,
                    dim,
                );
            }
        }
    }
}

/// Position d'un côté de la carte le long d'un axe : après le pointeur s'il
/// y a la place, avant sinon, et toujours dans la fenêtre.
fn place(pointer: i32, extent: i32, window: i32, offset: i32, margin: i32) -> i32 {
    let mut start = pointer + offset;
    if start + extent > window - margin {
        start = pointer - offset - extent;
    }
    start.min(window - margin - extent).max(margin)
}

fn inside(r: Rect, x: i32, y: i32) -> bool {
    x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
}

/// Moyenne de deux couleurs.
fn mix(a: Rgb, b: Rgb) -> Rgb {
    (
        u8::midpoint(a.0, b.0),
        u8::midpoint(a.1, b.1),
        u8::midpoint(a.2, b.2),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mesure factice : un demi-cadratin par caractère. Les épreuves de
    /// géométrie n'ont ainsi besoin d'aucune police du système.
    fn measure(size: f32, text: &str) -> f32 {
        text.chars().count() as f32 * size * 0.5
    }

    /// [A, —, B grisé, C], ouvert en (100, 100) dans une fenêtre 800 × 600.
    fn sample() -> Menu<char> {
        let mut m = Menu::new(100, 100)
            .item(None, "Alpha", "Ctrl+A", 'a', true)
            .separator()
            .item(None, "Bravo", "", 'b', false)
            .item(Some(Icon::Rotate), "Charlie", "", 'c', true);
        m.layout(&mut measure, 13.0, 1.0, 800, 600);
        m
    }

    /// Centre de la ligne `index`.
    fn center(m: &Menu<char>, index: usize) -> (i32, i32) {
        let (x, y, w, h) = m.rows[index];
        (x + w / 2, y + h / 2)
    }

    #[test]
    fn le_menu_se_recale_dans_la_fenetre() {
        let mut m = Menu::new(790, 590)
            .item(None, "Un élément assez long", "Ctrl+Maj+E", 1, true)
            .item(None, "Deux", "", 2, true);
        m.layout(&mut measure, 13.0, 1.5, 800, 600);
        let (x, y, w, h) = m.card;
        assert!(
            x >= 8 && x + w <= 792,
            "carte hors de la fenêtre : {:?}",
            m.card
        );
        assert!(
            y >= 8 && y + h <= 592,
            "carte hors de la fenêtre : {:?}",
            m.card
        );
        // Elle a basculé à gauche et en haut du pointeur.
        assert!(x + w <= 790 && y + h <= 590);

        let mut m = Menu::new(0, 0).item(None, "Un", "", 1, true);
        m.layout(&mut measure, 13.0, 1.0, 800, 600);
        assert!(m.card.0 >= 8 && m.card.1 >= 8);

        // Au milieu de la fenêtre, elle s'ouvre à droite et sous le pointeur.
        let m = sample();
        assert_eq!((m.card.0, m.card.1), (102, 102));
    }

    #[test]
    fn la_largeur_tient_le_plus_long_libelle_et_le_raccourci() {
        let mut m = Menu::new(10, 10).item(
            None,
            "Un libellé vraiment très long pour un menu",
            "Ctrl+Maj+S",
            1,
            true,
        );
        m.layout(&mut measure, 13.0, 1.0, 2000, 600);
        let label = measure(13.0, "Un libellé vraiment très long pour un menu");
        let shortcut = measure(13.0 * SHORTCUT_SCALE, "Ctrl+Maj+S");
        assert!(m.card.2 as f32 >= label + shortcut + GAP + 2.0 * INSET);
        // Sans rien de long, la largeur minimale.
        assert_eq!(sample().card.2, 200);
    }

    #[test]
    fn les_fleches_sautent_separateurs_et_elements_grises() {
        let mut m = sample();
        assert_eq!(m.key(Key::Down), Outcome::Stay);
        assert_eq!(m.highlight, Some(0), "le premier élément");
        m.key(Key::Down);
        assert_eq!(m.highlight, Some(3), "séparateur et « Bravo » grisé sautés");
        m.key(Key::Down);
        assert_eq!(m.highlight, Some(0), "en boucle");
        m.key(Key::Up);
        assert_eq!(m.highlight, Some(3));
        m.key(Key::Home);
        assert_eq!(m.highlight, Some(0));
        m.key(Key::End);
        assert_eq!(m.highlight, Some(3));

        let mut fresh = sample();
        fresh.key(Key::Up);
        assert_eq!(fresh.highlight, Some(3), "Haut part de la fin");
    }

    #[test]
    fn entree_choisit_echap_ferme() {
        let mut m = sample();
        assert_eq!(m.key(Key::Enter), Outcome::Stay, "rien de mis en avant");
        m.key(Key::End);
        assert_eq!(m.key(Key::Enter), Outcome::Pick('c'));
        assert_eq!(m.key(Key::Space), Outcome::Pick('c'));
        assert_eq!(m.key(Key::Escape), Outcome::Close);
        assert_eq!(m.key(Key::ContextMenu), Outcome::Close);
        assert_eq!(m.key(Key::Tab), Outcome::Stay);
    }

    #[test]
    fn un_clic_dehors_ferme_un_clic_sur_un_grise_ne_fait_rien() {
        let mut m = sample();
        assert_eq!(m.mouse_down(700, 500), Outcome::Close);

        let (bx, by) = center(&m, 2);
        assert_eq!(m.mouse_down(bx, by), Outcome::Stay);
        assert_eq!(m.mouse_up(bx, by), Outcome::Stay, "« Bravo » est grisé");

        let (sx, sy) = center(&m, 1);
        assert_eq!(m.mouse_down(sx, sy), Outcome::Stay);
        assert_eq!(m.mouse_up(sx, sy), Outcome::Stay, "un séparateur");

        let (ax, ay) = center(&m, 0);
        assert_eq!(m.mouse_down(ax, ay), Outcome::Stay, "l'appui attend");
        assert_eq!(
            m.mouse_up(ax, ay),
            Outcome::Pick('a'),
            "le relâchement choisit"
        );
    }

    #[test]
    fn relacher_sans_bouger_ne_choisit_pas() {
        let mut m = sample();
        let (cx, cy) = center(&m, 3);
        // Le relâchement du clic droit qui a ouvert le menu : aucun geste.
        assert_eq!(m.mouse_up(cx, cy), Outcome::Stay);
        // Un frémissement de la main n'est pas un geste.
        assert!(!m.mouse_move(102, 101));
        assert_eq!(m.mouse_up(cx, cy), Outcome::Stay);
        // Glisser jusqu'à « Charlie », puis lâcher : choisi.
        assert!(m.mouse_move(cx, cy), "la mise en avant suit le pointeur");
        assert_eq!(m.highlight, Some(3));
        assert_eq!(m.mouse_up(cx, cy), Outcome::Pick('c'));
    }

    #[test]
    fn le_survol_ignore_grises_et_separateurs() {
        let mut m = sample();
        let (bx, by) = center(&m, 2);
        m.key(Key::Home);
        assert!(m.mouse_move(bx, by), "la mise en avant quitte « Alpha »");
        assert_eq!(m.highlight, None);
        assert!(!m.mouse_move(bx, by + 1), "rien n'a changé");
        // Hors de la carte, la mise en avant du clavier reste.
        m.key(Key::Home);
        assert!(!m.mouse_move(700, 500));
        assert_eq!(m.highlight, Some(0));
    }

    #[test]
    fn l_initiale_choisit_l_element_unique() {
        let mut m = sample();
        assert_eq!(m.char('c'), Outcome::Pick('c'));
        assert_eq!(m.char('C'), Outcome::Pick('c'), "la casse ne compte pas");
        assert_eq!(m.char('b'), Outcome::Stay, "« Bravo » est grisé");
        assert_eq!(m.char('z'), Outcome::Stay);

        let mut m = Menu::new(10, 10)
            .item(None, "Pivoter", "", 1, true)
            .item(None, "Poser une note", "", 2, true)
            .item(None, "Étendre", "", 3, true);
        m.layout(&mut measure, 13.0, 1.0, 800, 600);
        assert_eq!(m.char('p'), Outcome::Stay, "deux éléments en « p »");
        assert_eq!(m.highlight, Some(0));
        m.char('p');
        assert_eq!(m.highlight, Some(1));
        m.char('p');
        assert_eq!(m.highlight, Some(0), "en boucle");
        assert_eq!(m.char('e'), Outcome::Pick(3), "l'accent ne compte pas");
    }

    #[test]
    fn les_separateurs_superflus_disparaissent() {
        let mut m = Menu::new(10, 10)
            .separator()
            .item(None, "A", "", 1, true)
            .separator()
            .separator()
            .item(None, "B", "", 2, true)
            .separator();
        m.layout(&mut measure, 13.0, 1.0, 800, 600);
        assert_eq!(m.entries.len(), 3, "A, séparateur, B");
        assert_eq!(m.card.3, 2 * 5 + 30 + 9 + 30);
        assert!(Menu::<u8>::new(0, 0).separator().is_empty());
        assert!(!m.is_empty());
    }

    #[test]
    fn la_peinture_reste_dans_la_carte() {
        let Some(mut text) = TextRenderer::system() else {
            return;
        };
        let mut raster = Rasterizer::new();
        for dpi in [1.0_f32, 1.5, 2.0] {
            let (fw, fh) = (400_u32, 400_u32);
            let mut pixels = vec![7_u8; (fw * fh * 4) as usize];
            let mut m = Menu::new(20, 20)
                .item(Some(Icon::Print), "Imprimer…", "Ctrl+P", 1, true)
                .separator()
                .item(None, "Propriétés du document", "Ctrl+D", 2, false);
            m.layout(
                &mut |s, t| text.measure(s, t),
                13.0,
                dpi,
                fw as i32,
                fh as i32,
            );
            m.key(Key::Home);
            let mut frame = Frame::new(fw, fh, &mut pixels);
            m.paint(&mut frame, &mut text, &mut raster, &Theme::dark());
            // Rien n'est peint au-delà de la carte et de son ombre.
            let (x, y, w, h) = m.card;
            let spread = (18.0 * dpi) as i32 + (3.0 * dpi) as i32 + 2;
            for py in 0..fh as i32 {
                for px in 0..fw as i32 {
                    let near = px >= x - spread
                        && px < x + w + spread
                        && py >= y - spread
                        && py < y + h + spread;
                    let i = ((py * fw as i32 + px) * 4) as usize;
                    if !near {
                        assert_eq!(pixels[i..i + 3], [7, 7, 7], "pixel ({px}, {py}) à {dpi}");
                    }
                }
            }
            // Et la carte elle-même a bien été peinte.
            let i = (((y + h / 2) * fw as i32 + x + w / 2) * 4) as usize;
            assert_ne!(pixels[i..i + 3], [7, 7, 7]);
        }
    }
}
