//! Table `GPOS` : positionnement de glyphes (OpenType Layout).
//!
//! C'est `GPOS` qui rapproche le `V` du `A` (crénage), qui pose l'accent
//! exactement au-dessus de la lettre, qui relie deux lettres cursives.
//!
//! ## Types de sous-tables implémentés
//!
//! | Type | Nom                           | Formats | État |
//! |------|-------------------------------|---------|------|
//! | 1    | Ajustement simple             | 1, 2    | oui  |
//! | 2    | Ajustement de paire (crénage) | 1, 2    | oui  |
//! | 3    | Attachement cursif            | 1       | oui (horizontal) |
//! | 4    | Marque vers base              | 1       | oui  |
//! | 5    | Marque vers ligature          | 1       | oui  |
//! | 6    | Marque vers marque            | 1       | oui  |
//! | 7    | Contextuelle                  | 1, 2, 3 | oui  |
//! | 8    | Contextuelle enchaînée        | 1, 2, 3 | oui  |
//! | 9    | Extension                     | 1       | oui (transparente) |
//!
//! Les tables `Device` (corrections par taille de rendu) et les variations
//! (`ItemVariationStore`) sont lues et ignorées : nous rendons sans hinting.

use crate::opentype::buffer::{AttachKind, GlyphInfo, SkipList};
use crate::opentype::common::{
    read_anchor, read_value_record, value_record_size, Anchor, ClassDef, Coverage, Gdef,
    LayoutTable, Lookup, ValueRecord, LOOKUP_IGNORE_MARKS, LOOKUP_RIGHT_TO_LEFT,
};
use crate::opentype::context::{match_chain_context, match_context, ContextMatch};
use crate::reader::{u16_at, Reader};

/// Profondeur maximale d'imbrication des tables de recherche.
const MAX_NESTING: u8 = 6;

/// Nombre maximal d'éléments lus dans une sous-table.
const MAX_ITEMS: usize = 4096;

/// Type des sous-tables d'extension en `GPOS`.
const EXTENSION_KIND: u16 = 9;

/// Table `GPOS` analysée, tables de recherche résolues.
#[derive(Debug, Clone)]
pub struct Gpos<'a> {
    table: LayoutTable<'a>,
    lookups: Vec<Lookup<'a>>,
}

