//! The Unicode text of shown glyphs (ISO 32000-2, 9.10.2; ADR-0006, tranche
//! 3.3), in the order of the standard: the font's `/ToUnicode` CMap, then
//! the glyph name its encoding gives (simple and Type 3 fonts), through the
//! Adobe Glyph List, `uniXXXX`, `uXXXX` and `_` ligatures. A glyph with
//! neither is unknown: reported, never guessed.
//!
//! The `/ToUnicode` CMap is read on first use, when text is extracted: a
//! rendering never pays for it. One that cannot be read (damaged, not a
//! stream) counts as absent and is reported; limits and cancellation stop
//! the extraction.

use std::sync::{Arc, OnceLock};

use pdf_syntax::{Budget, Dictionary, Object, PdfFile, Source};

use super::cmap::{CMap, CMapError, Code, MAX_CODE_BYTES};
use super::font::unicode;
use super::stream_bytes;
use crate::Error;

/// Where the text of a glyph came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum UnicodeSource {
    /// The font's `/ToUnicode` CMap, for the code as shown.
    ToUnicode,
    /// The `/ToUnicode` CMap, for the same code value written with another
    /// length (`<0041>` for the one-byte code `41`): a producer error,
    /// tolerated as pdf.js and MuPDF do, and reported.
    ToUnicodeOtherLength,
    /// The glyph name of the code (`/Encoding`, `/Differences`).
    GlyphName,
    /// No source: the glyph's text is unknown.
    Unknown,
}

/// How the text of a font's codes is found.
#[derive(Debug, Default)]
pub(crate) struct TextMap {
    /// The `/ToUnicode` stream, read on first use. `null` and a name
    /// (`/Identity-H`, written by some producers) count as absent (7.3.9).
    to_unicode: Option<Object>,
    /// `Some(None)` once read and found unusable.
    parsed: OnceLock<Option<Arc<CMap>>>,
    /// Text of each one-byte code from its glyph name, for simple and Type 3
    /// fonts.
    by_code: Option<Box<[Option<Box<str>>]>>,
}

impl TextMap {
    /// The text map of the font dictionary `font`, with the glyph names of
    /// its 256 codes for a simple or Type 3 font.
    pub fn new(font: &Dictionary, names: Option<&[Option<Vec<u8>>]>) -> Self {
        Self {
            to_unicode: to_unicode(font),
            parsed: OnceLock::new(),
            by_code: names.map(|names| {
                names
                    .iter()
                    .map(|name| name.as_deref().and_then(name_text).map(Box::from))
                    .collect()
            }),
        }
    }

    /// Text of each one-byte code given directly (the dingbats of a
    /// `ZapfDingbats` replacement).
    pub fn with_texts(font: &Dictionary, texts: Vec<Option<Box<str>>>) -> Self {
        Self {
            to_unicode: to_unicode(font),
            parsed: OnceLock::new(),
            by_code: Some(texts.into_boxed_slice()),
        }
    }

    /// The `/ToUnicode` CMap, read once. `Ok(None)` without one or when it
    /// cannot be read; `unreadable` is set in the second case.
    fn to_unicode<S: Source>(
        &self,
        file: &PdfFile<S>,
        budget: &mut Budget,
        unreadable: &mut bool,
    ) -> Result<Option<&CMap>, Error> {
        if let Some(parsed) = self.parsed.get() {
            return Ok(parsed.as_deref());
        }
        let Some(object) = &self.to_unicode else {
            return Ok(None);
        };
        let parsed = match stream_bytes(file, object, budget)? {
            None => None,
            Some(data) => match CMap::parse(&data, budget) {
                Ok(cmap) => Some(Arc::new(cmap)),
                Err(CMapError::Syntax(
                    pdf_syntax::Error::Malformed { .. }
                    | pdf_syntax::Error::Truncated { .. }
                    | pdf_syntax::Error::Unsupported(_),
                ))
                | Err(CMapError::Unterminated { .. }) => None,
                Err(CMapError::Syntax(other)) => return Err(Error::Syntax(other)),
            },
        };
        if parsed.is_none() {
            *unreadable = true;
        }
        Ok(self.parsed.get_or_init(|| parsed).as_deref())
    }

