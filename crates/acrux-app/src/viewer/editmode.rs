//! Le mode « Modifier le PDF » dans le visualiseur : cadres des paragraphes,
//! curseur, sélection, frappe recomposée en direct, zones de texte neuves.
//!
//! La saisie et la barre du mode sont dans [`crate::ui::editpdf`] ; ce qui
//! est ici relie ces morceaux au document, à la mise en page de la fenêtre
//! et au rendu.
//!
//! # Une frappe
//!
//! 1. la saisie change le texte en mémoire ;
//! 2. [`set_paragraph_text`] recompose le paragraphe dans le document, et rend
//!    la position de chaque caractère tel qu'il vient d'être écrit ;
//! 3. la page est rendue **tout de suite**, sur ce fil : attendre le fil de
//!    rendu montrerait une page blanche entre deux lettres ;
//! 4. l'historique garde **une seule** opération par paragraphe, mise à jour
//!    à chaque frappe : annuler défait toute la saisie d'un coup.
//!
//! Une recomposition coûte environ 25 ms sur une vraie page, rendu compris :
//! la frappe suit.

use super::{
    base_matrix, collect_pages, render_page, Color, Cursor, Duration, EditOp, Frame, HashMap,
    Instant, Key, Matrix, Modifiers, PageBox, PageIndex, Point, Rect, RenderOptions, Toolbar,
    Viewer, WindowHandle,
};
use crate::ui::editpdf::{step_size, BarAction, Buffer, EditBar, EditTool};
use acrux_features::edit_text::{
    line_at, line_unit, normalized, open_paragraph, open_unit, set_paragraph_text, text_frame_at,
    text_units, CaretMap, NewTextStyle, ParagraphFrame, TextUnit,
};
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
    /// Écriture au service d'un autre outil (« remplir et signer ») : pas de
    /// barre à soi, et c'est l'autre outil qui commande.
    pub overlay: bool,
}

/// Paragraphe en cours d'édition.
pub(super) struct Active {
    /// Page.
    pub page: usize,
    /// Boîte relevée à l'ouverture, gardée jusqu'au bout.
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
}

impl Drop for EditMode {
    fn drop(&mut self) {
        self.ticking.store(false, Ordering::Relaxed);
    }
}

impl Viewer {
    /// Vrai quand le mode est actif.
    pub(super) fn edit_on(&self) -> bool {
        self.edit.is_some()
    }

