//! `CCITTFaxDecode` (ISO 32000-2, 7.4.6): ITU-T T.4 (Group 3, one- and
//! two-dimensional) and T.6 (Group 4) bilevel image data.
//!
//! Each row is a list of changing elements, the columns where the colour
//! changes; it is decoded either from run lengths (1D, modified Huffman) or
//! from the row above (2D: pass, horizontal and vertical modes), then
//! written out, one bit per pixel, most significant first, each row padded
//! to a byte. White is 1 unless `/BlackIs1` is true (the decoded data then
//! read as `DeviceGray`, where 0 is black).
//!
//! Every step reads at least one bit of input, so the work is bounded by
//! the input; every row is charged to the budget before it is kept (rows of
//! a white page cost one bit each). Two changing elements at the same
//! column (a run of length zero) cancel out, as in Poppler and pdf.js, so a
//! row holds at most `columns` + 1 of them however long its code.
//! Data that ends early (an end of block, or no more data) leaves the rows
//! left to `/Rows` white. Damaged data is an error, never a guessed image;
//! `/DamagedRowsBeforeError` is not honoured. Extension codes (uncompressed
//! mode) are unsupported.

use std::sync::OnceLock;

use super::{Emitter, malformed};
use crate::limits::{Budget, Capability, Error, MalformedKind};
use crate::object::{Dictionary, Object};

/// Widest row accepted, in pixels (`/Columns`): 2^17, far beyond a fax or
/// a 1200 dpi A0 scan.
pub(super) const MAX_COLUMNS: u32 = 1 << 17;

/// The parameters of `/DecodeParms` (7.4.6, table 11), validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Params {
    /// Negative: Group 4; zero: Group 3 one-dimensional; positive: Group 3
    /// mixing one- and two-dimensional rows, each announced by a tag bit.
    k: i64,
    columns: u32,
    rows: Option<u32>,
    end_of_line: bool,
    byte_align: bool,
    end_of_block: bool,
    black_is_1: bool,
}

impl Params {
    pub(super) fn from(params: Option<&Dictionary>, offset: u64) -> Result<Self, Error> {
        let invalid = || malformed(offset, MalformedKind::InvalidFilter);
        let get = |key: &[u8]| {
            params
                .and_then(|p| p.get(key))
                .filter(|v| **v != Object::Null)
        };
        let flag = |key: &[u8], default: bool| match get(key) {
            None => Ok(default),
            Some(Object::Boolean(value)) => Ok(*value),
            Some(_) => Err(invalid()),
        };
        let integer = |key: &[u8]| match get(key) {
            None => Ok(None),
            Some(Object::Integer(value)) => Ok(Some(*value)),
            Some(_) => Err(invalid()),
        };
        let columns = match integer(b"Columns")? {
            None => 1728,
            Some(value) => u32::try_from(value)
                .ok()
                .filter(|columns| (1..=MAX_COLUMNS).contains(columns))
                .ok_or_else(invalid)?,
        };
        let rows = match integer(b"Rows")? {
            None | Some(0) => None,
            Some(value) => Some(u32::try_from(value).map_err(|_| invalid())?),
        };
        Ok(Self {
            k: integer(b"K")?.unwrap_or(0),
            columns,
            rows,
            end_of_line: flag(b"EndOfLine", false)?,
            byte_align: flag(b"EncodedByteAlign", false)?,
            end_of_block: flag(b"EndOfBlock", true)?,
            black_is_1: flag(b"BlackIs1", false)?,
        })
    }
}

/// Reads bits, most significant first; past the end, bits are 0.
struct Bits<'a> {
    data: &'a [u8],
    /// Position in bits.
    at: usize,
}

