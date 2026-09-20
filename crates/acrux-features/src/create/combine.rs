//! Combinaison de fichiers en un seul document (inventaire Acrobat §1,
//! « Combiner des fichiers »).
//!
//! Chaque entrée — un PDF, une image ou du texte — est d'abord convertie en
//! document par les créateurs de ce module, puis toutes les pages sont
//! réunies par [`crate::pages::merge`].
//!
//! Trois garnitures facultatives :
//!
//! - **signets** : un par fichier, portant son nom, pointant sur sa première
//!   page ;
//! - **sommaire** : une ou plusieurs pages en tête, une ligne par fichier
//!   avec son numéro de page et un **lien cliquable** ;
//! - **numérotation continue** : un pied de page `n / total` posé sur toutes
//!   les pages par [`crate::stamp::add_header_footer`], la numérotation
//!   traversant les fichiers d'origine.
//!
//! Le sommaire pose un problème d'œuf et de poule : sa longueur décale les
//! pages qu'il annonce. Il est donc composé, mesuré, puis recomposé avec les
//! numéros corrigés, jusqu'à ce que sa longueur se stabilise (trois tours
//! suffisent en pratique, huit sont autorisés).

use acrux_core::{Error, Rect, Result};
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef};

use super::builder::{write_outlines, Pen};
use super::images::{from_images, ImageInput, ImageLayout};
use super::markdown::from_markdown;
use super::text::{from_text, Flow, TextLayout};
use crate::stamp::metrics::StandardFont;
use crate::stamp::{add_header_footer, HeaderFooterOptions, NumberFormat};

/// Nature d'une entrée à combiner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CombineSource {
    /// Un document PDF, repris tel quel.
    Pdf(Vec<u8>),
    /// Une image (PNG, JPEG, BMP, GIF, TIFF), mise en page par
    /// [`from_images`].
    Image(Vec<u8>),
    /// Du texte brut, mis en page par [`from_text`].
    Text(String),
    /// Du Markdown, mis en page par [`from_markdown`].
    Markdown(String),
}

/// Un fichier à combiner, avec le nom qui le désignera dans les signets et le
/// sommaire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombineInput {
    /// Nom affiché.
    pub name: String,
    /// Contenu.
    pub source: CombineSource,
}

impl CombineInput {
    /// Lit un fichier et devine sa nature : d'abord ses octets de tête
    /// (`%PDF`, signature PNG, `FF D8` d'un JPEG), puis son extension
    /// (`.md` pour du Markdown, tout le reste pour du texte).
    ///
    /// # Errors
    /// Fichier illisible, ou fichier texte qui n'est pas de l'UTF-8.
    pub fn from_path(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let path = path.as_ref();
        let data =
            std::fs::read(path).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let source = if data.starts_with(b"%PDF") {
            CombineSource::Pdf(data)
        } else if data.starts_with(&[0x89, b'P', b'N', b'G']) || data.starts_with(&[0xFF, 0xD8]) {
            CombineSource::Image(data)
        } else {
            let text = String::from_utf8(data).map_err(|_| {
                Error::Unsupported(format!(
                    "{} : format inconnu (ni PDF, ni image, ni texte UTF-8)",
                    path.display()
                ))
            })?;
            if extension == "md" || extension == "markdown" {
                CombineSource::Markdown(text)
            } else {
                CombineSource::Text(text)
            }
        };
        Ok(CombineInput { name, source })
    }
}

/// Réglages de la combinaison.
#[derive(Debug, Clone, PartialEq)]
pub struct CombineOptions {
    /// Poser un signet par fichier, portant son nom.
    pub bookmarks: bool,
    /// Engendrer un sommaire cliquable en tête du document.
    pub table_of_contents: bool,
    /// Titre du sommaire.
    pub toc_title: String,
    /// Poser un pied de page `n / total` sur toutes les pages.
    pub page_numbers: bool,
    /// Mise en page des entrées image.
    pub image_layout: ImageLayout,
    /// Mise en page des entrées texte et Markdown ; c'est aussi elle qui
    /// règle le format et la police du sommaire.
    pub text_layout: TextLayout,
}

