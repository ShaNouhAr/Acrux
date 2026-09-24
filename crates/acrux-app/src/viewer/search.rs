//! La recherche dans le document : la carte flottante en haut à droite
//! (Ctrl+F), son parcours par tranches, les surlignages, et le remplacement
//! (Ctrl+H).
//!
//! La carte est celle d'Acrobat et des navigateurs : un champ, « Aa »
//! (respecter la casse) et « Mot entier », précédente et suivante, une
//! croix pour fermer, et en dessous « 3 sur 12 » ou « Aucun résultat ».
//! Entrée et F3 vont à l'occurrence suivante, Maj+Entrée et Maj+F3 à la
//! précédente ; F3 rouvre la dernière recherche une fois la carte fermée.
//! Ctrl+F reprend le texte sélectionné sur la page, et, carte ouverte,
//! sélectionne la requête au lieu de l'effacer.
//!
//! Le moteur est [`acrux_features::text::find_matches`] : les surlignages,
//! « Remplacer » et « Tout remplacer » sortent du même parcours, avec les
//! mêmes options. L'occurrence remplacée est celle que l'on voit.
//!
//! Le parcours part de la page affichée et fait le tour du document par
//! tranches de quelques millisecondes : la première occurrence montrée est
//! la première **à partir de ce qu'on regarde**, comme dans Acrobat, et un
//! gros document ne fige jamais l'interface. Les occurrences restent triées
//! par page pendant tout le parcours. Chaque tranche est prise **pendant la
//! peinture**, qui demande un réveil — un seul par image — s'il en reste :
//! un réveil posté depuis la boucle d'événements en réponse à un réveil
//! remplirait la file, et Windows, qui ne repeint que file vide, figerait
//! la fenêtre jusqu'à la fin du parcours.

// Coordonnées d'écran entières, compteurs d'occurrences.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::time::{Duration, Instant};

use acrux_features::text::{find_matches, MatchPiece, SearchOptions};
use acrux_graphics::Rasterizer;
use acrux_render::page::base_matrix;

use super::{fill_rect_blend, log_line, shown_rotation, EditOp, PageBox, Viewer};
use crate::platform::{Cursor, Frame, Key, Modifiers, WindowHandle};
use crate::ui::icons::{self, Icon};
use crate::ui::input::{InputAction, TextInput};
use crate::ui::lang;
use crate::ui::modal::{inside, ButtonRow, Rect, RowButton};
use crate::ui::paint::{round_rect, round_rect_alpha, round_rect_outline};
use crate::ui::text::TextRenderer;
use crate::ui::theme::{Rgb, Theme};

/// Surlignage de l'occurrence courante. Les couleurs des surlignages sont
/// regroupées ici : le mode nuit et les thèmes les reprendront.
const CURRENT_HIT: Rgb = (255, 140, 0);
/// Surlignage des autres occurrences.
const OTHER_HIT: Rgb = (255, 230, 0);
/// Opacité des surlignages (sur 255) : le texte se lit au travers.
const HIT_ALPHA: u32 = 110;
/// Longueur maximale d'une requête reprise de la sélection : au-delà, ce
/// n'est plus une recherche mais un copier-coller égaré.
const MAX_QUERY: usize = 256;
/// Temps de parcours accordé à chaque image.
const SLICE: Duration = Duration::from_millis(12);

/// Où est le focus clavier de la carte de recherche.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchFocus {
    /// Le champ « rechercher ».
    Find,
    /// Le champ « remplacer par ».
    With,
    /// Un bouton : 0 remplace l'occurrence courante, 1 les remplace toutes.
    Button(usize),
}

/// Les petits boutons de la carte, à droite du champ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchButton {
    /// « Aa » : respecter la casse.
    MatchCase,
    /// « ab » souligné : mot entier.
    WholeWord,
    /// Occurrence précédente.
    Prev,
    /// Occurrence suivante.
    Next,
    /// Fermer la carte.
    Close,
}

/// Une occurrence trouvée : sa page et ses morceaux, un par ligne
/// traversée (espace PDF).
pub(super) struct SearchHit {
    /// Page.
    pub(super) page: usize,
    /// Morceaux, un par ligne ; le premier sert au défilement.
    pub(super) pieces: Vec<MatchPiece>,
    /// « Remplacer » peut la réécrire : elle tient sur une ligne et ne coupe
    /// aucune ligature (`TextMatch::editable`).
    pub(super) editable: bool,
}

/// État de la recherche dans le document.
pub(super) struct Search {
    /// Le champ « rechercher ».
    pub(super) input: TextInput,
    /// Champ « remplacer par », quand on l'a demandé (Ctrl+H).
    pub(super) replace: Option<TextInput>,
    /// Élément qui a le focus clavier.
    pub(super) focus: SearchFocus,
    /// Casse et mot entier.
    options: SearchOptions,
    /// Champs dessinés au dernier tour, pour les cliquer.
    fields: Vec<(Rect, SearchFocus)>,
    /// Petits boutons dessinés au dernier tour.
    icons: Vec<(Rect, SearchButton)>,
    /// Petit bouton survolé.
    hover: Option<SearchButton>,
    /// Petit bouton enfoncé, qui agira au relâchement s'il est encore visé.
    pressed: Option<SearchButton>,
    /// « Remplacer », « Tout remplacer » : la rangée commune des cartes.
    pub(super) row: ButtonRow,
    /// Rectangle de la carte au dernier dessin (repère de la vue).
    card: Rect,
    /// Occurrences, triées par page.
    hits: Vec<SearchHit>,
    /// Occurrence courante.
    current: usize,
    /// Requête et options du parcours en cours ; `None` force un nouveau
    /// parcours.
    last: Option<(String, SearchOptions)>,
    /// Nombre de pages déjà parcourues.
    scanned: usize,
    /// Page d'où le parcours est parti : celle qu'on regardait.
    origin: usize,
    /// Place où ranger les occurrences des pages d'avant `origin`, que le
    /// parcours atteint en dernier.
    insert_at: usize,
    /// On s'est rendu à la première occurrence trouvée.
    shown: bool,
    /// Texte de la page déjà repris comme requête : Ctrl+F ne le reprend
    /// qu'une fois, pour ne pas écraser ce qu'on a tapé depuis.
    taken: Option<String>,
    /// La vue a déjà sauté vers une première occurrence depuis l'ouverture
    /// de la carte : l'endroit d'où l'on est parti est dans l'historique de
    /// la vue. Taper « bonjour » y laisse une seule entrée, pas sept.
    jumped: bool,
}