impl<'a> Gpos<'a> {
    /// Analyse la table.
    #[must_use]
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let table = LayoutTable::parse(data)?;
        let count = table.lookup_count();
        let mut lookups = Vec::with_capacity(usize::from(count));
        for i in 0..count {
            lookups.push(table.lookup(i, EXTENSION_KIND).unwrap_or(Lookup {
                kind: 0,
                flags: 0,
                mark_filtering_set: None,
                subtables: Vec::new(),
            }));
        }
        Some(Self { table, lookups })
    }

    /// Squelette commun (scripts, langues, fonctionnalités).
    #[must_use]
    pub const fn table(&self) -> &LayoutTable<'a> {
        &self.table
    }

    /// Nombre de tables de recherche.
    #[must_use]
    pub fn lookup_count(&self) -> usize {
        self.lookups.len()
    }

    /// Applique une table de recherche à tout le tampon.
    pub fn apply(&self, index: u16, mask: u32, buffer: &mut [GlyphInfo], gdef: Option<&Gdef<'a>>) {
        let Some(lookup) = self.lookups.get(usize::from(index)) else {
            return;
        };
        let mut position = 0usize;
        while position < buffer.len() {
            let applies = buffer
                .get(position)
                .is_some_and(|g| g.mask & mask != 0 && !skipped(lookup, gdef, g.gid));
            if !applies {
                position += 1;
                continue;
            }
            let advance = self.apply_at(lookup, mask, buffer, gdef, position, 0);
            position += advance.max(1);
        }
    }

    /// Applique la première sous-table qui correspond en `position`.
    #[allow(clippy::too_many_arguments)] // la récursion a besoin de tout le contexte
    fn apply_at(
        &self,
        lookup: &Lookup<'a>,
        mask: u32,
        buffer: &mut [GlyphInfo],
        gdef: Option<&Gdef<'a>>,
        position: usize,
        depth: u8,
    ) -> usize {
        for (kind, subtable) in &lookup.subtables {
            let advance = match kind {
                1 => single(subtable, buffer, position),
                2 => pair(subtable, lookup, buffer, gdef, position),
                3 => cursive(subtable, lookup, buffer, gdef, position),
                4 => mark_to_base(subtable, lookup, buffer, gdef, position),
                5 => mark_to_ligature(subtable, lookup, buffer, gdef, position),
                6 => mark_to_mark(subtable, lookup, buffer, gdef, position),
                7 | 8 => {
                    self.contextual(*kind, subtable, lookup, mask, buffer, gdef, position, depth)
                }
                _ => None,
            };
            if let Some(advance) = advance {
                return advance;
            }
        }
        0
    }

    /// Types 7 et 8 : reconnaissance puis application des tables imbriquées.
    #[allow(clippy::too_many_arguments)] // la récursion a besoin de tout le contexte
    fn contextual(
        &self,
        kind: u16,
        subtable: &'a [u8],
        lookup: &Lookup<'a>,
        mask: u32,
        buffer: &mut [GlyphInfo],
        gdef: Option<&Gdef<'a>>,
        position: usize,
        depth: u8,
    ) -> Option<usize> {
        if depth > MAX_NESTING {
            return None;
        }
        let found: ContextMatch = {
            let skip = SkipList::new(buffer, lookup, gdef);
            if kind == 7 {
                match_context(subtable, &skip, position)?
            } else {
                match_chain_context(subtable, &skip, position)?
            }
        };
        for record in &found.records {
            let Some(&target) = found.input.get(usize::from(record.sequence_index)) else {
                continue;
            };
            let Some(nested) = self.lookups.get(usize::from(record.lookup_index)) else {
                continue;
            };
            let applies = buffer
                .get(target)
                .is_some_and(|g| !skipped(nested, gdef, g.gid));
            if applies {
                let _ = self.apply_at(nested, mask, buffer, gdef, target, depth + 1);
            }
        }
        let start = found.input.first().copied().unwrap_or(position);
        let last = found.input.last().copied().unwrap_or(position);
        Some(last.saturating_sub(start) + 1)
    }
}

/// Vrai si la table de recherche saute ce glyphe.
fn skipped(lookup: &Lookup<'_>, gdef: Option<&Gdef<'_>>, gid: u16) -> bool {
    crate::opentype::common::is_skipped(lookup, gdef, gid)
}

/// Cumule un `ValueRecord` sur la position d'un glyphe.
fn apply_value(buffer: &mut [GlyphInfo], position: usize, value: ValueRecord) {
    if let Some(info) = buffer.get_mut(position) {
        info.x_offset += i32::from(value.x_placement);
        info.y_offset += i32::from(value.y_placement);
        info.x_advance += i32::from(value.x_advance);
        info.y_advance += i32::from(value.y_advance);
    }
}

/// Type 1 : ajustement simple.
fn single(subtable: &[u8], buffer: &mut [GlyphInfo], position: usize) -> Option<usize> {
    let gid = buffer.get(position)?.gid;
    let coverage = Coverage::new(subtable.get(usize::from(u16_at(subtable, 2)?)..)?);
    let index = usize::from(coverage.index(gid)?);
    let format = u16_at(subtable, 0)?;
    let value_format = u16_at(subtable, 4)?;
    let value = match format {
        1 => {
            let mut r = Reader::at(subtable, 6);
            read_value_record(&mut r, value_format)
        }
        2 => {
            let count = usize::from(u16_at(subtable, 6)?);
            if index >= count {
                return None;
            }
            let size = value_record_size(value_format);
            let mut r = Reader::at(subtable, 8 + index * size);
            read_value_record(&mut r, value_format)
        }
        _ => return None,
    };
    apply_value(buffer, position, value);
    Some(1)
}

