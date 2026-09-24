//! Menus du clic droit : ce que proposent la page, une vignette du panneau,
//! un onglet et un document récent de l'accueil.
//!
//! Chaque élément lance une commande de la palette ([`Command`]) — celle-là
//! même qu'atteignent le raccourci et la colonne d'outils : le menu ne fait
//! rien de plus ni rien de moins, et le raccourci qu'il affiche vient de la
//! même source (`palette::describe`). Ce qui change, c'est la **cible** :
//! pivoter depuis une vignette pivote cette page-là, pas celle qu'on
//! regarde ; fermer depuis un onglet ferme cet onglet-là. La cible est posée
//! le temps d'une commande ([`Viewer::run_on`]), puis revient à « ce qui est
//! à l'écran ».
//!
//! Le bouton droit est aussi un geste à ne pas laisser traîner : son
//! relâchement n'atteint jamais l'outil en cours. Avant ce module, lâcher le
//! bouton droit pendant le tracé d'une signature, un trait d'encre ou le
//! déplacement d'un objet terminait le geste, comme si l'on avait lâché le
//! gauche.

use std::path::PathBuf;

use acrux_document::protect::Permissions;
use acrux_features::docinfo::{read_metadata, Date, Field};

use super::{log_line, Region, Viewer};
use crate::platform::{Cursor, Event, Frame, Key, MouseButton, WindowHandle};
use crate::ui::icons::Icon;
use crate::ui::lang::{self, tr, trf};
use crate::ui::menu::{Menu, Outcome};
use crate::ui::palette::{self, Command};
use crate::ui::panel::PanelTab;
use crate::ui::toolbar::Toolbar;

/// Ce que vise une commande lancée depuis un menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Target {
    /// Ce qui est à l'écran : la page courante, l'onglet actif. C'est la
    /// cible de la palette, des raccourcis et des boutons.
    #[default]
    Current,
    /// Une page précise (clic droit sur la page ou sur sa vignette).
    Page(usize),
    /// Un onglet précis, par son rang dans la barre.
    Tab(usize),
    /// Un document récent de l'accueil, par son rang dans la liste.
    Recent(usize),
}

/// Ce que rend un élément de menu : la commande et sa cible.
type Action = (Command, Target);

/// Un menu prêt à s'ouvrir : le menu, son nom pour le journal, et le point
/// du clic droit dans la vue.
type Opened = (Menu<Action>, &'static str, Option<(i32, i32)>);

/// Le menu ouvert.
pub(super) struct ContextMenu {
    menu: Menu<Action>,
    /// Point du clic droit dans la vue : c'est là que « Poser une note ici »
    /// pose la note, même si le pointeur est allé ensuite sur le menu.
    origin_view: Option<(i32, i32)>,
}

/// Raccourci d'une commande, tel que la palette l'écrit.
fn shortcut(command: Command) -> &'static str {
    palette::describe(command).map_or("", |(_, keys)| keys)
}

impl Viewer {
    /// Lance une commande sur une cible, puis revient à la cible ordinaire :
    /// la suivante, lancée au clavier, visera de nouveau ce qui est à
    /// l'écran.
    pub(super) fn run_on(
        &mut self,
        command: Command,
        target: Target,
        window: &mut dyn WindowHandle,
    ) {
        log_line(&format!("menu : {command:?} sur {target:?}"));
        self.command_target = target;
        self.run_command(command, window);
        self.command_target = Target::Current;
    }

    /// Page visée par la commande en cours : celle du clic droit, sinon la
    /// page courante.
    pub(super) fn target_page(&self) -> usize {
        match self.command_target {
            Target::Page(page) => {
                let count = self.loaded.as_ref().map_or(0, |l| l.pages.len());
                page.min(count.saturating_sub(1))
            }
            _ => self.current_page(),
        }
    }

    /// Onglet visé par la commande en cours : celui du clic droit, sinon
    /// l'onglet actif.
    pub(super) fn target_tab(&self) -> usize {
        match self.command_target {
            Target::Tab(index) => index,
            _ => self.active_tab,
        }
    }

