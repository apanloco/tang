import { describe, expect, test } from "vitest";

import type { Graph, Node } from "../protocol";
import { reachFixture, tracerSnapshot } from "./fixtures";
import { displayNames } from "./names";
import { drivablesOf, reachFrom } from "./reach";

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

describe("what a modulator can still be drawn to", () => {
  const tracer = tracerSnapshot.graph;
  const names = displayNames(tracer);
  const paramNames = (graph: Graph, id: number): string[] => {
    const node = graph.nodes.find((n) => n.id === id);
    return (node?.kind === "instrument" || node?.kind === "effect" ? node.plugin?.params : undefined)?.map((p) => p.name) ?? [];
  };

  test("is every parameter of every plugin it reaches, grouped by plugin and named as the canvas names it, except what it drives already", () => {
    const groups = drivablesOf(tracer, 2, names);
    expect(groups.map((group) => [group.node, group.name])).toEqual([[0, "Oscillator"], [1, "Filter"]]);
    expect(groups[0]?.params.map((p) => p.name)).toEqual(paramNames(tracer, 0));
    // The cutoff is driven by this LFO already.
    expect(groups[1]?.params.map((p) => p.name)).toEqual(["resonance", "type", "drive"]);
    // A parameter is told by the plugin's own index.
    expect(groups[1]?.params.map((p) => p.index)).toEqual([1, 2, 3]);
  });

  test("keeps a parameter that another modulator drives: two can drive one", () => {
    const second: Node = { id: 7, parent: 0, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 1 } };
    const graph: Graph = { ...tracer, nodes: [...tracer.nodes, second] };
    const groups = drivablesOf(graph, 7, displayNames(graph));
    expect(groups.find((group) => group.node === 1)?.params.map((p) => p.name)).toEqual(["cutoff", "resonance", "type", "drive"]);
  });

  test("leaves out a plugin with nothing left to drive, and says nothing for a modulator that reaches nothing", () => {
    const filter = tracer.nodes.find((n) => n.id === 1);
    if (filter?.kind !== "effect") throw new Error("the tracer's filter is not an effect");
    const cables = filter.plugin.params.map((param, i) => ({
      kind: "modulation" as const,
      id: 10 + i,
      amount: 0.5,
      from: { node: 2, port: { type: "modOut" as const } },
      to: { node: 1, port: { type: "param" as const, index: param.index } },
    }));
    const driven: Graph = { ...tracer, edges: [...tracer.edges.filter((e) => e.kind !== "modulation"), ...cables] };
    expect(drivablesOf(driven, 2, displayNames(driven)).map((group) => group.node)).toEqual([0]);
    // The reach fixture's lane with nothing in it: its LFO reaches nothing.
    const fixture = reachFixture.snapshot.graph;
    expect(drivablesOf(fixture, 17, displayNames(fixture))).toEqual([]);
    // And what is no modulator reaches nothing.
    expect(drivablesOf(tracer, 0, names)).toEqual([]);
  });

  test("is what the controller lets be drawn to, whole plugin by whole plugin (the fixture says which plugins)", () => {
    const { graph } = reachFixture.snapshot;
    const all = displayNames(graph);
    for (const [id, plugins] of Object.entries(reachFixture.reach)) {
      expect(drivablesOf(graph, Number(id), all).map((group) => group.node), `modulator ${id}`).toEqual(
        graph.nodes.filter((node) => plugins.includes(node.id)).map((node) => node.id),
      );
    }
  });
});
