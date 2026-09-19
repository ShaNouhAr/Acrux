//! Affichage d'une vidéo dans la page, et sa barre de commandes.
//!
//! L'image décodée n'est pas posée dans une fenêtre flottante : elle est
//! **composée dans le tampon de la page**, comme n'importe quel dessin. Elle
//! suit donc le défilement, le zoom et la découpe sans rien de particulier, et
//! une vidéo à moitié sortie de l'écran est à moitié dessinée — ce qu'une
//! fenêtre enfant ne sait pas faire.
//!
//! La mise à l'échelle est **bilinéaire**. Le plus proche voisin coûterait
//! deux fois moins cher et se verrait tout de suite : une vidéo réduite y
//! devient un grillage de pixels qui scintille dès que quelque chose bouge.

// Coordonnées d'écran entières et composantes 8 bits.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    clippy::too_many_arguments
)]

use crate::platform::Frame;
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Hauteur de la barre de commandes, en pixels logiques.
const BAR: f64 = 34.0;

/// Ce qu'un clic dans la vidéo demande.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hit {
    /// Démarrer ou mettre en pause.
    Toggle,
    /// Se placer à cette fraction de la durée, entre 0 et 1.
    Seek(f64),
    /// Dans l'image, mais pas sur une commande.
    Picture,
}

/// Rectangle occupé par la vidéo dans la vue, en pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Box2 {
    /// Bord gauche.
    pub x: f64,
    /// Bord haut.
    pub y: f64,
    /// Largeur.
    pub w: f64,
    /// Hauteur.
    pub h: f64,
}

