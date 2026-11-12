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
//! - a token the lexer refuses, or a block not closed by its `end` keyword,
//!   ends the reading with an error;
//! - an entry that breaks the rules (lengths that differ, a range whose
//!   start exceeds its end, a destination that is not UTF-16 or longer than
//!   [`MAX_DESTINATION_UNITS`], a CID above [`MAX_CID`]) is skipped and
//!   counted in [`CMap::skipped_entries`]. A block whose operand count does
//!   not fit its entries (a token missing or extra) is skipped whole, since
//!   pairing its operands would shift every entry after the gap;
//! - each token of a block, each entry kept and the lookup structures are
//!   charged to the budget; ranges are never expanded;
//! - `usecmap` only names the parent: finding it, and bounding the depth of
//!   such chains, is the caller's (see [`CMap::inherit`]).
//!
//! Two tolerances, both common practice (pdf.js, MuPDF): a destination of
//! one byte is read as one UTF-16 unit (Latin-1), and a `bfrange` adds the
//! code offset to the last UTF-16 unit of its destination (not its last
//! byte), without carrying past `U+FFFF`.
//!
//! Codes are looked up with their length in bytes: `<41>` and `<0041>` are
//! different codes. When ranges overlap, the one defined last wins. Lookups
//! take logarithmic time: ranges are flattened into disjoint segments once
//! the CMap is read.

use std::collections::BinaryHeap;
use std::mem::size_of;

use pdf_syntax::{Budget, Lexer, Token};

/// Longest code, in bytes (9.7.6.2).
pub const MAX_CODE_BYTES: usize = 4;
/// Highest CID (ISO 32000-2, annex C).
pub const MAX_CID: u32 = 65_535;
/// Longest destination string of a `bfchar` or `bfrange` entry, in UTF-16
/// units (512 bytes).
pub const MAX_DESTINATION_UNITS: usize = 256;
/// Most codespace ranges kept; real CMaps have a few.
pub const MAX_CODESPACES: usize = 256;

/// Why a CMap could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CMapError {
    /// The lexer refused a token, a limit was exceeded, or the reading was
    /// cancelled. Offsets are relative to the CMap data.
    Syntax(pdf_syntax::Error),
    /// A mapping block is not closed by its `end` keyword before the end of
    /// the data, another block or `endcmap`.
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

/// Codes `low..=high` take the range at index `range`.
#[derive(Debug, Clone, Copy)]
struct Segment {
    low: u32,
    high: u32,
    range: u32,
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

/// Memory charged for a stored value beyond its range.
trait Cost {
    fn cost(&self) -> u64;
}

impl Cost for u32 {
    fn cost(&self) -> u64 {
        0
    }
}

impl Cost for Destination {
    fn cost(&self) -> u64 {
        match self {
            Self::Base(units) => units.len() as u64 * 2,
            Self::Each(each) => each
                .iter()
                .map(|units| VEC_COST + units.len() as u64 * 2)
                .sum(),
        }
    }
}

/// The ranges of one code length in definition order, and the disjoint
/// segments they resolve to once the CMap is read.
#[derive(Debug, Clone)]
struct Table<T> {
    ranges: Vec<Range<T>>,
    segments: Vec<Segment>,
}

impl<T> Default for Table<T> {
    fn default() -> Self {
        Self {
            ranges: Vec::new(),
            segments: Vec::new(),
        }
    }
}

/// Approximate memory of a `Vec` header, charged with each inner vector.
const VEC_COST: u64 = size_of::<Vec<u16>>() as u64;
/// Approximate memory of one stored entry, charged to the budget.
const ENTRY_COST: u64 = size_of::<Range<Destination>>() as u64;
/// Memory used per range while flattening: two events, a heap slot, a flag
/// and up to two segments.
const FLATTEN_COST: u64 =
    4 * size_of::<(u64, usize)>() as u64 + 2 * size_of::<Segment>() as u64 + 1;
/// Memory charged per token kept while a block is read, including the
/// growth of the vector that holds them.
const TOKEN_COST: u64 = 2 * size_of::<Token<'static>>() as u64;

impl<T: Cost> Table<T> {
    fn cost(&self) -> u64 {
        self.ranges
            .iter()
            .map(|range| ENTRY_COST.saturating_add(range.value.cost()))
            .fold(0, u64::saturating_add)
    }

