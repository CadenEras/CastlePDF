//! Content stream interpretation (ISO 32000-2, 8): graphics state, paths,
//! painting, clipping and device colours. Capabilities not implemented yet
//! are skipped and reported in `Rendering::skipped`, never silently.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use pdf_syntax::{Budget, Dictionary, IndirectValue, Object, ObjectId, PdfFile, Source, Stream};

use crate::content::{ContentReader, Operation};
use crate::error::{ContentErrorKind, Error, RenderLimitKind};
use crate::geometry::{Matrix, Path, Point, Segment, Window};
use crate::image;
use crate::raster::{self, Canvas, HAIRLINE, LineCap, LineJoin, Rgba, StrokeStyle};
use crate::text::{self, Selected, TextState, font, type3};
use crate::{MAX_DASH_ENTRIES, RenderLimits, Unsupported};

/// Most glyph outline segments kept in the cache of one rendering (about
/// 56 bytes each); outlines beyond are drawn but not kept.
const MAX_CACHED_SEGMENTS: usize = 1_000_000;
/// Bytes of decoded images kept by one rendering; images beyond are decoded
/// again when drawn again.
const MAX_CACHED_IMAGE_BYTES: usize = 128 * 1024 * 1024;
/// Most form XObjects drawn inside one another (8.10); a deeper form is
/// skipped and reported (`XObject`).
const MAX_FORM_DEPTH: usize = 16;
/// Bytes of decoded form content kept by one rendering; forms beyond are
/// read and decoded again when drawn again.
const MAX_CACHED_FORM_BYTES: usize = 64 * 1024 * 1024;
/// Scopes of forms with their own resources kept by one rendering, so that
/// a form drawn again reuses what it read (fonts, colour spaces...).
const MAX_FORM_SCOPES: usize = 256;
/// Cancellation is checked every this many codes shown.
const CANCEL_CHECK_CODES: u64 = 4096;

/// Largest device coordinate accepted, padding of strokes included: far
/// beyond any canvas (65 535 pixels), yet small enough for the rasteriser's
/// single-precision arithmetic to stay finite.
const MAX_DEVICE_COORDINATE: f64 = 1e12;

/// Most colour components of a non-device colour space (DeviceN, 8.6.6.5).
const MAX_COMPONENTS: usize = 32;

/// Colour space of a paint (ISO 32000-2, 8.6).
#[derive(Debug, Clone, Copy, PartialEq)]
enum ColorSpace {
    Gray,
    Rgb,
    Cmyk,
    /// Pattern colour space: painting is skipped and reported.
    Pattern,
    /// Any other family, or a device family replaced by a `Default` colour
    /// space resource: painting is skipped and reported.
    Other,
}

impl ColorSpace {
    fn components(self) -> Option<usize> {
        match self {
            Self::Gray => Some(1),
            Self::Rgb => Some(3),
            Self::Cmyk => Some(4),
            Self::Pattern | Self::Other => None,
        }
    }

