//! Correspondance contextuelle et contextuelle enchaînée.
//!
//! Les sous-tables « contextuelles » de `GSUB` (types 5 et 6) et de `GPOS`
//! (types 7 et 8) ont **exactement** la même forme : elles décrivent un
//! motif (éventuellement précédé d'un contexte arrière et suivi d'un
//! contexte avant) et, quand le motif correspond, une liste de tables de
//! recherche à appliquer à des positions données du motif.
//!
//! Ce module ne fait que la reconnaissance ; l'application des tables
//! imbriquées appartient à `GSUB` ou à `GPOS` selon la table d'origine.
//!
//! Les trois formats sont couverts :
//! - **format 1** : motifs par identifiants de glyphes ;
//! - **format 2** : motifs par classes ;
//! - **format 3** : motifs par tables de couverture, une par position.

use crate::opentype::buffer::SkipList;
use crate::opentype::common::{ClassDef, Coverage};
use crate::reader::{u16_at, Reader};

/// Nombre maximal de règles ou de positions lues dans une sous-table.
const MAX_ITEMS: usize = 256;

/// Une table de recherche à appliquer à une position du motif.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceLookup {
    /// Rang, dans le motif reconnu, du glyphe où appliquer la table.
    pub sequence_index: u16,
    /// Indice de la table de recherche dans la `LookupList`.
    pub lookup_index: u16,
}

/// Motif reconnu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMatch {
    /// Positions, dans le tampon, des glyphes du motif (contexte exclu).
    pub input: Vec<usize>,
    /// Tables de recherche à appliquer.
    pub records: Vec<SequenceLookup>,
}

/// Lit `count` `uint16` à partir de `offset`.
fn read_u16_array(data: &[u8], offset: usize, count: usize) -> Option<Vec<u16>> {
    if count > MAX_ITEMS {
        return None;
    }
    let mut r = Reader::at(data, offset);
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(r.read_u16()?);
    }
    Some(out)
}

/// Lit une liste de `SequenceLookup` à partir de `offset`.
fn read_records(data: &[u8], offset: usize, count: usize) -> Vec<SequenceLookup> {
    let mut out = Vec::new();
    let mut r = Reader::at(data, offset);
    for _ in 0..count.min(MAX_ITEMS) {
        let (Some(sequence_index), Some(lookup_index)) = (r.read_u16(), r.read_u16()) else {
            break;
        };
        out.push(SequenceLookup {
            sequence_index,
            lookup_index,
        });
    }
    out
}

/// Comment comparer un élément du motif au glyphe du tampon.
enum Matcher<'a> {
    /// Identifiant de glyphe exact (formats 1).
    Glyph(u16),
    /// Classe (formats 2).
    Class(u16, ClassDef<'a>),
    /// Table de couverture (formats 3).
    Covered(Coverage<'a>),
}

impl Matcher<'_> {
    fn accepts(&self, gid: u16) -> bool {
        match self {
            Matcher::Glyph(expected) => *expected == gid,
            Matcher::Class(expected, classes) => classes.class(gid) == *expected,
            Matcher::Covered(coverage) => coverage.contains(gid),
        }
    }
}

