//! Page rendering: content stream interpretation (ISO 32000-2, 8) onto a
//! rasteriser behind an internal adapter (ADR-0003).
//!
//! Supported: graphics state (`q Q cm w J j M d ri i gs`), paths, painting,
//! clipping, device colours (`DeviceGray`, `DeviceRGB`, naive `DeviceCMYK`),
//! constant alpha, text in embedded TrueType, OpenType and CFF fonts or their
//! replacements and in Type 3 fonts, images (XObjects and inline, ADR-0005),
//! and form XObjects with transparency groups at a constant alpha. Text in
//! symbolic fonts other than Symbol and ZapfDingbats, shadings, patterns, other colour spaces, blend modes, knockout
//! groups, soft masks, transfer functions and optional content are not
//! rendered yet: when a page uses them, the rendering is
//! partial and `Rendering::skipped` lists them, so that the caller can say
//! so. Content syntax errors are refused, not repaired (ADR-0002).
//!
//! `render_area` renders one rectangle of the page (a tile of a viewer)
//! with exactly the pixels of that rectangle of `render_page`: both
//! rasterise the same cells of `CELL_SIDE` pixels (ADR-0003 amendment).
//!
//! ```
//! use pdf_document::Document;
//! use pdf_render::{RenderOptions, render_page};
//! use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};
//!
//! # let pdf = include_bytes!("../../../tests/corpus/public/shapes.pdf");
//! let mut budget = Budget::new(ResourceLimits::default(), CancellationToken::new());
//! let document = Document::open(MemorySource::from_vec(pdf.to_vec(), &ResourceLimits::default())?, &mut budget)?;
//! let page = document.page(0).ok_or("no page")?;
//! let mut options = RenderOptions::default();
//! options.scale = 2.0;
//! let rendering = render_page(document.file(), page, &options, &mut budget)?;
//! assert_eq!(rendering.bitmap.width(), 400);
//! assert!(rendering.skipped.is_empty());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod content;
mod error;
mod fonts;
mod geometry;
mod image;
mod interpret;
mod raster;
mod text;

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use pdf_document::Page;
use pdf_syntax::{Budget, IndirectValue, Object, PdfFile, Source};

pub use content::MAX_OPERANDS;

/// Largest bitmap width or height, in pixels. `vello_cpu` 0.3 computes tile
/// and wide-tile (256 px) positions in `u16` and panics on overflow near
/// 65 535 (seen at 65 535 and 65 532): stay far below. Larger views are
/// rendered as tiles.
pub const MAX_SIDE: u16 = 16_384;

/// Most entries in a dash array (`d` operator or `/D` of an `ExtGState`);
/// a longer array is an `InvalidOperands` or `InvalidResource` error.
pub const MAX_DASH_ENTRIES: usize = 64;
pub use error::{ContentErrorKind, Error, RenderLimitKind};
pub use fonts::{
    CidEncoding, FontKind, MAX_PAGE_FONTS, PageTextProfile, TextSource, page_font_kinds,
    page_text_profile,
};

use geometry::Matrix;
use interpret::Interpreter;
use raster::{Canvas, cell_pixels};

/// Limits of one page rendering, on top of the document `Budget`.
///
/// Non-exhaustive: start from `RenderLimits::default()` and adjust fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RenderLimits {
    /// Pixels rasterised (4 bytes each): those of the page for
    /// `render_page`, those of the cells an area touches for `render_area`
    /// (the area itself when it is aligned on the cell grid).
    pub max_pixels: u64,
    /// Operators interpreted, each segment of a glyph drawn counting as one
    /// (as the path operator that would draw it).
    pub max_operations: u64,
    /// `q` nesting.
    pub max_state_depth: u32,
    /// Estimated rasteriser work for the whole page: per segment, one unit
    /// plus the tiles it crosses (a quarter of its device extent within the
    /// canvas) and, for a curve, the square root of its device length over
    /// the flattening tolerance; per painted path or clip, its device area
    /// within the canvas over 1024; strokes three times their padded path,
    /// plus four per dash. Flattening counts once per cell of `CELL_SIDE`
    /// pixels the path may touch, each cell flattening it again. This
    /// bounds the rasteriser's time and memory, which keeps every painted
    /// path until the page is complete.
    pub max_path_work: u64,
    /// Clip paths active at once (each `W n` adds one until its `Q`).
    pub max_clips: u32,
    /// Segments of one glyph outline.
    pub max_glyph_segments: u64,
    /// Pixels of one image (width × height), checked before decoding: about
    /// 8 bytes each while it is converted and drawn. A larger image is not
    /// decoded but reported (`Unsupported::Image`).
    pub max_image_pixels: u64,
}

