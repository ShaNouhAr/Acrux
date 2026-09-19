//! Découpage d'un flux de contenu en opérations (ISO 32000-2 §7.8.2) :
//! chaque opération est un opérateur précédé de ses opérandes. Les images
//! en ligne `BI … ID … EI` (§8.9.7) sont extraites ici, car leurs données
//! binaires ne respectent pas la grammaire des tokens.

use acrux_core::Result;
use acrux_document::lexer::is_whitespace;
use acrux_document::{Dict, Name, Object, Parser, Token};

/// Une opération : opérateur et opérandes.
#[derive(Debug, Clone, PartialEq)]
pub struct Operation {
    /// Nom de l'opérateur (`BT`, `Tj`, `re`, …).
    pub operator: Vec<u8>,
    /// Opérandes dans l'ordre.
    pub operands: Vec<Object>,
    /// Pour `BI`/`ID`/`EI` : dictionnaire et données de l'image en ligne.
    pub inline_image: Option<InlineImage>,
}

/// Image en ligne extraite d'un flux de contenu.
#[derive(Debug, Clone, PartialEq)]
pub struct InlineImage {
    /// Dictionnaire (clés abrégées possibles : `/W`, `/H`, `/BPC`, `/CS`, `/F`…).
    pub dict: Dict,
    /// Données encodées telles quelles.
    pub data: Vec<u8>,
}

/// Itérateur d'opérations sur un flux de contenu.
pub struct ContentLexer<'a> {
    parser: Parser<'a>,
    data: &'a [u8],
}

impl<'a> ContentLexer<'a> {
    /// Nouveau lecteur.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            parser: Parser::new(data),
            data,
        }
    }

    /// Position courante dans le flux (octets consommés).
    ///
    /// Les octets `data[pos_avant..pos_après]` autour d'un appel à
    /// [`Self::next_operation`] contiennent l'opération lue **et** l'espacement
    /// qui la précède : les recopier tels quels reproduit le flux octet pour
    /// octet (voir `acrux_features::edit_text::rewrite_content`).
    #[must_use]
    pub fn pos(&self) -> usize {
        self.parser.pos()
    }

    /// Opération suivante, `None` à la fin. Les opérandes illisibles sont
    /// ignorés ; un opérateur inconnu est transmis tel quel (l'interpréteur
    /// décide).
    ///
    /// # Errors
    /// Seulement sur une erreur lexicale bloquante.
    pub fn next_operation(&mut self) -> Result<Option<Operation>> {
        let mut operands = Vec::new();
        loop {
            let tok = self.parser.peek_token()?;
            match tok {
                Token::Eof => {
                    return Ok(None);
                }
                Token::Keyword(k) => {
                    self.parser.next_token()?;
                    if k == b"BI" {
                        let image = self.read_inline_image();
                        return Ok(Some(Operation {
                            operator: b"BI".to_vec(),
                            operands: Vec::new(),
                            inline_image: image,
                        }));
                    }
                    if matches!(k.as_slice(), b"true" | b"false" | b"null") {
                        operands.push(match k.as_slice() {
                            b"true" => Object::Bool(true),
                            b"false" => Object::Bool(false),
                            _ => Object::Null,
                        });
                        continue;
                    }
                    return Ok(Some(Operation {
                        operator: k,
                        operands,
                        inline_image: None,
                    }));
                }
                Token::ArrayClose | Token::DictClose | Token::BraceOpen | Token::BraceClose => {
                    // Délimiteur orphelin : ignoré.
                    self.parser.next_token()?;
                }
                _ => {
                    // En cas d'erreur le parseur a déjà consommé le token fautif.
                    if let Ok(o) = self.parser.parse_object() {
                        if operands.len() < 64 {
                            operands.push(o);
                        }
                    }
                }
            }
        }
    }

    /// Lit `<dict> ID <data> EI` après `BI`.
    fn read_inline_image(&mut self) -> Option<InlineImage> {
        let mut dict = Dict::new();
        loop {
            match self.parser.peek_token().ok()? {
                Token::Name(key) => {
                    self.parser.next_token().ok()?;
                    let value = self.parser.parse_object().ok()?;
                    dict.insert(Name(key), value);
                }
                Token::Keyword(k) if k == b"ID" => {
                    self.parser.next_token().ok()?;
                    break;
                }
                Token::Eof => return None,
                _ => {
                    self.parser.next_token().ok()?;
                }
            }
        }
        // Un seul espacement après ID (§8.9.7).
        let mut pos = self.parser.pos();
        if pos < self.data.len() && is_whitespace(self.data[pos]) {
            pos += 1;
        }
        let start = pos;
        // Longueur connue pour les images non filtrées : on l'utilise pour ne
        // pas confondre des octets binaires avec `EI`.
        let end = match unfiltered_length(&dict) {
            Some(len) if start + len <= self.data.len() => {
                let mut e = start + len;
                // Puis `EI` après un espacement éventuel.
                while e < self.data.len() && is_whitespace(self.data[e]) {
                    e += 1;
                }
                if self.data[e..].starts_with(b"EI") && ends_token(self.data, e + 2) {
                    self.parser.seek(e + 2);
                    return Some(InlineImage {
                        dict,
                        data: self.data[start..start + len].to_vec(),
                    });
                }
                // Longueur déclarée fausse : recherche classique.
                self.find_ei(start)
            }
            _ => self.find_ei(start),
        };
        let end = end?;
        let mut data_end = end;
        // Retire l'espacement qui précède `EI`.
        if data_end > start && is_whitespace(self.data[data_end - 1]) {
            data_end -= 1;
        }
        self.parser.seek(end + 2);
        Some(InlineImage {
            dict,
            data: self.data[start..data_end].to_vec(),
        })
    }

    /// Cherche `EI` entouré d'espacements (ou en fin de données) à partir de `from`.
    fn find_ei(&self, from: usize) -> Option<usize> {
        let d = self.data;
        let mut i = from;
        while i + 1 < d.len() {
            if d[i] == b'E'
                && d[i + 1] == b'I'
                && (i == 0 || is_whitespace(d[i - 1]))
                && ends_token(d, i + 2)
            {
                return Some(i);
            }
            i += 1;
        }
        None
    }
}

