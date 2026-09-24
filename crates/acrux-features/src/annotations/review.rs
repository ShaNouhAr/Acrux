//! Relecture : répondre à un commentaire, lui donner un statut, et lire les
//! fils de discussion d'un document (§12.5.6.2 et §12.5.6.3).
//!
//! # Réponses et états
//!
//! Une **réponse** est une note (`/Text`) qui désigne son commentaire par
//! `/IRT` (« in reply to »), avec `/RT /R`. Elle partage son rectangle et
//! n'a pas d'apparence : ce n'est pas un dessin de plus sur la page, c'est
//! une ligne de plus dans le fil. Un **état** est une note cachée qui porte
//! `/State` et `/StateModel` : « Accepté » (`/StateModel (Review)`) ou
//! « Coché » (`/StateModel (Marked)`). Comme chez Acrobat, les états
//! s'ajoutent sans s'effacer : c'est l'historique des décisions, et le plus
//! récent (par `/M`) fait foi.
//!
//! # Fils
//!
//! [`comment_threads`] range les annotations d'un document en fils : chaque
//! commentaire racine avec ses réponses (et les réponses aux réponses,
//! indentées), son statut et sa case. Les membres d'un groupe (le barré d'un
//! remplacement) et les états n'y figurent pas à part. Le calcul est pur —
//! un document entre, une liste sort — : la ligne de commande, le panneau
//! des commentaires et la bulle d'une note lisent la même chose.

use std::collections::{HashMap, HashSet};

use acrux_core::{Error, Rect, Result};
use acrux_document::{Document, Name, Object, ObjectRef, Page};

use super::{
    annots_of, base_dict, color_array, current_page, encode_text, list_annotations, push_to_annots,
    rect_object, AnnotMeta, AnnotationInfo, Rgb,
};

/// Statut de relecture d'un commentaire (`/StateModel (Review)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReviewState {
    /// Aucun statut (ou statut retiré).
    #[default]
    None,
    /// Accepté.
    Accepted,
    /// Refusé.
    Rejected,
    /// Annulé.
    Cancelled,
    /// Terminé.
    Completed,
}

impl ReviewState {
    /// Tous, dans l'ordre d'Acrobat.
    pub const ALL: [ReviewState; 5] = [
        ReviewState::Accepted,
        ReviewState::Rejected,
        ReviewState::Cancelled,
        ReviewState::Completed,
        ReviewState::None,
    ];

    /// Valeur de `/State` (§12.5.6.3, table 176).
    #[must_use]
    pub fn pdf_name(self) -> &'static str {
        match self {
            ReviewState::None => "None",
            ReviewState::Accepted => "Accepted",
            ReviewState::Rejected => "Rejected",
            ReviewState::Cancelled => "Cancelled",
            ReviewState::Completed => "Completed",
        }
    }

    /// Depuis `/State` ; `None` pour une valeur inconnue.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        [
            ReviewState::None,
            ReviewState::Accepted,
            ReviewState::Rejected,
            ReviewState::Cancelled,
            ReviewState::Completed,
        ]
        .into_iter()
        .find(|s| s.pdf_name() == name)
    }

    /// Libellé français (clé de traduction de l'interface).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ReviewState::None => "Aucun statut",
            ReviewState::Accepted => "Accepté",
            ReviewState::Rejected => "Refusé",
            ReviewState::Cancelled => "Annulé",
            ReviewState::Completed => "Terminé",
        }
    }
}

/// Ce que change une annotation d'état.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateChange {
    /// Statut de relecture.
    Review(ReviewState),
    /// Case « coché » : vrai pour cocher.
    Marked(bool),
}

