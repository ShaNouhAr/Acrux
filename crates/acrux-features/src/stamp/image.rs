//! Incorporation d'une image fournie en octets (PNG ou JPEG) comme XObject
//! image (ISO 32000-2 §8.9).
//!
//! Deux chemins :
//!
//! - **JPEG** : les octets sont incorporés **tels quels** avec
//!   `/Filter /DCTDecode` ; seuls l'en-tête `SOFn` (dimensions, nombre de
//!   composantes) et le marqueur Adobe `APP14` (transformation, inversion
//!   CMJN) sont lus. Aucune recompression, donc aucune perte.
//! - **PNG** : décodé ici (zlib + dé-filtrage par `acrux-codecs`), converti en
//!   échantillons 8 bits gris ou RVB, puis recompressé en `FlateDecode` ; le
//!   canal alpha éventuel devient un `/SMask`.
//!
//! Formats PNG acceptés : profondeurs 1, 2, 4, 8 et 16 bits, types de couleur
//! 0 (gris), 2 (RVB), 3 (palette), 4 (gris + alpha) et 6 (RVBA), sans
//! entrelacement. L'entrelacement Adam7 est refusé avec un message clair.

use acrux_core::{Error, Result};
use acrux_document::{Dict, Document, Name, Object, ObjectRef};

/// Niveau du compresseur DEFLATE pour les images PNG réincorporées.
const LEVEL: u8 = 6;

/// Image prête à être écrite dans le document.
#[derive(Debug, Clone)]
pub struct DecodedImage {
    /// Largeur en pixels.
    pub width: u32,
    /// Hauteur en pixels.
    pub height: u32,
    /// Dictionnaire et flux de l'XObject image.
    object: Object,
    /// Masque de transparence (`/SMask`), déjà sous forme d'objet flux.
    smask: Option<Object>,
}

impl DecodedImage {
    /// Écrit l'image (et son masque) dans le document et renvoie la référence
    /// de l'XObject image.
    pub(crate) fn write(self, doc: &Document) -> ObjectRef {
        let Object::Stream { mut dict, raw } = self.object else {
            return doc.add(self.object);
        };
        if let Some(smask) = self.smask {
            let r = doc.add(smask);
            dict.insert(Name::new("SMask"), Object::Reference(r));
        }
        doc.add(Object::Stream { dict, raw })
    }
}

/// Décode une image PNG ou JPEG fournie en octets.
///
/// # Errors
/// Format inconnu, en-tête illisible, entrelacement Adam7, ou dimensions
/// nulles.
pub fn decode(data: &[u8]) -> Result<DecodedImage> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return png(data);
    }
    if data.starts_with(&[0xFF, 0xD8]) {
        return jpeg(data);
    }
    Err(Error::Unsupported(
        "image : seuls les fichiers PNG et JPEG sont acceptés".into(),
    ))
}

/// Dictionnaire commun aux XObjects image.
fn image_dict(width: u32, height: u32, color_space: &str, bpc: i64) -> Dict {
    let mut d = Dict::new();
    d.insert(Name::new("Type"), Object::Name(Name::new("XObject")));
    d.insert(Name::new("Subtype"), Object::Name(Name::new("Image")));
    d.insert(Name::new("Width"), Object::Integer(i64::from(width)));
    d.insert(Name::new("Height"), Object::Integer(i64::from(height)));
    d.insert(
        Name::new("ColorSpace"),
        Object::Name(Name::new(color_space)),
    );
    d.insert(Name::new("BitsPerComponent"), Object::Integer(bpc));
    d
}

fn stream(mut dict: Dict, raw: Vec<u8>) -> Object {
    dict.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
    );
    Object::Stream { dict, raw }
}

// --- JPEG -------------------------------------------------------------------

