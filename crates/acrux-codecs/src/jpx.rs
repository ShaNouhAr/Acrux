//! Filtre JPXDecode (ISO 32000-2 §7.4.9) : décodeur JPEG 2000 écrit
//! d'après ISO/IEC 15444-1 (JPEG 2000 partie 1), sans aucune dépendance ni
//! code tiers.
//!
//! Couvert :
//! - conteneurs : flux de code brut J2K (SOC en tête) et fichier JP2
//!   (annexe I : `jP  `, `ftyp`, `jp2h` avec `ihdr`, `colr` méthode 1
//!   (espaces énumérés sRGB, gris, sYCC, CMYK) et méthode 2 (profil ICC
//!   exposé tel quel), `pclr` + `cmap` (palette appliquée), `cdef` (canal
//!   alpha), `jp2c`) ;
//! - marqueurs : SIZ, COD, COC, QCD, QCC, RGN (méthode Maxshift appliquée),
//!   POC (changements de progression appliqués), PPM / PPT (en-têtes de
//!   paquets déportés), TLM / PLM / PLT / CRG / COM ignorés, SOT / SOD /
//!   EOC, SOP / EPH ;
//! - tuiles multiples, tuiles-parties multiples, composantes
//!   sous-échantillonnées (XRsiz / YRsiz, ramenées à la grille de la
//!   composante 0 par réplication), précisions 1 à 16 bits, signé ou non ;
//! - progressions LRCP, RLCP, RPCL, PCRL, CPRL, couches multiples,
//!   précincts, arbres d'étiquettes, en-têtes de paquets complets (B.10) ;
//! - tier-1 EBCOT (annexe D) : décodeur MQ (annexe C), trois passes, 19
//!   contextes, mode plage, et les six modes de bloc (bypass, reset,
//!   termall, causal vertical, terminaison prédictible, symboles de
//!   segmentation) ;
//! - déquantification (E.1 : sans quantification, scalaire dérivée ou
//!   explicite), ondelettes inverses 5-3 réversible et 9-7 irréversible
//!   (annexe F), transformation multi-composantes inverse RCT / ICT
//!   (annexe G), décalage DC, conversion en 8 bits ;
//! - décodage à résolution réduite ([`JpxOptions::max_resolution_reduction`]).
//!
//! Non couvert (`Err(Unsupported)`) : extensions de la partie 2 (ISO/IEC
//! 15444-2 : ondelettes arbitraires, MCT étendue, profils ICC libres…),
//! précisions au-delà de 16 bits, palettes de plus de 16 bits.
//!
//! Tolérance : données tronquées ou corrompues → image partielle (les
//! blocs jamais reçus restent à mi-échelle), EOC manquant, Psot nul ou
//! trop grand, marqueurs inconnus ignorés. Aucune entrée ne provoque de
//! panique ni de boucle infinie : toutes les tailles sont bornées
//! ([`MAX_PIXELS`], [`MAX_BUFFER_BYTES`], nombre de précincts et de blocs)
//! et chaque boucle de lecture s'arrête au premier dépassement.
//!
//! Sortie : échantillons entrelacés sur 8 bits après transformation
//! multi-composantes, palette et, pour sYCC, conversion en RVB. Les
//! composantes de plus de 8 bits sont tronquées (décalage à droite),
//! celles de moins de 8 bits étirées sur 0..255, les composantes signées
//! décalées de 2^(précision − 1) pour devenir non signées.

use acrux_core::{Error, Result};

mod boxes;
mod codestream;
mod dwt;
mod mq;
mod structure;
#[cfg(test)]
mod tests;
mod tier1;
mod tier2;

use boxes::{Colour, Container};
use codestream::{Cod, Codestream, CodingStyle, ComponentSiz, Quant, Tile};
use structure::{Band, Budget, Coeffs, Rect, TileComponent};
use tier1::{BlockCoder, BlockParams};

/// Nombre maximal de pixels de la composante la plus fine.
pub const MAX_PIXELS: u64 = 1 << 31;

/// Taille maximale, en octets, du tampon de sortie et, séparément, des
/// tampons de coefficients d'une tuile ; au-delà l'image est refusée
/// (`Err(Corrupt)`).
pub const MAX_BUFFER_BYTES: u64 = 1 << 30;

