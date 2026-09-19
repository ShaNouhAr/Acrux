//! Chaînage des filtres de flux (ISO 32000-2 §7.4).
//!
//! Un flux peut déclarer `/Filter` (nom ou tableau de noms) et
//! `/DecodeParms` (dictionnaire ou tableau de dictionnaires, un par filtre).
//! Les filtres s'appliquent dans l'ordre du tableau. Les codecs eux-mêmes
//! vivent dans `acrux-codecs` ; ce module ne fait que lire les paramètres.

use acrux_codecs::DecodeParms;
use acrux_core::Result;

use crate::objects::{Dict, Name, Object};

/// Résolveur d'objets indirects (les paramètres peuvent être des références).
pub type Resolver<'r> = &'r dyn Fn(&Object) -> Object;

/// Filtres d'image dont les données doivent rester encodées pour le décodeur
/// d'images (§7.4.1 : ils sont toujours en dernière position).
const IMAGE_FILTERS: [&[u8]; 6] = [
    b"DCTDecode",
    b"DCT",
    b"JPXDecode",
    b"JBIG2Decode",
    b"CCITTFaxDecode",
    b"CCF",
];

/// Liste ordonnée des filtres `(nom, paramètres)` déclarés par un flux.
#[must_use]
pub fn filter_chain(dict: &Dict, resolve: Resolver<'_>) -> Vec<(Vec<u8>, DecodeParms)> {
    let filters: Vec<Vec<u8>> = match dict.get(&Name::new("Filter")).map(resolve) {
        Some(Object::Name(n)) => vec![n.0],
        Some(Object::Array(a)) => a
            .iter()
            .filter_map(|o| resolve(o).as_name().map(|n| n.0.clone()))
            .collect(),
        _ => Vec::new(),
    };
    // `/DP` est l'abréviation autorisée dans les images inline (§8.9.7).
    let parms_obj = dict
        .get(&Name::new("DecodeParms"))
        .or_else(|| dict.get(&Name::new("DP")))
        .map(resolve);
    let parms: Vec<Option<Dict>> = match parms_obj {
        Some(Object::Dict(d)) => vec![Some(d)],
        Some(Object::Array(a)) => a
            .iter()
            .map(|o| match resolve(o) {
                Object::Dict(d) => Some(d),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    filters
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            let p = parms.get(i).cloned().flatten();
            (name, decode_parms(p.as_ref(), resolve))
        })
        .collect()
}

fn decode_parms(d: Option<&Dict>, resolve: Resolver<'_>) -> DecodeParms {
    let mut p = DecodeParms::default();
    let Some(d) = d else { return p };
    let int =
        |key: &str| -> Option<i64> { d.get(&Name::new(key)).and_then(|o| resolve(o).as_i64()) };
    if let Some(v) = int("Predictor") {
        p.predictor = u32::try_from(v.max(0)).unwrap_or(1);
    }
    if let Some(v) = int("Colors") {
        p.colors = u32::try_from(v.max(0)).unwrap_or(1);
    }
    if let Some(v) = int("BitsPerComponent") {
        p.bits_per_component = u32::try_from(v.max(0)).unwrap_or(8);
    }
    if let Some(v) = int("Columns") {
        p.columns = u32::try_from(v.max(0)).unwrap_or(1);
        p.columns_given = true;
    }
    if let Some(v) = int("EarlyChange") {
        p.early_change = v != 0;
    }
    // CCITTFaxDecode (tableau 11).
    if let Some(v) = int("K") {
        p.k = i32::try_from(v).unwrap_or(0);
    }
    if let Some(v) = int("Rows") {
        p.rows = u32::try_from(v.max(0)).unwrap_or(0);
    }
    let flag = |key: &str| -> Option<bool> {
        match d.get(&Name::new(key)).map(resolve) {
            Some(Object::Bool(b)) => Some(b),
            _ => None,
        }
    };
    if let Some(b) = flag("BlackIs1") {
        p.black_is_1 = b;
    }
    if let Some(b) = flag("EncodedByteAlign") {
        p.encoded_byte_align = b;
    }
    if let Some(b) = flag("EndOfLine") {
        p.end_of_line = b;
    }
    if let Some(b) = flag("EndOfBlock") {
        p.end_of_block = b;
    }
    // JBIG2Decode (tableau 12) : le flux global est lui-même filtré.
    if let Some(g) = d.get(&Name::new("JBIG2Globals")) {
        if let Object::Stream { dict, raw } = resolve(g) {
            if let Ok(decoded) = decode_stream(&dict, &raw, resolve) {
                p.jbig2_globals = Some(decoded.data);
            }
        }
    }
    p
}

/// Décode les données brutes d'un flux en appliquant tous ses filtres
/// standard. Les filtres d'image (DCT, JPX, JBIG2, CCITT) ne sont pas
/// appliqués : les données sont rendues telles quelles et le nom du filtre
/// restant est retourné avec elles.
///
/// # Errors
/// Filtre inconnu ou données irrécupérables.
pub fn decode_stream(dict: &Dict, raw: &[u8], resolve: Resolver<'_>) -> Result<Decoded> {
    let mut data = raw.to_vec();
    for (name, parms) in filter_chain(dict, resolve) {
        if IMAGE_FILTERS.contains(&name.as_slice()) {
            return Ok(Decoded {
                data,
                image_filter: Some((name, parms)),
            });
        }
        data = acrux_codecs::apply_filter(&name, &data, &parms)?;
    }
    Ok(Decoded {
        data,
        image_filter: None,
    })
}

/// Résultat du décodage d'un flux.
#[derive(Debug, Clone)]
pub struct Decoded {
    /// Données après application des filtres standard.
    pub data: Vec<u8>,
    /// Filtre d'image restant à appliquer, le cas échéant.
    pub image_filter: Option<(Vec<u8>, DecodeParms)>,
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    fn direct(o: &Object) -> Object {
        o.clone()
    }

    fn dict(src: &[u8]) -> Dict {
        match Parser::new(src).parse_object().unwrap() {
            Object::Dict(d) => d,
            _ => panic!("dict attendu"),
        }
    }

    #[test]
    fn chain_parsing() {
        let d = dict(b"<< /Filter [/ASCIIHexDecode /FlateDecode] /DecodeParms [null << /Predictor 12 /Columns 4 >>] >>");
        let chain = filter_chain(&d, &direct);
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].0, b"ASCIIHexDecode");
        assert_eq!(chain[0].1.predictor, 1);
        assert_eq!(chain[1].0, b"FlateDecode");
        assert_eq!(chain[1].1.predictor, 12);
        assert_eq!(chain[1].1.columns, 4);
        assert_eq!(chain[1].1.colors, 1);
        assert_eq!(chain[1].1.bits_per_component, 8);
    }

    #[test]
    fn single_filter_and_no_filter() {
        let d = dict(b"<< /Filter /Fl /DecodeParms << /EarlyChange 0 >> >>");
        let chain = filter_chain(&d, &direct);
        assert_eq!(chain.len(), 1);
        assert!(!chain[0].1.early_change);
        assert!(filter_chain(&dict(b"<< >>"), &direct).is_empty());
        assert_eq!(
            decode_stream(&dict(b"<< >>"), b"abc", &direct)
                .unwrap()
                .data,
            b"abc"
        );
    }

    #[test]
    fn image_filters_are_left_encoded() {
        let d = dict(b"<< /Filter [/ASCIIHexDecode /DCTDecode] >>");
        let out = decode_stream(&d, b"41 42>", &direct).unwrap();
        assert_eq!(out.data, b"AB");
        assert_eq!(out.image_filter.unwrap().0, b"DCTDecode");
    }

    #[test]
    fn unknown_filter_is_an_error() {
        let d = dict(b"<< /Filter /Nope >>");
        assert!(decode_stream(&d, b"x", &direct).is_err());
    }
}
