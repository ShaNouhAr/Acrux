//! Dictionnaire de symboles (T.88 §6.5, segment §7.4.3) et région de texte
//! (§6.4, segment §7.4.4), en codage arithmétique et en codage de Huffman.
//!
//! Longueur des codes d'identifiants : `max(1, ⌈log2(nombre de symboles)⌉)`
//! (§7.4.4.1.1 et §6.5.8.2.3 tels qu'amendés ; la valeur 0 de la version
//! initiale de la norme n'est produite par aucun encodeur connu).

use super::bitmap::{Bitmap, ComposeOp};
use super::generic::{
    decode_generic, decode_generic_mmr, decode_refinement, GenericParams, RefinementParams,
    GB_CONTEXTS, GR_CONTEXTS,
};
use super::huffman::{self, BitReader, Table};
use super::mq::{Context, IntContexts, MqDecoder};
use super::{corrupt, read_u16, read_u32, MAX_SYMBOLS, MAX_SYMBOL_PIXELS};
use crate::ccitt::MmrDecoder;
use acrux_core::{Error, Result};

/// `max(1, ⌈log2(n)⌉)`.
pub(crate) fn symbol_code_len(n: usize) -> u32 {
    let mut len = 0;
    while (1usize << len) < n {
        len += 1;
    }
    len.max(1)
}

/// Tables de Huffman d'une région de texte (§7.4.4.1.2).
pub(crate) struct TextHuffTables {
    pub fs: Table,
    pub ds: Table,
    pub dt: Table,
    pub rdw: Table,
    pub rdh: Table,
    pub rdx: Table,
    pub rdy: Table,
    pub rsize: Table,
    /// Codes des identifiants de symboles (§7.4.3.1.7).
    pub symbol_codes: Table,
}

/// Source de symboles : décodeur arithmétique et ses contextes, ou lecteur
/// de bits et tables de Huffman.
pub(crate) enum Coder<'a, 'b> {
    Arith {
        mq: &'b mut MqDecoder<'a>,
        ints: &'b mut IntContexts,
    },
    Huffman {
        reader: &'b mut BitReader<'a>,
        tables: &'b TextHuffTables,
    },
}

/// Paramètres de la procédure de décodage de région de texte (§6.4.2).
pub(crate) struct TextRegionParams<'a> {
    pub width: usize,
    pub height: usize,
    pub num_instances: u32,
    /// log2(SBSTRIPS).
    pub log_strips: u32,
    /// REFCORNER : 0 BOTTOMLEFT, 1 TOPLEFT, 2 BOTTOMRIGHT, 3 TOPRIGHT.
    pub ref_corner: u8,
    pub transposed: bool,
    pub comb_op: ComposeOp,
    pub def_pixel: u8,
    pub ds_offset: i32,
    pub refine: bool,
    pub refinement: RefinementParams,
    pub symbols: &'a [Bitmap],
}

/// Procédure de décodage de région de texte (§6.4.5).
///
/// # Errors
///
/// `Error::Corrupt` sur incohérence des données.
pub(crate) fn decode_text_region(
    p: &TextRegionParams<'_>,
    coder: &mut Coder<'_, '_>,
    rcx: &mut [Context],
) -> Result<Bitmap> {
    let mut bitmap = Bitmap::new(p.width, p.height, p.def_pixel)?;
    let strip_size = 1i64 << p.log_strips.min(3);
    let mut strip_t: i64 = -decode_dt(coder)? * strip_size;
    let mut first_s: i64 = 0;
    let mut instances: u32 = 0;
    let empty = Bitmap {
        width: 0,
        height: 0,
        data: Vec::new(),
    };
    while instances < p.num_instances {
        strip_t += decode_dt(coder)? * strip_size;
        first_s += decode_fs(coder)?;
        let mut s_pos = first_s;
        let mut first = true;
        loop {
            if !first {
                let Some(ids) = decode_ds(coder)? else {
                    break;
                };
                s_pos += ids + i64::from(p.ds_offset);
            }
            first = false;
            if instances >= p.num_instances {
                break;
            }
            let t_offset = if strip_size == 1 {
                0
            } else {
                decode_it(coder, p.log_strips)
            };
            let t_pos = strip_t + t_offset;
            let id = decode_id(coder)?;
            let symbol = p.symbols.get(id).unwrap_or(&empty);
            let refined;
            let symbol = if p.refine && decode_ri(coder) != 0 {
                refined = refine_symbol(p, coder, rcx, symbol)?;
                &refined
            } else {
                symbol
            };
            draw_symbol(&mut bitmap, symbol, p, s_pos, t_pos);
            let advance = if p.transposed {
                symbol.height
            } else {
                symbol.width
            };
            s_pos += i64::try_from(advance).unwrap_or(0).saturating_sub(1).max(0);
            instances += 1;
        }
    }
    Ok(bitmap)
}

