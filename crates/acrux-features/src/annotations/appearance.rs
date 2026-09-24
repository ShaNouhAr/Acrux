//! Refaire l'apparence d'une annotation existante d'après ce que dit son
//! dictionnaire — sa géométrie (`/Rect`, `/L`, `/Vertices`, `/InkList`,
//! `/QuadPoints`), ses couleurs (`/C`, `/IC`, `/DA`), son trait (`/BS`) et
//! son opacité (`/CA`).
//!
//! Chaque type est redessiné par **le même code** que celui qui l'a créé
//! (`shapes`, `freetext`, le balisage, l'icône des notes) : une forme dont on
//! change la couleur ressemble trait pour trait à celle qu'on aurait posée
//! de cette couleur. Une annotation d'un autre logiciel prend ainsi notre
//! dessin ; ses raffinements (bord en nuage, tirets) se perdent, mais ses
//! clés restent dans le dictionnaire, pour les lecteurs qui les dessinent.

use acrux_core::{Point, Rect};
use acrux_document::{Dict, Document, Name, Object};

use super::freetext::{self, TextBox};
use super::{
    appearance_stream, border_width, color_from, color_op, fmt, markup_appearance, note_icon,
    numbers, points_array, quads_from, shape_stream, shapes, Callout, LineEnding, MarkupKind, Rgb,
    ShapeStyle, TextAlign,
};
use crate::stamp::StandardFont;

/// Apparence refaite : le flux, le nouveau `/Rect` (la boîte du dessin), et
/// les clés à réécrire avec elle (`None` : à retirer).
pub(crate) struct Regenerated {
    /// Flux d'apparence `/N`.
    pub(crate) stream: Object,
    /// Nouveau `/Rect`.
    pub(crate) rect: Rect,
    /// Clés qui changent avec le dessin.
    pub(crate) keys: Vec<(&'static str, Option<Object>)>,
}

/// Ce que dit une chaîne `/DA` : police, corps, couleur de remplissage (le
/// texte) et de trait (le cadre d'une zone de texte).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DaStyle {
    /// Nom de ressource de la police, sans la barre oblique.
    pub font: Option<String>,
    /// Corps, en points.
    pub size: Option<f64>,
    /// Couleur de remplissage (`g`, `rg`, `k`).
    pub fill: Option<Rgb>,
    /// Couleur de trait (`G`, `RG`, `K`).
    pub stroke: Option<Rgb>,
}

/// Lit une chaîne d'apparence par défaut (`/DA`, §12.7.4.3) : une suite
/// d'opérandes et d'opérateurs de contenu, dont on ne garde que la police et
/// les couleurs. Ce qui ne se lit pas est ignoré.
#[must_use]
pub fn parse_da(da: &str) -> DaStyle {
    let mut out = DaStyle::default();
    let mut stack: Vec<String> = Vec::new();
    for token in da.split_whitespace() {
        let numbers = |stack: &[String], n: usize| -> Option<Vec<f64>> {
            let start = stack.len().checked_sub(n)?;
            stack[start..]
                .iter()
                .map(|s| s.parse::<f64>().ok())
                .collect()
        };
        match token {
            "g" | "rg" | "k" | "G" | "RG" | "K" => {
                let n = match token {
                    "g" | "G" => 1,
                    "rg" | "RG" => 3,
                    _ => 4,
                };
                let color = numbers(&stack, n).and_then(|c| color_from(&c));
                if token.chars().all(char::is_lowercase) {
                    out.fill = color.or(out.fill);
                } else {
                    out.stroke = color.or(out.stroke);
                }
                stack.clear();
            }
            "Tf" => {
                if stack.len() >= 2 {
                    let size = stack[stack.len() - 1].parse::<f64>().ok();
                    let font = stack[stack.len() - 2].trim_start_matches('/').to_string();
                    out.size = size.filter(|s| s.is_finite() && *s > 0.0).or(out.size);
                    out.font = Some(font);
                }
                stack.clear();
            }
            other => stack.push(other.to_string()),
        }
    }
    out
}

