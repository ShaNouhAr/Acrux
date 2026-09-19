//! Composition typographique (« shaping ») : d'une chaîne de caractères à
//! une suite de glyphes positionnés.
//!
//! Poser un texte, ce n'est pas dessiner un glyphe par caractère les uns
//! après les autres. Il faut :
//!
//! 1. **réordonner** ce qui se lit de droite à gauche ([`crate::unicode::bidi`]) ;
//! 2. **regrouper** en grappes pour que le curseur tombe juste
//!    ([`crate::unicode::grapheme`]) ;
//! 3. **substituer** : ligature `fi`, forme initiale d'une lettre arabe,
//!    forme verticale d'un idéogramme (`GSUB`) ;
//! 4. **positionner** : crénage, accent posé sur sa lettre (`GPOS`).
//!
//! ## Exemple
//!
//! ```no_run
//! use acrux_fonts::{shape::{shape, ShapeOptions}, TrueTypeFont};
//!
//! # fn main() -> acrux_core::Result<()> {
//! let data = std::fs::read(r"C:\Windows\Fonts\calibri.ttf")?;
//! let font = TrueTypeFont::parse(&data)?;
//! let glyphes = shape(&font, "affiche", &ShapeOptions::default());
//! // « ffi » devient un seul glyphe : moins de glyphes que de caractères.
//! assert!(glyphes.len() < 7);
//! # Ok(())
//! # }
//! ```
//!
//! ## Écritures couvertes
//!
//! - **Latin, grec, cyrillique** : ligatures, crénage, contextuelles,
//!   accents ancrés. Complet.
//! - **Arabe** : formes contextuelles (`init`, `medi`, `fina`, `isol`),
//!   ligatures obligatoires (`rlig`), marques ancrées, bidirectionnel.
//! - **Hébreu** : bidirectionnel, marques ancrées, crénage.
//! - **CJC** : substitutions et crénage, plus le mode vertical (`vert`,
//!   `vrt2`, métriques `vhea`/`vmtx`).
//!
//! ## Écritures **non** couvertes
//!
//! Les écritures dites complexes — indiennes (devanagari, bengali,
//! tamoul…), khmère, birmane, thaïe, tibétaine — demandent un moteur
//! syllabique : réordonner les voyelles autour de la consonne, découper en
//! syllabes, appliquer les fonctionnalités par position dans la syllabe.
//! Nous ne l'écrivons pas. Leur texte est composé « à plat » : les
//! substitutions et le crénage de la police s'appliquent quand même, mais
//! l'ordre des voyelles n'est pas corrigé. Le résultat est lisible pour du
//! texte simple et faux pour du texte complexe ; c'est dit ici plutôt que
//! promis ailleurs.
//!
//! De même, le syriaque (logique de l'alaph) et le mongol ne sont pas
//! traités ; l'algorithme bidirectionnel ne fait pas la règle N0 (paires de
//! parenthèses).

use std::collections::BTreeMap;

use crate::opentype::buffer::{refresh_classes, GlyphInfo};
use crate::opentype::common::{Gdef, LayoutTable};
use crate::opentype::gpos::{resolve_attachments, Gpos};
use crate::opentype::gsub::Gsub;
use crate::opentype::kern::KernTable;
use crate::opentype::vertical::VerticalMetrics;
use crate::truetype::TrueTypeFont;
use crate::unicode::bidi::{reorder_visual, BaseDirection};
use crate::unicode::ccc::combining_class;
use crate::unicode::grapheme::cluster_boundaries;
use crate::unicode::joining::{is_cursive_script, joining_forms, JoiningForm};
use crate::unicode::script::dominant_script;

/// Masque des fonctionnalités actives sur tous les glyphes.
const MASK_GLOBAL: u32 = 1;
/// Masque de la forme isolée.
const MASK_ISOL: u32 = 1 << 1;
/// Masque de la forme initiale.
const MASK_INIT: u32 = 1 << 2;
/// Masque de la forme médiane.
const MASK_MEDI: u32 = 1 << 3;
/// Masque de la forme finale.
const MASK_FINA: u32 = 1 << 4;

/// Nombre maximal de glyphes produits pour un passage (borne contre une
/// police qui multiplierait les substitutions multiples).
const MAX_GLYPHS: usize = 1 << 16;

