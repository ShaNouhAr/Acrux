//! Filtre DCTDecode (ISO 32000-2 §7.4.8) : décodeur JPEG écrit d'après
//! ITU-T T.81 (ISO/IEC 10918-1), sans aucune dépendance ni code tiers.
//!
//! Couvert :
//! - processus séquentiel de base (SOF0), séquentiel étendu à codage de
//!   Huffman (SOF1) et progressif à codage de Huffman (SOF2, annexe G) ;
//! - précision 8 bits ; 12 bits acceptés et ramenés à 8 bits ;
//! - 1 à 4 composantes, facteurs d'échantillonnage 1 à 4 (A.1.1), scans
//!   entrelacés ou non (A.2) ;
//! - tables DQT 8 et 16 bits, tables DHT incomplètes, intervalles de reprise
//!   (DRI, RSTn), DNL, segments APP0 (JFIF) et APP14 (Adobe, drapeau
//!   `transform`), APPn / COM inconnus ignorés ;
//! - transformations de couleur YCbCr→RGB (JFIF) et YCCK→CMYK ; les JPEG CMYK
//!   « Adobe » sont rendus tels quels, le drapeau [`JpegImage::adobe_inverted`]
//!   signalant à la couche PDF qu'ils sont stockés inversés.
//!
//! Non couvert (`Err(Unsupported)`) : codage arithmétique (SOF9 à SOF11,
//! SOF13 à SOF15), mode hiérarchique (SOF5 à SOF7, DHP), mode sans perte
//! (SOF3), extensions SOF11/SOF15.
//!
//! Tolérance aux fichiers abîmés (comportement d'Acrobat) : octets parasites
//! avant SOI ignorés, EOI manquant accepté, données tronquées ou corrompues →
//! image partielle (les blocs jamais décodés restent gris), marqueur RST
//! manquant ou décalé → resynchronisation sur le marqueur suivant. Aucune
//! entrée ne provoque de panique ni de boucle infinie : toutes les tailles
//! sont bornées ([`MAX_PIXELS`], [`MAX_BUFFER_BYTES`]) et toutes les tables
//! de Huffman sont validées avant usage.
//!
//! Sur-échantillonnage des chromas : interpolation triangulaire (« fancy
//! upsampling », poids 3/4-1/4, les échantillons chroma étant centrés entre
//! les échantillons luma comme le prescrit JFIF) pour les rapports 2:1
//! horizontaux et/ou verticaux ; réplication (plus proche voisin) pour les
//! autres rapports, entiers ou non.

use acrux_core::{Error, Result};

pub mod encode;

#[cfg(test)]
mod tests;

/// Nombre maximal de pixels (largeur × hauteur) accepté.
pub const MAX_PIXELS: u64 = 1 << 31;

/// Taille maximale, en octets, du tampon de sortie et, séparément, du tampon
/// de coefficients DCT ; au-delà l'image est refusée (`Err(Corrupt)`).
pub const MAX_BUFFER_BYTES: u64 = 1 << 30;

/// Image JPEG décodée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JpegImage {
    /// Largeur en pixels (X du SOF, B.2.2).
    pub width: u32,
    /// Hauteur en pixels (Y du SOF ou du DNL).
    pub height: u32,
    /// Nombre de composantes : 1 (gris), 3 (RGB) ou 4 (CMYK).
    pub components: u8,
    /// Échantillons entrelacés, ligne par ligne, `components` octets par pixel,
    /// après transformation de couleur (YCbCr→RGB, YCCK→CMYK).
    pub data: Vec<u8>,
    /// Vrai si un segment APP14 « Adobe » est présent et que l'image a quatre
    /// composantes : les valeurs CMYK sont alors stockées inversées (Photoshop,
    /// Acrobat) et c'est à la couche PDF d'appliquer /Decode.
    pub adobe_inverted: bool,
    /// Vrai si le fichier est progressif (SOF2).
    pub progressive: bool,
}

/// Marqueurs (T.81 tableau B.1).
mod marker {
    pub const SOF0: u8 = 0xC0;
    pub const SOF1: u8 = 0xC1;
    pub const SOF2: u8 = 0xC2;
    pub const SOF3: u8 = 0xC3;
    pub const DHT: u8 = 0xC4;
    pub const SOF5: u8 = 0xC5;
    pub const SOF7: u8 = 0xC7;
    pub const JPG: u8 = 0xC8;
    pub const SOF9: u8 = 0xC9;
    pub const SOF11: u8 = 0xCB;
    pub const SOF13: u8 = 0xCD;
    pub const SOF15: u8 = 0xCF;
    pub const RST0: u8 = 0xD0;
    pub const RST7: u8 = 0xD7;
    pub const SOI: u8 = 0xD8;
    pub const EOI: u8 = 0xD9;
    pub const SOS: u8 = 0xDA;
    pub const DQT: u8 = 0xDB;
    pub const DNL: u8 = 0xDC;
    pub const DRI: u8 = 0xDD;
    pub const DHP: u8 = 0xDE;
    pub const EXP: u8 = 0xDF;
    pub const APP14: u8 = 0xEE;
    pub const TEM: u8 = 0x01;
}

/// Ordre zigzag (T.81 figure A.6) : indice naturel (ligne × 8 + colonne) de
/// la k-ième position zigzag.
const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Coefficients par bloc 8 × 8 (A.3.3).
const BLOCK: usize = 64;

/// Nombre de bits résolus par la table de consultation rapide de Huffman.
const LOOKUP_BITS: u32 = 9;

fn corrupt(message: &str) -> Error {
    Error::Corrupt(format!("JPEG : {message}"))
}

// ---------------------------------------------------------------------------
// Tables de Huffman (annexe C, F.2.2.3)
// ---------------------------------------------------------------------------

/// Table de Huffman prête au décodage : consultation rapide sur
/// [`LOOKUP_BITS`] bits, puis procédure DECODE (figure F.16) pour les codes
/// plus longs.
struct HuffTable {
    /// Entrée `(longueur << 8) | valeur` ; 0 = code plus long ou invalide.
    lookup: Vec<u16>,
    /// Plus grand code de chaque longueur (1 à 16) ; -1 si aucun.
    maxcode: [i32; 17],
    /// Plus petit code de chaque longueur.
    mincode: [i32; 17],
    /// Indice dans `values` du premier code de chaque longueur.
    valptr: [usize; 17],
    /// HUFFVAL.
    values: Vec<u8>,
}

