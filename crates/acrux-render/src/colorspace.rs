//! Espaces colorimétriques (ISO 32000-2 §8.6) : conversion des composantes
//! d'une couleur vers RVB pour l'affichage.
//!
//! État : DeviceGray/RGB/CMYK, CalGray/CalRGB (traités comme les espaces
//! device, comme le fait Acrobat en pratique), Lab (conversion CIE exacte
//! vers sRGB), ICCBased (espace alternatif selon `/N` ; le parseur ICC
//! viendra plus tard), Indexed, Separation, DeviceN, Pattern.
//! La conversion CMJN → RVB est la formule multiplicative simple ; le profil
//! SWOP d'Acrobat sera reproduit avec le support ICC.

use std::rc::Rc;

use acrux_core::{Error, Result};
use acrux_document::{Dict, Document, Name, Object};
use acrux_graphics::{IccCache, IccProfile};

use crate::function::Function;

/// Espace colorimétrique.
#[derive(Debug, Clone)]
pub enum ColorSpace {
    /// Gris 1 composante.
    DeviceGray,
    /// RVB 3 composantes.
    DeviceRgb,
    /// CMJN 4 composantes.
    DeviceCmyk,
    /// CIE L*a*b*.
    Lab {
        /// Point blanc (Xw, Yw, Zw).
        white: [f64; 3],
        /// Bornes de a* et b* : [amin amax bmin bmax].
        range: [f64; 4],
    },
    /// Espace ICC : le profil incorporé quand il est lisible et n'est pas un
    /// sRGB (auquel cas la conversion est inutile), sinon l'espace alternatif.
    IccBased {
        /// Nombre de composantes.
        n: usize,
        /// Espace alternatif (device de même arité par défaut).
        alt: Box<ColorSpace>,
        /// Profil ICC avec cache de conversion, si utilisé.
        profile: Option<Rc<IccCache>>,
    },
    /// Palette.
    Indexed {
        /// Espace de base.
        base: Box<ColorSpace>,
        /// Index maximal.
        hival: u32,
        /// Table : `(hival + 1) × base.components()` octets.
        lookup: Vec<u8>,
    },
    /// Séparation (1 teinte) ou DeviceN (n teintes).
    Separation {
        /// Nombre de teintes.
        n: usize,
        /// Espace alternatif.
        alt: Box<ColorSpace>,
        /// Fonction de transformation des teintes.
        tint: Rc<Function>,
        /// Colorant `/None` : ne peint rien.
        is_none: bool,
        /// Colorant `/All` : peint sur toutes les plaques (rendu comme du gris).
        is_all: bool,
    },
    /// Motif, avec l'espace sous-jacent pour les motifs non colorés.
    Pattern {
        /// Espace des motifs non colorés (`PaintType 2`).
        base: Option<Box<ColorSpace>>,
    },
}

impl ColorSpace {
    /// Espace désigné par un nom de famille device ou par un objet complet.
    /// `resources` permet de résoudre les noms définis dans `/ColorSpace`.
    ///
    /// # Errors
    /// Espace inconnu ou mal formé.
    pub fn parse(doc: &Document, obj: &Object, resources: Option<&Dict>) -> Result<ColorSpace> {
        Self::parse_depth(doc, obj, resources, 0)
    }

