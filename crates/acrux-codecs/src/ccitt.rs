//! Filtre CCITTFaxDecode (ISO 32000-2 §7.4.6, tableau 11) : décodeur des
//! codages de télécopie ITU-T T.4 (Groupe 3 : une dimension « MH » et deux
//! dimensions « MR ») et ITU-T T.6 (Groupe 4 : « MMR »), écrit d'après les
//! recommandations, sans aucune dépendance ni code tiers.
//!
//! Paramètres pris en charge (tableau 11) : `K`, `Columns`, `Rows`,
//! `BlackIs1`, `EncodedByteAlign`, `EndOfLine`, `EndOfBlock`.
//! `DamagedRowsBeforeError` est ignoré : le décodeur est toujours tolérant et
//! rend ce qu'il a pu décoder, comme Acrobat qui affiche le haut de l'image.
//!
//! Couvert :
//! - codes de terminaison et de composition (« makeup ») blancs et noirs
//!   (T.4 §4.1.1, tableaux 2/T.4 et 3/T.4) et codes de composition étendus
//!   communs aux deux couleurs (tableau 3b/T.4, 1792 à 2560) ;
//! - modes bidimensionnels passe, horizontal et vertical V0, VR1 à VR3, VL1
//!   à VL3 (T.4 §4.2.1.3.1, tableau 4/T.4 ; T.6 §2.2.3, tableau 4/T.6) ;
//! - EOL (T.4 §4.1.2), bit d'étiquette 1-D/2-D après EOL quand K > 0
//!   (T.4 §4.2.1.3.4), RTC (six EOL, §4.1.3) et EOFB (deux EOL, T.6 §2.3) ;
//! - bits de remplissage avant EOL (T.4 §4.1.2) et alignement sur l'octet
//!   (`EncodedByteAlign`).
//!
//! Tolérance aux flux abîmés : EOL absents, RTC/EOFB absent, données finissant
//! au milieu d'une ligne (la ligne est complétée dans la couleur courante),
//! plages dépassant la largeur (tronquées), erreur de code après quelques
//! lignes (les lignes déjà décodées sont rendues, les lignes manquantes sont
//! blanches si `Rows` est connu). Seul un flux dont aucune ligne n'est
//! décodable rend `Err(Corrupt)`. Aucune entrée ne provoque de panique ni de
//! boucle infinie : chaque itération consomme au moins un bit et les tailles
//! sont bornées ([`MAX_COLUMNS`], [`MAX_OUTPUT_BYTES`]).
//!
//! Sortie : 1 bit par pixel, lignes alignées sur l'octet, `Rows` lignes (ou
//! autant que décodées si `Rows` vaut 0). Par défaut (`BlackIs1` faux) les
//! pixels noirs valent 0, conformément au tableau 11.
//!
//! Le décodeur T.6 est aussi utilisé par le filtre JBIG2Decode pour les
//! régions génériques en codage MMR (T.88 §6.2.6) via [`MmrDecoder`].

use acrux_core::{Error, Result};
use std::sync::OnceLock;

#[cfg(test)]
pub(crate) mod tests;

/// Largeur maximale acceptée, en pixels.
pub const MAX_COLUMNS: u32 = 1 << 20;

/// Taille maximale de la sortie, en octets (`Err(Corrupt)` au-delà).
pub const MAX_OUTPUT_BYTES: u64 = 1 << 30;

/// Paramètres du filtre (ISO 32000-2 §7.4.6, tableau 11).
// Les drapeaux reprennent un à un les entrées booléennes du tableau 11.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CcittParams {
    /// `K` : < 0 → Groupe 4 pur (T.6) ; 0 → Groupe 3 une dimension ; > 0 →
    /// Groupe 3 mixte, chaque ligne précédée d'un EOL et d'un bit d'étiquette.
    pub k: i32,
    /// `Columns` : pixels par ligne (défaut 1728).
    pub columns: u32,
    /// `Rows` : nombre de lignes, 0 si inconnu (le flux fixe la hauteur).
    pub rows: u32,
    /// `BlackIs1` : si faux (défaut), les pixels noirs sont codés 0 en sortie.
    pub black_is_1: bool,
    /// `EncodedByteAlign` : chaque ligne codée commence sur un octet.
    pub byte_align: bool,
    /// `EndOfLine` : des EOL sont attendus (ils sont acceptés dans tous les cas).
    pub end_of_line: bool,
    /// `EndOfBlock` : un RTC/EOFB termine les données (défaut vrai).
    pub end_of_block: bool,
}