/// Type 2 : ajustement de paire, c'est-à-dire le crénage.
fn pair(
    subtable: &[u8],
    lookup: &Lookup<'_>,
    buffer: &mut [GlyphInfo],
    gdef: Option<&Gdef<'_>>,
    position: usize,
) -> Option<usize> {
    let first = buffer.get(position)?.gid;
    let coverage = Coverage::new(subtable.get(usize::from(u16_at(subtable, 2)?)..)?);
    let index = usize::from(coverage.index(first)?);
    let second_position = {
        let skip = SkipList::new(buffer, lookup, gdef);
        skip.next(position)?
    };
    let second = buffer.get(second_position)?.gid;
    let format1 = u16_at(subtable, 4)?;
    let format2 = u16_at(subtable, 6)?;
    let (value1, value2) = match u16_at(subtable, 0)? {
        1 => pair_format1(subtable, index, second, format1, format2)?,
        2 => pair_format2(subtable, first, second, format1, format2)?,
        _ => return None,
    };
    apply_value(buffer, position, value1);
    apply_value(buffer, second_position, value2);
    // Si le second glyphe reçoit un ajustement, il est consommé : la paire
    // suivante commencera après lui.
    Some(if format2 == 0 {
        1
    } else {
        second_position.saturating_sub(position) + 1
    })
}

/// Format 1 : liste de paires explicites par premier glyphe.
fn pair_format1(
    subtable: &[u8],
    index: usize,
    second: u16,
    format1: u16,
    format2: u16,
) -> Option<(ValueRecord, ValueRecord)> {
    let set_count = usize::from(u16_at(subtable, 8)?);
    if index >= set_count {
        return None;
    }
    let set = usize::from(u16_at(subtable, 10 + index * 2)?);
    let count = usize::from(u16_at(subtable, set)?);
    let record_size = 2 + value_record_size(format1) + value_record_size(format2);
    // Les paires sont triées par `secondGlyph` : recherche dichotomique.
    let (mut lo, mut hi) = (0usize, count.min(MAX_ITEMS));
    while lo < hi {
        let mid = lo.midpoint(hi);
        let offset = set + 2 + mid * record_size;
        let candidate = u16_at(subtable, offset)?;
        match second.cmp(&candidate) {
            core::cmp::Ordering::Less => hi = mid,
            core::cmp::Ordering::Greater => lo = mid + 1,
            core::cmp::Ordering::Equal => {
                let mut r = Reader::at(subtable, offset + 2);
                let v1 = read_value_record(&mut r, format1);
                let v2 = read_value_record(&mut r, format2);
                return Some((v1, v2));
            }
        }
    }
    None
}

/// Format 2 : matrice classe × classe (le crénage compact des grandes
/// polices).
fn pair_format2(
    subtable: &[u8],
    first: u16,
    second: u16,
    format1: u16,
    format2: u16,
) -> Option<(ValueRecord, ValueRecord)> {
    let class1 = ClassDef::new(subtable.get(usize::from(u16_at(subtable, 8)?)..)?);
    let class2 = ClassDef::new(subtable.get(usize::from(u16_at(subtable, 10)?)..)?);
    let class1_count = usize::from(u16_at(subtable, 12)?);
    let class2_count = usize::from(u16_at(subtable, 14)?);
    let i = usize::from(class1.class(first));
    let j = usize::from(class2.class(second));
    if i >= class1_count || j >= class2_count {
        return None;
    }
    let record_size = value_record_size(format1) + value_record_size(format2);
    let offset = 16 + (i * class2_count + j) * record_size;
    let mut r = Reader::at(subtable, offset);
    let v1 = read_value_record(&mut r, format1);
    let v2 = read_value_record(&mut r, format2);
    Some((v1, v2))
}

