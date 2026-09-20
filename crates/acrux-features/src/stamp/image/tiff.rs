//! Lecture des images TIFF, **y compris multipages**.
//!
//! C'est le format de sortie de la plupart des scanners : un fichier, une
//! page par feuille, le plus souvent en noir et blanc comprimé en CCITT
//! Groupe 4 — exactement ce que nos décodeurs de flux PDF savent déjà lire.
//! Ce module n'est donc guère qu'un lecteur d'étiquettes posé sur
//! `acrux-codecs`.
//!
//! Sont pris en charge :
//!
//! - les deux boutismes (`II` et `MM`), et la chaîne des IFD : **chaque IFD
//!   devient une page** ;
//! - les compressions 1 (aucune), 2 (CCITT Huffman modifié), 3 (Groupe 3),
//!   4 (Groupe 4), 5 (LZW), 7 (JPEG, tables communes comprises), 8 et 32946
//!   (Deflate), 32773 (PackBits) ;
//! - 1, 2, 4, 8 et 16 bits par composante, 1, 3 ou 4 composantes ;
//! - les interprétations 0 (blanc = zéro), 1 (noir = zéro), 2 (RVB),
//!   3 (palette) et 5 (CMJN, converti en RVB) ;
//! - le prédicteur horizontal (étiquette 317) ;
//! - un canal supplémentaire déclaré `/ExtraSamples` comme alpha.
//!
//! Les images **en tuiles** et les plans séparés (`PlanarConfiguration` 2)
//! sont refusés avec un message clair : ils demandent un autre assemblage, et
//! aucun scanner n'en produit.

use acrux_core::{Error, Result};

use super::Raster;

fn corrupt(what: &str) -> Error {
    Error::Corrupt(format!("TIFF : {what}"))
}

/// Une entrée d'IFD, telle qu'elle est écrite dans le fichier.
struct Field {
    kind: u16,
    count: u32,
    /// Les quatre octets de la valeur, ou l'adresse où elle se trouve.
    payload: [u8; 4],
}

/// Le fichier et son boutisme, pour lire des nombres sans se répéter.
struct Reader<'a> {
    data: &'a [u8],
    /// Vrai si les nombres sont écrits petit-boutiste (`II`).
    little: bool,
}

