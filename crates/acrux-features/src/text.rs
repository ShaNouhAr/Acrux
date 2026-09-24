//! Extraction de texte structurée (base de la recherche, de la sélection, de
//! la copie, de l'export et, à terme, de l'édition) : parcours des opérateurs
//! d'une page avec le même modèle de positionnement que le rendu
//! (ISO 32000-2 §9.4), puis reconstruction géométrique de la mise en page :
//!
//! 1. glyphes → mots → lignes (`group_glyphs`, ce fichier) ;
//! 2. tableaux (`tables`) : grilles de filets fins ou colonnes de texte alignées ;
//! 3. colonnes et ordre de lecture par découpe XY récursive, puis paragraphes,
//!    en-têtes et pieds de page, listes, titres (`layout`) ;
//! 4. sorties texte brut, Markdown, HTML et texte positionné (`export`) ;
//! 5. recherche (`search`) : casse, mot entier, occurrences à cheval sur
//!    deux lignes d'un même paragraphe.
//!
//! Les styles (police, taille, gras, italique, couleur) sont suivis pendant
//! l'interprétation du contenu (`extract`). L'ordre de lecture est purement
//! géométrique ; le balisage (`/StructTreeRoot`, §14.7) donnera un ordre plus
//! fiable quand il existe — amélioration prévue.

mod export;
mod extract;
mod layout;
mod search;
mod tables;

use acrux_core::{Point, Rect, Result};
use acrux_document::{Document, Page};

pub use layout::mark_repeated_headers;
pub use search::{find_matches, MatchPiece, SearchOptions, TextMatch};

/// Style d'un mot ou d'une suite de glyphes.
#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    /// Nom de base de la police (`/BaseFont`, sans le préfixe de sous-ensemble).
    pub font: String,
    /// Taille effective en points.
    pub size: f64,
    /// Gras (nom de police, `/FontWeight` ≥ 600 ou drapeau ForceBold).
    pub bold: bool,
    /// Italique (nom de police, drapeau Italic ou `/ItalicAngle` ≠ 0).
    pub italic: bool,
    /// Couleur de remplissage courante, RVB dans [0, 1].
    pub color: [f32; 3],
}

impl Default for Style {
    fn default() -> Self {
        Self {
            font: String::new(),
            size: 0.0,
            bold: false,
            italic: false,
            color: [0.0; 3],
        }
    }
}

/// Un glyphe positionné sur la page (espace utilisateur PDF, origine en bas à gauche).
#[derive(Debug, Clone, Default)]
pub struct Glyph {
    /// Texte Unicode (souvent un caractère, parfois une ligature).
    pub text: String,
    /// Boîte englobante approximative (avance × taille).
    pub bbox: Rect,
    /// Taille de police effective en points.
    pub size: f64,
    /// Nom de base de la police.
    pub font: String,
    /// Le code source était un espace (mot suivant).
    pub is_space: bool,
    /// Angle de la ligne de base en radians (0 = horizontal).
    pub angle: f64,
    /// Position de l'origine projetée sur la direction du texte.
    pub along: f64,
    /// Fin de l'avance projetée sur la direction du texte.
    pub along_end: f64,
    /// Position de la ligne de base projetée perpendiculairement au texte.
    pub perp: f64,
    /// Police grasse.
    pub bold: bool,
    /// Police italique.
    pub italic: bool,
    /// Couleur de remplissage RVB au moment du tracé.
    pub color: [f32; 3],
}

impl Glyph {
    /// Style du glyphe.
    #[must_use]
    pub fn style(&self) -> Style {
        Style {
            font: self.font.clone(),
            size: self.size,
            bold: self.bold,
            italic: self.italic,
            color: self.color,
        }
    }
}

/// Projections d'un glyphe sur sa direction d'écriture.
fn projections(origin: Point, end: Point) -> (f64, f64, f64, f64) {
    let angle = (end.y - origin.y).atan2(end.x - origin.x);
    let (s, c) = angle.sin_cos();
    let along = origin.x * c + origin.y * s;
    let along_end = end.x * c + end.y * s;
    let perp = -origin.x * s + origin.y * c;
    (angle, along, along_end, perp)
}

/// Mot : suite de glyphes sans espace.
#[derive(Debug, Clone, Default)]
pub struct Word {
    /// Texte.
    pub text: String,
    /// Boîte englobante.
    pub bbox: Rect,
    /// Glyphes d'origine.
    pub glyphs: Vec<Glyph>,
    /// Style (celui du premier glyphe).
    pub style: Style,
}

