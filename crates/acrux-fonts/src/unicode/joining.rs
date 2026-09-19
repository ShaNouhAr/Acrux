//! Classes de jointure arabes (UAX #44, fichier `ArabicShaping.txt`).
//!
//! L'arabe s'écrit en cursive : une lettre change de forme selon qu'elle se
//! lie à la précédente, à la suivante, aux deux ou à aucune. Les quatre
//! formes portent les noms des fonctionnalités OpenType correspondantes :
//! `isol` (isolée), `init` (initiale), `medi` (médiane), `fina` (finale).
//!
//! ## Couverture
//!
//! Bloc arabe U+0600–U+06FF, supplément arabe U+0750–U+077F, arabe étendu-A
//! U+08A0–U+08FF, plus ZWJ/ZWNJ. Les valeurs sont saisies depuis
//! `ArabicShaping.txt` (aucun fichier officiel sur la machine).
//!
//! **Non couvert** : le syriaque (qui demande en plus la logique de l'alaph),
//! le mandéen, le n'ko, le mongol et les écritures du plan 1 ; leurs
//! caractères sont traités comme non joignants, donc rendus en forme isolée.

/// Type de jointure d'un caractère (`ArabicShaping.txt`, champ 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoiningType {
    /// `U` — ne se lie jamais.
    NonJoining,
    /// `L` — se lie seulement à la lettre qui suit (aucune en arabe).
    LeftJoining,
    /// `R` — se lie seulement à la lettre qui précède (alef, dal, reh, waw…).
    RightJoining,
    /// `D` — se lie des deux côtés (beh, seen, lam…).
    DualJoining,
    /// `C` — force la jointure sans occuper de forme (tatweel, ZWJ).
    JoinCausing,
    /// `T` — transparent : les marques, sautées lors du calcul des formes.
    Transparent,
}

impl JoiningType {
    /// Vrai si le caractère peut se lier à ce qui le précède (à sa droite).
    #[must_use]
    pub const fn joins_preceding(self) -> bool {
        matches!(
            self,
            JoiningType::RightJoining | JoiningType::DualJoining | JoiningType::JoinCausing
        )
    }

    /// Vrai si le caractère peut se lier à ce qui le suit (à sa gauche).
    #[must_use]
    pub const fn joins_following(self) -> bool {
        matches!(
            self,
            JoiningType::LeftJoining | JoiningType::DualJoining | JoiningType::JoinCausing
        )
    }
}

/// Forme contextuelle retenue pour une lettre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoiningForm {
    /// Aucune liaison : fonctionnalité `isol`.
    Isolated,
    /// Liée à gauche seulement : fonctionnalité `init`.
    Initial,
    /// Liée des deux côtés : fonctionnalité `medi`.
    Medial,
    /// Liée à droite seulement : fonctionnalité `fina`.
    Final,
}

impl JoiningForm {
    /// Tag OpenType de la fonctionnalité à activer pour cette forme.
    #[must_use]
    pub const fn feature(self) -> [u8; 4] {
        match self {
            JoiningForm::Isolated => *b"isol",
            JoiningForm::Initial => *b"init",
            JoiningForm::Medial => *b"medi",
            JoiningForm::Final => *b"fina",
        }
    }
}

/// Raccourcis pour la table ci-dessous.
use JoiningType::{
    DualJoining as D, JoinCausing as C, NonJoining as U, RightJoining as R, Transparent as T,
};

