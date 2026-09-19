//! Décodeur Flate (filtre FlateDecode, ISO 32000-2 §7.4.4.1).
//!
//! Le filtre FlateDecode encapsule un flux **zlib** (RFC 1950) contenant des
//! données **deflate** (RFC 1951). Ce module implémente les deux couches à
//! partir des RFC, sans aucune dépendance :
//!
//! - RFC 1951 §3.2.4 : blocs non compressés (« stored ») ;
//! - RFC 1951 §3.2.6 : blocs à codes de Huffman fixes ;
//! - RFC 1951 §3.2.7 : blocs à codes de Huffman dynamiques ;
//! - RFC 1951 §3.2.3 : fenêtre glissante de 32 Ko pour les références arrière ;
//! - RFC 1950 §2.2   : en-tête zlib (CMF / FLG) et somme de contrôle Adler-32.
//!
//! L'encodage est fourni par [`encode`] (blocs stored, sans compression) et
//! par [`compress`] (compresseur complet du sous-module `compress`).
//!
//! ## Tolérance aux flux abîmés
//!
//! Les PDF réels contiennent très souvent des flux Flate imparfaits. Comme
//! Acrobat, ce décodeur :
//!
//! - ignore les octets blancs parasites avant l'en-tête zlib (retour à la
//!   ligne oublié après le mot-clé `stream`, par exemple) ;
//! - accepte un flux deflate brut sans enveloppe zlib ;
//! - retourne les données décodées jusqu'au point d'erreur quand le flux est
//!   tronqué ou corrompu (`Ok` avec données partielles), et ne retourne
//!   `Err(Corrupt)` que si rien du tout n'a pu être décodé ;
//! - vérifie l'Adler-32 sans bloquer : un mauvais total est signalé dans
//!   [`Inflated::checksum_ok`] mais les données sont rendues quand même.
//!
//! Une limite de taille de sortie (256 Mo par défaut) protège contre les
//! bombes de décompression ; la dépasser est la seule erreur « dure ».

use acrux_core::{Error, Result};

/// Taille de sortie maximale par défaut, en octets (256 Mo).
pub const DEFAULT_MAX_OUTPUT: usize = 256 * 1024 * 1024;

/// Longueur maximale d'un code de Huffman (RFC 1951 §3.2.7 : 15 bits).
const MAX_CODE_BITS: usize = 15;

/// Nombre maximal de symboles littéraux / longueurs (RFC 1951 §3.2.7).
const MAX_LITLEN_SYMBOLS: usize = 288;

/// Nombre maximal de codes de distance (RFC 1951 §3.2.7).
const MAX_DIST_SYMBOLS: usize = 30;

/// Taille maximale d'un bloc stored (RFC 1951 §3.2.4 : LEN sur 16 bits).
const MAX_STORED_BLOCK: usize = 65_535;

mod compress;
pub use compress::compress;

/// Résultat détaillé d'un décodage Flate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inflated {
    /// Données décompressées (complètes ou partielles, voir `complete`).
    pub data: Vec<u8>,
    /// `true` si le dernier bloc a été décodé jusqu'à son marqueur de fin.
    pub complete: bool,
    /// Résultat de la vérification Adler-32 : `None` si le flux était brut,
    /// tronqué, ou si le total de contrôle manquait.
    pub checksum_ok: Option<bool>,
    /// `true` si le flux a été lu comme du deflate brut (sans en-tête zlib).
    pub raw: bool,
}

/// Décode un flux FlateDecode avec la limite de taille par défaut.
///
/// Voir la documentation du module pour la politique de tolérance.
///
/// # Errors
///
/// `Error::Corrupt` si aucun octet n'a pu être décodé, ou si la sortie
/// dépasserait [`DEFAULT_MAX_OUTPUT`].
pub fn decode(data: &[u8]) -> Result<Vec<u8>> {
    decode_with_limit(data, DEFAULT_MAX_OUTPUT)
}

/// Décode un flux FlateDecode en bornant la taille de sortie à `max_output` octets.
///
/// # Errors
///
/// `Error::Corrupt` si aucun octet n'a pu être décodé, ou si la sortie
/// dépasserait `max_output`.
pub fn decode_with_limit(data: &[u8], max_output: usize) -> Result<Vec<u8>> {
    decode_detailed(data, max_output).map(|inflated| inflated.data)
}

