//! `castle-pdf text <file> [--page <n>]`: prints the text of one page, or of
//! every page separated by form feeds, in the order the content shows it
//! (ADR-0006, tranche 3.3). A line break separates glyphs on different
//! baselines and a space separates glyphs set apart; reading order and
//! layout come with tranche 3.5. An unknown glyph prints as U+FFFD. Control
//! characters from the document are replaced by U+FFFD, and so are
//! bidirectional controls and line or paragraph separators: the output never
//! carries escape sequences to a terminal, nor reorders what it shows.
//!
//! A summary per page goes to the error output: glyphs, unknown ones, ones
//! read through a `/ToUnicode` code of another length, and what could not
//! be read. A page that fails is reported and the next ones are still
//! printed; the exit code is then 1. Exit codes otherwise as `render`.

use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::Path;

use pdf_document::Document;
use pdf_render::{
    PageText, RenderCache, RenderLimits, TextGlyph, UnicodeSource, Unsupported, page_text_cached,
};
use pdf_syntax::{Budget, CancellationToken, ResourceLimits};

/// Parsed arguments; `None` is a usage error.
pub struct Request<'a> {
    input: &'a Path,
    /// Zero-based page index; `None` for every page.
    page: Option<usize>,
}

impl<'a> Request<'a> {
    /// `<file> [--page <n>]`, page numbered from 1.
    pub fn parse(args: &[&'a OsStr]) -> Option<Self> {
        let (input, page) = match args {
            [input] => (input, None),
            [input, flag, page] if *flag == "--page" => {
                let page: usize = page.to_str()?.parse().ok()?;
                (input, Some(page.checked_sub(1)?))
            }
            _ => return None,
        };
        Some(Self {
            input: Path::new(*input),
            page,
        })
    }
}

/// Prints the text; returns 0 or 1. An `Err` means that writing failed.
pub fn run(request: &Request<'_>, out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
    let limits = ResourceLimits::default();
    let Some(source) = crate::open::source(request.input, &limits, err)? else {
        return Ok(1);
    };
    let mut budget = Budget::new(limits, CancellationToken::new());
    let document = match Document::open(source, &mut budget) {
        Ok(document) => document,
        Err(e) => {
            writeln!(err, "error: {e}")?;
            return Ok(1);
        }
    };
    let pages: Vec<usize> = match request.page {
        Some(page) if page < document.page_count() => vec![page],
        Some(page) => {
            writeln!(
                err,
                "error: no page {} (the document has {})",
                page + 1,
                document.page_count()
            )?;
            return Ok(1);
        }
        None => (0..document.page_count()).collect(),
    };
    // Fonts and their /ToUnicode CMaps are read once for all pages.
    let mut cache = RenderCache::default();
    let mut failed = false;
    for (rank, index) in pages.iter().enumerate() {
        let Some(page) = document.page(*index) else {
            continue;
        };
        if rank > 0 {
            out.write_all(b"\x0c")?;
        }
        // Each page gets a budget of its own, like a rendering.
        let mut budget = Budget::new(limits, CancellationToken::new());
        let limits = RenderLimits::default();
        match page_text_cached(document.file(), page, &limits, &mut cache, &mut budget) {
            Ok(text) => {
                out.write_all(layout(&text.glyphs).as_bytes())?;
                summary(*index, &text, err)?;
            }
            Err(e) => {
                writeln!(err, "error: page {}: {e}", index + 1)?;
                failed = true;
            }
        }
    }
    Ok(u8::from(failed))
}

/// The glyphs as lines of text: a line break where the next glyph does not
/// start on the baseline where the previous one ended (by more than half its
/// size), a space where it starts further than a fifth of its size from
/// there. Each line ends with a line break.
pub fn layout(glyphs: &[TextGlyph]) -> String {
    let mut text = String::new();
    let mut previous: Option<&TextGlyph> = None;
    for glyph in glyphs {
        if let Some(previous) = previous {
            let size = previous.size.max(glyph.size).max(f64::MIN_POSITIVE);
            let (dx, dy) = (glyph.x - previous.end_x, glyph.y - previous.end_y);
            if dy.abs() > size / 2.0 {
                text.push('\n');
            } else if dx.abs() > size / 5.0 {
                text.push(' ');
            }
        }
        match &glyph.text {
            Some(glyph_text) => text.extend(glyph_text.chars().map(printable)),
            None => text.push(char::REPLACEMENT_CHARACTER),
        }
        previous = Some(glyph);
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

/// A character of the document as printed: control characters (C0, C1,
/// DEL), bidirectional controls (embeddings, overrides, isolates, marks),
/// line and paragraph separators and the byte order mark become U+FFFD.
pub fn printable(c: char) -> char {
    let bidi = matches!(
        c,
        '\u{200E}' | '\u{200F}' | '\u{061C}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    );
    if c.is_control() || bidi || matches!(c, '\u{2028}' | '\u{2029}' | '\u{FEFF}') {
        char::REPLACEMENT_CHARACTER
    } else {
        c
    }
}

fn summary(index: usize, text: &PageText, err: &mut dyn Write) -> io::Result<()> {
    let count = |source| text.glyphs.iter().filter(|g| g.source == source).count();
    let fonts = if text.skipped.contains(&Unsupported::Text) {
        "; some text not read (font or glyph unsupported)"
    } else {
        ""
    };
    writeln!(
        err,
        "page {}: {} glyphs, {} unknown, {} through a /ToUnicode code of another length, {} unreadable /ToUnicode, {} bytes in fonts not read{fonts}",
        index + 1,
        text.glyphs.len(),
        count(UnicodeSource::Unknown),
        count(UnicodeSource::ToUnicodeOtherLength),
        text.unreadable_to_unicode,
        text.unread_bytes,
    )
}
