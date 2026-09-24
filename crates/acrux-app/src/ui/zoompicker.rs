//! Liste du zoom : ce que déroule la case « 125 % » de la barre d'outils, ou
//! le zoom de la barre d'état.
//!
//! Comme dans Acrobat, on y trouve les niveaux courants (50 à 400 %), les
//! trois ajustements (automatique, largeur, page entière) et un champ où
//! taper n'importe quel niveau, validé par Entrée. Le niveau en vigueur est
//! coché, et la liste s'ouvre dessus : Entrée seule le garde, les flèches
//! passent au voisin.
//!
//! C'est un [`Menu`] ouvert en liste déroulante ([`Menu::below`]) : mêmes
//! lignes, même clavier, même choix au relâchement que les menus du clic
//! droit. Il n'ajoute que le champ, dans la bande de tête du menu, et la
//! lecture de ce qu'on y tape ([`parse_zoom`]). Les libellés et raccourcis
//! des ajustements viennent de la palette : ils ne peuvent pas dire ici autre
//! chose que là.

// Coordonnées d'écran entières.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use acrux_graphics::Rasterizer;

use crate::platform::{Frame, Key};
use crate::ui::icons::Icon;
use crate::ui::input::{InputAction, TextInput};
use crate::ui::menu::{Menu, Outcome};
use crate::ui::paint::round_rect_outline;
use crate::ui::palette::{describe, Command};
use crate::ui::prefs::Fit;
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Rectangle `(x, y, largeur, hauteur)`.
type Rect = (i32, i32, i32, i32);

/// Niveaux proposés, en pourcent : ceux d'Acrobat, sans les extrêmes qu'on
/// atteint mieux au clavier ou à la molette.
pub const PRESETS: [u32; 8] = [50, 75, 100, 125, 150, 200, 300, 400];

/// Plus petit niveau accepté dans le champ, en pourcent.
pub const MIN_PERCENT: u32 = 10;

/// Plus grand niveau accepté dans le champ, en pourcent. Le visualiseur peut
/// plafonner plus bas pour un document aux pages immenses (voir
/// `Viewer::max_zoom`).
pub const MAX_PERCENT: u32 = 1600;

/// Hauteur du champ de saisie, en pixels logiques.
const FIELD: f32 = 30.0;

/// Ce qu'on choisit dans la liste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomChoice {
    /// Un niveau fixe, en pourcent.
    Percent(u32),
    /// Un ajustement à la fenêtre : automatique, largeur ou page entière.
    Fit(Fit),
}

/// Lit un niveau tapé : « 150 », « 150 % », « 150% », « 137,5 » ou
/// « 137.5 » (arrondi au pourcent). Rien d'autre : « abc », « 1,2,3 » ou un
/// champ vide rendent `None`. Le résultat est ramené entre [`MIN_PERCENT`] et
/// [`MAX_PERCENT`] : « 5 » donne 10 %, plutôt qu'un refus qui laisserait
/// chercher ce qui ne va pas.
#[must_use]
pub fn parse_zoom(typed: &str) -> Option<u32> {
    let number = typed
        .trim()
        .trim_end_matches('%')
        .trim_end()
        .replace(',', ".");
    let well_formed = !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit() || c == '.')
        && number.matches('.').count() <= 1;
    if !well_formed {
        return None;
    }
    let value: f64 = number.parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    let bounded = value
        .round()
        .clamp(f64::from(MIN_PERCENT), f64::from(MAX_PERCENT));
    Some(bounded as u32)
}

/// Élément en vigueur : l'ajustement s'il y en a un, sinon le niveau s'il
/// figure dans la liste. Un zoom fixe à 137 % n'en a pas : rien n'est coché,
/// et le champ montre la valeur.
fn current_choice(percent: u32, fit: Fit) -> Option<ZoomChoice> {
    match fit {
        Fit::Fixed => PRESETS
            .contains(&percent)
            .then_some(ZoomChoice::Percent(percent)),
        other => Some(ZoomChoice::Fit(other)),
    }
}

