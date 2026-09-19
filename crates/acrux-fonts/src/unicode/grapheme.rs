//! Grappes de graphèmes étendues (UAX #29, « Extended Grapheme Cluster »).
//!
//! Une grappe est ce que l'utilisateur perçoit comme **un caractère** : le
//! curseur ne s'arrête jamais à l'intérieur, la sélection ne la coupe pas,
//! et le shaper lui attribue un seul numéro de grappe (`cluster`). C'est ce
//! qui permet de placer un curseur entre « e » et « ́ » — ou plutôt, de ne
//! pas le placer là.
//!
//! ## Règles implémentées
//!
//! GB1 à GB5 (début, fin, CR LF, contrôles), GB6 à GB8 (syllabes hangûl),
//! GB9 (`Extend` et `ZWJ`), GB9a (`SpacingMark`), GB9b (`Prepend`), GB11
//! (suite emoji `\p{Extended_Pictographic} Extend* ZWJ ×
//! \p{Extended_Pictographic}`), GB12 et GB13 (paires d'indicateurs
//! régionaux), GB999.
//!
//! ## Couverture des propriétés
//!
//! Les tables sont saisies à la main (aucun fichier `GraphemeBreakProperty.txt`
//! sur la machine). `Extend` couvre les marques combinantes du latin, du
//! grec, du cyrillique, de l'hébreu, de l'arabe, du thaana, du n'ko, du
//! syriaque, les sélecteurs de variante et les marques indiennes et thaïes
//! les plus courantes. `SpacingMark` et `Prepend` ne couvrent que les
//! valeurs les plus fréquentes des écritures indiennes.
//!
//! **Limite honnête** : pour une écriture indienne ou khmère, le découpage
//! en grappes reste approximatif ; il suffit à ne pas couper une marque de
//! sa base, pas à reproduire exactement les grappes « héritées » d'UAX #29.

/// Propriété de coupure de grappe d'un caractère (UAX #29 tableau 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Break {
    Other,
    Cr,
    Lf,
    Control,
    Extend,
    Zwj,
    RegionalIndicator,
    Prepend,
    SpacingMark,
    HangulL,
    HangulV,
    HangulT,
    HangulLv,
    HangulLvt,
    Pictographic,
}

