//! Recomposition d'un paragraphe entier dans sa boîte : retour à la ligne aux
//! espaces, interligne, alignement et retrait de première ligne d'origine.
//!
//! Le paragraphe est repéré par son indice dans l'ordre de lecture de la page
//! (`PageText::blocks`, paragraphes mis bout à bout). Toutes les opérations de
//! dessin de texte qui l'ont produit sont supprimées, et le texte recomposé
//! est écrit **à la place de la première d'entre elles** : il hérite donc
//! exactement de la matrice courante, du découpage (`W n`) et de la couleur du
//! texte d'origine, et le reste de la page n'est pas touché.
//!
//! Comportement en cas de débordement : si le nouveau texte demande plus de
//! lignes que la boîte n'en contenait, les lignes supplémentaires sont écrites
//! **en dessous** de la boîte, au même interligne (elles peuvent recouvrir ce
//! qui suit). [`ReflowOptions::keep_font_size`] à `false` réduit d'abord la
//! taille (par pas de 5 %, jusqu'à `min_size`) pour tenir dans le nombre de
//! lignes d'origine.
//!
//! # Édition en direct
//!
//! Un éditeur de texte recompose le paragraphe **à chaque frappe**. Deux
//! choses deviennent alors indispensables, et ce module les fournit.
//!
//! **Une boîte figée** ([`ParagraphFrame`]). Après une recomposition, la
//! boîte que l'extraction retrouve est celle du texte réellement posé, donc
//! un peu plus étroite que l'ancienne ; recomposer dans *cette* boîte-là
//! rétrécirait le paragraphe d'un mot à chaque frappe. La boîte est donc
//! relevée une fois, à l'ouverture, et réutilisée jusqu'au bout.
//!
//! **La position de chaque caractère** ([`CaretMap`]). Pour poser un curseur,
//! il faut savoir où tombe chaque frontière de caractère, espaces compris —
//! ce que le texte extrait ne dit pas, puisqu'il ne garde que les mots. La
//! mise en page qui sert à écrire le paragraphe sert donc aussi à placer le
//! curseur : ce qu'on voit et ce qu'on édite ne peuvent pas diverger.
//!
//! Les retours à la ligne tapés (`\n`) sont respectés : ils coupent la ligne,
//! là où une espace ne ferait que la laisser couler.

use std::collections::BTreeMap;

use acrux_core::{Error, Matrix, Rect, Result};
use acrux_document::{Document, Name, Page};

use super::runs::{Run, Styles};
use super::{
    encode, fmt, out_str, restore_matrices, rewrite_show_op, scan, write_matrix, write_tj, Cut,
    GlyphIndex, Item, Replacement,
};
use crate::text::{extract_page_text, Alignment, Glyph, Line, PageText, Paragraph};
use std::fmt::Write as _;

/// Options de recomposition.
#[derive(Debug, Clone)]
pub struct ReflowOptions {
    /// Conserver la taille de police d'origine (défaut) : le texte trop long
    /// déborde sous la boîte.
    pub keep_font_size: bool,
    /// Taille minimale (points) si la réduction est autorisée.
    pub min_size: f64,
}

impl Default for ReflowOptions {
    fn default() -> Self {
        Self {
            keep_font_size: true,
            min_size: 6.0,
        }
    }
}

/// La boîte d'un paragraphe, relevée une fois et gardée pendant une édition.
///
/// Toutes les grandeurs sont en espace de page (points, ordonnées vers le
/// haut).
#[derive(Debug, Clone, PartialEq)]
pub struct ParagraphFrame {
    /// Bord gauche de la boîte.
    pub x0: f64,
    /// Largeur disponible pour les lignes.
    pub width: f64,
    /// Alignement.
    pub alignment: Alignment,
    /// Retrait de la première ligne.
    pub first_line_indent: f64,
    /// Distance entre deux lignes de base.
    pub line_spacing: f64,
    /// Ligne de base de la première ligne.
    pub baseline: f64,
    /// Corps du texte, en points de page.
    pub size: f64,
    /// Ressource de police à employer.
    pub font: Name,
    /// Police standard à ajouter à la page au moment d'écrire, pour une zone
    /// de texte neuve. Tant que rien n'est tapé, la page n'est pas touchée :
    /// ajouter la police dès l'ouverture de la zone laisserait une ressource
    /// orpheline à chaque clic sans suite.
    pub standard: Option<String>,
    /// Couleur d'un bloc **nouveau** ; un paragraphe existant garde la sienne.
    pub color: [f64; 3],
    /// Couleur **imposée** au bloc entier, choisie dans le nuancier.
    ///
    /// Vide, un paragraphe existant garde son encre — c'est le cas ordinaire.
    /// Renseignée, le bloc est réécrit avec elle, et l'encre d'avant est
    /// rétablie derrière lui pour la suite du flux.
    pub ink: Option<[f64; 3]>,
    /// Police imposée au bloc entier : famille, graisse, italique.
    ///
    /// Vide, le bloc garde ses polices d'origine — c'est le cas ordinaire.
    /// Renseignée, une police standard est ajoutée au document et tout le
    /// bloc est réécrit avec elle : c'est « changer la police » d'Acrobat.
    pub face: Option<FaceChoice>,
    /// Inclinaison du bloc, en radians (0 pour un texte droit).
    ///
    /// Un filigrane est posé en biais ; le recomposer horizontalement le
    /// redresserait. Les lignes se mettent donc en page dans le repère du
    /// bloc, puis se posent tournées — et `x0`/`baseline` sont alors
    /// l'**origine** du texte, non le coin de sa boîte.
    pub rotation: f64,
}

/// Police choisie pour un bloc.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FaceChoice {
    /// Famille (« Helvetica », « Times New Roman »…) ; vide = celle du bloc.
    pub family: Option<String>,
    /// Gras.
    pub bold: bool,
    /// Italique.
    pub italic: bool,
}

impl FaceChoice {
    /// Nom de famille à demander à la police standard, graisse comprise.
    ///
    /// C'est par le **nom** que passe le choix : « Times New Roman Bold »
    /// désigne la police standard grasse la plus proche.
    #[must_use]
    pub fn named(&self, fallback: &str) -> String {
        let family = self.family.clone().unwrap_or_else(|| fallback.to_string());
        match (self.bold, self.italic) {
            (true, true) => format!("{family} Bold Italic"),
            (true, false) => format!("{family} Bold"),
            (false, true) => format!("{family} Italic"),
            (false, false) => family,
        }
    }
}

/// Position d'une frontière de caractère.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaretStop {
    /// Abscisse, en espace de page.
    pub x: f64,
    /// Ligne de base.
    pub baseline: f64,
    /// Ligne visuelle.
    pub line: usize,
}

/// Une ligne visuelle : les frontières qu'elle possède.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaretLine {
    /// Première frontière de la ligne.
    pub first: usize,
    /// Dernière frontière de la ligne (incluse).
    pub last: usize,
    /// Ligne de base.
    pub baseline: f64,
}

/// Où tombe chaque frontière de caractère d'un paragraphe.
///
/// Pour un texte de `n` caractères il y a `n + 1` frontières : avant le
/// premier, entre chacun, après le dernier. Une frontière qui tombe sur une
/// coupure de ligne appartient à la ligne qui **commence**, comme dans tout
/// traitement de texte.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CaretMap {
    /// Une position par frontière.
    pub stops: Vec<CaretStop>,
    /// Lignes visuelles, de haut en bas.
    pub lines: Vec<CaretLine>,
    /// Corps du texte, en points de page.
    pub size: f64,
}

impl CaretMap {
    /// Nombre de caractères.
    #[must_use]
    pub fn len(&self) -> usize {
        self.stops.len().saturating_sub(1)
    }

    /// Vrai pour un paragraphe vide.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Frontière la plus proche d'un point de la page.
    #[must_use]
    pub fn nearest(&self, x: f64, y: f64) -> usize {
        let Some(line) = self
            .lines
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                let da = (a.baseline + self.size * 0.3 - y).abs();
                let db = (b.baseline + self.size * 0.3 - y).abs();
                da.total_cmp(&db)
            })
            .map(|(i, _)| i)
        else {
            return 0;
        };
        self.nearest_on_line(line, x)
    }

    fn nearest_on_line(&self, line: usize, x: f64) -> usize {
        let Some(l) = self.lines.get(line) else {
            return 0;
        };
        (l.first..=l.last.min(self.len()))
            .min_by(|a, b| {
                let da = (self.stops[*a].x - x).abs();
                let db = (self.stops[*b].x - x).abs();
                da.total_cmp(&db)
            })
            .unwrap_or(l.first)
    }

    /// Frontière d'une ligne au-dessus (`delta < 0`) ou au-dessous, à la même
    /// abscisse.
    #[must_use]
    pub fn vertical(&self, index: usize, delta: i32) -> usize {
        let Some(stop) = self.stops.get(index.min(self.len())) else {
            return index;
        };
        let target = i64::try_from(stop.line).unwrap_or(0) + i64::from(delta);
        if target < 0 {
            return 0;
        }
        let Ok(target) = usize::try_from(target) else {
            return index;
        };
        if target >= self.lines.len() {
            return self.len();
        }
        self.nearest_on_line(target, stop.x)
    }

    /// Début et fin de la ligne qui contient une frontière.
    #[must_use]
    pub fn line_bounds(&self, index: usize) -> (usize, usize) {
        let line = self.stops.get(index.min(self.len())).map_or(0, |s| s.line);
        self.lines
            .get(line)
            .map_or((0, self.len()), |l| (l.first, l.last.min(self.len())))
    }

    /// Rectangle du curseur à une frontière, en espace de page.
    #[must_use]
    pub fn caret_rect(&self, index: usize) -> Rect {
        let s = self
            .stops
            .get(index.min(self.len()))
            .copied()
            .unwrap_or(CaretStop {
                x: 0.0,
                baseline: 0.0,
                line: 0,
            });
        let w = (self.size * 0.06).max(0.5);
        Rect::new(
            s.x - w / 2.0,
            s.baseline - self.size * 0.22,
            s.x + w / 2.0,
            s.baseline + self.size * 0.82,
        )
    }

    /// Rectangles de sélection entre deux frontières, un par ligne.
    #[must_use]
    pub fn selection_rects(&self, a: usize, b: usize) -> Vec<Rect> {
        let (a, b) = (a.min(b), a.max(b).min(self.len()));
        if a == b {
            return Vec::new();
        }
        let mut out = Vec::new();
        for line in &self.lines {
            let last = line.last.min(self.len());
            if last < a || line.first > b {
                continue;
            }
            let from = a.max(line.first);
            let to = b.min(last);
            let (x0, x1) = (self.stops[from].x, self.stops[to].x);
            if (x1 - x0).abs() < 1e-6 && from == to {
                continue;
            }
            out.push(Rect::new(
                x0.min(x1),
                line.baseline - self.size * 0.22,
                x0.max(x1),
                line.baseline + self.size * 0.82,
            ));
        }
        out
    }

    /// Boîte englobant tout le texte.
    #[must_use]
    pub fn bounds(&self) -> Option<Rect> {
        let mut it = self.stops.iter();
        let first = it.next()?;
        let mut r = Rect::new(
            first.x,
            first.baseline - self.size * 0.22,
            first.x,
            first.baseline + self.size * 0.82,
        );
        for s in it {
            r.x0 = r.x0.min(s.x);
            r.x1 = r.x1.max(s.x);
            r.y0 = r.y0.min(s.baseline - self.size * 0.22);
            r.y1 = r.y1.max(s.baseline + self.size * 0.82);
        }
        Some(r)
    }
}

