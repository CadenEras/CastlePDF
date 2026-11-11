//! Decodes arbitrary bytes through the stream filters, with the filter and
//! the predictor parameters (predictor, colors, bits per component,
//! columns) taken from the first four bytes: FlateDecode, LZWDecode (with
//! or without EarlyChange, and a predictor), ASCIIHexDecode,
//! ASCII85Decode, RunLengthDecode, or ASCII85 then Flate. Properties: no
//! panic or overflow; the decoded length never exceeds what was charged,
//! and the charge never exceeds the limit (a decompression bomb, or a
//! RunLength or LZW expansion, stops at max_decoded_bytes).
#![no_main]

use libfuzzer_sys::fuzz_target;
use pdf_syntax::{Budget, CancellationToken, Dictionary, Object, ResourceLimits, decode_stream};

const LIMIT: u64 = 1 << 20;

fn name(n: &[u8]) -> Object {
    Object::Name(n.to_vec())
}

fuzz_target!(|data: &[u8]| {
    let (head, body) = data.split_at(data.len().min(4));
    let mut dictionary = Dictionary::new();
    dictionary.insert(b"Filter".to_vec(), name(b"FlateDecode"));
    if let [predictor, colors, bits, columns] = head {
        let mut params = Dictionary::new();
        let kind = match predictor % 4 {
            0 => 1,
            1 => 2,
            2 => 10 + i64::from(predictor % 6),
            _ => i64::from(*predictor),
        };
        params.insert(b"Predictor".to_vec(), Object::Integer(kind));
        params.insert(b"Colors".to_vec(), Object::Integer(i64::from(colors % 4) + 1));
        let bits = [1, 2, 4, 8, 16, 3][usize::from(bits % 6)];
        params.insert(b"BitsPerComponent".to_vec(), Object::Integer(bits));
        params.insert(b"Columns".to_vec(), Object::Integer(i64::from(*columns)));
        let filter = match (colors / 4) % 7 {
            0 => name(b"FlateDecode"),
            1 | 2 => {
                params.insert(
                    b"EarlyChange".to_vec(),
                    Object::Integer(i64::from((colors / 4) % 7 == 1)),
                );
                name(b"LZWDecode")
            }
            3 => name(b"ASCIIHexDecode"),
            4 => name(b"ASCII85Decode"),
            5 => name(b"RunLengthDecode"),
            _ => Object::Array(vec![name(b"ASCII85Decode"), name(b"FlateDecode")]),
        };
        let chained = matches!(filter, Object::Array(_));
        dictionary.insert(b"Filter".to_vec(), filter);
        let params = Object::Dictionary(params);
        dictionary.insert(
            b"DecodeParms".to_vec(),
            if chained {
                Object::Array(vec![Object::Null, params])
            } else {
                params
            },
        );
    }
    let mut limits = ResourceLimits::default();
    limits.max_decoded_bytes = LIMIT;
    let mut budget = Budget::new(limits, CancellationToken::new());
    if let Ok(decoded) = decode_stream(&dictionary, body, 0, &mut budget) {
        assert!(decoded.len() as u64 <= budget.decoded_bytes());
    }
    assert!(budget.decoded_bytes() <= LIMIT);
});
