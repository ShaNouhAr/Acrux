//! Panneau latéral de navigation : onglet « Vignettes » (une miniature par
//! page, la page courante encadrée) et onglet « Signets » (arbre dépliable).
//! Le panneau ne connaît pas le document : il reçoit les tailles et
//! bitmaps des vignettes et la liste aplatie des signets, et renvoie des
//! [`PanelAction`].

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names
)]

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use acrux_graphics::Bitmap;

use crate::platform::Frame;
use crate::ui::lang::{tr, trf};
use crate::ui::paint::round_rect;
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Onglet affiché.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelTab {
    /// Miniatures des pages.
    Thumbnails,
    /// Signets.
    Bookmarks,
    /// Liste des commentaires (annotations) du document.
    Comments,
    /// Calques (contenu optionnel) avec leur case à cocher.
    Layers,
    /// Pièces jointes du document.
    Attachments,
}

/// Ce que le panneau demande au visualiseur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelAction {
    /// Rien.
    #[default]
    None,
    /// Aller à une page (0 = première).
    GoToPage(usize),
    /// Suivre le signet d'identifiant donné.
    Follow(usize),
    /// Aller au commentaire d'indice donné dans la liste fournie.
    GoToComment(usize),
    /// Afficher ou masquer un calque (numéro d'objet du groupe).
    ToggleLayer(u32),
    /// Enregistrer la pièce jointe d'indice donné dans la liste fournie.
    SaveAttachment(usize),
    /// Joindre un fichier au document (bouton « Ajouter »).
    AddAttachment,
    /// Déplacer une page : `from` vient se placer à l'indice `to` dans le
    /// document réordonné.
    MovePage {
        /// Page déplacée.
        from: usize,
        /// Nouvelle position.
        to: usize,
    },
}

/// Signet aplati (ordre d'affichage).
#[derive(Debug, Clone)]
pub struct OutlineRow {
    /// Identifiant stable (position dans la liste aplatie complète).
    pub id: usize,
    /// Profondeur (0 = racine).
    pub depth: usize,
    /// Titre.
    pub title: String,
    /// Possède des enfants.
    pub has_children: bool,
}

/// Commentaire affiché dans le panneau (une annotation porteuse de texte).
#[derive(Debug, Clone)]
pub struct CommentRow {
    /// Page (0 = première).
    pub page: usize,
    /// Type, en clé française (« Note », « Surlignage »…) : il est traduit
    /// au dessin, si bien qu'un changement de langue se voit tout de suite.
    pub kind: &'static str,
    /// Auteur, s'il est connu.
    pub author: Option<String>,
    /// Texte du commentaire (peut être vide).
    pub contents: String,
    /// Rectangle de l'annotation, en coordonnées de page : cliquer la ligne
    /// y mène.
    pub rect: acrux_core::Rect,
    /// Position de l'annotation dans le `/Annots` de sa page.
    pub index: usize,
    /// Identifiant de l'annotation (`/NM`), quand elle en a un. Avec
    /// `index`, c'est ce qui la désigne sans ambiguïté — pour y répondre ou
    /// lui donner un statut.
    pub name: Option<String>,
}

/// Pièce jointe affichée dans le panneau.
#[derive(Debug, Clone)]
pub struct AttachmentRow {
    /// Nom du fichier.
    pub name: String,
    /// Taille en octets, quand le document la déclare.
    pub size: Option<u64>,
    /// Page portant l'icône, s'il y en a une (0 = première).
    pub page: Option<usize>,
    /// Description (`/Desc`) ou type MIME : la deuxième ligne de la fiche.
    pub detail: String,
}

/// Données affichées par le panneau.
pub struct PanelContent<'a> {
    /// Nombre de pages.
    pub page_count: usize,
    /// Page courante.
    pub current: usize,
    /// Taille en pixels de la vignette de chaque page.
    pub thumb_sizes: &'a [(u32, u32)],
    /// Bitmaps rendus, par (page, clé d'échelle).
    pub bitmaps: &'a HashMap<(usize, u32), Bitmap>,
    /// Clé d'échelle des vignettes.
    pub thumb_key: u32,
    /// Signets aplatis (tous, l'état déplié est géré par le panneau).
    pub outline: &'a [OutlineRow],
    /// Commentaires du document, dans l'ordre des pages.
    pub comments: &'a [CommentRow],
    /// Calques du document, avec leur visibilité courante.
    pub layers: &'a [(u32, String, bool)],
    /// Pièces jointes du document.
    pub attachments: &'a [AttachmentRow],
}

/// Taille de fichier en unités lisibles (base 1024, comme l'explorateur).
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["o", "Kio", "Mio", "Gio"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} o")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Tab(PanelTab),
    Page(usize),
    /// Ligne de signet : identifiant, et vrai si le clic est sur le triangle.
    Row(usize, bool),
    /// Ligne de commentaire (indice dans la liste fournie).
    Comment(usize),
    /// Ligne de calque (indice dans la liste fournie).
    Layer(usize),
    /// Ligne de pièce jointe (indice dans la liste fournie).
    Attachment(usize),
    /// Bouton « Ajouter un fichier ».
    AddAttachment,
}

