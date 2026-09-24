//! Le modèle commun des cartes modales : la question ([`crate::ui::dialog`]),
//! l'invite de saisie, la fiche « Paramètres », la fenêtre « Protéger ».
//!
//! Chacune dessinait sa carte à sa façon : un rayon ici, une ombre là, des
//! boutons dans un ordre puis dans l'autre, un fondu pour l'une et pas pour
//! l'autre. Ce qui ne devrait pas se remarquer se remarquait. Tout ce qu'elles
//! partagent est donc écrit ici une fois :
//!
//! - la carte elle-même ([`card`], [`title`], [`modal_rect`]) : mêmes marges,
//!   même rayon, même ombre ;
//! - l'apparition ([`Appear`]) : un fondu de 120 ms qui **se termine
//!   toujours**, même sans événement pour relancer la peinture ;
//! - la rangée de boutons ([`ButtonRow`]) : groupée à droite, l'action
//!   principale **avant** « Annuler », comme sous Windows ; survol, curseur
//!   main, et déclenchement **au relâchement** — on se ravise en glissant
//!   hors du bouton ;
//! - l'invite de saisie ([`PromptCard`]) : un titre, un libellé, un champ, et
//!   Tab qui passe du champ aux boutons.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    // Un cadre, un rectangle, un thème, une échelle : les regrouper dans une
    // structure n'apprendrait rien et alourdirait chaque appel.
    clippy::too_many_arguments
)]

use std::time::{Duration, Instant};

use crate::platform::{Frame, Key};
use crate::ui::anim::ease_out;
use crate::ui::input::{InputAction, TextInput};
use crate::ui::lang::tr;
use crate::ui::paint::{button, round_rect_alpha, round_rect_outline, shadow, veil, ButtonLook};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Rayon des coins d'une carte (px logiques).
pub const CARD_RADIUS: f32 = 14.0;
/// Étalement de l'ombre (px logiques).
pub const SHADOW_SPREAD: f32 = 26.0;
/// Force de l'ombre, de 0 à 1.
pub const SHADOW_STRENGTH: f32 = 0.45;
/// Décalage de l'ombre vers le bas (px logiques) : la carte flotte au-dessus
/// de la page, éclairée d'en haut.
pub const SHADOW_DROP: f32 = 6.0;
/// Marge intérieure d'une carte (px logiques).
pub const PAD: f32 = 24.0;
/// Hauteur d'un bouton (px logiques).
pub const BUTTON_H: f32 = 34.0;
/// Largeur minimale d'un bouton : « OK » ne doit pas être un timbre-poste.
pub const BUTTON_MIN_W: f32 = 88.0;
/// Écart entre deux boutons d'une rangée (px logiques).
pub const BUTTON_GAP: f32 = 8.0;
/// Taille du titre, relative au texte d'interface.
pub const TITLE_SCALE: f32 = 1.3;
/// Largeur d'une carte ordinaire (px logiques).
pub const CARD_WIDTH: f32 = 460.0;

/// Durée de l'apparition. Assez pour voir d'où vient la carte, pas assez
/// pour l'attendre.
const APPEAR: Duration = Duration::from_millis(120);
/// Au-delà, une carte que rien n'a encore peinte (fenêtre réduite, par
/// exemple) apparaîtra d'un coup : personne ne regardait.
const LATE: Duration = Duration::from_secs(1);
/// Montée de la carte pendant son apparition (px logiques).
const RISE: f32 = 14.0;

/// L'apparition d'une carte : le voile s'assombrit, la carte monte de
/// quelques pixels en devenant opaque.
///
/// Le fondu part de la **première peinture**, pas de la création : une
/// carte ouverte au milieu d'un long traitement ne saute pas son fondu. Et
/// il se déclare fini seulement une fois une image peinte à 1 : tant que ce
/// n'est pas fait, [`Appear::animating`] reste vrai et le visualiseur
/// continue de repeindre. Sans cela, la dernière image pouvait être celle
/// d'une carte à demi transparente, qui le restait jusqu'au prochain
/// mouvement de souris.
#[derive(Debug, Clone, Copy)]
pub struct Appear {
    /// Création de la carte.
    created: Instant,
    /// Première peinture.
    start: Option<Instant>,
    /// Une image a été peinte à pleine opacité.
    settled: bool,
}

