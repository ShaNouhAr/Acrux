//! Décodeur LZW (filtre LZWDecode, ISO 32000-2 §7.4.4.2).
//!
//! Variante LZW de PDF, héritée de TIFF :
//!
//! - codes de largeur variable, de 9 à 12 bits, rangés bit de poids fort en
//!   premier dans le flux ;
//! - codes 0 à 255 : un octet littéral ; 256 : ClearTable ; 257 : EOD ;
//!   258 et suivants : séquences apprises, jusqu'à 4095 ;
//! - la largeur passe à 10, 11 puis 12 bits quand la table atteint 512, 1024
//!   et 2048 entrées. Avec /EarlyChange = 1 (défaut) le passage se fait un
//!   code plus tôt ; avec 0 il est retardé autant que possible.
//!
//! Tolérance : un flux tronqué (sans EOD) rend ce qui a été décodé ; un code
//! impossible interrompt le décodage en rendant ce qui précède, ou
//! `Err(Corrupt)` si rien n'a été produit.

use acrux_core::{Error, Result};

/// Code de remise à zéro de la table (§7.4.4.2).
const CLEAR_TABLE: u16 = 256;
/// Code de fin de données (§7.4.4.2).
const END_OF_DATA: u16 = 257;
/// Premier code libre après la remise à zéro.
const FIRST_FREE_CODE: u16 = 258;
/// Largeur minimale et maximale d'un code, en bits.
const MIN_CODE_WIDTH: u32 = 9;
const MAX_CODE_WIDTH: u32 = 12;
/// Nombre maximal d'entrées de la table (codes sur 12 bits).
const MAX_TABLE_SIZE: usize = 1 << MAX_CODE_WIDTH;

/// Décode un flux LZWDecode.
///
/// `early_change` vaut `true` pour /EarlyChange 1 (défaut de la spécification)
/// et `false` pour /EarlyChange 0.
///
/// # Errors
///
/// `Error::Corrupt` si le flux référence un code inexistant avant d'avoir
/// produit le moindre octet.
pub fn decode(data: &[u8], early_change: bool) -> Result<Vec<u8>> {
    let mut decoder = Decoder::new(data, early_change);
    let stopped_early = decoder.run().is_err();
    if stopped_early && decoder.output.is_empty() {
        return Err(Error::Corrupt(
            "flux LZW : code invalide dès le début".to_string(),
        ));
    }
    Ok(decoder.output)
}

/// Lecteur de codes, bit de poids fort en premier (§7.4.4.2).
struct CodeReader<'a> {
    data: &'a [u8],
    pos: usize,
    buffer: u32,
    count: u32,
}

impl CodeReader<'_> {
    /// Lit un code de `width` bits ; `None` si le flux est épuisé.
    fn read(&mut self, width: u32) -> Option<u16> {
        while self.count < width {
            let byte = *self.data.get(self.pos)?;
            self.pos += 1;
            self.buffer = (self.buffer << 8) | u32::from(byte);
            self.count += 8;
        }
        self.count -= width;
        let code = (self.buffer >> self.count) & ((1 << width) - 1);
        self.buffer &= (1 << self.count) - 1;
        // Un code fait au plus 12 bits : la conversion ne peut pas échouer.
        u16::try_from(code).ok()
    }
}

/// État du décodeur.
struct Decoder<'a> {
    reader: CodeReader<'a>,
    early_change: bool,
    /// Table des séquences ; les 258 premières entrées sont fixes.
    table: Vec<Vec<u8>>,
    /// Séquence émise par le code précédent, `None` juste après un ClearTable.
    previous: Option<Vec<u8>>,
    width: u32,
    output: Vec<u8>,
}

impl<'a> Decoder<'a> {
    fn new(data: &'a [u8], early_change: bool) -> Self {
        let mut table = Vec::with_capacity(MAX_TABLE_SIZE);
        for byte in 0..=255u8 {
            table.push(vec![byte]);
        }
        table.push(Vec::new()); // 256 : ClearTable
        table.push(Vec::new()); // 257 : EOD
        Decoder {
            reader: CodeReader {
                data,
                pos: 0,
                buffer: 0,
                count: 0,
            },
            early_change,
            table,
            previous: None,
            width: MIN_CODE_WIDTH,
            output: Vec::new(),
        }
    }