impl Default for RenderLimits {
    /// Conservative starting values, to be calibrated with the corpus.
    fn default() -> Self {
        Self {
            max_pixels: 32 * 1024 * 1024,
            max_operations: 10_000_000,
            max_state_depth: 256,
            max_path_work: 50_000_000,
            max_clips: 1024,
            max_glyph_segments: 100_000,
            max_image_pixels: 32 * 1024 * 1024,
        }
    }
}

/// How to render a page.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct RenderOptions {
    /// Device pixels per PDF unit: 1.0 renders at 72 pixels per inch.
    pub scale: f64,
    pub limits: RenderLimits,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            scale: 1.0,
            limits: RenderLimits::default(),
        }
    }
}

/// A capability used by the page but not rendered: the rendering is partial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Unsupported {
    /// Text not drawn: a font that cannot be read or replaced (a symbolic
    /// font other than Symbol and ZapfDingbats, a CID font not embedded,
    /// a damaged Type 3 font), a font set by an `ExtGState`, a glyph the
    /// font program does not have or cannot draw, or a Type 3 glyph
    /// procedure that is damaged or drawn only up to an error.
    Text,
    /// Text rendering modes 4 to 7 (text added to the clip): what follows is
    /// painted without that clip, so the rendering may show too much.
    TextClip,
    /// An image (XObject or inline) not drawn: a filter not decoded yet
    /// (JPX, JBIG2, CCITT, LZW...), damaged data, or a size over
    /// `max_image_pixels`.
    Image,
    /// An XObject not drawn, or a form drawn only up to an error: not a
    /// readable stream, an unknown subtype (PostScript), a form whose
    /// dictionary or content is damaged, nested deeper than 16 forms or
    /// inside itself.
    XObject,
    /// Shadings painted with `sh`.
    Shading,
    /// Painting with a pattern colour space.
    Pattern,
    /// Painting with a colour space other than the device ones, or with a
    /// device one replaced by a `DefaultGray`, `DefaultRGB` or `DefaultCMYK`
    /// resource.
    ColorSpace,
    /// Compositing not reproduced: a blend mode other than `Normal`, or a
    /// knockout transparency group (drawn as a non-knockout one).
    BlendMode,
    /// A soft mask in an `ExtGState`: content is painted unmasked, so the
    /// rendering may show too much.
    SoftMask,
    /// `/AIS true` in an `ExtGState`.
    AlphaIsShape,
    /// A transfer function in an `ExtGState`.
    TransferFunction,
    /// Optional content (`BDC /OC`): content possibly hidden is drawn.
    OptionalContent,
    /// Text drawn with a replacement font: the PDF names a font it does
    /// not embed (one of the 14 standard fonts, for instance), or embeds a
    /// Type 1 or damaged program. Positions and widths are the PDF's; the
    /// glyphs only resemble the original ones (ADR-0005).
    FontSubstituted,
}

/// RGBA8 pixels with straight alpha, rows from top to bottom.
#[derive(Clone, PartialEq, Eq)]
pub struct Bitmap {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl Bitmap {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// `width * height * 4` bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn into_data(self) -> Vec<u8> {
        self.data
    }

    /// The pixel at `(x, y)`, if inside the bitmap.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = (u64::from(y) * u64::from(self.width) + u64::from(x)) * 4;
        let index = usize::try_from(index).ok()?;
        let bytes = self.data.get(index..index.checked_add(4)?)?;
        bytes.try_into().ok()
    }
}

/// Never prints pixels: they show document content.
impl fmt::Debug for Bitmap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bitmap")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// A rectangle of device pixels in the rendering of a whole page at a given
/// scale: `x` to the right and `y` down from the top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Side in pixels of the square cells a page rendering is rasterised in, on
/// a grid fixed to its top-left corner. A `render_area` aligned on this
/// grid, a viewer's tile of this side, rasterises nothing else.
pub const CELL_SIDE: u32 = raster::CELL;

/// A rendered page and what it could not render.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Rendering {
    pub bitmap: Bitmap,
    /// Empty when every operator of the page was rendered.
    pub skipped: BTreeSet<Unsupported>,
}

