//! Tables de références croisées (ISO 32000-2 §7.5.4), flux xref (§7.5.8),
//! fichiers hybrides (§7.5.8.4) et chaînage des mises à jour incrémentales
//! (§7.5.6).
//!
//! Le résultat est une [`Xref`] : pour chaque numéro d'objet, où le trouver,
//! plus le dictionnaire trailer fusionné (les sections les plus récentes
//! l'emportent).

use std::collections::{HashMap, HashSet};

use acrux_core::{Error, Result};

use crate::lexer::{Lexer, Token};
use crate::objects::{Dict, Name, Object, ObjectRef};
use crate::parser::Parser;

/// Localisation d'un objet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefEntry {
    /// Objet libre (type 0).
    Free,
    /// Objet à un offset du fichier (type 1).
    Offset {
        /// Offset en octets depuis le début du fichier.
        offset: u64,
        /// Génération attendue.
        generation: u16,
    },
    /// Objet compressé dans un flux d'objets (type 2, §7.5.7).
    InStream {
        /// Numéro du flux d'objets qui le contient.
        stream_number: u32,
        /// Index de l'objet dans ce flux.
        index: u32,
    },
}

/// Origine de la table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefKind {
    /// Table `xref` classique (au moins une section).
    Table,
    /// Flux xref (PDF 1.5+), éventuellement hybride.
    Stream,
    /// Table reconstruite par balayage du fichier (voir `repair`).
    Reconstructed,
}

/// Table de références croisées consolidée.
#[derive(Debug, Clone)]
pub struct Xref {
    /// Entrées par numéro d'objet.
    pub entries: HashMap<u32, XrefEntry>,
    /// Trailer fusionné : `/Root`, `/Info`, `/Encrypt`, `/ID`, `/Size`…
    pub trailer: Dict,
    /// Origine.
    pub kind: XrefKind,
    /// Avertissements non bloquants rencontrés pendant la lecture.
    pub warnings: Vec<String>,
}

impl Xref {
    /// Table vide.
    #[must_use]
    pub fn empty(kind: XrefKind) -> Self {
        Self {
            entries: HashMap::new(),
            trailer: Dict::new(),
            kind,
            warnings: Vec::new(),
        }
    }

    /// Entrée d'un objet.
    #[must_use]
    pub fn get(&self, number: u32) -> Option<XrefEntry> {
        self.entries.get(&number).copied()
    }

    /// Insère une entrée seulement si le numéro n'est pas déjà connu
    /// (les sections les plus récentes sont lues en premier).
    fn insert_if_absent(&mut self, number: u32, entry: XrefEntry) {
        self.entries.entry(number).or_insert(entry);
    }