    /// Décode jusqu'à EOD ou la fin du flux ; `Err(())` sur un code impossible.
    fn run(&mut self) -> std::result::Result<(), ()> {
        while let Some(code) = self.reader.read(self.width) {
            match code {
                CLEAR_TABLE => self.clear(),
                END_OF_DATA => return Ok(()),
                _ => self.emit(code)?,
            }
        }
        Ok(())
    }

    /// Remet la table et la largeur de code à leur état initial.
    fn clear(&mut self) {
        self.table.truncate(usize::from(FIRST_FREE_CODE));
        self.previous = None;
        self.width = MIN_CODE_WIDTH;
    }

    /// Émet la séquence d'un code et apprend une nouvelle entrée.
    fn emit(&mut self, code: u16) -> std::result::Result<(), ()> {
        let index = usize::from(code);
        let sequence = match (self.table.get(index), &self.previous) {
            (Some(entry), _) => entry.clone(),
            // Cas KwKwK : le code n'existe pas encore mais vaut exactement
            // la séquence précédente suivie de son premier octet.
            (None, Some(previous)) if index == self.table.len() => {
                let mut sequence = previous.clone();
                sequence.push(previous[0]);
                sequence
            }
            _ => return Err(()),
        };
        self.output.extend_from_slice(&sequence);

        if let Some(previous) = self.previous.take() {
            self.learn(previous, sequence[0]);
        }
        self.previous = Some(sequence);
        Ok(())
    }

