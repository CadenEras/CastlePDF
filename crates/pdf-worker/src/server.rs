//! Worker request loop. One job runs at a time on its own thread so that the
//! loop keeps reading and can deliver `Cancel` and `Shutdown` promptly; a job
//! received while another runs fails with `Busy` (the shell queues them).
//! Open documents live in a table shared with the job threads.
//! End of input means the parent is gone: the running job is cancelled and
//! the loop returns.

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use pdf_syntax::{Budget, CancellationToken, ResourceLimits};

use crate::PROTOCOL_VERSION;
use crate::documents::{self, Documents};
use crate::frame::{DEFAULT_MAX_FRAME_LEN, FrameError, read_frame, write_frame};
use crate::job::run_synthetic_job;
use crate::protocol::{ErrorCode, Request, Response};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeError {
    Frame(FrameError),
    /// The peer violated the protocol; `ProtocolError` was sent when possible.
    Protocol(ErrorCode),
    Spawn(io::ErrorKind),
}

impl fmt::Display for ServeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(e) => write!(f, "frame error: {e}"),
            Self::Protocol(code) => write!(f, "protocol error: {code:?}"),
            Self::Spawn(kind) => write!(f, "cannot start job thread: {kind}"),
        }
    }
}

impl std::error::Error for ServeError {}

impl From<FrameError> for ServeError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

struct Running {
    id: u64,
    token: CancellationToken,
}

type Shared<T> = Arc<Mutex<T>>;

/// What a job does once started, on its thread, with its own budget.
type Work = Box<dyn FnOnce(&mut Budget) -> Response + Send>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The output lock is held only while one frame is written.
fn send<W: Write>(output: &Mutex<W>, response: &Response) -> Result<(), FrameError> {
    let mut out = lock(output);
    write_frame(&mut *out, &response.encode(), DEFAULT_MAX_FRAME_LEN)?;
    out.flush().map_err(|e| FrameError::Io(e.kind()))
}

fn protocol_failure<W: Write>(output: &Mutex<W>, code: ErrorCode) -> ServeError {
    // Best effort: the peer may already be gone.
    let _ = send(output, &Response::ProtocolError { code });
    ServeError::Protocol(code)
}

fn failed(id: u64, code: ErrorCode) -> Response {
    Response::JobFailed { id, code }
}

pub fn serve<R, W>(mut input: R, output: W) -> Result<(), ServeError>
where
    R: Read,
    W: Write + Send + 'static,
{
    let output: Shared<W> = Arc::new(Mutex::new(output));

    let Some(first) = read_frame(&mut input, DEFAULT_MAX_FRAME_LEN)? else {
        return Ok(());
    };
    match Request::decode(&first) {
        Ok(Request::Hello { version }) if version == PROTOCOL_VERSION => {
            send(&output, &Response::HelloAck { version })?;
        }
        Ok(Request::Hello { .. }) => {
            return Err(protocol_failure(&output, ErrorCode::UnsupportedVersion));
        }
        _ => return Err(protocol_failure(&output, ErrorCode::MalformedMessage)),
    }

    let current: Shared<Option<Running>> = Arc::new(Mutex::new(None));
    let mut job: Option<JoinHandle<()>> = None;
    let result = request_loop(&mut input, &output, &current, &mut job);

    if let Some(running) = lock(&current).as_ref() {
        running.token.cancel();
    }
    // Rasterisation, after interpretation, is not cancellable (pdf-render);
    // its work is bounded by `RenderLimits::max_path_work`.
    if let Some(handle) = job.take() {
        let _ = handle.join();
    }
    // Sent only after the job thread is gone, so it is really the last frame.
    if let Err(ServeError::Protocol(code)) = &result {
        let _ = send(&output, &Response::ProtocolError { code: *code });
    }
    result
}

fn request_loop<R, W>(
    input: &mut R,
    output: &Shared<W>,
    current: &Shared<Option<Running>>,
    job: &mut Option<JoinHandle<()>>,
) -> Result<(), ServeError>
where
    R: Read,
    W: Write + Send + 'static,
{
    let documents: Shared<Documents> = Arc::default();
    let mut last_id: Option<u64> = None;
    loop {
        let frame = match read_frame(input, DEFAULT_MAX_FRAME_LEN) {
            Ok(Some(frame)) => frame,
            Ok(None) => return Ok(()),
            Err(FrameError::TooLarge { .. }) => {
                return Err(ServeError::Protocol(ErrorCode::FrameTooLarge));
            }
            Err(e) => return Err(e.into()),
        };
        let request = match Request::decode(&frame) {
            Ok(Request::Shutdown) => return Ok(()),
            Ok(Request::Cancel { id }) => {
                if let Some(running) = lock(current).as_ref().filter(|r| r.id == id) {
                    running.token.cancel();
                }
                continue;
            }
            Ok(Request::CloseDocument { document }) => {
                lock(&documents).remove(document);
                continue;
            }
            Ok(request) => request,
            Err(_) => return Err(ServeError::Protocol(ErrorCode::MalformedMessage)),
        };
        let Some((id, work)) = job_of(request, &documents) else {
            return Err(ServeError::Protocol(ErrorCode::MalformedMessage));
        };
        // Ids are never reused, so a late `Cancel` cannot hit a newer job.
        let fresh = last_id.is_none_or(|last| id > last);
        last_id = Some(last_id.map_or(id, |last| last.max(id)));
        if !fresh {
            send(output, &failed(id, ErrorCode::InvalidJobId))?;
            continue;
        }
        start(id, work, output, current, job)?;
    }
}

