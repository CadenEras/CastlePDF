//! Measurement, not a check (ignored by default; ADR-0008, tranche 4.2):
//! every file of a local corpus rewritten unchanged. Counts the files
//! rewritten, read back by the strict reader with the same pages, and
//! whose first page renders the same as before, pixel for pixel; the size
//! of the output against the input. Files are read from
//! `CASTLE_REWRITE_DIR` (recursively), never from the repository; only
//! counts are printed.
//!
//!   CASTLE_REWRITE_DIR=/path/to/pdfs cargo test -p pdf-write --release \
//!     --test rewrite_corpus -- --ignored --nocapture

#![allow(
    clippy::print_stderr,
    clippy::indexing_slicing,
    reason = "measurement code: results are printed"
)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use pdf_document::Document;
use pdf_render::{RenderOptions, render_page};
use pdf_syntax::{Budget, CancellationToken, MemorySource, PdfFile, ResourceLimits};
use pdf_write::{identity_plan, rewrite};

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

#[test]
#[ignore = "measurement on a local corpus: CASTLE_REWRITE_DIR"]
fn rewrite_every_file_of_a_corpus() {
    let Ok(dir) = std::env::var("CASTLE_REWRITE_DIR") else {
        eprintln!("CASTLE_REWRITE_DIR not set");
        return;
    };
    let mut paths = Vec::new();
    let mut pending = vec![std::path::PathBuf::from(dir)];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                paths.push(path);
            }
        }
    }
    paths.sort();
    let mut outcomes: BTreeMap<String, usize> = BTreeMap::new();
    let mut ratios = Vec::new();
    let mut times: Vec<Duration> = Vec::new();
    let mut count = |key: &str| *outcomes.entry(key.to_owned()).or_default() += 1;
    for path in &paths {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let input = bytes.len() as u64;
        let Ok(source) = MemorySource::from_vec(bytes, &ResourceLimits::default()) else {
            continue;
        };
        let Ok(document) = Document::open(source, &mut budget()) else {
            continue;
        };
        count("opened");
        let start = Instant::now();
        let plan = identity_plan(&document);
        let limit = input.saturating_mul(4).max(16 << 20);
        let rewritten = match rewrite(&document, &plan, limit, &mut budget()) {
            Ok(rewritten) => rewritten,
            Err(e) => {
                count(
                    &format!("rewrite failed: {e:?}")
                        .chars()
                        .take(60)
                        .collect::<String>(),
                );
                continue;
            }
        };
        times.push(start.elapsed());
        count("rewritten");
        ratios.push(rewritten.bytes.len() as f64 / input.max(1) as f64);
        for dropped in &rewritten.dropped {
            count(&format!("not kept: {dropped}"));
        }
        let Ok(source) = MemorySource::from_vec(rewritten.bytes, &ResourceLimits::default()) else {
            count("output too large to read back");
            continue;
        };
        let written = PdfFile::open_strict(source, &mut budget())
            .map_err(|e| format!("{e:?}"))
            .and_then(|file| {
                Document::from_file(file, &mut budget()).map_err(|e| format!("{e:?}"))
            });
        let written = match written {
            Ok(written) => written,
            Err(e) => {
                count(&format!(
                    "strict read back failed: {}",
                    e.chars().take(40).collect::<String>()
                ));
                continue;
            }
        };
        if written.page_count() != document.page_count() {
            count("page count differs");
            continue;
        }
        count("read back strictly, same pages");
        let (Some(a), Some(b)) = (document.page(0), written.page(0)) else {
            continue;
        };
        let options = RenderOptions::default();
        match (
            render_page(document.file(), a, &options, &mut budget()),
            render_page(written.file(), b, &options, &mut budget()),
        ) {
            (Ok(before), Ok(after))
                if before.bitmap == after.bitmap && before.skipped == after.skipped =>
            {
                count("first page renders the same");
            }
            (Ok(_), Ok(_)) => count("first page renders DIFFERENTLY"),
            (Err(_), Err(_)) => count("first page fails before and after"),
            (Ok(_), Err(_)) => count("first page fails only AFTER"),
            (Err(_), Ok(_)) => count("first page fails only before"),
        }
    }
    ratios.sort_by(f64::total_cmp);
    times.sort();
    let at = |v: &[f64], p: f64| {
        v.get(((v.len() as f64 - 1.0) * p).round() as usize)
            .copied()
            .unwrap_or(0.0)
    };
    eprintln!("{} files", paths.len());
    for (key, value) in &outcomes {
        eprintln!("  {value:6}  {key}");
    }
    eprintln!(
        "size output/input: median {:.2}, p95 {:.2}, max {:.2}",
        at(&ratios, 0.5),
        at(&ratios, 0.95),
        at(&ratios, 1.0)
    );
    let t = |p: f64| {
        times
            .get(((times.len() as f64 - 1.0) * p).round() as usize)
            .copied()
            .unwrap_or_default()
    };
    eprintln!(
        "rewrite time: median {:?}, p95 {:?}, max {:?}",
        t(0.5),
        t(0.95),
        t(1.0)
    );
}