/// La même chaîne `/DA`, la couleur du texte remplacée (ou ajoutée en tête
/// si elle n'y était pas). Le reste — police, corps, cadre — ne bouge pas.
pub(crate) fn replace_da_fill(da: &str, color: Rgb) -> String {
    let tokens: Vec<&str> = da.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len() + 4);
    let mut pending: Vec<&str> = Vec::new();
    let mut replaced = false;
    let op = color_op(color, false);
    for t in tokens {
        match t {
            "g" | "rg" | "k" => {
                if !replaced {
                    out.push(op.clone());
                    replaced = true;
                }
                pending.clear();
            }
            "G" | "RG" | "K" | "Tf" => {
                out.extend(pending.drain(..).map(str::to_string));
                out.push(t.to_string());
            }
            other => pending.push(other),
        }
    }
    out.extend(pending.into_iter().map(str::to_string));
    if !replaced {
        out.insert(0, op);
    }
    out.join(" ")
}

/// Les quatorze polices standard, pour retrouver celle d'un nom de
/// ressource.
const STANDARD_FONTS: [StandardFont; 14] = [
    StandardFont::Helvetica,
    StandardFont::HelveticaBold,
    StandardFont::HelveticaOblique,
    StandardFont::HelveticaBoldOblique,
    StandardFont::TimesRoman,
    StandardFont::TimesBold,
    StandardFont::TimesItalic,
    StandardFont::TimesBoldItalic,
    StandardFont::Courier,
    StandardFont::CourierBold,
    StandardFont::CourierOblique,
    StandardFont::CourierBoldOblique,
    StandardFont::Symbol,
    StandardFont::ZapfDingbats,
];

/// Police standard d'un nom de ressource de `/DA` : ceux qu'écrit Acrobat
/// (`Helv`, `TiRo`…) ou le nom même de la police. Une police inconnue — une
/// police incorporée d'un autre logiciel — retombe sur Helvetica.
fn font_of(name: Option<&str>) -> StandardFont {
    let Some(name) = name else {
        return StandardFont::Helvetica;
    };
    STANDARD_FONTS
        .into_iter()
        .find(|f| freetext::resource_name(*f) == name)
        .or_else(|| StandardFont::from_name(name))
        .unwrap_or(StandardFont::Helvetica)
}

fn num(doc: &Document, d: &Dict, key: &str) -> Option<f64> {
    doc.dict_get(d, key).ok().flatten().and_then(|o| o.as_f64())
}

fn text_of(doc: &Document, d: &Dict, key: &str) -> Option<String> {
    let o = doc.dict_get(d, key).ok().flatten()?;
    match &*o {
        Object::String(s) => Some(acrux_document::text::decode_text_string(s)),
        _ => None,
    }
}

/// Opacité de l'annotation (`/CA`), 1 par défaut.
fn opacity_of(doc: &Document, d: &Dict) -> f64 {
    num(doc, d, "CA")
        .filter(|a| a.is_finite())
        .map_or(1.0, |a| a.clamp(0.0, 1.0))
}

/// Suite de points `[x y x y …]`.
fn points(values: &[f64]) -> Vec<Point> {
    values
        .chunks_exact(2)
        .map(|p| Point::new(p[0], p[1]))
        .collect()
}

/// Terminaisons `/LE` : un tableau de deux noms, ou un seul nom (celle
/// d'une légende).
fn endings(doc: &Document, d: &Dict) -> (LineEnding, LineEnding) {
    let Some(le) = doc.dict_get(d, "LE").ok().flatten() else {
        return (LineEnding::None, LineEnding::None);
    };
    if let Some(n) = le.as_name() {
        let e = LineEnding::from_name(&n.as_str());
        return (e, LineEnding::None);
    }
    let names: Vec<LineEnding> = le
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|o| o.as_name().map(|n| LineEnding::from_name(&n.as_str())))
                .collect()
        })
        .unwrap_or_default();
    (
        names.first().copied().unwrap_or_default(),
        names.get(1).copied().unwrap_or_default(),
    )
}

