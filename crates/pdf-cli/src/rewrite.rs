//! Writing commands (ADR-0008), all through `pdf-write`:
//! - `castle-pdf rewrite <file> <output.pdf>` (tranche 4.2): the document
//!   written again whole, every page in order;
//! - `castle-pdf pages <file> <output.pdf> <plan>` (tranche 4.3): the pages
//!   of a plan, in its order, turned as it says (see `parse_plan`);
//! - `castle-pdf merge <output.pdf> <file> <file>...` (tranche 4.4): every
//!   page of each file, the files in order;
//! - `castle-pdf split <file> <directory> <pages>` (tranche 4.4): the
//!   document cut into files of `<pages>` pages, `part-001.pdf`... in an
//!   existing directory.
//!
//! Each result is read back by the strict reader (the plan's page count)
//! before it is written; an output is a new file, never an existing one,
//! never a source.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use pdf_document::Document;
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};
use pdf_write::{PlanPage, Rewritten, SourcePage, merge};

use crate::open;
use crate::render::publish;

/// The output may be this many times the input (object streams of the
/// source are written out as plain objects), and at least `MIN_LIMIT`.
const GROWTH: u64 = 4;
const MIN_LIMIT: u64 = 16 << 20;

/// Most files `merge` takes, and most files `split` writes.
const MAX_FILES: usize = 1000;

/// Documents opened from `inputs`, with their total size; `None` when one
/// cannot be (the error is written, naming the input by its position: a
/// path may hold control characters or private names). Together they may
/// not hold more than `max_input_bytes`, checked before any is read, as
/// the output could not be read back; a document without pages is refused.
fn open_all(
    inputs: &[&Path],
    limits: &ResourceLimits,
    budget: &mut Budget,
    err: &mut dyn Write,
) -> io::Result<Option<(Vec<Document<MemorySource>>, u64)>> {
    let several = inputs.len() > 1;
    let mut announced: u64 = 0;
    for input in inputs {
        let len = std::fs::metadata(input).map_or(0, |m| m.len());
        announced = announced.saturating_add(len);
    }
    if announced > limits.max_input_bytes {
        writeln!(
            err,
            "error: the files hold more than {} bytes together",
            limits.max_input_bytes
        )?;
        return Ok(None);
    }
    let mut documents = Vec::with_capacity(inputs.len());
    let mut total: u64 = 0;
    for (index, input) in inputs.iter().enumerate() {
        let label = if several {
            format!("input {}: ", index + 1)
        } else {
            String::new()
        };
        let mut message = Vec::new();
        let Some(source) = open::source(input, limits, &mut message)? else {
            let message = message.strip_prefix(b"error: ").unwrap_or(&message);
            err.write_all(b"error: ")?;
            err.write_all(label.as_bytes())?;
            err.write_all(message)?;
            return Ok(None);
        };
        total = total.saturating_add(pdf_syntax::Source::len(&source));
        if total > limits.max_input_bytes {
            writeln!(
                err,
                "error: the files hold more than {} bytes together",
                limits.max_input_bytes
            )?;
            return Ok(None);
        }
        match Document::open(source, budget) {
            Ok(document) if document.page_count() == 0 => {
                writeln!(err, "error: {label}the document has no pages")?;
                return Ok(None);
            }
            Ok(document) => documents.push(document),
            Err(e) => {
                writeln!(err, "error: {label}{e}")?;
                return Ok(None);
            }
        }
    }
    Ok(Some((documents, total)))
}

/// The plan written and read back by the strict reader with its page
/// count; the error, as text, otherwise.
fn write_plan(
    documents: &[Document<MemorySource>],
    plan: &[SourcePage],
    input_len: u64,
    limits: &ResourceLimits,
    budget: &mut Budget,
) -> Result<Rewritten, String> {
    // Never more than the reader takes back (it reads the result again).
    let limit = input_len
        .saturating_mul(GROWTH)
        .max(MIN_LIMIT)
        .min(limits.max_input_bytes);
    let sources: Vec<&Document<MemorySource>> = documents.iter().collect();
    let rewritten = merge(&sources, plan, limit, budget)
        .map_err(|e| format!("cannot rewrite the document: {e}"))?;
    let source = MemorySource::from_vec(rewritten.bytes.clone(), limits)
        .map_err(|e| format!("the rewritten document does not read back: {e}"))?;
    let mut budget = Budget::new(*limits, CancellationToken::new());
    let unreadable =
        |e: &dyn std::fmt::Display| format!("the rewritten document does not read back: {e}");
    let file = pdf_syntax::PdfFile::open_strict(source, &mut budget).map_err(|e| unreadable(&e))?;
    let written = Document::from_file(file, &mut budget).map_err(|e| unreadable(&e))?;
    if written.page_count() != plan.len() {
        return Err(format!(
            "the rewritten document reads back with {} pages, not {}",
            written.page_count(),
            plan.len()
        ));
    }
    Ok(rewritten)
}

