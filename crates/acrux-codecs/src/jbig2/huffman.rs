//! Tables de Huffman JBIG2 (T.88 annexe B) : tables standard B.1 à B.15,
//! tables personnalisées (segment de type 53, §7.4.13, B.2), affectation
//! des codes préfixes (B.3) et lecteur de bits associé (B.4).

use super::corrupt;
use acrux_core::Result;

/// Lecteur de bits, poids fort en tête (T.88 §B.4, §7.4.3.1.7).
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Position en bits.
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    pub(crate) fn read_bit(&mut self) -> u32 {
        let byte = self.data.get(self.pos / 8).copied().unwrap_or(0);
        let bit = (byte >> (7 - (self.pos % 8))) & 1;
        self.pos += 1;
        u32::from(bit)
    }

    /// Lit `n` bits (0 ≤ n ≤ 32).
    pub(crate) fn read_bits(&mut self, n: u32) -> u32 {
        let mut v: u32 = 0;
        for _ in 0..n {
            v = (v << 1) | self.read_bit();
        }
        v
    }

    /// Avance au prochain octet.
    pub(crate) fn align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }

    /// Position en octets (après alignement).
    pub(crate) fn byte_pos(&self) -> usize {
        self.pos.div_ceil(8)
    }

    /// Se place au début de l'octet `pos`.
    pub(crate) fn seek_byte(&mut self, pos: usize) {
        self.pos = pos * 8;
    }

    /// Vrai si la position dépasse les données.
    pub(crate) fn exhausted(&self) -> bool {
        self.pos > self.data.len() * 8
    }

    pub(crate) fn data(&self) -> &'a [u8] {
        self.data
    }
}

/// Ligne d'une table (B.2) : longueur du préfixe, longueur de plage, valeur
/// basse, et nature (plage normale, plage basse « lower range », OOB).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Line {
    pub prefix_len: u8,
    pub range_len: u8,
    pub low: i32,
    pub kind: LineKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineKind {
    Normal,
    /// Plage basse : valeur = low − bits lus (32 bits).
    Lower,
    Oob,
}

/// Table de Huffman prête au décodage (codes canoniques de B.3).
#[derive(Debug, Clone)]
pub(crate) struct Table {
    /// Lignes triées par (longueur, code).
    lines: Vec<Line>,
    /// Pour chaque longueur 1..=32 : premier code, indice de la première
    /// ligne dans `lines`, nombre de lignes.
    first_code: [u32; 33],
    first_index: [usize; 33],
    count: [usize; 33],
    /// Codes assignés, dans l'ordre de `lines` (pour l'encodeur de test).
    #[cfg(test)]
    codes: Vec<u32>,
}

impl Table {
    /// Construit la table et assigne les codes préfixes (B.3) dans l'ordre
    /// des lignes fournies ; les lignes de longueur 0 sont ignorées.
    ///
    /// # Errors
    ///
    /// `Error::Corrupt` si des longueurs dépassent 32 ou si le code est
    /// sursaturé.
    pub(crate) fn new(lines: &[Line]) -> Result<Table> {
        let mut len_count = [0usize; 33];
        for l in lines {
            if l.prefix_len > 32 || l.range_len > 32 {
                return Err(corrupt("table de Huffman invalide"));
            }
            if l.prefix_len > 0 {
                len_count[usize::from(l.prefix_len)] += 1;
            }
        }
        // B.3 : codes consécutifs par longueur croissante.
        let mut first_code = [0u32; 33];
        let mut cur_code: u64 = 0;
        let mut assigned: Vec<(u8, u32, Line)> = Vec::new();
        let mut prev_count = 0u64;
        for len in 1..=32u8 {
            cur_code = (cur_code + prev_count) << 1;
            if cur_code + len_count[usize::from(len)] as u64 > (1u64 << len) {
                return Err(corrupt("table de Huffman sursaturée"));
            }
            // cur_code < 2^32 : vérifié ci-dessus (len ≤ 32).
            #[allow(clippy::cast_possible_truncation)]
            {
                first_code[usize::from(len)] = cur_code as u32;
            }
            for (code, l) in (cur_code..).zip(lines.iter().filter(|l| l.prefix_len == len)) {
                #[allow(clippy::cast_possible_truncation)]
                assigned.push((len, code as u32, *l));
            }
            prev_count = len_count[usize::from(len)] as u64;
        }
        let mut first_index = [0usize; 33];
        let mut count = [0usize; 33];
        let mut sorted: Vec<Line> = Vec::with_capacity(assigned.len());
        for (i, (len, _, l)) in assigned.iter().enumerate() {
            if count[usize::from(*len)] == 0 {
                first_index[usize::from(*len)] = i;
            }
            count[usize::from(*len)] += 1;
            sorted.push(*l);
        }
        #[cfg(test)]
        let codes = lines
            .iter()
            .map(|l| {
                assigned
                    .iter()
                    .find(|(_, _, a)| a == l)
                    .map_or(0, |(_, c, _)| *c)
            })
            .collect();
        Ok(Table {
            lines: sorted,
            first_code,
            first_index,
            count,
            #[cfg(test)]
            codes,
        })
    }

