//! Polices TrueType et OpenType (spécification OpenType de Microsoft,
//! chapitres « The OpenType Font File », `head`, `maxp`, `loca`, `glyf`,
//! `hhea`, `hmtx`, `cmap`, `post`, `OS/2`).
//!
//! Formats acceptés :
//! - fichiers `.ttf` (sfntVersion 0x00010000 ou `'true'`) ;
//! - collections `.ttc` (`'ttcf'`, première police par défaut) ;
//! - OpenType à contours CFF (`'OTTO'`) : la table `CFF ` est confiée à
//!   [`crate::cff::CffFont`].
//!
//! Tolérance (les polices incorporées dans les PDF, ISO 32000-2 §9.9, sont
//! souvent des sous-ensembles tronqués ou dépourvus de tables) :
//! - les sommes de contrôle ne sont pas vérifiées ;
//! - `loca` hors limites ou `glyf` manquante → glyphe vide ;
//! - `head` manquante → 1000 unités par em ;
//! - `cmap` absente (normal pour une police symbolique) → aucune
//!   correspondance, l'appelant passe par les codes ou les noms `post` ;
//! - `post` absente ou de format 3 → pas de noms de glyphes.
//!
//! Le hinting (tables `fpgm`, `prep`, `cvt `, instructions de `glyf`) est
//! ignoré : le rendu compte sur l'anti-aliasing (ARCHITECTURE.md § fonts/).

use acrux_core::{Error, Matrix, Path, Point, Rect, Result};

use crate::cff::CffFont;
use crate::glyph::GlyphProvider;
use crate::reader::Reader;

/// Profondeur maximale d'imbrication des glyphes composites.
const MAX_COMPOSITE_DEPTH: u8 = 8;
/// Nombre total de composants visités pour un glyphe (borne contre les
/// arbres de composites exponentiels).
const MAX_COMPONENT_VISITS: u32 = 4096;
/// Nombre maximal de tables lues dans le répertoire.
const MAX_TABLES: u16 = 512;
/// Nombre total d'entrées de `glyphIdArray` mémorisées pour une sous-table
/// `cmap` (borne mémoire pour les polices hostiles).
const MAX_CMAP_ARRAY_ENTRIES: usize = 1 << 20;

// Drapeaux des glyphes simples (table `glyf`, « Simple Glyph Flags »).
const ON_CURVE_POINT: u8 = 0x01;
const X_SHORT_VECTOR: u8 = 0x02;
const Y_SHORT_VECTOR: u8 = 0x04;
const REPEAT_FLAG: u8 = 0x08;
const X_IS_SAME_OR_POSITIVE: u8 = 0x10;
const Y_IS_SAME_OR_POSITIVE: u8 = 0x20;

// Drapeaux des glyphes composites (table `glyf`, « Composite Glyph Flags »).
const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
const ARGS_ARE_XY_VALUES: u16 = 0x0002;
const WE_HAVE_A_SCALE: u16 = 0x0008;
const MORE_COMPONENTS: u16 = 0x0020;
const WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;
const SCALED_COMPONENT_OFFSET: u16 = 0x0800;

/// Entrée du répertoire de tables.
#[derive(Debug, Clone, Copy)]
struct TableRecord {
    tag: [u8; 4],
    offset: usize,
    length: usize,
}

/// Point d'un contour TrueType avant conversion en chemin.
#[derive(Debug, Clone, Copy, PartialEq)]
struct OutlinePoint {
    x: f64,
    y: f64,
    on_curve: bool,
}

/// Contour : liste de points.
type Contour = Vec<OutlinePoint>;

/// Métriques verticales de la table `hhea`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hhea {
    /// Ascendante typographique.
    pub ascender: i16,
    /// Descendante typographique (négative).
    pub descender: i16,
    /// Interligne supplémentaire.
    pub line_gap: i16,
    /// Nombre d'entrées complètes de `hmtx`.
    pub number_of_h_metrics: u16,
}

/// Sous-ensemble de la table `OS/2` utile au rendu et à la substitution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Os2 {
    /// Version de la table.
    pub version: u16,
    /// Graisse (100 à 900).
    pub weight_class: u16,
    /// Chasse (1 à 9).
    pub width_class: u16,
    /// Drapeaux `fsSelection` (italique bit 0, gras bit 5…).
    pub selection: u16,
    /// Ascendante typographique.
    pub typo_ascender: i16,
    /// Descendante typographique.
    pub typo_descender: i16,
    /// Hauteur d'x (version ≥ 2), sinon 0.
    pub x_height: i16,
    /// Hauteur des capitales (version ≥ 2), sinon 0.
    pub cap_height: i16,
}

/// Plage d'une sous-table `cmap` normalisée.
#[derive(Debug, Clone)]
enum SegmentKind {
    /// GID = code + delta (formats 4 sans `idRangeOffset`, 12).
    Delta(i64),
    /// GID lus dans `glyphIdArray` (format 4 avec `idRangeOffset`, format 6).
    Glyphs(Vec<u16>),
}

#[derive(Debug, Clone)]
struct Segment {
    start: u32,
    end: u32,
    kind: SegmentKind,
}

/// Sous-table `cmap` d'un couple (plateforme, encodage).
#[derive(Debug, Clone)]
struct CmapSubtable {
    platform: u16,
    encoding: u16,
    /// Segments triés par code de début.
    segments: Vec<Segment>,
}

impl CmapSubtable {
    fn lookup(&self, code: u32) -> Option<u16> {
        let idx = self.segments.partition_point(|s| s.start <= code);
        let seg = self.segments[..idx].iter().rev().find(|s| code <= s.end)?;
        let gid = match &seg.kind {
            SegmentKind::Delta(d) => {
                let v = i64::from(code) + d;
                // Arithmétique modulo 65536 (format 4).
                u16::try_from(v.rem_euclid(65536)).ok()?
            }
            SegmentKind::Glyphs(g) => *g.get(usize::try_from(code - seg.start).ok()?)?,
        };
        (gid != 0).then_some(gid)
    }
}

/// Police TrueType / OpenType chargée en mémoire.
#[derive(Debug, Clone)]
pub struct TrueTypeFont {
    data: Vec<u8>,
    tables: Vec<TableRecord>,
    units_per_em: u16,
    bbox: Rect,
    num_glyphs: u16,
    /// Offsets `loca` (n + 1 entrées au plus), vide si absente.
    loca: Vec<u32>,
    glyf: Option<TableRecord>,
    hmtx: Option<TableRecord>,
    hhea: Option<Hhea>,
    os2: Option<Os2>,
    cmap: Vec<CmapSubtable>,
    /// Noms `post` par GID (chaîne vide si inconnu), vide sans table.
    post_names: Vec<String>,
    cff: Option<CffFont>,
}

impl TrueTypeFont {
    /// Analyse une police (fichier `.ttf`, `.otf`, ou première police
    /// d'une collection `.ttc`).
    ///
    /// # Errors
    /// `Error::Corrupt` si l'en-tête est illisible ou qu'aucune table de
    /// contours (`glyf` ou `CFF `) n'est présente.
    pub fn parse(data: &[u8]) -> Result<Self> {
        Self::parse_collection_index(data, 0)
    }

    /// Analyse la police numéro `index` d'une collection `.ttc` (`index`
    /// ignoré pour un fichier simple).
    ///
    /// # Errors
    /// Comme [`Self::parse`], plus `Error::Corrupt` si l'index dépasse le
    /// nombre de polices de la collection.
    pub fn parse_collection_index(data: &[u8], index: u32) -> Result<Self> {
        let mut r = Reader::new(data);
        let tag = r
            .read_tag()
            .ok_or_else(|| Error::Corrupt("police TrueType : en-tête tronqué".into()))?;
        let directory_offset = if &tag == b"ttcf" {
            // En-tête TTC : version, numFonts, tableDirectoryOffsets[numFonts].
            r.skip(4);
            let num_fonts = r.read_u32().unwrap_or(0);
            if index >= num_fonts {
                return Err(Error::Corrupt(format!(
                    "collection TrueType : police {index} absente ({num_fonts} polices)"
                )));
            }
            let off = usize::try_from(index)
                .ok()
                .and_then(|i| i.checked_mul(4))
                .and_then(|skip| r.skip(skip))
                .and_then(|()| r.read_u32())
                .ok_or_else(|| Error::Corrupt("collection TrueType tronquée".into()))?;
            usize::try_from(off).unwrap_or(usize::MAX)
        } else {
            0
        };
        Self::parse_at(data, directory_offset)
    }

    /// Nombre de polices d'une collection (1 pour un fichier simple).
    #[must_use]
    pub fn collection_count(data: &[u8]) -> u32 {
        let mut r = Reader::new(data);
        if r.read_tag().as_ref() == Some(b"ttcf") {
            r.skip(4);
            r.read_u32().unwrap_or(0)
        } else {
            1
        }
    }