    /// Initial colour of the space (ISO 32000-2, 8.6.8, table 73): black.
    fn initial(self) -> [f32; 4] {
        match self {
            Self::Cmyk => [0.0, 0.0, 0.0, 1.0],
            _ => [0.0; 4],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Paint {
    space: ColorSpace,
    components: [f32; 4],
    alpha: f32,
}

impl Paint {
    const BLACK: Self = Self {
        space: ColorSpace::Gray,
        components: [0.0; 4],
        alpha: 1.0,
    };

    /// Device RGB. CMYK uses the conversion of ISO 32000-2, 10.4.2.4
    /// (`red = 1 - min(1, cyan + black)`...), without colour management.
    fn rgba(&self) -> Result<Rgba, Unsupported> {
        let [c0, c1, c2, c3] = self.components;
        let (r, g, b) = match self.space {
            ColorSpace::Gray => (c0, c0, c0),
            ColorSpace::Rgb => (c0, c1, c2),
            ColorSpace::Cmyk => (
                1.0 - (c0 + c3).min(1.0),
                1.0 - (c1 + c3).min(1.0),
                1.0 - (c2 + c3).min(1.0),
            ),
            ColorSpace::Pattern => return Err(Unsupported::Pattern),
            ColorSpace::Other => return Err(Unsupported::ColorSpace),
        };
        Ok(Rgba {
            r,
            g,
            b,
            a: self.alpha,
        })
    }
}

#[derive(Debug, Clone)]
struct GraphicsState {
    ctm: Matrix,
    fill: Paint,
    stroke: Paint,
    style: StrokeStyle,
    text: TextState,
    /// Clip paths pushed on the canvas since the matching `q`.
    clips: u32,
}

/// One parameter of an `ExtGState` that this renderer applies.
#[derive(Debug, Clone)]
enum Setting {
    Width(f64),
    Cap(LineCap),
    Join(LineJoin),
    MiterLimit(f64),
    Dash(Vec<f64>, f64),
    StrokeAlpha(f32),
    FillAlpha(f32),
}

/// An `ExtGState` read once: what it sets and what it uses but is not
/// rendered.
#[derive(Debug, Clone, Default)]
struct ExtGState {
    settings: Vec<Setting>,
    skipped: Vec<Unsupported>,
}

/// What names in a content stream refer to: a resource dictionary and what
/// was read from it. A form XObject with its own `/Resources` gets a scope
/// of its own (8.10.1); caches keyed by object (fonts, glyphs, images,
/// forms) stay shared.
#[derive(Default)]
struct Scope {
    resources: Option<Dictionary>,
    categories: BTreeMap<&'static [u8], Dictionary>,
    color_spaces: BTreeMap<Vec<u8>, ColorSpace>,
    ext_g_states: BTreeMap<Vec<u8>, ExtGState>,
    fonts: BTreeMap<Vec<u8>, Selected>,
}

/// A form XObject as read once (8.10.1): what drawing it needs.
struct Form {
    matrix: Matrix,
    bbox: [f64; 4],
    /// Its own resources; `None`: those of the enclosing content.
    resources: Option<Dictionary>,
    /// A transparency group (11.6.6): `Some(knockout)`.
    group: Option<bool>,
    /// `/Group` present but not a readable dictionary.
    damaged_group: bool,
    /// `/OC`: optional content, possibly hidden, is drawn.
    optional: bool,
    content: Arc<Vec<u8>>,
}

/// Interprets the content of one page onto a canvas.
pub(crate) struct Interpreter<'a, S> {
    file: &'a PdfFile<S>,
    page: ObjectId,
    resources: Option<Dictionary>,
    limits: RenderLimits,
    canvas: Canvas,
    /// The pixels the canvas covers, in the page rendering, for work estimates.
    window: Window,
    base: Matrix,
    state: GraphicsState,
    saved: Vec<GraphicsState>,
    path: Path,
    /// Set by `W` (`Some(false)`) or `W*` (`Some(true)` for even-odd).
    pending_clip: Option<bool>,
    active_clips: u32,
    in_text: bool,
    marked_content: u32,
    compatibility: u32,
    operations: u64,
    work: f64,
    skipped: BTreeSet<Unsupported>,
    /// Resource category dictionaries, resolved once.
    categories: BTreeMap<&'static [u8], Dictionary>,
    color_spaces: BTreeMap<Vec<u8>, ColorSpace>,
    ext_g_states: BTreeMap<Vec<u8>, ExtGState>,
    /// Text matrix and text line matrix (9.4.2), set by `BT`.
    text_matrix: Matrix,
    line_matrix: Matrix,
    /// Fonts by resource name, loaded once.
    fonts: BTreeMap<Vec<u8>, Selected>,
    /// The same fonts by object, so that two names of one font share it.
    font_objects: BTreeMap<ObjectId, Selected>,
    /// Fonts loaded, numbered in load order: the number keys the glyph cache.
    loaded_fonts: usize,
    /// Glyph outlines in font units, by font number and glyph id; `None` for
    /// a glyph the program cannot draw.
    glyphs: BTreeMap<(usize, u16), Option<Arc<Vec<Segment>>>>,
    cached_segments: usize,
    /// Images decoded in this rendering, by object and stencil colour; an
    /// image that cannot be drawn keeps the capability it lacks.
    images: BTreeMap<(ObjectId, [u8; 3]), Result<Arc<vello_cpu::Pixmap>, Unsupported>>,
    cached_image_bytes: usize,
    /// Forms being drawn, outermost first: a form inside itself is skipped.
    forms: Vec<ObjectId>,
    /// Saved states below this depth belong to the page or an enclosing
    /// form: a `Q` cannot restore them.
    floor: usize,
    /// Forms read in this rendering; `None` for one that cannot be drawn.
    forms_read: BTreeMap<ObjectId, Option<Arc<Form>>>,
    cached_form_bytes: usize,
    /// Scopes of the forms with their own resources, between two draws.
    form_scopes: BTreeMap<ObjectId, Scope>,
    /// Scopes of the Type 3 fonts with their own resources, by font number.
    type3_scopes: BTreeMap<usize, Scope>,
    /// Decoded Type 3 glyph procedures, counted with the forms' content;
    /// `None` for one that cannot be read.
    glyph_contents: BTreeMap<ObjectId, Option<Arc<Vec<u8>>>>,
    /// The next operation is the first of a Type 3 glyph procedure: only
    /// there are `d0` and `d1` allowed (9.6.4).
    glyph_start: bool,
    /// The glyph began with `d1`: it has no colour of its own, colour
    /// operators are ignored (9.6.4).
    uncolored: bool,
}

/// How nested content (a form, a Type 3 glyph) is entered.
struct Nested {
    /// Its current transformation matrix.
    ctm: Matrix,
    /// A clip (its bounding box) and its matrix to device space.
    clip: Option<(Path, Matrix)>,
    /// A transparency group: alphas start at 1; `Some(alpha)` composites
    /// it as one layer at that opacity.
    group: Option<Option<f32>>,
    /// Its own resource scope.
    scope: Option<Scope>,
}

impl<'a, S: Source> Interpreter<'a, S> {
    pub fn new(
        file: &'a PdfFile<S>,
        page: ObjectId,
        resources: Option<Dictionary>,
        limits: RenderLimits,
        canvas: Canvas,
        base: Matrix,
    ) -> Self {
        let window = canvas.window();
        Self {
            file,
            page,
            resources,
            limits,
            canvas,
            window,
            base,
            state: GraphicsState {
                ctm: Matrix::IDENTITY,
                fill: Paint::BLACK,
                stroke: Paint::BLACK,
                style: StrokeStyle {
                    width: 1.0,
                    cap: LineCap::Butt,
                    join: LineJoin::Miter,
                    miter_limit: 10.0,
                    dashes: Vec::new(),
                    dash_phase: 0.0,
                },
                text: TextState::default(),
                clips: 0,
            },
            saved: Vec::new(),
            path: Path::default(),
            pending_clip: None,
            active_clips: 0,
            in_text: false,
            marked_content: 0,
            compatibility: 0,
            operations: 0,
            work: 0.0,
            skipped: BTreeSet::new(),
            categories: BTreeMap::new(),
            color_spaces: BTreeMap::new(),
            ext_g_states: BTreeMap::new(),
            text_matrix: Matrix::IDENTITY,
            line_matrix: Matrix::IDENTITY,
            fonts: BTreeMap::new(),
            font_objects: BTreeMap::new(),
            loaded_fonts: 0,
            glyphs: BTreeMap::new(),
            cached_segments: 0,
            images: BTreeMap::new(),
            cached_image_bytes: 0,
            forms: Vec::new(),
            floor: 0,
            forms_read: BTreeMap::new(),
            cached_form_bytes: 0,
            form_scopes: BTreeMap::new(),
            type3_scopes: BTreeMap::new(),
            glyph_contents: BTreeMap::new(),
            glyph_start: false,
            uncolored: false,
        }
    }

    /// Runs every operation of `content`, then returns the canvas and the
    /// capabilities skipped. The content must end outside any text object,
    /// path, `q`, marked content or compatibility section (ISO 32000-2,
    /// 7.8.2, 8.4.2, 14.6).
    pub fn run(
        mut self,
        content: &[u8],
        budget: &mut Budget,
    ) -> Result<(Canvas, BTreeSet<Unsupported>), Error> {
        // The initial colour spaces are DeviceGray: apply `DefaultGray`.
        self.state.fill.space = self.device(ColorSpace::Gray, 0, budget)?;
        self.state.stroke.space = self.state.fill.space;
        self.run_content(content, budget)?;
        if self.in_text
            || !self.path.is_empty()
            || self.pending_clip.is_some()
            || !self.saved.is_empty()
            || self.marked_content > 0
            || self.compatibility > 0
        {
            return Err(self.error(content.len() as u64, ContentErrorKind::Unterminated));
        }
        Ok((self.canvas, self.skipped))
    }

    /// Executes every operation of `content`, counted against
    /// `max_operations` with those of the page and of every form.
    fn run_content(&mut self, content: &[u8], budget: &mut Budget) -> Result<(), Error> {
        let mut reader = ContentReader::new(content, self.page);
        while let Some(operation) = reader.next(budget)? {
            self.operations += 1;
            if self.operations > self.limits.max_operations {
                return Err(Error::LimitExceeded {
                    kind: RenderLimitKind::Operations,
                    limit: self.limits.max_operations,
                });
            }
            self.execute(&operation, budget)?;
        }
        Ok(())
    }

    fn error(&self, offset: u64, kind: ContentErrorKind) -> Error {
        content_error(self.page, offset, kind)
    }

    /// `q`: saves the graphics state.
    fn save(&mut self) -> Result<(), Error> {
        let depth = u32::try_from(self.saved.len()).unwrap_or(u32::MAX);
        if depth >= self.limits.max_state_depth {
            return Err(Error::LimitExceeded {
                kind: RenderLimitKind::StateDepth,
                limit: u64::from(self.limits.max_state_depth),
            });
        }
        let mut saved = self.state.clone();
        saved.clips = 0;
        let current = std::mem::replace(&mut self.state, saved);
        self.saved.push(current);
        Ok(())
    }

    /// `Q`: restores the last saved state and removes the clips set
    /// since; `false` if no state above `floor` is saved.
    fn restore(&mut self) -> bool {
        if self.saved.len() <= self.floor {
            return false;
        }
        let Some(previous) = self.saved.pop() else {
            return false;
        };
        for _ in 0..self.state.clips {
            self.canvas.pop_clip();
        }
        self.active_clips -= self.state.clips;
        self.state = previous;
        true
    }

    /// Exchanges the current scope with `scope`.
    fn swap_scope(&mut self, scope: &mut Scope) {
        std::mem::swap(&mut self.resources, &mut scope.resources);
        std::mem::swap(&mut self.categories, &mut scope.categories);
        std::mem::swap(&mut self.color_spaces, &mut scope.color_spaces);
        std::mem::swap(&mut self.ext_g_states, &mut scope.ext_g_states);
        std::mem::swap(&mut self.fonts, &mut scope.fonts);
    }

    fn execute(&mut self, op: &Operation<'_>, budget: &mut Budget) -> Result<(), Error> {
        let at = op.offset;
        let glyph_start = std::mem::take(&mut self.glyph_start);
        let ops = op.operands;
        let page = self.page;
        let bad = move || content_error(page, at, ContentErrorKind::InvalidOperands);
        let misplaced = move || content_error(page, at, ContentErrorKind::MisplacedOperator);
        let in_path = !self.path.is_empty() || self.pending_clip.is_some();

        // Operators allowed at this point (ISO 32000-2, 8.2, figure 9). A
        // painting operator may end an empty path: it paints nothing.
        match category(op.operator) {
            Category::PathConstruction | Category::Clip | Category::Painting if self.in_text => {
                return Err(misplaced());
            }
            Category::PathConstruction | Category::Clip | Category::Painting => {}
            Category::Unknown if self.compatibility > 0 => return Ok(()),
            Category::Unknown => {
                return Err(self.error(at, ContentErrorKind::UnknownOperator));
            }
            _ if in_path => return Err(misplaced()),
            Category::SpecialState | Category::External if self.in_text => {
                return Err(misplaced());
            }
            Category::TextPosition | Category::TextShow if !self.in_text => {
                return Err(misplaced());
            }
            // An uncoloured glyph is painted in the colour of the text.
            Category::Color if self.uncolored => return Ok(()),
            _ => {}
        }

        match op.operator {
            // Special graphics state (8.4.2).
            b"q" => {
                no_operands(ops).ok_or_else(bad)?;
                self.save()?;
            }
            b"Q" => {
                no_operands(ops).ok_or_else(bad)?;
                if !self.restore() {
                    return Err(self.error(at, ContentErrorKind::UnbalancedRestore));
                }
            }
            b"cm" => {
                let [a, b, c, d, e, f] = numbers(ops).ok_or_else(bad)?;
                let ctm = Matrix::new(a, b, c, d, e, f).then(&self.state.ctm);
                if !ctm.is_finite() {
                    return Err(self.error(at, ContentErrorKind::NumberOutOfRange));
                }
                self.state.ctm = ctm;
            }
            // General graphics state (8.4.3).
            b"w" => {
                let [width] = numbers(ops).ok_or_else(bad)?;
                self.state.style.width = (width >= 0.0).then_some(width).ok_or_else(bad)?;
            }
            b"J" => {
                let [cap] = numbers(ops).ok_or_else(bad)?;
                self.state.style.cap = line_cap(cap).ok_or_else(bad)?;
            }
            b"j" => {
                let [join] = numbers(ops).ok_or_else(bad)?;
                self.state.style.join = line_join(join).ok_or_else(bad)?;
            }
            b"M" => {
                let [limit] = numbers(ops).ok_or_else(bad)?;
                self.state.style.miter_limit = (limit > 0.0).then_some(limit).ok_or_else(bad)?;
            }
            b"d" => {
                let [Object::Array(array), phase] = ops else {
                    return Err(bad());
                };
                let (dashes, phase) = dash(array, phase).ok_or_else(bad)?;
                self.state.style.dashes = dashes;
                self.state.style.dash_phase = phase;
            }
            b"ri" => {
                let [Object::Name(_)] = ops else {
                    return Err(bad());
                };
            }
            b"i" => {
                let [flatness] = numbers(ops).ok_or_else(bad)?;
                if !(0.0..=100.0).contains(&flatness) {
                    return Err(bad());
                }
            }
            b"gs" => {
                let [Object::Name(name)] = ops else {
                    return Err(bad());
                };
                let state = self.ext_g_state(name, at, budget)?;
                self.apply(&state);
            }
            // Path construction (8.5.2).
            b"m" => {
                let [x, y] = numbers(ops).ok_or_else(bad)?;
                self.path.move_to(Point::new(x, y));
            }
            b"l" => {
                let [x, y] = numbers(ops).ok_or_else(bad)?;
                let added = self.path.line_to(Point::new(x, y));
                self.need_point(added, at)?;
            }
            b"c" => {
                let [x1, y1, x2, y2, x3, y3] = numbers(ops).ok_or_else(bad)?;
                let added =
                    self.path
                        .curve_to(Point::new(x1, y1), Point::new(x2, y2), Point::new(x3, y3));
                self.need_point(added, at)?;
            }
            b"v" => {
                let [x2, y2, x3, y3] = numbers(ops).ok_or_else(bad)?;
                let current = self
                    .path
                    .current_point()
                    .ok_or_else(|| self.error(at, ContentErrorKind::NoCurrentPoint))?;
                self.path
                    .curve_to(current, Point::new(x2, y2), Point::new(x3, y3));
            }
            b"y" => {
                let [x1, y1, x3, y3] = numbers(ops).ok_or_else(bad)?;
                let end = Point::new(x3, y3);
                let added = self.path.curve_to(Point::new(x1, y1), end, end);
                self.need_point(added, at)?;
            }
            b"h" => {
                no_operands(ops).ok_or_else(bad)?;
                self.path.close();
            }
            b"re" => {
                let [x, y, width, height] = numbers(ops).ok_or_else(bad)?;
                self.path.rectangle(x, y, width, height);
            }
            // Clipping (8.5.4): applies after the painting operator.
            b"W" | b"W*" => {
                no_operands(ops).ok_or_else(bad)?;
                self.pending_clip = Some(op.operator == b"W*");
            }
            // Path painting (8.5.3).
            b"n" | b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" => {
                no_operands(ops).ok_or_else(bad)?;
                let (close, fill, stroke) = match op.operator {
                    b"S" => (false, None, true),
                    b"s" => (true, None, true),
                    b"f" | b"F" => (false, Some(false), false),
                    b"f*" => (false, Some(true), false),
                    b"B" => (false, Some(false), true),
                    b"B*" => (false, Some(true), true),
                    b"b" => (true, Some(false), true),
                    b"b*" => (true, Some(true), true),
                    _ => (false, None, false),
                };
                if close {
                    self.path.close();
                }
                self.paint(at, fill, stroke)?;
            }
            // Colour (8.6.8).
            b"g" | b"G" | b"rg" | b"RG" | b"k" | b"K" => {
                let space = match op.operator {
                    b"g" | b"G" => ColorSpace::Gray,
                    b"rg" | b"RG" => ColorSpace::Rgb,
                    _ => ColorSpace::Cmyk,
                };
                let components = color_components(ops, space).ok_or_else(bad)?;
                let space = self.device(space, at, budget)?;
                let paint = self.paint_mut(op.operator);
                paint.space = space;
                paint.components = components;
            }
            b"cs" | b"CS" => {
                let [Object::Name(name)] = ops else {
                    return Err(bad());
                };
                let space = self.color_space(name, at, budget)?;
                let paint = self.paint_mut(op.operator);
                paint.space = space;
                paint.components = space.initial();
            }
            b"sc" | b"SC" | b"scn" | b"SCN" => {
                let space = self.paint_mut(op.operator).space;
                let named = op.operator.len() == 3;
                match space {
                    ColorSpace::Gray | ColorSpace::Rgb | ColorSpace::Cmyk => {
                        let components = color_components(ops, space).ok_or_else(bad)?;
                        self.paint_mut(op.operator).components = components;
                    }
                    // `scn` with a pattern name, after the components of an
                    // uncoloured pattern (8.7.3.3).
                    ColorSpace::Pattern => {
                        let [components @ .., Object::Name(name)] = ops else {
                            return Err(bad());
                        };
                        if !named || components.len() > MAX_COMPONENTS {
                            return Err(bad());
                        }
                        if components.iter().any(|c| number(c).is_none()) {
                            return Err(bad());
                        }
                        self.require_resource(b"Pattern", name, at, budget)?;
                    }
                    ColorSpace::Other => {
                        if ops.is_empty()
                            || ops.len() > MAX_COMPONENTS
                            || ops.iter().any(|c| number(c).is_none())
                        {
                            return Err(bad());
                        }
                    }
                }
            }
            // Text (9.3, 9.4). Fonts read so far: embedded TrueType
            // (ADR-0005); other fonts set `Text`.
            b"BT" => {
                no_operands(ops).ok_or_else(bad)?;
                if self.in_text {
                    return Err(misplaced());
                }
                self.in_text = true;
                self.text_matrix = Matrix::IDENTITY;
                self.line_matrix = Matrix::IDENTITY;
            }
            b"ET" => {
                no_operands(ops).ok_or_else(bad)?;
                if !self.in_text {
                    return Err(misplaced());
                }
                self.in_text = false;
            }
            b"Tc" | b"Tw" | b"Tz" | b"TL" | b"Ts" => {
                let [value] = numbers(ops).ok_or_else(bad)?;
                let text = &mut self.state.text;
                match op.operator {
                    b"Tc" => text.char_spacing = value,
                    b"Tw" => text.word_spacing = value,
                    b"Tz" => text.horizontal_scale = value / 100.0,
                    b"TL" => text.leading = value,
                    _ => text.rise = value,
                }
            }
            b"Tf" => {
                let [Object::Name(name), size] = ops else {
                    return Err(bad());
                };
                let size = number(size).ok_or_else(bad)?;
                self.state.text.font = self.font(name, at, budget)?;
                self.state.text.size = size;
            }
            b"Tr" => {
                let [mode] = numbers(ops).ok_or_else(bad)?;
                if !matches!(mode, 0.0 | 1.0 | 2.0 | 3.0 | 4.0 | 5.0 | 6.0 | 7.0) {
                    return Err(bad());
                }
                if mode >= 4.0 {
                    self.skipped.insert(Unsupported::TextClip);
                }
                // Checked above: an integer from 0 to 7.
                self.state.text.render_mode = mode as u8;
            }
            b"Td" | b"TD" => {
                let [tx, ty] = numbers(ops).ok_or_else(bad)?;
                if op.operator == b"TD" {
                    self.state.text.leading = -ty;
                }
                self.next_line(tx, ty, at)?;
            }
            b"Tm" => {
                let [a, b, c, d, e, f] = numbers(ops).ok_or_else(bad)?;
                let matrix = Matrix::new(a, b, c, d, e, f);
                if !matrix.is_finite() {
                    return Err(self.error(at, ContentErrorKind::NumberOutOfRange));
                }
                self.text_matrix = matrix;
                self.line_matrix = matrix;
            }
            b"T*" => {
                no_operands(ops).ok_or_else(bad)?;
                self.next_line(0.0, -self.state.text.leading, at)?;
            }
            b"Tj" | b"'" => {
                let [Object::String(string)] = ops else {
                    return Err(bad());
                };
                if op.operator == b"'" {
                    self.next_line(0.0, -self.state.text.leading, at)?;
                }
                self.show(&[Object::String(string.clone())], at, budget)?;
            }
            b"\"" => {
                let [word, character, Object::String(string)] = ops else {
                    return Err(bad());
                };
                let (word, character) = number(word).zip(number(character)).ok_or_else(bad)?;
                self.state.text.word_spacing = word;
                self.state.text.char_spacing = character;
                self.next_line(0.0, -self.state.text.leading, at)?;
                self.show(&[Object::String(string.clone())], at, budget)?;
            }
            b"TJ" => {
                let [Object::Array(items)] = ops else {
                    return Err(bad());
                };
                if items
                    .iter()
                    .any(|item| !matches!(item, Object::String(_)) && number(item).is_none())
                {
                    return Err(bad());
                }
                self.show(items, at, budget)?;
            }
            // Type 3 glyph metrics (9.6.4): the width is the font's; `d1`
            // makes the glyph uncoloured.
            b"d0" | b"d1" => {
                if !glyph_start {
                    return Err(misplaced());
                }
                let valid = match op.operator {
                    b"d0" => numbers::<2>(ops).is_some(),
                    _ => numbers::<6>(ops).is_some(),
                };
                if !valid {
                    return Err(bad());
                }
                if op.operator == b"d1" {
                    self.uncolored = true;
                }
            }
            // XObjects, shadings and inline images (8.8, 8.7.4.2, 8.9.7).
            b"Do" => {
                let [Object::Name(name)] = ops else {
                    return Err(bad());
                };
                let object = self.resource(b"XObject", name, at, budget)?;
                self.x_object(&object, at, budget)?;
            }
            b"sh" => {
                let [Object::Name(name)] = ops else {
                    return Err(bad());
                };
                self.require_resource(b"Shading", name, at, budget)?;
                self.skipped.insert(Unsupported::Shading);
            }
            b"BI" => {
                if let Some((dictionary, data)) = op.inline_image {
                    self.inline_image(dictionary, data, at, budget)?;
                }
            }
            // Marked content (14.6): optional content may hide what follows.
            b"BMC" | b"BDC" | b"MP" | b"DP" => {
                let (tag, properties) = match ops {
                    [Object::Name(tag)] if matches!(op.operator, b"BMC" | b"MP") => (tag, None),
                    [Object::Name(tag), properties] if matches!(op.operator, b"BDC" | b"DP") => {
                        (tag, Some(properties))
                    }
                    _ => return Err(bad()),
                };
                match properties {
                    None | Some(Object::Dictionary(_)) => {}
                    Some(Object::Name(name)) => {
                        self.require_resource(b"Properties", name, at, budget)?;
                    }
                    Some(_) => return Err(bad()),
                }
                if tag.as_slice() == b"OC" && op.operator == b"BDC" {
                    self.skipped.insert(Unsupported::OptionalContent);
                }
                if op.operator.starts_with(b"B") {
                    self.marked_content = self.marked_content.saturating_add(1);
                }
            }
            b"EMC" => {
                no_operands(ops).ok_or_else(bad)?;
                self.marked_content = self.marked_content.checked_sub(1).ok_or_else(misplaced)?;
            }
            // Compatibility sections (7.8.2).
            b"BX" => {
                no_operands(ops).ok_or_else(bad)?;
                self.compatibility = self.compatibility.saturating_add(1);
            }
            b"EX" => {
                no_operands(ops).ok_or_else(bad)?;
                self.compatibility = self.compatibility.checked_sub(1).ok_or_else(misplaced)?;
            }
            _ => return Err(self.error(at, ContentErrorKind::UnknownOperator)),
        }
        Ok(())
    }

    /// `Do` (8.8): draws an image or a form XObject. An image is decoded
    /// once per rendering.
    fn x_object(&mut self, object: &Object, at: u64, budget: &mut Budget) -> Result<(), Error> {
        if let Object::Reference(id) = object
            && let Some(form) = self.forms_read.get(id).cloned()
        {
            return self.draw_form(*id, form, budget);
        }
        // An XObject that is not a readable stream is skipped and reported,
        // as before images were drawn; limits and cancellation still stop.
        let stream = match object {
            Object::Reference(id) => match self.file.resolve(*id, budget) {
                Ok(IndirectValue::Stream(stream)) => Some(stream),
                Ok(IndirectValue::Object(_))
                | Err(pdf_syntax::Error::Malformed { .. } | pdf_syntax::Error::Truncated { .. }) => {
                    None
                }
                Err(other) => return Err(Error::Syntax(other)),
            },
            _ => None,
        };
        let Some(stream) = stream else {
            self.skipped.insert(Unsupported::XObject);
            return Ok(());
        };
        let Object::Reference(id) = object else {
            return Ok(());
        };
        match stream.dictionary.get(b"Subtype").and_then(Object::as_name) {
            Some(b"Image") => {}
            Some(b"Form") => {
                let form = self.read_form(*id, &stream, budget)?;
                return self.draw_form(*id, form, budget);
            }
            _ => {
                self.skipped.insert(Unsupported::XObject);
                return Ok(());
            }
        }
        // A stencil mask takes the fill colour: cached per colour. An image
        // whose colour space is a resource name depends on the scope it is
        // drawn in: not cached.
        if self.uncolored && stream.dictionary.get(b"ImageMask") != Some(&Object::Boolean(true)) {
            // An uncoloured glyph may only paint image masks (9.6.4).
            self.skipped.insert(Unsupported::Image);
            return Ok(());
        }
        let Some(fill) = self.stencil_fill(&stream.dictionary) else {
            return Ok(());
        };
        let key = (*id, fill);
        let cacheable = !matches!(
            stream.dictionary.get(b"ColorSpace"),
            Some(Object::Name(name)) if !is_device_name(name)
        );
        let cached = if cacheable {
            self.images.get(&key).cloned()
        } else {
            None
        };
        let image = match cached {
            Some(cached) => cached.clone(),
            None => {
                let Some(space) = self.image_space(&stream.dictionary, at, budget)? else {
                    return Ok(());
                };
                let decoded = image::decode(
                    self.file,
                    image::Body::Stream(&stream),
                    space.as_ref(),
                    fill,
                    &self.limits,
                    budget,
                )?;
                let image = decoded.map(|decoded| {
                    raster::pixmap(
                        decoded.rgba,
                        decoded.width,
                        decoded.height,
                        decoded.transparent,
                    )
                });
                let bytes = image
                    .as_ref()
                    .map_or(0, |p| usize::from(p.width()) * usize::from(p.height()) * 4);
                if cacheable && self.cached_image_bytes + bytes <= MAX_CACHED_IMAGE_BYTES {
                    self.cached_image_bytes += bytes;
                    self.images.insert(key, image.clone());
                }
                image
            }
        };
        let image = match image {
            Ok(image) => image,
            Err(capability) => {
                self.skipped.insert(capability);
                return Ok(());
            }
        };
        let interpolate = stream.dictionary.get(b"Interpolate") == Some(&Object::Boolean(true));
        self.paint_image(&image, interpolate, at)
    }

    /// Reads form `id` once: `None` if it cannot be drawn (no `/BBox`, an
    /// invalid `/Matrix`, content that cannot be decoded). Kept for the
    /// rendering while its content fits `MAX_CACHED_FORM_BYTES`.
    fn read_form(
        &mut self,
        id: ObjectId,
        stream: &Stream,
        budget: &mut Budget,
    ) -> Result<Option<Arc<Form>>, Error> {
        let form = self.form_parts(stream, budget)?.map(Arc::new);
        let bytes = form.as_ref().map_or(0, |form| form.content.len());
        if self.cached_form_bytes + bytes <= MAX_CACHED_FORM_BYTES {
            self.cached_form_bytes += bytes;
            self.forms_read.insert(id, form.clone());
        }
        Ok(form)
    }

    fn form_parts(&mut self, stream: &Stream, budget: &mut Budget) -> Result<Option<Form>, Error> {
        let dictionary = &stream.dictionary;
        let matrix = match dictionary.get(b"Matrix") {
            None => Some(Matrix::IDENTITY),
            Some(Object::Array(items)) => {
                numbers::<6>(items).map(|[a, b, c, d, e, f]| Matrix::new(a, b, c, d, e, f))
            }
            Some(_) => None,
        };
        let bbox = match dictionary.get(b"BBox") {
            Some(Object::Array(items)) => numbers::<4>(items),
            _ => None,
        };
        let (Some(matrix), Some(bbox)) = (matrix.filter(Matrix::is_finite), bbox) else {
            return Ok(None);
        };
        // A missing, null or unreadable /Resources: the enclosing ones.
        let resources = match dictionary.get(b"Resources") {
            None => None,
            Some(object) => text::resolve_dictionary(self.file, object, budget)?,
        };
        let (group, damaged_group) = match dictionary.get(b"Group") {
            None | Some(Object::Null) => (None, false),
            Some(object) => match text::resolve_dictionary(self.file, object, budget)? {
                Some(group)
                    if group.get(b"S").and_then(Object::as_name) == Some(b"Transparency") =>
                {
                    (Some(group.get(b"K") == Some(&Object::Boolean(true))), false)
                }
                // Another group subtype: none is defined.
                Some(_) => (None, false),
                None => (None, true),
            },
        };
        let content = match self.file.decoded_stream_data(stream, budget) {
            Ok(content) => Arc::new(content),
            Err(
                pdf_syntax::Error::Unsupported(_)
                | pdf_syntax::Error::Malformed { .. }
                | pdf_syntax::Error::Truncated { .. },
            ) => return Ok(None),
            Err(other) => return Err(Error::Syntax(other)),
        };
        Ok(Some(Form {
            matrix,
            bbox,
            resources,
            group,
            damaged_group,
            optional: dictionary.get(b"OC").is_some(),
            content,
        }))
    }

    /// Draws a form XObject (8.10): its content with its `/Matrix`, clipped
    /// to its `/BBox`, with its own resources if it has them (otherwise
    /// those of the enclosing content, page or form), inside a saved
    /// graphics state; marked content and compatibility sections must be
    /// balanced within it. A transparency group (11.6.6) starts with alphas
    /// of 1 and is composited as a whole with the fill alpha; a knockout
    /// group is drawn as a non-knockout one and reported (`BlendMode`), as
    /// is a damaged `/Group`.
    ///
    /// A form that cannot be drawn (`form` is `None`, nested too deep or
    /// inside itself, out of range) is skipped, and one with invalid content
    /// drawn up to its error; both are reported (`XObject`) and affect only
    /// themselves. The clip and the group layer count against `max_clips`
    /// and `max_path_work`; limits and cancellation stop the rendering.
    fn draw_form(
        &mut self,
        id: ObjectId,
        form: Option<Arc<Form>>,
        budget: &mut Budget,
    ) -> Result<(), Error> {
        let Some(form) = form else {
            self.skipped.insert(Unsupported::XObject);
            return Ok(());
        };
        if self.forms.len() >= MAX_FORM_DEPTH || self.forms.contains(&id) {
            self.skipped.insert(Unsupported::XObject);
            return Ok(());
        }
        let ctm = form.matrix.then(&self.state.ctm);
        let [x0, y0, x1, y1] = form.bbox;
        let mut clip = Path::default();
        clip.rectangle(x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs());
        let device = ctm.then(&self.base);
        if !(device.is_finite() && clip.is_within(&device, 0.0, MAX_DEVICE_COORDINATE)) {
            self.skipped.insert(Unsupported::XObject);
            return Ok(());
        }
        if self.active_clips >= self.limits.max_clips {
            return Err(Error::LimitExceeded {
                kind: RenderLimitKind::Clips,
                limit: u64::from(self.limits.max_clips),
            });
        }
        let work = clip.work(&device, 0.0, &self.window);
        self.charge_work(work)?;
        let alpha = self.state.fill.alpha;
        let layer = form.group.is_some() && alpha < 1.0;
        if layer {
            // Compositing the layer covers the same area again.
            self.charge_work(work)?;
        }
        if form.optional {
            self.skipped.insert(Unsupported::OptionalContent);
        }
        if form.group == Some(true) || form.damaged_group {
            self.skipped.insert(Unsupported::BlendMode);
        }

        let scope = form.resources.as_ref().map(|resources| {
            self.form_scopes.remove(&id).unwrap_or_else(|| Scope {
                resources: Some(resources.clone()),
                ..Scope::default()
            })
        });
        let nested = Nested {
            ctm,
            clip: Some((clip, device)),
            group: form.group.map(|_| layer.then_some(alpha)),
            scope,
        };
        let (result, scope) = self.run_nested(id, nested, &form.content, budget)?;
        if let Some(scope) = scope
            && self.form_scopes.len() < MAX_FORM_SCOPES
        {
            self.form_scopes.insert(id, scope);
        }
        self.nested_result(result, Unsupported::XObject)
    }

    /// `BI ... ID ... EI` (8.9.7): an inline image, decoded each time it is
    /// drawn (its data is part of the content, already budgeted).
    fn inline_image(
        &mut self,
        raw: &Dictionary,
        data: &[u8],
        at: u64,
        budget: &mut Budget,
    ) -> Result<(), Error> {
        let dictionary = image::inline_dictionary(raw);
        if self.uncolored && dictionary.get(b"ImageMask") != Some(&Object::Boolean(true)) {
            self.skipped.insert(Unsupported::Image);
            return Ok(());
        }
        let Some(fill) = self.stencil_fill(&dictionary) else {
            return Ok(());
        };
        let Some(space) = self.image_space(&dictionary, at, budget)? else {
            return Ok(());
        };
        let body = image::Body::Inline {
            dictionary: &dictionary,
            data,
        };
        let decoded = image::decode(self.file, body, space.as_ref(), fill, &self.limits, budget)?;
        let decoded = match decoded {
            Ok(decoded) => decoded,
            Err(capability) => {
                self.skipped.insert(capability);
                return Ok(());
            }
        };
        let image = raster::pixmap(
            decoded.rgba,
            decoded.width,
            decoded.height,
            decoded.transparent,
        );
        let interpolate = dictionary.get(b"Interpolate") == Some(&Object::Boolean(true));
        self.paint_image(&image, interpolate, at)
    }

    /// The colour painted through an image of `dictionary` if it is a
    /// stencil mask (8.9.6.2), black otherwise (unused). `None` when the
    /// fill colour cannot be drawn: the image is skipped and reported.
    fn stencil_fill(&mut self, dictionary: &Dictionary) -> Option<[u8; 3]> {
        if dictionary.get(b"ImageMask") != Some(&Object::Boolean(true)) {
            return Some([0; 3]);
        }
        match self.state.fill.rgba() {
            Ok(color) => {
                let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                Some([byte(color.r), byte(color.g), byte(color.b)])
            }
            Err(capability) => {
                self.skipped.insert(capability);
                None
            }
        }
    }

    /// The `/ColorSpace` of an image (none for a stencil mask), looked up in
    /// the resources when it names one. `None` when that name is not in the
    /// resources: only the image depends on it, so the image alone is
    /// skipped and reported (`ColorSpace`), the rest of the page drawn.
    fn image_space(
        &mut self,
        dictionary: &Dictionary,
        at: u64,
        budget: &mut Budget,
    ) -> Result<Option<Option<Object>>, Error> {
        if dictionary.get(b"ImageMask") == Some(&Object::Boolean(true)) {
            return Ok(Some(None));
        }
        match dictionary.get(b"ColorSpace") {
            Some(Object::Name(name)) if !is_device_name(name) => {
                match self.category(b"ColorSpace", at, budget)?.get(name) {
                    None | Some(Object::Null) => {
                        self.skipped.insert(Unsupported::ColorSpace);
                        Ok(None)
                    }
                    Some(object) => Ok(Some(Some(object.clone()))),
                }
            }
            other => Ok(Some(other.cloned())),
        }
    }

    /// Paints `image` in the unit square of the current transformation.
    fn paint_image(
        &mut self,
        image: &Arc<vello_cpu::Pixmap>,
        interpolate: bool,
        at: u64,
    ) -> Result<(), Error> {
        let matrix = self.state.ctm.then(&self.base);
        let mut unit = Path::default();
        unit.rectangle(0.0, 0.0, 1.0, 1.0);
        if !matrix.is_finite() || !unit.is_within(&matrix, 0.0, MAX_DEVICE_COORDINATE) {
            return Err(self.error(at, ContentErrorKind::NumberOutOfRange));
        }
        self.charge_work(unit.work(&matrix, 0.0, &self.window))?;
        self.canvas
            .draw_image(image, &matrix, self.state.fill.alpha, interpolate);
        Ok(())
    }

    /// `Td`: moves to the start of the next line, offset by (tx, ty).
    fn next_line(&mut self, tx: f64, ty: f64, at: u64) -> Result<(), Error> {
        let matrix = Matrix::new(1.0, 0.0, 0.0, 1.0, tx, ty).then(&self.line_matrix);
        if !matrix.is_finite() {
            return Err(self.error(at, ContentErrorKind::NumberOutOfRange));
        }
        self.line_matrix = matrix;
        self.text_matrix = matrix;
        Ok(())
    }

    /// The font named `name` in the page's `/Font` resources, loaded once.
    /// A name absent from the resources, or a font this renderer does not
    /// read yet, selects `Unsupported`: its text is skipped and reported.
    fn font(&mut self, name: &[u8], at: u64, budget: &mut Budget) -> Result<Selected, Error> {
        if let Some(selected) = self.fonts.get(name) {
            return Ok(selected.clone());
        }
        let entry = self.category(b"Font", at, budget)?.get(name).cloned();
        let id = match &entry {
            Some(Object::Reference(id)) => Some(*id),
            _ => None,
        };
        if let Some(selected) = id.and_then(|id| self.font_objects.get(&id)) {
            let selected = selected.clone();
            self.fonts.insert(name.to_vec(), selected.clone());
            return Ok(selected);
        }
        let dictionary = match entry {
            Some(object) => text::resolve_dictionary(self.file, &object, budget)?,
            None => None,
        };
        let selected = match dictionary {
            Some(dictionary)
                if dictionary.get(b"Subtype").and_then(Object::as_name) == Some(b"Type3") =>
            {
                match type3::load(self.file, &dictionary, budget)? {
                    Some(font) => {
                        self.loaded_fonts += 1;
                        Selected::Type3 {
                            number: self.loaded_fonts,
                            font: Arc::new(font),
                        }
                    }
                    None => Selected::Unsupported,
                }
            }
            Some(dictionary) => match font::load(self.file, &dictionary, budget)? {
                Some(font) => {
                    self.loaded_fonts += 1;
                    Selected::Font {
                        number: self.loaded_fonts,
                        font: Arc::new(font),
                    }
                }
                None => Selected::Unsupported,
            },
            None => Selected::Unsupported,
        };
        self.fonts.insert(name.to_vec(), selected.clone());
        if let Some(id) = id {
            self.font_objects.insert(id, selected.clone());
        }
        Ok(selected)
    }

    /// Shows the strings of `items`, with the numbers between them moving
    /// the text position (`TJ`), and advances the text matrix (9.4.4). The
    /// glyphs of one operator are painted as one path, in the text
    /// rendering mode: filled (nonzero), stroked, both, or neither.
    fn show(&mut self, items: &[Object], at: u64, budget: &mut Budget) -> Result<(), Error> {
        let (font_number, font) = match &self.state.text.font {
            Selected::Font { number, font } => (*number, Arc::clone(font)),
            Selected::Type3 { number, font } => {
                let (number, font) = (*number, Arc::clone(font));
                return self.show_type3(items, number, &font, at, budget);
            }
            Selected::None | Selected::Unsupported => {
                self.skipped.insert(Unsupported::Text);
                return Ok(());
            }
        };
        let text = self.state.text.clone();
        let mode = text.render_mode;
        let draws = !matches!(mode, 3 | 7);
        if draws && font.substituted() {
            self.skipped.insert(Unsupported::FontSubstituted);
        }
        let scale = text.size / font.units_per_em();
        let glyph_space = Matrix::new(
            scale * text.horizontal_scale,
            0.0,
            0.0,
            scale,
            0.0,
            text.rise,
        );
        let mut path = Path::default();
        let mut shown: u64 = 0;
        for item in items {
            let string = match item {
                Object::String(string) => string,
                other => {
                    let adjust = number(other).unwrap_or(0.0);
                    let tx = -adjust / 1000.0 * text.size * text.horizontal_scale;
                    self.advance(tx, at)?;
                    continue;
                }
            };
            for code in font.codes(string) {
                shown += 1;
                if shown.is_multiple_of(CANCEL_CHECK_CODES) {
                    budget.check_cancelled()?;
                }
                if draws && code.missing {
                    self.skipped.insert(Unsupported::Text);
                }
                if draws && let Some(glyph) = code.glyph {
                    match self.outline(font_number, &font, glyph, budget)? {
                        Some(outline) => {
                            // Bounded before allocating. Each segment of a
                            // glyph counts as an operation, as a path segment
                            // drawn by an operator would, and at least one
                            // unit of path work (`Path::work`), charged
                            // when painted.
                            self.operations += outline.len() as u64;
                            if self.operations > self.limits.max_operations {
                                return Err(Error::LimitExceeded {
                                    kind: RenderLimitKind::Operations,
                                    limit: self.limits.max_operations,
                                });
                            }
                            let segments = path.segments().len() + outline.len();
                            if self.work + segments as f64 > self.limits.max_path_work as f64 {
                                return Err(Error::LimitExceeded {
                                    kind: RenderLimitKind::PathWork,
                                    limit: self.limits.max_path_work,
                                });
                            }
                            let matrix = glyph_space.then(&self.text_matrix);
                            append(&mut path, &outline, &matrix);
                        }
                        None => {
                            self.skipped.insert(Unsupported::Text);
                        }
                    }
                }
                let spacing = text.char_spacing
                    + if code.is_space {
                        text.word_spacing
                    } else {
                        0.0
                    };
                let tx = (code.width / 1000.0 * text.size + spacing) * text.horizontal_scale;
                self.advance(tx, at)?;
            }
        }
        let fill = matches!(mode, 0 | 2 | 4 | 6);
        let stroke = matches!(mode, 1 | 2 | 5 | 6);
        if path.is_empty() || !(fill || stroke) {
            return Ok(());
        }
        self.path = path;
        self.paint(at, fill.then_some(false), stroke)
    }

    /// Shows the strings of `items` in a Type 3 font (9.6.4): each code's
    /// glyph procedure runs with the font matrix, the text rendering matrix
    /// and the font's resources, like a form; the text then advances by the
    /// code's width through the font matrix. Invisible modes (3, 7) only
    /// advance; other modes paint as the procedure does.
    fn show_type3(
        &mut self,
        items: &[Object],
        font_number: usize,
        font: &type3::Type3,
        at: u64,
        budget: &mut Budget,
    ) -> Result<(), Error> {
        let text = self.state.text.clone();
        let draws = !matches!(text.render_mode, 3 | 7);
        let size = Matrix::new(
            text.size * text.horizontal_scale,
            0.0,
            0.0,
            text.size,
            0.0,
            text.rise,
        );
        let mut shown: u64 = 0;
        for item in items {
            let string = match item {
                Object::String(string) => string,
                other => {
                    let adjust = number(other).unwrap_or(0.0);
                    let tx = -adjust / 1000.0 * text.size * text.horizontal_scale;
                    self.advance(tx, at)?;
                    continue;
                }
            };
            for &code in string {
                shown += 1;
                if shown.is_multiple_of(CANCEL_CHECK_CODES) {
                    budget.check_cancelled()?;
                }
                let index = usize::from(code);
                if draws && font.missing.get(index).copied().unwrap_or(false) {
                    self.skipped.insert(Unsupported::Text);
                }
                if draws && let Some(Some(id)) = font.procs.get(index).copied() {
                    let ctm = font
                        .matrix
                        .then(&size)
                        .then(&self.text_matrix)
                        .then(&self.state.ctm);
                    self.glyph(font_number, font, id, ctm, budget)?;
                }
                let spacing =
                    text.char_spacing + if code == b' ' { text.word_spacing } else { 0.0 };
                let tx = (font.advance(code) * text.size + spacing) * text.horizontal_scale;
                self.advance(tx, at)?;
            }
        }
        Ok(())
    }

    /// Runs the glyph procedure `id` of a Type 3 font with `ctm`. A glyph
    /// that cannot be drawn (not a readable stream, invalid content, nested
    /// too deep or inside itself) is skipped, or drawn up to its error, and
    /// reported (`Text`); the text goes on.
    fn glyph(
        &mut self,
        number: usize,
        font: &type3::Type3,
        id: ObjectId,
        ctm: Matrix,
        budget: &mut Budget,
    ) -> Result<(), Error> {
        // Each run is an operation, even of an empty procedure.
        self.operations += 1;
        if self.operations > self.limits.max_operations {
            return Err(Error::LimitExceeded {
                kind: RenderLimitKind::Operations,
                limit: self.limits.max_operations,
            });
        }
        if self.forms.len() >= MAX_FORM_DEPTH || self.forms.contains(&id) || !ctm.is_finite() {
            self.skipped.insert(Unsupported::Text);
            return Ok(());
        }
        let content = match self.glyph_contents.get(&id) {
            Some(Some(content)) => Arc::clone(content),
            Some(None) => {
                self.skipped.insert(Unsupported::Text);
                return Ok(());
            }
            None => {
                let decoded = match self.file.resolve(id, budget) {
                    Ok(IndirectValue::Stream(stream)) => {
                        match self.file.decoded_stream_data(&stream, budget) {
                            Ok(content) => Some(content),
                            Err(
                                pdf_syntax::Error::Unsupported(_)
                                | pdf_syntax::Error::Malformed { .. }
                                | pdf_syntax::Error::Truncated { .. },
                            ) => None,
                            Err(other) => return Err(Error::Syntax(other)),
                        }
                    }
                    Ok(IndirectValue::Object(_))
                    | Err(
                        pdf_syntax::Error::Malformed { .. } | pdf_syntax::Error::Truncated { .. },
                    ) => None,
                    Err(other) => return Err(Error::Syntax(other)),
                };
                let Some(decoded) = decoded else {
                    // Not read again at its next occurrence.
                    self.glyph_contents.insert(id, None);
                    self.skipped.insert(Unsupported::Text);
                    return Ok(());
                };
                let content = Arc::new(decoded);
                if self.cached_form_bytes + content.len() <= MAX_CACHED_FORM_BYTES {
                    self.cached_form_bytes += content.len();
                    self.glyph_contents.insert(id, Some(Arc::clone(&content)));
                }
                content
            }
        };
        let scope = font.resources.as_ref().map(|resources| {
            self.type3_scopes.remove(&number).unwrap_or_else(|| Scope {
                resources: Some(resources.clone()),
                ..Scope::default()
            })
        });
        let outer = (
            std::mem::replace(&mut self.glyph_start, true),
            std::mem::replace(&mut self.uncolored, false),
        );
        let nested = Nested {
            ctm,
            clip: None,
            group: None,
            scope,
        };
        let ran = self.run_nested(id, nested, &content, budget);
        (self.glyph_start, self.uncolored) = outer;
        let (result, scope) = ran?;
        if let Some(scope) = scope
            && self.type3_scopes.len() < MAX_FORM_SCOPES
        {
            self.type3_scopes.insert(number, scope);
        }
        self.nested_result(result, Unsupported::Text)
    }

    /// Runs `content` as nested content `id` (a form, a Type 3 glyph): in a
    /// saved state it cannot restore, with the matrix, clip, group and scope
    /// of `nested`, outside any text object, path, marked content or
    /// compatibility section; then undoes whatever the content left open.
    /// Returns the content's result and the scope, to be kept.
    fn run_nested(
        &mut self,
        id: ObjectId,
        nested: Nested,
        content: &[u8],
        budget: &mut Budget,
    ) -> Result<(Result<(), Error>, Option<Scope>), Error> {
        self.save()?;
        let outer_floor = std::mem::replace(&mut self.floor, self.saved.len());
        self.state.ctm = nested.ctm;
        if let Some((clip, device)) = &nested.clip {
            self.canvas.push_clip(clip, device, false);
            self.state.clips += 1;
            self.active_clips += 1;
        }
        let layer = match nested.group {
            Some(layer) => {
                self.state.fill.alpha = 1.0;
                self.state.stroke.alpha = 1.0;
                layer
            }
            None => None,
        };
        if let Some(alpha) = layer {
            self.canvas.push_opacity(alpha);
        }
        let outer = (
            std::mem::take(&mut self.path),
            self.pending_clip.take(),
            std::mem::take(&mut self.marked_content),
            std::mem::take(&mut self.compatibility),
            std::mem::replace(&mut self.in_text, false),
            self.text_matrix,
            self.line_matrix,
        );
        let mut scope = nested.scope;
        if let Some(scope) = &mut scope {
            self.swap_scope(scope);
        }
        self.forms.push(id);

        let result = self.run_content(content, budget);

        // Leave, whatever the content left open: its states, text object,
        // path, marked content; then the layer and the entry state.
        self.forms.pop();
        if let Some(scope) = &mut scope {
            self.swap_scope(scope);
        }
        while self.saved.len() > self.floor {
            self.restore();
        }
        if layer.is_some() {
            self.canvas.pop_layer();
        }
        self.floor = outer_floor;
        self.restore();
        (
            self.path,
            self.pending_clip,
            self.marked_content,
            self.compatibility,
            self.in_text,
            self.text_matrix,
            self.line_matrix,
        ) = outer;
        Ok((result, scope))
    }

    /// The result of nested content: invalid content (or damaged objects
    /// it reads) only affects it and is reported as `capability`; limits
    /// and cancellation stop the rendering.
    fn nested_result(
        &mut self,
        result: Result<(), Error>,
        capability: Unsupported,
    ) -> Result<(), Error> {
        match result {
            Ok(()) => Ok(()),
            Err(
                Error::Content { .. }
                | Error::Syntax(
                    pdf_syntax::Error::Malformed { .. } | pdf_syntax::Error::Truncated { .. },
                ),
            ) => {
                self.skipped.insert(capability);
                Ok(())
            }
            Err(other) => Err(other),
        }
    }

    fn advance(&mut self, tx: f64, at: u64) -> Result<(), Error> {
        let matrix = Matrix::new(1.0, 0.0, 0.0, 1.0, tx, 0.0).then(&self.text_matrix);
        if !matrix.is_finite() {
            return Err(self.error(at, ContentErrorKind::NumberOutOfRange));
        }
        self.text_matrix = matrix;
        Ok(())
    }

    /// The outline of `glyph`, from the cache or drawn now. Each outline drawn
    /// is charged one object; an outline over `max_glyph_segments` is an
    /// error. `None` for a glyph the program cannot draw.
    fn outline(
        &mut self,
        number: usize,
        font: &font::Font,
        glyph: u16,
        budget: &mut Budget,
    ) -> Result<Option<Arc<Vec<Segment>>>, Error> {
        let key = (number, glyph);
        if let Some(outline) = self.glyphs.get(&key) {
            return Ok(outline.clone());
        }
        budget.charge_object()?;
        let max = usize::try_from(self.limits.max_glyph_segments).unwrap_or(usize::MAX);
        let outline = match font.outline(glyph, max) {
            Ok(Some(outline)) => Arc::new(outline),
            Ok(None) => {
                self.glyphs.insert(key, None);
                return Ok(None);
            }
            Err(()) => {
                return Err(Error::LimitExceeded {
                    kind: RenderLimitKind::GlyphSegments,
                    limit: self.limits.max_glyph_segments,
                });
            }
        };
        if self.cached_segments + outline.len() <= MAX_CACHED_SEGMENTS {
            self.cached_segments += outline.len();
            self.glyphs.insert(key, Some(Arc::clone(&outline)));
        }
        Ok(Some(outline))
    }

    fn need_point(&self, added: bool, at: u64) -> Result<(), Error> {
        if added {
            Ok(())
        } else {
            Err(self.error(at, ContentErrorKind::NoCurrentPoint))
        }
    }

    /// The fill paint for lower-case colour operators, the stroke paint for
    /// upper-case ones.
    fn paint_mut(&mut self, operator: &[u8]) -> &mut Paint {
        if operator.first().is_some_and(u8::is_ascii_uppercase) {
            &mut self.state.stroke
        } else {
            &mut self.state.fill
        }
    }

    /// Paints the current path, applies a pending clip, then ends the path.
    fn paint(&mut self, at: u64, fill: Option<bool>, stroke: bool) -> Result<(), Error> {
        let matrix = self.state.ctm.then(&self.base);
        if !self.path.is_empty() {
            let pad = if stroke {
                self.stroke_pad(&matrix)
            } else {
                0.0
            };
            if !matrix.is_finite() || !self.path.is_within(&matrix, pad, MAX_DEVICE_COORDINATE) {
                return Err(self.error(at, ContentErrorKind::NumberOutOfRange));
            }
            if let Some(even_odd) = fill {
                self.charge_work(self.path.work(&matrix, 0.0, &self.window))?;
                match self.state.fill.rgba() {
                    Ok(color) => self.canvas.fill(&self.path, &matrix, even_odd, color),
                    Err(capability) => {
                        self.skipped.insert(capability);
                    }
                }
            }
            if stroke {
                self.charge_work(self.stroke_work(&matrix, pad))?;
                match self.state.stroke.rgba() {
                    Ok(color) => {
                        self.canvas
                            .stroke(&self.path, &matrix, &self.state.style, color);
                    }
                    Err(capability) => {
                        self.skipped.insert(capability);
                    }
                }
            }
        }
        if let Some(even_odd) = self.pending_clip.take()
            && !self.path.is_empty()
        {
            if !matrix.is_finite() || !self.path.is_within(&matrix, 0.0, MAX_DEVICE_COORDINATE) {
                return Err(self.error(at, ContentErrorKind::NumberOutOfRange));
            }
            if self.active_clips >= self.limits.max_clips {
                return Err(Error::LimitExceeded {
                    kind: RenderLimitKind::Clips,
                    limit: u64::from(self.limits.max_clips),
                });
            }
            self.charge_work(self.path.work(&matrix, 0.0, &self.window))?;
            self.canvas.push_clip(&self.path, &matrix, even_odd);
            self.state.clips += 1;
            self.active_clips += 1;
        }
        self.path.clear();
        Ok(())
    }

    /// How far a stroke outline extends from the path, in device pixels: half
    /// the width through the CTM's largest scale (bounded by the Frobenius
    /// norm), times the miter limit for miter joins or the square cap
    /// diagonal otherwise.
    fn stroke_pad(&self, matrix: &Matrix) -> f64 {
        let style = &self.state.style;
        if style.width == 0.0 {
            return HAIRLINE;
        }
        let norm =
            (matrix.a * matrix.a + matrix.b * matrix.b + matrix.c * matrix.c + matrix.d * matrix.d)
                .sqrt();
        let reach = match style.join {
            LineJoin::Miter => style.miter_limit.max(std::f64::consts::SQRT_2),
            _ => std::f64::consts::SQRT_2,
        };
        style.width / 2.0 * reach * norm
    }

    /// A stroke outline has both sides of every segment plus joins and caps,
    /// within `pad` of the path; a dashed stroke has a few segments per dash
    /// (dashes are cut in user space, so they are counted there).
    fn stroke_work(&self, matrix: &Matrix, pad: f64) -> f64 {
        let style = &self.state.style;
        let period: f64 = style.dashes.iter().sum();
        let dashes = if period > 0.0 {
            self.path.polygon_length() / period * style.dashes.len() as f64
        } else {
            0.0
        };
        3.0 * self.path.work(matrix, pad, &self.window) + 4.0 * dashes
    }

    fn charge_work(&mut self, work: f64) -> Result<(), Error> {
        self.work += work;
        // The comparison is false for NaN: refuse it as well.
        if self.work <= self.limits.max_path_work as f64 {
            Ok(())
        } else {
            Err(Error::LimitExceeded {
                kind: RenderLimitKind::PathWork,
                limit: self.limits.max_path_work,
            })
        }
    }

    /// A device colour space, or `Other` when the page replaces it with a
    /// `DefaultGray`, `DefaultRGB` or `DefaultCMYK` resource (8.6.5.6).
    fn device(
        &mut self,
        space: ColorSpace,
        at: u64,
        budget: &mut Budget,
    ) -> Result<ColorSpace, Error> {
        let key: &[u8] = match space {
            ColorSpace::Gray => b"DefaultGray",
            ColorSpace::Rgb => b"DefaultRGB",
            ColorSpace::Cmyk => b"DefaultCMYK",
            other => return Ok(other),
        };
        let replaced = self.category(b"ColorSpace", at, budget)?.get(key).is_some();
        Ok(if replaced { ColorSpace::Other } else { space })
    }

    /// Resolves a colour space name: the device families directly, other
    /// names through `/Resources /ColorSpace` (ISO 32000-2, 8.6.3), once.
    fn color_space(
        &mut self,
        name: &[u8],
        at: u64,
        budget: &mut Budget,
    ) -> Result<ColorSpace, Error> {
        if let Some(space) = device_space(name) {
            return self.device(space, at, budget);
        }
        if let Some(space) = self.color_spaces.get(name) {
            return Ok(*space);
        }
        let value = self.resource(b"ColorSpace", name, at, budget)?;
        let value = match value {
            Object::Reference(_) => self.direct(&value, at, budget)?,
            other => other,
        };
        let family = match &value {
            Object::Name(family) => Some(family.as_slice()),
            Object::Array(items) => match items.first() {
                Some(Object::Name(family)) => Some(family.as_slice()),
                _ => None,
            },
            _ => None,
        };
        let family = family.ok_or_else(|| self.error(at, ContentErrorKind::InvalidResource))?;
        let space = match (&value, device_space(family)) {
            (Object::Name(_), Some(space)) => self.device(space, at, budget)?,
            _ if family == b"Pattern" => ColorSpace::Pattern,
            _ => ColorSpace::Other,
        };
        self.color_spaces.insert(name.to_vec(), space);
        Ok(space)
    }

    fn apply(&mut self, state: &ExtGState) {
        for setting in &state.settings {
            let style = &mut self.state.style;
            match setting {
                Setting::Width(width) => style.width = *width,
                Setting::Cap(cap) => style.cap = *cap,
                Setting::Join(join) => style.join = *join,
                Setting::MiterLimit(limit) => style.miter_limit = *limit,
                Setting::Dash(dashes, phase) => {
                    style.dashes.clone_from(dashes);
                    style.dash_phase = *phase;
                }
                Setting::StrokeAlpha(alpha) => self.state.stroke.alpha = *alpha,
                Setting::FillAlpha(alpha) => self.state.fill.alpha = *alpha,
            }
        }
        self.skipped.extend(state.skipped.iter().copied());
    }

    /// Reads an `/ExtGState` dictionary once (ISO 32000-2, 8.4.5, table 57).
    /// Only the entries applied are resolved: device-dependent ones
    /// (halftones, transfer and generation functions) may be streams.
    fn ext_g_state(
        &mut self,
        name: &[u8],
        at: u64,
        budget: &mut Budget,
    ) -> Result<ExtGState, Error> {
        if let Some(state) = self.ext_g_states.get(name) {
            return Ok(state.clone());
        }
        let page = self.page;
        let invalid = move || content_error(page, at, ContentErrorKind::InvalidResource);
        let entry = self.resource(b"ExtGState", name, at, budget)?;
        let Object::Dictionary(dict) = self.direct(&entry, at, budget)? else {
            return Err(invalid());
        };
        let mut state = ExtGState::default();
        for (key, value) in dict.iter() {
            match key {
                b"LW" | b"LC" | b"LJ" | b"ML" | b"CA" | b"ca" => {
                    let value = number(&self.direct(value, at, budget)?).ok_or_else(invalid)?;
                    state.settings.push(match key {
                        b"LW" => {
                            Setting::Width((value >= 0.0).then_some(value).ok_or_else(invalid)?)
                        }
                        b"LC" => Setting::Cap(line_cap(value).ok_or_else(invalid)?),
                        b"LJ" => Setting::Join(line_join(value).ok_or_else(invalid)?),
                        b"ML" => {
                            Setting::MiterLimit((value > 0.0).then_some(value).ok_or_else(invalid)?)
                        }
                        b"CA" => Setting::StrokeAlpha(alpha(value)),
                        _ => Setting::FillAlpha(alpha(value)),
                    });
                }
                b"D" => {
                    let Object::Array(items) = self.direct(value, at, budget)? else {
                        return Err(invalid());
                    };
                    let [Object::Array(array), phase] = items.as_slice() else {
                        return Err(invalid());
                    };
                    let (dashes, phase) = dash(array, phase).ok_or_else(invalid)?;
                    state.settings.push(Setting::Dash(dashes, phase));
                }
                b"BM" => {
                    let normal =
                        |o: &Object| matches!(o.as_name(), Some(b"Normal" | b"Compatible"));
                    let supported = match self.direct(value, at, budget)? {
                        Object::Array(items) => items.first().is_some_and(normal),
                        other => normal(&other),
                    };
                    if !supported {
                        state.skipped.push(Unsupported::BlendMode);
                    }
                }
                b"SMask" if value.as_name() != Some(b"None") => {
                    state.skipped.push(Unsupported::SoftMask);
                }
                b"AIS" if *value != Object::Boolean(false) => {
                    state.skipped.push(Unsupported::AlphaIsShape);
                }
                b"Font" => state.skipped.push(Unsupported::Text),
                b"TR" | b"TR2" if !matches!(value.as_name(), Some(b"Identity" | b"Default")) => {
                    state.skipped.push(Unsupported::TransferFunction);
                }
                // Device-dependent parameters (overprint, halftone, black
                // generation, undercolour removal, flatness, smoothness,
                // stroke adjustment, rendering intent, text knockout) and
                // unknown keys do not change this rendering.
                _ => {}
            }
        }
        self.ext_g_states.insert(name.to_vec(), state.clone());
        Ok(state)
    }

    /// The resource category dictionary (`/ColorSpace`, `/ExtGState`...),
    /// resolved once; empty when absent.
    fn category(
        &mut self,
        category: &'static [u8],
        at: u64,
        budget: &mut Budget,
    ) -> Result<&Dictionary, Error> {
        if !self.categories.contains_key(category) {
            let entry = self
                .resources
                .as_ref()
                .and_then(|resources| resources.get(category))
                .cloned();
            let dict = match entry {
                None => Dictionary::new(),
                Some(entry) => match self.direct(&entry, at, budget)? {
                    Object::Dictionary(dict) => dict,
                    Object::Null => Dictionary::new(),
                    _ => return Err(self.error(at, ContentErrorKind::InvalidResource)),
                },
            };
            self.categories.insert(category, dict);
        }
        self.categories
            .get(category)
            .ok_or_else(|| self.error(at, ContentErrorKind::InvalidResource))
    }

    /// `/Resources /<category> /<name>` as written (possibly a reference);
    /// absent or null is an `InvalidResource`.
    fn resource(
        &mut self,
        category: &'static [u8],
        name: &[u8],
        at: u64,
        budget: &mut Budget,
    ) -> Result<Object, Error> {
        match self.category(category, at, budget)?.get(name) {
            None | Some(Object::Null) => Err(self.error(at, ContentErrorKind::InvalidResource)),
            Some(object) => Ok(object.clone()),
        }
    }

    /// Checks that a resource exists, without reading it (it may be a stream).
    fn require_resource(
        &mut self,
        category: &'static [u8],
        name: &[u8],
        at: u64,
        budget: &mut Budget,
    ) -> Result<(), Error> {
        match self.category(category, at, budget)?.get(name) {
            None | Some(Object::Null) => Err(self.error(at, ContentErrorKind::InvalidResource)),
            Some(_) => Ok(()),
        }
    }

    /// Resolves `object` once if it is a reference; a stream or a second
    /// reference is an `InvalidResource`.
    fn direct(&self, object: &Object, at: u64, budget: &mut Budget) -> Result<Object, Error> {
        let Object::Reference(id) = object else {
            return Ok(object.clone());
        };
        match self.file.resolve(*id, budget)? {
            IndirectValue::Object(Object::Reference(_)) | IndirectValue::Stream(_) => {
                Err(self.error(at, ContentErrorKind::InvalidResource))
            }
            IndirectValue::Object(object) => Ok(object),
        }
    }
}

/// Appends `outline` (font units) to `path` through `matrix`.
fn append(path: &mut Path, outline: &[Segment], matrix: &Matrix) {
    for segment in outline {
        match segment {
            Segment::MoveTo(point) => path.move_to(matrix.apply(*point)),
            Segment::LineTo(point) => {
                path.line_to(matrix.apply(*point));
            }
            Segment::CurveTo(c1, c2, end) => {
                path.curve_to(matrix.apply(*c1), matrix.apply(*c2), matrix.apply(*end));
            }
            Segment::Close => path.close(),
        }
    }
}

fn content_error(page: ObjectId, offset: u64, kind: ContentErrorKind) -> Error {
    Error::Content { page, offset, kind }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    SpecialState,
    GeneralState,
    PathConstruction,
    Painting,
    Clip,
    Color,
    TextObject,
    TextState,
    TextPosition,
    TextShow,
    /// XObjects, shadings and inline images.
    External,
    MarkedContent,
    Compatibility,
    Type3,
    Unknown,
}

/// Operator categories of ISO 32000-2, table 50.
fn category(operator: &[u8]) -> Category {
    match operator {
        b"q" | b"Q" | b"cm" => Category::SpecialState,
        b"w" | b"J" | b"j" | b"M" | b"d" | b"ri" | b"i" | b"gs" => Category::GeneralState,
        b"m" | b"l" | b"c" | b"v" | b"y" | b"h" | b"re" => Category::PathConstruction,
        b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"n" => {
            Category::Painting
        }
        b"W" | b"W*" => Category::Clip,
        b"CS" | b"cs" | b"SC" | b"SCN" | b"sc" | b"scn" | b"G" | b"g" | b"RG" | b"rg" | b"K"
        | b"k" => Category::Color,
        b"BT" | b"ET" => Category::TextObject,
        b"Tc" | b"Tw" | b"Tz" | b"TL" | b"Tf" | b"Tr" | b"Ts" => Category::TextState,
        b"Td" | b"TD" | b"Tm" | b"T*" => Category::TextPosition,
        b"Tj" | b"TJ" | b"'" | b"\"" => Category::TextShow,
        b"Do" | b"sh" | b"BI" => Category::External,
        b"MP" | b"DP" | b"BMC" | b"BDC" | b"EMC" => Category::MarkedContent,
        b"BX" | b"EX" => Category::Compatibility,
        b"d0" | b"d1" => Category::Type3,
        _ => Category::Unknown,
    }
}

/// A device colour space name, used as is by an image (not a resource).
fn is_device_name(name: &[u8]) -> bool {
    matches!(
        name,
        b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK" | b"G" | b"RGB" | b"CMYK"
    )
}

fn device_space(name: &[u8]) -> Option<ColorSpace> {
    match name {
        b"DeviceGray" => Some(ColorSpace::Gray),
        b"DeviceRGB" => Some(ColorSpace::Rgb),
        b"DeviceCMYK" => Some(ColorSpace::Cmyk),
        b"Pattern" => Some(ColorSpace::Pattern),
        _ => None,
    }
}

fn number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) if value.is_finite() => Some(*value),
        _ => None,
    }
}

