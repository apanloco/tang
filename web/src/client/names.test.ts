import { describe, expect, test } from "vitest";

import type { Edge, Endpoint, Graph, Node } from "../protocol";
import { builtinEffects, reachFixture, tracerSnapshot } from "./fixtures";
import { cableWords, displayNames, endLabel, portName } from "./names";

const lfo = (id: number, parent: number): Node => ({ id, parent, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 1, retrigger: false, fade: 0, sync: 0 } });

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

describe("what a lane that only holds effects is called", () => {
  const lane = (id: number, over: Partial<Extract<Node, { kind: "instrument" }>> = {}): Node => ({
    id,
    parent: null,
    name: "(empty)",
    kind: "instrument",
    plugin: null,
    range: null,
    transpose: 0,
    volume: 1,
    muted: false,
    solo: false,
    route: "out",
    pattern: null,
    ...over,
  });
  const filter = (id: number, parent: number, name = "Filter"): Node => {
    const found = tracerSnapshot.graph.nodes.find((node) => node.kind === "effect");
    if (found?.kind !== "effect") throw new Error("the tracer has an effect");
    return { ...found, id, parent, name };
  };
  const master = tracerSnapshot.graph.nodes.find((node) => node.kind === "master");
  if (!master) throw new Error("the tracer has a master");

  test("it is called by the first of its effects, with the number that effect has, and takes no number of its own", () => {
    const graph: Graph = { nodes: [lane(1), filter(2, 1), filter(3, 1, "Reverb"), lane(4), filter(5, 4), master], edges: [] };
    const names = displayNames(graph);
    expect([1, 2, 3, 4, 5].map((id) => names.get(id))).toEqual(["Filter 1", "Filter 1", "Reverb", "Filter 2", "Filter 2"]);
    // (The map lists the nodes in the order of the graph, whatever they are called.)
    expect([...names.keys()]).toEqual([1, 2, 3, 4, 5, master.id]);
  });

  test("the lanes that are drawn are numbered without it: an empty lane is alone with its name when the others are holders", () => {
    const graph: Graph = { nodes: [lane(1), filter(2, 1), lane(3), filter(4, 3), lane(5), master], edges: [] };
    expect(displayNames(graph).get(5)).toBe("(empty)");
    // A lane that has a key range is drawn: two of them are numbered, the holder is not one of them.
    const drawn: Graph = { nodes: [lane(1), filter(2, 1), lane(3, { range: { low: 1, high: 2 } }), lane(5, { range: { low: 3, high: 4 } }), master], edges: [] };
    const names = displayNames(drawn);
    expect([3, 5].map((id) => names.get(id))).toEqual(["(empty) 1", "(empty) 2"]);
  });

  test("a lane that holds effects and is in a group is drawn, and called as the server calls it", () => {
    const grouped = lane(1, { parent: 7 });
    const group: Node = { id: 7, parent: null, name: "Pad", kind: "group", volume: 1, pan: 0, muted: false, solo: false, route: "out" };
    const graph: Graph = { nodes: [group, grouped, filter(2, 1), master], edges: [] };
    expect(displayNames(graph).get(1)).toBe("(empty)");
  });
});

describe("what the ports a cable ends at are called", () => {
  const { graph } = tracerSnapshot;
  const filter = graph.nodes.find((node) => node.name === "Filter");
  const cutoff = filter?.kind === "effect" ? filter.plugin.params.find((param) => param.name === "cutoff") : undefined;

  test("a parameter is called what its plugin calls it, by the plugin's own index", () => {
    expect(cutoff).toBeDefined();
    expect(portName(graph, filter?.id ?? -1, { type: "param", index: cutoff?.index ?? -1 })).toBe("cutoff");
  });

  test("a parameter the node has not, or a node that has no plugin, or none, has no name", () => {
    expect(portName(graph, filter?.id ?? -1, { type: "param", index: 999 })).toBeNull();
    expect(portName(graph, 2, { type: "param", index: 0 })).toBeNull(); // the LFO has no plugin
    expect(portName(graph, 999, { type: "param", index: 0 })).toBeNull();
  });

  test("a setting of a modulator, and the depth of a cable, are called what they are", () => {
    for (const type of ["rate", "attack", "decay", "sustain", "release"] as const) expect(portName(graph, 2, { type })).toBe(type);
    expect(portName(graph, 2, { type: "depth", edge: 0 })).toBe("depth");
  });

  test("the ports that no cable ends at have no name", () => {
    for (const type of ["modOut", "audioIn", "audioInSide", "audioOut"] as const) expect(portName(graph, 2, { type })).toBeNull();
  });

  test("the two inputs of an effect that has a second one are called what its plugin calls them, and an effect that has one, a node that is not an effect or none has no name for either", () => {
    const math = { id: 20, parent: 0, ...builtinEffects["builtin:math"] } as Node;
    const filter = { id: 21, parent: 0, ...builtinEffects["builtin:filter"] } as Node;
    const withEffects: Graph = { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, math, filter] };
    expect(portName(withEffects, 20, { type: "audioIn" })).toBe("A");
    expect(portName(withEffects, 20, { type: "audioInSide" })).toBe("B");
    for (const type of ["audioIn", "audioInSide"] as const) {
      expect(portName(withEffects, 21, { type }), `an effect that has one input: ${type}`).toBeNull();
      expect(portName(withEffects, 0, { type }), `an oscillator: ${type}`).toBeNull();
      expect(portName(withEffects, 999, { type }), `no node: ${type}`).toBeNull();
    }
    // The words of a cable say which input it ends at, where there are two.
    const names = displayNames(withEffects);
    expect(endLabel(withEffects, names, { node: 20, port: { type: "audioInSide" } })).toBe("Math: B");
    expect(cableWords(withEffects, names, { from: { node: 0, port: { type: "audioOut" } }, to: { node: 20, port: { type: "audioIn" } } })).toBe("Oscillator → Math: A");
    expect(cableWords(withEffects, names, { from: { node: 0, port: { type: "audioOut" } }, to: { node: 21, port: { type: "audioIn" } } })).toBe("Oscillator → Filter 2");
  });
});

