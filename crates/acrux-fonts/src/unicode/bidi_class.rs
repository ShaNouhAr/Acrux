//! Classes bidirectionnelles (UAX #9 / `DerivedBidiClass.txt`).
//!
//! Table saisie à la main faute de fichier officiel sur la machine. Elle
//! couvre le latin, le grec, le cyrillique, l'arménien, l'hébreu, l'arabe,
//! le thaana, le n'ko, le samaritain, la ponctuation générale, les symboles
//! monétaires et les blocs de formes de présentation hébraïques et arabes,
//! c'est-à-dire tout ce qui est nécessaire pour mélanger du texte latin et
//! du texte de droite à gauche.
//!
//! **Non couvert finement** : les écritures indiennes, le thaï, le CJC et les
//! plans supplémentaires hors plages de droite à gauche ; ils tombent sur la
//! valeur par défaut de leur plage (`L`, ou `R`/`AL` pour les plages que la
//! norme déclare de droite à gauche par défaut).

/// Classe bidirectionnelle d'un caractère (UAX #9 tableau 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BidiClass {
    /// Gauche à droite (latin, grec…).
    L,
    /// Droite à gauche (hébreu…).
    R,
    /// Arabe (droite à gauche, influence les chiffres qui suivent).
    Al,
    /// Chiffre européen.
    En,
    /// Séparateur de chiffres européens (`+`, `-`).
    Es,
    /// Terminateur de chiffres européens (`$`, `%`, `°`).
    Et,
    /// Chiffre arabe (chiffres indo-arabes).
    An,
    /// Séparateur commun (`,`, `:`, `.`, espace insécable).
    Cs,
    /// Marque non espaçante (diacritique).
    Nsm,
    /// Neutre borné : sans effet, supprimé par la règle X9.
    Bn,
    /// Séparateur de paragraphe.
    B,
    /// Séparateur de segment (tabulation).
    S,
    /// Blanc.
    Ws,
    /// Autre neutre.
    On,
    /// Enchâssement gauche à droite (U+202A).
    Lre,
    /// Enchâssement droite à gauche (U+202B).
    Rle,
    /// Forçage gauche à droite (U+202D).
    Lro,
    /// Forçage droite à gauche (U+202E).
    Rlo,
    /// Dépilage d'un enchâssement ou d'un forçage (U+202C).
    Pdf,
    /// Isolat gauche à droite (U+2066).
    Lri,
    /// Isolat droite à gauche (U+2067).
    Rli,
    /// Isolat de première direction forte (U+2068).
    Fsi,
    /// Fin d'isolat (U+2069).
    Pdi,
}

impl BidiClass {
    /// Vrai pour les classes fortes de droite à gauche.
    #[must_use]
    pub const fn is_rtl_strong(self) -> bool {
        matches!(self, BidiClass::R | BidiClass::Al)
    }

    /// Vrai pour les initiateurs d'isolat (LRI, RLI, FSI).
    #[must_use]
    pub const fn is_isolate_initiator(self) -> bool {
        matches!(self, BidiClass::Lri | BidiClass::Rli | BidiClass::Fsi)
    }

    /// Vrai pour les caractères retirés par la règle X9 (enchâssements,
    /// forçages, dépilages et neutres bornés).
    #[must_use]
    pub const fn is_removed_by_x9(self) -> bool {
        matches!(
            self,
            BidiClass::Rle
                | BidiClass::Lre
                | BidiClass::Rlo
                | BidiClass::Lro
                | BidiClass::Pdf
                | BidiClass::Bn
        )
    }

    /// Vrai pour les neutres et isolats traités ensemble par les règles N.
    #[must_use]
    pub const fn is_neutral_or_isolate(self) -> bool {
        matches!(
            self,
            BidiClass::B
                | BidiClass::S
                | BidiClass::Ws
                | BidiClass::On
                | BidiClass::Fsi
                | BidiClass::Lri
                | BidiClass::Rli
                | BidiClass::Pdi
        )
    }
}

use BidiClass::{
    Al, An, Bn, Cs, En, Es, Et, Fsi, Lre, Lri, Lro, Nsm, On, Pdf, Pdi, Rle, Rli, Rlo, Ws, B, L, R,
    S,
};