    #[allow(clippy::too_many_lines)] // une branche par famille d'espace
    fn parse_depth(
        doc: &Document,
        obj: &Object,
        resources: Option<&Dict>,
        depth: usize,
    ) -> Result<ColorSpace> {
        if depth > 8 {
            return Err(Error::Corrupt(
                "espaces colorimétriques imbriqués trop profondément".into(),
            ));
        }
        let resolved = doc.resolve(obj)?;
        match &*resolved {
            Object::Name(n) => match n.0.as_slice() {
                b"DeviceGray" | b"G" | b"CalGray" => Ok(ColorSpace::DeviceGray),
                b"DeviceRGB" | b"RGB" | b"CalRGB" => Ok(ColorSpace::DeviceRgb),
                b"DeviceCMYK" | b"CMYK" => Ok(ColorSpace::DeviceCmyk),
                b"Pattern" => Ok(ColorSpace::Pattern { base: None }),
                b"Indexed" | b"I" => Err(Error::Corrupt("Indexed sans paramètres".into())),
                _ => {
                    // Nom défini dans les ressources de la page.
                    let cs_dict = resources
                        .and_then(|r| doc.dict_get(r, "ColorSpace").ok().flatten())
                        .and_then(|o| o.as_dict().cloned());
                    let Some(cs_dict) = cs_dict else {
                        return Err(Error::Corrupt(format!(
                            "espace colorimétrique inconnu /{}",
                            n.as_str()
                        )));
                    };
                    let Some(entry) = cs_dict.get(n).cloned() else {
                        return Err(Error::Corrupt(format!(
                            "espace colorimétrique /{} absent des ressources",
                            n.as_str()
                        )));
                    };
                    // Évite la boucle /X → /X.
                    if let Object::Name(m) = &entry {
                        if m == n {
                            return Err(Error::Corrupt(
                                "espace colorimétrique auto-référent".into(),
                            ));
                        }
                    }
                    Self::parse_depth(doc, &entry, None, depth + 1)
                }
            },
            Object::Array(items) => {
                let Some(first) = items.first() else {
                    return Err(Error::Corrupt("espace colorimétrique vide".into()));
                };
                let family = doc.resolve(first)?;
                let Some(family) = family.as_name().map(|n| n.0.clone()) else {
                    return Err(Error::Corrupt(
                        "famille d'espace colorimétrique invalide".into(),
                    ));
                };
                let param = |i: usize| items.get(i).cloned().unwrap_or(Object::Null);
                match family.as_slice() {
                    b"DeviceGray" | b"G" | b"DeviceRGB" | b"RGB" | b"DeviceCMYK" | b"CMYK"
                    | b"Pattern"
                        if items.len() == 1 =>
                    {
                        Self::parse_depth(doc, first, resources, depth + 1)
                    }
                    b"Lab" => {
                        let d = doc
                            .resolve(&param(1))?
                            .as_dict()
                            .cloned()
                            .unwrap_or_default();
                        let white =
                            nums(doc, &d, "WhitePoint").unwrap_or_else(|| vec![0.9505, 1.0, 1.089]);
                        let range = nums(doc, &d, "Range")
                            .unwrap_or_else(|| vec![-100.0, 100.0, -100.0, 100.0]);
                        Ok(ColorSpace::Lab {
                            white: [
                                white.first().copied().unwrap_or(0.9505),
                                white.get(1).copied().unwrap_or(1.0),
                                white.get(2).copied().unwrap_or(1.089),
                            ],
                            range: [
                                range.first().copied().unwrap_or(-100.0),
                                range.get(1).copied().unwrap_or(100.0),
                                range.get(2).copied().unwrap_or(-100.0),
                                range.get(3).copied().unwrap_or(100.0),
                            ],
                        })
                    }
                    b"ICCBased" => {
                        let p1 = param(1);
                        let stream = doc.resolve(&p1)?;
                        let d = stream.as_dict().cloned().unwrap_or_default();
                        let n = doc
                            .dict_get(&d, "N")?
                            .and_then(|o| o.as_i64())
                            .and_then(|n| usize::try_from(n).ok());
                        let alt = match d.get(&Name::new("Alternate")) {
                            Some(a) => Self::parse_depth(doc, a, resources, depth + 1).ok(),
                            None => None,
                        };
                        let n = n
                            .or_else(|| alt.as_ref().map(ColorSpace::components))
                            .unwrap_or(3);
                        let alt = match alt {
                            Some(a) if a.components() == n => a,
                            _ => match n {
                                1 => ColorSpace::DeviceGray,
                                4 => ColorSpace::DeviceCmyk,
                                _ => ColorSpace::DeviceRgb,
                            },
                        };
                        // Profil incorporé : utilisé s'il est lisible, cohérent avec /N
                        // et différent de sRGB (Acrobat convertit via le profil).
                        let profile = doc
                            .stream_data(&stream)
                            .ok()
                            .and_then(|d| IccProfile::parse(&d.data).ok())
                            .filter(|p| p.components() == n && !p.is_srgb_like())
                            .map(|p| Rc::new(IccCache::new(p)));
                        Ok(ColorSpace::IccBased {
                            n,
                            alt: Box::new(alt),
                            profile,
                        })
                    }
                    b"Indexed" | b"I" => {
                        let base = Self::parse_depth(doc, &param(1), resources, depth + 1)?;
                        let hival = doc.resolve(&param(2))?.as_i64().unwrap_or(0).clamp(0, 255);
                        let p3 = param(3);
                        let lookup_obj = doc.resolve(&p3)?;
                        let mut lookup = match &*lookup_obj {
                            Object::String(s) => s.clone(),
                            Object::Stream { .. } => doc.stream_data(&lookup_obj)?.data,
                            _ => Vec::new(),
                        };
                        let needed = (usize::try_from(hival).unwrap_or(0) + 1) * base.components();
                        lookup.resize(needed, 0);
                        Ok(ColorSpace::Indexed {
                            base: Box::new(base),
                            hival: u32::try_from(hival).unwrap_or(0),
                            lookup,
                        })
                    }
                    b"Separation" | b"DeviceN" => {
                        let p1 = param(1);
                        let names = doc.resolve(&p1)?;
                        let (n, is_none, is_all) = match &*names {
                            Object::Name(nm) => (1, nm.0 == b"None", nm.0 == b"All"),
                            Object::Array(a) => {
                                let all_none = !a.is_empty()
                                    && a.iter()
                                        .all(|o| matches!(o, Object::Name(nm) if nm.0 == b"None"));
                                (a.len().max(1), all_none, false)
                            }
                            _ => (1, false, false),
                        };
                        let alt = Self::parse_depth(doc, &param(2), resources, depth + 1)
                            .unwrap_or(ColorSpace::DeviceGray);
                        let tint = Function::parse(doc, &param(3)).map_or_else(
                            |_| {
                                // Sans fonction : teinte → gris inversé.
                                Rc::new(Function::Exponential {
                                    domain: vec![0.0, 1.0],
                                    c0: vec![1.0],
                                    c1: vec![0.0],
                                    n: 1.0,
                                })
                            },
                            Rc::new,
                        );
                        Ok(ColorSpace::Separation {
                            n,
                            alt: Box::new(alt),
                            tint,
                            is_none,
                            is_all,
                        })
                    }
                    b"Pattern" => {
                        let base = if items.len() > 1 {
                            Some(Box::new(Self::parse_depth(
                                doc,
                                &param(1),
                                resources,
                                depth + 1,
                            )?))
                        } else {
                            None
                        };
                        Ok(ColorSpace::Pattern { base })
                    }
                    b"DeviceGray" | b"G" | b"CalGray" => Ok(ColorSpace::DeviceGray),
                    b"DeviceRGB" | b"RGB" | b"CalRGB" => Ok(ColorSpace::DeviceRgb),
                    b"DeviceCMYK" | b"CMYK" => Ok(ColorSpace::DeviceCmyk),
                    other => Err(Error::Unsupported(format!(
                        "espace colorimétrique {}",
                        String::from_utf8_lossy(other)
                    ))),
                }
            }
            Object::Dict(_) | Object::Stream { .. } => {
                // Certains fichiers mettent directement le flux ICC.
                let d = resolved.as_dict().cloned().unwrap_or_default();
                if let Some(n) = d.get(&Name::new("N")).and_then(Object::as_i64) {
                    let n = usize::try_from(n).unwrap_or(3);
                    let alt = match n {
                        1 => ColorSpace::DeviceGray,
                        4 => ColorSpace::DeviceCmyk,
                        _ => ColorSpace::DeviceRgb,
                    };
                    return Ok(ColorSpace::IccBased {
                        n,
                        alt: Box::new(alt),
                        profile: None,
                    });
                }
                Err(Error::Corrupt("espace colorimétrique invalide".into()))
            }
            _ => Err(Error::Corrupt("espace colorimétrique invalide".into())),
        }
    }

