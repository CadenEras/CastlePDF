import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { PageSize, Tile, TileRequest } from "./ipc";
import { MAX_CANVAS_PIXELS, fitScale, tileGrid, Viewer } from "./Viewer";

// Mocks the shell commands only: this does not prove the Tauri binary works.
const ipc = vi.hoisted(() => ({
  generation: 0,
  beginView: vi.fn<() => Promise<number>>(),
  pageSize: vi.fn<(document: number, page: number, scale: number) => Promise<PageSize>>(),
  renderTile: vi.fn<(request: TileRequest) => Promise<Tile>>(),
}));

vi.mock("./ipc", async (original) => ({
  ...(await original<typeof import("./ipc")>()),
  beginView: ipc.beginView,
  pageSize: ipc.pageSize,
  renderTile: ipc.renderTile,
}));

const drawn: [number, number, number, number][] = [];

class FakeImageData {
  constructor(
    readonly data: Uint8ClampedArray,
    readonly width: number,
    readonly height: number,
  ) {}
}

function tileFor(request: TileRequest, skipped = 0): Tile {
  return {
    width: request.width,
    height: request.height,
    skipped,
    pixels: new Uint8ClampedArray(request.width * request.height * 4),
  };
}

/** Lets pending promises and the effects they trigger run. */
async function settle() {
  for (let i = 0; i < 20; i += 1) {
    await act(async () => {
      await Promise.resolve();
    });
  }
}

function status(): string {
  return screen.getByRole("status").textContent;
}

const INFO = { document: 3, pages: 2, name: "plan.pdf" };

