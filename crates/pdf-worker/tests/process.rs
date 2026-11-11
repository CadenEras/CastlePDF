//! Drives the real `pdf-worker` binary through its pipes.

// Test-only crate: helpers outside `#[test]` functions may fail loudly.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use pdf_worker::PROTOCOL_VERSION;
use pdf_worker::client::{ClientError, Stopped, WorkerProcess};
use pdf_worker::frame::{DEFAULT_MAX_FRAME_LEN, read_frame, write_frame};
use pdf_worker::protocol::{ErrorCode, Request, Response};

const TIMEOUT: Duration = Duration::from_secs(10);
/// Long enough to still be running when the test acts on it (100 s).
const LONG_JOB: Request = Request::RunSynthetic {
    id: 1,
    units: 10_000,
    unit_delay_ms: 10,
};

fn worker_path() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_pdf-worker"))
}

fn spawn() -> WorkerProcess {
    WorkerProcess::spawn(worker_path(), TIMEOUT).expect("worker starts and handshakes")
}

#[test]
fn synthetic_job_completes_and_worker_stops_cleanly() {
    let worker = spawn();
    worker
        .send(&Request::RunSynthetic {
            id: 5,
            units: 10,
            unit_delay_ms: 0,
        })
        .unwrap();
    assert_eq!(
        worker.recv_timeout(TIMEOUT),
        Ok(Response::JobDone { id: 5, units: 10 })
    );
    match worker.shutdown(TIMEOUT).unwrap() {
        Stopped::Exited(status) => assert!(status.success()),
        Stopped::Killed => panic!("worker ignored shutdown"),
    }
}

