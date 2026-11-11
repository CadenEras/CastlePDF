//! Documents held by the worker and the jobs that use them: open a file,
//! size a page, render a tile. Each job runs under its own `Budget`; the
//! documents themselves are immutable once opened and shared with the job
//! thread. Each keeps the cache of its renders (`RenderCache`), which its
//! tiles reuse.

use std::collections::BTreeMap;
use std::fs::File;
use std::ops::Deref;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use pdf_document::Document;
use pdf_render::{
    PixelRect, RenderCache, RenderOptions, Unsupported, page_size, render_area_cached,
};
use pdf_syntax::{Budget, MemorySource};

use crate::protocol::{
    ErrorCode, MAX_PATH_BYTES, MAX_TILE_SIDE, Repair, Response, Skipped, TileRect,
};

/// Documents open at once; each holds its whole file in memory (ADR-0002).
pub const MAX_DOCUMENTS: usize = 4;

/// An open document and the cache of its renders: fonts, images, forms
/// and page content read once for all its tiles, at most
/// `RenderCache::DEFAULT_BYTES` each, so `MAX_DOCUMENTS` times that in all.
pub struct Open {
    document: Document<MemorySource>,
    cache: Mutex<RenderCache>,
}

impl Deref for Open {
    type Target = Document<MemorySource>;

    fn deref(&self) -> &Self::Target {
        &self.document
    }
}

pub type Shared = Arc<Open>;

/// Open documents by id. Ids start at 1 and are never reused.
#[derive(Default)]
pub struct Documents {
    open: BTreeMap<u32, Shared>,
    last: u32,
}

impl Documents {
    pub fn get(&self, id: u32) -> Option<Shared> {
        self.open.get(&id).cloned()
    }

    pub fn is_full(&self) -> bool {
        self.open.len() >= MAX_DOCUMENTS
    }

    /// Stores a document; `None` when full or out of ids.
    pub fn insert(&mut self, document: Shared) -> Option<u32> {
        if self.is_full() {
            return None;
        }
        let id = self.last.checked_add(1)?;
        self.last = id;
        self.open.insert(id, document);
        Some(id)
    }

    pub fn remove(&mut self, id: u32) {
        self.open.remove(&id);
    }
}

/// Reads and opens the file at `path`.
pub fn open(path: &str, budget: &mut Budget) -> Result<Shared, ErrorCode> {
    if path.is_empty() || path.len() > usize::from(MAX_PATH_BYTES) {
        return Err(ErrorCode::InvalidRequest);
    }
    let path = Path::new(path);
    let limits = budget.limits();
    let metadata = std::fs::metadata(path).map_err(|_| ErrorCode::Unreadable)?;
    if !metadata.is_file() {
        return Err(ErrorCode::Unreadable);
    }
    let file = File::open(path).map_err(|_| ErrorCode::Unreadable)?;
    let source = MemorySource::from_reader(file, Some(metadata.len()), &limits)
        .map_err(|e| ErrorCode::from(&e))?;
    budget.check_cancelled().map_err(|e| ErrorCode::from(&e))?;
    Document::open(source, budget)
        .map(|document| {
            Arc::new(Open {
                document,
                cache: Mutex::new(RenderCache::default()),
            })
        })
        .map_err(|e| document_code(&e))
}

pub fn size(document: &Shared, page: u32, scale: f64, id: u64) -> Result<Response, ErrorCode> {
    let page = page_of(document, page)?;
    let mut options = RenderOptions::default();
    options.scale = scale;
    let (width, height) = page_size(page, &options).map_err(|e| render_code(&e))?;
    Ok(Response::PageSize { id, width, height })
}

pub fn tile(
    document: &Shared,
    page: u32,
    scale: f64,
    rect: TileRect,
    id: u64,
    budget: &mut Budget,
) -> Result<Response, ErrorCode> {
    let side = 1..=MAX_TILE_SIDE;
    if !side.contains(&rect.width) || !side.contains(&rect.height) {
        return Err(ErrorCode::InvalidRequest);
    }
    let page = page_of(document, page)?;
    let mut options = RenderOptions::default();
    options.scale = scale;
    let area = PixelRect {
        x: rect.x,
        y: rect.y,
        width: u32::from(rect.width),
        height: u32::from(rect.height),
    };
    // One job runs at a time: the lock is never contended. A job that
    // panicked left complete entries only, so a poisoned cache is usable.
    let mut cache = document
        .cache
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let rendering = render_area_cached(document.file(), page, &options, area, &mut cache, budget)
        .map_err(|e| render_code(&e))?;
    let skipped = rendering
        .skipped
        .iter()
        .fold(0, |bits, capability| bits | skipped_bit(*capability));
    Ok(Response::Tile {
        id,
        width: rect.width,
        height: rect.height,
        skipped: Skipped(skipped),
        repair: repair(document),
        pixels: rendering.bitmap.into_data(),
    })
}