/// `bytes` written to the new file `output`, which names no input.
fn create(output: &Path, inputs: &[&Path], bytes: &[u8]) -> Result<(), String> {
    if output.exists() || inputs.iter().any(|input| same_file(input, output)) {
        return Err("cannot create the document: the output exists".to_owned());
    }
    publish(output, |mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    })
    .map_err(|e| format!("cannot create the document: {}", e.kind()))
}

/// The report of a written document: pages, size, what is not kept.
fn report(
    out: &mut dyn Write,
    rewritten: &Rewritten,
    pages: usize,
    input_len: u64,
    several: bool,
) -> io::Result<()> {
    writeln!(
        out,
        "rewritten: {pages} pages, {} bytes (input {input_len}); not kept: {}",
        rewritten.bytes.len(),
        if rewritten.dropped.is_empty() {
            "nothing".to_owned()
        } else {
            rewritten.dropped.join(", ")
        }
    )?;
    if !rewritten.resources_kept_whole.is_empty() {
        let pages: Vec<String> = rewritten
            .resources_kept_whole
            .iter()
            .map(|(document, page)| {
                if several {
                    format!("{}:{}", document + 1, page + 1)
                } else {
                    (page + 1).to_string()
                }
            })
            .collect();
        writeln!(
            out,
            "resources kept whole (what they use cannot be told for sure), pages: {}",
            pages.join(", ")
        )?;
    }
    Ok(())
}

