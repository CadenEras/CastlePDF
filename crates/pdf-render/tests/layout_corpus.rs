//! Words of first pages against `pdftotext` on a local corpus (ADR-0006,
//! tranches 3.5a and 3.5b): ignored, run by hand with `CASTLE_CORPUS` (a
//! directory of PDF files, never in Git) and `CASTLE_PDFTOTEXT` set:
//!
//! ```sh
//! CASTLE_CORPUS=~/corpus/safe CASTLE_PDFTOTEXT=$(command -v pdftotext) \
//!   cargo test --release -p pdf-render --test layout_corpus -- --ignored --nocapture
//! ```
//!
//! For each file whose first page both sides read, against the words of
//! `pdftotext` (default mode: its reading order), white space separated:
//! - words as bags, order aside: F1 = 2 |common| / (|ours| + |theirs|);
//! - order: 2 LCS / (|ours| + |theirs|), LCS the longest common subsequence,
//!   for our content order (`text_lines`) and our reading order
//!   (`reading_lines`), on pages of at most `MAX_ORDER_WORDS` words a side.
//!
//! Only aggregates are printed, never words or file names.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use pdf_document::Document;
use pdf_render::{RenderLimits, TextLine, page_text, reading_lines, text_lines};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

/// Pages with more words a side are left out of the order measure (its
/// cost is the product of both counts).
const MAX_ORDER_WORDS: usize = 5000;

fn files(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("directory") {
            let entry = entry.expect("entry");
            let kind = entry.file_type().expect("type");
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    files
}

fn words(lines: Vec<TextLine>) -> Vec<String> {
    lines
        .into_iter()
        .flat_map(|line| line.words.into_iter())
        .flat_map(|word| {
            word.text
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Our words of the first page in content order and in reading order, and
/// how many `/ActualText` spans it has.
struct Ours {
    content: Vec<String>,
    reading: Vec<String>,
    spans: usize,
}

fn ours(path: &Path) -> Option<Ours> {
    let limits = ResourceLimits::default();
    let bytes = std::fs::read(path).ok()?;
    let source = MemorySource::from_vec(bytes, &limits).ok()?;
    let mut budget = Budget::new(limits, CancellationToken::new());
    let document = Document::open(source, &mut budget).ok()?;
    let page = document.page(0)?;
    let mut budget = Budget::new(limits, CancellationToken::new());
    let text = page_text(document.file(), page, &RenderLimits::default(), &mut budget).ok()?;
    Some(Ours {
        content: words(text_lines(&text)),
        reading: words(reading_lines(&text)),
        spans: text.actual_texts.len(),
    })
}

fn theirs(executable: &Path, path: &Path) -> Option<Vec<String>> {
    let output = Command::new(executable)
        .args(["-enc", "UTF-8", "-f", "1", "-l", "1"])
        .arg(path)
        .arg("-")
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    })
}

fn f1(ours: &[String], theirs: &[String]) -> f64 {
    let mut counts: HashMap<&str, i64> = HashMap::new();
    for word in theirs {
        *counts.entry(word.as_str()).or_default() += 1;
    }
    let mut common = 0;
    for word in ours {
        if let Some(count) = counts.get_mut(word.as_str())
            && *count > 0
        {
            *count -= 1;
            common += 1;
        }
    }
    2.0 * f64::from(common) / (ours.len() + theirs.len()) as f64
}

/// 2 LCS / (|a| + |b|), with two rows of dynamic programming.
fn order(a: &[String], b: &[String]) -> f64 {
    let mut previous = vec![0u32; b.len() + 1];
    let mut current = vec![0u32; b.len() + 1];
    for word in a {
        for (j, other) in b.iter().enumerate() {
            current[j + 1] = if word == other {
                previous[j] + 1
            } else {
                current[j].max(previous[j + 1])
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    2.0 * f64::from(previous[b.len()]) / (a.len() + b.len()) as f64
}

/// Mean, p10, median and the shares at or above 0.99 and 0.9.
fn summary(label: &str, mut scores: Vec<f64>) {
    scores.sort_by(f64::total_cmp);
    let at = |share: f64| {
        let index = ((scores.len() as f64 - 1.0) * share).round() as usize;
        scores.get(index).copied().unwrap_or(f64::NAN)
    };
    let mean = scores.iter().sum::<f64>() / scores.len() as f64;
    let share = |threshold: f64| {
        scores.iter().filter(|s| **s >= threshold).count() as f64 * 100.0 / scores.len() as f64
    };
    eprintln!(
        "{label} ({} pages): mean {mean:.3}, p10 {:.3}, median {:.3}; >= 0.99: {:.1}%, >= 0.9: {:.1}%",
        scores.len(),
        at(0.1),
        at(0.5),
        share(0.99),
        share(0.9)
    );
}

#[test]
#[ignore = "needs a local corpus and pdftotext"]
fn first_page_words_against_pdftotext() {
    let (Some(corpus), Some(executable)) = (
        std::env::var_os("CASTLE_CORPUS"),
        std::env::var_os("CASTLE_PDFTOTEXT"),
    ) else {
        panic!("set CASTLE_CORPUS and CASTLE_PDFTOTEXT");
    };
    let executable = PathBuf::from(executable);
    let (mut bags, mut content_order, mut reading_order) = (Vec::new(), Vec::new(), Vec::new());
    let (mut only_ours, mut only_theirs, mut with_actual_text) = (0, 0, 0);
    for path in files(Path::new(&corpus)) {
        let (Some(ours), Some(theirs)) = (ours(&path), theirs(&executable, &path)) else {
            continue;
        };
        with_actual_text += usize::from(ours.spans > 0);
        match (ours.reading.is_empty(), theirs.is_empty()) {
            (true, true) => {}
            (false, true) => only_ours += 1,
            (true, false) => only_theirs += 1,
            (false, false) => {
                bags.push(f1(&ours.reading, &theirs));
                if ours.reading.len().max(theirs.len()) <= MAX_ORDER_WORDS {
                    content_order.push(order(&ours.content, &theirs));
                    reading_order.push(order(&ours.reading, &theirs));
                }
            }
        }
    }
    eprintln!(
        "text only ours: {only_ours}; text only pdftotext: {only_theirs}; pages with /ActualText: {with_actual_text}"
    );
    summary("word F1", bags);
    summary("order, content", content_order);
    summary("order, reading", reading_order);
}