    /// Fusionne un trailer : les clés déjà présentes sont conservées.
    fn merge_trailer(&mut self, t: &Dict) {
        for (k, v) in t {
            self.trailer.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
}

/// Décodeur de flux fourni par l'appelant (les filtres vivent dans `filters`).
pub type StreamDecoder<'d> = &'d dyn Fn(&Dict, &[u8]) -> Result<Vec<u8>>;

/// Lit la chaîne complète des tables xref à partir de `startxref`.
///
/// # Errors
/// `startxref` introuvable ou première section illisible : l'appelant doit
/// alors reconstruire la table par balayage.
pub fn read(data: &[u8], decode: StreamDecoder<'_>) -> Result<Xref> {
    let start = find_startxref(data)?;
    // Octets parasites avant `%PDF` : Acrobat interprète alors les offsets
    // relativement à l'en-tête. On essaie l'offset brut puis l'offset décalé.
    let base = header_offset(data);
    let mut xref = Xref::empty(XrefKind::Table);
    let mut visited = HashSet::new();
    let mut queue = vec![start];
    let mut first = true;
    while let Some(offset) = queue.pop() {
        if !visited.insert(offset) || visited.len() > 512 {
            xref.warnings.push(format!(
                "boucle ou chaîne /Prev excessive à l'offset {offset}"
            ));
            continue;
        }
        let attempt = match read_section(data, offset, decode) {
            Ok(s) => Ok(s),
            Err(e) if base > 0 => read_section(data, offset + base, decode).map_err(|_| e),
            Err(e) => Err(e),
        };
        let section = match attempt {
            Ok(s) => s,
            Err(e) if first => return Err(e),
            Err(e) => {
                xref.warnings
                    .push(format!("section xref illisible à l'offset {offset} : {e}"));
                continue;
            }
        };
        first = false;
        if section.is_stream {
            xref.kind = XrefKind::Stream;
        }
        for (n, e) in section.entries {
            xref.insert_if_absent(n, e);
        }
        // Ordre de priorité (§7.5.8.4) : la table, puis /XRefStm, puis /Prev.
        // La file est LIFO : on empile /Prev d'abord pour traiter /XRefStm avant.
        if let Some(Object::Integer(p)) = section.trailer.get(&Name::new("Prev")) {
            if let Ok(p) = usize::try_from(*p) {
                queue.push(p);
            }
        }
        if let Some(Object::Integer(p)) = section.trailer.get(&Name::new("XRefStm")) {
            if let Ok(p) = usize::try_from(*p) {
                queue.push(p);
            }
        }
        xref.merge_trailer(&section.trailer);
    }
    if !xref.trailer.contains_key(&Name::new("Root")) {
        return Err(Error::Corrupt("trailer sans /Root".into()));
    }
    Ok(xref)
}

/// Position de `%PDF-` dans les 1024 premiers octets (0 si absent ou en tête).
#[must_use]
pub fn header_offset(data: &[u8]) -> usize {
    let window = &data[..data.len().min(1024)];
    window.windows(5).position(|w| w == b"%PDF-").unwrap_or(0)
}

/// Offset annoncé par le dernier `startxref` du fichier (§7.5.5).
///
/// # Errors
/// Mot-clé absent ou valeur illisible.
pub fn find_startxref(data: &[u8]) -> Result<usize> {
    let lx = Lexer::new(data);
    let Some(pos) = lx.rfind(b"startxref") else {
        return Err(Error::Corrupt("`startxref` introuvable".into()));
    };
    let mut lx = Lexer::at(data, pos + b"startxref".len());
    match lx.next_token()? {
        Token::Integer(off) if off >= 0 => {
            let off = usize::try_from(off).unwrap_or(usize::MAX);
            if off >= data.len() {
                return Err(Error::Corrupt(format!("startxref {off} hors du fichier")));
            }
            Ok(off)
        }
        _ => Err(Error::Corrupt("valeur de startxref invalide".into())),
    }
}

struct Section {
    entries: Vec<(u32, XrefEntry)>,
    trailer: Dict,
    is_stream: bool,
}

/// Lit une section à `offset` : table classique (`xref`) ou flux xref (`n g obj`).
fn read_section(data: &[u8], offset: usize, decode: StreamDecoder<'_>) -> Result<Section> {
    let mut lx = Lexer::at(data, offset);
    lx.skip_whitespace();
    let probe = lx.next_token()?;
    match probe {
        Token::Keyword(k) if k == b"xref" => read_table(data, lx.pos()),
        Token::Integer(_) => read_stream(data, offset, decode),
        _ => Err(Error::Syntax {
            offset: offset as u64,
            message: "ni `xref` ni flux xref à cet offset".into(),
        }),
    }
}

/// Table classique (§7.5.4). `pos` est juste après le mot-clé `xref`.
fn read_table(data: &[u8], pos: usize) -> Result<Section> {
    let mut p = Parser::at(data, pos);
    let mut entries = Vec::new();
    loop {
        match p.peek_token()? {
            Token::Keyword(k) if k == b"trailer" => {
                p.next_token()?;
                break;
            }
            Token::Integer(_) => {}
            Token::Eof => break,
            other => {
                return Err(Error::Syntax {
                    offset: p.pos() as u64,
                    message: format!("token inattendu dans la table xref : {other:?}"),
                })
            }
        }
        let Token::Integer(start) = p.next_token()? else {
            unreachable!("vérifié par peek")
        };
        let count = match p.next_token()? {
            Token::Integer(c) if c >= 0 => c,
            _ => {
                return Err(Error::Syntax {
                    offset: p.pos() as u64,
                    message: "nombre d'entrées attendu".into(),
                })
            }
        };
        let start = u32::try_from(start.max(0)).unwrap_or(u32::MAX);
        for i in 0..count {
            // Chaque entrée : offset(10) génération(5) n|f. On lit par tokens pour
            // tolérer les entrées de 19 ou 21 octets des fichiers mal formés.
            let save = p.pos();
            let offset = match p.next_token()? {
                Token::Integer(o) => o,
                Token::Keyword(k) if k == b"trailer" => {
                    // Sous-section plus courte qu'annoncé.
                    p.seek(save);
                    break;
                }
                _ => {
                    return Err(Error::Syntax {
                        offset: save as u64,
                        message: "offset d'entrée xref attendu".into(),
                    })
                }
            };
            let generation = match p.next_token()? {
                Token::Integer(g) => u16::try_from(g.clamp(0, 65535)).unwrap_or(u16::MAX),
                _ => {
                    return Err(Error::Syntax {
                        offset: save as u64,
                        message: "génération d'entrée xref attendue".into(),
                    })
                }
            };
            let number = start.saturating_add(u32::try_from(i).unwrap_or(u32::MAX));
            match p.next_token()? {
                Token::Keyword(k) if k == b"n" => {
                    entries.push((
                        number,
                        XrefEntry::Offset {
                            offset: u64::try_from(offset.max(0)).unwrap_or(0),
                            generation,
                        },
                    ));
                }
                Token::Keyword(k) if k == b"f" => entries.push((number, XrefEntry::Free)),
                _ => {
                    return Err(Error::Syntax {
                        offset: save as u64,
                        message: "type d'entrée xref (n/f) attendu".into(),
                    })
                }
            }
        }
    }
    let trailer = match p.peek_token() {
        Ok(Token::DictOpen) => match p.parse_object()? {
            Object::Dict(d) => d,
            _ => Dict::new(),
        },
        _ => Dict::new(),
    };
    Ok(Section {
        entries,
        trailer,
        is_stream: false,
    })
}

/// Flux xref (§7.5.8) : l'objet à `offset` doit être un flux `/Type /XRef`.
fn read_stream(data: &[u8], offset: usize, decode: StreamDecoder<'_>) -> Result<Section> {
    let mut p = Parser::at(data, offset);
    let (_, obj) = p.parse_indirect(&|_| None)?;
    let Object::Stream { dict, raw } = obj else {
        return Err(Error::Syntax {
            offset: offset as u64,
            message: "flux xref attendu".into(),
        });
    };
    let content = decode(&dict, &raw)?;
    let widths: Vec<usize> = dict
        .get(&Name::new("W"))
        .and_then(Object::as_array)
        .map(|a| {
            a.iter()
                .map(|o| usize::try_from(o.as_i64().unwrap_or(0).max(0)).unwrap_or(0))
                .collect()
        })
        .unwrap_or_default();
    if widths.len() < 3 || widths.iter().any(|&w| w > 8) {
        return Err(Error::Corrupt("flux xref : /W invalide".into()));
    }
    let size = dict
        .get(&Name::new("Size"))
        .and_then(Object::as_i64)
        .unwrap_or(0);
    let index: Vec<i64> = dict
        .get(&Name::new("Index"))
        .and_then(Object::as_array)
        .map_or_else(
            || vec![0, size],
            |a| a.iter().map(|o| o.as_i64().unwrap_or(0)).collect(),
        );
    let row_len: usize = widths.iter().sum();
    if row_len == 0 {
        return Err(Error::Corrupt("flux xref : /W nul".into()));
    }
    let mut entries = Vec::new();
    let mut cursor = 0usize;
    'outer: for pair in index.chunks(2) {
        let (start, count) = match pair {
            [s, c] => (*s, *c),
            _ => break,
        };
        let start = u32::try_from(start.max(0)).unwrap_or(u32::MAX);
        for i in 0..count.max(0) {
            if cursor + row_len > content.len() {
                break 'outer; // flux tronqué : on garde ce qui a été lu
            }
            let row = &content[cursor..cursor + row_len];
            cursor += row_len;
            let mut f = [0u64; 3];
            let mut at = 0;
            for (k, &w) in widths.iter().enumerate().take(3) {
                f[k] = row[at..at + w]
                    .iter()
                    .fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
                at += w;
            }
            // Un champ de type de largeur 0 vaut 1 par défaut (§7.5.8.3, table 17).
            let kind = if widths[0] == 0 { 1 } else { f[0] };
            let number = start.saturating_add(u32::try_from(i).unwrap_or(u32::MAX));
            let entry = match kind {
                0 => XrefEntry::Free,
                1 => XrefEntry::Offset {
                    offset: f[1],
                    generation: u16::try_from(f[2].min(65535)).unwrap_or(u16::MAX),
                },
                2 => XrefEntry::InStream {
                    stream_number: u32::try_from(f[1]).unwrap_or(u32::MAX),
                    index: u32::try_from(f[2]).unwrap_or(u32::MAX),
                },
                _ => continue, // type inconnu : ignoré (§7.5.8.3)
            };
            entries.push((number, entry));
        }
    }
    // Le dictionnaire du flux est aussi le trailer (sans les clés propres au flux).
    let mut trailer = dict;
    for k in ["Length", "Filter", "DecodeParms", "W", "Index", "Type"] {
        trailer.remove(&Name::new(k));
    }
    Ok(Section {
        entries,
        trailer,
        is_stream: true,
    })
}

/// Petit utilitaire : référence depuis un trailer.
#[must_use]
pub fn trailer_ref(trailer: &Dict, key: &str) -> Option<ObjectRef> {
    match trailer.get(&Name::new(key)) {
        Some(Object::Reference(r)) => Some(*r),
        _ => None,
    }
}

#[cfg(test)]
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::unnecessary_wraps,
    clippy::format_collect
)]
mod tests {
    use super::*;