/// Type 3 : attachement cursif (horizontal uniquement).
fn cursive(
    subtable: &[u8],
    lookup: &Lookup<'_>,
    buffer: &mut [GlyphInfo],
    gdef: Option<&Gdef<'_>>,
    position: usize,
) -> Option<usize> {
    if u16_at(subtable, 0)? != 1 {
        return None;
    }
    let coverage = Coverage::new(subtable.get(usize::from(u16_at(subtable, 2)?)..)?);
    let gid = buffer.get(position)?.gid;
    let index = usize::from(coverage.index(gid)?);
    let entry = entry_exit(subtable, index)?.0?;
    let previous = {
        let skip = SkipList::new(buffer, lookup, gdef);
        skip.previous(position)?
    };
    let previous_gid = buffer.get(previous)?.gid;
    let previous_index = usize::from(coverage.index(previous_gid)?);
    let exit = entry_exit(subtable, previous_index)?.1?;

    let right_to_left = lookup.flags & LOOKUP_RIGHT_TO_LEFT != 0;
    if right_to_left {
        let advance = i32::from(exit.x) + buffer.get(previous)?.x_offset;
        if let Some(info) = buffer.get_mut(previous) {
            info.x_advance = advance;
        }
        let delta = i32::from(entry.x) + buffer.get(position)?.x_offset;
        if let Some(info) = buffer.get_mut(position) {
            info.x_advance -= delta;
            info.x_offset -= delta;
        }
    } else {
        let delta = i32::from(exit.x) + buffer.get(previous)?.x_offset;
        if let Some(info) = buffer.get_mut(previous) {
            info.x_advance -= delta;
            info.x_offset -= delta;
        }
        let advance = i32::from(entry.x) + buffer.get(position)?.x_offset;
        if let Some(info) = buffer.get_mut(position) {
            info.x_advance = advance;
        }
    }
    // Le glyphe courant s'aligne verticalement sur le précédent.
    let delta_y = i32::from(exit.y) - i32::from(entry.y);
    if let Some(info) = buffer.get_mut(position) {
        info.y_offset += delta_y;
        info.attach_kind = AttachKind::Cursive;
        info.attach_chain = offset_between(position, previous);
    }
    Some(1)
}

/// Ancres d'entrée et de sortie d'un enregistrement cursif.
fn entry_exit(subtable: &[u8], index: usize) -> Option<(Option<Anchor>, Option<Anchor>)> {
    let count = usize::from(u16_at(subtable, 4)?);
    if index >= count.min(MAX_ITEMS) {
        return None;
    }
    let base = 6 + index * 4;
    let entry_offset = u16_at(subtable, base)?;
    let exit_offset = u16_at(subtable, base + 2)?;
    let entry = (entry_offset != 0)
        .then(|| read_anchor(subtable, usize::from(entry_offset)))
        .flatten();
    let exit = (exit_offset != 0)
        .then(|| read_anchor(subtable, usize::from(exit_offset)))
        .flatten();
    Some((entry, exit))
}

/// Décalage relatif de `to` par rapport à `from`.
fn offset_between(from: usize, to: usize) -> i32 {
    let a = i64::try_from(to).unwrap_or(0);
    let b = i64::try_from(from).unwrap_or(0);
    i32::try_from(a - b).unwrap_or(0)
}

/// Un `MarkArray` : la classe et l'ancre de chaque marque.
fn mark_record(mark_array: &[u8], index: usize) -> Option<(u16, Anchor)> {
    let count = usize::from(u16_at(mark_array, 0)?);
    if index >= count.min(MAX_ITEMS) {
        return None;
    }
    let base = 2 + index * 4;
    let class = u16_at(mark_array, base)?;
    let offset = u16_at(mark_array, base + 2)?;
    if offset == 0 {
        return None;
    }
    let anchor = read_anchor(mark_array, usize::from(offset))?;
    Some((class, anchor))
}

