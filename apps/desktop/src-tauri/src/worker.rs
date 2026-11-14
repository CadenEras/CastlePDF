//! Owns the `pdf-worker` process for the shell. Tauri commands validate their
//! arguments and queue jobs; a manager thread sends them to the worker one at
//! a time (the worker runs one job at a time), matches the responses, fills
//! the tile cache and publishes job events. A command waits for its result
//! only through a `Pending` with an explicit timeout, off the UI thread.
//!
//! A worker that dies (or breaks the protocol) is replaced, at most
//! `MAX_RESTARTS` times per `RESTART_WINDOW`: the job it was running fails
//! (it may be what killed it), the new worker reopens the current document
//! if its file has not changed, and the queued jobs go on. Document ids
//! given to the UI are the shell's, so they survive a restart.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use pdf_worker::client::{ClientError, WorkerProcess};
use pdf_worker::protocol::{
    ErrorCode, LAYER_UNITS_PER_PIXEL, MAX_PATH_BYTES, MAX_QUERY_BYTES, MAX_TILE_SIDE, Repair,
    Request, Response, TextLayer, TileRect,
};
use serde::{Deserialize, Serialize};

use crate::cache::{TileCache, TileKey};

pub const MAX_UNITS: u64 = 10_000_000;
pub const MAX_UNIT_DELAY_MS: u8 = 100;
/// Longest diagnostic job, units times delay: it must not hold the worker,
/// which runs one job at a time, away from rendering for long.
pub const MAX_JOB_MS: u64 = 60_000;
/// Largest scale accepted from the UI: 64 device pixels per point.
pub const MAX_SCALE: f64 = 64.0;
/// Bytes of the header that precedes the pixels of a tile sent to the UI:
/// width, height, skipped capabilities (`u16` little-endian each), the
/// repair state (`u8`: 0 none, 1 repaired, 2 incomplete, as `Repair`), then
/// one zero byte, so that the RGBA pixels start at an aligned offset.
pub const TILE_HEADER: usize = 8;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
/// Poll of the command queue while the worker is idle: upper bound on the
/// delay before a lost worker is noticed.
const IDLE_POLL: Duration = Duration::from_millis(20);
/// Wait for the running job's response before checking new commands.
const BUSY_POLL: Duration = Duration::from_millis(2);
/// Jobs queued before new ones are refused as `Busy`.
const MAX_QUEUED: usize = 256;
/// Workers started again after a loss within `RESTART_WINDOW`; beyond, the
/// worker stays unavailable and every request fails explicitly.
const MAX_RESTARTS: usize = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum JobOutcome {
    Done { units: u64 },
    Cancelled,
    LimitExceeded,
    Busy,
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobEvent {
    pub job_id: u64,
    pub outcome: JobOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "camelCase")]
pub enum CommandError {
    InvalidArgument {
        field: &'static str,
    },
    WorkerUnavailable,
    /// Too many jobs queued.
    Busy,
    /// The tile belongs to a view the UI has left (older generation).
    Stale,
    Timeout,
    /// The worker refused the job; `reason` is its error code in camelCase.
    Failed {
        reason: &'static str,
    },
}

/// A document opened by the shell, as announced to the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentInfo {
    pub document: u32,
    pub pages: u32,
    /// File name only, never the full path.
    pub name: String,
    /// Whether its structure was reconstructed when opening (ADR-0004).
    pub repair: RepairState,
}

/// `pdf_worker::protocol::Repair`, as named to the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RepairState {
    None,
    Repaired,
    Incomplete,
}

impl From<Repair> for RepairState {
    fn from(repair: Repair) -> Self {
        match repair {
            Repair::None => Self::None,
            Repair::Repaired => Self::Repaired,
            Repair::Incomplete => Self::Incomplete,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PageSize {
    pub width: u32,
    pub height: u32,
}

/// The text layer of a page, for the UI (ADR-0006, tranche 3.6a): lines in
/// reading order, each word placed on the page rendered at scale 1, in
/// pixels (y down, rotation applied). The UI scales it with its rendering.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextLayerDto {
    pub lines: Vec<Vec<WordDto>>,
    /// Words were left out to keep the layer bounded.
    pub truncated: bool,
    /// Text the page shows is missing (a font or glyph not read).
    pub incomplete: bool,
}

/// A word: its text and baseline (start and end) and font size, in pixels
/// of the page at scale 1.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WordDto {
    pub text: String,
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub size: f64,
}

impl From<TextLayer> for TextLayerDto {
    fn from(layer: TextLayer) -> Self {
        let pixels = |units: i32| f64::from(units) / LAYER_UNITS_PER_PIXEL;
        Self {
            lines: layer
                .lines
                .into_iter()
                .map(|line| {
                    line.words
                        .into_iter()
                        .map(|word| {
                            let [x0, y0, x1, y1] = word.baseline;
                            WordDto {
                                text: word.text,
                                x0: pixels(x0),
                                y0: pixels(y0),
                                x1: pixels(x1),
                                y1: pixels(y1),
                                size: f64::from(word.size) / LAYER_UNITS_PER_PIXEL,
                            }
                        })
                        .collect()
                })
                .collect(),
            truncated: layer.truncated,
            incomplete: layer.incomplete,
        }
    }
}

/// The occurrences of a query in a page (ADR-0006, tranche 3.7): each from
/// its first character to the one after its last, as `[line, word, char]`
/// indices into the page's text layer (`TextLayerDto`), characters counted
/// in Unicode scalar values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchHitsDto {
    pub hits: Vec<HitDto>,
    /// More occurrences than the worker carries for a page.
    pub truncated: bool,
    /// Text the page shows is missing: an occurrence there cannot be found.
    pub incomplete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HitDto {
    pub start: [u32; 3],
    pub end: [u32; 3],
}

/// What `search_page` looks for, as sent by the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    pub query: String,
    pub match_case: bool,
    pub match_diacritics: bool,
}

/// Arguments of `render_tile`, as sent by the UI.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TileRequest {
    pub document: u32,
    pub page: u32,
    pub scale: f64,
    pub x: u32,
    pub y: u32,
    pub width: u16,
    pub height: u16,
    /// The view the tile belongs to, as given by `begin_view`: tiles of
    /// older views are dropped or cancelled.
    pub generation: u64,
}

/// A result to come from the manager thread.
pub enum Pending<T> {
    Ready(Result<T, CommandError>),
    Waiting(Receiver<Result<T, CommandError>>),
}

impl<T> Pending<T> {
    /// Blocks the calling thread for at most `timeout`.
    pub fn wait(self, timeout: Duration) -> Result<T, CommandError> {
        match self {
            Self::Ready(result) => result,
            Self::Waiting(receiver) => match receiver.recv_timeout(timeout) {
                Ok(result) => result,
                Err(RecvTimeoutError::Timeout) => Err(CommandError::Timeout),
                Err(RecvTimeoutError::Disconnected) => Err(CommandError::WorkerUnavailable),
            },
        }
    }
}

type Reply<T> = Sender<Result<T, CommandError>>;

/// The document shown by the UI; requests for another one are refused.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Current {
    /// The id the UI knows: the shell's, kept across worker restarts.
    document: u32,
    pages: u32,
    /// The id the running worker gave it.
    worker_document: u32,
    /// Where to reopen it from after a restart, and its size and
    /// modification time when it was opened: a file changed since is not
    /// reopened under the same id.
    path: String,
    stamp: Option<(u64, SystemTime)>,
}

/// Size and modification time of the file at `path`, if readable.
fn stamp_of(path: &str) -> Option<(u64, SystemTime)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.len(), metadata.modified().ok()?))
}

