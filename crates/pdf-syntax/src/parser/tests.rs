use proptest::prelude::*;

use super::*;
use crate::limits::{CancellationToken, LimitKind, ResourceLimits};

fn budget() -> Budget {
    Budget::new(ResourceLimits::default(), CancellationToken::new())
}

fn limited(max_objects_visited: u64, max_nesting_depth: u32) -> Budget {
    Budget::new(
        ResourceLimits {
            max_objects_visited,
            max_nesting_depth,
            ..ResourceLimits::default()
        },
        CancellationToken::new(),
    )
}

fn object(input: &[u8]) -> Result<Object, Error> {
    Parser::new(input).parse_object(&mut budget())
}

fn indirect(input: &[u8]) -> Result<IndirectObject, Error> {
    Parser::new(input).parse_indirect_object(&mut budget(), &mut no_indirect_length)
}

fn no_indirect_length(id: ObjectId, _: &mut Budget) -> Result<i64, Error> {
    panic!("unexpected length resolution for {id:?}")
}

fn name(bytes: &[u8]) -> Object {
    Object::Name(bytes.to_vec())
}

fn id(number: u32, generation: u16) -> ObjectId {
    ObjectId { number, generation }
}

fn bad(offset: u64, kind: MalformedKind) -> Result<Object, Error> {
    Err(Error::Malformed { offset, kind })
}

fn stream(input: &[u8]) -> Result<Stream, Error> {
    match indirect(input)?.value {
        IndirectValue::Stream(stream) => Ok(stream),
        IndirectValue::Object(other) => panic!("expected a stream, got {other:?}"),
    }
}

#[test]
fn scalars() {
    assert_eq!(object(b"null"), Ok(Object::Null));
    assert_eq!(object(b"true"), Ok(Object::Boolean(true)));
    assert_eq!(object(b" false "), Ok(Object::Boolean(false)));
    assert_eq!(object(b"-17"), Ok(Object::Integer(-17)));
    assert_eq!(object(b"3.5"), Ok(Object::Real(3.5)));
    assert_eq!(object(b"(a\\)b)"), Ok(Object::String(b"a)b".to_vec())));
    assert_eq!(object(b"<4142>"), Ok(Object::String(b"AB".to_vec())));
    assert_eq!(object(b"/A#20B"), Ok(name(b"A B")));
}

#[test]
#[allow(
    clippy::approx_constant,
    reason = "3.14 is the literal from the ISO example"
)]
fn containers_and_references() {
    // ISO 32000-2, 7.3.6 and 7.3.7 examples.
    let array = object(b"[549 3.14 false (Ralph) /SomeName]");
    assert_eq!(
        array,
        Ok(Object::Array(vec![
            Object::Integer(549),
            Object::Real(3.14),
            Object::Boolean(false),
            Object::String(b"Ralph".to_vec()),
            name(b"SomeName"),
        ]))
    );
    let dict = object(
        b"<< /Type /Example /Version 0.01 /IntegerItem 12 /StringItem (a string)\n\
          /Subdictionary << /Item1 0.4 /Item2 true /LastItem (not!) >> >>",
    );
    let Ok(Object::Dictionary(dict)) = dict else {
        panic!("expected a dictionary: {dict:?}");
    };
    assert_eq!(dict.len(), 5);
    assert_eq!(dict.get(b"Type"), Some(&name(b"Example")));
    assert_eq!(
        dict.get(b"IntegerItem").and_then(Object::as_integer),
        Some(12)
    );
    let sub = dict.get(b"Subdictionary").and_then(Object::as_dictionary);
    assert_eq!(sub.map(Dictionary::len), Some(3));

    assert_eq!(
        object(b"[1 0 R 2 3 R 4 5 6 R]"),
        Ok(Object::Array(vec![
            Object::Reference(id(1, 0)),
            Object::Reference(id(2, 3)),
            Object::Integer(4),
            Object::Reference(id(5, 6)),
        ]))
    );
    assert_eq!(
        object(b"<</Parent 12 0 R/Kids[3 0 R]>>")
            .ok()
            .as_ref()
            .and_then(Object::as_dictionary)
            .and_then(|d| d.get(b"Parent"))
            .and_then(Object::as_reference),
        Some(id(12, 0))
    );
    // Two integers not followed by R stay integers, even at the end of input.
    assert_eq!(object(b"7 8"), Ok(Object::Integer(7)));
    assert_eq!(object(b"[]"), Ok(Object::Array(vec![])));
    assert_eq!(object(b"<<>>"), Ok(Object::Dictionary(Dictionary::new())));
}

