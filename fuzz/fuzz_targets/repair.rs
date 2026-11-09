//! Opens arbitrary bytes with reconstruction enabled (ADR-0004), then
//! resolves the entries found.
//! Properties: no panic, overflow or unbounded work (the scan stays within
//! the object budget); a repaired file has a root; a file that opens strictly
//! is never repaired when opening; every located stream lies inside the input.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pdf_syntax::{
    Budget, CancellationToken, IndirectValue, MemorySource, ObjectId, OpenOptions, PdfFile,
    RepairTrailer, ResourceLimits, XrefEntry,
};

fn budget() -> Budget {
    let mut limits = ResourceLimits::default();
    limits.max_decoded_bytes = 1 << 20;
    limits.max_objects_visited = 100_000;
    limits.max_nesting_depth = 32;
    Budget::new(limits, CancellationToken::new())
}

fn open(data: &[u8], options: OpenOptions) -> Option<PdfFile<MemorySource>> {
    let source = MemorySource::from_vec(data.to_vec(), &ResourceLimits::default()).ok()?;
    PdfFile::open_with(source, options, &mut budget()).ok()
}

fuzz_target!(|data: &[u8]| {
    let strict = open(data, OpenOptions::strict()).is_some();
    let Some(file) = open(data, OpenOptions::default()) else {
        assert!(!strict, "a file that opens strictly must open");
        return;
    };
    assert!(file.root().is_some());
    if strict {
        assert!(file.repair().is_none(), "a valid structure is not repaired");
    }
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
        if let Ok(IndirectValue::Stream(stream)) = file.resolve(id, &mut budget()) {
            assert!(stream.data.end <= data.len() as u64);
            assert!(file.stream_data(&stream).is_ok());
        }
    }
    // Lookups may relocate objects later, never rebuild the trailer.
    if strict {
        assert!(
            file.repair()
                .is_none_or(|report| report.trailer == RepairTrailer::Original)
        );
    }
});
