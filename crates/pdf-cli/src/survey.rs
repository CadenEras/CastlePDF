//! `castle-pdf survey <directory> [--timeout <seconds>] [--jobs <n>] [--paths]`:
//! measures how the engine handles every regular file under a directory.
//!
//! Each file is probed in its own child process (`castle-pdf probe`), killed
//! after the timeout: a panic, an abort or a hang in the engine becomes a
//! counted outcome instead of stopping the survey. The report aggregates the
//! outcomes (open and render rates, error categories, capabilities not
//! rendered, crashes, timeouts, time per file). It never holds document
//! content, and file paths only with `--paths`, since names can be private.
//! Exit code 0 when no file crashed or timed out, 1 otherwise (or when the
//! directory cannot be read).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Files listed at most: the list itself is held in memory.
pub const MAX_FILES: usize = 1_000_000;
/// Bytes of a probe's output read at most; a well-formed line is far shorter.
const MAX_PROBE_OUTPUT: u64 = 4096;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Each probe may hold the whole input (512 MiB by default), its decoded
/// streams and a bitmap: a few parallel probes, not one per core.
const DEFAULT_JOBS: usize = 4;
const MAX_JOBS: usize = 64;

/// Parsed arguments; `None` is a usage error.
pub struct Request<'a> {
    directory: &'a Path,
    timeout: Duration,
    jobs: usize,
    paths: bool,
}

impl<'a> Request<'a> {
    /// `<directory> [--timeout <seconds>] [--jobs <n>] [--paths]`.
    pub fn parse(args: &[&'a OsStr]) -> Option<Self> {
        let (directory, mut rest) = args.split_first()?;
        let mut request = Self {
            directory: Path::new(*directory),
            timeout: DEFAULT_TIMEOUT,
            jobs: DEFAULT_JOBS,
            paths: false,
        };
        while let Some((flag, tail)) = rest.split_first() {
            rest = tail;
            if *flag == "--paths" {
                request.paths = true;
                continue;
            }
            let (value, tail) = rest.split_first()?;
            rest = tail;
            let value = value.to_str()?;
            if *flag == "--timeout" {
                let seconds: f64 = value.parse().ok()?;
                if !(seconds > 0.0 && seconds <= 3600.0) {
                    return None;
                }
                request.timeout = Duration::try_from_secs_f64(seconds).ok()?;
            } else if *flag == "--jobs" {
                request.jobs = value.parse().ok().filter(|n| (1..=MAX_JOBS).contains(n))?;
            } else {
                return None;
            }
        }
        Some(request)
    }
}

/// What happened to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    /// The probe ran to its end. `render` is `None` when opening failed.
    Probed {
        open: String,
        render: Option<String>,
        skipped: Vec<String>,
        /// Category of the strict error, for a file opened by repair.
        repaired: Option<String>,
    },
    /// Killed after the timeout.
    Timeout,
    /// The probe ended without its line: panic, abort, signal.
    Crash(String),
    /// The probe could not be started.
    NotStarted(io::ErrorKind),
}

struct Outcome {
    status: Status,
    time: Duration,
}

/// Runs the survey; returns 0, or 1 when a file crashed or timed out or the
/// directory cannot be read. An `Err` means that writing the report failed.
pub fn run(request: &Request<'_>, out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
    let files = match list(request.directory) {
        Ok(files) => files,
        Err(e) => {
            writeln!(err, "error: cannot list the directory: {}", e.kind())?;
            return Ok(1);
        }
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            writeln!(
                err,
                "error: cannot find the castle-pdf executable: {}",
                e.kind()
            )?;
            return Ok(1);
        }
    };
    let outcomes = probe_all(&exe, &files, request);
    let failed = outcomes.iter().any(|o| {
        matches!(
            o.status,
            Status::Timeout | Status::Crash(_) | Status::NotStarted(_)
        )
    });
    report(&outcomes, out)?;
    if request.paths {
        paths(request.directory, &files, &outcomes, out)?;
    }
    Ok(u8::from(failed))
}

