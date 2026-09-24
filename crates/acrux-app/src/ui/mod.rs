//! Toolkit d'interface interne : tout est dessiné par le projet dans le
//! tampon de pixels de la fenêtre (pas de contrôles natifs, pas de web), pour
//! un rendu identique sur toutes les plateformes et des thèmes cohérents.
//!
//! | Module    | Rôle                                                  |
//! |-----------|-------------------------------------------------------|
//! | `anim`    | valeurs animées et horloge : panneaux, fondus, défilement |
//! | `controls` | contrôles composés : creux, segments, pastilles, cases, jauge |
//! | `cursors` | pointeurs dessinés : texte ajouté, surligneur, note… |
//! | `dialog`  | fenêtres de dialogue dessinées : question, boutons    |
//! | `editpdf` | mode « Modifier le PDF » : saisie et barre du mode    |
//! | `modebar` | barre fine d'un outil en cours : nom, consigne, sortie |
//! | `text`    | rendu de texte d'interface avec une police système    |
//! | `theme`   | couleurs et dimensions                                |
//! | `lang`    | langue de l'interface : français ou anglais           |
//! | `menu`    | menu contextuel du clic droit et listes déroulantes : éléments, raccourcis, clavier |
//! | `modal`   | modèle commun des cartes modales : carte, fondu, rangée de boutons, invite |
//! | `input`   | champ de saisie sur une ligne, avec sélection (Ctrl+A, Maj+flèches) |
//! | `icons`   | icônes vectorielles rasterisées à la demande          |
//! | `toolbar` | barre d'outils (boutons, champ de page, zoom, annuler / rétablir, bascules) |
//! | `panel`   | panneau latéral (vignettes, signets)                  |
//! | `prefs`   | réglages persistants et modes d'affichage             |
//! | `settings` | fiche « Paramètres » : langue, apparence, mises à jour |
//! | `protect` | fenêtre « Protéger par mot de passe » : deux mots de passe, permissions |
//! | `tabs`    | barre d'onglets (un par document ouvert)              |
//! | `paint`   | coins arrondis, ombres douces, aplats translucides    |
//! | `palette` | palette de commandes filtrable (Ctrl+Maj+P)           |
//! | `pickers` | liste des polices du système et nuancier              |
//! | `signpanel` | panneau « remplir et signer » : signatures, encre  |
//! | `sign`    | outil « remplir et signer » : barre et capture        |
//! | `objects` | outil « modifier » : boîte de sélection et poignées   |
//! | `tools`   | barre latérale des outils, à droite                   |
//! | `video`   | image de vidéo composée dans la page, barre de commandes |
//! | `zoompicker` | liste du zoom : niveaux, ajustements, niveau tapé |
//!
//! À venir : info-bulles détaillées.

pub mod anim;
pub mod controls;
pub mod cursors;
pub mod dialog;
pub mod editpdf;
pub mod icons;
pub mod input;
pub mod lang;
pub mod menu;
pub mod modal;
pub mod modebar;
pub mod objects;
pub mod paint;
pub mod palette;
pub mod panel;
pub mod pickers;
pub mod prefs;
pub mod protect;
pub mod settings;
pub mod sign;
pub mod signpanel;
pub mod tabs;
pub mod text;
pub mod theme;
pub mod toolbar;
pub mod tools;
pub mod video;
pub mod zoompicker;
