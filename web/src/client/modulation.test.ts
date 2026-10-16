import { describe, expect, test } from "vitest";

import type { Edge, Graph, ModulatorSource, Node, Plugin } from "../protocol";
import { tracerSnapshot } from "./fixtures";
import { modulationExtent, modulationsAt, modulationsOn, settingExtent } from "./modulation";
import type { Modulation } from "./modulation";

const lfo = (amount: number): Modulation => ({ source: { type: "lfo", waveform: "sine", rate: 1 }, amount });
const envelope = (amount: number): Modulation => ({ source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 }, amount });
const macro = (amount: number): Modulation => ({ source: { type: "macro", value: 0.5 }, amount });
const steps = (amount: number): Modulation => ({ source: { type: "steps", values: Array<number>(16).fill(0), length: 16, rate: 8, glide: 0, retrigger: true }, amount });
const midi = (control: Extract<ModulatorSource, { type: "midiLearn" }>["control"], amount: number): Modulation => ({
  source: { type: "midiLearn", control, learning: false },
  amount,
});

/** An extent, to the last digit a float can be trusted with. */
function expectExtent(actual: { low: number; high: number } | undefined, low: number, high: number): void {
  expect(actual?.low).toBeCloseTo(low, 10);
  expect(actual?.high).toBeCloseTo(high, 10);
}

