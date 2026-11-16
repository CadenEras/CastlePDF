//! An independent check of what `pdf-write` writes (ADR-0008): `qpdf
//! --check` finds no syntax or stream error in a written file, with names,
//! strings and reals that need escaping, a free entry and a stream.
//!
//! Runs only when `CASTLE_QPDF` names a qpdf executable (set by the
//! `oracle` workflow); otherwise it says so and passes.

#![allow(
    clippy::expect_used,
    clippy::print_stderr,
    reason = "test code: a failed expectation is a test failure"
)]

use std::process::Command;

use pdf_syntax::{Dictionary, Object, ObjectId};
use pdf_write::FileWriter;

fn written() -> Vec<u8> {
    let reference = |n: i64| Object::Reference(ObjectId::new(n, 0).expect("id"));
    let dictionary = |entries: Vec<(&str, Object)>| {
        let mut d = Dictionary::new();
        for (k, v) in entries {
            d.insert(k.as_bytes().to_vec(), v);
        }
        d
    };
    let name = |n: &[u8]| Object::Name(n.to_vec());
    let mut writer = FileWriter::new(1, 7, 1 << 20).expect("writer");
    writer
        .object(
            1,
            &Object::Dictionary(dictionary(vec![
                ("Type", name(b"Catalog")),
                ("Pages", reference(2)),
            ])),
        )
        .expect("catalog");
    writer
        .object(
            2,
            &Object::Dictionary(dictionary(vec![
                ("Type", name(b"Pages")),
                ("Kids", Object::Array(vec![reference(3)])),
                ("Count", Object::Integer(1)),
            ])),
        )
        .expect("pages");
    writer
        .object(
            3,
            &Object::Dictionary(dictionary(vec![
                ("Type", name(b"Page")),
                ("Parent", reference(2)),
                (
                    "MediaBox",
                    Object::Array(vec![
                        Object::Integer(0),
                        Object::Real(-0.25),
                        Object::Real(612.5),
                        Object::Real(1e3),
                    ]),
                ),
                ("Contents", reference(5)),
                // Required of a page, possibly inherited (7.7.3.3): qpdf
                // 11 warns without it.
                ("Resources", Object::Dictionary(Dictionary::new())),
                ("A#B C", name(b"x/y(z)\xE9")),
            ])),
        )
        .expect("page");
    writer
        .stream(5, &Dictionary::new(), b"BT /F1 12 Tf (a\\(b\\)\\\\) Tj ET")
        .expect("content");
    writer
        .object(
            6,
            &Object::Dictionary(dictionary(vec![(
                "Title",
                Object::String(b"(\r\n)\\\xFF".to_vec()),
            )])),
        )
        .expect("info");
    writer
        .finish(&dictionary(vec![
            ("Root", reference(1)),
            ("Info", reference(6)),
        ]))
        .expect("finish")
}

#[test]
fn qpdf_finds_no_error_in_a_written_file() {
    let Ok(qpdf) = std::env::var("CASTLE_QPDF") else {
        eprintln!("CASTLE_QPDF not set: qpdf check of written files skipped");
        return;
    };
    let dir = std::env::temp_dir().join(format!("castle-pdf-write-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temporary directory");
    let path = dir.join("written.pdf");
    std::fs::write(&path, written()).expect("write");
    let output = Command::new(qpdf)
        .arg("--check")
        .arg(&path)
        .output()
        .expect("run qpdf");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "qpdf --check failed:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("No syntax or stream encoding errors found"),
        "{stdout}\n{stderr}"
    );
    eprintln!("written files: qpdf --check finds no error");
}
