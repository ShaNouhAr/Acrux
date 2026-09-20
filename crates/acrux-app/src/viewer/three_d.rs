//! Les modèles 3D dans le visualiseur (ISO 32000-2 §13.6).
//!
//! Une annotation `/3D` occupe un rectangle de la page. Tant qu'on ne la
//! touche pas, c'est l'apparence du document qui s'affiche — l'affiche du
//! modèle. Un clic **active** le modèle : Acrux lit alors sa géométrie
//! ([`acrux_features::three_d`]), la dessine dans le rectangle, et le geste
//! suivant la fait tourner.
//!
//! # Ce que fait la souris
//!
//! | Geste | Effet |
//! | --- | --- |
//! | glisser | tourner autour du modèle |
//! | glisser avec Maj, ou bouton du milieu | déplacer |
//! | molette | s'approcher, s'éloigner |
//! | double-clic | revenir à la vue du document |
//! | Échap | refermer le modèle |
//!
//! # Pourquoi une image de côté
//!
//! Le modèle n'est pas dessiné dans la page rendue : il vit au-dessus, dans
//! sa propre image, refaite quand la caméra bouge. La page, elle, n'est pas
//! rendue à nouveau — tourner un modèle ne coûte donc que le modèle.

use acrux_core::Rect;
use acrux_features::three_d::{self, Camera, Scene};

use super::Viewer;
use crate::platform::{Frame, WindowHandle};

/// Le geste en cours sur un modèle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Drag {
    /// Tourner autour du centre.
    Orbit,
    /// Déplacer la vue.
    Pan,
}

/// Un modèle activé.
pub(crate) struct Active3d {
    /// Rang dans la liste des modèles du document.
    pub(crate) slot: usize,
    /// Page qui le porte.
    pub(crate) page: usize,
    /// Rectangle occupé, en coordonnées de la page.
    pub(crate) rect: Rect,
    /// La géométrie.
    pub(crate) scene: Scene,
    /// La caméra courante.
    pub(crate) camera: Camera,
    /// Celle du document, pour y revenir.
    pub(crate) home: Camera,
    /// Dernière image rendue : largeur, hauteur, pixels.
    image: Option<(u32, u32, Vec<u8>)>,
    /// Vrai s'il faut redessiner le modèle.
    dirty: bool,
    /// Geste en cours : point de départ et sorte.
    drag: Option<(i32, i32, Drag)>,
}

impl Active3d {
    /// Marque l'image à refaire.
    fn touch(&mut self) {
        self.dirty = true;
    }
}

impl Viewer {
    /// Modèle 3D sous un point de la vue, s'il y en a un.
    pub(crate) fn model_at(&self, x: i32, y: i32) -> Option<usize> {
        let loaded = self.loaded.as_ref()?;
        let (page, point) = self.page_at(x, y)?;
        loaded.models.iter().position(|m| {
            m.page == page
                && point.x >= m.rect.x0
                && point.x <= m.rect.x1
                && point.y >= m.rect.y0
                && point.y <= m.rect.y1
        })
    }

    /// Vrai si le point tombe sur le modèle déjà activé.
    fn on_active_3d(&self, x: i32, y: i32) -> bool {
        let Some(active) = &self.three_d else {
            return false;
        };
        let Some(rect) = self.page_rect_to_view(active.page, active.rect) else {
            return false;
        };
        let (fx, fy) = (f64::from(x), f64::from(y));
        fx >= rect.x && fx <= rect.x + rect.w && fy >= rect.y && fy <= rect.y + rect.h
    }

    /// Active le modèle : lit sa géométrie et place la caméra.
    pub(crate) fn activate_3d(&mut self, slot: usize, window: &mut dyn WindowHandle) {
        let Some(loaded) = &self.loaded else { return };
        let Some(model) = loaded.models.get(slot).cloned() else {
            return;
        };
        let scene = match three_d::scene(&loaded.doc, &model) {
            Ok(scene) => scene,
            Err(e) => {
                self.set_notice(format!("{} : {e}", model.kind.label()));
                return;
            }
        };
        let camera = three_d::camera(&model, &scene);
        let triangles: usize = scene.items.iter().map(|i| i.mesh.faces.len()).sum();
        self.three_d = Some(Active3d {
            slot,
            page: model.page,
            rect: model.rect,
            scene,
            camera,
            home: camera,
            image: None,
            dirty: true,
            drag: None,
        });
        self.set_notice(crate::ui::lang::trf(
            "Modèle 3D ({} triangles) — glisser pour tourner, molette pour zoomer, Échap pour refermer",
            &[&triangles.to_string()],
        ));
        window.request_redraw();
    }