/// Vérifie le motif principal à partir de `position` et renvoie les
/// positions occupées. `matchers` ne contient **pas** le premier glyphe,
/// déjà reconnu par la couverture.
fn match_forward(
    skip: &SkipList<'_, '_>,
    position: usize,
    matchers: &[Matcher<'_>],
) -> Option<Vec<usize>> {
    let mut positions = vec![position];
    let mut current = position;
    for matcher in matchers {
        current = skip.next(current)?;
        if !matcher.accepts(skip.gid(current)?) {
            return None;
        }
        positions.push(current);
    }
    Some(positions)
}

/// Vérifie le contexte arrière : les éléments sont donnés du plus proche au
/// plus lointain, comme dans la spécification.
fn match_backtrack(
    skip: &SkipList<'_, '_>,
    position: usize,
    matchers: &[Matcher<'_>],
) -> Option<()> {
    let mut current = position;
    for matcher in matchers {
        current = skip.previous(current)?;
        if !matcher.accepts(skip.gid(current)?) {
            return None;
        }
    }
    Some(())
}

/// Vérifie le contexte avant, à partir du dernier glyphe du motif.
fn match_lookahead(skip: &SkipList<'_, '_>, last: usize, matchers: &[Matcher<'_>]) -> Option<()> {
    let mut current = last;
    for matcher in matchers {
        current = skip.next(current)?;
        if !matcher.accepts(skip.gid(current)?) {
            return None;
        }
    }
    Some(())
}

/// Sous-table contextuelle (GSUB 5, GPOS 7).
#[must_use]
pub fn match_context(
    subtable: &[u8],
    skip: &SkipList<'_, '_>,
    position: usize,
) -> Option<ContextMatch> {
    let gid = skip.gid(position)?;
    match u16_at(subtable, 0)? {
        1 => context_format1(subtable, skip, position, gid),
        2 => context_format2(subtable, skip, position, gid),
        3 => context_format3(subtable, skip, position),
        _ => None,
    }
}

/// Décalage de la table de règles à utiliser, d'après un `index` de jeu.
fn rule_set_offset(subtable: &[u8], count_offset: usize, index: usize) -> Option<usize> {
    let count = usize::from(u16_at(subtable, count_offset)?);
    if index >= count.min(MAX_ITEMS) {
        return None;
    }
    let offset = usize::from(u16_at(subtable, count_offset + 2 + index * 2)?);
    (offset != 0).then_some(offset)
}

/// Parcourt les règles d'un jeu et renvoie la première qui correspond.
fn first_matching_rule<F>(subtable: &[u8], set: usize, mut try_rule: F) -> Option<ContextMatch>
where
    F: FnMut(usize) -> Option<ContextMatch>,
{
    let count = usize::from(u16_at(subtable, set)?);
    for i in 0..count.min(MAX_ITEMS) {
        let Some(offset) = u16_at(subtable, set + 2 + i * 2) else {
            break;
        };
        let rule = set.checked_add(usize::from(offset))?;
        if let Some(found) = try_rule(rule) {
            return Some(found);
        }
    }
    None
}

/// Format 1 : motif par identifiants de glyphes.
fn context_format1(
    subtable: &[u8],
    skip: &SkipList<'_, '_>,
    position: usize,
    gid: u16,
) -> Option<ContextMatch> {
    let coverage_offset = usize::from(u16_at(subtable, 2)?);
    let index = Coverage::new(subtable.get(coverage_offset..)?).index(gid)?;
    let set = rule_set_offset(subtable, 4, usize::from(index))?;
    first_matching_rule(subtable, set, |rule| {
        let glyph_count = usize::from(u16_at(subtable, rule)?);
        let record_count = usize::from(u16_at(subtable, rule + 2)?);
        let sequence = read_u16_array(subtable, rule + 4, glyph_count.checked_sub(1)?)?;
        let matchers: Vec<Matcher<'_>> = sequence.into_iter().map(Matcher::Glyph).collect();
        let input = match_forward(skip, position, &matchers)?;
        let records = read_records(subtable, rule + 4 + (glyph_count - 1) * 2, record_count);
        Some(ContextMatch { input, records })
    })
}

/// Format 2 : motif par classes.
fn context_format2(
    subtable: &[u8],
    skip: &SkipList<'_, '_>,
    position: usize,
    gid: u16,
) -> Option<ContextMatch> {
    let coverage_offset = usize::from(u16_at(subtable, 2)?);
    Coverage::new(subtable.get(coverage_offset..)?).index(gid)?;
    let class_offset = usize::from(u16_at(subtable, 4)?);
    let classes = ClassDef::new(subtable.get(class_offset..)?);
    let set = rule_set_offset(subtable, 6, usize::from(classes.class(gid)))?;
    first_matching_rule(subtable, set, |rule| {
        let glyph_count = usize::from(u16_at(subtable, rule)?);
        let record_count = usize::from(u16_at(subtable, rule + 2)?);
        let sequence = read_u16_array(subtable, rule + 4, glyph_count.checked_sub(1)?)?;
        let matchers: Vec<Matcher<'_>> = sequence
            .into_iter()
            .map(|c| Matcher::Class(c, classes))
            .collect();
        let input = match_forward(skip, position, &matchers)?;
        let records = read_records(subtable, rule + 4 + (glyph_count - 1) * 2, record_count);
        Some(ContextMatch { input, records })
    })
}

/// Format 3 : une table de couverture par position.
fn context_format3(
    subtable: &[u8],
    skip: &SkipList<'_, '_>,
    position: usize,
) -> Option<ContextMatch> {
    let glyph_count = usize::from(u16_at(subtable, 2)?);
    let record_count = usize::from(u16_at(subtable, 4)?);
    if glyph_count == 0 || glyph_count > MAX_ITEMS {
        return None;
    }
    let coverages = read_u16_array(subtable, 6, glyph_count)?;
    let first = Coverage::new(subtable.get(usize::from(coverages[0])..)?);
    if !first.contains(skip.gid(position)?) {
        return None;
    }
    let mut matchers = Vec::new();
    for offset in &coverages[1..] {
        matchers.push(Matcher::Covered(Coverage::new(
            subtable.get(usize::from(*offset)..)?,
        )));
    }
    let input = match_forward(skip, position, &matchers)?;
    let records = read_records(subtable, 6 + glyph_count * 2, record_count);
    Some(ContextMatch { input, records })
}

/// Sous-table contextuelle enchaînée (GSUB 6, GPOS 8).
#[must_use]
pub fn match_chain_context(
    subtable: &[u8],
    skip: &SkipList<'_, '_>,
    position: usize,
) -> Option<ContextMatch> {
    let gid = skip.gid(position)?;
    match u16_at(subtable, 0)? {
        1 => chain_format1(subtable, skip, position, gid),
        2 => chain_format2(subtable, skip, position, gid),
        3 => chain_format3(subtable, skip, position),
        _ => None,
    }
}

/// Les trois sections d'une règle enchaînée, lues séquentiellement.
struct ChainRule {
    backtrack: Vec<u16>,
    input: Vec<u16>,
    lookahead: Vec<u16>,
    records: Vec<SequenceLookup>,
}

/// Lit une règle enchaînée (formats 1 et 2 ont la même disposition, seul le
/// sens des nombres change).
fn read_chain_rule(subtable: &[u8], rule: usize) -> Option<ChainRule> {
    let mut position = rule;
    let backtrack_count = usize::from(u16_at(subtable, position)?);
    let backtrack = read_u16_array(subtable, position + 2, backtrack_count)?;
    position += 2 + backtrack_count * 2;
    let input_count = usize::from(u16_at(subtable, position)?);
    let input = read_u16_array(subtable, position + 2, input_count.checked_sub(1)?)?;
    position += 2 + (input_count - 1) * 2;
    let lookahead_count = usize::from(u16_at(subtable, position)?);
    let lookahead = read_u16_array(subtable, position + 2, lookahead_count)?;
    position += 2 + lookahead_count * 2;
    let record_count = usize::from(u16_at(subtable, position)?);
    let records = read_records(subtable, position + 2, record_count);
    Some(ChainRule {
        backtrack,
        input,
        lookahead,
        records,
    })
}

/// Vérifie une règle enchaînée déjà lue, avec une fabrique de comparateurs.
fn check_chain_rule<'a, F>(
    rule: &ChainRule,
    skip: &SkipList<'_, '_>,
    position: usize,
    make: F,
) -> Option<ContextMatch>
where
    F: Fn(u16, usize) -> Matcher<'a>,
{
    // Section 0 : contexte arrière, 1 : motif, 2 : contexte avant.
    let backtrack: Vec<Matcher<'_>> = rule.backtrack.iter().map(|v| make(*v, 0)).collect();
    let input: Vec<Matcher<'_>> = rule.input.iter().map(|v| make(*v, 1)).collect();
    let lookahead: Vec<Matcher<'_>> = rule.lookahead.iter().map(|v| make(*v, 2)).collect();
    match_backtrack(skip, position, &backtrack)?;
    let positions = match_forward(skip, position, &input)?;
    let last = positions.last().copied()?;
    match_lookahead(skip, last, &lookahead)?;
    Some(ContextMatch {
        input: positions,
        records: rule.records.clone(),
    })
}

/// Format 1 enchaîné : motifs par identifiants de glyphes.
fn chain_format1(
    subtable: &[u8],
    skip: &SkipList<'_, '_>,
    position: usize,
    gid: u16,
) -> Option<ContextMatch> {
    let coverage_offset = usize::from(u16_at(subtable, 2)?);
    let index = Coverage::new(subtable.get(coverage_offset..)?).index(gid)?;
    let set = rule_set_offset(subtable, 4, usize::from(index))?;
    first_matching_rule(subtable, set, |rule| {
        let parsed = read_chain_rule(subtable, rule)?;
        check_chain_rule(&parsed, skip, position, |v, _| Matcher::Glyph(v))
    })
}

/// Format 2 enchaîné : motifs par classes, une table de classes par section.
fn chain_format2(
    subtable: &[u8],
    skip: &SkipList<'_, '_>,
    position: usize,
    gid: u16,
) -> Option<ContextMatch> {
    let coverage_offset = usize::from(u16_at(subtable, 2)?);
    Coverage::new(subtable.get(coverage_offset..)?).index(gid)?;
    let backtrack_classes = ClassDef::new(subtable.get(usize::from(u16_at(subtable, 4)?)..)?);
    let input_classes = ClassDef::new(subtable.get(usize::from(u16_at(subtable, 6)?)..)?);
    let lookahead_classes = ClassDef::new(subtable.get(usize::from(u16_at(subtable, 8)?)..)?);
    let set = rule_set_offset(subtable, 10, usize::from(input_classes.class(gid)))?;
    first_matching_rule(subtable, set, |rule| {
        let parsed = read_chain_rule(subtable, rule)?;
        check_chain_rule(&parsed, skip, position, |v, section| match section {
            0 => Matcher::Class(v, backtrack_classes),
            2 => Matcher::Class(v, lookahead_classes),
            _ => Matcher::Class(v, input_classes),
        })
    })
}

/// Format 3 enchaîné : trois listes de tables de couverture.
fn chain_format3(
    subtable: &[u8],
    skip: &SkipList<'_, '_>,
    position: usize,
) -> Option<ContextMatch> {
    let mut offset = 2usize;
    let backtrack_count = usize::from(u16_at(subtable, offset)?);
    let backtrack = read_u16_array(subtable, offset + 2, backtrack_count)?;
    offset += 2 + backtrack_count * 2;
    let input_count = usize::from(u16_at(subtable, offset)?);
    if input_count == 0 {
        return None;
    }
    let input = read_u16_array(subtable, offset + 2, input_count)?;
    offset += 2 + input_count * 2;
    let lookahead_count = usize::from(u16_at(subtable, offset)?);
    let lookahead = read_u16_array(subtable, offset + 2, lookahead_count)?;
    offset += 2 + lookahead_count * 2;
    let record_count = usize::from(u16_at(subtable, offset)?);
    let records = read_records(subtable, offset + 2, record_count);

    let coverage =
        |o: u16| -> Option<Coverage<'_>> { subtable.get(usize::from(o)..).map(Coverage::new) };
    if !coverage(input[0])?.contains(skip.gid(position)?) {
        return None;
    }
    let mut backtrack_matchers = Vec::new();
    for o in &backtrack {
        backtrack_matchers.push(Matcher::Covered(coverage(*o)?));
    }
    let mut input_matchers = Vec::new();
    for o in &input[1..] {
        input_matchers.push(Matcher::Covered(coverage(*o)?));
    }
    let mut lookahead_matchers = Vec::new();
    for o in &lookahead {
        lookahead_matchers.push(Matcher::Covered(coverage(*o)?));
    }
    match_backtrack(skip, position, &backtrack_matchers)?;
    let positions = match_forward(skip, position, &input_matchers)?;
    let last = positions.last().copied()?;
    match_lookahead(skip, last, &lookahead_matchers)?;
    Some(ContextMatch {
        input: positions,
        records,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::opentype::buffer::GlyphInfo;
    use crate::opentype::common::tests::{class_def_format2, coverage_format1};
    use crate::opentype::common::Lookup;

    fn buffer(gids: &[u16]) -> Vec<GlyphInfo> {
        gids.iter()
            .enumerate()
            .map(|(i, g)| GlyphInfo::new(*g, u32::try_from(i).unwrap_or(0), 1))
            .collect()
    }

    fn lookup() -> Lookup<'static> {
        Lookup {
            kind: 6,
            flags: 0,
            mark_filtering_set: None,
            subtables: Vec::new(),
        }
    }

    /// Contextuel format 3 : « 10 11 12 » déclenche une table sur le glyphe
    /// du milieu.
    #[test]
    fn context_format3_matches() {
        let c10 = coverage_format1(&[10]);
        let c11 = coverage_format1(&[11]);
        let c12 = coverage_format1(&[12]);
        let header = 6 + 3 * 2;
        let records = header + 4; // deux SequenceLookup
        let mut t = Vec::new();
        t.extend_from_slice(&3u16.to_be_bytes());
        t.extend_from_slice(&3u16.to_be_bytes());
        t.extend_from_slice(&1u16.to_be_bytes());
        let o10 = u16::try_from(records).unwrap();
        let o11 = o10 + u16::try_from(c10.len()).unwrap();
        let o12 = o11 + u16::try_from(c11.len()).unwrap();
        t.extend_from_slice(&o10.to_be_bytes());
        t.extend_from_slice(&o11.to_be_bytes());
        t.extend_from_slice(&o12.to_be_bytes());
        t.extend_from_slice(&1u16.to_be_bytes()); // sequenceIndex
        t.extend_from_slice(&7u16.to_be_bytes()); // lookupListIndex
        while t.len() < usize::from(o10) {
            t.push(0);
        }
        t.extend_from_slice(&c10);
        t.extend_from_slice(&c11);
        t.extend_from_slice(&c12);

        let b = buffer(&[10, 11, 12]);
        let l = lookup();
        let skip = SkipList::new(&b, &l, None);
        let found = match_context(&t, &skip, 0).unwrap();
        assert_eq!(found.input, vec![0, 1, 2]);
        assert_eq!(
            found.records,
            vec![SequenceLookup {
                sequence_index: 1,
                lookup_index: 7
            }]
        );

        let b = buffer(&[10, 99, 12]);
        let skip = SkipList::new(&b, &l, None);
        assert!(match_context(&t, &skip, 0).is_none());
    }

    /// Enchaîné format 3 : contexte arrière « 10 », motif « 11 », contexte
    /// avant « 12 ».
    #[test]
    fn chain_format3_matches() {
        let c10 = coverage_format1(&[10]);
        let c11 = coverage_format1(&[11]);
        let c12 = coverage_format1(&[12]);
        // en-tête : format, 1 backtrack, 1 input, 1 lookahead, 1 record
        let header = 2 + 2 + 2 + 2 + 2 + 2 + 2 + 2 + 4;
        let o10 = u16::try_from(header).unwrap();
        let o11 = o10 + u16::try_from(c10.len()).unwrap();
        let o12 = o11 + u16::try_from(c11.len()).unwrap();
        let mut t = Vec::new();
        t.extend_from_slice(&3u16.to_be_bytes());
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&o10.to_be_bytes());
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&o11.to_be_bytes());
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&o12.to_be_bytes());
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&0u16.to_be_bytes());
        t.extend_from_slice(&5u16.to_be_bytes());
        t.extend_from_slice(&c10);
        t.extend_from_slice(&c11);
        t.extend_from_slice(&c12);

        let b = buffer(&[10, 11, 12]);
        let l = lookup();
        let skip = SkipList::new(&b, &l, None);
        let found = match_chain_context(&t, &skip, 1).unwrap();
        assert_eq!(found.input, vec![1]);
        assert_eq!(found.records[0].lookup_index, 5);

        // Sans le contexte arrière, rien ne correspond.
        let b = buffer(&[11, 12]);
        let skip = SkipList::new(&b, &l, None);
        assert!(match_chain_context(&t, &skip, 0).is_none());
    }

    #[test]
    fn corrupt_subtables_return_none() {
        let b = buffer(&[1, 2]);
        let l = lookup();
        let skip = SkipList::new(&b, &l, None);
        assert!(match_context(&[], &skip, 0).is_none());
        assert!(match_context(&[0, 9, 0, 0], &skip, 0).is_none());
        assert!(match_chain_context(&[0, 3], &skip, 0).is_none());
        assert_eq!(class_def_format2(&[]).len(), 4);
    }
}
