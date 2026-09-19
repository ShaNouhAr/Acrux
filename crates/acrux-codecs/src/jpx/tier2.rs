//! Tier-2 (ISO/IEC 15444-1 annexe B) : ordre des paquets (B.12, avec les
//! changements de progression POC), en-têtes de paquets (B.10 : bit de
//! paquet vide, arbres d'étiquettes d'inclusion et de plans nuls,
//! nombre de passes, Lblock, longueurs de segments, bourrage de bits,
//! marqueurs SOP et EPH) et distribution des corps de paquets aux blocs.
//!
//! Une troncature ou une incohérence arrête simplement la lecture des
//! paquets : les blocs déjà reçus sont décodés et l'image est partielle.

use super::codestream::{marker, PocEntry, Progression};
use super::structure::{Precinct, PrecinctBand, Rect, TileComponent};
use super::tier1::{pass_terminates, Chunk};

/// Arbre d'étiquettes (B.10.2). Le niveau 0 est celui des feuilles ; chaque
/// niveau supérieur divise les dimensions par deux jusqu'à une racine.
pub(super) struct TagTree {
    /// (largeur, hauteur, indice du premier nœud) par niveau.
    levels: Vec<(usize, usize, usize)>,
    /// Borne inférieure courante et drapeau « valeur connue ».
    values: Vec<u32>,
    known: Vec<bool>,
}

impl TagTree {
    pub fn new(w: u32, h: u32) -> Self {
        let mut levels = Vec::new();
        let (mut lw, mut lh) = (
            usize::try_from(w).unwrap_or(0),
            usize::try_from(h).unwrap_or(0),
        );
        let mut total = 0usize;
        if lw > 0 && lh > 0 {
            loop {
                levels.push((lw, lh, total));
                total += lw * lh;
                if lw == 1 && lh == 1 {
                    break;
                }
                lw = lw.div_ceil(2);
                lh = lh.div_ceil(2);
            }
        }
        TagTree {
            levels,
            values: vec![0; total],
            known: vec![false; total],
        }
    }

    fn node(&self, level: usize, x: usize, y: usize) -> usize {
        let (w, _, base) = self.levels[level];
        base + (y >> level) * w + (x >> level)
    }

    /// Procédure de décodage (B.10.2) pour la feuille (x, y) avec le seuil
    /// `threshold` : rend vrai si la valeur est connue et strictement
    /// inférieure au seuil.
    pub fn decode(&mut self, reader: &mut BitReader<'_>, x: u32, y: u32, threshold: u32) -> bool {
        let (x, y) = (
            usize::try_from(x).unwrap_or(0),
            usize::try_from(y).unwrap_or(0),
        );
        if self.levels.is_empty() {
            return false;
        }
        let mut low = 0u32;
        for level in (0..self.levels.len()).rev() {
            let n = self.node(level, x, y);
            if self.values[n] < low {
                self.values[n] = low;
            }
            while !self.known[n] && self.values[n] < threshold {
                if reader.bit() == 1 {
                    self.known[n] = true;
                } else {
                    self.values[n] += 1;
                }
                if reader.overrun {
                    return false;
                }
            }
            low = self.values[n];
            if !self.known[n] {
                return false;
            }
        }
        low < threshold
    }

    /// Valeur (ou borne inférieure courante) de la feuille (x, y).
    pub fn value(&self, x: u32, y: u32) -> u32 {
        if self.levels.is_empty() {
            return 0;
        }
        let n = self.node(
            0,
            usize::try_from(x).unwrap_or(0),
            usize::try_from(y).unwrap_or(0),
        );
        self.values[n]
    }
}

