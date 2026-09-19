//! Décodage des images (ISO 32000-2 §8.9) : XObjects image et images en
//! ligne → pixels RVB 8 bits + alpha, prêts pour le rasteriseur.
//!
//! Gère `/BitsPerComponent` 1, 2, 4, 8, 16, tous les espaces colorimétriques
//! de `colorspace`, `/Decode`, `/ImageMask` (pochoir), `/SMask` (alpha
//! doux), `/Mask` (pochoir ou clé de couleur), et les quatre filtres d'image
//! via `acrux-codecs` : DCT (JPEG), JPX (JPEG 2000, avec `/SMaskInData` et
//! l'espace de couleur du conteneur JP2), CCITT (fax G3/G4) et JBIG2 (avec
//! `/JBIG2Globals`). Un flux d'image illisible est rendu en gris moyen pour
//! ne pas masquer la mise en page.

// Conversions d'échantillons entières volontaires (valeurs bornées par bpc),
// `decode_image` qui suit les étapes de §8.9.5 dans l'ordre, et fonctions de
// bas niveau qui reçoivent chaque paramètre d'image séparément.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    clippy::too_many_arguments
)]

use std::rc::Rc;

use acrux_codecs::jpx::{JpxColorSpace, JpxOptions};
use acrux_core::{Error, Result};
use acrux_document::{Dict, Document, Name, Object};
use acrux_graphics::{IccCache, IccProfile};

use crate::colorspace::ColorSpace;

/// Image décodée.
#[derive(Debug, Clone)]
pub struct DecodedImage {
    /// Largeur en pixels.
    pub width: u32,
    /// Hauteur en pixels.
    pub height: u32,
    /// RVB entrelacé, 3 octets par pixel, ligne par ligne depuis le haut.
    /// Vide pour un pochoir (`is_stencil`).
    pub rgb: Vec<u8>,
    /// Alpha 0..255 par pixel (`None` = opaque).
    pub alpha: Option<Vec<u8>>,
    /// Pochoir (`/ImageMask true`) : `alpha` contient la couverture à peindre
    /// avec la couleur de remplissage courante.
    pub is_stencil: bool,
    /// `/Interpolate`.
    pub interpolate: bool,
    /// Avertissements (filtre non pris en charge, données tronquées…).
    pub warnings: Vec<String>,
}

/// Limite de sécurité : 256 mégapixels.
const MAX_PIXELS: u64 = 256 * 1024 * 1024;

