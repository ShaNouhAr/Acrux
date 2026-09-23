//! Primitives de dessin de l'interface : coins arrondis, ombres douces,
//! aplats translucides.
//!
//! Tout le reste de l'interface est fait de rectangles francs, et cela se
//! voit : les angles vifs et les bords nets donnent l'air d'une maquette
//! plutôt que d'un logiciel fini. Ces trois fonctions suffisent à corriger
//! cela, et elles sont écrites ici une fois pour toutes — chaque panneau qui
//! les emploie hérite du même rendu.
//!
//! Le lissage vient d'une **fonction de distance** : pour chaque pixel, on
//! mesure sa distance au bord de la forme, et la couverture passe de 1 à 0
//! sur un pixel. C'est exact aux quatre coins comme sur les côtés, sans cas
//! particulier.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    // Un rectangle, un rayon, une couleur : les regrouper dans une structure
    // n'apprendrait rien et alourdirait chaque appel.
    clippy::too_many_arguments
)]

use crate::platform::Frame;

/// Couleur RVB.
pub type Rgb = (u8, u8, u8);

/// Distance signée d'un point au bord d'un rectangle arrondi.
///
/// Négative dedans, positive dehors, nulle sur le bord.
fn distance(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let dx = (px - cx).abs() - (w / 2.0 - r);
    let dy = (py - cy).abs() - (h / 2.0 - r);
    let outside = dx.max(0.0).hypot(dy.max(0.0));
    outside + dx.max(dy).min(0.0) - r
}

/// Compose une couleur sur un pixel, selon une couverture de 0 à 1.
fn blend(frame: &mut Frame<'_>, x: i32, y: i32, color: Rgb, alpha: f32) {
    if alpha <= 0.0 || x < 0 || y < 0 || x >= frame.width as i32 || y >= frame.height as i32 {
        return;
    }
    let a = alpha.min(1.0);
    let i = frame.index(x as usize, y as usize);
    let p = &mut frame.pixels[i..i + 4];
    let mix = |src: u8, dst: u8| (f32::from(src) * a + f32::from(dst) * (1.0 - a)) as u8;
    p[0] = mix(color.2, p[0]);
    p[1] = mix(color.1, p[1]);
    p[2] = mix(color.0, p[2]);
}

/// Rectangle aux coins arrondis, lissé.
pub fn round_rect(frame: &mut Frame<'_>, x: i32, y: i32, w: i32, h: i32, radius: f32, color: Rgb) {
    round_rect_alpha(frame, x, y, w, h, radius, color, 1.0);
}

/// Rectangle arrondi translucide (`alpha` de 0 à 1).
pub fn round_rect_alpha(
    frame: &mut Frame<'_>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    radius: f32,
    color: Rgb,
    alpha: f32,
) {
    if w <= 0 || h <= 0 || alpha <= 0.0 {
        return;
    }
    let (fx, fy, fw, fh) = (x as f32, y as f32, w as f32, h as f32);
    for py in y.max(0)..(y + h).min(frame.height as i32) {
        for px in x.max(0)..(x + w).min(frame.width as i32) {
            let d = distance(px as f32 + 0.5, py as f32 + 0.5, fx, fy, fw, fh, radius);
            let coverage = (0.5 - d).clamp(0.0, 1.0);
            blend(frame, px, py, color, coverage * alpha);
        }
    }
}

/// Contour d'un rectangle arrondi, d'une épaisseur donnée.
pub fn round_rect_outline(
    frame: &mut Frame<'_>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    radius: f32,
    thickness: f32,
    color: Rgb,
) {
    if w <= 0 || h <= 0 || thickness <= 0.0 {
        return;
    }
    let (fx, fy, fw, fh) = (x as f32, y as f32, w as f32, h as f32);
    let pad = thickness.ceil() as i32 + 1;
    for py in (y - pad).max(0)..(y + h + pad).min(frame.height as i32) {
        for px in (x - pad).max(0)..(x + w + pad).min(frame.width as i32) {
            let d = distance(px as f32 + 0.5, py as f32 + 0.5, fx, fy, fw, fh, radius);
            // Le trait est centré sur le bord : la distance en vaut la moitié.
            let coverage = (thickness / 2.0 - d.abs() + 0.5).clamp(0.0, 1.0);
            blend(frame, px, py, color, coverage);
        }
    }
}

