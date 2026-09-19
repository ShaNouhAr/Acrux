//! Point d'entrée de l'application native Acrux.
//!
//! Le fenêtrage passe par l'API du système derrière une abstraction minimale
//! (`platform`), les widgets sont dessinés par le projet.
//! Aucune technologie web n'est utilisée (voir CHARTE_PROJET.md §1.3).
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;

fn main() {
    // Tous les arguments sont des documents à ouvrir (un onglet chacun).
    let initial: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    // Taille de fenêtre de la session précédente (pixels logiques).
    let (width, height) = acrux_app::ui::prefs::Prefs::load().window;
    let viewer = acrux_app::viewer::Viewer::new(initial);
    if let Err(e) = acrux_app::platform::run("Acrux", width, height, Box::new(viewer)) {
        eprintln!("erreur : {e}");
        std::process::exit(1);
    }
}
