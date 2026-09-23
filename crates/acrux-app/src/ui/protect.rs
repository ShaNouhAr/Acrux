//! Fenêtre « Protéger par mot de passe » : les deux mots de passe et les
//! permissions, comme dans la boîte « Sécurité par mot de passe » d'Acrobat.
//!
//! Deux mots de passe, pour deux usages : celui **d'ouverture**, facultatif,
//! sans lequel le document ne s'ouvre pas ; celui **des permissions**, qui
//! lève les restrictions et permet seul de changer la protection. Jusqu'à la
//! 0.22, l'application employait le même pour les deux — si bien que
//! quiconque ouvrait le document avait tous les droits, et que des
//! permissions n'avaient aucun sens.
//!
//! Chaque mot de passe se tape deux fois (masqué, une faute de frappe
//! enfermerait le document pour de bon), et une jauge dit sa force pendant
//! qu'on le tape.
//!
//! L'état est pur : la fenêtre reçoit des touches et des clics et rend une
//! [`Action`] ; c'est le visualiseur qui chiffre et enregistre. Les tests
//! peuvent donc tout éprouver sans rien dessiner.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    // La peinture est une mise en page lue de haut en bas : la découper en
    // morceaux ferait perdre le fil des ordonnées.
    clippy::too_many_lines
)]

use acrux_document::protect::{password_strength, password_warnings, Permissions, PrintLevel};

use crate::platform::{Frame, Key};
use crate::ui::controls::{self, CheckLook, Segment, SegmentItem};
use crate::ui::input::{InputAction, TextInput};
use crate::ui::lang::tr;
use crate::ui::modal::{self, Appear};
use crate::ui::paint::{button, focus_ring, round_rect_alpha, veil, ButtonLook, Rgb};
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Deux saisies d'un même mot de passe qui ne correspondent pas.
const MISMATCH: &str = "les deux saisies diffèrent";

/// Champ du mot de passe d'ouverture.
const OPEN: usize = 0;
/// Sa confirmation.
const OPEN_CONFIRM: usize = 1;
/// Champ du mot de passe des permissions.
const OWNER: usize = 2;
/// Sa confirmation.
const OWNER_CONFIRM: usize = 3;

/// Une permission de la liste à cocher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Perm {
    /// Modifier le contenu.
    Modify,
    /// Copier le texte et les images.
    Copy,
    /// Commenter (comprend remplir les formulaires).
    Annotate,
    /// Remplir les formulaires.
    FillForms,
    /// Extraction pour l'accessibilité (comprise dans la copie).
    Accessibility,
    /// Assembler : pages, signets.
    Assemble,
}

impl Perm {
    /// Les six cases, dans l'ordre de la fenêtre.
    const ALL: [Perm; 6] = [
        Perm::Modify,
        Perm::Copy,
        Perm::Annotate,
        Perm::FillForms,
        Perm::Accessibility,
        Perm::Assemble,
    ];

    /// Libellé.
    fn label(self) -> &'static str {
        match self {
            Perm::Modify => tr("Modifier le contenu"),
            Perm::Copy => tr("Copier le texte et les images"),
            Perm::Annotate => tr("Commenter"),
            Perm::FillForms => tr("Remplir les formulaires"),
            Perm::Accessibility => tr("Extraction pour l'accessibilité"),
            Perm::Assemble => tr("Assembler (pages, signets)"),
        }
    }
}

/// Ce que vise un clic ou le focus clavier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// La case « exiger un mot de passe pour ouvrir ».
    RequireOpen,
    /// Un des quatre champs.
    Field(usize),
    /// Un niveau d'impression ; au clavier, le contrôle entier.
    Print(PrintLevel),
    /// Une permission.
    Check(Perm),
    /// « Annuler ».
    Cancel,
    /// « Protéger ».
    Apply,
}

impl Target {
    /// Même arrêt de tabulation : les trois niveaux d'impression n'en font
    /// qu'un, que les flèches parcourent.
    fn same_stop(self, other: Target) -> bool {
        match (self, other) {
            (Target::Print(_), Target::Print(_)) => true,
            (a, b) => a == b,
        }
    }
}

/// Protection demandée, prête à appliquer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Mot de passe d'ouverture (vide : ouverture libre).
    pub user: String,
    /// Mot de passe des permissions (vide : le même que celui d'ouverture,
    /// ce qui n'est permis que sans restriction).
    pub owner: String,
    /// Permissions accordées à qui n'a que le mot de passe d'ouverture.
    pub permissions: Permissions,
}

