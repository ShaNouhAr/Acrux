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
//! | `/Sound` | PDF 1.2, §13.3 | du son seul, sans image |
//!
//! Le `/Sound` est à part : il ne désigne pas un fichier mais **des
//! échantillons bruts**, avec leur fréquence, leur nombre de voies et leur
//! codage, posés tels quels dans un flux. Il n'y a pas d'en-tête à lire, pas
//! de conteneur : c'est du son nu, comme sur un disque des années quatre-vingt.
//! L'extraction lui rend donc un en-tête WAV, seul moyen d'en faire un fichier
//! que le reste du monde sait ouvrir.
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
    /// Annotation `/Sound` : du son seul, en échantillons bruts (§13.3).
    Sound,
}

impl Kind {
    /// Nom court pour les listes.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Kind::Screen => "écran",
            Kind::RichMedia => "média enrichi",
            Kind::Movie => "film (obsolète)",
            Kind::Sound => "son",
        }
    }
}

/// Codage des échantillons d'un flux `/Sound` (§13.3, entrée `/E`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SoundEncoding {
    /// `/Raw` : entiers **non signés**, le zéro au milieu de l'intervalle.
    #[default]
    Raw,
    /// `/Signed` : entiers signés en complément à deux.
    Signed,
    /// `/muLaw` : loi µ, la compression téléphonique nord-américaine.
    MuLaw,
    /// `/ALaw` : loi A, son équivalent européen.
    ALaw,
}

