//! Filtre JBIG2Decode (ISO 32000-2 §7.4.7) : décodeur ITU-T T.88 (JBIG2)
//! pour le profil PDF — flux embarqué (annexe D.3, organisation
//! séquentielle, sans en-tête de fichier) plus flux `/JBIG2Globals` —,
//! écrit d'après la recommandation, sans aucune dépendance ni code tiers.
//!
//! Couvert :
//! - en-têtes de segment (§7.2), y compris la longueur inconnue
//!   0xFFFFFFFF des régions génériques immédiates (§7.2.7) ;
//! - informations de page (48), fin de bande (50), fin de page (49), fin de
//!   fichier (51), profils (52), extensions (62) ignorés ;
//! - région générique immédiate (36, 38, 39) : codage arithmétique MQ
//!   (annexe E) avec gabarits 0 à 3, pixels adaptatifs, TPGDON ; codage MMR
//!   (T.6, via [`crate::ccitt`]) ;
//! - dictionnaire de symboles (0) et région de texte (4, 6, 7) en codage
//!   arithmétique (procédures entières de l'annexe A), avec raffinement et
//!   agrégation ; en codage de Huffman avec les tables standard B.1 à B.15
//!   et les tables personnalisées (53), sans raffinement ;
//! - région de raffinement (40, 42, 43), gabarits 0 et 1, TPGRON ;
//! - dictionnaire de motifs (16) et région de demi-teintes (20, 22, 23),
//!   arithmétique et MMR ;
//! - régions intermédiaires (36, 40, 20, 4) conservées pour référence.
//!
//! Non couvert (`Err(Unsupported)` ou région ignorée) : dictionnaire de
//! symboles Huffman avec raffinement (SDHUFF = 1 et SDREFAGG = 1),
//! raffinement Huffman de taille inconnue (BMSIZE = 0), gabarit étendu à
//! douze pixels adaptatifs (EXTTEMPLATE, amendement 2), extension couleur.
//!
//! Robustesse : toutes les tailles sont bornées ([`MAX_BITMAP_PIXELS`],
//! [`MAX_SYMBOLS`], [`MAX_SYMBOL_PIXELS`], [`MAX_PATTERNS`],
//! [`MAX_HALFTONE_CELLS`], [`MAX_REGION_OVERHANG`]), toutes les
//! boucles sont bornées par les compteurs des en-têtes, un segment tronqué
//! ou corrompu est ignoré et le décodage continue avec la page telle
//! qu'elle est (Acrobat affiche ce qui a pu être décodé), un segment
//! référencé absent rend la région ignorée. Aucune entrée ne provoque de
//! panique.
//!
//! Polarité : dans JBIG2 (T.88 §2.4) un pixel à 1 est noir. ISO 32000-2
//! §7.4.7 précise que le filtre JBIG2Decode produit une image monochrome à
//! 1 bit par pixel dans laquelle, comme JBIG2 code 1 = noir, **les bits
//! doivent être inversés** pour que l'image se lise avec l'espace DeviceGray
//! à 1 bit (0 = noir) et le tableau /Decode par défaut [0 1] : « The filter
//! shall produce … 1 bits as black pixels … the filter inverts the
//! bits … so that 0 pixels = black » (paraphrase, §7.4.7). [`decode`]
//! livre donc 0 pour un pixel noir, comme CCITTFaxDecode avec BlackIs1 faux.

use acrux_core::{Error, Result};
use std::collections::HashMap;

mod bitmap;
mod generic;
mod halftone;
mod huffman;
mod mq;
mod symbol;

#[cfg(test)]
mod tests;

use bitmap::{Bitmap, ComposeOp};
use generic::{GenericParams, RefinementParams, GB_CONTEXTS, GR_CONTEXTS};
use symbol::RetainedContexts;

/// Nombre maximal de pixels d'une image (page, région, bitmap collectif).
pub const MAX_BITMAP_PIXELS: u64 = 1 << 30;

/// Nombre maximal de symboles d'un dictionnaire (entrants ou nouveaux).
pub const MAX_SYMBOLS: usize = 1 << 20;

