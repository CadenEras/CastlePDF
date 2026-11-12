//! Words and lines of a page's text (ADR-0006, tranche 3.5a), from the
//! glyphs `page_text` records, in content order: no reading order across
//! columns yet (tranche 3.5b).
//!
//! Each glyph advances along a direction, the vector from its origin to its
//! end: rightwards for horizontal text, down for vertical text, any other way
//! for rotated text. A glyph continues the current line when it starts on its
//! baseline (within half the font size across it), goes the same way and does
//! not jump back by more than its size nor forward by more than `JOIN_GAP`
//! sizes (a gutter or a table cell: `reading_lines` joins near pieces back);
//! otherwise it starts a line. Within a
//! line, a glyph whose text is white space, or a gap along the baseline wider
//! than `WORD_GAP` times the font size, separates words. An `/ActualText`
//! span replaces its glyphs as one unit at their place; a span without
//! glyphs (an image of text) is a word of its own on the current line.

use std::ops::Range;

use crate::{PageText, TextGlyph};

/// Gap along the baseline, in font sizes, that separates two words: a
/// space is usually a quarter to a third of the size, letter spacing far
/// less.
pub const WORD_GAP: f64 = 0.15;
/// Offset across the baseline, in font sizes, beyond which a glyph starts a
/// new line.
const LINE_OFFSET: f64 = 0.5;
/// Cosine above which two directions are the same.
const SAME_DIRECTION: f64 = 0.9;
/// Share of the font size above the baseline and below it, for bounds.
const ASCENT: f64 = 0.8;
const DESCENT: f64 = 0.2;

/// A word: its text, the glyphs it comes from, and its bounds.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TextWord {
    pub text: String,
    /// Indices into `PageText::glyphs`.
    pub glyphs: Range<usize>,
    /// Axis-aligned bounds in default user space: left, bottom, right, top,
    /// from the glyphs' baseline, advance and size (not their outlines).
    pub bounds: [f64; 4],
}

/// A line: its words in content order.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TextLine {
    pub words: Vec<TextWord>,
    /// Unit direction of its baseline in default user space: `[1, 0]` for
    /// horizontal text, `[0, -1]` for vertical text.
    pub direction: [f64; 2],
}

impl TextLine {
    /// The words joined by single spaces.
    pub fn text(&self) -> String {
        let words: Vec<&str> = self.words.iter().map(|word| word.text.as_str()).collect();
        words.join(" ")
    }

    /// The union of its words' bounds: left, bottom, right, top.
    pub fn bounds(&self) -> [f64; 4] {
        self.words.iter().fold(
            [
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ],
            |bounds, word| union(bounds, word.bounds),
        )
    }

    /// Horizontal left-to-right text.
    fn horizontal(&self) -> bool {
        self.direction[0] > SAME_DIRECTION
    }
}

/// One unit of text: a glyph, or an `/ActualText` span standing for its
/// glyphs. `start`/`end` are `None` for a span without glyphs.
struct Item<'a> {
    text: &'a str,
    glyphs: Range<usize>,
    geometry: Option<Geometry>,
}

#[derive(Clone, Copy)]
struct Geometry {
    start: (f64, f64),
    end: (f64, f64),
    size: f64,
}

/// The words and lines of `text`, in content order.
pub fn text_lines(text: &PageText) -> Vec<TextLine> {
    let mut builder = Builder::default();
    let mut spans = text.actual_texts.iter().peekable();
    let mut index = 0;
    while index < text.glyphs.len() || spans.peek().is_some() {
        // A span starting here (possibly without glyphs) replaces its glyphs.
        if let Some(span) = spans.next_if(|span| span.glyphs.start <= index) {
            let glyphs = text.glyphs.get(span.glyphs.clone()).unwrap_or_default();
            builder.push(Item {
                text: &span.text,
                glyphs: span.glyphs.clone(),
                geometry: span_geometry(glyphs),
            });
            index = index.max(span.glyphs.end);
            continue;
        }
        let Some(glyph) = text.glyphs.get(index) else {
            break;
        };
        builder.push(Item {
            text: glyph.text.as_deref().unwrap_or("\u{FFFD}"),
            glyphs: index..index + 1,
            geometry: Some(Geometry {
                start: (glyph.x, glyph.y),
                end: (glyph.end_x, glyph.end_y),
                size: glyph.size,
            }),
        });
        index += 1;
    }
    builder.finish()
}

