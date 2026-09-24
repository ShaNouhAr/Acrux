//! Zoom : niveaux, ajustements, Ctrl+molette **vers le pointeur**, et la
//! liste qui se déroule sous la case du zoom (ou au-dessus du zoom de la
//! barre d'état).
//!
//! Tout changement d'échelle passe par [`Viewer::rescale_at`] : il garde
//! immobile un point de la vue — le pointeur pour la molette, le centre pour
//! les boutons, la palette et la liste. Avant lui, zoomer ne gardait que la
//! hauteur du centre : la page glissait de côté à chaque cran, et Ctrl+molette
//! sur un coin de tableau le faisait sortir de l'écran. C'est la porte
//! d'entrée unique d'un changement de zoom : l'historique de la vue s'y
//! branchera.
//!
//! Le zoom est un état de la vue, pas du document : rien ici ne passe par le
//! fil de rendu ni par l'historique d'annulation.

use super::{log_line, PageBox, Viewer};
use crate::platform::{Cursor, Event, Frame, MouseButton, WindowHandle};
use crate::ui::lang::trf;
use crate::ui::menu::Outcome;
use crate::ui::prefs::Fit;
use crate::ui::zoompicker::{ZoomChoice, ZoomPicker};

/// Paliers de zoom des boutons, des touches et de la molette.
const ZOOM_STEPS: [f64; 16] = [
    0.25, 0.33, 0.5, 0.67, 0.75, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 6.0,
];

/// Plus petit zoom permis (10 %).
const MIN_ZOOM: f64 = 0.1;

/// Plus grand zoom permis (1600 %), si les pages le supportent.
const MAX_ZOOM: f64 = 16.0;

/// Nombre maximal de pixels d'une page rendue.
///
/// Une page est rendue d'un bloc : à 1600 %, une page A4 sur un écran à
/// 150 % ferait 19 000 × 27 000 pixels, deux gigaoctets — de quoi faire
/// tomber l'application. Le zoom est donc plafonné pour que la plus grande
/// page du document tienne dans 64 millions de pixels (256 Mo) : environ
/// 570 % pour de l'A4 à 150 %, bien plus pour un petit format. Le rendu par
/// tuiles lèvera ce plafond.
const MAX_PAGE_PIXELS: f64 = 64_000_000.0;

/// De quoi la liste du zoom s'est déroulée.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ZoomAnchor {
    /// La case du zoom de la barre d'outils (ou la palette).
    Toolbar,
    /// Le zoom de la barre d'état : la liste s'ouvre au-dessus.
    Status,
}

/// Défilement qui garde sous `point` (coordonnées de la vue) le même point
/// de la page quand sa boîte passe de `before` à `after`.
///
/// Le point est repéré en fractions de la boîte : cela reste juste pour une
/// page pivotée ou pour des pages de tailles différentes. Les fractions
/// peuvent sortir de 0..1 — pointeur dans la gouttière, à côté de la page —
/// et le calcul reste continu : la gouttière s'élargit autour du pointeur.
pub(super) fn anchored_scroll(
    before: &PageBox,
    after: &PageBox,
    point: (f64, f64),
    scroll: (f64, f64),
) -> (f64, f64) {
    let (px, py) = point;
    let (dx, dy) = (px + scroll.0, py + scroll.1);
    let u = (dx - f64::from(before.x)) / f64::from(before.w.max(1));
    let v = (dy - f64::from(before.y)) / f64::from(before.h.max(1));
    (
        f64::from(after.x) + u * f64::from(after.w) - px,
        f64::from(after.y) + v * f64::from(after.h) - py,
    )
}