/// Vrai si le caractère est une marque combinante au sens d'`Extend`.
#[allow(clippy::too_many_lines)] // table de données, une plage par ligne
fn is_extend(code: u32) -> bool {
    const RANGES: &[(u32, u32)] = &[
        (0x0300, 0x036F),
        (0x0483, 0x0489),
        (0x0591, 0x05BD),
        (0x05BF, 0x05BF),
        (0x05C1, 0x05C2),
        (0x05C4, 0x05C5),
        (0x05C7, 0x05C7),
        (0x0610, 0x061A),
        (0x064B, 0x065F),
        (0x0670, 0x0670),
        (0x06D6, 0x06DC),
        (0x06DF, 0x06E4),
        (0x06E7, 0x06E8),
        (0x06EA, 0x06ED),
        (0x0711, 0x0711),
        (0x0730, 0x074A),
        (0x07A6, 0x07B0),
        (0x07EB, 0x07F3),
        (0x0816, 0x0819),
        (0x081B, 0x0823),
        (0x0825, 0x0827),
        (0x0829, 0x082D),
        (0x0859, 0x085B),
        (0x0898, 0x089F),
        (0x08CA, 0x08E1),
        (0x08E3, 0x0902),
        (0x093A, 0x093A),
        (0x093C, 0x093C),
        (0x0941, 0x0948),
        (0x094D, 0x094D),
        (0x0951, 0x0957),
        (0x0962, 0x0963),
        (0x0981, 0x0981),
        (0x09BC, 0x09BC),
        (0x09C1, 0x09C4),
        (0x09CD, 0x09CD),
        (0x09E2, 0x09E3),
        (0x0A01, 0x0A02),
        (0x0A3C, 0x0A3C),
        (0x0A41, 0x0A51),
        (0x0A70, 0x0A71),
        (0x0A81, 0x0A82),
        (0x0ABC, 0x0ABC),
        (0x0AC1, 0x0ACD),
        (0x0B01, 0x0B01),
        (0x0B3C, 0x0B3C),
        (0x0B3F, 0x0B3F),
        (0x0B41, 0x0B44),
        (0x0B4D, 0x0B56),
        (0x0B82, 0x0B82),
        (0x0BC0, 0x0BC0),
        (0x0BCD, 0x0BCD),
        (0x0C00, 0x0C00),
        (0x0C3E, 0x0C40),
        (0x0C46, 0x0C56),
        (0x0C81, 0x0C81),
        (0x0CBC, 0x0CBC),
        (0x0CCC, 0x0CCD),
        (0x0D00, 0x0D01),
        (0x0D41, 0x0D44),
        (0x0D4D, 0x0D4D),
        (0x0DCA, 0x0DCA),
        (0x0DD2, 0x0DD6),
        (0x0E31, 0x0E31),
        (0x0E34, 0x0E3A),
        (0x0E47, 0x0E4E),
        (0x0EB1, 0x0EB1),
        (0x0EB4, 0x0EBC),
        (0x0EC8, 0x0ECD),
        (0x0F18, 0x0F19),
        (0x0F35, 0x0F35),
        (0x0F37, 0x0F37),
        (0x0F39, 0x0F39),
        (0x0F71, 0x0F7E),
        (0x0F80, 0x0F84),
        (0x0F86, 0x0F87),
        (0x0F8D, 0x0FBC),
        (0x102D, 0x1030),
        (0x1032, 0x1037),
        (0x1039, 0x103A),
        (0x1058, 0x1059),
        (0x135D, 0x135F),
        (0x1712, 0x1714),
        (0x17B4, 0x17B5),
        (0x17B7, 0x17BD),
        (0x17C6, 0x17C6),
        (0x17C9, 0x17D3),
        (0x180B, 0x180D),
        (0x18A9, 0x18A9),
        (0x1A17, 0x1A18),
        (0x1AB0, 0x1AFF),
        (0x1B00, 0x1B03),
        (0x1B34, 0x1B34),
        (0x1B36, 0x1B3A),
        (0x1B6B, 0x1B73),
        (0x1BE6, 0x1BE6),
        (0x1C2C, 0x1C33),
        (0x1CD0, 0x1CE0),
        (0x1CED, 0x1CED),
        (0x1DC0, 0x1DFF),
        (0x200C, 0x200C),
        (0x20D0, 0x20F0),
        (0x2CEF, 0x2CF1),
        (0x2D7F, 0x2D7F),
        (0x2DE0, 0x2DFF),
        (0x302A, 0x302F),
        (0x3099, 0x309A),
        (0xA66F, 0xA672),
        (0xA674, 0xA67D),
        (0xA69E, 0xA69F),
        (0xA806, 0xA806),
        (0xA8E0, 0xA8F1),
        (0xA9B3, 0xA9B3),
        (0xAAB0, 0xAAB0),
        (0xFB1E, 0xFB1E),
        (0xFE00, 0xFE0F),
        (0xFE20, 0xFE2F),
        (0x1_01FD, 0x1_01FD),
        (0x1_0AE5, 0x1_0AE6),
        (0x1_1301, 0x1_1301),
        (0x1_E000, 0x1_E02A),
        (0xE_0100, 0xE_01EF),
    ];
    RANGES.iter().any(|&(a, b)| (a..=b).contains(&code))
}

/// Vrai si le caractère est une marque espaçante fréquente (`SpacingMark`).
fn is_spacing_mark(code: u32) -> bool {
    const RANGES: &[(u32, u32)] = &[
        (0x0903, 0x0903),
        (0x093B, 0x093B),
        (0x093E, 0x0940),
        (0x0949, 0x094C),
        (0x094E, 0x094F),
        (0x0982, 0x0983),
        (0x09BE, 0x09C0),
        (0x09C7, 0x09CC),
        (0x0A03, 0x0A03),
        (0x0A3E, 0x0A40),
        (0x0A83, 0x0A83),
        (0x0ABE, 0x0AC0),
        (0x0B02, 0x0B03),
        (0x0B40, 0x0B40),
        (0x0BBE, 0x0BBF),
        (0x0BC1, 0x0BCC),
        (0x0C01, 0x0C03),
        (0x0C41, 0x0C44),
        (0x0C82, 0x0C83),
        (0x0CBE, 0x0CBE),
        (0x0CC0, 0x0CC4),
        (0x0D02, 0x0D03),
        (0x0D3E, 0x0D40),
        (0x0D46, 0x0D4C),
        (0x0D82, 0x0D83),
        (0x0DD0, 0x0DD1),
        (0x0DD8, 0x0DDF),
        (0x1031, 0x1031),
        (0x103B, 0x103C),
        (0x1056, 0x1057),
        (0x17B6, 0x17B6),
        (0x17BE, 0x17C5),
        (0x17C7, 0x17C8),
        (0x1B04, 0x1B04),
        (0x1B35, 0x1B35),
        (0x1B3B, 0x1B44),
        (0x1C24, 0x1C2B),
    ];
    RANGES.iter().any(|&(a, b)| (a..=b).contains(&code))
}

