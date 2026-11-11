//! `render_area` and `page_size`: tiles of a page assemble into the page
//! rendering exactly, and invalid areas are refused.

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
    fixture("shapes.pdf")
}

fn fixture(name: &str) -> Document<MemorySource> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/public")
        .join(name);
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
/// the whole-page rendering: the same pixels exactly (same cells).
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
                    assert_eq!(
                        got,
                        want,
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
fn tiles_of_forms_and_groups_assemble_into_the_page() {
    // Clipped and nested forms, and a transparency group drawn through an
    // opacity layer.
    let document = fixture("forms.pdf");
    assert_tiles_match(&document, 0, 3.0, 256);
    assert_tiles_match(&document, 0, 1.37, 97);
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
    // An area off the cell grid counts the cells it touches: 1 x 300 pixels
    // at (10, 10) touch two cells of 256 x 256.
    let narrow = PixelRect {
        x: 10,
        y: 10,
        width: 1,
        height: 300,
    };
    options.limits.max_pixels = 2 * 256 * 256 - 1;
    assert_eq!(
        render_area(document.file(), page, &options, narrow, &mut budget()).err(),
        Some(Error::LimitExceeded {
            kind: RenderLimitKind::Pixels,
            limit: 2 * 256 * 256 - 1
        })
    );
    options.limits.max_pixels = 2 * 256 * 256;
    assert!(render_area(document.file(), page, &options, narrow, &mut budget()).is_ok());
    // At the page's side, cells are cut: the last column is 800 - 768 wide.
    let side = PixelRect {
        x: 790,
        y: 0,
        width: 10,
        height: 10,
    };
    options.limits.max_pixels = 32 * 256;
    assert!(render_area(document.file(), page, &options, side, &mut budget()).is_ok());
    options.limits.max_pixels = 32 * 256 - 1;
    assert!(render_area(document.file(), page, &options, side, &mut budget()).is_err());
    // `page_size` does not apply `max_pixels`.
    assert_eq!(page_size(page, &options), Ok((800, 800)));
}

/// A 64 x 64 page whose content is `content`.
fn page_with_content(content: &[u8]) -> Document<MemorySource> {
    let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    stream.extend_from_slice(content);
    stream.extend_from_slice(b"\nendstream");
    let bodies: [&[u8]; 4] = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 64 64] /Contents 4 0 R >>",
        &stream,
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
    out.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    Document::open(
        MemorySource::from_vec(out, &ResourceLimits::default()).expect("source"),
        &mut budget(),
    )
    .expect("document")
}

/// Found by the `render` fuzz target: a shear of 71 069 makes a sliver
/// millions of pixels long. The rasteriser keeps segments in single
/// precision, so before paths were clipped in double precision the tile
/// and the page placed its edge differently (a channel off by 34).
#[test]
fn far_geometry_renders_the_same_in_a_tile() {
    assert_fuzz_area_matches("far-shear-tile.bin");
}

/// Found by the `render` fuzz target: the same sheared square drawn twice.
/// The rasteriser's coverage depends on where an edge lies on its canvas,
/// so a tile and the page, each with its own origin, differed by 1 per
/// draw, and by 2 where two edges overlapped. Both now rasterise the same
/// cells from the same coordinates.
#[test]
fn overlapping_edges_render_the_same_in_a_tile() {
    assert_fuzz_area_matches("overlap-tile.bin");
}

/// Renders a `render` fuzz input as its target does, then the area the
/// target checks (off the cell grid): the same pixels exactly.
fn assert_fuzz_area_matches(name: &str) {
    let input = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name),
    )
    .expect("fuzz input");
    // As in the fuzz target: the first byte chooses the scale.
    let (scale, content) = input.split_first().expect("input");
    let scale = u32::from(scale % 8) + 1;
    let document = page_with_content(content);
    let page = document.page(0).expect("page");
    let options = options(f64::from(scale));
    let whole = render_page(document.file(), page, &options, &mut budget()).expect("page");
    let side = 64 * scale;
    let area = PixelRect {
        x: side / 3 + 1,
        y: side / 5 + 1,
        width: side / 2,
        height: side / 2,
    };
    let part = render_area(document.file(), page, &options, area, &mut budget()).expect("area");
    for y in 0..area.height {
        for x in 0..area.width {
            let got = part.bitmap.pixel(x, y).expect("area pixel");
            let want = whole
                .bitmap
                .pixel(area.x + x, area.y + y)
                .expect("page pixel");
            assert_eq!(got, want, "({x}, {y}) {got:?} != {want:?}");
        }
    }
}
