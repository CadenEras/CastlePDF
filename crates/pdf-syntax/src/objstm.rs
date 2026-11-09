//! Object streams (ISO 32000-2:2020, 7.5.7): decoded once, parsed on demand.
//!
//! The header (`/N` pairs of object number and offset, before `/First`) is
//! validated when the stream is decoded. Decoded streams are cached per
//! `PdfFile` (at most 8 streams and 32 MiB, outside any task budget): the
//! budget of the task that decodes a stream is charged once; a later task
//! that finds it in the cache is not charged again. Errors inside the decoded data carry
//! the file offset where the encoded stream data starts, with the precise
//! kind: decoded bytes have no file offset of their own.

use std::sync::Arc;

use crate::lexer::{Lexer, Spanned, Token};
use crate::limits::{Budget, Error, MalformedKind};
use crate::object::{Dictionary, Object};
use crate::parser::Parser;

/// At most this many decoded object streams are cached...
const CACHE_STREAMS: usize = 8;
/// ...holding at most this many decoded bytes in total.
const CACHE_BYTES: usize = 32 * 1024 * 1024;

/// A decoded object stream and its validated header.
#[derive(Debug)]
pub(crate) struct ObjectStream {
    data: Vec<u8>,
    /// `/First`: where the first object starts in `data`.
    first: usize,
    /// (object number, offset relative to `first`), in increasing offsets.
    header: Vec<(u32, usize)>,
    /// File offset of the encoded data, to locate errors.
    data_offset: u64,
}

impl ObjectStream {
    /// Validates `/N`, `/First` and the header of decoded `data`.
    pub(crate) fn parse(
        dictionary: &Dictionary,
        data: Vec<u8>,
        data_offset: u64,
        budget: &mut Budget,
    ) -> Result<Self, Error> {
        let invalid = || Error::Malformed {
            offset: data_offset,
            kind: MalformedKind::InvalidObjectStream,
        };
        let count = dictionary
            .get(b"N")
            .and_then(Object::as_integer)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(invalid)?;
        let first = dictionary
            .get(b"First")
            .and_then(Object::as_integer)
            .and_then(|f| usize::try_from(f).ok())
            .filter(|f| *f <= data.len())
            .ok_or_else(invalid)?;
        // Each header pair takes at least 4 bytes ("1 0 ", the last one 3):
        // a /N larger than the header can hold is refused, and the header
        // vector grows with the pairs actually read and charged.
        if count.saturating_mul(4) > first.saturating_add(1) {
            return Err(invalid());
        }
        let header_bytes = data.get(..first).ok_or_else(invalid)?;
        let mut lexer = Lexer::new(header_bytes);
        let mut header = Vec::new();
        header
            .try_reserve_exact(count.min(1024))
            .map_err(|_| invalid())?;
        let mut previous: Option<usize> = None;
        for i in 0..count {
            if i % 1024 == 0 {
                budget.check_cancelled()?;
            }
            budget.charge_object()?;
            let mut integer = || match lexer.next_token(budget) {
                Ok(Some(Spanned {
                    token: Token::Integer(value),
                    ..
                })) => Ok(value),
                _ => Err(invalid()),
            };
            let number = u32::try_from(integer()?)
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(invalid)?;
            let offset = usize::try_from(integer()?)
                .ok()
                .filter(|o| first.checked_add(*o).is_some_and(|at| at < data.len()))
                .filter(|o| previous.is_none_or(|p| *o > p))
                .ok_or_else(invalid)?;
            previous = Some(offset);
            header.push((number, offset));
        }
        Ok(Self {
            data,
            first,
            header,
            data_offset,
        })
    }

    /// Object `index`, which must be listed as object `number`.
    pub(crate) fn object(
        &self,
        number: u32,
        index: u32,
        budget: &mut Budget,
    ) -> Result<Object, Error> {
        let located = |kind| Error::Malformed {
            offset: self.data_offset,
            kind,
        };
        let (listed, offset) = usize::try_from(index)
            .ok()
            .and_then(|i| self.header.get(i))
            .copied()
            .ok_or(located(MalformedKind::InvalidObjectStream))?;
        if listed != number {
            return Err(located(MalformedKind::ObjectIdMismatch));
        }
        let relocate = |e| match e {
            Error::Malformed { kind, .. } => located(kind),
            Error::Truncated { .. } => Error::Truncated {
                offset: self.data_offset,
            },
            other => other,
        };
        let mut parser = Parser::new(&self.data);
        parser.seek((self.first + offset) as u64)?;
        let object = parser.parse_object(budget).map_err(relocate)?;
        // An object stream never holds a stream (ISO 32000-2, 7.5.7).
        let mut lexer = Lexer::new(&self.data);
        lexer.seek(parser.position())?;
        if let Ok(Some(Spanned {
            token: Token::Keyword(b"stream"),
            ..
        })) = lexer.next_token(budget)
        {
            return Err(located(MalformedKind::InvalidObjectStream));
        }
        Ok(object)
    }

    /// Object numbers listed in the header, with their index.
    pub(crate) fn numbers(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.header
            .iter()
            .zip(0_u32..)
            .map(|((number, _), index)| (*number, index))
    }