    /// Fichier visé : celui de l'onglet ou du document récent du clic droit,
    /// sinon celui du document actif. `None` pour un document qui n'a pas
    /// encore de fichier à lui (une image ouverte, pas encore enregistrée) :
    /// son chemin serait celui d'un fichier temporaire.
    pub(super) fn target_path(&self) -> Option<PathBuf> {
        let file = |l: &super::Loaded| (!l.temporary).then(|| l.path.clone());
        match self.command_target {
            Target::Recent(index) => self.prefs.recent.get(index).cloned(),
            Target::Tab(index) => self.tab_doc(index).and_then(file),
            Target::Current | Target::Page(_) => self.loaded.as_ref().and_then(file),
        }
    }

    /// Vrai quand une carte modale, la palette ou une liste déroulante a la
    /// main : le menu ne s'ouvre pas, et se referme s'il l'était.
    pub(super) fn menu_blocked(&self) -> bool {
        self.palette.is_some()
            || self.prompt.is_some()
            || self.settings.is_some()
            || self.protect.is_some()
            || !self.dialogs.is_empty()
            || self.capture.is_some()
            || self.edit_menu_open()
            || self.zoom_menu.is_some()
    }

    /// Vrai pendant un geste du bouton gauche ou du milieu : un clic droit
    /// au beau milieu ne l'interrompt pas, et n'ouvre rien.
    fn gesture_in_progress(&self) -> bool {
        self.sel_dragging
            || self.carrying
            || self.inking.is_some()
            || self.drag_last.is_some()
            || self.panel.dragging()
            || self.placed.as_ref().is_some_and(|p| p.drag.is_some())
            || self.objects.as_ref().is_some_and(|t| t.drag.is_some())
    }

    /// Donne l'événement au menu du clic droit. Rend vrai s'il l'a pris.
    ///
    /// Menu ouvert, il prend **tout** — comme un menu de Windows : un clic
    /// dehors le referme sans atteindre ce qui est dessous, la molette le
    /// referme sans rien faire défiler. Seuls un raccourci Ctrl (qui le
    /// referme puis agit), le redimensionnement, un fichier déposé et la
    /// fermeture de la fenêtre suivent leur cours.
    ///
    /// Menu fermé, il prend le clic droit, pour s'ouvrir, et **tout**
    /// relâchement du bouton droit : aucun outil ne doit le confondre avec
    /// celui du gauche.
    pub(super) fn context_menu_event(
        &mut self,
        event: &Event,
        window: &mut dyn WindowHandle,
    ) -> bool {
        let Some(open) = &mut self.context_menu else {
            return self.closed_menu_event(event, window);
        };
        let outcome = match *event {
            Event::MouseDown { button, x, y, .. } => {
                if button == MouseButton::Middle {
                    if open.menu.contains(x, y) {
                        Outcome::Stay
                    } else {
                        Outcome::Close
                    }
                } else {
                    let outcome = open.menu.mouse_down(x, y);
                    // Un clic droit ailleurs rouvre le menu à ce nouvel
                    // endroit, sur ce qui s'y trouve.
                    if outcome == Outcome::Close && button == MouseButton::Right {
                        self.close_context_menu(window);
                        self.open_context_menu(x, y, window);
                        return true;
                    }
                    outcome
                }
            }
            Event::MouseUp { button, x, y } => {
                if button == MouseButton::Middle {
                    Outcome::Stay
                } else {
                    open.menu.mouse_up(x, y)
                }
            }
            Event::MouseMove { x, y, .. } => {
                if open.menu.mouse_move(x, y) {
                    window.request_redraw();
                }
                window.set_cursor(Cursor::Arrow);
                return true;
            }
            Event::Wheel { .. } => Outcome::Close,
            // Maj+F10, comme la touche « menu », referme ce qu'elle a ouvert.
            Event::Key(Key::F(10), m) if m.shift => Outcome::Close,
            Event::Key(key, _) => {
                window.request_redraw();
                open.menu.key(key)
            }
            Event::Char(_, m) if m.ctrl => {
                self.close_context_menu(window);
                return false;
            }
            Event::Char(c, _) => {
                window.request_redraw();
                open.menu.char(c)
            }
            Event::Resize { .. } | Event::DpiChanged(_) | Event::FileDropped(_) | Event::Close => {
                self.context_menu = None;
                return false;
            }
            Event::Wake => return false,
        };
        self.menu_outcome(outcome, window);
        true
    }