impl Default for CombineOptions {
    /// Signets par fichier, pas de sommaire, pas de numérotation.
    fn default() -> Self {
        CombineOptions {
            bookmarks: true,
            table_of_contents: false,
            toc_title: "Sommaire".into(),
            page_numbers: false,
            image_layout: ImageLayout::default(),
            text_layout: TextLayout::default(),
        }
    }
}

/// Lien du sommaire : page du sommaire qui le porte, rectangle cliquable,
/// index de la page visée dans le document final.
type TocLink = (usize, Rect, usize);

/// Sommaire composé : son document, son nombre de pages et ses liens.
type Toc = (Document, usize, Vec<TocLink>);

/// Une entrée du sommaire, une fois les numéros de page connus.
#[derive(Debug, Clone)]
struct Entry {
    /// Nom du fichier.
    name: String,
    /// Index, dans le document final, de sa première page.
    start: usize,
}

/// Combine plusieurs fichiers en un document.
///
/// # Errors
/// Liste vide, entrée illisible (PDF corrompu, image d'un format inconnu),
/// ou assemblage impossible.
pub fn combine(inputs: &[CombineInput], options: &CombineOptions) -> Result<Document> {
    if inputs.is_empty() {
        return Err(Error::Corrupt("aucun fichier à combiner".into()));
    }
    let parts: Vec<Document> = inputs
        .iter()
        .map(|input| convert(input, options))
        .collect::<Result<Vec<_>>>()?;
    let counts: Vec<usize> = parts
        .iter()
        .map(|d| collect_pages(d).map(|p| p.len()))
        .collect::<Result<Vec<_>>>()?;

    // 1. Le sommaire, dont la longueur décale ce qu'il annonce.
    let toc = if options.table_of_contents {
        Some(stable_toc(inputs, &counts, options)?)
    } else {
        None
    };
    let offset = toc.as_ref().map_or(0, |(_, pages, _)| *pages);
    let entries = entries_of(inputs, &counts, offset);

    // 2. L'assemblage proprement dit.
    let mut all: Vec<&Document> = Vec::with_capacity(parts.len() + 1);
    if let Some((doc, _, _)) = &toc {
        all.push(doc);
    }
    all.extend(parts.iter());
    let merged = crate::pages::merge(&all)?;
    let pages = collect_pages(&merged)?;
    let page_refs: Vec<ObjectRef> = pages.iter().filter_map(|p| p.reference).collect();

    // 3. Les liens du sommaire, une fois les pages définitives en place.
    if let Some((_, _, links)) = &toc {
        for (page, rect, target) in links {
            let Some(target_ref) = page_refs.get(*target) else {
                continue;
            };
            add_goto_link(&merged, &pages, *page, *rect, *target_ref);
        }
    }

    // 4. Les signets.
    if options.bookmarks {
        let bookmarks: Vec<(String, usize)> =
            entries.iter().map(|e| (e.name.clone(), e.start)).collect();
        if let Some(root) = write_outlines(&merged, &bookmarks, &page_refs) {
            let mut catalog = merged.catalog()?;
            catalog.insert(Name::new("Outlines"), Object::Reference(root));
            catalog.insert(
                Name::new("PageMode"),
                Object::Name(Name::new("UseOutlines")),
            );
            let cat_ref = acrux_document::xref::trailer_ref(&merged.trailer(), "Root")
                .ok_or_else(|| Error::Corrupt("document sans /Root".into()))?;
            merged.set(cat_ref, Object::Dict(catalog));
        }
    }

    // 5. La numérotation continue.
    if options.page_numbers {
        add_header_footer(
            &merged,
            &HeaderFooterOptions {
                footer: [String::new(), "{page} / {pages}".into(), String::new()],
                font: StandardFont::Helvetica,
                size: 9.0,
                color: [0.35, 0.35, 0.35],
                margins: options.text_layout.setup.margins,
                start_number: 1,
                format: NumberFormat::Decimal,
                ..HeaderFooterOptions::default()
            },
        )?;
    }
    Ok(merged)
}

/// Convertit une entrée en document.
fn convert(input: &CombineInput, options: &CombineOptions) -> Result<Document> {
    match &input.source {
        CombineSource::Pdf(bytes) => Document::from_bytes(bytes.clone()),
        CombineSource::Image(bytes) => {
            let image = ImageInput {
                data: bytes.clone(),
                name: input.name.clone(),
            };
            from_images(std::slice::from_ref(&image), &options.image_layout)
        }
        CombineSource::Text(text) => from_text(text, &options.text_layout),
        CombineSource::Markdown(text) => from_markdown(text, &options.text_layout),
    }
}

