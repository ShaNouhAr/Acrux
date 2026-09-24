//! Barre fine d'un outil en cours : son nom, ce qu'il attend, et le moyen
//! d'en sortir.
//!
//! Un outil qui change ce que fait un clic sur la page doit le dire. Sans
//! cette barre, rien ne distingue « je sélectionne du texte » de « je
//! surligne » avant qu'il soit trop tard — et l'on cherche comment sortir.
//! Acrobat affiche pour cela une barre contextuelle ; celle-ci en est la
//! forme la plus simple.
//!
//! # La barre des commentaires
//!
//! Les outils de commentaire, eux, se tiennent ensemble, comme la barre
//! « Commenter » d'Acrobat : surligner, souligner, barrer, insérer,
//! remplacer, poser une note… Relire un document, c'est passer de l'un à
//! l'autre sans cesse ; les chercher un à un dans la colonne de droite
//! serait un aller-retour à chaque remarque. [`ModeBar::paint_tools`]
//! dessine donc une rangée de boutons d'icône, l'outil en cours en accent,
//! et la consigne de celui-ci.
//!
//! La barre ne connaît pas les outils : elle reçoit leurs icônes et rend le
//! rang de celui qu'on clique. C'est le visualiseur qui sait ce qu'ils font
//! — le même principe que la colonne d'outils, qui se teste seule.
//!
//! # Les réglages de l'outil
//!
//! Un outil de dessin a ses réglages, à droite de ses boutons : couleur du
//! trait, remplissage, épaisseur, opacité ; pour une zone de texte, couleur
//! du texte, corps, police, cadre et fond. La barre les dessine d'après une
//! description ([`Setting`]) — une pastille de couleur ou une valeur à
//! dérouler — et rend le rang de celui qu'on clique ; le visualiseur ouvre
//! le nuancier ou la liste sous lui. On règle donc l'outil là où on le
//! choisit, sans panneau à ouvrir.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss
)]

use acrux_graphics::Rasterizer;

use crate::platform::Frame;
use crate::ui::icons::{self, Icon};
use crate::ui::lang::tr;
use crate::ui::paint::{button, round_rect, round_rect_outline, ButtonLook};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Rectangle `(x, y, largeur, hauteur)`.
type Rect = (i32, i32, i32, i32);

/// Allure d'une pastille de couleur : ce que la couleur colore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Swatch {
    /// Le trait : un anneau.
    Stroke,
    /// Le fond : un carré plein.
    Fill,
    /// Le texte : un « A » souligné de la couleur.
    Text,
}

/// Un réglage de l'outil en cours, tel que la barre le montre.
#[derive(Debug, Clone, PartialEq)]
pub enum Setting {
    /// Une couleur ; `None` pour « aucune » (pas de fond, pas de cadre).
    Color {
        /// La couleur en cours.
        color: Option<(u8, u8, u8)>,
        /// Ce qu'elle colore.
        swatch: Swatch,
    },
    /// Une valeur à choisir dans une liste (« 2 pt », « Helvetica »).
    Choice(String),
}

/// Ce que le pointeur survole dans la barre des commentaires, pour
/// l'info-bulle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hovered {
    /// Le bouton d'un outil, par son rang.
    Tool(usize),
    /// Un réglage, par son rang.
    Setting(usize),
}

fn inside(r: Rect, x: i32, y: i32) -> bool {
    x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
}

/// Barre d'un outil.
#[derive(Debug, Default)]
pub struct ModeBar {
    /// Zone du bouton « Terminer », remplie au dessin.
    close: Option<Rect>,
    /// Le pointeur est sur « Terminer ».
    hover: bool,
    /// Zones des boutons d'outil de la barre des commentaires, dans l'ordre
    /// reçu ; vide pour la barre d'un seul outil.
    tools: Vec<Rect>,
    /// Bouton d'outil sous le pointeur.
    hover_tool: Option<usize>,
    /// Zones des réglages de l'outil en cours, dans l'ordre reçu ; seuls
    /// ceux qui tiennent dans la barre y sont.
    settings: Vec<Rect>,
    /// Réglage sous le pointeur.
    hover_setting: Option<usize>,
}

impl ModeBar {
    /// Hauteur de la barre en pixels : la même que la barre « Modifier le
    /// PDF », pour que les deux se ressemblent.
    #[must_use]
    pub fn height(dpi: f32) -> i32 {
        (44.0 * dpi) as i32
    }

