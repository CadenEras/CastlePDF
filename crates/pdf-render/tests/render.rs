//! Rendering of the `shapes.pdf` fixture probed at known points, and content
//! errors, limits and skipped capabilities on small documents built here.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use pdf_document::Document;
use pdf_render::{
    ContentErrorKind, Error, RenderLimitKind, RenderOptions, Rendering, Unsupported, render_page,
};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/public")
        .join(name);
    std::fs::read(path).expect("fixture")
}

fn render(pdf: Vec<u8>, index: usize, options: &RenderOptions) -> Result<Rendering, Error> {
    let mut budget = budget();
    let document = Document::open(
        MemorySource::from_vec(pdf, &ResourceLimits::default()).expect("source"),
        &mut budget,
    )
    .expect("document");
    let page = document.page(index).expect("page");
    render_page(document.file(), page, options, &mut budget)
}

/// Classic-xref PDF with one page of the given size, content and resources.
fn one_page(media_box: &str, extra: &str, content: &str) -> Vec<u8> {
    page_with_objects(media_box, extra, "4 0 R", content, &[])
}

/// Like `one_page`, with the `/Contents` value given and `more` objects
/// numbered from 9.
fn page_with_objects(
    media_box: &str,
    extra: &str,
    contents: &str,
    content: &str,
    more: &[&str],
) -> Vec<u8> {
    let mut bodies = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox {media_box} {extra} /Contents {contents} >>"
        ),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        "<< /Type /ExtGState /CA 0.25 /LW 3 /BM /Multiply >>".to_owned(),
        "[/ICCBased 6 0 R]".to_owned(),
        "<< /FunctionType 4 /Domain [0 1] /Range [0 1] /Length 5 >>
stream
{1 }
endstream"
            .to_owned(),
        "<< /Type /ExtGState /TR 7 0 R /HT 7 0 R /BG2 7 0 R /ca 0.5 >>".to_owned(),
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

fn small(content: &str) -> Vec<u8> {
    one_page(
        "[0 0 20 20]",
        concat!(
            "/Resources << /ExtGState << /G 5 0 R /G2 8 0 R >> ",
            "/ColorSpace << /CS0 6 0 R /CS1 /DeviceRGB /P /Pattern >> ",
            "/XObject << /Im1 7 0 R >> /Shading << /Sh1 6 0 R >> /Pattern << /P1 6 0 R >> ",
            "/Properties << /L1 << /Type /OCG >> >> >>",
        ),
        content,
    )
}

fn render_small(content: &str) -> Result<Rendering, Error> {
    render(small(content), 0, &RenderOptions::default())
}

fn content_error(content: &str) -> (u64, ContentErrorKind) {
    match render_small(content) {
        Err(Error::Content { offset, kind, .. }) => (offset, kind),
        other => panic!("expected a content error for {content:?}, got {other:?}"),
    }
}

/// `(x, y)` in user space of a 200-unit-high page at scale 1: pixel centre.
fn at(rendering: &Rendering, x: u32, y: u32) -> [u8; 4] {
    rendering
        .bitmap
        .pixel(x, 200 - y)
        .expect("probe inside the bitmap")
}

fn close(actual: [u8; 4], expected: [u8; 3]) -> bool {
    actual[3] == 255 && actual.iter().zip(expected).all(|(a, e)| a.abs_diff(e) <= 2)
}

#[track_caller]
fn assert_color(actual: [u8; 4], expected: [u8; 3]) {
    assert!(
        close(actual, expected),
        "pixel {actual:?}, expected {expected:?}"
    );
}

const BACKGROUND: [u8; 3] = [230, 230, 230];