/// Espace de couleur déclaré par le conteneur JP2 (boîte `colr`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JpxColorSpace {
    /// sRGB (espace énuméré 16).
    Srgb,
    /// Niveaux de gris (17).
    Gray,
    /// sYCC (18) : les échantillons rendus dans [`JpxImage::data`] ont déjà
    /// été convertis en RVB et peuvent être traités comme sRGB.
    Sycc,
    /// CMYK (12).
    Cmyk,
    /// Profil ICC restreint (méthode 2) : octets du profil.
    Icc(Vec<u8>),
    /// Flux brut sans conteneur, espace énuméré inconnu ou boîte absente :
    /// c'est à la couche PDF d'appliquer /ColorSpace.
    Unknown,
}

/// Image JPEG 2000 décodée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JpxImage {
    /// Largeur en pixels (grille de la composante 0, réduite le cas échéant).
    pub width: u32,
    /// Hauteur en pixels.
    pub height: u32,
    /// Nombre de canaux entrelacés dans `data` (alpha compris si présent).
    pub components: u8,
    /// Échantillons entrelacés, ligne par ligne, `components` octets par
    /// pixel, sur 8 bits, après transformation multi-composantes.
    pub data: Vec<u8>,
    /// Espace de couleur déclaré par le conteneur.
    pub colorspace: JpxColorSpace,
    /// Vrai si le dernier canal de `data` est un canal d'opacité (boîte
    /// `cdef`), conservé parce que [`JpxOptions::smask_in_data`] l'a demandé.
    pub has_alpha: bool,
    /// Vrai si une palette (`pclr` + `cmap`) a été appliquée.
    pub palette_applied: bool,
}

/// Options de décodage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JpxOptions {
    /// Nombre de niveaux de résolution à abandonner : l'image rendue est
    /// réduite d'un facteur 2^n (borné par le nombre de niveaux de
    /// décomposition du flux).
    pub max_resolution_reduction: u8,
    /// Vrai pour conserver le canal alpha du JPX dans `data` (/SMaskInData
    /// du dictionnaire d'image, ISO 32000-2 §8.9.5) ; faux pour le retirer.
    pub smask_in_data: bool,
}

fn corrupt(message: &str) -> Error {
    Error::Corrupt(format!("JPEG 2000 : {message}"))
}

/// Décode un fichier JP2 ou un flux de code J2K avec les options par
/// défaut (pleine résolution, canal alpha retiré).
///
/// # Errors
///
/// - `Error::Corrupt` si l'en-tête principal est inexploitable (pas de
///   SOC/SIZ, COD ou QCD absent, géométrie absurde, image trop grande…) ;
/// - `Error::Unsupported` pour les extensions de la partie 2 et les
///   précisions au-delà de 16 bits.
///
/// Une fois l'en-tête principal lu, toute anomalie arrête le décodage de la
/// tuile concernée et l'image partielle est rendue.
pub fn decode(data: &[u8]) -> Result<JpxImage> {
    decode_with_options(data, &JpxOptions::default())
}

/// Décode avec des options (voir [`decode`] pour les erreurs).
///
/// # Errors
///
/// Voir [`decode`].
pub fn decode_with_options(data: &[u8], options: &JpxOptions) -> Result<JpxImage> {
    let container = boxes::parse(data)?;
    let cs = codestream::parse(container.codestream)?;
    let planes = decode_codestream(&cs, options.max_resolution_reduction)?;
    assemble(&planes, &container, options.smask_in_data)
}

/// Lit les dimensions (grille de la composante 0, pleine résolution) et le
/// nombre de canaux de couleur que [`decode`] rendrait, sans décoder.
///
/// # Errors
///
/// `Error::Corrupt` si le conteneur ou le segment SIZ est invalide,
/// `Error::Unsupported` pour les précisions au-delà de 16 bits.
pub fn read_header(data: &[u8]) -> Result<(u32, u32, u8)> {
    let container = boxes::parse(data)?;
    let siz = codestream::read_siz(container.codestream)?;
    let rect = siz.component_rect(0);
    let layout = channel_layout(&container, siz.components.len())?;
    let count = u8::try_from(layout.colour.len())
        .map_err(|_| Error::Unsupported("JPEG 2000 : plus de 255 canaux".to_owned()))?;
    Ok((rect.width(), rect.height(), count))
}

