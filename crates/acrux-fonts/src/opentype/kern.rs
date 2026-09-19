//! Table `kern` ancienne (TrueType) : crénage par paires, format 0.
//!
//! Avant `GPOS`, le crénage vivait dans la table `kern`. Beaucoup de polices
//! anciennes — et quelques polices récentes destinées à des logiciels qui
//! ne lisent pas `GPOS` — ne proposent que celle-ci. Nous ne l'utilisons
//! qu'en **repli**, quand `GPOS` est absent ou ne fournit aucune
//! fonctionnalité `kern` pour le script demandé.
//!
//! ## Ce que nous lisons
//!
//! - L'en-tête **Microsoft** (`version` 0 sur 16 bits, `nTables` sur 16
//!   bits) et l'en-tête **Apple** (`version` 1.0 sur 32 bits, `nTables` sur
//!   32 bits) ; les deux existent dans la nature.
//! - Les sous-tables de **format 0** (liste de paires triée), horizontales
//!   et non perpendiculaires.
//! - Le drapeau `override` (bit 3 du coverage Microsoft) : la sous-table
//!   remplace le cumul au lieu de s'y ajouter.
//!
//! ## Ce que nous refusons
//!
//! Les formats 1 (machine à états, Apple), 2 (tableau de classes) et 3
//! (Apple, compact) : ils sont rarissimes et le format 2 est de toute façon
//! supplanté par `GPOS` type 2 format 2. Une sous-table d'un format inconnu
//! est sautée, jamais interprétée de travers.

use crate::reader::{u16_at, Reader};

/// Nombre maximal de sous-tables lues.
const MAX_SUBTABLES: usize = 32;

/// Une sous-table de crénage de format 0, prête à être interrogée.
#[derive(Debug, Clone, Copy)]
struct Format0<'a> {
    pairs: &'a [u8],
    count: usize,
    /// Vrai si la sous-table remplace le cumul au lieu de s'y ajouter.
    overrides: bool,
}

impl Format0<'_> {
    /// Valeur de crénage d'une paire, en unités de police.
    fn value(&self, left: u16, right: u16) -> Option<i16> {
        let key = (u32::from(left) << 16) | u32::from(right);
        let (mut lo, mut hi) = (0usize, self.count);
        while lo < hi {
            let mid = lo.midpoint(hi);
            let offset = mid * 6;
            let candidate = (u32::from(u16_at(self.pairs, offset)?) << 16)
                | u32::from(u16_at(self.pairs, offset + 2)?);
            match key.cmp(&candidate) {
                core::cmp::Ordering::Less => hi = mid,
                core::cmp::Ordering::Greater => lo = mid + 1,
                core::cmp::Ordering::Equal => return crate::reader::i16_at(self.pairs, offset + 4),
            }
        }
        None
    }
}

/// Table `kern` analysée.
#[derive(Debug, Clone, Default)]
pub struct KernTable<'a> {
    subtables: Vec<Format0<'a>>,
}

