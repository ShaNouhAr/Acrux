//! Gérer les commentaires sur la page : sélectionner une annotation — les
//! nôtres comme celles d'autres logiciels —, la déplacer, la
//! redimensionner, changer sa couleur, son fond, son trait, son opacité ou
//! son texte, la supprimer, y répondre, lui donner un statut.
//!
//! # Un clic, une sélection
//!
//! Un clic sur une forme, une note, une zone de texte ou un tampon le
//! sélectionne : un cadre d'accent, ses poignées quand il se redimensionne,
//! et au-dessus la barre de propriétés (`ui/annotbar.rs`). Glisser déplace,
//! tirer une poignée redimensionne, Suppr supprime, les flèches décalent
//! d'un point (dix avec Maj), Échap désélectionne. Un surlignage, un
//! soulignement ou un barré suivent le texte : ils ne bougent pas, et un clic
//! dessus ne vole pas la sélection du texte — c'est le relâchement sans
//! glisser qui les sélectionne, et le double-clic qui ouvre leur bulle.
//!
//! Pendant le geste, rien n'est écrit : l'annotation suit le pointeur sous
//! la forme d'un **fantôme**, elle seule rendue par le moteur
//! (`acrux_render::render_annotation`), une fois au début du geste et de
//! nouveau quand sa taille change. Au relâchement, une seule modification
//! (`EditOp::AnnotSet`) : l'annulation, le fil de rendu et l'enregistrement
//! n'ont rien de nouveau à apprendre, et Ctrl+Z défait le geste entier.
//!
//! # Ce qui ne change pas
//!
//! « Modifier le PDF », « Modifier les objets », « Remplir et signer », les
//! outils de dessin et le modèle 3D ont leurs propres gestes : tant que l'un
//! d'eux a la main, aucune annotation ne se sélectionne ici, et un élément
//! de « remplir et signer » ne se sélectionne jamais ici.
//!
//! # Désigner une annotation
//!
//! Une modification désigne l'annotation par sa page et son rang dans
//! `/Annots` — c'est ce qui se rejoue. La sélection, elle, retient aussi sa
//! **référence** : après une modification ou une annulation, elle retrouve
//! son annotation par elle, ou disparaît si elle n'est plus là. Suppr ne
//! peut donc jamais effacer une autre annotation que celle qu'on voit
//! sélectionnée.

// Coordonnées d'écran entières et échelles flottantes, comme le reste du
// visualiseur.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use acrux_core::{Point, Rect};
use acrux_document::ObjectRef;
use acrux_features::annotations::review::{ReviewState, StateChange};
use acrux_features::annotations::{pdf_date_now, AnnotChanges, AnnotMeta, AnnotationInfo};
use acrux_graphics::Bitmap;

use super::dialogs::Then;
use super::draw::{preview_rgb, Choice, DrawPopup, DrawSetting, OPACITIES, WIDTHS};
use super::{author_name, log_line, shown_rotation, Prompt, PromptKind, Viewer};
use crate::platform::{Cursor, Event, Frame, Key, Modifiers, MouseButton, WindowHandle};
use crate::render_worker::EditOp;
use crate::ui::annotbar::BarItem;
use crate::ui::bubble::{BubbleAction, NoteBubble, Thread};
use crate::ui::icons::Icon;
use crate::ui::input::TextInput;
use crate::ui::lang::{self, tr, trf};
use crate::ui::menu::{Menu, Outcome};
use crate::ui::modebar::{Setting, Swatch};
use crate::ui::objects::{self as objects_ui, Handle, ViewRect};
use crate::ui::palette::Command;
use crate::ui::panel::{CommentMenu, CommentSort};
use crate::ui::pickers::ColorPicker;

/// Annotation sélectionnée sur la page.
pub(super) struct AnnotSel {
    /// Page.
    pub(super) page: usize,
    /// Rang dans `/Annots`.
    pub(super) index: usize,
    /// Référence de l'annotation : c'est elle qui la retrouve après une
    /// modification, quand son rang a pu changer.
    reference: Option<ObjectRef>,
    /// `/Subtype`.
    subtype: String,
    /// Rectangle, en coordonnées de page.
    pub(super) rect: Rect,
    /// Elle se déplace.
    movable: bool,
    /// Elle se redimensionne par ses poignées.
    resizable: bool,
    /// Geste en cours.
    pub(super) drag: Option<AnnotDrag>,
    /// Poignée survolée.
    hover: Option<Handle>,
}

/// Un geste sur l'annotation sélectionnée.
pub(super) struct AnnotDrag {
    /// Poignée saisie (`Handle::Body` : on déplace).
    handle: Handle,
    /// Point de départ, en coordonnées de page.
    from: Point,
    /// Rectangle courant.
    current: Rect,
    /// Le pointeur a vraiment bougé : un simple clic n'écrit rien.
    moved: bool,
    /// L'annotation rendue seule, et le rectangle pour lequel elle l'a été.
    ghost: Option<(Rect, Bitmap)>,
}

/// L'annotation telle qu'elle est après une modification, montrée le
/// temps que sa page revienne du fil de rendu : sans elle, un déplacement
/// semblerait revenir en arrière un instant.
pub(super) struct AnnotGhost {
    page: usize,
    key_scale: u32,
    rect: Rect,
    bitmap: Bitmap,
}

/// Choix d'une liste du panneau des commentaires.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum CommentPick {
    /// Un ordre.
    Sort(CommentSort),
    /// Montrer ou masquer un type.
    Kind(&'static str),
    /// Tous les types.
    AllKinds,
    /// Un auteur (rang dans la liste retenue à l'ouverture), ou tous.
    Author(Option<usize>),
    /// Un statut pour le commentaire d'indice donné.
    Status(usize, ReviewState),
}

/// Liste déroulée du panneau des commentaires, et les auteurs qu'elle
/// propose.
pub(super) struct CommentMenuOpen {
    menu: Menu<CommentPick>,
    authors: Vec<String>,
}

/// Types qui se déplacent. Pas un signe d'insertion : il désigne un
/// endroit du texte, et déplacé il ne voudrait plus rien dire.
fn movable(subtype: &str) -> bool {
    matches!(
        subtype,
        "Square"
            | "Circle"
            | "Line"
            | "Polygon"
            | "PolyLine"
            | "Ink"
            | "FreeText"
            | "Stamp"
            | "Text"
            | "FileAttachment"
            | "Sound"
    )
}

/// Types qui se redimensionnent. Pas une note : son icône garde sa taille
/// quel que soit le zoom (`/NoZoom`).
fn resizable(subtype: &str) -> bool {
    matches!(
        subtype,
        "Square" | "Circle" | "FreeText" | "Stamp" | "Ink" | "Line" | "Polygon" | "PolyLine"
    )
}

/// Balisages du texte : ils suivent le texte, ne se déplacent pas, et
/// laissent la sélection du texte passer.
fn follows_text(subtype: &str) -> bool {
    matches!(
        subtype,
        "Highlight" | "Underline" | "StrikeOut" | "Squiggly" | "Redact"
    )
}

/// Vrai pour une annotation qu'un clic sélectionne ici : un commentaire
/// visible, qui n'est ni une réponse, ni un état, ni le membre d'un groupe,
/// ni un élément de « remplir et signer ».
fn selectable(a: &AnnotationInfo) -> bool {
    acrux_features::annotations::review::is_markup(&a.subtype)
        && !a.fill_sign
        && !a.hidden()
        && a.in_reply_to.is_none()
}

