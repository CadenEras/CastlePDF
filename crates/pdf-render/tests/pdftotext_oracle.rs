//! Differential test of text extraction against `pdftotext` (Poppler),
//! ADR-0006 tranche 3.3. Run by the `oracle` workflow with
//! `CASTLE_PDFTOTEXT` set to the executable; skipped otherwise.
//!
//! Compared: the non-white-space characters of each page, in content order
//! (`pdftotext -raw`). White space and line breaks are layout, compared from
//! tranche 3.5. Only pages whose every glyph has a known text are compared:
//! where we report a glyph unknown, Poppler prints something else on
//! purpose, as documented below.
//!
//! Known, deliberate differences (not compared here):
//! - a Type 0 font without `/ToUnicode` (`truetype.pdf` and `cff.pdf` line
//!   3, `text.pdf` page 2): Poppler prints the raw CIDs as characters, often
//!   control characters; we report the glyphs unknown;
//! - a damaged `/ToUnicode` (`text.pdf` page 2): Poppler keeps the entries
//!   before the damage; we do not use it and fall back to glyph names;
//! - a Type 3 glyph name outside the Adobe Glyph List (`type3.pdf`, `/tri`):
//!   Poppler prints the code as a character (`A`); we report it unknown.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test code: a failed expectation is a test failure"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use pdf_document::Document;
use pdf_render::{RenderLimits, UnicodeSource, page_text};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

/// Fixture and page index, every glyph's text known.
const CASES: [(&str, usize); 11] = [
    ("minimal.pdf", 0),
    ("incremental.pdf", 0),
    ("hybrid.pdf", 0),
    ("objstm.pdf", 0),
    ("flate.pdf", 0),
    ("page-tree.pdf", 0),
    ("page-tree.pdf", 2),
    ("text.pdf", 0),
    ("cmaps.pdf", 0),
    ("cmaps.pdf", 1),
    ("filters.pdf", 0),
];

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/public")
        .join(name)
}

fn without_space(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

fn ours(name: &str, page_index: usize) -> String {
    let bytes = std::fs::read(corpus(name)).expect("fixture");
    let source = MemorySource::from_vec(bytes, &ResourceLimits::default()).expect("source");
    let mut budget = Budget::new(ResourceLimits::default(), CancellationToken::new());
    let document = Document::open(source, &mut budget).expect("document");
    let page = document.page(page_index).expect("page");
    let text =
        page_text(document.file(), page, &RenderLimits::default(), &mut budget).expect("text");
    assert!(
        text.glyphs
            .iter()
            .all(|glyph| glyph.source != UnicodeSource::Unknown),
        "{name} page {}: a glyph is unknown; compare known pages only",
        page_index + 1
    );
    without_space(
        &text
            .glyphs
            .iter()
            .filter_map(|glyph| glyph.text.as_deref())
            .collect::<String>(),
    )
}

fn theirs(executable: &Path, name: &str, page_index: usize) -> String {
    let page = (page_index + 1).to_string();
    let output = Command::new(executable)
        .args(["-raw", "-enc", "UTF-8", "-f", &page, "-l", &page])
        .arg(corpus(name))
        .arg("-")
        .output()
        .expect("pdftotext runs");
    assert!(output.status.success(), "pdftotext failed on {name}");
    without_space(&String::from_utf8(output.stdout).expect("UTF-8 output"))
}

#[test]
fn text_matches_pdftotext() {
    let Some(executable) = std::env::var_os("CASTLE_PDFTOTEXT") else {
        eprintln!("pdftotext oracle skipped: CASTLE_PDFTOTEXT is not set");
        return;
    };
    let executable = PathBuf::from(executable);
    for (name, page_index) in CASES {
        let (ours, theirs) = (
            ours(name, page_index),
            theirs(&executable, name, page_index),
        );
        eprintln!(
            "{name} page {}: {} characters",
            page_index + 1,
            ours.chars().count()
        );
        assert_eq!(ours, theirs, "{name} page {}", page_index + 1);
    }
    eprintln!("fixtures: text matches pdftotext");
}