/// Renders `page` of `file`: its crop box, rotated by `/Rotate`, at
/// `options.scale` device pixels per 1/72 inch, on a white background. A
/// page with a `/UserUnit` other than 1 is scaled accordingly.
///
/// `budget` is charged by the decoded content and the resources read. The
/// output size is `ceil(width * scale * user_unit)` by the same for the
/// height (swapped for 90 and 270 degrees), bounded by `max_pixels` and
/// `MAX_SIDE` per side, checked before any allocation. Rasterisation itself,
/// after the content has been interpreted, is not cancellable; its cost is
/// bounded by `RenderLimits::max_path_work`.
pub fn render_page<S: Source>(
    file: &PdfFile<S>,
    page: &Page,
    options: &RenderOptions,
    budget: &mut Budget,
) -> Result<Rendering, Error> {
    render_page_cached(file, page, options, &mut RenderCache::default(), budget)
}

/// `render_page`, reusing and filling `cache` (see `RenderCache`).
pub fn render_page_cached<S: Source>(
    file: &PdfFile<S>,
    page: &Page,
    options: &RenderOptions,
    cache: &mut RenderCache,
    budget: &mut Budget,
) -> Result<Rendering, Error> {
    let (width, height, base) = page_geometry(page, options)?;
    check_pixels(u64::from(width) * u64::from(height), options)?;
    let canvas = Canvas::new((width, height), 0, 0, width, height);
    render(file, page, options, canvas, base, cache, budget)
}

/// What the renders of one document reuse from one another: fonts, glyph
/// outlines, decoded images, forms, Type 3 glyph procedures and decoded
/// page content, all read from objects that do not change once the
/// document is open. Viewers render a page in many tiles, each of which
/// would otherwise read and decode them again.
///
/// A cache serves one document only (its objects are keyed by number): give
/// each open document its own. What it keeps counts against `max_bytes`
/// (decoded pixels, content and programs; outlines by segment); what does
/// not fit is used once and not kept. A render that starts with the cache
/// more than three quarters full empties it first, so that it refills with
/// what is viewed now; so do renders with other `RenderLimits`, under which
/// what was kept (or refused) might differ. Results are the same with or
/// without a cache, only faster.
pub struct RenderCache {
    caches: interpret::Caches,
    limits: Option<RenderLimits>,
}

impl RenderCache {
    /// The default bound of one document's cache.
    pub const DEFAULT_BYTES: usize = 64 * 1024 * 1024;

    /// An empty cache that keeps at most `max_bytes`; 0 keeps nothing.
    pub fn new(max_bytes: usize) -> Self {
        Self {
            caches: interpret::Caches::new(max_bytes),
            limits: None,
        }
    }

    /// Bytes kept now.
    pub fn bytes(&self) -> usize {
        self.caches.bytes()
    }

    /// Empties the cache: for instance, to free its memory.
    pub fn clear(&mut self) {
        self.caches.clear();
    }

    /// Readies the cache for a render under `limits`.
    fn start(&mut self, limits: &RenderLimits) {
        let full = self.caches.bytes() > self.caches.max_bytes / 4 * 3;
        if full || self.limits.as_ref() != Some(limits) {
            self.caches.clear();
            self.limits = Some(*limits);
        }
    }
}

impl Default for RenderCache {
    fn default() -> Self {
        Self::new(Self::DEFAULT_BYTES)
    }
}

impl fmt::Debug for RenderCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RenderCache")
            .field("bytes", &self.caches.bytes())
            .field("max_bytes", &self.caches.max_bytes)
            .finish()
    }
}

/// Size in device pixels of the whole page rendered at `options.scale`:
/// the size of `render_page`'s bitmap, without its `max_pixels` check.
/// Each side is at most `MAX_SIDE`.
pub fn page_size(page: &Page, options: &RenderOptions) -> Result<(u32, u32), Error> {
    let (width, height, _) = page_geometry(page, options)?;
    Ok((u32::from(width), u32::from(height)))
}