/// Épaisseur affichée : « 2 pt », « 0,5 pt ».
fn points(v: f64) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    let s = if lang::english() {
        s.to_string()
    } else {
        s.replace('.', ",")
    };
    format!("{s} pt")
}

impl Viewer {
    /// Vrai quand les annotations se sélectionnent : aucun mode qui a ses
    /// propres gestes n'est ouvert.
    pub(super) fn annot_mode(&self) -> bool {
        self.loaded.is_some()
            && !self.showing_home()
            && !self.edit_on()
            && self.objects.is_none()
            && self.sign_panel.is_none()
            && self.annot_tool.is_none()
            && self.three_d.is_none()
    }

    /// Oublie la sélection, la bulle et ce qui s'y rattache.
    pub(super) fn drop_annot_selection(&mut self) {
        if self.annot_sel.take().is_some() {
            log_line("commentaire : désélection");
        }
        self.bubble = None;
        self.annot_press = None;
        self.annot_bar.clear();
    }

    /// Un mode qui a ses propres gestes vient de s'ouvrir : la sélection
    /// d'annotation s'en va.
    pub(super) fn sanitize_annot_selection(&mut self) {
        if (self.annot_sel.is_some() || self.bubble.is_some()) && !self.annot_mode() {
            self.drop_annot_selection();
        }
    }

    /// Annotations d'une page, lues une fois puis gardées jusqu'à la
    /// prochaine modification : le survol les consulte à chaque mouvement.
    fn page_annots(&mut self, page: usize) -> Vec<AnnotationInfo> {
        self.loaded
            .as_mut()
            .map(|l| l.annots(page).to_vec())
            .unwrap_or_default()
    }

    /// Annotation de rang `index` sur `page`.
    fn annot_info(&mut self, page: usize, index: usize) -> Option<AnnotationInfo> {
        self.loaded
            .as_mut()
            .and_then(|l| l.annots(page).get(index).cloned())
    }

    /// Annotation sous un point de la vue : la plus petite qui le contient,
    /// pour qu'une note posée sur un rectangle reste atteignable. Un
    /// balisage se vise par ses zones, pas par la boîte qui les réunit ; un
    /// rectangle ou une ellipse sans fond, par son trait — le texte qu'il
    /// encadre reste sélectionnable.
    pub(super) fn annot_at(&mut self, x: i32, y: i32) -> Option<(usize, usize)> {
        let (page, pt) = self.page_at(x, y)?;
        let tol = 3.0 * self.dpi_scale / self.scale().max(1e-6);
        let near = |r: &Rect, pad: f64| {
            pt.x >= r.x0 - pad && pt.x <= r.x1 + pad && pt.y >= r.y0 - pad && pt.y <= r.y1 + pad
        };
        let mut best: Option<(f64, usize)> = None;
        for a in self.page_annots(page) {
            if !selectable(&a) {
                continue;
            }
            let hit = if follows_text(&a.subtype) && !a.quads.is_empty() {
                a.quads.iter().any(|q| near(q, tol))
            } else if matches!(a.subtype.as_str(), "Square" | "Circle") && a.fill.is_none() {
                let inset = a.border_width.unwrap_or(1.0).max(1.0) / 2.0 + 2.0 * tol;
                let inner = Rect::new(
                    a.rect.x0 + inset,
                    a.rect.y0 + inset,
                    a.rect.x1 - inset,
                    a.rect.y1 - inset,
                );
                near(&a.rect, tol)
                    && !(inner.x0 < inner.x1 && inner.y0 < inner.y1 && near(&inner, 0.0))
            } else {
                near(&a.rect, tol)
            };
            let area = a.rect.width() * a.rect.height();
            if hit && best.is_none_or(|(b, _)| area < b) {
                best = Some((area, a.index));
            }
        }
        best.map(|(_, index)| (page, index))
    }

    /// Commentaire que vise un clic droit au point `(x, y)` de la vue : celui
    /// qui est dessous, sauf si le point tombe sur du texte sélectionné. Le
    /// menu de la sélection (Copier, Surligner…) passe alors avant, comme
    /// dans Acrobat : sélectionner le commentaire effacerait la sélection
    /// qu'on voulait copier, et un passage surligné ne se copierait plus
    /// d'un clic droit.
    pub(super) fn annot_menu_at(&mut self, x: i32, y: i32) -> Option<(usize, usize)> {
        let hit = self.annot_at(x, y)?;
        if self.on_text_selection(x, y) {
            return None;
        }
        Some(hit)
    }

    /// Vrai si le point `(x, y)` de la vue tombe sur le texte sélectionné.
    /// Seule la page sous le point est lue : une sélection de tout le
    /// document ne fait pas extraire le texte de toutes ses pages.
    fn on_text_selection(&mut self, x: i32, y: i32) -> bool {
        let Some(sel) = self.selection.filter(|s| !s.is_empty()) else {
            return false;
        };
        let Some((page, pt)) = self.page_at(x, y) else {
            return false;
        };
        let tol = 2.0 * self.dpi_scale / self.scale().max(1e-6);
        let Some(l) = self.loaded.as_mut() else {
            return false;
        };
        if page >= l.pages.len() {
            return false;
        }
        let text = &l.text(page).1;
        let Some((from, to)) = sel.range_on_page(page, text.len()) else {
            return false;
        };
        text.rects(from, to).iter().any(|r| {
            pt.x >= r.x0.min(r.x1) - tol
                && pt.x <= r.x0.max(r.x1) + tol
                && pt.y >= r.y0.min(r.y1) - tol
                && pt.y <= r.y0.max(r.y1) + tol
        })
    }

    /// Sélectionne l'annotation de rang `index` sur `page`.
    pub(super) fn select_annot(&mut self, page: usize, index: usize) {
        let Some(a) = self.annot_info(page, index) else {
            return;
        };
        let rotated = self
            .loaded
            .as_ref()
            .and_then(|l| l.pages.get(page).map(|p| shown_rotation(l, p)))
            .unwrap_or(0);
        log_line(&format!(
            "commentaire : sélection page {} #{} {}",
            page + 1,
            index + 1,
            a.subtype
        ));
        // Sur une page tournée, les poignées de l'écran ne sont plus celles
        // de la page : l'annotation s'y déplace, sans se redimensionner.
        self.annot_sel = Some(AnnotSel {
            page,
            index,
            reference: a.reference,
            movable: movable(&a.subtype),
            resizable: resizable(&a.subtype) && rotated == 0,
            subtype: a.subtype,
            rect: a.rect,
            drag: None,
            hover: None,
        });
        self.selection = None;
        if self
            .bubble
            .as_ref()
            .is_some_and(|b| (b.page, b.index) != (page, index))
        {
            self.bubble = None;
        }
    }