/// `/RD` : les marges entre `/Rect` et le dessin, gauche, bas, droite, haut.
fn inner_rect(doc: &Document, d: &Dict, rect: Rect) -> Rect {
    match numbers(doc, d, "RD").as_deref() {
        Some([left, bottom, right, top])
            if rect.width() > left + right && rect.height() > bottom + top =>
        {
            Rect::new(
                rect.x0 + left,
                rect.y0 + bottom,
                rect.x1 - right,
                rect.y1 - top,
            )
        }
        _ => rect,
    }
}

/// Style d'une forme lu dans son dictionnaire.
fn shape_style(doc: &Document, d: &Dict) -> ShapeStyle {
    ShapeStyle {
        stroke: numbers(doc, d, "C").and_then(|c| color_from(&c)),
        fill: numbers(doc, d, "IC").and_then(|c| color_from(&c)),
        width: border_width(doc, d).unwrap_or(1.0),
        opacity: opacity_of(doc, d),
    }
    .clamped()
}

/// Ressources d'un état graphique `GS0` : opacité, et pour le surligneur la
/// fusion « produit ».
fn gs_resources(opacity: f64, multiply: bool) -> Dict {
    let mut gs = Dict::new();
    if multiply {
        gs.insert(Name::new("BM"), Object::Name(Name::new("Multiply")));
    }
    if opacity < 1.0 {
        gs.insert(Name::new("CA"), Object::Real(opacity));
        gs.insert(Name::new("ca"), Object::Real(opacity));
    }
    let mut ext = Dict::new();
    ext.insert(Name::new("GS0"), Object::Dict(gs));
    let mut res = Dict::new();
    res.insert(Name::new("ExtGState"), Object::Dict(ext));
    res
}

/// Flux d'un dessin qui ne porte pas lui-même son état graphique : `GS0`
/// ajouté en tête quand l'opacité n'est pas pleine.
fn with_gs(bbox: Rect, content: &str, opacity: f64) -> Object {
    if opacity >= 1.0 {
        return appearance_stream(bbox, content.to_string(), &[]);
    }
    appearance_stream(
        bbox,
        format!("/GS0 gs {content}"),
        &[("Resources", Object::Dict(gs_resources(opacity, false)))],
    )
}