    /// Referme le modèle activé. Rend vrai s'il y en avait un.
    pub(crate) fn close_3d(&mut self) -> bool {
        self.three_d.take().is_some()
    }

    /// Un clic : active un modèle, ou commence un geste sur celui qui l'est.
    /// Rend vrai si le clic a été pris.
    pub(crate) fn three_d_mouse_down(
        &mut self,
        x: i32,
        y: i32,
        clicks: u8,
        shift: bool,
        window: &mut dyn WindowHandle,
    ) -> bool {
        let Some(slot) = self.model_at(x, y) else {
            return false;
        };
        // Un clic sur un autre modèle que celui qui est activé : on change de
        // modèle plutôt que de le faire tourner.
        if self.three_d.as_ref().is_none_or(|a| a.slot != slot) {
            self.activate_3d(slot, window);
            return true;
        }
        if let Some(active) = &mut self.three_d {
            if clicks >= 2 {
                // Double-clic : on revient à la vue du document.
                active.camera = active.home;
                active.touch();
            } else {
                active.drag = Some((x, y, if shift { Drag::Pan } else { Drag::Orbit }));
            }
        }
        window.request_redraw();
        true
    }

    /// Le déplacement de la souris pendant un geste.
    pub(crate) fn three_d_mouse_move(
        &mut self,
        x: i32,
        y: i32,
        window: &mut dyn WindowHandle,
    ) -> bool {
        let Some(active) = &mut self.three_d else {
            return false;
        };
        let Some((from_x, from_y, kind)) = active.drag else {
            return false;
        };
        let dx = (x - from_x) as f32;
        let dy = (y - from_y) as f32;
        match kind {
            Drag::Orbit => {
                // Un demi-écran fait un demi-tour : c'est le dosage des
                // visionneuses, ni trop vif ni trop lourd.
                active.camera.yaw -= dx * 0.01;
                active.camera.pitch = (active.camera.pitch + dy * 0.01).clamp(-1.5, 1.5);
            }
            Drag::Pan => {
                active.camera.pan[0] -= dx * 0.002;
                active.camera.pan[1] += dy * 0.002;
            }
        }
        active.drag = Some((x, y, kind));
        active.touch();
        window.request_redraw();
        true
    }

    /// Fin du geste.
    pub(crate) fn three_d_mouse_up(&mut self) {
        if let Some(active) = &mut self.three_d {
            active.drag = None;
        }
    }

    /// La molette : s'approcher ou s'éloigner du modèle.
    pub(crate) fn three_d_wheel(
        &mut self,
        x: i32,
        y: i32,
        delta: f64,
        window: &mut dyn WindowHandle,
    ) -> bool {
        if !self.on_active_3d(x, y) {
            return false;
        }
        let Some(active) = &mut self.three_d else {
            return false;
        };
        // Un cran change la distance d'un dixième : le zoom reste doux, et
        // multiplier plutôt qu'ajouter garde le même ressenti à toute échelle.
        let factor = (1.0 - delta as f32 * 0.1).clamp(0.2, 5.0);
        active.camera.distance = (active.camera.distance * factor).clamp(1e-3, 1e9);
        active.touch();
        window.request_redraw();
        true
    }

    /// Dessine le modèle activé par-dessus la page.
    pub(crate) fn paint_3d(&mut self, frame: &mut Frame<'_>) {
        let Some(active) = &self.three_d else { return };
        let Some(rect) = self.page_rect_to_view(active.page, active.rect) else {
            return;
        };
        let (w, h) = (rect.w.round().max(1.0), rect.h.round().max(1.0));
        // Un modèle plus grand que l'écran ne sert à rien : on borne, la page
        // pouvant être zoomée très fort.
        let (w, h) = (w.min(4096.0) as u32, h.min(4096.0) as u32);
        let Some(active) = &mut self.three_d else {
            return;
        };
        let stale = active
            .image
            .as_ref()
            .is_none_or(|(iw, ih, _)| *iw != w || *ih != h);
        if stale || active.dirty {
            let bitmap = three_d::render(&active.scene, &active.camera, w, h);
            active.image = Some((w, h, bitmap.data().to_vec()));
            active.dirty = false;
        }
        if let Some((iw, ih, pixels)) = &active.image {
            frame.blit_rgba_premultiplied(
                rect.x.round() as i32,
                rect.y.round() as i32,
                *iw,
                *ih,
                pixels,
            );
        }
    }
}