    /// Analyse le répertoire de tables situé à `offset`.
    fn parse_at(data: &[u8], offset: usize) -> Result<Self> {
        let mut r = Reader::at(data, offset);
        let version = r
            .read_tag()
            .ok_or_else(|| Error::Corrupt("police TrueType : répertoire tronqué".into()))?;
        let known = matches!(&version, b"\0\x01\0\0" | b"true" | b"OTTO" | b"typ1");
        if !known {
            return Err(Error::Corrupt(format!(
                "police TrueType : signature inconnue {version:02X?}"
            )));
        }
        let num_tables = r.read_u16().unwrap_or(0).min(MAX_TABLES);
        r.skip(6); // searchRange, entrySelector, rangeShift
        let mut tables = Vec::with_capacity(usize::from(num_tables));
        for _ in 0..num_tables {
            let (Some(tag), Some(_checksum), Some(off), Some(len)) =
                (r.read_tag(), r.read_u32(), r.read_u32(), r.read_u32())
            else {
                break;
            };
            let off = usize::try_from(off).unwrap_or(usize::MAX);
            if off >= data.len() {
                continue;
            }
            // Une table tronquée est conservée, coupée à la fin du fichier.
            let length = usize::try_from(len)
                .unwrap_or(usize::MAX)
                .min(data.len() - off);
            tables.push(TableRecord {
                tag,
                offset: off,
                length,
            });
        }

        let mut font = TrueTypeFont {
            data: data.to_vec(),
            tables,
            units_per_em: 1000,
            bbox: Rect::default(),
            num_glyphs: 0,
            loca: Vec::new(),
            glyf: None,
            hmtx: None,
            hhea: None,
            os2: None,
            cmap: Vec::new(),
            post_names: Vec::new(),
            cff: None,
        };
        font.parse_tables()?;
        Ok(font)
    }

    fn table(&self, tag: [u8; 4]) -> Option<TableRecord> {
        self.tables.iter().copied().find(|t| t.tag == tag)
    }

    fn table_data(&self, rec: TableRecord) -> &[u8] {
        self.data
            .get(rec.offset..rec.offset + rec.length)
            .unwrap_or(&[])
    }

    fn parse_tables(&mut self) -> Result<()> {
        let index_to_loc_format = self.parse_head();
        self.parse_maxp();
        self.parse_hhea();
        self.parse_os2();
        self.hmtx = self.table(*b"hmtx");
        self.parse_cmap();
        self.parse_post();

        if let Some(cff_rec) = self.table(*b"CFF ") {
            if let Ok(cff) = CffFont::parse(self.table_data(cff_rec)) {
                if self.table(*b"head").is_none() {
                    // Pas de head : l'échelle vient de la FontMatrix CFF.
                    let scale = cff.font_matrix().a;
                    if scale > 0.0 {
                        self.units_per_em = upem_from_scale(scale);
                    }
                }
                if self.num_glyphs == 0 {
                    self.num_glyphs = u16::try_from(cff.glyph_count()).unwrap_or(u16::MAX);
                }
                self.cff = Some(cff);
            }
        }

        self.glyf = self.table(*b"glyf");
        if self.glyf.is_some() {
            self.parse_loca(index_to_loc_format);
        }
        if self.glyf.is_none() && self.cff.is_none() {
            return Err(Error::Corrupt(
                "police TrueType : ni table glyf ni table CFF".into(),
            ));
        }
        Ok(())
    }

    /// Table `head` ; renvoie `indexToLocFormat` (0 par défaut).
    fn parse_head(&mut self) -> i16 {
        let Some(rec) = self.table(*b"head") else {
            return 0;
        };
        let head = self.table_data(rec);
        let upem = Reader::at(head, 18).read_u16().filter(|&u| u > 0);
        let mut r = Reader::at(head, 36);
        let bbox = match (r.read_i16(), r.read_i16(), r.read_i16(), r.read_i16()) {
            (Some(x0), Some(y0), Some(x1), Some(y1)) => Some(Rect::new(
                f64::from(x0),
                f64::from(y0),
                f64::from(x1),
                f64::from(y1),
            )),
            _ => None,
        };
        let index_to_loc_format = Reader::at(head, 50).read_i16().unwrap_or(0);
        if let Some(upem) = upem {
            self.units_per_em = upem;
        }
        if let Some(bbox) = bbox {
            self.bbox = bbox;
        }
        index_to_loc_format
    }

    fn parse_maxp(&mut self) {
        if let Some(rec) = self.table(*b"maxp") {
            self.num_glyphs = Reader::at(self.table_data(rec), 4).read_u16().unwrap_or(0);
        }
    }

    fn parse_hhea(&mut self) {
        let Some(rec) = self.table(*b"hhea") else {
            return;
        };
        let hhea = self.table_data(rec);
        let mut r = Reader::at(hhea, 4);
        let (Some(ascender), Some(descender), Some(line_gap)) =
            (r.read_i16(), r.read_i16(), r.read_i16())
        else {
            return;
        };
        let number_of_h_metrics = Reader::at(hhea, 34).read_u16().unwrap_or(0);
        self.hhea = Some(Hhea {
            ascender,
            descender,
            line_gap,
            number_of_h_metrics,
        });
    }

    fn parse_os2(&mut self) {
        let Some(rec) = self.table(*b"OS/2") else {
            return;
        };
        let os2 = self.table_data(rec);
        let mut r = Reader::new(os2);
        let (Some(version), Some(_avg), Some(weight_class), Some(width_class)) =
            (r.read_u16(), r.read_i16(), r.read_u16(), r.read_u16())
        else {
            return;
        };
        let selection = Reader::at(os2, 62).read_u16().unwrap_or(0);
        let mut r = Reader::at(os2, 68);
        let typo_ascender = r.read_i16().unwrap_or(0);
        let typo_descender = r.read_i16().unwrap_or(0);
        let (x_height, cap_height) = if version >= 2 {
            let mut r = Reader::at(os2, 86);
            (r.read_i16().unwrap_or(0), r.read_i16().unwrap_or(0))
        } else {
            (0, 0)
        };
        self.os2 = Some(Os2 {
            version,
            weight_class,
            width_class,
            selection,
            typo_ascender,
            typo_descender,
            x_height,
            cap_height,
        });
    }

    /// Table `loca` : `numGlyphs + 1` offsets, courts (×2) ou longs.
    fn parse_loca(&mut self, index_to_loc_format: i16) {
        let Some(rec) = self.table(*b"loca") else {
            return;
        };
        let loca = self.table_data(rec);
        let entry_size = if index_to_loc_format == 0 { 2 } else { 4 };
        let wanted = usize::from(self.num_glyphs) + 1;
        let available = loca.len() / entry_size;
        let count = wanted.min(available);
        let mut r = Reader::new(loca);
        let mut offsets = Vec::with_capacity(count);
        for _ in 0..count {
            let Some(v) = (if entry_size == 2 {
                r.read_u16().map(|v| u32::from(v) * 2)
            } else {
                r.read_u32()
            }) else {
                break;
            };
            offsets.push(v);
        }
        self.loca = offsets;
    }

    /// Table `cmap` : en-tête puis sous-tables des formats 0, 4, 6 et 12.
    fn parse_cmap(&mut self) {
        let Some(rec) = self.table(*b"cmap") else {
            return;
        };
        let cmap = self.table_data(rec);
        let mut r = Reader::new(cmap);
        let (Some(_version), Some(num_tables)) = (r.read_u16(), r.read_u16()) else {
            return;
        };
        let mut subtables = Vec::new();
        for _ in 0..num_tables.min(64) {
            let (Some(platform), Some(encoding), Some(offset)) =
                (r.read_u16(), r.read_u16(), r.read_u32())
            else {
                break;
            };
            let offset = usize::try_from(offset).unwrap_or(usize::MAX);
            if let Some(segments) = parse_cmap_subtable(cmap, offset) {
                subtables.push(CmapSubtable {
                    platform,
                    encoding,
                    segments,
                });
            }
        }
        self.cmap = subtables;
    }

    /// Table `post` : noms de glyphes (formats 1.0 et 2.0).
    fn parse_post(&mut self) {
        let Some(rec) = self.table(*b"post") else {
            return;
        };
        let post = self.table_data(rec);
        let Some(version) = Reader::new(post).read_u32() else {
            return;
        };
        match version {
            0x0001_0000 => {
                self.post_names = MAC_GLYPH_NAMES.iter().map(|s| (*s).to_string()).collect();
            }
            0x0002_0000 => {
                let mut r = Reader::at(post, 32);
                let Some(count) = r.read_u16() else {
                    return;
                };
                let mut indices = Vec::with_capacity(usize::from(count));
                for _ in 0..count {
                    let Some(i) = r.read_u16() else {
                        break;
                    };
                    indices.push(i);
                }
                // Chaînes Pascal jusqu'à la fin de la table.
                let mut strings: Vec<String> = Vec::new();
                while let Some(len) = r.read_u8() {
                    let Some(bytes) = r.read_bytes(usize::from(len)) else {
                        break;
                    };
                    strings.push(String::from_utf8_lossy(bytes).into_owned());
                }
                self.post_names = indices
                    .iter()
                    .map(|&i| {
                        if usize::from(i) < MAC_GLYPH_NAMES.len() {
                            MAC_GLYPH_NAMES[usize::from(i)].to_string()
                        } else {
                            strings
                                .get(usize::from(i) - MAC_GLYPH_NAMES.len())
                                .cloned()
                                .unwrap_or_default()
                        }
                    })
                    .collect();
            }
            _ => {} // 2.5 (obsolète) et 3.0 (sans noms)
        }
    }