    /// Après une modification ou une annulation : la sélection retrouve son
    /// annotation par sa référence (son rang a pu changer), ou disparaît.
    /// La bulle suit.
    pub(super) fn refresh_annot_selection(&mut self) {
        if let Some(sel) = self.annot_sel.take() {
            let list = self.page_annots(sel.page);
            let found = list.iter().find(|a| match sel.reference {
                Some(r) => a.reference == Some(r),
                None => a.index == sel.index && a.subtype == sel.subtype,
            });
            match found {
                Some(a) if selectable(a) => {
                    let moved_index = a.index;
                    if let Some(b) = self
                        .bubble
                        .as_mut()
                        .filter(|b| (b.page, b.index) == (sel.page, sel.index))
                    {
                        b.index = moved_index;
                    }
                    self.annot_sel = Some(AnnotSel {
                        index: moved_index,
                        rect: a.rect,
                        drag: None,
                        ..sel
                    });
                }
                _ => {
                    log_line("commentaire : la sélection n'existe plus");
                    self.bubble = None;
                }
            }
        }
        let bubble_ok = self.bubble.as_ref().is_none_or(|b| {
            self.loaded.as_ref().is_some_and(|l| {
                l.comments
                    .iter()
                    .any(|c| (c.page, c.index) == (b.page, b.index))
            })
        });
        if !bubble_ok {
            self.bubble = None;
        }
    }

    /// Rectangle de la sélection dans la vue, geste compris.
    fn annot_view_rect(&self) -> Option<ViewRect> {
        let sel = self.annot_sel.as_ref()?;
        let rect = sel.drag.as_ref().map_or(sel.rect, |d| d.current);
        self.page_rect_to_view(sel.page, rect)
    }

    /// Nombre de clics de la série en cours, sans la compter : le clic
    /// ira ensuite à la sélection du texte, qui la compte elle-même.
    fn peek_series(&self, x: i32, y: i32, clicks: u8) -> u8 {
        if clicks >= 2 {
            return clicks;
        }
        match self.last_click {
            Some((t, lx, ly, n))
                if t.elapsed() < super::MULTI_CLICK && (x - lx).abs() < 4 && (y - ly).abs() < 4 =>
            {
                n + 1
            }
            _ => 1,
        }
    }

    /// Clic gauche sur la page. Rend vrai s'il l'a pris ; faux laisse le
    /// clic à la suite (champs, liens, sélection du texte).
    pub(super) fn annot_mouse_down(&mut self, x: i32, y: i32, clicks: u8) -> bool {
        if !self.annot_mode() {
            return false;
        }
        self.annot_press = None;
        let dpi = self.dpi_scale;
        let series = self.peek_series(x, y, clicks);
        // Sur la sélection : une poignée, ou le corps.
        if let (Some(view), Some((_, point))) = (self.annot_view_rect(), self.page_at(x, y)) {
            let handle = objects_ui::handle_at(view, f64::from(x), f64::from(y), dpi);
            if let (Some(h), Some(sel)) = (handle, self.annot_sel.as_mut()) {
                let takes = if h == Handle::Body {
                    sel.movable || !follows_text(&sel.subtype)
                } else {
                    sel.resizable
                };
                if takes {
                    if h == Handle::Body && series >= 2 {
                        let (page, index) = (sel.page, sel.index);
                        self.open_bubble(page, index, false);
                        return true;
                    }
                    if sel.movable || h != Handle::Body {
                        sel.drag = Some(AnnotDrag {
                            handle: h,
                            from: point,
                            current: sel.rect,
                            moved: false,
                            ghost: None,
                        });
                    }
                    // Le clic compte : un double-clic sur la forme ouvre sa
                    // bulle.
                    let _ = self.click_series(x, y, clicks);
                    return true;
                }
            }
        }
        let Some((page, index)) = self.annot_at(x, y) else {
            // Rien dessous : on désélectionne, et le clic continue.
            if self.annot_sel.is_some() || self.bubble.is_some() {
                self.drop_annot_selection();
            }
            return false;
        };
        let Some(info) = self.annot_info(page, index) else {
            return false;
        };
        if follows_text(&info.subtype) {
            self.drop_annot_selection();
            if series >= 2 {
                // Double-clic sur un passage balisé : sa bulle, pas le mot.
                self.select_annot(page, index);
                self.open_bubble(page, index, false);
                return true;
            }
            // Le texte dessous reste sélectionnable : c'est le relâchement
            // sans glisser qui sélectionnera le balisage.
            self.annot_press = Some((page, index));
            return false;
        }
        let series = self.click_series(x, y, clicks);
        self.select_annot(page, index);
        if info.subtype == "Text" || series >= 2 {
            self.open_bubble(page, index, false);
        }
        let at = self.page_at(x, y);
        if let (Some(sel), Some((_, point))) = (self.annot_sel.as_mut(), at) {
            if sel.movable {
                sel.drag = Some(AnnotDrag {
                    handle: Handle::Body,
                    from: point,
                    current: sel.rect,
                    moved: false,
                    ghost: None,
                });
            }
        }
        true
    }

    /// Relâchement ordinaire : un clic sur un balisage, sans glisser ni
    /// sélectionner de texte, le sélectionne.
    pub(super) fn annot_markup_release(&mut self) {
        let Some((page, index)) = self.annot_press.take() else {
            return;
        };
        if self.selection.is_none_or(|s| s.is_empty()) && self.annot_mode() {
            self.selection = None;
            self.select_annot(page, index);
        }
    }

    /// Suit le geste en cours sur l'annotation sélectionnée.
    pub(super) fn annot_mouse_move(&mut self, x: i32, y: i32, shift: bool) {
        let Some((_, point)) = self.page_at(x, y) else {
            return;
        };
        let scale = self.scale();
        let Some(sel) = self.annot_sel.as_mut() else {
            return;
        };
        let (rect, subtype) = (sel.rect, sel.subtype.clone());
        let Some(drag) = sel.drag.as_mut() else {
            return;
        };
        let (dx, dy) = (point.x - drag.from.x, point.y - drag.from.y);
        // Deux pixels d'écran : en deçà, c'est un clic qui tremble.
        if !drag.moved && dx.hypot(dy) * scale < 2.0 {
            return;
        }
        drag.moved = true;
        let keep_ratio = shift || subtype == "Stamp";
        drag.current = objects_ui::resized(rect, drag.handle, dx, dy, keep_ratio);
        let size = |r: &Rect| {
            (
                (r.width() * scale).round() as i64,
                (r.height() * scale).round() as i64,
            )
        };
        let outdated = drag
            .ghost
            .as_ref()
            .is_none_or(|(r, _)| size(r) != size(&drag.current));
        if outdated {
            let target = drag.current;
            let (page, index) = (sel.page, sel.index);
            let ghost = self.loaded.as_ref().and_then(|l| {
                let p = l.pages.get(page)?;
                acrux_render::render_annotation(
                    &l.doc,
                    p,
                    index,
                    scale,
                    shown_rotation(l, p),
                    target,
                )
            });
            if let Some(drag) = self.annot_sel.as_mut().and_then(|s| s.drag.as_mut()) {
                drag.ghost = ghost.map(|b| (target, b));
            }
        }
    }

    /// Fin du geste : le nouveau rectangle est écrit, en une modification.
    pub(super) fn annot_mouse_up(&mut self) {
        let Some(sel) = self.annot_sel.as_mut() else {
            return;
        };
        let Some(drag) = sel.drag.take() else {
            return;
        };
        let (from, to) = (sel.rect, drag.current);
        let changed = (to.x0 - from.x0).abs() > 0.5
            || (to.y0 - from.y0).abs() > 0.5
            || (to.width() - from.width()).abs() > 0.5
            || (to.height() - from.height()).abs() > 0.5;
        if !drag.moved || !changed {
            return;
        }
        let (page, index) = (sel.page, sel.index);
        let what = if drag.handle == Handle::Body {
            "déplacé"
        } else {
            "redimensionné"
        };
        let op = EditOp::AnnotSet {
            page,
            index,
            changes: AnnotChanges {
                rect: Some(to),
                date: Some(pdf_date_now()),
                ..AnnotChanges::default()
            },
        };
        let ghost = drag.ghost.filter(|(r, _)| *r == to);
        if self.apply_annot_edit(op, ghost) {
            log_line(&format!(
                "commentaire : {what} page {} #{} [{:.0} {:.0} {:.0} {:.0}]",
                page + 1,
                index + 1,
                to.x0,
                to.y0,
                to.x1,
                to.y1
            ));
        }
    }