/// Ligne de mots.
#[derive(Debug, Clone, Default)]
pub struct Line {
    /// Mots de gauche à droite.
    pub words: Vec<Word>,
    /// Boîte englobante.
    pub bbox: Rect,
}

impl Line {
    /// Texte de la ligne (mots séparés par des espaces).
    #[must_use]
    pub fn text(&self) -> String {
        let words: Vec<&str> = self.words.iter().map(|w| w.text.as_str()).collect();
        words.join(" ")
    }

    /// Ligne de base (ordonnée projetée du premier glyphe) ; bas de la boîte à défaut.
    #[must_use]
    pub fn baseline(&self) -> f64 {
        self.words
            .first()
            .and_then(|w| w.glyphs.first())
            .map_or(self.bbox.y0, |g| g.perp)
    }

    /// Angle d'écriture (radians).
    #[must_use]
    pub fn angle(&self) -> f64 {
        self.words
            .first()
            .and_then(|w| w.glyphs.first())
            .map_or(0.0, |g| g.angle)
    }

    /// Taille de police dominante (médiane des glyphes).
    #[must_use]
    pub fn size(&self) -> f64 {
        let mut sizes: Vec<f64> = self
            .words
            .iter()
            .flat_map(|w| w.glyphs.iter().map(|g| g.size))
            .collect();
        median(&mut sizes).unwrap_or(self.bbox.height().max(1.0))
    }
}

/// Alignement horizontal d'un paragraphe dans sa colonne.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Alignment {
    /// Aligné à gauche (drapeau à droite).
    #[default]
    Left,
    /// Aligné à droite.
    Right,
    /// Centré.
    Center,
    /// Justifié (les deux marges atteintes).
    Justify,
}

/// Paragraphe : lignes consécutives d'un même bloc de texte.
#[derive(Debug, Clone, Default)]
pub struct Paragraph {
    /// Boîte englobante.
    pub bbox: Rect,
    /// Indices des lignes dans `PageText::lines`, dans l'ordre de lecture.
    pub lines: Vec<usize>,
    /// Texte, lignes fusionnées par des espaces et césures réparées, sans le marqueur de liste.
    pub text: String,
    /// Alignement détecté.
    pub alignment: Alignment,
    /// Retrait de la première ligne par rapport aux suivantes (points, 0 si une seule ligne).
    pub first_line_indent: f64,
    /// Interligne (distance médiane entre lignes de base ; 0 si une seule ligne).
    pub line_spacing: f64,
    /// Taille de police dominante.
    pub size: f64,
    /// Marqueur de liste (« • », « - », « 1. »…) si le paragraphe est un élément de liste.
    /// Une puce dessinée (cercle, carré) est rapportée comme « • ».
    pub marker: Option<String>,
    /// Niveau de titre déduit de la taille (1 à 3), 0 sinon.
    pub heading: u8,
}

/// Rôle d'un bloc dans la page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlockKind {
    /// Corps de page.
    #[default]
    Body,
    /// En-tête (ligne isolée en haut de page, ou répétée d'une page à l'autre).
    Header,
    /// Pied de page (idem en bas ; numéros de page).
    Footer,
    /// Tableau : indice dans `PageText::tables` ; les paragraphes sont vides.
    Table(usize),
}

/// Bloc : zone rectangulaire homogène produite par la découpe XY (une colonne,
/// un encadré, un en-tête…), contenant des paragraphes.
#[derive(Debug, Clone, Default)]
pub struct Block {
    /// Rôle.
    pub kind: BlockKind,
    /// Boîte englobante.
    pub bbox: Rect,
    /// Paragraphes de haut en bas.
    pub paragraphs: Vec<Paragraph>,
}

/// Cellule de tableau.
#[derive(Debug, Clone, Default)]
pub struct Cell {
    /// Boîte de la cellule (grille) ou du texte (tableau sans filets).
    pub bbox: Rect,
    /// Texte (vide pour une cellule vide).
    pub text: String,
}

/// Tableau détecté.
#[derive(Debug, Clone, Default)]
pub struct Table {
    /// Boîte englobante.
    pub bbox: Rect,
    /// Lignes de cellules, de haut en bas puis de gauche à droite.
    pub rows: Vec<Vec<Cell>>,
    /// Indices des lignes de texte (`PageText::lines`) contenues dans le tableau.
    pub lines: Vec<usize>,
}