    /// Le clic droit et la touche « menu », quand aucun menu n'est ouvert.
    fn closed_menu_event(&mut self, event: &Event, window: &mut dyn WindowHandle) -> bool {
        match *event {
            Event::MouseDown {
                button: MouseButton::Right,
                x,
                y,
                ..
            } => {
                self.open_context_menu(x, y, window);
                true
            }
            Event::MouseUp {
                button: MouseButton::Right,
                ..
            } => true,
            Event::Key(Key::ContextMenu, _) => self.open_keyboard_menu(window),
            Event::Key(Key::F(10), m) if m.shift => self.open_keyboard_menu(window),
            _ => false,
        }
    }

    /// Exécute ce que le menu a répondu.
    fn menu_outcome(&mut self, outcome: Outcome<Action>, window: &mut dyn WindowHandle) {
        match outcome {
            Outcome::Pick((command, target)) | Outcome::Live((command, target)) => {
                let origin = self.context_menu.take().and_then(|open| open.origin_view);
                window.request_redraw();
                // La note se pose au point du clic droit, pas sous
                // l'élément du menu où le pointeur est allé la demander.
                if command == Command::Note {
                    if let Some(point) = origin {
                        self.last_mouse = Some(point);
                    }
                }
                self.run_on(command, target, window);
            }
            Outcome::Close => self.close_context_menu(window),
            Outcome::Stay => {}
        }
    }

    /// Referme le menu sans rien faire.
    fn close_context_menu(&mut self, window: &mut dyn WindowHandle) {
        if self.context_menu.take().is_some() {
            log_line("menu : fermé");
            window.request_redraw();
        }
    }

    /// Ouvre le menu de ce qui se trouve au point `(x, y)` de la fenêtre ;
    /// faux s'il n'y a rien à proposer là.
    ///
    /// La découpe de la fenêtre est celle du clic gauche : barre d'outils,
    /// onglets, barres de mode, colonne d'outils, panneau, puis la vue.
    fn open_context_menu(&mut self, x: i32, y: i32, window: &mut dyn WindowHandle) -> bool {
        self.context_menu = None;
        if self.menu_blocked() {
            return false;
        }
        if self.gesture_in_progress() {
            log_line("menu : refusé, un geste est en cours");
            return false;
        }
        let Some((menu, kind, origin_view)) = self.menu_at(x, y) else {
            return false;
        };
        self.show_menu(menu, kind, origin_view, window)
    }

    /// Le menu qui convient au point `(x, y)` de la fenêtre, son nom (pour
    /// le journal) et le point correspondant dans la vue.
    fn menu_at(&self, x: i32, y: i32) -> Option<Opened> {
        let top = self.view_top() as i32;
        let left = self.view_left() as i32;
        if !self.fullscreen && !self.reading {
            let bar = Toolbar::height(&self.theme, self.dpi_scale as f32);
            if y < bar {
                return None;
            }
            if y < bar + self.tabs_height() as i32 {
                let tab = self.tabs.tab_at(x, y - bar)?;
                return Some((self.tab_menu(x, y, tab), "onglet", None));
            }
        }
        // Barres de mode au-dessus de la vue, barre d'état en dessous,
        // colonne d'outils à droite : rien à proposer.
        if y < top || y >= top + self.view_height() as i32 {
            return None;
        }
        if self.tools_width() > 0 && x >= self.width.saturating_sub(self.tools_width()) as i32 {
            return None;
        }
        if x < left {
            if self.sign_panel_open() || !self.panel_open || self.panel.tab != PanelTab::Thumbnails
            {
                return None;
            }
            let page = self.panel.page_at(x, y - top)?;
            return Some((self.thumb_menu(x, y, page), "vignette", None));
        }
        let (vx, vy) = (x - left, y - top);
        if self.showing_home() {
            let index = self.recent_at(vx, vy)?;
            return Some((self.recent_menu(x, y, index), "document récent", None));
        }
        // Les modes qui ont leurs propres gestes sur la page gardent la main.
        if !self.page_menu_allowed() || self.media_at(vx, vy).is_some() {
            return None;
        }
        let (page, _) = self.page_at(vx, vy)?;
        Some((self.page_menu(x, y, page), "page", Some((vx, vy))))
    }

