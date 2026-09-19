//! Structures communes à `GSUB` et `GPOS` (OpenType, « OpenType Layout
//! Common Table Formats »).
//!
//! Les deux tables partagent exactement le même squelette :
//!
//! ```text
//! ScriptList  → Script → LangSys → indices de fonctionnalités
//! FeatureList → Feature → indices de tables de recherche
//! LookupList  → Lookup → sous-tables
//! ```
//!
//! Tout est lu **sans copie** : chaque structure n'est qu'une tranche
//! d'octets et un décalage, et chaque lecture est bornée (une police
//! corrompue donne `None`, jamais une panique).

use core::cmp::Ordering;

use crate::reader::{u16_at, Reader};

/// Nombre maximal d'entrées lues dans une liste (borne contre les polices
/// hostiles qui annoncent 65 535 scripts).
const MAX_ENTRIES: usize = 4096;

/// Table de couverture : l'ensemble des glyphes auxquels une sous-table
/// s'applique, avec leur rang.
///
/// Format 1 : liste triée de glyphes. Format 2 : plages triées avec le rang
/// de départ.
#[derive(Debug, Clone, Copy)]
pub struct Coverage<'a> {
    data: &'a [u8],
}

impl<'a> Coverage<'a> {
    /// Vue sur une table de couverture commençant au début de `data`.
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    /// Rang du glyphe dans la couverture, ou `None` s'il n'y figure pas.
    #[must_use]
    pub fn index(&self, gid: u16) -> Option<u16> {
        match u16_at(self.data, 0)? {
            1 => self.index_format1(gid),
            2 => self.index_format2(gid),
            _ => None,
        }
    }

    /// Format 1 : recherche dichotomique dans le tableau de glyphes.
    fn index_format1(&self, gid: u16) -> Option<u16> {
        let count = usize::from(u16_at(self.data, 2)?);
        let (mut lo, mut hi) = (0usize, count.min(MAX_ENTRIES * 16));
        while lo < hi {
            let mid = lo.midpoint(hi);
            let value = u16_at(self.data, 4 + mid * 2)?;
            match gid.cmp(&value) {
                Ordering::Less => hi = mid,
                Ordering::Greater => lo = mid + 1,
                Ordering::Equal => return u16::try_from(mid).ok(),
            }
        }
        None
    }

    /// Format 2 : recherche dichotomique dans les plages.
    fn index_format2(&self, gid: u16) -> Option<u16> {
        let count = usize::from(u16_at(self.data, 2)?);
        let (mut lo, mut hi) = (0usize, count.min(MAX_ENTRIES * 16));
        while lo < hi {
            let mid = lo.midpoint(hi);
            let base = 4 + mid * 6;
            let start = u16_at(self.data, base)?;
            let end = u16_at(self.data, base + 2)?;
            if gid < start {
                hi = mid;
            } else if gid > end {
                lo = mid + 1;
            } else {
                let first = u16_at(self.data, base + 4)?;
                return Some(first.wrapping_add(gid.wrapping_sub(start)));
            }
        }
        None
    }

    /// Vrai si le glyphe est couvert.
    #[must_use]
    pub fn contains(&self, gid: u16) -> bool {
        self.index(gid).is_some()
    }

    /// Tous les glyphes couverts, dans l'ordre des rangs.
    ///
    /// Utilisé par les substitutions inverses et par les tests ; la liste est
    /// bornée pour rester sûre sur une police corrompue.
    #[must_use]
    pub fn glyphs(&self) -> Vec<u16> {
        let mut out = Vec::new();
        let Some(format) = u16_at(self.data, 0) else {
            return out;
        };
        let count = u16_at(self.data, 2).map_or(0, usize::from);
        match format {
            1 => {
                for i in 0..count.min(MAX_ENTRIES * 16) {
                    if let Some(g) = u16_at(self.data, 4 + i * 2) {
                        out.push(g);
                    }
                }
            }
            2 => {
                for i in 0..count.min(MAX_ENTRIES) {
                    let base = 4 + i * 6;
                    let (Some(start), Some(end)) =
                        (u16_at(self.data, base), u16_at(self.data, base + 2))
                    else {
                        break;
                    };
                    for g in start..=end {
                        out.push(g);
                        if out.len() > MAX_ENTRIES * 16 {
                            return out;
                        }
                    }
                }
            }
            _ => {}
        }
        out
    }
}

