//! Ce que l'installateur fait vraiment : écrire les fichiers, poser les
//! raccourcis, s'inscrire auprès de Windows, et savoir tout défaire.
//!
//! Tout se passe dans le compte de l'utilisateur : `%LOCALAPPDATA%\Programs`
//! pour les fichiers, `HKEY_CURRENT_USER` pour le registre. **Aucun droit
//! d'administrateur n'est demandé**, et rien n'est touché qui appartienne aux
//! autres comptes de la machine — c'est ce qui permet d'installer Acrux sur un
//! poste de travail verrouillé.

use std::path::{Path, PathBuf};

use crate::payload::Entry;
use crate::win;

/// Clé d'inscription d'Acrux dans la liste « Applications installées ».
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Acrux";
/// Identifiant de type de document, pour « Ouvrir avec ».
const PROGID: &str = "Acrux.Document";

/// Ce que l'utilisateur a choisi dans la fenêtre.
pub struct Options {
    /// Dossier d'installation.
    pub folder: PathBuf,
    /// Raccourci sur le Bureau.
    pub desktop: bool,
    /// Proposer Acrux dans « Ouvrir avec » pour les PDF.
    pub associate: bool,
}

impl Options {
    /// Choix par défaut : le dossier des programmes de l'utilisateur.
    #[must_use]
    pub fn new() -> Self {
        Options {
            folder: default_folder(),
            desktop: true,
            associate: true,
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Self::new()
    }
}

/// Dossier d'installation par défaut.
#[must_use]
pub fn default_folder() -> PathBuf {
    win::known_folder(win::CSIDL_LOCAL_APPDATA)
        .unwrap_or_else(|| PathBuf::from(r"C:\Acrux"))
        .join("Programs")
        .join("Acrux")
}

/// Une étape de l'installation, pour la barre de progression.
pub enum Step {
    /// Message et avancement de 0 à 1.
    Progress(String, f32),
}

/// Installe les fichiers et déclare le programme au système.
///
/// `report` est appelé à chaque étape ; l'appelant s'en sert pour la barre de
/// progression.
///
/// # Errors
/// Écriture refusée, archive illisible, registre inaccessible.
pub fn install(
    entries: &[Entry],
    options: &Options,
    version: &str,
    report: &mut dyn FnMut(Step),
) -> Result<(), String> {
    // Un chemin donné à la main peut mélanger les séparateurs ; tout ce qui
    // finira dans le registre et dans les raccourcis doit être présentable.
    let folder = &normalise(&options.folder);
    std::fs::create_dir_all(folder).map_err(|e| format!("{} : {e}", folder.display()))?;

    let total = entries.len().max(1);
    let mut written = 0u64;
    for (index, entry) in entries.iter().enumerate() {
        let destination = safe_join(folder, &entry.name)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{} : {e}", parent.display()))?;
        }
        write_replacing(&destination, &entry.data)?;
        written += entry.data.len() as u64;
        #[allow(clippy::cast_precision_loss)] // compte de fichiers, très petit
        report(Step::Progress(
            format!("Copie de {}", entry.name),
            (index + 1) as f32 / total as f32 * 0.8,
        ));
    }

    // L'installateur se recopie sur place : c'est lui qui saura désinstaller,
    // et il est le seul à contenir ce qu'il faut pour cela.
    let uninstaller = folder.join("desinstaller.exe");
    if let Ok(current) = std::env::current_exe() {
        if current != uninstaller {
            let bytes =
                std::fs::read(&current).map_err(|e| format!("{} : {e}", current.display()))?;
            write_replacing(&uninstaller, &bytes)?;
            written += bytes.len() as u64;
        }
    }

    let exe = folder.join("acrux.exe");
    let icon = folder.join("acrux.exe");
    report(Step::Progress("Raccourcis".into(), 0.85));
    if let Some(programs) = win::known_folder(win::CSIDL_PROGRAMS) {
        let link = programs.join("Acrux.lnk");
        win::create_shortcut(&link, &exe, "Acrux — lecteur et éditeur PDF", Some(&icon))?;
    }
    if options.desktop {
        if let Some(desktop) = win::known_folder(win::CSIDL_DESKTOPDIRECTORY) {
            let link = desktop.join("Acrux.lnk");
            win::create_shortcut(&link, &exe, "Acrux — lecteur et éditeur PDF", Some(&icon))?;
        }
    }

