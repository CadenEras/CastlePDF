//! PDF writing (ADR-0008, tranche 4.1): objects serialized as the reader
//! of `pdf-syntax` reads them back, and whole files with a classic
//! cross-reference table. No document logic here: what to write comes from
//! the caller (later tranches build it from a page plan).
//!
//! What is written reads back as the same object: names escape what is
//! not a regular character (7.3.5), literal strings escape `\`, `(`, `)`
//! and the line ends a reader would normalise (7.3.4.2), real numbers are
//! written in full without an exponent (7.3.3), with a decimal point so
//! that they stay reals.

mod document;
mod prune;
mod save;

use std::collections::BTreeMap;
use std::fmt;

use pdf_syntax::{Dictionary, Object, is_delimiter, is_regular, is_whitespace};

pub use document::{
    MAX_PLAN_PAGES, PlanPage, Rewritten, SourcePage, identity_plan, merge, rewrite,
};
pub use save::{SaveError, save_as};

/// What can stop a write: the input, never a panic.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// A real number that is not finite: PDF has no notation for it.
    NonFiniteNumber,
    /// A name holding the byte 0, which ISO 32000 forbids (7.3.5).
    NullInName,
    /// An object number of 0, or one given twice.
    InvalidObjectNumber(u32),
    /// The file would exceed `limit` bytes.
    TooLarge { limit: u64 },
    /// A page plan that is empty, names a page the document does not have,
    /// or turns a page by other than a multiple of 90 degrees.
    InvalidPlan,
    /// Reading the source failed: limits, cancellation, input.
    Syntax(pdf_syntax::Error),
}

impl From<pdf_syntax::Error> for Error {
    fn from(error: pdf_syntax::Error) -> Self {
        Self::Syntax(error)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteNumber => f.write_str("a real number is not finite"),
            Self::NullInName => f.write_str("a name holds the byte 0"),
            Self::InvalidObjectNumber(n) => write!(f, "object number {n} is 0 or given twice"),
            Self::TooLarge { limit } => write!(f, "the file would exceed {limit} bytes"),
            Self::InvalidPlan => {
                f.write_str("the page plan is empty or names pages the document lacks")
            }
            Self::Syntax(error) => write!(f, "reading the source failed: {error}"),
        }
    }
}

impl std::error::Error for Error {}

/// Appends `object` to `out`, as a direct object.
pub fn write_object(object: &Object, out: &mut Vec<u8>) -> Result<(), Error> {
    match object {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Boolean(true) => out.extend_from_slice(b"true"),
        Object::Boolean(false) => out.extend_from_slice(b"false"),
        Object::Integer(value) => out.extend_from_slice(value.to_string().as_bytes()),
        Object::Real(value) => write_real(*value, out)?,
        Object::String(bytes) => write_string(bytes, out),
        Object::Name(name) => write_name(name, out)?,
        Object::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b' ');
                }
                write_object(item, out)?;
            }
            out.push(b']');
        }
        Object::Dictionary(dictionary) => write_dictionary(dictionary, out)?,
        Object::Reference(id) => {
            out.extend_from_slice(format!("{} {} R", id.number, id.generation).as_bytes());
        }
    }
    Ok(())
}