/// Regular files under `directory`, recursively, sorted; symbolic links are
/// not followed.
fn list(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                if files.len() == MAX_FILES {
                    return Err(io::Error::other("too many files"));
                }
                files.push(entry.path());
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Probes the files with `request.jobs` worker threads, each running one
/// child process at a time; returns the outcomes in file order.
fn probe_all(exe: &Path, files: &[PathBuf], request: &Request<'_>) -> Vec<Outcome> {
    let next = AtomicUsize::new(0);
    let (sender, receiver) = mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..request.jobs.min(files.len()) {
            let sender = sender.clone();
            let next = &next;
            scope.spawn(move || {
                while let Some(file) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let outcome = probe(exe, file, request.timeout);
                    if sender.send((file, outcome)).is_err() {
                        break;
                    }
                }
            });
        }
    });
    drop(sender);
    let mut outcomes: Vec<(&PathBuf, Outcome)> = receiver.into_iter().collect();
    outcomes.sort_by(|a, b| a.0.cmp(b.0));
    outcomes.into_iter().map(|(_, outcome)| outcome).collect()
}

fn probe(exe: &Path, file: &Path, timeout: Duration) -> Outcome {
    let start = Instant::now();
    let child = Command::new(exe)
        .arg("probe")
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let status = match child {
        Ok(mut child) => match wait(&mut child, start, timeout) {
            Ok(Some(status)) => finished(&mut child, status),
            Ok(None) => Status::Timeout,
            Err(e) => Status::NotStarted(e.kind()),
        },
        Err(e) => Status::NotStarted(e.kind()),
    };
    Outcome {
        status,
        time: start.elapsed(),
    }
}

/// Waits for `child` until `timeout` after `start`; on timeout, kills it and
/// returns `None`. The probe writes one short line, so its output pipe cannot
/// fill up and block it.
fn wait(child: &mut Child, start: Instant, timeout: Duration) -> io::Result<Option<ExitStatus>> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            // The child may have exited meanwhile: a failed kill is harmless.
            let _ = child.kill();
            child.wait()?;
            return Ok(None);
        }
        // Short polls for the many quick files, at most 20 ms for slow ones.
        let poll = (elapsed / 16).clamp(Duration::from_millis(1), Duration::from_millis(20));
        std::thread::sleep(poll.min(timeout - elapsed));
    }
}

fn finished(child: &mut Child, status: ExitStatus) -> Status {
    let mut line = String::new();
    if let Some(stdout) = child.stdout.take() {
        // A read error leaves the line incomplete: it then counts as a crash.
        let _ = stdout.take(MAX_PROBE_OUTPUT).read_to_string(&mut line);
    }
    match (status.code(), parse_line(&line)) {
        (Some(0), Some(probed)) => probed,
        (Some(101), _) => Status::Crash("panic".to_owned()),
        (Some(code), _) => Status::Crash(format!("exit {code}")),
        (None, _) => Status::Crash("signal".to_owned()),
    }
}

/// Parses the line of `castle-pdf probe`; `None` if it is not one.
fn parse_line(line: &str) -> Option<Status> {
    let line = line.strip_suffix('\n')?;
    let (mut open, mut render, mut skipped, mut repaired) = (None, None, Vec::new(), None);
    for token in line.split(' ') {
        let (key, value) = token.split_once('=')?;
        match key {
            "open" => open = Some(value.to_owned()),
            "pages" => {}
            "render" => render = Some(value.to_owned()),
            "skipped" if value != "none" => skipped = value.split(',').map(str::to_owned).collect(),
            "skipped" => {}
            "repaired" => repaired = Some(value.to_owned()),
            _ => return None,
        }
    }
    let open = open?;
    if (open == "ok") != render.is_some() || (repaired.is_some() && render.is_none()) {
        return None;
    }
    Some(Status::Probed {
        open,
        render,
        skipped,
        repaired,
    })
}

