//! Couche plateforme : fenêtre, événements, présentation d'un tampon de
//! pixels, dialogues de fichiers. C'est le **seul** endroit du projet où
//! `unsafe` est autorisé (CHARTE_PROJET.md §1.2) : chaque appel au système
//! y est documenté avec l'invariant qui le rend sûr.
//!
//! Abstraction minuscule : une application implémente [`App`], la plateforme
//! lui livre des [`Event`] et lui demande de peindre dans un [`Frame`].

// Manipulation de pixels : coordonnées d'écran (i32) et dimensions (u32)
// mélangées comme le fait l'API système, composantes r, g, b, a à une lettre,
// et primitives de dessin qui prennent position, taille et couleur à plat.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    clippy::too_many_arguments
)]

use std::path::PathBuf;

pub mod http;
#[cfg(windows)]
pub mod media;
#[cfg(windows)]
pub mod win32;

/// Chaîne terminée par zéro, prête pour les API « W » de Windows.
///
/// Déclarée ici parce que [`http`] et [`win32`] en ont besoin toutes les deux.
#[must_use]
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Bouton de souris.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// Gauche.
    Left,
    /// Droit.
    Right,
    /// Milieu.
    Middle,
}

/// Touches non imprimables gérées par le visualiseur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Flèche haut.
    Up,
    /// Flèche bas.
    Down,
    /// Flèche gauche.
    Left,
    /// Flèche droite.
    Right,
    /// Page précédente.
    PageUp,
    /// Page suivante.
    PageDown,
    /// Début.
    Home,
    /// Fin.
    End,
    /// Échap.
    Escape,
    /// Entrée.
    Enter,
    /// Retour arrière.
    Backspace,
    /// Suppr.
    Delete,
    /// Tabulation.
    Tab,
    /// Barre d'espace : active le bouton ou la ligne qui a le focus.
    Space,
    /// Touche de fonction F1..F12.
    F(u8),
    /// Autre touche (code virtuel de la plateforme).
    Other(u32),
}

/// Forme du pointeur de souris.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cursor {
    /// Flèche.
    #[default]
    Arrow,
    /// Barre verticale (texte).
    IBeam,
    /// Main (lien, déplacement).
    Hand,
    /// Poser une zone de texte (dessiné : [`crate::ui::cursors`]).
    AddText,
    /// Surligner.
    Highlight,
    /// Poser une note.
    Note,
    /// Biffer.
    Redact,
    /// Déplacer un objet.
    Move,
    /// Dessiner au stylo.
    Pen,
    /// Poser un élément : signature, paraphe, marque.
    Place,
    /// Redimensionner en largeur (bords gauche et droit).
    ResizeWE,
    /// Redimensionner en hauteur (bords haut et bas).
    ResizeNS,
    /// Redimensionner en diagonale « ↘ » (coins haut-gauche, bas-droit).
    ResizeNWSE,
    /// Redimensionner en diagonale « ↗ » (coins haut-droit, bas-gauche).
    ResizeNESW,
}

impl Cursor {
    /// Nombre de pointeurs.
    pub const COUNT: usize = 14;
}

/// Modificateurs enfoncés.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    /// Ctrl.
    pub ctrl: bool,
    /// Maj.
    pub shift: bool,
    /// Alt.
    pub alt: bool,
}

