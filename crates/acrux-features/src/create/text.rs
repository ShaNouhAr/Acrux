//! Création d'un document à partir de texte brut : un vrai metteur en page.
//!
//! Le texte est découpé en paragraphes (un par ligne d'entrée, une ligne vide
//! donnant un blanc), chaque paragraphe est découpé en lignes qui tiennent
//! dans la largeur utile ([`super::wrap`]), et les lignes s'empilent de haut
//! en bas jusqu'au bas de la boîte utile, où une page neuve s'ouvre.
//!
//! Quatre alignements : à gauche, à droite, centré et justifié. La **dernière
//! ligne d'un paragraphe ne se justifie jamais** — l'élargir étirerait un
//! demi-mot sur toute la largeur.
//!
//! La police est choisie automatiquement : une des quatorze polices standard
//! tant que le texte tient en WinAnsiEncoding, une police système incorporée
//! en sous-ensemble sinon, ce qui permet d'écrire n'importe quelle langue
//! sans que le texte cesse d'être extractible.

use acrux_core::{Rect, Result};
use acrux_document::Document;

use super::builder::{Builder, FontCache, FontRef, Pen};
use super::paper::PageSetup;
use super::wrap::wrap;
use crate::fontembed::FontStyle;
use crate::stamp::metrics::StandardFont;
use crate::stamp::Rgb;

/// Alignement des lignes dans la largeur utile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlign {
    /// Aligné à gauche, bord droit libre.
    #[default]
    Left,
    /// Aligné à droite.
    Right,
    /// Centré.
    Center,
    /// Justifié : les espaces s'élargissent pour atteindre les deux bords,
    /// sauf sur la dernière ligne de chaque paragraphe.
    Justify,
}

impl TextAlign {
    /// Alignement d'après son nom (`gauche`, `droite`, `centre`, `justifie`
    /// et leurs équivalents anglais).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let key: String = name
            .to_ascii_lowercase()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        match key.as_str() {
            "gauche" | "left" => Some(TextAlign::Left),
            "droite" | "right" => Some(TextAlign::Right),
            "centre" | "center" => Some(TextAlign::Center),
            "justifie" | "justify" | "justifier" => Some(TextAlign::Justify),
            _ => None,
        }
    }
}

/// Réglages de mise en page du texte et du Markdown.
#[derive(Debug, Clone, PartialEq)]
pub struct TextLayout {
    /// Format, orientation et marges. Le champ `pages` est ignoré : c'est la
    /// quantité de texte qui décide du nombre de pages.
    pub setup: PageSetup,
    /// Police standard souhaitée ; une police système la remplace dès que le
    /// texte sort de WinAnsiEncoding.
    pub font: StandardFont,
    /// Corps en points.
    pub size: f64,
    /// Interligne, en multiples du corps.
    pub line_height: f64,
    /// Blanc ajouté entre deux paragraphes, en points.
    pub paragraph_spacing: f64,
    /// Alignement.
    pub align: TextAlign,
    /// Couleur du texte.
    pub color: Rgb,
    /// En-tête répété sur chaque page ; jetons `{page}` et `{pages}`.
    pub header: Option<String>,
    /// Pied de page répété sur chaque page ; mêmes jetons.
    pub footer: Option<String>,
    /// Couper les mots plus longs que la ligne par une césure simple. À
    /// `false`, un tel mot déborde mais le texte extrait reste **exactement**
    /// le texte d'entrée.
    pub hyphenate: bool,
    /// Faire des titres Markdown des signets (arbre plat). Sans effet sur
    /// [`from_text`], qui ne connaît pas de titres.
    pub bookmark_headings: bool,
}

impl Default for TextLayout {
    /// A4 à la française, Helvetica 11 pt, interligne 1,35, alignement à
    /// gauche, sans en-tête ni pied de page.
    fn default() -> Self {
        TextLayout {
            setup: PageSetup::default(),
            font: StandardFont::Helvetica,
            size: 11.0,
            line_height: 1.35,
            paragraph_spacing: 6.0,
            align: TextAlign::Left,
            color: [0.0, 0.0, 0.0],
            header: None,
            footer: None,
            hyphenate: true,
            bookmark_headings: true,
        }
    }
}

impl TextLayout {
    /// Interligne en points.
    #[must_use]
    pub fn leading(&self) -> f64 {
        self.size * self.line_height
    }
}

/// Colonne de texte qui déborde de page en page.
///
/// Les créateurs de texte et de Markdown posent leurs lignes par
/// [`Flow::reserve`], qui ouvre une page neuve dès que la bande demandée ne
/// tient plus sous le curseur.
pub(crate) struct Flow {
    /// Document en construction.
    pub(crate) builder: Builder,
    /// Boîte utile, identique sur toutes les pages.
    area: Rect,
    /// Taille de page.
    page_size: (f64, f64),
    /// Haut de la bande encore libre sur la page courante.
    y: f64,
    /// Index de la page courante.
    page: usize,
}