fn report(outcomes: &[Outcome], out: &mut dyn Write) -> io::Result<()> {
    let total = outcomes.len();
    let (mut opened, mut rendered, mut timeouts) = (0, 0, 0);
    let mut repairs = BTreeMap::new();
    let mut open_failures = BTreeMap::new();
    let mut render_failures = BTreeMap::new();
    let mut skipped = BTreeMap::new();
    let mut crashes = BTreeMap::new();
    for outcome in outcomes {
        match &outcome.status {
            Status::Probed {
                open,
                render,
                skipped: capabilities,
                repaired,
            } => match render.as_deref() {
                None => *open_failures.entry(open.clone()).or_insert(0) += 1,
                Some(render) => {
                    opened += 1;
                    if let Some(cause) = repaired {
                        *repairs.entry(cause.clone()).or_insert(0) += 1;
                    }
                    if render == "ok" {
                        rendered += 1;
                        for capability in capabilities {
                            *skipped.entry(capability.clone()).or_insert(0) += 1;
                        }
                    } else {
                        *render_failures.entry(render.to_owned()).or_insert(0) += 1;
                    }
                }
            },
            Status::Timeout => timeouts += 1,
            Status::Crash(how) => *crashes.entry(how.clone()).or_insert(0) += 1,
            Status::NotStarted(kind) => {
                *crashes.entry(format!("not started: {kind:?}")).or_insert(0) += 1;
            }
        }
    }
    writeln!(out, "files: {total}")?;
    writeln!(out, "opened: {opened} ({})", percent(opened, total))?;
    let repaired: usize = repairs.values().sum();
    writeln!(
        out,
        "opened by repair: {repaired} ({})",
        percent(repaired, total)
    )?;
    writeln!(
        out,
        "first page rendered: {rendered} ({})",
        percent(rendered, total)
    )?;
    writeln!(out, "timeouts: {timeouts}")?;
    writeln!(out, "crashes: {}", crashes.values().sum::<usize>())?;
    let mut times: Vec<Duration> = outcomes.iter().map(|o| o.time).collect();
    times.sort();
    if let (Some(median), Some(p95), Some(max)) = (
        times.get(total.saturating_sub(1) / 2),
        times.get((total * 95).div_ceil(100).saturating_sub(1)),
        times.last(),
    ) {
        writeln!(
            out,
            "time per file (process included): median {} ms, p95 {} ms, max {} ms",
            median.as_millis(),
            p95.as_millis(),
            max.as_millis()
        )?;
    }
    section(out, "crashes", &crashes)?;
    section(out, "open failures", &open_failures)?;
    section(out, "repaired, by strict error", &repairs)?;
    section(out, "first page failures", &render_failures)?;
    section(out, "not rendered on rendered first pages", &skipped)
}

/// Writes the counts of `title`, the most frequent first.
fn section(out: &mut dyn Write, title: &str, counts: &BTreeMap<String, usize>) -> io::Result<()> {
    if counts.is_empty() {
        return Ok(());
    }
    writeln!(out, "\n{title}:")?;
    let mut counts: Vec<(&String, &usize)> = counts.iter().collect();
    counts.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (name, count) in counts {
        writeln!(out, "{count:>8}  {name}")?;
    }
    Ok(())
}

/// Lists the files that were not rendered, with their outcome, relative to
/// `directory`; paths are escaped (`Debug`) since names may hold control
/// characters.
fn paths(
    directory: &Path,
    files: &[PathBuf],
    outcomes: &[Outcome],
    out: &mut dyn Write,
) -> io::Result<()> {
    writeln!(out, "\nfiles not rendered:")?;
    for (file, outcome) in files.iter().zip(outcomes) {
        let what = match &outcome.status {
            Status::Probed {
                render: Some(render),
                ..
            } if render == "ok" => continue,
            Status::Probed {
                render: Some(render),
                ..
            } => render.clone(),
            Status::Probed { open, .. } => open.clone(),
            Status::Timeout => "timeout".to_owned(),
            Status::Crash(how) => format!("crash ({how})"),
            Status::NotStarted(kind) => format!("not started ({kind:?})"),
        };
        let relative = file.strip_prefix(directory).unwrap_or(file);
        writeln!(out, "{what}\t{relative:?}")?;
    }
    Ok(())
}

