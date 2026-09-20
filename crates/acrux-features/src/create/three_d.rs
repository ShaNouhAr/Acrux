//! Création d'un PDF portant un modèle 3D (ISO 32000-2 §13.6).
//!
//! Le modèle — un fichier **U3D** — devient un flux `/3D`, et une annotation
//! `/3D` lui réserve un rectangle sur la page. Le document porte aussi une
//! **vue par défaut** : la place de la caméra, son ouverture et la couleur du
//! fond, pour que le modèle s'affiche cadré dès l'ouverture plutôt que vu de
//! nulle part.
//!
//! Tant que le lecteur n'active pas le modèle, c'est **l'affiche** qui se
//! voit : l'image du modèle vu depuis sa vue par défaut, dessinée par notre
//! propre moteur 3D et posée dans la page. Le document montre donc quelque
//! chose partout — à l'impression, dans une vignette, dans un lecteur qui
//! ignore la 3D.

use acrux_core::{Error, Rect, Result};
use acrux_document::{Dict, Document, Name, Object};

use super::builder::Builder;
use super::paper::PageSetup;

/// Fabrique un document d'une page portant le modèle donné.
///
/// `model` est le contenu d'un fichier U3D. Le rectangle occupé est la page
/// moins ses marges.
///
/// # Errors
/// Modèle vide, ou document impossible à refermer.
pub fn from_3d(model: &[u8], setup: &PageSetup) -> Result<Document> {
    if model.is_empty() {
        return Err(Error::Corrupt("modèle 3D vide".into()));
    }
    let mut builder = Builder::new()?;
    let (width, height) = setup.page_size();
    let page = builder.add_page(width, height);
    let m = &setup.margins;
    let area = Rect::new(m.left, m.bottom, width - m.right, height - m.top);
    // L'affiche : le modèle vu depuis sa vue par défaut. À défaut (modèle
    // illisible), un aplat clair, qui vaut mieux qu'un trou dans la page.
    let scene = crate::three_d::u3d::parse(model);
    let framing = crate::three_d::Camera::framing(&scene);
    if let Some(poster) = poster(&scene, &framing, area) {
        let reference = poster.write(&builder.doc);
        builder.content(page).image("Ak3D", reference, area, None);
    } else {
        builder.content(page).fill_rect(area, [0.95, 0.95, 0.96]);
    }

    let mut stream = Dict::new();
    stream.insert(Name::new("Type"), Object::Name(Name::new("3D")));
    stream.insert(Name::new("Subtype"), Object::Name(Name::new("U3D")));
    stream.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(model.len()).unwrap_or(0)),
    );
    stream.insert(
        Name::new("VA"),
        Object::Array(vec![Object::Dict(view(&framing))]),
    );
    let reference = builder.doc.add(Object::Stream {
        dict: stream,
        raw: model.to_vec(),
    });

    let mut annot = Dict::new();
    annot.insert(Name::new("Type"), Object::Name(Name::new("Annot")));
    annot.insert(Name::new("Subtype"), Object::Name(Name::new("3D")));
    annot.insert(
        Name::new("Rect"),
        Object::Array(vec![
            Object::Real(area.x0),
            Object::Real(area.y0),
            Object::Real(area.x1),
            Object::Real(area.y1),
        ]),
    );
    // Imprimable, comme toute annotation qu'on veut voir sur papier.
    annot.insert(Name::new("F"), Object::Integer(4));
    annot.insert(Name::new("3DD"), Object::Reference(reference));
    annot.insert(Name::new("3DV"), Object::Integer(0));
    let mut activation = Dict::new();
    // Activé quand la page s'affiche, désactivé quand elle s'en va : c'est ce
    // que produisent les outils de CAO.
    activation.insert(Name::new("A"), Object::Name(Name::new("PV")));
    activation.insert(Name::new("D"), Object::Name(Name::new("PI")));
    annot.insert(Name::new("3DA"), Object::Dict(activation));
    builder.add_annotation(page, annot);
    builder.finish()
}

/// La vue par défaut : la caméra qui cadre le modèle, telle que notre moteur
/// la calcule, écrite dans le document pour que n'importe quel lecteur voie
/// la même chose.
fn view(camera: &crate::three_d::Camera) -> Dict {
    let mut view = Dict::new();
    view.insert(Name::new("Type"), Object::Name(Name::new("3DView")));
    view.insert(Name::new("XN"), Object::String(b"Vue par defaut".to_vec()));
    view.insert(Name::new("MS"), Object::Name(Name::new("M")));
    // Matrice caméra→monde : les trois axes de la caméra — une caméra PDF
    // regarde selon son axe des z **négatif** —, puis sa position.
    let (right, up, forward) = camera.basis();
    let eye = camera.eye();
    let c2w = [
        f64::from(right[0]),
        f64::from(right[1]),
        f64::from(right[2]),
        f64::from(up[0]),
        f64::from(up[1]),
        f64::from(up[2]),
        f64::from(-forward[0]),
        f64::from(-forward[1]),
        f64::from(-forward[2]),
        f64::from(eye[0]),
        f64::from(eye[1]),
        f64::from(eye[2]),
    ];
    view.insert(
        Name::new("C2W"),
        Object::Array(c2w.iter().map(|v| Object::Real(*v)).collect()),
    );
    view.insert(Name::new("CO"), Object::Real(f64::from(camera.distance)));
    let mut projection = Dict::new();
    projection.insert(Name::new("Subtype"), Object::Name(Name::new("P")));
    projection.insert(Name::new("FOV"), Object::Real(f64::from(camera.fov)));
    view.insert(Name::new("P"), Object::Dict(projection));
    let mut background = Dict::new();
    background.insert(Name::new("Type"), Object::Name(Name::new("3DBG")));
    background.insert(
        Name::new("C"),
        Object::Array(vec![
            Object::Real(0.95),
            Object::Real(0.95),
            Object::Real(0.96),
        ]),
    );
    view.insert(Name::new("BG"), Object::Dict(background));
    view
}

/// Rend l'affiche du modèle : son image vue depuis la vue par défaut.
///
/// La résolution suit la taille de l'aire, à peu près deux pixels par point :
/// de quoi rester net à l'écran sans alourdir le fichier.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // taille bornée à 16–2048 par le `clamp` juste au-dessus
fn poster(
    scene: &crate::three_d::Scene,
    camera: &crate::three_d::Camera,
    area: Rect,
) -> Option<crate::stamp::image::DecodedImage> {
    if scene.items.is_empty() {
        return None;
    }
    // Deux pixels par point, entre seize et deux mille : assez net à l'écran,
    // sans alourdir le fichier.
    let size = |v: f64| (v * 2.0).clamp(16.0, 2048.0).round() as u32;
    let (width, height) = (size(area.width()), size(area.height()));
    let bitmap = crate::three_d::render(scene, camera, width, height);
    let png = acrux_graphics::encode_png_rgb(width, height, &bitmap.to_rgb8_over_white());
    crate::stamp::image::decode(&png).ok()
}
