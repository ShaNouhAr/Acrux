//! Ce que donne un PDF sur une machine **sans une seule police installée**.
//!
//! C'est le cas qu'on ne peut pas vérifier par hasard : un poste de travail a
//! toujours des polices, un conteneur n'en a aucune. `ACRUX_FONT_DIR` remplace
//! les répertoires du système, si bien qu'un répertoire vide reproduit
//! fidèlement la machine nue.
//!
//! Ce fichier est un binaire de test à lui seul — c'est ce qui permet d'y
//! poser une variable d'environnement sans déranger les autres tests, qui
//! tournent dans d'autres processus.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Once;

use acrux_document::{Document, Parser};
use acrux_render::font::LoadedFont;

/// Coupe l'accès aux polices du système, une fois pour tout le binaire.
fn sans_polices() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let vide = std::env::temp_dir().join("acrux-aucune-police");
        std::fs::create_dir_all(&vide).ok();
        std::env::set_var("ACRUX_FONT_DIR", &vide);
    });
}

fn font(src: &str) -> LoadedFont {
    sans_polices();
    let doc =
        Document::from_bytes(b"%PDF-1.7\n1 0 obj << /Type /Catalog >> endobj\n".to_vec()).unwrap();
    let obj = Parser::new(src.as_bytes()).parse_object().unwrap();
    LoadedFont::load(&doc, obj.as_dict().unwrap()).unwrap()
}

/// Le texte s'affiche quand même : c'est tout l'enjeu.
#[test]
fn une_page_sans_police_installee_affiche_son_texte() {
    let f = font("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    assert!(f.substituted);
    let mut dessines = 0;
    for glyph in f.decode(b"Bonjour, le monde ! 42 %") {
        if let Some(path) = f.glyph_path(&glyph) {
            if path.bounds().is_some() {
                dessines += 1;
            }
        }
    }
    // 24 caractères, dont cinq espaces qui ne dessinent rien.
    assert_eq!(dessines, 19, "{dessines} glyphes dessinés sur 19 attendus");
}

/// Et il s'affiche **à la bonne place** : les largeurs viennent des tables
/// standard, pas d'une police mesurée au hasard.
#[test]
fn les_largeurs_restent_celles_du_document() {
    let f = font("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    let g = f.decode(b"AWi");
    assert!((g[0].width - 0.667).abs() < 1e-9);
    assert!((g[1].width - 0.944).abs() < 1e-9);
    assert!((g[2].width - 0.222).abs() < 1e-9);
    // Une police quelconque, avec ses largeurs déclarées.
    let d = font(
        "<< /Type /Font /Subtype /TrueType /BaseFont /Calibri /FirstChar 65 /Widths [800] \
         /Encoding /WinAnsiEncoding /FontDescriptor << /Flags 32 >> >>",
    );
    assert!((d.decode(b"A")[0].width - 0.8).abs() < 1e-9);
}

/// Le contour de secours est mis à la chasse déclarée, comme n'importe quelle
/// substitution : sans cela, la ligne ne tomberait pas juste.
#[test]
fn le_contour_de_secours_suit_la_chasse_declaree() {
    let etroit = font(
        "<< /Type /Font /Subtype /TrueType /BaseFont /Inconnue /FirstChar 72 /Widths [400] \
         /Encoding /WinAnsiEncoding /FontDescriptor << /Flags 32 >> >>",
    );
    let large = font(
        "<< /Type /Font /Subtype /TrueType /BaseFont /Inconnue /FirstChar 72 /Widths [800] \
         /Encoding /WinAnsiEncoding /FontDescriptor << /Flags 32 >> >>",
    );
    let a = etroit.glyph_path(&etroit.decode(b"H")[0]).unwrap();
    let b = large.glyph_path(&large.decode(b"H")[0]).unwrap();
    let (ba, bb) = (a.bounds().unwrap(), b.bounds().unwrap());
    let rapport = (bb.x1 - bb.x0) / (ba.x1 - ba.x0);
    assert!((rapport - 2.0).abs() < 0.02, "rapport {rapport}, attendu 2");
    // La hauteur, elle, ne bouge pas : on étire, on ne grossit pas.
    assert!((bb.y1 - ba.y1).abs() < 1e-6);
}

/// Les accents sont là : sans eux, un texte français serait criblé de trous.
#[test]
fn le_francais_sort_avec_ses_accents() {
    let f =
        font("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    // « Été à Noël, où ça ? » en WinAnsi.
    let bytes = [
        0xC9, 0x74, 0xE9, 0x20, 0xE0, 0x20, 0x4E, 0x6F, 0xEB, 0x6C, 0x2C, 0x20, 0x6F, 0xF9, 0x20,
        0xE7, 0x61, 0x20, 0x3F,
    ];
    for glyph in f.decode(&bytes) {
        if glyph.is_space {
            continue;
        }
        let nom = f.glyph_name(glyph.code).unwrap_or("?");
        assert!(
            f.glyph_path(&glyph).and_then(|p| p.bounds()).is_some(),
            "{nom} ne dessine rien"
        );
    }
}

/// Ce que la police de secours ne couvre pas, elle ne le prétend pas : un
/// glyphe grec reste vide plutôt que de sortir une lettre latine à sa place.
#[test]
fn ce_qui_nest_pas_couvert_reste_vide() {
    let f = font("<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>");
    let g = f.decode(b"a");
    assert_eq!(f.glyph_name(g[0].code), Some("alpha"));
    assert!(f.glyph_path(&g[0]).is_none());
    // La largeur, elle, reste juste : la ligne ne se décale pas.
    assert!((g[0].width - 0.631).abs() < 1e-9);
}
