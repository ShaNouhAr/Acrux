//! Épreuves du lecteur U3D.
//!
//! Le modèle d'épreuve est **écrit ici**, avec l'encodeur de la norme
//! (`bits::Writer`), puis relu par le décodeur : un cube de huit sommets et
//! douze faces doit ressortir tel quel, avec ses matériaux. Écrire et relire
//! par deux chemins inverses est la seule façon d'éprouver un codeur
//! arithmétique sans fichier de référence.

#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use super::super::bits::{Writer, STATIC_FULL};
use super::parse;

/// Emballe des données dans un bloc U3D : type, taille, métadonnées, et le
/// remplissage qui cale la suite sur quatre octets.
fn block(kind: u32, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + data.len() + 3);
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    out
}

/// Les huit sommets d'un cube centré sur l'origine.
fn corners(size: f32) -> Vec<[f32; 3]> {
    let s = size / 2.0;
    let mut out = Vec::with_capacity(8);
    for z in [-s, s] {
        for y in [-s, s] {
            for x in [-s, s] {
                out.push([x, y, z]);
            }
        }
    }
    out
}

/// Les douze triangles d'un cube, dans l'ordre des sommets ci-dessus.
const CUBE_FACES: [[u32; 3]; 12] = [
    [0, 2, 3],
    [0, 3, 1],
    [4, 5, 7],
    [4, 7, 6],
    [0, 1, 5],
    [0, 5, 4],
    [2, 6, 7],
    [2, 7, 3],
    [0, 4, 6],
    [0, 6, 2],
    [1, 3, 7],
    [1, 7, 5],
];

