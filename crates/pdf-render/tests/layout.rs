//! Words, lines and `/ActualText` (ADR-0006, tranche 3.5a).

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use std::path::PathBuf;

use pdf_document::Document;
use pdf_render::{Error, PageText, RenderLimitKind, RenderLimits, TextLine, page_text, text_lines};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

fn text_with(bytes: Vec<u8>, limits: &RenderLimits) -> Result<PageText, Error> {
    let source = MemorySource::from_vec(bytes, &ResourceLimits::default()).expect("source");
    let document = Document::open(source, &mut budget()).expect("document");
    let page = document.page(0).expect("page");
    page_text(document.file(), page, limits, &mut budget())
}

fn text(bytes: Vec<u8>) -> PageText {
    text_with(bytes, &RenderLimits::default()).expect("text")
}

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/public")
        .join(name);
    std::fs::read(path).expect("fixture")
}

fn lines(lines: &[TextLine]) -> Vec<String> {
    lines.iter().map(TextLine::text).collect()
}

/// One page showing `content` in Helvetica (`/F1`), with `resources` added
/// and `more` objects numbered from 6.
fn page(content: &str, resources: &str, more: &[&str]) -> Vec<u8> {
    let mut bodies = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << /Font << /F1 5 0 R >> {resources} >> /Contents 4 0 R >>"
        ),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    bodies.extend(more.iter().map(|body| (*body).to_owned()));
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            bodies.len() + 1
        )
        .as_bytes(),
    );
    out
}

#[test]
fn the_fixture_gives_words_lines_and_actual_text() {
    let page = text(fixture("layout.pdf"));
    assert_eq!(
        lines(&text_lines(&page)),
        [
            "fine print",
            "Reading order",
            "Outer Logo",
            "Gap WordsKern",
            "One by one",
            "Rotated text"
        ]
    );
    // Spans: inline, from a resource, the outer of two nested ones, and one
    // without glyphs.
    let spans: Vec<(&str, usize)> = page
        .actual_texts
        .iter()
        .map(|span| (span.text.as_str(), span.glyphs.len()))
        .collect();
    assert_eq!(
        spans,
        [("fi", 1), ("Reading", 7), ("Outer", 4), ("Logo", 0)]
    );
}

#[test]
fn words_carry_their_glyphs_and_bounds() {
    let page = text(page("BT /F1 10 Tf 20 50 Td (ab cd) Tj ET", "", &[]));
    let lines = text_lines(&page);
    let words = &lines[0].words;
    assert_eq!(words.len(), 2);
    assert_eq!(words[0].glyphs, 0..2);
    assert_eq!(words[1].glyphs, 3..5);
    // Baseline at 50, ascent 8 and descent 2 for a size of 10.
    let [left, bottom, right, top] = words[0].bounds;
    assert!((left - 20.0).abs() < 1e-9 && (bottom - 48.0).abs() < 1e-9);
    assert!((top - 58.0).abs() < 1e-9 && right > left);
    // The baseline runs from the first glyph's origin to the last one's end.
    let [x0, y0, x1, y1] = words[0].baseline;
    assert_eq!((x0, y0, y1, words[0].size), (20.0, 50.0, 50.0, 10.0));
    assert!((x1 - right).abs() < 1e-9);
}

#[test]
fn baselines_directions_and_gaps_split_lines() {
    // A superscript within half the size stays on the line; a line below
    // does not; text going back by more than its size starts a line.
    let content = "BT /F1 10 Tf 20 100 Td (x) Tj 0 4 Td (2) Tj 10 -4 Td (end) Tj ET \
        BT /F1 10 Tf 20 80 Td (below) Tj ET \
        BT /F1 10 Tf 200 60 Td (right) Tj -150 0 Td (left) Tj ET";
    assert_eq!(
        lines(&text_lines(&text(page(content, "", &[])))),
        ["x2 end", "below", "right", "left"]
    );
}

