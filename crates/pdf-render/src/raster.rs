//! The only module that touches `vello_cpu` (ADR-0003): replacing the
//! rasteriser, for example by `tiny-skia`, means rewriting this file.
//!
//! The adapter uses fills, strokes, the clip stack and opacity layers (one
//! per transparency group). `vello_cpu` asserts that every layer is popped
//! before rendering, whereas pending clip paths are allowed: the
//! interpreter pops the layer of a form whatever its content does, before
//! the form's own state is restored. Clips and layers are separate stacks.
//!
//! Every path reaches `vello_cpu` already in device space, flattened and
//! clipped to the canvas (plus `MARGIN`), in double precision. `vello_cpu`
//! keeps its line segments in single precision: a point millions of pixels
//! away would lose about a quarter of a pixel, differently in a tile and in
//! the whole page. Flattening uses the same tolerance as `vello_cpu` and
//! does not depend on where the canvas lies, so a tile and the page cut the
//! same lines. Clipping a closed polygon to a convex window keeps the
//! winding number of every point inside the window, so both fill rules hold.

use std::sync::Arc;
use vello_cpu::color::{AlphaColor, Srgb};

use vello_cpu::kurbo::{
    self, Affine, BezPath, Cap, Join, PathEl, Point as KPoint, Rect, Shape, Stroke, StrokeOpts,
};
use vello_cpu::peniko::{Extend, Fill, ImageAlphaType, ImageQuality, ImageSampler};
use vello_cpu::{Image, ImageSource, PixelMetadata, Pixmap, RenderContext, Resources};

use crate::geometry::{Matrix, Path, Segment, Window};

/// Colour with straight alpha, components in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineJoin {
    Miter,
    Round,
    Bevel,
}

/// Stroke parameters in user space (ISO 32000-2, 8.4.3).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StrokeStyle {
    // Dashes, when present, are an even number of non-negative lengths
    // with a positive finite sum, and `dash_phase` lies in one period.
    pub width: f64,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f64,
    /// Dash lengths (empty for a solid line) and phase.
    pub dashes: Vec<f64>,
    pub dash_phase: f64,
}

/// Width of a zero-width line, in device pixels: "the thinnest line that can
/// be rendered" (ISO 32000-2, 8.4.3.2).
pub(crate) const HAIRLINE: f64 = 1.0;

/// Flattening tolerance in device pixels: the one `vello_cpu` 0.3 uses
/// (`vello_common::flatten::TOL`), so that curves look the same.
const TOLERANCE: f64 = 0.25;
/// Pixels kept around the canvas when clipping: edges there do not change
/// any pixel of the canvas, yet stay clear of its border.
const MARGIN: f64 = 2.0;

/// Side of the square cells a canvas is cut into, on a grid fixed to the
/// top-left corner of the page rendering: the side of the viewer's tiles,
/// so that a tile is one cell.
pub(crate) const CELL: u32 = 256;

/// One cell: its own `vello_cpu` context, whose origin is the cell's
/// top-left corner.
struct Cell {
    context: RenderContext,
    /// Top-left corner in pixels of the whole page rendering.
    x: u32,
    y: u32,
    width: u16,
    height: u16,
}

/// A rectangle of cells, by column and row numbers (inclusive); empty when
/// a first number exceeds the last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    columns: (i64, i64),
    rows: (i64, i64),
}

impl Span {
    const EMPTY: Self = Self {
        columns: (0, -1),
        rows: (0, -1),
    };

    fn meet(self, other: Self) -> Self {
        Self {
            columns: (
                self.columns.0.max(other.columns.0),
                self.columns.1.min(other.columns.1),
            ),
            rows: (self.rows.0.max(other.rows.0), self.rows.1.min(other.rows.1)),
        }
    }
}

impl Cell {
    /// The cell's clipping window, in pixels of the page rendering.
    fn window(&self) -> Rect {
        let (x, y) = (f64::from(self.x), f64::from(self.y));
        Rect::new(
            x - MARGIN,
            y - MARGIN,
            x + f64::from(self.width) + MARGIN,
            y + f64::from(self.height) + MARGIN,
        )
    }

