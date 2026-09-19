//! Décodage des chaînes de texte PDF (ISO 32000-2 §7.9.2.2) :
//! UTF-16BE avec BOM, UTF-8 avec BOM (PDF 2.0), sinon PDFDocEncoding (annexe D).

/// Convertit une chaîne de texte PDF en `String`.
#[must_use]
pub fn decode_text_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .chunks(2)
            .map(|c| u16::from(c[0]) << 8 | u16::from(*c.get(1).unwrap_or(&0)))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        // BOM inversé : hors spec mais rencontré.
        let units: Vec<u16> = rest
            .chunks(2)
            .map(|c| u16::from(*c.get(1).unwrap_or(&0)) << 8 | u16::from(c[0]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    bytes.iter().map(|&b| pdfdoc_char(b)).collect()
}

/// Caractère Unicode d'un octet PDFDocEncoding (annexe D.2). Les octets
/// 0x00-0x7F et 0xA1-0xFF coïncident avec Latin-1 sauf exceptions listées.
#[must_use]
pub fn pdfdoc_char(b: u8) -> char {
    match b {
        0x18 => '\u{02D8}',        // breve
        0x19 => '\u{02C7}',        // caron
        0x1A => '\u{02C6}',        // circumflex
        0x1B => '\u{02D9}',        // dot above
        0x1C => '\u{02DD}',        // double acute
        0x1D => '\u{02DB}',        // ogonek
        0x1E => '\u{02DA}',        // ring
        0x1F => '\u{02DC}',        // tilde
        0x80 => '\u{2022}',        // bullet
        0x81 => '\u{2020}',        // dagger
        0x82 => '\u{2021}',        // double dagger
        0x83 => '\u{2026}',        // ellipsis
        0x84 => '\u{2014}',        // em dash
        0x85 => '\u{2013}',        // en dash
        0x86 => '\u{0192}',        // florin
        0x87 => '\u{2044}',        // fraction slash
        0x88 => '\u{2039}',        // single left guillemet
        0x89 => '\u{203A}',        // single right guillemet
        0x8A => '\u{2212}',        // minus
        0x8B => '\u{2030}',        // per mille
        0x8C => '\u{201E}',        // low double quote
        0x8D => '\u{201C}',        // left double quote
        0x8E => '\u{201D}',        // right double quote
        0x8F => '\u{2018}',        // left single quote
        0x90 => '\u{2019}',        // right single quote
        0x91 => '\u{201A}',        // low single quote
        0x92 => '\u{2122}',        // trademark
        0x93 => '\u{FB01}',        // fi
        0x94 => '\u{FB02}',        // fl
        0x95 => '\u{0141}',        // L stroke
        0x96 => '\u{0152}',        // OE
        0x97 => '\u{0160}',        // S caron
        0x98 => '\u{0178}',        // Y diaeresis
        0x99 => '\u{017D}',        // Z caron
        0x9A => '\u{0131}',        // dotless i
        0x9B => '\u{0142}',        // l stroke
        0x9C => '\u{0153}',        // oe
        0x9D => '\u{0161}',        // s caron
        0x9E => '\u{017E}',        // z caron
        0x9F | 0xAD => '\u{FFFD}', // non définis
        0xA0 => '\u{20AC}',        // euro
        other => char::from(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_with_bom() {
        assert_eq!(
            decode_text_string(&[0xFE, 0xFF, 0x00, 0x48, 0x00, 0xE9]),
            "Hé"
        );
        assert_eq!(decode_text_string(&[0xFF, 0xFE, 0x48, 0x00]), "H");
    }

    #[test]
    fn utf8_with_bom() {
        assert_eq!(decode_text_string("\u{FEFF}été".as_bytes()), "été");
    }

    #[test]
    fn pdfdoc() {
        assert_eq!(decode_text_string(b"Caf\xE9"), "Café");
        assert_eq!(
            decode_text_string(&[0x80, 0x20, 0x8D, 0x41, 0x8E, 0xA0]),
            "• “A”€"
        );
    }
}
