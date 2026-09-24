//! Fractionner un document en plusieurs fichiers (voir
//! FONCTIONNALITES_ADOBE_ACROBAT.txt §5, « Diviser le document ») : par
//! nombre de pages, par signets de premier niveau, par taille maximale, ou
//! par groupes de pages donnés (un fichier par page, une sélection).
//!
//! Le travail se fait en deux temps. [`plan_parts`] décide quelles pages
//! vont ensemble, sans rien écrire ; [`write_parts`] écrit ensuite chaque
//! partie et la passe à un puits, qui l'enregistre où il veut — rien n'est
//! gardé en mémoire d'une partie à l'autre, ce qui compte pour un document
//! de mille pages. Chaque partie est une extraction
//! ([`super::extract_pages`]) enregistrée sans les objets inatteignables :
//! une partie ne garde ni les images ni les polices des autres, et un lien
//! vers une page restée dans une autre partie ne l'y entraîne pas.

use std::collections::{HashMap, HashSet};

use acrux_core::{Error, Result};
use acrux_document::{collect_pages, writer, Document, Object, ObjectRef, Page, SaveOptions};

use super::extract_pages;
use crate::navigation::{outline, Action, PageIndex};

/// Nombre maximal d'écritures d'essai pour ajuster une partie à la taille
/// maximale : l'estimation est bonne, mais elle ignore la table xref et les
/// variations d'écriture ; au-delà, la partie est acceptée telle quelle.
const MAX_TRIES: usize = 8;

/// Ce que coûte une page, hors de ce qu'elle désigne : son dictionnaire,
/// sa place dans la table xref et dans `/Kids`.
const PAGE_BASE: u64 = 192;

/// Ce que coûte un fichier vide : en-tête, catalogue, arbre des pages,
/// table xref et trailer, métadonnées.
const FILE_BASE: u64 = 512;

/// Comment fractionner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitPlan {
    /// Des tranches de `n` pages (la dernière peut être plus courte).
    EveryN(usize),
    /// Une partie par signet de premier niveau, de sa page à la page qui
    /// précède le signet suivant.
    TopBookmarks,
    /// Des parties d'au plus tant d'octets.
    MaxBytes(u64),
    /// Des groupes de pages donnés : un fichier par page, une sélection.
    Groups(Vec<Vec<usize>>),
}

/// Une partie : ses pages, dans l'ordre, et le titre du signet qui l'ouvre
/// quand il y en a un (il entre dans le nom du fichier).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// Indices des pages (0 = première).
    pub pages: Vec<usize>,
    /// Titre du signet de premier niveau qui ouvre la partie.
    pub title: Option<String>,
}

/// Le découpage décidé, et ce qu'il faut en dire à l'utilisateur.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parts {
    /// Les parties, dans l'ordre du document.
    pub parts: Vec<Part>,
    /// Avertissements : une page plus grosse à elle seule que la taille
    /// maximale, par exemple. Le fractionnement a lieu quand même.
    pub warnings: Vec<String>,
}

/// Puits des parties écrites : il reçoit le rang de la partie, sa
/// description et ses octets, et les enregistre où il veut.
pub type PartSink<'a> = dyn FnMut(usize, &Part, Vec<u8>) -> Result<()> + 'a;

