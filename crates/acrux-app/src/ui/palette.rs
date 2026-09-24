//! Palette de commandes : une liste filtrable de tout ce que l'application
//! sait faire, avec le raccourci de chaque entrée. C'est ce qui rend les
//! fonctions trouvables sans barre de menus — et c'est aussi la
//! documentation vivante des raccourcis.
//!
//! Sans requête, les commandes suivent l'ordre d'usage — ouvrir, enregistrer,
//! rechercher, imprimer, remplir et signer… —, précédées des dernières
//! lancées depuis la palette, que les préférences retiennent d'une séance à
//! l'autre.
//!
//! Le filtrage est « approximatif par sous-séquence » : `bif` trouve
//! « Biffure : marquer la sélection », `enrs` trouve « Enregistrer sous ».
//! Les entrées dont le libellé contient la requête d'un seul tenant
//! remontent en tête, et les lettres trouvées sont dessinées dans la couleur
//! d'accent : on voit pourquoi une ligne est là. La recherche porte sur le
//! libellé **affiché** — « open » trouve « Open a document » en anglais.
//!
//! La liste défile avec la sélection, qui reste toujours visible : Entrée ne
//! lance jamais une commande que l'on ne voit pas.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names
)]

use crate::platform::{Frame, Key, Modifiers};
use crate::ui::input::{InputAction, TextInput};
use crate::ui::lang::{english, to_english};
use crate::ui::paint::{round_rect, round_rect_alpha, round_rect_outline, shadow};
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
    /// Onglet précédent.
    PrevTab,
    /// Vue précédente : revenir d'où un lien, un signet ou « aller à la
    /// page » nous a fait partir.
    ViewBack,
    /// Vue suivante : refaire le saut qu'on vient de défaire.
    ViewForward,
    /// Tourner la vue d'un quart de tour à droite, sans toucher au document.
    RotateViewRight,
    /// Tourner la vue d'un quart de tour à gauche, sans toucher au document.
    RotateViewLeft,
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
    /// Dérouler la liste du zoom : niveaux, ajustements, niveau tapé.
    ZoomMenu,
    /// Ajuster à la largeur.
    FitWidth,
    /// Ajuster à la page : la page entière tient dans la fenêtre.
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
    /// Occurrence suivante de la recherche (F3).
    FindNext,
    /// Occurrence précédente de la recherche (Maj+F3).
    FindPrevious,
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
    /// Propriétés du document : fichier, taille, métadonnées, protection.
    Properties,
    /// Fermer tous les onglets sauf un.
    CloseOtherTabs,
    /// Copier le chemin du fichier dans le presse-papiers.
    CopyPath,
    /// Montrer le fichier dans l'Explorateur.
    RevealInFolder,
    /// Retirer un document de la liste des récents (le fichier n'est pas
    /// touché). Absente de la palette : elle ne vaut que pour un document
    /// visé, depuis le menu d'une carte de l'accueil.
    ForgetRecent,
}

impl Command {
    /// Clé stable de la commande, écrite dans les préférences pour retenir
    /// les commandes récentes. Elle ne change jamais, même quand le libellé
    /// ou la place dans la palette changent : un fichier de préférences doit
    /// rester lisible d'une version à l'autre. Le `match` est exhaustif, si
    /// bien qu'une commande ajoutée sans sa clé ne compile pas.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Command::Home => "home",
            Command::Settings => "settings",
            Command::Open => "open",
            Command::Save => "save",
            Command::SaveAs => "save-as",
            Command::Print => "print",
            Command::Export => "export",
            Command::CloseTab => "close-tab",
            Command::NextTab => "next-tab",
            Command::PrevTab => "prev-tab",
            Command::ViewBack => "view-back",
            Command::ViewForward => "view-forward",
            Command::RotateViewRight => "rotate-view-right",
            Command::RotateViewLeft => "rotate-view-left",
            Command::PrevPage => "prev-page",
            Command::NextPage => "next-page",
            Command::FirstPage => "first-page",
            Command::LastPage => "last-page",
            Command::ZoomIn => "zoom-in",
            Command::ZoomOut => "zoom-out",
            Command::ZoomReset => "zoom-reset",
            Command::ZoomMenu => "zoom-menu",
            Command::FitWidth => "fit-width",
            Command::FitPage => "fit-page",
            Command::FitAutomatic => "fit-auto",
            Command::CycleViewMode => "view-mode",
            Command::Fullscreen => "fullscreen",
            Command::TogglePanel => "side-panel",
            Command::ToggleTools => "tools-pane",
            Command::ShowLayers => "layers",
            Command::ShowAttachments => "attachments",
            Command::AddAttachment => "attach-file",
            Command::GoToPage => "go-to-page",
            Command::ToggleTheme => "toggle-theme",
            Command::Search => "search",
            Command::FindNext => "find-next",
            Command::FindPrevious => "find-previous",
            Command::Replace => "replace",
            Command::Copy => "copy",
            Command::SelectAll => "select-all",
            Command::RotateRight => "rotate-right",
            Command::RotateLeft => "rotate-left",
            Command::DeletePage => "delete-page",
            Command::InsertPages => "insert-pages",
            Command::DuplicatePage => "duplicate-page",
            Command::ExtractPage => "extract-page",
            Command::Undo => "undo",
            Command::Redo => "redo",
            Command::EditText => "edit-text",
            Command::EditPdf => "edit-pdf",
            Command::AddTextBox => "add-text",
            Command::Highlight => "highlight",
            Command::HighlightTool => "highlight-tool",
            Command::NoteTool => "note-tool",
            Command::RedactTool => "redact-tool",
            Command::Note => "note",
            Command::CheckUpdates => "check-updates",
            Command::EditObjects => "edit-objects",
            Command::FillSign => "fill-sign",
            Command::MarkRedaction => "mark-redaction",
            Command::ApplyRedactions => "apply-redactions",
            Command::Protect => "protect",
            Command::Unprotect => "unprotect",
            Command::Properties => "properties",
            Command::CloseOtherTabs => "close-other-tabs",
            Command::CopyPath => "copy-path",
            Command::RevealInFolder => "reveal-in-folder",
            Command::ForgetRecent => "forget-recent",
        }
    }

    /// Commande d'une clé enregistrée ; `None` pour une clé inconnue, celle
    /// d'une version future par exemple, qui est alors simplement ignorée.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Command> {
        ENTRIES.iter().map(|e| e.command).find(|c| c.key() == key)
    }
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

