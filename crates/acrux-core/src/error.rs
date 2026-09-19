//! Erreurs communes.
//!
//! Principe (voir CHARTE_PROJET.md §1.4) : un fichier corrompu ne fait jamais
//! planter l'application. Les modules retournent une [`Error`] typée que
//! l'appelant peut afficher, journaliser ou contourner.

use std::fmt;

/// Alias de résultat utilisé dans tout le projet.
pub type Result<T> = std::result::Result<T, Error>;

/// Erreur de haut niveau. Chaque module ajoute sa propre variante plutôt que
/// d'utiliser des chaînes libres, afin que l'interface puisse réagir finement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Erreur d'entrée / sortie (fichier introuvable, lecture impossible…).
    Io(String),
    /// Le fichier n'est pas un PDF ou son en-tête est illisible.
    NotAPdf,
    /// Syntaxe PDF invalide à la position donnée (octet depuis le début du fichier).
    Syntax {
        /// Position de l'erreur dans le fichier.
        offset: u64,
        /// Description lisible.
        message: String,
    },
    /// Référence vers un objet inexistant.
    MissingObject {
        /// Numéro d'objet.
        number: u32,
        /// Numéro de génération.
        generation: u16,
    },
    /// Un filtre de flux ou un codec n'est pas encore pris en charge.
    Unsupported(String),
    /// Le document est chiffré et le mot de passe est absent ou incorrect.
    Encrypted,
    /// Données corrompues détectées dans un flux ou une police.
    Corrupt(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(m) => write!(f, "erreur d'entrée/sortie : {m}"),
            Error::NotAPdf => write!(f, "ce fichier n'est pas un PDF"),
            Error::Syntax { offset, message } => {
                write!(f, "syntaxe invalide à l'octet {offset} : {message}")
            }
            Error::MissingObject { number, generation } => {
                write!(f, "objet {number} {generation} R introuvable")
            }
            Error::Unsupported(m) => write!(f, "non pris en charge : {m}"),
            Error::Encrypted => write!(f, "document chiffré : mot de passe requis"),
            Error::Corrupt(m) => write!(f, "données corrompues : {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}