    /// Resolves the ranges into disjoint segments, each taking the range
    /// defined last among those covering it: a sweep over range starts and
    /// ends with a heap ordered by definition, in O(n log n).
    fn finish(&mut self, budget: &mut Budget) -> Result<(), CMapError> {
        let count = self.ranges.len();
        budget.charge_decoded_bytes((count as u64).saturating_mul(FLATTEN_COST))?;
        let mut starts: Vec<(u64, usize)> = self
            .ranges
            .iter()
            .enumerate()
            .map(|(index, range)| (u64::from(range.low), index))
            .collect();
        let mut ends: Vec<(u64, usize)> = self
            .ranges
            .iter()
            .enumerate()
            .map(|(index, range)| (u64::from(range.high) + 1, index))
            .collect();
        starts.sort_unstable();
        ends.sort_unstable();
        let mut positions: Vec<u64> = starts.iter().chain(&ends).map(|(at, _)| *at).collect();
        positions.sort_unstable();
        positions.dedup();
        let mut active = vec![false; count];
        let mut heap = BinaryHeap::with_capacity(count);
        let (mut next_start, mut next_end) = (starts.iter().peekable(), ends.iter().peekable());
        let mut segments: Vec<Segment> = Vec::new();
        for (at, next) in positions.iter().zip(positions.iter().skip(1)) {
            while let Some((_, index)) = next_end.next_if(|(end, _)| end == at) {
                if let Some(flag) = active.get_mut(*index) {
                    *flag = false;
                }
            }
            while let Some((_, index)) = next_start.next_if(|(start, _)| start == at) {
                if let (Some(flag), Some(range)) = (active.get_mut(*index), self.ranges.get(*index))
                {
                    *flag = true;
                    heap.push((range.order, *index));
                }
            }
            while let Some((_, index)) = heap.peek() {
                if active.get(*index).copied().unwrap_or(false) {
                    break;
                }
                heap.pop();
            }
            let Some((_, index)) = heap.peek() else {
                continue;
            };
            let (Ok(low), Ok(high), Ok(range)) = (
                u32::try_from(*at),
                u32::try_from(next - 1),
                u32::try_from(*index),
            ) else {
                continue;
            };
            match segments.last_mut() {
                Some(last) if last.range == range && u64::from(last.high) + 1 == *at => {
                    last.high = high;
                }
                _ => segments.push(Segment { low, high, range }),
            }
        }
        self.segments = segments;
        Ok(())
    }