    /// Vrai si le clic tombe sur « Terminer ».
    #[must_use]
    pub fn closes(&self, x: i32, y: i32) -> bool {
        self.close.is_some_and(|r| inside(r, x, y))
    }

    /// Rang du bouton d'outil sous un point de la fenêtre.
    #[must_use]
    pub fn tool_at(&self, x: i32, y: i32) -> Option<usize> {
        self.tools.iter().position(|r| inside(*r, x, y))
    }

    /// Rang du réglage sous un point de la fenêtre.
    #[must_use]
    pub fn setting_at(&self, x: i32, y: i32) -> Option<usize> {
        self.settings.iter().position(|r| inside(*r, x, y))
    }

    /// Zone d'un réglage, sous laquelle s'ouvre son nuancier ou sa liste.
    #[must_use]
    pub fn setting_rect(&self, index: usize) -> Option<Rect> {
        self.settings.get(index).copied()
    }

    /// Déplacement de la souris ; vrai si l'aspect a changé.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let over = self.closes(x, y);
        let tool = self.tool_at(x, y);
        let setting = self.setting_at(x, y);
        let changed =
            over != self.hover || tool != self.hover_tool || setting != self.hover_setting;
        self.hover = over;
        self.hover_tool = tool;
        self.hover_setting = setting;
        changed
    }

    /// Bouton d'outil survolé et sa zone, pour l'info-bulle : son nom, que
    /// l'icône seule ne dit pas.
    #[must_use]
    pub fn hover_tip(&self) -> Option<(usize, Rect)> {
        let index = self.hover_tool?;
        Some((index, *self.tools.get(index)?))
    }

    /// Ce que le pointeur survole — un outil ou un réglage — et sa zone.
    #[must_use]
    pub fn hovered(&self) -> Option<(Hovered, Rect)> {
        if let Some((i, r)) = self.hover_tip() {
            return Some((Hovered::Tool(i), r));
        }
        let index = self.hover_setting?;
        Some((Hovered::Setting(index), *self.settings.get(index)?))
    }

    /// Fond et filet du bas, communs aux deux formes de la barre.
    fn background(frame: &mut Frame<'_>, theme: &Theme, y: i32, h: i32) {
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
    }

    /// « Terminer », calé à droite ; rend son abscisse.
    #[allow(clippy::too_many_arguments)] // tout le contexte de dessin
    fn paint_done(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
        top: i32,
        ctl: i32,
        baseline: f32,
    ) -> i32 {
        let size = theme.font_size * dpi;
        let s = |v: f32| (v * dpi).round() as i32;
        let label = tr("Terminer");
        let w = text.measure(size, label) as i32 + s(28.0);
        let bx = frame.width as i32 - s(12.0) - w;
        // Le bouton principal, le même que partout.
        let ink = button(
            frame,
            bx,
            top,
            w,
            ctl,
            dpi,
            theme,
            ButtonLook {
                primary: true,
                hovered: self.hover,
                focused: false,
                ..ButtonLook::default()
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
        self.close = Some((bx, top, w, ctl));
        bx
    }

    /// Dessine la barre sur toute la largeur, à l'ordonnée `y` : l'icône et
    /// le nom de l'outil sur une pastille d'accent, sa consigne, et
    /// « Terminer ».
    #[allow(clippy::too_many_arguments)] // tout le contexte de dessin, rien de gardé
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f32,
        y: i32,
        icon: Icon,
        title: &'static str,
        hint: &'static str,
    ) {
        let h = Self::height(dpi);
        let size = theme.font_size * dpi;
        Self::background(frame, theme, y, h);
        self.tools.clear();
        self.hover_tool = None;
        self.settings.clear();
        self.hover_setting = None;
        let s = |v: f32| (v * dpi).round() as i32;
        let pad = s(12.0);
        let ctl = s(30.0);
        let top = y + (h - ctl) / 2;
        let baseline = (top + ctl / 2) as f32 + text.ascent(size) / 2.0;
        // La pastille : l'icône de l'outil et son nom, en accent. C'est ce
        // qu'on doit voir d'abord.
        let icon_side = s(18.0);
        let title = tr(title);
        let tw = icon_side + text.measure(size, title) as i32 + s(30.0);
        round_rect(frame, pad, top, tw, ctl, 7.0 * dpi, theme.accent);
        icons::draw(
            frame,
            raster,
            icon,
            pad + s(10.0),
            top + (ctl - icon_side) / 2,
            icon_side as f32,
            (255, 255, 255),
        );
        text.draw(
            frame,
            (pad + s(10.0) + icon_side + s(8.0)) as f32,
            baseline,
            size,
            title,
            (255, 255, 255),
        );
        let bx = self.paint_done(frame, text, theme, dpi, top, ctl, baseline);
        // La consigne, sur ce qui reste entre la pastille et « Terminer ».
        text.draw_clipped(
            frame,
            (pad + tw + s(14.0)) as f32,
            baseline,
            size,
            tr(hint),
            theme.text_dim,
            (bx - pad - tw - s(28.0)).max(0) as f32,
        );
    }

    /// Dessine la barre des commentaires à l'ordonnée `y` : son titre, un
    /// bouton par outil (`active` en accent), les réglages de l'outil en
    /// cours, sa consigne — ou, sans outil, comment s'en servir — et
    /// « Terminer ».
    ///
    /// `groups` dit où commence chaque groupe d'outils après le premier
    /// (rangs dans `tools`) : un filet les sépare, le balisage du texte d'un
    /// côté, le dessin de l'autre. `open` est le réglage dont le nuancier ou
    /// la liste est déroulé : il reste enfoncé.
    ///
    /// Quand la fenêtre est étroite, la consigne se rogne d'abord, puis le
    /// titre s'efface, puis les réglages qui ne tiennent plus : les boutons
    /// d'outil, eux, restent tous atteignables.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)] // tout le contexte de dessin, lu de gauche à droite
    pub fn paint_tools(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f32,
        y: i32,
        tools: &[Icon],
        groups: &[usize],
        active: Option<usize>,
        settings: &[Setting],
        open: Option<usize>,
        hint: &'static str,
    ) {
        let bar_h = Self::height(dpi);
        let size = theme.font_size * dpi;
        Self::background(frame, theme, y, bar_h);
        let s = |v: f32| (v * dpi).round() as i32;
        let pad = s(12.0);
        let ctl = s(30.0);
        let gap = s(4.0);
        let top = y + (bar_h - ctl) / 2;
        let baseline = (top + ctl / 2) as f32 + text.ascent(size) / 2.0;
        let icon_side = s(18.0);
        let done_x = self.paint_done(frame, text, theme, dpi, top, ctl, baseline);
        // Le titre : l'icône « commenter » et le mot, sans fond — l'accent
        // est réservé à l'outil en cours. Il cède sa place aux boutons.
        let title = tr("Commenter");
        let title_w = icon_side + s(8.0) + text.measure(size, title).ceil() as i32;
        let group_gap = s(17.0);
        let buttons_w = (ctl + gap) * tools.len() as i32 - gap + group_gap * groups.len() as i32;
        // Largeur de chaque réglage : une pastille, ou sa valeur et le
        // chevron.
        let widths: Vec<i32> = settings
            .iter()
            .map(|setting| setting_width(text, theme, dpi, setting))
            .collect();
        let mut left = pad;
        // Le titre cède sa place aux réglages et à la consigne d'un outil
        // de dessin : régler l'outil, et savoir s'en servir, comptent plus
        // que de lire « Commenter ».
        if settings.is_empty() && pad + title_w + s(16.0) + buttons_w + s(16.0) <= done_x {
            icons::draw(
                frame,
                raster,
                Icon::Comment,
                left,
                top + (ctl - icon_side) / 2,
                icon_side as f32,
                theme.text,
            );
            text.draw(
                frame,
                (left + icon_side + s(8.0)) as f32,
                baseline,
                size,
                title,
                theme.text,
            );
            left += title_w + s(12.0);
            // Un filet sépare le titre des outils.
            frame.fill_rect(
                left,
                top + s(5.0),
                s(1.0).max(1),
                ctl - s(10.0),
                theme.separator.0,
                theme.separator.1,
                theme.separator.2,
            );
            left += s(12.0);
        }
        self.tools.clear();
        for (index, icon) in tools.iter().enumerate() {
            if groups.contains(&index) {
                // Un filet entre deux groupes d'outils.
                frame.fill_rect(
                    left + (group_gap - gap) / 2 - 1,
                    top + s(7.0),
                    s(1.0).max(1),
                    ctl - s(14.0),
                    theme.separator.0,
                    theme.separator.1,
                    theme.separator.2,
                );
                left += group_gap;
            }
            let r = (left, top, ctl, ctl);
            let on = active == Some(index);
            if on {
                round_rect(frame, r.0, r.1, r.2, r.3, 7.0 * dpi, theme.accent);
            } else if self.hover_tool == Some(index) {
                round_rect(frame, r.0, r.1, r.2, r.3, 7.0 * dpi, theme.hover);
            }
            let colour = if on { (255, 255, 255) } else { theme.text };
            icons::draw(
                frame,
                raster,
                *icon,
                r.0 + (ctl - icon_side) / 2,
                r.1 + (ctl - icon_side) / 2,
                icon_side as f32,
                colour,
            );
            self.tools.push(r);
            left += ctl + gap;
        }
        // Les réglages de l'outil, après un filet ; ceux qui ne tiennent pas
        // avant « Terminer » ne sont pas dessinés.
        self.settings.clear();
        if !settings.is_empty() {
            let limit = done_x - s(14.0);
            left += s(8.0);
            if left + s(10.0) < limit {
                frame.fill_rect(
                    left,
                    top + s(5.0),
                    s(1.0).max(1),
                    ctl - s(10.0),
                    theme.separator.0,
                    theme.separator.1,
                    theme.separator.2,
                );
            }
            left += s(10.0);
            for (index, (setting, &w)) in settings.iter().zip(&widths).enumerate() {
                if left + w > limit {
                    break;
                }
                let r = (left, top, w, ctl);
                let face = if open == Some(index) {
                    theme.separator
                } else if self.hover_setting == Some(index) {
                    theme.hover
                } else {
                    theme.bar
                };
                paint_setting(frame, text, raster, theme, dpi, r, setting, face);
                self.settings.push(r);
                left += w + gap;
            }
        }
        // La consigne, rognée sur ce qui reste.
        let hx = left + s(10.0);
        text.draw_clipped(
            frame,
            hx as f32,
            baseline,
            size,
            tr(hint),
            theme.text_dim,
            (done_x - s(14.0) - hx).max(0) as f32,
        );
    }
}

