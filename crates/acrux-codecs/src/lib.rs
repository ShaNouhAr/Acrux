//! # acrux-codecs
//!
//! Décodeurs et encodeurs écrits de zéro, un sous-module par codec, chacun
//! avec ses propres tests et fichiers de référence. Côté écriture :
//! `flate::compress` (DEFLATE, RFC 1951) et `dct::encode` (JPEG de base,
//! T.81), utilisés par l'écriture de PDF et par `acrux_features::export`.
//!
//! | Module      | Filtre PDF         | Spécification            | État        |
//! |-------------|--------------------|--------------------------|-------------|
//! | `flate`     | FlateDecode        | RFC 1950 / RFC 1951      | Implémenté  |
//! | `predictor` | /Predictor (PNG, TIFF) | ISO 32000-2 §7.4.4.4 | Implémenté  |
//! | `lzw`       | LZWDecode          | ISO 32000-2 §7.4.4       | Implémenté  |
//! | `runlength` | RunLengthDecode    | ISO 32000-2 §7.4.5       | Implémenté  |
//! | `ascii`     | ASCIIHex / ASCII85 | ISO 32000-2 §7.4.2-3     | Implémenté  |
//! | `ccitt`     | CCITTFaxDecode     | ITU-T T.4 / T.6          | Implémenté (`ccitt::decode` avec `CcittParams`) |
//! | `dct`       | DCTDecode          | ITU-T T.81 (JPEG)        | Implémenté (`dct::decode` → `JpegImage`, `dct::encode::encode` → JPEG de base) |
//! | `jpx`       | JPXDecode          | ISO 15444-1 (JPEG 2000)  | Implémenté (`jpx::decode` → `JpxImage`) |
//! | `jbig2`     | JBIG2Decode        | ITU-T T.88               | Implémenté (`jbig2::decode`, flux embarqué + globaux) |
//!
//! Règle : API uniforme `decode(&[u8], …) -> Result<Vec<u8>>` par module, et un
//! point d'entrée générique [`apply_filter`] qui choisit le codec d'après le nom
//! du filtre (ISO 32000-2 §7.4, tableau 6) et applique les paramètres de
//! /DecodeParms (§7.4.4.3, tableau 8).

pub mod ascii;
pub mod ccitt;
pub mod dct;
pub mod flate;
pub mod jbig2;
pub mod jpx;
pub mod lzw;
pub mod predictor;
pub mod runlength;

use acrux_core::{Error, Result};

/// Paramètres de décodage issus du dictionnaire /DecodeParms
/// (ISO 32000-2 §7.4.4.3, tableau 8).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // un champ par entrée booléenne des tableaux 8, 11 et 12
pub struct DecodeParms {
    /// /Predictor : 1 (aucun), 2 (TIFF) ou 10 à 15 (PNG).
    pub predictor: u32,
    /// /Colors : composantes par pixel.
    pub colors: u32,
    /// /BitsPerComponent : 1, 2, 4, 8 ou 16.
    pub bits_per_component: u32,
    /// /Columns : pixels par ligne.
    pub columns: u32,
    /// /EarlyChange (LZW uniquement) : `true` pour 1, `false` pour 0.
    pub early_change: bool,
    /// `/Columns` était présent dans le dictionnaire (CCITT : sinon 1728).
    pub columns_given: bool,
    /// `/K` (CCITTFaxDecode, tableau 11) : < 0 Groupe 4, 0 Groupe 3 1D, > 0 mixte.
    pub k: i32,
    /// `/Rows` (CCITT) : 0 = inconnu.
    pub rows: u32,
    /// `/BlackIs1` (CCITT).
    pub black_is_1: bool,
    /// `/EncodedByteAlign` (CCITT).
    pub encoded_byte_align: bool,
    /// `/EndOfLine` (CCITT).
    pub end_of_line: bool,
    /// `/EndOfBlock` (CCITT, défaut vrai).
    pub end_of_block: bool,
    /// Contenu décodé du flux `/JBIG2Globals` (JBIG2Decode, tableau 12).
    pub jbig2_globals: Option<Vec<u8>>,
}

impl Default for DecodeParms {
    /// Valeurs par défaut de la spécification (§7.4.4.3, tableau 8).
    fn default() -> Self {
        DecodeParms {
            predictor: 1,
            colors: 1,
            bits_per_component: 8,
            columns: 1,
            early_change: true,
            columns_given: false,
            k: 0,
            rows: 0,
            black_is_1: false,
            encoded_byte_align: false,
            end_of_line: false,
            end_of_block: true,
            jbig2_globals: None,
        }
    }
}

impl DecodeParms {
    /// Paramètres CCITT équivalents (`/Columns` absent → 1728, tableau 11).
    #[must_use]
    pub fn ccitt(&self) -> ccitt::CcittParams {
        ccitt::CcittParams {
            k: self.k,
            columns: if self.columns_given {
                self.columns
            } else {
                1728
            },
            rows: self.rows,
            black_is_1: self.black_is_1,
            byte_align: self.encoded_byte_align,
            end_of_line: self.end_of_line,
            end_of_block: self.end_of_block,
        }
    }

    /// Applique le prédicteur éventuel aux données décompressées.
    fn unpredict(&self, data: Vec<u8>) -> Result<Vec<u8>> {
        if self.predictor <= 1 {
            return Ok(data);
        }
        predictor::apply(
            &data,
            self.predictor,
            self.colors,
            self.bits_per_component,
            self.columns,
        )
    }
}