/// JPEG incorporé tel quel : lecture du `SOFn` et de l'`APP14` Adobe.
fn jpeg(data: &[u8]) -> Result<DecodedImage> {
    let mut pos = 2;
    let mut geometry = None;
    let mut adobe_transform = None;
    while pos + 4 <= data.len() {
        if data[pos] != 0xFF {
            pos += 1;
            continue;
        }
        let marker = data[pos + 1];
        // Marqueurs sans charge utile.
        if marker == 0xFF {
            pos += 1;
            continue;
        }
        if matches!(marker, 0x01 | 0xD0..=0xD9) {
            pos += 2;
            continue;
        }
        let len = usize::from(u16::from_be_bytes([data[pos + 2], data[pos + 3]]));
        let body = data
            .get(pos + 4..pos + 2 + len)
            .ok_or_else(|| Error::Corrupt("JPEG tronqué".into()))?;
        match marker {
            // SOFn, sauf DHT (C4), JPG (C8) et DAC (CC).
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                if body.len() < 6 {
                    return Err(Error::Corrupt("JPEG : en-tête SOF trop court".into()));
                }
                let height = u32::from(u16::from_be_bytes([body[1], body[2]]));
                let width = u32::from(u16::from_be_bytes([body[3], body[4]]));
                geometry = Some((width, height, usize::from(body[5])));
            }
            // APP14 « Adobe » : l'octet de transformation est le dernier.
            0xEE => {
                if body.starts_with(b"Adobe") {
                    adobe_transform = body.last().copied();
                }
            }
            // Début du balayage : plus rien d'utile en clair après.
            0xDA => break,
            _ => {}
        }
        pos += 2 + len;
    }
    let (width, height, components) =
        geometry.ok_or_else(|| Error::Corrupt("JPEG sans marqueur SOF".into()))?;
    if width == 0 || height == 0 {
        return Err(Error::Corrupt("JPEG de dimensions nulles".into()));
    }
    let color_space = match components {
        1 => "DeviceGray",
        3 => "DeviceRGB",
        4 => "DeviceCMYK",
        n => {
            return Err(Error::Unsupported(format!(
                "JPEG à {n} composantes non pris en charge"
            )))
        }
    };
    let mut dict = image_dict(width, height, color_space, 8);
    dict.insert(Name::new("Filter"), Object::Name(Name::new("DCTDecode")));
    // Les JPEG CMJN écrits par Adobe stockent les composantes inversées
    // (§7.4.8, note) ; le marqueur APP14 le signale.
    if components == 4 && adobe_transform.is_some() {
        dict.insert(
            Name::new("Decode"),
            Object::Array(
                [1, 0, 1, 0, 1, 0, 1, 0]
                    .into_iter()
                    .map(Object::Integer)
                    .collect(),
            ),
        );
    }
    Ok(DecodedImage {
        width,
        height,
        object: stream(dict, data.to_vec()),
        smask: None,
    })
}

// --- PNG --------------------------------------------------------------------

/// En-tête `IHDR` d'un PNG.
struct Ihdr {
    width: u32,
    height: u32,
    depth: u8,
    color_type: u8,
}

impl Ihdr {
    /// Composantes par pixel selon le type de couleur (PNG §11.2.2).
    fn channels(&self) -> u32 {
        match self.color_type {
            2 => 3,
            4 => 2,
            6 => 4,
            _ => 1,
        }
    }
}

/// Image PNG décodée en pixels, prête à être analysée ou réécrite.
///
/// Les composantes valent 1 (niveaux de gris) ou 3 (RVB) ; la palette est
/// déjà développée. L'alpha est séparé, comme un `/SMask` PDF.
pub(crate) struct Raster {
    /// Largeur en pixels.
    pub width: u32,
    /// Hauteur en pixels.
    pub height: u32,
    /// Composantes par pixel : 1 ou 3.
    pub components: u8,
    /// Échantillons entrelacés, une ligne après l'autre, du haut vers le bas.
    pub data: Vec<u8>,
    /// Canal alpha, une valeur par pixel.
    pub alpha: Option<Vec<u8>>,
}