/// La liste du zoom ouverte.
pub struct ZoomPicker {
    menu: Menu<ZoomChoice>,
    /// Le champ où l'on tape un niveau. Vide à l'ouverture, avec le niveau
    /// en cours pour invite : taper remplace, sans rien à effacer d'abord.
    field: TextInput,
    /// Ce qui est tapé n'est pas un niveau : le champ se cercle de rouge
    /// jusqu'à la frappe suivante.
    error: bool,
}

impl ZoomPicker {
    /// Liste qui s'ouvrira sous le bouton `anchor` (au-dessus pour la barre
    /// d'état), le niveau `percent` et l'ajustement `fit` étant en vigueur.
    #[must_use]
    pub fn new(anchor: Rect, percent: u32, fit: Fit) -> Self {
        let current = current_choice(percent, fit);
        // La coche dit ce qui est en vigueur ; la colonne des icônes, que
        // le menu réserve dès qu'une ligne en a une, garde les libellés
        // alignés.
        let mark = |choice: ZoomChoice| (current == Some(choice)).then_some(Icon::Check);
        let mut menu = Menu::below(anchor).header(FIELD);
        for p in PRESETS {
            let choice = ZoomChoice::Percent(p);
            // 100 %, c'est la taille réelle : son raccourci est celui de
            // « Zoom 100 % » dans la palette.
            let keys = if p == 100 {
                shortcut(Command::ZoomReset)
            } else {
                ""
            };
            menu = menu.item(mark(choice), &format!("{p} %"), keys, choice, true);
        }
        menu = menu.separator();
        for (fit, command) in [
            (Fit::Automatic, Command::FitAutomatic),
            (Fit::Width, Command::FitWidth),
            (Fit::Page, Command::FitPage),
        ] {
            let choice = ZoomChoice::Fit(fit);
            let label = describe(command).map_or("", |(label, _)| label);
            menu = menu.item(mark(choice), label, shortcut(command), choice, true);
        }
        if let Some(choice) = current {
            menu.highlight_where(|c| c == choice);
        }
        let mut field = TextInput::new(&format!("{percent} %"));
        field.focused = true;
        Self {
            menu,
            field,
            error: false,
        }
    }

    /// Calcule la carte pour une fenêtre `win_w` × `win_h` (voir
    /// [`Menu::layout`]).
    pub fn layout(
        &mut self,
        measure: &mut dyn FnMut(f32, &str) -> f32,
        font: f32,
        dpi: f32,
        win_w: i32,
        win_h: i32,
    ) {
        self.menu.layout(measure, font, dpi, win_w, win_h);
    }

