//! Matrices, points and paths in the engine's own types, so that the
//! rasteriser stays behind `raster.rs` (ADR-0003).

/// Affine matrix `[a b c d e f]` with the PDF convention (ISO 32000-2, 8.3.4):
/// `x' = a x + c y + e`, `y' = b x + d y + f`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Matrix {
    pub const IDENTITY: Self = Self::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);

    pub const fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// The transform that applies `self`, then `next`. `cm` with operand `m`
    /// makes the new CTM `m.then(ctm)` (ISO 32000-2, 8.4.4).
    pub fn then(&self, next: &Self) -> Self {
        Self {
            a: self.a * next.a + self.b * next.c,
            b: self.a * next.b + self.b * next.d,
            c: self.c * next.a + self.d * next.c,
            d: self.c * next.b + self.d * next.d,
            e: self.e * next.a + self.f * next.c + next.e,
            f: self.e * next.b + self.f * next.d + next.f,
        }
    }

    pub fn apply(&self, point: Point) -> Point {
        Point {
            x: self.a * point.x + self.c * point.y + self.e,
            y: self.b * point.x + self.d * point.y + self.f,
        }
    }

    pub fn is_finite(&self) -> bool {
        [self.a, self.b, self.c, self.d, self.e, self.f]
            .iter()
            .all(|v| v.is_finite())
    }

    pub fn coefficients(&self) -> [f64; 6] {
        [self.a, self.b, self.c, self.d, self.e, self.f]
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    fn distance(self, other: Self) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

/// One path element, in user space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Segment {
    MoveTo(Point),
    LineTo(Point),
    CurveTo(Point, Point, Point),
    Close,
}

/// A path under construction (ISO 32000-2, 8.5.2).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Path {
    segments: Vec<Segment>,
    /// Start of the current subpath, if any.
    start: Option<Point>,
    /// Current point, if any.
    current: Option<Point>,
}

impl Path {
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn current_point(&self) -> Option<Point> {
        self.current
    }

    pub fn clear(&mut self) {
        self.segments.clear();
        self.start = None;
        self.current = None;
    }

    pub fn move_to(&mut self, point: Point) {
        // Consecutive `m` replace the previous one (ISO 32000-2, 8.5.2.1).
        if let Some(Segment::MoveTo(last)) = self.segments.last_mut() {
            *last = point;
        } else {
            self.segments.push(Segment::MoveTo(point));
        }
        self.start = Some(point);
        self.current = Some(point);
    }

    /// Returns `false`, changing nothing, without a current point.
    pub fn line_to(&mut self, point: Point) -> bool {
        if self.current.is_none() {
            return false;
        }
        self.segments.push(Segment::LineTo(point));
        self.current = Some(point);
        true
    }

    /// Returns `false`, changing nothing, without a current point.
    pub fn curve_to(&mut self, c1: Point, c2: Point, end: Point) -> bool {
        if self.current.is_none() {
            return false;
        }
        self.segments.push(Segment::CurveTo(c1, c2, end));
        self.current = Some(end);
        true
    }

    /// Closes the current subpath; nothing to do without one.
    pub fn close(&mut self) {
        let Some(start) = self.start else {
            return;
        };
        if !matches!(self.segments.last(), Some(Segment::Close)) {
            self.segments.push(Segment::Close);
        }
        self.current = Some(start);
    }

    pub fn rectangle(&mut self, x: f64, y: f64, width: f64, height: f64) {
        self.move_to(Point::new(x, y));
        self.segments
            .push(Segment::LineTo(Point::new(x + width, y)));
        self.segments
            .push(Segment::LineTo(Point::new(x + width, y + height)));
        self.segments
            .push(Segment::LineTo(Point::new(x, y + height)));
        self.segments.push(Segment::Close);
        self.current = Some(Point::new(x, y));
    }

    /// Every point of the path through `matrix`, padded by `pad`, lies
    /// within `limit` of the device origin (and is therefore finite).
    pub fn is_within(&self, matrix: &Matrix, pad: f64, limit: f64) -> bool {
        self.points().all(|p| {
            let q = matrix.apply(p);
            q.x.abs() + pad <= limit && q.y.abs() + pad <= limit
        })
    }

