//! Tampon de glyphes sur lequel travaillent `GSUB` et `GPOS`.
//!
//! Le shaper part d'une suite de caractères, la convertit en glyphes, puis
//! applique des tables de recherche qui **remplacent** des glyphes (`GSUB`)
//! et **déplacent** des glyphes (`GPOS`). Le tampon porte, pour chaque
//! glyphe, tout ce dont ces tables ont besoin.

use crate::opentype::common::{Gdef, Lookup};

/// Nature de l'attachement d'un glyphe à un autre (GPOS 3 à 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AttachKind {
    /// Aucun attachement.
    #[default]
    None,
    /// Marque attachée à une base, une ligature ou une autre marque.
    Mark,
    /// Liaison cursive (GPOS 3).
    Cursive,
}

/// Un glyphe et tout son contexte pendant la composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphInfo {
    /// Identifiant de glyphe dans la police.
    pub gid: u16,
    /// Indice d'octet, dans la chaîne d'origine, de la grappe dont ce
    /// glyphe fait partie.
    pub cluster: u32,
    /// Masque des fonctionnalités actives sur ce glyphe.
    pub mask: u32,
    /// Classe `GDEF` (0 inconnue, 1 base, 2 ligature, 3 marque, 4 composant).
    pub glyph_class: u16,
    /// Identifiant de la ligature à laquelle ce glyphe appartient (0 : aucune).
    pub lig_id: u16,
    /// Rang du composant dans la ligature (1 pour le premier).
    pub lig_component: u16,
    /// Avance horizontale, en unités de police.
    pub x_advance: i32,
    /// Avance verticale, en unités de police.
    pub y_advance: i32,
    /// Déplacement horizontal du dessin.
    pub x_offset: i32,
    /// Déplacement vertical du dessin.
    pub y_offset: i32,
    /// Décalage **relatif** vers le glyphe auquel celui-ci est attaché.
    pub attach_chain: i32,
    /// Nature de l'attachement.
    pub attach_kind: AttachKind,
}

impl GlyphInfo {
    /// Glyphe nu, sans ajustement ni attachement.
    #[must_use]
    pub const fn new(gid: u16, cluster: u32, mask: u32) -> Self {
        Self {
            gid,
            cluster,
            mask,
            glyph_class: 0,
            lig_id: 0,
            lig_component: 0,
            x_advance: 0,
            y_advance: 0,
            x_offset: 0,
            y_offset: 0,
            attach_chain: 0,
            attach_kind: AttachKind::None,
        }
    }

    /// Vrai si `GDEF` classe ce glyphe comme une marque.
    #[must_use]
    pub const fn is_mark(&self) -> bool {
        self.glyph_class == crate::opentype::common::GLYPH_CLASS_MARK
    }
}

/// Recalcule la classe `GDEF` de chaque glyphe du tampon.
pub fn refresh_classes(buffer: &mut [GlyphInfo], gdef: Option<&Gdef<'_>>) {
    let Some(gdef) = gdef else {
        return;
    };
    for info in buffer.iter_mut() {
        info.glyph_class = gdef.glyph_class(info.gid);
    }
}

/// Parcours du tampon en sautant les glyphes ignorés par une table de
/// recherche (drapeaux `lookupFlag` et table `GDEF`).
#[derive(Debug, Clone, Copy)]
pub struct SkipList<'a, 'b> {
    buffer: &'a [GlyphInfo],
    lookup: &'a Lookup<'b>,
    gdef: Option<&'a Gdef<'b>>,
}

impl<'a, 'b> SkipList<'a, 'b> {
    /// Nouveau parcours.
    #[must_use]
    pub const fn new(
        buffer: &'a [GlyphInfo],
        lookup: &'a Lookup<'b>,
        gdef: Option<&'a Gdef<'b>>,
    ) -> Self {
        Self {
            buffer,
            lookup,
            gdef,
        }
    }

    /// Identifiant du glyphe en `index`.
    #[must_use]
    pub fn gid(&self, index: usize) -> Option<u16> {
        self.buffer.get(index).map(|g| g.gid)
    }

    /// Vrai si le glyphe en `index` est sauté.
    #[must_use]
    pub fn is_skipped(&self, index: usize) -> bool {
        self.buffer
            .get(index)
            .is_some_and(|g| crate::opentype::common::is_skipped(self.lookup, self.gdef, g.gid))
    }

    /// Indice du glyphe non sauté qui suit `index` (exclu).
    #[must_use]
    pub fn next(&self, index: usize) -> Option<usize> {
        (index + 1..self.buffer.len()).find(|&i| !self.is_skipped(i))
    }

    /// Indice du glyphe non sauté qui précède `index` (exclu).
    #[must_use]
    pub fn previous(&self, index: usize) -> Option<usize> {
        (0..index).rev().find(|&i| !self.is_skipped(i))
    }

    /// Les `count` indices non sautés à partir de `start` inclus, ou `None`
    /// s'il n'y en a pas assez.
    #[must_use]
    pub fn forward(&self, start: usize, count: usize) -> Option<Vec<usize>> {
        let mut out = Vec::with_capacity(count);
        let mut i = start;
        while out.len() < count {
            if i >= self.buffer.len() {
                return None;
            }
            if !self.is_skipped(i) {
                out.push(i);
            }
            i += 1;
        }
        Some(out)
    }

    /// Les `count` indices non sautés avant `start` (exclu), du plus proche
    /// au plus lointain, ou `None` s'il n'y en a pas assez.
    #[must_use]
    pub fn backward(&self, start: usize, count: usize) -> Option<Vec<usize>> {
        let mut out = Vec::with_capacity(count);
        let mut i = start;
        while out.len() < count {
            i = i.checked_sub(1)?;
            if !self.is_skipped(i) {
                out.push(i);
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opentype::common::LOOKUP_IGNORE_MARKS;

    fn lookup(flags: u16) -> Lookup<'static> {
        Lookup {
            kind: 1,
            flags,
            mark_filtering_set: None,
            subtables: Vec::new(),
        }
    }

    #[test]
    fn glyph_info_defaults() {
        let g = GlyphInfo::new(7, 3, 1);
        assert_eq!(g.gid, 7);
        assert_eq!(g.cluster, 3);
        assert_eq!(g.mask, 1);
        assert!(!g.is_mark());
        assert_eq!(g.attach_kind, AttachKind::None);
    }

    /// Sans `GDEF`, rien n'est sauté même avec le drapeau « ignorer les
    /// marques ».
    #[test]
    fn nothing_is_skipped_without_gdef() {
        let buffer: Vec<GlyphInfo> = (0..4).map(|i| GlyphInfo::new(i, i.into(), 1)).collect();
        let l = lookup(LOOKUP_IGNORE_MARKS);
        let skip = SkipList::new(&buffer, &l, None);
        assert_eq!(skip.next(0), Some(1));
        assert_eq!(skip.previous(2), Some(1));
        assert_eq!(skip.forward(0, 3), Some(vec![0, 1, 2]));
        assert_eq!(skip.backward(3, 2), Some(vec![2, 1]));
        assert_eq!(skip.forward(2, 5), None);
        assert_eq!(skip.backward(1, 2), None);
    }
}
