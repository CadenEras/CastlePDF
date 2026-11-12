//! CMaps (ISO 32000-2, 9.7.5 and 9.10.3; Adobe Technical Note #5014): the
//! `/ToUnicode` map of a font, from character codes to Unicode, and the
//! encoding of a Type 0 font, from codes to CIDs (ADR-0006, tranche 3.2).
//!
//! A CMap is a PostScript program; only its mapping operators are read:
//! `begincodespacerange`, `beginbfchar`, `beginbfrange`, `begincidchar`,
//! `begincidrange`, `beginnotdefchar`, `beginnotdefrange` (each closed by
//! its `end` keyword), `usecmap` and `/WMode n def`. Everything else
//! (`/CIDInit /ProcSet findresource begin`, `/CIDSystemInfo`, `def`...) is
//! skipped.
//!
//! The input is untrusted:
//! - a token the lexer refuses ends the reading with an error;
//! - an entry that breaks the rules (lengths that differ, a range whose
//!   start exceeds its end, a destination that is not UTF-16) is skipped
//!   and counted in [`CMap::skipped_entries`], never guessed;
//! - ranges are stored as ranges, never expanded, and each stored entry is
//!   charged to the budget, so memory stays proportional to the input;
//! - `usecmap` only names the parent: merging it, and bounding the depth of
//!   such chains, is the caller's (see [`CMap::inherit`]).
//!
//! Codes are looked up with their length in bytes: `<41>` and `<0041>` are
//! different codes. When ranges overlap, the one defined last wins.

use std::mem::size_of;

use pdf_syntax::{Budget, Lexer, Token};

/// Longest code, in bytes (9.7.6.2).
pub const MAX_CODE_BYTES: usize = 4;

/// Why a CMap could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CMapError {
    /// The lexer refused a token, a limit was exceeded, or the reading was
    /// cancelled. Offsets are relative to the CMap data.
    Syntax(pdf_syntax::Error),
    /// A mapping block is not closed by its `end` keyword before the end of
    /// the data.
    Unterminated {
        /// Offset of the `begin` keyword.
        offset: u64,
    },
}

impl From<pdf_syntax::Error> for CMapError {
    fn from(error: pdf_syntax::Error) -> Self {
        Self::Syntax(error)
    }
}

/// A code: its value and its length in bytes (1 to 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Code {
    pub value: u32,
    pub len: u8,
}

/// One codespace range: each byte of a code lies between the bytes of the
/// same position in `low` and `high` (9.7.6.2), which is not a numeric
/// comparison: `<8140> <9FFC>` excludes `<81FF>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Codespace {
    len: u8,
    low: [u8; MAX_CODE_BYTES],
    high: [u8; MAX_CODE_BYTES],
}

impl Codespace {
    /// Leading bytes of `bytes` that lie in this range, at most `len`.
    fn matching_prefix(&self, bytes: &[u8]) -> usize {
        self.low
            .iter()
            .zip(&self.high)
            .zip(bytes)
            .take(usize::from(self.len))
            .take_while(|((low, high), byte)| (**low..=**high).contains(*byte))
            .count()
    }
}

/// A range of codes of one length and what it maps to.
#[derive(Debug, Clone)]
struct Range<T> {
    low: u32,
    high: u32,
    /// Definition order: the latest definition wins over earlier ones.
    order: u32,
    value: T,
}

/// Where a `bfchar` or `bfrange` entry sends its codes.
#[derive(Debug, Clone)]
enum Destination {
    /// UTF-16 code units of the first code; each next code of the range
    /// adds one to the last unit.
    Base(Vec<u16>),
    /// One UTF-16 string per code of the range (array form of `bfrange`).
    Each(Vec<Vec<u16>>),
}

/// Ranges of one code length, sorted by `low` once the CMap is read, with
/// the highest `high` seen up to each position, so that a lookup stops as
/// soon as no earlier range can contain the code.
#[derive(Debug, Clone)]
struct Table<T> {
    ranges: Vec<Range<T>>,
    reach: Vec<u32>,
}

impl<T> Default for Table<T> {
    fn default() -> Self {
        Self {
            ranges: Vec::new(),
            reach: Vec::new(),
        }
    }
}