    /// Nombre de composantes d'une couleur dans cet espace.
    #[must_use]
    pub fn components(&self) -> usize {
        match self {
            ColorSpace::DeviceGray | ColorSpace::Indexed { .. } | ColorSpace::Pattern { .. } => 1,
            ColorSpace::DeviceRgb | ColorSpace::Lab { .. } => 3,
            ColorSpace::DeviceCmyk => 4,
            ColorSpace::IccBased { n, .. } | ColorSpace::Separation { n, .. } => *n,
        }
    }

    /// Couleur initiale (§8.6.3 : noir, ou tout à 1 pour les séparations).
    #[must_use]
    pub fn initial_color(&self) -> Vec<f64> {
        match self {
            ColorSpace::DeviceCmyk => vec![0.0, 0.0, 0.0, 1.0],
            ColorSpace::Lab { .. } => vec![0.0, 0.0, 0.0],
            ColorSpace::IccBased { n, .. } if *n == 4 => vec![0.0, 0.0, 0.0, 1.0],
            ColorSpace::Separation { n, .. } => vec![1.0; *n],
            other => vec![0.0; other.components()],
        }
    }

    /// Plage de décodage par défaut d'une image (§8.9.5.2, table 90).
    #[must_use]
    pub fn default_decode(&self, bpc: u32) -> Vec<f64> {
        match self {
            ColorSpace::Indexed { .. } => vec![0.0, f64::from((1u32 << bpc.min(16)) - 1)],
            ColorSpace::Lab { range, .. } => {
                vec![0.0, 100.0, range[0], range[1], range[2], range[3]]
            }
            other => (0..other.components()).flat_map(|_| [0.0, 1.0]).collect(),
        }
    }

