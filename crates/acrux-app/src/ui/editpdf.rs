//! Mode « Modifier le PDF » : tout le texte de la page devient éditable.
//!
//! C'est le mode d'Acrobat. On ne sélectionne rien d'abord : on entre dans le
//! mode, chaque paragraphe s'encadre, et un clic dedans y pose le curseur. On
//! tape, on efface, on se déplace comme dans un traitement de texte, et le
//! paragraphe se recompose **à chaque frappe**, dans sa vraie police, sur son
//! vrai fond — ce qu'on voit est ce qui sera enregistré. « Ajouter du texte »
//! pose une zone neuve là où l'on clique.
//!
//! Ce module tient ce qui ne dépend pas du document : la saisie ([`Buffer`] :
//! texte, curseur, sélection) et la barre du mode ([`EditBar`]). Le
//! visualiseur y branche le document, la mise en page et le rendu.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    // La barre se dessine de gauche à droite en une passe, groupe après
    // groupe : la découper en fonctions obligerait à se passer la position
    // courante de l'une à l'autre sans rien gagner en clarté. Et `x`, `y`,
    // `w`, `h`, `b` sont les noms que tout le monde attend pour un rectangle.
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

use acrux_graphics::Rasterizer;

use crate::platform::{Frame, Key};
use crate::ui::controls::{self, Segment, SegmentItem};
use crate::ui::paint::round_rect;
use crate::ui::pickers::{ColorPicker, FontPicker, Outcome};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Outil actif du mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditTool {
    /// Cliquer dans un paragraphe pour le modifier.
    #[default]
    Select,
    /// Cliquer sur la page pour y poser une zone de texte neuve.
    AddText,
}

/// Ce qu'un clic dans la barre demande.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BarAction {
    /// Changer d'outil.
    Tool(EditTool),
    /// Réduire le corps.
    Smaller,
    /// Augmenter le corps.
    Larger,
    /// Ouvrir ou refermer le nuancier.
    Colors,
    /// Appliquer une couleur. `true` : le choix est fait, elle rejoint les
    /// récentes ; `false` : on glisse encore dans le nuancier.
    Ink([f64; 3], bool),
    /// Ouvrir ou refermer la liste des polices.
    Families,
    /// Choisir une police : un rang dans le catalogue du système
    /// ([`acrux_features::sysfonts::families`]), ou rien pour laisser au bloc
    /// la sienne.
    Family(Option<usize>),
    /// Un sélecteur a bougé ou s'est refermé : il n'y a qu'à repeindre.
    Refresh,
    /// Gras.
    Bold,
    /// Italique.
    Italic,
    /// Alignement du bloc.
    Align(usize),
    /// Interligne plus serré.
    Tighter,
    /// Interligne plus large.
    Looser,
    /// Quitter le mode.
    Close,
}

/// Alignements, dans l'ordre des boutons.
pub const ALIGNMENTS: [&str; 4] = ["gauche", "centré", "droite", "justifié"];

/// Corps proposés, en points.
const SIZES: [f64; 12] = [
    6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 14.0, 16.0, 18.0, 24.0, 36.0,
];

/// Corps suivant dans la gamme, vers le haut ou vers le bas.
#[must_use]
pub fn step_size(size: f64, up: bool) -> f64 {
    if up {
        SIZES
            .iter()
            .copied()
            .find(|s| *s > size + 0.05)
            .unwrap_or(size * 1.25)
            .min(144.0)
    } else {
        SIZES
            .iter()
            .rev()
            .copied()
            .find(|s| *s < size - 0.05)
            .unwrap_or(size / 1.25)
            .max(4.0)
    }
}

/// Texte en cours d'édition : curseur et ancre de sélection, en indices de
/// **caractères** (pas d'octets : un « é » compte pour un).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Buffer {
    /// Texte.
    pub text: String,
    /// Position du curseur.
    pub caret: usize,
    /// Autre bout de la sélection ; égal au curseur sans sélection.
    pub anchor: usize,
}

impl Buffer {
    /// Texte neuf, curseur à la fin.
    #[must_use]
    pub fn new(text: &str) -> Self {
        let n = text.chars().count();
        Self {
            text: text.to_string(),
            caret: n,
            anchor: n,
        }
    }

