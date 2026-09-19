//! Filigranes, en-têtes, pieds de page et numérotation Bates sur les fichiers
//! réels du corpus (`tests/corpus/reels/`, `tests/corpus/synthese/`).
//!
//! Ce que vérifient ces tests, sur chaque fichier :
//!
//! - le document **se relit** après la pose et l'enregistrement ;
//! - le **texte d'origine est intact** : pas un glyphe n'a bougé d'un
//!   centième de point ;
//! - le **nouveau texte est extractible** ;
//! - le **rendu hors de la zone du tampon est identique au pixel près** ;
//! - un tampon posé puis retiré rend **exactement** les pixels d'origine.
//!
//! La comparaison se fait au glyphe et non à la ligne : l'extraction de texte
//! regroupe en une seule ligne un tampon posé sur la même ligne de base qu'un
//! texte existant, sans qu'aucun glyphe n'ait pourtant bougé.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use acrux_core::{Point, Rect};
use acrux_document::{collect_pages, Document};
use acrux_features::stamp::{
    add_bates, add_header_footer, add_watermark, remove_stamps, Anchor, BatesOptions,
    HeaderFooterOptions, Opacity, Placement, Scale, StampKind, StampSource, StandardFont,
    WatermarkOptions,
};
use acrux_features::text::{extract_page_text, PageText};
use acrux_render::{render_page, RenderOptions};

fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join(name)
}

