use proptest::prelude::*;

use super::*;
use crate::limits::{CancellationToken, LimitKind, ResourceLimits};
use crate::object::IndirectValue;
use crate::source::MemorySource;
use crate::xref::{OpenOptions, PdfFile, XrefEntry};

const CATALOG: &str = "<< /Type /Catalog /Pages 2 0 R >>";
const PAGES: &str = "<< /Type /Pages /Kids [] /Count 0 >>";

/// A valid classic-xref file: object n is `bodies[n - 1]`.
fn build(bodies: &[&str]) -> Vec<u8> {
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
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

/// `bytes` without its last `startxref ... %%EOF`.
fn without_startxref(mut bytes: Vec<u8>) -> Vec<u8> {
    let at = find(&bytes, 0, b"startxref").expect("startxref");
    bytes.truncate(at);
    bytes
}

fn replace(bytes: &[u8], from: &str, to: &str) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes).into_owned();
    assert!(text.contains(from), "{from:?} not found");
    text.replacen(from, to, 1).into_bytes()
}

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

fn open_with(
    bytes: &[u8],
    options: OpenOptions,
    budget: &mut Budget,
) -> Result<PdfFile<MemorySource>, Error> {
    let source = MemorySource::from_vec(bytes.to_vec(), &ResourceLimits::default())?;
    PdfFile::open_with(source, options, budget)
}

fn open(bytes: &[u8]) -> Result<PdfFile<MemorySource>, Error> {
    open_with(bytes, OpenOptions::default(), &mut budget())
}

fn resolve(file: &PdfFile<MemorySource>, number: u32) -> IndirectValue {
    file.resolve(
        ObjectId {
            number,
            generation: 0,
        },
        &mut budget(),
    )
    .expect("resolvable")
}

fn dictionary(file: &PdfFile<MemorySource>, number: u32) -> Dictionary {
    match resolve(file, number) {
        IndirectValue::Object(Object::Dictionary(dictionary)) => dictionary,
        other => panic!("object {number}: {other:?}"),
    }
}

fn report(file: &PdfFile<MemorySource>) -> RepairReport {
    file.repair().expect("repaired")
}

#[test]
fn a_valid_file_is_never_repaired() {
    let file = open(&build(&[CATALOG, PAGES])).expect("valid");
    assert_eq!(file.repair(), None);
    assert_eq!(file.revisions(), 1);
}

#[test]
fn a_missing_startxref_is_repaired_from_the_trailer() {
    let bytes = without_startxref(build(&[CATALOG, PAGES]));
    assert_eq!(
        open_with(&bytes, OpenOptions::strict(), &mut budget()).err(),
        Some(Error::Malformed {
            offset: (bytes.len() as u64).saturating_sub(1024),
            kind: MalformedKind::MissingStartxref
        })
    );
    let file = open(&bytes).expect("repaired");
    let report = report(&file);
    assert!(matches!(
        report.cause,
        Error::Malformed {
            kind: MalformedKind::MissingStartxref,
            ..
        }
    ));
    assert_eq!(
        (report.objects, report.conflicts, report.trailer),
        (2, 0, RepairTrailer::Found)
    );
    assert_eq!(file.root().map(|id| id.number), Some(1));
    assert_eq!(file.trailer().get(b"Size"), Some(&Object::Integer(3)));
    assert_eq!(
        dictionary(&file, 2).get(b"Type"),
        Some(&Object::Name(b"Pages".to_vec()))
    );
    // A repaired file has no cross-reference section to update.
    assert_eq!((file.revisions(), file.startxref()), (0, 0));
}

#[test]
fn wrong_table_offsets_are_repaired_when_opening() {
    // `startxref` points one byte before `xref`.
    let bytes = build(&[CATALOG, PAGES]);
    let xref = find(&bytes, 0, b"\nxref").expect("xref") as u64;
    let bytes = replace(
        &bytes,
        &format!("startxref\n{}", xref + 1),
        &format!("startxref\n{xref}"),
    );
    let file = open(&bytes).expect("repaired");
    assert!(matches!(
        report(&file).cause,
        Error::Malformed {
            kind: MalformedKind::MissingXref,
            ..
        }
    ));
    assert_eq!(file.root().map(|id| id.number), Some(1));
}

