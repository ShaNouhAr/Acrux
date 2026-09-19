//! Détourage d'une signature photographiée ou numérisée.
//!
//! On signe une feuille blanche, on la photographie, et on obtient une image
//! opaque : posée sur un contrat, elle y colle un rectangle de papier gris.
//! Ce module en retire le papier — l'image garde son encre et devient
//! transparente partout ailleurs.
//!
//! # Comment le fond est reconnu
//!
//! Sans rien savoir de l'éclairage ni de la couleur du papier, on regarde la
//! **distribution des luminances**. Une photo de signature est faite à plus de
//! 90 % de papier : le haut de la distribution donne donc le blanc de
//! référence, et le bas l'encre. Entre les deux, une rampe adoucie transforme
//! la luminance en opacité, ce qui conserve l'anticrénelage des bords du trait
//! au lieu de le découper au ciseau.
//!
//! Un PNG qui porte déjà de la transparence utile est respecté tel quel : la
//! personne a déjà fait le travail, ou l'a obtenu d'un autre outil.
//!
//! # Ce qui est écrit
//!
//! Une image de base et son `/SMask`, tous deux rognés à ce que l'encre occupe
//! vraiment. Recolorée, l'image de base est un aplat de la couleur demandée —
//! constant, donc réduit à presque rien par la compression — et tout le dessin
//! vit dans le masque.

use acrux_core::{Error, Result};
use acrux_document::{Dict, Document, Name, Object};

use super::{fmt, Form, Rgb};
use crate::stamp::image::{decode, png_raster, Raster};

/// Réglages du détourage.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// Largeur de la rampe entre papier et encre, dans `[0.05, 0.9]`. Plus
    /// elle est grande, plus les bords sont doux — et plus le papier sale
    /// risque de rester.
    pub softness: f64,
    /// Remplace la couleur de l'encre par celle de la signature.
    pub recolor: bool,
    /// Marge gardée autour de l'encre, en fraction de la plus grande dimension.
    pub margin: f64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            softness: 0.38,
            recolor: true,
            margin: 0.02,
        }
    }
}

/// Construit le dessin d'une signature importée.
///
/// `options` à `None` pose l'image telle quelle, fond compris.
pub(super) fn form(
    doc: &Document,
    data: &[u8],
    options: Option<&Options>,
    color: Rgb,
) -> Result<Form> {
    let Some(options) = options else {
        let image = decode(data)?;
        let (w, h) = (f64::from(image.width), f64::from(image.height));
        return Ok(draw(doc, image.write(doc), w, h));
    };
    let raster = pixels(data)?;
    let cut = key(&raster, options)?;
    let reference = write_image(doc, &cut, options.recolor, color);
    Ok(draw(
        doc,
        reference,
        f64::from(cut.width),
        f64::from(cut.height),
    ))
}

/// Flux de contenu qui dessine une image occupant toute la boîte.
fn draw(doc: &Document, reference: acrux_document::ObjectRef, width: f64, height: f64) -> Form {
    let mut xobjects = Dict::new();
    xobjects.insert(Name::new("AkSig"), Object::Reference(reference));
    let mut resources = Dict::new();
    resources.insert(Name::new("XObject"), Object::Dict(xobjects));
    let _ = doc;
    Form {
        content: format!("q {} 0 0 {} 0 0 cm /AkSig Do Q\n", fmt(width), fmt(height)),
        width,
        height,
        resources,
    }
}

/// Image détourée et rognée.
struct Cut {
    width: u32,
    height: u32,
    /// Composantes RVB, trois octets par pixel.
    rgb: Vec<u8>,
    /// Opacité, un octet par pixel.
    alpha: Vec<u8>,
}

/// Décode une image en pixels, PNG ou JPEG.
fn pixels(data: &[u8]) -> Result<Raster> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return png_raster(data);
    }
    if data.starts_with(&[0xFF, 0xD8]) {
        let image = acrux_codecs::dct::decode(data)?;
        let components = match image.components {
            1 => 1,
            3 => 3,
            n => {
                return Err(Error::Unsupported(format!(
                    "signature importée : JPEG à {n} composantes non pris en charge"
                )))
            }
        };
        return Ok(Raster {
            width: image.width,
            height: image.height,
            components,
            data: image.data,
            alpha: None,
        });
    }
    Err(Error::Unsupported(
        "signature importée : seuls les fichiers PNG et JPEG sont acceptés".into(),
    ))
}

