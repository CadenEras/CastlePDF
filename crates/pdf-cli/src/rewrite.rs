//! `castle-pdf rewrite <file> <output.pdf>` (ADR-0008, tranche 4.2): the
//! document written again whole, every page in order, by `pdf-write`.
//! `castle-pdf pages <file> <output.pdf> <plan>` (tranche 4.3): the pages
//! of a plan, in its order, turned as it says (see `parse_plan`). The
//! result is read back by the strict reader (the plan's page count) before
//! it is written; the output is a new file, never an existing one, never
//! the source.

use std::io::{self, Write};
use std::path::Path;

use pdf_document::Document;
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};
use pdf_write::{PlanPage, identity_plan, rewrite};

use crate::open;
use crate::render::publish;

/// The output may be this many times the input (object streams of the
/// source are written out as plain objects), and at least `MIN_LIMIT`.
const GROWTH: u64 = 4;
const MIN_LIMIT: u64 = 16 << 20;

/// `plan`: the plan as given on the command line, `None` for every page.
pub fn run(
    input: &Path,
    output: &Path,
    plan: Option<&str>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<u8> {
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
    // Never more than the reader takes back (it reads the result again).
    let limit = input_len
        .saturating_mul(GROWTH)
        .max(MIN_LIMIT)
        .min(limits.max_input_bytes);
    let plan = match plan {
        None => identity_plan(&document),
        Some(spec) => match parse_plan(spec, document.page_count()) {
            Ok(plan) => plan,
            Err(e) => {
                writeln!(err, "error: invalid page plan: {e}")?;
                return Ok(2);
            }
        },
    };
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
        let file =
            pdf_syntax::PdfFile::open_strict(source, &mut budget).map_err(|e| e.to_string())?;
        let written = Document::from_file(file, &mut budget).map_err(|e| e.to_string())?;
        Ok(written.page_count())
    };
    match check() {
        Ok(pages) if pages == plan.len() => {}
        Ok(pages) => {
            writeln!(
                err,
                "error: the rewritten document reads back with {pages} pages, not {}",
                plan.len()
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
        plan.len(),
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
            .map(|page| (page + 1).to_string())
            .collect();
        writeln!(
            out,
            "resources kept whole (what they use cannot be told for sure), pages: {}",
            pages.join(", ")
        )?;
    }
    Ok(0)
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
