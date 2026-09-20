//! Langue de l'interface : français ou anglais.
//!
//! Le texte de l'interface est écrit **en français dans le code** — c'est la
//! langue du projet, et une clé qui se lit vaut mieux qu'un identifiant qu'il
//! faut aller chercher. La traduction est donc une table `français → anglais`
//! consultée à l'affichage ; ce qui n'y figure pas reste en français, ce qui
//! est une dégradation lisible plutôt qu'une clé nue à l'écran.
//!
//! La langue effective est un entier partagé : la lire coûte une instruction,
//! ce qui permet d'appeler [`tr`] partout sans y penser, y compris dans une
//! boucle de dessin.

use std::sync::atomic::{AtomicU8, Ordering};

/// Langue choisie dans les préférences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    /// Celle du système.
    #[default]
    Auto,
    /// Français.
    French,
    /// Anglais.
    English,
}

impl Lang {
    /// Clé écrite dans les préférences.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Lang::Auto => "auto",
            Lang::French => "fr",
            Lang::English => "en",
        }
    }

    /// Langue d'une clé de préférences.
    #[must_use]
    pub fn from_key(key: &str) -> Lang {
        match key {
            "fr" => Lang::French,
            "en" => Lang::English,
            _ => Lang::Auto,
        }
    }

    /// Nom affiché.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Lang::Auto => "Système",
            Lang::French => "Français",
            Lang::English => "English",
        }
    }
}

/// Langue effective : 0 = français, 1 = anglais.
static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Choisit la langue effective. `system` est celle du système, employée
/// quand le réglage vaut « auto ».
pub fn apply(choice: Lang, system: Lang) {
    let effective = match choice {
        Lang::Auto => system,
        other => other,
    };
    CURRENT.store(u8::from(effective == Lang::English), Ordering::Relaxed);
}

/// Vrai si l'interface est en anglais.
#[must_use]
pub fn english() -> bool {
    CURRENT.load(Ordering::Relaxed) == 1
}