#[test]
fn running_job_is_cancelled_promptly() {
    let worker = spawn();
    worker.send(&LONG_JOB).unwrap();
    assert_eq!(
        worker.recv_timeout(Duration::from_millis(200)),
        Err(ClientError::Timeout)
    );
    let start = Instant::now();
    worker.send(&Request::Cancel { id: 1 }).unwrap();
    assert_eq!(
        worker.recv_timeout(TIMEOUT),
        Ok(Response::JobFailed {
            id: 1,
            code: ErrorCode::Cancelled
        })
    );
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn second_job_is_refused_while_busy_then_accepted() {
    let worker = spawn();
    worker.send(&LONG_JOB).unwrap();
    worker
        .send(&Request::RunSynthetic {
            id: 2,
            units: 1,
            unit_delay_ms: 0,
        })
        .unwrap();
    assert_eq!(
        worker.recv_timeout(TIMEOUT),
        Ok(Response::JobFailed {
            id: 2,
            code: ErrorCode::Busy
        })
    );
    // Cancelling an unknown id is ignored; the right id stops the job.
    worker.send(&Request::Cancel { id: 99 }).unwrap();
    worker.send(&Request::Cancel { id: 1 }).unwrap();
    assert_eq!(
        worker.recv_timeout(TIMEOUT),
        Ok(Response::JobFailed {
            id: 1,
            code: ErrorCode::Cancelled
        })
    );
    worker
        .send(&Request::RunSynthetic {
            id: 3,
            units: 1,
            unit_delay_ms: 0,
        })
        .unwrap();
    assert_eq!(
        worker.recv_timeout(TIMEOUT),
        Ok(Response::JobDone { id: 3, units: 1 })
    );
}

#[test]
fn job_over_budget_reports_limit() {
    let worker = spawn();
    // Default budget allows 1_000_000 objects.
    worker
        .send(&Request::RunSynthetic {
            id: 4,
            units: 1_000_001,
            unit_delay_ms: 0,
        })
        .unwrap();
    assert_eq!(
        worker.recv_timeout(TIMEOUT),
        Ok(Response::JobFailed {
            id: 4,
            code: ErrorCode::LimitExceeded
        })
    );
}

#[test]
fn shutdown_during_job_exits_without_kill() {
    let worker = spawn();
    worker.send(&LONG_JOB).unwrap();
    let start = Instant::now();
    assert!(matches!(
        worker.shutdown(TIMEOUT),
        Ok(Stopped::Exited(status)) if status.success()
    ));
    assert!(start.elapsed() < Duration::from_secs(2));
}

// Raw pipe helpers for protocol violations the client never produces.

struct Raw {
    child: Child,
    stdin: ChildStdin,
    frames: Receiver<Option<Vec<u8>>>,
}

fn spawn_raw() -> Raw {
    let mut child = Command::new(worker_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let (tx, frames) = channel();
    thread::spawn(move || {
        while let Ok(frame) = read_frame(&mut stdout, DEFAULT_MAX_FRAME_LEN) {
            let end = frame.is_none();
            if tx.send(frame).is_err() || end {
                break;
            }
        }
    });
    Raw {
        child,
        stdin,
        frames,
    }
}

impl Raw {
    fn send(&mut self, payload: &[u8]) {
        write_frame(&mut self.stdin, payload, DEFAULT_MAX_FRAME_LEN).unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&self) -> Response {
        let frame = self.frames.recv_timeout(TIMEOUT).unwrap().unwrap();
        Response::decode(&frame).unwrap()
    }

    fn exit_code(mut self) -> Option<i32> {
        drop(self.stdin);
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code();
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!("worker did not exit");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn unsupported_version_is_refused() {
    let mut raw = spawn_raw();
    raw.send(
        &Request::Hello {
            version: PROTOCOL_VERSION + 1,
        }
        .encode(),
    );
    assert_eq!(
        raw.recv(),
        Response::ProtocolError {
            code: ErrorCode::UnsupportedVersion
        }
    );
    assert_eq!(raw.exit_code(), Some(2));
}

#[test]
fn request_before_handshake_is_refused() {
    let mut raw = spawn_raw();
    raw.send(&Request::Cancel { id: 1 }.encode());
    assert_eq!(
        raw.recv(),
        Response::ProtocolError {
            code: ErrorCode::MalformedMessage
        }
    );
    assert_eq!(raw.exit_code(), Some(2));
}

#[test]
fn oversized_frame_is_refused_before_reading_payload() {
    let mut raw = spawn_raw();
    raw.send(
        &Request::Hello {
            version: PROTOCOL_VERSION,
        }
        .encode(),
    );
    assert_eq!(
        raw.recv(),
        Response::HelloAck {
            version: PROTOCOL_VERSION
        }
    );
    raw.stdin.write_all(&u32::MAX.to_le_bytes()).unwrap();
    raw.stdin.flush().unwrap();
    assert_eq!(
        raw.recv(),
        Response::ProtocolError {
            code: ErrorCode::FrameTooLarge
        }
    );
    assert_eq!(raw.exit_code(), Some(2));
}

#[test]
fn malformed_message_after_handshake_is_refused() {
    let mut raw = spawn_raw();
    raw.send(
        &Request::Hello {
            version: PROTOCOL_VERSION,
        }
        .encode(),
    );
    raw.recv();
    raw.send(&[0x02, 1, 2, 3]);
    assert_eq!(
        raw.recv(),
        Response::ProtocolError {
            code: ErrorCode::MalformedMessage
        }
    );
    assert_eq!(raw.exit_code(), Some(2));
}

#[test]
fn closed_input_before_handshake_exits_cleanly() {
    let raw = spawn_raw();
    assert_eq!(raw.exit_code(), Some(0));
}

#[test]
fn reused_job_id_is_refused() {
    let worker = spawn();
    let job = Request::RunSynthetic {
        id: 7,
        units: 1,
        unit_delay_ms: 0,
    };
    worker.send(&job).unwrap();
    assert_eq!(
        worker.recv_timeout(TIMEOUT),
        Ok(Response::JobDone { id: 7, units: 1 })
    );
    worker.send(&job).unwrap();
    assert_eq!(
        worker.recv_timeout(TIMEOUT),
        Ok(Response::JobFailed {
            id: 7,
            code: ErrorCode::InvalidJobId
        })
    );
}

fn handshake(raw: &mut Raw) {
    raw.send(
        &Request::Hello {
            version: PROTOCOL_VERSION,
        }
        .encode(),
    );
    assert_eq!(
        raw.recv(),
        Response::HelloAck {
            version: PROTOCOL_VERSION
        }
    );
}

#[test]
fn closed_input_during_job_cancels_and_exits_cleanly() {
    let mut raw = spawn_raw();
    handshake(&mut raw);
    raw.send(&LONG_JOB.encode());
    let start = Instant::now();
    assert_eq!(raw.exit_code(), Some(0));
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn protocol_error_is_the_last_frame_even_with_a_running_job() {
    let mut raw = spawn_raw();
    handshake(&mut raw);
    raw.send(&LONG_JOB.encode());
    raw.send(&[0xff]);
    // The cancelled job reports first; the protocol error closes the stream.
    assert_eq!(
        raw.recv(),
        Response::JobFailed {
            id: 1,
            code: ErrorCode::Cancelled
        }
    );
    assert_eq!(
        raw.recv(),
        Response::ProtocolError {
            code: ErrorCode::MalformedMessage
        }
    );
    assert_eq!(raw.frames.recv_timeout(TIMEOUT), Ok(None));
    assert_eq!(raw.exit_code(), Some(2));
}

fn percentiles(mut times: Vec<Duration>) -> String {
    times.sort();
    let at = |p: f64| {
        let index = ((times.len() - 1) as f64 * p).round() as usize;
        times.get(index).copied().unwrap_or_default()
    };
    format!(
        "median {:?}, p95 {:?}, min {:?}, max {:?}",
        at(0.5),
        at(0.95),
        at(0.0),
        at(1.0)
    )
}

/// Worker start-up, 20 times in a row ():
/// through the client (launch and version handshake, as the shell does),
/// then step by step with raw pipes: launch, Hello written, reply read. The
/// first start (executable not yet in the file cache) is shown apart.
#[test]
#[ignore = "measurement"]
fn measure_worker_start() {
    const RUNS: usize = 20;
    let mut client: Vec<Duration> = (0..RUNS)
        .map(|_| {
            let start = Instant::now();
            let worker = spawn();
            let elapsed = start.elapsed();
            let _ = worker.shutdown(Duration::from_secs(2));
            elapsed
        })
        .collect();
    let first = client.remove(0);
    println!(
        "client start + handshake: first {first:?}; then {}",
        percentiles(client)
    );
    let (mut launch, mut written, mut answered) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..RUNS {
        let start = Instant::now();
        let mut child = Command::new(worker_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        launch.push(start.elapsed());
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let hello = Request::Hello {
            version: PROTOCOL_VERSION,
        }
        .encode();
        write_frame(&mut stdin, &hello, DEFAULT_MAX_FRAME_LEN).unwrap();
        stdin.flush().unwrap();
        written.push(start.elapsed());
        let reply = read_frame(&mut stdout, DEFAULT_MAX_FRAME_LEN)
            .unwrap()
            .unwrap();
        answered.push(start.elapsed());
        assert!(matches!(
            Response::decode(&reply),
            Ok(Response::HelloAck { .. })
        ));
        drop(stdin);
        let _ = child.wait();
    }
    println!("raw launch: {}", percentiles(launch));
    println!("raw Hello written: {}", percentiles(written));
    println!("raw reply read: {}", percentiles(answered));
}