    /// Nombre de caractères.
    #[must_use]
    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// Vrai pour un texte vide.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Vrai s'il y a une sélection.
    #[must_use]
    pub fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }

    /// Bornes de la sélection, dans l'ordre.
    #[must_use]
    pub fn range(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    /// Texte sélectionné.
    #[must_use]
    pub fn selected(&self) -> String {
        let (a, b) = self.range();
        self.text.chars().skip(a).take(b - a).collect()
    }

    /// Octet correspondant à un indice de caractère.
    fn byte(&self, index: usize) -> usize {
        self.text
            .char_indices()
            .nth(index)
            .map_or(self.text.len(), |(b, _)| b)
    }

    /// Remplace la sélection (ou insère au curseur) par un texte.
    pub fn insert(&mut self, s: &str) {
        let (a, b) = self.range();
        let (ba, bb) = (self.byte(a), self.byte(b));
        // Les caractères de contrôle n'ont rien à faire dans un paragraphe,
        // sauf le retour à la ligne ; une tabulation devient une espace.
        let clean: String = s
            .chars()
            .filter_map(|c| match c {
                '\r' => None,
                '\t' => Some(' '),
                '\n' => Some('\n'),
                c if c.is_control() => None,
                c => Some(c),
            })
            .collect();
        self.text.replace_range(ba..bb, &clean);
        self.caret = a + clean.chars().count();
        self.anchor = self.caret;
    }

    /// Retour arrière : efface la sélection, ou le caractère qui précède.
    pub fn backspace(&mut self) {
        if self.has_selection() {
            self.insert("");
        } else if self.caret > 0 {
            self.anchor = self.caret - 1;
            self.insert("");
        }
    }

    /// Suppr : efface la sélection, ou le caractère qui suit.
    pub fn delete(&mut self) {
        if self.has_selection() {
            self.insert("");
        } else if self.caret < self.len() {
            self.anchor = self.caret + 1;
            self.insert("");
        }
    }

    /// Place le curseur ; avec `extend`, étend la sélection au lieu de la
    /// quitter.
    pub fn move_to(&mut self, index: usize, extend: bool) {
        self.caret = index.min(self.len());
        if !extend {
            self.anchor = self.caret;
        }
    }

    /// Un caractère à gauche. Sans Maj, une sélection se referme sur son
    /// bord gauche, comme partout.
    pub fn left(&mut self, extend: bool) {
        if !extend && self.has_selection() {
            let (a, _) = self.range();
            self.move_to(a, false);
        } else {
            self.move_to(self.caret.saturating_sub(1), extend);
        }
    }

    /// Un caractère à droite.
    pub fn right(&mut self, extend: bool) {
        if !extend && self.has_selection() {
            let (_, b) = self.range();
            self.move_to(b, false);
        } else {
            self.move_to(self.caret + 1, extend);
        }
    }

    /// Mot précédent (Ctrl+←).
    pub fn word_left(&mut self, extend: bool) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.caret.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.move_to(i, extend);
    }

    /// Mot suivant (Ctrl+→).
    pub fn word_right(&mut self, extend: bool) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.caret.min(chars.len());
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        self.move_to(i, extend);
    }

    /// Tout sélectionner.
    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.len();
    }

    /// Sélectionne le mot sous un indice (double-clic).
    pub fn select_word(&mut self, index: usize) {
        let chars: Vec<char> = self.text.chars().collect();
        let i = index.min(chars.len());
        let is_word = |c: char| c.is_alphanumeric() || c == '\'' || c == '’' || c == '-';
        let mut a = i;
        while a > 0 && is_word(chars[a - 1]) {
            a -= 1;
        }
        let mut b = i;
        while b < chars.len() && is_word(chars[b]) {
            b += 1;
        }
        self.anchor = a;
        self.caret = b;
    }
}

/// Un état du texte d'une saisie : de quoi revenir en arrière sans toucher
/// au document. Le mode « Modifier le PDF » et la saisie dans un champ de
/// formulaire en tiennent chacun une pile.
#[derive(Debug, Clone)]
pub struct Step {
    /// Texte.
    pub text: String,
    /// Curseur.
    pub caret: usize,
    /// Autre bout de la sélection.
    pub anchor: usize,
}

/// Nature d'un geste de frappe : deux gestes de même nature qui se suivent
/// n'en font qu'un pour l'annulation — on défait un mot, pas une lettre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    /// Rien encore.
    None,
    /// Des caractères ajoutés.
    Insert,
    /// Des caractères retirés.
    Delete,
}

/// Nature d'un geste de frappe, et s'il **ouvre une étape** d'annulation.
///
/// La règle est celle d'un traitement de texte : les caractères qui se
/// suivent forment une étape, et l'on coupe quand l'utilisateur change de
/// geste (frapper puis effacer), qu'il déplace le curseur, ou qu'il tape un
/// blanc ou une ponctuation — ainsi Ctrl+Z défait un mot, pas une lettre, et
/// jamais toute une phrase.
#[must_use]
pub fn step_of(
    before: &Buffer,
    after: &Buffer,
    last: StepKind,
    last_caret: usize,
) -> (StepKind, bool) {
    let grew = after.text.chars().count() > before.text.chars().count();
    let kind = if grew {
        StepKind::Insert
    } else {
        StepKind::Delete
    };
    let typed_break = grew
        && after
            .text
            .chars()
            .nth(before.caret)
            .is_some_and(|c| c.is_whitespace() || c.is_ascii_punctuation());
    let jumped = before.caret != last_caret;
    (kind, kind != last || jumped || typed_break)
}

/// Le sélecteur déroulé sous la barre : un seul à la fois.
enum Popup {
    Fonts(Box<FontPicker>),
    Colors(Box<ColorPicker>),
}

