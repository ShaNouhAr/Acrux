//! Sous-ensemble de police TrueType : produit un fichier `.ttf` valide ne
//! contenant qu'une liste de glyphes choisie (OpenType, tables `head`, `hhea`,
//! `maxp`, `hmtx`, `loca`, `glyf`, `cmap`, `name`, `post`).
//!
//! Sert à l'édition de texte (`acrux_features::edit_text`) : quand un caractère
//! manque au sous-ensemble incorporé dans un PDF, on reconstruit une police
//! qui contient les glyphes d'origine **plus** les glyphes manquants pris dans
//! la police système correspondante.
//!
//! Principes :
//!
//! - Les descriptions `glyf` sont **recopiées octet pour octet** depuis la
//!   police source ; seuls les indices de glyphes des composantes (glyphes
//!   composites, §« Composite Glyph Description ») sont renumérotés. Les
//!   contours produits sont donc identiques à ceux de la source.
//! - Le glyphe produit d'indice `i` est `glyphs[i]` : l'appelant maîtrise la
//!   numérotation (indispensable pour un PDF `Identity-H` où le code d'un
//!   caractère **est** son indice de glyphe). L'appelant doit placer
//!   `.notdef` en position 0. Les composantes manquantes sont ajoutées à la
//!   suite de la liste.
//! - `loca` est toujours écrite au format long (`indexToLocFormat` = 1) et
//!   `hmtx` contient une entrée complète par glyphe (`numberOfHMetrics` =
//!   nombre de glyphes) : plus simple et toujours valide.
//! - `cmap` est reconstruite au format 4 à partir des correspondances
//!   Unicode → glyphe des polices sources (BMP uniquement).
//! - `post` est écrite en version 3.0 (sans noms de glyphes), comme le font
//!   les sous-ensembles produits par les imprimantes PDF courantes.
//!
//! Les polices `OTTO` (contours CFF) ne sont pas concernées : elles n'ont pas
//! de table `glyf`. [`subset`] renvoie alors `Error::Unsupported`.

use std::collections::BTreeMap;

use acrux_core::{Error, Result};

use crate::reader::{i16_at, u16_at, Reader};
use crate::truetype::TrueTypeFont;

/// Profondeur maximale de résolution des glyphes composites.
const MAX_DEPTH: u8 = 8;
/// Nombre maximal de glyphes d'un sous-ensemble (limite du format).
const MAX_GLYPHS: usize = 65_535;

/// Drapeaux d'une composante de glyphe composite.
const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
const WE_HAVE_A_SCALE: u16 = 0x0008;
const MORE_COMPONENTS: u16 = 0x0020;
const WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;

/// Police produite.
#[derive(Debug, Clone)]
pub struct Subset {
    /// Fichier TrueType complet.
    pub data: Vec<u8>,
    /// Correspondance (indice de police source, glyphe source) → glyphe produit.
    pub map: BTreeMap<(usize, u16), u16>,
    /// Nombre de glyphes de la police produite.
    pub glyph_count: u16,
}

impl Subset {
    /// Indice, dans la police produite, d'un glyphe de la police source `font`.
    #[must_use]
    pub fn new_gid(&self, font: usize, gid: u16) -> Option<u16> {
        self.map.get(&(font, gid)).copied()
    }
}

/// Sous-ensemble d'une seule police : le glyphe produit `i` est `glyphs[i]`.
///
/// `name` devient le nom PostScript et le nom de famille de la police
/// produite (par exemple `ABCDEF+Georgia`).
///
/// # Errors
/// Police sans table `glyf`, liste vide ou trop longue, tables obligatoires
/// illisibles.
pub fn subset(font: &TrueTypeFont, glyphs: &[u16], name: &str) -> Result<Subset> {
    let list: Vec<(usize, u16)> = glyphs.iter().map(|g| (0, *g)).collect();
    subset_merged(&[font], &list, name)
}

