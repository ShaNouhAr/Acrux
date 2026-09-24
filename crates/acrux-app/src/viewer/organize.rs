//! Organiser les pages : la sélection des vignettes et ce qu'on en fait —
//! pivoter, supprimer, dupliquer, extraire, glisser un bloc —, les pages
//! vierges, le remplacement de pages et le fractionnement du document.
//!
//! # Ce que vise une commande de page
//!
//! Toutes les commandes de page passent par [`Viewer::target_pages`] : les
//! vignettes sous le voile d'accent si le panneau les montre, sinon la page
//! courante, ou la page d'un clic droit sur la page. Ce qu'on voit
//! sélectionné est **exactement** ce sur quoi on agit — le pire bogue d'un
//! organiseur serait d'agir sur une autre sélection que celle qu'il montre.
//! `R`, `Maj+R`, `Ctrl+Suppr`, la palette, la colonne d'outils et le menu des
//! vignettes y passent tous.
//!
//! Une commande sur plusieurs pages est **une** modification (une seule
//! `EditOp`) : pivoter deux cents pages sélectionnées, c'est une entrée
//! d'historique, un rejeu au `Ctrl+Z`, un envoi au fil de rendu.
//!
//! # Fractionner
//!
//! La question « Fractionner le document » propose les découpages ; le
//! nombre de pages et la taille se demandent ensuite dans une invite. Le
//! dossier de sortie se choisit dans le dialogue d'enregistrement habituel
//! (le nom proposé, `rapport-1.pdf`, donne aussi le début des noms). Les
//! fichiers sont écrits par le fil de rendu — son document a rejoué les
//! modifications : on fractionne le document **tel qu'il s'affiche** — et un
//! seul avis arrive à la fin, par le canal des avis du fil.

use std::path::{Path, PathBuf};

use acrux_document::{collect_pages, Document};
use acrux_features::navigation::Action;
use acrux_features::pages::split::{parse_size, SplitPlan};
use acrux_features::pages::{duplicate_block, move_block};

use super::context::Target;
use super::dialogs::Then;
use super::{log_line, EditOp, Prompt, PromptKind, Region, Viewer};
use crate::platform::WindowHandle;
use crate::render_worker::Right;
use crate::ui::input::TextInput;
use crate::ui::lang::{tr, trf};
use crate::ui::panel::{PanelTab, SelectMode};

/// Un mégaoctet (binaire) : l'unité de la taille maximale tapée sans unité.
const MIB: u64 = 1 << 20;

/// Comment fractionner, dans l'ordre des boutons de la question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SplitKind {
    /// Des tranches de N pages (N se demande ensuite).
    EveryN,
    /// Un fichier par signet de premier niveau.
    Bookmarks,
    /// Des fichiers d'au plus tant de mégaoctets (demandé ensuite).
    MaxSize,
    /// Une page par fichier.
    Each,
}

impl SplitKind {
    /// Les découpages proposés : « par signets » seulement quand le
    /// document a des signets de premier niveau qui mènent à une page.
    pub(super) fn choices(with_bookmarks: bool) -> Vec<SplitKind> {
        let mut out = vec![SplitKind::EveryN];
        if with_bookmarks {
            out.push(SplitKind::Bookmarks);
        }
        out.extend([SplitKind::MaxSize, SplitKind::Each]);
        out
    }

    /// Libellé du bouton.
    fn label(self) -> &'static str {
        match self {
            SplitKind::EveryN => tr("Par nombre de pages"),
            SplitKind::Bookmarks => tr("Par signets"),
            SplitKind::MaxSize => tr("Par taille"),
            SplitKind::Each => tr("Une page par fichier"),
        }
    }
}

/// « page 4 », « pages 2 à 5 », ou « pages 1, 3, 7 » quand elles ne se
/// suivent pas.
fn pages_text(pages: &[usize]) -> String {
    match pages {
        [one] => trf("page {}", &[&(one + 1).to_string()]),
        [first, .., last] if pages.windows(2).all(|w| w[1] == w[0] + 1) => trf(
            "pages {} à {}",
            &[&(first + 1).to_string(), &(last + 1).to_string()],
        ),
        _ => {
            let list: Vec<String> = pages.iter().map(|p| (p + 1).to_string()).collect();
            trf("pages {}", &[&list.join(", ")])
        }
    }
}