#[test]
fn objects_off_their_offset_are_found_by_a_scan() {
    // Three bytes inserted after the header: every offset is short by 3, but
    // the table itself is found.
    let bytes = build(&[CATALOG, PAGES]);
    let xref = find(&bytes, 0, b"\nxref").expect("xref") + 1;
    let mut shifted = replace(&bytes, "%PDF-1.7\n", "%PDF-1.7\n%x\n");
    shifted = replace(
        &shifted,
        &format!("startxref\n{xref}"),
        &format!("startxref\n{}", xref + 3),
    );
    let strict = open_with(&shifted, OpenOptions::strict(), &mut budget()).expect("opens");
    assert!(matches!(
        strict.resolve(
            ObjectId {
                number: 1,
                generation: 0
            },
            &mut budget()
        ),
        Err(Error::Malformed {
            kind: MalformedKind::ObjectIdMismatch,
            ..
        })
    ));
    let file = open(&shifted).expect("opens");
    assert_eq!(file.repair(), None, "not repaired before a lookup fails");
    assert_eq!(
        dictionary(&file, 1).get(b"Type"),
        Some(&Object::Name(b"Catalog".to_vec()))
    );
    let report = report(&file);
    assert_eq!(
        (report.objects, report.trailer),
        (2, RepairTrailer::Original)
    );
    assert!(matches!(
        report.cause,
        Error::Malformed {
            kind: MalformedKind::ObjectIdMismatch,
            ..
        }
    ));
    // Object 2 uses the index already built.
    assert!(matches!(resolve(&file, 2), IndirectValue::Object(_)));
}

#[test]
fn a_file_cut_inside_an_object_finds_its_catalog() {
    let bytes = build(&[PAGES, CATALOG, "<< /Producer (cut here) >>"]);
    let cut = find(&bytes, 0, b"cut here").expect("marker");
    let file = open(&bytes[..cut]).expect("repaired");
    let report = report(&file);
    assert!(repairable(&report.cause), "{:?}", report.cause);
    assert_eq!(report.trailer, RepairTrailer::Catalog);
    assert_eq!(file.root().map(|id| id.number), Some(2));
    // Object 3 is indexed but cannot be read.
    assert!(
        file.resolve(
            ObjectId {
                number: 3,
                generation: 0
            },
            &mut budget()
        )
        .is_err()
    );
}

#[test]
fn the_last_occurrence_of_an_object_wins() {
    let mut bytes = without_startxref(build(&[CATALOG, PAGES, "(old)"]));
    bytes.extend_from_slice(b"3 0 obj\n(new)\nendobj\n");
    let file = open(&bytes).expect("repaired");
    assert_eq!(report(&file).conflicts, 1);
    assert_eq!(
        resolve(&file, 3),
        IndirectValue::Object(Object::String(b"new".to_vec()))
    );
}

#[test]
fn headers_inside_stream_data_are_not_indexed() {
    // Object 3: direct /Length; object 4: indirect /Length, skipped up to
    // `endstream`. Both hold text that looks like object headers.
    let first = "9 0 obj (fake) endobj";
    let second = "10 0 obj (fake) endobj";
    let bytes = without_startxref(build(&[
        CATALOG,
        PAGES,
        &format!("<< /Length {} >>\nstream\n{first}\nendstream", first.len()),
        "<< /Length 5 0 R >>\nstream\n10 0 obj (fake) endobj\nendstream",
        &second.len().to_string(),
    ]));
    let file = open(&bytes).expect("repaired");
    let numbers: Vec<u32> = file.entries().map(|(number, _)| number).collect();
    assert_eq!(numbers, [1, 2, 3, 4, 5]);
    let IndirectValue::Stream(stream) = resolve(&file, 4) else {
        panic!("object 4 is a stream");
    };
    assert_eq!(
        file.stream_data(&stream).expect("data").as_ref(),
        second.as_bytes()
    );
}

#[test]
fn a_header_needs_a_token_boundary() {
    let bytes = without_startxref(build(&[
        CATALOG,
        PAGES,
        "(x12 0 obj) (7 0 objx) (8 0 endobj)",
    ]));
    let file = open(&bytes).expect("repaired");
    assert_eq!(file.entries().count(), 3);
}

