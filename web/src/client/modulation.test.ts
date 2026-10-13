import { describe, expect, test } from "vitest";

import type { Edge, Graph, ModulatorSource, Node } from "../protocol";
import { tracerSnapshot } from "./fixtures";
import { modulationExtent, modulationsOn } from "./modulation";
import type { Modulation } from "./modulation";

const lfo = (amount: number): Modulation => ({ source: { type: "lfo", waveform: "sine", rate: 1 }, amount });
const envelope = (amount: number): Modulation => ({ source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 }, amount });
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

  test("goes up from the base by depth × range for a CC and for pressure, and down for a negative depth", () => {
    expect(modulationExtent(2, 0, 10, [midi({ type: "cc", number: 74 }, 0.5)])).toEqual({ low: 2, high: 7 });
    expect(modulationExtent(2, 0, 10, [midi({ type: "aftertouch" }, 0.25)])).toEqual({ low: 2, high: 4.5 });
    expect(modulationExtent(2, 0, 10, [midi({ type: "cc", number: 1 }, -0.5)])).toEqual({ low: 0, high: 2 });
  });

  test("goes both ways from the base for the pitch bend, which rests in the middle", () => {
    expect(modulationExtent(2, 0, 10, [midi({ type: "pitchBend" }, 0.5)])).toEqual({ low: 0, high: 7 });
    expect(modulationExtent(5, 0, 10, [midi({ type: "pitchBend" }, 0.25)])).toEqual({ low: 2.5, high: 7.5 });
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
