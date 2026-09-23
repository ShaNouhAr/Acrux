//! Palette de commandes : une liste filtrable de tout ce que l'application
//! sait faire, avec le raccourci de chaque entrée. C'est ce qui rend les
//! fonctions trouvables sans barre de menus — et c'est aussi la
//! documentation vivante des raccourcis.
//!
//! Le filtrage est « approximatif par sous-séquence » : `bif` trouve
//! « Biffure : marquer la sélection », `enrs` trouve « Enregistrer sous ».
//! Les entrées dont le libellé contient la requête d'un seul tenant
//! remontent en tête.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names
)]

use crate::platform::{Frame, Key};
use crate::ui::input::{InputAction, TextInput};
use crate::ui::lang::tr;
use crate::ui::paint::{round_rect, round_rect_outline, shadow};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Une action de l'application, déclenchable depuis la palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Revenir à l'accueil : les documents récents, sans fermer ce qui est
    /// ouvert.
    Home,
    /// Ouvrir les paramètres.
    Settings,
    /// Ouvrir un document.
    Open,
    /// Enregistrer.
    Save,
    /// Enregistrer sous.
    SaveAs,
    /// Imprimer.
    Print,
    /// Convertir le document dans un autre format.
    Export,
    /// Fermer l'onglet.
    CloseTab,
    /// Onglet suivant.
    NextTab,
    /// Page précédente.
    PrevPage,
    /// Page suivante.
    NextPage,
    /// Première page.
    FirstPage,
    /// Dernière page.
    LastPage,
    /// Zoom avant.
    ZoomIn,
    /// Zoom arrière.
    ZoomOut,
    /// Zoom 100 %.
    ZoomReset,
    /// Ajuster à la largeur.
    FitWidth,
    /// Afficher la page entière.
    FitPage,
    /// Ajustement automatique : la largeur, sans agrandir au-delà de 100 %.
    FitAutomatic,
    /// Changer la disposition des pages.
    CycleViewMode,
    /// Plein écran.
    Fullscreen,
    /// Panneau latéral.
    TogglePanel,
    /// Barre des outils, à droite.
    ToggleTools,
    /// Panneau latéral sur les calques.
    ShowLayers,
    /// Panneau latéral sur les pièces jointes.
    ShowAttachments,
    /// Joindre un fichier au document.
    AddAttachment,
    /// Saisir un numéro ou une étiquette de page dans la barre d'outils.
    GoToPage,
    /// Thème clair / sombre.
    ToggleTheme,
    /// Rechercher.
    Search,
    /// Rechercher et remplacer.
    Replace,
    /// Copier la sélection.
    Copy,
    /// Tout sélectionner.
    SelectAll,
    /// Pivoter la page à droite.
    RotateRight,
    /// Pivoter la page à gauche.
    RotateLeft,
    /// Supprimer la page.
    DeletePage,
    /// Insérer les pages d'un autre fichier avant la page courante.
    InsertPages,
    /// Dupliquer la page courante.
    DuplicatePage,
    /// Enregistrer la page courante dans un nouveau fichier.
    ExtractPage,
    /// Annuler.
    Undo,
    /// Rétablir.
    Redo,
    /// Modifier le texte sélectionné.
    EditText,
    /// Mode « Modifier le PDF » : tout le texte devient éditable.
    EditPdf,
    /// Poser une zone de texte neuve.
    AddTextBox,
    /// Surligner la sélection.
    Highlight,
    /// Outil « surligner » : on glisse sur le texte, il se surligne.
    HighlightTool,
    /// Outil « note » : un clic sur la page pose une note.
    NoteTool,
    /// Outil « biffer » : on glisse sur le texte, il est marqué.
    RedactTool,
    /// Poser une note.
    Note,
    /// Chercher une version plus récente.
    CheckUpdates,
    /// Ouvrir ou fermer l'outil « modifier les objets ».
    EditObjects,
    /// Ouvrir ou fermer l'outil « remplir et signer ».
    FillSign,
    /// Marquer la sélection pour biffure.
    MarkRedaction,
    /// Appliquer les biffures.
    ApplyRedactions,
    /// Protéger le document par un mot de passe.
    Protect,
    /// Retirer la protection d'un document chiffré.
    Unprotect,
}