/// Ce que l'on touche en cliquant dans la carte de recherche.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchPart {
    /// Un champ.
    Field(SearchFocus),
    /// Un bouton de la rangée « Remplacer », « Tout remplacer ».
    Row(usize),
    /// Un petit bouton.
    Button(SearchButton),
    /// Le reste de la carte : le clic s'arrête là, il ne va pas à la page.
    Card,
}

impl Search {
    /// Carte neuve : le champ « rechercher » seul, qui a le focus.
    fn new() -> Self {
        Self {
            input: TextInput::new(lang::tr("Rechercher dans le document")),
            replace: None,
            focus: SearchFocus::Find,
            options: SearchOptions::default(),
            fields: Vec::new(),
            icons: Vec::new(),
            hover: None,
            pressed: None,
            row: ButtonRow::default(),
            card: (0, 0, 0, 0),
            hits: Vec::new(),
            current: 0,
            last: None,
            scanned: 0,
            origin: 0,
            insert_at: 0,
            shown: false,
            taken: None,
            jumped: false,
        }
    }

    /// Place le focus ; seul le champ qui l'a montre son caret.
    pub(super) fn set_focus(&mut self, focus: SearchFocus) {
        self.focus = focus;
        self.input.focused = focus == SearchFocus::Find;
        if let Some(r) = &mut self.replace {
            r.focused = focus == SearchFocus::With;
        }
    }

    /// Tab (ou Maj+Tab) : rechercher → remplacer → les boutons, s'il y a de
    /// quoi remplacer → rechercher.
    pub(super) fn tab(&mut self, back: bool) {
        let mut stops = vec![SearchFocus::Find];
        if self.replace.is_some() {
            stops.push(SearchFocus::With);
            if !self.hits.is_empty() {
                stops.push(SearchFocus::Button(0));
                stops.push(SearchFocus::Button(1));
            }
        }
        let n = stops.len();
        let at = stops.iter().position(|s| *s == self.focus).unwrap_or(0);
        let next = if back { (at + n - 1) % n } else { (at + 1) % n };
        self.set_focus(stops[next]);
    }

    /// Un petit bouton répond-il ? Précédente et suivante attendent une
    /// occurrence.
    fn enabled(&self, button: SearchButton) -> bool {
        match button {
            SearchButton::Prev | SearchButton::Next => !self.hits.is_empty(),
            _ => true,
        }
    }

    /// Le champ qui a le focus, s'il y en a un (pas sur un bouton).
    fn focused_field(&mut self) -> Option<&mut TextInput> {
        match self.focus {
            SearchFocus::Find => Some(&mut self.input),
            SearchFocus::With => self.replace.as_mut(),
            SearchFocus::Button(_) => None,
        }
    }
}

/// La requête à reprendre du texte sélectionné sur la page : sa première
/// ligne non vide, blancs réduits à une espace, au plus 256 caractères.
fn query_from_selection(selected: &str) -> Option<String> {
    let line = selected
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .find(|l| !l.is_empty())?;
    Some(line.chars().take(MAX_QUERY).collect())
}

/// Indice de l'occurrence suivante (ou précédente), en bouclant.
fn step_index(current: usize, n: usize, forward: bool) -> usize {
    if n == 0 {
        return 0;
    }
    let current = current.min(n - 1);
    if forward {
        (current + 1) % n
    } else {
        (current + n - 1) % n
    }
}

impl Viewer {
    /// Ouvre la carte de recherche (Ctrl+F, la loupe, la palette).
    ///
    /// Le texte sélectionné sur la page devient la requête. Sinon, une carte
    /// neuve reprend la dernière recherche ; une carte déjà ouverte garde la
    /// sienne. Dans tous les cas la requête est **sélectionnée** : on la
    /// relance d'Entrée, ou on tape la suivante par-dessus.
    pub(super) fn open_search_ui(&mut self) {
        if self.loaded.is_none() {
            return;
        }
        let candidate = query_from_selection(&self.selected_text());
        let last = self.last_search.clone();
        let s = self.search.get_or_insert_with(|| {
            let mut s = Search::new();
            if let Some((query, options)) = last {
                s.input.set_value(&query);
                s.options = options;
            }
            s
        });
        s.set_focus(SearchFocus::Find);
        if let Some(query) = candidate {
            if s.taken.as_deref() != Some(query.as_str()) {
                s.input.set_value(&query);
                s.taken = Some(query);
            }
        }
        s.input.select_all();
        log_line(&format!("recherche : ouverte « {} »", s.input.value));
        self.update_search();
    }