    /// `path` through `transform` (to pixels of the page rendering),
    /// flattened, clipped to the cell's window, then moved to the cell's
    /// origin: closed polygons in the cell's device space. Done in double
    /// precision from the page's coordinates, the same way whatever area
    /// is rendered.
    fn polygons(&self, path: &BezPath, transform: Affine) -> BezPath {
        let window = self.window();
        let (dx, dy) = (f64::from(self.x), f64::from(self.y));
        let mut out = BezPath::new();
        let mut polygon: Vec<KPoint> = Vec::new();
        let flush = |polygon: &mut Vec<KPoint>, out: &mut BezPath| {
            let clipped = clip_polygon(polygon, window);
            polygon.clear();
            let mut points = clipped.into_iter().map(|p| KPoint::new(p.x - dx, p.y - dy));
            if let Some(first) = points.next() {
                out.move_to(first);
                for point in points {
                    out.line_to(point);
                }
                out.close_path();
            }
        };
        kurbo::flatten(path.iter().map(|el| transform * el), TOLERANCE, |el| {
            match el {
                PathEl::MoveTo(point) => {
                    flush(&mut polygon, &mut out);
                    polygon.push(point);
                }
                PathEl::LineTo(point) => polygon.push(point),
                PathEl::ClosePath => flush(&mut polygon, &mut out),
                // `flatten` emits moves, lines and closes only.
                PathEl::QuadTo(..) | PathEl::CurveTo(..) => {}
            }
        });
        flush(&mut polygon, &mut out);
        out
    }
}

/// The canvas of an area of the page rendering: the cells it touches, each
/// rasterised on its own (see the module documentation).
pub(crate) struct Canvas {
    /// Row by row.
    cells: Vec<Cell>,
    /// All the cells.
    all: Span,
    /// The cells each clip pushed was pushed on: those the clip path may
    /// touch among those visible then. The others are clipped out, so the
    /// visible cells are those of the last clip. A clip, a layer or a
    /// drawing only goes to the visible cells it may touch.
    clips: Vec<Span>,
    /// The cells each layer pushed was pushed on.
    layers: Vec<Span>,
    /// The area, in pixels of the page rendering.
    x: u32,
    y: u32,
    width: u16,
    height: u16,
    window: Window,
}

/// Pixels of the cells a canvas for that area has, all rasterised
/// whatever the area: those of whole cells of the grid, cut at the page's
/// sides. Computed before any allocation.
pub(crate) fn cell_pixels(page: (u16, u16), x: u32, y: u32, width: u16, height: u16) -> u64 {
    let side = |start: u32, length: u16, end: u16| {
        let first = start / CELL * CELL;
        let last = ((start + u32::from(length).max(1) - 1) / CELL + 1) * CELL;
        u64::from(last.min(u32::from(end)).saturating_sub(first))
    };
    side(x, width, page.0) * side(y, height, page.1)
}

impl Canvas {
    /// A canvas for the `width` x `height` area at (`x`, `y`) of a page
    /// rendering of `page` pixels, filled with opaque white, the paper
    /// colour. The area is non-empty and inside the page (checked by the
    /// callers).
    pub fn new(page: (u16, u16), x: u32, y: u32, width: u16, height: u16) -> Self {
        let (page_width, page_height) = (u32::from(page.0), u32::from(page.1));
        let (columns, rows) = (
            x / CELL..=(x + u32::from(width).max(1) - 1) / CELL,
            y / CELL..=(y + u32::from(height).max(1) - 1) / CELL,
        );
        let mut cells = Vec::new();
        for row in rows.clone() {
            for column in columns.clone() {
                let (cx, cy) = (column * CELL, row * CELL);
                // Within the page, so each side fits `u16` (at most CELL).
                let side = |start: u32, end: u32| {
                    u16::try_from(CELL.min(end.saturating_sub(start)).max(1)).unwrap_or(1)
                };
                let (cell_width, cell_height) = (side(cx, page_width), side(cy, page_height));
                let mut context = RenderContext::new(cell_width, cell_height);
                context.set_paint(AlphaColor::<Srgb>::WHITE);
                context.fill_rect(&Rect::new(
                    0.0,
                    0.0,
                    f64::from(cell_width),
                    f64::from(cell_height),
                ));
                cells.push(Cell {
                    context,
                    x: cx,
                    y: cy,
                    width: cell_width,
                    height: cell_height,
                });
            }
        }
        let cell = f64::from(CELL);
        let window = Window {
            x0: f64::from(*columns.start()) * cell,
            y0: f64::from(*rows.start()) * cell,
            x1: (f64::from(*columns.end() + 1) * cell).min(f64::from(page_width)),
            y1: (f64::from(*rows.end() + 1) * cell).min(f64::from(page_height)),
            cell,
            margin: MARGIN,
        };
        let all = Span {
            columns: (i64::from(*columns.start()), i64::from(*columns.end())),
            rows: (i64::from(*rows.start()), i64::from(*rows.end())),
        };
        Self {
            cells,
            all,
            clips: Vec::new(),
            layers: Vec::new(),
            x,
            y,
            width,
            height,
            window,
        }
    }

