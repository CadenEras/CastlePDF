//! Differential check against qpdf (ADR-0002: oracle for tests, never shipped).
//!
//! Runs only when `CASTLE_QPDF` names a qpdf executable (set by the `oracle`
//! workflow on Linux). For every fixture of tests/corpus/public, all of which
//! are valid files, `qpdf --check` must succeed and the in-use cross-reference
//! entries reported by `qpdf --show-xref` must equal those read by `PdfFile`.
//! A disagreement is analysed, not settled in favour of the oracle.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers: a failed expectation is a test failure"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use pdf_syntax::{
    Budget, CancellationToken, IndirectValue, MemorySource, Object, ObjectId, PdfFile,
    ResourceLimits, XrefEntry,
};

/// (number, generation) -> where the object lives, in qpdf's terms:
/// `uncompressed X` (file offset) or `compressed S I` (object stream, index).
type Entries = BTreeMap<(u32, u16), String>;

fn ours(path: &Path) -> Entries {
    let limits = ResourceLimits::default();
    let bytes = std::fs::read(path).expect("fixture readable");
    let source = MemorySource::from_vec(bytes, &limits).expect("within limits");
    let file = PdfFile::open(source, &mut Budget::new(limits, CancellationToken::new()))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    file.entries()
        .filter_map(|(number, entry)| match entry {
            XrefEntry::InUse { offset, generation } => {
                Some(((number, generation), format!("uncompressed {offset}")))
            }
            XrefEntry::Compressed { stream, index } => {
                Some(((number, 0), format!("compressed {stream} {index}")))
            }
            _ => None,
        })
        .collect()
}

fn qpdf(qpdf: &str, args: &[&str], path: &Path) -> String {
    let output = Command::new(qpdf)
        .args(args)
        .arg(path)
        .output()
        .expect("qpdf runs");
    assert!(
        output.status.success(),
        "qpdf {args:?} {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("qpdf prints UTF-8")
}

/// Parses `n/g: uncompressed; offset = X` and
/// `n/g: compressed; stream = S, index = I`; any other line fails.
fn theirs(show_xref: &str) -> Entries {
    show_xref
        .lines()
        .map(|line| {
            let parsed = line.split_once(": ").and_then(|(id, rest)| {
                let (number, generation) = id.split_once('/')?;
                let id = (number.parse().ok()?, generation.parse().ok()?);
                let place = if let Some(offset) = rest.strip_prefix("uncompressed; offset = ") {
                    format!("uncompressed {}", offset.trim().parse::<u64>().ok()?)
                } else {
                    let rest = rest.strip_prefix("compressed; stream = ")?;
                    let (stream, index) = rest.split_once(", index = ")?;
                    format!(
                        "compressed {} {}",
                        stream.trim().parse::<u32>().ok()?,
                        index.trim().parse::<u32>().ok()?
                    )
                };
                Some((id, place))
            });
            parsed.unwrap_or_else(|| panic!("unexpected qpdf line: {line}"))
        })
        .collect()
}

#[test]
fn cross_reference_entries_match_qpdf() {
    let Ok(qpdf_path) = std::env::var("CASTLE_QPDF") else {
        eprintln!("CASTLE_QPDF not set: qpdf differential test skipped");
        return;
    };
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/public");
    let mut compared = 0;
    for entry in std::fs::read_dir(&dir).expect("corpus directory") {
        let path = entry.expect("directory entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("pdf") {
            continue;
        }
        qpdf(&qpdf_path, &["--check"], &path);
        let expected = theirs(&qpdf(&qpdf_path, &["--show-xref"], &path));
        assert_eq!(ours(&path), expected, "{}", path.display());
        compared += 1;
    }
    assert!(compared > 0, "no fixture compared");
    eprintln!("{compared} fixtures match qpdf");
}

/// Raw stdout of qpdf (stream data is binary).
fn qpdf_bytes(qpdf: &str, args: &[&str], path: &Path) -> Vec<u8> {
    let output = Command::new(qpdf)
        .args(args)
        .arg(path)
        .output()
        .expect("qpdf runs");
    assert!(
        output.status.success(),
        "qpdf {args:?} {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Every stream of every fixture decodes to the bytes of
/// `qpdf --show-object=N --filtered-stream-data` (filters and predictors).
#[test]
fn decoded_streams_match_qpdf() {
    let Ok(qpdf_path) = std::env::var("CASTLE_QPDF") else {
        eprintln!("CASTLE_QPDF not set: qpdf differential test skipped");
        return;
    };
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/public");
    let mut compared = 0;
    for entry in std::fs::read_dir(&dir).expect("corpus directory") {
        let path = entry.expect("directory entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("pdf") {
            continue;
        }
        let limits = ResourceLimits::default();
        let bytes = std::fs::read(&path).expect("fixture readable");
        let source = MemorySource::from_vec(bytes, &limits).expect("within limits");
        let mut budget = Budget::new(limits, CancellationToken::new());
        let file = PdfFile::open(source, &mut budget).expect("valid fixture");
        let ids: Vec<ObjectId> = file
            .entries()
            .filter_map(|(number, entry)| match entry {
                // Compressed objects are never streams.
                XrefEntry::InUse { generation, .. } => Some(ObjectId { number, generation }),
                _ => None,
            })
            .collect();
        for id in ids {
            let IndirectValue::Stream(stream) = file.resolve(id, &mut budget).expect("resolvable")
            else {
                continue;
            };
            // JPEG data is an image codec, decoded by pdf-render (ADR-0005),
            // not a pdf-syntax filter; qpdf leaves it encoded as well.
            if stream.dictionary.get(b"Filter") == Some(&Object::Name(b"DCTDecode".to_vec())) {
                continue;
            }
            // qpdf does not decode CCITTFax ("unable to filter stream
            // data"): its oracle is pdfimages (tests/pdfimages_oracle.rs).
            if stream.dictionary.get(b"Filter") == Some(&Object::Name(b"CCITTFaxDecode".to_vec())) {
                continue;
            }
            let ours = file
                .decoded_stream_data(&stream, &mut budget)
                .unwrap_or_else(|e| panic!("{} object {}: {e}", path.display(), id.number));
            let show = format!("--show-object={},{}", id.number, id.generation);
            let theirs = qpdf_bytes(&qpdf_path, &[&show, "--filtered-stream-data"], &path);
            assert_eq!(ours, theirs, "{} object {}", path.display(), id.number);
            compared += 1;
        }
    }
    assert!(compared > 0, "no stream compared");
    eprintln!("{compared} streams: decoded data match qpdf");
}
