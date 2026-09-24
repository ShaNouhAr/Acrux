//! Rendu des pages sur un fil d'exécution séparé : l'interface reste fluide
//! pendant qu'une page lourde se dessine. Le fil possède sa propre copie du
//! document (le modèle objet n'est pas partageable entre fils) ; les demandes
//! et les résultats transitent par des canaux, et le fil réveille la fenêtre
//! via [`Waker`] quand un résultat est prêt.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use acrux_document::{collect_pages, Document};
use acrux_features::annotations::{add_annotation, NewAnnotation};
use acrux_features::edit_text::{apply_edits, TextEdit, TextRange};
use acrux_features::export::{
    export_docx, export_html, export_markdown, export_pages_jpeg, export_pages_png,
    export_tables_xlsx, export_text, HtmlOptions,
};
use acrux_features::forms::{set_field_value, FieldValue};
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
    /// Ajouter une annotation à une page.
    Annotate {
        /// Indice de page.
        page: usize,
        /// Annotation.
        annotation: NewAnnotation,
        /// Auteur (`/T`).
        author: Option<String>,
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
    /// Droit que la modification exige d'un document protégé. C'est le
    /// point unique où le visualiseur décide si un utilisateur qui n'a que
    /// le mot de passe d'ouverture peut la faire.
    #[must_use]
    pub fn required_right(&self) -> Right {
        match self {
            EditOp::Rotate { .. }
            | EditOp::Delete { .. }
            | EditOp::Insert { .. }
            | EditOp::Reorder { .. } => Right::Assemble,
            EditOp::Annotate { .. }
            | EditOp::Mark { .. }
            | EditOp::FillSign { .. }
            | EditOp::PlacedRect { .. } => Right::Annotate,
            EditOp::SetField { .. } | EditOp::ResetForm => Right::FillForms,
            EditOp::EditText { .. }
            | EditOp::ReplaceAll { .. }
            | EditOp::EditObject { .. }
            | EditOp::ApplyRedactions
            | EditOp::Attach { .. }
            | EditOp::Detach { .. }
            | EditOp::Paragraph { .. }
            | EditOp::FlattenForm => Right::Modify,
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
            EditOp::Annotate {
                page,
                annotation,
                author,
            } => {
                let pages = collect_pages(doc)?;
                let p = pages
                    .get(*page)
                    .ok_or_else(|| acrux_core::Error::Corrupt("page absente".into()))?;
                add_annotation(doc, p, annotation, author.as_deref()).map(|_| ())
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
            EditOp::Mark { marks } => mark_redactions(doc, marks).map(|_| ()),
            EditOp::ApplyRedactions => apply_redactions(doc).map(|_| ()),
            EditOp::Insert {
                path,
                password,
                pages,
                at,
            } => {
                let src = Document::load(path)?;
                if let Some(pw) = password {
                    src.authenticate(pw)?;
                }
                // Insérer les pages d'un document protégé dans un autre,
                // c'est en extraire le contenu : un document à ouverture
                // libre qui interdit la copie ne se recopie pas ainsi dans
                // un fichier sans permissions. Son propriétaire, lui, le peut.
                let restricted = src.security().is_some_and(|h| {
                    !h.is_owner()
                        && !acrux_document::protect::Permissions::from_p(h.permissions()).copy
                });
                if restricted {
                    return Err(acrux_core::Error::Unsupported(
                        "les permissions de ce document interdisent d'en extraire les pages".into(),
                    ));
                }
                let indices: Vec<usize> = if pages.is_empty() {
                    (0..collect_pages(&src)?.len()).collect()
                } else {
                    pages.clone()
                };
                acrux_features::pages::insert_pages_from(doc, &src, &indices, *at)
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
                    let mut sort_in = |m: WorkerMessage, batch: &mut Vec<RenderRequest>| match m {
                        WorkerMessage::Render(r) => batch.push(r),
                        WorkerMessage::Keep(k) => keep = Some(k),
                        WorkerMessage::Export { path, format } => exports.push((path, format)),
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
                    for (path, format) in exports {
                        let notice = match run_export(&doc, &path, format) {
                            Ok(message) => message,
                            Err(e) => format!("export impossible : {e}"),
                        };
                        if note_tx.send(notice).is_err() {
                            return;
                        }
                        waker.wake();
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
                EditOp::Annotate {
                    page: 0,
                    annotation: NewAnnotation::Note {
                        x: 0.0,
                        y: 0.0,
                        contents: "note".into(),
                        color: [1.0, 0.85, 0.0],
                    },
                    author: None,
                },
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
}
