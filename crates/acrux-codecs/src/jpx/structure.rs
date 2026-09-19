//! Découpage d'une tuile (ISO/IEC 15444-1 annexe B) : composantes de tuile
//! (B.3), résolutions (B.5), sous-bandes (B.5, équation B-15), précincts
//! (B.6) et blocs de code (B.7), avec les paramètres de quantification de
//! chaque sous-bande (E.1).
//!
//! Les structures construites ici sont remplies par le tier-2 (données des
//! blocs) puis consommées par le tier-1 et la transformée inverse.

use acrux_core::Result;

use super::codestream::{CodingStyle, ComponentSiz, Quant, QuantStyle};
use super::corrupt;
use super::tier1::{BandKind, Chunk};
use super::tier2::TagTree;

/// Nombre maximal de précincts par tuile (protection mémoire).
pub(super) const MAX_PRECINCTS: u64 = 1 << 20;
/// Nombre maximal de blocs de code par tuile (protection mémoire).
pub(super) const MAX_CODEBLOCKS: u64 = 1 << 22;

/// Rectangle demi-ouvert `[x0, x1) × [y0, y1)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Rect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Rect {
    pub fn width(&self) -> u32 {
        self.x1.saturating_sub(self.x0)
    }

    pub fn height(&self) -> u32 {
        self.y1.saturating_sub(self.y0)
    }

    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn area(&self) -> u64 {
        u64::from(self.width()) * u64::from(self.height())
    }

    /// Rectangle réduit de `d` niveaux : ⌈x / 2^d⌉ sur chaque bord.
    pub fn reduced(&self, d: u32) -> Rect {
        Rect {
            x0: ceil_shift(self.x0, d),
            y0: ceil_shift(self.y0, d),
            x1: ceil_shift(self.x1, d),
            y1: ceil_shift(self.y1, d),
        }
    }
}

/// ⌈v / 2^d⌉ (B-14) sans débordement.
pub(super) fn ceil_shift(v: u32, d: u32) -> u32 {
    if d >= 32 {
        return u32::from(v != 0);
    }
    u32::try_from((u64::from(v) + (1u64 << d) - 1) >> d).unwrap_or(u32::MAX)
}

/// ⌈a / b⌉ pour `b > 0`, `a` pouvant être négatif.
fn ceil_div(a: i64, b: i64) -> i64 {
    -((-a).div_euclid(b))
}

/// Coefficients d'une sous-bande : entiers (5-3) ou flottants (9-7), ou
/// pas encore alloués.
pub(super) enum Coeffs {
    Empty,
    Int(Vec<i32>),
    Float(Vec<f32>),
}

/// Sous-bande d'une résolution.
pub(super) struct Band {
    pub kind: BandKind,
    /// Rectangle de la sous-bande (tbx0, tby0, tbx1, tby1).
    pub rect: Rect,
    /// Exposant εb et mantisse μb du pas de quantification (E.1.1).
    pub epsilon: u8,
    pub mu: u16,
    /// log2 du gain nominal de la sous-bande (tableau E.1).
    pub gain_log2: u8,
    pub coeffs: Coeffs,
}

/// Bloc de code et l'état de sa réception paquet par paquet (B.10).
pub(super) struct CodeBlock {
    /// Rectangle en coordonnées de la sous-bande.
    pub rect: Rect,
    /// Lblock (B.10.7.1), 3 initialement.
    pub lblock: u8,
    /// Nombre de plans de bits nuls en tête P (B.10.5).
    pub zero_planes: u8,
    /// Passes de codage reçues.
    pub passes: u32,
    /// Déjà inclus dans une couche.
    pub included: bool,
    pub chunks: Vec<Chunk>,
}

/// Portion d'un précinct dans une sous-bande.
pub(super) struct PrecinctBand {
    /// Blocs par ligne ; le nombre de lignes est `blocks.len() / blocks_wide`.
    pub blocks_wide: u32,
    pub inclusion: TagTree,
    pub zero_planes: TagTree,
    /// Blocs en ordre de balayage (ligne par ligne).
    pub blocks: Vec<CodeBlock>,
}

/// Précinct d'une résolution.
pub(super) struct Precinct {
    /// Une entrée pour LL (résolution 0) ou trois (HL, LH, HH).
    pub bands: Vec<PrecinctBand>,
    /// Couches déjà reçues (progression, B.12).
    pub layers_done: u16,
}