/// Barre du mode, sous la barre d'outils.
// Gras, italique, corps imposé, couleur imposée, bloc ouvert : cinq
// interrupteurs indépendants, qu'un regroupement artificiel n'éclaircirait
// pas.
#[allow(clippy::struct_excessive_bools)]
pub struct EditBar {
    /// Outil actif.
    pub tool: EditTool,
    /// Corps du texte ajouté, en points.
    pub size: f64,
    /// Couleur du texte ajouté, ou du bloc ouvert.
    pub color: [f64; 3],
    /// Corps du paragraphe en cours, à afficher à la place du défaut.
    pub active_size: Option<f64>,
    /// Police du bloc en cours : rang dans le catalogue du système, ou rien
    /// quand le bloc garde la sienne.
    pub family: Option<usize>,
    /// Police **d'origine** du bloc ouvert, lue dans le document (« Georgia ») :
    /// c'est elle que le bouton montre tant qu'aucune autre n'est choisie.
    pub native: Option<String>,
    /// Gras du bloc en cours.
    pub bold: bool,
    /// Italique du bloc en cours.
    pub italic: bool,
    /// Alignement du bloc en cours (rang dans [`ALIGNMENTS`]).
    pub align: usize,
    /// Interligne du bloc en cours, en corps (1,2 = 120 %).
    pub leading: f64,
    /// Un bloc est ouvert : les réglages agissent sur lui.
    pub editing: bool,
    /// Le corps a été choisi dans la barre : il s'impose au texte ajouté,
    /// qui sinon prend celui du texte voisin.
    pub size_set: bool,
    /// La couleur a été choisie dans la barre (même règle).
    pub color_set: bool,
    /// Zones cliquables, remplies au dessin.
    hits: Vec<(i32, i32, i32, i32, BarAction)>,
    /// Zone survolée, rang dans `hits`.
    hover: Option<usize>,
    /// Le sélecteur déroulé : liste des polices ou nuancier.
    popup: Option<Popup>,
    /// Les boutons de police et de couleur, tels qu'ils ont été dessinés :
    /// leur sélecteur s'accroche dessous.
    font_anchor: (i32, i32, i32, i32),
    color_anchor: (i32, i32, i32, i32),
}

impl std::fmt::Debug for EditBar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditBar")
            .field("tool", &self.tool)
            .field("editing", &self.editing)
            .finish_non_exhaustive()
    }
}

impl Default for EditBar {
    fn default() -> Self {
        Self {
            tool: EditTool::Select,
            size: 12.0,
            color: [0.0, 0.0, 0.0],
            active_size: None,
            family: None,
            native: None,
            bold: false,
            italic: false,
            align: 0,
            leading: 1.2,
            editing: false,
            size_set: false,
            color_set: false,
            hits: Vec::new(),
            hover: None,
            popup: None,
            font_anchor: (0, 0, 0, 0),
            color_anchor: (0, 0, 0, 0),
        }
    }
}

impl EditBar {
    /// Barre neuve sur un outil.
    #[must_use]
    pub fn with_tool(tool: EditTool) -> Self {
        Self {
            tool,
            ..Self::default()
        }
    }

    /// Hauteur de la barre en pixels.
    #[must_use]
    pub fn height(dpi: f32) -> i32 {
        (44.0 * dpi) as i32
    }

    /// Déplacement de la souris. Rend ce qu'il faut faire : repeindre, ou
    /// appliquer la couleur qu'on est en train de glisser dans le nuancier.
    pub fn mouse_move(&mut self, x: i32, y: i32, dragging: bool) -> Option<BarAction> {
        match &mut self.popup {
            Some(Popup::Fonts(list)) => {
                return list.mouse_move(x, y).then_some(BarAction::Refresh);
            }
            Some(Popup::Colors(colors)) => {
                return Some(match colors.mouse_move(x, y, dragging) {
                    Outcome::Live(rgb) => BarAction::Ink(rgb, false),
                    _ => BarAction::Refresh,
                });
            }
            None => {}
        }
        let over = self
            .hits
            .iter()
            .position(|(bx, by, bw, bh, _)| x >= *bx && x < bx + bw && y >= *by && y < by + bh);
        let changed = over != self.hover;
        self.hover = over;
        changed.then_some(BarAction::Refresh)
    }

    /// Clic dans la barre — ou n'importe où quand un sélecteur est déroulé :
    /// il prend alors tous les clics, et un clic dehors le referme.
    pub fn mouse_down(&mut self, x: i32, y: i32) -> Option<BarAction> {
        let action = match &mut self.popup {
            Some(Popup::Fonts(list)) => match list.mouse_down(x, y) {
                Outcome::Pick(choice) => BarAction::Family(choice),
                Outcome::Close => BarAction::Families,
                _ => BarAction::Refresh,
            },
            Some(Popup::Colors(colors)) => match colors.mouse_down(x, y) {
                Outcome::Pick(rgb) => BarAction::Ink(rgb, true),
                Outcome::Live(rgb) => BarAction::Ink(rgb, false),
                Outcome::Close => BarAction::Colors,
                Outcome::Stay => BarAction::Refresh,
            },
            None => {
                return self
                    .hits
                    .iter()
                    .find(|(bx, by, bw, bh, _)| x >= *bx && x < bx + bw && y >= *by && y < by + bh)
                    .map(|h| h.4);
            }
        };
        Some(action)
    }