/// Luminance perçue d'un pixel, sur 0–255.
fn luma(raster: &Raster, index: usize) -> u8 {
    if raster.components == 1 {
        return raster.data.get(index).copied().unwrap_or(255);
    }
    let base = index * 3;
    let r = f64::from(raster.data.get(base).copied().unwrap_or(255));
    let g = f64::from(raster.data.get(base + 1).copied().unwrap_or(255));
    let b = f64::from(raster.data.get(base + 2).copied().unwrap_or(255));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // borné à 0–255
    let value = 0.114_f64
        .mul_add(b, 0.299_f64.mul_add(r, 0.587 * g))
        .round() as u8;
    value
}

/// Sépare l'encre du papier et rogne au plus juste.
fn key(raster: &Raster, options: &Options) -> Result<Cut> {
    let pixels = (raster.width as usize) * (raster.height as usize);
    if pixels == 0 {
        return Err(Error::Corrupt("signature importée : image vide".into()));
    }
    let alpha = match &raster.alpha {
        // Une transparence déjà présente et utile est respectée.
        Some(existing) if useful(existing) => existing.clone(),
        _ => from_luminance(raster, pixels, options),
    };
    crop(raster, &alpha, options.margin)
}

/// Vrai si un canal alpha porte une vraie information.
///
/// Un PNG enregistré « avec transparence » mais entièrement opaque n'en porte
/// aucune ; quelques pixels translucides au bord non plus.
fn useful(alpha: &[u8]) -> bool {
    let transparent = alpha.iter().filter(|&&v| v < 200).count();
    transparent * 40 > alpha.len()
}

/// Opacité déduite de la luminance, corrigée de l'éclairage.
///
/// Un seuil unique pour toute l'image ne tient pas : une photo prise au
/// téléphone a un côté plus clair que l'autre, une ombre dans un coin, un pli
/// qui assombrit une bande. Le papier est donc estimé **localement** — c'est
/// le maximum de luminance dans un voisinage plus large que le trait, adouci
/// — et chaque pixel est jugé par rapport à son propre voisinage. Ce qui
/// compte n'est plus « ce pixel est-il sombre ? » mais « ce pixel est-il plus
/// sombre que le papier autour de lui ? », et l'ombre d'un coin n'est plus
/// prise pour de l'encre.
fn from_luminance(raster: &Raster, pixels: usize, options: &Options) -> Vec<u8> {
    let luminance: Vec<u8> = (0..pixels).map(|i| luma(raster, i)).collect();
    let (w, h) = (raster.width as usize, raster.height as usize);
    // Le voisinage doit dépasser l'épaisseur d'un trait, sinon le papier
    // estimé au milieu d'un trait épais serait le trait lui-même.
    let radius = (w.min(h) / 12).clamp(6, 64);
    let background = local_paper(&luminance, w, h, radius);

    let mut histogram = [0u32; 256];
    for value in &luminance {
        histogram[usize::from(*value)] += 1;
    }
    let paper = f64::from(percentile(&histogram, pixels, 0.92)).max(1.0);
    let ink = f64::from(percentile(&histogram, pixels, 0.03));
    // Profondeur de l'encre la plus sombre, en proportion du papier : c'est
    // elle qui règle où placer la rampe.
    let depth_max = (1.0 - ink / paper).clamp(0.08, 1.0);
    if depth_max < 0.1 {
        // Image sans contraste : rien à retirer.
        return vec![255; pixels];
    }
    let softness = options.softness.clamp(0.05, 0.9);
    let middle = depth_max * 0.45;
    let half = depth_max * softness * 0.5;
    let low = (middle - half).max(0.02);
    let high = (middle + half).max(low + 0.02);

    let mut out = Vec::with_capacity(pixels);
    for index in 0..pixels {
        let local = f64::from(background[index]).max(1.0);
        // Réflectance : 0 sur le papier quel que soit l'éclairage, proche de 1
        // sur l'encre. C'est la correction de champ plat des scanners.
        let depth = (1.0 - f64::from(luminance[index]) / local).clamp(0.0, 1.0);
        let t = ((depth - low) / (high - low)).clamp(0.0, 1.0);
        let value = t * t * (3.0 - 2.0 * t);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // borné à 0–255
        out.push((value * 255.0).round() as u8);
    }
    out
}

