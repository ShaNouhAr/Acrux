//! Polices standard (« Base 14 », ISO 32000-2 annexe D) : choix, encodage
//! WinAnsi et métriques.
//!
//! Les apports visuels de ce module (filigrane texte, en-tête, pied de page,
//! numérotation Bates) doivent pouvoir mesurer une chaîne **au moment de
//! l'écriture**, pour centrer ou aligner à droite. Les largeurs viennent des
//! fichiers AFM d'Adobe ; elles sont ici sous une forme compacte :
//!
//! - codes 32 à 126 : une table de 95 largeurs par famille ;
//! - lettres accentuées de WinAnsi : ce sont des composés, leur avance est
//!   **exactement** celle de la lettre de base (`é` = `e`, `Ç` = `C`…) ;
//! - ponctuation et symboles du haut de WinAnsi : table [`SPECIALS`] ;
//! - tout le reste : largeur de `n`, approximation documentée (aucun de ces
//!   caractères n'est représentable en WinAnsi, il sera de toute façon
//!   remplacé par `?` à l'encodage).
//!
//! Courier est à chasse fixe (600 partout) et les variantes obliques ont les
//! mêmes largeurs que les droites.

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Police standard utilisable pour un filigrane, un en-tête, un pied de page
/// ou une numérotation Bates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, PartialOrd, Ord)]
pub enum StandardFont {
    /// Helvetica.
    #[default]
    Helvetica,
    /// Helvetica-Bold.
    HelveticaBold,
    /// Helvetica-Oblique.
    HelveticaOblique,
    /// Helvetica-BoldOblique.
    HelveticaBoldOblique,
    /// Times-Roman.
    TimesRoman,
    /// Times-Bold.
    TimesBold,
    /// Times-Italic.
    TimesItalic,
    /// Times-BoldItalic.
    TimesBoldItalic,
    /// Courier.
    Courier,
    /// Courier-Bold.
    CourierBold,
    /// Courier-Oblique.
    CourierOblique,
    /// Courier-BoldOblique.
    CourierBoldOblique,
}

impl StandardFont {
    /// Nom `/BaseFont` de la police.
    #[must_use]
    pub fn base_font(self) -> &'static str {
        match self {
            StandardFont::Helvetica => "Helvetica",
            StandardFont::HelveticaBold => "Helvetica-Bold",
            StandardFont::HelveticaOblique => "Helvetica-Oblique",
            StandardFont::HelveticaBoldOblique => "Helvetica-BoldOblique",
            StandardFont::TimesRoman => "Times-Roman",
            StandardFont::TimesBold => "Times-Bold",
            StandardFont::TimesItalic => "Times-Italic",
            StandardFont::TimesBoldItalic => "Times-BoldItalic",
            StandardFont::Courier => "Courier",
            StandardFont::CourierBold => "Courier-Bold",
            StandardFont::CourierOblique => "Courier-Oblique",
            StandardFont::CourierBoldOblique => "Courier-BoldOblique",
        }
    }

    /// Police d'après son nom, insensible à la casse et aux tirets : le nom
    /// PostScript exact (`Times-BoldItalic`), une forme libre (`times bold
    /// italic`, `helvetica-oblique`) ou une famille seule (`courier`).
    ///
    /// Renvoie `None` si le nom ne désigne aucune police standard.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        let key: String = lower.chars().filter(char::is_ascii_alphanumeric).collect();
        let bold = key.contains("bold");
        let italic = key.contains("italic") || key.contains("oblique");
        if key.contains("courier") || key.contains("mono") {
            return Some(match (bold, italic) {
                (true, true) => StandardFont::CourierBoldOblique,
                (true, false) => StandardFont::CourierBold,
                (false, true) => StandardFont::CourierOblique,
                (false, false) => StandardFont::Courier,
            });
        }
        if key.contains("times") || key.contains("roman") || key.contains("serif") {
            return Some(match (bold, italic) {
                (true, true) => StandardFont::TimesBoldItalic,
                (true, false) => StandardFont::TimesBold,
                (false, true) => StandardFont::TimesItalic,
                (false, false) => StandardFont::TimesRoman,
            });
        }
        if key.contains("helvetica") || key.contains("arial") || key.contains("sans") {
            return Some(match (bold, italic) {
                (true, true) => StandardFont::HelveticaBoldOblique,
                (true, false) => StandardFont::HelveticaBold,
                (false, true) => StandardFont::HelveticaOblique,
                (false, false) => StandardFont::Helvetica,
            });
        }
        None
    }

    /// Hauteur de hampe au-dessus de la ligne de base, en millièmes d'em
    /// (`Ascender` du fichier AFM).
    #[must_use]
    pub fn ascent(self) -> f64 {
        match self.family() {
            Family::Helvetica => 718.0,
            Family::Times => 683.0,
            Family::Courier => 629.0,
        }
    }

    /// Profondeur de jambage sous la ligne de base, négative, en millièmes
    /// d'em (`Descender` du fichier AFM).
    #[must_use]
    pub fn descent(self) -> f64 {
        match self.family() {
            Family::Helvetica => -207.0,
            Family::Times => -217.0,
            Family::Courier => -157.0,
        }
    }

    /// Largeur d'un caractère en millièmes d'em.
    #[must_use]
    pub fn char_width(self, c: char) -> f64 {
        if self.family() == Family::Courier {
            return 600.0;
        }
        let column = self.column();
        if let Some(row) = SPECIALS.iter().find(|r| r.0 == c) {
            return f64::from(row.1[column]);
        }
        let table = self.table();
        let base = base_letter(c);
        let index = (base as usize).wrapping_sub(32);
        f64::from(
            table
                .get(index)
                .copied()
                .unwrap_or(table['n' as usize - 32]),
        )
    }

    /// Largeur d'une chaîne à la taille donnée, en points.
    #[must_use]
    pub fn text_width(self, text: &str, size: f64) -> f64 {
        text.chars().map(|c| self.char_width(c)).sum::<f64>() * size / 1000.0
    }

    fn family(self) -> Family {
        match self {
            StandardFont::Helvetica
            | StandardFont::HelveticaBold
            | StandardFont::HelveticaOblique
            | StandardFont::HelveticaBoldOblique => Family::Helvetica,
            StandardFont::TimesRoman
            | StandardFont::TimesBold
            | StandardFont::TimesItalic
            | StandardFont::TimesBoldItalic => Family::Times,
            StandardFont::Courier
            | StandardFont::CourierBold
            | StandardFont::CourierOblique
            | StandardFont::CourierBoldOblique => Family::Courier,
        }
    }

    /// Colonne de [`SPECIALS`] : Helvetica, Helvetica-Bold, Times-Roman,
    /// Times-Bold, Times-Italic, Times-BoldItalic.
    fn column(self) -> usize {
        match self {
            StandardFont::HelveticaBold | StandardFont::HelveticaBoldOblique => 1,
            StandardFont::TimesRoman => 2,
            StandardFont::TimesBold => 3,
            StandardFont::TimesItalic => 4,
            StandardFont::TimesBoldItalic => 5,
            _ => 0,
        }
    }

    fn table(self) -> &'static [u16; 95] {
        match self.column() {
            1 => &HELVETICA_BOLD,
            2 => &TIMES_ROMAN,
            3 => &TIMES_BOLD,
            4 => &TIMES_ITALIC,
            5 => &TIMES_BOLD_ITALIC,
            _ => &HELVETICA,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Helvetica,
    Times,
    Courier,
}

