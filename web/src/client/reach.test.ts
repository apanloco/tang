import { describe, expect, test } from "vitest";

import type { Graph, Node } from "../protocol";
import { reachFixture, tracerSnapshot } from "./fixtures";
import { reachFrom } from "./reach";

const sorted = (nodes: Iterable<number>): number[] => [...nodes].sort((a, b) => a - b);

describe("where a cable from a modulator may end", () => {
  const { snapshot, reach } = reachFixture;

  test("is what the controller accepts, for every modulator of a session with groups and lanes in and out of them", () => {
    const modulators = snapshot.graph.nodes.filter((node) => node.kind === "modulator");
    expect(modulators.length).toBeGreaterThanOrEqual(6);
    expect(sorted(Object.keys(reach).map(Number))).toEqual(sorted(modulators.map((node) => node.id)));
    for (const modulator of modulators) {
      expect(sorted(reachFrom(snapshot.graph, modulator.id)), `${modulator.name} ${String(modulator.id)}`).toEqual(reach[String(modulator.id)]);
    }
  });

  test("is not empty for most of them, so that the agreement above says something", () => {
    const sizes = Object.values(reach).map((nodes) => nodes.length);
    expect(sizes.filter((size) => size > 0).length).toBeGreaterThanOrEqual(5);
    expect(Math.max(...sizes)).toBeGreaterThanOrEqual(4);
    expect(sizes).toContain(0);
  });

  test("is the lane's own plugins for a lane's modulator, which no other lane's or group's are", () => {
    const tracerLfo = tracerSnapshot.graph.nodes.find((node) => node.kind === "modulator");
    expect(tracerLfo).toBeDefined();
    // The tracer's one lane: the oscillator (0) and the filter (1).
    expect(sorted(reachFrom(tracerSnapshot.graph, tracerLfo?.id ?? -1))).toEqual([0, 1]);
  });

  test("is nothing for a node that is not a modulator, or not there", () => {
    for (const node of snapshot.graph.nodes.filter((n) => n.kind !== "modulator")) {
      expect(reachFrom(snapshot.graph, node.id).size, `${node.name} ${String(node.id)}`).toBe(0);
    }
    expect(reachFrom(snapshot.graph, 999_999).size).toBe(0);
  });

  test("is nothing for a modulator whose lane or group is not there", () => {
    const orphan: Node = { id: 50, parent: 60, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 1 } };
    const graph: Graph = { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, orphan] };
    expect(reachFrom(graph, 50).size).toBe(0);
    const homeless: Node = { ...orphan, id: 51, parent: null };
    expect(reachFrom({ ...graph, nodes: [...graph.nodes, homeless] }, 51).size).toBe(0);
  });
});