enum Job {
    Synthetic {
        ui_id: u64,
        units: u64,
        unit_delay_ms: u8,
    },
    Open {
        path: String,
        name: String,
        reply: Reply<DocumentInfo>,
    },
    Size {
        document: u32,
        page: u32,
        scale: f64,
        reply: Reply<PageSize>,
    },
    Tile {
        key: TileKey,
        generation: u64,
        reply: Reply<Arc<Vec<u8>>>,
    },
    /// A text layer belongs to a view, as a tile does: dropped or cancelled
    /// when a newer view starts, so that it never delays the current tiles.
    Text {
        document: u32,
        page: u32,
        generation: u64,
        reply: Reply<TextLayerDto>,
    },
    /// Searches one page; a search of the document is a page at a time, so
    /// that tiles go in between. It belongs to no view: a search goes on
    /// while the user scrolls. At most one waits in the queue, and every
    /// other job goes before it.
    Search {
        document: u32,
        page: u32,
        search: SearchRequest,
        reply: Reply<SearchHitsDto>,
    },
    /// Opens the current document again in a restarted worker.
    Reopen { path: String },
}

impl Job {
    /// The view a tile or a text layer belongs to.
    fn generation(&self) -> Option<u64> {
        match self {
            Self::Tile { generation, .. } | Self::Text { generation, .. } => Some(*generation),
            _ => None,
        }
    }
}

enum Command {
    Queue(Job),
    Cancel {
        ui_id: u64,
    },
    /// The UI starts view `generation`: older tiles are now stale.
    View {
        generation: u64,
    },
    Stop,
    /// Kills the worker as a crash would (tests of the restart).
    #[cfg(test)]
    KillWorker,
}

struct Shared {
    current: Mutex<Option<Current>>,
    cache: Mutex<TileCache>,
    /// Last generation given by `begin_view`. The shell numbers views, not
    /// the UI, so a reloaded page cannot restart below the manager's.
    issued: AtomicU64,
}

pub struct WorkerHandle {
    /// Last synthetic job id given to the UI; the lock also keeps ids and
    /// sends in the same order.
    commands: Mutex<(u64, Sender<Command>)>,
    manager: Mutex<Option<JoinHandle<()>>>,
    shared: Arc<Shared>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The worker is expected next to the shell executable.
pub fn bundled_worker_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.with_file_name(format!("pdf-worker{}", std::env::consts::EXE_SUFFIX)))
}

pub fn validate_run(units: u64, unit_delay_ms: u8) -> Result<(), CommandError> {
    if units == 0 || units > MAX_UNITS {
        return Err(CommandError::InvalidArgument { field: "units" });
    }
    if unit_delay_ms > MAX_UNIT_DELAY_MS {
        return Err(CommandError::InvalidArgument {
            field: "unitDelayMs",
        });
    }
    if units.saturating_mul(u64::from(unit_delay_ms)) > MAX_JOB_MS {
        return Err(CommandError::InvalidArgument { field: "units" });
    }
    Ok(())
}

fn validate_scale(scale: f64) -> Result<(), CommandError> {
    if scale.is_finite() && scale > 0.0 && scale <= MAX_SCALE {
        Ok(())
    } else {
        Err(CommandError::InvalidArgument { field: "scale" })
    }
}

impl WorkerHandle {
    pub fn start<F>(worker: Option<PathBuf>, publish: F) -> Self
    where
        F: Fn(JobEvent) + Send + 'static,
    {
        let shared = Arc::new(Shared {
            current: Mutex::new(None),
            cache: Mutex::new(TileCache::default()),
            issued: AtomicU64::new(0),
        });
        let (tx, rx) = channel();
        let manager_shared = Arc::clone(&shared);
        let manager = thread::Builder::new()
            .name("pdf-worker-manager".into())
            .spawn(move || manage(worker.as_deref(), &rx, &manager_shared, &publish));
        if let Err(error) = &manager {
            eprintln!("castle-pdf: cannot start worker manager: {error}");
        }
        Self {
            commands: Mutex::new((0, tx)),
            manager: Mutex::new(manager.ok()),
            shared,
        }
    }

    fn queue(&self, job: Job) -> Result<(), CommandError> {
        lock(&self.commands)
            .1
            .send(Command::Queue(job))
            .map_err(|_| CommandError::WorkerUnavailable)
    }

    pub fn run(&self, units: u64, unit_delay_ms: u8) -> Result<u64, CommandError> {
        validate_run(units, unit_delay_ms)?;
        let mut commands = lock(&self.commands);
        let ui_id = commands
            .0
            .checked_add(1)
            .ok_or(CommandError::WorkerUnavailable)?;
        commands
            .1
            .send(Command::Queue(Job::Synthetic {
                ui_id,
                units,
                unit_delay_ms,
            }))
            .map_err(|_| CommandError::WorkerUnavailable)?;
        commands.0 = ui_id;
        Ok(ui_id)
    }

    pub fn cancel(&self, ui_id: u64) -> Result<(), CommandError> {
        lock(&self.commands)
            .1
            .send(Command::Cancel { ui_id })
            .map_err(|_| CommandError::WorkerUnavailable)
    }

    /// Opens the file at `path`, chosen by the shell (dropped on the window
    /// or given on the command line), never by the UI. Once open, it
    /// replaces the current document.
    pub fn open(&self, path: &Path) -> Pending<DocumentInfo> {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        // The protocol carries UTF-8 paths of bounded length.
        let Some(path) = path
            .to_str()
            .filter(|p| !p.is_empty() && p.len() <= usize::from(MAX_PATH_BYTES))
        else {
            return Pending::Ready(Err(CommandError::Failed {
                reason: "unsupportedPath",
            }));
        };
        let (reply, receiver) = channel();
        match self.queue(Job::Open {
            path: path.to_owned(),
            name,
            reply,
        }) {
            Ok(()) => Pending::Waiting(receiver),
            Err(error) => Pending::Ready(Err(error)),
        }
    }

    fn check_page(&self, document: u32, page: u32) -> Result<(), CommandError> {
        let current = lock(&self.shared.current);
        match current.as_ref() {
            Some(current) if current.document == document => {
                if page < current.pages {
                    Ok(())
                } else {
                    Err(CommandError::InvalidArgument { field: "page" })
                }
            }
            _ => Err(CommandError::InvalidArgument { field: "document" }),
        }
    }

    pub fn page_size(&self, document: u32, page: u32, scale: f64) -> Pending<PageSize> {
        if let Err(error) = validate_scale(scale).and_then(|()| self.check_page(document, page)) {
            return Pending::Ready(Err(error));
        }
        let (reply, receiver) = channel();
        match self.queue(Job::Size {
            document,
            page,
            scale,
            reply,
        }) {
            Ok(()) => Pending::Waiting(receiver),
            Err(error) => Pending::Ready(Err(error)),
        }
    }

    /// The text layer of `page` of the current document, for view
    /// `generation` (as given by `begin_view`).
    pub fn page_text(&self, document: u32, page: u32, generation: u64) -> Pending<TextLayerDto> {
        if let Err(error) = self
            .check_page(document, page)
            .and_then(|()| self.check_generation(generation))
        {
            return Pending::Ready(Err(error));
        }
        let (reply, receiver) = channel();
        match self.queue(Job::Text {
            document,
            page,
            generation,
            reply,
        }) {
            Ok(()) => Pending::Waiting(receiver),
            Err(error) => Pending::Ready(Err(error)),
        }
    }

