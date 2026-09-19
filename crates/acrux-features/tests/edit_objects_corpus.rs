//! Édition des objets sur les fichiers réels du corpus.
//!
//! Ce que vérifient ces tests :
//!
//! - l'inventaire tient debout sur tous les fichiers : boîtes finies, plages
//!   d'octets valides et ordonnées ;
//! - déplacer un objet **ne touche à rien d'autre** : hors de la zone qu'il
//!   occupait et de celle qu'il occupe, le rendu est identique au pixel près ;
//! - une transformation identité ne change **pas un pixel** ;
//! - supprimer, réordonner et aligner font ce qu'ils annoncent ;
//! - une édition vide laisse le fichier rigoureusement intact.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use acrux_core::{Matrix, Rect};
use acrux_document::{collect_pages, Document};
use acrux_features::edit_objects::{self, Align, Edit, Kind, Order, PageObject};
use acrux_render::{render_page, RenderOptions};

fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join(name)
}

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

fn reload(doc: &Document) -> Document {
    let bytes = doc
        .save_incremental()
        .or_else(|_| doc.save_full())
        .expect("enregistrement");
    Document::from_bytes(bytes).expect("relecture")
}

/// Rendu d'une page en pixels, avec sa largeur.
fn render(doc: &Document, page: usize) -> (u32, u32, Vec<u8>) {
    let pages = collect_pages(doc).expect("pages");
    let out = render_page(doc, &pages[page], 1.0, &RenderOptions::default());
    (
        out.bitmap.width(),
        out.bitmap.height(),
        out.bitmap.data().to_vec(),
    )
}

/// Composantes qui diffèrent de plus de `tolerance`, **hors** des rectangles
/// donnés (en points PDF, origine en bas à gauche).
fn differences_outside(
    before: &(u32, u32, Vec<u8>),
    after: &(u32, u32, Vec<u8>),
    page_height: f64,
    zones: &[Rect],
    tolerance: u8,
) -> usize {
    assert_eq!(before.0, after.0);
    assert_eq!(before.1, after.1);
    let (w, h) = (before.0, before.1);
    let mut count = 0usize;
    for y in 0..h {
        for x in 0..w {
            // Le rendu est à 1 pixel par point ; l'ordonnée est retournée.
            let px = f64::from(x);
            let py = page_height - f64::from(y);
            if zones.iter().any(|z| {
                px >= z.x0 - 2.0 && px <= z.x1 + 2.0 && py >= z.y0 - 2.0 && py <= z.y1 + 2.0
            }) {
                continue;
            }
            let i = ((y * w + x) * 4) as usize;
            for c in 0..3 {
                if before.2[i + c].abs_diff(after.2[i + c]) > tolerance {
                    count += 1;
                }
            }
        }
    }
    count
}

/// Hauteur de la première page.
fn page_height(doc: &Document) -> f64 {
    let pages = collect_pages(doc).expect("pages");
    pages[0].crop_box(doc).height()
}

/// Vrai si la première page est posée droite.
///
/// Les tests qui comparent des **zones** du rendu supposent qu'un point de la
/// page correspond à un pixel au même endroit ; une page `/Rotate 90` casse
/// cette correspondance, et c'est le test qui serait faux, pas le code.
fn upright(doc: &Document) -> bool {
    let pages = collect_pages(doc).expect("pages");
    pages[0].rotate(doc) % 360 == 0
}

