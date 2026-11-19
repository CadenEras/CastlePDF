import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Saved } from "./ipc";
import { PagePanel, ROW_HEIGHT, visibleRows } from "./PagePanel";

// Mocks the IPC layer only: this does not prove the Tauri binary works.
const ipc = vi.hoisted(() => ({
  saveAs: vi.fn<(document: number, plan: readonly { page: number; rotation: number }[]) => Promise<Saved | null>>(),
}));

vi.mock("./ipc", async (original) => ({
  ...(await original<typeof import("./ipc")>()),
  saveAs: ipc.saveAs,
}));

const info = { document: 7, pages: 3, name: "doc.pdf", repair: "none" as const };

function rows(): string[] {
  return screen.getAllByRole("option").map((option) => option.textContent);
}

function selected(): string {
  return screen.getByRole("option", { selected: true }).textContent;
}

function press(key: string, modifiers: Partial<Record<"ctrlKey" | "shiftKey" | "altKey", boolean>> = {}) {
  fireEvent.keyDown(screen.getByRole("listbox"), { key, ...modifiers });
}

beforeEach(() => {
  ipc.saveAs.mockReset();
});

afterEach(() => {
  cleanup();
});

describe("page panel", () => {
  it("edits the plan from the keyboard, with undo and redo", () => {
    render(<PagePanel info={info} />);
    expect(rows()).toEqual(["Page 1", "Page 2", "Page 3"]);
    press("ArrowDown");
    expect(selected()).toBe("Page 2");
    press("r");
    expect(rows()[1]).toBe("Page 2, tournée de 90°");
    press("R", { shiftKey: true });
    press("R", { shiftKey: true });
    expect(rows()[1]).toBe("Page 2, tournée de 270°");
    press("ArrowUp", { altKey: true });
    expect(rows()).toEqual(["Page 2, tournée de 270°", "Page 1", "Page 3"]);
    expect(selected()).toBe("Page 2, tournée de 270°");
    press("End");
    press("Delete");
    expect(rows()).toEqual(["Page 2, tournée de 270°", "Page 1"]);
    expect(screen.getByRole("heading").textContent).toBe("Pages à enregistrer (2 sur 3)");
    press("z", { ctrlKey: true });
    expect(rows()).toHaveLength(3);
    press("z", { ctrlKey: true, shiftKey: true });
    expect(rows()).toHaveLength(2);
    press("y", { ctrlKey: true });
    expect(rows()).toHaveLength(2);
  });

  it("never removes the last page", () => {
    render(<PagePanel info={{ ...info, pages: 1 }} />);
    press("Delete");
    expect(rows()).toEqual(["Page 1"]);
    expect(screen.getByRole("button", { name: "Retirer" })).toHaveProperty("disabled", true);
  });

  it("saves the plan and shows what was not kept", async () => {
    ipc.saveAs.mockResolvedValue({
      pages: 2,
      bytes: 3 * 1024 * 1024,
      dropped: ["Outlines", "AcroForm"],
      keptWhole: [0],
    });
    render(<PagePanel info={info} />);
    fireEvent.click(screen.getByRole("button", { name: "Retirer" }));
    fireEvent.click(screen.getByRole("button", { name: "Pivoter à droite" }));
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Enregistrer sous…" }));
      await Promise.resolve();
    });
    expect(ipc.saveAs).toHaveBeenCalledWith(7, [
      { page: 1, rotation: 90 },
      { page: 2, rotation: 0 },
    ]);
    const status = screen.getByRole("status").textContent;
    expect(status).toContain("Enregistré : 2 pages, 3,0 Mio.");
    expect(status).toContain("Non conservé : Outlines, AcroForm.");
    expect(status).toContain("pages 1 :");
  });

  it("says nothing when the dialog is cancelled, and reports a refusal", async () => {
    ipc.saveAs.mockResolvedValueOnce(null);
    render(<PagePanel info={info} />);
    const save = screen.getByRole("button", { name: "Enregistrer sous…" });
    await act(async () => {
      fireEvent.click(save);
      await Promise.resolve();
    });
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
    ipc.saveAs.mockRejectedValueOnce(new Error("le fichier du document ouvert ne peut pas être remplacé"));
    await act(async () => {
      fireEvent.click(save);
      await Promise.resolve();
    });
    expect(screen.getByRole("alert").textContent).toBe(
      "Échec de l'enregistrement : le fichier du document ouvert ne peut pas être remplacé.",
    );
  });

  it("renders only the rows in sight of a long plan", () => {
    render(<PagePanel info={{ ...info, pages: 50_000 }} />);
    expect(rows().length).toBeLessThan(40);
    const list = screen.getByRole("listbox");
    act(() => {
      list.scrollTop = 25_000 * ROW_HEIGHT;
      fireEvent.scroll(list);
    });
    expect(rows()).toContain("Page 25001");
  });
});

describe("visible rows", () => {
  it("covers the view and some rows around it, within the plan", () => {
    expect(visibleRows(10, 0, 100)).toEqual({ first: 0, last: 10 });
    expect(visibleRows(1000, 100 * ROW_HEIGHT, 10 * ROW_HEIGHT)).toEqual({ first: 92, last: 118 });
    expect(visibleRows(0, 0, 100)).toEqual({ first: 0, last: 0 });
  });
});
