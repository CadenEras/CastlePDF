//! `render_area` and `page_size`: tiles of a page assemble into the page
//! rendering (each channel within 1), and invalid areas are refused.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code: a failed expectation is a test failure"
)]

use std::path::PathBuf;

use pdf_document::Document;
use pdf_render::{
    Error, PixelRect, RenderLimitKind, RenderOptions, page_size, render_area, render_page,
};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

fn shapes() -> Document<MemorySource> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/public/shapes.pdf");
    let pdf = std::fs::read(path).expect("fixture");
    Document::open(
        MemorySource::from_vec(pdf, &ResourceLimits::default()).expect("source"),
        &mut budget(),
    )
    .expect("document")
}

fn options(scale: f64) -> RenderOptions {
    let mut options = RenderOptions::default();
    options.scale = scale;
    options
}

/// Renders the page in tiles of `tile` pixels and compares every pixel with
/// the whole-page rendering: each channel within 1, the rasteriser rounding
/// shifted coordinates differently.
fn assert_tiles_match(document: &Document<MemorySource>, index: usize, scale: f64, tile: u32) {
    let page = document.page(index).expect("page");
    let options = options(scale);
    let whole = render_page(document.file(), page, &options, &mut budget()).expect("page");
    let (width, height) = page_size(page, &options).expect("size");
    assert_eq!(
        (width, height),
        (whole.bitmap.width(), whole.bitmap.height())
    );
    let mut tiles = 0;
    for y in (0..height).step_by(tile as usize) {
        for x in (0..width).step_by(tile as usize) {
            let area = PixelRect {
                x,
                y,
                width: tile.min(width - x),
                height: tile.min(height - y),
            };
            let part =
                render_area(document.file(), page, &options, area, &mut budget()).expect("tile");
            assert_eq!(part.skipped, whole.skipped);
            assert_eq!(
                (part.bitmap.width(), part.bitmap.height()),
                (area.width, area.height)
            );
            for ty in 0..area.height {
                for tx in 0..area.width {
                    let (Some(got), Some(want)) = (
                        part.bitmap.pixel(tx, ty),
                        whole.bitmap.pixel(x + tx, y + ty),
                    ) else {
                        panic!("pixel outside a bitmap");
                    };
                    assert!(
                        got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 1),
                        "page {index} scale {scale} tile {tile}: pixel ({}, {}) {got:?} != {want:?}",
                        x + tx,
                        y + ty
                    );
                }
            }
            tiles += 1;
        }
    }
    assert!(tiles > 1, "the page spans several tiles");
}

#[test]
fn tiles_assemble_into_the_page() {
    let document = shapes();
    // Page 1: two content streams (200 x 200); page 2: rotated 90 degrees
    // (200 x 100); page 3: CMYK (20 x 20). The factor makes each page span
    // several tiles.
    for (index, factor) in [(0, 1.0), (1, 1.0), (2, 10.0)] {
        assert_tiles_match(&document, index, 3.0 * factor, 256);
        // Tiles not aligned on the rasteriser's 4-pixel grid, odd scale.
        assert_tiles_match(&document, index, 1.37 * factor, 97);
    }
}

#[test]
fn areas_must_lie_inside_the_page() {
    let document = shapes();
    let page = document.page(0).expect("page");
    let options = options(1.0);
    assert_eq!(page_size(page, &options), Ok((200, 200)));
    for area in [
        PixelRect {
            x: 0,
            y: 0,
            width: 0,
            height: 10,
        },
        PixelRect {
            x: 0,
            y: 0,
            width: 10,
            height: 0,
        },
        PixelRect {
            x: 191,
            y: 0,
            width: 10,
            height: 10,
        },
        PixelRect {
            x: 0,
            y: 200,
            width: 1,
            height: 1,
        },
        PixelRect {
            x: u32::MAX,
            y: 0,
            width: 2,
            height: 1,
        },
    ] {
        assert_eq!(
            render_area(document.file(), page, &options, area, &mut budget()).err(),
            Some(Error::InvalidRequest),
            "{area:?}"
        );
    }
    let corner = PixelRect {
        x: 190,
        y: 190,
        width: 10,
        height: 10,
    };
    assert!(render_area(document.file(), page, &options, corner, &mut budget()).is_ok());
}

#[test]
fn the_pixel_limit_applies_to_the_area() {
    let document = shapes();
    let page = document.page(0).expect("page");
    let mut options = options(4.0);
    options.limits.max_pixels = 256 * 256;
    // The page (800 x 800) is over the limit, a 256 x 256 tile is not.
    assert_eq!(
        render_page(document.file(), page, &options, &mut budget()).err(),
        Some(Error::LimitExceeded {
            kind: RenderLimitKind::Pixels,
            limit: 256 * 256
        })
    );
    let tile = PixelRect {
        x: 512,
        y: 256,
        width: 256,
        height: 256,
    };
    assert!(render_area(document.file(), page, &options, tile, &mut budget()).is_ok());
    let larger = PixelRect {
        x: 512,
        y: 256,
        width: 256,
        height: 257,
    };
    assert_eq!(
        render_area(document.file(), page, &options, larger, &mut budget()).err(),
        Some(Error::LimitExceeded {
            kind: RenderLimitKind::Pixels,
            limit: 256 * 256
        })
    );
    // `page_size` does not apply `max_pixels`.
    assert_eq!(page_size(page, &options), Ok((800, 800)));
}