#[test]
fn shapes_page_matches_its_probes() {
    let rendering = render(fixture("shapes.pdf"), 0, &RenderOptions::default()).expect("render");
    assert_eq!(
        (rendering.bitmap.width(), rendering.bitmap.height()),
        (200, 200)
    );
    assert_eq!(rendering.skipped, BTreeSet::new());
    // Fill, split across the two content streams.
    assert_color(at(&rendering, 50, 40), [255, 0, 0]);
    // Stroke: on the edge, not inside.
    assert_color(at(&rendering, 110, 40), [0, 0, 255]);
    assert_color(at(&rendering, 150, 40), BACKGROUND);
    // Rotated square: centre painted, corner of the unrotated square not.
    assert_color(at(&rendering, 50, 120), [0, 153, 0]);
    assert_color(at(&rendering, 32, 102), BACKGROUND);
    // Circular clip.
    assert_color(at(&rendering, 150, 130), [255, 255, 0]);
    assert_color(at(&rendering, 123, 103), BACKGROUND);
    assert_color(at(&rendering, 115, 95), BACKGROUND);
    // 50 % red over the grey background.
    assert_color(at(&rendering, 65, 182), [243, 115, 115]);
    // Even-odd ring: ring painted, hole not.
    assert_color(at(&rendering, 93, 182), [0, 0, 0]);
    assert_color(at(&rendering, 105, 182), BACKGROUND);
    // Dashes [6 4] from x = 130: dash, gap, dash.
    assert_color(at(&rendering, 133, 182), [0, 0, 0]);
    assert_color(at(&rendering, 138, 182), BACKGROUND);
    assert_color(at(&rendering, 143, 182), [0, 0, 0]);
    // The `q`/`Q` around the clip restored it: the dashed line is outside.
    assert_color(at(&rendering, 5, 5), BACKGROUND);
}

#[test]
fn device_cmyk_uses_the_iso_conversion() {
    // ISO 32000-2, 10.4.2.4: red = 1 - min(1, cyan + black), and so on.
    let rendering = render(fixture("shapes.pdf"), 2, &RenderOptions::default()).expect("render");
    let bitmap = &rendering.bitmap;
    assert_color(bitmap.pixel(5, 5).expect("pixel"), [0, 255, 255]);
    assert_color(bitmap.pixel(15, 5).expect("pixel"), [255, 0, 255]);
    assert_color(bitmap.pixel(5, 15).expect("pixel"), [255, 255, 0]);
    assert_color(bitmap.pixel(15, 15).expect("pixel"), [0, 0, 0]);
}

#[test]
fn rotated_page_and_scale() {
    let rendering = render(fixture("shapes.pdf"), 1, &RenderOptions::default()).expect("render");
    // 200 x 100 rotated 90 degrees clockwise: 100 wide, 200 high, and user
    // x maps to device y.
    assert_eq!(
        (rendering.bitmap.width(), rendering.bitmap.height()),
        (100, 200)
    );
    assert_color(rendering.bitmap.pixel(50, 25).expect("pixel"), [255, 0, 0]);
    assert_color(
        rendering.bitmap.pixel(50, 150).expect("pixel"),
        [255, 255, 255],
    );

    let mut options = RenderOptions::default();
    options.scale = 0.5;
    let half = render(fixture("shapes.pdf"), 0, &options).expect("render");
    assert_eq!((half.bitmap.width(), half.bitmap.height()), (100, 100));
    assert_color(half.bitmap.pixel(25, 80).expect("pixel"), [255, 0, 0]);
    assert_eq!(half.bitmap.data().len(), 100 * 100 * 4);
}

#[test]
fn every_rotation_keeps_the_top_left_corner_in_place() {
    // A red square in the top-left corner of a 20 x 10 page.
    let content = "1 0 0 rg 0 5 5 5 re f";
    for (rotation, probe, size) in [
        (0, (2, 2), (20, 10)),
        (90, (7, 2), (10, 20)),
        (180, (17, 7), (20, 10)),
        (270, (2, 17), (10, 20)),
    ] {
        let pdf = one_page("[0 0 20 10]", &format!("/Rotate {rotation}"), content);
        let rendering = render(pdf, 0, &RenderOptions::default()).expect("render");
        let bitmap = &rendering.bitmap;
        assert_eq!(
            (bitmap.width(), bitmap.height()),
            size,
            "rotation {rotation}"
        );
        assert_color(bitmap.pixel(probe.0, probe.1).expect("pixel"), [255, 0, 0]);
        let (pixels, _) = bitmap.data().as_chunks::<4>();
        let painted = pixels.iter().filter(|p| close(**p, [255, 0, 0])).count();
        assert_eq!(painted, 25, "rotation {rotation}");
    }
}