/// Sens d'écriture demandé.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// Déduit du texte par l'algorithme bidirectionnel.
    #[default]
    Auto,
    /// Forcé de gauche à droite.
    LeftToRight,
    /// Forcé de droite à gauche.
    RightToLeft,
}

impl Direction {
    fn base(self) -> BaseDirection {
        match self {
            Direction::Auto => BaseDirection::Auto,
            Direction::LeftToRight => BaseDirection::LeftToRight,
            Direction::RightToLeft => BaseDirection::RightToLeft,
        }
    }
}

/// Mode d'écriture : en lignes ou en colonnes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WritingMode {
    /// Lignes horizontales (le cas courant).
    #[default]
    Horizontal,
    /// Colonnes verticales, de haut en bas (japonais, chinois).
    Vertical,
}

/// Fonctionnalités activées par défaut en écriture horizontale.
const DEFAULT_HORIZONTAL: &[[u8; 4]] = &[
    *b"ccmp", *b"locl", *b"rlig", *b"liga", *b"clig", *b"calt", *b"rclt", *b"kern", *b"mark",
    *b"mkmk", *b"curs", *b"isol", *b"init", *b"medi", *b"fina",
];

/// Fonctionnalités activées par défaut en écriture verticale.
const DEFAULT_VERTICAL: &[[u8; 4]] = &[
    *b"ccmp", *b"locl", *b"vert", *b"vrt2", *b"vkrn", *b"mark", *b"mkmk",
];

/// Réglages de composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapeOptions {
    /// Tag OpenType du script (`latn`, `arab`…). `None` : déduit du texte.
    pub script: Option<[u8; 4]>,
    /// Tag OpenType du système de langue (`FRA `, `TRK `…). `None` : la
    /// langue par défaut du script.
    pub language: Option<[u8; 4]>,
    /// Sens d'écriture.
    pub direction: Direction,
    /// Lignes ou colonnes.
    pub writing_mode: WritingMode,
    /// Fonctionnalités à appliquer, par tag.
    features: Vec<[u8; 4]>,
}

impl Default for ShapeOptions {
    /// Les fonctionnalités usuelles de l'écriture horizontale.
    fn default() -> Self {
        Self {
            script: None,
            language: None,
            direction: Direction::Auto,
            writing_mode: WritingMode::Horizontal,
            features: DEFAULT_HORIZONTAL.to_vec(),
        }
    }
}

impl ShapeOptions {
    /// Composition **sans aucune fonctionnalité** : un glyphe par grappe,
    /// l'avance de `hmtx`, rien d'autre.
    ///
    /// C'est le comportement d'avant le shaping ; sert de référence de
    /// non-régression et de repli quand on veut absolument que le texte
    /// mesure ce que mesure la somme des avances.
    #[must_use]
    pub fn plain() -> Self {
        Self {
            script: None,
            language: None,
            direction: Direction::LeftToRight,
            writing_mode: WritingMode::Horizontal,
            features: Vec::new(),
        }
    }

    /// Réglages d'écriture verticale (CJC).
    #[must_use]
    pub fn vertical() -> Self {
        Self {
            script: None,
            language: None,
            direction: Direction::LeftToRight,
            writing_mode: WritingMode::Vertical,
            features: DEFAULT_VERTICAL.to_vec(),
        }
    }

    /// Active une fonctionnalité (sans effet si elle l'est déjà).
    #[must_use]
    pub fn with_feature(mut self, tag: [u8; 4]) -> Self {
        if !self.features.contains(&tag) {
            self.features.push(tag);
        }
        self
    }

    /// Désactive une fonctionnalité.
    #[must_use]
    pub fn without_feature(mut self, tag: [u8; 4]) -> Self {
        self.features.retain(|t| *t != tag);
        self
    }

    /// Impose le script.
    #[must_use]
    pub const fn with_script(mut self, tag: [u8; 4]) -> Self {
        self.script = Some(tag);
        self
    }

    /// Impose le sens d'écriture.
    #[must_use]
    pub const fn with_direction(mut self, direction: Direction) -> Self {
        self.direction = direction;
        self
    }

    /// Vrai si la fonctionnalité est active.
    #[must_use]
    pub fn is_enabled(&self, tag: [u8; 4]) -> bool {
        self.features.contains(&tag)
    }

