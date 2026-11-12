//! Words and lines of a page's text (ADR-0006, tranche 3.5a), from the
//! glyphs `page_text` records, in content order: no reading order across
//! columns yet (tranche 3.5b).
//!
//! Each glyph advances along a direction, the vector from its origin to its
//! end: rightwards for horizontal text, down for vertical text, any other way
//! for rotated text. A glyph continues the current line when it starts on its
//! baseline (within half the font size across it), goes the same way and does
//! not jump back by more than its size; otherwise it starts a line. Within a
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
}

impl TextLine {
    /// The words joined by single spaces.
    pub fn text(&self) -> String {
        let words: Vec<&str> = self.words.iter().map(|word| word.text.as_str()).collect();
        words.join(" ")
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
                if across > LINE_OFFSET * size || gap < -size || turns {
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
            self.lines.push(TextLine {
                words: std::mem::take(&mut self.words),
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
