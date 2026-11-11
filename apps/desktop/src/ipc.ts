// Typed boundary with the Rust shell. Payloads are `unknown` until validated:
// a well-typed call here does not make the data trustworthy.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type JobOutcome =
  | { kind: "done"; units: number }
  | { kind: "cancelled" }
  | { kind: "limitExceeded" }
  | { kind: "busy" }
  | { kind: "failed"; reason: string };

export interface JobEvent {
  jobId: number;
  outcome: JobOutcome;
}

/**
 * Whether the structure of a document was reconstructed (ADR-0004), from the
 * least to the most serious: read as written, repaired, repaired with objects
 * possibly missing.
 */
export const REPAIR_STATES = ["none", "repaired", "incomplete"] as const;
export type RepairState = (typeof REPAIR_STATES)[number];

function isRepairState(value: unknown): value is RepairState {
  return REPAIR_STATES.some((state) => state === value);
}

/** The more serious of two repair states. */
export function worseRepair(a: RepairState, b: RepairState): RepairState {
  return REPAIR_STATES.indexOf(a) >= REPAIR_STATES.indexOf(b) ? a : b;
}

/** A document opened by the shell (dropped on the window): no path, a name. */
export interface DocumentInfo {
  document: number;
  pages: number;
  name: string;
  /** Repair state when it was opened. */
  repair: RepairState;
}

/**
 * What the shell announced about the document, ranked by `sequence`: an
 * event older than one already applied must be ignored.
 */
export type DocumentEvent = { sequence: number } & (
  | { kind: "opening"; name: string }
  | ({ kind: "opened" } & DocumentInfo)
  | { kind: "failed"; name: string; message: string }
);

export interface PageSize {
  width: number;
  height: number;
}

/** RGBA pixels with straight alpha, rows top to bottom (`ImageData` layout). */
export interface Tile {
  width: number;
  height: number;
  /** Capabilities the page uses but the renderer does not draw (`SKIPPED`). */
  skipped: number;
  /** Repair state of the document after this tile was rendered. */
  repair: RepairState;
  pixels: Uint8ClampedArray<ArrayBuffer>;
}

export const JOB_EVENT = "worker-job";
export const DOCUMENT_EVENT = "document";
export const MAX_TILE_SIDE = 256;
/** Bytes before the pixels of a tile: width, height, skipped, repair, padding. */
export const TILE_HEADER = 8;

/**
 * Bit of `Tile.skipped`: text drawn with a replacement font (the PDF does
 * not embed the font). Drawn, so not among the `SKIPPED` capabilities.
 */
export const FONT_SUBSTITUTED = 1 << 12;

/** Bits of `Tile.skipped` (the worker protocol's `Skipped`), with labels. */
export const SKIPPED: readonly (readonly [number, string])[] = [
  [1 << 0, "texte"],
  [1 << 1, "découpe par le texte"],
  [1 << 2, "images"],
  [1 << 3, "formulaires et autres XObjects"],
  [1 << 4, "dégradés"],
  [1 << 5, "motifs"],
  [1 << 6, "espaces de couleur"],
  [1 << 7, "modes de fusion et groupes à élimination"],
  [1 << 8, "masques adoucis"],
  [1 << 9, "transparence de forme"],
  [1 << 10, "fonctions de transfert"],
  [1 << 11, "contenu optionnel"],
  [1 << 15, "autres éléments"],
];

