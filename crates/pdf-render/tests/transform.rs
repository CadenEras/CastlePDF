//! `page_transform`: default user space to the pixels of a rendering, for
//! every rotation, an offset crop box, `/UserUnit`, and pages too large to
//! render whole (ADR-0006, tranche 3.6a).

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test code: a failed expectation is a test failure"
)]

use pdf_document::Document;
use pdf_render::{Error, RenderOptions, page_size, page_transform};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

/// A one-page PDF whose page dictionary has `entries`.
fn pdf(entries: &str) -> Vec<u8> {
    let bodies = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!("<< /Type /Page /Parent 2 0 R {entries} >>"),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

fn transform(entries: &str, scale: f64) -> Result<[f64; 6], Error> {
    let source = MemorySource::from_vec(pdf(entries), &ResourceLimits::default()).expect("source");
    let mut budget = Budget::new(ResourceLimits::default(), CancellationToken::new());
    let document = Document::open(source, &mut budget).expect("document");
    let mut options = RenderOptions::default();
    options.scale = scale;
    page_transform(document.page(0).expect("page"), &options)
}

fn apply(m: [f64; 6], (x, y): (f64, f64)) -> (f64, f64) {
    let [a, b, c, d, e, f] = m;
    (a * x + c * y + e, b * x + d * y + f)
}

#[test]
fn corners_land_on_the_rendering_for_every_rotation() {
    // Crop box from (10, 20) to (110, 220): 100 by 200 user units.
    let box_ = "/MediaBox [10 20 110 220]";
    let cases = [
        // Rotation, then where the top-left, bottom-left and top-right
        // corners of the crop box land, at scale 2.
        (0, (0.0, 0.0), (0.0, 400.0), (200.0, 0.0)),
        (90, (400.0, 0.0), (0.0, 0.0), (400.0, 200.0)),
        (180, (200.0, 400.0), (200.0, 0.0), (0.0, 400.0)),
        (270, (0.0, 200.0), (400.0, 200.0), (0.0, 0.0)),
    ];
    for (rotation, top_left, bottom_left, top_right) in cases {
        let m = transform(&format!("{box_} /Rotate {rotation}"), 2.0).expect("transform");
        assert_eq!(apply(m, (10.0, 220.0)), top_left, "rotation {rotation}");
        assert_eq!(apply(m, (10.0, 20.0)), bottom_left, "rotation {rotation}");
        assert_eq!(apply(m, (110.0, 220.0)), top_right, "rotation {rotation}");
    }
}

#[test]
fn user_unit_scales_like_a_rendering() {
    let m = transform("/MediaBox [0 0 100 100] /UserUnit 2", 1.0).expect("transform");
    assert_eq!(apply(m, (100.0, 0.0)), (200.0, 200.0));
}

#[test]
fn a_page_too_large_to_render_is_placed_all_the_same() {
    let page = "/MediaBox [0 0 20000 100]";
    let source = MemorySource::from_vec(pdf(page), &ResourceLimits::default()).expect("source");
    let mut budget = Budget::new(ResourceLimits::default(), CancellationToken::new());
    let document = Document::open(source, &mut budget).expect("document");
    let page = document.page(0).expect("page");
    assert!(page_size(page, &RenderOptions::default()).is_err());
    let m = page_transform(page, &RenderOptions::default()).expect("transform");
    assert_eq!(apply(m, (20000.0, 0.0)), (20000.0, 100.0));
}

#[test]
fn an_invalid_scale_is_refused() {
    for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(matches!(
            transform("/MediaBox [0 0 10 10]", scale),
            Err(Error::InvalidRequest)
        ));
    }
}
