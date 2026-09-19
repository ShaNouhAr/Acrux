//! Fonctions PDF (ISO 32000-2 §7.10) : échantillonnée (type 0),
//! exponentielle (type 2), par morceaux (type 3) et calculatrice PostScript
//! (type 4). Utilisées par les ombrages, les espaces Separation/DeviceN et
//! les fonctions de transfert.

use std::rc::Rc;

use acrux_core::{Error, Result};
use acrux_document::{Dict, Document, Object};

/// Fonction évaluable `f: R^m -> R^n`.
#[derive(Debug, Clone)]
pub enum Function {
    /// Type 0 : table d'échantillons interpolée.
    Sampled(Sampled),
    /// Type 2 : `C0 + x^N (C1 - C0)`.
    Exponential {
        /// Domaine.
        domain: Vec<f64>,
        /// Valeur en 0.
        c0: Vec<f64>,
        /// Valeur en 1.
        c1: Vec<f64>,
        /// Exposant.
        n: f64,
    },
    /// Type 3 : concaténation de fonctions à une entrée.
    Stitching {
        /// Domaine.
        domain: Vec<f64>,
        /// Sous-fonctions.
        functions: Vec<Rc<Function>>,
        /// Bornes intermédiaires (k-1 valeurs).
        bounds: Vec<f64>,
        /// Encodage de chaque sous-domaine.
        encode: Vec<f64>,
    },
    /// Type 4 : programme PostScript.
    PostScript {
        /// Domaine.
        domain: Vec<f64>,
        /// Étendue (obligatoire pour le type 4).
        range: Vec<f64>,
        /// Programme compilé.
        program: Vec<PsOp>,
    },
    /// Tableau de fonctions à une sortie chacune (ombrages : une par composante).
    Array(Vec<Rc<Function>>),
    /// Identité (`/Identity` dans les fonctions de transfert).
    Identity,
}

/// Fonction échantillonnée (type 0).
#[derive(Debug, Clone)]
pub struct Sampled {
    domain: Vec<f64>,
    range: Vec<f64>,
    size: Vec<usize>,
    bps: u32,
    encode: Vec<f64>,
    decode: Vec<f64>,
    /// Échantillons normalisés dans [0, 1], `n_out` valeurs par point.
    samples: Vec<f32>,
    n_out: usize,
}

/// Opération PostScript compilée (type 4, §7.10.5).
#[derive(Debug, Clone, PartialEq)]
pub enum PsOp {
    /// Pousse une constante.
    Push(f64),
    /// Opérateur nommé.
    Op(PsOperator),
    /// `{ … } if` : bloc exécuté si le sommet est vrai.
    If(Vec<PsOp>),
    /// `{ … } { … } ifelse`.
    IfElse(Vec<PsOp>, Vec<PsOp>),
}

/// Opérateurs de la calculatrice PostScript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum PsOperator {
    Abs,
    Add,
    Atan,
    Ceiling,
    Cos,
    Cvi,
    Cvr,
    Div,
    Exp,
    Floor,
    Idiv,
    Ln,
    Log,
    Mod,
    Mul,
    Neg,
    Round,
    Sin,
    Sqrt,
    Sub,
    Truncate,
    And,
    Bitshift,
    Eq,
    False,
    Ge,
    Gt,
    Le,
    Lt,
    Ne,
    Not,
    Or,
    True,
    Xor,
    Copy,
    Dup,
    Exch,
    Index,
    Pop,
    Roll,
}

const MAX_STACK: usize = 1000;
const MAX_STEPS: usize = 100_000;

impl Function {
    /// Construit une fonction depuis un objet PDF (dictionnaire, flux,
    /// tableau de fonctions ou nom `/Identity`).
    ///
    /// # Errors
    /// Type inconnu ou paramètres manquants.
    pub fn parse(doc: &Document, obj: &Object) -> Result<Function> {
        Self::parse_depth(doc, obj, 0)
    }