impl Bits<'_> {
    /// The next `count` bits (at most 24) as a number.
    fn peek(&self, count: u32) -> u32 {
        let byte = self.at / 8;
        let word = (0..4).fold(0_u32, |word, i| {
            (word << 8) | u32::from(self.data.get(byte + i).copied().unwrap_or(0))
        });
        let shift = (self.at % 8) as u32;
        (word << shift) >> (32 - count)
    }

    fn skip(&mut self, count: u32) {
        self.at = self.at.saturating_add(count as usize);
    }

    fn bit(&mut self) -> u32 {
        let bit = self.peek(1);
        self.skip(1);
        bit
    }

    fn align(&mut self) {
        self.at = self.at.div_ceil(8) * 8;
    }

    /// No 1 bit left: the end of the data, or its padding.
    fn exhausted(&self) -> bool {
        let byte = self.at / 8;
        let Some(first) = self.data.get(byte) else {
            return true;
        };
        let shift = self.at % 8;
        (first << shift) == 0
            && self
                .data
                .get(byte + 1..)
                .is_none_or(|rest| rest.iter().all(|b| *b == 0))
    }

    /// Consumes an end-of-line code (eleven 0 bits or more, then a 1: fill
    /// bits before it are allowed), if one comes next.
    fn end_of_line(&mut self) -> bool {
        let start = self.at;
        let mut zeros = 0_u32;
        while self.peek(1) == 0 {
            if self.at / 8 >= self.data.len() {
                self.at = start;
                return false;
            }
            self.skip(1);
            zeros += 1;
        }
        if zeros >= 11 {
            self.skip(1);
            true
        } else {
            self.at = start;
            false
        }
    }
}

/// A run-length code: its run, and whether it is a make-up code (another
/// code follows for the same run).
#[derive(Clone, Copy)]
struct Run {
    run: u16,
    len: u8,
}

/// Codes up to 13 bits are looked up by their next 13 bits.
const PEEK: u32 = 13;

/// Terminating codes 0 to 63, then make-up codes 64 to 1728, of the white
/// runs (T.4, tables 2 and 3): the code as a string of bits.
const WHITE: [&str; 91] = [
    "00110101",
    "000111",
    "0111",
    "1000",
    "1011",
    "1100",
    "1110",
    "1111",
    "10011",
    "10100",
    "00111",
    "01000",
    "001000",
    "000011",
    "110100",
    "110101",
    "101010",
    "101011",
    "0100111",
    "0001100",
    "0001000",
    "0010111",
    "0000011",
    "0000100",
    "0101000",
    "0101011",
    "0010011",
    "0100100",
    "0011000",
    "00000010",
    "00000011",
    "00011010",
    "00011011",
    "00010010",
    "00010011",
    "00010100",
    "00010101",
    "00010110",
    "00010111",
    "00101000",
    "00101001",
    "00101010",
    "00101011",
    "00101100",
    "00101101",
    "00000100",
    "00000101",
    "00001010",
    "00001011",
    "01010010",
    "01010011",
    "01010100",
    "01010101",
    "00100100",
    "00100101",
    "01011000",
    "01011001",
    "01011010",
    "01011011",
    "01001010",
    "01001011",
    "00110010",
    "00110011",
    "00110100", // 64 to 1728 by 64:
    "11011",
    "10010",
    "010111",
    "0110111",
    "00110110",
    "00110111",
    "01100100",
    "01100101",
    "01101000",
    "01100111",
    "011001100",
    "011001101",
    "011010010",
    "011010011",
    "011010100",
    "011010101",
    "011010110",
    "011010111",
    "011011000",
    "011011001",
    "011011010",
    "011011011",
    "010011000",
    "010011001",
    "010011010",
    "011000",
    "010011011",
];

/// The same for black runs.
const BLACK: [&str; 91] = [
    "0000110111",
    "010",
    "11",
    "10",
    "011",
    "0011",
    "0010",
    "00011",
    "000101",
    "000100",
    "0000100",
    "0000101",
    "0000111",
    "00000100",
    "00000111",
    "000011000",
    "0000010111",
    "0000011000",
    "0000001000",
    "00001100111",
    "00001101000",
    "00001101100",
    "00000110111",
    "00000101000",
    "00000010111",
    "00000011000",
    "000011001010",
    "000011001011",
    "000011001100",
    "000011001101",
    "000001101000",
    "000001101001",
    "000001101010",
    "000001101011",
    "000011010010",
    "000011010011",
    "000011010100",
    "000011010101",
    "000011010110",
    "000011010111",
    "000001101100",
    "000001101101",
    "000011011010",
    "000011011011",
    "000001010100",
    "000001010101",
    "000001010110",
    "000001010111",
    "000001100100",
    "000001100101",
    "000001010010",
    "000001010011",
    "000000100100",
    "000000110111",
    "000000111000",
    "000000100111",
    "000000101000",
    "000001011000",
    "000001011001",
    "000000101011",
    "000000101100",
    "000001011010",
    "000001100110",
    "000001100111", // 64 to 1728 by 64:
    "0000001111",
    "000011001000",
    "000011001001",
    "000001011011",
    "000000110011",
    "000000110100",
    "000000110101",
    "0000001101100",
    "0000001101101",
    "0000001001010",
    "0000001001011",
    "0000001001100",
    "0000001001101",
    "0000001110010",
    "0000001110011",
    "0000001110100",
    "0000001110101",
    "0000001110110",
    "0000001110111",
    "0000001010010",
    "0000001010011",
    "0000001010100",
    "0000001010101",
    "0000001011010",
    "0000001011011",
    "0000001100100",
    "0000001100101",
];

