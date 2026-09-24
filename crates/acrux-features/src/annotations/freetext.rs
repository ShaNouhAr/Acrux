//! Zone de texte (`/FreeText`, §12.5.6.6) : mise en lignes et apparence,
//! et la légende (`/IT /FreeTextCallout`) qui la relie à un point de la page
//! par une ligne coudée terminée d'une flèche.
//!
//! La mise en lignes est **publique** : l'application s'en sert pour
//! dessiner le texte pendant qu'on le tape. Ce qu'on voit en tapant se coupe
//! donc exactement comme ce qui sera écrit — mêmes largeurs de caractères
//! (les métriques des polices standard), mêmes règles de coupure.
//!
//! Le texte s'écrit dans une des polices standard, en WinAnsiEncoding : ce
//! que tout lecteur sait afficher sans police incorporée. Un caractère hors
//! de cet encodage devient « ? » ; [`unsupported_chars`] les nomme d'avance,
//! pour prévenir la personne plutôt que de la laisser le découvrir.

use std::fmt::Write as _;

use acrux_core::{Point, Rect};
use acrux_document::{Dict, Name, Object};

use super::shapes::{bounds_of, ending_shape, EndShape};
use super::{color_op, fmt, Callout, LineEnding, Rgb, TextAlign};
use crate::stamp::{encode_win_ansi, pdf_literal, StandardFont};

/// Marge intérieure entre le cadre et le texte, en points (hors bordure).
pub const PADDING: f64 = 2.0;

/// Interligne, en multiple du corps.
pub const LEADING: f64 = 1.15;

/// Distance, en points, entre la zone et le coude d'une légende.
pub const KNEE: f64 = 12.0;

/// Une ligne mise en page.
#[derive(Debug, Clone, PartialEq)]
pub struct LaidLine {
    /// Texte de la ligne (sans le saut ni les espaces de coupure).
    pub text: String,
    /// Largeur en points.
    pub width: f64,
    /// Rang, en caractères du texte entier, du premier caractère.
    pub start: usize,
    /// Rang qui suit le dernier caractère de la ligne.
    pub end: usize,
}

/// Largeur d'un caractère à ce corps.
fn char_width(font: StandardFont, c: char, size: f64) -> f64 {
    font.char_width(c) * size / 1000.0
}

/// Coupe un paragraphe (`chars[start..end]`, sans saut de ligne) en lignes
/// d'au plus `max_width` points : au dernier blanc qui tient, ou, pour un
/// mot plus long que la ligne, entre deux caractères.
fn wrap(
    chars: &[char],
    (start, end): (usize, usize),
    (font, size, max_width): (StandardFont, f64, f64),
    out: &mut Vec<LaidLine>,
) {
    let width_of = |a: usize, b: usize| -> f64 {
        chars[a..b].iter().map(|&c| char_width(font, c, size)).sum()
    };
    let push = |a: usize, b: usize, out: &mut Vec<LaidLine>| {
        out.push(LaidLine {
            text: chars[a..b].iter().collect(),
            width: width_of(a, b),
            start: a,
            end: b,
        });
    };
    let mut line_start = start;
    let mut line_w = 0.0;
    // Premier caractère d'un mot qui suit un blanc : là où l'on peut couper.
    let mut last_break: Option<usize> = None;
    let mut i = start;
    while i < end {
        let c = chars[i];
        let cw = char_width(font, c, size);
        if c != ' ' && i > line_start && line_w + cw > max_width {
            let at = last_break.filter(|&b| b > line_start && b <= i);
            let (cut, next) = match at {
                Some(b) => {
                    // Les blancs de la coupure n'appartiennent à aucune
                    // ligne : ils ne se voient pas.
                    let mut e = b;
                    while e > line_start && chars[e - 1] == ' ' {
                        e -= 1;
                    }
                    (e, b)
                }
                None => (i, i),
            };
            push(line_start, cut, out);
            line_start = next;
            last_break = None;
            line_w = width_of(line_start, i);
            continue;
        }
        line_w += cw;
        if c == ' ' {
            last_break = Some(i + 1);
        }
        i += 1;
    }
    push(line_start, end, out);
}

