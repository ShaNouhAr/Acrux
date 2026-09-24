//! Combiner des fichiers et déposer des fichiers sur la fenêtre.
//!
//! # Combiner
//!
//! La fenêtre ([`crate::ui::combine`]) tient la liste ; ce module la remplit
//! (dialogue d'ouverture à sélection multiple, fichiers déposés, document
//! ouvert), demande le mot de passe d'un PDF protégé au moment de combiner,
//! puis combine sur un **fil jetable** : lire vingt fichiers et réécrire le
//! résultat peut prendre plusieurs secondes, l'interface n'attend pas avec
//! lui. Le fil rend des octets (un `Document` ne passe pas d'un fil à
//! l'autre) et réveille la fenêtre **une fois**, depuis le fil : le
//! traitement du réveil ne réveille jamais (voir `Event::Wake` dans
//! `viewer.rs`). Le résultat s'ouvre dans un onglet neuf, non enregistré
//! (`open_made_bytes`).
//!
//! # Déposer
//!
//! Plusieurs fichiers déposés sur la fenêtre ouvrent un onglet chacun, le
//! premier devenant actif, comme la ligne de commande. Les ouvertures
//! attendent dans `pending_open` et s'arrêtent le temps d'une invite de mot
//! de passe : deux fichiers protégés ne s'écrasent pas l'invite. Déposés sur
//! les vignettes, ils s'insèrent là où l'on vise (`viewer/insert.rs`) ;
//! déposés sur la fenêtre « Combiner », ils rejoignent sa liste.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use acrux_document::{collect_pages, Document};
use acrux_features::create::{combine, image_format, looks_like_pdf, CombineInput, CombineOptions};

use super::{log_line, Prompt, PromptKind, Viewer};
use crate::platform::{Cursor, Event, Frame, MouseButton, WindowHandle};
use crate::ui::combine::{CombineItem, CombineOutcome, CombineWindow, Hit};
use crate::ui::input::TextInput;
use crate::ui::lang::{self, tr, trf};

/// Octets lus en tête d'un fichier pour reconnaître sa nature : la
/// tolérance du lecteur pour l'en-tête `%PDF-`.
const HEAD: u64 = 1024;

/// Ce que rend le fil de combinaison : les octets du PDF, son nombre de
/// pages et le nombre de fichiers réunis, ou le message d'erreur.
pub(super) type Combined = Result<(Vec<u8>, usize, usize), String>;

/// Les premiers octets d'un fichier.
fn head_of(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut head = Vec::new();
    std::fs::File::open(path)?
        .take(HEAD)
        .read_to_end(&mut head)?;
    Ok(head)
}

/// Vrai si ces premiers octets sont ceux d'un texte : de l'UTF-8 (un
/// caractère coupé à la fin du morceau lu est permis), sans octet nul. Un
/// `.docx`, une archive ou un exécutable n'en sont pas.
fn looks_like_text(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    }
}

/// Nom affiché d'un chemin.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Trie des fichiers déposés : ceux qu'on sait prendre, dans l'ordre, et
/// les noms de ceux qu'on écarte. Un dossier est écarté, comme un fichier
/// illisible ou d'un format inconnu ; les PDF et les images sont pris, les
/// textes seulement quand `text_ok` (la combinaison les met en page, un
/// onglet ne sait pas les ouvrir). La nature se lit dans les octets, pas
/// dans l'extension.
pub(super) fn classify_drop(paths: &[PathBuf], text_ok: bool) -> (Vec<PathBuf>, Vec<String>) {
    let mut accepted = Vec::new();
    let mut refused = Vec::new();
    for path in paths {
        if path.is_dir() {
            refused.push(trf("{} (dossier)", &[&file_name(path)]));
            continue;
        }
        let Ok(head) = head_of(path) else {
            refused.push(file_name(path));
            continue;
        };
        if looks_like_pdf(&head)
            || image_format(&head).is_some()
            || (text_ok && looks_like_text(&head))
        {
            accepted.push(path.clone());
        } else {
            refused.push(file_name(path));
        }
    }
    (accepted, refused)
}