/// Table triée `(début, fin, classe)`, bornes incluses.
const CLASSES: &[(u32, u32, BidiClass)] = &[
    (0x0000, 0x0008, Bn),
    (0x0009, 0x0009, S),
    (0x000A, 0x000A, B),
    (0x000B, 0x000B, S),
    (0x000C, 0x000C, Ws),
    (0x000D, 0x000D, B),
    (0x000E, 0x001B, Bn),
    (0x001C, 0x001E, B),
    (0x001F, 0x001F, S),
    (0x0020, 0x0020, Ws),
    (0x0021, 0x0022, On),
    (0x0023, 0x0025, Et),
    (0x0026, 0x002A, On),
    (0x002B, 0x002B, Es),
    (0x002C, 0x002C, Cs),
    (0x002D, 0x002D, Es),
    (0x002E, 0x002F, Cs),
    (0x0030, 0x0039, En),
    (0x003A, 0x003A, Cs),
    (0x003B, 0x0040, On),
    (0x0041, 0x005A, L),
    (0x005B, 0x0060, On),
    (0x0061, 0x007A, L),
    (0x007B, 0x007E, On),
    (0x007F, 0x0084, Bn),
    (0x0085, 0x0085, B),
    (0x0086, 0x009F, Bn),
    (0x00A0, 0x00A0, Cs),
    (0x00A1, 0x00A1, On),
    (0x00A2, 0x00A5, Et),
    (0x00A6, 0x00A9, On),
    (0x00AA, 0x00AA, L),
    (0x00AB, 0x00AC, On),
    (0x00AD, 0x00AD, Bn),
    (0x00AE, 0x00AF, On),
    (0x00B0, 0x00B1, Et),
    (0x00B2, 0x00B3, En),
    (0x00B4, 0x00B4, On),
    (0x00B5, 0x00B5, L),
    (0x00B6, 0x00B8, On),
    (0x00B9, 0x00B9, En),
    (0x00BA, 0x00BA, L),
    (0x00BB, 0x00BF, On),
    (0x00C0, 0x00D6, L),
    (0x00D7, 0x00D7, On),
    (0x00D8, 0x00F6, L),
    (0x00F7, 0x00F7, On),
    (0x00F8, 0x02B8, L),
    (0x02B9, 0x02BA, On),
    (0x02BB, 0x02C1, L),
    (0x02C2, 0x02CF, On),
    (0x02D0, 0x02D1, L),
    (0x02D2, 0x02DF, On),
    (0x02E0, 0x02E4, L),
    (0x02E5, 0x02FF, On),
    (0x0300, 0x036F, Nsm),
    (0x0370, 0x0373, L),
    (0x0374, 0x0375, On),
    (0x0376, 0x037D, L),
    (0x037E, 0x037E, On),
    (0x037F, 0x0383, L),
    (0x0384, 0x0385, On),
    (0x0386, 0x0386, L),
    (0x0387, 0x0387, On),
    (0x0388, 0x0482, L),
    (0x0483, 0x0489, Nsm),
    (0x048A, 0x0589, L),
    (0x058A, 0x058A, On),
    (0x058D, 0x058E, On),
    (0x058F, 0x058F, Et),
    (0x0591, 0x05BD, Nsm),
    (0x05BE, 0x05BE, R),
    (0x05BF, 0x05BF, Nsm),
    (0x05C0, 0x05C0, R),
    (0x05C1, 0x05C2, Nsm),
    (0x05C3, 0x05C3, R),
    (0x05C4, 0x05C5, Nsm),
    (0x05C6, 0x05C6, R),
    (0x05C7, 0x05C7, Nsm),
    (0x05C8, 0x05FF, R), // défaut R du bloc hébreu
    (0x0600, 0x0605, An),
    (0x0606, 0x0607, On),
    (0x0608, 0x0608, Al),
    (0x0609, 0x060A, Et),
    (0x060B, 0x060B, Al),
    (0x060C, 0x060C, Cs),
    (0x060D, 0x060D, Al),
    (0x060E, 0x060F, On),
    (0x0610, 0x061A, Nsm),
    (0x061B, 0x064A, Al),
    (0x064B, 0x065F, Nsm),
    (0x0660, 0x0669, An),
    (0x066A, 0x066A, Et),
    (0x066B, 0x066C, An),
    (0x066D, 0x066F, Al),
    (0x0670, 0x0670, Nsm),
    (0x0671, 0x06D5, Al),
    (0x06D6, 0x06DC, Nsm),
    (0x06DD, 0x06DD, An),
    (0x06DE, 0x06DE, On),
    (0x06DF, 0x06E4, Nsm),
    (0x06E5, 0x06E6, Al),
    (0x06E7, 0x06E8, Nsm),
    (0x06E9, 0x06E9, On),
    (0x06EA, 0x06ED, Nsm),
    (0x06EE, 0x06EF, Al),
    (0x06F0, 0x06F9, En),
    (0x06FA, 0x070D, Al),
    (0x070F, 0x0710, Al),
    (0x0711, 0x0711, Nsm),
    (0x0712, 0x072F, Al),
    (0x0730, 0x074A, Nsm),
    (0x074B, 0x07A5, Al),
    (0x07A6, 0x07B0, Nsm),
    (0x07B1, 0x07BF, Al),
    (0x07C0, 0x07EA, R),
    (0x07EB, 0x07F3, Nsm),
    (0x07F4, 0x07F5, R),
    (0x07F6, 0x07F9, On),
    (0x07FA, 0x0815, R),
    (0x0816, 0x0819, Nsm),
    (0x081A, 0x081A, R),
    (0x081B, 0x0823, Nsm),
    (0x0824, 0x0824, R),
    (0x0825, 0x0827, Nsm),
    (0x0828, 0x0828, R),
    (0x0829, 0x082D, Nsm),
    (0x082E, 0x0858, R),
    (0x0859, 0x085B, Nsm),
    (0x085C, 0x0897, R),
    (0x0898, 0x089F, Nsm),
    (0x08A0, 0x08C9, Al),
    (0x08CA, 0x08E1, Nsm),
    (0x08E2, 0x08E2, An),
    (0x08E3, 0x0902, Nsm),
    (0x0903, 0x1FFF, L), // écritures indiennes, thaï, grec étendu : défaut L
    (0x2000, 0x200A, Ws),
    (0x200B, 0x200D, Bn),
    (0x200E, 0x200E, L),
    (0x200F, 0x200F, R),
    (0x2010, 0x2027, On),
    (0x2028, 0x2028, Ws),
    (0x2029, 0x2029, B),
    (0x202A, 0x202A, Lre),
    (0x202B, 0x202B, Rle),
    (0x202C, 0x202C, Pdf),
    (0x202D, 0x202D, Lro),
    (0x202E, 0x202E, Rlo),
    (0x202F, 0x202F, Cs),
    (0x2030, 0x2034, Et),
    (0x2035, 0x2043, On),
    (0x2044, 0x2044, Cs),
    (0x2045, 0x205E, On),
    (0x205F, 0x205F, Ws),
    (0x2060, 0x2064, Bn),
    (0x2066, 0x2066, Lri),
    (0x2067, 0x2067, Rli),
    (0x2068, 0x2068, Fsi),
    (0x2069, 0x2069, Pdi),
    (0x206A, 0x206F, Bn),
    (0x2070, 0x2070, En),
    (0x2071, 0x2073, L),
    (0x2074, 0x2079, En),
    (0x207A, 0x207B, Es),
    (0x207C, 0x207F, On),
    (0x2080, 0x2089, En),
    (0x208A, 0x208B, Es),
    (0x208C, 0x209F, On),
    (0x20A0, 0x20CF, Et),
    (0x20D0, 0x20F0, Nsm),
    (0x20F1, 0x2101, On),
    (0x2102, 0x2102, L),
    (0x2103, 0x2106, On),
    (0x2107, 0x2107, L),
    (0x2108, 0x2109, On),
    (0x210A, 0x2113, L),
    (0x2114, 0x2114, On),
    (0x2115, 0x2115, L),
    (0x2116, 0x2118, On),
    (0x2119, 0x211D, L),
    (0x211E, 0x2123, On),
    (0x2124, 0x2124, L),
    (0x2125, 0x2125, On),
    (0x2126, 0x2126, L),
    (0x2127, 0x2127, On),
    (0x2128, 0x2128, L),
    (0x2129, 0x2129, On),
    (0x212A, 0x212D, L),
    (0x212E, 0x212E, Et),
    (0x212F, 0x2139, L),
    (0x213A, 0x213B, On),
    (0x213C, 0x213F, L),
    (0x2140, 0x2144, On),
    (0x2145, 0x2149, L),
    (0x214A, 0x214D, On),
    (0x214E, 0x214F, L),
    (0x2150, 0x215F, On),
    (0x2160, 0x2188, L),
    (0x2189, 0x2211, On),
    (0x2212, 0x2212, Es),
    (0x2213, 0x2213, Et),
    (0x2214, 0x2335, On),
    (0x2336, 0x237A, L),
    (0x237B, 0x2394, On),
    (0x2395, 0x2395, L),
    (0x2396, 0x2487, On),
    (0x2488, 0x249B, En),
    (0x249C, 0x24E9, L),
    (0x24EA, 0x2BFF, On),
    (0x2C00, 0x2FFB, L),
    (0x2FFC, 0x3004, On),
    (0x3005, 0x3007, L),
    (0x3008, 0x3020, On),
    (0x3021, 0x302F, L),
    (0x3030, 0x3030, On),
    (0x3031, 0xD7FF, L), // kana, hangul, idéogrammes
    (0xE000, 0xFB1C, L),
    (0xFB1D, 0xFB1D, R),
    (0xFB1E, 0xFB1E, Nsm),
    (0xFB1F, 0xFB4F, R),
    (0xFB50, 0xFDCF, Al),
    (0xFDF0, 0xFDFF, Al),
    (0xFE00, 0xFE0F, Nsm),
    (0xFE10, 0xFE19, On),
    (0xFE20, 0xFE2F, Nsm),
    (0xFE30, 0xFE4F, On),
    (0xFE50, 0xFE50, Cs),
    (0xFE51, 0xFE51, On),
    (0xFE52, 0xFE52, Cs),
    (0xFE53, 0xFE54, On),
    (0xFE55, 0xFE55, Cs),
    (0xFE56, 0xFE5F, On),
    (0xFE60, 0xFE68, On),
    (0xFE69, 0xFE6A, Et),
    (0xFE6B, 0xFE6F, On),
    (0xFE70, 0xFEFC, Al),
    (0xFEFF, 0xFEFF, Bn),
    (0xFF01, 0xFF02, On),
    (0xFF03, 0xFF05, Et),
    (0xFF06, 0xFF0A, On),
    (0xFF0B, 0xFF0B, Es),
    (0xFF0C, 0xFF0C, Cs),
    (0xFF0D, 0xFF0D, Es),
    (0xFF0E, 0xFF0F, Cs),
    (0xFF10, 0xFF19, En),
    (0xFF1A, 0xFF1A, Cs),
    (0xFF1B, 0xFF20, On),
    (0xFF21, 0xFF3A, L),
    (0xFF3B, 0xFF40, On),
    (0xFF41, 0xFF5A, L),
    (0xFF5B, 0xFF65, On),
    (0xFF66, 0xFFDF, L),
    (0xFFE0, 0xFFE1, Et),
    (0xFFE2, 0xFFE4, On),
    (0xFFE5, 0xFFE6, Et),
    (0xFFE7, 0xFFFD, On),
    // Plan 1 : écritures de droite à gauche historiques et sémitiques.
    (0x1_0800, 0x1_0FFF, R),
    (0x1_E800, 0x1_EC6F, R),
    (0x1_EC70, 0x1_ECBF, Al),
    (0x1_ECC0, 0x1_ECFF, Al),
    (0x1_ED00, 0x1_EDFF, Al),
    (0x1_EE00, 0x1_EEFF, Al),
];

