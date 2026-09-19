//! Format de fichier JP2 (ISO/IEC 15444-1 annexe I) : boîtes de signature
//! (I.5.1), de type de fichier (I.5.2), d'en-tête JP2 (I.5.3) avec `ihdr`
//! (I.5.3.1), `colr` (I.5.3.3), `pclr` (I.5.3.4), `cmap` (I.5.3.5) et
//! `cdef` (I.5.3.6), et de flux de code contigu `jp2c` (I.5.4).
//!
//! Un flux de code brut (commençant par SOC) est accepté tel quel. Les
//! boîtes inconnues sont ignorées, une boîte tronquée est lue jusqu'à la
//! fin des données.

use acrux_core::{Error, Result};

use super::corrupt;

/// Signature JP2 (I.5.1) : boîte `jP  ` de 12 octets.
const SIGNATURE: [u8; 12] = [
    0x00, 0x00, 0x00, 0x0C, 0x6A, 0x50, 0x20, 0x20, 0x0D, 0x0A, 0x87, 0x0A,
];

/// Espace de couleur déclaré par la boîte `colr`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Colour {
    /// Méthode 1 : espace énuméré (tableau I.10).
    Enumerated(u32),
    /// Méthode 2 : profil ICC restreint (octets du profil).
    Icc(Vec<u8>),
}

/// Palette (`pclr`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Palette {
    pub entries: usize,
    /// (profondeur en bits, signé) par colonne.
    pub columns: Vec<(u8, bool)>,
    /// Valeurs brutes, `entries × columns.len()`.
    pub lut: Vec<u32>,
}

/// Entrée de `cmap`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CmapEntry {
    pub component: u16,
    /// 0 : composante directe ; 1 : via la palette.
    pub mtyp: u8,
    pub pcol: u8,
}

/// Entrée de `cdef`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CdefEntry {
    pub channel: u16,
    /// 0 couleur, 1 opacité, 2 opacité prémultipliée, 65535 inconnu.
    pub typ: u16,
    pub assoc: u16,
}

/// Contenu utile d'un fichier JP2 (ou d'un flux brut).
#[derive(Clone, Debug, Default)]
pub(super) struct Container<'a> {
    pub codestream: &'a [u8],
    pub colour: Option<Colour>,
    pub palette: Option<Palette>,
    pub cmap: Vec<CmapEntry>,
    pub cdef: Vec<CdefEntry>,
}

/// Boîte à `pos` : type, contenu et position suivante (I.4).
fn next_box(data: &[u8], pos: usize) -> Option<([u8; 4], &[u8], usize)> {
    let head = data.get(pos..pos + 8)?;
    let lbox = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    let tbox = [head[4], head[5], head[6], head[7]];
    let (len, header) = match lbox {
        0 => ((data.len() - pos) as u64, 8usize),
        1 => {
            let x = data.get(pos + 8..pos + 16)?;
            (
                u64::from_be_bytes([x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7]]),
                16,
            )
        }
        n => (u64::from(n), 8),
    };
    if len < header as u64 {
        return None;
    }
    let end = usize::try_from(len)
        .ok()
        .and_then(|l| pos.checked_add(l))
        .map_or(data.len(), |e| e.min(data.len()));
    Some((tbox, &data[pos + header..end], end))
}

/// Localise le flux de code et lit les métadonnées JP2.
pub(super) fn parse(data: &[u8]) -> Result<Container<'_>> {
    if data.starts_with(&[0xFF, 0x4F]) {
        return Ok(Container {
            codestream: data,
            ..Container::default()
        });
    }
    let mut container = Container::default();
    let mut pos = 0usize;
    let mut found = false;
    if !data.starts_with(&SIGNATURE) {
        // Sans signature : on tente quand même les boîtes, puis un SOC nu.
        if let Some(at) = data.windows(4).position(|w| w == [0xFF, 0x4F, 0xFF, 0x51]) {
            container.codestream = &data[at..];
            return Ok(container);
        }
    }
    while let Some((tbox, content, next)) = next_box(data, pos) {
        match &tbox {
            b"jp2h" => parse_header_box(content, &mut container)?,
            b"jp2c" => {
                container.codestream = content;
                found = true;
                break;
            }
            _ => {}
        }
        if next <= pos {
            break;
        }
        pos = next;
    }
    if !found {
        return Err(corrupt("boîte jp2c absente"));
    }
    Ok(container)
}

/// Sous-boîtes de `jp2h` (I.5.3).
fn parse_header_box(data: &[u8], c: &mut Container<'_>) -> Result<()> {
    let mut pos = 0usize;
    while let Some((tbox, content, next)) = next_box(data, pos) {
        match &tbox {
            b"colr" if c.colour.is_none() => c.colour = parse_colr(content),
            b"pclr" => c.palette = Some(parse_pclr(content)?),
            b"cmap" => {
                c.cmap = content
                    .chunks_exact(4)
                    .map(|e| CmapEntry {
                        component: u16::from_be_bytes([e[0], e[1]]),
                        mtyp: e[2],
                        pcol: e[3],
                    })
                    .collect();
            }
            b"cdef" => {
                let n = content
                    .get(..2)
                    .map_or(0, |s| usize::from(u16::from_be_bytes([s[0], s[1]])));
                c.cdef = content
                    .get(2..)
                    .unwrap_or(&[])
                    .chunks_exact(6)
                    .take(n)
                    .map(|e| CdefEntry {
                        channel: u16::from_be_bytes([e[0], e[1]]),
                        typ: u16::from_be_bytes([e[2], e[3]]),
                        assoc: u16::from_be_bytes([e[4], e[5]]),
                    })
                    .collect();
            }
            _ => {}
        }
        if next <= pos {
            break;
        }
        pos = next;
    }
    Ok(())
}