#[test]
fn crop_box_origin_is_honoured() {
    let pdf = one_page(
        "[100 100 120 120]",
        "/CropBox [110 100 120 110]",
        "0 0 1 rg 110 100 5 5 re f",
    );
    let rendering = render(pdf, 0, &RenderOptions::default()).expect("render");
    assert_eq!(
        (rendering.bitmap.width(), rendering.bitmap.height()),
        (10, 10)
    );
    assert_color(rendering.bitmap.pixel(2, 7).expect("pixel"), [0, 0, 255]);
    assert_color(
        rendering.bitmap.pixel(7, 2).expect("pixel"),
        [255, 255, 255],
    );
}

#[test]
fn ext_g_state_and_unsupported_capabilities_are_reported() {
    let rendering = render_small(
        "q /G gs 0 0 1 RG 2 2 m 18 2 l S Q \
         BT /F1 12 Tf 2 2 Td (x) Tj ET \
         /Im1 Do /Sh1 sh BI /W 1 /H 1 /BPC 8 /CS /G ID \x7f EI \
         /OC /L1 BDC EMC \
         /CS0 cs 0.5 0.5 0.5 sc 0 0 5 5 re f",
    )
    .expect("render");
    let expected: BTreeSet<_> = [
        Unsupported::Text,
        Unsupported::XObject,
        Unsupported::Shading,
        Unsupported::Image,
        Unsupported::OptionalContent,
        Unsupported::BlendMode,
        Unsupported::ColorSpace,
    ]
    .into_iter()
    .collect();
    assert_eq!(rendering.skipped, expected);
    // The fill in the unsupported colour space is skipped, not painted black
    // (user (2, 4.5): inside the box, above the 3-unit line).
    assert_color(
        rendering.bitmap.pixel(2, 15).expect("pixel"),
        [255, 255, 255],
    );
    // CA 0.25 over white on a 3-unit line (LW from the ExtGState).
    assert_color(
        rendering.bitmap.pixel(10, 17).expect("pixel"),
        [191, 191, 255],
    );
    // A device space through a resource name is supported.
    let rendering = render_small("/CS1 cs 0 1 0 sc 0 0 20 20 re f").expect("render");
    assert!(rendering.skipped.is_empty());
    assert_color(rendering.bitmap.pixel(10, 10).expect("pixel"), [0, 255, 0]);
}

#[test]
fn content_errors_are_located() {
    use ContentErrorKind::*;
    assert_eq!(content_error("0 0 m 1 1 lineto S"), (10, UnknownOperator));
    assert_eq!(content_error("1 1 l"), (4, NoCurrentPoint));
    assert_eq!(content_error("1 1 2 2 v"), (8, NoCurrentPoint));
    assert_eq!(content_error("q Q Q"), (4, UnbalancedRestore));
    assert_eq!(
        content_error("1 0 0 rg 0 0 m 1 0 0 rg"),
        (21, MisplacedOperator)
    );
    assert_eq!(content_error("BT 0 0 m ET"), (7, MisplacedOperator));
    assert_eq!(content_error("BT q ET"), (3, MisplacedOperator));
    assert_eq!(content_error("BT BT"), (3, MisplacedOperator));
    assert_eq!(content_error("ET"), (0, MisplacedOperator));
    assert_eq!(content_error("(x) Tj"), (4, MisplacedOperator));
    assert_eq!(content_error("EX"), (0, MisplacedOperator));
    assert_eq!(content_error("0 0 d0"), (4, MisplacedOperator));
    assert_eq!(content_error("1 2 rg"), (4, InvalidOperands));
    assert_eq!(content_error("(a) g"), (4, InvalidOperands));
    assert_eq!(content_error("-1 w"), (3, InvalidOperands));
    assert_eq!(content_error("3 J"), (2, InvalidOperands));
    assert_eq!(content_error("[0 0] 0 d"), (8, InvalidOperands));
    assert_eq!(content_error("[-1 2] 0 d"), (9, InvalidOperands));
    assert_eq!(content_error("q 1 Q"), (4, InvalidOperands));
    assert_eq!(content_error("/Missing gs"), (9, InvalidResource));
    assert_eq!(content_error("/CS9 cs"), (5, InvalidResource));
    // PDF numbers have no exponent: overflow the CTM by repeated scaling.
    let scale = "9000000000000000000 0 0 9000000000000000000 0 0 cm ";
    let overflow = scale.repeat(18);
    // (9e18)^17 is beyond f64::MAX: the 17th `cm` overflows.
    let last = (17 * scale.len() - 3) as u64;
    assert_eq!(content_error(&overflow), (last, NumberOutOfRange));
    assert_eq!(
        content_error("q ) Q").1,
        Syntax(pdf_syntax::MalformedKind::UnexpectedDelimiter)
    );
    assert_eq!(content_error("0 0 m 1"), (6, Truncated));
}