/// Décide des parties, sans rien écrire. `options` sert au découpage par
/// taille, qui vérifie son estimation par une vraie écriture.
///
/// # Errors
/// Tranche de zéro page, taille nulle, groupe vide ou hors du document,
/// document sans signet de premier niveau (pour ce découpage-là), objets
/// illisibles.
pub fn plan_parts(doc: &Document, plan: &SplitPlan, options: &SaveOptions) -> Result<Parts> {
    let pages = collect_pages(doc)?;
    let count = pages.len();
    if count == 0 {
        return Err(Error::Corrupt("le document n'a pas de page".into()));
    }
    let parts = match plan {
        SplitPlan::EveryN(0) => {
            return Err(Error::Corrupt(
                "une partie doit compter au moins une page".into(),
            ))
        }
        SplitPlan::EveryN(n) => (0..count)
            .step_by(*n)
            .map(|start| Part {
                pages: (start..(start + n).min(count)).collect(),
                title: None,
            })
            .collect(),
        SplitPlan::TopBookmarks => by_bookmarks(doc, &pages)?,
        SplitPlan::MaxBytes(limit) => return by_size(doc, &pages, *limit, options),
        SplitPlan::Groups(groups) => {
            for group in groups {
                if group.is_empty() {
                    return Err(Error::Corrupt("groupe de pages vide".into()));
                }
                if let Some(&bad) = group.iter().find(|&&p| p >= count) {
                    return Err(Error::Corrupt(format!(
                        "page {} hors du document (1 à {count})",
                        bad + 1
                    )));
                }
            }
            if groups.is_empty() {
                return Err(Error::Corrupt("aucune page à écrire".into()));
            }
            groups
                .iter()
                .map(|g| Part {
                    pages: g.clone(),
                    title: None,
                })
                .collect()
        }
    };
    Ok(Parts {
        parts,
        warnings: Vec::new(),
    })
}

/// Une partie par signet de premier niveau qui mène à une page du
/// document. Deux signets sur la même page n'en font qu'une (le premier
/// donne son titre) ; les pages qui précèdent le premier signet forment une
/// partie sans titre.
fn by_bookmarks(doc: &Document, pages: &[Page]) -> Result<Vec<Part>> {
    let items = outline(doc, &PageIndex::new(pages))?;
    let mut starts: Vec<(usize, String)> = items
        .iter()
        .filter_map(|item| match &item.action {
            Some(Action::GoTo(d)) if d.page < pages.len() => Some((d.page, item.title.clone())),
            _ => None,
        })
        .collect();
    // Tri stable : à page égale, le premier signet reste devant.
    starts.sort_by_key(|(page, _)| *page);
    starts.dedup_by_key(|(page, _)| *page);
    let Some(&(first, _)) = starts.first() else {
        return Err(Error::Unsupported(
            "le document n'a pas de signet de premier niveau".into(),
        ));
    };
    let mut parts = Vec::with_capacity(starts.len() + 1);
    if first > 0 {
        parts.push(Part {
            pages: (0..first).collect(),
            title: None,
        });
    }
    for (k, (start, title)) in starts.iter().enumerate() {
        let end = starts.get(k + 1).map_or(pages.len(), |(next, _)| *next);
        let title = title.trim();
        parts.push(Part {
            pages: (*start..end).collect(),
            title: (!title.is_empty()).then(|| title.to_string()),
        });
    }
    Ok(parts)
}

/// Poids d'un objet dans un fichier écrit : sa syntaxe, et les octets de
/// son flux tels qu'ils seront recopiés.
fn object_weight(obj: &Object) -> u64 {
    let mut out = Vec::new();
    match obj {
        Object::Stream { dict, raw } => {
            writer::write_object(&Object::Dict(dict.clone()), &mut out);
            (out.len() + raw.len()) as u64 + 48
        }
        other => {
            writer::write_object(other, &mut out);
            out.len() as u64 + 24
        }
    }
}

/// Objets qu'emporte une page quand on l'extrait : ceux qu'on atteint
/// depuis son dictionnaire, sans passer par `/Parent` ni par une autre page
/// (la même règle que l'extraction, voir l'explication en tête de
/// [`super`]). Le poids de chaque objet est calculé une fois, dans
/// `weights`, et partagé entre les pages.
fn page_footprint(
    doc: &Document,
    page: &Page,
    page_numbers: &HashSet<u32>,
    weights: &mut HashMap<u32, u64>,
) -> HashSet<u32> {
    let mut seen = HashSet::new();
    let mut stack: Vec<Object> = page
        .dict
        .iter()
        .filter(|(k, _)| k.0 != b"Parent")
        .map(|(_, v)| v.clone())
        .collect();
    while let Some(obj) = stack.pop() {
        match obj {
            Object::Reference(r) => {
                if page_numbers.contains(&r.number) || !seen.insert(r.number) {
                    continue;
                }
                let Ok(target) = doc.get(r) else {
                    continue;
                };
                weights
                    .entry(r.number)
                    .or_insert_with(|| object_weight(&target));
                match &*target {
                    Object::Dict(d) | Object::Stream { dict: d, .. } => stack.extend(
                        d.iter()
                            .filter(|(k, _)| k.0 != b"Parent")
                            .map(|(_, v)| v.clone()),
                    ),
                    Object::Array(items) => stack.extend(items.iter().cloned()),
                    _ => {}
                }
            }
            Object::Array(items) => stack.extend(items),
            Object::Dict(d) | Object::Stream { dict: d, .. } => stack.extend(
                d.into_iter()
                    .filter(|(k, _)| k.0 != b"Parent")
                    .map(|(_, v)| v),
            ),
            _ => {}
        }
    }
    seen
}

