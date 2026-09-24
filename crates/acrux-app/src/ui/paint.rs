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
    round_rect_outline_f(
        frame, x as f32, y as f32, w as f32, h as f32, radius, thickness, color,
    );
}

/// Contour d'un rectangle arrondi aux coordonnées à virgule. Le trait est
/// centré sur le contour : pour qu'un anneau d'épaisseur impaire (3 pixels à
/// 150 %) tombe pile sur les pixels au lieu d'être à cheval sur deux, son
/// contour doit pouvoir passer entre deux pixels.
fn round_rect_outline_f(
    frame: &mut Frame<'_>,
    fx: f32,
    fy: f32,
    fw: f32,
    fh: f32,
    radius: f32,
    thickness: f32,
    color: Rgb,
) {
    if fw <= 0.0 || fh <= 0.0 || thickness <= 0.0 {
        return;
    }
    let (x, y) = (fx.floor() as i32, fy.floor() as i32);
    let (w, h) = (fw.ceil() as i32 + 1, fh.ceil() as i32 + 1);
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
///
/// La forme elle-même est assombrie de plein droit. Les cartes posent leur
/// ombre **décalée vers le bas** : laisser l'intérieur intact faisait
/// apparaître, sous chaque carte, une bande plus claire que l'ombre qui
/// l'entoure — un liseré gris là où l'ombre est la plus dense.
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
            let t = (1.0 - d.max(0.0) / spread).clamp(0.0, 1.0);
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
// Cinq états indépendants, qui se combinent : un bouton principal peut
// être survolé, enfoncé et avoir le focus. Une énumération les multiplierait.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ButtonLook {
    /// Le bouton principal : plein, de la couleur d'accent, texte blanc.
    pub primary: bool,
    /// Le pointeur est dessus.
    pub hovered: bool,
    /// Il a le focus clavier : un anneau d'accent l'entoure, à deux pixels
    /// de distance — sans cet écart, l'anneau se confondrait avec le bouton
    /// principal, de la même couleur que lui.
    pub focused: bool,
    /// Il ne répond pas pour l'instant : estompé.
    pub disabled: bool,
    /// Le bouton est enfoncé, pointeur dessus : il n'agira qu'au relâchement,
    /// et le montre en s'enfonçant.
    pub pressed: bool,
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
        let bg = if look.pressed {
            (
                a.0.saturating_sub(14),
                a.1.saturating_sub(14),
                a.2.saturating_sub(14),
            )
        } else if look.hovered {
            (
                a.0.saturating_add(18),
                a.1.saturating_add(18),
                a.2.saturating_add(10),
            )
        } else {
            a
        };
        (bg, (255, 255, 255))
    } else if look.pressed {
        // Enfoncé, le bouton secondaire rentre dans son fond : à mi-chemin
        // entre le repos et la carte.
        (mix(theme.hover, theme.bar), theme.text)
    } else if look.hovered {
        (theme.button_hover, theme.text)
    } else {
        (theme.hover, theme.text)
    };
    if look.focused {
        focus_ring(frame, x, y, w, h, radius, dpi, theme.accent);
    }
    round_rect(frame, x, y, w, h, radius, bg);
    if look.disabled {
        round_rect_outline(frame, x, y, w, h, radius, dpi.max(1.0), theme.separator);
    }
    fg
}

/// Anneau de focus autour d'un rectangle arrondi, **à deux pixels** de lui.
///
/// L'écart n'est pas peint : le fond sur lequel l'élément est posé y reste
/// visible, et c'est lui qui sépare l'anneau de l'élément. Un anneau collé
/// au bouton principal, de la même couleur d'accent, ne se voyait pas.
pub fn focus_ring(
    frame: &mut Frame<'_>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    radius: f32,
    dpi: f32,
    color: Rgb,
) {
    let gap = (2.0 * dpi).round().max(1.0) as i32;
    let ring = (2.0 * dpi).round().max(1.0);
    // Le trait est centré sur le contour qu'on lui donne : on écarte ce
    // contour d'une demi-épaisseur de plus, pour que le bord intérieur du
    // trait tombe juste à `gap` pixels de l'élément.
    let half = ring / 2.0;
    let o = gap as f32 + half;
    let (ox, oy) = (x as f32 - o, y as f32 - o);
    round_rect_outline_f(
        frame,
        ox,
        oy,
        w as f32 + 2.0 * o,
        h as f32 + 2.0 * o,
        radius + o,
        ring,
        color,
    );
}