/// Référence et dictionnaire de l'annotation que l'on vise : elle doit être
/// un objet indirect (c'est ce que désigne `/IRT`), et un commentaire —
/// pas un champ, un lien ni une fenêtre.
fn parent_of(
    doc: &Document,
    page: &Page,
    index: usize,
) -> Result<(ObjectRef, acrux_document::Dict)> {
    let (_, page_dict) = current_page(doc, page)?;
    let annots = annots_of(doc, &page_dict)?;
    let item = annots
        .get(index)
        .ok_or_else(|| Error::Corrupt(format!("annotation {} inexistante", index + 1)))?;
    let Object::Reference(r) = item else {
        return Err(Error::Unsupported(
            "annotation écrite dans la page : on ne peut pas lui répondre".into(),
        ));
    };
    let d = doc
        .get(*r)
        .ok()
        .and_then(|o| o.as_dict().cloned())
        .ok_or_else(|| Error::Corrupt("annotation illisible".into()))?;
    let subtype = d
        .get(&Name::new("Subtype"))
        .and_then(Object::as_name)
        .map(Name::as_str)
        .unwrap_or_default();
    if !is_markup(&subtype) {
        return Err(Error::Corrupt(format!(
            "une annotation /{subtype} n'est pas un commentaire"
        )));
    }
    Ok((*r, d))
}

/// Les annotations de balisage (§12.5.6.2, table 172) : celles qui sont des
/// commentaires, qu'on peut lister, auxquelles on répond.
#[must_use]
pub fn is_markup(subtype: &str) -> bool {
    matches!(
        subtype,
        "Text"
            | "FreeText"
            | "Line"
            | "Square"
            | "Circle"
            | "Polygon"
            | "PolyLine"
            | "Highlight"
            | "Underline"
            | "Squiggly"
            | "StrikeOut"
            | "Caret"
            | "Stamp"
            | "Ink"
            | "FileAttachment"
            | "Sound"
            | "Redact"
    )
}

/// Note sans apparence qui vise `parent` : la base d'une réponse et d'un
/// état.
fn note_about(
    doc: &Document,
    page: &Page,
    parent: (ObjectRef, &acrux_document::Dict),
    meta: &AnnotMeta,
    flags: i64,
) -> Result<acrux_document::Dict> {
    let (page_ref, _) = current_page(doc, page)?;
    let mut d = base_dict(page_ref, meta);
    d.insert(Name::new("Subtype"), Object::Name(Name::new("Text")));
    d.insert(Name::new("F"), Object::Integer(flags));
    d.insert(Name::new("IRT"), Object::Reference(parent.0));
    d.insert(Name::new("Name"), Object::Name(Name::new("Comment")));
    let rect = super::numbers(doc, parent.1, "Rect")
        .filter(|r| r.len() == 4)
        .map_or_else(Rect::default, |r| Rect::new(r[0], r[1], r[2], r[3]));
    d.insert(Name::new("Rect"), rect_object(rect));
    Ok(d)
}

/// Répond au commentaire de rang `parent` : une note `/IRT` qui porte le
/// texte, l'auteur et la date de `meta`. Rend la référence de la réponse.
///
/// # Errors
/// Index invalide, annotation écrite dans la page ou qui n'est pas un
/// commentaire, page non indirecte.
pub fn add_reply(
    doc: &Document,
    page: &Page,
    parent: usize,
    text: &str,
    meta: &AnnotMeta,
) -> Result<ObjectRef> {
    let (parent_ref, parent_dict) = parent_of(doc, page, parent)?;
    // Imprimable, sans zoom ni rotation : comme une note ; elle n'a pas
    // d'apparence, le rendu ne la dessine pas.
    let mut d = note_about(doc, page, (parent_ref, &parent_dict), meta, 4 | 8 | 16)?;
    d.insert(Name::new("RT"), Object::Name(Name::new("R")));
    d.insert(Name::new("Contents"), Object::String(encode_text(text)));
    let color: Option<Rgb> =
        super::numbers(doc, &parent_dict, "C").and_then(|c| super::color_from(&c));
    if let Some(c) = color {
        d.insert(Name::new("C"), color_array(c));
    }
    let reply = doc.add(Object::Dict(d));
    push_to_annots(doc, page, &[reply])?;
    Ok(reply)
}

