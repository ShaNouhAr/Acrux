//! Recherche et installation des mises à jour.
//!
//! Acrux demande à la page des versions du projet, sur GitHub, s'il existe
//! une version plus récente que la sienne. C'est tout ce qui sort de la
//! machine : une requête `GET`, sans identifiant, sans statistique, sans rien
//! d'autre que l'adresse du dépôt public. Le réglage `mises-a-jour=0` dans les
//! préférences la supprime entièrement, et la recherche n'a lieu **qu'une fois
//! par jour** au plus.
//!
//! Rien n'est jamais installé sans que l'utilisateur l'ait demandé : trouver
//! une version ne fait qu'afficher une phrase dans la barre d'état.
//!
//! # Ce qui est lu
//!
//! La réponse de GitHub est du JSON. Plutôt que d'y chercher des morceaux de
//! texte à l'aveugle — ce qui casse au premier champ qui contient une
//! accolade — [`json`] en fait une vraie lecture : chaînes échappées,
//! objets et tableaux imbriqués.

use crate::platform::http;

mod json;

/// Dépôt interrogé. Change avec le projet, pas avec l'utilisateur.
pub const REPO: &str = env!("ACRUX_REPO");

/// Taille maximale acceptée pour la réponse : une liste de versions tient
/// très largement dedans, et une réponse aberrante ne remplira pas la mémoire.
const MAX_ANSWER: usize = 512 * 1024;

/// Une version publiée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Numéro de version, sans le `v` initial.
    pub version: String,
    /// Adresse de l'installateur, si la version en publie un.
    pub installer: Option<String>,
    /// Adresse de la page de la version, pour l'ouvrir dans le navigateur.
    pub page: String,
}

/// Compare deux versions « majeur.mineur.correctif ».
///
/// Les parties non numériques valent zéro : une version étrange ne fait pas
/// croire à tort qu'une mise à jour existe.
#[must_use]
pub fn newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| -> [u64; 3] {
        let mut out = [0u64; 3];
        for (slot, piece) in out.iter_mut().zip(v.trim_start_matches('v').split('.')) {
            let digits: String = piece.chars().take_while(char::is_ascii_digit).collect();
            *slot = digits.parse().unwrap_or(0);
        }
        out
    };
    parts(candidate) > parts(current)
}

/// Interroge GitHub et rend la dernière version publiée.
///
/// # Errors
/// Réseau indisponible, réponse illisible, ou dépôt sans version publiée.
pub fn latest() -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = http::get(&url, MAX_ANSWER)?;
    parse(&body)
}

/// Lit la réponse de GitHub.
///
/// # Errors
/// JSON illisible ou sans numéro de version.
pub fn parse(body: &[u8]) -> Result<Release, String> {
    let text = std::str::from_utf8(body).map_err(|_| String::from("réponse illisible"))?;
    let value = json::parse(text).ok_or_else(|| String::from("réponse mal formée"))?;
    let version = value
        .get("tag_name")
        .and_then(json::Value::as_str)
        .ok_or_else(|| String::from("réponse sans numéro de version"))?
        .trim_start_matches('v')
        .to_string();
    let page = value
        .get("html_url")
        .and_then(json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let installer = value
        .get("assets")
        .and_then(json::Value::as_array)
        .and_then(|assets| {
            assets.iter().find_map(|asset| {
                let name = asset.get("name").and_then(json::Value::as_str)?;
                name.to_ascii_lowercase()
                    .ends_with(".exe")
                    .then(|| asset.get("browser_download_url")?.as_str())
                    .flatten()
                    .map(str::to_string)
            })
        });
    Ok(Release {
        version,
        installer,
        page,
    })
}

/// Télécharge un installateur dans le dossier temporaire et rend son chemin.
///
/// L'adresse doit venir de [`latest`] : elle est vérifiée pour appartenir au
/// domaine de publication de GitHub, afin qu'une réponse détournée ne puisse
/// pas faire télécharger n'importe quoi.
///
/// # Errors
/// Adresse hors du domaine attendu, réseau indisponible, écriture impossible.
pub fn download(url: &str, version: &str) -> Result<std::path::PathBuf, String> {
    if !url.starts_with("https://github.com/")
        && !url.starts_with("https://objects.githubusercontent.com/")
    {
        return Err(String::from(
            "l'installateur proposé ne vient pas de GitHub : téléchargement refusé",
        ));
    }
    // 256 Mio : très au-delà de ce que pèse l'installateur, très en deçà de ce
    // qui remplirait un disque.
    let bytes = http::get(url, 256 * 1024 * 1024)?;
    if bytes.len() < 1024 {
        return Err(String::from("installateur téléchargé trop petit"));
    }
    let path = std::env::temp_dir().join(format!("acrux-setup-{version}.exe"));
    std::fs::write(&path, &bytes).map_err(|e| format!("{} : {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{newer, parse};

    #[test]
    fn les_versions_se_comparent_par_nombres() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("1.0.0", "0.9.9"));
        assert!(newer("0.1.10", "0.1.9"), "dix vient après neuf");
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.0.9", "0.1.0"));
        assert!(newer("v0.2.0", "0.1.0"), "le v initial est ignoré");
        // Une version inattendue ne doit pas faire croire à une nouveauté.
        assert!(!newer("bizarre", "0.1.0"));
    }

    #[test]
    fn la_reponse_de_github_est_lue() {
        let body = br#"{
            "tag_name": "v0.3.1",
            "name": "Acrux 0.3.1",
            "html_url": "https://github.com/x/Acrux/releases/tag/v0.3.1",
            "body": "Des accolades { } et des \"guillemets\" dans le texte",
            "assets": [
                {"name": "acrux-0.3.1-portable.zip",
                 "browser_download_url": "https://github.com/x/Acrux/releases/download/v0.3.1/p.zip"},
                {"name": "acrux-setup.exe",
                 "browser_download_url": "https://github.com/x/Acrux/releases/download/v0.3.1/acrux-setup.exe"}
            ]
        }"#;
        let release = parse(body).expect("lecture");
        assert_eq!(release.version, "0.3.1");
        assert_eq!(
            release.installer.as_deref(),
            Some("https://github.com/x/Acrux/releases/download/v0.3.1/acrux-setup.exe")
        );
        assert!(release.page.ends_with("v0.3.1"));
    }

    #[test]
    fn une_reponse_sans_version_est_refusee() {
        assert!(parse(b"{}").is_err());
        assert!(parse(b"pas du json").is_err());
        assert!(parse(b"{\"tag_name\": 12}").is_err());
    }

    #[test]
    fn un_telechargement_hors_github_est_refuse() {
        assert!(super::download("https://exemple.fr/piege.exe", "1.0").is_err());
        assert!(super::download("http://github.com/x.exe", "1.0").is_err());
    }
}
