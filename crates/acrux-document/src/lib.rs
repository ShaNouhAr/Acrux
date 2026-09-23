//! # acrux-document
//!
//! Modèle objet PDF : lecture et écriture de la **syntaxe** du fichier
//! (ISO 32000-2 chapitre 7). Ce crate ne sait pas dessiner ; il expose des
//! objets, des flux décodés et l'arbre des pages aux couches supérieures.
//!
//! | Module      | Rôle                                                   | Spec      |
//! |-------------|--------------------------------------------------------|-----------|
//! | `asn1`      | DER strict : lecture et écriture (X.690)               | —         |
//! | `lexer`     | découpage en tokens                                    | §7.2      |
//! | `objects`   | types d'objets                                         | §7.3      |
//! | `parser`    | assemblage des objets, objets indirects, flux          | §7.3      |
//! | `filters`   | chaînage des filtres de flux (codecs dans `acrux-codecs`) | §7.4      |
//! | `xref`      | tables et flux de références croisées, mises à jour    | §7.5      |
//! | `repair`    | reconstruction par balayage des fichiers cassés        | —         |
//! | `document`  | accès aux objets, cache, flux d'objets, réparation     | §7.5.7    |
//! | `pages`     | arbre des pages et héritage                            | §7.7.3    |
//! | `text`      | chaînes de texte (UTF-16, PDFDoc)                      | §7.9.2    |
//! | `writer`    | sérialisation en syntaxe PDF                           | §7.3      |
//! | `crypt`     | chiffrement standard (RC4, AES-128/256, R2 à R6), aléa ChaCha20 | §7.6      |
//! | `edit`      | modifications, enregistrement incrémental et complet   | §7.5.6    |
//! | `protect`   | pose / retrait du chiffrement (AES-256 R6), permissions, force des mots de passe | §7.6.4    |
//!
//! À venir : `structure` (§14.7), `metadata` (§14.3).

pub mod asn1;
pub mod crypt;
pub mod document;
pub mod edit;
pub mod filters;
pub mod lexer;
pub mod objects;
pub mod pages;
pub mod parser;
pub mod protect;
pub mod repair;
pub mod text;
pub mod writer;
pub mod xref;

pub use document::{Document, Resolved};
pub use edit::SaveOptions;
pub use lexer::{Lexer, Token};
pub use objects::{Dict, Name, Object, ObjectRef};
pub use pages::{collect_pages, Page};
pub use parser::Parser;
pub use xref::{Xref, XrefEntry, XrefKind};
