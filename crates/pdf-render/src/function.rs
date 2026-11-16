//! PDF functions (ISO 32000-2, 7.10): sampled (type 0), exponential
//! (type 2), stitching (type 3) and PostScript calculator (type 4), as the
//! tint transforms of Separation and DeviceN colour spaces use them
//! (tranche 3.15). Read under the budget and evaluated without loops: a
//! sample table is bounded before it is read, stitching is at most
//! `MAX_DEPTH` deep, and a calculator program, which has no loop operator,
//! runs each of its at most `MAX_PROGRAM` operators at most once.

use pdf_syntax::{Budget, Dictionary, IndirectValue, Object, PdfFile, Source, Stream};

use crate::Error;

/// Inputs and outputs of a function at most (DeviceN has at most 32
/// colorants, 8.6.6.5).
pub(crate) const MAX_ARITY: usize = 32;
/// Samples of a type 0 function at most (all outputs counted).
const MAX_SAMPLES: u64 = 1 << 22;
/// Inputs a sampled function interpolates; with more, the nearest sample.
const MAX_INTERPOLATED: usize = 6;
/// Nesting of stitching functions at most.
const MAX_DEPTH: u32 = 4;
/// Operators of a calculator program at most, and its operand stack.
const MAX_PROGRAM: usize = 4096;
const MAX_STACK: usize = 100;

/// A function of `domain.len()` inputs and `outputs` outputs.
#[derive(Debug, Clone)]
pub(crate) struct Function {
    domain: Vec<(f64, f64)>,
    range: Option<Vec<(f64, f64)>>,
    outputs: usize,
    kind: Kind,
    /// Steps one evaluation takes at most (operators, samples, terms):
    /// what a caller charges to its operation budget.
    cost: u64,
}

#[derive(Debug, Clone)]
enum Kind {
    Sampled {
        size: Vec<usize>,
        encode: Vec<(f64, f64)>,
        decode: Vec<(f64, f64)>,
        /// Samples normalised to 0..1, outputs innermost, first input
        /// varying fastest (7.10.2).
        samples: Vec<f32>,
    },
    Exponential {
        c0: Vec<f64>,
        c1: Vec<f64>,
        n: f64,
    },
    Stitching {
        functions: Vec<Function>,
        bounds: Vec<f64>,
        encode: Vec<(f64, f64)>,
    },
    Calculator(Vec<Op>),
}

impl Function {
    pub(crate) fn inputs(&self) -> usize {
        self.domain.len()
    }

    pub(crate) fn outputs(&self) -> usize {
        self.outputs
    }

    pub(crate) fn cost(&self) -> u64 {
        self.cost
    }

    /// Evaluates the function: inputs clipped to the domain, outputs to
    /// the range. `None` when the input count is wrong or the evaluation
    /// fails (a calculator error, a result that is not a finite number).
    pub(crate) fn eval(&self, input: &[f64]) -> Option<Vec<f64>> {
        if input.len() != self.domain.len() {
            return None;
        }
        let x: Vec<f64> = input
            .iter()
            .zip(&self.domain)
            .map(|(v, (lo, hi))| v.clamp(*lo, *hi))
            .collect();
        let mut out = match &self.kind {
            Kind::Exponential { c0, c1, n } => {
                let x = *x.first()?;
                let power = x.powf(*n);
                if !power.is_finite() {
                    return None;
                }
                c0.iter()
                    .zip(c1)
                    .map(|(a, b)| a + power * (b - a))
                    .collect()
            }
            Kind::Stitching {
                functions,
                bounds,
                encode,
            } => {
                let x = *x.first()?;
                let (lo, hi) = *self.domain.first()?;
                // The subdomain of x: bounds split the domain in order.
                let index = bounds.iter().take_while(|b| x >= **b).count();
                let start = if index == 0 {
                    lo
                } else {
                    *bounds.get(index - 1)?
                };
                let end = bounds.get(index).copied().unwrap_or(hi);
                let (e0, e1) = *encode.get(index)?;
                let t = interpolate(x, start, end, e0, e1);
                functions.get(index)?.eval(&[t])?
            }
            Kind::Sampled {
                size,
                encode,
                decode,
                samples,
            } => sampled(
                &x,
                &self.domain,
                size,
                encode,
                decode,
                samples,
                self.outputs,
            )?,
            Kind::Calculator(program) => {
                let mut stack: Vec<Value> = x.iter().map(|v| Value::Number(*v)).collect();
                run(program, &mut stack)?;
                if stack.len() != self.outputs {
                    return None;
                }
                stack
                    .into_iter()
                    .map(|v| match v {
                        Value::Number(n) => Some(n),
                        Value::Bool(_) => None,
                    })
                    .collect::<Option<Vec<f64>>>()?
            }
        };
        if out.iter().any(|v| !v.is_finite()) {
            return None;
        }
        if let Some(range) = &self.range {
            for (v, (lo, hi)) in out.iter_mut().zip(range) {
                *v = v.clamp(*lo, *hi);
            }
        }
        Some(out)
    }
}

