// No console window for release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cache;
mod worker;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, DragDropEvent, Emitter, Manager, RunEvent, State, WindowEvent};

use worker::{CommandError, DocumentInfo, JobEvent, PageSize, Pending, TileRequest, WorkerHandle};

const JOB_EVENT: &str = "worker-job";
const DOCUMENT_EVENT: &str = "document";
/// Opening reads and parses the whole file (up to 512 MiB).
const OPEN_TIMEOUT: Duration = Duration::from_secs(120);
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// Announced to the UI as the shell opens a document: the file name only.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum DocumentEvent {
    Opening { name: String },
    Opened(DocumentInfo),
    Failed { name: String, error: CommandError },
}

/// A document event with its rank: the UI applies an event only if it is
/// newer than the last one it applied, whether it came as an event or from
/// `current_document`.
#[derive(Debug, Clone, Serialize)]
struct Announcement {
    sequence: u64,
    #[serde(flatten)]
    event: DocumentEvent,
}

/// The last document event, for a UI that subscribed after it was sent (the
/// file given on the command line opens before the page has loaded).
#[derive(Default)]
struct LastDocument(Mutex<Option<Announcement>>);

#[tauri::command]
fn current_document(last: State<'_, LastDocument>) -> Option<Announcement> {
    last.0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Waits for `pending` on a blocking thread, never on the UI thread.
async fn reply<T: Send + 'static>(
    pending: Pending<T>,
    timeout: Duration,
) -> Result<T, CommandError> {
    tauri::async_runtime::spawn_blocking(move || pending.wait(timeout))
        .await
        .unwrap_or(Err(CommandError::WorkerUnavailable))
}

#[tauri::command]
fn worker_run(
    worker: State<'_, WorkerHandle>,
    units: u64,
    unit_delay_ms: u8,
) -> Result<u64, CommandError> {
    worker.run(units, unit_delay_ms)
}

#[tauri::command]
fn worker_cancel(worker: State<'_, WorkerHandle>, job_id: u64) -> Result<(), CommandError> {
    worker.cancel(job_id)
}

#[tauri::command]
async fn page_size(
    worker: State<'_, WorkerHandle>,
    document: u32,
    page: u32,
    scale: f64,
) -> Result<PageSize, CommandError> {
    reply(worker.page_size(document, page, scale), REPLY_TIMEOUT).await
}

/// Starts a new view and returns its generation, numbered by the shell.
#[tauri::command]
fn begin_view(worker: State<'_, WorkerHandle>) -> Result<u64, CommandError> {
    worker.begin_view()
}

/// Raw bytes, received as an `ArrayBuffer`: no JSON or base64 (ADR-0003).
#[tauri::command]
async fn render_tile(
    worker: State<'_, WorkerHandle>,
    request: TileRequest,
) -> Result<tauri::ipc::Response, CommandError> {
    let tile = reply(worker.tile(request), REPLY_TIMEOUT).await?;
    Ok(tauri::ipc::Response::new(tile.as_ref().clone()))
}

fn publish_document(app: &AppHandle, event: DocumentEvent) {
    let last = app.state::<LastDocument>();
    // Held while emitting, so that events leave in sequence order.
    let mut last = last.0.lock().unwrap_or_else(PoisonError::into_inner);
    let sequence = last.as_ref().map_or(1, |previous| previous.sequence + 1);
    let announcement = Announcement { sequence, event };
    if let Err(error) = app.emit(DOCUMENT_EVENT, &announcement) {
        eprintln!("castle-pdf: cannot publish document event: {error}");
    }
    *last = Some(announcement);
}

/// Opens a file chosen by the user through the operating system (dropped on
/// the window, or given on the command line); the UI never names a path.
fn open_document(app: &AppHandle, path: PathBuf) {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    publish_document(app, DocumentEvent::Opening { name: name.clone() });
    let pending = app.state::<WorkerHandle>().open(&path);
    let app = app.clone();
    let waiter = std::thread::Builder::new()
        .name("pdf-open".into())
        .spawn(move || {
            let event = match pending.wait(OPEN_TIMEOUT) {
                Ok(info) => DocumentEvent::Opened(info),
                Err(error) => DocumentEvent::Failed { name, error },
            };
            publish_document(&app, event);
        });
    if let Err(error) = waiter {
        eprintln!("castle-pdf: cannot wait for the document: {error}");
    }
}

fn main() -> ExitCode {
    let app = tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();
            let worker =
                WorkerHandle::start(worker::bundled_worker_path(), move |event: JobEvent| {
                    if let Err(error) = handle.emit(JOB_EVENT, event) {
                        eprintln!("castle-pdf: cannot publish job event: {error}");
                    }
                });
            app.manage(worker);
            app.manage(LastDocument::default());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) = event {
                // One file at a time; other drops are ignored.
                if let [path] = paths.as_slice() {
                    open_document(window.app_handle(), path.clone());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            worker_run,
            worker_cancel,
            current_document,
            begin_view,
            page_size,
            render_tile
        ])
        .build(tauri::generate_context!());

    match app {
        Ok(app) => {
            app.run(|app, event| match event {
                // A file given on the command line opens once the UI is up.
                RunEvent::Ready => {
                    if let Some(path) = std::env::args_os().nth(1) {
                        open_document(app, PathBuf::from(path));
                    }
                }
                RunEvent::Exit => app.state::<WorkerHandle>().stop(),
                _ => {}
            });
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("castle-pdf: cannot start: {error}");
            ExitCode::FAILURE
        }
    }
}