/// Un paragraphe ouvert pour l'édition.
#[derive(Debug, Clone)]
pub struct OpenedParagraph {
    /// Boîte à conserver pendant l'édition.
    pub frame: ParagraphFrame,
    /// Texte éditable, tel qu'il est dessiné.
    pub text: String,
    /// Ce qui est actuellement dessiné, sans blancs : c'est ce qui permet de
    /// retrouver le paragraphe à la frappe suivante.
    pub drawn: String,
    /// Positions des frontières, sur le dessin d'origine.
    pub caret: CaretMap,
    /// Style de chaque caractère : police, corps, couleur.
    ///
    /// Une ligne mêle souvent plusieurs styles ; les relever permet de les
    /// **rendre** à l'écriture, au lieu d'aplatir le bloc d'une seule police.
    pub styles: Styles,
}

impl OpenedParagraph {
    /// Le texte auquel se rapportent les styles, tel qu'il était à
    /// l'ouverture.
    #[must_use]
    pub fn text_at_open(&self) -> String {
        self.text.clone()
    }
}

/// Recompose le paragraphe `paragraph_index` de la page avec `new_text`.
///
/// # Errors
/// Paragraphe inexistant, texte non modifiable (XObject, opérateurs non
/// textuels au milieu), texte non horizontal, ou page non indirecte.
pub fn reflow_paragraph(
    doc: &Document,
    page: &Page,
    paragraph_index: usize,
    new_text: &str,
    options: &ReflowOptions,
) -> Result<()> {
    let text = extract_page_text(doc, page)?;
    let paragraphs: Vec<&Paragraph> = text
        .blocks
        .iter()
        .flat_map(|b| b.paragraphs.iter())
        .collect();
    let para = paragraphs.get(paragraph_index).ok_or_else(|| {
        Error::Corrupt(format!(
            "paragraphe {paragraph_index} inexistant ({} dans la page)",
            paragraphs.len()
        ))
    })?;
    let unit = unit_of_paragraph(para, &text);
    write_paragraph(doc, page, &text, &unit, new_text, None, options, None).map(|_| ())
}

/// Ouvre un bloc pour l'édition (indice dans [`text_units`]) : relève sa
/// boîte, le texte tel qu'il est dessiné, et la position de chaque caractère
/// **sur le dessin d'origine** — cliquer dedans ne déplace donc rien.
///
/// # Errors
/// Bloc inexistant, non horizontal, ou dessiné dans un XObject.
pub fn open_paragraph(
    doc: &Document,
    page: &Page,
    text: &PageText,
    unit_index: usize,
) -> Result<OpenedParagraph> {
    let units = text_units(text);
    let unit = units.get(unit_index).ok_or_else(|| {
        Error::Corrupt(format!(
            "bloc {unit_index} inexistant ({} dans la page)",
            units.len()
        ))
    })?;
    open_unit(doc, page, text, unit)
}

/// Ouvre un bloc donné explicitement, plutôt que par son rang.
///
/// C'est par là que passe le repli sur **une seule ligne** : quand un bloc
/// entier refuse de s'ouvrir — une opération de dessin mêle ses lignes à
/// d'autres, cas courant des documents produits par un traitement de texte —
/// la ligne cliquée, elle, s'ouvre souvent très bien. Mieux vaut modifier une
/// ligne que rien du tout.
///
/// # Errors
/// Bloc non horizontal, dessiné dans un XObject, ou mêlé à un autre texte.
pub fn open_unit(
    doc: &Document,
    page: &Page,
    text: &PageText,
    unit: &TextUnit,
) -> Result<OpenedParagraph> {
    let target = Target::find(doc, page, text, unit)?;
    let size = target.size_text * target.scale;
    let crop = page.crop_box(doc);
    let slant =
        (target.rotation.abs() > 1e-4).then_some((target.rotation, target.trm.e, target.trm.f));
    let frame = frame_of(unit, text, size, target.font.clone(), &crop, slant);
    let (string, caret, sources) = caret_from_glyphs(unit, text, size);
    let styles = styles_of(&target, &sources, size);
    Ok(OpenedParagraph {
        drawn: normalized(&glyph_text(unit, text)),
        frame,
        text: string,
        caret,
        styles,
    })
}

/// Relève le style de chaque caractère : police, corps, couleur.
///
/// Le style se lit dans le **flux**, non dans l'extraction : c'est la
/// ressource de police (`/F2`) et les octets de couleur qu'il faudra
/// réémettre pour que le gras reste gras et le rouge rouge.
fn styles_of(target: &Target, sources: &[Option<&Glyph>], base: f64) -> Styles {
    let index = super::GlyphIndex::new(&target.scan);
    let mut styles = Styles::default();
    let mut per_char = Vec::with_capacity(sources.len());
    let mut last = 0_u16;
    for source in sources {
        let id = source
            .and_then(|glyph| index.find(&target.scan, glyph))
            .and_then(|found| {
                let op = target.scan.glyphs[found].site.op;
                let state = &target.scan.ops.get(op)?.after;
                let font = state.font.clone()?;
                Some(Run {
                    font,
                    size: state.size,
                    page_size: state.size * target.scale,
                    fill: state.fill.restore(),
                    color: source.map_or([0.0, 0.0, 0.0], |g| g.color),
                })
            })
            .map(|run| styles.intern(run));
        // Un blanc ajouté entre deux mots n'a pas de glyphe : il prend le
        // style de ce qui le précède.
        let id = id.unwrap_or(last);
        last = id;
        per_char.push(id);
    }
    styles.per_char = per_char;
    styles.base = base;
    styles
}

/// Bloc réduit à une seule ligne de la page.
///
/// La ligne est le plus petit morceau qu'on sache recomposer seul : elle a sa
/// boîte, sa ligne de base et ses mots.
#[must_use]
pub fn line_unit(text: &PageText, line_index: usize) -> Option<TextUnit> {
    let line = text.lines.get(line_index)?;
    if line.words.is_empty() {
        return None;
    }
    let size = line
        .words
        .iter()
        .flat_map(|w| w.glyphs.iter())
        .map(|g| g.size)
        .fold(0.0_f64, f64::max);
    Some(TextUnit {
        bbox: line.bbox,
        pieces: vec![Piece {
            line: line_index,
            words: (0, line.words.len()),
        }],
        alignment: Alignment::Left,
        first_line_indent: 0.0,
        line_spacing: size * 1.2,
        size,
    })
}

/// Ligne de la page sous un point, la plus proche verticalement.
#[must_use]
pub fn line_at(text: &PageText, x: f64, y: f64) -> Option<usize> {
    text.lines
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            let m = 2.0;
            x >= l.bbox.x0 - m && x <= l.bbox.x1 + m && y >= l.bbox.y0 - m && y <= l.bbox.y1 + m
        })
        .min_by(|(_, a), (_, b)| {
            let d = |r: &Rect| (f64::midpoint(r.y0, r.y1) - y).abs();
            d(&a.bbox).total_cmp(&d(&b.bbox))
        })
        .map(|(i, _)| i)
}

/// Donne un nouveau texte au paragraphe logé dans `frame`, et rend la
/// position de chaque caractère **tel qu'il vient d'être écrit**.
///
/// `expected` est ce qui est dessiné aujourd'hui, sans blancs : le paragraphe
/// n'est retrouvé que s'il porte exactement ce texte. C'est la sûreté de
/// l'édition en direct — si l'extraction avait fusionné le paragraphe avec
/// son voisin, le recomposer écraserait le voisin ; on refuse plutôt.
///
/// Un paragraphe vide, ou une zone de texte neuve, n'a rien à retrouver :
/// son texte est alors ajouté à la page, dans la boîte.
///
/// # Errors
/// Paragraphe introuvable alors qu'il porte du texte, ou recomposition
/// impossible.
pub fn set_paragraph_text(
    doc: &Document,
    page: &Page,
    frame: &ParagraphFrame,
    expected: &str,
    new_text: &str,
) -> Result<CaretMap> {
    move_paragraph(doc, page, frame, frame, expected, new_text)
}

/// Comme [`move_paragraph`], en **rendant à chaque caractère son style** :
/// sa police, son corps et sa couleur.
///
/// C'est ce qui permet de modifier une ligne où se mêlent du gras, de
/// l'italique et un lien sans tout aplatir d'une seule police. Les styles se
/// relèvent à l'ouverture ([`OpenedParagraph::styles`]) et se reportent sur
/// le texte modifié ([`Styles::carry`]).
///
/// # Errors
/// Bloc introuvable, ou recomposition impossible.
// Le document, la page, deux boîtes, deux textes et les styles : sept
// données distinctes, qu'un regroupement artificiel n'éclaircirait pas.
#[allow(clippy::too_many_arguments)]
pub fn move_paragraph_styled(
    doc: &Document,
    page: &Page,
    from: &ParagraphFrame,
    to: &ParagraphFrame,
    expected: &str,
    new_text: &str,
    styles: Option<&Styles>,
) -> Result<CaretMap> {
    move_styled(doc, page, from, to, expected, new_text, styles)
}

/// Réécrit un bloc **dans une autre boîte** : même texte, autre place, autre
/// largeur ou autre corps.
///
/// C'est ce qui permet de déplacer un bloc ou de le redimensionner : la boîte
/// de départ sert à le retrouver, celle d'arrivée à l'écrire. Le texte s'y
/// recompose, donc élargir la boîte reflue les lignes au lieu d'étirer les
/// lettres.
///
/// # Errors
/// Bloc introuvable, ou recomposition impossible.
pub fn move_paragraph(
    doc: &Document,
    page: &Page,
    from: &ParagraphFrame,
    to: &ParagraphFrame,
    expected: &str,
    new_text: &str,
) -> Result<CaretMap> {
    move_styled(doc, page, from, to, expected, new_text, None)
}

/// Corps commun de [`move_paragraph`] et [`move_paragraph_styled`].
#[allow(clippy::too_many_arguments)] // le document, deux boîtes, deux textes, le style
fn move_styled(
    doc: &Document,
    page: &Page,
    from: &ParagraphFrame,
    to: &ParagraphFrame,
    expected: &str,
    new_text: &str,
    styles: Option<&Styles>,
) -> Result<CaretMap> {
    let text = extract_page_text(doc, page)?;
    let expected = normalized(expected);
    if expected.is_empty() {
        if normalized(new_text).is_empty() {
            // Rien sur la page, rien de visible à y mettre : on calcule
            // seulement où tomberait le curseur, sans écrire. Des blancs seuls
            // n'ont pas de glyphe à retrouver ; les écrire ferait un bloc
            // invisible de plus à chaque frappe.
            let chars: Vec<char> = new_text.chars().collect();
            let laid = lay_out_with(&chars, &Geometry::from(to), |_, c| {
                // Aucun glyphe à mesurer ici : une largeur moyenne suffit à
                // placer le curseur d'une zone encore vide.
                if c == ' ' {
                    0.28 * to.size
                } else {
                    0.5 * to.size
                }
            });
            return Ok(caret_from_layout(&laid, to.size));
        }
        return append_block(doc, page, to, new_text);
    }
    // Un bloc déjà écrit par nous porte son ancre : on le retrouve par elle,
    // sans rien demander à l'extraction. C'est ce qui permet de continuer à
    // taper quand le bloc s'est mis à toucher son voisin.
    if let Some(target) = Target::by_tag(doc, page, &frame_tag(from))? {
        let unit = TextUnit {
            bbox: Rect::new(
                from.x0,
                from.baseline - from.size,
                from.x0 + from.width,
                from.baseline + from.size,
            ),
            pieces: Vec::new(),
            alignment: to.alignment,
            first_line_indent: to.first_line_indent,
            line_spacing: to.line_spacing,
            size: to.size,
        };
        let laid = write_to(
            doc,
            page,
            &unit,
            new_text,
            Some(to),
            &ReflowOptions::default(),
            &target,
            styles,
        )?;
        return Ok(caret_from_layout(&laid, to.size));
    }
    let unit = locate(&text, from, &expected).ok_or_else(|| {
        Error::Unsupported(
            "le paragraphe ne se retrouve plus seul sur la page : il touche un autre texte".into(),
        )
    })?;
    let laid = write_paragraph(
        doc,
        page,
        &text,
        &unit,
        new_text,
        Some(to),
        &ReflowOptions::default(),
        styles,
    )?;
    Ok(caret_from_layout(&laid, to.size))
}

