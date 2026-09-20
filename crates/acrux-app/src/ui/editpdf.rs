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

use crate::platform::Frame;
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
    /// Choisir une couleur de la palette.
    Color(usize),
    /// Police suivante de la liste.
    NextFamily,
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

/// Familles proposées : celles qu'un document emploie neuf fois sur dix, et
/// que toute machine sait dessiner.
pub const FAMILIES: [&str; 4] = ["Helvetica", "Times New Roman", "Courier New", "Verdana"];

/// Alignements, dans l'ordre des boutons.
pub const ALIGNMENTS: [&str; 4] = ["gauche", "centré", "droite", "justifié"];

/// Couleurs proposées pour le texte ajouté : noir, gris, bleu, rouge, vert.
///
/// Peu de choix, parce qu'un document se remplit de noir et que le reste
/// sert à corriger ou à signaler.
pub const COLORS: [[f64; 3]; 5] = [
    [0.0, 0.0, 0.0],
    [0.35, 0.35, 0.38],
    [0.08, 0.28, 0.72],
    [0.78, 0.1, 0.12],
    [0.1, 0.5, 0.2],
];

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

/// Indice de la couleur la plus proche dans [`COLORS`], pour reprendre une
/// encre choisie ailleurs.
#[must_use]
pub fn color_index(color: [f64; 3]) -> usize {
    COLORS
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let d = |c: &[f64; 3]| {
                (c[0] - color[0]).powi(2) + (c[1] - color[1]).powi(2) + (c[2] - color[2]).powi(2)
            };
            d(a).total_cmp(&d(b))
        })
        .map_or(0, |(i, _)| i)
}

/// Barre du mode, sous la barre d'outils.
#[derive(Debug)]
// Gras, italique, corps imposé, couleur imposée, bloc ouvert : cinq
// interrupteurs indépendants, qu'un regroupement artificiel n'éclaircirait
// pas.
#[allow(clippy::struct_excessive_bools)]
pub struct EditBar {
    /// Outil actif.
    pub tool: EditTool,
    /// Corps du texte ajouté, en points.
    pub size: f64,
    /// Couleur du texte ajouté (indice dans [`COLORS`]).
    pub color: usize,
    /// Corps du paragraphe en cours, à afficher à la place du défaut.
    pub active_size: Option<f64>,
    /// Police du bloc en cours : rang dans [`FAMILIES`], ou rien quand le
    /// bloc garde la sienne.
    pub family: Option<usize>,
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
}