/// Une entrée de la palette.
struct Entry {
    label: &'static str,
    shortcut: &'static str,
    /// Synonymes sans accent : ce que l'utilisateur tape quand il ne connaît
    /// pas le libellé exact (« rotation » pour « Pivoter la page »).
    keywords: &'static str,
    command: Command,
    /// Vrai si l'entrée n'a de sens qu'avec un document ouvert.
    needs_document: bool,
}

/// Libellé et raccourci d'une commande. La palette est la source unique de
/// ces textes : les info-bulles de la barre d'outils les reprennent, ce qui
/// évite qu'un raccourci change ici sans changer là.
#[must_use]
pub fn describe(command: Command) -> Option<(&'static str, &'static str)> {
    ENTRIES
        .iter()
        .find(|e| e.command == command)
        .map(|e| (tr(e.label), e.shortcut))
}

/// Toutes les commandes, dans l'ordre d'affichage quand rien n'est filtré.
const ENTRIES: &[Entry] = &[
    Entry {
        label: "Protéger par mot de passe",
        shortcut: "",
        keywords: "proteger mot de passe chiffrer securite password encrypt",
        command: Command::Protect,
        needs_document: true,
    },
    Entry {
        label: "Retirer la protection",
        shortcut: "",
        keywords: "retirer protection dechiffrer mot de passe decrypt",
        command: Command::Unprotect,
        needs_document: true,
    },
    Entry {
        label: "Paramètres",
        shortcut: "",
        keywords: "paramètres réglages langue settings options",
        command: Command::Settings,
        needs_document: false,
    },
    Entry {
        label: "Accueil",
        shortcut: "",
        keywords: "accueil maison récents démarrage base",
        command: Command::Home,
        needs_document: false,
    },
    Entry {
        label: "Ouvrir un document",
        shortcut: "Ctrl+O",
        keywords: "fichier parcourir ouverture",
        command: Command::Open,
        needs_document: false,
    },
    Entry {
        label: "Enregistrer",
        shortcut: "Ctrl+S",
        keywords: "sauvegarder ecrire disque enregistrement",
        command: Command::Save,
        needs_document: true,
    },
    Entry {
        label: "Enregistrer sous",
        shortcut: "Ctrl+Maj+S",
        keywords: "sauvegarder copie nouveau nom enregistrement",
        command: Command::SaveAs,
        needs_document: true,
    },
    Entry {
        label: "Imprimer",
        shortcut: "Ctrl+P",
        keywords: "impression papier imprimante",
        command: Command::Print,
        needs_document: true,
    },
    Entry {
        label: "Exporter (page web, Word, Excel, images, texte)",
        shortcut: "Ctrl+E",
        keywords: "conversion convertir html docx xlsx png jpeg markdown texte",
        command: Command::Export,
        needs_document: true,
    },
    Entry {
        label: "Fermer l'onglet",
        shortcut: "Ctrl+W",
        keywords: "quitter fermeture document",
        command: Command::CloseTab,
        needs_document: true,
    },
    Entry {
        label: "Onglet suivant",
        shortcut: "Ctrl+Tab",
        keywords: "changer basculer document",
        command: Command::NextTab,
        needs_document: true,
    },
    Entry {
        label: "Page précédente",
        shortcut: "Ctrl+Page préc.",
        keywords: "reculer precedent avant haut",
        command: Command::PrevPage,
        needs_document: true,
    },
    Entry {
        label: "Page suivante",
        shortcut: "Ctrl+Page suiv.",
        keywords: "avancer suivant apres bas",
        command: Command::NextPage,
        needs_document: true,
    },
    Entry {
        label: "Première page",
        shortcut: "Origine",
        keywords: "debut origine haut commencement",
        command: Command::FirstPage,
        needs_document: true,
    },
    Entry {
        label: "Dernière page",
        shortcut: "Fin",
        keywords: "fin bas terminer",
        command: Command::LastPage,
        needs_document: true,
    },
    Entry {
        label: "Zoom avant",
        shortcut: "+",
        keywords: "agrandir grossir loupe plus zoomer",
        command: Command::ZoomIn,
        needs_document: true,
    },
    Entry {
        label: "Zoom arrière",
        shortcut: "-",
        keywords: "reduire diminuer loupe moins dezoomer",
        command: Command::ZoomOut,
        needs_document: true,
    },
    Entry {
        label: "Zoom 100 %",
        shortcut: "1",
        keywords: "taille reelle cent reinitialiser",
        command: Command::ZoomReset,
        needs_document: true,
    },
    Entry {
        label: "Ajuster à la largeur",
        shortcut: "F",
        keywords: "adapter adaptation pleine largeur",
        command: Command::FitWidth,
        needs_document: true,
    },
    Entry {
        label: "Afficher la page entière",
        shortcut: "",
        keywords: "page entiere ajuster hauteur tout voir",
        command: Command::FitPage,
        needs_document: true,
    },
    Entry {
        label: "Ajustement automatique",
        shortcut: "Ctrl+0",
        keywords: "auto automatique defaut largeur sans agrandir",
        command: Command::FitAutomatic,
        needs_document: true,
    },
    Entry {
        label: "Disposition des pages",
        shortcut: "",
        keywords: "double page continu defilement livre disposition mode",
        command: Command::CycleViewMode,
        needs_document: true,
    },
    Entry {
        label: "Plein écran",
        shortcut: "F11",
        keywords: "presentation ecran entier diaporama",
        command: Command::Fullscreen,
        needs_document: false,
    },
    Entry {
        label: "Panneau latéral (vignettes, signets)",
        shortcut: "F4",
        keywords: "vignettes signets sommaire miniatures volet barre",
        command: Command::TogglePanel,
        needs_document: true,
    },
    Entry {
        label: "Barre des outils",
        shortcut: "F3",
        keywords: "outils colonne droite modifier signer biffer",
        command: Command::ToggleTools,
        needs_document: false,
    },
    Entry {
        label: "Calques (afficher ou masquer le contenu optionnel)",
        shortcut: "",
        keywords: "couches contenu optionnel ocg plan visibilite",
        command: Command::ShowLayers,
        needs_document: true,
    },
    Entry {
        label: "Pièces jointes du document",
        shortcut: "",
        keywords: "fichiers joints attachements annexes incorpores enregistrer extraire",
        command: Command::ShowAttachments,
        needs_document: true,
    },
    Entry {
        label: "Joindre un fichier au document",
        shortcut: "",
        keywords: "piece jointe attacher incorporer ajouter annexe fichier",
        command: Command::AddAttachment,
        needs_document: true,
    },
    Entry {
        label: "Aller à une page (numéro ou étiquette)",
        shortcut: "Ctrl+G",
        keywords: "atteindre saut numero etiquette folio romain i ii iii annexe",
        command: Command::GoToPage,
        needs_document: true,
    },
    Entry {
        label: "Thème clair / sombre",
        shortcut: "T",
        keywords: "nuit jour sombre clair couleurs apparence",
        command: Command::ToggleTheme,
        needs_document: false,
    },
    Entry {
        label: "Rechercher dans le document",
        shortcut: "Ctrl+F",
        keywords: "trouver chercher recherche texte occurrences",
        command: Command::Search,
        needs_document: true,
    },
    Entry {
        label: "Rechercher et remplacer",
        shortcut: "Ctrl+H",
        keywords: "remplacer substituer corriger partout texte",
        command: Command::Replace,
        needs_document: true,
    },
    Entry {
        label: "Copier la sélection",
        shortcut: "Ctrl+C",
        keywords: "presse papiers copie duplication",
        command: Command::Copy,
        needs_document: true,
    },
    Entry {
        label: "Tout sélectionner",
        shortcut: "Ctrl+A",
        keywords: "selection totale integralite",
        command: Command::SelectAll,
        needs_document: true,
    },
    Entry {
        label: "Pivoter la page à droite",
        shortcut: "R",
        keywords: "rotation tourner orientation horaire pivotement",
        command: Command::RotateRight,
        needs_document: true,
    },
    Entry {
        label: "Pivoter la page à gauche",
        shortcut: "Maj+R",
        keywords: "rotation tourner orientation antihoraire pivotement",
        command: Command::RotateLeft,
        needs_document: true,
    },
    Entry {
        label: "Supprimer la page",
        shortcut: "Ctrl+Suppr",
        keywords: "effacer retirer enlever suppression",
        command: Command::DeletePage,
        needs_document: true,
    },
    Entry {
        label: "Insérer les pages d'un fichier",
        shortcut: "Ctrl+I",
        keywords: "ajouter importer fusionner coller document",
        command: Command::InsertPages,
        needs_document: true,
    },
    Entry {
        label: "Dupliquer la page",
        shortcut: "",
        keywords: "copier doubler repeter",
        command: Command::DuplicatePage,
        needs_document: true,
    },
    Entry {
        label: "Extraire la page dans un fichier",
        shortcut: "",
        keywords: "exporter isoler separer decouper",
        command: Command::ExtractPage,
        needs_document: true,
    },
    Entry {
        label: "Annuler",
        shortcut: "Ctrl+Z",
        keywords: "revenir defaire annulation historique",
        command: Command::Undo,
        needs_document: true,
    },
    Entry {
        label: "Rétablir",
        shortcut: "Ctrl+Y",
        keywords: "refaire retablissement historique",
        command: Command::Redo,
        needs_document: true,
    },
    Entry {
        label: "Modifier le texte sélectionné",
        shortcut: "E",
        keywords: "corriger remplacer retoucher saisir mot phrase",
        command: Command::EditText,
        needs_document: true,
    },
    Entry {
        label: "Modifier le PDF",
        shortcut: "Ctrl+Maj+E",
        keywords: "editer edition modifier texte corriger paragraphe acrobat",
        command: Command::EditPdf,
        needs_document: true,
    },
    Entry {
        label: "Ajouter du texte",
        shortcut: "",
        keywords: "zone texte ajouter ecrire inserer boite",
        command: Command::AddTextBox,
        needs_document: true,
    },
    Entry {
        label: "Surligner la sélection",
        shortcut: "H",
        keywords: "surlignage annotation marqueur couleur",
        command: Command::Highlight,
        needs_document: true,
    },
    Entry {
        label: "Outil surligneur",
        shortcut: "",
        keywords: "surligner surligneur marquer fluo outil",
        command: Command::HighlightTool,
        needs_document: true,
    },
    Entry {
        label: "Outil note",
        shortcut: "",
        keywords: "note commentaire bulle poser outil",
        command: Command::NoteTool,
        needs_document: true,
    },
    Entry {
        label: "Outil biffure",
        shortcut: "",
        keywords: "biffer caviarder masquer noircir outil",
        command: Command::RedactTool,
        needs_document: true,
    },
    Entry {
        label: "Poser une note",
        shortcut: "N",
        keywords: "commentaire annotation bulle remarque",
        command: Command::Note,
        needs_document: true,
    },
    Entry {
        label: "Rechercher les mises à jour",
        shortcut: "",
        keywords: "version nouvelle telecharger installer maj update",
        command: Command::CheckUpdates,
        needs_document: false,
    },
    Entry {
        label: "Modifier : objets de la page",
        shortcut: "O",
        keywords: "image deplacer redimensionner recadrer ordre supprimer objet dessin logo",
        command: Command::EditObjects,
        needs_document: true,
    },
    Entry {
        label: "Remplir et signer",
        shortcut: "S",
        keywords: "signature signer parapher initiales coche croix formulaire manuscrite",
        command: Command::FillSign,
        needs_document: true,
    },
    Entry {
        label: "Biffure : marquer la sélection",
        shortcut: "M",
        keywords: "caviarder censurer masquer confidentiel expurger",
        command: Command::MarkRedaction,
        needs_document: true,
    },
    Entry {
        label: "Biffure : appliquer définitivement",
        shortcut: "Maj+M",
        keywords: "caviarder censurer expurger definitif confidentiel",
        command: Command::ApplyRedactions,
        needs_document: true,
    },
];

