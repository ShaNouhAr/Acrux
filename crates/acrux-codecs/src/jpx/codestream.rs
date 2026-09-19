//! Syntaxe du flux de code (ISO/IEC 15444-1 annexe A) : marqueurs et
//! segments de l'en-tête principal et des en-têtes de tuile-partie, puis
//! rassemblement des tuiles-parties par tuile.
//!
//! Segments interprétés : SIZ (A.5.1), COD (A.6.1), COC (A.6.2), RGN
//! (A.6.3), QCD (A.6.4), QCC (A.6.5), POC (A.6.6), PPM (A.7.4), PPT
//! (A.7.5), SOT (A.4.2), SOD (A.4.3), EOC (A.4.4). Ignorés : TLM, PLM, PLT
//! (index de longueurs, A.7), CRG (A.9.1), COM (A.9.2), CAP et tout
//! marqueur inconnu à longueur.
//!
//! Tolérance : une tuile-partie tronquée garde les octets disponibles ; un
//! Psot nul ou trop grand s'étend jusqu'à la fin des données ; l'EOC
//! manquant est accepté. Toute anomalie dans l'en-tête principal est une
//! erreur (`Corrupt`), car rien ne peut être décodé sans lui.

use acrux_core::{Error, Result};

use super::corrupt;
use super::structure::Rect;

/// Codes des marqueurs (tableau A.2).
pub(super) mod marker {
    pub const SOC: u16 = 0xFF4F;
    pub const CAP: u16 = 0xFF50;
    pub const SIZ: u16 = 0xFF51;
    pub const COD: u16 = 0xFF52;
    pub const COC: u16 = 0xFF53;
    pub const TLM: u16 = 0xFF55;
    pub const PLM: u16 = 0xFF57;
    pub const PLT: u16 = 0xFF58;
    pub const QCD: u16 = 0xFF5C;
    pub const QCC: u16 = 0xFF5D;
    pub const RGN: u16 = 0xFF5E;
    pub const POC: u16 = 0xFF5F;
    pub const PPM: u16 = 0xFF60;
    pub const PPT: u16 = 0xFF61;
    pub const CRG: u16 = 0xFF63;
    pub const COM: u16 = 0xFF64;
    pub const SOT: u16 = 0xFF90;
    pub const SOP: u16 = 0xFF91;
    pub const EPH: u16 = 0xFF92;
    pub const SOD: u16 = 0xFF93;
    pub const EOC: u16 = 0xFFD9;
}

/// Nombre maximal de tuiles (A.5.1 : numXtiles × numYtiles ≤ 65535).
pub(super) const MAX_TILES: u64 = 65535;

/// Paramètres d'une composante (SIZ, tableau A.11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ComponentSiz {
    /// Profondeur en bits (1 à 38 ; ce décodeur accepte jusqu'à 16).
    pub precision: u8,
    /// Échantillons signés.
    pub signed: bool,
    /// Pas d'échantillonnage horizontal XRsiz (≥ 1).
    pub xr: u32,
    /// Pas d'échantillonnage vertical YRsiz.
    pub yr: u32,
}

/// Segment SIZ : géométrie de l'image sur la grille de référence (A.5.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Siz {
    /// XOsiz, YOsiz : origine de l'image.
    pub x0: u32,
    pub y0: u32,
    /// Xsiz, Ysiz : bord droit et bas exclus.
    pub x1: u32,
    pub y1: u32,
    /// XTsiz, YTsiz : taille des tuiles.
    pub tile_w: u32,
    pub tile_h: u32,
    /// XTOsiz, YTOsiz : origine de la grille de tuiles.
    pub tile_x0: u32,
    pub tile_y0: u32,
    pub components: Vec<ComponentSiz>,
}

impl Siz {
    /// numXtiles (équation B-5).
    pub fn tiles_wide(&self) -> u32 {
        u32::try_from(
            (u64::from(self.x1) - u64::from(self.tile_x0)).div_ceil(u64::from(self.tile_w)),
        )
        .unwrap_or(u32::MAX)
    }

    /// numYtiles (équation B-5).
    pub fn tiles_high(&self) -> u32 {
        u32::try_from(
            (u64::from(self.y1) - u64::from(self.tile_y0)).div_ceil(u64::from(self.tile_h)),
        )
        .unwrap_or(u32::MAX)
    }

