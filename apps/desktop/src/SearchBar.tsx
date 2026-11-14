import { useEffect, useRef, useState, type RefObject } from "react";

import type { SearchOptions } from "./ipc";
import { MAX_RESULTS, counts, type SearchState } from "./search";

/** Pause in typing after which the query is searched. */
const TYPING_PAUSE_MS = 300;

/** What the search says, for the status line read by screen readers. */
export function describeSearch(state: SearchState): string {
  if (state.error !== null) {
    return `Recherche interrompue : ${state.error}.`;
  }
  if (state.query === "") {
    return "";
  }
  const { total, index } = counts(state.hits, state.current);
  const progress = state.running
    ? ` Recherche en cours, page ${String(state.searched)} sur ${String(state.pages)}…`
    : "";
  const limited = state.limited
    ? ` Plus de ${MAX_RESULTS.toLocaleString("fr-FR")} résultats : recherche arrêtée.`
    : "";
  if (total === 0) {
    return state.running ? progress.trim() : "Aucun résultat.";
  }
  const where =
    index === null
      ? `${String(total)} résultat${total > 1 ? "s" : ""}.`
      : `Résultat ${String(index + 1)} sur ${String(total)}.`;
  return `${where}${progress}${limited}`;
}

/**
 * The search field of a document: searched as the user types (after a
 * pause) or on Enter; Enter and Shift+Enter move to the next and previous
 * occurrence, Escape ends the search.
 */
export function SearchBar({
  state,
  input,
  onSearch,
  onNext,
  onPrevious,
  onClear,
}: {
  state: SearchState;
  input: RefObject<HTMLInputElement | null>;
  onSearch: (query: string, options: SearchOptions) => void;
  onNext: () => void;
  onPrevious: () => void;
  onClear: () => void;
}) {
  const [text, setText] = useState("");
  const [matchCase, setMatchCase] = useState(false);
  const [matchDiacritics, setMatchDiacritics] = useState(false);
  // What was last sent, so that a pause or Enter does not search it again.
  const sent = useRef({ text: "", matchCase: false, matchDiacritics: false });
  const submit = () => {
    const last = sent.current;
    if (
      last.text === text &&
      last.matchCase === matchCase &&
      last.matchDiacritics === matchDiacritics
    ) {
      return false;
    }
    sent.current = { text, matchCase, matchDiacritics };
    onSearch(text, { matchCase, matchDiacritics });
    return true;
  };
  const submitRef = useRef(submit);
  useEffect(() => {
    submitRef.current = submit;
  });
  useEffect(() => {
    const timer = setTimeout(() => {
      submitRef.current();
    }, TYPING_PAUSE_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [text, matchCase, matchDiacritics]);

  const { total } = counts(state.hits, state.current);
  const status = describeSearch(state);
  return (
    <div className="search" role="search" aria-label="Recherche dans le document">
      <input
        ref={input}
        type="search"
        value={text}
        aria-label="Rechercher dans le document"
        placeholder="Rechercher (Ctrl+F)"
        maxLength={256}
        onChange={(event) => {
          setText(event.target.value);
        }}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            // A query not searched yet is searched; then Enter moves on.
            if (!submit()) {
              if (event.shiftKey) {
                onPrevious();
              } else {
                onNext();
              }
            }
          } else if (event.key === "Escape") {
            event.preventDefault();
            setText("");
            sent.current = { ...sent.current, text: "" };
            onClear();
          }
        }}
      />
      <label>
        <input
          type="checkbox"
          checked={matchCase}
          onChange={(event) => {
            setMatchCase(event.target.checked);
          }}
        />
        Respecter la casse
      </label>
      <label>
        <input
          type="checkbox"
          checked={matchDiacritics}
          onChange={(event) => {
            setMatchDiacritics(event.target.checked);
          }}
        />
        Respecter les accents
      </label>
      <button type="button" onClick={onPrevious} disabled={total === 0}>
        Résultat précédent
      </button>
      <button type="button" onClick={onNext} disabled={total === 0}>
        Résultat suivant
      </button>
      <p aria-live="polite" aria-atomic="true" className="search-status">
        {status}
      </p>
      {state.partial && (
        <p className="partial" role="note" aria-label="Recherche partielle">
          Recherche partielle : une partie du texte n'a pas pu être lue ou
          cherchée ; des résultats peuvent manquer.
        </p>
      )}
    </div>
  );
}
