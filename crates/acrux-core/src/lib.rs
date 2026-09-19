//! # acrux-core
//!
//! Types fondamentaux partagés par tous les modules d'Acrux.
//! Ce crate ne dépend de rien d'autre que la bibliothèque standard et ne sait
//! rien du format PDF : il fournit la géométrie, les erreurs et les utilitaires.
//!
//! Voir `ARCHITECTURE.md` (section « core/ »).

pub mod error;
pub mod geom;
pub mod path;

pub use error::{Error, Result};
pub use geom::{Matrix, Point, Rect};
pub use path::{Path, PathCommand};
