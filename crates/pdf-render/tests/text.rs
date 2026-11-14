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
    with_program(
        content,
        "TrueType",
        font_extra,
        "FontFile2",
        "",
        program,
        flags,
    )
}

/// `with_font` with an embedded bare CFF program (`/Type1`, `/FontFile3
/// /Type1C`).
fn with_cff(content: &str, font_extra: &str, program: &[u8]) -> Vec<u8> {
    with_program(
        content,
        "Type1",
        font_extra,
        "FontFile3",
        " /Subtype /Type1C",
        program,
        32,
    )
}

/// A one-page file (200 x 100) whose /F1 has `/Subtype` `subtype` and
/// `font_extra`, and an embedded program in `file_key` with `stream_extra`
/// in its stream dictionary.
fn with_program(
    content: &str,
    subtype: &str,
    font_extra: &str,
    file_key: &str,
    stream_extra: &str,
    program: &[u8],
    flags: u32,
) -> Vec<u8> {
    let bodies: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()).into_bytes(),
        format!(
            "<< /Type /Font /Subtype /{subtype} /BaseFont /Arimo /FontDescriptor 7 0 R{font_extra} >>"
        )
        .into_bytes(),
        [
            format!("<< /Length {}{stream_extra} >>\nstream\n", program.len()).as_bytes(),
            program,
            b"\nendstream",
        ]
        .concat(),
        format!("<< /Type /FontDescriptor /FontName /Arimo /Flags {flags} /{file_key} 6 0 R >>")
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

#[cfg(feature = "standard-fonts")]
#[test]
fn standard_fonts_are_replaced_and_reported() {
    // minimal.pdf: Helvetica 24 at (72, 770), not embedded.
    let rendering =
        render(std::fs::read(repo("tests/corpus/public/minimal.pdf")).expect("fixture"));
    assert_eq!(
        rendering.skipped.iter().copied().collect::<Vec<_>>(),
        [Unsupported::FontSubstituted]
    );
    assert!(dark(&rendering, 842, (72, 770, 300, 790)) > 100);
}