/// Des parties d'au plus `limit` octets.
///
/// On ajoute des pages tant que la somme des objets de leur **union** reste
/// sous la limite : une police ou une image partagée par dix pages ne compte
/// qu'une fois. L'estimation est ensuite vérifiée en écrivant réellement la
/// partie ; trop grosse, elle est raccourcie en proportion de l'excès (d'une
/// page au moins) puis réécrite, au plus [`MAX_TRIES`] fois. Une page seule
/// plus grosse que la limite fait une partie à elle seule, et un
/// avertissement le dit : elle ne fait pas échouer tout le fractionnement.
fn by_size(doc: &Document, pages: &[Page], limit: u64, options: &SaveOptions) -> Result<Parts> {
    if limit == 0 {
        return Err(Error::Corrupt(
            "la taille maximale doit être positive".into(),
        ));
    }
    let page_numbers: HashSet<u32> = pages
        .iter()
        .filter_map(|p| p.reference.map(|r: ObjectRef| r.number))
        .collect();
    let mut weights = HashMap::new();
    let footprints: Vec<HashSet<u32>> = pages
        .iter()
        .map(|p| page_footprint(doc, p, &page_numbers, &mut weights))
        .collect();
    let mut out = Parts::default();
    let mut start = 0;
    while start < pages.len() {
        let mut union: HashSet<u32> = HashSet::new();
        let mut total = FILE_BASE;
        let mut end = start;
        while let Some(footprint) = footprints.get(end) {
            let added: u64 = footprint
                .iter()
                .filter(|n| !union.contains(n))
                .map(|n| weights.get(n).copied().unwrap_or(0))
                .sum::<u64>()
                + PAGE_BASE;
            if end > start && total + added > limit {
                break;
            }
            total += added;
            union.extend(footprint.iter().copied());
            end += 1;
        }
        let mut tries = 0;
        let size = loop {
            let list: Vec<usize> = (start..end).collect();
            let size = part_bytes(doc, &list, options)?.len() as u64;
            let len = end - start;
            if size <= limit || len == 1 || tries >= MAX_TRIES {
                break size;
            }
            // Raccourcir en proportion de l'excès, d'une page au moins.
            let keep =
                usize::try_from(u128::from(limit) * len as u128 / u128::from(size)).unwrap_or(1);
            end = start + keep.clamp(1, len - 1);
            tries += 1;
        };
        if size > limit {
            out.warnings.push(if end - start == 1 {
                format!(
                    "la page {} dépasse à elle seule la taille maximale ({} octets)",
                    start + 1,
                    size
                )
            } else {
                format!(
                    "les pages {} à {} dépassent la taille maximale ({} octets)",
                    start + 1,
                    end,
                    size
                )
            });
        }
        out.parts.push(Part {
            pages: (start..end).collect(),
            title: None,
        });
        start = end;
    }
    Ok(out)
}

/// Octets d'une partie : l'extraction de ses pages, écrite sans les objets
/// inatteignables.
fn part_bytes(doc: &Document, pages: &[usize], options: &SaveOptions) -> Result<Vec<u8>> {
    let part = extract_pages(doc, pages)?;
    part.save_full_with(&SaveOptions {
        drop_unreferenced: true,
        ..options.clone()
    })
}

