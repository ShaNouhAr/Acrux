//! Parseur d'objets : assemble les tokens du [`Lexer`] en [`Object`]
//! (ISO 32000-2 §7.3).
//!
//! Le parseur est tolérant comme le lexer : un `]` ou `>>` manquant, un
//! `/Length` faux, un `endobj` absent ne bloquent pas la lecture. Les cas
//! irrécupérables remontent une erreur [`Error::Syntax`] avec l'offset.

use acrux_core::{Error, Result};

use crate::lexer::{is_whitespace, Lexer, Token};
use crate::objects::{Dict, Name, Object, ObjectRef};

/// Profondeur d'imbrication maximale (tableaux / dictionnaires) : au-delà on
/// considère le fichier hostile ou corrompu.
const MAX_DEPTH: usize = 512;

/// Résout la longueur d'un flux quand `/Length` est une référence indirecte.
/// Retourne `None` si la référence est inconnue : le parseur cherchera alors
/// `endstream` lui-même.
pub type LengthResolver<'r> = &'r dyn Fn(ObjectRef) -> Option<i64>;

/// Parseur positionnel.
#[derive(Debug, Clone)]
pub struct Parser<'a> {
    lexer: Lexer<'a>,
}

impl<'a> Parser<'a> {
    /// Parseur positionné à `pos` dans `data`.
    #[must_use]
    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Self {
            lexer: Lexer::at(data, pos),
        }
    }

    /// Parseur au début de `data`.
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self {
            lexer: Lexer::new(data),
        }
    }

    /// Position courante.
    #[must_use]
    pub const fn pos(&self) -> usize {
        self.lexer.pos()
    }

    /// Déplace le parseur.
    pub fn seek(&mut self, pos: usize) {
        self.lexer.seek(pos);
    }

    /// Accès au lexer sous-jacent (pour les flux de contenu).
    pub fn lexer_mut(&mut self) -> &mut Lexer<'a> {
        &mut self.lexer
    }

    /// Lit le token suivant.
    ///
    /// # Errors
    /// Voir [`Lexer::next_token`].
    pub fn next_token(&mut self) -> Result<Token> {
        self.lexer.next_token()
    }

    /// Regarde le token suivant sans le consommer.
    ///
    /// # Errors
    /// Voir [`Lexer::next_token`].
    pub fn peek_token(&mut self) -> Result<Token> {
        let save = self.lexer.pos();
        let t = self.lexer.next_token();
        self.lexer.seek(save);
        t
    }

    /// Parse un objet direct complet (nombre, chaîne, nom, tableau,
    /// dictionnaire, référence `n g R`, `true`, `false`, `null`).
    ///
    /// Un mot-clé inattendu (`obj`, `endobj`, opérateur de contenu…) est
    /// retourné dans `Err(Syntax)` : l'appelant décide quoi en faire.
    ///
    /// # Errors
    /// Syntaxe irrécupérable ou profondeur excessive.
    pub fn parse_object(&mut self) -> Result<Object> {
        self.parse_object_depth(0)
    }

    fn parse_object_depth(&mut self, depth: usize) -> Result<Object> {
        if depth > MAX_DEPTH {
            // `Corrupt` et non `Syntax` : les conteneurs ne doivent pas avaler
            // cette erreur, sinon ils boucleraient sans consommer de token.
            return Err(Error::Corrupt(format!(
                "imbrication trop profonde à l'octet {}",
                self.lexer.pos()
            )));
        }
        let start = self.lexer.pos();
        let tok = self.lexer.next_token()?;
        match tok {
            Token::Integer(n) => {
                // Lookahead pour `n g R` (§7.3.10). Les deux entiers doivent être ≥ 0.
                if n >= 0 {
                    let save = self.lexer.pos();
                    if let Ok(Token::Integer(g)) = self.lexer.next_token() {
                        if g >= 0 {
                            if let Ok(Token::Keyword(k)) = self.lexer.next_token() {
                                if k == b"R" {
                                    return Ok(Object::Reference(ObjectRef {
                                        number: u32::try_from(n).unwrap_or(u32::MAX),
                                        generation: u16::try_from(g).unwrap_or(u16::MAX),
                                    }));
                                }
                            }
                        }
                    }
                    self.lexer.seek(save);
                }
                Ok(Object::Integer(n))
            }
            Token::Real(r) => Ok(Object::Real(r)),
            Token::String(s) => Ok(Object::String(s)),
            Token::Name(n) => Ok(Object::Name(Name(n))),
            Token::ArrayOpen => self.parse_array_body(depth + 1),
            Token::DictOpen => self.parse_dict_body(depth + 1),
            Token::Keyword(k) => match k.as_slice() {
                b"true" => Ok(Object::Bool(true)),
                b"false" => Ok(Object::Bool(false)),
                b"null" => Ok(Object::Null),
                _ => Err(Error::Syntax {
                    offset: start as u64,
                    message: format!("mot-clé inattendu `{}`", String::from_utf8_lossy(&k)),
                }),
            },
            Token::ArrayClose | Token::DictClose | Token::BraceOpen | Token::BraceClose => {
                Err(Error::Syntax {
                    offset: start as u64,
                    message: "délimiteur inattendu".into(),
                })
            }
            Token::Eof => Err(Error::Syntax {
                offset: start as u64,
                message: "fin de données inattendue".into(),
            }),
        }
    }

    /// Corps d'un tableau, `[` déjà consommé (§7.3.6).
    fn parse_array_body(&mut self, depth: usize) -> Result<Object> {
        let mut items = Vec::new();
        loop {
            match self.peek_token()? {
                Token::ArrayClose => {
                    self.lexer.next_token()?;
                    break;
                }
                // Tableau non fermé : `>>`, `endobj`, `stream` ou EOF y mettent fin.
                Token::DictClose | Token::Eof => break,
                Token::Keyword(ref k) if is_structural_keyword(k) => break,
                _ => {}
            }
            match self.parse_object_depth(depth) {
                Ok(o) => items.push(o),
                // Élément illisible (mot-clé parasite, déjà consommé) : on
                // l'ignore pour ne pas perdre le reste du tableau, comme Acrobat.
                Err(Error::Syntax { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(Object::Array(items))
    }

    /// Corps d'un dictionnaire, `<<` déjà consommé (§7.3.7).
    fn parse_dict_body(&mut self, depth: usize) -> Result<Object> {
        let mut dict = Dict::new();
        loop {
            let tok = self.peek_token()?;
            match tok {
                Token::DictClose => {
                    self.lexer.next_token()?;
                    break;
                }
                Token::Eof => break,
                Token::Keyword(ref k) if is_structural_keyword(k) => break,
                Token::Name(key) => {
                    self.lexer.next_token()?;
                    // Une clé sans valeur juste avant `>>` : valeur nulle.
                    if matches!(self.peek_token()?, Token::DictClose) {
                        dict.insert(Name(key), Object::Null);
                        continue;
                    }
                    match self.parse_object_depth(depth) {
                        Ok(v) => {
                            // Une valeur `null` équivaut à une clé absente (§7.3.7).
                            if v != Object::Null {
                                dict.insert(Name(key), v);
                            }
                        }
                        // Valeur illisible (token déjà consommé) : clé ignorée.
                        Err(Error::Syntax { .. }) => {}
                        Err(e) => return Err(e),
                    }
                }
                _ => {
                    // Valeur sans clé (fichier corrompu) : on l'ignore.
                    self.lexer.next_token()?;
                }
            }
        }
        Ok(Object::Dict(dict))
    }

    /// Parse un objet indirect `n g obj … endobj` à la position courante
    /// (§7.3.10), y compris un flux (§7.3.8). Retourne la référence lue et l'objet.
    ///
    /// `resolve_length` sert quand `/Length` est une référence indirecte.
    ///
    /// # Errors
    /// Si l'en-tête `n g obj` est absent ou si l'objet est illisible.
    pub fn parse_indirect(
        &mut self,
        resolve_length: LengthResolver<'_>,
    ) -> Result<(ObjectRef, Object)> {
        let start = self.lexer.pos();
        let number = match self.lexer.next_token()? {
            Token::Integer(n) if n >= 0 => u32::try_from(n).unwrap_or(u32::MAX),
            _ => {
                return Err(Error::Syntax {
                    offset: start as u64,
                    message: "numéro d'objet attendu".into(),
                })
            }
        };
        let generation = match self.lexer.next_token()? {
            Token::Integer(g) if g >= 0 => u16::try_from(g).unwrap_or(u16::MAX),
            _ => {
                return Err(Error::Syntax {
                    offset: start as u64,
                    message: "numéro de génération attendu".into(),
                })
            }
        };
        match self.lexer.next_token()? {
            Token::Keyword(k) if k == b"obj" => {}
            _ => {
                return Err(Error::Syntax {
                    offset: start as u64,
                    message: "mot-clé `obj` attendu".into(),
                })
            }
        }
        let r = ObjectRef { number, generation };

        // Objet vide `n g obj endobj` : valeur nulle.
        let obj = match self.peek_token()? {
            Token::Keyword(ref k) if k == b"endobj" => Object::Null,
            _ => self.parse_object()?,
        };

        // Flux ?
        let obj = match (obj, self.peek_token()?) {
            (Object::Dict(dict), Token::Keyword(ref k)) if k == b"stream" => {
                self.lexer.next_token()?;
                self.lexer.skip_stream_eol();
                let data_start = self.lexer.pos();
                let raw = self.read_stream_data(data_start, &dict, resolve_length);
                Object::Stream { dict, raw }
            }
            (o, _) => o,
        };

        // `endobj` optionnel (souvent absent dans les fichiers cassés).
        if let Ok(Token::Keyword(k)) = self.peek_token() {
            if k == b"endobj" {
                self.lexer.next_token()?;
            }
        }
        Ok((r, obj))
    }

    /// Données brutes d'un flux dont le contenu commence à `data_start`.
    ///
    /// Stratégie (celle des lecteurs de référence) :
    /// 1. si `/Length` est valide et qu'`endstream` suit bien les données, on l'utilise ;
    /// 2. sinon on cherche `endstream` et on retire l'EOL qui le précède.
    fn read_stream_data(
        &mut self,
        data_start: usize,
        dict: &Dict,
        resolve_length: LengthResolver<'_>,
    ) -> Vec<u8> {
        let data = self.lexer.data();
        let declared = match dict.get(&Name::new("Length")) {
            Some(Object::Integer(n)) => Some(*n),
            Some(Object::Reference(r)) => resolve_length(*r),
            _ => None,
        };
        if let Some(len) = declared {
            if let Ok(len) = usize::try_from(len) {
                let end = data_start.saturating_add(len);
                if end <= data.len() && Self::endstream_follows(data, end) {
                    self.lexer.seek(end);
                    self.consume_endstream();
                    return data[data_start..end].to_vec();
                }
            }
        }
        // Longueur absente ou fausse : recherche d'`endstream`.
        let lx = Lexer::at(data, data_start);
        let end = match lx.find(b"endstream") {
            Some(e) => e,
            None => data.len(),
        };
        let mut trimmed = end;
        // Retire l'EOL final (CRLF, LF ou CR) qui précède `endstream` (§7.3.8.1).
        if trimmed > data_start && data[trimmed - 1] == b'\n' {
            trimmed -= 1;
        }
        if trimmed > data_start && data[trimmed - 1] == b'\r' {
            trimmed -= 1;
        }
        self.lexer.seek(end);
        self.consume_endstream();
        data[data_start..trimmed].to_vec()
    }

    /// Vrai si, après d'éventuels espacements, `endstream` commence à `pos`.
    fn endstream_follows(data: &[u8], mut pos: usize) -> bool {
        // Tolère jusqu'à quelques octets d'espacement (CRLF, espaces parasites).
        let limit = pos.saturating_add(4).min(data.len());
        while pos < limit && is_whitespace(data[pos]) {
            pos += 1;
        }
        data[pos..].starts_with(b"endstream")
    }

    fn consume_endstream(&mut self) {
        if let Ok(Token::Keyword(k)) = self.peek_token() {
            if k == b"endstream" {
                let _ = self.lexer.next_token();
            }
        }
    }
}

/// Mots-clés qui terminent implicitement un tableau / dictionnaire non fermé.
fn is_structural_keyword(k: &[u8]) -> bool {
    matches!(
        k,
        b"endobj" | b"stream" | b"endstream" | b"obj" | b"xref" | b"trailer" | b"startxref"
    )
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn parse(src: &[u8]) -> Object {
        Parser::new(src).parse_object().expect("parse")
    }

    fn name(s: &str) -> Object {
        Object::Name(Name::new(s))
    }

    #[test]
    fn scalars() {
        assert_eq!(parse(b"42"), Object::Integer(42));
        assert_eq!(parse(b"-1.5"), Object::Real(-1.5));
        assert_eq!(parse(b"true"), Object::Bool(true));
        assert_eq!(parse(b"false"), Object::Bool(false));
        assert_eq!(parse(b"null"), Object::Null);
        assert_eq!(parse(b"(x)"), Object::String(b"x".to_vec()));
        assert_eq!(parse(b"/N"), name("N"));
    }

    #[test]
    fn references_need_two_nonnegative_integers_and_r() {
        assert_eq!(
            parse(b"12 0 R"),
            Object::Reference(ObjectRef {
                number: 12,
                generation: 0
            })
        );
        // `1 2` sans R : deux entiers, on ne lit que le premier.
        let mut p = Parser::new(b"1 2 obj");
        assert_eq!(p.parse_object().unwrap(), Object::Integer(1));
        assert_eq!(p.parse_object().unwrap(), Object::Integer(2));
        // Nombre négatif : jamais une référence.
        let mut p = Parser::new(b"-1 0 R");
        assert_eq!(p.parse_object().unwrap(), Object::Integer(-1));
    }

    #[test]
    fn arrays_and_dicts() {
        assert_eq!(
            parse(b"[1 2.5 /X (s) [3] << /K 4 >> 5 0 R]"),
            Object::Array(vec![
                Object::Integer(1),
                Object::Real(2.5),
                name("X"),
                Object::String(b"s".to_vec()),
                Object::Array(vec![Object::Integer(3)]),
                Object::Dict(Dict::from([(Name::new("K"), Object::Integer(4))])),
                Object::Reference(ObjectRef {
                    number: 5,
                    generation: 0
                }),
            ])
        );
        let d = parse(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Null null >>");
        let d = d.as_dict().unwrap();
        assert_eq!(d.get(&Name::new("Type")), Some(&name("Page")));
        assert_eq!(d.len(), 3, "une valeur null équivaut à une clé absente");
        assert_eq!(
            d.get(&Name::new("MediaBox"))
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            4
        );
    }

    #[test]
    fn unterminated_containers_are_closed_by_structural_keywords() {
        let mut p = Parser::new(b"[1 2 endobj");
        assert_eq!(
            p.parse_object().unwrap(),
            Object::Array(vec![Object::Integer(1), Object::Integer(2)])
        );
        let mut p = Parser::new(b"<< /A 1 /B 2 stream");
        let d = p.parse_object().unwrap();
        assert_eq!(d.as_dict().unwrap().len(), 2);
        // Le mot-clé n'a pas été consommé.
        assert_eq!(p.next_token().unwrap(), Token::Keyword(b"stream".to_vec()));
    }

    #[test]
    fn garbage_inside_containers_is_skipped() {
        assert_eq!(
            parse(b"[1 foo 2]"),
            Object::Array(vec![Object::Integer(1), Object::Integer(2)])
        );
        let d = parse(b"<< /A 1 3 /B 2 /C >>");
        let d = d.as_dict().unwrap();
        assert_eq!(d.get(&Name::new("A")), Some(&Object::Integer(1)));
        assert_eq!(d.get(&Name::new("B")), Some(&Object::Integer(2)));
        assert_eq!(d.get(&Name::new("C")), Some(&Object::Null));
    }

    #[test]
    fn excessive_nesting_is_rejected() {
        let src = vec![b'['; 2000];
        assert!(matches!(
            Parser::new(&src).parse_object(),
            Err(Error::Corrupt(_))
        ));
        let src = b"<< /A ".repeat(2000);
        assert!(matches!(
            Parser::new(&src).parse_object(),
            Err(Error::Corrupt(_))
        ));
    }

    fn no_resolve(_: ObjectRef) -> Option<i64> {
        None
    }

    #[test]
    fn indirect_object_and_stream_with_valid_length() {
        let src = b"4 0 obj\n<< /Length 5 >>\nstream\r\nhello\r\nendstream\nendobj\n";
        let mut p = Parser::new(src);
        let (r, o) = p.parse_indirect(&no_resolve).unwrap();
        assert_eq!(
            r,
            ObjectRef {
                number: 4,
                generation: 0
            }
        );
        match o {
            Object::Stream { dict, raw } => {
                assert_eq!(dict.get(&Name::new("Length")), Some(&Object::Integer(5)));
                assert_eq!(raw, b"hello");
            }
            other => panic!("flux attendu, obtenu {other:?}"),
        }
        assert_eq!(p.next_token().unwrap(), Token::Eof, "endobj consommé");
    }

    #[test]
    fn stream_with_wrong_length_falls_back_to_endstream_search() {
        let src = b"4 0 obj << /Length 999 >> stream\nhello world\nendstream endobj";
        let (_, o) = Parser::new(src).parse_indirect(&no_resolve).unwrap();
        match o {
            Object::Stream { raw, .. } => assert_eq!(raw, b"hello world"),
            other => panic!("flux attendu, obtenu {other:?}"),
        }
        // Longueur trop courte.
        let src = b"4 0 obj << /Length 2 >> stream\nhello\nendstream endobj";
        let (_, o) = Parser::new(src).parse_indirect(&no_resolve).unwrap();
        match o {
            Object::Stream { raw, .. } => assert_eq!(raw, b"hello"),
            other => panic!("flux attendu, obtenu {other:?}"),
        }
    }

    #[test]
    fn stream_with_indirect_length() {
        let src = b"4 0 obj << /Length 9 0 R >> stream\nabc\nendstream endobj";
        let resolve = |r: ObjectRef| if r.number == 9 { Some(3) } else { None };
        let (_, o) = Parser::new(src).parse_indirect(&resolve).unwrap();
        match o {
            Object::Stream { raw, .. } => assert_eq!(raw, b"abc"),
            other => panic!("flux attendu, obtenu {other:?}"),
        }
    }

    #[test]
    fn stream_without_endstream_takes_rest_of_data() {
        let src = b"1 0 obj << >> stream\nabc";
        let (_, o) = Parser::new(src).parse_indirect(&no_resolve).unwrap();
        match o {
            Object::Stream { raw, .. } => assert_eq!(raw, b"abc"),
            other => panic!("flux attendu, obtenu {other:?}"),
        }
    }

    #[test]
    fn empty_indirect_object_is_null() {
        let (r, o) = Parser::new(b"7 3 obj endobj")
            .parse_indirect(&no_resolve)
            .unwrap();
        assert_eq!(r.generation, 3);
        assert_eq!(o, Object::Null);
    }

    #[test]
    fn missing_endobj_is_tolerated() {
        let mut p = Parser::new(b"1 0 obj 5 2 0 obj 6 endobj");
        assert_eq!(p.parse_indirect(&no_resolve).unwrap().1, Object::Integer(5));
        assert_eq!(p.parse_indirect(&no_resolve).unwrap().1, Object::Integer(6));
    }

    #[test]
    fn bad_indirect_header() {
        assert!(Parser::new(b"<< >>").parse_indirect(&no_resolve).is_err());
        assert!(Parser::new(b"1 0 R").parse_indirect(&no_resolve).is_err());
    }
}
