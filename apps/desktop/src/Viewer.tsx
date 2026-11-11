import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";

import {
  MAX_TILE_SIDE,
  beginView,
  isStale,
  pageSize,
  renderTile,
  FONT_SUBSTITUTED,
  skippedLabels,
  worseRepair,
  type DocumentInfo,
  type PageSize,
  type RepairState,
} from "./ipc";

/** Tile requests in flight at once: keeps the shell queue busy, not full. */
const CONCURRENCY = 4;
const MIN_SCALE = 0.05;
const MAX_SCALE = 16;
/** Width used before the container has been measured (or in tests). */
const DEFAULT_WIDTH = 800;
/** Height used before the container has been measured (or in tests). */
const DEFAULT_HEIGHT = 600;
/** Width changes below this many CSS pixels do not re-render the pages. */
const WIDTH_STEP = 32;
/** CSS pixels between two pages. */
const GAP = 16;
/** Pages kept rendered (with a canvas) beyond those in view, each way. */
const KEEP_AROUND = 1;
/**
 * Most pixels in one page's canvas (64 MiB of RGBA in the WebView): a tall,
 * narrow page is drawn at a lower scale rather than in a huge canvas.
 */
export const MAX_CANVAS_PIXELS = 16 * 1024 * 1024;
/** Scale at which a page is first measured: pages up to 65 536 points fit. */
const MEASURE_SCALE = 0.25;

/** What the reader must know about a repaired document. */
const REPAIR_NOTES: Record<Exclude<RepairState, "none">, string> = {
  repaired:
    "Document réparé : sa structure était endommagée et a été reconstruite. " +
    "L'affichage peut différer d'autres lecteurs.",
  incomplete:
    "Document réparé partiellement : sa structure était endommagée et des éléments " +
    "peuvent manquer.",
};

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
  const bounded = Math.min(
    fit,
    Math.sqrt(MAX_CANVAS_PIXELS / (width * height)),
  );
  return Math.min(MAX_SCALE, Math.max(MIN_SCALE, bounded));
}

/**
 * Top of each page and the total height, in CSS pixels, from page heights:
 * pages one under the other, `GAP` apart.
 */
export function pageOffsets(heights: readonly number[]): {
  tops: number[];
  total: number;
} {
  const tops: number[] = [];
  let top = 0;
  for (const height of heights) {
    tops.push(top);
    top += height + GAP;
  }
  return { tops, total: Math.max(0, top - GAP) };
}

/**
 * First and last page that meet `[from, to)`; pages are sorted by top. When
 * none does (the view is in a gap or beyond the end), the first page below
 * `from`, or the last page.
 */
