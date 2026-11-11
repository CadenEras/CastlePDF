//! Content stream syntax (ISO 32000-2, 7.8.2): operands followed by an
//! operator, with an inline image (8.9.7) read as one `BI` operation.

use std::ops::Range;

use pdf_syntax::{Budget, Dictionary, Lexer, Object, ObjectId, Parser, Token};

use crate::error::{ContentErrorKind, Error};

/// Most operands accepted before an operator. The largest standard operation,
/// `scn` with 32 colorants and a pattern name, takes 33.
pub const MAX_OPERANDS: usize = 64;

/// Most entries in an inline image dictionary.
const MAX_INLINE_IMAGE_ENTRIES: usize = 64;

/// One operation: its operator, where it starts, and its operands.
#[derive(Debug)]
pub(crate) struct Operation<'a> {
    pub operator: &'a [u8],
    /// Offset of the operator keyword in the decoded content.
    pub offset: u64,
    pub operands: &'a [Object],
    /// For `BI`: the image's dictionary, as written (abbreviated keys),
    /// and its encoded data.
    pub inline_image: Option<(&'a Dictionary, &'a [u8])>,
}

pub(crate) struct ContentReader<'a> {
    data: &'a [u8],
    lexer: Lexer<'a>,
    page: ObjectId,
    operands: Vec<Object>,
    /// The last inline image read: dictionary and data range.
    inline_image: (Dictionary, Range<usize>),
}

impl<'a> ContentReader<'a> {
    pub fn new(data: &'a [u8], page: ObjectId) -> Self {
        Self {
            data,
            lexer: Lexer::new(data),
            page,
            operands: Vec::new(),
            inline_image: (Dictionary::new(), 0..0),
        }
    }

    pub fn error(&self, offset: u64, kind: ContentErrorKind) -> Error {
        Error::Content {
            page: self.page,
            offset,
            kind,
        }
    }

    /// Converts token-level errors, whose offsets are content offsets.
    fn syntax(&self, error: pdf_syntax::Error) -> Error {
        match error {
            pdf_syntax::Error::Malformed { offset, kind } => {
                self.error(offset, ContentErrorKind::Syntax(kind))
            }
            pdf_syntax::Error::Truncated { offset } => {
                self.error(offset, ContentErrorKind::Truncated)
            }
            other => Error::Syntax(other),
        }
    }

