//! Feuille « Insérer des pages » : quelles pages du fichier choisi, et où
//! les mettre dans le document — au début, avant ou après une page, à la
//! fin —, comme la boîte « Insérer des pages » d'Acrobat.
//!
//! Le champ « Pages » se lit comme partout dans Acrux (« 1-3, 5 », vide pour
//! toutes) et se vérifie pendant qu'on tape : l'erreur s'affiche sous lui,
//! avant qu'on valide. La position se choisit dans un contrôle segmenté ; le
//! numéro de page ne répond que pour « avant » et « après ».
//!
//! L'état est pur : la feuille reçoit des touches et des clics et rend une
//! [`SheetAction`] ; c'est le visualiseur qui insère.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    // La peinture est une mise en page lue de haut en bas : la découper en
    // morceaux ferait perdre le fil des ordonnées.
    clippy::too_many_lines
)]

use crate::platform::{Frame, Key};
use crate::ui::controls::{self, Segment, SegmentItem};
use crate::ui::input::{InputAction, TextInput};
use crate::ui::lang::{tr, trf};
use crate::ui::modal::{self, Appear};
use crate::ui::paint::{button, focus_ring, round_rect_alpha, veil, ButtonLook};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Où insérer, par rapport au document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// Avant la première page.
    Start,
    /// Avant la page N.
    Before,
    /// Après la page N.
    After,
    /// Après la dernière page.
    End,
}

impl Position {
    /// Les quatre positions, dans l'ordre du contrôle.
    pub const ALL: [Position; 4] = [
        Position::Start,
        Position::Before,
        Position::After,
        Position::End,
    ];

    /// Libellé français (traduit au dessin).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Position::Start => "Au début",
            Position::Before => "Avant la page",
            Position::After => "Après la page",
            Position::End => "À la fin",
        }
    }

    /// Vrai si la position se rapporte à une page : le numéro compte.
    #[must_use]
    pub fn needs_page(self) -> bool {
        matches!(self, Position::Before | Position::After)
    }
}

/// Index d'insertion (0 = avant la première page) pour la position `pos`,
/// la page `page` comptée à partir de 1 et un document de `count` pages ;
/// `None` quand la page n'existe pas.
#[must_use]
pub fn insertion_index(pos: Position, page: usize, count: usize) -> Option<usize> {
    let exists = (1..=count).contains(&page);
    match pos {
        Position::Start => Some(0),
        Position::End => Some(count),
        Position::Before => exists.then(|| page - 1),
        Position::After => exists.then_some(page),
    }
}

/// Ce qui a le focus clavier, ou ce que vise un clic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Le champ « Pages ».
    Pages,
    /// Une position ; au clavier, le contrôle entier.
    Position(Position),
    /// Le numéro de page.
    Number,
    /// « Insérer ».
    Insert,
    /// « Annuler ».
    Cancel,
}

impl Target {
    /// Même arrêt de tabulation : les quatre positions n'en font qu'un, que
    /// les flèches parcourent.
    fn same_stop(self, other: Target) -> bool {
        match (self, other) {
            (Target::Position(_), Target::Position(_)) => true,
            (a, b) => a == b,
        }
    }
}

/// Ce que la feuille demande au visualiseur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SheetAction {
    /// Rien.
    None,
    /// Redessiner.
    Redraw,
    /// Fermer sans rien faire.
    Cancel,
    /// Insérer ces pages de la source (vide : toutes) à cet index.
    Insert {
        /// Pages de la source, à partir de 0 ; vide pour toutes.
        pages: Vec<usize>,
        /// Index d'insertion dans le document.
        at: usize,
    },
}