    report(Step::Progress("Inscription".into(), 0.92));
    register(folder, &exe, &uninstaller, version, written)?;
    if options.associate {
        associate(&exe)?;
    }
    // Le dossier retient ses propres choix : la désinstallation saura quoi
    // défaire, et une mise à jour saura quoi reprendre.
    let note = format!(
        "version={version}\ndesktop={}\nassociate={}\n",
        u8::from(options.desktop),
        u8::from(options.associate)
    );
    let _ = std::fs::write(folder.join("installation.txt"), note);
    report(Step::Progress("Terminé".into(), 1.0));
    Ok(())
}

/// Écrit un fichier, en contournant le cas du fichier verrouillé.
///
/// Remplacer `acrux.exe` pendant qu'Acrux tourne échoue : Windows ne permet
/// pas d'écrire par-dessus un exécutable en cours. On renomme alors l'ancien
/// de côté — ce que Windows **autorise** — et on écrit le neuf à sa place ;
/// le vieux fichier disparaîtra au prochain démarrage. C'est ce qui rend une
/// mise à jour possible sans demander de fermer le programme.
fn write_replacing(path: &Path, data: &[u8]) -> Result<(), String> {
    match std::fs::write(path, data) {
        Ok(()) => Ok(()),
        Err(first) => {
            let aside = path.with_extension("ancien");
            let _ = std::fs::remove_file(&aside);
            if std::fs::rename(path, &aside).is_err() {
                return Err(format!("{} : {first}", path.display()));
            }
            std::fs::write(path, data).map_err(|e| format!("{} : {e}", path.display()))
        }
    }
}

/// Joint un chemin relatif au dossier d'installation, en refusant tout ce qui
/// en sortirait.
///
/// Une archive est une donnée : un nom comme `..\..\Windows\System32\x.dll`
/// ne doit jamais pouvoir écrire ailleurs que sous le dossier choisi.
fn safe_join(folder: &Path, name: &str) -> Result<PathBuf, String> {
    let mut out = folder.to_path_buf();
    for part in name.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.contains(':') {
            return Err(format!("nom de fichier refusé : {name}"));
        }
        out.push(part);
    }
    Ok(out)
}

/// Chemin sans séparateurs mélangés, tel que Windows l'écrit.
fn normalise(path: &Path) -> PathBuf {
    PathBuf::from(path.display().to_string().replace('/', "\\"))
}

/// Inscrit Acrux dans « Applications installées ».
fn register(
    folder: &Path,
    exe: &Path,
    uninstaller: &Path,
    version: &str,
    size: u64,
) -> Result<(), String> {
    let uninstall = format!("\"{}\" --uninstall", uninstaller.display());
    let folder_text = folder.display().to_string();
    let icon = exe.display().to_string();
    win::write_key(
        UNINSTALL_KEY,
        &[
            ("DisplayName", win::Value::Text("Acrux")),
            ("DisplayVersion", win::Value::Text(version)),
            ("Publisher", win::Value::Text("Contributeurs Acrux")),
            ("DisplayIcon", win::Value::Text(&icon)),
            ("UninstallString", win::Value::Text(&uninstall)),
            (
                "QuietUninstallString",
                win::Value::Text(&format!("{uninstall} --silent")),
            ),
            ("InstallLocation", win::Value::Text(&folder_text)),
            ("URLInfoAbout", win::Value::Text(crate::HOMEPAGE)),
            ("NoModify", win::Value::Number(1)),
            ("NoRepair", win::Value::Number(1)),
            // Windows affiche la taille en kilo-octets.
            (
                "EstimatedSize",
                win::Value::Number(u32::try_from(size / 1024).unwrap_or(0)),
            ),
        ],
    )
}