/// The geometry of a span: from its first glyph's origin to its last
/// glyph's end, at their largest size.
fn span_geometry(glyphs: &[TextGlyph]) -> Option<Geometry> {
    let (first, last) = (glyphs.first()?, glyphs.last()?);
    let size = glyphs.iter().map(|g| g.size).fold(0.0, f64::max);
    Some(Geometry {
        start: (first.x, first.y),
        end: (last.end_x, last.end_y),
        size,
    })
}

#[derive(Default)]
struct Builder {
    lines: Vec<TextLine>,
    words: Vec<TextWord>,
    word: Option<TextWord>,
    /// Baseline of the current line: a point and a unit direction.
    baseline: Option<((f64, f64), (f64, f64))>,
    /// Where the last item ended, and its size.
    last: Option<((f64, f64), f64)>,
}

impl Builder {
    fn push(&mut self, item: Item<'_>) {
        let Some(geometry) = item.geometry else {
            // No place of its own (an /ActualText without glyphs): a word of
            // its own on the current line.
            self.end_word();
            self.append(&item, None);
            self.end_word();
            return;
        };
        let direction = unit(
            geometry.end.0 - geometry.start.0,
            geometry.end.1 - geometry.start.1,
        );
        match (self.baseline, self.last) {
            (Some((origin, along)), Some((end, last_size))) => {
                let size = last_size.max(geometry.size).max(f64::MIN_POSITIVE);
                let offset = (geometry.start.0 - origin.0, geometry.start.1 - origin.1);
                let across = (offset.0 * along.1 - offset.1 * along.0).abs();
                let gap =
                    (geometry.start.0 - end.0) * along.0 + (geometry.start.1 - end.1) * along.1;
                let turns =
                    direction.is_some_and(|d| d.0 * along.0 + d.1 * along.1 < SAME_DIRECTION);
                if across > LINE_OFFSET * size || gap < -size || gap > JOIN_GAP * size || turns {
                    self.end_line();
                    self.baseline = Some((geometry.start, direction.unwrap_or(along)));
                } else if gap > WORD_GAP * size {
                    self.end_word();
                }
            }
            _ => {
                self.baseline = Some((geometry.start, direction.unwrap_or((1.0, 0.0))));
            }
        }
        self.last = Some((geometry.end, geometry.size));
        if item.text.chars().all(char::is_whitespace) {
            self.end_word();
            return;
        }
        let along = self.baseline.map_or((1.0, 0.0), |(_, along)| along);
        self.append(&item, Some(bounds(&geometry, along)));
    }

    fn append(&mut self, item: &Item<'_>, bounds: Option<[f64; 4]>) {
        match &mut self.word {
            Some(word) => {
                word.text.push_str(item.text);
                word.glyphs.end = word.glyphs.end.max(item.glyphs.end);
                if let Some(b) = bounds {
                    word.bounds = union(word.bounds, b);
                }
            }
            None => {
                let bounds = bounds
                    .or_else(|| self.last.map(|((x, y), _)| [x, y, x, y]))
                    .unwrap_or([0.0; 4]);
                self.word = Some(TextWord {
                    text: item.text.to_owned(),
                    glyphs: item.glyphs.clone(),
                    bounds,
                });
            }
        }
    }

    fn end_word(&mut self) {
        if let Some(word) = self.word.take() {
            self.words.push(word);
        }
    }

    fn end_line(&mut self) {
        self.end_word();
        if !self.words.is_empty() {
            let along = self.baseline.map_or((1.0, 0.0), |(_, along)| along);
            self.lines.push(TextLine {
                words: std::mem::take(&mut self.words),
                direction: [along.0, along.1],
            });
        }
    }

    fn finish(mut self) -> Vec<TextLine> {
        self.end_line();
        self.lines
    }
}

