//! Panneau latéral de navigation : onglet « Vignettes » (une miniature par
//! page, la page courante encadrée), onglet « Signets » (arbre dépliable),
//! onglet « Notes » (les commentaires, leurs réponses et leurs statuts),
//! calques et pièces jointes.
//! Le panneau ne connaît pas le document : il reçoit les tailles et
//! bitmaps des vignettes, la liste aplatie des signets et celle des
//! commentaires, et renvoie des [`PanelAction`].
//!
//! # Les commentaires
//!
//! Comme le volet « Commentaires » d'Acrobat, la liste se trie (page,
//! auteur, date, type), se filtre (par type, par auteur) et se cherche
//! (texte, auteur, réponses, sans tenir compte de la casse ni des accents).
//! Chaque commentaire montre sa case « coché » et son statut, qu'un clic
//! change ; le tri et les filtres sont calculés par une fonction pure,
//! [`comment_order`], que les épreuves exercent seule.
//!
//! # Les vignettes : sélectionner, glisser un bloc
//!
//! Comme dans l'Explorateur et dans l'organiseur d'Acrobat : un clic mène à
//! la page ; `Ctrl+clic` ajoute ou retire une page de la sélection,
//! `Maj+clic` sélectionne la plage depuis l'ancre, `Ctrl+A` sélectionne
//! tout. La sélection ([`PageSelection`]) vit dans le document ouvert, pas
//! ici : le panneau la reçoit pour la dessiner (cadre et voile d'accent) et
//! rend des [`PanelAction`] qui disent quoi en faire. Glisser une page d'une
//! sélection de plusieurs pages emporte **tout le bloc**, qui laisse ses
//! emplacements vides pendant le geste.

// Coordonnées d'écran entières.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names
)]

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;

use acrux_graphics::Bitmap;

use crate::platform::{Frame, Key, Modifiers};
use crate::ui::input::{InputAction, TextInput};
use crate::ui::lang::{tr, trf};
use crate::ui::paint::{round_rect, round_rect_alpha};
use crate::ui::palette::fold_char;
use crate::ui::text::TextRenderer;
use crate::ui::theme::Theme;

/// Onglet affiché.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelTab {
    /// Miniatures des pages.
    Thumbnails,
    /// Signets.
    Bookmarks,
    /// Liste des commentaires (annotations) du document.
    Comments,
    /// Calques (contenu optionnel) avec leur case à cocher.
    Layers,
    /// Pièces jointes du document.
    Attachments,
}

/// Ce que fait un clic sur une vignette de la sélection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectMode {
    /// Clic simple : aller à la page ; la sélection explicite est levée.
    Only,
    /// `Ctrl+clic` : ajouter la page à la sélection, ou l'en retirer.
    Toggle,
    /// `Maj+clic` : la plage depuis l'ancre.
    Extend,
}

/// Pages sélectionnées dans les vignettes, et l'ancre des plages.
///
/// Tant que l'ensemble est vide, la sélection est **implicite** : c'est la
/// page courante, celle que montre le cadre d'accent — un clic sur une
/// vignette ne fait que mener à la page, comme avant. `Ctrl+clic`,
/// `Maj+clic` et `Ctrl+A` la rendent explicite : ce qui est alors sous le
/// voile d'accent est exactement ce sur quoi agiront pivoter, supprimer,
/// dupliquer, extraire ou glisser. Les indices se bornent au nombre de
/// pages après chaque modification ([`PageSelection::clamp`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageSelection {
    set: BTreeSet<usize>,
    anchor: Option<usize>,
}

impl PageSelection {
    /// Plus de sélection explicite, plus d'ancre.
    pub fn clear(&mut self) {
        self.set.clear();
        self.anchor = None;
    }

    /// Un clic simple : pas de sélection explicite, l'ancre sur `page`
    /// (d'où partira un `Maj+clic`).
    pub fn focus(&mut self, page: usize) {
        self.set.clear();
        self.anchor = Some(page);
    }

    /// La seule page `page`, explicitement (le clic droit sur une vignette
    /// qui n'était pas sélectionnée, comme dans l'Explorateur).
    pub fn only(&mut self, page: usize) {
        self.set.clear();
        self.set.insert(page);
        self.anchor = Some(page);
    }

    /// Ajoute `page` à la sélection, ou l'en retire. Une sélection encore
    /// implicite devient explicite d'abord : `current`, la page courante,
    /// en fait partie, comme l'élément actif de l'Explorateur.
    pub fn toggle(&mut self, page: usize, current: usize) {
        if self.set.is_empty() && page != current {
            self.set.insert(current);
        }
        if !self.set.remove(&page) {
            self.set.insert(page);
        }
        self.anchor = Some(page);
    }

    /// La plage de l'ancre (la page courante s'il n'y en a pas) à `page`,
    /// dans un sens ou dans l'autre ; l'ancre ne bouge pas.
    pub fn extend_to(&mut self, page: usize, current: usize) {
        let anchor = *self.anchor.get_or_insert(current);
        self.set = (anchor.min(page)..=anchor.max(page)).collect();
    }

    /// Toutes les pages.
    pub fn all(&mut self, count: usize) {
        self.set = (0..count).collect();
        if self.anchor.is_none_or(|a| a >= count) {
            self.anchor = (count > 0).then_some(0);
        }
    }

    /// Vrai si `page` est sélectionnée explicitement.
    #[must_use]
    pub fn contains(&self, page: usize) -> bool {
        self.set.contains(&page)
    }

    /// Nombre de pages sélectionnées explicitement.
    #[must_use]
    pub fn len(&self) -> usize {
        self.set.len()
    }

    /// Vrai sans sélection explicite.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// Les pages sélectionnées, dans l'ordre du document.
    #[must_use]
    pub fn sorted(&self) -> Vec<usize> {
        self.set.iter().copied().collect()
    }

    /// Retire les pages qui n'existent plus (après une suppression).
    pub fn clamp(&mut self, count: usize) {
        self.set.retain(|&p| p < count);
        if self.anchor.is_some_and(|a| a >= count) {
            self.anchor = count.checked_sub(1);
        }
    }

    /// Suit une modification du document : chaque page sélectionnée va où
    /// `place` la mène (`None` : elle a disparu), et rien ne reste au-delà
    /// de `count` pages. Une page supprimée sort de la sélection au lieu de
    /// laisser son indice à la page qui prend sa place.
    pub fn remap(&mut self, place: impl Fn(usize) -> Option<usize>, count: usize) {
        self.set = self
            .set
            .iter()
            .filter_map(|&p| place(p))
            .filter(|&p| p < count)
            .collect();
        self.anchor = self.anchor.and_then(&place).filter(|&a| a < count);
    }

    /// Sélectionne le bloc `start..start + len` : les pages déplacées,
    /// dupliquées ou insérées, là où elles sont maintenant.
    pub fn set_block(&mut self, start: usize, len: usize) {
        self.set = (start..start + len).collect();
        self.anchor = Some(start);
    }
}

/// Ce que le panneau demande au visualiseur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelAction {
    /// Rien.
    #[default]
    None,
    /// Une vignette cliquée (0 = première page) : aller à la page, ou
    /// changer la sélection, selon le mode.
    SelectPage {
        /// Page cliquée.
        page: usize,
        /// Clic simple, `Ctrl+clic` ou `Maj+clic`.
        mode: SelectMode,
    },
    /// Suivre le signet d'identifiant donné.
    Follow(usize),
    /// Aller au commentaire d'indice donné dans la liste fournie.
    GoToComment(usize),
    /// Afficher ou masquer un calque (numéro d'objet du groupe).
    ToggleLayer(u32),
    /// Enregistrer la pièce jointe d'indice donné dans la liste fournie.
    SaveAttachment(usize),
    /// Joindre un fichier au document (bouton « Ajouter »).
    AddAttachment,
    /// Une vignette glissée puis lâchée : la page `from` (et avec elle le
    /// bloc sélectionné qui la contient) va à la position `at`, comptée dans
    /// l'ordre d'origine, de 0 (avant la première page) au nombre de pages
    /// (après la dernière). Voir `pages::move_block`.
    DropPages {
        /// Page saisie.
        from: usize,
        /// Position d'insertion.
        at: usize,
    },
    /// Cocher ou décocher le commentaire d'indice donné dans la liste
    /// fournie.
    ToggleMarked(usize),
    /// Dérouler une liste du panneau des commentaires, sous ce rectangle
    /// (coordonnées du panneau).
    CommentMenu(CommentMenu, (i32, i32, i32, i32)),
}

/// Signet aplati (ordre d'affichage).
#[derive(Debug, Clone)]
pub struct OutlineRow {
    /// Identifiant stable (position dans la liste aplatie complète).
    pub id: usize,
    /// Profondeur (0 = racine).
    pub depth: usize,
    /// Titre.
    pub title: String,
    /// Possède des enfants.
    pub has_children: bool,
}

/// Réponse, dans le fil d'un commentaire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyRow {
    /// Page de la réponse.
    pub page: usize,
    /// Rang de la réponse dans `/Annots`.
    pub index: usize,
    /// Auteur.
    pub author: Option<String>,
    /// Date lisible.
    pub date: String,
    /// Texte.
    pub contents: String,
    /// 1 pour une réponse au commentaire, 2 pour la réponse à une réponse…
    pub depth: usize,
}

/// Commentaire affiché dans le panneau (une annotation porteuse de texte).
#[derive(Debug, Clone)]
pub struct CommentRow {
    /// Page (0 = première).
    pub page: usize,
    /// Type, en clé française (« Note », « Surlignage »…) : il est traduit
    /// au dessin, si bien qu'un changement de langue se voit tout de suite.
    pub kind: &'static str,
    /// Auteur, s'il est connu.
    pub author: Option<String>,
    /// Texte du commentaire (peut être vide).
    pub contents: String,
    /// Rectangle de l'annotation, en coordonnées de page : cliquer la ligne
    /// y mène.
    pub rect: acrux_core::Rect,
    /// Position de l'annotation dans le `/Annots` de sa page.
    pub index: usize,
    /// Identifiant de l'annotation (`/NM`), quand elle en a un. Avec
    /// `index`, c'est ce qui la désigne sans ambiguïté — pour y répondre ou
    /// lui donner un statut.
    pub name: Option<String>,
    /// Date lisible (« 2024-03-12 10:30 »), vide si le fichier n'en dit
    /// rien.
    pub date: String,
    /// Date brute (`D:…`) : elle se trie comme du texte.
    pub date_raw: String,
    /// Couleur de l'annotation, pour sa pastille.
    pub color: Option<(u8, u8, u8)>,
    /// Réponses, dans l'ordre du fil.
    pub replies: Vec<ReplyRow>,
    /// Statut de relecture, en clé française (« Accepté »…).
    pub status: Option<&'static str>,
    /// Case « coché ».
    pub marked: bool,
}

