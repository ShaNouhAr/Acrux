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
    if english() {
        to_english(french)
    } else {
        french
    }
}

/// Traduction anglaise d'un texte, quelle que soit la langue effective ; le
/// texte d'origine si la table ne le connaît pas.
///
/// C'est le cœur de [`tr`], sans l'état global : la palette de commandes s'en
/// sert pour chercher dans les libellés de la langue qu'elle a lue à son
/// ouverture, et ses épreuves ne dépendent pas de celles d'ici, qui changent
/// la langue de tout le programme pendant qu'elles tournent.
#[must_use]
pub fn to_english(french: &'static str) -> &'static str {
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
    (" (permissions retouchées : les plus strictes s'appliquent)", " (permissions tampered with: the strictest apply)"),
    ("1 document récent", "1 recent document"),
    ("Accueil", "Home"),
    ("Accueil — Échap ou la maison pour revenir au document", "Home — Esc or the house to return to the document"),
    ("Acrux cherche une version plus récente au démarrage, au plus une fois par jour. Rien n'est installé sans votre accord.", "Acrux looks for a newer version at startup, at most once a day. Nothing is installed without your consent."),
    ("Acrux est à jour.", "Acrux is up to date."),
    ("Acrux {} est disponible.", "Acrux {} is available."),
    ("Afficher le panneau latéral", "Show side panel"),
    ("Ajouter", "Add"),
    ("Ajouter du texte", "Add text"),
    ("Ajouter une signature", "Add a signature"),
    ("Ajustement automatique", "Automatic fit"),
    ("Ajuster", "Adjust"),
    ("Ajuster à la largeur", "Fit width"),
    ("Ajuster à la page", "Fit page"),
    ("Aller à une page (numéro ou étiquette)", "Go to a page (number or label)"),
    ("Annuler", "Cancel"),
    ("Annuler la dernière action", "Undo the last action"),
    ("Annuler le trait", "Undo stroke"),
    ("Apparence", "Appearance"),
    ("Application", "Application"),
    ("Appliquer", "Apply"),
    ("Appliquer les biffures", "Apply redactions"),
    ("Appliquer les biffures ?", "Apply redactions?"),
    ("Assembler (pages, signets)", "Assemble (pages, bookmarks)"),
    ("Aucun document — Ctrl+O pour ouvrir, ou déposez un PDF ici", "No document — Ctrl+O to open, or drop a PDF here"),
    ("Aucun résultat", "No results"),
    ("Aucune commande", "No matching command"),
    ("Aucune vue précédente", "No previous view"),
    ("Aucune vue suivante", "No next view"),
    ("Auteur", "Author"),
    ("Barre des outils", "Tools pane"),
    ("Basse résolution", "Low resolution"),
    ("Biffer", "Redact"),
    ("Biffure : appliquer définitivement", "Redaction: apply permanently"),
    ("Biffure : marquer la sélection", "Redaction: mark the selection"),
    ("Bleu", "Blue"),
    ("Bon", "Good"),
    ("Calques (afficher ou masquer le contenu optionnel)", "Layers (show or hide optional content)"),
    ("Ce document est déjà protégé", "This document is already protected"),
    ("Ce mot de passe lève les restrictions ; lui seul permet de changer la protection.", "This password lifts the restrictions; only it can change the protection."),
    ("Ce n'est pas le mot de passe des permissions", "This is not the permissions password"),
    ("Cette occurrence coupe une ligature : cherchez le mot entier pour la remplacer.", "This match splits a ligature: search for the whole word to replace it."),
    ("Cette occurrence est à cheval sur deux lignes : modifiez-la dans l'éditeur de texte.", "This match spans two lines: change it in the text editor."),
    ("Champ de formulaire", "Form field"),
    ("Changer la protection", "Change the protection"),
    ("Changer sa protection, ou la retirer ?", "Change its protection, or remove it?"),
    ("Chemin copié", "Path copied"),
    ("Chercher au démarrage", "Check at startup"),
    ("Chiffrement AES-256. Notez vos mots de passe : un mot de passe perdu ne se retrouve pas.", "AES-256 encryption. Write your passwords down: a lost password cannot be recovered."),
    ("Choisir le niveau de zoom", "Choose the zoom level"),
    ("Choisir le niveau de zoom…", "Choose the zoom level…"),
    ("Choisir un élément posé, le déplacer, le redimensionner", "Pick a placed item, move it, resize it"),
    ("Choisir une image…", "Choose an image…"),
    ("Clair", "Light"),
    ("Cliquez sur la page et tapez", "Click the page and type"),
    ("Coche", "Check"),
    ("Commentaire :", "Comment:"),
    ("Commenter", "Comment"),
    ("Confirmez", "Confirm"),
    ("Convertisseur PDF", "PDF producer"),
    ("Copier", "Copy"),
    ("Copier la sélection", "Copy the selection"),
    ("Copier le chemin", "Copy path"),
    ("Copier le chemin du fichier", "Copy the file path"),
    ("Copier le texte et les images", "Copy text and images"),
    ("Couleur", "Colour"),
    ("Croix", "Cross"),
    ("Créer un paraphe", "Create initials"),
    ("Créer une signature", "Create a signature"),
    ("Créé le", "Created"),
    ("Ctrl+molette", "Ctrl+wheel"),
    ("Dernière page", "Last page"),
    ("Dessiner", "Draw"),
    ("Disposition : {} — cliquer pour passer à « {} »", "Layout: {} — click to switch to “{}”"),
    ("Disposition des pages", "Page layout"),
    ("Document", "Document"),
    ("Document protégé", "Protected document"),
    ("Document protégé : {}", "Protected document: {}"),
    ("Documents récents", "Recent documents"),
    ("Dossier", "Folder"),
    ("Dupliquer", "Duplicate"),
    ("Dupliquer la page", "Duplicate page"),
    ("Déplacer", "Move"),
    ("Effacer", "Clear"),
    ("Encre", "Ink"),
    ("English", "English"),
    ("Enregistrement impossible", "Could not save"),
    ("Enregistrer", "Save"),
    ("Enregistrer les modifications ?", "Save changes?"),
    ("Enregistrer sous", "Save as"),
    ("Entrée : suivante · Maj+Entrée : précédente", "Enter: next · Shift+Enter: previous"),
    ("Exiger un mot de passe pour ouvrir le document", "Require a password to open the document"),
    ("Export interdit", "Export not allowed"),
    ("Exporter", "Export"),
    ("Exporter (page web, Word, Excel, images, texte)", "Export (web page, Word, Excel, images, text)"),
    ("Extraction interdite", "Extraction not allowed"),
    ("Extraction pour l'accessibilité", "Extraction for accessibility"),
    ("Extraire la page", "Extract page"),
    ("Extraire la page dans un fichier", "Extract the page to a file"),
    ("Extraire…", "Extract…"),
    ("Faible", "Weak"),
    ("Faire pivoter la vue", "Rotate the view"),
    ("Faire pivoter la vue à droite", "Rotate the view clockwise"),
    ("Faire pivoter la vue à gauche", "Rotate the view counter-clockwise"),
    ("Fermer", "Close"),
    ("Fermer l'onglet", "Close the tab"),
    ("Fermer la recherche (Échap)", "Close search (Esc)"),
    ("Fermer les autres onglets", "Close other tabs"),
    ("Feutre", "Marker"),
    ("Fichier", "File"),
    ("Fichier introuvable", "File not found"),
    ("Fin", "Thin"),
    ("Fort", "Strong"),
    ("Français", "French"),
    ("Haute résolution", "High resolution"),
    ("Historique vidé", "History cleared"),
    ("Il donne tous les droits sur ce document :", "It grants every right over this document:"),
    ("Image convertie en PDF ({} page(s)) — Ctrl+S pour l'enregistrer", "Image converted to PDF ({} page(s)) — Ctrl+S to save it"),
    ("Importer", "Import"),
    ("Impression", "Printing"),
    ("Impression interdite", "Printing not allowed"),
    ("Imprimer", "Print"),
    ("Imprimer…", "Print…"),
    ("Installer", "Install"),
    ("Insérer des pages", "Insert pages"),
    ("Insérer les pages d'un fichier", "Insert pages from a file"),
    ("Jamais", "Never"),
    ("Joindre un fichier", "Attach a file"),
    ("Joindre un fichier au document", "Attach a file to the document"),
    ("La liste des documents récents sera effacée. Les fichiers eux-mêmes ne sont pas touchés.", "The list of recent documents will be cleared. The files themselves are not touched."),
    ("La page sera retirée du document. Ctrl+Z la rétablit ; Ctrl+S enregistre.", "The page will be removed from the document. Ctrl+Z brings it back; Ctrl+S saves."),
    ("La recherche automatique est désactivée : Acrux ne contacte rien au démarrage.", "Automatic checking is off: Acrux contacts nothing at startup."),
    ("Langue", "Language"),
    ("Le contenu couvert par les marques sera supprimé définitivement du document.", "Content covered by the marks will be permanently removed from the document."),
    ("Le mot de passe des permissions doit différer de celui d'ouverture", "The permissions password must differ from the open password"),
    ("Les permissions de ce document interdisent d'en extraire le contenu. Le mot de passe des permissions lève cette restriction.", "This document's permissions do not allow extracting its content. The permissions password lifts this restriction."),
    ("Les permissions de ce document interdisent l'impression. Le mot de passe des permissions lève cette restriction.", "This document's permissions do not allow printing. The permissions password lifts this restriction."),
    ("Les permissions du document interdisent cette modification. Le mot de passe des permissions lève cette restriction.", "The document's permissions do not allow this change. The permissions password lifts this restriction."),
    ("Les restrictions exigent un mot de passe des permissions", "Restrictions need a permissions password"),
    ("Lire, modifier, remplir et signer un PDF.", "Read, edit, fill in and sign a PDF."),
    ("Marques", "Marks"),
    ("Masquer le panneau latéral", "Hide side panel"),
    ("Masquer les outils", "Hide tools"),
    ("Mises à jour", "Updates"),
    ("Modification interdite", "Change not allowed"),
    ("Modifier", "Edit"),
    ("Modifier : objets de la page", "Edit: page objects"),
    ("Modifier le PDF", "Edit PDF"),
    ("Modifier le contenu", "Change the content"),
    ("Modifier le texte", "Edit text"),
    ("Modifier le texte sélectionné", "Edit the selected text"),
    ("Modifier les objets", "Edit objects"),
    ("Modifié le", "Modified"),
    ("Modèle 3D ({} triangles) — glisser pour tourner, molette pour zoomer, Échap pour refermer", "3D model ({} triangles) — drag to rotate, wheel to zoom, Esc to close"),
    ("Mot de passe", "Password"),
    ("Mot de passe d'ouverture", "Password to open"),
    ("Mot de passe des permissions", "Permissions password"),
    ("Mot de passe incorrect", "Incorrect password"),
    ("Mot de passe pour « {} » :", "Password for “{}”:"),
    ("Mot entier", "Whole word"),
    ("Mots-clés", "Keywords"),
    ("Moyen", "Medium"),
    ("Ne pas enregistrer", "Don't save"),
    ("Noir", "Black"),
    ("Non", "No"),
    ("Nouveau texte", "New text"),
    ("Nouveau texte :", "New text:"),
    ("Nouvelle note", "New note"),
    ("Nécessaire pour changer la protection :", "Needed to change the protection:"),
    ("Nécessaire pour retirer la protection :", "Needed to remove the protection:"),
    ("OK", "OK"),
    ("Occurrence précédente", "Previous match"),
    ("Occurrence précédente (Maj+F3)", "Previous match (Shift+F3)"),
    ("Occurrence suivante", "Next match"),
    ("Occurrence suivante (F3)", "Next match (F3)"),
    ("Onglet précédent", "Previous tab"),
    ("Onglet suivant", "Next tab"),
    ("Outil biffure", "Redaction tool"),
    ("Outil note", "Note tool"),
    ("Outil surligneur", "Highlighter tool"),
    ("Ouvrir", "Open"),
    ("Ouvrir le dossier du fichier", "Show in folder"),
    ("Ouvrir un document", "Open a document"),
    ("Ouvrir un document…", "Open a document…"),
    ("Page précédente", "Previous page"),
    ("Page suivante", "Next page"),
    ("Pages", "Pages"),
    ("Panneau latéral (vignettes, signets)", "Side panel (thumbnails, bookmarks)"),
    ("Paramètres", "Settings"),
    ("Paramètres…", "Settings…"),
    ("Paraphe", "Initials"),
    ("Passer au thème clair", "Switch to light theme"),
    ("Passer au thème sombre", "Switch to dark theme"),
    ("Permissions", "Permissions"),
    ("Pivoter", "Rotate"),
    ("Pivoter la page", "Rotate the page"),
    ("Pivoter la page à droite", "Rotate the page clockwise"),
    ("Pivoter la page à gauche", "Rotate the page counter-clockwise"),
    ("Pièces jointes du document", "Document attachments"),
    ("Plein écran", "Full screen"),
    ("Plume", "Fountain pen"),
    ("Point", "Dot"),
    ("Poser une note", "Add a note"),
    ("Poser une note ici", "Add a note here"),
    ("Première page", "First page"),
    ("Propriétés du document", "Document properties"),
    ("Protection impossible", "Could not protect the document"),
    ("Protéger", "Protect"),
    ("Protéger par mot de passe", "Protect with a password"),
    ("Protégé", "Protected"),
    ("Quitter sans enregistrer", "Quit without saving"),
    ("Recherche en cours…", "Checking…"),
    ("Rechercher dans le document", "Search the document"),
    ("Rechercher et remplacer", "Find and replace"),
    ("Rechercher les mises à jour", "Check for updates"),
    ("Rechercher maintenant", "Check now"),
    ("Refaire", "Redo it"),
    ("Remplacer", "Replace"),
    ("Remplacer par…", "Replace with…"),
    ("Remplir et signer", "Fill and sign"),
    ("Remplir les cases", "Fill in the boxes"),
    ("Remplir les formulaires", "Fill in forms"),
    ("Rendu en cours…", "Rendering…"),
    ("Respecter la casse", "Match case"),
    ("Retirer", "Remove"),
    ("Retirer de la liste", "Remove from the list"),
    ("Retirer la protection", "Remove the protection"),
    ("Retiré de la liste", "Removed from the list"),
    ("Rond", "Circle"),
    ("Rouge", "Red"),
    ("Rétablir l'action annulée", "Redo the undone action"),
    ("Saisir le mot de passe", "Enter the password"),
    ("Saisissez le mot de passe d'ouverture", "Enter the password to open the document"),
    ("Signature", "Signature"),
    ("Signatures", "Signatures"),
    ("Signer", "Sign"),
    ("Sombre", "Dark"),
    ("Stylo", "Pen"),
    ("Sujet", "Subject"),
    ("Supprimer", "Delete"),
    ("Supprimer la page", "Delete page"),
    ("Surligner", "Highlight"),
    ("Surligner et commenter", "Highlight and comment"),
    ("Surligner la sélection", "Highlight the selection"),
    ("Système", "System"),
    ("Système ({})", "System ({})"),
    ("Taille", "Size"),
    ("Taper", "Type"),
    ("Tapez une commande", "Type a command"),
    ("Terminer", "Done"),
    ("Texte", "Text"),
    ("Texte de la note (page {}) :", "Note text (page {}):"),
    ("Texte à répartir dans les {} cases :", "Text to spread over the {} boxes:"),
    ("Thème clair / sombre", "Light / dark theme"),
    ("Titre", "Title"),
    ("Tous les outils", "All tools"),
    ("Tout enregistrer", "Save all"),
    ("Tout remplacer", "Replace all"),
    ("Tout sélectionner", "Select all"),
    ("Tracer", "Draw"),
    ("Tracez votre signature ici — Ctrl+Z défait le dernier trait", "Draw your signature here — Ctrl+Z undoes the last stroke"),
    ("Tracez à main levée", "Draw freehand"),
    ("Trait", "Line"),
    ("Valeur", "Value"),
    ("Valider", "OK"),
    ("Version PDF", "PDF version"),
    ("Version installée : {}.", "Installed version: {}."),
    ("Vert", "Green"),
    ("Vider", "Clear"),
    ("Vider l'historique", "Clear history"),
    ("Votre commentaire", "Your comment"),
    ("Votre nom sera écrit dans une police manuscrite du système.", "Your name will be written in a handwriting font from the system."),
    ("Votre paraphe", "Your initials"),
    ("Votre remarque", "Your remark"),
    ("Votre signature", "Your signature"),
    ("Vue pivotée de {}° (le document n'est pas modifié)", "View rotated {}° (the document is unchanged)"),
    ("Vue pivotée de {}° — cliquer pour la remettre droite", "View rotated {}° — click to straighten it"),
    ("Vue précédente", "Previous view"),
    ("Vue remise droite", "View straightened"),
    ("Vue suivante", "Next view"),
    ("Zoom 100 %", "Zoom 100%"),
    ("Zoom arrière", "Zoom out"),
    ("Zoom avant", "Zoom in"),
    ("Zoom limité à {} % pour ce document : au-delà, ses pages ne tiendraient plus en mémoire", "Zoom limited to {}% for this document: beyond that, its pages would not fit in memory"),
    ("accueil", "home"),
    ("assemblage interdit", "assembly not allowed"),
    ("au-delà de 127 octets, la fin du mot de passe est ignorée", "beyond 127 bytes, the end of the password is ignored"),
    ("auto", "auto"),
    ("bloc posé : glissez-le, tirez une poignée, ou cliquez dedans pour écrire", "block placed: drag it, pull a handle, or click inside to type"),
    ("bloc sélectionné : glissez pour le déplacer, les poignées pour le redimensionner, double-cliquez pour écrire", "block selected: drag to move it, the handles to resize it, double-click to type"),
    ("ce bloc ne peut pas être déplacé", "this block cannot be moved"),
    ("ce document n'a pas encore de fichier : enregistrez-le d'abord", "this document has no file yet: save it first"),
    ("ce document n'est pas protégé", "this document is not protected"),
    ("commentaires interdits", "commenting not allowed"),
    ("continu", "continuous"),
    ("continu, deux pages", "continuous, two pages"),
    ("copie interdite", "copying not allowed"),
    ("copie interdite par les permissions du document", "copying is not allowed by the document's permissions"),
    ("deux pages", "two pages"),
    ("document protégé : le mot de passe sera demandé à l'ouverture", "document protected: the password will be asked when opening it"),
    ("document protégé : ouverture libre, permissions restreintes", "document protected: opens freely, permissions restricted"),
    ("extraction pour l'accessibilité interdite", "extraction for accessibility not allowed"),
    ("impression en basse résolution seulement", "low-resolution printing only"),
    ("impression interdite", "printing not allowed"),
    ("largeur", "width"),
    ("le texte de cette image n'a pas pu etre relu", "the text of this image could not be read"),
    ("lecture du texte de l'image...", "reading the text in the image..."),
    ("les deux saisies diffèrent", "the two entries differ"),
    ("mises à jour : recherche au démarrage", "updates: checking at startup"),
    ("mises à jour : recherche désactivée", "updates: checking off"),
    ("modification interdite", "changes not allowed"),
    ("modifications non enregistrées", "unsaved changes"),
    ("non", "no"),
    ("ou déposez un PDF sur la fenêtre · Ctrl+Maj+P pour toutes les commandes", "or drop a PDF on the window · Ctrl+Shift+P for every command"),
    ("oui", "yes"),
    ("oui, tous les droits accordés", "yes, every right granted"),
    ("page", "page"),
    ("page unique", "single page"),
    ("protection retirée : enregistrez pour l'appliquer", "protection removed: save to apply it"),
    ("remplacer substituer corriger partout texte", "replace substitute correct everywhere text"),
    ("remplissage des formulaires interdit", "form filling not allowed"),
    ("texte de l'image reconnu : modifiez-le", "text recognised in the image: edit it"),
    ("tous les droits sont accordés : refaites l'action", "every right is granted: do it again"),
    ("un caractère par case", "one character per box"),
    ("un mot de passe accentué risque d'être refusé par d'autres lecteurs PDF", "a password with accents may be refused by other PDF readers"),
    ("un mot de passe vide ne protège rien", "an empty password protects nothing"),
    ("vue {}°", "view {}°"),
    ("{} documents ouverts ont été modifiés. Les enregistrer avant de quitter ?", "{} open documents have been modified. Save them before quitting?"),
    ("{} documents récents", "{} recent documents"),
    ("{} occurrence(s) remplacée(s)", "{} occurrence(s) replaced"),
    ("{} occurrence(s) remplacée(s), {} ignorée(s) (à cheval sur deux lignes ou coupant une ligature)", "{} occurrence(s) replaced, {} skipped (spanning two lines or splitting a ligature)"),
    ("{} onglet(s) fermé(s)", "{} tab(s) closed"),
    ("{} onglet(s) modifié(s) laissé(s) ouvert(s) : enregistrez-les d'abord", "{} modified tab(s) left open: save them first"),
    ("{} sur {}", "{} of {}"),
    ("{} trouvée(s), recherche…", "{} found, searching…"),
    ("« {} » a été modifié. Enregistrer les modifications avant de le fermer ?", "“{}” has been modified. Save the changes before closing it?"),
    ("« {} » a été modifié. Enregistrer les modifications avant de quitter ?", "“{}” has been modified. Save the changes before quitting?"),
    ("Échec de la recherche : {}", "Check failed: {}"),
    ("Épais", "Thick"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// La langue est **globale** : deux épreuves qui la changent en même
    /// temps se marcheraient dessus. Elles passent donc l'une après l'autre.
    static SEUL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Prend la langue pour soi le temps d'une épreuve.
    fn seul() -> std::sync::MutexGuard<'static, ()> {
        SEUL.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

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
    fn to_english_ne_depend_pas_de_la_langue_choisie() {
        // Aucun `seul()` : la fonction ne lit pas l'état global, elle rend la
        // même chose pendant qu'une autre épreuve le change.
        assert_eq!(to_english("Terminer"), "Done");
        assert_eq!(to_english("Enregistrer sous"), "Save as");
        assert_eq!(
            to_english("Phrase absente de la table"),
            "Phrase absente de la table"
        );
    }

    /// Les info-bulles de la barre d'outils et des onglets ont leur anglais :
    /// une bascule qui dirait « Passer au thème sombre » à un anglophone
    /// passerait pour un oubli.
    #[test]
    fn les_bulles_de_la_barre_ont_leur_anglais() {
        for (fr, en) in [
            ("Passer au thème sombre", "Switch to dark theme"),
            ("Passer au thème clair", "Switch to light theme"),
            ("Afficher le panneau latéral", "Show side panel"),
            ("Masquer le panneau latéral", "Hide side panel"),
            ("Tous les outils", "All tools"),
            ("Masquer les outils", "Hide tools"),
            ("Fermer l'onglet", "Close the tab"),
            ("modifications non enregistrées", "unsaved changes"),
        ] {
            assert_eq!(to_english(fr), en);
        }
    }

    #[test]
    fn le_francais_se_rend_tel_quel() {
        let _seul = seul();
        apply(Lang::French, Lang::English);
        assert_eq!(tr("Terminer"), "Terminer");
        assert!(!english());
    }

    #[test]
    fn langlais_traduit_ce_quil_connait() {
        let _seul = seul();
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
        let _seul = seul();
        apply(Lang::Auto, Lang::English);
        assert!(english());
        apply(Lang::Auto, Lang::French);
        assert!(!english());
    }

    #[test]
    fn les_trous_se_remplissent_dans_les_deux_langues() {
        let _seul = seul();
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
