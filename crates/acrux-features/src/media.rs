//! Multimédia : retrouver les vidéos et les sons d'un document, et en sortir
//! le fichier.
//!
//! Un PDF ne contient pas « une vidéo » : il contient une annotation qui
//! occupe un rectangle, une action qui dit quoi jouer, et un fichier
//! incorporé quelque part. Trois générations de spécification se superposent,
//! et un lecteur sérieux doit les connaître toutes les trois :
//!
//! | Forme | Norme | Ce qu'Acrobat en fait |
//! |---|---|---|
//! | `/Screen` + action `/Rendition` | ISO 32000 §13.2 | la forme courante depuis Acrobat 6 |
//! | `/RichMedia` | §13.7 (extension Adobe, reprise en PDF 2.0) | ce qu'écrit Acrobat récent |
//! | `/Movie` | PDF 1.1, obsolète depuis | encore lu, jamais écrit |
//!
//! Ce module ne lit ni ne décode quoi que ce soit : il **trouve** le média,
//! dit de quoi il s'agit, et sait en extraire les octets. La lecture est
//! l'affaire de la plateforme (`acrux-app`), qui demande au système.
//!
//! # Ce qui est refusé, et pourquoi
//!
//! Un média peut être **extérieur** au document : un chemin sur le disque ou
//! une adresse réseau. Un fichier PDF est une donnée qui vient d'ailleurs ;
//! ouvrir ce qu'il désigne sans rien demander reviendrait à exécuter ses
//! instructions. Ces médias sont donc listés comme [`Source::External`] et
//! l'application ne les ouvrira pas d'elle-même.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use acrux_core::{Error, Rect, Result};
use acrux_document::text::decode_text_string;
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef};

/// Profondeur maximale de parcours : les renditions peuvent se contenir les
/// unes les autres, et un document malveillant pourrait le faire en rond.
const MAX_DEPTH: usize = 16;

/// Forme sous laquelle le média est déclaré.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Annotation `/Screen` avec une action de rendition (§13.2).
    Screen,
    /// Annotation `/RichMedia` (§13.7).
    RichMedia,
    /// Annotation `/Movie`, forme obsolète (PDF 1.1).
    Movie,
}

impl Kind {
    /// Nom court pour les listes.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Kind::Screen => "écran",
            Kind::RichMedia => "média enrichi",
            Kind::Movie => "film (obsolète)",
        }
    }
}

/// D'où viennent les octets du média.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Fichier incorporé au document : la référence du flux, et sa longueur
    /// annoncée. Les octets ne sont lus qu'à l'extraction — une vidéo pèse
    /// souvent plus que tout le reste du fichier.
    Embedded {
        /// Flux `/EmbeddedFile`.
        stream: ObjectRef,
        /// Taille déclarée, en octets, si le document la donne.
        size: Option<u64>,
    },
    /// Chemin ou adresse **hors** du document. Jamais ouvert tout seul.
    External(String),
}

/// Un média trouvé dans le document.
#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    /// Index de la page.
    pub page: usize,
    /// Position dans le tableau `/Annots` de la page.
    pub index: usize,
    /// Forme de déclaration.
    pub kind: Kind,
    /// Rectangle occupé, en coordonnées de page.
    pub rect: Rect,
    /// Nom du fichier, tel que le document le donne.
    pub name: Option<String>,
    /// Type de contenu déclaré (`video/mp4`, `audio/mpeg`…).
    pub content_type: Option<String>,
    /// Où sont les octets.
    pub source: Option<Source>,
    /// Vrai si l'annotation porte une apparence : c'est l'affiche, ce qu'on
    /// voit quand le média ne joue pas.
    pub has_poster: bool,
}

impl Media {
    /// Vrai si le média est incorporé, donc lisible sans rien ouvrir d'autre.
    #[must_use]
    pub fn is_embedded(&self) -> bool {
        matches!(self.source, Some(Source::Embedded { .. }))
    }

