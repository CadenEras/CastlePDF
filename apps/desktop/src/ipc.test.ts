import { describe, expect, it } from "vitest";

import {
  MAX_LAYER_WORDS,
  ShellError,
  TILE_HEADER,
  describeCommandError,
  isStale,
  parseDocumentEvent,
  parseJobEvent,
  parseJobId,
  parsePageSize,
  parseTextLayer,
  parseTile,
  skippedLabels,
} from "./ipc";

function tileBuffer(
  width: number,
  height: number,
  skipped: number,
  pixels: number,
  repair = 0,
): ArrayBuffer {
  const buffer = new ArrayBuffer(TILE_HEADER + pixels);
  const view = new DataView(buffer);
  view.setUint16(0, width, true);
  view.setUint16(2, height, true);
  view.setUint16(4, skipped, true);
  view.setUint8(6, repair);
  return buffer;
}

describe("IPC payload validation", () => {
  it("accepts well-formed job events", () => {
    expect(parseJobEvent({ jobId: 2, outcome: { kind: "done", units: 5 } })).toEqual({
      jobId: 2,
      outcome: { kind: "done", units: 5 },
    });
    expect(parseJobEvent({ jobId: 3, outcome: { kind: "failed", reason: "x" } })).toEqual({
      jobId: 3,
      outcome: { kind: "failed", reason: "x" },
    });
  });

  it("rejects malformed job events", () => {
    for (const payload of [
      null,
      "done",
      { jobId: -1, outcome: { kind: "cancelled" } },
      { jobId: 1.5, outcome: { kind: "cancelled" } },
      { jobId: 1, outcome: { kind: "done" } },
      { jobId: 1, outcome: { kind: "done", units: Number.MAX_SAFE_INTEGER + 1 } },
      { jobId: 1, outcome: { kind: "failed", reason: 4 } },
      { jobId: 1, outcome: { kind: "unknown" } },
    ]) {
      expect(parseJobEvent(payload)).toBeNull();
    }
  });

  it("describes serialized shell errors instead of showing objects", () => {
    expect(describeCommandError({ code: "workerUnavailable" })).toBe("worker indisponible");
    expect(describeCommandError({ code: "invalidArgument", field: "units" })).toBe(
      "argument invalide (units)",
    );
    expect(describeCommandError({ code: "invalidArgument", field: 3 })).toBe(
      "erreur inattendue du shell",
    );
    expect(describeCommandError("boom")).toBe("erreur inattendue du shell");
    expect(describeCommandError(new Error("x"))).toBe("x");
  });

  it("validates job ids returned by the shell", () => {
    expect(parseJobId(7)).toBe(7);
    expect(() => parseJobId("7")).toThrow();
    expect(() => parseJobId(-1)).toThrow();
  });
});