/// Ce que la fenêtre demande au visualiseur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Rien.
    None,
    /// Redessiner.
    Redraw,
    /// Fermer sans rien faire.
    Cancel,
    /// Protéger ainsi.
    Apply(Request),
}

/// La fenêtre « Protéger par mot de passe ».
#[derive(Debug)]
// Une case par permission, comme la table 22 de la norme : les regrouper
// dans une structure ne ferait que déplacer les mêmes booléens.
#[allow(clippy::struct_excessive_bools)]
pub struct ProtectDialog {
    /// Un mot de passe est demandé à l'ouverture.
    pub require_open: bool,
    /// Ouverture, confirmation, permissions, confirmation.
    fields: [TextInput; 4],
    /// Niveau d'impression permis.
    pub print: PrintLevel,
    /// Modifier le contenu.
    pub modify: bool,
    /// Copier.
    pub copy: bool,
    /// Commenter.
    pub annotate: bool,
    /// Remplir les formulaires (choix propre, quand « commenter » est
    /// décoché).
    pub fill_forms: bool,
    /// Extraction pour l'accessibilité (choix propre, quand « copier » est
    /// décoché).
    pub accessibility: bool,
    /// Assembler.
    pub assemble: bool,
    /// Élément qui a le focus clavier.
    focus: Target,
    /// Élément survolé.
    hover: Option<Target>,
    /// Élément enfoncé : il n'agira qu'au relâchement, pointeur dessus.
    pressed: Option<Target>,
    /// Zones cliquables, relevées au dernier dessin.
    targets: Vec<(i32, i32, i32, i32, Target)>,
    /// Erreur de la dernière validation, en français (traduite au dessin).
    error: Option<&'static str>,
    /// Le document était déjà protégé : on change sa protection.
    changing: bool,
    /// Apparition, comme toutes les cartes.
    appear: Appear,
}

impl Default for ProtectDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtectDialog {
    /// Fenêtre d'un document en clair : mot de passe d'ouverture demandé,
    /// tout permis, impression en haute résolution.
    #[must_use]
    pub fn new() -> Self {
        let mut fields: [TextInput; 4] = Default::default();
        for f in &mut fields {
            f.masked = true;
        }
        let mut dialog = Self {
            require_open: true,
            fields,
            print: PrintLevel::High,
            modify: true,
            copy: true,
            annotate: true,
            fill_forms: true,
            accessibility: true,
            assemble: true,
            focus: Target::Field(OPEN),
            hover: None,
            pressed: None,
            targets: Vec::new(),
            error: None,
            changing: false,
            appear: Appear::new(),
        };
        dialog.sync_focus();
        dialog
    }

    /// Fenêtre d'un document déjà protégé (ouvert avec le mot de passe des
    /// permissions) : les cases reprennent ses permissions, les mots de
    /// passe sont à ressaisir.
    #[must_use]
    pub fn from_current(p: Permissions, opens_freely: bool) -> Self {
        let mut dialog = Self {
            require_open: !opens_freely,
            print: p.print_level(),
            modify: p.modify,
            copy: p.copy,
            annotate: p.annotate,
            fill_forms: p.fill_forms,
            accessibility: p.accessibility,
            assemble: p.assemble,
            changing: true,
            ..Self::new()
        };
        dialog.focus = if dialog.require_open {
            Target::Field(OPEN)
        } else {
            Target::Field(OWNER)
        };
        dialog.sync_focus();
        dialog
    }

    /// Vrai tant que l'apparition a une image à peindre.
    #[must_use]
    pub fn animating(&self) -> bool {
        self.appear.animating()
    }

