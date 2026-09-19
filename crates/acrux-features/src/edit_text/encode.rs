//! Réencodage d'un texte avec la police en place, et complément de police
//! quand un caractère manque (ISO 32000-2 §9.6, §9.7).
//!
//! Trois chemins, dans l'ordre :
//!
//! 1. **Le caractère existe déjà dans la police** : on retrouve son code par
//!    la table `/ToUnicode` inversée, par l'encodage d'une police simple, ou
//!    par la `cmap` du programme incorporé. Chaque code trouvé est vérifié en
//!    le redécodant avec la police : aucune supposition n'est faite sur la
//!    structure de la CMap.
//! 2. **Le caractère manque à un sous-ensemble incorporé** (police composite
//!    `/Identity` + `CIDFontType2` + `FontFile2`) : on cherche la police
//!    système de la même famille (`C:/Windows/Fonts`, ou `ACRUX_FONT_DIR`), on
//!    fusionne les glyphes d'origine et les glyphes manquants avec
//!    [`acrux_fonts::subset`], puis on ajoute les codes, les largeurs `/W` et les
//!    entrées `/ToUnicode` correspondantes. Les glyphes déjà incorporés
//!    gardent leur indice : le reste de la page s'affiche à l'identique.
//! 3. **Sinon** : le texte est dessiné avec la police standard la plus proche
//!    (§9.6.2.2), ajoutée aux ressources de la page, et un avertissement est
//!    retourné.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use acrux_core::{Error, Result};
use acrux_document::{Dict, Document, Name, Object, Page};
use acrux_fonts::encodings::glyph_name_to_unicode;
use acrux_fonts::{subset, TrueTypeFont};
use acrux_render::font::{FontKind, LoadedFont, Program};

use super::StyleOverride;

/// Nombre maximal de caractères ajoutés à une police en une édition.
const MAX_NEW_GLYPHS: usize = 512;

/// Texte encodé.
#[derive(Debug, Clone, Default)]
pub(crate) struct Encoded {
    /// Chaîne encodée (écrite en hexadécimal dans le flux).
    pub bytes: Vec<u8>,
    /// Largeurs d'avance (espace texte, taille 1) et indicateur d'espace.
    pub widths: Vec<(f64, bool)>,
    /// Caractères sans glyphe, ignorés.
    pub lost: Vec<char>,
}

/// Police prête à écrire un texte : ressource à utiliser, police chargée et
/// table caractère → code.
pub(crate) struct Prepared {
    /// Nom de ressource à écrire dans `Tf` (celui d'origine, ou celui d'une
    /// police ajoutée).
    pub resource: Name,
    /// Police correspondante.
    pub font: LoadedFont,
    /// Avertissements de préparation.
    pub warnings: Vec<String>,
    table: HashMap<char, Vec<u8>>,
}

impl Prepared {
    /// Encode un texte (les caractères sans glyphe sont signalés dans
    /// [`Encoded::lost`]).
    pub fn encode(&self, text: &str) -> Encoded {
        let mut out = Encoded::default();
        for c in text.chars() {
            match self.table.get(&c) {
                Some(code) => {
                    out.widths.push(measure(&self.font, code));
                    out.bytes.extend_from_slice(code);
                }
                None => out.lost.push(c),
            }
        }
        out
    }

    /// Largeur d'un texte en espace texte pour une taille de 1.
    pub fn width(&self, text: &str) -> f64 {
        self.encode(text).widths.iter().map(|(w, _)| w).sum()
    }
}