/// Table de définition de classes : associe une classe (0 par défaut) à
/// chaque glyphe. Formats 1 (plage contiguë) et 2 (plages).
#[derive(Debug, Clone, Copy)]
pub struct ClassDef<'a> {
    data: &'a [u8],
}

impl<'a> ClassDef<'a> {
    /// Vue sur une table de classes commençant au début de `data`.
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    /// Classe d'un glyphe ; 0 si le glyphe n'est pas listé.
    #[must_use]
    pub fn class(&self, gid: u16) -> u16 {
        self.try_class(gid).unwrap_or(0)
    }

    fn try_class(&self, gid: u16) -> Option<u16> {
        match u16_at(self.data, 0)? {
            1 => {
                let start = u16_at(self.data, 2)?;
                let count = u16_at(self.data, 4)?;
                let offset = gid.checked_sub(start)?;
                if offset >= count {
                    return None;
                }
                u16_at(self.data, 6 + usize::from(offset) * 2)
            }
            2 => {
                let count = usize::from(u16_at(self.data, 2)?);
                let (mut lo, mut hi) = (0usize, count.min(MAX_ENTRIES * 16));
                while lo < hi {
                    let mid = lo.midpoint(hi);
                    let base = 4 + mid * 6;
                    let start = u16_at(self.data, base)?;
                    let end = u16_at(self.data, base + 2)?;
                    if gid < start {
                        hi = mid;
                    } else if gid > end {
                        lo = mid + 1;
                    } else {
                        return u16_at(self.data, base + 4);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Plus grande classe définie (utile pour dimensionner une matrice de
    /// paires format 2).
    #[must_use]
    pub fn max_class(&self) -> u16 {
        let Some(format) = u16_at(self.data, 0) else {
            return 0;
        };
        let mut max = 0;
        match format {
            1 => {
                let count = u16_at(self.data, 4).map_or(0, usize::from);
                for i in 0..count.min(MAX_ENTRIES * 16) {
                    if let Some(c) = u16_at(self.data, 6 + i * 2) {
                        max = max.max(c);
                    }
                }
            }
            2 => {
                let count = u16_at(self.data, 2).map_or(0, usize::from);
                for i in 0..count.min(MAX_ENTRIES * 16) {
                    if let Some(c) = u16_at(self.data, 4 + i * 6 + 4) {
                        max = max.max(c);
                    }
                }
            }
            _ => {}
        }
        max
    }
}

// Drapeaux de `valueFormat` (GPOS, « Value Record »).
/// Déplacement horizontal du glyphe.
pub const VALUE_X_PLACEMENT: u16 = 0x0001;
/// Déplacement vertical du glyphe.
pub const VALUE_Y_PLACEMENT: u16 = 0x0002;
/// Correction de l'avance horizontale.
pub const VALUE_X_ADVANCE: u16 = 0x0004;
/// Correction de l'avance verticale.
pub const VALUE_Y_ADVANCE: u16 = 0x0008;

/// Ajustement de position en unités de police (GPOS, « Value Record »).
///
/// Les quatre champs `Device` de la spécification (corrections par taille de
/// rendu et variations) sont **lus et sautés** : nous rendons avec de
/// l'anti-aliasing et sans hinting, ils n'auraient aucun effet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ValueRecord {
    /// Déplacement horizontal.
    pub x_placement: i16,
    /// Déplacement vertical.
    pub y_placement: i16,
    /// Correction de l'avance horizontale.
    pub x_advance: i16,
    /// Correction de l'avance verticale.
    pub y_advance: i16,
}

impl ValueRecord {
    /// Vrai si l'enregistrement ne change rien.
    #[must_use]
    pub const fn is_zero(&self) -> bool {
        self.x_placement == 0 && self.y_placement == 0 && self.x_advance == 0 && self.y_advance == 0
    }
}

/// Taille en octets d'un `ValueRecord` du format donné : deux octets par bit
/// à 1.
#[must_use]
pub fn value_record_size(format: u16) -> usize {
    usize::try_from(format.count_ones()).unwrap_or(0) * 2
}

/// Lit un `ValueRecord` à la position courante du lecteur.
///
/// Les champs absents du format valent zéro ; les décalages vers des tables
/// `Device` sont consommés puis ignorés.
pub fn read_value_record(r: &mut Reader<'_>, format: u16) -> ValueRecord {
    let mut value = ValueRecord::default();
    let mut read_if = |bit: u16, target: &mut i16| {
        if format & bit != 0 {
            *target = r.read_i16().unwrap_or(0);
        }
    };
    read_if(VALUE_X_PLACEMENT, &mut value.x_placement);
    read_if(VALUE_Y_PLACEMENT, &mut value.y_placement);
    read_if(VALUE_X_ADVANCE, &mut value.x_advance);
    read_if(VALUE_Y_ADVANCE, &mut value.y_advance);
    // Quatre décalages `Device` éventuels, volontairement ignorés.
    for bit in [0x0010u16, 0x0020, 0x0040, 0x0080] {
        if format & bit != 0 {
            let _ = r.read_u16();
        }
    }
    value
}

/// Point d'ancrage d'une marque ou d'une base, en unités de police.
///
/// Les trois formats existent ; le format 2 ajoute un numéro de point de
/// contour (ignoré, il suppose le hinting) et le format 3 des tables
/// `Device` (ignorées de même).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Anchor {
    /// Abscisse.
    pub x: i16,
    /// Ordonnée.
    pub y: i16,
}

/// Lit une table d'ancrage située à `offset` dans `data`.
#[must_use]
pub fn read_anchor(data: &[u8], offset: usize) -> Option<Anchor> {
    let mut r = Reader::at(data, offset);
    let format = r.read_u16()?;
    if !(1..=3).contains(&format) {
        return None;
    }
    let x = r.read_i16()?;
    let y = r.read_i16()?;
    Some(Anchor { x, y })
}

// Drapeaux d'une table de recherche (`lookupFlag`).
/// Inverse le sens du chaînage cursif (GPOS 3).
pub const LOOKUP_RIGHT_TO_LEFT: u16 = 0x0001;
/// Saute les glyphes de base.
pub const LOOKUP_IGNORE_BASE_GLYPHS: u16 = 0x0002;
/// Saute les ligatures.
pub const LOOKUP_IGNORE_LIGATURES: u16 = 0x0004;
/// Saute les marques.
pub const LOOKUP_IGNORE_MARKS: u16 = 0x0008;
/// Le champ `markFilteringSet` est présent.
pub const LOOKUP_USE_MARK_FILTERING_SET: u16 = 0x0010;
/// Masque de la classe d'attachement de marque (octet de poids fort).
pub const LOOKUP_MARK_ATTACHMENT_TYPE: u16 = 0xFF00;

/// Une table de recherche : son type, ses drapeaux et ses sous-tables.
#[derive(Debug, Clone)]
pub struct Lookup<'a> {
    /// Type de la table (sens différent en `GSUB` et en `GPOS`).
    pub kind: u16,
    /// Drapeaux `lookupFlag`.
    pub flags: u16,
    /// Indice du jeu de marques filtrant, si `LOOKUP_USE_MARK_FILTERING_SET`.
    pub mark_filtering_set: Option<u16>,
    /// Sous-tables, chacune vue comme une tranche commençant à son en-tête ;
    /// le type accompagne la tranche car une extension (GSUB 7, GPOS 9) le
    /// remplace.
    pub subtables: Vec<(u16, &'a [u8])>,
}

/// Squelette commun de `GSUB` et de `GPOS`.
#[derive(Debug, Clone)]
pub struct LayoutTable<'a> {
    data: &'a [u8],
    scripts: usize,
    features: usize,
    lookups: usize,
}

impl<'a> LayoutTable<'a> {
    /// Analyse l'en-tête (versions 1.0 et 1.1).
    ///
    /// Renvoie `None` si la table est tronquée ou si sa version majeure
    /// n'est pas 1.
    #[must_use]
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let _minor = r.read_u16()?;
        if major != 1 {
            return None;
        }
        let scripts = usize::from(r.read_u16()?);
        let features = usize::from(r.read_u16()?);
        let lookups = usize::from(r.read_u16()?);
        if scripts >= data.len() || features >= data.len() || lookups >= data.len() {
            return None;
        }
        Some(Self {
            data,
            scripts,
            features,
            lookups,
        })
    }

    /// Octets de la table.
    #[must_use]
    pub const fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Tags de tous les scripts déclarés.
    #[must_use]
    pub fn script_tags(&self) -> Vec<[u8; 4]> {
        let mut out = Vec::new();
        let count = u16_at(self.data, self.scripts).map_or(0, usize::from);
        for i in 0..count.min(MAX_ENTRIES) {
            let base = self.scripts + 2 + i * 6;
            if let Some(tag) = self.data.get(base..base + 4) {
                let mut t = [0u8; 4];
                t.copy_from_slice(tag);
                out.push(t);
            }
        }
        out
    }

    /// Décalage absolu de la table `Script` du tag donné.
    fn script_offset(&self, tag: [u8; 4]) -> Option<usize> {
        let count = u16_at(self.data, self.scripts).map_or(0, usize::from);
        for i in 0..count.min(MAX_ENTRIES) {
            let base = self.scripts + 2 + i * 6;
            if self.data.get(base..base + 4) == Some(&tag[..]) {
                let offset = usize::from(u16_at(self.data, base + 4)?);
                return Some(self.scripts + offset);
            }
        }
        None
    }

    /// Décalage absolu de la table `LangSys` demandée : la langue exacte,
    /// sinon la langue par défaut du script.
    fn lang_sys_offset(&self, script: usize, language: Option<[u8; 4]>) -> Option<usize> {
        if let Some(lang) = language {
            let count = u16_at(self.data, script + 2).map_or(0, usize::from);
            for i in 0..count.min(MAX_ENTRIES) {
                let base = script + 4 + i * 6;
                if self.data.get(base..base + 4) == Some(&lang[..]) {
                    let offset = usize::from(u16_at(self.data, base + 4)?);
                    return Some(script + offset);
                }
            }
        }
        let default = usize::from(u16_at(self.data, script)?);
        (default != 0).then_some(script + default)
    }

    /// Indices des fonctionnalités actives pour un script et une langue.
    ///
    /// Le script est cherché tel quel, puis `dflt`, puis `DFLT` ; la
    /// fonctionnalité obligatoire du `LangSys` (`requiredFeatureIndex`) est
    /// incluse.
    #[must_use]
    pub fn feature_indices(&self, script: [u8; 4], language: Option<[u8; 4]>) -> Vec<u16> {
        let script_offset = self
            .script_offset(script)
            .or_else(|| self.script_offset(*b"dflt"))
            .or_else(|| self.script_offset(*b"DFLT"));
        let Some(script_offset) = script_offset else {
            return Vec::new();
        };
        let Some(lang) = self.lang_sys_offset(script_offset, language) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Some(required) = u16_at(self.data, lang + 2) {
            if required != 0xFFFF {
                out.push(required);
            }
        }
        let count = u16_at(self.data, lang + 4).map_or(0, usize::from);
        for i in 0..count.min(MAX_ENTRIES) {
            if let Some(index) = u16_at(self.data, lang + 6 + i * 2) {
                out.push(index);
            }
        }
        out
    }

    /// Tag d'une fonctionnalité par son indice.
    #[must_use]
    pub fn feature_tag(&self, index: u16) -> Option<[u8; 4]> {
        let count = u16_at(self.data, self.features).map_or(0, usize::from);
        if usize::from(index) >= count {
            return None;
        }
        let base = self.features + 2 + usize::from(index) * 6;
        let bytes = self.data.get(base..base + 4)?;
        let mut tag = [0u8; 4];
        tag.copy_from_slice(bytes);
        Some(tag)
    }

    /// Indices des tables de recherche d'une fonctionnalité.
    #[must_use]
    pub fn feature_lookups(&self, index: u16) -> Vec<u16> {
        let mut out = Vec::new();
        let count = u16_at(self.data, self.features).map_or(0, usize::from);
        if usize::from(index) >= count {
            return out;
        }
        let base = self.features + 2 + usize::from(index) * 6;
        let Some(offset) = u16_at(self.data, base + 4) else {
            return out;
        };
        let feature = self.features + usize::from(offset);
        let lookup_count = u16_at(self.data, feature + 2).map_or(0, usize::from);
        for i in 0..lookup_count.min(MAX_ENTRIES) {
            if let Some(l) = u16_at(self.data, feature + 4 + i * 2) {
                out.push(l);
            }
        }
        out
    }

    /// Nombre de tables de recherche.
    #[must_use]
    pub fn lookup_count(&self) -> u16 {
        u16_at(self.data, self.lookups).unwrap_or(0)
    }

    /// Table de recherche par indice, extensions déjà résolues.
    #[must_use]
    pub fn lookup(&self, index: u16, extension_kind: u16) -> Option<Lookup<'a>> {
        let count = usize::from(self.lookup_count());
        if usize::from(index) >= count {
            return None;
        }
        let offset = usize::from(u16_at(
            self.data,
            self.lookups + 2 + usize::from(index) * 2,
        )?);
        let base = self.lookups.checked_add(offset)?;
        let kind = u16_at(self.data, base)?;
        let flags = u16_at(self.data, base + 2)?;
        let sub_count = usize::from(u16_at(self.data, base + 4)?);
        let mut subtables = Vec::new();
        for i in 0..sub_count.min(MAX_ENTRIES) {
            let Some(sub_offset) = u16_at(self.data, base + 6 + i * 2) else {
                break;
            };
            let Some(start) = base.checked_add(usize::from(sub_offset)) else {
                break;
            };
            let Some(slice) = self.data.get(start..) else {
                break;
            };
            if kind == extension_kind {
                if let Some((real_kind, real_slice)) = resolve_extension(slice) {
                    subtables.push((real_kind, real_slice));
                }
            } else {
                subtables.push((kind, slice));
            }
        }
        let mark_filtering_set = (flags & LOOKUP_USE_MARK_FILTERING_SET != 0)
            .then(|| u16_at(self.data, base + 6 + sub_count * 2))
            .flatten();
        Some(Lookup {
            kind: if kind == extension_kind {
                subtables.first().map_or(kind, |(k, _)| *k)
            } else {
                kind
            },
            flags,
            mark_filtering_set,
            subtables,
        })
    }
}

/// Résout une sous-table d'extension (GSUB type 7, GPOS type 9) : renvoie le
/// vrai type et la tranche de la sous-table visée.
fn resolve_extension(slice: &[u8]) -> Option<(u16, &[u8])> {
    let mut r = Reader::new(slice);
    if r.read_u16()? != 1 {
        return None;
    }
    let kind = r.read_u16()?;
    let offset = usize::try_from(r.read_u32()?).ok()?;
    let target = slice.get(offset..)?;
    Some((kind, target))
}

/// Table `GDEF` : classes de glyphes, classes d'attachement de marque et
/// jeux de marques filtrants.
///
/// Les listes `AttachList`, `LigCaretList` et `ItemVariationStore` ne sont
/// pas lues : la première sert au hinting, la deuxième au placement du
/// curseur dans une ligature (nous le faisons par les grappes), la
/// troisième aux polices variables (hors de notre périmètre).
#[derive(Debug, Clone, Default)]
pub struct Gdef<'a> {
    glyph_classes: Option<ClassDef<'a>>,
    mark_attach: Option<ClassDef<'a>>,
    mark_sets: Vec<Coverage<'a>>,
}