/// Lecteur de bits d'en-tête de paquet (B.10.1) : après un octet `0xFF`,
/// l'octet suivant ne porte que 7 bits.
pub(super) struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u32,
    bits: u32,
    prev_ff: bool,
    /// Lecture au-delà des données.
    pub overrun: bool,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        BitReader {
            data,
            pos,
            buf: 0,
            bits: 0,
            prev_ff: false,
            overrun: false,
        }
    }

    pub fn bit(&mut self) -> u32 {
        if self.bits == 0 {
            let Some(&byte) = self.data.get(self.pos) else {
                self.overrun = true;
                return 0;
            };
            self.pos += 1;
            if self.prev_ff {
                self.bits = 7;
                self.buf = u32::from(byte & 0x7F);
            } else {
                self.bits = 8;
                self.buf = u32::from(byte);
            }
            self.prev_ff = byte == 0xFF;
        }
        self.bits -= 1;
        (self.buf >> self.bits) & 1
    }

    /// `n` bits (n ≤ 32), poids fort en tête.
    pub fn bits(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit();
        }
        v
    }

    /// Fin de l'en-tête : abandonne les bits restants et, si le dernier
    /// octet lu vaut `0xFF`, saute l'octet de bourrage qui suit.
    pub fn align(&mut self) -> usize {
        self.bits = 0;
        if self.prev_ff {
            self.pos += 1;
            self.prev_ff = false;
        }
        self.pos
    }
}

/// Paramètres de la tuile utiles au tier-2. Les marqueurs SOP et EPH sont
/// reconnus qu'ils soient annoncés ou non par le COD.
pub(super) struct TileCoding<'a> {
    pub progression: Progression,
    pub layers: u16,
    pub poc: &'a [PocEntry],
    /// Rectangle de la tuile sur la grille de référence.
    pub tile: Rect,
}

/// Volume de progression (B.12.1) : bornes exclusives.
struct Volume {
    res_start: usize,
    res_end: usize,
    comp_start: usize,
    comp_end: usize,
    layer_end: u16,
    order: Progression,
}

/// Contribution d'un bloc annoncée dans un en-tête, à lire dans le corps.
struct Pending {
    band: usize,
    block: usize,
    len: u32,
    passes: u32,
}

struct PacketReader<'a> {
    body: &'a [u8],
    body_pos: usize,
    packed: Option<&'a [u8]>,
    packed_pos: usize,
    pending: Vec<Pending>,
    /// Vrai dès qu'une anomalie impose d'arrêter.
    stop: bool,
}

/// Blocs annoncés : un `PrecinctBand` compte `blocks_wide × blocks_high`
/// blocs en ordre de balayage.
fn block_position(index: usize, blocks_wide: u32) -> (u32, u32) {
    let i = u32::try_from(index).unwrap_or(0);
    let w = blocks_wide.max(1);
    (i % w, i / w)
}

/// Lit tous les paquets de la tuile dans l'ordre de progression et
/// distribue leurs corps aux blocs de code.
pub(super) fn decode_packets(
    comps: &mut [TileComponent],
    body: &[u8],
    packed: Option<&[u8]>,
    coding: &TileCoding<'_>,
) {
    let mut reader = PacketReader {
        body,
        body_pos: 0,
        packed,
        packed_pos: 0,
        pending: Vec::new(),
        stop: false,
    };
    let max_res = comps.iter().map(|c| c.resolutions.len()).max().unwrap_or(0);
    let mut volumes: Vec<Volume> = coding
        .poc
        .iter()
        .map(|p| Volume {
            res_start: usize::from(p.res_start),
            res_end: usize::from(p.res_end).min(max_res),
            comp_start: usize::from(p.comp_start),
            comp_end: usize::from(p.comp_end).min(comps.len()),
            layer_end: p.layer_end.min(coding.layers),
            order: p.progression,
        })
        .collect();
    // Sans POC (ou après les volumes POC, pour les paquets restants) : le
    // volume complet dans l'ordre du COD.
    volumes.push(Volume {
        res_start: 0,
        res_end: max_res,
        comp_start: 0,
        comp_end: comps.len(),
        layer_end: coding.layers,
        order: coding.progression,
    });
    for vol in &volumes {
        if reader.stop {
            break;
        }
        run_volume(&mut reader, comps, coding.tile, vol);
    }
}

