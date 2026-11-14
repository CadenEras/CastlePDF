//! Documents and tiles through the real `pdf-worker` binary and its pipes.

// Test-only crate: helpers outside `#[test]` functions may fail loudly.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pdf_worker::client::WorkerProcess;
use pdf_worker::documents::MAX_DOCUMENTS;
use pdf_worker::protocol::{ErrorCode, Repair, Request, Response, Skipped, TileRect};

const TIMEOUT: Duration = Duration::from_secs(10);

fn spawn() -> WorkerProcess {
    WorkerProcess::spawn(Path::new(env!("CARGO_BIN_EXE_pdf-worker")), TIMEOUT)
        .expect("worker starts and handshakes")
}

fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/public")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn call(worker: &WorkerProcess, request: &Request) -> Response {
    worker.send(request).unwrap();
    worker.recv_timeout(TIMEOUT).unwrap()
}

fn open(worker: &WorkerProcess, id: u64, name: &str) -> Response {
    call(
        worker,
        &Request::OpenDocument {
            id,
            path: fixture(name),
        },
    )
}

#[test]
fn a_page_gives_its_text_layer_through_the_pipes() {
    let worker = spawn();
    assert!(matches!(
        open(&worker, 1, "columns.pdf"),
        Response::DocumentOpened { document: 1, .. }
    ));
    let Response::PageText { id: 2, layer } = call(
        &worker,
        &Request::PageText {
            id: 2,
            document: 1,
            page: 0,
        },
    ) else {
        panic!("text layer expected");
    };
    assert!(!layer.truncated && !layer.incomplete);
    assert_eq!(layer.lines.len(), 9);
    assert_eq!(layer.lines[1].words[0].text, "Left");
    // An unknown document or page is refused.
    for (id, document, page, code) in [
        (3, 2, 0, ErrorCode::UnknownDocument),
        (4, 1, 1, ErrorCode::InvalidRequest),
    ] {
        assert_eq!(
            call(&worker, &Request::PageText { id, document, page }),
            Response::JobFailed { id, code }
        );
    }
}

#[test]
fn a_search_places_its_hits_in_the_text_layer() {
    let worker = spawn();
    assert!(matches!(
        open(&worker, 1, "columns.pdf"),
        Response::DocumentOpened { document: 1, .. }
    ));
    let Response::PageText { layer, .. } = call(
        &worker,
        &Request::PageText {
            id: 2,
            document: 1,
            page: 0,
        },
    ) else {
        panic!("text layer expected");
    };
    let search = |id: u64, document: u32, query: &str| {
        call(
            &worker,
            &Request::SearchPage {
                id,
                document,
                page: 0,
                query: query.to_owned(),
                match_case: false,
                match_diacritics: false,
            },
        )
    };
    let Response::SearchHits {
        id: 3,
        hits,
        truncated: false,
        incomplete: false,
    } = search(3, 1, "LEFT")
    else {
        panic!("hits expected");
    };
    assert_eq!(hits.len(), 3);
    for hit in &hits {
        // Each hit names the word "Left" of the layer, whole.
        let [line, word, char] = hit.start.map(|index| index as usize);
        assert_eq!(layer.lines[line].words[word].text, "Left");
        assert_eq!(char, 0);
        assert_eq!(hit.end, [hit.start[0], hit.start[1], 4]);
    }
    // Across two lines of the layer.
    let Response::SearchHits { hits, .. } = search(4, 1, "three right") else {
        panic!("hits expected");
    };
    assert_eq!(hits.len(), 1);
    let [start, end] = [hits[0].start, hits[0].end].map(|[l, w, c]| {
        let word = &layer.lines[l as usize].words[w as usize].text;
        (word.clone(), c)
    });
    assert_eq!(
        (start, end),
        (("three".to_owned(), 0), ("Right".to_owned(), 5))
    );
    // A query empty once folded, or too long, an unknown document.
    let long = "a".repeat(1025);
    for (id, document, query, code) in [
        (5, 1, " \t", ErrorCode::InvalidRequest),
        (6, 1, long.as_str(), ErrorCode::InvalidRequest),
        (7, 2, "left", ErrorCode::UnknownDocument),
    ] {
        assert_eq!(
            search(id, document, query),
            Response::JobFailed { id, code }
        );
    }
}