    /// Rectangle de la tuile d'indice `index` sur la grille de référence
    /// (équations B-7 à B-10).
    pub fn tile_rect(&self, index: u32) -> Rect {
        let p = u64::from(index % self.tiles_wide());
        let q = u64::from(index / self.tiles_wide());
        let clamp = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);
        let tx0 = (u64::from(self.tile_x0) + p * u64::from(self.tile_w)).max(u64::from(self.x0));
        let ty0 = (u64::from(self.tile_y0) + q * u64::from(self.tile_h)).max(u64::from(self.y0));
        let tx1 =
            (u64::from(self.tile_x0) + (p + 1) * u64::from(self.tile_w)).min(u64::from(self.x1));
        let ty1 =
            (u64::from(self.tile_y0) + (q + 1) * u64::from(self.tile_h)).min(u64::from(self.y1));
        Rect {
            x0: clamp(tx0),
            y0: clamp(ty0),
            x1: clamp(tx1),
            y1: clamp(ty1),
        }
    }

    /// Rectangle de la composante `c` sur sa propre grille (B.2, équation B-2).
    pub fn component_rect(&self, c: usize) -> Rect {
        let comp = self.components[c];
        Rect {
            x0: self.x0.div_ceil(comp.xr),
            y0: self.y0.div_ceil(comp.yr),
            x1: self.x1.div_ceil(comp.xr),
            y1: self.y1.div_ceil(comp.yr),
        }
    }
}

/// Ordre de progression (tableau A.16).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Progression {
    Lrcp,
    Rlcp,
    Rpcl,
    Pcrl,
    Cprl,
}

impl Progression {
    fn from_byte(b: u8) -> Result<Self> {
        Ok(match b {
            0 => Progression::Lrcp,
            1 => Progression::Rlcp,
            2 => Progression::Rpcl,
            3 => Progression::Pcrl,
            4 => Progression::Cprl,
            _ => return Err(corrupt("ordre de progression inconnu")),
        })
    }
}

/// Paramètres de codage d'une composante (SPcod / SPcoc, tableau A.15).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CodingStyle {
    /// Nombre de niveaux de décomposition NL (0 à 32).
    pub levels: u8,
    /// Exposants de la taille des blocs de code (xcb, ycb ; 2 à 10).
    pub xcb: u8,
    pub ycb: u8,
    /// Modes du bloc (tableau A.19).
    pub cbstyle: u8,
    /// Vrai pour le filtre 5-3 réversible, faux pour le 9-7.
    pub reversible: bool,
    /// Exposants (PPx, PPy) de la taille des précincts par résolution.
    pub precincts: Vec<(u8, u8)>,
}

/// Segment COD (tableau A.12) : partie SGcod commune à la tuile et SPcod.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Cod {
    pub progression: Progression,
    pub layers: u16,
    /// Transformation multi-composantes (RCT ou ICT).
    pub mct: bool,
    pub sop: bool,
    pub eph: bool,
    pub style: CodingStyle,
}

/// Style de quantification (tableau A.28).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum QuantStyle {
    /// Aucune quantification (réversible).
    None,
    /// Scalaire dérivée : un seul pas, les autres s'en déduisent (E-5).
    Derived,
    /// Scalaire explicite : un pas par sous-bande.
    Expounded,
}

/// Segment QCD / QCC (A.6.4, A.6.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Quant {
    /// Bits de garde G.
    pub guard: u8,
    pub style: QuantStyle,
    /// (εb, μb) par sous-bande dans l'ordre LL, puis HL, LH, HH de chaque
    /// résolution croissante.
    pub steps: Vec<(u8, u16)>,
}

/// Entrée d'un segment POC (tableau A.32).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PocEntry {
    pub res_start: u8,
    pub comp_start: u16,
    pub layer_end: u16,
    pub res_end: u8,
    pub comp_end: u16,
    pub progression: Progression,
}