impl<'a> KernTable<'a> {
    /// Analyse la table ; renvoie `None` si aucune sous-table exploitable.
    #[must_use]
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let mut r = Reader::new(data);
        let version = r.read_u16()?;
        let (count, mut offset) = if version == 0 {
            (usize::from(r.read_u16()?), 4usize)
        } else if version == 1 {
            // En-tête Apple : version 1.0 en 16.16, puis nTables sur 32 bits.
            let _low = r.read_u16()?;
            (usize::try_from(r.read_u32()?).ok()?, 8usize)
        } else {
            return None;
        };
        let mut subtables = Vec::new();
        for _ in 0..count.min(MAX_SUBTABLES) {
            let Some(parsed) = Self::parse_subtable(data, offset, version) else {
                break;
            };
            let (subtable, length) = parsed;
            if let Some(subtable) = subtable {
                subtables.push(subtable);
            }
            if length == 0 {
                break;
            }
            offset = offset.checked_add(length)?;
        }
        (!subtables.is_empty()).then_some(Self { subtables })
    }

    /// Lit une sous-table ; renvoie la sous-table (si elle est utilisable)
    /// et sa longueur totale en octets.
    fn parse_subtable(
        data: &'a [u8],
        offset: usize,
        version: u16,
    ) -> Option<(Option<Format0<'a>>, usize)> {
        // Microsoft : version u16, length u16, coverage u16.
        // Apple : length u32, coverage u8, format u8, tupleIndex u16.
        let (length, format, horizontal, cross, overrides, header) = if version == 0 {
            let length = usize::from(u16_at(data, offset + 2)?);
            let coverage = u16_at(data, offset + 4)?;
            (
                length,
                coverage >> 8,
                coverage & 0x0001 != 0,
                coverage & 0x0004 != 0,
                coverage & 0x0008 != 0,
                6usize,
            )
        } else {
            let length = usize::try_from(crate::reader::u32_at(data, offset)?).ok()?;
            let coverage = u16_at(data, offset + 4)?;
            (
                length,
                coverage & 0x00FF,
                coverage & 0x8000 == 0,
                coverage & 0x4000 != 0,
                false,
                8usize,
            )
        };
        if format != 0 || !horizontal || cross {
            return Some((None, length));
        }
        let body = offset + header;
        let count = usize::from(u16_at(data, body)?);
        let pairs = data.get(body + 8..body + 8 + count * 6)?;
        Some((
            Some(Format0 {
                pairs,
                count,
                overrides,
            }),
            length,
        ))
    }

    /// Crénage à appliquer entre deux glyphes, en unités de police.
    ///
    /// Les sous-tables cumulatives s'additionnent ; une sous-table marquée
    /// `override` efface ce qui précède.
    #[must_use]
    pub fn kerning(&self, left: u16, right: u16) -> i32 {
        let mut total = 0i32;
        for subtable in &self.subtables {
            if let Some(value) = subtable.value(left, right) {
                if subtable.overrides {
                    total = i32::from(value);
                } else {
                    total += i32::from(value);
                }
            }
        }
        total
    }

    /// Vrai si la table contient au moins une paire exploitable.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.subtables.iter().any(|s| s.count > 0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Fabrique une table `kern` Microsoft avec une sous-table format 0.
    fn kern_table(pairs: &[(u16, u16, i16)]) -> Vec<u8> {
        let mut sub = Vec::new();
        sub.extend_from_slice(&0u16.to_be_bytes()); // version de sous-table
        sub.extend_from_slice(&u16::try_from(14 + pairs.len() * 6).unwrap().to_be_bytes());
        sub.extend_from_slice(&1u16.to_be_bytes()); // coverage : horizontal
        sub.extend_from_slice(&u16::try_from(pairs.len()).unwrap().to_be_bytes());
        sub.extend_from_slice(&0u16.to_be_bytes()); // searchRange
        sub.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
        sub.extend_from_slice(&0u16.to_be_bytes()); // rangeShift
        for (l, r, v) in pairs {
            sub.extend_from_slice(&l.to_be_bytes());
            sub.extend_from_slice(&r.to_be_bytes());
            sub.extend_from_slice(&v.to_be_bytes());
        }
        let mut t = Vec::new();
        t.extend_from_slice(&0u16.to_be_bytes());
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&sub);
        t
    }

    #[test]
    fn reads_format0_pairs() {
        let data = kern_table(&[(10, 20, -50), (10, 21, -30), (11, 20, 40)]);
        let table = KernTable::parse(&data).unwrap();
        assert!(table.is_usable());
        assert_eq!(table.kerning(10, 20), -50);
        assert_eq!(table.kerning(10, 21), -30);
        assert_eq!(table.kerning(11, 20), 40);
        assert_eq!(table.kerning(10, 99), 0);
    }

    #[test]
    fn rejects_unknown_headers() {
        assert!(KernTable::parse(&[]).is_none());
        assert!(KernTable::parse(&[0, 7, 0, 1]).is_none());
        // Table annoncée mais tronquée : aucune sous-table utilisable.
        assert!(KernTable::parse(&[0, 0, 0, 1]).is_none());
    }

    /// Une sous-table de format inconnu est sautée sans casser la suivante.
    #[test]
    fn skips_unknown_formats() {
        let mut data = kern_table(&[(1, 2, -10)]);
        // Insère devant une sous-table de format 2, longueur 8.
        let mut prefix = Vec::new();
        prefix.extend_from_slice(&0u16.to_be_bytes());
        prefix.extend_from_slice(&2u16.to_be_bytes()); // nTables = 2
        prefix.extend_from_slice(&0u16.to_be_bytes());
        prefix.extend_from_slice(&8u16.to_be_bytes()); // length
        prefix.extend_from_slice(&0x0201u16.to_be_bytes()); // format 2, horizontal
        prefix.extend_from_slice(&0u16.to_be_bytes());
        prefix.extend_from_slice(&data[4..]);
        data = prefix;
        let table = KernTable::parse(&data).unwrap();
        assert_eq!(table.kerning(1, 2), -10);
    }
}
