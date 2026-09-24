//! Remplir un formulaire **sur place**, comme dans Acrobat.
//!
//! Un clic dans un champ de texte y pose le curseur : on tape, on
//! sélectionne, on colle, Entrée valide, Échap annule, Tab passe au champ
//! suivant (dans l'ordre `/Tabs` de la page). Une liste déroulante s'ouvre
//! sous son champ, une liste de choix se clique ligne à ligne, une case se
//! coche d'un clic. Les champs sont surlignés en bleu pâle et les champs
//! obligatoires cernés de rouge ; une barre, à l'ouverture d'un document qui
//! a des champs, le dit et donne « Effacer » et « Aplatir ».
//!
//! # La frappe
//!
//! Le document n'est pas touché pendant qu'on tape : le texte vit dans un
//! [`FieldEditor`], dessiné par-dessus la page à la taille, dans la couleur
//! et avec les marges que l'apparence écrira ([`forms::field_look`]), dans
//! une police de mêmes largeurs (Arial pour Helvetica). À la validation, une
//! seule opération `SetField` part au document et au fil de rendu, et seule
//! la page du champ se rend de nouveau — les autres gardent leur image :
//! passer d'un champ à l'autre au clavier ne fait jamais blanchir la vue.
//!
//! Ce qui est tapé et pas encore validé est porté au document avant tout ce
//! qui le lit ou le quitte : enregistrer, annuler, changer d'onglet, fermer,
//! imprimer, exporter, changer d'outil ([`Viewer::flush_typing`]).
//!
//! Limite : sur une page tournée (`/Rotate`) ou un widget tourné (`/MK /R`),
//! la saisie s'écrit à l'horizontale dans le rectangle du champ à l'écran ;
//! l'apparence écrite, elle, suit la rotation.

// Coordonnées d'écran entières et mesures de texte fractionnaires ; `x`,
// `y`, `w`, `h` sont les noms que tout le monde attend pour un rectangle.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use acrux_core::{Point, Rect};
use acrux_features::forms::{
    self, Field, FieldLook, FieldType, FieldValue, FontFamily, LINE_SPACING, TEXT_ASCENT,
    TEXT_DESCENT,
};
use acrux_render::render_page_rotated;

use super::dialogs::Then;
use super::{fill_rect_blend, log_line, EditOp, Region, Viewer};
use crate::platform::{Cursor, Event, Frame, Key, Modifiers, MouseButton, WindowHandle};
use crate::render_worker::Right;
use crate::ui::fieldedit::{line_of, FieldEditor, FieldKey, Line};
use crate::ui::formbar::{FormBar, FormBarAction};
use crate::ui::icons::{self, Icon};
use crate::ui::lang::tr;
use crate::ui::menu::{Menu, Outcome};
use crate::ui::text::TextRenderer;
use crate::ui::toolbar::Toolbar;

/// Demi-période du clignotement du curseur, comme sous Windows.
const BLINK_MS: u64 = 530;

/// Bleu pâle du surlignage des champs, celui d'Acrobat. Il est **multiplié**
/// avec la page : le fond blanc d'un champ prend exactement cette teinte, et
/// le texte noir reste noir.
pub(super) const FIELD_HIGHLIGHT: (u8, u8, u8) = (204, 215, 255);

/// Gris du bouton de la flèche d'une liste déroulante.
const ARROW_BACK: (u8, u8, u8) = (226, 228, 232);
/// Encre de la flèche.
const ARROW_INK: (u8, u8, u8) = (70, 72, 76);

/// Un champ en cours de saisie.
pub(super) struct FieldEdit {
    /// Nom qualifié du champ : son identité, qui survit à une relecture de
    /// l'inventaire.
    pub name: String,
    /// Rang du champ dans l'inventaire du document.
    pub fi: usize,
    /// Rang du widget dans le champ.
    pub wi: usize,
    /// Page du widget.
    pub page: usize,
    /// Rectangle du widget, en coordonnées de page.
    pub rect: Rect,
    /// Sorte de champ : texte, ou liste déroulante à saisie libre.
    pub kind: FieldType,
    /// Le texte en cours.
    pub editor: FieldEditor,
    /// Le texte à l'ouverture : rien n'est écrit s'il n'a pas changé.
    pub original: String,
    /// L'aspect de l'apparence, que la saisie reproduit.
    pub look: FieldLook,
    /// Un glisser sélectionne.
    pub selecting: bool,
    /// Départ du clignotement : remis à zéro à chaque frappe, pour que le
    /// curseur reste visible pendant qu'on tape.
    pub blink: Instant,
    /// Tient le fil du clignotement ; le passer à faux l'arrête.
    ticking: Arc<AtomicBool>,
}

impl Drop for FieldEdit {
    fn drop(&mut self) {
        self.ticking.store(false, Ordering::Relaxed);
    }
}

/// La liste d'une liste déroulante, ouverte sous son champ.
pub(super) struct FieldMenu {
    /// Les options, dans l'ordre de `/Opt`, chacune rendant son rang.
    pub menu: Menu<usize>,
    /// Nom qualifié du champ.
    pub name: String,
}

/// Géométrie de la saisie à l'écran, en pixels de la vue.
#[derive(Debug, Clone, Copy)]
struct EditBox {
    /// Coin haut gauche du widget.
    x: f32,
    y: f32,
    /// Taille du widget.
    w: f32,
    h: f32,
    /// Épaisseur de la bordure.
    border: f32,
    /// Marge du texte.
    pad: f32,
    /// Corps du texte.
    size: f32,
    /// Ligne de base de la première ligne, depuis le haut du widget.
    first_baseline: f32,
    /// Hauteur d'une ligne.
    line_h: f32,
    /// Largeur donnée à la mise en lignes : celle du texte, ou tout le
    /// widget pour un peigne.
    layout_w: f32,
    /// Hauteur de la zone de texte.
    inner_h: f32,
}

/// Sortes de champ qu'on remplit.
fn fillable(kind: FieldType) -> bool {
    matches!(
        kind,
        FieldType::Text
            | FieldType::CheckBox
            | FieldType::Radio
            | FieldType::ComboBox
            | FieldType::ListBox
    )
}

/// Vrai pour un champ où l'on tape : un texte, ou une liste déroulante à
/// saisie libre.
fn typed(field: &Field) -> bool {
    field.kind == FieldType::Text || (field.kind == FieldType::ComboBox && field.flags.edit)
}

/// Octets d'une couleur (0 à 1).
fn bytes(c: [f64; 3]) -> (u8, u8, u8) {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    (b(c[0]), b(c[1]), b(c[2]))
}

/// Multiplie un rectangle du tampon par une couleur : le blanc la prend, le
/// noir reste noir. C'est ce qui surligne un champ sans voiler son texte.
fn multiply_rect(frame: &mut Frame<'_>, x: i32, y: i32, w: i32, h: i32, color: (u8, u8, u8)) {
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w).min(frame.width as i32);
    let y1 = (y + h).min(frame.height as i32);
    for yy in y0..y1 {
        for xx in x0..x1 {
            let i = frame.index(xx as usize, yy as usize);
            let d = &mut frame.pixels[i..i + 4];
            d[0] = ((u32::from(d[0]) * u32::from(color.2)) / 255) as u8;
            d[1] = ((u32::from(d[1]) * u32::from(color.1)) / 255) as u8;
            d[2] = ((u32::from(d[2]) * u32::from(color.0)) / 255) as u8;
        }
    }
}

/// Contour d'un rectangle `(x, y, w, h)`, d'épaisseur `t`.
fn outline(frame: &mut Frame<'_>, (x, y, w, h): (i32, i32, i32, i32), t: i32, c: (u8, u8, u8)) {
    frame.fill_rect(x, y, w, t, c.0, c.1, c.2);
    frame.fill_rect(x, y + h - t, w, t, c.0, c.1, c.2);
    frame.fill_rect(x, y, t, h, c.0, c.1, c.2);
    frame.fill_rect(x + w - t, y, t, h, c.0, c.1, c.2);
}