/// Sous-ensemble fusionnant plusieurs polices : chaque entrée de `glyphs`
/// désigne une police (indice dans `fonts`) et un glyphe de cette police.
///
/// Toutes les polices doivent avoir le même nombre d'unités par em, sans quoi
/// les contours ne seraient pas à la même échelle.
///
/// # Errors
/// Polices d'échelles différentes, police sans `glyf`, liste vide ou trop
/// longue, tables obligatoires illisibles.
// Une étape par table produite : les découper masquerait l'ordre de
// construction (liste des glyphes, glyf, loca, hmtx, puis les tables recopiées).
#[allow(clippy::too_many_lines)]
pub fn subset_merged(
    fonts: &[&TrueTypeFont],
    glyphs: &[(usize, u16)],
    name: &str,
) -> Result<Subset> {
    let Some(first) = fonts.first() else {
        return Err(Error::Corrupt("sous-ensemble : aucune police".into()));
    };
    if glyphs.is_empty() || glyphs.len() > MAX_GLYPHS {
        return Err(Error::Corrupt(format!(
            "sous-ensemble : {} glyphes demandés (1 à {MAX_GLYPHS})",
            glyphs.len()
        )));
    }
    for f in fonts {
        if !f.has_glyf() {
            return Err(Error::Unsupported(
                "sous-ensemble : police sans table glyf (contours CFF)".into(),
            ));
        }
        if f.units_per_em() != first.units_per_em() {
            return Err(Error::Corrupt(
                "sous-ensemble : les polices fusionnées n'ont pas les mêmes unités par em".into(),
            ));
        }
    }
    // 1. Liste finale : les glyphes demandés, puis les composantes manquantes.
    let mut order: Vec<(usize, u16)> = Vec::with_capacity(glyphs.len());
    let mut map: BTreeMap<(usize, u16), u16> = BTreeMap::new();
    for key in glyphs {
        let new = u16::try_from(order.len()).unwrap_or(u16::MAX);
        order.push(*key);
        // Un glyphe demandé deux fois garde sa première position.
        map.entry(*key).or_insert(new);
    }
    let mut index = 0;
    let mut depth_of: Vec<u8> = vec![0; order.len()];
    while index < order.len() {
        let (fi, gid) = order[index];
        let depth = depth_of[index];
        index += 1;
        if depth >= MAX_DEPTH {
            continue;
        }
        let Some(font) = fonts.get(fi) else { continue };
        let Some(data) = font.glyph_data(gid) else {
            continue;
        };
        for component in components(data) {
            let key = (fi, component);
            if map.contains_key(&key) || order.len() >= MAX_GLYPHS {
                continue;
            }
            map.insert(key, u16::try_from(order.len()).unwrap_or(u16::MAX));
            order.push(key);
            depth_of.push(depth + 1);
        }
    }
    let count = u16::try_from(order.len()).unwrap_or(u16::MAX);

    // 2. glyf et loca.
    let mut glyf = Vec::new();
    let mut loca: Vec<u32> = Vec::with_capacity(order.len() + 1);
    for (fi, gid) in &order {
        loca.push(u32::try_from(glyf.len()).unwrap_or(u32::MAX));
        let Some(font) = fonts.get(*fi) else { continue };
        let Some(data) = font.glyph_data(*gid) else {
            continue; // glyphe vide : longueur nulle, c'est légal
        };
        let mut record = data.to_vec();
        renumber_components(&mut record, *fi, &map);
        glyf.extend_from_slice(&record);
        while glyf.len() % 4 != 0 {
            glyf.push(0);
        }
    }
    loca.push(u32::try_from(glyf.len()).unwrap_or(u32::MAX));
    let mut loca_bytes = Vec::with_capacity(loca.len() * 4);
    for o in &loca {
        loca_bytes.extend_from_slice(&o.to_be_bytes());
    }

    // 3. hmtx : une entrée complète par glyphe.
    let mut hmtx = Vec::with_capacity(order.len() * 4);
    for (fi, gid) in &order {
        let (advance, lsb) = fonts
            .get(*fi)
            .and_then(|f| f.metrics(*gid))
            .unwrap_or((first.units_per_em() / 2, 0));
        hmtx.extend_from_slice(&advance.to_be_bytes());
        hmtx.extend_from_slice(&lsb.to_be_bytes());
    }

    // 4. Tables recopiées et corrigées.
    let head = build_head(first)?;
    let hhea = build_hhea(first, count)?;
    let maxp_table = build_maxp(first, count);
    let post = build_post(first);
    let cmap = build_cmap(fonts, &order);
    let name_table = build_name(name);

    let tables: Vec<([u8; 4], Vec<u8>)> = vec![
        (*b"cmap", cmap),
        (*b"glyf", glyf),
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"hmtx", hmtx),
        (*b"loca", loca_bytes),
        (*b"maxp", maxp_table),
        (*b"name", name_table),
        (*b"post", post),
    ];
    Ok(Subset {
        data: assemble(&tables),
        map,
        glyph_count: count,
    })
}