/// An object stream holding objects 1 and 2, uncompressed, as object 3.
fn object_stream_file() -> Vec<u8> {
    let objects = [CATALOG, PAGES];
    let mut header = String::new();
    let mut body = String::new();
    for (i, object) in objects.iter().enumerate() {
        header.push_str(&format!("{} {} ", i + 1, body.len()));
        body.push_str(object);
        body.push('\n');
    }
    let data = format!("{header}{body}");
    let mut out = b"%PDF-1.5\n".to_vec();
    out.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /ObjStm /N 2 /First {} /Length {} >>\nstream\n{data}\nendstream\nendobj\n",
            header.len(),
            data.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(
        b"4 0 obj\n<< /Type /XRef /Size 5 /W [1 2 1] /Root 1 0 R /Length 0 >>\nstream\n\nendstream\nendobj\n",
    );
    out
}

#[test]
fn objects_of_unindexed_object_streams_are_found() {
    let file = open(&object_stream_file()).expect("repaired");
    let report = report(&file);
    assert_eq!((report.objects, report.trailer), (4, RepairTrailer::Found));
    assert_eq!(
        file.entry(1),
        Some(XrefEntry::Compressed {
            stream: 3,
            index: 0
        })
    );
    // The trailer is the cross-reference stream dictionary.
    assert_eq!(file.root().map(|id| id.number), Some(1));
    assert_eq!(
        dictionary(&file, 2).get(b"Type"),
        Some(&Object::Name(b"Pages".to_vec()))
    );
}

#[test]
fn a_direct_object_after_its_object_stream_wins() {
    let mut bytes = object_stream_file();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 /Later true >>\nendobj\n");
    let file = open(&bytes).expect("repaired");
    assert!(dictionary(&file, 2).get(b"Later").is_some());
    assert_eq!(report(&file).conflicts, 1);
}

#[test]
fn encryption_is_never_repaired() {
    let bytes = build(&[CATALOG, PAGES, "<< /Filter /Standard >>"]);
    let bytes = without_startxref(replace(&bytes, "/Root 1 0 R", "/Root 1 0 R /Encrypt 3 0 R"));
    assert_eq!(
        open(&bytes).err(),
        Some(Error::Unsupported(Capability::Encryption))
    );
    // Cut before the table: the `/Encrypt` key is lost with the trailer, but
    // the encryption dictionary itself (standard or public-key handler)
    // refuses the file.
    for handler in [
        "<< /Filter /Standard /V 2 /R 3 /O <00> /U <00> /P -4 >>",
        "<< /Filter /Adobe.PubSec /SubFilter /adbe.pkcs7.s4 /V 4 /Recipients [<00>] >>",
    ] {
        let mut bytes = build(&[CATALOG, PAGES, handler]);
        let at = find(&bytes, 0, b"xref").expect("xref");
        bytes.truncate(at);
        assert_eq!(
            open(&bytes).err(),
            Some(Error::Unsupported(Capability::Encryption)),
            "{handler}"
        );
    }
    // The word alone is not a dictionary: this file is not encrypted.
    let mut bytes = build(&[CATALOG, PAGES, "(/Encrypt /Filter /O /U)"]);
    let at = find(&bytes, 0, b"xref").expect("xref");
    bytes.truncate(at);
    assert!(open(&bytes).is_ok());
}

#[test]
fn a_file_without_any_root_keeps_its_strict_error() {
    let bytes = without_startxref(build(&[PAGES]));
    let bytes = replace(&bytes, "/Root 1 0 R", "/Root 9 0 R");
    assert!(matches!(
        open(&bytes).err(),
        Some(Error::Malformed {
            kind: MalformedKind::MissingStartxref,
            ..
        })
    ));
    // A header error is not repaired.
    let bytes = replace(&build(&[CATALOG]), "%PDF-1.7", "%PDX-1.7");
    assert!(matches!(
        open(&bytes).err(),
        Some(Error::Malformed {
            kind: MalformedKind::InvalidHeader,
            ..
        })
    ));
}