#[test]
fn compatibility_sections_ignore_unknown_operators() {
    let rendering =
        render_small("BX 1 2 foo BX bar EX EX 0 0 1 rg 0 0 20 20 re f").expect("render");
    assert_color(rendering.bitmap.pixel(10, 10).expect("pixel"), [0, 0, 255]);
    assert_eq!(
        content_error("BX EX foo"),
        (6, ContentErrorKind::UnknownOperator)
    );
}

#[test]
fn clips_are_restored_by_q_and_q() {
    // Clip to the left half inside q/Q, then fill everything after Q.
    let rendering =
        render_small("q 0 0 10 20 re W n 1 0 0 rg 0 0 20 20 re f Q 0 0 1 rg 10 0 10 20 re f")
            .expect("render");
    assert_color(rendering.bitmap.pixel(5, 10).expect("pixel"), [255, 0, 0]);
    assert_color(rendering.bitmap.pixel(15, 10).expect("pixel"), [0, 0, 255]);
    // Without q/Q the clip stays: nothing outside the left half.
    let rendering = render_small("0 0 10 20 re W n 0 0 1 rg 0 0 20 20 re f").expect("render");
    assert_color(
        rendering.bitmap.pixel(15, 10).expect("pixel"),
        [255, 255, 255],
    );
    // Two clips in one q/Q are both removed by Q.
    let rendering = render_small("q 0 0 10 20 re W n 0 0 20 10 re W n Q 0 1 0 rg 0 0 20 20 re f")
        .expect("render");
    assert_color(rendering.bitmap.pixel(15, 5).expect("pixel"), [0, 255, 0]);
}

#[test]
fn hairlines_stay_visible_at_any_scale() {
    let rendering =
        render_small("0.001 0 0 0.001 0 0 cm 0 w 0 10000 m 20000 10000 l S").expect("render");
    let row = 10;
    let dark = (0..20)
        .filter(|x| rendering.bitmap.pixel(*x, row).is_some_and(|p| p[0] < 200))
        .count();
    assert!(dark >= 18, "hairline drawn on {dark} pixels");
}