/// Met un texte en lignes d'au plus `max_width` points : les sauts de ligne
/// du texte d'abord, puis un retour à la ligne au dernier blanc qui tient.
/// Un mot plus long que la ligne est coupé entre deux caractères. Un texte
/// vide donne une ligne vide (le curseur a une place).
#[must_use]
pub fn layout(text: &str, font: StandardFont, size: f64, max_width: f64) -> Vec<LaidLine> {
    let chars: Vec<char> = text.chars().collect();
    let max_width = if max_width.is_finite() {
        max_width.max(0.0)
    } else {
        f64::MAX
    };
    let mut out = Vec::new();
    let mut start = 0;
    loop {
        let end = chars[start..]
            .iter()
            .position(|&c| c == '\n')
            .map_or(chars.len(), |p| start + p);
        wrap(&chars, (start, end), (font, size, max_width), &mut out);
        if end >= chars.len() {
            break;
        }
        start = end + 1;
    }
    out
}

/// Ligne et décalage (en points depuis le début de la ligne) du curseur
/// placé au caractère `caret`.
#[must_use]
pub fn caret_in(
    lines: &[LaidLine],
    text: &str,
    font: StandardFont,
    size: f64,
    caret: usize,
) -> (usize, f64) {
    let index = lines
        .iter()
        .rposition(|l| l.start <= caret)
        .unwrap_or_default();
    let Some(line) = lines.get(index) else {
        return (0, 0.0);
    };
    let upto = caret.min(line.end).saturating_sub(line.start);
    let x: f64 = text
        .chars()
        .skip(line.start)
        .take(upto)
        .map(|c| char_width(font, c, size))
        .sum();
    (index, x)
}

/// Rang du caractère le plus proche du point `(x, line)` : `x` en points
/// depuis le début de la ligne.
#[must_use]
pub fn char_at(
    lines: &[LaidLine],
    text: &str,
    font: StandardFont,
    size: f64,
    line: usize,
    x: f64,
) -> usize {
    let Some(l) = lines.get(line.min(lines.len().saturating_sub(1))) else {
        return 0;
    };
    let mut pen = 0.0;
    for (k, c) in text.chars().skip(l.start).take(l.end - l.start).enumerate() {
        let w = char_width(font, c, size);
        if x < pen + w / 2.0 {
            return l.start + k;
        }
        pen += w;
    }
    l.end
}

/// Marge intérieure totale, bordure comprise.
#[must_use]
pub fn inner_padding(border: f64) -> f64 {
    PADDING + border.max(0.0)
}

/// Distance du haut de la zone de texte (intérieur) à la première ligne de
/// base : de quoi loger les capitales accentuées, qui montent plus haut que
/// la hampe (`Ascender`) des métriques.
#[must_use]
pub fn first_baseline_drop(font: StandardFont, size: f64) -> f64 {
    let m = font.metrics();
    m.ascent.max(0.9 * m.bbox[3]) / 1000.0 * size
}

/// Hauteur qu'il faut à une zone de largeur `width` pour tout son texte.
#[must_use]
pub fn fit_height(text: &str, font: StandardFont, size: f64, width: f64, border: f64) -> f64 {
    let pad = inner_padding(border);
    let lines = layout(text, font, size, width - 2.0 * pad).len().max(1);
    #[allow(clippy::cast_precision_loss)] // quelques lignes
    let n = lines as f64;
    2.0 * pad + first_baseline_drop(font, size) + (n - 1.0) * LEADING * size
        - font.descent() / 1000.0 * size
}

/// Caractères du texte que la police standard ne peut pas écrire : ils
/// deviendront « ? ». Chaque caractère n'est nommé qu'une fois.
#[must_use]
pub fn unsupported_chars(text: &str) -> Vec<char> {
    let mut out = encode_win_ansi(text).replaced;
    out.retain(|c| !c.is_control());
    let mut seen = Vec::new();
    out.retain(|c| {
        if seen.contains(c) {
            false
        } else {
            seen.push(*c);
            true
        }
    });
    out
}

