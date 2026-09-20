//! Reconnaissance d'un caractère par **comparaison de formes**.
//!
//! Le principe est celui des premières machines à lire, et il convient bien à
//! un texte net : chaque tache d'encre est ramenée à une petite grille, et
//! comparée aux mêmes grilles tirées des polices installées sur la machine.
//! La lettre la plus proche gagne.
//!
//! Deux mesures s'ajoutent à la grille, sans lesquelles « o », « O » et « 0 »
//! seraient indiscernables : la **proportion** de la boîte, et sa **place par
//! rapport à la ligne de base**, en hauteurs d'x. C'est ce qui distingue une
//! capitale d'une minuscule, et une lettre d'un chiffre.
//!
//! Rien n'est appris ni deviné : ce sont les vraies polices du système qui
//! servent de modèle, si bien qu'un texte rendu avec l'une d'elles se relit
//! presque toujours exactement.
// Coordonnées et comptages de pixels : les conversions y sont partout, et
// toutes bornées par la taille de l'image.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use std::sync::OnceLock;

use acrux_core::Matrix;
use acrux_fonts::TrueTypeFont;

use super::segment::Box2;

/// Côté de la grille de comparaison.
const GRID: usize = 16;
/// Nombre de cases.
pub(super) const CELLS: usize = GRID * GRID;

/// Caractères recherchés : de quoi relire un texte français ou anglais.
const CHARSET: &str = "abcdefghijklmnopqrstuvwxyz\
                       ABCDEFGHIJKLMNOPQRSTUVWXYZ\
                       0123456789\
                       .,;:!?'()[]-–—/\\&@#%+=<>*\"\
                       éèêëàâäîïôöùûüçÉÈÊÀÂÎÔÙÇ°€$£\u{fb01}\u{fb02}";

/// Ce qu'un caractère du jeu donne à lire : les ligatures en valent deux.
///
/// Un traitement de texte dessine « fi » et « fl » d'un seul glyphe ; la
/// tache est donc unique, et la lire lettre à lettre est impossible. Le
/// modèle, lui, connaît la ligature et rend les deux lettres.
#[must_use]
pub fn spelled(c: char) -> &'static str {
    match c {
        '\u{fb01}' => "fi",
        '\u{fb02}' => "fl",
        _ => "",
    }
}

/// Une police candidate, avec ses modèles.
pub struct Face {
    /// Famille, telle qu'on la nommera dans le document.
    pub family: String,
    /// Graisse.
    pub bold: bool,
    /// Italique.
    pub italic: bool,
    /// Modèles, un par caractère du jeu.
    templates: Vec<Template>,
    /// Police elle-même, gardée pour les épreuves.
    font: Option<TrueTypeFont>,
}

/// Le modèle d'un caractère dans une police.
struct Template {
    /// Caractère représenté.
    c: char,
    /// Grille de couverture, 0–255.
    grid: [u8; CELLS],
    /// Largeur / hauteur de la tache d'encre.
    aspect: f64,
    /// Hauteur du haut de la lettre au-dessus de la ligne de base, en
    /// hauteurs d'x.
    top: f64,
    /// Idem pour le bas (négatif pour une jambage descendant).
    bottom: f64,
    /// Avance, en cadratins.
    advance: f64,
}

/// Un candidat pesé : le caractère et sa distance.
type Candidate = (char, f64);

/// Le verdict d'une comparaison.
#[derive(Debug, Clone, Copy)]
pub struct Match {
    /// Caractère reconnu.
    pub c: char,
    /// Distance : zéro pour une forme identique.
    pub score: f64,
    /// Second candidat, et sa distance.
    ///
    /// Il sert à trancher par le contexte : « l » et « 1 » se ressemblent
    /// trop pour que la forme seule décide, mais un chiffre au milieu d'un
    /// mot est presque toujours une lettre.
    pub second: Option<(char, f64)>,
}

/// Polices installées, lues une seule fois.
static FACES: OnceLock<Vec<Face>> = OnceLock::new();