/// Raffinement d'une instance (§6.4.11).
fn refine_symbol(
    p: &TextRegionParams<'_>,
    coder: &mut Coder<'_, '_>,
    rcx: &mut [Context],
    symbol: &Bitmap,
) -> Result<Bitmap> {
    let rdw = i64::from(decode_int(coder, IntKind::Rdw)?);
    let rdh = i64::from(decode_int(coder, IntKind::Rdh)?);
    let rdx = i64::from(decode_int(coder, IntKind::Rdx)?);
    let rdy = i64::from(decode_int(coder, IntKind::Rdy)?);
    let w = i64::try_from(symbol.width).unwrap_or(0) + rdw;
    let h = i64::try_from(symbol.height).unwrap_or(0) + rdh;
    if w < 0 || h < 0 || w > 1 << 20 || h > 1 << 20 {
        return Err(corrupt("raffinement : dimensions invalides"));
    }
    let (w, h) = (
        usize::try_from(w).unwrap_or(0),
        usize::try_from(h).unwrap_or(0),
    );
    // GRREFERENCEDX = ⌊RDW / 2⌋ + RDX, GRREFERENCEDY = ⌊RDH / 2⌋ + RDY.
    let dx = rdw.div_euclid(2) + rdx;
    let dy = rdh.div_euclid(2) + rdy;
    match coder {
        Coder::Arith { mq, .. } => decode_refinement(w, h, p.refinement, symbol, dx, dy, mq, rcx),
        Coder::Huffman { reader, tables } => {
            let bmsize = tables.rsize.decode(reader)?.unwrap_or(0);
            if bmsize <= 0 {
                return Err(Error::Unsupported(
                    "JBIG2 : raffinement Huffman de taille inconnue".into(),
                ));
            }
            reader.align();
            let start = reader.byte_pos();
            let end = start
                .saturating_add(usize::try_from(bmsize).unwrap_or(0))
                .min(reader.data().len());
            let data = reader.data().get(start..end).unwrap_or(&[]);
            let mut mq = MqDecoder::new(data);
            let out = decode_refinement(w, h, p.refinement, symbol, dx, dy, &mut mq, rcx)?;
            reader.seek_byte(end);
            Ok(out)
        }
    }
}

/// Dessine une instance selon REFCORNER et TRANSPOSED (§6.4.5, étapes
/// 3 c vi à xi) : quelle que soit le coin de référence, le bord gauche
/// (ou haut, en mode transposé) de l'instance est en CURS.
fn draw_symbol(
    bitmap: &mut Bitmap,
    symbol: &Bitmap,
    p: &TextRegionParams<'_>,
    s_pos: i64,
    t_pos: i64,
) {
    let width = i64::try_from(symbol.width).unwrap_or(0);
    let height = i64::try_from(symbol.height).unwrap_or(0);
    let (x, y) = if p.transposed {
        let x = match p.ref_corner {
            0 | 1 => t_pos,
            _ => t_pos - width + 1,
        };
        (x, s_pos)
    } else {
        let y = match p.ref_corner {
            1 | 3 => t_pos,
            _ => t_pos - height + 1,
        };
        (s_pos, y)
    };
    bitmap.compose(symbol, x, y, p.comb_op);
}

#[derive(Clone, Copy)]
enum IntKind {
    Rdw,
    Rdh,
    Rdx,
    Rdy,
}