    /// Applique une modification d'annotation. L'image de la page, avant,
    /// reste à l'écran le temps que le fil de rendu la refasse (voir
    /// `apply_edit`) ; `ghost` montre par-dessus l'annotation à sa nouvelle
    /// place, pour qu'elle ne semble pas revenir en arrière.
    fn apply_annot_edit(&mut self, op: EditOp, ghost: Option<(Rect, Bitmap)>) -> bool {
        let page = match &op {
            EditOp::AnnotSet { page, .. }
            | EditOp::AnnotRemove { page, .. }
            | EditOp::AnnotReply { page, .. }
            | EditOp::AnnotState { page, .. } => *page,
            _ => self.current_page(),
        };
        let key_scale = (self.scale() * 1000.0).round() as u32;
        if !self.apply_edit(op) {
            return false;
        }
        self.annot_ghost = ghost.map(|(rect, bitmap)| AnnotGhost {
            page,
            key_scale,
            rect,
            bitmap,
        });
        true
    }

    /// Touche quand une annotation est sélectionnée. Rend vrai si elle l'a
    /// prise.
    pub(super) fn annot_key(&mut self, key: Key, m: Modifiers) -> bool {
        if !self.annot_mode() || m.alt {
            return false;
        }
        let Some(sel) = self.annot_sel.as_ref() else {
            return false;
        };
        let (page, index, movable, rect) = (sel.page, sel.index, sel.movable, sel.rect);
        match key {
            Key::Delete | Key::Backspace if !m.ctrl => {
                self.delete_selected_annot();
                true
            }
            Key::Escape => {
                if self.bubble.take().is_none() {
                    self.drop_annot_selection();
                }
                true
            }
            Key::Enter => {
                self.open_bubble(page, index, false);
                true
            }
            Key::Left | Key::Right | Key::Up | Key::Down if movable && !m.ctrl => {
                let step = if m.shift { 10.0 } else { 1.0 };
                // La flèche dit une direction de l'écran (y vers le bas) :
                // sur une page tournée, ou dans une vue pivotée, ce n'est
                // plus un axe de la page. Le décalage passe donc par
                // l'inverse de la matrice d'affichage — sans elle, → faisait
                // descendre l'annotation dans une vue pivotée d'un quart de
                // tour.
                let (sx, sy) = match key {
                    Key::Left => (-step, 0.0),
                    Key::Right => (step, 0.0),
                    Key::Up => (0.0, -step),
                    _ => (0.0, step),
                };
                let rotation = self
                    .loaded
                    .as_ref()
                    .and_then(|l| l.pages.get(page).map(|p| shown_rotation(l, p)))
                    .unwrap_or(0);
                let screen = acrux_render::page::base_matrix(&rect, 1.0, rotation, 1, 1);
                let Some(delta) = screen
                    .invert()
                    .map(|inv| inv.apply_vector(Point::new(sx, sy)))
                else {
                    return true;
                };
                let (dx, dy) = (delta.x, delta.y);
                let to = Rect::new(rect.x0 + dx, rect.y0 + dy, rect.x1 + dx, rect.y1 + dy);
                self.apply_annot_edit(
                    EditOp::AnnotSet {
                        page,
                        index,
                        changes: AnnotChanges {
                            rect: Some(to),
                            date: Some(pdf_date_now()),
                            ..AnnotChanges::default()
                        },
                    },
                    None,
                );
                true
            }
            _ => false,
        }
    }

    /// Commentaire de la liste du panneau qui correspond à `(page, index)`.
    fn comment_row(&self, page: usize, index: usize) -> Option<usize> {
        self.loaded.as_ref().and_then(|l| {
            l.comments
                .iter()
                .position(|c| (c.page, c.index) == (page, index))
        })
    }

    /// Supprime l'annotation sélectionnée ; si elle a des réponses, après
    /// confirmation, puisqu'elles partent avec elle.
    pub(super) fn delete_selected_annot(&mut self) {
        let Some((page, index)) = self.annot_sel.as_ref().map(|s| (s.page, s.index)) else {
            return;
        };
        let replies = self
            .comment_row(page, index)
            .and_then(|i| self.loaded.as_ref()?.comments.get(i))
            .map_or(0, |c| c.replies.len());
        if replies > 0 {
            self.confirm(
                tr("Supprimer le commentaire"),
                &trf(
                    "Supprimer le commentaire et ses {} réponse(s) ?",
                    &[&replies.to_string()],
                ),
                tr("Supprimer"),
                Then::DeleteComment(page, index),
            );
            return;
        }
        self.remove_annot(page, index);
    }

    /// Supprime l'annotation `(page, index)`, réponses comprises.
    pub(super) fn remove_annot(&mut self, page: usize, index: usize) {
        if self.apply_annot_edit(EditOp::AnnotRemove { page, index }, None) {
            log_line(&format!(
                "commentaire : supprimé page {} #{}",
                page + 1,
                index + 1
            ));
            self.drop_annot_selection();
            self.set_notice(tr("Commentaire supprimé — Ctrl+Z le rétablit").into());
        }
    }

    // --- Barre de propriétés ------------------------------------------------

    /// Réglages de la barre pour l'annotation sélectionnée : ce qu'ils
    /// changent, et comment la barre les montre.
    fn annot_settings(&mut self) -> Vec<(DrawSetting, Setting)> {
        let Some((page, index)) = self.annot_sel.as_ref().map(|s| (s.page, s.index)) else {
            return Vec::new();
        };
        let Some(a) = self.annot_info(page, index) else {
            return Vec::new();
        };
        let color = |c: Option<[f64; 3]>, swatch| Setting::Color {
            color: c.map(preview_rgb),
            swatch,
        };
        let stroke = (DrawSetting::Stroke, color(a.color, Swatch::Stroke));
        let fill = (DrawSetting::Fill, color(a.fill, Swatch::Fill));
        let width = (
            DrawSetting::Width,
            Setting::Choice(points(a.border_width.unwrap_or(1.0))),
        );
        let opacity = (
            DrawSetting::Opacity,
            Setting::Choice(format!("{} %", (a.opacity * 100.0).round())),
        );
        match a.subtype.as_str() {
            "Square" | "Circle" | "Polygon" => vec![stroke, fill, width, opacity],
            "Line" | "PolyLine" | "Ink" => vec![stroke, width, opacity],
            "Highlight" | "Text" | "Caret" => {
                vec![(DrawSetting::Stroke, color(a.color, Swatch::Fill)), opacity]
            }
            "Underline" | "StrikeOut" | "Squiggly" => vec![stroke, opacity],
            "FreeText" => vec![
                (DrawSetting::TextColor, color(a.color, Swatch::Text)),
                (DrawSetting::TextFill, color(a.fill, Swatch::Fill)),
                opacity,
            ],
            "Redact" => Vec::new(),
            _ => vec![opacity],
        }
    }

