//! Type 3 fonts (ISO 32000-2, 9.6.4; ADR-0005, step 5.5): glyph procedures
//! run with the font matrix, uncoloured (`d1`) and coloured (`d0`) glyphs,
//! advances, resources, and what a damaged glyph affects.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use pdf_document::Document;
use pdf_render::{
    Error, PixelRect, RenderLimitKind, RenderOptions, Rendering, Unsupported, page_size,
    render_area, render_page,
};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

fn document(bytes: Vec<u8>) -> Document<MemorySource> {
    let source = MemorySource::from_vec(bytes, &ResourceLimits::default()).expect("source");
    Document::open(source, &mut budget()).expect("document")
}

fn render_with(bytes: Vec<u8>, options: &RenderOptions) -> Result<Rendering, Error> {
    let document = document(bytes);
    let page = document.page(0).expect("page");
    render_page(document.file(), page, options, &mut budget())
}

fn render(bytes: Vec<u8>) -> Rendering {
    render_with(bytes, &RenderOptions::default()).expect("rendering")
}

/// RGB at user point (x, y) of a page 200 high, at scale 1.
fn at(rendering: &Rendering, x: u32, y: u32) -> [u8; 3] {
    let [r, g, b, _] = rendering.bitmap.pixel(x, 200 - y).expect("inside");
    [r, g, b]
}

fn skipped(rendering: &Rendering) -> Vec<Unsupported> {
    rendering.skipped.iter().copied().collect()
}

fn stream(dictionary: &str, data: &str) -> String {
    format!(
        "<< {dictionary} /Length {} >>\nstream\n{data}\nendstream",
        data.len()
    )
}

/// An uncoloured square, 1000 units (a 1000-wide glyph).
const SQUARE: &str = "1000 0 0 0 1000 1000 d1 0 0 1000 1000 re f";
/// A coloured red rectangle, 500 x 1000 units (a 500-wide glyph).
const RED_BAR: &str = "500 0 d0 1 0 0 rg 0 0 500 1000 re f";

/// A 200 x 200 page with `content`, whose /T3 is a Type 3 font with
/// `font_extra` entries: code 65 (`A`) is glyph 6, code 66 (`B`) glyph 7,
/// widths 1000 and 500, font matrix 0.001. `more` objects from 8.
fn page(content: &str, font_extra: &str, glyphs: [&str; 2], more: &[String]) -> Vec<u8> {
    let mut bodies = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /T3 5 0 R >> >> /Contents 4 0 R >>".to_owned(),
        stream("", content),
        format!(
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] \
             /CharProcs << /square 6 0 R /bar 7 0 R >> /Encoding << /Type /Encoding /Differences [65 /square /bar] >> \
             /FirstChar 65 /LastChar 66 /Widths [1000 500] {font_extra} >>"
        ),
        stream("", glyphs[0]),
        stream("", glyphs[1]),
    ];
    bodies.extend(more.iter().cloned());
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

fn simple(content: &str) -> Vec<u8> {
    page(content, "", [SQUARE, RED_BAR], &[])
}

const BLUE: [u8; 3] = [0, 0, 255];
const RED: [u8; 3] = [255, 0, 0];
const WHITE: [u8; 3] = [255, 255, 255];

#[test]
fn glyphs_are_drawn_and_advance_by_their_widths() {
    // "ABA" at 40 points from (10, 30): a blue square (uncoloured, in the
    // fill colour) over 10..50, a red bar over 50..70, a square over 70..110.
    let rendering = render(simple("0 0 1 rg BT /T3 40 Tf 10 30 Td (ABA) Tj ET"));
    assert!(skipped(&rendering).is_empty(), "{:?}", skipped(&rendering));
    assert_eq!(at(&rendering, 30, 50), BLUE);
    assert_eq!(at(&rendering, 60, 50), RED);
    assert_eq!(at(&rendering, 90, 50), BLUE);
    assert_eq!(at(&rendering, 115, 50), WHITE);
    assert_eq!(at(&rendering, 30, 75), WHITE);
    // The glyph's colour does not leak: text after it is blue again.
    let rendering = render(simple("0 0 1 rg BT /T3 40 Tf 10 30 Td (BA) Tj ET"));
    assert_eq!(at(&rendering, 40, 50), BLUE);
}

