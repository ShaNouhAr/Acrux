//! Filtres ASCIIHexDecode (ISO 32000-2 §7.4.2) et ASCII85Decode (§7.4.3).
//!
//! Ces deux filtres transforment des données binaires en texte 7 bits. Ils
//! sont tolérants : les blancs sont ignorés partout, le marqueur de fin est
//! facultatif, et un caractère étranger est simplement sauté (Acrobat ne
//! rejette pas le flux pour autant).

use acrux_core::{Error, Result};

/// Blanc au sens PDF (ISO 32000-2 §7.2.3, tableau 1).
fn is_pdf_whitespace(byte: u8) -> bool {
    matches!(byte, 0x00 | 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

/// Décode un flux ASCIIHexDecode (§7.4.2).
///
/// Chaque paire de chiffres hexadécimaux donne un octet ; `>` marque la fin ;
/// un chiffre isolé en fin de flux est complété par un 0 (poids faible).
///
/// # Errors
///
/// Cette fonction ne retourne jamais d'erreur aujourd'hui (les caractères
/// invalides sont ignorés) ; le type `Result` est conservé pour l'uniformité
/// de l'API des codecs.
pub fn decode_hex(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 2);
    let mut pending: Option<u8> = None;

    for &byte in data {
        if byte == b'>' {
            break;
        }
        let Some(digit) = hex_value(byte) else {
            continue;
        };
        match pending.take() {
            Some(high) => out.push((high << 4) | digit),
            None => pending = Some(digit),
        }
    }
    if let Some(high) = pending {
        out.push(high << 4);
    }
    Ok(out)
}

/// Valeur d'un chiffre hexadécimal, `None` pour tout autre caractère.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Décode un flux ASCII85Decode (§7.4.3).
///
/// Cinq caractères `!`..`u` codent quatre octets (base 85, grand-boutiste) ;
/// `z` code quatre octets nuls ; `~>` marque la fin ; un préfixe `<~` est
/// accepté (usage PostScript courant dans les PDF). Un dernier groupe partiel
/// de n caractères (2 ≤ n ≤ 4) donne n − 1 octets.
///
/// # Errors
///
/// `Error::Corrupt` si un groupe dépasse 2^32 − 1, si `z` apparaît au milieu
/// d'un groupe, ou si le dernier groupe ne compte qu'un seul caractère.
pub fn decode_85(data: &[u8]) -> Result<Vec<u8>> {
    let start = data
        .iter()
        .position(|&b| !is_pdf_whitespace(b))
        .unwrap_or(data.len());
    let body = data.get(start..).unwrap_or(&[]);
    let body = body.strip_prefix(b"<~").unwrap_or(body);

    let mut out = Vec::with_capacity(body.len() * 4 / 5);
    let mut group = [0u8; 5];
    let mut filled = 0usize;

    for &byte in body {
        match byte {
            b'~' => break,
            b'z' if filled == 0 => out.extend_from_slice(&[0, 0, 0, 0]),
            b'z' => {
                return Err(Error::Corrupt(
                    "ASCII85 : 'z' au milieu d'un groupe".to_string(),
                ))
            }
            b'!'..=b'u' => {
                group[filled] = byte - b'!';
                filled += 1;
                if filled == 5 {
                    out.extend_from_slice(&decode_group(group, 5)?);
                    filled = 0;
                }
            }
            // Blancs et caractères étrangers : ignorés.
            _ => {}
        }
    }

    match filled {
        0 => {}
        1 => {
            return Err(Error::Corrupt(
                "ASCII85 : groupe final d'un seul caractère".to_string(),
            ))
        }
        n => {
            // Groupe partiel : complété avec 'u' (84), on garde n − 1 octets.
            group[n..].fill(84);
            out.extend_from_slice(&decode_group(group, n)?[..n - 1]);
        }
    }
    Ok(out)
}

/// Convertit un groupe de cinq valeurs base 85 en quatre octets.
/// `count` est le nombre de caractères réellement lus (pour le message d'erreur).
fn decode_group(group: [u8; 5], count: usize) -> Result<[u8; 4]> {
    let mut value: u64 = 0;
    for digit in group {
        value = value * 85 + u64::from(digit);
    }
    let value = u32::try_from(value).map_err(|_| {
        Error::Corrupt(format!(
            "ASCII85 : groupe de {count} caractères hors de la plage 32 bits"
        ))
    })?;
    Ok(value.to_be_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_decodes_pairs_and_ignores_whitespace() {
        assert_eq!(decode_hex(b"48 65\n6C6c\r6F>"), Ok(b"Hello".to_vec()));
    }

    #[test]
    fn hex_accepts_lowercase_and_uppercase() {
        assert_eq!(decode_hex(b"aAbBfF09"), Ok(vec![0xAA, 0xBB, 0xFF, 0x09]));
    }

    #[test]
    fn hex_completes_odd_digit_with_zero() {
        assert_eq!(decode_hex(b"7>"), Ok(vec![0x70]));
        assert_eq!(decode_hex(b"417"), Ok(vec![0x41, 0x70]));
    }

    #[test]
    fn hex_stops_at_end_marker() {
        assert_eq!(decode_hex(b"41>42"), Ok(vec![0x41]));
    }

    #[test]
    fn hex_skips_invalid_characters() {
        assert_eq!(decode_hex(b"4x1"), Ok(vec![0x41]));
    }

    #[test]
    fn hex_empty_input() {
        assert_eq!(decode_hex(b""), Ok(Vec::new()));
        assert_eq!(decode_hex(b">"), Ok(Vec::new()));
    }

    #[test]
    fn a85_decodes_known_text() {
        // Vecteurs classiques : « Man » → « 9jqo », « Man  » → « 9jqo^ ».
        assert_eq!(decode_85(b"9jqo^~>"), Ok(b"Man ".to_vec()));
        assert_eq!(
            decode_85(b"<~9jqo^BlbD-BleB1DJ+*+F(f,q~>"),
            Ok(b"Man is distinguished".to_vec())
        );
    }

    #[test]
    fn a85_handles_partial_final_group() {
        assert_eq!(decode_85(b"F*2M7/c~>"), Ok(b"sure.".to_vec()));
        assert_eq!(decode_85(b"9jqo~>"), Ok(b"Man".to_vec()));
        assert_eq!(decode_85(b"BE~>"), Ok(b"h".to_vec()));
        assert_eq!(decode_85(b"BOtu~>"), Ok(b"hel".to_vec()));
    }

    #[test]
    fn a85_z_expands_to_four_zero_bytes() {
        assert_eq!(
            decode_85(b"z9jqo^z~>"),
            Ok(vec![0, 0, 0, 0, b'M', b'a', b'n', b' ', 0, 0, 0, 0])
        );
    }

    #[test]
    fn a85_ignores_whitespace_and_missing_terminator() {
        assert_eq!(decode_85(b" \n<~9j\r\nqo ^"), Ok(b"Man ".to_vec()));
    }

    #[test]
    fn a85_rejects_z_inside_group() {
        assert!(matches!(decode_85(b"9jzqo^~>"), Err(Error::Corrupt(_))));
    }

    #[test]
    fn a85_rejects_single_trailing_character() {
        assert!(matches!(decode_85(b"9jqo^9~>"), Err(Error::Corrupt(_))));
    }

    #[test]
    fn a85_rejects_group_overflow() {
        // « s8W-! » vaut exactement 2^32 − 1 : accepté.
        assert_eq!(decode_85(b"s8W-!~>"), Ok(vec![0xFF, 0xFF, 0xFF, 0xFF]));
        // « s8W-" » vaut 2^32 : refusé.
        assert!(matches!(decode_85(b"s8W-\"~>"), Err(Error::Corrupt(_))));
    }

    #[test]
    fn a85_all_zero_group_written_longhand() {
        assert_eq!(decode_85(b"!!!!!~>"), Ok(vec![0, 0, 0, 0]));
    }

    #[test]
    fn a85_empty_input() {
        assert_eq!(decode_85(b""), Ok(Vec::new()));
        assert_eq!(decode_85(b"<~~>"), Ok(Vec::new()));
    }
}