    /// Couleur en cours d'un réglage de l'annotation sélectionnée, pour le
    /// nuancier.
    pub(super) fn annot_current_color(&mut self, target: DrawSetting) -> Option<[f64; 3]> {
        let (page, index) = self.annot_sel.as_ref().map(|s| (s.page, s.index))?;
        let a = self.annot_info(page, index)?;
        match target {
            DrawSetting::Fill | DrawSetting::TextFill => a.fill,
            _ => a.color,
        }
    }

    /// Déroule le nuancier ou la liste du réglage `index` de la barre.
    fn open_annot_setting(&mut self, index: usize, anchor: (i32, i32, i32, i32)) {
        let settings = self.annot_settings();
        let Some(&(target, _)) = settings.get(index) else {
            return;
        };
        let Some((page, sel_index)) = self.annot_sel.as_ref().map(|s| (s.page, s.index)) else {
            return;
        };
        let Some(a) = self.annot_info(page, sel_index) else {
            return;
        };
        let popup = match target {
            DrawSetting::Width | DrawSetting::Opacity => {
                let (values, current): (&[f64], f64) = if target == DrawSetting::Width {
                    (&WIDTHS, a.border_width.unwrap_or(1.0))
                } else {
                    (&OPACITIES, a.opacity)
                };
                let mut menu = Menu::below(anchor);
                for v in values {
                    let label = if target == DrawSetting::Width {
                        points(*v)
                    } else {
                        format!("{} %", (v * 100.0).round())
                    };
                    menu = menu.item(None, &label, "", Choice::Number(*v), true);
                }
                menu.highlight_where(|c| c == Choice::Number(current));
                self.layout_menu(&mut menu);
                DrawPopup::List { target, menu }
            }
            _ => {
                let current = match target {
                    DrawSetting::Fill | DrawSetting::TextFill => a.fill,
                    _ => a.color,
                };
                let picker = ColorPicker::new(current.unwrap_or([1.0, 1.0, 1.0]));
                let picker = match target.none_label() {
                    Some(label) => picker.with_none(label, current.is_none()),
                    None => picker,
                };
                DrawPopup::Colors {
                    target,
                    picker: Box::new(picker),
                    anchor,
                }
            }
        };
        log_line(&format!("commentaire : réglage {target:?}"));
        self.tip = None;
        self.draw_popup = Some(popup);
        self.popup_for_annot = true;
    }

    /// Met en page une liste déroulante avant qu'elle ne s'affiche : un clic
    /// qui arriverait avant la première peinture la trouve à sa place.
    pub(super) fn layout_menu<T: Copy>(&mut self, menu: &mut Menu<T>) {
        let (w, h) = (self.width as i32, self.height as i32);
        let (font, dpi) = (self.theme.font_size, self.dpi_scale as f32);
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
    }

    /// Couleur choisie dans le nuancier de la barre (`None` : aucune).
    pub(super) fn annot_popup_color(&mut self, target: DrawSetting, color: Option<[f64; 3]>) {
        let changes = match target {
            DrawSetting::Fill | DrawSetting::TextFill => AnnotChanges {
                fill: Some(color),
                ..AnnotChanges::default()
            },
            _ => match color {
                Some(c) => AnnotChanges {
                    color: Some(c),
                    ..AnnotChanges::default()
                },
                None => return,
            },
        };
        self.set_selected_annot(changes, "couleur");
    }

    /// Valeur choisie dans une liste de la barre.
    pub(super) fn annot_popup_choice(&mut self, target: DrawSetting, choice: Choice) {
        let Choice::Number(v) = choice else { return };
        let changes = match target {
            DrawSetting::Width => AnnotChanges {
                width: Some(v),
                ..AnnotChanges::default()
            },
            DrawSetting::Opacity => AnnotChanges {
                opacity: Some(v),
                ..AnnotChanges::default()
            },
            _ => return,
        };
        let what = if target == DrawSetting::Width {
            "épaisseur"
        } else {
            "opacité"
        };
        self.set_selected_annot(changes, what);
    }

    /// Modifie l'annotation sélectionnée.
    fn set_selected_annot(&mut self, mut changes: AnnotChanges, what: &str) {
        let Some((page, index)) = self.annot_sel.as_ref().map(|s| (s.page, s.index)) else {
            return;
        };
        changes.date = Some(pdf_date_now());
        if self.apply_annot_edit(
            EditOp::AnnotSet {
                page,
                index,
                changes,
            },
            None,
        ) {
            log_line(&format!(
                "commentaire : {what} page {} #{}",
                page + 1,
                index + 1
            ));
        }
    }

    /// Ouvre l'invite du texte de l'annotation sélectionnée.
    pub(super) fn edit_annot_text(&mut self) {
        let Some((page, index)) = self.annot_sel.as_ref().map(|s| (s.page, s.index)) else {
            return;
        };
        let current = self
            .annot_info(page, index)
            .and_then(|a| a.contents)
            .unwrap_or_default();
        let mut input = TextInput::new(tr("Votre commentaire"));
        input.set_value(&current);
        input.select_all();
        self.prompt = Some(Prompt::new(
            tr("Texte du commentaire"),
            trf(
                "Texte du commentaire (page {}) :",
                &[&(page + 1).to_string()],
            ),
            input,
            PromptKind::AnnotText { page, index },
        ));
    }

    /// Le texte de l'invite est validé.
    pub(super) fn set_annot_text(&mut self, page: usize, index: usize, text: String) {
        if self.apply_annot_edit(
            EditOp::AnnotSet {
                page,
                index,
                changes: AnnotChanges {
                    contents: Some(text),
                    date: Some(pdf_date_now()),
                    ..AnnotChanges::default()
                },
            },
            None,
        ) {
            log_line(&format!(
                "commentaire : texte page {} #{}",
                page + 1,
                index + 1
            ));
        }
    }

    // --- La bulle -----------------------------------------------------------

    /// Ouvre la bulle du commentaire `(page, index)` ; `typing` ouvre tout
    /// de suite le champ de réponse.
    pub(super) fn open_bubble(&mut self, page: usize, index: usize, typing: bool) {
        match &mut self.bubble {
            Some(b) if (b.page, b.index) == (page, index) => {
                if typing {
                    b.typing = true;
                    b.reply.focused = true;
                }
            }
            _ => self.bubble = Some(NoteBubble::new(page, index, typing)),
        }
        log_line(&format!(
            "commentaire : bulle page {} #{}",
            page + 1,
            index + 1
        ));
    }

    /// Vrai quand une réponse se tape dans la bulle.
    pub(super) fn bubble_typing(&self) -> bool {
        self.bubble.as_ref().is_some_and(|b| b.typing)
    }

    /// Touche pendant la frappe d'une réponse.
    pub(super) fn bubble_key(&mut self, key: Key, m: Modifiers) -> bool {
        if m.ctrl {
            return false;
        }
        let Some(b) = self.bubble.as_mut() else {
            return false;
        };
        let action = b.key(key, m.shift);
        self.bubble_action(action);
        true
    }

    /// Caractère tapé dans la réponse.
    pub(super) fn bubble_char(&mut self, c: char) {
        if let Some(b) = self.bubble.as_mut() {
            b.char(c);
        }
    }