/// Nom de ressource d'une police standard, ceux qu'écrit Acrobat
/// (`/Helv`, `/TiRo`, `/Cour`…) : un lecteur qui régénère l'apparence à
/// partir de `/DA` les reconnaît.
#[must_use]
pub fn resource_name(font: StandardFont) -> &'static str {
    match font {
        StandardFont::Helvetica => "Helv",
        StandardFont::HelveticaBold => "HeBo",
        StandardFont::HelveticaOblique => "HeOb",
        StandardFont::HelveticaBoldOblique => "HeBO",
        StandardFont::TimesRoman => "TiRo",
        StandardFont::TimesBold => "TiBo",
        StandardFont::TimesItalic => "TiIt",
        StandardFont::TimesBoldItalic => "TiBI",
        StandardFont::Courier => "Cour",
        StandardFont::CourierBold => "CoBo",
        StandardFont::CourierOblique => "CoOb",
        StandardFont::CourierBoldOblique => "CoBO",
        StandardFont::Symbol => "Symb",
        StandardFont::ZapfDingbats => "ZaDb",
    }
}

/// Dictionnaire `/Font` des ressources de l'apparence : la police standard,
/// en WinAnsiEncoding, sans rien d'incorporé.
#[must_use]
pub fn font_resources(font: StandardFont) -> Dict {
    let mut f = Dict::new();
    f.insert(Name::new("Type"), Object::Name(Name::new("Font")));
    f.insert(Name::new("Subtype"), Object::Name(Name::new("Type1")));
    f.insert(
        Name::new("BaseFont"),
        Object::Name(Name::new(font.base_font())),
    );
    if font.has_builtin_encoding() {
        // Symbol et ZapfDingbats ont leur propre encodage.
    } else {
        f.insert(
            Name::new("Encoding"),
            Object::Name(Name::new("WinAnsiEncoding")),
        );
    }
    let mut fonts = Dict::new();
    fonts.insert(Name::new(resource_name(font)), Object::Dict(f));
    fonts
}

/// Tout ce qu'il faut pour dessiner une zone de texte.
#[derive(Debug, Clone, PartialEq)]
pub struct TextBox<'a> {
    /// Zone du texte.
    pub rect: Rect,
    /// Texte, sauts de ligne compris.
    pub text: &'a str,
    /// Police standard.
    pub font: StandardFont,
    /// Corps en points.
    pub size: f64,
    /// Couleur du texte.
    pub color: Rgb,
    /// Cadre : couleur et épaisseur.
    pub border: Option<(Rgb, f64)>,
    /// Fond.
    pub fill: Option<Rgb>,
    /// Alignement des lignes.
    pub align: TextAlign,
}

impl TextBox<'_> {
    /// Épaisseur du cadre, nulle sans cadre.
    #[must_use]
    pub fn border_width(&self) -> f64 {
        self.border.map_or(0.0, |(_, w)| w.max(0.0))
    }
}