/// Score de correspondance : plus il est petit, mieux c'est ; `None` si la
/// requête ne correspond pas du tout.
fn fold(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ä' | 'ã' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            other => other,
        })
        .collect()
}

/// Score d'une entrée : son libellé d'abord, ses synonymes ensuite (pénalisés
/// pour qu'une correspondance dans le libellé reste toujours devant).
fn entry_score(entry: &Entry, query: &str) -> Option<usize> {
    score(entry.label, query).or_else(|| score(entry.keywords, query).map(|s| s + 2000))
}

fn score(label: &str, query: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(1000);
    }
    let label_low = fold(label);
    let query_low = fold(query);
    if let Some(at) = label_low.find(&query_low) {
        // Correspondance d'un seul tenant : d'autant meilleure qu'elle est tôt.
        return Some(at);
    }
    // Sinon, sous-séquence : toutes les lettres dans l'ordre.
    let mut chars = label_low.chars();
    let mut spread = 0usize;
    for want in query_low.chars() {
        let mut steps = 0;
        loop {
            let c = chars.next()?;
            steps += 1;
            if c == want {
                break;
            }
        }
        spread += steps;
    }
    Some(500 + spread)
}

/// Où tombe l'appui de la souris.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteDown {
    /// Sur une ligne : elle s'enfonce, et s'exécutera au relâchement.
    Row,
    /// Dans la carte, hors des lignes (le champ) : la palette reste.
    Inside,
    /// Dehors : la palette se ferme, sans rien exécuter.
    Outside,
}