/// La feuille « Insérer des pages ».
#[derive(Debug)]
pub struct InsertSheet {
    /// Nom du fichier source.
    source: String,
    /// Ses pages.
    source_pages: usize,
    /// Pages du document d'arrivée.
    doc_pages: usize,
    /// Pages à prendre (« 1-3, 5 », vide pour toutes).
    pages: TextInput,
    /// Numéro de la page de référence.
    number: TextInput,
    /// Position choisie.
    pub position: Position,
    /// Élément qui a le focus clavier.
    focus: Target,
    /// Élément survolé.
    hover: Option<Target>,
    /// Élément enfoncé : il n'agira qu'au relâchement, pointeur dessus.
    pressed: Option<Target>,
    /// Zones cliquables, relevées au dernier dessin.
    targets: Vec<(i32, i32, i32, i32, Target)>,
    /// Apparition, comme toutes les cartes.
    appear: Appear,
}

impl InsertSheet {
    /// Feuille pour `source_pages` pages de `source`, à insérer dans un
    /// document de `doc_pages` pages ; la position proposée est « après la
    /// page `after` » (comptée à partir de 1), comme dans Acrobat.
    #[must_use]
    pub fn new(source: &str, source_pages: usize, doc_pages: usize, after: usize) -> Self {
        let mut number = TextInput::new("");
        number.set_value(&after.clamp(1, doc_pages.max(1)).to_string());
        number.focused = false;
        let mut sheet = Self {
            source: source.to_string(),
            source_pages,
            doc_pages,
            pages: TextInput::new(tr("Toutes (ou par exemple 1-3, 5)")),
            number,
            position: Position::After,
            focus: Target::Pages,
            hover: None,
            pressed: None,
            targets: Vec::new(),
            appear: Appear::new(),
        };
        sheet.sync_focus();
        sheet
    }

    /// Vrai tant que l'apparition a une image à peindre.
    #[must_use]
    pub fn animating(&self) -> bool {
        self.appear.animating()
    }

    /// Pages du fichier source.
    #[must_use]
    pub fn source_pages(&self) -> usize {
        self.source_pages
    }

    /// Vrai si le focus est dans un champ : l'espace y est un caractère.
    #[must_use]
    pub fn typing(&self) -> bool {
        matches!(self.focus, Target::Pages | Target::Number)
    }