    /// Décode une valeur : `Ok(Some(v))`, `Ok(None)` pour OOB.
    ///
    /// # Errors
    ///
    /// `Error::Corrupt` si aucun code ne correspond.
    pub(crate) fn decode(&self, reader: &mut BitReader<'_>) -> Result<Option<i32>> {
        let mut code: u32 = 0;
        for len in 1..=32usize {
            code = (code << 1) | reader.read_bit();
            let n = self.count[len];
            if n > 0 {
                let offset = code.wrapping_sub(self.first_code[len]) as usize;
                if code >= self.first_code[len] && offset < n {
                    let line = self.lines[self.first_index[len] + offset];
                    return Ok(match line.kind {
                        LineKind::Oob => None,
                        LineKind::Lower => {
                            let v = reader.read_bits(32);
                            Some(line.low.wrapping_sub(i32::try_from(v).unwrap_or(i32::MAX)))
                        }
                        LineKind::Normal => {
                            let v = reader.read_bits(u32::from(line.range_len));
                            Some(line.low.wrapping_add(i32::try_from(v).unwrap_or(i32::MAX)))
                        }
                    });
                }
            }
            if reader.exhausted() {
                break;
            }
        }
        Err(corrupt("code de Huffman inconnu"))
    }

    /// Code préfixe et longueur assignés à la ligne `i` (ordre de
    /// construction), pour l'encodeur de test.
    #[cfg(test)]
    pub(crate) fn code_of(&self, i: usize) -> u32 {
        self.codes[i]
    }
}

/// Ligne normale.
const fn n(prefix_len: u8, range_len: u8, low: i32) -> Line {
    Line {
        prefix_len,
        range_len,
        low,
        kind: LineKind::Normal,
    }
}

/// Ligne de plage basse (32 bits soustraits).
const fn lower(prefix_len: u8, low: i32) -> Line {
    Line {
        prefix_len,
        range_len: 32,
        low,
        kind: LineKind::Lower,
    }
}

const fn oob(prefix_len: u8) -> Line {
    Line {
        prefix_len,
        range_len: 0,
        low: 0,
        kind: LineKind::Oob,
    }
}

