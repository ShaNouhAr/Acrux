//! Lecture d'un modèle U3D (ECMA-363) : blocs, maillages, matériaux.
//!
//! Un fichier U3D est une suite de **blocs** : un type, une taille, des
//! données comprimées par le codeur arithmétique de [`super::bits`], et des
//! métadonnées. Les blocs de déclaration sont rangés dans des **chaînes de
//! modificateurs** qui les regroupent par objet.
//!
//! Ce qui est lu ici :
//!
//! | Bloc | Ce qu'on en tire |
//! | --- | --- |
//! | `0xFFFFFF14` chaîne de modificateurs | ses blocs imbriqués |
//! | `0xFFFFFF22` nœud modèle | le nom de la ressource et sa matrice |
//! | `0xFFFFFF21` nœud groupe | la matrice d'un parent |
//! | `0xFFFFFF31` déclaration de maillage | les attributs qu'il faut pour lire le maillage |
//! | `0xFFFFFF3B` maillage de base | les positions, les normales et les faces |
//! | `0xFFFFFF45` modificateur d'ombrage | quel nuanceur pour quel groupe de faces |
//! | `0xFFFFFF53` nuanceur | le nom du matériau |
//! | `0xFFFFFF54` matériau | les couleurs |
//!
//! # Ce qui n'est pas lu
//!
//! Le **raffinement progressif** (`0xFFFFFF3C`) : un maillage U3D est écrit à
//! sa résolution minimale, puis affiné par une suite d'opérations de division
//! d'arêtes. On affiche donc le maillage de base, qui est le modèle complet
//! dans la grande majorité des fichiers — un exportateur n'écrit du progressif
//! que si on le lui demande. Les textures, l'animation et les squelettes ne
//! sont pas lus non plus.

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use super::bits::{Reader, STATIC_FULL};

/// Contexte adaptatif de l'indice d'ombrage d'une face.
///
/// C'est le seul contexte adaptatif employé dans un bloc de maillage de base :
/// son numéro n'a donc pas besoin de coïncider avec celui de l'encodeur, il
/// suffit qu'il soit le même d'un bout à l'autre du bloc.
const CONTEXT_SHADING: u32 = 1;

/// Contexte statique couvrant `count` valeurs.
fn range(count: u32) -> u32 {
    STATIC_FULL + count
}

/// Un bloc du fichier.
struct Block<'a> {
    kind: u32,
    data: &'a [u8],
}

/// Découpe une suite d'octets en blocs.
fn blocks(data: &[u8]) -> Vec<Block<'_>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 12 <= data.len() {
        let read = |i: usize| {
            let b = &data[i..i + 4];
            u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize
        };
        let kind = u32::try_from(read(at)).unwrap_or(0);
        let size = read(at + 4);
        let meta = read(at + 8);
        let start = at + 12;
        let Some(body) = data.get(start..start + size) else {
            break;
        };
        out.push(Block { kind, data: body });
        // Les sections sont calées sur quatre octets.
        at = start + size.next_multiple_of(4) + meta.next_multiple_of(4);
    }
    out
}

/// Un coin de face : les indices de ses attributs.
#[derive(Debug, Clone, Copy, Default)]
pub struct Corner {
    /// Indice de la position.
    pub position: u32,
    /// Indice de la normale.
    pub normal: u32,
}

/// Une face triangulaire.
#[derive(Debug, Clone, Copy)]
pub struct Face {
    /// Groupe d'ombrage, qui désigne le matériau.
    pub shading: u32,
    /// Les trois coins.
    pub corners: [Corner; 3],
}

/// Un maillage tel qu'il est écrit dans le fichier.
#[derive(Debug, Clone, Default)]
pub struct Mesh {
    /// Nom de la ressource.
    pub name: String,
    /// Sommets.
    pub positions: Vec<[f32; 3]>,
    /// Normales aux sommets ; vide si le maillage n'en porte pas.
    pub normals: Vec<[f32; 3]>,
    /// Couleurs par sommet, si le maillage en porte.
    pub diffuse: Vec<[f32; 4]>,
    /// Faces.
    pub faces: Vec<Face>,
}

/// Les couleurs d'un matériau.
#[derive(Debug, Clone, Copy)]
pub struct Material {
    /// Couleur diffuse, celle qu'on voit.
    pub diffuse: [f32; 3],
    /// Couleur spéculaire.
    pub specular: [f32; 3],
    /// Opacité.
    pub opacity: f32,
}

