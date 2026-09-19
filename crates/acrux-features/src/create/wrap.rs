//! Découpe d'un paragraphe en lignes : le cœur de la mise en page.
//!
//! La coupure se fait aux espaces. Les suites d'espaces et de tabulations
//! sont ramenées à une espace simple, comme le fait tout metteur en page : le
//! texte rendu est ainsi identique à ce que l'extraction relira.
//!
//! Un mot plus long que la ligne est coupé : soit par une **césure simple**
//! (le plus grand préfixe qui tient, suivi d'un trait d'union), soit, si la
//! césure est refusée, laissé seul sur sa ligne quitte à déborder. La césure
//! ajoute un caractère au texte : c'est le seul cas où le texte extrait
//! diffère du texte d'entrée, et c'est pourquoi elle est désactivable.

/// Découpe `text` en lignes qui tiennent dans `width` points.
///
/// `measure` donne la largeur d'un fragment en points. Un texte vide rend une
/// ligne vide, pour que les lignes blanches d'un document comptent dans la
/// hauteur.
pub(crate) fn wrap(
    text: &str,
    width: f64,
    hyphenate: bool,
    measure: &dyn Fn(&str) -> f64,
) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return vec![String::new()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in words {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if measure(&candidate) <= width {
            current = candidate;
            continue;
        }
        // Le mot ne tient pas sur la ligne en cours : elle est close.
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        if measure(word) <= width || !hyphenate {
            // Le mot tient seul sur une ligne neuve — ou déborde, faute de
            // césure, ce que l'appelant a accepté.
            current = word.to_string();
            continue;
        }
        let pieces = hyphenate_word(word, width, measure);
        let last = pieces.len().saturating_sub(1);
        for (i, piece) in pieces.into_iter().enumerate() {
            if i == last {
                current = piece;
            } else {
                lines.push(piece);
            }
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Découpe `text` à la largeur, **sans rien ajouter** : la coupure tombe
/// entre deux caractères quelconques. C'est ce qu'il faut pour un bloc de
/// code, où un trait d'union de césure serait un contresens.
pub(crate) fn wrap_hard(text: &str, width: f64, measure: &dyn Fn(&str) -> f64) -> Vec<String> {
    if text.is_empty() || measure(text) <= width {
        return vec![text.to_string()];
    }
    let chars: Vec<char> = text.chars().collect();
    let mut lines = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut take = 1;
        while start + take < chars.len() {
            let candidate: String = chars[start..=start + take].iter().collect();
            if measure(&candidate) > width {
                break;
            }
            take += 1;
        }
        lines.push(chars[start..start + take].iter().collect());
        start += take;
    }
    lines
}

/// Coupe un mot trop long en morceaux, chacun terminé par un trait d'union
/// sauf le dernier. Au moins un caractère par morceau : une largeur
/// ridiculement petite ne fait donc pas tourner la boucle indéfiniment.
fn hyphenate_word(word: &str, width: f64, measure: &dyn Fn(&str) -> f64) -> Vec<String> {
    let chars: Vec<char> = word.chars().collect();
    let mut pieces = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let rest: String = chars[start..].iter().collect();
        if measure(&rest) <= width {
            pieces.push(rest);
            return pieces;
        }
        let mut take = 1;
        // Le plus grand préfixe qui tient, trait d'union compris.
        while start + take < chars.len() {
            let candidate: String = chars[start..=start + take].iter().collect();
            if measure(&format!("{candidate}-")) > width {
                break;
            }
            take += 1;
        }
        let piece: String = chars[start..start + take].iter().collect();
        pieces.push(format!("{piece}-"));
        start += take;
    }
    if pieces.is_empty() {
        pieces.push(String::new());
    }
    pieces
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Police fictive de largeur fixe : un caractère vaut un point.
    #[allow(clippy::cast_precision_loss)]
    fn fixed(text: &str) -> f64 {
        text.chars().count() as f64
    }

    #[test]
    fn an_empty_text_gives_one_empty_line() {
        assert_eq!(wrap("", 10.0, true, &fixed), vec![String::new()]);
        assert_eq!(wrap("   ", 10.0, true, &fixed), vec![String::new()]);
        assert_eq!(wrap("\t", 10.0, true, &fixed), vec![String::new()]);
    }

    #[test]
    fn multiple_spaces_collapse() {
        assert_eq!(wrap("un    deux", 20.0, true, &fixed), vec!["un deux"]);
        assert_eq!(wrap("  un deux  ", 20.0, true, &fixed), vec!["un deux"]);
        assert_eq!(wrap("un\tdeux", 20.0, true, &fixed), vec!["un deux"]);
    }

    #[test]
    fn breaking_happens_at_spaces() {
        // « un deux » fait 7, la ligne en vaut 7 : elle tient tout juste.
        assert_eq!(wrap("un deux", 7.0, true, &fixed), vec!["un deux"]);
        assert_eq!(wrap("un deux", 6.9, true, &fixed), vec!["un", "deux"]);
        assert_eq!(
            wrap("alpha beta gamma delta", 12.0, true, &fixed),
            vec!["alpha beta", "gamma delta"]
        );
    }

    #[test]
    fn a_word_longer_than_the_line_is_hyphenated() {
        // 10 caractères, ligne de 4 : « abc- », « def- », « ghij ».
        let lines = wrap("abcdefghij", 4.0, true, &fixed);
        assert_eq!(lines, vec!["abc-", "def-", "ghij"]);
        // Le texte reconstitué garde toutes les lettres.
        let joined: String = lines.iter().map(|l| l.trim_end_matches('-')).collect();
        assert_eq!(joined, "abcdefghij");
    }

    #[test]
    fn hyphenation_can_be_refused_and_the_word_overflows() {
        assert_eq!(wrap("abcdefghij", 4.0, false, &fixed), vec!["abcdefghij"]);
        assert_eq!(
            wrap("court abcdefghij", 6.0, false, &fixed),
            vec!["court", "abcdefghij"]
        );
    }

    #[test]
    fn a_long_word_after_a_short_one_starts_a_new_line() {
        let lines = wrap("un abcdefghij", 4.0, true, &fixed);
        assert_eq!(lines, vec!["un", "abc-", "def-", "ghij"]);
    }

    #[test]
    fn an_absurdly_narrow_line_still_terminates() {
        let lines = wrap("abcd", 0.5, true, &fixed);
        assert!(lines.len() >= 4, "{lines:?}");
        assert!(lines.iter().all(|l| !l.is_empty()));
    }

    #[test]
    fn hard_wrapping_adds_nothing_to_the_text() {
        assert_eq!(wrap_hard("", 10.0, &fixed), vec![String::new()]);
        assert_eq!(wrap_hard("court", 10.0, &fixed), vec!["court"]);
        let lines = wrap_hard("let x = 12345;", 5.0, &fixed);
        assert_eq!(lines.concat(), "let x = 12345;");
        assert!(lines.iter().all(|l| l.chars().count() <= 5), "{lines:?}");
    }

    #[test]
    fn accents_count_as_one_character_not_two_bytes() {
        // « éé » vaut 2 caractères et 4 octets : la coupure suit les caractères.
        assert_eq!(wrap("éé éé", 5.0, true, &fixed), vec!["éé éé"]);
        assert_eq!(wrap("éé éé", 4.0, true, &fixed), vec!["éé", "éé"]);
    }
}