/// Prépare l'écriture de `text` avec la police `name` de la page.
///
/// `style` peut imposer une autre famille (nom, gras, italique) : on passe
/// alors par une police standard ajoutée aux ressources de la page.
///
/// # Errors
/// Ressources illisibles, ou page non indirecte (ajout d'une police).
pub(crate) fn prepare(
    doc: &Document,
    page: &Page,
    name: &Name,
    text: &str,
    style: Option<&StyleOverride>,
) -> Result<Prepared> {
    let dict = super::font_dict_of(doc, page, name)?;
    let font = LoadedFont::load(doc, &dict)?;
    if let Some(s) = style {
        if s.font.is_some() || s.bold == Some(true) || s.italic == Some(true) {
            let family = s.font.clone().unwrap_or_else(|| font.base_font.clone());
            return standard(
                doc,
                page,
                &family,
                s.bold.unwrap_or(false),
                s.italic.unwrap_or(false),
            );
        }
    }
    let table = reverse_table(&font);
    let missing: Vec<char> = {
        let mut m: Vec<char> = Vec::new();
        for c in text.chars() {
            if !table.contains_key(&c) && !m.contains(&c) {
                m.push(c);
            }
        }
        m
    };
    if missing.is_empty() {
        return Ok(Prepared {
            resource: name.clone(),
            font,
            warnings: Vec::new(),
            table,
        });
    }
    match extend_font(doc, &dict, &font, &missing) {
        Ok(()) => {
            let font = LoadedFont::load(doc, &super::font_dict_of(doc, page, name)?)?;
            let table = reverse_table(&font);
            let still: String = missing.iter().filter(|c| !table.contains_key(c)).collect();
            let mut warnings = Vec::new();
            if !still.is_empty() {
                warnings.push(format!("caractères sans glyphe, ignorés : {still}"));
            }
            Ok(Prepared {
                resource: name.clone(),
                font,
                warnings,
                table,
            })
        }
        Err(why) => {
            let mut prepared = standard(doc, page, &font.base_font, false, false)?;
            prepared.warnings.insert(
                0,
                format!(
                    "police « {}» : caractères manquants ({}) — {why}",
                    font.base_font,
                    missing.iter().collect::<String>()
                ),
            );
            Ok(prepared)
        }
    }
}

/// Largeur d'avance et caractère d'espacement d'un code encodé.
fn measure(font: &LoadedFont, code: &[u8]) -> (f64, bool) {
    font.decode(code)
        .first()
        .map_or((0.0, false), |g| (g.width, g.is_space))
}

/// Table caractère → octets, construite et **vérifiée** avec la police.
fn reverse_table(font: &LoadedFont) -> HashMap<char, Vec<u8>> {
    let mut out: HashMap<char, Vec<u8>> = HashMap::new();
    if font.is_composite() {
        if let Some(tu) = &font.to_unicode {
            for (code, s) in tu.to_unicode_pairs() {
                let mut chars = s.chars();
                let (Some(c), None) = (chars.next(), chars.next()) else {
                    continue; // ligature : pas de caractère unique
                };
                if let Some(bytes) = encode_code(font, code) {
                    out.entry(c).or_insert(bytes);
                }
            }
        }
        // Complément par la cmap du programme (sous-ensemble sans ToUnicode).
        if let Program::TrueType(tt) = &font.program {
            for code in 0x20..0x3000u32 {
                let Some(c) = char::from_u32(code) else {
                    continue;
                };
                if out.contains_key(&c) {
                    continue;
                }
                let Some(gid) = tt.unicode_to_gid(c) else {
                    continue;
                };
                let Some(bytes) = encode_code(font, u32::from(gid)) else {
                    continue;
                };
                // Le code ne convient que s'il désigne bien ce glyphe.
                let cid = font.decode(&bytes).first().map(|g| g.cid);
                if cid.and_then(|cid| font.gid_for(cid)) == Some(u32::from(gid)) {
                    out.insert(c, bytes);
                }
            }
        }
        return out;
    }
    for code in 0..=255u32 {
        let bytes = vec![u8::try_from(code).unwrap_or(0)];
        let Some(glyph) = font.decode(&bytes).into_iter().next() else {
            continue;
        };
        // Un code n'est utilisable que si la police sait dessiner son glyphe.
        if !matches!(font.program, Program::None) && font.gid_for(glyph.cid).is_none() {
            continue;
        }
        let text = font
            .glyph_name(code)
            .and_then(glyph_name_to_unicode)
            .map(|c| c.to_string())
            .or_else(|| font.to_unicode(&glyph));
        let Some(text) = text else { continue };
        let mut chars = text.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            out.entry(c).or_insert(bytes);
        }
    }
    out
}