impl<T> Table<T> {
    fn finish(&mut self) {
        self.ranges.sort_by_key(|range| (range.low, range.order));
        let mut reach = 0;
        self.reach = self
            .ranges
            .iter()
            .map(|range| {
                reach = reach.max(range.high);
                reach
            })
            .collect();
    }

    /// The range defined last among those containing `value`.
    fn find(&self, value: u32) -> Option<&Range<T>> {
        let end = self.ranges.partition_point(|range| range.low <= value);
        let mut best: Option<&Range<T>> = None;
        for (range, reach) in self.ranges.iter().zip(&self.reach).take(end).rev() {
            if *reach < value {
                break;
            }
            if range.high >= value && best.is_none_or(|best| range.order > best.order) {
                best = Some(range);
            }
        }
        best
    }
}

/// Tables indexed by code length minus one.
type ByLength<T> = [Table<T>; MAX_CODE_BYTES];

/// A CMap read from its data.
#[derive(Debug, Clone, Default)]
pub struct CMap {
    codespaces: Vec<Codespace>,
    unicode: ByLength<Destination>,
    cids: ByLength<u32>,
    notdefs: ByLength<u32>,
    /// Number of definitions so far, for `Range::order`.
    defined: u32,
    /// The parent named by `usecmap`, not merged yet.
    parent: Option<Vec<u8>>,
    vertical: bool,
    skipped: u64,
}

/// Approximate memory of one stored entry, charged to the budget.
const ENTRY_COST: u64 = size_of::<Range<Destination>>() as u64;

impl CMap {
    /// Reads the mapping operators of `data`, charging `budget` for the
    /// tokens and for each entry kept.
    pub fn parse(data: &[u8], budget: &mut Budget) -> Result<Self, CMapError> {
        let mut cmap = Self::default();
        let mut lexer = Lexer::new(data);
        // The last two tokens, for `/Name usecmap` and `/WMode n def`.
        let mut previous: Option<Token<'_>> = None;
        let mut before: Option<Token<'_>> = None;
        while let Some(spanned) = lexer.next_token(budget)? {
            let offset = spanned.offset;
            match &spanned.token {
                Token::Keyword(b"begincodespacerange") => {
                    let operands = block(&mut lexer, b"endcodespacerange", offset, budget)?;
                    cmap.codespace_block(&operands, budget)?;
                }
                Token::Keyword(b"beginbfchar") => {
                    let operands = block(&mut lexer, b"endbfchar", offset, budget)?;
                    cmap.bfchar_block(&operands, budget)?;
                }
                Token::Keyword(b"beginbfrange") => {
                    let operands = block(&mut lexer, b"endbfrange", offset, budget)?;
                    cmap.bfrange_block(&operands, budget)?;
                }
                Token::Keyword(b"begincidchar") => {
                    let operands = block(&mut lexer, b"endcidchar", offset, budget)?;
                    cmap.cid_block(&operands, false, false, budget)?;
                }
                Token::Keyword(b"begincidrange") => {
                    let operands = block(&mut lexer, b"endcidrange", offset, budget)?;
                    cmap.cid_block(&operands, true, false, budget)?;
                }
                Token::Keyword(b"beginnotdefchar") => {
                    let operands = block(&mut lexer, b"endnotdefchar", offset, budget)?;
                    cmap.cid_block(&operands, false, true, budget)?;
                }
                Token::Keyword(b"beginnotdefrange") => {
                    let operands = block(&mut lexer, b"endnotdefrange", offset, budget)?;
                    cmap.cid_block(&operands, true, true, budget)?;
                }
                Token::Keyword(b"usecmap") => {
                    if let Some(Token::Name(name)) = &previous {
                        cmap.parent = Some(name.clone());
                    }
                }
                Token::Keyword(b"def") => {
                    if let (Some(Token::Name(key)), Some(Token::Integer(mode))) =
                        (&before, &previous)
                        && key.as_slice() == b"WMode"
                    {
                        cmap.vertical = *mode == 1;
                    }
                }
                _ => {}
            }
            before = previous.take();
            previous = Some(spanned.token);
        }
        cmap.finish();
        Ok(cmap)
    }

