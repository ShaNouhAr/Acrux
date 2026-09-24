//! Tamponner, ajouter une image, créer un PDF — le chantier « Tampons et
//! images » vu de l'application.
//!
//! # Tamponner
//!
//! L'outil « Tamponner » vit dans la barre des commentaires. L'allumer
//! déroule le sélecteur (`ui/stamps.rs`) : les douze tampons d'Acrobat,
//! chacun rendu par le moteur tel qu'il sera posé, les images récentes, la
//! case « Ajouter mon nom et la date ». Le tampon choisi suit ensuite le
//! pointeur, en transparence, à la taille exacte qu'il aura sur la page ; un
//! clic le pose (`EditOp::Stamp`). Comme dans Acrobat, l'outil se referme
//! alors et le tampon neuf est **sélectionné**, ses poignées prêtes : on le
//! déplace et on l'agrandit tout de suite (voir `viewer/comments.rs`, qui
//! garde ses proportions). Maj+clic garde l'outil, pour en poser d'autres.
//!
//! Les vignettes du sélecteur se calculent à son ouverture, dans le
//! traitement de l'événement ; l'aperçu sous le pointeur, au plus une fois
//! par image quand il manque. Jamais de réveil de la fenêtre pour cela.
//!
//! # Ajouter une image
//!
//! « Ajouter une image » choisit un fichier puis attend un clic : un cadre
//! d'accent à la taille réelle de l'image montre où elle tombera. Ctrl+V
//! sur la page pose l'image du presse-papiers **tout de suite**, au milieu
//! de la partie visible, comme Acrobat. Dans les deux cas l'image est un
//! objet du contenu (`EditOp::AddImage`), aussitôt sélectionné dans
//! « Modifier les objets » : poignées, déplacement, redimensionnement.
//!
//! # Nouveau PDF
//!
//! « Nouveau PDF vierge » et « Nouveau PDF depuis le presse-papiers »
//! ouvrent un onglet neuf. Son fichier vit dans le dossier temporaire tant
//! qu'on ne l'a pas enregistré : le premier Ctrl+S demande où le ranger, et
//! il n'entre pas dans les documents récents.

// Coordonnées d'écran entières et échelles flottantes, comme le reste du
// visualiseur.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use acrux_core::{Point, Rect};
use acrux_document::{collect_pages, Document};
use acrux_features::annotations::AnnotMeta;
use acrux_features::create;
use acrux_features::rubber_stamp::{self, Language, Options, Source, StandardStamp};
use acrux_features::stamp::PreparedImage;
use acrux_graphics::Bitmap;

use super::{author_name, log_line, AnnotTool, Viewer};
use crate::platform::{Event, Frame, WindowHandle};
use crate::render_worker::{EditOp, Right};
use crate::ui::lang::{self, tr};
use crate::ui::objects::ViewRect;
use crate::ui::pickers::Outcome;
use crate::ui::stamps::{Pick, StampPicker, MAX_IMAGES};

/// Opacité de l'aperçu sous le pointeur : assez pour le lire, assez peu pour
/// qu'on ne le prenne pas pour un tampon posé.
const GHOST_ALPHA: f64 = 0.7;
/// Part de la page visible qu'une image ajoutée occupe au plus, à sa taille
/// naturelle.
const IMAGE_SHARE: f64 = 0.8;

/// Tampon choisi.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum StampChoice {
    /// Un des douze tampons standard.
    Standard(StandardStamp),
    /// Une image, par son fichier.
    Image(PathBuf),
}

/// Image qui attend son clic (« Ajouter une image »).
pub(super) struct PendingImage {
    /// L'image, décodée une fois.
    image: Arc<PreparedImage>,
}

/// Ce que l'outil « Tamponner » et « Ajouter une image » retiennent.
#[derive(Default)]
pub(super) struct StampState {
    /// Sélecteur déroulé.
    picker: Option<StampPicker>,
    /// Bouton sous lequel le sélecteur s'ouvre ; `None` : le réglage de la
    /// barre des commentaires.
    anchor: Option<(i32, i32, i32, i32)>,
    /// Vignettes des douze tampons, et ce qui les a fait : la ligne
    /// dynamique, la langue, l'échelle d'écran.
    thumbs: Option<(ThumbKey, Vec<Option<Bitmap>>)>,
    /// Aperçu sous le pointeur, déjà atténué, et sa clé.
    ghost: Option<(GhostKey, Option<Bitmap>)>,
    /// L'image du tampon image choisi, décodée une fois.
    image: Option<(PathBuf, Arc<PreparedImage>)>,
    /// « Ajouter une image » : l'image qui attend son clic.
    pub(super) pending: Option<PendingImage>,
}

/// Ce dont dépendent les vignettes.
#[derive(Debug, Clone, PartialEq)]
struct ThumbKey {
    dynamic: Option<String>,
    english: bool,
    dpi: u32,
}