/// Refait l'apparence d'une annotation de type `subtype` d'après son
/// dictionnaire `d`. `None` pour un type qu'on ne sait pas dessiner
/// (tampon, pièce jointe, son, biffure…) ou une géométrie illisible.
#[allow(clippy::too_many_lines)] // une branche par type d'annotation
pub(crate) fn regenerate(doc: &Document, d: &Dict, subtype: &str) -> Option<Regenerated> {
    let r = numbers(doc, d, "Rect").filter(|r| r.len() == 4)?;
    let rect = Rect::new(
        r[0].min(r[2]),
        r[1].min(r[3]),
        r[0].max(r[2]),
        r[1].max(r[3]),
    );
    let simple = |drawn: shapes::Drawn, opacity: f64| Regenerated {
        stream: shape_stream(drawn.bbox, drawn.content, opacity),
        rect: drawn.bbox,
        keys: Vec::new(),
    };
    match subtype {
        "Square" | "Circle" => {
            let style = shape_style(doc, d);
            let zone = inner_rect(doc, d, rect);
            let drawn = if subtype == "Square" {
                shapes::rectangle(&zone, &style)
            } else {
                shapes::ellipse(&zone, &style)
            };
            let mut regen = simple(drawn, style.opacity);
            // Le dessin occupe désormais tout le rectangle.
            regen.keys.push(("RD", None));
            Some(regen)
        }
        "Line" => {
            let style = shape_style(doc, d);
            let l = numbers(doc, d, "L").filter(|l| l.len() >= 4)?;
            let (start, end) = endings(doc, d);
            let drawn = shapes::open_path(
                &[Point::new(l[0], l[1]), Point::new(l[2], l[3])],
                &style,
                start,
                end,
            )?;
            Some(simple(drawn, style.opacity))
        }
        "PolyLine" | "Polygon" => {
            let style = shape_style(doc, d);
            let vertices = points(&numbers(doc, d, "Vertices")?);
            let drawn = if subtype == "Polygon" {
                shapes::polygon(&vertices, &style)?
            } else {
                let (start, end) = endings(doc, d);
                shapes::open_path(&vertices, &style, start, end)?
            };
            Some(simple(drawn, style.opacity))
        }
        "Ink" => {
            let style = shape_style(doc, d);
            let ink = doc.dict_get(d, "InkList").ok().flatten()?;
            let strokes: Vec<Vec<Point>> = ink
                .as_array()?
                .iter()
                .filter_map(|s| {
                    let a = doc.resolve(s).ok()?;
                    let values: Vec<f64> = a
                        .as_array()?
                        .iter()
                        .filter_map(|v| doc.resolve(v).ok().and_then(|x| x.as_f64()))
                        .collect();
                    Some(points(&values))
                })
                .filter(|s| !s.is_empty())
                .collect();
            let drawn = shapes::ink(&strokes, &style)?;
            Some(simple(drawn, style.opacity))
        }
        "Highlight" | "Underline" | "StrikeOut" | "Squiggly" => {
            let kind = match subtype {
                "Highlight" => MarkupKind::Highlight,
                "Underline" => MarkupKind::Underline,
                "StrikeOut" => MarkupKind::StrikeOut,
                _ => MarkupKind::Squiggly,
            };
            let color = numbers(doc, d, "C").and_then(|c| color_from(&c))?;
            let mut quads = numbers(doc, d, "QuadPoints")
                .map(|q| quads_from(&q))
                .unwrap_or_default();
            if quads.is_empty() {
                quads.push(rect);
            }
            let opacity = opacity_of(doc, d);
            let (bbox, content) = markup_appearance(kind, &quads, color);
            let stream = if kind == MarkupKind::Highlight {
                // Même flux que celui de la création, l'opacité dans `GS0`.
                let mut group = Dict::new();
                group.insert(Name::new("S"), Object::Name(Name::new("Transparency")));
                appearance_stream(
                    bbox,
                    content,
                    &[
                        ("Resources", Object::Dict(gs_resources(opacity, true))),
                        ("Group", Object::Dict(group)),
                    ],
                )
            } else {
                with_gs(bbox, &content, opacity)
            };
            Some(Regenerated {
                stream,
                rect: bbox,
                keys: Vec::new(),
            })
        }
        "Caret" => {
            let color = numbers(doc, d, "C")
                .and_then(|c| color_from(&c))
                .unwrap_or(super::CARET_COLOR);
            let mid = f64::midpoint(rect.x0, rect.x1);
            let notch = rect.y0 + 0.25 * rect.height();
            let content = format!(
                "{} {} {} m {} {} l {} {} l {} {} l h f",
                color_op(color, false),
                fmt(rect.x0),
                fmt(rect.y0),
                fmt(mid),
                fmt(rect.y1),
                fmt(rect.x1),
                fmt(rect.y0),
                fmt(mid),
                fmt(notch)
            );
            Some(Regenerated {
                stream: with_gs(rect, &content, opacity_of(doc, d)),
                rect,
                keys: Vec::new(),
            })
        }
        "Text" => {
            let color = numbers(doc, d, "C")
                .and_then(|c| color_from(&c))
                .unwrap_or([1.0, 0.8, 0.0]);
            let content = note_icon(rect, color);
            Some(Regenerated {
                stream: with_gs(rect, &content, opacity_of(doc, d)),
                rect,
                keys: Vec::new(),
            })
        }
        "FreeText" => Some(free_text(doc, d, rect)),
        _ => None,
    }
}