    /// Extension de fichier déduite du nom ou du type déclaré.
    #[must_use]
    pub fn extension(&self) -> String {
        if let Some(name) = &self.name {
            if let Some(dot) = name.rfind('.') {
                let ext: String = name[dot + 1..]
                    .chars()
                    .filter(char::is_ascii_alphanumeric)
                    .collect();
                if !ext.is_empty() && ext.len() <= 5 {
                    return ext.to_ascii_lowercase();
                }
            }
        }
        match self.content_type.as_deref() {
            Some("video/mp4") => "mp4".into(),
            Some("video/quicktime") => "mov".into(),
            Some("video/x-m4v") => "m4v".into(),
            Some("video/webm") => "webm".into(),
            Some("audio/mpeg") => "mp3".into(),
            Some("audio/mp4") => "m4a".into(),
            Some("audio/wav" | "audio/x-wav") => "wav".into(),
            _ => "bin".into(),
        }
    }

    /// Vrai si le type déclaré, ou l'extension, désigne de l'audio seul.
    #[must_use]
    pub fn is_audio_only(&self) -> bool {
        if let Some(t) = &self.content_type {
            return t.starts_with("audio/");
        }
        matches!(self.extension().as_str(), "mp3" | "m4a" | "wav" | "aac")
    }
}

/// Inventaire des médias de tout le document, dans l'ordre des pages.
///
/// # Errors
/// Document illisible.
pub fn list(doc: &Document) -> Result<Vec<Media>> {
    let mut out = Vec::new();
    for (page_index, page) in collect_pages(doc)?.iter().enumerate() {
        let Some(annots) = page.dict.get(&Name::new("Annots")) else {
            continue;
        };
        let Ok(resolved) = doc.resolve(annots) else {
            continue;
        };
        let Some(list) = resolved.as_array() else {
            continue;
        };
        for (index, entry) in list.iter().enumerate() {
            let Ok(annot) = doc.resolve(entry) else {
                continue;
            };
            let Some(annot) = annot.as_dict() else {
                continue;
            };
            if let Some(media) = read_annotation(doc, annot, page_index, index) {
                out.push(media);
            }
        }
    }
    Ok(out)
}

/// Lit une annotation et en tire un média, si c'en est un.
fn read_annotation(doc: &Document, annot: &Dict, page: usize, index: usize) -> Option<Media> {
    let subtype = name_of(doc, annot, "Subtype")?;
    let kind = match subtype.as_str() {
        "Screen" => Kind::Screen,
        "RichMedia" => Kind::RichMedia,
        "Movie" => Kind::Movie,
        _ => return None,
    };
    let rect = rect_of(doc, annot).unwrap_or_default();
    let has_poster = doc
        .dict_get(annot, "AP")
        .ok()
        .flatten()
        .and_then(|ap| ap.as_dict().map(|d| d.contains_key(&Name::new("N"))))
        .unwrap_or(false);
    let clip = match kind {
        Kind::Screen => screen_clip(doc, annot),
        Kind::RichMedia => rich_media_asset(doc, annot),
        Kind::Movie => movie_clip(doc, annot),
    };
    let (name, content_type, source) = clip.unwrap_or((None, None, None));
    // Le titre de l'annotation sert de nom quand le clip n'en donne pas.
    let name = name.or_else(|| text_of(doc, annot, "T"));
    Some(Media {
        page,
        index,
        kind,
        rect,
        name,
        content_type,
        source,
        has_poster,
    })
}

/// Ce qu'un `/Screen` désigne, en suivant action → rendition → clip.
fn screen_clip(doc: &Document, annot: &Dict) -> Option<Clip> {
    // L'action est dans `/A`, ou dans les actions additionnelles `/AA` —
    // `/PV` (page visible) est ce qu'écrivent les documents qui démarrent
    // tout seuls.
    let action = doc
        .dict_get(annot, "A")
        .ok()
        .flatten()
        .and_then(|a| a.as_dict().cloned())
        .or_else(|| {
            let aa = doc.dict_get(annot, "AA").ok().flatten()?;
            let aa = aa.as_dict()?;
            for key in ["PV", "PO", "E", "U", "D"] {
                if let Some(entry) = doc.dict_get(aa, key).ok().flatten() {
                    if let Some(d) = entry.as_dict() {
                        return Some(d.clone());
                    }
                }
            }
            None
        })?;
    let rendition = doc.dict_get(&action, "R").ok().flatten()?;
    let rendition = rendition.as_dict()?;
    rendition_clip(doc, rendition, 0)
}