fn decode_int(coder: &mut Coder<'_, '_>, kind: IntKind) -> Result<i32> {
    match coder {
        Coder::Arith { mq, ints } => {
            let ctx = match kind {
                IntKind::Rdw => &mut ints.iardw,
                IntKind::Rdh => &mut ints.iardh,
                IntKind::Rdx => &mut ints.iardx,
                IntKind::Rdy => &mut ints.iardy,
            };
            Ok(ctx.decode(mq).unwrap_or(0))
        }
        Coder::Huffman { reader, tables } => {
            let table = match kind {
                IntKind::Rdw => &tables.rdw,
                IntKind::Rdh => &tables.rdh,
                IntKind::Rdx => &tables.rdx,
                IntKind::Rdy => &tables.rdy,
            };
            Ok(table.decode(reader)?.unwrap_or(0))
        }
    }
}

fn decode_dt(coder: &mut Coder<'_, '_>) -> Result<i64> {
    Ok(i64::from(match coder {
        Coder::Arith { mq, ints } => ints.iadt.decode(mq).unwrap_or(0),
        Coder::Huffman { reader, tables } => tables.dt.decode(reader)?.unwrap_or(0),
    }))
}

fn decode_fs(coder: &mut Coder<'_, '_>) -> Result<i64> {
    Ok(i64::from(match coder {
        Coder::Arith { mq, ints } => ints.iafs.decode(mq).unwrap_or(0),
        Coder::Huffman { reader, tables } => tables.fs.decode(reader)?.unwrap_or(0),
    }))
}

/// `None` = OOB (fin de bande).
fn decode_ds(coder: &mut Coder<'_, '_>) -> Result<Option<i64>> {
    Ok(match coder {
        Coder::Arith { mq, ints } => ints.iads.decode(mq).map(i64::from),
        Coder::Huffman { reader, tables } => tables.ds.decode(reader)?.map(i64::from),
    })
}

fn decode_it(coder: &mut Coder<'_, '_>, log_strips: u32) -> i64 {
    match coder {
        Coder::Arith { mq, ints } => i64::from(ints.iait.decode(mq).unwrap_or(0)),
        Coder::Huffman { reader, .. } => i64::from(reader.read_bits(log_strips)),
    }
}

fn decode_id(coder: &mut Coder<'_, '_>) -> Result<usize> {
    Ok(match coder {
        Coder::Arith { mq, ints } => ints.iaid.decode(mq) as usize,
        Coder::Huffman { reader, tables } => {
            let v = tables.symbol_codes.decode(reader)?.unwrap_or(0);
            usize::try_from(v).unwrap_or(usize::MAX)
        }
    })
}

fn decode_ri(coder: &mut Coder<'_, '_>) -> i32 {
    match coder {
        Coder::Arith { mq, ints } => ints.iari.decode(mq).unwrap_or(0),
        Coder::Huffman { reader, .. } => i32::try_from(reader.read_bit()).unwrap_or(0),
    }
}

// ---------------------------------------------------------------------------
// Dictionnaire de symboles
// ---------------------------------------------------------------------------

/// Contextes conservés par un dictionnaire (§7.4.3.1.1, bits 8 et 9).
pub(crate) struct RetainedContexts {
    pub generic: Vec<Context>,
    pub refinement: Vec<Context>,
}

/// Résultat du décodage d'un dictionnaire.
pub(crate) struct SymbolDict {
    pub exported: Vec<Bitmap>,
    pub retained: Option<RetainedContexts>,
}

