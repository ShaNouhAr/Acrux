//! Barre latérale des outils, à droite de la page.
//!
//! Une barre d'outils en haut ne tient que ce qui sert *à lire* : tourner les
//! pages, zoomer, chercher. Tout ce qui sert à *travailler* — modifier le
//! texte, signer, biffer, réorganiser les pages — n'y trouve pas de place, et
//! se retrouve derrière des raccourcis que personne ne devine. C'est pour cela
//! qu'Acrobat range ses outils dans une colonne à droite : ils sont là,
//! nommés, sans qu'on ait à les chercher.
//!
//! Cette barre ne connaît ni le document ni le visualiseur. Elle reçoit ce
//! qu'elle doit savoir ([`ToolsInfo`]) et rend une [`Command`] quand on clique.
//! C'est la même commande que la palette et les raccourcis clavier : un outil
//! ne peut donc pas faire autre chose ici qu'ailleurs.
//!
//! # Ce qui est grisé
//!
//! Un outil qui demande un document ouvert reste visible mais éteint quand il
//! n'y en a pas. Le masquer ferait une colonne qui change de forme au
//! chargement ; le laisser cliquable mentirait.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    // `paint` reçoit tout son contexte en arguments : la barre ne garde rien
    // du visualiseur, c'est ce qui la rend testable seule.
    clippy::too_many_arguments
)]

use acrux_graphics::Rasterizer;

use crate::platform::Frame;
use crate::ui::icons::{self, Icon};
use crate::ui::lang::tr;
use crate::ui::paint::round_rect;
use crate::ui::palette::Command;
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Largeur de la barre, en pixels logiques.
pub const WIDTH: u32 = 232;
/// Hauteur d'une ligne d'outil, en pixels logiques.
const ROW: f64 = 32.0;
/// Hauteur d'un intertitre.
const HEADING: f64 = 28.0;
/// Marge gauche du contenu.
const PAD: f64 = 12.0;
/// Côté de l'icône.
const ICON: f64 = 18.0;

/// Ce que la barre doit savoir pour se dessiner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ToolsInfo {
    /// Vrai si un document est ouvert.
    pub has_document: bool,
    /// Outil actuellement ouvert, mis en évidence.
    pub active: Option<Command>,
}