/// L'objet le plus large qui n'est ni rogné, ni minuscule, ni un fond de page :
/// c'est celui dont le déplacement se verra à coup sûr dans le rendu.
///
/// Un aplat qui couvre toute la page se déplace sans que rien ne change —
/// c'est du blanc sur du blanc — et ne prouverait rien.
fn pick_on(objects: &[PageObject], page: Rect) -> Option<&PageObject> {
    let page_area = (page.width() * page.height()).max(1.0);
    objects
        .iter()
        .filter(|o| {
            !o.cut()
                && o.bbox.width() > 12.0
                && o.bbox.height() > 12.0
                && o.bbox.width() * o.bbox.height() < page_area * 0.9
        })
        .max_by(|a, b| {
            let area = |o: &PageObject| o.bbox.width() * o.bbox.height();
            area(a)
                .partial_cmp(&area(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// Comme [`pick_on`], pour la première page du document.
fn pick(doc: &Document, objects: &[PageObject]) -> Option<usize> {
    let pages = collect_pages(doc).expect("pages");
    pick_on(objects, pages[0].crop_box(doc)).map(|o| o.index)
}

// ---------------------------------------------------------------------------
// Inventaire.
// ---------------------------------------------------------------------------

#[test]
fn linventaire_tient_debout_sur_tout_le_corpus() {
    let files = corpus_files();
    assert!(!files.is_empty(), "corpus vide");
    let mut seen = 0usize;
    for path in files {
        let doc = Document::load(&path).expect("chargement");
        for (page_index, objects) in edit_objects::list_all(&doc).expect("inventaire") {
            for (n, o) in objects.iter().enumerate() {
                assert_eq!(o.index, n, "{}", path.display());
                assert!(
                    o.range.0 < o.range.1,
                    "{} page {} objet {n} : plage vide",
                    path.display(),
                    page_index + 1
                );
                assert!(o.ops.0 < o.ops.1);
                for v in [o.bbox.x0, o.bbox.y0, o.bbox.x1, o.bbox.y1] {
                    assert!(v.is_finite(), "{} : boîte non finie", path.display());
                }
                assert!(o.matrix.determinant().is_finite());
                seen += 1;
            }
        }
    }
    assert!(seen > 100, "inventaire suspicieusement vide : {seen}");
}

#[test]
fn les_blocs_de_texte_ont_la_boite_de_leurs_glyphes() {
    let path = corpus("reels/chrome-skia-2pages-texte-tableau-svg.pdf");
    let doc = Document::load(&path).expect("chargement");
    let pages = collect_pages(&doc).expect("pages");
    let objects = edit_objects::list(&doc, &pages[0]).expect("inventaire");
    let blocks: Vec<&PageObject> = objects.iter().filter(|o| o.kind == Kind::Text).collect();
    assert!(blocks.len() > 3, "{} blocs", blocks.len());
    // Chaque bloc a une boîte réelle, et deux blocs différents ne partagent
    // pas la même : l'appariement se fait bien par opération.
    for b in &blocks {
        assert!(
            b.bbox.width() > 0.5 && b.bbox.height() > 0.5,
            "bloc {} sans boîte",
            b.index
        );
    }
    let first = blocks[0].bbox;
    assert!(
        blocks[1..]
            .iter()
            .any(|b| (b.bbox.y0 - first.y0).abs() > 1.0),
        "tous les blocs à la même hauteur : l'appariement a échoué"
    );
}

// ---------------------------------------------------------------------------
// Transformations.
// ---------------------------------------------------------------------------

#[test]
fn une_transformation_identite_ne_change_pas_un_pixel() {
    for path in corpus_files() {
        let doc = Document::load(&path).expect("chargement");
        let pages = collect_pages(&doc).expect("pages");
        let objects = edit_objects::list(&doc, &pages[0]).expect("inventaire");
        let Some(index) = pick(&doc, &objects) else {
            continue;
        };
        let object = &objects[index];
        let before = render(&doc, 0);
        edit_objects::apply(
            &doc,
            &pages[0],
            &[Edit::Transform {
                index: object.index,
                matrix: Matrix::IDENTITY,
            }],
        )
        .expect("transformation");
        let doc = reload(&doc);
        let after = render(&doc, 0);
        assert_eq!(
            differences_outside(&before, &after, 0.0, &[], 0),
            0,
            "l'identité doit être invisible ({})",
            path.display()
        );
    }
}

#[test]
fn deplacer_un_objet_ne_touche_a_rien_dautre() {
    let mut tested = 0usize;
    for path in corpus_files() {
        let doc = Document::load(&path).expect("chargement");
        if !upright(&doc) {
            continue;
        }
        let height = page_height(&doc);
        let pages = collect_pages(&doc).expect("pages");
        let objects = edit_objects::list(&doc, &pages[0]).expect("inventaire");
        let Some(index) = pick(&doc, &objects) else {
            continue;
        };
        let old = objects[index].bbox;
        let (dx, dy) = (23.0, -17.0);
        let new = Rect::new(old.x0 + dx, old.y0 + dy, old.x1 + dx, old.y1 + dy);

        let before = render(&doc, 0);
        edit_objects::translate(&doc, &pages[0], index, dx, dy).expect("déplacement");
        let doc = reload(&doc);
        let after = render(&doc, 0);

        // Hors de l'ancienne et de la nouvelle place, rien n'a le droit de
        // bouger — pas même d'un pas de quantification.
        let outside = differences_outside(&before, &after, height, &[old, new], 1);
        assert_eq!(
            outside,
            0,
            "{} : {outside} composantes ont bougé hors de la zone déplacée",
            path.display()
        );
        // Et quelque chose a bien bougé.
        let inside = differences_outside(&before, &after, height, &[], 8);
        assert!(inside > 20, "{} : rien n'a bougé", path.display());
        tested += 1;
    }
    assert!(tested >= 4, "trop peu de fichiers éprouvés : {tested}");
}

#[test]
fn agrandir_se_fait_autour_du_centre() {
    let path = corpus("reels/chrome-skia-images-jpeg-png-tirets-opacite-cjk.pdf");
    let doc = Document::load(&path).expect("chargement");
    let pages = collect_pages(&doc).expect("pages");
    let objects = edit_objects::list(&doc, &pages[0]).expect("inventaire");
    let image = objects
        .iter()
        .find(|o| o.kind == Kind::Image && o.bbox.width() > 50.0)
        .expect("une image");
    let (index, before) = (image.index, image.bbox);
    edit_objects::scale(&doc, &pages[0], index, 2.0, 2.0).expect("échelle");

    let doc = reload(&doc);
    let pages = collect_pages(&doc).expect("pages");
    let after = edit_objects::list(&doc, &pages[0]).expect("inventaire")[index].bbox;
    assert!(
        (after.width() - before.width() * 2.0).abs() < 0.1,
        "{after:?}"
    );
    assert!((after.height() - before.height() * 2.0).abs() < 0.1);
    // Le centre n'a pas bougé.
    assert!((f64::midpoint(after.x0, after.x1) - f64::midpoint(before.x0, before.x1)).abs() < 0.1);
    assert!((f64::midpoint(after.y0, after.y1) - f64::midpoint(before.y0, before.y1)).abs() < 0.1);
}

#[test]
fn placer_amene_lobjet_exactement_dans_le_rectangle() {
    let path = corpus("reels/chrome-skia-images-jpeg-png-tirets-opacite-cjk.pdf");
    let doc = Document::load(&path).expect("chargement");
    let pages = collect_pages(&doc).expect("pages");
    let objects = edit_objects::list(&doc, &pages[0]).expect("inventaire");
    let image = objects
        .iter()
        .find(|o| o.kind == Kind::Image)
        .expect("une image");
    let target = Rect::new(300.0, 120.0, 420.0, 200.0);
    edit_objects::place(&doc, &pages[0], image.index, target).expect("placement");

    let doc = reload(&doc);
    let pages = collect_pages(&doc).expect("pages");
    let after = edit_objects::list(&doc, &pages[0]).expect("inventaire")[image.index].bbox;
    for (got, want) in [
        (after.x0, target.x0),
        (after.y0, target.y0),
        (after.x1, target.x1),
        (after.y1, target.y1),
    ] {
        assert!((got - want).abs() < 0.05, "{got} contre {want}");
    }
}

// ---------------------------------------------------------------------------
// Suppression, ordre, alignement.
// ---------------------------------------------------------------------------

#[test]
fn supprimer_retire_lobjet_et_rien_dautre() {
    let path = corpus("reels/chrome-skia-images-jpeg-png-tirets-opacite-cjk.pdf");
    let doc = Document::load(&path).expect("chargement");
    let height = page_height(&doc);
    let pages = collect_pages(&doc).expect("pages");
    let objects = edit_objects::list(&doc, &pages[0]).expect("inventaire");
    let image = objects
        .iter()
        .find(|o| o.kind == Kind::Image && o.bbox.width() > 50.0)
        .expect("une image");
    let zone = image.bbox;
    let before = render(&doc, 0);
    edit_objects::apply(&doc, &pages[0], &[Edit::Delete { index: image.index }])
        .expect("suppression");
    let doc = reload(&doc);
    let after = render(&doc, 0);

    assert_eq!(
        differences_outside(&before, &after, height, &[zone], 1),
        0,
        "la suppression a débordé de l'objet"
    );
    assert!(
        differences_outside(&before, &after, height, &[], 8) > 100,
        "l'objet n'a pas disparu"
    );
    // L'inventaire a bien un objet de moins.
    let pages = collect_pages(&doc).expect("pages");
    let left = edit_objects::list(&doc, &pages[0]).expect("inventaire");
    assert_eq!(left.len(), objects.len() - 1);
}

/// Changer l'ordre d'un objet que rien ne recouvre ne doit **rien** changer
/// au rendu : il est réémis ailleurs dans le flux, mais exactement au même
/// endroit de la page.
///
/// C'est le test qui attrape l'erreur classique : réémettre la matrice
/// absolue de l'objet à un endroit où une matrice racine est déjà en vigueur
/// — beaucoup de producteurs en posent une qu'ils ne referment jamais — et
/// l'appliquer ainsi deux fois, ce qui envoie l'objet hors de la page.
#[test]
fn reordonner_ne_deplace_pas_lobjet() {
    let mut tested = 0usize;
    for path in corpus_files() {
        let doc = Document::load(&path).expect("chargement");
        let pages = collect_pages(&doc).expect("pages");
        let objects = edit_objects::list(&doc, &pages[0]).expect("inventaire");
        // Un objet que rien d'autre ne recouvre : son ordre n'a donc aucune
        // conséquence visible.
        let alone = objects.iter().find(|o| {
            o.movable_in_order()
                && o.depth == 0
                && o.bbox.width() > 12.0
                && o.bbox.height() > 12.0
                && objects
                    .iter()
                    .filter(|other| other.index != o.index)
                    .all(|other| other.bbox.intersect(&o.bbox).is_empty())
        });
        let Some(object) = alone else { continue };
        let index = object.index;
        let before = render(&doc, 0);
        edit_objects::apply(
            &doc,
            &pages[0],
            &[Edit::Arrange {
                index,
                to: Order::Front,
            }],
        )
        .expect("ordre");
        let doc = reload(&doc);
        let after = render(&doc, 0);
        assert_eq!(
            differences_outside(&before, &after, 0.0, &[], 1),
            0,
            "{} : réordonner a déplacé l'objet",
            path.display()
        );
        tested += 1;
    }
    assert!(tested >= 2, "trop peu de fichiers éprouvés : {tested}");
}

#[test]
fn envoyer_devant_met_lobjet_au_dessus() {
    let path = corpus("reels/chrome-skia-images-jpeg-png-tirets-opacite-cjk.pdf");
    let doc = Document::load(&path).expect("chargement");
    let pages = collect_pages(&doc).expect("pages");
    let objects = edit_objects::list(&doc, &pages[0]).expect("inventaire");
    let image = objects
        .iter()
        .find(|o| o.kind == Kind::Image && o.movable_in_order() && o.bbox.width() > 50.0)
        .expect("une image déplaçable");
    let index = image.index;
    // On l'amène par-dessus un objet dessiné après lui, puis tout devant.
    let later = objects
        .iter()
        .rev()
        .find(|o| o.index > index && o.bbox.width() > 20.0)
        .expect("un objet plus tardif");
    let target = later.bbox;
    edit_objects::place(&doc, &pages[0], index, target).expect("placement");
    let doc = reload(&doc);
    let pages = collect_pages(&doc).expect("pages");
    let covered = render(&doc, 0);

    edit_objects::apply(
        &doc,
        &pages[0],
        &[Edit::Arrange {
            index,
            to: Order::Front,
        }],
    )
    .expect("ordre");
    let doc = reload(&doc);
    let on_top = render(&doc, 0);
    assert!(
        differences_outside(&covered, &on_top, 0.0, &[], 8) > 50,
        "passer devant n'a rien changé"
    );
    // L'objet est désormais le dernier dessiné.
    let pages = collect_pages(&doc).expect("pages");
    let after = edit_objects::list(&doc, &pages[0]).expect("inventaire");
    let last = after.last().expect("au moins un objet");
    assert_eq!(last.kind, Kind::Image, "le dernier objet doit être l'image");
}

#[test]
fn aligner_fait_glisser_sans_deformer() {
    let path = corpus("reels/chrome-skia-images-jpeg-png-tirets-opacite-cjk.pdf");
    let doc = Document::load(&path).expect("chargement");
    let pages = collect_pages(&doc).expect("pages");
    let objects = edit_objects::list(&doc, &pages[0]).expect("inventaire");
    let images: Vec<&PageObject> = objects
        .iter()
        .filter(|o| o.kind == Kind::Image)
        .take(3)
        .collect();
    assert!(images.len() >= 2, "il faut deux images");
    let indices: Vec<usize> = images.iter().map(|o| o.index).collect();
    let sizes: Vec<(f64, f64)> = images
        .iter()
        .map(|o| (o.bbox.width(), o.bbox.height()))
        .collect();

    edit_objects::align(&doc, &pages[0], &indices, Align::Left).expect("alignement");
    let doc = reload(&doc);
    let pages = collect_pages(&doc).expect("pages");
    let after = edit_objects::list(&doc, &pages[0]).expect("inventaire");

    let left = after[indices[0]].bbox.x0;
    for (slot, &index) in indices.iter().enumerate() {
        let b = after[index].bbox;
        assert!((b.x0 - left).abs() < 0.05, "objet {index} mal aligné");
        assert!((b.width() - sizes[slot].0).abs() < 0.05, "largeur changée");
        assert!((b.height() - sizes[slot].1).abs() < 0.05, "hauteur changée");
    }
}

#[test]
fn recadrer_ne_montre_que_le_rectangle_demande() {
    let path = corpus("reels/chrome-skia-images-jpeg-png-tirets-opacite-cjk.pdf");
    let doc = Document::load(&path).expect("chargement");
    let height = page_height(&doc);
    let pages = collect_pages(&doc).expect("pages");
    let objects = edit_objects::list(&doc, &pages[0]).expect("inventaire");
    let image = objects
        .iter()
        .find(|o| o.kind == Kind::Image && o.bbox.width() > 80.0)
        .expect("une image");
    let full = image.bbox;
    // On ne garde que la moitié gauche. Les trois autres bords du rectangle
    // de découpe débordent volontairement de l'image : un bord de découpe
    // posé **exactement** sur le bord d'une image en retirerait les pixels
    // partiellement couverts, et ce serait le test qui aurait tort.
    let middle = f64::midpoint(full.x0, full.x1);
    let keep = Rect::new(full.x0 - 6.0, full.y0 - 6.0, middle, full.y1 + 6.0);
    let before = render(&doc, 0);
    edit_objects::apply(
        &doc,
        &pages[0],
        &[Edit::Clip {
            index: image.index,
            rect: keep,
        }],
    )
    .expect("recadrage");
    let doc = reload(&doc);
    let after = render(&doc, 0);

    // La moitié gardée n'a pas bougé, la moitié coupée a disparu.
    let removed = Rect::new(middle, full.y0, full.x1, full.y1);
    assert_eq!(
        differences_outside(&before, &after, height, &[removed], 1),
        0,
        "le recadrage a débordé de la partie coupée"
    );
    assert!(
        differences_outside(&before, &after, height, &[], 8) > 50,
        "rien n'a été coupé"
    );
}

#[test]
fn une_edition_vide_ne_touche_a_rien() {
    for path in corpus_files() {
        let doc = Document::load(&path).expect("chargement");
        let pages = collect_pages(&doc).expect("pages");
        let before = acrux_render::page::page_content(&doc, &pages[0]);
        assert_eq!(edit_objects::apply(&doc, &pages[0], &[]).expect("rien"), 0);
        let after = acrux_render::page::page_content(&doc, &pages[0]);
        assert_eq!(
            before,
            after,
            "{} : le flux a changé sans édition",
            path.display()
        );
    }
}

/// Une édition **réelle** ne change que ce qu'elle doit : le flux d'avant se
/// retrouve dans celui d'après, à l'encadrement près.
#[test]
fn le_reste_du_flux_est_recopie_octet_pour_octet() {
    for path in corpus_files() {
        let doc = Document::load(&path).expect("chargement");
        let pages = collect_pages(&doc).expect("pages");
        let objects = edit_objects::list(&doc, &pages[0]).expect("inventaire");
        let Some(index) = pick(&doc, &objects) else {
            continue;
        };
        let before = acrux_render::page::page_content(&doc, &pages[0]);
        let (from, to) = objects[index].range;
        edit_objects::translate(&doc, &pages[0], index, 5.0, 5.0).expect("déplacement");
        let after = acrux_render::page::page_content(&doc, &pages[0]);
        // Avant la plage éditée : les mêmes octets, exactement.
        assert_eq!(&after[..from], &before[..from], "{}", path.display());
        // Après : les mêmes octets aussi, à la fin du flux.
        let tail = before.len() - to;
        assert_eq!(
            &after[after.len() - tail..],
            &before[to..],
            "{}",
            path.display()
        );
        // Et la plage éditée s'y retrouve telle quelle, encadrée.
        assert!(
            after.windows(to - from).any(|w| w == &before[from..to]),
            "{} : l'objet n'a pas été recopié tel quel",
            path.display()
        );
    }
}

#[test]
fn un_index_inconnu_est_refuse() {
    let path = corpus("reels/chrome-skia-2pages-texte-tableau-svg.pdf");
    let doc = Document::load(&path).expect("chargement");
    let pages = collect_pages(&doc).expect("pages");
    assert!(edit_objects::apply(&doc, &pages[0], &[Edit::Delete { index: 9_999 }]).is_err());
}
