//! `castle-pdf rewrite <file> <output.pdf>` (ADR-0008, tranche 4.2): the
//! document written again whole, every page in order, by `pdf-write`. The
//! result is read back by the strict reader (same page count) before it is
//! written; the output is a new file, never an existing one, never the
//! source.

use std::io::{self, Write};
use std::path::Path;

use pdf_document::Document;
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};
use pdf_write::{identity_plan, rewrite};

use crate::open;
use crate::render::publish;

/// The output may be this many times the input (object streams of the
/// source are written out as plain objects), and at least `MIN_LIMIT`.
const GROWTH: u64 = 4;
const MIN_LIMIT: u64 = 16 << 20;

pub fn run(input: &Path, output: &Path, out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
    let limits = ResourceLimits::default();
    let Some(source) = open::source(input, &limits, err)? else {
        return Ok(1);
    };
    let input_len = pdf_syntax::Source::len(&source);
    let mut budget = Budget::new(limits, CancellationToken::new());
    let document = match Document::open(source, &mut budget) {
        Ok(document) => document,
        Err(e) => {
            writeln!(err, "error: {e}")?;
            return Ok(1);
        }
    };
    let limit = input_len.saturating_mul(GROWTH).max(MIN_LIMIT);
    let plan = identity_plan(&document);
    let rewritten = match rewrite(&document, &plan, limit, &mut budget) {
        Ok(rewritten) => rewritten,
        Err(e) => {
            writeln!(err, "error: cannot rewrite the document: {e}")?;
            return Ok(1);
        }
    };
    // Read back before it is written: the strict reader, the same pages.
    let size = rewritten.bytes.len();
    let check = || -> Result<usize, String> {
        let source =
            MemorySource::from_vec(rewritten.bytes.clone(), &limits).map_err(|e| e.to_string())?;
        let mut budget = Budget::new(limits, CancellationToken::new());
        let file = pdf_syntax::PdfFile::open_strict(source, &mut budget).map_err(|e| e.to_string())?;
        let written = Document::from_file(file, &mut budget).map_err(|e| e.to_string())?;
        Ok(written.page_count())
    };
    match check() {
        Ok(pages) if pages == document.page_count() => {}
        Ok(pages) => {
            writeln!(
                err,
                "error: the rewritten document reads back with {pages} pages, not {}",
                document.page_count()
            )?;
            return Ok(1);
        }
        Err(e) => {
            writeln!(err, "error: the rewritten document does not read back: {e}")?;
            return Ok(1);
        }
    }
    if output.exists() || same_file(input, output) {
        writeln!(err, "error: cannot create the document: the output exists")?;
        return Ok(1);
    }
    if let Err(e) = publish(output, |mut file| {
        file.write_all(&rewritten.bytes)?;
        file.sync_all()
    }) {
        writeln!(err, "error: cannot create the document: {}", e.kind())?;
        return Ok(1);
    }
    writeln!(
        out,
        "rewritten: {} pages, {size} bytes (input {input_len}); not kept: {}",
        document.page_count(),
        if rewritten.dropped.is_empty() {
            "nothing".to_owned()
        } else {
            rewritten.dropped.join(", ")
        }
    )?;
    Ok(0)
}

/// Whether `output` names the same file as `input` (through links too).
fn same_file(input: &Path, output: &Path) -> bool {
    match (std::fs::canonicalize(input), std::fs::canonicalize(output)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