/// Make-up codes 1792 to 2560 by 64, for both colours (T.4, table 4).
const EXTENDED: [&str; 13] = [
    "00000001000",
    "00000001100",
    "00000001101",
    "000000010010",
    "000000010011",
    "000000010100",
    "000000010101",
    "000000010110",
    "000000010111",
    "000000011100",
    "000000011101",
    "000000011110",
    "000000011111",
];

/// The lookup table of a colour: the next 13 bits to the code they start
/// with (`len` 0 where no code does).
fn table(codes: &[&str; 91]) -> Box<[Run]> {
    let mut table = vec![Run { run: 0, len: 0 }; 1 << PEEK].into_boxed_slice();
    let runs = (0..64_u16).chain((1..=27).map(|i| i * 64));
    let extended = (28..=40_u16).map(|i| i * 64);
    for (code, run) in codes.iter().zip(runs).chain(EXTENDED.iter().zip(extended)) {
        let len = code.len() as u32;
        let value = u32::from_str_radix(code, 2).unwrap_or(0);
        let first = (value << (PEEK - len)) as usize;
        let count = 1_usize << (PEEK - len);
        for slot in table.iter_mut().skip(first).take(count) {
            *slot = Run {
                run,
                len: len as u8,
            };
        }
    }
    table
}

fn tables() -> &'static [Box<[Run]>; 2] {
    static TABLES: OnceLock<[Box<[Run]>; 2]> = OnceLock::new();
    TABLES.get_or_init(|| [table(&WHITE), table(&BLACK)])
}

/// A run of `black` or white pixels: make-up codes, then a terminating code.
fn run(
    bits: &mut Bits<'_>,
    black: bool,
    columns: u32,
    bad: &impl Fn() -> Error,
) -> Result<u32, Error> {
    let [white_codes, black_codes] = tables();
    let table = if black { black_codes } else { white_codes };
    let mut total = 0_u32;
    loop {
        let Some(code) = table
            .get(bits.peek(PEEK) as usize)
            .copied()
            .filter(|code| code.len > 0)
        else {
            // A one-dimensional extension code (000000001xxx: uncompressed
            // mode) is unsupported; anything else is damaged.
            if bits.peek(9) == 1 {
                return Err(Error::Unsupported(Capability::StreamFilter));
            }
            return Err(bad());
        };
        bits.skip(u32::from(code.len));
        total = total.saturating_add(u32::from(code.run));
        if total > columns {
            return Err(bad());
        }
        if code.run < 64 {
            return Ok(total);
        }
    }
}

/// A two-dimensional coding mode (T.4, table 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Pass,
    Horizontal,
    /// a1 is b1 plus this offset.
    Vertical(i64),
}

fn mode(bits: &mut Bits<'_>, bad: &impl Fn() -> Error) -> Result<Mode, Error> {
    // Codes from the shortest: 1, 011, 010, 001, 0001, 000011, 000010,
    // 0000011, 0000010.
    let codes: [(u32, u32, Mode); 9] = [
        (1, 0b1, Mode::Vertical(0)),
        (3, 0b011, Mode::Vertical(1)),
        (3, 0b010, Mode::Vertical(-1)),
        (3, 0b001, Mode::Horizontal),
        (4, 0b0001, Mode::Pass),
        (6, 0b000011, Mode::Vertical(2)),
        (6, 0b000010, Mode::Vertical(-2)),
        (7, 0b0000011, Mode::Vertical(3)),
        (7, 0b0000010, Mode::Vertical(-3)),
    ];
    for (len, value, mode) in codes {
        if bits.peek(len) == value {
            bits.skip(len);
            return Ok(mode);
        }
    }
    // An extension code (0000001xxx: uncompressed mode); otherwise (an end
    // of line inside a row, or no code) the data is damaged.
    if bits.peek(7) == 1 {
        return Err(Error::Unsupported(Capability::StreamFilter));
    }
    Err(bad())
}

