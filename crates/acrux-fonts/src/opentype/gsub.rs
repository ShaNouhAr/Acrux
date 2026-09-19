//! Table `GSUB` : substitution de glyphes (OpenType Layout).
//!
//! C'est `GSUB` qui transforme `f` + `i` en la ligature `fi`, qui choisit la
//! forme initiale ou finale d'une lettre arabe, qui remplace un chiffre par
//! sa version elzévirienne, qui décompose un caractère précomposé en base et
//! marque.
//!
//! ## Types de sous-tables implémentés
//!
//! | Type | Nom                        | Formats | État |
//! |------|----------------------------|---------|------|
//! | 1    | Substitution simple        | 1, 2    | oui  |
//! | 2    | Substitution multiple      | 1       | oui  |
//! | 3    | Substitution alternative   | 1       | oui (première variante) |
//! | 4    | Substitution de ligature   | 1       | oui  |
//! | 5    | Contextuelle               | 1, 2, 3 | oui  |
//! | 6    | Contextuelle enchaînée     | 1, 2, 3 | oui  |
//! | 7    | Extension                  | 1       | oui (transparente) |
//! | 8    | Contextuelle inverse simple| 1       | oui  |
//!
//! Pour le type 3, la spécification laisse le choix de la variante à
//! l'application (`aalt` dans un menu de caractères) ; faute d'interface,
//! nous prenons systématiquement la première, qui est la variante par défaut.

use crate::opentype::buffer::{GlyphInfo, SkipList};
use crate::opentype::common::{Coverage, Gdef, LayoutTable, Lookup};
use crate::opentype::context::{match_chain_context, match_context, ContextMatch};
use crate::reader::u16_at;

/// Profondeur maximale d'imbrication des tables de recherche contextuelles.
const MAX_NESTING: u8 = 6;

/// Nombre maximal d'éléments lus dans une sous-table.
const MAX_ITEMS: usize = 256;

/// Type des sous-tables d'extension en `GSUB`.
const EXTENSION_KIND: u16 = 7;

/// Table `GSUB` analysée, tables de recherche résolues.
#[derive(Debug, Clone)]
pub struct Gsub<'a> {
    table: LayoutTable<'a>,
    lookups: Vec<Lookup<'a>>,
}

