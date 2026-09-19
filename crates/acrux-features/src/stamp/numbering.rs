//! Numérotation et jetons de texte des en-têtes, pieds de page et
//! numérotations Bates.
//!
//! Un en-tête n'est pas un texte figé : il contient des **jetons** remplacés
//! au moment de l'écriture, comme dans Acrobat. La liste est volontairement
//! courte et lisible :
//!
//! | Jeton | Remplacé par |
//! |-------|--------------|
//! | `{page}` | numéro de la page, dans le format demandé |
//! | `{pages}` | nombre total de pages du document |
//! | `{date}` | date du jour, `AAAA-MM-JJ` (UTC, décalable) |
//! | `{time}` | heure, `HH:MM` (UTC) |
//! | `{datetime}` | `AAAA-MM-JJ HH:MM` (UTC) |
//! | `{filename}` | nom du fichier d'origine |
//! | `{bates}` | numéro Bates complet (préfixe, chiffres, suffixe) |
//!
//! `{{` et `}}` écrivent une accolade littérale. Un jeton inconnu est laissé
//! tel quel et signalé à l'appelant.

use std::fmt::Write as _;

/// Format d'un numéro de page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NumberFormat {
    /// 1, 2, 3…
    #[default]
    Decimal,
    /// i, ii, iii…
    LowerRoman,
    /// I, II, III…
    UpperRoman,
    /// a, b, c… z, aa, ab…
    LowerAlpha,
    /// A, B, C… Z, AA, AB…
    UpperAlpha,
}

impl NumberFormat {
    /// Format d'après son nom : `1`, `i`, `I`, `a`, `A`, ou les noms longs
    /// `decimal`, `romain`, `roman`, `alpha`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "1" | "decimal" | "arabe" => Some(NumberFormat::Decimal),
            "i" | "romain" | "roman" => Some(NumberFormat::LowerRoman),
            "I" | "ROMAIN" | "ROMAN" => Some(NumberFormat::UpperRoman),
            "a" | "alpha" => Some(NumberFormat::LowerAlpha),
            "A" | "ALPHA" => Some(NumberFormat::UpperAlpha),
            _ => None,
        }
    }

    /// Écrit un numéro dans ce format.
    ///
    /// Les numéros nuls ou négatifs n'ont pas de représentation romaine ni
    /// alphabétique : ils sont écrits en chiffres arabes, comme le fait
    /// Acrobat.
    #[must_use]
    pub fn format(self, number: i64) -> String {
        let Ok(n) = u32::try_from(number) else {
            return number.to_string();
        };
        if n == 0 {
            return number.to_string();
        }
        match self {
            NumberFormat::Decimal => number.to_string(),
            NumberFormat::LowerRoman => roman(n).to_lowercase(),
            NumberFormat::UpperRoman => roman(n),
            NumberFormat::LowerAlpha => alpha(n).to_lowercase(),
            NumberFormat::UpperAlpha => alpha(n),
        }
    }
}

/// Chiffres romains en majuscules (notation soustractive, jusqu'à 3999 ;
/// au-delà les milliers sont écrits par autant de `M`).
fn roman(mut n: u32) -> String {
    const STEPS: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (value, letters) in STEPS {
        while n >= value {
            out.push_str(letters);
            n -= value;
        }
    }
    out
}

/// Numérotation alphabétique bijective en base 26 : A, B… Z, AA, AB…
fn alpha(mut n: u32) -> String {
    let mut letters = Vec::new();
    while n > 0 {
        let rem = (n - 1) % 26;
        letters.push(char::from(b'A' + u8::try_from(rem).unwrap_or(0)));
        n = (n - 1) / 26;
    }
    letters.iter().rev().collect()
}

/// Valeurs disponibles pour le remplacement des jetons d'une page.
#[derive(Debug, Clone)]
pub struct Tokens {
    /// Numéro de page déjà mis en forme.
    pub page: String,
    /// Nombre total de pages.
    pub pages: usize,
    /// Nom du fichier.
    pub file_name: String,
    /// Numéro Bates complet, si la commande en produit un.
    pub bates: Option<String>,
    /// Date `AAAA-MM-JJ` (UTC).
    pub date: String,
    /// Heure `HH:MM` (UTC).
    pub time: String,
}

impl Tokens {
    /// Remplace les jetons de `template` ; les jetons inconnus sont laissés
    /// tels quels et ajoutés à `unknown`.
    #[must_use]
    pub fn expand(&self, template: &str, unknown: &mut Vec<String>) -> String {
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(start) = rest.find(['{', '}']) {
            out.push_str(&rest[..start]);
            let tail = &rest[start..];
            // Accolade doublée : accolade littérale.
            if tail.starts_with("{{") || tail.starts_with("}}") {
                out.push(tail.as_bytes()[0] as char);
                rest = &tail[2..];
                continue;
            }
            let Some(end) = tail.find('}') else {
                out.push_str(tail);
                return out;
            };
            let name = &tail[1..end];
            if let Some(value) = self.value(name) {
                out.push_str(&value);
            } else {
                unknown.push(name.to_string());
                out.push_str(&tail[..=end]);
            }
            rest = &tail[end + 1..];
        }
        out.push_str(rest);
        out
    }

    fn value(&self, name: &str) -> Option<String> {
        match name {
            "page" => Some(self.page.clone()),
            "pages" => Some(self.pages.to_string()),
            "date" => Some(self.date.clone()),
            "time" | "heure" => Some(self.time.clone()),
            "datetime" => Some(format!("{} {}", self.date, self.time)),
            "filename" | "fichier" => Some(self.file_name.clone()),
            "bates" => self.bates.clone(),
            _ => None,
        }
    }
}