/// Parcourt un volume de progression (B.12.1.1 à B.12.1.5).
fn run_volume(
    reader: &mut PacketReader<'_>,
    comps: &mut [TileComponent],
    tile: Rect,
    vol: &Volume,
) {
    let nres = |comps: &[TileComponent], c: usize| comps[c].resolutions.len();
    let nprec = |comps: &[TileComponent], c: usize, r: usize| {
        usize::try_from(comps[c].resolutions[r].precinct_count()).unwrap_or(0)
    };
    match vol.order {
        Progression::Lrcp => {
            for l in 0..vol.layer_end {
                for r in vol.res_start..vol.res_end {
                    for c in vol.comp_start..vol.comp_end {
                        if r >= nres(comps, c) {
                            continue;
                        }
                        for p in 0..nprec(comps, c, r) {
                            if !reader.emit(comps, c, r, p, l) {
                                return;
                            }
                        }
                    }
                }
            }
        }
        Progression::Rlcp => {
            for r in vol.res_start..vol.res_end {
                for l in 0..vol.layer_end {
                    for c in vol.comp_start..vol.comp_end {
                        if r >= nres(comps, c) {
                            continue;
                        }
                        for p in 0..nprec(comps, c, r) {
                            if !reader.emit(comps, c, r, p, l) {
                                return;
                            }
                        }
                    }
                }
            }
        }
        Progression::Rpcl | Progression::Pcrl | Progression::Cprl => {
            // Ordres pilotés par la position : chaque précinct est ancré
            // sur la grille de référence (B.12.1.3), puis trié.
            let mut keys: Vec<(u64, u64, usize, usize, usize)> = Vec::new();
            for c in vol.comp_start..vol.comp_end {
                for r in vol.res_start..vol.res_end.min(nres(comps, c)) {
                    for p in 0..nprec(comps, c, r) {
                        let (x, y) = precinct_anchor(&comps[c], r, p, tile);
                        keys.push((y, x, c, r, p));
                    }
                }
            }
            match vol.order {
                Progression::Rpcl => keys.sort_unstable_by_key(|&(y, x, c, r, p)| (r, y, x, c, p)),
                Progression::Pcrl => keys.sort_unstable_by_key(|&(y, x, c, r, p)| (y, x, c, r, p)),
                _ => keys.sort_unstable_by_key(|&(y, x, c, r, p)| (c, y, x, r, p)),
            }
            for (_, _, c, r, p) in keys {
                for l in 0..vol.layer_end {
                    if !reader.emit(comps, c, r, p, l) {
                        return;
                    }
                }
            }
        }
    }
}

/// Ancre (x, y) du précinct `p` sur la grille de référence : origine de
/// sa partition ramenée à la grille, bornée par le coin de la tuile.
fn precinct_anchor(comp: &TileComponent, r: usize, p: usize, tile: Rect) -> (u64, u64) {
    let res = &comp.resolutions[r];
    let pw = u64::from(res.precincts_wide.max(1));
    let (px, py) = (p as u64 % pw, p as u64 / pw);
    let shift = u32::from(comp.style.levels) - u32::try_from(r).unwrap_or(0);
    let rx = ((u64::from(res.rect.x0 >> res.ppx) + px) << res.ppx) << shift;
    let ry = ((u64::from(res.rect.y0 >> res.ppy) + py) << res.ppy) << shift;
    (
        (rx * u64::from(comp.siz.xr)).max(u64::from(tile.x0)),
        (ry * u64::from(comp.siz.yr)).max(u64::from(tile.y0)),
    )
}

/// Nombre de passes de codage (tableau B.4).
fn read_pass_count(reader: &mut BitReader<'_>) -> u32 {
    if reader.bit() == 0 {
        return 1;
    }
    if reader.bit() == 0 {
        return 2;
    }
    let v = reader.bits(2);
    if v < 3 {
        return 3 + v;
    }
    let v = reader.bits(5);
    if v < 31 {
        return 6 + v;
    }
    37 + reader.bits(7)
}