    fn identity(_: &Dict, raw: &[u8]) -> Result<Vec<u8>> {
        Ok(raw.to_vec())
    }

    /// Construit un PDF minimal avec table classique et offsets exacts.
    fn classic_pdf() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let o1 = out.len();
        out.extend_from_slice(b"1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n");
        let o2 = out.len();
        out.extend_from_slice(b"2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\n");
        let xref = out.len();
        out.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
        out.extend_from_slice(format!("{o1:010} 00000 n \n{o2:010} 00000 n \n").as_bytes());
        out.extend_from_slice(b"trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n");
        out.extend_from_slice(format!("{xref}\n%%EOF\n").as_bytes());
        out
    }

    #[test]
    fn classic_table() {
        let pdf = classic_pdf();
        let x = read(&pdf, &identity).unwrap();
        assert_eq!(x.kind, XrefKind::Table);
        assert_eq!(x.get(0), Some(XrefEntry::Free));
        assert!(matches!(
            x.get(1),
            Some(XrefEntry::Offset {
                offset: 9,
                generation: 0
            })
        ));
        assert!(matches!(x.get(2), Some(XrefEntry::Offset { .. })));
        assert_eq!(
            trailer_ref(&x.trailer, "Root"),
            Some(ObjectRef {
                number: 1,
                generation: 0
            })
        );
        assert!(x.warnings.is_empty());
    }

    #[test]
    fn incremental_update_overrides_older_entries() {
        let mut pdf = classic_pdf();
        let first_xref = find_startxref(&pdf).unwrap();
        let o1 = pdf.len();
        pdf.extend_from_slice(b"1 0 obj << /Type /Catalog /Pages 2 0 R /Version /1.7 >> endobj\n");
        let xref = pdf.len();
        pdf.extend_from_slice(b"xref\n1 1\n");
        pdf.extend_from_slice(format!("{o1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 3 /Root 1 0 R /Prev {first_xref} /Extra 1 >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );
        let x = read(&pdf, &identity).unwrap();
        assert!(
            matches!(x.get(1), Some(XrefEntry::Offset { offset, .. }) if offset as usize == o1)
        );
        assert!(
            matches!(x.get(2), Some(XrefEntry::Offset { .. })),
            "entrée héritée de /Prev"
        );
        assert_eq!(
            x.trailer.get(&Name::new("Extra")),
            Some(&Object::Integer(1))
        );
    }

    #[test]
    fn prev_loop_is_detected() {
        let mut pdf = classic_pdf();
        let xref = find_startxref(&pdf).unwrap();
        // Remplace le trailer par un /Prev qui pointe sur lui-même.
        let t = pdf.windows(7).position(|w| w == b"trailer").unwrap();
        pdf.truncate(t);
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 3 /Root 1 0 R /Prev {xref} >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );
        let x = read(&pdf, &identity).unwrap();
        assert_eq!(x.warnings.len(), 1);
    }

    #[test]
    fn xref_stream_unfiltered() {
        // /W [1 2 1] : type(1) champ2(2) champ3(1)
        let mut rows = Vec::new();
        rows.extend_from_slice(&[0, 0, 0, 0]); // 0 : libre
        rows.extend_from_slice(&[1, 0, 9, 0]); // 1 : offset 9
        rows.extend_from_slice(&[2, 0, 5, 3]); // 2 : dans le flux 5, index 3
        rows.extend_from_slice(&[9, 0, 0, 0]); // 3 : type inconnu, ignoré
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.5\n");
        pdf.extend_from_slice(b"1 0 obj << /Type /Catalog >> endobj\n");
        let xo = pdf.len();
        pdf.extend_from_slice(
            format!(
                "7 0 obj << /Type /XRef /W [1 2 1] /Size 4 /Root 1 0 R /Length {} >> stream\n",
                rows.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(&rows);
        pdf.extend_from_slice(format!("\nendstream endobj\nstartxref\n{xo}\n%%EOF").as_bytes());
        let x = read(&pdf, &identity).unwrap();
        assert_eq!(x.kind, XrefKind::Stream);
        assert_eq!(x.get(0), Some(XrefEntry::Free));
        assert_eq!(
            x.get(1),
            Some(XrefEntry::Offset {
                offset: 9,
                generation: 0
            })
        );
        assert_eq!(
            x.get(2),
            Some(XrefEntry::InStream {
                stream_number: 5,
                index: 3
            })
        );
        assert_eq!(x.get(3), None);
        assert!(!x.trailer.contains_key(&Name::new("W")));
        assert!(x.trailer.contains_key(&Name::new("Root")));
    }

    #[test]
    fn xref_stream_with_index_and_default_type() {
        // /W [0 1 0] : type par défaut 1, offset sur 1 octet ; /Index [10 2]
        let rows = [0x20, 0x30];
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.5\n");
        let xo = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj << /Type /XRef /W [0 1 0] /Index [10 2] /Size 12 /Root 1 0 R /Length 2 >> stream\n",
        );
        pdf.extend_from_slice(&rows);
        pdf.extend_from_slice(format!("\nendstream endobj\nstartxref\n{xo}\n%%EOF").as_bytes());
        let x = read(&pdf, &identity).unwrap();
        assert_eq!(
            x.get(10),
            Some(XrefEntry::Offset {
                offset: 0x20,
                generation: 0
            })
        );
        assert_eq!(
            x.get(11),
            Some(XrefEntry::Offset {
                offset: 0x30,
                generation: 0
            })
        );
    }

    #[test]
    fn missing_startxref_is_an_error() {
        assert!(read(b"%PDF-1.4\n1 0 obj << >> endobj", &identity).is_err());
        assert!(find_startxref(b"startxref\n999999\n%%EOF").is_err());
    }

    #[test]
    fn short_subsection_is_tolerated() {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\n");
        let xo = pdf.len();
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n0000000009 00000 n \ntrailer\n<< /Root 1 0 R >>\nstartxref\n");
        pdf.extend_from_slice(format!("{xo}\n%%EOF").as_bytes());
        let x = read(&pdf, &identity).unwrap();
        assert_eq!(x.entries.len(), 2);
    }
}