    /// Pages choisies (à partir de 0 ; vide pour toutes), ou le message
    /// d'erreur.
    fn chosen_pages(&self) -> Result<Vec<usize>, String> {
        let spec = self.pages.value.trim();
        if spec.is_empty() {
            return Ok(Vec::new());
        }
        acrux_features::pages::parse_page_spec(spec, self.source_pages)
            .ok()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| {
                trf(
                    "Pages invalides : écrivez par exemple 1-3, 5 (de 1 à {})",
                    &[&self.source_pages.to_string()],
                )
            })
    }

    /// Index d'insertion, ou le message d'erreur.
    fn chosen_index(&self) -> Result<usize, String> {
        let page = self.number.value.trim().parse::<usize>().unwrap_or(0);
        insertion_index(self.position, page, self.doc_pages)
            .ok_or_else(|| trf("Numéro de page : de 1 à {}", &[&self.doc_pages.to_string()]))
    }

    /// Arrêts de tabulation, dans l'ordre de la feuille.
    fn stops(&self) -> Vec<Target> {
        let mut out = vec![Target::Pages, Target::Position(self.position)];
        if self.position.needs_page() {
            out.push(Target::Number);
        }
        out.push(Target::Insert);
        out.push(Target::Cancel);
        out
    }

    /// Passe le focus à l'arrêt suivant (ou précédent).
    fn move_focus(&mut self, forward: bool) {
        let stops = self.stops();
        let n = stops.len();
        let at = stops.iter().position(|s| s.same_stop(self.focus));
        let next = match (at, forward) {
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
            (None, _) => 0,
        };
        if let Some(&stop) = stops.get(next) {
            self.focus = stop;
        }
        self.sync_focus();
    }

    /// Le caret ne se montre que dans le champ qui a le focus.
    fn sync_focus(&mut self) {
        self.pages.focused = self.focus == Target::Pages;
        self.number.focused = self.focus == Target::Number;
    }

    /// Champ qui a le focus, s'il y en a un.
    fn focused_field(&mut self) -> Option<&mut TextInput> {
        match self.focus {
            Target::Pages => Some(&mut self.pages),
            Target::Number => Some(&mut self.number),
            _ => None,
        }
    }

    /// Touche pressée.
    pub fn key(&mut self, key: Key, shift: bool) -> SheetAction {
        match key {
            Key::Escape => SheetAction::Cancel,
            Key::Tab => {
                self.move_focus(!shift);
                SheetAction::Redraw
            }
            Key::Enter => match self.focus {
                Target::Cancel => SheetAction::Cancel,
                _ => self.submit(),
            },
            Key::Space => match self.focus {
                Target::Pages | Target::Number => SheetAction::None,
                other => self.press(other),
            },
            Key::Left | Key::Right if matches!(self.focus, Target::Position(_)) => {
                let all = Position::ALL;
                let at = all.iter().position(|p| *p == self.position).unwrap_or(0);
                let next = if key == Key::Right {
                    (at + 1).min(all.len() - 1)
                } else {
                    at.saturating_sub(1)
                };
                self.position = all[next];
                self.focus = Target::Position(self.position);
                SheetAction::Redraw
            }
            _ => {
                let Some(field) = self.focused_field() else {
                    return SheetAction::None;
                };
                match field.key(key, shift) {
                    InputAction::Submit => self.submit(),
                    InputAction::Cancel => SheetAction::Cancel,
                    // Le contenu ou le caret a pu changer.
                    InputAction::Changed | InputAction::None => SheetAction::Redraw,
                }
            }
        }
    }

    /// Caractère tapé : il va au champ qui a le focus. Le numéro ne prend
    /// que des chiffres ; les pages, des chiffres, des virgules, des tirets
    /// et des espaces.
    pub fn char(&mut self, c: char) -> SheetAction {
        let allowed = match self.focus {
            Target::Number => c.is_ascii_digit(),
            Target::Pages => c.is_ascii_digit() || matches!(c, ',' | '-' | ' ' | ';'),
            _ => false,
        };
        if !allowed {
            return SheetAction::None;
        }
        let c = if c == ';' { ',' } else { c };
        let changed = self
            .focused_field()
            .is_some_and(|field| field.insert_char(c) == InputAction::Changed);
        if changed {
            SheetAction::Redraw
        } else {
            SheetAction::None
        }
    }

    /// Texte collé (Ctrl+V) dans le champ qui a le focus.
    pub fn paste(&mut self, text: &str) -> SheetAction {
        let clean: String = text
            .chars()
            .filter(|c| c.is_ascii_digit() || matches!(c, ',' | '-' | ' '))
            .collect();
        let changed = self
            .focused_field()
            .is_some_and(|field| field.paste(&clean) == InputAction::Changed);
        if changed {
            SheetAction::Redraw
        } else {
            SheetAction::None
        }
    }

    /// Élément sous un point, d'après le dernier dessin.
    #[must_use]
    pub fn target_at(&self, x: i32, y: i32) -> Option<Target> {
        self.targets
            .iter()
            .find(|(tx, ty, tw, th, _)| x >= *tx && x < tx + tw && y >= *ty && y < ty + th)
            .map(|t| t.4)
    }

    /// Appui. Un champ prend le focus tout de suite ; un segment ou un
    /// bouton n'agit qu'au relâchement.
    pub fn mouse_down(&mut self, x: i32, y: i32) -> SheetAction {
        self.pressed = None;
        match self.target_at(x, y) {
            Some(target @ (Target::Pages | Target::Number)) => self.press(target),
            Some(target) => {
                self.pressed = Some(target);
                self.hover = Some(target);
                SheetAction::Redraw
            }
            None => SheetAction::None,
        }
    }

    /// Relâchement : l'élément enfoncé agit, si le pointeur est resté dessus.
    pub fn mouse_up(&mut self, x: i32, y: i32) -> SheetAction {
        let Some(pressed) = self.pressed.take() else {
            return SheetAction::None;
        };
        if self.target_at(x, y) == Some(pressed) {
            self.press(pressed)
        } else {
            SheetAction::Redraw
        }
    }

    /// Survol ; rend vrai si l'affichage doit changer.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let over = self.target_at(x, y);
        let changed = over != self.hover;
        self.hover = over;
        changed
    }

    /// Effet d'un élément, cliqué ou pressé au clavier.
    fn press(&mut self, target: Target) -> SheetAction {
        match target {
            Target::Pages => self.focus = Target::Pages,
            Target::Number => {
                if self.position.needs_page() {
                    self.focus = Target::Number;
                }
            }
            Target::Position(p) => {
                self.position = p;
                self.focus = Target::Position(p);
            }
            Target::Insert => return self.submit(),
            Target::Cancel => return SheetAction::Cancel,
        }
        self.sync_focus();
        SheetAction::Redraw
    }

    /// Valide et rend l'insertion, ou met le focus sur le champ à reprendre
    /// (son erreur est déjà affichée dessous).
    fn submit(&mut self) -> SheetAction {
        let Ok(pages) = self.chosen_pages() else {
            self.focus = Target::Pages;
            self.sync_focus();
            return SheetAction::Redraw;
        };
        let Ok(at) = self.chosen_index() else {
            self.focus = Target::Number;
            self.sync_focus();
            return SheetAction::Redraw;
        };
        SheetAction::Insert { pages, at }
    }

    /// Dessine la feuille au centre du cadre et relève les zones cliquables.
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
    ) {
        let s = |v: f32| (v * dpi).round() as i32;
        let size = theme.font_size * dpi;
        let small = size * 0.92;
        let (fw, fh) = (frame.width as i32, frame.height as i32);
        let width = modal::card_width(fw, 500.0, dpi);
        let height = s(334.0);
        let pad = s(22.0);
        let inner = width - 2 * pad;
        self.targets.clear();

        let progress = self.appear.progress();
        veil(frame, progress);
        let (x, y) = modal::modal_rect(fw, fh, width, height, dpi, progress);
        let y = y.min((fh - height).max(s(8.0)));
        let left = x + pad;
        modal::card(frame, theme, dpi, x, y, width, height, progress);

        let mut cy = y + pad;
        let title_size = modal::title_size(theme, dpi);
        modal::title(
            frame,
            text,
            theme,
            dpi,
            left,
            cy + text.ascent(title_size) as i32,
            tr("Insérer des pages"),
        );
        cy += s(30.0);
        text.draw_clipped(
            frame,
            left as f32,
            cy as f32 + text.ascent(small),
            small,
            &trf(
                "Depuis « {} » — {} page(s)",
                &[&self.source, &self.source_pages.to_string()],
            ),
            theme.text_dim,
            inner as f32,
        );
        cy += s(32.0);

        // Les pages à prendre.
        let hover = self.hover;
        let hovered = |t: Target| hover == Some(t);
        text.draw(
            frame,
            left as f32,
            cy as f32 + text.ascent(small),
            small,
            tr("Pages à insérer"),
            theme.text_dim,
        );
        cy += s(20.0);
        let field_h = s(32.0);
        self.pages
            .draw(frame, text, theme, dpi, left, cy, inner, field_h);
        self.targets.push((left, cy, inner, field_h, Target::Pages));
        cy += field_h + s(6.0);
        let (line, tone) = match self.chosen_pages() {
            Ok(p) if p.is_empty() => (
                trf("Les {} pages du fichier", &[&self.source_pages.to_string()]),
                theme.text_dim,
            ),
            Ok(p) => (
                trf("{} page(s) choisie(s)", &[&p.len().to_string()]),
                theme.text_dim,
            ),
            Err(e) => (e, theme.danger),
        };
        text.draw_clipped(
            frame,
            left as f32,
            cy as f32 + text.ascent(small),
            small,
            &line,
            tone,
            inner as f32,
        );
        cy += s(30.0);

        // La position.
        text.draw(
            frame,
            left as f32,
            cy as f32 + text.ascent(small),
            small,
            &trf(
                "Position dans le document ({} page(s))",
                &[&self.doc_pages.to_string()],
            ),
            theme.text_dim,
        );
        cy += s(20.0);
        let seg_h = s(30.0);
        let labels: Vec<&str> = Position::ALL.iter().map(|p| tr(p.label())).collect();
        let items: Vec<SegmentItem<'_>> = Position::ALL
            .iter()
            .zip(&labels)
            .map(|(p, label)| SegmentItem {
                content: Segment::Label(label),
                on: *p == self.position,
                hovered: hovered(Target::Position(*p)),
            })
            .collect();
        let (seg_w, rects) = controls::segmented(frame, text, theme, dpi, left, cy, seg_h, &items);
        if matches!(self.focus, Target::Position(_)) {
            focus_ring(frame, left, cy, seg_w, seg_h, 7.0 * dpi, dpi, theme.accent);
        }
        for (p, (rx, ry, rw, rh)) in Position::ALL.iter().zip(rects) {
            self.targets.push((rx, ry, rw, rh, Target::Position(*p)));
        }
        cy += seg_h + s(12.0);

        // Le numéro de page, grisé quand la position n'en dépend pas.
        let enabled = self.position.needs_page();
        let label = tr("Page");
        let lw = text.measure(size, label) as i32;
        text.draw(
            frame,
            left as f32,
            (cy + field_h / 2) as f32 + text.ascent(size) / 2.0,
            size,
            label,
            if enabled { theme.text } else { theme.text_dim },
        );
        let nx = left + lw + s(12.0);
        let nw = s(76.0);
        self.number
            .draw(frame, text, theme, dpi, nx, cy, nw, field_h);
        if enabled {
            self.targets.push((nx, cy, nw, field_h, Target::Number));
        } else {
            round_rect_alpha(frame, nx, cy, nw, field_h, 7.0 * dpi, theme.bar, 0.6);
        }
        let (after, tone) = match self.chosen_index() {
            Err(e) if enabled => (e, theme.danger),
            _ => (
                trf("sur {}", &[&self.doc_pages.to_string()]),
                theme.text_dim,
            ),
        };
        let ax = nx + nw + s(10.0);
        text.draw_clipped(
            frame,
            ax as f32,
            (cy + field_h / 2) as f32 + text.ascent(small) / 2.0,
            small,
            &after,
            tone,
            (left + inner - ax) as f32,
        );

        // Les boutons, groupés à droite dans l'ordre de Windows :
        // « Insérer », puis « Annuler » — posés de droite à gauche.
        let bh = s(modal::BUTTON_H);
        let by = y + height - pad - bh;
        let mut bx = x + width - pad;
        for (label, target, primary) in [
            (tr("Annuler"), Target::Cancel, false),
            (tr("Insérer"), Target::Insert, true),
        ] {
            let bw = (text.measure(size, label) as i32 + s(32.0)).max(s(modal::BUTTON_MIN_W));
            bx -= bw;
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
                    hovered: hovered(target),
                    focused: self.focus == target,
                    disabled: false,
                    pressed: hovered(target) && self.pressed == Some(target),
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
            self.targets.push((bx, by, bw, bh, target));
            bx -= s(8.0);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)] // tests
