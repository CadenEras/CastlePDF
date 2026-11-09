//! Runs the `castle-pdf` binary on the public corpus and on invalid inputs.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers: a failed expectation is a test failure"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/public")
        .join(name)
}

fn inspect(path: &Path) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_castle-pdf"))
        .arg("inspect")
        .arg(path)
        .output()
        .expect("binary runs");
    (
        output.status.code().expect("exit code"),
        String::from_utf8(output.stdout).expect("UTF-8 report"),
        String::from_utf8(output.stderr).expect("UTF-8 errors"),
    )
}

/// Writes `bytes` to a file in Cargo's scratch directory for integration tests.
fn scratch(name: &str, bytes: &[u8]) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, bytes).expect("scratch file written");
    path
}

#[test]
fn page_tree_report_is_stable() {
    let (code, out, err) = inspect(&corpus("page-tree.pdf"));
    assert_eq!(code, 0, "{err}");
    assert_eq!(err, "");
    assert_eq!(
        out,
        "size: 1109 bytes\n\
         version: 1.4\n\
         startxref: 824\n\
         revisions: 1\n  \
         section 1: xref table at 824, 11 entries\n\
         objects: 10 in use, 0 compressed, 1 free\n\
         trailer keys: /Root /Size\n\
         root: 1 0 R\n\
         size entry: 11\n\
         pages: 3\n  \
         page 1: object 4 0, media box [0 0 595 842], crop box [0 0 595 842], rotation 90\n  \
         page 2: object 5 0, media box [0 0 842 595], crop box [0 0 842 595], rotation 90\n  \
         page 3: object 6 0, media box [0 0 595 842], crop box [0 0 595 842], rotation 0\n\
         repair: none\n"
    );
}

#[test]
fn a_repaired_file_says_so() {
    // minimal.pdf cut before its cross-reference table: no table, no trailer.
    let mut bytes = std::fs::read(corpus("minimal.pdf")).expect("fixture");
    let at = bytes
        .windows(5)
        .position(|w| w == b"xref\n")
        .expect("xref present");
    bytes.truncate(at);
    let (code, out, err) = inspect(&scratch("no-xref.pdf", &bytes));
    assert_eq!((code, err.as_str()), (0, ""), "{out}");
    assert!(out.contains("revisions: 0\n"), "{out}");
    assert!(out.contains("pages: 1\n"), "{out}");
    assert!(
        out.ends_with(
            "repair: 5 objects indexed by scan, 0 found more than once; \
             no trailer, root is the last catalog; strict error: \
             malformed PDF syntax at byte 0: MissingStartxref\n"
        ),
        "{out}"
    );
}

#[test]
fn incremental_report_lists_both_revisions() {
    let (code, out, _) = inspect(&corpus("incremental.pdf"));
    assert_eq!(code, 0);
    assert!(out.contains(
        "revisions: 2\n  section 1: xref table at 715, 1 entries\n  section 2: xref table at 425, 6 entries\n"
    ));
    assert!(out.contains("trailer keys: /Prev /Root /Size\n"));
}

#[test]
fn the_report_never_contains_document_content() {
    let (_, out, _) = inspect(&corpus("minimal.pdf"));
    assert!(!out.contains("CastlePDF minimal fixture"));
    assert!(!out.contains("Helvetica"));
}

#[test]
fn unreadable_and_malformed_inputs_exit_with_1() {
    let (code, out, err) = inspect(Path::new("this-file-does-not-exist.pdf"));
    assert_eq!(code, 1);
    assert_eq!(out, "");
    assert!(err.starts_with("error: cannot open the file"), "{err}");
    // The path is never echoed.
    assert!(!err.contains("this-file-does-not-exist"), "{err}");

    // A directory is refused before any read, on every OS.
    let (code, out, err) = inspect(Path::new(env!("CARGO_MANIFEST_DIR")));
    assert_eq!(code, 1);
    assert_eq!(out, "");
    assert_eq!(err, "error: not a regular file\n");

    let (code, out, err) = inspect(&scratch("not-a-pdf.pdf", b"hello"));
    assert_eq!(code, 1);
    assert_eq!(out, "size: 5 bytes\n");
    assert!(err.contains("malformed PDF syntax at byte 0"), "{err}");
}