/// Début des noms d'un fractionnement, tiré du nom choisi dans le dialogue :
/// `rapport-1.pdf` → `rapport` (le « -1 » proposé retombe), `parties.pdf`
/// → `parties`.
fn split_stem(chosen: &Path, fallback: &str) -> String {
    let stem = chosen
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = stem.strip_suffix("-1").unwrap_or(&stem).trim().to_string();
    if stem.is_empty() {
        fallback.to_string()
    } else {
        stem
    }
}

impl Viewer {
    /// Vrai quand le panneau montre les vignettes : leur sélection est
    /// alors visible, et c'est elle que visent les commandes de page.
    pub(super) fn thumbs_shown(&self) -> bool {
        self.panel_open
            && !self.reading
            && self.sign_panel.is_none()
            && self.panel.tab == PanelTab::Thumbnails
    }

    /// Pages que vise une commande de page, dans l'ordre du document : celle
    /// d'un clic droit sur la page, sinon les vignettes sélectionnées si le
    /// panneau les montre, sinon la page courante.
    pub(super) fn target_pages(&self) -> Vec<usize> {
        let Some(l) = &self.loaded else {
            return Vec::new();
        };
        let count = l.pages.len();
        if count == 0 {
            return Vec::new();
        }
        if let Target::Page(page) = self.command_target {
            return vec![page.min(count - 1)];
        }
        if self.thumbs_shown() && !l.page_selection.is_empty() {
            let pages: Vec<usize> = l
                .page_selection
                .sorted()
                .into_iter()
                .filter(|&p| p < count)
                .collect();
            if !pages.is_empty() {
                return pages;
            }
        }
        vec![self.current_page().min(count - 1)]
    }

    /// Nom du document sans extension, pour proposer des noms de fichiers.
    fn document_stem(&self) -> String {
        self.loaded
            .as_ref()
            .and_then(|l| l.path.file_stem())
            .map_or_else(
                || "document".to_string(),
                |s| s.to_string_lossy().into_owned(),
            )
    }

    /// Un clic sur une vignette : aller à la page, ou changer la sélection.
    pub(super) fn select_page(&mut self, page: usize, mode: SelectMode) {
        let current = self.current_page();
        let Some(l) = &mut self.loaded else { return };
        if page >= l.pages.len() {
            return;
        }
        match mode {
            SelectMode::Only => {
                l.page_selection.focus(page);
                self.navigate(|v| v.scroll_to_page(page));
            }
            SelectMode::Toggle => l.page_selection.toggle(page, current),
            SelectMode::Extend => l.page_selection.extend_to(page, current),
        }
        if let Some(l) = &self.loaded {
            log_line(&format!(
                "vignettes : sélection {:?}",
                l.page_selection.sorted()
            ));
        }
    }

    /// Toutes les pages sélectionnées (Ctrl+A dans le panneau), le panneau
    /// ouvert sur les vignettes pour qu'on voie ce qui l'est.
    pub(super) fn select_all_pages(&mut self) {
        if self.loaded.is_none() {
            return;
        }
        if !self.thumbs_shown() {
            self.show_thumbnails();
        }
        if let Some(l) = &mut self.loaded {
            let count = l.pages.len();
            l.page_selection.all(count);
            log_line(&format!("vignettes : les {count} pages sélectionnées"));
        }
    }

    /// Ouvre le panneau sur les vignettes, s'il ne les montre pas.
    fn show_thumbnails(&mut self) {
        if !self.panel_open {
            self.toggle_panel();
        }
        self.panel.tab = PanelTab::Thumbnails;
    }

