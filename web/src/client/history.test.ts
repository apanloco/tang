import { describe, expect, test } from "vitest";

import { HISTORY_MAX_STEPS, HISTORY_SAME_STEP } from "../protocol";
import type { Event, Intent, Snapshot } from "../protocol";
import { historyFixture } from "./fixtures";
import type { HistoryRow } from "./fixtures";
import { History, planOf, stepOf } from "./history";
import type { Key } from "./history";
import { MockServer } from "./mock";

// The history of what a person turned and the way back (docs/web-ui.md §5, D35): the mock engine's, which has to be the controller's
// (`src/app/history.rs`), so that what is built on it is built on what the program does.

/** A step that sets the lock to `redo` and puts it back to `undo`: what a step is made of does not matter to the history. */
const lock = (label: string, undo: boolean, redo: boolean, key: Key | null, at: number) =>
  stepOf(label, { type: "setLocked", locked: undo }, { type: "setLocked", locked: redo }, key, at);

const knob = (n: number): Key => `param:1:${String(n)}`;

describe("the history", () => {
  test("has nothing to undo or redo to begin with", () => {
    const history = new History();
    expect(history.state()).toEqual({ undo: null, redo: null });
    expect(history.takeUndo()).toBeUndefined();
    expect(history.takeRedo()).toBeUndefined();
  });

  test("keeps what was recorded, and the last of it comes first", () => {
    const history = new History();
    history.record(lock("set cutoff", false, true, null, 0));
    history.record(lock("set mix", true, false, null, 5));
    expect(history.state()).toEqual({ undo: "set mix", redo: null });
    const last = history.takeUndo();
    expect([last?.label, last?.undo, last?.redo]).toEqual(["set mix", [{ type: "setLocked", locked: true }], [{ type: "setLocked", locked: false }]]);
    expect(history.state().undo).toBe("set cutoff");
  });

  test("keeps a step that was undone to be redone, and one that was redone to be undone again", () => {
    const history = new History();
    history.record(lock("set cutoff", false, true, null, 0));
    const undone = history.takeUndo();
    if (!undone) throw new Error("no step");
    history.keptUndone(undone);
    expect(history.state()).toEqual({ undo: null, redo: "set cutoff" });
    const redone = history.takeRedo();
    if (!redone) throw new Error("no step");
    history.keptRedone(redone);
    expect(history.state()).toEqual({ undo: "set cutoff", redo: null });
  });

  test("forgets what could be redone when something is recorded", () => {
    const history = new History();
    history.record(lock("set cutoff", false, true, null, 0));
    history.record(lock("set mix", false, true, null, 5));
    const undone = history.takeUndo();
    if (!undone) throw new Error("no step");
    history.keptUndone(undone);
    expect(history.state().redo).toBe("set mix");
    history.record(lock("set volume", false, true, null, 9));
    expect(history.state()).toEqual({ undo: "set volume", redo: null });
  });

  test("makes one step of changes of one thing that follow each other closely: it goes back to before the first and does what the last did", () => {
    const history = new History();
    history.record(stepOf("set cutoff", { type: "setLocked", locked: false }, { type: "setLocked", locked: true }, knob(3), 0));
    history.record(stepOf("set cutoff", { type: "setLocked", locked: true }, { type: "setLocked", locked: false }, knob(3), 0.016));
    history.record(stepOf("set cutoff", { type: "setLocked", locked: false }, { type: "setLocked", locked: true }, knob(3), 0.032));
    const only = history.takeUndo();
    expect([only?.undo, only?.redo]).toEqual([[{ type: "setLocked", locked: false }], [{ type: "setLocked", locked: true }]]);
    expect(history.takeUndo()).toBeUndefined();
  });

  test("keeps a drag one step for as long as no change is more than a second after the one before it", () => {
    const history = new History();
    for (let frame = 0; frame < 10; frame += 1) history.record(lock("set cutoff", false, true, knob(3), 0.9 * frame));
    expect(history.takeUndo()).toBeDefined();
    expect(history.takeUndo()).toBeUndefined();
  });

  test("makes a step of its own of a change a second after the last of its kind, and not of one exactly a second after", () => {
    const history = new History();
    history.record(lock("set cutoff", false, true, knob(3), 0));
    history.record(lock("set cutoff", true, false, knob(3), HISTORY_SAME_STEP));
    expect(history.undoable).toBe(1);
    history.record(lock("set cutoff", false, true, knob(3), HISTORY_SAME_STEP + 1.001));
    expect(history.undoable).toBe(2);
  });

  test("makes steps of their own of other things, of the first thing again after another, and of a change that has no key", () => {
    const history = new History();
    history.record(lock("set cutoff", false, true, knob(3), 0));
    history.record(lock("set resonance", false, true, knob(4), 0.01));
    history.record(lock("set cutoff", false, true, knob(3), 0.02));
    expect(history.undoable).toBe(3);
    history.record(lock("rename a group", false, true, null, 0.03));
    history.record(lock("rename a group", false, true, null, 0.04));
    expect(history.undoable).toBe(5);
  });

  test("does not merge a change into what was undone", () => {
    const history = new History();
    history.record(lock("set cutoff", false, true, knob(3), 0));
    const undone = history.takeUndo();
    if (!undone) throw new Error("no step");
    history.keptUndone(undone);
    history.record(lock("set cutoff", false, true, knob(3), 0.01));
    expect([history.undoable, history.redoable]).toEqual([1, 0]);
  });

  test("forgets everything on a clear, and says whether there was something, a step to redo only included", () => {
    const history = new History();
    expect(history.clear()).toBe(false);
    history.record(lock("set cutoff", false, true, null, 0));
    const undone = history.takeUndo();
    if (!undone) throw new Error("no step");
    history.keptUndone(undone);
    expect([history.undoable, history.redoable]).toEqual([0, 1]);
    expect(history.clear()).toBe(true);
    expect(history.state()).toEqual({ undo: null, redo: null });
    history.record(lock("set cutoff", false, true, null, 0));
    history.record(lock("set mix", false, true, null, 5));
    expect(history.clear()).toBe(true);
    expect(history.undoable).toBe(0);
  });

  test("lets the oldest steps go when there are more than it keeps", () => {
    const history = new History();
    for (let n = 0; n < HISTORY_MAX_STEPS + 5; n += 1) history.record(lock(`step ${String(n)}`, false, true, null, 5 * n));
    expect(history.undoable).toBe(HISTORY_MAX_STEPS);
    expect(history.state().undo).toBe(`step ${String(HISTORY_MAX_STEPS + 4)}`);
    let oldest: string | undefined;
    for (let step = history.takeUndo(); step; step = history.takeUndo()) oldest = step.label;
    expect(oldest).toBe("step 5");
  });
});

