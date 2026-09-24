//! Barre d'état : le nom du document (ou un message passager) à gauche ;
//! à droite la rotation de la vue quand elle est tournée, la page, le zoom
//! et la disposition, puis la version.
//!
//! Les trois indications de droite sont des **boutons** : le survol les
//! éclaire et dit ce que fera le clic ; la page prend le focus du champ de
//! page (comme Ctrl+G), le zoom déroule la liste du zoom au-dessus de lui,
//! la disposition passe à la suivante. Avant, la barre n'était qu'une ligne
//! de texte — et elle affichait la durée du dernier rendu, une mesure de
//! développement qui n'avait rien à dire à la personne qui lit : elle va
//! désormais au journal (`ACRUX_LOG`).

use super::zoom::ZoomAnchor;
use super::{log_line, Viewer, MAX_WELCOME};
use crate::platform::{Frame, WindowHandle};
use crate::ui::lang::{self, tr, trf};
use crate::ui::paint::round_rect;
use crate::ui::toolbar::{page_tip, zoom_tip};

/// Rectangle `(x, y, largeur, hauteur)`, en coordonnées de la fenêtre.
type Rect = (i32, i32, i32, i32);

/// Une indication cliquable de la barre d'état.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusSeg {
    /// « page 3 / 12 » : aller à une page.
    Page,
    /// « 125 % (largeur) » : la liste du zoom.
    Zoom,
    /// « continu » : la disposition suivante.
    Layout,
    /// « vue 90° », tant que la vue est tournée : la remettre droite. Une
    /// vue tournée ne doit jamais passer pour un document tourné — celui-ci
    /// ne l'est pas, et rien ne s'enregistrera.
    Rotation,
}

fn inside(r: Rect, x: i32, y: i32) -> bool {
    x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
}

impl Viewer {
    /// Rectangle d'une indication au dernier dessin de la barre.
    pub(super) fn status_rect(&self, seg: StatusSeg) -> Option<Rect> {
        self.status_hits
            .iter()
            .find(|(s, _)| *s == seg)
            .map(|(_, r)| *r)
    }

    /// Indication sous un point de la fenêtre.
    fn status_seg_at(&self, x: i32, y: i32) -> Option<StatusSeg> {
        if self.status_height() == 0 {
            return None;
        }
        self.status_hits
            .iter()
            .find(|(_, r)| inside(*r, x, y))
            .map(|(s, _)| *s)
    }

    /// Survol ; vrai si l'indication survolée a changé (il faut repeindre,
    /// et réévaluer l'info-bulle).
    pub(super) fn status_mouse_move(&mut self, x: i32, y: i32) -> bool {
        let over = self.status_seg_at(x, y);
        let changed = over != self.status_hover;
        self.status_hover = over;
        changed
    }

    /// Vrai si le point est dans la barre d'état.
    pub(super) fn in_status_bar(&self, y: i32) -> bool {
        let h = self.status_height() as i32;
        h > 0 && y >= self.height as i32 - h
    }

    /// Clic dans la barre d'état.
    pub(super) fn status_click(&mut self, x: i32, y: i32, window: &mut dyn WindowHandle) {
        let Some(seg) = self.status_seg_at(x, y) else {
            return;
        };
        log_line(&format!("barre d'état : {seg:?}"));
        self.tip = None;
        // Comme un clic ailleurs, il met fin à une saisie de page restée
        // ouverte ; la page, elle, la rouvre aussitôt.
        self.toolbar.blur();
        match seg {
            StatusSeg::Page => {
                let info = self.toolbar_info();
                self.toolbar.focus_page(&info);
            }
            StatusSeg::Zoom => self.open_zoom_menu(ZoomAnchor::Status, window),
            StatusSeg::Layout => self.set_view_mode(self.view_mode.next()),
            StatusSeg::Rotation => {
                let now = self.loaded.as_ref().map_or(0, |l| l.view_rotation);
                self.rotate_view(-now);
            }
        }
        window.request_redraw();
    }

    /// Info-bulle de l'indication survolée.
    pub(super) fn status_tip(&self) -> Option<(String, Rect)> {
        // Liste du zoom déroulée : la bulle se tairait sous elle.
        if self.zoom_menu.is_some() {
            return None;
        }
        let seg = self.status_hover?;
        let rect = self.status_rect(seg)?;
        let text = match seg {
            StatusSeg::Page => page_tip()?,
            StatusSeg::Zoom => zoom_tip(),
            StatusSeg::Layout => trf(
                "Disposition : {} — cliquer pour passer à « {} »",
                &[
                    tr(self.view_mode.label()),
                    tr(self.view_mode.next().label()),
                ],
            ),
            StatusSeg::Rotation => trf(
                "Vue pivotée de {}° — cliquer pour la remettre droite",
                &[&self
                    .loaded
                    .as_ref()
                    .map_or(0, |l| l.view_rotation)
                    .to_string()],
            ),
        };
        Some((text, rect))
    }