/// Une ligne de la liste « Combiner » pour ce fichier : sa nature, ses
/// pages, sa taille. Un PDF protégé est marqué : son mot de passe sera
/// demandé au moment de combiner. `None` pour un fichier qu'on ne sait pas
/// combiner.
fn describe(path: &Path) -> Option<CombineItem> {
    let head = head_of(path).ok()?;
    let size = std::fs::metadata(path).map_or(0, |m| m.len());
    let size = super::context::size_text(size, lang::english());
    let mut item = CombineItem {
        path: path.to_path_buf(),
        name: file_name(path),
        detail: String::new(),
        image: false,
        locked: false,
        password: None,
        data: None,
    };
    if looks_like_pdf(&head) {
        // La table des objets seulement : ni rendu, ni texte.
        let doc = Document::load(path).ok()?;
        item.locked = doc.needs_password() || !acrux_features::pages::extraction_allowed(&doc);
        item.detail = if doc.needs_password() {
            format!("PDF · {size}")
        } else {
            let pages = collect_pages(&doc).map_or(0, |p| p.len());
            trf("{} page(s) · {}", &[&pages.to_string(), &size])
        };
    } else if let Some(format) = image_format(&head) {
        item.image = true;
        item.detail = trf("Image {} · {}", &[format, &size]);
    } else if looks_like_text(&head) {
        let markdown = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"));
        let kind = if markdown { "Markdown" } else { tr("Texte") };
        item.detail = format!("{kind} · {size}");
    } else {
        return None;
    }
    Some(item)
}

/// Un fichier à combiner, tel que le fil le lira.
struct Job {
    /// Chemin (lu par le fil si `data` manque).
    path: PathBuf,
    /// Nom du signet.
    name: String,
    /// Contenu déjà en mémoire.
    data: Option<Arc<Vec<u8>>>,
    /// Mot de passe d'un PDF protégé.
    password: Option<Vec<u8>>,
}

/// Le travail du fil : lire les fichiers, les combiner, écrire le résultat.
fn combine_job(jobs: Vec<Job>, options: &CombineOptions) -> Combined {
    let files = jobs.len();
    let mut inputs = Vec::with_capacity(files);
    for job in jobs {
        let extension = job
            .path
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();
        let read = match job.data {
            Some(data) => CombineInput::from_bytes(job.name.clone(), data.to_vec(), &extension),
            None => CombineInput::from_path(&job.path),
        };
        let mut input = read.map_err(|e| format!("{}\n\n{e}", job.name))?;
        input.name = job.name;
        input.password = job.password;
        inputs.push(input);
    }
    let doc = combine(&inputs, options).map_err(|e| e.to_string())?;
    let pages = collect_pages(&doc).map_or(0, |p| p.len());
    let bytes = doc.save_full().map_err(|e| e.to_string())?;
    Ok((bytes, pages, files))
}

impl Viewer {
    /// Ouvre la fenêtre « Combiner des fichiers ». Le document ouvert y est
    /// déjà, comme dans Acrobat — avec ses modifications pas encore
    /// enregistrées, s'il en a.
    pub(super) fn open_combine(&mut self) {
        self.palette = None;
        self.context_menu = None;
        let mut window = CombineWindow::new();
        if let Some(l) = &self.loaded {
            let unsaved = l.modified || l.temporary;
            let data = if unsaved {
                l.doc.save_full().ok().map(Arc::new)
            } else {
                None
            };
            if !unsaved || data.is_some() {
                let size = data.as_ref().map_or_else(
                    || std::fs::metadata(&l.path).map_or(0, |m| m.len()),
                    |d| d.len() as u64,
                );
                window.add(vec![CombineItem {
                    path: l.path.clone(),
                    name: file_name(&l.path),
                    detail: trf(
                        "{} page(s) · {}",
                        &[
                            &l.pages.len().to_string(),
                            &super::context::size_text(size, lang::english()),
                        ],
                    ),
                    image: false,
                    // Ouvert avec le seul mot de passe d'ouverture, un
                    // document qui interdit la copie demandera celui des
                    // permissions au moment de combiner.
                    locked: !acrux_features::pages::extraction_allowed(&l.doc),
                    password: l.password.clone(),
                    data,
                }]);
            }
        }
        log_line(&format!(
            "combinaison : fenêtre ouverte ({} fichier(s))",
            window.items.len()
        ));
        self.combine = Some(window);
    }

    /// Ajoute des fichiers à la liste « Combiner » ; ceux qu'on ne sait pas
    /// combiner sont nommés en bas de la fenêtre.
    fn combine_add_paths(&mut self, paths: &[PathBuf]) {
        let (accepted, mut refused) = classify_drop(paths, true);
        let mut items = Vec::with_capacity(accepted.len());
        for path in accepted {
            match describe(&path) {
                Some(item) => items.push(item),
                None => refused.push(file_name(&path)),
            }
        }
        let Some(window) = &mut self.combine else {
            return;
        };
        let added = window.add(items);
        log_line(&format!(
            "combinaison : {added} fichier(s) ajouté(s), {} en tout",
            window.items.len()
        ));
        window.set_note((!refused.is_empty()).then(|| {
            trf(
                "Ignoré(s), ni PDF, ni image, ni texte : {}",
                &[&refused.join(", ")],
            )
        }));
    }