export function pagesBetween(
  tops: readonly number[],
  heights: readonly number[],
  from: number,
  to: number,
): [number, number] {
  let first = -1;
  let last = -1;
  tops.forEach((top, page) => {
    const bottom = top + (heights[page] ?? 0);
    if (bottom > from && top < to) {
      if (first < 0) {
        first = page;
      }
      last = page;
    }
  });
  if (first >= 0) {
    return [first, last];
  }
  const below = tops.findIndex((top) => top >= from);
  const page = below >= 0 ? below : Math.max(0, tops.length - 1);
  return [page, page];
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function useSize(element: RefObject<HTMLElement | null>): {
  width: number;
  height: number;
} {
  const [size, setSize] = useState({
    width: DEFAULT_WIDTH,
    height: DEFAULT_HEIGHT,
  });
  useEffect(() => {
    const target = element.current;
    if (target === null || typeof ResizeObserver === "undefined") {
      return;
    }
    const observer = new ResizeObserver(([entry]) => {
      const rect = entry?.contentRect;
      const width = Math.floor((rect?.width ?? 0) / WIDTH_STEP) * WIDTH_STEP;
      const height = Math.floor(rect?.height ?? 0);
      if (width > 0 && height > 0) {
        setSize((current) =>
          current.width === width && current.height === height
            ? current
            : { width, height },
        );
      }
    });
    observer.observe(target);
    return () => {
      observer.disconnect();
    };
  }, [element]);
  return size;
}

/** A page measured at the scale it is drawn at. */
interface Measured {
  scale: number;
  /** Size of its rendering in device pixels. */
  pixels: PageSize;
}

/** A page canvas on screen and the tiles drawn on it. */
interface Mounted {
  element: HTMLCanvasElement;
  scale: number;
  drawn: Set<string>;
}

type Failure = { page: number; message: string } | null;

export function Viewer({ info }: { info: DocumentInfo }) {
  const { document, pages } = info;
  const scroller = useRef<HTMLDivElement>(null);
  const { width, height: viewHeight } = useSize(scroller);
  const ratio =
    typeof window !== "undefined" && window.devicePixelRatio > 0
      ? window.devicePixelRatio
      : 1;

  // Page sizes in points, measured as pages come near the view; the first
  // one stands for those not measured yet.
  const [points, setPoints] = useState<(PageSize | null)[]>(() =>
    Array.from({ length: pages }, () => null),
  );
  // Sizes at the drawing scale, for one width: another width makes them
  // obsolete, so every page is measured and drawn again.
  const [sizes, setSizes] = useState<{
    width: number;
    pages: Map<number, Measured>;
  }>(() => ({
    width: 0,
    pages: new Map(),
  }));
  const measured = useMemo(
    () => (sizes.width === width ? sizes.pages : new Map<number, Measured>()),
    [sizes, width],
  );
  const [scrollTop, setScrollTop] = useState(0);
  const [failure, setFailure] = useState<Failure>(null);
  const [progress, setProgress] = useState({ done: 0, total: 0 });
  // What the drawn tiles reported, for every page drawn so far.
  const [reported, setReported] = useState({
    skipped: 0,
    repair: "none" as RepairState,
  });
  const mounted = useRef(new Map<number, Mounted>());

  const fallback = points.find((size) => size !== null) ?? null;
  const cssHeights = useMemo(
    () =>
      Array.from({ length: pages }, (_, page) => {
        const exact = measured.get(page);
        if (exact !== undefined) {
          return exact.pixels.height / ratio;
        }
        const size = points[page] ?? fallback;
        if (size === null) {
          return width * 1.3;
        }
        return (
          (size.height * fitScale(width, size.width, size.height, ratio)) /
          ratio
        );
      }),
    [pages, measured, points, fallback, width, ratio],
  );
  const { tops, total } = useMemo(() => pageOffsets(cssHeights), [cssHeights]);
  const [firstVisible, lastVisible] = pagesBetween(
    tops,
    cssHeights,
    scrollTop,
    scrollTop + viewHeight,
  );
  const keepFirst = Math.max(0, firstVisible - KEEP_AROUND);
  const keepLast = Math.min(pages - 1, lastVisible + KEEP_AROUND);

  // Releases the canvases of pages scrolled far away (their element is
  // unmounted; the registry must not keep it alive).
  useEffect(() => {
    for (const page of mounted.current.keys()) {
      if (page < keepFirst || page > keepLast) {
        mounted.current.delete(page);
      }
    }
  }, [keepFirst, keepLast]);

  // Keeps the page at the top of the view in place when pages above it
  // change height (measured, or a new width).
  const anchor = useRef<{ page: number; within: number } | null>(null);
  useLayoutEffect(() => {
    const element = scroller.current;
    const held = anchor.current;
    if (element === null || held === null) {
      return;
    }
    const top = tops[held.page];
    if (
      top !== undefined &&
      Math.abs(element.scrollTop - (top + held.within)) > 1
    ) {
      element.scrollTop = top + held.within;
      setScrollTop(element.scrollTop);
    }
  }, [tops]);

  const onScroll = useCallback(() => {
    const element = scroller.current;
    if (element === null) {
      return;
    }
    const top = element.scrollTop;
    setScrollTop(top);
    const [page] = pagesBetween(tops, cssHeights, top, top + 1);
    anchor.current = { page, within: top - (tops[page] ?? 0) };
  }, [tops, cssHeights]);

  // Measures the pages kept that are not measured yet, nearest first.
  useEffect(() => {
    let cancelled = false;
    const wanted: number[] = [];
    for (let page = keepFirst; page <= keepLast; page += 1) {
      if (!measured.has(page)) {
        wanted.push(page);
      }
    }
    if (wanted.length === 0) {
      return;
    }
    const measure = async () => {
      for (const page of wanted) {
        const base = await pageSize(document, page, MEASURE_SCALE);
        const size = {
          width: base.width / MEASURE_SCALE,
          height: base.height / MEASURE_SCALE,
        };
        const scale = fitScale(width, size.width, size.height, ratio);
        const pixels = await pageSize(document, page, scale);
        if (cancelled) {
          return;
        }
        setPoints((current) =>
          current.map((known, index) => (index === page ? size : known)),
        );
        setSizes((current) => ({
          width,
          pages: new Map(current.width === width ? current.pages : []).set(
            page,
            {
              scale,
              pixels,
            },
          ),
        }));
      }
    };
    measure().catch((error: unknown) => {
      if (!cancelled && !isStale(error)) {
        setFailure({ page: wanted[0] ?? 0, message: errorMessage(error) });
      }
    });
    return () => {
      cancelled = true;
    };
  }, [document, keepFirst, keepLast, measured, width, ratio]);

  // The pages to draw now: those in view that are measured.
  const drawable = useMemo(() => {
    const list: { page: number; scale: number; pixels: PageSize }[] = [];
    for (let page = firstVisible; page <= lastVisible; page += 1) {
      const exact = measured.get(page);
      if (exact !== undefined) {
        list.push({ page, ...exact });
      }
    }
    return list;
  }, [firstVisible, lastVisible, measured]);
  const drawKey = drawable
    .map(({ page, scale }) => `${String(page)}@${String(scale)}`)
    .join(",");

  // Draws the tiles of the pages in view not drawn yet. Each set of pages
  // in view is a new generation: the shell drops the tiles queued for the
  // views left (the pages scrolled away), and their late results are
  // ignored here.
  useEffect(() => {
    if (drawable.length === 0) {
      return;
    }
    let cancelled = false;
    let failed = false;
    const active = () => !cancelled && !failed;
    const draw = async () => {
      const generation = await beginView();
      const queue: { page: number; scale: number; rect: Rect; key: string }[] =
        [];
      for (const { page, scale, pixels } of drawable) {
        for (const rect of tileGrid(pixels.width, pixels.height)) {
          const key = `${String(rect.x)},${String(rect.y)}`;
          const target = mounted.current.get(page);
          if (target?.scale === scale && target.drawn.has(key)) {
            continue;
          }
          queue.push({ page, scale, rect, key });
        }
      }
      let done = 0;
      setProgress({ done, total: queue.length });
      let next = 0;
      const worker = async () => {
        while (active() && next < queue.length) {
          const item = queue[next];
          next += 1;
          if (item === undefined) {
            return;
          }
          let tile;
          try {
            tile = await renderTile({
              document,
              page: item.page,
              scale: item.scale,
              generation,
              ...item.rect,
            });
          } catch (error: unknown) {
            if (!isStale(error)) {
              failed = true;
              setFailure({ page: item.page, message: errorMessage(error) });
            }
            return;
          }
          const target = mounted.current.get(item.page);
          if (!active() || target?.scale !== item.scale) {
            return;
          }
          const context = target.element.getContext("2d");
          if (context === null) {
            failed = true;
            setFailure({
              page: item.page,
              message: "affichage indisponible (canvas)",
            });
            return;
          }
          context.putImageData(
            new ImageData(tile.pixels, tile.width, tile.height),
            item.rect.x,
            item.rect.y,
          );
          target.drawn.add(item.key);
          const { skipped, repair } = tile;
          setReported((current) => ({
            skipped: current.skipped | skipped,
            repair: worseRepair(current.repair, repair),
          }));
          done += 1;
          setProgress({ done, total: queue.length });
        }
      };
      await Promise.all(Array.from({ length: CONCURRENCY }, worker));
    };
    draw().catch((error: unknown) => {
      if (!cancelled && !isStale(error)) {
        setFailure({
          page: drawable[0]?.page ?? 0,
          message: errorMessage(error),
        });
      }
    });
    return () => {
      cancelled = true;
    };
    // `drawKey` names the pages and scales of `drawable`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [document, drawKey]);

  const scrollToPage = (page: number) => {
    const element = scroller.current;
    const top = tops[page];
    if (element === null || top === undefined) {
      return;
    }
    element.scrollTop = top;
    onScroll();
  };

  const current = firstVisible;
  const loading = progress.done < progress.total;
  const where = `Page ${String(current + 1)} sur ${String(pages)}`;
  const status =
    failure !== null
      ? `Page ${String(failure.page + 1)} sur ${String(pages)} : échec du rendu (${failure.message}).`
      : loading
        ? `${where} : rendu en cours…`
        : `${where}.`;
  const missing = skippedLabels(reported.skipped);
  const substituted = (reported.skipped & FONT_SUBSTITUTED) !== 0;
  // Shown as soon as the document is known to be repaired, or when a tile
  // reveals it (an object found by a scan while rendering).
  const repair = worseRepair(info.repair, reported.repair);

  // Only the pages near the view are in the document: the track's height
  // keeps the place of the others. A page is announced as soon as it is
  // there, drawn or not; its canvas comes once it is measured.
  const pageViews = Array.from(
    { length: keepLast - keepFirst + 1 },
    (_, index) => {
      const page = keepFirst + index;
      const top = tops[page] ?? 0;
      const cssHeight = cssHeights[page] ?? 0;
      const exact = measured.get(page);
      const size = points[page] ?? fallback;
      const cssWidth =
        exact !== undefined
          ? exact.pixels.width / ratio
          : size === null
            ? width
            : (size.width * fitScale(width, size.width, size.height, ratio)) /
              ratio;
      return (
        <div
          key={page}
          className="sheet"
          role="img"
          aria-label={`Page ${String(page + 1)} de ${info.name}`}
          style={{
            top: `${String(top)}px`,
            height: `${String(cssHeight)}px`,
            width: `${String(cssWidth)}px`,
          }}
        >
          {exact !== undefined && (
            <canvas
              // Another scale is another canvas: what was drawn is gone.
              key={exact.scale}
              // Called again at each render: only a new element starts anew.
              // Entries of pages no longer kept are pruned below.
              ref={(element) => {
                if (
                  element !== null &&
                  mounted.current.get(page)?.element !== element
                ) {
                  mounted.current.set(page, {
                    element,
                    scale: exact.scale,
                    drawn: new Set(),
                  });
                }
              }}
              width={exact.pixels.width}
              height={exact.pixels.height}
              data-page={page}
              aria-hidden="true"
            />
          )}
        </div>
      );
    },
  );

  return (
    <div className="viewer">
      <div className="toolbar">
        <button
          type="button"
          onClick={() => {
            scrollToPage(Math.max(0, current - 1));
          }}
          disabled={current === 0}
        >
          Page précédente
        </button>
        <button
          type="button"
          onClick={() => {
            scrollToPage(Math.min(pages - 1, current + 1));
          }}
          disabled={current + 1 >= pages}
        >
          Page suivante
        </button>
        <p role="status" aria-live="polite">
          {status}
        </p>
        {loading && (
          <span className="progress" aria-hidden="true">
            {progress.done}/{progress.total}
          </span>
        )}
      </div>
      {repair !== "none" && (
        <p className="repaired" role="note" aria-label="Document réparé">
          {REPAIR_NOTES[repair]}
        </p>
      )}
      {substituted && (
        <p className="partial" role="note" aria-label="Polices remplacées">
          Polices remplacées : des polices ne sont pas incluses dans le
          document. Le texte est affiché avec une police approchante, aux mêmes
          largeurs.
        </p>
      )}
      {missing.length > 0 && (
        <p className="partial" role="note" aria-label="Rendu partiel">
          Rendu partiel : non affichés pour l'instant — {missing.join(", ")}.
        </p>
      )}
      <div
        className="pages"
        ref={scroller}
        onScroll={onScroll}
        tabIndex={0}
        aria-label={`Pages de ${info.name}`}
      >
        <div className="track" style={{ height: `${String(total)}px` }}>
          {pageViews}
        </div>
      </div>
    </div>
  );
}