    /// « Organiser les pages » : le panneau des vignettes s'ouvre et prend
    /// le clavier, sur la page courante. C'est la porte d'entrée de tout le
    /// reste — sélectionner, glisser, pivoter, supprimer.
    pub(super) fn organize_pages(&mut self) {
        if self.loaded.is_none() {
            return;
        }
        self.show_thumbnails();
        self.region = Region::Panel;
        self.leave_region(Region::Panel);
        let current = self.current_page();
        self.panel.focus_page(current);
        let (_, _, sizes) = self.thumb_geometry();
        self.panel
            .reveal_page(current, &sizes, self.dpi_scale as f32);
        self.set_notice(
            tr("Organiser : Ctrl+clic et Maj+clic sélectionnent, un glisser déplace, R pivote, Suppr supprime")
                .into(),
        );
    }

    /// Pivote les pages visées : une seule modification, quel que soit leur
    /// nombre.
    pub(super) fn rotate_targets(&mut self, degrees: i32) {
        let pages = self.target_pages();
        if pages.is_empty() {
            return;
        }
        let count = pages.len();
        log_line(&format!("pages : pivoter {pages:?} de {degrees}°"));
        if self.apply_edit(EditOp::Rotate { pages, degrees }) && count > 1 {
            self.set_notice(trf("{} pages pivotées", &[&count.to_string()]));
        }
    }

    /// Supprime les pages visées, après confirmation. Les pages sont
    /// retenues dans la question : la réponse, qui arrive plus tard,
    /// supprime bien celles qui étaient visées.
    pub(super) fn delete_targets(&mut self) {
        let Some(l) = &self.loaded else { return };
        let count = l.pages.len();
        let pages = self.target_pages();
        if pages.is_empty() {
            return;
        }
        // Tout supprimer est refusé d'emblée, sans question.
        if pages.len() >= count {
            self.alert(
                tr("Suppression impossible"),
                tr("Un document doit garder au moins une page."),
            );
            return;
        }
        // Le droit se vérifie avant de faire confirmer pour rien.
        if !self.require_right(Right::Assemble) {
            return;
        }
        let (title, message) = if let [page] = pages[..] {
            (
                trf("Supprimer la page {} ?", &[&(page + 1).to_string()]),
                tr("La page sera retirée du document. Ctrl+Z la rétablit ; Ctrl+S enregistre.")
                    .to_string(),
            )
        } else {
            (
                trf("Supprimer {} pages ?", &[&pages.len().to_string()]),
                trf(
                    "Les {} seront retirées du document. Ctrl+Z les rétablit ; Ctrl+S enregistre.",
                    &[&pages_text(&pages)],
                ),
            )
        };
        self.confirm(&title, &message, tr("Supprimer"), Then::DeletePages(pages));
    }

    /// La réponse à « Supprimer ? » : les pages retenues partent.
    pub(super) fn delete_pages_now(&mut self, pages: Vec<usize>) {
        let count = pages.len();
        log_line(&format!("pages : supprimer {pages:?}"));
        if self.apply_edit(EditOp::Delete { pages }) {
            self.set_notice(trf(
                "{} page(s) supprimée(s) — Ctrl+Z pour revenir",
                &[&count.to_string()],
            ));
        }
    }

    /// Duplique les pages visées : les copies suivent, ensemble, la
    /// dernière page du bloc, et une sélection de plusieurs pages passe sur
    /// les copies. Chaque copie a ses propres commentaires.
    pub(super) fn duplicate_targets(&mut self) {
        let Some(l) = &self.loaded else { return };
        let count = l.pages.len();
        let pages = self.target_pages();
        let Some(&last) = pages.last() else { return };
        let order = duplicate_block(count, &pages);
        if !self.apply_edit(EditOp::Reorder { order }) {
            return;
        }
        let start = last + 1;
        if let Some(l) = &mut self.loaded {
            if pages.len() > 1 {
                l.page_selection.set_block(start, pages.len());
            }
        }
        self.scroll_to_page(start);
        self.reveal_thumbnail(start);
        self.set_notice(if let [page] = pages[..] {
            trf("page {} dupliquée", &[&(page + 1).to_string()])
        } else {
            trf("{} pages dupliquées", &[&pages.len().to_string()])
        });
    }