/// Propose Acrux dans « Ouvrir avec » pour les fichiers PDF.
///
/// Sans jamais s'imposer comme application par défaut : voler l'association
/// d'un type de fichier dans le dos de l'utilisateur est un procédé de
/// logiciel indélicat, et Windows le bloque de toute façon.
fn associate(exe: &Path) -> Result<(), String> {
    let command = format!("\"{}\" \"%1\"", exe.display());
    let icon = format!("{},0", exe.display());
    win::write_key(
        &format!(r"Software\Classes\{PROGID}"),
        &[("", win::Value::Text("Document PDF"))],
    )?;
    win::write_key(
        &format!(r"Software\Classes\{PROGID}\DefaultIcon"),
        &[("", win::Value::Text(&icon))],
    )?;
    win::write_key(
        &format!(r"Software\Classes\{PROGID}\shell\open\command"),
        &[("", win::Value::Text(&command))],
    )?;
    win::write_key(
        r"Software\Classes\.pdf\OpenWithProgids",
        &[(PROGID, win::Value::Text(""))],
    )?;
    win::write_key(
        r"Software\Acrux\Capabilities",
        &[
            ("ApplicationName", win::Value::Text("Acrux")),
            (
                "ApplicationDescription",
                win::Value::Text("Lecteur et éditeur PDF"),
            ),
        ],
    )?;
    win::write_key(
        r"Software\Acrux\Capabilities\FileAssociations",
        &[(".pdf", win::Value::Text(PROGID))],
    )
}

/// Retire tout ce que [`install`] a posé.
///
/// Les préférences de l'utilisateur (`%APPDATA%\Acrux`) sont **laissées en
/// place** : elles lui appartiennent, et une réinstallation les retrouve.
///
/// # Errors
/// Jamais : la désinstallation avance malgré les fichiers déjà absents et
/// rend la liste de ce qui n'a pas pu être retiré.
pub fn uninstall(folder: &Path) -> Vec<String> {
    let mut trouble = Vec::new();
    for name in ["Acrux.lnk"] {
        for base in [
            win::known_folder(win::CSIDL_PROGRAMS),
            win::known_folder(win::CSIDL_DESKTOPDIRECTORY),
        ]
        .into_iter()
        .flatten()
        {
            let link = base.join(name);
            if link.exists() {
                if let Err(e) = std::fs::remove_file(&link) {
                    trouble.push(format!("{} : {e}", link.display()));
                }
            }
        }
    }
    win::delete_key(UNINSTALL_KEY);
    // Les préférences restent, la trace dans le registre part.
    win::delete_key(&format!(r"Software\Classes\{PROGID}"));
    win::delete_key(r"Software\Acrux\Capabilities");
    // Le dossier part en dernier : l'exécutable en cours y vit encore, donc
    // on retire ce qu'on peut et on demande au système d'achever au
    // redémarrage ce qui résiste.
    remove_folder(folder);
    trouble
}

/// Vide et retire un dossier, sans s'arrêter au premier fichier verrouillé.
///
/// Ne signale rien : ce qui résiste est le programme de désinstallation
/// lui-même, et Windows s'en chargera au prochain démarrage.
fn remove_folder(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let current = std::env::current_exe().unwrap_or_default();
    let mut left = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path == current {
            left += 1;
            continue;
        }
        let outcome = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if outcome.is_err() {
            left += 1;
        }
    }
    if left == 0 {
        let _ = std::fs::remove_dir(folder);
        return;
    }
    // Ce qui reste, c'est le programme de désinstallation lui-même, en train
    // de tourner. Windows sait le supprimer au prochain démarrage.
    win::delete_on_reboot(&current);
    win::delete_on_reboot(folder);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::safe_join;
    use std::path::Path;

    #[test]
    fn un_nom_darchive_ne_sort_pas_du_dossier() {
        let base = Path::new(r"C:\Programs\Acrux");
        assert_eq!(
            safe_join(base, "acrux.exe").unwrap(),
            base.join("acrux.exe")
        );
        assert_eq!(
            safe_join(base, "docs/lisez-moi.txt").unwrap(),
            base.join("docs").join("lisez-moi.txt")
        );
        for mauvais in [
            "../evasion.exe",
            r"..\..\Windows\System32\x.dll",
            r"C:\Windows\x.dll",
            "docs/../../dehors.txt",
        ] {
            assert!(safe_join(base, mauvais).is_err(), "{mauvais} accepté");
        }
    }
}
