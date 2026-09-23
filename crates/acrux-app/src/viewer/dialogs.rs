//! Questions et messages du visualiseur, dans les fenêtres dessinées de
//! [`crate::ui::dialog`], et le routage de toutes les cartes modales.
//!
//! Une question ne bloque rien : elle est posée, la page reste peinte
//! derrière un voile, et l'action attend la réponse dans [`Then`]. Tant
//! qu'une carte modale est ouverte — question, invite, fiche
//! « Paramètres » —, elle reçoit seule le clavier et la souris : rien ne
//! passe au document, à la barre ou aux onglets qu'elle recouvre.

use super::{log_line, EditOp, Viewer};
use crate::platform::{Cursor, Event, Frame, MouseButton, WindowHandle};
use crate::ui::dialog::{Dialog, Tone};
use crate::ui::lang::{tr, trf};

/// Ce qu'on fait quand l'utilisateur accepte.
#[derive(Debug, Clone)]
pub(super) enum Then {
    /// Rien : un simple message.
    Nothing,
    /// Fermer l'onglet actif.
    CloseTab,
    /// Quitter.
    Quit,
    /// Appliquer les biffures.
    ApplyRedactions,
    /// Supprimer une page.
    DeletePage(usize),
    /// Télécharger et lancer l'installateur (adresse, version).
    InstallUpdate(String, String),
    /// Document déjà protégé : changer sa protection, ou la retirer.
    Protection,
    /// Une action refusée par les permissions : saisir le mot de passe des
    /// permissions, qui les lève.
    OwnerPassword,
    /// Vider la liste des documents récents.
    ClearRecent,
}

/// Forme de la question, qui dit ce que veut chaque bouton.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// « OK ».
    Alert,
    /// « Faire », « Annuler ».
    Confirm,
    /// « Enregistrer », « Ne pas enregistrer », « Annuler ».
    SaveFirst,
    /// Un choix parmi plusieurs : c'est le rang du bouton qui compte, et le
    /// dernier annule.
    Choice,
}

/// Une question en attente de réponse.
#[derive(Debug)]
pub(super) struct Asking {
    dialog: Dialog,
    kind: Kind,
    then: Then,
}

impl Viewer {
    /// Pose une question. Elle apparaît en fondu : `sync_anim`, à la fin de
    /// l'événement, arme l'horloge des animations pour que la fenêtre se
    /// repeigne jusqu'à la dernière image.
    fn ask(&mut self, asking: Asking) {
        self.dialogs.push(asking);
    }

    /// Vrai tant que la question du dessus a une image d'apparition à peindre.
    pub(super) fn dialog_animating(&self) -> bool {
        self.dialogs.last().is_some_and(|a| a.dialog.animating())
    }

    /// Affiche un message d'erreur.
    pub(super) fn alert(&mut self, title: &str, message: &str) {
        log_line(&format!("message : {title} — {message}"));
        self.ask(Asking {
            dialog: Dialog::alert(title, message),
            kind: Kind::Alert,
            then: Then::Nothing,
        });
    }

    /// Pose une question à plusieurs réponses ; le dernier bouton annule.
    pub(super) fn push_choice(&mut self, title: &str, message: &str, labels: &[&str], then: Then) {
        self.ask(Asking {
            dialog: Dialog::choice(title, message, Tone::Question, labels),
            kind: Kind::Choice,
            then,
        });
    }

    /// Demande confirmation d'une action : `verb` nomme le bouton qui la
    /// lance.
    pub(super) fn confirm(&mut self, title: &str, message: &str, verb: &str, then: Then) {
        self.ask(Asking {
            dialog: Dialog::choice(title, message, Tone::Question, &[verb, tr("Annuler")]),
            kind: Kind::Confirm,
            then,
        });
    }