/// `rewrite` and `pages`. `plan`: the plan as given on the command line,
/// `None` for every page.
pub fn run(
    input: &Path,
    output: &Path,
    plan: Option<&str>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<u8> {
    let limits = ResourceLimits::default();
    let mut budget = Budget::new(limits, CancellationToken::new());
    let Some((documents, input_len)) = open_all(&[input], &limits, &mut budget, err)? else {
        return Ok(1);
    };
    let page_count = documents.first().map_or(0, Document::page_count);
    let plan = match plan {
        None => (0..page_count)
            .map(|page| PlanPage { page, rotation: 0 })
            .collect(),
        Some(spec) => match parse_plan(spec, page_count) {
            Ok(plan) => plan,
            Err(e) => {
                writeln!(err, "error: invalid page plan: {e}")?;
                return Ok(2);
            }
        },
    };
    let plan: Vec<SourcePage> = plan
        .iter()
        .map(|p| SourcePage {
            document: 0,
            page: p.page,
            rotation: p.rotation,
        })
        .collect();
    let written = write_plan(&documents, &plan, input_len, &limits, &mut budget)
        .and_then(|rewritten| create(output, &[input], &rewritten.bytes).map(|()| rewritten));
    match written {
        Ok(rewritten) => {
            report(out, &rewritten, plan.len(), input_len, false)?;
            Ok(0)
        }
        Err(e) => {
            writeln!(err, "error: {e}")?;
            Ok(1)
        }
    }
}

/// `merge`: every page of each input, the inputs in order.
pub fn run_merge(
    output: &Path,
    inputs: &[&Path],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<u8> {
    if inputs.len() > MAX_FILES {
        writeln!(err, "error: more than {MAX_FILES} files")?;
        return Ok(2);
    }
    let limits = ResourceLimits::default();
    let mut budget = Budget::new(limits, CancellationToken::new());
    let Some((documents, input_len)) = open_all(inputs, &limits, &mut budget, err)? else {
        return Ok(1);
    };
    let plan: Vec<SourcePage> = documents
        .iter()
        .enumerate()
        .flat_map(|(document, source)| {
            (0..source.page_count()).map(move |page| SourcePage {
                document,
                page,
                rotation: 0,
            })
        })
        .collect();
    let written = write_plan(&documents, &plan, input_len, &limits, &mut budget)
        .and_then(|rewritten| create(output, inputs, &rewritten.bytes).map(|()| rewritten));
    match written {
        Ok(rewritten) => {
            report(out, &rewritten, plan.len(), input_len, true)?;
            Ok(0)
        }
        Err(e) => {
            writeln!(err, "error: {e}")?;
            Ok(1)
        }
    }
}

/// `split`: the document cut into files of `size` pages each (the last
/// may have fewer), `part-001.pdf`... in `directory`. None is written when
/// one of them exists already.
pub fn run_split(
    input: &Path,
    directory: &Path,
    size: &str,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<u8> {
    let Some(size) = size
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0 && size.bytes().all(|b| b.is_ascii_digit()))
    else {
        writeln!(
            err,
            "error: invalid part size: \"{size}\" is not a number of pages"
        )?;
        return Ok(2);
    };
    if !directory.is_dir() {
        writeln!(err, "error: the output directory is not a directory")?;
        return Ok(1);
    }
    let limits = ResourceLimits::default();
    let mut budget = Budget::new(limits, CancellationToken::new());
    let Some((documents, input_len)) = open_all(&[input], &limits, &mut budget, err)? else {
        return Ok(1);
    };
    let page_count = documents.first().map_or(0, Document::page_count);
    let parts = page_count.div_ceil(size);
    if parts > MAX_FILES {
        writeln!(err, "error: more than {MAX_FILES} files")?;
        return Ok(2);
    }
    let names: Vec<PathBuf> = (1..=parts)
        .map(|part| directory.join(format!("part-{part:03}.pdf")))
        .collect();
    if let Some(existing) = names.iter().find(|name| name.exists()) {
        writeln!(
            err,
            "error: cannot create the documents: {} exists",
            part_name(existing)
        )?;
        return Ok(1);
    }
    // Parts written so far: removed if a later one fails, so that a failed
    // split leaves the directory as it was (ADR-0008, point 4). Each was
    // created new by `publish`, never an existing file.
    let mut created: Vec<&PathBuf> = Vec::new();
    for (part, name) in names.iter().enumerate() {
        let first = part * size;
        let plan: Vec<SourcePage> = (first..page_count.min(first + size))
            .map(|page| SourcePage {
                document: 0,
                page,
                rotation: 0,
            })
            .collect();
        // Each part is a task of its own, with its own budget.
        let mut budget = Budget::new(limits, CancellationToken::new());
        let written = write_plan(&documents, &plan, input_len, &limits, &mut budget)
            .and_then(|rewritten| create(name, &[input], &rewritten.bytes).map(|()| rewritten));
        match written {
            Ok(rewritten) => {
                created.push(name);
                write!(out, "{}: ", part_name(name))?;
                report(out, &rewritten, plan.len(), input_len, false)?;
            }
            Err(e) => {
                for written in created {
                    let _ = std::fs::remove_file(written);
                }
                writeln!(err, "error: {}: {e}", part_name(name))?;
                return Ok(1);
            }
        }
    }
    Ok(0)
}

/// The file name of a part (`part-001.pdf`), never its directory.
fn part_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Most entries a plan may have, pages repeated included.
const MAX_PLAN: usize = 100_000;

/// A plan given as comma-separated items, pages numbered from 1: `n`, or a
/// range `a-b` (in either direction), each optionally followed by
/// `@90`, `@180` or `@270` to turn it clockwise. Pages left out are
/// deleted; order is the plan's. `"3,1-2@90"`: page 3, then pages 1 and 2
/// turned a quarter.
pub fn parse_plan(spec: &str, page_count: usize) -> Result<Vec<PlanPage>, String> {
    let mut plan = Vec::new();
    for item in spec.split(',') {
        let item = item.trim();
        let (pages, rotation) = match item.split_once('@') {
            Some((pages, rotation)) => (pages, rotation),
            None => (item, "0"),
        };
        let rotation = match rotation.trim() {
            "0" => 0,
            "90" => 90,
            "180" => 180,
            "270" => 270,
            _ => return Err(format!("rotation of \"{item}\" is not 90, 180 or 270")),
        };
        let page = |text: &str| -> Result<usize, String> {
            let digits = text.trim();
            match digits.parse::<usize>() {
                Ok(n)
                    if digits.bytes().all(|b| b.is_ascii_digit())
                        && (1..=page_count).contains(&n) =>
                {
                    Ok(n - 1)
                }
                _ => Err(format!("\"{text}\" is not a page from 1 to {page_count}")),
            }
        };
        let (first, last) = match pages.split_once('-') {
            Some((first, last)) => (page(first)?, page(last)?),
            None => (page(pages)?, page(pages)?),
        };
        let count = first.abs_diff(last) + 1;
        if plan.len() + count > MAX_PLAN {
            return Err(format!("more than {MAX_PLAN} pages"));
        }
        let range: Box<dyn Iterator<Item = usize>> = if first <= last {
            Box::new(first..=last)
        } else {
            Box::new((last..=first).rev())
        };
        plan.extend(range.map(|page| PlanPage { page, rotation }));
    }
    Ok(plan)
}

/// Whether `output` names the same file as `input` (through links too).
fn same_file(input: &Path, output: &Path) -> bool {
    match (std::fs::canonicalize(input), std::fs::canonicalize(output)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_plan;

    fn pages(spec: &str, count: usize) -> Result<Vec<(usize, u16)>, String> {
        parse_plan(spec, count).map(|plan| plan.iter().map(|p| (p.page, p.rotation)).collect())
    }

    #[test]
    fn plans_name_pages_ranges_and_rotations() {
        assert_eq!(pages("3,1-2@90", 3), Ok(vec![(2, 0), (0, 90), (1, 90)]));
        assert_eq!(pages("3-1", 3), Ok(vec![(2, 0), (1, 0), (0, 0)]));
        assert_eq!(pages(" 2 @ 270 , 2", 2), Ok(vec![(1, 270), (1, 0)]));
    }

    #[test]
    fn invalid_plans_are_refused() {
        for spec in [
            "", "0", "4", "1-4", "1@45", "1@", "a", "1,,2", "-1", "1-", "+1", "1-+2",
        ] {
            assert!(pages(spec, 3).is_err(), "{spec}");
        }
        assert!(pages("1-3", usize::MAX).is_ok());
    }
}
