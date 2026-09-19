//! Encodage WinAnsi et littéraux PDF pour le texte que nous **écrivons**
//! (filigrane, en-tête, pied de page, numérotation Bates, apparences).
//!
//! Les métriques des quatorze polices standard ne sont plus ici : elles sont
//! descendues dans [`acrux_fonts::standard`], parce que le rendu en a besoin
//! autant que l'écriture. Un PDF qui emploie Helvetica sans `/Widths` doit se
//! composer pareil qu'on soit en train de l'afficher ou d'y ajouter une ligne
//! — deux tables séparées auraient fini par diverger.
//!
//! Reste ici ce qui ne concerne que l'écriture : encoder une chaîne en
//! WinAnsiEncoding en signalant ce qui n'y tient pas, et échapper un littéral.

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Une des quatorze polices standard.
///
/// Réexport de [`acrux_fonts::standard::Standard`] : même type, nom conservé
/// pour les appelants.
pub use acrux_fonts::standard::Standard as StandardFont;

/// Table inverse de WinAnsiEncoding : caractère Unicode → code.
///
/// Construite une seule fois à partir des tables d'`acrux-fonts` (annexe D),
/// pour ne pas dupliquer l'encodage du moteur.
fn reverse_win_ansi() -> &'static BTreeMap<char, u8> {
    static TABLE: OnceLock<BTreeMap<char, u8>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut map = BTreeMap::new();
        for code in 32..=255u8 {
            let Some(name) = acrux_fonts::encodings::win_ansi(code) else {
                continue;
            };
            let Some(c) = acrux_fonts::encodings::glyph_name_to_unicode(name) else {
                continue;
            };
            map.entry(c).or_insert(code);
        }
        map
    })
}

/// Texte encodé pour une police standard en WinAnsiEncoding.
#[derive(Debug, Clone, Default)]
pub struct Encoded {
    /// Octets à écrire dans l'opérateur `Tj`.
    pub bytes: Vec<u8>,
    /// Correspondances code → texte, pour le `/ToUnicode` de la police.
    pub mapping: Vec<(u8, char)>,
    /// Caractères remplacés par `?` faute d'être représentables en WinAnsi.
    pub replaced: Vec<char>,
}

/// Encode un texte en WinAnsiEncoding ; les caractères absents de l'encodage
/// deviennent `?` et sont signalés.
#[must_use]
pub fn encode_win_ansi(text: &str) -> Encoded {
    let table = reverse_win_ansi();
    let mut out = Encoded::default();
    for c in text.chars() {
        if let Some(&code) = table.get(&c) {
            out.bytes.push(code);
            out.mapping.push((code, c));
        } else {
            out.bytes.push(b'?');
            out.mapping.push((b'?', '?'));
            out.replaced.push(c);
        }
    }
    out.mapping.sort_unstable();
    out.mapping.dedup();
    out
}

/// Chaîne littérale PDF `(…)` avec les échappements de §7.3.4.2.
#[must_use]
pub fn pdf_literal(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 2);
    out.push('(');
    for &b in bytes {
        match b {
            b'(' => out.push_str("\\("),
            b')' => out.push_str("\\)"),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7E => out.push(b as char),
            _ => {
                out.push('\\');
                out.push(char::from(b'0' + (b >> 6)));
                out.push(char::from(b'0' + ((b >> 3) & 7)));
                out.push(char::from(b'0' + (b & 7)));
            }
        }
    }
    out.push(')');
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn font_names_are_recognised() {
        assert_eq!(
            StandardFont::from_name("Times-BoldItalic"),
            Some(StandardFont::TimesBoldItalic)
        );
        assert_eq!(
            StandardFont::from_name("helvetica oblique"),
            Some(StandardFont::HelveticaOblique)
        );
        assert_eq!(
            StandardFont::from_name("COURIER"),
            Some(StandardFont::Courier)
        );
        assert_eq!(
            StandardFont::from_name("arial-bold"),
            Some(StandardFont::HelveticaBold)
        );
        assert_eq!(StandardFont::from_name("Wingdings"), None);
        assert_eq!(
            StandardFont::TimesBoldItalic.base_font(),
            "Times-BoldItalic"
        );
    }

    #[test]
    fn widths_follow_the_afm_tables() {
        // Valeurs connues des fichiers AFM.
        assert_eq!(StandardFont::Helvetica.char_width('A'), 667.0);
        assert_eq!(StandardFont::Helvetica.char_width(' '), 278.0);
        assert_eq!(StandardFont::HelveticaBold.char_width('W'), 944.0);
        assert_eq!(StandardFont::TimesRoman.char_width('m'), 778.0);
        assert_eq!(StandardFont::TimesItalic.char_width('f'), 278.0);
        assert_eq!(StandardFont::Courier.char_width('i'), 600.0);
        assert_eq!(StandardFont::CourierBoldOblique.char_width('W'), 600.0);
        // Un composé a l'avance de sa lettre de base.
        assert_eq!(
            StandardFont::Helvetica.char_width('é'),
            StandardFont::Helvetica.char_width('e')
        );
        assert_eq!(
            StandardFont::TimesBold.char_width('Ç'),
            StandardFont::TimesBold.char_width('C')
        );
        // Les symboles passent par la table dédiée.
        assert_eq!(StandardFont::Helvetica.char_width('€'), 556.0);
        assert_eq!(StandardFont::TimesRoman.char_width('«'), 500.0);
        // Les obliques ont les largeurs des droites.
        assert_eq!(
            StandardFont::HelveticaOblique.char_width('g'),
            StandardFont::Helvetica.char_width('g')
        );
    }

    #[test]
    fn text_width_scales_with_size() {
        let w = StandardFont::Helvetica.text_width("AB", 10.0);
        assert!((w - (667.0 + 667.0) / 100.0).abs() < 1e-9);
        assert_eq!(StandardFont::Helvetica.text_width("", 12.0), 0.0);
        let double = StandardFont::Helvetica.text_width("AB", 20.0);
        assert!((double - 2.0 * w).abs() < 1e-9);
    }

    #[test]
    fn win_ansi_encoding_and_replacement() {
        let e = encode_win_ansi("Été « 42 » €");
        assert!(e.replaced.is_empty(), "{:?}", e.replaced);
        assert_eq!(e.bytes[0], 0xC9); // Eacute
        assert!(e.mapping.iter().any(|(c, u)| *c == 0x80 && *u == '€'));
        // Un caractère hors WinAnsi devient « ? » et est signalé.
        let e = encode_win_ansi("漢字");
        assert_eq!(e.bytes, b"??");
        assert_eq!(e.replaced, vec!['漢', '字']);
    }

    #[test]
    fn literal_strings_are_escaped() {
        assert_eq!(pdf_literal(b"a(b)c\\d"), "(a\\(b\\)c\\\\d)");
        assert_eq!(pdf_literal(&[0xE9]), "(\\351)");
        assert_eq!(pdf_literal(b"\n"), "(\\012)");
    }
}
