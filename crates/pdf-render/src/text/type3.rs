//! Type 3 fonts (ISO 32000-2, 9.6.4; ADR-0005, step 5.5): each glyph is a
//! content stream of `/CharProcs`, run by the interpreter like a form with
//! `/FontMatrix` and the font's `/Resources` (or those of the content that
//! shows the text). This module reads the font dictionary; drawing the
//! glyphs is the interpreter's.

use pdf_syntax::{Budget, Dictionary, Object, ObjectId, PdfFile, Source};

use super::font::{encoding_names, simple_widths};
use super::resolve_dictionary;
use crate::Error;
use crate::geometry::Matrix;

/// A Type 3 font ready to show text.
#[derive(Debug)]
pub(crate) struct Type3 {
    /// Glyph space to text space.
    pub matrix: Matrix,
    /// The glyph procedure of each code (an indirect stream), if any.
    pub procs: Box<[Option<ObjectId>; 256]>,
    /// Codes whose glyph name has no procedure: their text is reported.
    pub missing: Box<[bool; 256]>,
    /// Widths in glyph space (through `matrix` to text space).
    pub widths: Box<[f64; 256]>,
    /// The font's resources; `None`: those of the content showing text.
    pub resources: Option<Dictionary>,
}

impl Type3 {
    /// The horizontal displacement of `code` in text space for a font size
    /// of 1 (9.4.4): its width through the font matrix.
    pub fn advance(&self, code: u8) -> f64 {
        self.widths.get(usize::from(code)).copied().unwrap_or(0.0) * self.matrix.a
    }
}

/// Reads a Type 3 font dictionary. `Ok(None)` when it cannot be drawn: no
/// readable `/CharProcs` or `/Encoding` (required: codes would map to
/// nothing, silently), a `/FontMatrix` that is not an invertible matrix,
/// or `/Resources` given but unreadable (names would resolve elsewhere).
/// The encoding is `/Differences` over an optional `/BaseEncoding`.
/// Limits and cancellation are errors.
pub(crate) fn load<S: Source>(
    file: &PdfFile<S>,
    font: &Dictionary,
    budget: &mut Budget,
) -> Result<Option<Type3>, Error> {
    let matrix = match font.get(b"FontMatrix") {
        Some(object) => match super::resolve(file, object, budget)? {
            Some(Object::Array(items)) => numbers6(&items),
            _ => None,
        },
        None => None,
    };
    let Some(matrix) = matrix.filter(|m| m.is_finite() && (m.a * m.d - m.b * m.c) != 0.0) else {
        return Ok(None);
    };
    let Some(procs_dictionary) = (match font.get(b"CharProcs") {
        Some(object) => resolve_dictionary(file, object, budget)?,
        None => None,
    }) else {
        return Ok(None);
    };
    let encoding = match font.get(b"Encoding") {
        Some(object) => super::resolve(file, object, budget)?,
        None => None,
    };
    if !matches!(encoding, Some(Object::Dictionary(_) | Object::Name(_))) {
        return Ok(None);
    }
    let names = encoding_names(file, encoding.as_ref(), None, budget)?;
    let mut procs = Box::new([None; 256]);
    let mut missing = Box::new([false; 256]);
    for ((slot, lost), name) in procs.iter_mut().zip(missing.iter_mut()).zip(&names) {
        let Some(name) = name.as_deref() else {
            continue;
        };
        match procs_dictionary.get(name) {
            Some(Object::Reference(id)) => *slot = Some(*id),
            _ => *lost = name != b".notdef" && name != b"space",
        }
    }
    let widths = simple_widths(
        file,
        font,
        &Dictionary::new(),
        None,
        &[None; 256],
        1000.0,
        budget,
    )?;
    let resources = match font.get(b"Resources") {
        None | Some(Object::Null) => None,
        Some(object) => match resolve_dictionary(file, object, budget)? {
            Some(resources) => Some(resources),
            None => return Ok(None),
        },
    };
    Ok(Some(Type3 {
        matrix,
        procs,
        missing,
        widths,
        resources,
    }))
}

fn numbers6(items: &[Object]) -> Option<Matrix> {
    let mut values = [0.0; 6];
    if items.len() != 6 {
        return None;
    }
    for (slot, item) in values.iter_mut().zip(items) {
        *slot = match item {
            Object::Integer(v) => *v as f64,
            Object::Real(v) => *v,
            _ => return None,
        };
    }
    let [a, b, c, d, e, f] = values;
    Some(Matrix::new(a, b, c, d, e, f))
}