    /// Nombre de glyphes (`maxp`, ou CFF à défaut).
    #[must_use]
    pub fn glyph_count(&self) -> u16 {
        self.num_glyphs
    }

    /// Unités par em (`head`), 1000 si la table manque.
    #[must_use]
    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// Boîte englobante de la police (`head`), en unités de police.
    #[must_use]
    pub fn bbox(&self) -> Rect {
        self.bbox
    }

    /// Métriques `hhea`.
    #[must_use]
    pub fn hhea(&self) -> Option<Hhea> {
        self.hhea
    }

    /// Métriques `OS/2`.
    #[must_use]
    pub fn os2(&self) -> Option<Os2> {
        self.os2
    }

    /// Vrai si une table `glyf` est présente.
    #[must_use]
    pub fn has_glyf(&self) -> bool {
        self.glyf.is_some()
    }

    /// Vrai si les contours sont en CFF (`OTTO`).
    #[must_use]
    pub fn is_cff(&self) -> bool {
        self.cff.is_some()
    }

    /// Police CFF incorporée (`OTTO`).
    #[must_use]
    pub fn cff(&self) -> Option<&CffFont> {
        self.cff.as_ref()
    }

    /// Vrai si une table est présente (tag de quatre octets, ex. `*b"GSUB"`).
    #[must_use]
    pub fn has_table(&self, tag: [u8; 4]) -> bool {
        self.table(tag).is_some()
    }

    /// Octets bruts d'une table (`None` si absente).
    ///
    /// Utilisé par le sous-ensemble ([`crate::subset`]), qui recopie ou
    /// recalcule les tables une à une.
    #[must_use]
    pub fn table_bytes(&self, tag: [u8; 4]) -> Option<&[u8]> {
        self.table(tag).map(|rec| self.table_data(rec))
    }

    /// Avance et « left side bearing » bruts d'un glyphe (`hmtx`), en unités
    /// de police.
    ///
    /// Utilisé par le sous-ensemble ([`crate::subset`]), qui réécrit `hmtx`.
    #[must_use]
    pub fn metrics(&self, gid: u16) -> Option<(u16, i16)> {
        let hmtx = self.table_data(self.hmtx?);
        let num_h = self.hhea.map_or(0, |h| h.number_of_h_metrics);
        let num_h = if num_h == 0 {
            u16::try_from(hmtx.len() / 4).unwrap_or(u16::MAX)
        } else {
            num_h
        };
        if num_h == 0 {
            return None;
        }
        if gid < num_h {
            let mut r = Reader::at(hmtx, usize::from(gid) * 4);
            return Some((r.read_u16()?, r.read_i16()?));
        }
        // Au-delà : avance de la dernière entrée, lsb dans le tableau qui suit.
        let advance = Reader::at(hmtx, usize::from(num_h - 1) * 4).read_u16()?;
        let offset = usize::from(num_h) * 4 + usize::from(gid - num_h) * 2;
        Some((advance, Reader::at(hmtx, offset).read_i16().unwrap_or(0)))
    }

    /// Avance horizontale (`hmtx`) en unités de police.
    #[must_use]
    pub fn advance(&self, gid: u16) -> Option<u16> {
        let Some(rec) = self.hmtx else {
            // Pas de hmtx : largeur des charstrings pour une police OTTO.
            return self.cff_advance(gid);
        };
        let hmtx = self.table_data(rec);
        let num_h = self.hhea.map_or(0, |h| h.number_of_h_metrics);
        let num_h = if num_h == 0 {
            // hhea absente : autant d'entrées que la table en contient.
            u16::try_from(hmtx.len() / 4).unwrap_or(u16::MAX)
        } else {
            num_h
        };
        if num_h == 0 {
            return self.cff_advance(gid);
        }
        let index = gid.min(num_h - 1);
        Reader::at(hmtx, usize::from(index) * 4).read_u16()
    }

    /// Avance issue de la table `CFF ` (police `OTTO` sans `hmtx`).
    fn cff_advance(&self, gid: u16) -> Option<u16> {
        self.cff
            .as_ref()
            .and_then(|c| c.advance(u32::from(gid)))
            .and_then(to_u16)
    }

    /// Vrai si une sous-table `cmap` (plateforme, encodage) existe.
    #[must_use]
    pub fn has_cmap(&self, platform: u16, encoding: u16) -> bool {
        self.cmap
            .iter()
            .any(|s| s.platform == platform && s.encoding == encoding)
    }

    /// Couples (plateforme, encodage) des sous-tables `cmap`.
    #[must_use]
    pub fn cmap_subtables(&self) -> Vec<(u16, u16)> {
        self.cmap.iter().map(|s| (s.platform, s.encoding)).collect()
    }

    /// GID d'un code dans la sous-table (plateforme, encodage) ; `None` si
    /// la sous-table manque ou si le code n'est pas mappé (ou mappé à 0).
    #[must_use]
    pub fn cmap_lookup(&self, platform: u16, encoding: u16, code: u32) -> Option<u16> {
        self.cmap
            .iter()
            .find(|s| s.platform == platform && s.encoding == encoding)?
            .lookup(code)
    }

    /// GID d'un caractère Unicode via les sous-tables (3,10), (3,1) puis
    /// (0,x), dans cet ordre (ISO 32000-2 §9.6.5.4).
    #[must_use]
    pub fn unicode_to_gid(&self, c: char) -> Option<u16> {
        let code = u32::from(c);
        if let Some(g) = self.cmap_lookup(3, 10, code) {
            return Some(g);
        }
        if let Some(g) = self.cmap_lookup(3, 1, code) {
            return Some(g);
        }
        self.cmap
            .iter()
            .filter(|s| s.platform == 0)
            .find_map(|s| s.lookup(code))
    }

    /// Nom `post` d'un glyphe.
    #[must_use]
    pub fn glyph_name(&self, gid: u16) -> Option<&str> {
        self.post_names
            .get(usize::from(gid))
            .map(String::as_str)
            .filter(|n| !n.is_empty())
    }

    /// GID d'un nom de glyphe (`post`, puis CFF pour `OTTO`).
    #[must_use]
    pub fn gid_by_name(&self, name: &str) -> Option<u16> {
        if let Some(i) = self.post_names.iter().position(|n| n == name) {
            return u16::try_from(i).ok();
        }
        self.cff
            .as_ref()
            .and_then(|c| c.gid_by_name(name))
            .and_then(|g| u16::try_from(g).ok())
    }

    /// Contour d'un glyphe en unités de police ; `None` si le GID est hors
    /// de la police. Un glyphe vide ou illisible donne un chemin vide.
    #[must_use]
    pub fn glyph_path(&self, gid: u16) -> Option<Path> {
        if let Some(cff) = &self.cff {
            return cff.glyph_path(u32::from(gid));
        }
        if gid >= self.num_glyphs {
            return None;
        }
        let mut budget = MAX_COMPONENT_VISITS;
        let contours = self.glyph_contours(gid, 0, &mut budget).unwrap_or_default();
        let mut path = Path::new();
        for c in &contours {
            contour_to_path(c, &mut path);
        }
        Some(path)
    }

    /// Données `glyf` d'un glyphe (`None` si vide ou hors limites).
    ///
    /// Publique pour le sous-ensemble ([`crate::subset`]), qui recopie les
    /// descriptions telles quelles : les contours produits sont alors
    /// identiques à ceux de la police d'origine, au bit près.
    #[must_use]
    pub fn glyph_data(&self, gid: u16) -> Option<&[u8]> {
        let glyf = self.table_data(self.glyf?);
        let start = usize::try_from(*self.loca.get(usize::from(gid))?).ok()?;
        let end = usize::try_from(*self.loca.get(usize::from(gid) + 1)?).ok()?;
        if end <= start || start >= glyf.len() {
            return None;
        }
        glyf.get(start..end.min(glyf.len()))
    }

    /// Points d'un glyphe, composites résolus récursivement.
    fn glyph_contours(&self, gid: u16, depth: u8, budget: &mut u32) -> Option<Vec<Contour>> {
        if depth > MAX_COMPOSITE_DEPTH || *budget == 0 {
            return None;
        }
        *budget -= 1;
        let data = self.glyph_data(gid)?;
        let mut r = Reader::new(data);
        let number_of_contours = r.read_i16()?;
        r.skip(8)?; // xMin, yMin, xMax, yMax
        if number_of_contours >= 0 {
            parse_simple_glyph(&mut r, u16::try_from(number_of_contours).ok()?)
        } else {
            self.parse_composite_glyph(&mut r, depth, budget)
        }
    }

