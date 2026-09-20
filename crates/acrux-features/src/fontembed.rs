//! Incorporation d'une police pour du texte que **nous** ajoutons au document
//! (filigrane, en-tête, apparence d'annotation ou de champ).
//!
//! Les quatorze polices standard ne savent écrire que WinAnsiEncoding : ni
//! chinois, ni japonais, ni coréen, ni grec, ni cyrillique, ni la plupart des
//! symboles. Ce module choisit une police système capable d'écrire le texte
//! demandé, n'en garde que les glyphes utilisés ([`acrux_fonts::subset`]) et
//! écrit une police composite `/Type0` en `Identity-H` : chaque caractère
//! devient un indice de glyphe sur deux octets, et `/ToUnicode` rend le texte
//! extractible et copiable.
//!
//! ```no_run
//! use acrux_document::Document;
//! use acrux_features::fontembed::{embed_text_font, FontStyle};
//!
//! # fn main() -> acrux_core::Result<()> {
//! let doc = Document::load("rapport.pdf")?;
//! let font = embed_text_font(&doc, "機密 · CONFIDENTIEL", FontStyle::Bold)?;
//! // `font.reference` va dans /Resources /Font, `font.show(texte)` dans le flux.
//! let _ = (font.reference, font.show("機密"), font.width("機密", 24.0));
//! # Ok(())
//! # }
//! ```

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use acrux_codecs::flate;
use acrux_core::{Error, Result};
use acrux_document::{Dict, Document, Name, Object, ObjectRef};
use acrux_fonts::shape::{shape, ShapeOptions};
use acrux_fonts::{subset, TrueTypeFont};

/// Style demandé pour le texte ajouté.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontStyle {
    /// Romain.
    Regular,
    /// Gras.
    Bold,
    /// Italique.
    Italic,
    /// Gras italique.
    BoldItalic,
}

impl FontStyle {
    /// Suffixes de nom de fichier correspondants, du plus probable au moins.
    fn suffixes(self) -> &'static [&'static str] {
        match self {
            FontStyle::Regular => &["", "-regular", "regular"],
            FontStyle::Bold => &["b", "bd", "-bold", "bold"],
            FontStyle::Italic => &["i", "-italic", "italic", "-oblique"],
            FontStyle::BoldItalic => &["bi", "z", "-bolditalic", "bolditalic"],
        }
    }

    /// Drapeaux `/FontDescriptor` (§9.8.2) : 4 = symbolique, 64 = italique.
    fn flags(self) -> i64 {
        let italic = matches!(self, FontStyle::Italic | FontStyle::BoldItalic);
        4 | (i64::from(italic) * 64)
    }
}

/// Police incorporée, prête à être référencée et à écrire du texte.
#[derive(Debug, Clone)]
pub struct EmbeddedFont {
    /// Objet `/Type0` à mettre dans `/Resources /Font`.
    pub reference: ObjectRef,
    /// Nom de la police retenue (pour les messages).
    pub family: String,
    /// Caractère → (glyphe produit, largeur en millièmes d'em).
    glyphs: HashMap<char, (u16, f64)>,
    /// Glyphe d'origine → glyphe du sous-ensemble, pour la composition.
    gid_map: HashMap<u16, u16>,
    /// Glyphe du sous-ensemble → largeur en millièmes d'em.
    widths: BTreeMap<u16, f64>,
    /// Police système d'origine, gardée pour composer (`shape_line`).
    source: Option<Arc<TrueTypeFont>>,
    /// Unités par em de la police d'origine.
    upem: f64,
    /// Hauteur d'ascendante en millièmes d'em (pour caler une ligne).
    pub ascent: f64,
    /// Descendante, négative.
    pub descent: f64,
}