/// Ce dont dépend l'aperçu sous le pointeur.
#[derive(Debug, Clone, PartialEq)]
struct GhostKey {
    choice: StampChoice,
    dynamic: Option<String>,
    english: bool,
    scale: u32,
    rotation: i32,
}

/// Langue des libellés des tampons : celle de l'interface.
fn label_lang() -> Language {
    if lang::english() {
        Language::En
    } else {
        Language::Fr
    }
}

/// Rend un tampon seul, sur fond transparent, à `scale` pixels par point,
/// tourné de `rotation` degrés : il est posé sur une page d'essai, puis
/// rendu par le moteur comme il le serait sur le document. Ce qu'on voit
/// dans le sélecteur et sous le pointeur est donc exactement ce qui sera
/// posé.
fn render_stamp(options: &Options, scale: f64, rotation: i32) -> Option<Bitmap> {
    let doc = create::new_document(&create::PageSetup {
        size: create::PageSize::Custom {
            width: 1000.0,
            height: 1000.0,
        },
        ..create::PageSetup::default()
    })
    .ok()?;
    let options = Options {
        page: 0,
        center: Point::new(500.0, 500.0),
        size: None,
        ..options.clone()
    };
    rubber_stamp::place(&doc, &options).ok()?;
    let pages = collect_pages(&doc).ok()?;
    let rect = rubber_stamp::list(&doc).ok()?.first()?.rect;
    acrux_render::render_annotation(&doc, pages.first()?, 0, scale, rotation, rect)
}

/// La même image, atténuée : ses composantes prémultipliées, alpha compris,
/// multipliées par `alpha`.
fn faded(mut bitmap: Bitmap, alpha: f64) -> Bitmap {
    for v in bitmap.data_mut() {
        *v = (f64::from(*v) * alpha).round() as u8;
    }
    bitmap
}

/// Nom affiché d'un fichier.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Lit et décode une image ; le message d'erreur est prêt à montrer.
fn read_image(path: &Path) -> Result<Arc<PreparedImage>, String> {
    let data = std::fs::read(path).map_err(|e| format!("{}\n\n{e}", path.display()))?;
    acrux_features::stamp::prepare_image(&data)
        .map(Arc::new)
        .map_err(|e| format!("{}\n\n{e}", path.display()))
}

/// Taille d'une image posée à sa taille naturelle (96 ppp), réduite pour
/// tenir dans les quatre cinquièmes de ce que la page montre.
fn fitted_size(image: &PreparedImage, shown: (f64, f64)) -> (f64, f64) {
    let (w, h) = image.natural_size(None);
    let k = (IMAGE_SHARE * shown.0 / w)
        .min(IMAGE_SHARE * shown.1 / h)
        .min(1.0);
    (w * k, h * k)
}

impl Viewer {
    // --- Le tampon choisi ---------------------------------------------------

    /// Tampon choisi : celui des préférences, « Approuvé » à défaut.
    pub(super) fn stamp_choice(&self) -> StampChoice {
        if self.prefs.stamp.is_empty() {
            if let Some(path) = self.prefs.stamp_images.first() {
                return StampChoice::Image(path.clone());
            }
        }
        StampChoice::Standard(
            StandardStamp::from_name(&self.prefs.stamp).unwrap_or(StandardStamp::Approved),
        )
    }

    /// Nom du tampon choisi, pour le bouton de la barre des commentaires.
    pub(super) fn stamp_label(&self) -> String {
        match self.stamp_choice() {
            StampChoice::Standard(s) => s.label(label_lang()).to_string(),
            StampChoice::Image(path) => file_name(&path),
        }
    }

    /// Seconde ligne du tampon dynamique, pour l'heure présente, si la case
    /// est cochée. Tirée au moment où l'on décide, elle voyage ensuite avec
    /// l'opération (voir `rubber_stamp`).
    fn stamp_dynamic_line(&self) -> Option<String> {
        self.prefs.stamp_dynamic.then(|| {
            rubber_stamp::dynamic_line(
                &author_name().unwrap_or_default(),
                label_lang(),
                crate::platform::local_offset_minutes(),
            )
        })
    }

    /// Ce que montre le tampon choisi ; l'image est décodée une fois, puis
    /// gardée. `None` (message affiché) si elle ne se lit plus.
    fn stamp_source(&mut self) -> Option<Source> {
        match self.stamp_choice() {
            StampChoice::Standard(s) => Some(Source::Standard(s)),
            StampChoice::Image(path) => {
                if let Some((p, image)) = &self.stamps.image {
                    if *p == path {
                        return Some(Source::Image(Arc::clone(image)));
                    }
                }
                match read_image(&path) {
                    Ok(image) => {
                        self.stamps.image = Some((path, Arc::clone(&image)));
                        Some(Source::Image(image))
                    }
                    Err(e) => {
                        self.alert(tr("Image illisible"), &e);
                        None
                    }
                }
            }
        }
    }