    fn points(&self) -> impl Iterator<Item = Point> + '_ {
        self.segments.iter().flat_map(|segment| {
            let (points, count): ([Point; 3], usize) = match *segment {
                Segment::MoveTo(p) | Segment::LineTo(p) => ([p, p, p], 1),
                Segment::CurveTo(a, b, c) => ([a, b, c], 3),
                Segment::Close => ([Point::new(0.0, 0.0); 3], 0),
            };
            points.into_iter().take(count)
        })
    }

    /// Estimated rasteriser work to paint this path through `matrix` on a
    /// canvas covering `window`, each point widened by `pad` device pixels
    /// (half a stroke width, 0 for a fill or a clip). An upper-bound style
    /// estimate, in units of about one line segment or one tile:
    /// - flattening: one per segment, plus for a curve the square root of
    ///   its control polygon length in device pixels over the flattening
    ///   tolerance (0.25 px);
    /// - strips: each segment crosses about a quarter of its device extent
    ///   (clipped to the canvas) in 4-pixel tiles;
    /// - filling: the clipped bounding box area over 1024 pixels (one wide
    ///   tile command per 256 x 4 pixels), plus its compositing;
    /// - cells: each cell of the window that the bounding box touches
    ///   flattens and clips the path again.
    pub fn work(&self, matrix: &Matrix, pad: f64, window: &Window) -> f64 {
        let clip =
            |low: f64, high: f64, start: f64, end: f64| (high.min(end) - low.max(start)).max(0.0);
        let width = |low: f64, high: f64| clip(low, high, window.x0, window.x1);
        let height = |low: f64, high: f64| clip(low, high, window.y0, window.y1);
        // Flattening, done again in every cell.
        let mut segments = 0.0;
        let mut work = 0.0;
        let mut current = Point::new(0.0, 0.0);
        let mut bounds = Bounds::EMPTY;
        for segment in &self.segments {
            let (points, count): ([Point; 3], usize) = match *segment {
                Segment::MoveTo(p) => {
                    current = matrix.apply(p);
                    segments += 1.0;
                    continue;
                }
                Segment::LineTo(p) => ([matrix.apply(p); 3], 1),
                Segment::CurveTo(a, b, c) => {
                    ([matrix.apply(a), matrix.apply(b), matrix.apply(c)], 3)
                }
                Segment::Close => {
                    segments += 1.0;
                    continue;
                }
            };
            let mut segment_bounds = Bounds::EMPTY;
            segment_bounds.add(current);
            let mut length = 0.0;
            let mut previous = current;
            for point in points.iter().take(count) {
                segment_bounds.add(*point);
                length += previous.distance(*point);
                previous = *point;
            }
            if count == 3 {
                segments += (length * 4.0).sqrt();
            }
            let extent = width(segment_bounds.x0 - pad, segment_bounds.x1 + pad)
                + height(segment_bounds.y0 - pad, segment_bounds.y1 + pad);
            segments += 1.0;
            work += extent / 4.0;
            bounds.merge(&segment_bounds);
            current = previous;
        }
        let mut cells = 1.0;
        if bounds.x0 <= bounds.x1 {
            let (x0, x1) = (bounds.x0 - pad, bounds.x1 + pad);
            let (y0, y1) = (bounds.y0 - pad, bounds.y1 + pad);
            work += width(x0, x1) * height(y0, y1) / 1024.0;
            let count = |span: Option<(i64, i64)>| span.map_or(0.0, |(a, b)| (b - a + 1) as f64);
            cells = (count(window.columns(x0, x1)) * count(window.rows(y0, y1))).max(1.0);
        }
        work + segments * cells
    }

    /// Length of the control polygon in user space: an upper bound of the
    /// arc length, used to bound the number of dashes.
    pub fn polygon_length(&self) -> f64 {
        let mut length = 0.0;
        let mut start = Point::new(0.0, 0.0);
        let mut current = start;
        for segment in &self.segments {
            match *segment {
                Segment::MoveTo(p) => {
                    start = p;
                    current = p;
                }
                Segment::LineTo(p) => {
                    length += current.distance(p);
                    current = p;
                }
                Segment::CurveTo(a, b, c) => {
                    length += current.distance(a) + a.distance(b) + b.distance(c);
                    current = c;
                }
                Segment::Close => {
                    length += current.distance(start);
                    current = start;
                }
            }
        }
        length
    }
}