export function skippedLabels(bits: number): string[] {
  return SKIPPED.filter(([bit]) => (bits & bit) !== 0).map(([, label]) => label);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isCount(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isU32(value: unknown): value is number {
  return isCount(value) && value <= 0xffff_ffff;
}

export function parseJobId(value: unknown): number {
  if (!isCount(value)) {
    throw new Error("invalid job id from shell");
  }
  return value;
}

function parseOutcome(value: unknown): JobOutcome | null {
  if (!isRecord(value)) {
    return null;
  }
  switch (value.kind) {
    case "done":
      return isCount(value.units) ? { kind: "done", units: value.units } : null;
    case "cancelled":
    case "limitExceeded":
    case "busy":
      return { kind: value.kind };
    case "failed":
      return typeof value.reason === "string"
        ? { kind: "failed", reason: value.reason }
        : null;
    default:
      return null;
  }
}

export function parseJobEvent(value: unknown): JobEvent | null {
  if (!isRecord(value) || !isCount(value.jobId)) {
    return null;
  }
  const outcome = parseOutcome(value.outcome);
  return outcome === null ? null : { jobId: value.jobId, outcome };
}

const FAILURES: Record<string, string> = {
  encrypted: "document chiffré, non pris en charge pour l'instant",
  unsupported: "fonctionnalité PDF non prise en charge pour l'instant",
  malformed: "fichier PDF invalide ou endommagé",
  unreadable: "fichier illisible",
  limitExceeded: "limite de ressources atteinte",
  unsupportedPath: "chemin de fichier non pris en charge",
  tooManyDocuments: "trop de documents ouverts",
  unknownDocument: "document fermé",
  invalidRequest: "requête invalide",
  cancelled: "annulé",
  busy: "worker occupé",
  internal: "erreur interne du worker",
};

/** Turns a rejected command (a serialized Rust `CommandError`) into a message. */
export function describeCommandError(error: unknown): string {
  if (isRecord(error)) {
    switch (error.code) {
      case "workerUnavailable":
        return "worker indisponible";
      case "busy":
        return "trop de requêtes en attente";
      case "stale":
        return "requête dépassée";
      case "timeout":
        return "délai dépassé";
      case "invalidArgument":
        if (typeof error.field === "string") {
          return `argument invalide (${error.field})`;
        }
        break;
      case "failed":
        if (typeof error.reason === "string") {
          return FAILURES[error.reason] ?? `échec (${error.reason})`;
        }
        break;
    }
  }
  if (error instanceof Error) {
    return error.message;
  }
  return "erreur inattendue du shell";
}

/** A rejected command: `code` is the shell's error code, when it sent one. */
export class ShellError extends Error {
  readonly code: string | null;

  constructor(cause: unknown) {
    super(describeCommandError(cause), { cause });
    this.name = "ShellError";
    this.code = isRecord(cause) && typeof cause.code === "string" ? cause.code : null;
  }
}

/** True for a tile dropped because the view changed: not an error to show. */
export function isStale(error: unknown): boolean {
  return error instanceof ShellError && error.code === "stale";
}

export function parseDocumentEvent(value: unknown): DocumentEvent | null {
  if (!isRecord(value) || typeof value.name !== "string" || !isCount(value.sequence)) {
    return null;
  }
  const { sequence, name } = value;
  switch (value.kind) {
    case "opening":
      return { sequence, kind: "opening", name };
    case "opened":
      return isU32(value.document) && isU32(value.pages) && isRepairState(value.repair)
        ? {
            sequence,
            kind: "opened",
            document: value.document,
            pages: value.pages,
            name,
            repair: value.repair,
          }
        : null;
    case "failed":
      return { sequence, kind: "failed", name, message: describeCommandError(value.error) };
    default:
      return null;
  }
}

export function parsePageSize(value: unknown): PageSize {
  if (!isRecord(value) || !isU32(value.width) || !isU32(value.height)) {
    throw new Error("invalid page size from shell");
  }
  if (value.width === 0 || value.height === 0) {
    throw new Error("invalid page size from shell");
  }
  return { width: value.width, height: value.height };
}

/** Checks a tile against the size requested and copies nothing. */
export function parseTile(value: unknown, width: number, height: number): Tile {
  if (!(value instanceof ArrayBuffer) || value.byteLength < TILE_HEADER) {
    throw new Error("invalid tile from shell");
  }
  const header = new DataView(value, 0, TILE_HEADER);
  const tile = {
    width: header.getUint16(0, true),
    height: header.getUint16(2, true),
    skipped: header.getUint16(4, true),
  };
  const repair = REPAIR_STATES[header.getUint8(6)];
  if (
    repair === undefined ||
    tile.width !== width ||
    tile.height !== height ||
    value.byteLength !== TILE_HEADER + width * height * 4
  ) {
    throw new Error("invalid tile from shell");
  }
  return { ...tile, repair, pixels: new Uint8ClampedArray(value, TILE_HEADER) };
}

async function command(name: string, args: Record<string, unknown>): Promise<unknown> {
  try {
    return await invoke<unknown>(name, args);
  } catch (error: unknown) {
    throw new ShellError(error);
  }
}

export async function runSyntheticJob(
  units: number,
  unitDelayMs: number,
): Promise<number> {
  return parseJobId(await command("worker_run", { units, unitDelayMs }));
}

export async function cancelJob(jobId: number): Promise<void> {
  await command("worker_cancel", { jobId });
}

/** The last document event, for a page loaded after it was sent. */
export async function currentDocument(): Promise<DocumentEvent | null> {
  const value = await command("current_document", {});
  return value === null ? null : parseDocumentEvent(value);
}

export async function pageSize(
  document: number,
  page: number,
  scale: number,
): Promise<PageSize> {
  return parsePageSize(await command("page_size", { document, page, scale }));
}

export interface TileRequest {
  document: number;
  page: number;
  scale: number;
  x: number;
  y: number;
  width: number;
  height: number;
  generation: number;
}

export async function renderTile(request: TileRequest): Promise<Tile> {
  const buffer = await command("render_tile", { request });
  return parseTile(buffer, request.width, request.height);
}

/**
 * Starts a new view (document, page, scale) and returns its generation. The
 * shell numbers views, so a reloaded page never restarts below it, and drops
 * the tiles of older views at once.
 */
export async function beginView(): Promise<number> {
  const generation = await command("begin_view", {});
  if (!isCount(generation) || generation === 0) {
    throw new Error("invalid view generation from shell");
  }
  return generation;
}

/** Malformed payloads are dropped rather than shown. */
export function onJobEvent(handler: (event: JobEvent) => void): Promise<UnlistenFn> {
  return listen<unknown>(JOB_EVENT, ({ payload }) => {
    const event = parseJobEvent(payload);
    if (event !== null) {
      handler(event);
    }
  });
}

export function onDocumentEvent(handler: (event: DocumentEvent) => void): Promise<UnlistenFn> {
  return listen<unknown>(DOCUMENT_EVENT, ({ payload }) => {
    const event = parseDocumentEvent(payload);
    if (event !== null) {
      handler(event);
    }
  });
}
