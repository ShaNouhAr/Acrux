//! Insérer des pages venues d'un fichier : la feuille « Insérer des pages »
//! ([`crate::ui::insertpages`]) — quelles pages, où —, le mot de passe
//! d'une source protégée, et les fichiers déposés sur les vignettes.
//!
//! La source est lue **une fois**, au geste, et gardée en mémoire dans
//! l'opération ([`InsertSource`]) : l'annulation la rejoue sans relire le
//! fichier, qui peut avoir changé ou disparu entre-temps. Une image est
//! convertie en PDF à ce moment-là (un TIFF de scanner donne une page par
//! feuille), comme à l'ouverture.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use acrux_document::{collect_pages, Document};
use acrux_features::create::{from_images, image_format, looks_like_pdf, ImageInput, ImageLayout};

use super::{log_line, EditOp, Prompt, PromptKind, Viewer};
use crate::platform::{Cursor, Event, Frame, MouseButton, WindowHandle};
use crate::render_worker::{InsertSource, Right};
use crate::ui::input::TextInput;
use crate::ui::insertpages::{InsertSheet, SheetAction, Target};
use crate::ui::lang::{tr, trf};

/// Ce que donne la lecture d'une source d'insertion.
pub(super) enum SourceLoad {
    /// La source, prête, et son nombre de pages.
    Ready(InsertSource, usize),
    /// Un PDF protégé dont on n'a pas (ou pas le bon) mot de passe.
    Locked,
}

/// Lit une source d'insertion : un PDF, gardé tel quel, ou une image,
/// convertie en PDF. Rend le message à afficher pour ce qui ne se prête pas
/// à une insertion — ni PDF ni image, ou des permissions qui interdisent
/// d'en extraire les pages.
pub(super) fn load_insert_source(
    path: &Path,
    password: Option<Vec<u8>>,
) -> Result<SourceLoad, String> {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let data = std::fs::read(path).map_err(|e| format!("{}\n\n{e}", path.display()))?;
    if looks_like_pdf(&data) {
        let doc = Document::from_bytes(data.clone()).map_err(|e| format!("{name}\n\n{e}"))?;
        if let Some(pw) = &password {
            let _ = doc.authenticate(pw);
        }
        if doc.needs_password() {
            return Ok(SourceLoad::Locked);
        }
        if !acrux_features::pages::extraction_allowed(&doc) {
            return Err(trf(
                "Les permissions de « {} » interdisent d'en extraire les pages. Ouvrez-le dans un onglet avec le mot de passe des permissions, puis recommencez.",
                &[&name],
            ));
        }
        let count = collect_pages(&doc).map_or(0, |p| p.len());
        return Ok(SourceLoad::Ready(
            InsertSource {
                name,
                pdf: data,
                password,
                forms: true,
            },
            count,
        ));
    }
    if image_format(&data).is_some() {
        let stem = path
            .file_stem()
            .map_or_else(|| name.clone(), |s| s.to_string_lossy().into_owned());
        let pdf = from_images(&[ImageInput { data, name: stem }], &ImageLayout::default())
            .and_then(|doc| {
                let count = collect_pages(&doc)?.len();
                Ok((doc.save_full()?, count))
            })
            .map_err(|e| format!("{name}\n\n{e}"))?;
        return Ok(SourceLoad::Ready(
            InsertSource {
                name,
                pdf: pdf.0,
                password: None,
                forms: false,
            },
            pdf.1,
        ));
    }
    Err(trf(
        "« {} » n'est ni un PDF ni une image (PNG, JPEG, BMP, GIF, TIFF).",
        &[&name],
    ))
}

impl Viewer {
    /// « Insérer des pages » : choisir le fichier, puis, dans la feuille,
    /// les pages et la position.
    pub(super) fn insert_pages(&mut self, window: &mut dyn WindowHandle) {
        // Le droit se vérifie avant de faire choisir un fichier pour rien.
        if self.loaded.is_none() || !self.require_right(Right::Assemble) {
            return;
        }
        let Some(path) = window.open_file_dialog() else {
            return;
        };
        self.prepare_insert(&path);
    }