    /// Vrai si la page peut offrir son menu : pas en « Modifier le PDF », ni
    /// avec l'outil des objets, « remplir et signer » ou un modèle 3D actif,
    /// qui ont leurs propres gestes et, bientôt, leurs propres menus.
    fn page_menu_allowed(&self) -> bool {
        self.loaded.is_some()
            && !self.edit_on()
            && self.objects.is_none()
            && self.sign_panel.is_none()
            && self.three_d.is_none()
    }

    /// Ouvre le menu au clavier (touche « menu » ou Maj+F10) : celui de la
    /// vignette qui a le focus dans le panneau, sinon celui de la page sous
    /// le pointeur, ou au milieu de la vue. Le premier élément est déjà mis
    /// en avant, prêt pour Entrée.
    fn open_keyboard_menu(&mut self, window: &mut dyn WindowHandle) -> bool {
        self.context_menu = None;
        if self.menu_blocked() || self.gesture_in_progress() {
            return false;
        }
        let top = self.view_top() as i32;
        let left = self.view_left() as i32;
        if self.region == Region::Panel {
            let page = self
                .panel
                .focused_page()
                .filter(|_| self.panel_open && !self.sign_panel_open());
            let Some(page) = page else {
                return false;
            };
            let (rx, ry, rw, rh) = self.panel.page_rect(page).unwrap_or((0, 0, left, 0));
            let mut menu = self.thumb_menu(rx + rw / 2, top + ry + rh / 2, page);
            menu.select_first();
            return self.show_menu(menu, "vignette (clavier)", None, window);
        }
        let (vw, vh) = (self.view_width() as i32, self.view_height() as i32);
        let pointer = self
            .last_mouse
            .filter(|&(x, y)| x >= 0 && y >= 0 && x < vw && y < vh);
        if self.showing_home() {
            let Some((index, (vx, vy))) =
                pointer.and_then(|(vx, vy)| self.recent_at(vx, vy).map(|i| (i, (vx, vy))))
            else {
                return false;
            };
            let mut menu = self.recent_menu(vx + left, vy + top, index);
            menu.select_first();
            return self.show_menu(menu, "document récent (clavier)", None, window);
        }
        if !self.page_menu_allowed() {
            return false;
        }
        let (vx, vy) = pointer.unwrap_or((vw / 2, vh / 3));
        let Some((page, _)) = self.page_at(vx, vy) else {
            return false;
        };
        let mut menu = self.page_menu(vx + left, vy + top, page);
        menu.select_first();
        self.show_menu(menu, "page (clavier)", Some((vx, vy)), window)
    }

    /// Met le menu en place et l'affiche.
    fn show_menu(
        &mut self,
        mut menu: Menu<Action>,
        kind: &str,
        origin_view: Option<(i32, i32)>,
        window: &mut dyn WindowHandle,
    ) -> bool {
        if menu.is_empty() {
            return false;
        }
        // La géométrie est calculée tout de suite : un clic qui arriverait
        // avant la première peinture trouve déjà la carte à sa place.
        let (font, dpi) = (self.theme.font_size, self.dpi_scale as f32);
        let (w, h) = (self.width as i32, self.height as i32);
        match self.text.as_mut() {
            Some(text) => menu.layout(&mut |s: f32, t: &str| text.measure(s, t), font, dpi, w, h),
            None => menu.layout(
                &mut |s: f32, t: &str| t.chars().count() as f32 * s * 0.55,
                font,
                dpi,
                w,
                h,
            ),
        }
        let (x, y) = menu.origin();
        log_line(&format!("menu : {kind} ({x}, {y})"));
        // L'info-bulle d'un bouton ou d'un document récent céderait sinon
        // sa place au menu une demi-seconde plus tard, par-dessus lui.
        self.tip = None;
        self.context_menu = Some(ContextMenu { menu, origin_view });
        window.set_cursor(Cursor::Arrow);
        window.request_redraw();
        true
    }