    /// Ajoute `previous + first` à la table et ajuste la largeur des codes.
    fn learn(&mut self, mut previous: Vec<u8>, first: u8) {
        if self.table.len() >= MAX_TABLE_SIZE {
            // Table pleine : on attend un ClearTable sans rien apprendre.
            return;
        }
        previous.push(first);
        self.table.push(previous);
        // Le décodeur a toujours une entrée de retard sur l'encodeur ; avec
        // EarlyChange le passage à la largeur supérieure se fait un code plus tôt.
        let threshold = self.table.len() + usize::from(self.early_change);
        if threshold >= 1 << self.width && self.width < MAX_CODE_WIDTH {
            self.width += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Vecteur de l'ISO 32000-2 §7.4.4.2 : « -----A---B ».
    const SPEC_INPUT: [u8; 9] = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
    const SPEC_OUTPUT: [u8; 10] = [45, 45, 45, 45, 45, 65, 45, 45, 45, 66];

    #[test]
    fn decodes_spec_example() {
        assert_eq!(decode(&SPEC_INPUT, true), Ok(SPEC_OUTPUT.to_vec()));
        assert_eq!(decode(&SPEC_INPUT, false), Ok(SPEC_OUTPUT.to_vec()));
    }

    #[test]
    fn truncated_stream_returns_partial_output() {
        // Les 5 premiers octets contiennent Clear, '-', 258, 258 : « ----- ».
        assert_eq!(decode(&SPEC_INPUT[..5], true), Ok(vec![45, 45, 45, 45, 45]));
    }

    #[test]
    fn empty_input_is_empty_output() {
        assert_eq!(decode(&[], true), Ok(Vec::new()));
    }

    #[test]
    fn invalid_first_code_is_corrupt() {
        // Clear (256) puis 300 : aucune entrée 300 n'existe encore.
        // 100000000 100101100 → 80 4B 00.
        assert!(matches!(
            decode(&[0x80, 0x4B, 0x00], true),
            Err(Error::Corrupt(_))
        ));
    }

    #[test]
    fn invalid_code_after_output_returns_partial() {
        // Clear, '-', puis 511 (inexistant) : on garde le '-'.
        // 100000000 000101101 111111111 → 80 0B 7F E0.
        assert_eq!(decode(&[0x80, 0x0B, 0x7F, 0xE0], true), Ok(vec![45]));
    }

    /// Encodeur LZW minimal, uniquement pour les tests aller-retour.
    fn encode(data: &[u8], early_change: bool) -> Vec<u8> {
        struct Writer {
            out: Vec<u8>,
            buffer: u64,
            count: u32,
        }
        impl Writer {
            fn put(&mut self, code: u16, width: u32) {
                self.buffer = (self.buffer << width) | u64::from(code);
                self.count += width;
                while self.count >= 8 {
                    self.count -= 8;
                    self.out.push(((self.buffer >> self.count) & 0xFF) as u8);
                }
            }
        }

        let mut writer = Writer {
            out: Vec::new(),
            buffer: 0,
            count: 0,
        };
        let mut dictionary: HashMap<Vec<u8>, u16> = HashMap::new();
        let reset = |dictionary: &mut HashMap<Vec<u8>, u16>| {
            dictionary.clear();
            for byte in 0..=255u8 {
                dictionary.insert(vec![byte], u16::from(byte));
            }
        };
        reset(&mut dictionary);
        let mut next_code: u16 = FIRST_FREE_CODE;
        let mut width = MIN_CODE_WIDTH;
        writer.put(CLEAR_TABLE, width);

        let mut current: Vec<u8> = Vec::new();
        for &byte in data {
            let mut candidate = current.clone();
            candidate.push(byte);
            if dictionary.contains_key(&candidate) {
                current = candidate;
                continue;
            }
            writer.put(dictionary[&current], width);
            if usize::from(next_code) < MAX_TABLE_SIZE {
                dictionary.insert(candidate, next_code);
                next_code += 1;
                // L'encodeur crée l'entrée n puis émet le code suivant : le
                // premier code de 10 bits suit la création de l'entrée 511
                // (EarlyChange 1) ou 512 (EarlyChange 0).
                let threshold = usize::from(next_code) + usize::from(early_change);
                if threshold > 1 << width && width < MAX_CODE_WIDTH {
                    width += 1;
                }
            } else {
                writer.put(CLEAR_TABLE, width);
                reset(&mut dictionary);
                next_code = FIRST_FREE_CODE;
                width = MIN_CODE_WIDTH;
            }
            current = vec![byte];
        }
        if !current.is_empty() {
            writer.put(dictionary[&current], width);
        }
        writer.put(END_OF_DATA, width);
        if writer.count > 0 {
            writer.put(0, 8 - writer.count);
        }
        writer.out
    }

    #[test]
    fn test_encoder_reproduces_spec_example() {
        assert_eq!(encode(&SPEC_OUTPUT, true), SPEC_INPUT.to_vec());
    }

    fn sample(len: usize) -> Vec<u8> {
        // Texte pseudo-aléatoire mais compressible : assez long pour franchir
        // les largeurs 10, 11 et 12 bits puis remplir la table.
        let words: [&[u8]; 6] = [b"lorem ", b"ipsum ", b"dolor ", b"sit ", b"amet ", b"pdf "];
        let mut state: u32 = 7;
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            out.extend_from_slice(words[(state >> 16) as usize % words.len()]);
        }
        out.truncate(len);
        out
    }

    #[test]
    fn round_trip_across_all_code_widths() {
        for early_change in [true, false] {
            let data = sample(40_000);
            let encoded = encode(&data, early_change);
            assert_eq!(decode(&encoded, early_change), Ok(data));
        }
    }

    #[test]
    fn early_change_matters_once_table_reaches_511_entries() {
        // Au-delà de 253 codes, les flux EarlyChange 0 et 1 divergent.
        let data = sample(3_000);
        let encoded = encode(&data, true);
        assert_ne!(encode(&data, false), encoded);
        assert_ne!(decode(&encoded, false), Ok(data));
    }

    #[test]
    fn random_data_never_panics() {
        let mut state: u32 = 0xBEEF;
        for len in [1usize, 2, 5, 33, 200, 4096] {
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            let _ = decode(&data, true);
            let _ = decode(&data, false);
        }
    }
}