/// Lettre de base d'un composé WinAnsi : son avance est celle du composé.
fn base_letter(c: char) -> char {
    match c {
        'À'..='Å' => 'A',
        'Ç' => 'C',
        'È'..='Ë' => 'E',
        'Ì'..='Ï' => 'I',
        'Ñ' => 'N',
        'Ò'..='Ö' => 'O',
        'Ù'..='Ü' => 'U',
        'Ý' | 'Ÿ' => 'Y',
        'Š' => 'S',
        'Ž' => 'Z',
        'à'..='å' => 'a',
        'ç' => 'c',
        'è'..='ë' => 'e',
        'ì'..='ï' => 'i',
        'ñ' => 'n',
        'ò'..='ö' => 'o',
        'ù'..='ü' => 'u',
        'ý' | 'ÿ' => 'y',
        'š' => 's',
        'ž' => 'z',
        '\u{00A0}' => ' ',
        '\u{00AD}' => '-',
        other => other,
    }
}

/// Largeurs Helvetica (AFM Adobe), codes 32 à 126.
const HELVETICA: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

/// Largeurs Helvetica-Bold, codes 32 à 126.
const HELVETICA_BOLD: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, 975, 722, 722, 722, 722, 667,
    611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 333, 278, 333, 584, 556, 333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556,
    278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
];

/// Largeurs Times-Roman, codes 32 à 126.
const TIMES_ROMAN: [u16; 95] = [
    250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444, 921, 722, 667, 667, 722, 611,
    556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667, 556, 611, 722, 722, 944, 722,
    722, 611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500,
    278, 778, 500, 500, 500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541,
];

/// Largeurs Times-Bold, codes 32 à 126.
const TIMES_BOLD: [u16; 95] = [
    250, 333, 555, 500, 500, 1000, 833, 278, 333, 333, 500, 570, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500, 930, 722, 667, 722, 722, 667,
    611, 778, 778, 389, 500, 778, 667, 944, 722, 778, 611, 778, 722, 556, 667, 722, 722, 1000, 722,
    722, 667, 333, 278, 333, 581, 500, 333, 500, 556, 444, 556, 444, 333, 500, 556, 278, 333, 556,
    278, 833, 556, 500, 556, 556, 444, 389, 333, 556, 500, 722, 500, 500, 444, 394, 220, 394, 520,
];