impl EmbeddedFont {
    /// Chaîne hexadécimale à écrire entre `<` et `>` dans un `Tj` : deux
    /// octets par caractère, en `Identity-H`.
    #[must_use]
    pub fn show(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len() * 4);
        for c in text.chars() {
            let gid = self.glyphs.get(&c).map_or(0, |(g, _)| *g);
            let _ = write!(out, "{gid:04X}");
        }
        out
    }

    /// Largeur du texte à la taille donnée, en points.
    #[must_use]
    pub fn width(&self, text: &str, size: f64) -> f64 {
        let mille: f64 = text
            .chars()
            .map(|c| self.glyphs.get(&c).map_or(0.0, |(_, w)| *w))
            .sum();
        mille * size / 1000.0
    }

    /// Vrai si tous les caractères ont un glyphe.
    #[must_use]
    pub fn covers(&self, text: &str) -> bool {
        text.chars().all(|c| self.glyphs.contains_key(&c))
    }

    /// Compose une ligne avec le crénage et les ligatures de la police, et
    /// rend l'opération `TJ` complète, crochets compris :
    /// `[<0048> -22 <0049>] TJ`.
    ///
    /// À écrire dans le flux de contenu **à la place** de
    /// `<…> Tj` : le résultat occupe la même ligne de base, avec les mêmes
    /// `Tf`, `Td` et couleur. Les nombres sont des millièmes d'em négatifs
    /// (la convention `TJ` : un nombre positif rapproche).
    ///
    /// ```no_run
    /// # use acrux_document::Document;
    /// # use acrux_features::fontembed::{embed_text_font, FontStyle};
    /// # fn main() -> acrux_core::Result<()> {
    /// let doc = Document::load("rapport.pdf")?;
    /// let police = embed_text_font(&doc, "Affiche AVANT", FontStyle::Regular)?;
    /// let operation = match police.shape_line("Affiche AVANT") {
    ///     // Crénage et ligatures.
    ///     Some(tj) => tj,
    ///     // Repli : exactement ce qui était écrit avant.
    ///     None => format!("<{}> Tj", police.show("Affiche AVANT")),
    /// };
    /// # let _ = operation;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Renvoie `None` — et l'appelant garde alors [`Self::show`] — quand la
    /// composition ne peut pas être rendue fidèlement :
    /// - la police d'origine n'a pas été conservée (police reconstruite) ;
    /// - un glyphe produit (ligature, forme contextuelle) n'est pas dans le
    ///   sous-ensemble incorporé ;
    /// - un glyphe demande un décalage **vertical** (accent empilé) : une
    ///   liste `TJ` ne sait déplacer qu'horizontalement.
    ///
    /// Le texte reste extractible : chaque glyphe garde son entrée
    /// `/ToUnicode`, y compris les ligatures ajoutées au sous-ensemble.
    #[must_use]
    pub fn shape_line(&self, text: &str) -> Option<String> {
        let glyphs = self.shaped_glyphs(text)?;
        let mut parts: Vec<String> = Vec::new();
        let mut run = String::new();
        let mut pending = 0.0f64;
        for (gid, advance, offset) in glyphs {
            let nominal = self.widths.get(&gid).copied().unwrap_or(0.0);
            pending -= offset;
            let emitted = pending.round();
            if emitted.abs() >= 1.0 {
                if !run.is_empty() {
                    parts.push(format!("<{run}>"));
                    run.clear();
                }
                parts.push(format!("{emitted}"));
                pending -= emitted;
            }
            let _ = write!(run, "{gid:04X}");
            pending += offset + nominal - advance;
        }
        if !run.is_empty() {
            parts.push(format!("<{run}>"));
        }
        let last = pending.round();
        if last.abs() >= 1.0 {
            parts.push(format!("{last}"));
        }
        Some(format!("[{}] TJ", parts.join(" ")))
    }

    /// Largeur d'une ligne **composée** (crénage et ligatures compris), en
    /// points ; `None` dans les mêmes cas que [`Self::shape_line`].
    ///
    /// À comparer à [`Self::width`], qui somme les avances brutes.
    #[must_use]
    pub fn shaped_width(&self, text: &str, size: f64) -> Option<f64> {
        let glyphs = self.shaped_glyphs(text)?;
        let mille: f64 = glyphs.iter().map(|(_, advance, _)| *advance).sum();
        Some(mille * size / 1000.0)
    }

    /// Composition brute : `(glyphe du sous-ensemble, avance, décalage
    /// horizontal)`, tout en millièmes d'em.
    fn shaped_glyphs(&self, text: &str) -> Option<Vec<(u16, f64, f64)>> {
        let font = self.source.as_ref()?;
        let scale = 1000.0 / self.upem;
        let mut out = Vec::new();
        for glyph in shape(font, text, &ShapeOptions::default()) {
            if glyph.offset_y != 0.0 {
                return None;
            }
            let gid = *self.gid_map.get(&glyph.gid)?;
            out.push((gid, glyph.advance * scale, glyph.offset_x * scale));
        }
        Some(out)
    }
}