/// En-tête commun aux types 4, 5 et 6.
struct MarkAttach<'a> {
    mark_index: usize,
    base_index: usize,
    class_count: usize,
    mark_array: &'a [u8],
    base_array: &'a [u8],
}

/// Lit l'en-tête d'une sous-table marque-vers-X et repère les deux glyphes.
fn mark_header(subtable: &[u8], mark_gid: u16, base_gid: u16) -> Option<MarkAttach<'_>> {
    if u16_at(subtable, 0)? != 1 {
        return None;
    }
    let mark_coverage = Coverage::new(subtable.get(usize::from(u16_at(subtable, 2)?)..)?);
    let base_coverage = Coverage::new(subtable.get(usize::from(u16_at(subtable, 4)?)..)?);
    let mark_index = usize::from(mark_coverage.index(mark_gid)?);
    let base_index = usize::from(base_coverage.index(base_gid)?);
    let class_count = usize::from(u16_at(subtable, 6)?);
    if class_count == 0 || class_count > MAX_ITEMS {
        return None;
    }
    let mark_array = subtable.get(usize::from(u16_at(subtable, 8)?)..)?;
    let base_array = subtable.get(usize::from(u16_at(subtable, 10)?)..)?;
    Some(MarkAttach {
        mark_index,
        base_index,
        class_count,
        mark_array,
        base_array,
    })
}

/// Pose la marque en `position` sur le glyphe en `anchor_position`.
fn attach_mark(
    buffer: &mut [GlyphInfo],
    position: usize,
    anchor_position: usize,
    mark: Anchor,
    base: Anchor,
) -> usize {
    if let Some(info) = buffer.get_mut(position) {
        info.x_offset = i32::from(base.x) - i32::from(mark.x);
        info.y_offset = i32::from(base.y) - i32::from(mark.y);
        info.attach_kind = AttachKind::Mark;
        info.attach_chain = offset_between(position, anchor_position);
    }
    1
}

/// Type 4 : marque vers base.
fn mark_to_base(
    subtable: &[u8],
    lookup: &Lookup<'_>,
    buffer: &mut [GlyphInfo],
    gdef: Option<&Gdef<'_>>,
    position: usize,
) -> Option<usize> {
    let mark_gid = buffer.get(position)?.gid;
    // La base est le glyphe précédent qui n'est pas une marque.
    let base_position = previous_non_mark(buffer, lookup, gdef, position)?;
    let base_gid = buffer.get(base_position)?.gid;
    let header = mark_header(subtable, mark_gid, base_gid)?;
    let (class, mark_anchor) = mark_record(header.mark_array, header.mark_index)?;
    if usize::from(class) >= header.class_count {
        return None;
    }
    let base_count = usize::from(u16_at(header.base_array, 0)?);
    if header.base_index >= base_count.min(MAX_ITEMS) {
        return None;
    }
    let offset = 2 + (header.base_index * header.class_count + usize::from(class)) * 2;
    let anchor_offset = u16_at(header.base_array, offset)?;
    if anchor_offset == 0 {
        return None;
    }
    let base_anchor = read_anchor(header.base_array, usize::from(anchor_offset))?;
    Some(attach_mark(
        buffer,
        position,
        base_position,
        mark_anchor,
        base_anchor,
    ))
}