    /// Lit la source choisie : la feuille s'ouvre, ou l'invite du mot de
    /// passe d'abord, ou le message qui dit pourquoi ce fichier ne convient
    /// pas.
    fn prepare_insert(&mut self, path: &Path) {
        // Un fichier protégé déjà ouvert dans un onglet prête son mot de
        // passe.
        let password = self.password_of_open(path);
        match load_insert_source(path, password) {
            Ok(SourceLoad::Ready(source, count)) => self.open_insert_sheet(source, count),
            Ok(SourceLoad::Locked) => {
                let name = path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                let mut input = TextInput::new(tr("Mot de passe"));
                input.masked = true;
                self.prompt = Some(Prompt::new(
                    tr("Fichier protégé"),
                    trf("Mot de passe pour « {} » :", &[&name]),
                    input,
                    PromptKind::InsertPassword {
                        path: path.to_path_buf(),
                    },
                ));
            }
            Err(message) => self.alert(tr("Insertion impossible"), &message),
        }
    }

    /// Le mot de passe d'une source d'insertion protégée. Rend l'erreur à
    /// afficher dans l'invite, ou rien si elle est close (la feuille s'ouvre
    /// alors, ou le message qui dit pourquoi le fichier ne convient pas).
    pub(super) fn insert_password_entered(&mut self, path: &Path, value: &str) -> Option<String> {
        match load_insert_source(path, Some(value.as_bytes().to_vec())) {
            Ok(SourceLoad::Ready(source, count)) => {
                self.prompt = None;
                self.open_insert_sheet(source, count);
                None
            }
            Ok(SourceLoad::Locked) => Some(tr("Mot de passe incorrect").into()),
            Err(message) => Some(message),
        }
    }

    /// Ouvre la feuille « Insérer des pages » : par défaut, toutes les
    /// pages, après la dernière page visée (la page courante, ou la
    /// sélection des vignettes).
    fn open_insert_sheet(&mut self, source: InsertSource, count: usize) {
        let Some(l) = &self.loaded else { return };
        let doc_pages = l.pages.len();
        let after = self.target_pages().last().map_or(doc_pages, |p| p + 1);
        log_line(&format!(
            "insertion : feuille pour « {} » ({count} page(s)), après la page {after}",
            source.name
        ));
        let sheet = InsertSheet::new(&source.name, count, doc_pages, after);
        self.insert_sheet = Some((sheet, Arc::new(source)));
    }

    /// Insère : une modification, une annulation.
    fn insert_now(
        &mut self,
        source: Arc<InsertSource>,
        pages: Vec<usize>,
        at: usize,
        count: usize,
    ) {
        log_line(&format!(
            "pages : Insert « {} » {pages:?} at = {at}",
            source.name
        ));
        let inserted = if pages.is_empty() { count } else { pages.len() };
        if !self.apply_edit(EditOp::Insert { source, pages, at }) {
            return;
        }
        self.scroll_to_page(at);
        self.reveal_thumbnail(at);
        self.set_notice(trf(
            "{} page(s) insérée(s) — Ctrl+Z pour annuler",
            &[&inserted.to_string()],
        ));
    }