/// Donne un statut au commentaire de rang `index`, ou coche sa case : une
/// annotation d'état cachée, ajoutée aux précédentes. Son texte suit la
/// convention d'Acrobat (« Accepted set by Zoé »), que les autres lecteurs
/// montrent tel quel.
///
/// # Errors
/// Comme [`add_reply`].
pub fn set_state(
    doc: &Document,
    page: &Page,
    index: usize,
    change: StateChange,
    meta: &AnnotMeta,
) -> Result<ObjectRef> {
    let (parent_ref, parent_dict) = parent_of(doc, page, index)?;
    // Cachée, imprimable, sans zoom ni rotation (§12.5.6.3).
    let mut d = note_about(doc, page, (parent_ref, &parent_dict), meta, 2 | 4 | 8 | 16)?;
    let (state, model) = match change {
        StateChange::Review(s) => (s.pdf_name(), "Review"),
        StateChange::Marked(true) => ("Marked", "Marked"),
        StateChange::Marked(false) => ("Unmarked", "Marked"),
    };
    d.insert(
        Name::new("State"),
        Object::String(state.as_bytes().to_vec()),
    );
    d.insert(
        Name::new("StateModel"),
        Object::String(model.as_bytes().to_vec()),
    );
    let who = meta.author.as_deref().unwrap_or("?");
    d.insert(
        Name::new("Contents"),
        Object::String(encode_text(&format!("{state} set by {who}"))),
    );
    let r = doc.add(Object::Dict(d));
    push_to_annots(doc, page, &[r])?;
    Ok(r)
}

/// Une réponse, dans le fil de son commentaire.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplyEntry {
    /// Page (0 = première).
    pub page: usize,
    /// Rang dans `/Annots`.
    pub index: usize,
    /// Auteur.
    pub author: Option<String>,
    /// Date brute (`D:…`).
    pub modified: Option<String>,
    /// Texte.
    pub contents: String,
    /// Distance au commentaire : 1 pour une réponse directe, 2 pour la
    /// réponse à une réponse…
    pub depth: usize,
}

/// Un commentaire racine et tout son fil.
#[derive(Debug, Clone, PartialEq)]
pub struct CommentEntry {
    /// Page (0 = première).
    pub page: usize,
    /// Rang dans `/Annots`.
    pub index: usize,
    /// Référence de l'annotation, si elle est indirecte.
    pub reference: Option<ObjectRef>,
    /// `/Subtype`.
    pub subtype: String,
    /// `/IT`.
    pub intent: Option<String>,
    /// `/NM`.
    pub name: Option<String>,
    /// Auteur.
    pub author: Option<String>,
    /// Date brute (`D:…`).
    pub modified: Option<String>,
    /// Texte.
    pub contents: String,
    /// Couleur principale.
    pub color: Option<Rgb>,
    /// Rectangle.
    pub rect: Rect,
    /// Membre principal d'un groupe (un remplacement : le signe
    /// d'insertion, que suit le texte barré).
    pub grouped: bool,
    /// Réponses, dans l'ordre du fil (chaque réponse suivie des siennes).
    pub replies: Vec<ReplyEntry>,
    /// Statut le plus récent et son auteur ; `None` sans statut.
    pub review: Option<(ReviewState, Option<String>)>,
    /// Case « coché ».
    pub marked: bool,
}

/// Profondeur maximale d'un fil : au-delà, un fichier aberrant (ou un
/// cycle d'`/IRT`) ne fait pas descendre sans fin.
const MAX_DEPTH: usize = 64;