impl HuffTable {
    /// Construit la table à partir de BITS et HUFFVAL (C.2, figures C.1 à
    /// C.3). Une table incomplète est acceptée ; une table sursaturée
    /// (plus de codes qu'il n'en existe pour une longueur) est refusée.
    fn build(bits: &[u8; 16], values: &[u8]) -> Result<HuffTable> {
        let total: usize = bits.iter().map(|&b| usize::from(b)).sum();
        if total > 256 || values.len() < total {
            return Err(corrupt("table de Huffman DHT incohérente"));
        }
        let mut table = HuffTable {
            lookup: vec![0; 1 << LOOKUP_BITS],
            maxcode: [-1; 17],
            mincode: [0; 17],
            valptr: [0; 17],
            values: values[..total].to_vec(),
        };
        let mut code: i32 = 0;
        let mut k = 0usize;
        for len in 1..=16usize {
            let count = usize::from(bits[len - 1]);
            if count > 0 {
                table.valptr[len] = k;
                table.mincode[len] = code;
                for _ in 0..count {
                    if code >= (1 << len) {
                        return Err(corrupt("table de Huffman sursaturée"));
                    }
                    if len <= LOOKUP_BITS as usize {
                        let shift = LOOKUP_BITS as usize - len;
                        let base = usize::try_from(code).unwrap_or(0) << shift;
                        let entry =
                            (u16::try_from(len).unwrap_or(0) << 8) | u16::from(table.values[k]);
                        for slot in &mut table.lookup[base..base + (1 << shift)] {
                            *slot = entry;
                        }
                    }
                    code += 1;
                    k += 1;
                }
                table.maxcode[len] = code - 1;
            }
            code <<= 1;
        }
        Ok(table)
    }