mod tests {
    use super::*;

    #[test]
    fn l_index_d_insertion_suit_la_position() {
        assert_eq!(insertion_index(Position::Start, 7, 10), Some(0));
        assert_eq!(insertion_index(Position::End, 7, 10), Some(10));
        assert_eq!(insertion_index(Position::Before, 1, 10), Some(0));
        assert_eq!(insertion_index(Position::Before, 10, 10), Some(9));
        assert_eq!(insertion_index(Position::After, 10, 10), Some(10));
        assert_eq!(insertion_index(Position::After, 3, 10), Some(3));
        assert_eq!(insertion_index(Position::After, 0, 10), None);
        assert_eq!(insertion_index(Position::Before, 11, 10), None);
        // Un document vide : seuls le début et la fin ont un sens.
        assert_eq!(insertion_index(Position::Start, 1, 0), Some(0));
        assert_eq!(insertion_index(Position::End, 1, 0), Some(0));
        assert_eq!(insertion_index(Position::After, 1, 0), None);
    }

    fn typed(sheet: &mut InsertSheet, text: &str) {
        for c in text.chars() {
            let _ = sheet.char(c);
        }
    }

    #[test]
    fn par_defaut_toutes_les_pages_apres_la_page_donnee() {
        let mut sheet = InsertSheet::new("source.pdf", 4, 10, 3);
        assert_eq!(
            sheet.key(Key::Enter, false),
            SheetAction::Insert {
                pages: Vec::new(),
                at: 3
            }
        );
    }