    fn parse_depth(doc: &Document, obj: &Object, depth: usize) -> Result<Function> {
        if depth > 16 {
            return Err(Error::Corrupt(
                "fonctions imbriquées trop profondément".into(),
            ));
        }
        let resolved = doc.resolve(obj)?;
        match &*resolved {
            Object::Name(n) if n.0 == b"Identity" => Ok(Function::Identity),
            Object::Array(items) => {
                let fns = items
                    .iter()
                    .map(|o| Self::parse_depth(doc, o, depth + 1).map(Rc::new))
                    .collect::<Result<Vec<_>>>()?;
                Ok(Function::Array(fns))
            }
            Object::Dict(_) | Object::Stream { .. } => {
                let dict = resolved.as_dict().cloned().unwrap_or_default();
                let kind = num(doc, &dict, "FunctionType").unwrap_or(-1.0);
                let domain = nums(doc, &dict, "Domain").unwrap_or_else(|| vec![0.0, 1.0]);
                #[allow(clippy::cast_possible_truncation)]
                match kind as i64 {
                    0 => {
                        let Object::Stream { .. } = &*resolved else {
                            return Err(Error::Corrupt("fonction de type 0 sans flux".into()));
                        };
                        let data = doc.stream_data(&resolved)?.data;
                        Sampled::new(doc, &dict, domain, &data).map(Function::Sampled)
                    }
                    2 => Ok(Function::Exponential {
                        domain,
                        c0: nums(doc, &dict, "C0").unwrap_or_else(|| vec![0.0]),
                        c1: nums(doc, &dict, "C1").unwrap_or_else(|| vec![1.0]),
                        n: num(doc, &dict, "N").unwrap_or(1.0),
                    }),
                    3 => {
                        let functions = match doc.dict_get(&dict, "Functions")? {
                            Some(f) => f
                                .as_array()
                                .unwrap_or(&[])
                                .iter()
                                .map(|o| Self::parse_depth(doc, o, depth + 1).map(Rc::new))
                                .collect::<Result<Vec<_>>>()?,
                            None => Vec::new(),
                        };
                        if functions.is_empty() {
                            return Err(Error::Corrupt(
                                "fonction de type 3 sans sous-fonctions".into(),
                            ));
                        }
                        let k = functions.len();
                        let mut encode = nums(doc, &dict, "Encode").unwrap_or_default();
                        if encode.len() < 2 * k {
                            encode = (0..k).flat_map(|_| [0.0, 1.0]).collect();
                        }
                        Ok(Function::Stitching {
                            domain,
                            bounds: nums(doc, &dict, "Bounds").unwrap_or_default(),
                            encode,
                            functions,
                        })
                    }
                    4 => {
                        let Object::Stream { .. } = &*resolved else {
                            return Err(Error::Corrupt("fonction de type 4 sans flux".into()));
                        };
                        let src = doc.stream_data(&resolved)?.data;
                        let program = parse_postscript(&src)?;
                        let range = nums(doc, &dict, "Range").ok_or_else(|| {
                            Error::Corrupt("fonction de type 4 sans /Range".into())
                        })?;
                        Ok(Function::PostScript {
                            domain,
                            range,
                            program,
                        })
                    }
                    other => Err(Error::Unsupported(format!("fonction de type {other}"))),
                }
            }
            _ => Err(Error::Corrupt("objet fonction invalide".into())),
        }
    }

    /// Nombre de sorties si connu.
    #[must_use]
    pub fn outputs(&self) -> Option<usize> {
        match self {
            Function::Sampled(s) => Some(s.n_out),
            Function::Exponential { c0, .. } => Some(c0.len()),
            Function::Stitching { functions, .. } => functions.first().and_then(|f| f.outputs()),
            Function::PostScript { range, .. } => Some(range.len() / 2),
            Function::Array(fns) => Some(fns.len()),
            Function::Identity => None,
        }
    }

    /// Évalue la fonction. Les entrées sont bornées au domaine, les sorties à
    /// l'étendue quand elle est définie. Ne panique jamais : une fonction
    /// invalide retourne des zéros.
    #[must_use]
    pub fn eval(&self, inputs: &[f64]) -> Vec<f64> {
        self.eval_depth(inputs, 0)
    }

    fn eval_depth(&self, inputs: &[f64], depth: usize) -> Vec<f64> {
        if depth > 32 {
            return Vec::new();
        }
        match self {
            Function::Identity => inputs.to_vec(),
            Function::Array(fns) => fns
                .iter()
                .map(|f| {
                    f.eval_depth(inputs, depth + 1)
                        .first()
                        .copied()
                        .unwrap_or(0.0)
                })
                .collect(),
            Function::Exponential { domain, c0, c1, n } => {
                let x = clamp_domain(inputs.first().copied().unwrap_or(0.0), domain, 0);
                let t = if (*n - 1.0).abs() < f64::EPSILON {
                    x
                } else {
                    x.abs().powf(*n) * x.signum()
                };
                c0.iter()
                    .zip(c1.iter().chain(std::iter::repeat(&0.0)))
                    .map(|(a, b)| a + t * (b - a))
                    .collect()
            }
            Function::Stitching {
                domain,
                functions,
                bounds,
                encode,
            } => {
                let x = clamp_domain(inputs.first().copied().unwrap_or(0.0), domain, 0);
                let d0 = domain.first().copied().unwrap_or(0.0);
                let d1 = domain.get(1).copied().unwrap_or(1.0);
                let k = functions.len();
                let mut i = 0;
                while i < bounds.len() && i + 1 < k && x >= bounds[i] {
                    i += 1;
                }
                let low = if i == 0 { d0 } else { bounds[i - 1] };
                let high = if i >= bounds.len() { d1 } else { bounds[i] };
                let e0 = encode.get(2 * i).copied().unwrap_or(0.0);
                let e1 = encode.get(2 * i + 1).copied().unwrap_or(1.0);
                let t = interpolate(x, low, high, e0, e1);
                functions[i].eval_depth(&[t], depth + 1)
            }
            Function::Sampled(s) => s.eval(inputs),
            Function::PostScript {
                domain,
                range,
                program,
            } => {
                let mut stack: Vec<f64> = inputs
                    .iter()
                    .enumerate()
                    .map(|(i, v)| clamp_domain(*v, domain, i))
                    .collect();
                let mut steps = 0;
                exec_postscript(program, &mut stack, &mut steps);
                let n_out = range.len() / 2;
                let start = stack.len().saturating_sub(n_out);
                let mut out: Vec<f64> = stack[start..].to_vec();
                while out.len() < n_out {
                    out.insert(0, 0.0);
                }
                for (i, v) in out.iter_mut().enumerate() {
                    *v = clamp_domain(*v, range, i);
                }
                out
            }
        }
    }
}