/// Prépare une zone de texte neuve dont la première ligne de base passe par
/// `(x, baseline)`.
///
/// La police sera une des quatorze polices standard, ajoutée aux ressources
/// de la page **à la première lettre tapée** : elle s'affiche partout et
/// couvre tout le français. Rien n'est écrit ici — une zone ouverte puis
/// abandonnée ne laisse aucune trace dans le fichier.
#[must_use]
pub fn new_text_frame(
    doc: &Document,
    page: &Page,
    x: f64,
    baseline: f64,
    size: f64,
    color: [f64; 3],
) -> ParagraphFrame {
    let crop = page.crop_box(doc);
    // La zone s'étend jusqu'à la marge droite : elle grandit en largeur tant
    // qu'on tape, puis coule à la ligne plutôt que de sortir de la page.
    let width = (crop.x1 - 18.0 - x).max(size * 4.0);
    ParagraphFrame {
        x0: x,
        width,
        alignment: Alignment::Left,
        first_line_indent: 0.0,
        line_spacing: size * 1.2,
        baseline,
        size,
        font: Name::new("Helv"),
        standard: Some("Helvetica".into()),
        color,
        ink: None,
        face: None,
        rotation: 0.0,
    }
}

/// Style d'une zone de texte neuve imposé par l'utilisateur ; ce qui n'est
/// pas imposé vient du texte voisin.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NewTextStyle {
    /// Corps, en points.
    pub size: Option<f64>,
    /// Couleur RVB.
    pub color: Option<[f64; 3]>,
}

/// Prépare une zone de texte neuve **là où l'on clique**, comme dans
/// Acrobat : elle écrit dans la police, le corps et la couleur du texte le
/// plus proche — une valeur ajoutée à un formulaire imprimé ressemble ainsi
/// au reste de la page, au lieu d'arriver en Helvetica.
///
/// Placement : le clic tombe au milieu des lettres (c'est là qu'on vise
/// quand on montre une ligne). Un clic sur une ligne de champ, ou juste
/// au-dessus (« Intitulé du compte : ______ »), pose le texte **sur** ce
/// trait.
///
/// Rien n'est écrit ici.
#[must_use]
pub fn text_frame_at(
    doc: &Document,
    page: &Page,
    text: &PageText,
    x: f64,
    y: f64,
    style: NewTextStyle,
) -> ParagraphFrame {
    let near = nearest_style(doc, page, text, x, y);
    let size = style
        .size
        .or(near.as_ref().map(|n| n.1))
        .unwrap_or(12.0)
        .clamp(4.0, 144.0);
    let color = style
        .color
        .or(near.as_ref().map(|n| n.2))
        .unwrap_or([0.0, 0.0, 0.0]);
    let (x, baseline) = field_line(doc, page, x, y, size).unwrap_or((x, y - size * 0.33));
    let mut frame = new_text_frame(doc, page, x, baseline, size, color);
    if let Some((font, _, _)) = near {
        frame.font = font;
        frame.standard = None;
    }
    frame
}

/// Police (ressource de la page), corps et couleur du bloc le plus proche,
/// s'il est modifiable et pas trop loin.
fn nearest_style(
    doc: &Document,
    page: &Page,
    text: &PageText,
    x: f64,
    y: f64,
) -> Option<(Name, f64, [f64; 3])> {
    let units = text_units(text);
    let distance = |r: &Rect| {
        let dx = (r.x0 - x).max(x - r.x1).max(0.0);
        let dy = (r.y0 - y).max(y - r.y1).max(0.0);
        // Le texte de la même ligne compte plus que celui du dessus.
        dx.hypot(dy * 2.0)
    };
    let mut order: Vec<(f64, &TextUnit)> = units
        .iter()
        .map(|u| (distance(&u.bbox), u))
        .filter(|(d, _)| *d < 200.0)
        .collect();
    order.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Le premier bloc dont la police se retrouve dans le flux de la page ;
    // quelques essais suffisent.
    order.iter().take(4).find_map(|(_, unit)| {
        let target = Target::find(doc, page, text, unit).ok()?;
        let size = target.size_text * target.scale;
        let color = piece_words(unit.pieces.first()?, text)
            .first()
            .and_then(|w| w.glyphs.first())
            .map_or([0.0, 0.0, 0.0], |g| g.color.map(f64::from));
        (size > 1.0).then_some((target.font, size, color))
    })
}

/// Trait de champ sous le clic : origine et ligne de base du texte à y poser.
fn field_line(doc: &Document, page: &Page, x: f64, y: f64, size: f64) -> Option<(f64, f64)> {
    let objects = crate::edit_objects::list(doc, page).ok()?;
    objects
        .iter()
        .filter(|o| o.kind == crate::edit_objects::Kind::Path)
        .map(|o| o.bbox)
        .filter(|b| {
            b.height() <= 2.5
                && b.width() >= size * 2.0
                && x >= b.x0 - size
                && x <= b.x1
                && y >= b.y0 - size * 0.5
                && y <= b.y1 + size * 1.3
        })
        .min_by(|a, b| (y - a.y1).abs().total_cmp(&(y - b.y1).abs()))
        .map(|b| (x.max(b.x0 + 1.5), b.y1 + size * 0.22))
}

/// Texte sans blancs, pour comparer ce qui est dessiné à ce qui a été écrit.
#[must_use]
pub fn normalized(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

// ---------------------------------------------------------------------------
// Blocs éditables.
// ---------------------------------------------------------------------------

/// Un morceau de ligne : les mots `words` de la ligne `line`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    /// Indice de la ligne dans `PageText::lines`.
    pub line: usize,
    /// Mots de la ligne, `[début, fin)`.
    pub words: (usize, usize),
}

/// Un bloc de texte **d'un seul tenant** : des morceaux de lignes empilés
/// dans une même colonne.
///
/// Le paragraphe de l'extraction est pensé pour la lecture, et il a raison
/// d'enjamber les colonnes : un paragraphe qui se poursuit en haut de la
/// colonne suivante reste une seule idée. Mais on ne l'édite pas d'un bloc —
/// le recomposer dans une boîte large de deux colonnes coulerait le texte
/// par-dessus la seconde. De même, un en-tête qui porte un titre à gauche et
/// une date à droite, sur la même ligne, fait deux zones et non une. Acrobat
/// les découpe ainsi ; ce type est ce découpage.
#[derive(Debug, Clone, PartialEq)]
pub struct TextUnit {
    /// Boîte englobante, en espace de page.
    pub bbox: Rect,
    /// Morceaux, de haut en bas : un par ligne visuelle.
    pub pieces: Vec<Piece>,
    /// Alignement.
    pub alignment: Alignment,
    /// Retrait de la première ligne.
    pub first_line_indent: f64,
    /// Interligne.
    pub line_spacing: f64,
    /// Corps dominant.
    pub size: f64,
}

/// Blocs éditables de la page, dans l'ordre de lecture. C'est cet ordre que
/// numérotent [`open_paragraph`] et le visualiseur.
#[must_use]
pub fn text_units(text: &PageText) -> Vec<TextUnit> {
    let mut out = Vec::new();
    for para in text.blocks.iter().flat_map(|b| b.paragraphs.iter()) {
        split_paragraph(para, text, &mut out);
    }
    out
}

/// Le paragraphe entier comme un seul bloc : c'est ce que fait
/// [`reflow_paragraph`], qui ne découpe rien.
fn unit_of_paragraph(para: &Paragraph, text: &PageText) -> TextUnit {
    TextUnit {
        bbox: para.bbox,
        pieces: para
            .lines
            .iter()
            .filter_map(|l| {
                text.lines.get(*l).map(|line| Piece {
                    line: *l,
                    words: (0, line.words.len()),
                })
            })
            .collect(),
        alignment: para.alignment,
        first_line_indent: para.first_line_indent,
        line_spacing: para.line_spacing,
        size: para.size,
    }
}

/// Découpe un paragraphe en blocs d'un seul tenant.
fn split_paragraph(para: &Paragraph, text: &PageText, out: &mut Vec<TextUnit>) {
    let size = para.size.max(1.0);
    // 1. Chaque ligne se coupe à ses grands blancs : au-delà de trois
    //    cadratins, ce n'est plus une espace entre deux mots mais une
    //    tabulation entre deux zones.
    let mut segments: Vec<(Piece, Rect, f64)> = Vec::new();
    for &li in &para.lines {
        let Some(line) = text.lines.get(li) else {
            continue;
        };
        let baseline = line.baseline();
        let mut start = 0;
        for i in 1..=line.words.len() {
            let cut = i == line.words.len()
                || line.words[i].bbox.x0 - line.words[i - 1].bbox.x1 > size * 3.0;
            if cut && i > start {
                let bbox = line.words[start..i]
                    .iter()
                    .map(|w| w.bbox)
                    .reduce(|a, b| {
                        Rect::new(
                            a.x0.min(b.x0),
                            a.y0.min(b.y0),
                            a.x1.max(b.x1),
                            a.y1.max(b.y1),
                        )
                    })
                    .unwrap_or(line.bbox);
                segments.push((
                    Piece {
                        line: li,
                        words: (start, i),
                    },
                    bbox,
                    baseline,
                ));
                start = i;
            }
        }
    }
    // 2. Les morceaux s'empilent en blocs : un morceau rejoint le bloc dont
    //    le dernier morceau est juste au-dessus de lui et le chevauche en
    //    largeur. Sinon il en ouvre un autre — autre colonne, autre zone.
    let spacing = if para.line_spacing > 0.1 {
        para.line_spacing
    } else {
        size * 1.2
    };
    let mut units: Vec<(Vec<Piece>, Rect, f64)> = Vec::new(); // morceaux, boîte, dernière ligne de base
    for (piece, bbox, baseline) in segments {
        let fits = units.iter_mut().rev().find(|(_, last_box, last_base)| {
            let drop = *last_base - baseline;
            let overlap = bbox.x1.min(last_box.x1) - bbox.x0.max(last_box.x0);
            drop > size * 0.3
                && drop < spacing * 1.8 + size * 0.2
                && overlap > bbox.width().min(last_box.width()) * 0.3
        });
        match fits {
            Some((pieces, last_box, last_base)) => {
                pieces.push(piece);
                *last_box = bbox;
                *last_base = baseline;
            }
            None => units.push((vec![piece], bbox, baseline)),
        }
    }
    let whole = units.len() == 1;
    for (index, (pieces, _, _)) in units.into_iter().enumerate() {
        let bbox = pieces
            .iter()
            .filter_map(|p| piece_box(p, text))
            .reduce(|a, b| {
                Rect::new(
                    a.x0.min(b.x0),
                    a.y0.min(b.y0),
                    a.x1.max(b.x1),
                    a.y1.max(b.y1),
                )
            })
            .unwrap_or(para.bbox);
        // Un bloc qui n'est qu'une partie d'une ligne prend l'alignement que
        // sa place lui donne : collé à droite du paragraphe, il s'aligne à
        // droite, et grandira donc vers la gauche.
        let alignment = if whole || pieces.len() > 1 {
            para.alignment
        } else if (bbox.x1 - para.bbox.x1).abs() < 2.0 && bbox.x0 - para.bbox.x0 > size {
            Alignment::Right
        } else {
            Alignment::Left
        };
        out.push(TextUnit {
            bbox,
            alignment,
            // Seule la toute première partie garde le retrait : les suivantes
            // continuent le paragraphe, elles ne le commencent pas.
            first_line_indent: if index == 0 {
                para.first_line_indent
            } else {
                0.0
            },
            line_spacing: if pieces.len() > 1 {
                para.line_spacing
            } else {
                0.0
            },
            size: para.size,
            pieces,
        });
    }
}