/// La police de la saisie : celle du champ si elle a pu être chargée, sinon
/// celle de l'interface.
fn font<'a>(
    field_font: &'a mut Option<(FontFamily, TextRenderer)>,
    text: &'a mut Option<TextRenderer>,
) -> Option<&'a mut TextRenderer> {
    match field_font {
        Some((_, f)) => Some(f),
        None => text.as_mut(),
    }
}

/// Décalage d'une ligne selon l'alignement `/Q`, dans la largeur `width`.
fn aligned(quadding: i64, line_w: f32, width: f32) -> f32 {
    match quadding {
        1 => ((width - line_w) / 2.0).max(0.0),
        2 => (width - line_w).max(0.0),
        _ => 0.0,
    }
}

impl Viewer {
    // -----------------------------------------------------------------
    // Où sont les champs.
    // -----------------------------------------------------------------

    /// Widget sous un point de la vue : rang du champ, du widget, et le
    /// point en coordonnées de page. Un widget masqué ne se clique pas.
    pub(super) fn widget_at(&self, x: i32, y: i32) -> Option<(usize, usize, Point)> {
        let (page, pt) = self.page_at(x, y)?;
        let l = self.loaded.as_ref()?;
        for (fi, f) in l.fields.iter().enumerate() {
            for (wi, w) in f.widgets.iter().enumerate() {
                if !w.hidden && w.page == Some(page) && w.rect.contains(pt) {
                    return Some((fi, wi, pt));
                }
            }
        }
        None
    }

    /// Rectangle d'un widget dans la vue.
    fn widget_view_rect(&self, fi: usize, wi: usize) -> Option<(f64, f64, f64, f64)> {
        let l = self.loaded.as_ref()?;
        let w = l.fields.get(fi)?.widgets.get(wi)?;
        let r = self.page_rect_to_view(w.page?, w.rect)?;
        Some((r.x, r.y, r.w, r.h))
    }

    /// Vrai si le point `(x, y)` de la vue est sur la flèche d'une liste
    /// déroulante : le carré de droite, de côté la hauteur du champ.
    fn on_combo_arrow(&self, fi: usize, wi: usize, x: i32) -> bool {
        self.widget_view_rect(fi, wi)
            .is_some_and(|(rx, _, rw, rh)| f64::from(x) >= rx + rw - rh.min(rw / 3.0))
    }

    /// Pointeur au-dessus d'un widget : la barre de texte là où l'on tape, la
    /// main ailleurs. `None` hors des widgets.
    pub(super) fn widget_cursor(&self, x: i32, y: i32) -> Option<Cursor> {
        let (fi, wi, _) = self.widget_at(x, y)?;
        let field = self.loaded.as_ref()?.fields.get(fi)?;
        let typing = !field.flags.read_only
            && typed(field)
            && !(field.kind == FieldType::ComboBox && self.on_combo_arrow(fi, wi, x));
        Some(if typing { Cursor::IBeam } else { Cursor::Hand })
    }

    // -----------------------------------------------------------------
    // La barre de formulaire.
    // -----------------------------------------------------------------

    /// Hauteur de la barre de formulaire : nulle sans champ à remplir, une
    /// fois fermée, en plein écran, en lecture et en « Modifier le PDF »
    /// (trois barres empilées ne laisseraient plus voir la page).
    pub(super) fn form_bar_height(&self) -> u32 {
        let shown = self
            .loaded
            .as_ref()
            .is_some_and(|l| l.fillable && !l.form_bar_closed)
            && !self.fullscreen
            && !self.reading
            && !self.edit_on()
            && !self.showing_home();
        if shown {
            FormBar::height(self.dpi_scale as f32).max(0) as u32
        } else {
            0
        }
    }

    /// Haut de la barre de formulaire, sous les autres barres.
    fn form_bar_top(&self) -> i32 {
        Toolbar::height(&self.theme, self.dpi_scale as f32)
            + self.tabs_height() as i32
            + self.edit_bar_height() as i32
            + self.mode_bar_height() as i32
    }

    /// Vrai si l'ordonnée `y` de la fenêtre tombe dans la barre.
    pub(super) fn in_form_bar(&self, y: i32) -> bool {
        let h = self.form_bar_height() as i32;
        let top = self.form_bar_top();
        h > 0 && y >= top && y < top + h
    }

    /// Survol de la barre ; vrai si l'aspect a changé.
    pub(super) fn form_bar_hover(&mut self, x: i32, y: i32) -> bool {
        if self.in_form_bar(y) {
            self.form_bar.mouse_move(x, y)
        } else {
            self.form_bar.leave()
        }
    }

    /// Clic dans la barre.
    pub(super) fn form_bar_click(&mut self, x: i32, y: i32, window: &mut dyn WindowHandle) {
        if let Some(action) = self.form_bar.mouse_down(x, y) {
            log_line(&format!("barre de formulaire : {action:?}"));
            match action {
                FormBarAction::ToggleHighlight => self.toggle_field_highlight(),
                FormBarAction::Reset => self.ask_reset_form(),
                FormBarAction::Flatten => self.ask_flatten_form(),
                FormBarAction::Close => {
                    if let Some(l) = &mut self.loaded {
                        l.form_bar_closed = true;
                    }
                    self.form_bar.leave();
                    self.clamp_scroll();
                }
            }
            window.request_redraw();
        }
    }

    /// Dessine la barre, si elle est affichée.
    pub(super) fn paint_form_bar(&mut self, frame: &mut Frame<'_>) {
        if self.form_bar_height() == 0 {
            return;
        }
        let y = self.form_bar_top();
        let (theme, dpi, on) = (
            self.theme,
            self.dpi_scale as f32,
            self.prefs.highlight_fields,
        );
        if let Some(text) = self.text.as_mut() {
            self.form_bar
                .paint(frame, text, &mut self.raster, &theme, dpi, y, on);
        }
    }

    /// Surligne les champs, ou cesse de les surligner. Le choix est retenu.
    pub(super) fn toggle_field_highlight(&mut self) {
        self.prefs.highlight_fields = !self.prefs.highlight_fields;
        self.prefs.save();
        self.set_notice(
            tr(if self.prefs.highlight_fields {
                "champs surlignés"
            } else {
                "champs non surlignés"
            })
            .into(),
        );
    }

    /// Faux, avec un mot dans la barre d'état, quand le document n'a aucun
    /// champ de formulaire.
    fn require_form(&mut self) -> bool {
        if self.loaded.as_ref().is_some_and(|l| !l.fields.is_empty()) {
            return true;
        }
        self.set_notice(tr("ce document n'a pas de champ de formulaire").into());
        false
    }

    /// « Effacer le formulaire », après confirmation.
    pub(super) fn ask_reset_form(&mut self) {
        self.commit_field();
        if !self.require_form() || !self.require_right(Right::FillForms) {
            return;
        }
        self.confirm(
            tr("Effacer le formulaire ?"),
            tr("Chaque champ reprend sa valeur par défaut, ou redevient vide. Ctrl+Z annule."),
            tr("Effacer"),
            Then::ResetForm,
        );
    }

    /// « Aplatir le formulaire », après confirmation.
    pub(super) fn ask_flatten_form(&mut self) {
        self.commit_field();
        if !self.require_form() || !self.require_right(Right::Modify) {
            return;
        }
        self.confirm(
            tr("Aplatir le formulaire ?"),
            tr("Les champs deviennent du contenu fixe des pages : ils ne pourront plus être remplis. Ctrl+Z annule tant que le document n'est pas enregistré."),
            tr("Aplatir"),
            Then::FlattenForm,
        );
    }

    // -----------------------------------------------------------------
    // Écrire une valeur.
    // -----------------------------------------------------------------