    /// Masque associé à un tag : les quatre formes arabes ont chacune le
    /// leur, tout le reste est global.
    fn mask_of(tag: [u8; 4]) -> u32 {
        match &tag {
            b"isol" => MASK_ISOL,
            b"init" => MASK_INIT,
            b"medi" => MASK_MEDI,
            b"fina" => MASK_FINA,
            _ => MASK_GLOBAL,
        }
    }
}

/// Un glyphe composé, prêt à être dessiné.
///
/// Toutes les longueurs sont en **unités de police** : diviser par
/// `font.units_per_em()` pour obtenir des ems, multiplier par la taille de
/// corps pour obtenir des points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapedGlyph {
    /// Identifiant du glyphe dans la police.
    pub gid: u16,
    /// Indice, en **octets**, du début de la grappe d'origine dans la
    /// chaîne passée à [`shape`].
    ///
    /// Plusieurs glyphes peuvent partager une grappe (une base et ses
    /// accents) et un glyphe peut couvrir plusieurs caractères (une
    /// ligature) : c'est cet indice qui permet de poser un curseur et de
    /// sélectionner du texte sans jamais couper une grappe.
    pub cluster: u32,
    /// Avance dans le sens d'écriture, crénage compris.
    pub advance: f64,
    /// Déplacement horizontal du dessin par rapport à la plume.
    pub offset_x: f64,
    /// Déplacement vertical du dessin par rapport à la plume.
    pub offset_y: f64,
}

/// Compositeur : garde les tables de la police analysées d'une fois sur
/// l'autre.
///
/// Composer une ligne demande de lire `GSUB`, `GPOS` et `GDEF` ; les
/// analyser une fois pour tout un document est nettement plus rapide que
/// pour chaque ligne.
#[derive(Debug)]
pub struct Shaper<'a> {
    font: &'a TrueTypeFont,
    gsub: Option<Gsub<'a>>,
    gpos: Option<Gpos<'a>>,
    gdef: Option<Gdef<'a>>,
    kern: Option<KernTable<'a>>,
    vertical: Option<VerticalMetrics>,
}

impl<'a> Shaper<'a> {
    /// Analyse les tables de composition d'une police.
    #[must_use]
    pub fn new(font: &'a TrueTypeFont) -> Self {
        Self {
            font,
            gsub: font.table_bytes(*b"GSUB").and_then(Gsub::parse),
            gpos: font.table_bytes(*b"GPOS").and_then(Gpos::parse),
            gdef: font.table_bytes(*b"GDEF").and_then(Gdef::parse),
            kern: font.table_bytes(*b"kern").and_then(KernTable::parse),
            vertical: VerticalMetrics::read(font),
        }
    }

    /// Vrai si la police déclare des substitutions.
    #[must_use]
    pub const fn has_gsub(&self) -> bool {
        self.gsub.is_some()
    }

    /// Vrai si la police déclare des positionnements.
    #[must_use]
    pub const fn has_gpos(&self) -> bool {
        self.gpos.is_some()
    }

    /// Vrai si la police a une table `kern` ancienne exploitable.
    #[must_use]
    pub fn has_legacy_kern(&self) -> bool {
        self.kern.as_ref().is_some_and(KernTable::is_usable)
    }

    /// Métriques verticales, si la police en a.
    #[must_use]
    pub const fn vertical_metrics(&self) -> Option<&VerticalMetrics> {
        self.vertical.as_ref()
    }

    /// Compose un texte et renvoie les glyphes dans l'**ordre visuel**.
    #[must_use]
    pub fn shape(&self, text: &str, options: &ShapeOptions) -> Vec<ShapedGlyph> {
        if text.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for run in reorder_visual(text, options.direction.base()) {
            let Some(slice) = text.get(run.start..run.end) else {
                continue;
            };
            let glyphs = self.shape_run(slice, run.start, run.is_rtl(), options);
            out.extend(glyphs);
            if out.len() > MAX_GLYPHS {
                break;
            }
        }
        out
    }

