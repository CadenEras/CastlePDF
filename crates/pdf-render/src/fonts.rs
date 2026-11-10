//! Kinds of the fonts a page declares (ISO 32000-2, 9.6 to 9.9), to measure
//! which font programs the renderer must read (ADR-0005, step 5.1). Nothing
//! here reads a font program: only font dictionaries and descriptors.

use std::collections::BTreeSet;

use pdf_document::Page;
use pdf_syntax::{Budget, Dictionary, IndirectValue, Object, PdfFile, Source};

use crate::Error;

/// How a font is given: its type and where its glyphs come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum FontKind {
    /// Simple font with an embedded Type 1 program (`/FontFile`), which
    /// `skrifa` does not read.
    Type1,
    /// Simple font with an embedded bare CFF program (`/FontFile3 /Type1C`).
    Type1C,
    /// Simple font with an embedded TrueType program (`/FontFile2`).
    TrueType,
    /// Simple or CID font with an embedded OpenType program
    /// (`/FontFile3 /OpenType`).
    OpenType,
    /// One of the 14 standard fonts, not embedded.
    Standard14,
    /// Any other simple font without an embedded program.
    NotEmbedded,
    /// Type 0 font whose CID font has a bare CFF program
    /// (`/FontFile3 /CIDFontType0C`).
    CidType0C,
    /// Type 0 font whose CID font has a TrueType program (`/FontFile2`).
    CidTrueType,
    /// Type 0 font whose CID font has no embedded program.
    CidNotEmbedded,
    /// Type 3 font: glyphs are content procedures.
    Type3,
    /// A font dictionary that cannot be classified: wrong type, missing
    /// entries, or an object that does not resolve.
    Invalid,
}

/// The 14 standard fonts (ISO 32000-2, 9.6.2.2).
const STANDARD_14: [&[u8]; 14] = [
    b"Times-Roman",
    b"Times-Bold",
    b"Times-Italic",
    b"Times-BoldItalic",
    b"Helvetica",
    b"Helvetica-Bold",
    b"Helvetica-Oblique",
    b"Helvetica-BoldOblique",
    b"Courier",
    b"Courier-Bold",
    b"Courier-Oblique",
    b"Courier-BoldOblique",
    b"Symbol",
    b"ZapfDingbats",
];

/// Most fonts examined on one page; the others are not classified.
pub const MAX_PAGE_FONTS: usize = 1024;

/// Kinds of the fonts named in the page's `/Resources /Font` dictionary.
/// Fonts used only inside form XObjects or patterns are not listed. A
/// font that does not resolve is `Invalid`; limits and cancellation are
/// errors.
pub fn page_font_kinds<S: Source>(
    file: &PdfFile<S>,
    page: &Page,
    budget: &mut Budget,
) -> Result<BTreeSet<FontKind>, Error> {
    let mut kinds = BTreeSet::new();
    let Some(resources) = page.resources.as_deref() else {
        return Ok(kinds);
    };
    let Some(resources) = dictionary(file, resources, budget)? else {
        return Ok(kinds);
    };
    let Some(fonts) = resources.get(b"Font") else {
        return Ok(kinds);
    };
    let Some(fonts) = dictionary(file, fonts, budget)? else {
        kinds.insert(FontKind::Invalid);
        return Ok(kinds);
    };
    for (_, font) in fonts.iter().take(MAX_PAGE_FONTS) {
        budget.check_cancelled()?;
        kinds.insert(font_kind(file, font, budget)?);
    }
    Ok(kinds)
}

fn font_kind<S: Source>(
    file: &PdfFile<S>,
    font: &Object,
    budget: &mut Budget,
) -> Result<FontKind, Error> {
    let Some(font) = dictionary(file, font, budget)? else {
        return Ok(FontKind::Invalid);
    };
    let subtype = font.get(b"Subtype").and_then(Object::as_name);
    match subtype {
        Some(b"Type3") => Ok(FontKind::Type3),
        Some(b"Type0") => cid_kind(file, &font, budget),
        Some(b"Type1" | b"MMType1" | b"TrueType") => {
            let program = match font.get(b"FontDescriptor") {
                Some(descriptor) => match dictionary(file, descriptor, budget)? {
                    Some(descriptor) => embedded(file, &descriptor, budget)?,
                    None => return Ok(FontKind::Invalid),
                },
                None => None,
            };
            Ok(program.unwrap_or_else(|| {
                let name = font.get(b"BaseFont").and_then(Object::as_name);
                if name.is_some_and(is_standard_14) {
                    FontKind::Standard14
                } else {
                    FontKind::NotEmbedded
                }
            }))
        }
        _ => Ok(FontKind::Invalid),
    }
}

