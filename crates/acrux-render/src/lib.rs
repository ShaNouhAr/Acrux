//! # acrux-render
//!
//! Interprète le flux de contenu d'une page (ISO 32000-2 §8 et §9) et le
//! dessine via `acrux-graphics`, en utilisant `acrux-fonts` pour les glyphes et
//! `acrux-codecs` pour les images.
//!
//! | Module        | Rôle                                                     | Spec       |
//! |---------------|----------------------------------------------------------|------------|
//! | `content`     | découpage du flux en opérations, images en ligne         | §7.8.2     |
//! | `function`    | fonctions PDF (types 0, 2, 3, 4)                         | §7.10      |
//! | `colorspace`  | espaces colorimétriques → RVB                            | §8.6       |
//! | `state`       | état graphique et état texte                             | §8.4, §9.3 |
//! | `font`        | polices PDF : encodages, largeurs, substitution          | §9         |
//! | `image`       | décodage des images (bpc, Decode, masques, SMask)        | §8.9       |
//! | `shading`     | ombrages 1 à 7                                           | §8.7.4.5   |
//! | `interpreter` | opérateurs, chemins, texte, XObjects, motifs, transparence| §8, §9     |
//! | `page`        | matrice de base, contenu de page, annotations            | §12.5.5    |
//!
//! À venir : `text_extract` (texte structuré), `cache`.

pub mod colorspace;
pub mod content;
pub mod font;
pub mod function;
pub mod image;
pub mod interpreter;
pub mod page;
pub mod shading;
pub mod state;

pub use colorspace::ColorSpace;
pub use content::{parse_content, ContentLexer, InlineImage, Operation};
pub use function::Function;
pub use interpreter::{layers, Layer, RenderOptions, Renderer};
pub use page::{
    add_rotation, page_pixel_size, page_pixel_size_rotated, render_page, render_page_rotated,
    RenderedPage,
};
pub use state::GraphicsState;