#[test]
fn every_header_is_charged_and_cancellation_is_honoured() {
    let bytes = without_startxref(build(&[CATALOG, PAGES, "1", "2", "3"]));
    let opens = |limit: u64| {
        let mut budget = Budget::new(
            ResourceLimits {
                max_objects_visited: limit,
                ..ResourceLimits::default()
            },
            CancellationToken::new(),
        );
        open_with(&bytes, OpenOptions::default(), &mut budget)
    };
    // The smallest limit that opens; one less is refused by the limit.
    let exact = (1..100).find(|limit| opens(*limit).is_ok()).expect("opens");
    assert!(exact >= 5, "each of the 5 headers is charged: {exact}");
    assert_eq!(
        opens(exact - 1).err(),
        Some(Error::LimitExceeded {
            kind: LimitKind::ObjectsVisited,
            limit: exact - 1
        })
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut cancelled = Budget::new(ResourceLimits::default(), cancel);
    assert_eq!(
        open_with(&bytes, OpenOptions::default(), &mut cancelled).err(),
        Some(Error::Cancelled)
    );
}

#[test]
fn many_headers_are_stopped_by_the_object_limit() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    for _ in 0..10_000 {
        bytes.extend_from_slice(b"1 0 obj ");
    }
    let mut budget = Budget::new(
        ResourceLimits {
            max_objects_visited: 1000,
            ..ResourceLimits::default()
        },
        CancellationToken::new(),
    );
    assert_eq!(
        open_with(&bytes, OpenOptions::default(), &mut budget).err(),
        Some(Error::LimitExceeded {
            kind: LimitKind::ObjectsVisited,
            limit: 1000
        })
    );
}

#[test]
fn header_parts_are_bounded() {
    let data = b" 4294967295 65535 obj 4294967296 0 obj 1 65536 obj 0 0 obj";
    let headers: Vec<_> = (0..data.len())
        .filter(|at| keyword_at(data, *at, b"obj"))
        .filter_map(|at| header_before(data, at))
        .collect();
    assert_eq!(headers, [(1, u32::MAX, u16::MAX)]);
}

/// `build`, with every table entry moved by `delta` bytes; the table is
/// still found.
fn with_entries_moved(bodies: &[&str], delta: i64) -> Vec<u8> {
    let bytes = build(bodies);
    let text = String::from_utf8(bytes).expect("ASCII");
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        match line.strip_suffix(" 00000 n \n") {
            Some(offset) => {
                let offset: i64 = offset.parse().expect("offset");
                out.push_str(&format!("{:010} 00000 n \n", offset + delta));
            }
            None => out.push_str(line),
        }
    }
    out.into_bytes()
}

#[test]
fn any_entry_that_does_not_lead_to_its_object_is_relocated() {
    // +2 lands on `0 obj` (a number then `obj`); +27 inside the catalog on
    // `2 0 R` (no `obj`). Both are mismatches, repaired on lookup.
    for delta in [2, 27, -3] {
        let bytes = with_entries_moved(&[CATALOG, PAGES], delta);
        let strict = open_with(&bytes, OpenOptions::strict(), &mut budget()).expect("opens");
        assert!(
            matches!(
                strict.resolve(
                    ObjectId {
                        number: 1,
                        generation: 0
                    },
                    &mut budget()
                ),
                Err(Error::Malformed {
                    kind: MalformedKind::ObjectIdMismatch,
                    ..
                })
            ),
            "delta {delta}"
        );
        let file = open(&bytes).expect("opens");
        assert_eq!(
            dictionary(&file, 1).get(b"Type"),
            Some(&Object::Name(b"Catalog".to_vec())),
            "delta {delta}"
        );
        assert_eq!(report(&file).trailer, RepairTrailer::Original);
    }
}

#[test]
fn a_relocation_follows_the_budget_and_cancellation() {
    let bytes = with_entries_moved(&[CATALOG, PAGES], 2);
    let file = open(&bytes).expect("opens");
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut cancelled = Budget::new(ResourceLimits::default(), cancel);
    assert_eq!(
        file.resolve(
            ObjectId {
                number: 1,
                generation: 0
            },
            &mut cancelled
        )
        .err(),
        Some(Error::Cancelled)
    );
    assert_eq!(file.repair(), None, "a stopped scan repairs nothing");
    // A later task with its own budget scans again.
    assert!(matches!(resolve(&file, 1), IndirectValue::Object(_)));
    assert!(file.repair().is_some());
}

#[test]
fn an_unreadable_object_stream_is_skipped_and_reported() {
    let mut bytes = object_stream_file();
    let mut other = b"%PDF-1.5\n".to_vec();
    other.extend_from_slice(
        b"9 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Filter /LZWDecode /Length 3 >>\nstream\nabc\nendstream\nendobj\n",
    );
    bytes.extend_from_slice(&other[9..]);
    let file = open(&bytes).expect("repaired");
    let report = report(&file);
    assert_eq!(report.skipped_object_streams, 1);
    assert!(report.is_incomplete());
    assert_eq!(file.root().map(|id| id.number), Some(1));
}