/// Panneau latéral.
pub struct Panel {
    /// Onglet courant.
    pub tab: PanelTab,
    /// Défilement du contenu, en pixels.
    scroll: f64,
    /// Zones cliquables calculées au dernier dessin : rectangle et cible.
    hits: Vec<((i32, i32, i32, i32), Hit)>,
    hover: Option<Hit>,
    /// Cible du focus clavier, quand le panneau l'a. Le panneau est une liste :
    /// les flèches y montent et descendent, Entrée active, Échap rend le focus
    /// au document.
    focus: Option<Hit>,
    /// Signets dépliés (par identifiant). Les racines le sont par défaut.
    expanded: HashSet<usize>,
    /// Hauteur totale du contenu au dernier dessin (pour borner le défilement).
    content_height: i32,
    /// Hauteur visible au dernier dessin.
    view_height: i32,
    /// Vignettes visibles au dernier dessin (pour les demandes de rendu).
    visible_pages: Vec<usize>,
    /// Page sur laquelle le bouton a été enfoncé, tant qu'on ne sait pas
    /// encore si c'est un clic ou un glisser.
    press: Option<(usize, i32, i32)>,
    /// Glisser en cours : page déplacée et position d'insertion visée.
    drag: Option<(usize, usize)>,
}

impl Default for Panel {
    fn default() -> Self {
        Self::new()
    }
}

