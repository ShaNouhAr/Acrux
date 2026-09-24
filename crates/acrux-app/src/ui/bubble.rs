//! Bulle d'un commentaire : ce qu'une note dit, qui l'a écrite et quand,
//! son statut, les réponses qu'elle a reçues, et de quoi y répondre.
//!
//! C'est la « fenêtre contextuelle » d'Acrobat, posée à côté de
//! l'annotation, par-dessus la page. On l'ouvre d'un clic sur une note,
//! d'un double-clic sur un surlignage ou une forme, ou par « Répondre » ;
//! Échap ou un clic ailleurs la referment. La réponse se tape dans la bulle
//! même, et Entrée la publie : elle rejoint le fil sans qu'on ait quitté la
//! page des yeux.
//!
//! La bulle ne connaît pas le document : elle reçoit le fil à dessiner
//! ([`Thread`]) et rend ce qu'on lui demande ([`BubbleAction`]). Le
//! visualiseur en fait une modification du document, qui s'annule comme les
//! autres.

// Coordonnées d'écran entières, mesures de texte fractionnaires.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use acrux_graphics::Rasterizer;

use crate::platform::{Frame, Key};
use crate::ui::icons::{self, Icon};
use crate::ui::input::{InputAction, TextInput};
use crate::ui::lang::tr;
use crate::ui::paint::{button, round_rect, round_rect_outline, shadow, ButtonLook};
use crate::ui::panel::ReplyRow;
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Rectangle `(x, y, largeur, hauteur)`.
pub type Rect = (i32, i32, i32, i32);

/// Largeur de la bulle, en pixels logiques.
const WIDTH: f32 = 290.0;
/// Marge intérieure.
const PAD: f32 = 12.0;
/// Retrait d'une réponse par niveau.
const INDENT: f32 = 14.0;
/// Écart entre la bulle et l'annotation.
const GAP: f32 = 10.0;

/// Le fil à montrer.
#[derive(Debug, Clone, Copy)]
pub struct Thread<'a> {
    /// Type du commentaire, déjà traduit.
    pub kind: &'a str,
    /// Auteur.
    pub author: &'a str,
    /// Date lisible.
    pub date: &'a str,
    /// Texte.
    pub contents: &'a str,
    /// Pastille de couleur.
    pub color: Option<(u8, u8, u8)>,
    /// Statut, déjà traduit.
    pub status: Option<&'a str>,
    /// Case « coché ».
    pub marked: bool,
    /// Réponses, dans l'ordre du fil.
    pub replies: &'a [ReplyRow],
}

/// Ce que la bulle demande.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BubbleAction {
    /// Rien.
    None,
    /// Le champ de réponse s'ouvre.
    StartReply,
    /// Publier cette réponse.
    Publish(String),
    /// Refermer la bulle.
    Close,
}

/// Ce qui se clique dans la bulle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Close,
    Reply,
    Publish,
    Field,
}

/// La bulle ouverte.
#[derive(Debug, Clone)]
pub struct NoteBubble {
    /// Page du commentaire.
    pub page: usize,
    /// Rang du commentaire dans `/Annots`.
    pub index: usize,
    /// Réponse en cours de frappe.
    pub reply: TextInput,
    /// Le champ de réponse est ouvert.
    pub typing: bool,
    /// Défilement du fil, en pixels.
    scroll: i32,
    /// Hauteur du fil au dernier dessin, et de sa fenêtre.
    content_h: i32,
    view_h: i32,
    /// La carte au dernier dessin.
    card: Rect,
    hits: Vec<(Rect, Hit)>,
    hover: Option<Hit>,
}

fn inside(r: Rect, x: i32, y: i32) -> bool {
    x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
}

/// Coupe un texte en lignes d'au plus `width` pixels, aux espaces ; un mot
/// plus long que la ligne est coupé où il déborde. Les sauts de ligne du
/// texte sont gardés.
pub fn wrap(text: &str, width: f32, measure: &mut dyn FnMut(&str) -> f32) -> Vec<String> {
    wrap_at_most(text, width, measure, usize::MAX)
}