    /// Montre la vignette d'une page dans le panneau.
    fn reveal_thumbnail(&mut self, page: usize) {
        let (_, _, sizes) = self.thumb_geometry();
        self.panel.reveal_page(page, &sizes, self.dpi_scale as f32);
    }

    /// Une vignette glissée puis lâchée : la page — ou le bloc sélectionné
    /// qui la contient — va à la position `at` (voir `pages::move_block`).
    /// Déposée à sa propre place, rien ne change et rien n'entre dans
    /// l'historique.
    pub(super) fn drop_pages(&mut self, from: usize, at: usize) {
        let Some(l) = &self.loaded else { return };
        let count = l.pages.len();
        if from >= count {
            return;
        }
        let block = if l.page_selection.len() >= 2 && l.page_selection.contains(from) {
            l.page_selection.sorted()
        } else {
            vec![from]
        };
        let order = move_block(count, &block, at);
        if order.iter().enumerate().all(|(i, &p)| i == p) {
            return;
        }
        log_line(&format!("pages : déplacer {block:?} en {at} → {order:?}"));
        let at = at.min(count);
        let start = at - block.iter().filter(|&&b| b < at).count();
        if !self.apply_edit(EditOp::Reorder { order }) {
            return;
        }
        if let Some(l) = &mut self.loaded {
            if block.len() > 1 {
                l.page_selection.set_block(start, block.len());
            }
        }
        // Pas d'entrée dans l'historique de la vue : on n'a pas sauté
        // ailleurs, on a déplacé ce qu'on regardait.
        self.scroll_to_page(start);
        self.set_notice(if block.len() > 1 {
            trf(
                "{} pages déplacées en position {}",
                &[&block.len().to_string(), &(start + 1).to_string()],
            )
        } else {
            trf("page déplacée en position {}", &[&(start + 1).to_string()])
        });
    }

    /// Insère une page vierge avant la première page visée, ou après la
    /// dernière, au format et à la rotation de cette page voisine.
    pub(super) fn insert_blank(&mut self, after: bool) {
        let pages = self.target_pages();
        let Some(&neighbour) = (if after { pages.last() } else { pages.first() }) else {
            return;
        };
        let Some(l) = &self.loaded else { return };
        let Some(page) = l.pages.get(neighbour) else {
            return;
        };
        let (media, rotate) = (page.media_box(&l.doc), page.rotate(&l.doc));
        let at = if after { neighbour + 1 } else { neighbour };
        if !self.apply_edit(EditOp::InsertBlank { at, media, rotate }) {
            return;
        }
        // La page neuve est montrée ; une sélection explicite passe sur elle.
        if let Some(l) = &mut self.loaded {
            if !l.page_selection.is_empty() {
                l.page_selection.set_block(at, 1);
            }
        }
        self.scroll_to_page(at);
        self.reveal_thumbnail(at);
        self.set_notice(trf(
            "page vierge insérée en position {} — Ctrl+Z pour revenir",
            &[&(at + 1).to_string()],
        ));
    }

    /// Mot de passe d'un fichier déjà ouvert dans un onglet : c'est ainsi
    /// qu'un fichier protégé devient une source d'insertion ou de
    /// remplacement.
    pub(super) fn password_of_open(&self, path: &Path) -> Option<Vec<u8>> {
        self.loaded
            .iter()
            .chain(self.others.iter())
            .find(|l| l.path == path)
            .and_then(|l| l.password.clone())
    }