    /// Compose un passage homogène en direction.
    fn shape_run(
        &self,
        text: &str,
        base_offset: usize,
        rtl: bool,
        options: &ShapeOptions,
    ) -> Vec<ShapedGlyph> {
        let chars = ordered_chars(text);
        let script = options
            .script
            .unwrap_or_else(|| dominant_script(&chars.iter().map(|c| c.value).collect::<String>()));
        let mut buffer = self.map_to_glyphs(&chars, base_offset);
        refresh_classes(&mut buffer, self.gdef.as_ref());

        self.substitute(&mut buffer, script, options);
        self.set_advances(&mut buffer, options);
        let kerned = self.position(&mut buffer, script, options);
        if !kerned && options.is_enabled(*b"kern") {
            self.apply_legacy_kern(&mut buffer);
        }
        resolve_attachments(&mut buffer);

        if rtl {
            buffer.reverse();
        }
        buffer
            .into_iter()
            .map(|g| ShapedGlyph {
                gid: g.gid,
                cluster: g.cluster,
                advance: f64::from(if options.writing_mode == WritingMode::Vertical {
                    -g.y_advance
                } else {
                    g.x_advance
                }),
                offset_x: f64::from(g.x_offset),
                offset_y: f64::from(g.y_offset),
            })
            .collect()
    }

    /// Caractères → glyphes, avec les masques de fonctionnalités.
    fn map_to_glyphs(&self, chars: &[OrderedChar], base_offset: usize) -> Vec<GlyphInfo> {
        let values: Vec<char> = chars.iter().map(|c| c.value).collect();
        let forms = joining_forms(&values);
        let mut buffer = Vec::with_capacity(chars.len());
        for (i, c) in chars.iter().enumerate() {
            let gid = self.glyph_for(c.value);
            let mut mask = MASK_GLOBAL;
            if is_cursive_script(c.value) {
                mask |= match forms.get(i).copied().unwrap_or(JoiningForm::Isolated) {
                    JoiningForm::Isolated => MASK_ISOL,
                    JoiningForm::Initial => MASK_INIT,
                    JoiningForm::Medial => MASK_MEDI,
                    JoiningForm::Final => MASK_FINA,
                };
            }
            let cluster = u32::try_from(base_offset + c.cluster).unwrap_or(u32::MAX);
            buffer.push(GlyphInfo::new(gid, cluster, mask));
        }
        buffer
    }

    /// Glyphe d'un caractère : `cmap` Unicode, puis les sous-tables
    /// symboliques (3,0) et (1,0) des polices sans Unicode.
    fn glyph_for(&self, c: char) -> u16 {
        if let Some(gid) = self.font.unicode_to_gid(c) {
            return gid;
        }
        let code = u32::from(c);
        if code < 0x100 {
            for base in [0xF000u32, 0] {
                if let Some(gid) = self.font.cmap_lookup(3, 0, base + code) {
                    return gid;
                }
            }
            if let Some(gid) = self.font.cmap_lookup(1, 0, code) {
                return gid;
            }
        }
        0
    }

    /// Applique les substitutions `GSUB`.
    fn substitute(&self, buffer: &mut Vec<GlyphInfo>, script: [u8; 4], options: &ShapeOptions) {
        let Some(gsub) = self.gsub.as_ref() else {
            return;
        };
        let plan = build_plan(gsub.table(), script, options);
        for (index, mask) in plan {
            gsub.apply(index, mask, buffer, self.gdef.as_ref());
            if buffer.len() > MAX_GLYPHS {
                buffer.truncate(MAX_GLYPHS);
                return;
            }
        }
    }

    /// Avance de départ de chaque glyphe, avant `GPOS`.
    fn set_advances(&self, buffer: &mut [GlyphInfo], options: &ShapeOptions) {
        let vertical = options.writing_mode == WritingMode::Vertical;
        for info in buffer.iter_mut() {
            if vertical {
                let metrics = self.vertical.as_ref();
                let advance = metrics.map_or_else(
                    || i32::from(self.font.units_per_em()),
                    |m| i32::from(m.advance(info.gid)),
                );
                info.y_advance = -advance;
                info.x_advance = 0;
                // Le glyphe est dessiné depuis son origine verticale :
                // centré horizontalement, décalé de l'origine en ordonnée.
                let horizontal = i32::from(self.font.advance(info.gid).unwrap_or(0));
                info.x_offset -= horizontal / 2;
                if let Some(m) = metrics {
                    info.y_offset -= i32::from(m.vertical_origin(info.gid));
                }
            } else {
                info.x_advance = i32::from(self.font.advance(info.gid).unwrap_or(0));
                info.y_advance = 0;
            }
        }
    }