describe("how far modulation swings a parameter", () => {
  test("is ± depth × range around the base for a LFO", () => {
    expectExtent(modulationExtent(0.5, 0, 1, [lfo(0.4)]), 0.1, 0.9);
    expect(modulationExtent(0, -24, 24, [lfo(0.25)])).toEqual({ low: -12, high: 12 });
    expect(modulationExtent(5, 0, 10, [lfo(0.1)])).toEqual({ low: 4, high: 6 });
  });

  test("is the same for a LFO of the opposite depth: it wobbles both ways", () => {
    expect(modulationExtent(5, 0, 10, [lfo(-0.1)])).toEqual({ low: 4, high: 6 });
  });

  test("stops at the ends of the parameter's range, as the engine clamps", () => {
    expectExtent(modulationExtent(0.9, 0, 1, [lfo(0.4)]), 0.5, 1);
    expectExtent(modulationExtent(0.1, 0, 1, [lfo(0.4)]), 0, 0.5);
    expect(modulationExtent(1, 0, 1, [lfo(0.5)])).toEqual({ low: 0.5, high: 1 });
  });

  test("goes from depth × (base - min) below the base up to it for an envelope, whose peak the base is", () => {
    expect(modulationExtent(0.8, 0, 1, [envelope(1)])).toEqual({ low: 0, high: 0.8 });
    expect(modulationExtent(0.8, 0, 1, [envelope(0.5)])).toEqual({ low: 0.4, high: 0.8 });
    expect(modulationExtent(5, 2, 10, [envelope(0.5)])).toEqual({ low: 3.5, high: 5 });
  });

  test("goes above the base for an envelope of negative depth, up to the end of the range", () => {
    expect(modulationExtent(0.5, 0, 1, [envelope(-0.5)])).toEqual({ low: 0.5, high: 0.75 });
    expect(modulationExtent(0.8, 0, 1, [envelope(-1)])).toEqual({ low: 0.8, high: 1 });
  });

  test("is nothing for an envelope on a parameter at its lowest, which has nowhere to go below", () => {
    expect(modulationExtent(0, 0, 1, [envelope(1)])).toBeUndefined();
  });

  test("goes up from the base by depth × range for a CC, for pressure and for a velocity, and down for a negative depth", () => {
    expect(modulationExtent(2, 0, 10, [midi({ type: "cc", number: 74 }, 0.5)])).toEqual({ low: 2, high: 7 });
    expect(modulationExtent(2, 0, 10, [midi({ type: "aftertouch" }, 0.25)])).toEqual({ low: 2, high: 4.5 });
    expect(modulationExtent(2, 0, 10, [midi({ type: "velocity" }, 0.25)])).toEqual({ low: 2, high: 4.5 });
    expect(modulationExtent(2, 0, 10, [midi({ type: "velocity" }, -0.5)])).toEqual({ low: 0, high: 2 });
    expect(modulationExtent(2, 0, 10, [midi({ type: "cc", number: 1 }, -0.5)])).toEqual({ low: 0, high: 2 });
  });

  test("is ± depth × range around the base for a step sequencer, whose steps go both ways as an LFO's output does", () => {
    expect(modulationExtent(2, 0, 10, [steps(0.5)])).toEqual({ low: 0, high: 7 });
    expect(modulationExtent(5, 0, 10, [steps(0.1)])).toEqual({ low: 4, high: 6 });
    expect(modulationExtent(5, 0, 10, [steps(-0.1)])).toEqual({ low: 4, high: 6 });
    // (Not from nothing up, as a macro's is.)
    expect(modulationExtent(5, 0, 10, [macro(0.1)])).toEqual({ low: 5, high: 6 });
  });

  test("goes up from the base by depth × range for a macro, which rests at nothing as a CC does, and down for a negative depth", () => {
    expect(modulationExtent(2, 0, 10, [macro(0.5)])).toEqual({ low: 2, high: 7 });
    expect(modulationExtent(2, 0, 10, [macro(-0.5)])).toEqual({ low: 0, high: 2 });
    // (It does not wobble both ways: an LFO of the same depth does.)
    expect(modulationExtent(2, 0, 10, [lfo(0.5)])).toEqual({ low: 0, high: 7 });
    expect(modulationExtent(2, 0, 10, [macro(0.25)])).toEqual({ low: 2, high: 4.5 });
  });

  test("goes both ways from the base for the pitch bend, which rests in the middle", () => {
    expect(modulationExtent(2, 0, 10, [midi({ type: "pitchBend" }, 0.5)])).toEqual({ low: 0, high: 7 });
    expect(modulationExtent(5, 0, 10, [midi({ type: "pitchBend" }, 0.25)])).toEqual({ low: 2.5, high: 7.5 });
  });

  test("goes both ways from the base for a key, which rests at middle C and has five octaves either way for the whole of its depth", () => {
    expect(modulationExtent(2, 0, 10, [midi({ type: "key" }, 0.5)])).toEqual({ low: 0, high: 7 });
    expect(modulationExtent(5, 0, 10, [midi({ type: "key" }, 0.25)])).toEqual({ low: 2.5, high: 7.5 });
  });

  test("is nothing for a MIDI control that is not bound yet: the base shows through", () => {
    expect(modulationExtent(0.5, 0, 1, [midi(null, 1)])).toBeUndefined();
  });

  test("adds up what each of several modulators can do, and then stops at the ends", () => {
    // A LFO of ±0.1 and a CC of up to +0.2 on a base of 0.5 in 0…1.
    const extent = modulationExtent(0.5, 0, 1, [lfo(0.1), midi({ type: "cc", number: 1 }, 0.2)]);
    expect(extent?.low).toBeCloseTo(0.4, 10);
    expect(extent?.high).toBeCloseTo(0.8, 10);
    expectExtent(modulationExtent(0.9, 0, 1, [lfo(0.3), midi({ type: "cc", number: 1 }, 0.3)]), 0.6, 1);
  });

  test("ignores a source that is not bound among those that are", () => {
    expect(modulationExtent(5, 0, 10, [midi(null, 1), lfo(0.1)])).toEqual({ low: 4, high: 6 });
  });

  test("is nothing when nothing drives the parameter, or a depth of nothing does", () => {
    expect(modulationExtent(0.5, 0, 1, [])).toBeUndefined();
    expect(modulationExtent(0.5, 0, 1, [lfo(0)])).toBeUndefined();
  });

  test("is nothing for a parameter with no range", () => {
    expect(modulationExtent(3, 3, 3, [lfo(0.5)])).toBeUndefined();
  });
});

