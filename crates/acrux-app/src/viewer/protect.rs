//! Protection du document dans le visualiseur : la fenêtre « Protéger par
//! mot de passe », le mot de passe des permissions, et les droits qu'un
//! document protégé laisse à qui l'a ouvert.
//!
//! Un document ouvert avec le seul mot de passe d'ouverture n'accorde que
//! ses permissions : l'impression, la copie et les modifications passent par
//! [`Viewer::rights`]. Ce qu'elles interdisent se demande ici — avec, à
//! chaque refus, l'offre de saisir le mot de passe des permissions, qui
//! donne tous les droits (c'est aussi ce que fait Acrobat).

use acrux_document::protect::{Permissions, PrintLevel};

use super::dialogs::Then;
use super::{Prompt, PromptKind, Viewer};
use crate::platform::{Cursor, Event, Frame, MouseButton, WindowHandle};
use crate::render_worker::Right;
use crate::ui::input::TextInput;
use crate::ui::lang::{tr, trf};
use crate::ui::protect::{Action, ProtectDialog, Request};

/// Ce qu'on fait une fois le mot de passe des permissions accepté.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OwnerThen {
    /// Ouvrir la fenêtre « Changer la protection ».
    Protect,
    /// Retirer la protection.
    Unprotect,
    /// Rien de plus : les droits sont levés, l'action refusée peut se refaire.
    Unlock,
}

/// Résolution plafond de l'impression « basse résolution » : lisible, mais
/// pas une copie fidèle du document (Acrobat imprime alors une image de
/// 150 ppp).
pub(super) const LOW_PRINT_DPI: f64 = 150.0;

impl Viewer {
    /// Droits sur le document actif : tous s'il est en clair ou ouvert avec
    /// le mot de passe des permissions, sinon ceux que ses permissions
    /// laissent.
    pub(super) fn rights(&self) -> Permissions {
        self.loaded
            .as_ref()
            .and_then(|l| l.doc.security())
            .filter(|h| !h.is_owner())
            .map_or_else(Permissions::all, |h| Permissions::from_p(h.permissions()))
    }

    /// Vrai si le document permet `right` ; sinon, le dit et propose le mot
    /// de passe des permissions.
    pub(super) fn require_right(&mut self, right: Right) -> bool {
        if right.allowed_by(self.rights()) {
            return true;
        }
        self.refuse(
            tr("Modification interdite"),
            tr("Les permissions du document interdisent cette modification. Le mot de passe des permissions lève cette restriction."),
        );
        false
    }

    /// Refus motivé, avec l'offre de saisir le mot de passe des permissions.
    pub(super) fn refuse(&mut self, title: &str, message: &str) {
        self.push_choice(
            title,
            message,
            &[tr("Saisir le mot de passe"), tr("Annuler")],
            Then::OwnerPassword,
        );
    }

    /// Ce que le document interdit, dit une fois à l'ouverture.
    pub(super) fn announce_restrictions(&mut self) {
        let Some(h) = self.loaded.as_ref().and_then(|l| l.doc.security()) else {
            return;
        };
        if h.is_owner() {
            return;
        }
        let list: Vec<&str> = Permissions::from_p(h.permissions())
            .restrictions()
            .into_iter()
            .map(tr)
            .collect();
        if list.is_empty() && !h.perms_tampered() {
            return;
        }
        let mut message = trf("Document protégé : {}", &[&list.join(", ")]);
        if h.perms_tampered() {
            message.push_str(tr(
                " (permissions retouchées : les plus strictes s'appliquent)",
            ));
        }
        self.set_notice(message);
    }

    /// « Protéger par mot de passe ». Document en clair : la fenêtre.
    /// Déjà protégé : changer ou retirer, ce que seul le mot de passe des
    /// permissions autorise — il est demandé s'il n'a pas été donné.
    pub(super) fn protect_command(&mut self) {
        let Some(l) = &self.loaded else { return };
        if !l.doc.is_encrypted() {
            self.protect = Some(ProtectDialog::new());
            return;
        }
        if !l.doc.can_change_protection() {
            self.ask_owner_password(OwnerThen::Protect);
            return;
        }
        self.push_choice(
            tr("Ce document est déjà protégé"),
            tr("Changer sa protection, ou la retirer ?"),
            &[
                tr("Changer la protection"),
                tr("Retirer la protection"),
                tr("Annuler"),
            ],
            Then::Protection,
        );
    }