/// Segments de paramètres d'un en-tête (principal ou de tuile).
#[derive(Clone, Debug, Default)]
pub(super) struct Headers {
    pub cod: Option<Cod>,
    pub coc: Vec<Option<CodingStyle>>,
    pub qcd: Option<Quant>,
    pub qcc: Vec<Option<Quant>>,
    /// Décalage ROI (RGN) par composante.
    pub rgn: Vec<Option<u8>>,
    pub poc: Vec<PocEntry>,
}

impl Headers {
    fn new(ncomp: usize) -> Self {
        Headers {
            cod: None,
            coc: vec![None; ncomp],
            qcd: None,
            qcc: vec![None; ncomp],
            rgn: vec![None; ncomp],
            poc: Vec::new(),
        }
    }
}

/// Tuile rassemblée à partir de ses tuiles-parties.
#[derive(Clone, Debug, Default)]
pub(super) struct Tile {
    pub headers: Headers,
    /// Corps des tuiles-parties concaténés (paquets).
    pub body: Vec<u8>,
    /// En-têtes de paquets déportés (PPM / PPT) concaténés.
    pub packed_headers: Vec<u8>,
    pub has_packed_headers: bool,
    /// Nombre de tuiles-parties rencontrées.
    pub parts: u32,
}

/// Flux de code analysé.
#[derive(Clone, Debug)]
pub(super) struct Codestream {
    pub siz: Siz,
    pub main: Headers,
    /// Tuiles par indice ; `None` si aucune tuile-partie n'a été trouvée.
    pub tiles: Vec<Option<Tile>>,
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// Vrai pour les marqueurs sans segment de paramètres (A.4, tableau A.2).
fn has_no_length(m: u16) -> bool {
    matches!(
        m,
        marker::SOC | marker::SOD | marker::EOC | marker::EPH | 0xFF30..=0xFF3F
    )
}

/// Lit un marqueur et son segment à `pos` : code, corps (sans le champ
/// longueur) et position suivante.
fn read_segment(data: &[u8], pos: usize) -> Result<(u16, &[u8], usize)> {
    let m = be16(data, pos).ok_or_else(|| corrupt("en-tête tronqué"))?;
    if m >> 8 != 0xFF {
        return Err(corrupt("marqueur attendu"));
    }
    if has_no_length(m) {
        return Ok((m, &[], pos + 2));
    }
    let len = usize::from(be16(data, pos + 2).ok_or_else(|| corrupt("segment tronqué"))?);
    if len < 2 {
        return Err(corrupt("longueur de segment invalide"));
    }
    let body = data
        .get(pos + 4..pos + 2 + len)
        .ok_or_else(|| corrupt("segment tronqué"))?;
    Ok((m, body, pos + 2 + len))
}

/// Segment SIZ (tableau A.9).
fn parse_siz(body: &[u8]) -> Result<Siz> {
    let field = |at: usize| be32(body, at).ok_or_else(|| corrupt("SIZ tronqué"));
    let ncomp = usize::from(be16(body, 34).ok_or_else(|| corrupt("SIZ tronqué"))?);
    if ncomp == 0 || ncomp > 16384 {
        return Err(corrupt("nombre de composantes invalide"));
    }
    let siz = Siz {
        x1: field(2)?,
        y1: field(6)?,
        x0: field(10)?,
        y0: field(14)?,
        tile_w: field(18)?,
        tile_h: field(22)?,
        tile_x0: field(26)?,
        tile_y0: field(30)?,
        components: Vec::with_capacity(ncomp),
    };
    let mut siz = siz;
    for c in 0..ncomp {
        let spec = body
            .get(36 + 3 * c..39 + 3 * c)
            .ok_or_else(|| corrupt("SIZ tronqué"))?;
        let precision = (spec[0] & 0x7F) + 1;
        if precision > 16 {
            return Err(Error::Unsupported(format!(
                "JPEG 2000 : précision {precision} bits"
            )));
        }
        if spec[1] == 0 || spec[2] == 0 {
            return Err(corrupt("pas d'échantillonnage nul"));
        }
        siz.components.push(ComponentSiz {
            precision,
            signed: spec[0] & 0x80 != 0,
            xr: u32::from(spec[1]),
            yr: u32::from(spec[2]),
        });
    }
    if siz.x1 <= siz.x0 || siz.y1 <= siz.y0 {
        return Err(corrupt("dimensions d'image nulles"));
    }
    if siz.tile_w == 0 || siz.tile_h == 0 {
        return Err(corrupt("taille de tuile nulle"));
    }
    if siz.tile_x0 > siz.x0
        || siz.tile_y0 > siz.y0
        || u64::from(siz.tile_x0) + u64::from(siz.tile_w) <= u64::from(siz.x0)
        || u64::from(siz.tile_y0) + u64::from(siz.tile_h) <= u64::from(siz.y0)
    {
        return Err(corrupt("grille de tuiles incohérente"));
    }
    let tiles = u64::from(siz.tiles_wide()) * u64::from(siz.tiles_high());
    if tiles == 0 || tiles > MAX_TILES {
        return Err(corrupt("nombre de tuiles invalide"));
    }
    Ok(siz)
}

/// Partie SPcod / SPcoc (tableau A.15).
fn parse_coding_style(b: &[u8], custom_precincts: bool) -> Result<CodingStyle> {
    if b.len() < 5 {
        return Err(corrupt("COD/COC tronqué"));
    }
    let levels = b[0];
    let xcb = (b[1] & 15) + 2;
    let ycb = (b[2] & 15) + 2;
    if levels > 32 || xcb > 10 || ycb > 10 || xcb + ycb > 12 {
        return Err(corrupt("paramètres de décomposition ou de bloc invalides"));
    }
    let reversible = match b[4] {
        0 => false,
        1 => true,
        _ => return Err(corrupt("transformation en ondelettes inconnue")),
    };
    let mut precincts = Vec::with_capacity(usize::from(levels) + 1);
    for r in 0..=usize::from(levels) {
        if custom_precincts {
            let p = *b.get(5 + r).ok_or_else(|| corrupt("COD/COC tronqué"))?;
            let (ppx, ppy) = (p & 15, p >> 4);
            if r > 0 && (ppx == 0 || ppy == 0) {
                return Err(corrupt("précinct de taille 1 hors de la résolution 0"));
            }
            precincts.push((ppx, ppy));
        } else {
            precincts.push((15, 15));
        }
    }
    Ok(CodingStyle {
        levels,
        xcb,
        ycb,
        cbstyle: b[3],
        reversible,
        precincts,
    })
}

/// Segment COD (tableau A.12).
fn parse_cod(body: &[u8]) -> Result<Cod> {
    if body.len() < 10 {
        return Err(corrupt("COD tronqué"));
    }
    let scod = body[0];
    let layers = u16::from_be_bytes([body[2], body[3]]);
    if layers == 0 {
        return Err(corrupt("nombre de couches nul"));
    }
    Ok(Cod {
        progression: Progression::from_byte(body[1])?,
        layers,
        mct: body[4] != 0,
        sop: scod & 2 != 0,
        eph: scod & 4 != 0,
        style: parse_coding_style(&body[5..], scod & 1 != 0)?,
    })
}

/// Indice de composante codé sur 1 octet (Csiz < 257) ou 2 (A.6.2).
fn component_index(body: &[u8], ncomp: usize) -> Result<(usize, usize)> {
    if ncomp < 257 {
        let c = *body.first().ok_or_else(|| corrupt("segment tronqué"))?;
        Ok((usize::from(c), 1))
    } else {
        let c = be16(body, 0).ok_or_else(|| corrupt("segment tronqué"))?;
        Ok((usize::from(c), 2))
    }
}

/// Segment COC (tableau A.23).
fn parse_coc(body: &[u8], ncomp: usize) -> Result<(usize, CodingStyle)> {
    let (c, at) = component_index(body, ncomp)?;
    if c >= ncomp {
        return Err(corrupt("COC : composante inconnue"));
    }
    let scoc = *body.get(at).ok_or_else(|| corrupt("COC tronqué"))?;
    Ok((c, parse_coding_style(&body[at + 1..], scoc & 1 != 0)?))
}

/// Corps d'un QCD / QCC (tableaux A.28 à A.30).
fn parse_quant(body: &[u8]) -> Result<Quant> {
    let sq = *body.first().ok_or_else(|| corrupt("QCD tronqué"))?;
    let guard = sq >> 5;
    let rest = &body[1..];
    let (style, steps) = match sq & 0x1F {
        0 => (
            QuantStyle::None,
            rest.iter().map(|&b| (b >> 3, 0u16)).collect::<Vec<_>>(),
        ),
        1 => {
            let v = be16(rest, 0).ok_or_else(|| corrupt("QCD tronqué"))?;
            (QuantStyle::Derived, vec![((v >> 11) as u8, v & 0x7FF)])
        }
        2 => (
            QuantStyle::Expounded,
            rest.chunks_exact(2)
                .map(|s| {
                    let v = u16::from_be_bytes([s[0], s[1]]);
                    ((v >> 11) as u8, v & 0x7FF)
                })
                .collect(),
        ),
        _ => return Err(corrupt("style de quantification inconnu")),
    };
    if steps.is_empty() {
        return Err(corrupt("QCD sans pas de quantification"));
    }
    Ok(Quant {
        guard,
        style,
        steps,
    })
}

/// Segment QCC (tableau A.31).
fn parse_qcc(body: &[u8], ncomp: usize) -> Result<(usize, Quant)> {
    let (c, at) = component_index(body, ncomp)?;
    if c >= ncomp {
        return Err(corrupt("QCC : composante inconnue"));
    }
    Ok((c, parse_quant(&body[at..])?))
}

/// Segment RGN (tableau A.25) : décalage de la méthode Maxshift (H.1).
fn parse_rgn(body: &[u8], ncomp: usize) -> Result<(usize, u8)> {
    let (c, at) = component_index(body, ncomp)?;
    if c >= ncomp {
        return Err(corrupt("RGN : composante inconnue"));
    }
    let shift = *body.get(at + 1).ok_or_else(|| corrupt("RGN tronqué"))?;
    Ok((c, shift))
}

/// Segment POC (tableau A.32).
fn parse_poc(body: &[u8], ncomp: usize) -> Result<Vec<PocEntry>> {
    let wide = ncomp >= 257;
    let size = if wide { 9 } else { 7 };
    let mut entries = Vec::new();
    // Disposition : RSpoc(1) CSpoc(1|2) LYEpoc(2) REpoc(1) CEpoc(1|2) Ppoc(1).
    for e in body.chunks_exact(size) {
        let entry = if wide {
            PocEntry {
                res_start: e[0],
                comp_start: u16::from_be_bytes([e[1], e[2]]),
                layer_end: u16::from_be_bytes([e[3], e[4]]),
                res_end: e[5],
                comp_end: u16::from_be_bytes([e[6], e[7]]),
                progression: Progression::from_byte(e[8])?,
            }
        } else {
            PocEntry {
                res_start: e[0],
                comp_start: u16::from(e[1]),
                layer_end: u16::from_be_bytes([e[2], e[3]]),
                res_end: e[4],
                // CEpoc = 0 sur un octet signifie 256 (tableau A.32).
                comp_end: if e[5] == 0 { 256 } else { u16::from(e[5]) },
                progression: Progression::from_byte(e[6])?,
            }
        };
        entries.push(entry);
    }
    Ok(entries)
}

/// Applique un segment de paramètres à un jeu d'en-têtes.
fn apply_parameter_segment(m: u16, body: &[u8], ncomp: usize, h: &mut Headers) -> Result<bool> {
    match m {
        marker::COD => h.cod = Some(parse_cod(body)?),
        marker::COC => {
            let (c, style) = parse_coc(body, ncomp)?;
            h.coc[c] = Some(style);
        }
        marker::QCD => h.qcd = Some(parse_quant(body)?),
        marker::QCC => {
            let (c, q) = parse_qcc(body, ncomp)?;
            h.qcc[c] = Some(q);
        }
        marker::RGN => {
            let (c, shift) = parse_rgn(body, ncomp)?;
            h.rgn[c] = Some(shift);
        }
        marker::POC => h.poc.extend(parse_poc(body, ncomp)?),
        _ => return Ok(false),
    }
    Ok(true)
}

/// Lit SOC puis SIZ et rend la géométrie avec la position suivante.
fn read_soc_siz(data: &[u8]) -> Result<(Siz, usize)> {
    if be16(data, 0) != Some(marker::SOC) {
        return Err(corrupt("marqueur SOC absent"));
    }
    let (m, body, next) = read_segment(data, 2)?;
    if m != marker::SIZ {
        return Err(corrupt("SIZ doit suivre SOC"));
    }
    Ok((parse_siz(body)?, next))
}

/// Lit uniquement le segment SIZ (pour [`super::read_header`]).
pub(super) fn read_siz(data: &[u8]) -> Result<Siz> {
    read_soc_siz(data).map(|(siz, _)| siz)
}

/// Analyse un flux de code complet.
pub(super) fn parse(data: &[u8]) -> Result<Codestream> {
    let (siz, mut pos) = read_soc_siz(data)?;
    let ncomp = siz.components.len();
    let mut main = Headers::new(ncomp);
    let mut ppm_parts: Vec<(u8, &[u8])> = Vec::new();
    // En-tête principal jusqu'au premier SOT (A.3).
    loop {
        let (m, body, next) = read_segment(data, pos)?;
        match m {
            marker::SOT => break,
            marker::EOC => return Err(corrupt("aucune tuile")),
            marker::PPM => {
                let (z, rest) = body.split_first().ok_or_else(|| corrupt("PPM tronqué"))?;
                ppm_parts.push((*z, rest));
            }
            // Index de longueurs, calage des composantes, commentaires et
            // capacités : sans effet sur le décodage.
            marker::TLM | marker::PLM | marker::CRG | marker::COM | marker::CAP => {}
            _ => {
                apply_parameter_segment(m, body, ncomp, &mut main)?;
            }
        }
        pos = next;
    }
    if main.cod.is_none() {
        return Err(corrupt("segment COD absent"));
    }
    if main.qcd.is_none() {
        return Err(corrupt("segment QCD absent"));
    }
    let ntiles = usize::try_from(u64::from(siz.tiles_wide()) * u64::from(siz.tiles_high()))
        .map_err(|_| corrupt("nombre de tuiles invalide"))?;
    let mut tiles: Vec<Option<Tile>> = (0..ntiles).map(|_| None).collect();
    let mut part_order: Vec<usize> = Vec::new();
    // Tuiles-parties (A.4.2) : on s'arrête proprement à la première
    // anomalie pour rendre ce qui a été lu.
    while be16(data, pos) == Some(marker::SOT) {
        let Ok(()) = read_tile_part(data, &mut pos, ncomp, &mut tiles, &mut part_order) else {
            break;
        };
    }
    if part_order.is_empty() {
        return Err(corrupt("aucune tuile-partie exploitable"));
    }
    if !ppm_parts.is_empty() {
        distribute_ppm(&mut ppm_parts, &part_order, &mut tiles);
    }
    Ok(Codestream { siz, main, tiles })
}

/// Lit une tuile-partie à partir de son SOT (tableau A.5) et l'ajoute à sa
/// tuile. `pos` est avancé à la fin de la tuile-partie.
fn read_tile_part(
    data: &[u8],
    pos: &mut usize,
    ncomp: usize,
    tiles: &mut [Option<Tile>],
    part_order: &mut Vec<usize>,
) -> Result<()> {
    let sot_start = *pos;
    let (_, body, mut p) = read_segment(data, sot_start)?;
    if body.len() < 8 {
        return Err(corrupt("SOT tronqué"));
    }
    let isot = usize::from(u16::from_be_bytes([body[0], body[1]]));
    let psot = u32::from_be_bytes([body[2], body[3], body[4], body[5]]);
    if isot >= tiles.len() {
        return Err(corrupt("indice de tuile hors limites"));
    }
    if psot != 0 && psot < 14 {
        return Err(corrupt("Psot invalide"));
    }
    let tile = tiles[isot].get_or_insert_with(|| Tile {
        headers: Headers::new(ncomp),
        ..Tile::default()
    });
    let mut ppt_parts: Vec<(u8, &[u8])> = Vec::new();
    // En-tête de tuile-partie jusqu'à SOD (A.4.3).
    let sod_end = loop {
        let (m, seg, next) = read_segment(data, p)?;
        match m {
            marker::SOD => break next,
            marker::PPT => {
                let (z, rest) = seg.split_first().ok_or_else(|| corrupt("PPT tronqué"))?;
                ppt_parts.push((*z, rest));
            }
            marker::SOT | marker::EOC => return Err(corrupt("SOD absent")),
            marker::PLT | marker::COM => {}
            _ => {
                apply_parameter_segment(m, seg, ncomp, &mut tile.headers)?;
            }
        }
        p = next;
    };
    let part_end = if psot == 0 {
        data.len()
    } else {
        (sot_start + usize::try_from(psot).unwrap_or(usize::MAX)).min(data.len())
    }
    .max(sod_end);
    tile.body.extend_from_slice(&data[sod_end..part_end]);
    tile.parts += 1;
    ppt_parts.sort_by_key(|(z, _)| *z);
    for (_, chunk) in ppt_parts {
        tile.packed_headers.extend_from_slice(chunk);
        tile.has_packed_headers = true;
    }
    part_order.push(isot);
    *pos = part_end;
    Ok(())
}

/// Répartit les en-têtes de paquets PPM (A.7.4) : la concaténation des
/// segments, dans l'ordre Zppm, est une suite de morceaux (Nppm, octets),
/// un par tuile-partie dans l'ordre du flux.
fn distribute_ppm(parts: &mut [(u8, &[u8])], part_order: &[usize], tiles: &mut [Option<Tile>]) {
    parts.sort_by_key(|(z, _)| *z);
    let all: Vec<u8> = parts.iter().flat_map(|(_, d)| d.iter().copied()).collect();
    let mut at = 0usize;
    for &tile_index in part_order {
        let Some(n) = be32(&all, at) else {
            break;
        };
        let start = at + 4;
        let end = (start + usize::try_from(n).unwrap_or(usize::MAX)).min(all.len());
        if let Some(tile) = tiles.get_mut(tile_index).and_then(Option::as_mut) {
            tile.packed_headers
                .extend_from_slice(&all[start..end.max(start)]);
            tile.has_packed_headers = true;
        }
        at = end;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn siz_body(x1: u32, y1: u32, tw: u32, th: u32) -> Vec<u8> {
        let mut b = vec![0, 0];
        for v in [x1, y1, 0, 0, tw, th, 0, 0] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        b.extend_from_slice(&[0, 1, 7, 1, 1]);
        b
    }

    #[test]
    fn siz_tile_geometry_follows_b3() {
        let siz = parse_siz(&siz_body(100, 70, 32, 32)).unwrap();
        assert_eq!(siz.tiles_wide(), 4);
        assert_eq!(siz.tiles_high(), 3);
        let last = siz.tile_rect(11);
        assert_eq!((last.x0, last.y0, last.x1, last.y1), (96, 64, 100, 70));
    }

    #[test]
    fn siz_rejects_absurd_geometry() {
        let mut b = siz_body(100, 70, 0, 32);
        assert!(parse_siz(&b).is_err());
        b = siz_body(0, 70, 32, 32);
        assert!(parse_siz(&b).is_err());
        b = siz_body(100, 70, 32, 32);
        b[36] = 20; // précision 21 bits
        assert!(matches!(parse_siz(&b), Err(Error::Unsupported(_))));
    }

    #[test]
    fn quantization_styles_are_parsed() {
        let none = parse_quant(&[0x40, 8 << 3, 9 << 3]).unwrap();
        assert_eq!(none.guard, 2);
        assert_eq!(none.style, QuantStyle::None);
        assert_eq!(none.steps, vec![(8, 0), (9, 0)]);
        let derived = parse_quant(&[0x41, 0x48, 0x01]).unwrap();
        assert_eq!(derived.style, QuantStyle::Derived);
        assert_eq!(derived.steps, vec![(9, 1)]);
        assert!(parse_quant(&[0x43]).is_err());
        assert!(parse_quant(&[0x40]).is_err());
    }

    #[test]
    fn poc_entries_use_one_byte_components_below_257() {
        let entries = parse_poc(&[0, 0, 0, 5, 3, 0, 1], 3).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].comp_end, 256);
        assert_eq!(entries[0].layer_end, 5);
        assert_eq!(entries[0].progression, Progression::Rlcp);
    }
}
