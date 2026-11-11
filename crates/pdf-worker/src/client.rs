//! Shell side of the worker boundary: spawns the worker process and talks to
//! it through two threads (a writer fed by a bounded queue and a reader), so
//! that no call blocks the caller beyond an explicit timeout, even if the
//! worker stops reading or writing. Dropping a `WorkerProcess` kills it.

use std::fmt;
use std::io::{self, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use crate::PROTOCOL_VERSION;
use crate::frame::{DEFAULT_MAX_FRAME_LEN, FrameError, read_frame, write_frame};
use crate::protocol::{Request, Response};

/// Requests queued before `send` reports `Backpressure`.
const REQUEST_QUEUE: usize = 64;
/// Responses buffered before the reader thread stops draining the worker.
const RESPONSE_QUEUE: usize = 64;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    Io(io::ErrorKind),
    Frame(FrameError),
    Malformed,
    Handshake(Option<Response>),
    Timeout,
    /// The request queue is full: the worker is not consuming requests.
    Backpressure,
    Closed,
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(f, "worker I/O error: {kind}"),
            Self::Frame(e) => write!(f, "worker frame error: {e}"),
            Self::Malformed => f.write_str("malformed worker response"),
            Self::Handshake(reply) => write!(f, "worker handshake failed: {reply:?}"),
            Self::Timeout => f.write_str("worker response timed out"),
            Self::Backpressure => f.write_str("worker request queue is full"),
            Self::Closed => f.write_str("worker connection closed"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<io::Error> for ClientError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

impl From<FrameError> for ClientError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// The worker exited by itself within the grace period.
    Exited(ExitStatus),
    /// The grace period elapsed and the worker was killed.
    Killed,
}

/// Non-blocking request sink: frames are written by a dedicated thread.
/// Dropping it lets the thread drain the queue and close the pipe.
struct RequestWriter {
    queue: SyncSender<Vec<u8>>,
}

impl RequestWriter {
    fn spawn<W: Write + Send + 'static>(mut pipe: W) -> io::Result<Self> {
        let (queue, frames) = sync_channel::<Vec<u8>>(REQUEST_QUEUE);
        thread::Builder::new()
            .name("pdf-worker-writer".into())
            .spawn(move || {
                for payload in frames {
                    let written = write_frame(&mut pipe, &payload, DEFAULT_MAX_FRAME_LEN)
                        .and_then(|()| pipe.flush().map_err(|e| FrameError::Io(e.kind())));
                    if written.is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self { queue })
    }

    fn send(&self, request: &Request) -> Result<(), ClientError> {
        self.queue
            .try_send(request.encode())
            .map_err(|error| match error {
                TrySendError::Full(_) => ClientError::Backpressure,
                TrySendError::Disconnected(_) => ClientError::Closed,
            })
    }
}

pub struct WorkerProcess {
    child: Child,
    requests: Option<RequestWriter>,
    responses: Receiver<Result<Response, ClientError>>,
}

impl WorkerProcess {
    /// Starts `program` and completes the version handshake within `timeout`.
    pub fn spawn(program: &Path, timeout: Duration) -> Result<Self, ClientError> {
        let mut command = Command::new(program);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // The worker is a console program. Launched from a program without a
        // console (the release shell, a GUI application), Windows would give
        // it a console of its own: about 0.4 s more at each start, and a
        // window that may flash. Its stderr then goes where the shell's does.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn()?;
        let pipes = (child.stdin.take(), child.stdout.take());
        let (tx, rx) = sync_channel(RESPONSE_QUEUE);
        let mut worker = Self {
            child,
            requests: None,
            responses: rx,
        };
        let (Some(stdin), Some(stdout)) = pipes else {
            return Err(ClientError::Closed);
        };

        thread::Builder::new()
            .name("pdf-worker-reader".into())
            .spawn(move || {
                let mut stdout = BufReader::new(stdout);
                loop {
                    let message = match read_frame(&mut stdout, DEFAULT_MAX_FRAME_LEN) {
                        Ok(Some(frame)) => {
                            Response::decode(&frame).map_err(|_| ClientError::Malformed)
                        }
                        Ok(None) => Err(ClientError::Closed),
                        Err(e) => Err(ClientError::Frame(e)),
                    };
                    let last = message.is_err();
                    if tx.send(message).is_err() || last {
                        break;
                    }
                }
            })?;
        worker.requests = Some(RequestWriter::spawn(stdin)?);

        worker.send(&Request::Hello {
            version: PROTOCOL_VERSION,
        })?;
        match worker.recv_timeout(timeout) {
            Ok(Response::HelloAck { version }) if version == PROTOCOL_VERSION => Ok(worker),
            Ok(other) => Err(ClientError::Handshake(Some(other))),
            Err(ClientError::Timeout) => Err(ClientError::Timeout),
            Err(_) => Err(ClientError::Handshake(None)),
        }
    }

    /// Queues a request without blocking.
    pub fn send(&self, request: &Request) -> Result<(), ClientError> {
        self.requests
            .as_ref()
            .ok_or(ClientError::Closed)?
            .send(request)
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<Response, ClientError> {
        match self.responses.recv_timeout(timeout) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => Err(ClientError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(ClientError::Closed),
        }
    }

    /// Whether the worker process is still running: a client checks it
    /// before sending a job, so that a worker that died while idle is
    /// noticed before a job is lost with it.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Kills the worker at once, as a crash would: its pipes close and the
    /// next `recv_timeout` reports it (`Closed`). For tests of what a
    /// client does when its worker dies.
    pub fn kill(&mut self) -> Result<(), ClientError> {
        self.child.kill()?;
        self.child.wait()?;
        Ok(())
    }

    /// Requests a clean stop and closes the request pipe, then kills the
    /// worker if it has not exited after `grace`. Never blocks on the pipe.
    pub fn shutdown(mut self, grace: Duration) -> Result<Stopped, ClientError> {
        let _ = self.send(&Request::Shutdown);
        self.requests = None;
        let deadline = Instant::now() + grace;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(Stopped::Exited(status));
            }
            if Instant::now() >= deadline {
                self.child.kill()?;
                self.child.wait()?;
                return Ok(Stopped::Killed);
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_pipe_reports_backpressure_without_blocking() {
        // Nobody reads `_unread`: the pipe fills up and the writer thread blocks.
        let (_unread, pipe) = io::pipe().unwrap();
        let writer = RequestWriter::spawn(pipe).unwrap();
        let request = Request::RunSynthetic {
            id: 1,
            units: 1,
            unit_delay_ms: 0,
        };
        let start = Instant::now();
        let outcome = (0..1_000_000).find_map(|_| writer.send(&request).err());
        assert_eq!(outcome, Some(ClientError::Backpressure));
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