#[test]
fn text_state_applies_to_type3_glyphs() {
    // Character spacing (Tc 10) and a TJ adjustment of -500 (20 points
    // forward at 40); then horizontal scaling (Tz 50) and rise (Ts 5).
    let rendering = render(simple(
        "0 0 1 rg BT /T3 40 Tf 10 Tc 10 30 Td [(A) -500 (A)] TJ ET",
    ));
    // First square 10..50, then 10 (Tc) and 20 (TJ): second at 80..120.
    assert_eq!(at(&rendering, 60, 50), WHITE);
    assert_eq!(at(&rendering, 100, 50), BLUE);
    let rendering = render(simple(
        "0 0 1 rg BT /T3 40 Tf 50 Tz 5 Ts 10 30 Td (AA) Tj ET",
    ));
    // Half as wide: squares over 10..30 and 30..50, raised by 5.
    assert_eq!(at(&rendering, 45, 50), BLUE);
    assert_eq!(at(&rendering, 55, 50), WHITE);
    assert_eq!(at(&rendering, 20, 32), WHITE);
    assert_eq!(at(&rendering, 20, 37), BLUE);
    // Invisible text advances but draws nothing.
    let rendering = render(simple("BT /T3 40 Tf 3 Tr 10 30 Td (AB) Tj ET"));
    assert_eq!(at(&rendering, 30, 50), WHITE);
    assert!(skipped(&rendering).is_empty());
}

#[test]
fn an_uncoloured_glyph_ignores_its_colour_operators() {
    let rendering = render(page(
        "0 0 1 rg BT /T3 40 Tf 10 30 Td (A) Tj ET",
        "",
        [
            "1000 0 0 0 1000 1000 d1 1 0 0 rg 0 0 1000 1000 re f",
            RED_BAR,
        ],
        &[],
    ));
    assert!(skipped(&rendering).is_empty(), "{:?}", skipped(&rendering));
    assert_eq!(at(&rendering, 30, 50), BLUE);
}

#[test]
fn the_font_matrix_maps_glyph_space() {
    // A matrix of 0.002: the 1000-unit square is 2 text units wide (80
    // points at 40), and so is its advance.
    let bytes = simple("0 0 1 rg BT /T3 40 Tf 10 30 Td (AA) Tj ET")
        .into_iter()
        .collect::<Vec<u8>>();
    let text = String::from_utf8_lossy(&bytes).replace(
        "/FontMatrix [0.001 0 0 0.001 0 0]",
        "/FontMatrix [0.002 0 0 0.002 0 0]",
    );
    let rendering = render(text.into_bytes());
    assert_eq!(at(&rendering, 85, 105), BLUE);
    assert_eq!(at(&rendering, 165, 105), BLUE);
    assert_eq!(at(&rendering, 85, 115), WHITE);
}

#[test]
fn glyphs_use_the_font_resources() {
    // The glyph paints image /I of the font's resources (a red pixel); the
    // page has no /XObject.
    let image = stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
        "\u{0}",
    );
    let rendering = render(page(
        "BT /T3 40 Tf 10 30 Td (A) Tj ET",
        "/Resources << /XObject << /I 8 0 R >> >>",
        ["1000 0 d0 q 1000 0 0 1000 0 0 cm /I Do Q", RED_BAR],
        &[image],
    ));
    assert!(skipped(&rendering).is_empty(), "{:?}", skipped(&rendering));
    assert_eq!(at(&rendering, 30, 50), [0, 0, 0]);
}

#[test]
fn a_damaged_or_missing_glyph_affects_only_itself() {
    for glyph in [
        // Invalid content after a painted part.
        "1000 0 d0 0 0 500 1000 re f /X re",
        // The glyph shows text in its own font: inside itself.
        "1000 0 d0 0 0 500 1000 re f BT /T3 1 Tf (A) Tj ET",
    ] {
        let rendering = render(page(
            "0 0 1 rg BT /T3 40 Tf 10 30 Td (AB) Tj ET 0 0 1 rg 150 150 20 20 re f",
            "/Resources << /Font << /T3 5 0 R >> >>",
            [glyph, RED_BAR],
            &[],
        ));
        assert_eq!(skipped(&rendering), [Unsupported::Text], "{glyph}");
        // Drawn up to its error; the next glyph and the page go on.
        assert_eq!(at(&rendering, 15, 50), BLUE, "{glyph}");
        assert_eq!(at(&rendering, 60, 50), RED, "{glyph}");
        assert_eq!(at(&rendering, 160, 160), BLUE, "{glyph}");
    }
    // A code whose name has no procedure: reported, advances.
    let rendering = render(simple("0 0 1 rg BT /T3 40 Tf 10 30 Td (CA) Tj ET"));
    assert!(skipped(&rendering).is_empty());
    let text = String::from_utf8_lossy(&simple("0 0 1 rg BT /T3 40 Tf 10 30 Td (AB) Tj ET"))
        .replace("/bar 7 0 R", "");
    let rendering = render(text.into_bytes());
    assert_eq!(skipped(&rendering), [Unsupported::Text]);
    assert_eq!(at(&rendering, 30, 50), BLUE);
    // d0 outside a glyph is misplaced.
    assert!(render_with(simple("1000 0 d0"), &RenderOptions::default()).is_err());
}