#[test]
fn invalid_references() {
    assert_eq!(object(b"0 0 R"), bad(0, MalformedKind::InvalidObjectId));
    assert_eq!(object(b"[1 -1 R]"), bad(1, MalformedKind::InvalidObjectId));
    assert_eq!(object(b"1 65536 R"), bad(0, MalformedKind::InvalidObjectId));
    assert_eq!(
        object(b"4294967296 0 R"),
        bad(0, MalformedKind::InvalidObjectId)
    );
    assert_eq!(
        object(b"4294967295 65535 R"),
        Ok(Object::Reference(id(u32::MAX, u16::MAX)))
    );
    assert_eq!(object(b"R"), bad(0, MalformedKind::UnexpectedToken));
    assert_eq!(object(b"[1 R]"), bad(3, MalformedKind::UnexpectedToken));
}

#[test]
fn malformed_containers() {
    assert_eq!(object(b"]"), bad(0, MalformedKind::UnexpectedToken));
    assert_eq!(object(b"[1>>"), bad(2, MalformedKind::UnexpectedToken));
    assert_eq!(object(b"<<]"), bad(2, MalformedKind::UnexpectedToken));
    assert_eq!(object(b"{1}"), bad(0, MalformedKind::UnexpectedToken));
    assert_eq!(object(b"[obj]"), bad(1, MalformedKind::UnexpectedToken));
    assert_eq!(
        object(b"<< /A >>"),
        bad(6, MalformedKind::MissingDictionaryValue)
    );
    assert_eq!(
        object(b"<< 1 2 >>"),
        bad(3, MalformedKind::DictionaryKeyNotName)
    );
    assert_eq!(
        object(b"<< /A 1 /A 2 >>"),
        bad(8, MalformedKind::DuplicateKey)
    );
    // Escapes are resolved before comparing keys.
    assert_eq!(
        object(b"<< /A 1 /#41 2 >>"),
        bad(8, MalformedKind::DuplicateKey)
    );
    // Lexer errors keep their own offset.
    assert_eq!(object(b"[1 <zz>]"), bad(4, MalformedKind::InvalidHexDigit));
}

#[test]
fn truncation_reports_the_innermost_open_container() {
    assert_eq!(object(b""), Err(Error::Truncated { offset: 0 }));
    assert_eq!(object(b"  "), Err(Error::Truncated { offset: 0 }));
    assert_eq!(object(b"[1 [2"), Err(Error::Truncated { offset: 3 }));
    assert_eq!(object(b"<< /A [1"), Err(Error::Truncated { offset: 6 }));
    assert_eq!(object(b"<< /A"), Err(Error::Truncated { offset: 0 }));
    assert_eq!(object(b"(abc"), Err(Error::Truncated { offset: 0 }));
}

#[test]
fn nesting_depth_exact_limit_and_limit_plus_one() {
    let nested = |depth: usize| [vec![b'['; depth], vec![b']'; depth]].concat();
    assert!(
        Parser::new(&nested(3))
            .parse_object(&mut limited(1000, 3))
            .is_ok()
    );
    assert_eq!(
        Parser::new(&nested(4)).parse_object(&mut limited(1000, 3)),
        Err(Error::LimitExceeded {
            kind: LimitKind::NestingDepth,
            limit: 3
        })
    );
    assert_eq!(
        Parser::new(b"<</A[<</B[]>>]>>").parse_object(&mut limited(1000, 3)),
        Err(Error::LimitExceeded {
            kind: LimitKind::NestingDepth,
            limit: 3
        })
    );
}