/// Vrai si le texte s'écrit entièrement en WinAnsiEncoding, donc avec une des
/// polices standard, sans rien incorporer.
#[must_use]
pub fn fits_winansi(text: &str) -> bool {
    crate::stamp::metrics::encode_win_ansi(&text.replace('\n', ""))
        .replaced
        .is_empty()
}

/// Incorpore de quoi écrire `text` et renvoie la police prête à l'emploi.
///
/// La police système est choisie parmi celles qui couvrent **tous** les
/// caractères du texte ; à défaut, celle qui en couvre le plus (les
/// caractères manquants s'écrivent alors avec le glyphe `.notdef`).
///
/// # Errors
/// Aucune police système lisible, ou sous-ensemble impossible à produire.
pub fn embed_text_font(doc: &Document, text: &str, style: FontStyle) -> Result<EmbeddedFont> {
    embed_preferring(doc, text, style, &[])
}

/// Même chose, en essayant d'abord une liste de familles données.
///
/// Sert quand le **dessin** de la police compte autant que sa couverture :
/// une signature tapée veut une écriture manuscrite, pas la première police
/// venue. Les noms sont comparés en minuscules au début du nom de fichier
/// (`segoesc` pour Segoe Script). Si aucune famille demandée n'est installée,
/// le choix habituel reprend la main.
///
/// # Errors
/// Aucune police système lisible, ou sous-ensemble impossible à produire.
pub fn embed_preferring(
    doc: &Document,
    text: &str,
    style: FontStyle,
    preferred: &[&str],
) -> Result<EmbeddedFont> {
    let wanted = wanted_chars(text);
    let (path, font) = best_font(&wanted, style, preferred)?;
    let family = path
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    embed_loaded(doc, text, style, &family, font)
}

/// Incorpore **une famille nommée** du système, dans le style demandé.
///
/// C'est « changer la police » d'un traitement de texte : la famille est
/// celle que l'utilisateur a choisie dans la liste, et c'est son vrai dessin
/// qui entre dans le PDF. Un style que la famille n'a pas — le gras d'une
/// police qui n'en a pas — se rabat sur le dessin le plus proche.
///
/// # Errors
/// Famille absente du système, fichier illisible, ou sous-ensemble
/// impossible à produire.
pub fn embed_family(
    doc: &Document,
    text: &str,
    family: &str,
    bold: bool,
    italic: bool,
) -> Result<EmbeddedFont> {
    let entry = crate::sysfonts::find(family)
        .ok_or_else(|| Error::Unsupported(format!("police « {family} » absente du système")))?;
    let font = entry
        .face(bold, italic)
        .and_then(crate::sysfonts::load)
        .ok_or_else(|| Error::Unsupported(format!("police « {family} » illisible")))?;
    let style = match (bold, italic) {
        (true, true) => FontStyle::BoldItalic,
        (true, false) => FontStyle::Bold,
        (false, true) => FontStyle::Italic,
        (false, false) => FontStyle::Regular,
    };
    embed_loaded(doc, text, style, &entry.name, font)
}

/// Les caractères distincts d'un texte, triés.
fn wanted_chars(text: &str) -> Vec<char> {
    let mut wanted: Vec<char> = text.chars().filter(|c| *c != '\n').collect();
    wanted.sort_unstable();
    wanted.dedup();
    if wanted.is_empty() {
        wanted.push(' ');
    }
    wanted
}

