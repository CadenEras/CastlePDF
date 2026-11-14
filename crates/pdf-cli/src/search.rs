//! `castle-pdf search <file> <query> [--match-case] [--match-diacritics]`:
//! prints each line of the document that holds an occurrence of the query
//! (ADR-0006, tranche 3.7), as `page <n>: <line>`, once per line, with the
//! comparison of `pdf_render::search`. The line is printed as `text` prints
//! it (`printable`).
//!
//! The error output gets the number of occurrences and pages, and the time
//! taken to search, opening excluded. A page that fails is reported and the
//! next ones are still searched; the exit code is then 1. A query that is
//! empty once folded, or too long, is a usage error (2).

use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::Path;
use std::time::Instant;

use pdf_document::Document;
use pdf_render::search::{Query, SearchOptions, find};
use pdf_render::{RenderCache, RenderLimits, page_text_cached, reading_lines};
use pdf_syntax::{Budget, CancellationToken, ResourceLimits};

use crate::text::printable;

/// Most occurrences counted on a page, as in the worker.
const MAX_PAGE_HITS: usize = 1000;

/// Parsed arguments; `None` is a usage error.
pub struct Request<'a> {
    input: &'a Path,
    query: Query,
}

impl<'a> Request<'a> {
    /// `<file> <query> [--match-case] [--match-diacritics]`.
    pub fn parse(args: &[&'a OsStr]) -> Option<Self> {
        let [input, query, flags @ ..] = args else {
            return None;
        };
        let mut options = SearchOptions::default();
        for flag in flags {
            match flag.to_str()? {
                "--match-case" if !options.match_case => options.match_case = true,
                "--match-diacritics" if !options.match_diacritics => {
                    options.match_diacritics = true;
                }
                _ => return None,
            }
        }
        let query = Query::new(query.to_str()?, options).ok()?;
        Some(Self {
            input: Path::new(*input),
            query,
        })
    }
}

/// Searches every page; returns 0 or 1. An `Err` means that writing failed.
pub fn run(request: &Request<'_>, out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
    let limits = ResourceLimits::default();
    let Some(source) = crate::open::source(request.input, &limits, err)? else {
        return Ok(1);
    };
    let mut budget = Budget::new(limits, CancellationToken::new());
    let document = match Document::open(source, &mut budget) {
        Ok(document) => document,
        Err(e) => {
            writeln!(err, "error: {e}")?;
            return Ok(1);
        }
    };
    let started = Instant::now();
    let mut cache = RenderCache::default();
    let (mut hits, mut pages, mut failed) = (0usize, 0usize, false);
    for index in 0..document.page_count() {
        let Some(page) = document.page(index) else {
            continue;
        };
        let mut budget = Budget::new(limits, CancellationToken::new());
        let text = match page_text_cached(
            document.file(),
            page,
            &RenderLimits::default(),
            &mut cache,
            &mut budget,
        ) {
            Ok(text) => text,
            Err(e) => {
                writeln!(err, "error: page {}: {e}", index + 1)?;
                failed = true;
                continue;
            }
        };
        let lines = reading_lines(&text);
        let found = find(&lines, &request.query, MAX_PAGE_HITS);
        if found.hits.is_empty() {
            continue;
        }
        hits += found.hits.len();
        pages += 1;
        if found.truncated {
            writeln!(
                err,
                "page {}: more than {MAX_PAGE_HITS} occurrences, the others not counted",
                index + 1
            )?;
        }
        let mut shown = None;
        for hit in &found.hits {
            // An occurrence across lines shows its first one.
            if shown == Some(hit.start.line) {
                continue;
            }
            shown = Some(hit.start.line);
            if let Some(line) = lines.get(hit.start.line) {
                let text: String = line.text().chars().map(printable).collect();
                writeln!(out, "page {}: {text}", index + 1)?;
            }
        }
    }
    writeln!(
        err,
        "{hits} occurrences in {pages} of {} pages, searched in {} ms",
        document.page_count(),
        started.elapsed().as_millis()
    )?;
    Ok(u8::from(failed))
}