/// Indices de glyphes des composantes d'un glyphe composite (vide sinon).
fn components(data: &[u8]) -> Vec<u16> {
    let mut out = Vec::new();
    let Some(contours) = i16_at(data, 0) else {
        return out;
    };
    if contours >= 0 {
        return out;
    }
    let mut pos = 10;
    loop {
        let (Some(flags), Some(gid)) = (u16_at(data, pos), u16_at(data, pos + 2)) else {
            return out;
        };
        out.push(gid);
        pos += 4;
        pos += if flags & ARG_1_AND_2_ARE_WORDS != 0 {
            4
        } else {
            2
        };
        if flags & WE_HAVE_A_SCALE != 0 {
            pos += 2;
        } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
            pos += 4;
        } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
            pos += 8;
        }
        if flags & MORE_COMPONENTS == 0 || out.len() > 64 {
            return out;
        }
    }
}

/// Renumérote les composantes d'un glyphe composite selon `map`.
fn renumber_components(data: &mut [u8], font: usize, map: &BTreeMap<(usize, u16), u16>) {
    let Some(contours) = i16_at(data, 0) else {
        return;
    };
    if contours >= 0 {
        return;
    }
    let mut pos = 10;
    let mut seen = 0;
    loop {
        let (Some(flags), Some(gid)) = (u16_at(data, pos), u16_at(data, pos + 2)) else {
            return;
        };
        let new = map.get(&(font, gid)).copied().unwrap_or(0);
        if let Some(slot) = data.get_mut(pos + 2..pos + 4) {
            slot.copy_from_slice(&new.to_be_bytes());
        }
        pos += 4;
        pos += if flags & ARG_1_AND_2_ARE_WORDS != 0 {
            4
        } else {
            2
        };
        if flags & WE_HAVE_A_SCALE != 0 {
            pos += 2;
        } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
            pos += 4;
        } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
            pos += 8;
        }
        seen += 1;
        if flags & MORE_COMPONENTS == 0 || seen > 64 {
            return;
        }
    }
}

/// `head` recopiée, `indexToLocFormat` forcée au format long et
/// `checkSumAdjustment` remise à zéro (recalculée par [`assemble`]).
fn build_head(font: &TrueTypeFont) -> Result<Vec<u8>> {
    let mut head = font
        .table_bytes(*b"head")
        .map(<[u8]>::to_vec)
        .ok_or_else(|| Error::Corrupt("sous-ensemble : table head absente".into()))?;
    head.resize(54, 0);
    head[8..12].copy_from_slice(&0u32.to_be_bytes()); // checkSumAdjustment
    head[50..52].copy_from_slice(&1i16.to_be_bytes()); // indexToLocFormat : long
    Ok(head)
}