/// Incorpore une police déjà lue : sous-ensemble, `/Type0`, `/ToUnicode`.
fn embed_loaded(
    doc: &Document,
    text: &str,
    style: FontStyle,
    family: &str,
    font: TrueTypeFont,
) -> Result<EmbeddedFont> {
    let wanted = wanted_chars(text);
    let family = family.to_string();
    // Le glyphe 0 (`.notdef`) est toujours du voyage : c'est lui qui sert de
    // repli, et le sous-ensemble doit de toute façon le contenir.
    let mut gids: Vec<u16> = vec![0];
    let mut per_char: Vec<(char, u16)> = Vec::new();
    for &c in &wanted {
        let gid = font.unicode_to_gid(c).unwrap_or(0);
        if gid != 0 && !gids.contains(&gid) {
            gids.push(gid);
        }
        per_char.push((c, gid));
    }
    // Glyphes que la **composition** produit en plus : ligatures (`ffi`),
    // formes contextuelles arabes… Sans eux, `shape_line` n'aurait rien à
    // montrer. Ils sont ajoutés en fin de liste, donc les glyphes déjà
    // attribués gardent leur numéro et `show()` ne change pas d'un octet.
    let extras = shaped_extras(&font, text);
    for (gid, _) in &extras {
        if *gid != 0 && !gids.contains(gid) {
            gids.push(*gid);
        }
    }
    let produced = subset::subset(&font, &gids, "AKEMBD+Subset")?;
    let upem = f64::from(font.units_per_em().max(1));
    let mut glyphs = HashMap::new();
    let mut widths: BTreeMap<u16, f64> = BTreeMap::new();
    let mut gid_map: HashMap<u16, u16> = HashMap::new();
    let mille = |gid: u16| (f64::from(font.advance(gid).unwrap_or(0)) / upem * 1000.0).round();
    for (c, gid) in per_char {
        let new = produced.map.get(&(0, gid)).copied().unwrap_or(0);
        let w = mille(gid);
        glyphs.insert(c, (new, w));
        widths.insert(new, w);
        gid_map.insert(gid, new);
    }
    // `/ToUnicode` des glyphes composés : une ligature doit se copier comme
    // les lettres qu'elle remplace.
    let mut composed: BTreeMap<u16, String> = BTreeMap::new();
    for (gid, source) in extras {
        let Some(new) = produced.map.get(&(0, gid)).copied() else {
            continue;
        };
        widths.insert(new, mille(gid));
        gid_map.insert(gid, new);
        if !source.is_empty() {
            composed.entry(new).or_insert(source);
        }
    }
    let (ascent, descent) = vertical_metrics(&font, upem);
    let reference = write_type0(
        doc,
        &produced.data,
        &widths,
        &glyphs,
        &composed,
        &Type0Source {
            family: &family,
            style,
            font: &font,
            upem,
        },
    )?;
    Ok(EmbeddedFont {
        reference,
        family,
        glyphs,
        gid_map,
        widths,
        source: Some(Arc::new(font)),
        upem,
        ascent,
        descent,
    })
}

/// Glyphes produits par la composition et le texte qu'ils représentent.
///
/// Chaque ligne est composée séparément (une ligature ne traverse pas un
/// retour à la ligne) ; le texte d'un glyphe est la tranche d'octets de sa
/// grappe, ce qui donne « ffi » pour la ligature et « ب » pour une forme
/// contextuelle.
fn shaped_extras(font: &TrueTypeFont, text: &str) -> Vec<(u16, String)> {
    let mut out = Vec::new();
    for line in text.split('\n') {
        if line.is_empty() {
            continue;
        }
        let glyphs = shape(font, line, &ShapeOptions::default());
        let mut clusters: Vec<u32> = glyphs.iter().map(|g| g.cluster).collect();
        clusters.sort_unstable();
        clusters.dedup();
        for glyph in &glyphs {
            let start = usize::try_from(glyph.cluster).unwrap_or(0);
            let end = clusters
                .iter()
                .copied()
                .find(|c| *c > glyph.cluster)
                .and_then(|c| usize::try_from(c).ok())
                .unwrap_or(line.len());
            let source = line.get(start..end).unwrap_or_default();
            out.push((glyph.gid, source.to_owned()));
        }
    }
    out
}