    /// Vrai pour les espaces de motifs.
    #[must_use]
    pub fn is_pattern(&self) -> bool {
        matches!(self, ColorSpace::Pattern { .. })
    }

    /// Convertit des composantes en RVB (0..1). Les composantes manquantes
    /// valent 0, les excédentaires sont ignorées.
    #[must_use]
    pub fn to_rgb(&self, c: &[f64]) -> [f32; 3] {
        let get = |i: usize| c.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        match self {
            ColorSpace::DeviceGray => {
                #[allow(clippy::cast_possible_truncation)]
                let g = get(0) as f32;
                [g, g, g]
            }
            #[allow(clippy::cast_possible_truncation)]
            ColorSpace::DeviceRgb => [get(0) as f32, get(1) as f32, get(2) as f32],
            ColorSpace::DeviceCmyk => cmyk_to_rgb(get(0), get(1), get(2), get(3)),
            ColorSpace::Lab { white, range } => lab_to_rgb(c, white, range),
            ColorSpace::IccBased { alt, profile, .. } => match profile {
                Some(p) => {
                    let clamped: Vec<f64> = c.iter().map(|v| v.clamp(0.0, 1.0)).collect();
                    p.to_srgb(&clamped)
                }
                None => alt.to_rgb(c),
            },
            ColorSpace::Indexed {
                base,
                hival,
                lookup,
            } => {
                let idx = c.first().copied().unwrap_or(0.0);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let idx = (idx.round().max(0.0) as u32).min(*hival) as usize;
                let n = base.components();
                let comps: Vec<f64> = (0..n)
                    .map(|k| f64::from(*lookup.get(idx * n + k).unwrap_or(&0)) / 255.0)
                    .collect();
                // Lab indexé : les octets couvrent la plage de décodage.
                if let ColorSpace::Lab { range, .. } = &**base {
                    let scaled = [
                        comps[0] * 100.0,
                        range[0] + comps.get(1).copied().unwrap_or(0.0) * (range[1] - range[0]),
                        range[2] + comps.get(2).copied().unwrap_or(0.0) * (range[3] - range[2]),
                    ];
                    return base.to_rgb(&scaled);
                }
                base.to_rgb(&comps)
            }
            ColorSpace::Separation {
                n,
                alt,
                tint,
                is_none,
                is_all,
            } => {
                if *is_none {
                    return [1.0, 1.0, 1.0];
                }
                if *is_all {
                    #[allow(clippy::cast_possible_truncation)]
                    let g = (1.0 - get(0)) as f32;
                    return [g, g, g];
                }
                let inputs: Vec<f64> = (0..*n).map(get).collect();
                let out = tint.eval(&inputs);
                if out.len() >= alt.components() {
                    alt.to_rgb(&out)
                } else {
                    #[allow(clippy::cast_possible_truncation)]
                    let g = (1.0 - get(0)) as f32;
                    [g, g, g]
                }
            }
            ColorSpace::Pattern { .. } => [0.0, 0.0, 0.0],
        }
    }
}

