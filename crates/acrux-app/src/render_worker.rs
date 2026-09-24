//! Rendu des pages sur un fil d'exécution séparé : l'interface reste fluide
//! pendant qu'une page lourde se dessine. Le fil possède sa propre copie du
//! document (le modèle objet n'est pas partageable entre fils) ; les demandes
//! et les résultats transitent par des canaux, et le fil réveille la fenêtre
//! via [`Waker`] quand un résultat est prêt.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use acrux_document::{collect_pages, Document};
use acrux_features::annotations::review::StateChange;
use acrux_features::annotations::{add_annotation_with, AnnotChanges, AnnotMeta, NewAnnotation};
use acrux_features::edit_text::{apply_edits, TextEdit, TextRange};
use acrux_features::export::{
    export_docx, export_html, export_markdown, export_pages_jpeg, export_pages_png,
    export_tables_xlsx, export_text, HtmlOptions,
};
use acrux_features::forms::{set_field_value, FieldValue};
use acrux_features::pages::split::{part_file_name, plan_parts, write_parts, SplitPlan};
use acrux_features::redact::{apply_redactions, mark_redactions, RedactionMark};
use acrux_features::text::SearchOptions;
use acrux_graphics::{Bitmap, Color};
use acrux_render::{render_page_rotated, RenderOptions};

use crate::platform::Waker;

/// Demande de rendu.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderRequest {
    /// Index de page.
    pub page: usize,
    /// Échelle en pixels par point.
    pub scale: f64,
    /// Clé d'échelle (millièmes) pour associer le résultat.
    pub scale_key: u32,
    /// Génération du document (incrémentée à chaque modification).
    pub generation: u32,
}

/// Modification du document, appliquée à l'identique par le visualiseur et
/// par le fil de rendu (chacun possède sa copie du document).
#[derive(Debug, Clone)]
// Une opération de texte porte deux boîtes de paragraphe, plus grosses que
// les autres variantes : les mettre derrière un pointeur ferait une
// indirection à chaque frappe pour économiser quelques octets par message.
#[allow(clippy::large_enum_variant)]
pub enum EditOp {
    /// Pivoter des pages (multiple de 90°).
    Rotate {
        /// Indices de pages.
        pages: Vec<usize>,
        /// Angle horaire.
        degrees: i32,
    },
    /// Supprimer des pages.
    Delete {
        /// Indices de pages.
        pages: Vec<usize>,
    },
    /// Poser des annotations : un geste de l'utilisateur, une opération —
    /// et donc une seule annulation, même quand le passage balisé court sur
    /// plusieurs pages.
    Annotate {
        /// Annotations, dans l'ordre où elles sont posées.
        items: Vec<AnnotItem>,
    },
    /// Remplacer un fragment de texte d'une ligne par un autre.
    EditText {
        /// Indice de page.
        page: usize,
        /// Ligne dans le texte extrait de la page.
        line: usize,
        /// Premier glyphe de la ligne à remplacer.
        start: usize,
        /// Glyphe suivant le dernier remplacé.
        end: usize,
        /// Texte de remplacement.
        text: String,
    },
    /// Remplacer un texte partout dans le document.
    ReplaceAll {
        /// Texte cherché.
        find: String,
        /// Texte de remplacement.
        with: String,
        /// Casse et mot entier, comme la recherche qui a trouvé les
        /// occurrences : on remplace ce qui était surligné, rien d'autre.
        options: SearchOptions,
    },
    /// Modifier un objet de la page : déplacer, redimensionner, supprimer,
    /// réordonner.
    EditObject {
        /// Indice de page.
        page: usize,
        /// Modifications, telles que `acrux-features` les comprend.
        edits: Vec<acrux_features::edit_objects::Edit>,
    },
    /// Poser un élément « remplir et signer » (signature, marque, texte).
    FillSign {
        /// Ce qui est posé, et où.
        options: Box<acrux_features::fillsign::Options>,
    },
    /// Marquer des zones pour biffure (annotations `/Redact`, réversible).
    Mark {
        /// Zones à biffer.
        marks: Vec<RedactionMark>,
    },
    /// Appliquer toutes les marques de biffure (destructif).
    ApplyRedactions,
    /// Insérer les pages d'un autre fichier.
    Insert {
        /// Fichier source.
        path: PathBuf,
        /// Mot de passe du fichier source, s'il est chiffré.
        password: Option<Vec<u8>>,
        /// Pages du fichier source (vide = toutes).
        pages: Vec<usize>,
        /// Position d'insertion (0 = avant la première page).
        at: usize,
    },
    /// Insérer une page vierge.
    InsertBlank {
        /// Position (0 = avant la première page).
        at: usize,
        /// Format de la page : celui de la page voisine.
        media: acrux_core::Rect,
        /// Rotation de la page : celle de la page voisine.
        rotate: i32,
    },
    /// Remplacer le contenu de pages par celui des pages d'un autre fichier
    /// (voir `pages::replace_pages`). Comme `Insert`, le fichier source est
    /// relu à chaque rejeu.
    Replace {
        /// Fichier source.
        path: PathBuf,
        /// Mot de passe du fichier source, s'il est chiffré.
        password: Option<Vec<u8>>,
        /// Paires (page remplacée, page source).
        pairs: Vec<(usize, usize)>,
    },
    /// Réordonner les pages (ordre complet des indices d'origine).
    Reorder {
        /// Nouvel ordre : `order[i]` est l'indice d'origine de la page `i`.
        order: Vec<usize>,
    },
    /// Joindre un fichier au document (arbre `/Names /EmbeddedFiles`).
    Attach {
        /// Nom de la pièce jointe dans le document.
        name: String,
        /// Contenu du fichier.
        data: Vec<u8>,
        /// `/Desc`.
        description: Option<String>,
    },
    /// Retirer une pièce jointe par son nom.
    Detach {
        /// Nom de la pièce jointe.
        name: String,
    },
    /// Donner un nouveau texte à un paragraphe, ou créer une zone de texte.
    ///
    /// C'est l'opération du mode « Modifier le PDF ». Elle est rejouable :
    /// appliquée au fichier d'origine, elle retrouve le paragraphe par sa
    /// boîte et par le texte qu'il portait (`expected`), et donne le même
    /// résultat que toute la session de frappe. L'historique n'en garde donc
    /// qu'une par paragraphe modifié, et l'annulation défait la session
    /// entière d'un coup, comme dans Acrobat.
    Paragraph {
        /// Indice de page.
        page: usize,
        /// Boîte du paragraphe, relevée à l'ouverture.
        frame: acrux_features::edit_text::ParagraphFrame,
        /// Boîte d'arrivée, quand le bloc est déplacé ou redimensionné.
        to: Option<acrux_features::edit_text::ParagraphFrame>,
        /// Texte dessiné avant, sans blancs.
        expected: String,
        /// Texte à écrire.
        text: String,
    },
    /// Déplacer ou redimensionner un élément de « remplir et signer ».
    PlacedRect {
        /// Indice de page.
        page: usize,
        /// Rang de l'annotation dans la page.
        index: usize,
        /// Nouveau rectangle, en coordonnées de page.
        rect: acrux_core::Rect,
    },
    /// Donner une valeur à un champ de formulaire (apparences régénérées).
    SetField {
        /// Nom qualifié du champ.
        name: String,
        /// Valeur.
        value: FieldValue,
    },
    /// Effacer le formulaire : chaque champ remplissable reprend sa valeur
    /// par défaut, ou redevient vide (`forms::reset_fields`).
    ResetForm,
    /// Aplatir le formulaire : les champs deviennent du contenu fixe des
    /// pages et le formulaire disparaît (`forms::flatten_fields`).
    FlattenForm,
    /// Modifier une annotation existante : la déplacer, la redimensionner,
    /// changer ses couleurs, son opacité, son trait ou son texte.
    AnnotSet {
        /// Indice de page.
        page: usize,
        /// Rang de l'annotation dans `/Annots`.
        index: usize,
        /// Ce qui change, date comprise (tirée quand on décide).
        changes: AnnotChanges,
    },
    /// Supprimer une annotation, avec ses réponses, ses états et sa
    /// fenêtre.
    AnnotRemove {
        /// Indice de page.
        page: usize,
        /// Rang de l'annotation dans `/Annots`.
        index: usize,
    },
    /// Répondre à un commentaire.
    AnnotReply {
        /// Indice de page.
        page: usize,
        /// Rang du commentaire dans `/Annots`.
        index: usize,
        /// Texte de la réponse.
        text: String,
        /// Auteur, identifiant et date de la réponse, tirés une fois.
        meta: AnnotMeta,
    },
    /// Poser un tampon (Approuvé, Confidentiel…, dynamique ou image).
    ///
    /// Tout ce qui dépend du moment — la ligne dynamique, l'identité de
    /// l'annotation — est déjà dans les options : la pose se rejoue à
    /// l'identique.
    Stamp {
        /// Le tampon, sa page, sa place.
        options: Box<acrux_features::rubber_stamp::Options>,
    },
    /// Poser une image sur la page, comme objet de son contenu
    /// (« Ajouter une image »).
    AddImage {
        /// Indice de page.
        page: usize,
        /// L'image, décodée une fois : l'opération est appliquée deux fois
        /// puis rejouée à chaque annulation, sans redécoder, et ses copies
        /// partagent les mêmes octets.
        image: std::sync::Arc<acrux_features::stamp::PreparedImage>,
        /// Centre, en coordonnées de page.
        center: acrux_core::Point,
        /// Largeur et hauteur telles qu'on les voit, en points.
        size: (f64, f64),
    },
    /// Donner un statut à un commentaire, ou cocher sa case.
    AnnotState {
        /// Indice de page.
        page: usize,
        /// Rang du commentaire dans `/Annots`.
        index: usize,
        /// Statut ou case.
        change: StateChange,
        /// Auteur, identifiant et date de l'état, tirés une fois.
        meta: AnnotMeta,
    },
}