    /// Applique les positionnements `GPOS` ; renvoie vrai si un crénage a
    /// été appliqué par cette table (auquel cas la table `kern` ancienne
    /// n'est pas utilisée).
    fn position(&self, buffer: &mut [GlyphInfo], script: [u8; 4], options: &ShapeOptions) -> bool {
        let Some(gpos) = self.gpos.as_ref() else {
            return false;
        };
        let plan = build_plan(gpos.table(), script, options);
        let has_kern = has_feature(gpos.table(), script, options, *b"kern")
            || has_feature(gpos.table(), script, options, *b"vkrn");
        for (index, mask) in plan {
            gpos.apply(index, mask, buffer, self.gdef.as_ref());
        }
        has_kern
    }

    /// Repli sur la table `kern` ancienne.
    fn apply_legacy_kern(&self, buffer: &mut [GlyphInfo]) {
        let Some(kern) = self.kern.as_ref() else {
            return;
        };
        for i in 0..buffer.len().saturating_sub(1) {
            let (Some(left), Some(right)) = (buffer.get(i), buffer.get(i + 1)) else {
                break;
            };
            let value = kern.kerning(left.gid, right.gid);
            if value != 0 {
                if let Some(info) = buffer.get_mut(i) {
                    info.x_advance += value;
                }
            }
        }
    }
}

/// Vrai si le script déclare cette fonctionnalité **et** qu'elle est
/// demandée.
fn has_feature(
    table: &LayoutTable<'_>,
    script: [u8; 4],
    options: &ShapeOptions,
    tag: [u8; 4],
) -> bool {
    options.is_enabled(tag)
        && table
            .feature_indices(script, options.language)
            .into_iter()
            .any(|i| table.feature_tag(i) == Some(tag))
}

/// Construit la liste des tables de recherche à appliquer, avec leur masque.
///
/// Les tables sont appliquées dans l'ordre de leur indice, comme l'exige la
/// spécification (« lookups are applied in the order they appear in the
/// LookupList »).
fn build_plan(table: &LayoutTable<'_>, script: [u8; 4], options: &ShapeOptions) -> Vec<(u16, u32)> {
    let mut masks: BTreeMap<u16, u32> = BTreeMap::new();
    // `vrt2` remplace `vert` quand la police propose les deux.
    let indices = table.feature_indices(script, options.language);
    let tags: Vec<Option<[u8; 4]>> = indices
        .iter()
        .map(|i| table.feature_tag(*i))
        .collect::<Vec<_>>();
    let has_vrt2 = tags.iter().flatten().any(|t| t == b"vrt2");
    for (index, tag) in indices.iter().zip(tags.iter()) {
        let Some(tag) = tag else { continue };
        if !options.is_enabled(*tag) {
            continue;
        }
        if has_vrt2 && tag == b"vert" {
            continue;
        }
        let mask = ShapeOptions::mask_of(*tag);
        for lookup in table.feature_lookups(*index) {
            *masks.entry(lookup).or_insert(0) |= mask;
        }
    }
    masks.into_iter().collect()
}

/// Un caractère avec le numéro de grappe auquel il appartient.
struct OrderedChar {
    value: char,
    cluster: usize,
}

/// Découpe le texte en grappes, trie les marques de chaque grappe dans
/// l'ordre canonique et note à quelle grappe appartient chaque caractère.
///
/// L'ordre canonique (classes combinatoires croissantes, les classes 0
/// restant à leur place) est ce qu'OpenType suppose : une police qui ancre
/// une chadda puis une fatha ne reconnaîtrait pas l'ordre inverse.
fn ordered_chars(text: &str) -> Vec<OrderedChar> {
    let bounds = cluster_boundaries(text);
    let mut out = Vec::with_capacity(text.len());
    for window in bounds.windows(2) {
        let (start, end) = (window[0], window[1]);
        let Some(slice) = text.get(start..end) else {
            continue;
        };
        let mut chars: Vec<char> = slice.chars().collect();
        sort_canonically(&mut chars);
        for value in chars {
            out.push(OrderedChar {
                value,
                cluster: start,
            });
        }
    }
    out
}

