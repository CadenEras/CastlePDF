//! Stream filters (ISO 32000-2:2020, 7.4): `FlateDecode` and `LZWDecode`
//! with their predictors, `ASCIIHexDecode`, `ASCII85Decode` and
//! `RunLengthDecode`.
//!
//! Every filter and parameter is validated before any byte is decoded.
//! Decoding is incremental: inflate writes into a fixed 64 KiB window, and each
//! window is charged to `max_decoded_bytes` before it is kept, so a
//! decompression bomb stops at the limit. The output buffer never grows beyond
//! what the budget still allows; a reallocation briefly holds the old and new
//! buffers, so the transient peak is about 1.5 × the decoded size. Predictors
//! are undone in place, without a second buffer or a second charge. In a chain
//! of filters, each stage's output is charged, since both buffers are alive.
//! The other decoders write through `emit`, which charges each piece before
//! it is kept: RunLength and LZW expand their input (a 2-byte run is 128
//! bytes), so their output is bounded by the budget, not by the input.
//! Other filters (CCITT, JBIG2, DCT, JPX, Crypt) are reported as unsupported,
//! never passed through undecoded.

use std::io;

use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};

use crate::limits::{Budget, Capability, Error, MalformedKind};
use crate::object::{Dictionary, Object};

/// Size of the inflate output window; also the cancellation granularity.
const WINDOW: usize = 64 * 1024;
/// Cancellation is checked every this many predicted rows.
const CANCEL_CHECK_ROWS: usize = 1024;
/// Cancellation is checked every this many pieces written by `emit`.
const CANCEL_CHECK_PIECES: usize = 4096;

/// One filter of a stream's chain, its parameters validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Flate(Option<Predictor>),
    Lzw {
        /// `/EarlyChange`: the code length grows one code early (default).
        early_change: bool,
        predictor: Option<Predictor>,
    },
    AsciiHex,
    Ascii85,
    RunLength,
}

/// Decodes the data of a stream whose `dictionary` is given, applying every
/// filter of `/Filter` in order with its `/DecodeParms`.
///
/// `/Filter` and `/DecodeParms` must be direct objects here; `PdfFile`
/// resolves references first. `data_offset` (where the encoded data starts in
/// the file) locates errors. Every decoded byte is charged to `budget`; bytes
/// after the end of a Flate stream are ignored.
pub fn decode_stream(
    dictionary: &Dictionary,
    encoded: &[u8],
    data_offset: u64,
    budget: &mut Budget,
) -> Result<Vec<u8>, Error> {
    let stages = plan(dictionary, data_offset)?;
    if stages.is_empty() {
        return copy(encoded, budget);
    }
    let mut data: Option<Vec<u8>> = None;
    for stage in stages {
        let input = data.as_deref().unwrap_or(encoded);
        let (mut decoded, predictor) = match stage {
            Stage::Flate(predictor) => (inflate_zlib(input, data_offset, budget)?, predictor),
            Stage::Lzw {
                early_change,
                predictor,
            } => (lzw(input, early_change, data_offset, budget)?, predictor),
            Stage::AsciiHex => (ascii_hex(input, data_offset, budget)?, None),
            Stage::Ascii85 => (ascii85(input, data_offset, budget)?, None),
            Stage::RunLength => (run_length(input, data_offset, budget)?, None),
        };
        if let Some(predictor) = predictor {
            predictor.apply_in_place(&mut decoded, data_offset, budget)?;
        }
        data = Some(decoded);
    }
    data.ok_or(malformed(data_offset, MalformedKind::InvalidFilter))
}