/// Boîte d'un morceau de ligne.
fn piece_box(piece: &Piece, text: &PageText) -> Option<Rect> {
    let line = text.lines.get(piece.line)?;
    line.words
        .get(piece.words.0..piece.words.1)?
        .iter()
        .map(|w| w.bbox)
        .reduce(|a, b| {
            Rect::new(
                a.x0.min(b.x0),
                a.y0.min(b.y0),
                a.x1.max(b.x1),
                a.y1.max(b.y1),
            )
        })
}

/// Mots d'un morceau de ligne.
fn piece_words<'a>(piece: &Piece, text: &'a PageText) -> &'a [crate::text::Word] {
    text.lines
        .get(piece.line)
        .and_then(|l| l.words.get(piece.words.0..piece.words.1))
        .unwrap_or(&[])
}

/// Texte réellement dessiné par un bloc, glyphe après glyphe.
fn glyph_text(unit: &TextUnit, text: &PageText) -> String {
    unit.pieces
        .iter()
        .flat_map(|p| piece_words(p, text).iter())
        .flat_map(|w| w.glyphs.iter())
        .map(|g| g.text.as_str())
        .collect()
}

/// Ligne de base du premier morceau d'un bloc.
fn first_baseline(unit: &TextUnit, text: &PageText) -> f64 {
    unit.pieces
        .first()
        .and_then(|p| text.lines.get(p.line))
        .map_or(unit.bbox.y0, crate::text::Line::baseline)
}

/// Balise de contenu marqué qui ancre un bloc écrit par nous.
///
/// Elle se déduit de la **boîte**, qui est figée pendant toute une édition :
/// pas besoin de la retenir ailleurs, et deux blocs différents ne partagent
/// pas la leur. C'est ce qui permet de retrouver le bloc même quand le texte
/// qu'il porte se mêle, à l'extraction, à ce qui l'entoure — une ligne qui
/// déborde sur celle du dessous, par exemple.
fn frame_tag(frame: &ParagraphFrame) -> Name {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let key = ((frame.x0 * 4.0).round() as i64 & 0xFFFF) as u64
        | ((((frame.baseline * 4.0).round() as i64) & 0xFFFF) as u64) << 16;
    Name::new(&format!("Acrux{key:08X}"))
}

/// Glyphes d'un flux portant une balise donnée.
fn sites_by_tag(scan: &scan::Scan, tag: &Name) -> Vec<usize> {
    scan.glyphs
        .iter()
        .enumerate()
        .filter(|(_, g)| g.site.tag.as_ref() == Some(tag))
        .map(|(i, _)| i)
        .collect()
}

/// Retrouve le paragraphe logé dans une boîte et portant un texte donné.
///
/// Si aucun bloc entier ne convient, on regarde **la part de chaque bloc qui
/// tombe dans la boîte** : une zone ajoutée juste après un libellé
/// (« IBAN : » puis la saisie, sur la même ligne) est lue par l'extraction
/// comme une seule ligne avec lui — c'est pourtant bien la zone seule qu'on
/// recompose, le libellé restant à sa place.
fn locate(text: &PageText, frame: &ParagraphFrame, expected: &str) -> Option<TextUnit> {
    let units = text_units(text);
    locate_whole(&units, text, frame, expected).or_else(|| locate_run(text, frame, expected))
}

/// Suite de mots qui porte **exactement** le texte attendu, à partir de la
/// ligne de base du cadre.
///
/// C'est le rattrapage quand aucun bloc entier ne convient : une zone ajoutée
/// dans une page déjà pleine se retrouve mêlée, à l'extraction, au texte qui
/// l'entoure — un libellé à gauche, un filigrane par-dessus, une autre
/// colonne à droite. On ne cherche donc pas un bloc : on lit les mots depuis
/// le cadre, ligne par ligne vers le bas, et l'on s'arrête dès que ce qu'on a
/// lu fait le texte attendu. Ce qui n'est pas dans la largeur du cadre est
/// ignoré, ce qui diverge arrête la recherche.
fn locate_run(text: &PageText, frame: &ParagraphFrame, expected: &str) -> Option<TextUnit> {
    if expected.is_empty() {
        return None;
    }
    let tol = frame.size * 0.35;
    let (left, right) = (frame.x0 - frame.size * 0.3, frame.x0 + frame.width + 1.0);
    // Les lignes du cadre et celles d'en dessous, de haut en bas.
    let mut lines: Vec<usize> = (0..text.lines.len())
        .filter(|&i| {
            let line = &text.lines[i];
            !line.words.is_empty() && line.baseline() <= frame.baseline + tol
        })
        .collect();
    lines.sort_by(|&a, &b| {
        text.lines[b]
            .baseline()
            .total_cmp(&text.lines[a].baseline())
    });
    // La première ligne lue doit être celle du cadre.
    let first = *lines.first()?;
    if (text.lines[first].baseline() - frame.baseline).abs() > tol {
        return None;
    }
    let mut pieces: Vec<Piece> = Vec::new();
    let mut read = String::new();
    for index in lines {
        let line = &text.lines[index];
        let mut range: Option<(usize, usize)> = None;
        for (wi, word) in line.words.iter().enumerate() {
            if word.bbox.x0 < left || word.bbox.x0 > right {
                // Hors du cadre : avant le premier mot lu on passe, après on
                // s'arrête (le reste de la ligne appartient à un voisin).
                if range.is_some() {
                    break;
                }
                continue;
            }
            let glyphs: String = word.glyphs.iter().map(|g| g.text.as_str()).collect();
            let grown = format!("{read}{}", normalized(&glyphs));
            if !expected.starts_with(&grown) {
                break;
            }
            read = grown;
            range = Some(match range {
                Some((a, _)) => (a, wi + 1),
                None => (wi, wi + 1),
            });
            if read == expected {
                break;
            }
        }
        if let Some((a, b)) = range {
            pieces.push(Piece {
                line: index,
                words: (a, b),
            });
        }
        if read == expected {
            break;
        }
        // Une ligne vide de notre texte ne coupe pas forcément le bloc : sur
        // une page à deux colonnes, une ligne de la colonne voisine peut
        // s'intercaler. C'est l'**écart vertical** qui tranche — au-delà de
        // deux interlignes, on n'est plus dans le même bloc.
        if !pieces.is_empty() {
            let last = pieces
                .last()
                .and_then(|p| text.lines.get(p.line))
                .map_or(frame.baseline, Line::baseline);
            let gap = last - text.lines[index].baseline();
            if gap > 2.2 * frame.line_spacing.max(frame.size) {
                break;
            }
        }
    }
    if read != expected || pieces.is_empty() {
        return None;
    }
    let bbox = pieces
        .iter()
        .filter_map(|p| piece_box(p, text))
        .reduce(|a, b| a.union(&b))?;
    Some(TextUnit {
        bbox,
        pieces,
        alignment: frame.alignment,
        first_line_indent: frame.first_line_indent,
        line_spacing: frame.line_spacing,
        size: frame.size,
    })
}

/// Bloc entier logé dans une boîte et portant un texte donné.
fn locate_whole(
    units: &[TextUnit],
    text: &PageText,
    frame: &ParagraphFrame,
    expected: &str,
) -> Option<TextUnit> {
    // Un bloc en biais ne se repère pas à sa ligne de base : sa boîte
    // englobante ne dit rien de son origine. C'est son texte qui le désigne,
    // et sa boîte qui départage deux homonymes.
    if frame.rotation.abs() > 1e-4 {
        return units
            .iter()
            .filter(|u| normalized(&glyph_text(u, text)) == expected)
            .min_by(|a, b| {
                let d = |u: &TextUnit| {
                    (f64::midpoint(u.bbox.x0, u.bbox.x1) - frame.x0)
                        .hypot(f64::midpoint(u.bbox.y0, u.bbox.y1) - frame.baseline)
                };
                d(a).total_cmp(&d(b))
            })
            .cloned();
    }
    units
        .iter()
        .filter(|u| {
            (first_baseline(u, text) - frame.baseline).abs() <= frame.size * 0.35
                && u.bbox.x1 >= frame.x0 - 1.0
                && u.bbox.x0 <= frame.x0 + frame.width + 1.0
                && normalized(&glyph_text(u, text)) == expected
        })
        .min_by(|a, b| {
            (first_baseline(a, text) - frame.baseline)
                .abs()
                .total_cmp(&(first_baseline(b, text) - frame.baseline).abs())
        })
        .cloned()
}

/// Boîte d'un paragraphe existant.
///
/// Une ligne seule n'a pas de largeur « voulue » : c'est un titre, une
/// légende, un champ recopié. On la laisse donc grandir jusqu'à la marge, du
/// côté où elle s'ouvre, au lieu de la faire couler dès le premier mot ajouté.
fn frame_of(
    para: &TextUnit,
    text: &PageText,
    size: f64,
    font: Name,
    crop: &Rect,
    slant: Option<(f64, f64, f64)>,
) -> ParagraphFrame {
    // Un bloc en biais se décrit par son **origine** et sa direction : sa
    // boîte englobante, elle, ne dit rien de sa mise en page.
    if let Some((rotation, x0, baseline)) = slant {
        // Un filigrane ne coule pas : il tient sur sa ligne, dans sa
        // direction. On lui laisse donc toute la diagonale de la page, et on
        // le pose au fil du texte, sans retrait ni centrage — son origine
        // **est** son début.
        let width = crop.width().hypot(crop.height()).max(size);
        return ParagraphFrame {
            x0,
            width,
            alignment: Alignment::Left,
            first_line_indent: 0.0,
            line_spacing: if para.line_spacing > 0.1 {
                para.line_spacing
            } else {
                size * 1.2
            },
            baseline,
            size,
            font,
            standard: None,
            color: unit_color(para, text),
            ink: None,
            face: None,
            rotation,
        };
    }
    let baseline = first_baseline(para, text);
    let spacing = if para.line_spacing > 0.1 {
        para.line_spacing
    } else {
        size * 1.2
    };
    let (x0, width) = if para.pieces.len() > 1 {
        (para.bbox.x0, para.bbox.width())
    } else {
        let (left, right) = (crop.x0 + 18.0, crop.x1 - 18.0);
        match para.alignment {
            Alignment::Right => (left, (para.bbox.x1 - left).max(para.bbox.width())),
            Alignment::Center => {
                let c = f64::midpoint(para.bbox.x0, para.bbox.x1);
                let half = (c - left).min(right - c).max(para.bbox.width() / 2.0);
                (c - half, half * 2.0)
            }
            _ => (para.bbox.x0, (right - para.bbox.x0).max(para.bbox.width())),
        }
    };
    ParagraphFrame {
        x0,
        width,
        alignment: para.alignment,
        first_line_indent: para.first_line_indent,
        line_spacing: spacing,
        baseline,
        size,
        font,
        standard: None,
        color: unit_color(para, text),
        ink: None,
        face: None,
        rotation: 0.0,
    }
}