/// Renders `area` of the page rendering at `options.scale` (see
/// `page_size`): the bitmap is `area.width` by `area.height` and holds
/// exactly the pixels of that rectangle of `render_page`, so tiles join
/// without seams. Both rasterise the same cells of 256 pixels, on a grid
/// fixed to the top-left corner of the page, each from the page's
/// coordinates: an area aligned on that grid (a viewer tile) costs no more
/// than its size, any other is rendered on the cells it touches.
/// `max_pixels` applies to the cells the area touches, not to the page; the
/// area must be non-empty and inside the page, otherwise `InvalidRequest`.
/// The whole content is interpreted; the rasteriser work outside the cells
/// is culled and not counted.
pub fn render_area<S: Source>(
    file: &PdfFile<S>,
    page: &Page,
    options: &RenderOptions,
    area: PixelRect,
    budget: &mut Budget,
) -> Result<Rendering, Error> {
    render_area_cached(
        file,
        page,
        options,
        area,
        &mut RenderCache::default(),
        budget,
    )
}

/// `render_area`, reusing and filling `cache` (see `RenderCache`).
pub fn render_area_cached<S: Source>(
    file: &PdfFile<S>,
    page: &Page,
    options: &RenderOptions,
    area: PixelRect,
    cache: &mut RenderCache,
    budget: &mut Budget,
) -> Result<Rendering, Error> {
    let (page_width, page_height, base) = page_geometry(page, options)?;
    let inside = |start: u32, length: u32, side: u16| {
        length > 0
            && start
                .checked_add(length)
                .is_some_and(|end| end <= u32::from(side))
    };
    if !(inside(area.x, area.width, page_width) && inside(area.y, area.height, page_height)) {
        return Err(Error::InvalidRequest);
    }
    // Within the page, so each side fits `u16`.
    let (Ok(width), Ok(height)) = (u16::try_from(area.width), u16::try_from(area.height)) else {
        return Err(Error::InvalidRequest);
    };
    let size = (page_width, page_height);
    check_pixels(cell_pixels(size, area.x, area.y, width, height), options)?;
    let canvas = Canvas::new(size, area.x, area.y, width, height);
    render(file, page, options, canvas, base, cache, budget)
}

fn check_pixels(pixels: u64, options: &RenderOptions) -> Result<(), Error> {
    if pixels > options.limits.max_pixels {
        return Err(Error::LimitExceeded {
            kind: RenderLimitKind::Pixels,
            limit: options.limits.max_pixels,
        });
    }
    Ok(())
}

fn render<S: Source>(
    file: &PdfFile<S>,
    page: &Page,
    options: &RenderOptions,
    canvas: Canvas,
    base: Matrix,
    cache: &mut RenderCache,
    budget: &mut Budget,
) -> Result<Rendering, Error> {
    cache.start(&options.limits);
    let (width, height) = canvas.size();
    let content = match cache.caches.contents.get(&page.id) {
        Some(content) => Arc::clone(content),
        None => {
            let content = Arc::new(page_content(file, page, budget)?);
            if cache.caches.reserve(content.len()) {
                cache.caches.contents.insert(page.id, Arc::clone(&content));
            }
            content
        }
    };
    let resources = match page.resources.as_deref() {
        None => None,
        Some(object) => Some(resource_dictionary(file, object, page, budget)?),
    };
    let caches = std::mem::take(&mut cache.caches);
    let interpreter = Interpreter::new(
        file,
        page.id,
        resources,
        options.limits,
        canvas,
        base,
        caches,
    );
    let (result, caches) = interpreter.run(&content, budget);
    cache.caches = caches;
    let (canvas, skipped) = result?;
    Ok(Rendering {
        bitmap: Bitmap {
            width: u32::from(width),
            height: u32::from(height),
            data: canvas.finish(),
        },
        skipped,
    })
}

