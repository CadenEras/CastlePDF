//! Measurement, not a check (ignored by default): the cost of rendering
//! first pages in viewer tiles (256 pixels, scale 2), without and with a
//! `RenderCache`, against each page rendered whole; for the slowest pages
//! in tiles, also one pass over the content alone (what every tile pays
//! before drawing its cells). Files are read from `CASTLE_TILE_DIR` (at most
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
use pdf_render::{
    PixelRect, RenderCache, RenderOptions, page_size, render_area_cached, render_page_cached,
};
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

/// The first page rendered whole at scale 2 with a cache of its own, as a
/// reference for its tiles: what tiles cost beyond it is the content
/// interpreted again for each tile.
fn page_whole(document: &Document<MemorySource>) -> Option<Duration> {
    let page = document.page(0)?;
    let mut options = RenderOptions::default();
    options.scale = 2.0;
    let mut cache = RenderCache::default();
    let start = Instant::now();
    render_page_cached(document.file(), page, &options, &mut cache, &mut budget()).ok()?;
    Some(start.elapsed())
}

/// One pass over the content with almost nothing to draw: a 1-pixel area
/// in the top-left corner, with a warm cache, as a median of 9 passes.
/// What each tile pays before rasterising its own cells.
fn page_pass(document: &Document<MemorySource>) -> Option<Duration> {
    let page = document.page(0)?;
    let mut options = RenderOptions::default();
    options.scale = 2.0;
    let mut cache = RenderCache::default();
    let corner = PixelRect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    let mut times = Vec::new();
    for _ in 0..10 {
        let start = Instant::now();
        render_area_cached(
            document.file(),
            page,
            &options,
            corner,
            &mut cache,
            &mut budget(),
        )
        .ok()?;
        times.push(start.elapsed());
    }
    // The first pass fills the cache.
    let mut warm = times.split_off(1);
    warm.sort();
    Some(percentile(&warm, 0.5))
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
    // (tiles with cache, whole page, tile count, file name)
    let mut pages = Vec::new();
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
        let (Some((a, count)), Some((b, _)), Some(whole)) = (
            page_in_tiles(&document, false),
            page_in_tiles(&document, true),
            page_whole(&document),
        ) else {
            continue;
        };
        without.push(a);
        with.push(b);
        tiles += count;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        pages.push((b, whole, count, name.unwrap_or_default()));
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
    let wholes: Vec<Duration> = {
        let mut v: Vec<Duration> = pages.iter().map(|p| p.1).collect();
        v.sort();
        v
    };
    eprintln!(
        "whole page: median {:?}, p95 {:?}, max {:?}, total {:?}",
        percentile(&wholes, 0.5),
        percentile(&wholes, 0.95),
        percentile(&wholes, 1.0),
        total(&wholes)
    );
    // The slowest pages in tiles (with cache), against one whole render.
    pages.sort_by_key(|a| std::cmp::Reverse(a.0));
    let dir = std::env::var("CASTLE_TILE_DIR").unwrap_or_default();
    for (tiled, whole, count, name) in pages.iter().take(8) {
        let pass = std::fs::read(std::path::Path::new(&dir).join(name))
            .ok()
            .and_then(|bytes| MemorySource::from_vec(bytes, &ResourceLimits::default()).ok())
            .and_then(|source| Document::open(source, &mut budget()).ok())
            .and_then(|document| page_pass(&document));
        eprintln!(
            "  {tiled:?} in {count} tiles, whole {whole:?} (x{:.1}), one pass {pass:?}: {name}",
            tiled.as_secs_f64() / whole.as_secs_f64().max(1e-9)
        );
    }
}
