//! Classes combinatoires canoniques (UAX #15, champ 3 de `UnicodeData.txt`).
//!
//! Le shaping s'en sert pour deux choses :
//! - trier les marques d'une même grappe dans l'ordre canonique avant de
//!   chercher une substitution `ccmp` ou un ancrage `mark` (OpenType suppose
//!   l'ordre canonique) ;
//! - savoir quelles marques s'empilent verticalement (classes 230 au-dessus,
//!   220 en dessous) quand la police n'a pas d'ancrage.
//!
//! ## Couverture
//!
//! Les valeurs sont saisies à la main depuis `UnicodeData.txt` (aucun fichier
//! officiel n'étant présent sur la machine de développement) pour les blocs
//! qui nous concernent : diacritiques latins et grecs (U+0300–U+036F),
//! cyrilliques (U+0483–U+0489), hébreux (U+0591–U+05C7), arabes
//! (U+0610–U+061A, U+064B–U+065F, U+0670, U+06D6–U+06ED), signes combinants
//! pour symboles (U+20D0–U+20F0) et demi-marques (U+FE20–U+FE2F).
//!
//! **Non couvert** : les écritures indiennes, le thaï, le khmer, le tibétain,
//! le birman et les plans supplémentaires, qui renvoient 0. Ces écritures
//! demandent de toute façon un moteur syllabique que nous n'écrivons pas
//! (voir `spec-notes/opentype-gsub-gpos.md`).

/// Table triée `(début, fin, classe)`, bornes incluses.
///
/// L'ordre est strictement croissant et les plages ne se recouvrent pas, ce
/// que le test `table_is_sorted` vérifie.
const CCC: &[(u32, u32, u8)] = &[
    // Diacritiques combinants (U+0300–U+036F).
    (0x0300, 0x0314, 230),
    (0x0315, 0x0315, 232),
    (0x0316, 0x0319, 220),
    (0x031A, 0x031A, 232),
    (0x031B, 0x031B, 216),
    (0x031C, 0x0320, 220),
    (0x0321, 0x0322, 202),
    (0x0323, 0x0326, 220),
    (0x0327, 0x0328, 202),
    (0x0329, 0x0333, 220),
    (0x0334, 0x0338, 1),
    (0x0339, 0x033C, 220),
    (0x033D, 0x0344, 230),
    (0x0345, 0x0345, 240),
    (0x0346, 0x0346, 230),
    (0x0347, 0x0349, 220),
    (0x034A, 0x034C, 230),
    (0x034D, 0x034E, 220),
    // U+034F COMBINING GRAPHEME JOINER : classe 0, volontairement absent.
    (0x0350, 0x0352, 230),
    (0x0353, 0x0356, 220),
    (0x0357, 0x0357, 230),
    (0x0358, 0x0358, 232),
    (0x0359, 0x035A, 220),
    (0x035B, 0x035B, 230),
    (0x035C, 0x035C, 233),
    (0x035D, 0x035E, 234),
    (0x035F, 0x035F, 233),
    (0x0360, 0x0361, 234),
    (0x0362, 0x0362, 233),
    (0x0363, 0x036F, 230),
    // Cyrillique.
    (0x0483, 0x0487, 230),
    // Hébreu : accents de cantillation puis voyelles (classes 10 à 25).
    (0x0591, 0x0591, 220),
    (0x0592, 0x0595, 230),
    (0x0596, 0x0596, 220),
    (0x0597, 0x0599, 230),
    (0x059A, 0x059A, 222),
    (0x059B, 0x059B, 220),
    (0x059C, 0x05A1, 230),
    (0x05A2, 0x05A7, 220),
    (0x05A8, 0x05A9, 230),
    (0x05AA, 0x05AA, 220),
    (0x05AB, 0x05AC, 230),
    (0x05AD, 0x05AD, 222),
    (0x05AE, 0x05AE, 228),
    (0x05AF, 0x05AF, 230),
    (0x05B0, 0x05B0, 10),
    (0x05B1, 0x05B1, 11),
    (0x05B2, 0x05B2, 12),
    (0x05B3, 0x05B3, 13),
    (0x05B4, 0x05B4, 14),
    (0x05B5, 0x05B5, 15),
    (0x05B6, 0x05B6, 16),
    (0x05B7, 0x05B7, 17),
    (0x05B8, 0x05B8, 18),
    (0x05B9, 0x05BA, 19),
    (0x05BB, 0x05BB, 20),
    (0x05BC, 0x05BC, 21),
    (0x05BD, 0x05BD, 22),
    (0x05BF, 0x05BF, 23),
    (0x05C1, 0x05C1, 24),
    (0x05C2, 0x05C2, 25),
    (0x05C4, 0x05C4, 230),
    (0x05C5, 0x05C5, 220),
    (0x05C7, 0x05C7, 18),
    // Arabe : marques hautes et basses, puis voyelles (classes 27 à 35).
    (0x0610, 0x0617, 230),
    (0x0618, 0x0618, 30),
    (0x0619, 0x0619, 31),
    (0x061A, 0x061A, 32),
    (0x064B, 0x064B, 27),
    (0x064C, 0x064C, 28),
    (0x064D, 0x064D, 29),
    (0x064E, 0x064E, 30),
    (0x064F, 0x064F, 31),
    (0x0650, 0x0650, 32),
    (0x0651, 0x0651, 33),
    (0x0652, 0x0652, 34),
    (0x0653, 0x0654, 230),
    (0x0655, 0x0656, 220),
    (0x0657, 0x065B, 230),
    (0x065C, 0x065C, 220),
    (0x065D, 0x065E, 230),
    (0x065F, 0x065F, 220),
    (0x0670, 0x0670, 35),
    (0x06D6, 0x06DC, 230),
    (0x06DF, 0x06E2, 230),
    (0x06E3, 0x06E3, 220),
    (0x06E4, 0x06E4, 230),
    (0x06E7, 0x06E8, 230),
    (0x06EA, 0x06EA, 220),
    (0x06EB, 0x06EC, 230),
    (0x06ED, 0x06ED, 220),
    // Signes combinants pour symboles.
    (0x20D0, 0x20D1, 230),
    (0x20D2, 0x20D3, 1),
    (0x20D4, 0x20D7, 230),
    (0x20D8, 0x20DA, 1),
    (0x20DB, 0x20DC, 230),
    (0x20E1, 0x20E1, 230),
    (0x20E5, 0x20E6, 1),
    (0x20E7, 0x20E7, 230),
    (0x20E8, 0x20E8, 220),
    (0x20E9, 0x20E9, 230),
    (0x20EA, 0x20EB, 1),
    (0x20EC, 0x20EF, 220),
    (0x20F0, 0x20F0, 230),
    // Demi-marques combinantes.
    (0xFE20, 0xFE2F, 230),
];