describe("what the name of a group is put back to", () => {
  const snapshot = historyFixture.snapshot;
  const [named, unnamed] = snapshot.graph.nodes.flatMap((node) => (node.kind === "group" ? [node] : []));
  if (!named || !unnamed) throw new Error("the fixture has two groups");
  const rename = (node: number): Intent => ({ type: "renameGroup", node, name: "Strings" });
  /** The intents that put back what `intent` changes in `from`, or none if it is no step. */
  const undoOf = (from: Snapshot, intent: Intent): Intent[] => {
    const plan = planOf(from, intent, 0);
    return typeof plan === "object" ? plan.step.undo : [];
  };

  test("is its name, for a group that has been given one", () => {
    expect(named.name).toBe("Pad");
    expect(undoOf(snapshot, rename(named.id))).toEqual([{ type: "renameGroup", node: named.id, name: "Pad" }]);
  });

  test("is none, for a group that is called by its place, which it is called by again wherever that is by then", () => {
    expect(unnamed.name).toBe("Group 2");
    expect(undoOf(snapshot, rename(unnamed.id))).toEqual([{ type: "renameGroup", node: unnamed.id, name: "" }]);
  });

  test("is its name, for a group that was given the name that another place has by default", () => {
    const nodes = snapshot.graph.nodes.map((node) => (node.id === unnamed.id ? { ...node, name: "Group 1" } : node));
    expect(undoOf({ ...snapshot, graph: { ...snapshot.graph, nodes } }, rename(unnamed.id))).toEqual([{ type: "renameGroup", node: unnamed.id, name: "Group 1" }]);
  });
});

