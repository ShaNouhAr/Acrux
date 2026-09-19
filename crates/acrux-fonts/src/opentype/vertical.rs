//! Métriques verticales : tables `vhea`, `vmtx` et `VORG`.
//!
//! Le japonais et le chinois se composent aussi **de haut en bas**, colonnes
//! de droite à gauche. Chaque glyphe a alors une avance verticale (la
//! hauteur qu'il occupe) et une origine verticale (le point d'où il est
//! dessiné), qui n'ont rien à voir avec les métriques horizontales.
//!
//! - `vhea` : en-tête, dont `numOfLongVerMetrics`.
//! - `vmtx` : avance verticale et « top side bearing » par glyphe, avec la
//!   même compression que `hmtx` (les derniers glyphes partagent la
//!   dernière avance).
//! - `VORG` : origine verticale des polices à contours CFF ; sans elle,
//!   l'origine se déduit de l'ascendante de `vhea`.
//!
//! La fonctionnalité OpenType qui remplace les glyphes par leur forme
//! verticale (`vert`, ou `vrt2` pour les formes pivotées) est activée par le
//! shaper, pas ici.

use crate::reader::{i16_at, u16_at};
use crate::truetype::TrueTypeFont;

/// Métriques verticales d'une police, en unités de police.
#[derive(Debug, Clone)]
pub struct VerticalMetrics {
    /// Ascendante verticale (moitié de la chasse d'un idéogramme, en
    /// général `units_per_em / 2`).
    pub ascender: i16,
    /// Descendante verticale, négative.
    pub descender: i16,
    /// Interligne supplémentaire entre deux colonnes.
    pub line_gap: i16,
    /// Nombre d'entrées complètes de `vmtx`.
    number_of_v_metrics: u16,
    /// Contenu de `vmtx`.
    vmtx: Vec<u8>,
    /// Origine verticale par défaut (`VORG`), si la table existe.
    default_vert_origin: Option<i16>,
    /// Origines verticales explicites, triées par glyphe.
    vert_origins: Vec<(u16, i16)>,
    /// Avance verticale de repli quand `vmtx` est absente.
    fallback_advance: u16,
}

impl VerticalMetrics {
    /// Lit les métriques verticales d'une police.
    ///
    /// Renvoie `None` si la police n'a pas de table `vhea` : elle n'est
    /// alors pas prévue pour l'écriture verticale, et l'appelant doit se
    /// rabattre sur une avance d'un em.
    #[must_use]
    pub fn read(font: &TrueTypeFont) -> Option<Self> {
        let vhea = font.table_bytes(*b"vhea")?;
        let ascender = i16_at(vhea, 4)?;
        let descender = i16_at(vhea, 6)?;
        let line_gap = i16_at(vhea, 8)?;
        let number_of_v_metrics = u16_at(vhea, 34)?;
        let vmtx = font.table_bytes(*b"vmtx").unwrap_or(&[]).to_vec();
        let (default_vert_origin, vert_origins) = font
            .table_bytes(*b"VORG")
            .and_then(read_vorg)
            .unwrap_or((None, Vec::new()));
        Some(Self {
            ascender,
            descender,
            line_gap,
            number_of_v_metrics,
            vmtx,
            default_vert_origin,
            vert_origins,
            fallback_advance: font.units_per_em(),
        })
    }

    /// Avance verticale d'un glyphe, en unités de police.
    ///
    /// Comme pour `hmtx`, les glyphes au-delà de `numOfLongVerMetrics`
    /// reprennent la dernière avance déclarée.
    #[must_use]
    pub fn advance(&self, gid: u16) -> u16 {
        let count = if self.number_of_v_metrics == 0 {
            u16::try_from(self.vmtx.len() / 4).unwrap_or(0)
        } else {
            self.number_of_v_metrics
        };
        if count == 0 {
            return self.fallback_advance;
        }
        let index = gid.min(count - 1);
        u16_at(&self.vmtx, usize::from(index) * 4).unwrap_or(self.fallback_advance)
    }

    /// « Top side bearing » d'un glyphe.
    #[must_use]
    pub fn top_side_bearing(&self, gid: u16) -> i16 {
        let count = self.number_of_v_metrics;
        if count == 0 {
            return 0;
        }
        if gid < count {
            return i16_at(&self.vmtx, usize::from(gid) * 4 + 2).unwrap_or(0);
        }
        let offset = usize::from(count) * 4 + usize::from(gid - count) * 2;
        i16_at(&self.vmtx, offset).unwrap_or(0)
    }

