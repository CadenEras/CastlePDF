//! Arbitrary bytes read as a CMap (ADR-0006, tranche 3.2), then every code
//! of the input looked up: next code, Unicode, CID and notdef; the CMap is
//! also merged under itself, as `usecmap` would.
//! Properties: no panic, overflow or unbounded work or memory; an invalid
//! CMap is an error, never fatal.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pdf_render::CMap;
use pdf_syntax::{Budget, CancellationToken, ResourceLimits};

fuzz_target!(|data: &[u8]| {
    let mut limits = ResourceLimits::default();
    limits.max_decoded_bytes = 4 << 20;
    let mut budget = Budget::new(limits, CancellationToken::new());
    let Ok(mut cmap) = CMap::parse(data, &mut budget) else {
        return;
    };
    let mut rest = data;
    while let Some(code) = cmap.next_code(rest) {
        let _ = (cmap.unicode(code), cmap.cid(code), cmap.notdef(code));
        rest = rest.get(usize::from(code.len)..).unwrap_or_default();
    }
    let parent = cmap.clone();
    if cmap.inherit(&parent, &mut budget).is_ok() {
        let _ = cmap.next_code(data).map(|code| cmap.unicode(code));
    }
});