#[test]
fn limits_are_exact() {
    let content = "q Q q Q 0 0 1 1 re f";
    // 6 operations: exact limit passes, one less fails.
    let mut options = RenderOptions::default();
    options.limits.max_operations = 6;
    assert!(render(small(content), 0, &options).is_ok());
    options.limits.max_operations = 5;
    assert_eq!(
        render(small(content), 0, &options).err(),
        Some(Error::LimitExceeded {
            kind: RenderLimitKind::Operations,
            limit: 5
        })
    );

    let mut options = RenderOptions::default();
    options.limits.max_state_depth = 2;
    assert!(render(small("q q Q Q"), 0, &options).is_ok());
    assert_eq!(
        render(small("q q q"), 0, &options).err(),
        Some(Error::LimitExceeded {
            kind: RenderLimitKind::StateDepth,
            limit: 2
        })
    );

    // 20 x 20 page: 400 pixels at scale 1.
    let mut options = RenderOptions::default();
    options.limits.max_pixels = 400;
    assert!(render(small(""), 0, &options).is_ok());
    options.limits.max_pixels = 399;
    assert_eq!(
        render(small(""), 0, &options).err(),
        Some(Error::LimitExceeded {
            kind: RenderLimitKind::Pixels,
            limit: 399
        })
    );
    // More than 65 535 pixels on a side, whatever the pixel limit.
    let mut options = RenderOptions::default();
    options.limits.max_pixels = u64::MAX;
    options.scale = 4000.0;
    assert!(matches!(
        render(small(""), 0, &options),
        Err(Error::LimitExceeded {
            kind: RenderLimitKind::Pixels,
            ..
        })
    ));
}

#[test]
fn path_work_bounds_dashes_and_huge_curves() {
    // A 1 x 1 rectangle filled: move 1, three sides of 1 + 1/4 (one pixel
    // of extent, in 4-pixel tiles), close 1, area 1/1024: 5.751.
    let mut options = RenderOptions::default();
    options.limits.max_path_work = 6;
    assert!(render(small("0 0 1 1 re f"), 0, &options).is_ok());
    options.limits.max_path_work = 5;
    assert!(matches!(
        render(small("0 0 1 1 re f"), 0, &options),
        Err(Error::LimitExceeded {
            kind: RenderLimitKind::PathWork,
            limit: 5
        })
    ));
    // Coverage counts: each full-page rectangle at 1 Mpx costs about
    // 1000 (its area over 1024), so 2000 of them exceed a budget of one
    // million, whereas 2000 one-pixel rectangles do not.
    let mut options = RenderOptions::default();
    options.scale = 50.0;
    options.limits.max_path_work = 1_000_000;
    let small_boxes = "0 0 0.02 0.02 re f ".repeat(2000);
    assert!(render(small(&small_boxes), 0, &options).is_ok());
    let big = "0 0 20 20 re f ".repeat(2000);
    assert!(matches!(
        render(small(&big), 0, &options),
        Err(Error::LimitExceeded {
            kind: RenderLimitKind::PathWork,
            ..
        })
    ));
    // A hairline dashed through an extremely anisotropic CTM: dashes are cut
    // in user space and counted there (it used to loop in the rasteriser).
    assert!(matches!(
        render_small("0 w [1 1] 0 d 1 0 0 0.0000000001 0 0 cm 0 0 m 100000000 0 l S"),
        Err(Error::LimitExceeded {
            kind: RenderLimitKind::PathWork,
            ..
        })
    ));
    assert!(render_small("0 w [1 1] 0 d 1 0 0 0.0000000001 0 0 cm 0 0 m 100 0 l S").is_ok());
    assert!(render_small("0 w [1 1] 0 d 0 0 0 0 0 0 cm 0 0 m 100 0 l S").is_ok());
    // Points too far away, even if finite.
    assert_eq!(
        content_error("0 0 m 9000000000000000000 0 l S").1,
        ContentErrorKind::NumberOutOfRange
    );
    // A billion dashes on a 10-unit line is refused before stroking.
    let dashes = render_small("[0.00000001] 0 d 0 10 m 10 10 l S");
    assert!(matches!(
        dashes,
        Err(Error::LimitExceeded {
            kind: RenderLimitKind::PathWork,
            ..
        })
    ));
    // A curve of 1e11 units costs over a million; a line of the same length
    // is cheap (only its part inside the canvas counts).
    let mut options = RenderOptions::default();
    options.limits.max_path_work = 1_000_000;
    assert!(matches!(
        render(
            small("0 0 m 100000000000 100000000000 0 100000000000 0 0 c f"),
            0,
            &options
        ),
        Err(Error::LimitExceeded {
            kind: RenderLimitKind::PathWork,
            ..
        })
    ));
    let line = render(small("0 0 m 100000000000 0 l 0 1 l f"), 0, &options);
    assert!(line.is_ok(), "{line:?}");
}