/// CMJN → RVB, formule multiplicative (§10.4.2.3 donne la formule additive
/// simple ; la multiplicative est plus proche du rendu d'Acrobat).
#[must_use]
pub fn cmyk_to_rgb(c: f64, m: f64, y: f64, k: f64) -> [f32; 3] {
    #[allow(clippy::cast_possible_truncation)]
    {
        [
            ((1.0 - c) * (1.0 - k)) as f32,
            ((1.0 - m) * (1.0 - k)) as f32,
            ((1.0 - y) * (1.0 - k)) as f32,
        ]
    }
}

/// CIE L*a*b* → sRGB (§8.6.5.4 pour Lab → XYZ, puis matrice sRGB D65 et gamma).
#[must_use]
#[allow(clippy::many_single_char_names)] // notation CIE (L, a, b, x, y, z)
pub fn lab_to_rgb(c: &[f64], white: &[f64; 3], range: &[f64; 4]) -> [f32; 3] {
    let l = c.first().copied().unwrap_or(0.0).clamp(0.0, 100.0);
    let a = c.get(1).copied().unwrap_or(0.0).clamp(range[0], range[1]);
    let b = c.get(2).copied().unwrap_or(0.0).clamp(range[2], range[3]);
    let m = (l + 16.0) / 116.0;
    let ll = m + a / 500.0;
    let n = m - b / 200.0;
    let g = |x: f64| {
        if x >= 6.0 / 29.0 {
            x * x * x
        } else {
            108.0 / 841.0 * (x - 4.0 / 29.0)
        }
    };
    let x = white[0] * g(ll);
    let y = white[1] * g(m);
    let z = white[2] * g(n);
    // Adaptation grossière : on suppose un blanc proche de D50/D65 et on
    // applique la matrice XYZ → sRGB linéaire (D65).
    let r = 3.2406 * x - 1.5372 * y - 0.4986 * z;
    let gg = -0.9689 * x + 1.8758 * y + 0.0415 * z;
    let bb = 0.0557 * x - 0.2040 * y + 1.0570 * z;
    let gamma = |v: f64| {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.003_130_8 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    };
    #[allow(clippy::cast_possible_truncation)]
    {
        [gamma(r) as f32, gamma(gg) as f32, gamma(bb) as f32]
    }
}

fn nums(doc: &Document, dict: &Dict, key: &str) -> Option<Vec<f64>> {
    let v = doc.dict_get(dict, key).ok().flatten()?;
    let arr = v.as_array()?;
    Some(
        arr.iter()
            .map(|o| doc.resolve(o).ok().and_then(|r| r.as_f64()).unwrap_or(0.0))
            .collect(),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::float_cmp)]
mod tests {
    use super::*;
    use acrux_document::Parser;

    fn doc() -> Document {
        Document::from_bytes(b"%PDF-1.7\n1 0 obj << /Type /Catalog >> endobj\n".to_vec()).unwrap()
    }