/// `hhea` recopiée avec `numberOfHMetrics` = nombre de glyphes.
fn build_hhea(font: &TrueTypeFont, count: u16) -> Result<Vec<u8>> {
    let mut hhea = font
        .table_bytes(*b"hhea")
        .map(<[u8]>::to_vec)
        .unwrap_or_default();
    if hhea.len() < 36 {
        let h = font
            .hhea()
            .ok_or_else(|| Error::Corrupt("sous-ensemble : table hhea absente".into()))?;
        hhea = vec![0; 36];
        hhea[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        hhea[4..6].copy_from_slice(&h.ascender.to_be_bytes());
        hhea[6..8].copy_from_slice(&h.descender.to_be_bytes());
        hhea[8..10].copy_from_slice(&h.line_gap.to_be_bytes());
    }
    hhea.truncate(36);
    hhea[34..36].copy_from_slice(&count.to_be_bytes());
    Ok(hhea)
}

/// `maxp` recopiée avec `numGlyphs` corrigé (les autres maxima de la source
/// restent des majorants valides).
fn build_maxp(font: &TrueTypeFont, count: u16) -> Vec<u8> {
    let mut maxp = font
        .table_bytes(*b"maxp")
        .map(<[u8]>::to_vec)
        .unwrap_or_default();
    if maxp.len() < 32 {
        maxp = vec![0; 32];
        maxp[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        // Majorants prudents pour un glyphe quelconque.
        maxp[6..8].copy_from_slice(&u16::MAX.to_be_bytes()); // maxPoints
        maxp[8..10].copy_from_slice(&u16::MAX.to_be_bytes()); // maxContours
        maxp[10..12].copy_from_slice(&u16::MAX.to_be_bytes()); // maxCompositePoints
        maxp[12..14].copy_from_slice(&u16::MAX.to_be_bytes()); // maxCompositeContours
        maxp[26..28].copy_from_slice(&u16::from(MAX_DEPTH).to_be_bytes()); // maxComponentDepth
    }
    maxp.truncate(32);
    maxp[4..6].copy_from_slice(&count.to_be_bytes());
    maxp
}

/// `post` version 3.0 : en-tête seul, sans noms de glyphes.
fn build_post(font: &TrueTypeFont) -> Vec<u8> {
    let src = font.table_bytes(*b"post").unwrap_or(&[]);
    let mut post = vec![0u8; 32];
    post[0..4].copy_from_slice(&0x0003_0000u32.to_be_bytes());
    // italicAngle, underlinePosition, underlineThickness, isFixedPitch.
    if let Some(kept) = src.get(4..src.len().min(32)) {
        post[4..4 + kept.len()].copy_from_slice(kept);
    }
    post
}

/// Table `name` minimale (plateforme Windows, anglais US, UTF-16BE).
fn build_name(name: &str) -> Vec<u8> {
    let family = name.split('+').next_back().unwrap_or(name);
    let records: [(u16, &str); 5] = [
        (1, family),
        (2, "Regular"),
        (3, name),
        (4, family),
        (6, name),
    ];
    let mut storage = Vec::new();
    let mut table = Vec::new();
    table.extend_from_slice(&0u16.to_be_bytes()); // format
    table.extend_from_slice(&u16::try_from(records.len()).unwrap_or(0).to_be_bytes());
    let offset = 6 + records.len() * 12;
    table.extend_from_slice(&u16::try_from(offset).unwrap_or(0).to_be_bytes());
    for (id, value) in records {
        let encoded: Vec<u8> = value
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .take(2048)
            .collect();
        table.extend_from_slice(&3u16.to_be_bytes()); // platformID : Windows
        table.extend_from_slice(&1u16.to_be_bytes()); // encodingID : Unicode BMP
        table.extend_from_slice(&0x0409u16.to_be_bytes()); // languageID : en-US
        table.extend_from_slice(&id.to_be_bytes());
        table.extend_from_slice(&u16::try_from(encoded.len()).unwrap_or(0).to_be_bytes());
        table.extend_from_slice(&u16::try_from(storage.len()).unwrap_or(0).to_be_bytes());
        storage.extend_from_slice(&encoded);
    }
    table.extend_from_slice(&storage);
    table
}

/// `cmap` format 4 construite en inversant les `cmap` des polices sources.
fn build_cmap(fonts: &[&TrueTypeFont], order: &[(usize, u16)]) -> Vec<u8> {
    // Glyphe source → nouvel indice, par police.
    let mut wanted: BTreeMap<(usize, u16), u16> = BTreeMap::new();
    for (new, key) in order.iter().enumerate() {
        wanted
            .entry(*key)
            .or_insert_with(|| u16::try_from(new).unwrap_or(0));
    }
    let mut pairs: BTreeMap<u16, u16> = BTreeMap::new(); // code BMP → nouvel indice
    for (fi, font) in fonts.iter().enumerate() {
        for code in 1..=0xFFFEu32 {
            let Some(c) = char::from_u32(code) else {
                continue;
            };
            let Some(gid) = font.unicode_to_gid(c) else {
                continue;
            };
            if let Some(new) = wanted.get(&(fi, gid)) {
                #[allow(clippy::cast_possible_truncation)] // code ≤ 0xFFFE
                pairs.entry(code as u16).or_insert(*new);
            }
        }
    }
    let mut format4 = build_format4(&pairs);
    // En-tête cmap : une sous-table (3, 1).
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&1u16.to_be_bytes()); // numTables
    out.extend_from_slice(&3u16.to_be_bytes()); // platformID
    out.extend_from_slice(&1u16.to_be_bytes()); // encodingID
    out.extend_from_slice(&12u32.to_be_bytes()); // offset
    out.append(&mut format4);
    out
}

/// Sous-table `cmap` format 4 (segments delta, BMP).
fn build_format4(pairs: &BTreeMap<u16, u16>) -> Vec<u8> {
    // Segments de codes consécutifs ; les glyphes sont toujours donnés par
    // `glyphIdArray` (idRangeOffset), ce qui évite l'arithmétique delta.
    let mut segments: Vec<(u16, u16)> = Vec::new(); // (début, fin)
    for &code in pairs.keys() {
        match segments.last_mut() {
            Some((_, end)) if *end + 1 == code => *end = code,
            _ => segments.push((code, code)),
        }
    }
    segments.push((0xFFFF, 0xFFFF));
    let seg_count = u16::try_from(segments.len()).unwrap_or(1);
    let mut end_codes = Vec::new();
    let mut start_codes = Vec::new();
    let mut deltas = Vec::new();
    let mut range_offsets = Vec::new();
    let mut glyph_array: Vec<u16> = Vec::new();
    for (i, (start, end)) in segments.iter().enumerate() {
        end_codes.extend_from_slice(&end.to_be_bytes());
        start_codes.extend_from_slice(&start.to_be_bytes());
        if *start == 0xFFFF {
            // Segment terminal obligatoire : 0xFFFF → glyphe 0.
            deltas.extend_from_slice(&1u16.to_be_bytes());
            range_offsets.extend_from_slice(&0u16.to_be_bytes());
            continue;
        }
        deltas.extend_from_slice(&0u16.to_be_bytes());
        // idRangeOffset = distance en octets depuis sa propre case jusqu'au
        // premier glyphe du segment dans glyphIdArray.
        let remaining = usize::from(seg_count) - i;
        let offset = (remaining + glyph_array.len()) * 2;
        range_offsets.extend_from_slice(&u16::try_from(offset).unwrap_or(0).to_be_bytes());
        for code in *start..=*end {
            glyph_array.push(pairs.get(&code).copied().unwrap_or(0));
        }
    }
    let length = 16 + usize::from(seg_count) * 8 + glyph_array.len() * 2;
    let mut out = Vec::with_capacity(length);
    out.extend_from_slice(&4u16.to_be_bytes()); // format
    out.extend_from_slice(&u16::try_from(length).unwrap_or(0).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // language
    out.extend_from_slice(&(seg_count * 2).to_be_bytes());
    let search = search_params(seg_count);
    out.extend_from_slice(&search.0.to_be_bytes()); // searchRange
    out.extend_from_slice(&search.1.to_be_bytes()); // entrySelector
    out.extend_from_slice(&search.2.to_be_bytes()); // rangeShift
    out.extend_from_slice(&end_codes);
    out.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
    out.extend_from_slice(&start_codes);
    out.extend_from_slice(&deltas);
    out.extend_from_slice(&range_offsets);
    for g in &glyph_array {
        out.extend_from_slice(&g.to_be_bytes());
    }
    out
}

/// `searchRange`, `entrySelector`, `rangeShift` pour `n` entrées de 2 octets.
fn search_params(n: u16) -> (u16, u16, u16) {
    let mut selector = 0u16;
    let mut power = 1u16;
    while power * 2 <= n && power <= 0x3FFF {
        power *= 2;
        selector += 1;
    }
    (power * 2, selector, n * 2 - power * 2)
}

/// Assemble l'en-tête sfnt, le répertoire et les tables (alignées sur 4
/// octets), puis corrige `checkSumAdjustment` dans `head`.
fn assemble(tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let n = u16::try_from(tables.len()).unwrap_or(0);
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // sfntVersion
    out.extend_from_slice(&n.to_be_bytes());
    let search = search_params_16(n);
    out.extend_from_slice(&search.0.to_be_bytes());
    out.extend_from_slice(&search.1.to_be_bytes());
    out.extend_from_slice(&search.2.to_be_bytes());
    let mut offset = 12 + tables.len() * 16;
    let mut head_offset = 0;
    for (tag, data) in tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum(data).to_be_bytes());
        out.extend_from_slice(&u32::try_from(offset).unwrap_or(0).to_be_bytes());
        out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(0).to_be_bytes());
        if tag == b"head" {
            head_offset = offset;
        }
        offset += data.len().div_ceil(4) * 4;
    }
    for (_, data) in tables {
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    // §« head » : checkSumAdjustment = 0xB1B0AFBA − somme de contrôle du fichier.
    if head_offset + 12 <= out.len() {
        let total = checksum(&out);
        let adjust = 0xB1B0_AFBAu32.wrapping_sub(total);
        out[head_offset + 8..head_offset + 12].copy_from_slice(&adjust.to_be_bytes());
    }
    out
}