impl Default for EditBar {
    fn default() -> Self {
        Self {
            tool: EditTool::Select,
            size: 12.0,
            color: 0,
            active_size: None,
            family: None,
            bold: false,
            italic: false,
            align: 0,
            leading: 1.2,
            editing: false,
            size_set: false,
            color_set: false,
            hits: Vec::new(),
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
        (38.0 * dpi) as i32
    }

    /// Clic dans la barre.
    #[must_use]
    pub fn mouse_down(&self, x: i32, y: i32) -> Option<BarAction> {
        self.hits
            .iter()
            .find(|(bx, by, bw, bh, _)| x >= *bx && x < bx + bw && y >= *by && y < by + bh)
            .map(|h| h.4)
    }

    /// Couleur du texte ajouté.
    #[must_use]
    pub fn rgb(&self) -> [f64; 3] {
        COLORS[self.color.min(COLORS.len() - 1)]
    }

    /// Dessine la barre sur toute la largeur, à l'ordonnée `y`.
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
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
        let pad = (10.0 * dpi) as i32;
        let inner = h - (8.0 * dpi) as i32;
        let top = y + (4.0 * dpi) as i32;
        let baseline = |text: &mut TextRenderer| (y + h / 2) as f32 + text.ascent(size) / 2.0;
        let mut x = pad;

        // Titre du mode : on sait toujours où l'on est.
        let title = "Modifier le PDF";
        let tb = baseline(text);
        text.draw(frame, x as f32, tb, size, title, theme.text_dim);
        x += text.measure(size, title) as i32 + (18.0 * dpi) as i32;

        // Les deux outils.
        for (label, tool) in [
            ("Modifier le texte", EditTool::Select),
            ("Ajouter du texte", EditTool::AddText),
        ] {
            let w = text.measure(size, label) as i32 + (22.0 * dpi) as i32;
            let on = self.tool == tool;
            if on {
                frame.fill_rect(
                    x,
                    top,
                    w,
                    inner,
                    theme.accent.0,
                    theme.accent.1,
                    theme.accent.2,
                );
            }
            let color = if on { (255, 255, 255) } else { theme.text };
            let b = baseline(text);
            text.draw(
                frame,
                (x + (11.0 * dpi) as i32) as f32,
                b,
                size,
                label,
                color,
            );
            self.hits.push((x, top, w, inner, BarAction::Tool(tool)));
            x += w + (4.0 * dpi) as i32;
        }
        x += (10.0 * dpi) as i32;
        separator(frame, theme, x, top, inner);
        x += (14.0 * dpi) as i32;

        // Corps : celui du paragraphe en cours, sinon celui du texte ajouté.
        let shown = self.active_size.unwrap_or(self.size);
        let b = baseline(text);
        text.draw(frame, x as f32, b, size, "Taille", theme.text_dim);
        x += text.measure(size, "Taille") as i32 + (8.0 * dpi) as i32;
        let button = inner;
        for (label, action) in [("−", BarAction::Smaller), ("+", BarAction::Larger)] {
            if action == BarAction::Larger {
                let value = format!("{shown:.0} pt");
                let vw = text.measure(size, &value) as i32;
                text.draw(
                    frame,
                    (x + (4.0 * dpi) as i32) as f32,
                    b,
                    size,
                    &value,
                    theme.text,
                );
                x += vw + (8.0 * dpi) as i32;
            }
            frame.fill_rect(
                x,
                top,
                button,
                inner,
                theme.hover.0,
                theme.hover.1,
                theme.hover.2,
            );
            let lw = text.measure(size, label);
            text.draw(
                frame,
                x as f32 + (button as f32 - lw) / 2.0,
                b,
                size,
                label,
                theme.text,
            );
            self.hits.push((x, top, button, inner, action));
            x += button + (4.0 * dpi) as i32;
        }
        x += (10.0 * dpi) as i32;
        separator(frame, theme, x, top, inner);
        x += (14.0 * dpi) as i32;

        // Couleurs du texte ajouté.
        text.draw(frame, x as f32, b, size, "Couleur", theme.text_dim);
        x += text.measure(size, "Couleur") as i32 + (8.0 * dpi) as i32;
        let swatch = (inner as f32 * 0.62) as i32;
        let sy = top + (inner - swatch) / 2;
        for (i, c) in COLORS.iter().enumerate() {
            let rgb = (
                (c[0] * 255.0) as u8,
                (c[1] * 255.0) as u8,
                (c[2] * 255.0) as u8,
            );
            if i == self.color {
                let ring = (2.0 * dpi) as i32;
                frame.fill_rect(
                    x - ring,
                    sy - ring,
                    swatch + ring * 2,
                    swatch + ring * 2,
                    theme.accent.0,
                    theme.accent.1,
                    theme.accent.2,
                );
            }
            frame.fill_rect(x, sy, swatch, swatch, rgb.0, rgb.1, rgb.2);
            self.hits.push((x, top, swatch, inner, BarAction::Color(i)));
            x += swatch + (8.0 * dpi) as i32;
        }

        // Mise en forme du bloc ouvert : police, graisse, alignement,
        // interligne. Ce sont les réglages d'Acrobat, et ils n'ont de sens
        // que lorsqu'un bloc est ouvert.
        if self.editing {
            x += (10.0 * dpi) as i32;
            separator(frame, theme, x, top, inner);
            x += (14.0 * dpi) as i32;

            // La police : un bouton qui fait défiler les familles.
            let family = self
                .family
                .and_then(|i| FAMILIES.get(i))
                .copied()
                .unwrap_or("Police du texte");
            // Largeur figée sur le plus long des noms : sans cela, changer de
            // police déplacerait tous les boutons suivants sous le pointeur.
            let widest = FAMILIES
                .iter()
                .chain(std::iter::once(&"Police du texte"))
                .map(|f| text.measure(size, f) as i32)
                .max()
                .unwrap_or(0);
            let fw_label = widest + (22.0 * dpi) as i32;
            frame.fill_rect(
                x,
                top,
                fw_label,
                inner,
                theme.hover.0,
                theme.hover.1,
                theme.hover.2,
            );
            text.draw(
                frame,
                (x + (11.0 * dpi) as i32) as f32,
                b,
                size,
                family,
                theme.text,
            );
            self.hits
                .push((x, top, fw_label, inner, BarAction::NextFamily));
            x += fw_label + (8.0 * dpi) as i32;

            // Gras et italique.
            for (label, on, action) in [
                ("G", self.bold, BarAction::Bold),
                ("I", self.italic, BarAction::Italic),
            ] {
                let bw = button;
                let (fill, ink) = if on {
                    (theme.accent, (255, 255, 255))
                } else {
                    (theme.hover, theme.text)
                };
                frame.fill_rect(x, top, bw, inner, fill.0, fill.1, fill.2);
                let lw = text.measure(size, label);
                text.draw(
                    frame,
                    x as f32 + (bw as f32 - lw) / 2.0,
                    b,
                    size,
                    label,
                    ink,
                );
                self.hits.push((x, top, bw, inner, action));
                x += bw + (4.0 * dpi) as i32;
            }
            x += (6.0 * dpi) as i32;

            // Alignement : quatre boutons dessinés en barres, celui du bloc
            // allumé. Les glyphes d'alignement d'Unicode manquent à trop de
            // polices pour qu'on s'y fie.
            for i in 0..ALIGNMENTS.len() {
                let bw = button;
                let on = self.align == i;
                let (fill, ink) = if on {
                    (theme.accent, (255, 255, 255))
                } else {
                    (theme.hover, theme.text)
                };
                frame.fill_rect(x, top, bw, inner, fill.0, fill.1, fill.2);
                align_icon(frame, x, top, bw, inner, dpi, i, ink);
                self.hits.push((x, top, bw, inner, BarAction::Align(i)));
                x += bw + (4.0 * dpi) as i32;
            }
            x += (6.0 * dpi) as i32;

            // Interligne.
            text.draw(frame, x as f32, b, size, "Interligne", theme.text_dim);
            x += text.measure(size, "Interligne") as i32 + (8.0 * dpi) as i32;
            for (label, action) in [("−", BarAction::Tighter), ("+", BarAction::Looser)] {
                if action == BarAction::Looser {
                    let value = format!("{:.2}", self.leading);
                    let vw = text.measure(size, &value) as i32;
                    text.draw(
                        frame,
                        (x + (4.0 * dpi) as i32) as f32,
                        b,
                        size,
                        &value,
                        theme.text,
                    );
                    x += vw + (8.0 * dpi) as i32;
                }
                frame.fill_rect(
                    x,
                    top,
                    button,
                    inner,
                    theme.hover.0,
                    theme.hover.1,
                    theme.hover.2,
                );
                let lw = text.measure(size, label);
                text.draw(
                    frame,
                    x as f32 + (button as f32 - lw) / 2.0,
                    b,
                    size,
                    label,
                    theme.text,
                );
                self.hits.push((x, top, button, inner, action));
                x += button + (4.0 * dpi) as i32;
            }
        }

        // Terminer, à droite.
        let label = "Terminer";
        let w = text.measure(size, label) as i32 + (26.0 * dpi) as i32;
        let bx = fw - pad - w;
        if bx > x {
            frame.fill_rect(
                bx,
                top,
                w,
                inner,
                theme.accent.0,
                theme.accent.1,
                theme.accent.2,
            );
            text.draw(
                frame,
                (bx + (13.0 * dpi) as i32) as f32,
                b,
                size,
                label,
                (255, 255, 255),
            );
            self.hits.push((bx, top, w, inner, BarAction::Close));
        }
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

/// Filet vertical entre deux groupes de la barre.
fn separator(frame: &mut Frame<'_>, theme: &Theme, x: i32, top: i32, h: i32) {
    frame.fill_rect(
        x,
        top + h / 6,
        1,
        h * 2 / 3,
        theme.separator.0,
        theme.separator.1,
        theme.separator.2,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

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