    /// Bouton relâché : un glissement dans le nuancier s'arrête. Rend la
    /// couleur à retenir, s'il y en avait un.
    pub fn mouse_up(&mut self) {
        if let Some(Popup::Colors(colors)) = &mut self.popup {
            colors.mouse_up();
        }
    }

    /// Touche, quand un sélecteur est déroulé : il la prend.
    pub fn key(&mut self, key: Key) -> Option<BarAction> {
        Some(match self.popup.as_mut()? {
            Popup::Fonts(list) => match list.key(key) {
                Outcome::Pick(choice) => BarAction::Family(choice),
                Outcome::Close => BarAction::Families,
                _ => BarAction::Refresh,
            },
            Popup::Colors(colors) => match colors.key(key) {
                Outcome::Pick(rgb) => BarAction::Ink(rgb, true),
                Outcome::Live(rgb) => BarAction::Ink(rgb, false),
                Outcome::Close => BarAction::Colors,
                Outcome::Stay => BarAction::Refresh,
            },
        })
    }

    /// Caractère tapé, quand un sélecteur est déroulé : la recherche d'une
    /// police, ou le code d'une couleur.
    pub fn char(&mut self, c: char) -> Option<BarAction> {
        Some(match self.popup.as_mut()? {
            Popup::Fonts(list) => {
                list.char(c);
                BarAction::Refresh
            }
            Popup::Colors(colors) => match colors.char(c) {
                Outcome::Live(rgb) => BarAction::Ink(rgb, false),
                _ => BarAction::Refresh,
            },
        })
    }

    /// Molette ; vrai si un sélecteur l'a prise.
    pub fn wheel(&mut self, delta: f64) -> bool {
        match &mut self.popup {
            Some(Popup::Fonts(list)) => {
                list.wheel(delta);
                true
            }
            Some(Popup::Colors(_)) => true,
            None => false,
        }
    }

    /// Vrai si un sélecteur est déroulé : il prend alors clics et touches.
    #[must_use]
    pub fn menu_open(&self) -> bool {
        self.popup.is_some()
    }

    /// Vrai si la liste des polices a encore des noms à dessiner : la fenêtre
    /// doit se repeindre.
    #[must_use]
    pub fn pending(&self) -> bool {
        matches!(&self.popup, Some(Popup::Fonts(list)) if list.pending())
    }

    /// Déroule ou referme la liste des polices.
    pub fn toggle_menu(&mut self) {
        if matches!(self.popup, Some(Popup::Fonts(_))) {
            self.popup = None;
            return;
        }
        // La liste s'ouvre sur la police du bloc, choisie ou d'origine.
        let current = self.family.or_else(|| {
            let native = self.native.as_deref()?;
            acrux_features::sysfonts::families()
                .iter()
                .position(|f| f.name.eq_ignore_ascii_case(native))
        });
        self.popup = Some(Popup::Fonts(Box::new(FontPicker::new(current))));
    }

    /// Déroule ou referme le nuancier.
    pub fn toggle_colors(&mut self) {
        self.popup = match self.popup {
            Some(Popup::Colors(_)) => None,
            _ => Some(Popup::Colors(Box::new(ColorPicker::new(self.color)))),
        };
    }

    /// Referme le sélecteur ; vrai s'il y en avait un.
    pub fn close_menu(&mut self) -> bool {
        self.popup.take().is_some()
    }

    /// Couleur du texte ajouté.
    #[must_use]
    pub fn rgb(&self) -> [f64; 3] {
        self.color
    }