#[test]
fn a_page_is_rendered_tile_by_tile() {
    let worker = spawn();
    assert_eq!(
        open(&worker, 1, "shapes.pdf"),
        Response::DocumentOpened {
            id: 1,
            document: 1,
            pages: 3,
            repair: Repair::None,
        }
    );
    assert_eq!(
        call(
            &worker,
            &Request::PageSize {
                id: 2,
                document: 1,
                page: 0,
                scale: 2.0
            }
        ),
        Response::PageSize {
            id: 2,
            width: 400,
            height: 400
        }
    );
    // A 400 x 400 page: four tiles, the last ones cut at the page edge.
    for (id, (x, y, width, height)) in (3..).zip([
        (0, 0, 256, 256),
        (256, 0, 144, 256),
        (0, 256, 256, 144),
        (256, 256, 144, 144),
    ]) {
        let rect = TileRect {
            x,
            y,
            width,
            height,
        };
        match call(
            &worker,
            &Request::RenderTile {
                id,
                document: 1,
                page: 0,
                scale: 2.0,
                rect,
            },
        ) {
            Response::Tile {
                id: got,
                width: w,
                height: h,
                skipped,
                repair,
                pixels,
            } => {
                assert_eq!(
                    (got, w, h, skipped, repair),
                    (id, width, height, Skipped(0), Repair::None)
                );
                assert_eq!(pixels.len(), usize::from(width) * usize::from(height) * 4);
            }
            other => panic!("tile expected, got {other:?}"),
        }
    }
    // Closing forgets the document: later requests name an unknown one.
    worker
        .send(&Request::CloseDocument { document: 1 })
        .unwrap();
    assert_eq!(
        call(
            &worker,
            &Request::PageSize {
                id: 10,
                document: 1,
                page: 0,
                scale: 1.0
            }
        ),
        Response::JobFailed {
            id: 10,
            code: ErrorCode::UnknownDocument
        }
    );
}

#[test]
fn errors_name_their_cause() {
    let worker = spawn();
    assert_eq!(
        open(&worker, 1, "missing.pdf"),
        Response::JobFailed {
            id: 1,
            code: ErrorCode::Unreadable
        }
    );
    assert_eq!(
        open(&worker, 2, "manifest.json"),
        Response::JobFailed {
            id: 2,
            code: ErrorCode::Malformed
        }
    );
    assert!(matches!(
        open(&worker, 3, "minimal.pdf"),
        Response::DocumentOpened { document: 1, .. }
    ));
    let tile = |id, rect| Request::RenderTile {
        id,
        document: 1,
        page: 0,
        scale: 1.0,
        rect,
    };
    let rect = TileRect {
        x: 0,
        y: 0,
        width: 64,
        height: 64,
    };
    match call(&worker, &tile(4, rect)) {
        Response::Tile { skipped, .. } => {
            assert_eq!(skipped, Skipped(Skipped::FONT_SUBSTITUTED));
        }
        other => panic!("tile expected, got {other:?}"),
    }
    for (id, rect) in [
        (5, TileRect { width: 257, ..rect }),
        (6, TileRect { x: 595, ..rect }),
    ] {
        assert_eq!(
            call(&worker, &tile(id, rect)),
            Response::JobFailed {
                id,
                code: ErrorCode::InvalidRequest
            }
        );
    }
    // The table is bounded; the first document is still open.
    for id in 7..7 + MAX_DOCUMENTS as u64 - 1 {
        assert!(matches!(
            open(&worker, id, "minimal.pdf"),
            Response::DocumentOpened { .. }
        ));
    }
    assert_eq!(
        open(&worker, 20, "minimal.pdf"),
        Response::JobFailed {
            id: 20,
            code: ErrorCode::TooManyDocuments
        }
    );
}

/// Cost of a full 256 x 256 tile through the worker pipe (render, encode,
/// frame, decode), median of 20, on the machine running the tests. Printed,
/// not asserted: `cargo test -p pdf-worker --release --test documents -- --ignored --nocapture`.
#[test]
#[ignore = "measurement"]
fn measure_tile_round_trip() {
    let worker = spawn();
    assert!(matches!(
        open(&worker, 1, "shapes.pdf"),
        Response::DocumentOpened { .. }
    ));
    let mut times = Vec::new();
    for id in 2..22 {
        let start = Instant::now();
        let response = call(
            &worker,
            &Request::RenderTile {
                id,
                document: 1,
                page: 0,
                scale: 4.0,
                rect: TileRect {
                    x: 256,
                    y: 256,
                    width: 256,
                    height: 256,
                },
            },
        );
        times.push(start.elapsed());
        assert!(matches!(response, Response::Tile { .. }));
    }
    times.sort();
    println!(
        "256 x 256 tile through the worker pipe: median {:?}, min {:?}, max {:?}",
        times[times.len() / 2],
        times[0],
        times[times.len() - 1]
    );
}

#[test]
fn a_repaired_document_says_so() {
    let worker = spawn();
    let damaged = |name: &str| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/corpus/damaged")
            .join(name)
            .to_string_lossy()
            .into_owned()
    };
    // Index rebuilt when opening (no trailer), then an object found by a
    // scan while the page tree is read (entries off their objects).
    for (id, name) in [(1, "no-trailer.pdf"), (2, "shifted-objects.pdf")] {
        let response = call(
            &worker,
            &Request::OpenDocument {
                id,
                path: damaged(name),
            },
        );
        assert!(
            matches!(
                response,
                Response::DocumentOpened {
                    repair: Repair::Repaired,
                    ..
                }
            ),
            "{name}: {response:?}"
        );
    }
    let tile = call(
        &worker,
        &Request::RenderTile {
            id: 3,
            document: 1,
            page: 0,
            scale: 1.0,
            rect: TileRect {
                x: 0,
                y: 0,
                width: 8,
                height: 8,
            },
        },
    );
    assert!(
        matches!(
            tile,
            Response::Tile {
                repair: Repair::Repaired,
                ..
            }
        ),
        "{tile:?}"
    );
}