/// `searchRange`, `entrySelector`, `rangeShift` du répertoire (entrées de 16 octets).
fn search_params_16(n: u16) -> (u16, u16, u16) {
    let mut selector = 0u16;
    let mut power = 1u16;
    while power * 2 <= n && power <= 0x03FF {
        power *= 2;
        selector += 1;
    }
    (power * 16, selector, n * 16 - power * 16)
}

/// Somme de contrôle OpenType : somme des mots de 32 bits, modulo 2³².
fn checksum(data: &[u8]) -> u32 {
    let mut sum = 0u32;
    let mut i = 0;
    while i < data.len() {
        let mut word = [0u8; 4];
        let end = (i + 4).min(data.len());
        word[..end - i].copy_from_slice(&data[i..end]);
        sum = sum.wrapping_add(u32::from_be_bytes(word));
        i += 4;
    }
    sum
}

/// Nombre de glyphes déclaré par une police (`maxp`), sans l'analyser.
#[must_use]
pub fn glyph_count_of(data: &[u8]) -> Option<u16> {
    let mut r = Reader::new(data);
    r.read_tag()?;
    let num_tables = r.read_u16()?;
    r.skip(6)?;
    for _ in 0..num_tables {
        let (tag, _, off, _) = (r.read_tag()?, r.read_u32()?, r.read_u32()?, r.read_u32()?);
        if &tag == b"maxp" {
            return u16_at(data, usize::try_from(off).ok()? + 4);
        }
    }
    None
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp
)]
mod tests {
    use super::*;