/// Boîte `colr` (I.5.3.3) : METH, PREC, APPROX puis EnumCS ou profil.
fn parse_colr(content: &[u8]) -> Option<Colour> {
    match *content.first()? {
        1 => content
            .get(3..7)
            .map(|e| Colour::Enumerated(u32::from_be_bytes([e[0], e[1], e[2], e[3]]))),
        2 | 3 => content.get(3..).map(|p| Colour::Icc(p.to_vec())),
        _ => None,
    }
}

/// Boîte `pclr` (I.5.3.4).
fn parse_pclr(content: &[u8]) -> Result<Palette> {
    let entries = usize::from(u16::from_be_bytes([
        *content.first().ok_or_else(|| corrupt("pclr tronquée"))?,
        *content.get(1).ok_or_else(|| corrupt("pclr tronquée"))?,
    ]));
    let ncol = usize::from(*content.get(2).ok_or_else(|| corrupt("pclr tronquée"))?);
    if entries == 0 || entries > 1024 || ncol == 0 {
        return Err(corrupt("palette invalide"));
    }
    let mut columns = Vec::with_capacity(ncol);
    for i in 0..ncol {
        let b = *content.get(3 + i).ok_or_else(|| corrupt("pclr tronquée"))?;
        let depth = (b & 0x7F) + 1;
        if depth > 16 {
            return Err(Error::Unsupported(format!(
                "JPEG 2000 : palette de {depth} bits"
            )));
        }
        columns.push((depth, b & 0x80 != 0));
    }
    let mut lut = Vec::with_capacity(entries * ncol);
    let mut at = 3 + ncol;
    for _ in 0..entries {
        for &(depth, _) in &columns {
            let size = usize::from(depth.div_ceil(8));
            let bytes = content
                .get(at..at + size)
                .ok_or_else(|| corrupt("pclr tronquée"))?;
            lut.push(bytes.iter().fold(0u32, |acc, &b| (acc << 8) | u32::from(b)));
            at += size;
        }
    }
    Ok(Palette {
        entries,
        columns,
        lut,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn make_box(t: [u8; 4], content: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&u32::try_from(content.len() + 8).unwrap().to_be_bytes());
        b.extend_from_slice(&t);
        b.extend_from_slice(content);
        b
    }

    #[test]
    fn raw_codestream_is_passed_through() {
        let c = parse(&[0xFF, 0x4F, 0xFF, 0x51]).unwrap();
        assert_eq!(c.codestream.len(), 4);
        assert!(c.colour.is_none());
    }

    #[test]
    fn jp2_boxes_expose_colour_palette_and_channels() {
        let mut colr = vec![1, 0, 0];
        colr.extend_from_slice(&16u32.to_be_bytes());
        let mut pclr = vec![0, 2, 3, 7, 7, 15];
        pclr.extend_from_slice(&[1, 2, 0, 3, 4, 5, 0, 6]);
        let cmap = [0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 2];
        let cdef = [0, 1, 0, 0, 0, 1, 0, 1];
        let mut jp2h = Vec::new();
        jp2h.extend(make_box(*b"ihdr", &[0; 14]));
        jp2h.extend(make_box(*b"colr", &colr));
        jp2h.extend(make_box(*b"pclr", &pclr));
        jp2h.extend(make_box(*b"cmap", &cmap));
        jp2h.extend(make_box(*b"cdef", &cdef));
        let mut file = SIGNATURE.to_vec();
        file.extend(make_box(*b"ftyp", b"jp2 \0\0\0\0jp2 "));
        file.extend(make_box(*b"jp2h", &jp2h));
        file.extend(make_box(*b"jp2c", &[0xFF, 0x4F, 0xFF, 0x51]));
        let c = parse(&file).unwrap();
        assert_eq!(c.codestream, &[0xFF, 0x4F, 0xFF, 0x51]);
        assert_eq!(c.colour, Some(Colour::Enumerated(16)));
        let p = c.palette.unwrap();
        assert_eq!(p.entries, 2);
        assert_eq!(p.columns, vec![(8, false), (8, false), (16, false)]);
        assert_eq!(p.lut, vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(c.cmap.len(), 3);
        assert_eq!(c.cmap[2].pcol, 2);
        assert_eq!(c.cdef.len(), 1);
        assert_eq!(c.cdef[0].typ, 1);
    }

    #[test]
    fn missing_jp2c_is_corrupt_and_truncated_box_is_clipped() {
        let mut file = SIGNATURE.to_vec();
        file.extend(make_box(*b"ftyp", b"jp2 "));
        assert!(matches!(parse(&file), Err(Error::Corrupt(_))));
        let mut file2 = SIGNATURE.to_vec();
        file2.extend_from_slice(&[0, 0, 1, 0, b'j', b'p', b'2', b'c', 0xFF, 0x4F]);
        let c = parse(&file2).unwrap();
        assert_eq!(c.codestream, &[0xFF, 0x4F]);
    }

    #[test]
    fn stray_bytes_before_soc_are_skipped() {
        let c = parse(&[1, 2, 3, 0xFF, 0x4F, 0xFF, 0x51, 9]).unwrap();
        assert_eq!(c.codestream, &[0xFF, 0x4F, 0xFF, 0x51, 9]);
    }
}