/// Adds a changing element at `at`: one at the same column as the last
/// cancels it instead (a run of length zero between them), so that the
/// elements stay strictly increasing and at most `columns` + 1.
fn change(changes: &mut Vec<u32>, at: u32) {
    if changes.last() == Some(&at) {
        changes.pop();
    } else {
        changes.push(at);
    }
}

/// A one-dimensional row: alternate white and black runs from white.
fn row_1d(bits: &mut Bits<'_>, columns: u32, bad: &impl Fn() -> Error) -> Result<Vec<u32>, Error> {
    let mut changes = Vec::new();
    let mut at = 0_u32;
    let mut black = false;
    while at < columns {
        at += run(bits, black, columns - at, bad)?;
        change(&mut changes, at);
        black = !black;
    }
    Ok(changes)
}

/// The first changing element of `reference` after `a0` whose colour is
/// the opposite of `black` (b1), and the next one (b2); `columns` past the
/// last. Element i of a row turns its pixels black when i is even.
fn b1_b2(reference: &[u32], a0: i64, black: bool, columns: u32) -> (i64, i64) {
    let mut i = reference.partition_point(|change| i64::from(*change) <= a0);
    // To white (odd) after black, to black (even) after white.
    if i % 2 == usize::from(!black) {
        i += 1;
    }
    let at = |index: usize| i64::from(reference.get(index).copied().unwrap_or(columns));
    (at(i), at(i + 1))
}

/// A two-dimensional row, coded against `reference`.
fn row_2d(
    bits: &mut Bits<'_>,
    reference: &[u32],
    columns: u32,
    bad: &impl Fn() -> Error,
) -> Result<Vec<u32>, Error> {
    let width = i64::from(columns);
    let mut changes = Vec::new();
    let mut a0 = -1_i64;
    let mut black = false;
    let push = |changes: &mut Vec<u32>, at: i64| -> Result<(), Error> {
        change(changes, u32::try_from(at).map_err(|_| bad())?);
        Ok(())
    };
    while a0 < width {
        let (b1, b2) = b1_b2(reference, a0, black, columns);
        match mode(bits, bad)? {
            Mode::Pass => a0 = b2,
            Mode::Horizontal => {
                let start = a0.max(0);
                let left = u32::try_from(width - start).map_err(|_| bad())?;
                let a1 = start + i64::from(run(bits, black, left, bad)?);
                let left = u32::try_from(width - a1).map_err(|_| bad())?;
                let a2 = a1 + i64::from(run(bits, !black, left, bad)?);
                push(&mut changes, a1)?;
                push(&mut changes, a2)?;
                a0 = a2;
            }
            Mode::Vertical(offset) => {
                let a1 = b1 + offset;
                if a1 < a0.max(0) || a1 > width {
                    return Err(bad());
                }
                push(&mut changes, a1)?;
                a0 = a1;
                black = !black;
            }
        }
    }
    Ok(changes)
}

/// Writes a row of `columns` pixels from its changing elements.
fn paint(row: &mut [u8], changes: &[u32], columns: u32, black_is_1: bool) {
    let (white, black) = if black_is_1 {
        (0x00, 0xFF)
    } else {
        (0xFF, 0x00)
    };
    row.fill(white);
    let mut runs = changes.iter().map(|c| (*c).min(columns));
    let mut start = 0_u32;
    let mut is_black = false;
    loop {
        let end = runs.next().unwrap_or(columns);
        if is_black && end > start {
            for pixel in start..end {
                let mask = 0x80_u8 >> (pixel % 8);
                if let Some(byte) = row.get_mut((pixel / 8) as usize) {
                    *byte = (*byte & !mask) | (black & mask);
                }
            }
        }
        if end >= columns {
            break;
        }
        start = end;
        is_black = !is_black;
    }
}