impl Default for CcittParams {
    /// Valeurs par défaut du tableau 11.
    fn default() -> Self {
        CcittParams {
            k: 0,
            columns: 1728,
            rows: 0,
            black_is_1: false,
            byte_align: false,
            end_of_line: false,
            end_of_block: true,
        }
    }
}

fn corrupt(message: &str) -> Error {
    Error::Corrupt(format!("CCITT : {message}"))
}

// ---------------------------------------------------------------------------
// Tables de codes (T.4 §4.1.1, tableaux 2/T.4, 3a/T.4 et 3b/T.4)
// ---------------------------------------------------------------------------

/// Codes de terminaison blancs, indexés par la longueur de plage 0 à 63 :
/// (nombre de bits, code).
const WHITE_TERMINAL: [(u8, u16); 64] = [
    (8, 0b0011_0101),
    (6, 0b00_0111),
    (4, 0b0111),
    (4, 0b1000),
    (4, 0b1011),
    (4, 0b1100),
    (4, 0b1110),
    (4, 0b1111),
    (5, 0b1_0011),
    (5, 0b1_0100),
    (5, 0b0_0111),
    (5, 0b0_1000),
    (6, 0b00_1000),
    (6, 0b00_0011),
    (6, 0b11_0100),
    (6, 0b11_0101),
    (6, 0b10_1010),
    (6, 0b10_1011),
    (7, 0b010_0111),
    (7, 0b000_1100),
    (7, 0b000_1000),
    (7, 0b001_0111),
    (7, 0b000_0011),
    (7, 0b000_0100),
    (7, 0b010_1000),
    (7, 0b010_1011),
    (7, 0b001_0011),
    (7, 0b010_0100),
    (7, 0b001_1000),
    (8, 0b0000_0010),
    (8, 0b0000_0011),
    (8, 0b0001_1010),
    (8, 0b0001_1011),
    (8, 0b0001_0010),
    (8, 0b0001_0011),
    (8, 0b0001_0100),
    (8, 0b0001_0101),
    (8, 0b0001_0110),
    (8, 0b0001_0111),
    (8, 0b0010_1000),
    (8, 0b0010_1001),
    (8, 0b0010_1010),
    (8, 0b0010_1011),
    (8, 0b0010_1100),
    (8, 0b0010_1101),
    (8, 0b0000_0100),
    (8, 0b0000_0101),
    (8, 0b0000_1010),
    (8, 0b0000_1011),
    (8, 0b0101_0010),
    (8, 0b0101_0011),
    (8, 0b0101_0100),
    (8, 0b0101_0101),
    (8, 0b0010_0100),
    (8, 0b0010_0101),
    (8, 0b0101_1000),
    (8, 0b0101_1001),
    (8, 0b0101_1010),
    (8, 0b0101_1011),
    (8, 0b0100_1010),
    (8, 0b0100_1011),
    (8, 0b0011_0010),
    (8, 0b0011_0011),
    (8, 0b0011_0100),
];

