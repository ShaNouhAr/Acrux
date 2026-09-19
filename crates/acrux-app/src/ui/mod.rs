//! Toolkit d'interface interne : tout est dessiné par le projet dans le
//! tampon de pixels de la fenêtre (pas de contrôles natifs, pas de web), pour
//! un rendu identique sur toutes les plateformes et des thèmes cohérents.
//!
//! | Module    | Rôle                                                  |
//! |-----------|-------------------------------------------------------|
//! | `text`    | rendu de texte d'interface avec une police système    |
//! | `theme`   | couleurs et dimensions                                |
//! | `input`   | champ de saisie sur une ligne                         |
//! | `icons`   | icônes vectorielles rasterisées à la demande          |
//! | `toolbar` | barre d'outils (boutons, champ de page, zoom)         |
//! | `panel`   | panneau latéral (vignettes, signets)                  |
//! | `prefs`   | réglages persistants et modes d'affichage             |
//! | `tabs`    | barre d'onglets (un par document ouvert)              |
//! | `palette` | palette de commandes filtrable (Ctrl+Maj+P)           |
//! | `sign`    | outil « remplir et signer » : barre et capture        |
//! | `objects` | outil « modifier » : boîte de sélection et poignées   |
//! | `video`   | image de vidéo composée dans la page, barre de commandes |
//!
//! À venir : panneaux latéraux, menus, info-bulles.

pub mod icons;
pub mod input;
pub mod objects;
pub mod palette;
pub mod panel;
pub mod prefs;
pub mod sign;
pub mod tabs;
pub mod text;
pub mod theme;
pub mod toolbar;
pub mod video;
