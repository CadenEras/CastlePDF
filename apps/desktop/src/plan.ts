/**
 * A page plan over the current document (ADR-0008, tranche 4.6): the pages
 * to save, in order, each with the quarter turns added to its own rotation.
 * Editing never touches the document: each operation gives a new plan, and
 * the history keeps the previous ones for undo and redo.
 */

export type Rotation = 0 | 90 | 180 | 270;

export interface PlanPage {
  /** Page of the document, from 0. */
  readonly page: number;
  readonly rotation: Rotation;
}

export type Plan = readonly PlanPage[];

/** Plans kept for undo; older ones are forgotten. */
export const MAX_HISTORY = 100;

export function identityPlan(pages: number): Plan {
  return Array.from({ length: pages }, (_, page) => ({ page, rotation: 0 }));
}

/** Whether `plan` is every page of a document of `pages` pages, in order, unturned. */
export function isIdentity(plan: Plan, pages: number): boolean {
  return plan.length === pages && plan.every((entry, index) => entry.page === index && entry.rotation === 0);
}

/** Entry `index` turned by a quarter, clockwise (`1`) or anticlockwise (`-1`). */
export function rotate(plan: Plan, index: number, direction: 1 | -1): Plan {
  const entry = plan[index];
  if (entry === undefined) {
    return plan;
  }
  const rotation = (((entry.rotation + direction * 90) % 360) + 360) % 360;
  return plan.map((current, at) =>
    at === index ? { page: current.page, rotation: rotation as Rotation } : current,
  );
}

/** Entry `index` removed; the last page is never removed (a plan is never empty). */
export function remove(plan: Plan, index: number): Plan {
  if (plan.length <= 1 || index < 0 || index >= plan.length) {
    return plan;
  }
  return plan.filter((_, at) => at !== index);
}

/** Entry `from` moved to position `to` (both within the plan). */
export function move(plan: Plan, from: number, to: number): Plan {
  if (from === to || from < 0 || to < 0 || from >= plan.length || to >= plan.length) {
    return plan;
  }
  const next = [...plan];
  const [entry] = next.splice(from, 1);
  if (entry === undefined) {
    return plan;
  }
  next.splice(to, 0, entry);
  return next;
}

export interface History {
  readonly past: readonly Plan[];
  readonly present: Plan;
  readonly future: readonly Plan[];
}

export function startHistory(plan: Plan): History {
  return { past: [], present: plan, future: [] };
}

/** `next` as the present plan; an unchanged plan records nothing. */
export function commit(history: History, next: Plan): History {
  if (next === history.present) {
    return history;
  }
  const past = [...history.past, history.present].slice(-MAX_HISTORY);
  return { past, present: next, future: [] };
}

export function undo(history: History): History {
  const previous = history.past.at(-1);
  if (previous === undefined) {
    return history;
  }
  return {
    past: history.past.slice(0, -1),
    present: previous,
    future: [history.present, ...history.future],
  };
}

export function redo(history: History): History {
  const [next, ...future] = history.future;
  if (next === undefined) {
    return history;
  }
  return { past: [...history.past, history.present], present: next, future };
}