    /// Colle dans la réponse ; vrai si la bulle attendait du texte.
    pub(super) fn bubble_paste(&mut self, text: &str) -> bool {
        match self.bubble.as_mut() {
            Some(b) if b.typing => {
                b.paste(text);
                true
            }
            _ => false,
        }
    }

    /// Ce que la bulle a demandé.
    fn bubble_action(&mut self, action: BubbleAction) {
        match action {
            BubbleAction::None | BubbleAction::StartReply => {}
            BubbleAction::Close => self.bubble = None,
            BubbleAction::Publish(text) => {
                let Some((page, index)) = self.bubble.as_ref().map(|b| (b.page, b.index)) else {
                    return;
                };
                let meta = AnnotMeta::fresh(author_name().as_deref());
                if self.apply_annot_edit(
                    EditOp::AnnotReply {
                        page,
                        index,
                        text,
                        meta,
                    },
                    None,
                ) {
                    log_line(&format!(
                        "commentaire : réponse publiée page {} #{}",
                        page + 1,
                        index + 1
                    ));
                    self.set_notice(tr("Réponse publiée").into());
                }
            }
        }
    }

    /// Clic sur la barre de propriétés ou sur la bulle, qui flottent
    /// au-dessus de la page. Rend vrai s'il les a touchées.
    pub(super) fn annot_overlay_mouse_down(&mut self, x: i32, y: i32) -> bool {
        if let Some(b) = self.bubble.as_mut() {
            if let Some(action) = b.mouse_down(x, y) {
                self.bubble_action(action);
                return true;
            }
            // Un clic ailleurs referme le champ de réponse : ce qui y était
            // tapé reste dans la bulle jusqu'à sa fermeture.
            b.typing = false;
            b.reply.focused = false;
        }
        if self.annot_sel.is_none() || !self.annot_bar.contains(x, y) {
            return false;
        }
        match self.annot_bar.hit(x, y) {
            Some(BarItem::Setting(i)) => {
                if let Some(r) = self.annot_bar.setting_rect(i) {
                    let (left, top) = (self.view_left() as i32, self.view_top() as i32);
                    self.open_annot_setting(i, (r.0 + left, r.1 + top, r.2, r.3));
                }
            }
            Some(BarItem::EditText) => self.edit_annot_text(),
            Some(BarItem::Reply) => {
                if let Some((page, index)) = self.annot_sel.as_ref().map(|s| (s.page, s.index)) {
                    self.open_bubble(page, index, true);
                }
            }
            Some(BarItem::Delete) => self.delete_selected_annot(),
            None => {}
        }
        true
    }

    /// Info-bulle d'un bouton de la barre de propriétés, en coordonnées de
    /// la fenêtre : ses boutons ne sont que des icônes et des pastilles, que
    /// seul ce texte explique.
    pub(super) fn annot_bar_tip(&mut self) -> Option<(String, (i32, i32, i32, i32))> {
        let subtype = self.annot_sel.as_ref()?.subtype.clone();
        let (item, r) = self.annot_bar.hovered()?;
        let label = match item {
            BarItem::Setting(i) => match self.annot_settings().get(i)?.0 {
                // Une note, un surlignage, un signe d'insertion n'ont pas de
                // trait : leur pastille est leur couleur.
                DrawSetting::Stroke
                    if !matches!(
                        subtype.as_str(),
                        "Square" | "Circle" | "Polygon" | "Line" | "PolyLine" | "Ink"
                    ) =>
                {
                    "Couleur"
                }
                target => target.label(),
            },
            BarItem::EditText => "Modifier le texte…",
            BarItem::Reply => "Répondre",
            BarItem::Delete => "Supprimer le commentaire",
        };
        let (left, top) = (self.view_left() as i32, self.view_top() as i32);
        Some((tr(label).to_string(), (r.0 + left, r.1 + top, r.2, r.3)))
    }

    /// Molette au-dessus de la bulle : son fil défile. Vrai si elle l'a
    /// prise (coordonnées de la fenêtre).
    pub(super) fn bubble_wheel(&mut self, x: i32, y: i32, delta: f32) -> bool {
        let (vx, vy) = (x - self.view_left() as i32, y - self.view_top() as i32);
        match self.bubble.as_mut() {
            Some(b) if b.contains(vx, vy) => {
                b.wheel(delta);
                true
            }
            _ => false,
        }
    }

    /// Survol (coordonnées de la vue) : poignées, barre, bulle. Vrai si
    /// l'image change.
    pub(super) fn annot_hover(&mut self, x: i32, y: i32) -> bool {
        let mut changed = self.annot_bar.mouse_move(x, y);
        if let Some(b) = self.bubble.as_mut() {
            changed |= b.mouse_move(x, y);
        }
        let hover = self.annot_view_rect().and_then(|v| {
            objects_ui::handle_at(v, f64::from(x), f64::from(y), self.dpi_scale)
                .filter(|h| *h != Handle::Body)
        });
        if let Some(sel) = self.annot_sel.as_mut() {
            let hover = hover.filter(|_| sel.resizable);
            changed |= sel.hover != hover;
            sel.hover = hover;
        }
        changed
    }

    /// Pointeur au-dessus d'une annotation, d'une poignée, de la barre ou
    /// de la bulle ; `None` ailleurs (et sur un balisage, où le texte
    /// dessous se sélectionne).
    pub(super) fn annot_cursor(&mut self, x: i32, y: i32) -> Option<Cursor> {
        if !self.annot_mode() {
            return None;
        }
        if self.annot_sel.is_some() && self.annot_bar.contains(x, y)
            || self.bubble.as_ref().is_some_and(|b| b.contains(x, y))
        {
            return Some(Cursor::Arrow);
        }
        if let (Some(view), Some(sel)) = (self.annot_view_rect(), self.annot_sel.as_ref()) {
            match objects_ui::handle_at(view, f64::from(x), f64::from(y), self.dpi_scale) {
                Some(Handle::Body) if sel.movable => return Some(Cursor::Move),
                Some(h) if h != Handle::Body && sel.resizable => return Some(h.cursor()),
                _ => {}
            }
        }
        let (page, index) = self.annot_at(x, y)?;
        let a = self.annot_info(page, index)?;
        if follows_text(&a.subtype) {
            None
        } else if movable(&a.subtype) {
            Some(Cursor::Move)
        } else {
            Some(Cursor::Arrow)
        }
    }

    // --- Dessin ---------------------------------------------------------------

    /// L'annotation modifiée, à sa nouvelle place, tant que la page
    /// n'est pas revenue du fil de rendu.
    pub(super) fn paint_annot_ghost(&mut self, frame: &mut Frame<'_>) {
        let Some(ghost) = self.annot_ghost.take() else {
            return;
        };
        let current = (self.scale() * 1000.0).round() as u32;
        let arrived = self
            .loaded
            .as_ref()
            .is_none_or(|l| l.cache.contains_key(&(ghost.page, ghost.key_scale)));
        if arrived || current != ghost.key_scale {
            return;
        }
        if let Some(v) = self.page_rect_to_view(ghost.page, ghost.rect) {
            let b = &ghost.bitmap;
            frame.blit_rgba_premultiplied(
                v.x.round() as i32,
                v.y.round() as i32,
                b.width(),
                b.height(),
                b.data(),
            );
        }
        self.annot_ghost = Some(ghost);
    }