/// Résolution d'une composante de tuile.
pub(super) struct Resolution {
    /// (trx0, try0, trx1, try1).
    pub rect: Rect,
    pub ppx: u8,
    pub ppy: u8,
    pub precincts_wide: u32,
    pub precincts_high: u32,
    pub bands: Vec<Band>,
    pub precincts: Vec<Precinct>,
}

impl Resolution {
    pub fn precinct_count(&self) -> u32 {
        self.precincts_wide.saturating_mul(self.precincts_high)
    }
}

/// Composante de tuile.
pub(super) struct TileComponent {
    /// (tcx0, tcy0, tcx1, tcy1).
    pub rect: Rect,
    pub siz: ComponentSiz,
    pub style: CodingStyle,
    pub quant: Quant,
    pub roi_shift: u8,
    pub resolutions: Vec<Resolution>,
}

/// Compteurs de structures alloués pour une tuile.
#[derive(Default)]
pub(super) struct Budget {
    pub precincts: u64,
    pub blocks: u64,
}

/// Paramètres de quantification (εb, μb) de la sous-bande d'indice
/// `band_index` (0 = LL, puis 3(r−1)+1..3 pour HL, LH, HH) de la
/// résolution `r` (E.1.1, équation E-5 pour le style dérivé).
fn band_quant(quant: &Quant, r: u8, band_index: usize, levels: u8) -> (u8, u16) {
    match quant.style {
        QuantStyle::Derived => {
            let (e0, mu0) = quant.steps.first().copied().unwrap_or((0, 0));
            // εb = ε0 − NL + nb, avec nb = NL pour LL et NL − r + 1 sinon.
            let e = if r == 0 {
                e0
            } else {
                e0.saturating_sub(r).saturating_add(1)
            };
            let _ = levels;
            (e, mu0)
        }
        QuantStyle::None | QuantStyle::Expounded => quant
            .steps
            .get(band_index)
            .or(quant.steps.last())
            .copied()
            .unwrap_or((0, 0)),
    }
}

/// Construit la composante de tuile `comp` de la tuile `tile` (grille de
/// référence) avec ses résolutions, sous-bandes, précincts et blocs.
pub(super) fn build_tile_component(
    tile: Rect,
    siz: ComponentSiz,
    style: &CodingStyle,
    quant: &Quant,
    roi_shift: u8,
    budget: &mut Budget,
) -> Result<TileComponent> {
    let rect = Rect {
        x0: tile.x0.div_ceil(siz.xr),
        y0: tile.y0.div_ceil(siz.yr),
        x1: tile.x1.div_ceil(siz.xr),
        y1: tile.y1.div_ceil(siz.yr),
    };
    let levels = style.levels;
    let mut resolutions = Vec::with_capacity(usize::from(levels) + 1);
    for r in 0..=levels {
        let res = build_resolution(rect, r, style, quant, budget)?;
        resolutions.push(res);
    }
    Ok(TileComponent {
        rect,
        siz,
        style: style.clone(),
        quant: quant.clone(),
        roi_shift,
        resolutions,
    })
}

/// Rectangle d'une sous-bande (équation B-15) : nb niveaux de décomposition,
/// (xob, yob) ∈ {0, 1}².
fn band_rect(tc: Rect, nb: u32, xob: i64, yob: i64) -> Rect {
    let den = 1i64 << nb;
    let off = if nb == 0 { 0 } else { 1i64 << (nb - 1) };
    let f = |v: u32, ob: i64| {
        u32::try_from(ceil_div(i64::from(v) - off * ob, den).max(0)).unwrap_or(u32::MAX)
    };
    Rect {
        x0: f(tc.x0, xob),
        y0: f(tc.y0, yob),
        x1: f(tc.x1, xob),
        y1: f(tc.y1, yob),
    }
}

