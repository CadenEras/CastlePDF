//! `RenderCache`: what renders of one document reuse. Results are the same
//! with or without it; it saves reading and decoding; it stays bounded and
//! usable after a failed render.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test code: a failed expectation is a test failure"
)]

use std::path::PathBuf;

use pdf_document::Document;
use pdf_render::{
    Error, PixelRect, RenderCache, RenderLimitKind, RenderOptions, Rendering, page_size,
    render_area, render_area_cached, render_page, render_page_cached,
};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

fn fixture(name: &str) -> Document<MemorySource> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/public")
        .join(name);
    let bytes = std::fs::read(path).expect("fixture");
    let source = MemorySource::from_vec(bytes, &ResourceLimits::default()).expect("source");
    Document::open(source, &mut budget()).expect("document")
}

fn same(a: &Rendering, b: &Rendering, what: &str) {
    assert_eq!(a.skipped, b.skipped, "{what}");
    assert!(a.bitmap.data() == b.bitmap.data(), "{what}: pixels differ");
}

#[test]
fn renders_are_the_same_with_a_cache() {
    for name in [
        "truetype.pdf",
        "cff.pdf",
        "images.pdf",
        "jpeg.pdf",
        "inline-images.pdf",
        "forms.pdf",
        "type3.pdf",
        "minimal.pdf",
    ] {
        let document = fixture(name);
        let mut cache = RenderCache::default();
        let mut options = RenderOptions::default();
        options.scale = 1.7;
        // Pages in both orders, twice, whole and in tiles, one cache.
        let count = document.page_count();
        let order: Vec<usize> = (0..count).chain((0..count).rev()).collect();
        for &index in &order {
            let page = document.page(index).expect("page");
            let plain = render_page(document.file(), page, &options, &mut budget()).expect("page");
            let cached =
                render_page_cached(document.file(), page, &options, &mut cache, &mut budget())
                    .expect("cached page");
            same(&plain, &cached, &format!("{name} page {index}"));
            let (width, height) = page_size(page, &options).expect("size");
            let area = PixelRect {
                x: width / 3,
                y: height / 4,
                width: (width / 2).max(1),
                height: (height / 2).max(1),
            };
            let plain =
                render_area(document.file(), page, &options, area, &mut budget()).expect("area");
            let cached = render_area_cached(
                document.file(),
                page,
                &options,
                area,
                &mut cache,
                &mut budget(),
            )
            .expect("cached area");
            same(&plain, &cached, &format!("{name} page {index} area"));
        }
        assert!(cache.bytes() > 0, "{name}: nothing kept");
    }
}

#[test]
fn a_cache_saves_reading_and_decoding() {
    // The image fixture: decoded once, then reused. What the second render
    // still counts is the content it interprets (strings are charged as
    // they are read).
    let document = fixture("images.pdf");
    let page = document.page(0).expect("page");
    let options = RenderOptions::default();
    let mut cache = RenderCache::default();
    let mut first = budget();
    render_page_cached(document.file(), page, &options, &mut cache, &mut first).expect("first");
    let mut second = budget();
    render_page_cached(document.file(), page, &options, &mut cache, &mut second).expect("second");
    assert!(
        second.decoded_bytes() * 2 < first.decoded_bytes(),
        "{} then {}",
        first.decoded_bytes(),
        second.decoded_bytes()
    );
    assert!(second.objects_visited() < first.objects_visited());
    // Fonts and glyph outlines too.
    let document = fixture("truetype.pdf");
    let page = document.page(0).expect("page");
    let mut cache = RenderCache::default();
    let mut first = budget();
    render_page_cached(document.file(), page, &options, &mut cache, &mut first).expect("first");
    let mut second = budget();
    render_page_cached(document.file(), page, &options, &mut cache, &mut second).expect("second");
    assert!(second.decoded_bytes() * 2 < first.decoded_bytes());
}

#[test]
fn a_cache_stays_within_its_bound() {
    let document = fixture("images.pdf");
    let options = RenderOptions::default();
    // Nothing kept by an empty bound, and the result is the same.
    let mut none = RenderCache::new(0);
    for index in 0..2 {
        let page = document.page(index).expect("page");
        let plain = render_page(document.file(), page, &options, &mut budget()).expect("page");
        let cached = render_page_cached(document.file(), page, &options, &mut none, &mut budget())
            .expect("cached");
        same(&plain, &cached, "empty bound");
        assert_eq!(none.bytes(), 0);
    }
    // A small bound is never exceeded; a render that starts more than
    // three quarters full empties it first.
    let mut small = RenderCache::new(4096);
    for index in [0, 1, 0, 1] {
        let page = document.page(index).expect("page");
        render_page_cached(document.file(), page, &options, &mut small, &mut budget())
            .expect("cached");
        assert!(small.bytes() <= 4096, "{}", small.bytes());
    }
    small.clear();
    assert_eq!(small.bytes(), 0);
}

#[test]
fn other_limits_empty_the_cache() {
    // An image refused under a small max_image_pixels is drawn under the
    // default one: what was kept does not leak from one to the other.
    let document = fixture("images.pdf");
    let page = document.page(0).expect("page");
    let mut cache = RenderCache::default();
    let mut tight = RenderOptions::default();
    tight.limits.max_image_pixels = 1;
    let refused = render_page_cached(document.file(), page, &tight, &mut cache, &mut budget())
        .expect("tight");
    assert!(!refused.skipped.is_empty());
    let normal = RenderOptions::default();
    let drawn = render_page_cached(document.file(), page, &normal, &mut cache, &mut budget())
        .expect("normal");
    let plain = render_page(document.file(), page, &normal, &mut budget()).expect("plain");
    same(&plain, &drawn, "after other limits");
}

#[test]
fn a_cache_stays_usable_after_a_failed_render() {
    let document = fixture("forms.pdf");
    let page = document.page(0).expect("page");
    let mut cache = RenderCache::default();
    let mut tight = RenderOptions::default();
    tight.limits.max_operations = 5;
    assert!(matches!(
        render_page_cached(document.file(), page, &tight, &mut cache, &mut budget()),
        Err(Error::LimitExceeded {
            kind: RenderLimitKind::Operations,
            ..
        })
    ));
    let normal = RenderOptions::default();
    let cached = render_page_cached(document.file(), page, &normal, &mut cache, &mut budget())
        .expect("after failure");
    let plain = render_page(document.file(), page, &normal, &mut budget()).expect("plain");
    same(&plain, &cached, "after a failed render");
    // A cancelled render too.
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut cancelled = Budget::new(ResourceLimits::default(), cancel);
    assert!(
        render_page_cached(document.file(), page, &normal, &mut cache, &mut cancelled).is_err()
    );
    let again = render_page_cached(document.file(), page, &normal, &mut cache, &mut budget())
        .expect("after cancellation");
    same(&plain, &again, "after a cancelled render");
}