impl Default for Material {
    /// Le gris clair d'un modèle sans matériau, comme les visionneuses en
    /// montrent.
    fn default() -> Self {
        Material {
            diffuse: [0.75, 0.75, 0.78],
            specular: [0.2, 0.2, 0.2],
            opacity: 1.0,
        }
    }
}

/// Ce qu'il faut savoir d'un groupe d'ombrage pour lire les coins des faces.
#[derive(Debug, Clone, Copy, Default)]
struct Shading {
    /// Le groupe porte des couleurs diffuses par sommet.
    diffuse: bool,
    /// … et des couleurs spéculaires.
    specular: bool,
    /// Nombre de couches de texture (autant d'indices par coin).
    layers: u32,
}

/// La déclaration d'un maillage : ce qu'il faut savoir avant de lire ses
/// données.
#[derive(Debug, Clone, Default)]
struct Declaration {
    /// Le maillage n'a pas de normales.
    exclude_normals: bool,
    shadings: Vec<Shading>,
}

/// Un modèle à dessiner : un maillage, sa place dans le monde, ses matériaux.
#[derive(Debug, Clone)]
pub struct Item {
    /// Le maillage.
    pub mesh: Mesh,
    /// Matrice de placement, en colonnes (comme dans le fichier).
    pub transform: [f32; 16],
    /// Matériau de chaque groupe d'ombrage.
    pub materials: Vec<Material>,
}

/// Une scène lue dans un fichier U3D.
#[derive(Debug, Clone, Default)]
pub struct Scene {
    /// Ce qu'il y a à dessiner.
    pub items: Vec<Item>,
}

/// Tout ce qu'on récolte en parcourant les blocs.
#[derive(Default)]
struct Harvest {
    declarations: HashMap<String, Declaration>,
    meshes: HashMap<String, Mesh>,
    /// Nœuds modèle : (nom du nœud, nom de la ressource, matrice).
    nodes: Vec<(String, String, [f32; 16])>,
    /// Matrice d'un nœud groupe, par nom.
    groups: HashMap<String, [f32; 16]>,
    /// Nuanceurs d'un nœud, groupe d'ombrage par groupe d'ombrage.
    shading: HashMap<String, Vec<Vec<String>>>,
    /// Nom du matériau d'un nuanceur.
    shaders: HashMap<String, String>,
    materials: HashMap<String, Material>,
}

/// Matrice identité.
const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];

/// Lit un fichier U3D et rend ce qu'il y a à dessiner.
///
/// Un fichier illisible ne fait pas échouer : on rend ce qui a pu être lu, ce
/// qui vaut mieux qu'une page vide devant un modèle un peu abîmé.
#[must_use]
pub fn parse(data: &[u8]) -> Scene {
    let mut harvest = Harvest::default();
    for block in blocks(data) {
        visit(&block, &mut harvest);
    }
    assemble(&harvest)
}

/// Lit un bloc, et les blocs qu'il contient.
fn visit(block: &Block<'_>, harvest: &mut Harvest) {
    match block.kind {
        0xFFFF_FF14 => chain(block.data, harvest),
        0xFFFF_FF21 => group_node(block.data, harvest),
        0xFFFF_FF22 => model_node(block.data, harvest),
        0xFFFF_FF31 => declaration(block.data, harvest),
        0xFFFF_FF3B => base_mesh(block.data, harvest),
        0xFFFF_FF45 => shading_modifier(block.data, harvest),
        0xFFFF_FF53 => shader(block.data, harvest),
        0xFFFF_FF54 => material(block.data, harvest),
        _ => {}
    }
}

/// Une chaîne de modificateurs : son en-tête, puis les blocs qu'elle porte.
///
/// Les blocs imbriqués commencent au premier multiple de quatre octets après
/// l'en-tête. Comme le décodeur sait exactement combien de bits il a
/// consommés, on peut calculer cette position — et si elle ne tombe pas sur un
/// type de bloc connu, on la cherche, plutôt que d'abandonner la chaîne.
fn chain(data: &[u8], harvest: &mut Harvest) {
    let mut r = Reader::new(data);
    let _name = r.string();
    let _kind = r.u32();
    let attributes = r.u32();
    if attributes & 0x1 != 0 {
        r.skip_u32(4);
    }
    if attributes & 0x2 != 0 {
        r.skip_u32(6);
    }
    // Après l'en-tête viennent, calés sur quatre octets, le nombre de
    // modificateurs puis les blocs eux-mêmes.
    let after = r.byte_position().next_multiple_of(4) + 4;
    let start = nested_start(data, after);
    let Some(rest) = data.get(start..) else {
        return;
    };
    for block in blocks(rest) {
        visit(&block, harvest);
    }
}

