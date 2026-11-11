/**
 * Time spent per tile on the WebView side, kept for the last tiles only:
 * from the `render_tile` request to its data (worker rendering, queueing in
 * the shell and IPC transport), and drawing it (`ImageData` and
 * `putImageData`). Shown in the viewer's "Mesures" panel, to measure the
 * transport chosen by ADR-0003 in the real application.
 */
export interface TileTiming {
  /** Request to data received, in milliseconds. */
  receive: number;
  /** `ImageData` and `putImageData`, in milliseconds. */
  draw: number;
}

/** Tiles kept: the oldest are dropped beyond. */
export const MAX_TIMINGS = 2000;

export interface Spread {
  median: number;
  p95: number;
}

export interface TimingSummary {
  count: number;
  receive: Spread;
  draw: Spread;
}

/** Median and 95th percentile (nearest rank); zeros for no value. */
export function spread(values: readonly number[]): Spread {
  if (values.length === 0) {
    return { median: 0, p95: 0 };
  }
  const sorted = [...values].sort((a, b) => a - b);
  const at = (p: number) =>
    sorted[Math.min(sorted.length - 1, Math.ceil(p * sorted.length) - 1)] ?? 0;
  return { median: at(0.5), p95: at(0.95) };
}

export function summarize(timings: readonly TileTiming[]): TimingSummary {
  return {
    count: timings.length,
    receive: spread(timings.map((t) => t.receive)),
    draw: spread(timings.map((t) => t.draw)),
  };
}

/** Adds `timing`, dropping the oldest beyond `MAX_TIMINGS`. */
export function record(timings: TileTiming[], timing: TileTiming): void {
  timings.push(timing);
  if (timings.length > MAX_TIMINGS) {
    timings.splice(0, timings.length - MAX_TIMINGS);
  }
}

/** One line for the panel, in French like the rest of the UI. */
export function describeTimings(summary: TimingSummary): string {
  if (summary.count === 0) {
    return "Aucune tuile mesurée.";
  }
  const ms = (value: number) => `${value.toFixed(1).replace(".", ",")} ms`;
  return (
    `${String(summary.count)} tuiles. Réception (requête → données) : médiane ` +
    `${ms(summary.receive.median)}, p95 ${ms(summary.receive.p95)}. Dessin (ImageData + ` +
    `putImageData) : médiane ${ms(summary.draw.median)}, p95 ${ms(summary.draw.p95)}.`
  );
}
