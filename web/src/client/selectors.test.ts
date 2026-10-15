import { describe, expect, test } from "vitest";

import type { ClientState } from "./connection";
import { MASTER, groupsFixture, movesFixture, nodeNamed, reachFixture, tracerSnapshot } from "./fixtures";
import { cableDepthOf, cableSourceOf, chainPlaceOf, envelopeStagesOf, gainOf, groupChoicesOf, hasPins, isKnob, modulatorFieldOf, nodeById, paramCountOf, paramListOf, paramOf, presetsOf, rangeOf, removalOf, sameChainPlace, sameGroupChoices, sameParamList, sameRemoval, sameStages } from "./selectors";
import type { Node, Snapshot } from "../protocol";
import type { Removal } from "./selectors";

const stateOf = (snapshot: Snapshot | null): ClientState => ({ status: "open", problem: null, hello: null, snapshot, catalog: null, history: { undo: null, redo: null }, reconnects: 0 });

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

  test("the modulator a cable starts at, by the id of the cable: the node whose depth setting the cable's slot is named at", () => {
    expect(cableSourceOf(state, 0)).toBe(lfo.id);
    expect(cableSourceOf(state, 12345)).toBeUndefined();
    expect(cableSourceOf(stateOf(null), 0)).toBeUndefined();
    // (And not of an edge that is not a cable of modulation.)
    expect(cableSourceOf(state, 1)).toBeUndefined();
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

describe("finding the settings of an envelope", () => {
  // The reach fixture's modulators: LFOs (6, 15, 17), envelopes (8, 16) and a MIDI control (13), the envelopes at the engine's defaults.
  const reach = stateOf(reachFixture.snapshot);

  test("are its four stages, by name: the three times and the sustain level", () => {
    expect(envelopeStagesOf(reach, 8)).toEqual({ attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 });
    expect(envelopeStagesOf(reach, 16)).toEqual({ attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 });
  });

  test("are none for what is no envelope: an LFO, a MIDI control, a plugin, a node that is not there, or before there is a session", () => {
    for (const id of [6, 13, 15, 17, 0, 12345, MASTER]) expect(envelopeStagesOf(reach, id), String(id)).toBeNull();
    expect(envelopeStagesOf(stateOf(null), 8)).toBeNull();
  });

  test("follow a stage that changes, and are the same settings while nothing of them does", () => {
    const changed = structuredClone(reachFixture.snapshot);
    const envelope = changed.graph.nodes.find((n) => n.id === 8);
    if (envelope?.kind === "modulator" && envelope.source.type === "envelope") envelope.source.release = 4;
    expect(envelopeStagesOf(stateOf(changed), 8)).toEqual({ attack: 0.01, decay: 0.3, sustain: 0.7, release: 4 });
    expect(sameStages(envelopeStagesOf(reach, 8), envelopeStagesOf(stateOf(structuredClone(reachFixture.snapshot)), 8))).toBe(true);
    for (const stage of ["attack", "decay", "sustain", "release"] as const) {
      const a = { attack: 1, decay: 2, sustain: 0.5, release: 3 };
      expect(sameStages(a, { ...a, [stage]: a[stage] + 1 }), stage).toBe(false);
    }
    expect(sameStages(null, null)).toBe(true);
    expect(sameStages(null, { attack: 1, decay: 2, sustain: 0.5, release: 3 })).toBe(false);
    expect(sameStages({ attack: 1, decay: 2, sustain: 0.5, release: 3 }, null)).toBe(false);
  });
});

describe("finding where an effect is in its chain", () => {
  // The moves fixture: a bus of four effects (1 to 4) of the group 0, a lane (5) in it with four effects (6 to 9) and a lane (11) in none with three (12 to 14).
  const moves = stateOf(movesFixture.snapshot);

  test("is its place, counted from 0, and how many effects the chain has: of a lane's chain and of a bus", () => {
    expect(chainPlaceOf(moves, 6)).toEqual({ index: 0, count: 4 });
    expect(chainPlaceOf(moves, 8)).toEqual({ index: 2, count: 4 });
    expect(chainPlaceOf(moves, 9)).toEqual({ index: 3, count: 4 });
    expect(chainPlaceOf(moves, 12)).toEqual({ index: 0, count: 3 });
    expect(chainPlaceOf(moves, 14)).toEqual({ index: 2, count: 3 });
    expect(chainPlaceOf(moves, 1)).toEqual({ index: 0, count: 4 });
    expect(chainPlaceOf(moves, 3)).toEqual({ index: 2, count: 4 });
  });

  test("counts the effects of the chain it is in, and not those of the other chains of the session", () => {
    // The tracer's lane has one effect.
    expect(chainPlaceOf(state, filter.id)).toEqual({ index: 0, count: 1 });
  });

  test("is none for a node that is no effect, a node that is not there, or before there is a session", () => {
    for (const id of [0, 5, 10, 11, 16, MASTER, 12345]) expect(chainPlaceOf(moves, id), String(id)).toBeNull();
    expect(chainPlaceOf(stateOf(null), 6)).toBeNull();
  });

  test("follows the effect when it moves, and is the same answer while only a value changes", () => {
    // (The first two effects of the lane's chain change places in the graph.)
    const nodes = [...movesFixture.snapshot.graph.nodes];
    const first = nodes.findIndex((n) => n.id === 6);
    nodes.splice(first, 2, ...nodes.slice(first, first + 2).reverse());
    const moved: Snapshot = { ...movesFixture.snapshot, graph: { ...movesFixture.snapshot.graph, nodes } };
    expect(chainPlaceOf(stateOf(moved), 6)).toEqual({ index: 1, count: 4 });
    expect(chainPlaceOf(stateOf(moved), 7)).toEqual({ index: 0, count: 4 });

    expect(sameChainPlace(chainPlaceOf(moves, 6), chainPlaceOf(stateOf(structuredClone(movesFixture.snapshot)), 6))).toBe(true);
    expect(sameChainPlace(chainPlaceOf(moves, 6), chainPlaceOf(stateOf(moved), 6))).toBe(false);
    expect(sameChainPlace({ index: 1, count: 4 }, { index: 1, count: 5 })).toBe(false);
    expect(sameChainPlace(null, null)).toBe(true);
    expect(sameChainPlace(null, { index: 0, count: 1 })).toBe(false);
    expect(sameChainPlace({ index: 0, count: 1 }, null)).toBe(false);
  });
});

describe("finding the keys of a lane", () => {
  const reach = stateOf(reachFixture.snapshot);

  test("is none for a lane that takes every note, and its range for one that does not", () => {
    expect(rangeOf(state, oscillator.id)).toBeNull();
    const ranged = structuredClone(tracerSnapshot);
    const node = ranged.graph.nodes.find((n) => n.id === oscillator.id);
    if (node?.kind !== "instrument") throw new Error("no lane");
    node.range = { low: 48, high: 71 };
    expect(rangeOf(stateOf(ranged), oscillator.id)).toEqual({ low: 48, high: 71 });
  });

  test("is there for a lane that has no instrument", () => {
    expect(rangeOf(reach, 14)).toBeNull();
  });

  test("is not there for what is not a lane, a node that is not there, or before there is a session", () => {
    expect(rangeOf(state, filter.id)).toBeUndefined();
    expect(rangeOf(state, lfo.id)).toBeUndefined();
    expect(rangeOf(state, MASTER)).toBeUndefined();
    expect(rangeOf(reach, 0)).toBeUndefined(); // a group
    expect(rangeOf(state, 424242)).toBeUndefined();
    expect(rangeOf(stateOf(null), oscillator.id)).toBeUndefined();
  });
});

describe("which parameters a node shows as knobs", () => {
  /** The tracer with these parameters pinned to the plugin of the node. */
  const pinned = (node: Node, params: number[]): ClientState => {
    const nodes = tracerSnapshot.graph.nodes.map((n) => {
      if (n.id !== node.id || (n.kind !== "instrument" && n.kind !== "effect") || !n.plugin) return n;
      return { ...n, plugin: { ...n.plugin, pinned: params } };
    });
    return stateOf({ ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes } });
  };

  test("a parameter is one when it is among the first few and nothing is pinned, or when it is pinned", () => {
    expect(isKnob(state, filter.id, 0)).toBe(true);
    expect(isKnob(state, oscillator.id, 7)).toBe(true);
    // (The first eight: the tracer's plugins have no more.)
    const some = pinned(filter, [2, 0]);
    expect(isKnob(some, filter.id, 2)).toBe(true);
    expect(isKnob(some, filter.id, 0)).toBe(true);
    expect(isKnob(some, filter.id, 1)).toBe(false);
    expect(isKnob(some, filter.id, 3)).toBe(false);
    expect(isKnob(some, oscillator.id, 1)).toBe(true);
  });

  test("a parameter the plugin has not, a node that has no plugin and a session that is not there have none", () => {
    expect(isKnob(state, filter.id, 99)).toBe(false);
    expect(isKnob(state, lfo.id, 0)).toBe(false);
    expect(isKnob(state, MASTER, 0)).toBe(false);
    expect(isKnob(state, 12345, 0)).toBe(false);
    expect(isKnob(stateOf(null), filter.id, 0)).toBe(false);
  });

  test("a node has pins when its plugin has a list that is not empty", () => {
    expect(hasPins(state, filter.id)).toBe(false);
    expect(hasPins(pinned(filter, [1]), filter.id)).toBe(true);
    expect(hasPins(pinned(filter, [1]), oscillator.id)).toBe(false);
    expect(hasPins(state, lfo.id)).toBe(false);
    expect(hasPins(state, 12345)).toBe(false);
    expect(hasPins(stateOf(null), filter.id)).toBe(false);
  });
});

// The session of the groups fixture: three groups (0, 1 and 2, the last empty and called Lead; the first has two modulators and no effect, the second one
// modulator), four lanes (3 and 10 in the first group, 9 in the second, 5 in none, which has a filter and two modulators of its own).
describe("which group a lane is in, and which there are to choose from", () => {
  const groups = stateOf(groupsFixture.snapshot);
  const everyGroup = [
    { id: 0, name: "Group 1" },
    { id: 1, name: "Group 2" },
    { id: 2, name: "Lead" },
  ];

  test("the group a lane is in, and every group of the session in the order of the graph", () => {
    expect(groupChoicesOf(groups, 3)).toEqual({ current: 0, groups: everyGroup });
    expect(groupChoicesOf(groups, 9)).toEqual({ current: 1, groups: everyGroup });
    expect(groupChoicesOf(groups, 10)).toEqual({ current: 0, groups: everyGroup });
  });

  test("none, for a lane that is in no group", () => {
    expect(groupChoicesOf(groups, 5)).toEqual({ current: null, groups: everyGroup });
    expect(groupChoicesOf(state, oscillator.id)).toEqual({ current: null, groups: [] });
  });

  test("nothing for what is no lane: a group, an effect, a modulator, the master, a node that is not there, and before there is a session", () => {
    for (const node of [0, 6, 4, 13, MASTER, 9999]) expect(groupChoicesOf(groups, node), `node ${String(node)}`).toBeUndefined();
    expect(groupChoicesOf(stateOf(null), 3)).toBeUndefined();
  });

  test("says what a group is called now: the name it was given, or the one it has by default", () => {
    const named: Snapshot = {
      ...groupsFixture.snapshot,
      graph: { ...groupsFixture.snapshot.graph, nodes: groupsFixture.snapshot.graph.nodes.map((n) => (n.id === 1 ? { ...n, name: "Strings" } : n)) },
    };
    expect(groupChoicesOf(stateOf(named), 5)?.groups.map((g) => g.name)).toEqual(["Group 1", "Strings", "Lead"]);
  });

  test("is the same choice when it is the same group and the same groups by the same names, and not otherwise", () => {
    const a = groupChoicesOf(groups, 3);
    const b = groupChoicesOf(groups, 3);
    expect(a).not.toBe(b);
    expect(sameGroupChoices(a, b)).toBe(true);
    expect(sameGroupChoices(a, groupChoicesOf(groups, 10))).toBe(true);
    expect(sameGroupChoices(a, groupChoicesOf(groups, 9))).toBe(false);
    expect(sameGroupChoices(a, groupChoicesOf(groups, 5))).toBe(false);
    expect(sameGroupChoices(a, { current: 0, groups: everyGroup.slice(0, 2) })).toBe(false);
    expect(sameGroupChoices(a, { current: 0, groups: everyGroup.map((g) => ({ ...g, name: g.id === 2 ? "Pad" : g.name })) })).toBe(false);
    // The same names at the same places, but other nodes: an option's value is its node, so the choice that was drawn is stale.
    expect(sameGroupChoices(a, { current: 0, groups: everyGroup.map((g) => ({ ...g, id: g.id + 100 })) })).toBe(false);
    expect(sameGroupChoices(undefined, undefined)).toBe(true);
    expect(sameGroupChoices(a, undefined)).toBe(false);
    expect(sameGroupChoices(undefined, a)).toBe(false);
  });
});

describe("what goes when a lane or a group is taken out", () => {
  const groups = stateOf(groupsFixture.snapshot);

  test("for a group: what it is called, the effects of its bus and its modulators that go with it, and the lanes that stay", () => {
    expect(removalOf(groups, 0)).toEqual({ kind: "group", name: "Group 1", effects: 0, modulators: 2, lanes: 2, last: false });
    expect(removalOf(groups, 1)).toEqual({ kind: "group", name: "Group 2", effects: 0, modulators: 1, lanes: 1, last: false });
    expect(removalOf(groups, 2)).toEqual({ kind: "group", name: "Lead", effects: 0, modulators: 0, lanes: 0, last: false });
  });

  test("for a lane: what it is called, its effects and its modulators that go with it, whether it is in a group, and whether it is the last", () => {
    expect(removalOf(groups, 5)).toEqual({ kind: "lane", name: "Oscillator 2", effects: 1, modulators: 2, lanes: 0, last: false });
    expect(removalOf(groups, 10)).toEqual({ kind: "lane", name: "Oscillator 4", effects: 1, modulators: 1, lanes: 0, last: false });
    expect(removalOf(state, oscillator.id)).toEqual({ kind: "lane", name: "Oscillator", effects: 1, modulators: 1, lanes: 0, last: true });
  });

  test("nothing for what is neither: an effect, a modulator, the master, a node that is not there, and before there is a session", () => {
    for (const node of [6, 4, 13, MASTER, 9999]) expect(removalOf(groups, node), `node ${String(node)}`).toBeUndefined();
    expect(removalOf(stateOf(null), 0)).toBeUndefined();
  });

  test("a lane is the last when it is the only one, whatever else the session has, and a group never is", () => {
    // (The tracer with a second lane: neither is the last. And with a group: the group is not a lane, and the lane is still the last.)
    const second: Node = { ...oscillator, id: 9000, parent: null };
    const twoLanes = stateOf({ ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, second] } });
    expect([removalOf(twoLanes, oscillator.id)?.last, removalOf(twoLanes, second.id)?.last]).toEqual([false, false]);
    const found = groupsFixture.snapshot.graph.nodes.find((node) => node.id === 2);
    if (found?.kind !== "group") throw new Error("no group Lead");
    const lead: Node = { ...found, id: 9001 };
    const withGroup = stateOf({ ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, lead] } });
    expect([removalOf(withGroup, oscillator.id)?.last, removalOf(withGroup, lead.id)?.last]).toEqual([true, false]);
  });

  test("is the same removal when it says the same, and not when any one thing it says is different", () => {
    const a = removalOf(groups, 0);
    if (!a) throw new Error("no removal");
    expect(sameRemoval(a, removalOf(groups, 0))).toBe(true);
    expect(sameRemoval(a, removalOf(groups, 1))).toBe(false);
    const differing: Removal[] = [
      { ...a, kind: "lane" },
      { ...a, name: "Pad" },
      { ...a, effects: a.effects + 1 },
      { ...a, modulators: a.modulators + 1 },
      { ...a, lanes: a.lanes + 1 },
      { ...a, last: !a.last },
    ];
    for (const other of differing) expect(sameRemoval(a, other), JSON.stringify(other)).toBe(false);
    expect(sameRemoval(undefined, undefined)).toBe(true);
    expect(sameRemoval(a, undefined)).toBe(false);
    expect(sameRemoval(undefined, a)).toBe(false);
  });
});
