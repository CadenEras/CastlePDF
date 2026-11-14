import {
  Fragment,
  useEffect,
  useMemo,
  useRef,
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

/** Letter spacing that brings `text` from its natural width to `length`. */
function spacing(text: string, size: number, length: number): string | undefined {
  const natural = naturalWidth(text, size);
  const units = Array.from(text).length;
  if (natural === null || units === 0) {
    return undefined;
  }
  return `${String((length - natural) / units)}px`;
}

/** A line of the layer, placed: the line's box, then its words and gaps. */
export interface PlacedLine {
  style: CSSProperties;
  words: { text: string; style: CSSProperties }[];
  /** Style of the space before each word after the first. */
  gaps: CSSProperties[];
}

/**
 * Where a line goes: one box from its first word's baseline start, turned
 * along the line, where its words follow each other in one flow with real
 * spaces between them. Letter spacing brings each word and each space to
 * its length on the page, so that a word covers the word drawn on the
 * canvas below, and a screen reader reads a line as a line rather than
 * word by word. `factor` turns pixels of the page at scale 1 into CSS
 * pixels.
 */
export function placeLine(line: LayerWord[], factor: number): PlacedLine {
  const first = line[0];
  const last = line[line.length - 1];
  if (first === undefined || last === undefined) {
    return { style: {}, words: [], gaps: [] };
  }
  // The line's direction: from its start to its end, or along its only
  // word.
  let dx = last.x1 - first.x0;
  let dy = last.y1 - first.y0;
  if (Math.hypot(dx, dy) === 0) {
    dx = 1;
    dy = 0;
  }
  const norm = Math.hypot(dx, dy);
  const ux = dx / norm;
  const uy = dy / norm;
  const sizes = line.map((word) => Math.max(word.size * factor, 1));
  const lineSize = Math.max(...sizes);
  const ascent = lineSize * ASCENT;
  // Start and length of each word along the line, in CSS pixels.
  const spans = line.map((word) => ({
    start: ((word.x0 - first.x0) * ux + (word.y0 - first.y0) * uy) * factor,
    length: Math.hypot(word.x1 - word.x0, word.y1 - word.y0) * factor,
  }));
  const words = line.map((word, index) => {
    const size = sizes[index] ?? lineSize;
    return {
      text: word.text,
      style: {
        fontSize: `${String(size)}px`,
        letterSpacing: spacing(word.text, size, spans[index]?.length ?? 0),
      },
    };
  });
  const gaps = spans.slice(1).map((span, index) => {
    const before = spans[index];
    const size = sizes[index] ?? lineSize;
    const gap = before === undefined ? 0 : span.start - before.start - before.length;
    return {
      fontSize: `${String(size)}px`,
      letterSpacing: spacing(" ", size, gap),
    };
  });
  const angle = (Math.atan2(uy, ux) * 180) / Math.PI;
  return {
    style: {
      left: `${String(first.x0 * factor)}px`,
      top: `${String(first.y0 * factor - ascent)}px`,
      fontSize: `${String(lineSize)}px`,
      transformOrigin: `0 ${String(ascent)}px`,
      transform: `rotate(${String(angle)}deg)`,
    },
    words,
    gaps,
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
 * Keeps a selection in place while the pointer crosses a gap of the layer
 * (between lines, a margin, between columns), instead of the WebView
 * jumping it to whatever comes first in the document: the way pdf.js does
 * it. While the pointer is down, an element that cannot be selected covers
 * the layer under the lines, and it follows the line of the selection's
 * moving end in the DOM, so that a gap resolves next to that line. Returns
 * the function that removes it.
 */
export function holdSelection(layer: HTMLElement): () => void {
  const end = document.createElement("div");
  end.className = "end-of-content";
  end.setAttribute("aria-hidden", "true");
  layer.append(end);
  let selecting = false;

  const stop = () => {
    if (!selecting) {
      return;
    }
    selecting = false;
    end.style.width = "";
    end.style.height = "";
    layer.append(end);
  };
  const onPointerDown = (event: PointerEvent) => {
    // A press of another button than the main one does not select.
    if (event.button > 0) {
      return;
    }
    selecting = true;
    end.style.width = `${String(layer.clientWidth)}px`;
    end.style.height = `${String(layer.clientHeight)}px`;
  };
  const onSelectionChange = () => {
    if (!selecting) {
      return;
    }
    const selection = document.getSelection();
    const focus = selection?.focusNode ?? null;
    if (selection === null || focus === null || selection.rangeCount === 0) {
      return;
    }
    // The line the moving end is in: a gap past it resolves to its end
    // (forward) or to its start (backward), as in a text editor.
    const element =
      focus.nodeType === Node.ELEMENT_NODE
        ? (focus as Element)
        : focus.parentElement;
    const line = element?.closest("[data-line]") ?? null;
    if (line === null || line.parentNode !== layer) {
      return;
    }
    const range = selection.getRangeAt(0);
    const forward =
      range.endContainer === focus && range.endOffset === selection.focusOffset;
    const next = forward ? line.nextSibling : line;
    if (next !== end && next?.previousSibling !== end) {
      layer.insertBefore(end, next);
    }
  };

  layer.addEventListener("pointerdown", onPointerDown);
  document.addEventListener("selectionchange", onSelectionChange);
  document.addEventListener("pointerup", stop);
  document.addEventListener("pointercancel", stop);
  window.addEventListener("blur", stop);
  return () => {
    layer.removeEventListener("pointerdown", onPointerDown);
    document.removeEventListener("selectionchange", onSelectionChange);
    document.removeEventListener("pointerup", stop);
    document.removeEventListener("pointercancel", stop);
    window.removeEventListener("blur", stop);
    end.remove();
  };
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
    () => layer.lines.map((line) => placeLine(line, factor)),
    [layer, factor],
  );
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const element = root.current;
    return element === null ? undefined : holdSelection(element);
  }, []);
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
    <div
      className="text-layer"
      aria-label={label}
      onCopy={onCopy}
      ref={root}
    >
      {lines.map((line, index) => (
        <div className="text-line" data-line key={index} style={line.style}>
          {line.words.map((word, position) => (
            <Fragment key={position}>
              {position > 0 && (
                <span data-gap style={line.gaps[position - 1]}>
                  {" "}
                </span>
              )}
              <span data-word style={word.style}>
                {word.text}
              </span>
            </Fragment>
          ))}
        </div>
      ))}
    </div>
  );
}
