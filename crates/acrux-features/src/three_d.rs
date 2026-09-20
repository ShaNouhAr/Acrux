//! Les modèles 3D d'un PDF (ISO 32000-2 §13.6).
//!
//! Un PDF peut porter un objet en trois dimensions : une annotation `/3D`
//! occupe un rectangle de la page, et son flux `/3DD` contient le modèle —
//! au format **U3D** (ECMA-363) ou **PRC** (ISO 14739). Tant que le lecteur
//! ne l'active pas, la page montre une image de remplacement, l'affiche ;
//! une fois activé, l'objet se tourne à la souris.
//!
//! Ce module lit le modèle ([`u3d`]), le dessine ([`draw`]) et rend la vue par
//! défaut que le document demande. Le PRC n'est pas lu : son format de
//! géométrie est un tout autre travail, et c'est écrit tel quel plutôt que
//! deviné ([`Kind::Prc`]).

// La géométrie du document est en `f64`, celle du rendu en `f32` : la
// conversion est voulue, et sans conséquence à l'échelle d'un écran.
#![allow(clippy::cast_possible_truncation)]

mod bits;
pub mod draw;
#[cfg(test)]
mod tests;
pub mod u3d;

use acrux_core::{Rect, Result};
use acrux_document::text::decode_text_string;
use acrux_document::{collect_pages, Dict, Document, Name, Object};

pub use draw::{render, Camera};
pub use u3d::Scene;

/// Format du modèle porté par l'annotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// U3D (ECMA-363) : lu et dessiné.
    U3d,
    /// PRC (ISO 14739) : reconnu, mais pas lu.
    Prc,
    /// Un format que le document ne nomme pas.
    Unknown,
}

impl Kind {
    /// Nom affichable.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Kind::U3d => "U3D",
            Kind::Prc => "PRC",
            Kind::Unknown => "3D",
        }
    }
}

/// Un modèle 3D posé sur une page.
#[derive(Debug, Clone)]
pub struct Model {
    /// Page qui le porte.
    pub page: usize,
    /// Rang dans `/Annots`.
    pub index: usize,
    /// Rectangle occupé, en coordonnées de la page.
    pub rect: Rect,
    /// Format du modèle.
    pub kind: Kind,
    /// Nom de la vue par défaut, s'il y en a une.
    pub view: Option<String>,
    /// Couleur de fond demandée par la vue par défaut.
    pub background: Option<[f64; 3]>,
    /// Matrice caméra→monde de la vue par défaut (`/C2W`), et distance
    /// d'orbite (`/CO`).
    pub camera: Option<([f64; 12], f64)>,
    /// Ouverture verticale en degrés, si la vue en impose une.
    pub fov: Option<f64>,
}

/// Liste les modèles 3D du document.
///
/// # Errors
/// Arbre des pages illisible.
pub fn list(doc: &Document) -> Result<Vec<Model>> {
    let mut out = Vec::new();
    for (page, entry) in collect_pages(doc)?.iter().enumerate() {
        let Some(annots) = entry.dict.get(&Name::new("Annots")) else {
            continue;
        };
        let Ok(resolved) = doc.resolve(annots) else {
            continue;
        };
        let Some(list) = resolved.as_array() else {
            continue;
        };
        for (index, item) in list.iter().enumerate() {
            let Ok(annot) = doc.resolve(item) else {
                continue;
            };
            let Some(annot) = annot.as_dict() else {
                continue;
            };
            if name_of(doc, annot, "Subtype").as_deref() != Some("3D") {
                continue;
            }
            out.push(read(doc, annot, page, index));
        }
    }
    Ok(out)
}

/// Lit une annotation `/3D`.
fn read(doc: &Document, annot: &Dict, page: usize, index: usize) -> Model {
    let stream = doc.dict_get(annot, "3DD").ok().flatten();
    let kind = stream
        .as_ref()
        .and_then(|s| s.as_dict())
        .and_then(|d| name_of(doc, d, "Subtype"))
        .map_or(Kind::Unknown, |s| match s.as_str() {
            "U3D" => Kind::U3d,
            "PRC" => Kind::Prc,
            _ => Kind::Unknown,
        });
    let view = default_view(doc, annot);
    let (camera, fov, background, name) = match &view {
        Some(v) => (
            matrix_of(doc, v, "C2W").map(|m| (m, number_of(doc, v, "CO").unwrap_or(0.0))),
            projection_fov(doc, v),
            background_of(doc, v),
            text_of(doc, v, "XN"),
        ),
        None => (None, None, None, None),
    };
    Model {
        page,
        index,
        rect: rect_of(doc, annot).unwrap_or_default(),
        kind,
        view: name,
        background,
        camera,
        fov,
    }
}

