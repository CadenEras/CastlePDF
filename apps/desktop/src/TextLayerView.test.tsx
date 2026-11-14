import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { TextLayer } from "./ipc";
import { TextLayerView, placeLine, selectedText } from "./TextLayerView";

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
  it("places each line on its first word's baseline, scaled to the page", () => {
    // Factor 2: pixels at scale 1 to CSS pixels. Ascent: 0.8 of 20 px.
    const placed = placeLine(must(layer.lines[0]), 2);
    expect(placed.style).toMatchObject({
      left: "20px",
      top: "84px",
      fontSize: "20px",
      transformOrigin: "0 16px",
      transform: "rotate(0deg)",
    });
    expect(placed.words.map((word) => word.text)).toEqual(["Hello", "world"]);
    expect(placed.gaps).toHaveLength(1);
    // The largest word sets the line's size; each word keeps its own.
    const mixed = placeLine(
      [
        { text: "a", x0: 0, y0: 30, x1: 6, y1: 30, size: 12 },
        { text: "B", x0: 10, y0: 30, x1: 22, y1: 30, size: 18 },
      ],
      1,
    );
    expect(mixed.style.fontSize).toBe("18px");
    expect(mixed.words.map((word) => word.style.fontSize)).toEqual([
      "12px",
      "18px",
    ]);
    // A vertical line turns by 90 degrees.
    const vertical = [{ text: "V", x0: 5, y0: 5, x1: 5, y1: 25, size: 10 }];
    expect(placeLine(vertical, 1).style.transform).toBe("rotate(90deg)");
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
    // A line is one flow of words and real spaces, read as a line.
    expect(
      Array.from(container.querySelectorAll("[data-line]")).map(
        (line) => line.textContent,
      ),
    ).toEqual(["Hello world", "<b>not html</b>"]);
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

  it("marks the occurrences of a search, and still copies the words", () => {
    const revealed: string[] = [];
    const { container } = render(
      <TextLayerView
        layer={layer}
        factor={1}
        label="Texte"
        // From "ello" to "wo", across the space; then "not".
        hits={[
          { start: [0, 0, 1], end: [0, 1, 2] },
          { start: [1, 0, 3], end: [1, 0, 6] },
        ]}
        current={1}
        reveal
        onRevealed={(target) => {
          revealed.push(target.textContent);
        }}
      />,
    );
    const marks = Array.from(container.querySelectorAll("mark"));
    expect(marks.map((mark) => mark.textContent)).toEqual(["ello", " ", "wo", "not"]);
    expect(marks.map((mark) => mark.dataset.hit)).toEqual(["0", "0", "0", "1"]);
    expect(container.querySelectorAll("[data-current]")).toHaveLength(1);
    expect(container.querySelector("[data-current]")?.textContent).toBe("not");
    // Handed to the viewer to scroll to; the layer does not scroll.
    expect(revealed).toEqual(["not"]);
    // The words keep their text, and the line its spaces.
    expect(words(container).map((w) => w.textContent)).toEqual([
      "Hello",
      "world",
      "<b>not html</b>",
    ]);
    const root = must(container.querySelector<HTMLElement>(".text-layer"));
    const [hello, world] = words(container);
    // From the "H" before the mark to the "r" after the next one.
    const range = document.createRange();
    range.setStart(must(must(hello).firstChild), 0);
    range.setEnd(must(must(world).lastChild), 1);
    const selection = must(window.getSelection());
    selection.removeAllRanges();
    selection.addRange(range);
    expect(selectedText(root, selection)).toBe("Hello wor");
    selection.removeAllRanges();
  });

  it("keeps the selection's moving end over a gap while selecting", () => {
    const { container, unmount } = render(
      <TextLayerView layer={layer} factor={1} label="Texte" />,
    );
    const root = must(container.querySelector<HTMLElement>(".text-layer"));
    const end = must(root.querySelector<HTMLElement>(".end-of-content"));
    expect(end.getAttribute("aria-hidden")).toBe("true");
    expect(root.lastChild).toBe(end);
    const [hello, world, markup] = words(container);
    const selection = must(window.getSelection());
    const select = (from: Node, to: Node, offset: number) => {
      selection.removeAllRanges();
      selection.setBaseAndExtent(from, 0, to, offset);
      document.dispatchEvent(new Event("selectionchange"));
    };

    // Without a press, the selection does not move it.
    select(must(must(hello).firstChild), must(must(world).firstChild), 2);
    expect(root.lastChild).toBe(end);

    fireEvent.pointerDown(root, { button: 0 });
    const [first, second] = Array.from(
      root.querySelectorAll<HTMLElement>("[data-line]"),
    );
    // Forward: right after the line the selection ends in.
    select(must(must(hello).firstChild), must(must(world).firstChild), 2);
    expect(must(first).nextSibling).toBe(end);
    select(must(must(hello).firstChild), must(must(markup).firstChild), 1);
    expect(must(second).nextSibling).toBe(end);
    // Backward: right before it.
    select(must(must(markup).firstChild), must(must(world).firstChild), 1);
    expect(must(first).previousSibling).toBe(end);
    // A copy never takes it.
    expect(selectedText(root, selection)).toBe("orld");

    fireEvent.pointerUp(document);
    expect(root.lastChild).toBe(end);
    expect(end.style.width).toBe("");
    unmount();
    expect(end.isConnected).toBe(false);
    selection.removeAllRanges();
  });
});