/// Palette de commandes.
pub struct Palette {
    /// Champ de filtrage.
    pub input: TextInput,
    /// Indices dans [`ENTRIES`], filtrés et triés.
    filtered: Vec<usize>,
    /// Entrée sélectionnée dans `filtered`.
    selected: usize,
    /// Rectangles des lignes au dernier dessin.
    rows: Vec<(i32, i32, i32, i32)>,
    /// Rectangle de la carte au dernier dessin.
    card: (i32, i32, i32, i32),
    /// Ligne enfoncée, qui attend le relâchement pour s'exécuter.
    pressed: Option<usize>,
    /// Un document est ouvert.
    has_document: bool,
}

impl Palette {
    /// Palette ouverte, filtre vide.
    #[must_use]
    pub fn new(has_document: bool) -> Self {
        let mut p = Self {
            input: TextInput::new("Tapez une commande"),
            filtered: Vec::new(),
            selected: 0,
            rows: Vec::new(),
            card: (0, 0, 0, 0),
            pressed: None,
            has_document,
        };
        p.refilter();
        p
    }

    /// Recalcule la liste filtrée.
    fn refilter(&mut self) {
        let query = self.input.value.clone();
        let mut scored: Vec<(usize, usize)> = ENTRIES
            .iter()
            .enumerate()
            .filter(|(_, e)| self.has_document || !e.needs_document)
            .filter_map(|(i, e)| entry_score(e, &query).map(|s| (s, i)))
            .collect();
        scored.sort_by_key(|&(s, i)| (s, i));
        self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        self.selected = 0;
    }