    /// « Remplacer des pages… » : les pages visées prennent le contenu des
    /// premières pages d'un autre fichier. Le fichier est lu ici, pour
    /// refuser tout de suite — et le dire — ce qui échouerait plus tard :
    /// un mot de passe inconnu, des permissions qui interdisent la copie,
    /// trop peu de pages.
    pub(super) fn replace_pages_command(&mut self, window: &mut dyn WindowHandle) {
        if self.loaded.is_none() || !self.require_right(Right::Assemble) {
            return;
        }
        let targets = self.target_pages();
        if targets.is_empty() {
            return;
        }
        let Some(path) = window.open_file_dialog() else {
            return;
        };
        let password = self.password_of_open(&path);
        let title = tr("Remplacement impossible");
        let src = match Document::load(&path) {
            Ok(d) => d,
            Err(e) => {
                self.alert(title, &format!("{}\n\n{e}", path.display()));
                return;
            }
        };
        if let Some(pw) = &password {
            let _ = src.authenticate(pw);
        }
        if src.needs_password() {
            self.alert(
                title,
                tr("Ce fichier est protégé par un mot de passe : ouvrez-le d'abord dans un onglet pour le saisir, puis recommencez."),
            );
            return;
        }
        let restricted = src.security().is_some_and(|h| {
            !h.is_owner() && !acrux_document::protect::Permissions::from_p(h.permissions()).copy
        });
        if restricted {
            self.alert(
                title,
                tr("Les permissions de ce fichier interdisent d'en extraire les pages."),
            );
            return;
        }
        let available = collect_pages(&src).map_or(0, |p| p.len());
        if available < targets.len() {
            self.alert(
                title,
                &trf(
                    "Le fichier choisi n'a que {} page(s) pour {} page(s) à remplacer.",
                    &[&available.to_string(), &targets.len().to_string()],
                ),
            );
            return;
        }
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let (question, message) = if let [page] = targets[..] {
            (
                trf("Remplacer la page {} ?", &[&(page + 1).to_string()]),
                trf(
                    "La page {} prend le contenu de la première page de « {} ». Ses signets, liens et commentaires restent ; Ctrl+Z annule.",
                    &[&(page + 1).to_string(), &name],
                ),
            )
        } else {
            (
                trf("Remplacer {} pages ?", &[&targets.len().to_string()]),
                trf(
                    "Les {} prennent le contenu des {} premières pages de « {} ». Leurs signets, liens et commentaires restent ; Ctrl+Z annule.",
                    &[&pages_text(&targets), &targets.len().to_string(), &name],
                ),
            )
        };
        let pairs: Vec<(usize, usize)> = targets.into_iter().zip(0..).collect();
        self.confirm(
            &question,
            &message,
            tr("Remplacer"),
            Then::ReplacePages {
                path,
                password,
                pairs,
            },
        );
    }

    /// La réponse à « Remplacer ? ».
    pub(super) fn replace_pages_now(
        &mut self,
        path: PathBuf,
        password: Option<Vec<u8>>,
        pairs: Vec<(usize, usize)>,
    ) {
        let count = pairs.len();
        if self.apply_edit(EditOp::Replace {
            path,
            password,
            pairs,
        }) {
            self.set_notice(trf(
                "{} page(s) remplacée(s) — Ctrl+Z pour revenir",
                &[&count.to_string()],
            ));
        }
    }

    /// Refuse, avec l'offre du mot de passe des permissions, ce qui revient
    /// à extraire le contenu d'un document qui l'interdit. Vrai si l'on
    /// peut continuer.
    fn may_extract(&mut self) -> bool {
        if self.loaded.is_none() {
            return false;
        }
        // Les pages écrites ailleurs le sont en clair, sans les permissions
        // du document : c'est en extraire le contenu, que la permission de
        // copie refuse (Acrobat lie de même l'extraction à la copie).
        if self.rights().copy {
            return true;
        }
        self.refuse(
            tr("Extraction interdite"),
            tr("Les permissions de ce document interdisent d'en extraire le contenu. Le mot de passe des permissions lève cette restriction."),
        );
        false
    }

    /// « Extraire » : une page s'écrit dans un fichier ; plusieurs, au
    /// choix, dans un seul fichier ou un fichier par page.
    pub(super) fn extract_targets(&mut self, window: &mut dyn WindowHandle) {
        if !self.may_extract() {
            return;
        }
        let pages = self.target_pages();
        if pages.len() == 1 {
            self.extract_to_file(&pages, window);
            return;
        }
        if pages.is_empty() {
            return;
        }
        self.push_choice(
            &trf("Extraire {} pages", &[&pages.len().to_string()]),
            &trf(
                "Les {} s'écrivent dans un nouveau fichier, ou chacune dans le sien. Le document ne change pas.",
                &[&pages_text(&pages)],
            ),
            &[
                tr("Dans un seul fichier"),
                tr("Un fichier par page"),
                tr("Annuler"),
            ],
            Then::Extract(pages),
        );
    }

