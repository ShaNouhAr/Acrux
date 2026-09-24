//! Le mode « Modifier le PDF » dans le visualiseur : cadres des paragraphes,
//! curseur, sélection, frappe recomposée en direct, zones de texte neuves.
//!
//! La saisie et la barre du mode sont dans [`crate::ui::editpdf`] ; ce qui
//! est ici relie ces morceaux au document, à la mise en page de la fenêtre
//! et au rendu.
//!
//! # Une frappe
//!
//! Le document n'est **pas** touché pendant qu'on tape. À l'ouverture d'un
//! bloc, sa police est chargée une fois ([`LiveText`]) ; ensuite :
//!
//! 1. la saisie change le texte en mémoire ;
//! 2. le texte est mis en page en mémoire — quelques microsecondes, quelle
//!    que soit la page ;
//! 3. la page rendue est **masquée** à l'endroit du bloc, et l'application y
//!    dessine elle-même les glyphes tapés. C'est ce qui rend la frappe
//!    immédiate : ni extraction, ni réécriture, ni rendu de page ;
//! 4. en sortant du bloc — clic ailleurs, Échap, changement d'outil,
//!    enregistrement — le texte est écrit **une fois** dans le document, et
//!    l'historique reçoit une seule opération : annuler défait toute la
//!    saisie d'un coup.
//!
//! La mise en page de l'aperçu est celle qui écrira : ce qu'on voit en
//! tapant est ce qu'on obtient.

use super::{
    base_matrix, collect_pages, render_page_rotated, shown_rotation, Color, Cursor, Duration,
    EditOp, Frame, HashMap, Instant, Key, Matrix, Modifiers, PageBox, PageIndex, Point, Rect,
    RenderOptions, Toolbar, Viewer, WindowHandle,
};
use crate::ui::editpdf::{
    step_of, step_size, BarAction, Buffer, EditBar, EditTool, Step, StepKind,
};
use crate::ui::objects::{self as objects_ui, Handle, ViewRect};
use acrux_features::edit_text::{
    line_at, line_unit, move_paragraph_styled, normalized, open_paragraph, open_unit,
    set_paragraph_text, text_frame_at, text_units, CaretMap, LaidText, LiveText, NewTextStyle,
    ParagraphFrame, Styles, TextUnit,
};
use acrux_features::ocr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Demi-période du clignotement du curseur, comme sous Windows.
const BLINK_MS: u128 = 530;

/// État du mode.
pub(super) struct EditMode {
    /// Barre du mode.
    pub bar: EditBar,
    /// Paragraphe en cours d'édition.
    pub active: Option<Active>,
    /// Paragraphe survolé : page et boîte, en espace de page.
    pub hover: Option<(usize, Rect)>,
    /// Départ du clignotement : remis à zéro à chaque frappe, pour que le
    /// curseur reste visible pendant qu'on tape.
    pub blink: Instant,
    /// Tient le fil qui fait clignoter le curseur ; le passer à faux l'arrête.
    pub ticking: Arc<AtomicBool>,
    /// Blocs éditables par page, recalculés quand la page change.
    pub units: HashMap<usize, std::rc::Rc<Vec<TextUnit>>>,
    /// Texte lu dans les images de chaque page, quand on l'a demandé.
    ///
    /// La lecture d'une image coûte le prix d'une seconde : elle ne se fait
    /// qu'au premier clic sur une image, et son résultat sert ensuite.
    pub read: HashMap<usize, std::rc::Rc<Vec<ocr::ImageText>>>,
    /// Écriture au service d'un autre outil (« remplir et signer ») : pas de
    /// barre à soi, et c'est l'autre outil qui commande.
    pub overlay: bool,
}

/// Déplacement ou redimensionnement en cours.
///
/// Rien n'est écrit pendant le geste : c'est la boîte du bloc qui change, et
/// le texte s'y recompose à chaque pas — sous les yeux, comme dans Acrobat.
#[derive(Debug, Clone)]
pub(super) struct Drag {
    /// Poignée saisie ; `None` pour un déplacement.
    pub handle: Option<Handle>,
    /// Point de départ, en espace de page.
    pub from: Point,
    /// Boîte d'écriture au début du geste : **la** référence, pour que le
    /// mouvement ne s'emballe pas.
    pub start: ParagraphFrame,
    /// Boîte visible au début du geste.
    pub box_start: Rect,
    /// Repères d'alignement trouvés (abscisse, ordonnée).
    pub guides: (Option<f64>, Option<f64>),
    /// Majuscule enfoncée au départ : le déplacement suit alors un axe.
    pub shift: bool,
}

/// Paragraphe en cours d'édition.
///
/// C'est **le** seul état d'un bloc : l'ouvrir, c'est à la fois y poser le
/// curseur et le saisir. Le cadre, ses poignées, la frappe, le déplacement et
/// le redimensionnement portent tous sur lui, et une seule écriture les
/// conclut — celle de la sortie du bloc.
pub(super) struct Active {
    /// Page.
    pub page: usize,
    /// Boîte relevée à l'ouverture : c'est elle qui retrouvera le bloc dans
    /// le document, quoi qu'on ait déplacé depuis.
    pub origin: ParagraphFrame,
    /// Boîte courante, déplacée et redimensionnée à volonté.
    pub frame: ParagraphFrame,
    /// Ce que le paragraphe portait à l'ouverture, sans blancs : c'est ce que
    /// l'annulation retrouvera sur le fichier d'origine.
    pub original: String,
    /// Ce qu'il porte maintenant, sans blancs.
    pub drawn: String,
    /// Texte, curseur et sélection.
    pub buffer: Buffer,
    /// Position de chaque caractère.
    pub map: CaretMap,
    /// Place de l'opération de cette saisie dans l'historique.
    pub history: Option<usize>,
    /// Glisser en cours pour sélectionner.
    pub selecting: bool,
    /// Police et mesures du bloc : de quoi le dessiner sans rien écrire.
    /// Vide quand la police n'a pas pu être chargée — la frappe repasse
    /// alors par le document, comme avant.
    pub live: Option<LiveText>,
    /// Texte mis en page en mémoire, dessiné tel quel par l'application.
    pub laid: Option<LaidText>,
    /// Zone du texte d'origine, à masquer tant que la saisie dure : le
    /// document le porte encore, mais ce n'est plus lui qu'on montre.
    pub cover: Option<Rect>,
    /// Couleur du papier sous le bloc, relevée sur la page rendue.
    pub paper: (u8, u8, u8),
    /// Vrai dès qu'on a tapé ou bougé : le document ne le sait pas encore.
    pub dirty: bool,
    /// Zone de l'image à couvrir avant d'écrire, et la couleur du papier.
    ///
    /// C'est ce qui rend modifiable le texte d'une image : le texte lu reste
    /// dessiné dans l'image, on le couvre donc à l'écriture, et l'on pose le
    /// vrai texte par-dessus.
    pub mask: Option<(Rect, [f64; 3])>,
    /// Geste en cours sur le cadre.
    pub drag: Option<Drag>,
    /// Poignée survolée.
    pub hover: Option<Handle>,
    /// Style de chaque caractère : police, corps, couleur.
    ///
    /// Relevé à l'ouverture, reporté à chaque frappe : c'est ce qui garde le
    /// gras gras et l'italique italique quand on modifie une ligne qui les
    /// mêle.
    pub styles: Styles,
    /// Texte d'où viennent ces styles.
    pub styled_from: String,
    /// Étapes annulables **dans le bloc** : ce qu'était le texte avant
    /// chaque geste de frappe.
    ///
    /// Sans elles, Ctrl+Z refermait la saisie et défaisait tout d'un coup,
    /// alors qu'on venait d'effacer une lettre.
    pub undo: Vec<Step>,
    /// Étapes rétablissables.
    pub redo: Vec<Step>,
    /// Nature du dernier geste, pour savoir si le suivant le prolonge.
    pub last_step: StepKind,
    /// Où était le curseur à la fin du dernier geste.
    pub last_caret: usize,
    /// Vrai tant qu'on écrit ; faux quand le bloc est **posé** — il garde
    /// alors son cadre et ses poignées, et se déplace d'un glissement,
    /// comme une signature qu'on vient de poser.
    pub writing: bool,
}

impl Drop for EditMode {
    fn drop(&mut self) {
        self.ticking.store(false, Ordering::Relaxed);
    }
}

/// Lit le catalogue des polices du système en tâche de fond : quelques
/// centaines de fichiers à ouvrir, qu'on ne veut pas payer au premier clic
/// sur la liste.
fn warm_fonts() {
    let _ = std::thread::Builder::new()
        .name("polices".into())
        .spawn(|| {
            let _ = acrux_features::sysfonts::families();
        });
}

impl Viewer {
    /// Vrai quand le mode est actif.
    pub(super) fn edit_on(&self) -> bool {
        self.edit.is_some()
    }

    /// Vrai quand un paragraphe reçoit la frappe.
    ///
    /// Un bloc **posé** ne la reçoit pas : il est sélectionné, pas ouvert.
    pub(super) fn editing_text(&self) -> bool {
        self.edit
            .as_ref()
            .is_some_and(|e| e.active.as_ref().is_some_and(|a| a.writing))
    }

