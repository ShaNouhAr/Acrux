//! Sérialisation des objets en syntaxe PDF (ISO 32000-2 §7.3).
//!
//! Utilisé par l'outil `acr dump` et, plus tard, par l'écriture
//! incrémentale. La sortie est déterministe (clés de dictionnaire triées).

use crate::objects::{Name, Object};

/// Sérialise un objet en syntaxe PDF dans `out`.
pub fn write_object(obj: &Object, out: &mut Vec<u8>) {
    match obj {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Bool(true) => out.extend_from_slice(b"true"),
        Object::Bool(false) => out.extend_from_slice(b"false"),
        Object::Integer(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Object::Real(r) => write_real(*r, out),
        Object::String(s) => write_string(s, out),
        Object::Name(n) => write_name(n, out),
        Object::Array(a) => {
            out.push(b'[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                write_object(item, out);
            }
            out.push(b']');
        }
        Object::Dict(d) => write_dict(d, out),
        Object::Stream { dict, raw } => {
            write_dict(dict, out);
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(raw);
            out.extend_from_slice(b"\nendstream");
        }
        Object::Reference(r) => {
            out.extend_from_slice(format!("{} {} R", r.number, r.generation).as_bytes());
        }
    }
}

/// Version texte (lossy) d'un objet, pour l'affichage et les tests.
#[must_use]
pub fn to_string(obj: &Object) -> String {
    let mut out = Vec::new();
    write_object(obj, &mut out);
    String::from_utf8_lossy(&out).into_owned()
}

fn write_dict(d: &crate::objects::Dict, out: &mut Vec<u8>) {
    out.extend_from_slice(b"<<");
    for (k, v) in d {
        out.push(b' ');
        write_name(k, out);
        out.push(b' ');
        write_object(v, out);
    }
    out.extend_from_slice(b" >>");
}

/// Réel sans exposant (interdit en PDF), sans zéros inutiles.
fn write_real(r: f64, out: &mut Vec<u8>) {
    if !r.is_finite() {
        out.push(b'0');
        return;
    }
    if r.fract() == 0.0 && r.abs() < 1e15 {
        #[allow(clippy::cast_possible_truncation)]
        let i = r as i64;
        out.extend_from_slice(i.to_string().as_bytes());
        return;
    }
    // Représentation décimale la plus courte qui redonne exactement le même
    // f64 (`Display` n'emploie jamais la notation exponentielle, interdite en
    // PDF) : une réécriture ne dégrade jamais un nombre (matrices, coordonnées).
    let shortest = format!("{r}");
    let s = shortest.as_str();
    // `-0` n'a pas de sens.
    let s = if s == "-0" { "0" } else { s };
    out.extend_from_slice(s.as_bytes());
}

/// Chaîne littérale avec échappement des caractères spéciaux ; hexadécimale
/// si elle contient beaucoup d'octets non imprimables.
fn write_string(s: &[u8], out: &mut Vec<u8>) {
    let binary = s.iter().filter(|&&b| !(0x20..=0x7E).contains(&b)).count();
    if binary * 4 > s.len() && !s.is_empty() {
        out.push(b'<');
        for b in s {
            out.extend_from_slice(format!("{b:02X}").as_bytes());
        }
        out.push(b'>');
        return;
    }
    out.push(b'(');
    for &b in s {
        match b {
            b'(' | b')' | b'\\' => {
                out.push(b'\\');
                out.push(b);
            }
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0C => out.extend_from_slice(b"\\f"),
            b if !(0x20..=0x7E).contains(&b) => {
                out.extend_from_slice(format!("\\{b:03o}").as_bytes());
            }
            b => out.push(b),
        }
    }
    out.push(b')');
}

/// Nom avec encodage `#xx` des caractères non réguliers (§7.3.5).
fn write_name(n: &Name, out: &mut Vec<u8>) {
    out.push(b'/');
    for &b in &n.0 {
        if b == b'#' || !(0x21..=0x7E).contains(&b) || crate::lexer::is_delimiter(b) {
            out.extend_from_slice(format!("#{b:02X}").as_bytes());
        } else {
            out.push(b);
        }
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::objects::{Dict, ObjectRef};
    use crate::parser::Parser;

    #[test]
    fn roundtrip_through_parser() {
        let src = b"<< /A [1 2.5 -3 /N#20x (s\\(t\\)) <0A0B> true null 4 0 R] /B << /C 1 >> >>";
        let obj = Parser::new(src).parse_object().unwrap();
        let text = to_string(&obj);
        let again = Parser::new(text.as_bytes()).parse_object().unwrap();
        assert_eq!(obj, again);
    }

    #[test]
    fn formatting() {
        assert_eq!(to_string(&Object::Real(1.5)), "1.5");
        assert_eq!(to_string(&Object::Real(2.0)), "2");
        assert_eq!(to_string(&Object::Real(-0.000_001)), "-0.000001");
        assert_eq!(to_string(&Object::Real(1e-9)), "0.000000001");
        assert_eq!(to_string(&Object::Real(1e-15)), "0.000000000000001");
        assert_eq!(to_string(&Object::Real(0.1 + 0.2)), "0.30000000000000004");
        assert_eq!(
            to_string(&Object::Real(1.0 / 7000.0)),
            "0.00014285714285714287"
        );
        assert_eq!(to_string(&Object::String(b"a(b)".to_vec())), "(a\\(b\\))");
        assert_eq!(
            to_string(&Object::String(vec![0xFE, 0xFF, 0x00, 0x41])),
            "<FEFF0041>"
        );
        assert_eq!(to_string(&Object::Name(Name::new("A B/C"))), "/A#20B#2FC");
        assert_eq!(
            to_string(&Object::Reference(ObjectRef {
                number: 3,
                generation: 1
            })),
            "3 1 R"
        );
        let mut d = Dict::new();
        d.insert(Name::new("Z"), Object::Integer(1));
        d.insert(Name::new("A"), Object::Null);
        assert_eq!(to_string(&Object::Dict(d)), "<< /A null /Z 1 >>");
        assert_eq!(
            to_string(&Object::Stream {
                dict: Dict::new(),
                raw: b"xy".to_vec()
            }),
            "<< >>\nstream\nxy\nendstream"
        );
    }
}