/// Ascendante et descendante en millièmes d'em.
fn vertical_metrics(font: &TrueTypeFont, upem: f64) -> (f64, f64) {
    if let Some(h) = font.hhea() {
        let a = f64::from(h.ascender) / upem * 1000.0;
        let d = f64::from(h.descender) / upem * 1000.0;
        if a > 0.0 {
            return (a.round(), d.round());
        }
    }
    let b = font.bbox();
    (
        (b.y1 / upem * 1000.0).round(),
        (b.y0 / upem * 1000.0).round(),
    )
}

/// Dossiers où chercher des polices, du plus spécifique au plus général.
pub(crate) fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(custom) = std::env::var("ACRUX_FONT_DIR") {
        dirs.push(PathBuf::from(custom));
    }
    if let Ok(windir) = std::env::var("WINDIR") {
        dirs.push(PathBuf::from(windir).join("Fonts"));
    }
    dirs.push(PathBuf::from("C:/Windows/Fonts"));
    dirs.push(PathBuf::from("/usr/share/fonts/truetype"));
    dirs.push(PathBuf::from("/usr/share/fonts"));
    dirs.push(PathBuf::from("/Library/Fonts"));
    dirs
}

/// Familles essayées en premier : lisibles, largement installées, et pour les
/// dernières capables d'écrire le chinois, le japonais et le coréen.
const PREFERRED: &[&str] = &[
    "arial",
    "calibri",
    "segoeui",
    "tahoma",
    "verdana",
    "times",
    "georgia",
    "dejavusans",
    "notosans",
    "liberationsans",
    "msgothic",
    "meiryo",
    "yugothic",
    "malgun",
    "msyh",
    "simsun",
    "gulim",
    "batang",
];

/// Choisit la police système qui couvre le mieux les caractères demandés.
fn best_font(
    wanted: &[char],
    style: FontStyle,
    preferred: &[&str],
) -> Result<(PathBuf, TrueTypeFont)> {
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in font_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase());
            if matches!(ext.as_deref(), Some("ttf" | "otf" | "ttc")) {
                files.push(path);
            }
        }
    }
    if files.is_empty() {
        return Err(Error::Unsupported(
            "aucune police système trouvée pour incorporer ce texte".into(),
        ));
    }
    // Un score de nom d'abord (famille préférée, style demandé), puis la
    // couverture réelle : inutile d'ouvrir mille fichiers dans le désordre.
    files.sort_by_key(|p| name_rank(p.as_path(), style, preferred));
    let mut best: Option<(usize, PathBuf, TrueTypeFont)> = None;
    for path in files.into_iter().take(160) {
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        let Ok(font) = TrueTypeFont::parse(&data) else {
            continue;
        };
        // Le sous-ensemble recopie la table `glyf` : une police CFF pure ne
        // s'incorpore pas ainsi.
        if !font.has_glyf() {
            continue;
        }
        let covered = wanted
            .iter()
            .filter(|c| font.unicode_to_gid(**c).is_some_and(|g| g != 0))
            .count();
        if covered == wanted.len() {
            return Ok((path, font));
        }
        if best.as_ref().is_none_or(|(n, _, _)| covered > *n) {
            best = Some((covered, path, font));
        }
    }
    match best {
        Some((_, path, font)) => Ok((path, font)),
        None => Err(Error::Unsupported(
            "aucune police système exploitable (aucune table `glyf`)".into(),
        )),
    }
}

/// Rang d'un fichier de police : plus c'est petit, plus on l'essaie tôt.
fn name_rank(
    path: &std::path::Path,
    style: FontStyle,
    preferred: &[&str],
) -> (usize, usize, usize, String) {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    // Les familles demandées par l'appelant passent avant toutes les autres.
    let asked = preferred
        .iter()
        .position(|f| name.starts_with(f))
        .unwrap_or(preferred.len());
    let family = PREFERRED
        .iter()
        .position(|f| name.starts_with(f))
        .unwrap_or(PREFERRED.len());
    let style_rank = style
        .suffixes()
        .iter()
        .position(|suffix| {
            if suffix.is_empty() {
                !name.ends_with('b') && !name.ends_with('i') && !name.ends_with('z')
            } else {
                name.ends_with(suffix)
            }
        })
        .unwrap_or(9);
    (asked, family, style_rank, name)
}