impl Reader<'_> {
    fn u16_at(&self, at: usize) -> Result<u16> {
        let b = self
            .data
            .get(at..at + 2)
            .ok_or_else(|| corrupt("fichier tronqué"))?;
        Ok(if self.little {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    }

    fn u32_at(&self, at: usize) -> Result<u32> {
        let b = self
            .data
            .get(at..at + 4)
            .ok_or_else(|| corrupt("fichier tronqué"))?;
        Ok(if self.little {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    }

    /// Valeurs entières d'une entrée, quelle que soit sa taille.
    fn values(&self, f: &Field) -> Result<Vec<u64>> {
        let width = match f.kind {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 => 4,
            5 | 10 => 8,
            _ => return Ok(Vec::new()),
        };
        let count = f.count as usize;
        let total = count * width;
        // Jusqu'à quatre octets, la valeur tient dans l'entrée elle-même.
        let bytes: &[u8] = if total <= 4 {
            &f.payload
        } else {
            let at = self.payload_offset(f) as usize;
            self.data
                .get(at..at + total)
                .ok_or_else(|| corrupt("valeur hors du fichier"))?
        };
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let at = i * width;
            let v = match width {
                1 => u64::from(bytes[at]),
                2 => u64::from(if self.little {
                    u16::from_le_bytes([bytes[at], bytes[at + 1]])
                } else {
                    u16::from_be_bytes([bytes[at], bytes[at + 1]])
                }),
                _ => u64::from(if self.little {
                    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
                } else {
                    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
                }),
            };
            out.push(v);
        }
        Ok(out)
    }

    /// Adresse où la valeur d'une entrée est rangée.
    fn payload_offset(&self, f: &Field) -> u32 {
        if self.little {
            u32::from_le_bytes(f.payload)
        } else {
            u32::from_be_bytes(f.payload)
        }
    }
}

/// Les étiquettes d'une page, rangées par numéro.
type Ifd = std::collections::BTreeMap<u16, Field>;

/// Décode toutes les pages d'un TIFF.
///
/// # Errors
/// Signature absente, étiquette obligatoire manquante, compression ou
/// disposition hors du domaine pris en charge.
pub(crate) fn rasters(data: &[u8]) -> Result<Vec<Raster>> {
    let little = match data.get(..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return Err(corrupt("signature absente")),
    };
    let r = Reader { data, little };
    if r.u16_at(2)? != 42 {
        return Err(corrupt("nombre magique inattendu"));
    }
    let mut at = r.u32_at(4)? as usize;
    let mut out = Vec::new();
    let mut seen = Vec::new();
    while at != 0 {
        if seen.contains(&at) || out.len() > 10_000 {
            return Err(corrupt("chaîne d'IFD circulaire"));
        }
        seen.push(at);
        let (ifd, next) = read_ifd(&r, at)?;
        out.push(page(&r, &ifd)?);
        at = next;
    }
    if out.is_empty() {
        return Err(corrupt("aucune page"));
    }
    Ok(out)
}

/// Lit un IFD et rend l'adresse du suivant.
fn read_ifd(r: &Reader<'_>, at: usize) -> Result<(Ifd, usize)> {
    let count = usize::from(r.u16_at(at)?);
    let mut ifd = Ifd::new();
    for i in 0..count {
        let e = at + 2 + i * 12;
        let tag = r.u16_at(e)?;
        let kind = r.u16_at(e + 2)?;
        let n = r.u32_at(e + 4)?;
        let bytes = r
            .data
            .get(e + 8..e + 12)
            .ok_or_else(|| corrupt("entrée tronquée"))?;
        ifd.insert(
            tag,
            Field {
                kind,
                count: n,
                payload: [bytes[0], bytes[1], bytes[2], bytes[3]],
            },
        );
    }
    let next = r.u32_at(at + 2 + count * 12)? as usize;
    Ok((ifd, next))
}

/// Première valeur d'une étiquette, ou une valeur par défaut.
fn one(r: &Reader<'_>, ifd: &Ifd, tag: u16, default: u64) -> u64 {
    ifd.get(&tag)
        .and_then(|f| r.values(f).ok())
        .and_then(|v| v.first().copied())
        .unwrap_or(default)
}

/// Toutes les valeurs d'une étiquette.
fn many(r: &Reader<'_>, ifd: &Ifd, tag: u16) -> Vec<u64> {
    ifd.get(&tag)
        .and_then(|f| r.values(f).ok())
        .unwrap_or_default()
}

/// Ce qu'il faut savoir d'une page pour la décoder.
struct Layout {
    width: u32,
    height: u32,
    bits: u32,
    samples: u32,
    photometric: u64,
    compression: u64,
    predictor: u64,
    /// Table de couleurs développée en RVB.
    palette: Vec<[u8; 3]>,
    /// Vrai si la dernière composante est un canal alpha.
    alpha: bool,
}

/// Décode une page.
fn page(r: &Reader<'_>, ifd: &Ifd) -> Result<Raster> {
    if ifd.contains_key(&322) {
        return Err(Error::Unsupported(
            "TIFF en tuiles : non pris en charge".into(),
        ));
    }
    if one(r, ifd, 284, 1) != 1 {
        return Err(Error::Unsupported(
            "TIFF à plans séparés : non pris en charge".into(),
        ));
    }
    let l = layout(r, ifd)?;
    let body = strips(r, ifd, &l)?;
    Ok(assemble(&l, &body))
}

/// Relève la disposition de la page.
fn layout(r: &Reader<'_>, ifd: &Ifd) -> Result<Layout> {
    let width = u32::try_from(one(r, ifd, 256, 0)).map_err(|_| corrupt("largeur démesurée"))?;
    let height = u32::try_from(one(r, ifd, 257, 0)).map_err(|_| corrupt("hauteur démesurée"))?;
    if width == 0 || height == 0 {
        return Err(corrupt("dimensions nulles"));
    }
    let bits_all = many(r, ifd, 258);
    let bits = u32::try_from(bits_all.first().copied().unwrap_or(1)).unwrap_or(1);
    if bits_all.iter().any(|b| *b != u64::from(bits)) {
        return Err(Error::Unsupported(
            "TIFF : composantes de tailles différentes".into(),
        ));
    }
    if !matches!(bits, 1 | 2 | 4 | 8 | 16) {
        return Err(Error::Unsupported(format!(
            "TIFF : {bits} bits par composante"
        )));
    }
    let samples = u32::try_from(one(r, ifd, 277, 1)).unwrap_or(1);
    if !(1..=4).contains(&samples) {
        return Err(Error::Unsupported(format!(
            "TIFF : {samples} composantes par pixel"
        )));
    }
    let photometric = one(r, ifd, 262, 1);
    if !matches!(photometric, 0 | 1 | 2 | 3 | 5) {
        return Err(Error::Unsupported(format!(
            "TIFF : interprétation {photometric} non prise en charge"
        )));
    }
    // La table de couleurs est écrite sur seize bits, canal après canal.
    let map = many(r, ifd, 320);
    let mut palette = Vec::new();
    if photometric == 3 && !map.is_empty() {
        let third = map.len() / 3;
        for i in 0..third {
            let at = |k: usize| u8::try_from(map[k * third + i] >> 8).unwrap_or(0);
            palette.push([at(0), at(1), at(2)]);
        }
    }
    // Un canal supplémentaire non déclaré « sans signification » est un alpha.
    let extra = many(r, ifd, 338);
    let alpha = samples == 4 && photometric != 5 || (samples == 2 && !extra.is_empty());
    Ok(Layout {
        width,
        height,
        bits,
        samples,
        photometric,
        compression: one(r, ifd, 259, 1),
        predictor: one(r, ifd, 317, 1),
        palette,
        alpha,
    })
}

/// Décompresse les bandes et les met bout à bout.
fn strips(r: &Reader<'_>, ifd: &Ifd, l: &Layout) -> Result<Vec<u8>> {
    let offsets = many(r, ifd, 273);
    let counts = many(r, ifd, 279);
    if offsets.is_empty() {
        return Err(corrupt("aucune bande"));
    }
    let rows_per_strip = u32::try_from(one(r, ifd, 278, u64::from(l.height))).unwrap_or(l.height);
    let rows_per_strip = rows_per_strip.max(1);
    let stride = ((l.width * l.samples * l.bits) as usize).div_ceil(8);
    let mut out = Vec::with_capacity(stride * l.height as usize);
    let tables = jpeg_tables(r, ifd);
    for (i, off) in offsets.iter().enumerate() {
        let at = usize::try_from(*off).map_err(|_| corrupt("adresse démesurée"))?;
        let len = counts
            .get(i)
            .copied()
            .map_or(r.data.len().saturating_sub(at), |c| {
                usize::try_from(c).unwrap_or(0)
            });
        let raw = r
            .data
            .get(at..at + len)
            .ok_or_else(|| corrupt("bande hors du fichier"))?;
        // Les bandes font toutes la même hauteur, sauf la dernière.
        let rows = rows_per_strip.min(
            l.height
                .saturating_sub(u32::try_from(i).unwrap_or(0) * rows_per_strip),
        );
        let mut decoded = decode_strip(raw, l, rows, tables.as_deref())?;
        if l.predictor == 2 {
            undo_predictor(&mut decoded, l, stride);
        }
        out.extend_from_slice(&decoded);
    }
    Ok(out)
}

/// Les tables de quantification communes d'un JPEG en TIFF (étiquette 347).
fn jpeg_tables(r: &Reader<'_>, ifd: &Ifd) -> Option<Vec<u8>> {
    let f = ifd.get(&347)?;
    let at = r.payload_offset(f) as usize;
    let len = f.count as usize;
    r.data.get(at..at + len).map(<[u8]>::to_vec)
}

/// Décompresse une bande selon la compression déclarée.
fn decode_strip(raw: &[u8], l: &Layout, rows: u32, tables: Option<&[u8]>) -> Result<Vec<u8>> {
    match l.compression {
        1 => Ok(raw.to_vec()),
        2..=4 => ccitt(raw, l, rows),
        5 => acrux_codecs::lzw::decode(raw, true),
        7 => jpeg(raw, tables),
        8 | 32_946 => acrux_codecs::flate::decode(raw),
        32_773 => Ok(packbits(raw)),
        other => Err(Error::Unsupported(format!(
            "TIFF : compression {other} non prise en charge"
        ))),
    }
}

/// Décompresse une bande CCITT.
fn ccitt(raw: &[u8], l: &Layout, rows: u32) -> Result<Vec<u8>> {
    let params = acrux_codecs::ccitt::CcittParams {
        // Compression 2 : Huffman modifié, une ligne par octet entier et
        // aucun EOL. Compression 3 : Groupe 3, une ou deux dimensions selon
        // le premier bit de l'étiquette T4Options — le décodeur le lit dans
        // le flux. Compression 4 : Groupe 4 pur.
        k: match l.compression {
            2 => 0,
            3 => 1,
            _ => -1,
        },
        columns: l.width,
        rows,
        // Le décodeur rend les pixels noirs sur 1 quand l'étiquette 262 dit
        // « blanc = zéro » (le cas des télécopies et des scanners), et sur 0
        // quand elle dit l'inverse : dans les deux cas l'échantillon obtenu
        // est bien celui que la page déclare, et l'interprétation se fait
        // ensuite comme pour n'importe quelle image.
        black_is_1: l.photometric == 0,
        byte_align: l.compression == 2,
        end_of_line: false,
        end_of_block: false,
    };
    acrux_codecs::ccitt::decode(raw, &params)
}

/// Décompresse une bande JPEG, en lui rendant au besoin les tables communes.
fn jpeg(raw: &[u8], tables: Option<&[u8]>) -> Result<Vec<u8>> {
    let owned;
    let bytes = match tables {
        // Un flux « abrégé » n'a pas ses tables : on recolle celles de
        // l'étiquette 347, en retirant leur fin de fichier et le début du
        // nôtre.
        Some(t) if !raw.starts_with(&[0xFF, 0xD8]) && t.len() > 2 => {
            let mut v = t[..t.len() - 2].to_vec();
            v.extend_from_slice(raw);
            owned = v;
            &owned[..]
        }
        _ => raw,
    };
    Ok(acrux_codecs::dct::decode(bytes)?.data)
}

/// PackBits (étiquette 32773) : la variante TIFF du codage par répétition.
///
/// Écrite ici plutôt qu'empruntée au `RunLengthDecode` de PDF : les deux
/// codages sont les mêmes à un détail près, l'octet 128 qui termine le flux
/// en PDF et ne fait rien en TIFF.
fn packbits(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() * 2);
    let mut at = 0;
    while at < raw.len() {
        let n = i8::from_ne_bytes([raw[at]]);
        at += 1;
        if n >= 0 {
            // Une suite d'octets bruts, n + 1 en tout.
            let end = (at + n.unsigned_abs() as usize + 1).min(raw.len());
            out.extend_from_slice(&raw[at..end]);
            at = end;
        } else if n != -128 {
            // Une répétition : 1 - n fois le même octet. (-128 : sans effet.)
            let Some(b) = raw.get(at) else { break };
            out.extend(std::iter::repeat_n(*b, n.unsigned_abs() as usize + 1));
            at += 1;
        }
    }
    out
}

/// Défait le prédicteur horizontal.
fn undo_predictor(data: &mut [u8], l: &Layout, stride: usize) {
    if l.bits != 8 {
        return;
    }
    let step = l.samples as usize;
    for row in data.chunks_mut(stride) {
        for i in step..row.len() {
            row[i] = row[i].wrapping_add(row[i - step]);
        }
    }
}

/// Développe les échantillons en pixels.
fn assemble(l: &Layout, body: &[u8]) -> Raster {
    let (w, h) = (l.width as usize, l.height as usize);
    let stride = ((l.width * l.samples * l.bits) as usize).div_ceil(8);
    let colour = l.photometric == 2 || l.photometric == 5 || !l.palette.is_empty();
    let mut data = Vec::with_capacity(w * h * if colour { 3 } else { 1 });
    let mut alpha = Vec::with_capacity(if l.alpha { w * h } else { 0 });
    let max = u32::from(u16::try_from((1u32 << l.bits) - 1).unwrap_or(u16::MAX));
    for y in 0..h {
        for x in 0..w {
            let mut s = [0u8; 4];
            for (c, value) in s.iter_mut().enumerate().take(l.samples as usize) {
                let index = u32::try_from(x * l.samples as usize + c).unwrap_or(0);
                let raw = sample(body, y * stride, index, l.bits);
                // Tout est ramené à huit bits : c'est ce qu'un PDF écrit.
                *value = match l.bits {
                    16 => u8::try_from(raw >> 8).unwrap_or(0),
                    8 => u8::try_from(raw).unwrap_or(0),
                    // Les petites profondeurs s'étalent sur toute la plage :
                    // un bit à 1 doit donner 255, pas 1.
                    _ => u8::try_from(raw * 255 / max.max(1)).unwrap_or(255),
                };
            }
            push_pixel(l, s, colour, &mut data, &mut alpha);
        }
    }
    Raster {
        width: l.width,
        height: l.height,
        components: if colour { 3 } else { 1 },
        data,
        alpha: (l.alpha && alpha.iter().any(|a| *a != 255)).then_some(alpha),
    }
}

/// Un échantillon de la ligne, quelle que soit sa taille en bits.
fn sample(body: &[u8], row: usize, index: u32, bits: u32) -> u32 {
    match bits {
        16 => {
            let at = row + index as usize * 2;
            let b = body.get(at..at + 2).unwrap_or(&[0, 0]);
            u32::from(u16::from_be_bytes([b[0], b[1]]))
        }
        8 => u32::from(body.get(row + index as usize).copied().unwrap_or(0)),
        _ => {
            let bit = index * bits;
            let byte = body.get(row + (bit / 8) as usize).copied().unwrap_or(0);
            let shift = 8 - bits - bit % 8;
            u32::from(byte >> shift) & ((1 << bits) - 1)
        }
    }
}

/// Range un pixel, selon l'interprétation déclarée par la page.
fn push_pixel(l: &Layout, s: [u8; 4], colour: bool, data: &mut Vec<u8>, alpha: &mut Vec<u8>) {
    match l.photometric {
        // Palette : l'échantillon est un indice.
        3 => {
            let c = l
                .palette
                .get(usize::from(s[0]))
                .copied()
                .unwrap_or([0, 0, 0]);
            data.extend_from_slice(&c);
        }
        // CMJN : la conversion est celle de `DeviceCMYK` sans profil.
        5 => {
            let k = u32::from(s[3]);
            for c in &s[..3] {
                let v = 255u32.saturating_sub(u32::from(*c)) * (255 - k) / 255;
                data.push(u8::try_from(v).unwrap_or(0));
            }
        }
        2 => data.extend_from_slice(&s[..3]),
        // Niveaux de gris : « blanc = zéro » s'inverse.
        other => {
            let v = if other == 0 { 255 - s[0] } else { s[0] };
            if colour {
                data.extend_from_slice(&[v, v, v]);
            } else {
                data.push(v);
            }
        }
    }
    if l.alpha {
        let at = if l.samples >= 4 { 3 } else { 1 };
        alpha.push(s[at]);
    }
}
