//! Runs `castle-pdf text` (ADR-0006, tranche 3.3).
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers: a failed expectation is a test failure"
)]

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/public")
        .join(name)
}

fn text(args: &[&OsStr]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_castle-pdf"))
        .arg("text")
        .args(args)
        .output()
        .expect("binary runs");
    (
        output.status.code().expect("exit code"),
        String::from_utf8(output.stdout).expect("UTF-8 text"),
        String::from_utf8(output.stderr).expect("UTF-8 errors"),
    )
}

#[test]
fn prints_every_page_separated_by_form_feeds() {
    let file = corpus("text.pdf");
    let (code, out, err) = text(&[file.as_os_str()]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        "ToUnicode fine\nIdentity text\u{1D400}\nOther length\nCafé € çon fi\n\
         \x0c\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\nFallback\nHidden\n"
    );
    assert_eq!(
        err,
        "page 1: 51 glyphs, 0 unknown, 12 through a /ToUnicode code of another length, 0 unreadable /ToUnicode, 0 bytes in fonts not read\n\
         page 2: 21 glyphs, 7 unknown, 0 through a /ToUnicode code of another length, 1 unreadable /ToUnicode, 0 bytes in fonts not read\n"
    );
}

#[test]
fn prints_one_page() {
    let file = corpus("text.pdf");
    let (code, out, _) = text(&[file.as_os_str(), "--page".as_ref(), "2".as_ref()]);
    assert_eq!(code, 0);
    assert!(out.ends_with("Fallback\nHidden\n"), "{out}");
    assert!(!out.contains('\x0c'));
    let (code, _, err) = text(&[file.as_os_str(), "--page".as_ref(), "3".as_ref()]);
    assert_eq!(
        (code, err.as_str()),
        (1, "error: no page 3 (the document has 2)\n")
    );
    let (code, _, _) = text(&[file.as_os_str(), "--page".as_ref(), "0".as_ref()]);
    assert_eq!(code, 2);
}

#[test]
fn control_characters_of_the_document_are_not_printed() {
    // Glyph names giving ESC (U+001B) and BEL (U+0007): a terminal escape
    // sequence written by the document would reach the terminal raw.
    let content = "BT /F1 10 Tf 5 5 Td (A$[31m%B) Tj ET";
    let bodies = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding << /Differences [36 /uni001B /uni0007] >> >>".to_owned(),
    ];
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            bodies.len() + 1
        )
        .as_bytes(),
    );
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("control-characters.pdf");
    std::fs::write(&path, pdf).expect("written");
    let (code, out, _) = text(&[path.as_os_str()]);
    assert_eq!(code, 0);
    assert_eq!(out, "A\u{FFFD}[31m\u{FFFD}B\n");
}

/// A PDF of `bodies` numbered from 1.
fn pdf(bodies: &[String]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            bodies.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

#[test]
fn bidirectional_controls_are_not_printed() {
    // RIGHT-TO-LEFT OVERRIDE (U+202E) and FIRST STRONG ISOLATE (U+2068)
    // would reorder what the terminal shows.
    let content = "BT /F1 10 Tf 5 5 Td (A$B%C) Tj ET";
    let bodies = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding << /Differences [36 /uni202E /uni2068] >> >>".to_owned(),
    ];
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bidi.pdf");
    std::fs::write(&path, pdf(&bodies)).expect("written");
    let (code, out, _) = text(&[path.as_os_str()]);
    assert_eq!((code, out.as_str()), (0, "A\u{FFFD}B\u{FFFD}C\n"));
}

#[test]
fn a_failing_page_does_not_stop_the_next_ones() {
    // Page 1 ends inside a text object: an error; page 2 is printed.
    let bodies = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Font << /F1 7 0 R >> >> /Contents 5 0 R >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Font << /F1 7 0 R >> >> /Contents 6 0 R >>".to_owned(),
        "<< /Length 25 >>\nstream\nBT /F1 10 Tf (Broken) Tj\nendstream".to_owned(),
        "<< /Length 28 >>\nstream\nBT /F1 10 Tf (Second) Tj ET\nendstream".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("failing-page.pdf");
    std::fs::write(&path, pdf(&bodies)).expect("written");
    let (code, out, err) = text(&[path.as_os_str()]);
    assert_eq!(code, 1);
    assert_eq!(out, "\x0cSecond\n");
    assert!(err.starts_with("error: page 1: "), "{err}");
    assert!(err.contains("page 2: 6 glyphs"), "{err}");
}
