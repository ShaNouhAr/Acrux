//! Compresseur DEFLATE (RFC 1951) avec enveloppe zlib (RFC 1950), écrit de
//! zéro : recherche de correspondances LZ77 par chaînes de hachage (fenêtre
//! de 32 Ko, correspondances de 3 à 258 octets, évaluation paresseuse aux
//! niveaux élevés) puis codage de Huffman dynamique par bloc, avec repli sur
//! les codes fixes ou un bloc « stored » quand ils sont plus courts.
//!
//! Le résultat se relit avec [`super::decode`] et avec tout décodeur zlib.
//! Le compromis vitesse / taux est réglé par un niveau de 1 (rapide) à 9
//! (chaînes longues, évaluation paresseuse systématique).

// Arithmétique d'indices et de fréquences : bornée par la taille de fenêtre
// (32 768) et les tailles d'alphabet de la norme.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use super::adler32;

/// Taille de la fenêtre de recherche (RFC 1951 §2 : 32 Ko au maximum).
const WINDOW: usize = 32 * 1024;
/// Masque de la fenêtre (puissance de deux).
const WINDOW_MASK: usize = WINDOW - 1;
/// Taille de la table de hachage.
const HASH_SIZE: usize = 1 << 15;
/// Longueur minimale d'une correspondance.
const MIN_MATCH: usize = 3;
/// Longueur maximale d'une correspondance.
const MAX_MATCH: usize = 258;
/// Nombre de jetons par bloc avant émission.
const BLOCK_TOKENS: usize = 16 * 1024;

/// Bases des codes de longueur 257..=285 (tableau de la RFC 1951 §3.2.5).
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
/// Bits supplémentaires des codes de longueur.
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// Bases des codes de distance 0..=29.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
/// Bits supplémentaires des codes de distance.
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// Ordre de transmission des longueurs de l'alphabet des longueurs de code.
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Jeton LZ77.
#[derive(Debug, Clone, Copy)]
enum Token {
    /// Octet littéral.
    Literal(u8),
    /// Correspondance (longueur 3..=258, distance 1..=32768).
    Match { length: u16, distance: u16 },
}

/// Paramètres dérivés du niveau.
struct Params {
    /// Nombre maximal de maillons de chaîne examinés par position.
    max_chain: usize,
    /// Longueur « suffisante » au-delà de laquelle on arrête la recherche.
    good_length: usize,
    /// Évaluation paresseuse (essayer la position suivante avant d'émettre).
    lazy: bool,
}

impl Params {
    fn for_level(level: u8) -> Self {
        match level {
            0 | 1 => Params {
                max_chain: 4,
                good_length: 8,
                lazy: false,
            },
            2 => Params {
                max_chain: 8,
                good_length: 16,
                lazy: false,
            },
            3 => Params {
                max_chain: 16,
                good_length: 32,
                lazy: false,
            },
            4 => Params {
                max_chain: 16,
                good_length: 32,
                lazy: true,
            },
            5 => Params {
                max_chain: 32,
                good_length: 64,
                lazy: true,
            },
            6 => Params {
                max_chain: 64,
                good_length: 128,
                lazy: true,
            },
            7 => Params {
                max_chain: 128,
                good_length: 258,
                lazy: true,
            },
            8 => Params {
                max_chain: 512,
                good_length: 258,
                lazy: true,
            },
            _ => Params {
                max_chain: 2048,
                good_length: 258,
                lazy: true,
            },
        }
    }
}

