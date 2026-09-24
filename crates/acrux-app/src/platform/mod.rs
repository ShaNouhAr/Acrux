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
    /// Touche « menu contextuel » du clavier, entre AltGr et Ctrl droit :
    /// elle ouvre le menu du clic droit là où l'on travaille.
    ContextMenu,
    /// « + » de la rangée principale ou du pavé numérique, quelle que soit
    /// la disposition du clavier (Ctrl+Maj+Plus : tourner la vue).
    Plus,
    /// « - » de la rangée principale ou du pavé numérique, quelle que soit
    /// la disposition du clavier — en AZERTY, la touche du 6.
    Minus,
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
    /// Croix fine : tracer une forme, là où tombe exactement le coin.
    Cross,
}

impl Cursor {
    /// Nombre de pointeurs.
    pub const COUNT: usize = 15;
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
        /// Maj enfoncée : un geste de dessin se contraint (carré, cercle,
        /// angle multiple de 45°). Elle se lit à chaque mouvement, parce
        /// qu'on l'enfonce ou la relâche en plein geste.
        shift: bool,
    },
    /// Fichiers déposés sur la fenêtre, dans l'ordre où l'Explorateur les
    /// donne, au point `(x, y)` de la zone cliente : c'est ce point qui dit
    /// si l'on dépose sur les vignettes (une insertion) ou ailleurs (un
    /// onglet par fichier).
    FilesDropped {
        /// Chemins des fichiers.
        paths: Vec<PathBuf>,
        /// Position x du dépôt.
        x: i32,
        /// Position y du dépôt.
        y: i32,
    },
    /// Échelle DPI de la fenêtre (1.0 = 96 dpi).
    DpiChanged(f32),
    /// Réveil demandé par un autre fil via [`Waker::wake`] (résultat prêt).
    Wake,
    /// Vue précédente ou suivante : les boutons latéraux de la souris (4 et
    /// 5) ou les touches « Précédent » et « Suivant » d'un clavier
    /// multimédia.
    ///
    /// Un événement à part, et non un [`MouseButton`] de plus : tout ce qui
    /// prend `MouseDown` (outils, capture de signature, cartes) le prendrait
    /// pour un clic.
    Nav {
        /// Vers la vue suivante (bouton 5) plutôt que la précédente.
        forward: bool,
    },
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
                compose(&mut self.pixels[di..di + 4], s);
            }
        }
    }

    /// Étire un bitmap RGBA prémultiplié de `src_w` × `src_h` pixels sur le
    /// rectangle `dest` (x, y, largeur, hauteur), au plus proche voisin,
    /// composé sur le contenu existant et découpé aux bords.
    ///
    /// Seuls les pixels de destination **visibles** sont parcourus : le coût
    /// suit la taille du tampon, pas celle du rectangle — une page à 1600 %
    /// fait plus de dix mille pixels de large, et la parcourir en entier
    /// figerait la peinture. C'est l'aperçu d'une page le temps que son rendu
    /// à la bonne échelle arrive : il ne dure qu'un instant, le plus proche
    /// voisin suffit.
    pub fn blit_rgba_scaled(
        &mut self,
        dest: (i32, i32, u32, u32),
        src_w: u32,
        src_h: u32,
        rgba: &[u8],
    ) {
        let (x, y, w, h) = dest;
        let needed = (src_w as usize) * (src_h as usize) * 4;
        if w == 0 || h == 0 || src_w == 0 || src_h == 0 || rgba.len() < needed {
            return;
        }
        let x0 = x.max(0);
        let y0 = y.max(0);
        let x1 = (i64::from(x) + i64::from(w)).min(i64::from(self.width)) as i32;
        let y1 = (i64::from(y) + i64::from(h)).min(i64::from(self.height)) as i32;
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        // Rang dans la source d'une position dans la destination.
        let source = |offset: i32, dest: u32, src: u32| -> usize {
            let at = u64::from(offset.max(0) as u32) * u64::from(src) / u64::from(dest);
            at.min(u64::from(src - 1)) as usize
        };
        // La colonne source de chaque colonne visible, une fois pour toutes.
        let columns: Vec<usize> = (x0..x1).map(|dx| source(dx - x, w, src_w)).collect();
        let stride = src_w as usize * 4;
        for dy in y0..y1 {
            let sy = source(dy - y, h, src_h);
            let src_row = &rgba[sy * stride..(sy + 1) * stride];
            let start = self.index(x0 as usize, dy as usize);
            let row = &mut self.pixels[start..start + columns.len() * 4];
            for (d, &sx) in row.chunks_exact_mut(4).zip(&columns) {
                compose(d, &src_row[sx * 4..sx * 4 + 4]);
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

/// Compose un pixel RGBA prémultiplié `s` sur le pixel BGRA `d` du tampon.
fn compose(d: &mut [u8], s: &[u8]) {
    let a = u32::from(s[3]);
    if a == 255 {
        d[0] = s[2];
        d[1] = s[1];
        d[2] = s[0];
        d[3] = 255;
    } else if a > 0 {
        let inv = 255 - a;
        d[0] = ((u32::from(s[2]) * 255 + u32::from(d[0]) * inv) / 255) as u8;
        d[1] = ((u32::from(s[1]) * 255 + u32::from(d[1]) * inv) / 255) as u8;
        d[2] = ((u32::from(s[0]) * 255 + u32::from(d[2]) * inv) / 255) as u8;
        d[3] = 255;
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
    /// Image du presse-papiers du système, s'il en contient une, rendue
    /// comme les octets d'un **fichier** image (PNG ou BMP) : le décodeur
    /// d'images du moteur la lit comme un fichier ouvert.
    fn clipboard_image(&mut self) -> Option<Vec<u8>> {
        None
    }
    /// Dialogue « Ouvrir » limité aux images (PNG, JPEG, BMP, GIF, TIFF).
    fn open_image_dialog(&mut self) -> Option<PathBuf> {
        None
    }
    /// Dialogue « Ouvrir » à sélection multiple (PDF, images, textes), titré
    /// `title` : les fichiers choisis, dans l'ordre ; vide si l'on annule.
    /// Une plateforme qui ne sait pas choisir plusieurs fichiers en rend un.
    fn open_files_dialog(&mut self, title: &str) -> Vec<PathBuf> {
        let _ = title;
        self.open_file_dialog().into_iter().collect()
    }
    /// Ouvre une adresse `http(s)`/`mailto` dans l'application par défaut.
    /// Les autres schémas sont refusés par la plateforme.
    fn open_url(&mut self, url: &str);
    /// Montre un fichier dans le gestionnaire de fichiers du système, son
    /// dossier ouvert et le fichier sélectionné (son dossier seul, s'il
    /// n'existe plus). Rien n'est exécuté : c'est le dossier qui s'ouvre.
    fn reveal_in_folder(&mut self, path: &std::path::Path) {
        let _ = path;
    }
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

/// Décalage du fuseau horaire de la personne par rapport à UTC, en minutes
/// (+120 à Paris l'été) : l'heure d'un tampon dynamique est celle de sa
/// montre, pas celle de Greenwich.
#[must_use]
pub fn local_offset_minutes() -> i32 {
    #[cfg(windows)]
    {
        win32::local_offset_minutes()
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// Fait d'un DIB du presse-papiers (`CF_DIB`, `CF_DIBV5` : un
/// `BITMAPINFOHEADER` suivi des pixels) un fichier BMP complet, en lui
/// ajoutant l'en-tête de fichier de 14 octets.
///
/// Le seul champ délicat est `bfOffBits`, le début des pixels : après
/// l'en-tête d'image viennent, pour un en-tête de 40 octets en
/// `BI_BITFIELDS`, les trois masques de couleur (quatre en
/// `BI_ALPHABITFIELDS`), puis la palette d'une image de 8 bits ou moins.
/// Mal compté, l'image se décale de quelques pixels. Rend `None` pour un
/// DIB tronqué ou illisible.
#[must_use]
pub fn dib_to_bmp(dib: &[u8]) -> Option<Vec<u8>> {
    let u16_at = |at: usize| -> Option<u16> {
        let b = dib.get(at..at + 2)?;
        Some(u16::from_le_bytes([b[0], b[1]]))
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b = dib.get(at..at + 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let header = usize::try_from(u32_at(0)?).ok()?;
    if header < 12 || dib.len() < header {
        return None;
    }
    // En-tête « cœur » (12 octets) : dimensions sur 16 bits, palette de
    // triplets ; tous les autres commencent comme le BITMAPINFOHEADER.
    let (width, height, bits, compression, used) = if header == 12 {
        (
            u32::from(u16_at(4)?),
            u32::from(u16_at(6)?),
            u16_at(10)?,
            0,
            0,
        )
    } else {
        if header < 40 {
            return None;
        }
        let height = i32::from_le_bytes(u32_at(8)?.to_le_bytes()).unsigned_abs();
        (u32_at(4)?, height, u16_at(14)?, u32_at(16)?, u32_at(32)?)
    };
    if width == 0 || height == 0 || !matches!(bits, 1 | 4 | 8 | 16 | 24 | 32) {
        return None;
    }
    let masks = match (header, compression) {
        (40, 3) => 12,
        (40, 6) => 16,
        _ => 0,
    };
    let entry = if header == 12 { 3 } else { 4 };
    let colors = if used > 0 {
        usize::try_from(used).ok()?
    } else if bits <= 8 {
        1 << bits
    } else {
        0
    };
    let start = header + masks + entry * colors;
    // Sans compression, la taille des pixels se calcule : des lignes
    // alignées sur quatre octets. Un DIB plus court est tronqué.
    if matches!(compression, 0 | 3 | 6) {
        let row = (usize::try_from(width).ok()? * usize::from(bits)).div_ceil(32) * 4;
        let needed = row.checked_mul(usize::try_from(height).ok()?)?;
        if dib.len() < start.checked_add(needed)? {
            return None;
        }
    } else if dib.len() < start {
        return None;
    }
    let total = u32::try_from(14 + dib.len()).ok()?;
    let offset = u32::try_from(14 + start).ok()?;
    let mut out = Vec::with_capacity(14 + dib.len());
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(dib);
    Some(out)
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod dib_tests {
    use super::dib_to_bmp;

    /// En-tête BITMAPINFOHEADER de 40 octets.
    fn info(width: i32, height: i32, bits: u16, compression: u32, used: u32) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(&40u32.to_le_bytes());
        h.extend_from_slice(&width.to_le_bytes());
        h.extend_from_slice(&height.to_le_bytes());
        h.extend_from_slice(&1u16.to_le_bytes());
        h.extend_from_slice(&bits.to_le_bytes());
        h.extend_from_slice(&compression.to_le_bytes());
        h.extend_from_slice(&[0; 12]); // taille, résolutions
        h.extend_from_slice(&used.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes());
        h
    }

    /// Couleur du pixel (x, y) de l'image, compté du haut, telle que la
    /// voit tout le chemin du moteur : décodée, posée sur une page (200 pt
    /// de large, centrée en 300, 400), puis rendue.
    fn pixel(bmp: &[u8], x: u32, y: u32) -> [u8; 3] {
        use acrux_features::{create, edit_objects, stamp};
        let image = stamp::prepare_image(bmp).unwrap();
        let (w, h) = (f64::from(image.width()), f64::from(image.height()));
        let size = (200.0, 200.0 * h / w);
        let doc = create::new_document(&create::PageSetup::default()).unwrap();
        let pages = acrux_document::collect_pages(&doc).unwrap();
        let center = acrux_core::Point::new(300.0, 400.0);
        edit_objects::add_image(&doc, &pages[0], &image, center, size).unwrap();
        let pages = acrux_document::collect_pages(&doc).unwrap();
        let options = acrux_render::RenderOptions {
            background: Some(acrux_graphics::Color::WHITE),
            ..acrux_render::RenderOptions::default()
        };
        let bitmap = acrux_render::render_page(&doc, &pages[0], 1.0, &options).bitmap;
        let top = 400.0 + size.1 / 2.0;
        let (px, py) = (
            200.0 + (f64::from(x) + 0.5) * size.0 / w,
            top - (f64::from(y) + 0.5) * size.1 / h,
        );
        let height = pages[0].crop_box(&doc).height();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // dans la page
        let p = bitmap
            .pixel(px as u32, (height - py) as u32)
            .unwrap_or([0; 4]);
        [p[0], p[1], p[2]]
    }

    #[test]
    fn un_dib_24_bits_devient_un_bmp_lisible() {
        // 2 × 2, de bas en haut, lignes de 6 octets alignées sur 8.
        let mut dib = info(2, 2, 24, 0, 0);
        dib.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]); // bas : rouge, vert
        dib.extend_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0]); // haut : bleu, blanc
        let bmp = dib_to_bmp(&dib).unwrap();
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(u32::from_le_bytes([bmp[10], bmp[11], bmp[12], bmp[13]]), 54);
        assert_eq!(acrux_features::stamp::image_size(&bmp).unwrap(), (2, 2));
        assert_eq!(pixel(&bmp, 0, 0), [0, 0, 255]);
        assert_eq!(pixel(&bmp, 1, 1), [0, 255, 0]);
    }

    #[test]
    fn les_masques_dun_bitfields_decalent_les_pixels() {
        let mut dib = info(1, 1, 32, 3, 0);
        for mask in [0x00FF_0000u32, 0x0000_FF00, 0x0000_00FF] {
            dib.extend_from_slice(&mask.to_le_bytes());
        }
        dib.extend_from_slice(&[0x20, 0x40, 0xC0, 0x00]); // B G R X
        let bmp = dib_to_bmp(&dib).unwrap();
        assert_eq!(
            u32::from_le_bytes([bmp[10], bmp[11], bmp[12], bmp[13]]),
            14 + 40 + 12
        );
        assert_eq!(pixel(&bmp, 0, 0), [0xC0, 0x40, 0x20]);
    }

    #[test]
    fn une_palette_complete_suit_len_tete() {
        // 8 bits, biClrUsed nul : 256 couleurs de palette.
        let mut dib = info(1, 1, 8, 0, 0);
        for i in 0..=255u8 {
            dib.extend_from_slice(&[i, 255 - i, 7, 0]);
        }
        dib.extend_from_slice(&[3, 0, 0, 0]);
        let bmp = dib_to_bmp(&dib).unwrap();
        assert_eq!(
            u32::from_le_bytes([bmp[10], bmp[11], bmp[12], bmp[13]]),
            14 + 40 + 1024
        );
        assert_eq!(pixel(&bmp, 0, 0), [7, 252, 3]);
    }

    #[test]
    fn une_hauteur_negative_se_lit_de_haut_en_bas() {
        let mut dib = info(1, -2, 24, 0, 0);
        dib.extend_from_slice(&[0, 0, 255, 0]); // haut : rouge
        dib.extend_from_slice(&[255, 0, 0, 0]); // bas : bleu
        let bmp = dib_to_bmp(&dib).unwrap();
        assert_eq!(acrux_features::stamp::image_size(&bmp).unwrap(), (1, 2));
        assert_eq!(pixel(&bmp, 0, 0), [255, 0, 0]);
        assert_eq!(pixel(&bmp, 0, 1), [0, 0, 255]);
    }

    #[test]
    fn un_dib_tronque_est_refuse() {
        let mut dib = info(4, 4, 24, 0, 0);
        dib.extend_from_slice(&[0; 20]);
        assert!(dib_to_bmp(&dib).is_none());
        assert!(dib_to_bmp(&[40, 0, 0]).is_none());
        assert!(dib_to_bmp(&[]).is_none());
    }
}

#[cfg(test)]
mod frame_tests {
    use super::Frame;

    /// Une source 2 × 2 étirée en 4 × 4 depuis (-2, -2) dans un tampon
    /// 3 × 3 : seul le quart visible est peint, pris dans le pixel source
    /// du bas à droite, et rien ne déborde.
    #[test]
    fn blit_rgba_scaled_clips() {
        // Source : rouge, vert / bleu, blanc, opaques (RGBA).
        let src = [
            255, 0, 0, 255, 0, 255, 0, 255, //
            0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let mut pixels = vec![7_u8; 3 * 3 * 4];
        let mut frame = Frame::new(3, 3, &mut pixels);
        frame.blit_rgba_scaled((-2, -2, 4, 4), 2, 2, &src);
        for y in 0..3 {
            for x in 0..3 {
                let i = (y * 3 + x) * 4;
                let px = &pixels[i..i + 4];
                if x < 2 && y < 2 {
                    assert_eq!(px, [255, 255, 255, 255], "({x}, {y}) : blanc");
                } else {
                    assert_eq!(px, [7, 7, 7, 7], "({x}, {y}) : intact");
                }
            }
        }
        // Agrandir sans décalage : chaque pixel source couvre un bloc 2 × 2
        // (le tampon est en BGRA).
        let mut pixels = vec![0_u8; 4 * 4 * 4];
        let mut frame = Frame::new(4, 4, &mut pixels);
        frame.blit_rgba_scaled((0, 0, 4, 4), 2, 2, &src);
        // Une source trop courte pour ses dimensions n'est pas lue.
        frame.blit_rgba_scaled((0, 0, 4, 4), 3, 3, &src);
        assert_eq!(pixels[0..4], [0, 0, 255, 255], "rouge en haut à gauche");
        assert_eq!(pixels[(3 * 4 + 3) * 4..], [255, 255, 255, 255], "blanc");
        let bleu = (3 * 4) * 4;
        assert_eq!(pixels[bleu..bleu + 4], [255, 0, 0, 255], "bleu");
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
