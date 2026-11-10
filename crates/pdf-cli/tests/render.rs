//! Runs `castle-pdf render` on the public corpus.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers: a failed expectation is a test failure"
)]

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

fn render(args: &[&std::ffi::OsStr]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_castle-pdf"))
        .arg("render")
        .args(args)
        .output()
        .expect("binary runs");
    (
        output.status.code().expect("exit code"),
        String::from_utf8(output.stdout).expect("UTF-8 report"),
        String::from_utf8(output.stderr).expect("UTF-8 errors"),
    )
}

fn os(path: &Path) -> &std::ffi::OsStr {
    path.as_os_str()
}

#[test]
fn renders_a_page_to_a_new_ppm() {
    let input = corpus("shapes.pdf");
    let output = fresh("shapes-1.ppm");
    let (code, out, err) = render(&[
        os(&input),
        "1".as_ref(),
        os(&output),
        "--scale".as_ref(),
        "2".as_ref(),
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "page 1: 400x400 pixels; not rendered: none\n");
    let image = std::fs::read(&output).expect("image written");
    let header = b"P6\n400 400\n255\n";
    assert_eq!(&image[..header.len()], header);
    assert_eq!(image.len(), header.len() + 400 * 400 * 3);
    // User (50, 40) at scale 2: device (100, 320), inside the red fill.
    let index = header.len() + (320 * 400 + 100) * 3;
    assert_eq!(&image[index..index + 3], &[255, 0, 0]);

    // The output exists now: never overwritten.
    let (code, _, err) = render(&[os(&input), "1".as_ref(), os(&output)]);
    assert_eq!(code, 1);
    assert!(err.starts_with("error: cannot create the image"), "{err}");
    // No temporary file is left next to the output.
    let leftovers: Vec<_> = std::fs::read_dir(output.parent().expect("directory"))
        .expect("directory listing")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".shapes-1.ppm.")
        })
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    // Nor the input document.
    let copy = fresh("shapes-copy.pdf");
    std::fs::copy(&input, &copy).expect("copy");
    let (code, _, _) = render(&[os(&copy), "1".as_ref(), os(&copy)]);
    assert_eq!(code, 1);
    assert_eq!(
        std::fs::read(&copy).expect("copy"),
        std::fs::read(&input).expect("input")
    );
}

#[test]
fn reports_what_is_not_rendered_and_errors() {
    let output = fresh("minimal-1.ppm");
    let (code, out, err) = render(&[os(&corpus("minimal.pdf")), "1".as_ref(), os(&output)]);
    assert_eq!(code, 0, "{err}");
    // Helvetica is not embedded: drawn with a replacement font.
    assert_eq!(
        out,
        "page 1: 595x842 pixels; not rendered: original fonts (substituted)\n"
    );

    let output = fresh("missing-page.ppm");
    let (code, _, err) = render(&[os(&corpus("shapes.pdf")), "4".as_ref(), os(&output)]);
    assert_eq!(code, 1);
    assert_eq!(err, "error: no page 4 (the document has 3)\n");
    assert!(!output.exists());

    let (code, _, err) = render(&[
        os(&corpus("shapes.pdf")),
        "1".as_ref(),
        os(&output),
        "--scale".as_ref(),
        "0".as_ref(),
    ]);
    assert_eq!(code, 1);
    assert_eq!(err, "error: invalid render request\n");
    assert!(!output.exists());
}
