import { useEffect, useRef, useState, type KeyboardEvent, type UIEvent } from "react";

import { describeCommandError, saveAs, type DocumentInfo, type Saved } from "./ipc";
import {
  commit,
  identityPlan,
  isIdentity,
  move,
  redo,
  remove,
  rotate,
  startHistory,
  undo,
  type History,
  type Plan,
} from "./plan";

/** Height of a row of the list, in CSS pixels (see `.plan-list li`). */
export const ROW_HEIGHT = 32;
/** Height of the list when it cannot be measured (tests, first render). */
const DEFAULT_LIST_HEIGHT = 320;
/** Rows rendered above and below the visible ones. */
const OVERSCAN = 8;

type SaveState =
  | { kind: "idle" }
  | { kind: "saving" }
  | { kind: "saved"; result: Saved }
  | { kind: "failed"; message: string };

/** The rows to render for a list scrolled to `scrollTop`: only those seen. */
export function visibleRows(
  count: number,
  scrollTop: number,
  height: number,
): { first: number; last: number } {
  const first = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN);
  const last = Math.min(count, Math.ceil((scrollTop + height) / ROW_HEIGHT) + OVERSCAN);
  return { first, last };
}

function sizeLabel(bytes: number): string {
  if (bytes < 1024 * 1024) {
    return `${String(Math.max(1, Math.round(bytes / 1024)))} Kio`;
  }
  return `${(bytes / (1024 * 1024)).toFixed(1).replace(".", ",")} Mio`;
}

function pageLabel(page: number, rotation: number): string {
  return rotation === 0
    ? `Page ${String(page + 1)}`
    : `Page ${String(page + 1)}, tournée de ${String(rotation)}°`;
}

function SaveResult({ state }: { state: SaveState }) {
  switch (state.kind) {
    case "idle":
      return null;
    case "saving":
      return <p role="status">Enregistrement…</p>;
    case "failed":
      return <p role="alert">Échec de l'enregistrement : {state.message}.</p>;
    case "saved": {
      const { result } = state;
      return (
        <div role="status" className="save-result">
          <p>
            Enregistré : {result.pages} page{result.pages > 1 ? "s" : ""},{" "}
            {sizeLabel(result.bytes)}.
          </p>
          {result.dropped.length > 0 && (
            <p>Non conservé : {result.dropped.join(", ")}.</p>
          )}
          {result.keptWhole.length > 0 && (
            <p>
              Ressources gardées entières pour les pages{" "}
              {result.keptWhole.map((page) => String(page + 1)).join(", ")} : ce que dessinaient
              les pages retirées peut rester dans le fichier.
            </p>
          )}
        </div>
      );
    }
  }
}

/**
 * The pages of the current document as a plan to save (ADR-0008, tranche
 * 4.6a): turn, remove and move pages, undo and redo, then "save as". The
 * document itself never changes; the shell chooses the file.
 */
