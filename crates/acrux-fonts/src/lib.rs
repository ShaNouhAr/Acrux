//! # acrux-fonts
//!
//! Parseurs et rasteriseurs de polices écrits de zéro.
//!
//! Modules : `truetype`, `cff`, `type1`, `opentype`, `encodings`, `cmap`,
//! `standard`, `fallback`, `subset`, `unicode`, `shape`. À venir : `type3`.
//!
//! Sortie commune : des contours vectoriels (segments et courbes de Bézier)
//! en unités de police, que `acrux-graphics` transforme et rasterise.
//!
//! ## État des modules
//!
//! | Module      | Rôle                                                   | Spécification                         | État        |
//! |-------------|--------------------------------------------------------|---------------------------------------|-------------|
//! | `reader`    | Lecteur d'octets big-endian borné                      | OpenType « Data types »               | Implémenté  |
//! | `glyph`     | Trait [`GlyphProvider`] commun aux programmes de police | —                                    | Implémenté  |
//! | `truetype`  | `.ttf`, `.ttc`, OpenType `OTTO` (tables head, maxp, loca, glyf, hhea, hmtx, cmap, post, OS/2) | OpenType (Microsoft) | Implémenté |
//! | `cff`       | CFF nu et table `CFF `, charstrings Type 2, CID-keyed  | Adobe TN #5176, #5177                 | Implémenté  |
//! | `type1`     | PFA, PFB, FontFile ; eexec ; charstrings Type 1 ; flex | Adobe Type 1 Font Format              | Implémenté  |
//! | `encodings` | Standard, WinAnsi, MacRoman, MacExpert, PDFDoc, Symbol, ZapfDingbats ; AGL réduite | ISO 32000-2 annexe D | Implémenté |
//! | `cmap`      | CMaps incorporées, Identity-H/V, ToUnicode             | ISO 32000-2 §9.7.5, §9.10.3, TN #5014 | Implémenté  |
//! | `fallback`  | Police de secours dessinée par nous, pour les machines sans polices | —                                     | Implémenté  |
//! | `standard`  | Métriques des quatorze polices standard (largeurs AFM, descripteurs) | ISO 32000-2 §9.6.2.2, annexe D | Implémenté  |
//! | `subset`    | Sous-ensemble TrueType : tables head, hhea, maxp, hmtx, loca, glyf, cmap, name, post recalculées | OpenType (Microsoft) | Implémenté |
//! | `opentype`  | `GSUB` (types 1 à 8), `GPOS` (types 1 à 9), `GDEF`, `kern` format 0, `vhea`/`vmtx`/`VORG` | OpenType Layout | Implémenté |
//! | `unicode`   | Bidirectionnel UAX #9, grappes UAX #29, jointure arabe, classes combinatoires | UAX #9, #15, #29, #44 | Implémenté |
//! | `shape`     | Composition : ligatures, crénage, formes arabes, bidi, CJC vertical | OpenType Layout | Implémenté (hors écritures syllabiques) |
//! | `type3`     | Glyphes définis par des flux de contenu                | ISO 32000-2 §9.6.4                    | À venir (dans `acrux-render`) |
//! | `substitution` | Polices de secours métriquement compatibles         | —                                     | À venir     |
//!
//! ## Conventions
//!
//! - Les contours sont renvoyés en **unités de police** (`acrux_core::Path`) :
//!   pour TrueType, diviser par `units_per_em()` ; pour CFF et Type 1,
//!   appliquer `font_matrix()` (par défaut `1/1000`).
//! - Aucune fonction ne panique sur une police corrompue : les lectures
//!   sont bornées, les récursions (glyphes composites, sous-routines,
//!   `seac`) limitées en profondeur, et les boucles bornées par la taille
//!   des données ou un budget d'opérations.
//! - Le hinting est ignoré ; les instructions TrueType et les hints
//!   Type 1 / Type 2 sont lus et sautés.
//! - La composition ([`shape`]) rend des longueurs en **unités de police**
//!   elle aussi, et des numéros de grappe qui sont des **indices d'octets**
//!   dans la chaîne d'origine : c'est ce qui permet de poser un curseur.
//!   Les écritures syllabiques (indiennes, khmère, thaïe…) ne sont pas
//!   couvertes ; voir `spec-notes/opentype-gsub-gpos.md`.
//!
//! ## Exemple
//!
//! ```
//! use acrux_fonts::{encodings, cmap::CMap};
//!
//! assert_eq!(encodings::win_ansi(0xE9), Some("eacute"));
//! assert_eq!(encodings::glyph_name_to_unicode("eacute"), Some('é'));
//! let identity = CMap::predefined("Identity-H").unwrap();
//! assert_eq!(identity.decode(&[0x00, 0x41]), vec![(0x41, 0x41, 2)]);
//! ```
//!
//! ```no_run
//! use acrux_fonts::{shape::{shape, ShapeOptions}, TrueTypeFont};
//!
//! # fn main() -> acrux_core::Result<()> {
//! let data = std::fs::read(r"C:\Windows\Fonts\calibri.ttf")?;
//! let font = TrueTypeFont::parse(&data)?;
//! // « ffi » devient un seul glyphe, et « AV » est créné.
//! assert_eq!(shape(&font, "ffi", &ShapeOptions::default()).len(), 1);
//! # Ok(())
//! # }
//! ```