/// Type 5 : marque vers ligature. Le composant visé est celui qu'indique
/// `lig_component`, posé par la substitution de ligature.
fn mark_to_ligature(
    subtable: &[u8],
    lookup: &Lookup<'_>,
    buffer: &mut [GlyphInfo],
    gdef: Option<&Gdef<'_>>,
    position: usize,
) -> Option<usize> {
    let mark = *buffer.get(position)?;
    let lig_position = previous_non_mark(buffer, lookup, gdef, position)?;
    let ligature = *buffer.get(lig_position)?;
    let header = mark_header(subtable, mark.gid, ligature.gid)?;
    let (class, mark_anchor) = mark_record(header.mark_array, header.mark_index)?;
    if usize::from(class) >= header.class_count {
        return None;
    }
    let lig_count = usize::from(u16_at(header.base_array, 0)?);
    if header.base_index >= lig_count.min(MAX_ITEMS) {
        return None;
    }
    let attach_offset = u16_at(header.base_array, 2 + header.base_index * 2)?;
    let attach = header.base_array.get(usize::from(attach_offset)..)?;
    let component_count = usize::from(u16_at(attach, 0)?);
    if component_count == 0 {
        return None;
    }
    // Le rang de composant vient de la ligature formée par `GSUB` ; à
    // défaut, on vise le dernier composant, comme le fait HarfBuzz.
    let component = if mark.lig_id != 0 && mark.lig_id == ligature.lig_id {
        usize::from(mark.lig_component).saturating_sub(1)
    } else {
        component_count - 1
    }
    .min(component_count - 1);
    let offset = 2 + (component * header.class_count + usize::from(class)) * 2;
    let anchor_offset = u16_at(attach, offset)?;
    if anchor_offset == 0 {
        return None;
    }
    let lig_anchor = read_anchor(attach, usize::from(anchor_offset))?;
    Some(attach_mark(
        buffer,
        position,
        lig_position,
        mark_anchor,
        lig_anchor,
    ))
}

/// Type 6 : marque vers marque (empilement des diacritiques).
fn mark_to_mark(
    subtable: &[u8],
    lookup: &Lookup<'_>,
    buffer: &mut [GlyphInfo],
    gdef: Option<&Gdef<'_>>,
    position: usize,
) -> Option<usize> {
    let mark_gid = buffer.get(position)?.gid;
    let previous = {
        let skip = SkipList::new(buffer, lookup, gdef);
        skip.previous(position)?
    };
    let previous_gid = buffer.get(previous)?.gid;
    let header = mark_header(subtable, mark_gid, previous_gid)?;
    let (class, mark_anchor) = mark_record(header.mark_array, header.mark_index)?;
    if usize::from(class) >= header.class_count {
        return None;
    }
    let count = usize::from(u16_at(header.base_array, 0)?);
    if header.base_index >= count.min(MAX_ITEMS) {
        return None;
    }
    let offset = 2 + (header.base_index * header.class_count + usize::from(class)) * 2;
    let anchor_offset = u16_at(header.base_array, offset)?;
    if anchor_offset == 0 {
        return None;
    }
    let anchor = read_anchor(header.base_array, usize::from(anchor_offset))?;
    Some(attach_mark(buffer, position, previous, mark_anchor, anchor))
}

/// Glyphe précédent qui n'est pas une marque, en tenant compte des drapeaux.
fn previous_non_mark(
    buffer: &[GlyphInfo],
    lookup: &Lookup<'_>,
    gdef: Option<&Gdef<'_>>,
    position: usize,
) -> Option<usize> {
    // Les sous-tables marque-vers-base portent presque toujours le drapeau
    // « ignorer les marques » ; s'il manque, on saute quand même les
    // marques connues, sans quoi un accent servirait de base au suivant.
    let skip = SkipList::new(buffer, lookup, gdef);
    let mut candidate = skip.previous(position)?;
    if lookup.flags & LOOKUP_IGNORE_MARKS == 0 {
        while buffer.get(candidate).is_some_and(GlyphInfo::is_mark) {
            candidate = skip.previous(candidate)?;
        }
    }
    Some(candidate)
}

/// Profondeur maximale de la chaîne d'attachements.
const MAX_ATTACH_DEPTH: u8 = 32;

