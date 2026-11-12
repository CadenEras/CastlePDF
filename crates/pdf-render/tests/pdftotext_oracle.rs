//! Differential test of text extraction against `pdftotext` (Poppler),
//! ADR-0006 tranches 3.3 and 3.5a. Run by the `oracle` workflow with
//! `CASTLE_PDFTOTEXT` set to the executable; skipped otherwise.
//!
//! Compared, in content order (`pdftotext -raw`): the non-white-space
//! characters of each page (tranche 3.3), then its words, white space
//! separated (tranche 3.5a). Line breaks are not compared: Poppler and we
//! cut lines differently where text turns or jumps. Only pages whose every
//! glyph has a known text are compared: where we report a glyph unknown,
//! Poppler prints something else on purpose, as documented below.
//!
//! Known, deliberate differences (not compared here):
//! - a Type 0 font without `/ToUnicode` (`truetype.pdf` and `cff.pdf` line
//!   3, `text.pdf` page 2): Poppler prints the raw CIDs as characters, often
//!   control characters; we report the glyphs unknown;
//! - a damaged `/ToUnicode` (`text.pdf` page 2): Poppler keeps the entries
//!   before the damage; we do not use it and fall back to glyph names;
//! - a Type 3 glyph name outside the Adobe Glyph List (`type3.pdf`, `/tri`):
//!   Poppler prints the code as a character (`A`); we report it unknown;
//! - `/ActualText` (`layout.pdf`, ISO 32000-2, 14.9.4): Poppler ignores a
//!   span whose properties are a `/Properties` resource and a span without
//!   glyphs, and keeps the inner of two nested spans; we apply both kinds of
//!   span and the outer one replaces everything inside it;
//! - vertical writing (`cmaps.pdf` page 1): Poppler prints one glyph per
//!   line, we keep the column of glyphs as one word; its words are not
//!   compared, its characters are.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test code: a failed expectation is a test failure"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use pdf_document::Document;
use pdf_render::{RenderLimits, UnicodeSource, page_text, text_lines};
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};

/// Fixture, page index, every glyph's text known, and whether its words
/// are compared.
const CASES: [(&str, usize, bool); 11] = [
    ("minimal.pdf", 0, true),
    ("incremental.pdf", 0, true),
    ("hybrid.pdf", 0, true),
    ("objstm.pdf", 0, true),
    ("flate.pdf", 0, true),
    ("page-tree.pdf", 0, true),
    ("page-tree.pdf", 2, true),
    ("text.pdf", 0, true),
    ("cmaps.pdf", 0, false),
    ("cmaps.pdf", 1, true),
    ("filters.pdf", 0, true),
];

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/public")
        .join(name)
}

fn without_space(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Our characters (white space removed) and words of a page.
fn ours(name: &str, page_index: usize) -> (String, Vec<String>) {
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
    let characters = without_space(
        &text
            .glyphs
            .iter()
            .filter_map(|glyph| glyph.text.as_deref())
            .collect::<String>(),
    );
    let words = text_lines(&text)
        .into_iter()
        .flat_map(|line| line.words.into_iter().map(|word| word.text))
        .collect();
    (characters, words)
}

/// Poppler's characters (white space removed) and words of a page.
fn theirs(executable: &Path, name: &str, page_index: usize) -> (String, Vec<String>) {
    let page = (page_index + 1).to_string();
    let output = Command::new(executable)
        .args(["-raw", "-enc", "UTF-8", "-f", &page, "-l", &page])
        .arg(corpus(name))
        .arg("-")
        .output()
        .expect("pdftotext runs");
    assert!(output.status.success(), "pdftotext failed on {name}");
    let text = String::from_utf8(output.stdout).expect("UTF-8 output");
    let words = text.split_whitespace().map(str::to_owned).collect();
    (without_space(&text), words)
}

#[test]
fn text_matches_pdftotext() {
    let Some(executable) = std::env::var_os("CASTLE_PDFTOTEXT") else {
        eprintln!("pdftotext oracle skipped: CASTLE_PDFTOTEXT is not set");
        return;
    };
    let executable = PathBuf::from(executable);
    for (name, page_index, compare_words) in CASES {
        let (our_characters, our_words) = ours(name, page_index);
        let (their_characters, their_words) = theirs(&executable, name, page_index);
        eprintln!(
            "{name} page {}: {} characters, {} words",
            page_index + 1,
            our_characters.chars().count(),
            our_words.len()
        );
        assert_eq!(
            our_characters,
            their_characters,
            "{name} page {}",
            page_index + 1
        );
        if compare_words {
            assert_eq!(
                our_words,
                their_words,
                "{name} page {} words",
                page_index + 1
            );
        }
    }
    eprintln!("fixtures: text matches pdftotext");
}