/// Opérateurs qui dessinent la zone : fond, cadre, puis le texte, coupé à
/// l'intérieur du cadre.
#[must_use]
pub fn box_ops(b: &TextBox<'_>) -> String {
    let r = b.rect;
    let bw = b.border_width();
    let mut out = String::from("q ");
    if let Some(f) = b.fill {
        let _ = write!(
            out,
            "{} {} {} {} {} re f ",
            color_op(f, false),
            fmt(r.x0),
            fmt(r.y0),
            fmt(r.width()),
            fmt(r.height())
        );
    }
    if let (Some((c, _)), true) = (b.border, bw > 0.0) {
        let _ = write!(
            out,
            "{} {} w {} {} {} {} re S ",
            color_op(c, true),
            fmt(bw),
            fmt(r.x0 + bw / 2.0),
            fmt(r.y0 + bw / 2.0),
            fmt((r.width() - bw).max(0.0)),
            fmt((r.height() - bw).max(0.0))
        );
    }
    let pad = inner_padding(bw);
    let inner = Rect::new(r.x0 + pad, r.y0 + pad, r.x1 - pad, r.y1 - pad);
    let lines = layout(b.text, b.font, b.size, inner.width());
    let _ = write!(
        out,
        "{} {} {} {} re W n BT /{} {} Tf {} ",
        fmt(r.x0 + bw),
        fmt(r.y0 + bw),
        fmt((r.width() - 2.0 * bw).max(0.0)),
        fmt((r.height() - 2.0 * bw).max(0.0)),
        resource_name(b.font),
        fmt(b.size),
        color_op(b.color, false)
    );
    let mut y = r.y1 - pad - first_baseline_drop(b.font, b.size);
    for line in &lines {
        let x = match b.align {
            TextAlign::Left => inner.x0,
            TextAlign::Center => inner.x0 + (inner.width() - line.width) / 2.0,
            TextAlign::Right => inner.x1 - line.width,
        };
        if !line.text.is_empty() {
            let encoded = encode_win_ansi(&line.text);
            let _ = write!(
                out,
                "1 0 0 1 {} {} Tm {} Tj ",
                fmt(x),
                fmt(y),
                pdf_literal(&encoded.bytes)
            );
        }
        y -= LEADING * b.size;
    }
    out.push_str("ET Q");
    out
}

/// Géométrie d'une légende : l'ancre, le coude (s'il y en a un) et la
/// jonction sur le bord de la zone, dans cet ordre — l'ordre de `/CL`.
///
/// La jonction est le milieu du côté de la zone qui fait face à l'ancre ;
/// le coude, s'il n'est pas donné, en est écarté de [`KNEE`] points vers
/// l'ancre, pour que la ligne quitte la zone à angle droit.
#[must_use]
pub fn callout_points(rect: &Rect, callout: &Callout) -> Vec<Point> {
    let a = callout.anchor;
    let (cx, cy) = (
        f64::midpoint(rect.x0, rect.x1),
        f64::midpoint(rect.y0, rect.y1),
    );
    // Écart de l'ancre à la zone, rapporté à ses demi-dimensions : le côté
    // le plus « en face » l'emporte.
    let hw = (rect.width() / 2.0).max(1e-6);
    let hh = (rect.height() / 2.0).max(1e-6);
    let horizontal = ((a.x - cx) / hw).abs() >= ((a.y - cy) / hh).abs();
    let (junction, out) = if horizontal {
        if a.x < cx {
            (Point::new(rect.x0, cy), Point::new(-1.0, 0.0))
        } else {
            (Point::new(rect.x1, cy), Point::new(1.0, 0.0))
        }
    } else if a.y < cy {
        (Point::new(cx, rect.y0), Point::new(0.0, -1.0))
    } else {
        (Point::new(cx, rect.y1), Point::new(0.0, 1.0))
    };
    let knee = callout.knee.or_else(|| {
        // Un coude n'a de sens que si l'ancre est au-delà : trop près, la
        // ligne va droit.
        let reach = (a.x - junction.x) * out.x + (a.y - junction.y) * out.y;
        (reach > KNEE * 1.5)
            .then(|| Point::new(junction.x + KNEE * out.x, junction.y + KNEE * out.y))
    });
    let mut points = vec![a];
    points.extend(knee);
    points.push(junction);
    points
}