/// Zone de texte et légende : le texte se remet en lignes dans la zone,
/// avec la police, le corps et les couleurs de `/DA`.
fn free_text(doc: &Document, d: &Dict, rect: Rect) -> Regenerated {
    let da = parse_da(&text_of(doc, d, "DA").unwrap_or_default());
    let font = font_of(da.font.as_deref());
    let size = da.size.unwrap_or(12.0).clamp(1.0, 144.0);
    let width = border_width(doc, d).unwrap_or(1.0);
    let text = text_of(doc, d, "Contents").unwrap_or_default();
    let align = match num(doc, d, "Q") {
        Some(q) if (q - 1.0).abs() < 0.5 => TextAlign::Center,
        Some(q) if (q - 2.0).abs() < 0.5 => TextAlign::Right,
        _ => TextAlign::Left,
    };
    #[allow(clippy::cast_possible_truncation)] // un quart de tour, borné
    let rotation = num(doc, d, "Rotate").map_or(0, |r| r.round().clamp(-720.0, 720.0) as i32);
    let rotation = freetext::quarter_turns(rotation);
    let callout = numbers(doc, d, "CL").and_then(|cl| {
        let pts = points(&cl);
        let anchor = *pts.first()?;
        Some(Callout {
            anchor,
            knee: if pts.len() >= 3 {
                pts.get(1).copied()
            } else {
                None
            },
            ending: endings(doc, d).0,
        })
    });
    let inner = if callout.is_some() {
        inner_rect(doc, d, rect)
    } else {
        rect
    };
    let tb = TextBox {
        rect: inner,
        text: &text,
        font,
        size,
        color: da.fill.unwrap_or([0.0, 0.0, 0.0]),
        border: da.stroke.filter(|_| width > 0.0).map(|c| (c, width)),
        fill: numbers(doc, d, "C").and_then(|c| color_from(&c)),
        align,
    };
    let (content, bbox) = freetext::appearance(&tb, callout.as_ref(), rotation);
    let opacity = opacity_of(doc, d);
    let mut res = gs_resources(opacity, false);
    res.insert(
        Name::new("Font"),
        Object::Dict(freetext::font_resources(font)),
    );
    let content = if opacity < 1.0 {
        format!("/GS0 gs {content}")
    } else {
        content
    };
    let mut keys: Vec<(&'static str, Option<Object>)> = vec![(
        "DA",
        Some(Object::String(
            freetext::default_appearance(&tb).into_bytes(),
        )),
    )];
    if let Some(call) = &callout {
        keys.push((
            "CL",
            Some(points_array(&freetext::callout_points_turned(
                &inner, call, rotation,
            ))),
        ));
        keys.push((
            "RD",
            Some(Object::Array(
                freetext::rect_differences(&bbox, &inner)
                    .into_iter()
                    .map(Object::Real)
                    .collect(),
            )),
        ));
    }
    Regenerated {
        stream: appearance_stream(bbox, content, &[("Resources", Object::Dict(res))]),
        rect: bbox,
        keys,
    }
}