    /// « Ajouter des fichiers… » : le dialogue d'ouverture, plusieurs
    /// fichiers d'un coup.
    fn combine_add_files(&mut self, window: &mut dyn WindowHandle) {
        let paths = window.open_files_dialog(tr("Ajouter des fichiers"));
        if !paths.is_empty() {
            self.combine_add_paths(&paths);
        }
    }

    /// « Combiner » : demande d'abord le mot de passe d'un PDF protégé qui
    /// ne l'a pas encore, puis lance le fil de combinaison.
    fn run_combine(&mut self, window: &mut dyn WindowHandle) {
        let Some(w) = &mut self.combine else { return };
        if w.items.is_empty() || w.busy() || self.combine_rx.is_some() {
            return;
        }
        if let Some(index) = w.items.iter().position(|i| i.locked) {
            let name = w.items[index].name.clone();
            let mut input = TextInput::new(tr("Mot de passe"));
            input.masked = true;
            self.prompt = Some(Prompt::new(
                tr("Fichier protégé"),
                lang::trf("Mot de passe pour « {} » :", &[&name]),
                input,
                PromptKind::CombinePassword { index },
            ));
            return;
        }
        let jobs: Vec<Job> = w
            .items
            .iter()
            .map(|i| Job {
                path: i.path.clone(),
                name: i.name.clone(),
                data: i.data.clone(),
                password: i.password.clone(),
            })
            .collect();
        let options = CombineOptions {
            bookmarks: w.bookmarks,
            keep_forms: w.keep_forms,
            ..CombineOptions::default()
        };
        log_line(&format!(
            "combinaison : lancée, {} fichier(s), signets {}, formulaires {}",
            jobs.len(),
            w.bookmarks,
            w.keep_forms
        ));
        w.set_busy(true);
        w.set_note(None);
        let (tx, rx) = std::sync::mpsc::channel();
        let waker = window.waker();
        // Un fil jetable, qui réveille la fenêtre une seule fois, son
        // résultat envoyé. Si l'onglet ou l'application se ferme avant, le
        // résultat tombe dans le vide : `send` échoue sans bruit.
        let spawned = std::thread::Builder::new()
            .name("combinaison".into())
            .spawn(move || {
                let _ = tx.send(combine_job(jobs, &options));
                waker.wake();
            });
        if spawned.is_err() {
            w.set_busy(false);
            self.alert(
                tr("Combinaison impossible"),
                tr("Le travail n'a pas pu démarrer."),
            );
            return;
        }
        self.combine_rx = Some(rx);
    }

    /// Le mot de passe d'un fichier de la liste « Combiner » : vérifié ici,
    /// pour le redemander tout de suite s'il est faux. Rend l'erreur à
    /// afficher dans l'invite, ou rien si l'invite est close — la
    /// combinaison reprend alors, avec le fichier protégé suivant ou pour de
    /// bon.
    pub(super) fn combine_password_entered(
        &mut self,
        index: usize,
        value: &str,
        window: &mut dyn WindowHandle,
    ) -> Option<String> {
        let Some(item) = self.combine.as_ref().and_then(|w| w.items.get(index)) else {
            self.prompt = None;
            return None;
        };
        let bytes = match &item.data {
            Some(data) => Ok(data.to_vec()),
            None => std::fs::read(&item.path),
        };
        let doc = match bytes
            .map_err(|e| e.to_string())
            .and_then(|b| Document::from_bytes(b).map_err(|e| e.to_string()))
        {
            Ok(doc) => doc,
            Err(e) => return Some(e),
        };
        let password = value.as_bytes().to_vec();
        let _ = doc.authenticate(&password);
        if doc.needs_password() {
            return Some(tr("Mot de passe incorrect").into());
        }
        if !acrux_features::pages::extraction_allowed(&doc) {
            return Some(tr("Il faut le mot de passe des permissions.").into());
        }
        let pages = collect_pages(&doc).map_or(0, |p| p.len());
        if let Some(item) = self.combine.as_mut().and_then(|w| w.items.get_mut(index)) {
            item.password = Some(password);
            item.locked = false;
            let size = item.data.as_ref().map_or_else(
                || std::fs::metadata(&item.path).map_or(0, |m| m.len()),
                |d| d.len() as u64,
            );
            item.detail = trf(
                "{} page(s) · {}",
                &[
                    &pages.to_string(),
                    &super::context::size_text(size, lang::english()),
                ],
            );
        }
        self.prompt = None;
        self.run_combine(window);
        None
    }

