//! Lexer PDF : découpe un flux d'octets en tokens (ISO 32000-2 §7.2).
//!
//! Le lexer est volontairement tolérant : les fichiers réels contiennent des
//! nombres malformés (`--5`, `3.4.5`, `.`), des chaînes non fermées ou des
//! caractères hors spécification. Dans ces cas on produit le token le plus
//! plausible plutôt qu'une erreur, comme le font les lecteurs de référence.
//! Les seules erreurs remontées sont celles qui empêchent toute progression.
//!
//! Le même lexer sert pour la syntaxe du fichier (objets, xref) et pour les
//! flux de contenu (opérateurs), qui partagent la même grammaire lexicale.

use acrux_core::{Error, Result};

/// Token produit par le [`Lexer`].
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// Entier (§7.3.3). Les valeurs hors `i64` sont saturées.
    Integer(i64),
    /// Réel (§7.3.3).
    Real(f64),
    /// Chaîne littérale `(…)` ou hexadécimale `<…>` décodée en octets (§7.3.4).
    String(Vec<u8>),
    /// Nom `/…` avec les séquences `#xx` résolues (§7.3.5).
    Name(Vec<u8>),
    /// `[`
    ArrayOpen,
    /// `]`
    ArrayClose,
    /// `<<`
    DictOpen,
    /// `>>`
    DictClose,
    /// `{` (fonctions PostScript de type 4, §7.10.5)
    BraceOpen,
    /// `}`
    BraceClose,
    /// Mot-clé : `obj`, `endobj`, `stream`, `R`, `true`, `false`, `null`,
    /// ou un opérateur de flux de contenu (`BT`, `Tj`, `re`…).
    Keyword(Vec<u8>),
    /// Fin des données.
    Eof,
}