beforeEach(() => {
  vi.clearAllMocks();
  drawn.length = 0;
  vi.stubGlobal("ImageData", FakeImageData);
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({
    putImageData: (image: FakeImageData, x: number, y: number) => {
      drawn.push([x, y, image.width, image.height]);
    },
  } as unknown as CanvasRenderingContext2D);
  // 400 x 300 points; at the default 800 CSS pixels, scale 2: 800 x 600.
  ipc.pageSize.mockImplementation((_document, _page, scale) =>
    Promise.resolve({ width: Math.ceil(400 * scale), height: Math.ceil(300 * scale) }),
  );
  ipc.renderTile.mockImplementation((request) => Promise.resolve(tileFor(request)));
  // The shell numbers views.
  ipc.beginView.mockImplementation(() => {
    ipc.generation += 1;
    return Promise.resolve(ipc.generation);
  });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("tile layout", () => {
  it("covers the page row by row with edge tiles cut", () => {
    expect(tileGrid(300, 260)).toEqual([
      { x: 0, y: 0, width: 256, height: 256 },
      { x: 256, y: 0, width: 44, height: 256 },
      { x: 0, y: 256, width: 256, height: 4 },
      { x: 256, y: 256, width: 44, height: 4 },
    ]);
    expect(tileGrid(256, 256)).toHaveLength(1);
  });

  it("fits the width in device pixels within bounds", () => {
    expect(fitScale(800, 400, 300, 1)).toBe(2);
    expect(fitScale(800, 400, 300, 1.5)).toBe(3);
    expect(fitScale(10, 100_000, 10, 1)).toBe(0.05);
    expect(fitScale(100_000, 10, 10, 1)).toBe(16);
  });

  it("keeps a tall page within the canvas pixel limit", () => {
    // 200 x 14 400 points on a 4K screen: the width alone would ask for scale 19.
    const scale = fitScale(1920, 200, 14_400, 2);
    expect(200 * scale * 14_400 * scale).toBeLessThanOrEqual(MAX_CANVAS_PIXELS + 1);
    expect(scale).toBeCloseTo(2.41, 2);
  });
});

describe("Viewer", () => {
  it("draws every tile of the page at its place", async () => {
    render(<Viewer info={INFO} />);
    await settle();
    expect(ipc.pageSize).toHaveBeenCalledWith(3, 0, 0.25);
    expect(ipc.pageSize).toHaveBeenCalledWith(3, 0, 2);
    // 800 x 600: 4 columns, 3 rows.
    expect(ipc.renderTile).toHaveBeenCalledTimes(12);
    expect(drawn).toContainEqual([768, 512, 32, 88]);
    expect(drawn).toHaveLength(12);
    expect(status()).toBe("Page 1 sur 2.");
    const canvas = screen.getByRole<HTMLCanvasElement>("img", { name: "Page 1 de plan.pdf" });
    expect([canvas.width, canvas.height]).toEqual([800, 600]);
    const generations = new Set(ipc.renderTile.mock.calls.map(([request]) => request.generation));
    expect(generations.size).toBe(1);
  });

  it("says what the page uses but cannot show", async () => {
    ipc.renderTile.mockImplementation((request) =>
      Promise.resolve(tileFor(request, request.x === 0 ? 1 : 1 << 3)),
    );
    render(<Viewer info={INFO} />);
    await settle();
    expect(screen.getByRole("note").textContent).toBe(
      "Rendu partiel : non affichés pour l'instant — texte, images et formulaires (XObjects).",
    );
  });

  it("moves to the next page with a newer generation", async () => {
    render(<Viewer info={INFO} />);
    await settle();
    const first = ipc.renderTile.mock.calls[0]?.[0].generation ?? 0;
    fireEvent.click(screen.getByRole("button", { name: "Page suivante" }));
    await settle();
    const last = ipc.renderTile.mock.calls.at(-1)?.[0];
    expect(last?.page).toBe(1);
    expect(last?.generation).toBeGreaterThan(first);
    expect(status()).toBe("Page 2 sur 2.");
    expect(screen.getByRole<HTMLButtonElement>("button", { name: "Page suivante" }).disabled).toBe(
      true,
    );
  });

  it("reports a failure but not a dropped tile", async () => {
    const { ShellError } = await import("./ipc");
    ipc.renderTile.mockRejectedValueOnce(new ShellError({ code: "stale" }));
    render(<Viewer info={INFO} />);
    await settle();
    // Dropped because the view changed: not shown as an error.
    expect(status()).not.toContain("échec");
    cleanup();
    ipc.pageSize.mockRejectedValueOnce(
      new ShellError({ code: "failed", reason: "malformed" }),
    );
    render(<Viewer info={INFO} />);
    await settle();
    expect(status()).toBe("Page 1 sur 2 : échec du rendu (fichier PDF invalide ou endommagé).");
  });

  it("stops at the first failed tile and keeps the error shown", async () => {
    const { ShellError } = await import("./ipc");
    ipc.renderTile.mockImplementation((request) =>
      request.x === 256 && request.y === 256
        ? Promise.reject(new ShellError({ code: "failed", reason: "limitExceeded" }))
        : Promise.resolve(tileFor(request)),
    );
    render(<Viewer info={INFO} />);
    await settle();
    expect(status()).toBe("Page 1 sur 2 : échec du rendu (limite de ressources atteinte).");
    // Tiles after the failure are not requested: 12 in all otherwise.
    expect(ipc.renderTile.mock.calls.length).toBeLessThan(12);
  });

  it("reports a missing canvas instead of an empty page", async () => {
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
    render(<Viewer info={INFO} />);
    await settle();
    expect(status()).toBe("Page 1 sur 2 : échec du rendu (affichage indisponible (canvas)).");
    expect(ipc.renderTile).not.toHaveBeenCalled();
  });

  it("ignores the results of a view it left", async () => {
    let release: () => void = () => undefined;
    ipc.renderTile.mockImplementationOnce(
      (request) =>
        new Promise((done) => {
          release = () => {
            done(tileFor(request));
          };
        }),
    );
    const { unmount } = render(<Viewer info={INFO} />);
    await settle();
    unmount();
    const before = drawn.length;
    await act(async () => {
      release();
      await Promise.resolve();
    });
    expect(drawn.length).toBe(before);
  });
});