/// Décode une image XObject (ou en ligne : passer `dict` avec les clés
/// abrégées et `data` déjà passé par les filtres standard).
///
/// # Errors
/// Dimensions absentes ou absurdes.
pub fn decode_image(
    doc: &Document,
    dict: &Dict,
    data: &[u8],
    image_filter: Option<(&[u8], &acrux_codecs::DecodeParms)>,
    resources: Option<&Dict>,
) -> Result<DecodedImage> {
    let get = |long: &str, short: &str| -> Option<Object> {
        let o = dict
            .get(&Name::new(long))
            .or_else(|| dict.get(&Name::new(short)))?;
        doc.resolve(o).ok().map(|r| (*r).clone())
    };
    let width = get("Width", "W").and_then(|o| o.as_i64()).unwrap_or(0);
    let height = get("Height", "H").and_then(|o| o.as_i64()).unwrap_or(0);
    if width <= 0 || height <= 0 {
        return Err(Error::Corrupt("image sans dimensions".into()));
    }
    let (width, height) = (
        u32::try_from(width).unwrap_or(u32::MAX),
        u32::try_from(height).unwrap_or(u32::MAX),
    );
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(Error::Corrupt(format!(
            "image trop grande : {width}×{height}"
        )));
    }
    let is_stencil = matches!(get("ImageMask", "IM"), Some(Object::Bool(true)));
    let interpolate = matches!(get("Interpolate", "I"), Some(Object::Bool(true)));
    let mut bpc = get("BitsPerComponent", "BPC")
        .and_then(|o| o.as_i64())
        .unwrap_or(if is_stencil { 1 } else { 8 });
    let mut warnings = Vec::new();
    let decode_arr: Option<Vec<f64>> = get("Decode", "D").and_then(|o| {
        o.as_array().map(|a| {
            a.iter()
                .map(|v| doc.resolve(v).ok().and_then(|r| r.as_f64()).unwrap_or(0.0))
                .collect()
        })
    });

    // 1. Pochoir.
    if is_stencil {
        let n = usize::try_from(u64::from(width) * u64::from(height)).unwrap_or(0);
        let row_bytes = (width as usize).div_ceil(8);
        let mut alpha = vec![0u8; n];
        // Decode [1 0] inverse le sens : par défaut, 0 = peint (§8.9.6.2).
        let paint_zero = decode_arr
            .as_ref()
            .is_none_or(|d| d.first().copied().unwrap_or(0.0) < 0.5);
        let bits = if image_filter.is_some() {
            expand_image_filter(image_filter, data, width, height, 1, &mut warnings, true)
        } else {
            data.to_vec()
        };
        for y in 0..height as usize {
            for x in 0..width as usize {
                let byte = bits.get(y * row_bytes + x / 8).copied().unwrap_or(0xFF);
                let bit = (byte >> (7 - (x % 8))) & 1;
                let paint = (bit == 0) == paint_zero;
                alpha[y * width as usize + x] = if paint { 255 } else { 0 };
            }
        }
        return Ok(DecodedImage {
            width,
            height,
            rgb: Vec::new(),
            alpha: Some(alpha),
            is_stencil: true,
            interpolate,
            warnings,
        });
    }

    // 2. Espace colorimétrique.
    let cs_obj = get("ColorSpace", "CS");
    let mut cs = match &cs_obj {
        Some(o) => ColorSpace::parse(doc, o, resources).unwrap_or_else(|e| {
            warnings.push(format!(
                "espace colorimétrique illisible ({e}) : gris supposé"
            ));
            ColorSpace::DeviceGray
        }),
        None => ColorSpace::DeviceGray,
    };

    // 3. Données : filtre d'image éventuel.
    let mut ncomp = cs.components();
    let samples: Vec<u8>; // échantillons dépaquetés à `bpc` bits, ou 8 bits après DCT/JPX
    let mut jpeg_cmyk_inverted = false;
    // Canal d'opacité porté par le JPX lui-même (/SMaskInData).
    let mut jpx_alpha: Option<Vec<u8>> = None;
    let mut decode_arr = decode_arr;
    if let Some((name, parms)) = image_filter {
        match name {
            b"DCTDecode" | b"DCT" => match acrux_codecs::dct::decode(data) {
                Ok(img) => {
                    if img.width != width || img.height != height {
                        warnings.push(format!(
                            "JPEG {}×{} pour une image déclarée {width}×{height}",
                            img.width, img.height
                        ));
                    }
                    // Le JPEG impose ses composantes : gris, RVB ou CMJN.
                    let jc = usize::from(img.components);
                    if jc != ncomp {
                        cs = match jc {
                            1 => ColorSpace::DeviceGray,
                            4 => ColorSpace::DeviceCmyk,
                            _ => ColorSpace::DeviceRgb,
                        };
                        ncomp = jc;
                    }
                    jpeg_cmyk_inverted = img.adobe_inverted && jc == 4;
                    bpc = 8;
                    samples = resample_to(&img.data, img.width, img.height, jc, width, height);
                }
                Err(e) => {
                    warnings.push(format!("JPEG illisible : {e}"));
                    return Ok(gray_placeholder(width, height, interpolate, warnings));
                }
            },
            b"JPXDecode" => {
                let smask_in_data = get("SMaskInData", "SMaskInData")
                    .and_then(|o| o.as_i64())
                    .unwrap_or(0)
                    > 0;
                let options = JpxOptions {
                    max_resolution_reduction: 0,
                    smask_in_data,
                };
                match acrux_codecs::jpx::decode_with_options(data, &options) {
                    Ok(img) => {
                        if img.width != width || img.height != height {
                            warnings.push(format!(
                                "JPEG 2000 {}×{} pour une image déclarée {width}×{height}",
                                img.width, img.height
                            ));
                        }
                        let total = usize::from(img.components).max(1);
                        let colour = if img.has_alpha { total - 1 } else { total };
                        // /ColorSpace absent ou incompatible : celui du conteneur
                        // JP2 fait foi (§8.9.5, tableau 89, note sur JPXDecode).
                        if cs_obj.is_none() || ncomp != colour {
                            cs = jpx_colorspace(&img.colorspace, colour);
                            ncomp = cs.components();
                        }
                        // /Decode est ignoré pour JPX sauf avec un espace indexé.
                        if !matches!(cs, ColorSpace::Indexed { .. }) {
                            decode_arr = None;
                        }
                        let (colour_data, alpha) = split_alpha(&img.data, total, img.has_alpha);
                        if let Some(a) = alpha {
                            jpx_alpha =
                                Some(resample_to(&a, img.width, img.height, 1, width, height));
                        }
                        bpc = 8;
                        samples =
                            resample_to(&colour_data, img.width, img.height, colour, width, height);
                    }
                    Err(e) => {
                        warnings.push(format!("JPEG 2000 illisible : {e}"));
                        return Ok(gray_placeholder(width, height, interpolate, warnings));
                    }
                }
            }
            b"CCITTFaxDecode" | b"CCF" => {
                let fax = parms.ccitt();
                match acrux_codecs::ccitt::decode(data, &fax) {
                    Ok(bits) => {
                        if ncomp != 1 {
                            warnings.push("CCITT : espace à une composante attendu".into());
                            cs = ColorSpace::DeviceGray;
                            ncomp = 1;
                        }
                        bpc = 1;
                        // Blanc = 1 sauf BlackIs1 : les lignes manquantes sont blanches.
                        let white = if fax.black_is_1 { 0x00 } else { 0xFF };
                        samples = repack_rows(&bits, fax.columns, width, height, white);
                    }
                    Err(e) => {
                        warnings.push(format!("CCITT illisible : {e}"));
                        return Ok(gray_placeholder(width, height, interpolate, warnings));
                    }
                }
            }
            b"JBIG2Decode" => {
                match acrux_codecs::jbig2::decode(
                    data,
                    parms.jbig2_globals.as_deref(),
                    width,
                    height,
                ) {
                    Ok(bits) => {
                        if ncomp != 1 {
                            warnings.push("JBIG2 : espace à une composante attendu".into());
                            cs = ColorSpace::DeviceGray;
                            ncomp = 1;
                        }
                        bpc = 1;
                        samples = bits;
                    }
                    Err(e) => {
                        warnings.push(format!("JBIG2 illisible : {e}"));
                        return Ok(gray_placeholder(width, height, interpolate, warnings));
                    }
                }
            }
            other => {
                warnings.push(format!(
                    "filtre {} non pris en charge",
                    String::from_utf8_lossy(other)
                ));
                return Ok(gray_placeholder(width, height, interpolate, warnings));
            }
        }
    } else {
        samples = data.to_vec();
    }
    if ![1, 2, 4, 8, 16].contains(&bpc) {
        warnings.push(format!("BitsPerComponent {bpc} invalide : 8 supposé"));
        bpc = 8;
    }
    let bpc = bpc as u32;

    // 4. Conversion en RVB.
    let n = width as usize * height as usize;
    let mut rgb = vec![0u8; n * 3];
    let max = f64::from((1u32 << bpc) - 1);
    let mut decode = cs.default_decode(bpc);
    if let Some(d) = &decode_arr {
        if d.len() >= 2 * ncomp {
            decode.clone_from(d);
        }
    }
    if jpeg_cmyk_inverted {
        // Photoshop stocke les JPEG CMJN inversés (APP14) : on inverse, sauf si
        // /Decode [1 0 …] le fait déjà.
        let already = decode.first().copied().unwrap_or(0.0) > 0.5;
        decode = if already {
            vec![0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0]
        } else {
            vec![1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0]
        };
    }
    let row_bits = width as usize * ncomp * bpc as usize;
    let row_bytes = row_bits.div_ceil(8);
    // Cache pour les espaces à une composante (indexé, gris, séparation) : au
    // plus 2^bpc valeurs distinctes.
    let mut cache: Vec<Option<[u8; 3]>> = if ncomp == 1 && bpc <= 8 {
        vec![None; 1 << bpc]
    } else {
        Vec::new()
    };
    let mut comps = vec![0.0f64; ncomp];
    let truncated = samples.len() < row_bytes * height as usize;
    if truncated {
        warnings.push("données d'image tronquées".into());
    }
    for y in 0..height as usize {
        let row = y * row_bytes;
        for x in 0..width as usize {
            let px = y * width as usize + x;
            if ncomp == 1 && bpc <= 8 {
                let raw = read_sample(&samples, row, x, bpc);
                let idx = raw as usize;
                let c = if let Some(c) = cache.get(idx).copied().flatten() {
                    c
                } else {
                    let v = f64::from(raw);
                    let dmin = decode[0];
                    let dmax = decode[1];
                    // Formule unique de §8.9.5.2 : pour un espace indexé, le
                    // /Decode par défaut est [0 2^bpc−1] et `max` = 2^bpc−1,
                    // donc `val` est directement l'indice de la table.
                    let val = dmin + v * (dmax - dmin) / max;
                    let c = to_rgb8(cs.to_rgb(&[val]));
                    if let Some(slot) = cache.get_mut(idx) {
                        *slot = Some(c);
                    }
                    c
                };
                rgb[px * 3..px * 3 + 3].copy_from_slice(&c);
            } else {
                for (k, comp) in comps.iter_mut().enumerate() {
                    let raw = read_sample(&samples, row, x * ncomp + k, bpc);
                    let dmin = decode.get(2 * k).copied().unwrap_or(0.0);
                    let dmax = decode.get(2 * k + 1).copied().unwrap_or(1.0);
                    *comp = dmin + f64::from(raw) * (dmax - dmin) / max;
                }
                let c = to_rgb8(cs.to_rgb(&comps));
                rgb[px * 3..px * 3 + 3].copy_from_slice(&c);
            }
        }
    }

    // 5. Alpha : /SMask (image gris) ou /Mask (pochoir ou clé de couleur).
    let mut alpha = None;
    if let Some(sm) = get("SMask", "SMask") {
        if let Some(a) = decode_soft_mask(doc, &sm, width, height, &mut warnings) {
            alpha = Some(a);
        }
    } else if let Some(mask) = get("Mask", "Mask") {
        match &mask {
            Object::Array(ranges) => {
                // Clé de couleur (§8.9.6.4) : les pixels dont toutes les composantes
                // brutes sont dans les plages sont transparents.
                let r: Vec<i64> = ranges
                    .iter()
                    .map(|o| doc.resolve(o).ok().and_then(|v| v.as_i64()).unwrap_or(0))
                    .collect();
                if r.len() >= 2 * ncomp && !samples.is_empty() && bpc <= 16 {
                    let mut a = vec![255u8; n];
                    for y in 0..height as usize {
                        for x in 0..width as usize {
                            let mut masked = true;
                            for k in 0..ncomp {
                                let raw = i64::from(read_sample(
                                    &samples,
                                    y * row_bytes,
                                    x * ncomp + k,
                                    bpc,
                                ));
                                if raw < r[2 * k] || raw > r[2 * k + 1] {
                                    masked = false;
                                    break;
                                }
                            }
                            if masked {
                                a[y * width as usize + x] = 0;
                            }
                        }
                    }
                    alpha = Some(a);
                }
            }
            Object::Stream { .. } => {
                if let Some(a) = decode_stencil_mask(doc, &mask, width, height, &mut warnings) {
                    alpha = Some(a);
                }
            }
            _ => {}
        }
    }
    if alpha.is_none() {
        alpha = jpx_alpha;
    }

    Ok(DecodedImage {
        width,
        height,
        rgb,
        alpha,
        is_stencil: false,
        interpolate,
        warnings,
    })
}

