import {
  Fragment,
  useMemo,
  type ClipboardEvent,
  type CSSProperties,
} from "react";

import type { LayerWord, TextLayer } from "./ipc";

/** Share of the font size above the baseline, as the engine bounds words. */
const ASCENT = 0.8;

let measuring: CanvasRenderingContext2D | null | undefined;

/** Width of `text` at `size` CSS pixels in the layer's font, if measurable. */
function naturalWidth(text: string, size: number): number | null {
  if (measuring === undefined) {
    measuring =
      typeof document === "undefined"
        ? null
        : (() => {
            try {
              return document.createElement("canvas").getContext("2d");
            } catch {
              // No canvas (tests): words keep their natural width.
              return null;
            }
          })();
  }
  if (measuring === null) {
    return null;
  }
  try {
    measuring.font = `${String(size)}px sans-serif`;
    const width = measuring.measureText(text).width;
    return width > 0 ? width : null;
  } catch {
    // A context that cannot measure: words keep their natural width.
    return null;
  }
}

/**
 * Where a word's span goes: its baseline start, at its size, turned along
 * its baseline and stretched to its length, so that selecting it covers the
 * word drawn on the canvas below. `factor` turns pixels of the page at
 * scale 1 into CSS pixels.
 */
export function wordStyle(word: LayerWord, factor: number): CSSProperties {
  const size = Math.max(word.size * factor, 1);
  const dx = (word.x1 - word.x0) * factor;
  const dy = (word.y1 - word.y0) * factor;
  const length = Math.hypot(dx, dy);
  const angle = (Math.atan2(dy, dx) * 180) / Math.PI;
  const natural = naturalWidth(word.text, size);
  const stretch = natural !== null && length > 0 ? length / natural : 1;
  const ascent = size * ASCENT;
  return {
    left: `${String(word.x0 * factor)}px`,
    top: `${String(word.y0 * factor - ascent)}px`,
    fontSize: `${String(size)}px`,
    transformOrigin: `0 ${String(ascent)}px`,
    transform: `rotate(${String(angle)}deg) scaleX(${String(stretch)})`,
  };
}

/**
 * The text a selection covers, words of a line joined by one space and
 * lines by a line break, in reading order: what a copy puts on the
 * clipboard, rather than the browser's view of absolutely placed words.
 */
export function selectedText(root: HTMLElement, selection: Selection): string {
  if (selection.rangeCount === 0 || selection.isCollapsed) {
    return "";
  }
  const range = selection.getRangeAt(0);
  const lines: string[] = [];
  for (const line of Array.from(
    root.querySelectorAll<HTMLElement>("[data-line]"),
  )) {
    const words: string[] = [];
    for (const span of Array.from(
      line.querySelectorAll<HTMLElement>("[data-word]"),
    )) {
      if (!range.intersectsNode(span)) {
        continue;
      }
      const text = span.textContent;
      const node = span.firstChild;
      let start = 0;
      let end = text.length;
      if (node !== null && range.startContainer === node) {
        start = range.startOffset;
      }
      if (node !== null && range.endContainer === node) {
        end = range.endOffset;
      }
      const part = text.slice(start, end);
      if (part !== "") {
        words.push(part);
      }
    }
    if (words.length > 0) {
      lines.push(words.join(" "));
    }
  }
  return lines.join("\n");
}

/**
 * A page's text over its canvas: transparent, selectable, read by screen
 * readers in reading order. Text from the document is only ever text.
 */
export function TextLayerView({
  layer,
  factor,
  label,
}: {
  layer: TextLayer;
  factor: number;
  label: string;
}) {
  const lines = useMemo(
    () =>
      layer.lines.map((line) =>
        line.map((word) => ({
          text: word.text,
          style: wordStyle(word, factor),
        })),
      ),
    [layer, factor],
  );
  const onCopy = (event: ClipboardEvent<HTMLDivElement>) => {
    const selection = window.getSelection();
    if (selection === null) {
      return;
    }
    const text = selectedText(event.currentTarget, selection);
    if (text !== "") {
      event.clipboardData.setData("text/plain", text);
      event.preventDefault();
    }
  };
  return (
    <div className="text-layer" aria-label={label} onCopy={onCopy}>
      {lines.map((line, index) => (
        // The spaces and line breaks between words are for screen readers
        // and for a copy outside this layer; they take no room.
        <div className="text-line" data-line key={index}>
          {line.map((word, position) => (
            <Fragment key={position}>
              {position > 0 && " "}
              <span data-word style={word.style}>
                {word.text}
              </span>
            </Fragment>
          ))}
          {"\n"}
        </div>
      ))}
    </div>
  );
}
