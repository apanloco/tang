import { describe, expect, test } from "vitest";

import { HISTORY_MAX_STEPS, HISTORY_SAME_STEP } from "../protocol";
import type { Edge, Event, Intent, Node, Snapshot } from "../protocol";
import { builtinCatalog, groupsFixture, historyFixture, shapeOf } from "./fixtures";
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
    if ("added" in planned) return planned.added.label;
    return "step" in planned ? planned.step.label : planned.removed.label;
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

describe("what a removal is to the history", () => {
  const snapshot = historyFixture.snapshot;
  const cables = snapshot.graph.edges.flatMap((e) => (e.kind === "modulation" ? [e] : []));
  const [first] = cables;
  if (!first) throw new Error("the fixture has a cable");
  const plan = (intent: Intent, from: Snapshot = snapshot): ReturnType<typeof planOf> => planOf(from, intent, 7, builtinCatalog);

  test("of a cable is a step that is made when it is carried out, in the words of a button for it", () => {
    expect(plan({ type: "removeEdge", edge: first.id })).toEqual({ removed: { label: "remove a cable", intent: { type: "removeEdge", edge: first.id }, at: 7 } });
  });

  test("of a cable that is not there is no step, since it is refused", () => {
    expect(plan({ type: "removeEdge", edge: 9_999 })).toBe("nothing");
  });

  test("of a cable that drives the depth of a cable that goes with it, and so on round to itself, is what cannot be put back", () => {
    const [other] = cables.filter((e) => e.id !== first.id);
    if (!other) throw new Error("the fixture has two cables");
    // Each of the two drives the depth of the other.
    const edges = snapshot.graph.edges.map((e) => {
      if (e.kind !== "modulation") return e;
      if (e.id === first.id) return { ...e, to: { node: other.from.node, port: { type: "depth" as const, edge: other.id } } };
      if (e.id === other.id) return { ...e, to: { node: first.from.node, port: { type: "depth" as const, edge: first.id } } };
      return e;
    });
    const cycle = { ...snapshot, graph: { ...snapshot.graph, edges } };
    expect(plan({ type: "removeEdge", edge: first.id }, cycle)).toBe("barrier");
    expect(plan({ type: "removeEdge", edge: other.id }, cycle)).toBe("barrier");
    // (One that drives the other and not the other way round goes with the one it drives, and is put back with it.)
    const chain = { ...cycle, graph: { ...cycle.graph, edges: edges.map((e) => (e.kind === "modulation" && e.id === other.id ? { ...e, to: first.to } : e)) } };
    expect(plan({ type: "removeEdge", edge: first.id }, chain)).toEqual({ removed: { label: "remove a cable", intent: { type: "removeEdge", edge: first.id }, at: 7 } });
  });

  describe("of a modulator", () => {
    const modulators = snapshot.graph.nodes.flatMap((n) => (n.kind === "modulator" ? [n] : []));
    const lfo = modulators.find((m) => m.source.type === "lfo");
    const envelope = modulators.find((m) => m.source.type === "envelope");
    const [lane0] = snapshot.graph.nodes.flatMap((n) => (n.kind === "instrument" ? [n] : []));
    const group = snapshot.graph.nodes.find((n) => n.kind === "group");
    if (!lfo || !envelope || !lane0 || !group) throw new Error("the fixture has an LFO, an envelope, a lane and a group");

    test("is a step that is made when it is carried out, in the words of a button for the kind it is", () => {
      expect(plan({ type: "removeNode", node: lfo.id })).toEqual({ removed: { label: "remove an LFO", intent: { type: "removeNode", node: lfo.id }, at: 7 } });
      expect(plan({ type: "removeNode", node: envelope.id })).toEqual({ removed: { label: "remove an envelope", intent: { type: "removeNode", node: envelope.id }, at: 7 } });
      const control: Node = { id: 9_000, parent: lane0.id, name: "MIDI", kind: "modulator", source: { type: "midiLearn", control: null, learning: true } };
      const withControl = { ...snapshot, graph: { ...snapshot.graph, nodes: [...snapshot.graph.nodes, control] } };
      expect(plan({ type: "removeNode", node: control.id }, withControl)).toEqual({ removed: { label: "remove a MIDI control", intent: { type: "removeNode", node: control.id }, at: 7 } });
    });

    test("is no step for a node that is not there, which is refused", () => {
      expect(plan({ type: "removeNode", node: 9_999 })).toBe("nothing");
    });

    test("is what cannot be put back when a cable of it, or one that drives its settings, is one of cables that drive each other's depths", () => {
      const own = snapshot.graph.edges.flatMap((e) => (e.kind === "modulation" && e.from.node === lfo.id ? [e] : []));
      const theirs = snapshot.graph.edges.flatMap((e) => (e.kind === "modulation" && e.from.node === envelope.id ? [e] : []));
      const [mine] = own;
      const [that] = theirs;
      if (!mine || !that) throw new Error("the fixture has a cable of each");
      // The LFO's cable drives the depth of the envelope's, and the envelope's drives the depth of the LFO's.
      const edges = snapshot.graph.edges.map((e) => {
        if (e.kind !== "modulation") return e;
        if (e.id === mine.id) return { ...e, to: { node: envelope.id, port: { type: "depth" as const, edge: that.id } } };
        if (e.id === that.id) return { ...e, to: { node: lfo.id, port: { type: "depth" as const, edge: mine.id } } };
        return e;
      });
      const cycle = { ...snapshot, graph: { ...snapshot.graph, edges } };
      expect(plan({ type: "removeNode", node: lfo.id }, cycle)).toBe("barrier");
      expect(plan({ type: "removeNode", node: envelope.id }, cycle)).toBe("barrier");
    });
  });
});