// ---------------------------------------------------------------------------
// Décodage du flux de code, tuile par tuile
// ---------------------------------------------------------------------------

/// Plan d'échantillons d'une composante : valeurs brutes sur
/// `min(précision, 8)` bits (les composantes plus profondes sont
/// tronquées), non signées.
struct Plane {
    rect: Rect,
    /// Bits significatifs stockés (1 à 8).
    bits: u8,
    data: Vec<u8>,
}

/// Paramètres de codage d'une tuile après application des règles de
/// priorité (A.6.2 : COC de tuile > COD de tuile > COC principal > COD
/// principal, et de même pour QCC / QCD).
struct TileParams<'a> {
    cod: &'a Cod,
    styles: Vec<&'a CodingStyle>,
    quants: Vec<&'a Quant>,
    rois: Vec<u8>,
}

fn resolve_params<'a>(cs: &'a Codestream, tile: &'a Tile) -> Result<TileParams<'a>> {
    let cod = tile
        .headers
        .cod
        .as_ref()
        .or(cs.main.cod.as_ref())
        .ok_or_else(|| corrupt("COD absent"))?;
    let ncomp = cs.siz.components.len();
    let mut styles = Vec::with_capacity(ncomp);
    let mut quants = Vec::with_capacity(ncomp);
    let mut rois = Vec::with_capacity(ncomp);
    for c in 0..ncomp {
        let style = tile.headers.coc[c]
            .as_ref()
            .or(tile.headers.cod.as_ref().map(|d| &d.style))
            .or(cs.main.coc[c].as_ref())
            .unwrap_or(&cod.style);
        let quant = tile.headers.qcc[c]
            .as_ref()
            .or(tile.headers.qcd.as_ref())
            .or(cs.main.qcc[c].as_ref())
            .or(cs.main.qcd.as_ref())
            .ok_or_else(|| corrupt("QCD absent"))?;
        styles.push(style);
        quants.push(quant);
        rois.push(tile.headers.rgn[c].or(cs.main.rgn[c]).unwrap_or(0));
    }
    Ok(TileParams {
        cod,
        styles,
        quants,
        rois,
    })
}

/// Décode toutes les tuiles dans des plans par composante.
fn decode_codestream(cs: &Codestream, reduce_requested: u8) -> Result<Vec<Plane>> {
    let siz = &cs.siz;
    // Réduction effective : bornée par le plus petit nombre de niveaux.
    let mut reduce = u32::from(reduce_requested);
    for tile in cs.tiles.iter().flatten() {
        let params = resolve_params(cs, tile)?;
        for style in &params.styles {
            reduce = reduce.min(u32::from(style.levels));
        }
    }
    let mut planes = Vec::with_capacity(siz.components.len());
    let mut total_bytes = 0u64;
    for (c, comp) in siz.components.iter().enumerate() {
        let rect = siz.component_rect(c).reduced(reduce);
        if c == 0 && rect.area() > MAX_PIXELS {
            return Err(corrupt("image trop grande"));
        }
        total_bytes += rect.area();
        if total_bytes > MAX_BUFFER_BYTES {
            return Err(corrupt("image trop grande"));
        }
        let bits = comp.precision.min(8);
        let size = usize::try_from(rect.area()).map_err(|_| corrupt("image trop grande"))?;
        planes.push(Plane {
            rect,
            bits,
            data: vec![1 << (bits - 1); size],
        });
    }
    for (index, tile) in cs.tiles.iter().enumerate() {
        let Some(tile) = tile else {
            continue;
        };
        let params = resolve_params(cs, tile)?;
        let index = u32::try_from(index).map_err(|_| corrupt("indice de tuile"))?;
        decode_tile(cs, index, tile, &params, reduce, &mut planes)?;
    }
    Ok(planes)
}

/// Échantillons reconstruits d'une composante de tuile.
enum Samples {
    Int(Vec<i32>),
    Float(Vec<f32>),
}

