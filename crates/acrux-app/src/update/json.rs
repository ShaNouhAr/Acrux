//! Lecteur JSON minimal, écrit pour lire une réponse de GitHub.
//!
//! Chercher `"tag_name":"` dans le texte brut marche jusqu'au jour où le
//! texte de la version contient la même suite de caractères — et ce jour-là,
//! l'application propose d'installer n'importe quoi. Une vraie lecture coûte
//! cent lignes et supprime la question.
//!
//! Ce n'est pas un analyseur JSON complet, et il ne prétend pas l'être : les
//! nombres sont gardés tels quels sous forme de texte, puisque rien ici n'a
//! besoin de les calculer. Les chaînes, elles, sont décodées correctement —
//! échappements compris, `\uXXXX` et paires de substitution comprises.

use std::collections::BTreeMap;

/// Une valeur JSON.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `null`.
    Null,
    /// `true` ou `false`.
    Bool(bool),
    /// Nombre, gardé tel qu'il est écrit.
    Number(String),
    /// Chaîne, échappements résolus.
    Text(String),
    /// Tableau.
    Array(Vec<Value>),
    /// Objet.
    Object(BTreeMap<String, Value>),
}

impl Value {
    /// La valeur d'un champ, si c'est un objet qui le porte.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(map) => map.get(key),
            _ => None,
        }
    }

    /// La chaîne, si c'en est une.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Text(text) => Some(text),
            _ => None,
        }
    }

    /// Le tableau, si c'en est un.
    #[must_use]
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }
}

/// Lit un document JSON complet.
///
/// Rend `None` si le texte n'est pas du JSON valide, ou s'il reste quelque
/// chose après la valeur.
#[must_use]
pub fn parse(text: &str) -> Option<Value> {
    let bytes: Vec<char> = text.chars().collect();
    let mut at = 0usize;
    let value = value(&bytes, &mut at, 0)?;
    skip_space(&bytes, &mut at);
    (at >= bytes.len()).then_some(value)
}

/// Profondeur d'imbrication acceptée : une réponse normale en utilise trois
/// ou quatre, et cette borne empêche un document hostile de faire déborder
/// la pile.
const MAX_DEPTH: usize = 32;

fn value(text: &[char], at: &mut usize, depth: usize) -> Option<Value> {
    if depth > MAX_DEPTH {
        return None;
    }
    skip_space(text, at);
    match text.get(*at)? {
        '{' => object(text, at, depth),
        '[' => array(text, at, depth),
        '"' => string(text, at).map(Value::Text),
        't' => literal(text, at, "true").then_some(Value::Bool(true)),
        'f' => literal(text, at, "false").then_some(Value::Bool(false)),
        'n' => literal(text, at, "null").then_some(Value::Null),
        _ => number(text, at),
    }
}

fn object(text: &[char], at: &mut usize, depth: usize) -> Option<Value> {
    *at += 1; // '{'
    let mut map = BTreeMap::new();
    skip_space(text, at);
    if text.get(*at) == Some(&'}') {
        *at += 1;
        return Some(Value::Object(map));
    }
    loop {
        skip_space(text, at);
        let key = string(text, at)?;
        skip_space(text, at);
        if text.get(*at) != Some(&':') {
            return None;
        }
        *at += 1;
        let item = value(text, at, depth + 1)?;
        map.insert(key, item);
        skip_space(text, at);
        match text.get(*at) {
            Some(',') => *at += 1,
            Some('}') => {
                *at += 1;
                return Some(Value::Object(map));
            }
            _ => return None,
        }
    }
}

fn array(text: &[char], at: &mut usize, depth: usize) -> Option<Value> {
    *at += 1; // '['
    let mut items = Vec::new();
    skip_space(text, at);
    if text.get(*at) == Some(&']') {
        *at += 1;
        return Some(Value::Array(items));
    }
    loop {
        items.push(value(text, at, depth + 1)?);
        skip_space(text, at);
        match text.get(*at) {
            Some(',') => *at += 1,
            Some(']') => {
                *at += 1;
                return Some(Value::Array(items));
            }
            _ => return None,
        }
    }
}

fn string(text: &[char], at: &mut usize) -> Option<String> {
    if text.get(*at) != Some(&'"') {
        return None;
    }
    *at += 1;
    let mut out = String::new();
    loop {
        let c = *text.get(*at)?;
        *at += 1;
        match c {
            '"' => return Some(out),
            '\\' => {
                let escape = *text.get(*at)?;
                *at += 1;
                match escape {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => out.push(unicode(text, at)?),
                    _ => return None,
                }
            }
            // Les caractères de commande doivent être échappés (RFC 8259).
            c if (c as u32) < 0x20 => return None,
            c => out.push(c),
        }
    }
}

