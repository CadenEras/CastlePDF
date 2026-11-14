//! Runs `castle-pdf search` (ADR-0006, tranche 3.7).
#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is a test failure"
)]

use std::path::PathBuf;
use std::process::Command;

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/public")
        .join(name)
}

fn search(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_castle-pdf"))
        .arg("search")
        .args(args)
        .output()
        .expect("binary runs");
    (
        output.status.code().expect("exit code"),
        String::from_utf8(output.stdout).expect("UTF-8 text"),
        String::from_utf8(output.stderr).expect("UTF-8 errors"),
    )
}

fn counts(err: &str) -> &str {
    err.split(", searched").next().unwrap_or(err)
}

#[test]
fn prints_the_lines_holding_the_query_in_reading_order() {
    let file = corpus("columns.pdf");
    let file = file.to_str().expect("UTF-8 path");
    let (code, out, err) = search(&[file, "LEFT"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        "page 1: Left one\npage 1: Left two\npage 1: Left three\n"
    );
    assert_eq!(counts(&err), "3 occurrences in 1 of 1 pages");
    // Across the pieces of a line joined back, and from one line to the next.
    let (_, out, err) = search(&[file, "first half, second"]);
    assert_eq!(out, "page 1: First half, second half\n");
    assert_eq!(counts(&err), "1 occurrences in 1 of 1 pages");
    let (_, out, err) = search(&[file, "three right"]);
    assert_eq!(out, "page 1: Left three\n");
    assert_eq!(counts(&err), "1 occurrences in 1 of 1 pages");
    let (code, out, err) = search(&[file, "left", "--match-case"]);
    assert_eq!((code, out.as_str()), (0, ""));
    assert_eq!(counts(&err), "0 occurrences in 0 of 1 pages");
}

#[test]
fn accents_are_ignored_unless_asked() {
    let file = corpus("text.pdf");
    let file = file.to_str().expect("UTF-8 path");
    let (_, out, _) = search(&[file, "cafe"]);
    assert_eq!(out, "page 1: Café € çon fi\n");
    let (_, out, _) = search(&[file, "cafe", "--match-diacritics"]);
    assert_eq!(out, "");
    let (_, out, _) = search(&[file, "Café", "--match-diacritics", "--match-case"]);
    assert_eq!(out, "page 1: Café € çon fi\n");
}