#[test]
fn a_font_that_cannot_be_read_is_skipped() {
    for (from, to) in [
        (
            "/FontMatrix [0.001 0 0 0.001 0 0]",
            "/FontMatrix [0 0 0 0 0 0]",
        ),
        ("/FontMatrix [0.001 0 0 0.001 0 0]", ""),
        ("/CharProcs << /square 6 0 R /bar 7 0 R >>", ""),
    ] {
        let text =
            String::from_utf8_lossy(&simple("BT /T3 40 Tf 10 30 Td (A) Tj ET")).replace(from, to);
        let rendering = render(text.into_bytes());
        assert_eq!(skipped(&rendering), [Unsupported::Text], "{to}");
        assert_eq!(at(&rendering, 30, 50), WHITE, "{to}");
    }
}

#[test]
fn glyph_operations_count_against_the_limits() {
    // 100 glyphs of 3 operations each (d1, re, f): the page's operation
    // limit counts them.
    let content = format!("BT /T3 4 Tf 0 30 Td ({}) Tj ET", "A".repeat(100));
    let mut options = RenderOptions::default();
    options.limits.max_operations = 200;
    assert!(matches!(
        render_with(simple(&content), &options),
        Err(Error::LimitExceeded {
            kind: RenderLimitKind::Operations,
            ..
        })
    ));
    options.limits.max_operations = 1000;
    assert!(render_with(simple(&content), &options).is_ok());
}

#[test]
fn type3_tiles_match_the_page() {
    let document = document(simple(
        "0 0 1 rg BT /T3 37 Tf 0.7 0.7 -0.7 0.7 13 21 Tm (ABAB) Tj ET",
    ));
    let page = document.page(0).expect("page");
    let mut options = RenderOptions::default();
    options.scale = 2.7;
    let whole = render_page(document.file(), page, &options, &mut budget()).expect("page");
    let (width, height) = page_size(page, &options).expect("size");
    let area = PixelRect {
        x: 97,
        y: 131,
        width: (width - 97).min(300),
        height: (height - 131).min(300),
    };
    let part = render_area(document.file(), page, &options, area, &mut budget()).expect("area");
    for y in 0..area.height {
        for x in 0..area.width {
            assert_eq!(
                part.bitmap.pixel(x, y),
                whole.bitmap.pixel(area.x + x, area.y + y),
                "({x}, {y})"
            );
        }
    }
}

#[test]
fn the_type3_fixture_draws_every_glyph() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/public/type3.pdf");
    let rendering = render(std::fs::read(path).expect("fixture"));
    assert!(skipped(&rendering).is_empty(), "{:?}", skipped(&rendering));
    // "ABBA" at 30 points from (10, 150): A is blue around its hole, the
    // hole (350..650, 150..450 of 1000) white; B's bar red.
    assert_eq!(at(&rendering, 25, 153), BLUE);
    assert_eq!(at(&rendering, 25, 159), WHITE);
    assert_eq!(at(&rendering, 43, 165), RED);
}

#[test]
fn every_glyph_run_counts_as_an_operation() {
    // An empty procedure still costs one operation per code shown.
    let content = format!("BT /T3 4 Tf 0 30 Td ({}) Tj ET", "A".repeat(1000));
    let bytes = page(&content, "", ["", RED_BAR], &[]);
    let mut options = RenderOptions::default();
    options.limits.max_operations = 100;
    assert!(matches!(
        render_with(bytes.clone(), &options),
        Err(Error::LimitExceeded {
            kind: RenderLimitKind::Operations,
            ..
        })
    ));
    options.limits.max_operations = 2000;
    assert!(render_with(bytes, &options).is_ok());
}

#[test]
fn an_unreadable_glyph_is_read_once() {
    // /bar is not a stream: reported, and not resolved again for each of
    // its 1 or 500 occurrences.
    let visited = |count: usize| {
        let content = format!("BT /T3 1 Tf 0 30 Td ({}) Tj ET", "B".repeat(count));
        let text = String::from_utf8_lossy(&simple(&content)).replace("/bar 7 0 R", "/bar 5 0 R");
        let document = document(text.into_bytes());
        let page = document.page(0).expect("page");
        let mut budget = budget();
        let rendering = render_page(
            document.file(),
            page,
            &RenderOptions::default(),
            &mut budget,
        )
        .expect("rendering");
        assert_eq!(skipped(&rendering), [Unsupported::Text]);
        budget.objects_visited()
    };
    let (once, often) = (visited(1), visited(500));
    assert!(often <= once + 5, "{once} then {often}");
}

