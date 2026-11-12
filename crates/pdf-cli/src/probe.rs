//! `castle-pdf probe <file>`: opens one file and renders its first page at
//! scale 1, then prints a single line of `key=value` tokens describing the
//! outcome, for `castle-pdf survey`:
//!
//! - `open=ok pages=<n> render=ok skipped=<capabilities|none>`
//! - `open=ok pages=<n> render=<category>` or `render=no-pages`
//! - `open=<category>`
//!
//! When the first page exists, `fonts=<kinds>` (or `fonts=none`) lists the
//! kinds of the fonts its resources declare (ADR-0005, step 5.1), classified
//! with a budget of their own; `fonts=<category>` if that fails. Then
//! `text=<sources>` tells where their Unicode would come from and, if the
//! page has Type 0 fonts, `cmaps=<encodings>` lists their encodings
//! (ADR-0006, tranche 3.1); `text=<category>` if that fails. Last, the
//! text of the first page is extracted (tranche 3.3), with a budget of its
//! own: `glyphs=<n> unknown=<n> other-length=<n> unread=<n>` counts its
//! glyphs, those whose text is unknown, those read through a `/ToUnicode`
//! code of another length, and the bytes shown in fonts that cannot be read
//! (their glyphs are not counted); `glyphs=<category>` if the extraction
//! fails.
//!
//! A file whose structure was repaired (ADR-0004) ends its line with
//! `repaired=<category>`, the category of the strict reading error, prefixed
//! with `on-lookup/` when the file opened strictly and only objects off their
//! offset were found by a scan.
//!
//! A category names the kind of error without offsets or object numbers
//! (`malformed/MissingEndobj`, `content/UnknownOperator`...), so that files
//! failing for the same reason are counted together. The line never holds
//! document content. The document outcome is data: the exit code is 0
//! whenever the line is written, and 3 when writing it fails.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

use pdf_document::Document;
use pdf_render::{
    RenderLimits, RenderOptions, UnicodeSource, page_font_kinds, page_text, page_text_profile,
    render_page,
};
use pdf_syntax::{Budget, CancellationToken, MemorySource, RepairTrailer, ResourceLimits};

/// Probes `path` and writes the outcome line to `out`.
pub fn run(path: &Path, out: &mut dyn Write) -> io::Result<u8> {
    let limits = ResourceLimits::default();
    let source = match read(path, &limits) {
        Ok(source) => source,
        Err(category) => {
            writeln!(out, "open={category}")?;
            return Ok(0);
        }
    };
    // One budget for the opening and the rendering, as `castle-pdf render`.
    let mut budget = Budget::new(limits, CancellationToken::new());
    let document = match Document::open(source, &mut budget) {
        Ok(document) => document,
        Err(e) => {
            writeln!(out, "open={}", document_category(&e))?;
            return Ok(0);
        }
    };
    write!(out, "open=ok pages={}", document.page_count())?;
    match document.page(0) {
        None => write!(out, " render=no-pages")?,
        Some(page) => match render_page(
            document.file(),
            page,
            &RenderOptions::default(),
            &mut budget,
        ) {
            Ok(rendering) => {
                let skipped: Vec<String> =
                    rendering.skipped.iter().map(|s| format!("{s:?}")).collect();
                let skipped = if skipped.is_empty() {
                    "none".to_owned()
                } else {
                    skipped.join(",")
                };
                write!(out, " render=ok skipped={skipped}")?;
            }
            Err(e) => write!(out, " render={}", render_category(&e))?,
        },
    }
    if let Some(page) = document.page(0) {
        let mut budget = Budget::new(limits, CancellationToken::new());
        match page_font_kinds(document.file(), page, &mut budget) {
            Ok(kinds) if kinds.is_empty() => write!(out, " fonts=none")?,
            Ok(kinds) => {
                let kinds: Vec<String> = kinds.iter().map(|k| format!("{k:?}")).collect();
                write!(out, " fonts={}", kinds.join(","))?;
            }
            Err(e) => write!(out, " fonts={}", render_category(&e))?,
        }
        let mut budget = Budget::new(limits, CancellationToken::new());
        match page_text_profile(document.file(), page, &mut budget) {
            Ok(profile) => {
                write!(out, " text={}", list(profile.sources.iter()))?;
                if !profile.cid_encodings.is_empty() {
                    write!(out, " cmaps={}", list(profile.cid_encodings.iter()))?;
                }
            }
            Err(e) => write!(out, " text={}", render_category(&e))?,
        }
        let mut budget = Budget::new(limits, CancellationToken::new());
        match page_text(document.file(), page, &RenderLimits::default(), &mut budget) {
            Ok(text) => {
                let count = |source| text.glyphs.iter().filter(|g| g.source == source).count();
                write!(
                    out,
                    " glyphs={} unknown={} other-length={} unread={}",
                    text.glyphs.len(),
                    count(UnicodeSource::Unknown),
                    count(UnicodeSource::ToUnicodeOtherLength),
                    text.unread_bytes
                )?;
            }
            Err(e) => write!(out, " glyphs={}", render_category(&e))?,
        }
    }
    // Checked last: an object found by a scan during rendering also counts.
    if let Some(repair) = document.file().repair() {
        let when = if repair.trailer == RepairTrailer::Original {
            "on-lookup/"
        } else {
            ""
        };
        write!(out, " repaired={when}{}", syntax_category(&repair.cause))?;
    }
    writeln!(out)?;
    Ok(0)
}