/// Comme [`wrap`], mais s'arrête dès qu'il y a plus de `max` lignes : le
/// résultat en a alors au moins `max + 1`, ce qui suffit à savoir qu'il
/// faut couper. Le panneau des commentaires, qui ne montre que deux lignes
/// de chaque texte et se redessine à chaque survol, ne mesure ainsi pas
/// tout un long commentaire, ni tous ceux d'un document qui en compte des
/// centaines.
pub fn wrap_at_most(
    text: &str,
    width: f32,
    measure: &mut dyn FnMut(&str) -> f32,
    max: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    // Assez de lignes, dont une qui dit quelque chose au-delà de `max` : les
    // lignes vides de queue, elles, sont retirées à la fin.
    let enough =
        |out: &[String]| out.len() > max && out[max..].iter().any(|l: &String| !l.is_empty());
    'text: for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if measure(&candidate) <= width {
                line = candidate;
                continue;
            }
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
                if enough(&out) {
                    break 'text;
                }
            }
            // Un mot seul trop long : coupé lettre à lettre.
            let mut piece = String::new();
            for c in word.chars() {
                piece.push(c);
                if measure(&piece) > width && piece.chars().count() > 1 {
                    piece.pop();
                    out.push(std::mem::take(&mut piece));
                    if enough(&out) {
                        break 'text;
                    }
                    piece.push(c);
                }
            }
            line = piece;
        }
        out.push(line);
        if enough(&out) {
            break;
        }
    }
    // Pas de lignes vides en queue : un texte qui finit par un saut de
    // ligne ne grandit pas la bulle pour rien.
    while out.len() > 1 && out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

/// Place d'une carte de `size` à côté de `anchor` : à droite, à gauche si
/// la place manque, et toujours dans `bounds` (largeur, hauteur).
#[must_use]
pub fn place(anchor: Rect, size: (i32, i32), bounds: (i32, i32), gap: i32) -> (i32, i32) {
    let (w, h) = size;
    let right = anchor.0 + anchor.2 + gap;
    let x = if right + w <= bounds.0 {
        right
    } else if anchor.0 - gap - w >= 0 {
        anchor.0 - gap - w
    } else {
        (bounds.0 - w).max(0)
    };
    let y = anchor.1.clamp(0, (bounds.1 - h).max(0));
    (x, y)
}

impl NoteBubble {
    /// Bulle du commentaire `(page, index)` ; `typing` ouvre tout de suite
    /// le champ de réponse.
    #[must_use]
    pub fn new(page: usize, index: usize, typing: bool) -> Self {
        let mut reply = TextInput::new(tr("Votre réponse"));
        reply.focused = typing;
        Self {
            page,
            index,
            reply,
            typing,
            scroll: 0,
            content_h: 0,
            view_h: 0,
            card: (0, 0, 0, 0),
            hits: Vec::new(),
            hover: None,
        }
    }