/// A one-page file whose /F1 is `font`, a font without program.
fn with_named_font(content: &str, font: &str) -> Vec<u8> {
    let bodies: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        font.to_owned(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

/// Ink of "MMMM" at 40 points from (10, 30).
fn ink_of(font: &str) -> (Vec<Unsupported>, usize) {
    let rendering = render(with_named_font("BT /F1 40 Tf 10 30 Td (MMMM) Tj ET", font));
    (
        rendering.skipped.iter().copied().collect(),
        dark(&rendering, 100, (0, 0, 200, 100)),
    )
}

#[cfg(feature = "standard-fonts")]
#[test]
fn each_standard_family_and_style_has_its_widths() {
    // The replacements keep the widths of the fonts they replace: four M
    // at 40 points span 4 x 0.833 x 40 = 133 points in Helvetica, 4 x 0.889
    // x 40 = 142 in Times, 4 x 0.6 x 40 = 96 in Courier.
    for (name, right) in [
        ("Helvetica", 143.0),
        ("Helvetica-BoldOblique", 143.0),
        ("Times-Roman", 152.0),
        ("Times-BoldItalic", 152.0),
        ("Courier", 106.0),
        ("Courier-Oblique", 106.0),
    ] {
        let rendering = render(with_named_font(
            "BT /F1 40 Tf 10 30 Td (MMMM) Tj ET",
            &format!("<< /Type /Font /Subtype /Type1 /BaseFont /{name} >>"),
        ));
        assert_eq!(
            rendering.skipped.iter().copied().collect::<Vec<_>>(),
            [Unsupported::FontSubstituted],
            "{name}"
        );
        // Ink stops near the end of the fourth M, not beyond.
        let right = right as u32;
        assert!(
            dark(&rendering, 100, (right - 12, 30, right - 2, 60)) > 0,
            "{name}"
        );
        assert_eq!(dark(&rendering, 100, (right + 3, 0, 200, 100)), 0, "{name}");
    }
}

#[cfg(feature = "standard-fonts")]
#[test]
fn symbol_and_zapf_dingbats_are_replaced_with_their_widths() {
    // Symbol "abgd" is alpha beta gamma delta: 631 + 549 + 411 + 494
    // thousandths (Adobe's metrics), 83.4 points at 40 from x = 10.
    // ZapfDingbats "lmn" is a71 a72 a73: 791 + 873 + 761, 97 points.
    for (font, text, right) in [
        ("/BaseFont /Symbol", "abgd", 93),
        ("/BaseFont /ABCDEF+SymbolMT", "abgd", 93),
        ("/BaseFont /ZapfDingbats", "lmn", 107),
        ("/BaseFont /Dingbats", "lmn", 107),
    ] {
        let rendering = render(with_named_font(
            &format!("BT /F1 40 Tf 10 30 Td ({text}) Tj ET"),
            &format!("<< /Type /Font /Subtype /Type1 {font} >>"),
        ));
        assert_eq!(
            rendering.skipped.iter().copied().collect::<Vec<_>>(),
            [Unsupported::FontSubstituted],
            "{font}"
        );
        assert!(
            dark(&rendering, 100, (right - 12, 20, right - 2, 70)) > 0,
            "{font}"
        );
        assert_eq!(dark(&rendering, 100, (right + 3, 0, 200, 100)), 0, "{font}");
    }
    // The PDF's widths win: alpha at 2000 thousandths puts beta at x = 90.
    let rendering = render(with_named_font(
        "BT /F1 40 Tf 10 30 Td (ab) Tj ET",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Symbol /FirstChar 97 /LastChar 98 /Widths [2000 549] >>",
    ));
    assert!(dark(&rendering, 100, (40, 20, 88, 70)) == 0);
    assert!(dark(&rendering, 100, (90, 20, 112, 70)) > 0);
    // Glyph names through /Differences, including a private-use bracket
    // piece of Adobe's glyph list, drawn with its Unicode equivalent.
    let rendering = render(with_named_font(
        "BT /F1 40 Tf 10 30 Td (\\001\\002) Tj ET",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Symbol /Encoding << /Differences [1 /bracketlefttp /Omega] >> >>",
    ));
    assert_eq!(
        rendering.skipped.iter().copied().collect::<Vec<_>>(),
        [Unsupported::FontSubstituted]
    );
    assert!(dark(&rendering, 100, (10, 0, 25, 100)) > 0);
    assert!(dark(&rendering, 100, (26, 20, 60, 70)) > 0);
}

#[test]
fn other_symbolic_fonts_are_not_replaced() {
    let font =
        "<< /Type /Font /Subtype /TrueType /BaseFont /Wingdings /FontDescriptor << /Flags 4 >> >>";
    assert_eq!(ink_of(font), (vec![Unsupported::Text], 0), "{font}");
    // A name absent from the resources is not a font to replace.
    let rendering = render(with_named_font(
        "BT /F9 40 Tf 10 30 Td (A) Tj ET",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    ));
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

#[cfg(feature = "standard-fonts")]
#[test]
fn unreadable_fonts_are_replaced_or_skipped_not_fatal() {
    // A damaged program: replaced (the font is not symbolic).
    let rendering = render(with_font(
        "BT /F1 40 Tf 20 30 Td (A) Tj ET",
        WINANSI,
        b"not a font",
    ));
    assert_eq!(
        rendering.skipped.iter().copied().collect::<Vec<_>>(),
        [Unsupported::FontSubstituted]
    );
    assert!(dark(&rendering, 100, (20, 30, 60, 60)) > 50);
    // An unknown resource name: skipped.
    let rendering = render(with_font(
        "BT /F9 40 Tf 20 30 Td (A) Tj ET",
        WINANSI,
        &arimo(),
    ));
    assert_eq!(
        rendering.skipped.iter().copied().collect::<Vec<_>>(),
        [Unsupported::Text]
    );
    assert_eq!(dark(&rendering, 100, (0, 0, 200, 100)), 0);
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
            assert_eq!(got, want, "({x}, {y}) {got:?} != {want:?}");
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

#[test]
fn glyphs_of_one_operator_are_bounded_before_allocation() {
    // Path work counts at least one unit per segment: one operator showing
    // many glyphs is refused as soon as its outlines would exceed it.
    let renders = |count: usize, limit: u64| {
        let content = format!("BT /F1 10 Tf 0 50 Td ({}) Tj ET", "A".repeat(count));
        let mut options = RenderOptions::default();
        options.limits.max_path_work = limit;
        render_with(with_font(&content, WINANSI, &arimo()), &options)
    };
    let (mut low, mut high) = (1_u64, 1_000_000_u64);
    assert!(renders(50, high).is_ok());
    while low < high {
        let middle = (low + high) / 2;
        if renders(50, middle).is_ok() {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    assert_eq!(
        renders(50, low - 1).err(),
        Some(Error::LimitExceeded {
            kind: RenderLimitKind::PathWork,
            limit: low - 1
        })
    );
    // A million glyphs (25 million segments) with the default limits: every
    // glyph segment counts as an operation, so the 10 million operations
    // stop it before its path reaches about 560 MB.
    let started = std::time::Instant::now();
    assert_eq!(
        renders(1_000_000, 50_000_000).err(),
        Some(Error::LimitExceeded {
            kind: RenderLimitKind::Operations,
            limit: 10_000_000
        })
    );
    assert!(started.elapsed().as_secs() < 60);
}

#[test]
fn a_glyph_name_the_font_lacks_is_reported() {
    let rendering = render(with_font(
        "BT /F1 40 Tf 20 30 Td (AB) Tj ET",
        " /Encoding << /Differences [66 /nosuchglyph] >>",
        &arimo(),
    ));
    assert_eq!(
        rendering.skipped.iter().copied().collect::<Vec<_>>(),
        [Unsupported::Text]
    );
    // The A is still drawn.
    assert!(dark(&rendering, 100, (20, 30, 50, 60)) > 50);
    // `.notdef` is not a lost glyph.
    let rendering = render(with_font(
        "BT /F1 40 Tf 20 30 Td (AB) Tj ET",
        " /Encoding << /Differences [66 /.notdef] >>",
        &arimo(),
    ));
    assert!(rendering.skipped.is_empty());
}

fn source_sans_cff() -> Vec<u8> {
    std::fs::read(repo("tests/fonts/source-sans-subset.cff")).expect("font")
}

#[test]
fn the_cff_fixture_draws_every_line() {
    let rendering = render(std::fs::read(repo("tests/corpus/public/cff.pdf")).expect("fixture"));
    assert!(rendering.skipped.is_empty(), "{:?}", rendering.skipped);
    // Type1C (WinAnsi and built-in encoding), CID-keyed CFF, OpenType CFF.
    for baseline in [125, 95, 60, 25] {
        assert!(
            dark(&rendering, 150, (10, baseline, 150, baseline + 14)) > 50,
            "line at {baseline}"
        );
    }
    assert_eq!(dark(&rendering, 150, (260, 0, 300, 150)), 0);
}

#[test]
fn cff_glyphs_are_found_by_name() {
    // Both through WinAnsi names and through the built-in encoding.
    for extra in [WINANSI, ""] {
        let rendering = render(with_cff(
            "BT /F1 40 Tf 20 30 Td (AB) Tj ET",
            extra,
            &source_sans_cff(),
        ));
        assert!(rendering.skipped.is_empty(), "{extra}");
        assert!(dark(&rendering, 100, (20, 30, 70, 60)) > 100, "{extra}");
    }
    // A name the CFF charset lacks is reported; the A is still drawn.
    let rendering = render(with_cff(
        "BT /F1 40 Tf 20 30 Td (AB) Tj ET",
        " /Encoding << /Differences [66 /nosuchglyph] >>",
        &source_sans_cff(),
    ));
    assert_eq!(
        rendering.skipped.iter().copied().collect::<Vec<_>>(),
        [Unsupported::Text]
    );
    assert!(dark(&rendering, 100, (20, 30, 40, 60)) > 50);
}

#[cfg(feature = "standard-fonts")]
#[test]
fn a_damaged_cff_program_is_replaced() {
    let mut program = source_sans_cff();
    program.truncate(40);
    let rendering = render(with_cff(
        "BT /F1 40 Tf 20 30 Td (AB) Tj ET",
        WINANSI,
        &program,
    ));
    assert_eq!(
        rendering.skipped.iter().copied().collect::<Vec<_>>(),
        [Unsupported::FontSubstituted]
    );
    assert!(dark(&rendering, 100, (20, 30, 70, 60)) > 50);
}

#[cfg(feature = "standard-fonts")]
#[test]
fn an_unreadable_type1_program_is_replaced() {
    // A /FontFile that is not a Type 1 program (a header only): a
    // replacement draws it, and says so.
    let rendering = render(with_program(
        "BT /F1 40 Tf 20 30 Td (A) Tj ET",
        "Type1",
        WINANSI,
        "FontFile",
        "",
        b"%!PS-AdobeFont-1.0",
        32,
    ));
    assert_eq!(
        rendering.skipped.iter().copied().collect::<Vec<_>>(),
        [Unsupported::FontSubstituted]
    );
    assert!(dark(&rendering, 100, (20, 30, 60, 60)) > 50);
}

#[cfg(not(feature = "standard-fonts"))]
#[test]
fn without_replacement_fonts_the_text_is_skipped() {
    let rendering =
        render(std::fs::read(repo("tests/corpus/public/minimal.pdf")).expect("fixture"));
    assert_eq!(
        rendering.skipped.iter().copied().collect::<Vec<_>>(),
        [Unsupported::Text]
    );
    assert_eq!(dark(&rendering, 842, (0, 0, 595, 842)), 0);
}

#[test]
fn embedded_type1_programs_are_drawn_and_give_their_text() {
    // ADR-0005 amended, tranche 3.8: a synthetic Type 1 font, binary and
    // hexadecimal eexec, built-in encoding and /Differences, a subroutine
    // and a seac composite (scripts/make-fixtures.mjs, `type1Program`).
    let bytes = std::fs::read(repo("tests/corpus/public/type1.pdf")).expect("fixture");
    let rendering = render(bytes.clone());
    assert!(rendering.skipped.is_empty(), "{:?}", rendering.skipped);
    // The triangle "A" of the first line at 40 pt: ink at its base, middle.
    assert!(dark(&rendering, 180, (18, 131, 26, 135)) > 0);
    let document = document(bytes);
    let text = pdf_render::page_text(
        document.file(),
        document.page(0).expect("page"),
        &pdf_render::RenderLimits::default(),
        &mut budget(),
    )
    .expect("text");
    let all: String = text
        .glyphs
        .iter()
        .filter_map(|glyph| glyph.text.as_deref())
        .collect();
    assert_eq!(all, "ABC O \u{c1}ABC OBOA \u{c1}CABO");
}

/// The Type 1 program of `type1.pdf` (object 11): binary eexec,
/// FontMatrix 0.001.
fn type1_program() -> Vec<u8> {
    let pdf = std::fs::read(repo("tests/corpus/public/type1.pdf")).expect("fixture");
    let find = |from: usize, needle: &[u8]| {
        pdf[from..]
            .windows(needle.len())
            .position(|window| window == needle)
            .map(|at| from + at)
            .expect("marker")
    };
    let object = find(0, b"11 0 obj\n<< /Length ") + 20;
    let digits = pdf[object..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    let length: usize = std::str::from_utf8(&pdf[object..object + digits])
        .expect("digits")
        .parse()
        .expect("length");
    let start = find(object, b"stream\n") + 7;
    pdf[start..start + length].to_vec()
}

#[test]
fn without_widths_a_type1_program_gives_its_advances() {
    // No /Widths: "A" advances 600 units of 1000, so the second "A" starts
    // 24 points after the first at 40 points.
    let bytes = with_program(
        "BT /F1 40 Tf 10 30 Td (AA) Tj ET",
        "Type1",
        "",
        "FontFile",
        "",
        &type1_program(),
        4,
    );
    let document = document(bytes);
    let text = pdf_render::page_text(
        document.file(),
        document.page(0).expect("page"),
        &pdf_render::RenderLimits::default(),
        &mut budget(),
    )
    .expect("text");
    let xs: Vec<f64> = text.glyphs.iter().map(|glyph| glyph.x).collect();
    assert_eq!(xs.len(), 2);
    assert!((xs[1] - xs[0] - 24.0).abs() < 1e-6, "{xs:?}");
}

#[test]
fn a_type1_program_under_a_cid_font_is_not_read() {
    // /FontFile under a CIDFontType0: refused, its text not drawn.
    let rendering = render(with_program(
        "BT /F1 40 Tf 10 30 Td <0041> Tj ET",
        "Type0",
        "/Encoding /Identity-H /DescendantFonts [<< /Type /Font /Subtype /CIDFontType0 /BaseFont /F /FontDescriptor 7 0 R >>]",
        "FontFile",
        "",
        &type1_program(),
        4,
    ));
    assert!(rendering.skipped.contains(&Unsupported::Text));
}