/// Les fils de discussion du document, dans l'ordre des pages puis de
/// `/Annots`.
#[must_use]
pub fn comment_threads(doc: &Document, pages: &[Page]) -> Vec<CommentEntry> {
    // Toutes les annotations, et d'où elles viennent.
    let mut all: Vec<(usize, AnnotationInfo)> = Vec::new();
    for (page, p) in pages.iter().enumerate() {
        for info in list_annotations(doc, p).unwrap_or_default() {
            all.push((page, info));
        }
    }
    let by_ref: HashMap<ObjectRef, usize> = all
        .iter()
        .enumerate()
        .filter_map(|(i, (_, a))| a.reference.map(|r| (r, i)))
        .collect();
    let primaries: HashSet<ObjectRef> = all
        .iter()
        .filter(|(_, a)| a.is_group_member())
        .filter_map(|(_, a)| a.in_reply_to)
        .collect();
    let listed = |a: &AnnotationInfo| is_markup(&a.subtype) && !a.fill_sign;
    // Une réponse dont le commentaire a disparu devient elle-même un
    // commentaire : elle a encore quelque chose à dire.
    let is_root = |a: &AnnotationInfo| {
        listed(a)
            && !a.is_group_member()
            && !a.is_state()
            && a.in_reply_to.is_none_or(|r| !by_ref.contains_key(&r))
    };
    let mut children: HashMap<ObjectRef, Vec<usize>> = HashMap::new();
    for (i, (_, a)) in all.iter().enumerate() {
        if listed(a) && a.is_reply() {
            if let Some(parent) = a.in_reply_to.filter(|r| by_ref.contains_key(r)) {
                children.entry(parent).or_default().push(i);
            }
        }
    }
    // L'état le plus récent de chaque modèle, par annotation visée : la
    // date brute se compare en texte (même forme, même fuseau), à date
    // égale la dernière écrite l'emporte.
    let mut latest: HashMap<(ObjectRef, bool), (String, usize)> = HashMap::new();
    for (i, (_, a)) in all.iter().enumerate() {
        let (Some(target), true) = (a.in_reply_to, a.is_state()) else {
            continue;
        };
        let marked_model = a.state_model.as_deref() == Some("Marked");
        let key = (target, marked_model);
        let date = a.modified.clone().unwrap_or_default();
        let newer = latest
            .get(&key)
            .is_none_or(|(d, j)| (date.as_str(), i) > (d.as_str(), *j));
        if newer {
            latest.insert(key, (date, i));
        }
    }
    let mut out = Vec::new();
    for (page, a) in &all {
        if !is_root(a) {
            continue;
        }
        let mut replies = Vec::new();
        if let Some(r) = a.reference {
            let mut seen: HashSet<usize> = HashSet::new();
            collect_replies(r, 1, &children, &all, &mut seen, &mut replies);
        }
        let state_of = |marked_model: bool| {
            a.reference
                .and_then(|r| latest.get(&(r, marked_model)))
                .and_then(|(_, i)| all.get(*i))
                .map(|(_, s)| s)
        };
        let review = state_of(false).and_then(|s| {
            let state = ReviewState::from_name(s.state.as_deref().unwrap_or_default())?;
            (state != ReviewState::None).then(|| (state, s.author.clone()))
        });
        let marked = state_of(true).is_some_and(|s| s.state.as_deref() == Some("Marked"));
        out.push(CommentEntry {
            page: *page,
            index: a.index,
            reference: a.reference,
            subtype: a.subtype.clone(),
            intent: a.intent.clone(),
            name: a.name.clone(),
            author: a.author.clone(),
            modified: a.modified.clone(),
            contents: a.contents.clone().unwrap_or_default(),
            color: a.color,
            rect: a.rect,
            grouped: a.reference.is_some_and(|r| primaries.contains(&r)),
            replies,
            review,
            marked,
        });
    }
    out
}