/// `x` from [x0, x1] onto [y0, y1] (7.10.2); `y0` for an empty interval.
fn interpolate(x: f64, x0: f64, x1: f64, y0: f64, y1: f64) -> f64 {
    if x1 == x0 {
        y0
    } else {
        y0 + (x - x0) * (y1 - y0) / (x1 - x0)
    }
}

fn sampled(
    x: &[f64],
    domain: &[(f64, f64)],
    size: &[usize],
    encode: &[(f64, f64)],
    decode: &[(f64, f64)],
    samples: &[f32],
    outputs: usize,
) -> Option<Vec<f64>> {
    // Each input onto its sample index (fractional), clipped to the table.
    let mut position = Vec::with_capacity(x.len());
    for (((v, (d0, d1)), (e0, e1)), n) in x.iter().zip(domain).zip(encode).zip(size) {
        let e = interpolate(*v, *d0, *d1, *e0, *e1);
        position.push(e.clamp(0.0, (*n - 1) as f64));
    }
    let sample = |indices: &[usize], output: usize| -> Option<f64> {
        let mut offset = 0usize;
        let mut stride = 1usize;
        for (index, n) in indices.iter().zip(size) {
            offset = offset.checked_add(index.checked_mul(stride)?)?;
            stride = stride.checked_mul(*n)?;
        }
        samples
            .get(offset.checked_mul(outputs)?.checked_add(output)?)
            .map(|s| f64::from(*s))
    };
    let mut out = Vec::with_capacity(outputs);
    for output in 0..outputs {
        let value = if position.len() <= MAX_INTERPOLATED {
            // Multilinear interpolation over the 2^m corners around x.
            let mut total = 0.0;
            for corner in 0..(1usize << position.len()) {
                let mut weight = 1.0;
                let mut indices = Vec::with_capacity(position.len());
                for (bit, (p, n)) in position.iter().zip(size).enumerate() {
                    let low = p.floor();
                    let fraction = p - low;
                    let low = low as usize;
                    let high = (low + 1).min(n - 1);
                    if corner >> bit & 1 == 1 {
                        weight *= fraction;
                        indices.push(high);
                    } else {
                        weight *= 1.0 - fraction;
                        indices.push(low);
                    }
                }
                if weight > 0.0 {
                    total += weight * sample(&indices, output)?;
                }
            }
            total
        } else {
            let indices: Vec<usize> = position.iter().map(|p| p.round() as usize).collect();
            sample(&indices, output)?
        };
        let (d0, d1) = *decode.get(output)?;
        out.push(d0 + value * (d1 - d0));
    }
    Some(out)
}

/// Reads the function `object` (a dictionary or a stream, possibly by
/// reference). `None` when it is not a function this module evaluates or
/// is damaged; limits and cancellation are errors.
pub(crate) fn read<S: Source>(
    file: &PdfFile<S>,
    object: &Object,
    budget: &mut Budget,
) -> Result<Option<Function>, Error> {
    read_at(file, object, budget, 0)
}

fn read_at<S: Source>(
    file: &PdfFile<S>,
    object: &Object,
    budget: &mut Budget,
    depth: u32,
) -> Result<Option<Function>, Error> {
    if depth > MAX_DEPTH {
        return Ok(None);
    }
    let (dictionary, stream) = match object {
        Object::Dictionary(dictionary) => (dictionary.clone(), None),
        Object::Reference(id) => match file.resolve(*id, budget) {
            Ok(IndirectValue::Object(Object::Dictionary(dictionary))) => (dictionary, None),
            Ok(IndirectValue::Stream(stream)) => (stream.dictionary.clone(), Some(stream)),
            Ok(_) => return Ok(None),
            Err(pdf_syntax::Error::Malformed { .. } | pdf_syntax::Error::Truncated { .. }) => {
                return Ok(None);
            }
            Err(other) => return Err(Error::Syntax(other)),
        },
        _ => return Ok(None),
    };
    let Entry::Value(domain) = pairs(file, dictionary.get(b"Domain"), budget)? else {
        return Ok(None);
    };
    if domain.is_empty() || domain.len() > MAX_ARITY || !ordered(&domain) {
        return Ok(None);
    }
    let range = match pairs(file, dictionary.get(b"Range"), budget)? {
        Entry::Absent => None,
        Entry::Value(range) if !range.is_empty() && range.len() <= MAX_ARITY && ordered(&range) => {
            Some(range)
        }
        _ => return Ok(None),
    };
    let function_type = dictionary.get(b"FunctionType").and_then(Object::as_integer);
    let kind_and_outputs = match function_type {
        Some(0) => match (&stream, &range) {
            (Some(stream), Some(range)) => {
                sampled_kind(file, &dictionary, stream, &domain, range, budget)?
            }
            _ => None,
        },
        Some(2) => exponential_kind(file, &dictionary, &domain, budget)?,
        Some(3) => stitching_kind(file, &dictionary, &domain, budget, depth)?,
        Some(4) => match (&stream, &range) {
            (Some(stream), Some(range)) => calculator_kind(file, stream, range, budget)?,
            _ => None,
        },
        _ => None,
    };
    let Some((kind, outputs)) = kind_and_outputs else {
        return Ok(None);
    };
    if outputs == 0 || outputs > MAX_ARITY || range.as_ref().is_some_and(|r| r.len() != outputs) {
        return Ok(None);
    }
    let cost = match &kind {
        Kind::Exponential { .. } => outputs as u64,
        Kind::Sampled { size, .. } => {
            let corners = if size.len() <= MAX_INTERPOLATED {
                1u64 << size.len()
            } else {
                1
            };
            (outputs as u64).saturating_mul(corners)
        }
        Kind::Stitching { functions, .. } => {
            1 + functions.iter().map(Function::cost).max().unwrap_or(0)
        }
        Kind::Calculator(program) => program_size(program),
    };
    Ok(Some(Function {
        domain,
        range,
        outputs,
        kind,
        cost,
    }))
}