    /// Ferme l'onglet `index`, après avoir proposé d'enregistrer s'il a été
    /// modifié. Un onglet d'arrière-plan passe d'abord au premier plan : on
    /// ne demande pas d'enregistrer un document qu'on ne voit pas.
    pub(super) fn guard_close_tab(&mut self, index: usize, window: &mut dyn WindowHandle) {
        if index >= self.tab_count() {
            return;
        }
        // Ce qui est tapé n'est pas encore au document : sans cela, on
        // fermerait un onglet réputé intact en perdant la saisie.
        self.close_active();
        let modified = if index == self.active_tab {
            self.loaded.as_ref().is_some_and(|l| l.modified)
        } else {
            let at = if index > self.active_tab {
                index - 1
            } else {
                index
            };
            self.others.get(at).is_some_and(|l| l.modified)
        };
        if !modified {
            self.close_tab_now(index);
            return;
        }
        self.select_tab(index);
        let name = self.document_name();
        self.ask(Asking {
            dialog: Dialog::choice(
                tr("Enregistrer les modifications ?"),
                &trf(
                    "« {} » a été modifié. Enregistrer les modifications avant de le fermer ?",
                    &[&name],
                ),
                Tone::Question,
                &[tr("Enregistrer"), tr("Ne pas enregistrer"), tr("Annuler")],
            ),
            kind: Kind::SaveFirst,
            then: Then::CloseTab,
        });
        window.request_redraw();
    }

    /// Quitte, après avoir proposé d'enregistrer les documents modifiés.
    pub(super) fn guard_quit(&mut self, window: &mut dyn WindowHandle) {
        // La question est déjà posée : un second clic sur la croix ne la
        // double pas.
        if self.dialogs.iter().any(|d| matches!(d.then, Then::Quit)) {
            return;
        }
        // La saisie en cours passe au document avant qu'on se demande s'il
        // est modifié — sinon elle disparaîtrait sans un mot.
        self.close_active();
        let active = self.loaded.as_ref().is_some_and(|l| l.modified);
        let others = self.others.iter().filter(|l| l.modified).count();
        let total = others + usize::from(active);
        if total == 0 {
            self.save_prefs();
            window.close();
            return;
        }
        let message = if total == 1 {
            if !active {
                // Le seul document modifié passe au premier plan.
                if let Some(i) = self.others.iter().position(|l| l.modified) {
                    let index = if i >= self.active_tab { i + 1 } else { i };
                    self.select_tab(index);
                }
            }
            trf(
                "« {} » a été modifié. Enregistrer les modifications avant de quitter ?",
                &[&self.document_name()],
            )
        } else {
            trf(
                "{} documents ouverts ont été modifiés. Les enregistrer avant de quitter ?",
                &[&total.to_string()],
            )
        };
        let save = if total == 1 {
            tr("Enregistrer")
        } else {
            tr("Tout enregistrer")
        };
        self.ask(Asking {
            dialog: Dialog::choice(
                tr("Enregistrer les modifications ?"),
                &message,
                Tone::Question,
                &[save, tr("Quitter sans enregistrer"), tr("Annuler")],
            ),
            kind: Kind::SaveFirst,
            then: Then::Quit,
        });
        window.request_redraw();
    }

    /// Nom du document actif, pour les questions.
    fn document_name(&self) -> String {
        self.loaded
            .as_ref()
            .and_then(|l| l.path.file_name())
            .map_or_else(|| "Document".into(), |n| n.to_string_lossy().into_owned())
    }

    /// Donne l'événement à la fenêtre ouverte. Rend faux s'il n'y en a pas,
    /// ou si l'événement ne la concerne pas (redimensionnement, réveil…).
    pub(super) fn dialog_event(&mut self, event: &Event, window: &mut dyn WindowHandle) -> bool {
        let Some(top) = self.dialogs.last_mut() else {
            return false;
        };
        let choice = match *event {
            Event::Key(key, m) => top.dialog.key(key, m.shift),
            // L'appui enfonce ; c'est le relâchement, pointeur toujours sur
            // le bouton, qui choisit. On se ravise en glissant dehors.
            Event::MouseDown {
                button: MouseButton::Left,
                x,
                y,
                ..
            } => {
                top.dialog.mouse_down(x, y);
                None
            }
            Event::MouseUp { x, y, .. } => top.dialog.mouse_up(x, y),
            Event::MouseMove { x, y, .. } => {
                if top.dialog.mouse_move(x, y) {
                    window.request_redraw();
                }
                window.set_cursor(if top.dialog.over_button(x, y) {
                    Cursor::Hand
                } else {
                    Cursor::Arrow
                });
                None
            }
            Event::Char(..) | Event::MouseDown { .. } | Event::Wheel { .. } => None,
            Event::FileDropped(_) | Event::Close => return !matches!(event, Event::Close),
            _ => return false,
        };
        if let Some(index) = choice {
            if let Some(asking) = self.dialogs.pop() {
                self.answer(&asking, index, window);
            }
        }
        window.request_redraw();
        true
    }