/// Codes de composition blancs pour les plages 64, 128, …, 1728 (indice i →
/// plage 64 × (i + 1)).
const WHITE_MAKEUP: [(u8, u16); 27] = [
    (5, 0b1_1011),
    (5, 0b1_0010),
    (6, 0b01_0111),
    (7, 0b011_0111),
    (8, 0b0011_0110),
    (8, 0b0011_0111),
    (8, 0b0110_0100),
    (8, 0b0110_0101),
    (8, 0b0110_1000),
    (8, 0b0110_0111),
    (9, 0b0_1100_1100),
    (9, 0b0_1100_1101),
    (9, 0b0_1101_0010),
    (9, 0b0_1101_0011),
    (9, 0b0_1101_0100),
    (9, 0b0_1101_0101),
    (9, 0b0_1101_0110),
    (9, 0b0_1101_0111),
    (9, 0b0_1101_1000),
    (9, 0b0_1101_1001),
    (9, 0b0_1101_1010),
    (9, 0b0_1101_1011),
    (9, 0b0_1001_1000),
    (9, 0b0_1001_1001),
    (9, 0b0_1001_1010),
    (6, 0b01_1000),
    (9, 0b0_1001_1011),
];

/// Codes de terminaison noirs, plages 0 à 63.
const BLACK_TERMINAL: [(u8, u16); 64] = [
    (10, 0b00_0011_0111),
    (3, 0b010),
    (2, 0b11),
    (2, 0b10),
    (3, 0b011),
    (4, 0b0011),
    (4, 0b0010),
    (5, 0b0_0011),
    (6, 0b00_0101),
    (6, 0b00_0100),
    (7, 0b000_0100),
    (7, 0b000_0101),
    (7, 0b000_0111),
    (8, 0b0000_0100),
    (8, 0b0000_0111),
    (9, 0b0_0001_1000),
    (10, 0b00_0001_0111),
    (10, 0b00_0001_1000),
    (10, 0b00_0000_1000),
    (11, 0b000_0110_0111),
    (11, 0b000_0110_1000),
    (11, 0b000_0110_1100),
    (11, 0b000_0011_0111),
    (11, 0b000_0010_1000),
    (11, 0b000_0001_0111),
    (11, 0b000_0001_1000),
    (12, 0b0000_1100_1010),
    (12, 0b0000_1100_1011),
    (12, 0b0000_1100_1100),
    (12, 0b0000_1100_1101),
    (12, 0b0000_0110_1000),
    (12, 0b0000_0110_1001),
    (12, 0b0000_0110_1010),
    (12, 0b0000_0110_1011),
    (12, 0b0000_1101_0010),
    (12, 0b0000_1101_0011),
    (12, 0b0000_1101_0100),
    (12, 0b0000_1101_0101),
    (12, 0b0000_1101_0110),
    (12, 0b0000_1101_0111),
    (12, 0b0000_0110_1100),
    (12, 0b0000_0110_1101),
    (12, 0b0000_1101_1010),
    (12, 0b0000_1101_1011),
    (12, 0b0000_0101_0100),
    (12, 0b0000_0101_0101),
    (12, 0b0000_0101_0110),
    (12, 0b0000_0101_0111),
    (12, 0b0000_0110_0100),
    (12, 0b0000_0110_0101),
    (12, 0b0000_0101_0010),
    (12, 0b0000_0101_0011),
    (12, 0b0000_0010_0100),
    (12, 0b0000_0011_0111),
    (12, 0b0000_0011_1000),
    (12, 0b0000_0010_0111),
    (12, 0b0000_0010_1000),
    (12, 0b0000_0101_1000),
    (12, 0b0000_0101_1001),
    (12, 0b0000_0010_1011),
    (12, 0b0000_0010_1100),
    (12, 0b0000_0101_1010),
    (12, 0b0000_0110_0110),
    (12, 0b0000_0110_0111),
];

/// Codes de composition noirs, plages 64 à 1728.
const BLACK_MAKEUP: [(u8, u16); 27] = [
    (10, 0b00_0000_1111),
    (12, 0b0000_1100_1000),
    (12, 0b0000_1100_1001),
    (12, 0b0000_0101_1011),
    (12, 0b0000_0011_0011),
    (12, 0b0000_0011_0100),
    (12, 0b0000_0011_0101),
    (13, 0b0_0000_0110_1100),
    (13, 0b0_0000_0110_1101),
    (13, 0b0_0000_0100_1010),
    (13, 0b0_0000_0100_1011),
    (13, 0b0_0000_0100_1100),
    (13, 0b0_0000_0100_1101),
    (13, 0b0_0000_0111_0010),
    (13, 0b0_0000_0111_0011),
    (13, 0b0_0000_0111_0100),
    (13, 0b0_0000_0111_0101),
    (13, 0b0_0000_0111_0110),
    (13, 0b0_0000_0111_0111),
    (13, 0b0_0000_0101_0010),
    (13, 0b0_0000_0101_0011),
    (13, 0b0_0000_0101_0100),
    (13, 0b0_0000_0101_0101),
    (13, 0b0_0000_0101_1010),
    (13, 0b0_0000_0101_1011),
    (13, 0b0_0000_0110_0100),
    (13, 0b0_0000_0110_0101),
];

