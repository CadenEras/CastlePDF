//! Text with embedded TrueType fonts (ADR-0005, step 5.2a): simple fonts and
//! Type 0 `Identity-H` fonts drawn from their outlines; any other font is
//! skipped and reported as `Text`.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use std::path::PathBuf;

use pdf_document::Document;
use pdf_render::{
    Error, PixelRect, RenderLimitKind, RenderOptions, Rendering, Unsupported, render_area,
    render_page,
};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

fn repo(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

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

/// Dark pixels (any channel below 128) in the rectangle of user space
/// [x0, x1) x [y0, y1) of a page `height` points high, at scale 1.
fn dark(rendering: &Rendering, height: u32, (x0, y0, x1, y1): (u32, u32, u32, u32)) -> usize {
    let mut count = 0;
    for y in (height - y1)..(height - y0) {
        for x in x0..x1 {
            let pixel = rendering.bitmap.pixel(x, y).expect("inside");
            count += usize::from(pixel[..3].iter().any(|c| *c < 128));
        }
    }
    count
}

/// A one-page file (200 x 100) whose /F1 is the Arimo subset embedded as a
/// simple TrueType font with `font_extra` added to its dictionary, and
/// whose content is `content`. Object 6 is the font program, `program`.
fn with_font(content: &str, font_extra: &str, program: &[u8]) -> Vec<u8> {
    with_font_flags(content, font_extra, program, 32)
}

/// `with_font` with the descriptor's `/Flags`.
fn with_font_flags(content: &str, font_extra: &str, program: &[u8], flags: u32) -> Vec<u8> {
    let bodies: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()).into_bytes(),
        format!(
            "<< /Type /Font /Subtype /TrueType /BaseFont /Arimo /FontDescriptor 7 0 R{font_extra} >>"
        )
        .into_bytes(),
        [
            format!("<< /Length {} >>\nstream\n", program.len()).as_bytes(),
            program,
            b"\nendstream",
        ]
        .concat(),
        format!("<< /Type /FontDescriptor /FontName /Arimo /Flags {flags} /FontFile2 6 0 R >>")
            .into_bytes(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
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

fn arimo() -> Vec<u8> {
    std::fs::read(repo("tests/fonts/arimo-subset.ttf")).expect("font")
}

const WINANSI: &str = " /Encoding /WinAnsiEncoding";

#[test]
fn the_truetype_fixture_draws_every_line() {
    let rendering =
        render(std::fs::read(repo("tests/corpus/public/truetype.pdf")).expect("fixture"));
    assert!(rendering.skipped.is_empty(), "{:?}", rendering.skipped);
    // Lines at baselines 120, 85, 50 and 15 (300 x 150 page): ink above
    // each baseline, none in the right margin.
    for baseline in [120, 85, 50, 15] {
        assert!(
            dark(&rendering, 150, (10, baseline, 150, baseline + 14)) > 50,
            "line at {baseline}"
        );
    }
    assert_eq!(dark(&rendering, 150, (260, 0, 300, 150)), 0);
}

#[test]
fn standard_fonts_are_still_reported() {
    let rendering =
        render(std::fs::read(repo("tests/corpus/public/minimal.pdf")).expect("fixture"));
    assert_eq!(
        rendering.skipped.iter().copied().collect::<Vec<_>>(),
        [Unsupported::Text]
    );
}

#[test]
fn text_is_drawn_where_the_text_matrix_puts_it() {
    // "AAAA" at 40 points from (20, 30), then the same moved by Td.
    let left = render(with_font(
        "BT /F1 40 Tf 20 30 Td (AAAA) Tj ET",
        WINANSI,
        &arimo(),
    ));
    assert!(left.skipped.is_empty());
    assert!(dark(&left, 100, (20, 30, 60, 60)) > 100);
    // Without /Widths, the advances of the program: 4 x 26.7 points.
    assert!(dark(&left, 100, (110, 30, 128, 60)) > 0);
    assert_eq!(dark(&left, 100, (130, 0, 200, 100)), 0);
    let moved = render(with_font(
        "BT /F1 40 Tf 20 30 Td 100 0 Td (A) Tj ET",
        WINANSI,
        &arimo(),
    ));
    assert_eq!(dark(&moved, 100, (0, 0, 115, 100)), 0);
    assert!(dark(&moved, 100, (120, 30, 150, 60)) > 50);
}

#[test]
fn invisible_and_clipping_modes_draw_nothing() {
    for (mode, skipped) in [("3", vec![]), ("7", vec![Unsupported::TextClip])] {
        let rendering = render(with_font(
            &format!("BT /F1 40 Tf {mode} Tr 20 30 Td (AAAA) Tj ET"),
            WINANSI,
            &arimo(),
        ));
        assert_eq!(
            rendering.skipped.iter().copied().collect::<Vec<_>>(),
            skipped
        );
        assert_eq!(dark(&rendering, 100, (0, 0, 200, 100)), 0, "mode {mode}");
    }
}

#[test]
fn unreadable_fonts_are_skipped_not_fatal() {
    // A damaged program, an unknown resource name, and a Type 1 font.
    for (content, program) in [
        ("BT /F1 40 Tf 20 30 Td (A) Tj ET", b"not a font".to_vec()),
        ("BT /F9 40 Tf 20 30 Td (A) Tj ET", arimo()),
    ] {
        let rendering = render(with_font(content, WINANSI, &program));
        assert_eq!(
            rendering.skipped.iter().copied().collect::<Vec<_>>(),
            [Unsupported::Text]
        );
        assert_eq!(dark(&rendering, 100, (0, 0, 200, 100)), 0);
    }
}

#[test]
fn glyph_outlines_are_limited() {
    let bytes = with_font("BT /F1 40 Tf 20 30 Td (A) Tj ET", WINANSI, &arimo());
    let renders = |limit: u64| {
        let mut options = RenderOptions::default();
        options.limits.max_glyph_segments = limit;
        render_with(bytes.clone(), &options)
    };
    let exact = (1..1000)
        .find(|limit| renders(*limit).is_ok())
        .expect("renders");
    assert!(exact > 4, "the A has several segments: {exact}");
    assert_eq!(
        renders(exact - 1).err(),
        Some(Error::LimitExceeded {
            kind: RenderLimitKind::GlyphSegments,
            limit: exact - 1
        })
    );
}

#[test]
fn text_tiles_assemble_into_the_page() {
    let document =
        document(std::fs::read(repo("tests/corpus/public/truetype.pdf")).expect("fixture"));
    let page = document.page(0).expect("page");
    let mut options = RenderOptions::default();
    options.scale = 2.0;
    let whole = render_page(document.file(), page, &options, &mut budget()).expect("page");
    let area = PixelRect {
        x: 100,
        y: 100,
        width: 256,
        height: 150,
    };
    let part = render_area(document.file(), page, &options, area, &mut budget()).expect("tile");
    for y in 0..area.height {
        for x in 0..area.width {
            let got = part.bitmap.pixel(x, y).expect("tile pixel");
            let want = whole
                .bitmap
                .pixel(area.x + x, area.y + y)
                .expect("page pixel");
            assert!(
                got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 1),
                "({x}, {y}) {got:?} != {want:?}"
            );
        }
    }
}

/// Found by the `font` fuzz target: a simple glyph with data but no point
/// made `read-fonts` 0.41 index an empty array (fixed in 0.42; ADR-0005
/// amendment). The program is the fuzz input, the page that of the target.
#[test]
fn a_glyph_without_points_does_not_panic() {
    let program = std::fs::read(repo("crates/pdf-render/tests/data/glyf-zero-points.ttf"))
        .expect("regression program");
    let codes: String = (0u16..256).map(|c| format!("{c:02x}")).collect();
    let content = format!(
        "BT /F1 9 Tf 1 Tr 2 30 Td 0.3 Tc 80 Tz <{codes}> Tj 0 -10 Td [<{codes}> -250 <41>] TJ ET"
    );
    let bytes = with_font_flags(
        &content,
        " /Encoding << /Differences [65 /A /uni00E9 /g3] >>",
        &program,
        4,
    );
    let mut options = RenderOptions::default();
    options.scale = 2.0;
    assert!(render_with(bytes, &options).is_ok());
}