/// The id and the work of a job request; `None` for other requests (a
/// second `Hello`, the others being handled by the request loop).
fn job_of(request: Request, documents: &Shared<Documents>) -> Option<(u64, Work)> {
    Some(match request {
        Request::RunSynthetic {
            id,
            units,
            unit_delay_ms,
        } => {
            let delay = Duration::from_millis(u64::from(unit_delay_ms));
            let work = move |budget: &mut Budget| {
                let result = run_synthetic_job(units, budget, |_| {
                    if !delay.is_zero() {
                        thread::sleep(delay);
                    }
                });
                match result {
                    Ok(done) => Response::JobDone { id, units: done },
                    Err(e) => failed(id, ErrorCode::from(&e)),
                }
            };
            (id, Box::new(work))
        }
        Request::OpenDocument { id, path } => {
            let documents = Arc::clone(documents);
            let work = move |budget: &mut Budget| {
                // Checked before reading the file, and again on insertion.
                if lock(&documents).is_full() {
                    return failed(id, ErrorCode::TooManyDocuments);
                }
                let opened = documents::open(&path, budget).and_then(|document| {
                    let pages = u32::try_from(document.page_count())
                        .map_err(|_| ErrorCode::LimitExceeded)?;
                    let stored = lock(&documents).insert(document);
                    stored
                        .map(|document| (document, pages))
                        .ok_or(ErrorCode::TooManyDocuments)
                });
                match opened {
                    Ok((document, pages)) => Response::DocumentOpened {
                        id,
                        document,
                        pages,
                    },
                    Err(code) => failed(id, code),
                }
            };
            (id, Box::new(work))
        }
        Request::PageSize {
            id,
            document,
            page,
            scale,
        } => {
            let document = lock(documents).get(document);
            let work = move |_: &mut Budget| {
                document
                    .ok_or(ErrorCode::UnknownDocument)
                    .and_then(|document| documents::size(&document, page, scale, id))
                    .unwrap_or_else(|code| failed(id, code))
            };
            (id, Box::new(work))
        }
        Request::RenderTile {
            id,
            document,
            page,
            scale,
            rect,
        } => {
            let document = lock(documents).get(document);
            let work = move |budget: &mut Budget| {
                document
                    .ok_or(ErrorCode::UnknownDocument)
                    .and_then(|document| documents::tile(&document, page, scale, rect, id, budget))
                    .unwrap_or_else(|code| failed(id, code))
            };
            (id, Box::new(work))
        }
        Request::Hello { .. }
        | Request::Cancel { .. }
        | Request::Shutdown
        | Request::CloseDocument { .. } => return None,
    })
}

/// Runs `work` on a new job thread, or answers `Busy` if a job is running.
fn start<W: Write + Send + 'static>(
    id: u64,
    work: Work,
    output: &Shared<W>,
    current: &Shared<Option<Running>>,
    job: &mut Option<JoinHandle<()>>,
) -> Result<(), ServeError> {
    let token = CancellationToken::new();
    {
        let mut slot = lock(current);
        if slot.is_some() {
            drop(slot);
            send(output, &failed(id, ErrorCode::Busy))?;
            return Ok(());
        }
        *slot = Some(Running {
            id,
            token: token.clone(),
        });
    }
    if let Some(previous) = job.take() {
        let _ = previous.join();
    }
    let output = Arc::clone(output);
    let current = Arc::clone(current);
    let handle = thread::Builder::new()
        .name("pdf-job".into())
        .spawn(move || {
            let mut budget = Budget::new(ResourceLimits::default(), token);
            let response = work(&mut budget);
            // Free the slot first so the peer can start a new job as soon as
            // it reads this response.
            *lock(&current) = None;
            let _ = send(&output, &response);
        })
        .map_err(|e| ServeError::Spawn(e.kind()))?;
    *job = Some(handle);
    Ok(())
}