/// Vrai pour les six caractères d'espacement du PDF (§7.2.3, table 1).
#[must_use]
pub const fn is_whitespace(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

/// Vrai pour les dix caractères délimiteurs (§7.2.3, table 2).
#[must_use]
pub const fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// Caractère « régulier » : ni espacement ni délimiteur.
#[must_use]
pub const fn is_regular(b: u8) -> bool {
    !is_whitespace(b) && !is_delimiter(b)
}

/// Lexer positionnel sur une tranche d'octets.
///
/// Il ne possède pas les données : le document est mappé en mémoire et le
/// lexer se déplace dedans avec [`Lexer::seek`].
#[derive(Debug, Clone)]
pub struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    /// Crée un lexer au début des données.
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Crée un lexer positionné à `pos` (borné à la fin des données).
    #[must_use]
    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Self {
            data,
            pos: pos.min(data.len()),
        }
    }

    /// Position courante (octet depuis le début des données).
    #[must_use]
    pub const fn pos(&self) -> usize {
        self.pos
    }

    /// Déplace la position courante.
    pub fn seek(&mut self, pos: usize) {
        self.pos = pos.min(self.data.len());
    }

    /// Données sous-jacentes.
    #[must_use]
    pub const fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Octet courant sans avancer.
    #[must_use]
    pub fn peek_byte(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn byte_at(&self, offset: usize) -> Option<u8> {
        self.data.get(self.pos + offset).copied()
    }

    /// Saute espacements et commentaires (§7.2.4 : un commentaire va de `%` à la fin de ligne).
    pub fn skip_whitespace(&mut self) {
        while let Some(b) = self.peek_byte() {
            if is_whitespace(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(c) = self.peek_byte() {
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// Lit le token suivant.
    ///
    /// # Errors
    /// Seulement si une structure ne peut pas être lue du tout (cas rare : le
    /// lexer préfère un token dégradé à une erreur).
    pub fn next_token(&mut self) -> Result<Token> {
        self.skip_whitespace();
        let Some(b) = self.peek_byte() else {
            return Ok(Token::Eof);
        };
        match b {
            b'[' => {
                self.pos += 1;
                Ok(Token::ArrayOpen)
            }
            b']' => {
                self.pos += 1;
                Ok(Token::ArrayClose)
            }
            b'{' => {
                self.pos += 1;
                Ok(Token::BraceOpen)
            }
            b'}' => {
                self.pos += 1;
                Ok(Token::BraceClose)
            }
            b'<' => {
                if self.byte_at(1) == Some(b'<') {
                    self.pos += 2;
                    Ok(Token::DictOpen)
                } else {
                    self.pos += 1;
                    Ok(Token::String(self.read_hex_string()))
                }
            }
            b'>' => {
                if self.byte_at(1) == Some(b'>') {
                    self.pos += 2;
                    Ok(Token::DictClose)
                } else {
                    // `>` isolé : hors spécification, on l'ignore et on continue.
                    self.pos += 1;
                    self.next_token()
                }
            }
            b'(' => {
                self.pos += 1;
                Ok(Token::String(self.read_literal_string()))
            }
            b')' => {
                // `)` isolé : hors spécification, on l'ignore et on continue.
                self.pos += 1;
                self.next_token()
            }
            b'/' => {
                self.pos += 1;
                Ok(Token::Name(self.read_name()))
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => Ok(self.read_number()),
            _ => {
                let start = self.pos;
                while let Some(c) = self.peek_byte() {
                    if !is_regular(c) {
                        break;
                    }
                    self.pos += 1;
                }
                if self.pos == start {
                    // Octet délimiteur inattendu déjà traité ci-dessus ; sécurité anti-boucle.
                    return Err(Error::Syntax {
                        offset: start as u64,
                        message: format!("octet inattendu 0x{b:02X}"),
                    });
                }
                Ok(Token::Keyword(self.data[start..self.pos].to_vec()))
            }
        }
    }

    /// Nombre entier ou réel (§7.3.3), tolérant aux formes malformées
    /// rencontrées dans les fichiers réels (`--5`, `3.`, `.5`, `3.4.5`, `6-2`).
    fn read_number(&mut self) -> Token {
        let start = self.pos;
        let mut negative = false;
        // Plusieurs signes consécutifs sont acceptés : Acrobat lit `--5` comme `-5`
        // (tout signe moins rend le nombre négatif, il n'y a pas d'annulation).
        while let Some(c) = self.peek_byte() {
            match c {
                b'-' => {
                    negative = true;
                    self.pos += 1;
                }
                b'+' => self.pos += 1,
                _ => break,
            }
        }
        let mut int_part: i64 = 0;
        let mut int_overflow = false;
        let mut has_digits = false;
        while let Some(c @ b'0'..=b'9') = self.peek_byte() {
            has_digits = true;
            let digit = i64::from(c - b'0');
            match int_part.checked_mul(10).and_then(|v| v.checked_add(digit)) {
                Some(v) => int_part = v,
                None => int_overflow = true,
            }
            self.pos += 1;
        }
        let mut is_real = false;
        let mut frac: f64 = 0.0;
        let mut scale: f64 = 1.0;
        if self.peek_byte() == Some(b'.') {
            is_real = true;
            self.pos += 1;
            while let Some(c) = self.peek_byte() {
                match c {
                    b'0'..=b'9' => {
                        has_digits = true;
                        scale /= 10.0;
                        frac += f64::from(c - b'0') * scale;
                        self.pos += 1;
                    }
                    // Un second point (`3.4.5`) ou un signe interne (`3.4-2`) : on ignore
                    // la suite du mot, comme Acrobat.
                    b'.' | b'-' | b'+' => {
                        while let Some(r) = self.peek_byte() {
                            if !is_regular(r) {
                                break;
                            }
                            self.pos += 1;
                        }
                        break;
                    }
                    _ => break,
                }
            }
        } else if matches!(self.peek_byte(), Some(b'-' | b'+')) {
            // `6-2` : Acrobat lit 6 et ignore le reste.
            while let Some(c) = self.peek_byte() {
                if !is_regular(c) {
                    break;
                }
                self.pos += 1;
            }
        }
        if !has_digits {
            // `.`, `-`, `+-` seuls : la spécification ne les autorise pas ; Acrobat lit 0.
            let _ = start;
            return if is_real {
                Token::Real(0.0)
            } else {
                Token::Integer(0)
            };
        }
        if is_real || int_overflow {
            #[allow(clippy::cast_precision_loss)]
            let mut v = int_part as f64 + frac;
            if int_overflow {
                // Dépassement : on relit la partie entière en flottant.
                v = self.data[start..self.pos]
                    .iter()
                    .filter(|c| c.is_ascii_digit())
                    .fold(0.0_f64, |acc, c| acc * 10.0 + f64::from(c - b'0'));
                if self.data[start..self.pos].contains(&b'.') {
                    v += frac;
                }
            }
            Token::Real(if negative { -v } else { v })
        } else {
            Token::Integer(if negative { -int_part } else { int_part })
        }
    }

    /// Chaîne littérale (§7.3.4.2). Le `(` ouvrant est déjà consommé.
    /// Gère les parenthèses équilibrées, les séquences d'échappement, les
    /// codes octaux `\ddd` et la continuation de ligne `\<EOL>`.
    fn read_literal_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut depth = 1usize;
        while let Some(b) = self.peek_byte() {
            self.pos += 1;
            match b {
                b'\\' => {
                    let Some(e) = self.peek_byte() else { break };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0C),
                        b'(' => out.push(b'('),
                        b')' => out.push(b')'),
                        b'\\' => out.push(b'\\'),
                        b'\r' => {
                            // Continuation de ligne : `\` suivi de CR ou CRLF.
                            if self.peek_byte() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            // Jusqu'à trois chiffres octaux ; dépassement tronqué à 8 bits (§7.3.4.2).
                            let mut v: u32 = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.peek_byte() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            #[allow(clippy::cast_possible_truncation)]
                            out.push((v & 0xFF) as u8);
                        }
                        // Barre oblique devant un autre caractère : elle est ignorée.
                        other => out.push(other),
                    }
                }
                b'(' => {
                    depth += 1;
                    out.push(b'(');
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(b')');
                }
                b'\r' => {
                    // Tout EOL non échappé est normalisé en LF (§7.3.4.2).
                    if self.peek_byte() == Some(b'\n') {
                        self.pos += 1;
                    }
                    out.push(b'\n');
                }
                other => out.push(other),
            }
        }
        out
    }

    /// Chaîne hexadécimale (§7.3.4.3). Le `<` ouvrant est déjà consommé.
    /// Les espacements sont ignorés ; un nombre impair de chiffres est complété par 0 ;
    /// un caractère non hexadécimal est ignoré (tolérance).
    fn read_hex_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut hi: Option<u8> = None;
        while let Some(b) = self.peek_byte() {
            self.pos += 1;
            if b == b'>' {
                break;
            }
            let Some(v) = hex_value(b) else { continue };
            match hi.take() {
                None => hi = Some(v),
                Some(h) => out.push(h << 4 | v),
            }
        }
        if let Some(h) = hi {
            out.push(h << 4);
        }
        out
    }

    /// Nom (§7.3.5). Le `/` est déjà consommé. `#xx` est décodé ; un `#` non
    /// suivi de deux chiffres hexadécimaux est conservé tel quel (tolérance).
    fn read_name(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(b) = self.peek_byte() {
            if !is_regular(b) {
                break;
            }
            self.pos += 1;
            if b == b'#' {
                if let (Some(h), Some(l)) = (
                    self.peek_byte().and_then(hex_value),
                    self.byte_at(1).and_then(hex_value),
                ) {
                    out.push(h << 4 | l);
                    self.pos += 2;
                    continue;
                }
            }
            out.push(b);
        }
        out
    }

    /// Après le mot-clé `stream`, positionne le lexer au premier octet des
    /// données du flux : la spécification impose CRLF ou LF (§7.3.8.1), mais
    /// certains fichiers n'ont que CR ou des espaces.
    pub fn skip_stream_eol(&mut self) {
        while self.peek_byte() == Some(b' ') || self.peek_byte() == Some(b'\t') {
            self.pos += 1;
        }
        match self.peek_byte() {
            Some(b'\r') => {
                self.pos += 1;
                if self.peek_byte() == Some(b'\n') {
                    self.pos += 1;
                }
            }
            Some(b'\n') => self.pos += 1,
            _ => {}
        }
    }

    /// Cherche `needle` à partir de la position courante et retourne son offset.
    #[must_use]
    pub fn find(&self, needle: &[u8]) -> Option<usize> {
        if needle.is_empty() {
            return Some(self.pos);
        }
        self.data[self.pos..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| p + self.pos)
    }

    /// Cherche `needle` en arrière depuis la fin des données (pour `startxref`, `%%EOF`).
    #[must_use]
    pub fn rfind(&self, needle: &[u8]) -> Option<usize> {
        if needle.is_empty() || needle.len() > self.data.len() {
            return None;
        }
        self.data.windows(needle.len()).rposition(|w| w == needle)
    }
}

/// Valeur d'un chiffre hexadécimal ASCII.
#[must_use]
pub const fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp
)]
mod tests {
    use super::*;