/// Reporte les décalages des glyphes d'ancrage sur ceux qui y sont attachés.
///
/// Tant que `GPOS` travaille, une marque ne connaît que sa position
/// **relative** à sa base. Cette passe finale ajoute la position de la base
/// et retire les avances accumulées entre les deux, pour que le décalage
/// devienne absolu.
pub fn resolve_attachments(buffer: &mut [GlyphInfo]) {
    for i in 0..buffer.len() {
        resolve_one(buffer, i, 0);
    }
}

fn resolve_one(buffer: &mut [GlyphInfo], index: usize, depth: u8) {
    let Some(info) = buffer.get(index) else {
        return;
    };
    let kind = info.attach_kind;
    let chain = info.attach_chain;
    if kind == AttachKind::None || chain == 0 || depth > MAX_ATTACH_DEPTH {
        return;
    }
    let target = isize::try_from(index).unwrap_or(0) + isize::try_from(chain).unwrap_or(0);
    let Ok(target) = usize::try_from(target) else {
        return;
    };
    if target >= buffer.len() {
        return;
    }
    // Marquer avant de récurser : coupe les cycles d'une police corrompue.
    if let Some(info) = buffer.get_mut(index) {
        info.attach_chain = 0;
    }
    resolve_one(buffer, target, depth + 1);
    let (base_x, base_y) = buffer
        .get(target)
        .map_or((0, 0), |g| (g.x_offset, g.y_offset));
    if kind == AttachKind::Cursive {
        if let Some(info) = buffer.get_mut(index) {
            info.y_offset += base_y;
        }
        return;
    }
    let (mut dx, mut dy) = (base_x, base_y);
    if target < index {
        for k in target..index {
            if let Some(g) = buffer.get(k) {
                dx -= g.x_advance;
                dy -= g.y_advance;
            }
        }
    } else {
        for k in index + 1..=target {
            if let Some(g) = buffer.get(k) {
                dx += g.x_advance;
                dy += g.y_advance;
            }
        }
    }
    if let Some(info) = buffer.get_mut(index) {
        info.x_offset += dx;
        info.y_offset += dy;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod tests {
    use super::*;
    use crate::opentype::common::tests::{class_def_format2, coverage_format1};
    use crate::opentype::common::VALUE_X_ADVANCE;

    fn buffer(gids: &[u16]) -> Vec<GlyphInfo> {
        gids.iter()
            .enumerate()
            .map(|(i, g)| GlyphInfo::new(*g, u32::try_from(i).unwrap(), 1))
            .collect()
    }

    fn lookup(kind: u16, subtable: &[u8]) -> Lookup<'_> {
        Lookup {
            kind,
            flags: 0,
            mark_filtering_set: None,
            subtables: vec![(kind, subtable)],
        }
    }

    /// Crénage format 1 : une seule paire, ajustement sur le premier glyphe.
    pub(crate) fn pair_format1_table(first: u16, second: u16, adjust: i16) -> Vec<u8> {
        let coverage = coverage_format1(&[first]);
        let header = 12;
        let set_offset = header + coverage.len();
        let mut t = Vec::new();
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&u16::try_from(header).unwrap().to_be_bytes());
        t.extend_from_slice(&VALUE_X_ADVANCE.to_be_bytes());
        t.extend_from_slice(&0u16.to_be_bytes());
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&u16::try_from(set_offset).unwrap().to_be_bytes());
        t.extend_from_slice(&coverage);
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&second.to_be_bytes());
        t.extend_from_slice(&adjust.to_be_bytes());
        t
    }

    #[test]
    fn single_adjustment_format1() {
        let coverage = coverage_format1(&[10]);
        let mut t = Vec::new();
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&8u16.to_be_bytes());
        t.extend_from_slice(&VALUE_X_ADVANCE.to_be_bytes());
        t.extend_from_slice(&(-40i16).to_be_bytes());
        t.extend_from_slice(&coverage);
        let mut b = buffer(&[10, 11]);
        assert_eq!(single(&t, &mut b, 0), Some(1));
        assert_eq!(single(&t, &mut b, 1), None);
        assert_eq!(b[0].x_advance, -40);
    }

    #[test]
    fn pair_kerning_format1() {
        let t = pair_format1_table(10, 11, -80);
        let l = lookup(2, &t);
        let mut b = buffer(&[10, 11]);
        assert_eq!(pair(&t, &l, &mut b, None, 0), Some(1));
        assert_eq!(b[0].x_advance, -80);
        assert_eq!(b[1].x_advance, 0);
        // Paire absente : rien ne bouge.
        let mut b = buffer(&[10, 12]);
        assert_eq!(pair(&t, &l, &mut b, None, 0), None);
    }

    #[test]
    fn pair_kerning_format2() {
        let c1 = class_def_format2(&[(10, 10, 1)]);
        let c2 = class_def_format2(&[(11, 11, 1)]);
        let coverage = coverage_format1(&[10]);
        let header = 16;
        // Deux classes de chaque côté : quatre enregistrements de 2 octets.
        let matrix = 2 * 2 * 2;
        let coverage_offset = header + matrix;
        let c1_offset = coverage_offset + coverage.len();
        let c2_offset = c1_offset + c1.len();
        let mut t = Vec::new();
        t.extend_from_slice(&2u16.to_be_bytes());
        t.extend_from_slice(&u16::try_from(coverage_offset).unwrap().to_be_bytes());
        t.extend_from_slice(&VALUE_X_ADVANCE.to_be_bytes());
        t.extend_from_slice(&0u16.to_be_bytes());
        t.extend_from_slice(&u16::try_from(c1_offset).unwrap().to_be_bytes());
        t.extend_from_slice(&u16::try_from(c2_offset).unwrap().to_be_bytes());
        t.extend_from_slice(&2u16.to_be_bytes());
        t.extend_from_slice(&2u16.to_be_bytes());
        for value in [0i16, 0, 0, -120] {
            t.extend_from_slice(&value.to_be_bytes());
        }
        t.extend_from_slice(&coverage);
        t.extend_from_slice(&c1);
        t.extend_from_slice(&c2);

        let l = lookup(2, &t);
        let mut b = buffer(&[10, 11]);
        assert_eq!(pair(&t, &l, &mut b, None, 0), Some(1));
        assert_eq!(b[0].x_advance, -120);
    }

    /// La passe finale rend les décalages de marque absolus.
    #[test]
    fn attachment_offsets_are_propagated() {
        let mut b = buffer(&[1, 2]);
        b[0].x_advance = 500;
        b[1].x_offset = 100;
        b[1].y_offset = 200;
        b[1].attach_kind = AttachKind::Mark;
        b[1].attach_chain = -1;
        resolve_attachments(&mut b);
        assert_eq!(b[1].x_offset, 100 - 500);
        assert_eq!(b[1].y_offset, 200);
    }

    /// Une chaîne d'attachements circulaire ne fait pas boucler la passe.
    #[test]
    fn cyclic_attachments_terminate() {
        let mut b = buffer(&[1, 2]);
        b[0].attach_kind = AttachKind::Mark;
        b[0].attach_chain = 1;
        b[1].attach_kind = AttachKind::Mark;
        b[1].attach_chain = -1;
        resolve_attachments(&mut b);
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn corrupt_subtables_are_ignored() {
        let mut b = buffer(&[1, 2]);
        let l = lookup(2, &[]);
        assert_eq!(single(&[], &mut b, 0), None);
        assert_eq!(pair(&[0, 1], &l, &mut b, None, 0), None);
        assert_eq!(cursive(&[0, 2], &l, &mut b, None, 0), None);
        assert_eq!(mark_to_base(&[0, 2], &l, &mut b, None, 1), None);
        assert!(Gpos::parse(&[0, 1]).is_none());
    }
}