    /// The next operation, or `None` at the end of the content. Operands left
    /// without an operator at the end are an error.
    pub fn next(&mut self, budget: &mut Budget) -> Result<Option<Operation<'_>>, Error> {
        self.operands.clear();
        let mut first_operand = 0;
        loop {
            let Some(spanned) = self.lexer.next_token(budget).map_err(|e| self.syntax(e))? else {
                if self.operands.is_empty() {
                    return Ok(None);
                }
                return Err(self.error(first_operand, ContentErrorKind::Truncated));
            };
            let offset = spanned.offset;
            let operand = match spanned.token {
                Token::Keyword(b"true") => Object::Boolean(true),
                Token::Keyword(b"false") => Object::Boolean(false),
                Token::Keyword(b"null") => Object::Null,
                Token::Keyword(b"BI") => {
                    if !self.operands.is_empty() {
                        return Err(self.error(offset, ContentErrorKind::InvalidOperands));
                    }
                    self.read_inline_image(offset, budget)?;
                    let (dictionary, range) = &self.inline_image;
                    let data = self.data.get(range.clone()).unwrap_or_default();
                    return Ok(Some(Operation {
                        operator: b"BI",
                        offset,
                        operands: &[],
                        inline_image: Some((dictionary, data)),
                    }));
                }
                Token::Keyword(operator) => {
                    return Ok(Some(Operation {
                        operator,
                        offset,
                        operands: &self.operands,
                        inline_image: None,
                    }));
                }
                Token::Integer(value) => Object::Integer(value),
                Token::Real(value) => Object::Real(value),
                Token::LiteralString(bytes) | Token::HexString(bytes) => Object::String(bytes),
                Token::Name(name) => Object::Name(name),
                Token::ArrayStart | Token::DictStart => self.container(offset, budget)?,
                Token::ArrayEnd | Token::DictEnd | Token::BraceStart | Token::BraceEnd => {
                    return Err(self.error(
                        offset,
                        ContentErrorKind::Syntax(pdf_syntax::MalformedKind::UnexpectedToken),
                    ));
                }
            };
            if self.operands.is_empty() {
                first_operand = offset;
            }
            if self.operands.len() >= MAX_OPERANDS {
                return Err(self.error(offset, ContentErrorKind::TooManyOperands));
            }
            self.operands.push(operand);
        }
    }

    /// Parses an array or dictionary starting at `offset` with the object
    /// parser (charged per node, depth bounded), then resumes after it.
    fn container(&mut self, offset: u64, budget: &mut Budget) -> Result<Object, Error> {
        let mut parser = Parser::new(self.data);
        parser.seek(offset).map_err(|e| self.syntax(e))?;
        let object = parser.parse_object(budget).map_err(|e| self.syntax(e))?;
        self.lexer
            .seek(parser.position())
            .map_err(|e| self.syntax(e))?;
        Ok(object)
    }

    /// Reads `BI <key value pairs> ID <data> EI` into `inline_image`. The
    /// data length comes from `/L` or `/Length` (PDF 2.0) when it is
    /// followed by `EI`; otherwise (no length, or a wrong one, as readers
    /// that ignore it accept) the data ends before the white space preceding
    /// the first `EI` followed by white space, a delimiter or the end of the
    /// content (the usual heuristic: binary data can contain such an `EI`).
    fn read_inline_image(&mut self, start: u64, budget: &mut Budget) -> Result<(), Error> {
        let invalid = |reader: &Self| reader.error(start, ContentErrorKind::InvalidInlineImage);
        let mut length = None;
        let mut entries = 0usize;
        let mut dictionary = Dictionary::new();
        loop {
            let Some(spanned) = self.lexer.next_token(budget).map_err(|e| self.syntax(e))? else {
                return Err(invalid(self));
            };
            match spanned.token {
                Token::Keyword(b"ID") => break,
                Token::Name(key) => {
                    entries += 1;
                    if entries > MAX_INLINE_IMAGE_ENTRIES {
                        return Err(invalid(self));
                    }
                    let value = self.inline_value(budget)?;
                    if key == b"L" || key == b"Length" {
                        length = value.as_integer().and_then(|v| usize::try_from(v).ok());
                    }
                    dictionary.insert(key, value);
                }
                _ => return Err(invalid(self)),
            }
        }
        // One white-space byte separates `ID` from the data. The keyword may
        // also end at a delimiter, which is then the first data byte.
        let after_id = usize::try_from(self.lexer.position()).map_err(|_| invalid(self))?;
        let data_start = match self.data.get(after_id) {
            Some(byte) if pdf_syntax::is_whitespace(*byte) => after_id + 1,
            Some(_) => after_id,
            None => return Err(invalid(self)),
        };
        if let Some(data_end) = length
            .and_then(|length| data_start.checked_add(length))
            .filter(|end| *end <= self.data.len())
        {
            self.lexer
                .seek(data_end as u64)
                .map_err(|e| self.syntax(e))?;
            if let Some(t) = self.lexer.next_token(budget).map_err(|e| self.syntax(e))?
                && t.token == Token::Keyword(b"EI")
            {
                self.inline_image = (dictionary, data_start..data_end);
                return Ok(());
            }
        }
        let end = find_end_marker(self.data, data_start).ok_or_else(|| invalid(self))?;
        // `EI` and the white space before it are not data; an image with no
        // data at all ends right after `ID `.
        let data_end = end.saturating_sub(3).max(data_start);
        self.inline_image = (dictionary, data_start..data_end);
        self.lexer.seek(end as u64).map_err(|e| self.syntax(e))
    }

    /// A value of an inline image dictionary: a scalar or a container.
    fn inline_value(&mut self, budget: &mut Budget) -> Result<Object, Error> {
        let Some(spanned) = self.lexer.next_token(budget).map_err(|e| self.syntax(e))? else {
            return Err(self.error(self.lexer.position(), ContentErrorKind::Truncated));
        };
        Ok(match spanned.token {
            Token::Integer(value) => Object::Integer(value),
            Token::Real(value) => Object::Real(value),
            Token::Name(name) => Object::Name(name),
            Token::LiteralString(bytes) | Token::HexString(bytes) => Object::String(bytes),
            Token::Keyword(b"true") => Object::Boolean(true),
            Token::Keyword(b"false") => Object::Boolean(false),
            Token::Keyword(b"null") => Object::Null,
            Token::ArrayStart | Token::DictStart => self.container(spanned.offset, budget)?,
            _ => {
                return Err(self.error(spanned.offset, ContentErrorKind::InvalidInlineImage));
            }
        })
    }
}