    /// Ouvre la recherche **avec** le champ de remplacement.
    ///
    /// C'est le « Rechercher et remplacer » d'Acrobat : on cherche un texte,
    /// on en donne un autre, et le document est réécrit sans que rien ne
    /// bouge autour — chaque occurrence garde sa police et sa couleur.
    pub(super) fn open_replace(&mut self) {
        if self.loaded.is_none() {
            return;
        }
        if self.search.is_none() {
            self.open_search_ui();
        }
        if let Some(s) = &mut self.search {
            if s.replace.is_none() {
                s.replace = Some(TextInput::new(lang::tr("Remplacer par…")));
            }
            // Le clavier va au champ qui manque : on ne remplace rien tant
            // qu'on n'a pas dit quoi chercher.
            let focus = if s.input.value.is_empty() {
                SearchFocus::Find
            } else {
                SearchFocus::With
            };
            s.set_focus(focus);
        }
    }

    /// Ferme la carte. La requête est retenue : F3 la relance, Ctrl+F la
    /// propose de nouveau.
    pub(super) fn close_search(&mut self) {
        let Some(s) = self.search.take() else { return };
        if !s.input.value.trim().is_empty() {
            self.last_search = Some((s.input.value, s.options));
        }
        log_line("recherche : fermée");
    }

    /// F3 (ou Maj+F3) : occurrence suivante (ou précédente). Carte fermée,
    /// la dernière recherche revient, à partir de la page affichée.
    pub(super) fn find_again(&mut self, forward: bool) {
        if self.loaded.is_none() || self.prompt.is_some() {
            return;
        }
        if self.search.is_some() {
            self.update_search();
            self.go_to_hit(forward);
            return;
        }
        let Some((query, options)) = self.last_search.clone() else {
            self.open_search_ui();
            return;
        };
        let mut s = Search::new();
        s.input.set_value(&query);
        s.input.select_all();
        s.options = options;
        self.search = Some(s);
        // Le parcours se place de lui-même sur la première occurrence à
        // partir de la page affichée : c'est la « suivante ». Pour la
        // précédente, on recule d'un cran — si le tour du document est déjà
        // fait, sans quoi la dernière trouvée ne serait pas la précédente.
        self.update_search();
        if !forward && !self.search_scanning() {
            self.go_to_hit(false);
        }
    }

    /// Passe à l'occurrence suivante ou précédente, en bouclant, et la
    /// montre. Chaque passage demandé (Entrée, F3, les flèches de la carte)
    /// est un saut : Alt+← ramène à l'occurrence d'avant.
    pub(super) fn go_to_hit(&mut self, forward: bool) {
        let Some(s) = &mut self.search else { return };
        let n = s.hits.len();
        if n == 0 {
            return;
        }
        s.current = step_index(s.current, n, forward);
        s.shown = true;
        s.jumped = true;
        let line = format!(
            "recherche : « {} » {} sur {n}",
            s.input.value,
            s.current + 1
        );
        self.navigate(Self::scroll_to_hit);
        log_line(&line);
    }

    /// Relance le parcours si la requête ou les options ont changé.
    pub(super) fn update_search(&mut self) {
        let origin = self.current_page();
        let Some(s) = &mut self.search else { return };
        let key = (s.input.value.clone(), s.options);
        if s.last.as_ref() == Some(&key) {
            return;
        }
        s.last = Some(key);
        s.hits.clear();
        s.current = 0;
        s.scanned = 0;
        s.origin = origin;
        s.insert_at = 0;
        s.shown = false;
        self.step_search();
    }

    /// Vrai s'il reste des pages à parcourir pour la recherche en cours.
    pub(super) fn search_scanning(&self) -> bool {
        let count = self.loaded.as_ref().map_or(0, |l| l.pages.len());
        self.search.as_ref().is_some_and(|s| s.scanned < count)
    }

    /// Parcourt quelques pages de plus (budget de temps court) ; vrai s'il
    /// en reste.
    pub(super) fn step_search(&mut self) -> bool {
        let count = self.loaded.as_ref().map_or(0, |l| l.pages.len());
        let deadline = Instant::now() + SLICE;
        let mut finished = None;
        {
            let (Some(l), Some(s)) = (self.loaded.as_mut(), self.search.as_mut()) else {
                return false;
            };
            if s.scanned >= count {
                return false;
            }
            if s.input.value.trim().is_empty() {
                s.scanned = count;
                return false;
            }
            let origin = s.origin.min(count - 1);
            while s.scanned < count {
                let page = (origin + s.scanned) % count;
                let found: Vec<SearchHit> =
                    find_matches(&l.text(page).0, &s.input.value, s.options)
                        .into_iter()
                        .map(|m| SearchHit {
                            page,
                            editable: m.editable().is_some(),
                            pieces: m.pieces,
                        })
                        .collect();
                if page < origin {
                    // Les pages d'avant celle qu'on regardait arrivent en
                    // dernier : leurs occurrences se rangent devant, et
                    // l'occurrence courante reste la même.
                    let n = found.len();
                    if n > 0 {
                        if !s.hits.is_empty() && s.current >= s.insert_at {
                            s.current += n;
                        }
                        let at = s.insert_at;
                        s.hits.splice(at..at, found);
                        s.insert_at += n;
                    }
                } else {
                    s.hits.extend(found);
                }
                s.scanned += 1;
                if Instant::now() >= deadline {
                    break;
                }
            }
            if s.scanned >= count {
                finished = Some(format!(
                    "recherche : « {} » : {} occurrence(s)",
                    s.input.value,
                    s.hits.len()
                ));
            }
        }
        // Dès que la première occurrence apparaît, on s'y rend. Le premier
        // de ces sauts depuis l'ouverture de la carte retient d'où l'on
        // vient ; ceux qui suivent la frappe ne font que suivre la requête.
        let place = self.search.as_mut().and_then(|s| {
            let place = !s.shown && !s.hits.is_empty();
            s.shown |= place;
            place.then(|| std::mem::replace(&mut s.jumped, true))
        });
        match place {
            Some(false) => self.navigate(Self::scroll_to_hit),
            Some(true) => self.scroll_to_hit(),
            None => {}
        }
        if let Some(line) = finished {
            log_line(&line);
        }
        self.search_scanning()
    }