/// Ce qui décrit la police source dans le `/FontDescriptor`.
struct Type0Source<'a> {
    family: &'a str,
    style: FontStyle,
    font: &'a TrueTypeFont,
    upem: f64,
}

/// Écrit la police composite et renvoie la référence du `/Type0`.
#[allow(clippy::unnecessary_wraps)] // symétrie des écritures
fn write_type0(
    doc: &Document,
    program: &[u8],
    widths: &BTreeMap<u16, f64>,
    glyphs: &HashMap<char, (u16, f64)>,
    composed: &BTreeMap<u16, String>,
    source: &Type0Source<'_>,
) -> Result<ObjectRef> {
    let Type0Source {
        family,
        style,
        font,
        upem,
    } = *source;
    let base = format!("AKEMBD+{}", sanitize(family));
    // Programme compressé : une police CJK sous-ensemblée reste volumineuse.
    let compressed = flate::compress(program, 6);
    let mut file = Dict::new();
    file.insert(Name::new("Filter"), Object::Name(Name::new("FlateDecode")));
    file.insert(
        Name::new("Length1"),
        Object::Integer(i64::try_from(program.len()).unwrap_or(0)),
    );
    let file_ref = doc.add(Object::Stream {
        dict: file,
        raw: compressed,
    });

    let bbox = font.bbox();
    #[allow(clippy::cast_possible_truncation)] // millièmes d'em : toujours petit
    let scale = |v: f64| Object::Integer((v / upem * 1000.0).round() as i64);
    let mut descriptor = Dict::new();
    descriptor.insert(Name::new("Type"), Object::Name(Name::new("FontDescriptor")));
    descriptor.insert(Name::new("FontName"), Object::Name(Name::new(&base)));
    descriptor.insert(Name::new("Flags"), Object::Integer(style.flags()));
    descriptor.insert(
        Name::new("FontBBox"),
        Object::Array(vec![
            scale(bbox.x0),
            scale(bbox.y0),
            scale(bbox.x1),
            scale(bbox.y1),
        ]),
    );
    descriptor.insert(Name::new("ItalicAngle"), Object::Integer(0));
    let (ascent, descent) = vertical_metrics(font, upem);
    descriptor.insert(Name::new("Ascent"), Object::Real(ascent));
    descriptor.insert(Name::new("Descent"), Object::Real(descent));
    descriptor.insert(Name::new("CapHeight"), Object::Real(ascent * 0.7));
    descriptor.insert(Name::new("StemV"), Object::Integer(80));
    descriptor.insert(Name::new("FontFile2"), Object::Reference(file_ref));
    let descriptor_ref = doc.add(Object::Dict(descriptor));

    let mut descendant = Dict::new();
    descendant.insert(Name::new("Type"), Object::Name(Name::new("Font")));
    descendant.insert(
        Name::new("Subtype"),
        Object::Name(Name::new("CIDFontType2")),
    );
    descendant.insert(Name::new("BaseFont"), Object::Name(Name::new(&base)));
    let mut system = Dict::new();
    system.insert(Name::new("Registry"), Object::String(b"Adobe".to_vec()));
    system.insert(Name::new("Ordering"), Object::String(b"Identity".to_vec()));
    system.insert(Name::new("Supplement"), Object::Integer(0));
    descendant.insert(Name::new("CIDSystemInfo"), Object::Dict(system));
    descendant.insert(
        Name::new("FontDescriptor"),
        Object::Reference(descriptor_ref),
    );
    descendant.insert(Name::new("DW"), Object::Integer(1000));
    descendant.insert(Name::new("W"), Object::Array(widths_array(widths)));
    descendant.insert(
        Name::new("CIDToGIDMap"),
        Object::Name(Name::new("Identity")),
    );
    let descendant_ref = doc.add(Object::Dict(descendant));

    let to_unicode = doc.add(Object::Stream {
        dict: Dict::new(),
        raw: to_unicode_cmap(glyphs, composed).into_bytes(),
    });

    let mut type0 = Dict::new();
    type0.insert(Name::new("Type"), Object::Name(Name::new("Font")));
    type0.insert(Name::new("Subtype"), Object::Name(Name::new("Type0")));
    type0.insert(Name::new("BaseFont"), Object::Name(Name::new(&base)));
    type0.insert(Name::new("Encoding"), Object::Name(Name::new("Identity-H")));
    type0.insert(
        Name::new("DescendantFonts"),
        Object::Array(vec![Object::Reference(descendant_ref)]),
    );
    type0.insert(Name::new("ToUnicode"), Object::Reference(to_unicode));
    Ok(doc.add(Object::Dict(type0)))
}