describe("how far modulation swings a setting of a modulator", () => {
  test("is that of the cable that writes last alone: the engine writes a setting once for each cable, each from the value the user set, and the last stands", () => {
    // An LFO at 0.1 of the range 0 to 10 of a setting set to 5, and an envelope at 0.5, which rises to it: the one that is later is the one that counts.
    expect(settingExtent(5, 0, 10, [lfo(0.1), envelope(0.5)])).toEqual({ low: 2.5, high: 5 });
    expect(settingExtent(5, 0, 10, [envelope(0.5), lfo(0.1)])).toEqual({ low: 4, high: 6 });
  });

  test("is what one cable does to a parameter when only one drives it, whatever the source", () => {
    for (const only of [lfo(0.4), envelope(0.5), envelope(-0.5), midi({ type: "cc", number: 74 }, 0.5), midi({ type: "pitchBend" }, 0.25)]) {
      expect(settingExtent(5, 0, 10, [only])).toEqual(modulationExtent(5, 0, 10, [only]));
    }
  });

  test("stops at the ends of the range of the setting, as the engine clamps", () => {
    expect(settingExtent(9, 0, 10, [lfo(0.5)])).toEqual({ low: 4, high: 10 });
  });

  test("is nothing when nothing drives it, or when the cable that writes last is a control that is not bound: it writes the set value over the others", () => {
    expect(settingExtent(5, 0, 10, [])).toBeUndefined();
    expect(settingExtent(5, 0, 10, [lfo(0.1), midi(null, 0.5)])).toBeUndefined();
    // (One that is not the last changes nothing.)
    expect(settingExtent(5, 0, 10, [midi(null, 0.5), lfo(0.1)])).toEqual({ low: 4, high: 6 });
  });
});

describe("how far modulation swings a setting that is turned on a log scale", () => {
  // An attack of 0.001 to 10 seconds is four decades: 0.1 s is half way up the knob, and the engine moves such a setting a fraction of the knob's travel (`Modulator::modulated_value`).
  const at = (place: number): number => 0.001 * 10_000 ** place;

  test("is ± depth of the knob's travel around the base for a LFO: as far up the knob as down it", () => {
    expectExtent(settingExtent(0.1, 0.001, 10, [lfo(0.2)], "log"), at(0.3), at(0.7));
  });

  test("goes from depth of the way down the knob to the lowest value up to the base for an envelope, whose peak the base is", () => {
    expectExtent(settingExtent(0.1, 0.001, 10, [envelope(0.5)], "log"), at(0.25), at(0.5));
  });

  test("goes up the knob from the base by depth for a CC, and both ways for the pitch bend", () => {
    expectExtent(settingExtent(0.1, 0.001, 10, [midi({ type: "cc", number: 74 }, 0.2)], "log"), at(0.5), at(0.7));
    expectExtent(settingExtent(0.1, 0.001, 10, [midi({ type: "pitchBend" }, 0.2)], "log"), at(0.3), at(0.7));
  });

  test("stops at the ends of the knob, as the engine clamps: a base near the lowest value has a long way up and none down", () => {
    // 0.01 s is a quarter of the way up the knob: half the travel either way is below the lowest value and a quarter more than the half above.
    expectExtent(settingExtent(0.01, 0.001, 10, [lfo(0.5)], "log"), 0.001, at(0.75));
  });

  test("is the last cable's alone, as for any setting, and nothing when nothing drives it", () => {
    expectExtent(settingExtent(0.1, 0.001, 10, [lfo(0.1), envelope(0.5)], "log"), at(0.25), at(0.5));
    expect(settingExtent(0.1, 0.001, 10, [], "log")).toBeUndefined();
  });

  test("is by the range for a range that starts at nothing, which has no decades, and for a scale that is not asked for", () => {
    expect(settingExtent(5, 0, 10, [lfo(0.1)], "log")).toEqual(settingExtent(5, 0, 10, [lfo(0.1)]));
    expect(settingExtent(5, 0, 10, [lfo(0.1)], "linear")).toEqual({ low: 4, high: 6 });
    // (And one that starts above nothing is by the range too, unless the scale is asked for: that is the default.)
    expect(settingExtent(5, 1, 11, [lfo(0.1)])).toEqual({ low: 4, high: 6 });
    expect(settingExtent(5, 1, 11, [lfo(0.1)], "linear")).toEqual({ low: 4, high: 6 });
  });
});