/// Encre d'un bloc : celle de son premier glyphe dessiné.
///
/// C'est ce qui permet de **redessiner** le bloc à l'identique pendant qu'on
/// tape, sans repasser par le document.
fn unit_color(para: &TextUnit, text: &PageText) -> [f64; 3] {
    para.pieces
        .iter()
        .flat_map(|piece| piece_words(piece, text))
        .flat_map(|w| w.glyphs.iter())
        .find(|g| !g.is_space)
        .map_or([0.0, 0.0, 0.0], |g| {
            [
                f64::from(g.color[0]),
                f64::from(g.color[1]),
                f64::from(g.color[2]),
            ]
        })
}

// Une ligne, ses mots, ses glyphes et ses césures : la boucle se lit d'un
// trait, la couper en morceaux la rendrait plus obscure.
#[allow(clippy::too_many_lines)]
/// Texte éditable et positions des frontières, lus sur les glyphes dessinés.
///
/// Les mots d'une ligne sont séparés par une espace, et les lignes aussi —
/// sauf une césure (mot coupé par un trait d'union en fin de ligne, suite en
/// minuscule) : le trait d'union disparaît et le mot se recolle, sans quoi la
/// première frappe écrirait « exem- ple » au milieu d'une ligne.
fn caret_from_glyphs<'a>(
    para: &TextUnit,
    text: &'a PageText,
    size: f64,
) -> (String, CaretMap, Vec<Option<&'a Glyph>>) {
    let mut out = String::new();
    // Le glyphe d'où vient chaque caractère : c'est par lui qu'on retrouvera
    // sa police et sa couleur. Les blancs ajoutés entre mots n'en ont pas.
    let mut sources: Vec<Option<&Glyph>> = Vec::new();
    let mut stops: Vec<CaretStop> = Vec::new();
    let mut lines: Vec<CaretLine> = Vec::new();
    let line_refs: Vec<(f64, &[crate::text::Word])> = para
        .pieces
        .iter()
        .filter_map(|p| {
            let line = text.lines.get(p.line)?;
            Some((line.baseline(), piece_words(p, text)))
        })
        .filter(|(_, words)| !words.is_empty())
        .collect();
    for (li, (baseline, words)) in line_refs.iter().enumerate() {
        let baseline = *baseline;
        let first = stops.len();
        let mut end_x = words.first().map_or(para.bbox.x0, |w| w.bbox.x0);
        let next = line_refs.get(li + 1);
        let hyphenated = next.is_some_and(|(_, n)| {
            let ends = words
                .last()
                .and_then(|w| w.glyphs.last())
                .is_some_and(|g| g.text == "-" || g.text == "\u{2010}");
            let lower = n
                .first()
                .and_then(|w| w.text.chars().next())
                .is_some_and(char::is_lowercase);
            ends && lower
        });
        let word_count = words.len();
        for (wi, word) in words.iter().enumerate() {
            if wi > 0 {
                // L'espace commence là où le mot précédent finit.
                stops.push(CaretStop {
                    x: end_x,
                    baseline,
                    line: li,
                });
                out.push(' ');
                sources.push(None);
            }
            let glyph_count = word.glyphs.len();
            for (gi, glyph) in word.glyphs.iter().enumerate() {
                if hyphenated && wi + 1 == word_count && gi + 1 == glyph_count {
                    // Trait d'union de césure : il ne fait pas partie du texte.
                    continue;
                }
                let chars: Vec<char> = glyph.text.chars().collect();
                let n = chars.len().max(1);
                #[allow(clippy::cast_precision_loss)] // une ligature compte peu de lettres
                for (k, c) in chars.iter().enumerate() {
                    let x = glyph.bbox.x0 + glyph.bbox.width() * k as f64 / n as f64;
                    stops.push(CaretStop {
                        x,
                        baseline,
                        line: li,
                    });
                    out.push(*c);
                    sources.push(Some(glyph));
                }
                end_x = glyph.bbox.x1;
            }
        }
        if next.is_some() {
            if !hyphenated {
                // L'espace qui joint deux lignes : son curseur est en fin de
                // ligne, celui du caractère suivant en début de la suivante.
                stops.push(CaretStop {
                    x: end_x,
                    baseline,
                    line: li,
                });
                out.push(' ');
                sources.push(None);
            }
            lines.push(CaretLine {
                first,
                last: stops.len().saturating_sub(1).max(first),
                baseline,
            });
        } else {
            stops.push(CaretStop {
                x: end_x,
                baseline,
                line: li,
            });
            lines.push(CaretLine {
                first,
                last: stops.len() - 1,
                baseline,
            });
        }
    }
    if stops.is_empty() {
        stops.push(CaretStop {
            x: para.bbox.x0,
            baseline: para.bbox.y0,
            line: 0,
        });
        lines.push(CaretLine {
            first: 0,
            last: 0,
            baseline: para.bbox.y0,
        });
    }
    (out, CaretMap { stops, lines, size }, sources)
}

// ---------------------------------------------------------------------------
// Mise en page.
// ---------------------------------------------------------------------------

/// Ce qu'il faut pour poser des lignes.
pub(super) struct Geometry {
    pub(super) x0: f64,
    pub(super) width: f64,
    pub(super) alignment: Alignment,
    pub(super) indent: f64,
    pub(super) spacing: f64,
    pub(super) baseline: f64,
    /// Corps en points de page.
    pub(super) size: f64,
}

impl From<&ParagraphFrame> for Geometry {
    fn from(f: &ParagraphFrame) -> Self {
        Self {
            x0: f.x0,
            width: f.width,
            alignment: f.alignment,
            indent: f.first_line_indent,
            spacing: f.line_spacing,
            baseline: f.baseline,
            size: f.size,
        }
    }
}

/// Une ligne posée.
#[derive(Debug, Clone)]
pub(super) struct LaidLine {
    /// Premier caractère de la ligne.
    pub(super) start: usize,
    /// Caractère suivant le dernier de la ligne, blancs de fin compris.
    pub(super) end: usize,
    /// Fin de ce qui est dessiné (blancs de fin et retour à la ligne exclus).
    pub(super) draw_end: usize,
    /// Début de la ligne.
    pub(super) x: f64,
    /// Ligne de base.
    pub(super) baseline: f64,
    /// Espace ajouté à chaque blanc pour justifier (points de page).
    pub(super) gap: f64,
    /// Abscisse de chaque frontière, de `start` à `end` inclus.
    pub(super) stops: Vec<f64>,
}

/// Coupe un texte en lignes et calcule la place de chaque caractère.
pub(super) fn lay_out(prepared: &encode::Prepared, chars: &[char], g: &Geometry) -> Vec<LaidLine> {
    lay_out_with(chars, g, |_, c| prepared.width(&c.to_string()) * g.size)
}

// La découpe en lignes, la justification et les frontières : trois passes
// sur la même donnée, qu'on suit mieux ensemble que séparées.
#[allow(clippy::too_many_lines)]
/// Même découpe, avec une mesure d'avance quelconque **en points de page**,
/// donnée pour chaque caractère : c'est ce qui permet à un mot en gras de
/// mesurer ce qu'il mesure vraiment, et ce qui rend la découpe testable sans
/// document ni police.
pub(super) fn lay_out_with(
    chars: &[char],
    g: &Geometry,
    advance: impl Fn(usize, char) -> f64,
) -> Vec<LaidLine> {
    let widths: Vec<f64> = chars
        .iter()
        .enumerate()
        .map(|(i, c)| if *c == '\n' { 0.0 } else { advance(i, *c) })
        .collect();
    // 1. Coupure : aux retours à la ligne tapés, et aux espaces quand la
    //    ligne est pleine ; au caractère pour un mot plus large que la boîte.
    let mut ranges: Vec<(usize, usize, usize, bool)> = Vec::new(); // début, fin, fin dessinée, coupure dure
    let mut start = 0;
    while start <= chars.len() {
        let available = if ranges.is_empty() {
            (g.width - g.indent).max(1.0)
        } else {
            g.width.max(1.0)
        };
        let mut width = 0.0;
        let mut last_space: Option<usize> = None;
        let mut i = start;
        let mut hard = false;
        let mut cut: Option<(usize, usize)> = None; // (fin dessinée, reprise)
        while i < chars.len() {
            let c = chars[i];
            if c == '\n' {
                hard = true;
                cut = Some((i, i + 1));
                break;
            }
            if c == ' ' {
                last_space = Some(i);
            } else if width + widths[i] > available && i > start {
                cut = Some(match last_space {
                    Some(s) if s > start => {
                        // Les blancs qui suivent restent en fin de ligne.
                        let mut resume = s;
                        while resume < chars.len() && chars[resume] == ' ' {
                            resume += 1;
                        }
                        (s, resume)
                    }
                    _ => (i, i),
                });
                break;
            }
            width += widths[i];
            i += 1;
        }
        let (draw_end, resume) = cut.unwrap_or((chars.len(), chars.len()));
        // Blancs de fin : ils appartiennent à la ligne mais ne se dessinent pas.
        let mut draw_end = draw_end;
        while draw_end > start && chars[draw_end - 1] == ' ' {
            draw_end -= 1;
        }
        ranges.push((start, resume, draw_end, hard));
        if cut.is_none() {
            break;
        }
        start = resume;
        if start == chars.len() && !hard {
            break;
        }
    }
    // 2. Placement.
    let count = ranges.len();
    ranges
        .iter()
        .enumerate()
        .map(|(li, &(start, end, draw_end, hard))| {
            let drawn: f64 = widths[start..draw_end].iter().sum();
            let indent = if li == 0 { g.indent } else { 0.0 };
            let last = li + 1 == count || hard;
            let x = match g.alignment {
                Alignment::Right => g.x0 + g.width - drawn,
                Alignment::Center => g.x0 + (g.width - drawn) / 2.0,
                _ => g.x0 + indent,
            };
            let spaces = chars[start..draw_end].iter().filter(|c| **c == ' ').count();
            let extra = g.width - indent - drawn;
            #[allow(clippy::cast_precision_loss)] // quelques dizaines de blancs
            let gap = if g.alignment == Alignment::Justify && !last && spaces > 0 && extra > 0.0 {
                extra / spaces as f64
            } else {
                0.0
            };
            let mut stops = Vec::with_capacity(end - start + 1);
            let mut cursor = x;
            for i in start..end {
                stops.push(cursor);
                cursor += widths[i];
                if i < draw_end && chars[i] == ' ' {
                    cursor += gap;
                }
            }
            stops.push(cursor);
            #[allow(clippy::cast_precision_loss)] // au plus quelques milliers de lignes
            let baseline = g.baseline - li as f64 * g.spacing;
            LaidLine {
                start,
                end,
                draw_end,
                x,
                baseline,
                gap,
                stops,
            }
        })
        .collect()
}

