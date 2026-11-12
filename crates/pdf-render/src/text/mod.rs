//! Text (ISO 32000-2, 9): text state, fonts and glyph outlines (ADR-0005).

pub(crate) mod cmap;
#[allow(
    dead_code,
    reason = "the Symbol and ZapfDingbats encodings serve the standard fonts (ADR-0005, step 5.3)"
)]
pub(crate) mod encodings;
pub(crate) mod font;
pub(crate) mod standard;
pub(crate) mod string;
pub(crate) mod type3;
pub(crate) mod unicode;

use std::sync::Arc;

use pdf_syntax::{Budget, Dictionary, IndirectValue, Object, PdfFile, Source};

use crate::Error;

/// The font selected by `Tf`.
#[derive(Debug, Clone, Default)]
pub(crate) enum Selected {
    /// No `Tf` yet.
    #[default]
    None,
    /// A font this renderer cannot draw yet: its text is skipped.
    Unsupported,
    /// A loaded font and its number in the rendering (key of the glyph
    /// cache: an address could be reused by another font once one is freed).
    Font {
        number: usize,
        font: Arc<font::Font>,
    },
    /// A Type 3 font, whose glyphs are content streams, and its number in
    /// the rendering (key of its resource scope).
    Type3 {
        number: usize,
        font: Arc<type3::Type3>,
    },
}

/// Text state parameters (9.3), part of the graphics state.
#[derive(Debug, Clone)]
pub(crate) struct TextState {
    pub char_spacing: f64,
    pub word_spacing: f64,
    /// `Tz` / 100.
    pub horizontal_scale: f64,
    pub leading: f64,
    pub font: Selected,
    pub size: f64,
    pub render_mode: u8,
    pub rise: f64,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 1.0,
            leading: 0.0,
            font: Selected::None,
            size: 0.0,
            render_mode: 0,
            rise: 0.0,
        }
    }
}

/// A syntax error makes the font unreadable rather than the page; limits,
/// cancellation and input errors stop the rendering.
fn tolerate<T>(result: Result<T, pdf_syntax::Error>) -> Result<Option<T>, Error> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(
            pdf_syntax::Error::Malformed { .. }
            | pdf_syntax::Error::Truncated { .. }
            | pdf_syntax::Error::Unsupported(_),
        ) => Ok(None),
        Err(other) => Err(Error::Syntax(other)),
    }
}

/// `object` resolved once if it is a reference; a stream gives its
/// dictionary. `None` if it does not resolve.
pub(crate) fn resolve<S: Source>(
    file: &PdfFile<S>,
    object: &Object,
    budget: &mut Budget,
) -> Result<Option<Object>, Error> {
    let Object::Reference(id) = object else {
        return Ok(Some(object.clone()));
    };
    Ok(
        tolerate(file.resolve(*id, budget))?.map(|value| match value {
            IndirectValue::Object(object) => object,
            IndirectValue::Stream(stream) => Object::Dictionary(stream.dictionary),
        }),
    )
}

pub(crate) fn resolve_dictionary<S: Source>(
    file: &PdfFile<S>,
    object: &Object,
    budget: &mut Budget,
) -> Result<Option<Dictionary>, Error> {
    Ok(match resolve(file, object, budget)? {
        Some(Object::Dictionary(dictionary)) => Some(dictionary),
        _ => None,
    })
}

/// The decoded data of the stream `object` refers to, charged to
/// `max_decoded_bytes`. `None` if it is not a readable stream.
pub(crate) fn stream_bytes<S: Source>(
    file: &PdfFile<S>,
    object: &Object,
    budget: &mut Budget,
) -> Result<Option<Vec<u8>>, Error> {
    let Object::Reference(id) = object else {
        return Ok(None);
    };
    let Some(IndirectValue::Stream(stream)) = tolerate(file.resolve(*id, budget))? else {
        return Ok(None);
    };
    tolerate(file.decoded_stream_data(&stream, budget))
}