#[test]
fn nesting_is_capped_even_when_the_limit_is_lifted() {
    // `Object` drops, clones and compares recursively: the hard cap keeps a
    // complete deep object within the stack whatever max_nesting_depth says.
    let nested = |depth: usize| [vec![b'['; depth], vec![b']'; depth]].concat();
    let cap = MAX_SUPPORTED_NESTING_DEPTH as usize;
    let parsed = Parser::new(&nested(cap)).parse_object(&mut limited(u64::MAX, u32::MAX));
    assert!(parsed.is_ok());
    assert_eq!(parsed.clone(), parsed);
    drop(parsed);
    let refused = Err(Error::LimitExceeded {
        kind: LimitKind::NestingDepth,
        limit: u64::from(MAX_SUPPORTED_NESTING_DEPTH),
    });
    assert_eq!(
        Parser::new(&nested(cap + 1)).parse_object(&mut limited(u64::MAX, u32::MAX)),
        refused
    );
    assert_eq!(
        Parser::new(&nested(200_000)).parse_object(&mut limited(u64::MAX, u32::MAX)),
        refused
    );
}

#[test]
fn a_container_key_is_refused_before_its_content() {
    let mut b = budget();
    assert_eq!(
        Parser::new(b"<< [1 2 3] 2 >>").parse_object(&mut b),
        bad(3, MalformedKind::DictionaryKeyNotName)
    );
    // Only the dictionary itself was charged.
    assert_eq!(b.objects_visited(), 1);
    assert_eq!(
        object(b"<< /A 1 << /B 2 >> 3 >>"),
        bad(8, MalformedKind::DictionaryKeyNotName)
    );
}

#[test]
fn a_complete_object_is_returned_before_a_faulty_token() {
    assert_eq!(object(b"5 )"), Ok(Object::Integer(5)));
    assert_eq!(object(b"5 0 <zz>"), Ok(Object::Integer(5)));
    let mut parser = Parser::new(b"5 0 <zz>");
    let mut b = budget();
    assert_eq!(parser.parse_object(&mut b), Ok(Object::Integer(5)));
    assert_eq!(parser.parse_object(&mut b), Ok(Object::Integer(0)));
    assert_eq!(
        parser.parse_object(&mut b),
        bad(5, MalformedKind::InvalidHexDigit)
    );
}

#[test]
fn dictionary_keys_count_as_objects() {
    // `<< /A 1 /B 2 >>`: the dictionary, two keys and two values.
    let mut b = budget();
    assert!(Parser::new(b"<< /A 1 /B 2 >>").parse_object(&mut b).is_ok());
    assert_eq!(b.objects_visited(), 5);
}

#[test]
fn object_budget_exact_limit_and_limit_plus_one() {
    // `[1 2 0 R /K]`: array, integer, reference, name = 4 objects.
    let input = b"[1 2 0 R /K]";
    let mut b = limited(4, 64);
    assert!(Parser::new(input).parse_object(&mut b).is_ok());
    assert_eq!(b.objects_visited(), 4);
    assert_eq!(
        Parser::new(input).parse_object(&mut limited(3, 64)),
        Err(Error::LimitExceeded {
            kind: LimitKind::ObjectsVisited,
            limit: 3
        })
    );
}

#[test]
fn cancellation_stops_parsing() {
    let token = CancellationToken::new();
    let mut b = Budget::new(ResourceLimits::default(), token.clone());
    token.cancel();
    assert_eq!(
        Parser::new(b"[1 2]").parse_object(&mut b),
        Err(Error::Cancelled)
    );
}

#[test]
fn sequential_objects_and_position() {
    let mut b = budget();
    let mut parser = Parser::new(b"1 2 [3] 4");
    assert_eq!(parser.parse_object(&mut b), Ok(Object::Integer(1)));
    // `2` and `[` were read ahead to test for `1 2 R`; position still points at `2`.
    assert_eq!(parser.position(), 2);
    assert_eq!(parser.parse_object(&mut b), Ok(Object::Integer(2)));
    assert_eq!(
        parser.parse_object(&mut b),
        Ok(Object::Array(vec![Object::Integer(3)]))
    );
    assert_eq!(parser.parse_object(&mut b), Ok(Object::Integer(4)));
    assert_eq!(
        parser.parse_object(&mut b),
        Err(Error::Truncated { offset: 9 })
    );
    assert_eq!(parser.seek(4), Ok(()));
    assert_eq!(
        parser.parse_object(&mut b),
        Ok(Object::Array(vec![Object::Integer(3)]))
    );
    assert_eq!(parser.seek(10), Err(Error::Truncated { offset: 10 }));
}