/// Octets d'un code, vérifiés en les redécodant : la longueur est celle
/// qu'impose la CMap de la police (1 à 4 octets).
fn encode_code(font: &LoadedFont, code: u32) -> Option<Vec<u8>> {
    for len in [2usize, 1, 3, 4] {
        if len < 4 && code >= 1 << (8 * len) {
            continue;
        }
        let bytes: Vec<u8> = code.to_be_bytes()[4 - len..].to_vec();
        let decoded = font.decode_spans(&bytes);
        if decoded.len() == 1 && decoded[0].1 == len && decoded[0].0.code == code {
            return Some(bytes);
        }
    }
    None
}

/// Complète une police incorporée avec les glyphes manquants pris dans la
/// police système de la même famille.
fn extend_font(
    doc: &Document,
    dict: &Dict,
    font: &LoadedFont,
    missing: &[char],
) -> std::result::Result<(), String> {
    if missing.len() > MAX_NEW_GLYPHS {
        return Err("trop de caractères à ajouter".into());
    }
    if font.kind != FontKind::Type0 {
        return Err("seules les polices composites sont complétées".into());
    }
    if font.substituted {
        return Err("police non incorporée".into());
    }
    let Program::TrueType(embedded) = &font.program else {
        return Err("programme incorporé absent ou non TrueType".into());
    };
    if !embedded.has_glyf() {
        return Err("programme incorporé sans table glyf".into());
    }
    let system = find_system_font(&font.base_font, missing)
        .ok_or_else(|| format!("aucune police système pour « {} »", font.base_font))?;
    if system.units_per_em() != embedded.units_per_em() {
        return Err("la police système n'a pas la même échelle".into());
    }
    // Liste fusionnée : tous les glyphes d'origine à leur place, puis les
    // nouveaux, pour que les codes existants désignent toujours le même
    // glyphe (`/CIDToGIDMap /Identity`).
    let old_count = embedded.glyph_count();
    let mut glyphs: Vec<(usize, u16)> = (0..old_count).map(|g| (0, g)).collect();
    let mut codes: Vec<(u32, char)> = Vec::new();
    let mut widths: Vec<(u32, f64)> = Vec::new();
    let scale = 1000.0 / f64::from(system.units_per_em().max(1));
    for c in missing {
        let Some(gid) = system.unicode_to_gid(*c) else {
            continue;
        };
        let Ok(new) = u16::try_from(glyphs.len()) else {
            break;
        };
        glyphs.push((1, gid));
        codes.push((u32::from(new), *c));
        widths.push((
            u32::from(new),
            f64::from(system.advance(gid).unwrap_or(0)) * scale,
        ));
    }
    if codes.is_empty() {
        return Err("la police système n'a pas non plus ces caractères".into());
    }
    let base = strip_subset_prefix(&font.base_font);
    let produced = subset::subset_merged(&[embedded, &system], &glyphs, &format!("AKSUBS+{base}"))
        .map_err(|e| e.to_string())?;
    write_font(doc, dict, font, &produced.data, &widths, &codes).map_err(|e| e.to_string())
}

