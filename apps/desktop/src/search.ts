import { useCallback, useEffect, useRef, useState } from "react";

import {
  isStale,
  searchPage,
  type SearchHit,
  type SearchOptions,
  type TextPlace,
} from "./ipc";

/**
 * Most occurrences kept for a document: past them the search stops and says
 * so, rather than holding millions of results for a frequent letter.
 */
export const MAX_RESULTS = 10_000;

/** An occurrence: its page and its rank among the page's occurrences. */
export interface Place {
  page: number;
  rank: number;
}

/** A search of the document, page by page (ADR-0006, tranche 3.7). */
export interface SearchState {
  /** The query searched; empty when there is no search. */
  query: string;
  /** Occurrences of each page that has some, in reading order. */
  hits: ReadonlyMap<number, readonly SearchHit[]>;
  /** The occurrence shown, if any. */
  current: Place | null;
  /** Pages searched so far, out of `pages`. */
  searched: number;
  pages: number;
  running: boolean;
  /** More than `MAX_RESULTS` occurrences: the search stopped. */
  limited: boolean;
  /** Some page has text that could not be searched (missing or cut). */
  partial: boolean;
  error: string | null;
}

export const NO_SEARCH: SearchState = {
  query: "",
  hits: new Map(),
  current: null,
  searched: 0,
  pages: 0,
  running: false,
  limited: false,
  partial: false,
  error: null,
};

/** Pages in the order a search visits them: from `start` to the end, then from the first. */
export function searchOrder(pages: number, start: number): number[] {
  const first = Math.min(Math.max(0, start), Math.max(0, pages - 1));
  return Array.from({ length: pages }, (_, index) => (first + index) % pages);
}

/** Occurrences in all, and the 0-based rank of `current` among them. */
export function counts(
  hits: ReadonlyMap<number, readonly SearchHit[]>,
  current: Place | null,
): { total: number; index: number | null } {
  let total = 0;
  let index: number | null = null;
  for (const page of [...hits.keys()].sort((a, b) => a - b)) {
    if (current !== null && page === current.page) {
      index = total + current.rank;
    }
    total += hits.get(page)?.length ?? 0;
  }
  return { total, index };
}

/**
 * The occurrence after (`step` 1) or before (`step` -1) `current`, in
 * document order, wrapping at either end; the first or last one when there
 * is no current one.
 */
export function step(
  hits: ReadonlyMap<number, readonly SearchHit[]>,
  current: Place | null,
  direction: 1 | -1,
): Place | null {
  const pages = [...hits.keys()]
    .filter((page) => (hits.get(page)?.length ?? 0) > 0)
    .sort((a, b) => a - b);
  const last = (page: number) => (hits.get(page)?.length ?? 1) - 1;
  const firstPage = pages[0];
  const lastPage = pages[pages.length - 1];
  if (firstPage === undefined || lastPage === undefined) {
    return null;
  }
  if (current === null) {
    return direction === 1
      ? { page: firstPage, rank: 0 }
      : { page: lastPage, rank: last(lastPage) };
  }
  const rank = current.rank + direction;
  if (rank >= 0 && rank <= last(current.page) && hits.has(current.page)) {
    return { page: current.page, rank };
  }
  if (direction === 1) {
    const page = pages.find((other) => other > current.page) ?? firstPage;
    return { page, rank: 0 };
  }
  const page =
    [...pages].reverse().find((other) => other < current.page) ?? lastPage;
  return { page, rank: last(page) };
}

/** A piece of a word: its text and whether an occurrence covers it. */
export interface Segment {
  text: string;
  /** Index of the occurrence covering it among the page's, if any. */
  hit: number | null;
}

/**
 * Splits the text of word `word` of line `line` into the pieces the page's
 * occurrences cover and those they do not, counting characters in code
 * points as the shell does. The same for the space before the word
 * (`gap`), which an occurrence covers when it runs across it.
 */