    /// Montre l'occurrence courante : la vue ne bouge que si elle n'est pas
    /// déjà bien en vue, et se centre alors dessus, en hauteur comme en
    /// largeur (une page zoomée déborde de la fenêtre).
    #[allow(clippy::many_single_char_names)] // coordonnées et matrices
    pub(super) fn scroll_to_hit(&mut self) {
        let Some((page, rect)) = self.search.as_ref().and_then(|s| {
            let hit = s.hits.get(s.current)?;
            Some((hit.page, hit.pieces.first()?.rect))
        }) else {
            return;
        };
        if self.view_mode.is_paged() {
            self.anchor = page;
        }
        let layout = self.layout();
        let Some(&PageBox { x, y, w, h, .. }) = layout.get(page) else {
            return;
        };
        let Some(l) = &self.loaded else { return };
        let p = &l.pages[page];
        let m = base_matrix(
            &p.crop_box(&l.doc),
            self.scale(),
            shown_rotation(l, p),
            w,
            h,
        );
        let dev = m.transform_rect(&rect);
        let (view_w, view_h) = (f64::from(self.view_width()), f64::from(self.view_height()));
        // En hauteur, une marge d'un septième : la carte de recherche, en
        // haut, ne doit pas couvrir l'occurrence qu'on vient de trouver.
        let (top, bottom) = (f64::from(y) + dev.y0, f64::from(y) + dev.y1);
        let margin = view_h / 7.0;
        if top < self.scroll_y + margin || bottom > self.scroll_y + view_h - margin {
            self.scroll_y = (top + bottom) / 2.0 - view_h / 2.0;
        }
        let (left, right) = (f64::from(x) + dev.x0, f64::from(x) + dev.x1);
        if left < self.scroll_x || right > self.scroll_x + view_w {
            self.scroll_x = (left + right) / 2.0 - view_w / 2.0;
        }
        self.clamp_scroll();
    }

    /// Dessine les surlignages des occurrences et la carte de recherche.
    #[allow(clippy::many_single_char_names)] // coordonnées et matrices
    pub(super) fn paint_search(&mut self, frame: &mut Frame<'_>) {
        let Some(s) = &self.search else { return };
        let layout = self.layout();
        let view_h = f64::from(frame.height);
        if let Some(l) = &self.loaded {
            let scale = self.scale();
            // Les occurrences sont triées par page : seules celles des pages
            // visibles sont regardées, et chaque page ne calcule sa matrice
            // qu'une fois — un mot courant en a des milliers.
            for (page, b) in layout.iter().enumerate() {
                let Some((x0, top)) = self.page_screen(&layout, page) else {
                    continue;
                };
                if top + f64::from(b.h) < 0.0 || top > view_h {
                    continue;
                }
                let lo = s.hits.partition_point(|h| h.page < page);
                let hi = s.hits.partition_point(|h| h.page <= page);
                if lo == hi {
                    continue;
                }
                let p = &l.pages[page];
                let m = base_matrix(&p.crop_box(&l.doc), scale, shown_rotation(l, p), b.w, b.h);
                for (i, hit) in s.hits[lo..hi].iter().enumerate() {
                    let color = if lo + i == s.current {
                        CURRENT_HIT
                    } else {
                        OTHER_HIT
                    };
                    for piece in &hit.pieces {
                        let dev = m.transform_rect(&piece.rect);
                        fill_rect_blend(
                            frame,
                            (x0 + dev.x0).round() as i32,
                            (top + dev.y0).round() as i32,
                            dev.width().round().max(2.0) as i32,
                            dev.height().round().max(2.0) as i32,
                            color,
                            HIT_ALPHA,
                        );
                    }
                }
            }
        }
        let pages = self.loaded.as_ref().map_or(0, |l| l.pages.len());
        let (t, dpi) = (self.theme, self.dpi_scale as f32);
        let (Some(text), Some(s)) = (self.text.as_mut(), self.search.as_mut()) else {
            return;
        };
        paint_card(frame, text, &mut self.raster, s, &t, dpi, pages);
    }

    /// Ce que vise un point de la fenêtre dans la carte de recherche, s'il y
    /// tombe. Les rectangles ont été relevés au dernier dessin, dans le
    /// repère de la vue : on y ramène le point.
    pub(super) fn search_hit(&self, x: i32, y: i32) -> Option<SearchPart> {
        let s = self.search.as_ref()?;
        let x = x - self.view_left() as i32;
        let y = y - self.view_top() as i32;
        if let Some(i) = s.row.hit(x, y) {
            return Some(SearchPart::Row(i));
        }
        if let Some((_, b)) = s.icons.iter().find(|(r, _)| inside(*r, x, y)) {
            return Some(SearchPart::Button(*b));
        }
        if let Some((_, focus)) = s.fields.iter().find(|(r, _)| inside(*r, x, y)) {
            return Some(SearchPart::Field(*focus));
        }
        inside(s.card, x, y).then_some(SearchPart::Card)
    }

    /// Un bouton de la carte attend son relâchement.
    pub(super) fn search_pressed(&self) -> bool {
        self.search
            .as_ref()
            .is_some_and(|s| s.row.is_pressed() || s.pressed.is_some())
    }

