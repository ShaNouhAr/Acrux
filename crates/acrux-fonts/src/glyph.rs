//! Interface commune des programmes de police : contours et métriques par
//! identifiant de glyphe (GID).
//!
//! Le moteur de rendu (`acrux-render`) et l'éditeur de texte n'ont pas besoin
//! de savoir si une police est TrueType, CFF ou Type 1 : ils demandent le
//! contour d'un GID en unités de police et le convertissent avec
//! `1 / units_per_em()` (ou la matrice de police pour CFF/Type 1, voir
//! `font_matrix()` de chaque type).

use acrux_core::Path;

/// Fournisseur de glyphes.
pub trait GlyphProvider {
    /// Contour du glyphe `gid` en unités de police, ou `None` si le glyphe
    /// n'existe pas ou est illisible. Un glyphe vide (espace) renvoie
    /// `Some(Path::new())`.
    fn glyph_path(&self, gid: u32) -> Option<Path>;

    /// Avance horizontale du glyphe en unités de police.
    fn advance(&self, gid: u32) -> Option<f64>;

    /// Nombre d'unités de police par em (1000 pour CFF/Type 1 avec la
    /// matrice par défaut, souvent 1000 ou 2048 pour TrueType).
    fn units_per_em(&self) -> f64;

    /// Nombre de glyphes.
    fn glyph_count(&self) -> u32;
}