    /// Ordonnée de l'origine verticale d'un glyphe.
    ///
    /// C'est le point par lequel le glyphe est posé quand on écrit en
    /// colonne : `VORG` s'il existe, sinon l'ascendante verticale.
    #[must_use]
    pub fn vertical_origin(&self, gid: u16) -> i16 {
        if let Ok(index) = self.vert_origins.binary_search_by_key(&gid, |(g, _)| *g) {
            if let Some((_, y)) = self.vert_origins.get(index) {
                return *y;
            }
        }
        self.default_vert_origin.unwrap_or(self.ascender)
    }
}

/// Origine verticale par défaut et origines explicites triées par glyphe.
type VertOrigins = (Option<i16>, Vec<(u16, i16)>);

/// Lit la table `VORG` : origine par défaut et origines explicites.
fn read_vorg(data: &[u8]) -> Option<VertOrigins> {
    if u16_at(data, 0)? != 1 {
        return None;
    }
    let default = i16_at(data, 4)?;
    let count = usize::from(u16_at(data, 6)?);
    let mut out = Vec::with_capacity(count.min(4096));
    for i in 0..count.min(4096) {
        let base = 8 + i * 4;
        let (Some(gid), Some(y)) = (u16_at(data, base), i16_at(data, base + 2)) else {
            break;
        };
        out.push((gid, y));
    }
    out.sort_unstable_by_key(|(g, _)| *g);
    Some((Some(default), out))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Table `vhea` minimale avec `numOfLongVerMetrics`.
    fn vhea(count: u16) -> Vec<u8> {
        let mut t = vec![0u8; 36];
        t[4..6].copy_from_slice(&500i16.to_be_bytes());
        t[6..8].copy_from_slice(&(-500i16).to_be_bytes());
        t[8..10].copy_from_slice(&0i16.to_be_bytes());
        t[34..36].copy_from_slice(&count.to_be_bytes());
        t
    }

    /// Table `vmtx` : couples (avance, tsb).
    fn vmtx(entries: &[(u16, i16)]) -> Vec<u8> {
        let mut t = Vec::new();
        for (a, b) in entries {
            t.extend_from_slice(&a.to_be_bytes());
            t.extend_from_slice(&b.to_be_bytes());
        }
        t
    }

    fn font_with_vertical(count: u16, entries: &[(u16, i16)]) -> Vec<u8> {
        crate::truetype::tests::Builder::new()
            .table(*b"head", crate::truetype::tests::head_table(1000, false))
            .table(*b"maxp", crate::truetype::tests::maxp_table(4))
            .table(*b"loca", vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
            .table(*b"glyf", vec![0; 8])
            .table(*b"vhea", vhea(count))
            .table(*b"vmtx", vmtx(entries))
            .build(*b"\x00\x01\x00\x00")
    }

    #[test]
    fn reads_vhea_and_vmtx() {
        let data = font_with_vertical(2, &[(1000, 50), (900, 40)]);
        let font = TrueTypeFont::parse(&data).unwrap();
        let metrics = VerticalMetrics::read(&font).unwrap();
        assert_eq!(metrics.ascender, 500);
        assert_eq!(metrics.descender, -500);
        assert_eq!(metrics.advance(0), 1000);
        assert_eq!(metrics.advance(1), 900);
        // Au-delà : la dernière avance déclarée.
        assert_eq!(metrics.advance(3), 900);
        assert_eq!(metrics.top_side_bearing(0), 50);
        // Sans VORG, l'origine est l'ascendante.
        assert_eq!(metrics.vertical_origin(0), 500);
    }

    #[test]
    fn missing_vhea_gives_none() {
        let data = crate::truetype::tests::sample_font();
        let font = TrueTypeFont::parse(&data).unwrap();
        assert!(VerticalMetrics::read(&font).is_none());
    }

    #[test]
    fn vorg_is_read() {
        let mut t = Vec::new();
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&0u16.to_be_bytes());
        t.extend_from_slice(&880i16.to_be_bytes());
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&2u16.to_be_bytes());
        t.extend_from_slice(&700i16.to_be_bytes());
        let (default, origins) = read_vorg(&t).unwrap();
        assert_eq!(default, Some(880));
        assert_eq!(origins, vec![(2, 700)]);
        assert!(read_vorg(&[0, 2]).is_none());
    }
}