/// Décode un flux FlateDecode et rapporte l'état du décodage (complet ou
/// non, somme de contrôle, enveloppe détectée).
///
/// # Errors
///
/// `Error::Corrupt` si aucun octet n'a pu être décodé, ou si la sortie
/// dépasserait `max_output`.
pub fn decode_detailed(data: &[u8], max_output: usize) -> Result<Inflated> {
    let start = skip_pdf_whitespace(data);

    // 1. Enveloppe zlib (RFC 1950 §2.2), éventuellement précédée de blancs.
    if has_zlib_header(data, start) {
        let outcome = inflate(data, start + 2, max_output);
        if matches!(outcome.stop, Some(Stop::TooLarge)) {
            return Err(too_large(max_output));
        }
        if !outcome.output.is_empty() || outcome.stop.is_none() {
            let complete = outcome.stop.is_none();
            let checksum_ok = if complete {
                verify_adler32(data, outcome.end, &outcome.output)
            } else {
                None
            };
            return Ok(Inflated {
                data: outcome.output,
                complete,
                checksum_ok,
                raw: false,
            });
        }
        // Rien décodé malgré un en-tête plausible : on tente le deflate brut.
    }

    // 2. Deflate brut (RFC 1951), depuis l'octet 0 puis après les blancs. Un
    //    octet blanc peut se lire comme un en-tête de bloc plausible et produire
    //    quelques octets de bruit : on garde la tentative complète, sinon la
    //    plus longue.
    let mut outcome = inflate(data, 0, max_output);
    if outcome.stop.is_some() && start > 0 {
        let alternative = inflate(data, start, max_output);
        if alternative.stop.is_none() || alternative.output.len() > outcome.output.len() {
            outcome = alternative;
        }
    }
    match outcome.stop {
        Some(Stop::TooLarge) => Err(too_large(max_output)),
        Some(stop) if outcome.output.is_empty() => Err(stop.into_error()),
        stop => Ok(Inflated {
            data: outcome.output,
            complete: stop.is_none(),
            checksum_ok: None,
            raw: true,
        }),
    }
}

/// Encode `data` dans une enveloppe zlib en n'utilisant que des blocs stored
/// (RFC 1951 §3.2.4). Aucune compression n'est faite : le flux est valide et
/// suffisant pour écrire des PDF de test.
#[must_use]
pub fn encode(data: &[u8]) -> Vec<u8> {
    // En-tête zlib : CM = 8 (deflate), CINFO = 7 (fenêtre 32 Ko), FLEVEL = 0,
    // FCHECK choisi pour que CMF * 256 + FLG soit multiple de 31 (RFC 1950 §2.2).
    let mut out = Vec::with_capacity(data.len() + data.len() / MAX_STORED_BLOCK * 5 + 11);
    out.extend_from_slice(&[0x78, 0x01]);

    let mut chunks = data.chunks(MAX_STORED_BLOCK).peekable();
    if chunks.peek().is_none() {
        // Entrée vide : un unique bloc stored final de longueur 0.
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
    }
    while let Some(chunk) = chunks.next() {
        let is_final = chunks.peek().is_none();
        // Longueur bornée par MAX_STORED_BLOCK : la conversion ne peut pas échouer.
        let len = u16::try_from(chunk.len()).unwrap_or(u16::MAX);
        out.push(u8::from(is_final));
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }

    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Somme de contrôle Adler-32 (RFC 1950 §8).
#[must_use]
pub fn adler32(data: &[u8]) -> u32 {
    const MODULUS: u32 = 65_521;
    // Nombre maximal d'octets que l'on peut accumuler dans un u32 avant de
    // devoir réduire modulo 65 521 (valeur classique dérivée de la RFC).
    const CHUNK: usize = 5_552;

    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for chunk in data.chunks(CHUNK) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= MODULUS;
        b %= MODULUS;
    }
    (b << 16) | a
}

// ---------------------------------------------------------------------------
// Enveloppe zlib
// ---------------------------------------------------------------------------

/// Renvoie l'index du premier octet qui n'est pas un blanc PDF
/// (ISO 32000-2 §7.2.3, tableau 1 : NUL, HT, LF, FF, CR, SP).
fn skip_pdf_whitespace(data: &[u8]) -> usize {
    data.iter()
        .position(|b| !matches!(b, 0x00 | 0x09 | 0x0A | 0x0C | 0x0D | 0x20))
        .unwrap_or(data.len())
}

/// Vérifie la présence d'un en-tête zlib valide à la position `pos`
/// (RFC 1950 §2.2) : CM = 8, CINFO ≤ 7, FCHECK correct et pas de
/// dictionnaire prédéfini (FDICT), que PDF n'utilise jamais.
fn has_zlib_header(data: &[u8], pos: usize) -> bool {
    let (Some(&cmf), Some(&flg)) = (data.get(pos), data.get(pos + 1)) else {
        return false;
    };
    let method_is_deflate = cmf & 0x0F == 8;
    let window_is_valid = cmf >> 4 <= 7;
    let check_is_valid = (u32::from(cmf) * 256 + u32::from(flg)) % 31 == 0;
    let has_preset_dict = flg & 0x20 != 0;
    method_is_deflate && window_is_valid && check_is_valid && !has_preset_dict
}

