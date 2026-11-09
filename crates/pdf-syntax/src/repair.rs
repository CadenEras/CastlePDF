//! Cross-reference reconstruction (ADR-0004): when the strict reading of the
//! file structure fails, the objects are found again by one linear scan of the
//! file.
//!
//! The scan looks for `n g obj` headers and skips stream data. When `/Length`
//! is a direct integer followed by `endstream`, it skips by `/Length`;
//! otherwise it skips to the next `endstream`. Bytes that look like a header
//! inside stream data are therefore never indexed. When a number appears more
//! than once, the last occurrence in the file wins, as with incremental
//! updates. Each accepted header is charged to `max_objects_visited`.
//!
//! The scan never goes back. Every dictionary it parses is limited to the
//! bytes up to the next keyword it found (`obj`, `stream`, `trailer`), and
//! those ranges do not overlap, so its cost stays linear in the file size.

use std::collections::{BTreeMap, VecDeque};

use crate::lexer::{is_regular, is_whitespace};
use crate::limits::{Budget, Capability, Error, MalformedKind};
use crate::object::{Dictionary, IndirectValue, Object, ObjectId};
use crate::parser::Parser;

/// Only the last `trailer` keywords are kept and parsed, newest first.
const MAX_TRAILERS: usize = 16;
/// White space allowed between the parts of an `n g obj` header.
const MAX_HEADER_SPACE: usize = 32;

/// How a file was read after a repair.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RepairReport {
    /// The strict reading error that led to the repair.
    pub cause: Error,
    /// Objects in the reconstructed index, direct and compressed.
    pub objects: usize,
    /// Object numbers found more than once; the last occurrence is used.
    pub conflicts: usize,
    /// Where the trailer of the repaired file comes from.
    pub trailer: RepairTrailer,
    /// Object streams found but not readable (malformed, unsupported filter):
    /// their objects are missing from the index and resolve to `null`.
    pub skipped_object_streams: usize,
    /// Streams with no `endstream` after them: the scan stopped at the end of
    /// the file, so objects after such a stream may be missing.
    pub unterminated_streams: usize,
}

impl RepairReport {
    /// True when objects may be missing from the index.
    pub fn is_incomplete(&self) -> bool {
        self.skipped_object_streams > 0 || self.unterminated_streams > 0
    }
}

/// Origin of the trailer of a repaired file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RepairTrailer {
    /// The newest `trailer` dictionary or cross-reference stream dictionary
    /// whose `/Root` is in the index.
    Found,
    /// No usable trailer: `/Root` is the last `/Type /Catalog` object.
    Catalog,
    /// The file opened strictly; only objects whose offset was wrong were
    /// looked up in the reconstructed index.
    Original,
}

/// Header of an object found by the scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) offset: u64,
    pub(crate) generation: u16,
}

/// What the scan found.
#[derive(Debug, Default)]
pub(crate) struct Scan {
    /// Last header for each object number.
    pub(crate) objects: BTreeMap<u32, Found>,
    pub(crate) conflicts: usize,
    pub(crate) unterminated_streams: usize,
    /// Object streams (`/Type /ObjStm`), as (offset, number), file order.
    /// Only the last occurrence of a number counts: see `object_streams`.
    streams: Vec<(u64, u32)>,
    /// Cross-reference stream dictionaries, as (offset, dictionary).
    xref_streams: Vec<(u64, Dictionary)>,
    /// The last `trailer` keywords: offset, and end of the bytes its
    /// dictionary may use (the next keyword found, or the end of the file).
    trailers: VecDeque<(usize, usize)>,
}

impl Scan {
    /// Object streams that are the last occurrence of their number, in file
    /// order.
    pub(crate) fn object_streams(&self) -> impl Iterator<Item = (u64, u32)> + '_ {
        self.streams
            .iter()
            .copied()
            .filter(|(offset, number)| self.is_last(*number, *offset))
    }

    /// True when `number` is an object stream (its last occurrence).
    pub(crate) fn is_object_stream(&self, number: u32) -> bool {
        self.object_streams().any(|(_, n)| n == number)
    }

    fn is_last(&self, number: u32, offset: u64) -> bool {
        self.objects
            .get(&number)
            .is_some_and(|found| found.offset == offset)
    }
}

/// Finds the occurrences of `needle` in order, remembering the next one so
/// that the whole scan searches each byte a bounded number of times.
struct Finder {
    needle: &'static [u8],
    next: Option<usize>,
    searched: bool,
}