/// Vrai si ces quatre octets sont un type de bloc de déclaration.
fn known(kind: u32) -> bool {
    matches!(
        kind,
        0xFFFF_FF21..=0xFFFF_FF24 | 0xFFFF_FF31 | 0xFFFF_FF36 | 0xFFFF_FF37
    ) || matches!(kind, 0xFFFF_FF41..=0xFFFF_FF47)
        || matches!(kind, 0xFFFF_FF51..=0xFFFF_FF56)
}

/// Cale le début des blocs imbriqués sur un type connu.
fn nested_start(data: &[u8], guess: usize) -> usize {
    let kind_at = |at: usize| {
        data.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if kind_at(guess).is_some_and(known) {
        return guess;
    }
    // Le compte de modificateurs précède les blocs : on essaie juste après.
    let mut at = 0;
    while at + 4 <= data.len() {
        if kind_at(at).is_some_and(known) {
            return at;
        }
        at += 4;
    }
    guess
}

/// Lit les parents d'un nœud et rend la matrice du premier.
fn parents(r: &mut Reader<'_>) -> (String, [f32; 16]) {
    let count = r.u32();
    let mut first = (String::new(), IDENTITY);
    for i in 0..count {
        let name = r.string();
        let mut m = [0.0f32; 16];
        for value in &mut m {
            *value = r.f32();
        }
        if i == 0 {
            first = (name, m);
        }
    }
    (first.0, first.1)
}

/// Nœud groupe : on n'en garde que la matrice, pour placer ses enfants.
fn group_node(data: &[u8], harvest: &mut Harvest) {
    let mut r = Reader::new(data);
    let name = r.string();
    let (parent, m) = parents(&mut r);
    let base = harvest.groups.get(&parent).copied().unwrap_or(IDENTITY);
    harvest.groups.insert(name, multiply(&base, &m));
}

/// Nœud modèle : le nom de la ressource à dessiner et sa place.
fn model_node(data: &[u8], harvest: &mut Harvest) {
    let mut r = Reader::new(data);
    let name = r.string();
    let (parent, m) = parents(&mut r);
    let resource = r.string();
    let base = harvest.groups.get(&parent).copied().unwrap_or(IDENTITY);
    harvest.nodes.push((name, resource, multiply(&base, &m)));
}

/// Déclaration de maillage : les attributs nécessaires à la lecture.
fn declaration(data: &[u8], harvest: &mut Harvest) {
    let mut r = Reader::new(data);
    let name = r.string();
    let _chain_index = r.u32();
    let attributes = r.u32();
    let _faces = r.u32();
    let _positions = r.u32();
    let _normals = r.u32();
    let _diffuse = r.u32();
    let _specular = r.u32();
    let _texcoords = r.u32();
    let shading_count = r.u32();
    let mut shadings = Vec::new();
    for _ in 0..shading_count.min(4096) {
        let flags = r.u32();
        let layers = r.u32();
        for _ in 0..layers.min(64) {
            let _dimensions = r.u32();
        }
        let _original = r.u32();
        shadings.push(Shading {
            diffuse: flags & 0x1 != 0,
            specular: flags & 0x2 != 0,
            layers,
        });
    }
    harvest.declarations.insert(
        name,
        Declaration {
            exclude_normals: attributes & 0x1 != 0,
            shadings,
        },
    );
}

/// Borne de bon sens : un maillage de base qui annonce des millions de
/// sommets est un fichier abîmé, pas un modèle.
const LIMIT: u32 = 4_000_000;

/// Maillage de base : positions, normales, couleurs et faces.
fn base_mesh(data: &[u8], harvest: &mut Harvest) {
    let mut r = Reader::new(data);
    let name = r.string();
    let _chain_index = r.u32();
    let declaration = harvest.declarations.get(&name).cloned().unwrap_or_default();

    let faces = r.u32();
    let positions = r.u32();
    let normals = r.u32();
    let diffuse = r.u32();
    let specular = r.u32();
    let texcoords = r.u32();
    if faces > LIMIT || positions > LIMIT {
        return;
    }

    let mut mesh = Mesh {
        name: name.clone(),
        ..Mesh::default()
    };
    for _ in 0..positions {
        mesh.positions.push([r.f32(), r.f32(), r.f32()]);
    }
    for _ in 0..normals {
        mesh.normals.push([r.f32(), r.f32(), r.f32()]);
    }
    for _ in 0..diffuse {
        mesh.diffuse.push([r.f32(), r.f32(), r.f32(), r.f32()]);
    }
    for _ in 0..specular {
        let _ = [r.f32(), r.f32(), r.f32(), r.f32()];
    }
    for _ in 0..texcoords {
        let _ = [r.f32(), r.f32(), r.f32(), r.f32()];
    }
    for _ in 0..faces {
        let shading = r.compressed_u32(CONTEXT_SHADING);
        let group = declaration
            .shadings
            .get(shading as usize)
            .copied()
            .unwrap_or_default();
        let mut corners = [Corner::default(); 3];
        for corner in &mut corners {
            corner.position = r.compressed_u32(range(positions));
            if !declaration.exclude_normals {
                corner.normal = r.compressed_u32(range(normals));
            }
            if group.diffuse {
                let _ = r.compressed_u32(range(diffuse));
            }
            if group.specular {
                let _ = r.compressed_u32(range(specular));
            }
            for _ in 0..group.layers.min(8) {
                let _ = r.compressed_u32(range(texcoords));
            }
        }
        mesh.faces.push(Face { shading, corners });
    }
    harvest.meshes.insert(name, mesh);
}

/// Modificateur d'ombrage : les nuanceurs de chaque groupe de faces.
fn shading_modifier(data: &[u8], harvest: &mut Harvest) {
    let mut r = Reader::new(data);
    let name = r.string();
    let _chain_index = r.u32();
    let _attributes = r.u32();
    let lists = r.u32();
    let mut all = Vec::new();
    for _ in 0..lists.min(4096) {
        let count = r.u32();
        let mut names = Vec::new();
        for _ in 0..count.min(64) {
            names.push(r.string());
        }
        all.push(names);
    }
    harvest.shading.insert(name, all);
}

/// Nuanceur : on n'en retient que le matériau.
fn shader(data: &[u8], harvest: &mut Harvest) {
    let mut r = Reader::new(data);
    let name = r.string();
    let _attributes = r.u32();
    let _alpha_reference = r.f32();
    let _alpha_function = r.u32();
    let _blend_function = r.u32();
    let _render_pass = r.u32();
    let _channels = r.u32();
    let _alpha_channels = r.u32();
    let material = r.string();
    harvest.shaders.insert(name, material);
}

/// Matériau : ses couleurs.
fn material(data: &[u8], harvest: &mut Harvest) {
    let mut r = Reader::new(data);
    let name = r.string();
    let _attributes = r.u32();
    let ambient = [r.f32(), r.f32(), r.f32()];
    let diffuse = [r.f32(), r.f32(), r.f32()];
    let specular = [r.f32(), r.f32(), r.f32()];
    let _emissive = [r.f32(), r.f32(), r.f32()];
    let _reflectivity = r.f32();
    let opacity = r.f32();
    let _ = ambient;
    harvest.materials.insert(
        name,
        Material {
            diffuse,
            specular,
            opacity: if opacity.is_finite() {
                opacity.clamp(0.0, 1.0)
            } else {
                1.0
            },
        },
    );
}

/// Produit de deux matrices 4×4 rangées en colonnes.
fn multiply(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut out = [0.0f32; 16];
    for column in 0..4 {
        for row in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k * 4 + row] * b[column * 4 + k];
            }
            out[column * 4 + row] = sum;
        }
    }
    out
}