    /// Le menu d'une page : ce qu'on fait d'une sélection, d'un endroit,
    /// de la page et du document.
    fn page_menu(&self, x: i32, y: i32, page: usize) -> Menu<Action> {
        let selected = self.selection.is_some_and(|s| !s.is_empty());
        let rights = self.rights();
        let here = Target::Page(page);
        let now = Target::Current;
        Menu::new(x, y)
            .item(
                None,
                tr("Copier"),
                shortcut(Command::Copy),
                (Command::Copy, now),
                selected && rights.copy,
            )
            .item(
                Some(Icon::Highlight),
                tr("Surligner la sélection"),
                shortcut(Command::Highlight),
                (Command::Highlight, now),
                selected,
            )
            .item(
                Some(Icon::Note),
                tr("Poser une note ici"),
                shortcut(Command::Note),
                (Command::Note, here),
                true,
            )
            .separator()
            .item(
                None,
                tr("Tout sélectionner"),
                shortcut(Command::SelectAll),
                (Command::SelectAll, now),
                true,
            )
            .item(
                Some(Icon::Rotate),
                tr("Pivoter la page"),
                shortcut(Command::RotateRight),
                (Command::RotateRight, here),
                true,
            )
            .separator()
            .item(
                Some(Icon::Print),
                tr("Imprimer…"),
                shortcut(Command::Print),
                (Command::Print, now),
                rights.print,
            )
            .item(
                None,
                tr("Propriétés du document"),
                shortcut(Command::Properties),
                (Command::Properties, now),
                true,
            )
    }

    /// Le menu d'une vignette : la page qu'elle montre, et non la page
    /// courante. Le clic droit ne change pas la page affichée. Pas de
    /// raccourci affiché : ceux du clavier visent la page courante, qui
    /// peut être une autre.
    fn thumb_menu(&self, x: i32, y: i32, page: usize) -> Menu<Action> {
        let count = self.loaded.as_ref().map_or(0, |l| l.pages.len());
        let here = Target::Page(page);
        Menu::new(x, y)
            .item(
                Some(Icon::Rotate),
                tr("Pivoter"),
                "",
                (Command::RotateRight, here),
                true,
            )
            .item(
                Some(Icon::PageDuplicate),
                tr("Dupliquer"),
                "",
                (Command::DuplicatePage, here),
                true,
            )
            .item(
                Some(Icon::PageExtract),
                tr("Extraire…"),
                "",
                (Command::ExtractPage, here),
                true,
            )
            .separator()
            .item(
                Some(Icon::PageDelete),
                tr("Supprimer"),
                "",
                (Command::DeletePage, here),
                count > 1,
            )
    }

    /// Le menu d'un onglet.
    fn tab_menu(&self, x: i32, y: i32, tab: usize) -> Menu<Action> {
        let on = Target::Tab(tab);
        let has_file = self.tab_doc(tab).is_some_and(|l| !l.temporary);
        // Ctrl+W ferme l'onglet actif : il ne se lit qu'en face de lui.
        let close_keys = if tab == self.active_tab {
            shortcut(Command::CloseTab)
        } else {
            ""
        };
        Menu::new(x, y)
            .item(
                None,
                tr("Fermer"),
                close_keys,
                (Command::CloseTab, on),
                true,
            )
            .item(
                None,
                tr("Fermer les autres onglets"),
                "",
                (Command::CloseOtherTabs, on),
                self.tab_count() > 1,
            )
            .separator()
            .item(
                None,
                tr("Copier le chemin"),
                "",
                (Command::CopyPath, on),
                has_file,
            )
            .item(
                Some(Icon::Open),
                tr("Ouvrir le dossier du fichier"),
                "",
                (Command::RevealInFolder, on),
                has_file,
            )
    }

