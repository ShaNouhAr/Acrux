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
use acrux_graphics::{Bitmap, Color};
use acrux_render::{render_page, RenderOptions};

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
    /// Donner une valeur à un champ de formulaire (apparences régénérées).
    SetField {
        /// Nom qualifié du champ.
        name: String,
        /// Valeur.
        value: FieldValue,
    },
}

impl EditOp {
    /// Applique la modification.
    ///
    /// # Errors
    /// Page absente ou document illisible.
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
    Edit(EditOp),
    /// Échelles encore utiles : les demandes des autres échelles en attente
    /// sont abandonnées (zoom changé, panneau fermé…).
    Keep(Vec<u32>),
    /// Imposer la visibilité de calques (contenu optionnel).
    SetLayers(std::collections::HashMap<u32, bool>),
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
                let Ok(mut pages) = collect_pages(&doc) else {
                    return;
                };
                let mut options = RenderOptions {
                    annotations: true,
                    time_budget: Some(std::time::Duration::from_secs(30)),
                    background: Some(Color::WHITE),
                    ..RenderOptions::default()
                };
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
                        let rendered = render_page(&doc, page, r.scale, &options);
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
        self.pending.clear();
        let _ = self.requests.send(WorkerMessage::SetLayers(layers));
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
        self.generation = self.generation.wrapping_add(1);
        self.pending.clear();
        let _ = self.requests.send(WorkerMessage::Edit(op));
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