fn write_dictionary(dictionary: &Dictionary, out: &mut Vec<u8>) -> Result<(), Error> {
    out.extend_from_slice(b"<<");
    for (key, value) in dictionary.iter() {
        out.push(b' ');
        write_name(key, out)?;
        out.push(b' ');
        write_object(value, out)?;
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

/// The shortest decimal that reads back as `value` (Rust's `Display` for
/// `f64`, which never uses an exponent), with a decimal point.
fn write_real(value: f64, out: &mut Vec<u8>) -> Result<(), Error> {
    if !value.is_finite() {
        return Err(Error::NonFiniteNumber);
    }
    let text = value.to_string();
    out.extend_from_slice(text.as_bytes());
    if !text.contains('.') {
        out.extend_from_slice(b".0");
    }
    Ok(())
}

/// A literal string; escapes the bytes a reader would not give back as
/// written: `\`, the parentheses, CR and LF (an unescaped CR reads as LF).
fn write_string(bytes: &[u8], out: &mut Vec<u8>) {
    out.push(b'(');
    for &byte in bytes {
        match byte {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'(' => out.extend_from_slice(b"\\("),
            b')' => out.extend_from_slice(b"\\)"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\n' => out.extend_from_slice(b"\\n"),
            other => out.push(other),
        }
    }
    out.push(b')');
}

/// A name: `#xx` for what is not a regular printable character, and for
/// `#` itself (7.3.5).
fn write_name(name: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
    out.push(b'/');
    for &byte in name {
        if byte == 0 {
            return Err(Error::NullInName);
        }
        let plain = (0x21..=0x7E).contains(&byte)
            && byte != b'#'
            && is_regular(byte)
            && !is_delimiter(byte)
            && !is_whitespace(byte);
        if plain {
            out.push(byte);
        } else {
            out.extend_from_slice(format!("#{byte:02X}").as_bytes());
        }
    }
    Ok(())
}

/// A PDF file being written: header, indirect objects in any order, then
/// a classic cross-reference table and the trailer (7.5). Object numbers
/// are the caller's; numbers not given are free entries. Generation
/// numbers are 0. The file never exceeds `limit` bytes.
pub struct FileWriter {
    out: Vec<u8>,
    offsets: BTreeMap<u32, u64>,
    limit: u64,
}

impl FileWriter {
    /// A file of PDF version `major.minor`, at most `limit` bytes. The
    /// header's second line holds four bytes above 127, so that tools
    /// treat the file as binary (7.5.2).
    pub fn new(major: u8, minor: u8, limit: u64) -> Result<Self, Error> {
        let mut writer = Self {
            out: Vec::new(),
            offsets: BTreeMap::new(),
            limit,
        };
        let header = format!("%PDF-{major}.{minor}\n");
        writer.append(header.as_bytes())?;
        writer.append(b"%\xE2\xE3\xCF\xD3\n")?;
        Ok(writer)
    }

    /// Bytes written so far.
    pub fn len(&self) -> u64 {
        self.out.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }

    fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let total = (self.out.len() as u64).saturating_add(bytes.len() as u64);
        if total > self.limit {
            return Err(Error::TooLarge { limit: self.limit });
        }
        self.out.extend_from_slice(bytes);
        Ok(())
    }

    fn start(&mut self, number: u32) -> Result<(), Error> {
        if number == 0 || self.offsets.contains_key(&number) {
            return Err(Error::InvalidObjectNumber(number));
        }
        self.offsets.insert(number, self.out.len() as u64);
        self.append(format!("{number} 0 obj\n").as_bytes())
    }

    /// Writes indirect object `number` holding `object`.
    pub fn object(&mut self, number: u32, object: &Object) -> Result<(), Error> {
        let mut body = Vec::new();
        write_object(object, &mut body)?;
        self.start(number)?;
        self.append(&body)?;
        self.append(b"\nendobj\n")
    }

    /// Writes indirect object `number` as a stream: `dictionary` with its
    /// `/Length` set to `data`'s, then `data` as given (still encoded by
    /// the filters the dictionary names).
    pub fn stream(
        &mut self,
        number: u32,
        dictionary: &Dictionary,
        data: &[u8],
    ) -> Result<(), Error> {
        let mut dictionary = dictionary.clone();
        let length =
            i64::try_from(data.len()).map_err(|_| Error::TooLarge { limit: self.limit })?;
        dictionary.insert(b"Length".to_vec(), Object::Integer(length));
        let mut head = Vec::new();
        write_dictionary(&dictionary, &mut head)?;
        self.start(number)?;
        self.append(&head)?;
        self.append(b"\nstream\n")?;
        self.append(data)?;
        self.append(b"\nendstream\nendobj\n")
    }

    /// Ends the file: the cross-reference table for every number up to the
    /// largest written, then `trailer` with `/Size` and `/ID` set (`/ID`
    /// from the bytes written, so that the same file gets the same
    /// identifier), `startxref` and `%%EOF`.
    pub fn finish(mut self, trailer: &Dictionary) -> Result<Vec<u8>, Error> {
        let size = match self.offsets.keys().next_back() {
            None => 1,
            Some(last) => last
                .checked_add(1)
                .ok_or(Error::InvalidObjectNumber(*last))?,
        };
        let id = file_id(&self.out);
        let xref = self.out.len() as u64;
        self.append(format!("xref\n0 {size}\n").as_bytes())?;
        // Free entry 0 heads the free list; other free entries point to 0
        // too, with generation 0 (7.5.4: next generation to use).
        self.append(b"0000000000 65535 f\r\n")?;
        for number in 1..size {
            let entry = match self.offsets.get(&number) {
                Some(offset) => format!("{offset:010} 00000 n\r\n"),
                None => "0000000000 00000 f\r\n".to_owned(),
            };
            self.append(entry.as_bytes())?;
        }
        let mut trailer = trailer.clone();
        trailer.insert(b"Size".to_vec(), Object::Integer(i64::from(size)));
        trailer.insert(
            b"ID".to_vec(),
            Object::Array(vec![
                Object::String(id.to_vec()),
                Object::String(id.to_vec()),
            ]),
        );
        let mut body = Vec::new();
        write_dictionary(&trailer, &mut body)?;
        self.append(b"trailer\n")?;
        self.append(&body)?;
        self.append(format!("\nstartxref\n{xref}\n%%EOF\n").as_bytes())?;
        Ok(self.out)
    }
}

/// Sixteen bytes derived from `bytes` (two 64-bit FNV-1a hashes of
/// different offsets): an identifier that is the same for the same file,
/// not a cryptographic digest.
fn file_id(bytes: &[u8]) -> [u8; 16] {
    let hash = |seed: u64| {
        bytes.iter().fold(seed, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01B3)
        })
    };
    let (a, b) = (hash(0xCBF2_9CE4_8422_2325), hash(0x84222325_CBF29CE4));
    let mut id = [0u8; 16];
    for (slot, byte) in id
        .iter_mut()
        .zip(a.to_be_bytes().into_iter().chain(b.to_be_bytes()))
    {
        *slot = byte;
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(object: &Object) -> String {
        let mut out = Vec::new();
        write_object(object, &mut out).expect("write");
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn numbers_are_written_as_they_read_back() {
        assert_eq!(text(&Object::Integer(i64::MIN)), "-9223372036854775808");
        assert_eq!(text(&Object::Real(2.0)), "2.0");
        assert_eq!(text(&Object::Real(0.1)), "0.1");
        assert_eq!(text(&Object::Real(-0.5)), "-0.5");
        assert_eq!(text(&Object::Real(1e20)), "100000000000000000000.0");
        assert_eq!(
            write_object(&Object::Real(f64::NAN), &mut Vec::new()),
            Err(Error::NonFiniteNumber)
        );
    }

    #[test]
    fn names_and_strings_are_escaped() {
        assert_eq!(text(&Object::Name(b"Type".to_vec())), "/Type");
        assert_eq!(
            text(&Object::Name(b"A B#(c)/\xE9".to_vec())),
            "/A#20B#23#28c#29#2F#E9"
        );
        assert_eq!(
            write_object(&Object::Name(b"a\0".to_vec()), &mut Vec::new()),
            Err(Error::NullInName)
        );
        assert_eq!(
            text(&Object::String(b"a(b)\\c\r\nd".to_vec())),
            "(a\\(b\\)\\\\c\\r\\nd)"
        );
    }
}