    /// Procédure DECODE (F.2.2.3) : lit un code et rend la valeur associée,
    /// ou `None` si les bits suivants ne forment aucun code de la table.
    fn decode(&self, reader: &mut BitReader<'_>) -> Option<u8> {
        let peek = reader.peek16();
        let entry = self.lookup[(peek >> (16 - LOOKUP_BITS)) as usize];
        if entry != 0 {
            reader.consume(u32::from(entry >> 8));
            #[allow(clippy::cast_possible_truncation)]
            let value = entry as u8;
            return Some(value);
        }
        for len in (LOOKUP_BITS as usize + 1)..=16 {
            #[allow(clippy::cast_possible_wrap)]
            let code = (peek >> (16 - len)) as i32;
            if code <= self.maxcode[len] {
                reader.consume(u32::try_from(len).unwrap_or(16));
                let offset = usize::try_from(code - self.mincode[len]).ok()?;
                return self.values.get(self.valptr[len] + offset).copied();
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Lecture bit à bit du flux entropique (F.2.2.5, B.1.1.2, B.1.1.5)
// ---------------------------------------------------------------------------

/// Lecteur de bits du segment entropique. Les octets `FF 00` sont
/// « débourrés » en un seul `FF` ; un marqueur (`FF xx`, xx ≠ 0) arrête la
/// lecture : le lecteur fournit alors des zéros et comptabilise les bits de
/// bourrage consommés, ce qui permet de détecter une troncature.
struct BitReader<'a> {
    data: &'a [u8],
    /// Prochain octet à lire (pointe sur le `FF` d'un marqueur rencontré).
    pos: usize,
    /// Accumulateur, bits valides alignés à gauche.
    acc: u64,
    nbits: u32,
    /// Bits de zéros fournis au-delà des données réelles.
    pad_bits: u32,
    /// Marqueur rencontré (`Some(0)` signifie fin des données).
    stop: Option<u8>,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        BitReader {
            data,
            pos,
            acc: 0,
            nbits: 0,
            pad_bits: 0,
            stop: None,
        }
    }

    /// Remplit l'accumulateur jusqu'à au moins 57 bits.
    fn refill(&mut self) {
        while self.nbits <= 56 {
            let byte = if self.stop.is_some() {
                self.pad_bits += 8;
                0
            } else {
                match self.data.get(self.pos) {
                    None => {
                        self.stop = Some(0);
                        continue;
                    }
                    Some(&0xFF) => match self.data.get(self.pos + 1) {
                        Some(&0x00) => {
                            self.pos += 2;
                            0xFF
                        }
                        Some(&0xFF) => {
                            // Octet de remplissage (B.1.1.2).
                            self.pos += 1;
                            continue;
                        }
                        Some(&m) => {
                            self.stop = Some(m);
                            continue;
                        }
                        None => {
                            self.stop = Some(0);
                            continue;
                        }
                    },
                    Some(&b) => {
                        self.pos += 1;
                        b
                    }
                }
            };
            self.acc |= u64::from(byte) << (56 - self.nbits);
            self.nbits += 8;
        }
    }

    /// Les 16 prochains bits, sans les consommer.
    fn peek16(&mut self) -> u32 {
        if self.nbits < 16 {
            self.refill();
        }
        #[allow(clippy::cast_possible_truncation)]
        let bits = (self.acc >> 48) as u32;
        bits
    }

    fn consume(&mut self, n: u32) {
        self.acc <<= n;
        self.nbits -= n;
    }

    /// Procédure RECEIVE (F.2.2.4) : `n` bits (0 ≤ n ≤ 16), poids fort en tête.
    fn receive(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let bits = self.peek16() >> (16 - n);
        self.consume(n);
        bits
    }

    fn bit(&mut self) -> bool {
        self.receive(1) == 1
    }

    /// Vrai si des bits de bourrage ont été consommés : les données sont
    /// épuisées ou le flux est corrompu.
    fn overrun(&self) -> bool {
        self.pad_bits > self.nbits
    }

    /// Traitement d'un intervalle de reprise (F.2.2.5, E.1.4) : abandonne
    /// les bits restants et consomme le marqueur RSTn attendu. Si le flux est
    /// désynchronisé, cherche le prochain marqueur. Rend le numéro n du
    /// marqueur RSTn trouvé, ou `None` si le scan doit s'arrêter (autre
    /// marqueur ou fin des données).
    fn restart(&mut self) -> Option<u8> {
        self.acc = 0;
        self.nbits = 0;
        self.pad_bits = 0;
        loop {
            match self.stop {
                Some(m) if (marker::RST0..=marker::RST7).contains(&m) => {
                    self.pos += 2;
                    self.stop = None;
                    return Some(m - marker::RST0);
                }
                Some(_) => return None,
                None => {}
            }
            // Pas encore sur un marqueur : avance jusqu'au prochain.
            match next_marker(self.data, self.pos) {
                Some((m, _)) if (marker::RST0..=marker::RST7).contains(&m) => {
                    self.stop = Some(m);
                    self.pos = position_of_marker(self.data, self.pos);
                }
                _ => return None,
            }
        }
    }
}

/// Cherche le prochain marqueur à partir de `pos` : un octet `FF` suivi d'un
/// octet autre que `00` (octet bourré) ou `FF` (remplissage). Rend le code du
/// marqueur et la position de l'octet qui le suit.
fn next_marker(data: &[u8], pos: usize) -> Option<(u8, usize)> {
    let mut i = pos;
    while i + 1 < data.len() {
        if data[i] == 0xFF && data[i + 1] != 0x00 && data[i + 1] != 0xFF {
            return Some((data[i + 1], i + 2));
        }
        i += 1;
    }
    None
}

/// Position du `FF` du prochain marqueur (voir [`next_marker`]), ou fin des
/// données.
fn position_of_marker(data: &[u8], pos: usize) -> usize {
    next_marker(data, pos).map_or(data.len(), |(_, after)| after - 2)
}

// ---------------------------------------------------------------------------
// Structures de trame et de scan (B.2.2, B.2.3)
// ---------------------------------------------------------------------------

/// Composante d'image et ses coefficients DCT accumulés.
struct Component {
    /// Identifiant Ci.
    id: u8,
    /// Facteurs d'échantillonnage Hi, Vi (1 à 4).
    h: usize,
    v: usize,
    /// Table de quantification Tqi.
    tq: usize,
    /// Dimensions en échantillons (A.1.1) : ⌈X × Hi / Hmax⌉, ⌈Y × Vi / Vmax⌉.
    width: usize,
    height: usize,
    /// Dimensions du tampon de blocs, arrondies à l'unité MCU.
    blocks_w: usize,
    blocks_h: usize,
    /// Coefficients quantifiés, ordre naturel, 64 par bloc.
    coeffs: Vec<i16>,
    /// Prédicteur DC courant (F.1.1.5.1).
    dc_pred: i32,
}

struct Frame {
    width: usize,
    height: usize,
    precision: u8,
    progressive: bool,
    hmax: usize,
    vmax: usize,
    /// Nombre de MCU par ligne et de lignes de MCU (A.2.3).
    mcus_x: usize,
    mcus_y: usize,
    components: Vec<Component>,
}

/// Composante d'un scan : indice dans la trame et tables sélectionnées.
struct ScanComponent {
    index: usize,
    dc_table: usize,
    ac_table: usize,
}

/// En-tête de scan (B.2.3).
struct ScanHeader {
    components: Vec<ScanComponent>,
    ss: usize,
    se: usize,
    ah: u32,
    al: u32,
}

// ---------------------------------------------------------------------------
// Décodeur
// ---------------------------------------------------------------------------

struct Decoder<'a> {
    data: &'a [u8],
    /// Tables de quantification, ordre naturel (B.2.4.1).
    qt: [Option<[u16; BLOCK]>; 4],
    dc_tables: [Option<HuffTable>; 4],
    ac_tables: [Option<HuffTable>; 4],
    frame: Option<Frame>,
    restart_interval: usize,
    /// Drapeau `transform` du segment APP14 Adobe.
    adobe_transform: Option<u8>,
    /// Nombre de scans dont les données ont été abordées.
    scans: usize,
    /// Compteur EOBRUN des scans AC progressifs (G.1.2.2).
    eob_run: u32,
}

/// Décode un flux JPEG complet.
///
/// # Errors
///
/// - `Error::Corrupt` si aucune trame exploitable n'est trouvée (pas de SOI,
///   pas de SOF, pas de SOS, dimensions absurdes, table invalide avant le
///   premier scan…) ;
/// - `Error::Unsupported` pour le codage arithmétique, le mode hiérarchique
///   et le mode sans perte.
///
/// Une fois le premier scan abordé, toute anomalie arrête le décodage et
/// l'image partielle est rendue.
pub fn decode(data: &[u8]) -> Result<JpegImage> {
    let mut decoder = Decoder {
        data,
        qt: [None; 4],
        dc_tables: [None, None, None, None],
        ac_tables: [None, None, None, None],
        frame: None,
        restart_interval: 0,
        adobe_transform: None,
        scans: 0,
        eob_run: 0,
    };
    decoder.run()?;
    decoder.finish()
}

/// Lit les dimensions et le nombre de composantes sans décoder l'image.
///
/// # Errors
///
/// `Error::Corrupt` si l'en-tête est absent ou invalide, `Error::Unsupported`
/// pour les processus non pris en charge (voir [`decode`]).
pub fn read_header(data: &[u8]) -> Result<(u32, u32, u8)> {
    let mut pos = find_soi(data)?;
    while let Some((m, body)) = next_marker(data, pos) {
        match m {
            marker::SOF0 | marker::SOF1 | marker::SOF2 => {
                let (segment, _) = read_segment(data, body)?;
                let (precision, height, width, count) = parse_frame_dimensions(segment)?;
                let height = if height == 0 {
                    find_dnl(data, body)?
                } else {
                    height
                };
                check_precision(m, precision)?;
                check_dimensions(width, height, count)?;
                return Ok((width, height, count));
            }
            marker::SOI | marker::TEM | marker::RST0..=marker::RST7 => pos = body,
            marker::EOI | marker::SOS => break,
            _ if is_unsupported_frame(m) => return Err(unsupported_frame(m)),
            _ => pos = read_segment(data, body)?.1,
        }
    }
    Err(corrupt("en-tête de trame SOF absent"))
}

/// Position après le marqueur SOI, en ignorant les octets parasites.
fn find_soi(data: &[u8]) -> Result<usize> {
    data.windows(2)
        .position(|w| w == [0xFF, marker::SOI])
        .map(|p| p + 2)
        .ok_or_else(|| corrupt("marqueur SOI absent"))
}

/// Lit un segment à longueur (B.1.1.4) : rend son corps (hors champ longueur)
/// et la position qui suit.
fn read_segment(data: &[u8], pos: usize) -> Result<(&[u8], usize)> {
    let len = data
        .get(pos..pos + 2)
        .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
        .ok_or_else(|| corrupt("segment tronqué"))?;
    if len < 2 {
        return Err(corrupt("longueur de segment invalide"));
    }
    let end = pos + len;
    let body = data
        .get(pos + 2..end)
        .ok_or_else(|| corrupt("segment tronqué"))?;
    Ok((body, end))
}

fn is_unsupported_frame(m: u8) -> bool {
    matches!(
        m,
        marker::SOF3
            | marker::SOF5..=marker::SOF7
            | marker::JPG
            | marker::SOF9..=marker::SOF11
            | marker::SOF13..=marker::SOF15
            | marker::DHP
            | marker::EXP
    )
}

fn unsupported_frame(m: u8) -> Error {
    let what = match m {
        marker::SOF3 => "JPEG sans perte (SOF3)",
        marker::SOF5..=marker::SOF7 | marker::DHP | marker::EXP => "JPEG hiérarchique",
        _ => "JPEG à codage arithmétique",
    };
    Error::Unsupported(what.to_owned())
}

/// Champs P, Y, X, Nf de l'en-tête de trame (B.2.2).
fn parse_frame_dimensions(segment: &[u8]) -> Result<(u8, u32, u32, u8)> {
    if segment.len() < 6 {
        return Err(corrupt("en-tête SOF tronqué"));
    }
    let precision = segment[0];
    let height = u32::from(u16::from_be_bytes([segment[1], segment[2]]));
    let width = u32::from(u16::from_be_bytes([segment[3], segment[4]]));
    let count = segment[5];
    Ok((precision, height, width, count))
}

fn check_precision(sof: u8, precision: u8) -> Result<()> {
    match (sof, precision) {
        (_, 8) | (marker::SOF1 | marker::SOF2, 12) => Ok(()),
        _ => Err(Error::Unsupported(format!(
            "JPEG : précision {precision} bits"
        ))),
    }
}

fn check_dimensions(width: u32, height: u32, count: u8) -> Result<()> {
    if width == 0 || height == 0 {
        return Err(corrupt("dimensions nulles"));
    }
    if !matches!(count, 1..=4) {
        return Err(corrupt("nombre de composantes invalide"));
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_PIXELS || pixels * u64::from(count) > MAX_BUFFER_BYTES {
        return Err(corrupt("image trop grande"));
    }
    Ok(())
}

/// Cherche le segment DNL (B.2.5) qui définit le nombre de lignes lorsque
/// le SOF indique Y = 0. Les données entropiques ne peuvent pas contenir
/// `FF DC`, la recherche est donc sûre.
fn find_dnl(data: &[u8], from: usize) -> Result<u32> {
    let mut pos = from;
    while let Some((m, body)) = next_marker(data, pos) {
        if m == marker::DNL {
            let (segment, _) = read_segment(data, body)?;
            if segment.len() >= 2 {
                return Ok(u32::from(u16::from_be_bytes([segment[0], segment[1]])));
            }
            break;
        }
        pos = body;
    }
    Err(corrupt("hauteur inconnue (Y = 0 sans DNL)"))
}

impl Decoder<'_> {
    /// Boucle sur les marqueurs (B.2.1) jusqu'à EOI ou fin des données.
    fn run(&mut self) -> Result<()> {
        let mut pos = find_soi(self.data)?;
        while let Some((m, body)) = next_marker(self.data, pos) {
            let outcome = match m {
                marker::SOI | marker::TEM | marker::RST0..=marker::RST7 => {
                    pos = body;
                    continue;
                }
                marker::EOI => break,
                marker::SOS => match self.parse_scan_header(body) {
                    Ok((header, data_pos)) => {
                        pos = self.decode_scan(&header, data_pos);
                        continue;
                    }
                    Err(e) => Err(e),
                },
                _ if is_unsupported_frame(m) => Err(unsupported_frame(m)),
                _ => read_segment(self.data, body).and_then(|(segment, end)| {
                    self.parse_segment(m, segment, body)?;
                    pos = end;
                    Ok(())
                }),
            };
            if let Err(e) = outcome {
                // Après le premier scan, une anomalie d'en-tête arrête
                // simplement le décodage : l'image partielle est rendue.
                if self.scans > 0 {
                    break;
                }
                return Err(e);
            }
        }
        Ok(())
    }

    /// Interprète un segment de tableau ou de paramètres.
    fn parse_segment(&mut self, m: u8, segment: &[u8], body_pos: usize) -> Result<()> {
        match m {
            marker::SOF0 | marker::SOF1 | marker::SOF2 => self.parse_frame(m, segment, body_pos),
            marker::DHT => self.parse_dht(segment),
            marker::DQT => self.parse_dqt(segment),
            marker::DRI => {
                if segment.len() < 2 {
                    return Err(corrupt("segment DRI tronqué"));
                }
                self.restart_interval = usize::from(u16::from_be_bytes([segment[0], segment[1]]));
                Ok(())
            }
            marker::APP14 => {
                // Segment « Adobe » : "Adobe" (5), version (2), flags0 (2),
                // flags1 (2), transform (1).
                if segment.len() >= 12 && &segment[..5] == b"Adobe" {
                    self.adobe_transform = Some(segment[11]);
                }
                Ok(())
            }
            // APP0 (JFIF), autres APPn, COM, DNL (déjà exploité), DAC (sans
            // objet hors codage arithmétique) : ignorés.
            _ => Ok(()),
        }
    }

    /// En-tête de trame (B.2.2) et allocation des tampons de coefficients.
    fn parse_frame(&mut self, sof: u8, segment: &[u8], body_pos: usize) -> Result<()> {
        if self.frame.is_some() {
            return Err(corrupt("plusieurs en-têtes SOF"));
        }
        let (precision, height, width, count) = parse_frame_dimensions(segment)?;
        let height = if height == 0 {
            find_dnl(self.data, body_pos)?
        } else {
            height
        };
        check_precision(sof, precision)?;
        check_dimensions(width, height, count)?;
        let count = usize::from(count);
        if segment.len() < 6 + 3 * count {
            return Err(corrupt("en-tête SOF tronqué"));
        }
        let mut components = Vec::with_capacity(count);
        for i in 0..count {
            let spec = &segment[6 + 3 * i..9 + 3 * i];
            let h = usize::from(spec[1] >> 4);
            let v = usize::from(spec[1] & 15);
            let tq = usize::from(spec[2]);
            if !(1..=4).contains(&h) || !(1..=4).contains(&v) || tq > 3 {
                return Err(corrupt("facteurs d'échantillonnage ou table invalides"));
            }
            if components.iter().any(|c: &Component| c.id == spec[0]) {
                return Err(corrupt("identifiant de composante dupliqué"));
            }
            components.push(Component {
                id: spec[0],
                h,
                v,
                tq,
                width: 0,
                height: 0,
                blocks_w: 0,
                blocks_h: 0,
                coeffs: Vec::new(),
                dc_pred: 0,
            });
        }
        let hmax = components.iter().map(|c| c.h).max().unwrap_or(1);
        let vmax = components.iter().map(|c| c.v).max().unwrap_or(1);
        let too_big = || corrupt("image trop grande");
        let width = usize::try_from(width).map_err(|_| too_big())?;
        let height = usize::try_from(height).map_err(|_| too_big())?;
        let mcus_x = width.div_ceil(8 * hmax);
        let mcus_y = height.div_ceil(8 * vmax);
        let mut coeff_bytes: u64 = 0;
        for c in &mut components {
            c.width = (width * c.h).div_ceil(hmax);
            c.height = (height * c.v).div_ceil(vmax);
            c.blocks_w = mcus_x * c.h;
            c.blocks_h = mcus_y * c.v;
            coeff_bytes += (c.blocks_w as u64) * (c.blocks_h as u64) * (BLOCK as u64) * 2;
        }
        if coeff_bytes > MAX_BUFFER_BYTES {
            return Err(too_big());
        }
        for c in &mut components {
            c.coeffs = vec![0; c.blocks_w * c.blocks_h * BLOCK];
        }
        self.frame = Some(Frame {
            width,
            height,
            precision,
            progressive: sof == marker::SOF2,
            hmax,
            vmax,
            mcus_x,
            mcus_y,
            components,
        });
        Ok(())
    }

    /// Tables de Huffman (B.2.4.2) : plusieurs tables par segment.
    fn parse_dht(&mut self, mut segment: &[u8]) -> Result<()> {
        while !segment.is_empty() {
            if segment.len() < 17 {
                return Err(corrupt("segment DHT tronqué"));
            }
            let class = segment[0] >> 4;
            let id = usize::from(segment[0] & 15);
            if class > 1 || id > 3 {
                return Err(corrupt("classe ou identifiant de table DHT invalide"));
            }
            let mut bits = [0u8; 16];
            bits.copy_from_slice(&segment[1..17]);
            let total: usize = bits.iter().map(|&b| usize::from(b)).sum();
            let values = segment
                .get(17..17 + total)
                .ok_or_else(|| corrupt("segment DHT tronqué"))?;
            let table = HuffTable::build(&bits, values)?;
            if class == 0 {
                self.dc_tables[id] = Some(table);
            } else {
                self.ac_tables[id] = Some(table);
            }
            segment = &segment[17 + total..];
        }
        Ok(())
    }

    /// Tables de quantification (B.2.4.1), précision 8 ou 16 bits, données
    /// en ordre zigzag converties en ordre naturel.
    fn parse_dqt(&mut self, mut segment: &[u8]) -> Result<()> {
        while !segment.is_empty() {
            let pq = segment[0] >> 4;
            let tq = usize::from(segment[0] & 15);
            if pq > 1 || tq > 3 {
                return Err(corrupt("précision ou identifiant de table DQT invalide"));
            }
            let width = if pq == 0 { 1 } else { 2 };
            let body = segment
                .get(1..1 + BLOCK * width)
                .ok_or_else(|| corrupt("segment DQT tronqué"))?;
            let mut table = [0u16; BLOCK];
            for (k, &natural) in ZIGZAG.iter().enumerate() {
                let q = if pq == 0 {
                    u16::from(body[k])
                } else {
                    u16::from_be_bytes([body[2 * k], body[2 * k + 1]])
                };
                // Un pas nul rendrait le bloc muet ; 1 est la valeur la plus
                // proche qui garde un sens.
                table[usize::from(natural)] = q.max(1);
            }
            self.qt[tq] = Some(table);
            segment = &segment[1 + BLOCK * width..];
        }
        Ok(())
    }

    /// En-tête de scan (B.2.3). Rend l'en-tête et la position du début des
    /// données entropiques.
    fn parse_scan_header(&self, pos: usize) -> Result<(ScanHeader, usize)> {
        let frame = self
            .frame
            .as_ref()
            .ok_or_else(|| corrupt("SOS avant SOF"))?;
        let (segment, end) = read_segment(self.data, pos)?;
        let count = usize::from(*segment.first().ok_or_else(|| corrupt("SOS tronqué"))?);
        if !(1..=4).contains(&count) || segment.len() < 4 + 2 * count {
            return Err(corrupt("en-tête SOS invalide"));
        }
        let mut components = Vec::with_capacity(count);
        for i in 0..count {
            let id = segment[1 + 2 * i];
            let tables = segment[2 + 2 * i];
            let index = frame
                .components
                .iter()
                .position(|c| c.id == id)
                .ok_or_else(|| corrupt("composante de scan inconnue"))?;
            if components.iter().any(|c: &ScanComponent| c.index == index) {
                return Err(corrupt("composante de scan dupliquée"));
            }
            components.push(ScanComponent {
                index,
                dc_table: usize::from(tables >> 4).min(3),
                ac_table: usize::from(tables & 15).min(3),
            });
        }
        let base = 1 + 2 * count;
        let mut header = ScanHeader {
            components,
            ss: usize::from(segment[base]),
            se: usize::from(segment[base + 1]),
            ah: u32::from(segment[base + 2] >> 4),
            al: u32::from(segment[base + 2] & 15),
        };
        if frame.progressive {
            // Contraintes de G.1.1.1.1 et tableau B.4.
            let dc_scan = header.ss == 0;
            if (dc_scan && header.se != 0)
                || (!dc_scan && (header.se > 63 || header.se < header.ss || count != 1))
                || header.al > 13
                || header.ah > 13
            {
                return Err(corrupt("paramètres de scan progressif invalides"));
            }
        } else {
            header.ss = 0;
            header.se = 63;
            header.ah = 0;
            header.al = 0;
        }
        Ok((header, end))
    }

    /// Décode les données entropiques d'un scan (A.2, E.2.3, G.1.2) et rend
    /// la position où reprendre la lecture des marqueurs. Une anomalie
    /// interrompt le scan sans erreur : l'image reste partielle.
    fn decode_scan(&mut self, scan: &ScanHeader, pos: usize) -> usize {
        self.scans += 1;
        self.eob_run = 0;
        let Some(frame) = self.frame.as_mut() else {
            return pos;
        };
        for c in &mut frame.components {
            c.dc_pred = 0;
        }
        let progressive = frame.progressive;
        let single = scan.components.len() == 1;
        let (mcus_x, mcus_y) = if single {
            let c = &frame.components[scan.components[0].index];
            // Scan non entrelacé : les blocs de la composante seule, sans
            // le remplissage à l'unité MCU (A.2.2).
            (c.width.div_ceil(8), c.height.div_ceil(8))
        } else {
            (frame.mcus_x, frame.mcus_y)
        };
        let mut reader = BitReader::new(self.data, pos);
        let total = mcus_x * mcus_y;
        let mut mcu = 0;
        'mcus: while mcu < total {
            let ri = self.restart_interval;
            if ri > 0 && mcu > 0 && mcu % ri == 0 {
                let Some(found) = reader.restart() else {
                    break;
                };
                // Le numéro du marqueur (modulo 8) indique combien
                // d'intervalles ont été perdus : ils restent gris et le
                // décodage reprend au bon endroit (E.1.4).
                let expected = u8::try_from((mcu / ri - 1) % 8).unwrap_or(0);
                let skipped = usize::from((found + 8 - expected) % 8);
                mcu += skipped * ri;
                if mcu >= total {
                    break;
                }
                for c in &mut frame.components {
                    c.dc_pred = 0;
                }
                self.eob_run = 0;
            }
            let my = mcu / mcus_x;
            let mx = mcu % mcus_x;
            for sc in &scan.components {
                let component = &mut frame.components[sc.index];
                let (bh, bv) = if single {
                    (1, 1)
                } else {
                    (component.h, component.v)
                };
                for v in 0..bv {
                    for h in 0..bh {
                        let by = my * bv + v;
                        let bx = mx * bh + h;
                        let start = (by * component.blocks_w + bx) * BLOCK;
                        let Some(block) = component.coeffs.get_mut(start..start + BLOCK) else {
                            break 'mcus;
                        };
                        let ok = decode_block(
                            &mut reader,
                            block,
                            &mut component.dc_pred,
                            &mut self.eob_run,
                            &BlockCoding {
                                progressive,
                                dc: self.dc_tables[sc.dc_table].as_ref(),
                                ac: self.ac_tables[sc.ac_table].as_ref(),
                                ss: scan.ss,
                                se: scan.se,
                                ah: scan.ah,
                                al: scan.al,
                            },
                        );
                        if !ok {
                            break 'mcus;
                        }
                    }
                }
            }
            if reader.overrun() {
                break;
            }
            mcu += 1;
        }
        reader.pos
    }

