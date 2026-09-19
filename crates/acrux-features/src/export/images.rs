//! Extraction des images d'un document (ISO 32000-2 §8.9).
//!
//! Le flux de contenu de chaque page est parcouru avec le même modèle d'état
//! graphique que le rendu, mais on ne retient que les opérateurs qui placent
//! une image : `q` / `Q` / `cm` pour la matrice courante, `Do` pour les
//! XObjects (image, ou formulaire dans lequel on descend), `BI … ID … EI`
//! pour les images en ligne (§8.9.7).
//!
//! Chaque image est rendue **dans son format d'origine quand c'est possible** :
//! un `/DCTDecode` ressort octet pour octet en JPEG (aucune recompression,
//! donc aucune perte ajoutée) ; tout le reste est décodé par
//! `acrux_render::image` puis réencodé en PNG, avec un canal alpha si l'image
//! porte un `/SMask`.

use std::collections::{BTreeMap, BTreeSet};

use acrux_core::{Matrix, Result};
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef, Page};
use acrux_render::image::decode_image;

/// Profondeur maximale d'imbrication des XObjects de formulaire.
const MAX_DEPTH: u32 = 12;
/// Nombre maximal d'opérations lues par page (garde-fou anti-hostilité).
const MAX_OPS: usize = 400_000;
/// Nombre maximal d'images retenues par page.
const MAX_IMAGES: usize = 4_096;

/// Format de sortie d'une image extraite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// PNG (ISO/IEC 15948), avec alpha si l'image a un `/SMask`.
    Png,
    /// JPEG (ITU-T T.81) : les octets `/DCTDecode` d'origine, inchangés.
    Jpeg,
}

impl ImageFormat {
    /// Extension de fichier usuelle, sans le point.
    #[must_use]
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpeg",
        }
    }

    /// Type de média IANA, pour les URI `data:` et les `[Content_Types].xml`.
    #[must_use]
    pub fn media_type(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
        }
    }
}

/// Une image incorporée au document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedImage {
    /// Indice de la page où l'image est dessinée (0 = première).
    pub page: usize,
    /// Nom de la ressource (`/Im0`) ou `inline-N` pour une image en ligne.
    pub name: String,
    /// Largeur en pixels.
    pub width: u32,
    /// Hauteur en pixels.
    pub height: u32,
    /// Espace colorimétrique déclaré dans le PDF (`DeviceRGB`, `ICCBased(3)`…).
    pub colorspace: String,
    /// Bits par composante déclarés (`/BitsPerComponent`).
    pub bits: u8,
    /// Format des octets de `data`.
    pub format: ImageFormat,
    /// Fichier image complet, prêt à écrire sur disque.
    pub data: Vec<u8>,
}

/// Image extraite avec sa place sur la page.
pub(crate) struct PlacedImage {
    /// Matrice du carré unité de l'espace image vers l'espace utilisateur
    /// PDF (§8.9.4 : l'image occupe toujours le carré unité).
    pub ctm: Matrix,
    /// L'image elle-même.
    pub image: ExtractedImage,
    /// Numéro de l'objet PDF, pour reconnaître deux placements de la même
    /// ressource ; `None` pour une image en ligne.
    pub key: Option<u32>,
}

/// Toutes les images dessinées par le document, page par page.
///
/// Une même ressource dessinée plusieurs fois sur une page n'est rendue
/// qu'une fois (un seul fichier image à écrire).
///
/// # Errors
/// Arbre des pages illisible.
pub fn extract_images(doc: &Document) -> Result<Vec<ExtractedImage>> {
    let pages = collect_pages(doc)?;
    let mut out = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        let mut seen: BTreeSet<u32> = BTreeSet::new();
        for placed in placed_images(doc, page, i) {
            if placed.key.is_some_and(|k| !seen.insert(k)) {
                continue;
            }
            out.push(placed.image);
        }
    }
    Ok(out)
}

/// Images dessinées par une page, avec leur matrice de placement.
pub(crate) fn placed_images(doc: &Document, page: &Page, page_index: usize) -> Vec<PlacedImage> {
    let resources = doc
        .dict_get(&page.dict, "Resources")
        .ok()
        .flatten()
        .and_then(|r| r.as_dict().cloned())
        .unwrap_or_default();
    let content = acrux_render::page::page_content(doc, page);
    let mut walker = Walker {
        doc,
        page: page_index,
        out: Vec::new(),
        cache: BTreeMap::new(),
        inline_count: 0,
        ops: 0,
    };
    walker.run(&content, &resources, Matrix::IDENTITY, 0);
    walker.out
}

