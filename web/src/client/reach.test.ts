import { describe, expect, test } from "vitest";

import type { Edge, Graph, Node } from "../protocol";
import { fieldsFixture, reachFixture, tracerSnapshot } from "./fixtures";
import { displayNames } from "./names";
import { drivablesOf, fieldsOf, reachFrom } from "./reach";

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
      expect(drivablesOf(graph, Number(id), all).filter((group) => group.params.length > 0).map((group) => group.node), `modulator ${id}`).toEqual(
        graph.nodes.filter((node) => plugins.includes(node.id)).map((node) => node.id),
      );
    }
  });
});

describe("which settings of other modulators a modulator can be drawn to", () => {
  const { snapshot, fields } = fieldsFixture;
  /** A node and a port as a line of text, so that lists of them can be compared as sets. */
  const lines = (found: readonly { node: number; port: object }[]): string[] =>
    found.map((f) => `${String(f.node)} ${JSON.stringify(f.port, Object.keys(f.port).sort())}`).sort();

  test("is what the controller accepts, for every modulator of a session with racks of several, in a group and out of it", () => {
    const modulators = snapshot.graph.nodes.filter((node) => node.kind === "modulator");
    expect(modulators).toHaveLength(9);
    expect(sorted(Object.keys(fields).map(Number))).toEqual(sorted(modulators.map((node) => node.id)));
    for (const modulator of modulators) {
      expect(lines(fieldsOf(snapshot.graph, modulator.id)), `${modulator.name} ${String(modulator.id)}`).toEqual(lines(fields[String(modulator.id)] ?? []));
    }
  });

  test("is not empty for most of those that have a sibling or a member, so that the agreement above says something", () => {
    const sizes = Object.values(fields).map((found) => found.length);
    expect(sizes.filter((size) => size > 0)).toHaveLength(6);
    expect(Math.max(...sizes)).toBeGreaterThanOrEqual(16);
    expect(sizes.filter((size) => size === 0)).toHaveLength(3); // the lone ones
  });

  test("is in the order of the controller's list: a group's modulator, its members' modulators first, then its siblings", () => {
    // The group's LFO (13): the first member's LFO, envelope and MIDI control, the second member's LFO, then the group's envelope and MIDI control.
    const owners = fieldsOf(snapshot.graph, 13).map((found) => found.node);
    const firsts = owners.filter((node, i) => owners.indexOf(node) === i);
    expect(firsts).toEqual([4, 5, 6, 8, 14, 15]);
    // Within one modulator: its settings, then the depths of its cables.
    expect(fieldsOf(snapshot.graph, 13).filter((found) => found.node === 5).map((found) => found.port.type)).toEqual(["attack", "decay", "sustain", "release", "depth", "depth"]);
  });

  test("is nothing for a node that is not a modulator, or not there", () => {
    for (const node of snapshot.graph.nodes.filter((n) => n.kind !== "modulator")) expect(fieldsOf(snapshot.graph, node.id), `${node.name} ${String(node.id)}`).toEqual([]);
    expect(fieldsOf(snapshot.graph, 999_999)).toEqual([]);
  });

  test("is nothing for a modulator whose lane or group is not there", () => {
    const orphan: Node = { id: 50, parent: 60, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 1 } };
    const graph: Graph = { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, orphan] };
    expect(fieldsOf(graph, 50)).toEqual([]);
    expect(fieldsOf({ ...graph, nodes: [...graph.nodes, { ...orphan, id: 51, parent: null }] }, 51)).toEqual([]);
  });

  test("is the settings its source has: an LFO's rate, an envelope's four stages, and nothing of a MIDI control", () => {
    const rack = (...sources: Node[]): Graph => ({ ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, ...sources] });
    const lfo = (id: number): Node => ({ id, parent: 0, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 1 } });
    const envelope: Node = { id: 21, parent: 0, name: "ADSR", kind: "modulator", source: { type: "envelope", attack: 0.1, decay: 0.2, sustain: 0.3, release: 0.4 } };
    const control: Node = { id: 22, parent: 0, name: "CC 74", kind: "modulator", source: { type: "midiLearn", control: { type: "cc", number: 74 }, learning: false } };
    const graph = rack(lfo(20), envelope, control);
    const types = (id: number, of: number): string[] => fieldsOf(graph, id).filter((found) => found.node === of).map((found) => found.port.type);
    expect(types(20, 21)).toEqual(["attack", "decay", "sustain", "release"]);
    expect(types(21, 20)).toEqual(["rate"]);
    expect(types(20, 22)).toEqual([]);
    // (The tracer's own LFO is in the lane too: it has its rate, and the depth of its cable.)
    expect(types(20, 2)).toEqual(["rate", "depth"]);
  });
});