    /// Vrai si le point est sur la carte.
    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.menu.contains(x, y)
    }

    /// Vrai si le point est sur le champ de saisie (le pointeur y devient
    /// une barre d'insertion).
    #[must_use]
    pub fn over_field(&self, x: i32, y: i32) -> bool {
        let (fx, fy, fw, fh) = self.menu.head();
        x >= fx && x < fx + fw && y >= fy && y < fy + fh
    }

    /// Bouton enfoncé : dehors, la liste se referme ; dessus, elle attend
    /// le relâchement.
    pub fn mouse_down(&mut self, x: i32, y: i32) -> Outcome<ZoomChoice> {
        self.menu.mouse_down(x, y)
    }

    /// Bouton relâché : choisit la ligne sous le pointeur.
    pub fn mouse_up(&mut self, x: i32, y: i32) -> Outcome<ZoomChoice> {
        self.menu.mouse_up(x, y)
    }

    /// Déplacement du pointeur ; vrai s'il faut repeindre.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        self.menu.mouse_move(x, y)
    }

    /// Touche. Échap referme ; les flèches parcourent la liste ; Entrée
    /// valide ce qui est tapé s'il y a quelque chose, sinon la ligne mise en
    /// avant. Le reste (retour arrière, flèches horizontales…) va au champ.
    pub fn key(&mut self, key: Key) -> Outcome<ZoomChoice> {
        let typing = !self.field.value.is_empty();
        match key {
            Key::Escape => Outcome::Close,
            Key::Enter if typing => {
                if let Some(percent) = parse_zoom(&self.field.value) {
                    Outcome::Pick(ZoomChoice::Percent(percent))
                } else {
                    self.error = true;
                    Outcome::Stay
                }
            }
            // Une espace n'entre pas dans le champ : elle ne doit pas non
            // plus choisir la ligne en avant pendant qu'on tape un niveau.
            Key::Space if typing => Outcome::Stay,
            Key::Home | Key::End if !typing => self.menu.key(key),
            Key::Up | Key::Down | Key::PageUp | Key::PageDown | Key::Enter | Key::Space => {
                self.menu.key(key)
            }
            other => {
                if self.field.key(other, false) == InputAction::Changed {
                    self.error = false;
                }
                Outcome::Stay
            }
        }
    }

    /// Caractère tapé : chiffres, virgule, point et « % » vont au champ ;
    /// le reste est ignoré. Rend vrai si le caractère a été pris.
    pub fn char(&mut self, c: char) -> bool {
        if !(c.is_ascii_digit() || matches!(c, ',' | '.' | '%')) {
            return false;
        }
        self.field.insert_char(c);
        self.error = false;
        true
    }

    /// Dessine la liste, par-dessus tout ce qui est déjà peint. `layout`
    /// doit avoir été appelé pour la taille courante de la fenêtre.
    pub fn paint(
        &self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f32,
    ) {
        self.menu.paint(frame, text, raster, theme);
        let (x, y, w, h) = self.menu.head();
        if w <= 0 || h <= 0 {
            return;
        }
        self.field.draw(frame, text, theme, dpi, x, y, w, h);
        if self.error {
            let ring = (1.5 * dpi).max(1.0);
            round_rect_outline(frame, x, y, w, h, 7.0 * dpi, ring, theme.danger);
        }
    }
}