/// Décode un PNG en pixels.
///
/// # Errors
/// En-tête absent, entrelacement Adam7, profondeur ou type de couleur hors
/// norme, flux compressé illisible.
#[allow(clippy::too_many_lines)] // un bloc par type de couleur PNG, à la suite
pub(crate) fn png_raster(data: &[u8]) -> Result<Raster> {
    let mut pos = 8;
    let mut header: Option<Ihdr> = None;
    let mut idat = Vec::new();
    let mut palette = Vec::new();
    let mut transparency = Vec::new();
    while pos + 8 <= data.len() {
        let len = usize::try_from(u32::from_be_bytes([
            data[pos],
            data[pos + 1],
            data[pos + 2],
            data[pos + 3],
        ]))
        .map_err(|_| Error::Corrupt("PNG : bloc trop grand".into()))?;
        let kind = &data[pos + 4..pos + 8];
        let body = data
            .get(pos + 8..pos + 8 + len)
            .ok_or_else(|| Error::Corrupt("PNG tronqué".into()))?;
        match kind {
            b"IHDR" if body.len() >= 13 => {
                if body[12] != 0 {
                    return Err(Error::Unsupported(
                        "PNG entrelacé (Adam7) non pris en charge".into(),
                    ));
                }
                header = Some(Ihdr {
                    width: u32::from_be_bytes([body[0], body[1], body[2], body[3]]),
                    height: u32::from_be_bytes([body[4], body[5], body[6], body[7]]),
                    depth: body[8],
                    color_type: body[9],
                });
            }
            b"PLTE" => palette = body.to_vec(),
            b"tRNS" => transparency = body.to_vec(),
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        pos += 12 + len;
    }
    let h = header.ok_or_else(|| Error::Corrupt("PNG sans en-tête IHDR".into()))?;
    if h.width == 0 || h.height == 0 {
        return Err(Error::Corrupt("PNG de dimensions nulles".into()));
    }
    if !matches!(h.depth, 1 | 2 | 4 | 8 | 16) || !matches!(h.color_type, 0 | 2 | 3 | 4 | 6) {
        return Err(Error::Unsupported(format!(
            "PNG : profondeur {} et type de couleur {} non pris en charge",
            h.depth, h.color_type
        )));
    }
    let raw = acrux_codecs::flate::decode(&idat)?;
    let flat = acrux_codecs::predictor::apply(&raw, 15, h.channels(), u32::from(h.depth), h.width)?;
    let samples = expand(&flat, &h);
    let pixels = (h.width as usize) * (h.height as usize);
    // Séparation couleur / alpha, et développement de la palette.
    let (components, color, alpha) = match h.color_type {
        0 => (1, samples, None),
        2 => (3, samples, None),
        3 => {
            let mut rgb = Vec::with_capacity(pixels * 3);
            let mut a = Vec::with_capacity(pixels);
            for &index in samples.iter().take(pixels) {
                let base = usize::from(index) * 3;
                rgb.extend_from_slice(palette.get(base..base + 3).unwrap_or(&[0, 0, 0]));
                a.push(transparency.get(usize::from(index)).copied().unwrap_or(255));
            }
            let alpha = a.iter().any(|&v| v != 255).then_some(a);
            (3, rgb, alpha)
        }
        4 => {
            let mut gray = Vec::with_capacity(pixels);
            let mut a = Vec::with_capacity(pixels);
            for px in samples.chunks_exact(2) {
                gray.push(px[0]);
                a.push(px[1]);
            }
            (1, gray, Some(a))
        }
        _ => {
            let mut rgb = Vec::with_capacity(pixels * 3);
            let mut a = Vec::with_capacity(pixels);
            for px in samples.chunks_exact(4) {
                rgb.extend_from_slice(&px[..3]);
                a.push(px[3]);
            }
            (3, rgb, alpha_or_none(a))
        }
    };
    Ok(Raster {
        width: h.width,
        height: h.height,
        components,
        data: color,
        alpha,
    })
}

/// Un canal alpha entièrement opaque ne sert à rien : autant ne pas l'écrire.
fn alpha_or_none(a: Vec<u8>) -> Option<Vec<u8>> {
    a.iter().any(|&v| v != 255).then_some(a)
}

/// PNG réincorporé : les pixels décodés, recompressés en Flate.
fn png(data: &[u8]) -> Result<DecodedImage> {
    let raster = png_raster(data)?;
    let color_space = if raster.components == 1 {
        "DeviceGray"
    } else {
        "DeviceRGB"
    };
    let mut dict = image_dict(raster.width, raster.height, color_space, 8);
    dict.insert(Name::new("Filter"), Object::Name(Name::new("FlateDecode")));
    let object = stream(dict, acrux_codecs::flate::compress(&raster.data, LEVEL));
    let smask = raster.alpha.map(|a| {
        let mut d = image_dict(raster.width, raster.height, "DeviceGray", 8);
        d.insert(Name::new("Filter"), Object::Name(Name::new("FlateDecode")));
        stream(d, acrux_codecs::flate::compress(&a, LEVEL))
    });
    Ok(DecodedImage {
        width: raster.width,
        height: raster.height,
        object,
        smask,
    })
}

/// Ramène les échantillons à 8 bits par composante : les profondeurs 1, 2 et
/// 4 sont étendues sur toute la plage, la profondeur 16 ne garde que l'octet
/// de poids fort.
fn expand(flat: &[u8], h: &Ihdr) -> Vec<u8> {
    let channels = h.channels() as usize;
    let per_row = h.width as usize * channels;
    match h.depth {
        8 => flat.to_vec(),
        // Les composantes 16 bits sont en gros-boutiste : l'octet de poids
        // fort suffit pour une image de tampon.
        16 => flat.chunks_exact(2).map(|p| p[0]).collect(),
        depth => {
            // Les lignes sont alignées sur l'octet : on relit chaque ligne bit
            // à bit puis on passe à la ligne suivante.
            let bits = usize::from(depth);
            let row_bytes = (per_row * bits).div_ceil(8);
            let mut out = Vec::with_capacity(h.height as usize * per_row);
            let max = (1u8 << bits) - 1;
            for row in flat.chunks(row_bytes) {
                for i in 0..per_row {
                    let bit = i * bits;
                    let byte = row.get(bit / 8).copied().unwrap_or(0);
                    let shift = 8 - bits - (bit % 8);
                    let value = (byte >> shift) & max;
                    // La palette garde son indice brut, les niveaux de gris
                    // sont étalés sur 0..255.
                    out.push(if h.color_type == 3 {
                        value
                    } else {
                        u8::try_from(u16::from(value) * 255 / u16::from(max)).unwrap_or(0)
                    });
                }
            }
            out
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
pub(crate) mod tests {
    use super::*;

    /// PNG RVB(A) minimal écrit par l'encodeur d'`acrux-graphics`.
    pub(crate) fn sample_png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
        acrux_graphics::encode_png_rgb(width, height, rgb)
    }

    #[test]
    fn png_rgb_is_decoded_and_recompressed() {
        let rgb = vec![
            255, 0, 0, 0, 255, 0, // ligne 1
            0, 0, 255, 255, 255, 0, // ligne 2
        ];
        let png = sample_png(2, 2, &rgb);
        let image = decode(&png).unwrap();
        assert_eq!((image.width, image.height), (2, 2));
        assert!(image.smask.is_none());
        let Object::Stream { dict, raw } = &image.object else {
            panic!("flux attendu");
        };
        assert_eq!(
            dict.get(&Name::new("ColorSpace")),
            Some(&Object::Name(Name::new("DeviceRGB")))
        );
        assert_eq!(acrux_codecs::flate::decode(raw).unwrap(), rgb);
    }

    #[test]
    fn unknown_format_is_refused() {
        assert!(decode(b"GIF89a").is_err());
        assert!(decode(&[]).is_err());
    }

    #[test]
    fn jpeg_header_is_read_without_recompressing() {
        // JPEG minimal : SOI, SOF0 gris 4x3, SOS, EOI.
        let mut data = vec![0xFF, 0xD8];
        data.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        data.extend_from_slice(&3u16.to_be_bytes()); // hauteur
        data.extend_from_slice(&4u16.to_be_bytes()); // largeur
        data.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);
        data.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9]);
        let image = decode(&data).unwrap();
        assert_eq!((image.width, image.height), (4, 3));
        let Object::Stream { dict, raw } = &image.object else {
            panic!("flux attendu");
        };
        assert_eq!(raw, &data, "les octets JPEG doivent être intacts");
        assert_eq!(
            dict.get(&Name::new("Filter")),
            Some(&Object::Name(Name::new("DCTDecode")))
        );
        assert_eq!(
            dict.get(&Name::new("ColorSpace")),
            Some(&Object::Name(Name::new("DeviceGray")))
        );
    }
}