/// Une annotation à poser, avec son identité.
///
/// L'identité (`/NM`, dates) est tirée quand le visualiseur **décide** de
/// poser l'annotation, puis gardée ici : l'opération est appliquée deux fois
/// (au document affiché et à la copie du fil de rendu), et rejouée à chaque
/// annulation. Tirée à l'application, elle différerait d'une copie à l'autre
/// et changerait à chaque Ctrl+Z.
#[derive(Debug, Clone)]
pub struct AnnotItem {
    /// Indice de page.
    pub page: usize,
    /// Annotation.
    pub annotation: NewAnnotation,
    /// Auteur, identifiant, date.
    pub meta: AnnotMeta,
}

/// Droit qu'exige une modification d'un document protégé (§7.6.4.2,
/// table 22). Le propriétaire les a tous ; l'utilisateur, ceux que les
/// permissions lui laissent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Right {
    /// Modifier le contenu (bit 4) : texte, objets, biffures, pièces jointes.
    Modify,
    /// Commenter (bit 6) : annotations, marques, remplir et signer.
    Annotate,
    /// Remplir les champs de formulaire (bit 9, ou 6).
    FillForms,
    /// Assembler (bit 11) : pivoter, supprimer, insérer, réordonner des pages.
    Assemble,
}

impl Right {
    /// Vrai si ces permissions accordent ce droit. Commenter comprend le
    /// remplissage des formulaires (bit 6 de la table 22).
    #[must_use]
    pub fn allowed_by(self, p: acrux_document::protect::Permissions) -> bool {
        match self {
            Right::Modify => p.modify,
            Right::Annotate => p.annotate,
            Right::FillForms => p.fill_forms || p.annotate,
            Right::Assemble => p.assemble,
        }
    }
}

impl EditOp {
    /// Une seule annotation, avec une identité neuve.
    #[must_use]
    pub fn annotate(page: usize, annotation: NewAnnotation, author: Option<&str>) -> EditOp {
        EditOp::Annotate {
            items: vec![AnnotItem {
                page,
                annotation,
                meta: AnnotMeta::fresh(author),
            }],
        }
    }

    /// Droit que la modification exige d'un document protégé. C'est le
    /// point unique où le visualiseur décide si un utilisateur qui n'a que
    /// le mot de passe d'ouverture peut la faire.
    #[must_use]
    pub fn required_right(&self) -> Right {
        match self {
            EditOp::Rotate { .. }
            | EditOp::Delete { .. }
            | EditOp::Insert { .. }
            | EditOp::InsertBlank { .. }
            | EditOp::Replace { .. }
            | EditOp::Reorder { .. } => Right::Assemble,
            EditOp::Annotate { .. }
            | EditOp::Mark { .. }
            | EditOp::FillSign { .. }
            | EditOp::PlacedRect { .. }
            | EditOp::AnnotSet { .. }
            | EditOp::AnnotRemove { .. }
            | EditOp::AnnotReply { .. }
            | EditOp::AnnotState { .. }
            | EditOp::Stamp { .. } => Right::Annotate,
            EditOp::SetField { .. } | EditOp::ResetForm => Right::FillForms,
            EditOp::EditText { .. }
            | EditOp::ReplaceAll { .. }
            | EditOp::EditObject { .. }
            | EditOp::ApplyRedactions
            | EditOp::Attach { .. }
            | EditOp::Detach { .. }
            | EditOp::Paragraph { .. }
            | EditOp::FlattenForm
            | EditOp::AddImage { .. } => Right::Modify,
        }
    }