impl PacketReader<'_> {
    /// Lit le paquet (couche `l`) du précinct `p` de la résolution `r` de
    /// la composante `c` s'il n'a pas déjà été lu. Rend faux s'il faut
    /// arrêter.
    fn emit(&mut self, comps: &mut [TileComponent], c: usize, r: usize, p: usize, l: u16) -> bool {
        if self.stop {
            return false;
        }
        let comp = &mut comps[c];
        let style = comp.style.cbstyle;
        let res = &mut comp.resolutions[r];
        let Some(precinct) = res.precincts.get_mut(p) else {
            return true;
        };
        if precinct.layers_done != l {
            return true;
        }
        precinct.layers_done += 1;
        self.read_packet(precinct, l, style);
        !self.stop
    }

    /// Lit un paquet (B.10) : SOP éventuel, en-tête (en ligne ou dans le
    /// flux PPM / PPT), EPH éventuel, puis les morceaux du corps.
    fn read_packet(&mut self, precinct: &mut Precinct, layer: u16, style: u8) {
        // Marqueur SOP éventuel (A.8.1) : FF91, Lsop = 4, Nsop. Un en-tête
        // de paquet ne peut pas commencer par ces deux octets (bourrage
        // après 0xFF), la détection est donc sûre même sans le drapeau.
        if self.body.get(self.body_pos..self.body_pos + 2) == Some(&marker::SOP.to_be_bytes()) {
            self.body_pos += 6;
        }
        if self.body_pos > self.body.len() {
            self.stop = true;
            return;
        }
        self.pending.clear();
        let mut reader = match self.packed {
            Some(packed) => BitReader::new(packed, self.packed_pos),
            None => BitReader::new(self.body, self.body_pos),
        };
        if reader.bit() == 1 {
            for (band_index, pb) in precinct.bands.iter_mut().enumerate() {
                for block_index in 0..pb.blocks.len() {
                    let at = BlockRef {
                        band: band_index,
                        block: block_index,
                        layer,
                        style,
                    };
                    if !self.read_block_header(&mut reader, pb, at) {
                        self.stop = true;
                        return;
                    }
                }
            }
        }
        let mut pos = reader.align();
        if reader.overrun {
            self.stop = true;
            return;
        }
        // EPH (A.8.2) dans le flux d'en-têtes.
        // Annoncé par le COD ou non, il est reconnu s'il est présent.
        let header_stream = self.packed.unwrap_or(self.body);
        if header_stream.get(pos..pos + 2) == Some(&marker::EPH.to_be_bytes()) {
            pos += 2;
        }
        if self.packed.is_some() {
            self.packed_pos = pos;
        } else {
            self.body_pos = pos;
        }
        // Corps du paquet : un morceau par contribution.
        for pend in &self.pending {
            let start = self.body_pos;
            let len = usize::try_from(pend.len).unwrap_or(usize::MAX);
            let end = start.saturating_add(len);
            let truncated = end > self.body.len();
            let end = end.min(self.body.len());
            let cb = &mut precinct.bands[pend.band].blocks[pend.block];
            cb.chunks.push(Chunk {
                start,
                end,
                passes: pend.passes,
            });
            cb.passes += pend.passes;
            self.body_pos = end;
            if truncated {
                self.stop = true;
                return;
            }
        }
    }

    /// Contribution d'un bloc dans l'en-tête (B.10.4 à B.10.7) : inclusion,
    /// plans nuls, nombre de passes, Lblock et longueurs des segments. Rend
    /// faux si l'en-tête est tronqué ou incohérent.
    fn read_block_header(
        &mut self,
        reader: &mut BitReader<'_>,
        pb: &mut PrecinctBand,
        at: BlockRef,
    ) -> bool {
        let (bx, by) = block_position(at.block, pb.blocks_wide);
        let cb = &mut pb.blocks[at.block];
        // Inclusion (B.10.4).
        let included = if cb.included {
            reader.bit() == 1
        } else {
            pb.inclusion.decode(reader, bx, by, u32::from(at.layer) + 1)
        };
        if reader.overrun {
            return false;
        }
        if !included {
            return true;
        }
        if !cb.included {
            cb.included = true;
            // Plans de bits nuls (B.10.5) : arbre décodé jusqu'au bout.
            let mut t = 1u32;
            while !pb.zero_planes.decode(reader, bx, by, t) {
                t += 1;
                if t > 80 || reader.overrun {
                    return false;
                }
            }
            cb.zero_planes = u8::try_from(pb.zero_planes.value(bx, by)).unwrap_or(255);
            cb.lblock = 3;
        }
        let passes = read_pass_count(reader);
        // Lblock (B.10.7.1).
        while reader.bit() == 1 {
            cb.lblock = cb.lblock.saturating_add(1);
            if cb.lblock > 48 || reader.overrun {
                return false;
            }
        }
        // Une longueur par segment terminé, plus la fin (B.10.7).
        let end = cb.passes + passes;
        let mut count = 0u32;
        for i in cb.passes..end {
            count += 1;
            if pass_terminates(i, at.style) || i + 1 == end {
                let bits = u32::from(cb.lblock) + count.ilog2();
                if bits > 31 {
                    return false;
                }
                let len = reader.bits(bits);
                self.pending.push(Pending {
                    band: at.band,
                    block: at.block,
                    len,
                    passes: count,
                });
                count = 0;
            }
        }
        !reader.overrun
    }
}

