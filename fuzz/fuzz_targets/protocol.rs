//! Arbitrary bytes decoded as a worker request and as a worker response
//! (ADR-0001; text layers since ADR-0006, tranche 3.6a): the shell decodes
//! what a worker that read untrusted PDF data sends back.
//! Properties: no panic and no allocation beyond what the bytes can hold
//! (counts are checked before allocating); a message that decodes encodes
//! back to the same bytes.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pdf_worker::protocol::{Request, Response};

fuzz_target!(|data: &[u8]| {
    if let Ok(request) = Request::decode(data) {
        assert_eq!(request.encode(), data);
    }
    if let Ok(response) = Response::decode(data) {
        assert_eq!(response.encode(), data);
    }
});
