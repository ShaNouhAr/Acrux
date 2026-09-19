//! CMaps PDF : correspondance codes d'octets → CID pour les polices
//! composites (ISO 32000-2 §9.7.5) et CMaps ToUnicode (§9.10.3).
//!
//! Un CMap incorporé est un programme PostScript restreint (Adobe TN #5014)
//! dont seuls les opérateurs suivants nous intéressent :
//! `begincodespacerange`, `begincidrange`, `begincidchar`, `usecmap`,
//! `/WMode`, et pour ToUnicode `beginbfchar` / `beginbfrange`.
//! Le reste (dictionnaire CIDSystemInfo, `findresource`…) est ignoré.
//!
//! Le découpage d'une chaîne en codes suit §9.7.6.3 : on essaie les
//! longueurs 1 à 4 octets dans l'ordre jusqu'à trouver une plage de
//! codespace qui contienne le préfixe (comparaison octet par octet). Si
//! aucune ne convient, on consomme la longueur de la plage la plus courte
//! dont le premier octet correspond, sinon un octet, et le CID vaut 0.
//!
//! Seules les CMaps prédéfinies `Identity-H` et `Identity-V` sont fournies
//! ([`CMap::predefined`]) ; les CMaps CJK prédéfinies (UniGB-UCS2-H…)
//! viendront avec les ressources correspondantes.

use std::collections::HashMap;

use acrux_core::{Error, Result};

use crate::encodings::glyph_name_to_unicode;

/// Longueur maximale d'un code en octets (§9.7.6.2).
const MAX_CODE_BYTES: usize = 4;

/// Nombre maximal d'entrées dépliées par [`CMap::to_unicode_pairs`].
const MAX_REVERSE_ENTRIES: usize = 1 << 17;

/// Plage de codespace : intervalle multidimensionnel comparé octet par
/// octet (§9.7.6.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodespaceRange {
    /// Nombre d'octets des codes de cette plage (1 à 4).
    pub nbytes: u8,
    /// Borne basse, un octet par position.
    pub low: [u8; MAX_CODE_BYTES],
    /// Borne haute, un octet par position.
    pub high: [u8; MAX_CODE_BYTES],
}

impl CodespaceRange {
    fn contains(&self, code: &[u8]) -> bool {
        code.len() == usize::from(self.nbytes)
            && code
                .iter()
                .zip(self.low.iter().zip(self.high.iter()))
                .all(|(b, (lo, hi))| lo <= b && b <= hi)
    }
}

/// Plage `cidrange` : codes `lo..=hi` sur `nbytes` octets → CID consécutifs
/// à partir de `cid`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CidRange {
    nbytes: u8,
    lo: u32,
    hi: u32,
    cid: u32,
}

/// Plage `bfrange` : codes `lo..=hi` → chaînes UTF-16BE dont le dernier
/// octet est incrémenté.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BfRange {
    lo: u32,
    hi: u32,
    dst: Vec<u8>,
}

/// CMap (encodage d'une police composite ou table ToUnicode).
#[derive(Debug, Clone, Default)]
pub struct CMap {
    name: Option<String>,
    vertical: bool,
    codespaces: Vec<CodespaceRange>,
    /// Correspondances individuelles, clé `(nbytes << 32) | code`.
    cid_single: HashMap<u64, u32>,
    cid_ranges: Vec<CidRange>,
    bf_single: HashMap<u32, String>,
    bf_ranges: Vec<BfRange>,
    /// Longueur de code la plus fréquente parmi les correspondances,
    /// utilisée lorsque le CMap ne déclare aucun codespace.
    fallback_nbytes: u8,
}

/// Jeton du sous-ensemble PostScript des CMaps.
#[derive(Debug, Clone, PartialEq)]
enum Token {
    Hex(Vec<u8>),
    Number(f64),
    Name(String),
    Keyword(String),
    ArrayOpen,
    ArrayClose,
    /// Chaîne `(…)`, `<<`, `>>`, `{`, `}` : sans intérêt ici.
    Other,
}