describe("what a modulator can still be drawn to among the settings of the others", () => {
  const { snapshot } = fieldsFixture;
  const names = displayNames(snapshot.graph);
  /** The modulators of the groups a modulator is offered, with the settings (the cable depths are not offered). */
  const offered = (id: number) => drivablesOf(snapshot.graph, id, names).filter((group) => group.fields.length > 0).map((group) => [group.node, group.fields.map((f) => f.port.type)]);

  test("is each setting of each modulator it reaches, under the modulator's name, after the plugins; no cable's depth", () => {
    // The first member's LFO (4) can drive its envelope's four stages (5) and nothing of the MIDI control (6), which has no setting.
    expect(offered(4)).toEqual([[5, ["attack", "decay", "sustain", "release"]]]);
    // The envelope (5): the LFO's rate (4).
    expect(offered(5)).toEqual([[4, ["rate"]]]);
    // The group's LFO (13): its members' modulators, then its siblings.
    expect(offered(13)).toEqual([
      [4, ["rate"]],
      [5, ["attack", "decay", "sustain", "release"]],
      [8, ["rate"]],
      [14, ["attack", "decay", "sustain", "release"]],
    ]);
    // The settings are named by the port: `rate`, `attack`...
    const group = drivablesOf(snapshot.graph, 4, names).find((g) => g.node === 5);
    expect(group?.fields.map((f) => f.name)).toEqual(["attack", "decay", "sustain", "release"]);
    expect(group?.name).toBe(names.get(5));
    expect(group?.params).toEqual([]);
  });

  test("leaves out what it drives already, and a setting that another modulator drives is still offered", () => {
    const edge = (id: number, from: number, to: number, port: "rate" | "attack"): Edge => ({ kind: "modulation", id, amount: 0.5, from: { node: from, port: { type: "modOut" } }, to: { node: to, port: { type: port } } });
    const cabled: Graph = { ...snapshot.graph, edges: [...snapshot.graph.edges, edge(40, 4, 5, "attack"), edge(41, 6, 5, "attack")] };
    const stages = (id: number, of: number): string[] | undefined =>
      drivablesOf(cabled, id, displayNames(cabled)).find((g) => g.node === of)?.fields.map((f) => f.port.type);
    expect(stages(4, 5)).toEqual(["decay", "sustain", "release"]);
    expect(stages(6, 5)).toEqual(["decay", "sustain", "release"]);
    // The group's LFO drives neither: both of the others add up with it.
    expect(stages(13, 5)).toEqual(["attack", "decay", "sustain", "release"]);
  });

  test("leaves a modulator with nothing left out of the list, and offers nothing for what is not a modulator", () => {
    // The lone LFO of a lane has plugins to drive, and no modulator.
    expect(offered(8)).toEqual([]);
    expect(offered(10)).toEqual([]);
    expect(drivablesOf(snapshot.graph, 999_999, names)).toEqual([]);
    // A modulator that drives every setting there is leaves the other out.
    const ports = ["attack", "decay", "sustain", "release"] as const;
    const all: Graph = {
      ...snapshot.graph,
      edges: [...snapshot.graph.edges, ...ports.map((port, i): Edge => ({ kind: "modulation", id: 50 + i, amount: 0.5, from: { node: 4, port: { type: "modOut" } }, to: { node: 5, port: { type: port } } }))],
    };
    expect(drivablesOf(all, 4, displayNames(all)).filter((g) => g.fields.length > 0)).toEqual([]);
  });
});