/// Raccourci d'une commande, tel que la palette l'écrit.
fn shortcut(command: Command) -> &'static str {
    describe(command).map_or("", |(_, keys)| keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mesure factice : un demi-cadratin par caractère.
    fn measure(size: f32, text: &str) -> f32 {
        #[allow(clippy::cast_precision_loss)] // quelques caractères
        let n = text.chars().count() as f32;
        n * size * 0.5
    }

    /// Liste ouverte sous une case (300, 10, 90, 30) d'une fenêtre 1000 × 800.
    fn open(percent: u32, fit: Fit) -> ZoomPicker {
        let mut z = ZoomPicker::new((300, 10, 90, 30), percent, fit);
        z.layout(&mut measure, 13.0, 1.0, 1000, 800);
        z
    }

    #[test]
    fn parse_zoom_lit_ce_qu_on_tape() {
        assert_eq!(parse_zoom("150"), Some(150));
        assert_eq!(parse_zoom("150 %"), Some(150));
        assert_eq!(parse_zoom("150%"), Some(150));
        assert_eq!(parse_zoom("  150  "), Some(150));
        assert_eq!(parse_zoom("137,5"), Some(138));
        assert_eq!(parse_zoom("137.4"), Some(137));
        // Hors bornes : ramené dedans plutôt que refusé.
        assert_eq!(parse_zoom("5"), Some(MIN_PERCENT));
        assert_eq!(parse_zoom("0"), Some(MIN_PERCENT));
        assert_eq!(parse_zoom("99999"), Some(MAX_PERCENT));
        assert_eq!(parse_zoom("1e9"), None, "pas de notation scientifique");
        for rien in ["", " ", "%", "abc", "1,2,3", "12a", "-50", ".", "1..2"] {
            assert_eq!(parse_zoom(rien), None, "« {rien} »");
        }
    }

    #[test]
    fn la_liste_s_ouvre_sur_le_niveau_en_cours() {
        let mut z = open(118, Fit::Page);
        // L'ajustement en vigueur est mis en avant : Entrée le garde.
        assert_eq!(z.key(Key::Enter), Outcome::Pick(ZoomChoice::Fit(Fit::Page)));
        let mut z = open(125, Fit::Fixed);
        assert_eq!(z.key(Key::Enter), Outcome::Pick(ZoomChoice::Percent(125)));
        // Un niveau hors de la liste : rien de coché, rien de mis en avant.
        let mut z = open(137, Fit::Fixed);
        assert_eq!(z.key(Key::Enter), Outcome::Stay);
        assert_eq!(current_choice(137, Fit::Fixed), None);
        assert_eq!(
            current_choice(100, Fit::Automatic),
            Some(ZoomChoice::Fit(Fit::Automatic))
        );
    }

    #[test]
    fn le_clavier_parcourt_la_liste_et_le_champ() {
        let mut z = open(100, Fit::Fixed);
        // 100 % en avant ; deux crans plus bas, 150 %.
        z.key(Key::Down);
        z.key(Key::Down);
        assert_eq!(z.key(Key::Enter), Outcome::Pick(ZoomChoice::Percent(150)));
        // Après 400 %, le séparateur est sauté : l'ajustement automatique.
        let mut z = open(400, Fit::Fixed);
        z.key(Key::Down);
        assert_eq!(
            z.key(Key::Enter),
            Outcome::Pick(ZoomChoice::Fit(Fit::Automatic))
        );
        assert_eq!(z.key(Key::Escape), Outcome::Close);

        // Ce qu'on tape passe avant la ligne mise en avant.
        let mut z = open(100, Fit::Fixed);
        for c in "300".chars() {
            assert!(z.char(c));
        }
        assert!(!z.char('x'), "une lettre n'entre pas");
        assert!(!z.char(' '), "l'espace non plus");
        assert_eq!(z.key(Key::Space), Outcome::Stay, "ni ne choisit la ligne");
        assert_eq!(z.key(Key::Enter), Outcome::Pick(ZoomChoice::Percent(300)));

        // « 0 » se lit 10 % : la borne basse, pas une erreur.
        let mut z = open(100, Fit::Fixed);
        z.char('0');
        assert_eq!(
            z.key(Key::Enter),
            Outcome::Pick(ZoomChoice::Percent(MIN_PERCENT))
        );

        // Une saisie illisible reste dans le champ, signalée ; effacer la
        // corrige.
        let mut z = open(100, Fit::Fixed);
        z.char('.');
        assert_eq!(z.key(Key::Enter), Outcome::Stay);
        assert!(z.error);
        z.key(Key::Backspace);
        assert!(!z.error);
        assert_eq!(z.key(Key::Enter), Outcome::Pick(ZoomChoice::Percent(100)));
    }

    #[test]
    fn la_souris_choisit_au_relachement_et_ferme_dehors() {
        let mut z = open(100, Fit::Automatic);
        assert_eq!(z.mouse_down(900, 700), Outcome::Close);
        let (x, y, w, _) = z.menu.head();
        assert!(z.over_field(x + 2, y + 2));
        assert_eq!(z.mouse_down(x + w / 2, y + 5), Outcome::Stay, "le champ");
        // Le premier élément est sous la bande de tête (30 px, puis 4 px
        // d'écart) : 50 %, pris en son milieu.
        let (cx, cy) = (x + w / 2, y + 30 + 4 + 15);
        assert!(!z.over_field(cx, cy));
        assert_eq!(z.mouse_down(cx, cy), Outcome::Stay);
        assert_eq!(z.mouse_up(cx, cy), Outcome::Pick(ZoomChoice::Percent(50)));
        assert!(z.contains(cx, cy));
    }
}