    /// The occurrences of `search.query` in `page` of the current document.
    /// The query is refused when it is blank or longer than
    /// `MAX_QUERY_BYTES`; the worker also refuses one too long once folded.
    pub fn search_page(
        &self,
        document: u32,
        page: u32,
        search: SearchRequest,
    ) -> Pending<SearchHitsDto> {
        let query = &search.query;
        if query.trim().is_empty() || query.len() > MAX_QUERY_BYTES {
            return Pending::Ready(Err(CommandError::InvalidArgument { field: "query" }));
        }
        if let Err(error) = self.check_page(document, page) {
            return Pending::Ready(Err(error));
        }
        let (reply, receiver) = channel();
        match self.queue(Job::Search {
            document,
            page,
            search,
            reply,
        }) {
            Ok(()) => Pending::Waiting(receiver),
            Err(error) => Pending::Ready(Err(error)),
        }
    }

    /// Starts a new view (page, scale, document) and returns its generation:
    /// the queued tiles of older views are dropped at once, before the new
    /// view's requests.
    pub fn begin_view(&self) -> Result<u64, CommandError> {
        // The commands lock orders the generations as they are sent.
        let commands = lock(&self.commands);
        let generation = self.shared.issued.load(Ordering::Relaxed).saturating_add(1);
        commands
            .1
            .send(Command::View { generation })
            .map_err(|_| CommandError::WorkerUnavailable)?;
        self.shared.issued.store(generation, Ordering::Relaxed);
        Ok(generation)
    }

    /// The tile as sent to the UI: `TILE_HEADER` bytes, then RGBA pixels.
    /// Served from the cache when present.
    /// A generation already given by `begin_view`.
    fn check_generation(&self, generation: u64) -> Result<(), CommandError> {
        let issued = self.shared.issued.load(Ordering::Relaxed);
        if (1..=issued).contains(&generation) {
            Ok(())
        } else {
            Err(CommandError::InvalidArgument {
                field: "generation",
            })
        }
    }

    pub fn tile(&self, request: TileRequest) -> Pending<Arc<Vec<u8>>> {
        let side = 1..=MAX_TILE_SIDE;
        let checked = validate_scale(request.scale)
            .and_then(|()| {
                if side.contains(&request.width) && side.contains(&request.height) {
                    Ok(())
                } else {
                    Err(CommandError::InvalidArgument { field: "size" })
                }
            })
            .and_then(|()| self.check_page(request.document, request.page))
            .and_then(|()| self.check_generation(request.generation));
        if let Err(error) = checked {
            return Pending::Ready(Err(error));
        }
        let key = TileKey::new(
            request.document,
            request.page,
            request.scale,
            TileRect {
                x: request.x,
                y: request.y,
                width: request.width,
                height: request.height,
            },
        );
        if let Some(tile) = lock(&self.shared.cache).get(&key) {
            return Pending::Ready(Ok(tile));
        }
        let (reply, receiver) = channel();
        match self.queue(Job::Tile {
            key,
            generation: request.generation,
            reply,
        }) {
            Ok(()) => Pending::Waiting(receiver),
            Err(error) => Pending::Ready(Err(error)),
        }
    }

    #[cfg(test)]
    fn kill_worker(&self) {
        let _ = lock(&self.commands).1.send(Command::KillWorker);
    }

    /// Stops the worker; returns after at most the shutdown grace period
    /// plus one poll interval.
    pub fn stop(&self) {
        let _ = lock(&self.commands).1.send(Command::Stop);
        if let Some(manager) = lock(&self.manager).take() {
            let _ = manager.join();
        }
    }
}

fn outcome(code: ErrorCode) -> JobOutcome {
    match code {
        ErrorCode::Cancelled => JobOutcome::Cancelled,
        ErrorCode::LimitExceeded => JobOutcome::LimitExceeded,
        ErrorCode::Busy => JobOutcome::Busy,
        other => JobOutcome::Failed {
            reason: reason(other).to_owned(),
        },
    }
}

/// The worker error code as named to the UI.
fn reason(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Cancelled => "cancelled",
        ErrorCode::LimitExceeded => "limitExceeded",
        ErrorCode::Busy => "busy",
        ErrorCode::InvalidRequest => "invalidRequest",
        ErrorCode::UnknownDocument => "unknownDocument",
        ErrorCode::Unreadable => "unreadable",
        ErrorCode::Malformed => "malformed",
        ErrorCode::Unsupported => "unsupported",
        ErrorCode::Encrypted => "encrypted",
        ErrorCode::TooManyDocuments => "tooManyDocuments",
        ErrorCode::MalformedMessage
        | ErrorCode::UnsupportedVersion
        | ErrorCode::FrameTooLarge
        | ErrorCode::Internal
        | ErrorCode::InvalidJobId => "internal",
    }
}

fn failed(job_id: u64, reason: &str) -> JobEvent {
    JobEvent {
        job_id,
        outcome: JobOutcome::Failed {
            reason: reason.to_owned(),
        },
    }
}

/// Header and pixels of a tile, as sent to the UI.
fn tile_bytes(width: u16, height: u16, skipped: u16, repair: Repair, pixels: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(TILE_HEADER + pixels.len());
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(&skipped.to_le_bytes());
    bytes.extend_from_slice(&[repair as u8, 0]);
    bytes.extend_from_slice(pixels);
    bytes
}

struct Manager<'a> {
    worker: Option<WorkerProcess>,
    /// The worker executable, to start it again after a loss.
    program: Option<PathBuf>,
    /// When the worker was started again, within `RESTART_WINDOW`.
    restarts: VecDeque<Instant>,
    /// Next document id given to the UI.
    next_document: u32,
    shared: &'a Shared,
    publish: &'a dyn Fn(JobEvent),
    /// Last job id sent to the worker; strictly increasing.
    last_id: u64,
    queue: VecDeque<Job>,
    in_flight: Option<(u64, Job)>,
    /// Latest view generation seen in a tile request.
    generation: u64,
}