/// Les polices candidates, chargées à la première demande.
///
/// Quelques familles suffisent : un texte transformé en image vient presque
/// toujours d'un traitement de texte ou d'un navigateur, qui n'emploient
/// qu'une poignée de polices courantes.
pub fn faces() -> &'static [Face] {
    FACES.get_or_init(|| {
        let wanted: [(&str, &str, bool, bool); 10] = [
            ("Arial", "arial", false, false),
            ("Arial", "arialbd", true, false),
            ("Arial", "ariali", false, true),
            ("Times New Roman", "times", false, false),
            ("Times New Roman", "timesbd", true, false),
            ("Times New Roman", "timesi", false, true),
            ("Calibri", "calibri", false, false),
            ("Calibri", "calibrib", true, false),
            ("Courier New", "cour", false, false),
            ("Verdana", "verdana", false, false),
        ];
        let mut out = Vec::new();
        for (family, file, bold, italic) in wanted {
            if let Some(face) = load(family, file, bold, italic) {
                out.push(face);
            }
        }
        // Sans police système, la reconnaissance ne peut rien : la liste
        // reste vide et l'appelant le verra.
        out
    })
}

/// Dossiers où chercher les polices.
fn font_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(custom) = std::env::var("ACRUX_FONT_DIR") {
        dirs.push(std::path::PathBuf::from(custom));
    }
    if let Ok(windir) = std::env::var("WINDIR") {
        dirs.push(std::path::PathBuf::from(windir).join("Fonts"));
    }
    dirs.push(std::path::PathBuf::from("C:/Windows/Fonts"));
    dirs.push(std::path::PathBuf::from("/usr/share/fonts/truetype"));
    dirs.push(std::path::PathBuf::from("/usr/share/fonts"));
    dirs.push(std::path::PathBuf::from("/Library/Fonts"));
    dirs
}

/// Charge une police et en tire les modèles.
fn load(family: &str, file: &str, bold: bool, italic: bool) -> Option<Face> {
    let mut bytes = None;
    for dir in font_dirs() {
        for name in [format!("{file}.ttf"), format!("{file}.TTF")] {
            let path = dir.join(&name);
            if let Ok(data) = std::fs::read(&path) {
                bytes = Some(data);
                break;
            }
        }
        if bytes.is_some() {
            break;
        }
    }
    let font = TrueTypeFont::parse(&bytes?).ok()?;
    if !font.has_glyf() && !font.is_cff() {
        return None;
    }
    let unit = f64::from(font.units_per_em().max(1));
    // La hauteur d'x sert d'étalon : c'est la mesure la plus stable d'une
    // police, et celle qu'on saura relever sur l'image.
    let x_height = glyph_box(&font, 'x').map_or(0.5, |(_, ymax, _, _)| ymax / unit);
    let mut templates = Vec::new();
    for c in CHARSET.chars() {
        if let Some(template) = template_of(&font, c, x_height.max(0.05)) {
            templates.push(template);
        }
    }
    if templates.len() < 40 {
        return None;
    }
    Some(Face {
        family: family.to_string(),
        bold,
        italic,
        templates,
        font: Some(font),
    })
}

/// Boîte d'un glyphe en unités de police : (ymin, ymax, xmin, xmax).
fn glyph_box(font: &TrueTypeFont, c: char) -> Option<(f64, f64, f64, f64)> {
    let gid = font.unicode_to_gid(c)?;
    let path = font.glyph_path(gid)?;
    let b = path.bounds()?;
    Some((b.y0, b.y1, b.x0, b.x1))
}

/// Taille à laquelle les modèles sont dessinés.
///
/// Assez grande pour que le lissage compte peu, assez petite pour que cent
/// caractères se préparent en un clin d'œil.
const DRAWN: u32 = 48;

/// Modèle d'un caractère : sa grille et ses mesures.
///
/// Le modèle est **dessiné puis relu exactement comme une tache de l'image**
/// : même lissage, même binarisation, même grille. C'est la seule façon que
/// les deux soient comparables — un contour rasterisé directement en seize
/// cases ne ressemble pas à une lettre photographiée puis réduite.
fn template_of(font: &TrueTypeFont, c: char, x_height: f64) -> Option<Template> {
    let gid = font.unicode_to_gid(c)?;
    let path = font.glyph_path(gid)?;
    let unit = f64::from(font.units_per_em().max(1));
    let bounds = path.bounds()?;
    let (w, h) = (bounds.x1 - bounds.x0, bounds.y1 - bounds.y0);
    if w <= 0.0 || h <= 0.0 {
        // Le blanc n'a pas de forme : il se déduit des écarts.
        return None;
    }
    let (rgb, iw, ih) = draw_glyph(font, gid, DRAWN)?;
    let drawn = crate::ocr::image::Ink::from_rgb(&rgb, iw, ih);
    let bbox = ink_box(&drawn)?;
    let cells = grid_of(&drawn, bbox);
    Some(Template {
        c,
        grid: cells,
        aspect: w / h,
        top: bounds.y1 / unit / x_height,
        bottom: bounds.y0 / unit / x_height,
        advance: f64::from(font.advance(gid).unwrap_or(0)) / unit,
    })
}