/// Apparence d'une zone de texte, avec sa légende s'il y en a une : le
/// contenu, et la boîte de l'annotation (le `/Rect`), qui déborde de la
/// zone de la ligne et de la flèche.
#[must_use]
pub fn appearance(b: &TextBox<'_>, callout: Option<&Callout>) -> (String, Rect) {
    let mut content = String::new();
    let mut bbox = b.rect;
    if let Some(call) = callout {
        let points = callout_points(&b.rect, call);
        // La ligne prend la couleur du cadre, ou celle du texte sans cadre.
        let (line_color, line_w) = b.border.filter(|(_, w)| *w > 0.0).unwrap_or((b.color, 1.0));
        let _ = write!(
            content,
            "q {} {} w 1 J 1 j ",
            color_op(line_color, true),
            fmt(line_w)
        );
        for (i, p) in points.iter().enumerate() {
            let _ = write!(
                content,
                "{} {} {} ",
                fmt(p.x),
                fmt(p.y),
                if i == 0 { "m" } else { "l" }
            );
        }
        content.push_str("S ");
        let mut extent = points.clone();
        if let Some(from) = points.get(1) {
            if let Some(head) = ending_shape(call.anchor, *from, line_w, call.ending) {
                match &head {
                    EndShape::Open(p) => {
                        for (i, q) in p.iter().enumerate() {
                            let _ = write!(
                                content,
                                "{} {} {} ",
                                fmt(q.x),
                                fmt(q.y),
                                if i == 0 { "m" } else { "l" }
                            );
                        }
                        content.push_str("S ");
                        extent.extend(p.iter().copied());
                    }
                    EndShape::Closed(p) => {
                        let _ = write!(content, "{} ", color_op(line_color, false));
                        for (i, q) in p.iter().enumerate() {
                            let _ = write!(
                                content,
                                "{} {} {} ",
                                fmt(q.x),
                                fmt(q.y),
                                if i == 0 { "m" } else { "l" }
                            );
                        }
                        content.push_str("b ");
                        extent.extend(p.iter().copied());
                    }
                    EndShape::Disc(c, r) => {
                        let _ = write!(
                            content,
                            "{} {} b ",
                            color_op(line_color, false),
                            super::shapes::ellipse_ops(&Rect::new(
                                c.x - r,
                                c.y - r,
                                c.x + r,
                                c.y + r
                            ))
                        );
                        extent.push(Point::new(c.x - r, c.y - r));
                        extent.push(Point::new(c.x + r, c.y + r));
                    }
                }
            }
        }
        content.push_str("Q ");
        bbox = bbox.union(&bounds_of(&extent, line_w / 2.0 + 1.0));
    }
    content.push_str(&box_ops(b));
    (content, bbox)
}

/// `/DA` : couleur du texte, police et corps — et couleur du cadre, que
/// les lecteurs lisent là (`RG`) pour une zone de texte.
#[must_use]
pub fn default_appearance(b: &TextBox<'_>) -> String {
    let mut da = format!(
        "{} /{} {} Tf",
        color_op(b.color, false),
        resource_name(b.font),
        fmt(b.size)
    );
    if let Some((c, _)) = b.border {
        let _ = write!(da, " {}", color_op(c, true));
    }
    da
}

/// `/DS` : le même style en CSS, que certains lecteurs préfèrent à `/DA`
/// (§12.7.4.3).
#[must_use]
pub fn default_style(b: &TextBox<'_>) -> String {
    let family = match b.font {
        StandardFont::TimesRoman
        | StandardFont::TimesBold
        | StandardFont::TimesItalic
        | StandardFont::TimesBoldItalic => "Times New Roman",
        StandardFont::Courier
        | StandardFont::CourierBold
        | StandardFont::CourierOblique
        | StandardFont::CourierBoldOblique => "Courier New",
        _ => "Helvetica",
    };
    let byte = |v: f64| {
        // Borné à 0..=255 juste avant : la conversion est exacte.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let b = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        b
    };
    let align = match b.align {
        TextAlign::Left => "left",
        TextAlign::Center => "center",
        TextAlign::Right => "right",
    };
    format!(
        "font: {}{}{} {}pt; text-align:{align}; color:#{:02X}{:02X}{:02X}",
        if b.font.is_bold() { "bold " } else { "" },
        if b.font.is_italic() { "italic " } else { "" },
        family,
        fmt(b.size),
        byte(b.color[0]),
        byte(b.color[1]),
        byte(b.color[2])
    )
}

/// `/RD` d'une légende : l'écart entre le `/Rect` de l'annotation et la
/// zone du texte, côté gauche, bas, droit, haut (§12.5.6.6).
#[must_use]
pub fn rect_differences(outer: &Rect, inner: &Rect) -> [f64; 4] {
    [
        (inner.x0 - outer.x0).max(0.0),
        (inner.y0 - outer.y0).max(0.0),
        (outer.x1 - inner.x1).max(0.0),
        (outer.y1 - inner.y1).max(0.0),
    ]
}

