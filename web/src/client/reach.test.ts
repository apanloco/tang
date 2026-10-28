import { describe, expect, test } from "vitest";

import type { Edge, Graph, ModulatorSource, Node, Port } from "../protocol";
import { MASTER, fieldsFixture, freeReachFixture, reachFixture, tracerSnapshot } from "./fixtures";
import { displayNames, portName } from "./names";
import { drivablesOf, driversOf, fieldsOf, gainPorts, isGainPort, modulatorsInReach, reachFrom } from "./reach";

const sorted = (nodes: Iterable<number>): number[] => [...nodes].sort((a, b) => a - b);

const LFO: ModulatorSource = { type: "lfo", waveform: "sine", rate: 1, retrigger: false, fade: 0, sync: 0 };

/** A modulator of the session's own (every modulator is, since step 96): it has no parent, and hears the lane or the group `hears`, or every note. */
const modulator = (id: number, source: ModulatorSource = LFO, hears?: number): Node => ({ id, parent: null, name: source.type === "envelope" ? "ADSR" : "LFO", kind: "modulator", source, ...(hears === undefined ? {} : { hears }) });

describe("where a cable from a modulator may end", () => {
  const { snapshot, reach } = reachFixture;

  test("is what the controller accepts, for every modulator of a session with groups and lanes in and out of them", () => {
    const modulators = snapshot.graph.nodes.filter((node) => node.kind === "modulator");
    expect(modulators.length).toBeGreaterThanOrEqual(6);
    expect(sorted(Object.keys(reach).map(Number))).toEqual(sorted(modulators.map((node) => node.id)));
    for (const one of modulators) {
      expect(sorted(reachFrom(snapshot.graph, one.id)), `${one.name} ${String(one.id)}`).toEqual(reach[String(one.id)]);
    }
  });

  test("is every plugin of the session for every modulator, whichever lane or group it hears: the instruments that have one and every effect, and nothing else", () => {
    const plugins = snapshot.graph.nodes.filter((node) => (node.kind === "instrument" && node.plugin !== null) || node.kind === "effect").map((node) => node.id);
    // (Nine: the lane that has no instrument has nothing to draw a cable to.)
    expect(plugins).toHaveLength(9);
    for (const [id, nodes] of Object.entries(reach)) expect(sorted(nodes), `modulator ${id}`).toEqual(sorted(plugins));
    // One hears a lane, one a group, one nothing in particular.
    const ears = snapshot.graph.nodes.flatMap((node) => (node.kind === "modulator" ? [node.hears] : []));
    expect(ears).toContain(undefined);
    expect(ears.filter((ear) => ear !== undefined).length).toBeGreaterThanOrEqual(5);
  });

  test("is the tracer's two plugins for its one modulator, which hears the lane", () => {
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

  test("does not look at what a modulator hears: one whose lane is gone reaches what the others do", () => {
    const orphan = modulator(50, LFO, 60);
    const graph: Graph = { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, orphan] };
    expect(sorted(reachFrom(graph, 50))).toEqual([0, 1]);
  });
});

/** The tracer's lane (the oscillator 0 and the filter 1), a group with a bus effect, a lane in it that has no instrument, and three modulators (an LFO, an envelope and a macro) that hear every note. */
function withFreeModulators(): { graph: Graph; free: Node[]; bus: Node } {
  const free: Node[] = [modulator(80), modulator(81, { type: "envelope", attack: 0.1, decay: 0.2, sustain: 0.3, release: 0.4 }), modulator(82, { type: "macro", value: 0.5 })];
  const group: Node = { id: 70, parent: null, name: "Group 1", kind: "group", volume: 1, pan: 0, muted: false, solo: false, route: "out" };
  const filter = tracerSnapshot.graph.nodes.find((node) => node.kind === "effect");
  if (filter?.kind !== "effect") throw new Error("the tracer lacks a filter");
  const bus: Node = { ...filter, id: 71, parent: group.id, name: "Filter" };
  const empty: Node = { id: 72, parent: group.id, name: "(empty)", kind: "instrument", plugin: null, range: null, transpose: 0, volume: 1, muted: false, solo: false, route: "out", pattern: null };
  const nodes = [group, bus, empty, ...tracerSnapshot.graph.nodes.filter((node) => node.kind !== "master"), ...free, ...tracerSnapshot.graph.nodes.filter((node) => node.kind === "master")];
  return { graph: { ...tracerSnapshot.graph, nodes }, free, bus };
}

describe("where a modulator may draw a cable in a session with a group and a bus", () => {
  test("is every plugin of the session: the instruments that have one and every effect, a bus's too, and not a lane that has no instrument or a node that is no plugin", () => {
    const { graph, free, bus } = withFreeModulators();
    const plugins = graph.nodes.filter((node) => (node.kind === "instrument" && node.plugin !== null) || node.kind === "effect").map((node) => node.id);
    expect(plugins).toContain(bus.id);
    expect(plugins).not.toContain(72);
    for (const one of free) expect(sorted(reachFrom(graph, one.id)), `${one.name} ${String(one.id)}`).toEqual(sorted(plugins));
    // The oscillator and the filter of the tracer's lane, and the filter on the bus.
    expect(sorted(reachFrom(graph, 80))).toEqual([0, 1, 71]);
    // And the tracer's own modulator, which hears its lane, reaches the bus as well.
    const tracerLfo = graph.nodes.find((node) => node.kind === "modulator" && node.hears === 0);
    expect(sorted(reachFrom(graph, tracerLfo?.id ?? -1))).toEqual([0, 1, 71]);
  });

  test("is the other modulators, in the order of the graph, and never itself", () => {
    const { graph } = withFreeModulators();
    const tracerLfo = graph.nodes.find((node) => node.kind === "modulator" && node.hears === 0);
    expect(modulatorsInReach(graph, 80).map((node) => node.id)).toEqual([tracerLfo?.id, 81, 82]);
    expect(modulatorsInReach(graph, 81).map((node) => node.id)).toEqual([tracerLfo?.id, 80, 82]);
    expect(modulatorsInReach(graph, tracerLfo?.id ?? -1).map((node) => node.id)).toEqual([80, 81, 82]);
    // A node that is not a modulator, and one that is not there, reach none.
    expect(modulatorsInReach(graph, 71)).toEqual([]);
    expect(modulatorsInReach(graph, 999_999)).toEqual([]);
  });

  test("lists the settings and the depths of the cables of the others that it can drive: an LFO's rate, an envelope's four stages, nothing of a macro", () => {
    const { graph } = withFreeModulators();
    const cable: Edge = { kind: "modulation", id: 500, amount: 0.5, from: { node: 81, port: { type: "modOut" } }, to: { node: 0, port: { type: "param", index: 1 } } };
    const withCable: Graph = { ...graph, edges: [...graph.edges, cable] };
    const line = (found: { node: number; port: Port }): string => `${String(found.node)} ${found.port.type}`;
    // (The tracer's LFO is the first of the others, with its rate and the depth of its cable.)
    expect(fieldsOf(withCable, 80).map(line)).toEqual(["2 rate", "2 depth", "81 attack", "81 decay", "81 sustain", "81 release", "81 depth"]);
    expect(fieldsOf(withCable, 81).map(line)).toEqual(["2 rate", "2 depth", "80 rate"]);
    expect(fieldsOf(withCable, 82).map(line)).toEqual(["2 rate", "2 depth", "80 rate", "81 attack", "81 decay", "81 sustain", "81 release", "81 depth"]);
  });

  test("is every parameter of every plugin and the settings of the others, for a list of what can still be drawn to", () => {
    const { graph } = withFreeModulators();
    const names = displayNames(graph);
    const listed = drivablesOf(graph, 80, names);
    // The parameters of the plugins and the values that the host keeps, node by node in the order of the graph (the group, which has its volume and its pan and no parameter, its bus effect, then the tracer's lane),
    // and then the settings of the others.
    expect(listed.map((group) => group.node)).toEqual([70, 71, 0, 1, 2, 81]);
    expect(listed.find((group) => group.node === 81)?.fields.map((field) => field.port.type)).toEqual(["attack", "decay", "sustain", "release"]);
    // A parameter that it drives is not offered again, and another that drives it does not take it away.
    const cable: Edge = { kind: "modulation", id: 501, amount: 0.5, from: { node: 80, port: { type: "modOut" } }, to: { node: 71, port: { type: "param", index: 0 } } };
    const driven: Graph = { ...graph, edges: [...graph.edges, cable] };
    expect(drivablesOf(driven, 80, names).find((group) => group.node === 71)?.params.some((param) => param.index === 0)).toBe(false);
    expect(drivablesOf(driven, 81, names).find((group) => group.node === 71)?.params.some((param) => param.index === 0)).toBe(true);
  });

  test("can drive what a knob asks for, from the other side: every one of them, but not one that drives it already", () => {
    const { graph, bus } = withFreeModulators();
    const tracerLfo = graph.nodes.find((node) => node.kind === "modulator" && node.hears === 0);
    // The bus effect's knob: every modulator there is, the tracer's too.
    expect(driversOf(graph, bus.id, { type: "param", index: 0 }).map((node) => node.id)).toEqual([tracerLfo?.id, 80, 81, 82]);
    // The oscillator's: the same.
    expect(driversOf(graph, 0, { type: "param", index: 0 }).map((node) => node.id)).toEqual([tracerLfo?.id, 80, 81, 82]);
    const cable: Edge = { kind: "modulation", id: 502, amount: 0.5, from: { node: 81, port: { type: "modOut" } }, to: { node: bus.id, port: { type: "param", index: 0 } } };
    expect(driversOf({ ...graph, edges: [...graph.edges, cable] }, bus.id, { type: "param", index: 0 }).map((node) => node.id)).toEqual([tracerLfo?.id, 80, 82]);
    // The rate of one of them: the others, and not itself.
    expect(driversOf(graph, 80, { type: "rate" }).map((node) => node.id)).toEqual([tracerLfo?.id, 81, 82]);
  });
});

describe("where a modulator may draw a cable, as the controller says", () => {
  const { snapshot, reach, fields } = freeReachFixture;
  const modulators = snapshot.graph.nodes.filter((node) => node.kind === "modulator");
  /** A node and a port as a line of text, so that lists of them can be compared as sets. */
  const lines = (found: readonly { node: number; port: object }[]): string[] => found.map((f) => `${String(f.node)} ${JSON.stringify(f.port, Object.keys(f.port).sort())}`).sort();

  test("is told of a session with eight modulators: five that hear a lane or a group, and three that hear every note", () => {
    expect(modulators).toHaveLength(8);
    expect(modulators.every((node) => node.parent === null)).toBe(true);
    expect(modulators.filter((node) => node.hears === undefined).map((node) => node.source.type)).toEqual(["lfo", "lfo", "envelope"]);
  });

  test("is, for the plugins, what the controller accepts, for every modulator: nine each", () => {
    expect(sorted(Object.keys(reach).map(Number))).toEqual(sorted(modulators.map((node) => node.id)));
    for (const one of modulators) {
      expect(sorted(reachFrom(snapshot.graph, one.id)), `${one.name} ${String(one.id)}`).toEqual(reach[String(one.id)]);
    }
    expect(modulators.map((node) => reach[String(node.id)]?.length)).toEqual([9, 9, 9, 9, 9, 9, 9, 9]);
  });

  test("is, for the settings of modulators and the depths of their cables, what the controller accepts, for every modulator: every other, and none of its own", () => {
    for (const one of modulators) {
      expect(lines(fieldsOf(snapshot.graph, one.id)), `${one.name} ${String(one.id)}`).toEqual(lines(fields[String(one.id)] ?? []));
      expect(fieldsOf(snapshot.graph, one.id).some((found) => found.node === one.id), `${one.name} ${String(one.id)}`).toBe(false);
    }
    // The last two (an LFO with a cable and an envelope with a cable) reach each other's: the LFO the envelope's four stages and the depth of its cable, and the envelope the LFO's rate and the depth of its cable.
    const [lfo, envelope] = modulators.slice(-2);
    if (!lfo || !envelope) throw new Error("the session has no two modulators");
    expect(fields[String(lfo.id)]?.filter((found) => found.node === envelope.id).map((found) => found.port.type)).toEqual(["attack", "decay", "sustain", "release", "depth"]);
    expect(fields[String(envelope.id)]?.filter((found) => found.node === lfo.id).map((found) => found.port.type)).toEqual(["rate", "depth"]);
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
    const second = modulator(7, LFO, 0);
    const graph: Graph = { ...tracer, nodes: [...tracer.nodes, second] };
    const groups = drivablesOf(graph, 7, displayNames(graph));
    expect(groups.find((group) => group.node === 1)?.params.map((p) => p.name)).toEqual(["cutoff", "resonance", "type", "drive"]);
  });

  test("leaves out a plugin with nothing left to drive, and says nothing for what is not a modulator", () => {
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
    const left = drivablesOf(driven, 2, displayNames(driven));
    expect(left.map((group) => group.node)).toEqual([0, 1]);
    // (What the filter has left is its mix, which the host keeps: not a parameter of the plugin.)
    expect(left.find((group) => group.node === 1)).toEqual({ node: 1, name: "Filter", params: [], fields: [{ port: { type: "mix" }, name: "mix" }] });
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

  test("is what the controller accepts, for every modulator of a session with several that hear lanes and a group", () => {
    const modulators = snapshot.graph.nodes.filter((node) => node.kind === "modulator");
    expect(modulators).toHaveLength(9);
    expect(sorted(Object.keys(fields).map(Number))).toEqual(sorted(modulators.map((node) => node.id)));
    for (const one of modulators) {
      expect(lines(fieldsOf(snapshot.graph, one.id)), `${one.name} ${String(one.id)}`).toEqual(lines(fields[String(one.id)] ?? []));
    }
  });

  test("is not empty for any of them, since each reaches the eight others, so that the agreement above says something", () => {
    const sizes = Object.values(fields).map((found) => found.length);
    expect(sizes.filter((size) => size > 0)).toHaveLength(9);
    expect(Math.max(...sizes)).toBeGreaterThanOrEqual(16);
  });

  test("is in the order of the graph: the other modulators one after the other, whatever they hear", () => {
    // The first (7): the settings and the depths of the other eight, in the order of the session's own rack.
    const owners = fieldsOf(snapshot.graph, 7).map((found) => found.node);
    const firsts = owners.filter((node, i) => owners.indexOf(node) === i);
    expect(firsts).toEqual([8, 9, 10, 11, 12, 13, 14, 15]);
    // Within one modulator: its settings, then the depths of its cables.
    expect(fieldsOf(snapshot.graph, 7).filter((found) => found.node === 8).map((found) => found.port.type)).toEqual(["attack", "decay", "sustain", "release", "depth", "depth"]);
    // The last (15) reaches the first eight, and not itself.
    expect(fieldsOf(snapshot.graph, 15).map((found) => found.node).filter((node, i, all) => all.indexOf(node) === i)).toEqual([7, 8, 9, 10, 11, 12, 13, 14]);
  });

  test("is nothing for a node that is not a modulator, or not there", () => {
    for (const node of snapshot.graph.nodes.filter((n) => n.kind !== "modulator")) expect(fieldsOf(snapshot.graph, node.id), `${node.name} ${String(node.id)}`).toEqual([]);
    expect(fieldsOf(snapshot.graph, 999_999)).toEqual([]);
  });

  test("does not look at what a modulator hears: one whose lane or group is gone reaches the others like any", () => {
    const orphan = modulator(50, LFO, 60);
    const graph: Graph = { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, orphan] };
    expect(fieldsOf(graph, 50).map((found) => `${String(found.node)} ${found.port.type}`)).toEqual(["2 rate", "2 depth"]);
  });

  test("is the settings its source has: an LFO's rate, an envelope's four stages, and nothing of a MIDI control", () => {
    const rack = (...sources: Node[]): Graph => ({ ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, ...sources] });
    const envelope = modulator(21, { type: "envelope", attack: 0.1, decay: 0.2, sustain: 0.3, release: 0.4 }, 0);
    const control = modulator(22, { type: "midiLearn", control: { type: "cc", number: 74 }, learning: false }, 0);
    const knob = modulator(23, { type: "macro", value: 0.5 }, 0);
    const sequencer = modulator(24, { type: "steps", values: Array<number>(16).fill(0), length: 16, rate: 8, glide: 0, retrigger: true, sync: 0 }, 0);
    const graph = rack(modulator(20, LFO, 0), envelope, control, knob, sequencer);
    const types = (id: number, of: number): string[] => fieldsOf(graph, id).filter((found) => found.node === of).map((found) => found.port.type);
    expect(types(20, 21)).toEqual(["attack", "decay", "sustain", "release"]);
    expect(types(21, 20)).toEqual(["rate"]);
    expect(types(20, 22)).toEqual([]);
    // A macro's value is turned by a person: no cable ends at it. It can drive the settings of the others', as any modulator can.
    expect(types(20, 23)).toEqual([]);
    expect(types(23, 20)).toEqual(["rate"]);
    expect(types(23, 21)).toEqual(["attack", "decay", "sustain", "release"]);
    // A step sequencer's rate can be driven, as an LFO's is, and its glide, length and switch are set by a person.
    expect(types(20, 24)).toEqual(["rate"]);
    expect(types(24, 20)).toEqual(["rate"]);
    expect(types(24, 21)).toEqual(["attack", "decay", "sustain", "release"]);
    // (The tracer's own LFO is there too: it has its rate, and the depth of its cable.)
    expect(types(20, 2)).toEqual(["rate", "depth"]);
  });
});

describe("what a modulator can still be drawn to among the settings of the others", () => {
  const { snapshot } = fieldsFixture;
  const names = displayNames(snapshot.graph);
  /** A cable from the modulator `from` to a port of `to`. */
  const edge = (id: number, from: number, to: number, port: Port): Edge => ({ kind: "modulation", id, amount: 0.5, from: { node: from, port: { type: "modOut" } }, to: { node: to, port } });
  /** The modulators a modulator is offered, with the settings and the depths of the cables (`depth#1` is the depth of the cable 1). */
  const offered = (id: number) =>
    drivablesOf(snapshot.graph, id, names)
      .map((group) => ({ ...group, fields: group.fields.filter((f) => !isGainPort(f.port)) }))
      .filter((group) => group.fields.length > 0)
      .map((group) => [group.node, group.fields.map((f) => (f.port.type === "depth" ? `depth#${String(f.port.edge)}` : f.port.type))]);

  test("is each setting of each other modulator and the depth of each of its cables, under the modulator's name, after the plugins", () => {
    // The first (7, an LFO that drives a parameter by the cable 0) is offered the other eight, in the order of the graph: the envelope (8, the cables 1 and 2) its four stages and the depths of
    // its two cables, the MIDI control (9, the cable 3) the depth of its cable and no setting, the three LFOs that have no cable (10, 11, 12) their rates, and so on.
    expect(offered(7)).toEqual([
      [8, ["attack", "decay", "sustain", "release", "depth#1", "depth#2"]],
      [9, ["depth#3"]],
      [10, ["rate"]],
      [11, ["rate"]],
      [12, ["rate"]],
      [13, ["rate", "depth#4"]],
      [14, ["attack", "decay", "sustain", "release", "depth#5"]],
      [15, ["depth#6"]],
    ]);
    // The envelope (8): the same, with the first LFO's rate and the depth of its cable (0) in the place of its own.
    expect(offered(8)).toEqual([
      [7, ["rate", "depth#0"]],
      [9, ["depth#3"]],
      [10, ["rate"]],
      [11, ["rate"]],
      [12, ["rate"]],
      [13, ["rate", "depth#4"]],
      [14, ["attack", "decay", "sustain", "release", "depth#5"]],
      [15, ["depth#6"]],
    ]);
    // The settings are named by the port: `rate`, `attack`...
    const group = drivablesOf(snapshot.graph, 7, names).find((g) => g.node === 8);
    expect(group?.fields.slice(0, 4).map((f) => f.name)).toEqual(["attack", "decay", "sustain", "release"]);
    expect(group?.name).toBe(names.get(8));
    expect(group?.params).toEqual([]);
  });

  test("names the depth of a cable by where the cable goes: `depth → Oscillator 1: detune`, the cable's end as the canvas says it", () => {
    const group = drivablesOf(snapshot.graph, 7, names).find((g) => g.node === 8);
    const oscillator = snapshot.graph.nodes.find((node) => node.id === 3);
    const endOf = (cable: number): string => {
      const found = snapshot.graph.edges.find((e) => e.kind === "modulation" && e.id === cable);
      return `Oscillator 1: ${portName(snapshot.graph, found?.to.node ?? -1, found?.to.port ?? { type: "modOut" }) ?? "?"}`;
    };
    expect(oscillator?.kind).toBe("instrument");
    expect(group?.fields.slice(4).map((f) => f.name)).toEqual([`depth → ${endOf(1)}`, `depth → ${endOf(2)}`]);
    expect(group?.fields.slice(4).map((f) => f.name)).not.toContain("depth → Oscillator 1: ?");
    // A cable that ends at the depth of another is told by that cable, as the canvas does (`endLabel`): the MIDI control (9) drives the depth of the first LFO's cable (0).
    const cabled: Graph = { ...snapshot.graph, edges: [...snapshot.graph.edges, edge(40, 9, 7, { type: "depth", edge: 0 })] };
    const named = displayNames(cabled);
    const seen = drivablesOf(cabled, 10, named).find((g) => g.node === 9);
    expect(seen?.fields.map((f) => f.name)).toEqual([`depth → ${endOf(3)}`, `depth → depth of ${named.get(7) ?? "?"} → ${endOf(0)}`]);
  });

  test("leaves out what it drives already, and a setting that another modulator drives is still offered", () => {
    const cabled: Graph = { ...snapshot.graph, edges: [...snapshot.graph.edges, edge(40, 7, 8, { type: "attack" }), edge(41, 9, 8, { type: "attack" })] };
    const stages = (id: number, of: number): string[] | undefined =>
      drivablesOf(cabled, id, displayNames(cabled))
        .find((g) => g.node === of)
        ?.fields.map((f) => f.port.type)
        .filter((type) => type !== "depth");
    expect(stages(7, 8)).toEqual(["decay", "sustain", "release"]);
    expect(stages(9, 8)).toEqual(["decay", "sustain", "release"]);
    // A third drives neither: both of the others add up with it.
    expect(stages(13, 8)).toEqual(["attack", "decay", "sustain", "release"]);
  });

  test("leaves out the depth of a cable that it drives already, and not the depths of the other cables of that modulator, which are other ports", () => {
    // The first LFO (7) drives the depth of the envelope's cable 1; the MIDI control (9), the depth of its cable 2.
    const cabled: Graph = { ...snapshot.graph, edges: [...snapshot.graph.edges, edge(40, 7, 8, { type: "depth", edge: 1 }), edge(41, 9, 8, { type: "depth", edge: 2 })] };
    const depthsOf = (id: number): number[] =>
      drivablesOf(cabled, id, displayNames(cabled))
        .find((g) => g.node === 8)
        ?.fields.flatMap((f) => (f.port.type === "depth" ? [f.port.edge] : [])) ?? [];
    expect(depthsOf(7)).toEqual([2]);
    expect(depthsOf(9)).toEqual([1]);
    // (Another modulator may drive the same depth: the two add up.)
    expect(depthsOf(13)).toEqual([1, 2]);
    // The settings are as they were: a cable to a depth is not one to a setting.
    const settings = drivablesOf(cabled, 7, displayNames(cabled)).find((g) => g.node === 8)?.fields.filter((f) => f.port.type !== "depth");
    expect(settings?.map((f) => f.port.type)).toEqual(["attack", "decay", "sustain", "release"]);
  });

  test("leaves a modulator with nothing left out of the list, and offers nothing for what is not a modulator", () => {
    expect(drivablesOf(snapshot.graph, 999_999, names)).toEqual([]);
    // A modulator that drives every setting there is, and every depth, leaves the others out.
    const every = fieldsOf(snapshot.graph, 7);
    expect(every.length).toBeGreaterThan(4);
    const all: Graph = { ...snapshot.graph, edges: [...snapshot.graph.edges, ...every.map((field, i) => edge(50 + i, 7, field.node, field.port))] };
    expect(drivablesOf(all, 7, displayNames(all)).flatMap((g) => g.fields.filter((f) => !isGainPort(f.port)))).toEqual([]);
  });
});

describe("which modulators can still drive a port", () => {
  const tracer = tracerSnapshot.graph;
  const ids = (nodes: readonly Node[]): number[] => nodes.map((n) => n.id);
  const param = (node: number, name: string): Port => {
    const found = tracer.nodes.find((n) => n.id === node);
    const index = (found?.kind === "instrument" || found?.kind === "effect" ? found.plugin?.params : undefined)?.find((p) => p.name === name)?.index;
    if (index === undefined) throw new Error(`no ${name}`);
    return { type: "param", index };
  };
  /** The tracer with a second LFO that hears its lane. */
  const second = modulator(7, LFO, 0);
  const withSecond: Graph = { ...tracer, nodes: [...tracer.nodes, second] };

  test("is the modulators that reach a parameter, in the order of the graph, and none that drives it already", () => {
    // The tracer's LFO (2) drives the cutoff of the filter (1) and nothing else.
    expect(ids(driversOf(tracer, 1, param(1, "resonance")))).toEqual([2]);
    expect(ids(driversOf(tracer, 1, param(1, "cutoff")))).toEqual([]);
    expect(ids(driversOf(tracer, 0, param(0, "detune")))).toEqual([2]);
  });

  test("keeps a modulator that drives another port of the node, and one that does not drive this port when another does: two can drive one", () => {
    expect(ids(driversOf(withSecond, 1, param(1, "resonance")))).toEqual([2, 7]);
    expect(ids(driversOf(withSecond, 1, param(1, "cutoff")))).toEqual([7]);
  });

  test("is every other modulator for a setting of a modulator, never the modulator itself", () => {
    const { graph } = fieldsFixture.snapshot;
    // The first (7): the other eight, in the order of the graph; not itself.
    expect(ids(driversOf(graph, 7, { type: "rate" }))).toEqual([8, 9, 10, 11, 12, 13, 14, 15]);
    // (An LFO has no attack.)
    expect(ids(driversOf(graph, 7, { type: "attack" }))).toEqual([]);
    // The envelope's attack: every other, and not the envelope.
    expect(ids(driversOf(graph, 8, { type: "attack" }))).toEqual([7, 9, 10, 11, 12, 13, 14, 15]);
  });

  test("is the depth of a cable for the modulators that reach the modulator whose cable it is, the one that drives it already left out", () => {
    const { graph } = fieldsFixture.snapshot;
    const cable = graph.edges.find((e) => e.kind === "modulation" && e.from.node === 7);
    if (cable?.kind !== "modulation") throw new Error("the first LFO has a cable");
    const port: Port = { type: "depth", edge: cable.id };
    expect(ids(driversOf(graph, 7, port))).toEqual([8, 9, 10, 11, 12, 13, 14, 15]);
    // The envelope (8) drives the depth of that cable: it is left out, and the others are not.
    const driven: Graph = { ...graph, edges: [...graph.edges, { kind: "modulation", id: 90, amount: 0.5, from: { node: 8, port: { type: "modOut" } }, to: { node: 7, port } }] };
    expect(ids(driversOf(driven, 7, port))).toEqual([9, 10, 11, 12, 13, 14, 15]);
  });

  test("is nothing for a port that no cable ends at, a parameter the plugin has not, and a node that is not there, the master, or has no such port", () => {
    for (const port of [{ type: "modOut" }, { type: "audioIn" }, { type: "audioOut" }] as const) expect(driversOf(tracer, 1, port), port.type).toEqual([]);
    expect(driversOf(tracer, 1, { type: "param", index: 9_999 })).toEqual([]);
    expect(driversOf(tracer, 999_999, param(1, "resonance"))).toEqual([]);
    expect(driversOf(tracer, MASTER, param(1, "resonance"))).toEqual([]);
    // A modulator has no parameter, and an effect no rate.
    expect(driversOf(tracer, 2, { type: "param", index: 0 })).toEqual([]);
    expect(driversOf(tracer, 1, { type: "rate" })).toEqual([]);
    expect(driversOf(tracer, 2, { type: "depth", edge: 9_999 })).toEqual([]);
  });

  test("is what `drivablesOf` offers from the other side: a modulator is listed for a port exactly when the port is offered to the modulator", () => {
    for (const { snapshot } of [reachFixture, fieldsFixture]) {
      const { graph } = snapshot;
      const names = displayNames(graph);
      const modulators = graph.nodes.filter((n) => n.kind === "modulator");
      const key = (node: number, port: Port): string => `${String(node)}:${port.type}${port.type === "param" ? String(port.index) : port.type === "depth" ? String(port.edge) : ""}`;
      const offers = new Map<string, number[]>();
      for (const one of modulators) {
        for (const group of drivablesOf(graph, one.id, names)) {
          for (const p of group.params) offers.set(key(group.node, { type: "param", index: p.index }), [...(offers.get(key(group.node, { type: "param", index: p.index })) ?? []), one.id]);
          for (const f of group.fields) offers.set(key(group.node, f.port), [...(offers.get(key(group.node, f.port)) ?? []), one.id]);
        }
      }
      // Every port there is: the parameters of every plugin, the settings of every modulator and the depth of every cable.
      const ports: [number, Port][] = [
        ...graph.nodes.flatMap((n): [number, Port][] => ((n.kind === "instrument" || n.kind === "effect") && n.plugin ? n.plugin.params.map((p): [number, Port] => [n.id, { type: "param", index: p.index }]) : [])),
        ...modulators.flatMap((m): [number, Port][] => (["rate", "attack", "decay", "sustain", "release"] as const).map((type): [number, Port] => [m.id, { type }])),
        ...graph.edges.flatMap((e): [number, Port][] => (e.kind === "modulation" ? [[e.from.node, { type: "depth", edge: e.id }]] : [])),
      ];
      expect(ports.length).toBeGreaterThan(40);
      let listed = 0;
      for (const [node, port] of ports) {
        const expected = offers.get(key(node, port)) ?? [];
        listed += expected.length;
        expect(ids(driversOf(graph, node, port)), key(node, port)).toEqual(expected);
      }
      // (So that the agreement says something.)
      expect(listed).toBeGreaterThan(40);
    }
  });
});

describe("the host-side values a cable can end at: a lane's volume, an effect's mix, a group's volume and pan", () => {
  const { snapshot, gains } = freeReachFixture;
  const modulators = snapshot.graph.nodes.filter((node) => node.kind === "modulator");
  const lines = (found: readonly { node: number; port: object }[]): string[] => found.map((f) => `${String(f.node)} ${JSON.stringify(f.port, Object.keys(f.port).sort())}`).sort();
  const expected = snapshot.graph.nodes.flatMap((node) => gainPorts(node).map((port) => ({ node: node.id, port })));

  test("are, for every modulator, what the controller accepts: the volume of each lane that has an instrument, the mix of each effect, the volume and the pan of each group", () => {
    expect(sorted(Object.keys(gains).map(Number))).toEqual(sorted(modulators.map((node) => node.id)));
    for (const one of modulators) {
      expect(lines(expected), `${one.name} ${String(one.id)}`).toEqual(lines(gains[String(one.id)] ?? []));
    }
    // (Nine plugins: the volume of the lanes that have an instrument and the mix of the effects, and a volume and a pan for each of the two groups: so that the agreement says something.)
    expect(expected.filter((found) => found.port.type === "volume")).toHaveLength(snapshot.graph.nodes.filter((n) => (n.kind === "instrument" && n.plugin !== null) || n.kind === "group").length);
    expect(expected.filter((found) => found.port.type === "mix")).toHaveLength(snapshot.graph.nodes.filter((n) => n.kind === "effect").length);
    expect(expected.filter((found) => found.port.type === "pan")).toHaveLength(snapshot.graph.nodes.filter((n) => n.kind === "group").length);
  });

  test("are not a lane's that has no instrument, nor anything of a modulator, a source or the master", () => {
    const empty = snapshot.graph.nodes.find((n) => n.kind === "instrument" && n.plugin === null);
    if (!empty) throw new Error("the session has no lane without an instrument");
    expect(gainPorts(empty)).toEqual([]);
    for (const node of snapshot.graph.nodes.filter((n) => n.kind === "modulator" || n.kind === "master" || n.kind === "source")) expect(gainPorts(node), node.name).toEqual([]);
  });

  test("are what a modulator is offered after the parameters of each plugin, named by what they are, and a group is offered its two with no parameter", () => {
    const names = displayNames(snapshot.graph);
    const [one] = modulators;
    if (!one) throw new Error("the session has no modulator");
    const offered = drivablesOf(snapshot.graph, one.id, names);
    const lane = snapshot.graph.nodes.find((n) => n.kind === "instrument" && n.plugin !== null);
    const effect = snapshot.graph.nodes.find((n) => n.kind === "effect");
    const group = snapshot.graph.nodes.find((n) => n.kind === "group");
    if (!lane || !effect || !group) throw new Error("the session lacks a lane, an effect or a group");
    // A lane's volume is its output volume, which is not the oscillator's own `volume` parameter (it is in the list too).
    expect(offered.find((g) => g.node === lane.id)?.fields.map((f) => f.name)).toEqual(["output volume"]);
    expect(offered.find((g) => g.node === lane.id)?.params.map((p) => p.name)).toContain("volume");
    expect(offered.find((g) => g.node === effect.id)?.fields.map((f) => [f.port.type, f.name])).toEqual([["mix", "mix"]]);
    expect(offered.find((g) => g.node === group.id)).toEqual({ node: group.id, name: names.get(group.id), params: [], fields: [{ port: { type: "volume" }, name: "volume" }, { port: { type: "pan" }, name: "pan" }] });
  });

  test("are offered again to a modulator that drives them, to another, but not to the same", () => {
    const names = displayNames(snapshot.graph);
    const [first, second] = modulators;
    const group = snapshot.graph.nodes.find((n) => n.kind === "group");
    if (!first || !second || !group) throw new Error("the session lacks two modulators or a group");
    const cable: Edge = { kind: "modulation", id: 700, amount: 0.5, from: { node: first.id, port: { type: "modOut" } }, to: { node: group.id, port: { type: "pan" } } };
    const graph: Graph = { ...snapshot.graph, edges: [...snapshot.graph.edges, cable] };
    expect(drivablesOf(graph, first.id, names).find((g) => g.node === group.id)?.fields.map((f) => f.port.type)).toEqual(["volume"]);
    expect(drivablesOf(graph, second.id, names).find((g) => g.node === group.id)?.fields.map((f) => f.port.type)).toEqual(["volume", "pan"]);
    // From the other side: the pan of the group can be driven by every modulator but the one that does.
    expect(driversOf(graph, group.id, { type: "pan" }).map((n) => n.id)).toEqual(modulators.filter((m) => m.id !== first.id).map((m) => m.id));
    expect(driversOf(snapshot.graph, group.id, { type: "pan" }).map((n) => n.id)).toEqual(modulators.map((m) => m.id));
  });

  test("have no drivers where the node has not the value: a pan of a lane, a volume of an effect, a mix of a group, a volume of a lane that has no instrument", () => {
    const lane = snapshot.graph.nodes.find((n) => n.kind === "instrument" && n.plugin !== null);
    const empty = snapshot.graph.nodes.find((n) => n.kind === "instrument" && n.plugin === null);
    const effect = snapshot.graph.nodes.find((n) => n.kind === "effect");
    const group = snapshot.graph.nodes.find((n) => n.kind === "group");
    if (!lane || !empty || !effect || !group) throw new Error("the session lacks a lane, an empty lane, an effect or a group");
    expect(driversOf(snapshot.graph, lane.id, { type: "pan" })).toEqual([]);
    expect(driversOf(snapshot.graph, effect.id, { type: "volume" })).toEqual([]);
    expect(driversOf(snapshot.graph, group.id, { type: "mix" })).toEqual([]);
    expect(driversOf(snapshot.graph, empty.id, { type: "volume" })).toEqual([]);
    expect(driversOf(snapshot.graph, 999_999, { type: "mix" })).toEqual([]);
    // (And the ones it has, by every modulator.)
    expect(driversOf(snapshot.graph, lane.id, { type: "volume" })).toHaveLength(modulators.length);
    expect(driversOf(snapshot.graph, effect.id, { type: "mix" })).toHaveLength(modulators.length);
  });

  test("are said in the words of a cable: a lane's volume as its output volume, an effect's mix, a group's pan", () => {
    const lane = snapshot.graph.nodes.find((n) => n.kind === "instrument" && n.plugin !== null);
    const effect = snapshot.graph.nodes.find((n) => n.kind === "effect");
    const group = snapshot.graph.nodes.find((n) => n.kind === "group");
    if (!lane || !effect || !group) throw new Error("the session lacks a lane, an effect or a group");
    expect(portName(snapshot.graph, lane.id, { type: "volume" })).toBe("output volume");
    expect(portName(snapshot.graph, effect.id, { type: "mix" })).toBe("mix");
    expect(portName(snapshot.graph, group.id, { type: "volume" })).toBe("volume");
    expect(portName(snapshot.graph, group.id, { type: "pan" })).toBe("pan");
  });
});