/// Grille d'une tache d'encre relevée sur l'image, en demi-teintes.
#[must_use]
pub fn grid_of(ink: &super::image::Ink, bbox: Box2) -> [u8; CELLS] {
    let mut grid = [0_u8; CELLS];
    let (w, h) = (bbox.width().max(1), bbox.height().max(1));
    // La tache est mise à l'échelle de la grille, en gardant ses proportions
    // — exactement comme le modèle.
    let scale = (GRID as f64 - 0.5) / f64::from(w.max(h));
    let (ox, oy) = (
        (GRID as f64 - f64::from(w) * scale) / 2.0,
        (GRID as f64 - f64::from(h) * scale) / 2.0,
    );
    let mut counts = [0_u32; CELLS];
    let mut totals = [0_u32; CELLS];
    for y in bbox.y0..bbox.y1 {
        for x in bbox.x0..bbox.x1 {
            let cx = ((f64::from(x - bbox.x0) * scale + ox) as usize).min(GRID - 1);
            let cy = ((f64::from(y - bbox.y0) * scale + oy) as usize).min(GRID - 1);
            let cell = cy * GRID + cx;
            totals[cell] += 1;
            counts[cell] += u32::from(ink.cover(x, y));
        }
    }
    for i in 0..CELLS {
        if let Some(mean) = counts[i].checked_div(totals[i]) {
            grid[i] = u8::try_from(mean).unwrap_or(255);
        }
    }
    soften(&grid)
}

/// Adoucit une grille en moyennant chaque case avec ses voisines.
///
/// Deux formes identiques décalées d'un demi-pixel donnent des grilles
/// sensiblement différentes ; adoucies, elles se ressemblent de nouveau.
/// C'est ce qui rend la comparaison insensible au cadrage, qui ne tombe
/// jamais deux fois pareil.
fn soften(grid: &[u8; CELLS]) -> [u8; CELLS] {
    let mut out = [0_u8; CELLS];
    for y in 0..GRID {
        for x in 0..GRID {
            let (mut sum, mut n) = (0_u32, 0_u32);
            for dy in -1_i32..=1 {
                for dx in -1_i32..=1 {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= GRID as i32 || ny >= GRID as i32 {
                        continue;
                    }
                    // Le centre pèse le double de ses voisines.
                    let weight = if dx == 0 && dy == 0 { 2 } else { 1 };
                    sum += u32::from(grid[ny as usize * GRID + nx as usize]) * weight;
                    n += weight;
                }
            }
            out[y * GRID + x] = u8::try_from(sum / n.max(1)).unwrap_or(255);
        }
    }
    out
}

/// Compare une tache aux modèles d'une police.
///
/// `top` et `bottom` sont la place de la tache par rapport à la ligne de
/// base, en hauteurs d'x ; `aspect` sa proportion.
#[must_use]
pub fn best(face: &Face, grid: &[u8; CELLS], aspect: f64, top: f64, bottom: f64) -> Option<Match> {
    let views = views_of(grid);
    let (mut best, mut second): (Option<Candidate>, Option<Candidate>) = (None, None);
    for template in &face.templates {
        let shape = distance(&views, &template.grid);
        // La proportion pèse autant qu'un cinquième de la forme, la place sur
        // la ligne autant qu'un tiers : c'est ce qui sépare « o » de « O ».
        let proportion = (aspect.max(0.05).ln() - template.aspect.max(0.05).ln()).abs();
        let place = (top - template.top).abs() + (bottom - template.bottom).abs();
        let score = shape + 0.20 * proportion + 0.33 * place;
        if best.is_none_or(|(_, b)| score < b) {
            second = best;
            best = Some((template.c, score));
        } else if second.is_none_or(|(_, b)| score < b) {
            second = Some((template.c, score));
        }
    }
    best.map(|(c, score)| Match {
        c,
        score,
        second,
    })
}