/// Décode une tuile : structures (annexe B), paquets (tier-2), blocs
/// (tier-1), ondelettes inverses, MCT, puis écriture dans les plans.
fn decode_tile(
    cs: &Codestream,
    index: u32,
    tile: &Tile,
    params: &TileParams<'_>,
    reduce: u32,
    planes: &mut [Plane],
) -> Result<()> {
    let siz = &cs.siz;
    let tile_rect = siz.tile_rect(index);
    let mut budget = Budget::default();
    let mut comps = Vec::with_capacity(siz.components.len());
    let mut coeff_bytes = 0u64;
    for (c, comp) in siz.components.iter().enumerate() {
        let tc = structure::build_tile_component(
            tile_rect,
            *comp,
            params.styles[c],
            params.quants[c],
            params.rois[c],
            &mut budget,
        )?;
        // Coefficients des sous-bandes + tampon de reconstruction, 4 octets
        // par échantillon.
        coeff_bytes += tc.rect.area() * 8;
        if coeff_bytes > MAX_BUFFER_BYTES {
            return Err(corrupt("tuile trop grande"));
        }
        comps.push(tc);
    }
    let packed = if tile.has_packed_headers {
        Some(tile.packed_headers.as_slice())
    } else {
        None
    };
    let poc: Vec<_> = if tile.headers.poc.is_empty() {
        cs.main.poc.clone()
    } else {
        tile.headers.poc.clone()
    };
    tier2::decode_packets(
        &mut comps,
        &tile.body,
        packed,
        &tier2::TileCoding {
            progression: params.cod.progression,
            layers: params.cod.layers,
            poc: &poc,
            tile: tile_rect,
        },
    );
    let mut engine = BlockCoder::new();
    let mut samples = Vec::with_capacity(comps.len());
    for tc in &mut comps {
        samples.push(reconstruct_component(tc, &tile.body, reduce, &mut engine));
    }
    if params.cod.mct && comps.len() >= 3 {
        let r0 = comps[0].rect.reduced(reduce);
        let same = comps[1].rect.reduced(reduce) == r0 && comps[2].rect.reduced(reduce) == r0;
        if same {
            inverse_mct(&mut samples);
        }
    }
    for (c, tc) in comps.iter().enumerate() {
        store_plane(&samples[c], tc.rect.reduced(reduce), tc.siz, &mut planes[c]);
    }
    Ok(())
}

/// Tier-1 sur tous les blocs reçus puis ondelettes inverses (F.3) jusqu'à
/// la résolution demandée.
fn reconstruct_component(
    tc: &mut TileComponent,
    body: &[u8],
    reduce: u32,
    engine: &mut BlockCoder,
) -> Samples {
    let levels_used = usize::try_from(u32::from(tc.style.levels) - reduce).unwrap_or(0);
    let reversible = tc.style.reversible;
    let cbstyle = tc.style.cbstyle;
    let guard = tc.quant.guard;
    let roi = tc.roi_shift;
    let precision = tc.siz.precision;
    for res in tc.resolutions.iter_mut().take(levels_used + 1) {
        let structure::Resolution {
            bands, precincts, ..
        } = res;
        for band in bands.iter_mut() {
            let n = usize::try_from(band.rect.area()).unwrap_or(0);
            band.coeffs = if reversible {
                Coeffs::Int(vec![0; n])
            } else {
                Coeffs::Float(vec![0.0; n])
            };
        }
        for precinct in precincts.iter_mut() {
            for (b, pb) in precinct.bands.iter_mut().enumerate() {
                let band = &mut bands[b];
                for cb in &mut pb.blocks {
                    if cb.passes == 0 {
                        continue;
                    }
                    decode_block(engine, body, cb, band, cbstyle, guard, roi, precision);
                    cb.chunks = Vec::new();
                }
            }
        }
    }
    // Synthèse (F.3.2) : LL de la résolution 0 puis chaque niveau.
    let take = |coeffs: &mut Coeffs| std::mem::replace(coeffs, Coeffs::Empty);
    let mut current = take(&mut tc.resolutions[0].bands[0].coeffs);
    let mut next_int = Vec::new();
    let mut next_float = Vec::new();
    let mut scratch_int = Vec::new();
    let mut scratch_float = Vec::new();
    for r in 1..=levels_used {
        let res = &mut tc.resolutions[r];
        let rect = res.rect;
        let mut taken: Vec<Coeffs> = res.bands.iter_mut().map(|b| take(&mut b.coeffs)).collect();
        match (&mut current, taken.as_mut_slice()) {
            (Coeffs::Int(ll), [Coeffs::Int(hl), Coeffs::Int(lh), Coeffs::Int(hh)]) => {
                dwt::synthesize_level(
                    rect,
                    &dwt::Subbands { ll, hl, lh, hh },
                    &mut next_int,
                    &mut scratch_int,
                    dwt::filter_53,
                );
                std::mem::swap(ll, &mut next_int);
            }
            (Coeffs::Float(ll), [Coeffs::Float(hl), Coeffs::Float(lh), Coeffs::Float(hh)]) => {
                dwt::synthesize_level(
                    rect,
                    &dwt::Subbands { ll, hl, lh, hh },
                    &mut next_float,
                    &mut scratch_float,
                    dwt::filter_97,
                );
                std::mem::swap(ll, &mut next_float);
            }
            _ => break,
        }
    }
    let expected = usize::try_from(tc.rect.reduced(reduce).area()).unwrap_or(0);
    match current {
        Coeffs::Int(mut v) => {
            v.resize(expected, 0);
            Samples::Int(v)
        }
        Coeffs::Float(mut v) => {
            v.resize(expected, 0.0);
            Samples::Float(v)
        }
        Coeffs::Empty => {
            if reversible {
                Samples::Int(vec![0; expected])
            } else {
                Samples::Float(vec![0.0; expected])
            }
        }
    }
}