describe("the modulators that drive a place on a modulator", () => {
  const modulator = (id: number, source: ModulatorSource): Node => ({ id, parent: 0, name: `Modulator ${String(id)}`, kind: "modulator", source });
  const driven = modulator(60, { type: "envelope", attack: 0.1, decay: 0.1, sustain: 0.5, release: 0.1 });
  const driver = modulator(61, { type: "lfo", waveform: "sine", rate: 2 });
  const other = modulator(62, { type: "envelope", attack: 0.2, decay: 0.2, sustain: 0.6, release: 0.2 });
  const cable = (from: number, to: Edge["to"], amount: number, id: number): Edge => ({ kind: "modulation", id, amount, from: { node: from, port: { type: "modOut" } }, to });
  const graph: Graph = {
    nodes: [driven, driver, other],
    edges: [
      cable(61, { node: 60, port: { type: "attack" } }, 0.3, 1),
      cable(62, { node: 60, port: { type: "attack" } }, -0.5, 2),
      cable(62, { node: 60, port: { type: "release" } }, 0.25, 3),
      cable(61, { node: 60, port: { type: "depth", edge: 7 } }, 0.1, 4),
      cable(62, { node: 60, port: { type: "depth", edge: 8 } }, 0.2, 5),
      cable(61, { node: 60, port: { type: "param", index: 0 } }, 0.9, 6),
    ],
  };
  const summary = (found: readonly Modulation[]): [string, number][] => found.map((m) => [m.source.type, m.amount]);

  test("are those whose cables end at the setting, in the order of the graph's edges, with their depths", () => {
    expect(summary(modulationsAt(graph, 60, { type: "attack" }))).toEqual([["lfo", 0.3], ["envelope", -0.5]]);
    expect(summary(modulationsAt(graph, 60, { type: "release" }))).toEqual([["envelope", 0.25]]);
  });

  test("are those of the cable's depth, for the port of that cable and no other", () => {
    expect(summary(modulationsAt(graph, 60, { type: "depth", edge: 7 }))).toEqual([["lfo", 0.1]]);
    expect(summary(modulationsAt(graph, 60, { type: "depth", edge: 8 }))).toEqual([["envelope", 0.2]]);
    expect(modulationsAt(graph, 60, { type: "depth", edge: 9 })).toEqual([]);
  });

  test("are none for a setting nothing drives, a node that has none, or a node that is not there; a parameter's number is not a setting", () => {
    expect(modulationsAt(graph, 60, { type: "decay" })).toEqual([]);
    expect(modulationsAt(graph, 61, { type: "rate" })).toEqual([]);
    expect(modulationsAt(graph, 12345, { type: "attack" })).toEqual([]);
    // (And a parameter is the same place as `modulationsOn` says.)
    expect(summary(modulationsAt(graph, 60, { type: "param", index: 0 }))).toEqual([["lfo", 0.9]]);
    expect(modulationsOn(graph, 60, 0)).toEqual(modulationsAt(graph, 60, { type: "param", index: 0 }));
  });
});