/// Découpe un flux CMap en jetons (bornes : chaque jeton consomme au moins
/// un octet, la boucle termine donc toujours).
fn tokenize(data: &[u8]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        match b {
            b' ' | b'\t' | b'\r' | b'\n' | b'\x0C' | b'\0' => i += 1,
            b'%' => {
                while i < data.len() && data[i] != b'\n' && data[i] != b'\r' {
                    i += 1;
                }
            }
            b'<' => {
                if data.get(i + 1) == Some(&b'<') {
                    tokens.push(Token::Other);
                    i += 2;
                } else {
                    let (bytes, next) = read_hex(data, i + 1);
                    tokens.push(Token::Hex(bytes));
                    i = next;
                }
            }
            b'>' => {
                // `>>` ou `>` orphelin.
                i += if data.get(i + 1) == Some(&b'>') { 2 } else { 1 };
                tokens.push(Token::Other);
            }
            b'[' => {
                tokens.push(Token::ArrayOpen);
                i += 1;
            }
            b']' => {
                tokens.push(Token::ArrayClose);
                i += 1;
            }
            b'{' | b'}' => {
                tokens.push(Token::Other);
                i += 1;
            }
            b'(' => {
                i = skip_string(data, i + 1);
                tokens.push(Token::Other);
            }
            b'/' => {
                let (word, next) = read_word(data, i + 1);
                tokens.push(Token::Name(word));
                i = next;
            }
            _ => {
                let (word, next) = read_word(data, i);
                if next == i {
                    // Délimiteur isolé inattendu : on l'ignore.
                    i += 1;
                    continue;
                }
                i = next;
                match word.parse::<f64>() {
                    Ok(v) if v.is_finite() => tokens.push(Token::Number(v)),
                    _ => tokens.push(Token::Keyword(word)),
                }
            }
        }
    }
    tokens
}

fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t'
            | b'\r'
            | b'\n'
            | b'\x0C'
            | b'\0'
            | b'%'
            | b'<'
            | b'>'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'('
            | b')'
            | b'/'
    )
}

/// Lit un mot (nom ou mot-clé) à partir de `start`.
fn read_word(data: &[u8], start: usize) -> (String, usize) {
    let mut end = start;
    while end < data.len() && !is_delimiter(data[end]) {
        end += 1;
    }
    (String::from_utf8_lossy(&data[start..end]).into_owned(), end)
}

