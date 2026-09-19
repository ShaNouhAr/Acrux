//! Reconnaissance de l'écriture d'un caractère, exprimée directement par le
//! **tag OpenType** du script (`latn`, `arab`, `hebr`, `hani`…).
//!
//! OpenType indexe les fonctionnalités par script ; il faut donc savoir à
//! quel script appartient un texte pour choisir les bonnes tables `GSUB` et
//! `GPOS`. Les tags sont ceux de la « Script Tags » de la spécification
//! OpenType (Microsoft).
//!
//! **Couverture** : les écritures que nous savons composer ou pour
//! lesquelles le choix du tag change quelque chose. Tout le reste tombe sur
//! `latn`, ce qui est le repli sans effet (la plupart des polices ne
//! déclarent que `DFLT` et `latn`).

/// Tag OpenType du script d'un caractère.
///
/// Les caractères communs (chiffres, ponctuation, espaces) renvoient `None` :
/// ils héritent du script de leur voisinage.
#[must_use]
pub fn script_tag(c: char) -> Option<[u8; 4]> {
    let code = u32::from(c);
    let tag = match code {
        0x0041..=0x005A | 0x0061..=0x007A | 0x00C0..=0x02AF | 0x1E00..=0x1EFF => b"latn",
        0x0370..=0x03FF | 0x1F00..=0x1FFF => b"grek",
        0x0400..=0x052F | 0x2DE0..=0x2DFF | 0xA640..=0xA69F => b"cyrl",
        0x0530..=0x058F => b"armn",
        0x0590..=0x05FF | 0xFB1D..=0xFB4F => b"hebr",
        0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => {
            b"arab"
        }
        0x0700..=0x074F => b"syrc",
        0x0780..=0x07BF => b"thaa",
        0x0900..=0x097F => b"deva",
        0x0980..=0x09FF => b"beng",
        0x0A00..=0x0A7F => b"guru",
        0x0A80..=0x0AFF => b"gujr",
        0x0B00..=0x0B7F => b"orya",
        0x0B80..=0x0BFF => b"taml",
        0x0C00..=0x0C7F => b"telu",
        0x0C80..=0x0CFF => b"knda",
        0x0D00..=0x0D7F => b"mlym",
        0x0D80..=0x0DFF => b"sinh",
        0x0E00..=0x0E7F => b"thai",
        0x0E80..=0x0EFF => b"lao ",
        0x0F00..=0x0FFF => b"tibt",
        0x1000..=0x109F => b"mymr",
        0x10A0..=0x10FF => b"geor",
        0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => b"hang",
        0x1780..=0x17FF => b"khmr",
        0x3040..=0x30FF | 0x31F0..=0x31FF => b"kana",
        0x2E80..=0x2FDF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF => b"hani",
        _ => return None,
    };
    Some(*tag)
}

/// Script dominant d'un texte : le premier script fort rencontré.
///
/// Sert de valeur par défaut quand l'appelant ne précise pas le script.
#[must_use]
pub fn dominant_script(text: &str) -> [u8; 4] {
    text.chars().find_map(script_tag).unwrap_or(*b"latn")
}

/// Vrai si le script se compose de droite à gauche.
#[must_use]
pub fn is_rtl_script(tag: [u8; 4]) -> bool {
    matches!(
        &tag,
        b"arab" | b"hebr" | b"syrc" | b"thaa" | b"nko " | b"samr" | b"mand" | b"adlm"
    )
}

/// Vrai si le script demande un moteur syllabique que nous n'écrivons pas
/// (les substitutions `GSUB` s'appliquent quand même, mais sans réordonner
/// les voyelles ni découper les syllabes).
#[must_use]
pub fn needs_complex_engine(tag: [u8; 4]) -> bool {
    matches!(
        &tag,
        b"deva"
            | b"beng"
            | b"guru"
            | b"gujr"
            | b"orya"
            | b"taml"
            | b"telu"
            | b"knda"
            | b"mlym"
            | b"sinh"
            | b"khmr"
            | b"mymr"
            | b"tibt"
            | b"thai"
            | b"lao "
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_are_recognised() {
        assert_eq!(script_tag('a'), Some(*b"latn"));
        assert_eq!(script_tag('é'), Some(*b"latn"));
        assert_eq!(script_tag('α'), Some(*b"grek"));
        assert_eq!(script_tag('д'), Some(*b"cyrl"));
        assert_eq!(script_tag('א'), Some(*b"hebr"));
        assert_eq!(script_tag('ب'), Some(*b"arab"));
        assert_eq!(script_tag('漢'), Some(*b"hani"));
        assert_eq!(script_tag('あ'), Some(*b"kana"));
        assert_eq!(script_tag('1'), None);
        assert_eq!(script_tag(' '), None);
    }

    #[test]
    fn dominant_ignores_common_characters() {
        assert_eq!(dominant_script("12 אבג"), *b"hebr");
        assert_eq!(dominant_script("123"), *b"latn");
        assert_eq!(dominant_script(""), *b"latn");
    }

    #[test]
    fn direction_and_complexity() {
        assert!(is_rtl_script(*b"arab"));
        assert!(!is_rtl_script(*b"latn"));
        assert!(needs_complex_engine(*b"deva"));
        assert!(!needs_complex_engine(*b"arab"));
    }
}