    /// The pixels the cells cover, for work estimates.
    pub fn window(&self) -> Window {
        self.window
    }

    /// Width and height of the area, the size of the output.
    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// The cells whose clipping window meets `bounds` (`Window::columns`).
    fn touched(&self, bounds: Option<Rect>) -> Span {
        let Some(bounds) = bounds else {
            return Span::EMPTY;
        };
        match (
            self.window.columns(bounds.x0, bounds.x1),
            self.window.rows(bounds.y0, bounds.y1),
        ) {
            (Some(columns), Some(rows)) => Span { columns, rows },
            _ => Span::EMPTY,
        }
    }

    fn visible(&self) -> Span {
        self.clips.last().copied().unwrap_or(self.all)
    }

    /// Calls `each` with every cell of `span` (within the canvas).
    fn for_cells(&mut self, span: Span, mut each: impl FnMut(&mut Cell)) {
        let span = span.meet(self.all);
        let columns = self.all.columns.1 - self.all.columns.0 + 1;
        for row in span.rows.0..=span.rows.1 {
            for column in span.columns.0..=span.columns.1 {
                let index = (row - self.all.rows.0) * columns + (column - self.all.columns.0);
                if let Some(cell) = usize::try_from(index)
                    .ok()
                    .and_then(|i| self.cells.get_mut(i))
                {
                    each(cell);
                }
            }
        }
    }

    /// Calls `draw` with each visible cell that `path` through `transform`
    /// may touch, and the path's polygons in that cell.
    fn each_cell(
        &mut self,
        path: &BezPath,
        transform: Affine,
        mut draw: impl FnMut(&mut Cell, &BezPath),
    ) {
        let span = self
            .touched(control_bounds(path, transform))
            .meet(self.visible());
        self.for_cells(span, |cell| {
            let device = cell.polygons(path, transform);
            draw(cell, &device);
        });
    }

    pub fn fill(&mut self, path: &Path, matrix: &Matrix, even_odd: bool, color: Rgba) {
        self.each_cell(&bez_path(path), affine(matrix), |cell, device| {
            cell.context.set_transform(Affine::IDENTITY);
            cell.context.set_fill_rule(fill_rule(even_odd));
            cell.context.set_paint(paint(color));
            cell.context.fill_path(device);
        });
    }

    /// Strokes `path` (ISO 32000-2, 8.5.3.2). Dashes are always cut in
    /// user space along the untransformed path, as PDF defines them. The
    /// outline is then expanded in device space when the CTM is conformal
    /// (rotation, reflection and uniform scale) or the line is a hairline,
    /// so that the flattening tolerance is in device pixels; otherwise it is
    /// expanded in user space, where a non-uniform pen is correct.
    pub fn stroke(&mut self, path: &Path, matrix: &Matrix, style: &StrokeStyle, color: Rgba) {
        let color = paint(color);
        let mut user = bez_path(path);
        if !style.dashes.is_empty() {
            user = path_from(vello_cpu::kurbo::dash(
                user.into_iter(),
                style.dash_phase,
                &style.dashes,
            ));
        }
        let device_width = if style.width > 0.0 {
            conformal_scale(matrix).map(|scale| style.width * scale)
        } else {
            Some(HAIRLINE)
        };
        // The outline is expanded as `vello_cpu` would (same tolerance,
        // scaled down by the matrix in user space), then filled.
        let outline = match device_width {
            Some(width) => {
                let mut device = user;
                device.apply_affine(affine(matrix));
                kurbo::stroke(
                    device,
                    &stroke(style, width),
                    &StrokeOpts::default(),
                    TOLERANCE,
                )
            }
            None => {
                let scale = matrix.a.abs().max(matrix.d.abs()).max(1.0);
                let mut outline = kurbo::stroke(
                    user,
                    &stroke(style, style.width),
                    &StrokeOpts::default(),
                    TOLERANCE / scale,
                );
                outline.apply_affine(affine(matrix));
                outline
            }
        };
        self.each_cell(&outline, Affine::IDENTITY, |cell, device| {
            cell.context.set_paint(color);
            cell.context.set_transform(Affine::IDENTITY);
            cell.context.set_fill_rule(Fill::NonZero);
            cell.context.fill_path(device);
        });
    }