/// Texte structuré d'une page.
#[derive(Debug, Clone, Default)]
pub struct PageText {
    /// Lignes dans l'ordre de lecture (colonne de gauche avant celle de droite).
    pub lines: Vec<Line>,
    /// Blocs dans l'ordre de lecture (en-tête, colonnes, tableaux, pied de page).
    pub blocks: Vec<Block>,
    /// Tableaux détectés.
    pub tables: Vec<Table>,
    /// Filets fins du contenu (rectangles remplis ou traits de faible épaisseur),
    /// en espace utilisateur ; servent à la détection des tableaux.
    pub rules: Vec<Rect>,
    /// Petites formes pleines (puces dessinées, cases), en espace utilisateur.
    pub marks: Vec<Rect>,
    /// Boîte de la page (`/CropBox`) ; vide si inconnue (en-têtes et pieds non détectés).
    pub page_box: Rect,
}

impl PageText {
    /// Tous les mots.
    #[must_use]
    pub fn words(&self) -> Vec<&Word> {
        self.lines.iter().flat_map(|l| l.words.iter()).collect()
    }

    /// Reconstruit la mise en page à partir de `lines`, `rules`, `marks` et
    /// `page_box` : réordonne `lines` dans l'ordre de lecture et remplit
    /// `blocks` et `tables`. Appelé par `extract_page_text` ; utile après avoir
    /// construit ou modifié `lines` à la main.
    pub fn analyze(&mut self) {
        let lines = std::mem::take(&mut self.lines);
        let (lines, tables, blocks) =
            layout::analyze(lines, &self.rules, &self.marks, self.page_box);
        self.lines = lines;
        self.tables = tables;
        self.blocks = blocks;
    }

    /// Taille de police médiane de la page (pondérée par le nombre de glyphes).
    #[must_use]
    pub fn median_size(&self) -> f64 {
        let mut sizes: Vec<f64> = self
            .lines
            .iter()
            .flat_map(|l| l.words.iter())
            .flat_map(|w| w.glyphs.iter().map(|g| g.size))
            .collect();
        median(&mut sizes).unwrap_or(0.0)
    }
}

/// Médiane d'un vecteur (trié sur place) ; `None` s'il est vide.
pub(crate) fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(values[values.len() / 2])
}

/// Extrait le texte structuré d'une page.
///
/// # Errors
/// Ressources illisibles.
pub fn extract_page_text(doc: &Document, page: &Page) -> Result<PageText> {
    let extracted = extract::extract(doc, page)?;
    let mut text = lines_from_glyphs(extracted.glyphs);
    text.rules = extracted.rules;
    text.marks = extracted.marks;
    text.page_box = page.crop_box(doc);
    text.analyze();
    Ok(text)
}

/// Regroupe des glyphes en mots, lignes, puis blocs et paragraphes, dans l'ordre de lecture.
#[must_use]
pub fn group_glyphs(glyphs: Vec<Glyph>) -> PageText {
    let mut text = lines_from_glyphs(glyphs);
    text.analyze();
    text
}