/// Positions des frontières d'après une mise en page.
pub(super) fn caret_from_layout(laid: &[LaidLine], size: f64) -> CaretMap {
    let total = laid.last().map_or(0, |l| l.end);
    let mut stops = vec![
        CaretStop {
            x: 0.0,
            baseline: 0.0,
            line: 0,
        };
        total + 1
    ];
    let mut lines = Vec::with_capacity(laid.len());
    let count = laid.len();
    for (li, line) in laid.iter().enumerate() {
        let last_line = li + 1 == count;
        let upto = if last_line {
            line.end
        } else {
            line.end.saturating_sub(1).max(line.start)
        };
        for (offset, stop) in stops[line.start..=upto].iter_mut().enumerate() {
            if let Some(x) = line.stops.get(offset) {
                *stop = CaretStop {
                    x: *x,
                    baseline: line.baseline,
                    line: li,
                };
            }
        }
        lines.push(CaretLine {
            first: line.start,
            last: upto,
            baseline: line.baseline,
        });
    }
    CaretMap { stops, lines, size }
}

/// Réécrit une opération de dessin **sans les glyphes du bloc**.
///
/// L'opération qui ne dessine que le bloc disparaît ; celle qui dessine
/// aussi autre chose garde cet autre chose, à sa place exacte.
fn cut_out(
    doc: &Document,
    page: &Page,
    t: &Target,
    op: usize,
    warnings: &mut Vec<String>,
) -> Result<Vec<u8>> {
    let ranges = t.cuts.get(&op).cloned().unwrap_or_default();
    let cuts: Vec<Cut> = if ranges.is_empty() {
        vec![Cut {
            from: None,
            to: None,
            insert: None,
        }]
    } else {
        ranges
            .into_iter()
            .map(|(from, to)| Cut {
                from: Some(from),
                to: Some(to),
                insert: None,
            })
            .collect()
    };
    rewrite_show_op(doc, page, &t.scan, op, &cuts, warnings)
}

/// Prépare une police par style du bloc, prête à écrire.
///
/// Une police qui se dérobe ne doit pas faire perdre la frappe : cette
/// tranche s'écrira avec celle du bloc.
fn prepare_runs(
    doc: &Document,
    page: &Page,
    t: &Target,
    styles: Option<&Styles>,
    new_text: &str,
) -> Result<Vec<encode::Prepared>> {
    let Some(styles) = styles else {
        return Ok(Vec::new());
    };
    let mut fonts = Vec::with_capacity(styles.runs.len());
    for run in &styles.runs {
        match encode::prepare_in(doc, page, &t.scan.resources, &run.font, new_text, None) {
            Ok(p) => fonts.push(p),
            Err(_) => fonts.push(encode::prepare_in(
                doc,
                page,
                &t.scan.resources,
                &t.font,
                new_text,
                None,
            )?),
        }
    }
    Ok(fonts)
}

/// Matrice qui pose une ligne, inclinaison comprise.
///
/// Pour un texte droit, la ligne se pose à sa place dans la page. Pour un
/// texte en biais, elle se pose **dans le repère du bloc** : son décalage
/// local tourne avec lui, et le filigrane reste en biais.
fn line_matrix(t: &Target, frame: Option<&ParagraphFrame>, line: &LaidLine) -> Matrix {
    let slant = frame.map_or(0.0, |f| f.rotation);
    if slant.abs() <= 1e-4 {
        return Matrix::new(t.trm.a, t.trm.b, t.trm.c, t.trm.d, line.x, line.baseline);
    }
    let (sin, cos) = slant.sin_cos();
    let origin = frame.map_or((t.trm.e, t.trm.f), |f| (f.x0, f.baseline));
    let x = origin.0 + line.x * cos - line.baseline * sin;
    let y = origin.1 + line.x * sin + line.baseline * cos;
    Matrix::new(t.trm.a, t.trm.b, t.trm.c, t.trm.d, x, y)
}

/// Écrit une ligne en plusieurs tranches, une par style.
///
/// `scale` mène du corps écrit dans `Tf` au corps en points de page : c'est
/// par lui que passe un redimensionnement du bloc.
fn write_runs(
    out: &mut Vec<u8>,
    styles: &Styles,
    fonts: &[encode::Prepared],
    chars: &[char],
    line: &LaidLine,
    scale: f64,
) {
    let mut start = line.start;
    while start < line.draw_end {
        let id = styles.per_char.get(start).copied().unwrap_or(0);
        let mut end = start + 1;
        while end < line.draw_end && styles.per_char.get(end).copied().unwrap_or(0) == id {
            end += 1;
        }
        let index = id as usize;
        if let (Some(font), Some(run)) = (fonts.get(index), styles.runs.get(index)) {
            let _ = write!(
                out_str(out),
                "/{} {} Tf ",
                font.resource.as_str(),
                fmt(run.size)
            );
            out.extend_from_slice(&run.fill);
            out.push(b' ');
            let piece = LaidLine {
                start,
                end,
                draw_end: end,
                x: line.x,
                baseline: line.baseline,
                gap: line.gap,
                stops: Vec::new(),
            };
            write_tj(out, &line_items(font, chars, &piece, run.size * scale));
        }
        start = end;
    }
}

/// Éléments `TJ` d'une ligne posée.
fn line_items(
    prepared: &encode::Prepared,
    chars: &[char],
    line: &LaidLine,
    size: f64,
) -> Vec<Item> {
    let drawn: String = chars[line.start..line.draw_end].iter().collect();
    if line.gap <= 0.0 || size <= 0.0 {
        return vec![Item::Str(prepared.encode(&drawn).bytes)];
    }
    // §9.4.3 : un nombre de `TJ` déplace de −n/1000 × taille.
    let adjust = -line.gap / size * 1000.0;
    let mut items = Vec::new();
    let pieces: Vec<&str> = drawn.split(' ').collect();
    for (i, piece) in pieces.iter().enumerate() {
        let piece = if i + 1 == pieces.len() {
            (*piece).to_string()
        } else {
            format!("{piece} ")
        };
        items.push(Item::Str(prepared.encode(&piece).bytes));
        if i + 1 < pieces.len() {
            items.push(Item::Num(adjust));
        }
    }
    items
}

// ---------------------------------------------------------------------------
// Écriture.
// ---------------------------------------------------------------------------

/// Bloc visé et ce qu'il faut savoir de son dessin.
struct Target {
    scan: scan::Scan,
    /// Inclinaison du texte, en radians.
    rotation: f64,
    by_op: BTreeMap<usize, usize>,
    /// Pour chaque opération touchée, les plages à retirer — vide quand
    /// l'opération ne dessine que notre bloc et s'efface entière.
    ///
    /// C'est ce qui permet de modifier un paragraphe dont l'opération de
    /// dessin porte **aussi** d'autres textes, fût-ce en plein milieu : on
    /// n'y retire que nos glyphes, et les autres restent où ils sont.
    cuts: BTreeMap<usize, Vec<(super::Pos, super::Pos)>>,
    first_op: usize,
    trm: Matrix,
    inverse: Matrix,
    scale: f64,
    font: Name,
    size_text: f64,
    /// Flux à réécrire : celui de la page, ou celui d'un XObject.
    stream: Stream,
}

/// Où vit le texte qu'on modifie.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stream {
    /// Le flux de contenu de la page.
    Page,
    /// Le flux d'un XObject de formulaire.
    Form(acrux_document::ObjectRef),
}

impl Target {
    /// Retrouve le bloc dans le flux qui le dessine.
    ///
    /// Beaucoup de documents — ceux des traitements de texte en particulier —
    /// n'écrivent pas leur texte dans la page mais dans un **XObject de
    /// formulaire** que la page appelle. Refuser de les modifier reviendrait
    /// à refuser une bonne part des fichiers réels ; on rouvre donc ce flux
    /// comme s'il était la page, et c'est lui qu'on réécrit.
    fn find(doc: &Document, page: &Page, text: &PageText, unit: &TextUnit) -> Result<Target> {
        let page_scan = scan::scan(doc, page)?;
        match form_of(&page_scan, text, unit) {
            Some(site) => {
                let scan = scan::scan_form(doc, &site, page)?;
                Self::build(text, unit, scan, Stream::Form(site.reference))
            }
            None => Self::build(text, unit, page_scan, Stream::Page),
        }
    }