/// Largeurs Times-Italic, codes 32 à 126.
const TIMES_ITALIC: [u16; 95] = [
    250, 333, 420, 500, 500, 833, 778, 214, 333, 333, 500, 675, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 333, 333, 675, 675, 675, 500, 920, 611, 611, 667, 722, 611,
    611, 722, 722, 333, 444, 667, 556, 833, 667, 722, 611, 722, 611, 500, 556, 722, 611, 833, 611,
    556, 556, 389, 278, 389, 422, 500, 333, 500, 500, 444, 500, 444, 278, 500, 500, 278, 278, 444,
    278, 722, 500, 500, 500, 500, 389, 389, 278, 500, 444, 667, 444, 444, 389, 400, 275, 400, 541,
];

/// Largeurs Times-BoldItalic, codes 32 à 126.
const TIMES_BOLD_ITALIC: [u16; 95] = [
    250, 389, 555, 500, 500, 833, 778, 278, 333, 333, 500, 570, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500, 832, 667, 667, 667, 722, 667,
    667, 722, 778, 389, 500, 667, 611, 889, 722, 722, 611, 722, 667, 556, 611, 722, 667, 889, 667,
    611, 611, 333, 278, 333, 570, 500, 333, 500, 500, 444, 500, 444, 333, 500, 556, 278, 278, 500,
    278, 778, 556, 500, 500, 500, 389, 389, 278, 556, 444, 667, 500, 444, 389, 348, 220, 348, 570,
];

/// Largeurs des symboles du haut de WinAnsi qui ne sont pas des composés :
/// (caractère, [Helvetica, Helvetica-Bold, Times-Roman, Times-Bold,
/// Times-Italic, Times-BoldItalic]).
const SPECIALS: [(char, [u16; 6]); 55] = [
    ('€', [556, 556, 500, 500, 500, 500]),
    ('‚', [222, 278, 333, 333, 333, 333]),
    ('ƒ', [556, 556, 500, 500, 500, 500]),
    ('„', [333, 500, 444, 500, 500, 500]),
    ('…', [1000, 1000, 1000, 1000, 889, 1000]),
    ('†', [556, 556, 500, 500, 500, 500]),
    ('‡', [556, 556, 500, 500, 500, 500]),
    ('ˆ', [333, 333, 333, 333, 333, 333]),
    ('‰', [1000, 1000, 1000, 1000, 1000, 1000]),
    ('‹', [333, 333, 333, 333, 333, 333]),
    ('Œ', [1000, 1000, 889, 1000, 944, 944]),
    ('‘', [222, 278, 333, 333, 333, 333]),
    ('’', [222, 278, 333, 333, 333, 333]),
    ('“', [333, 500, 444, 500, 556, 500]),
    ('”', [333, 500, 444, 500, 556, 500]),
    ('•', [350, 350, 350, 350, 350, 350]),
    ('–', [556, 556, 500, 500, 500, 500]),
    ('—', [1000, 1000, 1000, 1000, 889, 1000]),
    ('˜', [333, 333, 333, 333, 333, 333]),
    ('™', [1000, 1000, 980, 1000, 980, 1000]),
    ('›', [333, 333, 333, 333, 333, 333]),
    ('œ', [944, 611, 722, 722, 667, 722]),
    ('¡', [333, 333, 333, 333, 389, 389]),
    ('¢', [556, 556, 500, 500, 500, 500]),
    ('£', [556, 556, 500, 500, 500, 500]),
    ('¤', [556, 556, 500, 500, 500, 500]),
    ('¥', [556, 556, 500, 500, 500, 500]),
    ('¦', [260, 280, 200, 220, 275, 220]),
    ('§', [556, 556, 500, 500, 500, 500]),
    ('¨', [333, 333, 333, 333, 333, 333]),
    ('©', [737, 737, 760, 747, 760, 747]),
    ('ª', [370, 370, 276, 300, 276, 266]),
    ('«', [556, 556, 500, 500, 500, 500]),
    ('¬', [584, 584, 564, 570, 675, 606]),
    ('®', [737, 737, 760, 747, 760, 747]),
    ('¯', [333, 333, 333, 333, 333, 333]),
    ('°', [400, 400, 400, 400, 400, 400]),
    ('±', [584, 584, 564, 570, 675, 570]),
    ('²', [333, 333, 300, 300, 300, 300]),
    ('³', [333, 333, 300, 300, 300, 300]),
    ('´', [333, 333, 333, 333, 333, 333]),
    ('µ', [556, 611, 500, 556, 500, 576]),
    ('¶', [537, 556, 453, 540, 523, 500]),
    ('·', [278, 278, 250, 250, 250, 250]),
    ('¸', [333, 333, 333, 333, 333, 333]),
    ('¹', [333, 333, 300, 300, 300, 300]),
    ('º', [365, 365, 310, 330, 310, 300]),
    ('»', [556, 556, 500, 500, 500, 500]),
    ('¼', [834, 834, 750, 750, 750, 750]),
    ('½', [834, 834, 750, 750, 750, 750]),
    ('¾', [834, 834, 750, 750, 750, 750]),
    ('¿', [611, 611, 444, 500, 500, 500]),
    ('Æ', [1000, 1000, 889, 1000, 889, 944]),
    ('Ø', [778, 778, 722, 778, 722, 722]),
    ('æ', [889, 889, 667, 722, 667, 722]),
];

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