impl Panel {
    /// Panneau sur l'onglet des vignettes.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tab: PanelTab::Thumbnails,
            scroll: 0.0,
            hits: Vec::new(),
            hover: None,
            focus: None,
            expanded: HashSet::new(),
            content_height: 0,
            view_height: 0,
            visible_pages: Vec::new(),
            press: None,
            drag: None,
        }
    }

    /// Ouvre les signets de premier niveau marqués ouverts dans le document.
    pub fn set_default_expanded(&mut self, ids: impl IntoIterator<Item = usize>) {
        self.expanded = ids.into_iter().collect();
    }

    /// Pages dont la vignette était visible au dernier dessin.
    #[must_use]
    pub fn visible_pages(&self) -> &[usize] {
        &self.visible_pages
    }

    /// Hauteur de la barre d'onglets.
    fn header_height(theme: &Theme, dpi: f32) -> i32 {
        (theme.toolbar_height as f32 * 0.8 * dpi).round() as i32
    }

    /// Onglets affichés : `(onglet, libellé, libellé abrégé)`. L'abrégé sert
    /// quand la case est trop étroite pour le libellé complet.
    fn tabs(with_layers: bool) -> Vec<(PanelTab, &'static str, &'static str)> {
        let mut tabs = vec![
            (PanelTab::Thumbnails, "Pages", "Pages"),
            (PanelTab::Bookmarks, "Signets", "Plan"),
            (PanelTab::Comments, "Notes", "Notes"),
        ];
        if with_layers {
            tabs.push((PanelTab::Layers, "Calques", "Calq."));
        }
        tabs.push((PanelTab::Attachments, "Fichiers", "Fich."));
        tabs
    }

    /// Lignes de signets visibles (ancêtres tous dépliés).
    fn visible_rows<'a>(&self, rows: &'a [OutlineRow]) -> Vec<&'a OutlineRow> {
        let mut out = Vec::new();
        // Profondeur au-delà de laquelle tout est masqué (None = rien de masqué).
        let mut hidden_below: Option<usize> = None;
        for r in rows {
            if let Some(d) = hidden_below {
                if r.depth > d {
                    continue;
                }
                hidden_below = None;
            }
            out.push(r);
            if r.has_children && !self.expanded.contains(&r.id) {
                hidden_below = Some(r.depth);
            }
        }
        out
    }

    /// Dessine le panneau dans `frame` (sous-vue de la fenêtre).
    #[allow(clippy::too_many_lines)] // en-tête, puis l'un des deux onglets
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
        content: &PanelContent<'_>,
    ) {
        let t = theme;
        let (w, h) = (frame.width as i32, frame.height as i32);
        frame.fill_rect(0, 0, w, h, t.bar.0, t.bar.1, t.bar.2);
        frame.fill_rect(w - 1, 0, 1, h, t.separator.0, t.separator.1, t.separator.2);
        self.hits.clear();
        self.visible_pages.clear();
        let size = t.font_size * dpi;
        let pad = (10.0 * dpi).round() as i32;
        // En-tête : deux onglets.
        let hh = Self::header_height(t, dpi);
        // L'onglet des calques n'apparaît que pour les documents qui en ont :
        // sinon il prendrait de la place pour rien (et Acrobat fait pareil).
        // Celui des pièces jointes est toujours là : c'est de lui qu'on en
        // ajoute une, y compris à un document qui n'en a aucune.
        let mut tabs = Self::tabs(!content.layers.is_empty());
        if content.layers.is_empty() && self.tab == PanelTab::Layers {
            self.tab = PanelTab::Thumbnails;
        }
        let tab_w = (w - 1) / tabs.len() as i32;
        let room = (tab_w - pad / 2) as f32;
        // Cinq onglets dans 240 px logiques : chaque libellé trop large pour
        // sa case retombe sur sa forme abrégée, plutôt que d'être tronqué au
        // milieu d'un mot.
        for entry in &mut tabs {
            if text.measure(size, entry.1) > room {
                entry.1 = entry.2;
            }
        }
        for (i, (tab, label, _)) in tabs.iter().enumerate() {
            let x = i as i32 * tab_w;
            let active = *tab == self.tab;
            if self.hover == Some(Hit::Tab(*tab)) && !active {
                round_rect(frame, x + 2, 3, tab_w - 4, hh - 6, 7.0 * dpi, t.hover);
            }
            let color = if active { t.text } else { t.text_dim };
            let tw = text.measure(size, label).min(room);
            let baseline = f32::midpoint(hh as f32, text.ascent(size)) - 1.0;
            text.draw_clipped(
                frame,
                x as f32 + (tab_w as f32 - tw) / 2.0,
                baseline,
                size,
                label,
                color,
                room,
            );
            if active {
                frame.fill_rect(
                    x + pad,
                    hh - 2,
                    tab_w - 2 * pad,
                    2,
                    t.accent.0,
                    t.accent.1,
                    t.accent.2,
                );
            }
            self.hits.push(((x, 0, tab_w, hh), Hit::Tab(*tab)));
        }
        frame.fill_rect(0, hh, w, 1, t.separator.0, t.separator.1, t.separator.2);
        let view_h = h - hh - 1;
        self.view_height = view_h;
        let mut body = frame.sub(0, hh + 1, (w - 1).max(0) as u32, view_h.max(0) as u32);
        let scroll = self.scroll.round() as i32;
        match self.tab {
            PanelTab::Thumbnails => {
                let label_h = (18.0 * dpi).round() as i32;
                let gap = (14.0 * dpi).round() as i32;
                let mut y = pad - scroll;
                for (page, &(tw, th)) in content.thumb_sizes.iter().enumerate() {
                    let cell_h = th as i32 + label_h + gap;
                    let x = ((w - 1) - tw as i32) / 2;
                    if y + cell_h >= 0 && y <= view_h {
                        self.visible_pages.push(page);
                        // Ombre, cadre (accent pour la page courante), bitmap ou blanc.
                        let current = page == content.current;
                        let hovered = self.hover == Some(Hit::Page(page))
                            || self.focus == Some(Hit::Page(page));
                        let moving = self.drag.is_some_and(|(from, _)| from == page);
                        if current || hovered {
                            let c = if current { t.accent } else { t.text_dim };
                            body.fill_rect(
                                x - 3,
                                y - 3,
                                tw as i32 + 6,
                                th as i32 + 6,
                                c.0,
                                c.1,
                                c.2,
                            );
                        }
                        body.fill_rect(
                            x + 2,
                            y + 2,
                            tw as i32,
                            th as i32,
                            t.page_shadow.0,
                            t.page_shadow.1,
                            t.page_shadow.2,
                        );
                        match content.bitmaps.get(&(page, content.thumb_key)) {
                            Some(b) if !moving => {
                                body.blit_rgba_premultiplied(x, y, b.width(), b.height(), b.data());
                            }
                            // Page en cours de déplacement : emplacement laissé vide.
                            _ => body.fill_rect(
                                x, y, tw as i32, th as i32, t.hover.0, t.hover.1, t.hover.2,
                            ),
                        }
                        // Trait d'insertion à l'endroit où la page tomberait.
                        if let Some((_, to)) = self.drag {
                            let line = (3.0 * dpi).round().max(2.0) as i32;
                            if to == page {
                                body.fill_rect(
                                    x - 6,
                                    y - 6,
                                    tw as i32 + 12,
                                    line,
                                    t.accent.0,
                                    t.accent.1,
                                    t.accent.2,
                                );
                            } else if to == page + 1 {
                                body.fill_rect(
                                    x - 6,
                                    y + th as i32 + 4,
                                    tw as i32 + 12,
                                    line,
                                    t.accent.0,
                                    t.accent.1,
                                    t.accent.2,
                                );
                            }
                        }
                        let label = format!("{}", page + 1);
                        let lw = text.measure(size, &label);
                        let baseline =
                            (y + th as i32) as f32 + label_h as f32 * 0.5 + text.ascent(size) / 2.0;
                        let color = if current { t.text } else { t.text_dim };
                        text.draw(
                            &mut body,
                            ((w - 1) as f32 - lw) / 2.0,
                            baseline,
                            size,
                            &label,
                            color,
                        );
                        self.hits
                            .push(((0, hh + 1 + y - 3, w - 1, cell_h), Hit::Page(page)));
                    }
                    y += cell_h;
                }
                self.content_height = y + scroll + pad;
            }
            PanelTab::Bookmarks => {
                let row_h = (24.0 * dpi).round() as i32;
                let indent = (14.0 * dpi).round() as i32;
                let rows = self.visible_rows(content.outline);
                if rows.is_empty() {
                    let baseline = pad as f32 + text.ascent(size) + 4.0 * dpi;
                    text.draw_clipped(
                        &mut body,
                        pad as f32,
                        baseline,
                        size,
                        "Aucun signet",
                        t.text_dim,
                        (w - 2 * pad) as f32,
                    );
                }
                let mut y = -scroll;
                for r in &rows {
                    if y + row_h >= 0 && y <= view_h {
                        let hovered = matches!(self.hover, Some(Hit::Row(id, _)) if id == r.id);
                        if hovered {
                            round_rect(&mut body, 4, y + 1, w - 9, row_h - 2, 7.0 * dpi, t.hover);
                        }
                        if matches!(self.focus, Some(Hit::Row(id, _)) if id == r.id) {
                            ring(&mut body, t, 0, y, w - 1, row_h);
                        }
                        let x = pad + r.depth as i32 * indent;
                        // Triangle de dépliage.
                        if r.has_children {
                            let open = self.expanded.contains(&r.id);
                            let cx = x + (5.0 * dpi) as i32;
                            let cy = y + row_h / 2;
                            let s = (4.0 * dpi).max(2.0) as i32;
                            for i in 0..s {
                                if open {
                                    body.fill_rect(
                                        cx - s + i,
                                        cy - s / 2 + i,
                                        2 * (s - i),
                                        1,
                                        t.text_dim.0,
                                        t.text_dim.1,
                                        t.text_dim.2,
                                    );
                                } else {
                                    body.fill_rect(
                                        cx - s / 2 + i,
                                        cy - s + i,
                                        1,
                                        2 * (s - i),
                                        t.text_dim.0,
                                        t.text_dim.1,
                                        t.text_dim.2,
                                    );
                                }
                            }
                        }
                        let tx = x + (14.0 * dpi) as i32;
                        let baseline =
                            y as f32 + f32::midpoint(row_h as f32, text.ascent(size)) - 1.0;
                        text.draw_clipped(
                            &mut body,
                            tx as f32,
                            baseline,
                            size,
                            &r.title,
                            t.text,
                            (w - 1 - tx - pad) as f32,
                        );
                        self.hits
                            .push(((0, hh + 1 + y, w - 1, row_h), Hit::Row(r.id, false)));
                        self.hits.push((
                            (x - 2, hh + 1 + y, (18.0 * dpi) as i32, row_h),
                            Hit::Row(r.id, true),
                        ));
                    }
                    y += row_h;
                }
                self.content_height = y + scroll + pad;
            }
            PanelTab::Layers => {
                let row_h = (text.line_height(size) * 1.8).round() as i32;
                let box_side = (12.0 * dpi).round() as i32;
                let mut y = -scroll;
                for (index, (_, name, visible)) in content.layers.iter().enumerate() {
                    if y + row_h >= 0 && y <= view_h {
                        if self.hover == Some(Hit::Layer(index)) {
                            body.fill_rect(0, y, w - 1, row_h, t.hover.0, t.hover.1, t.hover.2);
                        }
                        if self.focus == Some(Hit::Layer(index)) {
                            ring(&mut body, t, 0, y, w - 1, row_h);
                        }
                        let bx = pad;
                        let by = y + (row_h - box_side) / 2;
                        let c = if *visible { t.accent } else { t.text_dim };
                        body.fill_rect(bx, by, box_side, box_side, c.0, c.1, c.2);
                        if *visible {
                            // Coche : deux traits en diagonale.
                            let s2 = box_side / 4;
                            for i in 0..s2 {
                                body.fill_rect(
                                    bx + 2 + i,
                                    by + box_side / 2 + i,
                                    2,
                                    1,
                                    255,
                                    255,
                                    255,
                                );
                            }
                            for i in 0..(box_side / 2) {
                                body.fill_rect(
                                    bx + 2 + s2 + i,
                                    by + box_side / 2 + s2 - i,
                                    2,
                                    1,
                                    255,
                                    255,
                                    255,
                                );
                            }
                        } else {
                            // Case vide : un cadre, pas de coche.
                            body.fill_rect(
                                bx + 1,
                                by + 1,
                                box_side - 2,
                                box_side - 2,
                                t.bar.0,
                                t.bar.1,
                                t.bar.2,
                            );
                        }
                        let tx = bx + box_side + pad / 2;
                        let baseline =
                            y as f32 + f32::midpoint(row_h as f32, text.ascent(size)) - 1.0;
                        text.draw_clipped(
                            &mut body,
                            tx as f32,
                            baseline,
                            size,
                            name,
                            if *visible { t.text } else { t.text_dim },
                            (w - 1 - tx - pad) as f32,
                        );
                        self.hits
                            .push(((0, hh + 1 + y, w - 1, row_h), Hit::Layer(index)));
                    }
                    y += row_h;
                }
                self.content_height = y + scroll + pad;
            }
            PanelTab::Comments => {
                let line_h = (text.line_height(size)).round() as i32;
                let row_h = line_h * 2 + pad;
                if content.comments.is_empty() {
                    let baseline = pad as f32 + text.ascent(size);
                    text.draw_clipped(
                        &mut body,
                        pad as f32,
                        baseline,
                        size,
                        tr("Aucun commentaire"),
                        t.text_dim,
                        (w - 2 * pad) as f32,
                    );
                }
                let mut y = -scroll;
                for (index, c) in content.comments.iter().enumerate() {
                    if y + row_h >= 0 && y <= view_h {
                        if self.hover == Some(Hit::Comment(index)) {
                            body.fill_rect(0, y, w - 1, row_h, t.hover.0, t.hover.1, t.hover.2);
                        }
                        if self.focus == Some(Hit::Comment(index)) {
                            ring(&mut body, t, 0, y, w - 1, row_h);
                        }
                        // Première ligne : type, auteur et page.
                        let who = c.author.clone().unwrap_or_default();
                        let page = (c.page + 1).to_string();
                        let head = if who.is_empty() {
                            trf("{} — page {}", &[tr(c.kind), &page])
                        } else {
                            trf("{} de {} — page {}", &[tr(c.kind), &who, &page])
                        };
                        let base1 = y as f32 + (pad / 2) as f32 + text.ascent(size);
                        text.draw_clipped(
                            &mut body,
                            pad as f32,
                            base1,
                            size,
                            &head,
                            t.text_dim,
                            (w - 1 - 2 * pad) as f32,
                        );
                        let body_text = if c.contents.trim().is_empty() {
                            tr("(sans texte)").to_string()
                        } else {
                            c.contents.replace('\n', " ")
                        };
                        text.draw_clipped(
                            &mut body,
                            pad as f32,
                            base1 + line_h as f32,
                            size,
                            &body_text,
                            t.text,
                            (w - 1 - 2 * pad) as f32,
                        );
                        body.fill_rect(
                            0,
                            y + row_h - 1,
                            w - 1,
                            1,
                            t.separator.0,
                            t.separator.1,
                            t.separator.2,
                        );
                        self.hits
                            .push(((0, hh + 1 + y, w - 1, row_h), Hit::Comment(index)));
                    }
                    y += row_h;
                }
                self.content_height = y + scroll + pad;
            }
            PanelTab::Attachments => {
                let line_h = text.line_height(size).round() as i32;
                let row_h = line_h * 2 + pad;
                // Bouton « Ajouter », épinglé en haut : il doit rester
                // atteignable même quand la liste est longue.
                let button_h = line_h + pad;
                let hovered = self.hover == Some(Hit::AddAttachment);
                let c = if hovered { t.accent } else { t.hover };
                body.fill_rect(pad, pad / 2, w - 1 - 2 * pad, button_h, c.0, c.1, c.2);
                let label = "Ajouter un fichier…";
                let lw = text.measure(size, label);
                text.draw_clipped(
                    &mut body,
                    ((w - 1) as f32 - lw) / 2.0,
                    (pad / 2) as f32 + f32::midpoint(button_h as f32, text.ascent(size)) - 1.0,
                    size,
                    label,
                    t.text,
                    (w - 1 - 2 * pad) as f32,
                );
                let top = button_h + pad;
                let list_h = (view_h - top).max(0);
                let mut list = body.sub(0, top, (w - 1).max(0) as u32, list_h as u32);
                if content.attachments.is_empty() {
                    let baseline = pad as f32 + text.ascent(size);
                    text.draw_clipped(
                        &mut list,
                        pad as f32,
                        baseline,
                        size,
                        "Aucune pièce jointe",
                        t.text_dim,
                        (w - 2 * pad) as f32,
                    );
                }
                let mut y = -scroll;
                for (index, a) in content.attachments.iter().enumerate() {
                    if y + row_h >= 0 && y <= list_h {
                        if self.hover == Some(Hit::Attachment(index)) {
                            list.fill_rect(0, y, w - 1, row_h, t.hover.0, t.hover.1, t.hover.2);
                        }
                        if self.focus == Some(Hit::Attachment(index)) {
                            ring(&mut list, t, 0, y, w - 1, row_h);
                        }
                        let base1 = y as f32 + (pad / 2) as f32 + text.ascent(size);
                        text.draw_clipped(
                            &mut list,
                            pad as f32,
                            base1,
                            size,
                            &a.name,
                            t.text,
                            (w - 1 - 2 * pad) as f32,
                        );
                        // Deuxième ligne : taille, page, puis description.
                        let mut detail = match a.size {
                            Some(s) => human_size(s),
                            None => "taille inconnue".to_string(),
                        };
                        if let Some(p) = a.page {
                            let _ = write!(detail, " — page {}", p + 1);
                        }
                        if !a.detail.is_empty() {
                            detail.push_str(" — ");
                            detail.push_str(&a.detail);
                        }
                        text.draw_clipped(
                            &mut list,
                            pad as f32,
                            base1 + line_h as f32,
                            size,
                            &detail,
                            t.text_dim,
                            (w - 1 - 2 * pad) as f32,
                        );
                        list.fill_rect(
                            0,
                            y + row_h - 1,
                            w - 1,
                            1,
                            t.separator.0,
                            t.separator.1,
                            t.separator.2,
                        );
                        self.hits
                            .push(((0, hh + 1 + top + y, w - 1, row_h), Hit::Attachment(index)));
                    }
                    y += row_h;
                }
                // Le bouton est déclaré en dernier : `hit_at` prend la
                // dernière correspondance, donc il gagne sur une ligne qui
                // passerait dessous.
                self.hits.push((
                    (pad, hh + 1 + pad / 2, w - 1 - 2 * pad, button_h),
                    Hit::AddAttachment,
                ));
                self.content_height = y + scroll + top + pad;
            }
        }
        // Barre de défilement discrète.
        if self.content_height > view_h && view_h > 0 {
            let track = view_h;
            let thumb_h = ((f64::from(view_h) / f64::from(self.content_height)) * f64::from(track))
                .max(20.0) as i32;
            let max_scroll = f64::from((self.content_height - view_h).max(1));
            let ty = ((self.scroll / max_scroll) * f64::from(track - thumb_h)) as i32;
            frame.fill_rect(
                w - 5,
                hh + 1 + ty,
                3,
                thumb_h,
                t.text_dim.0,
                t.text_dim.1,
                t.text_dim.2,
            );
        }
    }

    /// Cibles atteignables au clavier dans l'onglet courant, dans l'ordre.
    fn focusable(&self, content: &PanelContent<'_>) -> Vec<Hit> {
        match self.tab {
            PanelTab::Thumbnails => (0..content.page_count).map(Hit::Page).collect(),
            PanelTab::Bookmarks => self
                .visible_rows(content.outline)
                .iter()
                .map(|r| Hit::Row(r.id, false))
                .collect(),
            PanelTab::Comments => (0..content.comments.len()).map(Hit::Comment).collect(),
            PanelTab::Layers => (0..content.layers.len()).map(Hit::Layer).collect(),
            PanelTab::Attachments => std::iter::once(Hit::AddAttachment)
                .chain((0..content.attachments.len()).map(Hit::Attachment))
                .collect(),
        }
    }

    /// Place le focus sur la première cible (ou la dernière si `last`).
    /// Renvoie faux si l'onglet courant est vide.
    pub fn focus_edge(&mut self, last: bool, content: &PanelContent<'_>) -> bool {
        let targets = self.focusable(content);
        self.focus = if last {
            targets.last().copied()
        } else {
            targets.first().copied()
        };
        self.focus.is_some()
    }

    /// Déplace le focus d'une ligne. Renvoie faux quand on sort de la liste.
    pub fn focus_step(&mut self, forward: bool, content: &PanelContent<'_>) -> bool {
        let targets = self.focusable(content);
        let Some(current) = self.focus else {
            return self.focus_edge(!forward, content);
        };
        let Some(at) = targets.iter().position(|h| *h == current) else {
            return self.focus_edge(!forward, content);
        };
        let next = if forward {
            Some(at + 1)
        } else {
            at.checked_sub(1)
        };
        if let Some(target) = next.and_then(|n| targets.get(n)) {
            self.focus = Some(*target);
            true
        } else {
            self.focus = None;
            false
        }
    }

    /// Abandonne le focus clavier.
    pub fn clear_focus(&mut self) {
        self.focus = None;
    }

    /// Vrai si le panneau tient le focus clavier.
    #[must_use]
    pub fn focused(&self) -> bool {
        self.focus.is_some()
    }

    /// Page ciblée par le focus, s'il vise une vignette (pour la faire
    /// apparaître dans la fenêtre du panneau).
    #[must_use]
    pub fn focused_page(&self) -> Option<usize> {
        match self.focus {
            Some(Hit::Page(p)) => Some(p),
            _ => None,
        }
    }

    /// Vignette sous un point du panneau, s'il y en a une : c'est ce que
    /// vise un clic droit, qui ouvre le menu de la page.
    #[must_use]
    pub fn page_at(&self, x: i32, y: i32) -> Option<usize> {
        match self.hit_at(x, y) {
            Some(Hit::Page(p)) => Some(p),
            _ => None,
        }
    }

    /// Rectangle de la vignette d'une page au dernier dessin, si elle est
    /// visible : le menu ouvert au clavier se pose dessus.
    #[must_use]
    pub fn page_rect(&self, page: usize) -> Option<(i32, i32, i32, i32)> {
        self.hits
            .iter()
            .find(|(_, hit)| *hit == Hit::Page(page))
            .map(|(rect, _)| *rect)
    }

    /// Active la ligne sous le focus (Entrée ou Espace).
    pub fn activate_focus(&mut self, content: &PanelContent<'_>) -> PanelAction {
        match self.focus {
            Some(hit) => self.act_on(hit, content),
            None => PanelAction::None,
        }
    }

    /// Déplie ou replie le signet sous le focus (flèches gauche et droite).
    pub fn toggle_focused_branch(&mut self, open: bool) -> bool {
        let Some(Hit::Row(id, _)) = self.focus else {
            return false;
        };
        if open {
            self.expanded.insert(id)
        } else {
            self.expanded.remove(&id)
        }
    }

    fn hit_at(&self, x: i32, y: i32) -> Option<Hit> {
        // Les zones spécifiques (triangles) sont déclarées après les lignes : on
        // prend la dernière correspondance.
        self.hits
            .iter()
            .rev()
            .find(|((rx, ry, rw, rh), _)| x >= *rx && x < rx + rw && y >= *ry && y < ry + rh)
            .map(|(_, h)| *h)
    }

    /// Déplacement de la souris. `dragging` indique que le bouton gauche est
    /// enfoncé : au-delà d'un seuil, le clic devient un glisser de page.
    pub fn mouse_move(&mut self, x: i32, y: i32, dragging: bool) -> bool {
        if !dragging {
            self.press = None;
            self.drag = None;
        } else if let Some((page, px, py)) = self.press {
            if self.drag.is_none() && ((x - px).abs() > 4 || (y - py).abs() > 4) {
                self.drag = Some((page, page));
            }
            if self.drag.is_some() {
                if let Some(to) = self.drop_index(y) {
                    let changed = self.drag.is_some_and(|(_, old)| old != to);
                    self.drag = Some((page, to));
                    return changed;
                }
                return false;
            }
        }
        let hover = self.hit_at(x, y).map(|h| match h {
            Hit::Row(id, _) => Hit::Row(id, false),
            other => other,
        });
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    /// Le pointeur a quitté le panneau.
    pub fn mouse_leave(&mut self) -> bool {
        let changed = self.hover.is_some();
        self.hover = None;
        changed
    }

    /// Position d'insertion visée par un glisser à l'ordonnée `y`.
    fn drop_index(&self, y: i32) -> Option<usize> {
        let mut best: Option<(i32, usize)> = None;
        for (rect, hit) in &self.hits {
            let Hit::Page(page) = hit else { continue };
            let (_, ry, _, rh) = *rect;
            // Moitié haute : insérer avant ; moitié basse : après.
            let (d, index) = if y < ry + rh / 2 {
                ((y - ry).abs(), *page)
            } else {
                ((y - ry - rh).abs(), page + 1)
            };
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, index));
            }
        }
        best.map(|(_, i)| i)
    }

    /// Bouton relâché : termine un glisser éventuel.
    pub fn mouse_up(&mut self) -> PanelAction {
        self.press = None;
        let Some((from, to)) = self.drag.take() else {
            return PanelAction::None;
        };
        // Déposer juste avant ou juste après soi-même ne change rien.
        if to == from || to == from + 1 {
            return PanelAction::None;
        }
        let to = if to > from { to - 1 } else { to };
        PanelAction::MovePage { from, to }
    }

    /// Vrai si un glisser de page est en cours.
    #[must_use]
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Clic gauche.
    pub fn mouse_down(&mut self, x: i32, y: i32, content: &PanelContent<'_>) -> PanelAction {
        if let Some(Hit::Page(p)) = self.hit_at(x, y) {
            self.press = Some((p, x, y));
        }
        match self.hit_at(x, y) {
            Some(hit) => self.act_on(hit, content),
            None => PanelAction::None,
        }
    }

    /// Ce que déclenche une cible, qu'on l'ait atteinte à la souris ou au
    /// clavier : une seule règle, donc pas de divergence entre les deux.
    fn act_on(&mut self, hit: Hit, content: &PanelContent<'_>) -> PanelAction {
        match hit {
            Hit::Tab(tab) => {
                if tab != self.tab {
                    self.tab = tab;
                    self.scroll = 0.0;
                    self.focus = None;
                }
                PanelAction::None
            }
            Hit::Page(p) => PanelAction::GoToPage(p),
            Hit::Row(id, on_toggle) => {
                let has_children = content.outline.iter().any(|r| r.id == id && r.has_children);
                if on_toggle && has_children {
                    if !self.expanded.remove(&id) {
                        self.expanded.insert(id);
                    }
                    PanelAction::None
                } else {
                    PanelAction::Follow(id)
                }
            }
            Hit::Comment(index) => PanelAction::GoToComment(index),
            Hit::Layer(index) => match content.layers.get(index) {
                Some((number, _, _)) => PanelAction::ToggleLayer(*number),
                None => PanelAction::None,
            },
            Hit::Attachment(index) if index < content.attachments.len() => {
                PanelAction::SaveAttachment(index)
            }
            Hit::AddAttachment => PanelAction::AddAttachment,
            Hit::Attachment(_) => PanelAction::None,
        }
    }

    /// Molette (crans positifs = vers le haut).
    pub fn wheel(&mut self, delta: f32) {
        self.scroll -= f64::from(delta) * 80.0;
        self.clamp();
    }

    fn clamp(&mut self) {
        let max = f64::from((self.content_height - self.view_height).max(0));
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// Fait défiler les vignettes pour montrer la page donnée.
    pub fn reveal_page(&mut self, page: usize, thumb_sizes: &[(u32, u32)], dpi: f32) {
        if self.tab != PanelTab::Thumbnails {
            return;
        }
        let label_h = (18.0 * dpi).round() as i32;
        let gap = (14.0 * dpi).round() as i32;
        let pad = (10.0 * dpi).round() as i32;
        let mut y = pad;
        for (i, &(_, th)) in thumb_sizes.iter().enumerate() {
            let cell_h = th as i32 + label_h + gap;
            if i == page {
                let top = f64::from(y - pad);
                let bottom = f64::from(y + cell_h);
                if top < self.scroll {
                    self.scroll = top;
                } else if bottom > self.scroll + f64::from(self.view_height) {
                    self.scroll = bottom - f64::from(self.view_height);
                }
                break;
            }
            y += cell_h;
        }
        self.clamp();
    }
}

