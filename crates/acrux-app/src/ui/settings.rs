//! Fiche « Paramètres » : la langue, l'apparence et les mises à jour, sur une
//! seule carte.
//!
//! « Paramètres » était une question — une icône « ? », trois boutons — qui
//! menait à d'autres questions : on ne voyait jamais d'un coup d'œil ce qui
//! était réglé, chaque changement coûtait trois clics, et le thème n'y
//! figurait pas. C'est désormais une fiche, comme celles de Windows : chaque
//! réglage est un groupe de cases, la case allumée dit l'état, et un clic le
//! change sur-le-champ — la fiche se repeint aussitôt dans la langue ou le
//! thème choisi.
//!
//! Clavier : Tab passe d'un groupe à l'autre, les flèches changent la valeur
//! du groupe (comme un groupe de boutons radio), Entrée ou Échap ferment.
//!
//! L'état est pur : la fiche reçoit des touches et des clics et rend une
//! [`SettingsAction`] ; c'est le visualiseur qui l'applique. Les tests
//! peuvent donc tout éprouver sans rien dessiner.

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
use crate::ui::lang::{tr, trf, Lang};
use crate::ui::modal::{self, inside, Appear, ButtonRow, Rect, RowButton};
use crate::ui::paint::{focus_ring, veil};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Où en est la recherche de mises à jour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateLine<'a> {
    /// Rien n'a été cherché depuis le lancement.
    Idle,
    /// Une recherche est en cours.
    Checking,
    /// La version installée est la plus récente.
    UpToDate,
    /// Une version plus récente existe.
    Available(&'a str),
    /// La recherche a échoué, pour cette raison.
    Failed(&'a str),
}

/// Ce que la fiche affiche : les réglages en vigueur.
#[derive(Debug, Clone, Copy)]
pub struct SettingsState<'a> {
    /// Langue choisie.
    pub language: Lang,
    /// Langue du système, que suit le choix « Système ».
    pub system: Lang,
    /// Thème sombre.
    pub dark: bool,
    /// Recherche des mises à jour au démarrage.
    pub auto_updates: bool,
    /// État de la recherche.
    pub update: UpdateLine<'a>,
    /// Version installée.
    pub version: &'a str,
}

/// Ce que la fiche demande au visualiseur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsAction {
    /// Fermer la fiche.
    Close,
    /// Changer de langue.
    Language(Lang),
    /// Thème sombre (vrai) ou clair (faux).
    Dark(bool),
    /// Chercher les mises à jour au démarrage, ou jamais.
    AutoUpdates(bool),
    /// Chercher une mise à jour maintenant.
    CheckNow,
    /// Installer la version trouvée.
    Install,
}

/// Arrêt de tabulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// Le groupe « Langue ».
    Language,
    /// Le groupe « Apparence ».
    Theme,
    /// Le groupe « Mises à jour ».
    Auto,
    /// « Rechercher maintenant ».
    Check,
    /// « Installer », quand une version est disponible.
    Install,
    /// « Fermer ».
    Close,
}

/// Case d'un groupe, visée par la souris.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// Une langue.
    Language(Lang),
    /// Sombre (vrai) ou clair (faux).
    Dark(bool),
    /// Au démarrage (vrai) ou jamais (faux).
    Auto(bool),
}

/// Les langues proposées, dans l'ordre de la fiche.
const LANGS: [Lang; 3] = [Lang::Auto, Lang::French, Lang::English];

/// Nom d'une langue **dans cette langue** : qui cherche l'anglais dans une
/// interface en français doit reconnaître « English », et inversement.
fn native(lang: Lang) -> &'static str {
    match lang {
        Lang::English => "English",
        Lang::French | Lang::Auto => "Français",
    }
}

/// La fiche « Paramètres ».
#[derive(Debug)]
pub struct SettingsSheet {
    /// Apparition.
    appear: Appear,
    /// Groupe ou bouton qui a le focus clavier.
    focus: Stop,
    /// Le clavier a servi : l'anneau de focus se montre. Comme sous
    /// Windows, il n'apparaît pas tant qu'on ne travaille qu'à la souris.
    keyboard: bool,
    /// Case survolée.
    hover: Option<Target>,
    /// Case enfoncée, en attente du relâchement.
    pressed: Option<Target>,
    /// Cases des groupes, relevées au dessin.
    hits: Vec<(Rect, Target)>,
    /// « Rechercher maintenant », « Installer ».
    updates: ButtonRow,
    /// « Fermer ».
    footer: ButtonRow,
}

