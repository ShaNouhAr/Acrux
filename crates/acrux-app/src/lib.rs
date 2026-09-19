//! Bibliothèque de l'application (séparée du `main` pour être testable).
//!
//! | Module     | Rôle                                                          |
//! |------------|---------------------------------------------------------------|
//! | `platform` | fenêtre, événements, présentation des pixels (Win32 pour l'instant) |
//! | `viewer`   | visualiseur : défilement, zoom, navigation, ouverture, barre d'état |
//! | `selection` | modèle de sélection de texte (curseurs, plages, rectangles)   |
//! | `ui`       | toolkit interne : texte d'interface, thèmes (widgets à venir) |
//! | `update`   | recherche des mises à jour auprès de la page des versions du projet |
//!
//! Modules prévus : `panels`, `tools`, `prefs`, `i18n`.

pub mod platform;
pub mod render_worker;
pub mod selection;
pub mod ui;
pub mod update;
pub mod viewer;