/// Événement livré à l'application.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// La zone cliente a changé de taille (pixels physiques).
    Resize {
        /// Largeur.
        width: u32,
        /// Hauteur.
        height: u32,
    },
    /// Touche enfoncée.
    Key(Key, Modifiers),
    /// Caractère saisi.
    Char(char, Modifiers),
    /// Molette : `delta` positif = vers le haut, en crans (1 = 120 unités Windows).
    Wheel {
        /// Crans.
        delta: f32,
        /// Position x.
        x: i32,
        /// Position y.
        y: i32,
        /// Modificateurs.
        modifiers: Modifiers,
    },
    /// Bouton enfoncé.
    MouseDown {
        /// Bouton.
        button: MouseButton,
        /// Position x.
        x: i32,
        /// Position y.
        y: i32,
        /// Modificateurs.
        modifiers: Modifiers,
        /// 1 pour un clic simple, 2 pour un double-clic système.
        clicks: u8,
    },
    /// Bouton relâché.
    MouseUp {
        /// Bouton.
        button: MouseButton,
        /// Position x.
        x: i32,
        /// Position y.
        y: i32,
    },
    /// Déplacement de la souris.
    MouseMove {
        /// Position x.
        x: i32,
        /// Position y.
        y: i32,
        /// Bouton gauche enfoncé pendant le déplacement.
        dragging: bool,
    },
    /// Fichier déposé sur la fenêtre.
    FileDropped(PathBuf),
    /// Échelle DPI de la fenêtre (1.0 = 96 dpi).
    DpiChanged(f32),
    /// Réveil demandé par un autre fil via [`Waker::wake`] (résultat prêt).
    Wake,
    /// Demande de fermeture.
    Close,
}

/// Poignée transmissible à un autre fil pour réveiller la fenêtre : la
/// fenêtre recevra [`Event::Wake`] sur son propre fil.
pub trait Waker: Send {
    /// Réveille la fenêtre (sans bloquer, sûr depuis n'importe quel fil).
    fn wake(&self);
}

/// Tampon de pixels de la fenêtre (ou une sous-vue rectangulaire de
/// celui-ci), BGRA 8 bits, ligne 0 en haut. Les lignes sont espacées de
/// `stride` pixels dans `pixels` : une sous-vue partage le tampon de son
/// parent sans copie (voir [`Frame::sub`]).
pub struct Frame<'a> {
    /// Largeur en pixels.
    pub width: u32,
    /// Hauteur en pixels.
    pub height: u32,
    /// Pixels par ligne du tampon sous-jacent (≥ `width`).
    pub stride: u32,
    /// Pixels BGRA ; la ligne `y` commence à l'octet `y × stride × 4`.
    pub pixels: &'a mut [u8],
}

impl Frame<'_> {
    /// Tampon plein cadre (pas de ligne = largeur).
    pub fn new(width: u32, height: u32, pixels: &mut [u8]) -> Frame<'_> {
        Frame {
            width,
            height,
            stride: width,
            pixels,
        }
    }

    /// Octet du pixel `(x, y)` (non vérifié).
    #[must_use]
    pub fn index(&self, x: usize, y: usize) -> usize {
        (y * self.stride as usize + x) * 4
    }

    /// Sous-vue rectangulaire `(x, y, w, h)`, découpée aux bords ; vide si
    /// le rectangle est hors du tampon. Dessiner dedans dessine dans le
    /// parent, en coordonnées relatives au coin `(x, y)`.
    pub fn sub(&mut self, x: i32, y: i32, w: u32, h: u32) -> Frame<'_> {
        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let x1 = (x + w as i32).clamp(0, self.width as i32) as u32;
        let y1 = (y + h as i32).clamp(0, self.height as i32) as u32;
        if x1 <= x0 || y1 <= y0 {
            return Frame {
                width: 0,
                height: 0,
                stride: self.stride,
                pixels: &mut [],
            };
        }
        let start = self.index(x0 as usize, y0 as usize);
        let end = self.index(x1 as usize, (y1 - 1) as usize);
        Frame {
            width: x1 - x0,
            height: y1 - y0,
            stride: self.stride,
            pixels: &mut self.pixels[start..end],
        }
    }

    /// Remplit toute la (sous-)vue d'une couleur.
    pub fn clear(&mut self, r: u8, g: u8, b: u8) {
        let (w, h) = (self.width as i32, self.height as i32);
        self.fill_rect(0, 0, w, h, r, g, b);
    }

    /// Copie un bitmap RGBA prémultiplié (ligne 0 en haut) à la position donnée,
    /// composé sur le contenu existant, découpé aux bords.
    pub fn blit_rgba_premultiplied(&mut self, x: i32, y: i32, w: u32, h: u32, rgba: &[u8]) {
        for row in 0..h {
            let dy = y + i64::from(row) as i32;
            if dy < 0 || dy >= self.height as i32 {
                continue;
            }
            let src_row = &rgba[(row as usize) * (w as usize) * 4..];
            for col in 0..w {
                let dx = x + col as i32;
                if dx < 0 || dx >= self.width as i32 {
                    continue;
                }
                let s = &src_row[(col as usize) * 4..(col as usize) * 4 + 4];
                let di = self.index(dx as usize, dy as usize);
                let a = u32::from(s[3]);
                if a == 255 {
                    self.pixels[di] = s[2];
                    self.pixels[di + 1] = s[1];
                    self.pixels[di + 2] = s[0];
                    self.pixels[di + 3] = 255;
                } else if a > 0 {
                    let inv = 255 - a;
                    let d = &mut self.pixels[di..di + 4];
                    d[0] = ((u32::from(s[2]) * 255 + u32::from(d[0]) * inv) / 255) as u8;
                    d[1] = ((u32::from(s[1]) * 255 + u32::from(d[1]) * inv) / 255) as u8;
                    d[2] = ((u32::from(s[0]) * 255 + u32::from(d[2]) * inv) / 255) as u8;
                    d[3] = 255;
                }
            }
        }
    }

    /// Rectangle plein (pour les cadres, ombres, barres).
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, r: u8, g: u8, b: u8) {
        let x0 = x.max(0);
        let y0 = y.max(0);
        let x1 = (x + w).min(self.width as i32);
        let y1 = (y + h).min(self.height as i32);
        for yy in y0..y1 {
            for xx in x0..x1 {
                let i = self.index(xx as usize, yy as usize);
                self.pixels[i] = b;
                self.pixels[i + 1] = g;
                self.pixels[i + 2] = r;
                self.pixels[i + 3] = 255;
            }
        }
    }
}