/// Une ligne de la barre.
enum Row {
    /// Intertitre de groupe.
    Heading(&'static str),
    /// Outil : libellé, icône, commande, et s'il lui faut un document.
    Tool(&'static str, Icon, Command, bool),
}

/// Les outils, dans l'ordre où ils s'affichent.
///
/// L'ordre suit celui du travail : on modifie, puis on commente, puis on
/// signe, puis on range les pages, puis on protège, puis on sort le document.
const ROWS: &[Row] = &[
    Row::Heading("Modifier"),
    Row::Tool("Modifier le PDF", Icon::EditText, Command::EditPdf, true),
    Row::Tool("Ajouter du texte", Icon::AddText, Command::AddTextBox, true),
    Row::Tool(
        "Modifier les objets",
        Icon::Objects,
        Command::EditObjects,
        true,
    ),
    Row::Tool("Ajouter une image", Icon::Image, Command::AddImage, true),
    Row::Heading("Commenter"),
    Row::Tool("Surligner", Icon::Highlight, Command::HighlightTool, true),
    Row::Tool("Poser une note", Icon::Note, Command::NoteTool, true),
    // Les formes et le crayon se choisissent dans la barre des commentaires,
    // qui s'ouvre avec le rectangle : une seule ligne ici.
    Row::Tool("Dessiner", Icon::Rectangle, Command::RectangleTool, true),
    Row::Tool("Zone de texte", Icon::TextBox, Command::TextBoxTool, true),
    Row::Tool("Tamponner", Icon::Stamp, Command::StampTool, true),
    // Souligner, barrer, insérer, remplacer… : la barre des commentaires
    // les tient tous ensemble, au-dessus de la page.
    Row::Tool(
        "Outils de commentaire",
        Icon::Comment,
        Command::CommentBar,
        true,
    ),
    Row::Heading("Signer"),
    Row::Tool("Remplir et signer", Icon::Sign, Command::FillSign, true),
    // « Organiser » d'abord : les vignettes, où l'on sélectionne ce que
    // visent les outils qui suivent (voir `viewer/organize.rs`).
    Row::Heading("Pages"),
    Row::Tool(
        "Organiser les pages",
        Icon::ViewMode,
        Command::OrganizePages,
        true,
    ),
    Row::Tool(
        "Insérer une page vierge",
        Icon::PageInsert,
        Command::InsertBlankAfter,
        true,
    ),
    Row::Tool("Insérer des pages", Icon::Open, Command::InsertPages, true),
    Row::Tool(
        "Remplacer des pages",
        Icon::Replace,
        Command::ReplacePages,
        true,
    ),
    Row::Tool(
        "Fractionner",
        Icon::PageDuplicate,
        Command::SplitDocument,
        true,
    ),
    Row::Tool("Extraire", Icon::PageExtract, Command::ExtractPage, true),
    Row::Tool("Pivoter", Icon::Rotate, Command::RotateRight, true),
    Row::Tool("Supprimer", Icon::PageDelete, Command::DeletePage, true),
    Row::Heading("Protéger"),
    Row::Tool(
        "Protéger par mot de passe",
        Icon::Lock,
        Command::Protect,
        true,
    ),
    Row::Tool("Biffer", Icon::Redact, Command::RedactTool, true),
    Row::Tool(
        "Appliquer les biffures",
        Icon::RedactApply,
        Command::ApplyRedactions,
        true,
    ),
    Row::Heading("Document"),
    Row::Tool(
        "Nouveau PDF vierge",
        Icon::PageInsert,
        Command::NewBlank,
        false,
    ),
    Row::Tool("Exporter", Icon::Export, Command::Export, true),
    Row::Tool(
        "Joindre un fichier",
        Icon::Attach,
        Command::AddAttachment,
        true,
    ),
    Row::Tool("Imprimer", Icon::Print, Command::Print, true),
];

/// La barre latérale des outils.
#[derive(Debug, Default)]
pub struct ToolsPanel {
    /// Ligne survolée.
    hover: Option<usize>,
    /// Décalage de défilement, en pixels logiques.
    scroll: f64,
}

impl ToolsPanel {
    /// Barre neuve.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Hauteur totale du contenu, en pixels logiques.
    fn content_height() -> f64 {
        ROWS.iter()
            .map(|r| match r {
                Row::Heading(_) => HEADING,
                Row::Tool(..) => ROW,
            })
            .sum::<f64>()
            + PAD * 2.0
    }

    /// Y du haut de chaque ligne, en pixels logiques, décalage compris.
    fn offsets(&self) -> Vec<f64> {
        let mut y = PAD - self.scroll;
        ROWS.iter()
            .map(|r| {
                let top = y;
                y += match r {
                    Row::Heading(_) => HEADING,
                    Row::Tool(..) => ROW,
                };
                top
            })
            .collect()
    }

    /// Indice de la ligne sous un point, en pixels physiques relatifs à la
    /// barre. `None` hors d'un outil (intertitre, marge).
    fn row_at(&self, y: f64, dpi: f64) -> Option<usize> {
        let logical = y / dpi;
        let tops = self.offsets();
        ROWS.iter().enumerate().position(|(i, r)| {
            let h = match r {
                Row::Heading(_) => HEADING,
                Row::Tool(..) => ROW,
            };
            matches!(r, Row::Tool(..)) && logical >= tops[i] && logical < tops[i] + h
        })
    }

    /// Note la position de la souris ; rend vrai si l'affichage doit changer.
    pub fn hover(&mut self, y: f64, dpi: f64) -> bool {
        let row = self.row_at(y, dpi);
        let changed = row != self.hover;
        self.hover = row;
        changed
    }

    /// Oublie le survol quand la souris quitte la barre.
    pub fn leave(&mut self) -> bool {
        let changed = self.hover.is_some();
        self.hover = None;
        changed
    }

    /// Commande de l'outil cliqué.
    #[must_use]
    pub fn click(&self, y: f64, dpi: f64, info: ToolsInfo) -> Option<Command> {
        let index = self.row_at(y, dpi)?;
        match ROWS.get(index)? {
            Row::Tool(_, _, command, needs) => (!*needs || info.has_document).then_some(*command),
            Row::Heading(_) => None,
        }
    }

    /// Fait défiler la barre. Rend vrai si quelque chose a bougé.
    pub fn scroll(&mut self, delta: f64, height: f64, dpi: f64) -> bool {
        let visible = height / dpi;
        let max = (Self::content_height() - visible).max(0.0);
        let before = self.scroll;
        self.scroll = (self.scroll + delta / dpi).clamp(0.0, max);
        (self.scroll - before).abs() > 0.5
    }

    /// Dessine la barre dans une sous-vue qui lui est propre.
    pub fn paint(
        &self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f64,
        info: ToolsInfo,
    ) {
        let (w, h) = (frame.width as i32, frame.height as i32);
        frame.fill_rect(0, 0, w, h, theme.bar.0, theme.bar.1, theme.bar.2);
        // Un filet à gauche sépare la barre de la page, comme la barre d'état
        // se sépare du document.
        let t = (dpi).round().max(1.0) as i32;
        frame.fill_rect(
            0,
            0,
            t,
            h,
            theme.separator.0,
            theme.separator.1,
            theme.separator.2,
        );

        let tops = self.offsets();
        for (index, row) in ROWS.iter().enumerate() {
            let top = tops[index] * dpi;
            match row {
                Row::Heading(label) => {
                    if top + HEADING * dpi < 0.0 || top > f64::from(h) {
                        continue;
                    }
                    let size = theme.font_size * 0.86 * dpi as f32;
                    let baseline =
                        top as f32 + (HEADING * dpi) as f32 * 0.5 + text.ascent(size) / 2.0;
                    text.draw(
                        frame,
                        (PAD * dpi) as f32,
                        baseline,
                        size,
                        tr(label),
                        theme.text_dim,
                    );
                }
                Row::Tool(label, icon, command, needs) => {
                    if top + ROW * dpi < 0.0 || top > f64::from(h) {
                        continue;
                    }
                    let enabled = !*needs || info.has_document;
                    let active = info.active == Some(*command);
                    let hovered = self.hover == Some(index) && enabled;
                    let row_h = (ROW * dpi) as i32;
                    let y = top as i32;
                    // Pastille arrondie plutôt qu'un bandeau plein largeur :
                    // c'est ce qui distingue une liste d'une barre.
                    let pill = (6.0 * dpi) as i32;
                    let radius = 8.0 * dpi as f32;
                    if active {
                        round_rect(
                            frame,
                            t + pill,
                            y + (2.0 * dpi) as i32,
                            w - t - 2 * pill,
                            row_h - (4.0 * dpi) as i32,
                            radius,
                            theme.accent,
                        );
                    } else if hovered {
                        round_rect(
                            frame,
                            t + pill,
                            y + (2.0 * dpi) as i32,
                            w - t - 2 * pill,
                            row_h - (4.0 * dpi) as i32,
                            radius,
                            theme.hover,
                        );
                    }
                    let colour = if active {
                        (0xFF, 0xFF, 0xFF)
                    } else if enabled {
                        theme.text
                    } else {
                        theme.text_dim
                    };
                    let side = (ICON * dpi).round() as f32;
                    let gx = (PAD * dpi) as i32;
                    let gy = y + (row_h - side as i32) / 2;
                    icons::draw(frame, raster, *icon, gx, gy, side, colour);
                    let corps = theme.font_size * dpi as f32;
                    let baseline = y as f32 + row_h as f32 * 0.5 + text.ascent(corps) / 2.0;
                    text.draw(
                        frame,
                        gx as f32 + side + (10.0 * dpi) as f32,
                        baseline,
                        corps,
                        tr(label),
                        colour,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_outils_se_reperent_par_leur_ligne() {
        let panel = ToolsPanel::new();
        let info = ToolsInfo {
            has_document: true,
            active: None,
        };
        // Le premier intertitre n'est pas cliquable.
        assert_eq!(panel.click(PAD + 2.0, 1.0, info), None);
        // Le premier outil est juste dessous.
        let premier = PAD + HEADING + ROW / 2.0;
        assert_eq!(panel.click(premier, 1.0, info), Some(Command::EditPdf));
        // Le deuxième suit.
        let second = premier + ROW;
        assert_eq!(panel.click(second, 1.0, info), Some(Command::AddTextBox));
    }

    #[test]
    fn sans_document_les_outils_ne_repondent_pas() {
        let panel = ToolsPanel::new();
        let info = ToolsInfo {
            has_document: false,
            active: None,
        };
        let premier = PAD + HEADING + ROW / 2.0;
        assert_eq!(panel.click(premier, 1.0, info), None);
    }

    #[test]
    fn lechelle_de_lecran_est_prise_en_compte() {
        let panel = ToolsPanel::new();
        let info = ToolsInfo {
            has_document: true,
            active: None,
        };
        let premier = (PAD + HEADING + ROW / 2.0) * 1.5;
        assert_eq!(panel.click(premier, 1.5, info), Some(Command::EditPdf));
    }

    #[test]
    fn le_survol_change_de_ligne() {
        let mut panel = ToolsPanel::new();
        let premier = PAD + HEADING + ROW / 2.0;
        assert!(panel.hover(premier, 1.0));
        assert!(
            !panel.hover(premier + 1.0, 1.0),
            "même ligne, rien à redessiner"
        );
        assert!(panel.hover(premier + ROW, 1.0));
        assert!(panel.leave());
        assert!(!panel.leave());
    }

    #[test]
    fn le_defilement_sarrete_aux_bornes() {
        let mut panel = ToolsPanel::new();
        // Zone plus haute que le contenu : rien ne défile.
        assert!(!panel.scroll(100.0, ToolsPanel::content_height() + 50.0, 1.0));
        // Zone courte : on peut descendre, mais pas au-delà du contenu.
        let courte = 200.0;
        assert!(panel.scroll(10_000.0, courte, 1.0));
        let max = ToolsPanel::content_height() - courte;
        assert!((panel.scroll - max).abs() < 1e-6, "{}", panel.scroll);
        assert!(!panel.scroll(100.0, courte, 1.0), "déjà en bas");
        assert!(panel.scroll(-1000.0, courte, 1.0));
        assert!(panel.scroll.abs() < 1e-6);
    }

    #[test]
    fn tous_les_outils_ont_un_libelle_et_une_commande() {
        let mut outils = 0;
        for row in ROWS {
            if let Row::Tool(label, _, command, _) = row {
                assert!(!label.is_empty());
                // La palette connaît la commande : le libellé et le raccourci
                // viennent de là, donc elle ne peut pas être inconnue.
                assert!(
                    crate::ui::palette::describe(*command).is_some(),
                    "{label} : commande absente de la palette"
                );
                outils += 1;
            }
        }
        assert!(outils >= 12, "{outils} outils");
    }
}