/// Parcours d'un flux de contenu limité aux opérateurs de placement d'image.
struct Walker<'a> {
    doc: &'a Document,
    page: usize,
    out: Vec<PlacedImage>,
    /// Images déjà construites pour cette page, par numéro d'objet : une
    /// ressource dessinée plusieurs fois n'est décodée qu'une fois.
    cache: BTreeMap<u32, ExtractedImage>,
    inline_count: usize,
    ops: usize,
}

impl Walker<'_> {
    fn run(&mut self, content: &[u8], resources: &Dict, ctm: Matrix, depth: u32) {
        if depth > MAX_DEPTH || self.out.len() >= MAX_IMAGES {
            return;
        }
        let Ok(operations) = acrux_render::parse_content(content) else {
            return;
        };
        let mut stack: Vec<Matrix> = Vec::new();
        let mut ctm = ctm;
        for op in operations {
            self.ops += 1;
            if self.ops > MAX_OPS || self.out.len() >= MAX_IMAGES {
                return;
            }
            match op.operator.as_slice() {
                b"q" => stack.push(ctm),
                b"Q" => {
                    if let Some(m) = stack.pop() {
                        ctm = m;
                    }
                }
                b"cm" => {
                    let v: Vec<f64> = op.operands.iter().filter_map(Object::as_f64).collect();
                    if v.len() >= 6 {
                        ctm = Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5]).then(&ctm);
                    }
                }
                b"Do" => {
                    if let Some(name) = op.operands.first().and_then(Object::as_name) {
                        self.do_xobject(name, resources, ctm, depth);
                    }
                }
                b"BI" | b"EI" | b"ID" => {
                    if let Some(img) = &op.inline_image {
                        self.inline(img, resources, ctm);
                    }
                }
                _ => {}
            }
        }
    }

    /// `Do` : image (retenue) ou formulaire (parcouru à son tour).
    fn do_xobject(&mut self, name: &Name, resources: &Dict, ctm: Matrix, depth: u32) {
        let Some(entry) = self
            .doc
            .dict_get(resources, "XObject")
            .ok()
            .flatten()
            .and_then(|x| x.as_dict().and_then(|d| d.get(name).cloned()))
        else {
            return;
        };
        let xref = match &entry {
            Object::Reference(r) => Some(*r),
            _ => None,
        };
        let Ok(resolved) = self.doc.resolve(&entry) else {
            return;
        };
        let object = (*resolved).clone();
        let Some(dict) = object.as_dict().cloned() else {
            return;
        };
        let subtype = dict
            .get(&Name::new("Subtype"))
            .and_then(Object::as_name)
            .map(Name::as_str);
        match subtype.as_deref() {
            Some("Image") => {
                self.image_xobject(&name.as_str(), &object, &dict, xref, ctm, resources);
            }
            Some("Form") => {
                let matrix: Vec<f64> = self
                    .doc
                    .dict_get(&dict, "Matrix")
                    .ok()
                    .flatten()
                    .and_then(|m| {
                        m.as_array().map(|a| {
                            a.iter()
                                .filter_map(|o| self.doc.resolve(o).ok().and_then(|r| r.as_f64()))
                                .collect()
                        })
                    })
                    .unwrap_or_default();
                let inner = if matrix.len() == 6 {
                    Matrix::new(
                        matrix[0], matrix[1], matrix[2], matrix[3], matrix[4], matrix[5],
                    )
                    .then(&ctm)
                } else {
                    ctm
                };
                let sub = self
                    .doc
                    .dict_get(&dict, "Resources")
                    .ok()
                    .flatten()
                    .and_then(|r| r.as_dict().cloned())
                    .unwrap_or_else(|| resources.clone());
                let Ok(data) = self.doc.stream_data(&object) else {
                    return;
                };
                self.run(&data.data, &sub, inner, depth + 1);
            }
            _ => {}
        }
    }

    // Les paramètres d'une image XObject viennent d'endroits différents
    // (résolution, dictionnaire, référence, état graphique) : les regrouper
    // dans une structure n'apporterait rien ici.
    #[allow(clippy::too_many_arguments)]
    fn image_xobject(
        &mut self,
        name: &str,
        object: &Object,
        dict: &Dict,
        xref: Option<ObjectRef>,
        ctm: Matrix,
        resources: &Dict,
    ) {
        let key = xref.map(|r| r.number);
        if let Some(cached) = key.and_then(|k| self.cache.get(&k)) {
            self.out.push(PlacedImage {
                ctm,
                image: cached.clone(),
                key,
            });
            return;
        }
        let Ok(decoded) = self.doc.stream_data(object) else {
            return;
        };
        if let Some(image) = self.build(
            name.to_string(),
            dict,
            &decoded.data,
            decoded.image_filter.as_ref(),
            resources,
        ) {
            if let Some(k) = key {
                self.cache.insert(k, image.clone());
            }
            self.out.push(PlacedImage { ctm, image, key });
        }
    }

    fn inline(&mut self, img: &acrux_render::InlineImage, resources: &Dict, ctm: Matrix) {
        let resolve = |o: &Object| -> Object { o.clone() };
        let Ok(decoded) = acrux_document::filters::decode_stream(&img.dict, &img.data, &resolve)
        else {
            return;
        };
        self.inline_count += 1;
        let name = format!("inline-{}", self.inline_count);
        if let Some(image) = self.build(
            name,
            &img.dict,
            &decoded.data,
            decoded.image_filter.as_ref(),
            resources,
        ) {
            self.out.push(PlacedImage {
                ctm,
                image,
                key: None,
            });
        }
    }

    /// Construit l'image extraite : JPEG tel quel, PNG sinon.
    fn build(
        &self,
        name: String,
        dict: &Dict,
        data: &[u8],
        image_filter: Option<&(Vec<u8>, acrux_codecs::DecodeParms)>,
        resources: &Dict,
    ) -> Option<ExtractedImage> {
        let colorspace = colorspace_label(self.doc, dict);
        let bits = u8::try_from(
            entry(self.doc, dict, "BitsPerComponent", "BPC")
                .and_then(|o| o.as_i64())
                .unwrap_or(8),
        )
        .unwrap_or(8);
        // 1. JPEG : les octets d'origine, sans recompression ni perte ajoutée.
        //    Un JPEG CMYK « Adobe » ou masqué garde son format : c'est ce que
        //    fait Acrobat quand il exporte les images d'un PDF.
        if matches!(image_filter, Some((name, _)) if name.as_slice() == b"DCTDecode" || name.as_slice() == b"DCT")
        {
            let (width, height) = acrux_codecs::dct::read_header(data)
                .map_or_else(|_| declared_size(self.doc, dict), |(w, h, _)| (w, h));
            return Some(ExtractedImage {
                page: self.page,
                name,
                width,
                height,
                colorspace,
                bits,
                format: ImageFormat::Jpeg,
                data: data.to_vec(),
            });
        }
        // 2. Tout le reste passe par le décodeur du moteur, puis PNG.
        let filter = image_filter.map(|(n, p)| (n.as_slice(), p));
        let img = decode_image(self.doc, dict, data, filter, Some(resources)).ok()?;
        let n = (img.width as usize).checked_mul(img.height as usize)?;
        let data = match (&img.alpha, img.is_stencil) {
            // Pochoir : noir là où il peint, transparent ailleurs.
            (Some(alpha), true) => {
                let mut rgba = Vec::with_capacity(n * 4);
                for &a in alpha.iter().take(n) {
                    rgba.extend_from_slice(&[0, 0, 0, a]);
                }
                rgba.resize(n * 4, 0);
                png(
                    img.width,
                    img.height,
                    acrux_graphics::PixelLayout::Rgba,
                    &rgba,
                )
            }
            (Some(alpha), false) => {
                let mut rgba = Vec::with_capacity(n * 4);
                for i in 0..n {
                    let px = img.rgb.get(i * 3..i * 3 + 3).unwrap_or(&[0, 0, 0]);
                    rgba.extend_from_slice(px);
                    rgba.push(alpha.get(i).copied().unwrap_or(255));
                }
                png(
                    img.width,
                    img.height,
                    acrux_graphics::PixelLayout::Rgba,
                    &rgba,
                )
            }
            (None, _) => png(
                img.width,
                img.height,
                acrux_graphics::PixelLayout::Rgb,
                &img.rgb,
            ),
        };
        Some(ExtractedImage {
            page: self.page,
            name,
            width: img.width,
            height: img.height,
            colorspace,
            bits,
            format: ImageFormat::Png,
            data,
        })
    }
}