fn clamp_domain(x: f64, domain: &[f64], i: usize) -> f64 {
    let lo = domain.get(2 * i).copied().unwrap_or(f64::MIN);
    let hi = domain.get(2 * i + 1).copied().unwrap_or(f64::MAX);
    if x.is_nan() {
        return lo;
    }
    x.clamp(lo.min(hi), hi.max(lo))
}

/// Interpolation linéaire (§7.10.2, formule « Interpolate »).
fn interpolate(x: f64, xmin: f64, xmax: f64, ymin: f64, ymax: f64) -> f64 {
    if (xmax - xmin).abs() < f64::EPSILON {
        ymin
    } else {
        ymin + (x - xmin) * (ymax - ymin) / (xmax - xmin)
    }
}

fn num(doc: &Document, dict: &Dict, key: &str) -> Option<f64> {
    doc.dict_get(dict, key)
        .ok()
        .flatten()
        .and_then(|o| o.as_f64())
}

fn nums(doc: &Document, dict: &Dict, key: &str) -> Option<Vec<f64>> {
    let v = doc.dict_get(dict, key).ok().flatten()?;
    let arr = v.as_array()?;
    Some(
        arr.iter()
            .map(|o| doc.resolve(o).ok().and_then(|r| r.as_f64()).unwrap_or(0.0))
            .collect(),
    )
}

