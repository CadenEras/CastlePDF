//! What `pdf-write` writes reads back the same through `pdf-syntax`
//! (ADR-0008, tranche 4.1): any object, as a direct object; a whole file,
//! opened by the strict reader (no repair).

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a test failure"
)]

use pdf_syntax::{
    Budget, CancellationToken, Dictionary, IndirectValue, MemorySource, Object, ObjectId, Parser,
    PdfFile, ResourceLimits,
};
use pdf_write::{Error, FileWriter, write_object};
use proptest::prelude::*;

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

fn name() -> impl Strategy<Value = Vec<u8>> {
    // Any bytes but 0, which no name may hold.
    prop::collection::vec(1u8..=255, 1..12)
}

fn object() -> impl Strategy<Value = Object> {
    let leaf = prop_oneof![
        Just(Object::Null),
        any::<bool>().prop_map(Object::Boolean),
        any::<i64>().prop_map(Object::Integer),
        (-1e15f64..1e15).prop_map(Object::Real),
        prop::num::f64::NORMAL.prop_map(Object::Real),
        prop::collection::vec(any::<u8>(), 0..24).prop_map(Object::String),
        name().prop_map(Object::Name),
        (1i64..1_000_000, 0i64..65_535)
            .prop_map(|(n, g)| { Object::Reference(ObjectId::new(n, g).expect("valid id")) }),
    ];
    leaf.prop_recursive(4, 48, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Object::Array),
            prop::collection::vec((name(), inner), 0..6).prop_map(|entries| {
                let mut dictionary = Dictionary::new();
                for (key, value) in entries {
                    dictionary.insert(key, value);
                }
                Object::Dictionary(dictionary)
            }),
        ]
    })
}

proptest! {
    #[test]
    fn objects_read_back_as_written(object in object()) {
        let mut bytes = Vec::new();
        write_object(&object, &mut bytes).expect("write");
        let read = Parser::new(&bytes).parse_object(&mut budget()).expect("read");
        prop_assert_eq!(read, object);
    }
}

/// A one-page file: catalog 1, pages 2, page 3, content stream 4, and a
/// free number 6 between it and an info dictionary 7.
fn sample() -> Vec<u8> {
    let reference = |n: i64| Object::Reference(ObjectId::new(n, 0).expect("id"));
    let dictionary = |entries: Vec<(&str, Object)>| {
        let mut d = Dictionary::new();
        for (k, v) in entries {
            d.insert(k.as_bytes().to_vec(), v);
        }
        d
    };
    let name = |n: &str| Object::Name(n.as_bytes().to_vec());
    let mut writer = FileWriter::new(1, 7, 1 << 20).expect("writer");
    writer
        .stream(
            4,
            &dictionary(vec![("Length", reference(9))]),
            b"0 0 1 rg 10 10 80 80 re f\n(\\)) Tj",
        )
        .expect("content");
    writer
        .object(
            3,
            &Object::Dictionary(dictionary(vec![
                ("Type", name("Page")),
                ("Parent", reference(2)),
                (
                    "MediaBox",
                    Object::Array(vec![
                        Object::Integer(0),
                        Object::Integer(0),
                        Object::Real(100.5),
                        Object::Integer(100),
                    ]),
                ),
                ("Contents", reference(4)),
            ])),
        )
        .expect("page");
    writer
        .object(
            2,
            &Object::Dictionary(dictionary(vec![
                ("Type", name("Pages")),
                ("Kids", Object::Array(vec![reference(3)])),
                ("Count", Object::Integer(1)),
            ])),
        )
        .expect("pages");
    writer
        .object(
            1,
            &Object::Dictionary(dictionary(vec![
                ("Type", name("Catalog")),
                ("Pages", reference(2)),
            ])),
        )
        .expect("catalog");
    writer
        .object(
            7,
            &Object::Dictionary(dictionary(vec![(
                "Title",
                Object::String(b"Caf\xE9 (1)".to_vec()),
            )])),
        )
        .expect("info");
    writer
        .finish(&dictionary(vec![
            ("Root", reference(1)),
            ("Info", reference(7)),
        ]))
        .expect("finish")
}

#[test]
fn a_written_file_opens_strictly_and_reads_back() {
    let bytes = sample();
    let source = MemorySource::from_vec(bytes, &ResourceLimits::default()).expect("source");
    let file = PdfFile::open_strict(source, &mut budget()).expect("strict open");
    let trailer = file.trailer();
    assert_eq!(trailer.get(b"Size"), Some(&Object::Integer(8)));
    assert_eq!(
        trailer.get(b"Root"),
        Some(&Object::Reference(ObjectId::new(1, 0).expect("id")))
    );
    let Some(Object::Array(id)) = trailer.get(b"ID") else {
        panic!("no /ID");
    };
    assert_eq!(id.len(), 2);
    // The page reads back with its real coordinate; the stream with its
    // data and a direct /Length (the indirect one was replaced).
    let Ok(IndirectValue::Object(Object::Dictionary(page))) =
        file.resolve(ObjectId::new(3, 0).expect("id"), &mut budget())
    else {
        panic!("page");
    };
    assert_eq!(
        page.get(b"MediaBox")
            .and_then(Object::as_array)
            .and_then(|a| a.get(2)),
        Some(&Object::Real(100.5))
    );
    let Ok(IndirectValue::Stream(stream)) =
        file.resolve(ObjectId::new(4, 0).expect("id"), &mut budget())
    else {
        panic!("stream");
    };
    assert_eq!(stream.dictionary.get(b"Length"), Some(&Object::Integer(33)));
    let data = file
        .decoded_stream_data(&stream, &mut budget())
        .expect("data");
    assert_eq!(data, b"0 0 1 rg 10 10 80 80 re f\n(\\)) Tj");
    let Ok(IndirectValue::Object(Object::Dictionary(info))) =
        file.resolve(ObjectId::new(7, 0).expect("id"), &mut budget())
    else {
        panic!("info");
    };
    assert_eq!(
        info.get(b"Title"),
        Some(&Object::String(b"Caf\xE9 (1)".to_vec()))
    );
    // Number 6 was not written: a free entry, which reads as null.
    assert_eq!(
        file.resolve(ObjectId::new(6, 0).expect("id"), &mut budget())
            .ok(),
        Some(IndirectValue::Object(Object::Null))
    );
}

#[test]
fn the_same_objects_give_the_same_file() {
    assert_eq!(sample(), sample());
}

#[test]
fn writes_stop_at_their_limits() {
    // The size limit: a file that would exceed it is refused, at exactly
    // the limit it is written.
    let size = sample().len() as u64;
    let write = |limit: u64| -> Result<Vec<u8>, Error> {
        let mut writer = FileWriter::new(1, 7, limit)?;
        writer.stream(
            4,
            &Dictionary::new(),
            b"0 0 1 rg 10 10 80 80 re f\n(\\)) Tj",
        )?;
        Ok(Vec::new())
    };
    assert!(write(size).is_ok());
    assert_eq!(write(40).err(), Some(Error::TooLarge { limit: 40 }));
    // Object numbers: 0 and a number given twice are refused.
    let mut writer = FileWriter::new(1, 7, 1 << 20).expect("writer");
    assert_eq!(
        writer.object(0, &Object::Null),
        Err(Error::InvalidObjectNumber(0))
    );
    writer.object(5, &Object::Null).expect("first");
    assert_eq!(
        writer.object(5, &Object::Null),
        Err(Error::InvalidObjectNumber(5))
    );
}