/// `(x, y)` scaled to length 1, or `None` for a (near) zero vector.
fn unit(x: f64, y: f64) -> Option<(f64, f64)> {
    let length = x.hypot(y);
    (length > 1e-9 && length.is_finite()).then(|| (x / length, y / length))
}

/// Bounds of a glyph from its baseline segment, extended across it by the
/// ascent and descent of its size.
fn bounds(geometry: &Geometry, along: (f64, f64)) -> [f64; 4] {
    // Perpendicular, pointing up for left-to-right text.
    let up = (-along.1, along.0);
    let (ascent, descent) = (geometry.size * ASCENT, geometry.size * DESCENT);
    let mut result = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for (x, y) in [geometry.start, geometry.end] {
        for k in [ascent, -descent] {
            let (px, py) = (x + up.0 * k, y + up.1 * k);
            result = union(result, [px, py, px, py]);
        }
    }
    result
}

fn union(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

/// Gap along the baseline, in font sizes, up to which two pieces of a
/// horizontal line shown apart are joined: wider than a word space, narrower
/// than most column gutters.
const JOIN_GAP: f64 = 1.0;
/// Baseline difference, in font sizes, within which two pieces are on the
/// same line.
const SAME_BASELINE: f64 = 0.25;
/// Most lines ordered by cuts; beyond, lines are sorted top to bottom,
/// then left to right (a bound on the cuts' quadratic worst case).
pub const MAX_ORDERED_LINES: usize = 10_000;
/// Share of the widest gap within which another gap cuts with it.
const GAP_TOLERANCE: f64 = 0.01;

/// The words and lines of `text` in reading order (ADR-0006, tranche 3.5b):
/// the lines of `text_lines`, with the pieces of one horizontal line shown
/// apart joined, ordered by recursive cuts along the widest empty bands
/// (XY-cut): a horizontal band splits rows read top to bottom, a vertical
/// band splits columns read left to right, whichever band is wider first.
/// A title over two columns is read before them, each column before the
/// next. Pages written right to left or top to bottom keep their lines but
/// are ordered as if left to right.
pub fn reading_lines(text: &PageText) -> Vec<TextLine> {
    let lines = join_lines(text_lines(text));
    if lines.len() > MAX_ORDERED_LINES {
        return sorted(lines);
    }
    let bounds: Vec<[f64; 4]> = lines.iter().map(TextLine::bounds).collect();
    let order = xy_cut(&bounds);
    let mut slots: Vec<Option<TextLine>> = lines.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|index| slots.get_mut(index).and_then(Option::take))
        .collect()
}

/// Lines top to bottom, then left to right.
fn sorted(mut lines: Vec<TextLine>) -> Vec<TextLine> {
    lines.sort_by(|a, b| {
        let (a, b) = (a.bounds(), b.bounds());
        b[3].total_cmp(&a[3]).then(a[0].total_cmp(&b[0]))
    });
    lines
}

/// Joins horizontal lines on the same baseline that follow each other
/// within `JOIN_GAP` font sizes, their words in left-to-right order. Other
/// lines are kept as they are.
fn join_lines(lines: Vec<TextLine>) -> Vec<TextLine> {
    let (mut horizontal, others): (Vec<TextLine>, Vec<TextLine>) =
        lines.into_iter().partition(TextLine::horizontal);
    // Baseline and size of a horizontal line, from its bounds.
    let metrics = |line: &TextLine| {
        let [_, bottom, _, top] = line.bounds();
        let size = (top - bottom).max(f64::MIN_POSITIVE);
        (bottom + DESCENT * size, size)
    };
    horizontal.sort_by(|a, b| {
        let ((a_base, _), (b_base, _)) = (metrics(a), metrics(b));
        b_base
            .total_cmp(&a_base)
            .then(a.bounds()[0].total_cmp(&b.bounds()[0]))
    });
    let mut joined: Vec<TextLine> = Vec::with_capacity(horizontal.len() + others.len());
    for line in horizontal {
        if let Some(last) = joined.last_mut() {
            let ((base, size), (line_base, line_size)) = (metrics(last), metrics(&line));
            let size = size.max(line_size);
            let gap = line.bounds()[0] - last.bounds()[2];
            if (base - line_base).abs() <= SAME_BASELINE * size
                && (-size..=JOIN_GAP * size).contains(&gap)
            {
                last.words.extend(line.words);
                continue;
            }
        }
        joined.push(line);
    }
    joined.extend(others);
    joined
}

