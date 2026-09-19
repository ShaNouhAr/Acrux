//! Raccordement au script de compilation d'un exécutable.
//!
//! Un `build.rs` appelle [`emit`], qui écrit le fichier `.res` dans le dossier
//! de sortie de Cargo et demande à l'éditeur de liens de l'incorporer. Sur une
//! cible qui n'est pas Windows/MSVC, la fonction ne fait rien : la compilation
//! croisée reste possible sans condition dans les scripts appelants.

use std::path::{Path, PathBuf};

use crate::{Resources, VersionInfo};

/// Raison pour laquelle les ressources n'ont pas pu être produites.
#[derive(Debug)]
pub enum BuildError {
    /// Cargo n'a pas fourni `OUT_DIR` : la fonction est appelée hors `build.rs`.
    NoOutDir,
    /// Le fichier d'icône est illisible.
    Icon(crate::Error),
    /// Lecture ou écriture impossible.
    Io(std::io::Error),
}

impl core::fmt::Display for BuildError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoOutDir => f.write_str("OUT_DIR absent : appel hors d'un script de compilation"),
            Self::Icon(error) => write!(f, "icône : {error}"),
            Self::Io(error) => write!(f, "entrée-sortie : {error}"),
        }
    }
}

impl std::error::Error for BuildError {}

impl From<std::io::Error> for BuildError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<crate::Error> for BuildError {
    fn from(error: crate::Error) -> Self {
        Self::Icon(error)
    }
}

/// Incorpore une icône et un bloc de version dans l'exécutable `binary`.
///
/// `icon` peut ne pas exister : la compilation se poursuit alors sans icône,
/// avec un avertissement Cargo. C'est volontaire, le dossier `branding/` n'est
/// pas indispensable pour construire le logiciel.
///
/// # Errors
///
/// Renvoie [`BuildError`] si `OUT_DIR` manque, si le fichier d'icône présent
/// est illisible, ou si l'écriture du `.res` échoue.
pub fn emit(binary: &str, icon: &Path, version: &VersionInfo<'_>) -> Result<(), BuildError> {
    println!("cargo:rerun-if-changed={}", icon.display());
    if !is_windows_msvc() {
        return Ok(());
    }

    let mut resources = Resources::new();
    match std::fs::read(icon) {
        Ok(bytes) => resources.add_icon(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "cargo:warning=icône absente ({}), l'exécutable gardera l'icône par défaut",
                icon.display()
            );
        }
        Err(error) => return Err(error.into()),
    }
    resources.add_version(version);

    let out_dir = std::env::var_os("OUT_DIR").ok_or(BuildError::NoOutDir)?;
    let path = PathBuf::from(out_dir).join(format!("{binary}.res"));
    std::fs::write(&path, resources.into_bytes())?;
    // L'éditeur de liens MSVC accepte un .res parmi ses fichiers d'entrée.
    println!("cargo:rustc-link-arg-bin={binary}={}", path.display());
    Ok(())
}

/// Vrai si la cible de compilation est Windows avec la chaîne d'outils MSVC.
fn is_windows_msvc() -> bool {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    os == "windows" && env == "msvc"
}