/// Whether the structure of `document` was reconstructed, so far.
pub fn repair(document: &Shared) -> Repair {
    match document.file().repair() {
        None => Repair::None,
        Some(report) if report.is_incomplete() => Repair::Incomplete,
        Some(_) => Repair::Repaired,
    }
}

fn page_of(document: &Shared, page: u32) -> Result<&pdf_document::Page, ErrorCode> {
    usize::try_from(page)
        .ok()
        .and_then(|index| document.page(index))
        .ok_or(ErrorCode::InvalidRequest)
}

fn skipped_bit(capability: Unsupported) -> u16 {
    match capability {
        Unsupported::Text => Skipped::TEXT,
        Unsupported::TextClip => Skipped::TEXT_CLIP,
        Unsupported::Image => Skipped::IMAGE,
        Unsupported::XObject => Skipped::XOBJECT,
        Unsupported::Shading => Skipped::SHADING,
        Unsupported::Pattern => Skipped::PATTERN,
        Unsupported::ColorSpace => Skipped::COLOR_SPACE,
        Unsupported::BlendMode => Skipped::BLEND_MODE,
        Unsupported::SoftMask => Skipped::SOFT_MASK,
        Unsupported::AlphaIsShape => Skipped::ALPHA_IS_SHAPE,
        Unsupported::TransferFunction => Skipped::TRANSFER_FUNCTION,
        Unsupported::OptionalContent => Skipped::OPTIONAL_CONTENT,
        Unsupported::FontSubstituted => Skipped::FONT_SUBSTITUTED,
        _ => Skipped::OTHER,
    }
}

fn document_code(error: &pdf_document::Error) -> ErrorCode {
    match error {
        pdf_document::Error::Syntax(e) => ErrorCode::from(e),
        pdf_document::Error::InvalidStructure { .. } => ErrorCode::Malformed,
        _ => ErrorCode::Internal,
    }
}

