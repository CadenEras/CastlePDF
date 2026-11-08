//! Versioned messages carried inside frames (see `frame`).
//!
//! Wire format: one tag byte, then little-endian fields; a string is a `u16`
//! byte length then UTF-8, and tile pixels fill the rest of the frame. A
//! message with trailing bytes, a missing field, an invalid string, or an
//! unknown tag or code is malformed. The `Hello` layout (tag + `u16` version)
//! never changes, so mismatched versions are always detected.

use std::fmt;

use pdf_syntax::Error;

/// Largest tile side, in pixels: a tile (256 KiB of RGBA) and its header fit
/// a default frame of 1 MiB (ADR-0003).
pub const MAX_TILE_SIDE: u16 = 256;
/// Longest path accepted by `OpenDocument`, in UTF-8 bytes.
pub const MAX_PATH_BYTES: u16 = 4096;

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Hello {
        version: u16,
    },
    /// Synthetic job: `units` work units, sleeping `unit_delay_ms` after each.
    /// Every job (this one and those below) has an `id` strictly greater than
    /// every previous job id of the connection, otherwise it fails with
    /// `InvalidJobId`.
    RunSynthetic {
        id: u64,
        units: u64,
        unit_delay_ms: u8,
    },
    Cancel {
        id: u64,
    },
    Shutdown,
    /// Reads and opens the PDF file at `path` (chosen by the shell, never by
    /// the UI); answered by `DocumentOpened`.
    OpenDocument {
        id: u64,
        path: String,
    },
    /// Size in pixels of a page rendered at `scale`; answered by `PageSize`.
    PageSize {
        id: u64,
        document: u32,
        page: u32,
        scale: f64,
    },
    /// Renders a rectangle of a page at `scale`, at most `MAX_TILE_SIDE` on
    /// each side; answered by `Tile`.
    RenderTile {
        id: u64,
        document: u32,
        page: u32,
        scale: f64,
        rect: TileRect,
    },
    /// Forgets a document; not a job, never answered. Unknown ids are ignored.
    CloseDocument {
        document: u32,
    },
}

/// A rectangle of device pixels of a page rendering (see `pdf_render::PixelRect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileRect {
    pub x: u32,
    pub y: u32,
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    HelloAck {
        version: u16,
    },
    JobDone {
        id: u64,
        units: u64,
    },
    JobFailed {
        id: u64,
        code: ErrorCode,
    },
    /// Fatal for the connection: last frame sent, then the worker exits.
    ProtocolError {
        code: ErrorCode,
    },
    DocumentOpened {
        id: u64,
        document: u32,
        pages: u32,
    },
    PageSize {
        id: u64,
        width: u32,
        height: u32,
    },
    /// RGBA8 pixels, straight alpha, rows top to bottom: exactly
    /// `width * height * 4` bytes. `skipped` lists what the page uses but
    /// the renderer does not draw yet (see `Skipped`).
    Tile {
        id: u64,
        width: u16,
        height: u16,
        skipped: Skipped,
        pixels: Vec<u8>,
    },
}

/// Capabilities used by a page and not rendered: one bit per
/// `pdf_render::Unsupported` variant, `OTHER` for those added later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Skipped(pub u16);

impl Skipped {
    pub const TEXT: u16 = 1 << 0;
    pub const TEXT_CLIP: u16 = 1 << 1;
    pub const IMAGE: u16 = 1 << 2;
    pub const XOBJECT: u16 = 1 << 3;
    pub const SHADING: u16 = 1 << 4;
    pub const PATTERN: u16 = 1 << 5;
    pub const COLOR_SPACE: u16 = 1 << 6;
    pub const BLEND_MODE: u16 = 1 << 7;
    pub const SOFT_MASK: u16 = 1 << 8;
    pub const ALPHA_IS_SHAPE: u16 = 1 << 9;
    pub const TRANSFER_FUNCTION: u16 = 1 << 10;
    pub const OPTIONAL_CONTENT: u16 = 1 << 11;
    pub const OTHER: u16 = 1 << 15;
    const KNOWN: u16 = ((1 << 12) - 1) | Self::OTHER;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrorCode {
    Cancelled = 1,
    LimitExceeded = 2,
    Busy = 3,
    MalformedMessage = 4,
    UnsupportedVersion = 5,
    FrameTooLarge = 6,
    Internal = 7,
    InvalidJobId = 8,
    /// A request field is out of range: empty or oversized tile, tile outside
    /// the page, scale not finite or not positive, page number too large.
    InvalidRequest = 9,
    /// No open document has this id.
    UnknownDocument = 10,
    /// The file cannot be read: missing, not a regular file, I/O error.
    Unreadable = 11,
    /// The file or the page breaks the PDF rules (ISO 32000-2).
    Malformed = 12,
    /// The document uses a capability not implemented yet.
    Unsupported = 13,
    /// The document is encrypted: not supported yet.
    Encrypted = 14,
    /// The worker already holds `MAX_DOCUMENTS` documents.
    TooManyDocuments = 15,
}

impl ErrorCode {
    fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::Cancelled,
            2 => Self::LimitExceeded,
            3 => Self::Busy,
            4 => Self::MalformedMessage,
            5 => Self::UnsupportedVersion,
            6 => Self::FrameTooLarge,
            7 => Self::Internal,
            8 => Self::InvalidJobId,
            9 => Self::InvalidRequest,
            10 => Self::UnknownDocument,
            11 => Self::Unreadable,
            12 => Self::Malformed,
            13 => Self::Unsupported,
            14 => Self::Encrypted,
            15 => Self::TooManyDocuments,
            _ => return None,
        })
    }
}