/// Un texte d'interface dans la langue demandée. La palette lit la langue
/// une fois, à son ouverture, plutôt que l'état global à chaque ligne : le
/// filtrage et le dessin voient ainsi toujours les mêmes libellés.
fn in_lang(french: &'static str, english: bool) -> &'static str {
    if english {
        to_english(french)
    } else {
        french
    }
}

/// Le raccourci tel qu'on l'écrit dans la langue demandée. Les noms de
/// touches ne passent pas par la table de traduction : « Fin » y est déjà
/// l'épaisseur d'un trait (« Thin »), pas la touche (« End »).
fn shortcut_in_lang(shortcut: &'static str, english: bool) -> &'static str {
    if !english {
        return shortcut;
    }
    match shortcut {
        "Ctrl+Maj+S" => "Ctrl+Shift+S",
        "Ctrl+Maj+E" => "Ctrl+Shift+E",
        "Ctrl+Maj+Tab" => "Ctrl+Shift+Tab",
        "Ctrl+Maj+Plus" => "Ctrl+Shift+Plus",
        "Ctrl+Maj+Moins" => "Ctrl+Shift+Minus",
        "Maj+R" => "Shift+R",
        "Maj+F3" => "Shift+F3",
        "Maj+F4" => "Shift+F4",
        "Maj+M" => "Shift+M",
        "Ctrl+Suppr" => "Ctrl+Del",
        "Ctrl+Page préc." => "Ctrl+Page Up",
        "Ctrl+Page suiv." => "Ctrl+Page Down",
        "Origine" => "Home",
        "Fin" => "End",
        other => other,
    }
}

/// Libellé et raccourci d'une commande. La palette est la source unique de
/// ces textes : les info-bulles de la barre d'outils les reprennent, ce qui
/// évite qu'un raccourci change ici sans changer là.
#[must_use]
pub fn describe(command: Command) -> Option<(&'static str, &'static str)> {
    let english = english();
    ENTRIES.iter().find(|e| e.command == command).map(|e| {
        (
            in_lang(e.label, english),
            shortcut_in_lang(e.shortcut, english),
        )
    })
}

/// Toutes les commandes, dans l'ordre d'affichage quand rien n'est filtré.
///
/// C'est l'ordre d'Acrobat et de l'usage, du plus employé au plus rare :
/// ouvrir, enregistrer, rechercher, imprimer, remplir et signer, modifier,
/// commenter ; puis la navigation et l'affichage ; puis les pages, la
/// biffure et la protection ; enfin les réglages. Une commande de tous les
/// jours ne doit pas se chercher sous « Retirer la protection ».
///
/// « Paramètres » reste la **dernière** : le scénario invisible l'atteint par
/// Fin puis Entrée, ce qui est sans effet sur le document (la commande
/// voisine, « Rechercher les mises à jour », irait sur le réseau).
const ENTRIES: &[Entry] = &[
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
        label: "Rechercher dans le document",
        shortcut: "Ctrl+F",
        keywords: "trouver chercher recherche texte occurrences",
        command: Command::Search,
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
        label: "Remplir et signer",
        shortcut: "S",
        keywords: "signature signer parapher initiales coche croix formulaire manuscrite",
        command: Command::FillSign,
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
        label: "Poser une note",
        shortcut: "N",
        keywords: "commentaire annotation bulle remarque",
        command: Command::Note,
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
        label: "Ajouter du texte",
        shortcut: "",
        keywords: "zone texte ajouter ecrire inserer boite",
        command: Command::AddTextBox,
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
        label: "Exporter (page web, Word, Excel, images, texte)",
        shortcut: "Ctrl+E",
        keywords: "conversion convertir html docx xlsx png jpeg markdown texte",
        command: Command::Export,
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
        label: "Occurrence suivante",
        shortcut: "F3",
        keywords: "recherche suivant trouver encore prochaine",
        command: Command::FindNext,
        needs_document: true,
    },
    Entry {
        label: "Occurrence précédente",
        shortcut: "Maj+F3",
        keywords: "recherche precedent trouver arriere",
        command: Command::FindPrevious,
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
        label: "Annuler la dernière action",
        shortcut: "Ctrl+Z",
        keywords: "revenir defaire annulation historique",
        command: Command::Undo,
        needs_document: true,
    },
    Entry {
        label: "Rétablir l'action annulée",
        shortcut: "Ctrl+Y",
        keywords: "refaire retablissement historique",
        command: Command::Redo,
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
        label: "Modifier le texte sélectionné",
        shortcut: "E",
        keywords: "corriger remplacer retoucher saisir mot phrase",
        command: Command::EditText,
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
        label: "Modifier : objets de la page",
        shortcut: "O",
        keywords: "image deplacer redimensionner recadrer ordre supprimer objet dessin logo",
        command: Command::EditObjects,
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
        label: "Choisir le niveau de zoom…",
        shortcut: "",
        keywords: "zoom pourcentage pourcent niveau taille liste agrandir reduire loupe",
        command: Command::ZoomMenu,
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
        label: "Ajuster à la largeur",
        shortcut: "F",
        keywords: "adapter adaptation pleine largeur",
        command: Command::FitWidth,
        needs_document: true,
    },
    Entry {
        label: "Ajuster à la page",
        shortcut: "",
        keywords: "page entiere afficher hauteur tout voir fenetre",
        command: Command::FitPage,
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
        label: "Vue précédente",
        shortcut: "Alt+←",
        keywords: "retour arriere historique revenir back lien signet",
        command: Command::ViewBack,
        needs_document: true,
    },
    Entry {
        label: "Vue suivante",
        shortcut: "Alt+→",
        keywords: "avancer historique forward refaire lien signet",
        command: Command::ViewForward,
        needs_document: true,
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
        shortcut: "Maj+F4",
        keywords: "outils colonne droite modifier signer biffer",
        command: Command::ToggleTools,
        needs_document: false,
    },
    Entry {
        label: "Plein écran",
        shortcut: "F11",
        keywords: "presentation ecran entier diaporama",
        command: Command::Fullscreen,
        needs_document: false,
    },
    Entry {
        label: "Onglet suivant",
        shortcut: "Ctrl+Tab",
        keywords: "changer basculer document",
        command: Command::NextTab,
        needs_document: true,
    },
    Entry {
        label: "Onglet précédent",
        shortcut: "Ctrl+Maj+Tab",
        keywords: "changer basculer document",
        command: Command::PrevTab,
        needs_document: true,
    },
    Entry {
        label: "Fermer l'onglet",
        shortcut: "Ctrl+W, Ctrl+F4",
        keywords: "quitter fermeture document",
        command: Command::CloseTab,
        needs_document: true,
    },
    Entry {
        label: "Fermer les autres onglets",
        shortcut: "",
        keywords: "quitter fermeture documents garder seul",
        command: Command::CloseOtherTabs,
        needs_document: true,
    },
    Entry {
        label: "Copier le chemin du fichier",
        shortcut: "",
        keywords: "emplacement adresse chemin presse papiers",
        command: Command::CopyPath,
        needs_document: true,
    },
    Entry {
        label: "Ouvrir le dossier du fichier",
        shortcut: "",
        keywords: "explorateur emplacement montrer afficher repertoire",
        command: Command::RevealInFolder,
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
    // Après les deux précédentes : « rotation » doit d'abord proposer de
    // pivoter la page, ce que l'on cherche le plus souvent.
    Entry {
        label: "Faire pivoter la vue à droite",
        shortcut: "Ctrl+Maj+Plus",
        keywords: "affichage vue temporaire horaire tourner lecture couche",
        command: Command::RotateViewRight,
        needs_document: true,
    },
    Entry {
        label: "Faire pivoter la vue à gauche",
        shortcut: "Ctrl+Maj+Moins",
        keywords: "affichage vue temporaire antihoraire tourner lecture couche",
        command: Command::RotateViewLeft,
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
        label: "Supprimer la page",
        shortcut: "Ctrl+Suppr",
        keywords: "effacer retirer enlever suppression",
        command: Command::DeletePage,
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
        label: "Outil biffure",
        shortcut: "",
        keywords: "biffer caviarder masquer noircir outil",
        command: Command::RedactTool,
        needs_document: true,
    },
    Entry {
        label: "Biffure : appliquer définitivement",
        shortcut: "Maj+M",
        keywords: "caviarder censurer expurger definitif confidentiel",
        command: Command::ApplyRedactions,
        needs_document: true,
    },
    Entry {
        label: "Propriétés du document",
        shortcut: "Ctrl+D",
        keywords: "proprietes infos informations metadonnees auteur titre version taille",
        command: Command::Properties,
        needs_document: true,
    },
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
        label: "Calques (afficher ou masquer le contenu optionnel)",
        shortcut: "",
        keywords: "couches contenu optionnel ocg plan visibilite",
        command: Command::ShowLayers,
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
        label: "Accueil",
        shortcut: "",
        keywords: "accueil maison récents démarrage base",
        command: Command::Home,
        needs_document: false,
    },
    Entry {
        label: "Rechercher les mises à jour",
        shortcut: "",
        keywords: "version nouvelle telecharger installer maj update",
        command: Command::CheckUpdates,
        needs_document: false,
    },
    Entry {
        label: "Paramètres",
        shortcut: "",
        keywords: "paramètres réglages langue settings options",
        command: Command::Settings,
        needs_document: false,
    },
];

/// Lignes affichées au plus, quand la fenêtre est assez haute.
pub const MAX_ROWS: usize = 12;

/// Rectangle `(x, y, largeur, hauteur)`.
type Rect = (i32, i32, i32, i32);

fn inside(r: Rect, x: i32, y: i32) -> bool {
    x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
}

/// Plie un caractère pour la comparaison : minuscule, sans accent.
///
/// Un caractère donne toujours **un** caractère — `to_lowercase` peut en
/// rendre plusieurs (« İ »), on garde le premier : le rang d'une lettre
/// trouvée dans le texte plié reste ainsi celui de la lettre affichée, et
/// c'est lui que le dessin met en évidence. Les menus contextuels s'en
/// servent aussi, pour choisir un élément à son initiale.
pub(crate) fn fold_char(c: char) -> char {
    match c.to_lowercase().next().unwrap_or(c) {
        'à' | 'á' | 'â' | 'ä' | 'ã' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        other => other,
    }
}

/// Cherche une requête, déjà pliée, dans un texte. Rend le score — plus il
/// est petit, mieux c'est — et les rangs, en caractères, des lettres
/// trouvées ; `None` si la requête ne correspond pas du tout.
///
/// Une correspondance d'un seul tenant vaut sa position (plus elle est tôt,
/// mieux c'est) ; sinon, toutes les lettres dans l'ordre (la sous-séquence)
/// valent 500 plus l'étendue parcourue, pour rester derrière.
fn find(text: &str, query: &[char]) -> Option<(usize, Vec<usize>)> {
    let hay: Vec<char> = text.chars().map(fold_char).collect();
    let n = query.len();
    if let Some(at) = run_at(&hay, query) {
        return Some((at, (at..at + n).collect()));
    }
    let mut marks = Vec::with_capacity(n);
    let mut from = 0;
    for &want in query {
        let at = from + hay.get(from..)?.iter().position(|&c| c == want)?;
        marks.push(at);
        from = at + 1;
    }
    Some((500 + from, marks))
}

/// Position de la requête, déjà pliée, d'un seul tenant dans un texte plié.
fn run_at(hay: &[char], query: &[char]) -> Option<usize> {
    if query.is_empty() {
        return Some(0);
    }
    hay.windows(query.len()).position(|w| w == query)
}

/// Position de la requête d'un seul tenant dans un texte, sans sous-séquence.
fn find_whole(text: &str, query: &[char]) -> Option<usize> {
    let hay: Vec<char> = text.chars().map(fold_char).collect();
    run_at(&hay, query)
}

/// Score d'une entrée et lettres à mettre en évidence : le libellé affiché
/// d'abord ; en anglais, le libellé français ensuite (on peut avoir appris
/// les commandes dans l'autre langue) ; les synonymes enfin. Ces deux
/// derniers sont pénalisés, pour qu'une correspondance dans ce qu'on lit
/// reste toujours devant, et ne marquent rien : les lettres trouvées ne
/// sont pas à l'écran.
///
/// Ce qu'on ne voit pas ne se trouve que **d'un seul tenant** : un synonyme
/// est un mot que l'on tape en entier (« rotation », « caviarder »). En
/// sous-séquence, la longue liste des synonymes répondait à presque tout —
/// « enrs » faisait monter « Accueil », dont les synonymes « accueil maison
/// récents » contiennent un e, un n, un r et un s dans cet ordre — et la
/// palette montrait des lignes sans que rien, à l'écran, dise pourquoi.
fn entry_score(entry: &Entry, query: &[char], english: bool) -> Option<(usize, Vec<usize>)> {
    if let Some(hit) = find(in_lang(entry.label, english), query) {
        return Some(hit);
    }
    if english {
        if let Some(at) = find_whole(entry.label, query) {
            return Some((at + 1000, Vec::new()));
        }
    }
    find_whole(entry.keywords, query).map(|at| (at + 2000, Vec::new()))
}

/// Une ligne de la liste filtrée.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hit {
    /// Indice dans [`ENTRIES`].
    entry: usize,
    /// Rangs, en caractères du libellé affiché, des lettres trouvées.
    marks: Vec<usize>,
    /// Commande récente, placée en tête de la liste sans filtre.
    recent: bool,
}