    /// Retient un choix dans les préférences.
    fn remember_stamp(&mut self, choice: &StampChoice) {
        match choice {
            StampChoice::Standard(s) => self.prefs.stamp = s.key().to_string(),
            StampChoice::Image(path) => {
                self.prefs.stamp.clear();
                self.prefs.stamp_images.retain(|p| p != path);
                self.prefs.stamp_images.insert(0, path.clone());
                self.prefs.stamp_images.truncate(MAX_IMAGES);
            }
        }
        self.stamps.ghost = None;
        self.save_prefs();
    }

    // --- Le sélecteur ----------------------------------------------------------

    /// Déroule le sélecteur de tampons sous `anchor`, ou sous le réglage de
    /// la barre des commentaires. Ses vignettes se calculent ici, une fois.
    pub(super) fn open_stamp_picker(&mut self, anchor: Option<(i32, i32, i32, i32)>) {
        self.stamp_thumbs();
        let images: Vec<String> = self
            .prefs
            .stamp_images
            .iter()
            .map(|p| file_name(p))
            .collect();
        let current = match self.stamp_choice() {
            StampChoice::Standard(s) => {
                StandardStamp::ALL.iter().position(|x| *x == s).unwrap_or(0)
            }
            StampChoice::Image(path) => {
                StandardStamp::ALL.len()
                    + self
                        .prefs
                        .stamp_images
                        .iter()
                        .position(|p| *p == path)
                        .unwrap_or(0)
            }
        };
        self.stamps.picker = Some(StampPicker::new(current, self.prefs.stamp_dynamic, images));
        self.stamps.anchor = anchor;
        self.tip = None;
        log_line("tampon : sélecteur ouvert");
    }

    /// Vrai si le sélecteur est déroulé.
    pub(super) fn stamp_picker_open(&self) -> bool {
        self.stamps.picker.is_some()
    }

    /// Referme le sélecteur et oublie l'image en attente : un autre outil
    /// prend la main.
    pub(super) fn close_stamp_tools(&mut self) {
        self.stamps.picker = None;
        self.stamps.pending = None;
    }

    /// Calcule les vignettes des douze tampons, si elles ne sont pas déjà
    /// celles de la langue, de l'échelle et de la ligne dynamique en cours.
    fn stamp_thumbs(&mut self) {
        let key = ThumbKey {
            dynamic: self.stamp_dynamic_line(),
            english: lang::english(),
            dpi: (self.dpi_scale * 1000.0).round() as u32,
        };
        if self.stamps.thumbs.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        let lang = label_lang();
        // Une seule échelle pour les douze : le plus large tient dans sa
        // case, et les autres se comparent à lui, comme sur la page.
        let (box_w, box_h) = StampPicker::thumb_box(self.dpi_scale as f32);
        let (max_w, max_h) = StandardStamp::ALL
            .iter()
            .fold((1.0_f64, 1.0_f64), |acc, s| {
                let (w, h) =
                    rubber_stamp::natural_size(&Source::Standard(*s), lang, key.dynamic.as_deref());
                (acc.0.max(w), acc.1.max(h))
            });
        let scale = (box_w / max_w).min(box_h / max_h);
        let thumbs = StandardStamp::ALL
            .iter()
            .map(|s| {
                let options = Options {
                    label_lang: lang,
                    dynamic: key.dynamic.clone(),
                    ..Options::new(Source::Standard(*s), 0, Point::new(0.0, 0.0))
                };
                render_stamp(&options, scale, 0)
            })
            .collect();
        self.stamps.thumbs = Some((key, thumbs));
    }

    /// Événements du sélecteur déroulé : il prend tout, comme les autres
    /// listes déroulées. Rend vrai s'il a pris l'événement.
    pub(super) fn stamp_picker_event(
        &mut self,
        event: &Event,
        window: &mut dyn WindowHandle,
    ) -> bool {
        let Some(picker) = self.stamps.picker.as_mut() else {
            return false;
        };
        let outcome = match *event {
            Event::MouseDown { x, y, .. } => picker.mouse_down(x, y),
            Event::MouseMove { x, y, .. } => {
                if picker.mouse_move(x, y) {
                    window.request_redraw();
                }
                window.set_cursor(crate::platform::Cursor::Arrow);
                return true;
            }
            Event::Key(key, _) => picker.key(key),
            Event::MouseUp { .. } | Event::Char(..) | Event::Wheel { .. } | Event::Nav { .. } => {
                return true
            }
            Event::Resize { .. }
            | Event::DpiChanged(_)
            | Event::FilesDropped { .. }
            | Event::Close
            | Event::Wake => {
                if !matches!(event, Event::Wake) {
                    self.stamps.picker = None;
                }
                return false;
            }
        };
        match outcome {
            Outcome::Stay => {}
            Outcome::Close => {
                self.stamps.picker = None;
                log_line("tampon : sélecteur refermé");
            }
            Outcome::Live(pick) | Outcome::Pick(pick) => {
                let close = matches!(outcome, Outcome::Pick(_));
                if close {
                    self.stamps.picker = None;
                }
                self.stamp_pick(pick, window);
            }
        }
        window.request_redraw();
        true
    }