/// Écrit la police complétée : `FontFile2`, `/W`, `/ToUnicode`.
fn write_font(
    doc: &Document,
    dict: &Dict,
    font: &LoadedFont,
    program: &[u8],
    widths: &[(u32, f64)],
    codes: &[(u32, char)],
) -> Result<()> {
    let Some(Object::Reference(descendant_ref)) = doc
        .dict_get(dict, "DescendantFonts")?
        .and_then(|d| d.as_array().and_then(|a| a.first().cloned()))
    else {
        return Err(Error::Corrupt(
            "police composite sans police descendante indirecte".into(),
        ));
    };
    let mut descendant = doc
        .get(descendant_ref)?
        .as_dict()
        .cloned()
        .ok_or_else(|| Error::Corrupt("police descendante illisible".into()))?;
    // Seule l'identité garantit que le code désigne l'indice de glyphe.
    match doc.dict_get(&descendant, "CIDToGIDMap")? {
        Some(m) if !matches!(&*m, Object::Name(n) if n.0 == b"Identity") => {
            return Err(Error::Unsupported(
                "/CIDToGIDMap explicite : complément de police non pris en charge".into(),
            ))
        }
        _ => {}
    }
    let Some(Object::Reference(descriptor_ref)) =
        descendant.get(&Name::new("FontDescriptor")).cloned()
    else {
        return Err(Error::Corrupt(
            "descripteur de police absent ou direct".into(),
        ));
    };
    let descriptor = doc
        .get(descriptor_ref)?
        .as_dict()
        .cloned()
        .ok_or_else(|| Error::Corrupt("descripteur illisible".into()))?;
    let Some(Object::Reference(file_ref)) = descriptor.get(&Name::new("FontFile2")).cloned() else {
        return Err(Error::Unsupported(
            "programme de police non incorporé comme objet indirect".into(),
        ));
    };
    // Nouveau programme, non compressé : relu tel quel par tous les moteurs.
    let mut stream = Dict::new();
    let length = i64::try_from(program.len()).unwrap_or(0);
    stream.insert(Name::new("Length1"), Object::Integer(length));
    stream.insert(Name::new("Length"), Object::Integer(length));
    doc.set(
        file_ref,
        Object::Stream {
            dict: stream,
            raw: program.to_vec(),
        },
    );
    // Largeurs des nouveaux CID (§9.7.4.3).
    let mut w: Vec<Object> = doc
        .dict_get(&descendant, "W")?
        .and_then(|o| o.as_array().map(<[Object]>::to_vec))
        .unwrap_or_default();
    for (cid, advance) in widths {
        w.push(Object::Integer(i64::from(*cid)));
        w.push(Object::Array(vec![Object::Real(
            (advance * 1000.0).round() / 1000.0,
        )]));
    }
    descendant.insert(Name::new("W"), Object::Array(w));
    doc.set(descendant_ref, Object::Dict(descendant));
    doc.set(descriptor_ref, Object::Dict(descriptor));
    // ToUnicode : régénéré avec les nouvelles correspondances (§9.10.3).
    if let Some(Object::Reference(tu_ref)) = dict.get(&Name::new("ToUnicode")).cloned() {
        let mut pairs: Vec<(u32, String)> = font
            .to_unicode
            .as_ref()
            .map(acrux_fonts::cmap::CMap::to_unicode_pairs)
            .unwrap_or_default();
        for (code, c) in codes {
            pairs.push((*code, c.to_string()));
        }
        pairs.sort_by_key(|(c, _)| *c);
        pairs.dedup_by_key(|(c, _)| *c);
        let data = to_unicode_cmap(&pairs);
        let mut d = Dict::new();
        d.insert(
            Name::new("Length"),
            Object::Integer(i64::try_from(data.len()).unwrap_or(0)),
        );
        doc.set(tu_ref, Object::Stream { dict: d, raw: data });
    }
    Ok(())
}

/// Écrit un CMap `ToUnicode` complet (§9.10.3).
fn to_unicode_cmap(pairs: &[(u32, String)]) -> Vec<u8> {
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo\n<< /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for chunk in pairs.chunks(100) {
        let _ = writeln!(out, "{} beginbfchar", chunk.len());
        for (code, text) in chunk {
            let dst = text.encode_utf16().fold(String::new(), |mut acc, u| {
                let _ = write!(acc, "{u:04X}");
                acc
            });
            let _ = writeln!(out, "<{code:04X}> <{dst}>");
        }
        out.push_str("endbfchar\n");
    }
    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out.into_bytes()
}

/// Nom de police sans le préfixe de sous-ensemble `ABCDEF+`.
pub(crate) fn strip_subset_prefix(name: &str) -> &str {
    match name.split_once('+') {
        Some((p, rest)) if p.len() == 6 && p.bytes().all(|b| b.is_ascii_uppercase()) => rest,
        _ => name,
    }
}