impl Box2 {
    /// Vrai si le point est dedans.
    #[must_use]
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Ce qu'un clic vise dans la vidéo.
#[must_use]
pub fn hit(area: Box2, x: f64, y: f64, dpi: f64) -> Option<Hit> {
    if !area.contains(x, y) {
        return None;
    }
    let bar = (BAR * dpi).min(area.h / 3.0);
    let bar_top = area.y + area.h - bar;
    if y < bar_top {
        // Cliquer dans l'image fait ce que fait un lecteur vidéo : basculer.
        return Some(Hit::Toggle);
    }
    let button = bar;
    if x < area.x + button {
        return Some(Hit::Toggle);
    }
    // Le reste de la barre est la ligne de temps.
    let track = area.w - button - bar * 2.2;
    if track > 1.0 {
        let fraction = ((x - area.x - button) / track).clamp(0.0, 1.0);
        return Some(Hit::Seek(fraction));
    }
    Some(Hit::Picture)
}

/// Compose une image de vidéo dans le tampon, mise à l'échelle.
///
/// Les pixels arrivent en BGRA ; le tampon de la fenêtre est dans le même
/// ordre, ce qui évite une conversion par pixel.
pub fn paint_frame(frame: &mut Frame<'_>, area: Box2, source: &[u8], src_w: u32, src_h: u32) {
    if src_w == 0 || src_h == 0 || area.w < 1.0 || area.h < 1.0 {
        return;
    }
    let (dst_w, dst_h) = (area.w as i32, area.h as i32);
    let (x0, y0) = (area.x as i32, area.y as i32);
    let (sw, sh) = (src_w as i32, src_h as i32);
    // Rapport entre un pixel d'arrivée et un pixel de départ.
    let step_x = f64::from(sw) / area.w;
    let step_y = f64::from(sh) / area.h;

    for row in 0..dst_h {
        let dy = y0 + row;
        if dy < 0 || dy >= frame.height as i32 {
            continue;
        }
        // Le centre du pixel d'arrivée, ramené dans l'image de départ.
        let fy = ((f64::from(row) + 0.5) * step_y - 0.5).max(0.0);
        let sy = fy as i32;
        let ty = fy - f64::from(sy);
        let sy1 = (sy + 1).min(sh - 1);
        for col in 0..dst_w {
            let dx = x0 + col;
            if dx < 0 || dx >= frame.width as i32 {
                continue;
            }
            let fx = ((f64::from(col) + 0.5) * step_x - 0.5).max(0.0);
            let sx = fx as i32;
            let tx = fx - f64::from(sx);
            let sx1 = (sx + 1).min(sw - 1);

            let at = |x: i32, y: i32| -> usize { ((y * sw + x) * 4) as usize };
            let (a, b, c, d) = (at(sx, sy), at(sx1, sy), at(sx, sy1), at(sx1, sy1));
            if d + 3 >= source.len() {
                continue;
            }
            let mix = |i: usize| -> u8 {
                let top = f64::from(source[a + i]) * (1.0 - tx) + f64::from(source[b + i]) * tx;
                let bottom = f64::from(source[c + i]) * (1.0 - tx) + f64::from(source[d + i]) * tx;
                (top * (1.0 - ty) + bottom * ty).round().clamp(0.0, 255.0) as u8
            };
            let index = frame.index(dx as usize, dy as usize);
            let target = &mut frame.pixels[index..index + 4];
            target[0] = mix(0);
            target[1] = mix(1);
            target[2] = mix(2);
            target[3] = 255;
        }
    }
}

/// Dessine la barre de commandes par-dessus le bas de l'image.
pub fn paint_controls(
    frame: &mut Frame<'_>,
    text: &mut TextRenderer,
    theme: &Theme,
    dpi: f64,
    area: Box2,
    playing: bool,
    position: f64,
    duration: f64,
) {
    let bar = (BAR * dpi).min(area.h / 3.0);
    if bar < 8.0 {
        return;
    }
    let top = (area.y + area.h - bar) as i32;
    let left = area.x as i32;
    let width = area.w as i32;
    let height = bar as i32;
    // Un voile sombre : la barre se lit sur une image claire comme sur une
    // image sombre, sans masquer ce qui se passe derrière.
    veil(frame, left, top, width, height);

    let button = bar;
    // Le symbole lecture ou pause, dessiné au trait plein.
    let cx = area.x + button / 2.0;
    let cy = area.y + area.h - bar / 2.0;
    let s = bar * 0.28;
    if playing {
        let w = (s * 0.34).max(1.0);
        fill(frame, cx - s * 0.45, cy - s, w, s * 2.0);
        fill(frame, cx + s * 0.11, cy - s, w, s * 2.0);
    } else {
        // Un triangle, ligne par ligne.
        let rows = (s * 2.0) as i32;
        for row in 0..rows {
            let t = f64::from(row) / f64::from(rows.max(1));
            let half = (0.5 - (t - 0.5).abs()) * 2.0;
            let len = s * 1.5 * half;
            if len < 1.0 {
                continue;
            }
            fill(frame, cx - s * 0.6, cy - s + f64::from(row), len, 1.0);
        }
    }

    // Ligne de temps.
    let label_width = bar * 2.2;
    let track_x = area.x + button;
    let track_w = area.w - button - label_width;
    if track_w > 4.0 {
        let track_y = cy - (2.0 * dpi).max(1.0) / 2.0;
        let thickness = (3.0 * dpi).max(1.0);
        fill_rgb(frame, track_x, track_y, track_w, thickness, 200, 200, 210);
        let done = if duration > 0.0 {
            (position / duration).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (r, g, b) = theme.accent;
        fill_rgb(frame, track_x, track_y, track_w * done, thickness, r, g, b);
        // La poignée, un petit rond plein.
        let knob = (5.0 * dpi).max(2.0);
        fill_rgb(
            frame,
            track_x + track_w * done - knob / 2.0,
            cy - knob,
            knob,
            knob * 2.0,
            255,
            255,
            255,
        );
    }

    // Le temps écoulé sur la durée totale.
    #[allow(clippy::cast_possible_truncation)] // taille de police, quelques dizaines
    let size = (f64::from(theme.font_size) * dpi * 0.92) as f32;
    let label = format!("{} / {}", clock(position), clock(duration));
    let w = f64::from(text.measure(size, &label));
    text.draw(
        frame,
        (area.x + area.w - w - bar * 0.35) as f32,
        (cy + f64::from(text.ascent(size)) / 2.0 - 1.0) as f32,
        size,
        &label,
        (0xF2, 0xF2, 0xF4),
    );
}

/// Durée en `m:ss`, ou `h:mm:ss` au-delà d'une heure.
#[must_use]
pub fn clock(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "0:00".into();
    }
    let total = seconds as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Assombrit une bande du tampon.
fn veil(frame: &mut Frame<'_>, x: i32, y: i32, w: i32, h: i32) {
    for row in y..y + h {
        if row < 0 || row >= frame.height as i32 {
            continue;
        }
        for col in x..x + w {
            if col < 0 || col >= frame.width as i32 {
                continue;
            }
            let index = frame.index(col as usize, row as usize);
            let pixel = &mut frame.pixels[index..index + 3];
            for c in pixel {
                *c = (u32::from(*c) * 40 / 100) as u8;
            }
        }
    }
}

/// Rectangle blanc.
fn fill(frame: &mut Frame<'_>, x: f64, y: f64, w: f64, h: f64) {
    fill_rgb(frame, x, y, w, h, 255, 255, 255);
}

/// Rectangle d'une couleur donnée, aux bornes flottantes arrondies.
fn fill_rgb(frame: &mut Frame<'_>, x: f64, y: f64, w: f64, h: f64, r: u8, g: u8, b: u8) {
    if w < 0.5 || h < 0.5 {
        return;
    }
    frame.fill_rect(
        x.round() as i32,
        y.round() as i32,
        w.round().max(1.0) as i32,
        h.round().max(1.0) as i32,
        r,
        g,
        b,
    );
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::panic)]
mod tests {
    use super::{clock, hit, Box2, Hit};