    /// Des fichiers déposés sur les vignettes : chacun s'insère en entier,
    /// dans l'ordre, à l'endroit visé (`at`, 0 = avant la première page).
    /// Un fichier protégé est écarté, avec un mot : son mot de passe se
    /// donne par « Insérer des pages ».
    pub(super) fn insert_dropped(&mut self, paths: &[PathBuf], at: usize) {
        if !self.require_right(Right::Assemble) {
            return;
        }
        let mut at = at;
        let mut files = 0;
        let mut pages = 0;
        let mut problems = Vec::new();
        for path in paths {
            let password = self.password_of_open(path);
            match load_insert_source(path, password) {
                Ok(SourceLoad::Ready(source, count)) => {
                    log_line(&format!(
                        "pages : Insert « {} » (déposé) at = {at}",
                        source.name
                    ));
                    let op = EditOp::Insert {
                        source: Arc::new(source),
                        pages: Vec::new(),
                        at,
                    };
                    if !self.apply_edit(op) {
                        break;
                    }
                    at += count;
                    files += 1;
                    pages += count;
                }
                Ok(SourceLoad::Locked) => problems.push(trf(
                    "« {} » est protégé par un mot de passe : passez par « Insérer des pages ».",
                    &[&path
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())],
                )),
                Err(message) => problems.push(message),
            }
        }
        if !problems.is_empty() {
            self.alert(tr("Insertion impossible"), &problems.join("\n\n"));
        }
        if files == 0 {
            return;
        }
        let first = at - pages;
        self.scroll_to_page(first);
        self.reveal_thumbnail(first);
        self.set_notice(if files == 1 {
            trf(
                "{} page(s) insérée(s) — Ctrl+Z pour annuler",
                &[&pages.to_string()],
            )
        } else {
            trf(
                "{} fichiers insérés ({} pages) — Ctrl+Z annule le dernier",
                &[&files.to_string(), &pages.to_string()],
            )
        });
    }

    /// Donne l'événement à la feuille « Insérer des pages », si elle est
    /// ouverte (voir `protect_event`, dont elle suit le routage).
    pub(super) fn insert_event(&mut self, event: &Event, window: &mut dyn WindowHandle) -> bool {
        let Some((sheet, _)) = &mut self.insert_sheet else {
            return false;
        };
        let action = match *event {
            Event::Key(key, m) => sheet.key(key, m.shift),
            // Ctrl+V colle dans le champ qui a le focus ; les autres
            // raccourcis n'atteignent pas le document sous la feuille.
            Event::Char(c, m) if m.ctrl => {
                if matches!(c, 'v' | 'V' | '\u{16}') {
                    let text = window.clipboard_text().unwrap_or_default();
                    sheet.paste(&text)
                } else {
                    SheetAction::None
                }
            }
            Event::Char(c, _) => sheet.char(c),
            Event::MouseDown {
                button: MouseButton::Left,
                x,
                y,
                ..
            } => sheet.mouse_down(x, y),
            Event::MouseUp { x, y, .. } => sheet.mouse_up(x, y),
            Event::MouseMove { x, y, .. } => {
                if sheet.mouse_move(x, y) {
                    window.request_redraw();
                }
                window.set_cursor(match sheet.target_at(x, y) {
                    Some(Target::Pages | Target::Number) => Cursor::IBeam,
                    Some(_) => Cursor::Hand,
                    None => Cursor::Arrow,
                });
                SheetAction::None
            }
            // Le reste de la souris, et un fichier déposé (il ouvrirait un
            // autre document sous la feuille), s'arrêtent là.
            Event::MouseDown { .. }
            | Event::Nav { .. }
            | Event::Wheel { .. }
            | Event::FilesDropped { .. } => SheetAction::None,
            _ => return false,
        };
        match action {
            SheetAction::None => {}
            SheetAction::Redraw => window.request_redraw(),
            SheetAction::Cancel => {
                log_line("insertion : feuille fermée");
                self.insert_sheet = None;
                window.request_redraw();
            }
            SheetAction::Insert { pages, at } => {
                if let Some((sheet, source)) = self.insert_sheet.take() {
                    let count = sheet.source_pages();
                    self.insert_now(source, pages, at, count);
                }
                window.request_redraw();
            }
        }
        true
    }

    /// Vrai si la feuille a le focus dans un champ : l'espace y écrit.
    pub(super) fn insert_typing(&self) -> bool {
        self.insert_sheet
            .as_ref()
            .is_some_and(|(sheet, _)| sheet.typing())
    }

    /// Peint la feuille « Insérer des pages », par-dessus la page.
    pub(super) fn paint_insert_sheet(&mut self, frame: &mut Frame<'_>) {
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        let (Some((sheet, _)), Some(text)) = (self.insert_sheet.as_mut(), self.text.as_mut())
        else {
            return;
        };
        sheet.paint(frame, text, &theme, dpi);
    }
}
