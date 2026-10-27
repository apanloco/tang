import { describe, expect, test } from "vitest";

import type { Node, Snapshot } from "../protocol";
import { groupsFixture, reachFixture, tracerSnapshot } from "./fixtures";
import { moved } from "./moved";
import { stressSnapshot } from "./stress";

const modulators = (snapshot: Snapshot): Extract<Node, { kind: "modulator" }>[] => snapshot.graph.nodes.filter((node): node is Extract<Node, { kind: "modulator" }> => node.kind === "modulator");

describe("a session as the program has it once it has loaded it", () => {
  test("has every modulator of a lane or a group the session's own, hearing the lane or the group it was in", () => {
    for (const [name, snapshot] of [
      ["the tracer", tracerSnapshot],
      ["groups", groupsFixture.snapshot],
      ["reach", reachFixture.snapshot],
      ["the stress scene", stressSnapshot()],
    ] as const) {
      const was = modulators(snapshot);
      const now = modulators(moved(snapshot));
      expect(now.length, name).toBe(was.length);
      for (const modulator of now) expect(modulator.parent, `${name}: ${modulator.name}`).toBeNull();
      // Each hears what it was in: by its id, whatever the order the nodes are listed in.
      const wasIn = new Map(was.map((node) => [node.id, node.parent]));
      for (const modulator of now) {
        const parent = wasIn.get(modulator.id);
        if (parent === null) expect(modulator.hears, `${name}: ${modulator.name}`).toBeUndefined();
        else expect(modulator.hears, `${name}: ${modulator.name}`).toBe(parent);
      }
    }
  });

  test("changes nothing else: the other nodes are as they were and in the order they were, the cables, the telemetry and the revision too", () => {
    for (const snapshot of [tracerSnapshot, groupsFixture.snapshot, reachFixture.snapshot]) {
      const after = moved(snapshot);
      expect(after.graph.nodes.filter((node) => node.kind !== "modulator")).toEqual(snapshot.graph.nodes.filter((node) => node.kind !== "modulator"));
      expect(after.graph.edges).toBe(snapshot.graph.edges);
      expect(after.telemetry).toBe(snapshot.telemetry);
      expect(after.rev).toBe(snapshot.rev);
    }
  });

  test("lists them where the session's own are, before the master: the lanes' first, in the order of the lanes, and then the groups', each rack in its order", () => {
    const after = moved(groupsFixture.snapshot);
    const list = after.graph.nodes;
    const master = list.findIndex((node) => node.kind === "master");
    const own = modulators(after);
    expect(list.slice(master - own.length, master).map((node) => node.id)).toEqual(own.map((node) => node.id));
    // (The fixture's lane modulators are in lane 5 and lane 10's strips, and the groups' in group 0's and 1's.)
    const ears = own.map((node) => node.hears);
    const kinds = new Map(list.map((node) => [node.id, node.kind]));
    const lanes = ears.filter((ear) => ear !== undefined && kinds.get(ear) === "instrument");
    const groups = ears.filter((ear) => ear !== undefined && kinds.get(ear) === "group");
    expect(ears).toEqual([...lanes, ...groups]);
    expect(lanes.length).toBeGreaterThan(0);
    expect(groups.length).toBeGreaterThan(0);
    // Lanes in the order of the session.
    const laneOrder = list.filter((node) => node.kind === "instrument").map((node) => node.id);
    expect(lanes.map((ear) => laneOrder.indexOf(ear ?? -1))).toEqual([...lanes.map((ear) => laneOrder.indexOf(ear ?? -1))].sort((a, b) => a - b));
  });

  test("puts them in front of the modulators that the session has of its own already, which keep their ears and their order", () => {
    const own: Node = { id: 500, parent: null, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 1, retrigger: false, fade: 0, sync: 0 }, hears: 3 };
    const base = groupsFixture.snapshot;
    const master = base.graph.nodes.findIndex((node) => node.kind === "master");
    const withOwn: Snapshot = { ...base, graph: { ...base.graph, nodes: [...base.graph.nodes.slice(0, master), own, ...base.graph.nodes.slice(master)] } };
    const ids = modulators(moved(withOwn)).map((node) => node.id);
    expect(ids.at(-1)).toBe(500);
    expect(ids).toHaveLength(modulators(base).length + 1);
    expect(modulators(moved(withOwn)).at(-1)).toBe(own);
  });

  test("is the same snapshot for a session that has no modulator in a lane or a group", () => {
    const own = moved(tracerSnapshot);
    expect(moved(own)).toBe(own);
    const none: Snapshot = { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: tracerSnapshot.graph.nodes.filter((node) => node.kind !== "modulator"), edges: [] } };
    expect(moved(none)).toBe(none);
  });

  test("leaves a modulator whose parent is not a lane or a group where it is", () => {
    const stray: Snapshot = { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: tracerSnapshot.graph.nodes.map((node) => (node.kind === "modulator" ? { ...node, parent: 9_999 } : node)) } };
    expect(moved(stray)).toBe(stray);
  });
});
