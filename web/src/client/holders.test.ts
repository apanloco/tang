import { describe, expect, test } from "vitest";

import type { Edge, Graph, Node, NodeId, PatternInfo, Plugin } from "../protocol";
import { holdersOf } from "./holders";
import { MASTER } from "./fixtures";

// -- Graphs made by hand --

const plugin = (id: string, isInstrument: boolean): Plugin => ({ id, format: "Built-in", isInstrument, params: [], presets: [], currentPreset: null, pinned: [] });

const lane = (id: NodeId, over: Partial<Extract<Node, { kind: "instrument" }>> = {}): Node => ({
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

const effect = (id: NodeId, parent: NodeId, name = `Effect ${String(id)}`): Node => ({ id, parent, name, kind: "effect", plugin: plugin("builtin:filter", false), mix: 1, feed: "chain" });
const group = (id: NodeId): Node => ({ id, parent: null, name: "Pad", kind: "group", volume: 1, pan: 0, muted: false, solo: false, route: "out" });
const master: Node = { id: MASTER, parent: null, name: "Master", kind: "master", volume: 1, limiter: true, ceiling: -1 };
const keys = (id: NodeId): Node => ({ id, parent: null, name: "Keys", kind: "source", source: { type: "keys" } });
const modulator = (id: NodeId, parent: NodeId): Node => ({
  id,
  parent,
  name: "LFO",
  kind: "modulator",
  source: { type: "lfo", waveform: "sine", rate: 1, retrigger: false, fade: 0, sync: 0 },
});

const audio = (from: NodeId, to: NodeId): Edge => ({ kind: "audio", from: { node: from, port: { type: "audioOut" } }, to: { node: to, port: { type: "audioIn" } } });
const send = (id: number, from: NodeId, to: NodeId): Edge => ({ kind: "cable", id, gain: 1, from: { node: from, port: { type: "audioOut" } }, to: { node: to, port: { type: "audioIn" } } });
const notes = (id: number, from: NodeId, to: NodeId): Edge => ({ kind: "events", id, from: { node: from, port: { type: "eventsOut" } }, to: { node: to, port: { type: "eventsIn" } } });
const hears = (from: NodeId, to: NodeId): Edge => ({ kind: "hears", from: { node: from, port: { type: "eventsOut" } }, to: { node: to, port: { type: "eventsIn" } } });
const drives = (id: number, from: NodeId, to: NodeId): Edge => ({ kind: "modulation", id, amount: 0.5, from: { node: from, port: { type: "modOut" } }, to: { node: to, port: { type: "param", index: 1 } } });

const aPattern: PatternInfo = { recording: false, enabled: true, looping: true, inKey: false, lengthBeats: 4, bpm: 120, notes: 3 };

/** A lane that holds a reverb and a filter, and nothing else. */
const held = (over: Partial<Extract<Node, { kind: "instrument" }>> = {}, edges: Edge[] = [], extra: Node[] = []): Graph => ({
  nodes: [lane(1, over), effect(2, 1, "Reverb"), effect(3, 1, "Filter"), ...extra, master],
  edges: [audio(1, 2), audio(2, 3), audio(3, MASTER), ...edges],
});

const ids = (graph: Graph): NodeId[] => [...holdersOf(graph).keys()];

describe("the lanes that are drawn as the effects they hold", () => {
  test("a lane that has no instrument and has effects, and nothing else that its card shows, holds them in the order of its chain", () => {
    const graph = held();
    expect(ids(graph)).toEqual([1]);
    expect(holdersOf(graph).get(1)?.map((node) => node.name)).toEqual(["Reverb", "Filter"]);
  });

  test("each holder has its own effects, and the effects of a lane that has an instrument are nobody's", () => {
    const graph: Graph = {
      nodes: [lane(1), effect(2, 1, "Reverb"), lane(3, { name: "Oscillator", plugin: plugin("builtin:osc", true) }), effect(4, 3, "Delay"), lane(5), effect(6, 5, "Filter"), effect(7, 5, "Drive"), master],
      edges: [],
    };
    expect(ids(graph)).toEqual([1, 5]);
    expect(holdersOf(graph).get(1)?.map((node) => node.name)).toEqual(["Reverb"]);
    expect(holdersOf(graph).get(5)?.map((node) => node.name)).toEqual(["Filter", "Drive"]);
  });

  test("a lane that has an instrument is not one, and neither is a lane that holds nothing", () => {
    expect(ids(held({ name: "Oscillator", plugin: plugin("builtin:osc", true) }))).toEqual([]);
    expect(ids({ nodes: [lane(1), master], edges: [] })).toEqual([]);
    // (An effect with no lane to be in is no holder's: nothing is drawn for a lane that is not there.)
    expect(ids({ nodes: [effect(2, 9), master], edges: [] })).toEqual([]);
  });

  test("a lane with a key range, with a pattern or in a group has things on its card that it keeps", () => {
    expect(ids(held({ range: { low: 36, high: 60 } }))).toEqual([]);
    expect(ids(held({ pattern: aPattern }))).toEqual([]);
    const grouped: Graph = { nodes: [group(10), lane(1, { parent: 10 }), effect(2, 1), master], edges: [] };
    expect(ids(grouped)).toEqual([]);
    // (A range that is every note is none: it is what a lane that has none has.)
    expect(ids(held({ range: null }))).toEqual([1]);
  });

  test("a lane that an audio cable starts at, or a cable of notes ends at, is where that cable is drawn, and keeps its card", () => {
    expect(ids(held({}, [send(0, 1, 2)]))).toEqual([]);
    expect(ids(held({}, [], [keys(9)]))).toEqual([1]);
    expect(ids(held({}, [notes(1, 9, 1)], [keys(9)]))).toEqual([]);
    // (The cable of another lane's is not this lane's.)
    const two: Graph = { nodes: [lane(1), effect(2, 1), lane(3), effect(4, 3), master], edges: [send(0, 2, 4), notes(1, 9, 3)] };
    expect(ids(two)).toEqual([1]);
  });

  test("what the graph derives, the links of the chain, the lane's link to every source and the cables of its modulators, are no cables", () => {
    const graph = held({}, [hears(9, 1), drives(0, 4, 2)], [keys(9), modulator(4, 1)]);
    expect(ids(graph)).toEqual([1]);
  });

  test("what a graph has is read from it each time it is asked: a lane that is given a key range after it was looked at is not one", () => {
    const graph = held();
    expect(ids(graph)).toEqual([1]);
    const lane = graph.nodes.find((node) => node.kind === "instrument");
    if (lane?.kind !== "instrument") throw new Error("no lane");
    lane.range = { low: 1, high: 2 };
    expect(ids(graph)).toEqual([]);
  });
});