impl Sampled {
    fn new(doc: &Document, dict: &Dict, domain: Vec<f64>, data: &[u8]) -> Result<Self> {
        let size: Vec<usize> = nums(doc, dict, "Size")
            .unwrap_or_default()
            .iter()
            .map(|v| {
                #[allow(clippy::cast_possible_truncation)]
                let n = v.clamp(1.0, 1e9) as i64;
                usize::try_from(n).unwrap_or(1)
            })
            .collect();
        let range = nums(doc, dict, "Range").unwrap_or_default();
        let m = size.len();
        let n_out = range.len() / 2;
        if m == 0 || n_out == 0 {
            return Err(Error::Corrupt(
                "fonction de type 0 sans /Size ou /Range".into(),
            ));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let bps = num(doc, dict, "BitsPerSample").unwrap_or(8.0) as u32;
        if ![1, 2, 4, 8, 12, 16, 24, 32].contains(&bps) {
            return Err(Error::Corrupt(format!("BitsPerSample {bps} invalide")));
        }
        let mut encode = nums(doc, dict, "Encode").unwrap_or_default();
        if encode.len() < 2 * m {
            #[allow(clippy::cast_precision_loss)]
            {
                encode = size.iter().flat_map(|s| [0.0, (*s as f64) - 1.0]).collect();
            }
        }
        let mut decode = nums(doc, dict, "Decode").unwrap_or_default();
        if decode.len() < 2 * n_out {
            decode.clone_from(&range);
        }
        let total: usize = size
            .iter()
            .try_fold(1usize, |acc, s| acc.checked_mul(*s))
            .unwrap_or(0);
        let count = total.checked_mul(n_out).unwrap_or(0);
        if count == 0 || count > 64 * 1024 * 1024 {
            return Err(Error::Corrupt(
                "fonction de type 0 : table d'échantillons absurde".into(),
            ));
        }
        let max = if bps == 32 {
            4_294_967_295.0
        } else {
            f64::from(u32::try_from((1u64 << bps) - 1).unwrap_or(u32::MAX))
        };
        let mut samples = Vec::with_capacity(count);
        let mut reader = BitReader { data, pos: 0 };
        for _ in 0..count {
            // Données tronquées : complétées par 0 (tolérance).
            let v = reader.read(bps).unwrap_or(0);
            #[allow(clippy::cast_possible_truncation)]
            samples.push((f64::from(v) / max) as f32);
        }
        Ok(Self {
            domain,
            range,
            size,
            bps,
            encode,
            decode,
            samples,
            n_out,
        })
    }

    /// Évaluation avec interpolation multilinéaire sur la première entrée
    /// (linéaire) et plus proche voisin au-delà de deux dimensions pour
    /// rester simple et rapide ; en pratique les fonctions à plus de deux
    /// entrées sont rarissimes.
    #[allow(clippy::many_single_char_names)] // notation de la spec (m, e, d…)
    fn eval(&self, inputs: &[f64]) -> Vec<f64> {
        let m = self.size.len();
        // Coordonnées d'échantillon réelles pour chaque entrée.
        let mut coords = Vec::with_capacity(m);
        for i in 0..m {
            let x = clamp_domain(inputs.get(i).copied().unwrap_or(0.0), &self.domain, i);
            let d0 = self.domain.get(2 * i).copied().unwrap_or(0.0);
            let d1 = self.domain.get(2 * i + 1).copied().unwrap_or(1.0);
            let e0 = self.encode.get(2 * i).copied().unwrap_or(0.0);
            #[allow(clippy::cast_precision_loss)]
            let e1 = self
                .encode
                .get(2 * i + 1)
                .copied()
                .unwrap_or(self.size[i] as f64 - 1.0);
            #[allow(clippy::cast_precision_loss)]
            let e = interpolate(x, d0, d1, e0, e1).clamp(0.0, self.size[i] as f64 - 1.0);
            coords.push(e);
        }
        let sample = |idx: &[usize], j: usize| -> f64 {
            let mut offset = 0usize;
            let mut stride = 1usize;
            for (i, &s) in self.size.iter().enumerate() {
                offset += idx[i].min(s - 1) * stride;
                stride *= s;
            }
            f64::from(*self.samples.get(offset * self.n_out + j).unwrap_or(&0.0))
        };
        // Interpolation linéaire sur la première dimension, plus proche voisin
        // sur les autres.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let mut idx: Vec<usize> = coords.iter().map(|c| c.round() as usize).collect();
        let x0 = coords[0].floor();
        let frac = coords[0] - x0;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let i0 = x0 as usize;
        let i1 = (i0 + 1).min(self.size[0] - 1);
        let mut out = Vec::with_capacity(self.n_out);
        for j in 0..self.n_out {
            idx[0] = i0;
            let a = sample(&idx, j);
            idx[0] = i1;
            let b = sample(&idx, j);
            let s = a + frac * (b - a);
            let dmin = self.decode.get(2 * j).copied().unwrap_or(0.0);
            let dmax = self.decode.get(2 * j + 1).copied().unwrap_or(1.0);
            let v = dmin + s * (dmax - dmin);
            out.push(clamp_domain(v, &self.range, j));
        }
        let _ = self.bps;
        out
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize, // en bits
}

impl BitReader<'_> {
    fn read(&mut self, bits: u32) -> Option<u32> {
        let mut v: u64 = 0;
        for _ in 0..bits {
            let byte = *self.data.get(self.pos / 8)?;
            let bit = (byte >> (7 - (self.pos % 8))) & 1;
            v = (v << 1) | u64::from(bit);
            self.pos += 1;
        }
        u32::try_from(v).ok()
    }
}

/// Compile un programme PostScript de type 4 (§7.10.5).
///
/// # Errors
/// Accolades déséquilibrées ou opérateur inconnu.
pub fn parse_postscript(src: &[u8]) -> Result<Vec<PsOp>> {
    let tokens = tokenize_ps(src);
    let mut pos = 0;
    // Le programme est entouré d'accolades.
    if tokens.first().map(String::as_str) == Some("{") {
        pos = 1;
    }
    let (ops, end) = parse_ps_block(&tokens, pos, 0)?;
    let _ = end;
    Ok(ops)
}

fn tokenize_ps(src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        match c {
            b'{' | b'}' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                out.push((c as char).to_string());
            }
            b'%' => {
                while i < src.len() && src[i] != b'\n' && src[i] != b'\r' {
                    i += 1;
                }
            }
            c if c.is_ascii_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c as char),
        }
        i += 1;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn parse_ps_block(tokens: &[String], mut pos: usize, depth: usize) -> Result<(Vec<PsOp>, usize)> {
    if depth > 64 {
        return Err(Error::Corrupt("programme PostScript trop imbriqué".into()));
    }
    let mut ops = Vec::new();
    let mut pending: Vec<Vec<PsOp>> = Vec::new();
    while pos < tokens.len() {
        let t = tokens[pos].as_str();
        pos += 1;
        match t {
            "{" => {
                let (block, next) = parse_ps_block(tokens, pos, depth + 1)?;
                pending.push(block);
                pos = next;
            }
            "}" => {
                if !pending.is_empty() {
                    return Err(Error::Corrupt("bloc `{}` non suivi de if/ifelse".into()));
                }
                return Ok((ops, pos));
            }
            "if" => {
                let Some(b) = pending.pop() else {
                    return Err(Error::Corrupt("`if` sans bloc".into()));
                };
                ops.push(PsOp::If(b));
            }
            "ifelse" => {
                let (Some(b2), Some(b1)) = (pending.pop(), pending.pop()) else {
                    return Err(Error::Corrupt("`ifelse` sans deux blocs".into()));
                };
                ops.push(PsOp::IfElse(b1, b2));
            }
            _ => {
                if !pending.is_empty() {
                    return Err(Error::Corrupt("bloc `{}` non suivi de if/ifelse".into()));
                }
                if let Ok(v) = t.parse::<f64>() {
                    ops.push(PsOp::Push(v));
                } else if let Some(v) = parse_radix(t) {
                    ops.push(PsOp::Push(v));
                } else {
                    ops.push(PsOp::Op(ps_operator(t)?));
                }
            }
        }
    }
    if !pending.is_empty() {
        return Err(Error::Corrupt("bloc `{}` non suivi de if/ifelse".into()));
    }
    Ok((ops, pos))
}