fn ends_token(data: &[u8], pos: usize) -> bool {
    pos >= data.len() || is_whitespace(data[pos]) || acrux_document::lexer::is_delimiter(data[pos])
}

/// Taille en octets d'une image en ligne sans filtre (§8.9.7) : W × H × BPC × composantes.
fn unfiltered_length(dict: &Dict) -> Option<usize> {
    let get = |long: &str, short: &str| {
        dict.get(&Name::new(long))
            .or_else(|| dict.get(&Name::new(short)))
    };
    let filter = get("Filter", "F");
    match filter {
        None => {}
        Some(Object::Array(a)) if a.is_empty() => {}
        Some(_) => return None,
    }
    let w = usize::try_from(get("Width", "W")?.as_i64()?).ok()?;
    let h = usize::try_from(get("Height", "H")?.as_i64()?).ok()?;
    let mask = matches!(get("ImageMask", "IM"), Some(Object::Bool(true)));
    let bpc = if mask {
        1
    } else {
        usize::try_from(get("BitsPerComponent", "BPC")?.as_i64()?).ok()?
    };
    let ncomp = if mask {
        1
    } else {
        match get("ColorSpace", "CS") {
            Some(Object::Name(n)) => match n.0.as_slice() {
                b"DeviceRGB" | b"RGB" | b"CalRGB" => 3,
                b"DeviceCMYK" | b"CMYK" => 4,
                b"DeviceGray" | b"G" | b"CalGray" | b"Indexed" | b"I" => 1,
                _ => return None, // espace nommé dans les ressources : inconnu ici
            },
            Some(Object::Array(a)) => match a.first() {
                Some(Object::Name(n)) if n.0 == b"Indexed" || n.0 == b"I" => 1,
                _ => return None,
            },
            None => 1,
            _ => return None,
        }
    };
    let row = w.checked_mul(bpc)?.checked_mul(ncomp)?.div_ceil(8);
    row.checked_mul(h)
}