fn build_resolution(
    tc: Rect,
    r: u8,
    style: &CodingStyle,
    quant: &Quant,
    budget: &mut Budget,
) -> Result<Resolution> {
    let levels = style.levels;
    let rect = tc.reduced(u32::from(levels - r));
    let (ppx, ppy) = style
        .precincts
        .get(usize::from(r))
        .copied()
        .unwrap_or((15, 15));
    // Sous-bandes (B.5) et leurs paramètres de quantification.
    let mut bands = Vec::with_capacity(3);
    if r == 0 {
        let (epsilon, mu) = band_quant(quant, 0, 0, levels);
        bands.push(Band {
            kind: BandKind::LL,
            rect: band_rect(tc, u32::from(levels), 0, 0),
            epsilon,
            mu,
            gain_log2: 0,
            coeffs: Coeffs::Empty,
        });
    } else {
        let nb = u32::from(levels - r + 1);
        for (i, (kind, xob, yob, gain)) in [
            (BandKind::HL, 1, 0, 1u8),
            (BandKind::LH, 0, 1, 1),
            (BandKind::HH, 1, 1, 2),
        ]
        .into_iter()
        .enumerate()
        {
            let (epsilon, mu) = band_quant(quant, r, 3 * (usize::from(r) - 1) + 1 + i, levels);
            bands.push(Band {
                kind,
                rect: band_rect(tc, nb, xob, yob),
                epsilon,
                mu,
                gain_log2: gain,
                coeffs: Coeffs::Empty,
            });
        }
    }
    // Précincts (B.6) : partition de la résolution ancrée en 0.
    let (pw, ph) = if rect.is_empty() {
        (0, 0)
    } else {
        (
            ceil_shift(rect.x1, u32::from(ppx)) - (rect.x0 >> ppx),
            ceil_shift(rect.y1, u32::from(ppy)) - (rect.y0 >> ppy),
        )
    };
    budget.precincts += u64::from(pw) * u64::from(ph);
    if budget.precincts > MAX_PRECINCTS {
        return Err(corrupt("trop de précincts"));
    }
    // Taille des blocs dans les sous-bandes (B.7, équations B-17 et B-18).
    let (scale_x, scale_y) = if r == 0 {
        (ppx, ppy)
    } else {
        (ppx.saturating_sub(1), ppy.saturating_sub(1))
    };
    let cbw = style.xcb.min(scale_x);
    let cbh = style.ycb.min(scale_y);
    let mut precincts = Vec::with_capacity(usize::try_from(pw * ph).unwrap_or(0));
    for py in 0..ph {
        for px in 0..pw {
            let mut pbands = Vec::with_capacity(bands.len());
            for band in &bands {
                // Rectangle du précinct projeté dans la sous-bande.
                let bx0 = (u64::from(rect.x0 >> ppx) + u64::from(px)) << scale_x;
                let by0 = (u64::from(rect.y0 >> ppy) + u64::from(py)) << scale_y;
                let pb = Rect {
                    x0: clamp_u32(bx0).max(band.rect.x0),
                    y0: clamp_u32(by0).max(band.rect.y0),
                    x1: clamp_u32(bx0 + (1u64 << scale_x)).min(band.rect.x1),
                    y1: clamp_u32(by0 + (1u64 << scale_y)).min(band.rect.y1),
                };
                pbands.push(build_precinct_band(pb, cbw, cbh, budget)?);
            }
            precincts.push(Precinct {
                bands: pbands,
                layers_done: 0,
            });
        }
    }
    Ok(Resolution {
        rect,
        ppx,
        ppy,
        precincts_wide: pw,
        precincts_high: ph,
        bands,
        precincts,
    })
}