    /// Écrit une valeur de champ, légèrement : le document et le fil de
    /// rendu la reçoivent, l'historique aussi, puis **seules les pages du
    /// champ** sont oubliées et rendues de nouveau, tout de suite si elles
    /// sont à l'écran. Les autres gardent leur image, la recherche et la
    /// sélection restent : cocher une case ne fait plus blanchir la vue.
    ///
    /// Rend faux si rien n'a été écrit (droit refusé, valeur refusée), ce
    /// qui est déjà dit.
    fn apply_field_edit(&mut self, op: EditOp) -> bool {
        if self.loaded.is_none() || !self.require_right(op.required_right()) {
            return false;
        }
        let EditOp::SetField { name, .. } = &op else {
            return false;
        };
        let name = name.clone();
        let scale = self.scale();
        let key_scale = (scale * 1000.0).round() as u32;
        let layout = self.layout();
        let (top, bottom) = (self.scroll_y, self.scroll_y + f64::from(self.view_height()));
        let failure = {
            let Some(l) = &mut self.loaded else {
                return false;
            };
            op.apply(&l.doc).err()
        };
        if let Some(e) = failure {
            self.alert(tr("Remplissage impossible"), &format!("{e}"));
            return false;
        }
        let Some(l) = &mut self.loaded else {
            return false;
        };
        let pages_of = |fields: &[Field]| -> Vec<usize> {
            fields
                .iter()
                .filter(|f| f.name == name)
                .flat_map(|f| f.widgets.iter().filter_map(|w| w.page))
                .collect()
        };
        let mut touched = pages_of(&l.fields);
        if let Some(w) = &mut l.worker {
            w.edit(op.clone());
        }
        l.history.push(op);
        l.redo.clear();
        l.modified = true;
        l.set_fields(forms::list_fields(&l.doc).unwrap_or_default());
        touched.extend(pages_of(&l.fields));
        touched.sort_unstable();
        touched.dedup();
        for p in &touched {
            l.texts.remove(p);
            l.boxes.remove(p);
        }
        l.cache.retain(|(p, _), _| !touched.contains(p));
        let options = l.screen_options(Duration::from_secs(5));
        for &p in &touched {
            let shown = layout.get(p).is_some_and(|b| {
                b.visible && f64::from(b.y + b.h as i32) >= top && f64::from(b.y) <= bottom
            });
            if let (true, Some(page)) = (shown, l.pages.get(p)) {
                let bitmap =
                    render_page_rotated(&l.doc, page, scale, l.view_rotation, &options).bitmap;
                l.cache.insert((p, key_scale), bitmap);
            }
        }
        self.title_dirty = true;
        true
    }

    /// Donne une valeur à un champ, et le note au journal — le nom seul :
    /// une valeur peut être un mot de passe.
    fn set_field(&mut self, name: &str, value: FieldValue) -> bool {
        let done = self.apply_field_edit(EditOp::SetField {
            name: name.to_string(),
            value,
        });
        if done {
            log_line(&format!("champ : « {name} » modifié"));
        }
        done
    }

    // -----------------------------------------------------------------
    // Clic sur un champ.
    // -----------------------------------------------------------------

    /// Clic sur le widget `wi` du champ `fi`, au point `(x, y)` de la vue
    /// (`pt` en coordonnées de page) : une case se coche, un bouton radio se
    /// choisit, une liste prend la ligne cliquée, une liste déroulante
    /// s'ouvre, un champ de texte reçoit le curseur.
    #[allow(clippy::too_many_arguments)] // le clic et tout ce qui l'accompagne
    pub(super) fn click_widget(
        &mut self,
        fi: usize,
        wi: usize,
        (x, y): (i32, i32),
        pt: Point,
        clicks: u8,
        m: Modifiers,
        window: &mut dyn WindowHandle,
    ) {
        self.selection = None;
        self.region = Region::Document;
        self.leave_region(Region::Document);
        let series = self.click_series(x, y, clicks);
        // Dans le champ en cours de saisie : le curseur, ou une sélection.
        if self
            .field_edit
            .as_ref()
            .is_some_and(|e| e.fi == fi && e.wi == wi)
        {
            self.place_caret(x, y, series, m.shift);
            window.request_redraw();
            return;
        }
        self.commit_field();
        let Some(field) = self.loaded.as_ref().and_then(|l| l.fields.get(fi)).cloned() else {
            return;
        };
        self.focus_field = Some((fi, wi));
        if field.flags.read_only {
            self.set_notice(tr("ce champ est en lecture seule").into());
            return;
        }
        match field.kind {
            // F9 branchera ici la signature d'un champ de signature.
            FieldType::Signature | FieldType::Button | FieldType::Unknown => return,
            _ => {}
        }
        // Le droit se vérifie avant de laisser taper : une saisie refusée à
        // la validation serait du travail perdu.
        if !self.require_right(Right::FillForms) {
            return;
        }
        match field.kind {
            FieldType::CheckBox => {
                let checked = match &field.value {
                    Some(FieldValue::State(s)) => s != "Off",
                    Some(FieldValue::Bool(b)) => *b,
                    _ => false,
                };
                self.set_field(&field.name, FieldValue::Bool(!checked));
            }
            FieldType::Radio => {
                let Some(w) = field.widgets.get(wi) else {
                    return;
                };
                let Some(on) = w.on_state.clone() else { return };
                let selected = w.state.as_deref() == Some(on.as_str());
                // Recliquer le bouton choisi le décoche, sauf si le groupe
                // exige un choix (« NoToggleToOff », §12.7.5.2.4).
                if selected && field.flags.no_toggle_to_off {
                    return;
                }
                let value = if selected { "Off".to_string() } else { on };
                self.set_field(&field.name, FieldValue::State(value));
            }
            FieldType::ComboBox => {
                if !field.flags.edit || self.on_combo_arrow(fi, wi, x) {
                    self.open_field_menu(fi, wi, window);
                } else if self.start_field_edit(fi, wi, false, window) {
                    self.place_caret(x, y, series, m.shift);
                }
            }
            FieldType::ListBox => self.click_list(&field, wi, pt, m),
            FieldType::Text => {
                if self.start_field_edit(fi, wi, false, window) {
                    self.place_caret(x, y, series, m.shift);
                }
            }
            FieldType::Button | FieldType::Signature | FieldType::Unknown => {}
        }
        window.request_redraw();
    }

    /// Clic dans une liste de choix : la ligne cliquée est choisie. Avec une
    /// sélection multiple, Ctrl+clic ajoute ou retire une ligne, Maj+clic
    /// prend toutes les lignes depuis le dernier clic.
    fn click_list(&mut self, field: &Field, wi: usize, pt: Point, m: Modifiers) {
        let Some(l) = &self.loaded else { return };
        let Some(row) = forms::list_row_at(&l.doc, field, wi, pt) else {
            return;
        };
        let mut chosen: Vec<usize> = match &field.value {
            Some(FieldValue::Choice(v)) => v
                .iter()
                .filter_map(|e| field.options.iter().position(|o| o.export == *e))
                .collect(),
            _ => Vec::new(),
        };
        let multi = field.flags.multi_select;
        let anchor = self
            .list_anchor
            .as_ref()
            .filter(|(name, _)| *name == field.name)
            .map(|(_, i)| *i);
        if multi && m.ctrl {
            if let Some(at) = chosen.iter().position(|&i| i == row) {
                chosen.remove(at);
            } else {
                chosen.push(row);
            }
            chosen.sort_unstable();
        } else if multi && m.shift {
            let from = anchor.unwrap_or(row);
            chosen = (from.min(row)..=from.max(row)).collect();
        } else {
            chosen = vec![row];
        }
        if !m.shift {
            self.list_anchor = Some((field.name.clone(), row));
        }
        let exports = chosen
            .iter()
            .filter_map(|&i| field.options.get(i).map(|o| o.export.clone()))
            .collect();
        self.set_field(&field.name, FieldValue::Choice(exports));
    }

