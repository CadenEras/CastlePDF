//! Opens arbitrary bytes as a PDF file, strictly (no repair: the `repair`
//! target covers it), then resolves every in-use and
//! compressed entry (object streams included).
//! Properties: no panic, overflow or unbounded work; every located stream lies
//! inside the input; resolving never recurses through /Length chains.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pdf_syntax::{
    Budget, CancellationToken, IndirectValue, MemorySource, ObjectId, PdfFile, ResourceLimits,
    XrefEntry,
};

fn budget() -> Budget {
    let mut limits = ResourceLimits::default();
    limits.max_decoded_bytes = 1 << 20;
    limits.max_objects_visited = 100_000;
    limits.max_nesting_depth = 32;
    Budget::new(limits, CancellationToken::new())
}

fuzz_target!(|data: &[u8]| {
    let limits = ResourceLimits::default();
    let Ok(source) = MemorySource::from_vec(data.to_vec(), &limits) else {
        return;
    };
    let Ok(file) = PdfFile::open_strict(source, &mut budget()) else {
        return;
    };
    assert!(file.root().is_some());
    let ids: Vec<ObjectId> = file
        .entries()
        .filter_map(|(number, entry)| match entry {
            XrefEntry::InUse { generation, .. } => Some(ObjectId { number, generation }),
            XrefEntry::Compressed { .. } => Some(ObjectId {
                number,
                generation: 0,
            }),
            _ => None,
        })
        .take(256)
        .collect();
    for id in ids {
        // A fresh budget per object: one object must not starve the others.
        if let Ok(IndirectValue::Stream(stream)) = file.resolve(id, &mut budget()) {
            assert!(stream.data.end <= data.len() as u64);
            assert!(file.stream_data(&stream).is_ok());
        }
    }
});