/// The device pixels a canvas covers, in the coordinates of the whole page
/// rendering, cut into square cells of side `cell` from its top-left corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub cell: f64,
    /// How far each cell clips beyond its sides.
    pub margin: f64,
}

impl Window {
    /// The columns of cells (numbered from the page's left side) whose
    /// clipping window, `[c × cell − margin, (c + 1) × cell + margin]`,
    /// meets `low..=high`, within this window; `None` if there is none.
    /// The rasteriser draws in exactly these cells, so that the work
    /// estimate counts them.
    pub fn columns(&self, low: f64, high: f64) -> Option<(i64, i64)> {
        self.cells(low, high, self.x0, self.x1)
    }

    /// The rows of cells, as `columns`.
    pub fn rows(&self, low: f64, high: f64) -> Option<(i64, i64)> {
        self.cells(low, high, self.y0, self.y1)
    }

    fn cells(&self, low: f64, high: f64, start: f64, end: f64) -> Option<(i64, i64)> {
        // Cell c meets the range when c × cell − margin ≤ high and
        // low ≤ (c + 1) × cell + margin. Casts saturate.
        let first = ((low - self.margin) / self.cell - 1.0)
            .ceil()
            .max((start / self.cell).floor());
        let last = ((high + self.margin) / self.cell)
            .floor()
            .min((end / self.cell).ceil() - 1.0);
        (first <= last).then_some((first as i64, last as i64))
    }
}

/// Axis-aligned bounds; empty while `x0 > x1`.
#[derive(Debug, Clone, Copy)]
struct Bounds {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Bounds {
    const EMPTY: Self = Self {
        x0: f64::INFINITY,
        y0: f64::INFINITY,
        x1: f64::NEG_INFINITY,
        y1: f64::NEG_INFINITY,
    };

    fn add(&mut self, p: Point) {
        self.x0 = self.x0.min(p.x);
        self.y0 = self.y0.min(p.y);
        self.x1 = self.x1.max(p.x);
        self.y1 = self.y1.max(p.y);
    }