#[test]
fn indirect_objects() {
    assert_eq!(
        indirect(b"12 0 obj (Brillig) endobj"),
        Ok(IndirectObject {
            id: id(12, 0),
            value: IndirectValue::Object(Object::String(b"Brillig".to_vec())),
        })
    );
    assert_eq!(
        indirect(b"8 3 obj\n<< /K 1 0 R >>\nendobj").map(|o| o.id),
        Ok(id(8, 3))
    );
    assert_eq!(
        indirect(b"5 0 obj 1 2 R endobj").map(|o| o.value),
        Ok(IndirectValue::Object(Object::Reference(id(1, 2))))
    );
    // Header errors.
    let err = |offset, kind| Err(Error::Malformed { offset, kind });
    assert_eq!(
        indirect(b"0 0 obj null endobj"),
        err(0, MalformedKind::InvalidObjectId)
    );
    assert_eq!(
        indirect(b"1 70000 obj null endobj"),
        err(0, MalformedKind::InvalidObjectId)
    );
    assert_eq!(
        indirect(b"/A 0 obj null endobj"),
        err(0, MalformedKind::InvalidObjectId)
    );
    assert_eq!(indirect(b"1 0 R"), err(4, MalformedKind::MissingObjKeyword));
    assert_eq!(
        indirect(b"1 0 obj null"),
        Err(Error::Truncated { offset: 0 })
    );
    assert_eq!(indirect(b"1 0"), Err(Error::Truncated { offset: 0 }));
    assert_eq!(
        indirect(b"1 0 obj null 2 0 obj"),
        err(13, MalformedKind::MissingEndobj)
    );
    assert_eq!(
        indirect(b"1 0 obj 5 stream\nendstream endobj"),
        err(10, MalformedKind::MissingEndobj)
    );
}

#[test]
fn stream_data_is_located_by_length() {
    let input = b"4 0 obj << /Length 5 >> stream\nhello\nendstream\nendobj";
    let s = stream(input);
    assert_eq!(s.as_ref().map(|s| s.data.clone()), Ok(31..36));
    assert_eq!(input.get(31..36), Some(&b"hello"[..]));
    // CRLF after `stream`; data that contains the word `endstream` itself.
    let input = b"4 0 obj<</Length 9>>stream\r\nendstreamendstream endobj";
    assert_eq!(stream(input).map(|s| s.data), Ok(28..37));
    // Empty data, and `endstream` immediately after the data.
    assert_eq!(
        stream(b"1 0 obj<</Length 0>>stream\nendstream endobj").map(|s| s.data),
        Ok(27..27)
    );
}

#[test]
fn stream_length_exact_and_off_by_one() {
    let with_length = |length: &str| {
        format!("1 0 obj << /Length {length} >> stream\nabcd\nendstream endobj").into_bytes()
    };
    // Header `1 0 obj << /Length 4 >> stream\n` is 31 bytes.
    assert_eq!(stream(&with_length("4")).map(|s| s.data), Ok(31..35));
    assert_eq!(
        stream(&with_length("3")),
        Err(Error::Malformed {
            offset: 34,
            kind: MalformedKind::MissingEndstream
        })
    );
    // ISO 32000-2 (7.3.8.1) excludes the EOL before `endstream` from /Length, but
    // that EOL is optional: a /Length that includes it cannot be told apart from
    // data ending in a line feed, so it is accepted.
    assert_eq!(stream(&with_length("5")).map(|s| s.data), Ok(31..36));
    assert_eq!(
        stream(&with_length("6")),
        Err(Error::Malformed {
            offset: 37,
            kind: MalformedKind::MissingEndstream
        })
    );
    // With a two-digit length, data starts at 32 and the input is 53 bytes long:
    // data ending exactly at the end leaves no room for `endstream`, one byte
    // more runs past the end.
    assert_eq!(
        stream(&with_length("21")),
        Err(Error::Truncated { offset: 53 })
    );
    assert_eq!(
        stream(&with_length("22")),
        Err(Error::Truncated { offset: 32 })
    );
    // Data running past the end of the input (the header grows with the digits).
    assert_eq!(
        stream(&with_length("1000")),
        Err(Error::Truncated { offset: 34 })
    );
    assert_eq!(
        stream(&with_length("9223372036854775807")),
        Err(Error::Truncated { offset: 49 })
    );
}