/// Codes de composition étendus, communs aux deux couleurs (tableau 3b/T.4),
/// plages 1792 à 2560 (indice i → 1792 + 64 × i).
const EXTENDED_MAKEUP: [(u8, u16); 13] = [
    (11, 0b000_0000_1000),
    (11, 0b000_0000_1100),
    (11, 0b000_0000_1101),
    (12, 0b0000_0001_0010),
    (12, 0b0000_0001_0011),
    (12, 0b0000_0001_0100),
    (12, 0b0000_0001_0101),
    (12, 0b0000_0001_0110),
    (12, 0b0000_0001_0111),
    (12, 0b0000_0001_1100),
    (12, 0b0000_0001_1101),
    (12, 0b0000_0001_1110),
    (12, 0b0000_0001_1111),
];

/// Longueur du plus long code de plage (composition noire) : les tables de
/// consultation sont indexées par les 13 prochains bits.
const PEEK_BITS: u32 = 13;

/// Table de consultation d'une couleur : entrée `(longueur << 12) | plage`,
/// 0 si les bits ne forment aucun code.
struct RunTable {
    entries: Vec<u16>,
}

impl RunTable {
    /// Construit la table à partir des codes de terminaison, de composition
    /// et de composition étendus. Rend aussi le nombre de collisions
    /// (deux codes dont l'un est préfixe de l'autre), nul pour les tables
    /// de la recommandation (vérifié par les tests).
    fn build(terminal: &[(u8, u16); 64], makeup: &[(u8, u16); 27]) -> (RunTable, usize) {
        let mut entries = vec![0u16; 1 << PEEK_BITS];
        let mut collisions = 0;
        let mut insert = |len: u8, code: u16, run: u16| {
            let shift = PEEK_BITS - u32::from(len);
            let base = usize::from(code) << shift;
            let entry = (u16::from(len) << 12) | run;
            for slot in &mut entries[base..base + (1 << shift)] {
                if *slot != 0 {
                    collisions += 1;
                }
                *slot = entry;
            }
        };
        for (run, &(len, code)) in (0u16..).zip(terminal.iter()) {
            insert(len, code, run);
        }
        for (i, &(len, code)) in makeup.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let run = 64 * (i as u16 + 1);
            insert(len, code, run);
        }
        for (i, &(len, code)) in EXTENDED_MAKEUP.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let run = 1792 + 64 * i as u16;
            insert(len, code, run);
        }
        (RunTable { entries }, collisions)
    }
}

/// Tables blanche et noire, construites une seule fois.
fn run_tables() -> &'static (RunTable, RunTable) {
    static TABLES: OnceLock<(RunTable, RunTable)> = OnceLock::new();
    TABLES.get_or_init(|| {
        (
            RunTable::build(&WHITE_TERMINAL, &WHITE_MAKEUP).0,
            RunTable::build(&BLACK_TERMINAL, &BLACK_MAKEUP).0,
        )
    })
}

// ---------------------------------------------------------------------------
// Lecture bit à bit
// ---------------------------------------------------------------------------