    /// Pose le bloc en cours : il est écrit, puis reste **sélectionné**,
    /// avec son cadre et ses poignées — prêt à être déplacé ou
    /// redimensionné, comme une signature qu'on vient de poser.
    fn pose_block(&mut self) {
        self.commit_active();
        if let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) {
            a.writing = false;
            a.selecting = false;
            a.drag = None;
        }
        self.set_notice(
            crate::ui::lang::tr(
                "bloc posé : glissez-le, tirez une poignée, ou cliquez dedans pour écrire",
            )
            .into(),
        );
    }

    /// Vrai quand un bloc est posé : encadré, mais pas en écriture.
    pub(super) fn block_posed(&self) -> bool {
        self.edit
            .as_ref()
            .is_some_and(|e| e.active.as_ref().is_some_and(|a| !a.writing))
    }

    /// Touche sur un bloc posé : flèches pour le déplacer au point près,
    /// Suppr pour l'effacer, Échap pour le lâcher.
    ///
    /// Rend vrai si la touche a été prise.
    pub(super) fn posed_key(
        &mut self,
        key: Key,
        m: Modifiers,
        window: &mut dyn WindowHandle,
    ) -> bool {
        if !self.block_posed() {
            return false;
        }
        // Un pas d'un point, dix avec la touche majuscule : c'est la mesure
        // d'Acrobat, et celle dont on a besoin pour aligner à l'œil.
        let step = if m.shift { 10.0 } else { 1.0 };
        let (dx, dy) = match key {
            Key::Left => (-step, 0.0),
            Key::Right => (step, 0.0),
            Key::Up => (0.0, step),
            Key::Down => (0.0, -step),
            Key::Escape => {
                self.close_active();
                window.request_redraw();
                return true;
            }
            Key::Enter => {
                // Entrée rouvre le texte, curseur à la fin.
                if let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) {
                    a.writing = true;
                    let end = a.buffer.text.chars().count();
                    a.buffer.move_to(end, false);
                }
                if let Some(mode) = &mut self.edit {
                    mode.blink = Instant::now();
                }
                window.request_redraw();
                return true;
            }
            Key::Delete | Key::Backspace => {
                // Le bloc s'efface, d'un seul geste annulable.
                if let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) {
                    a.buffer = Buffer::new("");
                    a.laid = None;
                    a.map = CaretMap::default();
                    a.dirty = true;
                }
                self.close_active();
                window.request_redraw();
                return true;
            }
            _ => return false,
        };
        if let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) {
            a.frame.x0 += dx;
            a.frame.baseline += dy;
            a.dirty = true;
            if let Some(live) = a.live.as_ref() {
                let laid = live.lay(&a.frame, &a.buffer.text);
                a.map = laid.caret.clone();
                a.laid = Some(laid);
            }
        }
        window.request_redraw();
        true
    }

    /// Outil du mode, s'il est actif.
    pub(super) fn edit_tool(&self) -> Option<EditTool> {
        self.edit.as_ref().map(|e| e.bar.tool)
    }

    /// Entre dans le mode (ou change d'outil s'il est déjà actif).
    pub(super) fn enter_edit(&mut self, tool: EditTool, window: &mut dyn WindowHandle) {
        if self.loaded.is_none() {
            return;
        }
        // Un champ de formulaire en saisie est validé : on change d'outil.
        self.commit_field();
        // Le mode écrit le document sans passer par `apply_edit` : le droit
        // se vérifie donc à l'entrée.
        if self.edit.is_none() && !self.require_right(crate::render_worker::Right::Modify) {
            window.request_redraw();
            return;
        }
        self.leave_home();
        if let Some(mode) = &mut self.edit {
            if mode.bar.tool == tool {
                // Recliquer l'outil actif ressort du mode, comme dans Acrobat.
                self.leave_edit();
                return;
            }
            mode.bar.tool = tool;
            return;
        }
        // Un seul outil à la fois : les autres rendent la main — la barre
        // des commentaires comprise, qui resterait sinon sous celle du mode
        // (Ctrl+Maj+E l'y laissait).
        self.sign_panel = None;
        self.objects = None;
        self.selection = None;
        self.close_annot_tools();
        warm_fonts();
        let ticking = Arc::new(AtomicBool::new(true));
        let waker = window.waker();
        let flag = Arc::clone(&ticking);
        let _ = std::thread::Builder::new()
            .name("curseur".into())
            .spawn(move || {
                while flag.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(
                        u64::try_from(BLINK_MS).unwrap_or(530),
                    ));
                    waker.wake();
                }
            });
        self.edit = Some(EditMode {
            bar: EditBar::with_tool(tool),
            active: None,
            hover: None,
            blink: Instant::now(),
            ticking,
            units: HashMap::new(),
            read: HashMap::new(),
            overlay: false,
        });
        self.set_notice(match tool {
            EditTool::Select => "cliquez dans un texte pour le modifier".into(),
            EditTool::AddText => "cliquez sur la page pour y poser du texte".into(),
        });
    }

    /// Sort du mode, après avoir porté au document ce qui a été tapé.
    pub(super) fn leave_edit(&mut self) {
        self.close_active();
        self.edit = None;
        self.title_dirty = true;
    }

    /// Vrai si l'on écrit au service d'un autre outil.
    pub(super) fn edit_overlay(&self) -> bool {
        self.edit.as_ref().is_some_and(|e| e.overlay)
    }

    /// Ouvre l'écriture sur la page pour un autre outil : une zone de texte
    /// neuve à l'endroit cliqué, sans barre ni cadre à soi.
    ///
    /// C'est ce que fait « remplir et signer » d'Acrobat : on clique sur le
    /// formulaire, on tape, on clique ailleurs, on tape. Passer par une boîte
    /// de dialogue pour chaque valeur ferait perdre le fil.
    pub(super) fn type_on_page(
        &mut self,
        page: usize,
        pt: Point,
        color: [f64; 3],
        window: &mut dyn WindowHandle,
    ) {
        if self.edit.is_none() {
            warm_fonts();
            let ticking = Arc::new(AtomicBool::new(true));
            let waker = window.waker();
            let flag = Arc::clone(&ticking);
            let _ = std::thread::Builder::new()
                .name("curseur".into())
                .spawn(move || {
                    while flag.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(
                            u64::try_from(BLINK_MS).unwrap_or(530),
                        ));
                        waker.wake();
                    }
                });
            self.edit = Some(EditMode {
                bar: EditBar::with_tool(EditTool::AddText),
                active: None,
                hover: None,
                blink: Instant::now(),
                ticking,
                units: HashMap::new(),
                read: HashMap::new(),
                overlay: true,
            });
        }
        if let Some(mode) = &mut self.edit {
            mode.bar.color_set = true;
            mode.bar.color = color;
        }
        self.open_new_box(page, pt, window);
    }

    /// Hauteur de la barre du mode, nulle quand il dort.
    pub(super) fn edit_bar_height(&self) -> u32 {
        if self.edit.as_ref().is_some_and(|e| !e.overlay) && !self.fullscreen && !self.reading {
            EditBar::height(self.dpi_scale as f32).max(0) as u32
        } else {
            0
        }
    }

    /// Dessine la barre du mode sous la barre d'outils.
    pub(super) fn paint_edit_bar(&mut self, frame: &mut Frame<'_>) {
        // Au service d'un autre outil, c'est sa barre à lui qui reste.
        if self.edit_overlay() {
            return;
        }
        let y = Toolbar::height(&self.theme, self.dpi_scale as f32) + self.tabs_height() as i32;
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        let active_size = self
            .edit
            .as_ref()
            .and_then(|e| e.active.as_ref())
            .map(|a| a.frame.size);
        if let (Some(mode), Some(text)) = (self.edit.as_mut(), self.text.as_mut()) {
            mode.bar.active_size = active_size;
            mode.bar
                .paint(frame, text, &mut self.raster, &theme, dpi, y);
            // La liste des polices dessine ses noms quelques-uns par image :
            // s'il en reste, **un** réveil par peinture en demande une autre.
            // Surtout pas depuis la boucle d'événements : chaque réveil en
            // posterait un nouveau, la file ne se viderait jamais, et Windows
            // — qui ne repeint que file vide — figerait la fenêtre.
            if mode.bar.pending() {
                if let Some(w) = &self.waker {
                    w.wake();
                }
            }
        }
    }

    /// Clic dans la barre du mode. Rend vrai s'il la concernait.
    pub(super) fn edit_bar_click(&mut self, x: i32, y: i32, window: &mut dyn WindowHandle) -> bool {
        let Some(action) = self.edit.as_mut().and_then(|e| e.bar.mouse_down(x, y)) else {
            return false;
        };
        if self.waker.is_none() {
            self.waker = Some(window.waker());
        }
        self.edit_bar_do(action, window);
        true
    }

    /// Touche, quand un sélecteur de la barre est déroulé : il la prend.
    pub(super) fn edit_popup_key(&mut self, key: Key, window: &mut dyn WindowHandle) -> bool {
        let Some(action) = self.edit.as_mut().and_then(|e| e.bar.key(key)) else {
            return false;
        };
        self.edit_bar_do(action, window);
        true
    }

    /// Caractère tapé, quand un sélecteur de la barre est déroulé.
    pub(super) fn edit_popup_char(&mut self, c: char, window: &mut dyn WindowHandle) -> bool {
        let Some(action) = self.edit.as_mut().and_then(|e| e.bar.char(c)) else {
            return false;
        };
        self.edit_bar_do(action, window);
        true
    }

    /// Molette ; vrai si un sélecteur de la barre l'a prise.
    pub(super) fn edit_popup_wheel(&mut self, delta: f64) -> bool {
        self.edit.as_mut().is_some_and(|e| e.bar.wheel(delta))
    }

    /// Déplacement de la souris, quand un sélecteur est déroulé : survol, ou
    /// glissement dans le nuancier — la couleur suit alors en direct.
    pub(super) fn edit_popup_move(
        &mut self,
        x: i32,
        y: i32,
        dragging: bool,
        window: &mut dyn WindowHandle,
    ) -> bool {
        if !self.edit_menu_open() {
            return false;
        }
        if let Some(action) = self
            .edit
            .as_mut()
            .and_then(|e| e.bar.mouse_move(x, y, dragging))
        {
            self.edit_bar_do(action, window);
        }
        true
    }

    /// Bouton relâché : le glissement dans le nuancier est fini.
    pub(super) fn edit_popup_up(&mut self) {
        if let Some(mode) = &mut self.edit {
            mode.bar.mouse_up();
        }
    }

    /// Exécute ce que la barre du mode demande.
    fn edit_bar_do(&mut self, action: BarAction, window: &mut dyn WindowHandle) {
        match action {
            BarAction::Refresh => {}
            BarAction::Colors => {
                if let Some(mode) = &mut self.edit {
                    mode.bar.toggle_colors();
                }
            }
            BarAction::Ink(rgb, chosen) => self.set_ink(rgb, chosen),
            BarAction::Families => {
                if let Some(mode) = &mut self.edit {
                    mode.bar.toggle_menu();
                }
            }
            BarAction::Family(choice) => self.set_family(choice),
            BarAction::Bold => self.toggle_face(true),
            BarAction::Italic => self.toggle_face(false),
            BarAction::Align(i) => self.set_alignment(i),
            BarAction::Tighter => self.change_leading(-0.1),
            BarAction::Looser => self.change_leading(0.1),
            BarAction::Close => self.leave_edit(),
            BarAction::Tool(tool) => {
                if tool == EditTool::AddText {
                    self.close_active();
                }
                if let Some(mode) = &mut self.edit {
                    mode.bar.tool = tool;
                }
            }
            BarAction::Smaller | BarAction::Larger => {
                let up = action == BarAction::Larger;
                // Le paragraphe en cours change de corps ; sans paragraphe,
                // c'est le corps du prochain texte ajouté.
                let resize = self.edit.as_mut().and_then(|mode| {
                    if let Some(a) = &mut mode.active {
                        let old = a.frame.size;
                        let new = step_size(old, up);
                        a.frame.size = new;
                        a.frame.line_spacing *= new / old.max(0.1);
                        Some(a.buffer.clone())
                    } else {
                        mode.bar.size = step_size(mode.bar.size, up);
                        mode.bar.size_set = true;
                        None
                    }
                });
                if let Some(before) = resize {
                    self.edit_apply(before, window);
                }
            }
        }
        window.request_redraw();
    }

    /// Applique une couleur : au bloc ouvert, tout entier et aussitôt visible,
    /// et au texte qu'on ajoutera ensuite. `chosen` : le choix est fait, le
    /// nuancier se referme et la couleur rejoint les récentes.
    fn set_ink(&mut self, rgb: [f64; 3], chosen: bool) {
        let Some(mode) = &mut self.edit else { return };
        mode.bar.color = rgb;
        mode.bar.color_set = true;
        if chosen {
            mode.bar.close_menu();
            crate::ui::pickers::remember_color(rgb);
        }
        let Some(a) = &mut mode.active else { return };
        a.frame.color = rgb;
        a.frame.ink = Some(rgb);
        #[allow(clippy::cast_possible_truncation)]
        let ink = [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32];
        // La couleur vaut pour tout le bloc ; ses styles gardent le reste —
        // un mot gras reste gras.
        let fill = format!("{:.4} {:.4} {:.4} rg", rgb[0], rgb[1], rgb[2]).into_bytes();
        for run in &mut a.styles.runs {
            run.color = ink;
            run.fill.clone_from(&fill);
        }
        if let Some(live) = &mut a.live {
            live.set_color(ink);
        }
        a.dirty = true;
        self.relay_active();
    }

    /// Matrice espace de page → coordonnées de la vue, pour une page.
    pub(super) fn page_to_view(&self, layout: &[PageBox], page: usize) -> Option<Matrix> {
        let l = self.loaded.as_ref()?;
        let (ox, top) = self.page_screen(layout, page)?;
        let PageBox { w, h, .. } = *layout.get(page)?;
        let p = l.pages.get(page)?;
        let m = base_matrix(
            &p.crop_box(&l.doc),
            self.scale(),
            shown_rotation(l, p),
            w,
            h,
        );
        Some(m.then(&Matrix::new(1.0, 0.0, 0.0, 1.0, ox, top)))
    }

    /// Bloc de texte sous un point de la page : son indice (dans l'ordre où
    /// l'éditeur les numérote) et sa boîte.
    fn paragraph_at(&mut self, page: usize, pt: Point) -> Option<(Option<usize>, Rect)> {
        let margin = 2.0;
        self.units(page)
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.bbox.x0 - margin <= pt.x
                    && pt.x <= p.bbox.x1 + margin
                    && p.bbox.y0 - margin <= pt.y
                    && pt.y <= p.bbox.y1 + margin
            })
            // Le plus petit l'emporte : un paragraphe logé dans un autre
            // (une légende dans un cadre) doit rester atteignable.
            .min_by(|(_, a), (_, b)| {
                (a.bbox.width() * a.bbox.height()).total_cmp(&(b.bbox.width() * b.bbox.height()))
            })
            .map(|(i, p)| (Some(i), p.bbox))
            .or_else(|| {
                // Aucun bloc ici : peut-être une ligne que l'extraction n'a
                // rattachée à aucun paragraphe. Elle se modifie quand même.
                let l = self.loaded.as_mut()?;
                let text = l.text(page).0.clone();
                let index = line_at(&text, pt.x, pt.y)?;
                Some((None, text.lines.get(index)?.bbox))
            })
    }

    /// Blocs éditables d'une page, gardés tant que la page ne change pas :
    /// le survol les consulte à chaque mouvement de souris.
    fn units(&mut self, page: usize) -> std::rc::Rc<Vec<TextUnit>> {
        if let Some(u) = self.edit.as_ref().and_then(|e| e.units.get(&page)) {
            return std::rc::Rc::clone(u);
        }
        let units = std::rc::Rc::new(
            self.loaded
                .as_mut()
                .map(|l| text_units(&l.text(page).0))
                .unwrap_or_default(),
        );
        if let Some(mode) = &mut self.edit {
            mode.units.insert(page, std::rc::Rc::clone(&units));
        }
        units
    }

    /// Dessine les cadres, la sélection et le curseur dans la vue.
    #[allow(clippy::too_many_lines)] // une peinture, lue de haut en bas
    pub(super) fn paint_edit(&mut self, frame: &mut Frame<'_>) {
        if self.edit.is_none() || self.loaded.is_none() {
            return;
        }
        let layout = self.layout();
        let view_h = f64::from(self.view_height());
        let accent = self.theme.accent;
        let dpi = self.dpi_scale;
        let thin = dpi.round().max(1.0) as i32;
        let (hover, active_page, blink) = {
            let mode = self.edit.as_ref();
            (
                mode.and_then(|m| m.hover),
                mode.and_then(|m| m.active.as_ref().map(|a| a.page)),
                mode.map_or(0, |m| m.blink.elapsed().as_millis()),
            )
        };
        // 1. Pas de cadre autour de chaque bloc : la page reste lisible. Seul
        //    le bloc survolé se signale, d'un filet discret — c'est ce qui dit
        //    « ceci se modifie » au moment où l'on s'en approche.
        if let Some((page, r)) = hover {
            let visible = layout.get(page).is_some_and(|b| b.visible);
            let is_active = active_page == Some(page)
                && self
                    .edit
                    .as_ref()
                    .and_then(|e| e.active.as_ref())
                    .and_then(|a| a.map.bounds())
                    .is_some_and(|b| b.x0 < r.x1 && r.x0 < b.x1 && b.y0 < r.y1 && r.y0 < b.y1);
            if visible && !is_active {
                if let Some(m) = self.page_to_view(&layout, page) {
                    let dev = m.transform_rect(&r);
                    if dev.y1 >= 0.0 && dev.y0 <= view_h {
                        outline(frame, &dev, 3.0 * dpi, thin, (0xA8, 0xB8, 0xD0));
                    }
                }
            }
        }
        // 4. Le bloc en cours : filet fin, sélection, curseur.
        let Some(page) = active_page else { return };
        let Some(m) = self.page_to_view(&layout, page) else {
            return;
        };
        // Le texte tapé, dessiné par l'application. Le document porte encore
        // l'ancien : on le masque, et l'on trace par-dessus, glyphe à glyphe,
        // avec la police et l'encre du bloc.
        self.paint_live(frame, &m);
        self.paint_active_box(frame, &layout);
        let Some(a) = self.edit.as_ref().and_then(|e| e.active.as_ref()) else {
            return;
        };
        let bounds = a.map.bounds().map_or_else(
            || {
                Rect::new(
                    a.frame.x0,
                    a.frame.baseline - a.frame.size * 0.3,
                    a.frame.x0 + a.frame.size,
                    a.frame.baseline + a.frame.size,
                )
            },
            |r| {
                // La boîte d'un paragraphe de plusieurs lignes est sa largeur
                // figée : on la montre, puisque c'est là que le texte coulera.
                if a.frame.width < 2000.0 && a.map.lines.len() > 1 {
                    Rect::new(a.frame.x0, r.y0, a.frame.x0 + a.frame.width, r.y1)
                } else {
                    r
                }
            },
        );
        // Une zone encore vide n'a pas de texte à montrer : sans repère, un
        // clic dans le blanc semble n'avoir rien fait. On pose donc, sous le
        // curseur, le trait sur lequel le texte va s'écrire.
        if a.buffer.text.is_empty() {
            let line = Rect::new(
                a.frame.x0,
                a.frame.baseline - a.frame.size * 0.12,
                a.frame.x0 + (a.frame.size * 9.0).min(a.frame.width),
                a.frame.baseline - a.frame.size * 0.04,
            );
            tint(frame, &m.transform_rect(&line), accent, 120);
        } else {
            outline(frame, &m.transform_rect(&bounds), 3.0 * dpi, thin, accent);
        }
        if !a.writing {
            // Bloc posé : le cadre et ses poignées suffisent, le curseur
            // n'aurait plus de sens.
            return;
        }
        let (s0, s1) = a.buffer.range();
        for r in a.map.selection_rects(s0, s1) {
            tint(frame, &m.transform_rect(&r), accent, 90);
        }
        if (blink / BLINK_MS) % 2 == 0 {
            let caret = m.transform_rect(&a.map.caret_rect(a.buffer.caret));
            let w = (caret.width().round() as i32).max(thin * 2);
            frame.fill_rect(
                caret.x0.round() as i32,
                caret.y0.round() as i32,
                w,
                caret.height().round().max(1.0) as i32,
                accent.0,
                accent.1,
                accent.2,
            );
        }
    }

    /// Le cadre du bloc ouvert, ses poignées et ses repères d'alignement.
    ///
    /// Se peint **après** le texte tapé : le masque qui cache l'ancien texte
    /// effacerait sinon les poignées.
    fn paint_active_box(&mut self, frame: &mut Frame<'_>, layout: &[PageBox]) {
        let accent = self.theme.accent;
        let dpi = self.dpi_scale;
        // Le bloc ouvert : son cadre, ses poignées, ses repères. Il n'y a
        //    pas d'état « sélectionné » distinct — ouvrir un bloc, c'est y
        //    poser le curseur **et** le saisir.
        if let Some((page, rect, handle, guides)) = self.edit.as_ref().and_then(|e| {
            let a = e.active.as_ref()?;
            let rect = self.active_box()?;
            Some((
                a.page,
                rect,
                a.drag.as_ref().and_then(|d| d.handle).or(a.hover),
                a.drag.as_ref().map_or((None, None), |d| d.guides),
            ))
        }) {
            if let Some(view) = self.page_rect_to_view(page, rect) {
                objects_ui::paint_selection(frame, &self.theme, dpi as f32, view, handle);
            }
            // Les repères : une ligne fine qui dit sur quoi le bloc s'aligne.
            if let Some(m) = self.page_to_view(layout, page) {
                let view_w = f64::from(frame.width);
                if let Some(x) = guides.0 {
                    let p0 = m.apply(Point::new(x, rect.y0));
                    let p1 = m.apply(Point::new(x, rect.y1));
                    guide_line(frame, p0.x, p0.y - 40.0, p1.x, p1.y + 40.0, accent);
                }
                if let Some(y) = guides.1 {
                    let p0 = m.apply(Point::new(rect.x0, y));
                    let p1 = m.apply(Point::new(rect.x1, y));
                    guide_line(
                        frame,
                        (p0.x - 40.0).max(0.0),
                        p0.y,
                        (p1.x + 40.0).min(view_w),
                        p1.y,
                        accent,
                    );
                }
            }
        }
    }

    /// Masque le texte d'origine et dessine celui qu'on tape.
    ///
    /// C'est ce qui remplace la réécriture du document à chaque lettre : la
    /// mise en page est la même que celle qui écrira, si bien que le texte ne
    /// bouge pas d'un pixel quand la saisie est enfin portée au fichier.
    #[allow(clippy::many_single_char_names)] // composantes de couleur
    fn paint_live(&mut self, frame: &mut Frame<'_>, view: &Matrix) {
        let Some((cover, paper, ink, paths)) = self.edit.as_ref().and_then(|e| {
            let a = e.active.as_ref()?;
            if !a.dirty {
                return None;
            }
            let live = a.live.as_ref()?;
            let laid = a.laid.as_ref()?;
            let [r, g, b] = live.color();
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let ink = (
                (r * 255.0).clamp(0.0, 255.0) as u8,
                (g * 255.0).clamp(0.0, 255.0) as u8,
                (b * 255.0).clamp(0.0, 255.0) as u8,
            );
            let size = a.frame.size;
            let styles = (!a.styles.uniform()).then_some(&a.styles);
            // Chaque glyphe porte son corps et son encre : un mot en gras se
            // dessine en gras, un lien en bleu.
            let paths: Vec<Traced> = live
                .glyphs_styled(laid, size, styles)
                .into_iter()
                .filter_map(|placed| {
                    let path = live.outline(&placed)?;
                    let body = placed.size;
                    let at = Matrix::new(body, 0.0, 0.0, body, placed.x, placed.baseline);
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let tint = (
                        (placed.color[0] * 255.0).clamp(0.0, 255.0) as u8,
                        (placed.color[1] * 255.0).clamp(0.0, 255.0) as u8,
                        (placed.color[2] * 255.0).clamp(0.0, 255.0) as u8,
                    );
                    Some((path, at.then(view), tint))
                })
                .collect();
            Some((a.cover, a.paper, ink, paths))
        }) else {
            return;
        };
        // 1. Le texte d'origine s'efface sous une plaque de la couleur du
        //    papier, relevée sur la page elle-même.
        if let Some(r) = cover {
            let margin = 0.12 * (r.y1 - r.y0).abs().max(1.0);
            let dev = view.transform_rect(&Rect::new(
                r.x0 - margin,
                r.y0 - margin,
                r.x1 + margin,
                r.y1 + margin,
            ));
            #[allow(clippy::cast_possible_truncation)]
            frame.fill_rect(
                dev.x0.min(dev.x1).round() as i32,
                dev.y0.min(dev.y1).round() as i32,
                dev.width().round().max(1.0) as i32,
                dev.height().round().max(1.0) as i32,
                paper.0,
                paper.1,
                paper.2,
            );
        }
        // 2. Puis les glyphes tapés, tracés un à un, chacun de son encre.
        let _ = ink;
        for (path, matrix, tint) in paths {
            crate::ui::sign::fill_path(frame, &mut self.raster, &path, &matrix, tint);
        }
    }

    /// Clic dans le document en mode édition. Rend vrai s'il l'a pris.
    pub(super) fn edit_mouse_down(
        &mut self,
        x: i32,
        y: i32,
        clicks: u8,
        shift: bool,
        window: &mut dyn WindowHandle,
    ) -> bool {
        let Some(tool) = self.edit_tool() else {
            return false;
        };
        let Some((page, pt)) = self.page_at(x, y) else {
            return true;
        };
        if let Some(mode) = &mut self.edit {
            mode.blink = Instant::now();
        }
        // 1. Le bord du cadre ou une poignée : on déplace, on redimensionne.
        if self.start_gesture(page, pt, x, y, shift) {
            window.request_redraw();
            return true;
        }
        // 2. Dans le bloc ouvert : poser le curseur, sélectionner.
        let writing = self
            .edit
            .as_ref()
            .and_then(|e| e.active.as_ref())
            .is_some_and(|a| a.writing);
        let inside = self
            .edit
            .as_ref()
            .and_then(|e| e.active.as_ref())
            .filter(|a| a.page == page)
            .zip(self.active_box())
            .is_some_and(|(a, r)| {
                let m = a.frame.size * 0.4;
                pt.x >= r.x0 - m && pt.x <= r.x1 + m && pt.y >= r.y0 - m && pt.y <= r.y1 + m
            });
        if inside && !writing {
            // Bloc posé : un clic dedans rend la main au texte, curseur au
            // point cliqué. C'est le geste inverse de « j'ai fini d'écrire ».
            if let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) {
                a.writing = true;
                a.drag = None;
                let index = a.map.nearest(pt.x, pt.y);
                a.buffer.move_to(index, false);
                a.selecting = true;
            }
            if let Some(mode) = &mut self.edit {
                mode.blink = Instant::now();
            }
            window.request_redraw();
            return true;
        }
        if inside {
            if let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) {
                let index = a.map.nearest(pt.x, pt.y);
                match clicks {
                    2 => a.buffer.select_word(index),
                    c if c >= 3 => a.buffer.select_all(),
                    _ => a.buffer.move_to(index, shift),
                }
                a.selecting = clicks < 2;
            }
            window.request_redraw();
            return true;
        }
        // 3. Ailleurs. Si l'on écrivait et que le clic tombe dans le blanc,
        //    on **pose** le bloc : il garde son cadre et ses poignées, prêt à
        //    être déplacé ou redimensionné — comme une signature qu'on vient
        //    de poser. Le clic suivant, lui, fera ce qu'il a à faire.
        let nothing_here = self.paragraph_at(page, pt).is_none();
        let posable = writing
            && nothing_here
            && !self.edit_overlay()
            && self
                .edit
                .as_ref()
                .and_then(|e| e.active.as_ref())
                .is_some_and(|a| a.page == page && !a.buffer.text.trim().is_empty());
        if posable {
            self.pose_block();
            window.request_redraw();
            return true;
        }
        self.close_active();
        // Au service de « remplir et signer », un clic ailleurs **repasse par
        // lui** : c'est lui qui sait viser une ligne à remplir, une case, un
        // peigne. Sans cela, le deuxième clic ouvrait une zone à l'endroit
        // brut du clic — sur le trait, et non au-dessus comme le premier.
        if self.edit_overlay() && self.sign_panel.is_some() {
            if !self.place_sign(x, y, window) {
                // Rien à remplir ici : l'écriture s'arrête là.
                self.leave_edit();
            }
            window.request_redraw();
            return true;
        }
        match tool {
            EditTool::AddText => self.open_new_box(page, pt, window),
            EditTool::Select => {
                // Un seul clic ouvre le bloc et y pose le curseur : dans
                // Acrobat, on clique sur un texte et l'on écrit.
                if let Some((index, _)) = self.paragraph_at(page, pt) {
                    self.open_existing(page, index, pt);
                    // Un double-clic choisit le mot, un triple le bloc.
                    if let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) {
                        let caret = a.buffer.caret;
                        match clicks {
                            2 => a.buffer.select_word(caret),
                            c if c >= 3 => a.buffer.select_all(),
                            _ => {}
                        }
                    }
                } else if !self.open_image_line(page, pt) {
                    // Un clic hors du texte prépare une zone neuve, invisible
                    // tant qu'on n'a rien tapé : pour remplir un formulaire
                    // imprimé, on clique sur chaque ligne et on écrit, sans
                    // repasser par la barre.
                    self.open_new_box(page, pt, window);
                }
            }
        }
        window.request_redraw();
        true
    }

    /// Point de la vue exprimé dans **une page donnée**.
    ///
    /// Pendant un glissement, `page_at` suivrait le pointeur jusqu'à la page
    /// voisine : le bloc sauterait alors d'un repère à l'autre. C'est la page
    /// du bloc qui compte, et elle seule.
    fn point_in_page(&self, page: usize, x: i32, y: i32) -> Option<Point> {
        let layout = self.layout();
        let m = self.page_to_view(&layout, page)?;
        let inverse = m.invert()?;
        Some(inverse.apply(Point::new(f64::from(x), f64::from(y))))
    }

    /// Boîte visible du bloc ouvert, en espace de page.
    ///
    /// C'est celle du texte tel qu'il est mis en page maintenant — pendant un
    /// geste, elle suit donc le bloc sans qu'on ait rien écrit.
    fn active_box(&self) -> Option<Rect> {
        let a = self.edit.as_ref()?.active.as_ref()?;
        // Une zone encore vide n'a pas de cadre : un clic dans le blanc
        // prépare une zone de texte, il ne doit pas couvrir la page de
        // poignées avant qu'on ait tapé quoi que ce soit.
        if a.buffer.text.is_empty() {
            return None;
        }
        let text = a
            .laid
            .as_ref()
            .map_or_else(|| a.map.bounds(), |l| l.caret.bounds());
        let f = &a.frame;
        let fallback = Rect::new(
            f.x0,
            f.baseline - f.size,
            f.x0 + f.width,
            f.baseline + f.size,
        );
        let r = text.unwrap_or(fallback);
        // Un paragraphe de plusieurs lignes montre la largeur où son texte
        // coule : c'est elle qu'on redimensionne.
        let wide = f.width < 2000.0
            && (a.map.lines.len() > 1 || a.laid.as_ref().is_some_and(|l| l.lines.len() > 1));
        let r = if wide {
            Rect::new(f.x0, r.y0, f.x0 + f.width, r.y1)
        } else {
            r
        };
        // Un peu d'air, comme dans Acrobat : les poignées ne doivent pas
        // mordre sur les lettres.
        let pad = f.size * 0.18;
        Some(Rect::new(r.x0 - pad, r.y0 - pad, r.x1 + pad, r.y1 + pad))
    }

    /// Le cadre du bloc ouvert dans la vue, s'il est visible.
    fn active_view_rect(&self) -> Option<ViewRect> {
        let page = self.edit.as_ref()?.active.as_ref()?.page;
        self.page_rect_to_view(page, self.active_box()?)
    }

    /// Commence un déplacement ou un redimensionnement du bloc ouvert.
    ///
    /// Rend vrai si le clic tombe sur une poignée ou sur le bord du cadre —
    /// l'intérieur, lui, appartient au texte.
    fn start_gesture(&mut self, page: usize, pt: Point, x: i32, y: i32, shift: bool) -> bool {
        let Some(view) = self.active_view_rect() else {
            return false;
        };
        let Some(bbox) = self.active_box() else {
            return false;
        };
        // Pendant l'écriture, l'intérieur du cadre appartient au **texte** :
        // on y pose le curseur et l'on y sélectionne, et seuls les poignées
        // et le bord saisissent le bloc. Une fois le bloc posé, tout son
        // corps le saisit — comme une signature qu'on vient de placer.
        let writing = self
            .edit
            .as_ref()
            .and_then(|e| e.active.as_ref())
            .is_some_and(|a| a.writing);
        let handle = objects_ui::handle_at(view, f64::from(x), f64::from(y), self.dpi_scale)
            .filter(|h| *h != Handle::Body || !writing);
        let on_border = {
            // Le bord : une bande de deux points de part et d'autre du cadre.
            let m = 2.5;
            let outer = pt.x >= bbox.x0 - m
                && pt.x <= bbox.x1 + m
                && pt.y >= bbox.y0 - m
                && pt.y <= bbox.y1 + m;
            let inner = pt.x > bbox.x0 + m
                && pt.x < bbox.x1 - m
                && pt.y > bbox.y0 + m
                && pt.y < bbox.y1 - m;
            outer && !inner
        };
        if handle.is_none() && !on_border {
            return false;
        }
        let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) else {
            return false;
        };
        if a.page != page {
            return false;
        }
        a.drag = Some(Drag {
            handle,
            from: pt,
            start: a.frame.clone(),
            box_start: bbox,
            guides: (None, None),
            shift,
        });
        a.selecting = false;
        true
    }

    /// Fait suivre le geste : la boîte change, et le texte s'y recompose.
    fn gesture_to(&mut self, pt: Point) {
        let guides = self.guides(pt);
        let page = self
            .edit
            .as_ref()
            .and_then(|e| e.active.as_ref())
            .map(|a| a.page);
        let crop = page.and_then(|page| {
            self.loaded
                .as_ref()
                .and_then(|l| l.pages.get(page).map(|p| p.crop_box(&l.doc)))
        });
        let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) else {
            return;
        };
        let Some(drag) = a.drag.as_mut() else { return };
        let shift = drag.shift;
        let (mut dx, mut dy) = (pt.x - drag.from.x, pt.y - drag.from.y);
        let mut frame = drag.start.clone();
        if let Some(handle) = drag.handle {
            // Une poignée ne change **que la boîte** : le corps du texte reste
            // le même et les lignes se recoupent dedans, comme dans Acrobat.
            let box_after = objects_ui::resized(drag.box_start, handle, dx, dy, false);
            frame.x0 = drag.start.x0 + (box_after.x0 - drag.box_start.x0);
            frame.width =
                (drag.start.width + box_after.width() - drag.box_start.width()).max(frame.size);
            frame.baseline = drag.start.baseline + (box_after.y1 - drag.box_start.y1);
            drag.guides = (None, None);
        } else {
            // Déplacement : la touche majuscule retient le geste sur un axe,
            // et les repères d'alignement attirent le bloc.
            if shift {
                if dx.abs() > dy.abs() {
                    dy = 0.0;
                } else {
                    dx = 0.0;
                }
            } else {
                if let Some(x) = guides.0 {
                    dx = x - drag.box_start.x0;
                }
                if let Some(y) = guides.1 {
                    dy = y - drag.box_start.y1;
                }
            }
            frame.x0 = drag.start.x0 + dx;
            frame.baseline = drag.start.baseline + dy;
            drag.guides = if shift { (None, None) } else { guides };
        }
        // La boîte reste sur la page : un bloc poussé dehors ne s'imprimerait
        // pas, et ne se retrouverait plus.
        if let Some(crop) = crop {
            frame.width = frame.width.min(crop.width());
            frame.x0 = frame
                .x0
                .clamp(crop.x0, (crop.x1 - frame.width).max(crop.x0));
            frame.baseline = frame
                .baseline
                .clamp(crop.y0 + frame.size, crop.y1 - frame.size * 0.2);
        }
        let changed = frame != a.frame;
        a.frame = frame;
        if changed {
            // Le texte se recompose dans sa nouvelle boîte : quelques
            // microsecondes, donc on le voit couler pendant le geste.
            if let Some(live) = a.live.as_ref() {
                let styles = (!a.styles.uniform()).then_some(&a.styles);
                let laid = live.lay_styled(&a.frame, &a.buffer.text, styles);
                a.map = laid.caret.clone();
                a.laid = Some(laid);
            }
            a.dirty = true;
        }
    }

    /// Repères d'alignement : bords des autres blocs de la page qui tombent
    /// à moins de quelques points de celui qu'on déplace.
    ///
    /// C'est ce qui permet d'aligner un bloc sur un autre sans viser au
    /// pixel — et la ligne qui s'affiche dit **pourquoi** il s'est aimanté.
    fn guides(&mut self, pt: Point) -> (Option<f64>, Option<f64>) {
        let Some(a) = self.edit.as_ref().and_then(|m| m.active.as_ref()) else {
            return (None, None);
        };
        let Some(drag) = a.drag.as_ref() else {
            return (None, None);
        };
        if drag.handle.is_some() {
            return (None, None);
        }
        let (page, bbox) = (a.page, drag.box_start);
        let (dx, dy) = (pt.x - drag.from.x, pt.y - drag.from.y);
        let (x0, y1) = (bbox.x0 + dx, bbox.y1 + dy);
        let tol = 4.0;
        let mut best_x: Option<(f64, f64)> = None;
        let mut best_y: Option<(f64, f64)> = None;
        for unit in self.units(page).iter() {
            if (unit.bbox.x0 - bbox.x0).abs() < 0.01 && (unit.bbox.y1 - bbox.y1).abs() < 0.01 {
                continue;
            }
            for candidate in [unit.bbox.x0, unit.bbox.x1] {
                let d = (candidate - x0).abs();
                if d < tol && best_x.is_none_or(|(_, b)| d < b) {
                    best_x = Some((candidate, d));
                }
            }
            for candidate in [unit.bbox.y1, unit.bbox.y0] {
                let d = (candidate - y1).abs();
                if d < tol && best_y.is_none_or(|(_, b)| d < b) {
                    best_y = Some((candidate, d));
                }
            }
        }
        (best_x.map(|(v, _)| v), best_y.map(|(v, _)| v))
    }

    /// Ouvre un paragraphe existant, curseur au point cliqué.
    fn open_existing(&mut self, page: usize, index: Option<usize>, pt: Point) {
        // Une saisie ouverte ailleurs se referme, et passe au document.
        self.close_active();
        let opened = {
            let Some(l) = self.loaded.as_mut() else {
                return;
            };
            let text = l.text(page).0.clone();
            let Some(p) = l.pages.get(page) else { return };
            // Le bloc entier d'abord ; s'il refuse — ses lignes sont mêlées à
            // d'autres dans le flux, ce qui arrive souvent aux documents d'un
            // traitement de texte — on retombe sur **la ligne cliquée**, qui
            // se recompose seule. Mieux vaut modifier une ligne que rien.
            let whole = index.map(|i| open_paragraph(&l.doc, p, &text, i));
            match whole {
                Some(Ok(o)) => Ok(o),
                other => {
                    let line = line_at(&text, pt.x, pt.y)
                        .and_then(|i| line_unit(&text, i))
                        .map(|unit| open_unit(&l.doc, p, &text, &unit));
                    match (line, other) {
                        (Some(Ok(o)), _) => Ok(o),
                        (Some(Err(e)), _) | (None, Some(Err(e))) => Err(e),
                        (None, _) => Err(acrux_core::Error::Unsupported(
                            "aucun texte modifiable ici".into(),
                        )),
                    }
                }
            }
        };
        match opened {
            Ok(o) => {
                let mut buffer = Buffer::new(&o.text);
                buffer.move_to(o.caret.nearest(pt.x, pt.y), false);
                // La police du bloc, chargée une fois : c'est elle qui rendra
                // la frappe immédiate. Si elle se dérobe, la saisie repassera
                // par le document à chaque lettre — plus lent, mais sûr.
                let live = self.open_live_styled(page, &o.frame, &o.styles);
                // Ce qu'il faudra masquer : la place qu'occupe le texte
                // d'origine, tant qu'il est encore sur la page.
                let cover = o.caret.bounds();
                let paper = cover.map_or((0xFF, 0xFF, 0xFF), |r| self.paper_around(page, r));
                if let Some(mode) = &mut self.edit {
                    mode.active = Some(Active {
                        page,
                        origin: o.frame.clone(),
                        frame: o.frame,
                        original: o.drawn.clone(),
                        drawn: o.drawn,
                        buffer,
                        map: o.caret,
                        history: None,
                        selecting: true,
                        live,
                        laid: None,
                        cover,
                        paper,
                        dirty: false,
                        mask: None,
                        styles: o.styles,
                        styled_from: o.text.clone(),
                        drag: None,
                        hover: None,
                        undo: Vec::new(),
                        redo: Vec::new(),
                        last_step: StepKind::None,
                        last_caret: 0,
                        writing: true,
                    });
                }
            }
            Err(e) => self.set_notice(format!("ce texte ne peut pas être modifié : {e}")),
        }
        self.sync_bar();
    }

    /// Texte lu dans les images d'une page, lu une fois pour toutes.
    fn image_text(&mut self, page: usize) -> std::rc::Rc<Vec<ocr::ImageText>> {
        if let Some(read) = self.edit.as_ref().and_then(|e| e.read.get(&page)) {
            return std::rc::Rc::clone(read);
        }
        let read = std::rc::Rc::new(
            self.loaded
                .as_ref()
                .and_then(|l| l.pages.get(page).map(|p| (&l.doc, p)))
                .and_then(|(doc, p)| ocr::read_page(doc, p).ok())
                .unwrap_or_default(),
        );
        if let Some(mode) = &mut self.edit {
            mode.read.insert(page, std::rc::Rc::clone(&read));
        }
        read
    }

    /// Ouvre la ligne de texte **lue dans une image** sous le point cliqué.
    ///
    /// C'est ce qui rend modifiable un document scanné : le texte n'est pas
    /// dans le fichier, il est dessiné dans l'image. On le reconnaît, on le
    /// donne à modifier comme du vrai texte, et l'écriture couvrira l'ancien.
    ///
    /// Rend vrai si une ligne a été ouverte.
    fn open_image_line(&mut self, page: usize, pt: Point) -> bool {
        if !self.image_under(page, pt) {
            return false;
        }
        let first = !self
            .edit
            .as_ref()
            .is_some_and(|e| e.read.contains_key(&page));
        if first {
            self.set_notice(crate::ui::lang::tr("lecture du texte de l'image...").into());
        }
        let read = self.image_text(page);
        let Some(line) = ocr::line_at(&read, pt.x, pt.y).cloned() else {
            if first {
                self.set_notice(
                    crate::ui::lang::tr("le texte de cette image n'a pas pu etre relu").into(),
                );
            }
            return false;
        };
        let crop = self
            .loaded
            .as_ref()
            .and_then(|l| l.pages.get(page).map(|p| p.crop_box(&l.doc)));
        // La boite d'ecriture part de la ligne lue et court jusqu'au bord de
        // la page : le texte tape pourra s'allonger.
        let right = crop.map_or(line.bbox.x1, |c| c.x1 - 18.0);
        let frame = ParagraphFrame {
            x0: line.bbox.x0,
            width: (right - line.bbox.x0).max(line.bbox.x1 - line.bbox.x0),
            alignment: acrux_features::text::Alignment::Left,
            first_line_indent: 0.0,
            line_spacing: line.size * 1.2,
            baseline: line.baseline,
            size: line.size,
            font: acrux_document::Name::new("AcruxOcr"),
            // La graisse et l'italique passent par le nom de la famille :
            // c'est ainsi que la police standard la plus proche est choisie.
            standard: Some(match (line.bold, line.italic) {
                (true, true) => format!("{} Bold Italic", line.family),
                (true, false) => format!("{} Bold", line.family),
                (false, true) => format!("{} Italic", line.family),
                (false, false) => line.family.clone(),
            }),
            color: line.color,
            ink: None,
            face: None,
            // Le texte lu dans une image est posé droit : on ne sait pas
            // encore relever l'inclinaison d'un scan de travers.
            rotation: 0.0,
        };
        let margin = line.size * 0.22;
        let cover = Rect::new(
            line.bbox.x0 - margin,
            line.bbox.y0 - margin * 0.5,
            line.bbox.x1 + margin,
            line.bbox.y1 + margin * 0.5,
        );
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let paper = (
            (line.background[0] * 255.0).clamp(0.0, 255.0) as u8,
            (line.background[1] * 255.0).clamp(0.0, 255.0) as u8,
            (line.background[2] * 255.0).clamp(0.0, 255.0) as u8,
        );
        let police = self.open_live(page, &frame);
        let map = police
            .as_ref()
            .map(|f| f.lay(&frame, &line.text).caret)
            .unwrap_or_default();
        let mut buffer = Buffer::new(&line.text);
        buffer.move_to(map.nearest(pt.x, pt.y), false);
        if let Some(mode) = &mut self.edit {
            mode.active = Some(Active {
                page,
                origin: frame.clone(),
                frame,
                // La page ne porte aucun texte ici : il est dans l'image.
                original: String::new(),
                drawn: String::new(),
                buffer,
                map,
                history: None,
                selecting: true,
                live: police,
                laid: None,
                cover: Some(cover),
                paper,
                dirty: false,
                mask: Some((cover, line.background)),
                styles: Styles::default(),
                styled_from: String::new(),
                drag: None,
                hover: None,
                undo: Vec::new(),
                redo: Vec::new(),
                last_step: StepKind::None,
                last_caret: 0,
                writing: true,
            });
        }
        self.sync_bar();
        self.set_notice(crate::ui::lang::tr("texte de l'image reconnu : modifiez-le").into());
        true
    }

    /// Vrai si une image de la page couvre ce point.
    fn image_under(&self, page: usize, pt: Point) -> bool {
        use acrux_features::edit_objects::{list, Kind};
        self.loaded
            .as_ref()
            .and_then(|l| l.pages.get(page).map(|p| (&l.doc, p)))
            .and_then(|(doc, p)| list(doc, p).ok())
            .is_some_and(|objects| {
                objects.iter().any(|o| {
                    matches!(o.kind, Kind::Image | Kind::InlineImage)
                        && o.bbox.x0 <= pt.x
                        && pt.x <= o.bbox.x1
                        && o.bbox.y0 <= pt.y
                        && pt.y <= o.bbox.y1
                })
            })
    }

    /// Charge la police d'un bloc pour la frappe en direct.
    ///
    /// Un bloc **en biais** n'y a pas droit : l'aperçu le dessinerait droit,
    /// et l'on ne verrait pas ce qu'on obtient. Sa frappe repasse donc par le
    /// document, qui le rend dans sa direction.
    fn open_live(&mut self, page: usize, frame: &ParagraphFrame) -> Option<LiveText> {
        if frame.rotation.abs() > 1e-4 {
            return None;
        }
        let l = self.loaded.as_ref()?;
        let page_ref = l.pages.get(page)?;
        LiveText::open(&l.doc, page_ref, frame).ok()
    }

    /// Idem, en chargeant aussi les polices des **styles** du bloc.
    fn open_live_styled(
        &mut self,
        page: usize,
        frame: &ParagraphFrame,
        styles: &Styles,
    ) -> Option<LiveText> {
        if frame.rotation.abs() > 1e-4 {
            return None;
        }
        let l = self.loaded.as_ref()?;
        let page_ref = l.pages.get(page)?;
        LiveText::open_styled(&l.doc, page_ref, frame, styles).ok()
    }

    /// Couleur du papier autour d'une boîte, relevée sur la page rendue.
    ///
    /// Masquer le texte d'origine demande de savoir ce qu'il y a dessous ; on
    /// le lit donc sur l'image de la page, dans une bande juste au-dessus et
    /// au-dessous du bloc. La couleur la plus fréquente gagne : une rayure de
    /// tableau ou un filet ne l'emporte pas sur le fond.
    fn paper_around(&self, page: usize, rect: Rect) -> (u8, u8, u8) {
        let white = (0xFF, 0xFF, 0xFF);
        let scale = self.scale();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let key = (scale * 1000.0).round() as u32;
        let Some(l) = self.loaded.as_ref() else {
            return white;
        };
        let (Some(bitmap), Some(page_ref)) = (l.cache.get(&(page, key)), l.pages.get(page)) else {
            return white;
        };
        let (pw, ph) = (bitmap.width(), bitmap.height());
        let m = base_matrix(
            &page_ref.crop_box(&l.doc),
            scale,
            shown_rotation(l, page_ref),
            pw,
            ph,
        );
        let dev = m.transform_rect(&rect);
        let data = bitmap.data();
        let mut tally: HashMap<(u8, u8, u8), u32> = HashMap::new();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let band = |y: f64, tally: &mut HashMap<(u8, u8, u8), u32>| {
            if y < 0.0 || y >= f64::from(ph) {
                return;
            }
            let row = y as u32;
            let x0 = (dev.x0.floor().max(0.0) as u32).min(pw);
            let x1 = (dev.x1.ceil().max(0.0) as u32).min(pw);
            for col in x0..x1 {
                let i = ((row * pw + col) * 4) as usize;
                if i + 2 < data.len() {
                    *tally
                        .entry((data[i + 2], data[i + 1], data[i]))
                        .or_default() += 1;
                }
            }
        };
        for step in 1..=3 {
            band(dev.y0.floor() - f64::from(step), &mut tally);
            band(dev.y1.ceil() + f64::from(step), &mut tally);
        }
        tally
            .into_iter()
            .max_by_key(|(_, n)| *n)
            .map_or(white, |(c, _)| c)
    }

    /// Ouvre une zone de texte neuve au point cliqué.
    fn open_new_box(&mut self, page: usize, pt: Point, window: &mut dyn WindowHandle) {
        self.close_active();
        let Some(style) = self.edit.as_ref().map(|e| NewTextStyle {
            size: e.bar.size_set.then_some(e.bar.size),
            color: e.bar.color_set.then(|| e.bar.rgb()),
        }) else {
            return;
        };
        let opened = {
            let Some(l) = self.loaded.as_mut() else {
                return;
            };
            let text = l.text(page).0.clone();
            let Some(p) = l.pages.get(page) else { return };
            // Hors de la page, rien à poser.
            let crop = p.crop_box(&l.doc);
            if pt.x < crop.x0 || pt.x > crop.x1 || pt.y < crop.y0 || pt.y > crop.y1 {
                return;
            }
            // Police, corps et couleur du texte voisin ; posée sur la ligne
            // de champ s'il y en a une sous le clic.
            let frame = text_frame_at(&l.doc, p, &text, pt.x, pt.y, style);
            set_paragraph_text(&l.doc, p, &frame, "", "").map(|map| (frame, map))
        };
        match opened {
            Ok((frame, map)) => {
                let live = self.open_live(page, &frame);
                if let Some(mode) = &mut self.edit {
                    mode.active = Some(Active {
                        page,
                        origin: frame.clone(),
                        frame,
                        original: String::new(),
                        drawn: String::new(),
                        buffer: Buffer::new(""),
                        map,
                        history: None,
                        selecting: false,
                        live,
                        laid: None,
                        cover: None,
                        paper: (0xFF, 0xFF, 0xFF),
                        dirty: false,
                        mask: None,
                        styles: Styles::default(),
                        styled_from: String::new(),
                        drag: None,
                        hover: None,
                        undo: Vec::new(),
                        redo: Vec::new(),
                        last_step: StepKind::None,
                        last_caret: 0,
                        writing: true,
                    });
                    // Une zone posée, on revient à l'édition : c'est ce qu'on
                    // fait ensuite neuf fois sur dix.
                    mode.bar.tool = EditTool::Select;
                }
            }
            Err(e) => self.set_notice(format!("zone de texte impossible : {e}")),
        }
        self.sync_bar();
        window.request_redraw();
    }

    /// Mouvement de souris en mode édition : sélection au glisser, cadre
    /// survolé, forme du pointeur.
    pub(super) fn edit_mouse_move(
        &mut self,
        x: i32,
        y: i32,
        dragging: bool,
        window: &mut dyn WindowHandle,
    ) {
        let hit = self.page_at(x, y);
        // Un geste sur le cadre l'emporte sur tout le reste : le texte s'y
        // recompose à chaque pas, sans que rien ne soit écrit.
        let gesturing = self
            .edit
            .as_ref()
            .and_then(|e| e.active.as_ref())
            .is_some_and(|a| a.drag.is_some());
        if dragging && gesturing {
            let page = self
                .edit
                .as_ref()
                .and_then(|e| e.active.as_ref())
                .map(|a| a.page);
            if let Some(pt) = page.and_then(|page| self.point_in_page(page, x, y)) {
                self.gesture_to(pt);
                window.request_redraw();
            }
            return;
        }
        if !dragging {
            // Survol des poignées : le pointeur dit ce qu'on peut saisir.
            let over = self
                .active_view_rect()
                .and_then(|v| objects_ui::handle_at(v, f64::from(x), f64::from(y), self.dpi_scale))
                .filter(|h| *h != Handle::Body);
            let changed = self
                .edit
                .as_ref()
                .and_then(|e| e.active.as_ref())
                .is_some_and(|a| a.hover != over);
            if let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) {
                a.hover = over;
            }
            if changed {
                window.request_redraw();
            }
            if let Some(handle) = over {
                window.set_cursor(handle.cursor());
                return;
            }
            // Sur le bord du cadre : on déplace le bloc.
            let on_border = hit
                .filter(|(page, _)| {
                    self.edit
                        .as_ref()
                        .and_then(|e| e.active.as_ref())
                        .is_some_and(|a| a.page == *page)
                })
                .zip(self.active_box())
                .is_some_and(|((_, pt), r)| {
                    let m = 2.5;
                    let outer = pt.x >= r.x0 - m
                        && pt.x <= r.x1 + m
                        && pt.y >= r.y0 - m
                        && pt.y <= r.y1 + m;
                    let inner =
                        pt.x > r.x0 + m && pt.x < r.x1 - m && pt.y > r.y0 + m && pt.y < r.y1 - m;
                    outer && !inner
                });
            if on_border {
                window.set_cursor(Cursor::Move);
                return;
            }
            // Un bloc posé se saisit tout entier : le pointeur le dit.
            if self.block_posed() {
                let inside = hit.zip(self.active_box()).is_some_and(|((_, pt), r)| {
                    pt.x >= r.x0 && pt.x <= r.x1 && pt.y >= r.y0 && pt.y <= r.y1
                });
                if inside {
                    window.set_cursor(Cursor::Move);
                    return;
                }
            }
        }
        self.edit_hover(x, y, dragging, window);
    }

    /// Survol hors du cadre : bloc signalé, forme du pointeur.
    fn edit_hover(&mut self, x: i32, y: i32, dragging: bool, window: &mut dyn WindowHandle) {
        let hit = self.page_at(x, y);
        if dragging {
            if let (Some((page, pt)), Some(a)) =
                (hit, self.edit.as_mut().and_then(|e| e.active.as_mut()))
            {
                if a.selecting && a.page == page {
                    let index = a.map.nearest(pt.x, pt.y);
                    a.buffer.move_to(index, true);
                    window.request_redraw();
                }
            }
            return;
        }
        let tool = self.edit_tool();
        let over = hit.and_then(|(page, pt)| self.paragraph_at(page, pt).map(|(_, r)| (page, r)));
        let changed = self.edit.as_ref().is_some_and(|e| e.hover != over);
        if let Some(mode) = &mut self.edit {
            mode.hover = over;
        }
        let in_active = hit.is_some_and(|(page, pt)| {
            self.edit
                .as_ref()
                .and_then(|e| e.active.as_ref())
                .filter(|a| a.page == page)
                .and_then(|a| a.map.bounds())
                .is_some_and(|r| {
                    let m = 4.0;
                    pt.x >= r.x0 - m && pt.x <= r.x1 + m && pt.y >= r.y0 - m && pt.y <= r.y1 + m
                })
        });
        let on_page = hit.is_some_and(|(page, pt)| {
            self.loaded
                .as_ref()
                .and_then(|l| l.pages.get(page).map(|p| p.crop_box(&l.doc)))
                .is_some_and(|c| pt.x >= c.x0 && pt.x <= c.x1 && pt.y >= c.y0 && pt.y <= c.y1)
        });
        // Comme dans Acrobat : la barre de texte sur du texte, le pointeur
        // « ajouter du texte » là où un clic posera une zone neuve.
        let cursor = if tool == Some(EditTool::AddText) && !in_active && on_page {
            Cursor::AddText
        } else if over.is_some() || in_active {
            Cursor::IBeam
        } else if on_page {
            Cursor::AddText
        } else {
            Cursor::Arrow
        };
        window.set_cursor(cursor);
        if changed {
            window.request_redraw();
        }
    }

    /// Bouton relâché : fin d'une sélection au glisser.
    pub(super) fn edit_mouse_up(&mut self) {
        if let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) {
            a.selecting = false;
            // Le geste s'arrête là ; ce qu'il a changé partira au document
            // avec le reste, en sortant du bloc.
            a.drag = None;
        }
    }

    /// Touche en mode édition. Rend vrai si elle a été prise.
    pub(super) fn edit_key(
        &mut self,
        key: Key,
        m: Modifiers,
        window: &mut dyn WindowHandle,
    ) -> bool {
        if !self.edit_on() {
            return false;
        }
        // Un bloc posé prend les flèches, Suppr et Entrée pour lui-même.
        if self.posed_key(key, m, window) {
            return true;
        }
        if !self.editing_text() {
            if key == Key::Escape {
                self.leave_edit();
                window.request_redraw();
                return true;
            }
            return false;
        }
        let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) else {
            return false;
        };
        let before = a.buffer.clone();
        let mut changed = false;
        match key {
            Key::Left if m.ctrl => a.buffer.word_left(m.shift),
            Key::Right if m.ctrl => a.buffer.word_right(m.shift),
            Key::Left => a.buffer.left(m.shift),
            Key::Right => a.buffer.right(m.shift),
            Key::Up => {
                let i = a.map.vertical(a.buffer.caret, -1);
                a.buffer.move_to(i, m.shift);
            }
            Key::Down => {
                let i = a.map.vertical(a.buffer.caret, 1);
                a.buffer.move_to(i, m.shift);
            }
            Key::Home if m.ctrl => a.buffer.move_to(0, m.shift),
            Key::End if m.ctrl => {
                let n = a.buffer.len();
                a.buffer.move_to(n, m.shift);
            }
            Key::Home => {
                let (first, _) = a.map.line_bounds(a.buffer.caret);
                a.buffer.move_to(first, m.shift);
            }
            Key::End => {
                let (_, last) = a.map.line_bounds(a.buffer.caret);
                a.buffer.move_to(last, m.shift);
            }
            Key::Backspace => {
                a.buffer.backspace();
                changed = true;
            }
            Key::Delete => {
                a.buffer.delete();
                changed = true;
            }
            Key::Enter => {
                a.buffer.insert("\n");
                changed = true;
            }
            Key::Escape => {
                // Échap pose le bloc ; une seconde fois, il se referme.
                let writing = self
                    .edit
                    .as_ref()
                    .and_then(|e| e.active.as_ref())
                    .is_some_and(|a| a.writing);
                if writing {
                    self.pose_block();
                } else {
                    self.close_active();
                }
                window.request_redraw();
                return true;
            }
            // L'espace et la tabulation arrivent aussi en caractères : la
            // touche elle-même ne doit ni faire défiler, ni changer de champ.
            Key::Space | Key::Tab => return true,
            _ => return false,
        }
        if let Some(mode) = &mut self.edit {
            mode.blink = Instant::now();
        }
        if changed {
            self.edit_apply(before, window);
        }
        window.request_redraw();
        true
    }

    /// Caractère tapé en mode édition. Rend vrai s'il a été pris.
    pub(super) fn edit_char(
        &mut self,
        c: char,
        m: Modifiers,
        window: &mut dyn WindowHandle,
    ) -> bool {
        if !self.editing_text() {
            return false;
        }
        // Copier ou couper le texte d'un bloc, c'est en extraire le contenu :
        // le droit de modifier ne donne pas celui de copier, et ce que les
        // permissions refusent à Ctrl+C sur la page, elles le refusent ici.
        let copying = m.ctrl
            && matches!(c, 'c' | 'C' | '\u{3}' | 'x' | 'X' | '\u{18}')
            && self
                .edit
                .as_ref()
                .and_then(|e| e.active.as_ref())
                .is_some_and(|a| !a.buffer.selected().is_empty());
        if copying && !self.rights().copy {
            self.set_notice(
                crate::ui::lang::tr("copie interdite par les permissions du document").into(),
            );
            window.request_redraw();
            return true;
        }
        let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) else {
            return false;
        };
        let before = a.buffer.clone();
        let changed = if m.ctrl {
            match c {
                'a' | 'A' | '\u{1}' => {
                    a.buffer.select_all();
                    false
                }
                'c' | 'C' | '\u{3}' => {
                    let s = a.buffer.selected();
                    if !s.is_empty() {
                        window.set_clipboard_text(&s);
                    }
                    false
                }
                'x' | 'X' | '\u{18}' => {
                    let s = a.buffer.selected();
                    if s.is_empty() {
                        false
                    } else {
                        window.set_clipboard_text(&s);
                        a.buffer.insert("");
                        true
                    }
                }
                'v' | 'V' | '\u{16}' => match window.clipboard_text() {
                    Some(s) if !s.is_empty() => {
                        a.buffer.insert(&s);
                        true
                    }
                    _ => false,
                },
                // Ctrl+Z défait d'abord **dans le bloc**, étape par étape,
                // sans refermer la saisie. Quand il n'y a plus rien à y
                // défaire, la main passe à l'annulation du document.
                'z' | 'Z' | '\u{1a}' => {
                    if self.undo_step(window) {
                        return true;
                    }
                    self.close_active();
                    return false;
                }
                'y' | 'Y' | '\u{19}' => {
                    if self.redo_step(window) {
                        return true;
                    }
                    self.close_active();
                    return false;
                }
                _ => return true,
            }
        } else {
            if c.is_control() {
                return true;
            }
            a.buffer.insert(&c.to_string());
            true
        };
        if let Some(mode) = &mut self.edit {
            mode.blink = Instant::now();
        }
        if changed {
            self.edit_apply(before, window);
        }
        window.request_redraw();
        true
    }

    /// Met la barre au diapason du bloc ouvert : sa police, son alignement,
    /// son interligne. Sans cela, les boutons montreraient l'état d'un autre
    /// bloc, ou celui d'aucun.
    fn sync_bar(&mut self) {
        let state = self.edit.as_ref().and_then(|e| e.active.as_ref()).map(|a| {
            use acrux_features::text::Alignment;
            let align = match a.frame.alignment {
                Alignment::Center => 1,
                Alignment::Right => 2,
                Alignment::Justify => 3,
                Alignment::Left => 0,
            };
            let leading = if a.frame.size > 0.1 {
                (a.frame.line_spacing / a.frame.size).clamp(0.6, 3.0)
            } else {
                1.2
            };
            let face = a.frame.face.clone();
            // La police d'origine du bloc, dite comme un lecteur la dirait.
            let native = a
                .live
                .as_ref()
                .filter(|_| face.is_none())
                .map(|l| acrux_features::sysfonts::describe(l.family()));
            (align, leading, face, a.frame.size, a.frame.color, native)
        });
        if let Some(mode) = &mut self.edit {
            if let Some((align, leading, face, size, color, native)) = state {
                {
                    mode.bar.editing = true;
                    // La pastille montre l'encre du bloc ; un bloc neuf, lui,
                    // a déjà pris celle de la barre.
                    mode.bar.color = color;
                    mode.bar.align = align;
                    mode.bar.leading = leading;
                    mode.bar.active_size = Some(size);
                    // Sans police imposée, la barre dit ce que le document
                    // donne au bloc : sa famille, son gras, son italique.
                    mode.bar.bold = face
                        .as_ref()
                        .map_or(native.as_ref().is_some_and(|n| n.1), |f| f.bold);
                    mode.bar.italic = face
                        .as_ref()
                        .map_or(native.as_ref().is_some_and(|n| n.2), |f| f.italic);
                    if let Some((name, _, _)) = native {
                        mode.bar.native = Some(name);
                    } else if face.is_none() {
                        mode.bar.native = None;
                    }
                    mode.bar.family = face.as_ref().and_then(|f| {
                        f.family.as_ref().and_then(|name| {
                            acrux_features::sysfonts::families()
                                .iter()
                                .position(|c| c.name.eq_ignore_ascii_case(name))
                        })
                    });
                }
            } else {
                mode.bar.editing = false;
                mode.bar.active_size = None;
                mode.bar.family = None;
                mode.bar.native = None;
                mode.bar.bold = false;
                mode.bar.italic = false;
            }
        }
    }

    /// Police choisie dans la liste, appliquée au bloc ouvert.
    ///
    /// Comme dans Acrobat : la police change pour **tout** le bloc, et se
    /// voit aussitôt.
    fn set_family(&mut self, choice: Option<usize>) {
        if let Some(mode) = &mut self.edit {
            mode.bar.family = choice;
            mode.bar.close_menu();
        }
        if let Some(family) = choice.and_then(|i| acrux_features::sysfonts::families().get(i)) {
            crate::ui::pickers::remember_font(&family.name);
        }
        self.apply_face();
    }

    /// Vrai si un sélecteur de la barre est déroulé : il prend les clics.
    pub(super) fn edit_menu_open(&self) -> bool {
        self.edit.as_ref().is_some_and(|e| e.bar.menu_open())
    }

    /// Bascule la graisse ou l'italique du bloc ouvert.
    fn toggle_face(&mut self, bold: bool) {
        if let Some(mode) = &mut self.edit {
            if bold {
                mode.bar.bold = !mode.bar.bold;
            } else {
                mode.bar.italic = !mode.bar.italic;
            }
        }
        self.apply_face();
    }

    /// Reporte la police choisie sur le bloc, et le redessine.
    fn apply_face(&mut self) {
        let choice = self
            .edit
            .as_ref()
            .map(|e| acrux_features::edit_text::FaceChoice {
                family: e
                    .bar
                    .family
                    .and_then(|i| acrux_features::sysfonts::families().get(i))
                    .map(|f| f.name.clone())
                    // « Police du texte » : la famille d'origine, pour que
                    // retirer le gras d'un titre gras rende bien son romain.
                    .or_else(|| e.bar.native.clone()),
                bold: e.bar.bold,
                italic: e.bar.italic,
            });
        let Some(choice) = choice else { return };
        let (page, frame) = {
            let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) else {
                return;
            };
            a.frame.face = Some(choice);
            // Une police imposée vaut pour tout le bloc : ses styles d'origine
            // ne s'appliquent plus.
            a.styles = acrux_features::edit_text::Styles::default();
            a.dirty = true;
            (a.page, a.frame.clone())
        };
        // La police de l'aperçu change avec elle.
        let live = self.open_live(page, &frame);
        if let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) {
            a.live = live;
        }
        self.relay_active();
    }

    /// Change l'alignement du bloc ouvert.
    fn set_alignment(&mut self, index: usize) {
        use acrux_features::text::Alignment;
        let alignment = match index {
            1 => Alignment::Center,
            2 => Alignment::Right,
            3 => Alignment::Justify,
            _ => Alignment::Left,
        };
        if let Some(mode) = &mut self.edit {
            mode.bar.align = index;
        }
        if let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) {
            a.frame.alignment = alignment;
            a.dirty = true;
        }
        self.relay_active();
    }

    /// Resserre ou élargit l'interligne du bloc ouvert.
    fn change_leading(&mut self, delta: f64) {
        let leading = self
            .edit
            .as_ref()
            .map_or(1.2, |e| (e.bar.leading + delta).clamp(0.6, 3.0));
        if let Some(mode) = &mut self.edit {
            mode.bar.leading = leading;
        }
        if let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) {
            a.frame.line_spacing = a.frame.size * leading;
            a.dirty = true;
        }
        self.relay_active();
    }

    /// Remet le bloc en page après un changement de mise en forme.
    fn relay_active(&mut self) {
        if let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) {
            if let Some(live) = a.live.as_ref() {
                let styles = (!a.styles.uniform()).then_some(&a.styles);
                let laid = live.lay_styled(&a.frame, &a.buffer.text, styles);
                a.map = laid.caret.clone();
                a.laid = Some(laid);
            }
        }
        self.title_dirty = true;
    }

    /// Prend une frappe.
    ///
    /// Deux chemins. **En direct** quand la police du bloc est chargée et
    /// connaît tous les caractères tapés : rien n'est écrit, le texte est mis
    /// en page en mémoire et dessiné par l'application. **Par le document**
    /// sinon — police introuvable, ou caractère à ajouter à la police, ce que
    /// seul le document sait faire.
    ///
    /// Les deux se suivent sans heurt : tant qu'on tape en direct, la page
    /// porte toujours le texte d'origine, et c'est lui que l'écriture ira
    /// chercher.
    fn edit_apply(&mut self, before: Buffer, window: &mut dyn WindowHandle) {
        self.note_step(&before);
        // On tape en direct dès que le bloc sait se dessiner — y compris
        // avec les glyphes prêtés par la police système. Seul un texte qu'on
        // ne saurait pas montrer repasse par le document.
        let live = self
            .edit
            .as_ref()
            .and_then(|e| e.active.as_ref())
            .is_some_and(|a| a.live.as_ref().is_some_and(|f| f.can_draw(&a.buffer.text)));
        if live {
            self.edit_live(window);
        } else {
            self.edit_written(before, window);
        }
    }

    /// Retient ce qu'était le texte avant ce geste, si le geste ouvre une
    /// nouvelle étape.
    ///
    /// Taper « bonjour » d'une traite fait **une** étape ; l'espace qui suit
    /// en ouvre une autre, comme dans n'importe quel traitement de texte. Un
    /// effacement après une frappe en ouvre une aussi : on ne veut pas
    /// défaire les deux d'un coup.
    fn note_step(&mut self, before: &Buffer) {
        let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) else {
            return;
        };
        let (kind, opens) = step_of(before, &a.buffer, a.last_step, a.last_caret);
        if opens || a.undo.is_empty() {
            a.undo.push(Step {
                text: before.text.clone(),
                caret: before.caret,
                anchor: before.anchor,
            });
            // Une trentaine d'étapes couvrent une séance de frappe ; au-delà,
            // c'est l'annulation du document qui prend le relais.
            if a.undo.len() > 60 {
                a.undo.remove(0);
            }
        }
        a.redo.clear();
        a.last_step = kind;
        a.last_caret = a.buffer.caret;
    }

    /// Ce que le bloc en cours de saisie a à défaire et à refaire, dans cet
    /// ordre. `(false, false)` quand aucun bloc n'est ouvert.
    ///
    /// C'est le point où la barre d'outils lit l'état d'« Annuler » et de
    /// « Rétablir » au-delà de l'historique du document : tout futur mode
    /// qui tient sa propre pile d'étapes (saisie dans un formulaire, zone de
    /// texte ajoutée) doit s'y ajouter, sans quoi les boutons resteraient
    /// grisés pendant qu'on y tape.
    pub(super) fn block_history(&self) -> (bool, bool) {
        let (field_undo, field_redo) = self.field_history();
        let (undo, redo) = self
            .edit
            .as_ref()
            .and_then(|e| e.active.as_ref())
            .map_or((false, false), |a| (!a.undo.is_empty(), !a.redo.is_empty()));
        (undo || field_undo, redo || field_redo)
    }

    /// Défait la dernière étape **dans le bloc**, sans fermer la saisie.
    ///
    /// Rend faux quand il n'y a plus rien à défaire ici : c'est alors à
    /// l'annulation du document de jouer.
    pub(super) fn undo_step(&mut self, window: &mut dyn WindowHandle) -> bool {
        // La saisie d'un champ de formulaire a ses propres étapes.
        if self.field_undo_step() {
            window.request_redraw();
            return true;
        }
        let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) else {
            return false;
        };
        let Some(step) = a.undo.pop() else {
            return false;
        };
        a.redo.push(Step {
            text: a.buffer.text.clone(),
            caret: a.buffer.caret,
            anchor: a.buffer.anchor,
        });
        a.buffer.text = step.text;
        a.buffer.caret = step.caret.min(a.buffer.text.chars().count());
        a.buffer.anchor = step.anchor.min(a.buffer.text.chars().count());
        a.last_step = StepKind::None;
        a.last_caret = a.buffer.caret;
        self.after_step(window);
        true
    }

    /// Refait l'étape défaite.
    pub(super) fn redo_step(&mut self, window: &mut dyn WindowHandle) -> bool {
        if self.field_redo_step() {
            window.request_redraw();
            return true;
        }
        let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) else {
            return false;
        };
        let Some(step) = a.redo.pop() else {
            return false;
        };
        a.undo.push(Step {
            text: a.buffer.text.clone(),
            caret: a.buffer.caret,
            anchor: a.buffer.anchor,
        });
        a.buffer.text = step.text;
        a.buffer.caret = step.caret.min(a.buffer.text.chars().count());
        a.buffer.anchor = step.anchor.min(a.buffer.text.chars().count());
        a.last_step = StepKind::None;
        a.last_caret = a.buffer.caret;
        self.after_step(window);
        true
    }

    /// Remet le bloc en page après une annulation locale.
    fn after_step(&mut self, window: &mut dyn WindowHandle) {
        if let Some(mode) = &mut self.edit {
            mode.blink = Instant::now();
        }
        let live = self
            .edit
            .as_ref()
            .and_then(|e| e.active.as_ref())
            .is_some_and(|a| a.live.as_ref().is_some_and(|f| f.can_draw(&a.buffer.text)));
        if live {
            self.edit_live(window);
        } else {
            let before = self
                .edit
                .as_ref()
                .and_then(|e| e.active.as_ref())
                .map(|a| a.buffer.clone());
            if let Some(before) = before {
                self.edit_written(before, window);
            }
        }
        window.request_redraw();
    }

    /// Frappe en direct : mise en page en mémoire, rien d'écrit.
    fn edit_live(&mut self, window: &mut dyn WindowHandle) {
        if let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) {
            // Les styles suivent le texte : ce qui n'a pas bougé garde le
            // sien, ce qu'on tape prend celui de son voisin de gauche.
            if !a.styles.uniform() {
                a.styles = a.styles.carry(&a.styled_from, &a.buffer.text);
                a.styled_from = a.buffer.text.clone();
            }
            if let Some(live) = a.live.as_ref() {
                let styles = (!a.styles.uniform()).then_some(&a.styles);
                let laid = live.lay_styled(&a.frame, &a.buffer.text, styles);
                a.map = laid.caret.clone();
                a.laid = Some(laid);
                a.dirty = true;
            }
        }
        window.request_redraw();
    }

    /// Écrit dans le document ce qui a été tapé en direct — une fois, à la
    /// sortie du bloc — et l'inscrit à l'historique comme **une** opération.
    ///
    /// Sans effet si rien n'a été tapé : ouvrir un bloc et en ressortir ne
    /// modifie pas le fichier.
    #[allow(clippy::too_many_lines)] // écrire, historiser, rendre : d'un trait
    pub(super) fn commit_active(&mut self) {
        let Some(what) = self
            .edit
            .as_ref()
            .and_then(|e| e.active.as_ref())
            .filter(|a| a.dirty)
            .map(Pending::of)
        else {
            return;
        };
        let Pending {
            page,
            origin,
            frame,
            expected,
            original,
            text,
            slot,
            mask,
            styles,
        } = what;
        // Le texte lu dans une image y est encore dessiné : on le couvre de
        // la couleur du papier avant d'écrire le nouveau par-dessus.
        if let Some((rect, color)) = mask {
            if !self.cover_image(page, rect, color) {
                return;
            }
        }
        let scale = self.scale();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let key_scale = (scale * 1000.0).round() as u32;
        let mut failure = None;
        {
            let Some(l) = self.loaded.as_mut() else {
                return;
            };
            let Some(page_ref) = l.pages.get(page).cloned() else {
                return;
            };
            // Une seule écriture pour toute la séance : le bloc est retrouvé
            // par sa boîte d'origine, et réécrit dans celle où on l'a laissé.
            let moved = frame != origin;
            match move_paragraph_styled(
                &l.doc,
                &page_ref,
                &origin,
                &frame,
                &expected,
                &text,
                styles.as_ref(),
            ) {
                Ok(map) => {
                    let op = EditOp::Paragraph {
                        page,
                        frame: origin.clone(),
                        to: moved.then(|| frame.clone()),
                        expected: original,
                        text: text.clone(),
                    };
                    // Le fil de rendu tient sa propre copie : il reçoit la
                    // saisie entière, d'un coup.
                    if let Some(w) = &mut l.worker {
                        w.edit(op.clone());
                    }
                    match slot {
                        Some(i) if i < l.history.len() => l.history[i] = op,
                        _ => {
                            l.history.push(op);
                            let index = l.history.len() - 1;
                            if let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) {
                                a.history = Some(index);
                            }
                        }
                    }
                    l.redo.clear();
                    l.modified = true;
                    if let Ok(pages) = collect_pages(&l.doc) {
                        l.pages = pages;
                        l.page_index = PageIndex::new(&l.pages);
                    }
                    l.texts.remove(&page);
                    l.cache.retain(|(p, _), _| *p != page);
                    if let Some(p) = l.pages.get(page) {
                        let options = RenderOptions {
                            annotations: true,
                            time_budget: Some(Duration::from_secs(5)),
                            background: Some(Color::WHITE),
                            ..RenderOptions::default()
                        };
                        let bitmap =
                            render_page_rotated(&l.doc, p, scale, l.view_rotation, &options).bitmap;
                        l.cache.insert((page, key_scale), bitmap);
                    }
                    if let Some(mode) = &mut self.edit {
                        mode.units.remove(&page);
                        mode.read.remove(&page);
                        mode.hover = None;
                        if let Some(a) = mode.active.as_mut() {
                            // L'image est couverte une fois pour toutes.
                            a.mask = None;
                            a.drawn = normalized(&text);
                            a.origin = a.frame.clone();
                            a.cover = map.bounds();
                            a.map = map;
                            a.laid = None;
                            a.dirty = false;
                        }
                    }
                }
                Err(e) => failure = Some(format!("modification refusée : {e}")),
            }
        }
        if let Some(message) = failure {
            self.set_notice(message);
        }
        self.title_dirty = true;
    }

    /// Couvre une zone d'image de la couleur du papier.
    ///
    /// Rend faux si le document refuse : la saisie n'est alors pas écrite,
    /// et l'image reste intacte.
    fn cover_image(&mut self, page: usize, rect: Rect, color: [f64; 3]) -> bool {
        let Some(l) = self.loaded.as_ref() else {
            return false;
        };
        let Some(page_ref) = l.pages.get(page).cloned() else {
            return false;
        };
        match ocr::mask(&l.doc, &page_ref, rect, color) {
            Ok(()) => true,
            Err(e) => {
                self.set_notice(format!("couverture impossible : {e}"));
                false
            }
        }
    }

    /// Referme la saisie en cours après l'avoir écrite.
    pub(super) fn close_active(&mut self) {
        self.commit_active();
        if let Some(mode) = &mut self.edit {
            mode.active = None;
        }
        self.sync_bar();
    }

    /// Écrit le texte en cours dans le document, et en tire les
    /// conséquences : historique, fil de rendu, caches, rendu immédiat.
    ///
    /// Si le document refuse, la saisie revient à `before` : ce qu'on voit
    /// reste ce qui est dans le fichier.
    fn edit_written(&mut self, before: Buffer, window: &mut dyn WindowHandle) {
        let scale = self.scale();
        let key_scale = (scale * 1000.0).round() as u32;
        let mut failure = None;
        {
            let (Some(mode), Some(l)) = (self.edit.as_mut(), self.loaded.as_mut()) else {
                return;
            };
            let Some(a) = mode.active.as_mut() else {
                return;
            };
            let Some(page_ref) = l.pages.get(a.page).cloned() else {
                return;
            };
            match set_paragraph_text(&l.doc, &page_ref, &a.frame, &a.drawn, &a.buffer.text) {
                Ok(map) => {
                    // Le fil de rendu tient sa propre copie : il reçoit la
                    // même frappe, dans le même ordre.
                    if let Some(w) = &mut l.worker {
                        w.edit(EditOp::Paragraph {
                            page: a.page,
                            frame: a.frame.clone(),
                            to: None,
                            expected: a.drawn.clone(),
                            text: a.buffer.text.clone(),
                        });
                    }
                    a.cover = map.bounds();
                    a.map = map;
                    a.drawn = normalized(&a.buffer.text);
                    // La page porte de nouveau ce qu'on voit : plus rien à
                    // masquer ni à dessiner nous-mêmes.
                    a.laid = None;
                    a.dirty = false;
                    // Une seule opération par saisie : du texte d'origine au
                    // texte actuel.
                    let op = EditOp::Paragraph {
                        page: a.page,
                        frame: a.frame.clone(),
                        to: None,
                        expected: a.original.clone(),
                        text: a.buffer.text.clone(),
                    };
                    match a.history {
                        Some(i) if i < l.history.len() => l.history[i] = op,
                        _ => {
                            l.history.push(op);
                            a.history = Some(l.history.len() - 1);
                        }
                    }
                    l.redo.clear();
                    l.modified = true;
                    if let Ok(pages) = collect_pages(&l.doc) {
                        l.pages = pages;
                        l.page_index = PageIndex::new(&l.pages);
                    }
                    l.texts.remove(&a.page);
                    mode.units.remove(&a.page);
                    l.cache.retain(|(p, _), _| *p != a.page);
                    // Rendu immédiat de la page : sans lui, la page passerait
                    // par le blanc « rendu en cours » à chaque lettre.
                    if let Some(p) = l.pages.get(a.page) {
                        let options = RenderOptions {
                            annotations: true,
                            time_budget: Some(Duration::from_secs(5)),
                            background: Some(Color::WHITE),
                            ..RenderOptions::default()
                        };
                        let bitmap =
                            render_page_rotated(&l.doc, p, scale, l.view_rotation, &options).bitmap;
                        l.cache.insert((a.page, key_scale), bitmap);
                    }
                }
                Err(e) => {
                    a.buffer = before;
                    failure = Some(format!("modification refusée : {e}"));
                }
            }
        }
        if let Some(message) = failure {
            self.set_notice(message);
        }
        // L'écriture a pu enrichir la police d'un glyphe qui lui manquait :
        // on la recharge, et la frappe repart en direct.
        self.reload_live();
        self.title_dirty = true;
        window.request_redraw();
    }

    /// Recharge la police du bloc en cours de saisie.
    fn reload_live(&mut self) {
        let Some((page, frame)) = self
            .edit
            .as_ref()
            .and_then(|e| e.active.as_ref())
            .map(|a| (a.page, a.frame.clone()))
        else {
            return;
        };
        let live = self.open_live(page, &frame);
        if let Some(a) = self.edit.as_mut().and_then(|e| e.active.as_mut()) {
            a.live = live;
        }
    }
}

