//! Métriques des quatorze polices standard (ISO 32000-2 §9.6.2.2, annexe D).
//!
//! Un PDF a le droit d'écrire `/BaseFont /Helvetica` sans `/Widths` et sans
//! programme incorporé : le lecteur est censé **connaître** cette police. Ce
//! module est cette connaissance. Sans lui, la seule issue est de mesurer une
//! police du système, et le même fichier ne se compose plus pareil d'une
//! machine à l'autre — sur une machine sans police du tout, les largeurs
//! tombent à zéro et les lettres s'empilent au même endroit.
//!
//! Les largeurs sont celles des fichiers AFM d'Adobe, en millièmes d'em. Ce
//! sont des mesures, pas du code : les retranscrire est légitime, et le test
//! `metriques_conformes_aux_polices_compatibles` les recoupe avec les polices
//! du système dessinées pour leur être métriquement compatibles (Arial pour
//! Helvetica, Times New Roman pour Times, Courier New pour Courier, Symbol
//! pour Symbol).
//!
//! # Forme des tables
//!
//! - codes 32 à 126 : une table de 95 largeurs par famille, indexée par le
//!   code **WinAnsi** (qui coïncide avec l'ASCII sur cette plage) ;
//! - au-delà : [`LATIN_EXTRA`], par nom de glyphe AFM ;
//! - composés accentués : leur avance est **exactement** celle de la lettre de
//!   base (`eacute` = `e`, `Ccedilla` = `C`…), parce que ces polices les
//!   construisent par superposition ; le « i » accentué fait exception et se
//!   bâtit sur `dotlessi`, plus large ; les autres exceptions sont listées ;
//! - Courier : chasse fixe, 600 partout ;
//! - Symbol : table propre, [`SYMBOL`] ;
//! - ZapfDingbats : **lacune connue**, voir plus bas.
//!
//! # Ce qui manque
//!
//! Les largeurs de ZapfDingbats ne sont pas ici. Les inventer serait pire que
//! l'absence : [`Standard::width_by_name`] rend `None`, et l'appelant sait
//! qu'il doit se rabattre ailleurs. En pratique ce cas ne se rencontre presque
//! jamais, les producteurs écrivant toujours `/Widths` pour cette police.

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Une des quatorze polices standard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, PartialOrd, Ord)]
pub enum Standard {
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
    /// Symbol.
    Symbol,
    /// ZapfDingbats.
    ZapfDingbats,
}

/// Grandeurs d'un descripteur de police, en millièmes d'em.
///
/// Elles servent quand le document ne fournit pas de `/FontDescriptor`, ce que
/// §9.6.2.2 autorise pour ces quatorze polices.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Hauteur de hampe (`Ascender`).
    pub ascent: f64,
    /// Profondeur de jambage (`Descender`), négative.
    pub descent: f64,
    /// Hauteur de capitale (`CapHeight`).
    pub cap_height: f64,
    /// Hauteur d'œil (`XHeight`).
    pub x_height: f64,
    /// Inclinaison en degrés, négative vers la droite.
    pub italic_angle: f64,
    /// Épaisseur de fût vertical.
    pub stem_v: f64,
    /// Boîte englobante de la police : `[x0, y0, x1, y1]`.
    pub bbox: [f64; 4],
    /// Drapeaux du descripteur (§9.8.2, table 123).
    pub flags: u32,
}

/// Famille, qui décide de la table de largeurs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Helvetica,
    Times,
    Courier,
    Symbol,
    ZapfDingbats,
}