    /// La réponse à « Extraire » : le rang du bouton choisi.
    pub(super) fn extract_choice(
        &mut self,
        pages: &[usize],
        index: usize,
        window: &mut dyn WindowHandle,
    ) {
        match index {
            0 => self.extract_to_file(pages, window),
            1 => {
                let plan = SplitPlan::Groups(pages.iter().map(|&p| vec![p]).collect());
                self.start_split(plan, window);
            }
            _ => {}
        }
    }

    /// Écrit les pages dans un seul nouveau fichier, choisi dans le
    /// dialogue d'enregistrement. C'est rapide : le fil principal s'en
    /// charge.
    fn extract_to_file(&mut self, pages: &[usize], window: &mut dyn WindowHandle) {
        let (Some(&first), Some(&last)) = (pages.first(), pages.last()) else {
            return;
        };
        let stem = self.document_stem();
        let suggested = if first == last {
            format!("{stem}-p{}.pdf", first + 1)
        } else {
            format!("{stem}-p{}-{}.pdf", first + 1, last + 1)
        };
        let Some(target) = window.save_file_dialog(&suggested) else {
            return;
        };
        let Some(l) = &self.loaded else { return };
        let options = acrux_document::SaveOptions {
            drop_unreferenced: true,
            ..acrux_document::SaveOptions::default()
        };
        let result = acrux_features::pages::extract_pages(&l.doc, pages)
            .and_then(|d| d.save_full_with(&options))
            .and_then(|bytes| {
                std::fs::write(&target, bytes)
                    .map_err(|e| acrux_core::Error::Io(format!("{}: {e}", target.display())))
            });
        match result {
            Ok(()) => self.set_notice(trf(
                "{} écrite(s) dans {}",
                &[&pages_text(pages), &target.display().to_string()],
            )),
            Err(e) => self.alert(tr("Extraction impossible"), &e.to_string()),
        }
    }

    /// Vrai si le document a des signets de premier niveau qui mènent à une
    /// page : sans eux, le découpage « par signets » n'est pas proposé.
    fn has_top_bookmarks(&self) -> bool {
        self.loaded.as_ref().is_some_and(|l| {
            l.outline_rows
                .iter()
                .zip(&l.outline_actions)
                .any(|(row, action)| row.depth == 0 && matches!(action, Some(Action::GoTo(_))))
        })
    }

    /// « Fractionner le document… » : la question des découpages.
    pub(super) fn split_document(&mut self) {
        if !self.may_extract() {
            return;
        }
        let with_bookmarks = self.has_top_bookmarks();
        let mut labels: Vec<&str> = SplitKind::choices(with_bookmarks)
            .into_iter()
            .map(SplitKind::label)
            .collect();
        labels.push(tr("Annuler"));
        let modified = self.loaded.as_ref().is_some_and(|l| l.modified);
        let mut message =
            tr("Chaque partie devient un fichier PDF, dans le dossier que vous choisirez ensuite.")
                .to_string();
        if modified {
            message.push(' ');
            message.push_str(tr(
                "Le document est fractionné tel qu'il s'affiche, modifications comprises.",
            ));
        }
        self.push_choice(
            tr("Fractionner le document"),
            &message,
            &labels,
            Then::Split(with_bookmarks),
        );
    }