    /// IDCT, sur-échantillonnage et transformation de couleur (A.3.3, JFIF,
    /// note technique Adobe n° 5116).
    fn finish(self) -> Result<JpegImage> {
        let frame = self
            .frame
            .ok_or_else(|| corrupt("en-tête de trame SOF absent"))?;
        if self.scans == 0 {
            return Err(corrupt("aucun scan (SOS absent)"));
        }
        let scale = if frame.precision == 12 {
            1.0 / 16.0
        } else {
            1.0
        };
        let idct = Idct::new(scale);
        let mut planes = Vec::with_capacity(frame.components.len());
        for c in &frame.components {
            let qt = self.qt[c.tq].unwrap_or([1; BLOCK]);
            planes.push(render_plane(c, &qt, &idct));
        }
        let count = frame.components.len();
        let transform = match (count, self.adobe_transform) {
            (3, Some(0)) => ColorTransform::None,
            (3, Some(_)) => ColorTransform::YCbCr,
            (3, None) => {
                let ids: Vec<u8> = frame.components.iter().map(|c| c.id).collect();
                if ids == b"RGB" {
                    ColorTransform::None
                } else {
                    ColorTransform::YCbCr
                }
            }
            (4, Some(2)) => ColorTransform::Ycck,
            _ => ColorTransform::None,
        };
        let mut data = vec![0u8; frame.width * frame.height * count];
        let mut rows: Vec<Vec<u8>> = vec![vec![0u8; frame.width]; count];
        let ycc = YccTables::new();
        for y in 0..frame.height {
            for ((c, plane), row) in frame.components.iter().zip(&planes).zip(&mut rows) {
                upsample_row(plane, c, &frame, y, row);
            }
            let out = &mut data[y * frame.width * count..(y + 1) * frame.width * count];
            interleave_row(&rows, out, transform, &ycc);
        }
        let too_big = || corrupt("image trop grande");
        Ok(JpegImage {
            width: u32::try_from(frame.width).map_err(|_| too_big())?,
            height: u32::try_from(frame.height).map_err(|_| too_big())?,
            components: u8::try_from(count).map_err(|_| too_big())?,
            data,
            adobe_inverted: self.adobe_transform.is_some() && count == 4,
            progressive: frame.progressive,
        })
    }
}

