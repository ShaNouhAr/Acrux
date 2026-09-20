//! Questions et messages du visualiseur, dans les fenêtres dessinées de
//! [`crate::ui::dialog`].
//!
//! Une question ne bloque rien : elle est posée, la page reste peinte
//! derrière un voile, et l'action attend la réponse dans [`Then`]. Tant
//! qu'une fenêtre est ouverte, elle reçoit seule le clavier et la souris.

use super::{log_line, EditOp, Viewer};
use crate::platform::{Cursor, Event, Frame, WindowHandle};
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
    /// Choisir la langue de l'interface : le rang du bouton dit laquelle.
    Language,
    /// Menu des paramètres : le rang du bouton dit quelle page ouvrir.
    Settings,
    /// Réglages des mises à jour.
    Updates,
    /// Document déjà protégé : changer le mot de passe, ou le retirer.
    Protection,
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
    /// Affiche un message d'erreur.
    pub(super) fn alert(&mut self, title: &str, message: &str) {
        log_line(&format!("message : {title} — {message}"));
        self.dialogs.push(Asking {
            dialog: Dialog::alert(title, message),
            kind: Kind::Alert,
            then: Then::Nothing,
        });
    }

    /// Pose une question à plusieurs réponses ; le dernier bouton annule.
    pub(super) fn push_choice(&mut self, title: &str, message: &str, labels: &[&str], then: Then) {
        self.dialogs.push(Asking {
            dialog: Dialog::choice(title, message, Tone::Question, labels),
            kind: Kind::Choice,
            then,
        });
    }

    /// Demande confirmation d'une action : `verb` nomme le bouton qui la
    /// lance.
    pub(super) fn confirm(&mut self, title: &str, message: &str, verb: &str, then: Then) {
        self.dialogs.push(Asking {
            dialog: Dialog::choice(title, message, Tone::Question, &[verb, "Annuler"]),
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
        self.dialogs.push(Asking {
            dialog: Dialog::choice(
                "Enregistrer les modifications ?",
                &format!(
                    "« {name} » a été modifié. Enregistrer les modifications avant de le fermer ?"
                ),
                Tone::Question,
                &["Enregistrer", "Ne pas enregistrer", "Annuler"],
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
            format!(
                "« {} » a été modifié. Enregistrer les modifications avant de quitter ?",
                self.document_name()
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
        self.dialogs.push(Asking {
            dialog: Dialog::choice(
                "Enregistrer les modifications ?",
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
            Event::MouseDown { x, y, .. } => top.dialog.button_at(x, y),
            Event::MouseMove { x, y, .. } => {
                if top.dialog.mouse_move(x, y) {
                    window.request_redraw();
                }
                window.set_cursor(if top.dialog.button_at(x, y).is_some() {
                    Cursor::Hand
                } else {
                    Cursor::Arrow
                });
                None
            }
            Event::Char(..) | Event::MouseUp { .. } | Event::Wheel { .. } => None,
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
                Then::Language => {
                    let choice = match index {
                        0 => Some(crate::ui::lang::Lang::Auto),
                        1 => Some(crate::ui::lang::Lang::French),
                        2 => Some(crate::ui::lang::Lang::English),
                        _ => None,
                    };
                    if let Some(choice) = choice {
                        self.set_language(choice, window);
                    }
                }
                Then::Settings => match index {
                    0 => self.open_language(window),
                    1 => self.open_updates(window),
                    _ => {}
                },
                Then::Updates => self.updates_answer(index, window),
                Then::Protection => match index {
                    0 => self.ask_password(None),
                    1 => self.remove_protection(),
                    _ => {}
                },
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
            Then::Nothing | Then::Language | Then::Settings | Then::Updates | Then::Protection => {}
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