    /// Ouvre la fenêtre sur la protection en place : ses permissions, et le
    /// mot de passe d'ouverture exigé ou non.
    pub(super) fn protect_change(&mut self) {
        let Some(l) = &self.loaded else { return };
        let Some(h) = l.doc.security() else {
            self.protect = Some(ProtectDialog::new());
            return;
        };
        self.protect = Some(ProtectDialog::from_current(
            Permissions::from_p(h.permissions()),
            l.doc.opens_without_password(),
        ));
    }

    /// Demande le mot de passe des permissions.
    pub(super) fn ask_owner_password(&mut self, then: OwnerThen) {
        if self.loaded.is_none() {
            return;
        }
        let mut input = TextInput::new(tr("Mot de passe des permissions"));
        input.masked = true;
        let label = match then {
            OwnerThen::Protect => tr("Nécessaire pour changer la protection :"),
            OwnerThen::Unprotect => tr("Nécessaire pour retirer la protection :"),
            OwnerThen::Unlock => tr("Il donne tous les droits sur ce document :"),
        };
        self.prompt = Some(Prompt::new(
            tr("Mot de passe des permissions"),
            label.into(),
            input,
            PromptKind::OwnerPassword { then },
        ));
    }

    /// Mot de passe des permissions saisi : accepté, il donne tous les
    /// droits et l'on reprend là où on en était.
    pub(super) fn owner_password_entered(
        &mut self,
        value: &str,
        then: OwnerThen,
        window: &mut dyn WindowHandle,
    ) {
        let Some(l) = &mut self.loaded else {
            self.prompt = None;
            return;
        };
        let pw = value.as_bytes().to_vec();
        // Un mot de passe refusé laisse le gestionnaire en place ; celui
        // d'ouverture le remplace par le même : dans les deux cas, rien ne
        // change pour le document.
        let owner =
            l.doc.authenticate(&pw).is_ok() && l.doc.security().is_some_and(|h| h.is_owner());
        if !owner {
            if let Some(p) = &mut self.prompt {
                p.error = Some(tr("Ce n'est pas le mot de passe des permissions").into());
                p.input.clear();
            }
            return;
        }
        // L'annulation rouvre le fichier avec ce mot de passe : elle doit
        // pouvoir rejouer ce qui n'est permis qu'au propriétaire.
        l.password = Some(pw);
        self.prompt = None;
        match then {
            OwnerThen::Protect => self.protect_command(),
            OwnerThen::Unprotect => self.remove_protection(),
            OwnerThen::Unlock => {
                self.set_notice(tr("tous les droits sont accordés : refaites l'action").into());
            }
        }
        window.request_redraw();
    }

    /// Chiffre le document comme demandé, puis l'enregistre.
    ///
    /// Le chiffrement ne vaut que sur le fichier : il faut donc réécrire le
    /// document **en entier**, et non y ajouter une mise à jour.
    pub(super) fn protect_with(&mut self, req: &Request, window: &mut dyn WindowHandle) {
        let Some(l) = &mut self.loaded else { return };
        if let Err(e) = l
            .doc
            .protect(req.user.as_bytes(), req.owner.as_bytes(), req.permissions)
        {
            self.alert(tr("Protection impossible"), &format!("{e}"));
            return;
        }
        // Le mot de passe retenu est celui qui donne **tous** les droits :
        // l'annulation rouvre le fichier avec lui et rejoue l'historique, que
        // de simples permissions pourraient refuser.
        let full = if req.owner.is_empty() {
            &req.user
        } else {
            &req.owner
        };
        l.password = Some(full.as_bytes().to_vec());
        l.modified = true;
        // `full_save` reste vrai jusqu'à la prochaine ouverture : `l.doc`
        // lit toujours l'ancien fichier, en clair, et une mise à jour
        // incrémentale s'y ajouterait, mêlant deux clés dans un fichier
        // illisible.
        l.full_save = true;
        self.title_dirty = true;
        self.set_notice(
            if req.user.is_empty() {
                tr("document protégé : ouverture libre, permissions restreintes")
            } else {
                tr("document protégé : le mot de passe sera demandé à l'ouverture")
            }
            .into(),
        );
        // Un chiffrement qui reste en mémoire ne protège rien : on écrit.
        self.save(false, window);
    }