impl Finder {
    fn new(needle: &'static [u8]) -> Self {
        Self {
            needle,
            next: None,
            searched: false,
        }
    }

    fn find(&mut self, data: &[u8], from: usize) -> Option<usize> {
        if self.searched && self.next.is_none_or(|next| next >= from) {
            return self.next;
        }
        self.next = find(data, from, self.needle);
        self.searched = true;
        self.next
    }
}

/// First occurrence of `needle` at or after `from`.
pub(crate) fn find(data: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    data.get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| from + at)
}

/// True when `keyword` is at `at` and is not part of a longer token.
fn keyword_at(data: &[u8], at: usize, keyword: &[u8]) -> bool {
    let before = at.checked_sub(1).and_then(|b| data.get(b)).copied();
    data.get(at..).is_some_and(|rest| rest.starts_with(keyword))
        && !before.is_some_and(is_regular)
        && !data
            .get(at + keyword.len())
            .copied()
            .is_some_and(is_regular)
}

/// Turns a syntax error into "not found"; keeps the errors that must stop
/// the scan (limits, cancellation, input errors).
pub(crate) fn tolerate<T>(result: Result<T, Error>) -> Result<Option<T>, Error> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(Error::Malformed { .. } | Error::Truncated { .. }) => Ok(None),
        Err(other) => Err(other),
    }
}

/// Scans `data` once for object headers, stream data and trailers.
pub(crate) fn scan(data: &[u8], budget: &mut Budget) -> Result<Scan, Error> {
    let mut scan = Scan::default();
    let mut objs = Finder::new(b"obj");
    let mut streams = Finder::new(b"stream");
    let mut trailers = Finder::new(b"trailer");
    // The last header not yet followed by a stream: (end of `obj`, number,
    // offset of the header).
    let mut header: Option<(usize, u32, u64)> = None;
    // A trailer whose end is the next keyword found.
    let mut open_trailer = false;
    let mut pos = 0;
    loop {
        budget.check_cancelled()?;
        let candidates = [
            objs.find(data, pos).map(|at| (at, 0)),
            streams.find(data, pos).map(|at| (at, 1)),
            trailers.find(data, pos).map(|at| (at, 2)),
        ];
        let Some((at, kind)) = candidates.into_iter().flatten().min() else {
            break;
        };
        if std::mem::take(&mut open_trailer)
            && let Some(last) = scan.trailers.back_mut()
        {
            last.1 = at;
        }
        match kind {
            0 => {
                pos = at + 3;
                if !keyword_at(data, at, b"obj") {
                    continue;
                }
                if let Some((offset, number, generation)) = header_before(data, at) {
                    budget.charge_object()?;
                    let found = Found { offset, generation };
                    if scan.objects.insert(number, found).is_some() {
                        scan.conflicts += 1;
                    }
                    header = Some((pos, number, offset));
                }
            }
            1 => {
                pos = at + 6;
                if !keyword_at(data, at, b"stream") {
                    continue;
                }
                // `stream`, then (tolerated) spaces, then CRLF or LF.
                let spaces = data
                    .get(pos..)
                    .unwrap_or_default()
                    .iter()
                    .take_while(|b| matches!(b, b' ' | b'\t'))
                    .count();
                let eol = pos + spaces;
                let start = match data.get(eol..) {
                    Some([b'\r', b'\n', ..]) => eol + 2,
                    Some([b'\n' | b'\r', ..]) => eol + 1,
                    _ => continue,
                };
                pos = skip_stream(data, at, start, header.take(), &mut scan, budget)?;
            }
            _ => {
                pos = at + 7;
                if keyword_at(data, at, b"trailer") {
                    if scan.trailers.len() == MAX_TRAILERS {
                        scan.trailers.pop_front();
                    }
                    scan.trailers.push_back((at, data.len()));
                    open_trailer = true;
                }
            }
        }
    }
    Ok(scan)
}