/// An optional entry as read: absent (its default applies), invalid (the
/// function is refused), or its value.
enum Entry<T> {
    Absent,
    Invalid,
    Value(T),
}

impl<T> Entry<T> {
    /// The value, the default when absent, `None` when invalid.
    fn or(self, default: T) -> Option<T> {
        match self {
            Entry::Absent => Some(default),
            Entry::Invalid => None,
            Entry::Value(value) => Some(value),
        }
    }
}

/// `object` resolved once, if it is a direct value or a reference to one.
fn resolved<S: Source>(
    file: &PdfFile<S>,
    object: Option<&Object>,
    budget: &mut Budget,
) -> Result<Option<Object>, Error> {
    match object {
        None => Ok(None),
        Some(Object::Reference(id)) => match file.resolve(*id, budget) {
            Ok(IndirectValue::Object(object)) => Ok(Some(object)),
            Ok(IndirectValue::Stream(_)) => Ok(None),
            Err(pdf_syntax::Error::Malformed { .. } | pdf_syntax::Error::Truncated { .. }) => {
                Ok(None)
            }
            Err(other) => Err(Error::Syntax(other)),
        },
        Some(other) => Ok(Some(other.clone())),
    }
}

/// An array of finite numbers.
fn reals<S: Source>(
    file: &PdfFile<S>,
    object: Option<&Object>,
    budget: &mut Budget,
) -> Result<Entry<Vec<f64>>, Error> {
    if object.is_none() {
        return Ok(Entry::Absent);
    }
    let Some(Object::Array(items)) = resolved(file, object, budget)? else {
        return Ok(Entry::Invalid);
    };
    if items.len() > 2 * MAX_ARITY.max(MAX_STITCHED) {
        return Ok(Entry::Invalid);
    }
    let values: Option<Vec<f64>> = items
        .iter()
        .map(|item| match item {
            Object::Integer(v) => Some(*v as f64),
            Object::Real(v) if v.is_finite() => Some(*v),
            _ => None,
        })
        .collect();
    Ok(values.map_or(Entry::Invalid, Entry::Value))
}

/// An array of numbers read in pairs.
fn pairs<S: Source>(
    file: &PdfFile<S>,
    object: Option<&Object>,
    budget: &mut Budget,
) -> Result<Entry<Vec<(f64, f64)>>, Error> {
    Ok(match reals(file, object, budget)? {
        Entry::Value(values) if values.len() % 2 == 0 => Entry::Value(
            values
                .as_chunks::<2>()
                .0
                .iter()
                .map(|[a, b]| (*a, *b))
                .collect(),
        ),
        Entry::Absent => Entry::Absent,
        _ => Entry::Invalid,
    })
}

/// Intervals whose ends are in order: a domain or a range (7.10.1), whose
/// clipping needs its minimum at most its maximum.
fn ordered(intervals: &[(f64, f64)]) -> bool {
    intervals.iter().all(|(lo, hi)| lo <= hi)
}

/// Functions a stitching function joins at most.
const MAX_STITCHED: usize = 256;

fn exponential_kind<S: Source>(
    file: &PdfFile<S>,
    dictionary: &Dictionary,
    domain: &[(f64, f64)],
    budget: &mut Budget,
) -> Result<Option<(Kind, usize)>, Error> {
    if domain.len() != 1 {
        return Ok(None);
    }
    let Some(c0) = reals(file, dictionary.get(b"C0"), budget)?.or(vec![0.0]) else {
        return Ok(None);
    };
    let Some(c1) = reals(file, dictionary.get(b"C1"), budget)?.or(vec![1.0]) else {
        return Ok(None);
    };
    let Some(n) = resolved(file, dictionary.get(b"N"), budget)?.and_then(|n| match n {
        Object::Integer(v) => Some(v as f64),
        Object::Real(v) if v.is_finite() => Some(v),
        _ => None,
    }) else {
        return Ok(None);
    };
    if c0.len() != c1.len() || c0.is_empty() {
        return Ok(None);
    }
    let outputs = c0.len();
    Ok(Some((Kind::Exponential { c0, c1, n }, outputs)))
}