    /// Exécute la réponse.
    fn answer(&mut self, asking: &Asking, index: usize, window: &mut dyn WindowHandle) {
        log_line(&format!(
            "réponse : {} → {}",
            asking.dialog.title,
            asking
                .dialog
                .buttons
                .get(index)
                .map_or("?", |b| b.label.as_str())
        ));
        if asking.kind == Kind::Choice {
            match asking.then {
                Then::Protection => match index {
                    0 => self.protect_change(),
                    1 => self.remove_protection(),
                    _ => {}
                },
                Then::OwnerPassword if index == 0 => {
                    self.ask_owner_password(super::OwnerThen::Unlock);
                }
                _ => {}
            }
            return;
        }
        let go = match asking.kind {
            // `Choice` est traité plus haut : le rang du bouton porte la
            // réponse. `Alert` n'a rien à faire non plus.
            Kind::Choice | Kind::Alert => false,
            Kind::Confirm => index == 0,
            Kind::SaveFirst => match index {
                0 => self.save_before(&asking.then, window),
                1 => true,
                _ => false,
            },
        };
        if !go {
            return;
        }
        match asking.then.clone() {
            // Rien à faire : un message, ou un choix déjà appliqué.
            Then::Nothing | Then::Protection | Then::OwnerPassword => {}
            Then::CloseTab => {
                let active = self.active_tab;
                self.close_tab_now(active);
            }
            Then::Quit => {
                self.save_prefs();
                window.close();
            }
            Then::ApplyRedactions => {
                self.apply_edit(EditOp::ApplyRedactions);
                log_line("biffures appliquées");
            }
            Then::DeletePage(page) => {
                self.apply_edit(EditOp::Delete { pages: vec![page] });
            }
            Then::InstallUpdate(url, version) => self.install_update_now(&url, &version),
            Then::ClearRecent => self.clear_recent(),
        }
    }