    /// Titre de la fenêtre, en français (traduit au dessin).
    #[must_use]
    pub fn title(&self) -> &'static str {
        if self.changing {
            "Changer la protection"
        } else {
            "Protéger par mot de passe"
        }
    }

    /// Permissions telles que cochées, rendues cohérentes.
    #[must_use]
    pub fn permissions(&self) -> Permissions {
        let mut p = Permissions {
            print: false,
            modify: self.modify,
            copy: self.copy,
            annotate: self.annotate,
            fill_forms: self.fill_forms,
            accessibility: self.accessibility,
            assemble: self.assemble,
            print_high_quality: false,
        };
        p.set_print(self.print);
        p.normalized()
    }

    /// Une case est-elle cochée (à l'écran) ?
    fn checked(&self, perm: Perm) -> bool {
        let p = self.permissions();
        match perm {
            Perm::Modify => p.modify,
            Perm::Copy => p.copy,
            Perm::Annotate => p.annotate,
            Perm::FillForms => p.fill_forms,
            Perm::Accessibility => p.accessibility,
            Perm::Assemble => p.assemble,
        }
    }

    /// Une case répond-elle ? « Remplir » est compris dans « commenter »,
    /// « accessibilité » dans « copier » : cochées d'office, elles se
    /// grisent tant que l'autre est cochée.
    fn enabled(&self, perm: Perm) -> bool {
        match perm {
            Perm::FillForms => !self.annotate,
            Perm::Accessibility => !self.copy,
            _ => true,
        }
    }

    /// Un champ répond-il ? Ceux de l'ouverture se grisent quand aucun mot
    /// de passe n'est exigé à l'ouverture.
    fn field_enabled(&self, index: usize) -> bool {
        self.require_open || index >= OWNER
    }

    /// Arrêts de tabulation, dans l'ordre de la fenêtre.
    fn stops(&self) -> Vec<Target> {
        let mut out = vec![Target::RequireOpen];
        for i in 0..4 {
            if self.field_enabled(i) {
                out.push(Target::Field(i));
            }
        }
        out.push(Target::Print(self.print));
        for perm in Perm::ALL {
            if self.enabled(perm) {
                out.push(Target::Check(perm));
            }
        }
        // Dans l'ordre où on les voit : « Protéger », puis « Annuler ».
        out.push(Target::Apply);
        out.push(Target::Cancel);
        out
    }

    /// Passe le focus à l'arrêt suivant (ou précédent).
    fn move_focus(&mut self, forward: bool) {
        let stops = self.stops();
        let n = stops.len();
        let at = stops.iter().position(|s| s.same_stop(self.focus));
        let next = match (at, forward) {
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
            (None, _) => 0,
        };
        if let Some(&stop) = stops.get(next) {
            self.focus = stop;
        }
        self.sync_focus();
    }

    /// Le caret ne se montre que dans le champ qui a le focus.
    fn sync_focus(&mut self) {
        for (i, f) in self.fields.iter_mut().enumerate() {
            f.focused = self.focus == Target::Field(i);
        }
    }

    /// Champ qui a le focus, s'il y en a un.
    fn focused_field(&mut self) -> Option<&mut TextInput> {
        match self.focus {
            Target::Field(i) => self.fields.get_mut(i),
            _ => None,
        }
    }

    /// Touche pressée.
    pub fn key(&mut self, key: Key, shift: bool) -> Action {
        match key {
            Key::Escape => Action::Cancel,
            Key::Tab => {
                self.move_focus(!shift);
                Action::Redraw
            }
            Key::Down => {
                self.move_focus(true);
                Action::Redraw
            }
            Key::Up => {
                self.move_focus(false);
                Action::Redraw
            }
            Key::Enter => match self.focus {
                Target::Cancel => Action::Cancel,
                _ => self.submit(),
            },
            // Dans un champ, l'espace est un caractère : il arrive par
            // `char`. Ailleurs, il coche ou presse.
            Key::Space => match self.focus {
                Target::Field(_) => Action::None,
                other => self.press(other),
            },
            Key::Left | Key::Right if matches!(self.focus, Target::Print(_)) => {
                let levels = [PrintLevel::None, PrintLevel::Low, PrintLevel::High];
                let at = levels.iter().position(|l| *l == self.print).unwrap_or(2);
                let next = if key == Key::Right {
                    (at + 1).min(2)
                } else {
                    at.saturating_sub(1)
                };
                self.print = levels[next];
                self.focus = Target::Print(self.print);
                Action::Redraw
            }
            _ => {
                let Some(field) = self.focused_field() else {
                    return Action::None;
                };
                match field.key(key, shift) {
                    InputAction::Changed => {
                        self.error = None;
                        Action::Redraw
                    }
                    InputAction::Submit => self.submit(),
                    InputAction::Cancel => Action::Cancel,
                    // Le caret a pu bouger.
                    InputAction::None => Action::Redraw,
                }
            }
        }
    }

    /// Caractère tapé : il va au champ qui a le focus.
    pub fn char(&mut self, c: char) -> Action {
        let Some(field) = self.focused_field() else {
            return Action::None;
        };
        if field.insert_char(c) == InputAction::Changed {
            self.error = None;
            Action::Redraw
        } else {
            Action::None
        }
    }

    /// Texte collé (Ctrl+V) dans le champ qui a le focus.
    pub fn paste(&mut self, text: &str) -> Action {
        let Some(field) = self.focused_field() else {
            return Action::None;
        };
        if field.paste(text) == InputAction::Changed {
            self.error = None;
            Action::Redraw
        } else {
            Action::None
        }
    }

    /// Élément sous un point, d'après le dernier dessin.
    #[must_use]
    pub fn target_at(&self, x: i32, y: i32) -> Option<Target> {
        self.targets
            .iter()
            .find(|(tx, ty, tw, th, _)| x >= *tx && x < tx + tw && y >= *ty && y < ty + th)
            .map(|t| t.4)
    }

    /// Appui. Un champ prend le focus tout de suite, pour qu'on puisse y
    /// taper ; une case ou un bouton s'enfonce seulement, et n'agira qu'au
    /// relâchement — on se ravise en glissant hors de lui.
    pub fn mouse_down(&mut self, x: i32, y: i32) -> Action {
        self.pressed = None;
        match self.target_at(x, y) {
            Some(target @ Target::Field(_)) => self.press(target),
            Some(target) => {
                self.pressed = Some(target);
                self.hover = Some(target);
                Action::Redraw
            }
            None => Action::None,
        }
    }

    /// Relâchement : l'élément enfoncé agit, si le pointeur est resté dessus.
    pub fn mouse_up(&mut self, x: i32, y: i32) -> Action {
        let Some(pressed) = self.pressed.take() else {
            return Action::None;
        };
        if self.target_at(x, y) == Some(pressed) {
            self.press(pressed)
        } else {
            Action::Redraw
        }
    }

    /// Survol ; rend vrai si l'affichage doit changer.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let over = self.target_at(x, y);
        let changed = over != self.hover;
        self.hover = over;
        changed
    }

    /// Effet d'un élément, cliqué ou pressé au clavier.
    fn press(&mut self, target: Target) -> Action {
        match target {
            Target::RequireOpen => {
                self.require_open = !self.require_open;
                self.focus = Target::RequireOpen;
                self.error = None;
            }
            Target::Field(i) => {
                if self.field_enabled(i) {
                    self.focus = target;
                }
            }
            Target::Print(level) => {
                self.print = level;
                self.focus = Target::Print(level);
            }
            Target::Check(perm) => {
                if self.enabled(perm) {
                    let slot = match perm {
                        Perm::Modify => &mut self.modify,
                        Perm::Copy => &mut self.copy,
                        Perm::Annotate => &mut self.annotate,
                        Perm::FillForms => &mut self.fill_forms,
                        Perm::Accessibility => &mut self.accessibility,
                        Perm::Assemble => &mut self.assemble,
                    };
                    *slot = !*slot;
                    self.focus = target;
                    self.error = None;
                }
            }
            Target::Cancel => return Action::Cancel,
            Target::Apply => return self.submit(),
        }
        self.sync_focus();
        Action::Redraw
    }

    /// Valide et rend la demande, ou affiche l'erreur et met le focus sur
    /// le champ à reprendre.
    fn submit(&mut self) -> Action {
        match self.validate() {
            Ok(request) => Action::Apply(request),
            Err((message, field)) => {
                self.error = Some(message);
                // Une confirmation qui ne correspond pas se retape en
                // entier : on ne voit pas où est la faute.
                if message == MISMATCH {
                    if let Some(f) = self.fields.get_mut(field) {
                        f.clear();
                    }
                }
                self.focus = Target::Field(field);
                self.sync_focus();
                Action::Redraw
            }
        }
    }

    /// La demande, ou le message d'erreur (en français) et le champ à
    /// reprendre.
    fn validate(&self) -> Result<Request, (&'static str, usize)> {
        let value = |i: usize| self.fields.get(i).map_or("", |f| f.value.as_str());
        let (open, owner) = (value(OPEN), value(OWNER));
        if self.require_open && open.is_empty() {
            return Err(("Saisissez le mot de passe d'ouverture", OPEN));
        }
        if self.require_open && open != value(OPEN_CONFIRM) {
            return Err((MISMATCH, OPEN_CONFIRM));
        }
        if owner != value(OWNER_CONFIRM) {
            return Err((MISMATCH, OWNER_CONFIRM));
        }
        let permissions = self.permissions();
        // Sans mot de passe des permissions, celui d'ouverture donnerait
        // tous les droits : les restrictions ne vaudraient rien.
        if !permissions.is_all() && owner.is_empty() {
            return Err((
                "Les restrictions exigent un mot de passe des permissions",
                OWNER,
            ));
        }
        if self.require_open && !owner.is_empty() && owner == open {
            return Err((
                "Le mot de passe des permissions doit différer de celui d'ouverture",
                OWNER,
            ));
        }
        if !self.require_open && owner.is_empty() {
            return Err(("un mot de passe vide ne protège rien", OWNER));
        }
        Ok(Request {
            user: if self.require_open {
                open.to_string()
            } else {
                String::new()
            },
            owner: owner.to_string(),
            permissions,
        })
    }

    /// Dessine la fenêtre au centre du cadre et relève les zones cliquables.
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
    ) {
        let s = |v: f32| (v * dpi).round() as i32;
        let size = theme.font_size * dpi;
        let small = size * 0.92;
        let (fw, fh) = (frame.width as i32, frame.height as i32);
        let width = s(540.0).min(fw - s(24.0)).max(s(320.0));
        let height = s(548.0);
        let pad = s(22.0);
        let inner = width - 2 * pad;
        self.targets.clear();

        // Voile, ombre et carte : celles de toutes les fenêtres
        // (`modal::card`), avec la même apparition.
        let progress = self.appear.progress();
        veil(frame, progress);
        let (x, y) = modal::modal_rect(fw, fh, width, height, dpi, progress);
        // Une fenêtre plus haute que le cadre se colle en haut plutôt que de
        // perdre ses boutons en bas.
        let y = y.min((fh - height).max(s(8.0)));
        let left = x + pad;
        modal::card(frame, theme, dpi, x, y, width, height, progress);

        let mut cy = y + pad;
        let title_size = modal::title_size(theme, dpi);
        let baseline = cy + text.ascent(title_size) as i32;
        modal::title(frame, text, theme, dpi, left, baseline, tr(self.title()));
        cy += s(30.0);
        text.draw_clipped(
            frame,
            left as f32,
            cy as f32 + text.ascent(small),
            small,
            tr("Chiffrement AES-256. Notez vos mots de passe : un mot de passe perdu ne se retrouve pas."),
            theme.text_dim,
            inner as f32,
        );
        cy += s(30.0);

        // Ouverture.
        let hover = self.hover;
        let hovered = |t: Target| hover == Some(t);
        let rect = controls::checkbox(
            frame,
            text,
            theme,
            dpi,
            left,
            cy,
            tr("Exiger un mot de passe pour ouvrir le document"),
            CheckLook {
                on: self.require_open,
                enabled: true,
                hovered: hovered(Target::RequireOpen),
                focused: self.focus == Target::RequireOpen,
            },
        );
        self.targets
            .push((rect.0, rect.1, rect.2, rect.3, Target::RequireOpen));
        cy += s(30.0);
        cy = self.paint_pair(
            frame,
            text,
            theme,
            dpi,
            (left, cy, inner),
            OPEN,
            tr("Mot de passe d'ouverture"),
        );
        cy += s(14.0);

        // Permissions.
        frame.fill_rect(
            left,
            cy,
            inner,
            s(1.0).max(1),
            theme.separator.0,
            theme.separator.1,
            theme.separator.2,
        );
        cy += s(12.0);
        text.draw(
            frame,
            left as f32,
            cy as f32 + text.ascent(size * 1.05),
            size * 1.05,
            tr("Permissions"),
            theme.text,
        );
        cy += s(22.0);
        text.draw_clipped(
            frame,
            left as f32,
            cy as f32 + text.ascent(small),
            small,
            tr("Ce mot de passe lève les restrictions ; lui seul permet de changer la protection."),
            theme.text_dim,
            inner as f32,
        );
        cy += s(24.0);
        cy = self.paint_pair(
            frame,
            text,
            theme,
            dpi,
            (left, cy, inner),
            OWNER,
            tr("Mot de passe des permissions"),
        );
        cy += s(12.0);

        // Impression : trois niveaux dans un contrôle segmenté.
        let seg_h = s(30.0);
        let label_w = s(110.0);
        text.draw(
            frame,
            left as f32,
            (cy + seg_h / 2) as f32 + text.ascent(size) / 2.0,
            size,
            tr("Impression"),
            theme.text,
        );
        let levels = [
            (PrintLevel::None, tr("Non")),
            (PrintLevel::Low, tr("Basse résolution")),
            (PrintLevel::High, tr("Haute résolution")),
        ];
        let items: Vec<SegmentItem<'_>> = levels
            .iter()
            .map(|(level, label)| SegmentItem {
                content: Segment::Label(label),
                on: *level == self.print,
                hovered: hovered(Target::Print(*level)),
            })
            .collect();
        let (seg_w, rects) =
            controls::segmented(frame, text, theme, dpi, left + label_w, cy, seg_h, &items);
        if matches!(self.focus, Target::Print(_)) {
            focus_ring(
                frame,
                left + label_w,
                cy,
                seg_w,
                seg_h,
                7.0 * dpi,
                dpi,
                theme.accent,
            );
        }
        for ((level, _), (rx, ry, rw, rh)) in levels.iter().zip(rects) {
            self.targets.push((rx, ry, rw, rh, Target::Print(*level)));
        }
        cy += seg_h + s(12.0);

        // Les six cases, sur deux colonnes.
        let col = inner / 2;
        for (i, perm) in Perm::ALL.into_iter().enumerate() {
            let cx = left + (i as i32 % 2) * col;
            let ry = cy + (i as i32 / 2) * s(26.0);
            let enabled = self.enabled(perm);
            let rect = controls::checkbox(
                frame,
                text,
                theme,
                dpi,
                cx,
                ry,
                perm.label(),
                CheckLook {
                    on: self.checked(perm),
                    enabled,
                    hovered: hovered(Target::Check(perm)),
                    focused: self.focus == Target::Check(perm),
                },
            );
            if enabled {
                self.targets
                    .push((rect.0, rect.1, rect.2, rect.3, Target::Check(perm)));
            }
        }
        cy += 3 * s(26.0) + s(6.0);

        // L'erreur, puis les boutons.
        if let Some(error) = self.error {
            text.draw_clipped(
                frame,
                left as f32,
                cy as f32 + text.ascent(size),
                size,
                tr(error),
                theme.danger,
                inner as f32,
            );
        }
        // Les boutons, groupés à droite dans l'ordre de Windows : « Protéger »,
        // puis « Annuler » — posés de droite à gauche.
        let bh = s(modal::BUTTON_H);
        let by = y + height - pad - bh;
        let mut bx = x + width - pad;
        for (label, target, primary) in [
            (tr("Annuler"), Target::Cancel, false),
            (tr("Protéger"), Target::Apply, true),
        ] {
            let bw = (text.measure(size, label) as i32 + s(32.0)).max(s(modal::BUTTON_MIN_W));
            bx -= bw;
            let ink = button(
                frame,
                bx,
                by,
                bw,
                bh,
                dpi,
                theme,
                ButtonLook {
                    primary,
                    hovered: hovered(target),
                    focused: self.focus == target,
                    disabled: false,
                    pressed: hovered(target) && self.pressed == Some(target),
                },
            );
            let lw = text.measure(size, label);
            text.draw(
                frame,
                bx as f32 + (bw as f32 - lw) / 2.0,
                by as f32 + f32::midpoint(bh as f32, text.ascent(size)) - 1.0,
                size,
                label,
                ink,
            );
            self.targets.push((bx, by, bw, bh, target));
            bx -= s(8.0);
        }
    }

    /// Un mot de passe et sa confirmation côte à côte, leurs libellés
    /// au-dessus, la jauge de force dessous. Rend l'ordonnée qui suit.
    #[allow(clippy::too_many_arguments)] // cadre, texte, thème, échelle, place, champ, libellé
    fn paint_pair(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
        (left, top, inner): (i32, i32, i32),
        first: usize,
        label: &str,
    ) -> i32 {
        let s = |v: f32| (v * dpi).round() as i32;
        let size = theme.font_size * dpi;
        let small = size * 0.92;
        let gap = s(12.0);
        let w = (inner - gap) / 2;
        let h = s(32.0);
        let enabled = self.field_enabled(first);
        let dim = if enabled {
            theme.text_dim
        } else {
            theme.separator
        };
        let mut cy = top;
        for (i, caption) in [(0, label), (1, tr("Confirmez"))] {
            text.draw_clipped(
                frame,
                (left + i * (w + gap)) as f32,
                cy as f32 + text.ascent(small),
                small,
                caption,
                dim,
                w as f32,
            );
        }
        cy += s(20.0);
        for i in 0..2 {
            let index = first + i;
            let fx = left + i as i32 * (w + gap);
            if let Some(field) = self.fields.get(index) {
                field.draw(frame, text, theme, dpi, fx, cy, w, h);
            }
            if enabled {
                self.targets.push((fx, cy, w, h, Target::Field(index)));
            } else {
                // Grisé : le fond de la carte repasse à moitié par-dessus.
                round_rect_alpha(frame, fx, cy, w, h, 7.0 * dpi, theme.bar, 0.6);
            }
        }
        cy += h + s(8.0);
        // Force du mot de passe, pendant qu'on le tape.
        let value = self.fields.get(first).map_or("", |f| f.value.as_str());
        if enabled && !value.is_empty() {
            let strength = password_strength(value);
            let level = strength.level();
            let meter_w = s(120.0);
            controls::strength_meter(frame, left, cy + s(4.0), meter_w, s(5.0), level, theme);
            let color: Rgb = match level {
                0 | 1 => theme.danger,
                2 => theme.warning,
                _ => theme.accent,
            };
            let lx = (left + meter_w + s(10.0)) as f32;
            // Le libellé se centre sur la jauge par sa hauteur d'x, pas par
            // ses capitales : il paraîtrait sinon trop bas.
            let baseline = cy as f32 + s(6.5) as f32 + text.ascent(small) * 0.36;
            let name = tr(strength.label());
            text.draw(frame, lx, baseline, small, name, color);
            if let Some(warning) = password_warnings(value).first() {
                let wx = lx + text.measure(small, name) + s(12.0) as f32;
                text.draw_clipped(
                    frame,
                    wx,
                    baseline,
                    small,
                    tr(warning),
                    theme.warning,
                    (left + inner) as f32 - wx,
                );
            }
        }
        cy + s(16.0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)] // tests