/// Où tombe l'appui de la souris.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteDown {
    /// Sur une ligne : elle s'enfonce, et s'exécutera au relâchement.
    Row,
    /// Dans la carte, hors des lignes (le champ, l'ascenseur) : la palette
    /// reste.
    Inside,
    /// Dehors : la palette se ferme, sans rien exécuter.
    Outside,
}

/// Palette de commandes.
pub struct Palette {
    /// Champ de filtrage.
    pub input: TextInput,
    /// Lignes filtrées et triées.
    filtered: Vec<Hit>,
    /// Ligne sélectionnée dans `filtered`. Elle est toujours visible :
    /// `first <= selected < first + visible()`.
    selected: usize,
    /// Première ligne visible dans `filtered`.
    first: usize,
    /// Lignes que la carte peut montrer, d'après le dernier dessin (la
    /// fenêtre peut être basse) ; [`MAX_ROWS`] avant le premier, jamais 0.
    capacity: usize,
    /// Rectangles des lignes dessinées ; le premier est la ligne `first`.
    rows: Vec<Rect>,
    /// Rectangle de la carte au dernier dessin.
    card: Rect,
    /// Piste de l'ascenseur au dernier dessin, quand la liste déborde.
    track: Option<Rect>,
    /// Hauteur du pouce de l'ascenseur au dernier dessin.
    thumb: i32,
    /// Ligne enfoncée, qui attend le relâchement pour s'exécuter.
    pressed: Option<usize>,
    /// Le pouce de l'ascenseur est tenu : la liste suit le pointeur.
    scrubbing: bool,
    /// Un document est ouvert.
    has_document: bool,
    /// Commandes qui n'ont rien à faire maintenant (« Vue précédente » au
    /// début de l'historique) : la palette ne les propose pas, comme elle
    /// tait celles qui demandent un document quand il n'y en a pas.
    hidden: Vec<Command>,
    /// Langue lue à l'ouverture : celle des libellés, du filtrage et du
    /// dessin.
    english: bool,
    /// Commandes récentes, de la plus récente à la plus ancienne.
    recent: Vec<Command>,
    /// Nombre de lignes sans filtre : c'est sur lui que la carte se pose,
    /// pour que le champ ne saute pas à chaque frappe.
    full_len: usize,
    /// Fraction de ligne que la molette n'a pas encore fait défiler. Un pavé
    /// tactile envoie de petits crans (un dixième, un quart) : arrondis un à
    /// un, ils ne faisaient rien — un geste lent laissait la liste immobile.
    wheel_rest: f32,
}

