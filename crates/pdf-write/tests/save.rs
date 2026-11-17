//! "Save as" through a checked temporary file (tranche 4.5): what is
//! written reads back, and every failure leaves the destination as it was,
//! with no temporary file left behind.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers: a failed expectation is a test failure"
)]

use std::path::{Path, PathBuf};

use pdf_document::Document;
use pdf_syntax::{Budget, CancellationToken, MemorySource, ResourceLimits};
use pdf_write::{SaveError, identity_plan, rewrite, save_as};

/// A rewritten public fixture of one page.
fn document() -> Vec<u8> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/public/minimal.pdf");
    let bytes = std::fs::read(path).expect("fixture");
    let mut budget = Budget::new(ResourceLimits::default(), CancellationToken::new());
    let source = MemorySource::from_vec(bytes, &ResourceLimits::default()).expect("source");
    let original = Document::open(source, &mut budget).expect("document");
    assert_eq!(original.page_count(), 1);
    rewrite(&original, &identity_plan(&original), 1 << 24, &mut budget)
        .expect("rewrite")
        .bytes
}

/// An empty directory of its own for a test.
fn directory(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("save")
        .join(name);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("directory");
    path
}

/// The names in `directory`, sorted.
fn entries(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .expect("list")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn limits() -> ResourceLimits {
    ResourceLimits::default()
}

#[test]
fn a_document_is_saved_and_an_other_file_replaced() {
    let dir = directory("saved");
    let bytes = document();
    let destination = dir.join("out.pdf");
    save_as(&bytes, 1, &destination, &[], &limits()).expect("save");
    assert_eq!(std::fs::read(&destination).expect("read"), bytes);
    // An existing file that is no source is replaced.
    std::fs::write(&destination, b"older").expect("older");
    save_as(&bytes, 1, &destination, &[], &limits()).expect("replace");
    assert_eq!(std::fs::read(&destination).expect("read"), bytes);
    assert_eq!(entries(&dir), vec!["out.pdf"]);
}

#[test]
fn a_source_is_never_the_destination() {
    let dir = directory("source");
    let bytes = document();
    let source = dir.join("source.pdf");
    std::fs::write(&source, b"source content").expect("source");
    // The same file, named directly or through another spelling.
    let spellings = [source.clone(), dir.join(".").join("source.pdf")];
    for destination in spellings {
        assert_eq!(
            save_as(&bytes, 1, &destination, &[&source], &limits()),
            Err(SaveError::SameAsSource)
        );
    }
    assert_eq!(std::fs::read(&source).expect("read"), b"source content");
    assert_eq!(entries(&dir), vec!["source.pdf"]);
}

#[cfg(unix)]
#[test]
fn a_link_to_a_source_is_refused() {
    let dir = directory("link");
    let bytes = document();
    let source = dir.join("source.pdf");
    std::fs::write(&source, b"source content").expect("source");
    let link = dir.join("link.pdf");
    std::os::unix::fs::symlink(&source, &link).expect("link");
    assert_eq!(
        save_as(&bytes, 1, &link, &[&source], &limits()),
        Err(SaveError::SameAsSource)
    );
    assert_eq!(std::fs::read(&source).expect("read"), b"source content");
}

#[test]
fn what_does_not_read_back_is_not_saved() {
    let dir = directory("unreadable");
    let destination = dir.join("out.pdf");
    std::fs::write(&destination, b"older").expect("older");
    let bytes = document();
    // Not a PDF; a PDF with another page count.
    assert!(matches!(
        save_as(b"not a pdf", 1, &destination, &[], &limits()),
        Err(SaveError::ReadBack(_))
    ));
    assert_eq!(
        save_as(&bytes, 2, &destination, &[], &limits()),
        Err(SaveError::PageCount {
            expected: 2,
            found: 1
        })
    );
    // A truncated file: the strict reader refuses it.
    assert!(matches!(
        save_as(&bytes[..bytes.len() / 2], 1, &destination, &[], &limits()),
        Err(SaveError::ReadBack(_))
    ));
    assert_eq!(std::fs::read(&destination).expect("read"), b"older");
    assert_eq!(entries(&dir), vec!["out.pdf"]);
}

#[test]
fn a_destination_that_is_no_file_in_a_folder_is_refused() {
    let dir = directory("invalid");
    let bytes = document();
    let missing = dir.join("missing").join("out.pdf");
    assert_eq!(
        save_as(&bytes, 1, &missing, &[], &limits()),
        Err(SaveError::InvalidDestination)
    );
    std::fs::create_dir(dir.join("folder.pdf")).expect("folder");
    assert_eq!(
        save_as(&bytes, 1, &dir.join("folder.pdf"), &[], &limits()),
        Err(SaveError::InvalidDestination)
    );
    assert_eq!(
        save_as(&bytes, 1, Path::new(""), &[], &limits()),
        Err(SaveError::InvalidDestination)
    );
    assert_eq!(entries(&dir), vec!["folder.pdf"]);
}

#[cfg(windows)]
#[test]
fn a_read_only_destination_is_left_as_it_was() {
    let dir = directory("read-only");
    let destination = dir.join("out.pdf");
    std::fs::write(&destination, b"older").expect("older");
    let mut permissions = std::fs::metadata(&destination)
        .expect("metadata")
        .permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&destination, permissions.clone()).expect("read-only");
    let result = save_as(&document(), 1, &destination, &[], &limits());
    assert!(matches!(result, Err(SaveError::Io(_))), "{result:?}");
    assert_eq!(std::fs::read(&destination).expect("read"), b"older");
    assert_eq!(entries(&dir), vec!["out.pdf"]);
    #[allow(clippy::permissions_set_readonly_false, reason = "test cleanup")]
    permissions.set_readonly(false);
    let _ = std::fs::set_permissions(&destination, permissions);
}

#[cfg(unix)]
#[test]
fn a_read_only_folder_is_left_as_it_was() {
    use std::os::unix::fs::PermissionsExt;
    let dir = directory("read-only");
    let destination = dir.join("out.pdf");
    std::fs::write(&destination, b"older").expect("older");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).expect("read-only");
    let result = save_as(&document(), 1, &destination, &[], &limits());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("restore");
    // Root may write anyway: then the save succeeded, as it should.
    if result.is_err() {
        assert!(matches!(result, Err(SaveError::Io(_))), "{result:?}");
        assert_eq!(std::fs::read(&destination).expect("read"), b"older");
    }
    assert_eq!(entries(&dir), vec!["out.pdf"]);
}