/// Regroupe les glyphes en mots puis en lignes géométriques (une ligne par
/// ligne de base, de haut en bas), sans analyse de mise en page.
#[must_use]
pub fn lines_from_glyphs(glyphs: Vec<Glyph>) -> PageText {
    if glyphs.is_empty() {
        return PageText::default();
    }
    // 1. Lignes : glyphes de même direction dont la ligne de base (projetée
    //    perpendiculairement au texte) est proche (tolérance ½ taille).
    let mut lines: Vec<(f64, f64, Vec<Glyph>)> = Vec::new(); // (angle, perp, glyphes)
    for g in glyphs {
        let tol = (g.size * 0.5).max(1.0);
        match lines
            .iter_mut()
            .find(|(a, p, _)| (*a - g.angle).abs() < 0.035 && (*p - g.perp).abs() <= tol)
        {
            Some((_, _, gs)) => gs.push(g),
            None => lines.push((g.angle, g.perp, vec![g])),
        }
    }
    // Lignes de haut en bas (sommet de la boîte, y décroissant en espace PDF).
    let top = |gs: &[Glyph]| gs.iter().map(|g| g.bbox.y1).fold(f64::MIN, f64::max);
    lines.sort_by(|a, b| {
        top(&b.2)
            .partial_cmp(&top(&a.2))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut out = Vec::with_capacity(lines.len());
    for (_, _, mut gs) in lines {
        gs.sort_by(|a, b| {
            a.along
                .partial_cmp(&b.along)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        // 2. Mots : coupure sur un espace explicite ou un écart > 0,15 em.
        let mut words: Vec<Word> = Vec::new();
        let mut current: Vec<Glyph> = Vec::new();
        let mut last_right: Option<f64> = None;
        for g in gs {
            let gap = last_right.map_or(0.0, |r| g.along - r);
            let breaks = g.is_space || gap > g.size * 0.15 || gap < -g.size;
            if breaks && !current.is_empty() {
                words.push(make_word(std::mem::take(&mut current)));
            }
            if g.is_space {
                last_right = Some(g.along_end.max(last_right.unwrap_or(g.along_end)));
            } else {
                last_right = Some(g.along_end);
                current.push(g);
            }
        }
        if !current.is_empty() {
            words.push(make_word(current));
        }
        if words.is_empty() {
            continue;
        }
        out.push(make_line(words));
    }
    PageText {
        lines: out,
        ..PageText::default()
    }
}

/// Construit une ligne à partir de ses mots (boîte = union).
pub(crate) fn make_line(words: Vec<Word>) -> Line {
    let bbox = words.iter().skip(1).fold(
        words.first().map_or_else(Rect::default, |w| w.bbox),
        |r, w| r.union(&w.bbox),
    );
    Line { words, bbox }
}

fn make_word(glyphs: Vec<Glyph>) -> Word {
    let text: String = glyphs.iter().map(|g| g.text.as_str()).collect();
    let bbox = glyphs
        .iter()
        .skip(1)
        .fold(glyphs[0].bbox, |r, g| r.union(&g.bbox));
    let style = glyphs[0].style();
    Word {
        text,
        bbox,
        glyphs,
        style,
    }
}

/// Recherche insensible à la casse : une boîte par morceau d'occurrence.
///
/// Raccourci de [`find_matches`] avec les options par défaut : une occurrence
/// qui passe à la ligne donne une boîte par ligne. Les occurrences ne se
/// chevauchent pas (« aa » se trouve deux fois dans « aaaa »).
#[must_use]
pub fn find(text: &PageText, needle: &str) -> Vec<Rect> {
    find_matches(text, needle, SearchOptions::default())
        .into_iter()
        .flat_map(|m| m.pieces.into_iter().map(|p| p.rect))
        .collect()
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Fabrique de `PageText` à la main pour les tests de mise en page.
    use super::*;

    /// Un glyphe horizontal de largeur `w` à (`x`, `y`), taille `size`.
    #[allow(clippy::too_many_arguments)]
    pub fn glyph(x: f64, y: f64, w: f64, size: f64, c: char, bold: bool, italic: bool) -> Glyph {
        Glyph {
            text: c.to_string(),
            bbox: Rect::new(x, y - 0.2 * size, x + w, y + 0.8 * size),
            size,
            font: if bold { "Helvetica-Bold" } else { "Helvetica" }.into(),
            is_space: false,
            angle: 0.0,
            along: x,
            along_end: x + w,
            perp: y,
            bold,
            italic,
            color: [0.0; 3],
        }
    }

    /// Glyphes d'une ligne de texte : chaque caractère avance de 0,5 em,
    /// une espace de 0,3 em. `*mot*` met le mot en gras.
    pub fn line_glyphs(x: f64, y: f64, size: f64, text: &str) -> Vec<Glyph> {
        let mut out = Vec::new();
        let mut cx = x;
        let mut bold = false;
        for c in text.chars() {
            if c == '*' {
                bold = !bold;
                continue;
            }
            if c == ' ' {
                cx += 0.3 * size;
                out.push(Glyph {
                    is_space: true,
                    ..glyph(cx, y, 0.0, size, ' ', false, false)
                });
                continue;
            }
            out.push(glyph(cx, y, 0.5 * size, size, c, bold, false));
            cx += 0.5 * size;
        }
        out
    }

    /// Page construite à partir de lignes `(x, y, taille, texte)`.
    pub fn page(lines: &[(f64, f64, f64, &str)]) -> PageText {
        let glyphs: Vec<Glyph> = lines
            .iter()
            .flat_map(|(x, y, s, t)| line_glyphs(*x, *y, *s, t))
            .collect();
        let mut text = lines_from_glyphs(glyphs);
        text.page_box = Rect::new(0.0, 0.0, 600.0, 800.0);
        text
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::float_cmp)]
mod tests {
    use super::*;
    use acrux_document::collect_pages;

    fn pdf_with_content(content: &str) -> Document {
        let src = format!(
            "%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >> /F2 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica-BoldOblique >> /F3 << /Type /Font /Subtype /TrueType /BaseFont /Truc /FontDescriptor << /Type /FontDescriptor /FontName /Truc /Flags 32 /FontWeight 700 /ItalicAngle -12 >> >> >> >> >> endobj\n4 0 obj << /Length {} >>\nstream\n{content}\nendstream\nendobj\n",
            content.len()
        );
        Document::from_bytes(src.into_bytes()).unwrap()
    }

    #[test]
    fn words_lines_and_order() {
        let doc = pdf_with_content(
            "BT /F1 12 Tf 20 250 Td (Bonjour le monde) Tj 0 -20 Td [(Deux) -600 (mots)] TJ ET BT /F1 12 Tf 20 100 Td (Bloc suivant) Tj ET",
        );
        let page = &collect_pages(&doc).unwrap()[0];
        let text = extract_page_text(&doc, page).unwrap();
        assert_eq!(text.lines.len(), 3);
        assert_eq!(
            text.lines[0]
                .words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>(),
            vec!["Bonjour", "le", "monde"]
        );
        assert_eq!(
            text.lines[1].words.len(),
            2,
            "le recul TJ de 600 crée un espace"
        );
        let plain = text.to_plain();
        assert!(
            plain.starts_with("Bonjour le monde Deux mots\n"),
            "interligne régulier : un seul paragraphe, lignes fusionnées : {plain:?}"
        );
        assert!(
            plain.contains("\n\nBloc suivant"),
            "écart vertical → ligne vide : {plain:?}"
        );
        assert!(text.lines[0].bbox.y0 > text.lines[1].bbox.y0);
        assert_eq!(text.blocks.len(), 2, "{:?}", text.blocks);
    }

    #[test]
    fn rotated_text_stays_on_one_line() {
        // Texte à 30° : sans projection sur la direction, chaque glyphe ferait sa propre ligne.
        let doc =
            pdf_with_content("BT /F1 12 Tf 0.866 0.5 -0.5 0.866 50 50 Tm (Tourne trente) Tj ET");
        let page = &collect_pages(&doc).unwrap()[0];
        let text = extract_page_text(&doc, page).unwrap();
        assert_eq!(text.lines.len(), 1, "{:?}", text.to_plain());
        assert_eq!(text.to_plain().trim(), "Tourne trente");
    }

    #[test]
    fn invisible_text_is_extracted_and_search_works() {
        let doc = pdf_with_content(
            "BT /F1 10 Tf 3 Tr 10 100 Td (cache) Tj 0 Tr 0 50 Td (Visible ici) Tj ET",
        );
        let page = &collect_pages(&doc).unwrap()[0];
        let text = extract_page_text(&doc, page).unwrap();
        assert_eq!(
            text.to_plain().trim(),
            "Visible ici\n\ncache",
            "le texte invisible (OCR) est extrait"
        );
        let hits = find(&text, "ICI");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].x0 > 10.0 && hits[0].width() > 5.0);
        assert!(find(&text, "absent").is_empty());
    }

    #[test]
    fn styles_from_font_name_descriptor_and_color() {
        let doc = pdf_with_content(
            "BT /F1 12 Tf 20 250 Td (normal) Tj /F2 12 Tf 0 -14 Td (gras-italique) Tj /F3 12 Tf 0 -14 Td 1 0 0 rg (descripteur) Tj 0 -14 Td 0.5 g (gris) Tj 0 -14 Td 0 0 0 1 k (noir) Tj ET",
        );
        let page = &collect_pages(&doc).unwrap()[0];
        let text = extract_page_text(&doc, page).unwrap();
        let words = text.words();
        let by_text = |t: &str| {
            words
                .iter()
                .find(|w| w.text == t)
                .unwrap_or_else(|| panic!("mot {t} absent"))
                .style
                .clone()
        };
        let s = by_text("normal");
        assert!(!s.bold && !s.italic && s.color == [0.0; 3] && s.font == "Helvetica");
        let s = by_text("gras-italique");
        assert!(s.bold && s.italic, "{s:?}");
        let s = by_text("descripteur");
        assert!(s.bold && s.italic, "FontWeight 700 + ItalicAngle : {s:?}");
        assert_eq!(s.color, [1.0, 0.0, 0.0]);
        assert_eq!(by_text("gris").color, [0.5; 3]);
        assert_eq!(by_text("noir").color, [0.0; 3]);
    }

    #[test]
    fn rules_and_marks_are_collected() {
        let doc = pdf_with_content(
            "q 1 w 20 200 100 0.5 re f 20 100 m 120 100 l S 20 100 m 20 200 l S 50 50 4 4 re f 10 10 200 100 re f Q BT /F1 12 Tf 60 52 Td (item) Tj ET",
        );
        let page = &collect_pages(&doc).unwrap()[0];
        let text = extract_page_text(&doc, page).unwrap();
        assert_eq!(text.rules.len(), 3, "{:?}", text.rules);
        assert_eq!(text.marks.len(), 1, "{:?}", text.marks);
        assert_eq!(text.blocks[0].paragraphs[0].marker.as_deref(), Some("•"));
        assert_eq!(text.to_plain().trim(), "• item");
    }
}