    /// Le menu d'un document récent de l'accueil.
    fn recent_menu(&self, x: i32, y: i32, index: usize) -> Menu<Action> {
        let on = Target::Recent(index);
        // Le disque n'est consulté qu'ici, à l'ouverture du menu, jamais
        // pendant la peinture. Un fichier disparu laisse voir son dossier
        // s'il existe encore.
        let reachable = self
            .prefs
            .recent
            .get(index)
            .is_some_and(|p| p.exists() || p.parent().is_some_and(std::path::Path::is_dir));
        Menu::new(x, y)
            .item(
                Some(Icon::Open),
                tr("Ouvrir"),
                "",
                (Command::Open, on),
                true,
            )
            .separator()
            .item(
                None,
                tr("Copier le chemin"),
                "",
                (Command::CopyPath, on),
                true,
            )
            .item(
                None,
                tr("Ouvrir le dossier du fichier"),
                "",
                (Command::RevealInFolder, on),
                reachable,
            )
            .separator()
            .item(
                None,
                tr("Retirer de la liste"),
                "",
                (Command::ForgetRecent, on),
                true,
            )
    }

    /// Dessine le menu ouvert, par-dessus la palette et sous les questions.
    pub(super) fn paint_context_menu(&mut self, frame: &mut Frame<'_>) {
        if let (Some(open), Some(text)) = (&self.context_menu, self.text.as_mut()) {
            open.menu.paint(frame, text, &mut self.raster, &self.theme);
        }
    }

    // --- Les commandes que les menus ont apportées -------------------------

    /// Ferme tous les onglets sauf celui visé. Ceux qui ont des
    /// modifications non enregistrées **restent ouverts**, et une notice le
    /// dit : fermer d'un geste plusieurs documents modifiés, c'est risquer
    /// d'en perdre un sans l'avoir vu.
    pub(super) fn close_other_tabs(&mut self) {
        let count = self.tab_count();
        let keep = self.target_tab();
        if count < 2 || keep >= count {
            return;
        }
        // Ce qui est tapé mais pas encore écrit compte comme une
        // modification : on l'écrit avant d'en juger.
        self.close_active();
        let modified: Vec<bool> = (0..count)
            .map(|i| self.tab_doc(i).is_some_and(|l| l.modified))
            .collect();
        let plan = close_plan(count, keep, &modified);
        for &index in &plan {
            self.close_tab_now(index);
        }
        let kept = keep - plan.iter().filter(|&&i| i < keep).count();
        if kept != self.active_tab {
            self.select_tab(kept);
        }
        let left_open = count - 1 - plan.len();
        if left_open > 0 {
            self.set_notice(trf(
                "{} onglet(s) modifié(s) laissé(s) ouvert(s) : enregistrez-les d'abord",
                &[&left_open.to_string()],
            ));
        } else {
            self.set_notice(trf("{} onglet(s) fermé(s)", &[&plan.len().to_string()]));
        }
    }

    /// Copie le chemin du fichier visé dans le presse-papiers.
    pub(super) fn copy_path(&mut self, window: &mut dyn WindowHandle) {
        let Some(path) = self.target_path() else {
            self.set_notice(
                tr("ce document n'a pas encore de fichier : enregistrez-le d'abord").into(),
            );
            return;
        };
        window.set_clipboard_text(&path.display().to_string());
        self.set_notice(tr("Chemin copié").into());
    }

    /// Montre le fichier visé dans l'Explorateur.
    pub(super) fn show_in_folder(&mut self, window: &mut dyn WindowHandle) {
        let Some(path) = self.target_path() else {
            self.set_notice(
                tr("ce document n'a pas encore de fichier : enregistrez-le d'abord").into(),
            );
            return;
        };
        log_line(&format!("dossier montré : {}", path.display()));
        window.reveal_in_folder(&path);
    }