/// Rassemble maillages, placements et matériaux.
fn assemble(harvest: &Harvest) -> Scene {
    let mut items = Vec::new();
    for (node, resource, transform) in &harvest.nodes {
        let Some(mesh) = harvest.meshes.get(resource) else {
            continue;
        };
        // Un matériau par groupe d'ombrage, en suivant nœud → nuanceur →
        // matériau ; ce qui manque prend le gris par défaut.
        let lists = harvest.shading.get(node);
        let groups = mesh.faces.iter().map(|f| f.shading).max().unwrap_or(0) + 1;
        let mut materials = Vec::with_capacity(groups as usize);
        for i in 0..groups as usize {
            let material = lists
                .and_then(|l| l.get(i))
                .and_then(|names| names.first())
                .and_then(|shader| harvest.shaders.get(shader))
                .and_then(|name| harvest.materials.get(name))
                .copied()
                .unwrap_or_default();
            materials.push(material);
        }
        items.push(Item {
            mesh: mesh.clone(),
            transform: *transform,
            materials,
        });
    }
    // Un fichier sans nœud modèle (rare, mais il en existe) : on dessine les
    // maillages tels quels.
    if items.is_empty() {
        for mesh in harvest.meshes.values() {
            items.push(Item {
                mesh: mesh.clone(),
                transform: IDENTITY,
                materials: vec![Material::default()],
            });
        }
    }
    Scene { items }
}