/// Lit `\uXXXX`, paire de substitution comprise.
fn unicode(text: &[char], at: &mut usize) -> Option<char> {
    let first = hex4(text, at)?;
    // Demi-haute d'une paire de substitution : la demi-basse suit.
    if (0xD800..0xDC00).contains(&first) {
        if text.get(*at) != Some(&'\\') || text.get(*at + 1) != Some(&'u') {
            return None;
        }
        *at += 2;
        let second = hex4(text, at)?;
        if !(0xDC00..0xE000).contains(&second) {
            return None;
        }
        let combined = 0x1_0000 + ((first - 0xD800) << 10) + (second - 0xDC00);
        return char::from_u32(combined);
    }
    char::from_u32(first)
}

fn hex4(text: &[char], at: &mut usize) -> Option<u32> {
    let mut value = 0u32;
    for _ in 0..4 {
        let digit = text.get(*at)?.to_digit(16)?;
        value = value * 16 + digit;
        *at += 1;
    }
    Some(value)
}

fn number(text: &[char], at: &mut usize) -> Option<Value> {
    let start = *at;
    if text.get(*at) == Some(&'-') {
        *at += 1;
    }
    while text
        .get(*at)
        .is_some_and(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
    {
        *at += 1;
    }
    (start != *at).then(|| Value::Number(text[start..*at].iter().collect()))
}

fn literal(text: &[char], at: &mut usize, word: &str) -> bool {
    let chars: Vec<char> = word.chars().collect();
    if text.len() < *at + chars.len() || text[*at..*at + chars.len()] != chars[..] {
        return false;
    }
    *at += chars.len();
    true
}

fn skip_space(text: &[char], at: &mut usize) {
    while text
        .get(*at)
        .is_some_and(|c| matches!(c, ' ' | '\t' | '\n' | '\r'))
    {
        *at += 1;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{parse, Value};

    #[test]
    fn les_valeurs_simples_se_lisent() {
        assert_eq!(parse("null"), Some(Value::Null));
        assert_eq!(parse(" true "), Some(Value::Bool(true)));
        assert_eq!(parse("false"), Some(Value::Bool(false)));
        assert_eq!(parse("-12.5e3"), Some(Value::Number("-12.5e3".into())));
        assert_eq!(parse("\"abc\""), Some(Value::Text("abc".into())));
    }

    #[test]
    fn les_echappements_sont_resolus() {
        let v = parse(r#""a\"b\\c\nd\u00e9e""#).unwrap();
        assert_eq!(v.as_str(), Some("a\"b\\c\nd\u{e9}e"));
        // Paire de substitution : un emoji hors du plan de base.
        let v = parse(r#""\ud83d\ude42""#).unwrap();
        assert_eq!(v.as_str(), Some("🙂"));
    }

    #[test]
    fn une_accolade_dans_une_chaine_ne_trompe_pas() {
        let text = r#"{"body": "texte avec { et \" et }", "tag_name": "v1.2.3"}"#;
        let v = parse(text).unwrap();
        assert_eq!(v.get("tag_name").unwrap().as_str(), Some("v1.2.3"));
        assert_eq!(
            v.get("body").unwrap().as_str(),
            Some("texte avec { et \" et }")
        );
    }

    #[test]
    fn les_tableaux_imbriques_se_lisent() {
        let v = parse(r#"{"a": [1, [2, {"b": "c"}], {"d": []}]}"#).unwrap();
        let a = v.get("a").unwrap().as_array().unwrap();
        assert_eq!(a.len(), 3);
        let inner = a[1].as_array().unwrap();
        assert_eq!(inner[1].get("b").unwrap().as_str(), Some("c"));
    }

    #[test]
    fn le_json_invalide_est_refuse() {
        for mauvais in [
            "{",
            "}",
            "{\"a\"}",
            "{\"a\": }",
            "[1, ]",
            "\"non terminée",
            "{\"a\": 1} en trop",
            "tru",
            "\"\\x\"",
        ] {
            assert_eq!(parse(mauvais), None, "« {mauvais} » accepté");
        }
    }

    #[test]
    fn limbrication_est_bornee() {
        let profond = "[".repeat(200) + &"]".repeat(200);
        assert_eq!(parse(&profond), None);
    }
}
