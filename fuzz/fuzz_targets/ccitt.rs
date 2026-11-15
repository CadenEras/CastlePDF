//! Arbitrary bytes decoded by `CCITTFaxDecode` (tranche 3.9), the
//! parameters taken from the first three bytes: K (-1, 0 or 1 to 4), the
//! columns (1 to 256, or 1728 by default), the rows (none or 1 to 64) and
//! the flags EndOfLine, EncodedByteAlign, EndOfBlock and BlackIs1.
//! Properties: no panic, overflow or unbounded work; the output is whole
//! rows of the given columns, never more than the budget allows.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pdf_syntax::{Budget, CancellationToken, Dictionary, Object, ResourceLimits, decode_stream};

const LIMIT: u64 = 1 << 20;

fuzz_target!(|data: &[u8]| {
    let [k, columns, flags, body @ ..] = data else {
        return;
    };
    let mut params = Dictionary::new();
    params.insert(b"K".to_vec(), Object::Integer(i64::from(*k % 6) - 1));
    let columns = if *columns == 0 { 1728 } else { i64::from(*columns) };
    params.insert(b"Columns".to_vec(), Object::Integer(columns));
    if flags & 0xF0 != 0 {
        params.insert(b"Rows".to_vec(), Object::Integer(i64::from(flags >> 4) * 4));
    }
    for (bit, key) in [
        (1, &b"EndOfLine"[..]),
        (2, b"EncodedByteAlign"),
        (4, b"EndOfBlock"),
        (8, b"BlackIs1"),
    ] {
        params.insert(key.to_vec(), Object::Boolean(flags & bit != 0));
    }
    let mut dictionary = Dictionary::new();
    dictionary.insert(b"Filter".to_vec(), Object::Name(b"CCITTFaxDecode".to_vec()));
    dictionary.insert(b"DecodeParms".to_vec(), Object::Dictionary(params));
    let mut limits = ResourceLimits::default();
    limits.max_decoded_bytes = LIMIT;
    let mut budget = Budget::new(limits, CancellationToken::new());
    if let Ok(decoded) = decode_stream(&dictionary, body, 0, &mut budget) {
        let stride = usize::try_from(columns).unwrap_or(1).div_ceil(8);
        assert_eq!(decoded.len() % stride, 0, "whole rows");
        assert!(decoded.len() as u64 <= budget.decoded_bytes());
        assert!(budget.decoded_bytes() <= LIMIT);
    }
});