/// Page qui sert de repère au zoom : celle sous le point `doc` (coordonnées
/// du document, défilement compris), sinon la plus proche. À égalité — le
/// pointeur entre deux pages d'une rangée —, la première.
pub(super) fn reference_page(layout: &[PageBox], doc: (f64, f64)) -> Option<usize> {
    layout
        .iter()
        .enumerate()
        .filter(|(_, b)| b.visible && b.w > 0 && b.h > 0)
        .map(|(i, b)| {
            let (x0, y0) = (f64::from(b.x), f64::from(b.y));
            let (x1, y1) = (x0 + f64::from(b.w), y0 + f64::from(b.h));
            let dx = (x0 - doc.0).max(doc.0 - x1).max(0.0);
            let dy = (y0 - doc.1).max(doc.1 - y1).max(0.0);
            (i, dx * dx + dy * dy)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Crans entiers de Ctrl+molette contenus dans `delta`, le reste gardé dans
/// `rest` pour la fois suivante.
///
/// Un pavé tactile de précision envoie des dixièmes de cran : les arrondir
/// un à un ne zoomait jamais, les compter pour un cran entier zoomait à la
/// moindre caresse. Ils s'additionnent donc jusqu'au cran ; un changement de
/// sens repart de zéro, pour que le geste réponde tout de suite.
pub(super) fn wheel_notches(rest: &mut f32, delta: f32) -> i32 {
    if !delta.is_finite() {
        return 0;
    }
    if *rest * delta < 0.0 {
        *rest = 0.0;
    }
    *rest += delta;
    let whole = rest.trunc();
    *rest -= whole;
    // Un geste fait quelques crans : la conversion ne déborde pas.
    #[allow(clippy::cast_possible_truncation)]
    let notches = whole as i32;
    notches
}

/// Palier suivant (`direction` > 0) ou précédent de `real` ; au-delà des
/// paliers, un quart de plus ou de moins.
fn next_step(real: f64, direction: i32) -> f64 {
    if direction > 0 {
        ZOOM_STEPS
            .iter()
            .copied()
            .find(|z| *z > real + 1e-6)
            .unwrap_or(real * 1.25)
    } else {
        ZOOM_STEPS
            .iter()
            .rev()
            .copied()
            .find(|z| *z < real - 1e-6)
            .unwrap_or(real / 1.25)
    }
}

/// Plus grand zoom pour une page de `w` × `h` points, sur un écran à
/// l'échelle `dpi_scale` : celui où elle remplit `MAX_PAGE_PIXELS`.
fn zoom_ceiling(w: f64, h: f64, dpi_scale: f64) -> f64 {
    let area = (w * h).max(1.0);
    let pixels_per_point = (MAX_PAGE_PIXELS / area).sqrt();
    (pixels_per_point / (dpi_scale * (96.0 / 72.0))).clamp(0.25, MAX_ZOOM)
}

/// Le même plafond, en échelle de rendu (pixels par point) : la borne que
/// `Viewer::scale` applique à tout zoom, d'où qu'il vienne.
pub(super) fn scale_ceiling(w: f64, h: f64, dpi_scale: f64) -> f64 {
    zoom_ceiling(w, h, dpi_scale) * dpi_scale * (96.0 / 72.0)
}

impl Viewer {
    /// Centre de la vue, en coordonnées de la vue.
    pub(super) fn view_center(&self) -> (f64, f64) {
        (
            f64::from(self.view_width()) / 2.0,
            f64::from(self.view_height()) / 2.0,
        )
    }

    /// Change l'échelle par `change` en gardant immobile le point `point` de
    /// la vue : ce qui était sous lui y reste.
    pub(super) fn rescale_at(&mut self, point: (f64, f64), change: impl FnOnce(&mut Self)) {
        let before = self.layout();
        let scroll = (self.scroll_x, self.scroll_y);
        let doc = (point.0 + scroll.0, point.1 + scroll.1);
        let reference = reference_page(&before, doc);
        change(self);
        if let Some(i) = reference {
            let after = self.layout();
            if let (Some(b), Some(a)) = (before.get(i), after.get(i)) {
                (self.scroll_x, self.scroll_y) = anchored_scroll(b, a, point, scroll);
            }
        }
        // Un glissement en cours visait une position de l'ancienne échelle.
        self.scroll_goal_y = None;
        self.clamp_scroll();
        self.title_dirty = true;
    }

    /// Plus grand zoom que supportent les pages du document (voir
    /// [`MAX_PAGE_PIXELS`]).
    pub(super) fn max_zoom(&self) -> f64 {
        let Some(l) = &self.loaded else {
            return MAX_ZOOM;
        };
        let (w, h) = l
            .pages
            .iter()
            .map(|p| {
                let b = p.crop_box(&l.doc);
                (b.width().abs(), b.height().abs())
            })
            .fold((1.0_f64, 1.0_f64), |(w, h), (pw, ph)| {
                if pw * ph > w * h {
                    (pw, ph)
                } else {
                    (w, h)
                }
            });
        zoom_ceiling(w, h, self.dpi_scale)
    }

    /// Zoom fixe `zoom` (1.0 = 100 %), en gardant immobile le point `point`
    /// de la vue. Borné par ce que supportent les pages ; quand il l'est, la
    /// barre d'état dit pourquoi.
    pub(super) fn zoom_at(&mut self, zoom: f64, point: (f64, f64)) {
        let max = self.max_zoom();
        // Comparés au pourcent affiché : taper « 575 » quand le plafond est
        // à 574,9 % ne demande rien de plus que ce que la barre montre, et
        // se faire dire « limité à 575 % » laissait croire à un refus.
        if (zoom * 100.0).round() > (max * 100.0).round() {
            // `max` est borné à 16 : l'arrondi tient dans un u32.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let percent = (max * 100.0).round() as u32;
            self.set_notice(trf(
                "Zoom limité à {} % pour ce document : au-delà, ses pages ne tiendraient plus en mémoire",
                &[&percent.to_string()],
            ));
        }
        let zoom = zoom.clamp(MIN_ZOOM, max.max(MIN_ZOOM));
        self.rescale_at(point, |v| {
            v.fit = Fit::Fixed;
            v.zoom = zoom;
        });
        log_line(&format!(
            "zoom : {:.0} % vers ({:.0}, {:.0})",
            self.effective_zoom() * 100.0,
            point.0,
            point.1
        ));
    }

    /// Zoom fixe, centré sur la vue.
    pub(super) fn set_zoom(&mut self, zoom: f64) {
        let center = self.view_center();
        self.zoom_at(zoom, center);
    }

    /// Palier suivant ou précédent, centré sur la vue.
    pub(super) fn zoom_step(&mut self, direction: i32) {
        let center = self.view_center();
        self.zoom_step_at(direction, center);
    }

    /// Palier suivant ou précédent, en gardant immobile le point `point`.
    pub(super) fn zoom_step_at(&mut self, direction: i32, point: (f64, f64)) {
        let next = next_step(self.effective_zoom(), direction);
        self.zoom_at(next, point);
    }

    /// Change le mode d'ajustement, en gardant le centre de la vue. « Page
    /// entière » montre la page courante en entier, calée en haut : garder le
    /// centre la couperait entre deux pages.
    pub(super) fn set_fit(&mut self, fit: Fit) {
        let page = self.current_page();
        let center = self.view_center();
        self.rescale_at(center, |v| v.fit = fit);
        if fit == Fit::Page {
            self.scroll_to_page(page);
        }
    }

    /// Ctrl+molette : zoome vers le pointeur `(x, y)` (coordonnées de la
    /// fenêtre), d'un palier par cran entier.
    pub(super) fn wheel_zoom(&mut self, delta: f32, x: i32, y: i32) {
        let notches = wheel_notches(&mut self.zoom_wheel, delta);
        if notches == 0 {
            return;
        }
        let (vx, vy) = (x - self.view_left() as i32, y - self.view_top() as i32);
        let inside =
            vx >= 0 && vy >= 0 && vx < self.view_width() as i32 && vy < self.view_height() as i32;
        // Hors de la vue (sur une barre), le centre fait l'affaire.
        let point = if inside {
            (f64::from(vx), f64::from(vy))
        } else {
            self.view_center()
        };
        for _ in 0..notches.unsigned_abs() {
            self.zoom_step_at(notches.signum(), point);
        }
    }

    /// Vrai quand quelque chose d'autre a la main : la liste du zoom ne
    /// s'ouvre pas, et se referme si elle l'était.
    fn zoom_menu_blocked(&self) -> bool {
        self.palette.is_some()
            || self.prompt.is_some()
            || self.settings.is_some()
            || self.protect.is_some()
            || !self.dialogs.is_empty()
            || self.capture.is_some()
            || self.context_menu.is_some()
            || self.edit_menu_open()
            || self.showing_home()
    }

    /// Referme la liste du zoom si quelque chose d'autre a pris la main (une
    /// question arrivée d'un autre fil, par exemple).
    pub(super) fn drop_blocked_zoom_menu(&mut self) {
        if self.zoom_menu.is_some() && self.zoom_menu_blocked() {
            self.zoom_menu = None;
        }
    }

    /// Déroule la liste du zoom depuis `anchor`, ou la referme si elle est
    /// déjà ouverte : la case se comporte comme toute liste déroulante.
    pub(super) fn open_zoom_menu(&mut self, anchor: ZoomAnchor, window: &mut dyn WindowHandle) {
        if self.zoom_menu.take().is_some() {
            log_line("zoom : liste fermée");
            window.request_redraw();
            return;
        }
        if self.zoom_menu_blocked() {
            return;
        }
        let dpi = self.dpi_scale;
        let rect = match anchor {
            ZoomAnchor::Toolbar if !self.fullscreen && !self.reading => self.toolbar.zoom_rect(),
            ZoomAnchor::Toolbar => None,
            ZoomAnchor::Status => self.status_rect(super::status::StatusSeg::Zoom),
        };
        // Barres masquées (plein écran, lecture) : la liste descend du haut
        // de la vue, centrée.
        let rect = rect.unwrap_or_else(|| {
            let w = (200.0 * dpi).round() as i32;
            let left = self.view_left() as i32 + (self.view_width() as i32 - w) / 2;
            (left, self.view_top() as i32, w, 0)
        });
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let percent = (self.effective_zoom() * 100.0).round() as u32;
        let mut picker = ZoomPicker::new(rect, percent, self.fit);
        // La géométrie tout de suite : un clic qui arriverait avant la
        // première peinture trouve déjà la carte à sa place.
        let (font, dpi) = (self.theme.font_size, dpi as f32);
        let (w, h) = (self.width as i32, self.height as i32);
        match self.text.as_mut() {
            Some(text) => picker.layout(&mut |s, t| text.measure(s, t), font, dpi, w, h),
            #[allow(clippy::cast_precision_loss)] // quelques caractères
            None => picker.layout(
                &mut |s, t| t.chars().count() as f32 * s * 0.55,
                font,
                dpi,
                w,
                h,
            ),
        }
        log_line(&format!("zoom : liste ouverte ({anchor:?}, {percent} %)"));
        // Une info-bulle apparaîtrait sinon par-dessus la liste.
        self.tip = None;
        self.toolbar.blur();
        self.zoom_menu = Some((picker, anchor));
        window.set_cursor(Cursor::Arrow);
        window.request_redraw();
    }

    /// Referme la liste du zoom sans rien changer.
    fn close_zoom_menu(&mut self, window: &mut dyn WindowHandle) {
        if self.zoom_menu.take().is_some() {
            log_line("zoom : liste fermée");
            window.request_redraw();
        }
    }

    /// Applique ce qu'on a choisi dans la liste.
    fn apply_zoom_choice(&mut self, choice: ZoomChoice) {
        log_line(&format!("zoom : choisi {choice:?}"));
        match choice {
            ZoomChoice::Percent(p) => self.set_zoom(f64::from(p) / 100.0),
            ZoomChoice::Fit(fit) => self.set_fit(fit),
        }
    }

    /// Donne l'événement à la liste du zoom ; vrai si elle l'a pris.
    ///
    /// Ouverte, elle prend **tout**, comme le menu du clic droit : un clic
    /// dehors la referme sans atteindre ce qui est dessous — un clic sur la
    /// case elle-même la referme donc sans la rouvrir —, la molette ne fait
    /// rien défiler, et le clavier va à son champ et à ses lignes. Seuls un
    /// raccourci Ctrl et Ctrl+molette la referment puis agissent ; le
    /// redimensionnement, un fichier déposé et la fermeture de la fenêtre
    /// la referment et suivent leur cours.
    pub(super) fn zoom_menu_event(&mut self, event: &Event, window: &mut dyn WindowHandle) -> bool {
        let Some((picker, _)) = &mut self.zoom_menu else {
            return false;
        };
        let outcome = match *event {
            Event::MouseDown { x, y, .. } => picker.mouse_down(x, y),
            Event::MouseUp { button, x, y } => {
                if button == MouseButton::Left {
                    picker.mouse_up(x, y)
                } else {
                    Outcome::Stay
                }
            }
            Event::MouseMove { x, y, .. } => {
                if picker.mouse_move(x, y) {
                    window.request_redraw();
                }
                let over_field = picker.over_field(x, y);
                window.set_cursor(if over_field {
                    Cursor::IBeam
                } else {
                    Cursor::Arrow
                });
                return true;
            }
            Event::Wheel { modifiers, .. } if modifiers.ctrl => {
                self.close_zoom_menu(window);
                return false;
            }
            Event::Wheel { .. } => return true,
            // Un bouton latéral de la souris est un clic ailleurs : la liste
            // se referme, et la vue ne bouge pas dessous.
            Event::Nav { .. } => Outcome::Close,
            Event::Key(key, _) => {
                window.request_redraw();
                picker.key(key)
            }
            Event::Char(_, m) if m.ctrl => {
                self.close_zoom_menu(window);
                return false;
            }
            Event::Char(c, _) => {
                if picker.char(c) {
                    window.request_redraw();
                }
                return true;
            }
            Event::Resize { .. } | Event::DpiChanged(_) | Event::FileDropped(_) | Event::Close => {
                self.zoom_menu = None;
                return false;
            }
            Event::Wake => return false,
        };
        match outcome {
            Outcome::Pick(choice) | Outcome::Live(choice) => {
                self.zoom_menu = None;
                self.apply_zoom_choice(choice);
                window.request_redraw();
            }
            Outcome::Close => self.close_zoom_menu(window),
            Outcome::Stay => {}
        }
        true
    }

    /// Dessine la liste du zoom, si elle est ouverte.
    pub(super) fn paint_zoom_menu(&mut self, frame: &mut Frame<'_>) {
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        if let (Some((picker, _)), Some(text)) = (&self.zoom_menu, self.text.as_mut()) {
            picker.paint(frame, text, &mut self.raster, &theme, dpi);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(x: i32, y: i32, w: u32, h: u32) -> PageBox {
        PageBox {
            x,
            y,
            w,
            h,
            visible: true,
        }
    }

    /// Point du document sous `point` pour une boîte et un défilement,
    /// exprimé en fractions de la boîte.
    fn fractions(b: &PageBox, point: (f64, f64), scroll: (f64, f64)) -> (f64, f64) {
        (
            (point.0 + scroll.0 - f64::from(b.x)) / f64::from(b.w),
            (point.1 + scroll.1 - f64::from(b.y)) / f64::from(b.h),
        )
    }

    #[test]
    fn le_point_sous_le_pointeur_ne_bouge_pas() {
        let before = page(100, 50, 600, 800);
        let after = page(40, 50, 1200, 1600);
        let (point, scroll) = ((300.0, 200.0), (0.0, 0.0));
        let new = anchored_scroll(&before, &after, point, scroll);
        let (u0, v0) = fractions(&before, point, scroll);
        let (u1, v1) = fractions(&after, point, new);
        assert!((u0 - u1).abs() < 1e-9 && (v0 - v1).abs() < 1e-9, "{new:?}");
        // Le calcul à la main : u = 1/3, v = 3/16.
        assert!((new.0 - (40.0 + 400.0 - 300.0)).abs() < 1e-9);
        assert!((new.1 - (50.0 + 300.0 - 200.0)).abs() < 1e-9);
    }

    #[test]
    fn dans_la_gouttiere_le_calcul_reste_continu() {
        // Pointeur à gauche de la page : u négatif.
        let before = page(100, 50, 600, 800);
        let after = page(16, 50, 1200, 1600);
        let point = (40.0, 100.0);
        let new = anchored_scroll(&before, &after, point, (0.0, 0.0));
        let (u, _) = fractions(&before, point, (0.0, 0.0));
        assert!(u < 0.0);
        let (u1, _) = fractions(&after, point, new);
        assert!((u - u1).abs() < 1e-9);
    }

    #[test]
    fn un_aller_retour_rend_le_defilement_de_depart() {
        let small = page(200, 16, 500, 700);
        let big = page(16, 32, 1000, 1400);
        let (point, scroll) = ((420.0, 333.0), (0.0, 120.0));
        let there = anchored_scroll(&small, &big, point, scroll);
        let back = anchored_scroll(&big, &small, point, there);
        assert!((back.0 - scroll.0).abs() < 1e-9 && (back.1 - scroll.1).abs() < 1e-9);
    }

    #[test]
    fn la_page_de_reference_est_sous_le_point_ou_la_plus_proche() {
        let layout = [
            page(100, 16, 600, 800),
            page(100, 832, 600, 800),
            PageBox::default(),
        ];
        assert_eq!(reference_page(&layout, (300.0, 900.0)), Some(1));
        // Dans la marge entre deux pages : la plus proche.
        assert_eq!(reference_page(&layout, (300.0, 820.0)), Some(0));
        assert_eq!(reference_page(&layout, (300.0, 830.0)), Some(1));
        // À côté de la page : elle reste la référence.
        assert_eq!(reference_page(&layout, (20.0, 400.0)), Some(0));
        assert_eq!(reference_page(&[], (0.0, 0.0)), None);
        // Une page masquée (mode page unique) ne compte pas.
        let hidden = [PageBox::default(), page(100, 16, 600, 800)];
        assert_eq!(reference_page(&hidden, (0.0, 0.0)), Some(1));
    }

    #[test]
    fn les_petits_crans_s_additionnent() {
        let mut rest = 0.0;
        for _ in 0..3 {
            assert_eq!(wheel_notches(&mut rest, 0.25), 0);
        }
        assert_eq!(wheel_notches(&mut rest, 0.25), 1, "le quatrième quart");
        assert_eq!(wheel_notches(&mut rest, 2.0), 2);
        // Changer de sens repart de zéro : un demi-cran en arrière ne
        // défait pas un reste en avant.
        assert_eq!(wheel_notches(&mut rest, 0.5), 0);
        assert_eq!(wheel_notches(&mut rest, -0.5), 0);
        assert_eq!(wheel_notches(&mut rest, -0.5), -1);
        assert_eq!(wheel_notches(&mut rest, f32::NAN), 0);
        assert!(rest.is_finite());
    }

    #[test]
    fn les_paliers_se_suivent() {
        assert!((next_step(1.0, 1) - 1.1).abs() < 1e-9);
        assert!((next_step(1.0, -1) - 0.9).abs() < 1e-9);
        // Entre deux paliers, le suivant dans le sens demandé.
        assert!((next_step(1.18, 1) - 1.25).abs() < 1e-9);
        assert!((next_step(1.18, -1) - 1.1).abs() < 1e-9);
        // Au-delà : un quart de plus, de moins.
        assert!((next_step(6.0, 1) - 7.5).abs() < 1e-9);
        assert!((next_step(0.25, -1) - 0.2).abs() < 1e-9);
    }

    #[test]
    fn le_plafond_protege_la_memoire() {
        // A4 sur un écran à 150 % : autour de 570 %.
        let a4 = zoom_ceiling(595.0, 842.0, 1.5);
        assert!(a4 > 5.5 && a4 < 5.8, "{a4}");
        // La page rendue au plafond tient dans le budget.
        let px = 595.0 * 842.0 * (a4 * 1.5 * 96.0 / 72.0).powi(2);
        assert!(px <= MAX_PAGE_PIXELS * 1.0001, "{px}");
        // Une carte de visite peut aller jusqu'à 1600 %.
        assert!((zoom_ceiling(252.0, 144.0, 1.0) - MAX_ZOOM).abs() < 1e-9);
        // Une affiche A0 garde au moins un quart.
        assert!(zoom_ceiling(2384.0 * 4.0, 3370.0 * 4.0, 2.0) >= 0.25);
    }

    /// Le plafond en pixels par point ne dépend pas de l'écran : un zoom
    /// fixe venu d'un écran moins dense (ou d'un autre document) ne peut pas
    /// rendre une page plus lourde que le budget.
    #[test]
    fn le_plafond_d_echelle_tient_le_budget_sur_tout_ecran() {
        for dpi in [1.0, 1.5, 2.0, 3.0] {
            let s = scale_ceiling(595.0, 842.0, dpi);
            let px = 595.0 * 842.0 * s * s;
            assert!(px <= MAX_PAGE_PIXELS * 1.0001, "{dpi} : {px}");
            assert!(px >= MAX_PAGE_PIXELS * 0.99, "{dpi} : {px}");
        }
        // Une page très haute ajustée à la largeur : 200 × 14 400 points,
        // la plus haute que permette la norme.
        let s = scale_ceiling(200.0, 14_400.0, 2.0);
        assert!(200.0 * 14_400.0 * s * s <= MAX_PAGE_PIXELS * 1.0001);
    }
}