/// Les neuf cadrages d'une grille : le sien, et ses voisins d'une case.
///
/// Une lettre relevée sur une image n'est jamais cadrée exactement comme son
/// modèle — un demi-pixel d'écart suffit à déplacer la grille d'une case.
/// Comparer les neuf cadrages et garder le meilleur rend l'appariement
/// insensible à ce hasard.
fn views_of(grid: &[u8; CELLS]) -> Vec<[u8; CELLS]> {
    let mut out = Vec::with_capacity(9);
    for dy in -1_i32..=1 {
        for dx in -1_i32..=1 {
            let mut shifted = [0_u8; CELLS];
            for y in 0..GRID {
                for x in 0..GRID {
                    let (sx, sy) = (x as i32 - dx, y as i32 - dy);
                    if sx < 0 || sy < 0 || sx >= GRID as i32 || sy >= GRID as i32 {
                        continue;
                    }
                    shifted[y * GRID + x] = grid[sy as usize * GRID + sx as usize];
                }
            }
            out.push(shifted);
        }
    }
    out
}

/// Écart entre une tache — dans ses neuf cadrages — et un modèle.
fn distance(views: &[[u8; CELLS]], template: &[u8; CELLS]) -> f64 {
    let mut best = u32::MAX;
    for view in views {
        let mut sum = 0_u32;
        for i in 0..CELLS {
            sum += u32::from(view[i].abs_diff(template[i]));
            if sum >= best {
                break;
            }
        }
        best = best.min(sum);
    }
    {
        f64::from(best) / (CELLS as f64 * 255.0)
    }
}

/// Avance d'un caractère dans une police, en cadratins.
#[must_use]
pub fn advance(face: &Face, c: char) -> Option<f64> {
    face.templates
        .iter()
        .find(|t| t.c == c)
        .map(|t| t.advance)
}

/// Les meilleurs candidats d'une tache, pour la mise au point.
#[must_use]
pub fn ranked(face: &Face, grid: &[u8; CELLS], aspect: f64, top: f64, bottom: f64) -> Vec<(char, f64, f64, f64, f64)> {
    let views = views_of(grid);
    let mut all: Vec<(char, f64, f64, f64, f64)> = face
        .templates
        .iter()
        .map(|template| {
            let shape = distance(&views, &template.grid);
            let proportion = (aspect.max(0.05).ln() - template.aspect.max(0.05).ln()).abs();
            let place = (top - template.top).abs() + (bottom - template.bottom).abs();
            (
                template.c,
                shape + 0.20 * proportion + 0.33 * place,
                shape,
                proportion,
                place,
            )
        })
        .collect();
    all.sort_by(|a, b| a.1.total_cmp(&b.1));
    all.truncate(5);
    all
}