/// Ordre de la liste des commentaires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommentSort {
    /// Par page, puis dans l'ordre de la page.
    #[default]
    Page,
    /// Par auteur.
    Author,
    /// Du plus récent au plus ancien.
    Date,
    /// Par type.
    Kind,
}

impl CommentSort {
    /// Tous, dans l'ordre du menu.
    pub const ALL: [CommentSort; 4] = [
        CommentSort::Page,
        CommentSort::Author,
        CommentSort::Date,
        CommentSort::Kind,
    ];

    /// Libellé français.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            CommentSort::Page => "Page",
            CommentSort::Author => "Auteur",
            CommentSort::Date => "Date",
            CommentSort::Kind => "Type",
        }
    }
}

/// Ce que montre la liste des commentaires : son ordre, ses filtres, sa
/// recherche. Il survit à l'annulation d'une modification : le document est
/// rechargé, la liste qu'on regardait ne change pas.
#[derive(Debug, Clone, Default)]
pub struct CommentView {
    /// Ordre.
    pub sort: CommentSort,
    /// Types masqués, en clés françaises.
    pub hidden: Vec<&'static str>,
    /// Seul auteur montré, s'il y en a un.
    pub author: Option<String>,
    /// Recherche dans les textes, les auteurs et les réponses.
    pub search: TextInput,
    /// Le champ de recherche a le clavier.
    pub searching: bool,
}

impl CommentView {
    /// Nombre de filtres en cours (types masqués, auteur) : le bouton
    /// « Filtrer » l'affiche.
    #[must_use]
    pub fn filters(&self) -> usize {
        usize::from(!self.hidden.is_empty()) + usize::from(self.author.is_some())
    }
}

/// Liste déroulante demandée par le panneau des commentaires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentMenu {
    /// L'ordre.
    Sort,
    /// Les filtres : types et auteurs.
    Filter,
    /// Le statut du commentaire d'indice donné dans la liste fournie.
    Status(usize),
}

/// Un texte plié : minuscules, sans accents, comme la palette le compare.
fn folded(text: &str) -> String {
    text.chars().map(fold_char).collect()
}

/// Ordre d'affichage des commentaires : ceux qui passent les filtres et la
/// recherche, triés. Le tri est stable, départagé par la page puis le rang
/// dans la page : deux listes identiques s'affichent toujours pareil.
///
/// La recherche ne tient pas compte de la casse ni des accents, et regarde
/// aussi les réponses : une discussion se retrouve par ce qu'on y a dit.
#[must_use]
pub fn comment_order(rows: &[CommentRow], view: &CommentView) -> Vec<usize> {
    let query = folded(view.search.value.trim());
    let found = |r: &CommentRow| {
        query.is_empty()
            || folded(&r.contents).contains(&query)
            || r.author
                .as_deref()
                .is_some_and(|a| folded(a).contains(&query))
            || folded(tr(r.kind)).contains(&query)
            || r.replies.iter().any(|reply| {
                folded(&reply.contents).contains(&query)
                    || reply
                        .author
                        .as_deref()
                        .is_some_and(|a| folded(a).contains(&query))
            })
    };
    let mut out: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            !view.hidden.contains(&r.kind)
                && view
                    .author
                    .as_deref()
                    .is_none_or(|a| r.author.as_deref() == Some(a))
                && found(r)
        })
        .map(|(i, _)| i)
        .collect();
    let place = |i: usize| (rows[i].page, rows[i].index);
    match view.sort {
        CommentSort::Page => out.sort_by_key(|&i| place(i)),
        CommentSort::Author => out.sort_by(|&a, &b| {
            let who = |i: usize| folded(rows[i].author.as_deref().unwrap_or_default());
            who(a).cmp(&who(b)).then(place(a).cmp(&place(b)))
        }),
        CommentSort::Date => out.sort_by(|&a, &b| {
            rows[b]
                .date_raw
                .cmp(&rows[a].date_raw)
                .then(place(a).cmp(&place(b)))
        }),
        CommentSort::Kind => out.sort_by(|&a, &b| {
            folded(tr(rows[a].kind))
                .cmp(&folded(tr(rows[b].kind)))
                .then(place(a).cmp(&place(b)))
        }),
    }
    out
}

/// Pièce jointe affichée dans le panneau.
#[derive(Debug, Clone)]
pub struct AttachmentRow {
    /// Nom du fichier.
    pub name: String,
    /// Taille en octets, quand le document la déclare.
    pub size: Option<u64>,
    /// Page portant l'icône, s'il y en a une (0 = première).
    pub page: Option<usize>,
    /// Description (`/Desc`) ou type MIME : la deuxième ligne de la fiche.
    pub detail: String,
}

/// Données affichées par le panneau.
pub struct PanelContent<'a> {
    /// Nombre de pages.
    pub page_count: usize,
    /// Page courante.
    pub current: usize,
    /// Taille en pixels de la vignette de chaque page.
    pub thumb_sizes: &'a [(u32, u32)],
    /// Bitmaps rendus, par (page, clé d'échelle).
    pub bitmaps: &'a HashMap<(usize, u32), Bitmap>,
    /// Clé d'échelle des vignettes.
    pub thumb_key: u32,
    /// Signets aplatis (tous, l'état déplié est géré par le panneau).
    pub outline: &'a [OutlineRow],
    /// Commentaires du document, dans l'ordre des pages.
    pub comments: &'a [CommentRow],
    /// Calques du document, avec leur visibilité courante.
    pub layers: &'a [(u32, String, bool)],
    /// Pièces jointes du document.
    pub attachments: &'a [AttachmentRow],
    /// Pages sélectionnées dans les vignettes.
    pub selected: &'a PageSelection,
}

/// Taille de fichier en unités lisibles (base 1024, comme l'explorateur).
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["o", "Kio", "Mio", "Gio"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} o")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Tab(PanelTab),
    Page(usize),
    /// Ligne de signet : identifiant, et vrai si le clic est sur le triangle.
    Row(usize, bool),
    /// Ligne de commentaire (indice dans la liste fournie).
    Comment(usize),
    /// Ligne de calque (indice dans la liste fournie).
    Layer(usize),
    /// Ligne de pièce jointe (indice dans la liste fournie).
    Attachment(usize),
    /// Bouton « Ajouter un fichier ».
    AddAttachment,
    /// Case « coché » d'un commentaire (indice dans la liste fournie).
    CommentCheck(usize),
    /// Statut d'un commentaire (indice dans la liste fournie).
    CommentStatus(usize),
    /// Champ de recherche des commentaires.
    CommentSearch,
    /// Bouton « Trier ».
    CommentSortButton,
    /// Bouton « Filtrer ».
    CommentFilterButton,
}

/// Panneau latéral.
pub struct Panel {
    /// Onglet courant.
    pub tab: PanelTab,
    /// Défilement du contenu, en pixels.
    scroll: f64,
    /// Zones cliquables calculées au dernier dessin : rectangle et cible.
    hits: Vec<((i32, i32, i32, i32), Hit)>,
    hover: Option<Hit>,
    /// Cible du focus clavier, quand le panneau l'a. Le panneau est une liste :
    /// les flèches y montent et descendent, Entrée active, Échap rend le focus
    /// au document.
    focus: Option<Hit>,
    /// Signets dépliés (par identifiant). Les racines le sont par défaut.
    expanded: HashSet<usize>,
    /// Hauteur totale du contenu au dernier dessin (pour borner le défilement).
    content_height: i32,
    /// Hauteur visible au dernier dessin.
    view_height: i32,
    /// Vignettes visibles au dernier dessin (pour les demandes de rendu).
    visible_pages: Vec<usize>,
    /// Page sur laquelle le bouton a été enfoncé, tant qu'on ne sait pas
    /// encore si c'est un clic ou un glisser.
    press: Option<(usize, i32, i32)>,
    /// L'appui est tombé sur une page d'une sélection de plusieurs pages :
    /// il ne réduit la sélection à cette page qu'au relâchement sans
    /// glisser, sans quoi on ne pourrait pas glisser le bloc.
    press_in_selection: bool,
    /// Glisser en cours : page déplacée et position d'insertion visée.
    drag: Option<(usize, usize)>,
    /// Ordre, filtres et recherche de la liste des commentaires.
    pub comments: CommentView,
    /// Commentaires affichés au dernier dessin, dans l'ordre : le clavier
    /// les parcourt ainsi.
    comment_rows: Vec<usize>,
}

impl Default for Panel {
    fn default() -> Self {
        Self::new()
    }
}