    /// Appui dans la carte de recherche ; rend vrai s'il la concernait. Un
    /// champ prend le focus tout de suite ; un bouton s'enfonce seulement,
    /// et agira au relâchement (`search_mouse_up`).
    pub(super) fn search_mouse_down(&mut self, x: i32, y: i32) -> bool {
        let Some(part) = self.search_hit(x, y) else {
            return false;
        };
        let (vx, vy) = (x - self.view_left() as i32, y - self.view_top() as i32);
        if let Some(s) = &mut self.search {
            match part {
                SearchPart::Field(focus) => s.set_focus(focus),
                SearchPart::Row(_) => {
                    s.row.mouse_down(vx, vy);
                }
                SearchPart::Button(b) if s.enabled(b) => {
                    s.pressed = Some(b);
                    s.hover = Some(b);
                }
                SearchPart::Button(_) | SearchPart::Card => {}
            }
        }
        true
    }

    /// Relâchement sur la carte de recherche : le bouton enfoncé agit, si le
    /// pointeur est resté dessus.
    pub(super) fn search_mouse_up(&mut self, x: i32, y: i32) {
        let part = self.search_hit(x, y);
        let (vx, vy) = (x - self.view_left() as i32, y - self.view_top() as i32);
        let Some(s) = &mut self.search else { return };
        if let Some(b) = s.pressed.take() {
            if part == Some(SearchPart::Button(b)) {
                self.search_button(b);
            }
            return;
        }
        match s.row.mouse_up(vx, vy) {
            Some(0) => {
                log_line("recherche : remplacer");
                self.replace_current();
            }
            Some(_) => {
                log_line("recherche : tout remplacer");
                self.replace_all();
            }
            None => {}
        }
    }

    /// Ce que fait un petit bouton.
    fn search_button(&mut self, button: SearchButton) {
        match button {
            SearchButton::MatchCase | SearchButton::WholeWord => {
                let Some(s) = &mut self.search else { return };
                let (name, on) = if button == SearchButton::MatchCase {
                    s.options.match_case = !s.options.match_case;
                    ("respecter la casse", s.options.match_case)
                } else {
                    s.options.whole_word = !s.options.whole_word;
                    ("mot entier", s.options.whole_word)
                };
                log_line(&format!(
                    "recherche : {name} {}",
                    if on { "activé" } else { "désactivé" }
                ));
                // D'autres options, d'autres occurrences : on refait le tour.
                self.update_search();
            }
            SearchButton::Prev => self.go_to_hit(false),
            SearchButton::Next => self.go_to_hit(true),
            SearchButton::Close => self.close_search(),
        }
    }

    /// Survol de la carte de recherche : le bouton survolé s'éclaire, avec
    /// son info-bulle ; le pointeur devient une main sur un bouton, une
    /// barre sur un champ. Rend vrai si le pointeur est sur la carte — la
    /// page dessous n'a alors rien à en savoir.
    pub(super) fn search_hover(&mut self, x: i32, y: i32, window: &mut dyn WindowHandle) -> bool {
        let part = self.search_hit(x, y);
        let (vx, vy) = (x - self.view_left() as i32, y - self.view_top() as i32);
        let Some(s) = &mut self.search else {
            return false;
        };
        let row_changed = if part.is_some() {
            s.row.mouse_move(vx, vy)
        } else {
            s.row.leave()
        };
        let hover = match part {
            Some(SearchPart::Button(b)) if s.enabled(b) => Some(b),
            _ => None,
        };
        let hover_changed = hover != s.hover;
        s.hover = hover;
        let cursor = match part {
            Some(SearchPart::Row(_)) => Cursor::Hand,
            Some(SearchPart::Button(_)) if hover.is_some() => Cursor::Hand,
            Some(SearchPart::Field(_)) => Cursor::IBeam,
            Some(SearchPart::Button(_) | SearchPart::Card) | None => Cursor::Arrow,
        };
        if row_changed || hover_changed {
            window.request_redraw();
        }
        if hover_changed {
            self.update_tip(window);
        }
        if part.is_none() {
            return false;
        }
        window.set_cursor(cursor);
        true
    }

    /// Info-bulle du petit bouton survolé, en coordonnées de la fenêtre.
    pub(super) fn search_tip(&self) -> Option<(String, Rect)> {
        let search = self.search.as_ref()?;
        let button = search.hover?;
        let &(rect, _) = search.icons.iter().find(|(_, b)| *b == button)?;
        let text = match button {
            SearchButton::MatchCase => lang::tr("Respecter la casse"),
            SearchButton::WholeWord => lang::tr("Mot entier"),
            SearchButton::Prev => lang::tr("Occurrence précédente (Maj+F3)"),
            SearchButton::Next => lang::tr("Occurrence suivante (F3)"),
            SearchButton::Close => lang::tr("Fermer la recherche (Échap)"),
        };
        let (left, top) = (self.view_left() as i32, self.view_top() as i32);
        Some((
            text.to_string(),
            (rect.0 + left, rect.1 + top, rect.2, rect.3),
        ))
    }

    /// Touche quand la carte a le clavier : elle va au champ qui a le focus.
    pub(super) fn search_key(&mut self, key: Key, m: Modifiers, window: &mut dyn WindowHandle) {
        let Some(s) = &mut self.search else { return };
        let focus = s.focus;
        let action = match focus {
            SearchFocus::With => s
                .replace
                .as_mut()
                .map_or(InputAction::None, |r| r.key(key, m.shift)),
            SearchFocus::Find => s.input.key(key, m.shift),
            // Sur un bouton, seul Échap compte : il ferme la recherche,
            // comme partout ailleurs dans la carte.
            SearchFocus::Button(_) if key == Key::Escape => InputAction::Cancel,
            SearchFocus::Button(_) => InputAction::None,
        };
        match action {
            InputAction::Cancel => self.close_search(),
            // Entrée dans le champ de remplacement remplace l'occurrence
            // courante ; dans l'autre, elle passe à la suivante (Maj : la
            // précédente).
            InputAction::Submit if focus == SearchFocus::With => self.replace_current(),
            InputAction::Submit => {
                self.update_search();
                self.go_to_hit(!m.shift);
            }
            InputAction::Changed if focus == SearchFocus::With => {}
            InputAction::Changed => self.update_search(),
            // Un champ d'une ligne n'a que faire des pages : elles font
            // défiler le document, la carte restant ouverte.
            InputAction::None
                if matches!(key, Key::PageUp | Key::PageDown | Key::Up | Key::Down) =>
            {
                self.key(key, m, window);
            }
            InputAction::None => {}
        }
        window.request_redraw();
    }