/// Reading order of boxes (left, bottom, right, top) by recursive XY-cut,
/// with an explicit stack: indices into `bounds`.
fn xy_cut(bounds: &[[f64; 4]]) -> Vec<usize> {
    let mut order = Vec::with_capacity(bounds.len());
    // Groups still to cut, the next to read last.
    let mut pending: Vec<Vec<usize>> = vec![(0..bounds.len()).collect()];
    while let Some(group) = pending.pop() {
        let parts = cut(bounds, group);
        match parts {
            Cut::Done(indices) => order.extend(indices),
            Cut::Parts(parts) => pending.extend(parts.into_iter().rev()),
        }
    }
    order
}

enum Cut {
    /// Read in this order.
    Done(Vec<usize>),
    /// Cut further, in reading order.
    Parts(Vec<Vec<usize>>),
}

/// Cuts `group` along every empty band of the axis with the widest one: by
/// rows (top to bottom) or by columns (left to right). Without any band,
/// the group is read top to bottom, then left to right.
fn cut(bounds: &[[f64; 4]], mut group: Vec<usize>) -> Cut {
    let bound = |index: &usize| bounds.get(*index).copied().unwrap_or([0.0; 4]);
    if group.len() <= 1 {
        return Cut::Done(group);
    }
    // Rows: by top, descending; a band opens where the next box's top is
    // below every bottom so far.
    let rows = bands(&group, |i| {
        let [_, bottom, _, top] = bound(i);
        (-top, -bottom)
    });
    let columns = bands(&group, |i| {
        let [left, _, right, _] = bound(i);
        (left, right)
    });
    let widest = |parts: &Option<(f64, Vec<Vec<usize>>)>| parts.as_ref().map_or(0.0, |p| p.0);
    let chosen = if widest(&rows) >= widest(&columns) {
        rows
    } else {
        columns
    };
    match chosen {
        Some((_, parts)) => Cut::Parts(parts),
        None => {
            group.sort_by(|a, b| {
                let (a, b) = (bound(a), bound(b));
                b[3].total_cmp(&a[3]).then(a[0].total_cmp(&b[0]))
            });
            Cut::Done(group)
        }
    }
}

/// Splits `group` along one axis at its widest empty bands: `span` gives
/// each box's start and end along it, in reading order (a higher top is
/// negated to come first). Only the widest gaps cut (those within
/// `GAP_TOLERANCE` of it, such as the even leading of a paragraph): a
/// narrower one, like the space between two lines of a column, must not
/// cut across a wider gutter. Returns the widest gap and the parts, or
/// `None` without a gap.
fn bands(group: &[usize], span: impl Fn(&usize) -> (f64, f64)) -> Option<(f64, Vec<Vec<usize>>)> {
    let mut sorted: Vec<(f64, f64, usize)> = group
        .iter()
        .map(|index| {
            let (start, end) = span(index);
            (start, end, *index)
        })
        .collect();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Each box with the gap before it (0 where it overlaps the boxes before).
    let mut reach = f64::NEG_INFINITY;
    let mut gaps: Vec<(f64, usize)> = Vec::with_capacity(sorted.len());
    for (start, end, index) in sorted {
        let gap = if reach.is_finite() {
            start - reach
        } else {
            0.0
        };
        gaps.push((gap.max(0.0), index));
        reach = reach.max(end);
    }
    let widest = gaps.iter().map(|(gap, _)| *gap).fold(0.0, f64::max);
    if !(widest > 0.0 && widest.is_finite()) {
        return None;
    }
    let threshold = widest * (1.0 - GAP_TOLERANCE);
    let mut parts: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    for (gap, index) in gaps {
        if !current.is_empty() && gap >= threshold {
            parts.push(std::mem::take(&mut current));
        }
        current.push(index);
    }
    parts.push(current);
    (parts.len() > 1).then_some((widest, parts))
}
