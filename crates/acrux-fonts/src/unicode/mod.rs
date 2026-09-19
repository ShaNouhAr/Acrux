//! Données et algorithmes Unicode nécessaires à la composition typographique.
//!
//! Tout est écrit ici, sans dépendance ni table copiée : les valeurs sont
//! saisies à la main depuis les fichiers normatifs (`UnicodeData.txt`,
//! `ArabicShaping.txt`, `DerivedBidiClass.txt`, `GraphemeBreakProperty.txt`)
//! pour les plages qui nous servent, et chaque module dit explicitement ce
//! qu'il couvre et ce qu'il ne couvre pas.
//!
//! | Module        | Norme    | Rôle dans le shaping                        |
//! |---------------|----------|---------------------------------------------|
//! | [`ccc`]       | UAX #15  | Ordonner les marques avant `ccmp` et `mark` |
//! | [`joining`]   | UAX #44  | Choisir `init`, `medi`, `fina`, `isol`      |
//! | [`bidi_class`]| UAX #9   | Classes bidirectionnelles                   |
//! | [`bidi`]      | UAX #9   | Ordre visuel des passages mixtes            |
//! | [`grapheme`]  | UAX #29  | Numéros de grappe justes pour le curseur    |
//! | [`script`]    | OpenType | Tag de script (`latn`, `arab`…)             |

pub mod bidi;
pub mod bidi_class;
pub mod ccc;
pub mod grapheme;
pub mod joining;
pub mod script;

pub use bidi::{reorder_visual, BaseDirection, BidiInfo, Run};
pub use bidi_class::{bidi_class, BidiClass};
pub use ccc::combining_class;
pub use grapheme::{cluster_boundaries, clusters};
pub use joining::{joining_forms, joining_type, JoiningForm, JoiningType};
pub use script::{dominant_script, is_rtl_script, script_tag};