    /// The CMap named by `usecmap`, if any: the caller looks it up and calls
    /// [`CMap::inherit`].
    pub fn parent(&self) -> Option<&[u8]> {
        self.parent.as_deref()
    }

    /// Writing mode: `/WMode 1` (vertical).
    pub fn is_vertical(&self) -> bool {
        self.vertical
    }

    /// Entries skipped because they break the rules.
    pub fn skipped_entries(&self) -> u64 {
        self.skipped
    }

    /// Whether the CMap declares codespace ranges.
    pub fn has_codespace(&self) -> bool {
        !self.codespaces.is_empty()
    }

    /// Takes the codespaces and mappings of `parent` (the CMap named by
    /// `usecmap`) under those of this CMap, which win where both define a
    /// code. The parent's own parent is not followed. Each entry copied is
    /// charged to `budget`.
    pub fn inherit(&mut self, parent: &Self, budget: &mut Budget) -> Result<(), CMapError> {
        let shift = parent.defined;
        let count =
            |tables: &[Table<u32>]| tables.iter().map(|t| t.ranges.len() as u64).sum::<u64>();
        let entries = parent
            .unicode
            .iter()
            .map(|t| t.ranges.len() as u64)
            .sum::<u64>()
            .saturating_add(count(&parent.cids))
            .saturating_add(count(&parent.notdefs));
        budget.charge_decoded_bytes(entries.saturating_mul(ENTRY_COST))?;
        fn merge<T: Clone>(own: &mut ByLength<T>, parent: &ByLength<T>, shift: u32) {
            for (own, parent) in own.iter_mut().zip(parent) {
                for range in &mut own.ranges {
                    range.order = range.order.saturating_add(shift);
                }
                own.ranges.extend(parent.ranges.iter().cloned());
                own.finish();
            }
        }
        merge(&mut self.unicode, &parent.unicode, shift);
        merge(&mut self.cids, &parent.cids, shift);
        merge(&mut self.notdefs, &parent.notdefs, shift);
        self.defined = self.defined.saturating_add(shift);
        let mut codespaces = parent.codespaces.clone();
        codespaces.extend(self.codespaces.iter().copied());
        self.codespaces = codespaces;
        Ok(())
    }

    /// The next code of `bytes` and its length (9.7.6.2), or `None` if
    /// `bytes` is empty. A sequence that matches no codespace range takes
    /// the length of the range matching most of its leading bytes, or the
    /// shortest range (9.7.6.3), never more bytes than remain. Without any
    /// codespace range, codes are one byte.
    pub fn next_code(&self, bytes: &[u8]) -> Option<Code> {
        if bytes.is_empty() {
            return None;
        }
        let mut best: Option<(usize, u8)> = None;
        for len in 1..=MAX_CODE_BYTES {
            for codespace in self.codespaces.iter().filter(|c| usize::from(c.len) == len) {
                let matched = codespace.matching_prefix(bytes);
                if matched == len {
                    return code(bytes, codespace.len);
                }
                // Partial: most leading bytes first, then the shortest range.
                if best.is_none_or(|(most, _)| matched > most) {
                    best = Some((matched, codespace.len));
                }
            }
        }
        let len = best.map_or(1, |(_, len)| len);
        let len = u8::try_from(bytes.len().min(usize::from(len))).unwrap_or(1);
        code(bytes, len)
    }

    /// The Unicode text of `code` from `bfchar` and `bfrange`, or `None`
    /// when the CMap does not map it or maps it to invalid UTF-16.
    pub fn unicode(&self, code: Code) -> Option<String> {
        let range = table(&self.unicode, code)?.find(code.value)?;
        let offset = code.value.checked_sub(range.low)?;
        let units = match &range.value {
            Destination::Base(base) => {
                let (last, head) = base.split_last()?;
                let last = u16::try_from(u32::from(*last).checked_add(offset)?).ok()?;
                let mut units = head.to_vec();
                units.push(last);
                units
            }
            Destination::Each(each) => each.get(usize::try_from(offset).ok()?)?.clone(),
        };
        char::decode_utf16(units)
            .collect::<Result<String, _>>()
            .ok()
    }