impl From<&Error> for ErrorCode {
    fn from(error: &Error) -> Self {
        match error {
            Error::Cancelled => Self::Cancelled,
            Error::LimitExceeded { .. } => Self::LimitExceeded,
            Error::Malformed { .. } | Error::Truncated { .. } => Self::Malformed,
            Error::Io(_) => Self::Unreadable,
            Error::Unsupported(pdf_syntax::Capability::Encryption) => Self::Encrypted,
            Error::Unsupported(_) => Self::Unsupported,
            _ => Self::Internal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError;

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("malformed worker message")
    }
}

impl std::error::Error for DecodeError {}

const HELLO: u8 = 0x01;
const RUN_SYNTHETIC: u8 = 0x02;
const CANCEL: u8 = 0x03;
const SHUTDOWN: u8 = 0x04;
const OPEN_DOCUMENT: u8 = 0x05;
const PAGE_SIZE: u8 = 0x06;
const RENDER_TILE: u8 = 0x07;
const CLOSE_DOCUMENT: u8 = 0x08;
const HELLO_ACK: u8 = 0x81;
const JOB_DONE: u8 = 0x82;
const JOB_FAILED: u8 = 0x83;
const PROTOCOL_ERROR: u8 = 0x84;
const DOCUMENT_OPENED: u8 = 0x85;
const PAGE_SIZE_REPLY: u8 = 0x86;
const TILE: u8 = 0x87;

/// Bytes of a `Tile` response before its pixels.
pub const TILE_HEADER_LEN: usize = 1 + 8 + 2 + 2 + 2;

struct Writer(Vec<u8>);

impl Writer {
    fn new(tag: u8, capacity: usize) -> Self {
        let mut out = Vec::with_capacity(capacity);
        out.push(tag);
        Self(out)
    }

    fn u8(mut self, value: u8) -> Self {
        self.0.push(value);
        self
    }

    fn u16(mut self, value: u16) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn u32(mut self, value: u32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn u64(mut self, value: u64) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn f64(self, value: f64) -> Self {
        self.u64(value.to_bits())
    }

    /// A string longer than `u16::MAX` bytes is cut at a character boundary;
    /// `OpenDocument` paths are checked against `MAX_PATH_BYTES` before.
    fn str(self, value: &str) -> Self {
        let mut end = value.len().min(usize::from(u16::MAX));
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        let bytes = value.as_bytes().get(..end).unwrap_or_default();
        let mut out = self.u16(u16::try_from(bytes.len()).unwrap_or(u16::MAX));
        out.0.extend_from_slice(bytes);
        out
    }

    fn bytes(mut self, value: &[u8]) -> Self {
        self.0.extend_from_slice(value);
        self
    }
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Hello { version } => Writer::new(HELLO, 3).u16(*version),
            Self::RunSynthetic {
                id,
                units,
                unit_delay_ms,
            } => Writer::new(RUN_SYNTHETIC, 18)
                .u64(*id)
                .u64(*units)
                .u8(*unit_delay_ms),
            Self::Cancel { id } => Writer::new(CANCEL, 9).u64(*id),
            Self::Shutdown => Writer::new(SHUTDOWN, 1),
            Self::OpenDocument { id, path } => Writer::new(OPEN_DOCUMENT, 11 + path.len())
                .u64(*id)
                .str(path),
            Self::PageSize {
                id,
                document,
                page,
                scale,
            } => Writer::new(PAGE_SIZE, 25)
                .u64(*id)
                .u32(*document)
                .u32(*page)
                .f64(*scale),
            Self::RenderTile {
                id,
                document,
                page,
                scale,
                rect,
            } => Writer::new(RENDER_TILE, 37)
                .u64(*id)
                .u32(*document)
                .u32(*page)
                .f64(*scale)
                .u32(rect.x)
                .u32(rect.y)
                .u16(rect.width)
                .u16(rect.height),
            Self::CloseDocument { document } => Writer::new(CLOSE_DOCUMENT, 5).u32(*document),
        }
        .0
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader(bytes);
        let message = match r.u8()? {
            HELLO => Self::Hello { version: r.u16()? },
            RUN_SYNTHETIC => Self::RunSynthetic {
                id: r.u64()?,
                units: r.u64()?,
                unit_delay_ms: r.u8()?,
            },
            CANCEL => Self::Cancel { id: r.u64()? },
            SHUTDOWN => Self::Shutdown,
            OPEN_DOCUMENT => Self::OpenDocument {
                id: r.u64()?,
                path: r.str()?,
            },
            PAGE_SIZE => Self::PageSize {
                id: r.u64()?,
                document: r.u32()?,
                page: r.u32()?,
                scale: r.f64()?,
            },
            RENDER_TILE => Self::RenderTile {
                id: r.u64()?,
                document: r.u32()?,
                page: r.u32()?,
                scale: r.f64()?,
                rect: TileRect {
                    x: r.u32()?,
                    y: r.u32()?,
                    width: r.u16()?,
                    height: r.u16()?,
                },
            },
            CLOSE_DOCUMENT => Self::CloseDocument { document: r.u32()? },
            _ => return Err(DecodeError),
        };
        r.finish()?;
        Ok(message)
    }
}