/// Décode un bloc (annexe D) et écrit ses coefficients déquantifiés
/// (E.1.1) dans la sous-bande.
#[allow(clippy::too_many_arguments)]
fn decode_block(
    engine: &mut BlockCoder,
    body: &[u8],
    cb: &structure::CodeBlock,
    band: &mut Band,
    cbstyle: u8,
    guard: u8,
    roi: u8,
    precision: u8,
) {
    let w = usize::try_from(cb.rect.width()).unwrap_or(0);
    let h = usize::try_from(cb.rect.height()).unwrap_or(0);
    if w == 0 || h == 0 || w > 1024 || h > 1024 {
        return;
    }
    // Mb = G + εb − 1, augmenté du décalage ROI (E-2, H.1).
    let mb = i32::from(guard) + i32::from(band.epsilon) - 1 + i32::from(roi);
    let planes = mb - i32::from(cb.zero_planes);
    let Ok(planes) = u32::try_from(planes) else {
        return;
    };
    if planes == 0 {
        return;
    }
    engine.reset(w, h, band.kind, cbstyle);
    let outcome = engine.decode(
        body,
        &cb.chunks,
        BlockParams {
            style: cbstyle,
            planes,
            passes: cb.passes,
        },
    );
    let bw = usize::try_from(band.rect.width()).unwrap_or(0);
    let ox = usize::try_from(cb.rect.x0 - band.rect.x0).unwrap_or(0);
    let oy = usize::try_from(cb.rect.y0 - band.rect.y0).unwrap_or(0);
    let roi_threshold: u64 = if roi > 0 && roi < 63 { 1 << roi } else { 0 };
    let lowest = outcome.lowest_plane.min(31);
    match &mut band.coeffs {
        Coeffs::Int(out) => {
            // Reconstruction : exacte si tous les plans sont décodés, sinon
            // au milieu de l'intervalle du dernier plan reçu.
            let half: u32 = if lowest > 0 { 1 << (lowest - 1) } else { 0 };
            for y in 0..h {
                let row = &mut out[(oy + y) * bw + ox..][..w];
                for (x, slot) in row.iter_mut().enumerate() {
                    let idx = engine.index(x, y);
                    let mut m = u64::from(engine.mag[idx]);
                    if m != 0 {
                        m |= u64::from(half);
                        if roi_threshold != 0 && m >= roi_threshold {
                            m >>= roi;
                        }
                    }
                    let v = i32::try_from(m).unwrap_or(i32::MAX);
                    *slot = if engine.is_negative(idx) { -v } else { v };
                }
            }
        }
        Coeffs::Float(out) => {
            // Δb = 2^(Rb − εb) (1 + μb / 2^11), Rb = précision + gain (E-3).
            let rb = i32::from(precision) + i32::from(band.gain_log2);
            let delta =
                2f32.powi(rb - i32::from(band.epsilon)) * (1.0 + f32::from(band.mu) / 2048.0);
            #[allow(clippy::cast_precision_loss)]
            let half = if lowest > 0 {
                (1u64 << lowest) as f32 * 0.5
            } else {
                0.5
            };
            #[allow(clippy::cast_precision_loss)]
            let roi_scale = if roi_threshold != 0 {
                1.0 / (roi_threshold as f32)
            } else {
                1.0
            };
            for y in 0..h {
                let row = &mut out[(oy + y) * bw + ox..][..w];
                for (x, slot) in row.iter_mut().enumerate() {
                    let idx = engine.index(x, y);
                    let m = engine.mag[idx];
                    if m == 0 {
                        *slot = 0.0;
                        continue;
                    }
                    #[allow(clippy::cast_precision_loss)]
                    let mut v = (m as f32 + half) * delta;
                    if roi_threshold != 0 && u64::from(m) >= roi_threshold {
                        v *= roi_scale;
                    }
                    *slot = if engine.is_negative(idx) { -v } else { v };
                }
            }
        }
        Coeffs::Empty => {}
    }
}

