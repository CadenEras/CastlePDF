//! Arbitrary bytes as a document rewritten unchanged (ADR-0008, tranche
//! 4.2). Property: whatever opens (repaired or not) and is rewritten reads
//! back with the strict reader, with the same number of pages; rewriting
//! never panics nor exceeds its size limit.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pdf_document::Document;
use pdf_syntax::{Budget, CancellationToken, MemorySource, PdfFile, ResourceLimits};
use pdf_write::{identity_plan, rewrite};

fn budget() -> Budget {
    let mut limits = ResourceLimits::default();
    limits.max_decoded_bytes = 16 << 20;
    limits.max_objects_visited = 100_000;
    Budget::new(limits, CancellationToken::new())
}

fuzz_target!(|data: &[u8]| {
    let Ok(source) = MemorySource::from_vec(data.to_vec(), &ResourceLimits::default()) else {
        return;
    };
    let Ok(document) = Document::open(source, &mut budget()) else {
        return;
    };
    let limit = (data.len() as u64).saturating_mul(4).max(1 << 20);
    let Ok(rewritten) = rewrite(&document, &identity_plan(&document), limit, &mut budget()) else {
        return;
    };
    assert!(rewritten.bytes.len() as u64 <= limit);
    let source = MemorySource::from_vec(rewritten.bytes, &ResourceLimits::default())
        .expect("the output is within the input limit");
    let file =
        PdfFile::open_strict(source, &mut budget()).expect("a rewritten file reads strictly");
    let written = Document::from_file(file, &mut budget()).expect("its page tree reads");
    assert_eq!(written.page_count(), document.page_count());
    // The last page left out (tranche 4.3): resources pruned, the same
    // property for the pages kept.
    let mut plan = identity_plan(&document);
    if plan.len() < 2 {
        return;
    }
    plan.pop();
    let Ok(rewritten) = rewrite(&document, &plan, limit, &mut budget()) else {
        return;
    };
    assert!(rewritten.bytes.len() as u64 <= limit);
    let source = MemorySource::from_vec(rewritten.bytes, &ResourceLimits::default())
        .expect("the output is within the input limit");
    let file =
        PdfFile::open_strict(source, &mut budget()).expect("a rewritten file reads strictly");
    let written = Document::from_file(file, &mut budget()).expect("its page tree reads");
    assert_eq!(written.page_count(), plan.len());
});