/// Traduit un texte d'interface écrit en français.
///
/// Rend la chaîne d'origine quand la langue est le français, ou qu'aucune
/// traduction n'est connue.
#[must_use]
pub fn tr(french: &'static str) -> &'static str {
    if !english() {
        return french;
    }
    match TABLE.binary_search_by(|(fr, _)| (*fr).cmp(french)) {
        Ok(i) => TABLE[i].1,
        Err(_) => french,
    }
}

/// Traduit un texte à trous : chaque `{}` est remplacé, dans l'ordre, par les
/// valeurs données.
///
/// `format!` demande un littéral ; une phrase traduite n'en est pas un. Cette
/// fonction fait le remplacement à la main, ce qui permet aux deux langues
/// d'avoir leurs trous dans le même ordre.
#[must_use]
pub fn trf(french: &'static str, values: &[&str]) -> String {
    let mut out = String::with_capacity(french.len() + 16);
    let mut rest = tr(french);
    for value in values {
        match rest.split_once("{}") {
            Some((before, after)) => {
                out.push_str(before);
                out.push_str(value);
                rest = after;
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// Table de traduction, **triée par le français** (une recherche
/// dichotomique la consulte). Un test vérifie l'ordre et l'absence de
/// doublons : une table désordonnée rendrait des traductions au hasard.
static TABLE: &[(&str, &str)] = &[
    ("1 document récent", "1 recent document"),
    ("Accueil", "Home"),
    ("Accueil — Échap ou la maison pour revenir au document", "Home — Esc or the house to return to the document"),
    ("Acrux est à jour.", "Acrux is up to date."),
    ("Acrux suit la langue du système ({}). Vous pouvez en imposer une autre ; le choix est retenu.", "Acrux follows the system language ({}). You can force another one; the choice is kept."),
    ("Ajouter", "Add"),
    ("Ajouter du texte", "Add text"),
    ("Ajouter une signature", "Add a signature"),
    ("Annuler", "Cancel"),
    ("Annuler le trait", "Undo stroke"),
    ("Appliquer", "Apply"),
    ("Appliquer les biffures", "Apply redactions"),
    ("Appliquer les biffures ?", "Apply redactions?"),
    ("Aucun document — Ctrl+O pour ouvrir, ou déposez un PDF ici", "No document — Ctrl+O to open, or drop a PDF here"),
    ("Biffer", "Redact"),
    ("Bleu", "Blue"),
    ("Choisir une image…", "Choose an image…"),
    ("Choix actuel : {}.", "Current choice: {}."),
    ("Cliquez sur la page et tapez", "Click the page and type"),
    ("Coche", "Check"),
    ("Commenter", "Comment"),
    ("Couleur", "Colour"),
    ("Croix", "Cross"),
    ("Créer un paraphe", "Create initials"),
    ("Créer une signature", "Create a signature"),
    ("Dessiner", "Draw"),
    ("Document", "Document"),
    ("Documents récents", "Recent documents"),
    ("Dupliquer la page", "Duplicate page"),
    ("Effacer", "Clear"),
    ("Encre", "Ink"),
    ("English", "English"),
    ("Enregistrement impossible", "Could not save"),
    ("Enregistrer", "Save"),
    ("Enregistrer les modifications ?", "Save changes?"),
    ("Exporter", "Export"),
    ("Extraire la page", "Extract page"),
    ("Feutre", "Marker"),
    ("Fichier introuvable", "File not found"),
    ("Fin", "Thin"),
    ("Français", "French"),
    ("Importer", "Import"),
    ("Imprimer", "Print"),
    ("Installer", "Install"),
    ("Insérer des pages", "Insert pages"),
    ("Joindre un fichier", "Attach a file"),
    ("La page sera retirée du document. Ctrl+Z la rétablit ; Ctrl+S enregistre.", "The page will be removed from the document. Ctrl+Z brings it back; Ctrl+S saves."),
    ("Langue de l'interface", "Interface language"),
    ("Le contenu couvert par les marques sera supprimé définitivement du document.", "Content covered by the marks will be permanently removed from the document."),
    ("Lire, modifier, remplir et signer un PDF.", "Read, edit, fill in and sign a PDF."),
    ("Marques", "Marks"),
    ("Modifier", "Edit"),
    ("Modifier le PDF", "Edit PDF"),
    ("Modifier le texte", "Edit text"),
    ("Modifier les objets", "Edit objects"),
    ("Moyen", "Medium"),
    ("Ne pas enregistrer", "Don't save"),
    ("Noir", "Black"),
    ("OK", "OK"),
    ("Ouvrir un document", "Open a document"),
    ("Ouvrir un document…", "Open a document…"),
    ("Pages", "Pages"),
    ("Paramètres", "Settings"),
    ("Paramètres…", "Settings…"),
    ("Paraphe", "Initials"),
    ("Pivoter", "Rotate"),
    ("Plume", "Fountain pen"),
    ("Point", "Dot"),
    ("Poser une note", "Add a note"),
    ("Protéger", "Protect"),
    ("Quitter sans enregistrer", "Quit without saving"),
    ("Refaire", "Redo it"),
    ("Remplir et signer", "Fill and sign"),
    ("Retirer", "Remove"),
    ("Rond", "Circle"),
    ("Rouge", "Red"),
    ("Signature", "Signature"),
    ("Signatures", "Signatures"),
    ("Signer", "Sign"),
    ("Stylo", "Pen"),
    ("Supprimer", "Delete"),
    ("Supprimer la page", "Delete page"),
    ("Surligner", "Highlight"),
    ("Système", "System"),
    ("Taille", "Size"),
    ("Taper", "Type"),
    ("Terminer", "Done"),
    ("Texte", "Text"),
    ("Tout enregistrer", "Save all"),
    ("Tracer", "Draw"),
    ("Tracez votre signature ici — Ctrl+Z défait le dernier trait", "Draw your signature here — Ctrl+Z undoes the last stroke"),
    ("Tracez à main levée", "Draw freehand"),
    ("Trait", "Line"),
    ("Vert", "Green"),
    ("Votre nom sera écrit dans une police manuscrite du système.", "Your name will be written in a handwriting font from the system."),
    ("Votre paraphe", "Your initials"),
    ("Votre signature", "Your signature"),
    ("accueil", "home"),
    ("auto", "auto"),
    ("bloc sélectionné : glissez pour le déplacer, les poignées pour le redimensionner, double-cliquez pour écrire", "block selected: drag to move it, the handles to resize it, double-click to type"),
    ("ce bloc ne peut pas être déplacé", "this block cannot be moved"),
    ("continu", "continuous"),
    ("continu, deux pages", "continuous, two pages"),
    ("deux pages", "two pages"),
    ("largeur", "width"),
    ("ou déposez un PDF sur la fenêtre · Ctrl+Maj+P pour toutes les commandes", "or drop a PDF on the window · Ctrl+Shift+P for every command"),
    ("page", "page"),
    ("page unique", "single page"),
    ("{} documents ouverts ont été modifiés. Les enregistrer avant de quitter ?", "{} open documents have been modified. Save them before quitting?"),
    ("{} documents récents", "{} recent documents"),
    ("« {} » a été modifié. Enregistrer les modifications avant de le fermer ?", "“{}” has been modified. Save the changes before closing it?"),
    ("« {} » a été modifié. Enregistrer les modifications avant de quitter ?", "“{}” has been modified. Save the changes before quitting?"),
    ("Épais", "Thick"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_table_est_triee_et_sans_doublon() {
        for pair in TABLE.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "table désordonnée : « {} » avant « {} »",
                pair[0].0,
                pair[1].0
            );
        }
    }

    #[test]
    fn le_francais_se_rend_tel_quel() {
        apply(Lang::French, Lang::English);
        assert_eq!(tr("Terminer"), "Terminer");
        assert!(!english());
    }

    #[test]
    fn langlais_traduit_ce_quil_connait() {
        apply(Lang::English, Lang::French);
        assert_eq!(tr("Terminer"), "Done");
        // Ce qui manque à la table reste lisible.
        assert_eq!(
            tr("Phrase absente de la table"),
            "Phrase absente de la table"
        );
        apply(Lang::French, Lang::French);
    }

    #[test]
    fn auto_suit_le_systeme() {
        apply(Lang::Auto, Lang::English);
        assert!(english());
        apply(Lang::Auto, Lang::French);
        assert!(!english());
    }

    #[test]
    fn les_trous_se_remplissent_dans_les_deux_langues() {
        apply(Lang::French, Lang::French);
        assert_eq!(trf("{} documents récents", &["4"]), "4 documents récents");
        apply(Lang::English, Lang::French);
        assert_eq!(trf("{} documents récents", &["4"]), "4 recent documents");
        // Plus de valeurs que de trous : le reste est ignoré sans casse.
        assert_eq!(trf("Terminer", &["x"]), "Done");
        apply(Lang::French, Lang::French);
    }

    #[test]
    fn les_cles_font_laller_retour() {
        for lang in [Lang::Auto, Lang::French, Lang::English] {
            assert_eq!(Lang::from_key(lang.key()), lang);
            assert!(!lang.label().is_empty());
        }
        assert_eq!(Lang::from_key("n'importe quoi"), Lang::Auto);
    }
}