    fn tokens(src: &[u8]) -> Vec<Token> {
        let mut lx = Lexer::new(src);
        let mut out = Vec::new();
        loop {
            let t = lx.next_token().expect("lexing");
            if t == Token::Eof {
                break;
            }
            out.push(t);
        }
        out
    }

    fn kw(s: &str) -> Token {
        Token::Keyword(s.as_bytes().to_vec())
    }

    #[test]
    fn integers_and_reals() {
        assert_eq!(
            tokens(b"123 43445 +17 -98 0 34.5 -3.62 +123.6 4. -.002 0.0"),
            vec![
                Token::Integer(123),
                Token::Integer(43445),
                Token::Integer(17),
                Token::Integer(-98),
                Token::Integer(0),
                Token::Real(34.5),
                Token::Real(-3.62),
                Token::Real(123.6),
                Token::Real(4.0),
                Token::Real(-0.002),
                Token::Real(0.0),
            ]
        );
    }

    #[test]
    fn malformed_numbers_are_tolerated() {
        assert_eq!(tokens(b"--5"), vec![Token::Integer(-5)]);
        assert_eq!(tokens(b"-"), vec![Token::Integer(0)]);
        assert_eq!(tokens(b"."), vec![Token::Real(0.0)]);
        assert_eq!(tokens(b"3.4.5"), vec![Token::Real(3.4)]);
        assert_eq!(tokens(b"6-2"), vec![Token::Integer(6)]);
        // Dépassement i64 : lu en réel.
        match tokens(b"99999999999999999999")[0] {
            Token::Real(v) => assert!(v > 9.9e19),
            ref t => panic!("attendu un réel, obtenu {t:?}"),
        }
    }

