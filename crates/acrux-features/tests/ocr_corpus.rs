//! Lire le texte d'une page qui n'est qu'une image.
//!
//! Le fichier d'épreuve est une page du corpus rendue en 150 ppp puis posée
//! comme image : exactement ce qu'on reçoit d'un scanneur ou d'une capture.
//! Ce que l'on vérifie n'est pas « la reconnaissance est parfaite » — aucune
//! ne l'est — mais qu'elle est **bonne et bien placée** : les lignes tombent
//! là où elles sont sur la page, et leur texte ressemble à l'original.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use acrux_document::{collect_pages, Document};
use acrux_features::ocr;

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus")
        .join(name)
}

/// Distance d'édition entre deux textes, rapportée au plus long.
fn distance(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0_usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current[j + 1] = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        previous.clone_from(&current);
    }
    #[allow(clippy::cast_precision_loss)]
    let longest = a.len().max(b.len()) as f64;
    #[allow(clippy::cast_precision_loss)]
    let d = previous[b.len()] as f64;
    d / longest
}

#[test]
fn une_page_en_image_se_relit() {
    if ocr::shapes::faces().is_empty() {
        return; // machine sans polices système : rien à comparer
    }
    let vrai = Document::load(corpus(
        "reels/chrome-skia-deux-colonnes-entete-pied-cesure.pdf",
    ))
    .unwrap();
    let pages_vrai = collect_pages(&vrai).unwrap();
    let texte = acrux_features::text::extract_page_text(&vrai, &pages_vrai[0]).unwrap();
    let vraies: Vec<String> = texte
        .lines
        .iter()
        .map(acrux_features::text::Line::text)
        .filter(|l| l.trim().len() > 3)
        .collect();

    let doc = Document::load(corpus("synthese/texte-en-image.pdf")).unwrap();
    let pages = collect_pages(&doc).unwrap();
    let read = ocr::read_page(&doc, &pages[0]).unwrap();
    assert_eq!(read.len(), 1, "une image, une lecture");
    let lines = &read[0].lines;
    assert!(lines.len() > 25, "seulement {} ligne(s) lue(s)", lines.len());

    // Chaque ligne lue doit ressembler à une ligne de l'original : c'est la
    // seule mesure qui vaille, et 15 % d'écart moyen laisse déjà voir
    // exactement de quoi il s'agit.
    let mut somme = 0.0;
    for line in lines {
        somme += vraies
            .iter()
            .map(|v| distance(&line.text, v))
            .fold(1.0_f64, f64::min);
    }
    #[allow(clippy::cast_precision_loss)]
    let moyenne = somme / lines.len() as f64;
    assert!(moyenne < 0.15, "lecture trop approximative ({moyenne:.3})");

    // Un quart des lignes au moins doit être lu **exactement** — sans quoi
    // la lecture serait seulement ressemblante, et non utilisable.
    let exactes = lines
        .iter()
        .filter(|l| vraies.iter().any(|v| v.trim() == l.text.trim()))
        .count();
    assert!(
        exactes * 4 >= lines.len(),
        "seulement {exactes} ligne(s) exacte(s) sur {}",
        lines.len()
    );

    // Les lignes sont posées dans la page, avec un corps plausible.
    let crop = pages[0].crop_box(&doc);
    for line in lines {
        assert!(
            line.bbox.x0 >= crop.x0 - 1.0 && line.bbox.x1 <= crop.x1 + 1.0,
            "ligne hors de la page : {:?}",
            line.bbox
        );
        assert!(line.size > 2.0 && line.size < 80.0, "corps {}", line.size);
        assert!(line.baseline >= line.bbox.y0 - 1.0 && line.baseline <= line.bbox.y1);
    }
}

#[test]
fn les_lignes_lues_savent_ou_elles_sont() {
    if ocr::shapes::faces().is_empty() {
        return;
    }
    let doc = Document::load(corpus("synthese/texte-en-image.pdf")).unwrap();
    let pages = collect_pages(&doc).unwrap();
    let read = ocr::read_page(&doc, &pages[0]).unwrap();
    let line = read[0]
        .lines
        .iter()
        .find(|l| l.confidence >= ocr::TRUSTED)
        .expect("au moins une ligne sûre");
    // Un point au milieu de la ligne doit retrouver cette ligne-là.
    let x = f64::midpoint(line.bbox.x0, line.bbox.x1);
    let y = f64::midpoint(line.bbox.y0, line.bbox.y1);
    let found = ocr::line_at(&read, x, y).expect("ligne sous le point");
    assert_eq!(found.text, line.text);
    // La ligne de base est dans la boîte, près du bas.
    assert!(line.baseline >= line.bbox.y0 - 1.0 && line.baseline <= line.bbox.y1);
}