/// Bloc en cours de lecture dans un en-tête de paquet.
#[derive(Clone, Copy)]
struct BlockRef {
    band: usize,
    block: usize,
    layer: u16,
    style: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_reader_stuffs_after_ff_and_aligns() {
        let data = [0xFF, 0x55, 0x80];
        let mut r = BitReader::new(&data, 0);
        assert_eq!(r.bits(8), 0xFF);
        // L'octet suivant n'a que 7 bits : 0x55 & 0x7F = 1010101.
        assert_eq!(r.bits(7), 0x55);
        assert_eq!(r.bit(), 1);
        assert!(!r.overrun);
        assert_eq!(r.bit(), 0);
        let end = r.align();
        assert_eq!(end, 3);
        let mut r2 = BitReader::new(&data, 0);
        r2.bits(8);
        // Dernier octet lu = 0xFF : l'alignement saute l'octet bourré.
        assert_eq!(r2.align(), 2);
        let mut r3 = BitReader::new(&data, 3);
        assert_eq!(r3.bit(), 0);
        assert!(r3.overrun);
    }

    #[test]
    fn pass_count_codewords_match_table_b4() {
        // (octets, bits à sauter d'abord, valeur attendue).
        let cases: [(&[u8], u32, u32); 5] = [
            (&[0b0000_0000], 0, 1),
            (&[0b1000_0000], 0, 2),
            (&[0b1101_0000], 0, 4),
            (&[0b1111_0010, 0b0000_0000], 0, 6 + 4),
            // 0 puis 1111 11111 0000101 : le 0 de tête évite un octet 0xFF.
            (&[0b0111_1111, 0b1100_0010, 0b1000_0000], 1, 37 + 5),
        ];
        for (bytes, skip, expected) in cases {
            let mut r = BitReader::new(bytes, 0);
            r.bits(skip);
            assert_eq!(read_pass_count(&mut r), expected);
        }
    }

    #[test]
    fn tag_tree_decodes_known_encoding() {
        // Arbre 1×1 de valeur 2 : bits 0 0 1.
        let data = [0b0010_0000];
        let mut tree = TagTree::new(1, 1);
        let mut r = BitReader::new(&data, 0);
        assert!(!tree.decode(&mut r, 0, 0, 1));
        assert!(!tree.decode(&mut r, 0, 0, 2));
        assert!(tree.decode(&mut r, 0, 0, 3));
        assert_eq!(tree.value(0, 0), 2);
        // Arbre 2×1 : racine 1 (bits 0 1), feuille 0 → 1 (bit 1), feuille 1 → 3 (0 0 1).
        let data = [0b0110_0100, 0b0000_0000];
        let mut tree = TagTree::new(2, 1);
        let mut r = BitReader::new(&data, 0);
        assert!(tree.decode(&mut r, 0, 0, 5));
        assert_eq!(tree.value(0, 0), 1);
        assert!(tree.decode(&mut r, 1, 0, 5));
        assert_eq!(tree.value(1, 0), 3);
    }

    #[test]
    fn empty_tag_tree_is_harmless() {
        let mut tree = TagTree::new(0, 0);
        let mut r = BitReader::new(&[], 0);
        assert!(!tree.decode(&mut r, 0, 0, 1));
        assert_eq!(tree.value(0, 0), 0);
    }
}