/// Niveau du papier en chaque point : maximum local, puis adouci.
///
/// Les deux filtres sont séparables — une passe horizontale, une passe
/// verticale — et le maximum glissant se calcule en temps constant par pixel
/// avec une file monotone. Le coût est donc linéaire, quelle que soit la
/// taille du voisinage.
fn local_paper(luminance: &[u8], w: usize, h: usize, radius: usize) -> Vec<u8> {
    let mut max = vec![0u8; luminance.len()];
    let mut row = vec![0u8; w.max(h)];
    for y in 0..h {
        sliding_max(&luminance[y * w..(y + 1) * w], radius, &mut row[..w]);
        max[y * w..(y + 1) * w].copy_from_slice(&row[..w]);
    }
    let mut column = vec![0u8; h];
    let mut out = vec![0u8; luminance.len()];
    for x in 0..w {
        for (y, value) in column.iter_mut().enumerate() {
            *value = max[y * w + x];
        }
        let source = column.clone();
        sliding_max(&source, radius, &mut column);
        for (y, value) in column.iter().enumerate() {
            out[y * w + x] = *value;
        }
    }
    // Le maximum local a des marches ; un flou de la même portée les efface,
    // sinon le fond estimé laisserait des bandes visibles dans l'opacité.
    blur(&mut out, w, h, radius);
    out
}

/// Maximum glissant sur une fenêtre de `2 · radius + 1`, en temps linéaire.
fn sliding_max(input: &[u8], radius: usize, out: &mut [u8]) {
    let n = input.len();
    if n == 0 {
        return;
    }
    // File monotone décroissante d'indices : la tête est le maximum courant.
    let mut queue: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    let mut next = 0usize;
    #[allow(clippy::needless_range_loop)] // l'indice sert des deux côtés, pas seulement à lire
    for i in 0..n {
        let end = (i + radius + 1).min(n);
        while next < end {
            while queue.back().is_some_and(|&j| input[j] <= input[next]) {
                queue.pop_back();
            }
            queue.push_back(next);
            next += 1;
        }
        let start = i.saturating_sub(radius);
        while queue.front().is_some_and(|&j| j < start) {
            queue.pop_front();
        }
        out[i] = queue.front().map_or(input[i], |&j| input[j]);
    }
}

/// Flou en caisson, deux passes séparables.
fn blur(data: &mut [u8], w: usize, h: usize, radius: usize) {
    let window = radius * 2 + 1;
    let mut line = vec![0u8; w.max(h)];
    for y in 0..h {
        let row: Vec<u8> = data[y * w..(y + 1) * w].to_vec();
        box_pass(&row, window, &mut line[..w]);
        data[y * w..(y + 1) * w].copy_from_slice(&line[..w]);
    }
    let mut column = vec![0u8; h];
    for x in 0..w {
        for (y, value) in column.iter_mut().enumerate() {
            *value = data[y * w + x];
        }
        let source = column.clone();
        box_pass(&source, window, &mut column);
        for (y, value) in column.iter().enumerate() {
            data[y * w + x] = *value;
        }
    }
}

/// Moyenne glissante, bords prolongés par la valeur du bord.
fn box_pass(input: &[u8], window: usize, out: &mut [u8]) {
    let n = input.len();
    if n == 0 {
        return;
    }
    let radius = window / 2;
    let mut sum: u32 = input.iter().take(radius + 1).map(|v| u32::from(*v)).sum();
    for (i, slot) in out.iter_mut().enumerate().take(n) {
        let count = (i + radius).min(n - 1) - i.saturating_sub(radius) + 1;
        #[allow(clippy::cast_possible_truncation)] // moyenne d'octets, donc un octet
        {
            *slot = (sum / u32::try_from(count).unwrap_or(1)) as u8;
        }
        let leaving = i.saturating_sub(radius);
        let entering = i + radius + 1;
        if entering < n {
            sum += u32::from(input[entering]);
        }
        if i >= radius && leaving < n {
            sum -= u32::from(input[leaving]);
        }
    }
}

