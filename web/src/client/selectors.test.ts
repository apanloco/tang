import { describe, expect, test } from "vitest";

import type { ClientState } from "./connection";
import { MASTER, nodeNamed, tracerSnapshot } from "./fixtures";
import { cableDepthOf, gainOf, modulatorFieldOf, nodeById, paramOf } from "./selectors";
import type { Node, Snapshot } from "../protocol";

const stateOf = (snapshot: Snapshot | null): ClientState => ({ status: "open", problem: null, hello: null, snapshot, reconnects: 0 });

const state = stateOf(tracerSnapshot);
const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");
const lfo = nodeNamed(tracerSnapshot, "LFO");

describe("finding what a view shows", () => {
  test("a node by its id", () => {
    expect(nodeById(state, filter.id)).toBe(filter);
    expect(nodeById(state, MASTER)?.kind).toBe("master");
    expect(nodeById(state, 12345)).toBeUndefined();
  });

  test("nothing, before there is a session", () => {
    const empty = stateOf(null);
    expect(nodeById(empty, filter.id)).toBeUndefined();
    expect(paramOf(empty, filter.id, 0)).toBeUndefined();
    expect(gainOf(empty, filter.id, "mix")).toBeUndefined();
  });

  test("a parameter of a plugin by its own index, in an instrument and in an effect", () => {
    expect(paramOf(state, filter.id, 0)?.name).toBe("cutoff");
    expect(paramOf(state, filter.id, 1)?.name).toBe("resonance");
    expect(paramOf(state, oscillator.id, 1)?.name).toBe("detune");
    expect(paramOf(state, oscillator.id, 0)?.labels).toEqual(["sine", "triangle", "square", "saw"]);
  });

  test("no parameter of a node that has none, or of one the plugin does not have", () => {
    expect(paramOf(state, filter.id, 99)).toBeUndefined();
    expect(paramOf(state, lfo.id, 0)).toBeUndefined();
    expect(paramOf(state, MASTER, 0)).toBeUndefined();
    expect(paramOf(state, 12345, 0)).toBeUndefined();
  });

  test("an instrument that has no plugin has no parameters", () => {
    const emptied = structuredClone(tracerSnapshot);
    const lane = emptied.graph.nodes.find((node) => node.kind === "instrument");
    if (lane?.kind === "instrument") lane.plugin = null;
    expect(paramOf(stateOf(emptied), oscillator.id, 0)).toBeUndefined();
  });

  test("a gain, of the node it belongs to", () => {
    expect(gainOf(state, oscillator.id, "volume")).toBeCloseTo(0.1, 6);
    expect(gainOf(state, filter.id, "mix")).toBe(1);
  });

  test("no gain that the node does not have", () => {
    expect(gainOf(state, oscillator.id, "mix")).toBeUndefined();
    expect(gainOf(state, filter.id, "volume")).toBeUndefined();
    expect(gainOf(state, filter.id, "groupVolume")).toBeUndefined();
    expect(gainOf(state, oscillator.id, "groupPan")).toBeUndefined();
    expect(gainOf(state, lfo.id, "volume")).toBeUndefined();
    expect(gainOf(state, MASTER, "mix")).toBeUndefined();
  });

  test("a group's volume and pan", () => {
    const grouped = structuredClone(tracerSnapshot);
    grouped.graph.nodes.push({ id: 77, parent: null, name: "Group 1", kind: "group", volume: 0.8, pan: -0.25 });
    const withGroup = stateOf(grouped);
    expect(gainOf(withGroup, 77, "groupVolume")).toBeCloseTo(0.8, 6);
    expect(gainOf(withGroup, 77, "groupPan")).toBeCloseTo(-0.25, 6);
    expect(gainOf(withGroup, 77, "volume")).toBeUndefined();
    expect(gainOf(withGroup, 77, "mix")).toBeUndefined();
  });
});

describe("the settings of a modulator and the depths of its cables", () => {
  const envelope: Node = {
    id: 41,
    parent: oscillator.id,
    name: "ADSR",
    kind: "modulator",
    source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 },
  };
  const control: Node = { id: 42, parent: oscillator.id, name: "CC 74", kind: "modulator", source: { type: "midiLearn", control: { type: "cc", number: 74 }, learning: false } };
  const withMore = stateOf({ ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, envelope, control] } });

  test("an LFO's rate, and its waveform as the index the intent takes", () => {
    expect(modulatorFieldOf(state, lfo.id, "rate")).toBe(0.5);
    expect(modulatorFieldOf(state, lfo.id, "waveform")).toBe(0);
    const saw = structuredClone(tracerSnapshot);
    const node = saw.graph.nodes.find((n) => n.id === lfo.id);
    if (node?.kind === "modulator" && node.source.type === "lfo") node.source.waveform = "saw";
    expect(modulatorFieldOf(stateOf(saw), lfo.id, "waveform")).toBe(2);
  });

  test("the stages of an envelope", () => {
    expect(["attack", "decay", "sustain", "release"].map((field) => modulatorFieldOf(withMore, envelope.id, field as "attack"))).toEqual([0.01, 0.3, 0.7, 0.5]);
  });

  test("the index of each of the four waveforms", () => {
    const names = ["sine", "triangle", "saw", "square"] as const;
    const indexes = names.map((name) => {
      const copy = structuredClone(tracerSnapshot);
      const node = copy.graph.nodes.find((n) => n.id === lfo.id);
      if (node?.kind === "modulator" && node.source.type === "lfo") node.source.waveform = name;
      return modulatorFieldOf(stateOf(copy), lfo.id, "waveform");
    });
    expect(indexes).toEqual([0, 1, 2, 3]);
  });

  test("no setting that the modulator does not have, or of a node that is not one", () => {
    expect(modulatorFieldOf(state, lfo.id, "attack")).toBeUndefined();
    expect(modulatorFieldOf(withMore, envelope.id, "rate")).toBeUndefined();
    expect(modulatorFieldOf(withMore, envelope.id, "waveform")).toBeUndefined();
    expect(modulatorFieldOf(withMore, control.id, "rate")).toBeUndefined();
    expect(modulatorFieldOf(state, filter.id, "rate")).toBeUndefined();
    expect(modulatorFieldOf(state, 12345, "rate")).toBeUndefined();
    expect(modulatorFieldOf(stateOf(null), lfo.id, "rate")).toBeUndefined();
  });

  test("the depth of a cable by its id", () => {
    expect(cableDepthOf(state, 0)).toBe(0.4);
    expect(cableDepthOf(state, 12345)).toBeUndefined();
    expect(cableDepthOf(stateOf(null), 0)).toBeUndefined();
  });

  test("no depth of an edge that is not a cable of modulation", () => {
    // The audio edges have no id of the kind: the number that is the id of a cable is not theirs.
    expect(tracerSnapshot.graph.edges.filter((edge) => edge.kind !== "modulation")).not.toHaveLength(0);
    expect(cableDepthOf(state, 1)).toBeUndefined();
  });
});