/// Encode une image extraite en PNG compressé.
fn png(width: u32, height: u32, layout: acrux_graphics::PixelLayout, pixels: &[u8]) -> Vec<u8> {
    acrux_graphics::encode_png_with(width, height, layout, pixels, &super::deflate_png)
}

/// Valeur d'une clé d'image, nom long ou abrégé (§8.9.7, tableau 92).
fn entry(doc: &Document, dict: &Dict, long: &str, short: &str) -> Option<Object> {
    let o = dict
        .get(&Name::new(long))
        .or_else(|| dict.get(&Name::new(short)))?;
    doc.resolve(o).ok().map(|r| (*r).clone())
}

/// Dimensions déclarées dans le dictionnaire d'image.
fn declared_size(doc: &Document, dict: &Dict) -> (u32, u32) {
    let get = |long, short| {
        entry(doc, dict, long, short)
            .and_then(|o| o.as_i64())
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0)
    };
    (get("Width", "W"), get("Height", "H"))
}

/// Développe les abréviations d'espace colorimétrique des images en ligne.
fn expand_abbreviation(name: &str) -> String {
    match name {
        "G" => "DeviceGray",
        "RGB" => "DeviceRGB",
        "CMYK" => "DeviceCMYK",
        "I" => "Indexed",
        other => other,
    }
    .to_string()
}