    /// The text of `code` and where it came from. `unreadable` is set when
    /// the font's `/ToUnicode` could not be read (first use only).
    pub fn text<S: Source>(
        &self,
        file: &PdfFile<S>,
        code: Code,
        budget: &mut Budget,
        unreadable: &mut bool,
    ) -> Result<(Option<String>, UnicodeSource), Error> {
        if let Some(cmap) = self.to_unicode(file, budget, unreadable)? {
            if let Some(text) = cmap.unicode(code) {
                return Ok((Some(text), UnicodeSource::ToUnicode));
            }
            let other = (1..=MAX_CODE_BYTES)
                .filter_map(|len| u8::try_from(len).ok())
                .filter(|len| *len != code.len)
                .find_map(|len| {
                    cmap.unicode(Code {
                        value: code.value,
                        len,
                    })
                });
            if let Some(text) = other {
                return Ok((Some(text), UnicodeSource::ToUnicodeOtherLength));
            }
        }
        let by_name = (code.len == 1)
            .then_some(self.by_code.as_ref())
            .flatten()
            .and_then(|texts| texts.get(usize::try_from(code.value).ok()?))
            .and_then(|text| text.as_deref());
        Ok(match by_name {
            Some(text) => (Some(text.to_owned()), UnicodeSource::GlyphName),
            None => (None, UnicodeSource::Unknown),
        })
    }
}

/// The `/ToUnicode` entry of `font` if it may be a stream: a reference.
fn to_unicode(font: &Dictionary) -> Option<Object> {
    font.get(b"ToUnicode")
        .filter(|object| matches!(object, Object::Reference(_)))
        .cloned()
}

/// The text of a glyph name (Adobe Glyph List specification): its part
/// before any `.` suffix, split on `_` into components, each an AGL name,
/// `uniXXXX` (several units allowed) or `uXXXX[XX]`. `.notdef` and an
/// unknown component give `None`.
pub(crate) fn name_text(name: &[u8]) -> Option<String> {
    let name = std::str::from_utf8(name).ok()?;
    let base = name.split('.').next().unwrap_or(name);
    if base.is_empty() {
        return None;
    }
    let mut text = String::new();
    for component in base.split('_') {
        if let Some(digits) = component.strip_prefix("uni").filter(|d| {
            d.len() >= 8 && d.len().is_multiple_of(4) && d.chars().all(|c| c.is_ascii_hexdigit())
        }) {
            // Several UTF-16 units in one `uni` name, without surrogates.
            for chunk in digits.as_bytes().chunks(4) {
                let unit = u32::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
                if (0xD800..=0xDFFF).contains(&unit) {
                    return None;
                }
                text.push(char::from_u32(unit)?);
            }
            continue;
        }
        let code = unicode(component.as_bytes())?;
        if (0xD800..=0xDFFF).contains(&code) {
            return None;
        }
        text.push(char::from_u32(code)?);
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_names_to_text() {
        for (name, text) in [
            (&b"A"[..], Some("A")),
            (b"eacute", Some("é")),
            (b"fi", Some("\u{FB01}")),
            (b"f_i", Some("fi")),
            (b"uni00E9", Some("é")),
            (b"uni00660069", Some("fi")),
            (b"u1F600", Some("\u{1F600}")),
            (b"a.sc", Some("a")),
            (b"Euro", Some("€")),
            (b".notdef", None),
            (b"g123", None),
            (b"uniD800", None),
            (b"f_g123", None),
        ] {
            assert_eq!(name_text(name).as_deref(), text, "{name:?}");
        }
    }
}