    /// The range defined last among those containing `value`.
    fn find(&self, value: u32) -> Option<&Range<T>> {
        let end = self
            .segments
            .partition_point(|segment| segment.low <= value);
        let segment = self.segments.get(end.checked_sub(1)?)?;
        if segment.high < value {
            return None;
        }
        self.ranges.get(usize::try_from(segment.range).ok()?)
    }
}

/// Tables indexed by code length minus one.
type ByLength<T> = [Table<T>; MAX_CODE_BYTES];

fn tables_cost<T: Cost>(tables: &ByLength<T>) -> u64 {
    tables.iter().map(Table::cost).fold(0, u64::saturating_add)
}

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

impl CMap {
    /// Reads the mapping operators of `data`, charging `budget` for the
    /// tokens, each entry kept and the lookup structures.
    pub fn parse(data: &[u8], budget: &mut Budget) -> Result<Self, CMapError> {
        let mut cmap = Self::default();
        let mut lexer = Lexer::new(data);
        // The last two tokens, for `/Name usecmap` and `/WMode n def`.
        let mut previous: Option<Token<'_>> = None;
        let mut before: Option<Token<'_>> = None;
        while let Some(spanned) = lexer.next_token(budget)? {
            let offset = spanned.offset;
            let kind = match &spanned.token {
                Token::Keyword(b"begincodespacerange") => Some(Block::Codespace),
                Token::Keyword(b"beginbfchar") => Some(Block::BfChar),
                Token::Keyword(b"beginbfrange") => Some(Block::BfRange),
                Token::Keyword(b"begincidchar") => Some(Block::Cid {
                    range: false,
                    notdef: false,
                }),
                Token::Keyword(b"begincidrange") => Some(Block::Cid {
                    range: true,
                    notdef: false,
                }),
                Token::Keyword(b"beginnotdefchar") => Some(Block::Cid {
                    range: false,
                    notdef: true,
                }),
                Token::Keyword(b"beginnotdefrange") => Some(Block::Cid {
                    range: true,
                    notdef: true,
                }),
                Token::Keyword(b"usecmap") => {
                    if let Some(Token::Name(name)) = &previous {
                        cmap.parent = Some(name.clone());
                    }
                    None
                }
                Token::Keyword(b"def") => {
                    if let (Some(Token::Name(key)), Some(Token::Integer(mode))) =
                        (&before, &previous)
                        && key.as_slice() == b"WMode"
                    {
                        cmap.vertical = *mode == 1;
                    }
                    None
                }
                _ => None,
            };
            if let Some(kind) = kind {
                let operands = block(&mut lexer, kind.end(), offset, budget)?;
                match kind {
                    Block::Codespace => cmap.codespace_block(&operands, budget)?,
                    Block::BfChar => cmap.bfchar_block(&operands, budget)?,
                    Block::BfRange => cmap.bfrange_block(&operands, budget)?,
                    Block::Cid { range, notdef } => {
                        cmap.cid_block(&operands, range, notdef, budget)?;
                    }
                }
            }
            before = previous.take();
            previous = Some(spanned.token);
        }
        cmap.finish(budget)?;
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
    /// code. The parent's own parent is not followed. Everything copied,
    /// and the lookup structures rebuilt, are charged to `budget`.
    pub fn inherit(&mut self, parent: &Self, budget: &mut Budget) -> Result<(), CMapError> {
        let copied = tables_cost(&parent.unicode)
            .saturating_add(tables_cost(&parent.cids))
            .saturating_add(tables_cost(&parent.notdefs))
            .saturating_add(parent.codespaces.len() as u64 * size_of::<Codespace>() as u64);
        budget.charge_decoded_bytes(copied)?;
        let shift = parent.defined;
        fn merge<T: Clone>(own: &mut ByLength<T>, parent: &ByLength<T>, shift: u32) {
            for (own, parent) in own.iter_mut().zip(parent) {
                for range in &mut own.ranges {
                    range.order = range.order.saturating_add(shift);
                }
                own.ranges.extend(parent.ranges.iter().cloned());
            }
        }
        merge(&mut self.unicode, &parent.unicode, shift);
        merge(&mut self.cids, &parent.cids, shift);
        merge(&mut self.notdefs, &parent.notdefs, shift);
        self.defined = self.defined.saturating_add(shift);
        let own = std::mem::take(&mut self.codespaces);
        for codespace in parent.codespaces.iter().chain(&own) {
            self.add_codespace(*codespace);
        }
        self.finish(budget)
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

    /// The Unicode text of `code` from `bfchar` and `bfrange`, at most
    /// [`MAX_DESTINATION_UNITS`] UTF-16 units, or `None` when the CMap does
    /// not map it or maps it to invalid UTF-16.
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

    /// The CID of `code` from `cidchar` and `cidrange`, or `None` (also
    /// when a range would reach past [`MAX_CID`]).
    pub fn cid(&self, code: Code) -> Option<u32> {
        let range = table(&self.cids, code)?.find(code.value)?;
        let cid = range
            .value
            .checked_add(code.value.checked_sub(range.low)?)?;
        (cid <= MAX_CID).then_some(cid)
    }

    /// The CID drawn for `code` when it has no CID of its own, from
    /// `notdefchar` and `notdefrange` (9.7.6.3), or `None`. A notdef range
    /// maps all its codes to the same CID.
    pub fn notdef(&self, code: Code) -> Option<u32> {
        Some(table(&self.notdefs, code)?.find(code.value)?.value)
    }

    fn finish(&mut self, budget: &mut Budget) -> Result<(), CMapError> {
        for table in self.unicode.iter_mut() {
            table.finish(budget)?;
        }
        for table in self.cids.iter_mut().chain(self.notdefs.iter_mut()) {
            table.finish(budget)?;
        }
        Ok(())
    }

    fn next_order(&mut self) -> u32 {
        let order = self.defined;
        self.defined = self.defined.saturating_add(1);
        order
    }

    /// Skips a whole block whose operands do not divide into entries of
    /// `width` tokens; returns whether it was skipped.
    fn misaligned(&mut self, operands: &[Token<'_>], width: usize) -> bool {
        if operands.len().is_multiple_of(width) {
            return false;
        }
        self.skipped = self
            .skipped
            .saturating_add(operands.len().div_ceil(width) as u64);
        true
    }

    fn add_codespace(&mut self, codespace: Codespace) {
        if self.codespaces.contains(&codespace) {
            return;
        }
        if self.codespaces.len() >= MAX_CODESPACES {
            self.skipped = self.skipped.saturating_add(1);
            return;
        }
        self.codespaces.push(codespace);
    }

    fn codespace_block(
        &mut self,
        operands: &[Token<'_>],
        budget: &mut Budget,
    ) -> Result<(), CMapError> {
        if self.misaligned(operands, 2) {
            return Ok(());
        }
        for pair in operands.chunks(2) {
            let [Token::HexString(low), Token::HexString(high)] = pair else {
                self.skipped = self.skipped.saturating_add(1);
                continue;
            };
            let valid = low.len() == high.len()
                && (1..=MAX_CODE_BYTES).contains(&low.len())
                && low.iter().zip(high).all(|(low, high)| low <= high);
            let (Some(len), true) = (u8::try_from(low.len()).ok(), valid) else {
                self.skipped = self.skipped.saturating_add(1);
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
            self.add_codespace(codespace);
        }
        Ok(())
    }

    fn bfchar_block(
        &mut self,
        operands: &[Token<'_>],
        budget: &mut Budget,
    ) -> Result<(), CMapError> {
        if self.misaligned(operands, 2) {
            return Ok(());
        }
        for pair in operands.chunks(2) {
            let [Token::HexString(source), Token::HexString(destination)] = pair else {
                // Includes destinations given as glyph names, not read.
                self.skipped = self.skipped.saturating_add(1);
                continue;
            };
            let (Some(source), Some(destination)) = (code_of(source), utf16(destination)) else {
                self.skipped = self.skipped.saturating_add(1);
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
                    rest.split_at(end.saturating_add(1).min(rest.len()))
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
                self.skipped = self.skipped.saturating_add(1);
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
                self.skipped = self.skipped.saturating_add(1);
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
        if self.misaligned(operands, width) {
            return Ok(());
        }
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
            let Some(((low, high), cid)) = parsed.filter(|(_, cid)| *cid <= MAX_CID) else {
                self.skipped = self.skipped.saturating_add(1);
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
        budget.charge_decoded_bytes(ENTRY_COST.saturating_add(destination.cost()))?;
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

/// The mapping blocks.
#[derive(Debug, Clone, Copy)]
enum Block {
    Codespace,
    BfChar,
    BfRange,
    Cid { range: bool, notdef: bool },
}

impl Block {
    fn end(self) -> &'static [u8] {
        match self {
            Self::Codespace => b"endcodespacerange",
            Self::BfChar => b"endbfchar",
            Self::BfRange => b"endbfrange",
            Self::Cid {
                range: false,
                notdef: false,
            } => b"endcidchar",
            Self::Cid {
                range: true,
                notdef: false,
            } => b"endcidrange",
            Self::Cid {
                range: false,
                notdef: true,
            } => b"endnotdefchar",
            Self::Cid {
                range: true,
                notdef: true,
            } => b"endnotdefrange",
        }
    }
}

/// The tokens between a `begin…` keyword and `end`, each charged as kept.
/// A nested `begin…` keyword, `endcmap` or the end of the data before `end`
/// is an error.
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
            token => {
                budget.charge_decoded_bytes(TOKEN_COST)?;
                operands.push(token);
            }
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

/// UTF-16BE code units of a destination string, at most
/// [`MAX_DESTINATION_UNITS`]. A single byte, as some producers write for
/// Latin text, is taken as one unit; another odd length or an empty string
/// is invalid.
fn utf16(bytes: &[u8]) -> Option<Vec<u16>> {
    match bytes {
        [] => None,
        [byte] => Some(vec![u16::from(*byte)]),
        _ if !bytes.len().is_multiple_of(2) || bytes.len() > 2 * MAX_DESTINATION_UNITS => None,
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