/// Pages à imprimer : la plateforme choisit l'imprimante et les pages, puis
/// demande le rendu de chaque page à la résolution du périphérique.
pub trait PrintSource {
    /// Nombre de pages.
    fn page_count(&self) -> usize;
    /// Taille d'une page en points (largeur, hauteur), rotation comprise.
    fn page_size(&self, index: usize) -> (f64, f64);
    /// Rend une page à `dpi` points par pouce : (largeur, hauteur, RVB 8 bits
    /// sur fond blanc, ligne 0 en haut).
    fn render(&mut self, index: usize, dpi: f64) -> Option<(u32, u32, Vec<u8>)>;
}

/// Résultat d'une impression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrintOutcome {
    /// Nombre de pages envoyées.
    Printed(usize),
    /// Dialogue annulé.
    Cancelled,
    /// Échec (message).
    Failed(String),
}

/// Actions que l'application peut demander à la fenêtre.
pub trait WindowHandle {
    /// Change le titre.
    fn set_title(&mut self, title: &str);
    /// Demande un nouveau rendu.
    fn request_redraw(&mut self);
    /// Ouvre le dialogue « Ouvrir » et retourne le chemin choisi.
    fn open_file_dialog(&mut self) -> Option<PathBuf>;
    /// Ouvre le dialogue « Enregistrer sous » (nom proposé) et retourne le chemin choisi.
    fn save_file_dialog(&mut self, suggested: &str) -> Option<PathBuf>;
    /// Même dialogue, avec la liste des types proposés : `(libellé, extension)`.
    /// Le premier type est celui présélectionné et donne l'extension par défaut.
    fn save_file_dialog_as(&mut self, suggested: &str, types: &[(&str, &str)]) -> Option<PathBuf>;
    /// Question oui / non ; vrai si l'utilisateur confirme.
    fn confirm(&mut self, title: &str, message: &str) -> bool;
    /// Boîte de message d'erreur.
    fn show_error(&mut self, title: &str, message: &str);
    /// Ferme la fenêtre.
    fn close(&mut self);
    /// Poignée de réveil pour les fils de travail.
    fn waker(&self) -> Box<dyn Waker>;
    /// Change la forme du pointeur (appliquée jusqu'au prochain changement).
    fn set_cursor(&mut self, cursor: Cursor);

