//! Le flux de bits d'U3D : décodeur arithmétique et gestionnaire de contextes
//! (ECMA-363, §10 et annexe A).
//!
//! Un fichier U3D est une suite de blocs, et **les données de chaque bloc sont
//! comprimées** par un codeur arithmétique adaptatif. On ne peut donc rien y
//! lire — pas même un entier — sans ce décodeur.
//!
//! # Comment il marche
//!
//! Le codeur représente une suite de symboles par un seul nombre entre 0 et 1,
//! tenu en virgule fixe sur seize bits (`low` et `high`). Chaque symbole
//! rétrécit l'intervalle proportionnellement à sa probabilité : plus un
//! symbole est fréquent, plus sa part est grande, moins il coûte de bits. Le
//! décodeur refait le même chemin — il lit le nombre, cherche dans quelle part
//! il tombe, et rétrécit l'intervalle de la même façon.
//!
//! Les probabilités viennent d'un **contexte**, désigné par un numéro :
//!
//! - le contexte 0 sert aux valeurs **non comprimées** : chaque octet y est
//!   écrit comme un symbole parmi 256 équiprobables (et ses bits sont
//!   retournés, héritage du format) ;
//! - les contextes 1 à 0x3FF sont **adaptatifs** : leur histogramme part vide
//!   et se remplit à mesure, si bien qu'une valeur qui revient souvent finit
//!   par ne presque plus rien coûter ;
//! - les contextes ≥ 0x400 sont **statiques** : `contexte - 0x400` symboles
//!   équiprobables, ce qui sert aux indices dont on connaît déjà la borne.
//!
//! La norme ne définit normativement que le **codage** ; le décodage en est
//! l'inverse exact, et c'est lui qui est écrit ici. L'écriture, elle, sert aux
//! épreuves (`bits::Writer`), pour vérifier l'aller-retour.

// Le curseur du flux mêle des mots de 32 bits, des décalages signés et des
// positions en octets : les conversions sont bornées par la taille du bloc,
// et les écrire en `try_from` masquerait l'algorithme.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

/// Contexte des lectures non comprimées.
pub(crate) const CONTEXT8: u32 = 0;

/// Premier contexte statique ; en dessous, les contextes sont adaptatifs.
pub(crate) const STATIC_FULL: u32 = 0x0000_0400;

/// Au-delà, les valeurs sont écrites telles quelles.
pub(crate) const MAX_RANGE: u32 = STATIC_FULL + 0x0000_3FFF;

/// Nombre d'occurrences au-delà duquel un histogramme est divisé par deux :
/// il garde ainsi sa faculté de s'adapter, et ne déborde jamais.
const ELEPHANT: u32 = 0x0000_1FFF;

/// Plus grand symbole rangé dans un histogramme.
const MAX_SYMBOL: u32 = 0x0000_FFFF;

/// Masques de l'intervalle de probabilité, en virgule fixe 16 bits.
const HALF: u32 = 0x0000_8000;
const NOT_HALF: u32 = 0x0000_7FFF;
const QUARTER: u32 = 0x0000_4000;
const NOT_THREE_QUARTER: u32 = 0x0000_3FFF;

/// Renversement des quatre bits de poids faible.
const SWAP4: [u32; 16] = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15];

/// Nombre de bits que `low` et `high` ont déjà en commun, lu sur leurs quatre
/// bits de poids fort.
const READ_COUNT: [u32; 16] = [4, 3, 2, 2, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0];

/// Masques employés pour ce décalage rapide.
const FAST_NOT: [u32; 5] = [
    0x0000_FFFF,
    0x0000_7FFF,
    0x0000_3FFF,
    0x0000_1FFF,
    0x0000_0FFF,
];

/// Renverse les huit bits d'une valeur.
fn swap8(value: u32) -> u32 {
    (SWAP4[(value & 0xF) as usize] << 4) | SWAP4[((value >> 4) & 0xF) as usize]
}

/// Histogramme d'un contexte adaptatif.
///
/// `counts[s]` est le nombre de fois que le symbole `s` a été vu ;
/// `cumulative[s]` la somme des occurrences de `s` **et de tous les symboles
/// plus grands**. `cumulative[0]` est donc le total.
#[derive(Default)]
struct Histogram {
    counts: Vec<u16>,
    cumulative: Vec<u16>,
}

