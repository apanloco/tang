import { describe, expect, test } from "vitest";

import type { ClientState } from "./connection";
import { MASTER, nodeNamed, reachFixture, tracerSnapshot } from "./fixtures";
import { cableDepthOf, gainOf, modulatorFieldOf, nodeById, paramCountOf, paramListOf, paramOf, presetsOf, sameParamList } from "./selectors";
import type { Node, Snapshot } from "../protocol";

const stateOf = (snapshot: Snapshot | null): ClientState => ({ status: "open", problem: null, hello: null, snapshot, catalog: null, reconnects: 0 });

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

describe("finding the presets a plugin can be given", () => {
  const reach = stateOf(reachFixture.snapshot);

  test("those of an effect of a lane, with none loaded to begin with", () => {
    const found = presetsOf(reach, 10);
    expect(found?.presets.map((p) => p.id)).toEqual(["arcadia", "cathedral", "plate", "room", "ambient"]);
    expect(found?.current).toBeNull();
  });

  test("and the one that was loaded last, by its name", () => {
    const loaded = structuredClone(reachFixture.snapshot);
    const reverb = loaded.graph.nodes.find((n) => n.id === 10);
    if (reverb?.kind !== "effect") throw new Error("no reverb");
    reverb.plugin.currentPreset = "Cathedral";
    expect(presetsOf(stateOf(loaded), 10)?.current).toBe("Cathedral");
  });

  test("those of an instrument", () => {
    const given = structuredClone(tracerSnapshot);
    const osc = given.graph.nodes.find((n) => n.id === oscillator.id);
    if (osc?.kind !== "instrument" || !osc.plugin) throw new Error("no instrument");
    osc.plugin.presets = [{ id: "pad", name: "Pad" }];
    expect(presetsOf(stateOf(given), oscillator.id)?.presets).toEqual([{ id: "pad", name: "Pad" }]);
  });

  test("none for an effect on a group's bus (a preset is loaded by a lane's slot), though it has them", () => {
    expect(paramOf(reach, 3, 0)).toBeDefined();
    expect(presetsOf(reach, 3)).toBeNull();
  });

  test("none for a node that has no plugin, a lane that has none, a node that is not there, or before there is a session", () => {
    expect(presetsOf(reach, 15)).toBeNull(); // a modulator
    expect(presetsOf(reach, 0)).toBeNull(); // a group
    expect(presetsOf(reach, 14)).toBeNull(); // a lane with no instrument
    expect(presetsOf(reach, 424242)).toBeNull();
    expect(presetsOf(reach, MASTER)).toBeNull();
    expect(presetsOf(stateOf(null), 10)).toBeNull();
  });

  test("a plugin that has no presets has an empty list: the caller decides what that is worth", () => {
    expect(presetsOf(state, filter.id)).toEqual({ presets: [], current: null });
  });
});