    /// La réponse à « Fractionner » : le découpage choisi, puis ce qu'il
    /// faut encore demander.
    pub(super) fn split_choice(&mut self, kind: SplitKind, window: &mut dyn WindowHandle) {
        match kind {
            SplitKind::EveryN => {
                let mut input = TextInput::new("2");
                input.set_value("2");
                input.select_all();
                self.prompt = Some(Prompt::new(
                    tr("Fractionner par nombre de pages"),
                    tr("Nombre de pages par fichier :").into(),
                    input,
                    PromptKind::SplitEvery,
                ));
            }
            SplitKind::MaxSize => {
                let mut input = TextInput::new("2");
                input.set_value("2");
                input.select_all();
                self.prompt = Some(Prompt::new(
                    tr("Fractionner par taille"),
                    tr("Taille maximale de chaque fichier, en Mo (ou « 500 Ko ») :").into(),
                    input,
                    PromptKind::SplitSize,
                ));
            }
            SplitKind::Bookmarks => self.start_split(SplitPlan::TopBookmarks, window),
            SplitKind::Each => self.start_split(SplitPlan::EveryN(1), window),
        }
    }

    /// L'invite du nombre de pages validée. Rend l'erreur à afficher dans
    /// l'invite, si la saisie ne convient pas.
    pub(super) fn split_every_entered(
        &mut self,
        value: &str,
        window: &mut dyn WindowHandle,
    ) -> Option<String> {
        match value.trim().parse::<usize>() {
            Ok(n) if n >= 1 => {
                self.prompt = None;
                self.start_split(SplitPlan::EveryN(n), window);
                None
            }
            _ => Some(tr("Un nombre entier de pages, 1 ou plus.").into()),
        }
    }

    /// L'invite de la taille validée : des mégaoctets, ou une taille avec
    /// son unité.
    pub(super) fn split_size_entered(
        &mut self,
        value: &str,
        window: &mut dyn WindowHandle,
    ) -> Option<String> {
        if let Some(bytes) = parse_size(value, MIB) {
            self.prompt = None;
            self.start_split(SplitPlan::MaxBytes(bytes), window);
            None
        } else {
            Some(tr("Une taille comme « 2 », « 2,5 » (Mo) ou « 500 Ko ».").into())
        }
    }

    /// Choisit le dossier de sortie (le dialogue d'enregistrement, qui
    /// propose `rapport-1.pdf`) puis confie le fractionnement au fil de
    /// rendu. Sans fil de rendu, il se fait ici, tout de suite.
    pub(super) fn start_split(&mut self, plan: SplitPlan, window: &mut dyn WindowHandle) {
        let stem = self.document_stem();
        let Some(chosen) = window.save_file_dialog(&format!("{stem}-1.pdf")) else {
            return;
        };
        let dir = chosen
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let name = split_stem(&chosen, &stem);
        log_line(&format!(
            "fractionnement : {plan:?} vers {} ({name})",
            dir.display()
        ));
        let Some(l) = &self.loaded else { return };
        if let Some(worker) = &l.worker {
            worker.split(dir.clone(), name, plan);
            self.set_notice(trf(
                "fractionnement en cours vers {}…",
                &[&dir.display().to_string()],
            ));
            return;
        }
        let notice = match crate::render_worker::run_split(&l.doc, &dir, &name, &plan) {
            Ok(message) => message,
            Err(e) => trf("fractionnement impossible : {}", &[&e.to_string()]),
        };
        self.set_notice(notice);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_signets_ne_sont_proposes_que_s_il_y_en_a() {
        assert_eq!(
            SplitKind::choices(true),
            [
                SplitKind::EveryN,
                SplitKind::Bookmarks,
                SplitKind::MaxSize,
                SplitKind::Each
            ]
        );
        assert_eq!(
            SplitKind::choices(false),
            [SplitKind::EveryN, SplitKind::MaxSize, SplitKind::Each],
            "le rang des boutons suit la liste"
        );
    }

    #[test]
    fn le_nom_choisi_donne_le_debut_des_noms() {
        assert_eq!(
            split_stem(Path::new("C:/x/rapport-1.pdf"), "doc"),
            "rapport"
        );
        assert_eq!(split_stem(Path::new("parties.pdf"), "doc"), "parties");
        assert_eq!(split_stem(Path::new("-1.pdf"), "doc"), "doc");
    }
}