    /// Retire un document de la liste des récents — la liste seulement :
    /// le fichier n'est pas touché.
    pub(super) fn forget_recent(&mut self, index: usize) {
        if index >= self.prefs.recent.len() {
            return;
        }
        let path = self.prefs.recent.remove(index);
        self.welcome_thumbs.remove(&path);
        self.prefs.save();
        // Les cartes se redessinent à la peinture suivante ; d'ici là, les
        // anciennes zones viseraient le document d'à côté.
        self.recent_hits.clear();
        self.tip = None;
        self.set_notice(tr("Retiré de la liste").into());
    }

    /// Affiche les propriétés du document : le fichier, ses pages, ses
    /// métadonnées et sa protection — ce que montre `acr metadata`, dans une
    /// fenêtre d'information.
    pub(super) fn show_properties(&mut self) {
        let Some(l) = &self.loaded else { return };
        let english = lang::english();
        let line = |label: &'static str, value: &str| field_line(tr(label), value, english);
        let mut file = Vec::new();
        if !l.temporary {
            if let Some(name) = l.path.file_name() {
                file.push(line("Fichier", &name.to_string_lossy()));
            }
            if let Some(dir) = l.path.parent() {
                file.push(line("Dossier", &break_path(&dir.display().to_string(), 44)));
            }
        }
        file.push(line(
            "Taille",
            &size_text(l.doc.bytes().len() as u64, english),
        ));
        file.push(line("Pages", &l.pages.len().to_string()));
        let (major, minor) = l.doc.version();
        file.push(line("Version PDF", &format!("{major}.{minor}")));

        let meta = read_metadata(&l.doc).unwrap_or_default();
        let mut about = Vec::new();
        for field in Field::ALL {
            let value = match field {
                Field::Created => meta.created.map(|d| date_text(d, english)),
                Field::Modified => meta.modified.map(|d| date_text(d, english)),
                other => meta.get(other),
            };
            if let Some(value) = value.filter(|v| !v.trim().is_empty()) {
                about.push(line(field.label(), value.trim()));
            }
        }

        let protection = match l.doc.security() {
            None => tr("non").to_string(),
            Some(h) if h.is_owner() => tr("oui, tous les droits accordés").to_string(),
            Some(h) => {
                let limits: Vec<&str> = Permissions::from_p(h.permissions())
                    .restrictions()
                    .into_iter()
                    .map(tr)
                    .collect();
                if limits.is_empty() {
                    tr("oui").to_string()
                } else {
                    format!("{} ({})", tr("oui"), limits.join(", "))
                }
            }
        };
        let mut blocks = vec![file.join("\n")];
        if !about.is_empty() {
            blocks.push(about.join("\n"));
        }
        blocks.push(line("Protégé", &protection));
        let message = blocks.join("\n\n");
        self.inform(tr("Propriétés du document"), &message);
    }
}

/// Onglets à fermer pour ne garder que `keep` : tous les autres, sauf ceux
/// qui ont des modifications non enregistrées, **du dernier au premier**.
/// Dans cet ordre, fermer un onglet ne décale aucun de ceux qui restent à
/// fermer.
fn close_plan(count: usize, keep: usize, modified: &[bool]) -> Vec<usize> {
    (0..count)
        .rev()
        .filter(|&i| i != keep && !modified.get(i).copied().unwrap_or(false))
        .collect()
}

/// « Libellé : valeur », avec l'espace avant les deux-points du français.
fn field_line(label: &str, value: &str, english: bool) -> String {
    if english {
        format!("{label}: {value}")
    } else {
        format!("{label} : {value}")
    }
}

/// Coupe un chemin trop long pour une ligne aux séparateurs de dossiers.
///
/// Les fenêtres de message coupent leurs lignes aux espaces ; un chemin
/// n'en a souvent aucun et déborderait de la carte. Chaque morceau garde
/// son séparateur en fin de ligne, pour se lire comme la suite du chemin.
fn break_path(path: &str, max: usize) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    let mut line = 0;
    for part in path.split_inclusive(['\\', '/']) {
        let n = part.chars().count();
        if line > 0 && line + n > max {
            out.push('\n');
            line = 0;
        }
        out.push_str(part);
        line += n;
    }
    out
}