/// Largeur d'un réglage : une pastille, ou sa valeur et le chevron. La
/// barre des commentaires et la barre de propriétés d'une annotation
/// sélectionnée (`annotbar`) en ont la même allure.
pub(crate) fn setting_width(
    text: &mut TextRenderer,
    theme: &Theme,
    dpi: f32,
    setting: &Setting,
) -> i32 {
    let s = |v: f32| (v * dpi).round() as i32;
    match setting {
        Setting::Color { .. } => s(46.0),
        Setting::Choice(label) => {
            (text.measure(theme.font_size * dpi, label).ceil() as i32 + s(36.0)).max(s(58.0))
        }
    }
}

/// Dessine un réglage dans `r` : son fond (`face`), sa pastille ou sa
/// valeur, et le chevron qui dit qu'il se déroule.
#[allow(clippy::too_many_arguments)] // un bouton, son contenu, son allure
pub(crate) fn paint_setting(
    frame: &mut Frame<'_>,
    text: &mut TextRenderer,
    raster: &mut Rasterizer,
    theme: &Theme,
    dpi: f32,
    r: Rect,
    setting: &Setting,
    face: (u8, u8, u8),
) {
    let s = |v: f32| (v * dpi).round() as i32;
    let size = theme.font_size * dpi;
    let (left, top, w, ctl) = r;
    let baseline = (top + ctl / 2) as f32 + text.ascent(size) / 2.0;
    round_rect(frame, r.0, r.1, r.2, r.3, 7.0 * dpi, face);
    let chip = s(18.0);
    let (cx, cy) = (left + s(7.0), top + (ctl - chip) / 2);
    match setting {
        Setting::Color { color, swatch } => {
            paint_swatch(frame, text, theme, dpi, (cx, cy, chip), (*color, *swatch));
        }
        Setting::Choice(label) => {
            text.draw(
                frame,
                (left + s(10.0)) as f32,
                baseline,
                size,
                label,
                theme.text,
            );
        }
    }
    let chevron = s(12.0);
    icons::draw(
        frame,
        raster,
        Icon::ChevronDown,
        left + w - chevron - s(6.0),
        top + (ctl - chevron) / 2,
        chevron as f32,
        theme.text_dim,
    );
}

