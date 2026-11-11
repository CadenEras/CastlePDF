//! Which file to open from what the operating system hands over: paths
//! dropped on the window, or URLs from the Finder (macOS). One file at a
//! time: when several are given, none is opened, rather than one picked
//! silently. Only local files: any other URL is ignored, never fetched.

use std::path::PathBuf;

#[cfg(any(test, target_os = "macos"))]
use tauri::Url;

/// The file to open from dropped paths: exactly one.
pub fn single_path(paths: &[PathBuf]) -> Option<PathBuf> {
    match paths {
        [path] => Some(path.clone()),
        _ => None,
    }
}

/// The file to open from URLs given by the Finder (`RunEvent::Opened`):
/// exactly one, with the `file` scheme and a local path.
#[cfg(any(test, target_os = "macos"))]
pub fn single_file_url(urls: &[Url]) -> Option<PathBuf> {
    match urls {
        [url] if url.scheme() == "file" => url.to_file_path().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    #[test]
    fn one_dropped_file_is_opened() {
        let path = local("a.pdf");
        assert_eq!(single_path(std::slice::from_ref(&path)), Some(path));
    }

    #[test]
    fn several_or_no_dropped_files_open_nothing() {
        assert_eq!(single_path(&[]), None);
        assert_eq!(single_path(&[local("a.pdf"), local("b.pdf")]), None);
    }

    #[test]
    fn one_file_url_gives_its_local_path() {
        let path = local("a b é.pdf");
        let url = Url::from_file_path(&path).unwrap();
        assert_eq!(single_file_url(&[url]), Some(path));
    }

    #[test]
    fn other_schemes_are_ignored() {
        for url in [
            "https://example.com/a.pdf",
            "http://localhost/a.pdf",
            "castlepdf://open?a.pdf",
        ] {
            assert_eq!(single_file_url(&[Url::parse(url).unwrap()]), None, "{url}");
        }
    }

    #[test]
    fn several_or_no_urls_open_nothing() {
        let url = Url::from_file_path(local("a.pdf")).unwrap();
        assert_eq!(single_file_url(&[]), None);
        assert_eq!(single_file_url(&[url.clone(), url]), None);
    }

    /// A file URL naming another host is not a local path on Unix (Windows
    /// would map it to a UNC path, but the event only exists on macOS).
    #[cfg(unix)]
    #[test]
    fn a_file_url_on_another_host_is_ignored() {
        let url = Url::parse("file://server.example/share/a.pdf").unwrap();
        assert_eq!(single_file_url(&[url]), None);
    }
}