    #[test]
    fn literal_strings() {
        assert_eq!(tokens(b"(Hello)"), vec![Token::String(b"Hello".to_vec())]);
        assert_eq!(tokens(b"(a(b)c)"), vec![Token::String(b"a(b)c".to_vec())]);
        assert_eq!(
            tokens(b"(\\n\\r\\t\\b\\f\\(\\)\\\\)"),
            vec![Token::String(b"\n\r\t\x08\x0C()\\".to_vec())]
        );
        assert_eq!(
            tokens(b"(\\101\\53\\0053)"),
            vec![Token::String(b"A+\x053".to_vec())]
        );
        // Continuation de ligne et normalisation des fins de ligne.
        assert_eq!(
            tokens(b"(ab\\\r\ncd\r\nef\rgh)"),
            vec![Token::String(b"abcd\nef\ngh".to_vec())]
        );
        // `\` devant un caractère quelconque est ignoré.
        assert_eq!(tokens(b"(\\q)"), vec![Token::String(b"q".to_vec())]);
        // Chaîne non fermée : on prend tout jusqu'à la fin.
        assert_eq!(tokens(b"(abc"), vec![Token::String(b"abc".to_vec())]);
    }

    #[test]
    fn hex_strings() {
        assert_eq!(
            tokens(b"<901FA3>"),
            vec![Token::String(vec![0x90, 0x1F, 0xA3])]
        );
        assert_eq!(
            tokens(b"<901FA>"),
            vec![Token::String(vec![0x90, 0x1F, 0xA0])]
        );
        assert_eq!(
            tokens(b"< 90 1f\nA3 >"),
            vec![Token::String(vec![0x90, 0x1F, 0xA3])]
        );
        assert_eq!(tokens(b"<>"), vec![Token::String(vec![])]);
        assert_eq!(tokens(b"<9zz0>"), vec![Token::String(vec![0x90])]);
    }