impl Palette {
    /// Palette ouverte, filtre vide ; les commandes `recent` (de la plus
    /// récente à la plus ancienne) viennent en tête.
    #[must_use]
    pub fn new(has_document: bool, recent: &[Command]) -> Self {
        Self::with_lang(has_document, recent, english())
    }

    /// Comme [`Palette::new`], dans une langue donnée : les épreuves ne
    /// dépendent pas de l'état global, que d'autres changent en parallèle.
    fn with_lang(has_document: bool, recent: &[Command], english: bool) -> Self {
        let mut unique: Vec<Command> = Vec::with_capacity(recent.len());
        for &command in recent {
            if !unique.contains(&command) {
                unique.push(command);
            }
        }
        let full_len = ENTRIES
            .iter()
            .filter(|e| has_document || !e.needs_document)
            .count();
        let mut p = Self {
            input: TextInput::new(in_lang("Tapez une commande", english)),
            filtered: Vec::new(),
            selected: 0,
            first: 0,
            capacity: MAX_ROWS,
            rows: Vec::new(),
            card: (0, 0, 0, 0),
            track: None,
            thumb: 0,
            pressed: None,
            scrubbing: false,
            has_document,
            hidden: Vec::new(),
            english,
            recent: unique,
            full_len,
            wheel_rest: 0.0,
        };
        p.refilter();
        p
    }

    /// La même palette, sans les commandes `hidden`, qui n'auraient rien à
    /// faire : une ligne qui ne répond que « Aucune vue précédente » n'a pas
    /// sa place dans la liste.
    #[must_use]
    pub fn without(mut self, hidden: &[Command]) -> Self {
        self.hidden = hidden.to_vec();
        self.full_len = ENTRIES.iter().filter(|e| self.allowed(e)).count();
        self.refilter();
        self
    }

    /// La commande `e` peut être proposée.
    fn allowed(&self, e: &Entry) -> bool {
        (self.has_document || !e.needs_document) && !self.hidden.contains(&e.command)
    }

    /// Recalcule la liste filtrée ; la sélection revient en tête.
    fn refilter(&mut self) {
        let query: Vec<char> = self.input.value.chars().map(fold_char).collect();
        let allowed = |e: &Entry| self.allowed(e);
        let mut hits = Vec::with_capacity(ENTRIES.len());
        if query.is_empty() {
            // Les récentes d'abord, dans leur ordre, puis tout le reste dans
            // l'ordre d'usage — sans les répéter.
            for &command in &self.recent {
                if let Some((entry, _)) = ENTRIES
                    .iter()
                    .enumerate()
                    .find(|(_, e)| e.command == command && allowed(e))
                {
                    hits.push(Hit {
                        entry,
                        marks: Vec::new(),
                        recent: true,
                    });
                }
            }
            for (entry, e) in ENTRIES.iter().enumerate() {
                if allowed(e) && !hits.iter().any(|h| h.entry == entry) {
                    hits.push(Hit {
                        entry,
                        marks: Vec::new(),
                        recent: false,
                    });
                }
            }
        } else {
            // Le score décide ; à score égal, la plus récente passe devant,
            // puis l'ordre d'usage.
            let mut scored: Vec<(usize, usize, usize, Vec<usize>)> = ENTRIES
                .iter()
                .enumerate()
                .filter(|(_, e)| allowed(e))
                .filter_map(|(entry, e)| {
                    let (score, marks) = entry_score(e, &query, self.english)?;
                    let rank = self
                        .recent
                        .iter()
                        .position(|&c| c == e.command)
                        .unwrap_or(usize::MAX);
                    Some((score, rank, entry, marks))
                })
                .collect();
            scored.sort_by_key(|&(score, rank, entry, _)| (score, rank, entry));
            hits.extend(scored.into_iter().map(|(_, _, entry, marks)| Hit {
                entry,
                marks,
                recent: false,
            }));
        }
        self.filtered = hits;
        self.selected = 0;
        self.first = 0;
    }

    /// Commande d'une ligne de la liste filtrée.
    fn command_at(&self, index: usize) -> Option<Command> {
        self.filtered
            .get(index)
            .and_then(|h| ENTRIES.get(h.entry))
            .map(|e| e.command)
    }

    /// Commande actuellement sélectionnée.
    #[must_use]
    pub fn current(&self) -> Option<Command> {
        self.command_at(self.selected)
    }

    /// Lignes visibles à la fois : au plus la capacité de la carte, au plus
    /// la liste, jamais zéro (une page vaut au moins une ligne).
    fn visible(&self) -> usize {
        self.filtered.len().min(self.capacity).max(1)
    }

    /// Fait défiler la liste juste assez pour montrer la sélection.
    fn reveal(&mut self) {
        let visible = self.visible();
        if self.selected < self.first {
            self.first = self.selected;
        } else if self.selected >= self.first + visible {
            self.first = self.selected + 1 - visible;
        }
        self.first = self.first.min(self.filtered.len().saturating_sub(visible));
    }

    /// L'inverse, après la molette : la liste a défilé, la sélection la
    /// suit pour rester parmi les lignes visibles.
    fn follow(&mut self) {
        let Some(last) = self.filtered.len().checked_sub(1) else {
            return;
        };
        let bottom = (self.first + self.visible() - 1).min(last);
        self.selected = self.selected.clamp(self.first.min(last), bottom);
    }

    /// Sélectionne une ligne (bornée à la liste) et la montre.
    fn select(&mut self, index: usize) {
        let Some(last) = self.filtered.len().checked_sub(1) else {
            return;
        };
        self.selected = index.min(last);
        self.reveal();
    }

    /// Touche ; `Some(commande)` quand l'utilisateur valide, et vrai quand
    /// la palette doit se fermer.
    ///
    /// Origine et Fin vont à la liste quand le champ est vide ; sinon au
    /// curseur du champ, comme dans tout champ de saisie — on corrige sa
    /// requête sans perdre sa place. Ctrl+Origine et Ctrl+Fin vont toujours
    /// à la liste.
    pub fn key(&mut self, key: Key, m: Modifiers) -> (Option<Command>, bool) {
        let page = self.visible().saturating_sub(1).max(1);
        let to_list = m.ctrl || self.input.value.is_empty();
        match key {
            Key::Up => self.select(self.selected.saturating_sub(1)),
            Key::Down => self.select(self.selected + 1),
            Key::PageUp => self.select(self.selected.saturating_sub(page)),
            Key::PageDown => self.select(self.selected + page),
            Key::Home if to_list => self.select(0),
            Key::End if to_list => self.select(usize::MAX),
            Key::Tab => {
                let n = self.filtered.len();
                if n > 0 {
                    let next = if m.shift {
                        (self.selected + n - 1) % n
                    } else {
                        (self.selected + 1) % n
                    };
                    self.select(next);
                }
            }
            other => {
                return match self.input.key(other, m.shift) {
                    InputAction::Submit => (self.current(), true),
                    InputAction::Cancel => (None, true),
                    InputAction::Changed => {
                        self.refilter();
                        (None, false)
                    }
                    InputAction::None => (None, false),
                }
            }
        }
        (None, false)
    }