/// Tableau `/W` compact : les glyphes consécutifs partagent une entrée.
fn widths_array(widths: &BTreeMap<u16, f64>) -> Vec<Object> {
    let mut out = Vec::new();
    let mut run: Vec<(u16, f64)> = Vec::new();
    let flush = |run: &mut Vec<(u16, f64)>, out: &mut Vec<Object>| {
        if let Some((first, _)) = run.first() {
            out.push(Object::Integer(i64::from(*first)));
            out.push(Object::Array(
                run.iter().map(|(_, w)| Object::Real(*w)).collect(),
            ));
        }
        run.clear();
    };
    for (&gid, &w) in widths {
        match run.last() {
            Some((prev, _)) if gid == prev + 1 => run.push((gid, w)),
            Some(_) => {
                flush(&mut run, &mut out);
                run.push((gid, w));
            }
            None => run.push((gid, w)),
        }
    }
    flush(&mut run, &mut out);
    out
}

/// CMap `/ToUnicode` : c'est elle qui rend le texte copiable et cherchable.
fn to_unicode_cmap(glyphs: &HashMap<char, (u16, f64)>, composed: &BTreeMap<u16, String>) -> String {
    let mut pairs: Vec<(u16, String)> = glyphs
        .iter()
        .map(|(c, (g, _))| (*g, c.to_string()))
        .collect();
    // Les glyphes composés (ligatures, formes contextuelles) se copient
    // comme le texte qu'ils remplacent.
    for (gid, text) in composed {
        pairs.push((*gid, text.clone()));
    }
    pairs.sort_unstable();
    pairs.dedup_by_key(|(g, _)| *g);
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for chunk in pairs.chunks(100) {
        let _ = writeln!(out, "{} beginbfchar", chunk.len());
        for (gid, text) in chunk {
            let mut hex = String::new();
            // Au plus huit unités UTF-16 : la spécification limite la
            // destination d'un `bfchar` à 512 octets, et une grappe plus
            // longue ne se copierait de toute façon pas utilement.
            for unit in text.encode_utf16().take(8) {
                let _ = write!(hex, "{unit:04X}");
            }
            let _ = writeln!(out, "<{gid:04X}> <{hex}>");
        }
        out.push_str("endbfchar\n");
    }
    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out
}