/// Anneau de focus tracé **à l'intérieur** d'un rectangle arrondi : son bord
/// extérieur suit celui de l'élément, au même rayon.
///
/// C'est celui des boutons de barre, posés à deux pixels les uns des autres :
/// l'anneau extérieur de [`focus_ring`] mordrait sur le voisin. Ces boutons
/// n'ont pas de fond au repos, rien ne le cache donc à l'intérieur.
pub fn inner_ring(
    frame: &mut Frame<'_>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    radius: f32,
    dpi: f32,
    color: Rgb,
) {
    let ring = (2.0 * dpi).round().max(1.0);
    // Même principe que `focus_ring` : le trait est centré sur le contour,
    // qu'on rentre d'une demi-épaisseur pour que le bord extérieur du trait
    // tombe pile sur celui de l'élément.
    let half = ring / 2.0;
    round_rect_outline_f(
        frame,
        x as f32 + half,
        y as f32 + half,
        w as f32 - ring,
        h as f32 - ring,
        (radius - half).max(0.0),
        ring,
        color,
    );
}

/// Moyenne de deux couleurs.
fn mix(a: Rgb, b: Rgb) -> Rgb {
    (a.0.midpoint(b.0), a.1.midpoint(b.1), a.2.midpoint(b.2))
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
    use crate::ui::theme::Theme;

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
    fn lombre_est_pleine_sous_la_forme_et_seteint_autour() {
        let mut buf = vec![255_u8; 60 * 60 * 4];
        let mut f = frame(&mut buf, 60, 60);
        shadow(&mut f, 20, 20, 20, 20, 6.0, 8.0, 0.5);
        let at = |x: usize, y: usize| buf[(y * 60 + x) * 4 + 1];
        // Sous la forme, l'ombre est à pleine force : une carte posée plus
        // haut laisse voir ce dessous, qui ne doit pas trancher en clair.
        assert_eq!(at(30, 30), 127, "le dessous de la forme est assombri");
        assert!(
            at(30, 44) < 255 && at(30, 44) > at(30, 30),
            "le bord s'estompe"
        );
        assert_eq!(at(59, 59), 255, "le lointain ne l'est pas");
    }

    /// Fond de carte, puis un bouton dessiné dessus.
    fn card_with_button(theme: &Theme, look: ButtonLook) -> Vec<u8> {
        let mut buf = vec![0_u8; 80 * 60 * 4];
        let mut f = frame(&mut buf, 80, 60);
        f.clear(theme.bar.0, theme.bar.1, theme.bar.2);
        let _ = button(&mut f, 20, 20, 40, 20, 1.0, theme, look);
        buf
    }

    /// Pixel RVB du tampon (qui est en BVRA).
    fn rgb(buf: &[u8], x: usize, y: usize) -> Rgb {
        let i = (y * 80 + x) * 4;
        (buf[i + 2], buf[i + 1], buf[i])
    }

    #[test]
    fn lanneau_de_focus_se_voit_autour_du_bouton_principal() {
        let theme = Theme::dark();
        let buf = card_with_button(
            &theme,
            ButtonLook {
                primary: true,
                focused: true,
                ..ButtonLook::default()
            },
        );
        // Deux pixels de fond de carte entre le bouton et l'anneau…
        assert_eq!(rgb(&buf, 19, 30), theme.bar, "l'écart reste au fond");
        assert_eq!(rgb(&buf, 18, 30), theme.bar, "l'écart fait deux pixels");
        // … puis l'anneau, de la couleur d'accent, sur deux pixels.
        assert_eq!(rgb(&buf, 17, 30), theme.accent, "l'anneau est d'accent");
        assert_eq!(rgb(&buf, 16, 30), theme.accent, "l'anneau fait deux pixels");
        assert_eq!(rgb(&buf, 15, 30), theme.bar, "et rien au-delà");
        assert_eq!(rgb(&buf, 30, 30), theme.accent, "le bouton, lui, est plein");
    }

    /// Luminance approchée d'une couleur.
    fn luma(c: Rgb) -> u32 {
        u32::from(c.0) * 3 + u32::from(c.1) * 6 + u32::from(c.2)
    }

    #[test]
    fn en_sombre_le_survol_eclaircit_et_en_clair_il_assombrit() {
        for (theme, lighter) in [(Theme::dark(), true), (Theme::light(), false)] {
            let rest = rgb(&card_with_button(&theme, ButtonLook::default()), 40, 30);
            let hovered = ButtonLook {
                hovered: true,
                ..ButtonLook::default()
            };
            let over = rgb(&card_with_button(&theme, hovered), 40, 30);
            assert_eq!(luma(over) > luma(rest), lighter, "{rest:?} → {over:?}");
        }
    }

    #[test]
    fn un_bouton_enfonce_se_distingue_du_survol() {
        let theme = Theme::light();
        for primary in [true, false] {
            let look = ButtonLook {
                primary,
                hovered: true,
                ..ButtonLook::default()
            };
            let over = rgb(&card_with_button(&theme, look), 40, 30);
            let pressed = ButtonLook {
                pressed: true,
                ..look
            };
            let down = rgb(&card_with_button(&theme, pressed), 40, 30);
            assert_ne!(over, down, "principal : {primary}");
        }
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