    /// Ce qu'on a choisi dans le sélecteur.
    fn stamp_pick(&mut self, pick: Pick, window: &mut dyn WindowHandle) {
        match pick {
            Pick::Stamp(s) => {
                log_line(&format!("tampon : {} choisi", s.key()));
                self.remember_stamp(&StampChoice::Standard(s));
            }
            Pick::Image(index) => {
                if let Some(path) = self.prefs.stamp_images.get(index).cloned() {
                    log_line(&format!("tampon : image {} choisie", path.display()));
                    self.remember_stamp(&StampChoice::Image(path));
                    // Décodée tout de suite : une image devenue illisible le
                    // dit maintenant, pas au clic sur la page.
                    let _ = self.stamp_source();
                }
            }
            Pick::FromFile => {
                let Some(path) = window.open_image_dialog() else {
                    return;
                };
                match read_image(&path) {
                    Ok(image) => {
                        log_line(&format!("tampon : image {} choisie", path.display()));
                        self.stamps.image = Some((path.clone(), image));
                        self.remember_stamp(&StampChoice::Image(path));
                    }
                    Err(e) => self.alert(tr("Image illisible"), &e),
                }
            }
            Pick::Dynamic(on) => {
                log_line(&format!(
                    "tampon : nom et date {}",
                    if on { "oui" } else { "non" }
                ));
                self.prefs.stamp_dynamic = on;
                self.stamps.ghost = None;
                self.save_prefs();
                // Les vignettes montrent désormais la seconde ligne.
                self.stamp_thumbs();
            }
        }
    }

    /// Peint le sélecteur, par-dessus tout ce qui est sous lui.
    pub(super) fn paint_stamp_picker(&mut self, frame: &mut Frame<'_>) {
        if self.stamps.picker.is_none() {
            return;
        }
        let dpi = self.dpi_scale as f32;
        let anchor = self
            .stamps
            .anchor
            .or_else(|| self.mode_bar.setting_rect(0))
            .unwrap_or((
                self.view_left() as i32 + (16.0 * dpi) as i32,
                self.view_top() as i32,
                0,
                0,
            ));
        let theme = self.theme;
        // Les vignettes sont prêtées, pas copiées : la carte se repeint à
        // chaque survol.
        let StampState { picker, thumbs, .. } = &mut self.stamps;
        let thumbs = thumbs.as_ref().map_or(&[][..], |(_, t)| t.as_slice());
        let (Some(text), Some(picker)) = (self.text.as_mut(), picker.as_mut()) else {
            return;
        };
        picker.paint(frame, text, &mut self.raster, &theme, dpi, anchor, thumbs);
    }

    // --- Poser un tampon -------------------------------------------------------------

    /// Options de pose du tampon choisi au point `center` de `page`.
    fn stamp_options(&self, source: Source, page: usize, center: Point) -> Options {
        Options {
            label_lang: label_lang(),
            dynamic: match source {
                Source::Standard(_) => self.stamp_dynamic_line(),
                Source::Custom { .. } | Source::Image(_) => None,
            },
            meta: AnnotMeta::fresh(author_name().as_deref()),
            ..Options::new(source, page, center)
        }
    }

    /// Clic de l'outil « Tamponner » au point `(x, y)` de la vue : le tampon
    /// est posé, puis sélectionné. Maj garde l'outil allumé.
    pub(super) fn place_stamp(&mut self, x: i32, y: i32, keep: bool) {
        let Some((page, point)) = self.page_at(x, y) else {
            return;
        };
        let Some(source) = self.stamp_source() else {
            return;
        };
        let options = self.stamp_options(source, page, point);
        let label = options.dynamic.clone().map_or_else(
            || self.stamp_label(),
            |line| format!("{} — {line}", self.stamp_label()),
        );
        if !self.apply_edit(EditOp::Stamp {
            options: Box::new(options),
        }) {
            return;
        }
        log_line(&format!(
            "tampon posé page {} en ({:.0}, {:.0}) : {label}",
            page + 1,
            point.x,
            point.y
        ));
        self.set_notice(tr("Tampon posé — Ctrl+S pour enregistrer").into());
        if keep {
            return;
        }
        // Comme Acrobat : l'outil rend la main, et le tampon neuf est
        // sélectionné, ses poignées prêtes.
        self.annot_tool = None;
        self.stamps.picker = None;
        let last = self
            .loaded
            .as_mut()
            .map(|l| l.annots(page).len())
            .unwrap_or_default();
        if let Some(index) = last.checked_sub(1) {
            self.select_annot(page, index);
        }
    }