    /// Draws `image` over the unit square of image space mapped by `matrix`
    /// (ISO 32000-2, 8.9.4), its first row at the top, with constant `alpha`.
    /// An image enlarged on both axes is sampled at the nearest pixel unless
    /// `interpolate` (`/Interpolate`, 8.9.5.3) asks for smoothing; any other
    /// is sampled bilinearly, so that a reduced image does not alias.
    pub fn draw_image(
        &mut self,
        image: &Arc<Pixmap>,
        matrix: &Matrix,
        alpha: f32,
        interpolate: bool,
    ) {
        let unit = Rect::new(0.0, 0.0, 1.0, 1.0).to_path(0.1);
        let (width, height) = (f64::from(image.width()), f64::from(image.height()));
        // Image pixel (u, v) to unit square (u / w, 1 - v / h), then device.
        let pixels = Affine::new([1.0 / width, 0.0, 0.0, -1.0 / height, 0.0, 1.0]);
        // Device pixels per image pixel along each image axis.
        let across = matrix.a.hypot(matrix.b) / width;
        let down = matrix.c.hypot(matrix.d) / height;
        let quality = if !interpolate && across >= 1.0 && down >= 1.0 {
            ImageQuality::Low
        } else {
            ImageQuality::Medium
        };
        let transform = affine(matrix);
        self.each_cell(&unit, transform, |cell, device| {
            cell.context.set_paint(Image {
                image: ImageSource::Pixmap(Arc::clone(image)),
                sampler: ImageSampler {
                    x_extend: Extend::Pad,
                    y_extend: Extend::Pad,
                    quality,
                    alpha,
                },
            });
            let origin = Affine::translate((-f64::from(cell.x), -f64::from(cell.y)));
            cell.context.set_transform(Affine::IDENTITY);
            cell.context.set_fill_rule(Fill::NonZero);
            cell.context
                .set_paint_transform(origin * transform * pixels);
            cell.context.fill_path(device);
            cell.context.reset_paint_transform();
        });
    }

    /// Clips the visible cells the clip path may touch; the others are
    /// clipped out until the clip is popped.
    pub fn push_clip(&mut self, path: &Path, matrix: &Matrix, even_odd: bool) {
        let path = bez_path(path);
        let transform = affine(matrix);
        let span = self
            .touched(control_bounds(&path, transform))
            .meet(self.visible());
        self.for_cells(span, |cell| {
            let device = cell.polygons(&path, transform);
            cell.context.set_transform(Affine::IDENTITY);
            cell.context.set_fill_rule(fill_rule(even_odd));
            cell.context.push_clip_path(&device);
        });
        self.clips.push(span);
    }

    pub fn pop_clip(&mut self) {
        if let Some(span) = self.clips.pop() {
            self.for_cells(span, |cell| cell.context.pop_clip());
        }
    }

    /// Starts a layer composited with `opacity` when popped (a
    /// transparency group painted with a constant alpha, ISO 32000-2,
    /// 11.6.4.4). Clips and layers are separate stacks: a clip pushed after
    /// the layer may be popped after it.
    pub fn push_opacity(&mut self, opacity: f32) {
        let span = self.visible();
        self.for_cells(span, |cell| cell.context.push_opacity_layer(opacity));
        self.layers.push(span);
    }

    pub fn pop_layer(&mut self) {
        if let Some(span) = self.layers.pop() {
            self.for_cells(span, |cell| cell.context.pop_layer());
        }
    }

    /// Rasterises each cell and copies its part of the area: RGBA8 with
    /// straight alpha, rows top to bottom.
    pub fn finish(self) -> Vec<u8> {
        let (width, height) = (usize::from(self.width), usize::from(self.height));
        let mut out = vec![0u8; width * height * 4];
        let mut resources = Resources::new();
        for mut cell in self.cells {
            let mut pixmap = Pixmap::new(cell.width, cell.height);
            cell.context.flush();
            cell.context.render(&mut pixmap, &mut resources);
            let rgba = pixmap.take_rgba8(ImageAlphaType::Alpha);
            // The overlap of the cell and the area, in page pixels.
            let x0 = cell.x.max(self.x);
            let x1 = (cell.x + u32::from(cell.width)).min(self.x + u32::from(self.width));
            let y0 = cell.y.max(self.y);
            let y1 = (cell.y + u32::from(cell.height)).min(self.y + u32::from(self.height));
            if x0 >= x1 {
                continue;
            }
            let bytes = (x1 - x0) as usize * 4;
            for y in y0..y1 {
                let from =
                    ((y - cell.y) as usize * usize::from(cell.width) + (x0 - cell.x) as usize) * 4;
                let to = ((y - self.y) as usize * width + (x0 - self.x) as usize) * 4;
                if let (Some(source), Some(target)) =
                    (rgba.get(from..from + bytes), out.get_mut(to..to + bytes))
                {
                    target.copy_from_slice(source);
                }
            }
        }
        out
    }
}

