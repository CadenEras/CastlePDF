import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { App } from "./App";
import type { DocumentEvent, JobEvent, PageSize } from "./ipc";

// Mocks the IPC layer only: this does not prove the Tauri binary works.
const ipc = vi.hoisted(() => ({
  handler: null as ((event: JobEvent) => void) | null,
  documents: null as ((event: DocumentEvent) => void) | null,
  currentDocument: vi.fn<() => Promise<DocumentEvent | null>>(),
  pageSize: vi.fn<(document: number, page: number, scale: number) => Promise<PageSize>>(),
  runSyntheticJob: vi.fn<(units: number, delay: number) => Promise<number>>(),
  cancelJob: vi.fn<(jobId: number) => Promise<void>>(),
  unlisten: vi.fn(),
}));

vi.mock("./ipc", async (original) => ({
  ...(await original<typeof import("./ipc")>()),
  pageSize: ipc.pageSize,
  currentDocument: ipc.currentDocument,
  beginView: () => Promise.resolve(1),
  onDocumentEvent: (handler: (event: DocumentEvent) => void) => {
    ipc.documents = handler;
    return Promise.resolve(ipc.unlisten);
  },
  runSyntheticJob: ipc.runSyntheticJob,
  cancelJob: ipc.cancelJob,
  onJobEvent: (handler: (event: JobEvent) => void) => {
    ipc.handler = handler;
    return Promise.resolve(ipc.unlisten);
  },
}));

/** The worker diagnostic status (the viewer has its own). */
function status(): string {
  return screen.getAllByRole("status").at(-1)?.textContent ?? "";
}

function open(event: DocumentEvent) {
  act(() => {
    ipc.documents?.(event);
  });
}

function emit(event: JobEvent) {
  act(() => {
    ipc.handler?.(event);
  });
}

async function startJob(jobId: number) {
  ipc.runSyntheticJob.mockResolvedValueOnce(jobId);
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Lancer une tâche de test" }));
    await Promise.resolve();
  });
}

async function renderApp() {
  const view = render(<App />);
  // Let the event subscription resolve: starting is disabled until then.
  await act(async () => {
    await Promise.resolve();
  });
  return view;
}

beforeEach(() => {
  ipc.handler = null;
  ipc.documents = null;
  // Never resolves: the viewer stays in preparation, which these tests ignore.
  ipc.pageSize.mockReturnValue(new Promise(() => undefined));
  ipc.currentDocument.mockResolvedValue(null);
  vi.clearAllMocks();
  ipc.cancelJob.mockResolvedValue(undefined);
});

afterEach(() => {
  cleanup();
});

describe("App", () => {
  it("runs a job and shows its result", async () => {
    await renderApp();
    expect(status()).toBe("Worker prêt.");
    await startJob(1);
    expect(status()).toBe("Tâche en cours…");
    emit({ jobId: 1, outcome: { kind: "done", units: 300 } });
    expect(status()).toBe("Tâche terminée (300 unités).");
  });

  it("cancels the running job", async () => {
    await renderApp();
    await startJob(4);
    fireEvent.click(screen.getByRole("button", { name: "Annuler" }));
    expect(ipc.cancelJob).toHaveBeenCalledWith(4);
    expect(status()).toBe("Annulation en cours…");
    emit({ jobId: 4, outcome: { kind: "cancelled" } });
    expect(status()).toBe("Tâche annulée.");
  });

  it("ignores results of older jobs", async () => {
    await renderApp();
    await startJob(1);
    emit({ jobId: 1, outcome: { kind: "cancelled" } });
    await startJob(2);
    emit({ jobId: 1, outcome: { kind: "done", units: 3 } });
    expect(status()).toBe("Tâche en cours…");
  });

  it("reports a shell error", async () => {
    await renderApp();
    ipc.runSyntheticJob.mockRejectedValueOnce(new Error("worker unavailable"));
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Lancer une tâche de test" }));
      await Promise.resolve();
    });
    expect(status()).toBe("Erreur : worker unavailable");
  });

  it("keeps a result that arrives before the job id", async () => {
    await renderApp();
    let resolve: (jobId: number) => void = () => undefined;
    ipc.runSyntheticJob.mockReturnValueOnce(
      new Promise<number>((done) => {
        resolve = done;
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Lancer une tâche de test" }));
    emit({ jobId: 9, outcome: { kind: "failed", reason: "worker unavailable" } });
    await act(async () => {
      resolve(9);
      await Promise.resolve();
    });
    expect(status()).toBe("Échec du worker : worker unavailable");
  });

  it("enables starting only once events are being received", async () => {
    render(<App />);
    const start = screen.getByRole("button", { name: "Lancer une tâche de test" });
    expect((start as HTMLButtonElement).disabled).toBe(true);
    await act(async () => {
      await Promise.resolve();
    });
    expect((start as HTMLButtonElement).disabled).toBe(false);
  });

  it("stops listening on unmount", async () => {
    const { unmount } = await renderApp();
    unmount();
    // Job events and document events.
    expect(ipc.unlisten).toHaveBeenCalledTimes(2);
  });

  it("invites to drop a file, then shows the document the shell opened", async () => {
    await renderApp();
    expect(screen.getByText("Déposez un fichier PDF sur la fenêtre pour l'ouvrir.")).toBeTruthy();
    open({ sequence: 1, kind: "opening", name: "plan.pdf" });
    expect(screen.getByText("Ouverture de plan.pdf…")).toBeTruthy();
    open({ sequence: 2, kind: "opened", document: 1, pages: 4, name: "plan.pdf" });
    expect(screen.getByRole("img", { name: "Page 1 de plan.pdf" })).toBeTruthy();
    await act(async () => {
      await Promise.resolve();
    });
    expect(ipc.pageSize).toHaveBeenCalledWith(1, 0, 0.25);
    // A late copy of an older event changes nothing.
    open({ sequence: 1, kind: "opening", name: "plan.pdf" });
    expect(screen.queryByText("Ouverture de plan.pdf…")).toBeNull();
  });

  it("shows a document the shell opened before the page subscribed", async () => {
    ipc.currentDocument.mockResolvedValueOnce({
      sequence: 2,
      kind: "opened",
      document: 1,
      pages: 3,
      name: "shapes.pdf",
    });
    await renderApp();
    await act(async () => {
      await Promise.resolve();
    });
    expect(screen.getByRole("img", { name: "Page 1 de shapes.pdf" })).toBeTruthy();
  });

  it("keeps the document shown when another file cannot be opened", async () => {
    await renderApp();
    open({ sequence: 1, kind: "opened", document: 1, pages: 4, name: "plan.pdf" });
    open({
      sequence: 2,
      kind: "failed",
      name: "secret.pdf",
      message: "document chiffré, non pris en charge pour l'instant",
    });
    expect(screen.getByRole("alert").textContent).toBe(
      "Impossible d'ouvrir secret.pdf : document chiffré, non pris en charge pour l'instant.",
    );
    expect(screen.getByRole("img", { name: "Page 1 de plan.pdf" })).toBeTruthy();
  });
});