/// `items` as a comma-separated list of their `Debug` names, or `none`.
fn list<T: std::fmt::Debug>(items: impl Iterator<Item = T>) -> String {
    let names: Vec<String> = items.map(|item| format!("{item:?}")).collect();
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(",")
    }
}

/// Reads the regular file at `path` within the input limit; the error is a
/// category.
fn read(path: &Path, limits: &ResourceLimits) -> Result<MemorySource, String> {
    let io_category = |e: io::Error| format!("io/{:?}", e.kind());
    let metadata = std::fs::metadata(path).map_err(io_category)?;
    if !metadata.is_file() {
        return Err("io/NotAFile".to_owned());
    }
    let file = File::open(path).map_err(io_category)?;
    MemorySource::from_reader(file, Some(metadata.len()), limits).map_err(|e| syntax_category(&e))
}

pub fn syntax_category(error: &pdf_syntax::Error) -> String {
    use pdf_syntax::Error as E;
    match error {
        E::LimitExceeded { kind, .. } => format!("limit/{kind:?}"),
        E::Cancelled => "cancelled".to_owned(),
        E::Malformed { kind, .. } => format!("malformed/{kind:?}"),
        E::Truncated { .. } => "truncated".to_owned(),
        E::Io(kind) => format!("io/{kind:?}"),
        E::Unsupported(capability) => format!("unsupported/{capability:?}"),
        _ => "syntax/other".to_owned(),
    }
}

pub fn document_category(error: &pdf_document::Error) -> String {
    match error {
        pdf_document::Error::Syntax(e) => syntax_category(e),
        pdf_document::Error::InvalidStructure { kind, .. } => format!("structure/{kind:?}"),
        _ => "document/other".to_owned(),
    }
}

pub fn render_category(error: &pdf_render::Error) -> String {
    use pdf_render::{ContentErrorKind, Error as E};
    match error {
        E::Syntax(e) => syntax_category(e),
        E::Document(e) => document_category(e),
        E::Content {
            kind: ContentErrorKind::Syntax(kind),
            ..
        } => format!("content/Syntax/{kind:?}"),
        E::Content { kind, .. } => format!("content/{kind:?}"),
        E::LimitExceeded { kind, .. } => format!("render-limit/{kind:?}"),
        E::InvalidRequest => "invalid-request".to_owned(),
        _ => "render/other".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use pdf_syntax::{LimitKind, MalformedKind};

    use super::*;

    #[test]
    fn categories_drop_offsets_and_objects() {
        let malformed = pdf_syntax::Error::Malformed {
            offset: 1234,
            kind: MalformedKind::MissingEndobj,
        };
        assert_eq!(syntax_category(&malformed), "malformed/MissingEndobj");
        let limit = pdf_syntax::Error::LimitExceeded {
            kind: LimitKind::DecodedBytes,
            limit: 7,
        };
        assert_eq!(
            render_category(&pdf_render::Error::Syntax(limit)),
            "limit/DecodedBytes"
        );
        let content = pdf_render::Error::Content {
            page: pdf_syntax::ObjectId {
                number: 3,
                generation: 0,
            },
            offset: 99,
            kind: pdf_render::ContentErrorKind::Syntax(MalformedKind::InvalidNumber),
        };
        assert_eq!(render_category(&content), "content/Syntax/InvalidNumber");
    }
}