/// Date et heure (`AAAA-MM-JJ`, `HH:MM`) décalées de `offset_minutes` par
/// rapport à UTC, déduites de la chaîne de date PDF produite par
/// [`crate::annotations::pdf_date_at`] : un seul calendrier dans le projet.
///
/// Rust seul ne sait pas lire le fuseau du système, et lire l'horloge locale
/// demanderait un appel système que cette bibliothèque s'interdit : c'est donc
/// à l'appelant de dire dans quel fuseau il tamponne (`--utc-offset` en ligne
/// de commande). Sans indication, les jetons restent en UTC, comme la date
/// `/M` des annotations.
#[must_use]
pub fn now_at(offset_minutes: i32) -> (String, String) {
    let stamp = crate::annotations::pdf_date_at(offset_minutes);
    let digits: Vec<char> = stamp.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 12 {
        return ("0000-00-00".into(), "00:00".into());
    }
    let take = |from: usize, len: usize| -> String { digits[from..from + len].iter().collect() };
    let mut date = String::new();
    let _ = write!(date, "{}-{}-{}", take(0, 4), take(4, 2), take(6, 2));
    let mut time = String::new();
    let _ = write!(time, "{}:{}", take(8, 2), take(10, 2));
    (date, time)
}

/// Numéro Bates complet : préfixe, numéro sur `digits` chiffres, suffixe.
#[must_use]
pub fn bates_label(prefix: &str, number: u64, digits: usize, suffix: &str) -> String {
    format!("{prefix}{number:0>width$}{suffix}", width = digits.min(20))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn number_formats() {
        assert_eq!(NumberFormat::Decimal.format(42), "42");
        assert_eq!(NumberFormat::LowerRoman.format(4), "iv");
        assert_eq!(NumberFormat::UpperRoman.format(1990), "MCMXC");
        assert_eq!(NumberFormat::UpperRoman.format(3), "III");
        assert_eq!(NumberFormat::LowerAlpha.format(1), "a");
        assert_eq!(NumberFormat::LowerAlpha.format(26), "z");
        assert_eq!(NumberFormat::LowerAlpha.format(27), "aa");
        assert_eq!(NumberFormat::UpperAlpha.format(28), "AB");
        // Hors domaine : repli en chiffres arabes.
        assert_eq!(NumberFormat::LowerRoman.format(0), "0");
        assert_eq!(NumberFormat::UpperAlpha.format(-3), "-3");
    }

    #[test]
    fn format_names() {
        assert_eq!(NumberFormat::from_name("i"), Some(NumberFormat::LowerRoman));
        assert_eq!(NumberFormat::from_name("A"), Some(NumberFormat::UpperAlpha));
        assert_eq!(NumberFormat::from_name("1"), Some(NumberFormat::Decimal));
        assert_eq!(NumberFormat::from_name("zz"), None);
    }

    fn tokens() -> Tokens {
        Tokens {
            page: "iv".into(),
            pages: 12,
            file_name: "rapport.pdf".into(),
            bates: Some("ABC-000042".into()),
            date: "2026-09-19".into(),
            time: "08:30".into(),
        }
    }

    #[test]
    fn tokens_are_replaced() {
        let mut unknown = Vec::new();
        let t = tokens();
        assert_eq!(
            t.expand("Page {page} sur {pages} — {filename}", &mut unknown),
            "Page iv sur 12 — rapport.pdf"
        );
        assert_eq!(t.expand("{date} {time}", &mut unknown), "2026-09-19 08:30");
        assert_eq!(t.expand("{datetime}", &mut unknown), "2026-09-19 08:30");
        assert_eq!(t.expand("{bates}", &mut unknown), "ABC-000042");
        assert!(unknown.is_empty());
        // Accolades littérales et jeton inconnu.
        assert_eq!(t.expand("{{page}}", &mut unknown), "{page}");
        assert_eq!(t.expand("{inconnu}", &mut unknown), "{inconnu}");
        assert_eq!(unknown, vec!["inconnu".to_string()]);
        // Accolade ouvrante jamais fermée : texte gardé tel quel.
        assert_eq!(t.expand("fin {page", &mut unknown), "fin {page");
        assert_eq!(t.expand("", &mut unknown), "");
    }

    #[test]
    fn bates_labels_are_padded() {
        assert_eq!(bates_label("ABC-", 42, 6, ""), "ABC-000042");
        assert_eq!(bates_label("", 1, 1, "-X"), "1-X");
        // Un numéro plus long que le gabarit n'est pas tronqué.
        assert_eq!(bates_label("", 123_456, 3, ""), "123456");
    }

    #[test]
    fn the_utc_offset_shifts_the_clock() {
        let (date, time) = now_at(0);
        let (date_plus, time_plus) = now_at(90);
        // Une heure et demie plus tard : soit l'heure a changé, soit on a
        // franchi minuit et c'est la date qui change.
        assert!(
            (date, time) != (date_plus.clone(), time_plus.clone()),
            "le décalage n'a rien changé : {date_plus} {time_plus}"
        );
        let (date_minus, _) = now_at(-24 * 60);
        assert!(
            date_minus < date_plus,
            "{date_minus} devrait précéder {date_plus}"
        );
    }

    #[test]
    fn now_has_the_expected_shape() {
        let (date, time) = now_at(0);
        assert_eq!(date.len(), 10);
        assert_eq!(time.len(), 5);
        assert!(date.starts_with("20"), "{date}");
        assert_eq!(time.as_bytes()[2], b':');
    }
}
