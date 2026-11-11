//! Measurement, not a check (ignored by default): the cost of rendering
//! first pages in viewer tiles (256 pixels, scale 2), without and with a
//! `RenderCache`. Files are read from `CASTLE_TILE_DIR` (at most
//! `CASTLE_TILE_FILES`, default 200), never from the repository.
//!
//!   CASTLE_TILE_DIR=/path/to/pdfs cargo test -p pdf-render --release \
//!     --test tile_cost -- --ignored --nocapture

#![allow(
    clippy::expect_used,
    clippy::print_stderr,
    reason = "measurement code: results are printed"
)]

use std::time::{Duration, Instant};

use pdf_document::Document;
use pdf_render::{PixelRect, RenderCache, RenderOptions, page_size, render_area_cached};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

/// Renders every 256-pixel tile of the first page; `None` if the file or
/// a tile fails. Without `shared`, each tile has a cache of its own, as
/// before `RenderCache` (reuse within one render only).
fn page_in_tiles(document: &Document<MemorySource>, shared: bool) -> Option<(Duration, usize)> {
    let page = document.page(0)?;
    let mut options = RenderOptions::default();
    options.scale = 2.0;
    let (width, height) = page_size(page, &options).ok()?;
    let mut cache = RenderCache::default();
    let start = Instant::now();
    let mut tiles = 0;
    for y in (0..height).step_by(256) {
        for x in (0..width).step_by(256) {
            let area = PixelRect {
                x,
                y,
                width: 256.min(width - x),
                height: 256.min(height - y),
            };
            if !shared {
                cache = RenderCache::default();
            }
            render_area_cached(
                document.file(),
                page,
                &options,
                area,
                &mut cache,
                &mut budget(),
            )
            .ok()?;
            tiles += 1;
        }
    }
    Some((start.elapsed(), tiles))
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted.get(index).copied().unwrap_or_default()
}

#[test]
#[ignore = "measurement on a local corpus: CASTLE_TILE_DIR"]
fn tile_cost_with_and_without_cache() {
    let Ok(dir) = std::env::var("CASTLE_TILE_DIR") else {
        eprintln!("CASTLE_TILE_DIR not set");
        return;
    };
    let limit: usize = std::env::var("CASTLE_TILE_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .expect("directory")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|e| e == "pdf"))
        .collect();
    paths.sort();
    let (mut without, mut with, mut tiles) = (Vec::new(), Vec::new(), 0);
    for path in paths.iter().take(limit) {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(source) = MemorySource::from_vec(bytes, &ResourceLimits::default()) else {
            continue;
        };
        let Ok(document) = Document::open(source, &mut budget()) else {
            continue;
        };
        // Same document, same order: without, then with.
        let (Some((a, count)), Some((b, _))) = (
            page_in_tiles(&document, false),
            page_in_tiles(&document, true),
        ) else {
            continue;
        };
        without.push(a);
        with.push(b);
        tiles += count;
    }
    without.sort();
    with.sort();
    let total = |v: &[Duration]| v.iter().sum::<Duration>();
    eprintln!("{} pages, {tiles} tiles at scale 2", without.len());
    for (name, times) in [("without cache", &without), ("with cache", &with)] {
        eprintln!(
            "{name}: page median {:?}, p95 {:?}, max {:?}, total {:?}",
            percentile(times, 0.5),
            percentile(times, 0.95),
            percentile(times, 1.0),
            total(times)
        );
    }
}