impl Default for Appear {
    fn default() -> Self {
        Self::new()
    }
}

impl Appear {
    /// Apparition qui commencera à la première peinture.
    #[must_use]
    pub fn new() -> Self {
        Self {
            created: Instant::now(),
            start: None,
            settled: false,
        }
    }

    /// Progression de 0 à 1, pour la peinture en cours.
    pub fn progress(&mut self) -> f32 {
        self.progress_at(Instant::now())
    }

    /// Progression à l'instant `now` (les tests fixent l'horloge).
    pub fn progress_at(&mut self, now: Instant) -> f32 {
        if self.settled {
            return 1.0;
        }
        if self.start.is_none() && now.saturating_duration_since(self.created) >= LATE {
            self.settled = true;
            return 1.0;
        }
        let start = *self.start.get_or_insert(now);
        let t = now.saturating_duration_since(start).as_secs_f64() / APPEAR.as_secs_f64();
        if t >= 1.0 {
            self.settled = true;
            return 1.0;
        }
        ease_out(t) as f32
    }

    /// Vrai tant qu'il reste une image à peindre : le visualiseur doit
    /// repeindre sans attendre d'événement.
    #[must_use]
    pub fn animating(&self) -> bool {
        !self.settled && (self.start.is_some() || self.created.elapsed() < LATE)
    }
}

/// Position d'une carte de `width` × `height` pixels, centrée dans le cadre,
/// un peu plus bas pendant son apparition.
#[must_use]
pub fn modal_rect(
    frame_w: i32,
    frame_h: i32,
    width: i32,
    height: i32,
    dpi: f32,
    progress: f32,
) -> (i32, i32) {
    let x = (frame_w - width) / 2;
    let rise = ((1.0 - progress) * RISE * dpi) as i32;
    let y = ((frame_h - height) / 2).max((20.0 * dpi) as i32) + rise;
    (x, y)
}

/// Largeur d'une carte de `logical` pixels logiques, ramenée à ce que le
/// cadre peut montrer.
#[must_use]
pub fn card_width(frame_w: i32, logical: f32, dpi: f32) -> i32 {
    let s = |v: f32| (v * dpi) as i32;
    s(logical).min(frame_w - s(40.0)).max(s(260.0))
}

/// La carte : ombre portée, fond aux coins arrondis, liseré. `alpha` va de
/// 0 à 1 pendant l'apparition.
pub fn card(
    frame: &mut Frame<'_>,
    theme: &Theme,
    dpi: f32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    alpha: f32,
) {
    let radius = CARD_RADIUS * dpi;
    shadow(
        frame,
        x,
        y + (SHADOW_DROP * dpi) as i32,
        w,
        h,
        radius,
        SHADOW_SPREAD * dpi,
        SHADOW_STRENGTH * alpha,
    );
    round_rect_alpha(frame, x, y, w, h, radius, theme.bar, alpha);
    round_rect_outline(frame, x, y, w, h, radius, dpi.max(1.0), theme.separator);
}

/// Taille du titre d'une carte.
#[must_use]
pub fn title_size(theme: &Theme, dpi: f32) -> f32 {
    theme.font_size * dpi * TITLE_SCALE
}

/// Le titre d'une carte, sur la ligne de base `baseline`.
pub fn title(
    frame: &mut Frame<'_>,
    text: &mut TextRenderer,
    theme: &Theme,
    dpi: f32,
    x: i32,
    baseline: i32,
    label: &str,
) {
    let size = title_size(theme, dpi);
    text.draw(frame, x as f32, baseline as f32, size, label, theme.text);
}

/// Coupe un texte en lignes tenant dans une largeur ; `\n` force la coupure.
pub fn wrap(text: &mut TextRenderer, size: f32, message: &str, width: f32) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in message.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if text.measure(size, &candidate) <= width || line.is_empty() {
                line = candidate;
            } else {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
            }
        }
        out.push(line);
    }
    out
}