    fn merge(&mut self, other: &Self) {
        self.x0 = self.x0.min(other.x0);
        self.y0 = self.y0.min(other.y0);
        self.x1 = self.x1.max(other.x1);
        self.y1 = self.y1.max(other.y1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn then_applies_left_first() {
        let scale = Matrix::new(2.0, 0.0, 0.0, 3.0, 0.0, 0.0);
        let shift = Matrix::new(1.0, 0.0, 0.0, 1.0, 10.0, 20.0);
        let p = scale.then(&shift).apply(Point::new(1.0, 1.0));
        assert_eq!(p, Point::new(12.0, 23.0));
        let q = shift.then(&scale).apply(Point::new(1.0, 1.0));
        assert_eq!(q, Point::new(22.0, 63.0));
    }

    #[test]
    fn path_construction_rules() {
        let mut path = Path::default();
        assert!(!path.line_to(Point::new(1.0, 1.0)));
        path.close();
        assert!(path.is_empty());
        path.move_to(Point::new(0.0, 0.0));
        path.move_to(Point::new(5.0, 5.0));
        assert_eq!(path.segments(), &[Segment::MoveTo(Point::new(5.0, 5.0))]);
        assert!(path.line_to(Point::new(6.0, 5.0)));
        path.close();
        path.close();
        assert_eq!(path.segments().len(), 3);
        assert_eq!(path.current_point(), Some(Point::new(5.0, 5.0)));
        path.rectangle(0.0, 0.0, 3.0, 4.0);
        assert_eq!(path.current_point(), Some(Point::new(0.0, 0.0)));
        assert_eq!(path.polygon_length(), 2.0 + 14.0);
    }

    #[test]
    fn work_counts_curves_crossings_and_area() {
        let canvas = Window {
            x0: 0.0,
            y0: 0.0,
            x1: 1000.0,
            y1: 1000.0,
            cell: 1000.0,
            margin: 0.0,
        };
        let work = |path: &Path, pad| path.work(&Matrix::IDENTITY, pad, &canvas);
        let mut path = Path::default();
        // A line outside the canvas: one move, one segment, nothing covered.
        path.move_to(Point::new(-5000.0, -10.0));
        path.line_to(Point::new(-10.0, -5000.0));
        assert_eq!(work(&path, 0.0), 2.0);
        // A long line across the canvas: its clipped extent in 4-pixel tiles.
        path.clear();
        path.move_to(Point::new(0.0, 0.0));
        path.line_to(Point::new(1e12, 1e12));
        assert_eq!(work(&path, 0.0), 1.0 + 1.0 + 2000.0 / 4.0 + 1e6 / 1024.0);
        // A padded stroke covers more.
        path.clear();
        path.move_to(Point::new(100.0, 100.0));
        path.line_to(Point::new(200.0, 100.0));
        assert!(work(&path, 10.0) > work(&path, 0.0));
        // A huge curve costs the square root of its size.
        path.clear();
        path.move_to(Point::new(0.0, 0.0));
        path.curve_to(
            Point::new(1e12, 1e12),
            Point::new(0.0, 1e12),
            Point::new(0.0, 0.0),
        );
        assert!(work(&path, 0.0) > 1e6);
        // Cut into 250-pixel cells, a path spanning all 16 flattens 16 times;
        // the window's position counts, not its origin.
        path.clear();
        path.move_to(Point::new(0.0, 0.0));
        path.line_to(Point::new(1e12, 1e12));
        let cells = Window {
            cell: 250.0,
            ..canvas
        };
        let whole = path.work(&Matrix::IDENTITY, 0.0, &canvas);
        assert_eq!(
            path.work(&Matrix::IDENTITY, 0.0, &cells),
            whole + 2.0 * 15.0
        );
        let shifted = Window {
            x0: 500.0,
            y0: 500.0,
            x1: 1500.0,
            y1: 1500.0,
            cell: 250.0,
            margin: 0.0,
        };
        // From (500, 500): the line covers 4 x 4 cells of this window too.
        assert_eq!(
            path.work(&Matrix::IDENTITY, 0.0, &shifted),
            1.0 + 1.0 + 2000.0 / 4.0 + 1e6 / 1024.0 + 2.0 * 15.0
        );
    }

    #[test]
    fn cells_count_their_margin_and_inclusive_sides() {
        let window = Window {
            x0: 0.0,
            y0: 0.0,
            x1: 1000.0,
            y1: 600.0,
            cell: 256.0,
            margin: 2.0,
        };
        // Inside cell 0, but within 2 pixels of cell 1.
        assert_eq!(window.columns(100.0, 255.0), Some((0, 1)));
        assert_eq!(window.columns(100.0, 253.0), Some((0, 0)));
        // Just right of cell 0's side: still in its window.
        assert_eq!(window.columns(256.0, 300.0), Some((0, 1)));
        assert_eq!(window.columns(258.0, 300.0), Some((0, 1)));
        assert_eq!(window.columns(258.5, 300.0), Some((1, 1)));
        // Clamped to the window: 1000 pixels are cells 0 to 3.
        assert_eq!(window.columns(-1e12, 1e12), Some((0, 3)));
        assert_eq!(window.columns(2000.0, 3000.0), None);
        assert_eq!(window.rows(590.0, 700.0), Some((2, 2)));
        // A square at a corner of four cells counts them.
        let mut path = Path::default();
        path.rectangle(254.0, 254.0, 4.0, 4.0);
        let single = Window {
            cell: 1e6,
            ..window
        };
        assert_eq!(
            path.work(&Matrix::IDENTITY, 0.0, &window),
            // Five segments (move, three lines, close), flattened in 3 more cells.
            path.work(&Matrix::IDENTITY, 0.0, &single) + 3.0 * 5.0
        );
    }

    #[test]
    fn bounds_are_checked_with_padding() {
        let mut path = Path::default();
        path.move_to(Point::new(0.0, 90.0));
        assert!(path.is_within(&Matrix::IDENTITY, 10.0, 100.0));
        assert!(!path.is_within(&Matrix::IDENTITY, 10.1, 100.0));
        path.line_to(Point::new(f64::NAN, 0.0));
        assert!(!path.is_within(&Matrix::IDENTITY, 0.0, 100.0));
    }
}