/// Anneau de focus d'une ligne de panneau, visible sur fond clair comme sur
/// fond sombre.
fn ring(frame: &mut Frame<'_>, t: &Theme, x: i32, y: i32, w: i32, h: i32) {
    if w <= 0 || h <= 0 {
        return;
    }
    let a = t.accent;
    for inset in [0, 1] {
        let (x, y, w, h) = (x + inset, y + inset, w - 2 * inset, h - 2 * inset);
        if w <= 0 || h <= 0 {
            continue;
        }
        frame.fill_rect(x, y, w, 1, a.0, a.1, a.2);
        frame.fill_rect(x, y + h - 1, w, 1, a.0, a.1, a.2);
        frame.fill_rect(x, y, 1, h, a.0, a.1, a.2);
        frame.fill_rect(x + w - 1, y, 1, h, a.0, a.1, a.2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<OutlineRow> {
        vec![
            OutlineRow {
                id: 0,
                depth: 0,
                title: "A".into(),
                has_children: true,
            },
            OutlineRow {
                id: 1,
                depth: 1,
                title: "A.1".into(),
                has_children: true,
            },
            OutlineRow {
                id: 2,
                depth: 2,
                title: "A.1.a".into(),
                has_children: false,
            },
            OutlineRow {
                id: 3,
                depth: 0,
                title: "B".into(),
                has_children: false,
            },
        ]
    }

    #[test]
    fn dragging_a_thumbnail_moves_the_page() {
        let mut p = Panel::new();
        p.hits = vec![
            ((0, 0, 200, 100), Hit::Page(0)),
            ((0, 100, 200, 100), Hit::Page(1)),
            ((0, 200, 200, 100), Hit::Page(2)),
        ];
        let bitmaps = HashMap::new();
        let content = PanelContent {
            page_count: 3,
            current: 0,
            thumb_sizes: &[(10, 10); 3],
            bitmaps: &bitmaps,
            thumb_key: 1,
            outline: &[],
            comments: &[],
            layers: &[],
            attachments: &[],
        };
        // Un clic simple reste un clic.
        assert_eq!(p.mouse_down(50, 50, &content), PanelAction::GoToPage(0));
        assert_eq!(p.mouse_up(), PanelAction::None);
        // Un déplacement au-delà du seuil devient un glisser.
        assert_eq!(p.mouse_down(50, 50, &content), PanelAction::GoToPage(0));
        // Déposé entre les vignettes 1 et 2 : la page 0 se glisse au milieu.
        p.mouse_move(52, 250, true);
        assert!(p.dragging());
        assert_eq!(p.mouse_up(), PanelAction::MovePage { from: 0, to: 1 });
        // Déposé sous la dernière vignette : la page 0 passe en dernier.
        assert_eq!(p.mouse_down(50, 50, &content), PanelAction::GoToPage(0));
        p.mouse_move(52, 295, true);
        assert_eq!(p.mouse_up(), PanelAction::MovePage { from: 0, to: 2 });
        assert!(!p.dragging());
        // Déposer à sa propre place ne déclenche rien.
        assert_eq!(p.mouse_down(50, 50, &content), PanelAction::GoToPage(0));
        p.mouse_move(52, 60, true);
        assert_eq!(p.mouse_up(), PanelAction::None);
    }

    /// Cinq onglets tiennent dans la largeur du panneau : chaque libellé
    /// retenu (complet ou abrégé) doit **entrer** dans sa case, sinon il
    /// serait coupé au milieu d'un mot. La mesure se fait avec la vraie
    /// police d'interface ; sans police système, le test ne peut rien dire.
    #[test]
    fn every_tab_label_fits_its_slot() {
        let Some(mut text) = TextRenderer::system() else {
            return;
        };
        let theme = Theme::dark();
        // Largeur du panneau (`PANEL_WIDTH` du visualiseur) à l'échelle 1.
        let panel_width = 240;
        let pad = 10;
        for with_layers in [false, true] {
            let tabs = Panel::tabs(with_layers);
            let tab_w = (panel_width - 1) / tabs.len() as i32;
            let room = (tab_w - pad / 2) as f32;
            for (tab, long, short) in tabs {
                let chosen = if text.measure(theme.font_size, long) > room {
                    short
                } else {
                    long
                };
                let width = text.measure(theme.font_size, chosen);
                assert!(
                    width <= room,
                    "libellé « {chosen} » de l'onglet {tab:?} : {width:.1} px pour {room:.1} px disponibles"
                );
            }
        }
    }

    #[test]
    fn human_sizes_use_binary_units() {
        assert_eq!(human_size(0), "0 o");
        assert_eq!(human_size(512), "512 o");
        assert_eq!(human_size(1024), "1.0 Kio");
        assert_eq!(human_size(1_572_864), "1.5 Mio");
    }

    /// Onglet des pièces jointes : un clic sur une ligne propose de
    /// l'enregistrer, un clic sur le bouton en ajoute une.
    #[test]
    fn attachment_rows_and_button_report_their_action() {
        let mut p = Panel::new();
        p.tab = PanelTab::Attachments;
        let bitmaps = HashMap::new();
        let rows = vec![
            AttachmentRow {
                name: "a.txt".into(),
                size: Some(12),
                page: None,
                detail: "text/plain".into(),
            },
            AttachmentRow {
                name: "b.bin".into(),
                size: None,
                page: Some(2),
                detail: String::new(),
            },
        ];
        let content = PanelContent {
            page_count: 3,
            current: 0,
            thumb_sizes: &[(10, 10); 3],
            bitmaps: &bitmaps,
            thumb_key: 1,
            outline: &[],
            comments: &[],
            layers: &[],
            attachments: &rows,
        };
        p.hits.push(((0, 40, 200, 40), Hit::Attachment(0)));
        p.hits.push(((0, 80, 200, 40), Hit::Attachment(1)));
        p.hits.push(((0, 120, 200, 40), Hit::Attachment(9)));
        p.hits.push(((0, 0, 200, 30), Hit::AddAttachment));
        assert_eq!(
            p.mouse_down(10, 50, &content),
            PanelAction::SaveAttachment(0)
        );
        assert_eq!(
            p.mouse_down(10, 90, &content),
            PanelAction::SaveAttachment(1)
        );
        // Indice hors liste (panneau repeint entre-temps) : rien ne se passe.
        assert_eq!(p.mouse_down(10, 130, &content), PanelAction::None);
        assert_eq!(p.mouse_down(10, 10, &content), PanelAction::AddAttachment);
    }

    #[test]
    fn collapsed_ancestors_hide_descendants() {
        let mut p = Panel::new();
        let r = rows();
        let v: Vec<usize> = p.visible_rows(&r).iter().map(|r| r.id).collect();
        assert_eq!(v, vec![0, 3]);
        p.set_default_expanded([0]);
        let v: Vec<usize> = p.visible_rows(&r).iter().map(|r| r.id).collect();
        assert_eq!(v, vec![0, 1, 3]);
        p.set_default_expanded([0, 1]);
        let v: Vec<usize> = p.visible_rows(&r).iter().map(|r| r.id).collect();
        assert_eq!(v, vec![0, 1, 2, 3]);
    }

    #[test]
    fn clicks_resolve_from_last_layout() {
        let mut p = Panel::new();
        p.tab = PanelTab::Bookmarks;
        let r = rows();
        let bitmaps = HashMap::new();
        let content = PanelContent {
            page_count: 2,
            current: 0,
            thumb_sizes: &[(100, 140), (100, 140)],
            bitmaps: &bitmaps,
            thumb_key: 1,
            outline: &r,
            comments: &[],
            layers: &[],
            attachments: &[],
        };
        p.hits.push(((0, 40, 200, 24), Hit::Row(0, false)));
        p.hits.push(((8, 40, 18, 24), Hit::Row(0, true)));
        assert_eq!(p.mouse_down(100, 50, &content), PanelAction::Follow(0));
        assert_eq!(p.mouse_down(12, 50, &content), PanelAction::None);
        assert!(p.expanded.contains(&0));
        p.hits
            .push(((0, 0, 100, 30), Hit::Tab(PanelTab::Thumbnails)));
        assert_eq!(p.mouse_down(10, 10, &content), PanelAction::None);
        assert_eq!(p.tab, PanelTab::Thumbnails);
        p.hits.push(((0, 40, 200, 160), Hit::Page(1)));
        assert_eq!(p.mouse_down(50, 100, &content), PanelAction::GoToPage(1));
        assert!(p.mouse_move(50, 100, false));
        assert!(!p.mouse_move(51, 101, false));
        assert!(p.mouse_leave());
    }

    #[test]
    fn le_clic_droit_vise_une_vignette() {
        let mut p = Panel::new();
        p.hits
            .push(((0, 0, 100, 30), Hit::Tab(PanelTab::Bookmarks)));
        p.hits.push(((0, 40, 200, 160), Hit::Page(3)));
        assert_eq!(p.page_at(50, 100), Some(3));
        assert_eq!(
            p.page_at(50, 10),
            None,
            "un onglet du panneau n'est pas une page"
        );
        assert_eq!(p.page_at(50, 300), None);
        assert_eq!(p.page_rect(3), Some((0, 40, 200, 160)));
        assert_eq!(p.page_rect(4), None, "vignette hors de la vue");
    }
}