    /// L'aperçu du tampon sous le pointeur, à sa taille exacte, là où un
    /// clic le poserait (ramené dans la page près d'un bord).
    pub(super) fn paint_stamp_ghost(&mut self, frame: &mut Frame<'_>) {
        if self.annot_tool != Some(AnnotTool::Stamp) || self.stamps.picker.is_some() {
            return;
        }
        let Some((mx, my)) = self.last_mouse else {
            return;
        };
        let Some((page, point)) = self.page_at(mx, my) else {
            return;
        };
        let choice = self.stamp_choice();
        // Une image pas encore décodée ne se décode pas pendant la peinture :
        // elle l'est au choix, et au premier clic.
        let source = match &choice {
            StampChoice::Standard(s) => Source::Standard(*s),
            StampChoice::Image(path) => match &self.stamps.image {
                Some((p, image)) if p == path => Source::Image(Arc::clone(image)),
                _ => return,
            },
        };
        let Some(l) = self.loaded.as_ref() else {
            return;
        };
        let Some(target) = l.pages.get(page) else {
            return;
        };
        let (crop, rotate) = (target.crop_box(&l.doc), target.rotate(&l.doc));
        let rotation = l.view_rotation;
        let dynamic = match source {
            Source::Standard(_) => self.stamp_dynamic_line(),
            Source::Custom { .. } | Source::Image(_) => None,
        };
        let natural = rubber_stamp::natural_size(&source, label_lang(), dynamic.as_deref());
        let rect = rubber_stamp::stamp_rect(point, natural, crop, rotate);
        let Some(view) = self.page_rect_to_view(page, rect) else {
            return;
        };
        // À fort zoom, le tampon rendu en entier pèserait des dizaines de
        // mégaoctets — un tampon image de 200 pt à 1600 % fait 6400 pixels
        // de côté —, rendus pendant la peinture pour n'en montrer qu'un coin.
        // Plus grand que deux fois la vue, l'aperçu se réduit à son cadre.
        let budget = 2.0 * f64::from(self.view_width()) * f64::from(self.view_height());
        if view.w * view.h > budget {
            self.paint_place_frame(frame, &view);
            return;
        }
        let key = GhostKey {
            choice,
            dynamic: dynamic.clone(),
            english: lang::english(),
            scale: (self.scale() * 1000.0).round() as u32,
            rotation,
        };
        if self.stamps.ghost.as_ref().is_none_or(|(k, _)| *k != key) {
            // Un seul rendu, borné, puis gardé : la peinture suivante le
            // reprend tel quel.
            let options = Options {
                label_lang: label_lang(),
                dynamic,
                ..Options::new(source, 0, Point::new(0.0, 0.0))
            };
            let bitmap =
                render_stamp(&options, self.scale(), rotation).map(|b| faded(b, GHOST_ALPHA));
            self.stamps.ghost = Some((key, bitmap));
        }
        if let Some((_, Some(b))) = &self.stamps.ghost {
            frame.blit_rgba_premultiplied(
                view.x.round() as i32,
                view.y.round() as i32,
                b.width(),
                b.height(),
                b.data(),
            );
        }
    }

    /// Le cadre d'accent, légèrement teinté, qui montre où tombera ce qu'on
    /// pose : l'image en attente, ou un tampon trop grand pour son aperçu.
    fn paint_place_frame(&self, frame: &mut Frame<'_>, view: &ViewRect) {
        let dpi = self.dpi_scale as f32;
        let (x, y, w, h) = (
            view.x.round() as i32,
            view.y.round() as i32,
            view.w.round() as i32,
            view.h.round() as i32,
        );
        let accent = self.theme.accent;
        crate::ui::paint::round_rect_alpha(frame, x, y, w, h, 2.0 * dpi, accent, 0.12);
        crate::ui::paint::round_rect_outline(
            frame,
            x,
            y,
            w,
            h,
            2.0 * dpi,
            (1.5 * dpi).max(1.0),
            accent,
        );
    }

    // --- Ajouter une image ---------------------------------------------------------