    /// La sélection : pendant un geste, l'annotation elle-même à sa
    /// nouvelle place ; puis le cadre et ses poignées.
    pub(super) fn paint_annot_sel(&mut self, frame: &mut Frame<'_>) {
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        let Some(view) = self.annot_view_rect() else {
            return;
        };
        let Some(sel) = self.annot_sel.as_ref() else {
            return;
        };
        if let Some((_, b)) = sel.drag.as_ref().and_then(|d| d.ghost.as_ref()) {
            frame.blit_rgba_premultiplied(
                view.x.round() as i32,
                view.y.round() as i32,
                b.width(),
                b.height(),
                b.data(),
            );
        }
        // Un léger débord : le cadre ne recouvre pas le trait qu'il entoure.
        let grow = f64::from(dpi) * 2.0;
        let framed = ViewRect {
            x: view.x - grow,
            y: view.y - grow,
            w: view.w + 2.0 * grow,
            h: view.h + 2.0 * grow,
        };
        if sel.resizable {
            let handle = sel.drag.as_ref().map(|d| d.handle).or(sel.hover);
            objects_ui::paint_selection(frame, &theme, dpi, framed, handle);
        } else {
            let (x, y, w, h) = (
                framed.x.round() as i32,
                framed.y.round() as i32,
                framed.w.round() as i32,
                framed.h.round() as i32,
            );
            crate::ui::paint::round_rect_outline(
                frame,
                x,
                y,
                w,
                h,
                3.0 * dpi,
                (1.5 * dpi).max(1.0),
                theme.accent,
            );
        }
    }

    /// La barre de propriétés et la bulle, par-dessus tout ce qui est
    /// dans la vue.
    pub(super) fn paint_annot_overlays(&mut self, frame: &mut Frame<'_>) {
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        if self.annot_sel.as_ref().is_some_and(|s| s.drag.is_none()) {
            if let Some(view) = self.annot_view_rect() {
                let pairs = self.annot_settings();
                let settings: Vec<Setting> = pairs.iter().map(|(_, s)| s.clone()).collect();
                let open = if self.popup_for_annot {
                    self.draw_popup
                        .as_ref()
                        .map(DrawPopup::target)
                        .and_then(|t| pairs.iter().position(|(d, _)| *d == t))
                } else {
                    None
                };
                let anchor = (
                    view.x.round() as i32,
                    view.y.round() as i32,
                    view.w.round() as i32,
                    view.h.round() as i32,
                );
                if let Some(text) = self.text.as_mut() {
                    self.annot_bar.paint(
                        frame,
                        text,
                        &mut self.raster,
                        &theme,
                        dpi,
                        anchor,
                        &settings,
                        open,
                        true,
                    );
                }
            } else {
                self.annot_bar.clear();
            }
        } else {
            self.annot_bar.clear();
        }
        let Some((page, index)) = self.bubble.as_ref().map(|b| (b.page, b.index)) else {
            return;
        };
        let Some(row) = self
            .loaded
            .as_ref()
            .and_then(|l| {
                l.comments
                    .iter()
                    .find(|c| (c.page, c.index) == (page, index))
            })
            .cloned()
        else {
            return;
        };
        let rect = self
            .annot_sel
            .as_ref()
            .filter(|s| (s.page, s.index) == (page, index))
            .map_or(row.rect, |s| s.rect);
        let Some(view) = self.page_rect_to_view(page, rect) else {
            return;
        };
        let anchor = (
            view.x.round() as i32,
            view.y.round() as i32,
            view.w.round() as i32,
            view.h.round() as i32,
        );
        let thread = Thread {
            kind: tr(row.kind),
            author: row.author.as_deref().unwrap_or_default(),
            date: &row.date,
            contents: &row.contents,
            color: row.color,
            status: row.status.map(tr),
            marked: row.marked,
            replies: &row.replies,
        };
        if let (Some(b), Some(text)) = (self.bubble.as_mut(), self.text.as_mut()) {
            b.paint(frame, text, &mut self.raster, &theme, dpi, anchor, &thread);
        }
    }

    // --- Statuts, cases, panneau ----------------------------------------------

    /// Donne un statut au commentaire d'indice `row` dans la liste du
    /// panneau, ou coche sa case.
    pub(super) fn set_comment_state(&mut self, row: usize, change: StateChange) {
        let Some((page, index)) = self
            .loaded
            .as_ref()
            .and_then(|l| l.comments.get(row))
            .map(|c| (c.page, c.index))
        else {
            return;
        };
        let meta = AnnotMeta::fresh(author_name().as_deref());
        if self.apply_annot_edit(
            EditOp::AnnotState {
                page,
                index,
                change,
                meta,
            },
            None,
        ) {
            let label = match change {
                StateChange::Review(s) => trf("Statut : {}", &[tr(s.label())]),
                StateChange::Marked(true) => tr("Commentaire coché").to_string(),
                StateChange::Marked(false) => tr("Commentaire décoché").to_string(),
            };
            log_line(&format!(
                "commentaire : {change:?} page {} #{}",
                page + 1,
                index + 1
            ));
            self.set_notice(label);
        }
    }

    /// Coche ou décoche le commentaire d'indice `row`.
    pub(super) fn toggle_comment_mark(&mut self, row: usize) {
        let marked = self
            .loaded
            .as_ref()
            .and_then(|l| l.comments.get(row))
            .is_some_and(|c| c.marked);
        self.set_comment_state(row, StateChange::Marked(!marked));
    }

    /// Statut ou case de l'annotation sélectionnée (menu, palette).
    pub(super) fn selected_comment_state(&mut self, change: Option<StateChange>) {
        let Some((page, index)) = self.annot_sel.as_ref().map(|s| (s.page, s.index)) else {
            self.set_notice(tr("Sélectionnez d'abord un commentaire").into());
            return;
        };
        let Some(row) = self.comment_row(page, index) else {
            return;
        };
        match change {
            Some(c) => self.set_comment_state(row, c),
            None => self.toggle_comment_mark(row),
        }
    }

    /// Mène au commentaire d'indice `row` : la vue montre son rectangle,
    /// l'annotation est sélectionnée, et la bulle d'une note s'ouvre.
    pub(super) fn go_to_comment(&mut self, row: usize) {
        let target = self
            .loaded
            .as_ref()
            .and_then(|l| l.comments.get(row))
            .map(|c| (c.page, c.rect, c.index, c.name.clone(), c.kind));
        let Some((page, rect, at, name, kind)) = target else {
            return;
        };
        log_line(&format!(
            "panneau : commentaire #{} de la page {} ({})",
            at + 1,
            page + 1,
            name.as_deref().unwrap_or("sans identifiant")
        ));
        // Au passage commenté, pas en haut de sa page : sur une page
        // longue, on ne le trouverait pas.
        self.navigate(|v| {
            v.scroll_to_page(page);
            v.reveal_page_rect(page, rect);
        });
        if self.annot_mode() {
            self.select_annot(page, at);
            if kind == "Note" {
                self.open_bubble(page, at, false);
            }
        }
    }