/// Classe `GDEF` d'un glyphe de base.
pub const GLYPH_CLASS_BASE: u16 = 1;
/// Classe `GDEF` d'une ligature.
pub const GLYPH_CLASS_LIGATURE: u16 = 2;
/// Classe `GDEF` d'une marque.
pub const GLYPH_CLASS_MARK: u16 = 3;
/// Classe `GDEF` d'un composant de ligature.
pub const GLYPH_CLASS_COMPONENT: u16 = 4;

impl<'a> Gdef<'a> {
    /// Analyse la table `GDEF`.
    #[must_use]
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let minor = r.read_u16()?;
        if major != 1 {
            return None;
        }
        let glyph_class_offset = r.read_u16()?;
        let _attach_list = r.read_u16()?;
        let _lig_caret_list = r.read_u16()?;
        let mark_attach_offset = r.read_u16()?;
        let mark_sets_offset = if minor >= 2 { r.read_u16()? } else { 0 };
        let sub = |offset: u16| -> Option<&'a [u8]> {
            (offset != 0)
                .then(|| data.get(usize::from(offset)..))
                .flatten()
        };
        let mut gdef = Self {
            glyph_classes: sub(glyph_class_offset).map(ClassDef::new),
            mark_attach: sub(mark_attach_offset).map(ClassDef::new),
            mark_sets: Vec::new(),
        };
        if let Some(sets) = sub(mark_sets_offset) {
            let count = u16_at(sets, 2).map_or(0, usize::from);
            for i in 0..count.min(MAX_ENTRIES) {
                let Some(offset) = crate::reader::u32_at(sets, 4 + i * 4) else {
                    break;
                };
                let Ok(offset) = usize::try_from(offset) else {
                    break;
                };
                if let Some(slice) = sets.get(offset..) {
                    gdef.mark_sets.push(Coverage::new(slice));
                }
            }
        }
        Some(gdef)
    }

    /// Classe d'un glyphe (0 si non classé).
    #[must_use]
    pub fn glyph_class(&self, gid: u16) -> u16 {
        self.glyph_classes.map_or(0, |c| c.class(gid))
    }

    /// Classe d'attachement de marque d'un glyphe.
    #[must_use]
    pub fn mark_attachment_class(&self, gid: u16) -> u16 {
        self.mark_attach.map_or(0, |c| c.class(gid))
    }

    /// Vrai si le glyphe appartient au jeu de marques filtrant `set`.
    #[must_use]
    pub fn in_mark_set(&self, set: u16, gid: u16) -> bool {
        self.mark_sets
            .get(usize::from(set))
            .is_some_and(|c| c.contains(gid))
    }
}