    /// Applique la modification.
    ///
    /// # Errors
    /// Page absente ou document illisible.
    #[allow(clippy::too_many_lines)] // une branche par sorte de modification
    pub fn apply(&self, doc: &Document) -> acrux_core::Result<()> {
        match self {
            EditOp::Rotate { pages, degrees } => {
                acrux_features::pages::rotate_pages(doc, pages, *degrees)
            }
            EditOp::Delete { pages } => acrux_features::pages::delete_pages(doc, pages),
            EditOp::Annotate { items } => {
                // Les pages une seule fois : `add_annotation_with` relit la
                // page avant chaque ajout, deux annotations sur la même
                // page ne se perdent donc pas l'une l'autre.
                let pages = collect_pages(doc)?;
                for item in items {
                    let p = pages
                        .get(item.page)
                        .ok_or_else(|| acrux_core::Error::Corrupt("page absente".into()))?;
                    add_annotation_with(doc, p, &item.annotation, &item.meta)?;
                }
                Ok(())
            }
            EditOp::EditText {
                page,
                line,
                start,
                end,
                text,
            } => apply_edits(
                doc,
                &[TextEdit {
                    page: *page,
                    target: TextRange {
                        line: *line,
                        start: *start,
                        end: *end,
                    },
                    new_text: text.clone(),
                    style: None,
                }],
            ),
            EditOp::ReplaceAll {
                find,
                with,
                options,
            } => replace_all_with(doc, find, with, *options).map(|_| ()),
            EditOp::EditObject { page, edits } => {
                let pages = acrux_document::collect_pages(doc)?;
                let target = pages.get(*page).ok_or_else(|| {
                    acrux_core::Error::Corrupt(format!("page {} inexistante", page + 1))
                })?;
                acrux_features::edit_objects::apply(doc, target, edits).map(|_| ())
            }
            EditOp::FillSign { options } => {
                acrux_features::fillsign::place(doc, options).map(|_| ())
            }
            EditOp::Stamp { options } => {
                acrux_features::rubber_stamp::place(doc, options).map(|_| ())
            }
            EditOp::AddImage {
                page,
                image,
                center,
                size,
            } => acrux_features::edit_objects::add_image(
                doc,
                &page_of(doc, *page)?,
                image,
                *center,
                *size,
            )
            .map(|_| ()),
            EditOp::Mark { marks } => mark_redactions(doc, marks).map(|_| ()),
            EditOp::ApplyRedactions => apply_redactions(doc).map(|_| ()),
            EditOp::Insert {
                path,
                password,
                pages,
                at,
            } => {
                let src = open_source(path, password.as_deref())?;
                let indices: Vec<usize> = if pages.is_empty() {
                    (0..collect_pages(&src)?.len()).collect()
                } else {
                    pages.clone()
                };
                acrux_features::pages::insert_pages_from(doc, &src, &indices, *at)
            }
            EditOp::InsertBlank { at, media, rotate } => {
                acrux_features::pages::insert_blank_page(doc, *at, *media, *rotate)
            }
            EditOp::Replace {
                path,
                password,
                pairs,
            } => {
                let src = open_source(path, password.as_deref())?;
                acrux_features::pages::replace_pages(doc, &src, pairs)
            }
            EditOp::Reorder { order } => acrux_features::pages::reorder_pages(doc, order),
            EditOp::Attach {
                name,
                data,
                description,
            } => acrux_features::attach::add_attachment(
                doc,
                name,
                data,
                &acrux_features::attach::AttachOptions {
                    description: description.clone(),
                    ..acrux_features::attach::AttachOptions::default()
                },
            )
            .map(|_| ()),
            EditOp::Detach { name } => acrux_features::attach::remove_attachment(doc, name),
            EditOp::SetField { name, value } => set_field_value(doc, name, value.clone()),
            EditOp::ResetForm => acrux_features::forms::reset_fields(doc).map(|_| ()),
            EditOp::FlattenForm => acrux_features::forms::flatten_fields(doc).map(|_| ()),
            EditOp::PlacedRect { page, index, rect } => {
                acrux_features::fillsign::set_rect(doc, *page, *index, *rect)
            }
            EditOp::AnnotSet {
                page,
                index,
                changes,
            } => acrux_features::annotations::set_annotation_properties(
                doc,
                &page_of(doc, *page)?,
                *index,
                changes,
            ),
            EditOp::AnnotRemove { page, index } => {
                acrux_features::annotations::remove_annotation(doc, &page_of(doc, *page)?, *index)
            }
            EditOp::AnnotReply {
                page,
                index,
                text,
                meta,
            } => acrux_features::annotations::review::add_reply(
                doc,
                &page_of(doc, *page)?,
                *index,
                text,
                meta,
            )
            .map(|_| ()),
            EditOp::AnnotState {
                page,
                index,
                change,
                meta,
            } => acrux_features::annotations::review::set_state(
                doc,
                &page_of(doc, *page)?,
                *index,
                *change,
                meta,
            )
            .map(|_| ()),
            EditOp::Paragraph {
                page,
                frame,
                to,
                expected,
                text,
            } => {
                let pages = collect_pages(doc)?;
                let page = pages.get(*page).ok_or_else(|| {
                    acrux_core::Error::Corrupt(format!("page {} absente", page + 1))
                })?;
                let to = to.as_ref().unwrap_or(frame);
                acrux_features::edit_text::move_paragraph(doc, page, frame, to, expected, text)
                    .map(|_| ())
            }
        }
    }
}

/// Ouvre le fichier dont on prend des pages (insérer, remplacer).
///
/// En prendre les pages, c'est en extraire le contenu : un document à
/// ouverture libre qui interdit la copie ne se recopie pas ainsi dans un
/// fichier sans permissions. Son propriétaire, lui, le peut.
fn open_source(path: &std::path::Path, password: Option<&[u8]>) -> acrux_core::Result<Document> {
    let src = Document::load(path)?;
    if let Some(pw) = password {
        src.authenticate(pw)?;
    }
    let restricted = src.security().is_some_and(|h| {
        !h.is_owner() && !acrux_document::protect::Permissions::from_p(h.permissions()).copy
    });
    if restricted {
        return Err(acrux_core::Error::Unsupported(
            "les permissions de ce document interdisent d'en extraire les pages".into(),
        ));
    }
    Ok(src)
}

/// Page `index` du document, telle qu'il est maintenant.
fn page_of(doc: &Document, index: usize) -> acrux_core::Result<acrux_document::Page> {
    collect_pages(doc)?
        .into_iter()
        .nth(index)
        .ok_or_else(|| acrux_core::Error::Corrupt(format!("page {} absente", index + 1)))
}