/// Écrit chaque partie et la passe au puits, avec son rang et sa
/// description ; rien n'est gardé d'une partie à l'autre. Une erreur du
/// puits (disque plein) arrête tout.
///
/// Un document chiffré se fractionne **en clair** : l'extraction copie les
/// flux déjà déchiffrés.
///
/// # Errors
/// Page illisible, ou erreur rendue par le puits.
pub fn write_parts(
    doc: &Document,
    parts: &[Part],
    options: &SaveOptions,
    sink: &mut PartSink<'_>,
) -> Result<()> {
    for (index, part) in parts.iter().enumerate() {
        let bytes = part_bytes(doc, &part.pages, options)?;
        sink(index, part, bytes)?;
    }
    Ok(())
}

/// Nom du fichier d'une partie : `rapport-01.pdf` … `rapport-12.pdf` (les
/// zéros de tête gardent l'ordre dans l'Explorateur), ou
/// `rapport-01 Introduction.pdf` quand un signet ouvre la partie. Le titre
/// est assaini pour Windows : sans `\ / : * ? " < > |` ni caractère de
/// contrôle, sans point ni espace final, 60 caractères au plus, et jamais un
/// nom réservé (`CON`, `NUL`, `COM1`…).
#[must_use]
pub fn part_file_name(stem: &str, index: usize, total: usize, title: Option<&str>) -> String {
    let digits = total.max(index + 1).to_string().len();
    let number = format!("{:0digits$}", index + 1);
    match title.map(clean_title).filter(|t| !t.is_empty()) {
        Some(t) => format!("{stem}-{number} {t}.pdf"),
        None => format!("{stem}-{number}.pdf"),
    }
}