fn numbers<const N: usize>(operands: &[Object]) -> Option<[f64; N]> {
    let operands: &[Object; N] = operands.try_into().ok()?;
    let mut out = [0.0; N];
    for (slot, operand) in out.iter_mut().zip(operands) {
        *slot = number(operand)?;
    }
    Some(out)
}

fn no_operands(operands: &[Object]) -> Option<()> {
    operands.is_empty().then_some(())
}

/// Exactly the space's number of components, clamped to `0.0..=1.0`.
fn color_components(operands: &[Object], space: ColorSpace) -> Option<[f32; 4]> {
    if Some(operands.len()) != space.components() {
        return None;
    }
    let mut out = [0.0; 4];
    for (slot, operand) in out.iter_mut().zip(operands) {
        *slot = number(operand)?.clamp(0.0, 1.0) as f32;
    }
    Some(out)
}

fn alpha(value: f64) -> f32 {
    value.clamp(0.0, 1.0) as f32
}

fn line_cap(value: f64) -> Option<LineCap> {
    match value {
        0.0 => Some(LineCap::Butt),
        1.0 => Some(LineCap::Round),
        2.0 => Some(LineCap::Square),
        _ => None,
    }
}

fn line_join(value: f64) -> Option<LineJoin> {
    match value {
        0.0 => Some(LineJoin::Miter),
        1.0 => Some(LineJoin::Round),
        2.0 => Some(LineJoin::Bevel),
        _ => None,
    }
}

/// A dash array of at most `MAX_DASH_ENTRIES` non-negative numbers, not all
/// zero unless empty, and its phase reduced to one period (ISO 32000-2,
/// 8.4.3.6). An odd array is repeated to an even one.
fn dash(array: &[Object], phase: &Object) -> Option<(Vec<f64>, f64)> {
    if array.len() > MAX_DASH_ENTRIES {
        return None;
    }
    let dashes = array.iter().map(number).collect::<Option<Vec<_>>>()?;
    let phase = number(phase)?;
    if dashes.iter().any(|d| *d < 0.0) {
        return None;
    }
    if dashes.is_empty() {
        return Some((dashes, 0.0));
    }
    let dashes = if dashes.len() % 2 == 1 {
        dashes.iter().chain(dashes.iter()).copied().collect()
    } else {
        dashes
    };
    let period: f64 = dashes.iter().sum();
    if !(period > 0.0 && period.is_finite()) {
        return None;
    }
    Some((dashes, phase.rem_euclid(period)))
}