/// Entrées du sommaire : nom du fichier et index de sa première page dans le
/// document final, `offset` pages de sommaire comprises.
fn entries_of(inputs: &[CombineInput], counts: &[usize], offset: usize) -> Vec<Entry> {
    let mut start = offset;
    let mut out = Vec::with_capacity(inputs.len());
    for (input, count) in inputs.iter().zip(counts) {
        out.push(Entry {
            name: input.name.clone(),
            start,
        });
        start += count;
    }
    out
}

/// Compose le sommaire jusqu'à ce que sa longueur ne bouge plus : le document
/// du sommaire, son nombre de pages, et les liens à poser
/// (page du sommaire, rectangle, page visée dans le document final).
fn stable_toc(inputs: &[CombineInput], counts: &[usize], options: &CombineOptions) -> Result<Toc> {
    let mut offset = 1;
    for _ in 0..8 {
        let entries = entries_of(inputs, counts, offset);
        let (doc, links) = build_toc(&entries, options)?;
        let pages = collect_pages(&doc)?.len();
        if pages == offset {
            return Ok((doc, pages, links));
        }
        offset = pages;
    }
    // Longueur oscillante (cas pathologique) : le dernier essai fait foi.
    let entries = entries_of(inputs, counts, offset);
    let (doc, links) = build_toc(&entries, options)?;
    let pages = collect_pages(&doc)?.len();
    Ok((doc, pages, links))
}

