//! Édition en direct d'un paragraphe, comme la fait l'application : une
//! recomposition **à chaque frappe**, sur de vrais fichiers.
//!
//! Ce qui est vérifié :
//!
//! - après chaque frappe, la page porte exactement le texte tapé ;
//! - le paragraphe se retrouve à chaque frappe, sans que sa boîte rétrécisse ;
//! - le reste de la page ne bouge pas ;
//! - vider un paragraphe puis retaper le fait renaître au même endroit ;
//! - une zone de texte neuve se crée, puis s'édite comme les autres ;
//! - **rejouer** la seule dernière frappe sur le fichier d'origine donne le
//!   même résultat que toute la session : c'est ce sur quoi repose
//!   l'annulation.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;

use acrux_document::{collect_pages, Document};
use acrux_features::edit_text::{
    new_text_frame, normalized, open_paragraph, set_paragraph_text, text_frame_at, NewTextStyle,
    OpenedParagraph,
};
use acrux_features::text::{extract_page_text, PageText};

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus")
        .join(name)
}

/// Texte de toute la page, sans blancs.
fn page_text(doc: &Document, page: usize) -> String {
    let pages = collect_pages(doc).unwrap();
    let text = extract_page_text(doc, &pages[page]).unwrap();
    normalized(
        &text
            .lines
            .iter()
            .map(acrux_features::text::Line::text)
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Premier paragraphe de plusieurs lignes, et le texte de la page.
fn a_paragraph(doc: &Document) -> Option<(usize, PageText, OpenedParagraph)> {
    let pages = collect_pages(doc).ok()?;
    let text = extract_page_text(doc, &pages[0]).ok()?;
    let count = text
        .blocks
        .iter()
        .map(|b| b.paragraphs.len())
        .sum::<usize>();
    for i in 0..count {
        if let Ok(opened) = open_paragraph(doc, &pages[0], &text, i) {
            if opened.text.split_whitespace().count() >= 6 {
                return Some((i, text, opened));
            }
        }
    }
    None
}

/// Tape `extra` à la fin du paragraphe, une frappe à la fois, et rend le
/// texte final et ce qui est dessiné.
fn type_at_end(doc: &Document, opened: &OpenedParagraph, extra: &str) -> (String, String) {
    let mut current = opened.text.clone();
    let mut drawn = opened.drawn.clone();
    for c in extra.chars() {
        current.push(c);
        let pages = collect_pages(doc).unwrap();
        let map = set_paragraph_text(doc, &pages[0], &opened.frame, &drawn, &current)
            .unwrap_or_else(|e| panic!("frappe « {c} » refusée : {e}"));
        assert_eq!(
            map.len(),
            current.chars().count(),
            "une frontière par caractère"
        );
        drawn = normalized(&current);
    }
    (current, drawn)
}

#[test]
fn chaque_frappe_ecrit_le_texte_tape() {
    let mut exerces = 0;
    for name in [
        "reels/chrome-skia-deux-colonnes-entete-pied-cesure.pdf",
        "synthese/comparaison-avant.pdf",
    ] {
        let path = corpus(name);
        let Ok(doc) = Document::load(&path) else {
            panic!("{name} illisible");
        };
        let Some((_, _, opened)) = a_paragraph(&doc) else {
            continue; // Pas de paragraphe de plusieurs mots sur cette page.
        };
        let before = page_text(&doc, 0);
        let (final_text, _) = type_at_end(&doc, &opened, " et encore");
        let after = page_text(&doc, 0);
        // Le texte tapé est là.
        assert!(
            after.contains(&normalized(&final_text)),
            "{name} : le texte tapé n'est pas sur la page"
        );
        // Rien d'autre n'a disparu : tout ce qui était sur la page, hors du
        // paragraphe, y est encore.
        let removed = before.replacen(&opened.drawn, "", 1);
        let kept = after.replacen(&normalized(&final_text), "", 1);
        assert_eq!(removed, kept, "{name} : le reste de la page a changé");
        exerces += 1;
    }
    // Un test qui saute tous ses cas ne prouve rien.
    assert_eq!(
        exerces, 2,
        "les deux fichiers doivent avoir un paragraphe éditable"
    );
}

#[test]
fn rejouer_la_derniere_frappe_donne_le_meme_resultat() {
    let path = corpus("reels/chrome-skia-deux-colonnes-entete-pied-cesure.pdf");
    let live = Document::load(&path).unwrap();
    let Some((_, _, opened)) = a_paragraph(&live) else {
        panic!("aucun paragraphe éditable dans le fichier d'essai");
    };
    let (final_text, _) = type_at_end(&live, &opened, " fin");
    // Un seul coup, depuis le fichier d'origine : c'est ce que fait
    // l'annulation quand elle rejoue l'historique.
    let replay = Document::load(&path).unwrap();
    let pages = collect_pages(&replay).unwrap();
    set_paragraph_text(
        &replay,
        &pages[0],
        &opened.frame,
        &opened.drawn,
        &final_text,
    )
    .unwrap();
    assert_eq!(page_text(&live, 0), page_text(&replay, 0));
}

#[test]
fn un_paragraphe_vide_renait_au_meme_endroit() {
    let path = corpus("synthese/comparaison-avant.pdf");
    let doc = Document::load(&path).unwrap();
    let Some((_, _, opened)) = a_paragraph(&doc) else {
        panic!("aucun paragraphe éditable dans le fichier d'essai");
    };
    let pages = collect_pages(&doc).unwrap();
    // Tout effacer.
    let map = set_paragraph_text(&doc, &pages[0], &opened.frame, &opened.drawn, "").unwrap();
    assert_eq!(map.len(), 0);
    assert!(!page_text(&doc, 0).contains(&opened.drawn));
    // Retaper : le texte revient, dans la même police, à la même ligne de base.
    let pages = collect_pages(&doc).unwrap();
    set_paragraph_text(&doc, &pages[0], &opened.frame, "", "Nouveau").unwrap();
    let pages = collect_pages(&doc).unwrap();
    let text = extract_page_text(&doc, &pages[0]).unwrap();
    let line = text
        .lines
        .iter()
        .find(|l| l.text().contains("Nouveau"))
        .unwrap_or_else(|| panic!("le texte retapé est absent"));
    assert!((line.baseline() - opened.frame.baseline).abs() < 0.5);
}

#[test]
fn une_zone_de_texte_neuve_se_cree_puis_seditte() {
    let path = corpus("synthese/comparaison-avant.pdf");
    let doc = Document::load(&path).unwrap();
    let pages = collect_pages(&doc).unwrap();
    let crop = pages[0].crop_box(&doc);
    // Tout en bas de la page, là où il n'y a rien.
    let (x, y) = (crop.x0 + 60.0, crop.y0 + 40.0);
    let frame = new_text_frame(&doc, &pages[0], x, y, 14.0, [0.1, 0.2, 0.6]);
    let mut drawn = String::new();
    let mut text = String::new();
    for c in "Bonjour, zone neuve".chars() {
        text.push(c);
        let pages = collect_pages(&doc).unwrap();
        set_paragraph_text(&doc, &pages[0], &frame, &drawn, &text)
            .unwrap_or_else(|e| panic!("frappe « {c} » refusée : {e}"));
        drawn = normalized(&text);
    }
    let pages = collect_pages(&doc).unwrap();
    let extracted = extract_page_text(&doc, &pages[0]).unwrap();
    let line = extracted
        .lines
        .iter()
        .find(|l| l.text().contains("zone"))
        .unwrap_or_else(|| panic!("la zone neuve est absente de la page"));
    assert_eq!(normalized(&line.text()), normalized("Bonjour, zone neuve"));
    assert!((line.baseline() - y).abs() < 0.5);
    assert!((line.bbox.x0 - x).abs() < 1.0);
    // La couleur choisie est bien celle du texte.
    let glyph = &line.words[0].glyphs[0];
    assert!(
        (f64::from(glyph.color[2]) - 0.6).abs() < 0.02,
        "{:?}",
        glyph.color
    );
}

#[test]
fn un_retour_a_la_ligne_tape_ajoute_une_ligne() {
    let path = corpus("synthese/comparaison-avant.pdf");
    let doc = Document::load(&path).unwrap();
    let pages = collect_pages(&doc).unwrap();
    let crop = pages[0].crop_box(&doc);
    let frame = new_text_frame(
        &doc,
        &pages[0],
        crop.x0 + 60.0,
        crop.y0 + 80.0,
        12.0,
        [0.0; 3],
    );
    let map = set_paragraph_text(&doc, &pages[0], &frame, "", "première\nseconde").unwrap();
    assert_eq!(map.lines.len(), 2);
    assert!(map.lines[1].baseline < map.lines[0].baseline);
}

#[test]
fn un_paragraphe_qui_a_change_nest_pas_ecrase() {
    let path = corpus("synthese/comparaison-avant.pdf");
    let doc = Document::load(&path).unwrap();
    let Some((_, _, opened)) = a_paragraph(&doc) else {
        panic!("aucun paragraphe éditable dans le fichier d'essai");
    };
    let pages = collect_pages(&doc).unwrap();
    // On prétend que la page porte autre chose que ce qui y est : la
    // recomposition doit refuser plutôt que d'écraser un texte inconnu.
    let refused = set_paragraph_text(&doc, &pages[0], &opened.frame, "textequinyestpas", "x");
    assert!(refused.is_err());
    assert!(
        page_text(&doc, 0).contains(&opened.drawn),
        "rien n'a été touché"
    );
}

/// Un fichier enregistré, sans son identifiant `/ID` : celui-ci est
/// recalculé à chaque enregistrement, il ne dit rien du contenu.
fn without_id(bytes: &[u8]) -> Vec<u8> {
    let Some(start) = bytes.windows(4).position(|w| w == b"/ID ") else {
        return bytes.to_vec();
    };
    let Some(len) = bytes[start..].iter().position(|b| *b == b']') else {
        return bytes.to_vec();
    };
    let mut out = bytes[..start].to_vec();
    out.extend_from_slice(&bytes[start + len + 1..]);
    out
}

#[test]
fn une_zone_abandonnee_ne_laisse_aucune_trace() {
    let path = corpus("synthese/comparaison-avant.pdf");
    let doc = Document::load(&path).unwrap();
    let before = without_id(&doc.save_full().unwrap());
    let pages = collect_pages(&doc).unwrap();
    let crop = pages[0].crop_box(&doc);
    let frame = new_text_frame(
        &doc,
        &pages[0],
        crop.x0 + 60.0,
        crop.y0 + 40.0,
        12.0,
        [0.0; 3],
    );
    // Des blancs seuls : un curseur, mais rien d'écrit.
    let map = set_paragraph_text(&doc, &pages[0], &frame, "", "  ").unwrap();
    assert_eq!(map.len(), 2);
    assert!(
        map.stops[2].x > map.stops[0].x,
        "le curseur avance sur les blancs"
    );
    let after = without_id(&doc.save_full().unwrap());
    assert!(
        after == before,
        "le fichier a changé alors que rien n'était tapé"
    );
}

#[test]
fn un_paragraphe_a_cheval_sur_deux_colonnes_donne_deux_blocs() {
    // La page Chrome à deux colonnes : un paragraphe se poursuit en haut de
    // la colonne de droite, et l'en-tête porte deux textes sur une ligne.
    let path = corpus("reels/chrome-skia-deux-colonnes-entete-pied-cesure.pdf");
    let doc = Document::load(&path).unwrap();
    let pages = collect_pages(&doc).unwrap();
    let text = extract_page_text(&doc, &pages[0]).unwrap();
    let crop = pages[0].crop_box(&doc);
    let middle = f64::midpoint(crop.x0, crop.x1);
    let units = acrux_features::edit_text::text_units(&text);
    // Aucun bloc de plusieurs lignes n'enjambe le milieu de la page : chacun
    // tient dans sa colonne.
    for u in &units {
        if u.pieces.len() > 1 {
            assert!(
                u.bbox.x1 < middle + 5.0 || u.bbox.x0 > middle - 5.0,
                "bloc à cheval sur les colonnes : {:?}",
                u.bbox
            );
        }
    }
    // Et l'en-tête fait deux blocs sur la même ligne de base.
    let top = units.iter().map(|u| u.bbox.y1).fold(f64::MIN, f64::max);
    let header: Vec<_> = units
        .iter()
        .filter(|u| (u.bbox.y1 - top).abs() < 3.0)
        .collect();
    assert!(
        header.len() >= 2,
        "l'en-tête devrait faire deux blocs : {}",
        header.len()
    );
}

#[test]
fn modifier_la_moitie_droite_dun_entete_ne_touche_pas_la_gauche() {
    let path = corpus("reels/chrome-skia-deux-colonnes-entete-pied-cesure.pdf");
    let doc = Document::load(&path).unwrap();
    let pages = collect_pages(&doc).unwrap();
    let text = extract_page_text(&doc, &pages[0]).unwrap();
    let units = acrux_features::edit_text::text_units(&text);
    let top = units.iter().map(|u| u.bbox.y1).fold(f64::MIN, f64::max);
    // Le bloc le plus à droite de la ligne du haut.
    let (index, right) = units
        .iter()
        .enumerate()
        .filter(|(_, u)| (u.bbox.y1 - top).abs() < 3.0)
        .max_by(|a, b| a.1.bbox.x0.total_cmp(&b.1.bbox.x0))
        .unwrap();
    let left_before: Vec<_> = units
        .iter()
        .filter(|u| (u.bbox.y1 - top).abs() < 3.0 && u.bbox.x1 < right.bbox.x0)
        .map(|u| u.bbox)
        .collect();
    let Ok(opened) = open_paragraph(&doc, &pages[0], &text, index) else {
        panic!("la partie droite de l'en-tête devrait pouvoir s'ouvrir");
    };
    set_paragraph_text(&doc, &pages[0], &opened.frame, &opened.drawn, "Acrux").unwrap();
    let pages = collect_pages(&doc).unwrap();
    let after = extract_page_text(&doc, &pages[0]).unwrap();
    let units_after = acrux_features::edit_text::text_units(&after);
    // La partie gauche est restée où elle était.
    for b in left_before {
        assert!(
            units_after
                .iter()
                .any(|u| (u.bbox.x0 - b.x0).abs() < 0.5 && (u.bbox.y1 - b.y1).abs() < 0.5),
            "la partie gauche de l'en-tête a bougé"
        );
    }
    // La partie droite finit toujours au même bord droit : alignée à
    // droite, elle a grandi (ou rétréci) vers la gauche.
    let word = after
        .lines
        .iter()
        .flat_map(|l| l.words.iter())
        .find(|w| w.text == "Acrux")
        .unwrap_or_else(|| panic!("le nouveau texte est absent"));
    assert!(
        (word.bbox.x1 - right.bbox.x1).abs() < 1.0,
        "bord droit {} au lieu de {}",
        word.bbox.x1,
        right.bbox.x1
    );
}

/// Un formulaire imprimé : des lignes de champ à remplir, comme on en reçoit
/// à compléter.
fn formulaire() -> Document {
    Document::load(corpus("reels/chrome-skia-formulaire-lignes-cases.pdf")).unwrap()
}

#[test]
fn une_zone_neuve_prend_la_police_du_document_et_se_pose_sur_la_ligne_de_champ() {
    let doc = formulaire();
    let pages = collect_pages(&doc).unwrap();
    let text = extract_page_text(&doc, &pages[0]).unwrap();
    // La ligne « Intitulé du compte : ______ » et son trait.
    let label = text
        .lines
        .iter()
        .find(|l| l.text().contains("Intitulé du compte"))
        .unwrap_or_else(|| panic!("le libellé est absent du fichier d'essai"));
    let (baseline, size) = (label.baseline(), label.words[0].glyphs[0].size);
    let police = label.words[0].glyphs[0].font.clone();
    // On clique sur le trait, à droite du libellé : un peu sous la ligne de
    // base du libellé, comme le ferait quelqu'un qui vise le trait.
    let x = label.bbox.x1 + 60.0;
    let frame = text_frame_at(
        &doc,
        &pages[0],
        &text,
        x,
        baseline - 1.0,
        NewTextStyle::default(),
    );
    assert!(
        (frame.size - size).abs() < 0.6,
        "corps hérité : {} au lieu de {size}",
        frame.size
    );
    assert!(
        frame.standard.is_none(),
        "la zone doit écrire avec une police du document, pas une police standard"
    );
    // Posée sur le trait, donc à peu près sur la ligne de base du libellé.
    assert!(
        (frame.baseline - baseline).abs() < size * 0.6,
        "ligne de base {} loin de celle du libellé {baseline}",
        frame.baseline
    );
    // Et l'on peut taper, frappe après frappe, bien que le libellé soit sur
    // la même ligne : c'est la zone seule qui est recomposée.
    let mut drawn = String::new();
    let mut typed = String::new();
    for c in "Jean Dupont".chars() {
        typed.push(c);
        let pages = collect_pages(&doc).unwrap();
        set_paragraph_text(&doc, &pages[0], &frame, &drawn, &typed)
            .unwrap_or_else(|e| panic!("frappe « {c} » refusée : {e}"));
        drawn = normalized(&typed);
    }
    let pages = collect_pages(&doc).unwrap();
    let after = extract_page_text(&doc, &pages[0]).unwrap();
    let line = after
        .lines
        .iter()
        .find(|l| l.text().contains("Jean Dupont"))
        .unwrap_or_else(|| panic!("la saisie est absente de la page"));
    // Le libellé est intact, et la saisie s'écrit dans sa police.
    assert!(line.text().contains("Intitulé du compte"));
    assert_eq!(
        line.words
            .iter()
            .flat_map(|w| w.glyphs.iter())
            .find(|g| g.text == "J")
            .map(|g| g.font.clone()),
        Some(police)
    );
}

#[test]
fn le_corps_et_la_couleur_choisis_dans_la_barre_lemportent() {
    let doc = formulaire();
    let pages = collect_pages(&doc).unwrap();
    let text = extract_page_text(&doc, &pages[0]).unwrap();
    let crop = pages[0].crop_box(&doc);
    let style = NewTextStyle {
        size: Some(24.0),
        color: Some([0.9, 0.1, 0.1]),
    };
    let frame = text_frame_at(
        &doc,
        &pages[0],
        &text,
        crop.x0 + 60.0,
        crop.y0 + 60.0,
        style,
    );
    assert!((frame.size - 24.0).abs() < 0.01);
    assert!((frame.color[0] - 0.9).abs() < 0.01);
}

#[test]
fn un_paragraphe_de_deux_lignes_reste_justifie() {
    let doc = formulaire();
    let pages = collect_pages(&doc).unwrap();
    let text = extract_page_text(&doc, &pages[0]).unwrap();
    let paragraphs: Vec<_> = text
        .blocks
        .iter()
        .flat_map(|b| b.paragraphs.iter())
        .collect();
    let two = paragraphs
        .iter()
        .find(|p| p.text.contains("Une demande de remboursement"))
        .unwrap_or_else(|| panic!("le paragraphe de deux lignes est absent"));
    assert_eq!(
        two.alignment,
        acrux_features::text::Alignment::Justify,
        "un paragraphe justifié de deux lignes doit être reconnu comme tel, \
         sans quoi la première frappe le mettrait en drapeau"
    );
}
