import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  PageSize,
  RepairState,
  TextLayer,
  Tile,
  TileRequest,
} from "./ipc";
import {
  MAX_CANVAS_PIXELS,
  fitScale,
  pageOffsets,
  pagesBetween,
  tileGrid,
  Viewer,
} from "./Viewer";

// Mocks the shell commands only: this does not prove the Tauri binary works.
const ipc = vi.hoisted(() => ({
  generation: 0,
  beginView: vi.fn<() => Promise<number>>(),
  pageSize:
    vi.fn<
      (document: number, page: number, scale: number) => Promise<PageSize>
    >(),
  renderTile: vi.fn<(request: TileRequest) => Promise<Tile>>(),
  pageText:
    vi.fn<
      (document: number, page: number, generation: number) => Promise<TextLayer>
    >(),
}));

vi.mock("./ipc", async (original) => ({
  ...(await original<typeof import("./ipc")>()),
  beginView: ipc.beginView,
  pageSize: ipc.pageSize,
  renderTile: ipc.renderTile,
  pageText: ipc.pageText,
}));

/** Tiles drawn: page canvas label, x, y, width, height. */
const drawn: [string, number, number, number, number][] = [];

class FakeImageData {
  constructor(
    readonly data: Uint8ClampedArray,
    readonly width: number,
    readonly height: number,
  ) {}
}

function tileFor(
  request: TileRequest,
  skipped = 0,
  repair: RepairState = "none",
): Tile {
  return {
    width: request.width,
    height: request.height,
    skipped,
    repair,
    pixels: new Uint8ClampedArray(request.width * request.height * 4),
  };
}

/** Lets pending promises and the effects they trigger run. */
async function settle() {
  for (let i = 0; i < 40; i += 1) {
    await act(async () => {
      await Promise.resolve();
    });
  }
}

function status(): string {
  return screen.getByRole("status").textContent;
}

function scrollTo(top: number) {
  const pages = screen.getByLabelText("Pages de plan.pdf");
  pages.scrollTop = top;
  fireEvent.scroll(pages);
}

function tilesOf(page: number): TileRequest[] {
  return ipc.renderTile.mock.calls
    .map(([request]) => request)
    .filter((r) => r.page === page);
}

// Three pages of 400 x 300 points. In an 800 x 600 view (the default before
// any measurement), each is drawn at scale 2: 800 x 600 pixels, 12 tiles,
// one under the other at 0, 616 and 1232 (16 apart).
const INFO = {
  document: 3,
  pages: 3,
  name: "plan.pdf",
  repair: "none",
} as const;

