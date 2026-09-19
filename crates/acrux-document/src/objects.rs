//! Types d'objets PDF (ISO 32000-2 §7.3).
//!
//! Huit types de base : booléen, entier, réel, chaîne, nom, tableau,
//! dictionnaire, flux, plus l'objet nul et les références indirectes.

use std::collections::BTreeMap;

/// Nom PDF (`/Type`, `/Pages`…). Stocké décodé (les séquences `#xx` sont résolues).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(pub Vec<u8>);

impl Name {
    /// Crée un nom depuis une chaîne ASCII.
    #[must_use]
    pub fn new(s: &str) -> Self {
        Name(s.as_bytes().to_vec())
    }

    /// Vue texte (avec remplacement des octets non UTF-8).
    #[must_use]
    pub fn as_str(&self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }
}

/// Référence indirecte `n g R` (§7.3.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectRef {
    /// Numéro d'objet.
    pub number: u32,
    /// Numéro de génération.
    pub generation: u16,
}

/// Dictionnaire PDF : clés ordonnées pour une sérialisation déterministe.
pub type Dict = BTreeMap<Name, Object>;

/// Objet PDF.
#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    /// `null`
    Null,
    /// `true` / `false`
    Bool(bool),
    /// Entier (§7.3.3).
    Integer(i64),
    /// Réel (§7.3.3).
    Real(f64),
    /// Chaîne d'octets, littérale ou hexadécimale (§7.3.4). L'encodage
    /// (PDFDoc, UTF-16BE, UTF-8) est interprété par l'appelant selon le contexte.
    String(Vec<u8>),
    /// Nom (§7.3.5).
    Name(Name),
    /// Tableau (§7.3.6).
    Array(Vec<Object>),
    /// Dictionnaire (§7.3.7).
    Dict(Dict),
    /// Flux : dictionnaire + données brutes encore encodées (§7.3.8).
    Stream {
        /// Dictionnaire du flux (`/Length`, `/Filter`…).
        dict: Dict,
        /// Données telles que présentes dans le fichier, avant application des filtres.
        raw: Vec<u8>,
    },
    /// Référence indirecte (§7.3.10).
    Reference(ObjectRef),
}

impl Object {
    /// Valeur numérique, que l'objet soit entier ou réel.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Object::Integer(i) => Some(*i as f64),
            Object::Real(r) => Some(*r),
            _ => None,
        }
    }

    /// Valeur entière.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Object::Integer(i) => Some(*i),
            _ => None,
        }
    }

    /// Nom, si l'objet en est un.
    #[must_use]
    pub fn as_name(&self) -> Option<&Name> {
        match self {
            Object::Name(n) => Some(n),
            _ => None,
        }
    }

    /// Dictionnaire, y compris celui d'un flux.
    #[must_use]
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Object::Dict(d) | Object::Stream { dict: d, .. } => Some(d),
            _ => None,
        }
    }

    /// Tableau.
    #[must_use]
    pub fn as_array(&self) -> Option<&[Object]> {
        match self {
            Object::Array(a) => Some(a),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_accessors() {
        assert_eq!(Object::Integer(3).as_f64(), Some(3.0));
        assert_eq!(Object::Real(2.5).as_f64(), Some(2.5));
        assert_eq!(Object::Real(2.5).as_i64(), None);
        assert_eq!(Object::Null.as_f64(), None);
    }

    #[test]
    fn stream_exposes_its_dict() {
        let mut d = Dict::new();
        d.insert(Name::new("Length"), Object::Integer(0));
        let s = Object::Stream {
            dict: d.clone(),
            raw: vec![],
        };
        assert_eq!(s.as_dict(), Some(&d));
    }
}