/// Vrai si le caractère s'attache à ce qui **suit** (`Prepend`).
fn is_prepend(code: u32) -> bool {
    matches!(code, 0x0600..=0x0605 | 0x06DD | 0x070F | 0x0890..=0x0891 | 0x08E2 | 0x0D4E | 0x110BD | 0x110CD)
}

/// Vrai si le caractère est un pictogramme étendu (approximation par blocs
/// emoji, suffisante pour ne pas couper une suite ZWJ).
fn is_pictographic(code: u32) -> bool {
    matches!(code,
        0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139
        | 0x2194..=0x21AA
        | 0x231A..=0x23FA
        | 0x24C2
        | 0x25AA..=0x25FE
        | 0x2600..=0x27BF
        | 0x2934..=0x2935
        | 0x2B00..=0x2BFF
        | 0x3030 | 0x303D | 0x3297 | 0x3299
        | 0x1_F000..=0x1_FAFF
        | 0x1_FC00..=0x1_FFFD)
}

/// Propriété de coupure d'un caractère.
fn break_property(c: char) -> Break {
    let code = u32::from(c);
    match code {
        0x000D => return Break::Cr,
        0x000A => return Break::Lf,
        0x200D => return Break::Zwj,
        0x1_F1E6..=0x1_F1FF => return Break::RegionalIndicator,
        0x1100..=0x115F => return Break::HangulL,
        0x1160..=0x11A7 => return Break::HangulV,
        0x11A8..=0x11FF => return Break::HangulT,
        0xAC00..=0xD7A3 => {
            return if (code - 0xAC00) % 28 == 0 {
                Break::HangulLv
            } else {
                Break::HangulLvt
            }
        }
        _ => {}
    }
    if is_extend(code) {
        return Break::Extend;
    }
    if is_spacing_mark(code) {
        return Break::SpacingMark;
    }
    if is_prepend(code) {
        return Break::Prepend;
    }
    if is_pictographic(code) {
        return Break::Pictographic;
    }
    // Contrôles restants : Cc, Cf et séparateurs de ligne.
    if matches!(code, 0x0000..=0x001F | 0x007F..=0x009F | 0x00AD | 0x2028 | 0x2029
        | 0x2060..=0x2064 | 0x206A..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB)
    {
        return Break::Control;
    }
    Break::Other
}

/// Vrai si une coupure de grappe existe entre `left` et `right`, sachant le
/// contexte déjà accumulé.
struct Scanner {
    /// Nombre d'indicateurs régionaux consécutifs vus avant la position
    /// courante (GB12, GB13).
    regional_run: usize,
    /// Vrai si l'on est dans une suite `Pictographic Extend*` (GB11).
    pictographic_chain: bool,
}

impl Scanner {
    const fn new() -> Self {
        Self {
            regional_run: 0,
            pictographic_chain: false,
        }
    }

    /// Met à jour l'état pour le caractère `p` qui vient d'être consommé.
    fn advance(&mut self, p: Break) {
        if p == Break::RegionalIndicator {
            self.regional_run += 1;
        } else {
            self.regional_run = 0;
        }
        self.pictographic_chain = match p {
            Break::Pictographic => true,
            Break::Extend | Break::Zwj => self.pictographic_chain,
            _ => false,
        };
    }

    /// Applique GB3 à GB999 entre `left` et `right`.
    fn breaks(&self, left: Break, right: Break) -> bool {
        // GB3 : CR × LF.
        if left == Break::Cr && right == Break::Lf {
            return false;
        }
        // GB4 : (Control | CR | LF) ÷
        if matches!(left, Break::Control | Break::Cr | Break::Lf) {
            return true;
        }
        // GB5 : ÷ (Control | CR | LF)
        if matches!(right, Break::Control | Break::Cr | Break::Lf) {
            return true;
        }
        // GB6, GB7, GB8 : syllabes hangûl.
        if hangul_joins(left, right) {
            return false;
        }
        // GB9 : × (Extend | ZWJ)
        if matches!(right, Break::Extend | Break::Zwj) {
            return false;
        }
        // GB9a : × SpacingMark
        if right == Break::SpacingMark {
            return false;
        }
        // GB9b : Prepend ×
        if left == Break::Prepend {
            return false;
        }
        // GB11 : Pictographic Extend* ZWJ × Pictographic
        if left == Break::Zwj && right == Break::Pictographic && self.pictographic_chain {
            return false;
        }
        // GB12, GB13 : les indicateurs régionaux vont par deux.
        if left == Break::RegionalIndicator
            && right == Break::RegionalIndicator
            && self.regional_run % 2 == 1
        {
            return false;
        }
        // GB999.
        true
    }
}