/// La vue par défaut : `/3DV` désigne une vue de `/3DD /VA`, par son rang,
/// par son nom, ou directement.
fn default_view(doc: &Document, annot: &Dict) -> Option<Dict> {
    let stream = doc
        .dict_get(annot, "3DD")
        .ok()
        .flatten()
        .and_then(|s| s.as_dict().cloned());
    let views = stream
        .as_ref()
        .and_then(|d| doc.dict_get(d, "VA").ok().flatten())
        .and_then(|a| a.as_array().map(<[Object]>::to_vec));
    let chosen = doc.dict_get(annot, "3DV").ok().flatten();
    let pick = |i: usize| -> Option<Dict> {
        let list = views.as_ref()?;
        let entry = list.get(i)?;
        doc.resolve(entry).ok()?.as_dict().cloned()
    };
    match chosen.as_deref() {
        Some(Object::Integer(i)) => pick(usize::try_from(*i).ok()?),
        Some(Object::Dict(d)) => Some(d.clone()),
        // « F » cadre le modèle, « L » reprend la dernière vue employée :
        // ni l'une ni l'autre n'est écrite dans le fichier, et l'on cadre
        // nous-mêmes.
        Some(Object::Name(_)) => None,
        Some(Object::String(s)) => {
            let wanted = decode_text_string(s);
            let list = views.as_ref()?;
            list.iter().find_map(|entry| {
                let d = doc.resolve(entry).ok()?.as_dict().cloned()?;
                (text_of(doc, &d, "XN").as_deref() == Some(wanted.as_str())).then_some(d)
            })
        }
        _ => pick(0),
    }
}

/// Ouverture verticale de la projection d'une vue, en degrés.
fn projection_fov(doc: &Document, view: &Dict) -> Option<f64> {
    let projection = doc.dict_get(view, "P").ok()??;
    let projection = projection.as_dict()?;
    match name_of(doc, projection, "Subtype").as_deref() {
        // Orthographique : pas d'ouverture, le rendu emploie une projection
        // parallèle.
        Some("O") => Some(0.0),
        _ => number_of(doc, projection, "FOV"),
    }
}

/// Couleur de fond d'une vue (`/BG /C`).
fn background_of(doc: &Document, view: &Dict) -> Option<[f64; 3]> {
    let bg = doc.dict_get(view, "BG").ok()??;
    let bg = bg.as_dict()?;
    let c = doc.dict_get(bg, "C").ok()??;
    let values: Vec<f64> = c
        .as_array()?
        .iter()
        .filter_map(|o| doc.resolve(o).ok().and_then(|v| v.as_f64()))
        .collect();
    match values.len() {
        1 => Some([values[0]; 3]),
        3 => Some([values[0], values[1], values[2]]),
        _ => None,
    }
}

/// Les données du modèle, décomprimées.
///
/// # Errors
/// Flux absent ou illisible.
pub fn data(doc: &Document, model: &Model) -> Result<Vec<u8>> {
    let pages = collect_pages(doc)?;
    let page = pages
        .get(model.page)
        .ok_or_else(|| acrux_core::Error::Corrupt("page absente".into()))?;
    let annots = page
        .dict
        .get(&Name::new("Annots"))
        .ok_or_else(|| acrux_core::Error::Corrupt("aucune annotation".into()))?;
    let resolved = doc.resolve(annots)?;
    let list = resolved
        .as_array()
        .ok_or_else(|| acrux_core::Error::Corrupt("annotations illisibles".into()))?;
    let entry = list
        .get(model.index)
        .ok_or_else(|| acrux_core::Error::Corrupt("annotation absente".into()))?;
    let annot = doc.resolve(entry)?;
    let annot = annot
        .as_dict()
        .ok_or_else(|| acrux_core::Error::Corrupt("annotation illisible".into()))?;
    let stream = doc
        .dict_get(annot, "3DD")?
        .ok_or_else(|| acrux_core::Error::Corrupt("modèle absent".into()))?;
    Ok(doc.stream_data(&stream)?.data)
}