/// Nombre total maximal de pixels des nouveaux symboles d'un dictionnaire.
pub const MAX_SYMBOL_PIXELS: u64 = 1 << 26;

/// Nombre maximal de motifs d'un dictionnaire de motifs.
pub const MAX_PATTERNS: usize = 1 << 16;

/// Nombre maximal de cellules de la grille d'une région de demi-teintes.
pub const MAX_HALFTONE_CELLS: u64 = 1 << 24;

/// Marge de largeur tolérée pour une région débordant de la page : au-delà
/// de `4 × largeur de page + MAX_REGION_OVERHANG`, la région est tenue pour
/// corrompue (le décodeur arithmétique devrait sinon consommer chaque pixel
/// invisible).
pub const MAX_REGION_OVERHANG: u64 = 4096;

/// Nombre maximal de segments référencés par un segment.
const MAX_REFERRED: usize = 1 << 16;

fn corrupt(message: &str) -> Error {
    Error::Corrupt(format!("JBIG2 : {message}"))
}

fn read_u16(data: &[u8], pos: usize) -> Result<u16> {
    data.get(pos..pos + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or_else(|| corrupt("segment tronqué"))
}

fn read_u32(data: &[u8], pos: usize) -> Result<u32> {
    data.get(pos..pos + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| corrupt("segment tronqué"))
}

// ---------------------------------------------------------------------------
// En-têtes de segment (§7.2)
// ---------------------------------------------------------------------------

/// Types de segment (§7.3).
mod segment_type {
    pub const SYMBOL_DICT: u8 = 0;
    pub const TEXT_INTERMEDIATE: u8 = 4;
    pub const TEXT_IMMEDIATE: u8 = 6;
    pub const TEXT_IMMEDIATE_LOSSLESS: u8 = 7;
    pub const PATTERN_DICT: u8 = 16;
    pub const HALFTONE_INTERMEDIATE: u8 = 20;
    pub const HALFTONE_IMMEDIATE: u8 = 22;
    pub const HALFTONE_IMMEDIATE_LOSSLESS: u8 = 23;
    pub const GENERIC_INTERMEDIATE: u8 = 36;
    pub const GENERIC_IMMEDIATE: u8 = 38;
    pub const GENERIC_IMMEDIATE_LOSSLESS: u8 = 39;
    pub const REFINEMENT_INTERMEDIATE: u8 = 40;
    pub const REFINEMENT_IMMEDIATE: u8 = 42;
    pub const REFINEMENT_IMMEDIATE_LOSSLESS: u8 = 43;
    pub const PAGE_INFO: u8 = 48;
    pub const END_OF_FILE: u8 = 51;
    pub const TABLES: u8 = 53;
    // Fin de page (49), fin de bande (50), profils (52) et extensions (62)
    // n'ont rien à décoder : ils tombent dans le cas par défaut.
}

/// En-tête de segment décodé (§7.2.2 à §7.2.7).
struct SegmentHeader {
    number: u32,
    kind: u8,
    referred: Vec<u32>,
    /// Longueur des données, `None` si inconnue (0xFFFFFFFF).
    length: Option<usize>,
}

/// Lit un en-tête de segment à `pos` ; rend l'en-tête et la position des
/// données.
fn parse_segment_header(data: &[u8], pos: usize) -> Result<(SegmentHeader, usize)> {
    let number = read_u32(data, pos)?;
    let flags = *data
        .get(pos + 4)
        .ok_or_else(|| corrupt("en-tête tronqué"))?;
    let kind = flags & 0x3F;
    let page_assoc_4 = flags & 0x40 != 0;
    let mut p = pos + 5;
    let rts = *data.get(p).ok_or_else(|| corrupt("en-tête tronqué"))?;
    let count = if rts >> 5 == 7 {
        let c = (read_u32(data, p)? & 0x1FFF_FFFF) as usize;
        // Bits de rétention : ⌈(count + 1) / 8⌉ octets (§7.2.4).
        p += 4 + (c + 8) / 8;
        c
    } else {
        p += 1;
        usize::from(rts >> 5)
    };
    if count > MAX_REFERRED {
        return Err(corrupt("trop de segments référencés"));
    }
    let ref_size = if number <= 256 {
        1
    } else if number <= 65536 {
        2
    } else {
        4
    };
    let mut referred = Vec::with_capacity(count);
    for _ in 0..count {
        let v = match ref_size {
            1 => u32::from(*data.get(p).ok_or_else(|| corrupt("en-tête tronqué"))?),
            2 => u32::from(read_u16(data, p)?),
            _ => read_u32(data, p)?,
        };
        referred.push(v);
        p += ref_size;
    }
    // Association de page (§7.2.6) : ignorée, toutes les pages sont composées.
    p += if page_assoc_4 { 4 } else { 1 };
    let length = read_u32(data, p)?;
    p += 4;
    if p > data.len() {
        return Err(corrupt("en-tête tronqué"));
    }
    let length = (length != 0xFFFF_FFFF).then_some(length as usize);
    Ok((
        SegmentHeader {
            number,
            kind,
            referred,
            length,
        },
        p,
    ))
}

/// Informations de région (§7.4.1) : 17 octets.
#[derive(Debug, Clone, Copy)]
struct RegionInfo {
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    op: ComposeOp,
}

fn parse_region_info(data: &[u8]) -> Result<RegionInfo> {
    let flags = *data.get(16).ok_or_else(|| corrupt("région tronquée"))?;
    Ok(RegionInfo {
        width: read_u32(data, 0)?,
        height: read_u32(data, 4)?,
        x: read_u32(data, 8)?,
        y: read_u32(data, 12)?,
        op: ComposeOp::from_code(flags & 7),
    })
}

// ---------------------------------------------------------------------------
// Décodeur
// ---------------------------------------------------------------------------

/// Résultat d'un segment de dictionnaire de symboles.
struct StoredDict {
    symbols: Vec<Bitmap>,
    retained: Option<RetainedContexts>,
}

struct Decoder {
    page: Bitmap,
    /// Valeur par défaut des pixels de la page (§7.4.8.5, bit 2).
    page_default: u8,
    symbol_dicts: HashMap<u32, StoredDict>,
    patterns: HashMap<u32, Vec<Bitmap>>,
    tables: HashMap<u32, huffman::Table>,
    /// Régions intermédiaires : bitmap et position.
    intermediate: HashMap<u32, (Bitmap, RegionInfo)>,
    /// Vrai dès qu'une région a été composée sur la page.
    any_region: bool,
}

impl Decoder {
    /// Parcourt une suite de segments (flux global ou flux de page).
    fn process_stream(&mut self, data: &[u8]) {
        let mut pos = 0usize;
        let mut guard = 0usize;
        while pos < data.len() && guard < 1 << 20 {
            guard += 1;
            let Ok((header, body_pos)) = parse_segment_header(data, pos) else {
                break;
            };
            let (body, next) = match header.length {
                Some(len) => {
                    let end = body_pos.saturating_add(len).min(data.len());
                    (&data[body_pos..end], end)
                }
                None => match unknown_length_end(&header, &data[body_pos..]) {
                    Some((body_len, total)) => (
                        &data[body_pos..body_pos + body_len],
                        body_pos.saturating_add(total),
                    ),
                    None => (&data[body_pos..], data.len()),
                },
            };
            // Une erreur dans un segment ne bloque pas les suivants.
            let _ = self.process_segment(&header, body);
            if header.kind == segment_type::END_OF_FILE {
                break;
            }
            if next <= pos {
                break;
            }
            pos = next;
        }
    }

    fn process_segment(&mut self, header: &SegmentHeader, body: &[u8]) -> Result<()> {
        use segment_type as t;
        match header.kind {
            t::SYMBOL_DICT => self.symbol_dict(header, body),
            t::TEXT_INTERMEDIATE | t::TEXT_IMMEDIATE | t::TEXT_IMMEDIATE_LOSSLESS => {
                self.text_region(header, body)
            }
            t::PATTERN_DICT => {
                let patterns = halftone::decode_pattern_dict_segment(body)?;
                self.patterns.insert(header.number, patterns);
                Ok(())
            }
            t::HALFTONE_INTERMEDIATE | t::HALFTONE_IMMEDIATE | t::HALFTONE_IMMEDIATE_LOSSLESS => {
                self.halftone_region(header, body)
            }
            t::GENERIC_INTERMEDIATE | t::GENERIC_IMMEDIATE | t::GENERIC_IMMEDIATE_LOSSLESS => {
                self.generic_region(header, body)
            }
            t::REFINEMENT_INTERMEDIATE
            | t::REFINEMENT_IMMEDIATE
            | t::REFINEMENT_IMMEDIATE_LOSSLESS => self.refinement_region(header, body),
            t::PAGE_INFO => self.page_info(body),
            t::TABLES => {
                let table = huffman::parse_custom(body)?;
                self.tables.insert(header.number, table);
                Ok(())
            }
            // Fin de page, de bande, de fichier, profils, extensions et
            // types inconnus : rien à décoder.
            _ => Ok(()),
        }
    }

    /// Informations de page (§7.4.8) : seule la valeur de pixel par défaut
    /// nous importe, les dimensions venant du dictionnaire d'image PDF.
    fn page_info(&mut self, body: &[u8]) -> Result<()> {
        let flags = *body.get(16).ok_or_else(|| corrupt("page tronquée"))?;
        let default = (flags >> 2) & 1;
        if default != self.page_default && !self.any_region {
            self.page_default = default;
            self.page.data.fill(default);
        }
        Ok(())
    }

    /// Dimensions effectives d'une région : hauteur coupée à la page (les
    /// lignes au-delà ne sont jamais visibles ; la largeur, elle, doit être
    /// décodée entièrement pour garder les contextes alignés) ; `None` si la
    /// région est hors page ou déraisonnablement plus large que la page.
    fn clip_region(&self, info: &RegionInfo) -> Option<(usize, usize)> {
        let x = info.x as usize;
        let y = info.y as usize;
        if x >= self.page.width || y >= self.page.height {
            return None;
        }
        let max_width = (self.page.width as u64) * 4 + MAX_REGION_OVERHANG;
        if u64::from(info.width) > max_width {
            return None;
        }
        let height = (info.height as usize).min(self.page.height - y);
        Some((info.width as usize, height))
    }

    fn compose(&mut self, header: &SegmentHeader, info: RegionInfo, bitmap: Bitmap) {
        let intermediate = matches!(
            header.kind,
            segment_type::GENERIC_INTERMEDIATE
                | segment_type::TEXT_INTERMEDIATE
                | segment_type::HALFTONE_INTERMEDIATE
                | segment_type::REFINEMENT_INTERMEDIATE
        );
        if intermediate {
            self.intermediate.insert(header.number, (bitmap, info));
        } else {
            self.page
                .compose(&bitmap, i64::from(info.x), i64::from(info.y), info.op);
            self.any_region = true;
        }
    }

    /// Région générique (§7.4.6).
    fn generic_region(&mut self, header: &SegmentHeader, body: &[u8]) -> Result<()> {
        let info = parse_region_info(body)?;
        let flags = *body.get(17).ok_or_else(|| corrupt("région tronquée"))?;
        let mmr = flags & 1 != 0;
        let template = (flags >> 1) & 3;
        let tpgdon = flags & 8 != 0;
        if flags & 0x10 != 0 {
            return Err(Error::Unsupported(
                "JBIG2 : gabarit étendu EXTTEMPLATE".into(),
            ));
        }
        let mut pos = 18;
        let mut at = [(0i8, 0i8); 4];
        if !mmr {
            let n = if template == 0 { 4 } else { 1 };
            for a in at.iter_mut().take(n) {
                *a = symbol::read_at(body, pos)?;
                pos += 2;
            }
        }
        let Some((width, height)) = self.clip_region(&info) else {
            return Ok(());
        };
        generic::check_size(width as u64, height as u64)?;
        let data = body.get(pos..).unwrap_or(&[]);
        let bitmap = if mmr {
            let mut decoder = crate::ccitt::MmrDecoder::new(data, info.width);
            generic::decode_generic_mmr(&mut decoder, width, height)?
        } else {
            let params = GenericParams {
                template,
                at,
                tpgdon,
            };
            let mut mq = mq::MqDecoder::new(data);
            let mut cx = vec![0u8; GB_CONTEXTS];
            generic::decode_generic(width, height, &params, &mut mq, &mut cx, None)?
        };
        self.compose(header, info, bitmap);
        Ok(())
    }

    /// Région de raffinement (§7.4.7) : raffine une région intermédiaire
    /// référencée ou, à défaut, la zone correspondante de la page.
    fn refinement_region(&mut self, header: &SegmentHeader, body: &[u8]) -> Result<()> {
        let info = parse_region_info(body)?;
        let flags = *body.get(17).ok_or_else(|| corrupt("région tronquée"))?;
        let template = flags & 1;
        let tpgron = flags & 2 != 0;
        let mut pos = 18;
        let mut at = [(0i8, 0i8); 2];
        if template == 0 {
            for a in &mut at {
                *a = symbol::read_at(body, pos)?;
                pos += 2;
            }
        }
        let Some((width, height)) = self.clip_region(&info) else {
            return Ok(());
        };
        generic::check_size(width as u64, height as u64)?;
        let reference = header
            .referred
            .iter()
            .find_map(|n| self.intermediate.remove(n))
            .map_or_else(
                || {
                    self.page
                        .window(i64::from(info.x), i64::from(info.y), width, height)
                },
                |(b, _)| Ok(b),
            )?;
        let params = RefinementParams {
            template,
            at,
            tpgron,
        };
        let data = body.get(pos..).unwrap_or(&[]);
        let mut mq = mq::MqDecoder::new(data);
        let mut cx = vec![0u8; GR_CONTEXTS];
        let bitmap =
            generic::decode_refinement(width, height, params, &reference, 0, 0, &mut mq, &mut cx)?;
        // §6.3.2 : le résultat remplace la zone raffinée.
        let info = RegionInfo {
            op: ComposeOp::Replace,
            ..info
        };
        self.compose(header, info, bitmap);
        Ok(())
    }

    /// Tables personnalisées référencées, dans l'ordre (§7.4.3.1.6).
    fn referred_tables(&self, header: &SegmentHeader) -> Vec<huffman::Table> {
        header
            .referred
            .iter()
            .filter_map(|n| self.tables.get(n).cloned())
            .collect()
    }

    /// Symboles des dictionnaires référencés, dans l'ordre.
    fn referred_symbols(&self, header: &SegmentHeader) -> Vec<Bitmap> {
        header
            .referred
            .iter()
            .filter_map(|n| self.symbol_dicts.get(n))
            .flat_map(|d| d.symbols.iter().cloned())
            .collect()
    }

    /// Dictionnaire de symboles (§7.4.3).
    fn symbol_dict(&mut self, header: &SegmentHeader, body: &[u8]) -> Result<()> {
        let input = self.referred_symbols(header);
        let tables = self.referred_tables(header);
        let reused = header
            .referred
            .iter()
            .filter_map(|n| self.symbol_dicts.get(n))
            .find_map(|d| d.retained.as_ref());
        let dict = symbol::decode_symbol_dict_segment(body, &input, &tables, reused)?;
        self.symbol_dicts.insert(
            header.number,
            StoredDict {
                symbols: dict.exported,
                retained: dict.retained,
            },
        );
        Ok(())
    }

    /// Région de texte (§7.4.4).
    fn text_region(&mut self, header: &SegmentHeader, body: &[u8]) -> Result<()> {
        let info = parse_region_info(body)?;
        let Some((width, height)) = self.clip_region(&info) else {
            return Ok(());
        };
        generic::check_size(width as u64, height as u64)?;
        let symbols = self.referred_symbols(header);
        if symbols.is_empty() {
            return Err(corrupt("région de texte sans dictionnaire"));
        }
        let tables = self.referred_tables(header);
        let data = body.get(17..).unwrap_or(&[]);
        let bitmap = symbol::decode_text_region_segment(data, width, height, &symbols, &tables)?;
        self.compose(header, info, bitmap);
        Ok(())
    }

    /// Région de demi-teintes (§7.4.5.2).
    fn halftone_region(&mut self, header: &SegmentHeader, body: &[u8]) -> Result<()> {
        let info = parse_region_info(body)?;
        let Some((width, height)) = self.clip_region(&info) else {
            return Ok(());
        };
        generic::check_size(width as u64, height as u64)?;
        let patterns: Vec<Bitmap> = header
            .referred
            .iter()
            .filter_map(|n| self.patterns.get(n))
            .flat_map(|p| p.iter().cloned())
            .collect();
        let data = body.get(17..).unwrap_or(&[]);
        let bitmap = halftone::decode_halftone_region_segment(data, width, height, &patterns)?;
        self.compose(header, info, bitmap);
        Ok(())
    }
}

/// Longueur inconnue (§7.2.7) : seules les régions génériques immédiates
/// l'admettent ; les données se terminent par `FF AC` (MQ) ou `00 00`
/// (MMR) suivi du nombre de lignes sur 4 octets. Rend (longueur du corps
/// utile, longueur totale consommée).
fn unknown_length_end(header: &SegmentHeader, body: &[u8]) -> Option<(usize, usize)> {
    if !matches!(
        header.kind,
        segment_type::GENERIC_IMMEDIATE | segment_type::GENERIC_IMMEDIATE_LOSSLESS
    ) {
        return None;
    }
    let flags = *body.get(17)?;
    let mmr = flags & 1 != 0;
    let template = (flags >> 1) & 3;
    let at_len = if mmr {
        0
    } else if template == 0 {
        8
    } else {
        2
    };
    let start = 18 + at_len;
    let terminator: [u8; 2] = if mmr { [0x00, 0x00] } else { [0xFF, 0xAC] };
    let offset = body
        .get(start..)?
        .windows(2)
        .position(|w| w == terminator)?;
    let end = start + offset + 2;
    Some((end, end + 4))
}

/// Décode un flux JBIG2 embarqué dans un PDF.
///
/// `globals` est le contenu du flux `/JBIG2Globals` éventuel ; `width` et
/// `height` viennent du dictionnaire d'image (/Width, /Height) et fixent la
/// taille de la page, dont la hauteur JBIG2 peut être inconnue
/// (0xFFFFFFFF, page en bandes).
///
/// Rend `height` lignes de `⌈width / 8⌉` octets, 1 bit par pixel, **0 =
/// noir** (voir la note de polarité du module).
///
/// # Errors
///
/// - `Error::Corrupt` si les dimensions sont nulles ou dépassent
///   [`MAX_BITMAP_PIXELS`], ou si aucun segment n'a pu être lu.
pub fn decode(data: &[u8], globals: Option<&[u8]>, width: u32, height: u32) -> Result<Vec<u8>> {
    if width == 0 || height == 0 {
        return Err(corrupt("dimensions nulles"));
    }
    let page = Bitmap::new(width as usize, height as usize, 0)?;
    let mut decoder = Decoder {
        page,
        page_default: 0,
        symbol_dicts: HashMap::new(),
        patterns: HashMap::new(),
        tables: HashMap::new(),
        intermediate: HashMap::new(),
        any_region: false,
    };
    if let Some(g) = globals {
        decoder.process_stream(g);
    }
    decoder.process_stream(data);
    if !decoder.any_region && parse_segment_header(data, 0).is_err() {
        return Err(corrupt("aucun segment lisible"));
    }
    // Inversion : JBIG2 code 1 = noir, le filtre livre 0 = noir (§7.4.7).
    let mut out = decoder.page.packed();
    for b in &mut out {
        *b = !*b;
    }
    Ok(out)
}