describe("what a removal of an effect is to the history", () => {
  const snapshot = historyFixture.snapshot;
  const plan = (intent: Intent, from: Snapshot = snapshot): ReturnType<typeof planOf> => planOf(from, intent, 7, builtinCatalog);
  const effects = snapshot.graph.nodes.flatMap((n) => (n.kind === "effect" ? [n] : []));

  test("is a step that is made when it is carried out, in the words of its plugin, for an effect of a lane and one of a bus", () => {
    // (The fixture has a lane's two effects, and a bus's.)
    expect(effects.map((effect) => effect.name)).toEqual(expect.arrayContaining(["Reverb", "Filter"]));
    expect(effects.length).toBeGreaterThanOrEqual(3);
    for (const effect of effects) {
      expect(plan({ type: "removeNode", node: effect.id })).toEqual({ removed: { label: `remove ${effect.name}`, intent: { type: "removeNode", node: effect.id }, at: 7 } });
    }
  });
});

describe("what a removal of a lane is to the history", () => {
  const snapshot = historyFixture.snapshot;
  const plan = (intent: Intent, from: Snapshot = snapshot): ReturnType<typeof planOf> => planOf(from, intent, 7, builtinCatalog);
  const lanes = snapshot.graph.nodes.flatMap((n) => (n.kind === "instrument" ? [n] : []));

  test("is a step that is made when it is carried out, in the words of its instrument", () => {
    expect(lanes.length).toBeGreaterThanOrEqual(2);
    for (const lane of lanes) {
      expect(plan({ type: "removeNode", node: lane.id })).toEqual({ removed: { label: `remove the lane of ${lane.name}`, intent: { type: "removeNode", node: lane.id }, at: 7 } });
    }
  });

  test("is a step in the words of a lane that has no instrument", () => {
    const [lane] = lanes;
    if (!lane) throw new Error("the fixture has a lane");
    const nodes = snapshot.graph.nodes.map((n) => (n.id === lane.id && n.kind === "instrument" ? { ...n, name: "(empty)", plugin: null } : n));
    const empty = { ...snapshot, graph: { ...snapshot.graph, nodes } };
    expect(plan({ type: "removeNode", node: lane.id }, empty)).toEqual({ removed: { label: "remove an empty lane", intent: { type: "removeNode", node: lane.id }, at: 7 } });
  });
});

