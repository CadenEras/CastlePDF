//! Object parser (ISO 32000-2:2020, 7.3 and 7.3.10).
//!
//! Strict (ADR-0002): any deviation is an error with its offset. Containers are
//! parsed with an explicit stack. `Object` is still a recursive type whose drop,
//! clone and comparison recurse, so the depth is capped at
//! [`MAX_SUPPORTED_NESTING_DEPTH`] whatever `max_nesting_depth` says.
//!
//! Every object node (dictionary keys included) is charged to the budget before
//! it is built; decoded strings and names are charged by the lexer. Container
//! vectors grow without `try_reserve`: their size is bounded by
//! `max_objects_visited` (a few dozen bytes per object).

use std::collections::VecDeque;

use crate::lexer::{Lexer, Spanned, Token, is_regular};
use crate::limits::{Budget, Capability, Error, LimitKind, MalformedKind};
use crate::object::{Dictionary, IndirectObject, IndirectValue, Object, ObjectId, Stream};

/// Hard cap on container nesting, above any configured `max_nesting_depth`, so
/// that recursive drop, clone and comparison of `Object` stay within the stack.
pub const MAX_SUPPORTED_NESTING_DEPTH: u32 = 256;

/// Resolves the value of an indirect `/Length` (`/Length 12 0 R`). Resolution
/// needs the cross-reference table, which this layer does not own.
///
/// Contract for implementations: the target must be a direct integer and a
/// stream target is refused. When the target is parsed with
/// `parse_indirect_object`, pass a resolver that refuses every call, so `/Length`
/// chains and cycles (`1 0 obj << /Length 1 0 R >> stream`) cannot recurse.
pub type LengthResolver<'r> = dyn FnMut(ObjectId, &mut Budget) -> Result<i64, Error> + 'r;

/// Parser over the bytes of a whole document.
///
/// ```
/// use pdf_syntax::{Budget, CancellationToken, IndirectValue, Object, Parser, ResourceLimits};
///
/// let mut budget = Budget::new(ResourceLimits::default(), CancellationToken::new());
/// let mut parser = Parser::new(b"3 0 obj << /Type /Page /Parent 2 0 R >> endobj");
/// let object = parser.parse_indirect_object(&mut budget, &mut |_, _| unreachable!())?;
/// let IndirectValue::Object(Object::Dictionary(page)) = object.value else { panic!() };
/// assert_eq!(page.get(b"Type"), Some(&Object::Name(b"Page".to_vec())));
/// # Ok::<(), pdf_syntax::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct Parser<'a> {
    data: &'a [u8],
    lexer: Lexer<'a>,
    /// Tokens read ahead to recognise `n g R`; at most two.
    lookahead: VecDeque<Spanned<'a>>,
    /// Accept, after `stream`, spaces and a lone CR as well (see
    /// `tolerating_stream_eol`).
    tolerant_stream_eol: bool,
}

enum Frame {
    Array {
        offset: u64,
        items: Vec<Object>,
    },
    Dictionary {
        offset: u64,
        entries: Dictionary,
        /// Key waiting for its value, with the key's offset.
        key: Option<(u64, Vec<u8>)>,
    },
}

impl Frame {
    fn offset(&self) -> u64 {
        match self {
            Self::Array { offset, .. } | Self::Dictionary { offset, .. } => *offset,
        }
    }
}