/// Fichiers du corpus lisibles sans mot de passe. Les fichiers `casse` sont
/// écartés : ils sont reconstruits par réparation, ce qui n'a rien à voir avec
/// ce qui est testé ici.
fn corpus_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ["reels", "synthese"] {
        let Ok(entries) = std::fs::read_dir(corpus(dir)) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = p.to_string_lossy().to_string();
            if p.extension().is_some_and(|x| x == "pdf")
                && !name.contains("mdp-")
                && !name.contains("casse")
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Enregistre puis relit, comme le ferait un lecteur.
fn reload(doc: &Document) -> Document {
    let bytes = doc
        .save_incremental()
        .or_else(|_| doc.save_full())
        .expect("enregistrement");
    Document::from_bytes(bytes).expect("relecture")
}

/// Glyphes d'une page, dans l'ordre de lecture.
fn glyphs(text: &PageText) -> Vec<(String, Rect)> {
    text.lines
        .iter()
        .flat_map(|l| l.words.iter())
        .flat_map(|w| w.glyphs.iter())
        .map(|g| (g.text.clone(), g.bbox))
        .collect()
}

/// Glyphes de `a` sans équivalent dans `b` : même texte, même boîte à 0,01
/// près, chaque glyphe de `b` ne servant qu'une fois.
fn glyphs_only_in(a: &PageText, b: &PageText) -> Vec<(String, Rect)> {
    let other = glyphs(b);
    let mut used = vec![false; other.len()];
    let mut out = Vec::new();
    for (text, rect) in glyphs(a) {
        let found = other.iter().enumerate().position(|(i, (t, r))| {
            !used[i]
                && *t == text
                && (r.x0 - rect.x0).abs() < 0.01
                && (r.y0 - rect.y0).abs() < 0.01
                && (r.x1 - rect.x1).abs() < 0.01
                && (r.y1 - rect.y1).abs() < 0.01
        });
        match found {
            Some(i) => used[i] = true,
            None => out.push((text, rect)),
        }
    }
    out
}

/// Texte des glyphes ajoutés par le tampon, et boîte qui les contient tous.
fn added(before: &PageText, after: &PageText) -> (String, Vec<Rect>) {
    let new = glyphs_only_in(after, before);
    let text: String = new.iter().map(|(t, _)| t.as_str()).collect();
    let mut zone: Option<Rect> = None;
    for (_, r) in &new {
        zone = Some(match zone {
            Some(z) => z.union(r),
            None => *r,
        });
    }
    (text, zone.into_iter().collect())
}

/// Aucun glyphe d'origine n'a bougé ni disparu.
fn assert_original_glyphs_kept(file: &Path, page: usize, before: &PageText, after: &PageText) {
    let lost = glyphs_only_in(before, after);
    assert!(
        lost.is_empty(),
        "{} page {} : {} glyphe(s) d'origine ont bougé ou disparu, par exemple {:?}",
        file.display(),
        page + 1,
        lost.len(),
        lost.first()
    );
}

/// Nombre de pixels différents hors des zones données (en espace utilisateur),
/// avec une marge de 2 px pour l'anti-aliasing.
fn different_pixels_outside(
    before: &Document,
    after: &Document,
    page: usize,
    zones: &[Rect],
) -> usize {
    let render = |d: &Document| {
        let pages = collect_pages(d).unwrap();
        render_page(d, &pages[page], 1.0, &RenderOptions::default())
    };
    let a = render(before);
    let b = render(after);
    assert_eq!(a.bitmap.width(), b.bitmap.width());
    assert_eq!(a.bitmap.height(), b.bitmap.height());
    let m = a.base_ctm;
    let boxes: Vec<Rect> = zones
        .iter()
        .map(|z| {
            let p0 = m.apply(Point::new(z.x0, z.y0));
            let p1 = m.apply(Point::new(z.x1, z.y1));
            Rect::new(
                p0.x.min(p1.x) - 2.0,
                p0.y.min(p1.y) - 2.0,
                p0.x.max(p1.x) + 2.0,
                p0.y.max(p1.y) + 2.0,
            )
        })
        .collect();
    let mut different = 0;
    for y in 0..a.bitmap.height() {
        for x in 0..a.bitmap.width() {
            let p = Point::new(f64::from(x), f64::from(y));
            if boxes.iter().any(|z| z.contains(p)) {
                continue;
            }
            if a.bitmap.pixel(x, y) != b.bitmap.pixel(x, y) {
                different += 1;
            }
        }
    }
    different
}

/// Numérotation Bates sur tout le corpus : le texte d'origine ne bouge pas,
/// le numéro est extractible, et le rendu ne change qu'autour du numéro.
#[test]
fn bates_leaves_everything_else_untouched() {
    let files = corpus_files();
    assert!(files.len() > 5, "corpus introuvable");
    for path in files {
        let original = Document::load(&path).unwrap();
        let doc = Document::load(&path).unwrap();
        let before: Vec<PageText> = collect_pages(&doc)
            .unwrap()
            .iter()
            .map(|p| extract_page_text(&doc, p).unwrap())
            .collect();
        let report = add_bates(
            &doc,
            &BatesOptions {
                prefix: "AK-".into(),
                digits: 6,
                start: 1,
                anchor: Anchor::BottomRight,
                margin_x: 24.0,
                margin_y: 18.0,
                ..BatesOptions::default()
            },
        )
        .unwrap();
        assert_eq!(report.pages, before.len(), "{}", path.display());
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let saved = reload(&doc);
        let pages = collect_pages(&saved).unwrap();
        assert_eq!(pages.len(), before.len(), "{}", path.display());
        for (index, page) in pages.iter().enumerate() {
            let after = extract_page_text(&saved, page).unwrap();
            assert_original_glyphs_kept(&path, index, &before[index], &after);
            let (new_text, zones) = added(&before[index], &after);
            assert_eq!(
                new_text,
                format!("AK-{:06}", index + 1),
                "{} page {} : numéro non extractible",
                path.display(),
                index + 1
            );
            let different = different_pixels_outside(&original, &saved, index, &zones);
            assert_eq!(
                different,
                0,
                "{} page {} : {different} pixels changés hors du numéro",
                path.display(),
                index + 1
            );
        }
    }
}

/// Un filigrane texte : le texte d'origine ne bouge pas, le filigrane est
/// extractible, et les pixels hors de sa boîte sont inchangés.
#[test]
fn watermark_only_changes_its_own_box() {
    for path in corpus_files() {
        let original = Document::load(&path).unwrap();
        let doc = Document::load(&path).unwrap();
        let before: Vec<PageText> = collect_pages(&doc)
            .unwrap()
            .iter()
            .map(|p| extract_page_text(&doc, p).unwrap())
            .collect();
        add_watermark(
            &doc,
            &WatermarkOptions {
                source: StampSource::Text {
                    text: "EPREUVE".into(),
                    font: StandardFont::HelveticaBold,
                    size: 18.0,
                    color: [0.1, 0.3, 0.8],
                },
                placement: Placement {
                    anchor: Anchor::TopLeft,
                    margin: 6.0,
                    scale: Scale::Absolute(1.0),
                    ..Placement::default()
                },
                opacity: Opacity {
                    fill: 0.5,
                    stroke: 0.5,
                },
                behind: false,
                pages: Vec::new(),
            },
        )
        .unwrap();
        let saved = reload(&doc);
        for (index, page) in collect_pages(&saved).unwrap().iter().enumerate() {
            let after = extract_page_text(&saved, page).unwrap();
            assert_original_glyphs_kept(&path, index, &before[index], &after);
            let (new_text, zones) = added(&before[index], &after);
            assert_eq!(
                new_text,
                "EPREUVE",
                "{} page {} : filigrane non extractible",
                path.display(),
                index + 1
            );
            let different = different_pixels_outside(&original, &saved, index, &zones);
            assert_eq!(
                different,
                0,
                "{} page {} : {different} pixels changés hors du filigrane",
                path.display(),
                index + 1
            );
        }
    }
}

/// Toutes les natures de tampon, pour un retrait complet.
const ALL_KINDS: [StampKind; 4] = [
    StampKind::Watermark,
    StampKind::Background,
    StampKind::HeaderFooter,
    StampKind::Bates,
];

/// Filigrane, en-tête et numéro Bates posés puis retirés : le rendu redevient
/// **exactement** celui du fichier d'origine.
///
/// Un fichier du corpus (`filigrane-entete-bates.pdf`) porte déjà des tampons :
/// la référence est donc ce même fichier une fois **tous** ses tampons retirés,
/// et le compte attendu tient compte de ceux qui s'y trouvaient déjà.
#[test]
fn stamps_are_reversible_on_the_corpus() {
    for path in corpus_files() {
        let original = Document::load(&path).unwrap();
        let already = remove_stamps(&original, &ALL_KINDS).unwrap();
        let original = if already == 0 {
            original
        } else {
            reload(&original)
        };
        let doc = Document::load(&path).unwrap();
        add_watermark(&doc, &WatermarkOptions::default()).unwrap();
        add_header_footer(
            &doc,
            &HeaderFooterOptions {
                header: ["{filename}".into(), String::new(), "{date}".into()],
                footer: [String::new(), "{page} / {pages}".into(), String::new()],
                file_name: "essai.pdf".into(),
                ..HeaderFooterOptions::default()
            },
        )
        .unwrap();
        add_bates(&doc, &BatesOptions::default()).unwrap();
        let pages = collect_pages(&doc).unwrap().len();
        let removed = remove_stamps(&doc, &ALL_KINDS).unwrap();
        assert_eq!(removed, already + pages * 3, "{}", path.display());
        let saved = reload(&doc);
        for index in 0..pages {
            let different = different_pixels_outside(&original, &saved, index, &[]);
            assert_eq!(
                different,
                0,
                "{} page {} : {different} pixels ne sont pas revenus à l'état d'origine",
                path.display(),
                index + 1
            );
        }
    }
}