describe("finding the parameters of a plugin to look through", () => {
  const reach = stateOf(reachFixture.snapshot);

  test("those of an instrument and of an effect, each by the plugin's own index, with the node's name and the plugin's format", () => {
    expect(paramListOf(state, oscillator.id)).toEqual({
      name: "Oscillator",
      format: "Built-in",
      params: ["waveform", "detune", "volume", "attack", "decay", "sustain", "release", "pan"].map((name, index) => ({ index, name })),
    });
    expect(paramListOf(state, filter.id)?.params).toEqual([
      { index: 0, name: "cutoff" },
      { index: 1, name: "resonance" },
      { index: 2, name: "type" },
      { index: 3, name: "drive" },
    ]);
  });

  test("by the plugin's own index, which is not its place in the list: a plugin can number its parameters with gaps", () => {
    const gaps = structuredClone(tracerSnapshot);
    const node = gaps.graph.nodes.find((n) => n.id === filter.id);
    if (node?.kind !== "effect") throw new Error("no filter");
    node.plugin.params = node.plugin.params.map((param, position) => ({ ...param, index: [3, 8, 20, 21][position] ?? 99 }));
    expect(paramListOf(stateOf(gaps), filter.id)?.params.map((param) => param.index)).toEqual([3, 8, 20, 21]);
  });

  test("those of an effect on a group's bus too: it has parameters to turn, whatever its presets", () => {
    expect(paramListOf(reach, 3)?.params.length).toBeGreaterThan(0);
    expect(presetsOf(reach, 3)).toBeNull();
  });

  test("the node's name is the one the canvas gives it: numbered when two nodes are called the same", () => {
    const twins = structuredClone(tracerSnapshot);
    const copy = structuredClone(oscillator);
    twins.graph.nodes.push({ ...copy, id: 777 });
    const twinned = stateOf(twins);
    expect(paramListOf(twinned, oscillator.id)?.name).toBe("Oscillator 1");
    expect(paramListOf(twinned, 777)?.name).toBe("Oscillator 2");
  });

  test("none for a node that has no plugin, a lane that has none, a node that is not there, or before there is a session", () => {
    expect(paramListOf(reach, 15)).toBeNull(); // a modulator
    expect(paramListOf(reach, 0)).toBeNull(); // a group
    expect(paramListOf(reach, 14)).toBeNull(); // a lane with no instrument
    expect(paramListOf(reach, 424242)).toBeNull();
    expect(paramListOf(reach, MASTER)).toBeNull();
    expect(paramListOf(stateOf(null), 10)).toBeNull();
  });

  test("a plugin that has no parameters has an empty list", () => {
    const bare = structuredClone(tracerSnapshot);
    const node = bare.graph.nodes.find((n) => n.id === filter.id);
    if (node?.kind !== "effect") throw new Error("no filter");
    node.plugin.params = [];
    expect(paramListOf(stateOf(bare), filter.id)?.params).toEqual([]);
  });

  test("is the same list when only a value changed, and another when a name, an index, the format, the node's name or the number of parameters did", () => {
    const base = paramListOf(state, filter.id);
    if (!base) throw new Error("no list");
    const moved = structuredClone(tracerSnapshot);
    const node = moved.graph.nodes.find((n) => n.id === filter.id);
    if (node?.kind !== "effect") throw new Error("no filter");
    node.plugin.params = node.plugin.params.map((param, position) => (position === 0 ? { ...param, value: 0.9 } : param));
    expect(sameParamList(base, paramListOf(stateOf(moved), filter.id))).toBe(true);
    expect(sameParamList(base, base)).toBe(true);
    expect(sameParamList(null, null)).toBe(true);
    expect(sameParamList(base, null)).toBe(false);
    expect(sameParamList(null, base)).toBe(false);

    const other = (change: (list: { name: string; format: string; params: { index: number; name: string }[] }) => void) => {
      const copy = structuredClone(base) as { name: string; format: string; params: { index: number; name: string }[] };
      change(copy);
      return sameParamList(base, copy);
    };
    expect(other((l) => (l.params[1] = { index: 1, name: "q" }))).toBe(false);
    expect(other((l) => (l.params[1] = { index: 5, name: "resonance" }))).toBe(false);
    expect(other((l) => (l.format = "VST3"))).toBe(false);
    expect(other((l) => (l.name = "Filter 2"))).toBe(false);
    expect(other((l) => l.params.pop())).toBe(false);
    expect(other((l) => l.params.push({ index: 4, name: "extra" }))).toBe(false);
  });
});

describe("counting the parameters of a plugin", () => {
  const reach = stateOf(reachFixture.snapshot);

  test("is how many the plugin of an instrument or an effect has, whatever the effect is in", () => {
    expect(paramCountOf(state, oscillator.id)).toBe(8);
    expect(paramCountOf(state, filter.id)).toBe(4);
    expect(paramCountOf(reach, 3)).toBe(paramListOf(reach, 3)?.params.length);
  });

  test("is none for a node that has no plugin, a node that is not there, or before there is a session", () => {
    expect(paramCountOf(reach, 15)).toBe(0); // a modulator
    expect(paramCountOf(reach, 0)).toBe(0); // a group
    expect(paramCountOf(reach, 14)).toBe(0); // a lane with no instrument
    expect(paramCountOf(reach, MASTER)).toBe(0);
    expect(paramCountOf(reach, 424242)).toBe(0);
    expect(paramCountOf(stateOf(null), filter.id)).toBe(0);
  });
});