/// Décode un segment de dictionnaire de symboles (§7.4.3) : `data` est le
/// contenu du segment, `input` les symboles des dictionnaires référencés,
/// `tables` les tables personnalisées référencées (dans l'ordre), `reused`
/// les contextes du dictionnaire référencé si le drapeau « bitmap coding
/// context used » est levé.
///
/// # Errors
///
/// - `Error::Corrupt` si le segment est tronqué ou incohérent ;
/// - `Error::Unsupported` pour le codage de Huffman avec raffinement /
///   agrégation (SDHUFF = 1 et SDREFAGG = 1).
pub(crate) fn decode_symbol_dict_segment(
    data: &[u8],
    input: &[Bitmap],
    tables: &[Table],
    reused: Option<&RetainedContexts>,
) -> Result<SymbolDict> {
    let flags = read_u16(data, 0)?;
    let huffman = flags & 1 != 0;
    let refagg = flags & 2 != 0;
    let height_table = (flags >> 2) & 3;
    let width_table = (flags >> 4) & 3;
    let bmsize_table = (flags >> 6) & 1;
    let ctx_used = flags & 0x100 != 0;
    let ctx_retained = flags & 0x200 != 0;
    #[allow(clippy::cast_possible_truncation)]
    let template = ((flags >> 10) & 3) as u8;
    #[allow(clippy::cast_possible_truncation)]
    let rtemplate = ((flags >> 12) & 1) as u8;
    let mut pos = 2;
    let mut at = [(0i8, 0i8); 4];
    if !huffman {
        let n = if template == 0 { 4 } else { 1 };
        for a in at.iter_mut().take(n) {
            *a = read_at(data, pos)?;
            pos += 2;
        }
    }
    let mut rat = [(0i8, 0i8); 2];
    if refagg && rtemplate == 0 {
        for a in &mut rat {
            *a = read_at(data, pos)?;
            pos += 2;
        }
    }
    let num_ex = read_u32(data, pos)? as usize;
    let num_new = read_u32(data, pos + 4)? as usize;
    pos += 8;
    if num_ex > MAX_SYMBOLS || num_new > MAX_SYMBOLS || input.len() > MAX_SYMBOLS {
        return Err(corrupt("dictionnaire : trop de symboles"));
    }
    if huffman && refagg {
        return Err(Error::Unsupported(
            "JBIG2 : dictionnaire de symboles Huffman avec raffinement".into(),
        ));
    }
    // Tables de Huffman (§7.4.3.1.6).
    let mut custom = tables.iter();
    let huff = if huffman {
        Some(DictHuffTables {
            dh: pick_table(&mut custom, height_table, &[4, 5])?,
            dw: pick_table(&mut custom, width_table, &[2, 3])?,
            bmsize: pick_table(&mut custom, bmsize_table, &[1])?,
        })
    } else {
        None
    };
    let params = DictParams {
        huffman,
        refagg,
        generic: GenericParams {
            template,
            at,
            tpgdon: false,
        },
        refinement: RefinementParams {
            template: rtemplate,
            at: rat,
            tpgron: false,
        },
        num_ex,
        num_new,
        huff,
    };
    let body = data.get(pos..).unwrap_or(&[]);
    let (mut gcx, mut rcx) = match reused {
        Some(r) if ctx_used => (r.generic.clone(), r.refinement.clone()),
        _ => (vec![0; GB_CONTEXTS], vec![0; GR_CONTEXTS]),
    };
    if gcx.len() < GB_CONTEXTS || rcx.len() < GR_CONTEXTS {
        gcx = vec![0; GB_CONTEXTS];
        rcx = vec![0; GR_CONTEXTS];
    }
    let exported = decode_symbol_dict(body, &params, input, &mut gcx, &mut rcx)?;
    let retained = ctx_retained.then_some(RetainedContexts {
        generic: gcx,
        refinement: rcx,
    });
    Ok(SymbolDict { exported, retained })
}

/// Choisit une table de Huffman d'après un champ de sélection : indice dans
/// `standard_choices`, ou la prochaine table personnalisée référencée quand
/// la sélection vaut 3 (ou 1 pour les champs à un bit) (§7.4.3.1.6,
/// §7.4.4.1.2).
fn pick_table<'a>(
    custom: &mut impl Iterator<Item = &'a Table>,
    selection: u16,
    standard_choices: &[u32],
) -> Result<Table> {
    if selection == 3 || (standard_choices.len() == 1 && selection == 1) {
        custom
            .next()
            .cloned()
            .ok_or_else(|| corrupt("table personnalisée manquante"))
    } else {
        let n = standard_choices
            .get(usize::from(selection))
            .copied()
            .ok_or_else(|| corrupt("sélection de table invalide"))?;
        huffman::standard(n)
    }
}

/// Lit une paire de coordonnées de pixel adaptatif (octets signés).
pub(crate) fn read_at(data: &[u8], pos: usize) -> Result<(i8, i8)> {
    match (data.get(pos), data.get(pos + 1)) {
        #[allow(clippy::cast_possible_wrap)]
        (Some(&x), Some(&y)) => Ok((x as i8, y as i8)),
        _ => Err(corrupt("pixels adaptatifs tronqués")),
    }
}

struct DictHuffTables {
    dh: Table,
    dw: Table,
    bmsize: Table,
}

struct DictParams {
    huffman: bool,
    refagg: bool,
    generic: GenericParams,
    refinement: RefinementParams,
    num_ex: usize,
    num_new: usize,
    huff: Option<DictHuffTables>,
}