/// Fabrique un fichier U3D : un cube nommé, posé par un nœud modèle, avec un
/// matériau rouge.
#[allow(clippy::too_many_lines)] // un bloc U3D après l'autre, dans l'ordre
pub(crate) fn cube_file(size: f32) -> Vec<u8> {
    let name = "cube";
    let mut out = Vec::new();

    // En-tête de fichier.
    let mut w = Writer::new();
    w.u16(0); // version majeure
    w.u16(0); // version mineure
    w.u32(0x0000_0000); // profil
    w.u32(0); // taille de la section de déclaration
    w.u32(0); // taille du fichier (poids faible)
    w.u32(0); // … et poids fort
    w.u32(106); // encodage : UTF-8
    w.f32(0.0); // facteur d'échelle (poids faible du F64)
    w.f32(1.0);
    out.extend_from_slice(&block(0x0044_3355, &w.finish()));

    // Chaîne du nœud : le nœud modèle qui place le cube.
    let mut node = Writer::new();
    node.string(name);
    node.u32(1); // un parent : la racine
    node.string("");
    for value in &identity() {
        node.f32(*value);
    }
    node.string(name); // ressource
    node.u32(3); // visible des deux côtés
    out.extend_from_slice(&chain(name, 0, &block(0xFFFF_FF22, &node.finish())));

    // Chaîne de la ressource : la déclaration du maillage.
    let mut declaration = Writer::new();
    declaration.string(name);
    declaration.u32(0); // indice dans la chaîne
    declaration.u32(0); // attributs : avec normales
    declaration.u32(12); // faces
    declaration.u32(8); // positions
    declaration.u32(6); // normales
    declaration.u32(0); // couleurs diffuses
    declaration.u32(0); // couleurs spéculaires
    declaration.u32(0); // coordonnées de texture
    declaration.u32(1); // un groupe d'ombrage
    declaration.u32(0); // attributs du groupe : ni diffus ni spéculaire
    declaration.u32(0); // aucune couche de texture
    declaration.u32(0); // identifiant d'origine
                        // Description CLOD : résolutions minimale et maximale, toutes deux au
                        // maillage de base.
    declaration.u32(0);
    declaration.u32(8);
    out.extend_from_slice(&chain(name, 1, &block(0xFFFF_FF31, &declaration.finish())));

    // Le maillage de base lui-même.
    let mut mesh = Writer::new();
    mesh.string(name);
    mesh.u32(0);
    mesh.u32(12); // faces
    mesh.u32(8); // positions
    mesh.u32(6); // normales
    mesh.u32(0);
    mesh.u32(0);
    mesh.u32(0);
    for p in corners(size) {
        mesh.f32(p[0]);
        mesh.f32(p[1]);
        mesh.f32(p[2]);
    }
    for n in [
        [0.0, 0.0, -1.0],
        [0.0, 0.0, 1.0],
        [0.0, -1.0, 0.0],
        [0.0, 1.0, 0.0],
        [-1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
    ] {
        mesh.f32(n[0]);
        mesh.f32(n[1]);
        mesh.f32(n[2]);
    }
    for (i, face) in CUBE_FACES.iter().enumerate() {
        mesh.compressed_u32(1, 0); // groupe d'ombrage
        for corner in face {
            mesh.compressed_u32(STATIC_FULL + 8, *corner);
            mesh.compressed_u32(STATIC_FULL + 6, u32::try_from(i / 2).unwrap());
        }
    }
    out.extend_from_slice(&block(0xFFFF_FF3B, &mesh.finish()));

    // Le nuanceur et son matériau, rouges.
    let mut shader = Writer::new();
    shader.string("rouge");
    shader.u32(0);
    shader.f32(0.0);
    shader.u32(0);
    shader.u32(0);
    shader.u32(1);
    shader.u32(0);
    shader.u32(0);
    shader.string("rouge");
    out.extend_from_slice(&block(0xFFFF_FF53, &shader.finish()));

    let mut material = Writer::new();
    material.string("rouge");
    material.u32(0x3F); // tous les champs
    for value in [
        0.2, 0.1, 0.1, // ambiante
        0.8, 0.15, 0.1, // diffuse
        0.4, 0.4, 0.4, // spéculaire
        0.0, 0.0, 0.0, // émissive
        0.0, // réflectivité
        1.0, // opacité
    ] {
        material.f32(value);
    }
    out.extend_from_slice(&block(0xFFFF_FF54, &material.finish()));

    // Le modificateur d'ombrage, qui relie le nœud au nuanceur.
    let mut shading = Writer::new();
    shading.string(name);
    shading.u32(0);
    shading.u32(1);
    shading.u32(1); // une liste
    shading.u32(1); // un nuanceur dedans
    shading.string("rouge");
    out.extend_from_slice(&chain(name, 0, &block(0xFFFF_FF45, &shading.finish())));

    out
}

/// Emballe des blocs de déclaration dans une chaîne de modificateurs.
fn chain(name: &str, kind: u32, inner: &[u8]) -> Vec<u8> {
    let mut w = Writer::new();
    w.string(name);
    w.u32(kind);
    w.u32(0); // aucune information d'encombrement
    let mut data = w.finish();
    while data.len() % 4 != 0 {
        data.push(0);
    }
    data.extend_from_slice(&1u32.to_le_bytes()); // un modificateur
    data.extend_from_slice(inner);
    block(0xFFFF_FF14, &data)
}

/// Matrice identité, en colonnes.
fn identity() -> [f32; 16] {
    let mut m = [0.0; 16];
    m[0] = 1.0;
    m[5] = 1.0;
    m[10] = 1.0;
    m[15] = 1.0;
    m
}

/// Écrit les fichiers d'épreuve du corpus. Lancé à la main, une fois :
/// `ACRUX_ECRIRE_CORPUS=1 cargo test -p acrux-features --lib ecrire_le_corpus`.
#[test]
fn ecrire_le_corpus() {
    if std::env::var("ACRUX_ECRIRE_CORPUS").is_err() {
        return;
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
        .join("synthese");
    let model = cube_file(4.0);
    std::fs::write(root.join("modele-3d-cube.u3d"), &model).unwrap();
    let doc = crate::create::from_3d(&model, &crate::create::PageSetup::default()).unwrap();
    std::fs::write(root.join("modele-3d-u3d.pdf"), doc.save_full().unwrap()).unwrap();
}

/// Ce qu'on écrit doit se relire à l'identique : c'est l'épreuve du codeur
/// arithmétique autant que celle du format.
#[test]
fn un_cube_ecrit_se_relit_tel_quel() {
    let scene = parse(&cube_file(4.0));
    assert_eq!(scene.items.len(), 1, "un seul objet");
    let item = &scene.items[0];
    assert_eq!(item.mesh.name, "cube");
    assert_eq!(item.mesh.positions.len(), 8);
    assert_eq!(item.mesh.normals.len(), 6);
    assert_eq!(item.mesh.faces.len(), 12);
    // Les sommets sont ceux d'un cube de quatre unités de côté.
    for p in &item.mesh.positions {
        for c in p {
            assert!((c.abs() - 2.0).abs() < 1e-6, "sommet {p:?}");
        }
    }
    // Les faces désignent bien les sommets écrits.
    for (face, expected) in item.mesh.faces.iter().zip(CUBE_FACES.iter()) {
        let got = face.corners.map(|c| c.position);
        assert_eq!(&got, expected);
    }
}

/// Le matériau suit le chemin nœud → nuanceur → matériau.
#[test]
fn le_materiau_du_noeud_est_retrouve() {
    let scene = parse(&cube_file(2.0));
    let material = scene.items[0].materials[0];
    assert!((material.diffuse[0] - 0.8).abs() < 1e-6, "{material:?}");
    assert!((material.diffuse[1] - 0.15).abs() < 1e-6);
    assert!((material.opacity - 1.0).abs() < 1e-6);
}

/// Un fichier tronqué ou vide ne fait pas tomber le lecteur.
#[test]
fn un_fichier_abime_ne_casse_rien() {
    assert!(parse(&[]).items.is_empty());
    assert!(parse(b"U3D\0").items.is_empty());
    let full = cube_file(1.0);
    for cut in [16, 64, 128, full.len() / 2, full.len() - 3] {
        let _ = parse(&full[..cut]);
    }
}