/// Ombre portée douce sous un rectangle arrondi.
///
/// L'ombre est la même forme, étalée : la couverture décroît avec la distance
/// au bord, ce qui donne un dégradé continu sans avoir à flouter une image.
pub fn shadow(
    frame: &mut Frame<'_>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    radius: f32,
    spread: f32,
    strength: f32,
) {
    if w <= 0 || h <= 0 || spread <= 0.0 {
        return;
    }
    let (fx, fy, fw, fh) = (x as f32, y as f32, w as f32, h as f32);
    let pad = spread.ceil() as i32 + 1;
    for py in (y - pad).max(0)..(y + h + pad).min(frame.height as i32) {
        for px in (x - pad).max(0)..(x + w + pad).min(frame.width as i32) {
            let d = distance(px as f32 + 0.5, py as f32 + 0.5, fx, fy, fw, fh, radius);
            if d <= 0.0 {
                continue;
            }
            let t = (1.0 - d / spread).clamp(0.0, 1.0);
            // Courbe douce : le carré évite le halo trop net des dégradés
            // linéaires.
            blend(frame, px, py, (0, 0, 0), t * t * strength);
        }
    }
}

/// Trait lissé d'épaisseur `width`, aux bouts arrondis, de `(x0, y0)` à
/// `(x1, y1)` (pixels, à virgule).
///
/// Même principe que les rectangles : la distance de chaque pixel au
/// segment donne sa couverture. Sert aux petits dessins de l'interface — la
/// coche d'une case — qui n'ont pas besoin du rastériseur des pages.
pub fn line(frame: &mut Frame<'_>, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Rgb) {
    let half = width / 2.0;
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len2 = dx * dx + dy * dy;
    let pad = half + 1.0;
    let left = (x0.min(x1) - pad).floor() as i32;
    let right = (x0.max(x1) + pad).ceil() as i32;
    let top = (y0.min(y1) - pad).floor() as i32;
    let bottom = (y0.max(y1) + pad).ceil() as i32;
    for py in top.max(0)..bottom.min(frame.height as i32) {
        for px in left.max(0)..right.min(frame.width as i32) {
            let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
            // Point du segment le plus proche du centre du pixel.
            let t = if len2 > 0.0 {
                (((cx - x0) * dx + (cy - y0) * dy) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let d = (cx - (x0 + t * dx)).hypot(cy - (y0 + t * dy));
            blend(frame, px, py, color, (half + 0.5 - d).clamp(0.0, 1.0));
        }
    }
}

/// Ce qu'un bouton a à dire de lui-même pour être dessiné.
// Quatre états indépendants, qui se combinent : un bouton principal peut
// être survolé et avoir le focus. Une énumération les multiplierait.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonLook {
    /// Le bouton principal : plein, de la couleur d'accent, texte blanc.
    pub primary: bool,
    /// Le pointeur est dessus.
    pub hovered: bool,
    /// Il a le focus clavier : un anneau d'accent l'entoure.
    pub focused: bool,
    /// Il ne répond pas pour l'instant : estompé.
    pub disabled: bool,
}

/// Un bouton, **le même partout** : fenêtres, bandeau de recherche, barres.
///
/// Rend la couleur du texte à y écrire — l'appelant connaît son libellé et
/// sa police, pas ce module.
pub fn button(
    frame: &mut Frame<'_>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    dpi: f32,
    theme: &crate::ui::theme::Theme,
    look: ButtonLook,
) -> Rgb {
    let radius = 8.0 * dpi;
    let (bg, fg) = if look.disabled {
        (theme.bar, theme.text_dim)
    } else if look.primary {
        let a = theme.accent;
        let bg = if look.hovered {
            (
                a.0.saturating_add(18),
                a.1.saturating_add(18),
                a.2.saturating_add(10),
            )
        } else {
            a
        };
        (bg, (255, 255, 255))
    } else if look.hovered {
        (theme.separator, theme.text)
    } else {
        (theme.hover, theme.text)
    };
    if look.focused {
        let ring = (2.0 * dpi).max(1.0);
        let r = ring as i32;
        round_rect_outline(
            frame,
            x - r,
            y - r,
            w + 2 * r,
            h + 2 * r,
            radius + ring,
            ring,
            theme.accent,
        );
    }
    round_rect(frame, x, y, w, h, radius, bg);
    if look.disabled {
        round_rect_outline(frame, x, y, w, h, radius, dpi.max(1.0), theme.separator);
    }
    fg
}

/// Assombrit tout le cadre : ce qui est dessiné par-dessus se détache, et le
/// reste attend.
///
/// `progress` va de 0 (rien) à 1 (voile complet), ce qui sert aux fenêtres
/// qui apparaissent en fondu.
pub fn veil(frame: &mut Frame<'_>, progress: f32) {
    let keep = 100 - (55.0 * progress.clamp(0.0, 1.0)) as u32;
    for pixel in frame.pixels.chunks_exact_mut(4) {
        pixel[0] = (u32::from(pixel[0]) * keep / 100) as u8;
        pixel[1] = (u32::from(pixel[1]) * keep / 100) as u8;
        pixel[2] = (u32::from(pixel[2]) * keep / 100) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(buf: &mut [u8], w: u32, h: u32) -> Frame<'_> {
        Frame::new(w, h, buf)
    }

    #[test]
    fn les_coins_sont_evides_et_le_centre_plein() {
        let mut buf = vec![0_u8; 40 * 40 * 4];
        let mut f = frame(&mut buf, 40, 40);
        round_rect(&mut f, 0, 0, 40, 40, 10.0, (255, 255, 255));
        let at = |x: usize, y: usize| buf[(y * 40 + x) * 4 + 1];
        assert!(at(20, 20) > 250, "le centre est plein");
        assert_eq!(at(0, 0), 0, "le coin est vide");
        assert!(at(20, 0) > 250, "le milieu du bord est plein");
    }

    #[test]
    fn le_lissage_donne_des_valeurs_intermediaires() {
        let mut buf = vec![0_u8; 40 * 40 * 4];
        let mut f = frame(&mut buf, 40, 40);
        round_rect(&mut f, 0, 0, 40, 40, 12.0, (255, 255, 255));
        let partial = buf
            .chunks_exact(4)
            .filter(|p| p[1] > 10 && p[1] < 245)
            .count();
        assert!(partial > 20, "{partial} pixels de bord lissés");
    }

    #[test]
    fn lombre_sort_de_la_forme_sans_la_remplir() {
        let mut buf = vec![255_u8; 60 * 60 * 4];
        let mut f = frame(&mut buf, 60, 60);
        shadow(&mut f, 20, 20, 20, 20, 6.0, 8.0, 0.5);
        let at = |x: usize, y: usize| buf[(y * 60 + x) * 4 + 1];
        assert_eq!(at(30, 30), 255, "l'intérieur n'est pas assombri");
        assert!(at(30, 44) < 255, "le dessous l'est");
        assert_eq!(at(59, 59), 255, "le lointain ne l'est pas");
    }

    #[test]
    fn un_trait_couvre_son_segment_et_rien_dautre() {
        let mut buf = vec![0_u8; 20 * 20 * 4];
        let mut f = frame(&mut buf, 20, 20);
        line(&mut f, 2.0, 10.0, 18.0, 10.0, 2.0, (255, 255, 255));
        let at = |x: usize, y: usize| buf[(y * 20 + x) * 4 + 1];
        assert!(at(10, 10) > 250, "le milieu du trait est plein");
        assert_eq!(at(10, 3), 0, "loin du trait, rien");
        // Un trait hors du cadre ne déborde pas.
        let mut f = frame(&mut buf, 20, 20);
        line(&mut f, -30.0, -30.0, -10.0, -5.0, 3.0, (0, 0, 0));
    }

    #[test]
    fn un_rectangle_vide_ne_fait_rien() {
        let mut buf = vec![7_u8; 10 * 10 * 4];
        let mut f = frame(&mut buf, 10, 10);
        round_rect(&mut f, 0, 0, 0, 5, 2.0, (0, 0, 0));
        round_rect(&mut f, 0, 0, 5, -3, 2.0, (0, 0, 0));
        assert!(buf.iter().all(|b| *b == 7));
    }
}