/// Lecteur de bits, poids fort en tête. Au-delà des données il fournit des
/// zéros ; [`BitReader::at_end`] signale l'épuisement.
struct BitReader<'a> {
    data: &'a [u8],
    /// Position en bits depuis le début.
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    /// Les `n` prochains bits (1 ≤ n ≤ 24), sans les consommer.
    fn peek(&self, n: u32) -> u32 {
        let byte = self.pos / 8;
        let mut acc: u32 = 0;
        for i in 0..4 {
            acc = (acc << 8) | u32::from(self.data.get(byte + i).copied().unwrap_or(0));
        }
        #[allow(clippy::cast_possible_truncation)]
        let shift = (self.pos % 8) as u32;
        (acc << shift) >> (32 - n)
    }

    fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }

    /// Vrai si toutes les données ont été consommées.
    fn at_end(&self) -> bool {
        self.pos >= self.data.len() * 8
    }

    /// Avance au prochain multiple de 8 bits.
    fn align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }

    /// Cherche un EOL (`000000000001`, T.4 §4.1.2), éventuellement précédé
    /// de bits de remplissage à zéro ; le consomme et rend vrai. Sans EOL,
    /// la position est inchangée.
    fn take_eol(&mut self) -> bool {
        let start = self.pos;
        let mut zeros = 0u32;
        while !self.at_end() && self.peek(1) == 0 {
            self.skip(1);
            zeros += 1;
        }
        if zeros >= 11 && !self.at_end() {
            self.skip(1);
            true
        } else {
            self.pos = start;
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Décodage d'une ligne en éléments de changement
// ---------------------------------------------------------------------------

/// Mode de codage bidimensionnel (T.4 tableau 4/T.4, T.6 tableau 4/T.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Pass,
    Horizontal,
    /// Mode vertical, décalage a1 − b1 de −3 à +3.
    Vertical(i32),
    /// EOL rencontré à la place d'un mode.
    Eol,
}

/// Issue du décodage d'une ligne.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowOutcome {
    /// Ligne complète.
    Complete,
    /// Ligne interrompue (EOL prématuré, code invalide, données épuisées) :
    /// les éléments de changement déjà lus restent valables.
    Truncated,
}

/// Décodeur de lignes : produit pour chaque ligne la liste de ses éléments
/// de changement (T.4 §4.2.1.3.1 : positions des pixels dont la couleur
/// diffère de celle du pixel précédent), en alternance à partir d'un passage
/// blanc → noir. Les positions valent au plus `columns`.
struct RowDecoder<'a> {
    reader: BitReader<'a>,
    columns: u32,
    /// Éléments de changement de la ligne de référence, terminés par deux
    /// sentinelles `columns`.
    reference: Vec<u32>,
    /// Éléments de changement de la ligne en cours.
    current: Vec<u32>,
    white: &'static RunTable,
    black: &'static RunTable,
}

impl<'a> RowDecoder<'a> {
    fn new(data: &'a [u8], columns: u32) -> Self {
        let (white, black) = run_tables();
        RowDecoder {
            reader: BitReader::new(data),
            columns,
            reference: vec![columns, columns],
            current: Vec::new(),
            white,
            black,
        }
    }

    /// Lit une longueur de plage complète : codes de composition éventuels
    /// puis code de terminaison (T.4 §4.1.1.2 : au-delà de 2560, plusieurs
    /// codes de composition se suivent).
    fn read_run(&mut self, black: bool) -> Option<u32> {
        let table = if black { self.black } else { self.white };
        let mut total: u32 = 0;
        loop {
            if self.reader.at_end() {
                return None;
            }
            let entry = table.entries[self.reader.peek(PEEK_BITS) as usize];
            if entry == 0 {
                return None;
            }
            let len = u32::from(entry >> 12);
            let run = u32::from(entry & 0x0FFF);
            self.reader.skip(len);
            total = total.saturating_add(run);
            if run < 64 {
                return Some(total);
            }
        }
    }