/// Transformation multi-composantes inverse sur les trois premières
/// composantes : RCT (G.2.2) en entiers, ICT (G.3.2) en flottants.
fn inverse_mct(samples: &mut [Samples]) {
    let (first, rest) = samples.split_at_mut(1);
    let (second, third) = rest.split_at_mut(1);
    match (&mut first[0], &mut second[0], &mut third[0]) {
        (Samples::Int(c0), Samples::Int(c1), Samples::Int(c2)) => {
            for ((y0, y1), y2) in c0.iter_mut().zip(c1.iter_mut()).zip(c2.iter_mut()) {
                let g = y0.wrapping_sub((y1.wrapping_add(*y2)) >> 2);
                let r = y2.wrapping_add(g);
                let b = y1.wrapping_add(g);
                *y0 = r;
                *y1 = g;
                *y2 = b;
            }
        }
        (Samples::Float(c0), Samples::Float(c1), Samples::Float(c2)) => {
            for ((y, cb), cr) in c0.iter_mut().zip(c1.iter_mut()).zip(c2.iter_mut()) {
                let r = *y + 1.402 * *cr;
                let g = *y - 0.344_13 * *cb - 0.714_14 * *cr;
                let b = *y + 1.772 * *cb;
                *y = r;
                *cb = g;
                *cr = b;
            }
        }
        _ => {}
    }
}