describe("the modulators that drive a parameter of a lane that is in a group", () => {
  // A group with an LFO of its own, a lane in it with an effect and an envelope of its own (and one more LFO, and a control that is not bound yet).
  const plugin: Plugin = { id: "builtin:filter", format: "Built-in", isInstrument: false, params: [], presets: [], currentPreset: null, pinned: [] };
  const group: Node = { id: 1, parent: null, name: "Group 1", kind: "group", volume: 1, pan: 0, muted: false, solo: false };
  const lane: Node = { id: 2, parent: 1, name: "Oscillator", kind: "instrument", plugin: null, range: null, transpose: 0, volume: 1, muted: false, solo: false, pattern: null };
  const filter: Node = { id: 3, parent: 2, name: "Filter", kind: "effect", plugin, mix: 1 };
  const groupLfo: Node = { id: 4, parent: 1, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 1 } };
  const laneEnvelope: Node = { id: 5, parent: 2, name: "ADSR", kind: "modulator", source: { type: "envelope", attack: 0.1, decay: 0.1, sustain: 0.5, release: 0.1 } };
  const laneLfo: Node = { id: 6, parent: 2, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "square", rate: 2 } };
  const laneControl: Node = { id: 7, parent: 2, name: "MIDI", kind: "modulator", source: { type: "midiLearn", control: null, learning: true } };
  const nodes = [group, lane, filter, groupLfo, laneEnvelope, laneLfo, laneControl];
  const cable = (from: number, index: number, amount: number, id: number): Edge => ({
    kind: "modulation",
    id,
    amount,
    from: { node: from, port: { type: "modOut" } },
    to: { node: filter.id, port: { type: "param", index } },
  });
  const summary = (found: readonly Modulation[]): [string, number][] => found.map((m) => [m.source.type, m.amount]);

  // The engine applies a group's modulators before its lanes render and a lane's own later, inside the lane, each from the value the user set: the lane's
  // write stands, and the group's offset is not added to it (`a_lane_modulator_and_a_group_modulator_on_one_parameter_leave_what_the_lane_makes_of_it`).

  test("are the lane's alone when the lane's own modulators drive it too: its write stands over the group's", () => {
    const graph: Graph = { nodes, edges: [cable(groupLfo.id, 0, 0.2, 1), cable(laneEnvelope.id, 0, -0.5, 2)] };
    expect(summary(modulationsOn(graph, filter.id, 0))).toEqual([["envelope", -0.5]]);
  });

  test("are the lane's alone in whatever order the graph lists the cables", () => {
    const graph: Graph = { nodes, edges: [cable(laneEnvelope.id, 0, -0.5, 2), cable(groupLfo.id, 0, 0.2, 1)] };
    expect(summary(modulationsOn(graph, filter.id, 0))).toEqual([["envelope", -0.5]]);
  });

  test("are all of the lane's, in the graph's order, when there are several: they add up, and the group's are not among them", () => {
    const graph: Graph = { nodes, edges: [cable(groupLfo.id, 0, 0.2, 1), cable(laneEnvelope.id, 0, -0.5, 2), cable(laneLfo.id, 0, 0.1, 3)] };
    expect(summary(modulationsOn(graph, filter.id, 0))).toEqual([
      ["envelope", -0.5],
      ["lfo", 0.1],
    ]);
  });

  test("are the group's when no modulator of the lane drives that parameter, whatever the lane's drive of the others", () => {
    const graph: Graph = { nodes, edges: [cable(groupLfo.id, 1, 0.3, 1), cable(laneEnvelope.id, 0, -0.5, 2)] };
    expect(summary(modulationsOn(graph, filter.id, 1))).toEqual([["lfo", 0.3]]);
    expect(summary(modulationsOn(graph, filter.id, 0))).toEqual([["envelope", -0.5]]);
  });

  test("are the lane's even when its control is not bound yet, which writes the value that the user set over the group's: nothing moves the parameter", () => {
    const graph: Graph = { nodes, edges: [cable(groupLfo.id, 0, 0.2, 1), cable(laneControl.id, 0, 0.5, 2)] };
    const found = modulationsOn(graph, filter.id, 0);
    expect(summary(found)).toEqual([["midiLearn", 0.5]]);
    expect(modulationExtent(0.5, 0, 1, found)).toBeUndefined();
  });

  test("are those of the group, all of them, when only the group's drive it: they add up, as they do for a lane's, and a bus effect has no others", () => {
    const secondLfo: Node = { id: 8, parent: 1, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "triangle", rate: 3 } };
    const bus: Node = { id: 9, parent: 1, name: "Reverb", kind: "effect", plugin, mix: 0.5 };
    const toBus = (from: number, amount: number, id: number): Edge => ({
      kind: "modulation",
      id,
      amount,
      from: { node: from, port: { type: "modOut" } },
      to: { node: bus.id, port: { type: "param", index: 0 } },
    });
    const graph: Graph = {
      nodes: [...nodes, secondLfo, bus],
      edges: [cable(groupLfo.id, 0, 0.2, 1), cable(secondLfo.id, 0, -0.1, 2), toBus(groupLfo.id, 0.4, 3), toBus(secondLfo.id, 0.6, 4)],
    };
    expect(summary(modulationsOn(graph, filter.id, 0))).toEqual([["lfo", 0.2], ["lfo", -0.1]]);
    expect(summary(modulationsOn(graph, bus.id, 0))).toEqual([["lfo", 0.4], ["lfo", 0.6]]);
  });
});