/// Les histogrammes de tous les contextes adaptatifs d'un bloc.
///
/// Les contextes repartent de zéro à chaque bloc : c'est ce qui permet de lire
/// un bloc sans avoir lu les précédents.
#[derive(Default)]
pub(crate) struct Contexts {
    histograms: std::collections::HashMap<u32, Histogram>,
}

/// Vrai si le contexte a un histogramme qui s'adapte.
fn adaptive(context: u32) -> bool {
    context < STATIC_FULL && context != CONTEXT8
}

impl Contexts {
    /// Ajoute une occurrence du symbole au contexte.
    fn add(&mut self, context: u32, symbol: u32) {
        if !adaptive(context) || symbol >= MAX_SYMBOL {
            return;
        }
        let h = self.histograms.entry(context).or_insert_with(|| Histogram {
            // Le symbole 0 est l'échappement : il vaut 1 dès le départ, sans
            // quoi rien ne pourrait jamais être écrit dans un contexte neuf.
            counts: vec![1],
            cumulative: vec![1],
        });
        let symbol = symbol as usize;
        if h.counts.len() <= symbol {
            h.counts.resize(symbol + 32, 0);
            h.cumulative.resize(symbol + 32, 0);
        }
        if u32::from(h.cumulative[0]) >= ELEPHANT {
            // Tout diviser par deux en repartant de la fin : les cumuls se
            // recalculent au passage.
            let mut running = 0u16;
            for i in (0..h.counts.len()).rev() {
                h.counts[i] >>= 1;
                running = running.saturating_add(h.counts[i]);
                h.cumulative[i] = running;
            }
            // L'échappement doit rester possible.
            h.counts[0] = h.counts[0].saturating_add(1);
            h.cumulative[0] = h.cumulative[0].saturating_add(1);
        }
        h.counts[symbol] = h.counts[symbol].saturating_add(1);
        for c in &mut h.cumulative[..=symbol] {
            *c = c.saturating_add(1);
        }
    }

    /// Fréquence d'un symbole.
    fn frequency(&self, context: u32, symbol: u32) -> u32 {
        if !adaptive(context) {
            return 1;
        }
        match self.histograms.get(&context) {
            Some(h) => h.counts.get(symbol as usize).map_or(0, |c| u32::from(*c)),
            // Tant que l'histogramme n'existe pas, seul l'échappement est là.
            None => u32::from(symbol == 0),
        }
    }

    /// Fréquence cumulée : les occurrences du symbole exclu et de tous ceux
    /// qui le précèdent.
    fn cumulative(&self, context: u32, symbol: u32) -> u32 {
        if !adaptive(context) {
            return symbol.saturating_sub(1);
        }
        let Some(h) = self.histograms.get(&context) else {
            return 0;
        };
        let total = u32::from(h.cumulative[0]);
        match h.cumulative.get(symbol as usize) {
            Some(c) => total - u32::from(*c),
            None => total,
        }
    }

    /// Total des occurrences d'un contexte.
    fn total(&self, context: u32) -> u32 {
        if adaptive(context) {
            return self
                .histograms
                .get(&context)
                .map_or(1, |h| u32::from(h.cumulative[0]));
        }
        if context == CONTEXT8 {
            return 256;
        }
        context - STATIC_FULL
    }

    /// Symbole dont la part contient cette fréquence cumulée.
    fn symbol_at(&self, context: u32, frequency: u32) -> u32 {
        if !adaptive(context) {
            return frequency + 1;
        }
        let Some(h) = self.histograms.get(&context) else {
            return 0;
        };
        if frequency == 0 || u32::from(h.cumulative[0]) < frequency {
            return 0;
        }
        let mut found = 0;
        for i in 0..u32::try_from(h.cumulative.len()).unwrap_or(u32::MAX) {
            if self.cumulative(context, i) <= frequency {
                found = i;
            } else {
                break;
            }
        }
        found
    }
}