/// Compresse `data` en un flux zlib (en-tête, blocs deflate, Adler-32).
/// `level` va de 1 (rapide) à 9 (compact) ; 0 produit des blocs stored.
#[must_use]
pub fn compress(data: &[u8], level: u8) -> Vec<u8> {
    if level == 0 {
        return super::encode(data);
    }
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    // CMF = 0x78 (deflate, fenêtre 32 Ko) ; FLG selon le niveau, FCHECK tel
    // que CMF·256 + FLG soit multiple de 31 (RFC 1950 §2.2).
    let flevel: u16 = match level {
        1 => 0,
        2..=5 => 1,
        6 => 2,
        _ => 3,
    };
    let cmf: u16 = 0x78;
    let mut flg = flevel << 6;
    let rem = (cmf * 256 + flg) % 31;
    if rem != 0 {
        flg += 31 - rem;
    }
    out.push(cmf as u8);
    out.push(flg as u8);
    let mut writer = BitWriter::new(out);
    deflate(data, &Params::for_level(level), &mut writer);
    let mut out = writer.finish();
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Écrit les blocs deflate de `data` (sans enveloppe).
#[allow(clippy::too_many_lines, clippy::many_single_char_names)] // boucle LZ77 avec ses fermetures
fn deflate(data: &[u8], params: &Params, w: &mut BitWriter) {
    if data.is_empty() {
        // Un bloc final vide en codes fixes : BFINAL=1, BTYPE=01, code 256.
        w.bits(1, 1);
        w.bits(1, 2);
        w.bits_rev(0, 7);
        return;
    }
    let mut head = vec![u32::MAX; HASH_SIZE];
    let mut prev = vec![u32::MAX; WINDOW];
    let mut tokens: Vec<Token> = Vec::with_capacity(BLOCK_TOKENS);
    let mut block_start = 0usize;
    let mut pos = 0usize;
    let n = data.len();
    let hash = |i: usize| -> usize {
        if i + 2 >= n {
            return usize::MAX;
        }
        let v = (u32::from(data[i]) << 16) | (u32::from(data[i + 1]) << 8) | u32::from(data[i + 2]);
        (v.wrapping_mul(2_654_435_761) >> 17) as usize & (HASH_SIZE - 1)
    };
    let insert = |i: usize, head: &mut [u32], prev: &mut [u32]| {
        let h = hash(i);
        if h != usize::MAX {
            prev[i & WINDOW_MASK] = head[h];
            head[h] = i as u32;
        }
    };
    // Recherche de la meilleure correspondance à la position `i`.
    let find = |i: usize, head: &[u32], prev: &[u32], min_len: usize| -> (usize, usize) {
        let h = hash(i);
        if h == usize::MAX {
            return (0, 0);
        }
        let mut best_len = 0usize;
        let mut best_dist = 0usize;
        let max_len = (n - i).min(MAX_MATCH);
        let mut cand = head[h];
        let mut chain = params.max_chain;
        while cand != u32::MAX && chain > 0 {
            let c = cand as usize;
            if c >= i || i - c > WINDOW {
                break;
            }
            // Test rapide sur l'octet qui améliorerait la meilleure longueur.
            if best_len == 0 || data[c + best_len] == data[i + best_len] {
                let mut l = 0;
                while l < max_len && data[c + l] == data[i + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_dist = i - c;
                    if l >= params.good_length || l == max_len {
                        break;
                    }
                }
            }
            let next = prev[c & WINDOW_MASK];
            if next != u32::MAX && next as usize >= c {
                break;
            }
            cand = next;
            chain -= 1;
        }
        if best_len < min_len.max(MIN_MATCH) {
            (0, 0)
        } else {
            (best_len, best_dist)
        }
    };
    while pos < n {
        let (mut len, mut dist) = find(pos, &head, &prev, MIN_MATCH);
        // Évaluation paresseuse : si la position suivante offre mieux, émettre
        // un littéral ici et prendre la correspondance suivante.
        if params.lazy && len > 0 && len < params.good_length && pos + 1 < n {
            insert(pos, &mut head, &mut prev);
            let (len2, dist2) = find(pos + 1, &head, &prev, len + 1);
            if len2 > len {
                tokens.push(Token::Literal(data[pos]));
                pos += 1;
                len = len2;
                dist = dist2;
            } else {
                // On a déjà inséré `pos` : éviter de le réinsérer plus bas.
                emit_match(&mut tokens, len, dist);
                for k in pos + 1..pos + len {
                    insert(k, &mut head, &mut prev);
                }
                pos += len;
                if tokens.len() >= BLOCK_TOKENS {
                    flush_block(data, block_start, pos, &tokens, false, w);
                    tokens.clear();
                    block_start = pos;
                }
                continue;
            }
        }
        if len == 0 {
            tokens.push(Token::Literal(data[pos]));
            insert(pos, &mut head, &mut prev);
            pos += 1;
        } else {
            emit_match(&mut tokens, len, dist);
            for k in pos..pos + len {
                insert(k, &mut head, &mut prev);
            }
            pos += len;
        }
        if tokens.len() >= BLOCK_TOKENS {
            flush_block(data, block_start, pos, &tokens, false, w);
            tokens.clear();
            block_start = pos;
        }
    }
    flush_block(data, block_start, n, &tokens, true, w);
}

fn emit_match(tokens: &mut Vec<Token>, len: usize, dist: usize) {
    tokens.push(Token::Match {
        length: len as u16,
        distance: dist as u16,
    });
}

/// Code de longueur (0..=28 relatif à 257) et bits supplémentaires.
fn length_code(len: u16) -> (usize, u32, u8) {
    let mut i = 28;
    while i > 0 && LENGTH_BASE[i] > len {
        i -= 1;
    }
    (i, u32::from(len - LENGTH_BASE[i]), LENGTH_EXTRA[i])
}

/// Code de distance (0..=29) et bits supplémentaires.
fn dist_code(dist: u16) -> (usize, u32, u8) {
    let mut i = 29;
    while i > 0 && DIST_BASE[i] > dist {
        i -= 1;
    }
    (i, u32::from(dist - DIST_BASE[i]), DIST_EXTRA[i])
}

/// Émet un bloc : choisit entre stored, Huffman fixe et Huffman dynamique
/// selon la taille produite.
fn flush_block(
    data: &[u8],
    start: usize,
    end: usize,
    tokens: &[Token],
    is_final: bool,
    w: &mut BitWriter,
) {
    // Fréquences.
    let mut lit_freq = [0u32; 286];
    let mut dist_freq = [0u32; 30];
    for t in tokens {
        match *t {
            Token::Literal(b) => lit_freq[usize::from(b)] += 1,
            Token::Match { length, distance } => {
                lit_freq[257 + length_code(length).0] += 1;
                dist_freq[dist_code(distance).0] += 1;
            }
        }
    }
    lit_freq[256] += 1;
    // Codes dynamiques.
    let lit_lens = huffman_lengths(&lit_freq, 15);
    let dist_lens = huffman_lengths(&dist_freq, 15);
    let dyn_header = DynamicHeader::build(&lit_lens, &dist_lens);
    let dyn_bits = dyn_header.bits + token_bits(tokens, &lit_lens, &dist_lens);
    // Codes fixes.
    let (fixed_lit, fixed_dist) = fixed_lengths();
    let fixed_bits = token_bits(tokens, &fixed_lit, &fixed_dist);
    // Stored : 5 octets d'en-tête par tranche de 65 535, alignement à l'octet.
    let stored_len = end - start;
    let stored_bits = 8 * (stored_len + 5 * stored_len.div_ceil(65_535).max(1)) + 7;

    if stored_bits <= dyn_bits && stored_bits <= fixed_bits {
        write_stored(data, start, end, is_final, w);
    } else if fixed_bits <= dyn_bits {
        w.bits(u32::from(is_final), 1);
        w.bits(1, 2);
        write_tokens(tokens, &fixed_lit, &fixed_dist, w);
    } else {
        w.bits(u32::from(is_final), 1);
        w.bits(2, 2);
        dyn_header.write(w);
        write_tokens(tokens, &lit_lens, &dist_lens, w);
    }
}

fn write_stored(data: &[u8], start: usize, end: usize, is_final: bool, w: &mut BitWriter) {
    let slice = &data[start..end];
    let mut chunks = slice.chunks(65_535).peekable();
    if chunks.peek().is_none() {
        w.bits(u32::from(is_final), 1);
        w.bits(0, 2);
        w.align();
        w.bytes(&[0, 0, 0xFF, 0xFF]);
        return;
    }
    while let Some(chunk) = chunks.next() {
        let last = chunks.peek().is_none();
        w.bits(u32::from(is_final && last), 1);
        w.bits(0, 2);
        w.align();
        let len = chunk.len() as u16;
        w.bytes(&len.to_le_bytes());
        w.bytes(&(!len).to_le_bytes());
        w.bytes(chunk);
    }
}

/// Nombre de bits nécessaires pour coder les jetons avec ces longueurs.
fn token_bits(tokens: &[Token], lit_lens: &[u8], dist_lens: &[u8]) -> usize {
    let mut bits = usize::from(lit_lens[256]);
    for t in tokens {
        match *t {
            Token::Literal(b) => bits += usize::from(lit_lens[usize::from(b)]),
            Token::Match { length, distance } => {
                let (lc, _, le) = length_code(length);
                let (dc, _, de) = dist_code(distance);
                bits += usize::from(lit_lens[257 + lc]) + usize::from(le);
                bits += usize::from(dist_lens[dc]) + usize::from(de);
            }
        }
    }
    bits
}

fn write_tokens(tokens: &[Token], lit_lens: &[u8], dist_lens: &[u8], w: &mut BitWriter) {
    let lit_codes = canonical_codes(lit_lens);
    let dist_codes = canonical_codes(dist_lens);
    for t in tokens {
        match *t {
            Token::Literal(b) => {
                let i = usize::from(b);
                w.bits_rev(lit_codes[i], lit_lens[i]);
            }
            Token::Match { length, distance } => {
                let (lc, lextra, lbits) = length_code(length);
                w.bits_rev(lit_codes[257 + lc], lit_lens[257 + lc]);
                w.bits(lextra, lbits);
                let (dc, dextra, dbits) = dist_code(distance);
                w.bits_rev(dist_codes[dc], dist_lens[dc]);
                w.bits(dextra, dbits);
            }
        }
    }
    w.bits_rev(lit_codes[256], lit_lens[256]);
}

/// Longueurs des codes fixes (RFC 1951 §3.2.6).
///
/// L'alphabet compte **288** symboles, pas 286 : 286 et 287 n'apparaissent
/// jamais dans un flux mais leurs longueurs de 8 bits participent au calcul
/// des codes canoniques. Les omettre décalerait de quatre tous les codes de
/// neuf bits (littéraux 144 à 255).
fn fixed_lengths() -> (Vec<u8>, Vec<u8>) {
    let mut lit = vec![0u8; 288];
    for (i, l) in lit.iter_mut().enumerate() {
        *l = match i {
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    (lit, vec![5u8; 30])
}

/// Longueurs de codes de Huffman limitées à `max_bits`, par construction
/// classique puis, si la profondeur déborde, aplatissement des fréquences
/// jusqu'à ce que l'arbre tienne (résultat valide, quasi optimal).
fn huffman_lengths(freq: &[u32], max_bits: u8) -> Vec<u8> {
    let n = freq.len();
    let mut lens = vec![0u8; n];
    let used: Vec<usize> = (0..n).filter(|&i| freq[i] > 0).collect();
    match used.len() {
        0 => return lens,
        1 => {
            // Un seul symbole : la RFC impose au moins un code d'un bit.
            lens[used[0]] = 1;
            let other = usize::from(used[0] == 0);
            if other < n {
                lens[other] = 1;
            }
            return lens;
        }
        _ => {}
    }
    let mut f: Vec<u64> = freq.iter().map(|&x| u64::from(x)).collect();
    loop {
        let l = build_lengths(&f);
        if l.iter().all(|&x| x <= max_bits) {
            return l;
        }
        // Aplatit la distribution : divise par deux en gardant les symboles présents.
        for (x, &orig) in f.iter_mut().zip(freq) {
            if orig > 0 {
                *x = x.div_ceil(2);
            }
        }
    }
}

/// Longueurs de codes par fusion des deux poids minimaux (Huffman).
fn build_lengths(freq: &[u64]) -> Vec<u8> {
    // Nœuds : (poids, parent).
    let n = freq.len();
    let mut weight: Vec<u64> = Vec::with_capacity(2 * n);
    let mut parent: Vec<usize> = Vec::with_capacity(2 * n);
    let mut leaves: Vec<usize> = Vec::new();
    for (i, &f) in freq.iter().enumerate() {
        if f > 0 {
            leaves.push(i);
            weight.push(f);
            parent.push(usize::MAX);
        }
    }
    // File de priorité simple : liste triée maintenue par insertion (les
    // alphabets ont au plus 286 symboles : coût négligeable).
    let mut queue: Vec<usize> = (0..weight.len()).collect();
    queue.sort_by_key(|&i| std::cmp::Reverse(weight[i]));
    while queue.len() > 1 {
        let a = queue.pop().unwrap_or(0);
        let b = queue.pop().unwrap_or(0);
        let w = weight[a] + weight[b];
        let idx = weight.len();
        weight.push(w);
        parent.push(usize::MAX);
        parent[a] = idx;
        parent[b] = idx;
        // Insertion en gardant l'ordre décroissant (pop = plus petit).
        let pos = queue.partition_point(|&i| weight[i] > w);
        queue.insert(pos, idx);
    }
    let mut lens = vec![0u8; n];
    for (k, &sym) in leaves.iter().enumerate() {
        let mut depth = 0u8;
        let mut node = k;
        while parent[node] != usize::MAX {
            node = parent[node];
            depth = depth.saturating_add(1);
        }
        lens[sym] = depth;
    }
    lens
}

/// Codes canoniques à partir des longueurs (RFC 1951 §3.2.2).
fn canonical_codes(lens: &[u8]) -> Vec<u32> {
    let mut count = [0u32; 16];
    for &l in lens {
        count[usize::from(l)] += 1;
    }
    count[0] = 0;
    let mut next = [0u32; 16];
    let mut code = 0u32;
    for bits in 1..16 {
        code = (code + count[bits - 1]) << 1;
        next[bits] = code;
    }
    lens.iter()
        .map(|&l| {
            if l == 0 {
                0
            } else {
                let c = next[usize::from(l)];
                next[usize::from(l)] += 1;
                c
            }
        })
        .collect()
}

/// En-tête d'un bloc dynamique : longueurs de codes compressées (RFC 1951 §3.2.7).
struct DynamicHeader {
    hlit: usize,
    hdist: usize,
    hclen: usize,
    cl_lens: Vec<u8>,
    /// Symboles de l'alphabet des longueurs (0..=18) avec leurs bits extra.
    symbols: Vec<(u8, u32, u8)>,
    /// Taille totale de l'en-tête en bits.
    bits: usize,
}

impl DynamicHeader {
    fn build(lit_lens: &[u8], dist_lens: &[u8]) -> Self {
        let hlit = (257..=286)
            .rev()
            .find(|&k| lit_lens[k - 1] != 0)
            .unwrap_or(257)
            .max(257);
        let hdist = (1..=30)
            .rev()
            .find(|&k| dist_lens[k - 1] != 0)
            .unwrap_or(1)
            .max(1);
        let mut all: Vec<u8> = Vec::with_capacity(hlit + hdist);
        all.extend_from_slice(&lit_lens[..hlit]);
        all.extend_from_slice(&dist_lens[..hdist]);
        // Codage RLE : 16 (répéter la précédente 3-6), 17 (zéros 3-10), 18 (zéros 11-138).
        let mut symbols: Vec<(u8, u32, u8)> = Vec::new();
        let mut i = 0;
        while i < all.len() {
            let v = all[i];
            let mut run = 1;
            while i + run < all.len() && all[i + run] == v {
                run += 1;
            }
            if v == 0 && run >= 3 {
                let mut r = run;
                while r >= 3 {
                    if r >= 11 {
                        let take = r.min(138);
                        symbols.push((18, (take - 11) as u32, 7));
                        r -= take;
                    } else {
                        symbols.push((17, (r - 3) as u32, 3));
                        r = 0;
                    }
                }
                for _ in 0..r {
                    symbols.push((0, 0, 0));
                }
                i += run;
            } else if run >= 4 {
                symbols.push((v, 0, 0));
                let mut r = run - 1;
                while r >= 3 {
                    let take = r.min(6);
                    symbols.push((16, (take - 3) as u32, 2));
                    r -= take;
                }
                for _ in 0..r {
                    symbols.push((v, 0, 0));
                }
                i += run;
            } else {
                for _ in 0..run {
                    symbols.push((v, 0, 0));
                }
                i += run;
            }
        }
        let mut cl_freq = [0u32; 19];
        for &(s, _, _) in &symbols {
            cl_freq[usize::from(s)] += 1;
        }
        let cl_lens = huffman_lengths(&cl_freq, 7);
        let hclen = (4..=19)
            .rev()
            .find(|&k| cl_lens[CODE_LENGTH_ORDER[k - 1]] != 0)
            .unwrap_or(4)
            .max(4);
        let mut bits = 5 + 5 + 4 + 3 * hclen;
        for &(s, _, extra) in &symbols {
            bits += usize::from(cl_lens[usize::from(s)]) + usize::from(extra);
        }
        Self {
            hlit,
            hdist,
            hclen,
            cl_lens,
            symbols,
            bits,
        }
    }

    fn write(&self, w: &mut BitWriter) {
        w.bits((self.hlit - 257) as u32, 5);
        w.bits((self.hdist - 1) as u32, 5);
        w.bits((self.hclen - 4) as u32, 4);
        for &idx in &CODE_LENGTH_ORDER[..self.hclen] {
            w.bits(u32::from(self.cl_lens[idx]), 3);
        }
        let codes = canonical_codes(&self.cl_lens);
        for &(s, extra, extra_bits) in &self.symbols {
            let i = usize::from(s);
            w.bits_rev(codes[i], self.cl_lens[i]);
            w.bits(extra, extra_bits);
        }
    }
}

/// Écriture de bits, poids faible en premier (RFC 1951 §3.1.1).
struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    nbits: u32,
}

impl BitWriter {
    fn new(out: Vec<u8>) -> Self {
        Self {
            out,
            acc: 0,
            nbits: 0,
        }
    }

    /// `n` bits de `value`, poids faible en premier.
    fn bits(&mut self, value: u32, n: u8) {
        if n == 0 {
            return;
        }
        self.acc |= u64::from(value & ((1u32 << n) - 1)) << self.nbits;
        self.nbits += u32::from(n);
        while self.nbits >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.nbits -= 8;
        }
    }

    /// Code de Huffman : poids fort en premier (inversé avant écriture).
    fn bits_rev(&mut self, code: u32, n: u8) {
        let mut rev = 0u32;
        for i in 0..n {
            if code & (1 << i) != 0 {
                rev |= 1 << (n - 1 - i);
            }
        }
        self.bits(rev, n);
    }

    fn align(&mut self) {
        if self.nbits > 0 {
            self.out.push(self.acc as u8);
            self.acc = 0;
            self.nbits = 0;
        }
    }

    fn bytes(&mut self, b: &[u8]) {
        self.align();
        self.out.extend_from_slice(b);
    }

    fn finish(mut self) -> Vec<u8> {
        self.align();
        self.out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests
mod tests {
    use super::*;
    use crate::flate::decode;

    fn lcg(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                (x >> 33) as u8
            })
            .collect()
    }

    fn roundtrip(data: &[u8], level: u8) -> usize {
        let z = compress(data, level);
        assert_eq!(
            decode(&z).unwrap(),
            data,
            "niveau {level}, {} octets",
            data.len()
        );
        z.len()
    }

    #[test]
    fn small_and_edge_inputs_roundtrip_at_every_level() {
        for level in 0..=9 {
            roundtrip(b"", level);
            roundtrip(b"a", level);
            roundtrip(b"ab", level);
            roundtrip(b"abc", level);
            roundtrip(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", level);
            roundtrip(&vec![0u8; 100_000], level);
        }
    }

    /// Un littéral de 144 à 255 codé par l'arbre fixe occupe neuf bits ; son
    /// code dépend du nombre de codes de huit bits, symboles 286 et 287
    /// compris. Une image RVB presque noire avec quelques pixels saturés est
    /// le cas typique (biffure d'image) : elle choisit le bloc fixe.
    #[test]
    fn high_literals_in_a_fixed_block_roundtrip() {
        for level in 0..=9 {
            for value in [144u8, 200, 251, 254, 255] {
                let mut data = vec![0u8; 48];
                data[6] = value;
                data[9] = value;
                roundtrip(&data, level);
            }
        }
    }

    #[test]
    fn text_and_binary_roundtrip() {
        let text = "Le PDF est un format de document portable. ".repeat(500);
        let mut mixed = text.clone().into_bytes();
        mixed.extend(lcg(50_000, 7));
        mixed.extend(text.bytes());
        for level in [1, 4, 6, 9] {
            roundtrip(text.as_bytes(), level);
            roundtrip(&lcg(70_000, 1), level);
            roundtrip(&mixed, level);
        }
    }

    #[test]
    #[allow(clippy::format_collect)] // contenu de test construit ligne à ligne
    fn compression_is_effective() {
        let text = "Le PDF est un format de document portable. ".repeat(500);
        let z = compress(text.as_bytes(), 6);
        assert!(z.len() * 20 < text.len(), "{} → {}", text.len(), z.len());
        let zeros = compress(&vec![0u8; 1 << 20], 6);
        assert!(zeros.len() < 2048, "{}", zeros.len());
        // Contenu de flux PDF typique : opérateurs répétitifs.
        let content = (0..2000)
            .map(|i| {
                format!(
                    "{} {} m {} {} l S\n",
                    i % 300,
                    i % 200,
                    i % 250 + 10,
                    i % 180 + 5
                )
            })
            .collect::<String>();
        let z = compress(content.as_bytes(), 6);
        assert!(
            z.len() * 3 < content.len(),
            "{} → {}",
            content.len(),
            z.len()
        );
        // Le niveau 9 ne fait pas pire que le niveau 1 sur du texte.
        let z1 = compress(content.as_bytes(), 1);
        let z9 = compress(content.as_bytes(), 9);
        assert!(z9.len() <= z1.len());
    }

    #[test]
    fn random_data_falls_back_to_stored_without_growing_much() {
        let noise = lcg(200_000, 42);
        let z = compress(&noise, 6);
        assert!(
            z.len() <= noise.len() + noise.len() / 1000 + 64,
            "{}",
            z.len()
        );
        assert_eq!(decode(&z).unwrap(), noise);
    }

    #[test]
    fn long_matches_and_far_distances() {
        // Motif de 300 octets répété : correspondances de 258 puis distances > 256.
        let unit = lcg(300, 3);
        let mut data = Vec::new();
        for _ in 0..50 {
            data.extend_from_slice(&unit);
        }
        // Répétition à distance maximale (32 768).
        let mut far = lcg(32_768, 5);
        far.extend_from_slice(&far.clone()[..1000]);
        for level in [1, 6, 9] {
            let n = roundtrip(&data, level);
            assert!(n < data.len() / 10);
            roundtrip(&far, level);
        }
    }

    #[test]
    fn huffman_lengths_respect_limits() {
        // Distribution très déséquilibrée : sans limitation, la profondeur dépasserait 15.
        let mut freq = vec![0u32; 286];
        let mut f = 1u32;
        for slot in freq.iter_mut().take(40) {
            *slot = f;
            f = f.saturating_mul(2);
        }
        let lens = huffman_lengths(&freq, 15);
        assert!(lens.iter().all(|&l| l <= 15));
        // Inégalité de Kraft : code complet ou préfixe valide.
        let kraft: f64 = lens
            .iter()
            .filter(|&&l| l > 0)
            .map(|&l| 2f64.powi(-i32::from(l)))
            .sum();
        assert!(kraft <= 1.0 + 1e-9);
    }
}