// ---------------------------------------------------------------------------
// Décodage d'un bloc (F.2.2, G.1.2)
// ---------------------------------------------------------------------------

/// Paramètres de codage d'un bloc dans le scan courant.
struct BlockCoding<'t> {
    progressive: bool,
    dc: Option<&'t HuffTable>,
    ac: Option<&'t HuffTable>,
    ss: usize,
    se: usize,
    ah: u32,
    al: u32,
}

/// Procédure EXTEND (F.2.2.1, figure F.12) appliquée à RECEIVE(s).
fn receive_extend(reader: &mut BitReader<'_>, s: u32) -> i32 {
    let v = reader.receive(s);
    #[allow(clippy::cast_possible_wrap)]
    let v = v as i32;
    if s > 0 && v < (1 << (s - 1)) {
        v - (1 << s) + 1
    } else {
        v
    }
}

/// Convertit en `i16` en saturant (un flux hostile peut faire déborder
/// le prédicteur DC ou une valeur décalée de Al bits).
fn saturate(v: i32) -> i16 {
    i16::try_from(v.clamp(i32::from(i16::MIN), i32::from(i16::MAX))).unwrap_or(0)
}

/// Décode un bloc selon le type de scan. Rend `false` si le flux est
/// inexploitable (code de Huffman inconnu, table manquante, indice hors
/// bloc) : le scan s'arrête alors.
fn decode_block(
    reader: &mut BitReader<'_>,
    block: &mut [i16],
    dc_pred: &mut i32,
    eob_run: &mut u32,
    coding: &BlockCoding<'_>,
) -> bool {
    if !coding.progressive {
        let (Some(dc), Some(ac)) = (coding.dc, coding.ac) else {
            return false;
        };
        return decode_block_sequential(reader, block, dc_pred, dc, ac);
    }
    if coding.ss == 0 {
        if coding.ah == 0 {
            let Some(dc) = coding.dc else {
                return false;
            };
            decode_dc_first(reader, block, dc_pred, dc, coding.al)
        } else {
            decode_dc_refine(reader, block, coding.al);
            true
        }
    } else {
        let Some(ac) = coding.ac else {
            return false;
        };
        if coding.ah == 0 {
            decode_ac_first(reader, block, ac, eob_run, coding)
        } else {
            decode_ac_refine(reader, block, ac, eob_run, coding)
        }
    }
}