    #[test]
    fn les_pages_et_la_position_se_choisissent_au_clavier() {
        let mut sheet = InsertSheet::new("source.pdf", 4, 10, 3);
        typed(&mut sheet, "2, 4-3x");
        assert_eq!(sheet.pages.value, "2, 4-3", "la lettre est refusée");
        // Tab passe à la position ; → va à « À la fin ».
        sheet.key(Key::Tab, false);
        assert!(matches!(sheet.focus, Target::Position(_)));
        sheet.key(Key::Right, false);
        assert_eq!(sheet.position, Position::End);
        // Sans numéro à donner, Tab saute le champ du numéro.
        sheet.key(Key::Tab, false);
        assert_eq!(sheet.focus, Target::Insert);
        assert_eq!(
            sheet.key(Key::Enter, false),
            SheetAction::Insert {
                pages: vec![1, 3, 2],
                at: 10
            }
        );
        sheet.focus = Target::Cancel;
        assert_eq!(sheet.key(Key::Enter, false), SheetAction::Cancel);
        assert_eq!(sheet.key(Key::Escape, false), SheetAction::Cancel);
    }

    #[test]
    fn une_saisie_fautive_ne_s_insere_pas() {
        let mut sheet = InsertSheet::new("source.pdf", 4, 10, 3);
        typed(&mut sheet, "9");
        assert!(sheet.chosen_pages().is_err(), "page 9 d'un fichier de 4");
        assert_eq!(sheet.key(Key::Enter, false), SheetAction::Redraw);
        assert_eq!(
            sheet.focus,
            Target::Pages,
            "le focus revient au champ fautif"
        );
        let _ = sheet.key(Key::Backspace, false);
        sheet.position = Position::Before;
        sheet.focus = Target::Number;
        sheet.sync_focus();
        sheet.number.clear();
        typed(&mut sheet, "12");
        assert!(sheet.chosen_index().is_err(), "page 12 d'un document de 10");
        assert_eq!(sheet.key(Key::Enter, false), SheetAction::Redraw);
        assert_eq!(sheet.focus, Target::Number);
        let _ = sheet.key(Key::Backspace, false);
        assert_eq!(
            sheet.key(Key::Enter, false),
            SheetAction::Insert {
                pages: Vec::new(),
                at: 0
            },
            "avant la page 1"
        );
    }

    #[test]
    fn les_segments_agissent_au_relachement() {
        let mut sheet = InsertSheet::new("source.pdf", 4, 10, 3);
        sheet.targets = vec![
            (0, 0, 50, 30, Target::Position(Position::Start)),
            (60, 0, 50, 30, Target::Number),
            (0, 40, 80, 30, Target::Insert),
        ];
        assert_eq!(sheet.mouse_down(10, 10), SheetAction::Redraw);
        assert_eq!(sheet.position, Position::After, "rien avant le relâchement");
        assert_eq!(sheet.mouse_up(12, 12), SheetAction::Redraw);
        assert_eq!(sheet.position, Position::Start);
        // Au début, le numéro ne prend pas le focus.
        sheet.mouse_down(70, 10);
        assert_ne!(sheet.focus, Target::Number);
        sheet.mouse_down(10, 50);
        assert_eq!(
            sheet.mouse_up(10, 50),
            SheetAction::Insert {
                pages: Vec::new(),
                at: 0
            }
        );
    }
}