/// Lecteur du flux de bits d'un bloc.
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    /// Bornes de l'intervalle de probabilité.
    high: u32,
    low: u32,
    /// Bits en attente, à cause du rétrécissement autour de la moitié.
    underflow: u32,
    /// Position de lecture : mot de 32 bits, puis bit dans ce mot.
    word: usize,
    offset: i32,
    contexts: Contexts,
}

impl<'a> Reader<'a> {
    /// Nouveau lecteur sur les données d'un bloc.
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Reader {
            data,
            high: 0x0000_FFFF,
            low: 0,
            underflow: 0,
            word: 0,
            offset: 0,
            contexts: Contexts::default(),
        }
    }

    /// Le mot de 32 bits d'indice donné.
    ///
    /// Le dernier mot d'un bloc est souvent **incomplet** — la taille des
    /// données n'a aucune raison d'être un multiple de quatre : les octets
    /// manquants valent zéro, et ceux qui sont là comptent.
    fn word_at(&self, index: usize) -> u32 {
        let at = index * 4;
        let mut bytes = [0u8; 4];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = self.data.get(at + i).copied().unwrap_or(0);
        }
        u32::from_le_bytes(bytes)
    }

    /// Avance d'un mot.
    fn next_word(&mut self) {
        self.word += 1;
    }

    /// Ramène le décalage dans `[0, 32)`.
    fn normalize(&mut self) {
        while self.offset >= 32 {
            self.offset -= 32;
            self.next_word();
        }
    }

    /// Lit un bit.
    fn read_bit(&mut self) -> u32 {
        let value = (self.word_at(self.word) >> self.offset) & 1;
        self.offset += 1;
        self.normalize();
        value
    }

    /// Lit quinze bits, renversés quatre par quatre comme le veut le format.
    fn read_15(&mut self) -> u32 {
        let mut value = self.word_at(self.word) >> self.offset;
        if self.offset > 17 {
            value |= self.word_at(self.word + 1) << (32 - self.offset);
        }
        value = value.wrapping_add(value);
        let swapped = SWAP4[((value >> 12) & 0xF) as usize]
            | (SWAP4[((value >> 8) & 0xF) as usize] << 4)
            | (SWAP4[((value >> 4) & 0xF) as usize] << 8)
            | (SWAP4[(value & 0xF) as usize] << 12);
        self.offset += 15;
        self.normalize();
        swapped
    }

    /// Position de lecture, en bits depuis le début du bloc.
    fn bit_position(&self) -> usize {
        (self.word << 5) + self.offset as usize
    }

    /// Reprend la lecture à une position donnée.
    fn seek(&mut self, position: usize) {
        self.word = position >> 5;
        self.offset = (position & 0x1F) as i32;
    }

    /// Lit un symbole dans le contexte donné. Le symbole 0 est
    /// l'échappement : la valeur suit alors, non comprimée.
    fn read_symbol(&mut self, context: u32) -> u32 {
        // Le mot de code : seize bits pris à la position courante, qu'on
        // relâche ensuite — seuls les bits vraiment consommés font avancer.
        let position = self.bit_position();
        let mut code = self.read_bit();
        self.offset += self.underflow as i32;
        self.normalize();
        code = (code << 15) | self.read_15();
        self.seek(position);

        let total = self.contexts.total(context).max(1);
        let range = self.high.wrapping_sub(self.low).wrapping_add(1);
        // Où tombe le mot de code dans l'intervalle courant. Le +1 et le -1
        // compensent la division entière.
        let code_frequency = total
            .wrapping_mul(1 + code - self.low)
            .wrapping_sub(1)
            .checked_div(range)
            .unwrap_or(0);
        let symbol = self.contexts.symbol_at(context, code_frequency);
        let value_cumulative = self.contexts.cumulative(context, symbol);
        let value_frequency = self.contexts.frequency(context, symbol);

        let mut low = self.low;
        let mut high = low + range * (value_cumulative + value_frequency) / total - 1;
        low += range * value_cumulative / total;
        self.contexts.add(context, symbol);

        // Les bits que `low` et `high` ont en commun sont acquis : on les
        // chasse de l'intervalle, et ce sont eux qu'on a consommés.
        let mut bits = READ_COUNT[(((low >> 12) ^ (high >> 12)) & 0xF) as usize];
        low &= FAST_NOT[bits as usize];
        high &= FAST_NOT[bits as usize];
        high = (high << bits) | ((1 << bits) - 1);
        low <<= bits;

        let mut masked_low = HALF & low;
        let mut masked_high = HALF & high;
        while (masked_low | masked_high) == 0 || (masked_low == HALF && masked_high == HALF) {
            low = (NOT_HALF & low) << 1;
            high = ((NOT_HALF & high) << 1) | 1;
            masked_low = HALF & low;
            masked_high = HALF & high;
            bits += 1;
        }
        let (saved_low, saved_high) = (masked_low, masked_high);
        if bits > 0 {
            bits += self.underflow;
            self.underflow = 0;
        }

        // Rétrécissement autour de la moitié : l'intervalle se resserre sans
        // qu'aucun bit ne se décide. On les compte pour plus tard.
        let mut masked_low = QUARTER & low;
        let mut masked_high = QUARTER & high;
        let mut underflow = 0;
        while masked_low == QUARTER && masked_high == 0 {
            low &= NOT_THREE_QUARTER;
            high &= NOT_THREE_QUARTER;
            low += low;
            high += high;
            high |= 1;
            masked_low = QUARTER & low;
            masked_high = QUARTER & high;
            underflow += 1;
        }
        self.underflow += underflow;
        self.low = low | saved_low;
        self.high = high | saved_high;

        self.offset += bits as i32;
        self.normalize();
        symbol
    }

    /// Lit un octet non comprimé.
    pub(crate) fn u8(&mut self) -> u8 {
        let value = self.read_symbol(CONTEXT8).wrapping_sub(1);
        u8::try_from(swap8(value & 0xFF)).unwrap_or(0)
    }

    /// Lit un entier de seize bits non comprimé.
    pub(crate) fn u16(&mut self) -> u16 {
        let low = u16::from(self.u8());
        let high = u16::from(self.u8());
        low | (high << 8)
    }

    /// Lit un entier de trente-deux bits non comprimé.
    pub(crate) fn u32(&mut self) -> u32 {
        let low = u32::from(self.u16());
        let high = u32::from(self.u16());
        low | (high << 16)
    }

    /// Lit un flottant simple précision.
    pub(crate) fn f32(&mut self) -> f32 {
        f32::from_bits(self.u32())
    }

    /// Lit un entier de trente-deux bits dans un contexte.
    pub(crate) fn compressed_u32(&mut self, context: u32) -> u32 {
        if context == CONTEXT8 || context >= MAX_RANGE {
            return self.u32();
        }
        let symbol = self.read_symbol(context);
        if symbol != 0 {
            return symbol - 1;
        }
        let value = self.u32();
        self.contexts.add(context, value + 1);
        value
    }

    /// Lit une chaîne : son nombre d'octets, puis ses octets.
    pub(crate) fn string(&mut self) -> String {
        let count = self.u16();
        let mut out = String::with_capacity(usize::from(count));
        for _ in 0..count {
            out.push(char::from(self.u8()));
        }
        out
    }

    /// Position de lecture, en octets depuis le début du bloc.
    ///
    /// Sert aux blocs qui mêlent des champs comprimés et des blocs imbriqués
    /// écrits tels quels : c'est là que ces derniers commencent.
    pub(crate) fn byte_position(&self) -> usize {
        self.bit_position().div_ceil(8)
    }

    /// Saute des entiers de trente-deux bits.
    pub(crate) fn skip_u32(&mut self, count: usize) {
        for _ in 0..count {
            let _ = self.u32();
        }
    }
}