/// Bloc séquentiel : DC différentiel (F.2.2.1) puis AC en (RUN, SIZE)
/// (F.2.2.2, figure F.13).
fn decode_block_sequential(
    reader: &mut BitReader<'_>,
    block: &mut [i16],
    dc_pred: &mut i32,
    dc: &HuffTable,
    ac: &HuffTable,
) -> bool {
    let Some(t) = dc.decode(reader) else {
        return false;
    };
    if t > 16 {
        return false;
    }
    *dc_pred = dc_pred.saturating_add(receive_extend(reader, u32::from(t)));
    block[0] = saturate(*dc_pred);
    let mut k = 1usize;
    while k < BLOCK {
        let Some(rs) = ac.decode(reader) else {
            return false;
        };
        let run = usize::from(rs >> 4);
        let size = u32::from(rs & 15);
        if size == 0 {
            if run == 15 {
                k += 16;
                continue;
            }
            break; // EOB
        }
        k += run;
        if k >= BLOCK {
            return false;
        }
        block[usize::from(ZIGZAG[k])] = saturate(receive_extend(reader, size));
        k += 1;
    }
    true
}

/// Premier scan DC progressif (G.1.2.1) : DC différentiel, décalé de Al bits.
fn decode_dc_first(
    reader: &mut BitReader<'_>,
    block: &mut [i16],
    dc_pred: &mut i32,
    dc: &HuffTable,
    al: u32,
) -> bool {
    let Some(t) = dc.decode(reader) else {
        return false;
    };
    if t > 16 {
        return false;
    }
    *dc_pred = dc_pred.saturating_add(receive_extend(reader, u32::from(t)));
    block[0] = saturate(dc_pred.saturating_mul(1 << al));
    true
}