/// Procédure de décodage d'un dictionnaire de symboles (§6.5.5), rend les
/// symboles exportés (§6.5.10).
fn decode_symbol_dict(
    data: &[u8],
    p: &DictParams,
    input: &[Bitmap],
    gcx: &mut [Context],
    rcx: &mut [Context],
) -> Result<Vec<Bitmap>> {
    let total = input.len() + p.num_new;
    let code_len = symbol_code_len(total);
    let mut new_symbols: Vec<Bitmap> = Vec::with_capacity(p.num_new);
    let mut pixel_budget: u64 = MAX_SYMBOL_PIXELS;
    let mut mq = MqDecoder::new(data);
    let mut ints = IntContexts::new(code_len);
    let mut reader = BitReader::new(data);
    let mut hcheight: i64 = 0;
    while new_symbols.len() < p.num_new {
        let hcdh = if let Some(h) = &p.huff {
            h.dh.decode(&mut reader)?.unwrap_or(0)
        } else {
            ints.iadh.decode(&mut mq).unwrap_or(0)
        };
        hcheight += i64::from(hcdh);
        if !(0..=1 << 20).contains(&hcheight) {
            return Err(corrupt("dictionnaire : hauteur de classe invalide"));
        }
        let mut symwidth: i64 = 0;
        let mut totwidth: i64 = 0;
        let hcfirst = new_symbols.len();
        let mut widths: Vec<usize> = Vec::new();
        loop {
            let dw = if let Some(h) = &p.huff {
                h.dw.decode(&mut reader)?
            } else {
                ints.iadw.decode(&mut mq)
            };
            let Some(dw) = dw else {
                break; // OOB : fin de la classe de hauteur.
            };
            if new_symbols.len() >= p.num_new {
                return Err(corrupt("dictionnaire : trop de symboles dans une classe"));
            }
            symwidth += i64::from(dw);
            if !(0..=1 << 20).contains(&symwidth) {
                return Err(corrupt("dictionnaire : largeur de symbole invalide"));
            }
            totwidth += symwidth;
            let (w, h) = (
                usize::try_from(symwidth).unwrap_or(0),
                usize::try_from(hcheight).unwrap_or(0),
            );
            let area = (w as u64) * (h as u64);
            if area > pixel_budget {
                return Err(corrupt("dictionnaire : symboles trop volumineux"));
            }
            pixel_budget -= area;
            if p.huffman {
                // Collectif : les bitmaps arrivent après la classe (§6.5.9).
                widths.push(w);
                new_symbols.push(Bitmap::new(w, h, 0)?);
            } else if !p.refagg {
                let bitmap = decode_generic(w, h, &p.generic, &mut mq, gcx, None)?;
                new_symbols.push(bitmap);
            } else {
                let all: Vec<Bitmap> = input.iter().chain(new_symbols.iter()).cloned().collect();
                let bitmap =
                    decode_aggregate_symbol(p, &all, total, (w, h), &mut mq, &mut ints, rcx)?;
                new_symbols.push(bitmap);
            }
        }
        if let Some(h) = &p.huff {
            read_collective_bitmap(
                &mut reader,
                h,
                &mut new_symbols[hcfirst..],
                &widths,
                usize::try_from(totwidth).unwrap_or(0),
                usize::try_from(hcheight).unwrap_or(0),
            )?;
        }
    }
    // Drapeaux d'exportation (§6.5.10).
    let all: Vec<Bitmap> = input.iter().cloned().chain(new_symbols).collect();
    let mut exported = Vec::with_capacity(p.num_ex);
    let mut i = 0usize;
    let mut cur_ex = false;
    let mut guard = 0usize;
    let ex_table = if p.huffman {
        Some(huffman::standard(1)?)
    } else {
        None
    };
    while i < all.len() && guard <= 2 * all.len() + 2 {
        guard += 1;
        let run = if let Some(t) = &ex_table {
            t.decode(&mut reader)?.unwrap_or(0)
        } else {
            ints.iaex.decode(&mut mq).unwrap_or(0)
        };
        let run = usize::try_from(run).unwrap_or(0).min(all.len() - i);
        if cur_ex {
            exported.extend_from_slice(&all[i..i + run]);
        }
        i += run;
        cur_ex = !cur_ex;
    }
    if exported.is_empty() && p.num_ex > 0 {
        // Drapeaux illisibles : exporter les nouveaux symboles (tolérance).
        exported = all[input.len()..].to_vec();
    }
    Ok(exported)
}