    /// Retire la protection d'un document chiffré (enregistrer ensuite).
    pub(super) fn remove_protection(&mut self) {
        let Some(l) = &mut self.loaded else { return };
        if !l.doc.is_encrypted() {
            self.set_notice(tr("ce document n'est pas protégé").into());
            return;
        }
        if !l.doc.can_change_protection() {
            self.ask_owner_password(OwnerThen::Unprotect);
            return;
        }
        if let Err(e) = l.doc.unprotect() {
            self.set_notice(format!("retrait impossible : {e}"));
            return;
        }
        // Le mot de passe reste retenu : tant que rien n'est enregistré, le
        // fichier sur le disque est chiffré, et l'annulation le rouvre.
        l.modified = true;
        l.full_save = true;
        self.title_dirty = true;
        self.set_notice(tr("protection retirée : enregistrez pour l'appliquer").into());
    }

    /// Donne l'événement à la fenêtre « Protéger », si elle est ouverte.
    /// Elle prend tout le clavier et la souris ; le reste (réveil,
    /// redimensionnement, peinture) suit son cours.
    pub(super) fn protect_event(&mut self, event: &Event, window: &mut dyn WindowHandle) -> bool {
        let Some(dialog) = &mut self.protect else {
            return false;
        };
        let action = match *event {
            Event::Key(key, m) => dialog.key(key, m.shift),
            // Ctrl+V colle dans le champ qui a le focus ; les autres
            // raccourcis n'atteignent pas le document sous la fenêtre.
            Event::Char(c, m) if m.ctrl => {
                if matches!(c, 'v' | 'V' | '\u{16}') {
                    let text = window.clipboard_text().unwrap_or_default();
                    dialog.paste(&text)
                } else {
                    Action::None
                }
            }
            Event::Char(c, _) => dialog.char(c),
            Event::MouseDown {
                button: MouseButton::Left,
                x,
                y,
                ..
            } => dialog.mouse_down(x, y),
            Event::MouseUp { x, y, .. } => dialog.mouse_up(x, y),
            Event::MouseMove { x, y, .. } => {
                if dialog.mouse_move(x, y) {
                    window.request_redraw();
                }
                window.set_cursor(match dialog.target_at(x, y) {
                    Some(crate::ui::protect::Target::Field(_)) => Cursor::IBeam,
                    Some(_) => Cursor::Hand,
                    None => Cursor::Arrow,
                });
                Action::None
            }
            // Le reste de la souris, et un fichier déposé (il ouvrirait un
            // autre document sous la fenêtre), s'arrêtent là.
            Event::MouseDown { .. }
            | Event::Nav { .. }
            | Event::Wheel { .. }
            | Event::FilesDropped { .. } => Action::None,
            _ => return false,
        };
        match action {
            Action::None => {}
            Action::Redraw => window.request_redraw(),
            Action::Cancel => {
                self.protect = None;
                window.request_redraw();
            }
            Action::Apply(request) => {
                self.protect = None;
                self.protect_with(&request, window);
                window.request_redraw();
            }
        }
        true
    }

    /// Peint la fenêtre « Protéger », par-dessus la page.
    pub(super) fn paint_protect(&mut self, frame: &mut Frame<'_>) {
        let (theme, dpi) = (self.theme, self.dpi_scale as f32);
        let (Some(dialog), Some(text)) = (self.protect.as_mut(), self.text.as_mut()) else {
            return;
        };
        dialog.paint(frame, text, &theme, dpi);
    }

    /// Niveau d'impression permis ; `None` quand l'impression est
    /// interdite, après l'avoir dit.
    pub(super) fn print_level_or_refuse(&mut self) -> Option<PrintLevel> {
        let level = self.rights().print_level();
        if level == PrintLevel::None {
            self.refuse(
                tr("Impression interdite"),
                tr("Les permissions de ce document interdisent l'impression. Le mot de passe des permissions lève cette restriction."),
            );
            return None;
        }
        Some(level)
    }

    /// Copie la sélection dans le presse-papiers, si les permissions le
    /// permettent.
    pub(super) fn copy_selection(&mut self, window: &mut dyn WindowHandle) {
        let text = self.selected_text();
        if text.is_empty() {
            return;
        }
        if !self.rights().copy {
            self.set_notice(tr("copie interdite par les permissions du document").into());
            return;
        }
        window.set_clipboard_text(&text);
    }
}
