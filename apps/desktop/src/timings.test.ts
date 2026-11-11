import { describe, expect, it } from "vitest";

import {
  MAX_TIMINGS,
  describeTimings,
  record,
  spread,
  summarize,
  type TileTiming,
} from "./timings";

describe("tile timings", () => {
  it("gives the median and the 95th percentile", () => {
    expect(spread([])).toEqual({ median: 0, p95: 0 });
    expect(spread([5])).toEqual({ median: 5, p95: 5 });
    const values = Array.from({ length: 100 }, (_, i) => 100 - i);
    expect(spread(values)).toEqual({ median: 50, p95: 95 });
  });

  it("keeps the last tiles only", () => {
    const timings: TileTiming[] = [];
    for (let i = 0; i < MAX_TIMINGS + 10; i += 1) {
      record(timings, { receive: i, draw: 1 });
    }
    expect(timings).toHaveLength(MAX_TIMINGS);
    expect(timings[0]?.receive).toBe(10);
  });

  it("describes them in one line", () => {
    expect(describeTimings(summarize([]))).toBe("Aucune tuile mesurée.");
    const line = describeTimings(
      summarize([
        { receive: 10, draw: 0.5 },
        { receive: 30, draw: 1.5 },
      ]),
    );
    expect(line).toBe(
      "2 tuiles. Réception (requête → données) : médiane 10,0 ms, p95 30,0 ms. " +
        "Dessin (ImageData + putImageData) : médiane 0,5 ms, p95 1,5 ms.",
    );
  });
});