#[test]
fn the_structure_is_reported_before_a_page_tree_error() {
    // Valid file structure, but /Count says 2 for one page.
    // Edited in place, byte for byte: the header holds non-UTF-8 bytes and every
    // offset must stay valid.
    let mut bytes = std::fs::read(corpus("minimal.pdf")).expect("fixture");
    let at = bytes
        .windows(8)
        .position(|w| w == b"/Count 1")
        .expect("/Count 1 present");
    if let Some(digit) = bytes.get_mut(at + 7) {
        *digit = b'2';
    }
    let (code, out, err) = inspect(&scratch("bad-count.pdf", &bytes));
    assert_eq!(code, 1);
    assert!(
        out.contains("objects: 5 in use, 0 compressed, 1 free\n"),
        "{out}"
    );
    assert!(!out.contains("pages:"));
    assert_eq!(
        err,
        "error: invalid document structure at object 2 0: CountMismatch\n"
    );
}

/// Builds a classic-xref file: object n is `bodies[n - 1]`.
fn build(bodies: &[&str], trailer_extra: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    let size = bodies.len() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {size} /Root 1 0 R{trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    out
}

const CATALOG: &str = "<< /Type /Catalog /Pages 2 0 R >>";
const PAGE: &str = "<< /Type /Page /Parent 2 0 R >>";

#[test]
fn encrypted_documents_are_reported_as_unsupported() {
    let bytes = build(
        &[CATALOG, "<< /Type /Pages /Kids [] /Count 0 >>"],
        " /Encrypt << /Filter /Standard >>",
    );
    let (code, out, err) = inspect(&scratch("encrypted.pdf", &bytes));
    assert_eq!(code, 1);
    assert!(out.starts_with("size: "));
    assert_eq!(err, "error: unsupported PDF capability: Encryption\n");
}

#[test]
fn nonstandard_trailer_keys_are_counted_not_named() {
    let bytes = build(
        &[CATALOG, "<< /Type /Pages /Kids [] /Count 0 >>"],
        " /PrivateNote#0A#1B 1 /Other (secret)",
    );
    let (code, out, _) = inspect(&scratch("trailer-keys.pdf", &bytes));
    assert_eq!(code, 0);
    assert!(
        out.contains("trailer keys: /Root /Size (+2 other)\n"),
        "{out}"
    );
    assert!(!out.contains("Private") && !out.contains("secret"), "{out}");
}

#[test]
fn extreme_coordinates_print_on_bounded_lines() {
    let huge = format!("17{}.0", "0".repeat(307));
    let root = format!("<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [-{huge} 0 {huge} 1] >>");
    let bytes = build(&[CATALOG, root.as_str(), PAGE], "");
    let (code, out, err) = inspect(&scratch("extreme-box.pdf", &bytes));
    assert_eq!(code, 0, "{err}");
    let line = out
        .lines()
        .find(|l| l.starts_with("  page 1:"))
        .expect("page line");
    assert!(line.contains("media box [-1.7e308 0 1.7e308 1]"), "{line}");
    assert!(line.len() < 200, "{line}");
}

#[test]
fn xref_stream_and_hybrid_reports() {
    let (code, out, err) = inspect(&corpus("objstm.pdf"));
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("version: 1.5\n"), "{out}");
    assert!(
        out.contains("  section 1: xref stream at 507, 8 entries\n"),
        "{out}"
    );
    assert!(
        out.contains("objects: 4 in use, 3 compressed, 1 free\n"),
        "{out}"
    );
    assert!(
        out.contains("trailer keys: /DecodeParms /Filter /Length /Root /Size /Type /W\n"),
        "{out}"
    );
    let (code, out, err) = inspect(&corpus("hybrid.pdf"));
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("revisions: 1\n  section 1: xref table at 693, 5 entries\n  section 2: hybrid xref stream at 500, 3 entries\n"),
        "{out}"
    );
}