// -- the mock engine's, against what the controller makes of the same scripts --

/** A value as a string whatever the order its keys were made in: the controller writes them as it likes, and the mock in its own order. */
function canonical(value: unknown): string {
  const sorted = (v: unknown): unknown => {
    if (Array.isArray(v)) return v.map(sorted);
    if (typeof v === "object" && v !== null) return Object.fromEntries(Object.entries(v).sort(([a], [b]) => a.localeCompare(b)).map(([key, inner]) => [key, sorted(inner)]));
    return v;
  };
  return JSON.stringify(sorted(value));
}

/** What the mock does with a script: for each op the same as the fixture's row says, or what differs. */
function run(snapshot: Snapshot, rows: readonly HistoryRow[]): string[] {
  let seconds = 0;
  const server = new MockServer({ snapshot, clock: () => seconds });
  const wrong: string[] = [];
  rows.forEach((row, at) => {
    if ("wait" in row) {
      seconds += row.wait / 1000;
      return;
    }
    const what = `${String(at)}: ${JSON.stringify(row.intent)}`;
    let events: readonly Event[] = [];
    let restructured = false;
    let error: string | null = null;
    try {
      const told = server.apply(row.intent);
      events = told.events;
      restructured = told.restructured;
    } catch (thrown) {
      error = thrown instanceof Error ? thrown.message : String(thrown);
    }
    if ("error" in row) {
      if (error !== row.error) wrong.push(`${what}: ${error ?? "done"}, and the controller says ${row.error}`);
    } else if (error !== null) {
      wrong.push(`${what}: refused (${error}), and the controller took it`);
    } else {
      if (canonical(events) !== canonical(row.events)) wrong.push(`${what}: events ${canonical(events)}, not ${canonical(row.events)}`);
      if (restructured !== row.restructured) wrong.push(`${what}: restructured ${String(restructured)}, not ${String(row.restructured)}`);
    }
    if (canonical(server.history) !== canonical(row.history)) wrong.push(`${what}: history ${canonical(server.history)}, not ${canonical(row.history)}`);
  });
  return wrong;
}

describe("the mock engine's history", () => {
  test("is what the controller makes of each script of its fixture: the events of each op with their revisions, whether the structure changed, the words of what Undo and Redo would do, and the words of a refusal", () => {
    const wrong = historyFixture.scripts.flatMap((script) => run(historyFixture.snapshot, script.rows).map((line) => `${script.name} / ${line}`));
    expect(wrong).toEqual([]);
  });

  test("has been tried with every kind of change the history knows, with each of what it cannot undo yet, and with the refusals of Undo and Redo: the fixture is not thin", () => {
    const rows = historyFixture.scripts.flatMap((script) => script.rows);
    const intents = new Set(rows.flatMap((row) => ("intent" in row ? [row.intent.type] : [])));
    const undoable: Intent["type"][] = ["setParam", "setGain", "setModulatorField", "setEdgeDepth", "setRange", "setNodeUi", "setPinned", "renameGroup", "setScale", "setLocked", "moveEffect"];
    const barriers: Intent["type"][] = ["loadPreset", "addGroup", "setLaneGroup", "removeEdge", "addModulator", "addModEdge", "removeNode", "addEffect", "addInstrument", "replaceInstrument"];
    const kinds: Intent["type"][] = [...undoable, ...barriers, "undo", "redo"];
    for (const type of kinds) expect(intents.has(type), type).toBe(true);
    const refusals = new Set(rows.flatMap((row) => ("error" in row ? [row.error] : [])));
    expect(refusals).toContain("there is nothing to undo");
    expect(refusals).toContain("there is nothing to redo");
  });
});