    fn cs(src: &str) -> ColorSpace {
        let obj = Parser::new(src.as_bytes()).parse_object().unwrap();
        ColorSpace::parse(&doc(), &obj, None).unwrap()
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b.iter()).all(|(x, y)| (x - y).abs() < 0.01)
    }

    #[test]
    fn device_spaces() {
        assert_eq!(cs("/DeviceGray").components(), 1);
        assert_eq!(cs("/DeviceRGB").components(), 3);
        assert_eq!(cs("/DeviceCMYK").components(), 4);
        assert!(close(cs("/DeviceGray").to_rgb(&[0.5]), [0.5, 0.5, 0.5]));
        assert!(close(
            cs("/DeviceRGB").to_rgb(&[1.0, 0.0, 0.5]),
            [1.0, 0.0, 0.5]
        ));
        assert!(close(
            cs("/DeviceCMYK").to_rgb(&[0.0, 0.0, 0.0, 1.0]),
            [0.0, 0.0, 0.0]
        ));
        assert!(close(
            cs("/DeviceCMYK").to_rgb(&[1.0, 0.0, 0.0, 0.0]),
            [0.0, 1.0, 1.0]
        ));
        assert_eq!(cs("/DeviceCMYK").initial_color(), vec![0.0, 0.0, 0.0, 1.0]);
        assert_eq!(
            cs("[/CalRGB << /WhitePoint [0.95 1 1.09] >>]").components(),
            3
        );
    }

    #[test]
    fn icc_uses_n() {
        let c = cs("[/ICCBased << /N 4 /Length 0 >>]");
        assert_eq!(c.components(), 4);
        assert!(close(c.to_rgb(&[0.0, 0.0, 0.0, 0.0]), [1.0, 1.0, 1.0]));
        assert_eq!(
            cs("[/ICCBased << /N 1 /Alternate /DeviceRGB >>]").components(),
            1,
            "/N prime sur un /Alternate incohérent"
        );
    }

    #[test]
    fn indexed_lookup_and_decode() {
        let c = cs("[/Indexed /DeviceRGB 1 <ff000000ff00>]");
        assert_eq!(c.components(), 1);
        assert!(close(c.to_rgb(&[0.0]), [1.0, 0.0, 0.0]));
        assert!(close(c.to_rgb(&[1.0]), [0.0, 1.0, 0.0]));
        assert!(
            close(c.to_rgb(&[7.0]), [0.0, 1.0, 0.0]),
            "index borné à hival"
        );
        assert_eq!(c.default_decode(8), vec![0.0, 255.0]);
        assert_eq!(
            cs("/DeviceRGB").default_decode(8),
            vec![0.0, 1.0, 0.0, 1.0, 0.0, 1.0]
        );
    }

    #[test]
    fn separation_with_tint_transform() {
        let c = cs("[/Separation /Spot /DeviceRGB << /FunctionType 2 /Domain [0 1] /C0 [1 1 1] /C1 [1 0 0] /N 1 >>]");
        assert_eq!(c.components(), 1);
        assert_eq!(c.initial_color(), vec![1.0]);
        assert!(close(c.to_rgb(&[0.0]), [1.0, 1.0, 1.0]));
        assert!(close(c.to_rgb(&[1.0]), [1.0, 0.0, 0.0]));
        let none = cs("[/Separation /None /DeviceGray << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>]");
        assert!(close(none.to_rgb(&[1.0]), [1.0, 1.0, 1.0]));
        let all = cs("[/Separation /All /DeviceGray << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>]");
        assert!(close(all.to_rgb(&[1.0]), [0.0, 0.0, 0.0]));
        let dn = cs("[/DeviceN [/A /B] /DeviceRGB << /FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1] /Length 12 >>]");
        assert_eq!(dn.components(), 2);
    }

    #[test]
    fn lab_white_and_black() {
        let c = cs("[/Lab << /WhitePoint [0.9505 1 1.089] /Range [-100 100 -100 100] >>]");
        assert!(close(c.to_rgb(&[100.0, 0.0, 0.0]), [1.0, 1.0, 1.0]));
        assert!(close(c.to_rgb(&[0.0, 0.0, 0.0]), [0.0, 0.0, 0.0]));
        let red = c.to_rgb(&[54.0, 81.0, 70.0]);
        assert!(red[0] > 0.9 && red[1] < 0.2 && red[2] < 0.2, "{red:?}");
        assert_eq!(
            c.default_decode(8),
            vec![0.0, 100.0, -100.0, 100.0, -100.0, 100.0]
        );
    }

    #[test]
    fn named_resource_and_errors() {
        let d = doc();
        let res =
            Parser::new(b"<< /ColorSpace << /CS0 [/Indexed /DeviceGray 0 <80>] /Loop /Loop >> >>")
                .parse_object()
                .unwrap();
        let res = res.as_dict().unwrap().clone();
        let c = ColorSpace::parse(&d, &Object::Name(Name::new("CS0")), Some(&res)).unwrap();
        assert!(close(c.to_rgb(&[0.0]), [0.5, 0.5, 0.5]));
        assert!(ColorSpace::parse(&d, &Object::Name(Name::new("Loop")), Some(&res)).is_err());
        assert!(ColorSpace::parse(&d, &Object::Name(Name::new("Nope")), Some(&res)).is_err());
        assert!(ColorSpace::parse(&d, &Object::Integer(3), None).is_err());
        assert!(matches!(
            cs("[/Pattern /DeviceRGB]"),
            ColorSpace::Pattern { base: Some(_) }
        ));
        assert!(cs("/Pattern").is_pattern());
    }
}
