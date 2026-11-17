//! "Save as", transactional (ADR-0008, point 4; tranche 4.5): the bytes of
//! a rewritten document written to a temporary file in the destination's
//! directory, flushed to disk, read back by the strict reader (its page
//! count checked), then renamed over the destination. Any failure removes
//! the temporary file and leaves the destination as it was; a source is
//! never the destination.
//!
//! The rename replaces the destination's directory entry, never the file
//! it named: a destination that is a link (symbolic or hard) to a source
//! is replaced as a link, the source keeps its content. The destination
//! itself is refused when it is, once links are resolved, one of the
//! sources.
//!
//! Not covered: a filesystem without an atomic rename (some network
//! shares) may leave neither file under the destination's name if the
//! machine stops during the rename itself; the temporary file then holds
//! the complete document.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use pdf_document::Document;
use pdf_syntax::{Budget, CancellationToken, MemorySource, PdfFile, ResourceLimits};

/// Why a save failed; the destination is as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SaveError {
    /// The destination is, links resolved, one of the sources.
    SameAsSource,
    /// The destination has no file name, or its directory does not exist
    /// or is not a directory, or the destination is a directory.
    InvalidDestination,
    /// Writing, flushing or renaming failed (full disk, permission,
    /// destination locked...).
    Io(io::ErrorKind),
    /// What was written does not read back with the strict reader.
    ReadBack(String),
    /// What was written reads back with another page count.
    PageCount { expected: usize, found: usize },
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SameAsSource => write!(f, "the destination is a document being saved"),
            Self::InvalidDestination => write!(f, "the destination is not a file in a folder"),
            Self::Io(kind) => write!(f, "cannot write the destination: {kind}"),
            Self::ReadBack(e) => write!(f, "the written document does not read back: {e}"),
            Self::PageCount { expected, found } => write!(
                f,
                "the written document reads back with {found} pages, not {expected}"
            ),
        }
    }
}

impl std::error::Error for SaveError {}

/// Writes `bytes`, a document of `pages` pages, to `destination`, through a
/// checked temporary file (see the module). `sources` are the files the
/// document was read from: none may be the destination. The read back is
/// bounded by `limits`.
pub fn save_as(
    bytes: &[u8],
    pages: usize,
    destination: &Path,
    sources: &[&Path],
    limits: &ResourceLimits,
) -> Result<(), SaveError> {
    let (target, resolved) = resolved_destination(destination)?;
    for source in sources {
        if std::fs::canonicalize(source).is_ok_and(|source| source == resolved) {
            return Err(SaveError::SameAsSource);
        }
    }
    let (temporary, file) = create_temporary(&target)?;
    let result = write_and_check(file, &temporary, bytes, pages, limits)
        .and_then(|()| std::fs::rename(&temporary, &target).map_err(|e| SaveError::Io(e.kind())));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
        return result;
    }
    sync_directory(&target);
    Ok(())
}

/// The destination's entry, its directory resolved, and the file it
/// names, links resolved, to compare with the sources (the entry itself
/// when it does not exist yet).
fn resolved_destination(destination: &Path) -> Result<(PathBuf, PathBuf), SaveError> {
    let name = destination
        .file_name()
        .ok_or(SaveError::InvalidDestination)?;
    let directory = match destination.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let directory = std::fs::canonicalize(directory).map_err(|_| SaveError::InvalidDestination)?;
    if !directory.is_dir() {
        return Err(SaveError::InvalidDestination);
    }
    let entry = directory.join(name);
    match std::fs::metadata(&entry) {
        Ok(metadata) if metadata.is_dir() => Err(SaveError::InvalidDestination),
        Ok(_) => {
            let resolved = std::fs::canonicalize(&entry).map_err(|e| SaveError::Io(e.kind()))?;
            Ok((entry, resolved))
        }
        // Not there, or a link leading nowhere: replaced as an entry.
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((entry.clone(), entry)),
        Err(e) => Err(SaveError::Io(e.kind())),
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A new temporary file next to `target`, hidden, named after it.
fn create_temporary(target: &Path) -> Result<(PathBuf, File), SaveError> {
    let name = target.file_name().ok_or(SaveError::InvalidDestination)?;
    for _ in 0..16 {
        let mut temporary_name = std::ffi::OsString::from(".");
        temporary_name.push(name);
        temporary_name.push(format!(
            ".{}-{}.castle.tmp",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let temporary = target.with_file_name(temporary_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(SaveError::Io(e.kind())),
        }
    }
    Err(SaveError::Io(io::ErrorKind::AlreadyExists))
}

/// `bytes` written to `file` and flushed, then `temporary` read back from
/// disk by the strict reader, with `pages` pages.
fn write_and_check(
    mut file: File,
    temporary: &Path,
    bytes: &[u8],
    pages: usize,
    limits: &ResourceLimits,
) -> Result<(), SaveError> {
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| SaveError::Io(e.kind()))?;
    drop(file);
    let written = std::fs::read(temporary).map_err(|e| SaveError::Io(e.kind()))?;
    let unreadable = |e: &dyn std::fmt::Display| SaveError::ReadBack(e.to_string());
    let source = MemorySource::from_vec(written, limits).map_err(|e| unreadable(&e))?;
    let mut budget = Budget::new(*limits, CancellationToken::new());
    let file = PdfFile::open_strict(source, &mut budget).map_err(|e| unreadable(&e))?;
    let document = Document::from_file(file, &mut budget).map_err(|e| unreadable(&e))?;
    if document.page_count() != pages {
        return Err(SaveError::PageCount {
            expected: pages,
            found: document.page_count(),
        });
    }
    Ok(())
}

/// The rename made durable where the system allows it (a directory can be
/// flushed on Unix); a failure there does not undo the save.
fn sync_directory(target: &Path) {
    #[cfg(unix)]
    if let Some(directory) = target.parent()
        && let Ok(directory) = File::open(directory)
    {
        let _ = directory.sync_all();
    }
    #[cfg(not(unix))]
    let _ = target;
}