/// Boîte de l'encre d'une image.
fn ink_box(ink: &crate::ocr::image::Ink) -> Option<Box2> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0_u32, 0_u32);
    for y in 0..ink.height {
        for x in 0..ink.width {
            if ink.at(x, y) {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    (x0 != u32::MAX).then_some(Box2 { x0, y0, x1, y1 })
}

/// Dessine un caractère d'une police dans une image RVB, pour les épreuves.
///
/// Rend l'image et la boîte d'encre : de quoi refaire le chemin complet —
/// dessiner, binariser, comparer — et vérifier qu'une lettre se reconnaît
/// elle-même.
#[must_use]
pub fn draw(face: &Face, c: char, size: u32) -> Option<(Vec<u8>, u32, u32)> {
    let font = face.font.as_ref()?;
    let gid = font.unicode_to_gid(c)?;
    draw_glyph(font, gid, size)
}

/// Dessine un glyphe dans une image RVB blanche.
fn draw_glyph(font: &TrueTypeFont, gid: u16, size: u32) -> Option<(Vec<u8>, u32, u32)> {
    let path = font.glyph_path(gid)?;
    let unit = f64::from(font.units_per_em().max(1));
    let scale = f64::from(size) / unit;
    let (w, h) = (size * 2, size * 2);
    let m = Matrix::new(
        scale,
        0.0,
        0.0,
        -scale,
        f64::from(size) / 2.0,
        f64::from(size) * 3.0 / 2.0,
    );
    let mask = acrux_graphics::path_coverage(&path, &m, acrux_graphics::FillRule::NonZero, w, h);
    let mut rgb = vec![255_u8; (w * h * 3) as usize];
    for (i, cover) in mask.data().iter().enumerate() {
        let v = 255 - *cover;
        rgb[i * 3] = v;
        rgb[i * 3 + 1] = v;
        rgb[i * 3 + 2] = v;
    }
    Some((rgb, w, h))
}


/// Dessine une grille en caractères, pour la mise au point.
#[must_use]
pub fn ascii(grid: &[u8; CELLS]) -> String {
    let mut out = String::new();
    for y in 0..GRID {
        for x in 0..GRID {
            let v = grid[y * GRID + x];
            out.push(match v {
                0..=40 => ' ',
                41..=110 => '.',
                111..=180 => '+',
                _ => '#',
            });
        }
        out.push('\n');
    }
    out
}

/// Grille d'un caractère d'une police, pour la mise au point.
#[must_use]
pub fn template_grid(face: &Face, c: char) -> Option<[u8; CELLS]> {
    face.templates.iter().find(|t| t.c == c).map(|t| t.grid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sans police système, la liste est vide mais rien ne casse.
    #[test]
    fn les_polices_se_chargent_sans_erreur() {
        let all = faces();
        for face in all {
            assert!(!face.family.is_empty());
            assert!(face.templates.len() >= 40);
        }
    }

    /// Taux de reconnaissance d'un alphabet dessiné puis relu.
    ///
    /// Le banc d'essai de la reconnaissance : il donne un nombre, et c'est
    /// lui qu'on cherche à faire monter.
    fn recognition_rate(face: &Face, sizes: &[u32], verbose: bool) -> f64 {
        let letters = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let (mut good, mut total) = (0_u32, 0_u32);
        for size in sizes {
            for c in letters.chars() {
                let Some((rgb, w, h)) = draw(face, c, *size) else {
                    continue;
                };
                let ink = crate::ocr::image::Ink::from_rgb(&rgb, w, h);
                let Some(bbox) = ink_box(&ink) else { continue };
                let grid = grid_of(&ink, bbox);
                // La place sur la ligne est celle du modèle lui-même : on
                // éprouve ici la forme, pas le relevé de la ligne de base.
                let Some(template) = face.templates.iter().find(|t| t.c == c) else {
                    continue;
                };
                let aspect = f64::from(bbox.width()) / f64::from(bbox.height());
                let found = best(face, &grid, aspect, template.top, template.bottom);
                total += 1;
                match found {
                    Some(m) if m.c == c => good += 1,
                    Some(m) if verbose => println!("  {size}px « {c} » lu « {} »", m.c),
                    _ => {}
                }
            }
        }
        if total == 0 {
            return 0.0;
        }
        f64::from(good) / f64::from(total)
    }

    /// Le banc d'essai, affiché (`-- --ignored --nocapture le_banc`).
    #[test]
    #[ignore = "banc d'essai"]
    fn le_banc() {
        for size in [14_u32, 20, 28, 40, 56] {
            let mut sum = 0.0;
            let mut n = 0;
            for face in faces() {
                sum += recognition_rate(face, &[size], false);
                n += 1;
            }
            println!("--- {size} px : {:.0} % ---", sum / f64::from(n.max(1)) * 100.0);
        }
        for face in faces() {
            let rate = recognition_rate(face, &[20, 28, 40], false);
            println!(
                "{} {}{} : {:.0} %",
                face.family,
                if face.bold { "gras " } else { "" },
                if face.italic { "italique" } else { "" },
                rate * 100.0
            );
        }
    }

    /// Une lettre dessinée puis relue doit se reconnaître elle-même.
    ///
    /// C'est l'épreuve du chemin complet : dessin, binarisation, découpe,
    /// grille, comparaison. Si elle échoue, ce n'est pas l'image qui est en
    /// cause.
    #[test]
    fn une_lettre_se_reconnait_elle_meme() {
        let Some(face) = faces().first() else { return };
        for size in [28_u32, 40] {
            let rate = recognition_rate(face, &[size], false);
            assert!(
                rate > 0.92,
                "{} à {size} px : {:.0} % seulement",
                face.family,
                rate * 100.0
            );
        }
    }

    /// Le modèle d'un « o » est plus rond que celui d'un « l ».
    #[test]
    fn la_proportion_distingue_les_formes() {
        let Some(face) = faces().first() else { return };
        let o = face.templates.iter().find(|t| t.c == 'o');
        let l = face.templates.iter().find(|t| t.c == 'l');
        if let (Some(o), Some(l)) = (o, l) {
            assert!(o.aspect > l.aspect);
            assert!(o.top < l.top, "le « l » monte plus haut que le « o »");
        }
    }
}