pub mod cff;
pub mod cmap;
pub mod encodings;
pub mod fallback;
pub mod glyph;
pub mod opentype;
pub mod reader;
pub mod shape;
pub mod standard;
pub mod subset;
pub mod truetype;
pub mod type1;
pub mod unicode;

pub use cff::CffFont;
pub use cmap::CMap;
pub use encodings::BaseEncoding;
pub use glyph::GlyphProvider;
pub use shape::{shape, ShapeOptions, ShapedGlyph, Shaper};
pub use subset::{subset, subset_merged, Subset};
pub use truetype::TrueTypeFont;
pub use type1::Type1Font;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    /// Une police OpenType `OTTO` délègue ses contours à la table `CFF `.
    #[test]
    fn opentype_cff_delegation() {
        let cff = cff::tests::sample_font();
        let data = truetype::tests::Builder::new()
            .table(*b"head", truetype::tests::head_table(1000, false))
            .table(*b"maxp", truetype::tests::maxp_table(8))
            .table(*b"hhea", truetype::tests::hhea_table(1))
            .table(*b"hmtx", truetype::tests::hmtx_table(&[777]))
            .table(*b"CFF ", cff)
            .build(*b"OTTO");
        let font = TrueTypeFont::parse(&data).unwrap();
        assert!(font.is_cff());
        assert!(!font.has_glyf());
        assert_eq!(font.glyph_count(), 8);
        assert_eq!(font.glyph_path(1).unwrap().commands().len(), 5);
        assert_eq!(font.advance(1), Some(777));
        assert_eq!(font.gid_by_name("hinted"), Some(4));
        assert_eq!(font.cff().unwrap().advance(1), Some(500.0));

        // Sans head ni hmtx : unités et avances viennent du CFF.
        let data = truetype::tests::Builder::new()
            .table(*b"CFF ", cff::tests::sample_font())
            .build(*b"OTTO");
        let font = TrueTypeFont::parse(&data).unwrap();
        assert_eq!(font.units_per_em(), 1000);
        assert_eq!(font.glyph_count(), 8);
        assert_eq!(font.advance(1), Some(500));
        let p: &dyn GlyphProvider = &font;
        assert_eq!(p.advance(1), Some(500.0));
    }

    /// Les trois programmes de police exposent la même interface.
    #[test]
    fn providers_are_object_safe() {
        let tt = TrueTypeFont::parse(&truetype::tests::sample_font()).unwrap();
        let cff = CffFont::parse(&cff::tests::sample_font()).unwrap();
        let t1 = Type1Font::parse(&type1::tests::sample_pfa()).unwrap();
        let providers: Vec<&dyn GlyphProvider> = vec![&tt, &cff, &t1];
        for p in providers {
            assert!(p.glyph_count() > 1);
            assert_eq!(p.units_per_em(), 1000.0);
            assert!(p.glyph_path(1).is_some());
            assert!(p.advance(1).is_some());
        }
    }
}
