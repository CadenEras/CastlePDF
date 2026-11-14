import { describe, expect, it } from "vitest";

import type { SearchHit } from "./ipc";
import { counts, searchOrder, step, wordSegments } from "./search";

const hit = (
  start: readonly [number, number, number],
  end: readonly [number, number, number],
): SearchHit => ({ start, end });

describe("search", () => {
  it("visits pages from the one in view, then from the first", () => {
    expect(searchOrder(5, 2)).toEqual([2, 3, 4, 0, 1]);
    expect(searchOrder(3, 0)).toEqual([0, 1, 2]);
    expect(searchOrder(3, 9)).toEqual([2, 0, 1]);
    expect(searchOrder(0, 0)).toEqual([]);
  });

  it("counts occurrences and ranks the current one in document order", () => {
    // Pages found out of order (the search started on page 4).
    const hits = new Map([
      [4, [hit([0, 0, 0], [0, 0, 1])]],
      [1, [hit([0, 0, 0], [0, 0, 1]), hit([1, 0, 0], [1, 0, 1])]],
    ]);
    expect(counts(hits, null)).toEqual({ total: 3, index: null });
    expect(counts(hits, { page: 1, rank: 1 })).toEqual({ total: 3, index: 1 });
    expect(counts(hits, { page: 4, rank: 0 })).toEqual({ total: 3, index: 2 });
  });

  it("moves to the next and previous occurrence, wrapping around", () => {
    const one = [hit([0, 0, 0], [0, 0, 1])];
    const two = [...one, hit([1, 0, 0], [1, 0, 1])];
    const hits = new Map([
      [5, one],
      [2, two],
    ]);
    expect(step(hits, null, 1)).toEqual({ page: 2, rank: 0 });
    expect(step(hits, null, -1)).toEqual({ page: 5, rank: 0 });
    expect(step(hits, { page: 2, rank: 0 }, 1)).toEqual({ page: 2, rank: 1 });
    expect(step(hits, { page: 2, rank: 1 }, 1)).toEqual({ page: 5, rank: 0 });
    expect(step(hits, { page: 5, rank: 0 }, 1)).toEqual({ page: 2, rank: 0 });
    expect(step(hits, { page: 2, rank: 0 }, -1)).toEqual({ page: 5, rank: 0 });
    expect(step(hits, { page: 5, rank: 0 }, -1)).toEqual({ page: 2, rank: 1 });
    expect(step(new Map(), null, 1)).toBeNull();
  });

  it("splits a word where occurrences start and end, in code points", () => {
    // "𝐀bc" holds a character outside the BMP: two UTF-16 units, one code point.
    expect(wordSegments("𝐀bcd", 0, 0, [hit([0, 0, 1], [0, 0, 3])])).toEqual({
      segments: [
        { text: "𝐀", hit: null },
        { text: "bc", hit: 0 },
        { text: "d", hit: null },
      ],
      gapHit: null,
    });
    // An occurrence across words and lines covers the end of one word, the
    // space before the next and its start.
    const across = [hit([0, 1, 2], [1, 0, 2])];
    expect(wordSegments("abcd", 0, 1, across).segments).toEqual([
      { text: "ab", hit: null },
      { text: "cd", hit: 0 },
    ]);
    expect(wordSegments("xyz", 1, 0, across)).toEqual({
      segments: [
        { text: "xy", hit: 0 },
        { text: "z", hit: null },
      ],
      gapHit: 0,
    });
    // A word between two occurrences, and one past them.
    const two = [hit([0, 0, 0], [0, 0, 1]), hit([0, 2, 0], [0, 2, 1])];
    expect(wordSegments("mid", 0, 1, two)).toEqual({
      segments: [{ text: "mid", hit: null }],
      gapHit: null,
    });
    expect(wordSegments("ab", 0, 2, two).segments).toEqual([
      { text: "a", hit: 1 },
      { text: "b", hit: null },
    ]);
  });
});