describe("what putting an instrument in a lane is to the history", () => {
  const snapshot = historyFixture.snapshot;
  const plan = (intent: Intent, from: Snapshot = snapshot): ReturnType<typeof planOf> => planOf(from, intent, 7, builtinCatalog);
  const lanes = snapshot.graph.nodes.flatMap((n) => (n.kind === "instrument" ? [n] : []));
  const put = (node: number, plugin = "builtin:osc"): Intent => ({ type: "replaceInstrument", node, plugin });

  test("is a step that is made when it is carried out, in the words of the instrument that goes in", () => {
    expect(lanes.length).toBeGreaterThanOrEqual(2);
    for (const lane of lanes) {
      expect(lane.plugin).not.toBeNull();
      expect(plan(put(lane.id))).toEqual({ removed: { label: "change the instrument to Oscillator", intent: put(lane.id), at: 7 } });
    }
  });

  test("is a step in the words of a lane that has none, which is filled", () => {
    const [lane] = lanes;
    if (!lane) throw new Error("the fixture has a lane");
    const nodes = snapshot.graph.nodes.map((n) => (n.id === lane.id && n.kind === "instrument" ? { ...n, name: "(empty)", plugin: null } : n));
    const empty = { ...snapshot, graph: { ...snapshot.graph, nodes } };
    expect(plan(put(lane.id), empty)).toEqual({ removed: { label: "put Oscillator in a lane", intent: put(lane.id), at: 7 } });
  });

  test("is no step for a node that is no lane, a node that is not there or a plugin that the catalog does not name, which are refused", () => {
    const [lane] = lanes;
    const effect = snapshot.graph.nodes.find((n) => n.kind === "effect");
    if (!lane || !effect) throw new Error("the fixture has a lane and an effect");
    expect(plan(put(effect.id))).toBe("nothing");
    expect(plan(put(9_999))).toBe("nothing");
    expect(plan(put(lane.id, "builtin:nothing"))).toBe("nothing");
  });
});