impl Manager<'_> {
    /// Handles a command; `false` on `Stop`.
    fn command(&mut self, command: Command) -> bool {
        match command {
            Command::Stop => return false,
            #[cfg(test)]
            Command::KillWorker => {
                if let Some(worker) = &mut self.worker {
                    let _ = worker.kill();
                }
            }
            Command::Cancel { ui_id } => self.cancel(ui_id),
            Command::View { generation } => {
                if generation > self.generation {
                    self.new_generation(generation);
                }
            }
            Command::Queue(job) => {
                // A newer search page replaces the one still waiting: the
                // UI moved on to another page or another query.
                if matches!(job, Job::Search { .. })
                    && let Some(index) = self
                        .queue
                        .iter()
                        .position(|queued| matches!(queued, Job::Search { .. }))
                    && let Some(older) = self.queue.remove(index)
                {
                    self.refuse(older, CommandError::Stale);
                }
                if self.queue.len() >= MAX_QUEUED {
                    self.refuse(job, CommandError::Busy);
                } else {
                    self.queue.push_back(job);
                }
            }
        }
        true
    }

    fn cancel(&mut self, ui_id: u64) {
        if let Some((id, Job::Synthetic { ui_id: running, .. })) = &self.in_flight
            && *running == ui_id
        {
            if let Some(worker) = &self.worker {
                let _ = worker.send(&Request::Cancel { id: *id });
            }
            return;
        }
        let queued = self
            .queue
            .iter()
            .position(|job| matches!(job, Job::Synthetic { ui_id: id, .. } if *id == ui_id));
        if let Some(index) = queued {
            self.queue.remove(index);
            (self.publish)(JobEvent {
                job_id: ui_id,
                outcome: JobOutcome::Cancelled,
            });
        }
    }

    /// Drops the queued tiles of older views and cancels the running one.
    fn new_generation(&mut self, generation: u64) {
        self.generation = generation;
        let (stale, kept): (Vec<Job>, Vec<Job>) = self
            .queue
            .drain(..)
            .partition(|job| job.generation().is_some_and(|g| g < generation));
        self.queue = kept.into();
        for job in stale {
            self.refuse(job, CommandError::Stale);
        }
        if let Some((id, job)) = &self.in_flight
            && job.generation().is_some_and(|g| g < generation)
            && let Some(worker) = &self.worker
        {
            let _ = worker.send(&Request::Cancel { id: *id });
        }
    }

    fn refuse(&self, job: Job, error: CommandError) {
        match job {
            Job::Synthetic { ui_id, .. } => (self.publish)(match error {
                CommandError::Busy => JobEvent {
                    job_id: ui_id,
                    outcome: JobOutcome::Busy,
                },
                _ => failed(ui_id, "worker unavailable"),
            }),
            Job::Open { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Job::Size { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Job::Tile { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Job::Text { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Job::Search { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Job::Reopen { .. } => {}
        }
    }

    /// The id the running worker gave to document `document` (the UI's).
    fn worker_document(&self, document: u32) -> Option<u32> {
        lock(&self.shared.current)
            .as_ref()
            .filter(|current| current.document == document)
            .map(|current| current.worker_document)
    }

    /// Sends the next queued job if none is running.
    fn pump(&mut self) {
        while self.in_flight.is_none() {
            // A worker that died while idle is replaced before a job is
            // sent to it (and lost with it).
            if self.queue.front().is_some()
                && self
                    .worker
                    .as_mut()
                    .is_some_and(|worker| !worker.is_running())
            {
                self.lose_worker();
            }
            // Tiles and the rest go before a search page: a search never
            // keeps the view waiting longer than the page it is on.
            let next = self
                .queue
                .iter()
                .position(|job| !matches!(job, Job::Search { .. }))
                .unwrap_or(0);
            let Some(job) = self.queue.remove(next) else {
                return;
            };
            let Some(worker) = &self.worker else {
                self.refuse(job, CommandError::WorkerUnavailable);
                continue;
            };
            if job.generation().is_some_and(|g| g < self.generation) {
                self.refuse(job, CommandError::Stale);
                continue;
            }
            if let Job::Tile { key, reply, .. } = &job {
                // Filled by an identical request meanwhile.
                if let Some(tile) = lock(&self.shared.cache).get(key) {
                    let _ = reply.send(Ok(tile));
                    continue;
                }
            }
            let target = match &job {
                Job::Size { document, .. }
                | Job::Text { document, .. }
                | Job::Search { document, .. } => Some(*document),
                Job::Tile { key, .. } => Some(key.document),
                _ => None,
            };
            let worker_document = match target.map(|document| self.worker_document(document)) {
                None => 0,
                Some(Some(worker_document)) => worker_document,
                Some(None) => {
                    self.refuse(job, CommandError::InvalidArgument { field: "document" });
                    continue;
                }
            };
            let Some(id) = self.last_id.checked_add(1) else {
                self.refuse(job, CommandError::WorkerUnavailable);
                continue;
            };
            let request = match &job {
                Job::Synthetic {
                    units,
                    unit_delay_ms,
                    ..
                } => Request::RunSynthetic {
                    id,
                    units: *units,
                    unit_delay_ms: *unit_delay_ms,
                },
                Job::Open { path, .. } => Request::OpenDocument {
                    id,
                    path: path.clone(),
                },
                Job::Size { page, scale, .. } => Request::PageSize {
                    id,
                    document: worker_document,
                    page: *page,
                    scale: *scale,
                },
                Job::Tile { key, .. } => Request::RenderTile {
                    id,
                    document: worker_document,
                    page: key.page,
                    scale: key.scale(),
                    rect: key.rect(),
                },
                Job::Text { page, .. } => Request::PageText {
                    id,
                    document: worker_document,
                    page: *page,
                },
                Job::Search { page, search, .. } => Request::SearchPage {
                    id,
                    document: worker_document,
                    page: *page,
                    query: search.query.clone(),
                    match_case: search.match_case,
                    match_diacritics: search.match_diacritics,
                },
                Job::Reopen { path } => Request::OpenDocument {
                    id,
                    path: path.clone(),
                },
            };
            match worker.send(&request) {
                Ok(()) => {
                    self.last_id = id;
                    self.in_flight = Some((id, job));
                }
                Err(_) => self.refuse(job, CommandError::WorkerUnavailable),
            }
        }
    }

    /// Matches a worker response with the running job.
    fn response(&mut self, response: Response) {
        let id = match &response {
            Response::JobDone { id, .. }
            | Response::JobFailed { id, .. }
            | Response::DocumentOpened { id, .. }
            | Response::PageSize { id, .. }
            | Response::Tile { id, .. }
            | Response::PageText { id, .. }
            | Response::SearchHits { id, .. } => *id,
            Response::HelloAck { .. } | Response::ProtocolError { .. } => {
                eprintln!("castle-pdf: unexpected worker response: {response:?}");
                self.lose_worker();
                return;
            }
        };
        let Some((running, job)) = self.in_flight.take() else {
            eprintln!("castle-pdf: worker response without a running job");
            self.lose_worker();
            return;
        };
        if running != id {
            eprintln!("castle-pdf: worker response for another job");
            self.in_flight = Some((running, job));
            self.lose_worker();
            return;
        }
        match (job, response) {
            (Job::Synthetic { ui_id, .. }, Response::JobDone { units, .. }) => {
                (self.publish)(JobEvent {
                    job_id: ui_id,
                    outcome: JobOutcome::Done { units },
                });
            }
            (Job::Synthetic { ui_id, .. }, Response::JobFailed { code, .. }) => {
                (self.publish)(JobEvent {
                    job_id: ui_id,
                    outcome: outcome(code),
                });
            }
            (
                Job::Open {
                    path, name, reply, ..
                },
                Response::DocumentOpened {
                    document: worker_document,
                    pages,
                    repair,
                    ..
                },
            ) => {
                let document = self.next_document;
                self.next_document = document.saturating_add(1);
                // The new document is current before anyone learns its id,
                // so that a request sent right after the answer finds it.
                // If nobody waits any more (the opening timed out and the
                // UI was told it failed), the previous document is put back
                // and the new one closed. The lock is held throughout: no
                // request sees the new document unless it was announced.
                let stamp = stamp_of(&path);
                let closed = {
                    let mut current = lock(&self.shared.current);
                    let previous = current.replace(Current {
                        document,
                        pages,
                        worker_document,
                        path,
                        stamp,
                    });
                    let answered = reply
                        .send(Ok(DocumentInfo {
                            document,
                            pages,
                            name,
                            repair: repair.into(),
                        }))
                        .is_ok();
                    if answered {
                        previous.map(|previous| (previous.document, previous.worker_document))
                    } else {
                        *current = previous;
                        Some((document, worker_document))
                    }
                };
                if let Some((closed, worker_closed)) = closed {
                    lock(&self.shared.cache).remove_document(closed);
                    if let Some(worker) = &self.worker {
                        let _ = worker.send(&Request::CloseDocument {
                            document: worker_closed,
                        });
                    }
                }
            }
            (
                Job::Reopen { path },
                Response::DocumentOpened {
                    document: worker_document,
                    pages,
                    ..
                },
            ) => {
                // The same file, the same pages: the UI's id now names the
                // new worker's document. Otherwise it is gone.
                let mut current = lock(&self.shared.current);
                let same = current
                    .as_ref()
                    .is_some_and(|c| c.path == path && c.pages == pages);
                if same && let Some(current) = current.as_mut() {
                    current.worker_document = worker_document;
                } else {
                    *current = None;
                    drop(current);
                    lock(&self.shared.cache).clear();
                    if let Some(worker) = &self.worker {
                        let _ = worker.send(&Request::CloseDocument {
                            document: worker_document,
                        });
                    }
                }
            }
            (Job::Reopen { .. }, Response::JobFailed { code, .. }) => {
                eprintln!("castle-pdf: document not reopened after a restart: {code:?}");
                *lock(&self.shared.current) = None;
                lock(&self.shared.cache).clear();
            }
            (Job::Size { reply, .. }, Response::PageSize { width, height, .. }) => {
                let _ = reply.send(Ok(PageSize { width, height }));
            }
            (Job::Text { reply, .. }, Response::PageText { layer, .. }) => {
                let _ = reply.send(Ok(TextLayerDto::from(layer)));
            }
            (
                Job::Search { reply, .. },
                Response::SearchHits {
                    hits,
                    truncated,
                    incomplete,
                    ..
                },
            ) => {
                let _ = reply.send(Ok(SearchHitsDto {
                    hits: hits
                        .into_iter()
                        .map(|hit| HitDto {
                            start: hit.start,
                            end: hit.end,
                        })
                        .collect(),
                    truncated,
                    incomplete,
                }));
            }
            (
                Job::Tile { key, reply, .. },
                Response::Tile {
                    width,
                    height,
                    skipped,
                    repair,
                    pixels,
                    ..
                },
            ) => {
                if key.rect().width != width || key.rect().height != height {
                    let _ = reply.send(Err(CommandError::Failed { reason: "internal" }));
                    return;
                }
                let tile = Arc::new(tile_bytes(width, height, skipped.0, repair, &pixels));
                lock(&self.shared.cache).insert(key, Arc::clone(&tile));
                let _ = reply.send(Ok(tile));
            }
            (job @ (Job::Tile { .. } | Job::Text { .. }), Response::JobFailed { code, .. }) => {
                let stale = job.generation().is_some_and(|g| g < self.generation);
                let error = if code == ErrorCode::Cancelled && stale {
                    CommandError::Stale
                } else {
                    CommandError::Failed {
                        reason: reason(code),
                    }
                };
                self.refuse(job, error);
            }
            (job, Response::JobFailed { code, .. }) => self.refuse(
                job,
                CommandError::Failed {
                    reason: reason(code),
                },
            ),
            (job, other) => {
                eprintln!("castle-pdf: worker response of another kind: {other:?}");
                self.in_flight = Some((running, job));
                self.lose_worker();
            }
        }
    }

    /// The worker is gone or broken. The job it was running fails: it may
    /// be what killed it, so it is not sent again. A new worker is started
    /// if `restart` allows it; otherwise every job still waiting fails.
    fn lose_worker(&mut self) {
        self.worker = None;
        if let Some((_, job)) = self.in_flight.take() {
            self.refuse_lost(job);
        }
        if !self.restart() {
            self.give_up();
        }
    }

    /// Starts the worker again, at most `MAX_RESTARTS` times per
    /// `RESTART_WINDOW`, and queues the reopening of the current document
    /// before anything else; a document whose file changed since it was
    /// opened is dropped instead. `false` if no worker could be started.
    fn restart(&mut self) -> bool {
        let now = Instant::now();
        while self
            .restarts
            .front()
            .is_some_and(|at| now.duration_since(*at) > RESTART_WINDOW)
        {
            self.restarts.pop_front();
        }
        if self.restarts.len() >= MAX_RESTARTS {
            eprintln!(
                "castle-pdf: worker lost {MAX_RESTARTS} times within a minute, not restarted"
            );
            return false;
        }
        let Some(program) = &self.program else {
            return false;
        };
        self.restarts.push_back(now);
        match WorkerProcess::spawn(program, HANDSHAKE_TIMEOUT) {
            Ok(worker) => {
                eprintln!("castle-pdf: worker restarted");
                self.worker = Some(worker);
            }
            Err(error) => {
                eprintln!("castle-pdf: worker not restarted: {error}");
                return false;
            }
        }
        let reopen = lock(&self.shared.current)
            .as_ref()
            .map(|current| (current.path.clone(), current.stamp));
        if let Some((path, stamp)) = reopen {
            if stamp.is_some() && stamp_of(&path) == stamp {
                self.queue.push_front(Job::Reopen { path });
            } else {
                *lock(&self.shared.current) = None;
                lock(&self.shared.cache).clear();
            }
        }
        true
    }

    /// No worker any more: no current document, every job fails.
    fn give_up(&mut self) {
        self.worker = None;
        *lock(&self.shared.current) = None;
        lock(&self.shared.cache).clear();
        if let Some((_, job)) = self.in_flight.take() {
            self.refuse_lost(job);
        }
        while let Some(job) = self.queue.pop_front() {
            self.refuse_lost(job);
        }
    }

    fn refuse_lost(&self, job: Job) {
        match job {
            Job::Synthetic { ui_id, .. } => (self.publish)(failed(ui_id, "worker exited")),
            other => self.refuse(other, CommandError::WorkerUnavailable),
        }
    }

    fn poll_worker(&mut self, timeout: Duration) {
        let Some(worker) = &self.worker else { return };
        match worker.recv_timeout(timeout) {
            Ok(response) => self.response(response),
            Err(ClientError::Timeout) => {}
            Err(error) => {
                eprintln!("castle-pdf: worker lost: {error}");
                self.lose_worker();
            }
        }
    }
}

fn manage(
    path: Option<&Path>,
    commands: &Receiver<Command>,
    shared: &Shared,
    publish: &dyn Fn(JobEvent),
) {
    let worker = match path.map(|p| WorkerProcess::spawn(p, HANDSHAKE_TIMEOUT)) {
        Some(Ok(worker)) => Some(worker),
        Some(Err(error)) => {
            eprintln!("castle-pdf: worker unavailable: {error}");
            None
        }
        None => {
            eprintln!("castle-pdf: worker path unknown");
            None
        }
    };
    let mut manager = Manager {
        worker,
        program: path.map(Path::to_path_buf),
        restarts: VecDeque::new(),
        next_document: 1,
        shared,
        publish,
        last_id: 0,
        queue: VecDeque::new(),
        in_flight: None,
        generation: 0,
    };

    'serve: loop {
        if manager.in_flight.is_some() {
            // Waiting on the worker first keeps a tile's latency low; new
            // commands wait at most `BUSY_POLL`.
            manager.poll_worker(BUSY_POLL);
            loop {
                match commands.try_recv() {
                    Ok(command) => {
                        if !manager.command(command) {
                            break 'serve;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break 'serve,
                }
            }
        } else {
            match commands.recv_timeout(IDLE_POLL) {
                Ok(command) => {
                    if !manager.command(command) {
                        break 'serve;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break 'serve,
            }
            // Notices a worker that exited while idle.
            manager.poll_worker(Duration::ZERO);
        }
        manager.pump();
    }

    if let Some(worker) = manager.worker.take() {
        let _ = worker.shutdown(SHUTDOWN_GRACE);
    }
    manager.give_up();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    const TIMEOUT: Duration = Duration::from_secs(10);

    /// `cargo test --workspace` builds `pdf-worker` into the same target dir.
    fn test_worker_path() -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let dir = exe.parent().and_then(Path::parent).unwrap();
        let path = dir.join(format!("pdf-worker{}", std::env::consts::EXE_SUFFIX));
        assert!(path.exists(), "build pdf-worker first: {}", path.display());
        path
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tests/corpus/public")
            .join(name)
    }

    fn start() -> (WorkerHandle, mpsc::Receiver<JobEvent>) {
        let (tx, rx) = mpsc::channel();
        let handle = WorkerHandle::start(Some(test_worker_path()), move |event| {
            let _ = tx.send(event);
        });
        (handle, rx)
    }

    fn tile_request(document: u32, x: u32, generation: u64) -> TileRequest {
        TileRequest {
            document,
            page: 0,
            scale: 2.0,
            x,
            y: 0,
            width: 128,
            height: 128,
            generation,
        }
    }

    #[test]
    fn a_dead_worker_is_restarted_and_the_document_reopened() {
        let (handle, events) = start();
        let info = handle.open(&fixture("shapes.pdf")).wait(TIMEOUT).unwrap();
        let generation = handle.begin_view().unwrap();
        let before = handle
            .tile(tile_request(info.document, 0, generation))
            .wait(TIMEOUT)
            .unwrap();
        handle.kill_worker();
        // The same id still names the document, rendered by a new worker:
        // a tile not in the shell's cache, then one that is.
        let after = handle
            .tile(tile_request(info.document, 128, generation))
            .wait(TIMEOUT)
            .unwrap();
        assert_eq!(after.len(), TILE_HEADER + 128 * 128 * 4);
        let again = handle
            .tile(tile_request(info.document, 0, generation))
            .wait(TIMEOUT)
            .unwrap();
        assert!(before == again);
        // The job running when the worker died fails; it is not sent again.
        let id = handle.run(5_000, 10).unwrap();
        thread::sleep(Duration::from_millis(100));
        handle.kill_worker();
        assert_eq!(
            events.recv_timeout(TIMEOUT),
            Ok(failed(id, "worker exited"))
        );
        let next = handle.run(1, 0).unwrap();
        assert_eq!(
            events.recv_timeout(TIMEOUT),
            Ok(JobEvent {
                job_id: next,
                outcome: JobOutcome::Done { units: 1 }
            })
        );
        handle.stop();
    }

    #[test]
    fn restarts_are_bounded() {
        let (handle, events) = start();
        // Three restarts within the window, then the worker stays down.
        for _ in 0..MAX_RESTARTS {
            handle.kill_worker();
            let id = handle.run(1, 0).unwrap();
            assert_eq!(
                events.recv_timeout(TIMEOUT),
                Ok(JobEvent {
                    job_id: id,
                    outcome: JobOutcome::Done { units: 1 }
                })
            );
        }
        handle.kill_worker();
        let id = handle.run(1, 0).unwrap();
        let event = events.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(event.job_id, id);
        assert!(
            matches!(event.outcome, JobOutcome::Failed { .. }),
            "{event:?}"
        );
        assert_eq!(
            handle.open(&fixture("shapes.pdf")).wait(TIMEOUT),
            Err(CommandError::WorkerUnavailable)
        );
        handle.stop();
    }

    #[test]
    fn a_file_changed_since_it_was_opened_is_not_reopened() {
        let dir = std::env::temp_dir().join(format!("castle-restart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let copy = dir.join("copy.pdf");
        std::fs::copy(fixture("shapes.pdf"), &copy).unwrap();
        let (handle, _events) = start();
        let info = handle.open(&copy).wait(TIMEOUT).unwrap();
        let generation = handle.begin_view().unwrap();
        // Another file under the same name (another size).
        std::fs::copy(fixture("minimal.pdf"), &copy).unwrap();
        handle.kill_worker();
        assert_eq!(
            handle
                .tile(tile_request(info.document, 0, generation))
                .wait(TIMEOUT),
            Err(CommandError::InvalidArgument { field: "document" })
        );
        handle.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn arguments_are_validated_before_reaching_the_worker() {
        assert_eq!(validate_run(1, 0), Ok(()));
        assert_eq!(validate_run(MAX_UNITS, 0), Ok(()));
        // 600 x 100 ms is the longest job accepted.
        assert_eq!(validate_run(600, MAX_UNIT_DELAY_MS), Ok(()));
        for (units, delay, field) in [
            (0, 0, "units"),
            (MAX_UNITS + 1, 0, "units"),
            (1, MAX_UNIT_DELAY_MS + 1, "unitDelayMs"),
            (601, MAX_UNIT_DELAY_MS, "units"),
            (MAX_UNITS, 1, "units"),
        ] {
            assert_eq!(
                validate_run(units, delay),
                Err(CommandError::InvalidArgument { field })
            );
        }
        for scale in [0.0, -1.0, f64::NAN, f64::INFINITY, MAX_SCALE * 2.0] {
            assert_eq!(
                validate_scale(scale),
                Err(CommandError::InvalidArgument { field: "scale" })
            );
        }
        assert_eq!(validate_scale(MAX_SCALE), Ok(()));
    }

    #[test]
    fn job_runs_and_reports_through_the_real_worker() {
        let (handle, events) = start();
        let id = handle.run(5, 0).unwrap();
        assert_eq!(
            events.recv_timeout(TIMEOUT),
            Ok(JobEvent {
                job_id: id,
                outcome: JobOutcome::Done { units: 5 }
            })
        );
        handle.stop();
    }

    #[test]
    fn job_is_cancelled_and_stop_is_bounded() {
        let (handle, events) = start();
        let id = handle.run(5_000, 10).unwrap();
        handle.cancel(id).unwrap();
        assert_eq!(
            events.recv_timeout(TIMEOUT),
            Ok(JobEvent {
                job_id: id,
                outcome: JobOutcome::Cancelled
            })
        );
        let second = handle.run(5_000, 10).unwrap();
        assert!(second > id);
        let start = Instant::now();
        handle.stop();
        assert!(start.elapsed() < SHUTDOWN_GRACE + Duration::from_secs(1));
    }

    #[test]
    fn overlapping_jobs_are_queued_and_each_get_one_final_event() {
        let (handle, events) = start();
        let first = handle.run(5_000, 10).unwrap();
        let second = handle.run(1, 0).unwrap();
        let third = handle.run(1, 0).unwrap();
        // The third is still queued: cancelling it never reaches the worker.
        handle.cancel(third).unwrap();
        handle.cancel(first).unwrap();
        let mut seen: Vec<JobEvent> = (0..3)
            .map(|_| events.recv_timeout(TIMEOUT).unwrap())
            .collect();
        seen.sort_by_key(|e| e.job_id);
        assert_eq!(
            seen,
            vec![
                JobEvent {
                    job_id: first,
                    outcome: JobOutcome::Cancelled
                },
                JobEvent {
                    job_id: second,
                    outcome: JobOutcome::Done { units: 1 }
                },
                JobEvent {
                    job_id: third,
                    outcome: JobOutcome::Cancelled
                },
            ]
        );
        handle.stop();
    }

    #[test]
    fn missing_worker_reports_failure_instead_of_hanging() {
        let (tx, events) = mpsc::channel();
        let handle = WorkerHandle::start(Some(PathBuf::from("does-not-exist")), move |e| {
            let _ = tx.send(e);
        });
        let id = handle.run(1, 0).unwrap();
        assert_eq!(
            events.recv_timeout(TIMEOUT),
            Ok(failed(id, "worker unavailable"))
        );
        assert_eq!(
            handle.open(&fixture("shapes.pdf")).wait(TIMEOUT),
            Err(CommandError::WorkerUnavailable)
        );
        handle.stop();
    }

    #[test]
    fn a_page_gives_its_text_layer() {
        let (handle, _events) = start();
        assert_eq!(
            handle.page_text(1, 0, 1).wait(TIMEOUT),
            Err(CommandError::InvalidArgument { field: "document" })
        );
        let info = handle.open(&fixture("columns.pdf")).wait(TIMEOUT).unwrap();
        // Like a tile, a text layer names a view the shell started.
        assert_eq!(
            handle.page_text(info.document, 0, 1).wait(TIMEOUT),
            Err(CommandError::InvalidArgument {
                field: "generation"
            })
        );
        let generation = handle.begin_view().unwrap();
        let layer = handle
            .page_text(info.document, 0, generation)
            .wait(TIMEOUT)
            .unwrap();
        assert!(!layer.truncated && !layer.incomplete);
        let texts: Vec<String> = layer
            .lines
            .iter()
            .map(|line| {
                line.iter()
                    .map(|word| word.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        assert_eq!(texts[1..4], ["Left one", "Left two", "Left three"]);
        // In pixels of the page at scale 1: "A" at (60, 40), 18 px.
        let first = &layer.lines[0][0];
        assert_eq!((first.x0, first.y0, first.size), (60.0, 40.0, 18.0));
        assert_eq!(
            handle.page_text(info.document, 1, generation).wait(TIMEOUT),
            Err(CommandError::InvalidArgument { field: "page" })
        );
        handle.stop();
    }

    #[test]
    fn a_page_is_searched() {
        let (handle, _events) = start();
        let search = |query: &str, match_case: bool| SearchRequest {
            query: query.to_owned(),
            match_case,
            match_diacritics: false,
        };
        assert_eq!(
            handle
                .search_page(1, 0, search("left", false))
                .wait(TIMEOUT),
            Err(CommandError::InvalidArgument { field: "document" })
        );
        let info = handle.open(&fixture("columns.pdf")).wait(TIMEOUT).unwrap();
        // No view needed: a search goes on while the view changes.
        let found = handle
            .search_page(info.document, 0, search("LEFT", false))
            .wait(TIMEOUT)
            .unwrap();
        assert!(!found.truncated && !found.incomplete);
        assert_eq!(
            found.hits,
            [1, 2, 3].map(|line| HitDto {
                start: [line, 0, 0],
                end: [line, 0, 4],
            })
        );
        let found = handle
            .search_page(info.document, 0, search("LEFT", true))
            .wait(TIMEOUT)
            .unwrap();
        assert!(found.hits.is_empty());
        let long = "a".repeat(MAX_QUERY_BYTES + 1);
        for query in ["", " \t ", long.as_str()] {
            assert_eq!(
                handle
                    .search_page(info.document, 0, search(query, false))
                    .wait(TIMEOUT),
                Err(CommandError::InvalidArgument { field: "query" })
            );
        }
        // Blank only once folded: refused by the worker.
        assert_eq!(
            handle
                .search_page(info.document, 0, search("\u{AD}", false))
                .wait(TIMEOUT),
            Err(CommandError::Failed {
                reason: "invalidRequest"
            })
        );
        assert_eq!(
            handle
                .search_page(info.document, 1, search("left", false))
                .wait(TIMEOUT),
            Err(CommandError::InvalidArgument { field: "page" })
        );
        handle.stop();
    }

    #[test]
    fn a_waiting_search_page_is_replaced_by_the_next() {
        let (handle, _events) = start();
        let info = handle.open(&fixture("columns.pdf")).wait(TIMEOUT).unwrap();
        // A diagnostic job holds the worker: the searches wait in the queue.
        handle.run(20, 50).unwrap();
        let search = |query: &str| SearchRequest {
            query: query.to_owned(),
            match_case: false,
            match_diacritics: false,
        };
        let first = handle.search_page(info.document, 0, search("left"));
        let second = handle.search_page(info.document, 0, search("right"));
        assert_eq!(first.wait(TIMEOUT), Err(CommandError::Stale));
        assert_eq!(second.wait(TIMEOUT).unwrap().hits.len(), 3);
        handle.stop();
    }

    #[test]
    fn a_text_layer_of_an_older_view_is_dropped() {
        let (handle, _events) = start();
        let info = handle.open(&fixture("columns.pdf")).wait(TIMEOUT).unwrap();
        let old = handle.begin_view().unwrap();
        // A diagnostic job holds the worker: the text layer waits in the queue.
        handle.run(20, 50).unwrap();
        let pending = handle.page_text(info.document, 0, old);
        let current = handle.begin_view().unwrap();
        assert_eq!(pending.wait(TIMEOUT), Err(CommandError::Stale));
        // The current view's text still comes.
        assert!(
            handle
                .page_text(info.document, 0, current)
                .wait(TIMEOUT)
                .is_ok()
        );
        handle.stop();
    }

    #[test]
    fn a_document_is_opened_sized_and_tiled() {
        let (handle, _events) = start();
        // Nothing is open yet: the UI cannot name a document.
        assert_eq!(
            handle.page_size(1, 0, 1.0).wait(TIMEOUT),
            Err(CommandError::InvalidArgument { field: "document" })
        );
        let info = handle.open(&fixture("shapes.pdf")).wait(TIMEOUT).unwrap();
        assert_eq!(
            info,
            DocumentInfo {
                document: 1,
                pages: 3,
                name: "shapes.pdf".to_owned(),
                repair: RepairState::None,
            }
        );
        assert_eq!(
            handle.page_size(1, 0, 2.0).wait(TIMEOUT),
            Ok(PageSize {
                width: 400,
                height: 400
            })
        );
        assert_eq!(
            handle.page_size(1, 3, 1.0).wait(TIMEOUT),
            Err(CommandError::InvalidArgument { field: "page" })
        );
        // Generations come from the shell: none issued yet, then only 1.
        assert_eq!(
            handle.tile(tile_request(1, 0, 1)).wait(TIMEOUT),
            Err(CommandError::InvalidArgument {
                field: "generation"
            })
        );
        assert_eq!(handle.begin_view(), Ok(1));
        for generation in [0, 2] {
            assert_eq!(
                handle.tile(tile_request(1, 0, generation)).wait(TIMEOUT),
                Err(CommandError::InvalidArgument {
                    field: "generation"
                })
            );
        }
        let tile = handle.tile(tile_request(1, 0, 1)).wait(TIMEOUT).unwrap();
        assert_eq!(tile.len(), TILE_HEADER + 128 * 128 * 4);
        assert_eq!(
            tile.get(..TILE_HEADER),
            Some(&[128, 0, 128, 0, 0, 0, 0, 0][..])
        );
        // A second identical request is served from the cache.
        match handle.tile(tile_request(1, 0, 1)) {
            Pending::Ready(Ok(cached)) => assert!(Arc::ptr_eq(&cached, &tile)),
            _ => panic!("cached tile expected"),
        }
        for (request, field) in [
            (
                TileRequest {
                    width: MAX_TILE_SIDE + 1,
                    ..tile_request(1, 0, 1)
                },
                "size",
            ),
            (
                TileRequest {
                    scale: f64::NAN,
                    ..tile_request(1, 0, 1)
                },
                "scale",
            ),
            (tile_request(2, 0, 1), "document"),
        ] {
            assert_eq!(
                handle.tile(request).wait(TIMEOUT),
                Err(CommandError::InvalidArgument { field })
            );
        }
        // Outside the page: refused by the worker.
        assert_eq!(
            handle.tile(tile_request(1, 300, 1)).wait(TIMEOUT),
            Err(CommandError::Failed {
                reason: "invalidRequest"
            })
        );
        // Opening another document replaces the first one.
        let second = handle.open(&fixture("minimal.pdf")).wait(TIMEOUT).unwrap();
        assert_eq!((second.document, second.pages), (2, 1));
        assert_eq!(
            handle.page_size(1, 0, 1.0).wait(TIMEOUT),
            Err(CommandError::InvalidArgument { field: "document" })
        );
        assert_eq!(
            handle.open(&fixture("manifest.json")).wait(TIMEOUT),
            Err(CommandError::Failed {
                reason: "malformed"
            })
        );
        handle.stop();
    }

    #[test]
    fn a_repaired_document_is_announced_and_marked_in_its_tiles() {
        let (handle, _events) = start();
        let damaged = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tests/corpus/damaged/no-trailer.pdf");
        let info = handle.open(&damaged).wait(TIMEOUT).unwrap();
        assert_eq!(info.repair, RepairState::Repaired);
        assert_eq!(handle.begin_view(), Ok(1));
        let tile = handle
            .tile(TileRequest {
                scale: 0.1,
                width: 16,
                height: 16,
                ..tile_request(info.document, 0, 1)
            })
            .wait(TIMEOUT)
            .unwrap();
        // Bytes 6 and 7 of the header: repair state (1, repaired), padding.
        assert_eq!(tile.get(6..8), Some(&[1, 0][..]));
    }

    #[test]
    fn a_document_is_current_as_soon_as_it_is_announced() {
        // The answer used to come before the document became current: a
        // request sent at once could find no document (seen on CI).
        let (handle, _events) = start();
        for _ in 0..20 {
            let info = handle.open(&fixture("shapes.pdf")).wait(TIMEOUT).unwrap();
            assert!(
                handle
                    .page_size(info.document, 0, 1.0)
                    .wait(TIMEOUT)
                    .is_ok()
            );
        }
    }

    #[test]
    fn a_document_opened_after_its_timeout_is_closed() {
        let (handle, _events) = start();
        let first = handle.open(&fixture("shapes.pdf")).wait(TIMEOUT).unwrap();
        // Nobody waits for this one any more, as after a timeout.
        drop(handle.open(&fixture("minimal.pdf")));
        // Queued after the opening: the first document is still current.
        assert_eq!(
            handle.page_size(first.document, 0, 1.0).wait(TIMEOUT),
            Ok(PageSize {
                width: 200,
                height: 200
            })
        );
        assert_eq!(
            handle.page_size(first.document + 1, 0, 1.0).wait(TIMEOUT),
            Err(CommandError::InvalidArgument { field: "document" })
        );
        handle.stop();
    }

    #[test]
    fn tiles_of_an_older_view_are_dropped() {
        let (handle, events) = start();
        handle.open(&fixture("shapes.pdf")).wait(TIMEOUT).unwrap();
        // A long synthetic job keeps the worker busy, so the tiles queue.
        let blocker = handle.run(5_000, 10).unwrap();
        let first = handle.begin_view().unwrap();
        let old = handle.tile(tile_request(1, 0, first));
        // A new view drops the queued tiles of the previous one at once.
        let second = handle.begin_view().unwrap();
        assert_eq!(old.wait(TIMEOUT), Err(CommandError::Stale));
        let newer = handle.tile(tile_request(1, 128, second));
        handle.cancel(blocker).unwrap();
        assert!(newer.wait(TIMEOUT).is_ok());
        assert_eq!(
            events.recv_timeout(TIMEOUT).map(|e| e.outcome),
            Ok(JobOutcome::Cancelled)
        );
        handle.stop();
    }

    /// Worker start-up as the shell does it: launching the process and
    /// completing the version handshake, 20 times in a row; the first start
    /// is shown apart. In release, this test binary is a GUI program like
    /// the shell (`windows_subsystem`), so a console allocated for the
    /// worker would show here.
    /// `cargo test -p castle-pdf-desktop --release -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement"]
    fn measure_worker_start() {
        const RUNS: usize = 20;
        let path = test_worker_path();
        let mut times: Vec<Duration> = (0..RUNS)
            .map(|_| {
                let start = Instant::now();
                let worker = WorkerProcess::spawn(&path, HANDSHAKE_TIMEOUT).unwrap();
                let elapsed = start.elapsed();
                let _ = worker.shutdown(SHUTDOWN_GRACE);
                elapsed
            })
            .collect();
        let first = times.remove(0);
        times.sort();
        let at = |p: f64| times[((times.len() - 1) as f64 * p).round() as usize];
        println!(
            "worker start ({RUNS} runs): first {first:?}; then median {:?}, p95 {:?}, min {:?}, max {:?}",
            at(0.5),
            at(0.95),
            at(0.0),
            at(1.0)
        );
    }

    /// Time from opening a file to the last tile of its first page, 1000
    /// pixels wide (16 tiles), through the shell queue and the worker already
    /// started, then
    /// the same page again from the cache. The WebView side is not included.
    /// `cargo test -p castle-pdf-desktop --release -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement"]
    fn measure_first_page() {
        let (handle, events) = start();
        // The worker process starts in the background: wait until it answers.
        handle.run(1, 0).unwrap();
        events.recv_timeout(TIMEOUT).unwrap();
        let start = Instant::now();
        let info = handle.open(&fixture("shapes.pdf")).wait(TIMEOUT).unwrap();
        let view = handle.begin_view().unwrap();
        let scale = 5.0;
        let size = handle
            .page_size(info.document, 0, scale)
            .wait(TIMEOUT)
            .unwrap();
        let tiles = |generation| -> Vec<Pending<Arc<Vec<u8>>>> {
            let side = u32::from(MAX_TILE_SIDE);
            let mut pending = Vec::new();
            for y in (0..size.height).step_by(MAX_TILE_SIDE.into()) {
                for x in (0..size.width).step_by(MAX_TILE_SIDE.into()) {
                    pending.push(handle.tile(TileRequest {
                        document: info.document,
                        page: 0,
                        scale,
                        x,
                        y,
                        width: u16::try_from(side.min(size.width - x)).unwrap(),
                        height: u16::try_from(side.min(size.height - y)).unwrap(),
                        generation,
                    }));
                }
            }
            pending
        };
        let wait_all = |pending: Vec<Pending<Arc<Vec<u8>>>>| {
            let tiles: Vec<_> = pending
                .into_iter()
                .map(|p| p.wait(TIMEOUT).unwrap())
                .collect();
            tiles.len()
        };
        let count = wait_all(tiles(view));
        let first = start.elapsed();
        let again = Instant::now();
        let cached = wait_all(tiles(view));
        println!(
            "first page ({} x {} px, {count} tiles): {first:?} from opening; again from the cache ({cached} tiles): {:?}",
            size.width,
            size.height,
            again.elapsed()
        );
        handle.stop();
    }

    #[test]
    fn events_serialize_to_the_frontend_contract() {
        let event = JobEvent {
            job_id: 3,
            outcome: JobOutcome::Done { units: 9 },
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"jobId":3,"outcome":{"kind":"done","units":9}}"#
        );
        assert_eq!(
            serde_json::to_string(&CommandError::InvalidArgument { field: "units" }).unwrap(),
            r#"{"code":"invalidArgument","field":"units"}"#
        );
        assert_eq!(
            serde_json::to_string(&CommandError::Failed {
                reason: "encrypted"
            })
            .unwrap(),
            r#"{"code":"failed","reason":"encrypted"}"#
        );
        assert_eq!(
            serde_json::to_string(&DocumentInfo {
                document: 1,
                pages: 2,
                name: "a.pdf".to_owned(),
                repair: RepairState::Incomplete,
            })
            .unwrap(),
            r#"{"document":1,"pages":2,"name":"a.pdf","repair":"incomplete"}"#
        );
        let request: TileRequest = serde_json::from_str(
            r#"{"document":1,"page":0,"scale":1.5,"x":0,"y":256,"width":256,"height":10,"generation":4}"#,
        )
        .unwrap();
        assert_eq!(request.y, 256);
        assert!(
            serde_json::from_str::<TileRequest>(
                r#"{"document":1,"page":0,"scale":1.5,"x":0,"y":0,"width":65536,"height":1,"generation":1}"#
            )
            .is_err()
        );
    }
}