    /// Rang du clic dans une série (simple, double, triple), comme pour la
    /// sélection du texte de la page.
    pub(super) fn click_series(&mut self, x: i32, y: i32, clicks: u8) -> u8 {
        let now = Instant::now();
        let series = match self.last_click {
            Some((t, lx, ly, n))
                if now.duration_since(t) < super::MULTI_CLICK
                    && (x - lx).abs() < 4
                    && (y - ly).abs() < 4 =>
            {
                if clicks >= 2 {
                    2
                } else {
                    n + 1
                }
            }
            _ => 1,
        };
        self.last_click = Some((now, x, y, series));
        series
    }

    // -----------------------------------------------------------------
    // La saisie.
    // -----------------------------------------------------------------

    /// Ouvre la saisie dans le widget `wi` du champ `fi` ; faux si ce n'est
    /// pas un champ où l'on tape. `select_all` : tout le texte sélectionné,
    /// comme quand on arrive dans un champ par Tab.
    pub(super) fn start_field_edit(
        &mut self,
        fi: usize,
        wi: usize,
        select_all: bool,
        window: &mut dyn WindowHandle,
    ) -> bool {
        self.commit_field();
        let Some(l) = &self.loaded else { return false };
        let Some(field) = l.fields.get(fi) else {
            return false;
        };
        let Some(widget) = field.widgets.get(wi) else {
            return false;
        };
        let Some(page) = widget.page else {
            return false;
        };
        if field.flags.read_only || !typed(field) {
            return false;
        }
        let text = match &field.value {
            Some(FieldValue::Text(t)) => t.clone(),
            // Une liste déroulante montre le libellé de son option, pas sa
            // valeur d'export.
            Some(FieldValue::Choice(v)) => v
                .first()
                .map(|e| {
                    field
                        .options
                        .iter()
                        .find(|o| o.export == *e)
                        .map_or_else(|| e.clone(), |o| o.display.clone())
                })
                .unwrap_or_default(),
            Some(other) => other.to_display(),
            None => String::new(),
        };
        let look = forms::field_look(&l.doc, field, wi);
        let is_text = field.kind == FieldType::Text;
        let mut editor = FieldEditor::new(
            &text,
            if is_text { field.max_len } else { None },
            is_text && field.flags.multiline,
            look.comb_cells,
            is_text && field.flags.password,
        );
        if select_all {
            editor.buffer.select_all();
        }
        let (name, kind, rect) = (field.name.clone(), field.kind, widget.rect);
        let family = look.family;
        self.load_field_font(family);
        let ticking = Arc::new(AtomicBool::new(true));
        let waker = window.waker();
        let flag = Arc::clone(&ticking);
        // Le curseur clignote : un fil réveille la fenêtre à chaque demi-
        // période, comme celui du mode « Modifier le PDF ». C'est lui qui
        // réveille, jamais la boucle d'événements.
        let _ = std::thread::Builder::new()
            .name("curseur de champ".into())
            .spawn(move || {
                while flag.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(BLINK_MS));
                    if flag.load(Ordering::Relaxed) {
                        waker.wake();
                    }
                }
            });
        log_line(&format!("champ : saisie de « {name} »"));
        self.field_edit = Some(FieldEdit {
            name,
            fi,
            wi,
            page,
            rect,
            kind,
            original: text,
            editor,
            look,
            selecting: false,
            blink: Instant::now(),
            ticking,
        });
        self.focus_field = Some((fi, wi));
        self.region = Region::Document;
        window.request_redraw();
        true
    }

    /// Charge une police de mêmes largeurs que celle du champ, si ce n'est
    /// déjà fait ; sans elle, la saisie se dessine dans celle de
    /// l'interface.
    fn load_field_font(&mut self, family: FontFamily) {
        if self
            .field_font
            .as_ref()
            .is_some_and(|(loaded, _)| *loaded == family)
        {
            return;
        }
        let names: &[&str] = match family {
            FontFamily::Sans => &["arial.ttf", "LiberationSans-Regular.ttf", "DejaVuSans.ttf"],
            FontFamily::Serif => &[
                "times.ttf",
                "LiberationSerif-Regular.ttf",
                "DejaVuSerif.ttf",
            ],
            FontFamily::Mono => &[
                "cour.ttf",
                "LiberationMono-Regular.ttf",
                "DejaVuSansMono.ttf",
            ],
        };
        self.field_font = TextRenderer::load(names).map(|f| (family, f));
    }

    /// Porte au document ce qui a été tapé, s'il a changé, et referme la
    /// saisie. Le champ garde le focus.
    pub(super) fn commit_field(&mut self) {
        let Some(edit) = self.field_edit.take() else {
            return;
        };
        let text = edit.editor.buffer.text.clone();
        if text == edit.original {
            log_line(&format!("champ : « {} » quitté sans changement", edit.name));
            return;
        }
        let count = text.chars().count();
        // Une liste déroulante à saisie libre reçoit le texte tel quel : le
        // moteur y reconnaît une option par sa valeur ou son libellé.
        let done = self.apply_field_edit(EditOp::SetField {
            name: edit.name.clone(),
            value: FieldValue::Text(text),
        });
        if done {
            // Le nom et la longueur seulement : un mot de passe ne va pas
            // au journal.
            log_line(&format!(
                "champ : « {} » validé ({count} caractère(s))",
                edit.name
            ));
        }
    }

    /// Referme la saisie sans rien écrire (Échap).
    pub(super) fn cancel_field(&mut self) {
        if let Some(edit) = self.field_edit.take() {
            log_line(&format!("champ : saisie de « {} » annulée", edit.name));
        }
    }

    /// Porte au document **tout** ce qui est tapé et pas encore écrit : le
    /// bloc de « Modifier le PDF », puis le champ de formulaire. À appeler
    /// avant ce qui lit le document ou le quitte.
    pub(super) fn flush_typing(&mut self) {
        self.close_active();
        self.commit_field();
        // La forme, le dessin ou la zone de texte en cours aussi.
        self.commit_draft();
    }

    /// Vrai quand un champ reçoit la frappe : aucune carte, palette ou
    /// fenêtre de capture ne passe devant lui.
    pub(super) fn field_typing(&self) -> bool {
        self.field_edit.is_some()
            && self.prompt.is_none()
            && self.palette.is_none()
            && self.capture.is_none()
    }

    /// Géométrie de la saisie à l'écran.
    fn edit_box(&self) -> Option<EditBox> {
        let edit = self.field_edit.as_ref()?;
        let view = self.page_rect_to_view(edit.page, edit.rect)?;
        let look = &edit.look;
        // Pixels par point.
        let k = self.scale();
        let (w, h) = (view.w as f32, view.h as f32);
        let pad = (look.padding * k) as f32;
        let size_pt = look.text_size(&edit.editor.buffer.text);
        let size = (((size_pt * k) as f32) * 4.0).round() / 4.0;
        let size = size.max(1.0);
        let line_h = size * LINE_SPACING as f32;
        let first_baseline = if look.multiline && look.comb_cells.is_none() {
            pad + size * TEXT_ASCENT as f32
        } else {
            h - (look.baseline(size_pt) * k) as f32
        };
        let layout_w = if look.comb_cells.is_some() {
            w
        } else {
            (w - 2.0 * pad).max(1.0)
        };
        Some(EditBox {
            x: view.x as f32,
            y: view.y as f32,
            w,
            h,
            border: (look.border_width * k) as f32,
            pad,
            size,
            first_baseline,
            line_h,
            layout_w,
            inner_h: (h - 2.0 * pad).max(1.0),
        })
    }

    /// Décalage horizontal d'une ligne dans la zone de texte : l'alignement
    /// `/Q`, ou le défilement d'une ligne plus large que le champ.
    fn line_offset(edit: &FieldEdit, b: &EditBox, line: &Line) -> f32 {
        if edit.editor.multiline {
            return aligned(edit.look.quadding, line.width, b.layout_w);
        }
        if line.width > b.layout_w {
            -edit.editor.scroll
        } else {
            aligned(edit.look.quadding, line.width, b.layout_w)
        }
    }

    /// Pose le curseur au point `(x, y)` de la vue : un clic simple le pose
    /// (Maj étend la sélection), un double sélectionne le mot, un triple
    /// tout le texte.
    fn place_caret(&mut self, x: i32, y: i32, series: u8, shift: bool) {
        let Some(b) = self.edit_box() else { return };
        let Some(edit) = self.field_edit.as_mut() else {
            return;
        };
        let Some(f) = font(&mut self.field_font, &mut self.text) else {
            return;
        };
        let size = b.size;
        let mut measure = |s: &str| f.measure(size, s);
        let index = if edit.editor.comb.is_some() {
            edit.editor
                .index_at(&mut measure, b.layout_w, x as f32 - b.x, 0)
        } else {
            let lines = edit.editor.layout(&mut measure, b.layout_w);
            let scroll_y = if edit.editor.multiline {
                edit.editor.scroll
            } else {
                0.0
            };
            let top = b.y + b.first_baseline - size * TEXT_ASCENT as f32;
            let row = ((y as f32 - top + scroll_y) / b.line_h).floor().max(0.0) as usize;
            let row = row.min(lines.len().saturating_sub(1));
            let offset = lines
                .get(row)
                .map_or(0.0, |line| Self::line_offset(edit, &b, line));
            let lx = x as f32 - (b.x + b.pad) - offset;
            edit.editor.index_at(&mut measure, b.layout_w, lx, row)
        };
        match series {
            3.. => edit.editor.buffer.select_all(),
            2 => edit.editor.buffer.select_word(index),
            _ => edit.editor.buffer.move_to(index, shift),
        }
        edit.selecting = series < 2;
        edit.blink = Instant::now();
    }

    /// Vrai pendant un glisser qui sélectionne dans le champ.
    pub(super) fn field_selecting(&self) -> bool {
        self.field_edit.as_ref().is_some_and(|e| e.selecting)
    }

    /// Glisser dans le champ : la sélection suit le pointeur.
    pub(super) fn field_drag(&mut self, x: i32, y: i32) {
        self.place_caret(x, y, 1, true);
        if let Some(e) = &mut self.field_edit {
            e.selecting = true;
        }
    }

    /// Bouton relâché : fin d'une sélection au glisser.
    pub(super) fn field_mouse_up(&mut self) {
        if let Some(e) = &mut self.field_edit {
            e.selecting = false;
        }
    }

    /// Vrai si le point `(x, y)` de la **fenêtre** tombe dans le champ en
    /// cours de saisie : un clic ailleurs le valide d'abord.
    pub(super) fn in_field_edit(&self, x: i32, y: i32) -> bool {
        let Some(b) = self.edit_box() else {
            return false;
        };
        let (vx, vy) = (
            (x - self.view_left() as i32) as f32,
            (y - self.view_top() as i32) as f32,
        );
        vx >= b.x && vx < b.x + b.w && vy >= b.y && vy < b.y + b.h
    }

    /// Une touche pendant la saisie ; vrai si elle a été prise.
    pub(super) fn field_key(
        &mut self,
        key: Key,
        m: Modifiers,
        window: &mut dyn WindowHandle,
    ) -> bool {
        let Some((fi, wi, kind)) = self.field_edit.as_ref().map(|e| (e.fi, e.wi, e.kind)) else {
            return false;
        };
        if m.alt {
            // Alt+↓ déroule la liste d'une liste déroulante à saisie libre.
            if key == Key::Down && kind == FieldType::ComboBox {
                self.commit_field();
                self.open_field_menu(fi, wi, window);
                return true;
            }
            // Alt+↑ et Alt+↓ ne font pas défiler la page sous le curseur ;
            // Alt+← et Alt+→ (vue précédente, suivante) suivent leur cours.
            return matches!(key, Key::Up | Key::Down);
        }
        // Ctrl avec une autre touche que celles de la saisie (Ctrl+Tab,
        // Ctrl+Page suivante…) passe son chemin : ce qu'elle fait valide le
        // champ s'il le faut (changer d'onglet, par exemple). Ctrl lui-même,
        // enfoncé avant la lettre d'un raccourci, ne doit rien valider.
        if m.ctrl
            && !matches!(
                key,
                Key::Left
                    | Key::Right
                    | Key::Home
                    | Key::End
                    | Key::Backspace
                    | Key::Delete
                    | Key::Enter
            )
        {
            return false;
        }
        // La barre d'espace écrit son caractère, qui arrive à part.
        if key == Key::Space {
            return true;
        }
        // Le champ n'est plus à l'écran (une autre page, en mode page par
        // page) : on le quitte comme d'un clic ailleurs, et la touche suit
        // son cours.
        let Some(b) = self.edit_box() else {
            self.commit_field();
            return false;
        };
        let outcome = {
            let Some(edit) = self.field_edit.as_mut() else {
                return false;
            };
            let Some(f) = font(&mut self.field_font, &mut self.text) else {
                return false;
            };
            let size = b.size;
            let mut measure = |s: &str| f.measure(size, s);
            edit.blink = Instant::now();
            edit.editor.key(key, m, &mut measure, b.layout_w)
        };
        window.request_redraw();
        match outcome {
            FieldKey::Changed | FieldKey::Moved => true,
            FieldKey::Commit => {
                self.commit_field();
                true
            }
            FieldKey::Cancel => {
                self.cancel_field();
                true
            }
            FieldKey::Next(forward) => {
                self.commit_field();
                self.focus_next_field(forward);
                self.enter_focused_field(window);
                true
            }
            // Les flèches et l'effacement restent dans le champ, même sans
            // effet : la page ne doit pas défiler sous le curseur.
            FieldKey::None => matches!(
                key,
                Key::Up
                    | Key::Down
                    | Key::Left
                    | Key::Right
                    | Key::Home
                    | Key::End
                    | Key::Backspace
                    | Key::Delete
                    | Key::Enter
            ),
        }
    }

    /// Un caractère pendant la saisie ; vrai s'il a été pris. Ctrl+A, C, X,
    /// V agissent dans le champ ; Ctrl+Z et Ctrl+Y défont et refont d'abord
    /// dans le champ, puis dans le document ; les autres raccourcis valident
    /// la saisie et suivent leur cours.
    pub(super) fn field_char(
        &mut self,
        c: char,
        m: Modifiers,
        window: &mut dyn WindowHandle,
    ) -> bool {
        if self.field_edit.is_none() {
            return false;
        }
        window.request_redraw();
        if m.ctrl {
            let copying = matches!(c, 'c' | 'C' | '\u{3}' | 'x' | 'X' | '\u{18}');
            if copying && !self.rights().copy {
                self.set_notice(tr("copie interdite par les permissions du document").into());
                return true;
            }
            let Some(edit) = self.field_edit.as_mut() else {
                return false;
            };
            edit.blink = Instant::now();
            match c {
                'a' | 'A' | '\u{1}' => edit.editor.buffer.select_all(),
                'c' | 'C' | '\u{3}' => {
                    if let Some(text) = edit.editor.copy_text() {
                        window.set_clipboard_text(&text);
                    }
                }
                'x' | 'X' | '\u{18}' => {
                    if let Some(text) = edit.editor.cut() {
                        window.set_clipboard_text(&text);
                    }
                }
                'v' | 'V' | '\u{16}' => {
                    if let Some(text) = window.clipboard_text() {
                        edit.editor.insert(&text);
                    }
                }
                'z' | 'Z' | '\u{1a}' => {
                    let done = if m.shift {
                        edit.editor.redo_step()
                    } else {
                        edit.editor.undo_step()
                    };
                    if !done {
                        self.commit_field();
                        return false;
                    }
                }
                'y' | 'Y' | '\u{19}' => {
                    if !edit.editor.redo_step() {
                        self.commit_field();
                        return false;
                    }
                }
                _ => {
                    self.commit_field();
                    return false;
                }
            }
            return true;
        }
        if c.is_control() {
            return true;
        }
        if let Some(edit) = self.field_edit.as_mut() {
            edit.blink = Instant::now();
            edit.editor.insert(&c.to_string());
        }
        true
    }

    /// Ce que la saisie a à défaire et à refaire, dans cet ordre.
    pub(super) fn field_history(&self) -> (bool, bool) {
        self.field_edit.as_ref().map_or((false, false), |e| {
            (e.editor.can_undo(), e.editor.can_redo())
        })
    }

    /// Défait une étape de la saisie ; faux s'il n'y en a pas.
    pub(super) fn field_undo_step(&mut self) -> bool {
        let done = self
            .field_edit
            .as_mut()
            .is_some_and(|e| e.editor.undo_step());
        if let (true, Some(e)) = (done, self.field_edit.as_mut()) {
            e.blink = Instant::now();
        }
        done
    }

    /// Refait une étape de la saisie ; faux s'il n'y en a pas.
    pub(super) fn field_redo_step(&mut self) -> bool {
        let done = self
            .field_edit
            .as_mut()
            .is_some_and(|e| e.editor.redo_step());
        if let (true, Some(e)) = (done, self.field_edit.as_mut()) {
            e.blink = Instant::now();
        }
        done
    }

    // -----------------------------------------------------------------
    // Le focus et la tabulation.
    // -----------------------------------------------------------------

    /// Arrêts de la tabulation, dans l'ordre (voir [`forms::tab_order`]).
    fn tab_stops(&self) -> Vec<(usize, usize)> {
        self.loaded
            .as_ref()
            .and_then(|l| forms::tab_order(&l.doc, &l.fields).ok())
            .unwrap_or_default()
    }

    /// Donne le focus au champ suivant (`forward`) ou précédent, et le
    /// montre s'il sort de la vue.
    pub(super) fn focus_next_field(&mut self, forward: bool) {
        let order = self.tab_stops();
        if order.is_empty() {
            return;
        }
        let pos = self.focus_field.and_then(|(fi, wi)| {
            order
                .iter()
                .position(|&s| s == (fi, wi))
                .or_else(|| order.iter().position(|&(f, _)| f == fi))
        });
        let next = match (pos, forward) {
            (None, true) => 0,
            (None, false) => order.len() - 1,
            (Some(p), true) => (p + 1) % order.len(),
            (Some(p), false) => (p + order.len() - 1) % order.len(),
        };
        let (fi, wi) = order[next];
        self.focus_field = Some((fi, wi));
        if let Some(name) = self
            .loaded
            .as_ref()
            .and_then(|l| l.fields.get(fi))
            .map(|f| f.name.clone())
        {
            log_line(&format!("champ : focus sur « {name} »"));
        }
        self.reveal_field(fi, wi);
    }

    /// Fait défiler la vue pour que le widget se voie en entier.
    fn reveal_field(&mut self, fi: usize, wi: usize) {
        let target = self.loaded.as_ref().and_then(|l| {
            let w = l.fields.get(fi)?.widgets.get(wi)?;
            Some((w.page?, w.rect))
        });
        let Some((page, rect)) = target else { return };
        if self.view_mode.is_paged() && self.anchor != page {
            self.scroll_to_page(page);
        }
        let Some(r) = self.page_rect_to_view(page, rect) else {
            return;
        };
        let vh = f64::from(self.view_height());
        if r.y < 0.0 || r.y + r.h > vh {
            self.scroll_y = (self.scroll_y + r.y - vh / 3.0).max(0.0);
            self.clamp_scroll();
        }
    }

    /// Entre dans le champ qui a le focus s'il se tape (texte, liste
    /// déroulante à saisie libre), tout son texte sélectionné — ce que fait
    /// Tab dans Acrobat.
    pub(super) fn enter_focused_field(&mut self, window: &mut dyn WindowHandle) {
        let Some((fi, wi)) = self.focus_field else {
            return;
        };
        let typing = self
            .loaded
            .as_ref()
            .and_then(|l| l.fields.get(fi))
            .is_some_and(|f| !f.flags.read_only && typed(f));
        if typing {
            self.start_field_edit(fi, wi, true, window);
        }
    }

    /// Active le champ qui a le focus (Entrée, Espace) : entrer dans un
    /// texte, cocher une case, choisir un bouton radio, dérouler une liste.
    pub(super) fn activate_focused_field(&mut self, window: &mut dyn WindowHandle) {
        let Some((fi, wi)) = self.focus_field else {
            return;
        };
        let Some(field) = self.loaded.as_ref().and_then(|l| l.fields.get(fi)).cloned() else {
            return;
        };
        match field.kind {
            FieldType::Text => self.enter_focused_field(window),
            FieldType::ComboBox => {
                if field.flags.edit {
                    self.enter_focused_field(window);
                } else if !field.flags.read_only && self.require_right(Right::FillForms) {
                    self.open_field_menu(fi, wi, window);
                }
            }
            FieldType::CheckBox | FieldType::Radio => {
                let Some(w) = field.widgets.get(wi) else {
                    return;
                };
                let center = Point::new(
                    f64::midpoint(w.rect.x0, w.rect.x1),
                    f64::midpoint(w.rect.y0, w.rect.y1),
                );
                let at = self
                    .widget_view_rect(fi, wi)
                    .map_or((0, 0), |(x, y, w, h)| {
                        ((x + w / 2.0) as i32, (y + h / 2.0) as i32)
                    });
                self.click_widget(fi, wi, at, center, 1, Modifiers::default(), window);
            }
            FieldType::ListBox | FieldType::Button | FieldType::Signature | FieldType::Unknown => {}
        }
    }

    /// Touche quand un champ a le focus sans être en saisie : les flèches
    /// changent la ligne d'une liste ou le bouton d'un groupe radio, Alt+↓
    /// déroule une liste déroulante. Vrai si elle a été prise.
    pub(super) fn field_nav_key(
        &mut self,
        key: Key,
        m: Modifiers,
        window: &mut dyn WindowHandle,
    ) -> bool {
        let Some((fi, wi)) = self.focus_field else {
            return false;
        };
        let Some(field) = self.loaded.as_ref().and_then(|l| l.fields.get(fi)).cloned() else {
            return false;
        };
        if field.flags.read_only {
            return false;
        }
        let step: i64 = match key {
            Key::Up | Key::Left => -1,
            Key::Down | Key::Right => 1,
            _ => return false,
        };
        match field.kind {
            FieldType::ComboBox if m.alt && key == Key::Down => {
                if self.require_right(Right::FillForms) {
                    self.open_field_menu(fi, wi, window);
                }
                true
            }
            FieldType::ListBox if !m.alt && matches!(key, Key::Up | Key::Down) => {
                let current = match &field.value {
                    Some(FieldValue::Choice(v)) => v
                        .last()
                        .and_then(|e| field.options.iter().position(|o| o.export == *e)),
                    _ => None,
                };
                let last = field.options.len().saturating_sub(1) as i64;
                let next = current.map_or(0, |c| (c as i64 + step).clamp(0, last)) as usize;
                if let Some(o) = field.options.get(next) {
                    if Some(next) != current && self.require_right(Right::FillForms) {
                        self.set_field(&field.name, FieldValue::Choice(vec![o.export.clone()]));
                    }
                }
                window.request_redraw();
                true
            }
            FieldType::Radio if !m.alt => {
                // Le bouton suivant du groupe, dans l'ordre des widgets.
                let count = field.widgets.len();
                if count == 0 {
                    return false;
                }
                let next = ((wi as i64 + step).rem_euclid(count as i64)) as usize;
                let Some(on) = field.widgets.get(next).and_then(|w| w.on_state.clone()) else {
                    return true;
                };
                if self.require_right(Right::FillForms) {
                    self.focus_field = Some((fi, next));
                    self.set_field(&field.name, FieldValue::State(on));
                }
                window.request_redraw();
                true
            }
            _ => false,
        }
    }

    // -----------------------------------------------------------------
    // La liste déroulante.
    // -----------------------------------------------------------------

    /// Déroule la liste d'une liste déroulante sous son champ, ouverte sur
    /// l'option en cours.
    pub(super) fn open_field_menu(&mut self, fi: usize, wi: usize, window: &mut dyn WindowHandle) {
        let Some((x, y, w, h)) = self.widget_view_rect(fi, wi) else {
            return;
        };
        let Some(field) = self.loaded.as_ref().and_then(|l| l.fields.get(fi)).cloned() else {
            return;
        };
        if field.options.is_empty() {
            self.set_notice(tr("cette liste n'a aucune option").into());
            return;
        }
        let (left, top) = (self.view_left() as i32, self.view_top() as i32);
        let anchor = (x as i32 + left, y as i32 + top, w as i32, h as i32);
        let current = match &field.value {
            Some(FieldValue::Choice(v)) => v.first().cloned(),
            Some(FieldValue::Text(s)) => Some(s.clone()),
            _ => None,
        };
        let chosen = field
            .options
            .iter()
            .position(|o| Some(&o.export) == current.as_ref());
        let mut menu = Menu::below(anchor);
        for (i, o) in field.options.iter().enumerate() {
            // L'option en vigueur est cochée, comme dans la liste du zoom.
            let icon = (Some(i) == chosen).then_some(Icon::Check);
            menu = menu.item(icon, &o.display, "", i, true);
        }
        match chosen {
            Some(c) => menu.highlight_where(|i| i == c),
            None => menu.select_first(),
        }
        let (font_size, dpi) = (self.theme.font_size, self.dpi_scale as f32);
        let (ww, wh) = (self.width as i32, self.height as i32);
        match self.text.as_mut() {
            Some(text) => menu.layout(
                &mut |s: f32, t: &str| text.measure(s, t),
                font_size,
                dpi,
                ww,
                wh,
            ),
            None => menu.layout(
                &mut |s: f32, t: &str| t.chars().count() as f32 * s * 0.55,
                font_size,
                dpi,
                ww,
                wh,
            ),
        }
        log_line(&format!("champ : liste de « {} » ouverte", field.name));
        self.tip = None;
        self.focus_field = Some((fi, wi));
        self.field_menu = Some(FieldMenu {
            menu,
            name: field.name,
        });
        window.set_cursor(Cursor::Arrow);
        window.request_redraw();
    }

    /// Referme la liste si une carte modale ou un autre menu a pris la main.
    pub(super) fn drop_blocked_field_menu(&mut self) {
        let blocked = self.palette.is_some()
            || self.prompt.is_some()
            || self.settings.is_some()
            || self.protect.is_some()
            || !self.dialogs.is_empty()
            || self.capture.is_some()
            || self.context_menu.is_some()
            || self.zoom_menu.is_some();
        if blocked {
            self.field_menu = None;
        }
    }

    /// Donne l'événement à la liste déroulante ; vrai si elle l'a pris.
    ///
    /// Ouverte, elle prend tout, comme la liste du zoom : un clic dehors la
    /// referme sans rien atteindre dessous, la molette la fait défiler, le
    /// clavier la parcourt (flèches, initiales, Entrée, Échap).
    pub(super) fn field_menu_event(
        &mut self,
        event: &Event,
        window: &mut dyn WindowHandle,
    ) -> bool {
        let Some(open) = &mut self.field_menu else {
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
                if open.menu.wheel(delta) {
                    window.request_redraw();
                }
                return true;
            }
            Event::Nav { .. } | Event::Key(Key::Tab, _) => Outcome::Close,
            Event::Key(key, _) => {
                window.request_redraw();
                open.menu.key(key)
            }
            Event::Char(_, m) if m.ctrl => {
                self.field_menu = None;
                window.request_redraw();
                return false;
            }
            Event::Char(c, _) => {
                window.request_redraw();
                open.menu.char(c)
            }
            Event::Resize { .. } | Event::DpiChanged(_) | Event::FileDropped(_) | Event::Close => {
                self.field_menu = None;
                return false;
            }
            Event::Wake => return false,
        };
        match outcome {
            Outcome::Pick(index) | Outcome::Live(index) => {
                let Some(open) = self.field_menu.take() else {
                    return true;
                };
                let export = self
                    .loaded
                    .as_ref()
                    .and_then(|l| l.fields.iter().find(|f| f.name == open.name))
                    .and_then(|f| f.options.get(index))
                    .map(|o| o.export.clone());
                if let Some(export) = export {
                    self.set_field(&open.name, FieldValue::Choice(vec![export]));
                }
                window.request_redraw();
            }
            Outcome::Close => {
                self.field_menu = None;
                log_line("champ : liste fermée");
                window.request_redraw();
            }
            Outcome::Stay => {}
        }
        true
    }

    /// Dessine la liste déroulante, si elle est ouverte.
    pub(super) fn paint_field_menu(&mut self, frame: &mut Frame<'_>) {
        let theme = self.theme;
        if let (Some(open), Some(text)) = (&self.field_menu, self.text.as_mut()) {
            open.menu.paint(frame, text, &mut self.raster, &theme);
        }
    }

    // -----------------------------------------------------------------
    // La peinture.
    // -----------------------------------------------------------------

    /// Surligne les champs, cerne de rouge les champs obligatoires, pose la
    /// flèche des listes déroulantes, puis dessine la saisie en cours.
    pub(super) fn paint_fields(&mut self, view: &mut Frame<'_>) {
        if self.loaded.as_ref().is_none_or(|l| l.fields.is_empty()) {
            return;
        }
        let highlight = self.prefs.highlight_fields;
        let dpi = self.dpi_scale;
        let layout = self.layout();
        let (vw, vh) = (f64::from(view.width), f64::from(view.height));
        let scroll = self.scroll_y;
        // Ce qu'il faut dessiner, relevé d'abord : le dessin des flèches a
        // besoin du rastériseur, qu'on ne peut emprunter pendant qu'on lit
        // l'inventaire.
        let mut fills: Vec<(i32, i32, i32, i32)> = Vec::new();
        let mut required: Vec<(i32, i32, i32, i32)> = Vec::new();
        let mut arrows: Vec<(i32, i32, i32, i32)> = Vec::new();
        {
            let Some(l) = &self.loaded else { return };
            let mut matrices = vec![None; l.pages.len()];
            for f in &l.fields {
                if !fillable(f.kind) {
                    continue;
                }
                for w in &f.widgets {
                    let Some(page) = w.page.filter(|_| !w.hidden) else {
                        continue;
                    };
                    let Some(slot) = matrices.get_mut(page) else {
                        continue;
                    };
                    if slot.is_none() {
                        // Une page hors de la vue n'a rien à montrer : on
                        // ne calcule pas sa matrice (le coût d'une image
                        // doit suivre ce qu'on voit, pas la taille du
                        // formulaire).
                        let shown = layout.get(page).is_some_and(|b| {
                            b.visible
                                && f64::from(b.y + b.h as i32) >= scroll
                                && f64::from(b.y) <= scroll + vh
                        });
                        *slot = Some(if shown {
                            self.page_to_view(&layout, page)
                        } else {
                            None
                        });
                    }
                    let Some(Some(m)) = slot else { continue };
                    let d = m.transform_rect(&w.rect);
                    if d.x1 < 0.0 || d.y1 < 0.0 || d.x0 > vw || d.y0 > vh {
                        continue;
                    }
                    let r = (
                        d.x0.round() as i32,
                        d.y0.round() as i32,
                        (d.x1 - d.x0).round().max(1.0) as i32,
                        (d.y1 - d.y0).round().max(1.0) as i32,
                    );
                    if f.flags.read_only {
                        continue;
                    }
                    if highlight {
                        fills.push(r);
                    }
                    if f.flags.required {
                        required.push(r);
                    }
                    if f.kind == FieldType::ComboBox {
                        arrows.push(r);
                    }
                }
            }
        }
        for &(x, y, w, h) in &fills {
            multiply_rect(view, x, y, w, h, FIELD_HIGHLIGHT);
        }
        let t = dpi.round().max(1.0) as i32;
        for &r in &required {
            outline(view, r, t, self.theme.danger);
        }
        for &(x, y, w, h) in &arrows {
            let side = h.min(w / 3).max(1);
            let bx = x + w - side;
            let inset = (side / 6).max(1);
            fill_rect_blend(
                view,
                bx + inset,
                y + inset,
                side - 2 * inset,
                h - 2 * inset,
                ARROW_BACK,
                220,
            );
            let icon = (side as f32 * 0.8).max(6.0);
            icons::draw(
                view,
                &mut self.raster,
                Icon::ChevronDown,
                bx + ((side as f32 - icon) / 2.0) as i32,
                y + ((h as f32 - icon) / 2.0) as i32,
                icon,
                ARROW_INK,
            );
        }
        self.paint_field_edit(view);
    }

    /// Dessine la saisie en cours par-dessus le champ : son fond, le texte
    /// tapé, la sélection et le curseur.
    #[allow(clippy::too_many_lines)] // une passe : fond, lignes, sélection, curseur
    fn paint_field_edit(&mut self, view: &mut Frame<'_>) {
        let Some(b) = self.edit_box() else { return };
        let (accent, dpi, highlight) = (
            self.theme.accent,
            self.dpi_scale as f32,
            self.prefs.highlight_fields,
        );
        let Some(edit) = self.field_edit.as_mut() else {
            return;
        };
        let Some(f) = font(&mut self.field_font, &mut self.text) else {
            return;
        };
        let size = b.size;
        let lines = {
            let mut measure = |s: &str| f.measure(size, s);
            edit.editor
                .ensure_visible(&mut measure, b.layout_w, b.inner_h, b.line_h);
            edit.editor.layout(&mut measure, b.layout_w)
        };
        let shown: Vec<char> = edit.editor.display().chars().collect();
        let (sel_a, sel_b) = edit.editor.buffer.range();
        let caret = edit.editor.buffer.caret;
        let ink = bytes(edit.look.color);
        // Le fond du champ, surligné comme les autres quand le surlignage
        // est allumé : le champ en saisie ne change pas de teinte.
        let paper = edit.look.background.map_or((255, 255, 255), bytes);
        let paper = if highlight {
            let tint = |a: u8, b: u8| ((u32::from(a) * u32::from(b)) / 255) as u8;
            (
                tint(paper.0, FIELD_HIGHLIGHT.0),
                tint(paper.1, FIELD_HIGHLIGHT.1),
                tint(paper.2, FIELD_HIGHLIGHT.2),
            )
        } else {
            paper
        };
        // Tout se dessine dans le rectangle du champ, coupé à ses bords.
        let (x0, y0) = (b.x.floor() as i32, b.y.floor() as i32);
        let (w, h) = (b.w.ceil() as i32 + 1, b.h.ceil() as i32 + 1);
        let mut clip = view.sub(x0, y0, w.max(0) as u32, h.max(0) as u32);
        // `sub` recale un coin sorti de la vue à zéro : ce décalage remet les
        // coordonnées relatives au coin du champ.
        let (dx, dy) = ((x0 - x0.max(0)) as f32, (y0 - y0.max(0)) as f32);
        let (fx, fy) = (b.x - x0 as f32 + dx, b.y - y0 as f32 + dy);
        let border = b.border.round() as i32;
        clip.fill_rect(
            fx as i32 + border,
            fy as i32 + border,
            b.w as i32 - 2 * border,
            b.h as i32 - 2 * border,
            paper.0,
            paper.1,
            paper.2,
        );
        let visible = (edit.blink.elapsed().as_millis() / u128::from(BLINK_MS)) % 2 == 0;
        let caret_w = dpi.round().max(1.0) as i32;
        let rise = size * TEXT_ASCENT as f32;
        let fall = size * TEXT_DESCENT as f32;
        if let Some(cells) = edit.editor.comb {
            // Un peigne : un caractère centré par case, les séparateurs dans
            // la couleur de la bordure, redessinés par-dessus le fond.
            let cell = b.w / cells as f32;
            let sep = edit.look.border.map_or((170, 170, 170), bytes);
            for i in 1..cells {
                let x = (fx + cell * i as f32).round() as i32;
                clip.fill_rect(x, fy as i32, border.max(1), b.h as i32, sep.0, sep.1, sep.2);
            }
            let baseline = fy + b.first_baseline;
            for (i, ch) in shown.iter().enumerate().take(cells) {
                let cx = fx + cell * i as f32;
                if i >= sel_a && i < sel_b {
                    fill_rect_blend(
                        &mut clip,
                        cx.round() as i32,
                        (baseline - rise) as i32,
                        cell.round() as i32,
                        (rise + fall).ceil() as i32,
                        accent,
                        90,
                    );
                }
                let s = ch.to_string();
                let cw = f.measure(size, &s);
                f.draw(&mut clip, cx + (cell - cw) / 2.0, baseline, size, &s, ink);
            }
            if visible && sel_a == sel_b {
                let x = (fx + cell * caret.min(cells) as f32).round() as i32;
                let x = x.min((fx + b.w) as i32 - caret_w - 1);
                clip.fill_rect(
                    x,
                    (baseline - rise) as i32,
                    caret_w,
                    (rise + fall).ceil() as i32,
                    ink.0,
                    ink.1,
                    ink.2,
                );
            }
            return;
        }
        let scroll_y = if edit.editor.multiline {
            edit.editor.scroll
        } else {
            0.0
        };
        let caret_line = line_of(&lines, caret);
        for (row, line) in lines.iter().enumerate() {
            let baseline = fy + b.first_baseline + row as f32 * b.line_h - scroll_y;
            if baseline + fall < fy || baseline - rise > fy + b.h {
                continue;
            }
            let left = fx + b.pad + Self::line_offset(edit, &b, line);
            let prefix = |upto: usize| -> String {
                shown[line.start..upto.clamp(line.start, line.end)]
                    .iter()
                    .collect()
            };
            // La sélection, sous le texte.
            let (a, z) = (sel_a.max(line.start), sel_b.min(line.end));
            if a < z || (sel_a < sel_b && sel_a <= line.end && sel_b > line.end) {
                let xa = left + f.measure(size, &prefix(a));
                let xz = if z > a {
                    left + f.measure(size, &prefix(z))
                } else {
                    xa
                };
                // Un saut de ligne sélectionné se montre par un peu de
                // largeur au bout de la ligne.
                let tail = if sel_b > line.end && line.end < shown.len() {
                    size * 0.3
                } else {
                    0.0
                };
                fill_rect_blend(
                    &mut clip,
                    xa.round() as i32,
                    (baseline - rise).round() as i32,
                    (xz - xa + tail).round().max(1.0) as i32,
                    (rise + fall).ceil() as i32,
                    accent,
                    90,
                );
            }
            let text: String = shown[line.start..line.end].iter().collect();
            f.draw(&mut clip, left, baseline, size, &text, ink);
            if visible && row == caret_line {
                let x = (left + f.measure(size, &prefix(caret))).round() as i32;
                clip.fill_rect(
                    x,
                    (baseline - rise).round() as i32,
                    caret_w,
                    (rise + fall).ceil() as i32,
                    ink.0,
                    ink.1,
                    ink.2,
                );
            }
        }
    }

    /// Cadre d'accent autour du champ qui a le focus clavier.
    pub(super) fn paint_field_focus(&mut self, frame: &mut Frame<'_>) {
        let Some((fi, wi)) = self.focus_field else {
            return;
        };
        let Some((x, y, w, h)) = self.widget_view_rect(fi, wi) else {
            return;
        };
        let accent = self.theme.accent;
        let t = (2.0 * self.dpi_scale).round().max(1.0) as i32;
        outline(
            frame,
            (
                x.round() as i32 - 2,
                y.round() as i32 - 2,
                w.round() as i32 + 4,
                h.round() as i32 + 4,
            ),
            t,
            accent,
        );
    }
}