/// Lignes des tables standard B.1 à B.15 (T.88 §B.5), dans l'ordre
/// d'affectation des codes : plages normales croissantes, plage basse,
/// plage haute, OOB.
const B1: &[Line] = &[n(1, 4, 0), n(2, 8, 16), n(3, 16, 272), n(3, 32, 65808)];
const B2: &[Line] = &[
    n(1, 0, 0),
    n(2, 0, 1),
    n(3, 0, 2),
    n(4, 3, 3),
    n(5, 6, 11),
    n(6, 32, 75),
    oob(6),
];
const B3: &[Line] = &[
    n(8, 8, -256),
    n(1, 0, 0),
    n(2, 0, 1),
    n(3, 0, 2),
    n(4, 3, 3),
    n(5, 6, 11),
    lower(8, -257),
    n(7, 32, 75),
    oob(6),
];
const B4: &[Line] = &[
    n(1, 0, 1),
    n(2, 0, 2),
    n(3, 0, 3),
    n(4, 3, 4),
    n(5, 6, 12),
    n(5, 32, 76),
];
const B5: &[Line] = &[
    n(7, 8, -255),
    n(1, 0, 1),
    n(2, 0, 2),
    n(3, 0, 3),
    n(4, 3, 4),
    n(5, 6, 12),
    lower(7, -256),
    n(6, 32, 76),
];
const B6: &[Line] = &[
    n(5, 10, -2048),
    n(4, 9, -1024),
    n(4, 8, -512),
    n(4, 7, -256),
    n(5, 6, -128),
    n(5, 5, -64),
    n(4, 5, -32),
    n(2, 7, 0),
    n(3, 7, 128),
    n(3, 8, 256),
    n(4, 9, 512),
    n(4, 10, 1024),
    lower(6, -2049),
    n(6, 32, 2048),
];
const B7: &[Line] = &[
    n(4, 9, -1024),
    n(3, 8, -512),
    n(4, 7, -256),
    n(5, 6, -128),
    n(5, 5, -64),
    n(4, 5, -32),
    n(4, 9, 0),
    n(5, 10, 512),
    n(3, 10, 1536),
    lower(6, -1025),
    n(6, 32, 2560),
];
const B8: &[Line] = &[
    n(8, 3, -15),
    n(9, 1, -7),
    n(8, 1, -5),
    n(9, 0, -3),
    n(7, 0, -2),
    n(4, 0, -1),
    n(2, 1, 0),
    n(5, 0, 2),
    n(6, 0, 3),
    n(3, 4, 4),
    n(6, 1, 20),
    n(4, 4, 22),
    n(4, 5, 38),
    n(5, 6, 70),
    n(5, 7, 134),
    n(6, 7, 262),
    n(7, 8, 390),
    n(6, 10, 646),
    lower(9, -16),
    n(9, 32, 1670),
    oob(2),
];
const B9: &[Line] = &[
    n(8, 4, -31),
    n(9, 2, -15),
    n(8, 2, -11),
    n(9, 1, -7),
    n(7, 1, -5),
    n(4, 1, -3),
    n(3, 1, -1),
    n(3, 1, 1),
    n(5, 1, 3),
    n(6, 1, 5),
    n(3, 5, 7),
    n(6, 2, 39),
    n(4, 5, 43),
    n(4, 6, 75),
    n(5, 7, 139),
    n(5, 8, 267),
    n(6, 8, 523),
    n(7, 9, 779),
    n(6, 11, 1291),
    lower(9, -32),
    n(9, 32, 3339),
    oob(2),
];
const B10: &[Line] = &[
    n(7, 4, -21),
    n(8, 0, -5),
    n(7, 0, -4),
    n(5, 0, -3),
    n(2, 2, -2),
    n(5, 0, 2),
    n(6, 0, 3),
    n(7, 0, 4),
    n(8, 0, 5),
    n(2, 6, 6),
    n(5, 5, 70),
    n(6, 6, 102),
    n(7, 7, 166),
    n(8, 8, 294),
    n(9, 9, 550),
    n(10, 10, 1062),
    lower(9, -22),
    n(9, 32, 2086),
    oob(2),
];
const B11: &[Line] = &[
    n(1, 0, 0),
    n(2, 1, 1),
    n(4, 0, 3),
    n(4, 1, 4),
    n(5, 1, 6),
    n(5, 2, 8),
    n(6, 2, 12),
    n(7, 2, 16),
    n(7, 3, 20),
    n(7, 4, 28),
    n(7, 5, 44),
    n(7, 6, 76),
    n(7, 32, 140),
];
const B12: &[Line] = &[
    n(1, 0, 0),
    n(2, 0, 1),
    n(3, 1, 2),
    n(5, 0, 4),
    n(5, 1, 5),
    n(6, 1, 7),
    n(7, 0, 9),
    n(7, 1, 10),
    n(7, 2, 12),
    n(7, 3, 16),
    n(7, 4, 24),
    n(8, 5, 40),
    n(8, 32, 72),
];
const B13: &[Line] = &[
    n(1, 0, 0),
    n(3, 0, 1),
    n(4, 0, 2),
    n(5, 0, 3),
    n(4, 1, 4),
    n(3, 3, 6),
    n(6, 1, 14),
    n(6, 2, 16),
    n(6, 3, 20),
    n(6, 4, 28),
    n(6, 5, 44),
    n(7, 6, 76),
    n(7, 32, 140),
];
const B14: &[Line] = &[n(3, 0, -2), n(3, 0, -1), n(1, 0, 0), n(3, 0, 1), n(3, 0, 2)];
const B15: &[Line] = &[
    n(7, 4, -24),
    n(6, 2, -8),
    n(5, 1, -4),
    n(4, 0, -2),
    n(3, 0, -1),
    n(1, 0, 0),
    n(3, 0, 1),
    n(4, 0, 2),
    n(5, 1, 3),
    n(6, 2, 5),
    n(7, 4, 9),
    lower(7, -25),
    n(7, 32, 25),
];

/// Lignes de la table standard B.`number`.
pub(crate) fn standard_lines(number: u32) -> Option<&'static [Line]> {
    Some(match number {
        1 => B1,
        2 => B2,
        3 => B3,
        4 => B4,
        5 => B5,
        6 => B6,
        7 => B7,
        8 => B8,
        9 => B9,
        10 => B10,
        11 => B11,
        12 => B12,
        13 => B13,
        14 => B14,
        15 => B15,
        _ => return None,
    })
}