/// Valeur de luminance sous laquelle se trouve la fraction demandée des pixels.
fn percentile(histogram: &[u32; 256], pixels: usize, fraction: f64) -> u8 {
    #[allow(clippy::cast_precision_loss)] // nombre de pixels, très loin de 2^53
    let target = (pixels as f64 * fraction).max(1.0);
    let mut seen = 0f64;
    for (value, count) in histogram.iter().enumerate() {
        seen += f64::from(*count);
        if seen >= target {
            #[allow(clippy::cast_possible_truncation)] // l'indice reste sous 256
            return value as u8;
        }
    }
    255
}

/// Rogne l'image à ce que l'encre occupe, plus une marge.
fn crop(raster: &Raster, alpha: &[u8], margin: f64) -> Result<Cut> {
    let (w, h) = (raster.width as usize, raster.height as usize);
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            if alpha.get(y * w + x).copied().unwrap_or(0) > 12 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x1 < x0 || y1 < y0 {
        return Err(Error::Corrupt(
            "signature importée : aucune encre trouvée sur l'image".into(),
        ));
    }
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let pad = (margin.clamp(0.0, 0.25) * w.max(h) as f64).round() as usize;
    let x0 = x0.saturating_sub(pad);
    let y0 = y0.saturating_sub(pad);
    let x1 = (x1 + pad).min(w - 1);
    let y1 = (y1 + pad).min(h - 1);
    let (cw, ch) = (x1 - x0 + 1, y1 - y0 + 1);
    let mut rgb = Vec::with_capacity(cw * ch * 3);
    let mut out_alpha = Vec::with_capacity(cw * ch);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let index = y * w + x;
            if raster.components == 1 {
                let v = raster.data.get(index).copied().unwrap_or(255);
                rgb.extend_from_slice(&[v, v, v]);
            } else {
                let base = index * 3;
                rgb.extend_from_slice(raster.data.get(base..base + 3).unwrap_or(&[255, 255, 255]));
            }
            out_alpha.push(alpha.get(index).copied().unwrap_or(0));
        }
    }
    Ok(Cut {
        width: u32::try_from(cw).unwrap_or(1),
        height: u32::try_from(ch).unwrap_or(1),
        rgb,
        alpha: out_alpha,
    })
}

/// Écrit l'image de base et son masque.
fn write_image(doc: &Document, cut: &Cut, recolor: bool, color: Rgb) -> acrux_document::ObjectRef {
    let pixels = (cut.width as usize) * (cut.height as usize);
    let base = if recolor {
        // Aplat de la couleur d'encre, aux mêmes dimensions que le masque : le
        // `/SMask` est rééchantillonné à la taille de l'image de base
        // (§11.6.5.3), un aplat de 1 × 1 réduirait donc le dessin à un pixel.
        // Constant, il ne coûte presque rien une fois compressé.
        let byte = |v: f64| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // borné à 0–255
            let b = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            b
        };
        let rgb = [byte(color[0]), byte(color[1]), byte(color[2])];
        let mut flat = Vec::with_capacity(pixels * 3);
        for _ in 0..pixels {
            flat.extend_from_slice(&rgb);
        }
        flat
    } else {
        cut.rgb.clone()
    };
    let mut dict = image_dict(cut.width, cut.height, "DeviceRGB");
    dict.insert(Name::new("Filter"), Object::Name(Name::new("FlateDecode")));
    let mut mask = image_dict(cut.width, cut.height, "DeviceGray");
    mask.insert(Name::new("Filter"), Object::Name(Name::new("FlateDecode")));
    let mask_ref = doc.add(stream(mask, acrux_codecs::flate::compress(&cut.alpha, 6)));
    dict.insert(Name::new("SMask"), Object::Reference(mask_ref));
    doc.add(stream(dict, acrux_codecs::flate::compress(&base, 6)))
}

fn image_dict(width: u32, height: u32, space: &str) -> Dict {
    let mut d = Dict::new();
    d.insert(Name::new("Type"), Object::Name(Name::new("XObject")));
    d.insert(Name::new("Subtype"), Object::Name(Name::new("Image")));
    d.insert(Name::new("Width"), Object::Integer(i64::from(width)));
    d.insert(Name::new("Height"), Object::Integer(i64::from(height)));
    d.insert(Name::new("ColorSpace"), Object::Name(Name::new(space)));
    d.insert(Name::new("BitsPerComponent"), Object::Integer(8));
    d
}

fn stream(mut dict: Dict, raw: Vec<u8>) -> Object {
    dict.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
    );
    Object::Stream { dict, raw }
}