    /// Construit la cible dans un flux déjà balayé.
    /// Cible d'un bloc **balisé** : celui qu'une édition précédente a écrit.
    ///
    /// On ne passe plus par l'extraction : les glyphes sont ceux que porte la
    /// balise, quoi qu'ils soient devenus à la lecture (mêlés au texte du
    /// dessous, par exemple). C'est ce qui permet de continuer à taper quand
    /// un bloc déborde sur son voisin.
    fn by_tag(doc: &Document, page: &Page, tag: &Name) -> Result<Option<Target>> {
        let scan = scan::scan(doc, page)?;
        let tagged = sites_by_tag(&scan, tag);
        if tagged.is_empty() {
            return Ok(None);
        }
        // Dans la page même : les opérations du balayage sont les bonnes.
        if tagged.iter().all(|i| !scan.glyphs[*i].site.in_form) {
            return Ok(Some(Self::from_sites(&tagged, scan, Stream::Page)?));
        }
        // Dans un XObject : c'est **son** flux qu'il faut rouvrir, sans quoi
        // les opérations pointeraient sur le `Do` de la page, qui ne porte ni
        // police ni matrice de texte.
        let Some(site) = tagged.iter().find_map(|i| scan.glyphs[*i].site.form) else {
            return Ok(None);
        };
        let form = scan::scan_form(doc, &site, page)?;
        let sites = sites_by_tag(&form, tag);
        if sites.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self::from_sites(
            &sites,
            form,
            Stream::Form(site.reference),
        )?))
    }

    /// Cible construite depuis des glyphes déjà choisis.
    fn from_sites(sites: &[usize], scan: scan::Scan, stream: Stream) -> Result<Target> {
        let mut by_op: BTreeMap<usize, usize> = BTreeMap::new();
        for i in sites {
            *by_op.entry(scan.glyphs[*i].site.op).or_default() += 1;
        }
        // Un bloc balisé occupe ses opérations à lui seul : elles s'effacent
        // entières.
        let cuts = by_op.keys().map(|op| (*op, Vec::new())).collect();
        let first_op = *by_op
            .keys()
            .next()
            .ok_or_else(|| Error::Corrupt("bloc balisé sans glyphe".into()))?;
        let first_site = &scan.glyphs[sites[0]].site;
        let state = &scan.ops[first_op].after;
        let ctm = state.ctm;
        let trm = first_site.tm_before.then(&ctm);
        let scale = trm.a.hypot(trm.b);
        if scale < 1e-9 {
            return Err(Error::Unsupported("bloc dégénéré".into()));
        }
        let inverse = ctm
            .invert()
            .ok_or_else(|| Error::Corrupt("matrice courante non inversible".into()))?;
        let font = state
            .font
            .clone()
            .ok_or_else(|| Error::Corrupt("aucune police active".into()))?;
        let size_text = state.size;
        Ok(Target {
            scan,
            by_op,
            cuts,
            first_op,
            trm,
            inverse,
            scale,
            rotation: trm.b.atan2(trm.a),
            font,
            size_text,
            stream,
        })
    }

    fn build(text: &PageText, unit: &TextUnit, scan: scan::Scan, stream: Stream) -> Result<Target> {
        let glyph_index = GlyphIndex::new(&scan);
        // 1. Tous les glyphes du bloc, et les opérations qui les ont produits.
        let mut sites: Vec<usize> = Vec::new();
        for piece in &unit.pieces {
            let words = piece_words(piece, text);
            let mut found: Vec<usize> = Vec::new();
            for glyph in words.iter().flat_map(|w| w.glyphs.iter()) {
                let i = glyph_index.find(&scan, glyph).ok_or_else(|| {
                    Error::Corrupt(format!(
                        "glyphe « {} » introuvable dans le flux de contenu",
                        glyph.text
                    ))
                })?;
                found.push(i);
            }
            let (Some(min), Some(max)) = (found.iter().min().copied(), found.iter().max().copied())
            else {
                continue;
            };
            // Les espaces entre les mots ne figurent pas dans `PageText` mais
            // font partie de la ligne : ils doivent disparaître avec elle.
            // Un texte **étranger** posé au milieu, lui, reste où il est :
            // on ne retire que nos glyphes, en plusieurs plages s'il le faut.
            for i in min..=max {
                if !found.contains(&i) && scan.glyphs[i].text != " " {
                    continue;
                }
                sites.push(i);
            }
        }
        sites.sort_unstable();
        sites.dedup();
        if sites.is_empty() {
            return Err(Error::Corrupt("paragraphe sans glyphe".into()));
        }
        // 2. Opérations concernées : toutes doivent être entièrement
        //    consommées.
        let mut by_op: BTreeMap<usize, usize> = BTreeMap::new();
        for i in &sites {
            let site = &scan.glyphs[*i].site;
            if site.in_form {
                return Err(Error::Unsupported(
                    "paragraphe dessiné dans un XObject imbriqué : non modifiable".into(),
                ));
            }
            *by_op.entry(site.op).or_default() += 1;
        }
        let mut total: BTreeMap<usize, usize> = BTreeMap::new();
        for g in &scan.glyphs {
            if by_op.contains_key(&g.site.op) {
                *total.entry(g.site.op).or_default() += 1;
            }
        }
        // Une opération qui dessine aussi d'autres textes n'est pas effacée
        // en entier : on n'y retire que la plage de nos glyphes, et le reste
        // garde sa place. Sans cela, un document où une seule opération pose
        // le titre et son voisin serait inmodifiable.
        let mut cuts: BTreeMap<usize, Vec<(super::Pos, super::Pos)>> = BTreeMap::new();
        for (op, count) in &by_op {
            if total.get(op) == Some(count) {
                cuts.insert(*op, Vec::new());
                continue;
            }
            // Nos glyphes de cette opération, groupés en plages continues :
            // ce qui les sépare appartient à quelqu'un d'autre.
            let mut ranges: Vec<(super::Pos, super::Pos)> = Vec::new();
            let mut previous: Option<usize> = None;
            for i in &sites {
                let site = &scan.glyphs[*i].site;
                if site.op != *op {
                    continue;
                }
                let follows = previous.is_some_and(|p| p + 1 == *i);
                match ranges.last_mut() {
                    Some(last) if follows => last.1 = (site.item, site.end),
                    _ => ranges.push(((site.item, site.start), (site.item, site.end))),
                }
                previous = Some(*i);
            }
            cuts.insert(*op, ranges);
        }
        let first_op = *by_op.keys().next().unwrap_or(&0);
        let first_site = &scan.glyphs[sites[0]].site;
        let state = &scan.ops[first_op].after;
        let ctm = state.ctm;
        // 3. Géométrie : matrice de rendu du texte de la première ligne.
        let trm = first_site.tm_before.then(&ctm);
        let scale = trm.a.hypot(trm.b);
        if scale < 1e-9 {
            return Err(Error::Unsupported("bloc dégénéré".into()));
        }
        // Un texte en biais n'est pas refusé : son inclinaison est relevée,
        // et les lignes se poseront dans son repère à lui.
        let rotation = trm.b.atan2(trm.a);
        let inverse = ctm
            .invert()
            .ok_or_else(|| Error::Corrupt("matrice courante non inversible".into()))?;
        let font = state
            .font
            .clone()
            .ok_or_else(|| Error::Corrupt("aucune police active".into()))?;
        let size_text = state.size;
        Ok(Target {
            scan,
            rotation,
            by_op,
            cuts,
            first_op,
            trm,
            inverse,
            scale,
            font,
            size_text,
            stream,
        })
    }
}

/// XObject de formulaire qui dessine **tout** le bloc, s'il y en a un.
///
/// Un bloc dont les glyphes viennent de deux formulaires différents, ou
/// partiellement de la page, n'est pas d'un seul tenant : on le laisse à la
/// procédure ordinaire, qui dira pourquoi elle refuse.
fn form_of(scan: &scan::Scan, text: &PageText, unit: &TextUnit) -> Option<scan::FormSite> {
    let index = GlyphIndex::new(scan);
    let mut site: Option<scan::FormSite> = None;
    for piece in &unit.pieces {
        for glyph in piece_words(piece, text)
            .iter()
            .flat_map(|w| w.glyphs.iter())
        {
            let found = index.find(scan, glyph)?;
            let form = scan.glyphs[found].site.form?;
            match site {
                Some(known) if known != form => return None,
                Some(_) => {}
                None => site = Some(form),
            }
        }
    }
    site
}

/// Recompose un paragraphe et rend sa mise en page.
///
/// Sans boîte imposée, la boîte est celle du paragraphe tel qu'il est
/// (comportement de [`reflow_paragraph`]).
// Le document, la page, son texte, le bloc, le nouveau texte, la boîte et les
// options : sept données distinctes, qu'un regroupement artificiel
// n'éclaircirait pas.
#[allow(clippy::too_many_arguments)]
fn write_paragraph(
    doc: &Document,
    page: &Page,
    text: &PageText,
    para: &TextUnit,
    new_text: &str,
    frame: Option<&ParagraphFrame>,
    options: &ReflowOptions,
    styles: Option<&Styles>,
) -> Result<Vec<LaidLine>> {
    let t = Target::find(doc, page, text, para)?;
    write_to(doc, page, para, new_text, frame, options, &t, styles)
}

// Préparer, mesurer, poser, écrire, remettre l'état : les cinq temps d'une
// écriture, dans l'ordre.
#[allow(clippy::too_many_lines)]
/// Écrit le texte dans une cible déjà trouvée.
#[allow(clippy::too_many_arguments)]
fn write_to(
    doc: &Document,
    page: &Page,
    para: &TextUnit,
    new_text: &str,
    frame: Option<&ParagraphFrame>,
    options: &ReflowOptions,
    t: &Target,
    styles: Option<&Styles>,
) -> Result<Vec<LaidLine>> {
    let chars: Vec<char> = new_text.chars().collect();
    // Une police choisie dans la barre l'emporte sur celle du bloc : c'est
    // « changer la police » d'Acrobat, et elle s'applique au bloc entier.
    let chosen = frame.and_then(|f| f.face.clone());
    let override_style = chosen.as_ref().map(|face| super::StyleOverride {
        font: Some(face.named(t.font.as_str().as_str())),
        size: None,
        color: None,
        bold: Some(face.bold),
        italic: Some(face.italic),
    });
    // La police est citée par le flux qu'on réécrit : page ou XObject.
    let prepared = encode::prepare_in(
        doc,
        page,
        &t.scan.resources,
        &t.font,
        new_text,
        override_style.as_ref(),
    )?;
    // Les styles du bloc, chacun avec sa police prête à écrire. Un bloc d'un
    // seul style — le cas ordinaire — n'en a pas besoin.
    let styles = styles
        .filter(|_| chosen.is_none())
        .filter(|s| !s.uniform() && s.per_char.len() == chars.len());
    let fonts = prepare_runs(doc, page, t, styles, new_text)?;
    // 4. Boîte et corps. Un bloc en biais se met en page dans son repère à
    //    lui, l'origine à zéro : les lignes seront ensuite posées tournées.
    let slanted = frame.is_some_and(|f| f.rotation.abs() > 1e-4);
    let mut geometry = match frame {
        Some(f) if slanted => Geometry {
            x0: 0.0,
            width: f.width,
            alignment: f.alignment,
            indent: f.first_line_indent,
            spacing: f.line_spacing,
            baseline: 0.0,
            size: f.size,
        },
        Some(f) => Geometry::from(f),
        None => Geometry {
            x0: para.bbox.x0,
            width: para.bbox.width(),
            alignment: para.alignment,
            indent: para.first_line_indent,
            spacing: if para.line_spacing > 0.1 {
                para.line_spacing
            } else {
                para.size * 1.2
            },
            baseline: t.trm.f,
            size: t.size_text * t.scale,
        },
    };
    // Chaque caractère mesure selon **sa** police et **son** corps.
    let ratio = |g: &Geometry| g.size / (t.size_text * t.scale).max(1e-9);
    let measure = |g: &Geometry, i: usize, c: char| match (styles, fonts.is_empty()) {
        (Some(styles), false) => {
            let id = styles.per_char.get(i).copied().unwrap_or(0) as usize;
            let font = fonts.get(id).unwrap_or(&prepared);
            let size = styles.runs.get(id).map_or(t.size_text, |r| r.size) * t.scale * ratio(g);
            font.width(&c.to_string()) * size
        }
        _ => prepared.width(&c.to_string()) * g.size,
    };
    let mut laid = lay_out_with(&chars, &geometry, |i, c| measure(&geometry, i, c));
    if frame.is_none() && !options.keep_font_size {
        while laid.len() > para.pieces.len() && geometry.size > options.min_size {
            geometry.size *= 0.95;
            laid = lay_out_with(&chars, &geometry, |i, c| measure(&geometry, i, c));
        }
    }
    let size_text = geometry.size / t.scale;
    // 5. Écriture du bloc à la place de la première opération.
    let mut out = Vec::new();
    // Ancre : le bloc se retrouvera à la frappe suivante sans dépendre de ce
    // que l'extraction en dira.
    if let Some(f) = frame {
        let _ = write!(out_str(&mut out), "/{} BMC ", frame_tag(f).as_str());
    }
    let with_runs = styles.filter(|_| !fonts.is_empty());
    let touched = if let Some(styles) = with_runs {
        // Un bloc à plusieurs styles : chaque tranche rétablit sa police et sa
        // couleur avant d'écrire ses lettres. C'est ce qui garde le gras gras
        // et le rouge rouge.
        for line in &laid {
            let placed = Matrix::new(t.trm.a, t.trm.b, t.trm.c, t.trm.d, line.x, line.baseline);
            write_matrix(&mut out, &placed.then(&t.inverse));
            write_runs(
                &mut out,
                styles,
                &fonts,
                &chars,
                line,
                t.scale * ratio(&geometry),
            );
        }
        true
    } else {
        {
            // Une encre choisie dans le nuancier s'écrit devant le bloc ;
            // celle d'avant est rétablie derrière lui, comme la police.
            let ink = frame.and_then(|f| f.ink);
            let changed = (size_text - t.size_text).abs() > 1e-9
                || prepared.resource != t.font
                || ink.is_some();
            if changed {
                let _ = write!(
                    out_str(&mut out),
                    "/{} {} Tf ",
                    prepared.resource.as_str(),
                    fmt(size_text)
                );
            }
            if let Some([r, g, b]) = ink {
                let _ = write!(out_str(&mut out), "{} {} {} rg ", fmt(r), fmt(g), fmt(b));
            }
            for line in &laid {
                let placed = line_matrix(t, frame, line);
                write_matrix(&mut out, &placed.then(&t.inverse));
                write_tj(
                    &mut out,
                    &line_items(&prepared, &chars, line, geometry.size),
                );
            }
            changed
        }
    };
    if touched {
        let state = &t.scan.ops[t.first_op].after;
        let _ = write!(
            out_str(&mut out),
            "/{} {} Tf ",
            t.font.as_str(),
            fmt(t.size_text)
        );
        // La couleur du bloc est rétablie telle qu'elle était : la suite du
        // flux compte dessus.
        out.extend_from_slice(&state.fill.restore());
        out.push(b' ');
    }
    if frame.is_some() {
        out.extend_from_slice(b"EMC ");
    }
    let state = &t.scan.ops[t.first_op].after;
    restore_matrices(&mut out, state, super::needs_advance(&t.scan, t.first_op));
    while out.last() == Some(&b' ') {
        out.pop();
    }
    // 6. Réécriture : le bloc remplace la première opération, les autres
    //    opérations de texte du paragraphe sont vidées (leur avance est
    //    conservée pour ne rien déplacer après elles).
    let mut ignored = Vec::new();
    // La première opération porte le bloc réécrit. Si elle dessine aussi
    // d'autres textes, ceux-là restent : on ne retire que nos glyphes, puis
    // on écrit le bloc à la suite.
    let mut first = cut_out(doc, page, t, t.first_op, &mut ignored)?;
    first.extend_from_slice(&out);
    let mut replacements = vec![Replacement {
        operation: t.first_op,
        bytes: first,
    }];
    for op in t.by_op.keys().skip(1) {
        replacements.push(Replacement {
            operation: *op,
            bytes: cut_out(doc, page, t, *op, &mut ignored)?,
        });
    }
    let content = super::rewrite_bytes(&t.scan.content, &replacements)?;
    match t.stream {
        Stream::Page => super::set_page_content(doc, page, content)?,
        Stream::Form(reference) => set_form_content(doc, reference, content)?,
    }
    // Les lignes d'un bloc en biais ont été posées dans son repère : on les
    // rend en espace de page, pour que le curseur tombe où il faut.
    if slanted {
        if let Some(f) = frame {
            let (sin, cos) = f.rotation.sin_cos();
            for line in &mut laid {
                let (lx, ly) = (line.x, line.baseline);
                let x = f.x0 + lx * cos - ly * sin;
                let y = f.baseline + lx * sin + ly * cos;
                let shift = x - lx;
                for stop in &mut line.stops {
                    *stop += shift;
                }
                line.x = x;
                line.baseline = y;
            }
        }
    }
    Ok(laid)
}