    #[test]
    fn names() {
        assert_eq!(tokens(b"/Type"), vec![Token::Name(b"Type".to_vec())]);
        assert_eq!(tokens(b"/A#20B#2Fc"), vec![Token::Name(b"A B/c".to_vec())]);
        assert_eq!(tokens(b"/"), vec![Token::Name(vec![])]);
        assert_eq!(tokens(b"/a#zz"), vec![Token::Name(b"a#zz".to_vec())]);
        assert_eq!(
            tokens(b"/Name1/Name2"),
            vec![
                Token::Name(b"Name1".to_vec()),
                Token::Name(b"Name2".to_vec())
            ]
        );
        assert_eq!(
            tokens(b"/A.B;C_D-E*F?"),
            vec![Token::Name(b"A.B;C_D-E*F?".to_vec())]
        );
    }

    #[test]
    fn delimiters_keywords_and_comments() {
        assert_eq!(
            tokens(b"<< /K [1 2] >> % commentaire\n{ } 1 0 R obj endobj stream true null"),
            vec![
                Token::DictOpen,
                Token::Name(b"K".to_vec()),
                Token::ArrayOpen,
                Token::Integer(1),
                Token::Integer(2),
                Token::ArrayClose,
                Token::DictClose,
                Token::BraceOpen,
                Token::BraceClose,
                Token::Integer(1),
                Token::Integer(0),
                kw("R"),
                kw("obj"),
                kw("endobj"),
                kw("stream"),
                kw("true"),
                kw("null"),
            ]
        );
    }

    #[test]
    fn content_stream_operators() {
        assert_eq!(
            tokens(b"BT /F1 12 Tf 72 712 Td (Hi) Tj ET q 1 0 0 1 0 0 cm Q"),
            vec![
                kw("BT"),
                Token::Name(b"F1".to_vec()),
                Token::Integer(12),
                kw("Tf"),
                Token::Integer(72),
                Token::Integer(712),
                kw("Td"),
                Token::String(b"Hi".to_vec()),
                kw("Tj"),
                kw("ET"),
                kw("q"),
                Token::Integer(1),
                Token::Integer(0),
                Token::Integer(0),
                Token::Integer(1),
                Token::Integer(0),
                Token::Integer(0),
                kw("cm"),
                kw("Q"),
            ]
        );
    }

    #[test]
    fn stray_delimiters_are_skipped() {
        assert_eq!(
            tokens(b") 1 > 2"),
            vec![Token::Integer(1), Token::Integer(2)]
        );
    }

    #[test]
    fn whitespace_set_matches_spec() {
        for b in [0x00, 0x09, 0x0A, 0x0C, 0x0D, 0x20] {
            assert!(is_whitespace(b), "0x{b:02X} doit être un espacement");
        }
        assert!(!is_whitespace(0x0B));
        assert_eq!(tokens(b"\x00\t\n\x0C\r 7"), vec![Token::Integer(7)]);
    }

    #[test]
    fn stream_eol_and_search() {
        let src = b"<< /Length 3 >>\nstream\r\nabc\nendstream";
        let mut lx = Lexer::new(src);
        loop {
            match lx.next_token().unwrap() {
                Token::Keyword(k) if k == b"stream" => break,
                Token::Eof => panic!("stream introuvable"),
                _ => {}
            }
        }
        lx.skip_stream_eol();
        assert_eq!(&src[lx.pos()..lx.pos() + 3], b"abc");
        assert_eq!(lx.find(b"endstream"), Some(src.len() - 9));
        assert_eq!(lx.rfind(b"stream"), Some(src.len() - 6));
        assert_eq!(lx.find(b"zzz"), None);
    }

    #[test]
    fn seek_is_bounded() {
        let mut lx = Lexer::new(b"12");
        lx.seek(100);
        assert_eq!(lx.pos(), 2);
        assert_eq!(lx.next_token().unwrap(), Token::Eof);
    }
}