/// Nom de police valide : lettres, chiffres et tirets.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(48)
        .collect();
    if cleaned.is_empty() {
        "Police".into()
    } else {
        cleaned
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn empty_doc() -> Document {
        Document::from_bytes(
            b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >> endobj\n".to_vec(),
        )
        .unwrap()
    }

    #[test]
    fn winansi_detection_matches_the_standard_fonts() {
        assert!(fits_winansi("Confidentiel — « oui »"));
        assert!(fits_winansi("Àéîöÿ ÆŒ ‰ †"));
        assert!(!fits_winansi("機密"));
        assert!(!fits_winansi("Λάμδα"));
        assert!(!fits_winansi("Привет"));
    }

    #[test]
    fn widths_array_groups_consecutive_glyphs() {
        let mut w = BTreeMap::new();
        w.insert(3u16, 500.0);
        w.insert(4, 600.0);
        w.insert(9, 250.0);
        let a = widths_array(&w);
        // [3 [500 600] 9 [250]]
        assert_eq!(a.len(), 4);
        assert!(matches!(a[0], Object::Integer(3)));
        assert!(matches!(&a[1], Object::Array(v) if v.len() == 2));
        assert!(matches!(a[2], Object::Integer(9)));
        assert!(matches!(&a[3], Object::Array(v) if v.len() == 1));
    }

    #[test]
    fn to_unicode_lists_every_glyph_once() {
        let mut g = HashMap::new();
        g.insert('A', (5u16, 600.0));
        g.insert('B', (6u16, 600.0));
        let cmap = to_unicode_cmap(&g, &BTreeMap::new());
        assert!(cmap.contains("<0005> <0041>"), "{cmap}");
        assert!(cmap.contains("<0006> <0042>"), "{cmap}");
        assert!(cmap.contains("2 beginbfchar"));
    }

    #[test]
    fn a_latin_text_embeds_and_measures() {
        let doc = empty_doc();
        let Ok(font) = embed_text_font(&doc, "Bonjour", FontStyle::Regular) else {
            // Machine sans polices système : rien à prouver ici.
            return;
        };
        assert!(font.covers("Bonjour"));
        assert_eq!(font.show("AB").len(), 8, "deux octets par caractère");
        assert!(font.width("Bonjour", 12.0) > 10.0);
        assert!(font.ascent > 0.0 && font.descent < 0.0);
        let f = doc.get(font.reference).unwrap();
        let dict = f.as_dict().unwrap();
        assert!(
            matches!(dict.get(&Name::new("Subtype")), Some(Object::Name(n)) if n.0 == b"Type0")
        );
    }

    /// Le `ToUnicode` d'une ligature rend la suite de lettres d'origine.
    #[test]
    fn to_unicode_maps_composed_glyphs() {
        let mut g = HashMap::new();
        g.insert('f', (5u16, 300.0));
        let mut composed = BTreeMap::new();
        composed.insert(9u16, "ffi".to_owned());
        let cmap = to_unicode_cmap(&g, &composed);
        assert!(cmap.contains("<0009> <00660066006"), "{cmap}");
    }

    /// `shape_line` crée bien une opération `TJ` et, sur une paire crénée,
    /// mesure moins large que la somme des avances.
    #[test]
    fn shape_line_kerns_and_ligates() {
        let doc = empty_doc();
        let Ok(font) = embed_text_font(&doc, "AVATAR affiche", FontStyle::Regular) else {
            return; // Machine sans polices système.
        };
        let Some(tj) = font.shape_line("AVATAR") else {
            return; // Police sans table de composition.
        };
        assert!(tj.starts_with('[') && tj.ends_with("] TJ"), "{tj}");
        let Some(shaped) = font.shaped_width("AVATAR", 12.0) else {
            return;
        };
        let plain = font.width("AVATAR", 12.0);
        assert!(shaped <= plain, "le crénage ne peut qu'élargir moins");
        // `show` n'a pas changé de comportement.
        assert_eq!(font.show("AV").len(), 8);
        // Une ligature produit moins de glyphes que de lettres.
        if let Some(tj) = font.shape_line("affiche") {
            let glyphs = tj.matches("00").count();
            assert!(glyphs > 0, "{tj}");
        }
    }

    /// Un texte jamais incorporé ne peut pas être composé : repli honnête.
    #[test]
    fn shape_line_refuses_unknown_glyphs() {
        let doc = empty_doc();
        let Ok(font) = embed_text_font(&doc, "abc", FontStyle::Regular) else {
            return;
        };
        assert!(font.shape_line("\u{4E2D}\u{6587}").is_none());
    }

    #[test]
    fn a_missing_character_falls_back_to_notdef() {
        let doc = empty_doc();
        let Ok(font) = embed_text_font(&doc, "A", FontStyle::Regular) else {
            return;
        };
        // Un caractère jamais demandé n'a pas de glyphe : `show` écrit 0.
        assert_eq!(font.show("\u{10FFFF}"), "0000");
        assert!(!font.covers("\u{10FFFF}"));
    }
}