/// Remplace le flux d'un XObject de formulaire.
///
/// Le dictionnaire est conservé tel quel — `/BBox`, `/Matrix`, `/Resources`
/// disent où et comment le dessin se pose — sauf la longueur et le filtre :
/// on réécrit en clair.
fn set_form_content(
    doc: &Document,
    reference: acrux_document::ObjectRef,
    content: Vec<u8>,
) -> Result<()> {
    let object = acrux_document::Object::Reference(reference);
    let resolved = doc.resolve(&object)?;
    let acrux_document::Object::Stream { dict, .. } = &*resolved else {
        return Err(Error::Corrupt(
            "le XObject de formulaire n'est pas un flux".into(),
        ));
    };
    let mut dict = dict.clone();
    dict.remove(&Name::new("Filter"));
    dict.remove(&Name::new("DecodeParms"));
    dict.insert(
        Name::new("Length"),
        acrux_document::Object::Integer(i64::try_from(content.len()).unwrap_or(0)),
    );
    doc.set(
        reference,
        acrux_document::Object::Stream { dict, raw: content },
    );
    Ok(())
}

/// Ajoute un bloc de texte neuf à la page, dans sa boîte.
///
/// Le contenu existant est d'abord enveloppé dans `q … Q` : quoi qu'il laisse
/// derrière lui (matrice, couleur, découpage), le bloc repart de l'état
/// initial de la page.
fn append_block(
    doc: &Document,
    page: &Page,
    frame: &ParagraphFrame,
    new_text: &str,
) -> Result<CaretMap> {
    let resource = match &frame.standard {
        Some(family) => encode::standard_font(doc, page, family, false, false)?,
        None => frame.font.clone(),
    };
    let prepared = encode::prepare(doc, page, &resource, new_text, None)?;
    let chars: Vec<char> = new_text.chars().collect();
    let geometry = Geometry::from(frame);
    let laid = lay_out(&prepared, &chars, &geometry);
    let scanned = scan::scan(doc, page)?;
    let mut content = Vec::with_capacity(scanned.content.len() + 256);
    content.extend_from_slice(b"q\n");
    content.extend_from_slice(&scanned.content);
    let _ = write!(
        out_str(&mut content),
        "\nQ\nq /{} BMC BT ",
        frame_tag(frame).as_str()
    );
    let [r, g, b] = frame.color;
    let _ = write!(
        out_str(&mut content),
        "/{} {} Tf {} {} {} rg ",
        prepared.resource.as_str(),
        fmt(frame.size),
        fmt(r),
        fmt(g),
        fmt(b)
    );
    for line in &laid {
        write_matrix(
            &mut content,
            &Matrix::new(1.0, 0.0, 0.0, 1.0, line.x, line.baseline),
        );
        write_tj(
            &mut content,
            &line_items(&prepared, &chars, line, frame.size),
        );
    }
    content.extend_from_slice(b"ET EMC Q\n");
    super::set_page_content(doc, page, content)?;
    Ok(caret_from_layout(&laid, frame.size))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(width: f64, alignment: Alignment) -> Geometry {
        Geometry {
            x0: 100.0,
            width,
            alignment,
            indent: 0.0,
            spacing: 14.0,
            baseline: 700.0,
            size: 10.0,
        }
    }

    /// Mise en page sans police : chaque caractère fait un demi-cadratin.
    fn laid(text: &str, width: f64, alignment: Alignment) -> Vec<LaidLine> {
        let chars: Vec<char> = text.chars().collect();
        lay_out_fixed(&chars, &geometry(width, alignment))
    }

    /// La découpe de l'écriture, avec une police qui mesure tout à 0,5 em :
    /// c'est la découpe qu'on teste, pas la police.
    fn lay_out_fixed(chars: &[char], g: &Geometry) -> Vec<LaidLine> {
        lay_out_with(chars, g, |_, _| 0.5 * g.size)
    }

    #[test]
    fn les_retours_a_la_ligne_tapes_coupent_la_ligne() {
        let l = laid("ab\ncd", 500.0, Alignment::Left);
        assert_eq!(l.len(), 2);
        assert_eq!((l[0].start, l[0].end, l[0].draw_end), (0, 3, 2));
        assert_eq!((l[1].start, l[1].end), (3, 5));
        assert!((l[1].baseline - 686.0).abs() < 1e-9);
    }

    #[test]
    fn une_ligne_pleine_coule_a_lespace() {
        // 5 pt par caractère, boîte de 42 pt : « aaaa bbbb » ne tient pas.
        let l = laid("aaaa bbbb", 42.0, Alignment::Left);
        assert_eq!(l.len(), 2);
        // L'espace reste en fin de première ligne, invisible.
        assert_eq!((l[0].start, l[0].end, l[0].draw_end), (0, 5, 4));
        assert_eq!(l[1].start, 5);
    }

    #[test]
    fn chaque_frontiere_a_sa_position() {
        let l = laid("abc", 500.0, Alignment::Left);
        let map = caret_from_layout(&l, 10.0);
        assert_eq!(map.len(), 3);
        let xs: Vec<f64> = map.stops.iter().map(|s| s.x).collect();
        assert_eq!(xs, vec![100.0, 105.0, 110.0, 115.0]);
    }

    #[test]
    fn la_frontiere_de_coupure_appartient_a_la_ligne_suivante() {
        let l = laid("aaaa bbbb", 42.0, Alignment::Left);
        let map = caret_from_layout(&l, 10.0);
        // Avant l'espace : fin de la première ligne.
        assert_eq!(map.stops[4].line, 0);
        // Après l'espace : début de la seconde, à gauche de la boîte.
        assert_eq!(map.stops[5].line, 1);
        assert!((map.stops[5].x - 100.0).abs() < 1e-9);
        // Descendre depuis le début garde l'abscisse.
        assert_eq!(map.vertical(0, 1), 5);
        assert_eq!(map.vertical(5, -1), 0);
    }

    #[test]
    fn un_clic_tombe_sur_la_frontiere_la_plus_proche() {
        let l = laid("abc\ndef", 500.0, Alignment::Left);
        let map = caret_from_layout(&l, 10.0);
        // Ligne du haut, entre « a » et « b ».
        assert_eq!(map.nearest(104.0, 702.0), 1);
        // Ligne du bas, après « f ».
        assert_eq!(map.nearest(200.0, 688.0), 7);
    }

    #[test]
    fn centre_et_droite_se_placent_dans_la_boite() {
        let c = laid("ab", 100.0, Alignment::Center);
        assert!((c[0].x - 145.0).abs() < 1e-9);
        let r = laid("ab", 100.0, Alignment::Right);
        assert!((r[0].x - 190.0).abs() < 1e-9);
    }

    #[test]
    fn la_justification_elargit_les_blancs_sauf_en_derniere_ligne() {
        let l = laid("aa bb cc dd", 38.0, Alignment::Justify);
        assert!(l.len() >= 2);
        assert!(l[0].gap > 0.0, "première ligne justifiée");
        assert!(
            l.last().is_some_and(|x| x.gap == 0.0),
            "dernière ligne libre"
        );
        // La première ligne justifiée finit exactement au bord droit.
        let end = l[0].stops[l[0].draw_end - l[0].start];
        assert!((end - 138.0).abs() < 1e-6, "{end}");
    }

    #[test]
    fn la_selection_donne_un_rectangle_par_ligne() {
        let l = laid("abc\ndef", 500.0, Alignment::Left);
        let map = caret_from_layout(&l, 10.0);
        assert_eq!(map.selection_rects(1, 6).len(), 2);
        assert!(map.selection_rects(2, 2).is_empty());
    }

    #[test]
    fn un_texte_vide_a_quand_meme_un_curseur() {
        let l = laid("", 500.0, Alignment::Left);
        let map = caret_from_layout(&l, 10.0);
        assert_eq!(map.len(), 0);
        assert_eq!(map.stops.len(), 1);
        assert!((map.stops[0].x - 100.0).abs() < 1e-9);
    }

    #[test]
    fn un_mot_trop_long_est_coupe_au_caractere() {
        let l = laid("abcdefghij", 22.0, Alignment::Left);
        assert!(l.len() >= 2);
        assert!(l.iter().all(|x| x.draw_end > x.start));
    }
}