    /// Vrai si la fenêtre occupe tout l'écran de bureau (bouton « agrandir »).
    ///
    /// Une fenêtre agrandie doit être **rouverte agrandie** : rouvrir à sa
    /// taille en fenêtre ordinaire la ferait déborder de l'écran, et la
    /// moitié droite de l'interface se retrouverait hors de vue.
    fn maximised(&self) -> bool {
        false
    }
    /// Place du texte dans le presse-papiers du système.
    fn set_clipboard_text(&mut self, text: &str);
    /// Texte du presse-papiers du système, s'il en contient.
    fn clipboard_text(&mut self) -> Option<String>;
    /// Ouvre une adresse `http(s)`/`mailto` dans l'application par défaut.
    /// Les autres schémas sont refusés par la plateforme.
    fn open_url(&mut self, url: &str);
    /// Dialogue d'impression puis impression des pages choisies, chaque page
    /// ajustée à la zone imprimable en conservant ses proportions.
    fn print(&mut self, title: &str, source: &mut dyn PrintSource) -> PrintOutcome;
    /// Passe la fenêtre en plein écran (sans bordure, sur tout le moniteur)
    /// ou la restaure.
    fn set_fullscreen(&mut self, on: bool);
    /// Accorde la décoration de la fenêtre (barre de titre, bordure) au
    /// thème de l'application : sombre ou clair, et les couleurs exactes si
    /// le système sait les prendre.
    ///
    /// Sans cela, une application sombre garde une barre de titre blanche
    /// collée en haut, et l'illusion tombe.
    fn set_frame_theme(&mut self, dark: bool, caption: (u8, u8, u8), text: (u8, u8, u8)) {
        let _ = (dark, caption, text);
    }
}

/// Langue de l'interface du système, en deux lettres (`fr`, `en`, `de`…).
///
/// Sert à choisir la langue d'Acrux au premier lancement : l'utilisateur ne
/// devrait pas avoir à la régler pour lire son logiciel dans sa langue.
#[must_use]
pub fn system_language() -> String {
    #[cfg(windows)]
    {
        win32::system_language()
    }
    #[cfg(not(windows))]
    {
        // Convention POSIX : `fr_FR.UTF-8`, `en_GB`…
        std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LC_MESSAGES"))
            .or_else(|_| std::env::var("LANG"))
            .ok()
            .and_then(|v| v.get(..2).map(str::to_lowercase))
            .unwrap_or_else(|| "en".into())
    }
}

/// Remplit `out` d'octets du générateur cryptographique du système ; faux
/// s'il n'y en a pas.
///
/// L'aléa des clés de chiffrement (`acrux_document::crypt::random`) vient de
/// la bibliothèque standard, faute de pouvoir appeler le système hors de ce
/// module ; l'application le renforce en y branchant celui-ci au démarrage.
#[must_use]
pub fn system_random(out: &mut [u8]) -> bool {
    #[cfg(windows)]
    {
        win32::system_random(out)
    }
    #[cfg(not(windows))]
    {
        let _ = out;
        false
    }
}

/// Application pilotée par la plateforme.
pub trait App {
    /// Traite un événement.
    fn event(&mut self, event: Event, window: &mut dyn WindowHandle);
    /// Peint la fenêtre.
    fn paint(&mut self, frame: &mut Frame<'_>);
}

/// Lance la boucle d'événements avec la fenêtre principale. Ne retourne
/// qu'à la fermeture.
///
/// # Errors
/// Impossible de créer la fenêtre.
pub fn run(
    title: &str,
    width: u32,
    height: u32,
    maximised: bool,
    app: Box<dyn App>,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        win32::run(title, width, height, maximised, app)
    }
    #[cfg(not(windows))]
    {
        let _ = (title, width, height, maximised, app);
        Err("plateforme non prise en charge pour l'instant (Windows uniquement)".into())
    }
}

#[cfg(all(test, windows))]
mod tests {
    /// Le générateur du système répond, et deux tirages diffèrent.
    #[test]
    fn system_random_fills_differently() {
        let (mut a, mut b) = ([0u8; 32], [0u8; 32]);
        assert!(super::system_random(&mut a) && super::system_random(&mut b));
        assert_ne!(a, b);
    }
}