/// Terminaison par défaut d'une légende : la flèche ouverte d'Acrobat.
#[must_use]
pub fn default_ending() -> LineEnding {
    LineEnding::OpenArrow
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    const H: StandardFont = StandardFont::Helvetica;

    #[test]
    fn coupure_aux_blancs() {
        let w3 = H.text_width("un deux trois", 12.0) + 0.5;
        let lines = layout("un deux trois quatre", H, 12.0, w3);
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["un deux trois", "quatre"]);
        assert!(lines.iter().all(|l| l.width <= w3));
        assert_eq!(lines[1].start, 14);
        assert_eq!(lines[1].end, 20);
    }

    #[test]
    fn sauts_de_ligne_et_lignes_vides() {
        let lines = layout("a\n\nb", H, 12.0, 500.0);
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["a", "", "b"]);
        assert_eq!(lines[2].start, 3);
        assert_eq!(layout("", H, 12.0, 100.0).len(), 1);
    }

    #[test]
    fn mot_trop_long_coupe() {
        let lines = layout("anticonstitutionnellement", H, 12.0, 40.0);
        assert!(lines.len() > 2, "{lines:?}");
        assert!(lines.iter().all(|l| l.width <= 40.0 + 1e-9));
        let joined: String = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(joined, "anticonstitutionnellement");
    }

    #[test]
    fn curseur_et_clic() {
        let text = "Bonjour\nà tous";
        let lines = layout(text, H, 12.0, 500.0);
        let (line, x) = caret_in(&lines, text, H, 12.0, 9);
        assert_eq!(line, 1);
        assert!((x - H.text_width("à", 12.0)).abs() < 1e-9);
        assert_eq!(caret_in(&lines, text, H, 12.0, 0), (0, 0.0));
        assert_eq!(char_at(&lines, text, H, 12.0, 1, 0.1), 8);
        assert_eq!(char_at(&lines, text, H, 12.0, 0, 1000.0), 7);
    }

    #[test]
    fn hors_winansi_signale() {
        assert_eq!(unsupported_chars("Ωmega Ω €"), vec!['Ω']);
        assert!(unsupported_chars("Café à l'œil").is_empty());
    }

    #[test]
    fn legende_du_bon_cote() {
        let r = Rect::new(100.0, 100.0, 200.0, 140.0);
        let call = Callout {
            anchor: Point::new(20.0, 120.0),
            knee: None,
            ending: LineEnding::OpenArrow,
        };
        let p = callout_points(&r, &call);
        assert_eq!(p.len(), 3);
        assert_eq!(p[2], Point::new(100.0, 120.0));
        assert_eq!(p[1], Point::new(88.0, 120.0));
        // Ancre toute proche : pas de coude.
        let near = Callout {
            anchor: Point::new(150.0, 150.0),
            ..call
        };
        assert_eq!(callout_points(&r, &near).len(), 2);
        let tb = TextBox {
            rect: r,
            text: "Voir ici",
            font: H,
            size: 12.0,
            color: [0.0, 0.0, 0.0],
            border: Some(([1.0, 0.0, 0.0], 1.0)),
            fill: None,
            align: TextAlign::Left,
        };
        let (content, bbox) = appearance(&tb, Some(&call));
        assert!(bbox.x0 < 21.0, "{bbox:?}");
        assert!(content.contains("(Voir ici) Tj"), "{content}");
        assert_eq!(default_appearance(&tb), "0 0 0 rg /Helv 12 Tf 1 0 0 RG");
    }

    #[test]
    fn hauteur_ajustee() {
        let one = fit_height("a", H, 12.0, 200.0, 0.0);
        let two = fit_height("a\nb", H, 12.0, 200.0, 0.0);
        assert!((two - one - LEADING * 12.0).abs() < 1e-9);
        assert!(one > 12.0 && one < 20.0, "{one}");
    }
}