/// Symbole codé par raffinement ou agrégation (§6.5.8.2) : un seul
/// raffinement (§6.5.8.2.2) ou région de texte agrégée (§6.5.8.2.1,
/// tableau 17). `all` contient les symboles entrants puis les nouveaux
/// symboles déjà décodés.
// Sept paramètres : ceux du tableau 17 de la norme plus les trois états
// partagés du dictionnaire (décodeur MQ, contextes entiers, contextes de
// raffinement).
#[allow(clippy::too_many_arguments)]
fn decode_aggregate_symbol(
    p: &DictParams,
    all: &[Bitmap],
    total: usize,
    (w, h): (usize, usize),
    mq: &mut MqDecoder<'_>,
    ints: &mut IntContexts,
    rcx: &mut [Context],
) -> Result<Bitmap> {
    let nrefs = ints.iaai.decode(mq).unwrap_or(1);
    if nrefs == 1 {
        let id = ints.iaid.decode(mq) as usize;
        let rdx = ints.iardx.decode(mq).unwrap_or(0);
        let rdy = ints.iardy.decode(mq).unwrap_or(0);
        let empty = Bitmap::new(0, 0, 0)?;
        let reference = all.get(id).unwrap_or(&empty);
        let (dx, dy) = (i64::from(rdx), i64::from(rdy));
        return decode_refinement(w, h, p.refinement, reference, dx, dy, mq, rcx);
    }
    let mut padded = all.to_vec();
    padded.resize(total, Bitmap::new(0, 0, 0)?);
    let tp = TextRegionParams {
        width: w,
        height: h,
        num_instances: u32::try_from(nrefs).unwrap_or(0),
        log_strips: 0,
        ref_corner: 1,
        transposed: false,
        comb_op: ComposeOp::Or,
        def_pixel: 0,
        ds_offset: 0,
        refine: true,
        refinement: p.refinement,
        symbols: &padded,
    };
    let mut coder = Coder::Arith { mq, ints };
    decode_text_region(&tp, &mut coder, rcx)
}

