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

  test("gives no two nodes of a session the same name", () => {
    const names = [...displayNames(reachFixture.snapshot.graph).values()];
    expect(new Set(names).size).toBe(names.length);
    expect(names).toContain("Oscillator 4");
  });
});