describe("what a removal of a group is to the history", () => {
  const snapshot = historyFixture.snapshot;
  const plan = (intent: Intent, from: Snapshot = snapshot): ReturnType<typeof planOf> => planOf(from, intent, 7, builtinCatalog);
  const groups = snapshot.graph.nodes.flatMap((n) => (n.kind === "group" ? [n] : []));

  test("is a step that is made when it is carried out, in the words of its name", () => {
    expect(groups.length).toBeGreaterThanOrEqual(2);
    for (const group of groups) {
      expect(plan({ type: "removeNode", node: group.id })).toEqual({ removed: { label: `remove ${group.name}`, intent: { type: "removeNode", node: group.id }, at: 7 } });
    }
  });

  test("is what cannot be put back when cables of its modulators, or that drive their depths, drive each other's depths round to themselves", () => {
    // A group with two modulators whose cables drive each other's depths (the fixture has none: a file can make that, and the controller does not put it back).
    const [group] = groups;
    if (!group) throw new Error("the fixture has a group");
    const modulators: Node[] = [
      { id: 9_001, parent: group.id, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 1 } },
      { id: 9_002, parent: group.id, name: "LFO 2", kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 2 } },
    ];
    const lane = snapshot.graph.nodes.find((n) => n.kind === "instrument");
    if (!lane) throw new Error("the fixture has a lane");
    const cable = (id: number, from: number, to: Edge["to"]): Edge => ({ from: { node: from, port: { type: "modOut" } }, to, kind: "modulation", id, amount: 0.5 });
    // Cable 9101 drives the depth of 9102 and the other way round; both belong to the group's modulators.
    const edges = [
      ...snapshot.graph.edges,
      cable(9_101, 9_001, { node: 9_002, port: { type: "depth", edge: 9_102 } }),
      cable(9_102, 9_002, { node: 9_001, port: { type: "depth", edge: 9_101 } }),
    ];
    const cycle = { ...snapshot, graph: { ...snapshot.graph, nodes: [...snapshot.graph.nodes, ...modulators], edges } };
    expect(plan({ type: "removeNode", node: group.id }, cycle)).toBe("barrier");
    // (One of the two driving a place that is not a cable's depth is no cycle.)
    const chain = { ...cycle, graph: { ...cycle.graph, edges: edges.map((e) => (e.kind === "modulation" && e.id === 9_102 ? { ...e, to: { node: lane.id, port: { type: "param" as const, index: 1 } } } : e)) } };
    expect(plan({ type: "removeNode", node: group.id }, chain)).toEqual({ removed: { label: `remove ${group.name}`, intent: { type: "removeNode", node: group.id }, at: 7 } });
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

describe("the mock engine's undo of what is taken out", () => {
  const fresh = (): MockServer => new MockServer({ snapshot: historyFixture.snapshot });
  const lanes = historyFixture.snapshot.graph.nodes.flatMap((n) => (n.kind === "instrument" ? [n.id] : []));
  const [lane0] = lanes;
  const modulators = historyFixture.snapshot.graph.nodes.flatMap((n) => (n.kind === "modulator" && n.parent === lane0 ? [n] : []));
  const [lfo, envelope] = modulators;
  if (lane0 === undefined || !lfo || !envelope) throw new Error("the fixture has a lane with two modulators");
  const cableOf = (server: MockServer, modulator: number): number => {
    const found = server.snapshot.graph.edges.find((e) => e.kind === "modulation" && e.from.node === modulator);
    if (found?.kind !== "modulation") throw new Error("no cable");
    return found.id;
  };
  /** The session without its revision and its flag: what a removal that is undone has to leave as it was. */
  const pieces = (server: MockServer): string => canonical({ graph: server.snapshot.graph, telemetry: server.snapshot.telemetry });
  const labels = (server: MockServer): [string | null, string | null] => [server.history.undo, server.history.redo];

  test("puts a cable back, with the layout of the telemetry it had, by undo, and takes it out again by redo", () => {
    const server = fresh();
    const before = pieces(server);
    const edge = cableOf(server, lfo.id);
    server.apply({ type: "removeEdge", edge });
    const after = pieces(server);
    expect(after).not.toBe(before);
    expect(labels(server)).toEqual(["remove a cable", null]);
    server.apply({ type: "undo" });
    expect(pieces(server)).toBe(before);
    expect(labels(server)).toEqual([null, "remove a cable"]);
    server.apply({ type: "redo" });
    expect(pieces(server)).toBe(after);
    expect(labels(server)).toEqual(["remove a cable", null]);
    server.apply({ type: "undo" });
    expect(pieces(server)).toBe(before);
  });

  test("puts back, after what an add made has been undone and redone, the session as it was before the redo of the removal, with the slots that the redo of the add made", () => {
    const server = fresh();
    server.apply({ type: "addInstrument", plugin: "builtin:osc" });
    const lane = server.snapshot.graph.nodes.filter((n) => n.kind === "instrument").at(-1);
    if (!lane) throw new Error("the lane that was added");
    server.apply({ type: "removeNode", node: lane.id });
    server.apply({ type: "undo" });
    server.apply({ type: "undo" });
    // The lane is made again, and its meters are slots of the telemetry that it did not have the first time.
    const first = pieces(server);
    server.apply({ type: "redo" });
    const added = pieces(server);
    expect(added).not.toBe(first);
    server.apply({ type: "redo" });
    server.apply({ type: "undo" });
    expect(pieces(server), "as it was before the removal, which is how the add made it the second time").toBe(added);
  });

  test("puts an instrument back, with the cables that drove it and the layout of the telemetry it had, by undo, and puts another in again by redo", () => {
    const server = fresh();
    server.apply({ type: "setLocked", locked: true });
    const before = pieces(server);
    const cablesBefore = server.snapshot.graph.edges.filter((e) => e.kind === "modulation").length;
    server.apply({ type: "replaceInstrument", node: lane0, plugin: "builtin:osc" });
    const after = pieces(server);
    expect(after).not.toBe(before);
    expect(server.snapshot.graph.edges.filter((e) => e.kind === "modulation").length, "the cables that drove the old instrument are gone").toBeLessThan(cablesBefore);
    expect(labels(server)).toEqual(["change the instrument to Oscillator", null]);
    server.apply({ type: "undo" });
    expect(pieces(server)).toBe(before);
    expect(labels(server)).toEqual(["lock the piano to its scale", "change the instrument to Oscillator"]);
    server.apply({ type: "redo" });
    expect(pieces(server)).toBe(after);
    expect(labels(server)).toEqual(["change the instrument to Oscillator", null]);
    server.apply({ type: "undo" });
    expect(pieces(server)).toBe(before);
  });

  test("leaves the history as it was for an instrument that is refused", () => {
    const server = fresh();
    server.apply({ type: "setLocked", locked: true });
    const before = labels(server);
    expect(() => server.apply({ type: "replaceInstrument", node: lane0, plugin: "builtin:reverb" })).toThrow();
    expect(() => server.apply({ type: "replaceInstrument", node: 9_999, plugin: "builtin:osc" })).toThrow();
    expect(labels(server)).toEqual(before);
  });

  test("puts back the cables that went with it, each where it was", () => {
    const server = fresh();
    const driven = cableOf(server, lfo.id);
    // The envelope's cable drives the depth of the LFO's.
    server.apply({ type: "addModEdge", modulator: envelope.id, to: { node: lfo.id, port: { type: "depth", edge: driven } } });
    const before = pieces(server);
    const cablesBefore = server.snapshot.graph.edges.filter((e) => e.kind === "modulation").length;
    server.apply({ type: "removeEdge", edge: driven });
    expect(server.snapshot.graph.edges.filter((e) => e.kind === "modulation").length).toBe(cablesBefore - 2);
    server.apply({ type: "undo" });
    expect(pieces(server)).toBe(before);
  });

  test("is a step after the steps before it, and what was done before it is still to be undone", () => {
    const server = fresh();
    server.apply({ type: "setLocked", locked: true });
    server.apply({ type: "removeEdge", edge: cableOf(server, lfo.id) });
    expect(labels(server)).toEqual(["remove a cable", null]);
    server.apply({ type: "undo" });
    expect(labels(server)).toEqual(["lock the piano to its scale", "remove a cable"]);
  });

  test("forgets what came before cables that drive each other's depths, which are not put back", () => {
    // (A cable is made to drive a cable that is there already, so only a session that was written can have two that drive each other's.)
    const first = historyFixture.snapshot.graph.edges.find((e) => e.kind === "modulation" && e.from.node === lfo.id);
    const second = historyFixture.snapshot.graph.edges.find((e) => e.kind === "modulation" && e.from.node === envelope.id);
    if (first?.kind !== "modulation" || second?.kind !== "modulation") throw new Error("the fixture has two cables");
    const edges = historyFixture.snapshot.graph.edges.map((e) => {
      if (e.kind !== "modulation") return e;
      if (e.id === first.id) return { ...e, to: { node: envelope.id, port: { type: "depth" as const, edge: second.id } } };
      if (e.id === second.id) return { ...e, to: { node: lfo.id, port: { type: "depth" as const, edge: first.id } } };
      return e;
    });
    const server = new MockServer({ snapshot: { ...historyFixture.snapshot, graph: { ...historyFixture.snapshot.graph, edges } } });
    server.apply({ type: "setLocked", locked: true });
    expect(server.history.undo).toBe("lock the piano to its scale");
    server.apply({ type: "removeEdge", edge: first.id });
    expect(labels(server)).toEqual([null, null]);
  });

  test("leaves the history as it was for a cable that is not there", () => {
    const server = fresh();
    server.apply({ type: "setLocked", locked: true });
    const before = labels(server);
    expect(() => server.apply({ type: "removeEdge", edge: 9_999 })).toThrow();
    expect(labels(server)).toEqual(before);
  });

  test("puts a modulator back with its cables and the layout of the telemetry it had, by undo, and takes it out again by redo", () => {
    const server = fresh();
    const before = pieces(server);
    server.apply({ type: "removeNode", node: lfo.id });
    const after = pieces(server);
    expect(after).not.toBe(before);
    expect(server.snapshot.graph.nodes.some((n) => n.id === lfo.id)).toBe(false);
    expect(labels(server)).toEqual(["remove an LFO", null]);
    server.apply({ type: "undo" });
    expect(pieces(server)).toBe(before);
    expect(labels(server)).toEqual([null, "remove an LFO"]);
    server.apply({ type: "redo" });
    expect(pieces(server)).toBe(after);
    server.apply({ type: "undo" });
    expect(pieces(server)).toBe(before);
  });

  test("puts back a modulator and the cable that drove the depth of its cable, each where it was, and says what it was in its words", () => {
    const server = fresh();
    const driven = cableOf(server, lfo.id);
    server.apply({ type: "addModEdge", modulator: envelope.id, to: { node: lfo.id, port: { type: "depth", edge: driven } } });
    const before = pieces(server);
    server.apply({ type: "removeNode", node: lfo.id });
    expect(server.snapshot.graph.edges.filter((e) => e.kind === "modulation" && (e.from.node === lfo.id || e.to.node === lfo.id))).toEqual([]);
    server.apply({ type: "undo" });
    expect(pieces(server)).toBe(before);
    // The envelope has words of its own.
    server.apply({ type: "removeNode", node: envelope.id });
    expect(labels(server)).toEqual(["remove an envelope", null]);
  });

  test("leaves what was done before a removal of a modulator still to be undone, and keeps nothing for a node that is not there", () => {
    const server = fresh();
    server.apply({ type: "setLocked", locked: true });
    server.apply({ type: "removeNode", node: envelope.id });
    expect(labels(server)).toEqual(["remove an envelope", null]);
    server.apply({ type: "undo" });
    expect(labels(server)).toEqual(["lock the piano to its scale", "remove an envelope"]);
    expect(() => server.apply({ type: "removeNode", node: 9_999 })).toThrow();
    expect(labels(server)).toEqual(["lock the piano to its scale", "remove an envelope"]);
  });


  const effectNamed = (name: string, parent: number): Node => {
    const found = historyFixture.snapshot.graph.nodes.find((n) => n.kind === "effect" && n.name === name && n.parent === parent);
    if (!found) throw new Error(`the fixture has a ${name} under ${String(parent)}`);
    return found;
  };

  test("puts an effect back with the cables that drove it and its place in its chain, by undo, and takes it out again by redo", () => {
    const server = fresh();
    const filter = effectNamed("Filter", lane0);
    // The LFO drives a parameter of the filter, so that there is a cable to put back as well.
    server.apply({ type: "addModEdge", modulator: lfo.id, to: { node: filter.id, port: { type: "param", index: 1 } } });
    const before = pieces(server);
    server.apply({ type: "removeNode", node: filter.id });
    const after = pieces(server);
    expect(after).not.toBe(before);
    expect(server.snapshot.graph.nodes.some((n) => n.id === filter.id)).toBe(false);
    expect(labels(server)).toEqual(["remove Filter", null]);
    server.apply({ type: "undo" });
    expect(pieces(server)).toBe(before);
    expect(labels(server)).toEqual(["draw a cable", "remove Filter"]);
    server.apply({ type: "redo" });
    expect(pieces(server)).toBe(after);
    server.apply({ type: "undo" });
    expect(pieces(server)).toBe(before);
  });

  test("puts a lane back with its plugins, modulators and cables, in its place and its group, by undo, and takes it out again by redo", () => {
    const server = fresh();
    const [first, second] = lanes;
    if (first === undefined || second === undefined) throw new Error("the fixture has two lanes");
    const before = pieces(server);
    for (const lane of [first, second]) {
      server.apply({ type: "removeNode", node: lane });
      const after = pieces(server);
      expect(after).not.toBe(before);
      expect(server.snapshot.graph.nodes.some((n) => n.id === lane)).toBe(false);
      expect(labels(server)[0]).toMatch(/^remove the lane of /);
      server.apply({ type: "undo" });
      expect(pieces(server)).toBe(before);
      expect(labels(server)[1]).toMatch(/^remove the lane of /);
      server.apply({ type: "redo" });
      expect(pieces(server)).toBe(after);
      server.apply({ type: "undo" });
      expect(pieces(server)).toBe(before);
    }
  });

  test("puts back a member lane with the cables of its group to it, and what drives their depths, each where it was", () => {
    const server = new MockServer({ snapshot: groupsFixture.snapshot });
    // (Lane 10 is in group 0, whose modulators drive a parameter of it and the rate of its LFO; the envelope drives the depth of the first of those cables.)
    const before = pieces(server);
    server.apply({ type: "removeNode", node: 10 });
    expect(server.snapshot.graph.edges.filter((e) => e.kind === "modulation").map((e) => e.id)).toEqual([0, 4]);
    expect(labels(server)).toEqual(["remove the lane of Oscillator", null]);
    server.apply({ type: "undo" });
    expect(pieces(server)).toBe(before);
    expect(labels(server)).toEqual([null, "remove the lane of Oscillator"]);
  });

  test("puts a group back with its bus effects, its members and its cables, by undo, and takes it out again by redo", () => {
    const server = new MockServer({ snapshot: groupsFixture.snapshot });
    const before = pieces(server);
    // (Group 0 has two lanes, 3 and 10, an LFO and an envelope whose cables drive them; group 1 has the lane 9.)
    const [first, second] = groupsFixture.snapshot.graph.nodes.flatMap((n) => (n.kind === "group" ? [n.id] : []));
    if (first === undefined || second === undefined) throw new Error("the fixture has two groups");
    for (const group of [first, second]) {
      server.apply({ type: "removeNode", node: group });
      const after = pieces(server);
      expect(after).not.toBe(before);
      expect(server.snapshot.graph.nodes.some((n) => n.id === group)).toBe(false);
      expect(labels(server)[0]).toMatch(/^remove /);
      server.apply({ type: "undo" });
      expect(pieces(server)).toBe(before);
      server.apply({ type: "redo" });
      expect(pieces(server)).toBe(after);
      server.apply({ type: "undo" });
      expect(pieces(server)).toBe(before);
    }
  });

  test("does not put back the last lane of a session, which is refused and leaves the history as it was", () => {
    const server = fresh();
    for (const lane of lanes.slice(1)) server.apply({ type: "removeNode", node: lane });
    const [kept] = lanes;
    if (kept === undefined) throw new Error("the fixture has a lane");
    const steps = labels(server);
    expect(() => server.apply({ type: "removeNode", node: kept })).toThrow();
    expect(labels(server)).toEqual(steps);
  });

  test("puts back the first effect of a chain before the one after it, and an effect of a bus", () => {
    const server = fresh();
    const before = pieces(server);
    for (const effect of [effectNamed("Reverb", lane0), historyFixture.snapshot.graph.nodes.find((n) => n.kind === "effect" && n.parent !== lane0)]) {
      if (!effect) throw new Error("the fixture has the effect");
      server.apply({ type: "removeNode", node: effect.id });
      expect(labels(server)).toEqual([`remove ${effect.name}`, null]);
      server.apply({ type: "undo" });
      expect(pieces(server)).toBe(before);
    }
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
    const wrong = historyFixture.scripts.flatMap((script) => run(script.snapshot ?? historyFixture.snapshot, script.rows).map((line) => `${script.name} / ${line}`));
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
      "removeEdge",
      "addEffect",
      "addInstrument",
      "addModulator",
      "addModEdge",
      "addGroup",
      "setLaneGroup",
      "replaceInstrument",
    ];
    const barriers: Intent["type"][] = ["loadPreset", "addGroup", "setLaneGroup", "removeNode"];
    const kinds: Intent["type"][] = [...undoable, ...barriers, "undo", "redo"];
    for (const type of kinds) expect(intents.has(type), type).toBe(true);
    const refusals = new Set(rows.flatMap((row) => ("error" in row ? [row.error] : [])));
    expect(refusals).toContain("there is nothing to undo");
    expect(refusals).toContain("there is nothing to redo");
  });
});