#[test]
fn invalid_stream_headers() {
    let err = |offset, kind| Err(Error::Malformed { offset, kind });
    // A lone CR, or a space before the EOL, is refused in strict mode.
    assert_eq!(
        stream(b"1 0 obj<</Length 0>>stream\rendstream endobj"),
        err(26, MalformedKind::MissingStreamEol)
    );
    assert_eq!(
        stream(b"1 0 obj<</Length 0>>stream \nendstream endobj"),
        err(26, MalformedKind::MissingStreamEol)
    );
    assert_eq!(
        stream(b"1 0 obj<</Length 0>>stream"),
        Err(Error::Truncated { offset: 26 })
    );
    assert_eq!(
        stream(b"1 0 obj<</Length 0>>stream\r"),
        Err(Error::Truncated { offset: 26 })
    );
    assert_eq!(
        stream(b"1 0 obj<<>>stream\nendstream endobj"),
        err(18, MalformedKind::InvalidStreamLength)
    );
    assert_eq!(
        stream(b"1 0 obj<</Length -1>>stream\nendstream endobj"),
        err(28, MalformedKind::InvalidStreamLength)
    );
    assert_eq!(
        stream(b"1 0 obj<</Length 1.0>>stream\nx endstream endobj"),
        err(29, MalformedKind::InvalidStreamLength)
    );
    assert_eq!(
        stream(b"1 0 obj<</Length 0>>stream\nendstream"),
        Err(Error::Truncated { offset: 0 })
    );
    assert_eq!(
        stream(b"1 0 obj<</Length 0>>stream\nendstream 2"),
        err(37, MalformedKind::MissingEndobj)
    );
    assert_eq!(
        stream(b"1 0 obj<</Length 0>>stream\nendstreamX endobj"),
        err(27, MalformedKind::MissingEndstream)
    );
    assert_eq!(
        stream(b"1 0 obj<</Length 0>>stream\nendstr"),
        Err(Error::Truncated { offset: 27 })
    );
    assert_eq!(
        stream(b"1 0 obj<</F (data.bin) /Length 0>>stream\nendstream endobj"),
        Err(Error::Unsupported(Capability::ExternalStreamData))
    );
}

#[test]
fn bytes_after_a_short_length_are_never_skipped() {
    // Data starts at 27; /Length 2 stops at 29, inside the real data.
    let err = |offset| {
        Err(Error::Malformed {
            offset,
            kind: MalformedKind::MissingEndstream,
        })
    };
    // White space and a comment are not skipped before `endstream`.
    assert_eq!(
        stream(b"1 0 obj<</Length 2>>stream\nab  \t\n%junk\nendstream endobj"),
        err(29)
    );
    // A delimiter, or a string that the lexer would decode and charge, is not
    // read as a token.
    assert_eq!(
        stream(b"1 0 obj<</Length 2>>stream\nab)\nendstream endobj"),
        err(29)
    );
    let mut b = budget();
    let input = b"1 0 obj<</Length 2>>stream\nab(hidden string)\nendstream endobj";
    assert_eq!(
        Parser::new(input)
            .parse_indirect_object(&mut b, &mut no_indirect_length)
            .map(|o| o.id),
        Err(Error::Malformed {
            offset: 29,
            kind: MalformedKind::MissingEndstream,
        })
    );
    // Only the name `Length` was decoded, not the hidden string.
    assert_eq!(b.decoded_bytes(), 6);
    // CR, LF and CRLF before `endstream` are each accepted once.
    for eol in ["\r", "\n", "\r\n"] {
        let input = format!("1 0 obj<</Length 2>>stream\nab{eol}endstream endobj");
        assert_eq!(stream(input.as_bytes()).map(|s| s.data), Ok(27..29));
    }
    assert_eq!(
        stream(b"1 0 obj<</Length 2>>stream\nab\n\nendstream endobj"),
        err(29)
    );
}

