import { describe, expect, it } from "vitest";

import {
  MAX_HISTORY,
  commit,
  identityPlan,
  isIdentity,
  move,
  redo,
  remove,
  rotate,
  startHistory,
  undo,
} from "./plan";

const pages = (plan: readonly { page: number }[]) => plan.map((entry) => entry.page);

describe("page plan", () => {
  it("starts with every page in order, unturned", () => {
    const plan = identityPlan(3);
    expect(pages(plan)).toEqual([0, 1, 2]);
    expect(isIdentity(plan, 3)).toBe(true);
    expect(isIdentity(plan, 4)).toBe(false);
  });

  it("turns a page by quarters both ways", () => {
    let plan = identityPlan(2);
    plan = rotate(plan, 1, 1);
    expect(plan[1]?.rotation).toBe(90);
    plan = rotate(rotate(plan, 1, -1), 1, -1);
    expect(plan[1]?.rotation).toBe(270);
    expect(plan[0]?.rotation).toBe(0);
    expect(isIdentity(plan, 2)).toBe(false);
    expect(rotate(plan, 5, 1)).toBe(plan);
  });

  it("removes pages but never the last one", () => {
    let plan = remove(identityPlan(3), 1);
    expect(pages(plan)).toEqual([0, 2]);
    plan = remove(plan, 0);
    expect(pages(plan)).toEqual([2]);
    expect(remove(plan, 0)).toBe(plan);
    expect(remove(plan, 7)).toBe(plan);
  });

  it("moves a page to another position", () => {
    const plan = identityPlan(4);
    expect(pages(move(plan, 0, 3))).toEqual([1, 2, 3, 0]);
    expect(pages(move(plan, 3, 1))).toEqual([0, 3, 1, 2]);
    expect(move(plan, 1, 1)).toBe(plan);
    expect(move(plan, 1, 4)).toBe(plan);
    expect(move(plan, -1, 0)).toBe(plan);
  });
});

describe("history", () => {
  it("undoes and redoes, a new change dropping what was undone", () => {
    const start = identityPlan(3);
    let history = startHistory(start);
    history = commit(history, remove(history.present, 0));
    history = commit(history, rotate(history.present, 0, 1));
    history = undo(history);
    expect(pages(history.present)).toEqual([1, 2]);
    expect(history.present[0]?.rotation).toBe(0);
    history = undo(history);
    expect(history.present).toBe(start);
    expect(undo(history)).toBe(history);
    history = redo(history);
    expect(pages(history.present)).toEqual([1, 2]);
    history = commit(history, move(history.present, 0, 1));
    expect(history.future).toEqual([]);
    expect(redo(history)).toBe(history);
  });

  it("records nothing for an unchanged plan and forgets the oldest plans", () => {
    let history = startHistory(identityPlan(2));
    expect(commit(history, history.present)).toBe(history);
    for (let i = 0; i < MAX_HISTORY + 10; i += 1) {
      history = commit(history, rotate(history.present, 0, 1));
    }
    expect(history.past).toHaveLength(MAX_HISTORY);
  });
});