/// Lit le modèle et rend la scène à dessiner.
///
/// # Errors
/// Flux illisible, ou format qu'on ne sait pas lire.
pub fn scene(doc: &Document, model: &Model) -> Result<Scene> {
    if model.kind == Kind::Prc {
        return Err(acrux_core::Error::Unsupported(
            "modèle PRC : seul l'U3D est lu pour l'instant".into(),
        ));
    }
    let data = data(doc, model)?;
    let scene = u3d::parse(&data);
    if scene.items.is_empty() {
        return Err(acrux_core::Error::Corrupt(
            "modèle 3D sans géométrie lisible".into(),
        ));
    }
    Ok(scene)
}

/// Caméra de départ : celle que demande le document, ou un cadrage de la
/// scène.
///
/// La matrice `/C2W` d'une vue PDF donne la position et l'orientation de la
/// caméra dans le monde ; on en retire l'angle de vue et la distance, parce
/// que la suite se pilote à la souris en tournant autour du modèle.
#[must_use]
pub fn camera(model: &Model, scene: &Scene) -> Camera {
    let mut camera = Camera::framing(scene);
    if let Some(background) = model.background {
        let unit = |v: f64| v.clamp(0.0, 1.0) as f32;
        camera.background = acrux_graphics::Color::rgb(
            unit(background[0]),
            unit(background[1]),
            unit(background[2]),
        );
    }
    if let Some(fov) = model.fov {
        if fov > 1.0 && fov < 170.0 {
            camera.fov = fov as f32;
        }
    }
    if let Some((m, orbit)) = model.camera {
        // Les trois premières colonnes sont les axes de la caméra, la
        // quatrième sa position. L'axe « avant » d'une caméra PDF est -z.
        let eye = [m[9] as f32, m[10] as f32, m[11] as f32];
        let forward = [-m[6] as f32, -m[7] as f32, -m[8] as f32];
        let distance = if orbit.abs() > 1e-6 {
            orbit as f32
        } else {
            camera.distance
        };
        camera.target = [
            eye[0] + forward[0] * distance,
            eye[1] + forward[1] * distance,
            eye[2] + forward[2] * distance,
        ];
        camera.distance = distance;
        // Angles d'orbite correspondant à cette direction.
        camera.pitch = (-forward[1]).clamp(-1.0, 1.0).asin();
        camera.yaw = (-forward[0]).atan2(-forward[2]);
    }
    camera
}

/// Une matrice de `count` nombres, lue dans une entrée de dictionnaire.
fn matrix_of(doc: &Document, dict: &Dict, key: &str) -> Option<[f64; 12]> {
    let values = doc.dict_get(dict, key).ok()??;
    let array = values.as_array()?;
    let numbers: Vec<f64> = array
        .iter()
        .filter_map(|o| doc.resolve(o).ok().and_then(|v| v.as_f64()))
        .collect();
    let mut out = [0.0; 12];
    if numbers.len() < 12 {
        return None;
    }
    out.copy_from_slice(&numbers[..12]);
    Some(out)
}

fn number_of(doc: &Document, dict: &Dict, key: &str) -> Option<f64> {
    doc.dict_get(dict, key).ok()??.as_f64()
}

fn name_of(doc: &Document, dict: &Dict, key: &str) -> Option<String> {
    match &*doc.dict_get(dict, key).ok()?? {
        Object::Name(n) => Some(n.as_str()),
        _ => None,
    }
}

fn text_of(doc: &Document, dict: &Dict, key: &str) -> Option<String> {
    match &*doc.dict_get(dict, key).ok()?? {
        Object::String(s) => Some(decode_text_string(s)),
        Object::Name(n) => Some(n.as_str()),
        _ => None,
    }
}

fn rect_of(doc: &Document, dict: &Dict) -> Option<Rect> {
    let values = doc.dict_get(dict, "Rect").ok()??;
    let array = values.as_array()?;
    let n: Vec<f64> = array
        .iter()
        .filter_map(|o| doc.resolve(o).ok().and_then(|v| v.as_f64()))
        .collect();
    (n.len() == 4).then(|| Rect::new(n[0], n[1], n[2], n[3]))
}