/// Un rectangle `(x, y, largeur, hauteur)`.
pub type Rect = (i32, i32, i32, i32);

/// Vrai si le point est dans le rectangle.
#[must_use]
pub fn inside(r: Rect, x: i32, y: i32) -> bool {
    x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
}

/// Un bouton d'une rangée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowButton {
    /// Libellé : un verbe qui dit ce qui va se passer.
    pub label: String,
    /// L'action principale, mise en avant.
    pub primary: bool,
    /// Il répond ; grisé sinon.
    pub enabled: bool,
}

impl RowButton {
    /// Bouton actif.
    #[must_use]
    pub fn new(label: &str, primary: bool) -> Self {
        Self {
            label: label.to_string(),
            primary,
            enabled: true,
        }
    }
}

/// Une rangée de boutons, **la même partout**.
///
/// Les boutons se donnent dans l'ordre de Windows — l'action principale
/// d'abord, « Annuler » en dernier — et la rangée se groupe à droite.
///
/// Un bouton n'agit qu'**au relâchement**, et seulement si le pointeur est
/// encore dessus : appuyer, puis glisser hors du bouton, c'est se raviser.
/// L'appui ne fait que l'enfoncer.
#[derive(Debug, Clone, Default)]
pub struct ButtonRow {
    /// Les boutons, de gauche à droite.
    pub buttons: Vec<RowButton>,
    /// Bouton survolé.
    hover: Option<usize>,
    /// Bouton enfoncé, en attente du relâchement.
    pressed: Option<usize>,
    /// Rectangles des boutons, relevés à la mise en page.
    hits: Vec<Rect>,
}

impl ButtonRow {
    /// Rangée de ces boutons.
    #[must_use]
    pub fn new(buttons: Vec<RowButton>) -> Self {
        Self {
            buttons,
            ..Self::default()
        }
    }

    /// Place les boutons, alignés à droite sur `right`, en haut à `y` et
    /// hauts de `h` pixels. Rend l'abscisse du bord gauche du groupe.
    pub fn layout_right(
        &mut self,
        text: &mut TextRenderer,
        size: f32,
        dpi: f32,
        right: i32,
        y: i32,
        h: i32,
    ) -> i32 {
        let widths = self.widths(text, size, dpi);
        self.layout_with_widths(&widths, right, y, h, (BUTTON_GAP * dpi) as i32)
    }

    /// Largeur de la rangée entière, écarts compris.
    pub fn natural_width(&self, text: &mut TextRenderer, size: f32, dpi: f32) -> i32 {
        let widths = self.widths(text, size, dpi);
        let gap = (BUTTON_GAP * dpi) as i32;
        widths.iter().sum::<i32>() + gap * (widths.len() as i32 - 1).max(0)
    }

    /// Place les boutons alignés à gauche à partir de `left`. Rend l'abscisse
    /// qui suit le dernier.
    pub fn layout_left(
        &mut self,
        text: &mut TextRenderer,
        size: f32,
        dpi: f32,
        left: i32,
        y: i32,
        h: i32,
    ) -> i32 {
        let widths = self.widths(text, size, dpi);
        let gap = (BUTTON_GAP * dpi) as i32;
        let total = widths.iter().sum::<i32>() + gap * (widths.len() as i32 - 1).max(0);
        self.layout_with_widths(&widths, left + total, y, h, gap);
        left + total
    }

    /// Largeur de chaque bouton : son libellé, de l'air, et un minimum.
    fn widths(&self, text: &mut TextRenderer, size: f32, dpi: f32) -> Vec<i32> {
        self.buttons
            .iter()
            .map(|b| {
                (text.measure(size, &b.label) as i32 + (32.0 * dpi) as i32)
                    .max((BUTTON_MIN_W * dpi) as i32)
            })
            .collect()
    }