describe("documents and tiles", () => {
  it("accepts tiles whose size matches the request, without copying", () => {
    const buffer = tileBuffer(2, 3, 1, 2 * 3 * 4);
    const tile = parseTile(buffer, 2, 3);
    expect([tile.width, tile.height, tile.skipped, tile.repair]).toEqual([2, 3, 1, "none"]);
    expect(parseTile(tileBuffer(2, 3, 0, 24, 2), 2, 3).repair).toBe("incomplete");
    expect(tile.pixels.length).toBe(24);
    expect(tile.pixels.buffer).toBe(buffer);
  });

  it("rejects tiles of another size or truncated", () => {
    for (const [buffer, width, height] of [
      [tileBuffer(2, 3, 0, 24), 3, 2],
      [tileBuffer(2, 3, 0, 23), 2, 3],
      [tileBuffer(2, 3, 0, 25), 2, 3],
      [tileBuffer(2, 3, 0, 24, 3), 2, 3],
      [new ArrayBuffer(4), 2, 3],
      ["tile", 2, 3],
      [new Uint8Array(32), 2, 3],
    ] as const) {
      expect(() => parseTile(buffer, width, height)).toThrow("invalid tile");
    }
  });

  it("validates document events and names their failures", () => {
    expect(
      parseDocumentEvent({
        sequence: 3,
        kind: "opened",
        document: 1,
        pages: 2,
        name: "a.pdf",
        repair: "repaired",
      }),
    ).toEqual({
      sequence: 3,
      kind: "opened",
      document: 1,
      pages: 2,
      name: "a.pdf",
      repair: "repaired",
    });
    expect(
      parseDocumentEvent({
        sequence: 4,
        kind: "failed",
        name: "a.pdf",
        error: { code: "failed", reason: "encrypted" },
      }),
    ).toEqual({
      sequence: 4,
      kind: "failed",
      name: "a.pdf",
      message: "document chiffré, non pris en charge pour l'instant",
    });
    for (const payload of [
      null,
      { sequence: 1, kind: "opened", document: -1, pages: 2, name: "a.pdf" },
      { sequence: 1, kind: "opened", document: 1, pages: 2 ** 32, name: "a.pdf" },
      { sequence: 1, kind: "opened", document: 1, pages: 2, name: 4, repair: "none" },
      { sequence: 1, kind: "opened", document: 1, pages: 2, name: "a.pdf" },
      { sequence: 1, kind: "opened", document: 1, pages: 2, name: "a.pdf", repair: "fixed" },
      { sequence: 1, kind: "closed", name: "a.pdf" },
      { kind: "opening", name: "a.pdf" },
      { sequence: -1, kind: "opening", name: "a.pdf" },
    ]) {
      expect(parseDocumentEvent(payload)).toBeNull();
    }
  });

  it("validates page sizes", () => {
    expect(parsePageSize({ width: 612, height: 792 })).toEqual({ width: 612, height: 792 });
    for (const value of [null, { width: 0, height: 1 }, { width: 1.5, height: 1 }, { width: 1 }]) {
      expect(() => parsePageSize(value)).toThrow();
    }
  });

  it("describes worker failures and tells dropped tiles apart", () => {
    expect(describeCommandError({ code: "failed", reason: "malformed" })).toBe(
      "fichier PDF invalide ou endommagé",
    );
    expect(describeCommandError({ code: "failed", reason: "new" })).toBe("échec (new)");
    expect(describeCommandError({ code: "timeout" })).toBe("délai dépassé");
    expect(isStale(new ShellError({ code: "stale" }))).toBe(true);
    expect(isStale(new ShellError({ code: "busy" }))).toBe(false);
    expect(isStale(new Error("stale"))).toBe(false);
  });

  it("labels skipped capabilities", () => {
    expect(skippedLabels(0)).toEqual([]);
    expect(skippedLabels(1 | (1 << 2) | (1 << 15))).toEqual(["texte", "images", "autres éléments"]);
  });

  it("checks text layers and keeps their known fields only", () => {
    const word = { text: "Café", x0: 1, y0: 2.5, x1: 30, y1: 2.5, size: 12 };
    expect(
      parseTextLayer({
        lines: [[{ ...word, extra: "dropped" }], []],
        truncated: false,
        incomplete: true,
      }),
    ).toEqual({ lines: [[word], []], truncated: false, incomplete: true });
    for (const value of [
      null,
      { lines: [], truncated: false },
      { lines: {}, truncated: false, incomplete: false },
      { lines: [[{ ...word, x1: Number.NaN }]], truncated: false, incomplete: false },
      { lines: [[{ ...word, y0: Infinity }]], truncated: false, incomplete: false },
      { lines: [[{ ...word, size: -1 }]], truncated: false, incomplete: false },
      { lines: [[{ ...word, text: 5 }]], truncated: false, incomplete: false },
      { lines: [word], truncated: false, incomplete: false },
      { lines: [], truncated: "no", incomplete: false },
    ]) {
      expect(() => parseTextLayer(value)).toThrow();
    }
    // Beyond the bound of the shell's response: refused.
    const many = Array.from({ length: MAX_LAYER_WORDS + 1 }, () => word);
    expect(() => parseTextLayer({ lines: [many], truncated: false, incomplete: false })).toThrow();
  });
});