impl Panel {
    /// Panneau sur l'onglet des vignettes.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tab: PanelTab::Thumbnails,
            scroll: 0.0,
            hits: Vec::new(),
            hover: None,
            focus: None,
            expanded: HashSet::new(),
            content_height: 0,
            view_height: 0,
            visible_pages: Vec::new(),
            press: None,
            press_in_selection: false,
            drag: None,
            comments: CommentView::default(),
            comment_rows: Vec::new(),
        }
    }

    /// Ouvre les signets de premier niveau marqués ouverts dans le document.
    pub fn set_default_expanded(&mut self, ids: impl IntoIterator<Item = usize>) {
        self.expanded = ids.into_iter().collect();
    }

    /// Pages dont la vignette était visible au dernier dessin.
    #[must_use]
    pub fn visible_pages(&self) -> &[usize] {
        &self.visible_pages
    }

    /// Hauteur de la barre d'onglets.
    fn header_height(theme: &Theme, dpi: f32) -> i32 {
        (theme.toolbar_height as f32 * 0.8 * dpi).round() as i32
    }

    /// Onglets affichés : `(onglet, libellé, libellé abrégé)`. L'abrégé sert
    /// quand la case est trop étroite pour le libellé complet.
    fn tabs(with_layers: bool) -> Vec<(PanelTab, &'static str, &'static str)> {
        let mut tabs = vec![
            (PanelTab::Thumbnails, "Pages", "Pages"),
            (PanelTab::Bookmarks, "Signets", "Plan"),
            (PanelTab::Comments, "Notes", "Notes"),
        ];
        if with_layers {
            tabs.push((PanelTab::Layers, "Calques", "Calq."));
        }
        tabs.push((PanelTab::Attachments, "Fichiers", "Fich."));
        tabs
    }

    /// Lignes de signets visibles (ancêtres tous dépliés).
    fn visible_rows<'a>(&self, rows: &'a [OutlineRow]) -> Vec<&'a OutlineRow> {
        let mut out = Vec::new();
        // Profondeur au-delà de laquelle tout est masqué (None = rien de masqué).
        let mut hidden_below: Option<usize> = None;
        for r in rows {
            if let Some(d) = hidden_below {
                if r.depth > d {
                    continue;
                }
                hidden_below = None;
            }
            out.push(r);
            if r.has_children && !self.expanded.contains(&r.id) {
                hidden_below = Some(r.depth);
            }
        }
        out
    }

    /// Dessine le panneau dans `frame` (sous-vue de la fenêtre).
    #[allow(clippy::too_many_lines)] // en-tête, puis l'un des deux onglets
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        text: &mut TextRenderer,
        theme: &Theme,
        dpi: f32,
        content: &PanelContent<'_>,
    ) {
        let t = theme;
        let (w, h) = (frame.width as i32, frame.height as i32);
        frame.fill_rect(0, 0, w, h, t.bar.0, t.bar.1, t.bar.2);
        frame.fill_rect(w - 1, 0, 1, h, t.separator.0, t.separator.1, t.separator.2);
        self.hits.clear();
        self.visible_pages.clear();
        let size = t.font_size * dpi;
        let pad = (10.0 * dpi).round() as i32;
        // En-tête : deux onglets.
        let hh = Self::header_height(t, dpi);
        // L'onglet des calques n'apparaît que pour les documents qui en ont :
        // sinon il prendrait de la place pour rien (et Acrobat fait pareil).
        // Celui des pièces jointes est toujours là : c'est de lui qu'on en
        // ajoute une, y compris à un document qui n'en a aucune.
        let mut tabs = Self::tabs(!content.layers.is_empty());
        if content.layers.is_empty() && self.tab == PanelTab::Layers {
            self.tab = PanelTab::Thumbnails;
        }
        let tab_w = (w - 1) / tabs.len() as i32;
        let room = (tab_w - pad / 2) as f32;
        // Cinq onglets dans 240 px logiques : chaque libellé trop large pour
        // sa case retombe sur sa forme abrégée, plutôt que d'être tronqué au
        // milieu d'un mot.
        for entry in &mut tabs {
            if text.measure(size, entry.1) > room {
                entry.1 = entry.2;
            }
        }
        for (i, (tab, label, _)) in tabs.iter().enumerate() {
            let x = i as i32 * tab_w;
            let active = *tab == self.tab;
            if self.hover == Some(Hit::Tab(*tab)) && !active {
                round_rect(frame, x + 2, 3, tab_w - 4, hh - 6, 7.0 * dpi, t.hover);
            }
            let color = if active { t.text } else { t.text_dim };
            let tw = text.measure(size, label).min(room);
            let baseline = f32::midpoint(hh as f32, text.ascent(size)) - 1.0;
            text.draw_clipped(
                frame,
                x as f32 + (tab_w as f32 - tw) / 2.0,
                baseline,
                size,
                label,
                color,
                room,
            );
            if active {
                frame.fill_rect(
                    x + pad,
                    hh - 2,
                    tab_w - 2 * pad,
                    2,
                    t.accent.0,
                    t.accent.1,
                    t.accent.2,
                );
            }
            self.hits.push(((x, 0, tab_w, hh), Hit::Tab(*tab)));
        }
        frame.fill_rect(0, hh, w, 1, t.separator.0, t.separator.1, t.separator.2);
        let view_h = h - hh - 1;
        self.view_height = view_h;
        let mut body = frame.sub(0, hh + 1, (w - 1).max(0) as u32, view_h.max(0) as u32);
        let scroll = self.scroll.round() as i32;
        match self.tab {
            PanelTab::Thumbnails => {
                let label_h = (18.0 * dpi).round() as i32;
                let gap = (14.0 * dpi).round() as i32;
                let mut y = pad - scroll;
                for (page, &(tw, th)) in content.thumb_sizes.iter().enumerate() {
                    let cell_h = th as i32 + label_h + gap;
                    let x = ((w - 1) - tw as i32) / 2;
                    if y + cell_h >= 0 && y <= view_h {
                        self.visible_pages.push(page);
                        // Ombre, cadre (accent pour la page courante et les
                        // pages sélectionnées), bitmap ou blanc.
                        let current = page == content.current;
                        let selected = content.selected.contains(page);
                        let hovered = self.hover == Some(Hit::Page(page))
                            || self.focus == Some(Hit::Page(page));
                        // Un bloc glissé laisse tous ses emplacements vides.
                        let moving = self.drag.is_some_and(|(from, _)| {
                            from == page
                                || (content.selected.len() >= 2
                                    && content.selected.contains(from)
                                    && selected)
                        });
                        if current || hovered || selected {
                            let c = if current || selected {
                                t.accent
                            } else {
                                t.text_dim
                            };
                            body.fill_rect(
                                x - 3,
                                y - 3,
                                tw as i32 + 6,
                                th as i32 + 6,
                                c.0,
                                c.1,
                                c.2,
                            );
                        }
                        body.fill_rect(
                            x + 2,
                            y + 2,
                            tw as i32,
                            th as i32,
                            t.page_shadow.0,
                            t.page_shadow.1,
                            t.page_shadow.2,
                        );
                        match content.bitmaps.get(&(page, content.thumb_key)) {
                            Some(b) if !moving => {
                                body.blit_rgba_premultiplied(x, y, b.width(), b.height(), b.data());
                            }
                            // Page en cours de déplacement : emplacement laissé vide.
                            _ => body.fill_rect(
                                x, y, tw as i32, th as i32, t.hover.0, t.hover.1, t.hover.2,
                            ),
                        }
                        // Le voile d'accent : ce qui est sélectionné se voit
                        // d'un coup d'œil, même sur une page blanche.
                        if selected && !moving {
                            round_rect_alpha(
                                &mut body, x, y, tw as i32, th as i32, 0.0, t.accent, 0.22,
                            );
                        }
                        // Trait d'insertion à l'endroit où la page tomberait.
                        if let Some((_, to)) = self.drag {
                            let line = (3.0 * dpi).round().max(2.0) as i32;
                            if to == page {
                                body.fill_rect(
                                    x - 6,
                                    y - 6,
                                    tw as i32 + 12,
                                    line,
                                    t.accent.0,
                                    t.accent.1,
                                    t.accent.2,
                                );
                            } else if to == page + 1 {
                                body.fill_rect(
                                    x - 6,
                                    y + th as i32 + 4,
                                    tw as i32 + 12,
                                    line,
                                    t.accent.0,
                                    t.accent.1,
                                    t.accent.2,
                                );
                            }
                        }
                        let label = format!("{}", page + 1);
                        let lw = text.measure(size, &label);
                        let baseline =
                            (y + th as i32) as f32 + label_h as f32 * 0.5 + text.ascent(size) / 2.0;
                        let color = if current || selected {
                            t.text
                        } else {
                            t.text_dim
                        };
                        text.draw(
                            &mut body,
                            ((w - 1) as f32 - lw) / 2.0,
                            baseline,
                            size,
                            &label,
                            color,
                        );
                        self.hits
                            .push(((0, hh + 1 + y - 3, w - 1, cell_h), Hit::Page(page)));
                    }
                    y += cell_h;
                }
                self.content_height = y + scroll + pad;
            }
            PanelTab::Bookmarks => {
                let row_h = (24.0 * dpi).round() as i32;
                let indent = (14.0 * dpi).round() as i32;
                let rows = self.visible_rows(content.outline);
                if rows.is_empty() {
                    let baseline = pad as f32 + text.ascent(size) + 4.0 * dpi;
                    text.draw_clipped(
                        &mut body,
                        pad as f32,
                        baseline,
                        size,
                        "Aucun signet",
                        t.text_dim,
                        (w - 2 * pad) as f32,
                    );
                }
                let mut y = -scroll;
                for r in &rows {
                    if y + row_h >= 0 && y <= view_h {
                        let hovered = matches!(self.hover, Some(Hit::Row(id, _)) if id == r.id);
                        if hovered {
                            round_rect(&mut body, 4, y + 1, w - 9, row_h - 2, 7.0 * dpi, t.hover);
                        }
                        if matches!(self.focus, Some(Hit::Row(id, _)) if id == r.id) {
                            ring(&mut body, t, 0, y, w - 1, row_h);
                        }
                        let x = pad + r.depth as i32 * indent;
                        // Triangle de dépliage.
                        if r.has_children {
                            let open = self.expanded.contains(&r.id);
                            let cx = x + (5.0 * dpi) as i32;
                            let cy = y + row_h / 2;
                            let s = (4.0 * dpi).max(2.0) as i32;
                            for i in 0..s {
                                if open {
                                    body.fill_rect(
                                        cx - s + i,
                                        cy - s / 2 + i,
                                        2 * (s - i),
                                        1,
                                        t.text_dim.0,
                                        t.text_dim.1,
                                        t.text_dim.2,
                                    );
                                } else {
                                    body.fill_rect(
                                        cx - s / 2 + i,
                                        cy - s + i,
                                        1,
                                        2 * (s - i),
                                        t.text_dim.0,
                                        t.text_dim.1,
                                        t.text_dim.2,
                                    );
                                }
                            }
                        }
                        let tx = x + (14.0 * dpi) as i32;
                        let baseline =
                            y as f32 + f32::midpoint(row_h as f32, text.ascent(size)) - 1.0;
                        text.draw_clipped(
                            &mut body,
                            tx as f32,
                            baseline,
                            size,
                            &r.title,
                            t.text,
                            (w - 1 - tx - pad) as f32,
                        );
                        self.hits
                            .push(((0, hh + 1 + y, w - 1, row_h), Hit::Row(r.id, false)));
                        self.hits.push((
                            (x - 2, hh + 1 + y, (18.0 * dpi) as i32, row_h),
                            Hit::Row(r.id, true),
                        ));
                    }
                    y += row_h;
                }
                self.content_height = y + scroll + pad;
            }
            PanelTab::Layers => {
                let row_h = (text.line_height(size) * 1.8).round() as i32;
                let box_side = (12.0 * dpi).round() as i32;
                let mut y = -scroll;
                for (index, (_, name, visible)) in content.layers.iter().enumerate() {
                    if y + row_h >= 0 && y <= view_h {
                        if self.hover == Some(Hit::Layer(index)) {
                            body.fill_rect(0, y, w - 1, row_h, t.hover.0, t.hover.1, t.hover.2);
                        }
                        if self.focus == Some(Hit::Layer(index)) {
                            ring(&mut body, t, 0, y, w - 1, row_h);
                        }
                        let bx = pad;
                        let by = y + (row_h - box_side) / 2;
                        let c = if *visible { t.accent } else { t.text_dim };
                        body.fill_rect(bx, by, box_side, box_side, c.0, c.1, c.2);
                        if *visible {
                            // Coche : deux traits en diagonale.
                            let s2 = box_side / 4;
                            for i in 0..s2 {
                                body.fill_rect(
                                    bx + 2 + i,
                                    by + box_side / 2 + i,
                                    2,
                                    1,
                                    255,
                                    255,
                                    255,
                                );
                            }
                            for i in 0..(box_side / 2) {
                                body.fill_rect(
                                    bx + 2 + s2 + i,
                                    by + box_side / 2 + s2 - i,
                                    2,
                                    1,
                                    255,
                                    255,
                                    255,
                                );
                            }
                        } else {
                            // Case vide : un cadre, pas de coche.
                            body.fill_rect(
                                bx + 1,
                                by + 1,
                                box_side - 2,
                                box_side - 2,
                                t.bar.0,
                                t.bar.1,
                                t.bar.2,
                            );
                        }
                        let tx = bx + box_side + pad / 2;
                        let baseline =
                            y as f32 + f32::midpoint(row_h as f32, text.ascent(size)) - 1.0;
                        text.draw_clipped(
                            &mut body,
                            tx as f32,
                            baseline,
                            size,
                            name,
                            if *visible { t.text } else { t.text_dim },
                            (w - 1 - tx - pad) as f32,
                        );
                        self.hits
                            .push(((0, hh + 1 + y, w - 1, row_h), Hit::Layer(index)));
                    }
                    y += row_h;
                }
                self.content_height = y + scroll + pad;
            }
            PanelTab::Comments => {
                self.paint_comments(&mut body, text, t, dpi, content, hh, view_h);
            }
            PanelTab::Attachments => {
                let line_h = text.line_height(size).round() as i32;
                let row_h = line_h * 2 + pad;
                // Bouton « Ajouter », épinglé en haut : il doit rester
                // atteignable même quand la liste est longue.
                let button_h = line_h + pad;
                let hovered = self.hover == Some(Hit::AddAttachment);
                let c = if hovered { t.accent } else { t.hover };
                body.fill_rect(pad, pad / 2, w - 1 - 2 * pad, button_h, c.0, c.1, c.2);
                let label = "Ajouter un fichier…";
                let lw = text.measure(size, label);
                text.draw_clipped(
                    &mut body,
                    ((w - 1) as f32 - lw) / 2.0,
                    (pad / 2) as f32 + f32::midpoint(button_h as f32, text.ascent(size)) - 1.0,
                    size,
                    label,
                    t.text,
                    (w - 1 - 2 * pad) as f32,
                );
                let top = button_h + pad;
                let list_h = (view_h - top).max(0);
                let mut list = body.sub(0, top, (w - 1).max(0) as u32, list_h as u32);
                if content.attachments.is_empty() {
                    let baseline = pad as f32 + text.ascent(size);
                    text.draw_clipped(
                        &mut list,
                        pad as f32,
                        baseline,
                        size,
                        "Aucune pièce jointe",
                        t.text_dim,
                        (w - 2 * pad) as f32,
                    );
                }
                let mut y = -scroll;
                for (index, a) in content.attachments.iter().enumerate() {
                    if y + row_h >= 0 && y <= list_h {
                        if self.hover == Some(Hit::Attachment(index)) {
                            list.fill_rect(0, y, w - 1, row_h, t.hover.0, t.hover.1, t.hover.2);
                        }
                        if self.focus == Some(Hit::Attachment(index)) {
                            ring(&mut list, t, 0, y, w - 1, row_h);
                        }
                        let base1 = y as f32 + (pad / 2) as f32 + text.ascent(size);
                        text.draw_clipped(
                            &mut list,
                            pad as f32,
                            base1,
                            size,
                            &a.name,
                            t.text,
                            (w - 1 - 2 * pad) as f32,
                        );
                        // Deuxième ligne : taille, page, puis description.
                        let mut detail = match a.size {
                            Some(s) => human_size(s),
                            None => "taille inconnue".to_string(),
                        };
                        if let Some(p) = a.page {
                            let _ = write!(detail, " — page {}", p + 1);
                        }
                        if !a.detail.is_empty() {
                            detail.push_str(" — ");
                            detail.push_str(&a.detail);
                        }
                        text.draw_clipped(
                            &mut list,
                            pad as f32,
                            base1 + line_h as f32,
                            size,
                            &detail,
                            t.text_dim,
                            (w - 1 - 2 * pad) as f32,
                        );
                        list.fill_rect(
                            0,
                            y + row_h - 1,
                            w - 1,
                            1,
                            t.separator.0,
                            t.separator.1,
                            t.separator.2,
                        );
                        self.hits
                            .push(((0, hh + 1 + top + y, w - 1, row_h), Hit::Attachment(index)));
                    }
                    y += row_h;
                }
                // Le bouton est déclaré en dernier : `hit_at` prend la
                // dernière correspondance, donc il gagne sur une ligne qui
                // passerait dessous.
                self.hits.push((
                    (pad, hh + 1 + pad / 2, w - 1 - 2 * pad, button_h),
                    Hit::AddAttachment,
                ));
                self.content_height = y + scroll + top + pad;
            }
        }
        // Barre de défilement discrète.
        if self.content_height > view_h && view_h > 0 {
            let track = view_h;
            let thumb_h = ((f64::from(view_h) / f64::from(self.content_height)) * f64::from(track))
                .max(20.0) as i32;
            let max_scroll = f64::from((self.content_height - view_h).max(1));
            let ty = ((self.scroll / max_scroll) * f64::from(track - thumb_h)) as i32;
            frame.fill_rect(
                w - 5,
                hh + 1 + ty,
                3,
                thumb_h,
                t.text_dim.0,
                t.text_dim.1,
                t.text_dim.2,
            );
        }
    }

    /// L'onglet des commentaires : un bandeau fixe (recherche, tri, filtre),
    /// puis les commentaires dans l'ordre choisi, chacun avec sa pastille,
    /// son type, son auteur, sa page, sa date, son statut, sa case, son
    /// texte et ses réponses indentées.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)] // une liste, ligne à ligne
    fn paint_comments(
        &mut self,
        body: &mut Frame<'_>,
        text: &mut TextRenderer,
        t: &Theme,
        dpi: f32,
        content: &PanelContent<'_>,
        hh: i32,
        view_h: i32,
    ) {
        let s = |v: f32| (v * dpi).round() as i32;
        let w = body.width as i32;
        let size = t.font_size * dpi;
        let small = size * 0.9;
        let pad = s(10.0);
        let line_h = text.line_height(size).round() as i32;
        let small_h = text.line_height(small).round() as i32;
        // Le bandeau : la recherche, puis le tri et le filtre.
        let field_h = s(28.0);
        let button_h = s(24.0);
        let header_h = pad / 2 + field_h + s(6.0) + button_h + pad / 2;
        self.comments.search.placeholder = tr("Rechercher dans les commentaires").to_string();
        self.comments.search.focused = self.comments.searching;
        let field = (pad, pad / 2, w - 2 * pad, field_h);
        self.comments
            .search
            .draw(body, text, t, dpi, field.0, field.1, field.2, field.3);
        // Le bandeau se déclare après les lignes (voir la fin) : une ligne
        // à demi défilée sous lui ne lui vole pas ses clics.
        let mut header_hits = vec![(
            (field.0, hh + 1 + field.1, field.2, field.3),
            Hit::CommentSearch,
        )];
        let by = field.1 + field_h + s(6.0);
        let bw = (w - 2 * pad - s(6.0)) / 2;
        let sort_label = trf("Trier : {}", &[tr(self.comments.sort.label())]);
        let filters = self.comments.filters();
        let filter_label = if filters == 0 {
            tr("Filtrer").to_string()
        } else {
            trf("Filtrer ({})", &[&filters.to_string()])
        };
        for (i, (label, hit)) in [
            (sort_label, Hit::CommentSortButton),
            (filter_label, Hit::CommentFilterButton),
        ]
        .into_iter()
        .enumerate()
        {
            let bx = pad + i as i32 * (bw + s(6.0));
            let hovered = self.hover == Some(hit);
            let active = hit == Hit::CommentFilterButton && filters > 0;
            let face = if hovered { t.button_hover } else { t.hover };
            round_rect(body, bx, by, bw, button_h, 6.0 * dpi, face);
            let color = if active { t.accent } else { t.text };
            text.draw_clipped(
                body,
                (bx + s(8.0)) as f32,
                (by + button_h / 2) as f32 + text.ascent(small) / 2.0,
                small,
                &label,
                color,
                (bw - s(24.0)) as f32,
            );
            // Le chevron, en deux traits : la liste se déroule.
            let (cx, cy) = ((bx + bw - s(12.0)) as f32, (by + button_h / 2) as f32);
            let r = 3.0 * dpi;
            crate::ui::paint::line(
                body,
                cx - r,
                cy - r / 2.0,
                cx,
                cy + r / 2.0,
                dpi,
                t.text_dim,
            );
            crate::ui::paint::line(
                body,
                cx,
                cy + r / 2.0,
                cx + r,
                cy - r / 2.0,
                dpi,
                t.text_dim,
            );
            if self.focus == Some(hit) {
                ring(body, t, bx, by, bw, button_h);
            }
            header_hits.push(((bx, hh + 1 + by, bw, button_h), hit));
        }
        body.fill_rect(
            0,
            header_h - 1,
            w,
            1,
            t.separator.0,
            t.separator.1,
            t.separator.2,
        );
        let list_h = (view_h - header_h).max(0);
        let mut list = body.sub(0, header_h, w.max(0) as u32, list_h as u32);
        let order = comment_order(content.comments, &self.comments);
        if order.is_empty() {
            let message = if content.comments.is_empty() {
                tr("Aucun commentaire")
            } else {
                tr("Aucun commentaire ne correspond")
            };
            text.draw_clipped(
                &mut list,
                pad as f32,
                pad as f32 + text.ascent(size),
                size,
                message,
                t.text_dim,
                (w - 2 * pad) as f32,
            );
        }
        let scroll = self.scroll.round() as i32;
        let box_side = s(14.0);
        let mut y = -scroll;
        for &index in &order {
            let Some(c) = content.comments.get(index) else {
                continue;
            };
            let room = (w - 2 * pad) as f32;
            let mut measure = |l: &str| text.measure(size, l);
            let mut lines = if c.contents.trim().is_empty() {
                vec![tr("(sans texte)").to_string()]
            } else {
                // Deux lignes montrées : une troisième suffit à savoir qu'il
                // faut couper.
                crate::ui::bubble::wrap_at_most(&c.contents, room, &mut measure, 2)
            };
            if lines.len() > 2 {
                lines.truncate(2);
                if let Some(last) = lines.last_mut() {
                    last.push('…');
                }
            }
            let replies_h = if c.replies.is_empty() {
                0
            } else {
                s(4.0) + c.replies.len() as i32 * small_h
            };
            let row_h = pad / 2
                + line_h
                + small_h
                + s(4.0)
                + lines.len() as i32 * line_h
                + replies_h
                + pad / 2
                + s(2.0);
            if y + row_h >= 0 && y <= list_h {
                if self.hover == Some(Hit::Comment(index)) {
                    list.fill_rect(0, y, w, row_h, t.hover.0, t.hover.1, t.hover.2);
                }
                if self.focus == Some(Hit::Comment(index)) {
                    ring(&mut list, t, 0, y, w, row_h);
                }
                // Première ligne : pastille, type et auteur ; la page et la
                // case à droite.
                let top = y + pad / 2;
                let base1 = top as f32 + text.ascent(size);
                let mut x = pad;
                if let Some(color) = c.color {
                    let chip = s(9.0);
                    round_rect(
                        &mut list,
                        x,
                        top + (line_h - chip) / 2,
                        chip,
                        chip,
                        chip as f32 / 2.0,
                        color,
                    );
                    x += chip + s(6.0);
                }
                let bx = w - pad - box_side;
                let by = top + (line_h - box_side) / 2;
                paint_check(&mut list, t, dpi, (bx, by, box_side), c.marked);
                self.hits.push((
                    (
                        bx - s(4.0),
                        hh + 1 + header_h + by - s(4.0),
                        box_side + s(8.0),
                        box_side + s(8.0),
                    ),
                    Hit::CommentCheck(index),
                ));
                let page = trf("p. {}", &[&(c.page + 1).to_string()]);
                let page_w = text.measure(small, &page).ceil() as i32;
                let page_x = bx - s(8.0) - page_w;
                text.draw(&mut list, page_x as f32, base1, small, &page, t.text_dim);
                let head = match &c.author {
                    Some(who) if !who.is_empty() => format!("{} — {who}", tr(c.kind)),
                    _ => tr(c.kind).to_string(),
                };
                text.draw_clipped(
                    &mut list,
                    x as f32,
                    base1,
                    size,
                    &head,
                    t.text,
                    (page_x - s(6.0) - x) as f32,
                );
                // Deuxième ligne : la date, et le statut (cliquable) à droite.
                let top2 = top + line_h;
                let base2 = top2 as f32 + text.ascent(small);
                let status = c.status.map_or(tr("Statut"), tr);
                let chip_w = text.measure(small, status).ceil() as i32 + s(14.0);
                let chip_x = w - pad - chip_w;
                let chip_y = top2 + s(1.0);
                let chip_h = small_h;
                if c.status.is_some() {
                    round_rect(
                        &mut list,
                        chip_x,
                        chip_y,
                        chip_w,
                        chip_h,
                        chip_h as f32 / 2.0,
                        t.accent,
                    );
                } else {
                    crate::ui::paint::round_rect_outline(
                        &mut list,
                        chip_x,
                        chip_y,
                        chip_w,
                        chip_h,
                        chip_h as f32 / 2.0,
                        dpi.max(1.0),
                        if self.hover == Some(Hit::CommentStatus(index)) {
                            t.accent
                        } else {
                            t.separator
                        },
                    );
                }
                text.draw(
                    &mut list,
                    (chip_x + s(7.0)) as f32,
                    base2,
                    small,
                    status,
                    if c.status.is_some() {
                        (255, 255, 255)
                    } else {
                        t.text_dim
                    },
                );
                self.hits.push((
                    (chip_x, hh + 1 + header_h + chip_y, chip_w, chip_h),
                    Hit::CommentStatus(index),
                ));
                text.draw_clipped(
                    &mut list,
                    pad as f32,
                    base2,
                    small,
                    &c.date,
                    t.text_dim,
                    (chip_x - s(6.0) - pad) as f32,
                );
                // Le texte, sur deux lignes au plus.
                let mut ty = top2 + small_h + s(4.0);
                for l in &lines {
                    text.draw_clipped(
                        &mut list,
                        pad as f32,
                        ty as f32 + text.ascent(size),
                        size,
                        l,
                        t.text,
                        room,
                    );
                    ty += line_h;
                }
                // Les réponses, en retrait, une ligne chacune.
                if !c.replies.is_empty() {
                    ty += s(4.0);
                    for r in &c.replies {
                        let indent = pad + s(14.0) * r.depth.min(6) as i32;
                        let who = r.author.as_deref().unwrap_or("?");
                        list.fill_rect(
                            indent - s(7.0),
                            ty + s(2.0),
                            s(2.0).max(1),
                            small_h - s(4.0),
                            t.accent.0,
                            t.accent.1,
                            t.accent.2,
                        );
                        let line = format!("{who} : {}", r.contents.replace('\n', " "));
                        text.draw_clipped(
                            &mut list,
                            indent as f32,
                            ty as f32 + text.ascent(small),
                            small,
                            &line,
                            t.text_dim,
                            (w - pad - indent) as f32,
                        );
                        ty += small_h;
                    }
                }
                list.fill_rect(
                    0,
                    y + row_h - 1,
                    w,
                    1,
                    t.separator.0,
                    t.separator.1,
                    t.separator.2,
                );
                // La ligne entière, déclarée avant la case et le statut :
                // `hit_at` prend la dernière correspondance, eux gagnent.
                let at = self.hits.len().saturating_sub(2);
                self.hits.insert(
                    at,
                    ((0, hh + 1 + header_h + y, w, row_h), Hit::Comment(index)),
                );
            }
            y += row_h;
        }
        self.hits.extend(header_hits);
        self.comment_rows = order;
        self.content_height = y + scroll + header_h + pad;
    }

    /// Cibles atteignables au clavier dans l'onglet courant, dans l'ordre.
    fn focusable(&self, content: &PanelContent<'_>) -> Vec<Hit> {
        match self.tab {
            PanelTab::Thumbnails => (0..content.page_count).map(Hit::Page).collect(),
            PanelTab::Bookmarks => self
                .visible_rows(content.outline)
                .iter()
                .map(|r| Hit::Row(r.id, false))
                .collect(),
            PanelTab::Comments => [Hit::CommentSortButton, Hit::CommentFilterButton]
                .into_iter()
                .chain(
                    self.comment_rows
                        .iter()
                        .filter(|&&i| i < content.comments.len())
                        .map(|&i| Hit::Comment(i)),
                )
                .collect(),
            PanelTab::Layers => (0..content.layers.len()).map(Hit::Layer).collect(),
            PanelTab::Attachments => std::iter::once(Hit::AddAttachment)
                .chain((0..content.attachments.len()).map(Hit::Attachment))
                .collect(),
        }
    }

    /// Place le focus sur la première cible (ou la dernière si `last`).
    /// Renvoie faux si l'onglet courant est vide.
    pub fn focus_edge(&mut self, last: bool, content: &PanelContent<'_>) -> bool {
        let targets = self.focusable(content);
        self.focus = if last {
            targets.last().copied()
        } else {
            targets.first().copied()
        };
        self.focus.is_some()
    }

    /// Déplace le focus d'une ligne. Renvoie faux quand on sort de la liste.
    pub fn focus_step(&mut self, forward: bool, content: &PanelContent<'_>) -> bool {
        let targets = self.focusable(content);
        let Some(current) = self.focus else {
            return self.focus_edge(!forward, content);
        };
        let Some(at) = targets.iter().position(|h| *h == current) else {
            return self.focus_edge(!forward, content);
        };
        let next = if forward {
            Some(at + 1)
        } else {
            at.checked_sub(1)
        };
        if let Some(target) = next.and_then(|n| targets.get(n)) {
            self.focus = Some(*target);
            true
        } else {
            self.focus = None;
            false
        }
    }

    /// Abandonne le focus clavier.
    pub fn clear_focus(&mut self) {
        self.focus = None;
    }

    /// Vrai si le panneau tient le focus clavier.
    #[must_use]
    pub fn focused(&self) -> bool {
        self.focus.is_some()
    }

    /// Page ciblée par le focus, s'il vise une vignette (pour la faire
    /// apparaître dans la fenêtre du panneau).
    #[must_use]
    pub fn focused_page(&self) -> Option<usize> {
        match self.focus {
            Some(Hit::Page(p)) => Some(p),
            _ => None,
        }
    }

    /// Met le focus clavier sur la vignette d'une page : « Organiser les
    /// pages » ouvre le panneau là où l'on est.
    pub fn focus_page(&mut self, page: usize) {
        if self.tab == PanelTab::Thumbnails {
            self.focus = Some(Hit::Page(page));
        }
    }

    /// Vignette sous un point du panneau, s'il y en a une : c'est ce que
    /// vise un clic droit, qui ouvre le menu de la page.
    #[must_use]
    pub fn page_at(&self, x: i32, y: i32) -> Option<usize> {
        match self.hit_at(x, y) {
            Some(Hit::Page(p)) => Some(p),
            _ => None,
        }
    }

    /// Rectangle de la vignette d'une page au dernier dessin, si elle est
    /// visible : le menu ouvert au clavier se pose dessus.
    #[must_use]
    pub fn page_rect(&self, page: usize) -> Option<(i32, i32, i32, i32)> {
        self.hits
            .iter()
            .find(|(_, hit)| *hit == Hit::Page(page))
            .map(|(rect, _)| *rect)
    }

    /// Active la ligne sous le focus (Entrée ou Espace).
    pub fn activate_focus(&mut self, content: &PanelContent<'_>) -> PanelAction {
        match self.focus {
            Some(hit) => self.act_on(hit, content),
            None => PanelAction::None,
        }
    }

    /// Déplie ou replie le signet sous le focus (flèches gauche et droite).
    pub fn toggle_focused_branch(&mut self, open: bool) -> bool {
        let Some(Hit::Row(id, _)) = self.focus else {
            return false;
        };
        if open {
            self.expanded.insert(id)
        } else {
            self.expanded.remove(&id)
        }
    }

    fn hit_at(&self, x: i32, y: i32) -> Option<Hit> {
        // Les zones spécifiques (triangles) sont déclarées après les lignes : on
        // prend la dernière correspondance.
        self.hits
            .iter()
            .rev()
            .find(|((rx, ry, rw, rh), _)| x >= *rx && x < rx + rw && y >= *ry && y < ry + rh)
            .map(|(_, h)| *h)
    }

    /// Déplacement de la souris. `dragging` indique que le bouton gauche est
    /// enfoncé : au-delà d'un seuil, le clic devient un glisser de page.
    pub fn mouse_move(&mut self, x: i32, y: i32, dragging: bool) -> bool {
        if !dragging {
            self.press = None;
            self.press_in_selection = false;
            self.drag = None;
        } else if let Some((page, px, py)) = self.press {
            if self.drag.is_none() && ((x - px).abs() > 4 || (y - py).abs() > 4) {
                self.drag = Some((page, page));
            }
            if self.drag.is_some() {
                if let Some(to) = self.drop_index(y) {
                    let changed = self.drag.is_some_and(|(_, old)| old != to);
                    self.drag = Some((page, to));
                    return changed;
                }
                return false;
            }
        }
        let hover = self.hit_at(x, y).map(|h| match h {
            Hit::Row(id, _) => Hit::Row(id, false),
            other => other,
        });
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    /// Le pointeur a quitté le panneau.
    pub fn mouse_leave(&mut self) -> bool {
        let changed = self.hover.is_some();
        self.hover = None;
        changed
    }

    /// Position d'insertion (0 = avant la première page) visée par des
    /// fichiers déposés au point `(x, y)` du panneau : entre deux vignettes,
    /// comme le glisser d'une page. `None` si le panneau ne montre pas les
    /// vignettes, ou n'en a dessiné aucune.
    #[must_use]
    pub fn insertion_at(&self, x: i32, y: i32) -> Option<usize> {
        if self.tab != PanelTab::Thumbnails || x < 0 {
            return None;
        }
        self.drop_index(y)
    }

    /// Position d'insertion visée par un glisser à l'ordonnée `y`.
    fn drop_index(&self, y: i32) -> Option<usize> {
        let mut best: Option<(i32, usize)> = None;
        for (rect, hit) in &self.hits {
            let Hit::Page(page) = hit else { continue };
            let (_, ry, _, rh) = *rect;
            // Moitié haute : insérer avant ; moitié basse : après.
            let (d, index) = if y < ry + rh / 2 {
                ((y - ry).abs(), *page)
            } else {
                ((y - ry - rh).abs(), page + 1)
            };
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, index));
            }
        }
        best.map(|(_, i)| i)
    }

    /// Bouton relâché : termine un glisser éventuel. Un appui sur une page
    /// d'une sélection de plusieurs pages, relâché sans glisser, devient un
    /// clic simple sur cette page.
    ///
    /// La position rendue est brute : déposer un bloc à sa propre place
    /// rend l'ordre d'origine (`pages::move_block`), et le visualiseur ne
    /// fait alors rien.
    pub fn mouse_up(&mut self) -> PanelAction {
        let press = self.press.take();
        let in_selection = std::mem::take(&mut self.press_in_selection);
        match self.drag.take() {
            Some((from, at)) => PanelAction::DropPages { from, at },
            None => match press {
                Some((page, _, _)) if in_selection => PanelAction::SelectPage {
                    page,
                    mode: SelectMode::Only,
                },
                _ => PanelAction::None,
            },
        }
    }

    /// Vrai si un glisser de page est en cours.
    #[must_use]
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Clic gauche, avec les modificateurs du clavier : `Ctrl` ajoute ou
    /// retire la vignette de la sélection, `Maj` sélectionne la plage.
    pub fn mouse_down(
        &mut self,
        x: i32,
        y: i32,
        m: Modifiers,
        content: &PanelContent<'_>,
    ) -> PanelAction {
        self.press = None;
        self.press_in_selection = false;
        if let Some(Hit::Page(page)) = self.hit_at(x, y) {
            self.comments.searching = false;
            if m.ctrl {
                return PanelAction::SelectPage {
                    page,
                    mode: SelectMode::Toggle,
                };
            }
            if m.shift {
                return PanelAction::SelectPage {
                    page,
                    mode: SelectMode::Extend,
                };
            }
            // Seul un appui sans modificateur peut devenir un glisser.
            self.press = Some((page, x, y));
            if content.selected.len() >= 2 && content.selected.contains(page) {
                self.press_in_selection = true;
                return PanelAction::None;
            }
        }
        let hit = self.hit_at(x, y);
        // Un clic ailleurs que dans le champ de recherche lui rend le
        // clavier : les raccourcis du document reprennent.
        if hit != Some(Hit::CommentSearch) {
            self.comments.searching = false;
        }
        match hit {
            Some(hit) => self.act_on(hit, content),
            None => PanelAction::None,
        }
    }

    /// Rectangle d'une cible au dernier dessin.
    fn rect_of(&self, hit: Hit) -> (i32, i32, i32, i32) {
        self.hits
            .iter()
            .rev()
            .find(|(_, h)| *h == hit)
            .map_or((0, 0, 0, 0), |(r, _)| *r)
    }

    /// Vrai si le champ de recherche des commentaires a le clavier.
    #[must_use]
    pub fn comment_search_focused(&self) -> bool {
        self.tab == PanelTab::Comments && self.comments.searching
    }

    /// Le champ de recherche des commentaires rend le clavier.
    pub fn blur_comment_search(&mut self) {
        self.comments.searching = false;
    }

    /// Touche dans le champ de recherche. Entrée et Échap le quittent ;
    /// rend vrai si la liste change.
    pub fn comment_search_key(&mut self, key: Key, shift: bool) -> bool {
        match self.comments.search.key(key, shift) {
            InputAction::Changed => {
                self.scroll = 0.0;
                true
            }
            InputAction::Submit | InputAction::Cancel => {
                self.comments.searching = false;
                true
            }
            InputAction::None => false,
        }
    }

    /// Caractère tapé dans le champ de recherche.
    pub fn comment_search_char(&mut self, c: char) {
        if self.comments.search.insert_char(c) == InputAction::Changed {
            self.scroll = 0.0;
        }
    }

    /// Texte collé dans le champ de recherche.
    pub fn comment_search_paste(&mut self, text: &str) {
        let _ = self.comments.search.paste(text);
        self.scroll = 0.0;
    }

    /// Ce que déclenche une cible, qu'on l'ait atteinte à la souris ou au
    /// clavier : une seule règle, donc pas de divergence entre les deux.
    fn act_on(&mut self, hit: Hit, content: &PanelContent<'_>) -> PanelAction {
        match hit {
            Hit::Tab(tab) => {
                if tab != self.tab {
                    self.tab = tab;
                    self.scroll = 0.0;
                    self.focus = None;
                }
                PanelAction::None
            }
            Hit::Page(page) => PanelAction::SelectPage {
                page,
                mode: SelectMode::Only,
            },
            Hit::Row(id, on_toggle) => {
                let has_children = content.outline.iter().any(|r| r.id == id && r.has_children);
                if on_toggle && has_children {
                    if !self.expanded.remove(&id) {
                        self.expanded.insert(id);
                    }
                    PanelAction::None
                } else {
                    PanelAction::Follow(id)
                }
            }
            Hit::Comment(index) => PanelAction::GoToComment(index),
            Hit::CommentCheck(index) if index < content.comments.len() => {
                PanelAction::ToggleMarked(index)
            }
            Hit::CommentStatus(index) if index < content.comments.len() => {
                PanelAction::CommentMenu(CommentMenu::Status(index), self.rect_of(hit))
            }
            Hit::CommentSearch => {
                self.comments.searching = true;
                PanelAction::None
            }
            Hit::CommentSortButton => {
                PanelAction::CommentMenu(CommentMenu::Sort, self.rect_of(hit))
            }
            Hit::CommentFilterButton => {
                PanelAction::CommentMenu(CommentMenu::Filter, self.rect_of(hit))
            }
            Hit::Layer(index) => match content.layers.get(index) {
                Some((number, _, _)) => PanelAction::ToggleLayer(*number),
                None => PanelAction::None,
            },
            Hit::Attachment(index) if index < content.attachments.len() => {
                PanelAction::SaveAttachment(index)
            }
            Hit::AddAttachment => PanelAction::AddAttachment,
            // Indice hors de la liste (elle a changé depuis le dessin) : rien.
            Hit::Attachment(_) | Hit::CommentCheck(_) | Hit::CommentStatus(_) => PanelAction::None,
        }
    }

    /// Molette (crans positifs = vers le haut).
    pub fn wheel(&mut self, delta: f32) {
        self.scroll -= f64::from(delta) * 80.0;
        self.clamp();
    }

    fn clamp(&mut self) {
        let max = f64::from((self.content_height - self.view_height).max(0));
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// Fait défiler les vignettes pour montrer la page donnée.
    pub fn reveal_page(&mut self, page: usize, thumb_sizes: &[(u32, u32)], dpi: f32) {
        if self.tab != PanelTab::Thumbnails {
            return;
        }
        let label_h = (18.0 * dpi).round() as i32;
        let gap = (14.0 * dpi).round() as i32;
        let pad = (10.0 * dpi).round() as i32;
        let mut y = pad;
        for (i, &(_, th)) in thumb_sizes.iter().enumerate() {
            let cell_h = th as i32 + label_h + gap;
            if i == page {
                let top = f64::from(y - pad);
                let bottom = f64::from(y + cell_h);
                if top < self.scroll {
                    self.scroll = top;
                } else if bottom > self.scroll + f64::from(self.view_height) {
                    self.scroll = bottom - f64::from(self.view_height);
                }
                break;
            }
            y += cell_h;
        }
        self.clamp();
    }
}