    /// Contours d'un glyphe, arrondis, pour comparer deux polices.
    fn outline(font: &TrueTypeFont, gid: u16) -> Vec<String> {
        font.glyph_path(gid).map_or_else(Vec::new, |p| {
            p.commands()
                .iter()
                .map(|c| format!("{c:?}"))
                .collect::<Vec<_>>()
        })
    }

    #[test]
    fn subset_keeps_outlines_metrics_and_cmap() {
        let source = TrueTypeFont::parse(&crate::truetype::tests::sample_font()).unwrap();
        let wanted: Vec<u16> = (0..source.glyph_count()).collect();
        let sub = subset(&source, &wanted, "ABCDEF+Essai").unwrap();
        let produced = TrueTypeFont::parse(&sub.data).unwrap();
        assert_eq!(produced.glyph_count(), source.glyph_count());
        assert_eq!(produced.units_per_em(), source.units_per_em());
        for gid in 0..source.glyph_count() {
            let new = sub.new_gid(0, gid).unwrap();
            assert_eq!(
                outline(&produced, new),
                outline(&source, gid),
                "contours du glyphe {gid}"
            );
            assert_eq!(produced.advance(new), source.advance(gid), "avance {gid}");
        }
        // La cmap produite retrouve les mêmes caractères.
        for c in ['A', 'B'] {
            if let Some(gid) = source.unicode_to_gid(c) {
                assert_eq!(produced.unicode_to_gid(c), sub.new_gid(0, gid));
            }
        }
    }

