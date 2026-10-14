import { describe, expect, test } from "vitest";

import { HISTORY_MAX_STEPS, HISTORY_SAME_STEP } from "../protocol";
import type { Event, Intent, Node, Snapshot } from "../protocol";
import { builtinCatalog, historyFixture, shapeOf } from "./fixtures";
import type { HistoryRow } from "./fixtures";
import { History, planOf, stepOf, takesOut } from "./history";
import type { Key, Made } from "./history";
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
    const plan = planOf(from, intent, 0, builtinCatalog);
    return typeof plan === "object" && "step" in plan ? plan.step.undo : [];
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

describe("what an add is to the history", () => {
  const snapshot = historyFixture.snapshot;
  const nodeOf = (name: string): number => {
    const found = snapshot.graph.nodes.find((n) => n.name === name);
    if (!found) throw new Error(`no node called ${name}`);
    return found.id;
  };
  const [lane0, lane1] = snapshot.graph.nodes.flatMap((n) => (n.kind === "instrument" ? [n.id] : []));
  const [pad, other] = snapshot.graph.nodes.flatMap((n) => (n.kind === "group" ? [n.id] : []));
  if (lane0 === undefined || lane1 === undefined || pad === undefined || other === undefined) throw new Error("the fixture has two lanes and two groups");
  const plan = (intent: Intent, from: Snapshot = snapshot): ReturnType<typeof planOf> => planOf(from, intent, 7, builtinCatalog);
  const labelOf = (intent: Intent): string | null => {
    const planned = plan(intent);
    if (typeof planned !== "object") return null;
    return "added" in planned ? planned.added.label : planned.step.label;
  };

  test("is named in the words a button has for it", () => {
    expect(labelOf({ type: "addEffect", parent: lane0, plugin: "builtin:reverb" })).toBe("add Reverb");
    expect(labelOf({ type: "addInstrument", plugin: "builtin:osc" })).toBe("add Oscillator as an instrument");
    expect(labelOf({ type: "addModulator", parent: lane0, kind: "lfo" })).toBe("add an LFO");
    expect(labelOf({ type: "addModulator", parent: lane0, kind: "envelope" })).toBe("add an envelope");
    expect(labelOf({ type: "addModulator", parent: lane0, kind: "midiLearn" })).toBe("add a MIDI control");
    expect(labelOf({ type: "addModEdge", modulator: nodeOf("LFO"), to: { node: lane0, port: { type: "param", index: 1 } } })).toBe("draw a cable");
    expect(labelOf({ type: "addGroup", members: [] })).toBe("add a group");
    expect(labelOf({ type: "setLaneGroup", node: lane1, group: other })).toBe("put a lane in a group");
    expect(labelOf({ type: "setLaneGroup", node: lane0, group: null })).toBe("take a lane out of its group");
  });

  test("is no step for a plugin that the catalog does not name, which is refused", () => {
    expect(plan({ type: "addEffect", parent: lane0, plugin: "builtin:nothing" })).toBe("nothing");
    expect(plan({ type: "addInstrument", plugin: "builtin:nothing" })).toBe("nothing");
  });

  test("is carried by its time, and an undo is made only once it is known what was made", () => {
    const planned = plan({ type: "addGroup", members: [] });
    expect(planned).toEqual({ added: { label: "add a group", intent: { type: "addGroup", members: [] }, at: 7 } });
  });

  test("of a group made of a lane that is in another is what cannot be undone, whatever else is in the list", () => {
    expect(plan({ type: "addGroup", members: [lane0] })).toBe("barrier");
    expect(plan({ type: "addGroup", members: [lane1, lane0] })).toBe("barrier");
    expect(labelOf({ type: "addGroup", members: [lane1] })).toBe("add a group");
  });

  test("of a lane that is put in the group it is in, or taken out of none, is no change", () => {
    expect(plan({ type: "setLaneGroup", node: lane0, group: pad })).toBe("nothing");
    expect(plan({ type: "setLaneGroup", node: lane1, group: null })).toBe("nothing");
  });

  test("puts a lane back in the group it was in, and a lane that was in none in none", () => {
    const undo = (intent: Intent): Intent[] => {
      const planned = plan(intent);
      return typeof planned === "object" && "step" in planned ? planned.step.undo : [];
    };
    expect(undo({ type: "setLaneGroup", node: lane1, group: other })).toEqual([{ type: "setLaneGroup", node: lane1, group: null }]);
    expect(undo({ type: "setLaneGroup", node: lane0, group: other })).toEqual([{ type: "setLaneGroup", node: lane0, group: pad }]);
    expect(undo({ type: "setLaneGroup", node: lane0, group: null })).toEqual([{ type: "setLaneGroup", node: lane0, group: pad }]);
  });
});