    /// Relève le résultat de la combinaison, s'il est arrivé : il s'ouvre
    /// dans un onglet neuf et la fenêtre se ferme ; en cas d'échec, elle
    /// reste ouverte, sa liste intacte. Rend vrai s'il faut repeindre.
    ///
    /// Appelé au réveil que le fil a posté ; ne réveille jamais lui-même.
    pub(super) fn poll_combine(&mut self, window: &mut dyn WindowHandle) -> bool {
        let Some(rx) = &self.combine_rx else {
            return false;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(tr("Le travail s'est interrompu.").to_string())
            }
        };
        self.combine_rx = None;
        match result {
            Ok((bytes, pages, files)) => {
                log_line(&format!("combinaison : {files} fichiers, {pages} pages"));
                let bookmarks = self.combine.take().is_some_and(|w| w.bookmarks);
                let opened = self.open_made_bytes(
                    tr("Fichiers combinés"),
                    &bytes,
                    (
                        "Combinaison impossible",
                        trf(
                            "{} fichiers combinés ({} pages) — Ctrl+S pour enregistrer",
                            &[&files.to_string(), &pages.to_string()],
                        ),
                    ),
                    window,
                );
                // Le document combiné s'ouvre sur ses signets, un par
                // fichier, comme le demande son `/PageMode /UseOutlines` et
                // comme le fait Acrobat.
                if opened && bookmarks {
                    self.panel_open = true;
                    self.panel.tab = crate::ui::panel::PanelTab::Bookmarks;
                    self.wake_anim();
                    self.clamp_scroll();
                }
            }
            Err(message) => {
                log_line(&format!("combinaison : échec — {message}"));
                if let Some(w) = &mut self.combine {
                    w.set_busy(false);
                }
                self.alert(tr("Combinaison impossible"), &message);
            }
        }
        window.request_redraw();
        true
    }

    /// Donne l'événement à la fenêtre « Combiner », si elle est ouverte :
    /// elle prend tout le clavier, la souris et la molette (elle est
    /// modale), et accueille les fichiers déposés. Le redimensionnement, le
    /// réveil et la fermeture suivent leur cours : rend faux pour eux.
    pub(super) fn combine_event(&mut self, event: &Event, window: &mut dyn WindowHandle) -> bool {
        let Some(w) = &mut self.combine else {
            return false;
        };
        let outcome = match event {
            Event::Key(key, m) => w.key(*key, *m),
            Event::MouseDown {
                button: MouseButton::Left,
                x,
                y,
                ..
            } => {
                self.tip = None;
                w.mouse_down(*x, *y)
            }
            Event::MouseUp { x, y, .. } => {
                self.drag_last = None;
                self.sel_dragging = false;
                w.mouse_up(*x, *y)
            }
            Event::MouseMove { x, y, dragging, .. } => {
                self.tip = None;
                let changed = w.mouse_move(*x, *y, *dragging);
                window.set_cursor(match w.target_at(*x, *y) {
                    Some(Hit::Row(_) | Hit::List) | None => Cursor::Arrow,
                    Some(_) => Cursor::Hand,
                });
                if changed {
                    CombineOutcome::Redraw
                } else {
                    CombineOutcome::None
                }
            }
            Event::Wheel { delta, .. } => {
                if w.wheel(*delta) {
                    CombineOutcome::Redraw
                } else {
                    CombineOutcome::None
                }
            }
            Event::FilesDropped { paths, .. } => {
                if !w.busy() {
                    let paths = paths.clone();
                    self.combine_add_paths(&paths);
                }
                CombineOutcome::Redraw
            }
            // Les caractères, les autres boutons, les boutons latéraux
            // s'arrêtent là : rien n'atteint le document sous la fenêtre.
            Event::Char(..) | Event::MouseDown { .. } | Event::Nav { .. } => CombineOutcome::None,
            _ => return false,
        };
        match outcome {
            CombineOutcome::None => {}
            CombineOutcome::Redraw => window.request_redraw(),
            CombineOutcome::AddFiles => {
                self.combine_add_files(window);
                window.request_redraw();
            }
            CombineOutcome::Combine => {
                self.run_combine(window);
                window.request_redraw();
            }
            CombineOutcome::Close => {
                log_line("combinaison : fenêtre fermée");
                self.combine = None;
                window.request_redraw();
            }
        }
        true
    }

    /// Peint la fenêtre « Combiner », par-dessus la page.
    pub(super) fn paint_combine(&mut self, frame: &mut Frame<'_>) {
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        let (Some(w), Some(text)) = (self.combine.as_mut(), self.text.as_mut()) else {
            return;
        };
        w.paint(frame, text, &mut self.raster, &theme, dpi);
    }

    // --- Déposer ------------------------------------------------------------------

    /// Des fichiers déposés sur la fenêtre (hors de la fenêtre « Combiner »,
    /// qui les prend elle-même) : sur les vignettes, une insertion à
    /// l'endroit visé ; ailleurs, un onglet par fichier.
    pub(super) fn files_dropped(
        &mut self,
        paths: &[PathBuf],
        x: i32,
        y: i32,
        window: &mut dyn WindowHandle,
    ) {
        let (accepted, refused) = classify_drop(paths, false);
        if !refused.is_empty() {
            self.alert(
                tr("Fichiers ignorés"),
                &trf(
                    "Acrux ouvre les PDF et les images (PNG, JPEG, BMP, GIF, TIFF). Ignoré(s) :\n\n{}",
                    &[&refused.join("\n")],
                ),
            );
        }
        if accepted.is_empty() {
            return;
        }
        let (left, top) = (self.view_left() as i32, self.view_top() as i32);
        if self.loaded.is_some() && self.thumbs_shown() && x >= 0 && x < left && y >= top {
            if let Some(at) = self.panel.insertion_at(x, y - top) {
                self.insert_dropped(&accepted, at);
                window.request_redraw();
                return;
            }
        }
        self.queue_open(accepted);
        self.drain_pending_open(window);
        window.request_redraw();
    }

    /// Met des fichiers en attente d'ouverture ; le premier d'entre eux
    /// deviendra l'onglet actif. Chaque document ouvert se range juste
    /// après l'onglet actif : le premier arrivera donc là.
    pub(super) fn queue_open(&mut self, paths: Vec<PathBuf>) {
        if self.pending_focus.is_none() {
            self.pending_focus = Some(if self.loaded.is_some() {
                self.active_tab + 1
            } else {
                0
            });
        }
        self.pending_open.extend(paths);
    }

    /// Ouvre les fichiers en attente, un onglet chacun, puis rend la main au
    /// premier. S'arrête tant qu'une invite (un mot de passe) est ouverte :
    /// la suite reprend quand elle se referme.
    pub(super) fn drain_pending_open(&mut self, window: &mut dyn WindowHandle) {
        while self.prompt.is_none() && !self.pending_open.is_empty() {
            let path = self.pending_open.remove(0);
            self.leave_document_modes();
            self.open(&path, window);
        }
        if self.pending_open.is_empty() {
            if let Some(first) = self.pending_focus.take() {
                if first < self.tab_count() {
                    self.select_tab(first);
                }
            }
        }
    }

    /// Quitte ce qui travaille sur le document actif — saisie en cours,
    /// mode « Modifier le PDF », outil d'annotation — avant qu'un autre
    /// document ne prenne sa place : sans cela, une saisie resterait
    /// attachée au mauvais document.
    pub(super) fn leave_document_modes(&mut self) {
        self.flush_typing();
        self.edit = None;
        self.close_annot_tools();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests
mod tests {
    use super::*;

    #[test]
    fn un_depot_garde_les_pdf_et_les_images_dans_l_ordre() {
        let dir = std::env::temp_dir().join(format!("acrux-depot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("dossier")).unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let image = write("photo.png", &acrux_graphics::encode_png_rgb(1, 1, &[0; 3]));
        let pdf = write("rapport.pdf", b"%PDF-1.7\n1 0 obj << >> endobj\n");
        let docx = write("lettre.docx", b"PK\x03\x04\x14\x00\x06\x00\x08\x00\x00\x00");
        let notes = write("notes.md", "# Été\n".as_bytes());
        let list = vec![
            image.clone(),
            dir.join("dossier"),
            docx,
            pdf.clone(),
            notes.clone(),
            dir.join("absent.pdf"),
        ];
        let (accepted, refused) = classify_drop(&list, false);
        assert_eq!(accepted, [image.clone(), pdf.clone()]);
        assert_eq!(refused.len(), 4, "{refused:?}");
        assert!(refused[0].contains("dossier"));
        // La combinaison prend aussi les textes.
        let (accepted, _) = classify_drop(&list, true);
        assert_eq!(accepted, [image, pdf, notes]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn un_texte_se_reconnait_meme_coupe() {
        assert!(looks_like_text("Été".as_bytes()));
        // Un « é » coupé en deux par la fin du morceau lu.
        assert!(looks_like_text(&"é".as_bytes()[..1]));
        assert!(!looks_like_text(b"ab\0cd"));
        assert!(!looks_like_text(&[0xFF, 0xFE, 0x41]));
    }
}
