import { useEffect, useRef, useState, type RefObject } from "react";

import {
  MAX_TILE_SIDE,
  beginView,
  isStale,
  pageSize,
  renderTile,
  skippedLabels,
  type DocumentInfo,
} from "./ipc";

/** Tile requests in flight at once: keeps the shell queue busy, not full. */
const CONCURRENCY = 4;
const MIN_SCALE = 0.05;
const MAX_SCALE = 16;
/** Width used before the container has been measured (or in tests). */
const DEFAULT_WIDTH = 800;
/** Width changes below this many CSS pixels do not re-render the page. */
const WIDTH_STEP = 32;
/**
 * Most pixels in the canvas (64 MiB of RGBA in the WebView): a tall, narrow
 * page is drawn at a lower scale rather than in a huge canvas.
 */
export const MAX_CANVAS_PIXELS = 16 * 1024 * 1024;
/** Scale at which the page is first measured: pages up to 65 536 points fit. */
const MEASURE_SCALE = 0.25;

type RenderState =
  | { kind: "loading"; done: number; total: number }
  | { kind: "ready"; skipped: number }
  | { kind: "error"; message: string };

const PREPARING: RenderState = { kind: "loading", done: 0, total: 0 };

interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** The tiles covering a page, row by row from the top. */
export function tileGrid(width: number, height: number): Rect[] {
  const tiles: Rect[] = [];
  for (let y = 0; y < height; y += MAX_TILE_SIDE) {
    for (let x = 0; x < width; x += MAX_TILE_SIDE) {
      tiles.push({
        x,
        y,
        width: Math.min(MAX_TILE_SIDE, width - x),
        height: Math.min(MAX_TILE_SIDE, height - y),
      });
    }
  }
  return tiles;
}

/**
 * Device pixels per point that fit `available` CSS pixels across a page of
 * `width` x `height` points, within `MAX_CANVAS_PIXELS`.
 */
export function fitScale(
  available: number,
  width: number,
  height: number,
  pixelRatio: number,
): number {
  const fit = (available * pixelRatio) / width;
  const bounded = Math.min(fit, Math.sqrt(MAX_CANVAS_PIXELS / (width * height)));
  return Math.min(MAX_SCALE, Math.max(MIN_SCALE, bounded));
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function describe(state: RenderState, page: number, pages: number): string {
  const where = `Page ${String(page + 1)} sur ${String(pages)}`;
  switch (state.kind) {
    case "loading":
      // Announced once per view: the tile count is shown apart, not read out.
      return state.total === 0 ? `${where} : préparation…` : `${where} : rendu en cours…`;
    case "ready":
      return `${where}.`;
    case "error":
      return `${where} : échec du rendu (${state.message}).`;
  }
}

/** Width of `element` in CSS pixels, rounded down to `WIDTH_STEP`. */
function useWidth(element: RefObject<HTMLElement | null>): number {
  const [width, setWidth] = useState(DEFAULT_WIDTH);
  useEffect(() => {
    const target = element.current;
    if (target === null || typeof ResizeObserver === "undefined") {
      return;
    }
    const observer = new ResizeObserver(([entry]) => {
      const measured = Math.floor((entry?.contentRect.width ?? 0) / WIDTH_STEP) * WIDTH_STEP;
      if (measured > 0) {
        setWidth(measured);
      }
    });
    observer.observe(target);
    return () => {
      observer.disconnect();
    };
  }, [element]);
  return width;
}

export function Viewer({ info }: { info: DocumentInfo }) {
  const [page, setPage] = useState(0);
  // The state of one view: another view shows as being prepared.
  const [rendered, setRendered] = useState<{ view: string; state: RenderState } | null>(null);
  const container = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const width = useWidth(container);
  const { document } = info;
  const view = `${String(document)}/${String(page)}/${String(width)}`;
  const state = rendered?.view === view ? rendered.state : PREPARING;

  useEffect(() => {
    // Each view (document, page, width) is a new generation: the shell drops
    // the tiles of older ones, and their late results are ignored here.
    let cancelled = false;
    // The first failure stops the other tile loops; the error stays shown.
    let failed = false;
    const active = () => !cancelled && !failed;
    const setState = (next: RenderState) => {
      setRendered({ view, state: next });
    };

    const draw = async () => {
      const generation = await beginView();
      const ratio = window.devicePixelRatio > 0 ? window.devicePixelRatio : 1;
      const base = await pageSize(document, page, MEASURE_SCALE);
      const scale = fitScale(
        width,
        base.width / MEASURE_SCALE,
        base.height / MEASURE_SCALE,
        ratio,
      );
      const size = await pageSize(document, page, scale);
      const target = canvas.current;
      if (!active() || target === null) {
        return;
      }
      target.width = size.width;
      target.height = size.height;
      target.style.width = `${String(size.width / ratio)}px`;
      const context = target.getContext("2d");
      if (context === null) {
        throw new Error("affichage indisponible (canvas)");
      }
      const tiles = tileGrid(size.width, size.height);
      let done = 0;
      let skipped = 0;
      setState({ kind: "loading", done, total: tiles.length });
      let next = 0;
      const worker = async () => {
        while (active() && next < tiles.length) {
          const rect = tiles[next];
          next += 1;
          if (rect === undefined) {
            return;
          }
          let tile;
          try {
            tile = await renderTile({ document, page, scale, generation, ...rect });
          } catch (error: unknown) {
            if (!isStale(error)) {
              failed = true;
            }
            throw error;
          }
          if (!active()) {
            return;
          }
          context.putImageData(new ImageData(tile.pixels, tile.width, tile.height), rect.x, rect.y);
          skipped |= tile.skipped;
          done += 1;
          setState({ kind: "loading", done, total: tiles.length });
        }
      };
      await Promise.all(Array.from({ length: CONCURRENCY }, worker));
      if (active()) {
        setState({ kind: "ready", skipped });
      }
    };

    draw().catch((error: unknown) => {
      if (!cancelled && !isStale(error)) {
        setState({ kind: "error", message: errorMessage(error) });
      }
    });
    return () => {
      cancelled = true;
    };
  }, [document, page, width, view]);

  const missing = state.kind === "ready" ? skippedLabels(state.skipped) : [];

  return (
    <div className="viewer">
      <div className="toolbar">
        <button
          type="button"
          onClick={() => {
            setPage((current) => Math.max(0, current - 1));
          }}
          disabled={page === 0}
        >
          Page précédente
        </button>
        <button
          type="button"
          onClick={() => {
            setPage((current) => Math.min(info.pages - 1, current + 1));
          }}
          disabled={page + 1 >= info.pages}
        >
          Page suivante
        </button>
        <p role="status" aria-live="polite">
          {describe(state, page, info.pages)}
        </p>
        {state.kind === "loading" && state.total > 0 && (
          <span className="progress" aria-hidden="true">
            {state.done}/{state.total}
          </span>
        )}
      </div>
      {missing.length > 0 && (
        <p className="partial" role="note">
          Rendu partiel : non affichés pour l'instant — {missing.join(", ")}.
        </p>
      )}
      <div className="page" ref={container}>
        <canvas
          ref={canvas}
          role="img"
          aria-label={`Page ${String(page + 1)} de ${info.name}`}
        />
      </div>
    </div>
  );
}