/// `n g obj` ending at `at` (where `obj` starts): the header offset, number
/// and generation.
fn header_before(data: &[u8], at: usize) -> Option<(u64, u32, u16)> {
    let space = |end: usize| {
        let count = data
            .get(..end)?
            .iter()
            .rev()
            .take(MAX_HEADER_SPACE + 1)
            .take_while(|b| is_whitespace(**b))
            .count();
        (1..=MAX_HEADER_SPACE).contains(&count).then(|| end - count)
    };
    let digits = |end: usize, max: usize| {
        let count = data
            .get(..end)?
            .iter()
            .rev()
            .take(max + 1)
            .take_while(|b| b.is_ascii_digit())
            .count();
        if !(1..=max).contains(&count) {
            return None;
        }
        let start = end - count;
        let value = data.get(start..end)?.iter().try_fold(0_u64, |acc, d| {
            acc.checked_mul(10)?.checked_add(u64::from(d - b'0'))
        })?;
        Some((start, value))
    };
    let (start, generation) = digits(space(at)?, 5)?;
    let (start, number) = digits(space(start)?, 10)?;
    let before = start.checked_sub(1).and_then(|b| data.get(b)).copied();
    if before.is_some_and(is_regular) {
        return None;
    }
    let id = ObjectId::new(i64::try_from(number).ok()?, i64::try_from(generation).ok()?)?;
    Some((start as u64, id.number, id.generation))
}

/// Skips the data of the stream whose keyword is at `keyword` and whose data
/// starts at `start`; returns the position after its `endstream`, or the end
/// of the file. Records object streams and cross-reference stream
/// dictionaries.
fn skip_stream(
    data: &[u8],
    keyword: usize,
    start: usize,
    header: Option<(usize, u32, u64)>,
    scan: &mut Scan,
    budget: &mut Budget,
) -> Result<usize, Error> {
    let mut length = None;
    if let Some((after_obj, number, offset)) = header {
        // The dictionary lies between the header and `stream`: parsing is
        // limited to those bytes, which no other parse covers.
        let before = data.get(..keyword).unwrap_or_default();
        let mut parser = Parser::new(before);
        let parsed = tolerate(
            parser
                .seek(after_obj as u64)
                .and_then(|()| parser.parse_object(budget)),
        )?;
        // The header is this stream's only if nothing but white space lies
        // between its dictionary and `stream` (no `endobj` of an earlier
        // object, for instance).
        let adjacent = usize::try_from(parser.position())
            .ok()
            .and_then(|end| before.get(end..))
            .is_some_and(|gap| gap.iter().all(|b| is_whitespace(*b)));
        if let Some(Object::Dictionary(dictionary)) = parsed
            && adjacent
        {
            length = dictionary
                .get(b"Length")
                .and_then(Object::as_integer)
                .and_then(|l| usize::try_from(l).ok());
            match dictionary.get(b"Type").and_then(Object::as_name) {
                Some(b"ObjStm") => scan.streams.push((offset, number)),
                Some(b"XRef") => scan.xref_streams.push((offset, dictionary)),
                _ => {}
            }
        }
    }
    if let Some(end) = length.and_then(|l| start.checked_add(l)) {
        let after = end
            + data
                .get(end..)
                .unwrap_or_default()
                .iter()
                .take_while(|b| is_whitespace(**b))
                .count();
        if keyword_at(data, after, b"endstream") {
            return Ok(after + 9);
        }
    }
    match find(data, start, b"endstream") {
        Some(at) => Ok(at + 9),
        None => {
            scan.unterminated_streams += 1;
            Ok(data.len())
        }
    }
}

/// The trailer of the repaired file: the newest trailer or cross-reference
/// stream dictionary whose `/Root` is an object `exists` accepts. Refuses the
/// file if any trailer examined has `/Encrypt`.
pub(crate) fn choose_trailer(
    data: &[u8],
    scan: &Scan,
    exists: impl Fn(ObjectId) -> bool,
    budget: &mut Budget,
) -> Result<Option<Dictionary>, Error> {
    let mut dictionaries: Vec<(u64, Dictionary)> = scan.xref_streams.clone();
    for (at, end) in &scan.trailers {
        let mut parser = Parser::new(data.get(..*end).unwrap_or_default());
        let parsed = tolerate(
            parser
                .seek(*at as u64 + 7)
                .and_then(|()| parser.parse_object(budget)),
        )?;
        if let Some(Object::Dictionary(dictionary)) = parsed {
            dictionaries.push((*at as u64, dictionary));
        }
    }
    if dictionaries
        .iter()
        .any(|(_, dictionary)| dictionary.get(b"Encrypt").is_some())
    {
        return Err(Error::Unsupported(Capability::Encryption));
    }
    dictionaries.sort_by_key(|(offset, _)| std::cmp::Reverse(*offset));
    Ok(dictionaries
        .into_iter()
        .map(|(_, dictionary)| dictionary)
        .find(|dictionary| {
            dictionary
                .get(b"Root")
                .and_then(Object::as_reference)
                .is_some_and(&exists)
        }))
}