    /// Lit un code de mode bidimensionnel.
    fn read_mode(&mut self) -> Option<Mode> {
        if self.reader.at_end() {
            return None;
        }
        let bits = self.reader.peek(7);
        let (len, mode) = match bits {
            0b100_0000..=0b111_1111 => (1, Mode::Vertical(0)),
            0b011_0000..=0b011_1111 => (3, Mode::Vertical(1)),
            0b010_0000..=0b010_1111 => (3, Mode::Vertical(-1)),
            0b001_0000..=0b001_1111 => (3, Mode::Horizontal),
            0b000_1000..=0b000_1111 => (4, Mode::Pass),
            0b000_0110..=0b000_0111 => (6, Mode::Vertical(2)),
            0b000_0100..=0b000_0101 => (6, Mode::Vertical(-2)),
            0b000_0011 => (7, Mode::Vertical(3)),
            0b000_0010 => (7, Mode::Vertical(-3)),
            _ => {
                // Sept zéros : EOL (ou EOFB) ou extension non prise en charge.
                return self.reader.take_eol().then_some(Mode::Eol);
            }
        };
        self.reader.skip(len);
        Some(mode)
    }

    /// Décode une ligne en codage unidimensionnel (T.4 §4.1).
    fn decode_row_1d(&mut self) -> RowOutcome {
        self.current.clear();
        let mut pos: u32 = 0;
        let mut black = false;
        while pos < self.columns {
            let Some(run) = self.read_run(black) else {
                return RowOutcome::Truncated;
            };
            pos = pos.saturating_add(run).min(self.columns);
            self.current.push(pos);
            black = !black;
        }
        RowOutcome::Complete
    }

    /// Décode une ligne en codage bidimensionnel (T.4 §4.2.1.3, T.6 §2.2).
    fn decode_row_2d(&mut self) -> RowOutcome {
        self.current.clear();
        let columns = self.columns;
        // a0 : position de départ, −1 pour l'élément imaginaire avant la ligne.
        let mut a0: i64 = -1;
        let mut black = false;
        let mut ref_idx = 0usize;
        while a0 < i64::from(columns) {
            // b1 : premier élément de changement de la ligne de référence à
            // droite de a0 et de couleur opposée à celle de a0 ; b2 : le suivant.
            while ref_idx < self.reference.len() && i64::from(self.reference[ref_idx]) <= a0 {
                ref_idx += 1;
            }
            let i = ref_idx + usize::from((ref_idx & 1) != usize::from(black));
            let b1 = self.reference.get(i).copied().unwrap_or(columns);
            let b2 = self.reference.get(i + 1).copied().unwrap_or(columns);
            let Some(mode) = self.read_mode() else {
                return RowOutcome::Truncated;
            };
            match mode {
                Mode::Pass => {
                    // Les pixels de a0 à b2 gardent la couleur courante.
                    a0 = i64::from(b2);
                }
                Mode::Horizontal => {
                    let Some(run1) = self.read_run(black) else {
                        return RowOutcome::Truncated;
                    };
                    let Some(run2) = self.read_run(!black) else {
                        return RowOutcome::Truncated;
                    };
                    let start = u32::try_from(a0.max(0)).unwrap_or(0);
                    let a1 = start.saturating_add(run1).min(columns);
                    let a2 = a1.saturating_add(run2).min(columns);
                    self.current.push(a1);
                    self.current.push(a2);
                    a0 = i64::from(a2);
                }
                Mode::Vertical(delta) => {
                    let a1 = i64::from(b1) + i64::from(delta);
                    if a1 < 0 || a1 < a0 {
                        return RowOutcome::Truncated;
                    }
                    let a1 = u32::try_from(a1).unwrap_or(columns).min(columns);
                    self.current.push(a1);
                    a0 = i64::from(a1);
                    black = !black;
                }
                Mode::Eol => return RowOutcome::Truncated,
            }
        }
        RowOutcome::Complete
    }

    /// La ligne courante devient la ligne de référence de la suivante.
    fn commit_row(&mut self) {
        std::mem::swap(&mut self.reference, &mut self.current);
        self.reference.push(self.columns);
        self.reference.push(self.columns);
    }
}

// ---------------------------------------------------------------------------
// Rendu des lignes
// ---------------------------------------------------------------------------

