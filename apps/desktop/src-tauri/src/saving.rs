//! The destination of "save as" (ADR-0009): a path the native dialog gave,
//! checked by the shell before the worker writes to it. The UI never names
//! it, and never learns it.

use std::path::{Path, PathBuf};

/// The name the dialog proposes for a copy of `source`.
pub fn suggested_name(source: &Path) -> String {
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "document".to_owned());
    format!("{stem} (copie).pdf")
}

/// `chosen` as written: absolute, ending in `.pdf` (added when the name has
/// another extension or none), and never one of `sources`, links resolved.
/// The error is the reason named to the UI.
pub fn checked_destination(chosen: &Path, sources: &[&Path]) -> Result<PathBuf, &'static str> {
    if !chosen.is_absolute() {
        return Err("invalidDestination");
    }
    let name = chosen.file_name().ok_or("invalidDestination")?;
    let is_pdf = Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
    let destination = if is_pdf {
        chosen.to_path_buf()
    } else {
        let mut name = name.to_os_string();
        name.push(".pdf");
        chosen.with_file_name(name)
    };
    let resolved = resolved(&destination);
    if sources
        .iter()
        .any(|source| std::fs::canonicalize(source).is_ok_and(|source| Some(source) == resolved))
    {
        return Err("sameAsSource");
    }
    Ok(destination)
}

/// `path` with its links resolved: the file itself when it exists, else its
/// folder and its name; `None` when the folder does not exist either (the
/// worker then refuses the destination).
fn resolved(path: &Path) -> Option<PathBuf> {
    if let Ok(resolved) = std::fs::canonicalize(path) {
        return Some(resolved);
    }
    let folder = std::fs::canonicalize(path.parent()?).ok()?;
    Some(folder.join(path.file_name()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("castle-saving-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap_or_else(|e| panic!("{e}"));
        path
    }

    #[test]
    fn the_proposed_name_is_a_copy_of_the_source() {
        assert_eq!(
            suggested_name(Path::new("/docs/rapport.pdf")),
            "rapport (copie).pdf"
        );
        assert_eq!(suggested_name(Path::new("/")), "document (copie).pdf");
    }

    #[test]
    fn a_destination_ends_in_pdf_and_is_absolute() {
        let dir = folder("extension");
        assert_eq!(
            checked_destination(&dir.join("out"), &[]),
            Ok(dir.join("out.pdf"))
        );
        assert_eq!(
            checked_destination(&dir.join("out.v2"), &[]),
            Ok(dir.join("out.v2.pdf"))
        );
        assert_eq!(
            checked_destination(&dir.join("OUT.PDF"), &[]),
            Ok(dir.join("OUT.PDF"))
        );
        assert_eq!(
            checked_destination(Path::new("relative.pdf"), &[]),
            Err("invalidDestination")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_source_is_never_the_destination() {
        let dir = folder("source");
        let source = dir.join("source.pdf");
        std::fs::write(&source, b"%PDF").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            checked_destination(&source, &[&source]),
            Err("sameAsSource")
        );
        // Another spelling of the same file, and the name without extension.
        assert_eq!(
            checked_destination(&dir.join(".").join("source.pdf"), &[&source]),
            Err("sameAsSource")
        );
        assert_eq!(
            checked_destination(&dir.join("source"), &[&source]),
            Err("sameAsSource")
        );
        assert_eq!(
            checked_destination(&dir.join("other.pdf"), &[&source]),
            Ok(dir.join("other.pdf"))
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