/// Répertoires de polices système.
fn font_dirs() -> Vec<PathBuf> {
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

/// Cherche la police système de la même famille contenant les caractères
/// demandés : fichiers dont le nom commence par la famille, classés par
/// concordance de style (`georgiab.ttf` pour Georgia-Bold…).
fn find_system_font(base_font: &str, missing: &[char]) -> Option<TrueTypeFont> {
    let base = strip_subset_prefix(base_font);
    let family: String = base
        .split(['-', ','])
        .next()
        .unwrap_or(base)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect();
    if family.len() < 3 {
        return None;
    }
    let lower = base.to_ascii_lowercase();
    let bold = ["bold", "black", "heavy", "semibold"]
        .iter()
        .any(|k| lower.contains(k));
    let italic = lower.contains("italic") || lower.contains("oblique");
    let suffixes: &[&str] = match (bold, italic) {
        (true, true) => &["bi", "z", "bolditalic"],
        (true, false) => &["b", "bd", "bold"],
        (false, true) => &["i", "italic"],
        (false, false) => &["", "regular"],
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    for dir in font_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            let ok = path
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case("ttf"));
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if ok && stem.starts_with(&family) {
                candidates.push(path);
            }
        }
        if !candidates.is_empty() {
            break;
        }
    }
    candidates.sort_by_key(|path| {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let rest = stem
            .strip_prefix(&family)
            .unwrap_or(stem.as_str())
            .to_string();
        suffixes
            .iter()
            .position(|s| *s == rest)
            .unwrap_or(suffixes.len() + rest.len())
    });
    for path in candidates {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(font) = TrueTypeFont::parse(&bytes) else {
            continue;
        };
        if font.has_glyf() && missing.iter().any(|c| font.unicode_to_gid(*c).is_some()) {
            return Some(font);
        }
    }
    None
}

/// Police standard la plus proche (§9.6.2.2), ajoutée aux ressources.
/// Ajoute (ou retrouve) une des quatorze polices standard dans les
/// ressources de la page, et rend son nom de ressource.
///
/// # Errors
/// Page non indirecte.
pub(crate) fn standard_font(
    doc: &Document,
    page: &Page,
    family: &str,
    bold: bool,
    italic: bool,
) -> Result<Name> {
    standard(doc, page, family, bold, italic).map(|p| p.resource)
}

fn standard(
    doc: &Document,
    page: &Page,
    family: &str,
    bold: bool,
    italic: bool,
) -> Result<Prepared> {
    let lower = strip_subset_prefix(family).to_ascii_lowercase();
    let bold = bold || ["bold", "black", "heavy"].iter().any(|k| lower.contains(k));
    let italic = italic || lower.contains("italic") || lower.contains("oblique");
    let base = if lower.contains("courier") || lower.contains("mono") {
        match (bold, italic) {
            (true, true) => "Courier-BoldOblique",
            (true, false) => "Courier-Bold",
            (false, true) => "Courier-Oblique",
            (false, false) => "Courier",
        }
    } else if lower.contains("times")
        || lower.contains("georgia")
        || lower.contains("roman")
        || lower.contains("garamond")
        || lower.contains("book")
        || lower.contains("serif") && !lower.contains("sans")
    {
        match (bold, italic) {
            (true, true) => "Times-BoldItalic",
            (true, false) => "Times-Bold",
            (false, true) => "Times-Italic",
            (false, false) => "Times-Roman",
        }
    } else {
        match (bold, italic) {
            (true, true) => "Helvetica-BoldOblique",
            (true, false) => "Helvetica-Bold",
            (false, true) => "Helvetica-Oblique",
            (false, false) => "Helvetica",
        }
    };
    let mut dict = Dict::new();
    dict.insert(Name::new("Type"), Object::Name(Name::new("Font")));
    dict.insert(Name::new("Subtype"), Object::Name(Name::new("Type1")));
    dict.insert(Name::new("BaseFont"), Object::Name(Name::new(base)));
    dict.insert(
        Name::new("Encoding"),
        Object::Name(Name::new("WinAnsiEncoding")),
    );
    let resource = super::add_font_resource(doc, page, &dict)?;
    let font = LoadedFont::load(doc, &dict)?;
    let table = reverse_table(&font);
    Ok(Prepared {
        resource,
        font,
        warnings: vec![format!("texte dessiné avec la police standard {base}")],
        table,
    })
}