/// Nombres PostScript en base explicite (`16#FF`), rares mais légaux.
#[allow(clippy::cast_precision_loss)]
fn parse_radix(t: &str) -> Option<f64> {
    let (base, digits) = t.split_once('#')?;
    let base: u32 = base.parse().ok()?;
    if !(2..=36).contains(&base) {
        return None;
    }
    i64::from_str_radix(digits, base).ok().map(|v| v as f64) // précision suffisante pour un littéral
}

fn ps_operator(t: &str) -> Result<PsOperator> {
    use PsOperator::{
        Abs, Add, And, Atan, Bitshift, Ceiling, Copy, Cos, Cvi, Cvr, Div, Dup, Eq, Exch, Exp,
        False, Floor, Ge, Gt, Idiv, Index, Le, Ln, Log, Lt, Mod, Mul, Ne, Neg, Not, Or, Pop, Roll,
        Round, Sin, Sqrt, Sub, True, Truncate, Xor,
    };
    Ok(match t {
        "abs" => Abs,
        "add" => Add,
        "atan" => Atan,
        "ceiling" => Ceiling,
        "cos" => Cos,
        "cvi" => Cvi,
        "cvr" => Cvr,
        "div" => Div,
        "exp" => Exp,
        "floor" => Floor,
        "idiv" => Idiv,
        "ln" => Ln,
        "log" => Log,
        "mod" => Mod,
        "mul" => Mul,
        "neg" => Neg,
        "round" => Round,
        "sin" => Sin,
        "sqrt" => Sqrt,
        "sub" => Sub,
        "truncate" => Truncate,
        "and" => And,
        "bitshift" => Bitshift,
        "eq" => Eq,
        "false" => False,
        "ge" => Ge,
        "gt" => Gt,
        "le" => Le,
        "lt" => Lt,
        "ne" => Ne,
        "not" => Not,
        "or" => Or,
        "true" => True,
        "xor" => Xor,
        "copy" => Copy,
        "dup" => Dup,
        "exch" => Exch,
        "index" => Index,
        "pop" => Pop,
        "roll" => Roll,
        other => {
            return Err(Error::Corrupt(format!(
                "opérateur PostScript inconnu `{other}`"
            )))
        }
    })
}

