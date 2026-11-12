use std::fmt;

use pdf_syntax::{MalformedKind, ObjectId};

/// Error while rendering a page.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Error of the syntax layer while reading resources or content streams:
    /// malformed objects, document limits, cancellation, I/O or unsupported
    /// capability (such as a stream filter).
    Syntax(pdf_syntax::Error),
    /// Error of the document model (page structure).
    Document(pdf_document::Error),
    /// The content of page `page` breaks the content stream rules
    /// (ISO 32000-2, 7.8 and 8). `offset` is a byte position in the decoded
    /// content: the page's content streams concatenated in order, each one
    /// followed by a single newline.
    Content {
        page: ObjectId,
        offset: u64,
        kind: ContentErrorKind,
    },
    /// A rendering limit of `RenderLimits` was reached.
    LimitExceeded { kind: RenderLimitKind, limit: u64 },
    /// The request itself is invalid: scale not finite or not positive, or a
    /// page box of zero area.
    InvalidRequest,
}

/// Precise reason for an `Error::Content`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContentErrorKind {
    /// Token-level error in the content (bad string, number, delimiter...).
    Syntax(MalformedKind),
    /// The content ends inside a token, an array, a dictionary or an operation.
    Truncated,
    /// An operator that ISO 32000-2 does not define, outside `BX`/`EX`.
    UnknownOperator,
    /// Wrong number or type of operands, or an out-of-range value.
    InvalidOperands,
    /// More operands than `MAX_OPERANDS` before an operator.
    TooManyOperands,
    /// An operator where the content state forbids it: a path or `q`/`Q`
    /// inside a text object, a non-path operator inside a path, `BT` inside
    /// a text object, `ET` or `EX` without its opening operator.
    MisplacedOperator,
    /// `Q` without a matching `q`.
    UnbalancedRestore,
    /// A path operator that needs a current point, without one.
    NoCurrentPoint,
    /// A named resource is absent or of the wrong type.
    InvalidResource,
    /// An inline image without `ID` or without its `EI` end.
    InvalidInlineImage,
    /// A matrix is not finite, or a point (with the stroke width around it)
    /// lies more than 10^12 pixels away from the device origin.
    NumberOutOfRange,
    /// The content ends inside a text object, a path, a `q`, marked content
    /// or a compatibility section; the offset is the end of the content.
    Unterminated,
}

/// Which `RenderLimits` entry was reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RenderLimitKind {
    /// Pixels of the output bitmap.
    Pixels,
    /// Operators interpreted.
    Operations,
    /// `q` nesting.
    StateDepth,
    /// Estimated rasteriser work (line segments, tiles crossed, area covered,
    /// stroke outlines, dashes).
    PathWork,
    /// Clip paths active at once.
    Clips,
    /// Segments of one glyph outline.
    GlyphSegments,
    /// Glyphs recorded when the text of a page is extracted.
    TextGlyphs,
}

impl From<pdf_syntax::Error> for Error {
    fn from(error: pdf_syntax::Error) -> Self {
        Self::Syntax(error)
    }
}

impl From<pdf_document::Error> for Error {
    fn from(error: pdf_document::Error) -> Self {
        Self::Document(error)
    }
}

/// Never prints document content: only kinds, identifiers and offsets.
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(error) => write!(f, "{error}"),
            Self::Document(error) => write!(f, "{error}"),
            Self::Content { page, offset, kind } => write!(
                f,
                "invalid content of page object {} {} at content offset {offset}: {kind:?}",
                page.number, page.generation
            ),
            Self::LimitExceeded { kind, limit } => {
                write!(f, "render limit exceeded: {kind:?} (limit {limit})")
            }
            Self::InvalidRequest => write!(f, "invalid render request"),
        }
    }
}

impl std::error::Error for Error {}