fn stitching_kind<S: Source>(
    file: &PdfFile<S>,
    dictionary: &Dictionary,
    domain: &[(f64, f64)],
    budget: &mut Budget,
    depth: u32,
) -> Result<Option<(Kind, usize)>, Error> {
    if domain.len() != 1 {
        return Ok(None);
    }
    let Some(Object::Array(items)) = resolved(file, dictionary.get(b"Functions"), budget)? else {
        return Ok(None);
    };
    if items.is_empty() || items.len() > MAX_STITCHED {
        return Ok(None);
    }
    let mut functions = Vec::with_capacity(items.len());
    for item in &items {
        match read_at(file, item, budget, depth + 1)? {
            Some(function) if function.inputs() == 1 => functions.push(function),
            _ => return Ok(None),
        }
    }
    let outputs = functions.first().map_or(0, Function::outputs);
    if functions.iter().any(|f| f.outputs() != outputs) {
        return Ok(None);
    }
    let Some(bounds) = reals(file, dictionary.get(b"Bounds"), budget)?.or(Vec::new()) else {
        return Ok(None);
    };
    let Entry::Value(encode) = pairs(file, dictionary.get(b"Encode"), budget)? else {
        return Ok(None);
    };
    let Some((lo, hi)) = domain.first().copied() else {
        return Ok(None);
    };
    // In order, within the domain (7.10.4).
    let mut previous = lo;
    for bound in &bounds {
        if *bound < previous || *bound > hi {
            return Ok(None);
        }
        previous = *bound;
    }
    if bounds.len() + 1 != functions.len() || encode.len() != functions.len() {
        return Ok(None);
    }
    Ok(Some((
        Kind::Stitching {
            functions,
            bounds,
            encode,
        },
        outputs,
    )))
}

fn sampled_kind<S: Source>(
    file: &PdfFile<S>,
    dictionary: &Dictionary,
    stream: &Stream,
    domain: &[(f64, f64)],
    range: &[(f64, f64)],
    budget: &mut Budget,
) -> Result<Option<(Kind, usize)>, Error> {
    let inputs = domain.len();
    let outputs = range.len();
    let Entry::Value(size) = reals(file, dictionary.get(b"Size"), budget)? else {
        return Ok(None);
    };
    let size: Option<Vec<usize>> = size
        .iter()
        .map(|n| (n.fract() == 0.0 && *n >= 1.0 && *n <= MAX_SAMPLES as f64).then_some(*n as usize))
        .collect();
    let Some(size) = size.filter(|s| s.len() == inputs) else {
        return Ok(None);
    };
    let bits = dictionary
        .get(b"BitsPerSample")
        .and_then(Object::as_integer);
    let Some(bits) = bits.filter(|b| matches!(b, 1 | 2 | 4 | 8 | 12 | 16 | 24 | 32)) else {
        return Ok(None);
    };
    let bits = bits as u64;
    // Samples counted before the table is read.
    let mut count: u64 = outputs as u64;
    for n in &size {
        count = count.saturating_mul(*n as u64);
    }
    if count > MAX_SAMPLES {
        return Ok(None);
    }
    let encode = match pairs(file, dictionary.get(b"Encode"), budget)? {
        Entry::Value(encode) if encode.len() == inputs => encode,
        Entry::Absent => size.iter().map(|n| (0.0, (*n - 1) as f64)).collect(),
        _ => return Ok(None),
    };
    let decode = match pairs(file, dictionary.get(b"Decode"), budget)? {
        Entry::Value(decode) if decode.len() == outputs => decode,
        Entry::Absent => range.to_vec(),
        _ => return Ok(None),
    };
    let data = match file.decoded_stream_data(stream, budget) {
        Ok(data) => data,
        Err(
            pdf_syntax::Error::Malformed { .. }
            | pdf_syntax::Error::Truncated { .. }
            | pdf_syntax::Error::Unsupported(_),
        ) => return Ok(None),
        Err(other) => return Err(Error::Syntax(other)),
    };
    // The table, unpacked: 4 bytes per sample, charged before allocation.
    budget.charge_decoded_bytes(count.saturating_mul(4))?;
    let max = ((1u64 << bits) - 1) as f64;
    let mut samples = Vec::with_capacity(count as usize);
    let mut reader = BitReader::new(&data);
    for _ in 0..count {
        // Data too short for the table: the function is damaged.
        let Some(value) = reader.read(bits) else {
            return Ok(None);
        };
        samples.push((value as f64 / max) as f32);
    }
    Ok(Some((
        Kind::Sampled {
            size,
            encode,
            decode,
            samples,
        },
        outputs,
    )))
}