    /// Caractère quand la carte a le clavier.
    pub(super) fn search_char(&mut self, c: char) {
        let Some(s) = &mut self.search else { return };
        // Les caractères ne vont qu'à un champ : sur un bouton, la barre
        // d'espace le presse, elle ne s'écrit pas.
        match s.focus {
            SearchFocus::With => {
                if let Some(r) = s.replace.as_mut() {
                    let _ = r.insert_char(c);
                }
            }
            SearchFocus::Find => {
                if s.input.insert_char(c) == InputAction::Changed {
                    self.update_search();
                }
            }
            SearchFocus::Button(_) => {}
        }
    }

    /// Colle dans le champ de la carte qui a le focus.
    pub(super) fn search_paste(&mut self, text: &str) {
        let Some(s) = &mut self.search else { return };
        let find = s.focus == SearchFocus::Find;
        let changed = s
            .focused_field()
            .is_some_and(|f| f.paste(text) == InputAction::Changed);
        if changed && find {
            self.update_search();
        }
    }

    /// Ctrl+A quand la carte a le clavier : tout le champ, pas tout le
    /// document. Rend faux si la carte n'est pas là pour le prendre.
    pub(super) fn search_select_all(&mut self) -> bool {
        if self.editing_text() {
            return false;
        }
        let Some(s) = &mut self.search else {
            return false;
        };
        if let Some(f) = s.focused_field() {
            f.select_all();
        }
        true
    }

    /// Remplace l'occurrence courante, puis passe à la suivante.
    pub(super) fn replace_current(&mut self) {
        let Some((with, page, piece, editable, spans)) = self.search.as_ref().and_then(|s| {
            let with = s.replace.as_ref()?.value.clone();
            let hit = s.hits.get(s.current)?;
            Some((
                with,
                hit.page,
                *hit.pieces.first()?,
                hit.editable,
                hit.pieces.len() > 1,
            ))
        }) else {
            return;
        };
        if !editable {
            // Une édition réécrit une ligne à la fois, et des glyphes
            // entiers : l'occurrence coupée par une fin de ligne, ou qui
            // coupe une ligature, reste, et l'on passe à la suivante.
            let why = if spans {
                "Cette occurrence est à cheval sur deux lignes : modifiez-la dans l'éditeur de texte."
            } else {
                "Cette occurrence coupe une ligature : cherchez le mot entier pour la remplacer."
            };
            self.set_notice(lang::tr(why).into());
            self.go_to_hit(true);
            return;
        }
        // Une modification referme la recherche (le document a changé sous
        // elle) : on la met de côté pour la rendre telle quelle, et l'on
        // reparcourt le document. Le rang courant ne bouge pas : l'occurrence
        // remplacée ayant disparu, c'est la suivante qui se trouve surlignée.
        let saved = self.search.take();
        self.apply_edit(EditOp::EditText {
            page,
            line: piece.line,
            start: piece.start,
            end: piece.end,
            text: with,
        });
        self.restore_search(saved);
    }

    /// Rend à la carte de recherche l'état qu'elle avait avant une
    /// modification, et refait le tour du document — d'un coup : il faut
    /// les occurrences toutes comptées pour retrouver la courante.
    fn restore_search(&mut self, saved: Option<Search>) {
        let Some(mut s) = saved else { return };
        s.hits.clear();
        s.fields.clear();
        s.icons.clear();
        s.pressed = None;
        s.last = None;
        let current = s.current;
        // Une modification n'est pas un saut : rien à retenir.
        s.jumped = true;
        self.search = Some(s);
        self.update_search();
        while self.step_search() {}
        if let Some(s) = &mut self.search {
            s.current = current.min(s.hits.len().saturating_sub(1));
            s.shown = true;
            // Plus rien à remplacer : les boutons se grisent, et le focus
            // revient au champ plutôt que de rester sur un bouton muet.
            if s.hits.is_empty() && matches!(s.focus, SearchFocus::Button(_)) {
                s.set_focus(SearchFocus::With);
            }
        }
        self.scroll_to_hit();
    }

    /// Remplace toutes les occurrences du document, d'un seul geste
    /// annulable. Celles qui sont à cheval sur deux lignes, ou qui coupent
    /// une ligature, restent, et la notice le dit.
    pub(super) fn replace_all(&mut self) {
        // Le compte doit être complet avant d'annoncer quoi que ce soit.
        while self.step_search() {}
        let Some((find, with, options, total, spanning)) = self.search.as_ref().and_then(|s| {
            let with = s.replace.as_ref()?.value.clone();
            let spanning = s.hits.iter().filter(|h| !h.editable).count();
            Some((
                s.input.value.clone(),
                with,
                s.options,
                s.hits.len(),
                spanning,
            ))
        }) else {
            return;
        };
        if find.trim().is_empty() || total == 0 {
            return;
        }
        let editable = total - spanning;
        let mut replaced = false;
        if editable > 0 {
            let mut saved = self.search.take();
            if let Some(s) = &mut saved {
                s.current = 0;
            }
            replaced = self.apply_edit(EditOp::ReplaceAll {
                find,
                with,
                options,
            });
            self.restore_search(saved);
        }
        if spanning > 0 {
            self.set_notice(lang::trf(
                "{} occurrence(s) remplacée(s), {} ignorée(s) (à cheval sur deux lignes ou coupant une ligature)",
                &[
                    &(if replaced { editable } else { 0 }).to_string(),
                    &spanning.to_string(),
                ],
            ));
        } else if replaced {
            self.set_notice(lang::trf(
                "{} occurrence(s) remplacée(s)",
                &[&editable.to_string()],
            ));
        }
    }
}