/// Applique un filtre PDF standard désigné par son nom (§7.4, tableau 6), ou par
/// son abréviation d'image en ligne (§8.9.7, tableau 92).
///
/// Filtres pris en charge : `FlateDecode` / `Fl`, `LZWDecode` / `LZW`,
/// `ASCIIHexDecode` / `AHx`, `ASCII85Decode` / `A85`, `RunLengthDecode` / `RL`.
/// Les prédicteurs de `parms` s'appliquent à Flate et LZW.
///
/// # Errors
///
/// - `Error::Unsupported` pour les filtres d'image (`DCTDecode`, `JPXDecode`,
///   `JBIG2Decode`, `CCITTFaxDecode`), le filtre `Crypt` (géré par la couche
///   document) et tout nom inconnu ;
/// - `Error::Corrupt` si le codec ne parvient à rien décoder.
pub fn apply_filter(name: &[u8], data: &[u8], parms: &DecodeParms) -> Result<Vec<u8>> {
    match name {
        b"FlateDecode" | b"Fl" => parms.unpredict(flate::decode(data)?),
        b"LZWDecode" | b"LZW" => parms.unpredict(lzw::decode(data, parms.early_change)?),
        b"ASCIIHexDecode" | b"AHx" => ascii::decode_hex(data),
        b"ASCII85Decode" | b"A85" => ascii::decode_85(data),
        b"RunLengthDecode" | b"RL" => runlength::decode(data),
        other => Err(Error::Unsupported(format!(
            "filtre {}",
            String::from_utf8_lossy(other)
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_parms_follow_the_spec() {
        let parms = DecodeParms::default();
        assert_eq!(parms.predictor, 1);
        assert_eq!(parms.colors, 1);
        assert_eq!(parms.bits_per_component, 8);
        assert_eq!(parms.columns, 1);
        assert!(parms.early_change);
    }

    #[test]
    fn dispatches_flate_by_full_name_and_abbreviation() {
        let hello = [
            0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00, 0x06, 0x2c, 0x02, 0x15,
        ];
        let parms = DecodeParms::default();
        assert_eq!(
            apply_filter(b"FlateDecode", &hello, &parms),
            Ok(b"hello".to_vec())
        );
        assert_eq!(apply_filter(b"Fl", &hello, &parms), Ok(b"hello".to_vec()));
    }

    #[test]
    fn dispatches_lzw_with_early_change() {
        let spec = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
        let expected = b"-----A---B".to_vec();
        let parms = DecodeParms::default();
        assert_eq!(
            apply_filter(b"LZWDecode", &spec, &parms),
            Ok(expected.clone())
        );
        assert_eq!(apply_filter(b"LZW", &spec, &parms), Ok(expected));
    }

    #[test]
    fn dispatches_ascii_and_runlength() {
        let parms = DecodeParms::default();
        assert_eq!(
            apply_filter(b"ASCIIHexDecode", b"4142>", &parms),
            Ok(b"AB".to_vec())
        );
        assert_eq!(apply_filter(b"AHx", b"4142>", &parms), Ok(b"AB".to_vec()));
        assert_eq!(
            apply_filter(b"ASCII85Decode", b"9jqo~>", &parms),
            Ok(b"Man".to_vec())
        );
        assert_eq!(apply_filter(b"A85", b"9jqo~>", &parms), Ok(b"Man".to_vec()));
        assert_eq!(
            apply_filter(b"RunLengthDecode", &[1, b'o', b'k', 128], &parms),
            Ok(b"ok".to_vec())
        );
        assert_eq!(
            apply_filter(b"RL", &[1, b'o', b'k', 128], &parms),
            Ok(b"ok".to_vec())
        );
    }

    #[test]
    fn applies_png_predictor_after_flate() {
        // Deux lignes de 3 octets, filtre Up sur la seconde, compressées en stored.
        let predicted = [0, 1, 2, 3, 2, 1, 1, 1];
        let compressed = flate::encode(&predicted);
        let parms = DecodeParms {
            predictor: 12,
            columns: 3,
            ..DecodeParms::default()
        };
        assert_eq!(
            apply_filter(b"FlateDecode", &compressed, &parms),
            Ok(vec![1, 2, 3, 2, 3, 4])
        );
    }

    #[test]
    fn applies_tiff_predictor_after_lzw() {
        // « -----A---B » lu comme 10 échantillons de 8 bits en différences TIFF.
        let spec = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
        let parms = DecodeParms {
            predictor: 2,
            columns: 10,
            ..DecodeParms::default()
        };
        let expected: Vec<u8> = b"-----A---B"
            .iter()
            .scan(0u8, |acc, &b| {
                *acc = acc.wrapping_add(b);
                Some(*acc)
            })
            .collect();
        assert_eq!(apply_filter(b"LZWDecode", &spec, &parms), Ok(expected));
    }

    #[test]
    fn image_filters_are_unsupported_for_now() {
        let parms = DecodeParms::default();
        for name in [
            &b"DCTDecode"[..],
            b"DCT",
            b"JPXDecode",
            b"JBIG2Decode",
            b"CCITTFaxDecode",
            b"CCF",
        ] {
            assert!(matches!(
                apply_filter(name, b"", &parms),
                Err(Error::Unsupported(_))
            ));
        }
    }

    #[test]
    fn unknown_filter_is_unsupported() {
        assert!(matches!(
            apply_filter(b"Inconnu", b"", &DecodeParms::default()),
            Err(Error::Unsupported(_))
        ));
    }
}