/// Compare l'Adler-32 stocké après les données (RFC 1950 §2.2, grand-boutiste)
/// avec celui de la sortie. `None` si le total de contrôle manque.
fn verify_adler32(data: &[u8], end: usize, output: &[u8]) -> Option<bool> {
    let trailer = data.get(end..end.checked_add(4)?)?;
    let expected = u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    Some(adler32(output) == expected)
}

fn too_large(max_output: usize) -> Error {
    Error::Corrupt(format!(
        "flux Flate : la sortie dépasse la limite de {max_output} octets"
    ))
}

// ---------------------------------------------------------------------------
// Décodeur deflate (RFC 1951)
// ---------------------------------------------------------------------------

/// Raison pour laquelle le décodage s'est arrêté avant la fin normale.
#[derive(Debug)]
enum Stop {
    /// Données invalides, avec un message lisible.
    Corrupt(String),
    /// Le flux se termine avant la fin du dernier bloc.
    Truncated,
    /// La sortie dépasserait la limite configurée.
    TooLarge,
}

impl Stop {
    fn corrupt(message: &str) -> Self {
        Stop::Corrupt(format!("flux Flate : {message}"))
    }

    fn into_error(self) -> Error {
        match self {
            Stop::Corrupt(message) => Error::Corrupt(message),
            Stop::Truncated => Error::Corrupt("flux Flate tronqué".to_string()),
            Stop::TooLarge => Error::Corrupt("flux Flate : sortie trop volumineuse".to_string()),
        }
    }
}

/// Résultat interne d'une passe de décodage.
type Step<T> = std::result::Result<T, Stop>;

/// Sortie d'une tentative de décodage deflate.
struct Outcome {
    /// Octets décodés (éventuellement partiels).
    output: Vec<u8>,
    /// `None` si le dernier bloc a été lu jusqu'au bout.
    stop: Option<Stop>,
    /// Position (en octets) juste après les données deflate consommées.
    end: usize,
}

/// Décode un flux deflate brut commençant à l'octet `start`.
fn inflate(data: &[u8], start: usize, max_output: usize) -> Outcome {
    let mut inflater = Inflater {
        reader: BitReader::new(data, start),
        output: Vec::new(),
        max_output,
    };
    let stop = inflater.run().err();
    let end = inflater.reader.byte_position();
    Outcome {
        output: inflater.output,
        stop,
        end,
    }
}

/// Lecteur de bits, poids faible en premier (RFC 1951 §3.1.1).
struct BitReader<'a> {
    data: &'a [u8],
    /// Prochain octet à charger dans `buffer`.
    pos: usize,
    /// Bits en attente, le bit 0 étant le prochain à lire.
    buffer: u64,
    /// Nombre de bits valides dans `buffer`.
    count: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], start: usize) -> Self {
        BitReader {
            data,
            pos: start.min(data.len()),
            buffer: 0,
            count: 0,
        }
    }

    /// Charge des octets tant qu'il reste de la place dans le tampon.
    fn refill(&mut self) {
        while self.count <= 56 {
            let Some(&byte) = self.data.get(self.pos) else {
                return;
            };
            self.buffer |= u64::from(byte) << self.count;
            self.count += 8;
            self.pos += 1;
        }
    }

    /// Regarde les `n` prochains bits (`n` ≤ 32) sans les consommer.
    /// Au-delà de la fin du flux, les bits manquants valent zéro.
    fn peek(&mut self, n: u32) -> u32 {
        self.refill();
        let masked = self.buffer & ((1u64 << n) - 1);
        // `masked` tient sur `n` ≤ 32 bits : la conversion ne peut pas échouer.
        u32::try_from(masked).unwrap_or(u32::MAX)
    }

    /// Consomme `n` bits ; `Truncated` s'ils ne sont pas tous disponibles.
    fn consume(&mut self, n: u32) -> Step<()> {
        if n > self.count {
            return Err(Stop::Truncated);
        }
        self.buffer >>= n;
        self.count -= n;
        Ok(())
    }

    /// Lit `n` bits (`n` ≤ 32) comme un entier, poids faible en premier.
    fn bits(&mut self, n: u32) -> Step<u32> {
        if n == 0 {
            return Ok(0);
        }
        let value = self.peek(n);
        self.consume(n)?;
        Ok(value)
    }

    /// Abandonne les bits restants de l'octet courant (RFC 1951 §3.2.4).
    fn align_to_byte(&mut self) {
        let remainder = self.count % 8;
        self.buffer >>= remainder;
        self.count -= remainder;
    }

    /// Position en octets du prochain octet non encore consommé.
    fn byte_position(&mut self) -> usize {
        self.align_to_byte();
        self.pos - (self.count / 8) as usize
    }

    /// Repositionne le lecteur sur un octet donné, en vidant le tampon.
    fn seek(&mut self, pos: usize) {
        self.pos = pos.min(self.data.len());
        self.buffer = 0;
        self.count = 0;
    }
}