    fn area() -> Box2 {
        Box2 {
            x: 100.0,
            y: 50.0,
            w: 400.0,
            h: 225.0,
        }
    }

    #[test]
    fn les_durees_sont_lisibles() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(9.4), "0:09");
        assert_eq!(clock(75.0), "1:15");
        assert_eq!(clock(3661.0), "1:01:01");
        assert_eq!(clock(f64::NAN), "0:00");
        assert_eq!(clock(-5.0), "0:00");
    }

    #[test]
    fn cliquer_dans_limage_bascule() {
        assert_eq!(hit(area(), 300.0, 100.0, 1.0), Some(Hit::Toggle));
        // Hors de la vidéo, rien.
        assert_eq!(hit(area(), 50.0, 100.0, 1.0), None);
        assert_eq!(hit(area(), 300.0, 400.0, 1.0), None);
    }

    #[test]
    fn la_barre_donne_le_bouton_puis_la_ligne_de_temps() {
        let a = area();
        let bas = a.y + a.h - 10.0;
        // Tout à gauche de la barre : le bouton.
        assert_eq!(hit(a, a.x + 5.0, bas, 1.0), Some(Hit::Toggle));
        // Au milieu de la ligne de temps : une fraction plausible.
        match hit(a, a.x + 200.0, bas, 1.0) {
            Some(Hit::Seek(f)) => assert!(f > 0.3 && f < 0.8, "fraction {f}"),
            autre => panic!("attendu un déplacement, reçu {autre:?}"),
        }
    }

    #[test]
    fn la_fraction_reste_entre_zero_et_un() {
        let a = area();
        let bas = a.y + a.h - 5.0;
        for x in [a.x, a.x + a.w - 1.0, a.x + a.w / 2.0] {
            if let Some(Hit::Seek(f)) = hit(a, x, bas, 1.0) {
                assert!((0.0..=1.0).contains(&f), "fraction {f} pour x {x}");
            }
        }
    }
}