/// Tri canonique stable d'une grappe (UAX #15, « Canonical Ordering
/// Algorithm ») : échange deux marques adjacentes quand la seconde a une
/// classe plus petite et non nulle.
fn sort_canonically(chars: &mut [char]) {
    if chars.len() < 2 {
        return;
    }
    let mut changed = true;
    let mut rounds = 0;
    while changed && rounds < chars.len() {
        changed = false;
        rounds += 1;
        for i in 1..chars.len() {
            let (previous, current) = (combining_class(chars[i - 1]), combining_class(chars[i]));
            if previous > current && current != 0 {
                chars.swap(i - 1, i);
                changed = true;
            }
        }
    }
}

/// Compose un texte avec une police.
///
/// Raccourci de [`Shaper::new`] suivi de [`Shaper::shape`] ; préférer le
/// [`Shaper`] quand on compose plusieurs lignes avec la même police.
#[must_use]
pub fn shape(font: &TrueTypeFont, text: &str, options: &ShapeOptions) -> Vec<ShapedGlyph> {
    Shaper::new(font).shape(text, options)
}

/// Largeur totale d'un texte composé, en unités de police.
#[must_use]
pub fn shaped_width(glyphs: &[ShapedGlyph]) -> f64 {
    glyphs.iter().map(|g| g.advance).sum()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_gives_no_glyph() {
        let data = crate::truetype::tests::sample_font();
        let font = TrueTypeFont::parse(&data).unwrap();
        assert!(shape(&font, "", &ShapeOptions::default()).is_empty());
    }

    #[test]
    fn options_are_composable() {
        let options = ShapeOptions::default()
            .without_feature(*b"liga")
            .with_feature(*b"smcp")
            .with_script(*b"latn")
            .with_direction(Direction::LeftToRight);
        assert!(!options.is_enabled(*b"liga"));
        assert!(options.is_enabled(*b"smcp"));
        assert!(options.is_enabled(*b"kern"));
        assert_eq!(options.script, Some(*b"latn"));
        assert_eq!(options.direction, Direction::LeftToRight);
        assert!(ShapeOptions::plain().features.is_empty());
        assert!(ShapeOptions::vertical().is_enabled(*b"vert"));
        assert_eq!(ShapeOptions::vertical().writing_mode, WritingMode::Vertical);
    }

    #[test]
    fn feature_masks() {
        assert_eq!(ShapeOptions::mask_of(*b"liga"), MASK_GLOBAL);
        assert_eq!(ShapeOptions::mask_of(*b"init"), MASK_INIT);
        assert_eq!(ShapeOptions::mask_of(*b"fina"), MASK_FINA);
    }

    /// Le tri canonique remet une chadda avant une fatha.
    #[test]
    fn canonical_order() {
        // fatha (30) puis chadda (33) : déjà dans l'ordre.
        let mut chars = vec!['\u{0628}', '\u{064E}', '\u{0651}'];
        sort_canonically(&mut chars);
        assert_eq!(chars, vec!['\u{0628}', '\u{064E}', '\u{0651}']);
        // chadda (33) puis fatha (30) : à échanger.
        let mut chars = vec!['\u{0628}', '\u{0651}', '\u{064E}'];
        sort_canonically(&mut chars);
        assert_eq!(chars, vec!['\u{0628}', '\u{064E}', '\u{0651}']);
        // Une base ne bouge jamais.
        let mut chars = vec!['a', 'b'];
        sort_canonically(&mut chars);
        assert_eq!(chars, vec!['a', 'b']);
    }

    /// Les numéros de grappe sont des indices d'octets dans la chaîne.
    #[test]
    fn clusters_are_byte_offsets() {
        let ordered = ordered_chars("aé\u{0301}b");
        let clusters: Vec<usize> = ordered.iter().map(|c| c.cluster).collect();
        // « a » à 0 ; « é » (deux octets) à 1, avec son accent combinant
        // (deux octets de plus) dans la même grappe ; « b » à 5.
        assert_eq!(clusters, vec![0, 1, 1, 5]);
    }

    #[test]
    fn width_sums_advances() {
        let glyphs = vec![
            ShapedGlyph {
                gid: 1,
                cluster: 0,
                advance: 100.0,
                offset_x: 0.0,
                offset_y: 0.0,
            },
            ShapedGlyph {
                gid: 2,
                cluster: 1,
                advance: 250.0,
                offset_x: 0.0,
                offset_y: 0.0,
            },
        ];
        assert_eq!(shaped_width(&glyphs), 350.0);
    }
}