/// Lit une chaîne hexadécimale après `<` ; un nombre impair de chiffres est
/// complété par un zéro (ISO 32000-2 §7.3.4.3).
fn read_hex(data: &[u8], start: usize) -> (Vec<u8>, usize) {
    let mut bytes = Vec::new();
    let mut nibble: Option<u8> = None;
    let mut i = start;
    while i < data.len() {
        let b = data[i];
        i += 1;
        if b == b'>' {
            break;
        }
        let Some(v) = hex_value(b) else {
            continue;
        };
        match nibble.take() {
            Some(hi) => bytes.push((hi << 4) | v),
            None => nibble = Some(v),
        }
    }
    if let Some(hi) = nibble {
        bytes.push(hi << 4);
    }
    (bytes, i)
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Saute une chaîne littérale `(…)` avec parenthèses imbriquées et
/// échappements ; renvoie la position après la parenthèse fermante.
fn skip_string(data: &[u8], start: usize) -> usize {
    let mut depth = 1usize;
    let mut i = start;
    while i < data.len() && depth > 0 {
        match data[i] {
            b'\\' => i += 1,
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    i
}

/// Valeur entière big-endian d'un code (au plus 4 octets, les octets
/// excédentaires de tête sont ignorés).
fn code_value(bytes: &[u8]) -> u32 {
    let tail = if bytes.len() > MAX_CODE_BYTES {
        &bytes[bytes.len() - MAX_CODE_BYTES..]
    } else {
        bytes
    };
    tail.iter().fold(0u32, |acc, &b| (acc << 8) | u32::from(b))
}

/// Longueur d'un code bornée à 1..=4.
fn code_len(bytes: &[u8]) -> u8 {
    u8::try_from(bytes.len().clamp(1, MAX_CODE_BYTES)).unwrap_or(1)
}

/// Décode une destination `bf*` : UTF-16BE (§9.10.3), ou octets simples si
/// la longueur est impaire (ToUnicode non conformes rencontrés en pratique).
fn decode_bf_destination(bytes: &[u8]) -> String {
    if bytes.len() % 2 == 1 {
        return bytes.iter().map(|&b| char::from(b)).collect();
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    char::decode_utf16(units)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Incrémente une destination `bfrange` de `delta` (le dernier octet, avec
/// retenue sur les précédents, §9.10.3 : « the last byte is incremented »).
fn offset_bf_destination(dst: &[u8], delta: u32) -> Vec<u8> {
    let mut out = dst.to_vec();
    let mut carry = delta;
    for b in out.iter_mut().rev() {
        if carry == 0 {
            break;
        }
        let sum = u32::from(*b) + (carry & 0xFF);
        *b = (sum & 0xFF) as u8;
        carry = (carry >> 8) + (sum >> 8);
    }
    out
}

impl CMap {
    /// CMap vide (aucun code reconnu).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// CMap prédéfinie : seulement `Identity-H` et `Identity-V` pour
    /// l'instant (§9.7.5.2, tableau 116) ; `None` pour les autres.
    #[must_use]
    pub fn predefined(name: &str) -> Option<CMap> {
        match name {
            "Identity-H" => Some(Self::identity(false)),
            "Identity-V" => Some(Self::identity(true)),
            _ => None,
        }
    }

    /// Identity : codes de 2 octets, CID = code.
    fn identity(vertical: bool) -> CMap {
        CMap {
            name: Some(if vertical { "Identity-V" } else { "Identity-H" }.to_string()),
            vertical,
            codespaces: vec![CodespaceRange {
                nbytes: 2,
                low: [0, 0, 0, 0],
                high: [0xFF, 0xFF, 0, 0],
            }],
            cid_ranges: vec![CidRange {
                nbytes: 2,
                lo: 0,
                hi: 0xFFFF,
                cid: 0,
            }],
            fallback_nbytes: 2,
            ..Default::default()
        }
    }

    /// Analyse un flux CMap incorporé (encodage ou ToUnicode).
    ///
    /// # Errors
    /// `Error::Corrupt` si le flux ne contient aucune définition utilisable.
    pub fn parse(data: &[u8]) -> Result<CMap> {
        let tokens = tokenize(data);
        let mut cmap = CMap::new();
        let mut i = 0;
        while i < tokens.len() {
            let Token::Keyword(kw) = &tokens[i] else {
                i += 1;
                continue;
            };
            i += 1;
            match kw.as_str() {
                "begincodespacerange" => i = cmap.parse_codespace(&tokens, i),
                "begincidrange" => i = cmap.parse_cidrange(&tokens, i),
                "begincidchar" => i = cmap.parse_cidchar(&tokens, i),
                "beginbfchar" => i = cmap.parse_bfchar(&tokens, i),
                "beginbfrange" => i = cmap.parse_bfrange(&tokens, i),
                "usecmap" => {
                    if let Some(Token::Name(n)) = i.checked_sub(2).and_then(|k| tokens.get(k)) {
                        if let Some(parent) = CMap::predefined(n) {
                            cmap.merge_parent(&parent);
                        }
                    }
                }
                "def" => cmap.parse_def(&tokens, i - 1),
                _ => {}
            }
        }
        cmap.compute_fallback();
        if cmap.codespaces.is_empty()
            && cmap.cid_single.is_empty()
            && cmap.cid_ranges.is_empty()
            && cmap.bf_single.is_empty()
            && cmap.bf_ranges.is_empty()
        {
            return Err(Error::Corrupt("CMap sans aucune correspondance".into()));
        }
        Ok(cmap)
    }

    /// `/WMode 1 def`, `/CMapName /X def`.
    fn parse_def(&mut self, tokens: &[Token], def_index: usize) {
        let Some(key_index) = def_index.checked_sub(2) else {
            return;
        };
        match (&tokens[key_index], &tokens[key_index + 1]) {
            (Token::Name(k), Token::Number(v)) if k == "WMode" => {
                self.vertical = (*v - 1.0).abs() < 1e-9;
            }
            (Token::Name(k), Token::Name(v)) if k == "CMapName" => self.name = Some(v.clone()),
            _ => {}
        }
    }

    fn parse_codespace(&mut self, tokens: &[Token], mut i: usize) -> usize {
        while let (Some(Token::Hex(lo)), Some(Token::Hex(hi))) = (tokens.get(i), tokens.get(i + 1))
        {
            let n = usize::from(code_len(lo));
            let mut low = [0u8; MAX_CODE_BYTES];
            let mut high = [0u8; MAX_CODE_BYTES];
            for k in 0..n {
                low[k] = lo.get(k).copied().unwrap_or(0);
                high[k] = hi.get(k).copied().unwrap_or(0xFF);
            }
            self.codespaces.push(CodespaceRange {
                nbytes: code_len(lo),
                low,
                high,
            });
            i += 2;
        }
        i
    }

    fn parse_cidrange(&mut self, tokens: &[Token], mut i: usize) -> usize {
        while let (Some(Token::Hex(lo)), Some(Token::Hex(hi)), Some(Token::Number(cid))) =
            (tokens.get(i), tokens.get(i + 1), tokens.get(i + 2))
        {
            let (lo_v, hi_v) = (code_value(lo), code_value(hi));
            if lo_v <= hi_v && *cid >= 0.0 {
                self.cid_ranges.push(CidRange {
                    nbytes: code_len(lo),
                    lo: lo_v,
                    hi: hi_v,
                    cid: number_to_u32(*cid),
                });
            }
            i += 3;
        }
        i
    }

    fn parse_cidchar(&mut self, tokens: &[Token], mut i: usize) -> usize {
        while let (Some(Token::Hex(code)), Some(Token::Number(cid))) =
            (tokens.get(i), tokens.get(i + 1))
        {
            let key = single_key(code_len(code), code_value(code));
            self.cid_single.insert(key, number_to_u32(*cid));
            i += 2;
        }
        i
    }

    fn parse_bfchar(&mut self, tokens: &[Token], mut i: usize) -> usize {
        loop {
            let Some(Token::Hex(src)) = tokens.get(i) else {
                return i;
            };
            let dst = match tokens.get(i + 1) {
                Some(Token::Hex(d)) => decode_bf_destination(d),
                Some(Token::Name(n)) => glyph_name_to_unicode(n)
                    .map(String::from)
                    .unwrap_or_default(),
                _ => return i,
            };
            self.bf_single.insert(code_value(src), dst);
            i += 2;
        }
    }

    fn parse_bfrange(&mut self, tokens: &[Token], mut i: usize) -> usize {
        loop {
            let (Some(Token::Hex(lo)), Some(Token::Hex(hi))) = (tokens.get(i), tokens.get(i + 1))
            else {
                return i;
            };
            let (lo_v, hi_v) = (code_value(lo), code_value(hi));
            match tokens.get(i + 2) {
                Some(Token::Hex(dst)) => {
                    if lo_v <= hi_v {
                        self.bf_ranges.push(BfRange {
                            lo: lo_v,
                            hi: hi_v,
                            dst: dst.clone(),
                        });
                    }
                    i += 3;
                }
                Some(Token::ArrayOpen) => {
                    // Forme tableau : une destination par code (§9.10.3).
                    i += 3;
                    let mut code = lo_v;
                    while let Some(tok) = tokens.get(i) {
                        i += 1;
                        match tok {
                            Token::ArrayClose => break,
                            Token::Hex(d) if code <= hi_v => {
                                self.bf_single.insert(code, decode_bf_destination(d));
                                code = code.saturating_add(1);
                            }
                            _ => {}
                        }
                    }
                }
                _ => return i,
            }
        }
    }

    /// Hérite les définitions d'un CMap parent (`usecmap`).
    fn merge_parent(&mut self, parent: &CMap) {
        self.codespaces.extend(parent.codespaces.iter().cloned());
        self.cid_ranges.extend(parent.cid_ranges.iter().cloned());
        for (k, v) in &parent.cid_single {
            self.cid_single.entry(*k).or_insert(*v);
        }
    }

    /// Détermine la longueur de code de repli (voir `fallback_nbytes`).
    fn compute_fallback(&mut self) {
        let mut counts = [0usize; MAX_CODE_BYTES + 1];
        for r in &self.cid_ranges {
            counts[usize::from(r.nbytes)] += 1;
        }
        for k in self.cid_single.keys() {
            let n = usize::try_from(k >> 32).unwrap_or(0).min(MAX_CODE_BYTES);
            counts[n] += 1;
        }
        let best = (1..=MAX_CODE_BYTES)
            .max_by_key(|&n| counts[n])
            .filter(|&n| counts[n] > 0)
            .unwrap_or(if self.bf_single.is_empty() && self.bf_ranges.is_empty() {
                1
            } else {
                2
            });
        self.fallback_nbytes = u8::try_from(best).unwrap_or(1);
    }

    /// Nom déclaré par `/CMapName`, ou nom prédéfini.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Vrai pour une écriture verticale (`/WMode 1` ou `Identity-V`).
    #[must_use]
    pub const fn vertical(&self) -> bool {
        self.vertical
    }

    /// Plages de codespace déclarées.
    #[must_use]
    pub fn codespace_ranges(&self) -> &[CodespaceRange] {
        &self.codespaces
    }

    /// CID d'un code de `nbytes` octets ; 0 (`.notdef`) s'il n'est pas
    /// défini (§9.7.6.3).
    #[must_use]
    pub fn cid(&self, code: u32, nbytes: usize) -> u32 {
        let n = u8::try_from(nbytes.clamp(1, MAX_CODE_BYTES)).unwrap_or(1);
        if let Some(cid) = self.cid_single.get(&single_key(n, code)) {
            return *cid;
        }
        self.cid_ranges
            .iter()
            .find(|r| r.nbytes == n && r.lo <= code && code <= r.hi)
            .map_or(0, |r| r.cid.saturating_add(code - r.lo))
    }

    /// Texte Unicode associé à un code par un CMap ToUnicode.
    #[must_use]
    pub fn to_unicode(&self, code: u32) -> Option<String> {
        if let Some(s) = self.bf_single.get(&code) {
            return Some(s.clone());
        }
        self.bf_ranges
            .iter()
            .find(|r| r.lo <= code && code <= r.hi)
            .map(|r| decode_bf_destination(&offset_bf_destination(&r.dst, code - r.lo)))
    }

    /// Toutes les correspondances `code → texte` d'un CMap ToUnicode.
    ///
    /// Sert à construire la table inverse (texte → code) dont l'édition de
    /// texte a besoin pour réencoder une chaîne avec la police en place
    /// (`acrux_features::edit_text`). Les plages sont dépliées ; le nombre total
    /// d'entrées est borné (`MAX_REVERSE_ENTRIES`) pour les fichiers hostiles.
    #[must_use]
    pub fn to_unicode_pairs(&self) -> Vec<(u32, String)> {
        let mut out: Vec<(u32, String)> = self
            .bf_single
            .iter()
            .map(|(c, s)| (*c, s.clone()))
            .collect();
        for r in &self.bf_ranges {
            for code in r.lo..=r.hi {
                if out.len() >= MAX_REVERSE_ENTRIES {
                    break;
                }
                out.push((
                    code,
                    decode_bf_destination(&offset_bf_destination(&r.dst, code - r.lo)),
                ));
            }
        }
        out.sort_by_key(|(c, _)| *c);
        out
    }

    /// Longueur (en octets) du code commençant à `bytes[0]`, ou `None` si
    /// aucune plage de codespace ne contient un préfixe complet.
    fn match_codespace(&self, bytes: &[u8]) -> Option<usize> {
        (1..=MAX_CODE_BYTES.min(bytes.len()))
            .find(|&n| self.codespaces.iter().any(|cs| cs.contains(&bytes[..n])))
    }

    /// Longueur consommée pour un code invalide : plage la plus courte dont
    /// le premier octet correspond, sinon la longueur de repli.
    fn partial_match_len(&self, first: u8) -> usize {
        self.codespaces
            .iter()
            .filter(|cs| cs.low[0] <= first && first <= cs.high[0])
            .map(|cs| usize::from(cs.nbytes))
            .min()
            .unwrap_or_else(|| {
                if self.codespaces.is_empty() {
                    usize::from(self.fallback_nbytes)
                } else {
                    1
                }
            })
    }

    /// Découpe une chaîne d'octets en codes et renvoie `(code, cid, nbytes)`
    /// pour chacun (§9.7.6.3). Les derniers octets d'une chaîne tronquée
    /// forment un code court dont le CID vaut 0.
    #[must_use]
    pub fn decode(&self, bytes: &[u8]) -> Vec<(u32, u32, usize)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let rest = &bytes[i..];
            let n = self
                .match_codespace(rest)
                .unwrap_or_else(|| self.partial_match_len(rest[0]))
                .clamp(1, rest.len());
            let code = code_value(&rest[..n]);
            out.push((code, self.cid(code, n), n));
            i += n;
        }
        out
    }
}

fn single_key(nbytes: u8, code: u32) -> u64 {
    (u64::from(nbytes) << 32) | u64::from(code)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // borné avant conversion
fn number_to_u32(v: f64) -> u32 {
    v.clamp(0.0, f64::from(u32::MAX)) as u32
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const EMBEDDED: &[u8] = b"%!PS-Adobe-3.0 Resource-CMap
/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (Japan1) /Supplement 2 >> def
/CMapName /Test-H def
/WMode 0 def
2 begincodespacerange
<00> <80>
<8140> <9FFC>
endcodespacerange
1 begincidrange
<8140> <817E> 633
endcidrange
2 begincidchar
<20> 1
<41> 34
endcidchar
endcmap
CMapName currentdict /CMap defineresource pop
end end";

    #[test]
    fn embedded_two_byte_cmap() {
        let cmap = CMap::parse(EMBEDDED).unwrap();
        assert_eq!(cmap.name(), Some("Test-H"));
        assert!(!cmap.vertical());
        assert_eq!(cmap.codespace_ranges().len(), 2);
        let decoded = cmap.decode(&[0x41, 0x81, 0x42, 0x20, 0x81, 0x7E]);
        assert_eq!(
            decoded,
            vec![
                (0x41, 34, 1),
                (0x8142, 635, 2),
                (0x20, 1, 1),
                (0x817E, 695, 2)
            ]
        );
        // Code de 2 octets hors plage cidrange : CID 0.
        assert_eq!(cmap.decode(&[0x90, 0x40]), vec![(0x9040, 0, 2)]);
        // Octet invalide : la plage la plus courte dont le premier octet
        // convient est consommée, ici un seul octet.
        assert_eq!(
            cmap.decode(&[0xF0, 0x41]),
            vec![(0xF0, 0, 1), (0x41, 34, 1)]
        );
        // Chaîne tronquée au milieu d'un code à 2 octets.
        assert_eq!(cmap.decode(&[0x81]), vec![(0x81, 0, 1)]);
    }

    #[test]
    fn identity_h_and_v() {
        let h = CMap::predefined("Identity-H").unwrap();
        assert!(!h.vertical());
        assert_eq!(
            h.decode(&[0x12, 0x34, 0x00, 0x05]),
            vec![(0x1234, 0x1234, 2), (5, 5, 2)]
        );
        let v = CMap::predefined("Identity-V").unwrap();
        assert!(v.vertical());
        assert_eq!(v.cid(0xFFFF, 2), 0xFFFF);
        assert!(CMap::predefined("UniGB-UCS2-H").is_none());
    }

    #[test]
    fn usecmap_identity_and_wmode() {
        let src = b"/Identity-H usecmap /WMode 1 def 1 begincidchar <0005> 99 endcidchar";
        let cmap = CMap::parse(src).unwrap();
        assert!(cmap.vertical());
        assert_eq!(cmap.cid(5, 2), 99);
        assert_eq!(cmap.cid(6, 2), 6);
    }

    #[test]
    fn to_unicode_with_bfchar_and_bfrange_forms() {
        let src = b"/CIDInit /ProcSet findresource begin begincmap
1 begincodespacerange <0000> <FFFF> endcodespacerange
2 beginbfchar
<0003> <0020>
<0024> <D83DDE00>
endbfchar
3 beginbfrange
<0044> <0046> <0061>
<0050> <0052> [<0058> <00590059> <005A>]
<00FF> <00FF> /eacute
endbfrange
endcmap";
        let cmap = CMap::parse(src).unwrap();
        assert_eq!(cmap.to_unicode(3).as_deref(), Some(" "));
        assert_eq!(cmap.to_unicode(0x24).as_deref(), Some("\u{1F600}"));
        assert_eq!(cmap.to_unicode(0x44).as_deref(), Some("a"));
        assert_eq!(cmap.to_unicode(0x46).as_deref(), Some("c"));
        assert_eq!(cmap.to_unicode(0x50).as_deref(), Some("X"));
        assert_eq!(cmap.to_unicode(0x51).as_deref(), Some("YY"));
        assert_eq!(cmap.to_unicode(0x52).as_deref(), Some("Z"));
        assert_eq!(cmap.to_unicode(0x53), None);
        // La forme `/name` en destination n'est pas une forme bfrange
        // valide : l'entrée est ignorée sans erreur.
        assert_eq!(cmap.to_unicode(0xFF), None);
    }

    #[test]
    fn bfrange_carry_over_last_byte() {
        let src = b"1 beginbfrange <00> <10> <00FE> endbfrange";
        let cmap = CMap::parse(src).unwrap();
        assert_eq!(cmap.to_unicode(0).as_deref(), Some("\u{FE}"));
        assert_eq!(cmap.to_unicode(2).as_deref(), Some("\u{100}"));
    }

    #[test]
    fn bfchar_with_glyph_name_destination() {
        let src = b"1 beginbfchar <41> /eacute endbfchar";
        let cmap = CMap::parse(src).unwrap();
        assert_eq!(cmap.to_unicode(0x41).as_deref(), Some("\u{E9}"));
    }

    #[test]
    fn missing_codespace_falls_back_to_mapping_width() {
        let src = b"1 begincidrange <0000> <FFFF> 0 endcidrange";
        let cmap = CMap::parse(src).unwrap();
        assert_eq!(cmap.decode(&[0x00, 0x41]), vec![(0x41, 0x41, 2)]);
    }

    #[test]
    fn empty_and_garbage_inputs() {
        assert!(CMap::parse(b"").is_err());
        assert!(CMap::parse(b"hello (world) << /a 1 >> [ ] { } %comment").is_err());
        for n in 0..EMBEDDED.len() {
            if let Ok(c) = CMap::parse(&EMBEDDED[..n]) {
                let _ = c.decode(&EMBEDDED[..n.min(64)]);
                let _ = c.to_unicode(0x41);
            }
        }
        // Chaînes hexadécimales impaires, non fermées, plages inversées.
        let odd = b"1 begincodespacerange <0> <FFF> endcodespacerange 1 begincidrange <10> <00> 5 endcidrange 1 begincidchar <20> 7 endcidchar";
        let cmap = CMap::parse(odd).unwrap();
        assert_eq!(cmap.cid(0x20, 1), 7);
        assert_eq!(cmap.cid(0x05, 1), 0);
    }
}