/// Table standard B.`number`.
///
/// # Errors
///
/// `Error::Corrupt` si le numéro n'existe pas.
pub(crate) fn standard(number: u32) -> Result<Table> {
    let lines = standard_lines(number).ok_or_else(|| corrupt("table standard inconnue"))?;
    Table::new(lines)
}

/// Nombre maximal de lignes acceptées dans une table personnalisée.
const MAX_CUSTOM_LINES: usize = 1 << 16;

/// Lit un segment de table personnalisée (§7.4.13, B.2).
///
/// # Errors
///
/// `Error::Corrupt` si le segment est tronqué ou incohérent.
pub(crate) fn parse_custom(data: &[u8]) -> Result<Table> {
    if data.len() < 9 {
        return Err(corrupt("table personnalisée tronquée"));
    }
    let flags = data[0];
    let has_oob = flags & 1 != 0;
    let prefix_size = u32::from((flags >> 1) & 7) + 1;
    let range_size = u32::from((flags >> 4) & 7) + 1;
    let low = i32::from_be_bytes([data[1], data[2], data[3], data[4]]);
    let high = i32::from_be_bytes([data[5], data[6], data[7], data[8]]);
    if low > high {
        return Err(corrupt("table personnalisée : bornes inversées"));
    }
    let mut reader = BitReader::new(&data[9..]);
    let mut lines = Vec::new();
    let mut cur: i64 = i64::from(low);
    while cur < i64::from(high) {
        if lines.len() >= MAX_CUSTOM_LINES || reader.exhausted() {
            return Err(corrupt("table personnalisée trop longue ou tronquée"));
        }
        #[allow(clippy::cast_possible_truncation)]
        let prefix_len = reader.read_bits(prefix_size) as u8;
        #[allow(clippy::cast_possible_truncation)]
        let range_len = reader.read_bits(range_size) as u8;
        if range_len > 32 {
            return Err(corrupt("table personnalisée : plage invalide"));
        }
        #[allow(clippy::cast_possible_truncation)]
        lines.push(n(prefix_len, range_len, cur as i32));
        cur += 1i64 << range_len;
    }
    #[allow(clippy::cast_possible_truncation)]
    let lower_len = reader.read_bits(prefix_size) as u8;
    lines.push(lower(lower_len, low.wrapping_sub(1)));
    #[allow(clippy::cast_possible_truncation)]
    let upper_len = reader.read_bits(prefix_size) as u8;
    lines.push(n(upper_len, 32, high));
    if has_oob {
        #[allow(clippy::cast_possible_truncation)]
        let oob_len = reader.read_bits(prefix_size) as u8;
        lines.push(oob(oob_len));
    }
    Table::new(&lines)
}

/// Lit la table des codes d'identifiants de symboles d'une région de texte
/// en codage de Huffman (§7.4.3.1.7) : longueurs des 35 codes de plages,
/// puis longueurs des codes de chaque symbole. Le lecteur est ensuite
/// aligné sur l'octet.
///
/// # Errors
///
/// `Error::Corrupt` si les longueurs sont incohérentes.
pub(crate) fn read_symbol_id_codes(reader: &mut BitReader<'_>, num_syms: usize) -> Result<Table> {
    let mut run_lines = Vec::with_capacity(35);
    for i in 0..35 {
        #[allow(clippy::cast_possible_truncation)]
        let len = reader.read_bits(4) as u8;
        run_lines.push(n(len, 0, i));
    }
    let run_table = Table::new(&run_lines)?;
    let mut lengths: Vec<u8> = Vec::with_capacity(num_syms);
    let mut prev: u8 = 0;
    while lengths.len() < num_syms {
        if reader.exhausted() {
            return Err(corrupt("codes de symboles tronqués"));
        }
        let code = run_table.decode(reader)?.unwrap_or(0);
        match code {
            0..=31 => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let len = code as u8;
                lengths.push(len);
                prev = len;
            }
            32 => {
                let repeat = reader.read_bits(2) + 3;
                for _ in 0..repeat {
                    lengths.push(prev);
                }
            }
            33 => {
                let repeat = reader.read_bits(3) + 3;
                lengths.resize(lengths.len() + repeat as usize, 0);
            }
            _ => {
                let repeat = reader.read_bits(7) + 11;
                lengths.resize(lengths.len() + repeat as usize, 0);
            }
        }
    }
    lengths.truncate(num_syms);
    let lines: Vec<Line> = lengths
        .iter()
        .enumerate()
        .map(|(i, &len)| n(len, 0, i32::try_from(i).unwrap_or(i32::MAX)))
        .collect();
    reader.align();
    Table::new(&lines)
}