/// Nom, type et source d'un clip.
type Clip = (Option<String>, Option<String>, Option<Source>);

/// Descend une rendition jusqu'au clip média (§13.2.3).
fn rendition_clip(doc: &Document, rendition: &Dict, depth: usize) -> Option<Clip> {
    if depth > MAX_DEPTH {
        return None;
    }
    match name_of(doc, rendition, "S").as_deref() {
        // Rendition média : le clip est dans `/C`.
        Some("MR") => {
            let clip = doc.dict_get(rendition, "C").ok().flatten()?;
            media_clip(doc, clip.as_dict()?, depth + 1)
        }
        // Rendition de sélection : la première de `/R` qui donne quelque chose.
        Some("SR") => {
            let list = doc.dict_get(rendition, "R").ok().flatten()?;
            for entry in list.as_array()? {
                let Ok(resolved) = doc.resolve(entry) else {
                    continue;
                };
                let Some(dict) = resolved.as_dict() else {
                    continue;
                };
                if let Some(found) = rendition_clip(doc, dict, depth + 1) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

/// Lit un `/MediaClip` (§13.2.4).
fn media_clip(doc: &Document, clip: &Dict, depth: usize) -> Option<Clip> {
    if depth > MAX_DEPTH {
        return None;
    }
    // Section de clip : le vrai clip est dans `/D`.
    if name_of(doc, clip, "S").as_deref() == Some("MCS") {
        let inner = doc.dict_get(clip, "D").ok().flatten()?;
        return media_clip(doc, inner.as_dict()?, depth + 1);
    }
    // Données de clip : `/D` est le fichier, `/CT` son type.
    let name = text_of(doc, clip, "N");
    let content_type = text_of(doc, clip, "CT");
    let data = doc.dict_get(clip, "D").ok().flatten()?;
    let source = file_source(doc, &data);
    Some((name, content_type, source))
}

/// Lit une annotation `/Movie` (PDF 1.1, §9.4 de l'époque).
fn movie_clip(doc: &Document, annot: &Dict) -> Option<Clip> {
    let movie = doc.dict_get(annot, "Movie").ok().flatten()?;
    let movie = movie.as_dict()?;
    let data = doc.dict_get(movie, "F").ok().flatten()?;
    let source = file_source(doc, &data);
    Some((None, None, source))
}

/// Premier bien d'une annotation `/RichMedia` (§13.7).
///
/// Le contenu porte un arbre de noms `/Assets` ; la configuration dit lequel
/// est joué, mais dans les faits un média enrichi vidéo n'en contient qu'un.
fn rich_media_asset(doc: &Document, annot: &Dict) -> Option<Clip> {
    let content = doc.dict_get(annot, "RichMediaContent").ok().flatten()?;
    let content = content.as_dict()?;
    let assets = doc.dict_get(content, "Assets").ok().flatten()?;
    let assets = assets.as_dict()?;
    let mut found: Vec<(String, Object)> = Vec::new();
    let mut seen = HashSet::new();
    collect_names(doc, assets, &mut found, &mut seen, 0);
    // Un média enrichi embarque souvent une affiche à côté de la vidéo : on
    // prend le premier bien qui ressemble à un média jouable.
    let pick = found
        .iter()
        .find(|(name, _)| playable_extension(name))
        .or_else(|| found.first())?;
    let spec = doc.resolve(&pick.1).ok()?;
    let source = file_source(doc, &spec);
    Some((Some(pick.0.clone()), None, source))
}

/// Vrai si le nom porte une extension de média jouable.
fn playable_extension(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        ".mp4", ".m4v", ".mov", ".webm", ".avi", ".wmv", ".mkv", ".mp3", ".m4a", ".wav", ".aac",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

/// Parcourt un arbre de noms et rend ses paires (§7.9.6).
fn collect_names(
    doc: &Document,
    node: &Dict,
    out: &mut Vec<(String, Object)>,
    seen: &mut HashSet<u32>,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        return;
    }
    if let Some(list) = doc.dict_get(node, "Names").ok().flatten() {
        if let Some(pairs) = list.as_array() {
            for pair in pairs.chunks_exact(2) {
                if let Ok(key) = doc.resolve(&pair[0]) {
                    if let Object::String(s) = &*key {
                        out.push((decode_text_string(s), pair[1].clone()));
                    }
                }
            }
        }
    }
    if let Some(kids) = doc.dict_get(node, "Kids").ok().flatten() {
        if let Some(list) = kids.as_array() {
            for kid in list {
                if let Object::Reference(r) = kid {
                    if !seen.insert(r.number) {
                        continue;
                    }
                }
                if let Ok(resolved) = doc.resolve(kid) {
                    if let Some(dict) = resolved.as_dict() {
                        collect_names(doc, dict, out, seen, depth + 1);
                    }
                }
            }
        }
    }
}

/// Où sont les octets décrits par une spécification de fichier (§7.11).
///
/// Accepte aussi un flux passé directement : certains producteurs écrivent le
/// média là où la norme attend une spécification.
fn file_source(doc: &Document, spec: &Object) -> Option<Source> {
    // Un flux directement à la place de la spécification.
    if matches!(spec, Object::Stream { .. }) {
        return None; // sans référence, on ne peut pas y revenir
    }
    let dict = spec.as_dict()?;
    // Fichier incorporé : `/EF /F` (ou `/UF`, `/DOS`, `/Mac`, `/Unix`).
    if let Some(ef) = doc.dict_get(dict, "EF").ok().flatten() {
        if let Some(ef) = ef.as_dict() {
            for key in ["F", "UF", "DOS", "Mac", "Unix"] {
                if let Some(Object::Reference(r)) = ef.get(&Name::new(key)) {
                    let size = doc
                        .get(*r)
                        .ok()
                        .and_then(|o| o.as_dict().cloned())
                        .and_then(|d| embedded_size(doc, &d));
                    return Some(Source::Embedded { stream: *r, size });
                }
            }
        }
    }
    // Sinon, un nom de fichier extérieur.
    for key in ["UF", "F", "DOS", "Unix", "Mac"] {
        if let Some(name) = text_of(doc, dict, key) {
            if !name.is_empty() {
                return Some(Source::External(name));
            }
        }
    }
    None
}

/// Taille annoncée d'un fichier incorporé (`/Params /Size`, sinon `/Length`).
fn embedded_size(doc: &Document, stream: &Dict) -> Option<u64> {
    if let Some(params) = doc.dict_get(stream, "Params").ok().flatten() {
        if let Some(params) = params.as_dict() {
            if let Some(size) = doc
                .dict_get(params, "Size")
                .ok()
                .flatten()
                .and_then(|o| o.as_i64())
            {
                return u64::try_from(size).ok();
            }
        }
    }
    doc.dict_get(stream, "Length")
        .ok()
        .flatten()
        .and_then(|o| o.as_i64())
        .and_then(|v| u64::try_from(v).ok())
}

/// Écrit le média incorporé dans un fichier et rend son chemin.
///
/// Le nom est choisi par nous, jamais repris du document : un PDF peut
/// nommer son fichier `..\..\Windows\System32\quelquechose.dll`, et un nom
/// venu d'une donnée n'a rien à faire dans un chemin.
///
/// # Errors
/// Média extérieur au document, flux illisible, ou écriture impossible.
pub fn extract(doc: &Document, media: &Media, dir: &Path) -> Result<PathBuf> {
    let Some(Source::Embedded { stream, .. }) = &media.source else {
        return Err(Error::Unsupported(
            "ce média n'est pas incorporé au document".into(),
        ));
    };
    let object = doc.get(*stream)?;
    let decoded = doc.stream_data(&object)?;
    if decoded.data.is_empty() {
        return Err(Error::Corrupt("média incorporé vide".into()));
    }
    std::fs::create_dir_all(dir).map_err(|e| Error::Corrupt(format!("{} : {e}", dir.display())))?;
    let path = dir.join(format!(
        "acrux-media-p{}-{}.{}",
        media.page + 1,
        media.index,
        media.extension()
    ));
    std::fs::write(&path, &decoded.data)
        .map_err(|e| Error::Corrupt(format!("{} : {e}", path.display())))?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// Petits accès de dictionnaire.
// ---------------------------------------------------------------------------

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