/// Validates `/Filter` and `/DecodeParms` before decoding: one stage per
/// filter, with its parameters.
fn plan(dictionary: &Dictionary, offset: u64) -> Result<Vec<Stage>, Error> {
    let invalid = || malformed(offset, MalformedKind::InvalidFilter);
    let filters: Vec<&[u8]> = match dictionary.get(b"Filter") {
        None | Some(Object::Null) => Vec::new(),
        Some(Object::Name(name)) => vec![name.as_slice()],
        Some(Object::Array(items)) => items
            .iter()
            .map(|item| item.as_name().ok_or_else(invalid))
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(invalid()),
    };
    // Without a filter, /DecodeParms has nothing to parameterise.
    if filters.is_empty() {
        return Ok(Vec::new());
    }
    let params: Vec<Option<&Dictionary>> = match dictionary.get(b"DecodeParms") {
        None | Some(Object::Null) => vec![None; filters.len()],
        Some(Object::Dictionary(params)) if filters.len() == 1 => vec![Some(params)],
        Some(Object::Array(items)) if items.len() == filters.len() => items
            .iter()
            .map(|item| match item {
                Object::Null => Ok(None),
                Object::Dictionary(params) => Ok(Some(params)),
                _ => Err(invalid()),
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(invalid()),
    };
    filters
        .into_iter()
        .zip(params)
        .map(|(filter, params)| match filter {
            b"FlateDecode" => Predictor::from_params(params, offset).map(Stage::Flate),
            b"LZWDecode" => {
                let early_change = match params.and_then(|p| p.get(b"EarlyChange")) {
                    None | Some(Object::Null) | Some(Object::Integer(1)) => true,
                    Some(Object::Integer(0)) => false,
                    Some(_) => return Err(invalid()),
                };
                Ok(Stage::Lzw {
                    early_change,
                    predictor: Predictor::from_params(params, offset)?,
                })
            }
            b"ASCIIHexDecode" => Ok(Stage::AsciiHex),
            b"ASCII85Decode" => Ok(Stage::Ascii85),
            b"RunLengthDecode" => Ok(Stage::RunLength),
            _ => Err(Error::Unsupported(Capability::StreamFilter)),
        })
        .collect()
}

/// Output of the decoders other than Flate: each piece is charged to the
/// budget before it is kept, and cancellation is checked as pieces come.
struct Emitter<'b> {
    out: Vec<u8>,
    budget: &'b mut Budget,
    pieces: usize,
}

impl<'b> Emitter<'b> {
    fn new(budget: &'b mut Budget) -> Self {
        Self {
            out: Vec::new(),
            budget,
            pieces: 0,
        }
    }

    fn emit(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.pieces += 1;
        if self.pieces.is_multiple_of(CANCEL_CHECK_PIECES) {
            self.budget.check_cancelled()?;
        }
        self.budget.charge_decoded_bytes(bytes.len() as u64)?;
        reserve_within_budget(&mut self.out, bytes.len(), self.budget)?;
        self.out.extend_from_slice(bytes);
        Ok(())
    }

    /// Writes `count` copies of `byte` (a RunLength run, at most 128).
    fn repeat(&mut self, byte: u8, count: usize) -> Result<(), Error> {
        let run = [byte; 128];
        self.emit(run.get(..count.min(128)).unwrap_or_default())
    }
}

/// `ASCIIHexDecode` (7.4.2): pairs of hexadecimal digits, white space
/// ignored, ending at `>` (or at the end of the data). A last single digit
/// is followed by an implied 0.
fn ascii_hex(input: &[u8], offset: u64, budget: &mut Budget) -> Result<Vec<u8>, Error> {
    let bad = || malformed(offset, MalformedKind::InvalidFilterData);
    let mut out = Emitter::new(budget);
    let mut high: Option<u8> = None;
    for &byte in input {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            b'>' => break,
            _ if crate::lexer::is_whitespace(byte) => continue,
            _ => return Err(bad()),
        };
        match high.take() {
            Some(first) => out.emit(&[(first << 4) | digit])?,
            None => high = Some(digit),
        }
    }
    if let Some(first) = high {
        out.emit(&[first << 4])?;
    }
    Ok(out.out)
}

/// `ASCII85Decode` (7.4.3): groups of five characters `!` to `u` for four
/// bytes, `z` for four zero bytes, white space ignored, ending at `~>` (or
/// at the end of the data). A last group of n characters (2 to 4) gives
/// n - 1 bytes; a group worth more than 2^32 - 1, a `z` inside a group or a
/// last group of one character is invalid.
fn ascii85(input: &[u8], offset: u64, budget: &mut Budget) -> Result<Vec<u8>, Error> {
    let bad = || malformed(offset, MalformedKind::InvalidFilterData);
    let mut out = Emitter::new(budget);
    let mut group = [0_u8; 5];
    let mut count = 0;
    let mut bytes = input.iter().copied();
    while let Some(byte) = bytes.next() {
        match byte {
            b'!'..=b'u' => {
                if let Some(slot) = group.get_mut(count) {
                    *slot = byte - b'!';
                }
                count += 1;
                if count == 5 {
                    out.emit(&group_value(&group, bad)?.to_be_bytes())?;
                    count = 0;
                }
            }
            b'z' if count == 0 => out.emit(&[0; 4])?,
            b'~' => {
                // `~` is only the start of `~>`.
                for next in bytes.by_ref() {
                    if crate::lexer::is_whitespace(next) {
                        continue;
                    }
                    if next != b'>' {
                        return Err(bad());
                    }
                    break;
                }
                break;
            }
            _ if crate::lexer::is_whitespace(byte) => {}
            _ => return Err(bad()),
        }
    }
    match count {
        0 => {}
        1 => return Err(bad()),
        _ => {
            for slot in group.iter_mut().skip(count) {
                *slot = b'u' - b'!';
            }
            let value = group_value(&group, bad)?.to_be_bytes();
            out.emit(value.get(..count - 1).ok_or_else(bad)?)?;
        }
    }
    Ok(out.out)
}

