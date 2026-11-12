//! `castle-pdf`: diagnostic command line over the CastlePDF engine.
//!
//! `castle-pdf inspect <file>` reports the file structure (version, revisions,
//! cross-reference entries, trailer keys) and the pages (ids, boxes, rotation).
//! It never prints document content: no strings, streams or resource values,
//! and only standard trailer key names. `castle-pdf render` writes one page to
//! a new PPM image. `castle-pdf text` prints the text of the pages: the only
//! command that prints document content, on request. `castle-pdf survey` measures how the engine handles every
//! file of a directory, each probed in a child process (`castle-pdf probe`),
//! and reports aggregated counts only. Exit codes: 0 success, 1 the document
//! cannot be read or rendered, or the image cannot be created, or a surveyed
//! file crashed or timed out, 2 usage error, 3 the report could not be written.

mod inspect;
mod open;
mod probe;
mod render;
mod survey;
mod text;

use std::io::Write;
use std::process::ExitCode;

const USAGE: &str = "\
castle-pdf — diagnostic tool of the CastlePDF engine

Usage:
  castle-pdf inspect <file>   Report the file structure and the pages
  castle-pdf render <file> <page> <output.ppm> [--scale <factor>]
                              Render a page (numbered from 1) to a new PPM
                              image; scale 1 is 72 pixels per inch
  castle-pdf text <file> [--page <n>]
                              Print the text of a page (numbered from 1), or
                              of every page separated by form feeds, in
                              content order; a summary per page goes to the
                              error output
  castle-pdf survey <directory> [--timeout <seconds>] [--jobs <n>] [--paths]
                              Open every file under the directory and render
                              its first page, each in a child process killed
                              after the timeout (default 30 s, 4 jobs); report
                              aggregated counts, and with --paths the files
                              not rendered
  castle-pdf probe <file>     One survey step: print a line describing how the
                              file opens and renders
  castle-pdf --help, -h       Show this help
  castle-pdf --version        Show the version

`inspect` lists structure only, never document content; `text` prints the
document's text, with control characters replaced by U+FFFD. `render` never
overwrites a file and lists what the page uses but it cannot render yet.
`survey` reports never hold document content, and file names only with
--paths. Exit codes: 0 success, 1 the document cannot be read or rendered, or
the image cannot be created (the error says why), or a surveyed file crashed or
timed out, 2 usage error, 3 the report could not be written (for example a
closed pipe).
";

/// Exit code when writing the report or a message fails.
const WRITE_FAILED: u8 = 3;

fn main() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let code = run(&args, &mut stdout.lock(), &mut stderr.lock());
    ExitCode::from(code)
}

/// Runs one command; returns the process exit code.
fn run(args: &[std::ffi::OsString], out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let args: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
    let result = match args.as_slice() {
        [flag] if *flag == "--help" || *flag == "-h" => out.write_all(USAGE.as_bytes()).map(|()| 0),
        [flag] if *flag == "--version" => {
            writeln!(out, "castle-pdf {}", env!("CARGO_PKG_VERSION")).map(|()| 0)
        }
        [command, path] if *command == "inspect" => inspect::run(path.as_ref(), out, err),
        [command, rest @ ..] if *command == "render" => match render::Request::parse(rest) {
            Some(request) => render::run(&request, out, err),
            None => err.write_all(USAGE.as_bytes()).map(|()| 2),
        },
        [command, rest @ ..] if *command == "text" => match text::Request::parse(rest) {
            Some(request) => text::run(&request, out, err),
            None => err.write_all(USAGE.as_bytes()).map(|()| 2),
        },
        [command, rest @ ..] if *command == "survey" => match survey::Request::parse(rest) {
            Some(request) => survey::run(&request, out, err),
            None => err.write_all(USAGE.as_bytes()).map(|()| 2),
        },
        [command, path] if *command == "probe" => probe::run(path.as_ref(), out),
        _ => err.write_all(USAGE.as_bytes()).map(|()| 2),
    };
    // A closed output (such as a pipe to `head`) is not a document error: it
    // gets its own code, so that scripts can tell the two apart.
    match result.and_then(|code| out.flush().map(|()| code)) {
        Ok(code) => code,
        Err(_) => WRITE_FAILED,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn call(args: &[&str]) -> (u8, String, String) {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&args, &mut out, &mut err);
        (
            code,
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
        )
    }

    /// An output that refuses every write, like a pipe closed by its reader.
    struct ClosedPipe;

    impl Write for ClosedPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_closed_output_has_its_own_exit_code() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/corpus/public/minimal.pdf"
        );
        let args = [OsString::from("inspect"), OsString::from(fixture)];
        let mut err = Vec::new();
        assert_eq!(run(&args, &mut ClosedPipe, &mut err), WRITE_FAILED);
        assert_eq!(
            run(&[OsString::from("--help")], &mut ClosedPipe, &mut err),
            WRITE_FAILED
        );
        // The same file with a working output succeeds.
        assert_eq!(run(&args, &mut Vec::new(), &mut err), 0);
    }

    #[test]
    fn help_version_and_usage_errors() {
        let (code, out, _) = call(&["-h"]);
        assert_eq!(code, 0);
        assert!(out.contains("--help, -h"));
        let (code, out, _) = call(&["--help"]);
        assert_eq!(code, 0);
        assert!(out.contains("castle-pdf inspect <file>"));
        let (code, out, _) = call(&["--version"]);
        assert_eq!(
            (code, out.trim()),
            (0, concat!("castle-pdf ", env!("CARGO_PKG_VERSION")))
        );
        for args in [
            &[][..],
            &["inspect"],
            &["inspect", "a", "b"],
            &["open", "a"],
            &["render", "a", "1"],
            &["render", "a", "0", "b"],
            &["render", "a", "x", "b"],
            &["render", "a", "1", "b", "--scale"],
            &["render", "a", "1", "b", "--zoom", "2"],
            &["render", "a", "1", "b", "--scale", "big"],
            &["survey"],
            &["survey", "d", "--timeout"],
            &["survey", "d", "--timeout", "0"],
            &["survey", "d", "--timeout", "-1"],
            &["survey", "d", "--timeout", "NaN"],
            &["survey", "d", "--timeout", "3601"],
            &["survey", "d", "--jobs", "0"],
            &["survey", "d", "--jobs", "65"],
            &["survey", "d", "--paths", "x"],
            &["probe"],
            &["probe", "a", "b"],
        ] {
            let (code, out, err) = call(args);
            assert_eq!(code, 2, "{args:?}");
            assert!(out.is_empty());
            assert!(err.contains("Usage:"));
        }
    }
}