/// Décrit l'espace colorimétrique déclaré, en une chaîne lisible.
fn colorspace_label(doc: &Document, dict: &Dict) -> String {
    if matches!(
        entry(doc, dict, "ImageMask", "IM"),
        Some(Object::Bool(true))
    ) {
        return "ImageMask".into();
    }
    let Some(obj) = entry(doc, dict, "ColorSpace", "CS") else {
        return "DeviceGray".into();
    };
    match &obj {
        Object::Name(n) => expand_abbreviation(&n.as_str()),
        Object::Array(a) => {
            let family = a
                .first()
                .and_then(Object::as_name)
                .map(|n| expand_abbreviation(&n.as_str()))
                .unwrap_or_default();
            match family.as_str() {
                "ICCBased" => {
                    let n = a
                        .get(1)
                        .and_then(|o| doc.resolve(o).ok())
                        .and_then(|s| {
                            s.as_dict()
                                .and_then(|d| d.get(&Name::new("N")).and_then(Object::as_i64))
                        })
                        .unwrap_or(0);
                    format!("ICCBased({n})")
                }
                "Indexed" => {
                    let base = a
                        .get(1)
                        .and_then(Object::as_name)
                        .map_or_else(|| "…".into(), |n| expand_abbreviation(&n.as_str()));
                    format!("Indexed({base})")
                }
                "DeviceN" => {
                    let n = a
                        .get(1)
                        .and_then(Object::as_array)
                        .map_or(0, <[Object]>::len);
                    format!("DeviceN({n})")
                }
                "" => "?".into(),
                other => other.to_string(),
            }
        }
        _ => "?".into(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    /// PDF d'une page avec un XObject image 2 × 2 en RVB non compressé.
    fn pdf_with_image(extra_image_keys: &str, content: &str) -> Document {
        let pixels = "\x7f\x01\x01\x01\x7f\x01\x01\x01\x7f\x7f\x7f\x01";
        let src = format!(
            "%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
             2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
             3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
             /Resources << /XObject << /Im0 5 0 R >> >> >> endobj\n\
             4 0 obj << /Length {} >>\nstream\n{content}\nendstream\nendobj\n\
             5 0 obj << /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB \
             /BitsPerComponent 8 {extra_image_keys} /Length 12 >>\nstream\n{pixels}\nendstream\nendobj\n",
            content.len()
        );
        Document::from_bytes(src.into_bytes()).unwrap()
    }

    #[test]
    fn image_xobject_is_extracted_as_png_with_its_placement() {
        let doc = pdf_with_image("", "q 100 0 0 50 20 30 cm /Im0 Do Q");
        let images = extract_images(&doc).unwrap();
        assert_eq!(images.len(), 1);
        let img = &images[0];
        assert_eq!((img.width, img.height, img.bits), (2, 2, 8));
        assert_eq!(img.colorspace, "DeviceRGB");
        assert_eq!(img.format, ImageFormat::Png);
        assert_eq!(&img.data[..8], &acrux_graphics::png::PNG_SIGNATURE);
        let pages = collect_pages(&doc).unwrap();
        let placed = placed_images(&doc, &pages[0], 0);
        let m = placed[0].ctm;
        assert!((m.a - 100.0).abs() < 1e-9 && (m.d - 50.0).abs() < 1e-9);
        assert!((m.e - 20.0).abs() < 1e-9 && (m.f - 30.0).abs() < 1e-9);
    }

    #[test]
    fn repeated_resource_is_extracted_once() {
        let doc = pdf_with_image(
            "",
            "q 10 0 0 10 0 0 cm /Im0 Do Q q 20 0 0 20 50 50 cm /Im0 Do Q",
        );
        assert_eq!(extract_images(&doc).unwrap().len(), 1);
    }

    #[test]
    fn smask_becomes_a_png_alpha_channel() {
        // Un /SMask 2 × 2 en gris : deux pixels opaques, deux transparents.
        let src = "%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
             2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
             3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
             /Resources << /XObject << /Im0 5 0 R >> >> >> endobj\n\
             4 0 obj << /Length 31 >>\nstream\nq 10 0 0 10 0 0 cm /Im0 Do Q\nendstream\nendobj\n\
             5 0 obj << /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB \
             /BitsPerComponent 8 /SMask 6 0 R /Length 12 >>\nstream\n\x7f\x01\x01\x01\x7f\x01\x01\x01\x7f\x7f\x7f\x01\nendstream\nendobj\n\
             6 0 obj << /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray \
             /BitsPerComponent 8 /Length 4 >>\nstream\n\x7f\x7f\x00\x00\nendstream\nendobj\n";
        let doc = Document::from_bytes(src.as_bytes().to_vec()).unwrap();
        let images = extract_images(&doc).unwrap();
        assert_eq!(images.len(), 1);
        // Type de couleur 6 (RGBA) dans l'IHDR : octet 25 du fichier.
        assert_eq!(images[0].data[25], 6, "PNG RGBA");
    }

    #[test]
    fn form_xobjects_are_walked_and_matrices_composed() {
        let src = "%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
             2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
             3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
             /Resources << /XObject << /Fm0 7 0 R >> >> >> endobj\n\
             4 0 obj << /Length 27 >>\nstream\nq 1 0 0 1 10 20 cm /Fm0 Do Q\nendstream\nendobj\n\
             5 0 obj << /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB \
             /BitsPerComponent 8 /Length 12 >>\nstream\n\x7f\x01\x01\x01\x7f\x01\x01\x01\x7f\x7f\x7f\x01\nendstream\nendobj\n\
             7 0 obj << /Type /XObject /Subtype /Form /BBox [0 0 100 100] /Matrix [2 0 0 2 0 0] \
             /Resources << /XObject << /Im0 5 0 R >> >> /Length 24 >>\nstream\n5 0 0 5 1 2 cm /Im0 Do\nendstream\nendobj\n";
        let doc = Document::from_bytes(src.as_bytes().to_vec()).unwrap();
        let pages = collect_pages(&doc).unwrap();
        let placed = placed_images(&doc, &pages[0], 0);
        assert_eq!(placed.len(), 1);
        let m = placed[0].ctm;
        // 5·2 = 10 d'échelle ; translation (1,2)·2 + (10,20) = (12, 24).
        assert!((m.a - 10.0).abs() < 1e-9, "{m:?}");
        assert!(
            (m.e - 12.0).abs() < 1e-9 && (m.f - 24.0).abs() < 1e-9,
            "{m:?}"
        );
    }

    #[test]
    fn inline_images_are_extracted() {
        let src = "%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
             2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
             3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
             /Resources << >> >> endobj\n\
             4 0 obj << /Length 60 >>\nstream\nq 20 0 0 20 5 5 cm BI /W 2 /H 2 /CS /G /BPC 8 ID \x00\x40\x60\x7f EI Q\nendstream\nendobj\n";
        let doc = Document::from_bytes(src.as_bytes().to_vec()).unwrap();
        let images = extract_images(&doc).unwrap();
        assert_eq!(images.len(), 1, "{images:?}");
        assert_eq!(images[0].name, "inline-1");
        assert_eq!(images[0].colorspace, "DeviceGray");
        assert_eq!((images[0].width, images[0].height), (2, 2));
    }

    #[test]
    fn colorspace_labels_cover_arrays() {
        let doc = pdf_with_image("", "q 1 0 0 1 0 0 cm /Im0 Do Q");
        let mut dict = Dict::new();
        dict.insert(
            Name::new("ColorSpace"),
            Object::Array(vec![
                Object::Name(Name::new("Indexed")),
                Object::Name(Name::new("DeviceRGB")),
                Object::Integer(255),
                Object::String(vec![0; 3]),
            ]),
        );
        assert_eq!(colorspace_label(&doc, &dict), "Indexed(DeviceRGB)");
        let mut dict = Dict::new();
        dict.insert(Name::new("IM"), Object::Bool(true));
        assert_eq!(colorspace_label(&doc, &dict), "ImageMask");
        assert_eq!(colorspace_label(&doc, &Dict::new()), "DeviceGray");
    }
}
