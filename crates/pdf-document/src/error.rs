use std::fmt;

use pdf_syntax::ObjectId;

/// Error while building the document model.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Error of the syntax layer: malformed bytes, limits, cancellation, I/O or
    /// unsupported capability.
    Syntax(pdf_syntax::Error),
    /// The object `object` breaks the document structure (ISO 32000-2, 7.7).
    InvalidStructure {
        object: ObjectId,
        kind: StructureErrorKind,
    },
}

/// Precise reason for an `Error::InvalidStructure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StructureErrorKind {
    /// `/Root` is not a dictionary of type `/Catalog`.
    InvalidCatalog,
    /// The catalog has no `/Pages` reference.
    MissingPages,
    /// A page tree node is not a dictionary of type `/Pages` or `/Page`.
    InvalidNode,
    /// `/Kids` is absent or not an array of references.
    InvalidKids,
    /// `/Count` is absent, negative, or differs from the leaves below the node.
    CountMismatch,
    /// `/Parent` does not reference the node that lists this one, or the root
    /// has one.
    InvalidParent,
    /// A node is reached twice: a cycle or a shared subtree.
    RepeatedNode,
    /// A `/Kids` entry references a free or absent object; reported on the
    /// node that lists it.
    MissingKid,
    /// `/Resources` is neither a dictionary nor a reference to one.
    InvalidResources,
    /// `/MediaBox` is missing on a page and its ancestors (reported on the
    /// page), or a box is not four finite numbers (reported on the node that
    /// carries it).
    InvalidBox,
    /// `/Rotate` is not an integer multiple of 90; reported on the node that
    /// carries it.
    InvalidRotate,
    /// An indirect value resolves to a stream or to another reference where a
    /// direct object is expected.
    UnexpectedIndirection,
    /// `/UserUnit` is not a positive finite number; reported on the page.
    InvalidUserUnit,
    /// `/Contents` is not a stream reference, an array of them, a reference
    /// to such an array, or null; reported on the page by the consumers that
    /// read it.
    InvalidContents,
}

impl From<pdf_syntax::Error> for Error {
    fn from(error: pdf_syntax::Error) -> Self {
        Self::Syntax(error)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(error) => error.fmt(f),
            Self::InvalidStructure { object, kind } => write!(
                f,
                "invalid document structure at object {} {}: {kind:?}",
                object.number, object.generation
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Syntax(error) => Some(error),
            Self::InvalidStructure { .. } => None,
        }
    }
}