#[test]
fn a_stream_without_endstream_is_reported() {
    let mut bytes = without_startxref(build(&[CATALOG, PAGES]));
    bytes.extend_from_slice(b"3 0 obj\n<< /Length 999 >>\nstream\nno end\n4 0 obj (lost) endobj\n");
    let file = open(&bytes).expect("repaired");
    let report = report(&file);
    assert_eq!(report.unterminated_streams, 1);
    assert!(report.is_incomplete());
    assert_eq!(file.entry(4), None, "hidden by the unterminated stream");
}

#[test]
fn only_the_last_trailers_are_kept() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    for _ in 0..10_000 {
        bytes.extend_from_slice(b"trailer\n");
    }
    let scan = scan(&bytes, &mut budget()).expect("scanned");
    assert_eq!(scan.trailers.len(), MAX_TRAILERS);
}

#[test]
fn a_stream_does_not_take_the_header_of_an_earlier_object() {
    // Object 3 is not a stream; the bare `stream` after it has no header of
    // its own and its data are skipped up to `endstream`, not by the /Length
    // of object 3 (which would hide object 4).
    let mut bytes = without_startxref(build(&[CATALOG, PAGES]));
    bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /ObjStm /Length 40 >>\nendobj\nstream\nxx\nendstream\n4 0 obj (kept) endobj\n",
    );
    let file = open(&bytes).expect("repaired");
    assert!(file.entry(4).is_some());
    assert_eq!(report(&file).skipped_object_streams, 0);
}

#[test]
fn a_trailer_root_needs_the_right_generation() {
    // `/Root 1 5 R` names no object: the catalog is used instead.
    let bytes = without_startxref(build(&[CATALOG, PAGES]));
    let bytes = replace(&bytes, "/Root 1 0 R", "/Root 1 5 R");
    let file = open(&bytes).expect("repaired");
    assert_eq!(report(&file).trailer, RepairTrailer::Catalog);
    assert_eq!(
        file.root(),
        Some(ObjectId {
            number: 1,
            generation: 0
        })
    );
}

#[test]
fn decoded_bytes_are_charged_during_a_repair() {
    let bytes = object_stream_file();
    let opens = |limit: u64| {
        let mut budget = Budget::new(
            ResourceLimits {
                max_decoded_bytes: limit,
                ..ResourceLimits::default()
            },
            CancellationToken::new(),
        );
        open_with(&bytes, OpenOptions::default(), &mut budget)
    };
    let exact = (1..4096)
        .find(|limit| opens(*limit).is_ok())
        .expect("opens");
    assert_eq!(
        opens(exact - 1).err(),
        Some(Error::LimitExceeded {
            kind: LimitKind::DecodedBytes,
            limit: exact - 1
        })
    );
}

proptest! {
    /// Arbitrary bytes after a header never panic, and the scan stays within
    /// the object limit.
    #[test]
    fn arbitrary_bytes_are_handled(body in prop::collection::vec(any::<u8>(), 0..2048)) {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        bytes.extend_from_slice(&body);
        let mut budget = budget();
        if let Ok(file) = open_with(&bytes, OpenOptions::default(), &mut budget) {
            for (number, _) in file.entries().collect::<Vec<_>>() {
                let _ = file.resolve(ObjectId { number, generation: 0 }, &mut budget);
            }
        }
    }

    /// Damaging any byte of a valid file never panics; a file that still
    /// opens strictly is not repaired.
    #[test]
    fn damaged_files_are_handled(position in any::<prop::sample::Index>(), byte in any::<u8>()) {
        let mut bytes = build(&[CATALOG, PAGES, "<< /Length 3 >>\nstream\nabc\nendstream"]);
        let index = position.index(bytes.len());
        if let Some(b) = bytes.get_mut(index) {
            *b = byte;
        }
        let strict = open_with(&bytes, OpenOptions::strict(), &mut budget()).is_ok();
        if let Ok(file) = open(&bytes) {
            if strict {
                prop_assert!(file.repair().is_none() || matches!(
                    file.repair().map(|r| r.trailer),
                    Some(RepairTrailer::Original)
                ));
            }
            for (number, _) in file.entries().collect::<Vec<_>>() {
                let _ = file.resolve(ObjectId { number, generation: 0 }, &mut budget());
            }
        }
    }
}