impl Default for SettingsSheet {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsSheet {
    /// Fiche neuve, focus sur la langue.
    #[must_use]
    pub fn new() -> Self {
        Self {
            appear: Appear::new(),
            focus: Stop::Language,
            keyboard: false,
            hover: None,
            pressed: None,
            hits: Vec::new(),
            updates: ButtonRow::default(),
            footer: ButtonRow::default(),
        }
    }

    /// Vrai tant que l'apparition a une image à peindre.
    #[must_use]
    pub fn animating(&self) -> bool {
        self.appear.animating()
    }

    /// Arrêts de tabulation, dans l'ordre de la fiche.
    fn stops(state: &SettingsState<'_>) -> Vec<Stop> {
        let mut out = vec![Stop::Language, Stop::Theme, Stop::Auto, Stop::Check];
        if matches!(state.update, UpdateLine::Available(_)) {
            out.push(Stop::Install);
        }
        out.push(Stop::Close);
        out
    }

    /// Passe le focus à l'arrêt suivant (ou précédent).
    fn move_focus(&mut self, forward: bool, state: &SettingsState<'_>) {
        let stops = Self::stops(state);
        let n = stops.len();
        let next = match stops.iter().position(|s| *s == self.focus) {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None => 0,
        };
        if let Some(&stop) = stops.get(next) {
            self.focus = stop;
        }
    }

    /// Flèche dans un groupe : la valeur voisine, s'il y en a une.
    fn step(&self, forward: bool, state: &SettingsState<'_>) -> Option<SettingsAction> {
        match self.focus {
            Stop::Language => {
                let at = LANGS.iter().position(|l| *l == state.language)?;
                let next = if forward {
                    (at + 1).min(LANGS.len() - 1)
                } else {
                    at.saturating_sub(1)
                };
                (next != at).then_some(SettingsAction::Language(LANGS[next]))
            }
            // Clair, puis sombre.
            Stop::Theme => (forward != state.dark).then_some(SettingsAction::Dark(forward)),
            // Au démarrage, puis jamais.
            Stop::Auto => {
                (forward == state.auto_updates).then_some(SettingsAction::AutoUpdates(!forward))
            }
            Stop::Check | Stop::Install | Stop::Close => None,
        }
    }

    /// Effet d'un bouton, pressé au clavier.
    fn press(&self, state: &SettingsState<'_>) -> Option<SettingsAction> {
        match self.focus {
            Stop::Check if state.update != UpdateLine::Checking => Some(SettingsAction::CheckNow),
            Stop::Install => Some(SettingsAction::Install),
            Stop::Close => Some(SettingsAction::Close),
            _ => None,
        }
    }

    /// Touche pressée.
    pub fn key(
        &mut self,
        key: Key,
        shift: bool,
        state: &SettingsState<'_>,
    ) -> Option<SettingsAction> {
        match key {
            Key::Escape => Some(SettingsAction::Close),
            Key::Tab => {
                self.keyboard = true;
                self.move_focus(!shift, state);
                None
            }
            Key::Left | Key::Right | Key::Up | Key::Down => {
                self.keyboard = true;
                self.step(matches!(key, Key::Right | Key::Down), state)
            }
            // Entrée presse le bouton qui a le focus ; sur un groupe, c'est
            // le bouton par défaut, « Fermer ».
            Key::Enter => match self.focus {
                Stop::Language | Stop::Theme | Stop::Auto => Some(SettingsAction::Close),
                _ => self.press(state),
            },
            Key::Space => self.press(state),
            _ => None,
        }
    }

    /// Case sous un point, d'après le dernier dessin.
    fn target_at(&self, x: i32, y: i32) -> Option<Target> {
        self.hits
            .iter()
            .find(|(r, _)| inside(*r, x, y))
            .map(|(_, t)| *t)
    }

    /// Vrai si quelque chose se clique sous le point : le pointeur devient
    /// une main.
    #[must_use]
    pub fn over(&self, x: i32, y: i32) -> bool {
        self.target_at(x, y).is_some()
            || self.updates.hit(x, y).is_some()
            || self.footer.hit(x, y).is_some()
    }

    /// Survol ; rend vrai si l'affichage doit changer.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let over = self.target_at(x, y);
        let changed = over != self.hover;
        self.hover = over;
        changed | self.updates.mouse_move(x, y) | self.footer.mouse_move(x, y)
    }

