//! Annotations (ISO 32000-2, 12.5), not drawn yet (P5): tranche 3.13
//! reports a page that has one other readers show, so that it is not taken
//! for complete.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use pdf_document::Document;
use pdf_render::{RenderCache, RenderOptions, Unsupported, render_area_cached, render_page};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

/// A 100 x 100 page with `annots` as its `/Annots` value (absent when
/// empty) and a black square; `more` objects from 5.
fn page(annots: &str, more: &[&str]) -> Document<MemorySource> {
    let content = "0 g 10 10 80 80 re f";
    let annots = if annots.is_empty() {
        String::new()
    } else {
        format!(" /Annots {annots}")
    };
    let mut bodies = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R{annots} >>"),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
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
    let source = MemorySource::from_vec(out, &ResourceLimits::default()).expect("source");
    Document::open(source, &mut budget()).expect("document")
}

fn skipped(document: &Document<MemorySource>) -> Vec<Unsupported> {
    let page = document.page(0).expect("page");
    let rendering = render_page(
        document.file(),
        page,
        &RenderOptions::default(),
        &mut budget(),
    )
    .expect("render");
    // The page itself is drawn either way.
    assert_eq!(rendering.bitmap.pixel(50, 50).map(|p| p[0]), Some(0));
    rendering.skipped.iter().copied().collect()
}

/// An appearance stream: a form XObject (object 5 in `page`).
const APPEARANCE: &str =
    "<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Length 0 >>\nstream\n\nendstream";

#[test]
fn annotations_shown_by_other_readers_are_reported() {
    for annots in [
        // An appearance stream: a stamp, a highlight, a note's icon...
        "[<< /Type /Annot /Subtype /Square /Rect [0 0 10 10] /AP << /N 5 0 R >> >>]",
        // A form field, whose appearance other readers may generate.
        "[<< /Type /Annot /Subtype /Widget /Rect [0 0 10 10] /FT /Tx /V (Name) >>]",
        // Among others, through references, with the array itself indirect.
        "6 0 R",
    ] {
        let more = [
            APPEARANCE,
            "[7 0 R 8 0 R]",
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] >>",
            "<< /Type /Annot /Subtype /Text /Rect [0 0 10 10] /AP << /N 5 0 R >> >>",
        ];
        assert_eq!(
            skipped(&page(annots, &more)),
            vec![Unsupported::Annotation],
            "{annots}"
        );
    }
}

#[test]
fn annotations_that_show_nothing_are_not_reported() {
    for annots in [
        "",
        "[]",
        // A link has no appearance: it is an area to click, not drawn.
        "[<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Border [0 0 0] >>]",
        // A pop-up is shown on demand.
        "[<< /Type /Annot /Subtype /Popup /Rect [0 0 10 10] /AP << /N 5 0 R >> >>]",
        // Hidden (flag 2) or not for display (NoView, flag 32).
        "[<< /Type /Annot /Subtype /Square /F 2 /Rect [0 0 10 10] /AP << /N 5 0 R >> >>]",
        "[<< /Type /Annot /Subtype /Widget /F 34 /Rect [0 0 10 10] >>]",
        // Damaged: not an array, not a dictionary, a missing object, a
        // reference to a stream, an object whose syntax is broken. The page
        // is rendered all the same.
        "(not an array)",
        "[42 /Name 9 0 R 5 0 R 6 0 R]",
    ] {
        let damaged = "<< /Type /Annot /Subtype /Square /AP << /N 5 0 R >>";
        assert!(
            skipped(&page(annots, &[APPEARANCE, damaged])).is_empty(),
            "{annots}"
        );
    }
}

#[test]
fn annots_beyond_the_bound_are_reported_without_being_read() {
    // At most 4 096 entries are read: 4 096 links show nothing; with one
    // more, the page is reported, which cannot be told without reading.
    let link = "<< /Subtype /Link /Rect [0 0 1 1] >> ";
    for (count, expected) in [(4096, vec![]), (4097, vec![Unsupported::Annotation])] {
        let annots = format!("[{}]", link.repeat(count));
        assert_eq!(skipped(&page(&annots, &[])), expected, "{count}");
    }
}

#[test]
fn every_tile_reports_the_annotations_of_its_page() {
    // Tiles of one page share the document's render cache, where the
    // answer is kept: each tile reports it, the first and the next ones.
    let document = page(
        "[<< /Type /Annot /Subtype /Widget /Rect [0 0 10 10] >>]",
        &[],
    );
    let page = document.page(0).expect("page");
    let mut cache = RenderCache::default();
    for x in [0, 50] {
        let area = pdf_render::PixelRect {
            x,
            y: 0,
            width: 50,
            height: 100,
        };
        let rendering = render_area_cached(
            document.file(),
            page,
            &RenderOptions::default(),
            area,
            &mut cache,
            &mut budget(),
        )
        .expect("render");
        assert_eq!(
            rendering.skipped.iter().copied().collect::<Vec<_>>(),
            vec![Unsupported::Annotation]
        );
    }
}