#[test]
fn couvrir_une_ligne_ne_touche_pas_limage() {
    let doc = Document::load(corpus("synthese/texte-en-image.pdf")).unwrap();
    let pages = collect_pages(&doc).unwrap();
    let before = acrux_features::edit_objects::list(&doc, &pages[0]).unwrap().len();
    let rect = acrux_core::Rect::new(100.0, 700.0, 300.0, 720.0);
    ocr::mask(&doc, &pages[0], rect, [1.0, 1.0, 1.0]).unwrap();
    let after = acrux_features::edit_objects::list(&doc, &pages[0]).unwrap();
    assert_eq!(
        after.len(),
        before + 1,
        "le masque ajoute un tracé, et rien d'autre"
    );
    assert!(after
        .iter()
        .any(|o| matches!(o.kind, acrux_features::edit_objects::Kind::Image)));
}

/// Affiche ce qui a été lu (`cargo test -p acrux-features --test ocr_corpus
/// -- --ignored --nocapture voir_la_lecture`).
#[test]
#[ignore = "sortie de mise au point"]
fn voir_la_lecture() {
    let doc = Document::load(corpus("synthese/texte-en-image.pdf")).unwrap();
    let pages = collect_pages(&doc).unwrap();
    let read = ocr::read_page(&doc, &pages[0]).unwrap();
    for image in &read {
        for line in &image.lines {
            println!(
                "{:>5.2} {:>6.1} {:>6.1} {:>5.1}pt {} | {}",
                line.confidence,
                line.bbox.x0,
                line.baseline,
                line.size,
                line.family,
                line.text
            );
        }
    }
}

/// Affiche la grille d'un modèle et celle d'une tache de l'image.
#[test]
#[ignore = "sortie de mise au point"]
fn voir_les_grilles() {
    let Some(face) = ocr::shapes::faces().iter().find(|f| f.family.starts_with("Times")) else {
        return;
    };
    for c in ['p', 'e', 'n'] {
        if let Some(grid) = ocr::shapes::template_grid(face, c) {
            println!("--- modèle « {c} » ---\n{}", ocr::shapes::ascii(&grid));
        }
    }
}

/// Affiche les premières taches de l'image telles qu'elles sont comparées.
#[test]
#[ignore = "sortie de mise au point"]
fn voir_les_taches() {
    let doc = Document::load(corpus("synthese/texte-en-image.pdf")).unwrap();
    let pages = collect_pages(&doc).unwrap();
    for (label, art) in ocr::probe_grids(&doc, &pages[0], 5) {
        println!("--- {label} ---\n{art}");
    }
}

/// Affiche les candidats des premières taches du titre.
#[test]
#[ignore = "sortie de mise au point"]
fn voir_les_candidats() {
    let doc = Document::load(corpus("synthese/texte-en-image.pdf")).unwrap();
    let pages = collect_pages(&doc).unwrap();
    for line in ocr::probe_ranking(&doc, &pages[0], 2, 6) {
        println!("{line}");
    }
}

/// Qualité de la lecture d'une vraie page, chiffrée.
///
/// Chaque ligne lue est rapprochée de la ligne d'origine la plus proche — le
/// fichier d'épreuve est le rendu d'une page dont on a le texte — et l'on
/// affiche l'écart moyen. C'est le nombre à faire baisser.
#[test]
#[ignore = "banc d'essai"]
fn le_banc_de_la_page() {
    let vrai = Document::load(corpus("reels/chrome-skia-deux-colonnes-entete-pied-cesure.pdf")).unwrap();
    let pages_vrai = collect_pages(&vrai).unwrap();
    let texte = acrux_features::text::extract_page_text(&vrai, &pages_vrai[0]).unwrap();
    let lignes: Vec<String> = texte
        .lines
        .iter()
        .map(acrux_features::text::Line::text)
        .filter(|l| l.trim().len() > 3)
        .collect();
    let doc = Document::load(corpus("synthese/texte-en-image.pdf")).unwrap();
    let pages = collect_pages(&doc).unwrap();
    let read = ocr::read_page(&doc, &pages[0]).unwrap();
    let (mut somme, mut n) = (0.0_f64, 0_u32);
    for image in &read {
        for line in &image.lines {
            let (meilleur, ecart) = lignes
                .iter()
                .map(|v| (v.as_str(), distance(&line.text, v)))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap_or(("", 1.0));
            println!("{ecart:.2} lu   : {}", line.text);
            println!("     vrai : {meilleur}");
            somme += ecart;
            n += 1;
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let moyenne = somme / f64::from(n.max(1));
    println!("--- écart moyen : {moyenne:.3} sur {n} ligne(s) ---");
}