    /// Dessine la barre sur toute la largeur, à l'ordonnée `y`.
    #[allow(clippy::too_many_arguments)] // le cadre, les deux moteurs de dessin, le thème et la place
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f32,
        y: i32,
    ) {
        let h = Self::height(dpi);
        let size = theme.font_size * dpi;
        let fw = frame.width as i32;
        frame.fill_rect(0, y, fw, h, theme.bar.0, theme.bar.1, theme.bar.2);
        frame.fill_rect(
            0,
            y + h - 1,
            fw,
            1,
            theme.separator.0,
            theme.separator.1,
            theme.separator.2,
        );
        self.hits.clear();
        let s = |v: f32| (v * dpi).round() as i32;
        let pad = s(12.0);
        // Tous les contrôles ont la même hauteur, centrée dans la barre.
        let ctl = s(30.0);
        let top = y + (h - ctl) / 2;
        let radius = 7.0 * dpi;
        let baseline = (top + ctl / 2) as f32 + text.ascent(size) / 2.0;
        let close_label = "Terminer";
        let close_w = text.measure(size, close_label) as i32 + s(28.0);
        // Rien ne se dessine au-delà : le bouton « Terminer » garde sa place,
        // et sur une fenêtre étroite ce sont les derniers réglages qui
        // s'effacent, pas lui.
        let limit = fw - pad - close_w - s(10.0);
        let geometry = Geometry {
            top,
            ctl,
            radius,
            baseline,
            size,
            dpi,
            limit,
        };
        let mut x = pad;

        // Titre du mode, tant qu'un bloc n'est pas ouvert : alors le contrôle
        // allumé le dit déjà, et la place manque aux réglages.
        if !self.editing {
            let title = "Modifier le PDF";
            text.draw(frame, x as f32, baseline, size, title, theme.text_dim);
            x += text.measure(size, title) as i32 + s(14.0);
        }

        'groups: {
            // Les deux outils, en contrôle segmenté.
            let Some(w) = self.segmented(
                frame,
                text,
                theme,
                &geometry,
                x,
                &[
                    (
                        Content::Label("Modifier le texte"),
                        self.tool == EditTool::Select,
                        BarAction::Tool(EditTool::Select),
                    ),
                    (
                        Content::Label("Ajouter du texte"),
                        self.tool == EditTool::AddText,
                        BarAction::Tool(EditTool::AddText),
                    ),
                ],
            ) else {
                break 'groups;
            };
            x += w + s(12.0);

            // Corps : celui du paragraphe en cours, sinon celui du texte
            // ajouté. La valeur « 12 pt » se passe de libellé.
            let shown = self.active_size.unwrap_or(self.size);
            let Some(next) = self.stepper(
                frame,
                text,
                theme,
                &geometry,
                x,
                None,
                &format!("{shown:.0} pt"),
                BarAction::Smaller,
                BarAction::Larger,
            ) else {
                break 'groups;
            };
            x = next + s(12.0);

            // La couleur : une pastille de l'encre en cours, qui déroule le
            // nuancier.
            let cw = s(52.0);
            if x + cw > limit {
                break 'groups;
            }
            let index = self.hits.len();
            let open = matches!(self.popup, Some(Popup::Colors(_)));
            let face = if open || self.hover == Some(index) {
                theme.separator
            } else {
                theme.hover
            };
            round_rect(frame, x, top, cw, ctl, radius, face);
            self.color_anchor = (x, top, cw, ctl);
            let chip = s(18.0);
            let ink = (
                (self.color[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                (self.color[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                (self.color[2].clamp(0.0, 1.0) * 255.0).round() as u8,
            );
            // Un « A » souligné de l'encre, comme dans un traitement de
            // texte : la lettre dit « couleur du texte », la barre dit
            // laquelle. Un liseré clair la détache quand elle est sombre — une
            // encre noire sur une barre noire ne se verrait pas.
            let letter_w = text.measure(size, "A");
            text.draw(
                frame,
                (x + s(8.0)) as f32 + (chip as f32 - letter_w) / 2.0,
                baseline - 3.0 * dpi,
                size,
                "A",
                theme.text,
            );
            let bar_h = s(5.0);
            let by = top + ctl - bar_h - s(4.0);
            round_rect(
                frame,
                x + s(7.0),
                by - 1,
                chip + s(2.0),
                bar_h + 2,
                3.0 * dpi,
                theme.text_dim,
            );
            round_rect(frame, x + s(8.0), by, chip, bar_h, 2.0 * dpi, ink);
            chevron(frame, x + cw - s(14.0), top + ctl / 2, dpi, theme.text_dim);
            self.hits.push((x, top, cw, ctl, BarAction::Colors));
            x += cw + s(6.0);

            // Mise en forme du bloc ouvert : police, graisse, alignement,
            // interligne. Ce sont les réglages d'Acrobat, et ils n'ont de sens
            // que lorsqu'un bloc est ouvert.
            if self.editing {
                x += s(6.0);

                // La police : un bouton-menu qui déroule la liste des polices
                // installées. Sa largeur est fixe : sans cela, changer de
                // police déplacerait tous les boutons suivants sous le pointeur.
                // Le bouton dit la police du bloc : celle qu'on a choisie,
                // sinon celle que le document lui donne.
                let family = self
                    .family
                    .and_then(|i| acrux_features::sysfonts::families().get(i))
                    .map(|f| f.name.as_str())
                    .or(self.native.as_deref())
                    .unwrap_or("Police du texte");
                let fw_label = s(176.0);
                if x + fw_label > limit {
                    break 'groups;
                }
                let index = self.hits.len();
                let face =
                    if matches!(self.popup, Some(Popup::Fonts(_))) || self.hover == Some(index) {
                        theme.separator
                    } else {
                        theme.hover
                    };
                round_rect(frame, x, top, fw_label, ctl, radius, face);
                self.font_anchor = (x, top, fw_label, ctl);
                text.draw_clipped(
                    frame,
                    (x + s(10.0)) as f32,
                    baseline,
                    size,
                    family,
                    theme.text,
                    (fw_label - s(34.0)) as f32,
                );
                chevron(
                    frame,
                    x + fw_label - s(16.0),
                    top + ctl / 2,
                    dpi,
                    theme.text_dim,
                );
                self.hits.push((x, top, fw_label, ctl, BarAction::Families));
                x += fw_label + s(10.0);

                // Gras et italique.
                let Some(w) = self.segmented(
                    frame,
                    text,
                    theme,
                    &geometry,
                    x,
                    &[
                        (Content::Label("G"), self.bold, BarAction::Bold),
                        (Content::Label("I"), self.italic, BarAction::Italic),
                    ],
                ) else {
                    break 'groups;
                };
                x += w + s(8.0);

                // Alignement : quatre segments dessinés en barres, celui du bloc
                // allumé. Les glyphes d'alignement d'Unicode manquent à trop de
                // polices pour qu'on s'y fie.
                let aligns: Vec<(Content<'_>, bool, BarAction)> = (0..ALIGNMENTS.len())
                    .map(|i| (Content::Align(i), self.align == i, BarAction::Align(i)))
                    .collect();
                let Some(w) = self.segmented(frame, text, theme, &geometry, x, &aligns) else {
                    break 'groups;
                };
                x += w + s(12.0);

                // Interligne : une icône plutôt qu'un mot, la place est comptée.
                let icon = s(18.0);
                if x + icon + s(6.0) < limit {
                    leading_icon(frame, x, top + (ctl - icon) / 2, icon, dpi, theme.text_dim);
                    x += icon + s(6.0);
                }
                if let Some(next) = self.stepper(
                    frame,
                    text,
                    theme,
                    &geometry,
                    x,
                    None,
                    &format!("{:.2}", self.leading),
                    BarAction::Tighter,
                    BarAction::Looser,
                ) {
                    x = next;
                }
            }
        }

        // Terminer, à droite : le bouton principal, le même que partout.
        let (label, w) = (close_label, close_w);
        let bx = fw - pad - w;
        if bx > x {
            let index = self.hits.len();
            let ink = crate::ui::paint::button(
                frame,
                bx,
                top,
                w,
                ctl,
                dpi,
                theme,
                crate::ui::paint::ButtonLook {
                    primary: true,
                    hovered: self.hover == Some(index),
                    focused: false,
                    ..crate::ui::paint::ButtonLook::default()
                },
            );
            let lw = text.measure(size, label);
            text.draw(
                frame,
                bx as f32 + (w as f32 - lw) / 2.0,
                baseline,
                size,
                label,
                ink,
            );
            self.hits.push((bx, top, w, ctl, BarAction::Close));
        }

        // Le sélecteur déroulé, par-dessus la page. Il n'a de sens que pour
        // le bouton qui l'ouvre : la liste des polices se referme avec le bloc.
        if !self.editing && matches!(self.popup, Some(Popup::Fonts(_))) {
            self.popup = None;
        }
        match &mut self.popup {
            Some(Popup::Fonts(list)) => {
                list.paint(frame, text, raster, theme, dpi, self.font_anchor);
            }
            Some(Popup::Colors(colors)) => {
                colors.paint(frame, text, theme, dpi, self.color_anchor, self.color);
            }
            None => {}
        }
    }

    /// Un contrôle segmenté (voir [`controls::segmented`]), dont chaque case
    /// devient une zone cliquable. Rend la largeur occupée, ou rien si la
    /// place manque.
    #[allow(clippy::too_many_arguments)] // le cadre, la police, le thème, la géométrie et les cases
    fn segmented(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        g: &Geometry,
        x: i32,
        items: &[(Content<'_>, bool, BarAction)],
    ) -> Option<i32> {
        // Les dessins d'alignement doivent vivre le temps de l'appel.
        let draws: Vec<controls::BoxedDraw> = items
            .iter()
            .map(|(content, _, _)| {
                let kind = match content {
                    Content::Align(kind) => *kind,
                    Content::Label(_) => 0,
                };
                let dpi = g.dpi;
                Box::new(
                    move |frame: &mut Frame<'_>,
                          (x, y, w, h): (i32, i32, i32, i32),
                          ink: (u8, u8, u8)| {
                        align_icon(frame, x, y, w, h, dpi, kind, ink);
                    },
                ) as controls::BoxedDraw
            })
            .collect();
        let first = self.hits.len();
        let segments: Vec<SegmentItem<'_>> = items
            .iter()
            .enumerate()
            .map(|(i, (content, on, _))| SegmentItem {
                content: match content {
                    Content::Label(label) => Segment::Label(label),
                    Content::Align(_) => Segment::Custom {
                        width: g.ctl,
                        draw: &*draws[i],
                    },
                },
                on: *on,
                hovered: self.hover == Some(first + i),
            })
            .collect();
        if x + controls::segmented_width(text, theme, g.dpi, g.ctl, &segments) > g.limit {
            return None;
        }
        let (total, rects) =
            controls::segmented(frame, text, theme, g.dpi, x, g.top, g.ctl, &segments);
        for ((_, _, action), (rx, ry, rw, rh)) in items.iter().zip(rects) {
            self.hits.push((rx, ry, rw, rh, *action));
        }
        Some(total)
    }

    /// Un pas-à-pas : un libellé s'il y en a un, puis « − valeur + » dans un
    /// même creux. Rend l'abscisse atteinte, ou rien si la place manque.
    #[allow(clippy::too_many_arguments)] // un libellé, une valeur, deux actions
    fn stepper(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        g: &Geometry,
        x: i32,
        label: Option<&str>,
        value: &str,
        minus: BarAction,
        plus: BarAction,
    ) -> Option<i32> {
        let s = |v: f32| (v * g.dpi).round() as i32;
        let label_w = label.map_or(0, |l| text.measure(g.size, l) as i32 + s(10.0));
        let value_w = text.measure(g.size, value) as i32 + s(12.0);
        let total = g.ctl * 2 + value_w;
        if x + label_w + total > g.limit {
            return None;
        }
        if let Some(label) = label {
            text.draw(frame, x as f32, g.baseline, g.size, label, theme.text_dim);
        }
        let x = x + label_w;
        controls::well(frame, x, g.top, total, g.ctl, g.radius, theme);
        let inset = s(2.0);
        for (i, (glyph, action)) in [("\u{2212}", minus), ("+", plus)].into_iter().enumerate() {
            let bx = if i == 0 { x } else { x + g.ctl + value_w };
            let index = self.hits.len();
            if self.hover == Some(index) {
                round_rect(
                    frame,
                    bx + inset,
                    g.top + inset,
                    g.ctl - 2 * inset,
                    g.ctl - 2 * inset,
                    g.radius - g.dpi,
                    theme.separator,
                );
            }
            let lw = text.measure(g.size, glyph);
            text.draw(
                frame,
                bx as f32 + (g.ctl as f32 - lw) / 2.0,
                g.baseline,
                g.size,
                glyph,
                theme.text,
            );
            self.hits.push((bx, g.top, g.ctl, g.ctl, action));
        }
        let vw = text.measure(g.size, value);
        text.draw(
            frame,
            (x + g.ctl) as f32 + (value_w as f32 - vw) / 2.0,
            g.baseline,
            g.size,
            value,
            theme.text,
        );
        Some(x + total)
    }
}

/// Ce que tous les contrôles de la barre partagent : leur ligne, leur
/// hauteur, leur rayon, leur police.
struct Geometry {
    top: i32,
    ctl: i32,
    radius: f32,
    baseline: f32,
    size: f32,
    dpi: f32,
    /// Abscisse à ne pas dépasser : la place du bouton « Terminer ».
    limit: i32,
}

/// Ce qu'un segment affiche.
enum Content<'a> {
    /// Un libellé.
    Label(&'a str),
    /// L'icône d'un alignement (rang dans [`ALIGNMENTS`]).
    Align(usize),
}

/// L'icône de l'interligne : trois lignes de texte et une flèche double qui
/// dit qu'on les écarte ou les resserre.
fn leading_icon(frame: &mut Frame<'_>, x: i32, y: i32, size: i32, dpi: f32, ink: (u8, u8, u8)) {
    let thick = (1.5 * dpi).max(1.0) as i32;
    let lines_x = x + size * 2 / 5;
    let lines_w = size - size * 2 / 5;
    for i in 0..3 {
        let ly = y + i * (size - thick) / 2;
        frame.fill_rect(lines_x, ly, lines_w, thick, ink.0, ink.1, ink.2);
    }
    // La flèche : un trait vertical et deux pointes.
    let ax = x + size / 6;
    frame.fill_rect(ax, y, thick, size, ink.0, ink.1, ink.2);
    let head = (size / 5).max(2);
    for d in 0..head {
        frame.fill_rect(ax - d, y + d, 2 * d + thick, 1, ink.0, ink.1, ink.2);
        frame.fill_rect(
            ax - d,
            y + size - 1 - d,
            2 * d + thick,
            1,
            ink.0,
            ink.1,
            ink.2,
        );
    }
}

/// Un chevron vers le bas, centré sur `(cx, cy)` : le bouton est un menu.
fn chevron(frame: &mut Frame<'_>, cx: i32, cy: i32, dpi: f32, ink: (u8, u8, u8)) {
    let step = (1.0 * dpi).max(1.0) as i32;
    let half = (4.0 * dpi) as i32;
    let mut y = cy - half / 2;
    let mut reach = half;
    while reach >= 0 {
        frame.fill_rect(cx - reach, y, 2 * reach + 1, step, ink.0, ink.1, ink.2);
        y += step;
        reach -= step;
    }
}

/// Quatre barres qui disent l'alignement : à gauche, centrées, à droite, ou
/// toutes pleines pour le justifié.
// Le cadre, sa boîte, l'échelle, la sorte et l'encre : huit valeurs, toutes
// nécessaires au dessin.
#[allow(clippy::too_many_arguments)]
fn align_icon(
    frame: &mut Frame<'_>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    dpi: f32,
    kind: usize,
    ink: (u8, u8, u8),
) {
    let thick = (1.5 * dpi).max(1.0) as i32;
    let gap = (3.0 * dpi).max(2.0) as i32;
    let full = (w as f32 * 0.56) as i32;
    let short = (full as f32 * 0.62) as i32;
    let total = thick * 4 + gap * 3;
    let mut ly = y + (h - total) / 2;
    for row in 0..4 {
        // Une ligne sur deux est courte, comme un vrai paragraphe ; un texte
        // justifié n'a que sa dernière ligne courte.
        let len = if kind == 3 {
            if row < 3 {
                full
            } else {
                short
            }
        } else if row % 2 == 0 {
            full
        } else {
            short
        };
        let lx = match kind {
            1 => x + (w - len) / 2,
            2 => x + (w + full) / 2 - len,
            _ => x + (w - full) / 2,
        };
        frame.fill_rect(lx, ly, len, thick, ink.0, ink.1, ink.2);
        ly += thick + gap;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un texte et son curseur, pour éprouver le découpage en étapes.
    fn buffer(text: &str, caret: usize) -> Buffer {
        let mut b = Buffer::new(text);
        b.caret = caret;
        b.anchor = caret;
        b
    }

    /// Des lettres qui se suivent n'ouvrent qu'une étape : Ctrl+Z défait le
    /// mot, pas la lettre.
    #[test]
    fn les_lettres_qui_se_suivent_font_une_etape() {
        let before = buffer("bonjou", 6);
        let after = buffer("bonjour", 7);
        let (kind, opens) = step_of(&before, &after, StepKind::Insert, 6);
        assert_eq!(kind, StepKind::Insert);
        assert!(!opens, "une lettre de plus prolonge l'étape");
    }

    /// Un blanc ferme le mot : l'étape suivante recommence là.
    #[test]
    fn un_blanc_ouvre_une_etape() {
        let before = buffer("bonjour", 7);
        let after = buffer("bonjour ", 8);
        let (_, opens) = step_of(&before, &after, StepKind::Insert, 7);
        assert!(opens);
    }

    /// Effacer après avoir tapé ouvre une étape : on ne défait pas les deux
    /// d'un coup.
    #[test]
    fn effacer_apres_avoir_tape_ouvre_une_etape() {
        let before = buffer("bonjour", 7);
        let after = buffer("bonjou", 6);
        let (kind, opens) = step_of(&before, &after, StepKind::Insert, 7);
        assert_eq!(kind, StepKind::Delete);
        assert!(opens);
    }

    /// Déplacer le curseur ailleurs ouvre une étape.
    #[test]
    fn deplacer_le_curseur_ouvre_une_etape() {
        let before = buffer("bonjour", 2);
        let after = buffer("bXonjour", 3);
        let (_, opens) = step_of(&before, &after, StepKind::Insert, 7);
        assert!(opens, "la frappe reprend ailleurs : nouvelle étape");
    }

    /// Effacer lettre après lettre ne fait qu'une étape.
    #[test]
    fn les_effacements_qui_se_suivent_font_une_etape() {
        let before = buffer("bonjou", 6);
        let after = buffer("bonjo", 5);
        let (_, opens) = step_of(&before, &after, StepKind::Delete, 6);
        assert!(!opens);
    }

    #[test]
    fn la_frappe_remplace_la_selection() {
        let mut b = Buffer::new("Bonjour monde");
        b.anchor = 8;
        b.caret = 13;
        b.insert("Acrux");
        assert_eq!(b.text, "Bonjour Acrux");
        assert_eq!((b.caret, b.anchor), (13, 13));
    }

    #[test]
    fn les_accents_comptent_pour_un_caractere() {
        let mut b = Buffer::new("été");
        b.backspace();
        assert_eq!(b.text, "ét");
        b.left(false);
        b.insert("c");
        assert_eq!(b.text, "éct");
        assert_eq!(b.caret, 2);
    }

    #[test]
    fn retour_arriere_et_suppr() {
        let mut b = Buffer::new("abc");
        b.move_to(1, false);
        b.delete();
        assert_eq!(b.text, "ac");
        b.backspace();
        assert_eq!(b.text, "c");
        assert_eq!(b.caret, 0);
        // Aux bords, rien ne se passe.
        b.backspace();
        assert_eq!(b.text, "c");
        b.move_to(1, false);
        b.delete();
        assert_eq!(b.text, "c");
    }

    #[test]
    fn les_fleches_referment_la_selection_sur_son_bord() {
        let mut b = Buffer::new("abcdef");
        b.anchor = 1;
        b.caret = 4;
        b.left(false);
        assert_eq!((b.caret, b.anchor), (1, 1));
        b.anchor = 1;
        b.caret = 4;
        b.right(false);
        assert_eq!((b.caret, b.anchor), (4, 4));
        // Avec Maj, la sélection s'étend.
        b.right(true);
        assert_eq!(b.range(), (4, 5));
    }

    #[test]
    fn mots_et_double_clic() {
        let mut b = Buffer::new("le petit chat");
        b.move_to(0, false);
        b.word_right(false);
        assert_eq!(b.caret, 3);
        b.word_right(false);
        assert_eq!(b.caret, 9);
        b.word_left(false);
        assert_eq!(b.caret, 3);
        b.select_word(5);
        assert_eq!(b.selected(), "petit");
        b.select_all();
        assert_eq!(b.selected(), "le petit chat");
    }

    #[test]
    fn les_caracteres_de_controle_sont_filtres() {
        let mut b = Buffer::new("");
        b.insert("a\tb\rc\u{7}d\ne");
        assert_eq!(b.text, "a bcd\ne");
    }

    #[test]
    fn la_gamme_des_corps() {
        assert!((step_size(12.0, true) - 14.0).abs() < 1e-9);
        assert!((step_size(12.0, false) - 11.0).abs() < 1e-9);
        assert!((step_size(36.0, true) - 45.0).abs() < 1e-9);
        assert!((step_size(6.0, false) - 4.8).abs() < 1e-9);
        assert!(step_size(4.0, false) >= 4.0);
        // Un corps hors gamme retombe sur la marche voisine.
        assert!((step_size(12.5, true) - 14.0).abs() < 1e-9);
    }
}