#[test]
fn an_actual_text_spans_a_form() {
    // A span opened around a form covers the form's glyphs.
    let form = "BT /F1 10 Tf 0 0 Td (inside) Tj ET";
    let form_object = format!(
        "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /F1 5 0 R >> >> /Length {} >>\nstream\n{form}\nendstream",
        form.len()
    );
    let content = "/Span << /ActualText (Whole) >> BDC q 1 0 0 1 20 50 cm /X1 Do Q EMC";
    let page = text(page(content, "/XObject << /X1 6 0 R >>", &[&form_object]));
    assert_eq!(page.actual_texts.len(), 1);
    assert_eq!(page.actual_texts[0].glyphs, 0..6);
    assert_eq!(lines(&text_lines(&page)), ["Whole"]);
}

#[test]
fn a_span_left_open_in_a_form_ends_with_it() {
    let form = "/Span << /ActualText (Cut) >> BDC BT /F1 10 Tf 0 0 Td (abc) Tj ET";
    let form_object = format!(
        "<< /Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /F1 5 0 R >> >> /Length {} >>\nstream\n{form}\nendstream",
        form.len()
    );
    let content = "q 1 0 0 1 20 50 cm /X1 Do Q BT /F1 10 Tf 20 30 Td (after) Tj ET";
    let page = text(page(content, "/XObject << /X1 6 0 R >>", &[&form_object]));
    assert_eq!(page.actual_texts[0].glyphs, 0..3);
    assert_eq!(lines(&text_lines(&page)), ["Cut", "after"]);
}

#[test]
fn actual_text_strings_are_decoded() {
    let content = "BT /F1 10 Tf 20 50 Td /Span << /ActualText <FEFF00E9> >> BDC (e) Tj EMC \
        /Span << /ActualText (\\223) >> BDC (f) Tj EMC ET";
    let page = text(page(content, "", &[]));
    // UTF-16BE, then PDFDocEncoding 0x93: the fi ligature.
    assert_eq!(lines(&text_lines(&page)), ["é\u{FB01}"]);
}

#[test]
fn actual_text_spans_count_against_the_glyph_limit() {
    let content = "/Span << /ActualText (a) >> BDC EMC /Span << /ActualText (b) >> BDC EMC";
    let mut limits = RenderLimits::default();
    limits.max_text_glyphs = 2;
    assert_eq!(
        text_with(page(content, "", &[]), &limits)
            .expect("text")
            .actual_texts
            .len(),
        2
    );
    limits.max_text_glyphs = 1;
    assert!(matches!(
        text_with(page(content, "", &[]), &limits),
        Err(Error::LimitExceeded {
            kind: RenderLimitKind::TextGlyphs,
            ..
        })
    ));
}

#[test]
fn reading_order_reads_a_title_then_each_column() {
    let page = text(fixture("columns.pdf"));
    assert_eq!(
        lines(&pdf_render::reading_lines(&page)),
        [
            "A title over both columns",
            "Left one",
            "Left two",
            "Left three",
            "Right one",
            "Right two",
            "Right three",
            "First half, second half",
            "Footer under both columns"
        ]
    );
    // Content order is kept by `text_lines`: the columns alternate and the
    // pieces of a line stay apart.
    let content = lines(&text_lines(&page));
    assert_eq!(&content[1..3], ["Left one", "Right one"]);
    assert!(content.contains(&"second half".to_owned()));
    assert!(content.contains(&"First half,".to_owned()));
}

#[test]
fn reading_order_keeps_simple_pages_as_they_are() {
    for name in ["text.pdf", "layout.pdf", "minimal.pdf"] {
        let page = text(fixture(name));
        assert_eq!(
            lines(&pdf_render::reading_lines(&page)),
            lines(&text_lines(&page)),
            "{name}"
        );
    }
}

#[test]
fn a_wide_gap_on_a_line_makes_columns_a_near_one_does_not() {
    // "a" and "b" 1 size apart join; "c" 3 sizes further is another column.
    let content = "BT /F1 10 Tf 20 100 Td (a) Tj ET BT /F1 10 Tf 35 100 Td (b) Tj ET \
        BT /F1 10 Tf 80 100 Td (c) Tj ET BT /F1 10 Tf 20 85 Td (d) Tj ET BT /F1 10 Tf 80 85 Td (e) Tj ET";
    assert_eq!(
        lines(&pdf_render::reading_lines(&text(page(content, "", &[])))),
        ["a b", "d", "c", "e"]
    );
}