/// Un glyphe prêt à tracer : son contour, où le poser, et de quelle encre.
type Traced = (std::rc::Rc<acrux_core::Path>, Matrix, (u8, u8, u8));

/// Ce qu'une saisie a produit et qu'il reste à porter au document.
struct Pending {
    /// Page.
    page: usize,
    /// Boîte d'origine, par laquelle le bloc se retrouve.
    origin: ParagraphFrame,
    /// Boîte d'arrivée, où le texte s'écrit.
    frame: ParagraphFrame,
    /// Ce que la page porte aujourd'hui.
    expected: String,
    /// Ce qu'elle portait à l'ouverture : c'est lui qui va à l'historique.
    original: String,
    /// Texte tapé.
    text: String,
    /// Place de l'opération dans l'historique.
    slot: Option<usize>,
    /// Zone d'image à couvrir avant d'écrire.
    mask: Option<(Rect, [f64; 3])>,
    /// Styles à rendre : police, corps et couleur de chaque caractère.
    styles: Option<Styles>,
}

impl Pending {
    /// Relève ce qu'il y a à écrire dans une saisie en cours.
    fn of(a: &Active) -> Self {
        Self {
            page: a.page,
            origin: a.origin.clone(),
            frame: a.frame.clone(),
            expected: a.drawn.clone(),
            original: a.original.clone(),
            text: a.buffer.text.clone(),
            slot: a.history,
            mask: a.mask,
            styles: (!a.styles.uniform()).then(|| a.styles.clone()),
        }
    }
}