/// Format d'export, déduit de l'extension choisie par l'utilisateur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// Page web fidèle (texte positionné, images et polices incorporées).
    Html,
    /// Document Word.
    Docx,
    /// Classeur Excel (un onglet par page, tableaux détectés).
    Xlsx,
    /// Markdown.
    Markdown,
    /// Texte brut.
    Text,
    /// Une image PNG par page.
    Png,
    /// Une image JPEG par page.
    Jpeg,
}

impl ExportFormat {
    /// Format correspondant à une extension de fichier (sans le point).
    #[must_use]
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "html" | "htm" => Some(Self::Html),
            "docx" => Some(Self::Docx),
            "xlsx" => Some(Self::Xlsx),
            "md" | "markdown" => Some(Self::Markdown),
            "txt" | "text" => Some(Self::Text),
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            _ => None,
        }
    }

    /// Extensions proposées dans le dialogue d'enregistrement.
    #[must_use]
    pub fn all() -> &'static [(&'static str, &'static str)] {
        &[
            ("Page web", "html"),
            ("Document Word", "docx"),
            ("Classeur Excel", "xlsx"),
            ("Markdown", "md"),
            ("Texte", "txt"),
            ("Images PNG", "png"),
            ("Images JPEG", "jpg"),
        ]
    }
}

/// Écrit l'export demandé et décrit ce qui a été produit.
fn run_export(
    doc: &Document,
    path: &std::path::Path,
    format: ExportFormat,
) -> acrux_core::Result<String> {
    let write = |p: &std::path::Path, data: &[u8]| -> acrux_core::Result<()> {
        std::fs::write(p, data).map_err(|e| acrux_core::Error::Corrupt(format!("écriture : {e}")))
    };
    match format {
        ExportFormat::Png | ExportFormat::Jpeg => {
            let stem = path.with_extension("");
            let ext = path
                .extension()
                .map_or_else(|| "png".to_string(), |e| e.to_string_lossy().into_owned());
            let mut count = 0usize;
            let mut sink = |index: usize, data: &[u8]| -> acrux_core::Result<()> {
                count += 1;
                let target = format!("{}-{}.{ext}", stem.display(), index + 1);
                write(std::path::Path::new(&target), data)
            };
            if format == ExportFormat::Png {
                export_pages_png(doc, &[], 150.0, &mut sink)?;
            } else {
                export_pages_jpeg(doc, &[], 150.0, 85, true, &mut sink)?;
            }
            Ok(format!(
                "{count} image(s) écrite(s) à côté de {}",
                stem.display()
            ))
        }
        ExportFormat::Html => {
            let title = path.file_stem().map_or_else(
                || "Document".to_string(),
                |s| s.to_string_lossy().into_owned(),
            );
            let options = HtmlOptions {
                title,
                ..HtmlOptions::default()
            };
            write(path, export_html(doc, &options)?.as_bytes())?;
            Ok(format!("exporté : {}", path.display()))
        }
        ExportFormat::Docx => {
            write(path, &export_docx(doc)?)?;
            Ok(format!("exporté : {}", path.display()))
        }
        ExportFormat::Xlsx => {
            write(path, &export_tables_xlsx(doc)?)?;
            Ok(format!("exporté : {}", path.display()))
        }
        ExportFormat::Markdown => {
            write(path, export_markdown(doc)?.as_bytes())?;
            Ok(format!("exporté : {}", path.display()))
        }
        ExportFormat::Text => {
            write(path, export_text(doc)?.as_bytes())?;
            Ok(format!("exporté : {}", path.display()))
        }
    }
}