    /// « Ajouter une image » : choisir un fichier, puis cliquer sur la page.
    pub(super) fn add_image_command(&mut self, window: &mut dyn WindowHandle) {
        if self.loaded.is_none() || self.showing_home() || !self.require_right(Right::Modify) {
            return;
        }
        let Some(path) = window.open_image_dialog() else {
            return;
        };
        let image = match read_image(&path) {
            Ok(image) => image,
            Err(e) => {
                self.alert(tr("Image illisible"), &e);
                return;
            }
        };
        // Un seul outil à la fois.
        self.commit_field();
        self.edit = None;
        self.sign_panel = None;
        self.objects = None;
        self.close_annot_tools();
        log_line(&format!(
            "image : {} ({} × {} px) en attente d'un clic",
            path.display(),
            image.width(),
            image.height()
        ));
        self.stamps.pending = Some(PendingImage { image });
        self.clamp_scroll();
        window.request_redraw();
    }

    /// Taille d'une image posée sur `page` : naturelle, dans la limite de ce
    /// que la page montre.
    fn image_size_on(&self, page: usize, image: &PreparedImage) -> Option<(f64, f64)> {
        let l = self.loaded.as_ref()?;
        let p = l.pages.get(page)?;
        let crop = p.crop_box(&l.doc);
        let (w, h) = (crop.width().abs(), crop.height().abs());
        let shown = if matches!(p.rotate(&l.doc), 90 | 270) {
            (h, w)
        } else {
            (w, h)
        };
        Some(fitted_size(image, shown))
    }

    /// Clic qui pose l'image en attente. Rend vrai s'il l'a posée.
    pub(super) fn place_pending_image(&mut self, x: i32, y: i32) -> bool {
        let Some(image) = self.stamps.pending.as_ref().map(|p| Arc::clone(&p.image)) else {
            return false;
        };
        let Some((page, point)) = self.page_at(x, y) else {
            return false;
        };
        self.stamps.pending = None;
        self.put_image(page, point, image);
        true
    }

    /// Ctrl+V sur la page : l'image du presse-papiers, posée tout de suite
    /// au milieu de ce qu'on voit. Rend vrai si le presse-papiers en avait
    /// une.
    pub(super) fn paste_image(&mut self, window: &mut dyn WindowHandle) -> bool {
        if self.loaded.is_none() || self.showing_home() || self.edit_on() {
            return false;
        }
        let Some(data) = window.clipboard_image() else {
            return false;
        };
        let image = match acrux_features::stamp::prepare_image(&data) {
            Ok(image) => Arc::new(image),
            Err(e) => {
                self.alert(tr("Image illisible"), &format!("{e}"));
                return true;
            }
        };
        if !self.require_right(Right::Modify) {
            return true;
        }
        // Le milieu de la vue, ramené dans la page la plus proche.
        let (vw, vh) = (self.view_width() as i32, self.view_height() as i32);
        let Some((page, point)) = self.page_at(vw / 2, vh / 2) else {
            return true;
        };
        let Some(crop) = self
            .loaded
            .as_ref()
            .and_then(|l| l.pages.get(page).map(|p| p.crop_box(&l.doc)))
        else {
            return true;
        };
        let center = Point::new(
            point.x.clamp(crop.x0.min(crop.x1), crop.x0.max(crop.x1)),
            point.y.clamp(crop.y0.min(crop.y1), crop.y0.max(crop.y1)),
        );
        log_line(&format!(
            "image collée : {} × {} px",
            image.width(),
            image.height()
        ));
        self.put_image(page, center, image);
        window.request_redraw();
        true
    }

    /// Pose une image sur une page, puis la sélectionne dans « Modifier les
    /// objets ».
    fn put_image(&mut self, page: usize, center: Point, image: Arc<PreparedImage>) {
        let Some(size) = self.image_size_on(page, &image) else {
            return;
        };
        if !self.apply_edit(EditOp::AddImage {
            page,
            image,
            center,
            size,
        }) {
            return;
        }
        log_line(&format!(
            "image posée page {} en ({:.0}, {:.0}), {:.0} × {:.0} pt",
            page + 1,
            center.x,
            center.y,
            size.0,
            size.1
        ));
        self.close_annot_tools();
        self.edit = None;
        self.sign_panel = None;
        self.load_objects(page);
        if let Some(tool) = self.objects.as_mut() {
            let last = tool.objects.len().checked_sub(1);
            tool.selected =
                last.filter(|&i| tool.objects[i].kind == acrux_features::edit_objects::Kind::Image);
        }
        self.set_notice(tr("Image ajoutée — glissez-la ou tirez ses poignées").into());
    }