/// The value of five base-85 digits, if it fits 32 bits.
fn group_value(group: &[u8; 5], bad: impl Fn() -> Error) -> Result<u32, Error> {
    let value = group
        .iter()
        .fold(0_u64, |value, digit| value * 85 + u64::from(*digit));
    u32::try_from(value).map_err(|_| bad())
}

/// `RunLengthDecode` (7.4.5): a length byte L, then L + 1 bytes to copy (L
/// below 128), or one byte to repeat 257 - L times (L above 128); 128 ends
/// the data, as does its end. A copy cut short by the end is kept as far as
/// it goes.
fn run_length(input: &[u8], offset: u64, budget: &mut Budget) -> Result<Vec<u8>, Error> {
    let bad = || malformed(offset, MalformedKind::InvalidFilterData);
    let mut out = Emitter::new(budget);
    let mut at = 0;
    while let Some(&length) = input.get(at) {
        at += 1;
        match length {
            0..=127 => {
                let end = at + usize::from(length) + 1;
                let run = input.get(at..end.min(input.len())).ok_or_else(bad)?;
                out.emit(run)?;
                at = end;
            }
            128 => break,
            _ => {
                let Some(&byte) = input.get(at) else {
                    break;
                };
                out.repeat(byte, 257 - usize::from(length))?;
                at += 1;
            }
        }
    }
    Ok(out.out)
}

/// Most entries of an LZW table: 4096 codes of at most 12 bits.
const LZW_TABLE: usize = 4096;

/// An LZW string: the entry of its prefix (none for a single byte), its
/// last byte, and its first byte.
#[derive(Debug, Clone, Copy)]
struct LzwEntry {
    prefix: Option<usize>,
    last: u8,
    first: u8,
}
const LZW_CLEAR: u16 = 256;
const LZW_END: u16 = 257;

/// `LZWDecode` (7.4.4): codes of 9 to 12 bits, most significant bit first;
/// 256 resets the table, 257 ends the data (as does its end). With
/// `early_change`, the code length grows one code before the table needs
/// it. A full table is kept until the next 256. Each entry is stored as its
/// prefix code and last byte, so the table stays at 4096 small entries.
fn lzw(
    input: &[u8],
    early_change: bool,
    offset: u64,
    budget: &mut Budget,
) -> Result<Vec<u8>, Error> {
    let bad = || malformed(offset, MalformedKind::InvalidFilterData);
    let mut out = Emitter::new(budget);
    // Codes 0 to 255 are single bytes; 256 and 257 have no string.
    let mut table: Vec<LzwEntry> = Vec::with_capacity(LZW_TABLE);
    table.extend((0..=255_u8).map(|byte| LzwEntry {
        prefix: None,
        last: byte,
        first: byte,
    }));
    let reset = table.len();
    let mut width: u32 = 9;
    let mut previous: Option<usize> = None;
    let mut string = Vec::with_capacity(LZW_TABLE);
    let (mut buffer, mut bits): (u32, u32) = (0, 0);
    let mut bytes = input.iter();
    loop {
        while bits < width {
            let Some(&byte) = bytes.next() else {
                return Ok(out.out);
            };
            buffer = (buffer << 8) | u32::from(byte);
            bits += 8;
        }
        let code = ((buffer >> (bits - width)) & ((1 << width) - 1)) as u16;
        bits -= width;
        buffer &= (1 << bits) - 1;
        match code {
            LZW_CLEAR => {
                table.truncate(reset);
                width = 9;
                previous = None;
                continue;
            }
            LZW_END => return Ok(out.out),
            _ => {}
        }
        // Codes 258 and up sit 2 entries lower (256 and 257 are not stored).
        let index = match usize::from(code) {
            code @ 0..=255 => code,
            code => code - 2,
        };
        // A known code, or the one being defined now (the KwKwK case: the
        // previous string plus its own first byte).
        let first_byte = match (table.get(index), previous) {
            (Some(entry), _) => entry.first,
            (None, Some(p)) if index == table.len() => table.get(p).ok_or_else(bad)?.first,
            _ => return Err(bad()),
        };
        if let Some(p) = previous
            && table.len() + 2 < LZW_TABLE
        {
            let first = table.get(p).ok_or_else(bad)?.first;
            table.push(LzwEntry {
                prefix: Some(p),
                last: first_byte,
                first,
            });
        }
        // The string of `code`, from its last byte back to its first.
        string.clear();
        let mut at = Some(index);
        while let Some(current) = at {
            let entry = table.get(current).ok_or_else(bad)?;
            string.push(entry.last);
            at = entry.prefix;
            if string.len() > LZW_TABLE {
                return Err(bad());
            }
        }
        string.reverse();
        out.emit(&string)?;
        previous = Some(index);
        // The next code to be defined, as the encoder counts it.
        let next = table.len() + 2;
        let limit = if early_change { next + 1 } else { next };
        width = match limit {
            0..=511 => 9,
            512..=1023 => 10,
            1024..=2047 => 11,
            _ => 12,
        };
    }
}