/// Contour d'un rectangle de la vue, élargi d'une marge.
fn outline(frame: &mut Frame<'_>, r: &Rect, margin: f64, width: i32, color: (u8, u8, u8)) {
    let x0 = (r.x0.min(r.x1) - margin).round() as i32;
    let y0 = (r.y0.min(r.y1) - margin).round() as i32;
    let x1 = (r.x0.max(r.x1) + margin).round() as i32;
    let y1 = (r.y0.max(r.y1) + margin).round() as i32;
    let (w, h) = (x1 - x0, y1 - y0);
    if w <= 0 || h <= 0 {
        return;
    }
    frame.fill_rect(x0, y0, w, width, color.0, color.1, color.2);
    frame.fill_rect(x0, y1 - width, w, width, color.0, color.1, color.2);
    frame.fill_rect(x0, y0, width, h, color.0, color.1, color.2);
    frame.fill_rect(x1 - width, y0, width, h, color.0, color.1, color.2);
}

/// Voile coloré sur un rectangle de la vue (sélection).
fn tint(frame: &mut Frame<'_>, r: &Rect, color: (u8, u8, u8), alpha: u32) {
    let x0 = r.x0.min(r.x1).round().max(0.0) as usize;
    let y0 = r.y0.min(r.y1).round().max(0.0) as usize;
    let x1 = (r.x0.max(r.x1).round().max(0.0) as usize).min(frame.width as usize);
    let y1 = (r.y0.max(r.y1).round().max(0.0) as usize).min(frame.height as usize);
    let inv = 255 - alpha;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = frame.index(x, y);
            let d = &mut frame.pixels[i..i + 4];
            d[0] = ((u32::from(color.2) * alpha + u32::from(d[0]) * inv) / 255) as u8;
            d[1] = ((u32::from(color.1) * alpha + u32::from(d[1]) * inv) / 255) as u8;
            d[2] = ((u32::from(color.0) * alpha + u32::from(d[2]) * inv) / 255) as u8;
        }
    }
}

/// Trait fin d'alignement, horizontal ou vertical.
fn guide_line(frame: &mut Frame<'_>, x0: f64, y0: f64, x1: f64, y1: f64, color: (u8, u8, u8)) {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (x0, y0, x1, y1) = (
        x0.round() as i32,
        y0.round() as i32,
        x1.round() as i32,
        y1.round() as i32,
    );
    if (x1 - x0).abs() >= (y1 - y0).abs() {
        frame.fill_rect(
            x0.min(x1),
            y0,
            (x1 - x0).abs().max(1),
            1,
            color.0,
            color.1,
            color.2,
        );
    } else {
        frame.fill_rect(
            x0,
            y0.min(y1),
            1,
            (y1 - y0).abs().max(1),
            color.0,
            color.1,
            color.2,
        );
    }
}
