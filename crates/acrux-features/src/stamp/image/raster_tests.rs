//! Épreuves des décodeurs BMP, GIF et TIFF.
//!
//! Les fichiers de `tests/corpus/images/` sont écrits par **GDI+**, l'encodeur
//! de Windows : ce sont donc des fichiers d'un autre programme que le nôtre,
//! et non des octets fabriqués par le code qu'ils éprouvent. Le même motif y
//! est enregistré dans chaque format ; la référence est sa version PNG, que
//! nous savons déjà lire, et la comparaison est **exacte au pixel près** —
//! les huit couleurs du motif traversent sans perte la palettisation d'un GIF
//! comme celle d'un BMP.

#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use std::path::PathBuf;

use super::{rasters, Raster};

/// Un fichier du corpus d'images.
fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join("images")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{} : {e}", path.display()))
}

/// Pixels en RVB, quel que soit le nombre de composantes : une image sans
/// couleur est rendue en niveaux de gris, ce qui est le bon choix pour le PDF
/// mais empêche la comparaison directe.
fn rgb(r: &Raster) -> Vec<u8> {
    if r.components == 3 {
        return r.data.clone();
    }
    r.data.iter().flat_map(|v| [*v, *v, *v]).collect()
}

/// Décode un fichier et rend sa première page.
fn one(name: &str) -> Raster {
    let data = fixture(name);
    rasters(&data)
        .unwrap_or_else(|e| panic!("{name} : {e}"))
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("{name} : aucune page"))
}

/// Compare une image à la référence PNG du même motif.
fn same_as(name: &str, reference: &str) {
    let got = one(name);
    let want = one(reference);
    assert_eq!(
        (got.width, got.height),
        (want.width, want.height),
        "{name} : dimensions"
    );
    let (a, b) = (rgb(&got), rgb(&want));
    let wrong = a
        .chunks_exact(3)
        .zip(b.chunks_exact(3))
        .filter(|(p, q)| p != q)
        .count();
    assert_eq!(wrong, 0, "{name} : {wrong} pixels diffèrent du PNG");
}

#[test]
fn un_bmp_24_bits_se_lit_comme_son_png() {
    same_as("motif-24.bmp", "motif.png");
    same_as("damier-24.bmp", "damier.png");
}

#[test]
fn un_gif_se_lit_comme_son_png() {
    same_as("motif.gif", "motif.png");
}

#[test]
fn les_trois_compressions_tiff_donnent_la_meme_image() {
    same_as("motif-none.tif", "motif.png");
    same_as("motif-lzw.tif", "motif.png");
    same_as("motif-packbits.tif", "motif.png");
}

/// Le Groupe 4 est la compression des scanners : c'est le cas qui compte.
#[test]
fn un_tiff_groupe_4_se_lit_comme_son_png() {
    same_as("damier-g4.tif", "damier.png");
}

/// Un TIFF de scanner porte une page par feuille : elles doivent toutes
/// ressortir, dans l'ordre.
#[test]
fn un_tiff_multipage_rend_toutes_ses_pages() {
    let data = fixture("deux-pages.tif");
    let pages = rasters(&data).expect("deux-pages.tif");
    assert_eq!(pages.len(), 2, "deux pages attendues");
    let motif = one("motif.png");
    let damier = one("damier.png");
    assert_eq!(rgb(&pages[0]), rgb(&motif), "première page");
    assert_eq!(rgb(&pages[1]), rgb(&damier), "seconde page");
}

/// Une image en niveaux de gris s'écrit sur une composante : trois fois moins
/// d'octets pour exactement la même image.
#[test]
fn le_noir_et_blanc_sort_en_niveaux_de_gris() {
    assert_eq!(one("damier-g4.tif").components, 1);
    assert_eq!(one("damier-24.bmp").components, 1);
}

#[test]
fn un_format_inconnu_le_dit() {
    let e = rasters(b"RIFF\0\0\0\0WEBPVP8 ")
        .map(|_| ())
        .expect_err("un WebP n'est pas lu");
    assert!(e.to_string().contains("PNG"), "{e}");
}