    /// Appui : il enfonce, il ne décide rien. Le groupe cliqué prend le
    /// focus, comme un groupe radio de Windows.
    pub fn mouse_down(&mut self, x: i32, y: i32) {
        self.pressed = None;
        if self.updates.mouse_down(x, y) || self.footer.mouse_down(x, y) {
            return;
        }
        self.pressed = self.target_at(x, y);
        if let Some(target) = self.pressed {
            self.focus = match target {
                Target::Language(_) => Stop::Language,
                Target::Dark(_) => Stop::Theme,
                Target::Auto(_) => Stop::Auto,
            };
        }
    }

    /// Relâchement : l'action choisie, si l'appui s'est fait au même endroit.
    pub fn mouse_up(&mut self, x: i32, y: i32) -> Option<SettingsAction> {
        if let Some(i) = self.updates.mouse_up(x, y) {
            return Some(if i == 0 {
                SettingsAction::CheckNow
            } else {
                SettingsAction::Install
            });
        }
        if self.footer.mouse_up(x, y).is_some() {
            return Some(SettingsAction::Close);
        }
        let pressed = self.pressed.take()?;
        if self.target_at(x, y) != Some(pressed) {
            return None;
        }
        Some(match pressed {
            Target::Language(l) => SettingsAction::Language(l),
            Target::Dark(d) => SettingsAction::Dark(d),
            Target::Auto(a) => SettingsAction::AutoUpdates(a),
        })
    }