impl Flow {
    /// Ouvre une colonne et sa première page.
    ///
    /// # Errors
    /// Document impossible à créer.
    pub(crate) fn new(setup: &PageSetup) -> Result<Self> {
        let mut builder = Builder::new()?;
        let page_size = setup.page_size();
        let page = builder.add_page(page_size.0, page_size.1);
        let area = setup.content_box();
        Ok(Flow {
            builder,
            area,
            page_size,
            y: area.y1,
            page,
        })
    }

    /// Boîte utile.
    pub(crate) fn area(&self) -> Rect {
        self.area
    }

    /// Index de la page courante.
    pub(crate) fn page(&self) -> usize {
        self.page
    }

    /// Ouvre une page neuve et y replace le curseur.
    pub(crate) fn new_page(&mut self) {
        self.page = self.builder.add_page(self.page_size.0, self.page_size.1);
        self.y = self.area.y1;
    }

    /// Réserve une bande de `height` points et renvoie son bord supérieur.
    /// Une page neuve s'ouvre si la bande ne tient pas sous le curseur — sauf
    /// si la page est déjà vierge, auquel cas la bande déborde plutôt que de
    /// provoquer une suite de pages blanches.
    pub(crate) fn reserve(&mut self, height: f64) -> f64 {
        let empty_page = (self.y - self.area.y1).abs() < 0.001;
        if !empty_page && self.y - height < self.area.y0 - 0.001 {
            self.new_page();
        }
        let top = self.y;
        self.y -= height;
        top
    }

    /// Avance le curseur sans rien réserver (blanc entre deux paragraphes).
    /// Un blanc n'ouvre jamais de page à lui seul.
    pub(crate) fn skip(&mut self, height: f64) {
        self.y -= height;
    }

    /// Ligne de base d'une ligne de texte posée dans une bande de `leading`
    /// points dont le haut est `top` : le blanc restant est partagé également
    /// au-dessus et au-dessous des hampes.
    pub(crate) fn baseline(
        fonts: &FontCache,
        font: FontRef,
        size: f64,
        top: f64,
        leading: f64,
    ) -> f64 {
        let ascent = fonts.ascent(font) * size / 1000.0;
        let descent = fonts.descent(font) * size / 1000.0;
        let slack = (leading - (ascent - descent)).max(0.0);
        top - slack / 2.0 - ascent
    }
}

/// Écrit une ligne déjà découpée, selon l'alignement demandé.
pub(crate) fn draw_line(
    flow: &mut Flow,
    pen: Pen,
    (align, justify): (TextAlign, bool),
    (x0, x1): (f64, f64),
    (baseline, text): (f64, &str),
) {
    if text.is_empty() {
        return;
    }
    let width = x1 - x0;
    let measured = flow.builder.fonts.width(pen.font, text, pen.size);
    let page = flow.page;
    let (fonts, content) = flow.builder.draw(page);
    match align {
        TextAlign::Left => content.text(fonts, pen, (x0, baseline), text),
        TextAlign::Right => content.text(fonts, pen, (x1 - measured, baseline), text),
        TextAlign::Center => {
            content.text(fonts, pen, (x0 + (width - measured) / 2.0, baseline), text);
        }
        TextAlign::Justify => {
            let spaces = text.chars().filter(|c| *c == ' ').count();
            #[allow(clippy::cast_precision_loss)] // au plus quelques dizaines
            let extra = if justify && spaces > 0 && measured < width {
                (width - measured) / spaces as f64
            } else {
                0.0
            };
            content.justified_text(fonts, pen, (x0, baseline), text, extra);
        }
    }
}

/// Compose un document à partir de texte brut.
///
/// Chaque ligne d'entrée est un paragraphe ; une ligne vide laisse un blanc.
/// Les fins de ligne `\r\n`, `\r` et `\n` sont acceptées indifféremment.
///
/// # Errors
/// Texte hors WinAnsiEncoding sans aucune police système exploitable, ou
/// document impossible à refermer.
pub fn from_text(text: &str, layout: &TextLayout) -> Result<Document> {
    let mut flow = Flow::new(&layout.setup)?;
    let font = flow
        .builder
        .fonts
        .pick(&flow.builder.doc, text, layout.font, FontStyle::Regular)?;
    let area = flow.area();
    let leading = layout.leading();
    let pen = Pen {
        font,
        size: layout.size,
        color: layout.color,
    };
    let paragraphs: Vec<&str> = normalized(text);
    for (i, paragraph) in paragraphs.iter().enumerate() {
        if i > 0 {
            flow.skip(layout.paragraph_spacing);
        }
        let measure = |s: &str| flow.builder.fonts.width(font, s, layout.size);
        let lines = wrap(paragraph, area.width(), layout.hyphenate, &measure);
        let last = lines.len().saturating_sub(1);
        for (k, line) in lines.iter().enumerate() {
            let top = flow.reserve(leading);
            let baseline = Flow::baseline(&flow.builder.fonts, font, layout.size, top, leading);
            draw_line(
                &mut flow,
                pen,
                (layout.align, k != last),
                (area.x0, area.x1),
                (baseline, line),
            );
        }
    }
    let mut builder = flow.builder;
    running_titles(&mut builder, layout)?;
    builder.finish()
}