/// A pixmap of straight RGBA8 pixels (`width × height × 4` bytes, checked
/// by the caller).
pub(crate) fn pixmap(rgba: Vec<u8>, width: u16, height: u16, transparent: bool) -> Arc<Pixmap> {
    Arc::new(Pixmap::from_parts(
        rgba,
        width,
        height,
        PixelMetadata {
            may_have_transparency: transparent,
            alpha_type: ImageAlphaType::Alpha,
        },
    ))
}

/// Bounds of the control points of `path` through `transform`: they hold
/// the path; `None` for an empty path or one not finite.
fn control_bounds(path: &BezPath, transform: Affine) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    for element in path.iter() {
        let (points, count) = match transform * element {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => ([p, p, p], 1),
            PathEl::QuadTo(a, b) => ([a, b, b], 2),
            PathEl::CurveTo(a, b, c) => ([a, b, c], 3),
            PathEl::ClosePath => continue,
        };
        for point in points.into_iter().take(count) {
            if !(point.x.is_finite() && point.y.is_finite()) {
                return None;
            }
            bounds = Some(match bounds {
                None => Rect::from_points(point, point),
                Some(b) => Rect::new(
                    b.x0.min(point.x),
                    b.y0.min(point.y),
                    b.x1.max(point.x),
                    b.y1.max(point.y),
                ),
            });
        }
    }
    bounds
}

fn affine(matrix: &Matrix) -> Affine {
    Affine::new(matrix.coefficients())
}

fn fill_rule(even_odd: bool) -> Fill {
    if even_odd {
        Fill::EvenOdd
    } else {
        Fill::NonZero
    }
}

fn paint(color: Rgba) -> AlphaColor<Srgb> {
    AlphaColor::new([color.r, color.g, color.b, color.a])
}

/// The uniform scale of a conformal matrix, `None` for any other.
fn conformal_scale(matrix: &Matrix) -> Option<f64> {
    let scale = (matrix.a * matrix.d - matrix.b * matrix.c).abs().sqrt();
    let tolerance = 1e-9 * scale;
    let rotation =
        (matrix.a - matrix.d).abs() <= tolerance && (matrix.b + matrix.c).abs() <= tolerance;
    let reflection =
        (matrix.a + matrix.d).abs() <= tolerance && (matrix.b - matrix.c).abs() <= tolerance;
    (scale > 0.0 && (rotation || reflection)).then_some(scale)
}

/// A solid pen: dashes are applied to the path beforehand.
fn stroke(style: &StrokeStyle, width: f64) -> Stroke {
    let cap = match style.cap {
        LineCap::Butt => Cap::Butt,
        LineCap::Round => Cap::Round,
        LineCap::Square => Cap::Square,
    };
    let join = match style.join {
        LineJoin::Miter => Join::Miter,
        LineJoin::Round => Join::Round,
        LineJoin::Bevel => Join::Bevel,
    };
    Stroke::new(width)
        .with_caps(cap)
        .with_join(join)
        .with_miter_limit(style.miter_limit)
}

/// Builds a path from `elements`, dropping drawing elements that come before
/// any move: they have no start point, so nothing to draw (`kurbo`'s dash
/// output for a degenerate closed subpath starts so, and `BezPath` panics on
/// it).
fn path_from(elements: impl Iterator<Item = PathEl>) -> BezPath {
    let mut out = BezPath::new();
    for element in elements {
        if out.elements().is_empty() && !matches!(element, PathEl::MoveTo(_)) {
            continue;
        }
        out.push(element);
    }
    out
}