/// Case à cocher de `côté` pixels en `(x, y)` : cochée, pleine d'accent
/// avec sa coche blanche ; sinon un cadre.
fn paint_check(
    frame: &mut Frame<'_>,
    t: &Theme,
    dpi: f32,
    (x, y, side): (i32, i32, i32),
    on: bool,
) {
    let radius = 3.0 * dpi;
    if on {
        round_rect(frame, x, y, side, side, radius, t.accent);
        let (fx, fy, fs) = (x as f32, y as f32, side as f32);
        let w = (1.6 * dpi).max(1.0);
        crate::ui::paint::line(
            frame,
            fx + fs * 0.22,
            fy + fs * 0.52,
            fx + fs * 0.43,
            fy + fs * 0.72,
            w,
            (255, 255, 255),
        );
        crate::ui::paint::line(
            frame,
            fx + fs * 0.43,
            fy + fs * 0.72,
            fx + fs * 0.78,
            fy + fs * 0.3,
            w,
            (255, 255, 255),
        );
    } else {
        crate::ui::paint::round_rect_outline(
            frame,
            x,
            y,
            side,
            side,
            radius,
            dpi.max(1.0),
            t.text_dim,
        );
    }
}

/// Anneau de focus d'une ligne de panneau, visible sur fond clair comme sur
/// fond sombre.
fn ring(frame: &mut Frame<'_>, t: &Theme, x: i32, y: i32, w: i32, h: i32) {
    if w <= 0 || h <= 0 {
        return;
    }
    let a = t.accent;
    for inset in [0, 1] {
        let (x, y, w, h) = (x + inset, y + inset, w - 2 * inset, h - 2 * inset);
        if w <= 0 || h <= 0 {
            continue;
        }
        frame.fill_rect(x, y, w, 1, a.0, a.1, a.2);
        frame.fill_rect(x, y + h - 1, w, 1, a.0, a.1, a.2);
        frame.fill_rect(x, y, 1, h, a.0, a.1, a.2);
        frame.fill_rect(x + w - 1, y, 1, h, a.0, a.1, a.2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<OutlineRow> {
        vec![
            OutlineRow {
                id: 0,
                depth: 0,
                title: "A".into(),
                has_children: true,
            },
            OutlineRow {
                id: 1,
                depth: 1,
                title: "A.1".into(),
                has_children: true,
            },
            OutlineRow {
                id: 2,
                depth: 2,
                title: "A.1.a".into(),
                has_children: false,
            },
            OutlineRow {
                id: 3,
                depth: 0,
                title: "B".into(),
                has_children: false,
            },
        ]
    }

    fn plain() -> Modifiers {
        Modifiers::default()
    }

    fn only(page: usize) -> PanelAction {
        PanelAction::SelectPage {
            page,
            mode: SelectMode::Only,
        }
    }

    #[test]
    fn dropped_files_aim_between_the_thumbnails() {
        let mut p = Panel::new();
        p.tab = PanelTab::Thumbnails;
        // Sans vignette dessinée, aucune place à viser.
        assert_eq!(p.insertion_at(50, 50), None);
        p.hits = vec![
            ((0, 0, 200, 100), Hit::Page(0)),
            ((0, 100, 200, 100), Hit::Page(1)),
        ];
        assert_eq!(p.insertion_at(50, 20), Some(0), "moitié haute : avant");
        assert_eq!(p.insertion_at(50, 80), Some(1), "moitié basse : après");
        assert_eq!(p.insertion_at(50, 180), Some(2), "après la dernière");
        assert_eq!(p.insertion_at(-5, 80), None, "hors du panneau");
        // Sur un autre onglet, un dépôt n'est pas une insertion.
        p.tab = PanelTab::Bookmarks;
        assert_eq!(p.insertion_at(50, 80), None);
    }

    #[test]
    fn dragging_a_thumbnail_moves_the_page() {
        let mut p = Panel::new();
        p.hits = vec![
            ((0, 0, 200, 100), Hit::Page(0)),
            ((0, 100, 200, 100), Hit::Page(1)),
            ((0, 200, 200, 100), Hit::Page(2)),
        ];
        let bitmaps = HashMap::new();
        let none = PageSelection::default();
        let content = PanelContent {
            page_count: 3,
            current: 0,
            thumb_sizes: &[(10, 10); 3],
            bitmaps: &bitmaps,
            thumb_key: 1,
            outline: &[],
            comments: &[],
            layers: &[],
            attachments: &[],
            selected: &none,
        };
        // Un clic simple reste un clic : il mène à la page.
        assert_eq!(p.mouse_down(50, 50, plain(), &content), only(0));
        assert_eq!(p.mouse_up(), PanelAction::None);
        // Un déplacement au-delà du seuil devient un glisser.
        assert_eq!(p.mouse_down(50, 50, plain(), &content), only(0));
        // Déposé entre les vignettes 1 et 2 : position brute 2.
        p.mouse_move(52, 250, true);
        assert!(p.dragging());
        assert_eq!(p.mouse_up(), PanelAction::DropPages { from: 0, at: 2 });
        // Déposé sous la dernière vignette : après la dernière page.
        assert_eq!(p.mouse_down(50, 50, plain(), &content), only(0));
        p.mouse_move(52, 295, true);
        assert_eq!(p.mouse_up(), PanelAction::DropPages { from: 0, at: 3 });
        assert!(!p.dragging());
        // Déposé à sa propre place : la position est rendue telle quelle,
        // c'est `move_block` qui y voit l'ordre d'origine.
        assert_eq!(p.mouse_down(50, 50, plain(), &content), only(0));
        p.mouse_move(52, 60, true);
        assert_eq!(p.mouse_up(), PanelAction::DropPages { from: 0, at: 1 });
    }

    /// Ctrl+clic et Maj+clic changent la sélection sans commencer de
    /// glisser ; un appui sur une page d'un bloc sélectionné ne le réduit
    /// qu'au relâchement, et un glisser l'emporte tout entier.
    #[test]
    fn modifiers_select_and_a_block_can_be_dragged() {
        let mut p = Panel::new();
        p.hits = (0..4)
            .map(|i| ((0, i * 100, 200, 100), Hit::Page(i as usize)))
            .collect();
        let bitmaps = HashMap::new();
        let mut selection = PageSelection::default();
        selection.toggle(1, 0);
        let content = PanelContent {
            page_count: 4,
            current: 0,
            thumb_sizes: &[(10, 10); 4],
            bitmaps: &bitmaps,
            thumb_key: 1,
            outline: &[],
            comments: &[],
            layers: &[],
            attachments: &[],
            selected: &selection,
        };
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(
            p.mouse_down(50, 250, ctrl, &content),
            PanelAction::SelectPage {
                page: 2,
                mode: SelectMode::Toggle
            }
        );
        p.mouse_move(52, 390, true);
        assert!(!p.dragging(), "un Ctrl+clic ne glisse pas");
        assert_eq!(p.mouse_up(), PanelAction::None);
        assert_eq!(
            p.mouse_down(50, 350, shift, &content),
            PanelAction::SelectPage {
                page: 3,
                mode: SelectMode::Extend
            }
        );
        assert_eq!(p.mouse_up(), PanelAction::None);
        // Pages 0 et 1 sélectionnées : l'appui sur la 1 attend.
        assert_eq!(p.mouse_down(50, 150, plain(), &content), PanelAction::None);
        assert_eq!(p.mouse_up(), only(1), "relâché sans glisser : un clic");
        assert_eq!(p.mouse_down(50, 150, plain(), &content), PanelAction::None);
        p.mouse_move(50, 395, true);
        assert_eq!(p.mouse_up(), PanelAction::DropPages { from: 1, at: 4 });
        // Une page hors de la sélection : un clic ordinaire.
        assert_eq!(p.mouse_down(50, 250, plain(), &content), only(2));
    }

    #[test]
    fn the_selection_follows_explorer_rules() {
        let mut s = PageSelection::default();
        assert!(s.is_empty());
        // Ctrl+clic depuis la page courante 2 : elle entre avec la page 5.
        s.toggle(5, 2);
        assert_eq!(s.sorted(), [2, 5]);
        // Maj+clic : la plage depuis l'ancre (la dernière page cliquée),
        // dans les deux sens, l'ancre ne bougeant pas.
        s.extend_to(8, 0);
        assert_eq!(s.sorted(), [5, 6, 7, 8]);
        s.extend_to(3, 0);
        assert_eq!(s.sorted(), [3, 4, 5]);
        // Un second Ctrl+clic retire la page.
        s.toggle(4, 2);
        assert_eq!(s.sorted(), [3, 5]);
        // Sans ancre, la plage part de la page courante.
        let mut fresh = PageSelection::default();
        fresh.extend_to(1, 3);
        assert_eq!(fresh.sorted(), [1, 2, 3]);
        s.all(4);
        assert_eq!(s.sorted(), [0, 1, 2, 3]);
        assert_eq!(s.len(), 4);
        s.clamp(2);
        assert_eq!(s.sorted(), [0, 1], "après une suppression");
        s.set_block(3, 2);
        assert_eq!(s.sorted(), [3, 4]);
        // Une suppression de la page 3 : la 4 devient la 3.
        s.remap(|p| (p != 3).then(|| if p > 3 { p - 1 } else { p }), 9);
        assert_eq!(s.sorted(), [3]);
        s.set_block(3, 2);
        assert!(s.contains(4) && !s.contains(2));
        s.only(7);
        assert_eq!(s.sorted(), [7]);
        s.focus(1);
        assert!(s.is_empty(), "un clic simple lève la sélection");
        s.extend_to(3, 0);
        assert_eq!(s.sorted(), [1, 2, 3], "depuis la page du clic");
        s.clear();
        assert!(s.is_empty());
    }

    /// Cinq onglets tiennent dans la largeur du panneau : chaque libellé
    /// retenu (complet ou abrégé) doit **entrer** dans sa case, sinon il
    /// serait coupé au milieu d'un mot. La mesure se fait avec la vraie
    /// police d'interface ; sans police système, le test ne peut rien dire.
    #[test]
    fn every_tab_label_fits_its_slot() {
        let Some(mut text) = TextRenderer::system() else {
            return;
        };
        let theme = Theme::dark();
        // Largeur du panneau (`PANEL_WIDTH` du visualiseur) à l'échelle 1.
        let panel_width = 240;
        let pad = 10;
        for with_layers in [false, true] {
            let tabs = Panel::tabs(with_layers);
            let tab_w = (panel_width - 1) / tabs.len() as i32;
            let room = (tab_w - pad / 2) as f32;
            for (tab, long, short) in tabs {
                let chosen = if text.measure(theme.font_size, long) > room {
                    short
                } else {
                    long
                };
                let width = text.measure(theme.font_size, chosen);
                assert!(
                    width <= room,
                    "libellé « {chosen} » de l'onglet {tab:?} : {width:.1} px pour {room:.1} px disponibles"
                );
            }
        }
    }

    #[test]
    fn human_sizes_use_binary_units() {
        assert_eq!(human_size(0), "0 o");
        assert_eq!(human_size(512), "512 o");
        assert_eq!(human_size(1024), "1.0 Kio");
        assert_eq!(human_size(1_572_864), "1.5 Mio");
    }

    /// Onglet des pièces jointes : un clic sur une ligne propose de
    /// l'enregistrer, un clic sur le bouton en ajoute une.
    #[test]
    fn attachment_rows_and_button_report_their_action() {
        let mut p = Panel::new();
        p.tab = PanelTab::Attachments;
        let bitmaps = HashMap::new();
        let none = PageSelection::default();
        let rows = vec![
            AttachmentRow {
                name: "a.txt".into(),
                size: Some(12),
                page: None,
                detail: "text/plain".into(),
            },
            AttachmentRow {
                name: "b.bin".into(),
                size: None,
                page: Some(2),
                detail: String::new(),
            },
        ];
        let content = PanelContent {
            page_count: 3,
            current: 0,
            thumb_sizes: &[(10, 10); 3],
            bitmaps: &bitmaps,
            thumb_key: 1,
            outline: &[],
            comments: &[],
            layers: &[],
            attachments: &rows,
            selected: &none,
        };
        p.hits.push(((0, 40, 200, 40), Hit::Attachment(0)));
        p.hits.push(((0, 80, 200, 40), Hit::Attachment(1)));
        p.hits.push(((0, 120, 200, 40), Hit::Attachment(9)));
        p.hits.push(((0, 0, 200, 30), Hit::AddAttachment));
        assert_eq!(
            p.mouse_down(10, 50, plain(), &content),
            PanelAction::SaveAttachment(0)
        );
        assert_eq!(
            p.mouse_down(10, 90, plain(), &content),
            PanelAction::SaveAttachment(1)
        );
        // Indice hors liste (panneau repeint entre-temps) : rien ne se passe.
        assert_eq!(p.mouse_down(10, 130, plain(), &content), PanelAction::None);
        assert_eq!(
            p.mouse_down(10, 10, plain(), &content),
            PanelAction::AddAttachment
        );
    }

    #[test]
    fn collapsed_ancestors_hide_descendants() {
        let mut p = Panel::new();
        let r = rows();
        let v: Vec<usize> = p.visible_rows(&r).iter().map(|r| r.id).collect();
        assert_eq!(v, vec![0, 3]);
        p.set_default_expanded([0]);
        let v: Vec<usize> = p.visible_rows(&r).iter().map(|r| r.id).collect();
        assert_eq!(v, vec![0, 1, 3]);
        p.set_default_expanded([0, 1]);
        let v: Vec<usize> = p.visible_rows(&r).iter().map(|r| r.id).collect();
        assert_eq!(v, vec![0, 1, 2, 3]);
    }

    #[test]
    fn clicks_resolve_from_last_layout() {
        let mut p = Panel::new();
        p.tab = PanelTab::Bookmarks;
        let r = rows();
        let bitmaps = HashMap::new();
        let none = PageSelection::default();
        let content = PanelContent {
            page_count: 2,
            current: 0,
            thumb_sizes: &[(100, 140), (100, 140)],
            bitmaps: &bitmaps,
            thumb_key: 1,
            outline: &r,
            comments: &[],
            layers: &[],
            attachments: &[],
            selected: &none,
        };
        p.hits.push(((0, 40, 200, 24), Hit::Row(0, false)));
        p.hits.push(((8, 40, 18, 24), Hit::Row(0, true)));
        assert_eq!(
            p.mouse_down(100, 50, plain(), &content),
            PanelAction::Follow(0)
        );
        assert_eq!(p.mouse_down(12, 50, plain(), &content), PanelAction::None);
        assert!(p.expanded.contains(&0));
        p.hits
            .push(((0, 0, 100, 30), Hit::Tab(PanelTab::Thumbnails)));
        assert_eq!(p.mouse_down(10, 10, plain(), &content), PanelAction::None);
        assert_eq!(p.tab, PanelTab::Thumbnails);
        p.hits.push(((0, 40, 200, 160), Hit::Page(1)));
        assert_eq!(p.mouse_down(50, 100, plain(), &content), only(1));
        assert!(p.mouse_move(50, 100, false));
        assert!(!p.mouse_move(51, 101, false));
        assert!(p.mouse_leave());
    }

    fn comment(
        page: usize,
        index: usize,
        kind: &'static str,
        author: &str,
        date: &str,
    ) -> CommentRow {
        CommentRow {
            page,
            kind,
            author: Some(author.to_string()),
            contents: format!("texte {page}-{index}"),
            rect: acrux_core::Rect::default(),
            index,
            name: None,
            date: date.to_string(),
            date_raw: format!("D:{date}"),
            color: None,
            replies: Vec::new(),
            status: None,
            marked: false,
        }
    }

    fn comments() -> Vec<CommentRow> {
        let mut rows = vec![
            comment(1, 0, "Note", "Bruno", "20240103"),
            comment(0, 2, "Ellipse", "Alice", "20240101"),
            comment(0, 1, "Note", "Élodie", "20240105"),
            comment(2, 0, "Rectangle", "alice", "20240102"),
        ];
        rows[1].replies.push(ReplyRow {
            page: 0,
            index: 3,
            author: Some("Bruno".into()),
            date: String::new(),
            contents: "Je corrige ça".into(),
            depth: 1,
        });
        rows
    }

    /// Tri par page, auteur (sans accents ni casse), date (du plus récent)
    /// et type ; filtres par type et par auteur ; recherche qui trouve
    /// aussi ce que disent les réponses.
    #[test]
    fn les_commentaires_se_trient_se_filtrent_et_se_cherchent() {
        let rows = comments();
        let mut view = CommentView::default();
        assert_eq!(comment_order(&rows, &view), [2, 1, 0, 3]);
        view.sort = CommentSort::Author;
        assert_eq!(comment_order(&rows, &view), [1, 3, 0, 2]);
        view.sort = CommentSort::Date;
        assert_eq!(comment_order(&rows, &view), [2, 0, 3, 1]);
        // Des types dont l'ordre est le même en français et en anglais :
        // d'autres épreuves changent la langue pendant que celle-ci tourne.
        view.sort = CommentSort::Kind;
        assert_eq!(comment_order(&rows, &view), [1, 2, 0, 3]);
        view.sort = CommentSort::Page;
        view.hidden.push("Note");
        assert_eq!(comment_order(&rows, &view), [1, 3]);
        assert_eq!(view.filters(), 1);
        view.hidden.clear();
        view.author = Some("Bruno".into());
        assert_eq!(comment_order(&rows, &view), [0]);
        view.author = None;
        view.search.set_value("CORRIGE");
        assert_eq!(comment_order(&rows, &view), [1], "trouvé dans une réponse");
        view.search.set_value("elodie");
        assert_eq!(comment_order(&rows, &view), [2], "sans accent");
        view.search.set_value("rien de tel");
        assert!(comment_order(&rows, &view).is_empty());
    }

    /// La case d'un commentaire le coche, sa ligne y mène — par l'indice
    /// dans la liste fournie, même quand l'ordre affiché diffère ; le
    /// statut et les boutons déroulent leur liste sous eux.
    #[test]
    fn les_clics_du_panneau_des_commentaires() {
        let mut p = Panel::new();
        p.tab = PanelTab::Comments;
        let rows = comments();
        let bitmaps = HashMap::new();
        let none = PageSelection::default();
        let content = PanelContent {
            page_count: 3,
            current: 0,
            thumb_sizes: &[(10, 10); 3],
            bitmaps: &bitmaps,
            thumb_key: 1,
            outline: &[],
            comments: &rows,
            layers: &[],
            attachments: &[],
            selected: &none,
        };
        p.hits = vec![
            ((0, 100, 200, 60), Hit::Comment(2)),
            ((170, 105, 20, 20), Hit::CommentCheck(2)),
            ((120, 125, 50, 16), Hit::CommentStatus(2)),
            ((0, 160, 200, 60), Hit::Comment(9)),
            ((10, 40, 180, 28), Hit::CommentSearch),
            ((10, 72, 88, 24), Hit::CommentSortButton),
        ];
        assert_eq!(
            p.mouse_down(50, 130, plain(), &content),
            PanelAction::GoToComment(2)
        );
        assert_eq!(
            p.mouse_down(175, 110, plain(), &content),
            PanelAction::ToggleMarked(2)
        );
        assert_eq!(
            p.mouse_down(130, 130, plain(), &content),
            PanelAction::CommentMenu(CommentMenu::Status(2), (120, 125, 50, 16))
        );
        assert_eq!(
            p.mouse_down(20, 80, plain(), &content),
            PanelAction::CommentMenu(CommentMenu::Sort, (10, 72, 88, 24))
        );
        // Le champ de recherche prend le clavier ; un autre clic le lui
        // reprend.
        assert_eq!(p.mouse_down(20, 50, plain(), &content), PanelAction::None);
        assert!(p.comment_search_focused());
        p.comment_search_char('b');
        assert!(p.comment_search_key(Key::Backspace, false));
        assert!(p.comments.search.value.is_empty());
        p.comment_search_char('x');
        assert!(p.comment_search_key(Key::Escape, false));
        assert!(!p.comment_search_focused());
        assert_eq!(p.mouse_down(20, 50, plain(), &content), PanelAction::None);
        assert_eq!(
            p.mouse_down(50, 130, plain(), &content),
            PanelAction::GoToComment(2)
        );
        assert!(!p.comment_search_focused());
    }

    #[test]
    fn le_clic_droit_vise_une_vignette() {
        let mut p = Panel::new();
        p.hits
            .push(((0, 0, 100, 30), Hit::Tab(PanelTab::Bookmarks)));
        p.hits.push(((0, 40, 200, 160), Hit::Page(3)));
        assert_eq!(p.page_at(50, 100), Some(3));
        assert_eq!(
            p.page_at(50, 10),
            None,
            "un onglet du panneau n'est pas une page"
        );
        assert_eq!(p.page_at(50, 300), None);
        assert_eq!(p.page_rect(3), Some((0, 40, 200, 160)));
        assert_eq!(p.page_rect(4), None, "vignette hors de la vue");
    }
}