/// Enveloppe l'apparence d'une annotation qu'on ne sait pas redessiner (un
/// tampon, une pièce jointe) dans un formulaire qui l'atténue : `/GS0 gs`
/// puis l'apparence d'origine, intacte, dessinée par `Do`. L'opacité vaut
/// ainsi pour tout lecteur, quel que soit le contenu de l'original.
///
/// Une enveloppe déjà posée par nous est remplacée, pas empilée : changer
/// dix fois l'opacité ne fait pas dix formulaires imbriqués.
pub(crate) fn with_opacity(doc: &Document, d: &Dict, opacity: f64) -> Option<Object> {
    let ap = doc.dict_get(d, "AP").ok().flatten()?;
    let Some(Object::Reference(mut inner)) = ap.as_dict()?.get(&Name::new("N")).cloned() else {
        return None;
    };
    let inner_dict = |r| {
        doc.get(r).ok().and_then(|o| match &*o {
            Object::Stream { dict, .. } => Some(dict.clone()),
            _ => None,
        })
    };
    let mut stream = inner_dict(inner)?;
    // Notre enveloppe : son unique XObject est l'original.
    let wrapped = doc
        .dict_get(&stream, "Resources")
        .ok()
        .flatten()
        .and_then(|r| r.as_dict().cloned())
        .and_then(|r| {
            doc.dict_get(&r, "XObject")
                .ok()
                .flatten()
                .and_then(|x| x.as_dict().cloned())
        })
        .and_then(|x| match x.get(&Name::new("AkOriginal")) {
            Some(Object::Reference(o)) => Some(*o),
            _ => None,
        });
    if let Some(original) = wrapped {
        inner = original;
        stream = inner_dict(inner)?;
    }
    let bbox = numbers(doc, &stream, "BBox").filter(|b| b.len() == 4)?;
    let matrix = numbers(doc, &stream, "Matrix")
        .filter(|m| m.len() == 6)
        .map_or(acrux_core::Matrix::IDENTITY, |m| {
            acrux_core::Matrix::new(m[0], m[1], m[2], m[3], m[4], m[5])
        });
    // La boîte de l'enveloppe est celle de l'original **transformée** : la
    // matrice de l'original s'applique au `Do`, pas deux fois.
    let outer = matrix.transform_rect(&Rect::new(bbox[0], bbox[1], bbox[2], bbox[3]));
    let mut res = gs_resources(opacity, false);
    let mut xobjects = Dict::new();
    xobjects.insert(Name::new("AkOriginal"), Object::Reference(inner));
    res.insert(Name::new("XObject"), Object::Dict(xobjects));
    let mut content = String::new();
    if opacity < 1.0 {
        content.push_str("/GS0 gs ");
    }
    content.push_str("/AkOriginal Do");
    Some(appearance_stream(
        outer,
        content,
        &[("Resources", Object::Dict(res))],
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn la_chaine_da_se_lit() {
        let da = parse_da("0 0 1 rg /Helv 12 Tf 1 0 0 RG");
        assert_eq!(da.font.as_deref(), Some("Helv"));
        assert_eq!(da.size, Some(12.0));
        assert_eq!(da.fill, Some([0.0, 0.0, 1.0]));
        assert_eq!(da.stroke, Some([1.0, 0.0, 0.0]));
        let gray = parse_da("/TiRo 9 Tf 0.5 g");
        assert_eq!(gray.fill, Some([0.5, 0.5, 0.5]));
        assert_eq!(font_of(gray.font.as_deref()), StandardFont::TimesRoman);
        assert_eq!(font_of(Some("Inconnue")), StandardFont::Helvetica);
        assert_eq!(parse_da("n'importe quoi"), DaStyle::default());
    }

    #[test]
    fn la_couleur_du_texte_se_remplace_seule() {
        let da = replace_da_fill("0 0 0 rg /Helv 12 Tf 1 0 0 RG", [0.0, 0.5, 1.0]);
        let parsed = parse_da(&da);
        assert_eq!(parsed.fill, Some([0.0, 0.5, 1.0]));
        assert_eq!(parsed.stroke, Some([1.0, 0.0, 0.0]));
        assert_eq!(parsed.size, Some(12.0));
        // Sans couleur de texte, elle s'ajoute.
        let added = parse_da(&replace_da_fill("/Cour 10 Tf", [1.0, 0.0, 0.0]));
        assert_eq!(added.fill, Some([1.0, 0.0, 0.0]));
        assert_eq!(added.font.as_deref(), Some("Cour"));
    }
}