/// Scan d'affinement DC (G.1.2.1) : un bit de précision supplémentaire.
fn decode_dc_refine(reader: &mut BitReader<'_>, block: &mut [i16], al: u32) {
    if reader.bit() {
        block[0] |= saturate(1 << al);
    }
}

/// Premier scan AC d'une bande spectrale (G.1.2.2, figure G.3) avec
/// gestion des séries de blocs EOB (EOBRUN).
fn decode_ac_first(
    reader: &mut BitReader<'_>,
    block: &mut [i16],
    ac: &HuffTable,
    eob_run: &mut u32,
    coding: &BlockCoding<'_>,
) -> bool {
    if *eob_run > 0 {
        *eob_run -= 1;
        return true;
    }
    let mut k = coding.ss;
    while k <= coding.se {
        let Some(rs) = ac.decode(reader) else {
            return false;
        };
        let run = usize::from(rs >> 4);
        let size = u32::from(rs & 15);
        if size == 0 {
            if run < 15 {
                // EOBn : 2^run blocs (celui-ci compris) plus `run` bits.
                let extra = reader.receive(u32::try_from(run).unwrap_or(0));
                *eob_run = (1u32 << run) - 1 + extra;
                break;
            }
            k += 16;
            continue;
        }
        k += run;
        if k > 63 {
            return false;
        }
        let value = receive_extend(reader, size).saturating_mul(1 << coding.al);
        block[usize::from(ZIGZAG[k])] = saturate(value);
        k += 1;
    }
    true
}

/// Ajoute un bit de précision à un coefficient déjà non nul (G.1.2.3) : la
/// magnitude croît vers le signe du coefficient.
fn refine_nonzero(reader: &mut BitReader<'_>, coef: &mut i16, p1: i16) {
    if reader.bit() && *coef & p1 == 0 {
        *coef = if *coef >= 0 {
            coef.saturating_add(p1)
        } else {
            coef.saturating_sub(p1)
        };
    }
}

/// Scan d'affinement AC (G.1.2.3, figures G.7 et G.8).
fn decode_ac_refine(
    reader: &mut BitReader<'_>,
    block: &mut [i16],
    ac: &HuffTable,
    eob_run: &mut u32,
    coding: &BlockCoding<'_>,
) -> bool {
    let p1 = saturate(1 << coding.al);
    let mut k = coding.ss;
    if *eob_run == 0 {
        while k <= coding.se {
            let Some(rs) = ac.decode(reader) else {
                return false;
            };
            let mut run = usize::from(rs >> 4);
            let size = rs & 15;
            let mut value: i16 = 0;
            if size == 0 {
                if run < 15 {
                    let extra = reader.receive(u32::try_from(run).unwrap_or(0));
                    *eob_run = (1u32 << run) + extra;
                    break;
                }
                // run == 15 : ZRL, seize coefficients nuls à sauter.
            } else {
                // Un coefficient nouvellement non nul : son signe.
                value = if reader.bit() { p1 } else { -p1 };
            }
            // Avance sur la bande : les coefficients déjà non nuls reçoivent
            // un bit de correction, les nuls consomment la série `run`.
            while k <= coding.se {
                let z = usize::from(ZIGZAG[k]);
                if block[z] != 0 {
                    refine_nonzero(reader, &mut block[z], p1);
                } else {
                    if run == 0 {
                        if value != 0 {
                            block[z] = value;
                        }
                        k += 1;
                        break;
                    }
                    run -= 1;
                }
                k += 1;
            }
        }
    }
    if *eob_run > 0 {
        // Fin de bloc dans une série EOB : seuls les coefficients déjà non
        // nuls du reste de la bande reçoivent un bit de correction.
        while k <= coding.se {
            let z = usize::from(ZIGZAG[k]);
            if block[z] != 0 {
                refine_nonzero(reader, &mut block[z], p1);
            }
            k += 1;
        }
        *eob_run -= 1;
    }
    true
}

// ---------------------------------------------------------------------------
// IDCT (A.3.3) et reconstruction des plans
// ---------------------------------------------------------------------------

/// Transformée en cosinus discrète inverse 8 × 8 séparable, en virgule
/// flottante, avec tables de cosinus précalculées :
/// `cos[x][u] = C(u) / 2 · cos((2x + 1) u π / 16)`.
struct Idct {
    cos: [[f32; 8]; 8],
    /// Facteur de sortie (1 pour 8 bits, 1/16 pour ramener 12 bits à 8).
    scale: f32,
}

impl Idct {
    fn new(scale: f32) -> Self {
        let mut cos = [[0f32; 8]; 8];
        for (x, row) in cos.iter_mut().enumerate() {
            for (u, cell) in row.iter_mut().enumerate() {
                let cu = if u == 0 {
                    std::f64::consts::FRAC_1_SQRT_2
                } else {
                    1.0
                };
                #[allow(clippy::cast_precision_loss)]
                let angle = (2.0 * x as f64 + 1.0) * u as f64 * std::f64::consts::PI / 16.0;
                #[allow(clippy::cast_possible_truncation)]
                let value = (cu / 2.0 * angle.cos()) as f32;
                *cell = value;
            }
        }
        Idct { cos, scale }
    }

