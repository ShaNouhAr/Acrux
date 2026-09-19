//! `acrux-setup` — l'installateur d'Acrux, écrit comme le reste : à la main,
//! sans outil tiers ni bibliothèque.
//!
//! Un seul fichier à télécharger, qui porte le programme et ses fichiers
//! ([`payload`]), montre une fenêtre native ([`win`]), écrit dans le compte de
//! l'utilisateur sans jamais demander les droits d'administrateur
//! ([`install`]), et sait tout défaire.
//!
//! # Les trois façons de l'appeler
//!
//! ```text
//! acrux-setup.exe                       la fenêtre d'installation
//! acrux-setup.exe --silent [--dir D]    installation sans rien afficher
//! acrux-setup.exe --uninstall [--silent] désinstallation
//! ```
//!
//! Et une quatrième, réservée à la fabrication de la release :
//!
//! ```text
//! acrux-setup.exe --pack --out dist/acrux-setup.exe acrux.exe=target/release/acrux.exe …
//! ```
//!
//! qui recopie le programme lui-même en y collant l'archive des fichiers
//! donnés. C'est le même exécutable qui charge et qui installe : rien à
//! maintenir en double.

#![cfg_attr(not(windows), allow(dead_code))]

/// Adresse du projet, inscrite dans « Applications installées ».
pub const HOMEPAGE: &str = "https://github.com/ShaNouhAr/Acrux";

mod payload;

#[cfg(windows)]
mod install;
#[cfg(windows)]
mod ui;
#[cfg(windows)]
mod win;

/// Version installée, celle du paquet.
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--pack") {
        if let Err(message) = pack(&args) {
            eprintln!("acrux-setup : {message}");
            std::process::exit(1);
        }
        return;
    }
    run(&args);
}

/// Mode fabrication : recopie le programme en y collant l'archive.
fn pack(args: &[String]) -> Result<(), String> {
    let out = value(args, "--out").ok_or_else(|| String::from("--pack attend --out <fichier>"))?;
    let stub = match value(args, "--stub") {
        Some(path) => std::path::PathBuf::from(path),
        None => std::env::current_exe().map_err(|e| format!("programme courant : {e}"))?,
    };
    let mut files = Vec::new();
    for arg in args {
        if arg.starts_with("--") {
            continue;
        }
        let Some((name, path)) = arg.split_once('=') else {
            continue;
        };
        files.push((name.to_string(), std::path::PathBuf::from(path)));
    }
    if files.is_empty() {
        return Err(String::from(
            "aucun fichier : donner des paires nom=chemin, par exemple acrux.exe=target/release/acrux.exe",
        ));
    }
    let archive = payload::pack(&files)?;
    let out = std::path::PathBuf::from(out);
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    payload::write_self_extracting(&stub, &archive, &out)?;
    let size = std::fs::metadata(&out).map_or(0, |m| m.len());
    println!(
        "installateur écrit : {} ({} fichiers, {} Kio)",
        out.display(),
        files.len(),
        size / 1024
    );
    Ok(())
}

/// Valeur d'une option `--nom valeur`.
fn value<'a>(args: &'a [String], name: &str) -> Option<&'a String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
}

#[cfg(windows)]
fn run(args: &[String]) {
    let silent = args.iter().any(|a| a == "--silent" || a == "/S");
    if args.iter().any(|a| a == "--uninstall") {
        let folder = value(args, "--dir").map_or_else(
            || {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
                    .unwrap_or_else(install::default_folder)
            },
            std::path::PathBuf::from,
        );
        if !silent
            && !win::confirm(
                std::ptr::null_mut(),
                "Désinstaller Acrux ?\n\nVos préférences et vos documents ne seront pas touchés.",
            )
        {
            return;
        }
        let trouble = install::uninstall(&folder);
        if !silent && !trouble.is_empty() {
            win::error_box(std::ptr::null_mut(), &trouble.join("\n"));
        }
        return;
    }

    let Some(entries) = payload::read_self() else {
        let message = "Ce programme ne contient aucun fichier à installer.\n\n\
                       Téléchargez l'installateur depuis la page des versions du projet.";
        if silent {
            eprintln!("acrux-setup : {message}");
            std::process::exit(1);
        }
        win::error_box(std::ptr::null_mut(), message);
        return;
    };

    if silent {
        let mut options = install::Options::new();
        if let Some(dir) = value(args, "--dir") {
            options.folder = std::path::PathBuf::from(dir);
        }
        // Une installation silencieuse sert aux mises à jour : elle ne
        // redemande ni raccourcis ni association, elle reprend ce que
        // l'installation précédente avait choisi.
        let previous = read_choices(&options.folder);
        options.desktop = previous.0;
        options.associate = previous.1;
        match install::install(&entries, &options, VERSION, &mut |_| {}) {
            Ok(()) => println!("Acrux {VERSION} installé dans {}", options.folder.display()),
            Err(message) => {
                eprintln!("acrux-setup : {message}");
                std::process::exit(1);
            }
        }
        return;
    }
    ui::show(entries, VERSION);
}

/// Choix retenus par une installation précédente, ou les choix par défaut.
#[cfg(windows)]
fn read_choices(folder: &std::path::Path) -> (bool, bool) {
    let Ok(text) = std::fs::read_to_string(folder.join("installation.txt")) else {
        return (true, true);
    };
    let flag = |key: &str, fallback: bool| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .map_or(fallback, |v| v.trim() == "1")
    };
    (flag("desktop=", true), flag("associate=", true))
}

#[cfg(not(windows))]
fn run(_args: &[String]) {
    eprintln!(
        "acrux-setup installe Acrux sur Windows.\n\
         Sur les autres systèmes, construisez depuis les sources : cargo build --release"
    );
}