fn malformed(offset: u64, kind: MalformedKind) -> Error {
    Error::Malformed { offset, kind }
}

fn out_of_memory() -> Error {
    Error::Io(io::ErrorKind::OutOfMemory)
}

/// Charges, then copies, unfiltered data.
fn copy(data: &[u8], budget: &mut Budget) -> Result<Vec<u8>, Error> {
    budget.charge_decoded_bytes(data.len() as u64)?;
    let mut out = Vec::new();
    out.try_reserve_exact(data.len())
        .map_err(|_| out_of_memory())?;
    out.extend_from_slice(data);
    Ok(out)
}

/// Inflates zlib data (RFC 1950, checksum verified) under the budget.
fn inflate_zlib(input: &[u8], offset: u64, budget: &mut Budget) -> Result<Vec<u8>, Error> {
    let bad = || malformed(offset, MalformedKind::InvalidFlateData);
    let mut state = InflateState::new_boxed(DataFormat::Zlib);
    let mut window = vec![0_u8; WINDOW];
    let mut out: Vec<u8> = Vec::new();
    let mut rest = input;
    loop {
        budget.check_cancelled()?;
        let result = inflate(&mut state, rest, &mut window, MZFlush::None);
        rest = rest.get(result.bytes_consumed..).unwrap_or_default();
        let written = window.get(..result.bytes_written).ok_or_else(bad)?;
        // Charged before it is kept: the limit stops a decompression bomb.
        budget.charge_decoded_bytes(written.len() as u64)?;
        reserve_within_budget(&mut out, written.len(), budget)?;
        out.extend_from_slice(written);
        match result.status {
            Ok(MZStatus::StreamEnd) => return Ok(out),
            Ok(MZStatus::Ok) if result.bytes_consumed > 0 || result.bytes_written > 0 => {}
            // No progress: the input ended before the end of the stream.
            Ok(MZStatus::Ok) | Err(MZError::Buf) if rest.is_empty() => {
                return Err(Error::Truncated { offset });
            }
            _ => return Err(bad()),
        }
    }
}

/// Makes room for `additional` bytes already charged: amortised doubling, but
/// never beyond the length the remaining budget still allows.
fn reserve_within_budget(
    out: &mut Vec<u8>,
    additional: usize,
    budget: &Budget,
) -> Result<(), Error> {
    let needed = out.len().saturating_add(additional);
    if out.capacity() >= needed {
        return Ok(());
    }
    let remaining = budget
        .limits()
        .max_decoded_bytes
        .saturating_sub(budget.decoded_bytes());
    let ceiling = needed.saturating_add(usize::try_from(remaining).unwrap_or(usize::MAX));
    let target = out.capacity().saturating_mul(2).max(needed).min(ceiling);
    out.try_reserve_exact(target - out.len())
        .map_err(|_| out_of_memory())
}

/// `/Predictor` and its parameters (ISO 32000-2, table 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Predictor {
    /// 2 for TIFF, 10 to 15 for PNG (the actual filter is chosen per row).
    kind: u8,
    /// Bytes per row of decoded samples.
    row_len: usize,
    /// Bytes per complete pixel, at least 1.
    pixel_len: usize,
}