    /// Glyphe composite (`glyf`, « Composite Glyph Description »).
    fn parse_composite_glyph(
        &self,
        r: &mut Reader<'_>,
        depth: u8,
        budget: &mut u32,
    ) -> Option<Vec<Contour>> {
        let mut contours: Vec<Contour> = Vec::new();
        loop {
            let flags = r.read_u16()?;
            let glyph_index = r.read_u16()?;
            let (arg1, arg2) = if flags & ARG_1_AND_2_ARE_WORDS != 0 {
                (i32::from(r.read_i16()?), i32::from(r.read_i16()?))
            } else if flags & ARGS_ARE_XY_VALUES != 0 {
                (i32::from(r.read_i8()?), i32::from(r.read_i8()?))
            } else {
                (i32::from(r.read_u8()?), i32::from(r.read_u8()?))
            };
            let mut m = Matrix::IDENTITY;
            if flags & WE_HAVE_A_SCALE != 0 {
                let s = r.read_f2dot14()?;
                m = Matrix::scale(s, s);
            } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
                let sx = r.read_f2dot14()?;
                let sy = r.read_f2dot14()?;
                m = Matrix::scale(sx, sy);
            } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
                // xscale, scale01, scale10, yscale :
                // x' = xscale·x + scale10·y ; y' = scale01·x + yscale·y.
                let xscale = r.read_f2dot14()?;
                let scale01 = r.read_f2dot14()?;
                let scale10 = r.read_f2dot14()?;
                let yscale = r.read_f2dot14()?;
                m = Matrix::new(xscale, scale01, scale10, yscale, 0.0, 0.0);
            }

            if let Some(child) = self.glyph_contours(glyph_index, depth + 1, budget) {
                let mut child: Vec<Contour> = child
                    .into_iter()
                    .map(|c| c.into_iter().map(|p| transform_point(&m, p)).collect())
                    .collect();
                let (dx, dy) = if flags & ARGS_ARE_XY_VALUES != 0 {
                    let (dx, dy) = (f64::from(arg1), f64::from(arg2));
                    if flags & SCALED_COMPONENT_OFFSET != 0 {
                        let v = m.apply_vector(Point::new(dx, dy));
                        (v.x, v.y)
                    } else {
                        (dx, dy)
                    }
                } else {
                    // Appariement de points : le point `arg2` du composant
                    // vient se superposer au point `arg1` du parent.
                    let parent_pt = nth_point(&contours, arg1);
                    let child_pt = nth_point(&child, arg2);
                    match (parent_pt, child_pt) {
                        (Some(p), Some(c)) => (p.x - c.x, p.y - c.y),
                        _ => (0.0, 0.0),
                    }
                };
                for c in &mut child {
                    for p in c.iter_mut() {
                        p.x += dx;
                        p.y += dy;
                    }
                }
                contours.extend(child);
            }
            if flags & MORE_COMPONENTS == 0 {
                break;
            }
        }
        Some(contours)
    }
}

fn to_u16(v: f64) -> Option<u16> {
    if v.is_finite() && (0.0..=65535.0).contains(&v) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // borné
        Some(v.round() as u16)
    } else {
        None
    }
}

/// Unités par em déduites d'une échelle de FontMatrix (`1/upem`).
fn upem_from_scale(scale: f64) -> u16 {
    to_u16((1.0 / scale).round())
        .filter(|&u| u > 0)
        .unwrap_or(1000)
}

fn transform_point(m: &Matrix, p: OutlinePoint) -> OutlinePoint {
    let t = m.apply_vector(Point::new(p.x, p.y));
    OutlinePoint {
        x: t.x,
        y: t.y,
        on_curve: p.on_curve,
    }
}

/// `index`-ième point d'une liste de contours (numérotation continue).
fn nth_point(contours: &[Contour], index: i32) -> Option<OutlinePoint> {
    let mut index = usize::try_from(index).ok()?;
    for c in contours {
        if index < c.len() {
            return Some(c[index]);
        }
        index -= c.len();
    }
    None
}

/// Glyphe simple (`glyf`, « Simple Glyph Description ») : fins de
/// contours, instructions, drapeaux puis coordonnées en deltas.
fn parse_simple_glyph(r: &mut Reader<'_>, number_of_contours: u16) -> Option<Vec<Contour>> {
    let mut end_pts = Vec::with_capacity(usize::from(number_of_contours));
    for _ in 0..number_of_contours {
        end_pts.push(r.read_u16()?);
    }
    let num_points = end_pts.last().map_or(0, |&e| usize::from(e) + 1);
    let instruction_length = r.read_u16()?;
    r.skip(usize::from(instruction_length))?;

    // Drapeaux, avec répétitions.
    let mut flags = Vec::with_capacity(num_points);
    while flags.len() < num_points {
        let f = r.read_u8()?;
        flags.push(f);
        if f & REPEAT_FLAG != 0 {
            let n = r.read_u8()?;
            for _ in 0..n {
                if flags.len() >= num_points {
                    break;
                }
                flags.push(f);
            }
        }
    }

    let xs = read_coordinates(r, &flags, X_SHORT_VECTOR, X_IS_SAME_OR_POSITIVE)?;
    let ys = read_coordinates(r, &flags, Y_SHORT_VECTOR, Y_IS_SAME_OR_POSITIVE)?;

    let mut contours = Vec::with_capacity(end_pts.len());
    let mut start = 0usize;
    for &end in &end_pts {
        let end = usize::from(end);
        if end < start || end >= num_points {
            // Fins de contours non croissantes : on arrête proprement.
            break;
        }
        let contour: Contour = (start..=end)
            .map(|i| OutlinePoint {
                x: f64::from(xs[i]),
                y: f64::from(ys[i]),
                on_curve: flags[i] & ON_CURVE_POINT != 0,
            })
            .collect();
        contours.push(contour);
        start = end + 1;
    }
    Some(contours)
}

/// Coordonnées absolues d'un axe (deltas cumulés selon les drapeaux).
fn read_coordinates(
    r: &mut Reader<'_>,
    flags: &[u8],
    short_flag: u8,
    same_flag: u8,
) -> Option<Vec<i32>> {
    let mut out = Vec::with_capacity(flags.len());
    let mut v: i32 = 0;
    for &f in flags {
        let delta = if f & short_flag != 0 {
            let d = i32::from(r.read_u8()?);
            if f & same_flag != 0 {
                d
            } else {
                -d
            }
        } else if f & same_flag != 0 {
            0
        } else {
            i32::from(r.read_i16()?)
        };
        v = v.saturating_add(delta);
        out.push(v);
    }
    Some(out)
}

/// Convertit un contour TrueType (points on/off-curve, points implicites
/// entre deux points off-curve consécutifs) en chemin de Bézier
/// quadratiques (`glyf`, « Glyph Outline Description »).
fn contour_to_path(contour: &[OutlinePoint], path: &mut Path) {
    let n = contour.len();
    if n == 0 {
        return;
    }
    let pt = |p: &OutlinePoint| Point::new(p.x, p.y);
    let mid = |a: Point, b: Point| Point::new(f64::midpoint(a.x, b.x), f64::midpoint(a.y, b.y));

    // Point de départ : premier point on-curve, sinon le milieu entre le
    // dernier et le premier point (tous off-curve).
    let first_on = contour.iter().position(|p| p.on_curve);
    let (start, order): (Point, Vec<usize>) = match first_on {
        Some(i) => (pt(&contour[i]), (1..n).map(|k| (i + k) % n).collect()),
        None => (mid(pt(&contour[n - 1]), pt(&contour[0])), (0..n).collect()),
    };
    path.move_to(start);
    let mut prev_off: Option<Point> = None;
    for i in order {
        let p = pt(&contour[i]);
        if contour[i].on_curve {
            match prev_off.take() {
                Some(c) => path.quad_to(c, p),
                None => path.line_to(p),
            }
        } else {
            if let Some(c) = prev_off {
                path.quad_to(c, mid(c, p));
            }
            prev_off = Some(p);
        }
    }
    match prev_off {
        Some(c) => path.quad_to(c, start),
        None => path.line_to(start),
    }
    path.close();
}

/// Sous-table `cmap` à `offset` : formats 0, 4, 6 et 12.
fn parse_cmap_subtable(cmap: &[u8], offset: usize) -> Option<Vec<Segment>> {
    let mut r = Reader::at(cmap, offset);
    let format = r.read_u16()?;
    let mut segments = match format {
        0 => parse_cmap_format0(&mut r)?,
        4 => parse_cmap_format4(cmap, offset)?,
        6 => parse_cmap_format6(&mut r)?,
        12 => parse_cmap_format12(&mut r)?,
        _ => return None,
    };
    segments.sort_by_key(|s| s.start);
    Some(segments)
}

/// Format 0 : 256 GID sur un octet.
fn parse_cmap_format0(r: &mut Reader<'_>) -> Option<Vec<Segment>> {
    r.skip(4)?; // length, language
    let bytes = r.read_bytes(256)?;
    Some(vec![Segment {
        start: 0,
        end: 255,
        kind: SegmentKind::Glyphs(bytes.iter().map(|&b| u16::from(b)).collect()),
    }])
}