describe("what a cable joins, in words", () => {
  const { graph } = tracerSnapshot;
  const names = displayNames(graph);
  // The tracer's cable: the LFO (2) to the cutoff of the filter (1).
  const tracerCable = graph.edges.find((edge) => edge.kind === "modulation");
  const envelope: Node = { id: 7, parent: 0, name: "ADSR", kind: "modulator", source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 } };
  const cable = (id: number, from: number, to: Endpoint): Edge => ({ kind: "modulation", id, amount: 0.5, from: { node: from, port: { type: "modOut" } }, to });
  const withCables = (...edges: Edge[]): Graph => ({ ...graph, nodes: [...graph.nodes, envelope], edges: [...graph.edges, ...edges] });
  const words = (g: Graph, edge: Edge | undefined): string => (edge ? cableWords(g, displayNames(g), edge) : "no such cable");

  test("the cable of the tracer is its modulator, an arrow, the node and the parameter it drives", () => {
    expect(tracerCable).toBeDefined();
    expect(words(graph, tracerCable)).toBe("LFO → Filter: cutoff");
    expect(endLabel(graph, names, tracerCable?.to ?? { node: -1, port: { type: "modOut" } })).toBe("Filter: cutoff");
  });

  test("a setting of a modulator is the modulator, a colon and the setting", () => {
    const g = withCables(cable(5, 7, { node: 2, port: { type: "rate" } }));
    expect(words(g, g.edges.at(-1))).toBe("ADSR → LFO: rate");
  });

  test("the depth of a cable is the words of that cable: the cable of a cable is read from the one it drives out", () => {
    const g = withCables(cable(5, 7, { node: 2, port: { type: "depth", edge: 0 } }));
    expect(words(g, g.edges.at(-1))).toBe("ADSR → depth of LFO → Filter: cutoff");
    expect(endLabel(g, displayNames(g), { node: 2, port: { type: "depth", edge: 0 } })).toBe("depth of LFO → Filter: cutoff");
  });

  test("the depth of a cable that drives a depth in turn is the words of every cable down to the plugin", () => {
    const g = withCables(cable(5, 7, { node: 2, port: { type: "depth", edge: 0 } }), cable(6, 2, { node: 7, port: { type: "depth", edge: 5 } }));
    expect(words(g, g.edges.at(-1))).toBe("LFO → depth of ADSR → depth of LFO → Filter: cutoff");
  });

  test("cables that drive each other's depths are not gone round for ever: a cable that comes again is called after the node it ends at", () => {
    // The LFO's cable 5 drives the depth of the envelope's cable 6, which drives the depth of cable 5.
    const g = withCables(cable(5, 2, { node: 7, port: { type: "depth", edge: 6 } }), cable(6, 7, { node: 2, port: { type: "depth", edge: 5 } }));
    expect(words(g, g.edges.at(-2))).toBe("LFO → depth of ADSR → LFO: depth");
    expect(words(g, g.edges.at(-1))).toBe("ADSR → depth of LFO → ADSR: depth");
  });

  test("a cable that is not made yet has no id to be read from, and is told as one that is", () => {
    const g = withCables();
    const to: Endpoint = { node: 2, port: { type: "depth", edge: 0 } };
    expect(cableWords(g, displayNames(g), { from: { node: 7, port: { type: "modOut" } }, to })).toBe("ADSR → depth of LFO → Filter: cutoff");
  });

  test("the depth of a cable that is not there is called after the node it is the port of", () => {
    expect(endLabel(graph, names, { node: 2, port: { type: "depth", edge: 999 } })).toBe("LFO: depth");
    expect(endLabel(graph, names, { node: 999, port: { type: "depth", edge: 999 } })).toBe("?: depth");
  });

  test("a port that is not one a cable ends at is the node alone, and a node that is not there is a question mark", () => {
    expect(endLabel(graph, names, { node: 2, port: { type: "modOut" } })).toBe("LFO");
    expect(endLabel(graph, names, { node: 1, port: { type: "param", index: 999 } })).toBe("Filter");
    expect(words(graph, cable(9, 999, { node: 2, port: { type: "rate" } }))).toBe("? → LFO: rate");
  });
});
