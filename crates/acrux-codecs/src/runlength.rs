//! Filtre RunLengthDecode (ISO 32000-2 §7.4.5).
//!
//! Le flux est une suite de plages, chacune introduite par un octet de
//! longueur `L` :
//!
//! - `0 ≤ L ≤ 127` : les `L + 1` octets suivants sont copiés tels quels ;
//! - `129 ≤ L ≤ 255` : l'octet suivant est répété `257 − L` fois ;
//! - `L = 128` : fin des données (EOD).
//!
//! Tolérance : une plage tronquée en fin de flux rend les octets disponibles,
//! et l'absence d'EOD n'est pas une erreur.

use acrux_core::Result;

/// Marqueur de fin de données (§7.4.5).
const END_OF_DATA: u8 = 128;

/// Décode un flux RunLengthDecode.
///
/// # Errors
///
/// Cette fonction ne retourne jamais d'erreur aujourd'hui (un flux tronqué
/// rend les données disponibles) ; le type `Result` est conservé pour
/// l'uniformité de l'API des codecs.
pub fn decode(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut pos = 0usize;

    while let Some(&length) = data.get(pos) {
        pos += 1;
        match length {
            END_OF_DATA => break,
            0..=127 => {
                let count = usize::from(length) + 1;
                let end = (pos + count).min(data.len());
                out.extend_from_slice(&data[pos..end]);
                pos = end;
            }
            _ => {
                let Some(&byte) = data.get(pos) else {
                    break;
                };
                pos += 1;
                let repeat = 257 - usize::from(length);
                out.resize(out.len() + repeat, byte);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_literal_and_repeat_runs() {
        // 2 → « abc » littéral ; 254 → 'x' × 3 ; EOD.
        assert_eq!(
            decode(&[2, b'a', b'b', b'c', 254, b'x', 128]),
            Ok(b"abcxxx".to_vec())
        );
    }

    #[test]
    fn repeat_run_of_maximum_length() {
        // 129 → 128 répétitions.
        assert_eq!(decode(&[129, 7]), Ok(vec![7; 128]));
    }

    #[test]
    fn literal_run_of_maximum_length() {
        let mut data = vec![127];
        data.extend(0..128u8);
        assert_eq!(decode(&data), Ok((0..128u8).collect()));
    }

    #[test]
    fn stops_at_end_of_data_marker() {
        assert_eq!(decode(&[0, b'a', 128, 0, b'b']), Ok(b"a".to_vec()));
    }

    #[test]
    fn missing_eod_is_tolerated() {
        assert_eq!(decode(&[1, b'a', b'b']), Ok(b"ab".to_vec()));
    }

    #[test]
    fn truncated_literal_run_returns_available_bytes() {
        assert_eq!(decode(&[5, b'a', b'b']), Ok(b"ab".to_vec()));
    }

    #[test]
    fn truncated_repeat_run_returns_previous_data() {
        assert_eq!(decode(&[0, b'a', 200]), Ok(b"a".to_vec()));
    }

    #[test]
    fn empty_input() {
        assert_eq!(decode(&[]), Ok(Vec::new()));
        assert_eq!(decode(&[128]), Ok(Vec::new()));
    }
}
