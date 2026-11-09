//! Cross-reference streams, object streams and hybrid files (ISO 32000-2,
//! 7.5.7 and 7.5.8).

use std::collections::BTreeMap;

use miniz_oxide::deflate::compress_to_vec_zlib;

use super::*;
use crate::limits::{CancellationToken, ResourceLimits};
use crate::source::MemorySource;

/// A file whose cross-reference section is a stream.
#[derive(Default)]
struct Spec<'a> {
    /// Uncompressed objects (number, body).
    direct: Vec<(u32, &'a str)>,
    /// Object stream number and its objects (number, body), in index order.
    objstm: Option<(u32, Vec<(u32, &'a str)>)>,
    /// Appended to the cross-reference stream dictionary.
    extra: &'a str,
    /// Appended to the object stream dictionary.
    objstm_extra: &'a str,
    /// Encode the cross-reference data with Flate and the PNG Up predictor.
    flate_png: bool,
    /// Replaces entries after they are computed: number -> (type, field 2, field 3).
    overrides: Vec<(u32, (u64, u64, u64))>,
}

const W: [usize; 3] = [1, 4, 2];

/// Returns the bytes and the offset of the cross-reference stream.
fn build(spec: &Spec<'_>) -> (Vec<u8>, u64) {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut entries: BTreeMap<u32, (u64, u64, u64)> = BTreeMap::new();
    entries.insert(0, (0, 0, 65535));
    for (number, body) in &spec.direct {
        entries.insert(*number, (1, out.len() as u64, 0));
        out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    if let Some((stream, objects)) = &spec.objstm {
        let mut header = String::new();
        let mut body = String::new();
        for (index, (number, object)) in objects.iter().enumerate() {
            header.push_str(&format!("{number} {} ", body.len()));
            body.push_str(object);
            body.push('\n');
            entries.insert(*number, (2, u64::from(*stream), index as u64));
        }
        let content = format!("{header}{body}");
        entries.insert(*stream, (1, out.len() as u64, 0));
        out.extend_from_slice(
            format!(
                "{stream} 0 obj\n<< /Type /ObjStm /N {} /First {} /Length {}{} >>\nstream\n{content}\nendstream\nendobj\n",
                objects.len(),
                header.len(),
                content.len(),
                spec.objstm_extra
            )
            .as_bytes(),
        );
    }
    let xref_number = entries.keys().max().copied().unwrap_or(0) + 1;
    let xref_offset = out.len() as u64;
    entries.insert(xref_number, (1, xref_offset, 0));
    for (number, entry) in &spec.overrides {
        entries.insert(*number, *entry);
    }
    let size = xref_number + 1;
    let record_len: usize = W.iter().sum();
    let mut records = Vec::new();
    for number in 0..size {
        let (kind, f2, f3) = entries.get(&number).copied().unwrap_or((0, 0, 0));
        for (value, width) in [(kind, W[0]), (f2, W[1]), (f3, W[2])] {
            records.extend_from_slice(&value.to_be_bytes()[8 - width..]);
        }
    }
    let (data, filter) = if spec.flate_png {
        let mut predicted = Vec::new();
        let mut previous = vec![0_u8; record_len];
        for row in records.chunks(record_len) {
            predicted.push(2);
            predicted.extend(row.iter().zip(&previous).map(|(x, up)| x.wrapping_sub(*up)));
            previous = row.to_vec();
        }
        (
            compress_to_vec_zlib(&predicted, 6),
            format!(" /Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns {record_len} >>"),
        )
    } else {
        (records, String::new())
    };
    out.extend_from_slice(
        format!(
            "{xref_number} 0 obj\n<< /Type /XRef /Size {size} /W [1 4 2] /Root 1 0 R{}{filter} /Length {} >>\nstream\n",
            spec.extra,
            data.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(&data);
    out.extend_from_slice(
        format!("\nendstream\nendobj\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    (out, xref_offset)
}

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

fn open(bytes: &[u8]) -> Result<PdfFile<MemorySource>, Error> {
    let source = MemorySource::from_vec(bytes.to_vec(), &ResourceLimits::default())?;
    PdfFile::open_strict(source, &mut budget())
}

fn kind(result: Result<PdfFile<MemorySource>, Error>) -> Option<MalformedKind> {
    match result {
        Err(Error::Malformed { kind, .. }) => Some(kind),
        other => panic!("expected Malformed, got {other:?}"),
    }
}

fn id(number: u32, generation: u16) -> ObjectId {
    ObjectId { number, generation }
}

fn replace(bytes: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let at = bytes
        .windows(from.len())
        .position(|w| w == from)
        .expect("pattern present");
    [&bytes[..at], to, &bytes[at + from.len()..]].concat()
}

const CATALOG: &str = "<< /Type /Catalog /Pages 2 0 R >>";
const PAGES: &str = "<< /Type /Pages /Kids [3 0 R] /Count 1 >>";
const PAGE: &str = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Contents 4 0 R >>";
const CONTENT: &str = "<< /Length 5 0 R >>\nstream\n0 0 m\nendstream";

/// Catalog, pages and the content length (5) compressed in object stream 6;
/// the page (3) and its content stream (4) uncompressed.
fn typical(flate_png: bool) -> Spec<'static> {
    Spec {
        direct: vec![(3, PAGE), (4, CONTENT)],
        objstm: Some((6, vec![(1, CATALOG), (2, PAGES), (5, "5")])),
        flate_png,
        ..Spec::default()
    }
}

#[test]
fn cross_reference_and_object_streams() {
    for flate_png in [false, true] {
        let (bytes, xref) = build(&typical(flate_png));
        let file = open(&bytes).expect("valid file");
        assert_eq!(file.startxref(), xref);
        assert_eq!(file.revisions(), 1);
        assert_eq!(
            file.sections().first().map(|s| s.kind),
            Some(SectionKind::Stream)
        );
        assert_eq!(
            file.entry(1),
            Some(XrefEntry::Compressed {
                stream: 6,
                index: 0
            })
        );
        assert_eq!(
            file.entry(5),
            Some(XrefEntry::Compressed {
                stream: 6,
                index: 2
            })
        );
        assert_eq!(file.root(), Some(id(1, 0)));
        assert_eq!(
            file.trailer().get(b"Type"),
            Some(&Object::Name(b"XRef".to_vec()))
        );
        let catalog = file.resolve(id(1, 0), &mut budget());
        let Ok(IndirectValue::Object(Object::Dictionary(catalog))) = catalog else {
            panic!("catalog: {catalog:?}");
        };
        assert_eq!(
            catalog.get(b"Type"),
            Some(&Object::Name(b"Catalog".to_vec()))
        );
        // A compressed object has generation 0: another generation is null.
        assert_eq!(
            file.resolve(id(1, 1), &mut budget()),
            Ok(IndirectValue::Object(Object::Null))
        );
        // The content stream's /Length is a compressed integer.
        let Ok(IndirectValue::Stream(content)) = file.resolve(id(4, 0), &mut budget()) else {
            panic!("content stream");
        };
        assert_eq!(
            file.decoded_stream_data(&content, &mut budget()),
            Ok(b"0 0 m".to_vec())
        );
    }
}

#[test]
fn decoded_object_streams_are_cached() {
    let (bytes, _) = build(&typical(false));
    let file = open(&bytes).expect("valid file");
    // The first access decodes the object stream (copied, unfiltered) and
    // parses the catalog; the second only parses the catalog again.
    let mut b = budget();
    assert!(file.resolve(id(1, 0), &mut b).is_ok());
    let first = b.decoded_bytes();
    assert!(file.resolve(id(1, 0), &mut b).is_ok());
    let second = b.decoded_bytes() - first;
    let objstm_len = {
        let Some(XrefEntry::InUse { offset, .. }) = file.entry(6) else {
            panic!("object stream entry");
        };
        let text = String::from_utf8_lossy(&bytes[offset as usize..]).into_owned();
        let at = text.find("/Length ").expect("length") + 8;
        text[at..]
            .split(' ')
            .next()
            .and_then(|n| n.parse::<u64>().ok())
            .expect("number")
    };
    assert!(
        first >= objstm_len + second,
        "first access decodes the stream"
    );
    // Other objects of the same stream do not decode it again.
    let before = b.decoded_bytes();
    assert!(file.resolve(id(2, 0), &mut b).is_ok());
    assert!(file.resolve(id(5, 0), &mut b).is_ok());
    assert!(
        b.decoded_bytes() - before < objstm_len,
        "object stream decoded once"
    );
}

#[test]
fn invalid_cross_reference_streams() {
    let (bytes, _) = build(&typical(false));
    let check = |from: &[u8], to: &[u8]| kind(open(&replace(&bytes, from, to)));
    let bad = Some(MalformedKind::InvalidXrefStream);
    assert_eq!(check(b"/W [1 4 2]", b"/W [1 4]  "), bad);
    assert_eq!(check(b"/W [1 4 2]", b"/W [1 9 2]"), bad);
    assert_eq!(check(b"/W [1 4 2]", b"/W [1 0 2]"), bad);
    // Data length must match /Size and /W exactly.
    assert_eq!(check(b"/W [1 4 2]", b"/W [1 4 3]"), bad);
    assert_eq!(check(b"/Type /XRef /Size 8", b"/Type /XRef /Size 9"), bad);
    // /Index pairs; here one subsection that is too long for the data.
    let (with_index, _) = build(&Spec {
        extra: " /Index [0 9]",
        ..typical(false)
    });
    assert_eq!(kind(open(&with_index)), bad);
    let (odd_index, _) = build(&Spec {
        extra: " /Index [0]",
        ..typical(false)
    });
    assert_eq!(kind(open(&odd_index)), bad);
    // The dictionary of a cross-reference stream is direct, /Length included.
    let length_at = bytes
        .windows(9)
        .rposition(|w| w == b"/Length 5")
        .expect("xref /Length");
    let mut indirect = bytes.clone();
    indirect.splice(length_at..length_at + 10, b"/Length 5 0 R".iter().copied());
    assert_eq!(kind(open(&indirect)), bad);
    // Not /Type /XRef: not a cross-reference section at all.
    assert_eq!(
        check(b"/Type /XRef", b"/Type /XRaf"),
        Some(MalformedKind::MissingXref)
    );
    // Encryption in the stream dictionary.
    let (encrypted, _) = build(&Spec {
        extra: " /Encrypt << >>",
        ..typical(false)
    });
    assert_eq!(
        open(&encrypted).err(),
        Some(Error::Unsupported(Capability::Encryption))
    );
}

#[test]
fn invalid_compressed_entries() {
    let entry_error = |overrides: Vec<(u32, (u64, u64, u64))>| {
        let (bytes, _) = build(&Spec {
            overrides,
            ..typical(false)
        });
        kind(open(&bytes))
    };
    let bad = Some(MalformedKind::InvalidXrefEntry);
    // In an object stream that is itself compressed, or in itself.
    assert_eq!(entry_error(vec![(5, (2, 1, 0))]), bad);
    assert_eq!(entry_error(vec![(5, (2, 5, 0))]), bad);
    // Stream number 0.
    assert_eq!(entry_error(vec![(5, (2, 0, 0))]), bad);
    // An unknown entry type is a reference to null: a free entry.
    let (bytes, _) = build(&Spec {
        overrides: vec![(3, (7, 1, 2))],
        ..typical(false)
    });
    let file = open(&bytes).expect("valid file");
    assert!(matches!(file.entry(3), Some(XrefEntry::Free { .. })));
    assert_eq!(
        file.resolve(id(3, 0), &mut budget()),
        Ok(IndirectValue::Object(Object::Null))
    );
}

#[test]
fn invalid_object_streams() {
    let resolve_error = |bytes: &[u8], number| {
        let file = open(bytes).expect("structure valid");
        match file.resolve(id(number, 0), &mut budget()) {
            Err(Error::Malformed { kind, .. }) => Some(kind),
            other => panic!("expected Malformed, got {other:?}"),
        }
    };
    let (bytes, _) = build(&typical(false));
    assert_eq!(
        resolve_error(&replace(&bytes, b"/Type /ObjStm", b"/Type /ObjStx"), 1),
        Some(MalformedKind::InvalidObjectStream)
    );
    // An index beyond /N, and a header listing another object number.
    let (beyond, _) = build(&Spec {
        overrides: vec![(5, (2, 6, 7))],
        ..typical(false)
    });
    assert_eq!(
        resolve_error(&beyond, 5),
        Some(MalformedKind::InvalidObjectStream)
    );
    let (swapped, _) = build(&Spec {
        overrides: vec![(5, (2, 6, 0))],
        ..typical(false)
    });
    assert_eq!(
        resolve_error(&swapped, 5),
        Some(MalformedKind::ObjectIdMismatch)
    );
}

#[test]
fn hybrid_file_reads_the_table_then_its_stream() {
    // Object 1 (catalog) in the table; object 2 (pages) compressed in object
    // stream 3, listed only by the /XRefStm stream (object 4).
    let mut out = b"%PDF-1.5\n".to_vec();
    let catalog_at = out.len();
    out.extend_from_slice(format!("1 0 obj\n{CATALOG}\nendobj\n").as_bytes());
    let objstm_body = format!("2 0 {}", "<< /Type /Pages /Kids [] /Count 0 >>");
    let objstm_at = out.len();
    out.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length {} >>\nstream\n{objstm_body}\nendstream\nendobj\n",
            objstm_body.len()
        )
        .as_bytes(),
    );
    let stream_at = out.len();
    // Entries 2 (compressed in 3, index 0); /Index [2 1]; W [1 4 2].
    let records = [2_u8, 0, 0, 0, 3, 0, 0];
    out.extend_from_slice(
        b"4 0 obj\n<< /Type /XRef /Size 5 /W [1 4 2] /Index [2 1] /Length 7 >>\nstream\n",
    );
    out.extend_from_slice(&records);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    let table_at = out.len();
    out.extend_from_slice(
        format!(
            "xref\n0 2\n0000000000 65535 f \n{catalog_at:010} 00000 n \n3 2\n{objstm_at:010} 00000 n \n{stream_at:010} 00000 n \n\
             trailer\n<< /Size 5 /Root 1 0 R /XRefStm {stream_at} >>\nstartxref\n{table_at}\n%%EOF\n"
        )
        .as_bytes(),
    );
    let file = open(&out).expect("valid hybrid file");
    assert_eq!(file.revisions(), 1);
    let kinds: Vec<SectionKind> = file.sections().iter().map(|s| s.kind).collect();
    assert_eq!(kinds, vec![SectionKind::Table, SectionKind::HybridStream]);
    let pages = file.resolve(id(2, 0), &mut budget());
    assert!(
        matches!(pages, Ok(IndirectValue::Object(Object::Dictionary(_)))),
        "{pages:?}"
    );
    // /XRefStm pointing back at the table itself is a cycle.
    let looped = replace(
        &out,
        format!("/XRefStm {stream_at}").as_bytes(),
        format!("/XRefStm {table_at}").as_bytes(),
    );
    assert_eq!(kind(open(&looped)), Some(MalformedKind::XrefPrevCycle));
    // Producers usually list compressed objects as free in the table: the
    // stream entry of the same revision replaces the free entry.
    let free_listed = replace(
        &out,
        format!("{catalog_at:010} 00000 n \n3 2\n").as_bytes(),
        format!("{catalog_at:010} 00000 n \n2 3\n0000000000 00000 f \n").as_bytes(),
    );
    let file = open(&free_listed).expect("valid hybrid file");
    assert_eq!(
        file.entry(2),
        Some(XrefEntry::Compressed {
            stream: 3,
            index: 0
        })
    );
    let pages = file.resolve(id(2, 0), &mut budget());
    assert!(
        matches!(pages, Ok(IndirectValue::Object(Object::Dictionary(_)))),
        "{pages:?}"
    );
}

#[test]
fn object_stream_filters_cannot_recurse() {
    // Object stream 6 takes its /Filter from object 7, compressed in 6 itself:
    // decoding 6 would need 6. It must fail cleanly, not exhaust the stack.
    let (bytes, _) = build(&Spec {
        direct: vec![(3, PAGE), (4, CONTENT)],
        objstm: Some((
            6,
            vec![(1, CATALOG), (2, PAGES), (5, "5"), (7, "/FlateDecode")],
        )),
        objstm_extra: " /Filter 7 0 R",
        ..Spec::default()
    });
    let file = open(&bytes).expect("structure valid");
    let result = file.resolve(id(1, 0), &mut budget());
    assert!(matches!(
        result,
        Err(Error::Malformed {
            kind: MalformedKind::InvalidFilter,
            ..
        })
    ));
}