    /// Caractère saisi.
    pub fn char(&mut self, c: char) {
        if self.input.insert_char(c) == InputAction::Changed {
            self.refilter();
        }
    }

    /// Texte collé dans le champ (Ctrl+V).
    pub fn paste(&mut self, text: &str) {
        if self.input.paste(text) == InputAction::Changed {
            self.refilter();
        }
    }

    /// Molette : la liste défile de trois lignes par cran, la sélection
    /// suit. Pas d'animation — rien à réveiller, la fin de l'événement
    /// repeint.
    ///
    /// Les fractions de cran s'additionnent jusqu'à faire une ligne ; un
    /// changement de sens repart de zéro, pour que la liste réponde aussitôt.
    pub fn wheel(&mut self, delta: f32) {
        if !delta.is_finite() {
            return;
        }
        if self.wheel_rest * delta < 0.0 {
            self.wheel_rest = 0.0;
        }
        self.wheel_rest += delta * 3.0;
        let lines = self.wheel_rest.trunc();
        self.wheel_rest -= lines;
        let max = self.filtered.len().saturating_sub(self.visible()) as i64;
        self.first = (self.first as i64 - lines as i64).clamp(0, max) as usize;
        self.follow();
    }

    /// Place la liste d'après la hauteur du pointeur sur la piste de
    /// l'ascenseur, le pouce centré sous lui.
    fn scrub(&mut self, y: i32) {
        let Some((_, top, _, height)) = self.track else {
            return;
        };
        let span = (height - self.thumb).max(1);
        let at = (y - top - self.thumb / 2).clamp(0, span);
        let max = self.filtered.len().saturating_sub(self.visible());
        self.first = ((at as f32 / span as f32) * max as f32).round() as usize;
        self.first = self.first.min(max);
        self.follow();
    }