/// Met à 1 les bits `[start, end)` de `row`.
fn set_bits(row: &mut [u8], start: usize, end: usize) {
    if start >= end {
        return;
    }
    let first = start / 8;
    let last = (end - 1) / 8;
    #[allow(clippy::cast_possible_truncation)]
    let head = 0xFFu8 >> (start % 8) as u8;
    #[allow(clippy::cast_possible_truncation)]
    let tail = 0xFFu8 << ((8 - end % 8) % 8) as u8;
    if first == last {
        if let Some(b) = row.get_mut(first) {
            *b |= head & tail;
        }
        return;
    }
    if let Some(b) = row.get_mut(first) {
        *b |= head;
    }
    for b in row.iter_mut().take(last).skip(first + 1) {
        *b = 0xFF;
    }
    if let Some(b) = row.get_mut(last) {
        *b |= tail;
    }
}

/// Écrit une ligne (1 = noir) à partir de ses éléments de changement ; la
/// ligne est supposée remplie de zéros.
fn render_row(row: &mut [u8], changes: &[u32], columns: u32) {
    for pair in changes.chunks(2) {
        let start = pair[0].min(columns);
        let end = pair.get(1).copied().unwrap_or(columns).min(columns);
        set_bits(row, start as usize, end as usize);
    }
}

// ---------------------------------------------------------------------------
// Filtre CCITTFaxDecode
// ---------------------------------------------------------------------------

/// Décode un flux CCITTFaxDecode.
///
/// Rend `Rows` lignes de `⌈Columns / 8⌉` octets (ou autant de lignes que le
/// flux en contient si `Rows` vaut 0), 1 bit par pixel, noir = 0 sauf si
/// `BlackIs1`.
///
/// # Errors
///
/// - `Error::Corrupt` si aucune ligne n'a pu être décodée, si `Columns` est
///   nul ou dépasse [`MAX_COLUMNS`], ou si la sortie dépasserait
///   [`MAX_OUTPUT_BYTES`].
pub fn decode(data: &[u8], params: &CcittParams) -> Result<Vec<u8>> {
    if params.columns == 0 || params.columns > MAX_COLUMNS {
        return Err(corrupt("largeur invalide"));
    }
    let columns = params.columns;
    let stride = (columns as usize).div_ceil(8);
    if u64::from(params.rows) * stride as u64 > MAX_OUTPUT_BYTES {
        return Err(corrupt("image trop grande"));
    }
    let mut decoder = RowDecoder::new(data, columns);
    let mut out: Vec<u8> = Vec::new();
    let mut rows_done: u32 = 0;
    let mut row = vec![0u8; stride];

    while params.rows == 0 || rows_done < params.rows {
        if (out.len() + stride) as u64 > MAX_OUTPUT_BYTES {
            break;
        }
        let Some(two_d) = start_row(&mut decoder.reader, params) else {
            break;
        };
        let outcome = if two_d {
            decoder.decode_row_2d()
        } else {
            decoder.decode_row_1d()
        };
        if outcome == RowOutcome::Truncated && decoder.current.is_empty() {
            // Rien de lisible : fin des données ou ligne corrompue dès le
            // premier code. Les lignes déjà décodées sont conservées.
            break;
        }
        row.fill(0);
        render_row(&mut row, &decoder.current, columns);
        out.extend_from_slice(&row);
        rows_done += 1;
        if outcome == RowOutcome::Truncated {
            break;
        }
        decoder.commit_row();
    }

    if rows_done == 0 {
        return Err(corrupt("aucune ligne décodable"));
    }
    // Lignes manquantes : blanches (bits à 0 avant application de BlackIs1).
    if params.rows > rows_done {
        out.resize(params.rows as usize * stride, 0);
    }
    if !params.black_is_1 {
        for b in &mut out {
            *b = !*b;
        }
    }
    Ok(out)
}