/// Écrivain du flux de bits d'un bloc.
///
/// C'est l'algorithme normatif de la norme (§10.4), dont le lecteur ci-dessus
/// est l'inverse. Il sert à **fabriquer** des fichiers d'épreuve : un modèle
/// écrit puis relu doit redonner exactement ce qu'on y a mis, et c'est ce que
/// vérifient les tests.
#[cfg(test)]
pub(crate) struct Writer {
    high: u32,
    low: u32,
    underflow: u32,
    /// Les bits écrits, du premier au dernier.
    bits: Vec<bool>,
    contexts: Contexts,
    /// Vrai dès qu'une valeur a été écrite : il faudra vider l'état.
    wrote: bool,
}

#[cfg(test)]
impl Writer {
    pub(crate) fn new() -> Self {
        Writer {
            high: 0x0000_FFFF,
            low: 0,
            underflow: 0,
            bits: Vec::new(),
            contexts: Contexts::default(),
            wrote: false,
        }
    }

    fn write_bit(&mut self, bit: bool) {
        self.bits.push(bit);
    }

    /// Écrit un symbole ; rend vrai si c'est l'échappement qui est sorti,
    /// auquel cas la valeur doit suivre, non comprimée.
    fn write_symbol(&mut self, context: u32, value: u32) -> bool {
        let mut symbol = value + 1;
        let total = self.contexts.total(context).max(1);
        let mut frequency = self.contexts.frequency(context, symbol);
        let mut cumulative = self.contexts.cumulative(context, symbol);
        let escaped = frequency == 0;
        if escaped {
            symbol = 0;
            cumulative = self.contexts.cumulative(context, symbol);
            frequency = self.contexts.frequency(context, symbol);
        }
        let range = self.high - self.low + 1;
        self.high = self.low + range * (cumulative + frequency) / total - 1;
        self.low += range * cumulative / total;
        self.contexts.add(context, symbol);

        // Les bits que les deux bornes ont en commun sont acquis : on les
        // écrit, en purgeant d'abord les bits en attente.
        while (self.high & HALF) == (self.low & HALF) {
            let bit = (self.high & HALF) != 0;
            self.write_bit(bit);
            self.high = ((self.high & NOT_HALF) << 1) | 1;
            self.low = (self.low & NOT_HALF) << 1;
            while self.underflow > 0 {
                self.write_bit(!bit);
                self.underflow -= 1;
            }
        }
        // Rétrécissement autour de la moitié : on retient un bit.
        while (self.low & QUARTER) == QUARTER && (self.high & QUARTER) == 0 {
            self.high = ((self.high & NOT_THREE_QUARTER) << 1) | 1 | HALF;
            self.low = (self.low & NOT_THREE_QUARTER) << 1;
            self.underflow += 1;
        }
        self.wrote = true;
        escaped
    }