mod tests {
    use super::*;

    fn typed(d: &mut ProtectDialog, text: &str) {
        for c in text.chars() {
            let _ = d.char(c);
        }
    }

    /// Remplit un champ par le clavier : Tab jusqu'à lui, puis la frappe.
    fn fill(d: &mut ProtectDialog, index: usize, text: &str) {
        d.focus = Target::Field(index);
        d.sync_focus();
        typed(d, text);
    }

    fn error_of(d: &mut ProtectDialog) -> Option<&'static str> {
        match d.key(Key::Enter, false) {
            Action::Apply(_) => None,
            _ => d.error,
        }
    }

    #[test]
    fn validate_reports_each_message_in_order() {
        let mut d = ProtectDialog::new();
        assert_eq!(
            error_of(&mut d),
            Some("Saisissez le mot de passe d'ouverture")
        );
        fill(&mut d, OPEN, "lecture");
        fill(&mut d, OPEN_CONFIRM, "lectur");
        assert_eq!(error_of(&mut d), Some("les deux saisies diffèrent"));
        // La confirmation fautive est vidée, et le focus y revient.
        assert!(d.fields[OPEN_CONFIRM].value.is_empty());
        assert_eq!(d.focus, Target::Field(OPEN_CONFIRM));
        typed(&mut d, "lecture");
        d.copy = false;
        assert_eq!(
            error_of(&mut d),
            Some("Les restrictions exigent un mot de passe des permissions")
        );
        fill(&mut d, OWNER, "lecture");
        fill(&mut d, OWNER_CONFIRM, "lecture");
        assert_eq!(
            error_of(&mut d),
            Some("Le mot de passe des permissions doit différer de celui d'ouverture")
        );
        fill(&mut d, OWNER_CONFIRM, "x");
        assert_eq!(error_of(&mut d), Some("les deux saisies diffèrent"));
        let mut open = ProtectDialog::new();
        open.require_open = false;
        assert_eq!(
            error_of(&mut open),
            Some("un mot de passe vide ne protège rien")
        );
    }

    #[test]
    fn enter_applies_a_valid_form() {
        let mut d = ProtectDialog::new();
        fill(&mut d, OPEN, "lecture");
        fill(&mut d, OPEN_CONFIRM, "lecture");
        fill(&mut d, OWNER, "Chef#2026!xQ");
        fill(&mut d, OWNER_CONFIRM, "Chef#2026!xQ");
        d.print = PrintLevel::Low;
        d.copy = false;
        d.assemble = false;
        let Action::Apply(req) = d.key(Key::Enter, false) else {
            panic!("formulaire valide");
        };
        assert_eq!(req.user, "lecture");
        assert_eq!(req.owner, "Chef#2026!xQ");
        let p = req.permissions;
        assert_eq!(p.print_level(), PrintLevel::Low);
        assert!(!p.copy && !p.assemble && p.modify && p.annotate);
        // Copier décoché : l'accessibilité reprend sa propre case, cochée.
        assert!(p.accessibility);
        // Sans mot de passe d'ouverture, l'utilisateur est vide.
        let mut free = ProtectDialog::new();
        free.require_open = false;
        fill(&mut free, OWNER, "chef");
        fill(&mut free, OWNER_CONFIRM, "chef");
        let Action::Apply(req) = free.key(Key::Enter, false) else {
            panic!("ouverture libre");
        };
        assert!(req.user.is_empty());
    }

    #[test]
    fn tab_cycles_through_every_stop_and_back() {
        let mut d = ProtectDialog::new();
        let start = d.focus;
        let n = d.stops().len();
        // Case d'ouverture, 4 champs, impression, 4 cases (remplir et
        // accessibilité sont grisées), 2 boutons.
        assert_eq!(n, 12);
        let mut seen = Vec::new();
        for _ in 0..n {
            assert_eq!(d.key(Key::Tab, false), Action::Redraw);
            seen.push(d.focus);
        }
        assert_eq!(d.focus, start);
        assert!(seen.contains(&Target::Apply) && seen.contains(&Target::RequireOpen));
        d.key(Key::Tab, true);
        assert_eq!(d.focus, Target::RequireOpen);
        // Sans mot de passe d'ouverture, ses deux champs sortent du tour.
        d.key(Key::Space, false);
        assert!(!d.require_open);
        assert_eq!(d.stops().len(), 10);
    }

    #[test]
    fn linked_boxes_follow_their_master() {
        let mut d = ProtectDialog::new();
        d.annotate = false;
        d.fill_forms = false;
        d.copy = false;
        d.accessibility = false;
        d.focus = Target::Check(Perm::Annotate);
        d.key(Key::Space, false);
        assert!(d.annotate && d.permissions().fill_forms);
        assert!(!d.enabled(Perm::FillForms));
        // Cliquer une case grisée ne change rien.
        assert_eq!(d.press(Target::Check(Perm::FillForms)), Action::Redraw);
        assert!(!d.fill_forms);
        d.press(Target::Check(Perm::Copy));
        assert!(d.permissions().accessibility);
        assert!(d.checked(Perm::Accessibility) && !d.enabled(Perm::Accessibility));
        // Les flèches parcourent les niveaux d'impression.
        d.focus = Target::Print(d.print);
        d.key(Key::Left, false);
        assert_eq!(d.print, PrintLevel::Low);
        d.key(Key::Left, false);
        d.key(Key::Left, false);
        assert_eq!(d.print, PrintLevel::None);
    }

    #[test]
    fn from_current_reflects_the_document() {
        let mut p = Permissions::all();
        p.set_print(PrintLevel::Low);
        p.copy = false;
        p.accessibility = false;
        p.assemble = false;
        let d = ProtectDialog::from_current(p, true);
        assert!(!d.require_open);
        assert_eq!(d.permissions(), p.normalized());
        assert_eq!(d.focus, Target::Field(OWNER));
        assert_eq!(d.title(), "Changer la protection");
        let d = ProtectDialog::from_current(Permissions::all(), false);
        assert!(d.require_open && d.permissions().is_all());
    }

    #[test]
    fn buttons_and_boxes_act_on_release_fields_on_press() {
        let mut d = ProtectDialog::new();
        d.targets = vec![
            (0, 0, 50, 30, Target::Cancel),
            (60, 0, 50, 30, Target::Check(Perm::Modify)),
            (0, 40, 110, 30, Target::Field(OWNER)),
        ];
        // L'appui enfonce, il ne ferme rien.
        assert_eq!(d.mouse_down(10, 10), Action::Redraw);
        assert_eq!(d.mouse_up(10, 12), Action::Cancel);
        // Glissé hors de la case : elle reste cochée.
        assert_eq!(d.mouse_down(70, 10), Action::Redraw);
        assert_eq!(d.mouse_up(10, 60), Action::Redraw);
        assert!(d.modify);
        // Un relâchement sans appui ne fait rien.
        assert_eq!(d.mouse_up(70, 10), Action::None);
        // Un champ prend le focus dès l'appui : on y tape aussitôt.
        d.mouse_down(20, 50);
        assert_eq!(d.focus, Target::Field(OWNER));
    }

    #[test]
    fn paste_escape_and_space_in_fields() {
        let mut d = ProtectDialog::new();
        assert_eq!(d.paste("colle\r\n"), Action::Redraw);
        assert_eq!(d.fields[OPEN].value, "colle");
        // L'espace d'un champ est un caractère, pas une coche.
        assert_eq!(d.key(Key::Space, false), Action::None);
        assert_eq!(d.char(' '), Action::Redraw);
        assert_eq!(d.fields[OPEN].value, "colle ");
        // Hors d'un champ, un caractère ne va nulle part.
        d.focus = Target::Check(Perm::Modify);
        d.sync_focus();
        assert_eq!(d.char('x'), Action::None);
        assert_eq!(d.paste("x"), Action::None);
        assert_eq!(d.key(Key::Escape, false), Action::Cancel);
        d.focus = Target::Cancel;
        assert_eq!(d.key(Key::Enter, false), Action::Cancel);
    }
}