/// Toutes les opérations d'un flux.
///
/// # Errors
/// Erreur lexicale bloquante.
pub fn parse_content(data: &[u8]) -> Result<Vec<Operation>> {
    let mut lx = ContentLexer::new(data);
    let mut ops = Vec::new();
    while let Some(op) = lx.next_operation()? {
        ops.push(op);
    }
    Ok(ops)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn ops(src: &[u8]) -> Vec<(String, usize)> {
        parse_content(src)
            .unwrap()
            .into_iter()
            .map(|o| {
                (
                    String::from_utf8_lossy(&o.operator).into_owned(),
                    o.operands.len(),
                )
            })
            .collect()
    }

    #[test]
    fn basic_operations() {
        assert_eq!(
            ops(b"q 1 0 0 1 10 20 cm BT /F1 12 Tf (Hi) Tj [(A) -20 (B)] TJ ET Q"),
            vec![
                ("q".into(), 0),
                ("cm".into(), 6),
                ("BT".into(), 0),
                ("Tf".into(), 2),
                ("Tj".into(), 1),
                ("TJ".into(), 1),
                ("ET".into(), 0),
                ("Q".into(), 0),
            ]
        );
    }

    #[test]
    fn garbage_is_tolerated() {
        assert_eq!(
            ops(b"1 2 ] >> } foo 3 re"),
            vec![("foo".into(), 2), ("re".into(), 1)]
        );
        assert_eq!(ops(b"true false null sc"), vec![("sc".into(), 3)]);
        assert_eq!(ops(b""), Vec::<(String, usize)>::new());
        assert_eq!(
            ops(b"1 2 3"),
            Vec::<(String, usize)>::new(),
            "opérandes sans opérateur"
        );
    }

    #[test]
    fn inline_image_unfiltered_with_binary_ei_lookalike() {
        // 2×2 gris 8 bits = 4 octets, qui commencent par "EI" suivi d'un octet
        // d'espacement (piège classique).
        let mut src = b"q BI /W 2 /H 2 /CS /G /BPC 8 ID ".to_vec();
        src.extend_from_slice(b"EI\x00\xff");
        src.extend_from_slice(b" EI Q");
        let all = parse_content(&src).unwrap();
        assert_eq!(all.len(), 3);
        let img = all[1].inline_image.as_ref().unwrap();
        assert_eq!(img.data, b"EI\x00\xff");
        assert_eq!(img.dict.get(&Name::new("W")), Some(&Object::Integer(2)));
        assert_eq!(all[2].operator, b"Q");
    }

    #[test]
    fn inline_image_filtered_uses_ei_search() {
        let src = b"BI /W 4 /H 4 /BPC 8 /CS /RGB /F /AHx ID\n00ff00 ff0000>\nEI\n1 0 0 RG";
        let all = parse_content(src).unwrap();
        assert_eq!(all.len(), 2);
        let img = all[0].inline_image.as_ref().unwrap();
        assert_eq!(img.data, b"00ff00 ff0000>");
        assert_eq!(all[1].operator, b"RG");
    }

    #[test]
    fn inline_image_with_wrong_length_falls_back() {
        // Déclare 2×2 gris (4 octets) mais n'en fournit que 2.
        let src = b"BI /W 2 /H 2 /CS /G /BPC 8 ID \x01\x02 EI Q";
        let all = parse_content(src).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].inline_image.as_ref().unwrap().data, b"\x01\x02");
    }

    #[test]
    fn unterminated_inline_image() {
        let all = parse_content(b"BI /W 1 /H 1 ID \x00").unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].inline_image.is_none());
    }
}