impl Predictor {
    fn from_params(params: Option<&Dictionary>, offset: u64) -> Result<Option<Self>, Error> {
        let invalid = || malformed(offset, MalformedKind::InvalidPredictor);
        let Some(params) = params else {
            return Ok(None);
        };
        let integer = |key: &[u8], default: i64| match params.get(key) {
            None | Some(Object::Null) => Ok(default),
            Some(Object::Integer(value)) => Ok(*value),
            Some(_) => Err(invalid()),
        };
        let kind = match integer(b"Predictor", 1)? {
            1 => return Ok(None),
            kind @ (2 | 10..=15) => kind as u8,
            _ => return Err(invalid()),
        };
        let colors = integer(b"Colors", 1)?;
        let bits = integer(b"BitsPerComponent", 8)?;
        let columns = integer(b"Columns", 1)?;
        let bits_per_component: u64 = match bits {
            1 | 2 | 4 | 8 | 16 => bits as u64,
            _ => return Err(invalid()),
        };
        let colors = u64::try_from(colors)
            .ok()
            .filter(|c| *c >= 1)
            .ok_or_else(invalid)?;
        let columns = u64::try_from(columns)
            .ok()
            .filter(|c| *c >= 1)
            .ok_or_else(invalid)?;
        let pixel_bits = colors.checked_mul(bits_per_component).ok_or_else(invalid)?;
        let row_bits = pixel_bits.checked_mul(columns).ok_or_else(invalid)?;
        let row_len = usize::try_from(row_bits.div_ceil(8)).map_err(|_| invalid())?;
        let pixel_len = usize::try_from(pixel_bits.div_ceil(8))
            .map_err(|_| invalid())?
            .max(1);
        // TIFF prediction is implemented for 8-bit components only.
        if kind == 2 && bits_per_component != 8 {
            return Err(Error::Unsupported(Capability::StreamFilter));
        }
        Ok(Some(Self {
            kind,
            row_len,
            pixel_len,
        }))
    }

    /// Undoes the prediction in `data`, in place. PNG rows shrink by their
    /// filter-type byte: row `r` is written at `r * row_len`, always behind the
    /// bytes still to read (at `r * (row_len + 1) + 1 + k`), and the previous
    /// decoded row is still intact when the current one reads it.
    fn apply_in_place(
        &self,
        data: &mut Vec<u8>,
        offset: u64,
        budget: &mut Budget,
    ) -> Result<(), Error> {
        let invalid = || malformed(offset, MalformedKind::InvalidPredictor);
        let png = self.kind >= 10;
        let stride = if png {
            self.row_len.checked_add(1).ok_or_else(invalid)?
        } else {
            self.row_len
        };
        if stride == 0 || !data.len().is_multiple_of(stride) {
            return Err(invalid());
        }
        let rows = data.len() / stride;
        let (row_len, pixel_len) = (self.row_len, self.pixel_len);
        let at = |data: &Vec<u8>, i: usize| data.get(i).copied().ok_or_else(invalid);
        for row in 0..rows {
            if row % CANCEL_CHECK_ROWS == 0 {
                budget.check_cancelled()?;
            }
            let out_start = row * row_len;
            let filter = if png { at(data, row * stride)? } else { 0 };
            for k in 0..row_len {
                let w = out_start + k;
                let x = if png {
                    at(data, row * stride + 1 + k)?
                } else {
                    at(data, w)?
                };
                let left = if k >= pixel_len {
                    at(data, w - pixel_len)?
                } else {
                    0
                };
                let predicted = if !png {
                    left
                } else {
                    let up = if row > 0 { at(data, w - row_len)? } else { 0 };
                    let up_left = if row > 0 && k >= pixel_len {
                        at(data, w - row_len - pixel_len)?
                    } else {
                        0
                    };
                    match filter {
                        0 => 0,
                        1 => left,
                        2 => up,
                        3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                        4 => paeth(left, up, up_left),
                        _ => return Err(invalid()),
                    }
                };
                *data.get_mut(w).ok_or_else(invalid)? = x.wrapping_add(predicted);
            }
        }
        data.truncate(rows * row_len);
        Ok(())
    }
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = (
        (p - i16::from(a)).abs(),
        (p - i16::from(b)).abs(),
        (p - i16::from(c)).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[cfg(test)]
mod tests;