    /// Le cadre de l'image en attente, à sa taille réelle, sous le pointeur,
    /// avec ses dimensions.
    pub(super) fn paint_pending_image(&mut self, frame: &mut Frame<'_>) {
        let Some(image) = self.stamps.pending.as_ref().map(|p| Arc::clone(&p.image)) else {
            return;
        };
        let Some((mx, my)) = self.last_mouse else {
            return;
        };
        let Some((page, point)) = self.page_at(mx, my) else {
            return;
        };
        let Some(size) = self.image_size_on(page, &image) else {
            return;
        };
        let rotate = self
            .loaded
            .as_ref()
            .and_then(|l| l.pages.get(page).map(|p| p.rotate(&l.doc)))
            .unwrap_or(0);
        let (w, h) = if matches!(rotate, 90 | 270) {
            (size.1, size.0)
        } else {
            size
        };
        let rect = Rect::new(
            point.x - w / 2.0,
            point.y - h / 2.0,
            point.x + w / 2.0,
            point.y + h / 2.0,
        );
        let Some(view) = self.page_rect_to_view(page, rect) else {
            return;
        };
        let dpi = self.dpi_scale as f32;
        let (x, y) = (view.x.round() as i32, view.y.round() as i32);
        let accent = self.theme.accent;
        self.paint_place_frame(frame, &view);
        let label = format!("{:.0} × {:.0} pt", size.0, size.1);
        let size_px = self.theme.font_size * dpi * 0.9;
        if let Some(text) = self.text.as_mut() {
            let tw = text.measure(size_px, &label);
            let pad = 6.0 * dpi;
            let (bx, by) = (x as f32 + pad, y as f32 + pad);
            crate::ui::paint::round_rect(
                frame,
                bx as i32,
                by as i32,
                (tw + 2.0 * pad) as i32,
                (size_px * 1.6) as i32,
                4.0 * dpi,
                accent,
            );
            text.draw(
                frame,
                bx + pad,
                by + size_px * 0.8 + text.ascent(size_px) / 2.0,
                size_px,
                &label,
                (255, 255, 255),
            );
        }
    }

    // --- Nouveau PDF ------------------------------------------------------------------