fn to_rgb8(c: [f32; 3]) -> [u8; 3] {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        [
            (c[0].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
            (c[1].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
            (c[2].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
        ]
    }
}

/// Lit l'échantillon numéro `index` (en composantes) de la ligne commençant à
/// l'octet `row`, sur `bpc` bits. Hors limites → 0.
fn read_sample(data: &[u8], row: usize, index: usize, bpc: u32) -> u32 {
    match bpc {
        8 => u32::from(data.get(row + index).copied().unwrap_or(0)),
        16 => {
            let i = row + index * 2;
            (u32::from(data.get(i).copied().unwrap_or(0)) << 8)
                | u32::from(data.get(i + 1).copied().unwrap_or(0))
        }
        1 | 2 | 4 => {
            let bit = index * bpc as usize;
            let byte = data.get(row + bit / 8).copied().unwrap_or(0);
            let shift = 8 - bpc as usize - (bit % 8);
            u32::from(byte >> shift) & ((1 << bpc) - 1)
        }
        _ => 0,
    }
}

/// Adapte une image décodée aux dimensions déclarées (plus proche voisin) si
/// elles diffèrent ; sinon retourne les données telles quelles.
fn resample_to(data: &[u8], sw: u32, sh: u32, ncomp: usize, dw: u32, dh: u32) -> Vec<u8> {
    if sw == dw && sh == dh {
        return data.to_vec();
    }
    let mut out = vec![0u8; dw as usize * dh as usize * ncomp];
    if sw == 0 || sh == 0 {
        return out;
    }
    for y in 0..dh as usize {
        let sy = (y as u64 * u64::from(sh) / u64::from(dh)) as usize;
        for x in 0..dw as usize {
            let sx = (x as u64 * u64::from(sw) / u64::from(dw)) as usize;
            let s = (sy * sw as usize + sx) * ncomp;
            let d = (y * dw as usize + x) * ncomp;
            for k in 0..ncomp {
                out[d + k] = data.get(s + k).copied().unwrap_or(0);
            }
        }
    }
    out
}

/// Espace de couleur déduit du conteneur JP2 (boîte `colr`) ou, à défaut,
/// du nombre de canaux.
fn jpx_colorspace(cs: &JpxColorSpace, channels: usize) -> ColorSpace {
    let by_count = |n: usize| match n {
        1 => ColorSpace::DeviceGray,
        4 => ColorSpace::DeviceCmyk,
        _ => ColorSpace::DeviceRgb,
    };
    match cs {
        JpxColorSpace::Gray => ColorSpace::DeviceGray,
        JpxColorSpace::Srgb | JpxColorSpace::Sycc => ColorSpace::DeviceRgb,
        JpxColorSpace::Cmyk => ColorSpace::DeviceCmyk,
        JpxColorSpace::Icc(bytes) => {
            let alt = by_count(channels);
            let profile = IccProfile::parse(bytes)
                .ok()
                .filter(|p| p.components() == channels && !p.is_srgb_like())
                .map(|p| Rc::new(IccCache::new(p)));
            ColorSpace::IccBased {
                n: channels,
                alt: Box::new(alt),
                profile,
            }
        }
        JpxColorSpace::Unknown => by_count(channels),
    }
}

/// Sépare le dernier canal (opacité) des canaux de couleur d'un tampon
/// entrelacé à `total` canaux par pixel.
fn split_alpha(data: &[u8], total: usize, has_alpha: bool) -> (Vec<u8>, Option<Vec<u8>>) {
    if !has_alpha || total < 2 {
        return (data.to_vec(), None);
    }
    let colour = total - 1;
    let mut rgb = Vec::with_capacity(data.len() / total * colour);
    let mut alpha = Vec::with_capacity(data.len() / total);
    for px in data.chunks_exact(total) {
        rgb.extend_from_slice(&px[..colour]);
        alpha.push(px[colour]);
    }
    (rgb, Some(alpha))
}

/// Recadre des lignes 1 bit de `columns` pixels en lignes de `width` pixels
/// sur `height` lignes : tronque ou complète chaque ligne avec `fill`, et
/// ajoute des lignes pleines de `fill` si le flux en fournit moins.
fn repack_rows(bits: &[u8], columns: u32, width: u32, height: u32, fill: u8) -> Vec<u8> {
    let src_row = (columns as usize).div_ceil(8);
    let dst_row = (width as usize).div_ceil(8);
    if src_row == dst_row {
        let mut out = bits.to_vec();
        out.resize(dst_row * height as usize, fill);
        return out;
    }
    let mut out = vec![fill; dst_row * height as usize];
    for (y, dst) in out.chunks_exact_mut(dst_row).enumerate() {
        let Some(src) = bits.get(y * src_row..) else {
            break;
        };
        let n = src_row.min(dst_row).min(src.len());
        dst[..n].copy_from_slice(&src[..n]);
    }
    out
}

fn gray_placeholder(
    width: u32,
    height: u32,
    interpolate: bool,
    warnings: Vec<String>,
) -> DecodedImage {
    let n = width as usize * height as usize;
    DecodedImage {
        width,
        height,
        rgb: vec![128u8; n * 3],
        alpha: None,
        is_stencil: false,
        interpolate,
        warnings,
    }
}

/// Pochoir passé par un filtre d'image : CCITT et JBIG2 rendent des bits
/// 1 bpp (0 = noir = peint par défaut) ; DCT et JPX n'ont pas de sens pour
/// un pochoir et donnent « rien de peint ».
fn expand_image_filter(
    filter: Option<(&[u8], &acrux_codecs::DecodeParms)>,
    data: &[u8],
    width: u32,
    height: u32,
    _bpc: u32,
    warnings: &mut Vec<String>,
    stencil: bool,
) -> Vec<u8> {
    let row = (width as usize).div_ceil(8);
    let blank = || vec![if stencil { 0xFF } else { 0 }; row * height as usize];
    let Some((name, parms)) = filter else {
        return blank();
    };
    match name {
        b"CCITTFaxDecode" | b"CCF" => {
            let fax = parms.ccitt();
            match acrux_codecs::ccitt::decode(data, &fax) {
                Ok(bits) => {
                    let white = if fax.black_is_1 { 0x00 } else { 0xFF };
                    repack_rows(&bits, fax.columns, width, height, white)
                }
                Err(e) => {
                    warnings.push(format!("CCITT (masque) illisible : {e}"));
                    blank()
                }
            }
        }
        b"JBIG2Decode" => {
            match acrux_codecs::jbig2::decode(data, parms.jbig2_globals.as_deref(), width, height) {
                Ok(bits) => bits,
                Err(e) => {
                    warnings.push(format!("JBIG2 (masque) illisible : {e}"));
                    blank()
                }
            }
        }
        other => {
            warnings.push(format!(
                "filtre {} sans objet pour un masque",
                String::from_utf8_lossy(other)
            ));
            blank()
        }
    }
}

/// `/SMask` : image en niveaux de gris dont la valeur est l'alpha (§11.6.5.3).
fn decode_soft_mask(
    doc: &Document,
    sm: &Object,
    width: u32,
    height: u32,
    warnings: &mut Vec<String>,
) -> Option<Vec<u8>> {
    let Object::Stream { dict, .. } = sm else {
        return None;
    };
    let decoded = doc.stream_data(sm).ok()?;
    let filter = decoded
        .image_filter
        .as_ref()
        .map(|(n, p)| (n.as_slice(), p));
    let img = match decode_image(doc, dict, &decoded.data, filter, None) {
        Ok(i) => i,
        Err(e) => {
            warnings.push(format!("SMask illisible : {e}"));
            return None;
        }
    };
    warnings.extend(img.warnings.iter().cloned());
    // Luminosité = composante rouge (image gris).
    let src: Vec<u8> = img.rgb.chunks(3).map(|c| c[0]).collect();
    Some(resample_to(&src, img.width, img.height, 1, width, height))
}

/// `/Mask` pochoir : 1 = masqué (transparent), 0 = visible (§8.9.6.4), sauf
/// `/Decode [1 0]`.
fn decode_stencil_mask(
    doc: &Document,
    mask: &Object,
    width: u32,
    height: u32,
    warnings: &mut Vec<String>,
) -> Option<Vec<u8>> {
    let Object::Stream { dict, .. } = mask else {
        return None;
    };
    let decoded = doc.stream_data(mask).ok()?;
    let filter = decoded
        .image_filter
        .as_ref()
        .map(|(n, p)| (n.as_slice(), p));
    let mut d = dict.clone();
    d.insert(Name::new("ImageMask"), Object::Bool(true));
    let img = match decode_image(doc, &d, &decoded.data, filter, None) {
        Ok(i) => i,
        Err(e) => {
            warnings.push(format!("Mask illisible : {e}"));
            return None;
        }
    };
    // Pour un pochoir « peint » = bit 0 = zone masquée ici : on inverse.
    let src: Vec<u8> = img.alpha?.iter().map(|a| 255 - a).collect();
    Some(resample_to(&src, img.width, img.height, 1, width, height))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use acrux_document::Parser;

    fn doc() -> Document {
        Document::from_bytes(b"%PDF-1.7\n1 0 obj << /Type /Catalog >> endobj\n".to_vec()).unwrap()
    }

    fn dict(src: &str) -> Dict {
        Parser::new(src.as_bytes())
            .parse_object()
            .unwrap()
            .as_dict()
            .unwrap()
            .clone()
    }

    #[test]
    fn gray_8_bits_and_decode_inversion() {
        let d = dict("<< /Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 >>");
        let img = decode_image(&doc(), &d, &[0, 255], None, None).unwrap();
        assert_eq!(img.rgb, vec![0, 0, 0, 255, 255, 255]);
        let d = dict(
            "<< /Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Decode [1 0] >>",
        );
        let img = decode_image(&doc(), &d, &[0, 255], None, None).unwrap();
        assert_eq!(img.rgb, vec![255, 255, 255, 0, 0, 0]);
    }

    #[test]
    fn one_bit_rows_are_byte_aligned() {
        // 3 pixels de large sur 2 lignes : chaque ligne tient sur un octet.
        let d = dict("<< /W 3 /H 2 /CS /G /BPC 1 >>");
        let img = decode_image(&doc(), &d, &[0b1010_0000, 0b0100_0000], None, None).unwrap();
        let px: Vec<u8> = img.rgb.chunks(3).map(|c| c[0]).collect();
        assert_eq!(px, vec![255, 0, 255, 0, 255, 0]);
    }

    #[test]
    fn rgb_16_bits_and_indexed_4_bits() {
        let d = dict("<< /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 16 >>");
        let img = decode_image(
            &doc(),
            &d,
            &[0xFF, 0xFF, 0x80, 0x00, 0x00, 0x00],
            None,
            None,
        )
        .unwrap();
        assert_eq!(img.rgb, vec![255, 128, 0]);
        let d = dict("<< /Width 2 /Height 1 /ColorSpace [/Indexed /DeviceRGB 1 <ff000000ff00>] /BitsPerComponent 4 >>");
        let img = decode_image(&doc(), &d, &[0x01], None, None).unwrap();
        assert_eq!(img.rgb, vec![255, 0, 0, 0, 255, 0]);
    }

    #[test]
    fn stencil_mask_and_decode() {
        let d = dict("<< /Width 8 /Height 1 /ImageMask true >>");
        let img = decode_image(&doc(), &d, &[0b1111_0000], None, None).unwrap();
        assert!(img.is_stencil);
        assert_eq!(img.alpha.unwrap(), vec![0, 0, 0, 0, 255, 255, 255, 255]);
        let d = dict("<< /Width 8 /Height 1 /ImageMask true /Decode [1 0] >>");
        let img = decode_image(&doc(), &d, &[0b1111_0000], None, None).unwrap();
        assert_eq!(img.alpha.unwrap(), vec![255, 255, 255, 255, 0, 0, 0, 0]);
    }

    #[test]
    fn color_key_mask() {
        let d = dict(
            "<< /Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Mask [250 255] >>",
        );
        let img = decode_image(&doc(), &d, &[10, 255], None, None).unwrap();
        assert_eq!(img.alpha.unwrap(), vec![255, 0]);
    }

    #[test]
    fn truncated_data_and_unsupported_filter() {
        let d = dict("<< /Width 4 /Height 4 /ColorSpace /DeviceRGB /BitsPerComponent 8 >>");
        let img = decode_image(&doc(), &d, &[255; 6], None, None).unwrap();
        assert_eq!(img.rgb.len(), 48);
        assert!(!img.warnings.is_empty());
        let parms = acrux_codecs::DecodeParms::default();
        let img = decode_image(&doc(), &d, &[], Some((b"JPXDecode", &parms)), None).unwrap();
        assert_eq!(img.rgb[0], 128);
        assert!(decode_image(&doc(), &dict("<< /Width 0 /Height 5 >>"), &[], None, None).is_err());
    }
}
