//! Tables OpenType Layout : `GSUB`, `GPOS`, `GDEF`, `kern`, `vhea`/`vmtx`.
//!
//! Ces tables décrivent ce qu'une police sait faire **au-delà** du dessin
//! d'un glyphe : quelles suites de caractères se remplacent par une
//! ligature, de combien rapprocher deux lettres, où poser un accent. Le
//! module [`crate::shape`] s'en sert pour composer du texte ; ce module-ci
//! ne fait que les lire.
//!
//! Chaque sous-module dit précisément ce qu'il implémente et ce qu'il
//! refuse ; la note `spec-notes/opentype-gsub-gpos.md` reprend l'ensemble.
//!
//! | Module      | Table(s)        | Rôle                                    |
//! |-------------|-----------------|-----------------------------------------|
//! | [`common`]  | —               | Couverture, classes, valeurs, `GDEF`    |
//! | [`buffer`]  | —               | Tampon de glyphes et saut des ignorés   |
//! | [`context`] | —               | Motifs contextuels, formats 1 à 3       |
//! | [`gsub`]    | `GSUB`          | Substitutions (ligatures, formes)       |
//! | [`gpos`]    | `GPOS`          | Positions (crénage, marques, cursif)    |
//! | [`kern`]    | `kern`          | Crénage ancien, en repli                |
//! | [`vertical`]| `vhea`, `vmtx`, `VORG` | Métriques de l'écriture verticale |

pub mod buffer;
pub mod common;
pub mod context;
pub mod gpos;
pub mod gsub;
pub mod kern;
pub mod vertical;

pub use buffer::{AttachKind, GlyphInfo};
pub use common::{Gdef, LayoutTable, Lookup};
pub use gpos::Gpos;
pub use gsub::Gsub;
pub use kern::KernTable;
pub use vertical::VerticalMetrics;