    /// Commande actuellement sélectionnée.
    #[must_use]
    pub fn current(&self) -> Option<Command> {
        self.filtered
            .get(self.selected)
            .and_then(|&i| ENTRIES.get(i))
            .map(|e| e.command)
    }

    /// Touche ; `Some(commande)` quand l'utilisateur valide.
    pub fn key(&mut self, key: Key, shift: bool) -> (Option<Command>, bool) {
        match key {
            Key::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                (None, false)
            }
            Key::Down => {
                if self.selected + 1 < self.filtered.len() {
                    self.selected += 1;
                }
                (None, false)
            }
            Key::Tab => {
                if self.filtered.is_empty() {
                    return (None, false);
                }
                let n = self.filtered.len();
                self.selected = if shift {
                    (self.selected + n - 1) % n
                } else {
                    (self.selected + 1) % n
                };
                (None, false)
            }
            other => match self.input.key(other, shift) {
                InputAction::Submit => (self.current(), true),
                InputAction::Cancel => (None, true),
                InputAction::Changed => {
                    self.refilter();
                    (None, false)
                }
                InputAction::None => (None, false),
            },
        }
    }

    /// Caractère saisi.
    pub fn char(&mut self, c: char) {
        if self.input.insert_char(c) == InputAction::Changed {
            self.refilter();
        }
    }

    /// Ligne sous un point, d'après le dernier dessin.
    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        self.rows
            .iter()
            .position(|&(rx, ry, rw, rh)| x >= rx && x < rx + rw && y >= ry && y < ry + rh)
    }

    /// Appui : une ligne s'enfonce (et se sélectionne), mais rien ne
    /// s'exécute encore — comme un bouton, une commande part au relâchement,
    /// et l'on se ravise en glissant hors de la ligne.
    pub fn mouse_down(&mut self, x: i32, y: i32) -> PaletteDown {
        self.pressed = self.row_at(x, y);
        if let Some(index) = self.pressed {
            self.selected = index;
            return PaletteDown::Row;
        }
        let (cx, cy, cw, ch) = self.card;
        if x >= cx && x < cx + cw && y >= cy && y < cy + ch {
            PaletteDown::Inside
        } else {
            PaletteDown::Outside
        }
    }

    /// Relâchement : la commande de la ligne enfoncée, si le pointeur est
    /// resté dessus.
    pub fn mouse_up(&mut self, x: i32, y: i32) -> Option<Command> {
        let pressed = self.pressed.take()?;
        if self.row_at(x, y) != Some(pressed) {
            return None;
        }
        self.filtered
            .get(pressed)
            .and_then(|&i| ENTRIES.get(i))
            .map(|e| e.command)
    }

    /// Survol : sélectionne la ligne sous le pointeur ; vrai si ça a changé.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let Some(index) = self.row_at(x, y) else {
            return false;
        };
        let changed = index != self.selected;
        self.selected = index;
        changed
    }

    /// Dessine la palette par-dessus la vue.
    #[allow(clippy::too_many_lines)] // une mise en page, lue de haut en bas
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
    ) {
        let t = theme;
        let size = t.font_size * dpi;
        let pad = (14.0 * dpi) as i32;
        let row = (size * 2.3) as i32;
        let field = (32.0 * dpi) as i32;
        let width = (560.0 * dpi).min(f32::from(frame.width as u16) * 0.9) as i32;
        let shown = self.filtered.len().min(12);
        let height = pad * 2 + field + (12.0 * dpi) as i32 + shown as i32 * row;
        let x = (frame.width as i32 - width) / 2;
        let y = ((frame.height as i32 - height) / 3).max(pad);
        // Une carte posée sur le voile : ombre, coins arrondis, liseré.
        let radius = 14.0 * dpi;
        shadow(
            frame,
            x,
            y + (8.0 * dpi) as i32,
            width,
            height,
            radius,
            30.0 * dpi,
            0.45,
        );
        round_rect(frame, x, y, width, height, radius, t.bar);
        round_rect_outline(
            frame,
            x,
            y,
            width,
            height,
            radius,
            dpi.max(1.0),
            t.separator,
        );
        self.card = (x, y, width, height);
        self.input.draw(
            frame,
            text,
            t,
            dpi,
            x + pad,
            y + pad,
            width - 2 * pad,
            field,
        );
        self.rows.clear();
        // Un trait sépare le champ des commandes.
        frame.fill_rect(
            x + pad,
            y + pad + field + (6.0 * dpi) as i32,
            width - 2 * pad,
            1,
            t.separator.0,
            t.separator.1,
            t.separator.2,
        );
        let mut ry = y + pad + field + (12.0 * dpi) as i32;
        for (index, &entry) in self.filtered.iter().take(shown).enumerate() {
            let Some(e) = ENTRIES.get(entry) else {
                continue;
            };
            if index == self.selected {
                round_rect(frame, x + 6, ry, width - 12, row, 8.0, t.hover);
            }
            let baseline = ry as f32 + f32::midpoint(row as f32, text.ascent(size)) - 1.0;
            let color = if index == self.selected {
                t.text
            } else {
                t.text_dim
            };
            text.draw_clipped(
                frame,
                (x + pad) as f32,
                baseline,
                size,
                tr(e.label),
                color,
                (width - 2 * pad - (110.0 * dpi) as i32) as f32,
            );
            if !e.shortcut.is_empty() {
                let w = text.measure(size * 0.92, e.shortcut);
                text.draw(
                    frame,
                    (x + width - pad) as f32 - w,
                    baseline,
                    size * 0.92,
                    e.shortcut,
                    t.text_dim,
                );
            }
            self.rows.push((x + 4, ry, width - 8, row));
            ry += row;
        }
        if self.filtered.is_empty() {
            let baseline = ry as f32 + text.ascent(size);
            text.draw(
                frame,
                (x + pad) as f32,
                baseline,
                size,
                "Aucune commande",
                t.text_dim,
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // tests : l'absence de résultat est l'échec cherché
mod tests {
    use super::*;

    /// Tape une requête dans une palette neuve et renvoie la commande en tête.
    fn top(query: &str) -> Option<Command> {
        let mut p = Palette::new(true);
        for c in query.chars() {
            p.char(c);
        }
        p.current()
    }

    #[test]
    fn synonyms_find_commands_whose_label_uses_another_word() {
        assert_eq!(top("rotation"), Some(Command::RotateRight));
        assert_eq!(top("caviarder"), Some(Command::MarkRedaction));
        assert_eq!(top("sombre"), Some(Command::ToggleTheme));
        assert_eq!(top("trouver"), Some(Command::Search));
        assert_eq!(top("diaporama"), Some(Command::Fullscreen));
    }

    #[test]
    fn accents_are_optional_in_both_directions() {
        assert_eq!(top("derniere"), Some(Command::LastPage));
        assert_eq!(top("Première"), Some(Command::FirstPage));
        assert_eq!(top("theme"), Some(Command::ToggleTheme));
        assert_eq!(top("retablir"), Some(Command::Redo));
    }

    #[test]
    fn label_matches_always_win_over_synonym_matches() {
        // « page » est dans plusieurs libellés et dans les synonymes de
        // « Copier » (presse-papiers) : un libellé doit passer devant.
        let first = top("page").expect("au moins une commande");
        assert!(
            matches!(
                first,
                Command::PrevPage
                    | Command::NextPage
                    | Command::FirstPage
                    | Command::LastPage
                    | Command::CycleViewMode
                    | Command::RotateRight
                    | Command::RotateLeft
                    | Command::DeletePage
            ),
            "{first:?} n'a pas « page » dans son libellé"
        );
    }

    #[test]
    fn filtering_finds_commands_by_prefix_and_subsequence() {
        let mut p = Palette::new(true);
        assert_eq!(
            p.filtered.len(),
            ENTRIES.len(),
            "tout est proposé au départ"
        );
        for c in "biff".chars() {
            p.char(c);
        }
        let first = p.current().expect("une commande correspond");
        assert!(matches!(
            first,
            Command::MarkRedaction | Command::ApplyRedactions
        ));
        p.input.clear();
        p.refilter();
        // Correspondance d'un seul tenant, unique dans la liste.
        for c in "sous".chars() {
            p.char(c);
        }
        assert_eq!(p.current(), Some(Command::SaveAs));
        p.input.clear();
        p.refilter();
        // Sous-séquence pure : « plécr » n'est le fragment d'aucun libellé.
        for c in "plécr".chars() {
            p.char(c);
        }
        assert_eq!(p.current(), Some(Command::Fullscreen));
    }

    #[test]
    fn without_a_document_only_global_commands_are_listed() {
        let p = Palette::new(false);
        let labels: Vec<&str> = p
            .filtered
            .iter()
            .filter_map(|&i| ENTRIES.get(i))
            .map(|e| e.label)
            .collect();
        assert!(labels.contains(&"Ouvrir un document"));
        assert!(!labels.iter().any(|l| l.contains("Enregistrer")));
    }

    #[test]
    fn navigation_and_validation() {
        let mut p = Palette::new(true);
        let first = p.current();
        let (cmd, close) = p.key(Key::Down, false);
        assert!(cmd.is_none() && !close);
        assert_ne!(p.current(), first, "la sélection a bougé");
        let (cmd, close) = p.key(Key::Up, false);
        assert!(cmd.is_none() && !close);
        assert_eq!(p.current(), first);
        // Entrée valide, Échap ferme sans rien faire.
        let (cmd, close) = p.key(Key::Enter, false);
        assert_eq!((cmd, close), (first, true));
        let (cmd, close) = p.key(Key::Escape, false);
        assert_eq!((cmd, close), (None, true));
    }

    #[test]
    fn la_palette_choisit_au_relachement() {
        let mut p = Palette::new(true);
        p.card = (0, 0, 400, 300);
        p.rows = vec![(0, 50, 400, 30), (0, 80, 400, 30)];
        let second = p
            .filtered
            .get(1)
            .and_then(|&i| ENTRIES.get(i))
            .map(|e| e.command);
        // L'appui sélectionne sans exécuter ; le relâchement exécute.
        assert_eq!(p.mouse_down(10, 90), PaletteDown::Row);
        assert_eq!(p.current(), second);
        assert_eq!(p.mouse_up(20, 95), second);
        // Glissé vers une autre ligne : on s'est ravisé.
        assert_eq!(p.mouse_down(10, 60), PaletteDown::Row);
        assert_eq!(p.mouse_up(10, 90), None);
        // Un relâchement sans appui ne fait rien.
        assert_eq!(p.mouse_up(10, 60), None);
        // Le champ garde la palette ouverte ; dehors, elle se ferme.
        assert_eq!(p.mouse_down(10, 10), PaletteDown::Inside);
        assert_eq!(p.mouse_down(500, 10), PaletteDown::Outside);
    }

    #[test]
    fn unknown_query_lists_nothing() {
        let mut p = Palette::new(true);
        for c in "zzzqqq".chars() {
            p.char(c);
        }
        assert!(p.filtered.is_empty());
        assert_eq!(p.current(), None);
        let (cmd, close) = p.key(Key::Enter, false);
        assert_eq!((cmd, close), (None, true));
    }
}
