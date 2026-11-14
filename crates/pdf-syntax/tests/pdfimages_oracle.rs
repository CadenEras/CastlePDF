//! Differential test of `CCITTFaxDecode` (tranche 3.9) against `pdfimages`
//! (Poppler, GPL): an oracle for tests only, never shipped (ADR-0002).
//! Runs only when `CASTLE_PDFIMAGES` names the executable (set by the
//! `oracle` workflow); skipped otherwise.
//!
//! `pdfimages` writes each bilevel image of `ccitt.pdf` as a PBM, 1 for
//! black as the image appears. Our decoded data has 1 for white unless
//! `/BlackIs1`; a stencil mask's PBM is written inverted. Every bit is
//! compared, padding of each row excluded.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use std::path::PathBuf;
use std::process::Command;

use pdf_syntax::{
    Budget, CancellationToken, IndirectValue, MemorySource, ObjectId, PdfFile, ResourceLimits,
};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/public/ccitt.pdf")
}

/// Width, height and rows of a binary PBM (P4).
fn read_pbm(bytes: &[u8]) -> (usize, usize, Vec<u8>) {
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(32)]).into_owned();
    let header: Vec<&str> = text.split_ascii_whitespace().take(3).collect();
    assert_eq!(header[0], "P4", "PBM header");
    let (width, height): (usize, usize) = (
        header[1].parse().expect("width"),
        header[2].parse().expect("height"),
    );
    let stride = width.div_ceil(8);
    let data = bytes[bytes.len() - stride * height..].to_vec();
    (width, height, data)
}

#[test]
fn ccitt_images_match_pdfimages() {
    let Some(executable) = std::env::var_os("CASTLE_PDFIMAGES") else {
        eprintln!("pdfimages oracle skipped: CASTLE_PDFIMAGES is not set");
        return;
    };
    let dir = std::env::temp_dir().join(format!("castle-pdfimages-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temporary directory");
    let status = Command::new(executable)
        .arg(fixture())
        .arg(dir.join("img"))
        .status()
        .expect("pdfimages runs");
    assert!(status.success(), "pdfimages failed");

    let limits = ResourceLimits::default();
    let bytes = std::fs::read(fixture()).expect("fixture");
    let source = MemorySource::from_vec(bytes, &limits).expect("source");
    let mut budget = Budget::new(limits, CancellationToken::new());
    let file = PdfFile::open(source, &mut budget).expect("file");
    // Object, pdfimages index (the inline image of page 1 is 3), and how a
    // decoded bit compares with the PBM: inverted for white-is-1 images.
    for (number, index, invert) in [(7, 0, true), (8, 1, true), (9, 2, false), (10, 4, false)] {
        let id = ObjectId::new(number, 0).expect("id");
        let IndirectValue::Stream(stream) = file.resolve(id, &mut budget).expect("object") else {
            panic!("object {number} is not a stream");
        };
        let ours = file
            .decoded_stream_data(&stream, &mut budget)
            .expect("decoded");
        let pbm = std::fs::read(dir.join(format!("img-{index:03}.pbm"))).expect("pdfimages output");
        let (width, height, theirs) = read_pbm(&pbm);
        assert_eq!(ours.len(), theirs.len(), "object {number}: size");
        let stride = width.div_ceil(8);
        let mut differing = 0;
        for y in 0..height {
            for x in 0..width {
                let bit = |data: &[u8]| (data[y * stride + x / 8] >> (7 - x % 8)) & 1;
                let mine = bit(&ours) ^ u8::from(invert);
                differing += usize::from(mine != bit(&theirs));
            }
        }
        eprintln!("ccitt.pdf object {number}: {width} x {height}, {differing} pixels differ");
        assert_eq!(differing, 0, "object {number} differs from pdfimages");
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("ccitt images match pdfimages");
}
