//! `castle-pdf render <file> <page> <output.ppm> [--scale <factor>]`: renders
//! one page to a binary PPM image (P6, 8 bits per channel), the format that
//! `pdftoppm` writes, for comparisons. The output file must not exist: the
//! command never overwrites a file, the input document included.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;

use pdf_document::Document;
use pdf_render::{RenderOptions, Unsupported, render_page};
use pdf_syntax::{Budget, CancellationToken, ResourceLimits};

/// Parsed arguments; `None` is a usage error.
pub struct Request<'a> {
    input: &'a Path,
    /// Zero-based page index.
    page: usize,
    output: &'a Path,
    scale: f64,
}

impl<'a> Request<'a> {
    /// `<file> <page> <output> [--scale <factor>]`, page numbered from 1.
    pub fn parse(args: &[&'a OsStr]) -> Option<Self> {
        let (input, page, output, scale) = match args {
            [input, page, output] => (input, page, output, 1.0),
            [input, page, output, flag, scale] if *flag == "--scale" => {
                (input, page, output, scale.to_str()?.parse().ok()?)
            }
            _ => return None,
        };
        let page: usize = page.to_str()?.parse().ok()?;
        Some(Self {
            input: Path::new(*input),
            page: page.checked_sub(1)?,
            output: Path::new(*output),
            scale,
        })
    }
}

/// Renders the page and writes the image; returns 0 or 1. A one-line summary
/// goes to `out`. An `Err` means that writing to `out` or `err` failed.
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
    let Some(page) = document.page(request.page) else {
        writeln!(
            err,
            "error: no page {} (the document has {})",
            request.page + 1,
            document.page_count()
        )?;
        return Ok(1);
    };
    let mut options = RenderOptions::default();
    options.scale = request.scale;
    let rendering = match render_page(document.file(), page, &options, &mut budget) {
        Ok(rendering) => rendering,
        Err(e) => {
            writeln!(err, "error: {e}")?;
            return Ok(1);
        }
    };
    let bitmap = &rendering.bitmap;
    if request.output.exists() {
        writeln!(err, "error: cannot create the image: the output exists")?;
        return Ok(1);
    }
    if let Err(e) = publish(request.output, |file| {
        write_ppm(file, bitmap.width(), bitmap.height(), bitmap.data())
    }) {
        writeln!(err, "error: cannot create the image: {}", e.kind())?;
        return Ok(1);
    }
    let skipped: Vec<&str> = rendering.skipped.iter().map(|s| name(*s)).collect();
    writeln!(
        out,
        "page {}: {}x{} pixels; not rendered: {}",
        request.page + 1,
        bitmap.width(),
        bitmap.height(),
        if skipped.is_empty() {
            "none".to_owned()
        } else {
            skipped.join(", ")
        }
    )?;
    Ok(0)
}

/// Writes a new file at `output` through `write`, never overwriting one:
/// the data goes to a temporary file next to it (created exclusively), which
/// is then hard-linked to `output` (refused if `output` exists) and removed.
/// After any failure, nothing is left at `output` nor in the temporary file.
fn publish(output: &Path, write: impl FnOnce(File) -> io::Result<()>) -> io::Result<()> {
    let name = output
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut temporary_name = std::ffi::OsString::from(".");
    temporary_name.push(name);
    temporary_name.push(format!(".{}.tmp", std::process::id()));
    let temporary = output.with_file_name(temporary_name);
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .and_then(|file| {
            let written = write(file).and_then(|()| std::fs::hard_link(&temporary, output));
            let _ = std::fs::remove_file(&temporary);
            written
        })
}

/// Binary PPM of opaque RGBA pixels: the alpha channel is dropped (pages are
/// rendered on an opaque white background).
fn write_ppm(file: File, width: u32, height: u32, rgba: &[u8]) -> io::Result<()> {
    let mut writer = BufWriter::new(file);
    write!(writer, "P6\n{width} {height}\n255\n")?;
    let (pixels, _) = rgba.as_chunks::<4>();
    for [r, g, b, _] in pixels {
        writer.write_all(&[*r, *g, *b])?;
    }
    writer.into_inner().map_err(|e| e.into_error())?.sync_all()
}

fn name(capability: Unsupported) -> &'static str {
    match capability {
        Unsupported::Text => "text",
        Unsupported::TextClip => "text clipping",
        Unsupported::Image => "inline images",
        Unsupported::XObject => "XObjects",
        Unsupported::Shading => "shadings",
        Unsupported::Pattern => "patterns",
        Unsupported::ColorSpace => "colour spaces",
        Unsupported::BlendMode => "blend modes",
        Unsupported::SoftMask => "soft masks",
        Unsupported::AlphaIsShape => "alpha is shape",
        Unsupported::TransferFunction => "transfer functions",
        Unsupported::OptionalContent => "optional content",
        Unsupported::FontSubstituted => "original fonts (substituted)",
        _ => "other",
    }
}