impl Response {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::HelloAck { version } => Writer::new(HELLO_ACK, 3).u16(*version),
            Self::JobDone { id, units } => Writer::new(JOB_DONE, 17).u64(*id).u64(*units),
            Self::JobFailed { id, code } => Writer::new(JOB_FAILED, 10).u64(*id).u8(*code as u8),
            Self::ProtocolError { code } => Writer::new(PROTOCOL_ERROR, 2).u8(*code as u8),
            Self::DocumentOpened {
                id,
                document,
                pages,
            } => Writer::new(DOCUMENT_OPENED, 17)
                .u64(*id)
                .u32(*document)
                .u32(*pages),
            Self::PageSize { id, width, height } => Writer::new(PAGE_SIZE_REPLY, 17)
                .u64(*id)
                .u32(*width)
                .u32(*height),
            Self::Tile {
                id,
                width,
                height,
                skipped,
                pixels,
            } => Writer::new(TILE, TILE_HEADER_LEN + pixels.len())
                .u64(*id)
                .u16(*width)
                .u16(*height)
                .u16(skipped.0)
                .bytes(pixels),
        }
        .0
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader(bytes);
        let message = match r.u8()? {
            HELLO_ACK => Self::HelloAck { version: r.u16()? },
            JOB_DONE => Self::JobDone {
                id: r.u64()?,
                units: r.u64()?,
            },
            JOB_FAILED => Self::JobFailed {
                id: r.u64()?,
                code: r.code()?,
            },
            PROTOCOL_ERROR => Self::ProtocolError { code: r.code()? },
            DOCUMENT_OPENED => Self::DocumentOpened {
                id: r.u64()?,
                document: r.u32()?,
                pages: r.u32()?,
            },
            PAGE_SIZE_REPLY => Self::PageSize {
                id: r.u64()?,
                width: r.u32()?,
                height: r.u32()?,
            },
            TILE => {
                let id = r.u64()?;
                let (width, height) = (r.u16()?, r.u16()?);
                let skipped = r.u16()?;
                let side_ok = |side: u16| (1..=MAX_TILE_SIDE).contains(&side);
                if !side_ok(width) || !side_ok(height) || skipped & !Skipped::KNOWN != 0 {
                    return Err(DecodeError);
                }
                let len = usize::from(width) * usize::from(height) * 4;
                if r.0.len() != len {
                    return Err(DecodeError);
                }
                let pixels = r.0.to_vec();
                r.0 = &[];
                Self::Tile {
                    id,
                    width,
                    height,
                    skipped: Skipped(skipped),
                    pixels,
                }
            }
            _ => return Err(DecodeError),
        };
        r.finish()?;
        Ok(message)
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let (head, rest) = self.0.split_first_chunk::<N>().ok_or(DecodeError)?;
        self.0 = rest;
        Ok(*head)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        self.take::<1>().map(|[b]| b)
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        self.take().map(u16::from_le_bytes)
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        self.take().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        self.take().map(u64::from_le_bytes)
    }

    fn f64(&mut self) -> Result<f64, DecodeError> {
        self.u64().map(f64::from_bits)
    }

    fn str(&mut self) -> Result<String, DecodeError> {
        let len = usize::from(self.u16()?);
        let (bytes, rest) = self.0.split_at_checked(len).ok_or(DecodeError)?;
        self.0 = rest;
        String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError)
    }

    fn code(&mut self) -> Result<ErrorCode, DecodeError> {
        ErrorCode::from_u8(self.u8()?).ok_or(DecodeError)
    }

    fn finish(self) -> Result<(), DecodeError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(DecodeError)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requests() -> Vec<Request> {
        vec![
            Request::Hello { version: 1 },
            Request::RunSynthetic {
                id: u64::MAX,
                units: 42,
                unit_delay_ms: 255,
            },
            Request::Cancel { id: 7 },
            Request::Shutdown,
            Request::OpenDocument {
                id: 8,
                path: "C:\\docs\\é.pdf".to_owned(),
            },
            Request::PageSize {
                id: 9,
                document: 1,
                page: 2,
                scale: 1.5,
            },
            Request::RenderTile {
                id: 10,
                document: 1,
                page: 0,
                scale: 2.0,
                rect: TileRect {
                    x: 256,
                    y: 512,
                    width: 256,
                    height: 13,
                },
            },
            Request::CloseDocument { document: 3 },
        ]
    }

    fn responses() -> Vec<Response> {
        vec![
            Response::HelloAck { version: 1 },
            Response::JobDone { id: 3, units: 9 },
            Response::JobFailed {
                id: 3,
                code: ErrorCode::Encrypted,
            },
            Response::ProtocolError {
                code: ErrorCode::FrameTooLarge,
            },
            Response::DocumentOpened {
                id: 4,
                document: 1,
                pages: 12,
            },
            Response::PageSize {
                id: 5,
                width: 612,
                height: 792,
            },
            Response::Tile {
                id: 6,
                width: 2,
                height: 1,
                skipped: Skipped(Skipped::TEXT | Skipped::OTHER),
                pixels: vec![1, 2, 3, 4, 5, 6, 7, 8],
            },
        ]
    }

    #[test]
    fn messages_round_trip() {
        for request in requests() {
            assert_eq!(Request::decode(&request.encode()), Ok(request));
        }
        for response in responses() {
            assert_eq!(Response::decode(&response.encode()), Ok(response));
        }
    }

    #[test]
    fn trailing_and_missing_bytes_are_rejected() {
        for request in requests() {
            let mut bytes = request.encode();
            bytes.push(0);
            assert_eq!(Request::decode(&bytes), Err(DecodeError), "{request:?}");
            bytes.truncate(bytes.len() - 2);
            if !bytes.is_empty() {
                assert_eq!(Request::decode(&bytes), Err(DecodeError), "{request:?}");
            }
        }
        for response in responses() {
            let mut bytes = response.encode();
            bytes.push(0);
            assert_eq!(Response::decode(&bytes), Err(DecodeError), "{response:?}");
            bytes.truncate(bytes.len() - 2);
            assert_eq!(Response::decode(&bytes), Err(DecodeError), "{response:?}");
        }
    }

    #[test]
    fn unknown_tags_and_codes_are_rejected() {
        assert_eq!(Request::decode(&[]), Err(DecodeError));
        assert_eq!(Request::decode(&[0x7f]), Err(DecodeError));
        assert_eq!(Request::decode(&[HELLO_ACK, 1, 0]), Err(DecodeError));
        assert_eq!(Response::decode(&[PROTOCOL_ERROR, 0]), Err(DecodeError));
        assert_eq!(Response::decode(&[PROTOCOL_ERROR, 16]), Err(DecodeError));
    }

    #[test]
    fn strings_must_be_utf8_and_complete() {
        let mut bytes = vec![OPEN_DOCUMENT];
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&[0xc3, 0x28]);
        assert_eq!(Request::decode(&bytes), Err(DecodeError));
        bytes.truncate(bytes.len() - 1);
        assert_eq!(Request::decode(&bytes), Err(DecodeError));
    }

    #[test]
    fn tiles_are_checked_against_their_size() {
        let tile = |width: u16, height: u16, skipped: u16, len: usize| {
            let mut bytes = vec![TILE];
            bytes.extend_from_slice(&1u64.to_le_bytes());
            bytes.extend_from_slice(&width.to_le_bytes());
            bytes.extend_from_slice(&height.to_le_bytes());
            bytes.extend_from_slice(&skipped.to_le_bytes());
            bytes.resize(TILE_HEADER_LEN + len, 0);
            Response::decode(&bytes)
        };
        assert!(tile(256, 256, 0, 256 * 256 * 4).is_ok());
        assert!(tile(1, 1, Skipped::OPTIONAL_CONTENT, 4).is_ok());
        for (width, height, skipped, len) in [
            (0, 1, 0, 0),
            (1, 0, 0, 0),
            (257, 1, 0, 257 * 4),
            (2, 2, 0, 15),
            (2, 2, 0, 17),
            (1, 1, 1 << 12, 4),
        ] {
            assert_eq!(tile(width, height, skipped, len), Err(DecodeError));
        }
    }

    #[test]
    fn hello_layout_is_stable() {
        assert_eq!(
            Request::Hello { version: 0x0102 }.encode(),
            vec![HELLO, 0x02, 0x01]
        );
    }
}