fn clamp_u32(v: u64) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// Blocs de code d'un précinct dans une sous-bande (B.7).
fn build_precinct_band(pb: Rect, cbw: u8, cbh: u8, budget: &mut Budget) -> Result<PrecinctBand> {
    if pb.is_empty() {
        return Ok(PrecinctBand {
            blocks_wide: 0,
            inclusion: TagTree::new(0, 0),
            zero_planes: TagTree::new(0, 0),
            blocks: Vec::new(),
        });
    }
    let cx0 = pb.x0 >> cbw;
    let cx1 = ceil_shift(pb.x1, u32::from(cbw));
    let cy0 = pb.y0 >> cbh;
    let cy1 = ceil_shift(pb.y1, u32::from(cbh));
    let (bw, bh) = (cx1 - cx0, cy1 - cy0);
    budget.blocks += u64::from(bw) * u64::from(bh);
    if budget.blocks > MAX_CODEBLOCKS {
        return Err(corrupt("trop de blocs de code"));
    }
    let mut blocks = Vec::with_capacity(usize::try_from(bw * bh).unwrap_or(0));
    for cy in cy0..cy1 {
        for cx in cx0..cx1 {
            blocks.push(CodeBlock {
                rect: Rect {
                    x0: (cx << cbw).max(pb.x0),
                    y0: (cy << cbh).max(pb.y0),
                    x1: clamp_u32((u64::from(cx) + 1) << cbw).min(pb.x1),
                    y1: clamp_u32((u64::from(cy) + 1) << cbh).min(pb.y1),
                },
                lblock: 3,
                zero_planes: 0,
                passes: 0,
                included: false,
                chunks: Vec::new(),
            });
        }
    }
    Ok(PrecinctBand {
        blocks_wide: bw,
        inclusion: TagTree::new(bw, bh),
        zero_planes: TagTree::new(bw, bh),
        blocks,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn style(levels: u8) -> CodingStyle {
        CodingStyle {
            levels,
            xcb: 6,
            ycb: 6,
            cbstyle: 0,
            reversible: true,
            precincts: vec![(15, 15); usize::from(levels) + 1],
        }
    }

    fn quant() -> Quant {
        Quant {
            guard: 2,
            style: QuantStyle::None,
            steps: vec![(8, 0); 16],
        }
    }

    #[test]
    fn band_rects_follow_equation_b15() {
        let tc = Rect {
            x0: 0,
            y0: 0,
            x1: 37,
            y1: 29,
        };
        // Un niveau : LL 19×15, HL 18×15, LH 19×14, HH 18×14.
        assert_eq!(band_rect(tc, 1, 0, 0).x1, 19);
        assert_eq!(band_rect(tc, 1, 1, 0).x1, 18);
        assert_eq!(band_rect(tc, 1, 0, 1).y1, 14);
        // Origine non nulle : tcx0 = 5, nb = 1, xob = 1 → ⌈(5 − 1) / 2⌉ = 2.
        let tc2 = Rect {
            x0: 5,
            y0: 0,
            x1: 37,
            y1: 29,
        };
        assert_eq!(band_rect(tc2, 1, 1, 0).x0, 2);
        assert_eq!(band_rect(tc2, 1, 0, 0).x0, 3);
    }

    #[test]
    fn tile_component_partition_counts_blocks() {
        let tile = Rect {
            x0: 0,
            y0: 0,
            x1: 130,
            y1: 70,
        };
        let siz = ComponentSiz {
            precision: 8,
            signed: false,
            xr: 1,
            yr: 1,
        };
        let mut budget = Budget::default();
        let tc = build_tile_component(tile, siz, &style(2), &quant(), 0, &mut budget).unwrap();
        assert_eq!(tc.resolutions.len(), 3);
        let r0 = &tc.resolutions[0];
        assert_eq!(r0.precinct_count(), 1);
        // LL 33×18 → un bloc 33×18.
        assert_eq!(r0.precincts[0].bands[0].blocks.len(), 1);
        let r2 = &tc.resolutions[2];
        // HL de la résolution 2 : 65×35 → 2×1 blocs de 64.
        assert_eq!(r2.precincts[0].bands[0].blocks_wide, 2);
        assert_eq!(r2.precincts[0].bands[0].blocks.len(), 2);
        assert_eq!(budget.blocks, 1 + 3 + 6);
    }

    #[test]
    fn custom_precincts_split_resolutions() {
        let tile = Rect {
            x0: 0,
            y0: 0,
            x1: 100,
            y1: 100,
        };
        let siz = ComponentSiz {
            precision: 8,
            signed: false,
            xr: 1,
            yr: 1,
        };
        let mut s = style(1);
        s.precincts = vec![(4, 4), (5, 5)];
        let mut budget = Budget::default();
        let tc = build_tile_component(tile, siz, &s, &quant(), 0, &mut budget).unwrap();
        // Résolution 1 : 100 / 32 → 4 précincts de large.
        assert_eq!(tc.resolutions[1].precincts_wide, 4);
        // Blocs limités à 2^(PPx − 1) = 16 dans les sous-bandes.
        let first = &tc.resolutions[1].precincts[0].bands[0].blocks[0];
        assert_eq!(first.rect.width(), 16);
        // Résolution 0 (50×50) : précincts de 16 → 4×4.
        assert_eq!(tc.resolutions[0].precinct_count(), 16);
    }
}