/// Table de décodage de Huffman canonique (RFC 1951 §3.2.2).
///
/// La table est indexée directement par les `bits` prochains bits du flux
/// (dans l'ordre de lecture, donc code inversé). Chaque entrée vaut
/// `symbole << 4 | longueur`, la longueur 0 marquant un code inutilisé.
struct Huffman {
    table: Vec<u32>,
    bits: u32,
}

impl Huffman {
    /// Construit la table à partir des longueurs de code de chaque symbole.
    fn new(lengths: &[u8]) -> Step<Self> {
        let max_len = usize::from(lengths.iter().copied().max().unwrap_or(0));
        if max_len == 0 {
            // Alphabet vide : toute tentative de décodage échouera proprement.
            return Ok(Huffman {
                table: Vec::new(),
                bits: 0,
            });
        }
        if max_len > MAX_CODE_BITS {
            return Err(Stop::corrupt("longueur de code de Huffman > 15"));
        }

        // Nombre de codes par longueur, puis premier code de chaque longueur
        // (RFC 1951 §3.2.2, étapes 1 et 2).
        let mut count = [0u32; MAX_CODE_BITS + 1];
        for &len in lengths {
            count[usize::from(len)] += 1;
        }
        count[0] = 0;
        let mut next_code = [0u32; MAX_CODE_BITS + 1];
        let mut code = 0u32;
        let mut remaining = 1i64;
        for len in 1..=MAX_CODE_BITS {
            code = (code + count[len - 1]) << 1;
            next_code[len] = code;
            remaining = (remaining << 1) - i64::from(count[len]);
            if remaining < 0 {
                return Err(Stop::corrupt("jeu de codes de Huffman sursouscrit"));
            }
        }

        // Étape 3 : attribution des codes, écrits inversés dans la table.
        let mut table = vec![0u32; 1 << max_len];
        for (symbol, &len) in lengths.iter().enumerate() {
            if len == 0 {
                continue;
            }
            let len_usize = usize::from(len);
            let code = next_code[len_usize];
            next_code[len_usize] += 1;
            let reversed = reverse_bits(code, len_usize);
            // Le symbole est < 288 et la longueur < 16 : l'entrée tient sur u32.
            let entry = (u32::try_from(symbol).unwrap_or(0) << 4) | u32::from(len);
            let step = 1usize << len_usize;
            let mut index = reversed;
            while index < table.len() {
                table[index] = entry;
                index += step;
            }
        }
        // `max_len` ≤ 15 : la conversion ne peut pas échouer.
        Ok(Huffman {
            table,
            bits: u32::try_from(max_len).unwrap_or(0),
        })
    }

    /// Décode le prochain symbole du flux.
    fn decode(&self, reader: &mut BitReader) -> Step<u16> {
        let index = reader.peek(self.bits) as usize;
        let entry = self.table.get(index).copied().unwrap_or(0);
        let len = entry & 0xF;
        if len == 0 {
            return Err(Stop::corrupt("code de Huffman invalide"));
        }
        reader.consume(len)?;
        // Le symbole est < 288 : la conversion ne peut pas échouer.
        Ok(u16::try_from(entry >> 4).unwrap_or(u16::MAX))
    }