export function PagePanel({ info }: { info: DocumentInfo }) {
  const [history, setHistory] = useState<History>(() => startHistory(identityPlan(info.pages)));
  const [selected, setSelected] = useState(0);
  const [save, setSave] = useState<SaveState>({ kind: "idle" });
  const [scrollTop, setScrollTop] = useState(0);
  const [listHeight, setListHeight] = useState(DEFAULT_LIST_HEIGHT);
  const list = useRef<HTMLOListElement>(null);
  // The rows seen depend on the list's height, measured as it changes.
  useEffect(() => {
    const element = list.current;
    if (element === null || typeof ResizeObserver === "undefined") {
      return;
    }
    const observer = new ResizeObserver(() => {
      setListHeight(element.clientHeight || DEFAULT_LIST_HEIGHT);
    });
    observer.observe(element);
    return () => {
      observer.disconnect();
    };
  }, []);
  const plan = history.present;
  const current = Math.min(selected, plan.length - 1);
  const saving = save.kind === "saving";

  const change = (next: Plan, select: number) => {
    setHistory((h) => commit(h, next));
    setSelected(Math.max(0, Math.min(select, next.length - 1)));
    setSave({ kind: "idle" });
  };
  const step = (go: (h: History) => History) => {
    const next = go(history);
    setHistory(next);
    setSelected((s) => Math.min(s, next.present.length - 1));
    setSave({ kind: "idle" });
  };
  const select = (index: number) => {
    const clamped = Math.max(0, Math.min(index, plan.length - 1));
    setSelected(clamped);
    // Keep the selected row in sight.
    const element = list.current;
    if (element !== null) {
      const top = clamped * ROW_HEIGHT;
      if (top < element.scrollTop) {
        element.scrollTop = top;
      } else if (top + ROW_HEIGHT > element.scrollTop + element.clientHeight) {
        element.scrollTop = top + ROW_HEIGHT - element.clientHeight;
      }
    }
  };

  const actions = {
    left: () => { change(rotate(plan, current, -1), current); },
    right: () => { change(rotate(plan, current, 1), current); },
    remove: () => { change(remove(plan, current), current); },
    up: () => { change(move(plan, current, current - 1), current - 1); },
    down: () => { change(move(plan, current, current + 1), current + 1); },
    undo: () => { step(undo); },
    redo: () => { step(redo); },
    save: () => {
      setSave({ kind: "saving" });
      saveAs(info.document, plan).then(
        (result) => { setSave(result === null ? { kind: "idle" } : { kind: "saved", result }); },
        (error: unknown) => { setSave({
            kind: "failed",
            message: error instanceof Error ? error.message : describeCommandError(error),
          }); },
      );
    },
  };

  const onKeyDown = (event: KeyboardEvent<HTMLOListElement>) => {
    const control = event.ctrlKey || event.metaKey;
    const key = event.key.toLowerCase();
    let handled = true;
    if (control && key === "z" && !event.shiftKey) {
      actions.undo();
    } else if (control && (key === "y" || (key === "z" && event.shiftKey))) {
      actions.redo();
    } else if (event.altKey && event.key === "ArrowUp") {
      actions.up();
    } else if (event.altKey && event.key === "ArrowDown") {
      actions.down();
    } else if (event.key === "ArrowUp") {
      select(current - 1);
    } else if (event.key === "ArrowDown") {
      select(current + 1);
    } else if (event.key === "Home") {
      select(0);
    } else if (event.key === "End") {
      select(plan.length - 1);
    } else if (event.key === "Delete") {
      actions.remove();
    } else if (key === "r" && !control) {
      if (event.shiftKey) {
        actions.left();
      } else {
        actions.right();
      }
    } else {
      handled = false;
    }
    if (handled) {
      event.preventDefault();
    }
  };

  const { first, last } = visibleRows(plan.length, scrollTop, listHeight);
  const unchanged = isIdentity(plan, info.pages);

  return (
    <section aria-labelledby="pages-title" className="plan">
      <h3 id="pages-title">
        Pages à enregistrer ({plan.length} sur {info.pages})
      </h3>
      <div className="actions" role="toolbar" aria-label="Opérations sur la page choisie">
        <button type="button" onClick={actions.left} disabled={saving}>
          Pivoter à gauche
        </button>
        <button type="button" onClick={actions.right} disabled={saving}>
          Pivoter à droite
        </button>
        <button type="button" onClick={actions.remove} disabled={saving || plan.length <= 1}>
          Retirer
        </button>
        <button type="button" onClick={actions.up} disabled={saving || current === 0}>
          Monter
        </button>
        <button
          type="button"
          onClick={actions.down}
          disabled={saving || current === plan.length - 1}
        >
          Descendre
        </button>
        <button type="button" onClick={actions.undo} disabled={saving || history.past.length === 0}>
          Annuler
        </button>
        <button
          type="button"
          onClick={actions.redo}
          disabled={saving || history.future.length === 0}
        >
          Rétablir
        </button>
      </div>
      <ol
        ref={list}
        className="plan-list"
        role="listbox"
        aria-label="Pages dans l'ordre d'enregistrement"
        aria-activedescendant={`plan-entry-${String(current)}`}
        aria-keyshortcuts="R Shift+R Delete Alt+ArrowUp Alt+ArrowDown Control+Z Control+Y"
        tabIndex={0}
        onKeyDown={onKeyDown}
        onScroll={(event: UIEvent<HTMLOListElement>) => { setScrollTop(event.currentTarget.scrollTop); }}
      >
        <li aria-hidden="true" style={{ height: first * ROW_HEIGHT }} className="spacer" />
        {plan.slice(first, last).map((entry, offset) => {
          const index = first + offset;
          return (
            <li
              key={`${String(entry.page)}-${String(index)}`}
              id={`plan-entry-${String(index)}`}
              role="option"
              aria-selected={index === current}
              aria-setsize={plan.length}
              aria-posinset={index + 1}
              onClick={() => { select(index); }}
            >
              {pageLabel(entry.page, entry.rotation)}
            </li>
          );
        })}
        <li
          aria-hidden="true"
          style={{ height: (plan.length - last) * ROW_HEIGHT }}
          className="spacer"
        />
      </ol>
      <p className="note">
        Le document ouvert n'est jamais modifié : « Enregistrer sous » écrit un nouveau fichier.
        Signets, formulaires, arbre de structure et signatures ne sont pas conservés ; retirer
        une page n'est pas un caviardage.
      </p>
      <div className="actions">
        <button type="button" onClick={actions.save} disabled={saving}>
          Enregistrer sous…
        </button>
        {unchanged && <span className="hint">Aucune modification : copie du document entier.</span>}
      </div>
      <SaveResult state={save} />
    </section>
  );
}