#[test]
fn invalid_requests_and_contents() {
    for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let mut options = RenderOptions::default();
        options.scale = scale;
        assert_eq!(
            render(small(""), 0, &options).err(),
            Some(Error::InvalidRequest)
        );
    }
    let zero = one_page("[0 0 0 10]", "", "");
    assert_eq!(
        render(zero, 0, &RenderOptions::default()).err(),
        Some(Error::InvalidRequest)
    );
    let pdf = String::from_utf8(small("")).expect("ascii");
    let not_stream = pdf.replace("/Contents 4 0 R", "/Contents 5 0 R");
    assert!(matches!(
        render(not_stream.into_bytes(), 0, &RenderOptions::default()),
        Err(Error::Document(pdf_document::Error::InvalidStructure {
            kind: pdf_document::StructureErrorKind::InvalidContents,
            ..
        }))
    ));
}

/// ISO 32000-2, 7.7.3.3: `/Contents` may be a reference to an array of
/// stream references (seen in real files).
#[test]
fn contents_may_be_an_indirect_array() {
    // The graphics state carries over from one stream to the next.
    let second = "0 g 10 10 10 10 re f";
    let second = format!(
        "<< /Length {} >>
stream
{second}
endstream",
        second.len()
    );
    let pdf = page_with_objects(
        "[0 0 20 20]",
        "",
        "9 0 R",
        "1 0 0 rg 0 0 10 10 re f",
        &["[4 0 R 10 0 R]", &second],
    );
    let rendering = render(pdf, 0, &RenderOptions::default()).expect("rendering");
    // Device pixels of the 20 x 20 page: user y grows upwards.
    let pixel = |x, y| rendering.bitmap.pixel(x, y).expect("inside");
    assert_color(pixel(5, 15), [255, 0, 0]);
    assert_color(pixel(15, 5), [0, 0, 0]);
    assert_color(pixel(15, 15), [255, 255, 255]);
    // One level only: an array referencing an array (here itself), a number
    // or a dictionary is refused.
    for (contents, body) in [
        ("9 0 R", "[9 0 R]"),
        ("9 0 R", "[4 0 R 10 0 R]"),
        ("9 0 R", "[4 0 R 5 0 R]"),
        ("9 0 R", "[4 0 R 1]"),
        ("[9 0 R]", "[4 0 R]"),
    ] {
        let pdf = page_with_objects("[0 0 20 20]", "", contents, "", &[body, "[4 0 R]"]);
        assert!(
            matches!(
                render(pdf, 0, &RenderOptions::default()),
                Err(Error::Document(pdf_document::Error::InvalidStructure {
                    kind: pdf_document::StructureErrorKind::InvalidContents,
                    ..
                }))
            ),
            "{contents} -> {body}"
        );
    }
}

#[test]
fn cancellation_is_honoured() {
    let pdf = small("0 0 1 1 re f");
    let cancel = CancellationToken::new();
    let mut budget = Budget::new(ResourceLimits::default(), cancel.clone());
    let document = Document::open(
        MemorySource::from_vec(pdf, &ResourceLimits::default()).expect("source"),
        &mut budget,
    )
    .expect("document");
    cancel.cancel();
    let page = document.page(0).expect("page");
    assert_eq!(
        render_page(
            document.file(),
            page,
            &RenderOptions::default(),
            &mut budget
        )
        .err(),
        Some(Error::Syntax(pdf_syntax::Error::Cancelled))
    );
}

#[test]
fn device_cmyk_mixes_inks_as_iso_specifies() {
    // C = K = 0.5: red = 1 - min(1, 0.5 + 0.5) = 0, green = blue = 0.5.
    let rendering = render_small("0.5 0 0 0.5 k 0 0 20 20 re f").expect("render");
    assert_color(
        rendering.bitmap.pixel(10, 10).expect("pixel"),
        [0, 128, 128],
    );
}