#[test]
fn glyph_operators_and_colours_follow_the_rules() {
    // d1 must be the first operator of the glyph: elsewhere the glyph is
    // drawn up to it and reported.
    let rendering = render(page(
        "0 0 1 rg BT /T3 40 Tf 10 30 Td (A) Tj ET",
        "",
        ["0 0 500 1000 re f 1000 0 0 0 1000 1000 d1", RED_BAR],
        &[],
    ));
    assert_eq!(skipped(&rendering), [Unsupported::Text]);
    assert_eq!(at(&rendering, 15, 50), BLUE);
    // A form called by an uncoloured glyph: its own d1 is misplaced (the
    // form is reported), its colour operators are ignored too.
    let form = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 1000 1000]",
        "1 0 0 rg 0 0 1000 1000 re f",
    );
    let rendering = render(page(
        "0 0 1 rg BT /T3 40 Tf 10 30 Td (A) Tj ET",
        "/Resources << /XObject << /F 8 0 R >> >>",
        ["1000 0 0 0 1000 1000 d1 /F Do", RED_BAR],
        &[form],
    ));
    assert!(skipped(&rendering).is_empty(), "{:?}", skipped(&rendering));
    assert_eq!(at(&rendering, 30, 50), BLUE);
    // An uncoloured glyph paints image masks in the text colour, not images.
    let mask = stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ImageMask true",
        "\u{0}",
    );
    let gray = stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
        "\u{0}",
    );
    let rendering = render(page(
        "0 0 1 rg BT /T3 40 Tf 10 30 Td (AB) Tj ET",
        "/Resources << /XObject << /M 8 0 R /G 9 0 R >> >>",
        [
            "1000 0 0 0 1000 1000 d1 q 1000 0 0 1000 0 0 cm /M Do Q",
            "500 0 0 0 500 1000 d1 q 500 0 0 1000 0 0 cm /G Do Q",
        ],
        &[mask, gray],
    ));
    assert_eq!(skipped(&rendering), [Unsupported::Image]);
    assert_eq!(at(&rendering, 30, 50), BLUE);
    assert_eq!(at(&rendering, 60, 50), WHITE);
}

#[test]
fn a_glyph_in_a_form_uses_the_form_resources() {
    // The font has no resources: its glyph's /I is the form's image, not
    // the page's (which has none).
    let form = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /Font << /T3 5 0 R >> /XObject << /I 9 0 R >> >>",
        "BT /T3 40 Tf 10 30 Td (A) Tj ET",
    );
    let image = stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
        "\u{0}",
    );
    let text = String::from_utf8_lossy(&page(
        "/F Do",
        "",
        ["1000 0 d0 q 1000 0 0 1000 0 0 cm /I Do Q", RED_BAR],
        &[form, image],
    ))
    .replacen("/Font << /T3 5 0 R >>", "/XObject << /F 8 0 R >>", 1);
    let rendering = render(text.into_bytes());
    assert!(skipped(&rendering).is_empty(), "{:?}", skipped(&rendering));
    assert_eq!(at(&rendering, 30, 50), [0, 0, 0]);
}

#[test]
fn word_spacing_applies_to_code_32() {
    // Code 32 shows the square too; Tw 20 moves what follows it.
    let text = String::from_utf8_lossy(&simple("0 0 1 rg BT /T3 20 Tf 20 Tw 10 30 Td ( A) Tj ET"))
        .replace("[65 /square /bar]", "[32 /square 65 /square /bar]")
        .replace("/FirstChar 65 /LastChar 66 /Widths [1000 500]", "/FirstChar 32 /LastChar 66 /Widths [1000 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1000 500]");
    let rendering = render(text.into_bytes());
    // The space glyph over 10..30, then 20 of word spacing: A over 50..70.
    assert_eq!(at(&rendering, 20, 40), BLUE);
    assert_eq!(at(&rendering, 40, 40), WHITE);
    assert_eq!(at(&rendering, 60, 40), BLUE);
}

#[test]
fn a_font_without_encoding_or_with_unreadable_resources_is_skipped() {
    for (from, to) in [
        (
            "/Encoding << /Type /Encoding /Differences [65 /square /bar] >>",
            "",
        ),
        ("/Widths [1000 500]", "/Widths [1000 500] /Resources 12"),
    ] {
        let text = String::from_utf8_lossy(&simple("0 0 1 rg BT /T3 40 Tf 10 30 Td (A) Tj ET"))
            .replace(from, to);
        let rendering = render(text.into_bytes());
        assert_eq!(skipped(&rendering), [Unsupported::Text], "{to}");
        assert_eq!(at(&rendering, 30, 50), WHITE, "{to}");
    }
}