/// Table triée `(début, fin, type)`, bornes incluses. Tout ce qui n'y figure
/// pas est non joignant.
const JOINING: &[(u32, u32, JoiningType)] = &[
    (0x0600, 0x0605, U), // signes de nombre arabes (Cf)
    (0x0610, 0x061A, T), // marques hautes
    (0x061C, 0x061C, T), // marque arabe de direction
    (0x0620, 0x0620, D),
    (0x0621, 0x0621, U), // hamza
    (0x0622, 0x0625, R), // alef madda / hamza / waw hamza / alef hamza dessous
    (0x0626, 0x0626, D), // yeh hamza
    (0x0627, 0x0627, R), // alef
    (0x0628, 0x0628, D), // beh
    (0x0629, 0x0629, R), // teh marbuta
    (0x062A, 0x062E, D), // teh, theh, jeem, hah, khah
    (0x062F, 0x0632, R), // dal, thal, reh, zain
    (0x0633, 0x063F, D), // seen … lettres à quatre formes
    (0x0640, 0x0640, C), // tatweel
    (0x0641, 0x0647, D), // feh … heh
    (0x0648, 0x0648, R), // waw
    (0x0649, 0x064A, D), // alef maksura, yeh
    (0x064B, 0x065F, T), // voyelles et signes
    (0x0660, 0x066D, U), // chiffres et ponctuation
    (0x066E, 0x066F, D), // beh et qaf sans point
    (0x0670, 0x0670, T), // alef suscrit
    (0x0671, 0x0673, R),
    (0x0674, 0x0675, U), // hamza haute
    (0x0676, 0x0677, R),
    (0x0678, 0x0687, D),
    (0x0688, 0x0699, R),
    (0x069A, 0x06BF, D),
    (0x06C0, 0x06C0, R),
    (0x06C1, 0x06C2, D),
    (0x06C3, 0x06CB, R),
    (0x06CC, 0x06CC, D), // farsi yeh
    (0x06CD, 0x06CD, R),
    (0x06CE, 0x06CE, D),
    (0x06CF, 0x06CF, R),
    (0x06D0, 0x06D1, D),
    (0x06D2, 0x06D3, R), // yeh barree
    (0x06D4, 0x06D4, U),
    (0x06D5, 0x06D5, R),
    (0x06D6, 0x06DC, T),
    (0x06DD, 0x06DE, U),
    (0x06DF, 0x06E4, T),
    (0x06E5, 0x06E6, U),
    (0x06E7, 0x06E8, T),
    (0x06E9, 0x06E9, U),
    (0x06EA, 0x06ED, T),
    (0x06EE, 0x06EF, R),
    (0x06F0, 0x06F9, U), // chiffres persans
    (0x06FA, 0x06FC, D),
    (0x06FD, 0x06FE, U),
    (0x06FF, 0x06FF, D),
    (0x0750, 0x077F, D), // supplément arabe : toutes duales sauf exceptions
    (0x0898, 0x089F, T), // arabe étendu-B : marques
    (0x08A0, 0x08AA, D),
    (0x08AB, 0x08AE, R),
    (0x08AF, 0x08B0, D),
    (0x08B1, 0x08B2, R),
    (0x08B3, 0x08B4, D),
    (0x08B5, 0x08B5, D),
    (0x08B6, 0x08C7, D),
    (0x08C8, 0x08C8, D),
    (0x08CA, 0x08E1, T),
    (0x08E3, 0x08FF, T),
    (0x200C, 0x200C, U), // ZWNJ : casse la liaison
    (0x200D, 0x200D, C), // ZWJ : force la liaison
];
// Les blocs de formes de présentation (U+FB50–U+FDFF, U+FE70–U+FEFF) sont
// volontairement absents : ces caractères sont **déjà** des formes
// contextuelles, les laisser non joignants les rend tels quels.

/// Type de jointure d'un caractère.
#[must_use]
pub fn joining_type(c: char) -> JoiningType {
    let code = u32::from(c);
    let mut lo = 0usize;
    let mut hi = JOINING.len();
    while lo < hi {
        let mid = lo.midpoint(hi);
        let (start, end, kind) = JOINING[mid];
        if code < start {
            hi = mid;
        } else if code > end {
            lo = mid + 1;
        } else {
            return kind;
        }
    }
    // Les marques combinantes des autres écritures sont transparentes.
    if crate::unicode::ccc::combining_class(c) != 0 {
        JoiningType::Transparent
    } else {
        JoiningType::NonJoining
    }
}

/// Vrai si le caractère est transparent au calcul des formes (marque).
#[must_use]
pub fn is_transparent(c: char) -> bool {
    matches!(joining_type(c), JoiningType::Transparent)
}

/// Vrai si le caractère appartient à une écriture cursive que nous savons
/// former (arabe et ses extensions).
#[must_use]
pub fn is_cursive_script(c: char) -> bool {
    let code = u32::from(c);
    (0x0600..=0x06FF).contains(&code)
        || (0x0750..=0x077F).contains(&code)
        || (0x08A0..=0x08FF).contains(&code)
        || (0xFB50..=0xFDFF).contains(&code)
        || (0xFE70..=0xFEFF).contains(&code)
}

