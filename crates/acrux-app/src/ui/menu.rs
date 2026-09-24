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
//!
//! Le même menu sert de **liste déroulante** : ouvert par [`Menu::below`], il
//! se place sous un bouton (au-dessus s'il manque de place, pour un bouton de
//! la barre d'état) au lieu du pointeur, et peut réserver en tête une bande
//! où l'appelant dessine un champ de saisie ([`Menu::header`]) — c'est la
//! liste du zoom, où l'on choisit un niveau ou l'on tape le sien. C'est aussi
//! la liste d'un champ de formulaire à choix, qui peut compter des dizaines
//! d'options : une liste plus haute que la place disponible **défile**, à la
//! molette ([`Menu::wheel`]) comme au clavier, avec un ascenseur fin.

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
use crate::ui::paint::{round_rect, round_rect_alpha, round_rect_outline, shadow, Rgb};
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
/// Retrait de la bande de tête par rapport aux bords de la carte.
const HEAD_INSET: f32 = 8.0;
/// Écart entre la bande de tête et la première ligne.
const HEAD_GAP: f32 = 4.0;
/// Écart entre une liste déroulante et le bouton qui l'a ouverte.
const DROP_GAP: f32 = 4.0;

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
    /// Bouton sous lequel s'ouvre une liste déroulante (voir
    /// [`Menu::below`]) ; `None` pour un menu ouvert au pointeur.
    below: Option<Rect>,
    /// Hauteur de la bande réservée en tête, en pixels logiques (0 : pas
    /// de bande).
    header: f32,
    /// La bande de tête, calculée par [`Menu::layout`].
    head: Rect,
    /// Partie de la carte où les lignes se montrent : toute la hauteur des
    /// lignes, ou moins quand la liste défile.
    viewport: Rect,
    /// Hauteur totale des lignes.
    content_h: i32,
    /// Défilement des lignes, en pixels.
    scroll: i32,
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
            below: None,
            header: 0.0,
            head: (x, y, 0, 0),
            viewport: (x, y, 0, 0),
            content_h: 0,
            scroll: 0,
        }
    }

    /// Liste déroulante vide, qui s'ouvrira **sous** le bouton `anchor`,
    /// calée sur son bord gauche et au moins aussi large que lui ; au-dessus
    /// s'il n'y a pas la place dessous (un bouton de la barre d'état).
    #[must_use]
    pub fn below(anchor: Rect) -> Self {
        let (x, y, w, h) = anchor;
        let mut menu = Self::new(x + w / 2, y + h / 2);
        menu.below = Some(anchor);
        menu
    }

    /// Réserve en tête de la carte une bande de `height` pixels logiques,
    /// que l'appelant remplit lui-même (voir [`Menu::head`]).
    #[must_use]
    pub fn header(mut self, height: f32) -> Self {
        self.header = height.max(0.0);
        self
    }

    /// La bande de tête, en coordonnées de la fenêtre (largeur nulle s'il
    /// n'y en a pas). Valable après [`Menu::layout`].
    #[must_use]
    pub fn head(&self) -> Rect {
        self.head
    }

    /// Met en avant le premier élément choisissable dont l'action vérifie
    /// `wanted` : une liste déroulante s'ouvre sur la valeur en cours.
    pub fn highlight_where(&mut self, wanted: impl Fn(T) -> bool) {
        if let Some(i) = (0..self.entries.len()).find(|&i| self.pick(i).is_some_and(&wanted)) {
            self.highlight = Some(i);
            self.reveal();
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
        let anchor_w = self.below.map_or(0, |b| b.2);
        let width = (label_x + label_w.ceil() as i32 + shortcuts + s(INSET))
            .max(s(MIN_WIDTH))
            .max(anchor_w);
        let heights: Vec<i32> = self
            .entries
            .iter()
            .map(|e| match e {
                Entry::Item(_) => s(ROW),
                Entry::Separator => s(SEPARATOR),
            })
            .collect();
        // La bande de tête pousse les lignes vers le bas ; sans elle, la
        // première ligne est à `PAD` du bord, comme avant.
        let head_h = s(self.header);
        let rows_top = if head_h > 0 {
            s(HEAD_INSET) + head_h + s(HEAD_GAP)
        } else {
            s(PAD)
        };
        let content_h = heights.iter().sum::<i32>();
        let full = rows_top + s(PAD) + content_h;
        // La place qu'a la carte : sous le bouton ou au-dessus, le plus
        // grand des deux, ou toute la fenêtre pour un menu du pointeur. Plus
        // haute, elle défile — en montrant toujours au moins trois lignes.
        let room = match self.below {
            Some((_, ay, _, ah)) => {
                let under = win_h - s(MARGIN) - (ay + ah + s(DROP_GAP));
                let over = ay - s(DROP_GAP) - s(MARGIN);
                under.max(over)
            }
            None => win_h - 2 * s(MARGIN),
        };
        let height = full.min(room.max(rows_top + s(PAD) + 3 * s(ROW)));
        let (x, y) = match self.below {
            Some(anchor) => drop_down(
                anchor,
                (width, height),
                (win_w, win_h),
                s(DROP_GAP),
                s(MARGIN),
            ),
            None => (
                place(self.origin.0, width, win_w, s(OFFSET), s(MARGIN)),
                place(self.origin.1, height, win_h, s(OFFSET), s(MARGIN)),
            ),
        };
        self.card = (x, y, width, height);
        self.head = if head_h > 0 {
            (
                x + s(HEAD_INSET),
                y + s(HEAD_INSET),
                width - 2 * s(HEAD_INSET),
                head_h,
            )
        } else {
            (x, y, 0, 0)
        };
        self.rows.clear();
        let mut ry = y + rows_top;
        for h in heights {
            self.rows.push((x + s(4.0), ry, width - s(8.0), h));
            ry += h;
        }
        self.viewport = (x, y + rows_top, width, height - rows_top - s(PAD));
        self.content_h = content_h;
        self.scroll = self.scroll.clamp(0, self.max_scroll());
        self.size = size;
        self.dpi = dpi;
        self.label_x = label_x;
        self.reveal();
    }

    /// Vrai si le point est sur la carte.
    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        inside(self.card, x, y)
    }

    /// Défilement le plus grand : la dernière ligne au bas de la liste.
    fn max_scroll(&self) -> i32 {
        (self.content_h - self.viewport.3).max(0)
    }

    /// Rectangle d'une ligne tel qu'il se montre, défilement compris.
    fn shown(&self, r: Rect) -> Rect {
        (r.0, r.1 - self.scroll, r.2, r.3)
    }

    /// Ligne sous un point, séparateurs compris. Une ligne sortie de la
    /// liste par le défilement ne se clique pas.
    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        if !inside(self.viewport, x, y) {
            return None;
        }
        self.rows.iter().position(|&r| inside(self.shown(r), x, y))
    }

    /// Fait venir la ligne mise en avant dans la liste, si elle défile.
    fn reveal(&mut self) {
        let Some(&(_, ry, _, rh)) = self.highlight.and_then(|i| self.rows.get(i)) else {
            return;
        };
        let (top, height) = (ry - self.viewport.1, self.viewport.3);
        if top < self.scroll {
            self.scroll = top;
        } else if top + rh > self.scroll + height {
            self.scroll = top + rh - height;
        }
        self.scroll = self.scroll.clamp(0, self.max_scroll());
    }

    /// Molette sur la liste : trois lignes par cran. Vrai si elle a défilé
    /// (il faut alors repeindre) ; une liste qui tient entière ne bouge pas.
    pub fn wheel(&mut self, delta: f32) -> bool {
        let row = (ROW * self.dpi).round();
        let before = self.scroll;
        let by = (delta * row * 3.0).round() as i32;
        self.scroll = (self.scroll - by).clamp(0, self.max_scroll());
        self.scroll != before
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
            Key::Home | Key::PageUp => {
                self.highlight = self.first_active();
                self.reveal();
            }
            Key::End | Key::PageDown => {
                self.highlight = self.last_active();
                self.reveal();
            }
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
                self.reveal();
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
        self.reveal();
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
        // Les lignes se dessinent dans la fenêtre de la liste, décalées du
        // défilement : celles qui en sortent sont coupées à son bord.
        let (vx, vy, vw, vh) = self.viewport;
        let scrolled = self.max_scroll() > 0;
        if scrolled {
            // L'ascenseur : un trait fin sur le bord droit, à la hauteur de
            // ce qu'on voit.
            let thumb_h = (vh * vh / self.content_h.max(1)).max(s(16.0));
            let thumb_y = vy + (vh - thumb_h) * self.scroll / self.max_scroll().max(1);
            round_rect_alpha(
                frame,
                x + w - s(7.0),
                thumb_y,
                s(4.0),
                thumb_h,
                2.0 * dpi,
                theme.text_dim,
                0.6,
            );
        }
        let margin = if scrolled { s(6.0) } else { 0 };
        let right = (x + w - s(INSET) - margin - vx) as f32;
        let mut clip = frame.sub(vx, vy, vw.max(0) as u32, vh.max(0) as u32);
        let frame = &mut clip;
        let (x, dy) = (x - vx, -vy - self.scroll);
        for (i, (entry, &(rx, ry, rw, rh))) in self.entries.iter().zip(&self.rows).enumerate() {
            let (rx, ry) = (rx - vx, ry + dy);
            if ry + rh <= 0 || ry >= vh {
                continue;
            }
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

/// Coin d'une liste déroulante de taille `size` ouverte depuis le bouton
/// `anchor` : sous lui, calée sur son bord gauche ; au-dessus quand la place
/// manque dessous. Elle reste toujours dans la fenêtre `window`.
fn drop_down(
    anchor: Rect,
    size: (i32, i32),
    window: (i32, i32),
    gap: i32,
    margin: i32,
) -> (i32, i32) {
    let (ax, ay, _, ah) = anchor;
    let ((width, height), (win_w, win_h)) = (size, window);
    let x = ax.min(win_w - margin - width).max(margin);
    let under = ay + ah + gap;
    let y = if under + height <= win_h - margin {
        under
    } else {
        ay - gap - height
    };
    (x, y.min(win_h - margin - height).max(margin))
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

    /// Liste déroulante : sous son bouton, calée à gauche, au moins aussi
    /// large que lui ; au-dessus quand elle ne tient pas dessous.
    #[test]
    fn la_liste_deroulante_s_ouvre_sous_ou_au_dessus_de_son_bouton() {
        let bouton = (300, 10, 260, 30);
        let mut m = Menu::below(bouton)
            .item(None, "50 %", "", 50, true)
            .item(None, "100 %", "", 100, true);
        m.layout(&mut measure, 13.0, 1.0, 800, 600);
        let (x, y, w, _) = m.card;
        assert_eq!((x, y), (300, 44), "sous le bouton, calée à gauche");
        assert_eq!(w, 260, "aussi large que le bouton");

        // Un bouton de la barre d'état : la place manque dessous.
        let etat = (600, 575, 60, 20);
        let mut m = Menu::below(etat).item(None, "Un", "", 1, true);
        m.layout(&mut measure, 13.0, 1.0, 800, 600);
        let (x, y, w, h) = m.card;
        assert_eq!(y + h, 571, "au-dessus du bouton, à distance");
        assert!(x + w <= 792, "recalée dans la fenêtre : {:?}", m.card);
    }

    /// La bande de tête prend place au-dessus des lignes, dans la carte ; un
    /// clic dedans ne ferme ni ne choisit rien.
    #[test]
    fn la_bande_de_tete_repousse_les_lignes() {
        let mut m = Menu::below((100, 10, 80, 30))
            .header(32.0)
            .item(None, "Alpha", "", 'a', true);
        m.layout(&mut measure, 13.0, 1.0, 800, 600);
        let (hx, hy, hw, hh) = m.head();
        let (x, y, w, _) = m.card;
        assert_eq!((hx, hy, hw, hh), (x + 8, y + 8, w - 16, 32));
        assert!(m.rows[0].1 >= hy + hh, "la ligne passe sous la bande");
        assert_eq!(m.mouse_down(hx + 5, hy + 5), Outcome::Stay);
        assert_eq!(m.mouse_up(hx + 5, hy + 5), Outcome::Stay);
        // Sans bande, rien de réservé.
        assert_eq!(sample().head().2, 0);
    }

    /// Une liste plus haute que la fenêtre défile : la carte tient dans la
    /// fenêtre, la molette et le clavier font venir les lignes cachées, et
    /// un clic vise la ligne qui se montre sous le pointeur.
    #[test]
    fn une_longue_liste_defile() {
        let mut m = Menu::below((100, 40, 120, 30));
        for i in 0..60_u32 {
            m = m.item(None, &format!("Option {i}"), "", i, true);
        }
        m.layout(&mut measure, 13.0, 1.0, 800, 600);
        let (_, y, _, h) = m.card;
        assert!(y >= 8 && y + h <= 592, "la carte tient : {:?}", m.card);
        assert!(m.max_scroll() > 0, "elle défile");
        let (rx, ry, rw, rh) = m.rows[0];
        let (cx, cy) = (rx + rw / 2, ry + rh / 2);
        assert_eq!(m.row_at(cx, cy), Some(0));
        assert!(m.wheel(-1.0), "un cran vers le bas");
        assert_eq!(m.row_at(cx, cy), Some(3), "trois lignes plus loin");
        assert!(!m.wheel(1.0) || m.scroll == 0);
        m.key(Key::End);
        assert_eq!(m.scroll, m.max_scroll(), "Fin montre la dernière ligne");
        let (_, ry, _, rh) = m.shown(m.rows[59]);
        assert!(ry + rh <= m.viewport.1 + m.viewport.3);
        assert_eq!(m.mouse_down(cx, ry + rh / 2), Outcome::Stay);
        assert_eq!(m.mouse_up(cx, ry + rh / 2), Outcome::Pick(59));
        // Une ligne sortie par le haut ne se clique pas.
        assert_eq!(m.row_at(cx, m.viewport.1 - 1), None);
        // Une liste courte ne défile pas.
        let mut short = Menu::below((100, 40, 120, 30)).item(None, "Un", "", 1_u32, true);
        short.layout(&mut measure, 13.0, 1.0, 800, 600);
        assert!(!short.wheel(-1.0));
    }

    #[test]
    fn la_liste_s_ouvre_sur_la_valeur_en_cours() {
        let mut m = sample();
        m.highlight_where(|c| c == 'c');
        assert_eq!(m.highlight, Some(3));
        // Un élément grisé ne se met pas en avant, même s'il correspond.
        m.highlight_where(|c| c == 'b');
        assert_eq!(m.highlight, Some(3));
        assert_eq!(m.key(Key::Enter), Outcome::Pick('c'));
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