describe("what takes out what an add made", () => {
  const snapshot = historyFixture.snapshot;
  const [lane0] = snapshot.graph.nodes.flatMap((n) => (n.kind === "instrument" ? [n.id] : []));
  const lfo = snapshot.graph.nodes.find((n) => n.kind === "modulator");
  const cable = snapshot.graph.edges.find((e) => e.kind === "modulation");
  if (lane0 === undefined || !lfo || cable?.kind !== "modulation") throw new Error("the fixture has a lane, a modulator and a cable");
  const addEffect: Intent = { type: "addEffect", parent: lane0, plugin: "builtin:reverb" };
  const addCable: Intent = { type: "addModEdge", modulator: lfo.id, to: { node: lane0, port: { type: "param", index: 1 } } };
  const nodes = (...ids: number[]): Made => ({ nodes: ids, edges: [] });
  const edges = (...ids: number[]): Made => ({ nodes: [], edges: ids });

  test("is the one node that was made, if it is there", () => {
    expect(takesOut(snapshot, addEffect, nodes(lfo.id))).toEqual({ type: "removeNode", node: lfo.id });
    expect(takesOut(snapshot, addEffect, nodes())).toBeNull();
    expect(takesOut(snapshot, addEffect, nodes(lfo.id, lane0))).toBeNull();
    expect(takesOut(snapshot, addEffect, nodes(9_999))).toBeNull();
  });

  test("is the one cable that was made, if it is there", () => {
    expect(takesOut(snapshot, addCable, edges(cable.id))).toEqual({ type: "removeEdge", edge: cable.id });
    expect(takesOut(snapshot, addCable, edges())).toBeNull();
    expect(takesOut(snapshot, addCable, edges(cable.id, cable.id))).toBeNull();
    expect(takesOut(snapshot, addCable, edges(9_999))).toBeNull();
  });

  test("is nothing when what was made is of the other kind, or both kinds", () => {
    expect(takesOut(snapshot, addCable, nodes(lfo.id))).toBeNull();
    expect(takesOut(snapshot, addCable, { nodes: [lfo.id], edges: [cable.id] })).toBeNull();
    expect(takesOut(snapshot, addEffect, edges(cable.id))).toBeNull();
    expect(takesOut(snapshot, addEffect, { nodes: [lfo.id], edges: [cable.id] })).toBeNull();
  });
});