#[test]
fn resolved_lengths_are_checked_like_direct_ones() {
    let input = b"1 0 obj<</Length 7 0 R>>stream\nabc\nendstream endobj";
    let parse = |length: i64| {
        let mut resolver = move |_: ObjectId, _: &mut Budget| Ok(length);
        Parser::new(input).parse_indirect_object(&mut budget(), &mut resolver)
    };
    assert_eq!(
        parse(-1),
        Err(Error::Malformed {
            offset: 31,
            kind: MalformedKind::InvalidStreamLength
        })
    );
    assert_eq!(parse(i64::MAX), Err(Error::Truncated { offset: 31 }));
}

#[test]
fn cancellation_during_an_indirect_object() {
    // The resolver cancels the task; the parser stops at its next token.
    let token = CancellationToken::new();
    let mut b = Budget::new(ResourceLimits::default(), token.clone());
    let mut resolver = |_: ObjectId, _: &mut Budget| {
        token.cancel();
        Ok(3)
    };
    assert_eq!(
        Parser::new(b"1 0 obj<</Length 7 0 R>>stream\nabc\nendstream endobj")
            .parse_indirect_object(&mut b, &mut resolver),
        Err(Error::Cancelled)
    );
}

#[test]
fn indirect_length_goes_through_the_resolver() {
    let input = b"1 0 obj<</Length 7 0 R>>stream\nabc\nendstream endobj";
    let mut asked = Vec::new();
    let mut resolver = |id: ObjectId, _: &mut Budget| {
        asked.push(id);
        Ok(3)
    };
    let result = Parser::new(input).parse_indirect_object(&mut budget(), &mut resolver);
    assert_eq!(asked, vec![id(7, 0)]);
    let Ok(IndirectObject {
        value: IndirectValue::Stream(s),
        ..
    }) = result
    else {
        panic!("expected a stream: {result:?}");
    };
    assert_eq!(s.data, 31..34);

    let mut failing = |_: ObjectId, _: &mut Budget| Err(Error::Truncated { offset: 99 });
    assert_eq!(
        Parser::new(input).parse_indirect_object(&mut budget(), &mut failing),
        Err(Error::Truncated { offset: 99 })
    );
}

// --- Property tests ---

fn arb_object() -> impl Strategy<Value = Object> {
    let bytes = prop::collection::vec(any::<u8>(), 0..8);
    let name_bytes = prop::collection::vec(1u8..=255, 0..8);
    let leaf = prop_oneof![
        Just(Object::Null),
        any::<bool>().prop_map(Object::Boolean),
        any::<i64>().prop_map(Object::Integer),
        (-1.0e6f64..1.0e6).prop_map(Object::Real),
        bytes.prop_map(Object::String),
        name_bytes.clone().prop_map(Object::Name),
        (1u32.., any::<u16>())
            .prop_map(|(number, generation)| Object::Reference(ObjectId { number, generation })),
    ];
    leaf.prop_recursive(5, 64, 6, move |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Object::Array),
            prop::collection::btree_map(name_bytes.clone(), inner, 0..6).prop_map(|entries| {
                let mut dict = Dictionary::new();
                for (k, v) in entries {
                    dict.insert(k, v);
                }
                Object::Dictionary(dict)
            }),
        ]
    })
}