    /// Tables fixes de la RFC 1951 §3.2.6.
    fn fixed_tables() -> Step<(Huffman, Huffman)> {
        let mut litlen = [0u8; MAX_LITLEN_SYMBOLS];
        for (symbol, len) in litlen.iter_mut().enumerate() {
            *len = match symbol {
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
        }
        let dist = [5u8; MAX_DIST_SYMBOLS];
        Ok((Huffman::new(&litlen)?, Huffman::new(&dist)?))
    }
}

/// Inverse l'ordre des `len` bits de poids faible de `code`.
fn reverse_bits(code: u32, len: usize) -> usize {
    let mut reversed = 0usize;
    for i in 0..len {
        if code & (1 << i) != 0 {
            reversed |= 1 << (len - 1 - i);
        }
    }
    reversed
}

/// Base et bits supplémentaires des codes de longueur 257..285 (RFC 1951 §3.2.5).
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

/// Base et bits supplémentaires des codes de distance 0..29 (RFC 1951 §3.2.5).
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12_289, 16_385, 24_577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Ordre de transmission des longueurs de l'alphabet des longueurs de code
/// (RFC 1951 §3.2.7).
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// État du décodeur deflate.
struct Inflater<'a> {
    reader: BitReader<'a>,
    /// Sortie complète : elle sert aussi de fenêtre glissante.
    output: Vec<u8>,
    max_output: usize,
}