/// Format 4 : segments avec delta ou `idRangeOffset` vers `glyphIdArray`.
fn parse_cmap_format4(cmap: &[u8], offset: usize) -> Option<Vec<Segment>> {
    let mut r = Reader::at(cmap, offset + 6); // format, length, language
    let seg_count = usize::from(r.read_u16()? / 2);
    r.skip(6)?; // searchRange, entrySelector, rangeShift
    let end_base = r.pos();
    let start_base = end_base + seg_count * 2 + 2; // reservedPad
    let delta_base = start_base + seg_count * 2;
    let range_base = delta_base + seg_count * 2;
    let mut segments = Vec::with_capacity(seg_count);
    let mut stored = 0usize;
    for i in 0..seg_count {
        let end = u32::from(Reader::at(cmap, end_base + i * 2).read_u16()?);
        let start = u32::from(Reader::at(cmap, start_base + i * 2).read_u16()?);
        let delta = Reader::at(cmap, delta_base + i * 2).read_i16()?;
        let range_pos = range_base + i * 2;
        let range_offset = Reader::at(cmap, range_pos).read_u16()?;
        if start > end || start == 0xFFFF {
            continue;
        }
        if range_offset == 0 {
            segments.push(Segment {
                start,
                end,
                kind: SegmentKind::Delta(i64::from(delta)),
            });
            continue;
        }
        let count = usize::try_from(end - start + 1).ok()?;
        if stored + count > MAX_CMAP_ARRAY_ENTRIES {
            break;
        }
        stored += count;
        let mut glyphs = Vec::with_capacity(count);
        for k in 0..count {
            // Adresse relative à l'entrée idRangeOffset[i] elle-même.
            let addr = range_pos + usize::from(range_offset) + k * 2;
            let g = Reader::at(cmap, addr).read_u16().unwrap_or(0);
            let g = if g == 0 {
                0
            } else {
                (i32::from(g) + i32::from(delta)).rem_euclid(65536)
            };
            glyphs.push(u16::try_from(g).unwrap_or(0));
        }
        segments.push(Segment {
            start,
            end,
            kind: SegmentKind::Glyphs(glyphs),
        });
    }
    Some(segments)
}

/// Format 6 : tableau dense à partir de `firstCode`.
fn parse_cmap_format6(r: &mut Reader<'_>) -> Option<Vec<Segment>> {
    r.skip(4)?; // length, language
    let first = u32::from(r.read_u16()?);
    let count = usize::from(r.read_u16()?);
    let mut glyphs = Vec::with_capacity(count);
    for _ in 0..count {
        let Some(g) = r.read_u16() else {
            break;
        };
        glyphs.push(g);
    }
    if glyphs.is_empty() {
        return Some(Vec::new());
    }
    let end = first + u32::try_from(glyphs.len()).ok()? - 1;
    Some(vec![Segment {
        start: first,
        end,
        kind: SegmentKind::Glyphs(glyphs),
    }])
}

/// Format 12 : groupes séquentiels sur 32 bits.
fn parse_cmap_format12(r: &mut Reader<'_>) -> Option<Vec<Segment>> {
    r.skip(10)?; // reserved, length, language
    let num_groups = r.read_u32()?;
    let mut segments = Vec::new();
    for _ in 0..num_groups {
        let (Some(start), Some(end), Some(start_gid)) = (r.read_u32(), r.read_u32(), r.read_u32())
        else {
            break;
        };
        if start > end {
            continue;
        }
        segments.push(Segment {
            start,
            end,
            kind: SegmentKind::Delta(i64::from(start_gid) - i64::from(start)),
        });
    }
    Some(segments)
}

impl GlyphProvider for TrueTypeFont {
    fn glyph_path(&self, gid: u32) -> Option<Path> {
        TrueTypeFont::glyph_path(self, u16::try_from(gid).ok()?)
    }

    fn advance(&self, gid: u32) -> Option<f64> {
        TrueTypeFont::advance(self, u16::try_from(gid).ok()?).map(f64::from)
    }

    fn units_per_em(&self) -> f64 {
        f64::from(self.units_per_em)
    }

    fn glyph_count(&self) -> u32 {
        u32::from(self.num_glyphs)
    }
}

/// Les 258 noms de glyphes Macintosh standard de la table `post`
/// (spécification OpenType, `post`, « Macintosh ordering »).
pub const MAC_GLYPH_NAMES: [&str; 258] = [
    ".notdef",
    ".null",
    "nonmarkingreturn",
    "space",
    "exclam",
    "quotedbl",
    "numbersign",
    "dollar",
    "percent",
    "ampersand",
    "quotesingle",
    "parenleft",
    "parenright",
    "asterisk",
    "plus",
    "comma",
    "hyphen",
    "period",
    "slash",
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "colon",
    "semicolon",
    "less",
    "equal",
    "greater",
    "question",
    "at",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "bracketleft",
    "backslash",
    "bracketright",
    "asciicircum",
    "underscore",
    "grave",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "braceleft",
    "bar",
    "braceright",
    "asciitilde",
    "Adieresis",
    "Aring",
    "Ccedilla",
    "Eacute",
    "Ntilde",
    "Odieresis",
    "Udieresis",
    "aacute",
    "agrave",
    "acircumflex",
    "adieresis",
    "atilde",
    "aring",
    "ccedilla",
    "eacute",
    "egrave",
    "ecircumflex",
    "edieresis",
    "iacute",
    "igrave",
    "icircumflex",
    "idieresis",
    "ntilde",
    "oacute",
    "ograve",
    "ocircumflex",
    "odieresis",
    "otilde",
    "uacute",
    "ugrave",
    "ucircumflex",
    "udieresis",
    "dagger",
    "degree",
    "cent",
    "sterling",
    "section",
    "bullet",
    "paragraph",
    "germandbls",
    "registered",
    "copyright",
    "trademark",
    "acute",
    "dieresis",
    "notequal",
    "AE",
    "Oslash",
    "infinity",
    "plusminus",
    "lessequal",
    "greaterequal",
    "yen",
    "mu",
    "partialdiff",
    "summation",
    "product",
    "pi",
    "integral",
    "ordfeminine",
    "ordmasculine",
    "Omega",
    "ae",
    "oslash",
    "questiondown",
    "exclamdown",
    "logicalnot",
    "radical",
    "florin",
    "approxequal",
    "Delta",
    "guillemotleft",
    "guillemotright",
    "ellipsis",
    "nonbreakingspace",
    "Agrave",
    "Atilde",
    "Otilde",
    "OE",
    "oe",
    "endash",
    "emdash",
    "quotedblleft",
    "quotedblright",
    "quoteleft",
    "quoteright",
    "divide",
    "lozenge",
    "ydieresis",
    "Ydieresis",
    "fraction",
    "currency",
    "guilsinglleft",
    "guilsinglright",
    "fi",
    "fl",
    "daggerdbl",
    "periodcentered",
    "quotesinglbase",
    "quotedblbase",
    "perthousand",
    "Acircumflex",
    "Ecircumflex",
    "Aacute",
    "Edieresis",
    "Egrave",
    "Iacute",
    "Icircumflex",
    "Idieresis",
    "Igrave",
    "Oacute",
    "Ocircumflex",
    "apple",
    "Ograve",
    "Uacute",
    "Ucircumflex",
    "Ugrave",
    "dotlessi",
    "circumflex",
    "tilde",
    "macron",
    "breve",
    "dotaccent",
    "ring",
    "cedilla",
    "hungarumlaut",
    "ogonek",
    "caron",
    "Lslash",
    "lslash",
    "Scaron",
    "scaron",
    "Zcaron",
    "zcaron",
    "brokenbar",
    "Eth",
    "eth",
    "Yacute",
    "yacute",
    "Thorn",
    "thorn",
    "minus",
    "multiply",
    "onesuperior",
    "twosuperior",
    "threesuperior",
    "onehalf",
    "onequarter",
    "threequarters",
    "franc",
    "Gbreve",
    "gbreve",
    "Idotaccent",
    "Scedilla",
    "scedilla",
    "Cacute",
    "cacute",
    "Ccaron",
    "ccaron",
    "dcroat",
];

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]
pub(crate) mod tests {
    use super::*;
    use acrux_core::PathCommand;

    /// Encodeur de tables pour fabriquer des polices de test.
    #[derive(Default)]
    pub(crate) struct Builder {
        tables: Vec<([u8; 4], Vec<u8>)>,
    }

    impl Builder {
        pub(crate) fn new() -> Self {
            Self { tables: Vec::new() }
        }

        pub(crate) fn table(mut self, tag: [u8; 4], data: Vec<u8>) -> Self {
            self.tables.push((tag, data));
            self
        }

        /// Sérialise le répertoire de tables et les tables (alignées sur 4).
        pub(crate) fn build(self, version: [u8; 4]) -> Vec<u8> {
            let n = self.tables.len();
            let mut out = Vec::new();
            out.extend_from_slice(&version);
            out.extend_from_slice(&(n as u16).to_be_bytes());
            out.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
            let mut offset = 12 + 16 * n;
            let mut body = Vec::new();
            for (tag, data) in &self.tables {
                out.extend_from_slice(tag);
                out.extend_from_slice(&[0, 0, 0, 0]);
                out.extend_from_slice(&(offset as u32).to_be_bytes());
                out.extend_from_slice(&(data.len() as u32).to_be_bytes());
                body.extend_from_slice(data);
                while body.len() % 4 != 0 {
                    body.push(0);
                }
                offset = 12 + 16 * n + body.len();
            }
            out.extend_from_slice(&body);
            out
        }
    }

    pub(crate) fn be16(v: i32) -> [u8; 2] {
        (v as i16).to_be_bytes()
    }

