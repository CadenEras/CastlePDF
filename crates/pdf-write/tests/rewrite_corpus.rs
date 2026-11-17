//! Measurement, not a check (ignored by default; ADR-0008, tranche 4.2):
//! every file of a local corpus rewritten unchanged. Counts the files
//! rewritten, read back by the strict reader with the same pages, and
//! whose first page renders the same as before, pixel for pixel; the size
//! of the output against the input. Then, for a document of two pages or
//! more, the last page left out (tranche 4.3, resources pruned): read back
//! strictly, and the first page rendered the same. Then each document is
//! merged after the previous one opened (tranche 4.4): read back strictly
//! with every page, and its first page rendered the same. Files are read from
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
use pdf_write::{PlanPage, SourcePage, identity_plan, merge, rewrite};

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
    let mut pruned_ratios = Vec::new();
    let mut kept_whole = 0;
    let mut previous: Option<Document<MemorySource>> = None;
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
        if let Some(before) = previous.take() {
            count(&merged_after(&before, &document));
        }
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
        if document.page_count() >= 2 {
            let plan = &plan[..plan.len() - 1];
            count(&last_page_left_out(
                &document,
                plan,
                limit,
                input,
                &mut pruned_ratios,
                &mut kept_whole,
            ));
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
        previous = Some(document);
    }
    ratios.sort_by(f64::total_cmp);
    pruned_ratios.sort_by(f64::total_cmp);
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
    eprintln!(
        "last page left out, size output/input: median {:.2}, p95 {:.2}, max {:.2}",
        at(&pruned_ratios, 0.5),
        at(&pruned_ratios, 0.95),
        at(&pruned_ratios, 1.0)
    );
    eprintln!("last page left out, some resources kept whole: {kept_whole} files");
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

/// The document without its last page: the outcome, as a count key.
fn last_page_left_out(
    document: &Document<MemorySource>,
    plan: &[PlanPage],
    limit: u64,
    input: u64,
    ratios: &mut Vec<f64>,
    kept_whole: &mut usize,
) -> String {
    let rewritten = match rewrite(document, plan, limit, &mut budget()) {
        Ok(rewritten) => rewritten,
        Err(e) => {
            return format!("last page left out: rewrite failed: {e:?}")
                .chars()
                .take(70)
                .collect();
        }
    };
    ratios.push(rewritten.bytes.len() as f64 / input.max(1) as f64);
    if !rewritten.resources_kept_whole.is_empty() {
        *kept_whole += 1;
    }
    let written = MemorySource::from_vec(rewritten.bytes, &ResourceLimits::default())
        .map_err(|e| format!("{e:?}"))
        .and_then(|source| {
            PdfFile::open_strict(source, &mut budget()).map_err(|e| format!("{e:?}"))
        })
        .and_then(|file| Document::from_file(file, &mut budget()).map_err(|e| format!("{e:?}")));
    let written = match written {
        Ok(written) if written.page_count() == plan.len() => written,
        Ok(_) => return "last page left out: page count differs".to_owned(),
        Err(e) => {
            return format!(
                "last page left out: strict read back failed: {}",
                e.chars().take(40).collect::<String>()
            );
        }
    };
    let (Some(a), Some(b)) = (document.page(0), written.page(0)) else {
        return "last page left out: no first page".to_owned();
    };
    let options = RenderOptions::default();
    match (
        render_page(document.file(), a, &options, &mut budget()),
        render_page(written.file(), b, &options, &mut budget()),
    ) {
        (Ok(before), Ok(after))
            if before.bitmap == after.bitmap && before.skipped == after.skipped =>
        {
            "last page left out: first page renders the same".to_owned()
        }
        (Ok(_), Ok(_)) => "last page left out: first page renders DIFFERENTLY".to_owned(),
        (Err(_), Err(_)) => "last page left out: first page fails before and after".to_owned(),
        (Ok(_), Err(_)) => "last page left out: first page fails only AFTER".to_owned(),
        (Err(_), Ok(_)) => "last page left out: first page fails only before".to_owned(),
    }
}

/// `second` merged after `first`: the outcome, as a count key.
fn merged_after(first: &Document<MemorySource>, second: &Document<MemorySource>) -> String {
    let sources = [first, second];
    let plan: Vec<SourcePage> = sources
        .iter()
        .enumerate()
        .flat_map(|(document, source)| {
            (0..source.page_count()).map(move |page| SourcePage {
                document,
                page,
                rotation: 0,
            })
        })
        .collect();
    let merged = match merge(&sources, &plan, 256 << 20, &mut budget()) {
        Ok(merged) => merged,
        Err(e) => {
            return format!("merged: failed: {e:?}").chars().take(60).collect();
        }
    };
    let written = MemorySource::from_vec(merged.bytes, &ResourceLimits::default())
        .map_err(|e| format!("{e:?}"))
        .and_then(|source| {
            PdfFile::open_strict(source, &mut budget()).map_err(|e| format!("{e:?}"))
        })
        .and_then(|file| Document::from_file(file, &mut budget()).map_err(|e| format!("{e:?}")));
    let written = match written {
        Ok(written) if written.page_count() == plan.len() => written,
        Ok(_) => return "merged: page count differs".to_owned(),
        Err(e) => {
            return format!(
                "merged: strict read back failed: {}",
                e.chars().take(40).collect::<String>()
            );
        }
    };
    let (Some(a), Some(b)) = (second.page(0), written.page(first.page_count())) else {
        return "merged: no first page".to_owned();
    };
    let options = RenderOptions::default();
    match (
        render_page(second.file(), a, &options, &mut budget()),
        render_page(written.file(), b, &options, &mut budget()),
    ) {
        (Ok(before), Ok(after))
            if before.bitmap == after.bitmap && before.skipped == after.skipped =>
        {
            "merged: second first page renders the same".to_owned()
        }
        (Ok(_), Ok(_)) => "merged: second first page renders DIFFERENTLY".to_owned(),
        (Err(_), Err(_)) => "merged: second first page fails before and after".to_owned(),
        (Ok(_), Err(_)) => "merged: second first page fails only AFTER".to_owned(),
        (Err(_), Ok(_)) => "merged: second first page fails only before".to_owned(),
    }
}
