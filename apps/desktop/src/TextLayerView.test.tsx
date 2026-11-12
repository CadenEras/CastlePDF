import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { TextLayer } from "./ipc";
import { TextLayerView, selectedText, wordStyle } from "./TextLayerView";

const layer: TextLayer = {
  lines: [
    [
      { text: "Hello", x0: 10, y0: 50, x1: 40, y1: 50, size: 10 },
      { text: "world", x0: 45, y0: 50, x1: 75, y1: 50, size: 10 },
    ],
    [{ text: "<b>not html</b>", x0: 10, y0: 70, x1: 90, y1: 70, size: 10 }],
  ],
  truncated: false,
  incomplete: false,
};

afterEach(cleanup);

/** `value`, or a failed test when it is missing. */
function must<T>(value: T | null | undefined): T {
  if (value === null || value === undefined) {
    throw new Error("missing in the test");
  }
  return value;
}

function words(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>("[data-word]"));
}

describe("text layer", () => {
  it("places each word on its baseline, scaled to the page", () => {
    // Factor 2: pixels at scale 1 to CSS pixels. Ascent: 0.8 of 20 px.
    expect(wordStyle(must(layer.lines[0]?.[0]), 2)).toMatchObject({
      left: "20px",
      top: "84px",
      fontSize: "20px",
      transformOrigin: "0 16px",
    });
    // Vertical text turns by 90 degrees.
    const vertical = { text: "V", x0: 5, y0: 5, x1: 5, y1: 25, size: 10 };
    expect(wordStyle(vertical, 1).transform).toMatch(/^rotate\(90deg\)/);
  });

  it("shows the document's text as text, never as markup", () => {
    const { container, getByLabelText } = render(
      <TextLayerView layer={layer} factor={1} label="Texte de la page 1" />,
    );
    expect(getByLabelText("Texte de la page 1")).toBeTruthy();
    expect(words(container).map((w) => w.textContent)).toEqual([
      "Hello",
      "world",
      "<b>not html</b>",
    ]);
    expect(container.querySelector("b")).toBeNull();
  });

  it("copies the selection as words and lines", () => {
    const { container } = render(
      <TextLayerView layer={layer} factor={1} label="Texte" />,
    );
    const root = must(container.querySelector<HTMLElement>(".text-layer"));
    const [hello, , markup] = words(container);
    // From "llo" in the first word to "<b>" in the second line.
    const range = document.createRange();
    range.setStart(must(must(hello).firstChild), 2);
    range.setEnd(must(must(markup).firstChild), 3);
    const selection = must(window.getSelection());
    selection.removeAllRanges();
    selection.addRange(range);
    expect(selectedText(root, selection)).toBe("llo world\n<b>");
    let copied = "";
    fireEvent.copy(root, {
      clipboardData: {
        setData: (_type: string, text: string) => {
          copied = text;
        },
      },
    });
    expect(copied).toBe("llo world\n<b>");
    selection.removeAllRanges();
    expect(selectedText(root, selection)).toBe("");
  });
});