    /// Place des boutons de largeurs connues, groupés à droite de `right`.
    /// Rend l'abscisse du bord gauche du groupe.
    pub fn layout_with_widths(
        &mut self,
        widths: &[i32],
        right: i32,
        y: i32,
        h: i32,
        gap: i32,
    ) -> i32 {
        let total = widths.iter().sum::<i32>() + gap * (widths.len() as i32 - 1).max(0);
        let left = right - total;
        self.hits.clear();
        let mut x = left;
        for w in widths {
            self.hits.push((x, y, *w, h));
            x += w + gap;
        }
        left
    }

    /// Dessine les boutons là où la mise en page les a placés ; `focus` est
    /// le bouton qui a le focus clavier, s'il y en a un.
    pub fn paint(
        &self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
        focus: Option<usize>,
    ) {
        let size = theme.font_size * dpi;
        for (i, (b, &(x, y, w, h))) in self.buttons.iter().zip(&self.hits).enumerate() {
            let hovered = b.enabled && self.hover == Some(i);
            let ink = button(
                frame,
                x,
                y,
                w,
                h,
                dpi,
                theme,
                ButtonLook {
                    primary: b.primary,
                    hovered,
                    focused: focus == Some(i),
                    disabled: !b.enabled,
                    // Enfoncé tant que le pointeur reste dessus : glissé
                    // dehors, il remonte, et c'est ce qui dit qu'on se ravise.
                    pressed: hovered && self.pressed == Some(i),
                },
            );
            let lw = text.measure(size, &b.label);
            text.draw(
                frame,
                x as f32 + (w as f32 - lw) / 2.0,
                y as f32 + f32::midpoint(h as f32, text.ascent(size)) - 1.0,
                size,
                &b.label,
                ink,
            );
        }
    }

    /// Bouton actif sous un point, s'il y en a un.
    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Option<usize> {
        self.hits
            .iter()
            .position(|r| inside(*r, x, y))
            .filter(|&i| self.buttons.get(i).is_some_and(|b| b.enabled))
    }

    /// Survol ; rend vrai si l'affichage doit changer.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let over = self.hit(x, y);
        let changed = over != self.hover;
        self.hover = over;
        changed
    }

    /// Le pointeur a quitté la rangée ; vrai si l'affichage doit changer.
    pub fn leave(&mut self) -> bool {
        self.hover.take().is_some()
    }

    /// Appui : enfonce le bouton visé. Rend vrai si c'en était un.
    pub fn mouse_down(&mut self, x: i32, y: i32) -> bool {
        self.pressed = self.hit(x, y);
        self.hover = self.pressed;
        self.pressed.is_some()
    }

    /// Relâchement : le bouton choisi, s'il a été enfoncé **et** que le
    /// pointeur est encore dessus. Un relâchement sans appui (le clic qui a
    /// ouvert la carte, par exemple) ne choisit rien.
    pub fn mouse_up(&mut self, x: i32, y: i32) -> Option<usize> {
        let pressed = self.pressed.take()?;
        (self.hit(x, y) == Some(pressed)).then_some(pressed)
    }

    /// Vrai si un bouton attend son relâchement.
    #[must_use]
    pub fn is_pressed(&self) -> bool {
        self.pressed.is_some()
    }
}

/// Où est le focus clavier d'une invite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptFocus {
    /// Le champ de saisie.
    Field,
    /// Un bouton : 0 valide, 1 annule.
    Button(usize),
}

/// Ce que demande l'invite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptAct {
    /// Valider la saisie.
    Submit,
    /// Fermer sans rien faire.
    Cancel,
}

/// Ce qu'une invite affiche : son titre, son libellé, son champ et l'erreur
/// de la dernière saisie.
pub struct PromptContent<'a> {
    /// Titre.
    pub title: &'a str,
    /// Libellé, coupé à la largeur de la carte.
    pub label: &'a str,
    /// Le champ.
    pub input: &'a TextInput,
    /// Erreur à afficher sous le champ.
    pub error: Option<&'a str>,
}