/// A Type 0 font: its single descendant CID font decides the kind.
fn cid_kind<S: Source>(
    file: &PdfFile<S>,
    font: &Dictionary,
    budget: &mut Budget,
) -> Result<FontKind, Error> {
    let descendants = match font.get(b"DescendantFonts") {
        Some(Object::Reference(id)) => match file.resolve(*id, budget) {
            Ok(IndirectValue::Object(object)) => object,
            Ok(IndirectValue::Stream(_)) => return Ok(FontKind::Invalid),
            Err(error) => return tolerate(error),
        },
        Some(other) => other.clone(),
        None => return Ok(FontKind::Invalid),
    };
    let Some([descendant]) = descendants.as_array() else {
        return Ok(FontKind::Invalid);
    };
    let Some(descendant) = dictionary(file, descendant, budget)? else {
        return Ok(FontKind::Invalid);
    };
    let Some(descriptor) = descendant.get(b"FontDescriptor") else {
        return Ok(FontKind::Invalid);
    };
    let Some(descriptor) = dictionary(file, descriptor, budget)? else {
        return Ok(FontKind::Invalid);
    };
    Ok(match embedded(file, &descriptor, budget)? {
        None => FontKind::CidNotEmbedded,
        Some(FontKind::TrueType) => FontKind::CidTrueType,
        Some(FontKind::Type1C) => FontKind::CidType0C,
        Some(other) => other,
    })
}

/// The program a font descriptor embeds, if any. `/FontFile3` is told
/// apart by the `/Subtype` of its stream dictionary (`Type1C`,
/// `CIDFontType0C` or `OpenType`); the stream data is never read here.
fn embedded<S: Source>(
    file: &PdfFile<S>,
    descriptor: &Dictionary,
    budget: &mut Budget,
) -> Result<Option<FontKind>, Error> {
    if descriptor.get(b"FontFile").is_some() {
        return Ok(Some(FontKind::Type1));
    }
    if descriptor.get(b"FontFile2").is_some() {
        return Ok(Some(FontKind::TrueType));
    }
    let Some(program) = descriptor.get(b"FontFile3") else {
        return Ok(None);
    };
    let Some(stream) = dictionary(file, program, budget)? else {
        return Ok(Some(FontKind::Invalid));
    };
    Ok(Some(
        match stream.get(b"Subtype").and_then(Object::as_name) {
            Some(b"Type1C" | b"CIDFontType0C") => FontKind::Type1C,
            Some(b"OpenType") => FontKind::OpenType,
            _ => FontKind::Invalid,
        },
    ))
}

/// `name` without a subset prefix (`ABCDEF+`) is a standard font name.
fn is_standard_14(name: &[u8]) -> bool {
    let base = match name.get(6) {
        Some(b'+') if name.iter().take(6).all(u8::is_ascii_uppercase) => {
            name.get(7..).unwrap_or_default()
        }
        _ => name,
    };
    STANDARD_14.contains(&base)
}

/// `object` resolved once to a dictionary (the dictionary of a stream, its
/// data unread); `None` if it is something else or does not resolve because
/// the file is damaged there.
fn dictionary<S: Source>(
    file: &PdfFile<S>,
    object: &Object,
    budget: &mut Budget,
) -> Result<Option<Dictionary>, Error> {
    let resolved = match object {
        Object::Reference(id) => match file.resolve(*id, budget) {
            Ok(IndirectValue::Object(object)) => object,
            Ok(IndirectValue::Stream(stream)) => Object::Dictionary(stream.dictionary),
            Err(error) => return tolerate(error).map(|_| None),
        },
        other => other.clone(),
    };
    Ok(match resolved {
        Object::Dictionary(dictionary) => Some(dictionary),
        _ => None,
    })
}

/// A syntax error in the file makes the font `Invalid`; limits,
/// cancellation and input errors stop the classification.
fn tolerate(error: pdf_syntax::Error) -> Result<FontKind, Error> {
    match error {
        pdf_syntax::Error::Malformed { .. }
        | pdf_syntax::Error::Truncated { .. }
        | pdf_syntax::Error::Unsupported(_) => Ok(FontKind::Invalid),
        other => Err(Error::Syntax(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_names_with_or_without_a_subset_prefix() {
        for name in [&b"Helvetica"[..], b"ABCDEF+Times-Bold", b"ZapfDingbats"] {
            assert!(is_standard_14(name), "{name:?}");
        }
        for name in [
            &b"Arial"[..],
            b"abcdef+Helvetica",
            b"ABCDE+Courier",
            b"Helvetica-Light",
        ] {
            assert!(!is_standard_14(name), "{name:?}");
        }
    }
}