#[test]
fn ext_g_state_streams_are_not_resolved() {
    // /TR, /HT and /BG2 are streams: the page renders, the transfer
    // function is reported, the fill alpha applies.
    let rendering = render_small("/G2 gs 0 g 0 0 20 20 re f").expect("render");
    assert_eq!(
        rendering.skipped,
        [Unsupported::TransferFunction].into_iter().collect()
    );
    assert_color(
        rendering.bitmap.pixel(10, 10).expect("pixel"),
        [128, 128, 128],
    );
}

#[test]
fn dash_arrays_and_clips_are_bounded() {
    let dashes = |n: usize| format!("[{}] 0 d", "1 ".repeat(n));
    assert!(render_small(&dashes(pdf_render::MAX_DASH_ENTRIES)).is_ok());
    assert_eq!(
        content_error(&dashes(pdf_render::MAX_DASH_ENTRIES + 1)).1,
        ContentErrorKind::InvalidOperands
    );
    let mut options = RenderOptions::default();
    options.limits.max_clips = 2;
    let clips = |n: usize| format!("q {}Q ", "0 0 5 5 re W n ".repeat(n));
    assert!(render(small(&clips(2)), 0, &options).is_ok());
    assert_eq!(
        render(small(&clips(3)), 0, &options).err(),
        Some(Error::LimitExceeded {
            kind: RenderLimitKind::Clips,
            limit: 2
        })
    );
    // Q releases its clips: the count is per active clip, not cumulative.
    let sequential = clips(2).repeat(5);
    assert!(render(small(&sequential), 0, &options).is_ok());
}

#[test]
fn content_must_be_terminated_and_operands_checked() {
    use ContentErrorKind::*;
    for content in ["BT", "0 0 m", "0 0 1 1 re W", "q", "/X BMC", "BX"] {
        assert_eq!(
            content_error(content),
            (content.len() as u64 + 1, Unterminated),
            "{content}"
        );
    }
    assert_eq!(content_error("EMC"), (0, MisplacedOperator));
    assert_eq!(content_error("/Absent Do"), (8, InvalidResource));
    assert_eq!(content_error("/Sh9 sh"), (5, InvalidResource));
    assert_eq!(content_error("/OC /L9 BDC EMC"), (8, InvalidResource));
    assert_eq!(content_error("/OC 1 BDC EMC"), (6, InvalidOperands));
    assert_eq!(content_error("1 Do"), (2, InvalidOperands));
    assert_eq!(content_error("BT 1 Tj ET"), (5, InvalidOperands));
    assert_eq!(content_error("BT [(a) /x] TJ ET"), (12, InvalidOperands));
    assert_eq!(content_error("8 Tr"), (2, InvalidOperands));
    assert_eq!(content_error("/P cs 1 sc"), (8, InvalidOperands));
    assert_eq!(content_error("/P cs /P9 scn"), (10, InvalidResource));
    assert_eq!(content_error("/CS0 cs /x sc"), (11, InvalidOperands));
    let rendering = render_small("/P cs /P1 scn 0 0 5 5 re f 4 Tr").expect("render");
    assert_eq!(
        rendering.skipped,
        [Unsupported::Pattern, Unsupported::TextClip]
            .into_iter()
            .collect()
    );
}

#[test]
fn default_color_spaces_are_reported() {
    let pdf = one_page(
        "[0 0 20 20]",
        "/Resources << /ColorSpace << /DefaultRGB 6 0 R >> >>",
        "1 0 0 rg 0 0 20 20 re f 0.5 g 0 0 5 5 re f",
    );
    let rendering = render(pdf, 0, &RenderOptions::default()).expect("render");
    assert_eq!(
        rendering.skipped,
        [Unsupported::ColorSpace].into_iter().collect()
    );
    // The RGB fill is skipped; DeviceGray is not replaced.
    assert_color(
        rendering.bitmap.pixel(10, 10).expect("pixel"),
        [255, 255, 255],
    );
    assert_color(
        rendering.bitmap.pixel(2, 17).expect("pixel"),
        [128, 128, 128],
    );
}