    /// Ligne de la liste filtrée sous un point, d'après le dernier dessin.
    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        self.rows
            .iter()
            .position(|&r| inside(r, x, y))
            .map(|i| self.first + i)
            .filter(|&index| index < self.filtered.len())
    }

    /// Appui : une ligne s'enfonce (et se sélectionne), mais rien ne
    /// s'exécute encore — comme un bouton, une commande part au relâchement,
    /// et l'on se ravise en glissant hors de la ligne. L'ascenseur se saisit
    /// n'importe où sur sa piste.
    pub fn mouse_down(&mut self, x: i32, y: i32) -> PaletteDown {
        self.pressed = None;
        self.scrubbing = false;
        if self.track.is_some_and(|t| inside(t, x, y)) {
            self.scrubbing = true;
            self.scrub(y);
            return PaletteDown::Inside;
        }
        if let Some(index) = self.row_at(x, y) {
            self.pressed = Some(index);
            self.selected = index;
            return PaletteDown::Row;
        }
        if inside(self.card, x, y) {
            PaletteDown::Inside
        } else {
            PaletteDown::Outside
        }
    }

    /// Relâchement : la commande de la ligne enfoncée, si le pointeur est
    /// resté dessus.
    pub fn mouse_up(&mut self, x: i32, y: i32) -> Option<Command> {
        self.scrubbing = false;
        let pressed = self.pressed.take()?;
        if self.row_at(x, y) != Some(pressed) {
            return None;
        }
        self.command_at(pressed)
    }

    /// Mouvement : le pouce tenu fait défiler, sinon le survol sélectionne
    /// la ligne sous le pointeur. Vrai si l'aspect a changé.
    pub fn mouse_move(&mut self, x: i32, y: i32, dragging: bool) -> bool {
        if self.scrubbing && dragging {
            let before = (self.first, self.selected);
            self.scrub(y);
            return before != (self.first, self.selected);
        }
        self.scrubbing = false;
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
        let s = |v: f32| (v * dpi) as i32;
        let size = t.font_size * dpi;
        let pad = s(14.0);
        let row = ((size * 2.3) as i32).max(1);
        let field = s(32.0);
        let gap = s(12.0);
        let chrome = pad * 2 + field + gap;
        let width = (560.0 * dpi).min(f32::from(frame.width as u16) * 0.9) as i32;
        // Autant de lignes que la fenêtre en tient, marges comprises : dans
        // une fenêtre basse, la carte ne déborde pas, elle défile plus tôt.
        let room = ((frame.height as i32 - 2 * pad - chrome) / row).max(1) as usize;
        self.capacity = MAX_ROWS.min(room);
        self.reveal();
        let len = self.filtered.len();
        let visible = self.visible();
        // La carte se pose d'après sa hauteur sans filtre : elle raccourcit
        // par le bas quand la liste se réduit, le champ ne bouge pas.
        let full = self.full_len.min(self.capacity).max(1) as i32;
        let x = (frame.width as i32 - width) / 2;
        let y = ((frame.height as i32 - (chrome + full * row)) / 3).max(pad);
        let height = chrome + visible as i32 * row;
        // Une carte posée sur le voile : ombre, coins arrondis, liseré.
        let radius = 14.0 * dpi;
        shadow(
            frame,
            x,
            y + s(8.0),
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
        // Un trait sépare le champ des commandes.
        frame.fill_rect(
            x + pad,
            y + pad + field + s(6.0),
            width - 2 * pad,
            1,
            t.separator.0,
            t.separator.1,
            t.separator.2,
        );
        let top = y + pad + field + gap;
        // La colonne de l'ascenseur, quand la liste déborde : les lignes et
        // les raccourcis s'arrêtent avant elle.
        let bar = if len > visible { s(12.0) } else { 0 };
        self.rows.clear();
        let mut ry = top;
        for (index, hit) in self
            .filtered
            .iter()
            .enumerate()
            .skip(self.first)
            .take(visible)
        {
            let Some(e) = ENTRIES.get(hit.entry) else {
                continue;
            };
            let chosen = index == self.selected;
            if chosen {
                round_rect(frame, x + 6, ry, width - 12 - bar, row, 8.0, t.hover);
            }
            let baseline = ry as f32 + f32::midpoint(row as f32, text.ascent(size)) - 1.0;
            let color = if chosen { t.text } else { t.text_dim };
            text.draw_marked(
                frame,
                (x + pad) as f32,
                baseline,
                size,
                in_lang(e.label, self.english),
                color,
                t.accent,
                &hit.marks,
                (width - 2 * pad - bar - s(110.0)) as f32,
            );
            let shortcut = shortcut_in_lang(e.shortcut, self.english);
            if !shortcut.is_empty() {
                let w = text.measure(size * 0.92, shortcut);
                text.draw(
                    frame,
                    (x + width - pad - bar) as f32 - w,
                    baseline,
                    size * 0.92,
                    shortcut,
                    t.text_dim,
                );
            }
            // Un trait sous la dernière commande récente : ce qui suit est
            // l'ordre habituel. Seulement un trait — aucune ligne à sauter.
            let next_is_usual = self.filtered.get(index + 1).is_some_and(|h| !h.recent);
            if hit.recent && next_is_usual && index + 1 < self.first + visible {
                frame.fill_rect(
                    x + pad,
                    ry + row,
                    width - 2 * pad - bar,
                    1,
                    t.separator.0,
                    t.separator.1,
                    t.separator.2,
                );
            }
            self.rows.push((x + 4, ry, width - 8 - bar, row));
            ry += row;
        }
        // L'ascenseur, fin comme celui de la liste des polices : il dit où
        // l'on est dans la liste, et se saisit pour la parcourir.
        self.track = None;
        if len > visible {
            let track = visible as i32 * row - s(8.0);
            let thumb = (track * visible as i32 / len as i32)
                .max(s(28.0))
                .min(track);
            let max_first = (len - visible) as f32;
            let thumb_top =
                top + s(4.0) + ((track - thumb) as f32 * self.first as f32 / max_first) as i32;
            round_rect_alpha(
                frame,
                x + width - s(10.0),
                thumb_top,
                s(4.0),
                thumb,
                2.0 * dpi,
                t.text_dim,
                0.55,
            );
            self.track = Some((x + width - bar - 2, top + s(4.0), bar + 2, track));
            self.thumb = thumb;
        }
        if self.filtered.is_empty() {
            let baseline = top as f32 + f32::midpoint(row as f32, text.ascent(size)) - 1.0;
            text.draw(
                frame,
                (x + pad) as f32,
                baseline,
                size,
                in_lang("Aucune commande", self.english),
                t.text_dim,
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // tests : l'absence de résultat est l'échec cherché
mod tests {
    use super::*;

    /// Palette en français avec un document, sans commande récente : aucune
    /// épreuve ne dépend de la langue globale, que celles de `lang` changent
    /// pendant qu'elles tournent.
    fn palette() -> Palette {
        Palette::with_lang(true, &[], false)
    }

    fn typed(mut p: Palette, query: &str) -> Palette {
        for c in query.chars() {
            p.char(c);
        }
        p
    }

    /// Tape une requête dans une palette neuve et renvoie la commande en tête.
    fn top(query: &str) -> Option<Command> {
        typed(palette(), query).current()
    }

    /// Commandes de la liste filtrée, dans l'ordre.
    fn commands(p: &Palette) -> Vec<Command> {
        (0..p.filtered.len())
            .filter_map(|i| p.command_at(i))
            .collect()
    }

    /// Lettres marquées pour une commande donnée.
    fn marks_of(p: &Palette, command: Command) -> Vec<usize> {
        p.filtered
            .iter()
            .find(|h| ENTRIES.get(h.entry).map(|e| e.command) == Some(command))
            .map(|h| h.marks.clone())
            .expect("la commande est dans la liste")
    }

    fn plain() -> Modifiers {
        Modifiers::default()
    }

    fn ctrl() -> Modifiers {
        Modifiers {
            ctrl: true,
            ..Modifiers::default()
        }
    }

    /// Vrai si la sélection est parmi les lignes dessinées.
    fn selection_visible(p: &Palette) -> bool {
        p.selected >= p.first && p.selected < p.first + p.visible()
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
        let mut p = palette();
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
            Command::MarkRedaction | Command::ApplyRedactions | Command::RedactTool
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
        let p = Palette::with_lang(false, &[], false);
        let labels: Vec<&str> = p
            .filtered
            .iter()
            .filter_map(|h| ENTRIES.get(h.entry))
            .map(|e| e.label)
            .collect();
        assert!(labels.contains(&"Ouvrir un document"));
        assert!(!labels.iter().any(|l| l.contains("Enregistrer")));
    }

    #[test]
    fn default_order_follows_usage() {
        let p = palette();
        let list = commands(&p);
        assert_eq!(
            list.get(..7),
            Some(
                &[
                    Command::Open,
                    Command::Save,
                    Command::Search,
                    Command::Print,
                    Command::FillSign,
                    Command::EditPdf,
                    Command::Note,
                ][..]
            )
        );
        // « Paramètres » en dernier : le scénario invisible y va par Fin.
        assert_eq!(list.last(), Some(&Command::Settings));
    }

    #[test]
    fn every_entry_has_a_unique_key_that_round_trips() {
        let mut keys: Vec<&str> = ENTRIES.iter().map(|e| e.command.key()).collect();
        for e in ENTRIES {
            assert_eq!(Command::from_key(e.command.key()), Some(e.command));
        }
        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), count, "deux commandes partagent une clé");
        assert_eq!(Command::from_key("une-commande-de-demain"), None);
        // Et chaque commande n'a qu'une ligne.
        let mut all: Vec<&str> = ENTRIES.iter().map(|e| e.label).collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), ENTRIES.len());
    }

    #[test]
    fn every_label_and_shortcut_reads_in_english() {
        for e in ENTRIES {
            assert_ne!(
                to_english(e.label),
                e.label,
                "« {} » n'a pas de traduction",
                e.label
            );
            let shortcut = shortcut_in_lang(e.shortcut, true);
            for french in ["Maj", "Suppr", "préc", "suiv", "Origine", "Fin"] {
                assert!(
                    !shortcut.contains(french),
                    "raccourci « {shortcut} » resté en français"
                );
            }
        }
        assert_eq!(
            to_english("Tapez une commande"),
            "Type a command",
            "le texte d'invite est traduit"
        );
        assert_ne!(to_english("Aucune commande"), "Aucune commande");
    }

    #[test]
    fn recent_commands_come_first_without_duplicates() {
        let p = Palette::with_lang(true, &[Command::ToggleTheme, Command::Print], false);
        let list = commands(&p);
        assert_eq!(
            list.get(..3),
            Some(&[Command::ToggleTheme, Command::Print, Command::Open][..])
        );
        assert_eq!(list.iter().filter(|&&c| c == Command::Print).count(), 1);
        assert_eq!(list.len(), ENTRIES.len(), "rien d'ajouté, rien de perdu");
        assert!(p.filtered.first().is_some_and(|h| h.recent));
        assert!(p.filtered.get(2).is_some_and(|h| !h.recent));
        // Une même commande donnée deux fois ne compte qu'une fois.
        let twice = Palette::with_lang(true, &[Command::Print, Command::Print], false);
        assert_eq!(commands(&twice).len(), ENTRIES.len());
    }

    #[test]
    fn recent_commands_needing_a_document_are_hidden_without_one() {
        let p = Palette::with_lang(false, &[Command::Save, Command::ToggleTheme], false);
        let list = commands(&p);
        assert!(!list.contains(&Command::Save));
        assert_eq!(list.first(), Some(&Command::ToggleTheme));
    }

    #[test]
    fn recency_only_breaks_ties_when_filtering() {
        let without = commands(&typed(palette(), "page"));
        let with = commands(&typed(
            Palette::with_lang(true, &[Command::LastPage], false),
            "page",
        ));
        let pos = |list: &[Command], c: Command| list.iter().position(|&x| x == c);
        // Sans récence, « Première page » précède « Dernière page » (même
        // score, ordre d'usage) ; récente, « Dernière page » passe devant…
        assert!(pos(&without, Command::FirstPage) < pos(&without, Command::LastPage));
        assert!(pos(&with, Command::LastPage) < pos(&with, Command::FirstPage));
        // … mais jamais devant « Page précédente », au meilleur score.
        assert!(pos(&with, Command::PrevPage) < pos(&with, Command::LastPage));
        assert!(pos(&with, Command::NextPage) < pos(&with, Command::LastPage));
    }

    #[test]
    fn navigation_and_validation() {
        let mut p = palette();
        let first = p.current();
        let (cmd, close) = p.key(Key::Down, plain());
        assert!(cmd.is_none() && !close);
        assert_ne!(p.current(), first, "la sélection a bougé");
        let (cmd, close) = p.key(Key::Up, plain());
        assert!(cmd.is_none() && !close);
        assert_eq!(p.current(), first);
        // Entrée valide, Échap ferme sans rien faire.
        let (cmd, close) = p.key(Key::Enter, plain());
        assert_eq!((cmd, close), (first, true));
        let (cmd, close) = p.key(Key::Escape, plain());
        assert_eq!((cmd, close), (None, true));
    }

    #[test]
    fn selection_stays_visible() {
        let mut p = palette();
        p.capacity = 5;
        for _ in 0..7 {
            p.key(Key::Down, plain());
        }
        assert_eq!((p.selected, p.first), (7, 3));
        for _ in 0..7 {
            p.key(Key::Up, plain());
        }
        assert_eq!((p.selected, p.first), (0, 0));
        // Maj+Tab depuis la première ligne boucle sur la dernière, visible.
        p.key(
            Key::Tab,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        );
        assert_eq!(p.selected, p.filtered.len() - 1);
        assert!(selection_visible(&p));
    }

    #[test]
    fn page_keys_move_by_a_screen_and_stay_visible() {
        let mut p = palette();
        p.capacity = 5;
        p.key(Key::PageDown, plain());
        assert_eq!((p.selected, p.first), (4, 0));
        p.key(Key::PageDown, plain());
        assert_eq!((p.selected, p.first), (8, 4));
        p.key(Key::PageUp, plain());
        assert_eq!((p.selected, p.first), (4, 4));
        p.key(Key::PageUp, plain());
        assert_eq!((p.selected, p.first), (0, 0));
        // Au bout, une page de trop s'arrête sur la dernière ligne.
        for _ in 0..40 {
            p.key(Key::PageDown, plain());
        }
        assert_eq!(p.selected, p.filtered.len() - 1);
        assert_eq!(p.first, p.filtered.len() - 5);
    }

    #[test]
    fn ctrl_home_end_jump_to_list_ends() {
        let mut p = typed(palette(), "a");
        let last = p.filtered.len() - 1;
        p.key(Key::End, ctrl());
        assert_eq!(p.selected, last);
        assert!(selection_visible(&p));
        assert_eq!(p.first + p.visible(), p.filtered.len());
        p.key(Key::Home, ctrl());
        assert_eq!((p.selected, p.first), (0, 0));
    }

    #[test]
    fn home_end_go_to_the_list_only_when_the_field_is_empty() {
        // Champ vide : Fin va à la dernière commande (« Paramètres »).
        let mut p = palette();
        p.key(Key::End, plain());
        assert_eq!(p.current(), Some(Command::Settings));
        assert!(selection_visible(&p));
        p.key(Key::Home, plain());
        assert_eq!(p.selected, 0);
        // Requête tapée : Origine et Fin déplacent le curseur du champ.
        let mut p = typed(palette(), "pa");
        p.key(Key::Down, plain());
        p.key(Key::Home, plain());
        assert_eq!((p.input.caret, p.selected), (0, 1));
        p.key(Key::End, plain());
        assert_eq!((p.input.caret, p.selected), (2, 1));
    }

    #[test]
    fn wheel_scrolls_and_pulls_selection_into_view() {
        let mut p = palette();
        p.capacity = 5;
        p.wheel(-2.0);
        assert_eq!(p.first, 6, "deux crans, trois lignes chacun");
        assert_eq!(p.selected, 6, "la sélection suit la liste");
        assert_eq!(p.current(), p.command_at(6));
        // Vers le haut, la sélection reste en bas de ce qui est visible.
        p.key(Key::Down, plain());
        p.key(Key::Down, plain());
        p.wheel(1.0);
        assert_eq!(p.first, 3);
        assert_eq!(p.selected, 7);
        // Jamais au-delà des bouts.
        p.wheel(50.0);
        assert_eq!(p.first, 0);
        p.wheel(-50.0);
        assert_eq!(p.first, p.filtered.len() - 5);
        assert!(selection_visible(&p));
    }

    #[test]
    fn small_wheel_steps_add_up() {
        // Un pavé tactile : des quarts de cran, qui ne faisaient rien un à un.
        let mut p = palette();
        p.capacity = 5;
        p.wheel(-0.25);
        assert_eq!(p.first, 0, "trois quarts de ligne : pas encore");
        for _ in 0..3 {
            p.wheel(-0.25);
        }
        assert_eq!(p.first, 3, "un cran entier en quatre fois : trois lignes");
        // Le sens change : le reste de l'autre sens ne retient pas la liste.
        p.wheel(-0.25);
        p.wheel(0.5);
        assert_eq!(p.first, 2);
        assert!(selection_visible(&p));
    }

    #[test]
    fn enter_never_runs_a_hidden_command() {
        let mut p = palette();
        p.capacity = 6;
        let moves: [&dyn Fn(&mut Palette); 8] = [
            &|p: &mut Palette| p.wheel(-1.0),
            &|p: &mut Palette| p.wheel(-3.0),
            &|p: &mut Palette| p.wheel(2.0),
            &|p: &mut Palette| {
                p.key(Key::Down, plain());
            },
            &|p: &mut Palette| {
                p.key(Key::PageDown, plain());
            },
            &|p: &mut Palette| {
                p.key(Key::Up, plain());
            },
            &|p: &mut Palette| {
                p.key(Key::End, ctrl());
            },
            &|p: &mut Palette| {
                p.key(Key::Tab, plain());
            },
        ];
        // Une suite pseudo-aléatoire mais fixe de gestes.
        let mut seed = 7u32;
        for _ in 0..300 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            if let Some(action) = moves.get((seed >> 16) as usize % moves.len()) {
                action(&mut p);
            }
            assert!(
                selection_visible(&p),
                "{} hors de {}+6",
                p.selected,
                p.first
            );
        }
    }

    #[test]
    fn clicks_map_through_the_scroll_offset() {
        let mut p = palette();
        p.capacity = 5;
        p.first = 4;
        p.selected = 4;
        p.card = (0, 0, 400, 300);
        p.rows = vec![(0, 50, 380, 30), (0, 80, 380, 30), (0, 110, 380, 30)];
        // La ligne dessinée en premier est la cinquième de la liste.
        assert_eq!(p.mouse_down(10, 60), PaletteDown::Row);
        assert_eq!(p.mouse_up(10, 60), p.command_at(4));
        assert_eq!(p.mouse_down(10, 90), PaletteDown::Row);
        assert_eq!(p.selected, 5);
        assert_eq!(p.mouse_up(10, 95), p.command_at(5));
        // Le survol aussi passe par le décalage.
        assert!(p.mouse_move(10, 120, false));
        assert_eq!(p.selected, 6);
        // Dans la carte hors des lignes, la palette reste ; dehors, non.
        assert_eq!(p.mouse_down(10, 10), PaletteDown::Inside);
        assert_eq!(p.mouse_down(500, 10), PaletteDown::Outside);
    }

    #[test]
    fn the_scrollbar_can_be_grabbed() {
        let mut p = palette();
        p.capacity = 5;
        p.card = (0, 0, 400, 300);
        p.track = Some((388, 50, 12, 150));
        p.thumb = 30;
        let max = p.filtered.len() - 5;
        // Un appui au bas de la piste mène au bout de la liste…
        assert_eq!(p.mouse_down(392, 199), PaletteDown::Inside);
        assert_eq!(p.first, max);
        assert!(selection_visible(&p));
        // … et le pouce tenu remonte avec le pointeur.
        assert!(p.mouse_move(392, 50, true));
        assert_eq!(p.first, 0);
        assert!(selection_visible(&p));
        // Relâché, rien ne s'exécute.
        assert_eq!(p.mouse_up(392, 50), None);
        assert!(!p.mouse_move(392, 120, true), "le pouce est lâché");
    }

    #[test]
    fn la_palette_choisit_au_relachement() {
        let mut p = palette();
        p.card = (0, 0, 400, 300);
        p.rows = vec![(0, 50, 400, 30), (0, 80, 400, 30)];
        let second = p.command_at(1);
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
    fn matched_letters_are_marked() {
        let p = typed(palette(), "enrs");
        assert_eq!(marks_of(&p, Command::SaveAs), [0, 1, 2, 6]);
        let p = typed(palette(), "sous");
        assert_eq!(marks_of(&p, Command::SaveAs), [12, 13, 14, 15]);
    }

    #[test]
    fn accented_letters_are_marked_in_place() {
        // « Première » : neuf caractères, dix octets. Les rangs sont des
        // caractères, sinon la mise en évidence glisserait d'une lettre.
        let p = typed(palette(), "premiere");
        assert_eq!(p.current(), Some(Command::FirstPage));
        assert_eq!(marks_of(&p, Command::FirstPage), (0..8).collect::<Vec<_>>());
        let p = typed(palette(), "page");
        assert_eq!(marks_of(&p, Command::FirstPage), [9, 10, 11, 12]);
    }

    #[test]
    fn synonyms_only_match_in_one_piece() {
        // Tapées dans l'ordre, ces lettres sont aussi dans les synonymes de
        // bien des commandes (« accueil maison récents ») : seules restent
        // celles dont le libellé, à l'écran, dit pourquoi.
        let p = typed(palette(), "enrs");
        assert_eq!(commands(&p), [Command::Save, Command::SaveAs]);
    }

    #[test]
    fn synonym_matches_mark_nothing() {
        let p = typed(palette(), "rotation");
        assert_eq!(p.current(), Some(Command::RotateRight));
        assert!(marks_of(&p, Command::RotateRight).is_empty());
    }

    #[test]
    fn english_labels_are_searchable() {
        let p = typed(Palette::with_lang(true, &[], true), "open");
        assert_eq!(p.current(), Some(Command::Open));
        assert_eq!(marks_of(&p, Command::Open), [0, 1, 2, 3]);
        // Le libellé français se trouve encore, derrière, sans marque.
        let p = typed(Palette::with_lang(true, &[], true), "ouvrir");
        assert_eq!(p.current(), Some(Command::Open));
        assert!(marks_of(&p, Command::Open).is_empty());
        assert_eq!(p.input.placeholder, "Type a command");
    }

    #[test]
    fn pasting_filters_like_typing() {
        let mut p = palette();
        p.paste("sous\r\n");
        assert_eq!(p.current(), Some(Command::SaveAs));
    }

    #[test]
    fn unknown_query_lists_nothing() {
        let mut p = typed(palette(), "zzzqqq");
        assert!(p.filtered.is_empty());
        assert_eq!(p.current(), None);
        // Aucune touche ne casse une liste vide.
        for key in [Key::Down, Key::PageDown, Key::End, Key::Tab, Key::Up] {
            p.key(key, ctrl());
        }
        p.wheel(-3.0);
        assert_eq!(p.current(), None);
        let (cmd, close) = p.key(Key::Enter, plain());
        assert_eq!((cmd, close), (None, true));
    }

    #[test]
    fn les_commandes_des_menus_contextuels_se_trouvent_aussi_ici() {
        assert_eq!(top("propr"), Some(Command::Properties));
        assert_eq!(top("metadonnees"), Some(Command::Properties));
        assert_eq!(top("dossier"), Some(Command::RevealInFolder));
        assert_eq!(top("chemin"), Some(Command::CopyPath));
        assert_eq!(top("autres onglets"), Some(Command::CloseOtherTabs));
        let properties = ENTRIES
            .iter()
            .find(|e| e.command == Command::Properties)
            .map(|e| (e.label, e.shortcut));
        assert_eq!(properties, Some(("Propriétés du document", "Ctrl+D")));
    }

    #[test]
    fn une_commande_sans_effet_n_est_pas_proposee() {
        let full = Palette::with_lang(true, &[], false);
        let p = Palette::with_lang(true, &[], false)
            .without(&[Command::ViewBack, Command::ViewForward]);
        assert_eq!(p.full_len, full.full_len - 2);
        assert!(commands(&full).contains(&Command::ViewBack));
        assert!(!commands(&p).contains(&Command::ViewBack));
        assert!(!commands(&p).contains(&Command::ViewForward));
        assert!(commands(&p).contains(&Command::RotateViewRight));
        // La frappe ne les fait pas revenir.
        assert_ne!(typed(p, "vue prec").current(), Some(Command::ViewBack));
    }

    #[test]
    fn l_historique_et_la_rotation_de_la_vue_se_trouvent() {
        assert_eq!(top("vue prec"), Some(Command::ViewBack));
        assert_eq!(top("vue suiv"), Some(Command::ViewForward));
        assert_eq!(top("pivoter la vue"), Some(Command::RotateViewRight));
        assert_eq!(top("onglet prec"), Some(Command::PrevTab));
        // « rotation » mène d'abord à la page, que l'on pivote plus souvent.
        assert_eq!(top("rotation"), Some(Command::RotateRight));
        let keys = |c| {
            ENTRIES
                .iter()
                .find(|e| e.command == c)
                .map(|e| (e.shortcut, shortcut_in_lang(e.shortcut, true)))
        };
        assert_eq!(keys(Command::ViewBack), Some(("Alt+←", "Alt+←")));
        assert_eq!(
            keys(Command::RotateViewRight),
            Some(("Ctrl+Maj+Plus", "Ctrl+Shift+Plus"))
        );
        assert_eq!(
            keys(Command::RotateViewLeft),
            Some(("Ctrl+Maj+Moins", "Ctrl+Shift+Minus"))
        );
        assert_eq!(
            keys(Command::CloseTab),
            Some(("Ctrl+W, Ctrl+F4", "Ctrl+W, Ctrl+F4"))
        );
    }

    #[test]
    fn retirer_un_recent_ne_se_lance_pas_depuis_la_palette() {
        // Elle vise une carte précise de l'accueil : sans cible, elle n'a
        // pas de sens, et la palette ne la propose donc pas.
        assert!(!ENTRIES.iter().any(|e| e.command == Command::ForgetRecent));
        assert_eq!(Command::from_key(Command::ForgetRecent.key()), None);
        let all = commands(&palette());
        assert!(!all.contains(&Command::ForgetRecent));
    }
}