    /// Vrai si le point est sur la bulle (au dernier dessin).
    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        inside(self.card, x, y)
    }

    fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        self.hits
            .iter()
            .rev()
            .find(|(r, _)| inside(*r, x, y))
            .map(|(_, h)| *h)
    }

    /// Clic : `None` s'il tombe hors de la bulle.
    pub fn mouse_down(&mut self, x: i32, y: i32) -> Option<BubbleAction> {
        if !self.contains(x, y) {
            return None;
        }
        Some(match self.hit(x, y) {
            Some(Hit::Close) => BubbleAction::Close,
            Some(Hit::Reply | Hit::Field) => {
                self.typing = true;
                self.reply.focused = true;
                BubbleAction::StartReply
            }
            Some(Hit::Publish) => self.publish(),
            None => BubbleAction::None,
        })
    }

    /// Survol ; vrai si l'image change.
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool {
        let hover = self.hit(x, y);
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    /// Molette sur la bulle : le fil défile.
    pub fn wheel(&mut self, delta: f32) {
        let max = (self.content_h - self.view_h).max(0);
        self.scroll = (self.scroll - (delta * 60.0) as i32).clamp(0, max);
    }

    /// La réponse tapée, si elle dit quelque chose ; le champ se vide.
    fn publish(&mut self) -> BubbleAction {
        let text = self.reply.value.trim().to_string();
        if text.is_empty() {
            return BubbleAction::None;
        }
        self.reply.clear();
        self.typing = false;
        self.reply.focused = false;
        BubbleAction::Publish(text)
    }

    /// Referme le champ de réponse, sans rien publier.
    fn cancel(&mut self) {
        self.reply.clear();
        self.typing = false;
        self.reply.focused = false;
    }

    /// Touche pendant la frappe d'une réponse : Entrée publie, Échap
    /// referme le champ (la bulle, s'il était vide).
    pub fn key(&mut self, key: Key, shift: bool) -> BubbleAction {
        match self.reply.key(key, shift) {
            InputAction::Submit => self.publish(),
            InputAction::Cancel => {
                if self.reply.value.is_empty() {
                    BubbleAction::Close
                } else {
                    self.cancel();
                    BubbleAction::None
                }
            }
            InputAction::Changed | InputAction::None => BubbleAction::None,
        }
    }

    /// Caractère tapé dans la réponse.
    pub fn char(&mut self, c: char) {
        if self.typing {
            let _ = self.reply.insert_char(c);
        }
    }

    /// Texte collé dans la réponse.
    pub fn paste(&mut self, text: &str) {
        if self.typing {
            let _ = self.reply.paste(text);
        }
    }

    /// Dessine la bulle à côté de `anchor` (le rectangle de l'annotation
    /// dans `frame`).
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)] // une carte, de haut en bas
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        raster: &mut Rasterizer,
        theme: &Theme,
        dpi: f32,
        anchor: Rect,
        thread: &Thread<'_>,
    ) {
        let s = |v: f32| (v * dpi).round() as i32;
        let size = theme.font_size * dpi;
        let small = size * 0.9;
        let line_h = text.line_height(size).ceil() as i32;
        let small_h = text.line_height(small).ceil() as i32;
        let (bw, bh) = (frame.width as i32, frame.height as i32);
        let width = s(WIDTH).min(bw - s(8.0)).max(s(160.0));
        let pad = s(PAD);
        let inner = (width - 2 * pad) as f32;
        let mut measure = |t: &str| text.measure(size, t);
        let body_lines = if thread.contents.trim().is_empty() {
            vec![tr("(sans texte)").to_string()]
        } else {
            wrap(thread.contents, inner, &mut measure)
        };
        let mut reply_lines: Vec<(usize, bool, String)> = Vec::new();
        for r in thread.replies {
            let indent = s(INDENT) * r.depth.min(6) as i32;
            let room = inner - indent as f32;
            let who = match (&r.author, r.date.is_empty()) {
                (Some(a), false) => format!("{a} · {}", r.date),
                (Some(a), true) => a.clone(),
                (None, false) => r.date.clone(),
                (None, true) => String::new(),
            };
            reply_lines.push((r.depth, true, who));
            let mut small_measure = |t: &str| text.measure(size, t);
            for l in wrap(&r.contents, room, &mut small_measure) {
                reply_lines.push((r.depth, false, l));
            }
        }
        // Hauteurs : en-tête, texte, réponses, pied (bouton ou champ).
        let head_h = line_h + small_h + s(6.0);
        let text_h = body_lines.len() as i32 * line_h;
        let replies_h = if reply_lines.is_empty() {
            0
        } else {
            s(10.0)
                + reply_lines
                    .iter()
                    .map(|(_, head, _)| if *head { small_h + s(4.0) } else { line_h })
                    .sum::<i32>()
        };
        let foot_h = s(32.0) + s(10.0);
        let content_h = text_h + replies_h;
        let max_h = (bh - s(16.0)).max(s(120.0));
        let view_h = content_h.min(max_h - pad * 2 - head_h - foot_h).max(line_h);
        let height = pad * 2 + head_h + view_h + foot_h;
        self.content_h = content_h;
        self.view_h = view_h;
        self.scroll = self.scroll.clamp(0, (content_h - view_h).max(0));
        let (x, y) = place(anchor, (width, height), (bw, bh), s(GAP));
        self.card = (x, y, width, height);
        self.hits.clear();
        let radius = 10.0 * dpi;
        shadow(
            frame,
            x,
            y + s(2.0),
            width,
            height,
            radius,
            12.0 * dpi,
            0.35,
        );
        round_rect(frame, x, y, width, height, radius, theme.bar);
        round_rect_outline(
            frame,
            x,
            y,
            width,
            height,
            radius,
            dpi.max(1.0),
            theme.separator,
        );
        // En-tête : pastille, auteur, fermer ; puis type, date et statut.
        let mut cx = x + pad;
        let top = y + pad;
        let base1 = top as f32 + text.ascent(size);
        if let Some(c) = thread.color {
            let chip = s(10.0);
            round_rect(
                frame,
                cx,
                top + (line_h - chip) / 2,
                chip,
                chip,
                chip as f32 / 2.0,
                c,
            );
            cx += chip + s(8.0);
        }
        let close = s(22.0);
        let close_r = (x + width - pad - close + s(4.0), top - s(3.0), close, close);
        let author = if thread.author.is_empty() {
            tr("Auteur inconnu")
        } else {
            thread.author
        };
        text.draw_clipped(
            frame,
            cx as f32,
            base1,
            size,
            author,
            theme.text,
            (close_r.0 - cx - s(4.0)) as f32,
        );
        if self.hover == Some(Hit::Close) {
            round_rect(
                frame,
                close_r.0,
                close_r.1,
                close,
                close,
                6.0 * dpi,
                theme.hover,
            );
        }
        let icon = s(14.0);
        icons::draw(
            frame,
            raster,
            Icon::Close,
            close_r.0 + (close - icon) / 2,
            close_r.1 + (close - icon) / 2,
            icon as f32,
            theme.text_dim,
        );
        self.hits.push((close_r, Hit::Close));
        let base2 = (top + line_h) as f32 + text.ascent(small);
        let meta = if thread.date.is_empty() {
            thread.kind.to_string()
        } else {
            format!("{} · {}", thread.kind, thread.date)
        };
        let mut meta_room = inner;
        let mut right = x + width - pad;
        if let Some(status) = thread.status {
            let sw = text.measure(small, status).ceil() as i32 + s(12.0);
            let sx = right - sw;
            right = sx - s(6.0);
            round_rect(
                frame,
                sx,
                top + line_h + s(1.0),
                sw,
                small_h,
                small_h as f32 / 2.0,
                theme.accent,
            );
            text.draw(
                frame,
                (sx + s(6.0)) as f32,
                base2,
                small,
                status,
                (255, 255, 255),
            );
            meta_room -= (sw + s(8.0)) as f32;
        }
        if thread.marked {
            // La case cochée : une coche d'accent, à côté du statut.
            let check = small_h;
            icons::draw(
                frame,
                raster,
                Icon::Check,
                right - check,
                top + line_h + s(1.0),
                check as f32,
                theme.accent,
            );
            meta_room -= (check + s(6.0)) as f32;
        }
        text.draw_clipped(
            frame,
            (x + pad) as f32,
            base2,
            small,
            &meta,
            theme.text_dim,
            meta_room,
        );
        // Le fil, dans sa fenêtre qui défile.
        let list_top = top + head_h;
        let mut clip = frame.sub(x + pad, list_top, (width - 2 * pad) as u32, view_h as u32);
        let mut yy = -self.scroll;
        for l in &body_lines {
            text.draw(
                &mut clip,
                0.0,
                yy as f32 + text.ascent(size),
                size,
                l,
                theme.text,
            );
            yy += line_h;
        }
        if !reply_lines.is_empty() {
            yy += s(4.0);
            clip.fill_rect(
                0,
                yy,
                width - 2 * pad,
                s(1.0).max(1),
                theme.separator.0,
                theme.separator.1,
                theme.separator.2,
            );
            yy += s(6.0);
            for (depth, head, l) in &reply_lines {
                let indent = s(INDENT) * (*depth).min(6) as i32;
                if *head {
                    // Un filet d'accent marque la réponse, décalé de son
                    // niveau.
                    clip.fill_rect(
                        indent - s(8.0),
                        yy + s(2.0),
                        s(2.0).max(1),
                        small_h,
                        theme.accent.0,
                        theme.accent.1,
                        theme.accent.2,
                    );
                    text.draw(
                        &mut clip,
                        indent as f32,
                        yy as f32 + s(2.0) as f32 + text.ascent(small),
                        small,
                        l,
                        theme.text_dim,
                    );
                    yy += small_h + s(4.0);
                } else {
                    text.draw(
                        &mut clip,
                        indent as f32,
                        yy as f32 + text.ascent(size),
                        size,
                        l,
                        theme.text,
                    );
                    yy += line_h;
                }
            }
        }
        // Le pied : « Répondre », ou le champ et « Publier ».
        let foot_y = list_top + view_h + s(10.0);
        let bh_ = s(32.0);
        if self.typing {
            let publish_w = text.measure(size, tr("Publier")).ceil() as i32 + s(24.0);
            let field_w = width - 2 * pad - publish_w - s(8.0);
            self.reply
                .draw(frame, text, theme, dpi, x + pad, foot_y, field_w, bh_);
            self.hits
                .push(((x + pad, foot_y, field_w, bh_), Hit::Field));
            let px = x + pad + field_w + s(8.0);
            let look = ButtonLook {
                primary: true,
                hovered: self.hover == Some(Hit::Publish),
                disabled: self.reply.value.trim().is_empty(),
                ..ButtonLook::default()
            };
            let fg = button(frame, px, foot_y, publish_w, bh_, dpi, theme, look);
            let lw = text.measure(size, tr("Publier"));
            text.draw(
                frame,
                px as f32 + (publish_w as f32 - lw) / 2.0,
                (foot_y + bh_ / 2) as f32 + text.ascent(size) / 2.0 - 1.0,
                size,
                tr("Publier"),
                fg,
            );
            self.hits.push(((px, foot_y, publish_w, bh_), Hit::Publish));
        } else {
            let label = tr("Répondre");
            let bw_ = text.measure(size, label).ceil() as i32 + s(40.0);
            let look = ButtonLook {
                hovered: self.hover == Some(Hit::Reply),
                ..ButtonLook::default()
            };
            let fg = button(frame, x + pad, foot_y, bw_, bh_, dpi, theme, look);
            let icon = s(16.0);
            icons::draw(
                frame,
                raster,
                Icon::Reply,
                x + pad + s(8.0),
                foot_y + (bh_ - icon) / 2,
                icon as f32,
                fg,
            );
            text.draw(
                frame,
                (x + pad + s(30.0)) as f32,
                (foot_y + bh_ / 2) as f32 + text.ascent(size) / 2.0 - 1.0,
                size,
                label,
                fg,
            );
            self.hits.push(((x + pad, foot_y, bw_, bh_), Hit::Reply));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mesure d'essai : dix pixels par caractère.
    fn tens(t: &str) -> f32 {
        t.chars().count() as f32 * 10.0
    }

    #[test]
    fn le_texte_se_coupe_aux_espaces() {
        let lines = wrap("un deux trois quatre", 90.0, &mut tens);
        assert_eq!(lines, ["un deux", "trois", "quatre"]);
        // Un mot trop long se coupe où il déborde.
        let lines = wrap("anticonstitutionnellement", 100.0, &mut tens);
        assert_eq!(lines[0], "anticonsti");
        assert!(lines.iter().all(|l| tens(l) <= 100.0));
        // Les sauts de ligne restent, pas ceux de la fin.
        assert_eq!(wrap("a\nb\n\n", 100.0, &mut tens), ["a", "b"]);
        assert_eq!(wrap("", 100.0, &mut tens), [""]);
    }

    /// Le panneau ne veut que deux lignes : la coupe s'arrête à la
    /// troisième, sans mesurer le reste, et des lignes vides en queue ne
    /// font pas croire qu'il y a une suite.
    #[test]
    fn la_coupe_s_arrete_au_nombre_de_lignes_voulu() {
        let long = "mot ".repeat(10_000);
        let mut calls = 0;
        let mut counted = |t: &str| {
            calls += 1;
            tens(t)
        };
        let lines = wrap_at_most(&long, 90.0, &mut counted, 2);
        assert_eq!(lines, ["mot mot", "mot mot", "mot mot"]);
        assert!(calls < 50, "{calls} mesures pour trois lignes");
        assert_eq!(wrap_at_most("a\nb\n\n\n", 100.0, &mut tens, 2), ["a", "b"]);
        assert_eq!(
            wrap_at_most("a\nb\n\nc", 100.0, &mut tens, 2),
            ["a", "b", "", "c"]
        );
        assert_eq!(
            wrap_at_most("un deux trois quatre", 90.0, &mut tens, usize::MAX),
            wrap("un deux trois quatre", 90.0, &mut tens)
        );
    }

    #[test]
    fn la_bulle_se_pose_a_cote_et_reste_dans_la_vue() {
        // Place à droite.
        assert_eq!(
            place((100, 50, 20, 20), (200, 100), (800, 600), 10),
            (130, 50)
        );
        // Pas de place à droite : à gauche.
        assert_eq!(
            place((700, 50, 20, 20), (200, 100), (800, 600), 10),
            (490, 50)
        );
        // Trop bas : remontée dans la vue.
        assert_eq!(place((100, 580, 20, 20), (200, 100), (800, 600), 10).1, 500);
    }

    #[test]
    fn entree_publie_echap_referme_le_vide_ne_publie_rien() {
        let mut b = NoteBubble::new(0, 3, true);
        assert_eq!(
            b.key(Key::Enter, false),
            BubbleAction::None,
            "rien à publier"
        );
        for c in "Vu, je corrige".chars() {
            b.char(c);
        }
        assert_eq!(
            b.key(Key::Enter, false),
            BubbleAction::Publish("Vu, je corrige".into())
        );
        assert!(!b.typing && b.reply.value.is_empty());
        // Échap sur un champ vide referme la bulle.
        b.typing = true;
        assert_eq!(b.key(Key::Escape, false), BubbleAction::Close);
        // Sur un champ rempli, il ne fait que le vider.
        b.char('x');
        assert_eq!(b.key(Key::Escape, false), BubbleAction::None);
        assert!(!b.typing);
    }
}
