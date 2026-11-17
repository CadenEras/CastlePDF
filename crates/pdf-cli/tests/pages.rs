//! Runs `castle-pdf pages` on the public corpus (tranche 4.3).
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers: a failed expectation is a test failure"
)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/public")
        .join(name)
}

/// A path in Cargo's scratch directory, removed if it exists.
fn fresh(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_file(&path);
    path
}

fn castle(args: &[&OsStr]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_castle-pdf"))
        .args(args)
        .output()
        .expect("binary runs");
    (
        output.status.code().expect("exit code"),
        String::from_utf8(output.stdout).expect("UTF-8 report"),
        String::from_utf8(output.stderr).expect("UTF-8 errors"),
    )
}

fn render(file: &Path, page: &str, name: &str) -> Vec<u8> {
    let output = fresh(name);
    let (code, _, err) = castle(&[
        "render".as_ref(),
        file.as_os_str(),
        page.as_ref(),
        output.as_os_str(),
    ]);
    assert_eq!(code, 0, "{err}");
    std::fs::read(&output).expect("image written")
}

#[test]
fn a_plan_deletes_reorders_and_turns_pages() {
    let input = corpus("shapes.pdf");
    let output = fresh("shapes-plan.pdf");
    let (code, out, err) = castle(&[
        "pages".as_ref(),
        input.as_os_str(),
        output.as_os_str(),
        "3,1".as_ref(),
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("rewritten: 2 pages, "), "{out}");
    // Page 3 of the source is page 1 of the result, and draws the same.
    assert_eq!(
        render(&input, "3", "shapes-source-3.ppm"),
        render(&output, "1", "shapes-plan-1.ppm")
    );
    assert_eq!(
        render(&input, "1", "shapes-source-1.ppm"),
        render(&output, "2", "shapes-plan-2.ppm")
    );
    // A quarter turn: the rendered page's sides are swapped.
    let turned = fresh("shapes-turned.pdf");
    let (code, _, err) = castle(&[
        "pages".as_ref(),
        input.as_os_str(),
        turned.as_os_str(),
        "1@90".as_ref(),
    ]);
    assert_eq!(code, 0, "{err}");
    let (_, inspected, _) = castle(&["inspect".as_ref(), turned.as_os_str()]);
    assert!(inspected.contains("pages: 1"), "{inspected}");
    assert!(inspected.contains(", rotation 90\n"), "{inspected}");
}

#[test]
fn an_invalid_plan_or_an_existing_output_is_refused() {
    let input = corpus("shapes.pdf");
    let output = fresh("shapes-refused.pdf");
    for plan in ["4", "1@45", ""] {
        let (code, _, err) = castle(&[
            "pages".as_ref(),
            input.as_os_str(),
            output.as_os_str(),
            plan.as_ref(),
        ]);
        assert_eq!(code, 2, "{plan}: {err}");
        assert!(err.starts_with("error: invalid page plan"), "{err}");
        assert!(!output.exists());
    }
    let (code, _, err) = castle(&[
        "pages".as_ref(),
        input.as_os_str(),
        input.as_os_str(),
        "1".as_ref(),
    ]);
    assert_eq!(code, 1);
    assert!(err.contains("the output exists"), "{err}");
}

#[test]
fn merge_writes_every_page_of_each_file_in_order() {
    let (shapes, text) = (corpus("shapes.pdf"), corpus("text.pdf"));
    let output = fresh("merged.pdf");
    let (code, out, err) = castle(&[
        "merge".as_ref(),
        output.as_os_str(),
        shapes.as_os_str(),
        text.as_os_str(),
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("rewritten: 5 pages, "), "{out}");
    assert_eq!(
        render(&text, "2", "text-source-2.ppm"),
        render(&output, "5", "merged-5.ppm")
    );
    // An output that is one of the inputs, or exists, is refused.
    let (code, _, err) = castle(&[
        "merge".as_ref(),
        shapes.as_os_str(),
        text.as_os_str(),
        text.as_os_str(),
    ]);
    assert_eq!(code, 1);
    assert!(err.contains("the output exists"), "{err}");
}

#[test]
fn split_cuts_a_document_into_new_files() {
    let input = corpus("shapes.pdf");
    let directory = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("split");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("directory");
    let (code, out, err) = castle(&[
        "split".as_ref(),
        input.as_os_str(),
        directory.as_os_str(),
        "2".as_ref(),
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.lines().count(), 2, "{out}");
    let second = directory.join("part-002.pdf");
    assert_eq!(
        render(&input, "3", "split-source-3.ppm"),
        render(&second, "1", "split-part-2.ppm")
    );
    // Again: the parts exist, none is written.
    let (code, _, err) = castle(&[
        "split".as_ref(),
        input.as_os_str(),
        directory.as_os_str(),
        "1".as_ref(),
    ]);
    assert_eq!(code, 1);
    assert!(err.contains("part-001.pdf exists"), "{err}");
    assert!(!directory.join("part-003.pdf").exists());
    for size in ["0", "+1", "x"] {
        let (code, _, _) = castle(&[
            "split".as_ref(),
            input.as_os_str(),
            directory.as_os_str(),
            size.as_ref(),
        ]);
        assert_eq!(code, 2, "{size}");
    }
}

#[test]
fn a_merge_input_that_cannot_be_read_is_named_by_its_position() {
    let output = fresh("merged-missing.pdf");
    let missing = fresh("no-such-input.pdf");
    let (code, _, err) = castle(&[
        "merge".as_ref(),
        output.as_os_str(),
        corpus("shapes.pdf").as_os_str(),
        missing.as_os_str(),
    ]);
    assert_eq!(code, 1);
    assert!(err.starts_with("error: input 2: "), "{err}");
    assert!(!err.contains("no-such-input"), "{err}");
    assert!(!output.exists());
}