impl Standard {
    /// Nom `/BaseFont` de la police.
    #[must_use]
    pub fn base_font(self) -> &'static str {
        match self {
            Standard::Helvetica => "Helvetica",
            Standard::HelveticaBold => "Helvetica-Bold",
            Standard::HelveticaOblique => "Helvetica-Oblique",
            Standard::HelveticaBoldOblique => "Helvetica-BoldOblique",
            Standard::TimesRoman => "Times-Roman",
            Standard::TimesBold => "Times-Bold",
            Standard::TimesItalic => "Times-Italic",
            Standard::TimesBoldItalic => "Times-BoldItalic",
            Standard::Courier => "Courier",
            Standard::CourierBold => "Courier-Bold",
            Standard::CourierOblique => "Courier-Oblique",
            Standard::CourierBoldOblique => "Courier-BoldOblique",
            Standard::Symbol => "Symbol",
            Standard::ZapfDingbats => "ZapfDingbats",
        }
    }

    /// Reconnaît un `/BaseFont`, y compris sous les noms que les producteurs
    /// emploient à la place (`Arial` pour Helvetica, `TimesNewRoman` pour
    /// Times, `CourierNew` pour Courier) et les préfixes de sous-ensemble
    /// `ABCDEF+`.
    ///
    /// Ces équivalences ne sont pas dans la norme : elles sont ce que font les
    /// lecteurs depuis toujours, parce que ces polices ont été dessinées pour
    /// être métriquement interchangeables. Les refuser ferait composer de
    /// travers les innombrables fichiers qui écrivent `Arial` sans incorporer
    /// quoi que ce soit.
    #[must_use]
    pub fn from_base_font(name: &str) -> Option<Standard> {
        let name = name.rsplit('+').next().unwrap_or(name);
        let mut key = String::with_capacity(name.len());
        for c in name.chars() {
            if c.is_ascii_alphanumeric() {
                key.push(c.to_ascii_lowercase());
            }
        }
        // `psmt`, `mt`, `ps` : suffixes de nommage Monotype et PostScript.
        for suffix in ["psmt", "psmc", "mt", "ps"] {
            if let Some(cut) = key.strip_suffix(suffix) {
                if !cut.is_empty() {
                    key = cut.to_string();
                    break;
                }
            }
        }
        if key.contains("zapf") || key.contains("dingbat") {
            return Some(Standard::ZapfDingbats);
        }
        if key.contains("symbol") {
            return Some(Standard::Symbol);
        }
        let bold = key.contains("bold");
        let italic = key.contains("italic") || key.contains("oblique");
        if key.contains("courier") {
            return Some(match (bold, italic) {
                (true, true) => Standard::CourierBoldOblique,
                (true, false) => Standard::CourierBold,
                (false, true) => Standard::CourierOblique,
                (false, false) => Standard::Courier,
            });
        }
        if key.contains("times") {
            return Some(match (bold, italic) {
                (true, true) => Standard::TimesBoldItalic,
                (true, false) => Standard::TimesBold,
                (false, true) => Standard::TimesItalic,
                (false, false) => Standard::TimesRoman,
            });
        }
        if key.contains("helvetica") || key.contains("arial") {
            return Some(match (bold, italic) {
                (true, true) => Standard::HelveticaBoldOblique,
                (true, false) => Standard::HelveticaBold,
                (false, true) => Standard::HelveticaOblique,
                (false, false) => Standard::Helvetica,
            });
        }
        None
    }

    /// Police d'après un nom donné par un humain — une option de ligne de
    /// commande, un champ d'interface : le nom PostScript exact
    /// (`Times-BoldItalic`), une forme libre (`times bold italic`,
    /// `helvetica-oblique`) ou une famille approchée (`mono`, `sans`,
    /// `serif`).
    ///
    /// À ne pas confondre avec [`Standard::from_base_font`], qui lit le
    /// `/BaseFont` d'un document : ici on cherche à comprendre une intention,
    /// là on identifie une police nommée. Symbol et ZapfDingbats sont écartées
    /// exprès — on ne compose pas un filigrane avec, et leur encodage n'est
    /// pas WinAnsi.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Standard> {
        let key: String = name
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect();
        let bold = key.contains("bold");
        let italic = key.contains("italic") || key.contains("oblique");
        if key.contains("courier") || key.contains("mono") {
            return Some(match (bold, italic) {
                (true, true) => Standard::CourierBoldOblique,
                (true, false) => Standard::CourierBold,
                (false, true) => Standard::CourierOblique,
                (false, false) => Standard::Courier,
            });
        }
        if key.contains("times") || key.contains("roman") || key.contains("serif") {
            return Some(match (bold, italic) {
                (true, true) => Standard::TimesBoldItalic,
                (true, false) => Standard::TimesBold,
                (false, true) => Standard::TimesItalic,
                (false, false) => Standard::TimesRoman,
            });
        }
        if key.contains("helvetica") || key.contains("arial") || key.contains("sans") {
            return Some(match (bold, italic) {
                (true, true) => Standard::HelveticaBoldOblique,
                (true, false) => Standard::HelveticaBold,
                (false, true) => Standard::HelveticaOblique,
                (false, false) => Standard::Helvetica,
            });
        }
        None
    }

    /// Hauteur de hampe, en millièmes d'em (`Ascender` du fichier AFM).
    #[must_use]
    pub fn ascent(self) -> f64 {
        self.metrics().ascent
    }

    /// Profondeur de jambage, négative, en millièmes d'em (`Descender`).
    #[must_use]
    pub fn descent(self) -> f64 {
        self.metrics().descent
    }

    /// Largeur d'un glyphe désigné par son nom AFM, en millièmes d'em.
    ///
    /// `None` quand le nom est inconnu de cette police — jamais une valeur
    /// inventée : l'appelant doit pouvoir distinguer « je sais » de « je ne
    /// sais pas ».
    #[must_use]
    pub fn width_by_name(self, glyph: &str) -> Option<f64> {
        self.width_by_name_depth(glyph, 0)
    }

    fn width_by_name_depth(self, glyph: &str, depth: u32) -> Option<f64> {
        match self.family() {
            // Chasse fixe : la question ne se pose pas.
            Family::Courier => return Some(600.0),
            Family::Symbol => {
                return SYMBOL
                    .iter()
                    .find(|(n, _)| *n == glyph)
                    .map(|(_, w)| f64::from(*w));
            }
            Family::ZapfDingbats => return None,
            Family::Helvetica | Family::Times => {}
        }
        let column = self.column();
        if let Some((_, row)) = LATIN_EXTRA.iter().find(|(n, _)| *n == glyph) {
            return Some(f64::from(row[column]));
        }
        if let Some(code) = win_ansi_code(glyph) {
            if (32..=126).contains(&code) {
                return Some(f64::from(self.table()[usize::from(code) - 32]));
            }
        }
        // Composé : l'avance est celle de la lettre de base.
        if depth == 0 {
            if let Some(base) = base_of_composite(glyph) {
                return self.width_by_name_depth(base, 1);
            }
        }
        None
    }

    /// Largeur d'un caractère Unicode, en millièmes d'em.
    ///
    /// Passe par le nom de glyphe WinAnsi ; `None` hors de ce répertoire.
    #[must_use]
    pub fn width_by_char(self, c: char) -> Option<f64> {
        if self.family() == Family::Courier {
            return Some(600.0);
        }
        let name = win_ansi_name(c)?;
        self.width_by_name(name)
    }

    /// Largeur d'un caractère, avec repli sur la largeur de `n`.
    ///
    /// Forme commode pour mesurer une chaîne que l'on s'apprête à encoder en
    /// WinAnsi : ce qui n'y tient pas sera de toute façon remplacé.
    #[must_use]
    pub fn char_width(self, c: char) -> f64 {
        self.width_by_char(c)
            .or_else(|| self.width_by_name("n"))
            .unwrap_or(500.0)
    }

    /// Largeur d'une chaîne à une taille donnée, en points.
    #[must_use]
    pub fn text_width(self, text: &str, size: f64) -> f64 {
        text.chars().map(|c| self.char_width(c)).sum::<f64>() * size / 1000.0
    }

    /// Grandeurs du descripteur.
    #[must_use]
    pub fn metrics(self) -> Metrics {
        const FIXED: u32 = 1;
        const SERIF: u32 = 1 << 1;
        const SYMBOLIC: u32 = 1 << 2;
        const NONSYMBOLIC: u32 = 1 << 5;
        const ITALIC: u32 = 1 << 6;
        let (ascent, descent, cap_height, x_height) = match self.family() {
            Family::Helvetica => (
                718.0,
                -207.0,
                718.0,
                if self.is_bold() { 532.0 } else { 523.0 },
            ),
            Family::Times => (
                683.0,
                -217.0,
                match self {
                    Standard::TimesBold => 676.0,
                    Standard::TimesItalic => 653.0,
                    Standard::TimesBoldItalic => 669.0,
                    _ => 662.0,
                },
                match self {
                    Standard::TimesBold => 461.0,
                    Standard::TimesItalic => 441.0,
                    Standard::TimesBoldItalic => 462.0,
                    _ => 450.0,
                },
            ),
            Family::Courier => (
                629.0,
                -157.0,
                562.0,
                if self.is_bold() { 439.0 } else { 426.0 },
            ),
            Family::Symbol | Family::ZapfDingbats => (0.0, 0.0, 0.0, 0.0),
        };
        let bbox = match self {
            Standard::Helvetica => [-166.0, -225.0, 1000.0, 931.0],
            Standard::HelveticaOblique => [-170.0, -225.0, 1116.0, 931.0],
            Standard::HelveticaBold => [-170.0, -228.0, 1003.0, 962.0],
            Standard::HelveticaBoldOblique => [-174.0, -228.0, 1114.0, 962.0],
            Standard::TimesRoman => [-168.0, -218.0, 1000.0, 898.0],
            Standard::TimesBold => [-168.0, -218.0, 1000.0, 935.0],
            Standard::TimesItalic => [-169.0, -217.0, 1010.0, 883.0],
            Standard::TimesBoldItalic => [-200.0, -218.0, 996.0, 921.0],
            Standard::Courier => [-23.0, -250.0, 715.0, 805.0],
            Standard::CourierOblique => [-27.0, -250.0, 849.0, 805.0],
            Standard::CourierBold => [-113.0, -250.0, 749.0, 801.0],
            Standard::CourierBoldOblique => [-57.0, -250.0, 869.0, 801.0],
            Standard::Symbol => [-180.0, -293.0, 1090.0, 1010.0],
            Standard::ZapfDingbats => [-1.0, -143.0, 981.0, 820.0],
        };
        let stem_v = match self {
            Standard::Helvetica | Standard::HelveticaOblique => 88.0,
            Standard::HelveticaBold | Standard::HelveticaBoldOblique => 140.0,
            Standard::TimesRoman => 84.0,
            Standard::TimesBold => 139.0,
            Standard::TimesItalic => 76.0,
            Standard::TimesBoldItalic => 121.0,
            Standard::Courier | Standard::CourierOblique => 51.0,
            Standard::CourierBold | Standard::CourierBoldOblique => 106.0,
            Standard::Symbol => 85.0,
            Standard::ZapfDingbats => 90.0,
        };
        let mut flags = match self.family() {
            Family::Helvetica => NONSYMBOLIC,
            Family::Times => SERIF | NONSYMBOLIC,
            Family::Courier => FIXED | SERIF | NONSYMBOLIC,
            Family::Symbol | Family::ZapfDingbats => SYMBOLIC,
        };
        if self.is_italic() {
            flags |= ITALIC;
        }
        let italic_angle = if self.is_italic() {
            match self {
                Standard::TimesItalic => -15.5,
                Standard::TimesBoldItalic => -15.0,
                _ => -12.0,
            }
        } else {
            0.0
        };
        Metrics {
            ascent,
            descent,
            cap_height,
            x_height,
            italic_angle,
            stem_v,
            bbox,
            flags,
        }
    }

    /// Vrai pour les graisses grasses.
    #[must_use]
    pub fn is_bold(self) -> bool {
        matches!(
            self,
            Standard::HelveticaBold
                | Standard::HelveticaBoldOblique
                | Standard::TimesBold
                | Standard::TimesBoldItalic
                | Standard::CourierBold
                | Standard::CourierBoldOblique
        )
    }

    /// Vrai pour les italiques et les obliques.
    #[must_use]
    pub fn is_italic(self) -> bool {
        matches!(
            self,
            Standard::HelveticaOblique
                | Standard::HelveticaBoldOblique
                | Standard::TimesItalic
                | Standard::TimesBoldItalic
                | Standard::CourierOblique
                | Standard::CourierBoldOblique
        )
    }

    /// Vrai si la police porte son propre encodage (§9.6.6.1) : pour Symbol et
    /// ZapfDingbats, appliquer StandardEncoding donne des glyphes faux.
    #[must_use]
    pub fn has_builtin_encoding(self) -> bool {
        matches!(self, Standard::Symbol | Standard::ZapfDingbats)
    }

    fn family(self) -> Family {
        match self {
            Standard::Helvetica
            | Standard::HelveticaBold
            | Standard::HelveticaOblique
            | Standard::HelveticaBoldOblique => Family::Helvetica,
            Standard::TimesRoman
            | Standard::TimesBold
            | Standard::TimesItalic
            | Standard::TimesBoldItalic => Family::Times,
            Standard::Courier
            | Standard::CourierBold
            | Standard::CourierOblique
            | Standard::CourierBoldOblique => Family::Courier,
            Standard::Symbol => Family::Symbol,
            Standard::ZapfDingbats => Family::ZapfDingbats,
        }
    }

    /// Colonne des tables latines : Helvetica, Helvetica-Bold, Times-Roman,
    /// Times-Bold, Times-Italic, Times-BoldItalic. Les obliques ont les
    /// largeurs de leurs droites.
    fn column(self) -> usize {
        match self {
            Standard::HelveticaBold | Standard::HelveticaBoldOblique => 1,
            Standard::TimesRoman => 2,
            Standard::TimesBold => 3,
            Standard::TimesItalic => 4,
            Standard::TimesBoldItalic => 5,
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

/// Lettre de base d'un nom de glyphe composé : `eacute` → `e`.
///
/// Les quatorze polices construisent leurs accentués par superposition, sans
/// toucher à l'avance ; la règle est donc exacte, pas approchée. Les rares
/// glyphes où elle serait fausse (`oslash`) sont dans [`LATIN_EXTRA`], qui est
/// consulté avant.
fn base_of_composite(glyph: &str) -> Option<&str> {
    const ACCENTS: [&str; 14] = [
        "acute",
        "grave",
        "circumflex",
        "dieresis",
        "tilde",
        "ring",
        "cedilla",
        "caron",
        "breve",
        "ogonek",
        "hungarumlaut",
        "macron",
        "dotaccent",
        "slash",
    ];
    for accent in ACCENTS {
        if let Some(base) = glyph.strip_suffix(accent) {
            if base.len() == 1 && base.as_bytes()[0].is_ascii_alphabetic() {
                // Un « i » accentué se bâtit sur le « i » sans point, qui est
                // plus large que le « i » ordinaire dans Helvetica (278 contre
                // 222) : le point ne peut pas cohabiter avec l'accent.
                return Some(if base == "i" { "dotlessi" } else { base });
            }
        }
    }
    None
}

/// Nom de glyphe WinAnsi d'un caractère, construit une fois.
fn win_ansi_name(c: char) -> Option<&'static str> {
    static TABLE: OnceLock<BTreeMap<char, &'static str>> = OnceLock::new();
    TABLE
        .get_or_init(|| {
            let mut map = BTreeMap::new();
            for code in 32u8..=255 {
                if let Some(name) = crate::encodings::win_ansi(code) {
                    if let Some(ch) = crate::encodings::glyph_name_to_unicode(name) {
                        map.entry(ch).or_insert(name);
                    }
                }
            }
            // WinAnsi n'a ni espace insécable ni trait d'union conditionnel
            // comme glyphes distincts : ils se composent comme l'espace et le
            // trait d'union (§9.6.6.4, note 2).
            map.insert('\u{00A0}', "space");
            map.insert('\u{00AD}', "hyphen");
            map
        })
        .get(&c)
        .copied()
}

/// Code WinAnsi d'un nom de glyphe, construit une fois.
fn win_ansi_code(glyph: &str) -> Option<u8> {
    static TABLE: OnceLock<BTreeMap<&'static str, u8>> = OnceLock::new();
    TABLE
        .get_or_init(|| {
            let mut map = BTreeMap::new();
            for code in 32u8..=255 {
                if let Some(name) = crate::encodings::win_ansi(code) {
                    map.entry(name).or_insert(code);
                }
            }
            map
        })
        .get(glyph)
        .copied()
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

/// Largeurs des glyphes hors codes 32 à 126, par nom AFM :
/// `[Helvetica, Helvetica-Bold, Times-Roman, Times-Bold, Times-Italic,
/// Times-BoldItalic]`.
///
/// `quoteright` et `quoteleft` y figurent bien qu'ils soient dans la plage
/// ASCII de StandardEncoding : WinAnsi met `quotesingle` et `grave` à ces
/// codes, et les deux paires n'ont pas la même avance.
const LATIN_EXTRA: [(&str, [u16; 6]); 67] = [
    ("quoteright", [222, 278, 333, 333, 333, 333]),
    ("quoteleft", [222, 278, 333, 333, 333, 333]),
    ("Euro", [556, 556, 500, 500, 500, 500]),
    ("quotesinglbase", [222, 278, 333, 333, 333, 333]),
    ("florin", [556, 556, 500, 500, 500, 500]),
    ("quotedblbase", [333, 500, 444, 500, 556, 500]),
    ("ellipsis", [1000, 1000, 1000, 1000, 889, 1000]),
    ("dagger", [556, 556, 500, 500, 500, 500]),
    ("daggerdbl", [556, 556, 500, 500, 500, 500]),
    ("circumflex", [333, 333, 333, 333, 333, 333]),
    ("perthousand", [1000, 1000, 1000, 1000, 1000, 1000]),
    ("guilsinglleft", [333, 333, 333, 333, 333, 333]),
    ("OE", [1000, 1000, 889, 1000, 944, 944]),
    ("quotedblleft", [333, 500, 444, 500, 556, 500]),
    ("quotedblright", [333, 500, 444, 500, 556, 500]),
    ("bullet", [350, 350, 350, 350, 350, 350]),
    ("endash", [556, 556, 500, 500, 500, 500]),
    ("emdash", [1000, 1000, 1000, 1000, 889, 1000]),
    ("tilde", [333, 333, 333, 333, 333, 333]),
    ("trademark", [1000, 1000, 980, 1000, 980, 1000]),
    ("guilsinglright", [333, 333, 333, 333, 333, 333]),
    ("oe", [944, 944, 722, 722, 667, 722]),
    ("exclamdown", [333, 333, 333, 333, 389, 389]),
    ("cent", [556, 556, 500, 500, 500, 500]),
    ("sterling", [556, 556, 500, 500, 500, 500]),
    ("currency", [556, 556, 500, 500, 500, 500]),
    ("yen", [556, 556, 500, 500, 500, 500]),
    ("brokenbar", [260, 280, 200, 220, 275, 220]),
    ("section", [556, 556, 500, 500, 500, 500]),
    ("dieresis", [333, 333, 333, 333, 333, 333]),
    ("copyright", [737, 737, 760, 747, 760, 747]),
    ("ordfeminine", [370, 370, 276, 300, 276, 266]),
    ("guillemotleft", [556, 556, 500, 500, 500, 500]),
    ("logicalnot", [584, 584, 564, 570, 675, 606]),
    ("registered", [737, 737, 760, 747, 760, 747]),
    ("macron", [333, 333, 333, 333, 333, 333]),
    ("degree", [400, 400, 400, 400, 400, 400]),
    ("plusminus", [584, 584, 564, 570, 675, 570]),
    ("twosuperior", [333, 333, 300, 300, 300, 300]),
    ("threesuperior", [333, 333, 300, 300, 300, 300]),
    ("acute", [333, 333, 333, 333, 333, 333]),
    ("mu", [556, 611, 500, 556, 500, 576]),
    ("paragraph", [537, 556, 453, 540, 523, 500]),
    ("periodcentered", [278, 278, 250, 250, 250, 250]),
    ("cedilla", [333, 333, 333, 333, 333, 333]),
    ("onesuperior", [333, 333, 300, 300, 300, 300]),
    ("ordmasculine", [365, 365, 310, 330, 310, 300]),
    ("guillemotright", [556, 556, 500, 500, 500, 500]),
    ("onequarter", [834, 834, 750, 750, 750, 750]),
    ("onehalf", [834, 834, 750, 750, 750, 750]),
    ("threequarters", [834, 834, 750, 750, 750, 750]),
    ("questiondown", [611, 611, 444, 500, 500, 500]),
    ("AE", [1000, 1000, 889, 1000, 889, 944]),
    ("ae", [889, 889, 667, 722, 667, 722]),
    ("multiply", [584, 584, 564, 570, 675, 570]),
    ("divide", [584, 584, 564, 570, 675, 570]),
    ("minus", [584, 584, 564, 570, 675, 606]),
    ("Eth", [722, 722, 722, 722, 722, 722]),
    ("Thorn", [667, 667, 556, 611, 611, 611]),
    ("eth", [556, 611, 500, 500, 500, 500]),
    ("thorn", [556, 611, 500, 556, 500, 500]),
    ("oslash", [611, 611, 500, 500, 500, 500]),
    ("germandbls", [611, 611, 500, 556, 500, 500]),
    ("fraction", [167, 167, 167, 167, 167, 167]),
    ("fi", [500, 611, 556, 556, 500, 556]),
    ("fl", [500, 611, 556, 556, 500, 556]),
    ("dotlessi", [278, 278, 278, 278, 278, 278]),
];

/// Largeurs Symbol (AFM Adobe), par nom de glyphe.
const SYMBOL: [(&str, u16); 190] = [
    ("space", 250),
    ("exclam", 333),
    ("universal", 713),
    ("numbersign", 500),
    ("existential", 549),
    ("percent", 833),
    ("ampersand", 778),
    ("suchthat", 439),
    ("parenleft", 333),
    ("parenright", 333),
    ("asteriskmath", 500),
    ("plus", 549),
    ("comma", 250),
    ("minus", 549),
    ("period", 250),
    ("slash", 278),
    ("zero", 500),
    ("one", 500),
    ("two", 500),
    ("three", 500),
    ("four", 500),
    ("five", 500),
    ("six", 500),
    ("seven", 500),
    ("eight", 500),
    ("nine", 500),
    ("colon", 278),
    ("semicolon", 278),
    ("less", 549),
    ("equal", 549),
    ("greater", 549),
    ("question", 444),
    ("congruent", 549),
    ("Alpha", 722),
    ("Beta", 667),
    ("Chi", 722),
    ("Delta", 612),
    ("Epsilon", 611),
    ("Phi", 763),
    ("Gamma", 603),
    ("Eta", 722),
    ("Iota", 333),
    ("theta1", 631),
    ("Kappa", 722),
    ("Lambda", 686),
    ("Mu", 889),
    ("Nu", 722),
    ("Omicron", 722),
    ("Pi", 768),
    ("Theta", 741),
    ("Rho", 556),
    ("Sigma", 592),
    ("Tau", 611),
    ("Upsilon", 690),
    ("sigma1", 439),
    ("Omega", 768),
    ("Xi", 645),
    ("Psi", 795),
    ("Zeta", 611),
    ("bracketleft", 333),
    ("therefore", 863),
    ("bracketright", 333),
    ("perpendicular", 658),
    ("underscore", 500),
    ("radicalex", 500),
    ("alpha", 631),
    ("beta", 549),
    ("chi", 549),
    ("delta", 494),
    ("epsilon", 439),
    ("phi", 521),
    ("gamma", 411),
    ("eta", 603),
    ("iota", 329),
    ("phi1", 603),
    ("kappa", 549),
    ("lambda", 549),
    ("mu", 576),
    ("nu", 521),
    ("omicron", 549),
    ("pi", 549),
    ("theta", 521),
    ("rho", 549),
    ("sigma", 603),
    ("tau", 439),
    ("upsilon", 576),
    ("omega1", 713),
    ("omega", 686),
    ("xi", 493),
    ("psi", 686),
    ("zeta", 494),
    ("braceleft", 480),
    ("bar", 200),
    ("braceright", 480),
    ("similar", 549),
    ("Euro", 750),
    ("Upsilon1", 620),
    ("minute", 247),
    ("lessequal", 549),
    ("fraction", 167),
    ("infinity", 713),
    ("florin", 500),
    ("club", 753),
    ("diamond", 753),
    ("heart", 753),
    ("spade", 753),
    ("arrowboth", 1042),
    ("arrowleft", 987),
    ("arrowup", 603),
    ("arrowright", 987),
    ("arrowdown", 603),
    ("degree", 400),
    ("plusminus", 549),
    ("second", 411),
    ("greaterequal", 549),
    ("multiply", 549),
    ("proportional", 713),
    ("partialdiff", 494),
    ("bullet", 460),
    ("divide", 549),
    ("notequal", 549),
    ("equivalence", 549),
    ("approxequal", 549),
    ("ellipsis", 1000),
    ("arrowvertex", 603),
    ("arrowhorizex", 1000),
    ("carriagereturn", 658),
    ("aleph", 823),
    ("Ifraktur", 686),
    ("Rfraktur", 795),
    ("weierstrass", 987),
    ("circlemultiply", 768),
    ("circleplus", 768),
    ("emptyset", 823),
    ("intersection", 768),
    ("union", 768),
    ("propersuperset", 713),
    ("reflexsuperset", 713),
    ("notsubset", 713),
    ("propersubset", 713),
    ("reflexsubset", 713),
    ("element", 713),
    ("notelement", 713),
    ("angle", 768),
    ("gradient", 713),
    ("registerserif", 790),
    ("copyrightserif", 790),
    ("trademarkserif", 890),
    ("product", 823),
    ("radical", 549),
    ("dotmath", 250),
    ("logicalnot", 713),
    ("logicaland", 603),
    ("logicalor", 603),
    ("arrowdblboth", 1042),
    ("arrowdblleft", 987),
    ("arrowdblup", 603),
    ("arrowdblright", 987),
    ("arrowdbldown", 603),
    ("lozenge", 494),
    ("angleleft", 329),
    ("registersans", 790),
    ("copyrightsans", 790),
    ("trademarksans", 786),
    ("summation", 713),
    ("parenlefttp", 384),
    ("parenleftex", 384),
    ("parenleftbt", 384),
    ("bracketlefttp", 384),
    ("bracketleftex", 384),
    ("bracketleftbt", 384),
    ("bracelefttp", 494),
    ("braceleftmid", 494),
    ("braceleftbt", 494),
    ("braceex", 494),
    ("angleright", 329),
    ("integral", 274),
    ("integraltp", 686),
    ("integralex", 686),
    ("integralbt", 686),
    ("parenrighttp", 384),
    ("parenrightex", 384),
    ("parenrightbt", 384),
    ("bracketrighttp", 384),
    ("bracketrightex", 384),
    ("bracketrightbt", 384),
    ("bracerighttp", 494),
    ("bracerightmid", 494),
    ("bracerightbt", 494),
    ("apple", 790),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noms_de_base_reconnus() {
        assert_eq!(
            Standard::from_base_font("Helvetica"),
            Some(Standard::Helvetica)
        );
        assert_eq!(
            Standard::from_base_font("ABCDEF+Times-BoldItalic"),
            Some(Standard::TimesBoldItalic)
        );
        assert_eq!(
            Standard::from_base_font("ArialMT"),
            Some(Standard::Helvetica)
        );
        assert_eq!(
            Standard::from_base_font("Arial-BoldMT"),
            Some(Standard::HelveticaBold)
        );
        assert_eq!(
            Standard::from_base_font("TimesNewRomanPSMT"),
            Some(Standard::TimesRoman)
        );
        assert_eq!(
            Standard::from_base_font("CourierNewPS-BoldItalicMT"),
            Some(Standard::CourierBoldOblique)
        );
        assert_eq!(
            Standard::from_base_font("ZapfDingbats"),
            Some(Standard::ZapfDingbats)
        );
        assert_eq!(Standard::from_base_font("Calibri"), None);
    }

    #[test]
    fn noms_humains() {
        assert_eq!(
            Standard::from_name("helvetica oblique"),
            Some(Standard::HelveticaOblique)
        );
        assert_eq!(Standard::from_name("COURIER"), Some(Standard::Courier));
        assert_eq!(Standard::from_name("mono"), Some(Standard::Courier));
        assert_eq!(
            Standard::from_name("sans-bold"),
            Some(Standard::HelveticaBold)
        );
        // Écartées : leur encodage n'est pas WinAnsi.
        assert_eq!(Standard::from_name("Symbol"), None);
        assert_eq!(Standard::from_name("Wingdings"), None);
    }

    #[test]
    fn largeurs_par_nom_et_par_caractere() {
        let h = Standard::Helvetica;
        assert_eq!(h.width_by_name("A"), Some(667.0));
        assert_eq!(h.width_by_name("space"), Some(278.0));
        assert_eq!(h.width_by_char('A'), Some(667.0));
        // Le composé prend l'avance de sa lettre de base.
        assert_eq!(h.width_by_name("eacute"), h.width_by_name("e"));
        assert_eq!(h.width_by_char('é'), Some(556.0));
        assert_eq!(h.width_by_name("Ccedilla"), h.width_by_name("C"));
        // Les exceptions ne suivent pas la règle.
        assert_eq!(h.width_by_name("oslash"), Some(611.0));
        assert_ne!(h.width_by_name("oslash"), h.width_by_name("o"));
        assert_eq!(h.width_by_name("germandbls"), Some(611.0));
        // Le « i » accentué se bâtit sur le « i » sans point.
        assert_eq!(h.width_by_name("igrave"), Some(278.0));
        assert_eq!(h.width_by_name("i"), Some(222.0));
        // quotesingle (WinAnsi) et quoteright (Standard) diffèrent.
        assert_eq!(h.width_by_name("quotesingle"), Some(191.0));
        assert_eq!(h.width_by_name("quoteright"), Some(222.0));
        // Courier ne connaît qu'une largeur.
        assert_eq!(Standard::Courier.width_by_name("i"), Some(600.0));
        assert_eq!(Standard::CourierBold.width_by_char('M'), Some(600.0));
        // Symbol a sa table.
        assert_eq!(Standard::Symbol.width_by_name("alpha"), Some(631.0));
        assert_eq!(Standard::Symbol.width_by_name("A"), None);
        // ZapfDingbats : lacune assumée, pas une valeur inventée.
        assert_eq!(Standard::ZapfDingbats.width_by_name("a1"), None);
        // Nom inconnu : on le dit.
        assert_eq!(h.width_by_name("uniFFFF"), None);
    }

    #[test]
    fn mesure_de_chaine() {
        let h = Standard::Helvetica;
        // « AV » : 667 + 667 millièmes à 12 pt.
        let attendu = (667.0 + 667.0) * 12.0 / 1000.0;
        assert!((h.text_width("AV", 12.0) - attendu).abs() < 1e-9);
        // Un caractère hors WinAnsi retombe sur la largeur de « n ».
        assert!((h.char_width('中') - 556.0).abs() < 1e-9);
    }

    #[test]
    fn descripteurs() {
        let m = Standard::Helvetica.metrics();
        assert!((m.ascent - 718.0).abs() < 1e-9);
        assert!((m.descent + 207.0).abs() < 1e-9);
        assert!(m.italic_angle.abs() < 1e-9);
        let i = Standard::TimesItalic.metrics();
        assert!((i.italic_angle + 15.5).abs() < 1e-9);
        assert!(i.flags & (1 << 6) != 0);
        assert!(Standard::Courier.metrics().flags & 1 != 0);
        assert!(Standard::Symbol.metrics().flags & (1 << 2) != 0);
    }
}