#[test]
fn v_and_y_repeat_an_end_point() {
    let same = |a: &str, b: &str| {
        let a = render_small(a).expect("render");
        let b = render_small(b).expect("render");
        assert_eq!(a.bitmap.data(), b.bitmap.data());
    };
    same("0 0 m 20 0 20 20 v f", "0 0 m 0 0 20 0 20 20 c f");
    same("0 0 m 20 0 20 20 y f", "0 0 m 20 0 20 20 20 20 c f");
}

#[test]
fn hairlines_and_dashes_under_anisotropic_ctm() {
    // x scaled by 2, y by 0.5: a hairline at user y = 20 is device row 10.
    let rendering = render_small("2 0 0 0.5 0 0 cm 0 w 0 20 m 10 20 l S").expect("render");
    let dark = |y| rendering.bitmap.pixel(10, y).is_some_and(|p| p[0] < 200);
    assert!(dark(9) || dark(10));
    assert!(!dark(5) && !dark(15));
    // Dashes follow the user-space path: [1 1] stretched twice along x.
    let rendering = render_small("2 0 0 1 0 0 cm 2 w [1 1] 0 d 0 10 m 10 10 l S").expect("render");
    let row: Vec<bool> = (0..20)
        .map(|x| rendering.bitmap.pixel(x, 10).is_some_and(|p| p[0] < 128))
        .collect();
    assert!(
        row[0] && row[1] && !row[2] && !row[3] && row[4] && row[5],
        "{row:?}"
    );
}

#[test]
fn user_unit_and_wide_canvases() {
    let pdf = one_page("[0 0 20 20]", "/UserUnit 2", "1 0 0 rg 0 0 10 10 re f");
    let rendering = render(pdf, 0, &RenderOptions::default()).expect("render");
    assert_eq!(
        (rendering.bitmap.width(), rendering.bitmap.height()),
        (40, 40)
    );
    assert_color(rendering.bitmap.pixel(10, 30).expect("pixel"), [255, 0, 0]);
    let invalid = one_page("[0 0 20 20]", "/UserUnit 0", "");
    assert!(matches!(
        Document::open(
            MemorySource::from_vec(invalid, &ResourceLimits::default()).expect("source"),
            &mut budget()
        ),
        Err(pdf_document::Error::InvalidStructure {
            kind: pdf_document::StructureErrorKind::InvalidUserUnit,
            ..
        })
    ));
    // The widest and tallest canvases: their last pixels are painted (a
    // wider one used to panic in the rasteriser).
    for media_box in ["[0 0 16384 1]", "[0 0 1 16384]"] {
        let pdf = one_page(media_box, "", "1 0 0 rg 0 0 16384 16384 re f");
        let rendering = render(pdf, 0, &RenderOptions::default()).expect("render");
        let bitmap = &rendering.bitmap;
        assert_eq!(
            bitmap.width().max(bitmap.height()),
            u32::from(pdf_render::MAX_SIDE)
        );
        let last = bitmap.pixel(bitmap.width() - 1, bitmap.height() - 1);
        assert_color(last.expect("pixel"), [255, 0, 0]);
    }
    let pdf = one_page("[0 0 16385 1]", "", "");
    assert!(matches!(
        render(pdf, 0, &RenderOptions::default()),
        Err(Error::LimitExceeded {
            kind: RenderLimitKind::Pixels,
            ..
        })
    ));
}

#[test]
fn dashing_a_degenerate_subpath_does_not_panic() {
    // Found by fuzzing: kurbo's dash output for a closed single point began
    // with a segment, not a move, and building a path from it panicked.
    for content in [
        "[6 4] 0 d 2 2 m s",
        "[6 4] 0 d 2 2 m h S",
        "[6 4] 3 d 2 2 m 2 2 l h 5 5 m S",
        "0 w [6 4] 0 d 2 2 m s",
        "2 0 0 1 0 0 cm [6 4] 0 d 2 2 m s",
    ] {
        let rendering = render_small(content);
        assert!(rendering.is_ok(), "{content}: {rendering:?}");
    }
}