    /// Vrai quand un paragraphe reçoit la frappe.
    pub(super) fn editing_text(&self) -> bool {
        self.edit.as_ref().is_some_and(|e| e.active.is_some())
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
        // Un seul outil à la fois : les autres rendent la main.
        self.sign_panel = None;
        self.objects = None;
        self.selection = None;
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
            overlay: false,
        });
        self.set_notice(match tool {
            EditTool::Select => "cliquez dans un texte pour le modifier".into(),
            EditTool::AddText => "cliquez sur la page pour y poser du texte".into(),
        });
    }

    /// Sort du mode. Tout ce qui a été tapé est déjà dans le document.
    pub(super) fn leave_edit(&mut self) {
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
                overlay: true,
            });
        }
        if let Some(mode) = &mut self.edit {
            mode.bar.color_set = true;
            mode.bar.color = crate::ui::editpdf::color_index(color);
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
            mode.bar.paint(frame, text, &theme, dpi, y);
        }
    }

    /// Clic dans la barre du mode. Rend vrai s'il la concernait.
    pub(super) fn edit_bar_click(&mut self, x: i32, y: i32, window: &mut dyn WindowHandle) -> bool {
        let Some(action) = self.edit.as_ref().and_then(|e| e.bar.mouse_down(x, y)) else {
            return false;
        };
        match action {
            BarAction::Close => self.leave_edit(),
            BarAction::Tool(tool) => {
                if let Some(mode) = &mut self.edit {
                    mode.bar.tool = tool;
                    if tool == EditTool::AddText {
                        mode.active = None;
                    }
                }
            }
            BarAction::Color(i) => {
                if let Some(mode) = &mut self.edit {
                    mode.bar.color = i;
                    mode.bar.color_set = true;
                    // Une zone neuve encore vide prend la couleur choisie.
                    let rgb = mode.bar.rgb();
                    if let Some(a) = &mut mode.active {
                        if a.original.is_empty() && a.drawn.is_empty() {
                            a.frame.color = rgb;
                        }
                    }
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
        true
    }

    /// Matrice espace de page → coordonnées de la vue, pour une page.
    fn page_to_view(&self, layout: &[PageBox], page: usize) -> Option<Matrix> {
        let l = self.loaded.as_ref()?;
        let (ox, top) = self.page_screen(layout, page)?;
        let PageBox { w, h, .. } = *layout.get(page)?;
        let p = l.pages.get(page)?;
        let m = base_matrix(&p.crop_box(&l.doc), self.scale(), p.rotate(&l.doc), w, h);
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
        // 2. Le bloc en cours : filet fin, sélection, curseur.
        let Some(page) = active_page else { return };
        let Some(m) = self.page_to_view(&layout, page) else {
            return;
        };
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
        // Dans le paragraphe en cours : déplacer le curseur, sélectionner.
        let inside = self
            .edit
            .as_ref()
            .and_then(|e| e.active.as_ref())
            .filter(|a| a.page == page)
            .and_then(|a| {
                let r = a.map.bounds()?;
                let m = a.frame.size * 0.4;
                let wide = a.frame.width < 2000.0 && a.map.lines.len() > 1;
                let (x0, x1) = if wide {
                    (a.frame.x0, a.frame.x0 + a.frame.width)
                } else {
                    (r.x0, r.x1)
                };
                (pt.x >= x0 - m && pt.x <= x1 + m && pt.y >= r.y0 - m && pt.y <= r.y1 + m)
                    .then_some(())
            })
            .is_some();
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
        // Ailleurs : la saisie en cours est finie (déjà dans le document).
        if let Some(mode) = &mut self.edit {
            mode.active = None;
        }
        match tool {
            EditTool::AddText => self.open_new_box(page, pt, window),
            EditTool::Select => {
                if let Some((index, _)) = self.paragraph_at(page, pt) {
                    self.open_existing(page, index, pt);
                } else {
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

    /// Ouvre un paragraphe existant, curseur au point cliqué.
    fn open_existing(&mut self, page: usize, index: Option<usize>, pt: Point) {
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
                if let Some(mode) = &mut self.edit {
                    mode.active = Some(Active {
                        page,
                        frame: o.frame,
                        original: o.drawn.clone(),
                        drawn: o.drawn,
                        buffer,
                        map: o.caret,
                        history: None,
                        selecting: true,
                    });
                }
            }
            Err(e) => self.set_notice(format!("ce texte ne peut pas être modifié : {e}")),
        }
    }

    /// Ouvre une zone de texte neuve au point cliqué.
    fn open_new_box(&mut self, page: usize, pt: Point, window: &mut dyn WindowHandle) {
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
                if let Some(mode) = &mut self.edit {
                    mode.active = Some(Active {
                        page,
                        frame,
                        original: String::new(),
                        drawn: String::new(),
                        buffer: Buffer::new(""),
                        map,
                        history: None,
                        selecting: false,
                    });
                    // Une zone posée, on revient à l'édition : c'est ce qu'on
                    // fait ensuite neuf fois sur dix.
                    mode.bar.tool = EditTool::Select;
                }
            }
            Err(e) => self.set_notice(format!("zone de texte impossible : {e}")),
        }
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
                if let Some(mode) = &mut self.edit {
                    mode.active = None;
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
                // Ctrl+Z : la saisie est déjà dans l'historique ; on la
                // referme et on laisse l'annulation générale la défaire.
                'z' | 'Z' | '\u{1a}' | 'y' | 'Y' | '\u{19}' => {
                    if let Some(mode) = &mut self.edit {
                        mode.active = None;
                    }
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

    /// Écrit le texte en cours dans le document, et en tire les
    /// conséquences : historique, fil de rendu, caches, rendu immédiat.
    ///
    /// Si le document refuse, la saisie revient à `before` : ce qu'on voit
    /// reste ce qui est dans le fichier.
    fn edit_apply(&mut self, before: Buffer, window: &mut dyn WindowHandle) {
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
                            expected: a.drawn.clone(),
                            text: a.buffer.text.clone(),
                        });
                    }
                    a.map = map;
                    a.drawn = normalized(&a.buffer.text);
                    // Une seule opération par saisie : du texte d'origine au
                    // texte actuel.
                    let op = EditOp::Paragraph {
                        page: a.page,
                        frame: a.frame.clone(),
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
                        let bitmap = render_page(&l.doc, p, scale, &options).bitmap;
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
        self.title_dirty = true;
        window.request_redraw();
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