/// Page size in pixels and the matrix from default user space to device
/// pixels (y down, origin at the top-left corner of the rotated crop box).
/// Each side is at most `MAX_SIDE`; `max_pixels` is checked by the callers.
fn page_geometry(page: &Page, options: &RenderOptions) -> Result<(u16, u16, Matrix), Error> {
    let scale = options.scale * page.user_unit;
    let crop = page.crop_box;
    let (w, h) = (crop.width(), crop.height());
    if !(options.scale.is_finite() && options.scale > 0.0) {
        return Err(Error::InvalidRequest);
    }
    if !(scale.is_finite() && scale > 0.0 && w > 0.0 && h > 0.0) {
        return Err(Error::InvalidRequest);
    }
    let (rotated_w, rotated_h) = if page.rotation % 180 == 90 {
        (h, w)
    } else {
        (w, h)
    };
    let too_large = Error::LimitExceeded {
        kind: RenderLimitKind::Pixels,
        limit: options.limits.max_pixels,
    };
    let side = |length: f64| {
        let pixels = (length * scale).ceil();
        // In range of u16 once checked, so `as` is exact.
        (pixels.is_finite() && pixels <= f64::from(MAX_SIDE)).then_some((pixels as u16).max(1))
    };
    let (Some(width), Some(height)) = (side(rotated_w), side(rotated_h)) else {
        return Err(too_large);
    };
    let s = scale;
    let (x0, y0, x1, y1) = (crop.llx, crop.lly, crop.urx, crop.ury);
    let base = match page.rotation {
        90 => Matrix::new(0.0, s, s, 0.0, -y0 * s, -x0 * s),
        180 => Matrix::new(-s, 0.0, 0.0, s, x1 * s, -y0 * s),
        270 => Matrix::new(0.0, -s, -s, 0.0, y1 * s, x1 * s),
        _ => Matrix::new(s, 0.0, 0.0, -s, -x0 * s, y1 * s),
    };
    Ok((width, height, base))
}

/// The page's content streams decoded and concatenated, each followed by a
/// newline so that tokens never join across streams (ISO 32000-2, 7.8.2).
/// A null or absent stream contributes nothing.
fn page_content<S: Source>(
    file: &PdfFile<S>,
    page: &Page,
    budget: &mut Budget,
) -> Result<Vec<u8>, Error> {
    let invalid = || {
        Error::Document(pdf_document::Error::InvalidStructure {
            object: page.id,
            kind: pdf_document::StructureErrorKind::InvalidContents,
        })
    };
    let references_in = |items: &[Object]| -> Result<Vec<_>, Error> {
        items
            .iter()
            .map(Object::as_reference)
            .collect::<Option<_>>()
            .ok_or_else(invalid)
    };
    // A stream reference, an array of them, or a reference to such an array
    // (ISO 32000-2, 7.7.3.3); the referenced array is resolved once, so a
    // reference to an array of arrays, or to itself, is refused below.
    let mut single = None;
    let references: Vec<_> = match &page.contents {
        None | Some(Object::Null) => Vec::new(),
        Some(Object::Reference(id)) => match file.resolve(*id, budget)? {
            IndirectValue::Object(Object::Array(items)) => references_in(&items)?,
            value => {
                single = Some(value);
                Vec::new()
            }
        },
        Some(Object::Array(items)) => references_in(items)?,
        Some(_) => return Err(invalid()),
    };
    let mut parts = Vec::with_capacity(references.len().max(1));
    let mut push = |value, budget: &mut Budget| -> Result<(), Error> {
        match value {
            IndirectValue::Stream(stream) => parts.push(file.decoded_stream_data(&stream, budget)?),
            IndirectValue::Object(Object::Null) => {}
            IndirectValue::Object(_) => return Err(invalid()),
        }
        Ok(())
    };
    if let Some(value) = single {
        push(value, budget)?;
    }
    for id in references {
        let value = file.resolve(id, budget)?;
        push(value, budget)?;
    }
    // One stream is used in place. Several are joined into a buffer of the
    // exact size, charged to the budget like the decoded data it copies.
    if parts.len() == 1
        && let Some(mut content) = parts.pop()
    {
        content.push(b'\n');
        return Ok(content);
    }
    let total: usize = parts.iter().map(|part| part.len() + 1).sum();
    budget.charge_decoded_bytes(total as u64)?;
    let mut content = Vec::with_capacity(total);
    for part in parts {
        content.extend_from_slice(&part);
        content.push(b'\n');
    }
    Ok(content)
}

fn resource_dictionary<S: Source>(
    file: &PdfFile<S>,
    object: &Object,
    page: &Page,
    budget: &mut Budget,
) -> Result<pdf_syntax::Dictionary, Error> {
    let invalid = || {
        Error::Document(pdf_document::Error::InvalidStructure {
            object: page.id,
            kind: pdf_document::StructureErrorKind::InvalidResources,
        })
    };
    let resolved = match object {
        Object::Reference(id) => match file.resolve(*id, budget)? {
            IndirectValue::Object(object) => object,
            IndirectValue::Stream(_) => return Err(invalid()),
        },
        other => other.clone(),
    };
    match resolved {
        Object::Dictionary(dict) => Ok(dict),
        _ => Err(invalid()),
    }
}
