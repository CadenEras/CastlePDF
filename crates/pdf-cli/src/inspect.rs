//! `castle-pdf inspect <file>`: structure report, never document content.
//!
//! The report is written line by line (no whole-report buffer); every line is
//! bounded: numbers beyond 1e15 use scientific notation, and only standard
//! trailer keys are named, other keys being counted.

use std::io::{self, Write};
use std::path::Path;

use pdf_document::{Document, Rectangle};
use pdf_syntax::{
    Budget, CancellationToken, Object, PdfFile, RepairTrailer, ResourceLimits, SectionKind, Source,
    XrefEntry,
};

/// Trailer keys of ISO 32000-2 (table 15), and the keys of a cross-reference
/// stream dictionary that serves as trailer (table 17, with the stream keys):
/// the only ones printed by name.
const TRAILER_KEYS: [&[u8]; 13] = [
    b"DecodeParms",
    b"Encrypt",
    b"Filter",
    b"ID",
    b"Index",
    b"Info",
    b"Length",
    b"Prev",
    b"Root",
    b"Size",
    b"Type",
    b"W",
    b"XRefStm",
];

/// Writes the report to `out` and errors to `err`; returns 0 or 1. An `Err`
/// means that writing the report itself failed.
pub fn run(path: &Path, out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
    let limits = ResourceLimits::default();
    let Some(source) = crate::open::source(path, &limits, err)? else {
        return Ok(1);
    };
    // One budget for the whole opening: structure, catalog and page tree.
    let mut budget = Budget::new(limits, CancellationToken::new());
    writeln!(out, "size: {} bytes", Source::len(&source))?;
    let file = match PdfFile::open(source, &mut budget) {
        Ok(file) => file,
        Err(e) => {
            writeln!(err, "error: {e}")?;
            return Ok(1);
        }
    };
    file_report(&file, out)?;
    match Document::from_file(file, &mut budget) {
        Ok(document) => {
            pages_report(&document, out)?;
            repair_report(document.file(), out)?;
            Ok(0)
        }
        Err(e) => {
            writeln!(err, "error: {e}")?;
            Ok(1)
        }
    }
}

/// Written last: reading the page tree may itself find an object by a scan.
fn repair_report<S: Source>(file: &PdfFile<S>, out: &mut dyn Write) -> io::Result<()> {
    let Some(repair) = file.repair() else {
        return writeln!(out, "repair: none");
    };
    let trailer = match repair.trailer {
        RepairTrailer::Found => "trailer found by scan",
        RepairTrailer::Catalog => "no trailer, root is the last catalog",
        RepairTrailer::Original => "original trailer, objects relocated",
        _ => "other",
    };
    write!(
        out,
        "repair: {} objects indexed by scan, {} found more than once; {trailer}; strict error: {}",
        repair.objects, repair.conflicts, repair.cause
    )?;
    if repair.is_incomplete() {
        write!(
            out,
            "; objects may be missing: {} unreadable object streams, {} streams without endstream",
            repair.skipped_object_streams, repair.unterminated_streams
        )?;
    }
    writeln!(out)
}

fn file_report<S: Source>(file: &PdfFile<S>, out: &mut dyn Write) -> io::Result<()> {
    let version = file.version();
    writeln!(out, "version: {}.{}", version.major, version.minor)?;
    writeln!(out, "startxref: {}", file.startxref())?;
    writeln!(out, "revisions: {}", file.revisions())?;
    for (i, section) in file.sections().iter().enumerate() {
        let kind = match section.kind {
            SectionKind::Table => "xref table",
            SectionKind::Stream => "xref stream",
            SectionKind::HybridStream => "hybrid xref stream",
            _ => "xref section",
        };
        writeln!(
            out,
            "  section {}: {kind} at {}, {} entries",
            i + 1,
            section.offset,
            section.entries
        )?;
    }
    let (mut in_use, mut compressed, mut free) = (0, 0, 0);
    for (_, entry) in file.entries() {
        match entry {
            XrefEntry::InUse { .. } => in_use += 1,
            XrefEntry::Compressed { .. } => compressed += 1,
            _ => free += 1,
        }
    }
    writeln!(
        out,
        "objects: {in_use} in use, {compressed} compressed, {free} free"
    )?;
    let trailer = file.trailer();
    let mut known = Vec::new();
    let mut others = 0_usize;
    for (key, _) in trailer.iter() {
        match TRAILER_KEYS.iter().find(|k| **k == key) {
            Some(k) => known.push(String::from_utf8_lossy(k).into_owned()),
            None => others += 1,
        }
    }
    let names: Vec<String> = known.iter().map(|k| format!("/{k}")).collect();
    write!(out, "trailer keys: {}", names.join(" "))?;
    if others > 0 {
        write!(out, " (+{others} other)")?;
    }
    writeln!(out)?;
    if let Some(root) = file.root() {
        writeln!(out, "root: {} {} R", root.number, root.generation)?;
    }
    if let Some(size) = trailer.get(b"Size").and_then(Object::as_integer) {
        writeln!(out, "size entry: {size}")?;
    }
    Ok(())
}

fn pages_report<S: Source>(document: &Document<S>, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "pages: {}", document.page_count())?;
    for (i, page) in document.pages().iter().enumerate() {
        writeln!(
            out,
            "  page {}: object {} {}, media box {}, crop box {}, rotation {}",
            i + 1,
            page.id.number,
            page.id.generation,
            rectangle(&page.media_box),
            rectangle(&page.crop_box),
            page.rotation
        )?;
    }
    Ok(())
}

fn rectangle(r: &Rectangle) -> String {
    format!(
        "[{} {} {} {}]",
        number(r.llx),
        number(r.lly),
        number(r.urx),
        number(r.ury)
    )
}

/// Plain notation for ordinary values, scientific beyond 1e15, so that one
/// coordinate never prints hundreds of digits.
fn number(value: f64) -> String {
    if value.abs() < 1e15 {
        format!("{value}")
    } else {
        format!("{value:e}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangles_print_bounded_numbers() {
        assert_eq!(
            rectangle(&Rectangle::new(0.0, 0.0, 595.0, 842.5)),
            "[0 0 595 842.5]"
        );
        assert_eq!(number(-1.7e308), "-1.7e308");
        assert!(number(f64::MAX).len() < 32);
    }
}