    #[test]
    fn subset_renumbers_composite_components() {
        let source = TrueTypeFont::parse(&crate::truetype::tests::sample_font()).unwrap();
        // Le glyphe composite de la police d'essai renvoie vers un glyphe simple.
        let composite = (0..source.glyph_count())
            .find(|g| !components(source.glyph_data(*g).unwrap_or(&[])).is_empty())
            .expect("la police d'essai contient un glyphe composite");
        let sub = subset(&source, &[0, composite], "ABCDEF+Comp").unwrap();
        let produced = TrueTypeFont::parse(&sub.data).unwrap();
        // La composante a été ajoutée à la suite : 3 glyphes au lieu de 2.
        assert!(produced.glyph_count() >= 3, "{}", produced.glyph_count());
        assert_eq!(outline(&produced, 1), outline(&source, composite));
    }

    #[test]
    fn subset_merges_two_fonts() {
        let a = TrueTypeFont::parse(&crate::truetype::tests::sample_font()).unwrap();
        let b = TrueTypeFont::parse(&crate::truetype::tests::sample_font()).unwrap();
        let sub = subset_merged(&[&a, &b], &[(0, 0), (0, 1), (1, 2)], "ABCDEF+Fusion").unwrap();
        let produced = TrueTypeFont::parse(&sub.data).unwrap();
        assert_eq!(sub.new_gid(0, 1), Some(1));
        assert_eq!(sub.new_gid(1, 2), Some(2));
        assert_eq!(outline(&produced, 1), outline(&a, 1));
        assert_eq!(outline(&produced, 2), outline(&b, 2));
        assert_eq!(glyph_count_of(&sub.data), Some(produced.glyph_count()));
    }

    /// Sur une vraie police système (des milliers de glyphes, des composites
    /// accentués) : le sous-ensemble doit rendre exactement les mêmes contours.
    /// Ignoré si la machine n'a pas la police.
    #[test]
    fn subset_of_a_system_font_keeps_every_outline() {
        let dirs = [
            std::env::var("ACRUX_FONT_DIR").unwrap_or_default(),
            "C:/Windows/Fonts".into(),
            "/usr/share/fonts/truetype/dejavu".into(),
        ];
        let names = ["georgia.ttf", "arial.ttf", "DejaVuSans.ttf"];
        let mut bytes = None;
        for dir in &dirs {
            for name in &names {
                if let Ok(data) = std::fs::read(std::path::Path::new(dir).join(name)) {
                    bytes = Some(data);
                    break;
                }
            }
        }
        let Some(bytes) = bytes else {
            return; // machine sans police système : rien à vérifier
        };
        let Ok(source) = TrueTypeFont::parse(&bytes) else {
            return;
        };
        if !source.has_glyf() {
            return;
        }
        // Un échantillon de caractères courants, accentués et composites.
        let mut wanted = vec![0u16];
        for c in "AZaz0é9àÉîçøß€@%Ωµ†— ".chars() {
            if let Some(gid) = source.unicode_to_gid(c) {
                if !wanted.contains(&gid) {
                    wanted.push(gid);
                }
            }
        }
        assert!(wanted.len() > 5, "police système sans cmap exploitable");
        let sub = subset(&source, &wanted, "ABCDEF+Essai").unwrap();
        let produced = TrueTypeFont::parse(&sub.data).unwrap();
        assert_eq!(produced.units_per_em(), source.units_per_em());
        for gid in &wanted {
            let new = sub.new_gid(0, *gid).unwrap();
            assert_eq!(
                outline(&produced, new),
                outline(&source, *gid),
                "contours du glyphe {gid}"
            );
            assert_eq!(produced.advance(new), source.advance(*gid));
        }
        // Les composantes des glyphes composites ont suivi.
        assert!(produced.glyph_count() >= u16::try_from(wanted.len()).unwrap_or(0));
        for c in "éàÉîç".chars() {
            if let Some(gid) = source.unicode_to_gid(c) {
                assert_eq!(
                    produced.unicode_to_gid(c),
                    sub.new_gid(0, gid),
                    "caractère {c} dans la cmap produite"
                );
            }
        }
    }

    #[test]
    fn subset_rejects_cff_and_empty_lists() {
        let a = TrueTypeFont::parse(&crate::truetype::tests::sample_font()).unwrap();
        assert!(subset(&a, &[], "X").is_err());
        let otto = crate::truetype::tests::Builder::new()
            .table(*b"CFF ", crate::cff::tests::sample_font())
            .build(*b"OTTO");
        let otto = TrueTypeFont::parse(&otto).unwrap();
        assert!(subset(&otto, &[0], "X").is_err());
    }
}