#[allow(clippy::too_many_lines, clippy::float_cmp)] // comparaisons exactes à 0 voulues
fn exec_postscript(program: &[PsOp], stack: &mut Vec<f64>, steps: &mut usize) {
    let pop = |s: &mut Vec<f64>| s.pop().unwrap_or(0.0);
    let bool_of = |v: f64| v != 0.0;
    let push_bool = |s: &mut Vec<f64>, b: bool| s.push(if b { 1.0 } else { 0.0 });
    for op in program {
        *steps += 1;
        if *steps > MAX_STEPS || stack.len() > MAX_STACK {
            return;
        }
        match op {
            PsOp::Push(v) => stack.push(*v),
            PsOp::If(block) => {
                if bool_of(pop(stack)) {
                    exec_postscript(block, stack, steps);
                }
            }
            PsOp::IfElse(b1, b2) => {
                if bool_of(pop(stack)) {
                    exec_postscript(b1, stack, steps);
                } else {
                    exec_postscript(b2, stack, steps);
                }
            }
            PsOp::Op(o) => {
                use PsOperator as P;
                match o {
                    P::Abs => {
                        let a = pop(stack);
                        stack.push(a.abs());
                    }
                    P::Add => {
                        let b = pop(stack);
                        let a = pop(stack);
                        stack.push(a + b);
                    }
                    P::Sub => {
                        let b = pop(stack);
                        let a = pop(stack);
                        stack.push(a - b);
                    }
                    P::Mul => {
                        let b = pop(stack);
                        let a = pop(stack);
                        stack.push(a * b);
                    }
                    P::Div => {
                        let b = pop(stack);
                        let a = pop(stack);
                        stack.push(if b == 0.0 { 0.0 } else { a / b });
                    }
                    P::Idiv => {
                        let b = pop(stack).trunc();
                        let a = pop(stack).trunc();
                        stack.push(if b == 0.0 { 0.0 } else { (a / b).trunc() });
                    }
                    P::Mod => {
                        let b = pop(stack).trunc();
                        let a = pop(stack).trunc();
                        stack.push(if b == 0.0 { 0.0 } else { a % b });
                    }
                    P::Neg => {
                        let a = pop(stack);
                        stack.push(-a);
                    }
                    P::Atan => {
                        let den = pop(stack);
                        let num = pop(stack);
                        let mut deg = num.atan2(den).to_degrees();
                        if deg < 0.0 {
                            deg += 360.0;
                        }
                        stack.push(deg);
                    }
                    P::Ceiling => {
                        let a = pop(stack);
                        stack.push(a.ceil());
                    }
                    P::Floor => {
                        let a = pop(stack);
                        stack.push(a.floor());
                    }
                    P::Round => {
                        let a = pop(stack);
                        stack.push((a + 0.5).floor());
                    }
                    P::Cos => {
                        let a = pop(stack);
                        stack.push(a.to_radians().cos());
                    }
                    P::Sin => {
                        let a = pop(stack);
                        stack.push(a.to_radians().sin());
                    }
                    P::Sqrt => {
                        let a = pop(stack);
                        stack.push(a.max(0.0).sqrt());
                    }
                    P::Exp => {
                        let e = pop(stack);
                        let b = pop(stack);
                        stack.push(b.powf(e));
                    }
                    P::Ln => {
                        let a = pop(stack);
                        stack.push(if a > 0.0 { a.ln() } else { 0.0 });
                    }
                    P::Log => {
                        let a = pop(stack);
                        stack.push(if a > 0.0 { a.log10() } else { 0.0 });
                    }
                    P::Cvi | P::Truncate => {
                        let a = pop(stack);
                        stack.push(a.trunc());
                    }
                    P::Cvr => {}
                    P::And => {
                        let b = pop(stack);
                        let a = pop(stack);
                        stack.push(f64::from(to_i32(a) & to_i32(b)));
                    }
                    P::Or => {
                        let b = pop(stack);
                        let a = pop(stack);
                        stack.push(f64::from(to_i32(a) | to_i32(b)));
                    }
                    P::Xor => {
                        let b = pop(stack);
                        let a = pop(stack);
                        stack.push(f64::from(to_i32(a) ^ to_i32(b)));
                    }
                    P::Not => {
                        let a = pop(stack);
                        // Sur un booléen : négation logique ; sur un entier : complément.
                        if a == 0.0 {
                            stack.push(1.0);
                        } else if a == 1.0 {
                            stack.push(0.0);
                        } else {
                            stack.push(f64::from(!to_i32(a)));
                        }
                    }
                    P::Bitshift => {
                        let shift = to_i32(pop(stack));
                        let a = to_i32(pop(stack));
                        let v = if shift >= 0 {
                            a.checked_shl(u32::try_from(shift).unwrap_or(31).min(31))
                                .unwrap_or(0)
                        } else {
                            a >> u32::try_from(-shift).unwrap_or(31).min(31)
                        };
                        stack.push(f64::from(v));
                    }
                    P::Eq => {
                        let b = pop(stack);
                        let a = pop(stack);
                        push_bool(stack, (a - b).abs() < 1e-12);
                    }
                    P::Ne => {
                        let b = pop(stack);
                        let a = pop(stack);
                        push_bool(stack, (a - b).abs() >= 1e-12);
                    }
                    P::Gt => {
                        let b = pop(stack);
                        let a = pop(stack);
                        push_bool(stack, a > b);
                    }
                    P::Ge => {
                        let b = pop(stack);
                        let a = pop(stack);
                        push_bool(stack, a >= b);
                    }
                    P::Lt => {
                        let b = pop(stack);
                        let a = pop(stack);
                        push_bool(stack, a < b);
                    }
                    P::Le => {
                        let b = pop(stack);
                        let a = pop(stack);
                        push_bool(stack, a <= b);
                    }
                    P::True => stack.push(1.0),
                    P::False => stack.push(0.0),
                    P::Pop => {
                        pop(stack);
                    }
                    P::Dup => {
                        let a = stack.last().copied().unwrap_or(0.0);
                        stack.push(a);
                    }
                    P::Exch => {
                        let b = pop(stack);
                        let a = pop(stack);
                        stack.push(b);
                        stack.push(a);
                    }
                    P::Copy => {
                        let n = to_usize(pop(stack));
                        let len = stack.len();
                        if n <= len && n <= MAX_STACK {
                            for i in 0..n {
                                stack.push(stack[len - n + i]);
                            }
                        }
                    }
                    P::Index => {
                        let n = to_usize(pop(stack));
                        let len = stack.len();
                        let v = if n < len { stack[len - 1 - n] } else { 0.0 };
                        stack.push(v);
                    }
                    P::Roll => {
                        let j = to_i32(pop(stack));
                        let n = to_usize(pop(stack));
                        let len = stack.len();
                        if n > 0 && n <= len {
                            let slice = &mut stack[len - n..];
                            let j = j.rem_euclid(i32::try_from(n).unwrap_or(1));
                            slice.rotate_right(usize::try_from(j).unwrap_or(0));
                        }
                    }
                }
            }
        }
    }
}