/// Découpe le texte en paragraphes, une par ligne d'entrée, fins de ligne
/// normalisées. Un texte vide donne un unique paragraphe vide, donc une page
/// blanche plutôt qu'une erreur.
fn normalized(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return vec![""];
    }
    let mut out = Vec::new();
    for part in text.split('\n') {
        out.push(part.strip_suffix('\r').unwrap_or(part));
    }
    // Une fin de ligne finale ne crée pas de paragraphe vide de plus.
    if out.len() > 1 && out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

/// Pose l'en-tête et le pied de page sur toutes les pages, une fois leur
/// nombre connu. Les jetons `{page}` et `{pages}` sont remplacés page par
/// page.
///
/// # Errors
/// Texte hors WinAnsiEncoding sans police système exploitable.
pub(crate) fn running_titles(builder: &mut Builder, layout: &TextLayout) -> Result<()> {
    let header = layout.header.as_deref().unwrap_or_default();
    let footer = layout.footer.as_deref().unwrap_or_default();
    if header.is_empty() && footer.is_empty() {
        return Ok(());
    }
    let total = builder.page_count();
    let size = (layout.size * 0.8).max(6.0);
    let alphabet = format!("{header}{footer}{total}0123456789");
    let font = builder
        .fonts
        .pick(&builder.doc, &alphabet, layout.font, FontStyle::Regular)?;
    let area = layout.setup.content_box();
    let (_, page_height) = layout.setup.page_size();
    let grey: Rgb = [0.35, 0.35, 0.35];
    for page in 0..total {
        let tokens = |template: &str| {
            template
                .replace("{page}", &(page + 1).to_string())
                .replace("{pages}", &total.to_string())
        };
        for (template, baseline) in [
            (header, page_height - layout.setup.margins.top * 0.55),
            (footer, layout.setup.margins.bottom * 0.45),
        ] {
            let line = tokens(template);
            if line.is_empty() {
                continue;
            }
            let width = builder.fonts.width(font, &line, size);
            let x = area.x0 + (area.width() - width) / 2.0;
            let pen = Pen {
                font,
                size,
                color: grey,
            };
            let (fonts, content) = builder.draw(page);
            content.text(fonts, pen, (x, baseline), &line);
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::create::paper::{Margins, PageSize};
    use acrux_document::collect_pages;

    /// Texte de toutes les pages d'un document, une chaîne par page.
    fn pages_text(doc: &Document) -> Vec<String> {
        collect_pages(doc)
            .unwrap()
            .iter()
            .map(|p| {
                crate::text::extract_page_text(doc, p)
                    .unwrap()
                    .lines
                    .iter()
                    .map(crate::text::Line::text)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect()
    }

    /// Document relu depuis ses octets, comme le ferait un afficheur.
    fn roundtrip(doc: &Document) -> Document {
        Document::from_bytes(doc.save_full().unwrap()).unwrap()
    }

    #[test]
    fn a_short_text_gives_one_page_whose_text_comes_back_intact() {
        let layout = TextLayout::default();
        let doc = from_text("Bonjour le monde.\nDeuxième ligne.", &layout).unwrap();
        let reread = roundtrip(&doc);
        assert_eq!(collect_pages(&reread).unwrap().len(), 1);
        assert_eq!(pages_text(&reread)[0], "Bonjour le monde.\nDeuxième ligne.");
    }

    #[test]
    fn an_empty_text_still_gives_a_blank_page() {
        let doc = from_text("", &TextLayout::default()).unwrap();
        let reread = roundtrip(&doc);
        assert_eq!(collect_pages(&reread).unwrap().len(), 1);
        assert_eq!(pages_text(&reread)[0], "");
    }

    #[test]
    fn pagination_opens_a_page_exactly_at_the_limit() {
        // Une boîte utile qui accueille exactement trois lignes.
        let layout = TextLayout {
            setup: PageSetup {
                size: PageSize::Custom {
                    width: 300.0,
                    height: 100.0,
                },
                margins: Margins {
                    top: 20.0,
                    bottom: 20.0,
                    left: 20.0,
                    right: 20.0,
                },
                // Une taille libre est redressée comme les autres : sans
                // cela, 300 × 100 deviendrait 100 × 300.
                orientation: crate::create::Orientation::Landscape,
                ..PageSetup::default()
            },
            size: 10.0,
            line_height: 2.0, // interligne de 20 pt, boîte utile de 60 pt
            paragraph_spacing: 0.0,
            ..TextLayout::default()
        };
        let three = "un\ndeux\ntrois";
        let doc = from_text(three, &layout).unwrap();
        assert_eq!(
            collect_pages(&doc).unwrap().len(),
            1,
            "trois lignes tiennent"
        );
        let four = "un\ndeux\ntrois\nquatre";
        let doc = from_text(four, &layout).unwrap();
        let pages = collect_pages(&doc).unwrap();
        assert_eq!(pages.len(), 2, "la quatrième ouvre une page");
        let reread = roundtrip(&doc);
        let texts = pages_text(&reread);
        assert_eq!(texts[0], "un\ndeux\ntrois");
        assert_eq!(texts[1], "quatre");
    }

    #[test]
    fn justification_spares_the_last_line_of_each_paragraph() {
        let layout = TextLayout {
            align: TextAlign::Justify,
            ..TextLayout::default()
        };
        let long = "Le petit chat boit du lait tous les matins dans la cuisine \
                    ensoleillée de la vieille maison de campagne, puis il dort.";
        let doc = from_text(long, &layout).unwrap();
        let reread = roundtrip(&doc);
        let pages = collect_pages(&reread).unwrap();
        let text = crate::text::extract_page_text(&reread, &pages[0]).unwrap();
        assert!(text.lines.len() >= 2, "le texte doit se couper");
        let area = layout.setup.content_box();
        let first = &text.lines[0];
        let first_right = first.words.last().map_or(0.0, |w| w.bbox.x1);
        assert!(
            (first_right - area.x1).abs() < 2.0,
            "la première ligne atteint la marge droite : {first_right} vs {}",
            area.x1
        );
        let last = text.lines.last().unwrap();
        let last_right = last.words.last().map_or(0.0, |w| w.bbox.x1);
        assert!(
            last_right < area.x1 - 5.0,
            "la dernière ligne n'est pas justifiée : {last_right} vs {}",
            area.x1
        );
    }

    #[test]
    fn alignments_place_the_line_where_they_should() {
        let area = TextLayout::default().setup.content_box();
        for (align, check) in [
            (TextAlign::Left, 0u8),
            (TextAlign::Center, 1),
            (TextAlign::Right, 2),
        ] {
            let layout = TextLayout {
                align,
                ..TextLayout::default()
            };
            let doc = roundtrip(&from_text("Court", &layout).unwrap());
            let pages = collect_pages(&doc).unwrap();
            let text = crate::text::extract_page_text(&doc, &pages[0]).unwrap();
            let word = text.words()[0];
            match check {
                0 => assert!((word.bbox.x0 - area.x0).abs() < 2.0, "{:?}", word.bbox),
                1 => {
                    let middle = f64::midpoint(word.bbox.x0, word.bbox.x1);
                    let center = f64::midpoint(area.x0, area.x1);
                    assert!((middle - center).abs() < 2.0, "{:?}", word.bbox);
                }
                _ => assert!((word.bbox.x1 - area.x1).abs() < 2.0, "{:?}", word.bbox),
            }
        }
    }

    #[test]
    fn a_header_and_a_footer_are_stamped_on_every_page() {
        let layout = TextLayout {
            header: Some("Rapport".into()),
            footer: Some("{page} / {pages}".into()),
            setup: PageSetup {
                size: PageSize::A6,
                ..PageSetup::default()
            },
            ..TextLayout::default()
        };
        let body = (0..60)
            .map(|i| format!("Ligne numéro {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let doc = roundtrip(&from_text(&body, &layout).unwrap());
        let pages = collect_pages(&doc).unwrap();
        assert!(pages.len() >= 2);
        let total = pages.len();
        for (i, page) in pages.iter().enumerate() {
            let text = crate::text::extract_page_text(&doc, page).unwrap();
            let all = text
                .lines
                .iter()
                .map(crate::text::Line::text)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(all.contains("Rapport"), "page {} : {all}", i + 1);
            assert!(
                all.contains(&format!("{} / {total}", i + 1)),
                "page {} : {all}",
                i + 1
            );
        }
    }

    #[test]
    fn a_non_latin_text_embeds_a_font_and_stays_extractable() {
        let layout = TextLayout::default();
        let Ok(doc) = from_text("Привет мир", &layout) else {
            // Machine sans police système couvrant le cyrillique.
            return;
        };
        let reread = roundtrip(&doc);
        assert_eq!(pages_text(&reread)[0], "Привет мир");
    }
}