    /// The CID of `code` from `cidchar` and `cidrange`, or `None`.
    pub fn cid(&self, code: Code) -> Option<u32> {
        let range = table(&self.cids, code)?.find(code.value)?;
        range.value.checked_add(code.value.checked_sub(range.low)?)
    }

    /// The CID drawn for `code` when it has no CID of its own, from
    /// `notdefchar` and `notdefrange` (9.7.6.3), or `None`. A notdef range
    /// maps all its codes to the same CID.
    pub fn notdef(&self, code: Code) -> Option<u32> {
        Some(table(&self.notdefs, code)?.find(code.value)?.value)
    }

    fn finish(&mut self) {
        for tables in [&mut self.cids, &mut self.notdefs] {
            tables.iter_mut().for_each(Table::finish);
        }
        self.unicode.iter_mut().for_each(Table::finish);
    }

    fn next_order(&mut self) -> u32 {
        let order = self.defined;
        self.defined = self.defined.saturating_add(1);
        order
    }

    fn codespace_block(
        &mut self,
        operands: &[Token<'_>],
        budget: &mut Budget,
    ) -> Result<(), CMapError> {
        for pair in operands.chunks(2) {
            let [Token::HexString(low), Token::HexString(high)] = pair else {
                self.skipped += 1;
                continue;
            };
            let valid = low.len() == high.len()
                && (1..=MAX_CODE_BYTES).contains(&low.len())
                && low.iter().zip(high).all(|(low, high)| low <= high);
            let (Some(len), true) = (u8::try_from(low.len()).ok(), valid) else {
                self.skipped += 1;
                continue;
            };
            budget.charge_decoded_bytes(size_of::<Codespace>() as u64)?;
            let mut codespace = Codespace {
                len,
                low: [0; MAX_CODE_BYTES],
                high: [0; MAX_CODE_BYTES],
            };
            codespace
                .low
                .iter_mut()
                .zip(low)
                .for_each(|(to, from)| *to = *from);
            codespace
                .high
                .iter_mut()
                .zip(high)
                .for_each(|(to, from)| *to = *from);
            self.codespaces.push(codespace);
        }
        Ok(())
    }

    fn bfchar_block(
        &mut self,
        operands: &[Token<'_>],
        budget: &mut Budget,
    ) -> Result<(), CMapError> {
        for pair in operands.chunks(2) {
            let [Token::HexString(source), Token::HexString(destination)] = pair else {
                // Includes destinations given as glyph names, not read.
                self.skipped += 1;
                continue;
            };
            let (Some(source), Some(destination)) = (code_of(source), utf16(destination)) else {
                self.skipped += 1;
                continue;
            };
            self.push_unicode(source, source.value, Destination::Base(destination), budget)?;
        }
        Ok(())
    }

    fn bfrange_block(
        &mut self,
        operands: &[Token<'_>],
        budget: &mut Budget,
    ) -> Result<(), CMapError> {
        let mut rest = operands;
        while !rest.is_empty() {
            let (entry, tail) = match rest {
                [
                    Token::HexString(_),
                    Token::HexString(_),
                    Token::ArrayStart,
                    ..,
                ] => {
                    let end = rest
                        .iter()
                        .position(|token| *token == Token::ArrayEnd)
                        .unwrap_or(rest.len());
                    rest.split_at((end + 1).min(rest.len()))
                }
                _ => rest.split_at(3.min(rest.len())),
            };
            rest = tail;
            let range = match entry {
                [
                    Token::HexString(low),
                    Token::HexString(high),
                    destination @ ..,
                ] => code_range(low, high).map(|range| (range, destination)),
                _ => None,
            };
            let Some(((low, high), destination)) = range else {
                self.skipped += 1;
                continue;
            };
            let destination = match destination {
                [Token::HexString(base)] => utf16(base).map(Destination::Base),
                [Token::ArrayStart, items @ .., Token::ArrayEnd] => items
                    .iter()
                    .map(|item| match item {
                        Token::HexString(text) => utf16(text),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()
                    .map(Destination::Each),
                _ => None,
            };
            let Some(destination) = destination else {
                self.skipped += 1;
                continue;
            };
            self.push_unicode(low, high, destination, budget)?;
        }
        Ok(())
    }

    fn cid_block(
        &mut self,
        operands: &[Token<'_>],
        range: bool,
        notdef: bool,
        budget: &mut Budget,
    ) -> Result<(), CMapError> {
        let width = if range { 3 } else { 2 };
        for entry in operands.chunks(width) {
            let parsed = match entry {
                [
                    Token::HexString(low),
                    Token::HexString(high),
                    Token::Integer(cid),
                ] if range => code_range(low, high).zip(u32::try_from(*cid).ok()),
                [Token::HexString(code), Token::Integer(cid)] if !range => code_of(code)
                    .map(|code| (code, code.value))
                    .zip(u32::try_from(*cid).ok()),
                _ => None,
            };
            let Some(((low, high), cid)) = parsed else {
                self.skipped += 1;
                continue;
            };
            budget.charge_decoded_bytes(ENTRY_COST)?;
            let order = self.next_order();
            let tables = if notdef {
                &mut self.notdefs
            } else {
                &mut self.cids
            };
            if let Some(table) = tables.get_mut(usize::from(low.len).saturating_sub(1)) {
                table.ranges.push(Range {
                    low: low.value,
                    high,
                    order,
                    value: cid,
                });
            }
        }
        Ok(())
    }

    fn push_unicode(
        &mut self,
        low: Code,
        high: u32,
        destination: Destination,
        budget: &mut Budget,
    ) -> Result<(), CMapError> {
        let units = match &destination {
            Destination::Base(base) => base.len(),
            Destination::Each(each) => each.iter().map(Vec::len).sum(),
        };
        budget.charge_decoded_bytes(ENTRY_COST.saturating_add(units as u64 * 2))?;
        let order = self.next_order();
        if let Some(table) = self.unicode.get_mut(usize::from(low.len).saturating_sub(1)) {
            table.ranges.push(Range {
                low: low.value,
                high,
                order,
                value: destination,
            });
        }
        Ok(())
    }
}

/// The tokens between a `begin…` keyword and `end`, charged as read. A
/// nested `begin…` keyword or the end of the data before `end` is an error.
fn block<'a>(
    lexer: &mut Lexer<'a>,
    end: &[u8],
    offset: u64,
    budget: &mut Budget,
) -> Result<Vec<Token<'a>>, CMapError> {
    let mut operands = Vec::new();
    loop {
        let Some(spanned) = lexer.next_token(budget)? else {
            return Err(CMapError::Unterminated { offset });
        };
        match spanned.token {
            Token::Keyword(keyword) if keyword == end => return Ok(operands),
            Token::Keyword(keyword) if keyword.starts_with(b"begin") || keyword == b"endcmap" => {
                return Err(CMapError::Unterminated { offset });
            }
            token => operands.push(token),
        }
    }
}

fn table<T>(tables: &ByLength<T>, code: Code) -> Option<&Table<T>> {
    tables.get(usize::from(code.len).checked_sub(1)?)
}

/// The first `len` bytes of `bytes` as a code.
fn code(bytes: &[u8], len: u8) -> Option<Code> {
    code_of(bytes.get(..usize::from(len))?)
}

/// A code of 1 to 4 bytes, big-endian.
fn code_of(bytes: &[u8]) -> Option<Code> {
    if !(1..=MAX_CODE_BYTES).contains(&bytes.len()) {
        return None;
    }
    let value = bytes
        .iter()
        .fold(0u32, |value, byte| (value << 8) | u32::from(*byte));
    Some(Code {
        value,
        len: u8::try_from(bytes.len()).ok()?,
    })
}

/// A range of codes of the same length, start not after end.
fn code_range(low: &[u8], high: &[u8]) -> Option<(Code, u32)> {
    let (low, high) = (code_of(low)?, code_of(high)?);
    (low.len == high.len && low.value <= high.value).then_some((low, high.value))
}

/// UTF-16BE code units of a destination string. A single byte, as some
/// producers write for Latin text, is taken as one unit; another odd length
/// or an empty string is invalid.
fn utf16(bytes: &[u8]) -> Option<Vec<u16>> {
    match bytes {
        [] => None,
        [byte] => Some(vec![u16::from(*byte)]),
        _ if bytes.len() % 2 == 1 => None,
        _ => Some(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_be_bytes(*pair))
                .collect(),
        ),
    }
}