/// Dessine la carte de recherche, flottante en haut à droite : le champ et
/// ses petits boutons, le champ de remplacement s'il est ouvert, le
/// compteur et les boutons « Remplacer ».
///
/// C'est la carte commune (`modal::card` : même rayon, même ombre), mais
/// sans titre ni voile : elle n'est pas modale, on lit la page à côté, comme
/// avec la barre de recherche d'Acrobat — un titre lui volerait de la
/// hauteur au-dessus du document.
// Une carte : x, y, s(…), thème ; et le cadre, le texte, les icônes, la
// recherche, l'échelle et le nombre de pages dont elle parle — les regrouper
// dans une structure n'apprendrait rien.
#[allow(
    clippy::many_single_char_names,
    clippy::too_many_lines,
    clippy::too_many_arguments
)]
fn paint_card(
    frame: &mut Frame<'_>,
    text: &mut TextRenderer,
    raster: &mut Rasterizer,
    search: &mut Search,
    theme: &Theme,
    dpi: f32,
    page_count: usize,
) {
    let t = theme;
    let s = |v: f32| (v * dpi).round() as i32;
    let size = t.font_size * dpi;
    let margin = s(12.0);
    // Une fenêtre étroite rétrécit la carte, et avec elle le champ ; jamais
    // de largeur négative.
    let card_w = s(440.0).min(frame.width as i32 - 2 * margin).max(0);
    let (pad, field_h, gap, footer_h) = (s(12.0), s(32.0), s(8.0), s(30.0));
    let (btn, btn_gap, sep) = (s(30.0), s(2.0), s(8.0));
    let rows = if search.replace.is_some() { 2 } else { 1 };
    let card_h = pad * 2 + field_h * rows + gap * rows + footer_h;
    let x = frame.width as i32 - card_w - margin;
    let y = margin;
    crate::ui::modal::card(frame, t, dpi, x, y, card_w, card_h, 1.0);
    search.card = (x, y, card_w, card_h);

    // Première rangée, de droite à gauche : fermer, suivante, précédente,
    // puis les deux bascules ; le champ prend ce qui reste.
    search.icons.clear();
    let by = y + pad + (field_h - btn) / 2;
    let close_x = x + card_w - pad - btn;
    let next_x = close_x - sep / 2 - btn;
    let prev_x = next_x - btn_gap - btn;
    let word_x = prev_x - sep - btn;
    let case_x = word_x - btn_gap - btn;
    for (bx, b) in [
        (case_x, SearchButton::MatchCase),
        (word_x, SearchButton::WholeWord),
        (prev_x, SearchButton::Prev),
        (next_x, SearchButton::Next),
        (close_x, SearchButton::Close),
    ] {
        search.icons.push(((bx, by, btn, btn), b));
    }
    let scanning = search.scanned < page_count;
    let query_empty = search.input.value.trim().is_empty();
    let no_match = !query_empty && !scanning && search.hits.is_empty();

    search.fields.clear();
    let fx = x + pad;
    let mut fy = y + pad;
    let fw = (case_x - gap - fx).max(0);
    search.input.draw(frame, text, t, dpi, fx, fy, fw, field_h);
    if no_match {
        // Rien trouvé : le champ se cercle de la teinte d'erreur, comme une
        // saisie refusée — on le voit sans lire le compteur.
        let ring = (1.5 * dpi).max(1.0);
        round_rect_outline(frame, fx, fy, fw, field_h, 7.0 * dpi, ring, t.danger);
    }
    search
        .fields
        .push(((fx, fy, fw, field_h), SearchFocus::Find));
    for &(rect, b) in &search.icons {
        let lit = match b {
            SearchButton::MatchCase => search.options.match_case,
            SearchButton::WholeWord => search.options.whole_word,
            _ => false,
        };
        let enabled = search.enabled(b);
        let pointer = if !enabled || search.hover != Some(b) {
            Pointer::Away
        } else if search.pressed == Some(b) {
            Pointer::Pressed
        } else {
            Pointer::Over
        };
        paint_button(
            frame,
            text,
            raster,
            t,
            dpi,
            rect,
            b,
            ButtonState {
                lit,
                enabled,
                pointer,
            },
        );
    }
    fy += field_h + gap;
    if let Some(r) = &search.replace {
        let rw = card_w - 2 * pad;
        r.draw(frame, text, t, dpi, fx, fy, rw, field_h);
        search
            .fields
            .push(((fx, fy, rw, field_h), SearchFocus::With));
        fy += field_h + gap;
    }

    // Le pied : le compteur à gauche, les boutons groupés à droite.
    let (counter, ink) = if query_empty {
        (
            lang::tr("Entrée : suivante · Maj+Entrée : précédente").to_string(),
            t.text_dim,
        )
    } else if scanning {
        (
            lang::trf(
                "{} trouvée(s), recherche…",
                &[&search.hits.len().to_string()],
            ),
            t.text_dim,
        )
    } else if no_match {
        (lang::tr("Aucun résultat").to_string(), t.danger)
    } else {
        (
            lang::trf(
                "{} sur {}",
                &[
                    &(search.current + 1).to_string(),
                    &search.hits.len().to_string(),
                ],
            ),
            t.text,
        )
    };
    let baseline = fy as f32 + f32::midpoint(footer_h as f32, text.ascent(size)) - 1.0;
    let mut right = x + card_w - pad;
    if search.replace.is_some() {
        // Dans l'ordre de Windows : l'action principale d'abord. Une
        // occurrence à cheval sur deux lignes, ou dans une ligature, ne se
        // remplace pas seule.
        let usable = !search.hits.is_empty();
        let one = search.hits.get(search.current).is_some_and(|h| h.editable);
        search.row.buttons = vec![
            RowButton {
                label: lang::tr("Remplacer").into(),
                primary: true,
                enabled: usable && one,
            },
            RowButton {
                label: lang::tr("Tout remplacer").into(),
                primary: false,
                enabled: usable,
            },
        ];
        right = search
            .row
            .layout_right(text, size, dpi, right, fy, footer_h)
            - gap;
        let focus = match search.focus {
            SearchFocus::Button(i) => Some(i),
            _ => None,
        };
        search.row.paint(frame, text, t, dpi, focus);
    } else {
        search.row.buttons.clear();
    }
    text.draw_clipped(
        frame,
        (fx + s(4.0)) as f32,
        baseline,
        size * 0.92,
        &counter,
        ink,
        (right - fx - s(8.0)).max(0) as f32,
    );
}