impl<'a> Gsub<'a> {
    /// Analyse la table.
    ///
    /// Les sous-tables d'extension (type 7) sont résolues une fois pour
    /// toutes, de sorte que le reste du code ne les voit jamais.
    #[must_use]
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let table = LayoutTable::parse(data)?;
        let count = table.lookup_count();
        let mut lookups = Vec::with_capacity(usize::from(count));
        for i in 0..count {
            let lookup = table.lookup(i, EXTENSION_KIND).unwrap_or(Lookup {
                kind: 0,
                flags: 0,
                mark_filtering_set: None,
                subtables: Vec::new(),
            });
            lookups.push(lookup);
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
    ///
    /// Seuls les glyphes dont le masque recouvre `mask` sont touchés, ce qui
    /// permet d'activer `init` sur une lettre et `fina` sur une autre.
    pub fn apply(
        &self,
        index: u16,
        mask: u32,
        buffer: &mut Vec<GlyphInfo>,
        gdef: Option<&Gdef<'a>>,
    ) {
        self.apply_nested(index, mask, buffer, gdef, 0);
    }

    fn apply_nested(
        &self,
        index: u16,
        mask: u32,
        buffer: &mut Vec<GlyphInfo>,
        gdef: Option<&Gdef<'a>>,
        depth: u8,
    ) {
        if depth > MAX_NESTING {
            return;
        }
        let Some(lookup) = self.lookups.get(usize::from(index)) else {
            return;
        };
        if lookup.kind == 8 {
            Self::apply_reverse(lookup, mask, buffer, gdef);
            return;
        }
        let mut position = 0usize;
        let mut budget = buffer.len() * 8 + 64;
        while position < buffer.len() && budget > 0 {
            budget -= 1;
            let applies = buffer
                .get(position)
                .is_some_and(|g| g.mask & mask != 0 && !skipped(lookup, gdef, g.gid));
            if !applies {
                position += 1;
                continue;
            }
            let advance = self.apply_at(lookup, mask, buffer, gdef, position, depth);
            position += advance.max(1);
        }
    }

    /// Applique la première sous-table qui correspond à `position` et
    /// renvoie de combien de glyphes avancer.
    #[allow(clippy::too_many_arguments)] // la récursion a besoin de tout le contexte
    fn apply_at(
        &self,
        lookup: &Lookup<'a>,
        mask: u32,
        buffer: &mut Vec<GlyphInfo>,
        gdef: Option<&Gdef<'a>>,
        position: usize,
        depth: u8,
    ) -> usize {
        for (kind, subtable) in &lookup.subtables {
            let advance = match kind {
                1 => simple(subtable, buffer, gdef, position),
                2 => multiple(subtable, buffer, gdef, position),
                3 => alternate(subtable, buffer, gdef, position),
                4 => ligature(subtable, lookup, buffer, gdef, position),
                5 | 6 => {
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

    /// Types 5 et 6 : reconnaissance puis application des tables imbriquées.
    #[allow(clippy::too_many_arguments)] // la récursion a besoin de tout le contexte
    fn contextual(
        &self,
        kind: u16,
        subtable: &'a [u8],
        lookup: &Lookup<'a>,
        mask: u32,
        buffer: &mut Vec<GlyphInfo>,
        gdef: Option<&Gdef<'a>>,
        position: usize,
        depth: u8,
    ) -> Option<usize> {
        let found = {
            let skip = SkipList::new(buffer, lookup, gdef);
            if kind == 5 {
                match_context(subtable, &skip, position)?
            } else {
                match_chain_context(subtable, &skip, position)?
            }
        };
        Some(self.apply_records(&found, mask, buffer, gdef, depth))
    }

    /// Applique une table de recherche **à une seule position**, comme le
    /// demandent les enregistrements `SequenceLookupRecord`.
    #[allow(clippy::too_many_arguments)] // la récursion a besoin de tout le contexte
    fn apply_one(
        &self,
        index: u16,
        mask: u32,
        buffer: &mut Vec<GlyphInfo>,
        gdef: Option<&Gdef<'a>>,
        position: usize,
        depth: u8,
    ) {
        if depth > MAX_NESTING {
            return;
        }
        let Some(lookup) = self.lookups.get(usize::from(index)) else {
            return;
        };
        let applies = buffer
            .get(position)
            .is_some_and(|g| !skipped(lookup, gdef, g.gid));
        if !applies {
            return;
        }
        let _ = self.apply_at(lookup, mask, buffer, gdef, position, depth);
    }

    /// Applique les tables imbriquées d'un motif reconnu et renvoie
    /// l'avance à consommer.
    fn apply_records(
        &self,
        found: &ContextMatch,
        mask: u32,
        buffer: &mut Vec<GlyphInfo>,
        gdef: Option<&Gdef<'a>>,
        depth: u8,
    ) -> usize {
        let start = found.input.first().copied().unwrap_or(0);
        let mut positions = found.input.clone();
        for record in &found.records {
            let Some(&target) = positions.get(usize::from(record.sequence_index)) else {
                continue;
            };
            let length_before = buffer.len();
            self.apply_one(record.lookup_index, mask, buffer, gdef, target, depth + 1);
            let after = isize::try_from(buffer.len()).unwrap_or(0);
            let before = isize::try_from(length_before).unwrap_or(0);
            let delta = after - before;
            if delta != 0 {
                for p in &mut positions {
                    if *p > target {
                        let moved = isize::try_from(*p).unwrap_or(0) + delta;
                        *p = usize::try_from(moved).unwrap_or(*p);
                    }
                }
            }
        }
        let last = positions.last().copied().unwrap_or(start).min(buffer.len());
        last.saturating_sub(start) + 1
    }

    /// Type 8 : substitution simple inverse, appliquée de la fin vers le
    /// début (une substitution peut dépendre de ce qui a déjà été substitué
    /// à sa droite).
    fn apply_reverse(
        lookup: &Lookup<'a>,
        mask: u32,
        buffer: &mut [GlyphInfo],
        gdef: Option<&Gdef<'a>>,
    ) {
        for position in (0..buffer.len()).rev() {
            let Some(info) = buffer.get(position) else {
                continue;
            };
            if info.mask & mask == 0 || skipped(lookup, gdef, info.gid) {
                continue;
            }
            for (kind, subtable) in &lookup.subtables {
                if *kind != 8 {
                    continue;
                }
                if let Some(new_gid) = reverse_chain(subtable, lookup, &*buffer, gdef, position) {
                    if let Some(info) = buffer.get_mut(position) {
                        info.gid = new_gid;
                        info.glyph_class = gdef.map_or(0, |g| g.glyph_class(new_gid));
                    }
                    break;
                }
            }
        }
    }
}

/// Vrai si la table de recherche saute ce glyphe.
fn skipped(lookup: &Lookup<'_>, gdef: Option<&Gdef<'_>>, gid: u16) -> bool {
    crate::opentype::common::is_skipped(lookup, gdef, gid)
}

/// Remplace le glyphe en `position` et met à jour sa classe.
fn replace(buffer: &mut [GlyphInfo], gdef: Option<&Gdef<'_>>, position: usize, gid: u16) {
    if let Some(info) = buffer.get_mut(position) {
        info.gid = gid;
        info.glyph_class = gdef.map_or(0, |g| g.glyph_class(gid));
    }
}

/// Type 1 : substitution simple.
fn simple(
    subtable: &[u8],
    buffer: &mut [GlyphInfo],
    gdef: Option<&Gdef<'_>>,
    position: usize,
) -> Option<usize> {
    let gid = buffer.get(position)?.gid;
    let coverage = Coverage::new(subtable.get(usize::from(u16_at(subtable, 2)?)..)?);
    let index = coverage.index(gid)?;
    let new_gid = match u16_at(subtable, 0)? {
        1 => {
            let delta = u16_at(subtable, 4)?;
            gid.wrapping_add(delta)
        }
        2 => {
            let count = u16_at(subtable, 4)?;
            if index >= count {
                return None;
            }
            u16_at(subtable, 6 + usize::from(index) * 2)?
        }
        _ => return None,
    };
    replace(buffer, gdef, position, new_gid);
    Some(1)
}

/// Type 2 : substitution multiple (un glyphe en plusieurs).
fn multiple(
    subtable: &[u8],
    buffer: &mut Vec<GlyphInfo>,
    gdef: Option<&Gdef<'_>>,
    position: usize,
) -> Option<usize> {
    if u16_at(subtable, 0)? != 1 {
        return None;
    }
    let gid = buffer.get(position)?.gid;
    let coverage = Coverage::new(subtable.get(usize::from(u16_at(subtable, 2)?)..)?);
    let index = usize::from(coverage.index(gid)?);
    let count = usize::from(u16_at(subtable, 4)?);
    if index >= count {
        return None;
    }
    let sequence = usize::from(u16_at(subtable, 6 + index * 2)?);
    let glyph_count = usize::from(u16_at(subtable, sequence)?);
    if glyph_count == 0 || glyph_count > MAX_ITEMS {
        // Un `glyphCount` nul supprimerait le glyphe ; la spécification
        // l'interdit et les polices réelles ne le font pas.
        return None;
    }
    let mut glyphs = Vec::with_capacity(glyph_count);
    for i in 0..glyph_count {
        glyphs.push(u16_at(subtable, sequence + 2 + i * 2)?);
    }
    let template = *buffer.get(position)?;
    replace(buffer, gdef, position, glyphs[0]);
    for (i, gid) in glyphs.iter().enumerate().skip(1) {
        let mut info = template;
        info.gid = *gid;
        info.glyph_class = gdef.map_or(0, |g| g.glyph_class(*gid));
        buffer.insert(position + i, info);
    }
    Some(glyph_count)
}

/// Type 3 : substitution alternative (première variante).
fn alternate(
    subtable: &[u8],
    buffer: &mut [GlyphInfo],
    gdef: Option<&Gdef<'_>>,
    position: usize,
) -> Option<usize> {
    if u16_at(subtable, 0)? != 1 {
        return None;
    }
    let gid = buffer.get(position)?.gid;
    let coverage = Coverage::new(subtable.get(usize::from(u16_at(subtable, 2)?)..)?);
    let index = usize::from(coverage.index(gid)?);
    let count = usize::from(u16_at(subtable, 4)?);
    if index >= count {
        return None;
    }
    let set = usize::from(u16_at(subtable, 6 + index * 2)?);
    if u16_at(subtable, set)? == 0 {
        return None;
    }
    let new_gid = u16_at(subtable, set + 2)?;
    replace(buffer, gdef, position, new_gid);
    Some(1)
}

/// Type 4 : substitution de ligature.
fn ligature(
    subtable: &[u8],
    lookup: &Lookup<'_>,
    buffer: &mut Vec<GlyphInfo>,
    gdef: Option<&Gdef<'_>>,
    position: usize,
) -> Option<usize> {
    if u16_at(subtable, 0)? != 1 {
        return None;
    }
    let gid = buffer.get(position)?.gid;
    let coverage = Coverage::new(subtable.get(usize::from(u16_at(subtable, 2)?)..)?);
    let index = usize::from(coverage.index(gid)?);
    let set_count = usize::from(u16_at(subtable, 4)?);
    if index >= set_count {
        return None;
    }
    let set = usize::from(u16_at(subtable, 6 + index * 2)?);
    let count = usize::from(u16_at(subtable, set)?);
    // L'emprunt immuable du tampon se termine avec ce bloc : la fusion qui
    // suit a besoin d'un emprunt mutable.
    let found = {
        let skip = SkipList::new(buffer, lookup, gdef);
        let mut found = None;
        for i in 0..count.min(MAX_ITEMS) {
            let Some(offset) = u16_at(subtable, set + 2 + i * 2) else {
                break;
            };
            let record = set + usize::from(offset);
            if let Some(result) = try_ligature(subtable, record, &skip, position) {
                found = Some(result);
                break;
            }
        }
        found
    };
    let (new_gid, components) = found?;
    Some(form_ligature(buffer, gdef, &components, new_gid))
}

/// Essaie une ligature : renvoie le glyphe produit et les positions des
/// composants.
fn try_ligature(
    subtable: &[u8],
    record: usize,
    skip: &SkipList<'_, '_>,
    position: usize,
) -> Option<(u16, Vec<usize>)> {
    let new_gid = u16_at(subtable, record)?;
    let component_count = usize::from(u16_at(subtable, record + 2)?);
    if component_count == 0 || component_count > MAX_ITEMS {
        return None;
    }
    let mut positions = vec![position];
    let mut current = position;
    for i in 1..component_count {
        current = skip.next(current)?;
        if skip.gid(current)? != u16_at(subtable, record + 2 + i * 2)? {
            return None;
        }
        positions.push(current);
    }
    Some((new_gid, positions))
}

/// Compteur de ligatures, propre à un tampon : le premier identifiant libre
/// est celui qui suit le plus grand déjà utilisé.
fn next_lig_id(buffer: &[GlyphInfo]) -> u16 {
    buffer.iter().map(|g| g.lig_id).max().unwrap_or(0) + 1
}

/// Fusionne les composants en une ligature et renvoie l'avance.
///
/// Les glyphes **sautés** qui se trouvaient entre deux composants (les
/// marques, typiquement) sont conservés et reçoivent le numéro de composant
/// auquel ils appartiennent : c'est ce qui permet à `GPOS` type 5 de les
/// ancrer au bon endroit de la ligature.
fn form_ligature(
    buffer: &mut Vec<GlyphInfo>,
    gdef: Option<&Gdef<'_>>,
    components: &[usize],
    new_gid: u16,
) -> usize {
    let Some(&first) = components.first() else {
        return 0;
    };
    let lig_id = next_lig_id(buffer);
    let cluster = components
        .iter()
        .filter_map(|&p| buffer.get(p).map(|g| g.cluster))
        .min()
        .unwrap_or(0);
    // Marques intercalées : elles suivent le composant qui les précède.
    for (rank, window) in components.windows(2).enumerate() {
        for position in window[0] + 1..window[1] {
            if let Some(info) = buffer.get_mut(position) {
                info.lig_id = lig_id;
                info.lig_component = u16::try_from(rank + 1).unwrap_or(1);
            }
        }
    }
    // Marques après le dernier composant : elles appartiennent au dernier.
    let last = components.last().copied().unwrap_or(first);
    if let Some(info) = buffer.get_mut(first) {
        info.gid = new_gid;
        info.cluster = cluster;
        info.lig_id = lig_id;
        info.lig_component = 0;
        info.glyph_class = gdef.map_or(0, |g| g.glyph_class(new_gid));
    }
    let component_count = u16::try_from(components.len()).unwrap_or(1);
    for position in (first + 1..=last).rev() {
        if components.contains(&position) {
            buffer.remove(position);
        } else if let Some(info) = buffer.get_mut(position) {
            info.lig_component = info.lig_component.max(1).min(component_count);
        }
    }
    1
}

/// Type 8 : substitution simple inverse enchaînée.
fn reverse_chain(
    subtable: &[u8],
    lookup: &Lookup<'_>,
    buffer: &[GlyphInfo],
    gdef: Option<&Gdef<'_>>,
    position: usize,
) -> Option<u16> {
    if u16_at(subtable, 0)? != 1 {
        return None;
    }
    let gid = buffer.get(position)?.gid;
    let coverage = Coverage::new(subtable.get(usize::from(u16_at(subtable, 2)?)..)?);
    let index = usize::from(coverage.index(gid)?);
    let skip = SkipList::new(buffer, lookup, gdef);

    let mut offset = 4usize;
    let backtrack_count = usize::from(u16_at(subtable, offset)?);
    if backtrack_count > MAX_ITEMS {
        return None;
    }
    let mut current = position;
    for i in 0..backtrack_count {
        let table = usize::from(u16_at(subtable, offset + 2 + i * 2)?);
        current = skip.previous(current)?;
        if !Coverage::new(subtable.get(table..)?).contains(skip.gid(current)?) {
            return None;
        }
    }
    offset += 2 + backtrack_count * 2;
    let lookahead_count = usize::from(u16_at(subtable, offset)?);
    if lookahead_count > MAX_ITEMS {
        return None;
    }
    let mut current = position;
    for i in 0..lookahead_count {
        let table = usize::from(u16_at(subtable, offset + 2 + i * 2)?);
        current = skip.next(current)?;
        if !Coverage::new(subtable.get(table..)?).contains(skip.gid(current)?) {
            return None;
        }
    }
    offset += 2 + lookahead_count * 2;
    let glyph_count = usize::from(u16_at(subtable, offset)?);
    if index >= glyph_count {
        return None;
    }
    u16_at(subtable, offset + 2 + index * 2)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod tests {
    use super::*;
    use crate::opentype::common::tests::coverage_format1;

    /// Sous-table de substitution simple format 2.
    pub(crate) fn single_format2(from: &[u16], to: &[u16]) -> Vec<u8> {
        let coverage = coverage_format1(from);
        let offset = 6 + to.len() * 2;
        let mut t = Vec::new();
        t.extend_from_slice(&2u16.to_be_bytes());
        t.extend_from_slice(&u16::try_from(offset).unwrap().to_be_bytes());
        t.extend_from_slice(&u16::try_from(to.len()).unwrap().to_be_bytes());
        for g in to {
            t.extend_from_slice(&g.to_be_bytes());
        }
        t.extend_from_slice(&coverage);
        t
    }

    /// Sous-table de ligature format 1 : une seule ligature.
    pub(crate) fn ligature_subtable(components: &[u16], result: u16) -> Vec<u8> {
        let mut ligature = Vec::new();
        ligature.extend_from_slice(&result.to_be_bytes());
        ligature.extend_from_slice(&u16::try_from(components.len()).unwrap().to_be_bytes());
        for c in &components[1..] {
            ligature.extend_from_slice(&c.to_be_bytes());
        }
        // LigatureSet : compte puis un décalage.
        let mut set = Vec::new();
        set.extend_from_slice(&1u16.to_be_bytes());
        set.extend_from_slice(&4u16.to_be_bytes());
        set.extend_from_slice(&ligature);

        let coverage = coverage_format1(&components[..1]);
        let header = 8;
        let set_offset = header + coverage.len();
        let mut t = Vec::new();
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&u16::try_from(header).unwrap().to_be_bytes());
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&u16::try_from(set_offset).unwrap().to_be_bytes());
        t.extend_from_slice(&coverage);
        t.extend_from_slice(&set);
        t
    }

    fn lookup(kind: u16, subtable: &[u8]) -> Lookup<'_> {
        Lookup {
            kind,
            flags: 0,
            mark_filtering_set: None,
            subtables: vec![(kind, subtable)],
        }
    }

    fn buffer(gids: &[u16]) -> Vec<GlyphInfo> {
        gids.iter()
            .enumerate()
            .map(|(i, g)| GlyphInfo::new(*g, u32::try_from(i).unwrap(), 1))
            .collect()
    }

    #[test]
    fn single_substitution_format1() {
        let coverage = coverage_format1(&[10, 11]);
        let mut t = Vec::new();
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&6u16.to_be_bytes());
        t.extend_from_slice(&5u16.to_be_bytes());
        t.extend_from_slice(&coverage);
        let mut b = buffer(&[10, 12, 11]);
        assert_eq!(simple(&t, &mut b, None, 0), Some(1));
        assert_eq!(simple(&t, &mut b, None, 1), None);
        assert_eq!(simple(&t, &mut b, None, 2), Some(1));
        assert_eq!(
            b.iter().map(|g| g.gid).collect::<Vec<_>>(),
            vec![15, 12, 16]
        );
    }

    #[test]
    fn single_substitution_format2() {
        let t = single_format2(&[10, 11], &[100, 101]);
        let mut b = buffer(&[10, 11]);
        assert_eq!(simple(&t, &mut b, None, 0), Some(1));
        assert_eq!(simple(&t, &mut b, None, 1), Some(1));
        assert_eq!(b.iter().map(|g| g.gid).collect::<Vec<_>>(), vec![100, 101]);
    }

    #[test]
    fn multiple_substitution_splits_one_glyph() {
        let coverage = coverage_format1(&[10]);
        let header = 8;
        let sequence_offset = header + coverage.len();
        let mut t = Vec::new();
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&u16::try_from(header).unwrap().to_be_bytes());
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&u16::try_from(sequence_offset).unwrap().to_be_bytes());
        t.extend_from_slice(&coverage);
        t.extend_from_slice(&3u16.to_be_bytes());
        for g in [20u16, 21, 22] {
            t.extend_from_slice(&g.to_be_bytes());
        }
        let mut b = buffer(&[10, 99]);
        assert_eq!(multiple(&t, &mut b, None, 0), Some(3));
        assert_eq!(
            b.iter().map(|g| g.gid).collect::<Vec<_>>(),
            vec![20, 21, 22, 99]
        );
        // Les trois glyphes produits gardent la grappe d'origine.
        assert!(b[..3].iter().all(|g| g.cluster == 0));
    }