/// Taille d'un fichier, comme l'Explorateur l'écrit : en kilo-octets
/// entiers, ou en méga-octets à une décimale — virgule en français, point
/// en anglais. Les cartes de l'accueil et les propriétés disent ainsi la
/// même chose d'un même fichier.
pub(super) fn size_text(bytes: u64, english: bool) -> String {
    const MIB: u64 = 1024 * 1024;
    let (kilo, mega, point) = if english {
        ("KB", "MB", '.')
    } else {
        ("Ko", "Mo", ',')
    };
    if bytes >= MIB {
        let tenths = bytes * 10 / MIB;
        format!("{}{point}{} {mega}", tenths / 10, tenths % 10)
    } else {
        format!("{} {kilo}", (bytes / 1024).max(1))
    }
}

/// Date d'une métadonnée, à la minute : « 12/03/2024 10:30 » en français,
/// « 2024-03-12 10:30 » en anglais (l'ordre jour/mois y serait ambigu).
fn date_text(d: Date, english: bool) -> String {
    if english {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            d.year, d.month, d.day, d.hour, d.minute
        )
    } else {
        format!(
            "{:02}/{:02}/{:04} {:02}:{:02}",
            d.day, d.month, d.year, d.hour, d.minute
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fermer_les_autres_garde_la_cible_et_les_modifies() {
        let plan = close_plan(5, 2, &[false, true, false, false, false]);
        assert_eq!(plan, vec![4, 3, 0], "du dernier au premier, sans 1 ni 2");
        assert_eq!(close_plan(1, 0, &[false]), Vec::<usize>::new());
        assert_eq!(close_plan(3, 0, &[false, false, false]), vec![2, 1]);
        // Tous modifiés : rien ne se ferme sans qu'on le demande.
        assert!(close_plan(3, 1, &[true, false, true]).is_empty());
    }

    #[test]
    fn un_long_chemin_se_coupe_aux_dossiers() {
        let path = r"C:\Users\quelquun\Documents\Projets\2026\Contrats signés\version finale";
        let cut = break_path(path, 30);
        assert_eq!(cut.replace('\n', ""), path, "rien ne se perd");
        assert!(cut.lines().count() > 1);
        for line in cut.lines() {
            assert!(line.chars().count() <= 30, "« {line} » dépasse");
        }
        assert!(cut.lines().next().is_some_and(|l| l.ends_with('\\')));
        // Un chemin court reste d'un seul tenant.
        assert_eq!(break_path(r"C:\Temp", 30), r"C:\Temp");
        // Un dossier plus long que la ligne n'est pas coupé en son milieu.
        assert_eq!(
            break_path("/un-nom-de-dossier-interminable/", 10)
                .lines()
                .count(),
            2
        );
    }

    #[test]
    fn tailles_et_dates_se_lisent_dans_les_deux_langues() {
        assert_eq!(size_text(10, false), "1 Ko", "jamais « 0 Ko »");
        assert_eq!(size_text(2048, true), "2 KB");
        assert_eq!(size_text(52_000, false), "50 Ko", "arrondi par défaut");
        assert_eq!(size_text(1_572_864, false), "1,5 Mo");
        assert_eq!(size_text(1_572_864, true), "1.5 MB");
        let d = Date {
            year: 2024,
            month: 3,
            day: 12,
            hour: 9,
            minute: 5,
            ..Date::default()
        };
        assert_eq!(date_text(d, false), "12/03/2024 09:05");
        assert_eq!(date_text(d, true), "2024-03-12 09:05");
        assert_eq!(field_line("Pages", "2", false), "Pages : 2");
        assert_eq!(field_line("Pages", "2", true), "Pages: 2");
    }

    #[test]
    fn la_cible_par_defaut_est_ce_qui_est_a_l_ecran() {
        assert_eq!(Target::default(), Target::Current);
    }
}