/// Offset just after the `EI` that ends inline image data starting at `from`.
fn find_end_marker(data: &[u8], from: usize) -> Option<usize> {
    let tail = data.get(from..)?;
    tail.windows(2).enumerate().find_map(|(i, pair)| {
        let at = from + i;
        let before = at.checked_sub(1).and_then(|b| data.get(b)).copied();
        let after = data.get(at + 2).copied();
        let ends =
            after.is_none_or(|b| pdf_syntax::is_whitespace(b) || pdf_syntax::is_delimiter(b));
        (pair == b"EI" && before.is_some_and(pdf_syntax::is_whitespace) && ends).then_some(at + 2)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_syntax::{CancellationToken, ResourceLimits};

    fn budget() -> Budget {
        Budget::new(ResourceLimits::default(), CancellationToken::new())
    }

    fn page() -> ObjectId {
        ObjectId::new(3, 0).unwrap_or_else(|| unreachable!())
    }

    fn operators(data: &[u8]) -> Result<Vec<(String, usize)>, Error> {
        let mut reader = ContentReader::new(data, page());
        let mut budget = budget();
        let mut out = Vec::new();
        while let Some(op) = reader.next(&mut budget)? {
            out.push((
                String::from_utf8_lossy(op.operator).into_owned(),
                op.operands.len(),
            ));
        }
        Ok(out)
    }

    fn kind(data: &[u8]) -> Option<(u64, ContentErrorKind)> {
        match operators(data) {
            Err(Error::Content { offset, kind, .. }) => Some((offset, kind)),
            _ => None,
        }
    }

    #[test]
    fn reads_operations_and_operands() -> Result<(), Error> {
        let ops = operators(b"q 1 0 0 1 10 20 cm\n[3 2] 0 d /GS1 gs <</MCID 0>> BDC EMC Q")?;
        let names: Vec<_> = ops.iter().map(|(o, n)| format!("{o}/{n}")).collect();
        assert_eq!(
            names,
            ["q/0", "cm/6", "d/2", "gs/1", "BDC/1", "EMC/0", "Q/0"]
        );
        assert_eq!(operators(b"")?, []);
        assert_eq!(operators(b" % comment only\n")?, []);
        Ok(())
    }

    /// Dictionary keys and data of an inline image.
    type Inline = (Vec<String>, Vec<u8>);

    /// The dictionary keys and data of each inline image of `data`.
    fn inline_images(data: &[u8]) -> Result<Vec<Inline>, Error> {
        let mut reader = ContentReader::new(data, page());
        let mut budget = budget();
        let mut out = Vec::new();
        while let Some(op) = reader.next(&mut budget)? {
            if let Some((dictionary, bytes)) = op.inline_image {
                let keys = dictionary
                    .iter()
                    .map(|(key, _)| String::from_utf8_lossy(key).into_owned())
                    .collect();
                out.push((keys, bytes.to_vec()));
            }
        }
        Ok(out)
    }

    #[test]
    fn reads_inline_images() -> Result<(), Error> {
        let ops = operators(b"q BI /W 2 /H 1 /CS /G /BPC 8 ID \x00EI\xff EI Q")?;
        assert_eq!(
            ops.iter().map(|(o, _)| o.as_str()).collect::<Vec<_>>(),
            ["q", "BI", "Q"]
        );
        // Without /L: data up to the white space before the delimited `EI`.
        let images = inline_images(b"q BI /W 2 /H 1 /CS /G /BPC 8 ID \x00EI\xff EI Q")?;
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].0.len(), 4);
        assert_eq!(images[0].1, b"\x00EI\xff");
        // PDF 2.0 length: the data may contain a delimited `EI`.
        let ops = operators(b"BI /W 4 /H 1 /L 4 ID  EI EI Q")?;
        assert_eq!(
            ops.iter().map(|(o, _)| o.as_str()).collect::<Vec<_>>(),
            ["BI", "Q"]
        );
        assert_eq!(
            inline_images(b"BI /W 4 /H 1 /L 4 ID  EI EI Q")?[0].1,
            b" EI "
        );
        let ops = operators(b"BI /W 1 /D [1 0] ID x EI")?;
        assert_eq!(ops.len(), 1);
        // No data at all.
        assert_eq!(inline_images(b"BI /W 1 ID EI")?[0].1, b"");
        // A wrong /L (too long, negative, not a number, not followed by
        // `EI`) is ignored, as by readers that do not know it.
        for content in [
            &b"BI /L 9 ID x EI Q"[..],
            b"BI /L -1 ID x EI Q",
            b"BI /L /A ID x EI Q",
            b"BI /L 0 ID x EI Q",
        ] {
            let images = inline_images(content)?;
            assert_eq!(images[0].1, b"x", "{}", String::from_utf8_lossy(content));
            assert_eq!(operators(content)?.len(), 2);
        }
        // `ID` ended by a delimiter: no separator byte to skip.
        assert_eq!(inline_images(b"BI /W 1 ID<ab> EI")?[0].1, b"<ab>");
        // CR LF after `ID`: only the CR separates.
        assert_eq!(inline_images(b"BI /W 1 ID\r\nx EI")?[0].1, b"\nx");
        // Each image has its own dictionary and data.
        let images = inline_images(b"BI /W 1 ID a EI BI /H 2 /W 1 ID bc EI")?;
        assert_eq!(images[0], (vec!["W".to_owned()], b"a".to_vec()));
        assert_eq!(images[1].1, b"bc");
        assert_eq!(images[1].0.len(), 2);
        Ok(())
    }

    #[test]
    fn rejects_invalid_content() {
        assert_eq!(kind(b"1 2"), Some((0, ContentErrorKind::Truncated)));
        assert_eq!(kind(b"q 1 2"), Some((2, ContentErrorKind::Truncated)));
        assert_eq!(
            kind(b"q ] Q"),
            Some((
                2,
                ContentErrorKind::Syntax(pdf_syntax::MalformedKind::UnexpectedToken)
            ))
        );
        assert_eq!(
            kind(b"1 BI ID x EI"),
            Some((2, ContentErrorKind::InvalidOperands))
        );
        assert_eq!(
            kind(b"BI /W 1 ID xEI"),
            Some((0, ContentErrorKind::InvalidInlineImage))
        );
        assert_eq!(
            kind(b"BI /W 1"),
            Some((0, ContentErrorKind::InvalidInlineImage))
        );
        assert_eq!(
            kind(b"BI 1 ID x EI"),
            Some((0, ContentErrorKind::InvalidInlineImage))
        );
        // A wrong /L falls back to the `EI` heuristic, which can still fail.
        assert_eq!(
            kind(b"BI /L 9 ID xEI"),
            Some((0, ContentErrorKind::InvalidInlineImage))
        );
        let many = "0 ".repeat(MAX_OPERANDS + 1) + "m";
        assert_eq!(
            kind(many.as_bytes()),
            Some((2 * MAX_OPERANDS as u64, ContentErrorKind::TooManyOperands))
        );
        let exact = "0 ".repeat(MAX_OPERANDS) + "m";
        assert!(operators(exact.as_bytes()).is_ok());
        assert_eq!(kind(b"[1 2"), Some((0, ContentErrorKind::Truncated)));
    }

    #[test]
    fn cancellation_stops_reading() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut budget = Budget::new(ResourceLimits::default(), cancel);
        let mut reader = ContentReader::new(b"q Q", page());
        assert!(matches!(
            reader.next(&mut budget),
            Err(Error::Syntax(pdf_syntax::Error::Cancelled))
        ));
    }
}
