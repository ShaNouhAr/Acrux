//! Épreuves du côté PDF : trouver un modèle, le lire, le dessiner.
//!
//! Le fichier `tests/corpus/synthese/modele-3d-u3d.pdf` est fabriqué par
//! Acrux lui-même ([`crate::create::from_3d`]) à partir du cube U3D écrit par
//! l'encodeur des épreuves : un cube rouge de quatre unités, vu de trois
//! quarts.

#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use acrux_document::Document;

use super::{camera, list, render, scene, Kind};

/// Ouvre le PDF 3D du corpus.
fn corpus() -> Document {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join("synthese")
        .join("modele-3d-u3d.pdf");
    Document::load(&path).unwrap_or_else(|e| panic!("{} : {e}", path.display()))
}

#[test]
fn le_modele_du_corpus_est_trouve_et_decrit() {
    let doc = corpus();
    let models = list(&doc).unwrap();
    assert_eq!(models.len(), 1, "un modèle sur la page");
    let model = &models[0];
    assert_eq!(model.page, 0);
    assert_eq!(model.kind, Kind::U3d);
    assert_eq!(model.view.as_deref(), Some("Vue par defaut"));
    assert!((model.fov.unwrap_or_default() - 30.0).abs() < 1e-6);
    let background = model.background.expect("fond de la vue");
    assert!((background[0] - 0.95).abs() < 1e-6);
    // Le rectangle occupe la page moins ses marges.
    assert!(model.rect.width() > 100.0 && model.rect.height() > 100.0);
}

#[test]
fn la_geometrie_se_relit_depuis_le_pdf() {
    let doc = corpus();
    let models = list(&doc).unwrap();
    let scene = scene(&doc, &models[0]).unwrap();
    assert_eq!(scene.items.len(), 1);
    let mesh = &scene.items[0].mesh;
    assert_eq!(mesh.positions.len(), 8, "un cube");
    assert_eq!(mesh.faces.len(), 12);
    let material = scene.items[0].materials[0];
    assert!(material.diffuse[0] > material.diffuse[1], "un cube rouge");
}

/// Le rendu doit montrer quelque chose : un objet au milieu, du fond autour.
#[test]
fn le_rendu_montre_le_modele_sur_son_fond() {
    let doc = corpus();
    let models = list(&doc).unwrap();
    let scene = scene(&doc, &models[0]).unwrap();
    let camera = camera(&models[0], &scene);
    let image = render(&scene, &camera, 200, 160);
    let center = image.pixel(100, 80).expect("pixel central");
    let corner = image.pixel(2, 2).expect("coin");
    assert!(
        center[0] > center[1] + 20 && center[0] > center[2] + 20,
        "le cube rouge occupe le centre : {center:?}"
    );
    assert!(
        corner[0] > 200 && corner[1] > 200 && corner[2] > 200,
        "le fond clair occupe les coins : {corner:?}"
    );
}

/// Tourner la caméra change ce qu'on voit — c'est tout l'intérêt.
#[test]
fn tourner_la_camera_change_l_image() {
    let doc = corpus();
    let models = list(&doc).unwrap();
    let scene = scene(&doc, &models[0]).unwrap();
    let base = camera(&models[0], &scene);
    let first = render(&scene, &base, 120, 100);
    let mut turned = base;
    turned.yaw += 0.8;
    let second = render(&scene, &turned, 120, 100);
    assert_ne!(first.data(), second.data(), "la vue a tourné");
    // Et de très loin, le modèle disparaît presque : la distance agit.
    let mut far = base;
    far.distance *= 40.0;
    let small = render(&scene, &far, 120, 100);
    let covered = |b: &acrux_graphics::Bitmap| {
        (0..b.width() * b.height())
            .filter(|i| {
                b.pixel(i % b.width(), i / b.width())
                    .is_some_and(|p| p[0] > p[2] + 10)
            })
            .count()
    };
    assert!(covered(&small) < covered(&first) / 4, "le modèle s'éloigne");
}

/// Une vue qui impose une caméra est suivie.
#[test]
fn la_vue_du_document_place_la_camera() {
    let doc = corpus();
    let models = list(&doc).unwrap();
    let scene = scene(&doc, &models[0]).unwrap();
    let got = camera(&models[0], &scene);
    // Le document porte la vue que notre propre cadrage a calculée à la
    // création : la relire doit rendre la même caméra, à l'arrondi de
    // l'écriture près. C'est l'aller-retour qui compte — un lecteur tiers
    // verra exactement ce que nous voyons.
    let want = super::draw::Camera::framing(&scene);
    assert!((got.distance - want.distance).abs() < 1e-3, "{got:?}");
    assert!((got.yaw - want.yaw).abs() < 1e-3, "{got:?}");
    assert!((got.pitch - want.pitch).abs() < 1e-3, "{got:?}");
    for i in 0..3 {
        assert!((got.target[i] - want.target[i]).abs() < 1e-3, "{got:?}");
    }
    assert!((got.fov - 30.0).abs() < 1e-3);
}