impl<'a> Parser<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            lexer: Lexer::new(data),
            lookahead: VecDeque::new(),
            tolerant_stream_eol: false,
        }
    }

    /// A parser that accepts, between `stream` and the data, spaces or tabs
    /// and then CRLF, LF or a lone CR, as the repair scan does (ADR-0004):
    /// for a file not read strictly. ISO 32000-2 (7.3.8.1) asks for CRLF or
    /// LF only; some producers (scanners) end the line with CR alone, which
    /// other readers accept. A CR followed by LF is always one end of line.
    #[must_use]
    pub fn tolerating_stream_eol(mut self) -> Self {
        self.tolerant_stream_eol = true;
        self
    }

    /// Offset of the next token not yet consumed, or of the next byte to read.
    pub fn position(&self) -> u64 {
        self.lookahead
            .front()
            .map_or_else(|| self.lexer.position(), |t| t.offset)
    }

    /// Moves to `offset`; an offset past the end of the input is refused.
    /// After an error, the position is unspecified: seek before parsing again.
    pub fn seek(&mut self, offset: u64) -> Result<(), Error> {
        self.lookahead.clear();
        self.lexer.seek(offset)
    }

    /// Parses one direct object at the current position.
    pub fn parse_object(&mut self, budget: &mut Budget) -> Result<Object, Error> {
        let start = self.position();
        let mut stack: Vec<Frame> = Vec::new();
        loop {
            let Some(Spanned { offset, token }) = self.next(budget)? else {
                let offset = stack.last().map_or(start, Frame::offset);
                return Err(Error::Truncated { offset });
            };
            let value = match token {
                Token::ArrayStart | Token::DictStart => {
                    // A container where a dictionary key is expected: refuse it
                    // before parsing (and charging) its content.
                    if let Some(Frame::Dictionary { key: None, .. }) = stack.last() {
                        return Err(malformed(offset, MalformedKind::DictionaryKeyNotName));
                    }
                    let depth = u32::try_from(stack.len().saturating_add(1)).unwrap_or(u32::MAX);
                    if depth > MAX_SUPPORTED_NESTING_DEPTH {
                        return Err(Error::LimitExceeded {
                            kind: LimitKind::NestingDepth,
                            limit: u64::from(MAX_SUPPORTED_NESTING_DEPTH),
                        });
                    }
                    budget.check_depth(depth)?;
                    budget.charge_object()?;
                    stack.push(if token == Token::ArrayStart {
                        Frame::Array {
                            offset,
                            items: Vec::new(),
                        }
                    } else {
                        Frame::Dictionary {
                            offset,
                            entries: Dictionary::new(),
                            key: None,
                        }
                    });
                    continue;
                }
                Token::ArrayEnd => match stack.pop() {
                    Some(Frame::Array { items, .. }) => Object::Array(items),
                    _ => return Err(malformed(offset, MalformedKind::UnexpectedToken)),
                },
                Token::DictEnd => match stack.pop() {
                    Some(Frame::Dictionary {
                        entries, key: None, ..
                    }) => Object::Dictionary(entries),
                    Some(Frame::Dictionary { key: Some(_), .. }) => {
                        return Err(malformed(offset, MalformedKind::MissingDictionaryValue));
                    }
                    _ => return Err(malformed(offset, MalformedKind::UnexpectedToken)),
                },
                scalar => {
                    budget.charge_object()?;
                    self.scalar(offset, scalar, budget)?
                }
            };
            match stack.last_mut() {
                None => return Ok(value),
                Some(Frame::Array { items, .. }) => items.push(value),
                Some(Frame::Dictionary { entries, key, .. }) => match key.take() {
                    None => match value {
                        Object::Name(name) => *key = Some((offset, name)),
                        _ => return Err(malformed(offset, MalformedKind::DictionaryKeyNotName)),
                    },
                    Some((key_offset, name)) => {
                        if entries.insert(name, value).is_some() {
                            return Err(malformed(key_offset, MalformedKind::DuplicateKey));
                        }
                    }
                },
            }
        }
    }

    /// Parses `n g obj <object> endobj` or `n g obj <dictionary> stream ...
    /// endstream endobj` at the current position.
    ///
    /// The stream data is located, not read: `/Length` bytes after the end-of-line
    /// that follows `stream`, which must be CRLF or LF, then an optional
    /// end-of-line and exactly `endstream`: bytes hidden by a short `/Length` are
    /// never skipped. An indirect `/Length` is passed to `resolve_length`. Data in
    /// an external file (`/F`) is refused as unsupported. The caller checks that
    /// the returned id matches the cross-reference entry that led here.
    pub fn parse_indirect_object(
        &mut self,
        budget: &mut Budget,
        resolve_length: &mut LengthResolver<'_>,
    ) -> Result<IndirectObject, Error> {
        let start = self.position();
        let number = self.expect_integer(start, budget)?;
        let generation = self.expect_integer(start, budget)?;
        let id = ObjectId::new(number, generation)
            .ok_or_else(|| malformed(start, MalformedKind::InvalidObjectId))?;
        self.expect_keyword(start, b"obj", MalformedKind::MissingObjKeyword, budget)?;
        let object = self.parse_object(budget)?;
        let Some(Spanned { offset, token }) = self.next(budget)? else {
            return Err(Error::Truncated { offset: start });
        };
        let value = match (token, object) {
            (Token::Keyword(b"endobj"), object) => {
                return Ok(IndirectObject {
                    id,
                    value: IndirectValue::Object(object),
                });
            }
            (Token::Keyword(b"stream"), Object::Dictionary(dictionary)) => {
                // `stream` is six bytes long; the data follows its end-of-line.
                let keyword_end = offset.saturating_add(6);
                let data = self.stream_data(keyword_end, &dictionary, budget, resolve_length)?;
                IndirectValue::Stream(Stream { dictionary, data })
            }
            _ => return Err(malformed(offset, MalformedKind::MissingEndobj)),
        };
        self.expect_keyword(start, b"endobj", MalformedKind::MissingEndobj, budget)?;
        Ok(IndirectObject { id, value })
    }

    fn stream_data(
        &mut self,
        keyword_end: u64,
        dictionary: &Dictionary,
        budget: &mut Budget,
        resolve_length: &mut LengthResolver<'_>,
    ) -> Result<std::ops::Range<u64>, Error> {
        let eol = usize::try_from(keyword_end).map_err(|_| Error::Truncated {
            offset: keyword_end,
        })?;
        // Spaces before the end of line, only when tolerated.
        let spaces = if self.tolerant_stream_eol {
            self.data
                .get(eol..)
                .unwrap_or_default()
                .iter()
                .take_while(|b| matches!(b, b' ' | b'\t'))
                .count()
        } else {
            0
        };
        let eol = eol + spaces;
        let keyword_end = keyword_end + spaces as u64;
        let data_start = match (self.data.get(eol), self.data.get(eol.wrapping_add(1))) {
            (Some(b'\n'), _) => keyword_end + 1,
            (Some(b'\r'), Some(b'\n')) => keyword_end + 2,
            (Some(b'\r'), Some(_)) if self.tolerant_stream_eol => keyword_end + 1,
            (None, _) | (Some(b'\r'), None) => {
                return Err(Error::Truncated {
                    offset: keyword_end,
                });
            }
            _ => return Err(malformed(keyword_end, MalformedKind::MissingStreamEol)),
        };
        if dictionary.get(b"F").is_some() {
            return Err(Error::Unsupported(Capability::ExternalStreamData));
        }
        let length = match dictionary.get(b"Length") {
            Some(Object::Integer(length)) => *length,
            Some(Object::Reference(id)) => resolve_length(*id, budget)?,
            _ => return Err(malformed(data_start, MalformedKind::InvalidStreamLength)),
        };
        let length = u64::try_from(length)
            .map_err(|_| malformed(data_start, MalformedKind::InvalidStreamLength))?;
        let data_end = data_start
            .checked_add(length)
            .filter(|end| *end <= self.data.len() as u64)
            .ok_or(Error::Truncated { offset: data_start })?;
        // Checked on raw bytes: the lexer would skip any white space or comment
        // and decode whatever follows a short `/Length`.
        const ENDSTREAM: &[u8] = b"endstream";
        let rest = usize::try_from(data_end)
            .ok()
            .and_then(|end| self.data.get(end..))
            .unwrap_or_default();
        let eol = match rest {
            [b'\r', b'\n', ..] => 2,
            [b'\n' | b'\r', ..] => 1,
            _ => 0,
        };
        let rest = rest.get(eol..).unwrap_or_default();
        if rest.starts_with(ENDSTREAM) && !rest.get(ENDSTREAM.len()).is_some_and(|b| is_regular(*b))
        {
            self.seek(data_end + eol as u64 + ENDSTREAM.len() as u64)?;
            Ok(data_start..data_end)
        } else if ENDSTREAM.starts_with(rest) {
            Err(Error::Truncated { offset: data_end })
        } else {
            Err(malformed(data_end, MalformedKind::MissingEndstream))
        }
    }

    fn scalar(
        &mut self,
        offset: u64,
        token: Token<'a>,
        budget: &mut Budget,
    ) -> Result<Object, Error> {
        Ok(match token {
            Token::Integer(number) => {
                if let Some(generation) = self.reference_generation(budget) {
                    let id = ObjectId::new(number, generation)
                        .ok_or_else(|| malformed(offset, MalformedKind::InvalidObjectId))?;
                    Object::Reference(id)
                } else {
                    Object::Integer(number)
                }
            }
            Token::Real(value) => Object::Real(value),
            Token::LiteralString(bytes) | Token::HexString(bytes) => Object::String(bytes),
            Token::Name(name) => Object::Name(name),
            Token::Keyword(b"true") => Object::Boolean(true),
            Token::Keyword(b"false") => Object::Boolean(false),
            Token::Keyword(b"null") => Object::Null,
            _ => return Err(malformed(offset, MalformedKind::UnexpectedToken)),
        })
    }

    /// After an integer, consumes `g R` and returns `g` if they follow.
    fn reference_generation(&mut self, budget: &mut Budget) -> Option<i64> {
        let Some(Token::Integer(generation)) = self.peek(0, budget).map(|t| &t.token) else {
            return None;
        };
        let generation = *generation;
        if !matches!(
            self.peek(1, budget).map(|t| &t.token),
            Some(Token::Keyword(b"R"))
        ) {
            return None;
        }
        self.lookahead.clear();
        Some(generation)
    }

    fn next(&mut self, budget: &mut Budget) -> Result<Option<Spanned<'a>>, Error> {
        match self.lookahead.pop_front() {
            Some(token) => Ok(Some(token)),
            None => self.lexer.next_token(budget),
        }
    }

    /// Reads ahead for `n g R`. A lexer error ends the look-ahead unreported: the
    /// lexer stays on the faulty token, so the error surfaces when that token is
    /// consumed, and a complete object before it (`5 )`) is still returned.
    fn peek(&mut self, index: usize, budget: &mut Budget) -> Option<&Spanned<'a>> {
        while self.lookahead.len() <= index {
            let token = self.lexer.next_token(budget).ok()??;
            self.lookahead.push_back(token);
        }
        self.lookahead.get(index)
    }

    fn expect_integer(&mut self, start: u64, budget: &mut Budget) -> Result<i64, Error> {
        match self.next(budget)? {
            Some(Spanned {
                token: Token::Integer(value),
                ..
            }) => Ok(value),
            Some(Spanned { offset, .. }) => Err(malformed(offset, MalformedKind::InvalidObjectId)),
            None => Err(Error::Truncated { offset: start }),
        }
    }

    fn expect_keyword(
        &mut self,
        start: u64,
        keyword: &[u8],
        kind: MalformedKind,
        budget: &mut Budget,
    ) -> Result<(), Error> {
        match self.next(budget)? {
            Some(Spanned {
                token: Token::Keyword(found),
                ..
            }) if found == keyword => Ok(()),
            Some(Spanned { offset, .. }) => Err(malformed(offset, kind)),
            None => Err(Error::Truncated { offset: start }),
        }
    }
}

fn malformed(offset: u64, kind: MalformedKind) -> Error {
    Error::Malformed { offset, kind }
}

#[cfg(test)]
mod tests;