    /// Déquantifie (A.3.4) et transforme un bloc, puis écrit les échantillons
    /// (décalés de +128, saturés à 0..255) dans `out` avec le pas `stride`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn block(&self, coeffs: &[i16], qt: &[u16; BLOCK], out: &mut [u8], stride: usize) {
        let mut deq = [0f32; BLOCK];
        let mut any_ac = false;
        for (i, (&c, &q)) in coeffs.iter().zip(qt).enumerate() {
            if c != 0 {
                deq[i] = f32::from(c) * f32::from(q);
                any_ac |= i != 0;
            }
        }
        if !any_ac {
            // Bloc uniforme (fréquent) : f(x, y) = F(0, 0) / 8.
            let value = (deq[0] / 8.0 * self.scale + 128.5)
                .floor()
                .clamp(0.0, 255.0) as u8;
            for y in 0..8 {
                out[y * stride..y * stride + 8].fill(value);
            }
            return;
        }
        // Passe 1 : chaque ligne v du bloc de coefficients devient une ligne
        // de 8 valeurs en x.
        let mut ws = [0f32; BLOCK];
        for v in 0..8 {
            let row = &deq[v * 8..v * 8 + 8];
            let target = &mut ws[v * 8..v * 8 + 8];
            if row[1..].iter().all(|&c| c == 0.0) {
                target.fill(row[0] * self.cos[0][0]);
                continue;
            }
            for (x, cell) in target.iter_mut().enumerate() {
                let cx = &self.cos[x];
                *cell = (0..8).map(|u| cx[u] * row[u]).sum();
            }
        }
        // Passe 2 : colonnes.
        for x in 0..8 {
            for y in 0..8 {
                let cy = &self.cos[y];
                let s: f32 = (0..8).map(|v| cy[v] * ws[v * 8 + x]).sum();
                out[y * stride + x] = (s * self.scale + 128.5).floor().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// Plan d'échantillons d'une composante (dimensions arrondies au bloc).
struct Plane {
    stride: usize,
    samples: Vec<u8>,
}

/// Applique l'IDCT à tous les blocs d'une composante.
fn render_plane(c: &Component, qt: &[u16; BLOCK], idct: &Idct) -> Plane {
    let stride = c.blocks_w * 8;
    let mut samples = vec![128u8; stride * c.blocks_h * 8];
    for by in 0..c.blocks_h {
        for bx in 0..c.blocks_w {
            let start = (by * c.blocks_w + bx) * BLOCK;
            let coeffs = &c.coeffs[start..start + BLOCK];
            let origin = by * 8 * stride + bx * 8;
            idct.block(coeffs, qt, &mut samples[origin..], stride);
        }
    }
    Plane { stride, samples }
}

/// Ligne d'échantillons d'une composante ramenée à la résolution de l'image.
/// Interpolation triangulaire pour les rapports 2:1, réplication sinon.
fn upsample_row(plane: &Plane, c: &Component, frame: &Frame, y: usize, out: &mut [u8]) {
    let last_row = c.height.saturating_sub(1);
    let last_col = c.width.saturating_sub(1);
    let row_at = |r: usize| {
        let r = r.min(last_row);
        &plane.samples[r * plane.stride..r * plane.stride + c.width]
    };
    let fancy_y = c.v * 2 == frame.vmax;
    let fancy_x = c.h * 2 == frame.hmax;
    // Passe verticale : valeurs × 4 (3 × proche + 1 × voisin) si
    // interpolation, sinon la ligne source la plus proche telle quelle.
    let (vertical, vscale): (Vec<u32>, u32) = if fancy_y {
        let sy = y / 2;
        let ny = if y % 2 == 0 {
            sy.saturating_sub(1)
        } else {
            sy + 1
        };
        let mixed = row_at(sy)
            .iter()
            .zip(row_at(ny))
            .map(|(&a, &b)| 3 * u32::from(a) + u32::from(b))
            .collect();
        (mixed, 4)
    } else {
        let sy = y * c.v / frame.vmax;
        (row_at(sy).iter().map(|&a| u32::from(a)).collect(), 1)
    };
    let at = |x: usize| vertical[x.min(last_col)];
    for (x, o) in out.iter_mut().enumerate() {
        let v = if fancy_x {
            // Interpolation horizontale : poids 3/4 pour l'échantillon le
            // plus proche, 1/4 pour le voisin. Le biais d'arrondi alterne
            // (½ à gauche, ½ − ε à droite) pour que la moyenne d'une paire
            // ne dérive pas vers le haut.
            let sx = x / 2;
            let (nx, bias) = if x % 2 == 0 {
                (sx.saturating_sub(1), 2 * vscale)
            } else {
                (sx + 1, 2 * vscale - 1)
            };
            (3 * at(sx) + at(nx) + bias) / (4 * vscale)
        } else {
            (at(x * c.h / frame.hmax) + vscale / 2) / vscale
        };
        *o = u8::try_from(v.min(255)).unwrap_or(255);
    }
}

/// Transformation de couleur appliquée à la sortie.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorTransform {
    None,
    YCbCr,
    /// YCC sur les trois premières composantes, K conservé (Adobe).
    Ycck,
}

/// Tables de conversion YCbCr→RGB (JFIF 1.02 §7, coefficients CCIR 601) en
/// virgule fixe 16 bits.
struct YccTables {
    cr_r: [i32; 256],
    cb_b: [i32; 256],
    cr_g: [i32; 256],
    cb_g: [i32; 256],
}

impl YccTables {
    #[allow(clippy::cast_possible_truncation)]
    fn new() -> Self {
        let mut t = YccTables {
            cr_r: [0; 256],
            cb_b: [0; 256],
            cr_g: [0; 256],
            cb_g: [0; 256],
        };
        for i in 0..256usize {
            let d = i32::try_from(i).unwrap_or(0) - 128;
            let fixed = |k: f64| (k * 65536.0).round() as i32;
            t.cr_r[i] = (fixed(1.402) * d + 32768) >> 16;
            t.cb_b[i] = (fixed(1.772) * d + 32768) >> 16;
            t.cr_g[i] = fixed(-0.714_136) * d;
            t.cb_g[i] = fixed(-0.344_136) * d;
        }
        t
    }

    fn to_rgb(&self, y: u8, cb: u8, cr: u8) -> (u8, u8, u8) {
        let y = i32::from(y);
        let (cb, cr) = (usize::from(cb), usize::from(cr));
        let r = y + self.cr_r[cr];
        let g = y + ((self.cb_g[cb] + self.cr_g[cr] + 32768) >> 16);
        let b = y + self.cb_b[cb];
        (clamp_u8(r), clamp_u8(g), clamp_u8(b))
    }
}

fn clamp_u8(v: i32) -> u8 {
    u8::try_from(v.clamp(0, 255)).unwrap_or(0)
}

/// Entrelace les lignes des composantes dans la ligne de sortie en
/// appliquant la transformation de couleur.
fn interleave_row(rows: &[Vec<u8>], out: &mut [u8], transform: ColorTransform, ycc: &YccTables) {
    let count = rows.len();
    for (x, pixel) in out.chunks_exact_mut(count).enumerate() {
        match transform {
            ColorTransform::YCbCr | ColorTransform::Ycck => {
                let (r, g, b) = ycc.to_rgb(rows[0][x], rows[1][x], rows[2][x]);
                pixel[0] = r;
                pixel[1] = g;
                pixel[2] = b;
                if let Some(k) = pixel.get_mut(3) {
                    *k = rows[3][x];
                }
            }
            ColorTransform::None => {
                for (p, row) in pixel.iter_mut().zip(rows) {
                    *p = row[x];
                }
            }
        }
    }
}
