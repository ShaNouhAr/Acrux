//! Toolkit d'interface interne : tout est dessiné par le projet dans le
//! tampon de pixels de la fenêtre (pas de contrôles natifs, pas de web), pour
//! un rendu identique sur toutes les plateformes et des thèmes cohérents.
//!
//! | Module    | Rôle                                                  |
//! |-----------|-------------------------------------------------------|
//! | `anim`    | valeurs animées et horloge : panneaux, fondus, défilement |
//! | `cursors` | pointeurs dessinés : texte ajouté, surligneur, note… |
//! | `dialog`  | fenêtres de dialogue dessinées : question, boutons    |
//! | `editpdf` | mode « Modifier le PDF » : saisie et barre du mode    |
//! | `modebar` | barre fine d'un outil en cours : nom, consigne, sortie |
//! | `text`    | rendu de texte d'interface avec une police système    |
//! | `theme`   | couleurs et dimensions                                |
//! | `input`   | champ de saisie sur une ligne                         |
//! | `icons`   | icônes vectorielles rasterisées à la demande          |
//! | `toolbar` | barre d'outils (boutons, champ de page, zoom)         |
//! | `panel`   | panneau latéral (vignettes, signets)                  |
//! | `prefs`   | réglages persistants et modes d'affichage             |
//! | `tabs`    | barre d'onglets (un par document ouvert)              |
//! | `paint`   | coins arrondis, ombres douces, aplats translucides    |
//! | `palette` | palette de commandes filtrable (Ctrl+Maj+P)           |
//! | `signpanel` | panneau « remplir et signer » : signatures, encre  |
//! | `sign`    | outil « remplir et signer » : barre et capture        |
//! | `objects` | outil « modifier » : boîte de sélection et poignées   |
//! | `tools`   | barre latérale des outils, à droite                   |
//! | `video`   | image de vidéo composée dans la page, barre de commandes |
//!
//! À venir : menus, info-bulles détaillées.

pub mod anim;
pub mod cursors;
pub mod dialog;
pub mod editpdf;
pub mod icons;
pub mod input;
pub mod modebar;
pub mod objects;
pub mod paint;
pub mod palette;
pub mod panel;
pub mod prefs;
pub mod sign;
pub mod signpanel;
pub mod tabs;
pub mod text;
pub mod theme;
pub mod toolbar;
pub mod tools;
pub mod video;