beforeEach(() => {
  vi.clearAllMocks();
  drawn.length = 0;
  vi.stubGlobal("ImageData", FakeImageData);
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation(
    function (this: HTMLCanvasElement) {
      const label = `Page ${String(Number(this.dataset.page) + 1)} de plan.pdf`;
      return {
        putImageData: (image: FakeImageData, x: number, y: number) => {
          drawn.push([label, x, y, image.width, image.height]);
        },
      } as unknown as CanvasRenderingContext2D;
    } as never,
  );
  ipc.pageSize.mockImplementation((_document, _page, scale) =>
    Promise.resolve({
      width: Math.ceil(400 * scale),
      height: Math.ceil(300 * scale),
    }),
  );
  ipc.renderTile.mockImplementation((request) =>
    Promise.resolve(tileFor(request)),
  );
  ipc.pageText.mockResolvedValue({
    lines: [],
    truncated: false,
    incomplete: false,
  });
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

describe("layout", () => {
  it("covers a page with tiles row by row, edge tiles cut", () => {
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
    expect(200 * scale * 14_400 * scale).toBeLessThanOrEqual(
      MAX_CANVAS_PIXELS + 1,
    );
    expect(scale).toBeCloseTo(2.41, 2);
  });

  it("stacks pages and finds those in a band", () => {
    const heights = [600, 600, 300];
    const { tops, total } = pageOffsets(heights);
    expect(tops).toEqual([0, 616, 1232]);
    expect(total).toBe(1532);
    expect(pagesBetween(tops, heights, 0, 600)).toEqual([0, 0]);
    expect(pagesBetween(tops, heights, 500, 700)).toEqual([0, 1]);
    // In the gap between two pages: the next one.
    expect(pagesBetween(tops, heights, 605, 610)).toEqual([1, 1]);
    // Beyond the end: the last one.
    expect(pagesBetween(tops, heights, 5000, 5600)).toEqual([2, 2]);
    expect(pagesBetween([], [], 0, 10)).toEqual([0, 0]);
  });
});

describe("Viewer", () => {
  it("draws the tiles of the pages in view only", async () => {
    render(<Viewer info={INFO} />);
    await settle();
    // Page 1 is in view; page 2 is kept measured (and given a canvas) next
    // to it, page 3 is neither.
    expect(ipc.pageSize).toHaveBeenCalledWith(3, 0, 0.25);
    expect(ipc.pageSize).toHaveBeenCalledWith(3, 0, 2);
    expect(ipc.pageSize).toHaveBeenCalledWith(3, 1, 2);
    expect(ipc.pageSize).not.toHaveBeenCalledWith(3, 2, 2);
    expect(tilesOf(0)).toHaveLength(12);
    expect(tilesOf(1)).toHaveLength(0);
    expect(drawn).toContainEqual(["Page 1 de plan.pdf", 768, 512, 32, 88]);
    expect(status()).toBe("Page 1 sur 3.");
    const canvas = screen
      .getByRole("group", { name: "Page 1 de plan.pdf" })
      .querySelector("canvas");
    expect([canvas?.width, canvas?.height]).toEqual([800, 600]);
    expect(
      screen.queryByRole("group", { name: "Page 3 de plan.pdf" }),
    ).toBeNull();
  });

  it("draws the pages scrolled to, in a newer view, and releases the others", async () => {
    render(<Viewer info={INFO} />);
    await settle();
    const first = tilesOf(0)[0]?.generation ?? 0;
    scrollTo(1300);
    await settle();
    // Page 3 in view; page 1 is now too far to keep a canvas.
    const third = tilesOf(2);
    expect(third).toHaveLength(12);
    expect(third[0]?.generation).toBeGreaterThan(first);
    expect(status()).toBe("Page 3 sur 3.");
    expect(
      screen.queryByRole("group", { name: "Page 1 de plan.pdf" }),
    ).toBeNull();
    // Back to page 1: its canvas is new, so it is drawn again.
    scrollTo(0);
    await settle();
    expect(tilesOf(0)).toHaveLength(24);
  });

  it("does not ask again for tiles already drawn on a page still shown", async () => {
    render(<Viewer info={INFO} />);
    await settle();
    // Between pages 1 and 2: both in view, page 1 already drawn.
    scrollTo(300);
    await settle();
    expect(tilesOf(0)).toHaveLength(12);
    expect(tilesOf(1)).toHaveLength(12);
  });

  it("moves page by page with the buttons", async () => {
    render(<Viewer info={INFO} />);
    await settle();
    fireEvent.click(screen.getByRole("button", { name: "Page suivante" }));
    await settle();
    expect(
      screen.getByLabelText<HTMLDivElement>("Pages de plan.pdf").scrollTop,
    ).toBe(616);
    expect(status()).toBe("Page 2 sur 3.");
    fireEvent.click(screen.getByRole("button", { name: "Page précédente" }));
    await settle();
    expect(status()).toBe("Page 1 sur 3.");
    expect(
      screen.getByRole<HTMLButtonElement>("button", { name: "Page précédente" })
        .disabled,
    ).toBe(true);
  });

  it("says what the pages use but cannot show", async () => {
    ipc.renderTile.mockImplementation((request) =>
      Promise.resolve(tileFor(request, request.x === 0 ? 1 : 1 << 3)),
    );
    render(<Viewer info={INFO} />);
    await settle();
    expect(
      screen.getByRole("note", { name: "Rendu partiel" }).textContent,
    ).toBe(
      "Rendu partiel : non affichés pour l'instant — texte, formulaires et autres XObjects.",
    );
  });

  it("says when fonts are replaced, apart from what is missing", async () => {
    ipc.renderTile.mockImplementation((request) =>
      Promise.resolve(tileFor(request, 1 << 12)),
    );
    render(<Viewer info={INFO} />);
    await settle();
    expect(
      screen.getByRole("note", { name: "Polices remplacées" }).textContent,
    ).toContain("police approchante");
    expect(screen.queryByRole("note", { name: "Rendu partiel" })).toBeNull();
  });

  it("says when the document was repaired", async () => {
    render(<Viewer info={{ ...INFO, repair: "repaired" }} />);
    // Known from the opening: shown before any tile arrives.
    expect(
      screen.getByRole("note", { name: "Document réparé" }).textContent,
    ).toBe(
      "Document réparé : sa structure était endommagée et a été reconstruite. " +
        "L'affichage peut différer d'autres lecteurs.",
    );
    await settle();
    expect(screen.queryByRole("note", { name: "Rendu partiel" })).toBeNull();
  });

  it("learns from a tile that objects were found by a repair", async () => {
    ipc.renderTile.mockImplementation((request) =>
      Promise.resolve(
        tileFor(request, 0, request.x === 256 ? "incomplete" : "none"),
      ),
    );
    render(<Viewer info={INFO} />);
    expect(screen.queryByRole("note", { name: "Document réparé" })).toBeNull();
    await settle();
    expect(
      screen.getByRole("note", { name: "Document réparé" }).textContent,
    ).toContain("des éléments peuvent manquer");
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
    expect(status()).toBe(
      "Page 1 sur 3 : échec du rendu (fichier PDF invalide ou endommagé).",
    );
  });

  it("stops at the first failed tile and keeps the error shown", async () => {
    const { ShellError } = await import("./ipc");
    ipc.renderTile.mockImplementation((request) =>
      request.x === 256 && request.y === 256
        ? Promise.reject(
            new ShellError({ code: "failed", reason: "limitExceeded" }),
          )
        : Promise.resolve(tileFor(request)),
    );
    render(<Viewer info={INFO} />);
    await settle();
    expect(status()).toBe(
      "Page 1 sur 3 : échec du rendu (limite de ressources atteinte).",
    );
    // Tiles after the failure are not requested: 12 in all otherwise.
    expect(ipc.renderTile.mock.calls.length).toBeLessThan(12);
  });

  it("reports a missing canvas instead of an empty page", async () => {
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
    render(<Viewer info={INFO} />);
    await settle();
    expect(status()).toBe(
      "Page 1 sur 3 : échec du rendu (affichage indisponible (canvas)).",
    );
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

  it("lays the text of the pages in view over them, after their tiles", async () => {
    let tilesWhenText = -1;
    ipc.pageText.mockImplementation((_document, page) => {
      tilesWhenText = ipc.renderTile.mock.calls.length;
      return Promise.resolve({
        lines: [[{ text: `page${String(page + 1)}`, x0: 10, y0: 20, x1: 60, y1: 20, size: 12 }]],
        truncated: false,
        incomplete: false,
      });
    });
    render(<Viewer info={INFO} />);
    await settle();
    // Page 1 in view: its 12 tiles first, then its text, for the same view.
    expect(tilesWhenText).toBe(12);
    expect(ipc.pageText).toHaveBeenCalledWith(3, 0, ipc.generation);
    const text = screen.getByLabelText("Texte de la page 1");
    expect(text.textContent).toContain("page1");
    // Scale 2 at a pixel ratio of 1: twice the page pixels at scale 1.
    expect(text.querySelector<HTMLElement>("[data-word]")?.style.left).toBe("20px");
    expect(screen.queryByRole("note", { name: "Texte partiel" })).toBeNull();
  });

  it("says when text is missing, and keeps the page shown when text fails", async () => {
    ipc.pageText.mockResolvedValueOnce({ lines: [], truncated: false, incomplete: true });
    render(<Viewer info={INFO} />);
    await settle();
    expect(screen.getByRole("note", { name: "Texte partiel" })).toBeTruthy();
    cleanup();
    ipc.pageText.mockRejectedValue(new Error("délai dépassé"));
    render(<Viewer info={INFO} />);
    await settle();
    expect(
      screen.getByRole("note", { name: "Texte indisponible" }).textContent,
    ).toContain("délai dépassé");
    expect(status()).toBe("Page 1 sur 3.");
  });
});