/// Classe bidirectionnelle d'un caractère.
///
/// Les caractères absents de la table prennent la valeur `L`, conformément à
/// la valeur par défaut de `DerivedBidiClass.txt` hors plages déclarées de
/// droite à gauche (qui, elles, figurent dans la table).
#[must_use]
pub fn bidi_class(c: char) -> BidiClass {
    let code = u32::from(c);
    let mut lo = 0usize;
    let mut hi = CLASSES.len();
    while lo < hi {
        let mid = lo.midpoint(hi);
        let (start, end, class) = CLASSES[mid];
        if code < start {
            hi = mid;
        } else if code > end {
            lo = mid + 1;
        } else {
            return class;
        }
    }
    BidiClass::L
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted() {
        let mut previous_end = None;
        for &(start, end, _) in CLASSES {
            assert!(start <= end, "plage inversée à U+{start:04X}");
            if let Some(prev) = previous_end {
                assert!(start > prev, "recouvrement à U+{start:04X}");
            }
            previous_end = Some(end);
        }
    }

    #[test]
    fn well_known_classes() {
        assert_eq!(bidi_class('A'), BidiClass::L);
        assert_eq!(bidi_class('é'), BidiClass::L);
        assert_eq!(bidi_class('1'), BidiClass::En);
        assert_eq!(bidi_class('א'), BidiClass::R);
        assert_eq!(bidi_class('ب'), BidiClass::Al);
        assert_eq!(bidi_class('\u{0660}'), BidiClass::An);
        assert_eq!(bidi_class(' '), BidiClass::Ws);
        assert_eq!(bidi_class(','), BidiClass::Cs);
        assert_eq!(bidi_class('\u{0301}'), BidiClass::Nsm);
        assert_eq!(bidi_class('\u{202B}'), BidiClass::Rle);
        assert_eq!(bidi_class('\u{2069}'), BidiClass::Pdi);
        assert_eq!(bidi_class('漢'), BidiClass::L);
    }

    #[test]
    fn helpers() {
        assert!(BidiClass::Al.is_rtl_strong());
        assert!(!BidiClass::En.is_rtl_strong());
        assert!(BidiClass::Rli.is_isolate_initiator());
        assert!(BidiClass::Bn.is_removed_by_x9());
        assert!(BidiClass::Ws.is_neutral_or_isolate());
    }
}