    /// Déroule une liste du panneau des commentaires, sous `rect`
    /// (coordonnées du panneau).
    pub(super) fn open_comment_menu(&mut self, kind: CommentMenu, rect: (i32, i32, i32, i32)) {
        let top = self.view_top() as i32;
        let anchor = (rect.0, rect.1 + top, rect.2, rect.3);
        let Some(l) = self.loaded.as_ref() else {
            return;
        };
        let view = &self.panel.comments;
        let check = |on: bool| on.then_some(Icon::Check);
        let mut authors: Vec<String> = Vec::new();
        let mut menu = Menu::below(anchor);
        match kind {
            CommentMenu::Sort => {
                for sort in CommentSort::ALL {
                    menu = menu.item(
                        check(view.sort == sort),
                        tr(sort.label()),
                        "",
                        CommentPick::Sort(sort),
                        true,
                    );
                }
            }
            CommentMenu::Filter => {
                let mut kinds: Vec<&'static str> = Vec::new();
                for c in &l.comments {
                    if !kinds.contains(&c.kind) {
                        kinds.push(c.kind);
                    }
                    if let Some(a) = c.author.as_ref().filter(|a| !a.is_empty()) {
                        if !authors.contains(a) {
                            authors.push(a.clone());
                        }
                    }
                }
                authors.sort_by_key(|a| a.to_lowercase());
                menu = menu.item(
                    check(view.hidden.is_empty()),
                    tr("Tous les types"),
                    "",
                    CommentPick::AllKinds,
                    true,
                );
                for k in kinds {
                    menu = menu.item(
                        check(!view.hidden.contains(&k)),
                        tr(k),
                        "",
                        CommentPick::Kind(k),
                        true,
                    );
                }
                menu = menu.separator().item(
                    check(view.author.is_none()),
                    tr("Tous les auteurs"),
                    "",
                    CommentPick::Author(None),
                    true,
                );
                for (i, a) in authors.iter().enumerate() {
                    menu = menu.item(
                        check(view.author.as_ref() == Some(a)),
                        a,
                        "",
                        CommentPick::Author(Some(i)),
                        true,
                    );
                }
            }
            CommentMenu::Status(row) => {
                let current = l.comments.get(row).and_then(|c| c.status);
                for state in ReviewState::ALL {
                    let on = match state {
                        ReviewState::None => current.is_none(),
                        s => current == Some(s.label()),
                    };
                    menu = menu.item(
                        check(on),
                        tr(state.label()),
                        "",
                        CommentPick::Status(row, state),
                        true,
                    );
                }
            }
        }
        self.layout_menu(&mut menu);
        log_line(&format!("panneau : liste {kind:?}"));
        self.tip = None;
        self.comment_menu = Some(CommentMenuOpen { menu, authors });
    }

    /// Un événement pour la liste déroulée du panneau des commentaires : elle
    /// prend tout, comme la liste du zoom. Vrai si elle l'a pris.
    pub(super) fn comment_menu_event(
        &mut self,
        event: &Event,
        window: &mut dyn WindowHandle,
    ) -> bool {
        let Some(open) = self.comment_menu.as_mut() else {
            return false;
        };
        let outcome = match *event {
            Event::MouseDown { x, y, .. } => open.menu.mouse_down(x, y),
            Event::MouseUp { button, x, y } => {
                if button == MouseButton::Left {
                    open.menu.mouse_up(x, y)
                } else {
                    Outcome::Stay
                }
            }
            Event::MouseMove { x, y, .. } => {
                if open.menu.mouse_move(x, y) {
                    window.request_redraw();
                }
                window.set_cursor(Cursor::Arrow);
                return true;
            }
            Event::Wheel { delta, .. } => {
                open.menu.wheel(delta);
                Outcome::Stay
            }
            Event::Key(key, _) => open.menu.key(key),
            Event::Char(c, m) if !m.ctrl => open.menu.char(c),
            Event::Char(..) | Event::Nav { .. } => Outcome::Close,
            Event::Resize { .. } | Event::DpiChanged(_) | Event::FileDropped(_) | Event::Close => {
                self.comment_menu = None;
                return false;
            }
            Event::Wake => return false,
        };
        match outcome {
            Outcome::Pick(pick) | Outcome::Live(pick) => {
                let authors = self
                    .comment_menu
                    .take()
                    .map(|o| o.authors)
                    .unwrap_or_default();
                self.comment_pick(pick, &authors);
            }
            Outcome::Close => self.comment_menu = None,
            Outcome::Stay => {}
        }
        window.request_redraw();
        true
    }

    /// Ce qu'on a choisi dans une liste du panneau.
    fn comment_pick(&mut self, pick: CommentPick, authors: &[String]) {
        log_line(&format!("panneau : choix {pick:?}"));
        let view = &mut self.panel.comments;
        match pick {
            CommentPick::Sort(sort) => view.sort = sort,
            CommentPick::AllKinds => view.hidden.clear(),
            CommentPick::Kind(k) => {
                if let Some(i) = view.hidden.iter().position(|h| *h == k) {
                    view.hidden.remove(i);
                } else {
                    view.hidden.push(k);
                }
            }
            CommentPick::Author(who) => view.author = who.and_then(|i| authors.get(i).cloned()),
            CommentPick::Status(row, state) => {
                self.set_comment_state(row, StateChange::Review(state));
            }
        }
    }

    /// Dessine la liste déroulée du panneau des commentaires.
    pub(super) fn paint_comment_menu(&mut self, frame: &mut Frame<'_>) {
        let theme = self.theme;
        if let (Some(open), Some(text)) = (&self.comment_menu, self.text.as_mut()) {
            open.menu.paint(frame, text, &mut self.raster, &theme);
        }
    }

    // --- Menu contextuel --------------------------------------------------------

    /// Le menu d'un commentaire, ouvert d'un clic droit dessus.
    pub(super) fn annot_menu(&mut self, x: i32, y: i32) -> Menu<(Command, super::Target)> {
        let now = super::Target::Current;
        let (status, marked) = self
            .annot_sel
            .as_ref()
            .map(|s| (s.page, s.index))
            .and_then(|(p, i)| self.comment_row(p, i))
            .and_then(|r| self.loaded.as_ref()?.comments.get(r))
            .map_or((None, false), |c| (c.status, c.marked));
        let annotate = self.rights().annotate;
        let shortcut = |c: Command| crate::ui::palette::describe(c).map_or("", |(_, keys)| keys);
        let mut menu = Menu::new(x, y)
            .item(
                Some(Icon::Reply),
                tr("Répondre"),
                shortcut(Command::ReplyComment),
                (Command::ReplyComment, now),
                annotate,
            )
            .item(
                Some(Icon::EditText),
                tr("Modifier le texte…"),
                "",
                (Command::EditCommentText, now),
                annotate,
            )
            .separator();
        for (state, command) in [
            (ReviewState::Accepted, Command::CommentAccepted),
            (ReviewState::Rejected, Command::CommentRejected),
            (ReviewState::Cancelled, Command::CommentCancelled),
            (ReviewState::Completed, Command::CommentCompleted),
            (ReviewState::None, Command::CommentNoStatus),
        ] {
            let on = match state {
                ReviewState::None => status.is_none(),
                s => status == Some(s.label()),
            };
            let label = if state == ReviewState::None {
                tr(state.label()).to_string()
            } else {
                trf("Statut : {}", &[tr(state.label())])
            };
            menu = menu.item(
                on.then_some(Icon::Check),
                &label,
                "",
                (command, now),
                annotate,
            );
        }
        menu.item(
            marked.then_some(Icon::Check),
            tr("Coché"),
            "",
            (Command::ToggleCommentMark, now),
            annotate,
        )
        .separator()
        .item(
            Some(Icon::Trash),
            tr("Supprimer le commentaire"),
            shortcut(Command::DeleteComment),
            (Command::DeleteComment, now),
            annotate,
        )
    }
}