/// Décalage DC (G.1.2), saturation et réduction à `min(précision, 8)`
/// bits, puis copie dans le plan de la composante.
fn store_plane(samples: &Samples, rect: Rect, comp: ComponentSiz, plane: &mut Plane) {
    let p = u32::from(comp.precision);
    let offset = 1i32 << (p - 1);
    let max = (1i32 << p) - 1;
    let shift = p.saturating_sub(8);
    let w = usize::try_from(rect.width()).unwrap_or(0);
    let pw = usize::try_from(plane.rect.width()).unwrap_or(0);
    if rect.x0 < plane.rect.x0 || rect.y0 < plane.rect.y0 {
        return;
    }
    let ox = usize::try_from(rect.x0 - plane.rect.x0).unwrap_or(0);
    let oy = usize::try_from(rect.y0 - plane.rect.y0).unwrap_or(0);
    let convert = |v: i32| -> u8 {
        let v = (v.saturating_add(offset)).clamp(0, max) >> shift;
        u8::try_from(v).unwrap_or(255)
    };
    for y in 0..usize::try_from(rect.height()).unwrap_or(0) {
        let start = (oy + y) * pw + ox;
        let Some(row) = plane.data.get_mut(start..start + w) else {
            break;
        };
        match samples {
            Samples::Int(s) => {
                for (slot, &v) in row.iter_mut().zip(&s[y * w..]) {
                    *slot = convert(v);
                }
            }
            Samples::Float(s) => {
                for (slot, &v) in row.iter_mut().zip(&s[y * w..]) {
                    #[allow(clippy::cast_possible_truncation)]
                    let rounded = (v + 0.5).floor() as i32;
                    *slot = convert(rounded);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Assemblage : palette, canaux, sous-échantillonnage, sYCC (annexe I)
// ---------------------------------------------------------------------------

/// Origine d'un canal de sortie.
struct ChannelSource {
    /// Composante du flux de code.
    plane: usize,
    /// Table palette → 8 bits (indexée par la valeur brute), ou `None`
    /// pour une composante directe.
    lut: Option<Vec<u8>>,
}

struct Layout {
    channels: Vec<ChannelSource>,
    /// Indices de canaux de couleur, dans l'ordre de sortie.
    colour: Vec<usize>,
    /// Indice du canal d'opacité, s'il existe.
    alpha: Option<usize>,
    palette_applied: bool,
}

/// Valeur d'une colonne de palette ramenée à 8 bits.
fn palette_to_8_bits(value: u32, depth: u8) -> u8 {
    let max = (1u32 << depth) - 1;
    let v = value.min(max);
    if depth >= 8 {
        u8::try_from(v >> (depth - 8)).unwrap_or(255)
    } else {
        u8::try_from((v * 255 + max / 2) / max).unwrap_or(255)
    }
}

/// Détermine les canaux de sortie d'après `pclr`, `cmap` et `cdef`.
fn channel_layout(container: &Container<'_>, ncomp: usize) -> Result<Layout> {
    let mut channels = Vec::new();
    let mut palette_applied = false;
    if let Some(pal) = &container.palette {
        // Sans cmap, la palette s'applique à la composante 0, une colonne
        // par canal (I.5.3.5 impose cmap ; on tolère son absence).
        let default_cmap: Vec<boxes::CmapEntry> = (0..pal.columns.len())
            .map(|i| boxes::CmapEntry {
                component: 0,
                mtyp: 1,
                pcol: u8::try_from(i).unwrap_or(0),
            })
            .collect();
        let cmap = if container.cmap.is_empty() {
            &default_cmap
        } else {
            &container.cmap
        };
        for e in cmap {
            let plane = usize::from(e.component);
            if plane >= ncomp {
                return Err(corrupt("cmap : composante inconnue"));
            }
            let lut = if e.mtyp == 1 {
                let col = usize::from(e.pcol);
                let &(depth, _) = pal
                    .columns
                    .get(col)
                    .ok_or_else(|| corrupt("cmap : colonne de palette inconnue"))?;
                let ncol = pal.columns.len();
                let lut: Vec<u8> = (0..256)
                    .map(|i| {
                        let entry = i.min(pal.entries - 1);
                        palette_to_8_bits(pal.lut[entry * ncol + col], depth)
                    })
                    .collect();
                palette_applied = true;
                Some(lut)
            } else {
                None
            };
            channels.push(ChannelSource { plane, lut });
        }
    } else {
        channels.extend((0..ncomp).map(|plane| ChannelSource { plane, lut: None }));
    }
    let mut colour: Vec<(u16, usize)> = Vec::new();
    let mut alpha = None;
    if container.cdef.is_empty() {
        colour.extend((0..channels.len()).map(|i| (0, i)));
    } else {
        let mut mentioned = vec![false; channels.len()];
        for e in &container.cdef {
            let ch = usize::from(e.channel);
            if ch >= channels.len() {
                continue;
            }
            mentioned[ch] = true;
            match e.typ {
                0 => colour.push((e.assoc, ch)),
                1 | 2 => alpha = alpha.or(Some(ch)),
                _ => {}
            }
        }
        for (ch, seen) in mentioned.iter().enumerate() {
            if !seen {
                colour.push((0, ch));
            }
        }
        colour.sort_by_key(|&(assoc, _)| assoc);
    }
    if colour.is_empty() {
        return Err(corrupt("aucun canal de couleur"));
    }
    Ok(Layout {
        channels,
        colour: colour.into_iter().map(|(_, ch)| ch).collect(),
        alpha,
        palette_applied,
    })
}

/// Table `valeur brute → 8 bits` d'un plan de `bits` bits.
fn scale_table(bits: u8) -> [u8; 256] {
    let mut t = [0u8; 256];
    let max = (1u32 << bits) - 1;
    for (i, slot) in t.iter_mut().enumerate() {
        let v = u32::try_from(i).unwrap_or(0).min(max);
        *slot = u8::try_from((v * 255 + max / 2) / max).unwrap_or(255);
    }
    t
}

/// Entrelace les canaux dans l'image finale.
fn assemble(planes: &[Plane], container: &Container<'_>, smask: bool) -> Result<JpxImage> {
    let layout = channel_layout(container, planes.len())?;
    let mut order = layout.colour.clone();
    let has_alpha = match layout.alpha {
        Some(a) if smask => {
            order.push(a);
            true
        }
        _ => false,
    };
    let count = order.len();
    let components = u8::try_from(count)
        .map_err(|_| Error::Unsupported("JPEG 2000 : plus de 255 canaux".to_owned()))?;
    // Grille de sortie : la plus fine des composantes utilisées.
    let width = order
        .iter()
        .map(|&ch| planes[layout.channels[ch].plane].rect.width())
        .max()
        .unwrap_or(0);
    let height = order
        .iter()
        .map(|&ch| planes[layout.channels[ch].plane].rect.height())
        .max()
        .unwrap_or(0);
    let (w, h) = (
        usize::try_from(width).unwrap_or(0),
        usize::try_from(height).unwrap_or(0),
    );
    if (w as u64) * (h as u64) * (count as u64) > MAX_BUFFER_BYTES {
        return Err(corrupt("image trop grande"));
    }
    let mut data = vec![0u8; w * h * count];
    let mut col_map: Vec<usize> = Vec::with_capacity(w);
    for (k, &ch) in order.iter().enumerate() {
        let source = &layout.channels[ch];
        let plane = &planes[source.plane];
        let pw = usize::try_from(plane.rect.width()).unwrap_or(0);
        let ph = usize::try_from(plane.rect.height()).unwrap_or(0);
        if pw == 0 || ph == 0 {
            continue;
        }
        let table: Vec<u8> = match &source.lut {
            Some(lut) => lut.clone(),
            None => scale_table(plane.bits).to_vec(),
        };
        col_map.clear();
        col_map.extend((0..w).map(|x| (x * pw / w).min(pw - 1)));
        for y in 0..h {
            let sy = (y * ph / h).min(ph - 1);
            let src = &plane.data[sy * pw..(sy + 1) * pw];
            let dst = &mut data[y * w * count..(y + 1) * w * count];
            for (x, &sx) in col_map.iter().enumerate() {
                dst[x * count + k] = table[usize::from(src[sx])];
            }
        }
    }
    let colorspace = match &container.colour {
        Some(Colour::Enumerated(16)) => JpxColorSpace::Srgb,
        Some(Colour::Enumerated(17)) => JpxColorSpace::Gray,
        Some(Colour::Enumerated(18)) => JpxColorSpace::Sycc,
        Some(Colour::Enumerated(12)) => JpxColorSpace::Cmyk,
        Some(Colour::Icc(profile)) => JpxColorSpace::Icc(profile.clone()),
        _ => JpxColorSpace::Unknown,
    };
    if colorspace == JpxColorSpace::Sycc && layout.colour.len() == 3 {
        sycc_to_rgb(&mut data, count);
    }
    Ok(JpxImage {
        width,
        height,
        components,
        data,
        colorspace,
        has_alpha,
        palette_applied: layout.palette_applied,
    })
}

/// sYCC → sRGB (IEC 61966-2-1 annexe F, coefficients de G.3.2) sur les
/// trois premiers canaux de chaque pixel.
fn sycc_to_rgb(data: &mut [u8], count: usize) {
    for px in data.chunks_exact_mut(count) {
        let y = f32::from(px[0]);
        let cb = f32::from(px[1]) - 128.0;
        let cr = f32::from(px[2]) - 128.0;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let to8 = |v: f32| (v + 0.5).floor().clamp(0.0, 255.0) as u8;
        px[0] = to8(y + 1.402 * cr);
        px[1] = to8(y - 0.344_13 * cb - 0.714_14 * cr);
        px[2] = to8(y + 1.772 * cb);
    }
}