fn render_code(error: &pdf_render::Error) -> ErrorCode {
    match error {
        pdf_render::Error::Syntax(e) => ErrorCode::from(e),
        pdf_render::Error::Document(e) => document_code(e),
        pdf_render::Error::Content { .. } => ErrorCode::Malformed,
        pdf_render::Error::LimitExceeded { .. } => ErrorCode::LimitExceeded,
        pdf_render::Error::InvalidRequest => ErrorCode::InvalidRequest,
        _ => ErrorCode::Internal,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pdf_syntax::{CancellationToken, ResourceLimits};

    use super::*;

    fn budget() -> Budget {
        Budget::new(ResourceLimits::default(), CancellationToken::new())
    }

    fn fixture(name: &str) -> String {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/corpus/public")
            .join(name)
            .to_string_lossy()
            .into_owned()
    }

    /// A tile of the viewer's grid is one rasterisation cell: a larger tile
    /// would not fit the frame, a smaller cell would make each tile render
    /// several (ADR-0003 amendment).
    #[test]
    fn a_tile_is_one_rasterisation_cell() {
        assert_eq!(u32::from(MAX_TILE_SIDE), pdf_render::CELL_SIDE);
    }

    /// The tiles of a document share its render cache: the second tile of
    /// a page reuses what the first one read, and gives the same pixels.
    #[test]
    fn tiles_reuse_the_document_cache() {
        let document = open(&fixture("images.pdf"), &mut budget()).unwrap();
        let rect = TileRect {
            x: 0,
            y: 0,
            width: 256,
            height: 256,
        };
        let pixels_of = |budget: &mut Budget| match tile(&document, 0, 2.0, rect, 1, budget) {
            Ok(Response::Tile { pixels, .. }) => pixels,
            other => panic!("tile expected: {other:?}"),
        };
        let mut first = budget();
        let a = pixels_of(&mut first);
        let kept = document.cache.lock().unwrap().bytes();
        assert!(kept > 0);
        let mut second = budget();
        let b = pixels_of(&mut second);
        assert!(a == b);
        assert!(second.decoded_bytes() < first.decoded_bytes());
    }

    #[test]
    fn documents_are_bounded_and_ids_never_reused() {
        let document = open(&fixture("shapes.pdf"), &mut budget()).unwrap();
        let mut documents = Documents::default();
        let ids: Vec<_> = (0..MAX_DOCUMENTS)
            .map(|_| documents.insert(Arc::clone(&document)))
            .collect();
        assert_eq!(ids, vec![Some(1), Some(2), Some(3), Some(4)]);
        assert!(documents.is_full());
        assert_eq!(documents.insert(Arc::clone(&document)), None);
        documents.remove(2);
        assert!(documents.get(2).is_none());
        assert_eq!(documents.insert(document), Some(5));
    }

    #[test]
    fn open_reports_why_a_file_cannot_be_used() {
        assert_eq!(
            open(&fixture("missing.pdf"), &mut budget()).err(),
            Some(ErrorCode::Unreadable)
        );
        assert_eq!(
            open(&fixture(""), &mut budget()).err(),
            Some(ErrorCode::Unreadable)
        );
        assert_eq!(
            open(&fixture("manifest.json"), &mut budget()).err(),
            Some(ErrorCode::Malformed)
        );
        assert_eq!(
            open("", &mut budget()).err(),
            Some(ErrorCode::InvalidRequest)
        );
        let too_long = "a".repeat(usize::from(MAX_PATH_BYTES) + 1);
        assert_eq!(
            open(&too_long, &mut budget()).err(),
            Some(ErrorCode::InvalidRequest)
        );
        let token = CancellationToken::new();
        token.cancel();
        let mut cancelled = Budget::new(ResourceLimits::default(), token);
        assert_eq!(
            open(&fixture("shapes.pdf"), &mut cancelled).err(),
            Some(ErrorCode::Cancelled)
        );
    }

    #[test]
    fn page_size_and_tiles() {
        let document = open(&fixture("shapes.pdf"), &mut budget()).unwrap();
        assert_eq!(
            size(&document, 0, 2.0, 7),
            Ok(Response::PageSize {
                id: 7,
                width: 400,
                height: 400
            })
        );
        assert_eq!(size(&document, 3, 1.0, 7), Err(ErrorCode::InvalidRequest));
        assert_eq!(
            size(&document, 0, f64::NAN, 7),
            Err(ErrorCode::InvalidRequest)
        );
        assert_eq!(size(&document, 0, 1e6, 7), Err(ErrorCode::LimitExceeded));

        let rect = TileRect {
            x: 256,
            y: 0,
            width: 144,
            height: 256,
        };
        let Ok(Response::Tile {
            id,
            width,
            height,
            skipped,
            repair,
            pixels,
        }) = tile(&document, 0, 2.0, rect, 8, &mut budget())
        else {
            panic!("tile expected");
        };
        assert_eq!(
            (id, width, height, skipped, repair),
            (8, 144, 256, Skipped(0), Repair::None)
        );
        assert_eq!(pixels.len(), 144 * 256 * 4);
        for (rect, code) in [
            (TileRect { width: 0, ..rect }, ErrorCode::InvalidRequest),
            (
                TileRect {
                    width: MAX_TILE_SIDE + 1,
                    ..rect
                },
                ErrorCode::InvalidRequest,
            ),
            (TileRect { x: 257, ..rect }, ErrorCode::InvalidRequest),
        ] {
            assert_eq!(
                tile(&document, 0, 2.0, rect, 9, &mut budget()).err(),
                Some(code)
            );
        }
        let minimal = open(&fixture("minimal.pdf"), &mut budget()).unwrap();
        let corner = TileRect {
            x: 0,
            y: 0,
            width: 16,
            height: 16,
        };
        let Ok(Response::Tile { skipped, .. }) = tile(&minimal, 0, 1.0, corner, 10, &mut budget())
        else {
            panic!("tile expected");
        };
        // Helvetica is not embedded: drawn with a replacement font.
        assert_eq!(skipped, Skipped(Skipped::FONT_SUBSTITUTED));
    }
}
