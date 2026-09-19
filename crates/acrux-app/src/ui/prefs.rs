//! Préférences de l'application, conservées entre deux sessions dans un
//! fichier texte (`%APPDATA%\Acrux\prefs.txt` sous Windows,
//! `$XDG_CONFIG_HOME/acrux/prefs.txt` ou `~/.config/…` ailleurs).
//!
//! Le format est volontairement trivial — une ligne `clé=valeur` par
//! réglage, les clés inconnues sont ignorées et les valeurs illisibles
//! retombent sur la valeur par défaut — pour qu'un utilisateur puisse le
//! lire et le corriger à la main, et pour qu'une version future ne casse
//! jamais le fichier d'une version antérieure.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Disposition des pages dans la vue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// Défilement continu, une page par rangée.
    #[default]
    Continuous,
    /// Une seule page à la fois.
    Single,
    /// Défilement continu, deux pages par rangée.
    ContinuousTwoUp,
    /// Deux pages à la fois.
    TwoUp,
}

impl ViewMode {
    /// Deux pages par rangée.
    #[must_use]
    pub const fn is_two_up(self) -> bool {
        matches!(self, ViewMode::ContinuousTwoUp | ViewMode::TwoUp)
    }

    /// Une seule rangée visible à la fois (pas de défilement entre les pages).
    #[must_use]
    pub const fn is_paged(self) -> bool {
        matches!(self, ViewMode::Single | ViewMode::TwoUp)
    }

    /// Mode suivant dans le cycle du bouton de la barre d'outils.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            ViewMode::Continuous => ViewMode::Single,
            ViewMode::Single => ViewMode::ContinuousTwoUp,
            ViewMode::ContinuousTwoUp => ViewMode::TwoUp,
            ViewMode::TwoUp => ViewMode::Continuous,
        }
    }

    /// Libellé court pour la barre d'état.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ViewMode::Continuous => "continu",
            ViewMode::Single => "page unique",
            ViewMode::ContinuousTwoUp => "continu, deux pages",
            ViewMode::TwoUp => "deux pages",
        }
    }

    fn key(self) -> &'static str {
        match self {
            ViewMode::Continuous => "continuous",
            ViewMode::Single => "single",
            ViewMode::ContinuousTwoUp => "continuous-two-up",
            ViewMode::TwoUp => "two-up",
        }
    }

    fn from_key(s: &str) -> Option<Self> {
        match s {
            "continuous" => Some(ViewMode::Continuous),
            "single" => Some(ViewMode::Single),
            "continuous-two-up" => Some(ViewMode::ContinuousTwoUp),
            "two-up" => Some(ViewMode::TwoUp),
            _ => None,
        }
    }
}

/// Nombre maximal de fichiers récents conservés.
pub const MAX_RECENT: usize = 10;

/// Réglages persistants.
// Plusieurs booléens indépendants : ce sont des cases à cocher de l'interface,
// les regrouper dans un type dédié n'apporterait rien au lecteur.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    /// Thème sombre (faux = clair).
    pub dark_theme: bool,
    /// Disposition des pages.
    pub view_mode: ViewMode,
    /// Panneau latéral ouvert.
    pub panel_open: bool,
    /// Ajuster à la largeur au lieu d'un zoom fixe.
    pub fit_width: bool,
    /// Zoom utilisé quand `fit_width` est faux (1.0 = 100 %).
    pub zoom: f64,
    /// Première page seule en mode deux pages (page de couverture).
    pub two_up_cover: bool,
    /// Taille de la fenêtre au dernier arrêt, en pixels logiques.
    pub window: (u32, u32),
    /// Vérifier l'existence d'une version plus récente au démarrage, au plus
    /// une fois par jour.
    pub check_updates: bool,
    /// Jour de la dernière vérification, en jours depuis 1970.
    pub last_update_check: u64,
    /// Signature enregistrée de « remplir et signer », sous sa forme texte.
    pub signature: Option<String>,
    /// Paraphe enregistré, même forme.
    pub initials: Option<String>,
    /// Documents ouverts récemment, du plus récent au plus ancien.
    pub recent: Vec<PathBuf>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            dark_theme: true,
            view_mode: ViewMode::default(),
            panel_open: false,
            fit_width: true,
            zoom: 1.0,
            two_up_cover: false,
            window: (1100, 900),
            check_updates: true,
            last_update_check: 0,
            signature: None,
            initials: None,
            recent: Vec::new(),
        }
    }
}

/// Dossier des préférences (créé au besoin par [`Prefs::save`]).
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return Some(PathBuf::from(appdata).join("Acrux"));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join("acrux"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("acrux"))
}

/// Chemin du fichier de préférences.
#[must_use]
pub fn config_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("prefs.txt"))
}

impl Prefs {
    /// Charge les préférences ; valeurs par défaut si le fichier est absent
    /// ou illisible (jamais d'erreur : un réglage perdu ne doit pas empêcher
    /// l'application de démarrer).
    #[must_use]
    pub fn load() -> Self {
        let Some(path) = config_file() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        Self::parse(&text)
    }