/// Dessine une pastille de couleur dans le carré `(x, y, côté)` : un anneau
/// pour un trait, un carré plein pour un fond, un « A » souligné pour du
/// texte ; barrée quand il n'y a pas de couleur.
fn paint_swatch(
    frame: &mut Frame<'_>,
    text: &mut TextRenderer,
    theme: &Theme,
    dpi: f32,
    (x, y, side): (i32, i32, i32),
    (color, swatch): (Option<(u8, u8, u8)>, Swatch),
) {
    let Some(ink) = color else {
        crate::ui::pickers::no_color_chip(frame, x, y, side, dpi, theme);
        return;
    };
    match swatch {
        Swatch::Stroke => {
            // Un liseré autour de l'anneau : un trait noir sur une barre
            // sombre ne se verrait pas.
            round_rect_outline(
                frame,
                x,
                y,
                side,
                side,
                side as f32 / 2.0,
                dpi.max(1.0),
                theme.text_dim,
            );
            let ring = (4.0 * dpi).round();
            let inset = dpi.round().max(1.0) as i32;
            round_rect_outline(
                frame,
                x + inset,
                y + inset,
                side - 2 * inset,
                side - 2 * inset,
                (side - 2 * inset) as f32 / 2.0,
                ring,
                ink,
            );
        }
        Swatch::Fill => {
            round_rect(frame, x, y, side, side, 3.0 * dpi, ink);
            round_rect_outline(
                frame,
                x,
                y,
                side,
                side,
                3.0 * dpi,
                dpi.max(1.0),
                theme.separator,
            );
        }
        Swatch::Text => {
            // Comme la barre « Modifier le PDF » : la lettre dit « couleur
            // du texte », la barre dit laquelle.
            let font_px = theme.font_size * dpi;
            let letter_w = text.measure(font_px, "A");
            let bar = (4.0 * dpi).round() as i32;
            text.draw(
                frame,
                x as f32 + (side as f32 - letter_w) / 2.0,
                (y + side - bar - (2.0 * dpi) as i32) as f32,
                font_px,
                "A",
                theme.text,
            );
            // Le même liseré sous la barre de couleur.
            round_rect(
                frame,
                x - 1,
                y + side - bar - 1,
                side + 2,
                bar + 2,
                3.0 * dpi,
                theme.text_dim,
            );
            round_rect(frame, x, y + side - bar, side, bar, 2.0 * dpi, ink);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLS: [Icon; 7] = [
        Icon::Highlight,
        Icon::Underline,
        Icon::StrikeOut,
        Icon::Squiggly,
        Icon::Insert,
        Icon::Replace,
        Icon::Note,
    ];

    fn painted(width: u32, active: Option<usize>) -> Option<ModeBar> {
        let mut text = TextRenderer::system()?;
        let mut raster = Rasterizer::new();
        let height = 60_u32;
        let mut pixels = vec![0_u8; (width * height * 4) as usize];
        let mut frame = Frame::new(width, height, &mut pixels);
        let mut bar = ModeBar::default();
        bar.paint_tools(
            &mut frame,
            &mut text,
            &mut raster,
            &Theme::dark(),
            1.0,
            0,
            &TOOLS,
            &[4],
            active,
            &[],
            None,
            "Sélectionnez du texte, puis choisissez un outil.",
        );
        Some(bar)
    }

    /// Les réglages se cliquent là où ils sont dessinés, après les outils ;
    /// ceux qui ne tiennent pas sont laissés de côté, jamais les outils.
    #[test]
    fn les_reglages_suivent_les_outils() {
        let Some(mut text) = TextRenderer::system() else {
            return;
        };
        let mut raster = Rasterizer::new();
        let settings = [
            Setting::Color {
                color: Some((200, 30, 30)),
                swatch: Swatch::Stroke,
            },
            Setting::Color {
                color: None,
                swatch: Swatch::Fill,
            },
            Setting::Choice("2 pt".into()),
            Setting::Choice("100 %".into()),
        ];
        for width in [1400_u32, 500] {
            let mut pixels = vec![0_u8; (width * 60 * 4) as usize];
            let mut frame = Frame::new(width, 60, &mut pixels);
            let mut bar = ModeBar::default();
            bar.paint_tools(
                &mut frame,
                &mut text,
                &mut raster,
                &Theme::light(),
                1.0,
                0,
                &TOOLS,
                &[4],
                Some(1),
                &settings,
                Some(2),
                "Faites glisser sur la page.",
            );
            assert_eq!(bar.tools.len(), TOOLS.len(), "{width}");
            if width > 1000 {
                assert_eq!(bar.settings.len(), settings.len());
            } else {
                assert!(bar.settings.len() < settings.len(), "{width}");
            }
            let done = bar.close.unwrap_or_default();
            assert!(bar.settings.iter().all(|r| r.0 + r.2 <= done.0));
            let last_tool = bar.tools[TOOLS.len() - 1];
            for (i, r) in bar.settings.iter().enumerate() {
                assert!(r.0 > last_tool.0 + last_tool.2, "{r:?}");
                let (x, y) = center(*r);
                assert_eq!(bar.setting_at(x, y), Some(i));
                assert_eq!(bar.tool_at(x, y), None);
            }
            if bar.settings.len() > 1 {
                let (x, y) = center(bar.settings[1]);
                assert!(bar.mouse_move(x, y));
                assert_eq!(bar.hovered().map(|(h, _)| h), Some(Hovered::Setting(1)));
            }
        }
    }

    /// Un filet sépare les groupes : le premier bouton du second groupe
    /// est plus loin de son voisin que les autres.
    #[test]
    fn un_filet_entre_les_groupes() {
        let Some(bar) = painted(1200, None) else {
            return;
        };
        let step = |i: usize| bar.tools[i + 1].0 - bar.tools[i].0;
        assert!(step(3) > step(2), "{} / {}", step(3), step(2));
        assert_eq!(step(0), step(1));
    }

    fn center(r: Rect) -> (i32, i32) {
        (r.0 + r.2 / 2, r.1 + r.3 / 2)
    }

    /// Chaque bouton se retrouve là où il a été dessiné ; la consigne et
    /// « Terminer » ne sont pas des outils.
    #[test]
    fn les_outils_se_cliquent_la_ou_ils_sont_dessines() {
        let Some(bar) = painted(1200, Some(2)) else {
            return;
        };
        assert_eq!(bar.tools.len(), TOOLS.len());
        for (i, r) in bar.tools.iter().enumerate() {
            let (x, y) = center(*r);
            assert_eq!(bar.tool_at(x, y), Some(i));
        }
        // De gauche à droite, sans chevauchement.
        assert!(bar.tools.windows(2).all(|w| w[0].0 + w[0].2 <= w[1].0));
        // À droite du dernier bouton : la consigne.
        let last = bar.tools[TOOLS.len() - 1];
        assert_eq!(bar.tool_at(last.0 + last.2 + 40, last.1 + 5), None);
        let done = bar.close.unwrap_or_default();
        assert!(bar.closes(done.0 + 2, done.1 + 2));
        assert_eq!(bar.tool_at(done.0 + 2, done.1 + 2), None);
    }

    /// Le survol ne demande à repeindre que quand il change de bouton, et
    /// dit lequel pour l'info-bulle.
    #[test]
    fn le_survol_change_de_bouton() {
        let Some(mut bar) = painted(1200, None) else {
            return;
        };
        let (x, y) = center(bar.tools[0]);
        assert!(bar.mouse_move(x, y));
        assert!(!bar.mouse_move(x + 1, y), "même bouton : rien à repeindre");
        assert_eq!(bar.hover_tip().map(|(i, _)| i), Some(0));
        let (x, y) = center(bar.tools[3]);
        assert!(bar.mouse_move(x, y));
        assert_eq!(bar.hover_tip().map(|(i, _)| i), Some(3));
        assert!(bar.mouse_move(5, 200));
        assert_eq!(bar.hover_tip(), None);
    }

    /// Fenêtre étroite : le titre s'efface, les boutons restent tous
    /// entiers et avant « Terminer ».
    #[test]
    fn fenetre_etroite_les_outils_restent() {
        let Some(bar) = painted(420, None) else {
            return;
        };
        let done = bar.close.unwrap_or_default();
        assert_eq!(bar.tools.len(), TOOLS.len());
        let last = bar.tools[TOOLS.len() - 1];
        assert!(last.0 + last.2 <= done.0, "{last:?} / {done:?}");
        // Le premier bouton a pris la place du titre.
        assert!(bar.tools[0].0 < 40, "{:?}", bar.tools[0]);
    }

    /// La barre d'un seul outil n'a pas de boutons d'outil.
    #[test]
    fn la_barre_dun_outil_na_pas_de_rangee() {
        let Some(mut text) = TextRenderer::system() else {
            return;
        };
        let mut raster = Rasterizer::new();
        let mut pixels = vec![0_u8; 800 * 60 * 4];
        let mut frame = Frame::new(800, 60, &mut pixels);
        let mut bar = ModeBar::default();
        bar.paint(
            &mut frame,
            &mut text,
            &mut raster,
            &Theme::light(),
            1.0,
            0,
            Icon::Redact,
            "Biffer",
            "Faites glisser sur le texte à biffer.",
        );
        assert!(bar.tools.is_empty());
        assert_eq!(bar.tool_at(30, 30), None);
        assert!(bar.close.is_some());
    }
}