/// GB6 à GB8 : liaisons internes aux syllabes hangûl.
fn hangul_joins(left: Break, right: Break) -> bool {
    match left {
        Break::HangulL => matches!(
            right,
            Break::HangulL | Break::HangulV | Break::HangulLv | Break::HangulLvt
        ),
        Break::HangulLv | Break::HangulV => matches!(right, Break::HangulV | Break::HangulT),
        Break::HangulLvt | Break::HangulT => right == Break::HangulT,
        _ => false,
    }
}

/// Indices d'octets où commence chaque grappe de graphèmes, plus la
/// longueur de la chaîne en dernier élément.
///
/// Une chaîne vide donne `[0]`.
///
/// ```
/// use acrux_fonts::unicode::grapheme::cluster_boundaries;
///
/// // « e » suivi d'un accent aigu combinant : une seule grappe.
/// assert_eq!(cluster_boundaries("e\u{0301}"), vec![0, 3]);
/// assert_eq!(cluster_boundaries("ab"), vec![0, 1, 2]);
/// ```
#[must_use]
pub fn cluster_boundaries(text: &str) -> Vec<usize> {
    let mut bounds = vec![0usize];
    let mut scanner = Scanner::new();
    let mut previous: Option<Break> = None;
    for (offset, c) in text.char_indices() {
        let current = break_property(c);
        if let Some(left) = previous {
            if scanner.breaks(left, current) {
                bounds.push(offset);
            }
        }
        scanner.advance(current);
        previous = Some(current);
    }
    bounds.push(text.len());
    bounds.dedup();
    bounds
}

/// Découpe le texte en grappes (tranches de la chaîne d'origine).
#[must_use]
pub fn clusters(text: &str) -> Vec<&str> {
    let bounds = cluster_boundaries(text);
    bounds
        .windows(2)
        .filter_map(|w| text.get(w[0]..w[1]))
        .collect()
}

/// Début de la grappe qui contient l'octet `offset`.
///
/// Sert à replacer un curseur qui atterrirait au milieu d'une grappe.
#[must_use]
pub fn cluster_start(text: &str, offset: usize) -> usize {
    let bounds = cluster_boundaries(text);
    bounds
        .iter()
        .rev()
        .copied()
        .find(|&b| b <= offset)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ascii() {
        assert_eq!(clusters("abc"), vec!["a", "b", "c"]);
        assert_eq!(cluster_boundaries(""), vec![0]);
    }

    /// GB9 : une marque combinante reste collée à sa base.
    #[test]
    fn combining_mark_sticks() {
        assert_eq!(clusters("e\u{0301}f"), vec!["e\u{0301}", "f"]);
        assert_eq!(clusters("\u{0628}\u{064E}"), vec!["\u{0628}\u{064E}"]);
    }

    /// GB3 : CR LF est une seule grappe.
    #[test]
    fn crlf_is_one_cluster() {
        assert_eq!(clusters("a\r\nb"), vec!["a", "\r\n", "b"]);
    }

    /// GB6 à GB8 : une syllabe hangûl décomposée est une grappe.
    #[test]
    fn hangul_syllable() {
        assert_eq!(clusters("\u{1100}\u{1161}\u{11A8}").len(), 1);
        assert_eq!(clusters("\u{AC00}\u{11A8}").len(), 1);
    }

    /// GB12 et GB13 : les drapeaux vont par paires.
    #[test]
    fn regional_indicators_pair_up() {
        let flags = "\u{1F1EB}\u{1F1F7}\u{1F1E9}\u{1F1EA}";
        assert_eq!(clusters(flags).len(), 2);
    }

    /// GB11 : une suite emoji reliée par ZWJ ne se coupe pas.
    #[test]
    fn emoji_zwj_sequence() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(clusters(family).len(), 1);
    }

    /// Un sélecteur de variante reste dans la grappe.
    #[test]
    fn variation_selector() {
        assert_eq!(clusters("\u{2764}\u{FE0F}").len(), 1);
    }

    #[test]
    fn cluster_start_snaps_back() {
        let text = "e\u{0301}f";
        assert_eq!(cluster_start(text, 0), 0);
        assert_eq!(cluster_start(text, 1), 0);
        assert_eq!(cluster_start(text, 3), 3);
    }
}