    pub(crate) fn head_table(upem: u16, long_loca: bool) -> Vec<u8> {
        let mut t = vec![0u8; 54];
        t[0..4].copy_from_slice(&[0, 1, 0, 0]);
        t[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
        t[18..20].copy_from_slice(&upem.to_be_bytes());
        t[36..38].copy_from_slice(&be16(-50));
        t[38..40].copy_from_slice(&be16(-200));
        t[40..42].copy_from_slice(&be16(1000));
        t[42..44].copy_from_slice(&be16(800));
        t[50..52].copy_from_slice(&be16(i32::from(long_loca)));
        t
    }

    pub(crate) fn maxp_table(num_glyphs: u16) -> Vec<u8> {
        let mut t = vec![0, 1, 0, 0];
        t.extend_from_slice(&num_glyphs.to_be_bytes());
        t.extend_from_slice(&[0; 26]);
        t
    }

    pub(crate) fn hhea_table(num_h_metrics: u16) -> Vec<u8> {
        let mut t = vec![0u8; 36];
        t[0..4].copy_from_slice(&[0, 1, 0, 0]);
        t[4..6].copy_from_slice(&be16(800));
        t[6..8].copy_from_slice(&be16(-200));
        t[8..10].copy_from_slice(&be16(90));
        t[34..36].copy_from_slice(&num_h_metrics.to_be_bytes());
        t
    }

    pub(crate) fn hmtx_table(advances: &[u16]) -> Vec<u8> {
        let mut t = Vec::new();
        for a in advances {
            t.extend_from_slice(&a.to_be_bytes());
            t.extend_from_slice(&[0, 0]);
        }
        t
    }

    /// Glyphe simple : un contour par liste de points `(x, y, on_curve)`.
    pub(crate) fn simple_glyph(contours: &[&[(i16, i16, bool)]]) -> Vec<u8> {
        let mut g = Vec::new();
        g.extend_from_slice(&be16(contours.len() as i32));
        g.extend_from_slice(&[0; 8]);
        let mut end = -1i32;
        for c in contours {
            end += c.len() as i32;
            g.extend_from_slice(&be16(end));
        }
        g.extend_from_slice(&[0, 0]); // instructionLength
        let pts: Vec<(i16, i16, bool)> = contours.iter().flat_map(|c| c.iter().copied()).collect();
        for p in &pts {
            g.push(u8::from(p.2)); // coordonnées longues, jamais « same »
        }
        let mut prev = 0i16;
        for p in &pts {
            g.extend_from_slice(&(p.0 - prev).to_be_bytes());
            prev = p.0;
        }
        prev = 0;
        for p in &pts {
            g.extend_from_slice(&(p.1 - prev).to_be_bytes());
            prev = p.1;
        }
        g
    }

    /// Glyphe simple utilisant les drapeaux courts et répétés.
    fn compact_square_glyph() -> Vec<u8> {
        // Carré 0,0 → 100,0 → 100,100 → 0,100 avec deltas courts positifs
        // puis négatifs, et un drapeau répété.
        let mut g = Vec::new();
        g.extend_from_slice(&be16(1));
        g.extend_from_slice(&[0; 8]);
        g.extend_from_slice(&be16(3));
        g.extend_from_slice(&[0, 0]);
        // Point 0 : on-curve, x same (0), y same (0).
        g.push(ON_CURVE_POINT | X_IS_SAME_OR_POSITIVE | Y_IS_SAME_OR_POSITIVE);
        // Point 1 : x court positif +100, y same.
        g.push(ON_CURVE_POINT | X_SHORT_VECTOR | X_IS_SAME_OR_POSITIVE | Y_IS_SAME_OR_POSITIVE);
        // Point 2 : x same, y court positif +100.
        g.push(ON_CURVE_POINT | X_IS_SAME_OR_POSITIVE | Y_SHORT_VECTOR | Y_IS_SAME_OR_POSITIVE);
        // Point 3 : x court négatif -100, y same.
        g.push(ON_CURVE_POINT | X_SHORT_VECTOR | Y_IS_SAME_OR_POSITIVE);
        g.extend_from_slice(&[100, 100]); // x : points 1 et 3
        g.extend_from_slice(&[100]); // y : point 2
        g
    }

    pub(crate) fn cmap_format4(pairs: &[(u16, u16)]) -> Vec<u8> {
        // Un segment par code, plus le segment final 0xFFFF.
        let seg_count = pairs.len() + 1;
        let mut t = Vec::new();
        t.extend_from_slice(&4u16.to_be_bytes());
        t.extend_from_slice(&((16 + seg_count * 8) as u16).to_be_bytes());
        t.extend_from_slice(&[0, 0]);
        t.extend_from_slice(&((seg_count * 2) as u16).to_be_bytes());
        t.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        for (c, _) in pairs {
            t.extend_from_slice(&c.to_be_bytes());
        }
        t.extend_from_slice(&[0xFF, 0xFF, 0, 0]);
        for (c, _) in pairs {
            t.extend_from_slice(&c.to_be_bytes());
        }
        t.extend_from_slice(&[0xFF, 0xFF]);
        for (c, g) in pairs {
            t.extend_from_slice(&be16(i32::from(*g) - i32::from(*c)));
        }
        t.extend_from_slice(&[0, 1]);
        for _ in 0..seg_count {
            t.extend_from_slice(&[0, 0]);
        }
        t
    }

    /// Format 4 avec `idRangeOffset` et `glyphIdArray` (un seul segment).
    fn cmap_format4_with_array(start: u16, glyphs: &[u16]) -> Vec<u8> {
        let seg_count = 2;
        let mut t = Vec::new();
        t.extend_from_slice(&4u16.to_be_bytes());
        t.extend_from_slice(&((16 + seg_count * 8 + glyphs.len() * 2) as u16).to_be_bytes());
        t.extend_from_slice(&[0, 0]);
        t.extend_from_slice(&((seg_count * 2) as u16).to_be_bytes());
        t.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let end = start + glyphs.len() as u16 - 1;
        t.extend_from_slice(&end.to_be_bytes());
        t.extend_from_slice(&[0xFF, 0xFF, 0, 0]);
        t.extend_from_slice(&start.to_be_bytes());
        t.extend_from_slice(&[0xFF, 0xFF]);
        t.extend_from_slice(&[0, 0, 0, 1]); // idDelta
                                            // idRangeOffset[0] : distance jusqu'à glyphIdArray = 2 entrées × 2.
        t.extend_from_slice(&4u16.to_be_bytes());
        t.extend_from_slice(&[0, 0]);
        for g in glyphs {
            t.extend_from_slice(&g.to_be_bytes());
        }
        t
    }

    pub(crate) fn cmap_format12(groups: &[(u32, u32, u32)]) -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(&12u16.to_be_bytes());
        t.extend_from_slice(&[0, 0]);
        t.extend_from_slice(&((16 + groups.len() * 12) as u32).to_be_bytes());
        t.extend_from_slice(&[0, 0, 0, 0]);
        t.extend_from_slice(&(groups.len() as u32).to_be_bytes());
        for (s, e, g) in groups {
            t.extend_from_slice(&s.to_be_bytes());
            t.extend_from_slice(&e.to_be_bytes());
            t.extend_from_slice(&g.to_be_bytes());
        }
        t
    }

    fn cmap_format0(pairs: &[(u8, u8)]) -> Vec<u8> {
        let mut t = vec![0, 0, 1, 6, 0, 0];
        let mut map = [0u8; 256];
        for (c, g) in pairs {
            map[usize::from(*c)] = *g;
        }
        t.extend_from_slice(&map);
        t
    }

