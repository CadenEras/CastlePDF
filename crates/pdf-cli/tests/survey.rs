//! Runs `castle-pdf survey` and `castle-pdf probe` on the public corpus.
#![allow(
    clippy::expect_used,
    reason = "test helpers: a failed expectation is a test failure"
)]

use std::path::PathBuf;
use std::process::Command;

fn corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/public")
}

fn castle(args: &[&std::ffi::OsStr]) -> (i32, String, String) {
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

#[test]
fn probe_prints_one_outcome_line() {
    let shapes = corpus().join("shapes.pdf");
    let (code, out, err) = castle(&["probe".as_ref(), shapes.as_os_str()]);
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (0, "open=ok pages=3 render=ok skipped=none fonts=none\n", "")
    );
    let minimal = corpus().join("minimal.pdf");
    let (_, out, _) = castle(&["probe".as_ref(), minimal.as_os_str()]);
    assert_eq!(
        out,
        "open=ok pages=1 render=ok skipped=FontSubstituted fonts=Standard14\n"
    );
    // A document error is an outcome, not a failure of the probe.
    let manifest = corpus().join("manifest.json");
    let (code, out, _) = castle(&["probe".as_ref(), manifest.as_os_str()]);
    assert_eq!((code, out.as_str()), (0, "open=malformed/InvalidHeader\n"));
    let (code, out, _) = castle(&["probe".as_ref(), corpus().as_os_str()]);
    assert_eq!((code, out.as_str()), (0, "open=io/NotAFile\n"));
}

#[test]
fn survey_reports_aggregates_without_names() {
    let corpus = corpus();
    let files = std::fs::read_dir(&corpus).expect("corpus").count();
    let (code, out, err) = castle(&["survey".as_ref(), corpus.as_os_str()]);
    assert_eq!((code, err.as_str()), (0, ""));
    let pdfs = files - 1;
    for expected in [
        format!("files: {files}\n"),
        format!("opened: {pdfs} "),
        format!("first page rendered: {pdfs} "),
        "timeouts: 0\ncrashes: 0\n".to_owned(),
        "open failures:\n       1  malformed/InvalidHeader\n".to_owned(),
        "not rendered on rendered first pages:\n".to_owned(),
    ] {
        assert!(out.contains(&expected), "{expected:?} in {out}");
    }
    // Names only on request.
    assert!(!out.contains("manifest") && !out.contains(".pdf"), "{out}");
    let (code, out, _) = castle(&["survey".as_ref(), corpus.as_os_str(), "--paths".as_ref()]);
    assert_eq!(code, 0);
    assert!(
        out.ends_with("files not rendered:\nmalformed/InvalidHeader\t\"manifest.json\"\n"),
        "{out}"
    );
}

#[test]
fn survey_kills_probes_after_the_timeout() {
    // No process exits within a microsecond of its creation: every probe
    // times out, and the survey reports it with exit code 1.
    let (code, out, _) = castle(&[
        "survey".as_ref(),
        corpus().as_os_str(),
        "--timeout".as_ref(),
        "0.000001".as_ref(),
        "--jobs".as_ref(),
        "2".as_ref(),
    ]);
    let files = std::fs::read_dir(corpus()).expect("corpus").count();
    assert_eq!(code, 1);
    assert!(out.contains("opened: 0 (0.0%)\n"), "{out}");
    assert!(out.contains(&format!("timeouts: {files}\n")), "{out}");
}

#[test]
fn survey_of_a_missing_directory_fails() {
    let missing = corpus().join("no-such-directory");
    let (code, out, err) = castle(&["survey".as_ref(), missing.as_os_str()]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(
        err.starts_with("error: cannot list the directory: "),
        "{err}"
    );
}