    /// True when the bytes of object `index` (up to the next object) contain
    /// `needle`: a cheap filter before parsing it.
    pub(crate) fn mentions(&self, index: u32, needle: &[u8]) -> bool {
        let Ok(index) = usize::try_from(index) else {
            return false;
        };
        let start = self
            .header
            .get(index)
            .map(|(_, offset)| self.first + offset);
        let end = self
            .header
            .get(index + 1)
            .map_or(self.data.len(), |(_, offset)| self.first + offset);
        start
            .and_then(|start| self.data.get(start..end))
            .is_some_and(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
    }

    fn size(&self) -> usize {
        self.data.len()
    }
}

/// Recently used decoded object streams, bounded in count and bytes.
#[derive(Debug, Default)]
pub(crate) struct ObjectStreamCache {
    /// Least recently used first.
    entries: Vec<(u32, Arc<ObjectStream>)>,
    bytes: usize,
}

impl ObjectStreamCache {
    pub(crate) fn get(&mut self, number: u32) -> Option<Arc<ObjectStream>> {
        let position = self.entries.iter().position(|(n, _)| *n == number)?;
        let entry = self.entries.remove(position);
        let found = Arc::clone(&entry.1);
        self.entries.push(entry);
        Some(found)
    }

    /// Keeps `stream` unless it alone exceeds the byte bound; evicts the
    /// least recently used streams to stay within both bounds.
    pub(crate) fn insert(&mut self, number: u32, stream: Arc<ObjectStream>) {
        if stream.size() > CACHE_BYTES || self.entries.iter().any(|(n, _)| *n == number) {
            return;
        }
        while !self.entries.is_empty()
            && (self.entries.len() >= CACHE_STREAMS || self.bytes + stream.size() > CACHE_BYTES)
        {
            let (_, evicted) = self.entries.remove(0);
            self.bytes -= evicted.size();
        }
        self.bytes += stream.size();
        self.entries.push((number, stream));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::{CancellationToken, ResourceLimits};

    fn budget() -> Budget {
        Budget::new(ResourceLimits::default(), CancellationToken::new())
    }

    fn header(n: i64, first: i64) -> Dictionary {
        let mut d = Dictionary::new();
        d.insert(b"N".to_vec(), Object::Integer(n));
        d.insert(b"First".to_vec(), Object::Integer(first));
        d
    }

    fn invalid() -> Result<ObjectStream, Error> {
        Err(Error::Malformed {
            offset: 7,
            kind: MalformedKind::InvalidObjectStream,
        })
    }

    #[test]
    fn objects_are_read_by_index_and_number() {
        let data = b"10 0 11 4 (ab) [1 2]".to_vec();
        let s = ObjectStream::parse(&header(2, 10), data, 7, &mut budget()).expect("valid");
        assert_eq!(
            s.object(10, 0, &mut budget()),
            Ok(Object::String(b"ab".to_vec()))
        );
        assert_eq!(
            s.object(11, 1, &mut budget()),
            Ok(Object::Array(vec![Object::Integer(1), Object::Integer(2)]))
        );
        let at = |kind| Err(Error::Malformed { offset: 7, kind });
        assert_eq!(
            s.object(12, 1, &mut budget()),
            at(MalformedKind::ObjectIdMismatch)
        );
        assert_eq!(
            s.object(10, 2, &mut budget()),
            at(MalformedKind::InvalidObjectStream)
        );
    }

    #[test]
    fn invalid_headers_are_refused() {
        let parse = |n, first, data: &[u8]| {
            ObjectStream::parse(&header(n, first), data.to_vec(), 7, &mut budget()).map(|_| ())
        };
        let bad = invalid().map(|_| ());
        // /First beyond the data, /N larger than the header can hold.
        assert_eq!(parse(1, 50, b"1 0 null"), bad);
        assert_eq!(parse(1_000_000, 4, b"1 0 null"), bad);
        // Exactly as many pairs as the header bytes can hold, then one more.
        assert_eq!(parse(2, 7, b"1 0 2 1 xy"), Ok(()));
        assert_eq!(parse(3, 7, b"1 0 2 1 xy"), bad);
        // Offsets out of range, decreasing, or object number 0.
        assert_eq!(parse(1, 4, b"1 9 null"), bad);
        assert_eq!(parse(2, 8, b"1 2 2 0 nullnull"), bad);
        assert_eq!(parse(1, 4, b"0 0 null"), bad);
        // Not integers.
        assert_eq!(parse(1, 4, b"/A 0 null"), bad);
        assert_eq!(parse(-1, 4, b"1 0 null"), bad);
        assert_eq!(parse(1, 4, b"1 0 null"), Ok(()));
    }

    #[test]
    fn a_stream_inside_an_object_stream_is_refused() {
        let s = ObjectStream::parse(
            &header(1, 4),
            b"1 0 << /Length 0 >> stream

endstream"
                .to_vec(),
            7,
            &mut budget(),
        )
        .expect("valid header");
        assert_eq!(
            s.object(1, 0, &mut budget()),
            Err(Error::Malformed {
                offset: 7,
                kind: MalformedKind::InvalidObjectStream
            })
        );
    }

    #[test]
    fn errors_inside_the_data_point_at_the_stream() {
        let s = ObjectStream::parse(&header(1, 4), b"1 0 [1".to_vec(), 7, &mut budget())
            .expect("valid header");
        assert_eq!(
            s.object(1, 0, &mut budget()),
            Err(Error::Truncated { offset: 7 })
        );
    }

    #[test]
    fn the_cache_is_bounded() {
        let stream = |len: usize| {
            Arc::new(ObjectStream {
                data: vec![0; len],
                first: 0,
                header: Vec::new(),
                data_offset: 0,
            })
        };
        let mut cache = ObjectStreamCache::default();
        for n in 1..=10 {
            cache.insert(n, stream(10));
        }
        assert_eq!(cache.entries.len(), CACHE_STREAMS);
        assert!(cache.get(1).is_none() && cache.get(10).is_some());
        // Byte bound: a large stream evicts older ones; a too large one is not kept.
        cache.insert(20, stream(CACHE_BYTES - 5));
        assert!(cache.bytes <= CACHE_BYTES);
        cache.insert(21, stream(CACHE_BYTES + 1));
        assert!(cache.get(21).is_none());
    }
}