    /// Écrit un octet non comprimé.
    pub(crate) fn u8(&mut self, value: u8) {
        let swapped = swap8(u32::from(value));
        self.write_symbol(CONTEXT8, swapped);
    }

    pub(crate) fn u16(&mut self, value: u16) {
        self.u8(u8::try_from(value & 0xFF).unwrap_or(0));
        self.u8(u8::try_from(value >> 8).unwrap_or(0));
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.u16(u16::try_from(value & 0xFFFF).unwrap_or(0));
        self.u16(u16::try_from(value >> 16).unwrap_or(0));
    }

    pub(crate) fn f32(&mut self, value: f32) {
        self.u32(value.to_bits());
    }

    /// Écrit un entier dans un contexte.
    pub(crate) fn compressed_u32(&mut self, context: u32, value: u32) {
        if context == CONTEXT8 || context >= MAX_RANGE {
            self.u32(value);
            return;
        }
        if self.write_symbol(context, value) {
            self.u32(value);
            self.contexts.add(context, value + 1);
        }
    }

    /// Écrit une chaîne : son nombre d'octets, puis ses octets.
    pub(crate) fn string(&mut self, value: &str) {
        let bytes = value.as_bytes();
        self.u16(u16::try_from(bytes.len()).unwrap_or(0));
        for b in bytes {
            self.u8(*b);
        }
    }

    /// Termine le flux et rend les octets du bloc.
    ///
    /// La norme demande d'écrire un entier nul de trente-deux bits : c'est ce
    /// qui garantit que le lecteur dispose de tous les bits dont il a besoin
    /// pour décoder la dernière valeur.
    pub(crate) fn finish(mut self) -> Vec<u8> {
        if self.wrote {
            self.u32(0);
        }
        let mut out = vec![0u8; self.bits.len().div_ceil(8)];
        for (i, bit) in self.bits.iter().enumerate() {
            if *bit {
                // Les bits se rangent du poids faible au poids fort dans
                // chaque octet, comme le lecteur les reprend.
                out[i / 8] |= 1 << (i % 8);
            }
        }
        out
    }
}