    #[test]
    fn alternate_takes_the_first_variant() {
        let coverage = coverage_format1(&[10]);
        let header = 8;
        let set_offset = header + coverage.len();
        let mut t = Vec::new();
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&u16::try_from(header).unwrap().to_be_bytes());
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&u16::try_from(set_offset).unwrap().to_be_bytes());
        t.extend_from_slice(&coverage);
        t.extend_from_slice(&2u16.to_be_bytes());
        t.extend_from_slice(&77u16.to_be_bytes());
        t.extend_from_slice(&78u16.to_be_bytes());
        let mut b = buffer(&[10]);
        assert_eq!(alternate(&t, &mut b, None, 0), Some(1));
        assert_eq!(b[0].gid, 77);
    }

    #[test]
    fn ligature_merges_components() {
        let t = ligature_subtable(&[10, 11], 200);
        let l = lookup(4, &t);
        let mut b = buffer(&[10, 11, 12]);
        assert_eq!(ligature(&t, &l, &mut b, None, 0), Some(1));
        assert_eq!(b.iter().map(|g| g.gid).collect::<Vec<_>>(), vec![200, 12]);
        assert_eq!(b[0].cluster, 0);
        assert_eq!(b[0].lig_id, 1);
        // Sans les bons composants, rien ne se passe.
        let mut b = buffer(&[10, 99]);
        assert_eq!(ligature(&t, &l, &mut b, None, 0), None);
    }

    /// Une ligature de trois composants marche aussi.
    #[test]
    fn three_component_ligature() {
        let t = ligature_subtable(&[1, 2, 3], 300);
        let l = lookup(4, &t);
        let mut b = buffer(&[1, 2, 3, 4]);
        assert_eq!(ligature(&t, &l, &mut b, None, 0), Some(1));
        assert_eq!(b.iter().map(|g| g.gid).collect::<Vec<_>>(), vec![300, 4]);
    }

    #[test]
    fn corrupt_subtables_are_ignored() {
        let mut b = buffer(&[1, 2]);
        assert_eq!(simple(&[], &mut b, None, 0), None);
        assert_eq!(simple(&[0, 9, 0, 4], &mut b, None, 0), None);
        assert_eq!(multiple(&[0, 2], &mut b, None, 0), None);
        assert_eq!(alternate(&[0, 2], &mut b, None, 0), None);
        assert!(Gsub::parse(&[0, 1]).is_none());
    }
}