export function wordSegments(
  text: string,
  line: number,
  word: number,
  hits: readonly SearchHit[],
): { segments: Segment[]; gapHit: number | null } {
  const chars = Array.from(text);
  const marks: (number | null)[] = chars.map(() => null);
  let gapHit: number | null = null;
  const before = (a: TextPlace, b: TextPlace) =>
    a[0] !== b[0] ? a[0] < b[0] : a[1] !== b[1] ? a[1] < b[1] : a[2] < b[2];
  hits.forEach((hit, index) => {
    const here: TextPlace = [line, word, 0];
    const after: TextPlace = [line, word, chars.length];
    // Not on this word: ends before it starts, or starts after it ends.
    if (!before(here, hit.end) || !before(hit.start, after)) {
      return;
    }
    // Starts on an earlier word: covers the space before this one.
    if (before(hit.start, here)) {
      gapHit = index;
    }
    const from =
      hit.start[0] === line && hit.start[1] === word ? hit.start[2] : 0;
    const to =
      hit.end[0] === line && hit.end[1] === word
        ? Math.min(hit.end[2], chars.length)
        : chars.length;
    for (let at = from; at < to; at += 1) {
      marks[at] = index;
    }
  });
  const segments: Segment[] = [];
  chars.forEach((char, at) => {
    const hit = marks[at] ?? null;
    const previous = segments[segments.length - 1];
    if (previous !== undefined && previous.hit === hit) {
      previous.text += char;
    } else {
      segments.push({ text: char, hit });
    }
  });
  return { segments, gapHit };
}

/**
 * Searches a document a page at a time, from the page in view, and keeps
 * the occurrences found; `show` is called with the first one found and with
 * each one moved to. A new query, new options or `clear` stop the search
 * running: its later results are dropped.
 */
export function useSearch(
  document: number,
  pages: number,
  show: (place: Place) => void,
): {
  state: SearchState;
  search: (query: string, options: SearchOptions, start: number) => void;
  next: () => void;
  previous: () => void;
  clear: () => void;
} {
  const [state, setState] = useState<SearchState>(NO_SEARCH);
  const run = useRef(0);
  const showRef = useRef(show);
  useEffect(() => {
    showRef.current = show;
  }, [show]);
  // A search never outlives its document or its view.
  useEffect(
    () => () => {
      run.current += 1;
    },
    [document],
  );

  const clear = useCallback(() => {
    run.current += 1;
    setState(NO_SEARCH);
  }, []);

  const search = useCallback(
    (query: string, options: SearchOptions, start: number) => {
      run.current += 1;
      const token = run.current;
      if (query.trim() === "") {
        setState(NO_SEARCH);
        return;
      }
      setState({ ...NO_SEARCH, query, pages, running: true });
      const found = new Map<number, readonly SearchHit[]>();
      let total = 0;
      let shown = false;
      const loop = async () => {
        let searched = 0;
        for (const page of searchOrder(pages, start)) {
          const result = await searchPage(document, page, query, options);
          if (token !== run.current) {
            return;
          }
          searched += 1;
          let hits = result.hits;
          const limited = total + hits.length > MAX_RESULTS;
          if (limited) {
            hits = hits.slice(0, MAX_RESULTS - total);
          }
          total += hits.length;
          if (hits.length > 0) {
            found.set(page, hits);
          }
          const partial = result.truncated || result.incomplete;
          const first = !shown && hits.length > 0;
          if (first) {
            shown = true;
          }
          const snapshot = new Map(found);
          setState((current) => ({
            ...current,
            hits: snapshot,
            current: first ? { page, rank: 0 } : current.current,
            searched,
            running: !limited && searched < pages,
            limited: current.limited || limited,
            partial: current.partial || partial,
          }));
          if (first) {
            showRef.current({ page, rank: 0 });
          }
          if (limited) {
            return;
          }
        }
      };
      loop().catch((error: unknown) => {
        // Replaced by a newer search, or the document changed.
        if (token !== run.current || isStale(error)) {
          return;
        }
        setState((current) => ({
          ...current,
          running: false,
          error: error instanceof Error ? error.message : String(error),
        }));
      });
    },
    [document, pages],
  );

  const { hits, current } = state;
  const move = useCallback(
    (direction: 1 | -1) => {
      const place = step(hits, current, direction);
      if (place === null) {
        return;
      }
      setState((current) => ({ ...current, current: place }));
      showRef.current(place);
    },
    [hits, current],
  );
  const next = useCallback(() => {
    move(1);
  }, [move]);
  const previous = useCallback(() => {
    move(-1);
  }, [move]);

  return { state, search, next, previous, clear };
}