describe("the modulators that drive a parameter", () => {
  const graph = tracerSnapshot.graph;
  const filter = graph.nodes.find((node) => node.kind === "effect");
  const modulator = graph.nodes.find((node) => node.kind === "modulator");
  if (!filter || !modulator) throw new Error("the tracer lacks a filter or a modulator");

  test("are those whose edges end at it, with their depths", () => {
    expect(modulationsOn(graph, filter.id, 0)).toEqual([{ source: { type: "lfo", waveform: "sine", rate: 0.5 }, amount: 0.4 }]);
  });

  test("are none for a parameter nothing drives, or a node that has none", () => {
    expect(modulationsOn(graph, filter.id, 1)).toEqual([]);
    expect(modulationsOn(graph, filter.id, 99)).toEqual([]);
    expect(modulationsOn(graph, modulator.id, 0)).toEqual([]);
    expect(modulationsOn(graph, 12345, 0)).toEqual([]);
  });

  test("are found in the order of the edges, however many there are, and are those of that parameter only", () => {
    const second: Node = { id: 50, parent: 0, name: "Envelope", kind: "modulator", source: { type: "envelope", attack: 0.1, decay: 0.1, sustain: 0.5, release: 0.1 } };
    const edge = (from: number, port: Edge["to"]["port"], amount: number, id: number): Edge => ({
      kind: "modulation",
      id,
      amount,
      from: { node: from, port: { type: "modOut" } },
      to: { node: filter.id, port },
    });
    const crowded: Graph = {
      nodes: [...graph.nodes, second],
      edges: [
        ...graph.edges,
        edge(50, { type: "param", index: 0 }, -0.25, 10),
        edge(50, { type: "param", index: 1 }, 0.75, 11),
        edge(50, { type: "rate" }, 0.5, 12),
      ],
    };
    expect(modulationsOn(crowded, filter.id, 0).map((m) => [m.source.type, m.amount])).toEqual([
      ["lfo", 0.4],
      ["envelope", -0.25],
    ]);
    expect(modulationsOn(crowded, filter.id, 1).map((m) => [m.source.type, m.amount])).toEqual([["envelope", 0.75]]);
  });

  test("do not include an edge that does not start at a modulator, or is not a modulation", () => {
    const odd: Graph = {
      nodes: graph.nodes,
      edges: [
        { kind: "modulation", id: 1, amount: 0.5, from: { node: filter.id, port: { type: "modOut" } }, to: { node: filter.id, port: { type: "param", index: 0 } } },
        { kind: "audio", from: { node: modulator.id, port: { type: "audioOut" } }, to: { node: filter.id, port: { type: "param", index: 0 } } },
        { kind: "modulation", id: 2, amount: 0.5, from: { node: 777, port: { type: "modOut" } }, to: { node: filter.id, port: { type: "param", index: 0 } } },
      ],
    };
    expect(modulationsOn(odd, filter.id, 0)).toEqual([]);
  });
});
