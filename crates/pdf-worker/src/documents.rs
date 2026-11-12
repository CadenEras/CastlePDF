//! Documents held by the worker and the jobs that use them: open a file,
//! size a page, render a tile, give a page's text layer. Each job runs under its own `Budget`; the
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
    PixelRect, RenderCache, RenderLimits, RenderOptions, TextWord, Unsupported, page_size,
    page_text_cached, page_transform, reading_lines, render_area_cached,
};
use pdf_syntax::{Budget, MemorySource};

use crate::protocol::{
    ErrorCode, LAYER_UNITS_PER_PIXEL, LayerLine, LayerWord, MAX_PATH_BYTES, MAX_TEXT_LAYER_BYTES,
    MAX_TILE_SIDE, PAGE_TEXT_HEADER_LEN, Repair, Response, Skipped, TextLayer, TileRect, word_len,
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

/// The text layer of `page` (ADR-0006, tranche 3.6a): its lines in reading
/// order, each word placed on the page rendered at scale 1. Words past
/// `MAX_TEXT_LAYER_BYTES` of response are left out (`truncated`). Fonts
/// and CMaps come from, and stay in, the document's cache.
pub fn page_text(
    document: &Shared,
    page: u32,
    id: u64,
    budget: &mut Budget,
) -> Result<Response, ErrorCode> {
    let page = page_of(document, page)?;
    let transform = page_transform(page, &RenderOptions::default()).map_err(|e| render_code(&e))?;
    let text = {
        // As for tiles: one job at a time, a poisoned cache is usable.
        let mut cache = document
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        page_text_cached(
            document.file(),
            page,
            &RenderLimits::default(),
            &mut cache,
            budget,
        )
        .map_err(|e| render_code(&e))?
    };
    let mut layer = TextLayer::default();
    let mut size = PAGE_TEXT_HEADER_LEN;
    'lines: for line in reading_lines(&text) {
        size += 4;
        if size > MAX_TEXT_LAYER_BYTES {
            layer.truncated = true;
            break;
        }
        let mut words = Vec::with_capacity(line.words.len());
        for word in &line.words {
            let word = layer_word(word, &transform);
            size += word_len(&word);
            if size > MAX_TEXT_LAYER_BYTES {
                layer.truncated = true;
                if !words.is_empty() {
                    layer.lines.push(LayerLine { words });
                }
                break 'lines;
            }
            words.push(word);
        }
        layer.lines.push(LayerLine { words });
    }
    Ok(Response::PageText { id, layer })
}

/// A word in layer units: its baseline through `transform` (user space to
/// pixels at scale 1), its size scaled likewise. Coordinates out of the
/// `i32` range are clamped (`as` saturates; a non-finite value gives 0).
fn layer_word(word: &TextWord, transform: &[f64; 6]) -> LayerWord {
    let [a, b, c, d, e, f] = *transform;
    let point = |x: f64, y: f64| (a * x + c * y + e, b * x + d * y + f);
    let [x0, y0, x1, y1] = word.baseline;
    let (start, end) = (point(x0, y0), point(x1, y1));
    let units = |value: f64| (value * LAYER_UNITS_PER_PIXEL).round() as i32;
    let scale = (a * d - b * c).abs().sqrt();
    LayerWord {
        text: word.text.clone(),
        baseline: [units(start.0), units(start.1), units(end.0), units(end.1)],
        size: (word.size * scale * LAYER_UNITS_PER_PIXEL).round() as u32,
    }
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

    fn layer(path: &str, page: u32) -> TextLayer {
        let document = open(&fixture(path), &mut budget()).unwrap();
        match page_text(&document, page, 3, &mut budget()) {
            Ok(Response::PageText { id: 3, layer }) => layer,
            other => panic!("text layer expected, got {other:?}"),
        }
    }

    fn line_texts(layer: &TextLayer) -> Vec<String> {
        layer
            .lines
            .iter()
            .map(|line| {
                line.words
                    .iter()
                    .map(|word| word.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    #[test]
    fn the_text_layer_is_in_reading_order_and_placed_on_the_rendering() {
        let layer = layer("columns.pdf", 0);
        assert!(!layer.truncated);
        assert_eq!(
            line_texts(&layer)[..3],
            ["A title over both columns", "Left one", "Left two"]
        );
        // "A" at (60, 360) in 18 pt on a 400 pt page: pixels (60, 40), in
        // units of 1/64 pixel.
        let first = &layer.lines[0].words[0];
        assert_eq!(first.text, "A");
        assert_eq!(first.baseline[..2], [60 * 64, 40 * 64]);
        assert_eq!(first.baseline[3], 40 * 64);
        assert!(first.baseline[2] > first.baseline[0]);
        assert_eq!(first.size, 18 * 64);
    }

    #[test]
    fn the_text_layer_follows_the_page_rotation() {
        // Page 1 is rotated by 90 degrees: horizontal text runs down.
        let layer = layer("page-tree.pdf", 0);
        let word = &layer.lines[0].words[0];
        assert_eq!(word.baseline[0], word.baseline[2]);
        assert!(word.baseline[3] > word.baseline[1]);
    }

    #[test]
    fn a_large_text_layer_is_truncated_within_the_bound() {
        // About 120 000 words: more than the response may hold.
        let line = "(w w w w w w w w w w) Tj T* ".repeat(12_000);
        let content = format!("BT /F1 1 Tf 1 TL 0 0 Td {line} ET");
        let bodies = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (index, body) in bodies.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        let path =
            std::env::temp_dir().join(format!("castle-text-layer-{}.pdf", std::process::id()));
        std::fs::write(&path, pdf).unwrap();
        let document = open(&path.to_string_lossy(), &mut budget()).unwrap();
        let _ = std::fs::remove_file(&path);
        let response = page_text(&document, 0, 1, &mut budget());
        let Ok(response @ Response::PageText { .. }) = response else {
            panic!("text layer expected, got {response:?}");
        };
        let Response::PageText { layer, .. } = &response else {
            unreachable!()
        };
        assert!(layer.truncated);
        let encoded = response.encode().len();
        assert!(encoded <= MAX_TEXT_LAYER_BYTES, "{encoded}");
        assert!(encoded > MAX_TEXT_LAYER_BYTES - 64, "{encoded}");
        assert_eq!(Response::decode(&response.encode()), Ok(response.clone()));
    }
}