fn to_i32(v: f64) -> i32 {
    if v.is_nan() {
        0
    } else {
        #[allow(clippy::cast_possible_truncation)]
        {
            v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
        }
    }
}

fn to_usize(v: f64) -> usize {
    usize::try_from(to_i32(v).max(0)).unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn doc_with(objects: &[(u32, &str)]) -> Document {
        let mut all = vec![(1, "<< /Type /Catalog >>")];
        all.extend_from_slice(objects);
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.7\n");
        for (n, body) in &all {
            out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        // Pas de xref : la réparation reconstruit tout.
        Document::from_bytes(out).unwrap()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn exponential() {
        let doc = doc_with(&[(
            2,
            "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 1] /C1 [1 0 0] /N 1 >>",
        )]);
        let f = Function::parse(
            &doc,
            &Object::Reference(acrux_document::ObjectRef {
                number: 2,
                generation: 0,
            }),
        )
        .unwrap();
        assert_eq!(f.outputs(), Some(3));
        let v = f.eval(&[0.25]);
        assert!(close(v[0], 0.25) && close(v[1], 0.0) && close(v[2], 0.75));
        assert!(close(f.eval(&[7.0])[0], 1.0), "entrée bornée au domaine");
    }

    #[test]
    fn stitching() {
        let doc = doc_with(&[(
            2,
            "<< /FunctionType 3 /Domain [0 1] /Functions [ << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >> << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >> ] /Bounds [0.5] /Encode [0 1 0 1] >>",
        )]);
        let f = Function::parse(
            &doc,
            &Object::Reference(acrux_document::ObjectRef {
                number: 2,
                generation: 0,
            }),
        )
        .unwrap();
        assert!(close(f.eval(&[0.25])[0], 0.5));
        assert!(close(f.eval(&[0.5])[0], 1.0));
        assert!(close(f.eval(&[0.75])[0], 0.5));
        assert!(close(f.eval(&[1.0])[0], 0.0));
    }

    #[test]
    fn sampled_8_bits_linear_interpolation() {
        // 3 échantillons sur [0,1] : 0, 255, 0 → triangle.
        let doc = doc_with(&[(
            2,
            "<< /FunctionType 0 /Domain [0 1] /Range [0 1] /Size [3] /BitsPerSample 8 /Filter /ASCIIHexDecode /Length 7 >>\nstream\n00ff00>\nendstream",
        )]);
        let f = Function::parse(
            &doc,
            &Object::Reference(acrux_document::ObjectRef {
                number: 2,
                generation: 0,
            }),
        )
        .unwrap();
        assert!(close(f.eval(&[0.0])[0], 0.0));
        assert!(close(f.eval(&[0.5])[0], 1.0));
        assert!(close(f.eval(&[0.25])[0], 0.5));
        assert!(close(f.eval(&[1.0])[0], 0.0));
    }

    #[test]
    fn sampled_two_inputs_and_two_outputs() {
        // Size [2 2], 2 sorties, 8 bits : (x,y) → (x, y) approximativement.
        let data = "0000ff0000ffffff>";
        let doc = doc_with(&[(
            2,
            &format!("<< /FunctionType 0 /Domain [0 1 0 1] /Range [0 1 0 1] /Size [2 2] /BitsPerSample 8 /Filter /ASCIIHexDecode /Length 17 >>\nstream\n{data}\nendstream"),
        )]);
        let f = Function::parse(
            &doc,
            &Object::Reference(acrux_document::ObjectRef {
                number: 2,
                generation: 0,
            }),
        )
        .unwrap();
        let v = f.eval(&[1.0, 0.0]);
        assert!(close(v[0], 1.0) && close(v[1], 0.0));
        let v = f.eval(&[0.0, 1.0]);
        assert!(close(v[0], 0.0) && close(v[1], 1.0));
        let v = f.eval(&[0.5, 0.0]);
        assert!(close(v[0], 0.5) && close(v[1], 0.0));
    }

    #[test]
    fn postscript_calculator() {
        let doc = doc_with(&[(
            2,
            "<< /FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1] /Length 60 >>\nstream\n{ 2 copy add 2 div 3 1 roll exch dup 0.5 gt { pop 1 } { pop 0 } ifelse exch }\nendstream",
        )]);
        let f = Function::parse(
            &doc,
            &Object::Reference(acrux_document::ObjectRef {
                number: 2,
                generation: 0,
            }),
        )
        .unwrap();
        // Entrées a=0.2, b=0.8 : pile [0.2 0.8] → 2 copy add 2 div → [0.2 0.8 0.5] → 3 1 roll → [0.5 0.2 0.8]
        // → exch → [0.5 0.8 0.2] → dup 0.5 gt → 0.2 > 0.5 faux → pop 0 → [0.5 0.8 0] → exch → [0.5 0 0.8]
        let v = f.eval(&[0.2, 0.8]);
        assert_eq!(v.len(), 3);
        assert!(
            close(v[0], 0.5) && close(v[1], 0.0) && close(v[2], 0.8),
            "{v:?}"
        );
    }

    #[test]
    fn postscript_operators() {
        let run = |src: &str, inputs: &[f64]| -> Vec<f64> {
            let program = parse_postscript(src.as_bytes()).unwrap();
            let mut stack = inputs.to_vec();
            let mut steps = 0;
            exec_postscript(&program, &mut stack, &mut steps);
            stack
        };
        assert_eq!(run("{ 3 4 add 2 mul }", &[]), vec![14.0]);
        assert_eq!(run("{ 7 2 idiv 7 2 mod }", &[]), vec![3.0, 1.0]);
        assert_eq!(run("{ 1 0 atan }", &[]), vec![90.0]);
        assert_eq!(run("{ 90 sin 180 cos }", &[]), vec![1.0, -1.0]);
        assert_eq!(run("{ 1 2 3 3 -1 roll }", &[]), vec![2.0, 3.0, 1.0]);
        assert_eq!(run("{ 1 2 3 3 1 roll }", &[]), vec![3.0, 1.0, 2.0]);
        assert_eq!(run("{ 1 2 1 index }", &[]), vec![1.0, 2.0, 1.0]);
        assert_eq!(run("{ 5 3 gt { 1 } if }", &[]), vec![1.0]);
        assert_eq!(run("{ 5 3 lt { 1 } { 0 } ifelse }", &[]), vec![0.0]);
        assert_eq!(
            run("{ 6 3 and 6 3 or 6 3 xor 1 3 bitshift }", &[]),
            vec![2.0, 7.0, 5.0, 8.0]
        );
        assert_eq!(run("{ true not false not }", &[]), vec![0.0, 1.0]);
        assert_eq!(
            run(
                "{ 2.5 floor 2.5 ceiling 2.5 round -2.5 truncate 16#FF }",
                &[]
            ),
            vec![2.0, 3.0, 3.0, -2.0, 255.0]
        );
        assert_eq!(
            run("{ 1 0 div 9 sqrt 2 3 exp 100 log }", &[]),
            vec![0.0, 3.0, 8.0, 2.0]
        );
        assert_eq!(run("{ 1 2 exch pop dup }", &[]), vec![2.0, 2.0]);
    }

    #[test]
    fn postscript_errors_and_limits() {
        assert!(parse_postscript(b"{ 1 2 frobnicate }").is_err());
        assert!(parse_postscript(b"{ { 1 } }").is_err(), "bloc sans if");
        assert!(parse_postscript(b"{ 1 if }").is_err());
        // Pile bornée : `dup` en boucle ne peut pas exploser.
        let program =
            parse_postscript(&format!("{{ 1 {} }}", "dup ".repeat(5000)).into_bytes()).unwrap();
        let mut stack = Vec::new();
        let mut steps = 0;
        exec_postscript(&program, &mut stack, &mut steps);
        assert!(stack.len() <= MAX_STACK + 1);
    }

    #[test]
    fn identity_and_array() {
        let doc = doc_with(&[(
            2,
            "[ << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >> << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >> ]",
        )]);
        let f = Function::parse(
            &doc,
            &Object::Reference(acrux_document::ObjectRef {
                number: 2,
                generation: 0,
            }),
        )
        .unwrap();
        assert_eq!(f.outputs(), Some(2));
        let v = f.eval(&[0.25]);
        assert!(close(v[0], 0.25) && close(v[1], 0.75));
        let id =
            Function::parse(&doc, &Object::Name(acrux_document::Name::new("Identity"))).unwrap();
        assert_eq!(id.eval(&[0.3, 0.6]), vec![0.3, 0.6]);
    }
}