/// `part / total` as a percentage with one decimal, rounded down.
fn percent(part: usize, total: usize) -> String {
    match (part * 1000).checked_div(total) {
        Some(per_mille) => format!("{}.{}%", per_mille / 10, per_mille % 10),
        None => "-".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probed(open: &str, render: Option<&str>, skipped: &[&str]) -> Status {
        Status::Probed {
            open: open.to_owned(),
            render: render.map(str::to_owned),
            skipped: skipped.iter().map(|s| (*s).to_owned()).collect(),
            repaired: None,
        }
    }

    #[test]
    fn probe_lines() {
        assert_eq!(
            parse_line("open=ok pages=3 render=ok skipped=Text,Image\n"),
            Some(probed("ok", Some("ok"), &["Text", "Image"]))
        );
        assert_eq!(
            parse_line("open=ok pages=1 render=ok skipped=none\n"),
            Some(probed("ok", Some("ok"), &[]))
        );
        assert_eq!(
            parse_line("open=ok pages=0 render=no-pages\n"),
            Some(probed("ok", Some("no-pages"), &[]))
        );
        assert_eq!(
            parse_line("open=malformed/InvalidHeader\n"),
            Some(probed("malformed/InvalidHeader", None, &[]))
        );
        let Some(Status::Probed { repaired, .. }) = parse_line(
            "open=ok pages=1 render=ok skipped=none repaired=malformed/MissingStartxref\n",
        ) else {
            panic!("a repaired file is a probe line");
        };
        assert_eq!(repaired.as_deref(), Some("malformed/MissingStartxref"));
        // Incomplete or foreign output is not a probe line.
        for line in [
            "open=truncated repaired=truncated\n",
            "",
            "open=ok pages=1 render=ok skipped=none",
            "open=ok pages=1\n",
            "open=truncated render=ok\n",
            "open=ok what=1 render=ok\n",
            "thread 'main' panicked\n",
        ] {
            assert_eq!(parse_line(line), None, "{line:?}");
        }
    }

    #[test]
    fn percentages() {
        assert_eq!(percent(0, 0), "-");
        assert_eq!(percent(1, 3), "33.3%");
        assert_eq!(percent(2, 3), "66.6%");
        assert_eq!(percent(7, 7), "100.0%");
    }

    #[test]
    fn report_counts_each_outcome() {
        let outcome = |status, ms| Outcome {
            status,
            time: Duration::from_millis(ms),
        };
        let outcomes = [
            outcome(probed("ok", Some("ok"), &["Text"]), 10),
            outcome(probed("ok", Some("ok"), &["Text", "Image"]), 20),
            outcome(probed("ok", Some("content/UnknownOperator"), &[]), 30),
            outcome(probed("malformed/InvalidHeader", None, &[]), 1),
            outcome(
                Status::Probed {
                    open: "ok".to_owned(),
                    render: Some("ok".to_owned()),
                    skipped: Vec::new(),
                    repaired: Some("truncated".to_owned()),
                },
                1,
            ),
            outcome(Status::Timeout, 500),
            outcome(Status::Crash("panic".to_owned()), 5),
        ];
        let mut out = Vec::new();
        assert!(report(&outcomes, &mut out).is_ok());
        let text = String::from_utf8_lossy(&out);
        for expected in [
            "files: 7\n",
            "opened: 4 (57.1%)\n",
            "opened by repair: 1 (14.2%)\n",
            "first page rendered: 3 (42.8%)\n",
            "timeouts: 1\n",
            "crashes: 1\n",
            "median 10 ms, p95 500 ms, max 500 ms\n",
            "repaired, by strict error:\n       1  truncated\n",
            "crashes:\n       1  panic\n",
            "open failures:\n       1  malformed/InvalidHeader\n",
            "first page failures:\n       1  content/UnknownOperator\n",
            "not rendered on rendered first pages:\n       2  Text\n       1  Image\n",
        ] {
            assert!(text.contains(expected), "{expected:?} in {text}");
        }
    }
}