/// Où en est le pointeur avec un petit bouton.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pointer {
    /// Ailleurs.
    Away,
    /// Dessus.
    Over,
    /// Dessus, bouton enfoncé.
    Pressed,
}

/// L'état d'un petit bouton, pour le peindre.
#[derive(Debug, Clone, Copy)]
struct ButtonState {
    /// Bascule allumée.
    lit: bool,
    /// Il répond.
    enabled: bool,
    /// Le pointeur.
    pointer: Pointer,
}

/// Un petit bouton de la carte. Les bascules allumées portent le fond et
/// l'encre d'accent, comme celles de la barre d'outils ; « Aa » et « ab »
/// sont des lettres, les autres des icônes.
#[allow(clippy::too_many_arguments, clippy::many_single_char_names)] // un cadre, un bouton, son état
fn paint_button(
    frame: &mut Frame<'_>,
    text: &mut TextRenderer,
    raster: &mut Rasterizer,
    t: &Theme,
    dpi: f32,
    (x, y, w, h): Rect,
    button: SearchButton,
    state: ButtonState,
) {
    let radius = 6.0 * dpi;
    match (state.lit, state.pointer) {
        (true, pointer) => {
            let alpha = if pointer == Pointer::Away { 0.18 } else { 0.28 };
            round_rect_alpha(frame, x, y, w, h, radius, t.accent, alpha);
        }
        (false, Pointer::Pressed) => round_rect(frame, x, y, w, h, radius, t.button_hover),
        (false, Pointer::Over) => round_rect(frame, x, y, w, h, radius, t.hover),
        (false, Pointer::Away) => {}
    }
    let ink = if !state.enabled {
        t.disabled()
    } else if state.lit {
        t.accent
    } else {
        t.text
    };
    let icon = match button {
        SearchButton::Prev => Some(Icon::ChevronUp),
        SearchButton::Next => Some(Icon::ChevronDown),
        SearchButton::Close => Some(Icon::Close),
        SearchButton::MatchCase | SearchButton::WholeWord => None,
    };
    if let Some(icon) = icon {
        let px = (18.0 * dpi).round();
        let (ix, iy) = (x + (w - px as i32) / 2, y + (h - px as i32) / 2);
        icons::draw(frame, raster, icon, ix, iy, px, ink);
        return;
    }
    let size = t.font_size * dpi;
    let label = if button == SearchButton::MatchCase {
        "Aa"
    } else {
        "ab"
    };
    let lw = text.measure(size, label);
    let lx = x as f32 + (w as f32 - lw) / 2.0;
    let baseline = y as f32 + f32::midpoint(h as f32, text.ascent(size)) - 1.0;
    text.draw(frame, lx, baseline, size, label, ink);
    if button == SearchButton::WholeWord {
        // « ab » posé dans un crochet ouvert vers le haut : le mot seul,
        // borné des deux côtés.
        let stroke = dpi.max(1.0).round() as i32;
        let (l, r) = (
            (lx - 2.0 * dpi).round() as i32,
            (lx + lw + 2.0 * dpi).round() as i32,
        );
        let under = (baseline + 3.0 * dpi).round() as i32;
        let tick = (3.0 * dpi).round() as i32;
        frame.fill_rect(l, under, r - l, stroke, ink.0, ink.1, ink.2);
        frame.fill_rect(l, under - tick, stroke, tick, ink.0, ink.1, ink.2);
        frame.fill_rect(r - stroke, under - tick, stroke, tick, ink.0, ink.1, ink.2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requete_tiree_de_la_selection() {
        assert_eq!(query_from_selection(""), None);
        assert_eq!(query_from_selection("  \n \t\n"), None);
        assert_eq!(
            query_from_selection("\n  premier   mot\u{a0}ici \nseconde ligne"),
            Some("premier mot ici".into())
        );
        let long = "x".repeat(1000);
        assert_eq!(
            query_from_selection(&long).map(|q| q.chars().count()),
            Some(MAX_QUERY)
        );
    }

    #[test]
    fn pas_suivant_et_precedent_bouclent() {
        assert_eq!(step_index(0, 0, true), 0);
        assert_eq!(step_index(0, 0, false), 0);
        assert_eq!(step_index(0, 3, true), 1);
        assert_eq!(step_index(2, 3, true), 0);
        assert_eq!(step_index(0, 3, false), 2);
        assert_eq!(step_index(1, 3, false), 0);
        // Un indice périmé (le document a changé) reste dans les bornes.
        assert_eq!(step_index(9, 3, true), 0);
        assert_eq!(step_index(9, 3, false), 1);
    }
}