/// What the direct objects of the scan hold that matters to the repair.
#[derive(Debug, Default)]
pub(crate) struct DirectObjects {
    /// `/Type /Catalog` objects, as (offset, id), file order.
    pub(crate) catalogs: Vec<(u64, ObjectId)>,
    /// An encryption dictionary was found: the file must be refused.
    pub(crate) encrypted: bool,
}

/// Looks at each direct object (its last occurrence) within the bytes up to
/// the next header, so that the regions do not overlap. An object is parsed
/// only if its bytes mention `/Catalog`, or `/Filter` with `/O` and `/U` or
/// `/Recipients` (an encryption dictionary, ISO 32000-2, 7.6.2).
pub(crate) fn direct_objects(
    data: &[u8],
    scan: &Scan,
    budget: &mut Budget,
) -> Result<DirectObjects, Error> {
    let mut headers: Vec<(u64, ObjectId)> = scan
        .objects
        .iter()
        .map(|(number, found)| {
            (
                found.offset,
                ObjectId {
                    number: *number,
                    generation: found.generation,
                },
            )
        })
        .collect();
    headers.sort_by_key(|(offset, _)| *offset);
    let ends = headers
        .iter()
        .skip(1)
        .map(|(offset, _)| *offset)
        .chain([data.len() as u64]);
    let mut found = DirectObjects::default();
    for ((offset, id), end) in headers.iter().zip(ends) {
        budget.check_cancelled()?;
        let (Ok(start), Ok(end)) = (usize::try_from(*offset), usize::try_from(end)) else {
            continue;
        };
        let Some(region) = data.get(start..end) else {
            continue;
        };
        let mentions = |needle: &[u8]| find(region, 0, needle).is_some();
        let catalog = mentions(b"/Catalog");
        let encryption = mentions(b"/Filter")
            && ((mentions(b"/O") && mentions(b"/U")) || mentions(b"/Recipients"));
        if !catalog && !encryption {
            continue;
        }
        let mut parser = Parser::new(data.get(..end).unwrap_or_default());
        let mut refuse = |_: ObjectId, _: &mut Budget| {
            Err(Error::Malformed {
                offset: *offset,
                kind: MalformedKind::InvalidStreamLength,
            })
        };
        let parsed = tolerate(
            parser
                .seek(*offset)
                .and_then(|()| parser.parse_indirect_object(budget, &mut refuse)),
        )?;
        let Some(object) = parsed else {
            continue;
        };
        let IndirectValue::Object(Object::Dictionary(dictionary)) = object.value else {
            continue;
        };
        if is_catalog(&dictionary) {
            found.catalogs.push((*offset, *id));
        }
        if is_encryption(&dictionary) {
            found.encrypted = true;
        }
    }
    Ok(found)
}

pub(crate) fn is_catalog(dictionary: &Dictionary) -> bool {
    dictionary.get(b"Type").and_then(Object::as_name) == Some(b"Catalog")
}

/// A security handler dictionary: `/Filter` names the handler, and the
/// standard (`/O`, `/U`) or public-key (`/Recipients`) entries are present.
fn is_encryption(dictionary: &Dictionary) -> bool {
    dictionary
        .get(b"Filter")
        .and_then(Object::as_name)
        .is_some()
        && ((dictionary.get(b"O").is_some() && dictionary.get(b"U").is_some())
            || dictionary.get(b"Recipients").is_some())
}

/// Errors of the strict reading that a reconstruction may get past
/// (ADR-0004). Encryption, unsupported versions and limits never are.
pub(crate) fn repairable(error: &Error) -> bool {
    match error {
        Error::Truncated { .. } => true,
        Error::Malformed { kind, .. } => matches!(
            kind,
            MalformedKind::MissingStartxref
                | MalformedKind::MissingEof
                | MalformedKind::MissingXref
                | MalformedKind::InvalidXrefEntry
                | MalformedKind::InvalidXrefSubsection
                | MalformedKind::InvalidTrailer
                | MalformedKind::XrefPrevCycle
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