/// Unsigned big-endian samples of `bits` bits each, packed without padding.
struct BitReader<'a> {
    data: &'a [u8],
    bit: u64,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }

    fn read(&mut self, bits: u64) -> Option<u64> {
        let mut value = 0u64;
        for _ in 0..bits {
            let byte = *self.data.get(usize::try_from(self.bit / 8).ok()?)?;
            let bit = (byte >> (7 - (self.bit % 8))) & 1;
            value = (value << 1) | u64::from(bit);
            self.bit += 1;
        }
        Some(value)
    }
}

// Type 4: PostScript calculator (7.10.5).

#[derive(Debug, Clone, Copy, PartialEq)]
enum Value {
    Number(f64),
    Bool(bool),
}

#[derive(Debug, Clone)]
enum Op {
    Push(Value),
    Operator(Operator),
    /// `{ ... } if`
    If(Vec<Op>),
    /// `{ ... } { ... } ifelse`
    IfElse(Vec<Op>, Vec<Op>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Operator {
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
    Ge,
    Gt,
    Le,
    Lt,
    Ne,
    Not,
    Or,
    Xor,
    Copy,
    Dup,
    Exch,
    Index,
    Pop,
    Roll,
}

fn operator(name: &[u8]) -> Option<Operator> {
    use Operator::*;
    Some(match name {
        b"abs" => Abs,
        b"add" => Add,
        b"atan" => Atan,
        b"ceiling" => Ceiling,
        b"cos" => Cos,
        b"cvi" => Cvi,
        b"cvr" => Cvr,
        b"div" => Div,
        b"exp" => Exp,
        b"floor" => Floor,
        b"idiv" => Idiv,
        b"ln" => Ln,
        b"log" => Log,
        b"mod" => Mod,
        b"mul" => Mul,
        b"neg" => Neg,
        b"round" => Round,
        b"sin" => Sin,
        b"sqrt" => Sqrt,
        b"sub" => Sub,
        b"truncate" => Truncate,
        b"and" => And,
        b"bitshift" => Bitshift,
        b"eq" => Eq,
        b"ge" => Ge,
        b"gt" => Gt,
        b"le" => Le,
        b"lt" => Lt,
        b"ne" => Ne,
        b"not" => Not,
        b"or" => Or,
        b"xor" => Xor,
        b"copy" => Copy,
        b"dup" => Dup,
        b"exch" => Exch,
        b"index" => Index,
        b"pop" => Pop,
        b"roll" => Roll,
        _ => return None,
    })
}

fn calculator_kind<S: Source>(
    file: &PdfFile<S>,
    stream: &Stream,
    range: &[(f64, f64)],
    budget: &mut Budget,
) -> Result<Option<(Kind, usize)>, Error> {
    let data = match file.decoded_stream_data(stream, budget) {
        Ok(data) => data,
        Err(
            pdf_syntax::Error::Malformed { .. }
            | pdf_syntax::Error::Truncated { .. }
            | pdf_syntax::Error::Unsupported(_),
        ) => return Ok(None),
        Err(other) => return Err(Error::Syntax(other)),
    };
    let tokens = tokenize(&data);
    let mut at = 0;
    let mut count = 0;
    // The whole program is one block: `{ ... }`.
    if tokens.first() != Some(&Token::Open) {
        return Ok(None);
    }
    at += 1;
    let Some(program) = block(&tokens, &mut at, &mut count, 0) else {
        return Ok(None);
    };
    budget.charge_decoded_bytes((count * std::mem::size_of::<Op>()) as u64)?;
    Ok(Some((Kind::Calculator(program), range.len())))
}

#[derive(Debug, Clone, PartialEq)]
enum Token<'a> {
    Open,
    Close,
    Number(f64),
    Word(&'a [u8]),
}

fn tokenize(data: &[u8]) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < data.len() && tokens.len() <= MAX_PROGRAM * 2 {
        let Some(&byte) = data.get(at) else {
            break;
        };
        match byte {
            b'{' => {
                tokens.push(Token::Open);
                at += 1;
            }
            b'}' => {
                tokens.push(Token::Close);
                at += 1;
            }
            b'%' => {
                while data.get(at).is_some_and(|b| *b != b'\n' && *b != b'\r') {
                    at += 1;
                }
            }
            b if b.is_ascii_whitespace() || b == 0 => at += 1,
            _ => {
                let start = at;
                while data
                    .get(at)
                    .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'{' | b'}' | b'%'))
                {
                    at += 1;
                }
                let word = data.get(start..at).unwrap_or_default();
                match std::str::from_utf8(word)
                    .ok()
                    .and_then(|w| w.parse::<f64>().ok())
                {
                    Some(n) if n.is_finite() => tokens.push(Token::Number(n)),
                    _ => tokens.push(Token::Word(word)),
                }
            }
        }
    }
    tokens
}