/// Vrai si un glyphe doit être sauté par une table de recherche, d'après
/// ses drapeaux et la table `GDEF`.
///
/// Sans `GDEF`, seule l'heuristique « pas de classe » s'applique : rien
/// n'est sauté, ce qui est le comportement sûr.
#[must_use]
pub fn is_skipped(lookup: &Lookup<'_>, gdef: Option<&Gdef<'_>>, gid: u16) -> bool {
    let Some(gdef) = gdef else {
        return false;
    };
    let class = gdef.glyph_class(gid);
    if lookup.flags & LOOKUP_IGNORE_BASE_GLYPHS != 0 && class == GLYPH_CLASS_BASE {
        return true;
    }
    if lookup.flags & LOOKUP_IGNORE_LIGATURES != 0 && class == GLYPH_CLASS_LIGATURE {
        return true;
    }
    if lookup.flags & LOOKUP_IGNORE_MARKS != 0 && class == GLYPH_CLASS_MARK {
        return true;
    }
    if class == GLYPH_CLASS_MARK {
        let wanted = (lookup.flags & LOOKUP_MARK_ATTACHMENT_TYPE) >> 8;
        if wanted != 0 && gdef.mark_attachment_class(gid) != wanted {
            return true;
        }
        if let Some(set) = lookup.mark_filtering_set {
            if !gdef.in_mark_set(set, gid) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod tests {
    use super::*;

    /// Table de couverture format 1 sur une liste de glyphes triée.
    pub(crate) fn coverage_format1(glyphs: &[u16]) -> Vec<u8> {
        let mut out = vec![0, 1];
        out.extend_from_slice(&u16::try_from(glyphs.len()).unwrap_or(0).to_be_bytes());
        for g in glyphs {
            out.extend_from_slice(&g.to_be_bytes());
        }
        out
    }

    /// Table de couverture format 2 sur des plages `(début, fin, rang)`.
    pub(crate) fn coverage_format2(ranges: &[(u16, u16, u16)]) -> Vec<u8> {
        let mut out = vec![0, 2];
        out.extend_from_slice(&u16::try_from(ranges.len()).unwrap_or(0).to_be_bytes());
        for (start, end, index) in ranges {
            out.extend_from_slice(&start.to_be_bytes());
            out.extend_from_slice(&end.to_be_bytes());
            out.extend_from_slice(&index.to_be_bytes());
        }
        out
    }

    /// Table de classes format 2 sur des plages `(début, fin, classe)`.
    pub(crate) fn class_def_format2(ranges: &[(u16, u16, u16)]) -> Vec<u8> {
        let mut out = vec![0, 2];
        out.extend_from_slice(&u16::try_from(ranges.len()).unwrap_or(0).to_be_bytes());
        for (start, end, class) in ranges {
            out.extend_from_slice(&start.to_be_bytes());
            out.extend_from_slice(&end.to_be_bytes());
            out.extend_from_slice(&class.to_be_bytes());
        }
        out
    }

    #[test]
    fn coverage_formats_agree() {
        let f1 = coverage_format1(&[3, 4, 5, 9]);
        let c1 = Coverage::new(&f1);
        assert_eq!(c1.index(3), Some(0));
        assert_eq!(c1.index(5), Some(2));
        assert_eq!(c1.index(9), Some(3));
        assert_eq!(c1.index(6), None);
        assert_eq!(c1.glyphs(), vec![3, 4, 5, 9]);

        let f2 = coverage_format2(&[(3, 5, 0), (9, 9, 3)]);
        let c2 = Coverage::new(&f2);
        for gid in [3u16, 4, 5, 9] {
            assert_eq!(c1.index(gid), c2.index(gid), "glyphe {gid}");
        }
        assert_eq!(c2.index(6), None);
        assert_eq!(c2.glyphs(), vec![3, 4, 5, 9]);
    }

    #[test]
    fn class_def_format1_and_2() {
        let mut f1 = vec![0, 1];
        f1.extend_from_slice(&10u16.to_be_bytes());
        f1.extend_from_slice(&3u16.to_be_bytes());
        for c in [1u16, 2, 1] {
            f1.extend_from_slice(&c.to_be_bytes());
        }
        let d1 = ClassDef::new(&f1);
        assert_eq!(d1.class(9), 0);
        assert_eq!(d1.class(10), 1);
        assert_eq!(d1.class(11), 2);
        assert_eq!(d1.class(13), 0);
        assert_eq!(d1.max_class(), 2);

        let f2 = class_def_format2(&[(10, 10, 1), (11, 11, 2), (12, 12, 1)]);
        let d2 = ClassDef::new(&f2);
        for gid in 9..14u16 {
            assert_eq!(d1.class(gid), d2.class(gid), "glyphe {gid}");
        }
        assert_eq!(d2.max_class(), 2);
    }

    #[test]
    fn value_records_follow_their_format() {
        assert_eq!(value_record_size(0), 0);
        assert_eq!(value_record_size(VALUE_X_ADVANCE), 2);
        assert_eq!(
            value_record_size(VALUE_X_PLACEMENT | VALUE_X_ADVANCE | 0x0040),
            6
        );
        let data = [0xFF, 0xF0, 0x00, 0x20];
        let mut r = Reader::new(&data);
        let v = read_value_record(&mut r, VALUE_X_PLACEMENT | VALUE_X_ADVANCE);
        assert_eq!(v.x_placement, -16);
        assert_eq!(v.x_advance, 32);
        assert!(!v.is_zero());
        assert!(ValueRecord::default().is_zero());
    }

    #[test]
    fn anchors_of_every_format() {
        for format in 1u16..=3 {
            let mut data = format.to_be_bytes().to_vec();
            data.extend_from_slice(&100i16.to_be_bytes());
            data.extend_from_slice(&(-50i16).to_be_bytes());
            data.extend_from_slice(&[0, 0, 0, 0]);
            assert_eq!(read_anchor(&data, 0), Some(Anchor { x: 100, y: -50 }));
        }
        assert_eq!(read_anchor(&[0, 9, 0, 0, 0, 0], 0), None);
        assert_eq!(read_anchor(&[0, 1], 0), None);
    }

    #[test]
    fn corrupt_tables_never_panic() {
        assert_eq!(Coverage::new(&[]).index(1), None);
        assert!(Coverage::new(&[0, 1, 0xFF, 0xFF]).glyphs().len() <= MAX_ENTRIES * 16);
        assert_eq!(ClassDef::new(&[0, 3]).class(7), 0);
        assert!(LayoutTable::parse(&[0, 2, 0, 0, 0, 0, 0, 0, 0, 0]).is_none());
        assert!(LayoutTable::parse(&[0, 1]).is_none());
        assert!(Gdef::parse(&[0, 1]).is_none());
    }
}