/// Ajoute à `out` les réponses de `parent`, chacune suivie des siennes.
fn collect_replies(
    parent: ObjectRef,
    depth: usize,
    children: &HashMap<ObjectRef, Vec<usize>>,
    all: &[(usize, AnnotationInfo)],
    seen: &mut HashSet<usize>,
    out: &mut Vec<ReplyEntry>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    let Some(list) = children.get(&parent) else {
        return;
    };
    for &i in list {
        if !seen.insert(i) {
            continue;
        }
        let Some((page, a)) = all.get(i) else {
            continue;
        };
        out.push(ReplyEntry {
            page: *page,
            index: a.index,
            author: a.author.clone(),
            modified: a.modified.clone(),
            contents: a.contents.clone().unwrap_or_default(),
            depth,
        });
        if let Some(r) = a.reference {
            collect_replies(r, depth + 1, children, all, seen, out);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // générateur de corpus : une erreur est l'échec cherché
mod tests {
    use super::*;
    use crate::annotations::freetext;
    use crate::annotations::{add_annotation_with, MarkupKind, NewAnnotation, ShapeStyle};
    use crate::stamp::StandardFont;
    use acrux_document::{collect_pages, Dict};

    fn meta(author: &str, date: &str, id: &str) -> AnnotMeta {
        AnnotMeta {
            author: Some(author.into()),
            name: Some(format!("acrux-corpus-{id}")),
            date: Some(date.into()),
        }
    }

    /// Une page de relecture : du texte, trois notes de deux auteurs avec
    /// des dates différentes, deux réponses dont une imbriquée, un statut
    /// « Accepté », une case cochée, un rectangle rouge avec sa fenêtre
    /// contextuelle et un surlignage sur une ligne. Toutes les dates et tous
    /// les identifiants sont figés : c'est le document sur lequel s'exercent
    /// la bulle, le panneau des commentaires et `acr annots`.
    #[test]
    #[ignore = "génère le fichier de corpus"]
    #[allow(clippy::too_many_lines)] // le document, annotation par annotation
    fn generate_comment_threads_corpus() {
        let lines = [
            (16.0, "F1", 262.0, r"Relecture du chapitre 2"),
            (
                11.0,
                "F2",
                228.0,
                r"Le projet avance selon le calendrier pr\351vu pour ce trimestre.",
            ),
            (
                11.0,
                "F2",
                210.0,
                r"Les essais de la semaine derni\350re ont confirm\351 les r\351sultats.",
            ),
            (
                11.0,
                "F2",
                192.0,
                r"Il reste \340 valider le budget et \340 relire la conclusion.",
            ),
            (
                11.0,
                "F2",
                174.0,
                r"Merci de noter vos remarques directement dans ce document.",
            ),
        ];
        let mut content = String::from("0.12 0.14 0.20 rg\n");
        for (size, font, y, text) in lines {
            let _ = std::fmt::Write::write_fmt(
                &mut content,
                format_args!("BT /{font} {size} Tf 40 {y} Td ({text}) Tj ET\n"),
            );
        }
        content.push_str("0.55 0.57 0.62 RG 1 w 40 252 m 380 252 l S\n");
        let objects = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [5 0 R] /Count 1 >>".to_string()),
            (
                3,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
                    .to_string(),
            ),
            (
                4,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                    .to_string(),
            ),
            (
                5,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 420 300] /Contents 6 0 R \
                 /Resources << /Font << /F1 3 0 R /F2 4 0 R >> >> >>"
                    .to_string(),
            ),
            (
                6,
                format!(
                    "<< /Length {} >>\nstream\n{content}\nendstream",
                    content.len()
                ),
            ),
        ];
        let doc = Document::from_bytes(crate::docinfo::tests::build_pdf(&objects, "")).unwrap();
        let page = || collect_pages(&doc).unwrap().remove(0);
        // Le surlignage couvre « Les essais de la semaine dernière ».
        let passage = "Les essais de la semaine derni\u{e8}re";
        let width = freetext::layout(passage, StandardFont::Helvetica, 11.0, 1000.0)[0].width;
        let annotations = [
            (
                NewAnnotation::Note {
                    x: 390.0,
                    y: 238.0,
                    contents: "Préciser la date exacte de fin.".into(),
                    color: [1.0, 0.85, 0.0],
                },
                meta("Alice", "D:20240110093000Z", "note-date"),
            ),
            (
                NewAnnotation::Note {
                    x: 390.0,
                    y: 202.0,
                    contents: "Chiffres à vérifier avec la comptabilité.".into(),
                    color: [0.35, 0.75, 1.0],
                },
                meta("Bruno", "D:20240109141500Z", "note-chiffres"),
            ),
            (
                NewAnnotation::Note {
                    x: 390.0,
                    y: 150.0,
                    contents: "Conclusion trop courte ?".into(),
                    color: [1.0, 0.85, 0.0],
                },
                meta("Alice", "D:20240115170000Z", "note-conclusion"),
            ),
            (
                NewAnnotation::Square {
                    rect: Rect::new(34.0, 168.0, 352.0, 186.0),
                    style: ShapeStyle::default(),
                    contents: Some("Formulation à revoir.".into()),
                },
                meta("Bruno", "D:20240108110000Z", "rectangle"),
            ),
            (
                NewAnnotation::Markup {
                    kind: MarkupKind::Highlight,
                    quads: vec![Rect::new(39.0, 207.0, 41.0 + width, 220.0)],
                    color: MarkupKind::Highlight.default_color(),
                    contents: Some("Bon résultat.".into()),
                },
                meta("Alice", "D:20240112080000Z", "surlignage"),
            ),
        ];
        for (a, m) in &annotations {
            add_annotation_with(&doc, &page(), a, m).unwrap();
        }
        // Les réponses et les états de la première note, puis la case de
        // la deuxième.
        add_reply(
            &doc,
            &page(),
            0,
            "Fin mars, je l\u{2019}ajoute.",
            &meta("Bruno", "D:20240111101500Z", "reponse-1"),
        )
        .unwrap();
        add_reply(
            &doc,
            &page(),
            5,
            "Parfait, merci !",
            &meta("Alice", "D:20240112090000Z", "reponse-2"),
        )
        .unwrap();
        set_state(
            &doc,
            &page(),
            0,
            StateChange::Review(ReviewState::Accepted),
            &meta("Bruno", "D:20240113100000Z", "etat-accepte"),
        )
        .unwrap();
        set_state(
            &doc,
            &page(),
            1,
            StateChange::Marked(true),
            &meta("Alice", "D:20240114100000Z", "etat-coche"),
        )
        .unwrap();
        // La fenêtre contextuelle du rectangle, fermée (§12.5.6.14).
        let p = page();
        let list = crate::annotations::list_annotations(&doc, &p).unwrap();
        let square = list[3].reference.unwrap();
        let mut popup = Dict::new();
        popup.insert(Name::new("Type"), Object::Name(Name::new("Annot")));
        popup.insert(Name::new("Subtype"), Object::Name(Name::new("Popup")));
        popup.insert(Name::new("Parent"), Object::Reference(square));
        popup.insert(
            Name::new("Rect"),
            rect_object(Rect::new(250.0, 60.0, 410.0, 150.0)),
        );
        popup.insert(Name::new("Open"), Object::Bool(false));
        let popup = doc.add(Object::Dict(popup));
        let mut sq = doc.get(square).unwrap().as_dict().unwrap().clone();
        sq.insert(Name::new("Popup"), Object::Reference(popup));
        doc.set(square, Object::Dict(sq));
        push_to_annots(&doc, &p, &[popup]).unwrap();
        let threads = comment_threads(&doc, &collect_pages(&doc).unwrap());
        assert_eq!(threads.len(), 5);
        assert_eq!(threads[0].replies.len(), 2);
        assert_eq!(
            threads[0].review.as_ref().map(|r| r.0),
            Some(ReviewState::Accepted)
        );
        assert!(threads[1].marked);
        let saved = doc.save_full().unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/corpus/synthese/commentaires-fils-etats.pdf");
        std::fs::write(&path, saved).unwrap();
        println!("écrit : {}", path.display());
    }
}