    /// Donne l'événement à l'invite ou à la fiche « Paramètres », si l'une
    /// est ouverte. Elle prend **tout** le clavier et la souris : les
    /// raccourcis (Ctrl+W, Ctrl+Maj+P, F6…), la molette et les clics
    /// n'atteignent pas ce qu'elle recouvre — une invite ouverte depuis la
    /// barre (F6) restait sinon insaisissable, et Ctrl+W fermait l'onglet
    /// sous l'invite du mot de passe. Le redimensionnement, le réveil et la
    /// fermeture de la fenêtre suivent leur cours : rend faux pour eux.
    ///
    /// Seule compte une carte ouverte **avant** l'événement : le relâchement
    /// qui ouvre l'invite du surlignage va, lui, au document.
    pub(super) fn modal_event(&mut self, event: &Event, window: &mut dyn WindowHandle) -> bool {
        if self.settings.is_some() {
            return self.settings_event(event, window);
        }
        if self.prompt.is_none() {
            return false;
        }
        match *event {
            Event::Key(key, m) => self.prompt_key(key, m, window),
            // Ctrl+V colle dans le champ ; les autres raccourcis s'arrêtent là.
            Event::Char(c, m) if m.ctrl => {
                let paste = matches!(c, 'v' | 'V' | '\u{16}');
                if let Some(p) = &mut self.prompt {
                    if paste && p.card.focus == crate::ui::modal::PromptFocus::Field {
                        let text = window.clipboard_text().unwrap_or_default();
                        if p.input.paste(&text) == crate::ui::input::InputAction::Changed {
                            p.error = None;
                        }
                    }
                }
            }
            Event::Char(c, _) => {
                if let Some(p) = &mut self.prompt {
                    p.card.char(c, &mut p.input, &mut p.error);
                }
            }
            Event::MouseDown {
                button: MouseButton::Left,
                x,
                y,
                ..
            } => {
                self.tip = None;
                if let Some(p) = &mut self.prompt {
                    p.card.mouse_down(x, y, &mut p.input);
                }
            }
            Event::MouseUp { x, y, .. } => {
                // Un geste commencé sous la carte ne se poursuit pas.
                self.drag_last = None;
                self.sel_dragging = false;
                let act = self.prompt.as_mut().and_then(|p| p.card.mouse_up(x, y));
                self.prompt_act(act, window);
            }
            Event::MouseMove { x, y, .. } => {
                self.tip = None;
                let Some(p) = &mut self.prompt else {
                    return true;
                };
                let changed = p.card.buttons.mouse_move(x, y);
                window.set_cursor(if p.card.buttons.hit(x, y).is_some() {
                    Cursor::Hand
                } else if p.card.over_field(x, y) {
                    Cursor::IBeam
                } else {
                    Cursor::Arrow
                });
                if !changed {
                    return true;
                }
            }
            // Les autres boutons de la souris, la molette et un fichier
            // déposé (il ouvrirait un document sous l'invite) s'arrêtent là.
            Event::MouseDown { .. } | Event::Wheel { .. } | Event::FileDropped(_) => {}
            _ => return false,
        }
        window.request_redraw();
        true
    }

    /// La fiche « Paramètres » : même routage que l'invite.
    fn settings_event(&mut self, event: &Event, window: &mut dyn WindowHandle) -> bool {
        let state = Self::settings_state(
            &self.prefs,
            &self.theme,
            self.update_rx.is_some(),
            self.update_found.as_ref(),
            self.update_outcome.as_ref(),
        );
        let Some(sheet) = &mut self.settings else {
            return false;
        };
        let action = match *event {
            Event::Key(key, m) => sheet.key(key, m.shift, &state),
            Event::MouseDown {
                button: MouseButton::Left,
                x,
                y,
                ..
            } => {
                self.tip = None;
                sheet.mouse_down(x, y);
                None
            }
            Event::MouseUp { x, y, .. } => {
                self.drag_last = None;
                self.sel_dragging = false;
                sheet.mouse_up(x, y)
            }
            Event::MouseMove { x, y, .. } => {
                self.tip = None;
                window.set_cursor(if sheet.over(x, y) {
                    Cursor::Hand
                } else {
                    Cursor::Arrow
                });
                if !sheet.mouse_move(x, y) {
                    return true;
                }
                None
            }
            Event::Char(..)
            | Event::MouseDown { .. }
            | Event::Wheel { .. }
            | Event::FileDropped(_) => None,
            _ => return false,
        };
        if let Some(action) = action {
            self.settings_action(action, window);
        }
        window.request_redraw();
        true
    }

    /// Enregistre ce que l'action va faire disparaître ; faux si un
    /// enregistrement a échoué (l'action n'a alors pas lieu).
    fn save_before(&mut self, then: &Then, window: &mut dyn WindowHandle) -> bool {
        if !matches!(then, Then::Quit) {
            return self.save(false, window);
        }
        for index in 0..self.tab_count() {
            self.select_tab(index);
            if self.loaded.as_ref().is_some_and(|l| l.modified) && !self.save(false, window) {
                return false;
            }
        }
        true
    }

    /// Peint la fenêtre ouverte par-dessus tout le reste.
    pub(super) fn paint_dialog(&mut self, frame: &mut Frame<'_>) {
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        let (Some(top), Some(text)) = (self.dialogs.last_mut(), self.text.as_mut()) else {
            return;
        };
        top.dialog.paint(frame, text, &theme, dpi);
    }
}