    /// Dessine la fiche au centre du cadre, sur un voile.
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
        state: &SettingsState<'_>,
    ) {
        let progress = self.appear.progress();
        veil(frame, progress);
        let s = |v: f32| (v * dpi).round() as i32;
        let (fw, fh) = (frame.width as i32, frame.height as i32);
        let size = theme.font_size * dpi;
        let small = size * 0.92;
        let title_size = modal::title_size(theme, dpi);
        let width = modal::card_width(fw, 500.0, dpi);
        let pad = s(modal::PAD);
        let inner = width - 2 * pad;
        let title_h = (title_size * 1.3) as i32;
        let caption_h = (small * 1.7) as i32;
        let seg_h = s(30.0);
        let line_h = (size * 1.6) as i32;
        let button_h = s(30.0);
        let section_gap = s(18.0);
        let footer_h = s(modal::BUTTON_H);
        // Ce que fait la recherche automatique, dit une fois pour toutes :
        // qu'Acrux contacte le réseau, et que rien ne s'installe sans accord.
        let hint = if state.auto_updates {
            tr("Acrux cherche une version plus récente au démarrage, au plus une fois par jour. Rien n'est installé sans votre accord.")
        } else {
            tr("La recherche automatique est désactivée : Acrux ne contacte rien au démarrage.")
        };
        let hint_lines = modal::wrap(text, small, hint, inner as f32);
        let hint_h = (small * 1.45) as i32;
        // Trois sections (intitulé et groupe), ce que fait la recherche,
        // son état et ses boutons, puis le pied.
        let height = pad
            + title_h
            + s(14.0)
            + 3 * (caption_h + seg_h)
            + 2 * section_gap
            + s(8.0)
            + hint_h * hint_lines.len() as i32
            + s(6.0)
            + line_h
            + s(6.0)
            + button_h
            + s(24.0)
            + footer_h
            + pad;
        let (x, y) = modal::modal_rect(fw, fh, width, height, dpi, progress);
        modal::card(frame, theme, dpi, x, y, width, height, progress);
        let left = x + pad;
        let mut cy = y + pad;
        let baseline = cy + text.ascent(title_size) as i32;
        modal::title(frame, text, theme, dpi, left, baseline, tr("Paramètres"));
        cy += title_h + s(14.0);
        self.hits.clear();

        // Langue : « Système » dit laquelle il suit.
        let system = trf("Système ({})", &[native(state.system)]);
        let languages: Vec<(String, Target)> = LANGS
            .iter()
            .map(|&l| {
                let label = match l {
                    Lang::Auto => system.clone(),
                    other => native(other).to_string(),
                };
                (label, Target::Language(l))
            })
            .collect();
        cy = self.group(
            frame,
            text,
            theme,
            dpi,
            (left, cy),
            tr("Langue"),
            &languages,
            |t| t == Target::Language(state.language),
            Stop::Language,
        );
        cy += section_gap;

        let themes = [
            (tr("Clair").to_string(), Target::Dark(false)),
            (tr("Sombre").to_string(), Target::Dark(true)),
        ];
        cy = self.group(
            frame,
            text,
            theme,
            dpi,
            (left, cy),
            tr("Apparence"),
            &themes,
            |t| t == Target::Dark(state.dark),
            Stop::Theme,
        );
        cy += section_gap;

        let autos = [
            (tr("Chercher au démarrage").to_string(), Target::Auto(true)),
            (tr("Jamais").to_string(), Target::Auto(false)),
        ];
        cy = self.group(
            frame,
            text,
            theme,
            dpi,
            (left, cy),
            tr("Mises à jour"),
            &autos,
            |t| t == Target::Auto(state.auto_updates),
            Stop::Auto,
        );
        cy += s(8.0);
        for line in &hint_lines {
            let baseline = (cy + hint_h) as f32 - (hint_h as f32 - text.ascent(small)) / 2.0;
            text.draw(frame, left as f32, baseline, small, line, theme.text_dim);
            cy += hint_h;
        }
        cy += s(6.0);

        // L'état de la recherche : la version installée, puis ce qu'on sait.
        let installed = trf("Version installée : {}.", &[state.version]);
        let baseline = (cy + line_h) as f32 - (line_h as f32 - text.ascent(size)) / 2.0;
        text.draw(
            frame,
            left as f32,
            baseline,
            size,
            &installed,
            theme.text_dim,
        );
        let (status, color) = match state.update {
            UpdateLine::Idle => (String::new(), theme.text_dim),
            UpdateLine::Checking => (tr("Recherche en cours…").to_string(), theme.text_dim),
            UpdateLine::UpToDate => (tr("Acrux est à jour.").to_string(), theme.text_dim),
            UpdateLine::Available(v) => (trf("Acrux {} est disponible.", &[v]), theme.accent),
            UpdateLine::Failed(why) => (trf("Échec de la recherche : {}", &[why]), theme.danger),
        };
        let sx = left as f32 + text.measure(size, &installed) + s(6.0) as f32;
        text.draw_clipped(
            frame,
            sx,
            baseline,
            size,
            &status,
            color,
            (left + inner) as f32 - sx,
        );
        cy += line_h + s(6.0);
        let mut buttons = vec![RowButton {
            label: tr("Rechercher maintenant").into(),
            primary: false,
            enabled: state.update != UpdateLine::Checking,
        }];
        if matches!(state.update, UpdateLine::Available(_)) {
            buttons.push(RowButton::new(tr("Installer"), false));
        }
        self.updates.buttons = buttons;
        self.updates
            .layout_left(text, size, dpi, left, cy, button_h);
        let focus = match self.focus {
            Stop::Check if self.keyboard => Some(0),
            Stop::Install if self.keyboard => Some(1),
            _ => None,
        };
        self.updates.paint(frame, text, theme, dpi, focus);

        // Le pied : un seul bouton, le principal.
        self.footer.buttons = vec![RowButton::new(tr("Fermer"), true)];
        let by = y + height - pad - footer_h;
        self.footer
            .layout_right(text, size, dpi, x + width - pad, by, footer_h);
        let focus = (self.keyboard && self.focus == Stop::Close).then_some(0);
        self.footer.paint(frame, text, theme, dpi, focus);
    }

    /// Une section : son intitulé, puis son groupe de cases. Rend l'ordonnée
    /// qui suit.
    #[allow(clippy::too_many_arguments)] // cadre, texte, thème, échelle, place, contenu, état
    fn group(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
        (left, top): (i32, i32),
        caption: &str,
        items: &[(String, Target)],
        on: impl Fn(Target) -> bool,
        stop: Stop,
    ) -> i32 {
        let s = |v: f32| (v * dpi).round() as i32;
        let small = theme.font_size * dpi * 0.92;
        let caption_h = (small * 1.7) as i32;
        let seg_h = s(30.0);
        text.draw(
            frame,
            left as f32,
            top as f32 + text.ascent(small),
            small,
            caption,
            theme.text_dim,
        );
        let cy = top + caption_h;
        let segments: Vec<SegmentItem<'_>> = items
            .iter()
            .map(|(label, target)| SegmentItem {
                content: Segment::Label(label),
                on: on(*target),
                hovered: self.hover == Some(*target),
            })
            .collect();
        let (seg_w, rects) =
            controls::segmented(frame, text, theme, dpi, left, cy, seg_h, &segments);
        if self.keyboard && self.focus == stop {
            focus_ring(frame, left, cy, seg_w, seg_h, 7.0 * dpi, dpi, theme.accent);
        }
        for ((_, target), rect) in items.iter().zip(rects) {
            self.hits.push((rect, *target));
        }
        cy + seg_h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(update: UpdateLine<'static>) -> SettingsState<'static> {
        SettingsState {
            language: Lang::Auto,
            system: Lang::French,
            dark: true,
            auto_updates: true,
            update,
            version: "0.23.0",
        }
    }

    #[test]
    fn echap_ferme_la_fiche() {
        let mut sheet = SettingsSheet::new();
        let st = state(UpdateLine::Idle);
        assert_eq!(
            sheet.key(Key::Escape, false, &st),
            Some(SettingsAction::Close)
        );
    }

    #[test]
    fn les_fleches_changent_la_valeur_du_groupe_focalise() {
        let mut sheet = SettingsSheet::new();
        let st = state(UpdateLine::Idle);
        // Langue : de « Système » à « Français » ; rien avant « Système ».
        assert_eq!(
            sheet.key(Key::Right, false, &st),
            Some(SettingsAction::Language(Lang::French))
        );
        assert_eq!(sheet.key(Key::Left, false, &st), None);
        // Apparence : on est en sombre, la flèche gauche passe en clair.
        sheet.key(Key::Tab, false, &st);
        assert_eq!(
            sheet.key(Key::Left, false, &st),
            Some(SettingsAction::Dark(false))
        );
        assert_eq!(sheet.key(Key::Right, false, &st), None);
        // Mises à jour : « au démarrage », puis « jamais ».
        sheet.key(Key::Tab, false, &st);
        assert_eq!(
            sheet.key(Key::Right, false, &st),
            Some(SettingsAction::AutoUpdates(false))
        );
    }

    #[test]
    fn tab_saute_installer_sans_mise_a_jour() {
        let order = |update| {
            let st = state(update);
            let mut sheet = SettingsSheet::new();
            let mut seen = vec![sheet.focus];
            for _ in 0..6 {
                sheet.key(Key::Tab, false, &st);
                seen.push(sheet.focus);
            }
            seen
        };
        assert_eq!(
            order(UpdateLine::Idle),
            vec![
                Stop::Language,
                Stop::Theme,
                Stop::Auto,
                Stop::Check,
                Stop::Close,
                Stop::Language,
                Stop::Theme
            ]
        );
        assert!(order(UpdateLine::Available("9.0.0")).contains(&Stop::Install));
    }

    #[test]
    fn entree_ferme_ou_presse_le_bouton_focalise() {
        let mut sheet = SettingsSheet::new();
        let st = state(UpdateLine::Idle);
        assert_eq!(
            sheet.key(Key::Enter, false, &st),
            Some(SettingsAction::Close),
            "sur un groupe, Entrée ferme"
        );
        for _ in 0..3 {
            sheet.key(Key::Tab, false, &st);
        }
        assert_eq!(
            sheet.key(Key::Enter, false, &st),
            Some(SettingsAction::CheckNow)
        );
        // Pendant une recherche, le bouton ne répond pas.
        let busy = state(UpdateLine::Checking);
        assert_eq!(sheet.key(Key::Enter, false, &busy), None);
        sheet.key(Key::Tab, false, &st);
        assert_eq!(
            sheet.key(Key::Space, false, &st),
            Some(SettingsAction::Close)
        );
    }

    #[test]
    fn une_case_se_choisit_au_relachement() {
        let mut sheet = SettingsSheet::new();
        sheet.hits = vec![
            ((0, 0, 50, 30), Target::Dark(false)),
            ((50, 0, 50, 30), Target::Dark(true)),
        ];
        sheet.mouse_down(10, 10);
        assert_eq!(sheet.focus, Stop::Theme, "le groupe cliqué prend le focus");
        assert_eq!(sheet.mouse_up(60, 10), None, "relâchée ailleurs : rien");
        sheet.mouse_down(60, 10);
        assert_eq!(sheet.mouse_up(70, 20), Some(SettingsAction::Dark(true)));
        assert_eq!(sheet.mouse_up(70, 20), None, "un relâchement sans appui");
    }
}