    fn cmap_format6(first: u16, glyphs: &[u16]) -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(&6u16.to_be_bytes());
        t.extend_from_slice(&((10 + glyphs.len() * 2) as u16).to_be_bytes());
        t.extend_from_slice(&[0, 0]);
        t.extend_from_slice(&first.to_be_bytes());
        t.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
        for g in glyphs {
            t.extend_from_slice(&g.to_be_bytes());
        }
        t
    }

    pub(crate) fn cmap_table(subtables: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
        let mut t = vec![0, 0];
        t.extend_from_slice(&(subtables.len() as u16).to_be_bytes());
        let mut offset = 4 + 8 * subtables.len();
        let mut body = Vec::new();
        for (p, e, data) in subtables {
            t.extend_from_slice(&p.to_be_bytes());
            t.extend_from_slice(&e.to_be_bytes());
            t.extend_from_slice(&(offset as u32).to_be_bytes());
            body.extend_from_slice(data);
            offset += data.len();
        }
        t.extend_from_slice(&body);
        t
    }

    pub(crate) fn post_table_v2(names: &[&str]) -> Vec<u8> {
        let mut t = vec![0u8; 32];
        t[0..4].copy_from_slice(&[0, 2, 0, 0]);
        t.extend_from_slice(&(names.len() as u16).to_be_bytes());
        let mut strings = Vec::new();
        for name in names {
            let index = MAC_GLYPH_NAMES.iter().position(|m| m == name);
            if let Some(i) = index {
                t.extend_from_slice(&(i as u16).to_be_bytes());
            } else {
                t.extend_from_slice(&((258 + strings.len()) as u16).to_be_bytes());
                strings.push(*name);
            }
        }
        for s in strings {
            t.push(s.len() as u8);
            t.extend_from_slice(s.as_bytes());
        }
        t
    }

    /// Construit `loca` (format long) et `glyf` à partir des glyphes.
    pub(crate) fn loca_and_glyf(glyphs: &[Vec<u8>]) -> (Vec<u8>, Vec<u8>) {
        let mut loca = Vec::new();
        let mut glyf = Vec::new();
        for g in glyphs {
            loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
            glyf.extend_from_slice(g);
            while glyf.len() % 2 != 0 {
                glyf.push(0);
            }
        }
        loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
        (loca, glyf)
    }

    /// Police de test : 0 = .notdef vide, 1 = carré, 2 = composite
    /// (carré décalé et carré mis à l'échelle), 3 = contour avec courbes,
    /// 4 = carré compact (drapeaux courts), 5 = composite par appariement
    /// de points.
    pub(crate) fn sample_font() -> Vec<u8> {
        let square: &[(i16, i16, bool)] = &[
            (0, 0, true),
            (100, 0, true),
            (100, 100, true),
            (0, 100, true),
        ];
        let curve: &[(i16, i16, bool)] = &[(0, 0, true), (50, 100, false), (100, 0, true)];
        let mut composite = Vec::new();
        composite.extend_from_slice(&be16(-1));
        composite.extend_from_slice(&[0; 8]);
        // Composant 1 : carré décalé de (200, 0), arguments sur 16 bits.
        composite.extend_from_slice(
            &(ARG_1_AND_2_ARE_WORDS | ARGS_ARE_XY_VALUES | MORE_COMPONENTS).to_be_bytes(),
        );
        composite.extend_from_slice(&1u16.to_be_bytes());
        composite.extend_from_slice(&be16(200));
        composite.extend_from_slice(&be16(0));
        // Composant 2 : carré à l'échelle 0.5, arguments sur 8 bits (10, 20).
        composite.extend_from_slice(&(ARGS_ARE_XY_VALUES | WE_HAVE_A_SCALE).to_be_bytes());
        composite.extend_from_slice(&1u16.to_be_bytes());
        composite.extend_from_slice(&[10, 20]);
        composite.extend_from_slice(&0x2000u16.to_be_bytes()); // 0.5 en F2Dot14

        let mut matched = Vec::new();
        matched.extend_from_slice(&be16(-1));
        matched.extend_from_slice(&[0; 8]);
        matched.extend_from_slice(&MORE_COMPONENTS.to_be_bytes());
        matched.extend_from_slice(&1u16.to_be_bytes());
        matched.extend_from_slice(&[0, 0]);
        // Second carré : son point 0 vient sur le point 2 (100,100) du parent.
        matched.extend_from_slice(&WE_HAVE_A_TWO_BY_TWO.to_be_bytes());
        matched.extend_from_slice(&1u16.to_be_bytes());
        matched.extend_from_slice(&[2, 0]);
        // Matrice [0 1 ; -1 0] : rotation de -90°.
        matched.extend_from_slice(&0u16.to_be_bytes());
        matched.extend_from_slice(&0x4000u16.to_be_bytes());
        matched.extend_from_slice(&(-0x4000i16).to_be_bytes());
        matched.extend_from_slice(&0u16.to_be_bytes());

        let glyphs = vec![
            Vec::new(),
            simple_glyph(&[square]),
            composite,
            simple_glyph(&[curve]),
            compact_square_glyph(),
            matched,
        ];
        let (loca, glyf) = loca_and_glyf(&glyphs);
        Builder::new()
            .table(*b"head", head_table(1000, true))
            .table(*b"maxp", maxp_table(6))
            .table(*b"hhea", hhea_table(2))
            .table(*b"hmtx", hmtx_table(&[500, 600]))
            .table(*b"loca", loca)
            .table(*b"glyf", glyf)
            .table(
                *b"cmap",
                cmap_table(&[
                    (3, 1, cmap_format4(&[(0x41, 1), (0x42, 2)])),
                    (3, 10, cmap_format12(&[(0x1F600, 0x1F602, 3)])),
                    (1, 0, cmap_format0(&[(0x61, 4)])),
                    (0, 3, cmap_format6(0x100, &[5, 0, 1])),
                    (3, 0, cmap_format4_with_array(0xF041, &[3, 0, 4])),
                ]),
            )
            .table(
                *b"post",
                post_table_v2(&[".notdef", "A", "B", "curve", "space", "matched"]),
            )
            .build(*b"\0\x01\0\0")
    }

    #[test]
    fn mac_glyph_names_count() {
        assert_eq!(MAC_GLYPH_NAMES.len(), 258);
        assert_eq!(MAC_GLYPH_NAMES[3], "space");
        assert_eq!(MAC_GLYPH_NAMES[257], "dcroat");
    }

    #[test]
    fn parses_header_and_metrics() {
        let font = TrueTypeFont::parse(&sample_font()).unwrap();
        assert_eq!(font.glyph_count(), 6);
        assert_eq!(font.units_per_em(), 1000);
        assert!(font.has_glyf());
        assert!(!font.is_cff());
        assert!(font.cff().is_none());
        assert_eq!(font.bbox(), Rect::new(-50.0, -200.0, 1000.0, 800.0));
        assert_eq!(font.hhea().unwrap().ascender, 800);
        assert_eq!(font.advance(0), Some(500));
        assert_eq!(font.advance(1), Some(600));
        // Au-delà de numberOfHMetrics : dernière avance.
        assert_eq!(font.advance(5), Some(600));
        assert!(font.has_table(*b"post"));
        assert!(!font.has_table(*b"GSUB"));
        assert_eq!(TrueTypeFont::collection_count(&sample_font()), 1);
    }

    #[test]
    fn simple_square_points() {
        let font = TrueTypeFont::parse(&sample_font()).unwrap();
        let path = font.glyph_path(1).unwrap();
        assert_eq!(
            path.commands(),
            &[
                PathCommand::MoveTo(Point::new(0.0, 0.0)),
                PathCommand::LineTo(Point::new(100.0, 0.0)),
                PathCommand::LineTo(Point::new(100.0, 100.0)),
                PathCommand::LineTo(Point::new(0.0, 100.0)),
                PathCommand::LineTo(Point::new(0.0, 0.0)),
                PathCommand::Close,
            ]
        );
        // Même carré encodé avec drapeaux courts / répétés.
        assert_eq!(font.glyph_path(4).unwrap().commands(), path.commands());
        // Glyphe vide.
        assert!(font.glyph_path(0).unwrap().is_empty());
        assert!(font.glyph_path(6).is_none());
    }

    #[test]
    fn quadratic_contour() {
        let font = TrueTypeFont::parse(&sample_font()).unwrap();
        let path = font.glyph_path(3).unwrap();
        let cmds = path.commands();
        assert_eq!(cmds[0], PathCommand::MoveTo(Point::new(0.0, 0.0)));
        match cmds[1] {
            PathCommand::CurveTo(c1, c2, p) => {
                // Quadratique (0,0)-(50,100)-(100,0) élevée en cubique.
                assert!((c1.x - 100.0 / 3.0).abs() < 1e-9 && (c1.y - 200.0 / 3.0).abs() < 1e-9);
                assert!((c2.x - 200.0 / 3.0).abs() < 1e-9 && (c2.y - 200.0 / 3.0).abs() < 1e-9);
                assert_eq!(p, Point::new(100.0, 0.0));
            }
            ref other => panic!("courbe attendue, obtenu {other:?}"),
        }
        assert_eq!(cmds[2], PathCommand::LineTo(Point::new(0.0, 0.0)));
        assert_eq!(cmds[3], PathCommand::Close);
    }

    #[test]
    fn implicit_midpoints_between_off_curve_points() {
        let mut path = Path::new();
        let contour = vec![
            OutlinePoint {
                x: 0.0,
                y: 0.0,
                on_curve: false,
            },
            OutlinePoint {
                x: 100.0,
                y: 0.0,
                on_curve: false,
            },
            OutlinePoint {
                x: 100.0,
                y: 100.0,
                on_curve: false,
            },
            OutlinePoint {
                x: 0.0,
                y: 100.0,
                on_curve: false,
            },
        ];
        contour_to_path(&contour, &mut path);
        // Départ au milieu du dernier et du premier point, quatre courbes.
        assert_eq!(
            path.commands()[0],
            PathCommand::MoveTo(Point::new(0.0, 50.0))
        );
        let curves = path
            .commands()
            .iter()
            .filter(|c| matches!(c, PathCommand::CurveTo(..)))
            .count();
        assert_eq!(curves, 4);
        assert_eq!(path.commands().last(), Some(&PathCommand::Close));
    }

    #[test]
    fn composite_glyph_offsets_and_scale() {
        let font = TrueTypeFont::parse(&sample_font()).unwrap();
        let path = font.glyph_path(2).unwrap();
        let cmds = path.commands();
        assert_eq!(cmds.len(), 12);
        assert_eq!(cmds[0], PathCommand::MoveTo(Point::new(200.0, 0.0)));
        assert_eq!(cmds[2], PathCommand::LineTo(Point::new(300.0, 100.0)));
        assert_eq!(cmds[6], PathCommand::MoveTo(Point::new(10.0, 20.0)));
        assert_eq!(cmds[7], PathCommand::LineTo(Point::new(60.0, 20.0)));
        assert_eq!(cmds[8], PathCommand::LineTo(Point::new(60.0, 70.0)));
    }

    #[test]
    fn composite_glyph_point_matching_and_two_by_two() {
        let font = TrueTypeFont::parse(&sample_font()).unwrap();
        let path = font.glyph_path(5).unwrap();
        let cmds = path.commands();
        assert_eq!(cmds.len(), 12);
        // Le second carré, tourné, a son point 0 sur (100, 100).
        assert_eq!(cmds[6], PathCommand::MoveTo(Point::new(100.0, 100.0)));
        // (100,0) → rotation [0 1; -1 0] : x' = 0·100 + (-1)·0 = 0, y' = 1·100 = 100,
        // puis décalage (100,100) : (100, 200).
        assert_eq!(cmds[7], PathCommand::LineTo(Point::new(100.0, 200.0)));
    }

    #[test]
    fn cmap_formats() {
        let font = TrueTypeFont::parse(&sample_font()).unwrap();
        assert!(font.has_cmap(3, 1));
        assert!(!font.has_cmap(3, 2));
        assert_eq!(font.cmap_subtables().len(), 5);
        assert_eq!(font.cmap_lookup(3, 1, 0x41), Some(1));
        assert_eq!(font.cmap_lookup(3, 1, 0x42), Some(2));
        assert_eq!(font.cmap_lookup(3, 1, 0x43), None);
        assert_eq!(font.cmap_lookup(3, 10, 0x1F601), Some(4));
        assert_eq!(font.cmap_lookup(1, 0, 0x61), Some(4));
        assert_eq!(font.cmap_lookup(1, 0, 0x62), None);
        assert_eq!(font.cmap_lookup(0, 3, 0x100), Some(5));
        assert_eq!(font.cmap_lookup(0, 3, 0x101), None);
        assert_eq!(font.cmap_lookup(0, 3, 0x102), Some(1));
        assert_eq!(font.cmap_lookup(3, 0, 0xF041), Some(3));
        assert_eq!(font.cmap_lookup(3, 0, 0xF042), None);
        assert_eq!(font.cmap_lookup(3, 0, 0xF043), Some(4));
        assert_eq!(font.unicode_to_gid('A'), Some(1));
        assert_eq!(font.unicode_to_gid('\u{1F600}'), Some(3));
        assert_eq!(font.unicode_to_gid('\u{100}'), Some(5));
        assert_eq!(font.unicode_to_gid('Z'), None);
    }

    #[test]
    fn post_names() {
        let font = TrueTypeFont::parse(&sample_font()).unwrap();
        assert_eq!(font.glyph_name(1), Some("A"));
        assert_eq!(font.glyph_name(3), Some("curve"));
        assert_eq!(font.gid_by_name("B"), Some(2));
        assert_eq!(font.gid_by_name("matched"), Some(5));
        assert_eq!(font.gid_by_name("zzz"), None);
    }

    #[test]
    fn glyph_provider_trait() {
        let font = TrueTypeFont::parse(&sample_font()).unwrap();
        let p: &dyn GlyphProvider = &font;
        assert_eq!(p.glyph_count(), 6);
        assert_eq!(p.units_per_em(), 1000.0);
        assert_eq!(p.advance(1), Some(600.0));
        assert_eq!(p.glyph_path(1).unwrap().commands().len(), 6);
        assert!(p.glyph_path(70000).is_none());
    }

    #[test]
    fn collection_and_signatures() {
        let single = sample_font();
        let mut ttc = Vec::new();
        ttc.extend_from_slice(b"ttcf");
        ttc.extend_from_slice(&[0, 1, 0, 0]);
        ttc.extend_from_slice(&2u32.to_be_bytes());
        let base = 12 + 8;
        ttc.extend_from_slice(&(base as u32).to_be_bytes());
        ttc.extend_from_slice(&(base as u32).to_be_bytes());
        // Le répertoire d'une police TTC référence les tables en absolu :
        // on recale les offsets du répertoire de `single`.
        let mut moved = single.clone();
        let n = u16::from_be_bytes([single[4], single[5]]) as usize;
        for i in 0..n {
            let p = 12 + 16 * i + 8;
            let off = u32::from_be_bytes([single[p], single[p + 1], single[p + 2], single[p + 3]]);
            moved[p..p + 4].copy_from_slice(&(off + base as u32).to_be_bytes());
        }
        ttc.extend_from_slice(&moved);
        assert_eq!(TrueTypeFont::collection_count(&ttc), 2);
        let font = TrueTypeFont::parse(&ttc).unwrap();
        assert_eq!(font.glyph_count(), 6);
        assert!(TrueTypeFont::parse_collection_index(&ttc, 1).is_ok());
        assert!(TrueTypeFont::parse_collection_index(&ttc, 2).is_err());
        assert!(TrueTypeFont::parse(b"XXXX\0\0").is_err());
        assert!(TrueTypeFont::parse(b"").is_err());
        // 'true' accepté.
        let mut apple = single.clone();
        apple[..4].copy_from_slice(b"true");
        assert!(TrueTypeFont::parse(&apple).is_ok());
    }

    #[test]
    fn missing_tables_are_tolerated() {
        let (loca, glyf) = loca_and_glyf(&[Vec::new(), simple_glyph(&[&[(0, 0, true)]])]);
        let data = Builder::new()
            .table(*b"maxp", maxp_table(2))
            .table(*b"loca", loca)
            .table(*b"glyf", glyf)
            .build(*b"\0\x01\0\0");
        let font = TrueTypeFont::parse(&data).unwrap();
        assert_eq!(font.units_per_em(), 1000);
        assert_eq!(font.advance(0), None);
        assert!(!font.has_cmap(3, 1));
        assert_eq!(font.unicode_to_gid('A'), None);
        assert_eq!(font.glyph_name(0), None);
        assert!(font.glyph_path(1).is_some());
        // Sans glyf ni CFF : erreur.
        let data = Builder::new()
            .table(*b"maxp", maxp_table(2))
            .build(*b"\0\x01\0\0");
        assert!(TrueTypeFont::parse(&data).is_err());
    }

    #[test]
    fn truncated_loca_gives_empty_glyphs() {
        let (mut loca, glyf) = loca_and_glyf(&[Vec::new(), simple_glyph(&[&[(0, 0, true)]])]);
        loca.truncate(4);
        let data = Builder::new()
            .table(*b"head", head_table(2048, true))
            .table(*b"maxp", maxp_table(2))
            .table(*b"loca", loca)
            .table(*b"glyf", glyf)
            .build(*b"\0\x01\0\0");
        let font = TrueTypeFont::parse(&data).unwrap();
        assert_eq!(font.units_per_em(), 2048);
        assert!(font.glyph_path(1).unwrap().is_empty());
    }

    #[test]
    fn self_referencing_composite_terminates() {
        let mut composite = Vec::new();
        composite.extend_from_slice(&be16(-1));
        composite.extend_from_slice(&[0; 8]);
        composite.extend_from_slice(&(ARGS_ARE_XY_VALUES | MORE_COMPONENTS).to_be_bytes());
        composite.extend_from_slice(&1u16.to_be_bytes());
        composite.extend_from_slice(&[1, 1]);
        composite.extend_from_slice(&ARGS_ARE_XY_VALUES.to_be_bytes());
        composite.extend_from_slice(&1u16.to_be_bytes());
        composite.extend_from_slice(&[1, 1]);
        let (loca, glyf) = loca_and_glyf(&[Vec::new(), composite]);
        let data = Builder::new()
            .table(*b"maxp", maxp_table(2))
            .table(*b"loca", loca)
            .table(*b"glyf", glyf)
            .build(*b"\0\x01\0\0");
        let font = TrueTypeFont::parse(&data).unwrap();
        assert!(font.glyph_path(1).is_some());
    }

    #[test]
    fn truncation_never_panics() {
        let data = sample_font();
        for n in 0..data.len() {
            if let Ok(font) = TrueTypeFont::parse(&data[..n]) {
                for gid in 0..=font.glyph_count() {
                    let _ = font.glyph_path(gid);
                    let _ = font.advance(gid);
                    let _ = font.glyph_name(gid);
                }
                let _ = font.unicode_to_gid('A');
                let _ = font.cmap_lookup(3, 0, 0xF041);
                let _ = font.gid_by_name("A");
            }
        }
    }

    #[test]
    fn random_bytes_never_panic() {
        // Générateur congruentiel simple, déterministe.
        let mut seed: u32 = 0x1234_5678;
        for round in 0..200 {
            let len = (round * 7) % 300;
            let mut data = vec![0u8; len];
            for b in &mut data {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *b = (seed >> 24) as u8;
            }
            if round % 3 == 0 && data.len() >= 4 {
                data[..4].copy_from_slice(&[0, 1, 0, 0]);
            }
            if let Ok(font) = TrueTypeFont::parse(&data) {
                for gid in 0..font.glyph_count().min(64) {
                    let _ = font.glyph_path(gid);
                }
            }
        }
    }
}