/// Prologue d'une ligne : alignement, EOL, bit d'étiquette. Rend `Some(true)`
/// si la ligne est codée en deux dimensions, `None` si les données sont
/// terminées (RTC, EOFB ou épuisement).
fn start_row(reader: &mut BitReader<'_>, params: &CcittParams) -> Option<bool> {
    if params.k < 0 && params.byte_align {
        reader.align();
    }
    let mut two_d = params.k < 0;
    let mut eols = consume_eols(reader, params.k, &mut two_d);
    if eols == 0 && params.k >= 0 && params.byte_align {
        reader.align();
        eols = consume_eols(reader, params.k, &mut two_d);
    }
    if eols >= 2 || reader.at_end() {
        // RTC (T.4 §4.1.3), EOFB (T.6 §2.3) ou données épuisées.
        return None;
    }
    if params.k > 0 && eols == 0 {
        // EOL absent : le bit d'étiquette est tout de même attendu (T.4 le
        // place après l'EOL ; les flux sans EOL le conservent en général).
        two_d = reader.peek(1) == 0;
        reader.skip(1);
    }
    Some(two_d)
}

/// Consomme les EOL consécutifs (au plus deux : au-delà c'est un RTC) et,
/// quand K > 0, leur bit d'étiquette (T.4 §4.2.1.3.4 : 1 = ligne 1-D,
/// 0 = ligne 2-D). Rend le nombre d'EOL consommés.
fn consume_eols(reader: &mut BitReader<'_>, k: i32, two_d: &mut bool) -> u32 {
    let mut eols = 0;
    while eols < 2 && reader.take_eol() {
        eols += 1;
        if k > 0 {
            *two_d = reader.peek(1) == 0;
            reader.skip(1);
        }
    }
    eols
}

// ---------------------------------------------------------------------------
// Décodeur MMR réutilisable (JBIG2, T.88 §6.2.6)
// ---------------------------------------------------------------------------

/// Décodeur T.6 pur, ligne par ligne, utilisé par JBIG2 pour les régions
/// génériques MMR : la ligne de référence persiste d'un appel à l'autre, ce
/// qui permet de décoder plusieurs plans consécutifs d'un même flux
/// (T.88 §C.5) sans réinitialisation.
pub(crate) struct MmrDecoder<'a> {
    decoder: RowDecoder<'a>,
}

impl<'a> MmrDecoder<'a> {
    /// Crée un décodeur sur `data` pour des lignes de `columns` pixels.
    pub(crate) fn new(data: &'a [u8], columns: u32) -> Self {
        MmrDecoder {
            decoder: RowDecoder::new(data, columns.clamp(1, MAX_COLUMNS)),
        }
    }

    /// Décode la ligne suivante et rend ses éléments de changement (paires
    /// début/fin de plages noires, dernière fin implicite = largeur), ou
    /// `None` si le flux est terminé (EOFB, données épuisées, erreur).
    pub(crate) fn next_row(&mut self) -> Option<&[u32]> {
        if self.decoder.reader.at_end() {
            return None;
        }
        let outcome = self.decoder.decode_row_2d();
        if outcome == RowOutcome::Truncated && self.decoder.current.is_empty() {
            return None;
        }
        self.decoder.commit_row();
        // Après `commit_row`, la ligne décodée est la référence (sans ses
        // deux sentinelles).
        let n = self.decoder.reference.len() - 2;
        Some(&self.decoder.reference[..n])
    }

    /// Consomme un EOFB s'il est présent (T.88 §6.2.6 : facultatif).
    #[cfg(test)]
    pub(crate) fn skip_eofb(&mut self) {
        if self.decoder.reader.take_eol() {
            self.decoder.reader.take_eol();
        }
    }

    /// Nombre d'octets consommés (position arrondie à l'octet supérieur).
    #[cfg(test)]
    pub(crate) fn bytes_consumed(&self) -> usize {
        self.decoder.reader.pos.div_ceil(8)
    }
}

/// Remplit une ligne d'un pixel par octet (1 = noir) à partir d'éléments de
/// changement ; la ligne est supposée nulle.
pub(crate) fn paint_changes(row: &mut [u8], changes: &[u32]) {
    let columns = row.len();
    for pair in changes.chunks(2) {
        let start = (pair[0] as usize).min(columns);
        let end = pair.get(1).map_or(columns, |&e| (e as usize).min(columns));
        if start < end {
            row[start..end].iter_mut().for_each(|p| *p = 1);
        }
    }
}