    /// Ouvre un document fabriqué ici dans un onglet neuf (voir
    /// [`Viewer::open_made_bytes`]). `title` nomme l'échec, s'il y en a un.
    pub(super) fn open_made(
        &mut self,
        name: &str,
        made: acrux_core::Result<Document>,
        (title, notice): (&'static str, String),
        window: &mut dyn WindowHandle,
    ) -> bool {
        match made.and_then(|doc| doc.save_full()) {
            Ok(bytes) => self.open_made_bytes(name, &bytes, (title, notice), window),
            Err(e) => {
                self.alert(tr(title), &format!("{name}\n\n{e}"));
                false
            }
        }
    }

    /// Ouvre dans un onglet neuf un document fabriqué ici, donné par ses
    /// octets. Son fichier va dans le dossier temporaire de **cette**
    /// instance d'Acrux, sous un nom qui n'écrase aucun autre onglet : le
    /// fil de rendu et l'annulation le relisent tant que l'onglet vit, il ne
    /// doit jamais être réécrit sous eux. Il est marqué « temporaire » (le
    /// premier Ctrl+S demande où l'enregistrer) et n'entre pas dans les
    /// documents récents. `title` nomme l'échec, s'il y en a un.
    pub(super) fn open_made_bytes(
        &mut self,
        name: &str,
        bytes: &[u8],
        (title, notice): (&'static str, String),
        window: &mut dyn WindowHandle,
    ) -> bool {
        let fail = |viewer: &mut Self, e: &dyn std::fmt::Display| {
            viewer.alert(tr(title), &format!("{name}\n\n{e}"));
            false
        };
        // Le fil de rendu relit le document sur le disque : le PDF fabriqué a
        // donc besoin d'un fichier. Un dossier par instance : deux Acrux
        // ouverts ne s'écrasent pas leurs « Sans titre.pdf ».
        let dir = made_dir();
        let _ = std::fs::create_dir_all(&dir);
        let open_paths: Vec<PathBuf> = self
            .loaded
            .iter()
            .chain(self.others.iter())
            .map(|l| l.path.clone())
            .collect();
        // « Sans titre.pdf », puis « Sans titre 2.pdf »… : jamais le fichier
        // d'un onglet ouvert.
        let target = std::iter::once(dir.join(format!("{name}.pdf")))
            .chain((2..10_000).map(|n| dir.join(format!("{name} {n}.pdf"))))
            .find(|p| !open_paths.contains(p))
            .unwrap_or_else(|| dir.join(format!("{name}.pdf")));
        if let Err(e) = std::fs::write(&target, bytes) {
            return fail(self, &e);
        }
        match Document::load(&target).and_then(|doc| {
            let pages = collect_pages(&doc)?;
            Ok((doc, pages))
        }) {
            Ok((doc, pages)) => {
                // Ce qui travaille sur le document actif ne suit pas dans le
                // nouvel onglet.
                self.leave_document_modes();
                self.leave_home();
                self.finish_open(target.clone(), doc, pages, None, window);
                if let Some(l) = &mut self.loaded {
                    l.temporary = true;
                    l.modified = true;
                }
                // Un fichier temporaire n'a rien à faire dans les récents :
                // il disparaîtra, et le rouvrir ne rendrait pas le document
                // qu'on croit.
                self.prefs.recent.retain(|p| *p != target);
                self.save_prefs();
                self.set_notice(notice);
                self.title_dirty = true;
                window.request_redraw();
                true
            }
            Err(e) => fail(self, &e),
        }
    }

    /// « Nouveau PDF vierge » : une page A4 blanche.
    pub(super) fn new_blank(&mut self, window: &mut dyn WindowHandle) {
        log_line("nouveau PDF vierge");
        let made = create::new_document(&create::PageSetup::default());
        self.open_made(
            tr("Sans titre"),
            made,
            (
                "Création impossible",
                tr("Nouveau PDF — Ctrl+S pour l'enregistrer").into(),
            ),
            window,
        );
    }

    /// « Nouveau PDF depuis le presse-papiers » : l'image qu'il contient,
    /// sinon son texte.
    pub(super) fn new_from_clipboard(&mut self, window: &mut dyn WindowHandle) {
        match document_from_clipboard(window) {
            Some(made) => {
                log_line("nouveau PDF depuis le presse-papiers");
                self.open_made(
                    tr("Presse-papiers"),
                    made,
                    (
                        "Création impossible",
                        tr("PDF créé depuis le presse-papiers — Ctrl+S pour l'enregistrer").into(),
                    ),
                    window,
                );
            }
            None => {
                self.set_notice(tr("Le presse-papiers ne contient ni image ni texte").into());
            }
        }
    }
}

/// Dossier des documents fabriqués par cette instance d'Acrux (images
/// ouvertes, combinaisons, nouveaux PDF) : un sous-dossier par processus du
/// dossier temporaire.
fn made_dir() -> PathBuf {
    std::env::temp_dir()
        .join("acrux-nouveaux")
        .join(std::process::id().to_string())
}

/// Vrai pour le fichier d'un document fabriqué : il n'entre pas dans les
/// documents récents.
pub(super) fn is_made(path: &Path) -> bool {
    path.starts_with(std::env::temp_dir().join("acrux-nouveaux"))
}

/// Document fait du presse-papiers : son image sur une page, sinon son texte
/// composé en pages ; `None` s'il est vide. C'est aussi ce qu'une insertion
/// de pages depuis le presse-papiers reprendra.
pub(super) fn document_from_clipboard(
    window: &mut dyn WindowHandle,
) -> Option<acrux_core::Result<Document>> {
    if let Some(data) = window.clipboard_image() {
        return Some(create::from_images(
            &[create::ImageInput::new(data)],
            &create::ImageLayout::default(),
        ));
    }
    let text = window.clipboard_text().filter(|t| !t.trim().is_empty())?;
    Some(create::from_text(&text, &create::TextLayout::default()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn un_tampon_se_rend_seul_a_la_bonne_taille() {
        let options = Options::new(
            Source::Standard(StandardStamp::Approved),
            0,
            Point::new(0.0, 0.0),
        );
        let (w, h) = rubber_stamp::natural_size(&options.source, Language::Fr, None);
        let bitmap = render_stamp(&options, 2.0, 0).unwrap();
        assert!((f64::from(bitmap.width()) - w * 2.0).abs() <= 2.0);
        assert!((f64::from(bitmap.height()) - h * 2.0).abs() <= 2.0);
        // Tourné d'un quart de tour, il se couche.
        let turned = render_stamp(&options, 2.0, 90).unwrap();
        assert_eq!(
            (turned.width(), turned.height()),
            (bitmap.height(), bitmap.width())
        );
        // Du vert, quelque part : le tampon est bien dessiné.
        assert!(bitmap
            .data()
            .chunks_exact(4)
            .any(|p| p[1] > p[0] + 40 && p[3] > 200));
    }

    #[test]
    fn une_image_ajoutee_tient_dans_la_page() {
        let png = acrux_graphics::encode_png_rgb(2000, 1000, &vec![0; 2000 * 1000 * 3]);
        let image = acrux_features::stamp::prepare_image(&png).unwrap();
        let (w, h) = fitted_size(&image, (595.0, 842.0));
        assert!((w - 476.0).abs() < 1e-6, "{w}");
        assert!((h - 238.0).abs() < 1e-6, "{h}");
        let small = acrux_features::stamp::prepare_image(&acrux_graphics::encode_png_rgb(
            40, 20, &[0; 2400],
        ))
        .unwrap();
        assert_eq!(fitted_size(&small, (595.0, 842.0)), (30.0, 15.0));
    }

    #[test]
    fn lapercu_est_attenue() {
        let mut b = Bitmap::new(1, 1);
        b.data_mut().copy_from_slice(&[200, 100, 0, 255]);
        assert_eq!(faded(b, 0.5).data(), &[100, 50, 0, 128]);
    }
}