    /// Analyse le contenu d'un fichier de préférences.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut p = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "theme" => p.dark_theme = value != "light",
                "view" => {
                    if let Some(v) = ViewMode::from_key(value) {
                        p.view_mode = v;
                    }
                }
                "panel" => p.panel_open = value == "1" || value == "true",
                "fit-width" => p.fit_width = value == "1" || value == "true",
                "zoom" => {
                    if let Ok(z) = value.parse::<f64>() {
                        if z.is_finite() && (0.05..=16.0).contains(&z) {
                            p.zoom = z;
                        }
                    }
                }
                "two-up-cover" => p.two_up_cover = value == "1" || value == "true",
                "window" => {
                    if let Some((w, h)) = value.split_once('x') {
                        if let (Ok(w), Ok(h)) = (w.trim().parse(), h.trim().parse()) {
                            if (200..=20_000).contains(&w) && (200..=20_000).contains(&h) {
                                p.window = (w, h);
                            }
                        }
                    }
                }
                "mises-a-jour" => p.check_updates = value == "1" || value == "true",
                "derniere-verification" => {
                    if let Ok(day) = value.parse::<u64>() {
                        p.last_update_check = day;
                    }
                }
                "signature" if !value.is_empty() => p.signature = Some(value.to_string()),
                "initials" if !value.is_empty() => p.initials = Some(value.to_string()),
                "recent" if !value.is_empty() && p.recent.len() < MAX_RECENT => {
                    p.recent.push(PathBuf::from(value));
                }
                _ => {}
            }
        }
        p
    }

    /// Texte du fichier de préférences.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::from(
            "# Préférences Acrux. Une ligne « clé=valeur » par réglage ;\n\
             # les lignes inconnues sont ignorées, un fichier absent = valeurs par défaut.\n",
        );
        let theme = if self.dark_theme { "dark" } else { "light" };
        let _ = writeln!(out, "theme={theme}");
        let _ = writeln!(out, "view={}", self.view_mode.key());
        let _ = writeln!(out, "panel={}", u8::from(self.panel_open));
        let _ = writeln!(out, "fit-width={}", u8::from(self.fit_width));
        let _ = writeln!(out, "zoom={}", self.zoom);
        let _ = writeln!(out, "two-up-cover={}", u8::from(self.two_up_cover));
        let _ = writeln!(out, "window={}x{}", self.window.0, self.window.1);
        let _ = writeln!(out, "mises-a-jour={}", u8::from(self.check_updates));
        let _ = writeln!(out, "derniere-verification={}", self.last_update_check);
        if let Some(value) = &self.signature {
            let _ = writeln!(out, "signature={value}");
        }
        if let Some(value) = &self.initials {
            let _ = writeln!(out, "initials={value}");
        }
        for path in self.recent.iter().take(MAX_RECENT) {
            let _ = writeln!(out, "recent={}", path.display());
        }
        out
    }

    /// Enregistre les préférences (silencieux en cas d'échec : un disque plein
    /// ne doit pas interrompre l'utilisateur).
    pub fn save(&self) {
        let Some(dir) = config_dir() else { return };
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let _ = std::fs::write(dir.join("prefs.txt"), self.to_text());
    }

    /// Place `path` en tête des fichiers récents (sans doublon, liste bornée).
    pub fn push_recent(&mut self, path: &Path) {
        let path = path.to_path_buf();
        self.recent.retain(|p| p != &path);
        self.recent.insert(0, path);
        self.recent.truncate(MAX_RECENT);
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // tests : valeurs écrites puis relues à l'identique
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_every_setting() {
        let mut p = Prefs {
            dark_theme: false,
            view_mode: ViewMode::TwoUp,
            panel_open: true,
            fit_width: false,
            zoom: 1.25,
            two_up_cover: true,
            window: (1440, 960),
            check_updates: false,
            last_update_check: 20_350,
            signature: Some("typed:Élise Marchand".into()),
            initials: Some("drawn:1.0,2.0 3.0,4.0".into()),
            recent: vec![PathBuf::from(r"C:\docs\a.pdf"), PathBuf::from(r"C:\b.pdf")],
        };
        p.push_recent(Path::new(r"C:\b.pdf"));
        assert_eq!(p.recent[0], PathBuf::from(r"C:\b.pdf"));
        assert_eq!(p.recent.len(), 2, "pas de doublon");
        let back = Prefs::parse(&p.to_text());
        assert_eq!(back, p);
    }

    #[test]
    fn unknown_or_broken_lines_fall_back_to_defaults() {
        let p = Prefs::parse(
            "# commentaire\n\
             couleur=rose\n\
             theme=light\n\
             view=hélicoïdal\n\
             zoom=beaucoup\n\
             window=0x0\n\
             panel=1\n\
             \n\
             sans-signe-egal\n",
        );
        assert!(!p.dark_theme, "theme=light est lu");
        assert!(p.panel_open);
        assert_eq!(p.view_mode, ViewMode::Continuous, "valeur inconnue ignorée");
        assert_eq!(p.zoom, 1.0);
        assert_eq!(p.window, (1100, 900), "taille absurde refusée");
    }

    #[test]
    fn recent_list_is_bounded() {
        let mut p = Prefs::default();
        for i in 0..MAX_RECENT + 5 {
            p.push_recent(Path::new(&format!("C:/f{i}.pdf")));
        }
        assert_eq!(p.recent.len(), MAX_RECENT);
        assert_eq!(p.recent[0], PathBuf::from("C:/f14.pdf"));
        let back = Prefs::parse(&p.to_text());
        assert_eq!(back.recent.len(), MAX_RECENT);
    }

    #[test]
    fn view_mode_cycle_and_flags() {
        let mut m = ViewMode::Continuous;
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(m);
            m = m.next();
        }
        assert_eq!(m, ViewMode::Continuous, "cycle complet");
        assert_eq!(seen.len(), 4);
        assert!(ViewMode::TwoUp.is_two_up() && ViewMode::TwoUp.is_paged());
        assert!(!ViewMode::Continuous.is_two_up() && !ViewMode::Continuous.is_paged());
        assert!(ViewMode::ContinuousTwoUp.is_two_up() && !ViewMode::ContinuousTwoUp.is_paged());
    }
}