/// Titre de signet rendu acceptable dans un nom de fichier Windows.
fn clean_title(title: &str) -> String {
    let kept: String = title
        .chars()
        .filter(|&c| {
            !c.is_control() && !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
        .collect();
    let words: Vec<&str> = kept.split_whitespace().collect();
    let mut short: String = words.join(" ").chars().take(60).collect();
    // Windows refuse un nom qui finit par un point ou une espace.
    while short.ends_with(['.', ' ']) {
        short.pop();
    }
    if reserved_name(&short) {
        short.push('_');
    }
    short
}

/// Vrai pour un nom que Windows réserve à un périphérique.
fn reserved_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let (Some(head), Some(tail)) = (upper.get(..3), upper.get(3..)) else {
        return false;
    };
    matches!(head, "COM" | "LPT")
        && tail.len() == 1
        && tail.bytes().all(|b| (b'1'..=b'9').contains(&b))
}

/// Une taille lue comme on l'écrit : « 2 », « 2,5 », « 500 Ko », « 1.5M »,
/// « 800 KB ». Les unités sont binaires (1 Ko = 1024 octets), comme
/// l'Explorateur les compte ; un nombre sans unité vaut `bare` octets par
/// unité — des mégaoctets dans la fenêtre « Fractionner », des octets pour
/// `acr split`. `None` pour une taille nulle, négative ou illisible.
#[must_use]
pub fn parse_size(text: &str, bare: u64) -> Option<u64> {
    let t = text.trim();
    let cut = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
        .unwrap_or(t.len());
    let (number, unit) = t.split_at(cut);
    let value: f64 = number.replace(',', ".").parse().ok()?;
    let factor: u64 = match unit.trim().to_lowercase().as_str() {
        "" => bare,
        "o" | "b" | "octet" | "octets" | "bytes" => 1,
        "k" | "ko" | "kb" | "kio" | "kib" => 1 << 10,
        "m" | "mo" | "mb" | "mio" | "mib" => 1 << 20,
        "g" | "go" | "gb" | "gio" | "gib" => 1 << 30,
        _ => return None,
    };
    // Des tailles de fichier : bien en deçà de 2^53, la conversion est
    // exacte à l'octet près, et le résultat est borné avant d'être rendu.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    {
        let bytes = (value * factor as f64).round();
        (bytes.is_finite() && (1.0..1e15).contains(&bytes)).then_some(bytes as u64)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::outline_edit::{set_outline, OutlineNode};

    /// Un document de pages dont chacune dessine un flux de la taille
    /// donnée (des octets de commentaire : le rendu n'en fait rien).
    fn pdf_with_sizes(sizes: &[usize]) -> Document {
        let n = sizes.len();
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            String::new(),
        ];
        let mut kids = Vec::new();
        for (i, &size) in sizes.iter().enumerate() {
            let page = 3 + 2 * i;
            kids.push(format!("{page} 0 R"));
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents {} 0 R >>",
                page + 1
            ));
            let body = format!("%{}\n", "x".repeat(size));
            objects.push(format!(
                "<< /Length {} >>\nstream\n{body}\nendstream",
                body.len()
            ));
        }
        objects[1] = format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" "));
        let mut src = b"%PDF-1.4\n".to_vec();
        for (i, body) in objects.iter().enumerate() {
            src.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        Document::from_bytes(src).unwrap()
    }

    fn pages_of(parts: &Parts) -> Vec<Vec<usize>> {
        parts.parts.iter().map(|p| p.pages.clone()).collect()
    }

    fn plan(doc: &Document, plan: &SplitPlan) -> Result<Parts> {
        plan_parts(doc, plan, &SaveOptions::default())
    }

    #[test]
    fn tranches_and_groups() {
        let doc = pdf_with_sizes(&[10; 5]);
        let parts = plan(&doc, &SplitPlan::EveryN(2)).unwrap();
        assert_eq!(pages_of(&parts), vec![vec![0, 1], vec![2, 3], vec![4]]);
        assert_eq!(
            pages_of(&plan(&doc, &SplitPlan::EveryN(9)).unwrap()),
            vec![vec![0, 1, 2, 3, 4]]
        );
        assert!(plan(&doc, &SplitPlan::EveryN(0)).is_err());
        let each = SplitPlan::Groups((0..5).map(|p| vec![p]).collect());
        assert_eq!(plan(&doc, &each).unwrap().parts.len(), 5);
        assert!(plan(&doc, &SplitPlan::Groups(vec![vec![0, 5]])).is_err());
        assert!(plan(&doc, &SplitPlan::Groups(vec![vec![]])).is_err());
        assert!(plan(&doc, &SplitPlan::Groups(Vec::new())).is_err());
    }

    #[test]
    fn one_part_per_top_bookmark() {
        let doc = pdf_with_sizes(&[10; 5]);
        assert!(
            plan(&doc, &SplitPlan::TopBookmarks).is_err(),
            "sans signet, une erreur claire"
        );
        let mut first = OutlineNode::to_page("Introduction", 0);
        first.children.push(OutlineNode::to_page("Détail", 1));
        set_outline(
            &doc,
            &[
                first,
                OutlineNode::to_page("Méthode", 2),
                OutlineNode::to_page("Résultats", 3),
                OutlineNode::to_page("Doublon", 3),
            ],
        )
        .unwrap();
        let parts = plan(&doc, &SplitPlan::TopBookmarks).unwrap();
        assert_eq!(pages_of(&parts), vec![vec![0, 1], vec![2], vec![3, 4]]);
        let titles: Vec<Option<&str>> = parts.parts.iter().map(|p| p.title.as_deref()).collect();
        assert_eq!(
            titles,
            vec![Some("Introduction"), Some("Méthode"), Some("Résultats")]
        );
        // Un premier signet qui ne commence pas à la première page.
        set_outline(
            &doc,
            &[
                OutlineNode::to_page("Suite", 1),
                OutlineNode::to_page("Fin", 4),
            ],
        )
        .unwrap();
        let parts = plan(&doc, &SplitPlan::TopBookmarks).unwrap();
        assert_eq!(pages_of(&parts), vec![vec![0], vec![1, 2, 3], vec![4]]);
        assert_eq!(parts.parts[0].title, None, "partie sans titre");
    }

    #[test]
    fn parts_stay_under_the_size_limit() {
        let sizes = [3000, 3000, 3000, 20_000, 3000, 3000, 100];
        let doc = pdf_with_sizes(&sizes);
        let limit = 8000;
        let parts = plan(&doc, &SplitPlan::MaxBytes(limit)).unwrap();
        let mut written = Vec::new();
        write_parts(
            &doc,
            &parts.parts,
            &SaveOptions::default(),
            &mut |i, part, bytes| {
                written.push((i, part.pages.clone(), bytes.len() as u64));
                Ok(())
            },
        )
        .unwrap();
        let all: Vec<usize> = written.iter().flat_map(|(_, p, _)| p.clone()).collect();
        assert_eq!(
            all,
            (0..sizes.len()).collect::<Vec<_>>(),
            "toutes les pages, une fois"
        );
        for (_, pages, size) in &written {
            if pages == &vec![3] {
                assert!(*size > limit, "la grosse page est seule");
            } else {
                assert!(*size <= limit, "{pages:?} : {size} octets");
            }
        }
        assert!(
            written.iter().any(|(_, p, _)| p.len() > 1),
            "des pages ensemble"
        );
        assert_eq!(parts.warnings.len(), 1, "{:?}", parts.warnings);
        assert!(parts.warnings[0].contains("page 4"));
        assert!(plan(&doc, &SplitPlan::MaxBytes(0)).is_err());
    }

    /// Une partie ne garde que ce que ses pages dessinent : chaque page
    /// écrite seule pèse moins que le document entier.
    #[test]
    fn a_part_does_not_carry_the_other_pages() {
        let doc = pdf_with_sizes(&[5000, 5000, 5000]);
        let whole = doc.save_full().unwrap().len();
        let parts = plan(&doc, &SplitPlan::EveryN(1)).unwrap();
        write_parts(
            &doc,
            &parts.parts,
            &SaveOptions::default(),
            &mut |_, _, bytes| {
                assert!(
                    bytes.len() < whole / 2,
                    "{} octets sur {whole}",
                    bytes.len()
                );
                let part = Document::from_bytes(bytes).unwrap();
                assert_eq!(collect_pages(&part).unwrap().len(), 1);
                Ok(())
            },
        )
        .unwrap();
    }

    #[test]
    fn sizes_read_as_people_write_them() {
        const MIB: u64 = 1 << 20;
        assert_eq!(parse_size("2", MIB), Some(2 * MIB), "sans unité : des Mo");
        assert_eq!(parse_size("2", 1), Some(2), "sans unité : des octets");
        assert_eq!(parse_size("2,5", MIB), Some(5 * MIB / 2));
        assert_eq!(parse_size(" 500 Ko ", MIB), Some(500 * 1024));
        assert_eq!(parse_size("1.5M", 1), Some(3 * MIB / 2));
        assert_eq!(parse_size("200K", 1), Some(200 * 1024));
        assert_eq!(parse_size("1 Go", 1), Some(1 << 30));
        assert_eq!(parse_size("0", MIB), None);
        assert_eq!(parse_size("abc", MIB), None);
        assert_eq!(parse_size("-2", MIB), None);
        assert_eq!(parse_size("2 lieues", MIB), None);
        assert_eq!(parse_size("", MIB), None);
    }

    #[test]
    fn file_names_sort_and_stay_valid() {
        assert_eq!(part_file_name("rapport", 0, 12, None), "rapport-01.pdf");
        assert_eq!(part_file_name("rapport", 11, 12, None), "rapport-12.pdf");
        assert_eq!(part_file_name("rapport", 2, 5, None), "rapport-3.pdf");
        assert_eq!(part_file_name("r", 99, 100, None), "r-100.pdf");
        assert_eq!(
            part_file_name("rapport", 0, 3, Some("Introduction")),
            "rapport-1 Introduction.pdf"
        );
        assert_eq!(part_file_name("r", 0, 1, Some("a/b:c?")), "r-1 abc.pdf");
        assert_eq!(part_file_name("r", 0, 1, Some("CON")), "r-1 CON_.pdf");
        assert_eq!(part_file_name("r", 0, 1, Some("com3")), "r-1 com3_.pdf");
        assert_eq!(part_file_name("r", 0, 1, Some("Fin.")), "r-1 Fin.pdf");
        assert_eq!(part_file_name("r", 0, 1, Some("  ?? ")), "r-1.pdf");
        assert_eq!(part_file_name("r", 0, 1, Some("a\u{7}\tb")), "r-1 ab.pdf");
        let long = "é".repeat(80);
        let name = part_file_name("r", 0, 1, Some(&long));
        assert_eq!(name.chars().filter(|&c| c == 'é').count(), 60);
    }
}