/// Minimal serializer for the round-trip property; the real writer comes in P4.
fn write_object(object: &Object, out: &mut Vec<u8>) {
    match object {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Boolean(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Object::Integer(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Object::Real(r) => {
            let text = r.to_string();
            out.extend_from_slice(text.as_bytes());
            if !text.contains('.') {
                out.extend_from_slice(b".0");
            }
        }
        Object::String(bytes) => {
            out.push(b'<');
            for b in bytes {
                out.extend_from_slice(format!("{b:02X}").as_bytes());
            }
            out.push(b'>');
        }
        Object::Name(name) => write_name(name, out),
        Object::Array(items) => {
            out.push(b'[');
            for item in items {
                write_object(item, out);
                out.push(b' ');
            }
            out.push(b']');
        }
        Object::Dictionary(dict) => {
            out.extend_from_slice(b"<<");
            for (key, value) in dict.iter() {
                write_name(key, out);
                out.push(b' ');
                write_object(value, out);
                out.push(b' ');
            }
            out.extend_from_slice(b">>");
        }
        Object::Reference(r) => {
            out.extend_from_slice(format!("{} {} R", r.number, r.generation).as_bytes());
        }
    }
}

fn write_name(name: &[u8], out: &mut Vec<u8>) {
    out.push(b'/');
    for &b in name {
        if b.is_ascii_alphanumeric() {
            out.push(b);
        } else {
            out.extend_from_slice(format!("#{b:02X}").as_bytes());
        }
    }
}

fn pdf_like_bytes() -> impl Strategy<Value = Vec<u8>> {
    let piece = prop_oneof![
        Just(&b"["[..]),
        Just(&b"]"[..]),
        Just(&b"<<"[..]),
        Just(&b">>"[..]),
        Just(&b" 0 R"[..]),
        Just(&b" 1 "[..]),
        Just(&b"/K"[..]),
        Just(&b" obj "[..]),
        Just(&b"endobj"[..]),
        Just(&b" stream\n"[..]),
        Just(&b"\nendstream "[..]),
        Just(&b"/Length 3"[..]),
        Just(&b"(s)"[..]),
        Just(&b"null"[..]),
    ];
    prop::collection::vec(piece, 0..40).prop_map(|pieces| pieces.concat())
}

proptest! {
    #[test]
    fn written_objects_read_back(object in arb_object()) {
        let mut text = Vec::new();
        write_object(&object, &mut text);
        prop_assert_eq!(Parser::new(&text).parse_object(&mut budget()), Ok(object));
    }

    #[test]
    fn arbitrary_bytes_are_handled(
        input in prop_oneof![prop::collection::vec(any::<u8>(), 0..256), pdf_like_bytes()]
    ) {
        let mut parser = Parser::new(&input);
        let mut b = limited(10_000, 16);
        // Every successful parse advances, so this loop terminates.
        while parser.parse_object(&mut b).is_ok() {}
        let mut resolver = |_: ObjectId, _: &mut Budget| Ok(2);
        for offset in 0..input.len() as u64 {
            // A fresh budget per start, so that one start cannot exhaust the
            // budget and turn every later check into a limit error.
            let mut b = limited(10_000, 16);
            let mut parser = Parser::new(&input);
            prop_assert!(parser.seek(offset).is_ok());
            if let Ok(IndirectObject { value: IndirectValue::Stream(s), .. }) =
                parser.parse_indirect_object(&mut b, &mut resolver)
            {
                prop_assert!(s.data.start <= s.data.end);
                prop_assert!(s.data.end <= input.len() as u64);
            }
        }
    }
}

#[test]
fn a_tolerant_parser_accepts_a_lone_cr_or_spaces_after_stream() {
    let tolerant = |input: &[u8]| -> Result<Stream, Error> {
        match Parser::new(input)
            .tolerating_stream_eol()
            .parse_indirect_object(&mut budget(), &mut no_indirect_length)?
            .value
        {
            IndirectValue::Stream(stream) => Ok(stream),
            IndirectValue::Object(other) => panic!("expected a stream, got {other:?}"),
        }
    };
    // Data starts after the lone CR, after the spaces and the end of line.
    let lone = b"1 0 obj<</Length 3>>stream\rabc\rendstream\rendobj";
    assert_eq!(tolerant(lone).map(|s| s.data), Ok(27..30));
    let spaced = b"1 0 obj<</Length 3>>stream  \t\r\nabc\nendstream endobj";
    assert_eq!(tolerant(spaced).map(|s| s.data), Ok(31..34));
    // CR LF stays one end of line, even when the data could start with LF.
    let crlf = b"1 0 obj<</Length 3>>stream\r\nabc\nendstream endobj";
    assert_eq!(tolerant(crlf).map(|s| s.data), Ok(28..31));
    // Still refused: no end of line at all, or the input ending on CR.
    assert_eq!(
        tolerant(b"1 0 obj<</Length 0>>stream endstream endobj"),
        Err(Error::Malformed {
            offset: 27,
            kind: MalformedKind::MissingStreamEol
        })
    );
    assert_eq!(
        tolerant(b"1 0 obj<</Length 0>>stream\r"),
        Err(Error::Truncated { offset: 26 })
    );
    // The strict parser is unchanged.
    assert_eq!(
        stream(lone),
        Err(Error::Malformed {
            offset: 26,
            kind: MalformedKind::MissingStreamEol
        })
    );
}