/// Sutherland-Hodgman: `polygon` (implicitly closed) clipped to `window`
/// one side at a time. Points on a side count as inside.
fn clip_polygon(polygon: &[KPoint], window: Rect) -> Vec<KPoint> {
    type Side = (
        fn(KPoint, f64) -> bool,
        fn(KPoint, KPoint, f64) -> KPoint,
        f64,
    );
    let at_x = |a: KPoint, b: KPoint, x: f64| {
        let t = (x - a.x) / (b.x - a.x);
        KPoint::new(x, a.y + t * (b.y - a.y))
    };
    let at_y = |a: KPoint, b: KPoint, y: f64| {
        let t = (y - a.y) / (b.y - a.y);
        KPoint::new(a.x + t * (b.x - a.x), y)
    };
    let sides: [Side; 4] = [
        (|p, x| p.x >= x, at_x, window.x0),
        (|p, x| p.x <= x, at_x, window.x1),
        (|p, y| p.y >= y, at_y, window.y0),
        (|p, y| p.y <= y, at_y, window.y1),
    ];
    let mut points = polygon.to_vec();
    for (inside, cross, edge) in sides {
        let Some(&last) = points.last() else {
            break;
        };
        let mut next = Vec::with_capacity(points.len() + 4);
        let mut previous = last;
        for &point in &points {
            match (inside(previous, edge), inside(point, edge)) {
                (true, true) => next.push(point),
                (true, false) => next.push(cross(previous, point, edge)),
                (false, true) => {
                    next.push(cross(previous, point, edge));
                    next.push(point);
                }
                (false, false) => {}
            }
            previous = point;
        }
        points = next;
    }
    if points.len() < 3 { Vec::new() } else { points }
}

fn bez_path(path: &Path) -> BezPath {
    let point = |p: crate::geometry::Point| KPoint::new(p.x, p.y);
    let mut out = BezPath::new();
    for segment in path.segments() {
        match *segment {
            Segment::MoveTo(p) => out.move_to(point(p)),
            Segment::LineTo(p) => out.line_to(point(p)),
            Segment::CurveTo(a, b, c) => out.curve_to(point(a), point(b), point(c)),
            Segment::Close => out.close_path(),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> Rect {
        Rect::new(0.0, 0.0, 10.0, 10.0)
    }

    #[test]
    fn polygons_inside_the_window_are_kept() {
        let square = [
            KPoint::new(2.0, 2.0),
            KPoint::new(8.0, 2.0),
            KPoint::new(8.0, 8.0),
            KPoint::new(2.0, 8.0),
        ];
        assert_eq!(clip_polygon(&square, window()), square);
        assert!(clip_polygon(&square[..2], window()).is_empty());
    }

    #[test]
    fn far_points_are_cut_at_the_window() {
        // A sliver from x = -3e6 to 3e6 crossing the window at y 4..6.
        let sliver = [
            KPoint::new(-3e6, 4.0),
            KPoint::new(3e6, 4.0),
            KPoint::new(3e6, 6.0),
            KPoint::new(-3e6, 6.0),
        ];
        let clipped = clip_polygon(&sliver, window());
        // Closed window: points on its sides are kept.
        assert!(
            clipped
                .iter()
                .all(|p| (0.0..=10.0).contains(&p.x) && (0.0..=10.0).contains(&p.y))
        );
        let area: f64 = clipped
            .iter()
            .zip(clipped.iter().cycle().skip(1))
            .map(|(a, b)| a.x * b.y - b.x * a.y)
            .sum::<f64>()
            / 2.0;
        assert!((area.abs() - 20.0).abs() < 1e-9, "{area}");
        // Entirely outside: nothing left.
        let outside = [
            KPoint::new(20.0, 20.0),
            KPoint::new(30.0, 20.0),
            KPoint::new(30.0, 30.0),
        ];
        assert!(clip_polygon(&outside, window()).is_empty());
    }

    #[test]
    fn conformal_matrices_are_recognised() {
        let rotation = Matrix::new(0.0, 2.0, -2.0, 0.0, 5.0, 5.0);
        assert_eq!(conformal_scale(&rotation), Some(2.0));
        let page_90 = Matrix::new(0.0, 3.0, 3.0, 0.0, 0.0, 0.0);
        assert_eq!(conformal_scale(&page_90), Some(3.0));
        assert_eq!(
            conformal_scale(&Matrix::new(1.0, 0.0, 0.0, 2.0, 0.0, 0.0)),
            None
        );
        assert_eq!(
            conformal_scale(&Matrix::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0)),
            None
        );
    }
}