/// The operators of a block up to its `}` (`at` just after its `{`).
fn block(tokens: &[Token<'_>], at: &mut usize, count: &mut usize, depth: u32) -> Option<Vec<Op>> {
    if depth > 64 {
        return None;
    }
    let mut ops = Vec::new();
    // Blocks read but not yet consumed by `if` or `ifelse`.
    let mut pending: Vec<Vec<Op>> = Vec::new();
    loop {
        *count += 1;
        if *count > MAX_PROGRAM {
            return None;
        }
        let token = tokens.get(*at)?;
        *at += 1;
        match token {
            Token::Close => {
                return pending.is_empty().then_some(ops);
            }
            Token::Open => pending.push(block(tokens, at, count, depth + 1)?),
            Token::Number(n) => {
                if !pending.is_empty() {
                    return None;
                }
                ops.push(Op::Push(Value::Number(*n)));
            }
            Token::Word(word @ (b"true" | b"false")) => {
                if !pending.is_empty() {
                    return None;
                }
                ops.push(Op::Push(Value::Bool(*word == b"true")));
            }
            Token::Word(b"if") => {
                let body = pending.pop()?;
                if !pending.is_empty() {
                    return None;
                }
                ops.push(Op::If(body));
            }
            Token::Word(b"ifelse") => {
                let otherwise = pending.pop()?;
                let then = pending.pop()?;
                if !pending.is_empty() {
                    return None;
                }
                ops.push(Op::IfElse(then, otherwise));
            }
            Token::Word(word) => {
                if !pending.is_empty() {
                    return None;
                }
                ops.push(Op::Operator(operator(word)?));
            }
        }
    }
}

/// Operators of a program, branches included: what one run takes at most.
fn program_size(program: &[Op]) -> u64 {
    program
        .iter()
        .map(|op| match op {
            Op::If(body) => 1 + program_size(body),
            Op::IfElse(then, otherwise) => 1 + program_size(then) + program_size(otherwise),
            _ => 1,
        })
        .sum()
}

fn run(program: &[Op], stack: &mut Vec<Value>) -> Option<()> {
    for op in program {
        match op {
            Op::Push(value) => push(stack, *value)?,
            Op::If(body) => match stack.pop()? {
                Value::Bool(true) => run(body, stack)?,
                Value::Bool(false) => {}
                Value::Number(_) => return None,
            },
            Op::IfElse(then, otherwise) => match stack.pop()? {
                Value::Bool(true) => run(then, stack)?,
                Value::Bool(false) => run(otherwise, stack)?,
                Value::Number(_) => return None,
            },
            Op::Operator(operator) => apply(*operator, stack)?,
        }
    }
    Some(())
}

fn push(stack: &mut Vec<Value>, value: Value) -> Option<()> {
    if stack.len() >= MAX_STACK {
        return None;
    }
    stack.push(value);
    Some(())
}

fn number(stack: &mut Vec<Value>) -> Option<f64> {
    match stack.pop()? {
        Value::Number(n) => Some(n),
        Value::Bool(_) => None,
    }
}

/// An integer operand: a number with no fractional part, within i64.
fn integer(stack: &mut Vec<Value>) -> Option<i64> {
    let n = number(stack)?;
    (n.fract() == 0.0 && n.abs() < 9.0e15).then_some(n as i64)
}

fn apply(operator: Operator, stack: &mut Vec<Value>) -> Option<()> {
    use Operator::*;
    let unary = |stack: &mut Vec<Value>, f: fn(f64) -> f64| -> Option<()> {
        let a = number(stack)?;
        push(stack, Value::Number(f(a)))
    };
    let binary = |stack: &mut Vec<Value>, f: fn(f64, f64) -> f64| -> Option<()> {
        let b = number(stack)?;
        let a = number(stack)?;
        push(stack, Value::Number(f(a, b)))
    };
    match operator {
        Abs => unary(stack, f64::abs),
        Add => binary(stack, |a, b| a + b),
        Sub => binary(stack, |a, b| a - b),
        Mul => binary(stack, |a, b| a * b),
        Div => {
            let b = number(stack)?;
            let a = number(stack)?;
            if b == 0.0 {
                return None;
            }
            push(stack, Value::Number(a / b))
        }
        Idiv | Mod => {
            let b = integer(stack)?;
            let a = integer(stack)?;
            let value = if operator == Idiv {
                a.checked_div(b)?
            } else {
                a.checked_rem(b)?
            };
            push(stack, Value::Number(value as f64))
        }
        // Degrees, as PostScript (atan: 0 to 360).
        Atan => {
            let den = number(stack)?;
            let num = number(stack)?;
            if num == 0.0 && den == 0.0 {
                return None;
            }
            let degrees = num.atan2(den).to_degrees();
            push(
                stack,
                Value::Number(if degrees < 0.0 {
                    degrees + 360.0
                } else {
                    degrees
                }),
            )
        }
        Sin => unary(stack, |a| a.to_radians().sin()),
        Cos => unary(stack, |a| a.to_radians().cos()),
        Ceiling => unary(stack, f64::ceil),
        Floor => unary(stack, f64::floor),
        Round => unary(stack, |a| (a + 0.5).floor()),
        Truncate | Cvi => unary(stack, f64::trunc),
        Cvr => unary(stack, |a| a),
        Exp => binary(stack, f64::powf),
        Ln => unary(stack, f64::ln),
        Log => unary(stack, f64::log10),
        Sqrt => unary(stack, f64::sqrt),
        Neg => unary(stack, |a| -a),
        Eq | Ne => {
            let b = stack.pop()?;
            let a = stack.pop()?;
            let equal = a == b;
            push(
                stack,
                Value::Bool(if operator == Eq { equal } else { !equal }),
            )
        }
        Ge | Gt | Le | Lt => {
            let b = number(stack)?;
            let a = number(stack)?;
            let result = match operator {
                Ge => a >= b,
                Gt => a > b,
                Le => a <= b,
                _ => a < b,
            };
            push(stack, Value::Bool(result))
        }
        And | Or | Xor => {
            let b = stack.pop()?;
            let a = stack.pop()?;
            let value = match (a, b) {
                (Value::Bool(a), Value::Bool(b)) => Value::Bool(match operator {
                    And => a && b,
                    Or => a || b,
                    _ => a ^ b,
                }),
                (Value::Number(a), Value::Number(b))
                    if a.fract() == 0.0
                        && b.fract() == 0.0
                        && a.abs() < 9.0e15
                        && b.abs() < 9.0e15 =>
                {
                    let (a, b) = (a as i64, b as i64);
                    Value::Number(match operator {
                        And => a & b,
                        Or => a | b,
                        _ => a ^ b,
                    } as f64)
                }
                _ => return None,
            };
            push(stack, value)
        }
        Not => match stack.pop()? {
            Value::Bool(a) => push(stack, Value::Bool(!a)),
            Value::Number(a) if a.fract() == 0.0 && a.abs() < 9.0e15 => {
                push(stack, Value::Number(!(a as i64) as f64))
            }
            Value::Number(_) => None,
        },
        Bitshift => {
            let shift = integer(stack)?;
            let value = integer(stack)?;
            // Bits shifted out are lost; 64 bits or more leave nothing.
            let shifted = match u32::try_from(shift.unsigned_abs()) {
                Ok(bits) if bits < 64 => {
                    if shift >= 0 {
                        value.wrapping_shl(bits)
                    } else {
                        value >> bits
                    }
                }
                _ => 0,
            };
            push(stack, Value::Number(shifted as f64))
        }
        Dup => {
            let top = *stack.last()?;
            push(stack, top)
        }
        Pop => stack.pop().map(|_| ()),
        Exch => {
            let b = stack.pop()?;
            let a = stack.pop()?;
            stack.push(b);
            stack.push(a);
            Some(())
        }
        Copy => {
            let n = usize::try_from(integer(stack)?).ok()?;
            let start = stack.len().checked_sub(n)?;
            if stack.len() + n > MAX_STACK {
                return None;
            }
            let copied: Vec<Value> = stack.get(start..)?.to_vec();
            stack.extend(copied);
            Some(())
        }
        Index => {
            let n = usize::try_from(integer(stack)?).ok()?;
            let value = *stack.get(stack.len().checked_sub(n + 1)?)?;
            push(stack, value)
        }
        Roll => {
            let j = integer(stack)?;
            let n = usize::try_from(integer(stack)?).ok()?;
            let start = stack.len().checked_sub(n)?;
            if n > 0 {
                let shift = usize::try_from(j.rem_euclid(n as i64)).ok()?;
                stack.get_mut(start..)?.rotate_right(shift);
            }
            Some(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calculator(source: &str, inputs: usize, outputs: usize) -> Option<Function> {
        let tokens = tokenize(source.as_bytes());
        let mut at = 1;
        let mut count = 0;
        let program = block(&tokens, &mut at, &mut count, 0)?;
        Some(Function {
            domain: vec![(0.0, 1.0); inputs],
            range: Some(vec![(-10.0, 10.0); outputs]),
            outputs,
            kind: Kind::Calculator(program),
            cost: 0,
        })
    }

    #[test]
    fn calculator_programs_run() {
        // A Separation to CMYK tint: c = 0, m = t, y = t, k = 0.
        let f = calculator("{ 0 exch dup 0 }", 1, 4).expect("program");
        assert_eq!(f.eval(&[0.5]), Some(vec![0.0, 0.5, 0.5, 0.0]));
        // `roll` and `index`: t 0 0 t.
        let f = calculator("{ dup 0 exch dup 0 4 1 roll pop }", 1, 4).expect("program");
        assert_eq!(f.eval(&[0.5]), Some(vec![0.5, 0.0, 0.0, 0.5]));
        let f = calculator("{ 0 1 index }", 1, 3).expect("program");
        assert_eq!(f.eval(&[0.5]), Some(vec![0.5, 0.0, 0.5]));
        let f = calculator("{ dup 0.5 gt { 1 sub } { 2 mul } ifelse }", 1, 1).expect("program");
        assert_eq!(f.eval(&[0.75]), Some(vec![-0.25]));
        assert_eq!(f.eval(&[0.25]), Some(vec![0.5]));
        // Clipped to the domain, then the range.
        assert_eq!(f.eval(&[3.0]), Some(vec![0.0]));
        // sin 90 = 1, 2^2 = 4, 7 idiv 2 = 3, 7 mod 2 = 1.
        let f = calculator("{ pop 90 sin 2 2 exp add 7 2 idiv add 7 2 mod add }", 1, 1)
            .expect("program");
        assert_eq!(f.eval(&[0.0]), Some(vec![9.0]));
        let f = calculator("{ 3 1 bitshift 2 copy pop pop }", 1, 2).expect("program");
        assert_eq!(f.eval(&[0.0]), Some(vec![0.0, 6.0]));
    }

    #[test]
    fn calculator_errors_are_no_value() {
        // Unknown operator, unbalanced braces, a loop operator, a wrong
        // result count, division by zero, stack underflow and overflow.
        assert!(calculator("{ 1 foo }", 1, 1).is_none());
        assert!(calculator("{ 1 { 2 }", 1, 1).is_none());
        assert!(calculator("{ 0 1 1 { } for }", 1, 1).is_none());
        let f = calculator("{ 1 }", 1, 1).expect("program");
        assert_eq!(f.eval(&[0.0]), None);
        let f = calculator("{ 0 div }", 1, 1).expect("program");
        assert_eq!(f.eval(&[0.5]), None);
        let f = calculator("{ pop pop }", 1, 1).expect("program");
        assert_eq!(f.eval(&[0.5]), None);
        let f = calculator(&format!("{{ {} }}", "dup ".repeat(200)), 1, 1).expect("program");
        assert_eq!(f.eval(&[0.5]), None);
        // A boolean after a pending block, `if` on a number, atan 0 0.
        assert!(calculator("{ { 1 } true if }", 1, 1).is_none());
        let f = calculator("{ { 2 } if }", 1, 1).expect("program");
        assert_eq!(f.eval(&[0.5]), None);
        let f = calculator("{ pop 0 0 atan }", 1, 1).expect("program");
        assert_eq!(f.eval(&[0.5]), None);
        // Bits shifted out are lost; 64 or more leave 0.
        let f = calculator("{ pop 1 64 bitshift 8 -2 bitshift }", 1, 2).expect("program");
        assert_eq!(f.eval(&[0.5]), Some(vec![0.0, 2.0]));
        // A program longer than the bound is refused when read.
        assert!(calculator(&format!("{{ {} }}", "1 pop ".repeat(MAX_PROGRAM)), 1, 1).is_none());
    }

    #[test]
    fn sampled_functions_interpolate() {
        // Two inputs, one output, a 2 x 2 table: 0, 1 / 1, 0 (8 bits).
        let function = Function {
            domain: vec![(0.0, 1.0); 2],
            range: Some(vec![(0.0, 1.0)]),
            outputs: 1,
            kind: Kind::Sampled {
                size: vec![2, 2],
                encode: vec![(0.0, 1.0); 2],
                decode: vec![(0.0, 1.0)],
                samples: vec![0.0, 1.0, 1.0, 0.0],
            },
            cost: 0,
        };
        assert_eq!(function.eval(&[0.0, 0.0]), Some(vec![0.0]));
        assert_eq!(function.eval(&[1.0, 0.0]), Some(vec![1.0]));
        assert_eq!(function.eval(&[0.5, 0.5]), Some(vec![0.5]));
        assert_eq!(function.eval(&[0.5]), None);
        let mut reader = BitReader::new(&[0b1010_0000, 0xFF]);
        assert_eq!(reader.read(4), Some(0b1010));
        assert_eq!(reader.read(12), Some(0x0FF));
        assert_eq!(reader.read(1), None);
    }

    #[test]
    fn exponential_and_stitching_functions() {
        let exponential = |c0: f64, c1: f64, n: f64| Function {
            domain: vec![(0.0, 1.0)],
            range: None,
            outputs: 1,
            kind: Kind::Exponential {
                c0: vec![c0],
                c1: vec![c1],
                n,
            },
            cost: 0,
        };
        assert_eq!(exponential(0.0, 1.0, 2.0).eval(&[0.5]), Some(vec![0.25]));
        // Two halves: rising then falling, each over the encoded 0..1.
        let stitching = Function {
            domain: vec![(0.0, 1.0)],
            range: None,
            outputs: 1,
            kind: Kind::Stitching {
                functions: vec![exponential(0.0, 1.0, 1.0), exponential(1.0, 0.0, 1.0)],
                bounds: vec![0.5],
                encode: vec![(0.0, 1.0), (0.0, 1.0)],
            },
            cost: 0,
        };
        assert_eq!(stitching.eval(&[0.25]), Some(vec![0.5]));
        assert_eq!(stitching.eval(&[0.5]), Some(vec![1.0]));
        assert_eq!(stitching.eval(&[0.75]), Some(vec![0.5]));
        assert_eq!(stitching.eval(&[1.0]), Some(vec![0.0]));
    }
}