/// Formes contextuelles de chaque caractère d'une suite, dans l'ordre
/// logique.
///
/// L'algorithme est celui d'UAX #9 annexe « Cursive joining » : pour chaque
/// lettre non transparente, on regarde la dernière lettre non transparente
/// avant elle et la première après elle ; la forme dépend de la capacité de
/// chacune à se lier du bon côté. Les caractères transparents et les
/// caractères non arabes reçoivent [`JoiningForm::Isolated`], qui n'active
/// aucune substitution utile pour eux.
#[must_use]
pub fn joining_forms(text: &[char]) -> Vec<JoiningForm> {
    let types: Vec<JoiningType> = text.iter().copied().map(joining_type).collect();
    let mut forms = vec![JoiningForm::Isolated; text.len()];
    for i in 0..text.len() {
        if types[i] == JoiningType::Transparent {
            continue;
        }
        // Voisin précédent non transparent.
        let prev_joins = types[..i]
            .iter()
            .rev()
            .find(|t| **t != JoiningType::Transparent)
            .is_some_and(|t| t.joins_following());
        // Voisin suivant non transparent.
        let next_joins = types[i + 1..]
            .iter()
            .find(|t| **t != JoiningType::Transparent)
            .is_some_and(|t| t.joins_preceding());
        let joins_right = prev_joins && types[i].joins_preceding();
        let joins_left = next_joins && types[i].joins_following();
        forms[i] = match (joins_right, joins_left) {
            (true, true) => JoiningForm::Medial,
            (true, false) => JoiningForm::Final,
            (false, true) => JoiningForm::Initial,
            (false, false) => JoiningForm::Isolated,
        };
    }
    forms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted() {
        let mut previous_end = 0u32;
        for &(start, end, _) in JOINING {
            assert!(start <= end, "plage inversée à U+{start:04X}");
            assert!(
                previous_end == 0 || start > previous_end,
                "recouvrement à U+{start:04X}"
            );
            previous_end = end;
        }
    }

    #[test]
    fn joining_types_are_correct() {
        assert_eq!(joining_type('\u{0628}'), JoiningType::DualJoining); // beh
        assert_eq!(joining_type('\u{0627}'), JoiningType::RightJoining); // alef
        assert_eq!(joining_type('\u{0621}'), JoiningType::NonJoining); // hamza
        assert_eq!(joining_type('\u{0640}'), JoiningType::JoinCausing); // tatweel
        assert_eq!(joining_type('\u{064E}'), JoiningType::Transparent); // fatha
        assert_eq!(joining_type('a'), JoiningType::NonJoining);
    }

    /// « بببب » : initiale, médiane, médiane, finale.
    #[test]
    fn four_behs_give_four_forms() {
        let text: Vec<char> = "\u{0628}\u{0628}\u{0628}\u{0628}".chars().collect();
        assert_eq!(
            joining_forms(&text),
            vec![
                JoiningForm::Initial,
                JoiningForm::Medial,
                JoiningForm::Medial,
                JoiningForm::Final
            ]
        );
    }

    /// Un alef ne se lie pas à la lettre suivante : « باب » donne
    /// initiale, finale, isolée.
    #[test]
    fn alef_breaks_the_join() {
        let text: Vec<char> = "\u{0628}\u{0627}\u{0628}".chars().collect();
        assert_eq!(
            joining_forms(&text),
            vec![
                JoiningForm::Initial,
                JoiningForm::Final,
                JoiningForm::Isolated
            ]
        );
    }

    /// Une voyelle entre deux lettres ne casse pas la liaison.
    #[test]
    fn marks_are_skipped() {
        let text: Vec<char> = "\u{0628}\u{064E}\u{0628}".chars().collect();
        let forms = joining_forms(&text);
        assert_eq!(forms[0], JoiningForm::Initial);
        assert_eq!(forms[2], JoiningForm::Final);
    }

    /// ZWNJ coupe la liaison, ZWJ la force.
    #[test]
    fn zero_width_joiners() {
        let cut: Vec<char> = "\u{0628}\u{200C}\u{0628}".chars().collect();
        assert_eq!(joining_forms(&cut)[0], JoiningForm::Isolated);
        let keep: Vec<char> = "\u{0628}\u{200D}".chars().collect();
        assert_eq!(joining_forms(&keep)[0], JoiningForm::Initial);
    }

    #[test]
    fn feature_tags() {
        assert_eq!(JoiningForm::Initial.feature(), *b"init");
        assert_eq!(JoiningForm::Final.feature(), *b"fina");
    }
}