    /// Dessine la barre d'état et relève le rectangle de ses indications.
    #[allow(clippy::too_many_lines)] // une passe : fond, textes, indications
    pub(super) fn paint_status(&mut self, frame: &mut Frame<'_>) {
        self.status_hits.clear();
        let t = self.theme;
        let h = self.status_height() as i32;
        let top = self.height as i32 - h;
        frame.fill_rect(0, top, self.width as i32, h, t.bar.0, t.bar.1, t.bar.2);
        frame.fill_rect(
            0,
            top,
            self.width as i32,
            1,
            t.separator.0,
            t.separator.1,
            t.separator.2,
        );
        let page = self.current_page() + 1;
        let zoom = self.effective_zoom();
        // Un message passager (fin d'export…) prend la place du nom de fichier
        // pendant quelques secondes.
        let notice = self.notice.as_ref().and_then(|(m, at)| {
            (at.elapsed() < std::time::Duration::from_secs(8)).then(|| m.clone())
        });
        // Les indications cliquables (document ouvert), ou un texte simple.
        let mut segments: Vec<(StatusSeg, String)> = Vec::new();
        let (left, plain) = if let Some(l) = self.loaded.as_ref().filter(|_| !self.showing_home()) {
            let name = l
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            // Document étiqueté : l'étiquette d'abord, le rang physique
            // entre parenthèses — « page iii (3 / 240) », comme Acrobat.
            let position = match l.labels.get(page - 1) {
                Some(label) if *label != page.to_string() => {
                    format!("page {label} ({page} / {})", l.pages.len())
                }
                _ => format!("page {page} / {}", l.pages.len()),
            };
            let fit = match self.fit.label() {
                "" => String::new(),
                mode => format!(" ({})", lang::tr(mode)),
            };
            if l.view_rotation != 0 {
                segments.push((
                    StatusSeg::Rotation,
                    trf("vue {}°", &[&l.view_rotation.to_string()]),
                ));
            }
            segments.push((StatusSeg::Page, position));
            segments.push((StatusSeg::Zoom, format!("{:.0} %{fit}", zoom * 100.0)));
            segments.push((
                StatusSeg::Layout,
                lang::tr(self.view_mode.label()).to_string(),
            ));
            (notice.unwrap_or(name), String::new())
        } else if self.home {
            // Un document est ouvert derrière : on dit comment y revenir,
            // c'est la question qu'on se pose ici.
            let count = self.prefs.recent.len().min(MAX_WELCOME);
            let right = match count {
                0 => String::new(),
                1 => lang::tr("1 document récent").to_string(),
                n => lang::trf("{} documents récents", &[&n.to_string()]),
            };
            let left =
                lang::tr("Accueil — Échap ou la maison pour revenir au document").to_string();
            (notice.unwrap_or(left), right)
        } else {
            let left =
                lang::tr("Aucun document — Ctrl+O pour ouvrir, ou déposez un PDF ici").to_string();
            (notice.unwrap_or(left), String::new())
        };
        let hover = self.status_hover;
        let Some(text) = &mut self.text else { return };
        let dpi = self.dpi_scale as f32;
        let size = t.font_size * dpi;
        let baseline = top as f32 + (h as f32 + text.ascent(size)) / 2.0 - 1.0;
        let pad = 10.0 * dpi;
        // La version, tout au bout, en plus petit : on sait d'un coup d'œil
        // quelle version on a sous la main, sans ouvrir de fenêtre « À propos ».
        let version = concat!("v", env!("CARGO_PKG_VERSION"));
        let small = size * 0.85;
        let version_w = text.measure(small, version);
        let version_x = self.width as f32 - pad - version_w;
        text.draw(frame, version_x, baseline, small, version, t.text_dim);
        let mut right_x = version_x;
        if !plain.is_empty() {
            right_x -= 1.6 * pad + text.measure(size, &plain);
            text.draw(frame, right_x, baseline, size, &plain, t.text_dim);
        }
        // Les indications, de droite à gauche. Chacune a sa marge, qui est
        // aussi la zone éclairée au survol : l'écart entre deux textes reste
        // celui d'avant, trois espaces environ.
        let seg_pad = (7.0 * dpi).round() as i32;
        let (seg_y, seg_h) = (top + (3.0 * dpi) as i32, h - (6.0 * dpi) as i32);
        let mut cursor = (version_x - 0.8 * pad).round() as i32;
        let mut hits = Vec::with_capacity(segments.len());
        for (seg, label) in segments.iter().rev() {
            let tw = text.measure(size, label).ceil() as i32;
            let w = tw + 2 * seg_pad;
            let x = cursor - w;
            let lit = hover == Some(*seg);
            if lit {
                round_rect(frame, x, seg_y, w, seg_h, 5.0 * dpi, t.hover);
            }
            let color = if lit { t.text } else { t.text_dim };
            text.draw(frame, (x + seg_pad) as f32, baseline, size, label, color);
            hits.push((*seg, (x, seg_y, w, seg_h)));
            cursor = x - (2.0 * dpi).round() as i32;
        }
        if !segments.is_empty() {
            right_x = cursor as f32;
        }
        let max_left = (right_x - 2.0 * pad).max(40.0);
        text.draw_clipped(frame, pad, baseline, size, &left, t.text, max_left);
        self.status_hits = hits;
    }
}