/// La carte d'une invite de saisie : « Valider » et « Annuler », le focus
/// qui passe du champ aux boutons, l'apparition.
///
/// Le contenu (titre, champ, erreur) appartient à l'appelant, qui sait quoi
/// faire de la saisie ; cette carte ne tient que ce qui est commun à toutes
/// les invites.
#[derive(Debug, Clone)]
pub struct PromptCard {
    /// Élément qui a le focus clavier.
    pub focus: PromptFocus,
    /// « Valider », « Annuler ».
    pub buttons: ButtonRow,
    /// Apparition.
    pub appear: Appear,
    /// Rectangle du champ, relevé au dessin.
    field: Rect,
}

impl Default for PromptCard {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptCard {
    /// Carte neuve, focus dans le champ.
    #[must_use]
    pub fn new() -> Self {
        Self {
            focus: PromptFocus::Field,
            buttons: ButtonRow::new(vec![
                RowButton::new(tr("Valider"), true),
                RowButton::new(tr("Annuler"), false),
            ]),
            appear: Appear::new(),
            field: (0, 0, 0, 0),
        }
    }

    /// Place le focus, et allume ou éteint le caret du champ avec lui.
    pub fn set_focus(&mut self, focus: PromptFocus, input: &mut TextInput) {
        self.focus = focus;
        input.focused = focus == PromptFocus::Field;
    }

    /// Tab (ou Maj+Tab) : champ → Valider → Annuler → champ.
    pub fn tab(&mut self, back: bool, input: &mut TextInput) {
        let order = [
            PromptFocus::Field,
            PromptFocus::Button(0),
            PromptFocus::Button(1),
        ];
        let at = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let next = if back { (at + 2) % 3 } else { (at + 1) % 3 };
        self.set_focus(order[next], input);
    }

    /// Ce que fait Entrée : valider, sauf sur « Annuler ».
    #[must_use]
    pub fn activate(&self) -> PromptAct {
        match self.focus {
            PromptFocus::Button(1) => PromptAct::Cancel,
            _ => PromptAct::Submit,
        }
    }

    /// Touche. Rend l'acte demandé, s'il y en a un ; `error` s'efface dès
    /// que la saisie change.
    pub fn key(
        &mut self,
        key: Key,
        shift: bool,
        input: &mut TextInput,
        error: &mut Option<String>,
    ) -> Option<PromptAct> {
        match key {
            Key::Tab => self.tab(shift, input),
            Key::Escape => return Some(PromptAct::Cancel),
            Key::Enter => return Some(self.activate()),
            // Sur un bouton, l'espace le presse ; dans le champ, c'est un
            // caractère, qui arrive par ailleurs.
            Key::Space if self.focus != PromptFocus::Field => return Some(self.activate()),
            Key::Left | Key::Right if self.focus != PromptFocus::Field => {
                let other = match self.focus {
                    PromptFocus::Button(0) => 1,
                    _ => 0,
                };
                self.set_focus(PromptFocus::Button(other), input);
            }
            _ if self.focus == PromptFocus::Field
                && input.key(key, shift) == InputAction::Changed =>
            {
                *error = None;
            }
            _ => {}
        }
        None
    }

    /// Caractère tapé : il ne va au champ que si le focus y est.
    pub fn char(&self, c: char, input: &mut TextInput, error: &mut Option<String>) {
        if self.focus == PromptFocus::Field && input.insert_char(c) == InputAction::Changed {
            *error = None;
        }
    }

    /// Appui de la souris : un bouton s'enfonce, le champ prend le focus.
    pub fn mouse_down(&mut self, x: i32, y: i32, input: &mut TextInput) {
        if !self.buttons.mouse_down(x, y) && inside(self.field, x, y) {
            self.set_focus(PromptFocus::Field, input);
        }
    }

    /// Relâchement : l'acte du bouton choisi, s'il y en a un.
    pub fn mouse_up(&mut self, x: i32, y: i32) -> Option<PromptAct> {
        match self.buttons.mouse_up(x, y)? {
            0 => Some(PromptAct::Submit),
            _ => Some(PromptAct::Cancel),
        }
    }

    /// Vrai si le point est sur le champ.
    #[must_use]
    pub fn over_field(&self, x: i32, y: i32) -> bool {
        inside(self.field, x, y)
    }