/// Decodes `input` with `params`; each row charged before it is kept.
pub(super) fn decode(
    input: &[u8],
    params: Params,
    offset: u64,
    budget: &mut Budget,
) -> Result<Vec<u8>, Error> {
    let bad = || malformed(offset, MalformedKind::InvalidFilterData);
    let columns = params.columns;
    let mut bits = Bits { data: input, at: 0 };
    let mut out = Emitter::new(budget);
    let mut row = vec![0_u8; columns.div_ceil(8) as usize];
    // The row above the first is white: no changing element.
    let mut reference: Vec<u32> = Vec::new();
    let mut decoded = 0_u32;
    loop {
        if params.rows.is_some_and(|rows| decoded >= rows) {
            break;
        }
        if params.byte_align && (params.k < 0 || !params.end_of_line) {
            bits.align();
        }
        // An end-of-line code may come before a row; two in a row end the
        // block (EOFB in Group 4, RTC in Group 3, the tag bit of a K > 0
        // one in between).
        if bits.end_of_line() {
            let start = bits.at;
            if params.k > 0 {
                bits.skip(1);
            }
            if bits.end_of_line() {
                break;
            }
            bits.at = start;
        }
        let two_dimensional = params.k < 0 || (params.k > 0 && bits.bit() == 0);
        if bits.exhausted() {
            break;
        }
        let changes = if two_dimensional {
            row_2d(&mut bits, &reference, columns, &bad)?
        } else {
            row_1d(&mut bits, columns, &bad)?
        };
        paint(&mut row, &changes, columns, params.black_is_1);
        out.emit(&row)?;
        reference = changes;
        reference.retain(|change| *change < columns);
        decoded += 1;
    }
    // Rows the data did not reach are white.
    if let Some(rows) = params.rows {
        paint(&mut row, &[], columns, params.black_is_1);
        for _ in decoded..rows {
            out.emit(&row)?;
        }
    }
    Ok(out.out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::{CancellationToken, ResourceLimits};

    fn budget() -> Budget {
        Budget::new(ResourceLimits::default(), CancellationToken::new())
    }

    /// Bits as a string of 0 and 1 (spaces ignored), padded to bytes.
    fn bits(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text
            .bytes()
            .filter(|b| *b != b' ')
            .map(|b| b - b'0')
            .collect();
        digits
            .chunks(8)
            .map(|chunk| {
                chunk
                    .iter()
                    .chain(std::iter::repeat(&0))
                    .take(8)
                    .fold(0, |byte, bit| (byte << 1) | bit)
            })
            .collect()
    }

    fn params(k: i64, columns: u32, rows: Option<u32>) -> Params {
        Params {
            k,
            columns,
            rows,
            end_of_line: false,
            byte_align: false,
            end_of_block: true,
            black_is_1: false,
        }
    }

    #[test]
    fn the_code_tables_are_prefix_free_and_complete() {
        for codes in [&WHITE, &BLACK] {
            let all: Vec<&str> = codes.iter().chain(EXTENDED.iter()).copied().collect();
            for (i, a) in all.iter().enumerate() {
                for (j, b) in all.iter().enumerate() {
                    assert!(i == j || !b.starts_with(a), "{a} is a prefix of {b}");
                }
            }
            // The table finds every code at its length.
            let table = table(codes);
            for (code, expected) in all.iter().zip((0..64).chain((1..=40).map(|i| i * 64))) {
                let value = u32::from_str_radix(code, 2).unwrap();
                let entry = table[(value << (PEEK - code.len() as u32)) as usize];
                assert_eq!((entry.run, usize::from(entry.len)), (expected, code.len()));
            }
        }
    }

    #[test]
    fn one_dimensional_rows_alternate_white_and_black_runs() {
        // Eight columns: 3 white (1000), 2 black (11), 3 white (1000); then
        // all white (white 8: 10011).
        let data = bits("1000 11 1000  10011");
        let decoded = decode(&data, params(0, 8, Some(2)), 0, &mut budget()).unwrap();
        // White is 1: 111 00 111, then 11111111.
        assert_eq!(decoded, [0b1110_0111, 0xFF]);
        let mut black_is_1 = params(0, 8, Some(1));
        black_is_1.black_is_1 = true;
        assert_eq!(
            decode(&data, black_is_1, 0, &mut budget()).unwrap(),
            [0b0001_1000]
        );
    }

    #[test]
    fn make_up_codes_add_to_a_run() {
        // 100 columns: white 64 (11011) + 36 (00010101).
        let data = bits("11011 00010101");
        assert_eq!(
            decode(&data, params(0, 100, Some(1)), 0, &mut budget()).unwrap(),
            [0xFF; 13]
        );
    }

    #[test]
    fn group_4_rows_follow_the_row_above() {
        // Row 1 (horizontal: 001, white 2 = 0111, black 3 = 10, then V0
        // reaching the end): ..###...; row 2 the same (V0, V0, V0); row 3
        // shifted right by one (VR1 011, VR1 011, V0); then EOFB.
        let data = bits("001 0111 10 1  1 1 1  011 011 1  000000000001 000000000001");
        let decoded = decode(&data, params(-1, 8, None), 0, &mut budget()).unwrap();
        assert_eq!(decoded, [0b1100_0111, 0b1100_0111, 0b1110_0011]);
    }

    #[test]
    fn pass_mode_skips_a_run_of_the_row_above() {
        // Row 1: 2 white, 2 black, 4 white (H 0111 11 then V0). Row 2 all
        // white: P (b1 = 2, b2 = 4), then V0 against b1 = 8.
        let data = bits("001 0111 11 1  0001 1");
        let decoded = decode(&data, params(-1, 8, Some(2)), 0, &mut budget()).unwrap();
        assert_eq!(decoded, [0b1100_1111, 0xFF]);
    }

    #[test]
    fn group_3_two_dimensional_rows_have_a_tag_bit() {
        // K > 0: tag 1, a 1D row (3 white, 5 black: 1000 0011); tag 0, a 2D
        // row copying it (V0, V0).
        let data = bits("1 1000 0011  0 1 1");
        let decoded = decode(&data, params(2, 8, Some(2)), 0, &mut budget()).unwrap();
        assert_eq!(decoded, [0b1110_0000, 0b1110_0000]);
    }

    #[test]
    fn end_of_line_codes_and_end_of_block() {
        // EOL, a row of 8 white, EOL EOL (end of block): one row.
        let mut with_eol = params(0, 8, None);
        with_eol.end_of_line = true;
        let data = bits("000000000001 10011 000000000001 000000000001");
        assert_eq!(decode(&data, with_eol, 0, &mut budget()).unwrap(), [0xFF]);
        // Fill bits before an EOL are allowed.
        let data = bits("0000000000000001 10011 000000000001 000000000001");
        assert_eq!(decode(&data, with_eol, 0, &mut budget()).unwrap(), [0xFF]);
    }

    #[test]
    fn rows_may_start_on_a_byte_boundary() {
        // Group 4 aligned: row 1 (V0) padded to a byte, row 2 (V0).
        let mut aligned = params(-1, 8, Some(2));
        aligned.byte_align = true;
        let data = bits("10000000 1");
        assert_eq!(
            decode(&data, aligned, 0, &mut budget()).unwrap(),
            [0xFF, 0xFF]
        );
    }

    #[test]
    fn damaged_data_is_an_error() {
        for (data, k) in [
            // A white run of 8 on 4 columns.
            (bits("10011 10011"), 0),
            // An end-of-line code inside a row, 1D and 2D.
            (bits("1000 000000000001"), 0),
            (bits("001 000000000001 1"), -1),
        ] {
            let result = decode(&data, params(k, 4, Some(2)), 7, &mut budget());
            assert_eq!(
                result,
                Err(Error::Malformed {
                    offset: 7,
                    kind: MalformedKind::InvalidFilterData
                }),
                "{data:?}"
            );
        }
        // Extension codes are unsupported, not damaged: 2D and 1D.
        let data = bits("0000001 111");
        assert_eq!(
            decode(&data, params(-1, 8, Some(1)), 0, &mut budget()),
            Err(Error::Unsupported(Capability::StreamFilter))
        );
        let data = bits("000000001 111");
        assert_eq!(
            decode(&data, params(0, 8, Some(1)), 0, &mut budget()),
            Err(Error::Unsupported(Capability::StreamFilter))
        );
    }

    #[test]
    fn runs_of_length_zero_cancel_out() {
        // 1D: white 3, black 0, white 5: one white row of 8.
        let data = bits("1000 0000110111 1100");
        assert_eq!(
            decode(&data, params(0, 8, Some(1)), 0, &mut budget()).unwrap(),
            [0xFF]
        );
        // 2D: 1 001 vertical modes one to the left of b1 = 8 put a0 at 7
        // again and again, alternating colours: the row keeps at most two
        // changing elements. An odd count leaves pixel 7 black.
        let mut text = "010 ".repeat(1001);
        text.push_str("1 ");
        let data = bits(&text);
        assert_eq!(
            decode(&data, params(-1, 8, Some(1)), 0, &mut budget()).unwrap(),
            [0xFE]
        );
        assert_eq!(change_after(&[7], 7), Vec::<u32>::new());
    }

    fn change_after(start: &[u32], at: u32) -> Vec<u32> {
        let mut changes = start.to_vec();
        change(&mut changes, at);
        changes
    }

    #[test]
    fn group_4_may_put_an_end_of_line_between_rows() {
        // EOL before each of two white rows, then EOFB.
        let data = bits("000000000001 1 000000000001 1 000000000001 000000000001");
        assert_eq!(
            decode(&data, params(-1, 8, None), 0, &mut budget()).unwrap(),
            [0xFF, 0xFF]
        );
    }

    #[test]
    fn rows_the_data_does_not_reach_are_white() {
        // EOFB after one row, three rows announced.
        let data = bits("1 000000000001 000000000001");
        assert_eq!(
            decode(&data, params(-1, 8, Some(3)), 0, &mut budget()).unwrap(),
            [0xFF; 3]
        );
        // No more data after one row; with BlackIs1, white is 0.
        let mut black_is_1 = params(-1, 8, Some(3));
        black_is_1.black_is_1 = true;
        assert_eq!(
            decode(&bits("1"), black_is_1, 0, &mut budget()).unwrap(),
            [0; 3]
        );
    }

    #[test]
    fn two_end_of_line_codes_end_a_group_3_block() {
        // Even without /EndOfBlock: EOL, a white row, RTC.
        let mut without = params(0, 8, None);
        without.end_of_block = false;
        without.end_of_line = true;
        let data = bits("000000000001 10011 000000000001 000000000001");
        assert_eq!(decode(&data, without, 0, &mut budget()).unwrap(), [0xFF]);
        // K > 0: EOL and a tag bit 1 before each row, and in the RTC.
        let data = bits("000000000001 1 10011 000000000001 1 000000000001 1");
        assert_eq!(
            decode(&data, params(2, 8, None), 0, &mut budget()).unwrap(),
            [0xFF]
        );
    }

    #[test]
    fn parameters_are_validated() {
        let mut dictionary = Dictionary::new();
        dictionary.insert(b"Columns".to_vec(), Object::Integer(0));
        assert!(Params::from(Some(&dictionary), 0).is_err());
        dictionary.insert(
            b"Columns".to_vec(),
            Object::Integer(i64::from(MAX_COLUMNS) + 1),
        );
        assert!(Params::from(Some(&dictionary), 0).is_err());
        dictionary.insert(b"Columns".to_vec(), Object::Integer(i64::from(MAX_COLUMNS)));
        assert!(Params::from(Some(&dictionary), 0).is_ok());
        dictionary.insert(b"BlackIs1".to_vec(), Object::Integer(1));
        assert!(Params::from(Some(&dictionary), 0).is_err());
        let defaults = Params::from(None, 0).unwrap();
        assert_eq!(
            (defaults.k, defaults.columns, defaults.rows),
            (0, 1728, None)
        );
        assert!(defaults.end_of_block && !defaults.black_is_1);
    }

    #[test]
    fn every_row_is_charged() {
        // Group 4, 1 000 white rows of 8 columns: one bit each.
        let data = bits(&"1".repeat(1000));
        let mut small = Budget::new(
            ResourceLimits {
                max_decoded_bytes: 999,
                ..ResourceLimits::default()
            },
            CancellationToken::new(),
        );
        assert!(decode(&data, params(-1, 8, Some(1000)), 0, &mut small).is_err());
        assert_eq!(
            decode(&data, params(-1, 8, Some(1000)), 0, &mut budget())
                .unwrap()
                .len(),
            1000
        );
    }
}