/// Compose les pages du sommaire et rend les rectangles de ses liens.
fn build_toc(entries: &[Entry], options: &CombineOptions) -> Result<(Document, Vec<TocLink>)> {
    let layout = &options.text_layout;
    let mut flow = Flow::new(&layout.setup)?;
    let area = flow.area();
    let leading = layout.leading();
    // Tout ce que le sommaire écrira, pour choisir la police une seule fois.
    let alphabet: String = std::iter::once(options.toc_title.clone())
        .chain(
            entries
                .iter()
                .map(|e| format!("{} {}", e.name, e.start + 1)),
        )
        .collect::<Vec<_>>()
        .join(" ");
    let doc = &flow.builder.doc;
    let regular = flow.builder.fonts.pick(
        doc,
        &alphabet,
        layout.font,
        crate::fontembed::FontStyle::Regular,
    )?;
    let doc = &flow.builder.doc;
    let bold = flow.builder.fonts.pick(
        doc,
        &options.toc_title,
        super::markdown::bold_variant(layout.font),
        crate::fontembed::FontStyle::Bold,
    )?;

    // Le titre.
    let title_size = layout.size * 1.6;
    let top = flow.reserve(title_size * 1.5);
    let baseline = Flow::baseline(&flow.builder.fonts, bold, title_size, top, title_size * 1.5);
    let page = flow.page();
    let (fonts, content) = flow.builder.draw(page);
    content.text(
        fonts,
        Pen {
            font: bold,
            size: title_size,
            color: [0.0, 0.0, 0.0],
        },
        (area.x0, baseline),
        &options.toc_title,
    );
    flow.skip(layout.paragraph_spacing);

    let mut links = Vec::with_capacity(entries.len());
    for entry in entries {
        let top = flow.reserve(leading);
        let baseline = Flow::baseline(&flow.builder.fonts, regular, layout.size, top, leading);
        let page = flow.page();
        let number = (entry.start + 1).to_string();
        let number_width = flow.builder.fonts.width(regular, &number, layout.size);
        let name_width = flow.builder.fonts.width(regular, &entry.name, layout.size);
        // Ligne de points entre le nom et le numéro, comme un vrai sommaire.
        let dot = flow.builder.fonts.width(regular, ".", layout.size).max(0.1);
        let gap = (area.x1 - number_width - 4.0) - (area.x0 + name_width + 4.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let dots = if gap > 0.0 {
            ".".repeat((gap / dot).floor().max(0.0) as usize)
        } else {
            String::new()
        };
        let ink = Pen {
            font: regular,
            size: layout.size,
            color: [0.0, 0.0, 0.0],
        };
        let faint = Pen {
            color: [0.65, 0.65, 0.65],
            ..ink
        };
        let (fonts, content) = flow.builder.draw(page);
        content.text(fonts, ink, (area.x0, baseline), &entry.name);
        if !dots.is_empty() {
            content.text(fonts, faint, (area.x0 + name_width + 4.0, baseline), &dots);
        }
        content.text(fonts, ink, (area.x1 - number_width, baseline), &number);
        let descent = flow.builder.fonts.descent(regular) * layout.size / 1000.0;
        let ascent = flow.builder.fonts.ascent(regular) * layout.size / 1000.0;
        links.push((
            page,
            Rect::new(area.x0, baseline + descent, area.x1, baseline + ascent),
            entry.start,
        ));
    }
    let doc = flow.builder.finish()?;
    Ok((doc, links))
}

/// Pose une annotation `/Link` vers une page du même document.
fn add_goto_link(
    doc: &Document,
    pages: &[acrux_document::Page],
    page: usize,
    rect: Rect,
    target: ObjectRef,
) {
    let Some(source) = pages.get(page) else {
        return;
    };
    let Some(source_ref) = source.reference else {
        return;
    };
    let mut action = Dict::new();
    action.insert(Name::new("S"), Object::Name(Name::new("GoTo")));
    action.insert(
        Name::new("D"),
        Object::Array(vec![
            Object::Reference(target),
            Object::Name(Name::new("Fit")),
        ]),
    );
    let mut annot = Dict::new();
    annot.insert(Name::new("Type"), Object::Name(Name::new("Annot")));
    annot.insert(Name::new("Subtype"), Object::Name(Name::new("Link")));
    annot.insert(
        Name::new("Rect"),
        Object::Array(vec![
            Object::Real(rect.x0),
            Object::Real(rect.y0),
            Object::Real(rect.x1),
            Object::Real(rect.y1),
        ]),
    );
    annot.insert(
        Name::new("Border"),
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(0),
        ]),
    );
    annot.insert(Name::new("H"), Object::Name(Name::new("N")));
    annot.insert(Name::new("A"), Object::Dict(action));
    let annot_ref = doc.add(Object::Dict(annot));
    // Le dictionnaire est relu dans le document : plusieurs liens se posent
    // sur la même page, et l'instantané de `collect_pages` ignorerait les
    // précédents.
    let live = doc.get(source_ref).ok();
    let mut dict = live
        .as_ref()
        .and_then(|o| o.as_dict())
        .cloned()
        .unwrap_or_else(|| source.dict.clone());
    let mut annots = match dict.get(&Name::new("Annots")) {
        Some(Object::Array(a)) => a.clone(),
        Some(Object::Reference(r)) => doc
            .get(*r)
            .ok()
            .and_then(|o| o.as_array().map(<[Object]>::to_vec))
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    annots.push(Object::Reference(annot_ref));
    dict.insert(Name::new("Annots"), Object::Array(annots));
    doc.set(source_ref, Object::Dict(dict));
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::create::{new_document, PageSetup};
    use crate::navigation::{outline, page_links, Action, OutlineItem, PageIndex};

    /// Page visée par un signet.
    fn target(item: &OutlineItem) -> Option<usize> {
        match item.action.as_ref()? {
            Action::GoTo(d) => Some(d.page),
            _ => None,
        }
    }

    fn pdf_of(pages: usize) -> Vec<u8> {
        let setup = PageSetup {
            pages,
            ..PageSetup::default()
        };
        new_document(&setup).unwrap().save_full().unwrap()
    }

    fn png() -> Vec<u8> {
        acrux_graphics::encode_png_rgb(4, 4, &[128u8; 48])
    }

    fn inputs() -> Vec<CombineInput> {
        vec![
            CombineInput {
                name: "rapport.pdf".into(),
                source: CombineSource::Pdf(pdf_of(2)),
            },
            CombineInput {
                name: "photo.png".into(),
                source: CombineSource::Image(png()),
            },
            CombineInput {
                name: "notes.md".into(),
                source: CombineSource::Markdown("# Notes\n\nDu texte.".into()),
            },
            CombineInput {
                name: "brut.txt".into(),
                source: CombineSource::Text("Une ligne.".into()),
            },
        ]
    }

    #[test]
    fn pages_are_concatenated_in_order() {
        let doc = combine(
            &inputs(),
            &CombineOptions {
                bookmarks: false,
                ..CombineOptions::default()
            },
        )
        .unwrap();
        let reread = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        assert_eq!(collect_pages(&reread).unwrap().len(), 5, "2 + 1 + 1 + 1");
    }

    #[test]
    fn one_bookmark_per_file_points_at_its_first_page() {
        let doc = combine(&inputs(), &CombineOptions::default()).unwrap();
        let reread = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        let pages = collect_pages(&reread).unwrap();
        let index = PageIndex::new(&pages);
        let items = outline(&reread, &index).unwrap();
        let names: Vec<&str> = items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(names, ["rapport.pdf", "photo.png", "notes.md", "brut.txt"]);
        let targets: Vec<Option<usize>> = items.iter().map(target).collect();
        assert_eq!(targets, [Some(0), Some(2), Some(3), Some(4)]);
    }

    #[test]
    fn a_table_of_contents_shifts_the_pages_and_links_to_them() {
        let options = CombineOptions {
            table_of_contents: true,
            ..CombineOptions::default()
        };
        let doc = combine(&inputs(), &options).unwrap();
        let reread = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        let pages = collect_pages(&reread).unwrap();
        assert_eq!(pages.len(), 6, "une page de sommaire devant les cinq");
        let index = PageIndex::new(&pages);
        let links = page_links(&reread, &pages[0], &index).unwrap();
        assert_eq!(links.len(), 4, "un lien par fichier : {links:?}");
        // Le premier fichier commence après le sommaire.
        let first = links
            .iter()
            .find_map(|l| match &l.action {
                Action::GoTo(d) => Some(d.page),
                _ => None,
            })
            .unwrap();
        assert_eq!(first, 1);
        // Les signets tiennent compte du décalage.
        let items = outline(&reread, &index).unwrap();
        assert_eq!(target(&items[0]), Some(1));
        // Le sommaire nomme bien les fichiers.
        let text = crate::text::extract_page_text(&reread, &pages[0]).unwrap();
        let flat = text
            .lines
            .iter()
            .map(crate::text::Line::text)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(flat.contains("Sommaire"), "{flat}");
        assert!(flat.contains("rapport.pdf"), "{flat}");
    }

    #[test]
    fn continuous_numbering_counts_across_the_files() {
        let options = CombineOptions {
            page_numbers: true,
            bookmarks: false,
            ..CombineOptions::default()
        };
        let doc = combine(&inputs(), &options).unwrap();
        let reread = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        let pages = collect_pages(&reread).unwrap();
        let last = pages.len();
        let text = crate::text::extract_page_text(&reread, &pages[last - 1]).unwrap();
        let flat = text
            .lines
            .iter()
            .map(crate::text::Line::text)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(flat.contains(&format!("{last} / {last}")), "{flat}");
    }

    #[test]
    fn an_empty_list_is_refused() {
        assert!(combine(&[], &CombineOptions::default()).is_err());
    }

    #[test]
    fn a_long_table_of_contents_takes_several_pages_and_stays_consistent() {
        let many: Vec<CombineInput> = (0..80)
            .map(|i| CombineInput {
                name: format!("fichier-{i:03}.txt"),
                source: CombineSource::Text(format!("Contenu {i}")),
            })
            .collect();
        let options = CombineOptions {
            table_of_contents: true,
            ..CombineOptions::default()
        };
        let doc = combine(&many, &options).unwrap();
        let reread = Document::from_bytes(doc.save_full().unwrap()).unwrap();
        let pages = collect_pages(&reread).unwrap();
        let index = PageIndex::new(&pages);
        let toc_pages = pages.len() - 80;
        assert!(toc_pages >= 2, "{toc_pages} page(s) de sommaire");
        // Le premier fichier commence juste après le sommaire.
        let items = outline(&reread, &index).unwrap();
        assert_eq!(target(&items[0]), Some(toc_pages));
        // Tous les liens du sommaire visent une page existante.
        let mut total = 0;
        for page in pages.iter().take(toc_pages) {
            total += page_links(&reread, page, &index).unwrap().len();
        }
        assert_eq!(total, 80);
    }
}