/// Classe combinatoire canonique d'un caractère (0 si inconnue ou non
/// combinante).
///
/// Une classe 0 signifie « caractère de base » pour l'ordre canonique ; elle
/// est aussi renvoyée pour les écritures que nous ne couvrons pas, ce qui est
/// sans effet puisque nous ne réordonnons alors rien.
#[must_use]
pub fn combining_class(c: char) -> u8 {
    let code = u32::from(c);
    let mut lo = 0usize;
    let mut hi = CCC.len();
    while lo < hi {
        let mid = lo.midpoint(hi);
        let (start, end, class) = CCC[mid];
        if code < start {
            hi = mid;
        } else if code > end {
            lo = mid + 1;
        } else {
            return class;
        }
    }
    0
}

/// Vrai si le caractère est une marque combinante connue (classe non nulle,
/// ou marque de classe 0 reconnue comme telle par la table de jointure).
#[must_use]
pub fn is_combining_mark(c: char) -> bool {
    combining_class(c) != 0 || crate::unicode::joining::is_transparent(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La recherche dichotomique suppose une table triée sans recouvrement.
    #[test]
    fn table_is_sorted() {
        let mut previous_end = 0u32;
        for &(start, end, _) in CCC {
            assert!(start <= end, "plage inversée à U+{start:04X}");
            assert!(
                previous_end == 0 || start > previous_end,
                "recouvrement à U+{start:04X}"
            );
            previous_end = end;
        }
    }

    #[test]
    fn known_classes() {
        assert_eq!(combining_class('\u{0301}'), 230); // accent aigu combinant
        assert_eq!(combining_class('\u{0327}'), 202); // cédille
        assert_eq!(combining_class('\u{0334}'), 1); // tilde couvrant
        assert_eq!(combining_class('\u{064E}'), 30); // fatha
        assert_eq!(combining_class('\u{0651}'), 33); // shadda
        assert_eq!(combining_class('\u{05B4}'), 14); // hiriq
        assert_eq!(combining_class('A'), 0);
        assert_eq!(combining_class('\u{034F}'), 0); // CGJ
        assert_eq!(combining_class('\u{0E38}'), 0); // thaï : non couvert
    }

    #[test]
    fn marks_are_detected() {
        assert!(is_combining_mark('\u{0301}'));
        assert!(is_combining_mark('\u{064B}'));
        assert!(!is_combining_mark('a'));
    }
}