    /// Dessine l'invite au centre du cadre, sur un voile. La hauteur suit le
    /// libellé, qui se coupe sur autant de lignes qu'il faut.
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
        content: &PromptContent<'_>,
    ) {
        let progress = self.appear.progress();
        veil(frame, progress);
        let s = |v: f32| (v * dpi) as i32;
        let (fw, fh) = (frame.width as i32, frame.height as i32);
        let size = theme.font_size * dpi;
        let title_size = title_size(theme, dpi);
        let width = card_width(fw, CARD_WIDTH, dpi);
        let pad = s(PAD);
        let inner = width - 2 * pad;
        let lines = wrap(text, size, content.label, inner as f32);
        let line_h = (size * 1.45) as i32;
        let field_h = s(32.0);
        let button_h = s(BUTTON_H);
        let title_h = (title_size * 1.3) as i32;
        let error_h = if content.error.is_some() {
            s(8.0) + line_h
        } else {
            0
        };
        let height = pad
            + title_h
            + s(6.0)
            + line_h * lines.len() as i32
            + s(12.0)
            + field_h
            + error_h
            + s(24.0)
            + button_h
            + pad;
        let (x, y) = modal_rect(fw, fh, width, height, dpi, progress);
        card(frame, theme, dpi, x, y, width, height, progress);
        let left = x + pad;
        let mut cy = y + pad;
        let baseline = cy + text.ascent(title_size) as i32;
        title(frame, text, theme, dpi, left, baseline, content.title);
        cy += title_h + s(6.0);
        for line in &lines {
            text.draw(
                frame,
                left as f32,
                (cy + line_h) as f32 - (line_h as f32 - text.ascent(size)) / 2.0,
                size,
                line,
                theme.text_dim,
            );
            cy += line_h;
        }
        cy += s(12.0);
        content
            .input
            .draw(frame, text, theme, dpi, left, cy, inner, field_h);
        self.field = (left, cy, inner, field_h);
        cy += field_h;
        if let Some(e) = content.error {
            cy += s(8.0);
            text.draw_clipped(
                frame,
                left as f32,
                (cy + line_h) as f32 - (line_h as f32 - text.ascent(size)) / 2.0,
                size,
                e,
                theme.danger,
                inner as f32,
            );
        }
        let by = y + height - pad - button_h;
        self.buttons
            .layout_right(text, size, dpi, x + width - pad, by, button_h);
        let focus = match self.focus {
            PromptFocus::Button(i) => Some(i),
            PromptFocus::Field => None,
        };
        self.buttons.paint(frame, text, theme, dpi, focus);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_fondu_se_termine_et_se_declare_fini() {
        let mut a = Appear::new();
        let t0 = Instant::now();
        assert!(
            a.progress_at(t0) < 0.01,
            "la première image est transparente"
        );
        assert!(a.animating(), "il reste des images à peindre");
        let mid = a.progress_at(t0 + Duration::from_millis(60));
        assert!(mid > 0.0 && mid < 1.0, "à mi-course : {mid}");
        assert!(a.animating());
        assert!((a.progress_at(t0 + Duration::from_millis(130)) - 1.0).abs() < f32::EPSILON);
        assert!(!a.animating(), "une image pleine a été peinte : c'est fini");
        // Et le reste.
        assert!((a.progress_at(t0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn le_fondu_part_de_la_premiere_peinture() {
        let mut a = Appear::new();
        let late = Instant::now() + Duration::from_millis(300);
        assert!(
            a.progress_at(late) < 0.01,
            "peinte tard, la carte fait quand même son entrée"
        );
        assert!(a.animating());
    }

    #[test]
    fn une_carte_jamais_peinte_ne_reveille_pas_indefiniment() {
        let mut a = Appear::new();
        if let Some(earlier) = a.created.checked_sub(LATE) {
            a.created = earlier;
        }
        assert!(!a.animating(), "rien ne la montre : inutile de repeindre");
        assert!((a.progress_at(Instant::now()) - 1.0).abs() < f32::EPSILON);
    }

    /// Deux boutons de 100 et 90 pixels, groupés à droite de 500.
    fn row() -> ButtonRow {
        let mut r = ButtonRow::new(vec![
            RowButton::new("Valider", true),
            RowButton::new("Annuler", false),
        ]);
        r.layout_with_widths(&[100, 90], 500, 10, 30, 8);
        r
    }

    #[test]
    fn le_groupe_est_aligne_a_droite_principal_en_premier() {
        let r = row();
        assert_eq!(r.hits, vec![(302, 10, 100, 30), (410, 10, 90, 30)]);
        assert_eq!(r.hit(310, 20), Some(0), "le principal est à gauche");
        assert_eq!(r.hit(499, 20), Some(1), "« Annuler » ferme la marche");
        assert_eq!(r.hit(405, 20), None, "l'écart n'est à personne");
    }

    #[test]
    fn un_bouton_se_declenche_au_relachement_seulement() {
        let mut r = row();
        assert!(r.mouse_down(310, 20));
        assert!(r.is_pressed());
        assert_eq!(r.mouse_up(320, 25), Some(0));
        assert!(!r.is_pressed());
    }

    #[test]
    fn glisser_hors_du_bouton_annule() {
        let mut r = row();
        assert!(r.mouse_down(310, 20));
        assert_eq!(r.mouse_up(10, 20), None, "relâché dehors : on s'est ravisé");
        assert!(r.mouse_down(310, 20));
        assert_eq!(
            r.mouse_up(450, 20),
            None,
            "relâché sur l'autre : rien non plus"
        );
    }

    #[test]
    fn un_relachement_sans_appui_ne_fait_rien() {
        let mut r = row();
        assert_eq!(r.mouse_up(310, 20), None);
    }

    #[test]
    fn un_bouton_desactive_ne_se_presse_pas() {
        let mut r = row();
        r.buttons[0].enabled = false;
        assert!(!r.mouse_down(310, 20));
        assert_eq!(r.mouse_up(310, 20), None);
        assert!(!r.mouse_move(310, 20), "ni ne se survole");
    }

    #[test]
    fn tab_parcourt_champ_puis_boutons() {
        let mut card = PromptCard::new();
        let mut input = TextInput::new("");
        assert_eq!(card.focus, PromptFocus::Field);
        card.tab(false, &mut input);
        assert_eq!(card.focus, PromptFocus::Button(0));
        assert!(!input.focused, "le caret s'éteint hors du champ");
        card.tab(false, &mut input);
        assert_eq!(card.focus, PromptFocus::Button(1));
        card.tab(false, &mut input);
        assert_eq!(card.focus, PromptFocus::Field);
        assert!(input.focused);
        card.tab(true, &mut input);
        assert_eq!(card.focus, PromptFocus::Button(1), "Maj+Tab recule");
    }

    #[test]
    fn entree_suit_le_focus_et_echap_annule() {
        let mut card = PromptCard::new();
        let mut input = TextInput::new("");
        let mut error = None;
        let mut key = |card: &mut PromptCard, k: Key| card.key(k, false, &mut input, &mut error);
        assert_eq!(key(&mut card, Key::Enter), Some(PromptAct::Submit));
        assert_eq!(key(&mut card, Key::Escape), Some(PromptAct::Cancel));
        key(&mut card, Key::Tab);
        key(&mut card, Key::Right);
        assert_eq!(card.focus, PromptFocus::Button(1));
        assert_eq!(key(&mut card, Key::Enter), Some(PromptAct::Cancel));
        assert_eq!(key(&mut card, Key::Space), Some(PromptAct::Cancel));
    }

    #[test]
    fn on_ne_tape_pas_dans_le_champ_depuis_un_bouton() {
        let mut card = PromptCard::new();
        let mut input = TextInput::new("");
        let mut error = Some("Mot de passe incorrect".to_string());
        card.char('a', &mut input, &mut error);
        assert_eq!(input.value, "a");
        assert!(error.is_none(), "l'erreur s'efface à la frappe");
        card.tab(false, &mut input);
        card.char('z', &mut input, &mut error);
        assert_eq!(input.value, "a", "le focus est sur « Valider »");
    }
}