impl SoundEncoding {
    /// Lit le nom d'un `/E`.
    #[must_use]
    pub fn from_name(name: &str) -> SoundEncoding {
        match name {
            "Signed" => SoundEncoding::Signed,
            "muLaw" => SoundEncoding::MuLaw,
            "ALaw" => SoundEncoding::ALaw,
            _ => SoundEncoding::Raw,
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
    /// Échantillons bruts d'un flux `/Sound` (§13.3) : il n'y a pas de
    /// fichier, seulement des nombres et de quoi les interpréter.
    Samples {
        /// Flux `/Sound`.
        stream: ObjectRef,
        /// Fréquence d'échantillonnage, en hertz (`/R`).
        rate: u32,
        /// Nombre de voies (`/C`, 1 par défaut).
        channels: u16,
        /// Bits par échantillon et par voie (`/B`, 8 par défaut).
        bits: u16,
        /// Codage (`/E`).
        encoding: SoundEncoding,
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
        matches!(
            self.source,
            Some(Source::Embedded { .. } | Source::Samples { .. })
        )
    }

    /// Extension de fichier déduite du nom ou du type déclaré.
    #[must_use]
    pub fn extension(&self) -> String {
        // Des échantillons nus deviennent un WAV : c'est le seul emballage
        // qui se contente de dire « voici du PCM, à telle fréquence ».
        if matches!(self.source, Some(Source::Samples { .. })) {
            return "wav".into();
        }
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
        if self.kind == Kind::Sound {
            return true;
        }
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
        "Sound" => Kind::Sound,
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
        Kind::Sound => sound_clip(doc, annot),
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

/// Ce qu'une annotation `/Sound` désigne.
///
/// Le flux est dans `/Sound`, directement sur l'annotation, ou porté par une
/// action `/S /Sound` — les deux formes existent, souvent dans le même
/// document.
fn sound_clip(doc: &Document, annot: &Dict) -> Option<Clip> {
    // On prend la **référence** sans la résoudre : c'est elle qu'il faudra
    // pour revenir chercher les octets au moment de l'extraction.
    let reference = if let Some(Object::Reference(r)) = annot.get(&Name::new("Sound")) {
        *r
    } else {
        let action = doc.dict_get(annot, "A").ok().flatten()?;
        let action = action.as_dict()?;
        if name_of(doc, action, "S").as_deref() != Some("Sound") {
            return None;
        }
        match action.get(&Name::new("Sound")) {
            Some(Object::Reference(r)) => *r,
            _ => return None,
        }
    };
    let stream = doc.get(reference).ok()?;
    let dict = stream.as_dict()?;
    // `/R` est obligatoire : sans fréquence, des échantillons ne veulent rien
    // dire. Un flux qui n'en donne pas n'est pas lisible, et le dire vaut
    // mieux que jouer n'importe quoi.
    let rate = u32::try_from(integer_of(doc, dict, "R")?)
        .ok()
        .filter(|r| *r > 0)?;
    let channels = integer_of(doc, dict, "C")
        .and_then(|c| u16::try_from(c).ok())
        .unwrap_or(1)
        .clamp(1, 8);
    let bits = integer_of(doc, dict, "B")
        .and_then(|b| u16::try_from(b).ok())
        .unwrap_or(8);
    // Au-delà de 16 bits, `waveOut` et la plupart des lecteurs renoncent ; en
    // deçà de 8, la norme n'en prévoit pas.
    if !matches!(bits, 8 | 16) {
        return None;
    }
    let encoding =
        name_of(doc, dict, "E").map_or(SoundEncoding::Raw, |n| SoundEncoding::from_name(&n));
    Some((
        text_of(doc, annot, "T"),
        Some(String::from("audio/wav")),
        Some(Source::Samples {
            stream: reference,
            rate,
            channels,
            bits,
            encoding,
        }),
    ))
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
    let bytes = match &media.source {
        Some(Source::Embedded { stream, .. }) => {
            let object = doc.get(*stream)?;
            doc.stream_data(&object)?.data
        }
        Some(Source::Samples {
            stream,
            rate,
            channels,
            bits,
            encoding,
        }) => {
            let object = doc.get(*stream)?;
            let samples = doc.stream_data(&object)?.data;
            wav(&samples, *rate, *channels, *bits, *encoding)
        }
        _ => {
            return Err(Error::Unsupported(
                "ce média n'est pas incorporé au document".into(),
            ))
        }
    };
    if bytes.is_empty() {
        return Err(Error::Corrupt("média incorporé vide".into()));
    }
    std::fs::create_dir_all(dir).map_err(|e| Error::Corrupt(format!("{} : {e}", dir.display())))?;
    let path = dir.join(format!(
        "acrux-media-p{}-{}.{}",
        media.page + 1,
        media.index,
        media.extension()
    ));
    std::fs::write(&path, &bytes)
        .map_err(|e| Error::Corrupt(format!("{} : {e}", path.display())))?;
    Ok(path)
}

/// Emballe des échantillons de PDF dans un fichier WAV.
///
/// Trois conversions, et chacune a sa raison.
///
/// Les échantillons de plus d'un octet sont rangés **gros-boutiens** dans un
/// PDF (§13.3) et **petits-boutiens** dans un WAV : il faut les retourner.
/// C'est le genre de détail qui, oublié, donne un souffle au lieu d'un son.
///
/// Le codage `/Raw` est non signé, `/Signed` est signé. Or le WAV veut du non
/// signé sur 8 bits et du signé sur 16 : l'un des deux cas demande de décaler
/// les valeurs d'une demi-amplitude.
///
/// Les lois µ et A sont des compressions logarithmiques de la téléphonie : un
/// octet y porte l'équivalent de douze à treize bits. Le WAV sait les déclarer
/// telles quelles, mais tous les lecteurs ne les décodent pas ; on les ramène
/// donc à du PCM 16 bits, ce que tout le monde sait jouer.
fn wav(samples: &[u8], rate: u32, channels: u16, bits: u16, encoding: SoundEncoding) -> Vec<u8> {
    let (data, bits) = match encoding {
        SoundEncoding::MuLaw => (
            samples
                .iter()
                .flat_map(|b| mu_law(*b).to_le_bytes())
                .collect(),
            16,
        ),
        SoundEncoding::ALaw => (
            samples
                .iter()
                .flat_map(|b| a_law(*b).to_le_bytes())
                .collect(),
            16,
        ),
        SoundEncoding::Raw if bits == 16 => {
            // Non signé sur 16 bits : le WAV n'en veut pas, on recentre.
            let converted = samples
                .chunks_exact(2)
                .flat_map(|c| {
                    let value = u16::from_be_bytes([c[0], c[1]]);
                    // Recentrer revient à basculer le bit de signe : le
                    // complément à deux est fait pour cela.
                    #[allow(clippy::cast_possible_wrap)] // recentrage voulu
                    let centre = value.wrapping_sub(0x8000) as i16;
                    centre.to_le_bytes()
                })
                .collect();
            (converted, 16)
        }
        SoundEncoding::Raw => (samples.to_vec(), 8),
        SoundEncoding::Signed if bits == 16 => (
            samples
                .chunks_exact(2)
                .flat_map(|c| [c[1], c[0]])
                .collect::<Vec<u8>>(),
            16,
        ),
        SoundEncoding::Signed => {
            // Signé sur 8 bits : le WAV veut du non signé, on recentre.
            let converted = samples
                .iter()
                .map(|b| {
                    #[allow(clippy::cast_possible_wrap)] // relecture signée
                    let signe = *b as i8;
                    let value = i16::from(signe) + 128;
                    u8::try_from(value.clamp(0, 255)).unwrap_or(128)
                })
                .collect();
            (converted, 8)
        }
    };
    let block_align = channels * bits / 8;
    let byte_rate = rate * u32::from(block_align);
    let mut out = Vec::with_capacity(data.len() + 44);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(
        &u32::try_from(36 + data.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM entier
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

/// Décode un octet de loi µ (G.711) en entier signé sur 16 bits.
fn mu_law(byte: u8) -> i16 {
    let byte = !byte;
    let sign = byte & 0x80;
    let exponent = (byte >> 4) & 0x07;
    let mantissa = byte & 0x0F;
    let magnitude = (i32::from(mantissa) << 3 | 0x84) << exponent;
    let value = magnitude - 0x84;
    let value = i16::try_from(value.clamp(0, i32::from(i16::MAX))).unwrap_or(i16::MAX);
    if sign == 0 {
        value
    } else {
        -value
    }
}

/// Décode un octet de loi A (G.711) en entier signé sur 16 bits.
fn a_law(byte: u8) -> i16 {
    let byte = byte ^ 0x55;
    let sign = byte & 0x80;
    let exponent = (byte >> 4) & 0x07;
    let mantissa = i32::from(byte & 0x0F);
    let magnitude = if exponent == 0 {
        (mantissa << 4) | 8
    } else {
        ((mantissa << 4) | 0x108) << (exponent - 1)
    };
    let value = i16::try_from(magnitude.clamp(0, i32::from(i16::MAX))).unwrap_or(i16::MAX);
    if sign == 0 {
        -value
    } else {
        value
    }
}

// ---------------------------------------------------------------------------
// Petits accès de dictionnaire.
// ---------------------------------------------------------------------------

fn integer_of(doc: &Document, dict: &Dict, key: &str) -> Option<i64> {
    doc.dict_get(dict, key).ok()??.as_i64()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Lit les champs d'un en-tête WAV.
    fn header(wav: &[u8]) -> (u16, u16, u32, u16, usize) {
        let get16 = |i: usize| u16::from_le_bytes([wav[i], wav[i + 1]]);
        let get32 = |i: usize| u32::from_le_bytes([wav[i], wav[i + 1], wav[i + 2], wav[i + 3]]);
        (
            get16(20),          // format
            get16(22),          // voies
            get32(24),          // fréquence
            get16(34),          // bits
            get32(40) as usize, // octets de données
        )
    }

    #[test]
    fn les_echantillons_signes_se_retournent_en_wav() {
        // Deux échantillons 16 bits, rangés gros-boutiens comme dans un PDF.
        let samples = [0x12, 0x34, 0xFF, 0x80];
        let wav = wav(&samples, 8000, 1, 16, SoundEncoding::Signed);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        let (format, voies, frequence, bits, taille) = header(&wav);
        assert_eq!((format, voies, frequence, bits), (1, 1, 8000, 16));
        assert_eq!(taille, 4);
        // Chaque paire d'octets est inversée : le WAV est petit-boutien.
        assert_eq!(&wav[44..48], &[0x34, 0x12, 0x80, 0xFF]);
    }

    #[test]
    fn le_non_signe_se_recentre() {
        // 8 bits : le WAV veut du non signé, comme `/Raw`. Rien à faire.
        let wav8 = wav(&[0, 128, 255], 11025, 1, 8, SoundEncoding::Raw);
        assert_eq!(&wav8[44..47], &[0, 128, 255]);
        // 8 bits signés : il faut décaler de 128.
        let signe = wav(&[0x80, 0x00, 0x7F], 11025, 1, 8, SoundEncoding::Signed);
        assert_eq!(&signe[44..47], &[0, 128, 255]);
        // 16 bits non signés : le milieu de l'intervalle devient le zéro.
        let wav16 = wav(&[0x80, 0x00], 8000, 1, 16, SoundEncoding::Raw);
        assert_eq!(&wav16[44..46], &[0, 0]);
    }

    #[test]
    fn les_lois_mu_et_a_se_decodent_en_seize_bits() {
        // La loi µ code le silence par 0xFF, et la loi A par 0xD5.
        assert_eq!(mu_law(0xFF), 0);
        assert!(a_law(0xD5).abs() <= 8);
        // Les extrêmes sont bien de part et d'autre du zéro.
        assert!(mu_law(0x00) < -30_000);
        assert!(mu_law(0x80) > 30_000);
        // Un octet donne deux octets de PCM.
        let wav = wav(&[0xFF, 0x00], 8000, 1, 8, SoundEncoding::MuLaw);
        let (_, _, _, bits, taille) = header(&wav);
        assert_eq!((bits, taille), (16, 4));
    }

    #[test]
    fn les_codages_se_lisent_et_ont_un_defaut() {
        assert_eq!(SoundEncoding::from_name("Signed"), SoundEncoding::Signed);
        assert_eq!(SoundEncoding::from_name("muLaw"), SoundEncoding::MuLaw);
        assert_eq!(SoundEncoding::from_name("ALaw"), SoundEncoding::ALaw);
        // Un nom inconnu ne fait pas tomber la lecture : c'est `/Raw`.
        assert_eq!(SoundEncoding::from_name("Inconnu"), SoundEncoding::Raw);
        assert_eq!(SoundEncoding::default(), SoundEncoding::Raw);
    }

    /// Construit un PDF minimal portant une annotation `/Sound`.
    fn document_avec_son(samples: &[u8]) -> Document {
        let mut out: Vec<u8> = Vec::new();
        let mut offsets = Vec::new();
        let push = |out: &mut Vec<u8>, offsets: &mut Vec<usize>, body: &[u8]| {
            offsets.push(out.len());
            out.extend_from_slice(body);
        };
        out.extend_from_slice(
            b"%PDF-1.7
",
        );
        push(
            &mut out,
            &mut offsets,
            b"1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
",
        );
        push(
            &mut out,
            &mut offsets,
            b"2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
",
        );
        push(
            &mut out,
            &mut offsets,
            b"3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100]               /Annots [4 0 R] >> endobj
",
        );
        push(
            &mut out,
            &mut offsets,
            b"4 0 obj << /Type /Annot /Subtype /Sound /Rect [10 20 90 60]               /T (sonnerie) /Sound 5 0 R >> endobj
",
        );
        offsets.push(out.len());
        out.extend_from_slice(
            format!(
                "5 0 obj << /Type /Sound /R 22050 /C 1 /B 16 /E /Signed /Length {} >>
stream
",
                samples.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(samples);
        out.extend_from_slice(
            b"
endstream endobj
",
        );
        let xref = out.len();
        out.extend_from_slice(
            b"xref
0 6
0000000000 65535 f 
",
        );
        for offset in &offsets {
            out.extend_from_slice(
                format!(
                    "{offset:010} 00000 n 
"
                )
                .as_bytes(),
            );
        }
        out.extend_from_slice(
            format!(
                "trailer << /Size 6 /Root 1 0 R >>
startxref
{xref}
%%EOF
"
            )
            .as_bytes(),
        );
        match Document::from_bytes(out) {
            Ok(doc) => doc,
            Err(e) => unreachable!("PDF d'essai illisible : {e}"),
        }
    }

    #[test]
    fn une_annotation_sound_est_trouvee_et_extraite() {
        // Quatre échantillons 16 bits signés, gros-boutiens comme le veut §13.3.
        let samples = [0x01, 0x02, 0x03, 0x04, 0xFF, 0xFE, 0x00, 0x10];
        let doc = document_avec_son(&samples);
        let medias = match list(&doc) {
            Ok(m) => m,
            Err(e) => unreachable!("inventaire impossible : {e}"),
        };
        assert_eq!(medias.len(), 1, "{medias:?}");
        let media = &medias[0];
        assert_eq!(media.kind, Kind::Sound);
        assert_eq!(media.name.as_deref(), Some("sonnerie"));
        assert!(media.is_audio_only() && media.is_embedded());
        let Some(Source::Samples {
            rate,
            channels,
            bits,
            encoding,
            ..
        }) = &media.source
        else {
            unreachable!("le son doit être en échantillons : {:?}", media.source)
        };
        assert_eq!((*rate, *channels, *bits), (22050, 1, 16));
        assert_eq!(*encoding, SoundEncoding::Signed);

        // L'extraction rend un WAV jouable, avec les octets retournés.
        let dir = std::env::temp_dir().join("acrux-essai-son");
        let path = match extract(&doc, media, &dir) {
            Ok(p) => p,
            Err(e) => unreachable!("extraction impossible : {e}"),
        };
        let bytes = std::fs::read(&path).unwrap_or_default();
        let _ = std::fs::remove_file(&path);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("wav"));
        let (format, voies, frequence, bits, taille) = header(&bytes);
        assert_eq!((format, voies, frequence, bits), (1, 1, 22050, 16));
        assert_eq!(taille, samples.len());
        assert_eq!(
            &bytes[44..52],
            &[0x02, 0x01, 0x04, 0x03, 0xFE, 0xFF, 0x10, 0x00]
        );
    }

    #[test]
    fn un_son_est_toujours_de_laudio_seul() {
        let media = Media {
            page: 0,
            index: 0,
            kind: Kind::Sound,
            rect: Rect::default(),
            name: None,
            content_type: None,
            source: Some(Source::Samples {
                stream: ObjectRef {
                    number: 1,
                    generation: 0,
                },
                rate: 8000,
                channels: 1,
                bits: 8,
                encoding: SoundEncoding::Raw,
            }),
            has_poster: false,
        };
        assert!(media.is_audio_only());
        assert!(media.is_embedded());
        // Des échantillons nus ressortent en WAV.
        assert_eq!(media.extension(), "wav");
        assert_eq!(Kind::Sound.label(), "son");
    }
}
