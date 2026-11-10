//! The only module that touches `vello_cpu` (ADR-0003): replacing the
//! rasteriser, for example by `tiny-skia`, means rewriting this file.
//!
//! The adapter only uses fills, strokes and the clip stack. It never pushes
//! a layer: `vello_cpu` asserts that every layer is popped before rendering,
//! whereas pending clip paths are allowed.
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

use crate::geometry::{Matrix, Path, Segment};

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

pub(crate) struct Canvas {
    context: RenderContext,
    width: u16,
    height: u16,
}

impl Canvas {
    /// A canvas filled with opaque white, the paper colour.
    pub fn new(width: u16, height: u16) -> Self {
        let mut context = RenderContext::new(width, height);
        context.set_paint(AlphaColor::<Srgb>::WHITE);
        context.fill_rect(&Rect::new(0.0, 0.0, f64::from(width), f64::from(height)));
        Self {
            context,
            width,
            height,
        }
    }

    /// Width and height in pixels.
    pub fn size(&self) -> (f64, f64) {
        (f64::from(self.width), f64::from(self.height))
    }

    pub fn fill(&mut self, path: &Path, matrix: &Matrix, even_odd: bool, color: Rgba) {
        let device = self.device_polygons(bez_path(path), affine(matrix));
        self.context.set_transform(Affine::IDENTITY);
        self.context.set_fill_rule(fill_rule(even_odd));
        self.context.set_paint(paint(color));
        self.context.fill_path(&device);
    }

    /// `path` through `transform`, flattened and clipped to the canvas
    /// window: closed polygons in device space.
    fn device_polygons(&self, path: BezPath, transform: Affine) -> BezPath {
        let window = Rect::new(
            -MARGIN,
            -MARGIN,
            f64::from(self.width) + MARGIN,
            f64::from(self.height) + MARGIN,
        );
        let mut out = BezPath::new();
        let mut polygon: Vec<KPoint> = Vec::new();
        let flush = |polygon: &mut Vec<KPoint>, out: &mut BezPath| {
            let clipped = clip_polygon(polygon, window);
            polygon.clear();
            let mut points = clipped.into_iter();
            if let Some(first) = points.next() {
                out.move_to(first);
                for point in points {
                    out.line_to(point);
                }
                out.close_path();
            }
        };
        kurbo::flatten(path.into_iter().map(|el| transform * el), TOLERANCE, |el| {
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

    /// Strokes `path` (ISO 32000-2, 8.5.3.2). Dashes are always cut in
    /// user space along the untransformed path, as PDF defines them. The
    /// outline is then expanded in device space when the CTM is conformal
    /// (rotation, reflection and uniform scale) or the line is a hairline,
    /// so that the flattening tolerance is in device pixels; otherwise it is
    /// expanded in user space, where a non-uniform pen is correct.
    pub fn stroke(&mut self, path: &Path, matrix: &Matrix, style: &StrokeStyle, color: Rgba) {
        self.context.set_paint(paint(color));
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
        let device = self.device_polygons(outline, Affine::IDENTITY);
        self.context.set_transform(Affine::IDENTITY);
        self.context.set_fill_rule(Fill::NonZero);
        self.context.fill_path(&device);
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
        let device = self.device_polygons(unit, affine(matrix));
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
        self.context.set_paint(Image {
            image: ImageSource::Pixmap(Arc::clone(image)),
            sampler: ImageSampler {
                x_extend: Extend::Pad,
                y_extend: Extend::Pad,
                quality,
                alpha,
            },
        });
        self.context.set_transform(Affine::IDENTITY);
        self.context.set_fill_rule(Fill::NonZero);
        self.context.set_paint_transform(affine(matrix) * pixels);
        self.context.fill_path(&device);
        self.context.reset_paint_transform();
    }

    pub fn push_clip(&mut self, path: &Path, matrix: &Matrix, even_odd: bool) {
        let device = self.device_polygons(bez_path(path), affine(matrix));
        self.context.set_transform(Affine::IDENTITY);
        self.context.set_fill_rule(fill_rule(even_odd));
        self.context.push_clip_path(&device);
    }

    pub fn pop_clip(&mut self) {
        self.context.pop_clip();
    }

    /// Rasterises the scene: RGBA8 with straight alpha, rows top to bottom.
    pub fn finish(mut self) -> Vec<u8> {
        let mut pixmap = Pixmap::new(self.width, self.height);
        let mut resources = Resources::new();
        self.context.flush();
        self.context.render(&mut pixmap, &mut resources);
        pixmap.take_rgba8(ImageAlphaType::Alpha)
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