impl Inflater<'_> {
    /// Décode les blocs successifs jusqu'au bloc final (RFC 1951 §3.2.3).
    fn run(&mut self) -> Step<()> {
        loop {
            let is_final = self.reader.bits(1)? == 1;
            match self.reader.bits(2)? {
                0 => self.stored_block()?,
                1 => {
                    let (litlen, dist) = Huffman::fixed_tables()?;
                    self.compressed_block(&litlen, &dist)?;
                }
                2 => {
                    let (litlen, dist) = self.read_dynamic_tables()?;
                    self.compressed_block(&litlen, &dist)?;
                }
                _ => return Err(Stop::corrupt("type de bloc 3 réservé")),
            }
            if is_final {
                return Ok(());
            }
        }
    }

    /// Bloc non compressé (RFC 1951 §3.2.4) : LEN, NLEN puis les octets bruts.
    fn stored_block(&mut self) -> Step<()> {
        self.reader.align_to_byte();
        let len = self.reader.bits(16)? as usize;
        let len_complement = self.reader.bits(16)? as usize;
        if len != !len_complement & 0xFFFF {
            return Err(Stop::corrupt("bloc stored : NLEN ne complète pas LEN"));
        }
        let start = self.reader.byte_position();
        let available = self.reader.data.len().saturating_sub(start);
        let take = len.min(available);
        self.reserve(take)?;
        self.output
            .extend_from_slice(&self.reader.data[start..start + take]);
        self.reader.seek(start + take);
        if take < len {
            return Err(Stop::Truncated);
        }
        Ok(())
    }

    /// Lit les tables de Huffman dynamiques d'un bloc (RFC 1951 §3.2.7).
    fn read_dynamic_tables(&mut self) -> Step<(Huffman, Huffman)> {
        let hlit = self.reader.bits(5)? as usize + 257;
        let hdist = self.reader.bits(5)? as usize + 1;
        let hclen = self.reader.bits(4)? as usize + 4;
        if hlit > 286 || hdist > MAX_DIST_SYMBOLS {
            return Err(Stop::corrupt("bloc dynamique : HLIT ou HDIST hors limites"));
        }

        // Longueurs de l'alphabet des longueurs de code, dans l'ordre permuté.
        let mut code_length_lengths = [0u8; 19];
        for &position in CODE_LENGTH_ORDER.iter().take(hclen) {
            // La valeur tient sur 3 bits : la conversion ne peut pas échouer.
            code_length_lengths[position] = u8::try_from(self.reader.bits(3)?).unwrap_or(0);
        }
        let code_length_tree = Huffman::new(&code_length_lengths)?;

        let lengths = self.read_code_lengths(&code_length_tree, hlit + hdist)?;
        if lengths[256] == 0 {
            return Err(Stop::corrupt("bloc dynamique : pas de code de fin de bloc"));
        }
        Ok((
            Huffman::new(&lengths[..hlit])?,
            Huffman::new(&lengths[hlit..])?,
        ))
    }

    /// Décode `total` longueurs de code avec les répétitions 16, 17 et 18
    /// (RFC 1951 §3.2.7).
    fn read_code_lengths(&mut self, tree: &Huffman, total: usize) -> Step<Vec<u8>> {
        let mut lengths = vec![0u8; total];
        let mut index = 0;
        while index < total {
            let symbol = tree.decode(&mut self.reader)?;
            let (value, repeat) = match symbol {
                0..=15 => (u8::try_from(symbol).unwrap_or(0), 1),
                16 => {
                    if index == 0 {
                        return Err(Stop::corrupt("bloc dynamique : répétition sans précédent"));
                    }
                    (lengths[index - 1], 3 + self.reader.bits(2)? as usize)
                }
                17 => (0, 3 + self.reader.bits(3)? as usize),
                18 => (0, 11 + self.reader.bits(7)? as usize),
                _ => {
                    return Err(Stop::corrupt(
                        "bloc dynamique : symbole de longueur invalide",
                    ))
                }
            };
            if index + repeat > total {
                return Err(Stop::corrupt(
                    "bloc dynamique : répétition au-delà des tables",
                ));
            }
            lengths[index..index + repeat].fill(value);
            index += repeat;
        }
        Ok(lengths)
    }

    /// Décode un bloc compressé avec les tables données (RFC 1951 §3.2.3 et §3.2.5).
    fn compressed_block(&mut self, litlen: &Huffman, dist: &Huffman) -> Step<()> {
        loop {
            let symbol = litlen.decode(&mut self.reader)?;
            if let Ok(byte) = u8::try_from(symbol) {
                self.reserve(1)?;
                self.output.push(byte);
                continue;
            }
            if symbol == 256 {
                return Ok(());
            }
            let length = self.read_length(symbol)?;
            let distance_code = dist.decode(&mut self.reader)?;
            let distance = self.read_distance(distance_code)?;
            self.copy_match(length, distance)?;
        }
    }

    /// Longueur d'une correspondance à partir du symbole 257..285.
    fn read_length(&mut self, symbol: u16) -> Step<usize> {
        let index = usize::from(symbol) - 257;
        let (Some(&base), Some(&extra)) = (LENGTH_BASE.get(index), LENGTH_EXTRA.get(index)) else {
            return Err(Stop::corrupt("code de longueur 286 ou 287 réservé"));
        };
        Ok(usize::from(base) + self.reader.bits(u32::from(extra))? as usize)
    }

    /// Distance d'une correspondance à partir du code de distance 0..29.
    fn read_distance(&mut self, code: u16) -> Step<usize> {
        let index = usize::from(code);
        let (Some(&base), Some(&extra)) = (DIST_BASE.get(index), DIST_EXTRA.get(index)) else {
            return Err(Stop::corrupt("code de distance 30 ou 31 réservé"));
        };
        Ok(usize::from(base) + self.reader.bits(u32::from(extra))? as usize)
    }

    /// Recopie `length` octets situés `distance` octets en arrière dans la
    /// sortie (RFC 1951 §3.2.3). Les recouvrements sont autorisés.
    fn copy_match(&mut self, length: usize, distance: usize) -> Step<()> {
        let len = self.output.len();
        if distance == 0 || distance > len {
            return Err(Stop::corrupt(
                "référence arrière avant le début des données",
            ));
        }
        self.reserve(length)?;
        let start = len - distance;
        if distance >= length {
            self.output.extend_from_within(start..start + length);
        } else {
            for i in 0..length {
                let byte = self.output[start + i];
                self.output.push(byte);
            }
        }
        Ok(())
    }

    /// Vérifie que `additional` octets de plus respectent la limite de sortie.
    fn reserve(&self, additional: usize) -> Step<()> {
        if self.output.len().saturating_add(additional) > self.max_output {
            return Err(Stop::TooLarge);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// zlib de « hello » (bloc Huffman fixe), vecteur classique.
    const HELLO: [u8; 13] = [
        0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00, 0x06, 0x2c, 0x02, 0x15,
    ];

    /// zlib niveau 0 de « Hello, stored block! » : un seul bloc stored.
    const STORED: [u8; 31] = [
        0x78, 0x01, 0x01, 0x14, 0x00, 0xeb, 0xff, 0x48, 0x65, 0x6c, 0x6c, 0x6f, 0x2c, 0x20, 0x73,
        0x74, 0x6f, 0x72, 0x65, 0x64, 0x20, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x21, 0x4b, 0x8c, 0x07,
        0x1e,
    ];

    /// zlib niveau 6 de `lorem()` : un bloc à codes de Huffman dynamiques
    /// (BTYPE = 10), vecteur produit indépendamment avec zlib 1.3.
    const DYNAMIC: [u8; 275] = [
        0x78, 0x9c, 0xed, 0x91, 0x4b, 0x6e, 0x83, 0x31, 0x08, 0x84, 0xaf, 0x32, 0x07, 0x88, 0xfe,
        0x53, 0xb4, 0xbb, 0x6c, 0x7b, 0x00, 0x6a, 0x4f, 0x52, 0x24, 0xdb, 0x38, 0x18, 0xa2, 0x1c,
        0xbf, 0x72, 0x93, 0x1e, 0xa2, 0x52, 0x77, 0x88, 0xc7, 0xc0, 0x7c, 0x9c, 0xcd, 0xd9, 0xa1,
        0x73, 0x65, 0x47, 0xb5, 0x66, 0x8e, 0xa5, 0x01, 0xe9, 0x8c, 0x13, 0x8a, 0x8d, 0xc5, 0x12,
        0x8c, 0x74, 0x48, 0xd5, 0xa9, 0xab, 0xe8, 0xb8, 0x82, 0x4d, 0xe3, 0x84, 0xc5, 0x8a, 0x6a,
        0xa0, 0xe6, 0xea, 0x56, 0x11, 0xec, 0xd3, 0x1c, 0x3a, 0x8a, 0x56, 0xad, 0x39, 0x02, 0x19,
        0x68, 0xf2, 0x69, 0x4e, 0x30, 0x9e, 0xd2, 0x44, 0x97, 0xeb, 0x10, 0x48, 0xd3, 0x5b, 0xca,
        0x81, 0x8f, 0x00, 0x87, 0x76, 0x48, 0x45, 0xd7, 0x1d, 0xdc, 0x39, 0x54, 0xfa, 0x09, 0xb7,
        0xd4, 0x85, 0x61, 0x2b, 0x3c, 0x2b, 0xf8, 0xa0, 0x17, 0x0d, 0x09, 0xb5, 0x81, 0x6c, 0x4d,
        0x7a, 0xb1, 0xa7, 0xf2, 0x6e, 0xd2, 0xa5, 0x7b, 0xd3, 0x8f, 0xa4, 0x4e, 0xf0, 0x01, 0x0a,
        0x8a, 0xf5, 0x6e, 0xd5, 0x9e, 0x06, 0x6e, 0x29, 0x71, 0xe0, 0x6d, 0x4b, 0x4a, 0x06, 0xa1,
        0x9e, 0xce, 0x97, 0x57, 0x1d, 0x70, 0x4e, 0xe7, 0x17, 0x47, 0xa5, 0x6b, 0xec, 0xc4, 0xdd,
        0x5a, 0xce, 0x90, 0x20, 0xee, 0xdb, 0x29, 0xb8, 0x16, 0x51, 0xb4, 0xb5, 0x5f, 0x42, 0x04,
        0x13, 0x97, 0xbc, 0xaa, 0x04, 0xc6, 0x3e, 0x08, 0x53, 0x5c, 0x25, 0xd2, 0x0f, 0xbc, 0x3f,
        0x0a, 0x67, 0x30, 0x37, 0xc6, 0x11, 0xb0, 0x52, 0x84, 0x45, 0x02, 0x25, 0xa7, 0x56, 0x89,
        0x3d, 0x61, 0x03, 0xd3, 0x4d, 0x2b, 0xc7, 0xa6, 0xb8, 0x49, 0xe9, 0x40, 0xc9, 0x36, 0x65,
        0xfb, 0x86, 0x5d, 0x2e, 0x5a, 0x54, 0x50, 0xb9, 0xe8, 0xbb, 0xda, 0xad, 0xed, 0x33, 0x64,
        0x03, 0xd2, 0x0a, 0xae, 0x17, 0xd7, 0xec, 0x07, 0xce, 0xff, 0xdf, 0x9b, 0x7f, 0xf7, 0x7b,
        0xdf, 0x57, 0xb8, 0x4a, 0x60,
    ];

    fn lorem() -> Vec<u8> {
        let paragraph = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do \
eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis \
nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure \
dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. \
Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim \
id est laborum. ";
        [paragraph.as_slice(), paragraph.as_slice()].concat()
    }

    #[test]
    fn decodes_fixed_huffman_hello() {
        assert_eq!(decode(&HELLO), Ok(b"hello".to_vec()));
    }

    #[test]
    fn decodes_stored_block() {
        assert_eq!(decode(&STORED), Ok(b"Hello, stored block!".to_vec()));
    }

    #[test]
    fn decodes_raw_stored_block_without_zlib_header() {
        // BFINAL = 1, BTYPE = 00, LEN = 5, NLEN = !5, puis « hello ».
        let raw = [0x01, 0x05, 0x00, 0xfa, 0xff, b'h', b'e', b'l', b'l', b'o'];
        let inflated = decode_detailed(&raw, DEFAULT_MAX_OUTPUT);
        assert_eq!(
            inflated,
            Ok(Inflated {
                data: b"hello".to_vec(),
                complete: true,
                checksum_ok: None,
                raw: true
            })
        );
    }

    #[test]
    fn decodes_dynamic_huffman_block() -> Result<()> {
        let inflated = decode_detailed(&DYNAMIC, DEFAULT_MAX_OUTPUT)?;
        assert_eq!(inflated.data, lorem());
        assert!(inflated.complete);
        assert_eq!(inflated.checksum_ok, Some(true));
        assert!(!inflated.raw);
        Ok(())
    }

    #[test]
    fn reports_checksum_mismatch_without_failing() -> Result<()> {
        let mut bad = HELLO;
        bad[12] ^= 0xFF;
        let inflated = decode_detailed(&bad, DEFAULT_MAX_OUTPUT)?;
        assert_eq!(inflated.data, b"hello");
        assert!(inflated.complete);
        assert_eq!(inflated.checksum_ok, Some(false));
        Ok(())
    }

    #[test]
    fn missing_checksum_is_reported_as_unknown() -> Result<()> {
        let inflated = decode_detailed(&HELLO[..9], DEFAULT_MAX_OUTPUT)?;
        assert_eq!(inflated.data, b"hello");
        assert!(inflated.complete);
        assert_eq!(inflated.checksum_ok, None);
        Ok(())
    }

    #[test]
    fn skips_leading_whitespace_before_header() {
        let mut padded = b"\r\n \t".to_vec();
        padded.extend_from_slice(&HELLO);
        assert_eq!(decode(&padded), Ok(b"hello".to_vec()));
    }

    #[test]
    fn accepts_raw_deflate_without_header() -> Result<()> {
        let inflated = decode_detailed(&HELLO[2..], DEFAULT_MAX_OUTPUT)?;
        assert_eq!(inflated.data, b"hello");
        assert!(inflated.raw);
        Ok(())
    }

    #[test]
    fn accepts_raw_deflate_after_whitespace() -> Result<()> {
        let mut padded = b"\n".to_vec();
        padded.extend_from_slice(&DYNAMIC[2..]);
        let inflated = decode_detailed(&padded, DEFAULT_MAX_OUTPUT)?;
        assert_eq!(inflated.data, lorem());
        assert!(inflated.raw);
        Ok(())
    }

    #[test]
    fn truncated_stream_returns_partial_data() -> Result<()> {
        let inflated = decode_detailed(&DYNAMIC[..150], DEFAULT_MAX_OUTPUT)?;
        assert!(!inflated.complete);
        assert!(!inflated.data.is_empty());
        assert!(lorem().starts_with(&inflated.data));
        Ok(())
    }

    #[test]
    fn corrupted_tail_returns_partial_data() -> Result<()> {
        let mut bad = DYNAMIC.to_vec();
        for byte in &mut bad[200..] {
            *byte = 0xFF;
        }
        let inflated = decode_detailed(&bad, DEFAULT_MAX_OUTPUT)?;
        assert!(!inflated.complete);
        assert!(!inflated.data.is_empty());
        Ok(())
    }

    #[test]
    fn empty_input_is_corrupt() {
        assert!(matches!(decode(&[]), Err(Error::Corrupt(_))));
    }

    #[test]
    fn reserved_block_type_is_corrupt() {
        // BFINAL = 1, BTYPE = 11 (réservé) dès le premier octet.
        assert!(matches!(
            decode(&[0xFF, 0xFF, 0xFF, 0xFF]),
            Err(Error::Corrupt(_))
        ));
    }

    #[test]
    fn random_data_never_panics() {
        // Générateur congruentiel simple pour rester déterministe.
        let mut state: u32 = 0x1234_5678;
        for len in [1usize, 2, 3, 7, 16, 64, 256, 1024] {
            let mut data = Vec::with_capacity(len);
            for _ in 0..len {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                data.push((state >> 24) as u8);
            }
            let _ = decode(&data);
        }
    }

    #[test]
    fn output_limit_is_enforced() {
        assert!(matches!(
            decode_with_limit(&DYNAMIC, 100),
            Err(Error::Corrupt(_))
        ));
        assert!(decode_with_limit(&DYNAMIC, lorem().len()).is_ok());
    }

    #[test]
    fn encode_round_trips() {
        for len in [
            0usize,
            1,
            100,
            MAX_STORED_BLOCK,
            MAX_STORED_BLOCK + 1,
            200_000,
        ] {
            let data: Vec<u8> = (0..len)
                .map(|i| u8::try_from(i % 251).unwrap_or(0))
                .collect();
            let encoded = encode(&data);
            let inflated = decode_detailed(&encoded, DEFAULT_MAX_OUTPUT);
            assert_eq!(
                inflated,
                Ok(Inflated {
                    data,
                    complete: true,
                    checksum_ok: Some(true),
                    raw: false
                })
            );
        }
    }

    #[test]
    fn adler32_matches_known_values() {
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(&lorem()), 0x57B8_4A60);
    }
}