/// Chemin libre pour `name` dans `dir` : le nom tel quel s'il n'est pas
/// pris, sinon « nom (2).pdf », « nom (3).pdf »… Un fractionnement
/// n'écrase **jamais** un fichier en silence.
fn free_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, "pdf"));
    (2..10_000)
        .map(|n| dir.join(format!("{stem} ({n}).{ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// Fractionne le document et rend le compte rendu à afficher : combien de
/// fichiers, où, et ce qui mérite d'être dit (une page plus grosse que la
/// taille maximale, des fichiers écrits en clair).
pub(crate) fn run_split(
    doc: &Document,
    dir: &std::path::Path,
    stem: &str,
    plan: &SplitPlan,
) -> acrux_core::Result<String> {
    use crate::ui::lang::{tr, trf};
    let options = acrux_document::SaveOptions::default();
    let planned = plan_parts(doc, plan, &options)?;
    let total = planned.parts.len();
    std::fs::create_dir_all(dir)
        .map_err(|e| acrux_core::Error::Io(format!("{}: {e}", dir.display())))?;
    let mut written = 0usize;
    write_parts(doc, &planned.parts, &options, &mut |index, part, bytes| {
        let name = part_file_name(stem, index, total, part.title.as_deref());
        let target = free_path(dir, &name);
        std::fs::write(&target, bytes)
            .map_err(|e| acrux_core::Error::Io(format!("{}: {e}", target.display())))?;
        written += 1;
        Ok(())
    })?;
    let mut notice = trf(
        "{} fichier(s) écrit(s) dans {}",
        &[&written.to_string(), &dir.display().to_string()],
    );
    if !planned.warnings.is_empty() {
        notice.push_str(&trf(
            " — {} partie(s) dépassent la taille maximale (une page seule plus grosse)",
            &[&planned.warnings.len().to_string()],
        ));
    }
    if doc.is_encrypted() {
        notice.push_str(tr(" — fichiers non protégés"));
    }
    Ok(notice)
}

/// Fait les travaux longs demandés au fil — conversions, fractionnements —
/// et envoie un avis par travail, suivi d'un réveil de la fenêtre. Ce
/// réveil part de ce fil, jamais du traitement d'un réveil : il n'y en a
/// qu'un par travail. Rend faux quand la fenêtre n'écoute plus.
fn run_jobs(
    doc: &Document,
    exports: Vec<(PathBuf, ExportFormat)>,
    splits: Vec<(PathBuf, String, SplitPlan)>,
    note_tx: &Sender<String>,
    waker: &dyn Waker,
) -> bool {
    let exported = exports.into_iter().map(|(path, format)| {
        run_export(doc, &path, format).unwrap_or_else(|e| format!("export impossible : {e}"))
    });
    let split = splits.into_iter().map(|(dir, stem, plan)| {
        run_split(doc, &dir, &stem, &plan).unwrap_or_else(|e| {
            crate::ui::lang::trf("fractionnement impossible : {}", &[&e.to_string()])
        })
    });
    for notice in exported.chain(split) {
        if note_tx.send(notice).is_err() {
            return false;
        }
        waker.wake();
    }
    true
}

/// Message envoyé au fil de rendu.
enum WorkerMessage {
    /// Rendre une page.
    Render(RenderRequest),
    /// Modifier le document (les rendus demandés avant sont obsolètes).
    ///
    /// L'opération est encadrée : une boîte de paragraphe pèse plus de trois
    /// cents octets, et ce message-ci circule à chaque frappe.
    Edit(Box<EditOp>),
    /// Échelles encore utiles : les demandes des autres échelles en attente
    /// sont abandonnées (zoom changé, panneau fermé…).
    Keep(Vec<u32>),
    /// Imposer la visibilité de calques (contenu optionnel).
    SetLayers(std::collections::HashMap<u32, bool>),
    /// Tourner la **vue** de ce nombre de degrés (multiple de 90) : les
    /// pages se rendent tournées, le document ne change pas.
    SetViewRotation(i32),
    /// Convertir le document dans un autre format (peut être long : c'est
    /// justement pour cela qu'il se fait ici).
    Export {
        /// Fichier à écrire.
        path: PathBuf,
        /// Format demandé.
        format: ExportFormat,
    },
    /// Fractionner le document en plusieurs fichiers : le document du fil,
    /// qui a rejoué les modifications, est fractionné tel qu'il s'affiche.
    Split {
        /// Dossier de sortie.
        dir: PathBuf,
        /// Début du nom des fichiers (`rapport` → `rapport-01.pdf`…).
        stem: String,
        /// Découpage.
        plan: SplitPlan,
    },
}

/// Résultat de rendu.
pub struct RenderResult {
    /// Index de page.
    pub page: usize,
    /// Clé d'échelle.
    pub scale_key: u32,
    /// Image rendue.
    pub bitmap: Bitmap,
    /// Durée du rendu en millisecondes.
    pub ms: f64,
    /// Avertissements du moteur.
    pub warnings: Vec<String>,
    /// Génération du document au moment du rendu.
    pub generation: u32,
}

/// Fil de rendu d'un document.
pub struct RenderWorker {
    requests: Sender<WorkerMessage>,
    results: Receiver<RenderResult>,
    notices: Receiver<String>,
    handle: Option<JoinHandle<()>>,
    /// Demandes envoyées et pas encore reçues (page, clé d'échelle).
    pending: Vec<(usize, u32)>,
    /// Génération courante du document.
    generation: u32,
}

impl RenderWorker {
    /// Démarre le fil sur le fichier donné. Retourne `None` si le document ne
    /// peut pas être rouvert (le visualiseur rend alors sur le fil principal).
    #[must_use]
    pub fn start(path: PathBuf, password: Option<Vec<u8>>, waker: Box<dyn Waker>) -> Option<Self> {
        let (req_tx, req_rx) = mpsc::channel::<WorkerMessage>();
        let (res_tx, res_rx) = mpsc::channel::<RenderResult>();
        let (note_tx, note_rx) = mpsc::channel::<String>();
        // Vérification préalable sur le fil appelant pour signaler l'échec tout de suite.
        Document::load(&path).ok()?;
        let handle = std::thread::Builder::new()
            .name("rendu".into())
            .spawn(move || {
                let Ok(doc) = Document::load(&path) else {
                    return;
                };
                if let Some(pw) = password {
                    let _ = doc.authenticate(&pw);
                }
                // Les apparences manquantes d'un formulaire, comme le fait le
                // visualiseur au même moment de la vie du document (après
                // l'authentification, avant toute modification) : les deux
                // copies restent identiques, et l'historique rejoué s'applique
                // aux mêmes objets.
                let _ = acrux_features::forms::prepare_display(&doc);
                let Ok(mut pages) = collect_pages(&doc) else {
                    return;
                };
                let mut options = RenderOptions {
                    annotations: true,
                    time_budget: Some(std::time::Duration::from_secs(30)),
                    background: Some(Color::WHITE),
                    ..RenderOptions::default()
                };
                // Rotation de la vue : elle ne vaut que pour l'écran, un
                // export rend le document tel qu'il est.
                let mut view_rotation = 0;
                while let Ok(first) = req_rx.recv() {
                    // Vide la file : la dernière consigne `Keep` élimine les
                    // demandes d'échelles devenues inutiles (zoom changé), les
                    // autres sont rendues dans l'ordre d'arrivée.
                    let mut batch = Vec::new();
                    let mut keep: Option<Vec<u32>> = None;
                    let mut exports = Vec::new();
                    let mut splits = Vec::new();
                    let mut sort_in = |m: WorkerMessage, batch: &mut Vec<RenderRequest>| match m {
                        WorkerMessage::Render(r) => batch.push(r),
                        WorkerMessage::Keep(k) => keep = Some(k),
                        WorkerMessage::Export { path, format } => exports.push((path, format)),
                        WorkerMessage::Split { dir, stem, plan } => splits.push((dir, stem, plan)),
                        WorkerMessage::SetLayers(map) => {
                            // Les rendus demandés avant sont périmés.
                            batch.clear();
                            options.layers = map;
                        }
                        WorkerMessage::SetViewRotation(degrees) => {
                            batch.clear();
                            view_rotation = degrees;
                        }
                        WorkerMessage::Edit(op) => {
                            // Les rendus demandés avant la modification sont périmés.
                            batch.clear();
                            if op.apply(&doc).is_ok() {
                                if let Ok(p) = collect_pages(&doc) {
                                    pages = p;
                                }
                            }
                        }
                    };
                    sort_in(first, &mut batch);
                    while let Ok(next) = req_rx.try_recv() {
                        sort_in(next, &mut batch);
                    }
                    if let Some(k) = &keep {
                        batch.retain(|r| k.contains(&r.scale_key));
                    }
                    if !run_jobs(&doc, exports, splits, &note_tx, waker.as_ref()) {
                        return;
                    }
                    for r in batch {
                        let Some(page) = pages.get(r.page) else {
                            continue;
                        };
                        let start = std::time::Instant::now();
                        let rendered =
                            render_page_rotated(&doc, page, r.scale, view_rotation, &options);
                        let ms = start.elapsed().as_secs_f64() * 1000.0;
                        if res_tx
                            .send(RenderResult {
                                page: r.page,
                                scale_key: r.scale_key,
                                bitmap: rendered.bitmap,
                                ms,
                                warnings: rendered.warnings,
                                generation: r.generation,
                            })
                            .is_err()
                        {
                            return;
                        }
                        waker.wake();
                    }
                }
            })
            .ok()?;
        Some(Self {
            requests: req_tx,
            results: res_rx,
            notices: note_rx,
            handle: Some(handle),
            pending: Vec::new(),
            generation: 0,
        })
    }

    /// Impose la visibilité de calques pour les rendus suivants.
    pub fn set_layers(&mut self, layers: std::collections::HashMap<u32, bool>) {
        self.invalidate();
        let _ = self.requests.send(WorkerMessage::SetLayers(layers));
    }

    /// Tourne la vue : les rendus suivants montrent les pages tournées de
    /// `degrees` (multiple de 90) en plus de leur `/Rotate`. Le document du
    /// fil n'est pas touché, ses exports non plus.
    pub fn set_view_rotation(&mut self, degrees: i32) {
        self.invalidate();
        let _ = self.requests.send(WorkerMessage::SetViewRotation(degrees));
    }

    /// Les rendus en route décrivent un état dépassé : ils seront écartés à
    /// réception, et les pages redemandées.
    ///
    /// Vider la liste d'attente ne suffit pas. La clé du cache est la page et
    /// l'échelle, pas l'orientation ni les calques : un rendu parti avant le
    /// changement, arrivé après, serait pris pour neuf et resterait affiché
    /// — la page de travers jusqu'au prochain zoom. C'est la génération qui
    /// le trahit.
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending.clear();
    }

    /// Demande la conversion du document. Le fichier est écrit par le fil de
    /// rendu ; le compte rendu arrive par [`RenderWorker::take_notice`].
    pub fn export(&self, path: PathBuf, format: ExportFormat) {
        let _ = self.requests.send(WorkerMessage::Export { path, format });
    }

    /// Demande le fractionnement du document dans `dir`. Les fichiers sont
    /// écrits par le fil de rendu ; le compte rendu arrive par
    /// [`RenderWorker::take_notice`].
    pub fn split(&self, dir: PathBuf, stem: String, plan: SplitPlan) {
        let _ = self.requests.send(WorkerMessage::Split { dir, stem, plan });
    }

    /// Message du fil à afficher à l'utilisateur, s'il y en a un.
    pub fn take_notice(&mut self) -> Option<String> {
        self.notices.try_recv().ok()
    }

    /// Demande le rendu d'une page si elle n'est pas déjà en attente.
    pub fn request(&mut self, page: usize, scale: f64, scale_key: u32) {
        if self.pending.contains(&(page, scale_key)) {
            return;
        }
        if self
            .requests
            .send(WorkerMessage::Render(RenderRequest {
                page,
                scale,
                scale_key,
                generation: self.generation,
            }))
            .is_ok()
        {
            self.pending.push((page, scale_key));
        }
    }

    /// Vrai si un rendu est en attente pour cette page et cette échelle.
    #[must_use]
    pub fn is_pending(&self, page: usize, scale_key: u32) -> bool {
        self.pending.contains(&(page, scale_key))
    }

    /// Résultats disponibles (sans bloquer).
    pub fn poll(&mut self) -> Vec<RenderResult> {
        let mut out = Vec::new();
        while let Ok(r) = self.results.try_recv() {
            if r.generation != self.generation {
                continue; // rendu d'avant une modification
            }
            self.pending
                .retain(|&(p, k)| !(p == r.page && k == r.scale_key));
            out.push(r);
        }
        out
    }

    /// Applique une modification sur la copie du fil de rendu ; les rendus en
    /// attente sont abandonnés.
    pub fn edit(&mut self, op: EditOp) {
        self.invalidate();
        let _ = self.requests.send(WorkerMessage::Edit(Box::new(op)));
    }

    /// Oublie les demandes des échelles absentes de `keep` (leurs résultats
    /// seront ignorés à réception).
    pub fn forget_other_scales(&mut self, keep: &[u32]) {
        if self.pending.iter().all(|&(_, k)| keep.contains(&k)) {
            return;
        }
        self.pending.retain(|&(_, k)| keep.contains(&k));
        let _ = self.requests.send(WorkerMessage::Keep(keep.to_vec()));
    }
}

impl Drop for RenderWorker {
    fn drop(&mut self) {
        // Fermer le canal termine le fil ; on ne l'attend pas (un rendu long ne
        // doit pas bloquer la fermeture du document).
        drop(std::mem::replace(&mut self.requests, mpsc::channel().0));
        if let Some(h) = self.handle.take() {
            drop(h);
        }
    }
}

/// Remplace toutes les occurrences d'un texte dans le document, sans tenir
/// compte de la casse (voir [`replace_all_with`]).
///
/// # Errors
/// Page illisible, ou réécriture impossible.
pub fn replace_all(doc: &Document, find: &str, with: &str) -> acrux_core::Result<usize> {
    replace_all_with(doc, find, with, SearchOptions::default())
}

/// Remplace toutes les occurrences d'un texte dans le document, selon les
/// options de la recherche (casse, mot entier).
///
/// La réécriture est **chirurgicale** : chaque occurrence garde la police,
/// le corps et la couleur de ce qu'elle remplace, et le reste de la page ne
/// bouge pas d'un glyphe. Une occurrence à cheval sur deux lignes (une
/// césure) n'est pas touchée : une édition réécrit une ligne à la fois. Rend
/// le nombre d'occurrences remplacées.
///
/// # Errors
/// Page illisible, ou réécriture impossible.
pub fn replace_all_with(
    doc: &Document,
    find: &str,
    with: &str,
    options: SearchOptions,
) -> acrux_core::Result<usize> {
    if find.trim().is_empty() {
        return Ok(0);
    }
    let pages = acrux_document::collect_pages(doc)?;
    let mut edits = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let Ok(text) = acrux_features::text::extract_page_text(doc, page) else {
            continue;
        };
        for target in acrux_features::edit_text::find_ranges_with(&text, find, options) {
            edits.push(TextEdit {
                page: index,
                target,
                new_text: with.to_string(),
                style: None,
            });
        }
    }
    if edits.is_empty() {
        return Ok(0);
    }
    let count = edits.len();
    apply_edits(doc, &edits)?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use acrux_document::protect::Permissions;

    #[test]
    fn each_edit_requires_its_right() {
        let cases = [
            (
                EditOp::Rotate {
                    pages: vec![0],
                    degrees: 90,
                },
                Right::Assemble,
            ),
            (EditOp::Delete { pages: vec![0] }, Right::Assemble),
            (
                EditOp::Insert {
                    path: PathBuf::from("x.pdf"),
                    password: None,
                    pages: Vec::new(),
                    at: 0,
                },
                Right::Assemble,
            ),
            (EditOp::Reorder { order: vec![1, 0] }, Right::Assemble),
            (
                EditOp::annotate(
                    0,
                    NewAnnotation::Note {
                        x: 0.0,
                        y: 0.0,
                        contents: "note".into(),
                        color: [1.0, 0.85, 0.0],
                    },
                    None,
                ),
                Right::Annotate,
            ),
            (EditOp::Mark { marks: Vec::new() }, Right::Annotate),
            (
                EditOp::PlacedRect {
                    page: 0,
                    index: 0,
                    rect: acrux_core::Rect::new(0.0, 0.0, 1.0, 1.0),
                },
                Right::Annotate,
            ),
            (
                EditOp::SetField {
                    name: "nom".into(),
                    value: FieldValue::Text("x".into()),
                },
                Right::FillForms,
            ),
            (
                EditOp::EditText {
                    page: 0,
                    line: 0,
                    start: 0,
                    end: 1,
                    text: "x".into(),
                },
                Right::Modify,
            ),
            (
                EditOp::ReplaceAll {
                    find: "a".into(),
                    with: "b".into(),
                    options: SearchOptions::default(),
                },
                Right::Modify,
            ),
            (
                EditOp::EditObject {
                    page: 0,
                    edits: Vec::new(),
                },
                Right::Modify,
            ),
            (EditOp::ApplyRedactions, Right::Modify),
            (
                EditOp::Attach {
                    name: "a.txt".into(),
                    data: Vec::new(),
                    description: None,
                },
                Right::Modify,
            ),
            (
                EditOp::Detach {
                    name: "a.txt".into(),
                },
                Right::Modify,
            ),
            (EditOp::ResetForm, Right::FillForms),
            (EditOp::FlattenForm, Right::Modify),
        ];
        for (op, right) in cases {
            assert_eq!(op.required_right(), right, "{op:?}");
        }
    }

    /// Une page vierge, un remplacement : c'est assembler le document.
    #[test]
    fn les_pages_vierges_et_remplacees_demandent_l_assemblage() {
        let blank = EditOp::InsertBlank {
            at: 0,
            media: acrux_core::Rect::new(0.0, 0.0, 595.0, 842.0),
            rotate: 0,
        };
        let replace = EditOp::Replace {
            path: PathBuf::from("x.pdf"),
            password: None,
            pairs: vec![(0, 0)],
        };
        assert_eq!(blank.required_right(), Right::Assemble);
        assert_eq!(replace.required_right(), Right::Assemble);
    }

    #[test]
    fn rights_follow_the_permissions() {
        let all = Permissions::all();
        for right in [
            Right::Modify,
            Right::Annotate,
            Right::FillForms,
            Right::Assemble,
        ] {
            assert!(right.allowed_by(all));
        }
        let mut p = all;
        p.fill_forms = false;
        // Commenter comprend remplir.
        assert!(Right::FillForms.allowed_by(p));
        p.annotate = false;
        assert!(!Right::FillForms.allowed_by(p) && !Right::Annotate.allowed_by(p));
        p.assemble = false;
        assert!(!Right::Assemble.allowed_by(p) && Right::Modify.allowed_by(p));
    }

    /// Un geste qui balise trois lignes sur deux pages : trois annotations
    /// posées, deux sur la même page sans que la première se perde ; et la
    /// même opération appliquée à deux copies donne les mêmes identifiants.
    #[test]
    #[allow(clippy::unwrap_used)] // tests
    fn un_geste_plusieurs_pages_une_operation() {
        use acrux_features::annotations::{list_annotations, MarkupKind};
        use acrux_features::create::{new_document, PageSetup};
        // Deux pages blanches.
        let blank = new_document(&PageSetup::default()).unwrap();
        let bytes = acrux_features::pages::merge(&[&blank, &blank])
            .unwrap()
            .save_full()
            .unwrap();
        let item = |page: usize, y: f64| AnnotItem {
            page,
            annotation: NewAnnotation::Markup {
                kind: MarkupKind::Underline,
                quads: vec![acrux_core::Rect::new(50.0, y, 200.0, y + 12.0)],
                color: MarkupKind::Underline.default_color(),
                contents: None,
            },
            meta: AnnotMeta::fresh(Some("Essai")),
        };
        let op = EditOp::Annotate {
            items: vec![item(0, 700.0), item(0, 600.0), item(1, 700.0)],
        };
        let names = |doc: &Document| -> Vec<Option<String>> {
            let pages = collect_pages(doc).unwrap();
            pages
                .iter()
                .flat_map(|p| list_annotations(doc, p).unwrap())
                .map(|a| a.name)
                .collect()
        };
        let a = Document::from_bytes(bytes.clone()).unwrap();
        let b = Document::from_bytes(bytes).unwrap();
        op.apply(&a).unwrap();
        op.apply(&b).unwrap();
        let (na, nb) = (names(&a), names(&b));
        assert_eq!(na.len(), 3, "{na:?}");
        assert_eq!(na, nb, "mêmes identifiants sur les deux copies");
        assert!(na.iter().all(Option::is_some));
    }

    /// Les modifications d'un commentaire — déplacer, répondre, statuer,
    /// supprimer — appliquées à deux copies d'un même document donnent le
    /// même document : c'est ce qui rend l'annulation par rejeu juste. Et
    /// chacune exige le droit de commenter.
    #[test]
    #[allow(clippy::unwrap_used)] // tests
    fn les_modifications_de_commentaires_se_rejouent_a_l_identique() {
        use acrux_features::annotations::review::{comment_threads, ReviewState, StateChange};
        use acrux_features::annotations::{list_annotations, ShapeStyle};
        use acrux_features::create::{new_document, PageSetup};
        let bytes = new_document(&PageSetup::default())
            .unwrap()
            .save_full()
            .unwrap();
        let meta = |who: &str, date: &str| AnnotMeta {
            author: Some(who.into()),
            name: Some(format!("{who}-{date}")),
            date: Some(date.into()),
        };
        let square = |x: f64, id: &str| AnnotItem {
            page: 0,
            annotation: NewAnnotation::Square {
                rect: acrux_core::Rect::new(x, 600.0, x + 80.0, 680.0),
                style: ShapeStyle::default(),
                contents: Some("À revoir".into()),
            },
            meta: meta(id, "D:20240101000000Z"),
        };
        let ops = [
            EditOp::Annotate {
                items: vec![square(50.0, "a"), square(300.0, "b")],
            },
            EditOp::AnnotSet {
                page: 0,
                index: 0,
                changes: AnnotChanges {
                    rect: Some(acrux_core::Rect::new(60.0, 500.0, 180.0, 560.0)),
                    color: Some([0.0, 0.0, 1.0]),
                    date: Some("D:20240102000000Z".into()),
                    ..AnnotChanges::default()
                },
            },
            EditOp::AnnotReply {
                page: 0,
                index: 0,
                text: "D'accord".into(),
                meta: meta("Bruno", "D:20240103000000Z"),
            },
            EditOp::AnnotState {
                page: 0,
                index: 0,
                change: StateChange::Review(ReviewState::Accepted),
                meta: meta("Alice", "D:20240104000000Z"),
            },
            EditOp::AnnotRemove { page: 0, index: 1 },
        ];
        for op in &ops[1..] {
            assert_eq!(op.required_right(), Right::Annotate, "{op:?}");
        }
        let run = || {
            let doc = Document::from_bytes(bytes.clone()).unwrap();
            for op in &ops {
                op.apply(&doc).unwrap();
            }
            doc
        };
        let (a, b) = (run(), run());
        let summary = |doc: &Document| {
            let pages = collect_pages(doc).unwrap();
            let list: Vec<String> = list_annotations(doc, &pages[0])
                .unwrap()
                .iter()
                .map(|x| format!("{} {:?} {:?} {:?}", x.subtype, x.rect, x.modified, x.color))
                .collect();
            (list, format!("{:?}", comment_threads(doc, &pages)))
        };
        assert_eq!(summary(&a), summary(&b));
        let pages = collect_pages(&a).unwrap();
        let threads = comment_threads(&a, &pages);
        assert_eq!(threads.len(), 1, "le second carré est supprimé");
        assert_eq!(threads[0].replies.len(), 1);
        assert_eq!(
            threads[0].review.as_ref().map(|r| r.0),
            Some(ReviewState::Accepted)
        );
        assert_eq!(threads[0].color, Some([0.0, 0.0, 1.0]));
    }

    /// Effacer puis aplatir le formulaire du corpus, lu en mémoire : les
    /// champs retombent à vide, puis il n'y a plus de formulaire du tout.
    #[test]
    #[allow(clippy::unwrap_used)] // tests
    fn reset_then_flatten_the_corpus_form() {
        use acrux_features::forms::list_fields;
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/corpus/synthese/formulaire-acroform-champs.pdf");
        let doc = Document::from_bytes(std::fs::read(path).unwrap()).unwrap();
        EditOp::ResetForm.apply(&doc).unwrap();
        let fields = list_fields(&doc).unwrap();
        let nom = fields.iter().find(|f| f.name == "nom").unwrap();
        assert_eq!(nom.value, Some(FieldValue::Text(String::new())));
        EditOp::FlattenForm.apply(&doc).unwrap();
        assert!(list_fields(&doc).unwrap().is_empty());
    }

    /// Les pages d'un document à ouverture libre qui interdit la copie ne
    /// s'insèrent pas dans un autre ; avec le mot de passe des permissions,
    /// si.
    #[test]
    #[allow(clippy::unwrap_used)] // tests
    fn insert_refuses_a_source_that_forbids_copying() {
        use acrux_features::create::{new_document, PageSetup};
        let plain = new_document(&PageSetup::default())
            .unwrap()
            .save_full()
            .unwrap();
        let src = Document::from_bytes(plain.clone()).unwrap();
        let mut perms = Permissions::all();
        perms.copy = false;
        src.protect(b"", b"chef", perms).unwrap();
        let path = std::env::temp_dir().join(format!("acrux-insert-{}.pdf", std::process::id()));
        std::fs::write(&path, src.save_full().unwrap()).unwrap();
        let insert = |password: Option<Vec<u8>>| {
            let target = Document::from_bytes(plain.clone()).unwrap();
            EditOp::Insert {
                path: path.clone(),
                password,
                pages: Vec::new(),
                at: 0,
            }
            .apply(&target)
        };
        let refused = insert(None);
        let allowed = insert(Some(b"chef".to_vec()));
        let _ = std::fs::remove_file(&path);
        assert!(
            matches!(refused, Err(acrux_core::Error::Unsupported(_))),
            "{refused:?}"
        );
        assert!(allowed.is_ok(), "{allowed:?}");
    }

    /// Fractionner écrit un fichier par partie, et n'écrase jamais : un
    /// second fractionnement au même endroit prend « (2) ».
    #[test]
    #[allow(clippy::unwrap_used)] // tests
    fn fractionner_n_ecrase_rien() {
        use acrux_features::create::{new_document, PageSetup};
        let doc = new_document(&PageSetup {
            pages: 3,
            ..PageSetup::default()
        })
        .unwrap();
        let dir = std::env::temp_dir().join(format!("acrux-split-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = run_split(&doc, &dir, "essai", &SplitPlan::EveryN(1)).unwrap();
        let second = run_split(&doc, &dir, "essai", &SplitPlan::EveryN(2)).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(first.starts_with('3'), "{first}");
        assert!(second.starts_with('2'), "{second}");
        assert_eq!(
            names,
            [
                "essai-1 (2).pdf",
                "essai-1.pdf",
                "essai-2 (2).pdf",
                "essai-2.pdf",
                "essai-3.pdf"
            ]
        );
    }

    #[test]
    #[allow(clippy::unwrap_used)] // tests
    fn un_tampon_et_une_image_se_posent_pareil_sur_chaque_copie() {
        use acrux_features::create::{new_document, PageSetup};
        use acrux_features::rubber_stamp::{Options, Source, StandardStamp};
        let bytes = new_document(&PageSetup::default())
            .unwrap()
            .save_full()
            .unwrap();
        let png = acrux_graphics::encode_png_rgb(3, 2, &[90; 18]);
        let image = std::sync::Arc::new(acrux_features::stamp::prepare_image(&png).unwrap());
        let stamp = EditOp::Stamp {
            options: Box::new(Options {
                dynamic: Some("par Nina, le 23/09/2026 à 14:05".into()),
                meta: AnnotMeta::fresh(Some("Nina")),
                ..Options::new(
                    Source::Standard(StandardStamp::Approved),
                    0,
                    acrux_core::Point::new(200.0, 300.0),
                )
            }),
        };
        let add = EditOp::AddImage {
            page: 0,
            image,
            center: acrux_core::Point::new(300.0, 400.0),
            size: (90.0, 60.0),
        };
        assert_eq!(stamp.required_right(), Right::Annotate);
        assert_eq!(add.required_right(), Right::Modify);
        // Le document affiché et la copie du fil de rendu reçoivent la même
        // opération : ils doivent finir identiques.
        let copies: Vec<(String, String)> = (0..2)
            .map(|_| {
                let doc = Document::from_bytes(bytes.clone()).unwrap();
                stamp.apply(&doc).unwrap();
                add.clone().apply(&doc).unwrap();
                let doc = Document::from_bytes(doc.save_full().unwrap()).unwrap();
                let pages = collect_pages(&doc).unwrap();
                let annots =
                    acrux_features::annotations::list_annotations(&doc, &pages[0]).unwrap();
                let objects = acrux_features::edit_objects::list(&doc, &pages[0]).unwrap();
                assert_eq!(annots.len(), 1);
                assert_eq!(annots[0].subtype, "Stamp");
                assert_eq!(objects.len(), 1);
                assert_eq!(objects[0].kind, acrux_features::edit_objects::Kind::Image);
                (
                    format!(
                        "{:?} {:?} {:?}",
                        annots[0].rect, annots[0].name, annots[0].contents
                    ),
                    format!("{:?}", objects[0].bbox),
                )
            })
            .collect();
        assert_eq!(copies[0], copies[1]);
    }
}