describe("the mock engine's undo of what is added", () => {
  const fresh = (): MockServer => new MockServer({ snapshot: historyFixture.snapshot });
  const lanes = historyFixture.snapshot.graph.nodes.flatMap((n) => (n.kind === "instrument" ? [n.id] : []));
  const groups = historyFixture.snapshot.graph.nodes.flatMap((n) => (n.kind === "group" ? [n.id] : []));
  const [lane0, lane1] = lanes;
  const [pad, other] = groups;
  if (lane0 === undefined || lane1 === undefined || pad === undefined || other === undefined) throw new Error("the fixture has two lanes and two groups");
  /** The structure of the session without what the telemetry says (its slots are never given again). */
  const graphOf = (server: MockServer): string => canonical(server.snapshot.graph);
  const labels = (server: MockServer): [string | null, string | null] => [server.history.undo, server.history.redo];
  /** Do `intent`, and say which node it made (the one the session did not have). */
  const made = (server: MockServer, intent: Intent): Node => {
    const had = new Set(server.snapshot.graph.nodes.map((n) => n.id));
    server.apply(intent);
    const added = server.snapshot.graph.nodes.find((n) => !had.has(n.id));
    if (!added) throw new Error(`${JSON.stringify(intent)} made no node`);
    return added;
  };

  test("takes an effect out by undo and puts it back by redo under the id it had, the session as it was each time", () => {
    const server = fresh();
    const before = graphOf(server);
    server.apply({ type: "addEffect", parent: lane0, plugin: "builtin:reverb" });
    const added = graphOf(server);
    expect(labels(server)).toEqual(["add Reverb", null]);
    server.apply({ type: "undo" });
    expect(graphOf(server)).toBe(before);
    expect(labels(server)).toEqual([null, "add Reverb"]);
    server.apply({ type: "redo" });
    expect(graphOf(server)).toBe(added);
    expect(labels(server)).toEqual(["add Reverb", null]);
  });

  test("puts back what was done to an effect that was added on the effect that comes back", () => {
    const server = fresh();
    const added = made(server, { type: "addEffect", parent: lane0, plugin: "builtin:reverb" });
    server.apply({ type: "setGain", node: added.id, gain: "mix", value: 0.25 });
    server.apply({ type: "undo" });
    server.apply({ type: "undo" });
    expect(server.snapshot.graph.nodes.some((n) => n.id === added.id)).toBe(false);
    server.apply({ type: "redo" });
    server.apply({ type: "redo" });
    const back = server.snapshot.graph.nodes.find((n) => n.id === added.id);
    expect(back?.kind === "effect" ? back.mix : null).toBe(0.25);
  });

  test("takes an effect on the bus of a group out and puts it back the same way", () => {
    const server = fresh();
    const before = graphOf(server);
    server.apply({ type: "addEffect", parent: pad, plugin: "builtin:filter" });
    const added = graphOf(server);
    server.apply({ type: "undo" });
    expect(graphOf(server)).toBe(before);
    server.apply({ type: "redo" });
    expect(graphOf(server)).toBe(added);
  });

  test("takes a modulator and a cable out one at a time, and puts them back under their ids", () => {
    const server = fresh();
    const filter = server.snapshot.graph.nodes.find((n) => n.name === "Filter" && n.parent === lane0);
    if (!filter) throw new Error("no filter");
    const before = graphOf(server);
    const lfo = made(server, { type: "addModulator", parent: lane0, kind: "lfo" });
    const withModulator = graphOf(server);
    server.apply({ type: "addModEdge", modulator: lfo.id, to: { node: filter.id, port: { type: "param", index: 1 } } });
    const withCable = graphOf(server);
    expect(labels(server)).toEqual(["draw a cable", null]);
    server.apply({ type: "undo" });
    expect(graphOf(server)).toBe(withModulator);
    server.apply({ type: "undo" });
    expect(graphOf(server)).toBe(before);
    server.apply({ type: "redo" });
    expect(graphOf(server)).toBe(withModulator);
    server.apply({ type: "redo" });
    expect(graphOf(server)).toBe(withCable);
  });

  test("takes an instrument out as a lane and puts it back with its id, though the telemetry gives it new slots", () => {
    const server = fresh();
    const before = graphOf(server);
    server.apply({ type: "addInstrument", plugin: "builtin:osc" });
    const added = graphOf(server);
    const slots = JSON.stringify(server.snapshot.telemetry.meters);
    server.apply({ type: "undo" });
    expect(graphOf(server)).toBe(before);
    server.apply({ type: "redo" });
    expect(graphOf(server)).toBe(added);
    expect(JSON.stringify(server.snapshot.telemetry.meters)).not.toBe(slots);
  });

  test("takes a group out with its lanes in none, and puts it back with them in it", () => {
    const server = fresh();
    const before = graphOf(server);
    server.apply({ type: "addGroup", members: [lane1] });
    const added = graphOf(server);
    server.apply({ type: "undo" });
    expect(graphOf(server)).toBe(before);
    server.apply({ type: "redo" });
    expect(graphOf(server)).toBe(added);
  });

  test("forgets what came before a group that took a lane out of another, as the controller does", () => {
    const server = fresh();
    server.apply({ type: "setLocked", locked: true });
    expect(server.history.undo).not.toBeNull();
    server.apply({ type: "addGroup", members: [lane0] });
    expect(labels(server)).toEqual([null, null]);
  });

  test("moves a lane between groups and back, and forgets what came before a lane that leaves a group that drives it", () => {
    const server = fresh();
    const before = graphOf(server);
    server.apply({ type: "setLaneGroup", node: lane1, group: other });
    server.apply({ type: "undo" });
    expect(graphOf(server)).toBe(before);
    server.apply({ type: "setLaneGroup", node: lane0, group: other });
    server.apply({ type: "undo" });
    expect(graphOf(server)).toBe(before);
    // A group's own modulator drives the lane: the cable goes when the lane leaves, and a step does not bring it back.
    const lfo = made(server, { type: "addModulator", parent: pad, kind: "lfo" });
    server.apply({ type: "addModEdge", modulator: lfo.id, to: { node: lane0, port: { type: "param", index: 1 } } });
    expect(server.history.undo).toBe("draw a cable");
    server.apply({ type: "setLaneGroup", node: lane0, group: null });
    expect(labels(server)).toEqual([null, null]);
  });

  test("leaves the history as it was for an add that is refused", () => {
    const server = fresh();
    server.apply({ type: "setLocked", locked: true });
    const before = labels(server);
    const refused: Intent[] = [
      { type: "addEffect", parent: lane0, plugin: "builtin:nothing" },
      { type: "addEffect", parent: 9_999, plugin: "builtin:reverb" },
      { type: "addInstrument", plugin: "builtin:reverb" },
      { type: "addModulator", parent: 9_999, kind: "lfo" },
      { type: "addGroup", members: [9_999] },
      { type: "setLaneGroup", node: 9_999, group: null },
    ];
    for (const intent of refused) expect(() => server.apply(intent), JSON.stringify(intent)).toThrow();
    expect(labels(server)).toEqual(before);
  });

  test("does not give the id an undone add had to another node, and a change after the undo ends the redo", () => {
    const server = fresh();
    const first = made(server, { type: "addEffect", parent: lane0, plugin: "builtin:reverb" });
    server.apply({ type: "undo" });
    const second = made(server, { type: "addEffect", parent: lane0, plugin: "builtin:filter" });
    expect(second.id).not.toBe(first.id);
    expect(labels(server)).toEqual(["add Filter", null]);
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
      else if (row.restructured) {
        // What the session comes to: the nodes and the cables with their ids, and the layout of the telemetry.
        const shape = shapeOf(server.snapshot);
        for (const list of ["nodes", "audio", "cables", "meters", "modulation"] as const) {
          if (JSON.stringify(shape[list]) !== JSON.stringify(row.shape[list])) wrong.push(`${what}: ${list} are ${JSON.stringify(shape[list])}, not ${JSON.stringify(row.shape[list])}`);
        }
      }
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
    const undoable: Intent["type"][] = [
      "setParam",
      "setGain",
      "setModulatorField",
      "setEdgeDepth",
      "setRange",
      "setNodeUi",
      "setPinned",
      "renameGroup",
      "setScale",
      "setLocked",
      "moveEffect",
      "addEffect",
      "addInstrument",
      "addModulator",
      "addModEdge",
      "addGroup",
      "setLaneGroup",
    ];
    const barriers: Intent["type"][] = ["loadPreset", "addGroup", "setLaneGroup", "removeEdge", "removeNode", "replaceInstrument"];
    const kinds: Intent["type"][] = [...undoable, ...barriers, "undo", "redo"];
    for (const type of kinds) expect(intents.has(type), type).toBe(true);
    const refusals = new Set(rows.flatMap((row) => ("error" in row ? [row.error] : [])));
    expect(refusals).toContain("there is nothing to undo");
    expect(refusals).toContain("there is nothing to redo");
  });
});
