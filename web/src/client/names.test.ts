import { describe, expect, test } from "vitest";

import type { Graph, Node } from "../protocol";
import { reachFixture, tracerSnapshot } from "./fixtures";
import { displayNames } from "./names";

const lfo = (id: number, parent: number): Node => ({ id, parent, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 1 } });

describe("what the nodes are called", () => {
  test("a node that is alone with its name keeps it", () => {
    expect([...displayNames(tracerSnapshot.graph)]).toEqual([
      [0, "Oscillator"],
      [1, "Filter"],
      [2, "LFO"],
      [4294967295, "Master"],
    ]);
  });

  test("a name that more than one node has is numbered from 1, in the order of the graph", () => {
    const graph: Graph = { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, lfo(7, 0), lfo(8, 0)] };
    const names = displayNames(graph);
    expect([2, 4294967295, 7, 8].map((id) => names.get(id))).toEqual(["LFO 1", "Master", "LFO 2", "LFO 3"]);
    expect(names.get(0)).toBe("Oscillator");
  });

  test("a number is not one that another node has taken as its own name", () => {
    // Two LFOs, and a plugin that is called "LFO 1" (it is alone with that name, so it keeps it): the LFOs are not
    // given the number 1, which would make two nodes "LFO 1".
    const named = (id: number, name: string): Node => ({ ...lfo(id, 0), name });
    const graph: Graph = { ...tracerSnapshot.graph, nodes: [named(10, "LFO"), named(11, "LFO"), named(12, "LFO 1")] };
    const names = displayNames(graph);
    expect([10, 11, 12].map((id) => names.get(id))).toEqual(["LFO 2", "LFO 3", "LFO 1"]);
    // A name that two nodes have is not one that either keeps: the numbers are not skipped for it.
    const both: Graph = { ...tracerSnapshot.graph, nodes: [named(10, "LFO"), named(11, "LFO"), named(12, "LFO 1"), named(13, "LFO 1")] };
    expect([10, 11, 12, 13].map((id) => displayNames(both).get(id))).toEqual(["LFO 1", "LFO 2", "LFO 1 1", "LFO 1 2"]);
  });

  test("gives no two nodes the same name, whatever the names are", () => {
    // Every list of up to six nodes whose names come from a few that look like each other's numbered forms.
    const words = ["LFO", "LFO 1", "LFO 2", "LFO 1 1"];
    const lists: string[][] = [[]];
    for (let size = 0; size < 6; size++) {
      for (const list of lists.filter((l) => l.length === size)) for (const word of words) lists.push([...list, word]);
    }
    for (const list of lists) {
      const graph: Graph = { ...tracerSnapshot.graph, nodes: list.map((name, i) => ({ ...lfo(100 + i, 0), name })) };
      const names = [...displayNames(graph).values()];
      expect(new Set(names).size, `${JSON.stringify(list)} became ${JSON.stringify(names)}`).toBe(names.length);
    }
  });

  test("gives no two nodes of a session the same name", () => {
    const names = [...displayNames(reachFixture.snapshot.graph).values()];
    expect(new Set(names).size).toBe(names.length);
    expect(names).toContain("Oscillator 4");
  });
});