/// Bitmap collectif d'une classe de hauteur (§6.5.9) : non compressé
/// (BMSIZE = 0) ou MMR, découpé en symboles selon leurs largeurs.
fn read_collective_bitmap(
    reader: &mut BitReader<'_>,
    h: &DictHuffTables,
    symbols: &mut [Bitmap],
    widths: &[usize],
    totwidth: usize,
    hcheight: usize,
) -> Result<()> {
    let bmsize = usize::try_from(h.bmsize.decode(reader)?.unwrap_or(0)).unwrap_or(0);
    reader.align();
    let start = reader.byte_pos();
    let data = reader.data();
    let mut collective = Bitmap::new(totwidth, hcheight, 0)?;
    let consumed = if bmsize == 0 {
        let stride = totwidth.div_ceil(8);
        for y in 0..hcheight {
            let row = data.get(start + y * stride..start + (y + 1) * stride);
            let Some(row) = row else { break };
            for x in 0..totwidth {
                collective.data[y * totwidth + x] = (row[x / 8] >> (7 - x % 8)) & 1;
            }
        }
        stride * hcheight
    } else {
        let end = start.saturating_add(bmsize).min(data.len());
        let slice = data.get(start..end).unwrap_or(&[]);
        let mut mmr = MmrDecoder::new(slice, u32::try_from(totwidth).unwrap_or(1));
        collective = decode_generic_mmr(&mut mmr, totwidth, hcheight)?;
        bmsize
    };
    reader.seek_byte(start.saturating_add(consumed));
    let mut x0 = 0usize;
    for (symbol, &w) in symbols.iter_mut().zip(widths) {
        *symbol = collective.window(i64::try_from(x0).unwrap_or(i64::MAX), 0, w, hcheight)?;
        x0 += w;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Segment de région de texte
// ---------------------------------------------------------------------------

/// Décode un segment de région de texte (§7.4.4) : `data` commence après
/// les informations de région (§7.4.1), `symbols` sont les symboles des
/// dictionnaires référencés, `tables` les tables personnalisées.
///
/// # Errors
///
/// `Error::Corrupt` si le segment est tronqué ou incohérent.
pub(crate) fn decode_text_region_segment(
    data: &[u8],
    width: usize,
    height: usize,
    symbols: &[Bitmap],
    tables: &[Table],
) -> Result<Bitmap> {
    let flags = read_u16(data, 0)?;
    let huffman = flags & 1 != 0;
    let refine = flags & 2 != 0;
    let log_strips = u32::from((flags >> 2) & 3);
    #[allow(clippy::cast_possible_truncation)]
    let ref_corner = ((flags >> 4) & 3) as u8;
    let transposed = flags & 0x40 != 0;
    #[allow(clippy::cast_possible_truncation)]
    let comb_op = ComposeOp::from_code(((flags >> 7) & 3) as u8);
    #[allow(clippy::cast_possible_truncation)]
    let def_pixel = ((flags >> 9) & 1) as u8;
    let mut ds_offset = i32::from((flags >> 10) & 0x1F);
    if ds_offset > 15 {
        ds_offset -= 32;
    }
    #[allow(clippy::cast_possible_truncation)]
    let rtemplate = ((flags >> 15) & 1) as u8;
    let mut pos = 2;
    let mut huff_flags = 0u16;
    if huffman {
        huff_flags = read_u16(data, pos)?;
        pos += 2;
    }
    let mut rat = [(0i8, 0i8); 2];
    if refine && rtemplate == 0 {
        for a in &mut rat {
            *a = read_at(data, pos)?;
            pos += 2;
        }
    }
    let num_instances = read_u32(data, pos)?;
    pos += 4;
    let code_len = symbol_code_len(symbols.len());
    let params = TextRegionParams {
        width,
        height,
        num_instances,
        log_strips,
        ref_corner,
        transposed,
        comb_op,
        def_pixel,
        ds_offset,
        refine,
        refinement: RefinementParams {
            template: rtemplate,
            at: rat,
            tpgron: false,
        },
        symbols,
    };
    let mut rcx = vec![0u8; GR_CONTEXTS];
    if huffman {
        let body = data.get(pos..).unwrap_or(&[]);
        let mut reader = BitReader::new(body);
        let tables = text_huffman_tables(huff_flags, tables, &mut reader, symbols.len())?;
        let mut coder = Coder::Huffman {
            reader: &mut reader,
            tables: &tables,
        };
        decode_text_region(&params, &mut coder, &mut rcx)
    } else {
        let body = data.get(pos..).unwrap_or(&[]);
        let mut mq = MqDecoder::new(body);
        let mut ints = IntContexts::new(code_len);
        let mut coder = Coder::Arith {
            mq: &mut mq,
            ints: &mut ints,
        };
        decode_text_region(&params, &mut coder, &mut rcx)
    }
}

/// Tables de Huffman d'une région de texte (§7.4.4.1.2) et codes des
/// identifiants de symboles (§7.4.3.1.7), lus en tête des données.
fn text_huffman_tables(
    huff_flags: u16,
    tables: &[Table],
    reader: &mut BitReader<'_>,
    num_symbols: usize,
) -> Result<TextHuffTables> {
    let mut custom = tables.iter();
    let fs = pick_table(&mut custom, huff_flags & 3, &[6, 7])?;
    let ds = pick_table(&mut custom, (huff_flags >> 2) & 3, &[8, 9, 10])?;
    let dt = pick_table(&mut custom, (huff_flags >> 4) & 3, &[11, 12, 13])?;
    let rdw = pick_table(&mut custom, (huff_flags >> 6) & 3, &[14, 15])?;
    let rdh = pick_table(&mut custom, (huff_flags >> 8) & 3, &[14, 15])?;
    let rdx = pick_table(&mut custom, (huff_flags >> 10) & 3, &[14, 15])?;
    let rdy = pick_table(&mut custom, (huff_flags >> 12) & 3, &[14, 15])?;
    let rsize = pick_table(&mut custom, (huff_flags >> 14) & 1, &[1])?;
    let symbol_codes = huffman::read_symbol_id_codes(reader, num_symbols)?;
    Ok(TextHuffTables {
        fs,
        ds,
        dt,
        rdw,
        rdh,
        rdx,
        rdy,
        rsize,
        symbol_codes,
    })
}
