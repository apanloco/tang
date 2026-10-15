import { describe, expect, test } from "vitest";

import type { Edge, Intent, ModulatorKind, Node, NodeId, Port } from "../protocol";
import { historyFixture } from "./fixtures";
import { MockServer } from "./mock";

// A knob modulated in one intent (`Intent::Modulate`, `src/app/apply.rs`): the mock engine makes the modulator for what a port is and draws the cable to it, as
// the controller does. The words of its history and the shape of what it makes are pinned to the controller's by the history's fixture (`history.test.ts`); what is
// checked here is what a person relies on.
const { snapshot } = historyFixture;
const fresh = (): MockServer => new MockServer({ snapshot });

const nodes = snapshot.graph.nodes;
const lane0 = nodes.find((n) => n.kind === "instrument");
const group = nodes.find((n) => n.kind === "group");
if (!lane0 || !group) throw new Error("the fixture has a lane and a group");
const effectOf = (parent: NodeId, name: string): Node => {
  const found = nodes.find((n) => n.kind === "effect" && n.parent === parent && n.name === name);
  if (!found) throw new Error(`no ${name} in ${String(parent)}`);
  return found;
};
const filter = effectOf(lane0.id, "Filter");
const bus = effectOf(group.id, "Filter");
const lfo = nodes.find((n) => n.kind === "modulator" && n.parent === lane0.id && n.source.type === "lfo");
const envelope = nodes.find((n) => n.kind === "modulator" && n.parent === lane0.id && n.source.type === "envelope");
if (!lfo || !envelope) throw new Error("the fixture has an LFO and an envelope in the first lane");
const cableOfLfo = snapshot.graph.edges.find((e) => e.kind === "modulation" && e.from.node === lfo.id);
if (cableOfLfo?.kind !== "modulation") throw new Error("the LFO has a cable");

const param = (node: Node, name: string): Port => {
  const found = (node.kind === "instrument" || node.kind === "effect") && node.plugin ? node.plugin.params.find((p) => p.name === name) : undefined;
  if (!found) throw new Error(`no ${name} in ${node.name}`);
  return { type: "param", index: found.index };
};

const modulate = (node: NodeId, port: Port, kind: ModulatorKind = "lfo", depth: number | null = null): Intent => ({ type: "modulate", to: { node, port }, kind, depth });

/** The nodes and the cables a session has that another has not. */
function made(before: MockServer["snapshot"], after: MockServer["snapshot"]): { node: Node; cable: Edge & { kind: "modulation" } } {
  const had = new Set(before.graph.nodes.map((n) => n.id));
  const added = after.graph.nodes.filter((n) => !had.has(n.id));
  const cables = after.graph.edges.filter((e): e is Edge & { kind: "modulation" } => e.kind === "modulation" && !before.graph.edges.includes(e));
  const [node] = added;
  const [cable] = cables;
  if (!node || !cable || added.length !== 1 || cables.length !== 1) throw new Error(`one node and one cable were to be made: ${String(added.length)} and ${String(cables.length)}`);
  return { node, cable };
}

describe("the mock engine's modulate", () => {
  test("makes a modulator in the lane of the plugin and a cable from it to the parameter, at half depth, in one change of the structure", () => {
    const server = fresh();
    const to = param(filter, "resonance");
    const told = server.apply(modulate(filter.id, to));
    expect(told).toEqual({ events: [], restructured: true });
    const { node, cable } = made(snapshot, server.snapshot);
    expect([node.kind, node.parent, node.name]).toEqual(["modulator", lane0.id, "LFO"]);
    expect(node.kind === "modulator" ? node.source : null).toEqual({ type: "lfo", waveform: "sine", rate: 1 });
    expect([cable.from, cable.to, cable.amount]).toEqual([{ node: node.id, port: { type: "modOut" } }, { node: filter.id, port: to }, 0.5]);
    // One intent is one revision, as for any change of the structure: the one it was, and the snapshot that tells it.
    expect(server.snapshot.rev).toBe(snapshot.rev + 2);
  });

  test("makes each kind of modulator, and a MIDI control is armed to learn", () => {
    for (const [kind, source] of [
      ["lfo", { type: "lfo", waveform: "sine", rate: 1 }],
      ["envelope", { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 }],
      ["midiLearn", { type: "midiLearn", control: null, learning: true }],
    ] as const) {
      const server = fresh();
      server.apply(modulate(filter.id, param(filter, "resonance"), kind));
      const { node } = made(snapshot, server.snapshot);
      expect(node.kind === "modulator" ? node.source : null, kind).toEqual(source);
    }
  });

  test("goes in the rack of the lane for the instrument and for an effect of its chain, and in the group's for a bus effect", () => {
    const racks: [Node, string, string][] = [
      [lane0, "detune", "the instrument"],
      [filter, "resonance", "an effect of the lane"],
      [bus, "resonance", "a bus effect"],
    ];
    for (const [target, name, what] of racks) {
      const server = fresh();
      server.apply(modulate(target.id, param(target, name)));
      expect(made(snapshot, server.snapshot).node.parent, what).toBe(target === bus ? group.id : lane0.id);
    }
  });

  test("goes in the rack of a modulator, a sibling that drives it, for its rate, its stages and the depth of its cables", () => {
    const rate: Port = { type: "rate" };
    const attack: Port = { type: "attack" };
    const depth: Port = { type: "depth", edge: cableOfLfo.id };
    for (const [target, port] of [
      [lfo, rate],
      [envelope, attack],
      [lfo, depth],
    ] as const) {
      const server = fresh();
      server.apply(modulate(target.id, port));
      const { node, cable } = made(snapshot, server.snapshot);
      expect([node.parent, cable.to], port.type).toEqual([target.parent, { node: target.id, port }]);
    }
  });

  test("goes after the nodes of the lane it joins, where the graph lists a lane's nodes together", () => {
    const server = fresh();
    server.apply(modulate(filter.id, param(filter, "resonance")));
    const { node } = made(snapshot, server.snapshot);
    const lastOfLane = nodes.findLastIndex((n) => n.id === lane0.id || (n.parent === lane0.id && (n.kind === "effect" || n.kind === "modulator")));
    expect(server.snapshot.graph.nodes.findIndex((n) => n.id === node.id)).toBe(lastOfLane + 1);
  });

  test("starts the cable at the depth that is asked for, clamped as a depth that is set is", () => {
    for (const [asked, got] of [
      [null, 0.5],
      [0.75, 0.75],
      [-0.25, -0.25],
      [0, 0],
      [5, 1],
      [-5, -1],
    ] as const) {
      const server = fresh();
      server.apply(modulate(filter.id, param(filter, "resonance"), "lfo", asked));
      expect(made(snapshot, server.snapshot).cable.amount, String(asked)).toBe(got);
    }
  });

  test("hands out the id of the node and then that of the cable, from where the session's own end", () => {
    const server = fresh();
    server.apply(modulate(filter.id, param(filter, "resonance")));
    const { node, cable } = made(snapshot, server.snapshot);
    const lastNode = Math.max(...nodes.filter((n) => n.kind !== "master").map((n) => n.id));
    const lastCable = Math.max(...snapshot.graph.edges.flatMap((e) => (e.kind === "modulation" ? [e.id] : [])));
    expect([node.id, cable.id]).toEqual([lastNode + 1, lastCable + 1]);
  });

  test("gives the modulator a place in the layout of the telemetry, and the parameter too unless another cable drives it already", () => {
    const server = fresh();
    server.apply(modulate(filter.id, param(filter, "resonance")));
    const { node } = made(snapshot, server.snapshot);
    const before = snapshot.telemetry.modulation;
    const after = server.snapshot.telemetry.modulation;
    expect(after.filter((entry) => !before.includes(entry)).map((entry) => [entry.kind, entry.node])).toEqual([
      ["modOutput", node.id],
      ["effective", filter.id],
    ]);
    // (The detune of the first lane is driven by its LFO: the layout names the value once.)
    const driven = fresh();
    driven.apply(modulate(lane0.id, param(lane0, "detune")));
    const { node: other } = made(snapshot, driven.snapshot);
    expect(driven.snapshot.telemetry.modulation.filter((entry) => !before.includes(entry)).map((entry) => [entry.kind, entry.node])).toEqual([["modOutput", other.id]]);
    expect(driven.snapshot.telemetry.rev).toBeGreaterThan(snapshot.telemetry.rev);
  });

  test("is refused, and nothing is made or used up, for what a cable to the port is refused, a group, a node that is not there and a depth that is not a number", () => {
    const refused: [string, Intent, string][] = [
      ["a node that is not there", modulate(9_999, { type: "param", index: 1 }), "there is no node 9999"],
      ["a group", modulate(group.id, { type: "param", index: 1 }), "a modulator drives a parameter of a plugin or a setting of a modulator, and that is neither"],
      ["a parameter the plugin lacks", modulate(filter.id, { type: "param", index: 9_999 }), "the plugin has no parameter 9999"],
      ["a modulator, which has no plugin", modulate(lfo.id, { type: "param", index: 1 }), "that node has no plugin"],
      ["a port that is no target", modulate(filter.id, { type: "modOut" }), "a cable can only end at a plugin's parameter, or at a modulator's rate, stage or depth"],
      ["a setting of an effect", modulate(filter.id, { type: "rate" }), "a cable ends at the settings of a modulator, and that is not one"],
      ["a setting the modulator has not", modulate(envelope.id, { type: "rate" }), "that modulator has no rate"],
      ["a cable that is not there", modulate(lfo.id, { type: "depth", edge: 9_999 }), "there is no cable 9999"],
      ["a depth that is not a number", modulate(filter.id, param(filter, "resonance"), "lfo", Number.NaN), "NaN is not a value for the depth of a cable"],
      ["a depth that 32 bits cannot hold", modulate(filter.id, param(filter, "resonance"), "lfo", 1e39), "inf is not a value for the depth of a cable"],
    ];
    for (const [what, intent, why] of refused) {
      const server = fresh();
      expect(() => server.apply(intent), what).toThrow(why);
      expect(server.snapshot, what).toBe(snapshot);
      expect(server.history, what).toEqual({ undo: null, redo: null });
      // What is made next has the ids and the slots it would have had if nothing had been asked.
      const next = fresh();
      for (const server_ of [server, next]) server_.apply({ type: "addModulator", parent: lane0.id, kind: "lfo" });
      expect(server.snapshot, what).toEqual(next.snapshot);
    }
  });

  test("is refused for an effect that is in no lane or group, which a graph does not have, and nothing is made", () => {
    const orphan = { ...snapshot, graph: { ...snapshot.graph, nodes: nodes.map((n) => (n.id === filter.id ? { ...n, parent: null } : n)) } };
    const server = new MockServer({ snapshot: orphan });
    expect(() => server.apply(modulate(filter.id, param(filter, "resonance")))).toThrow("that node is in no lane or group");
    expect(server.snapshot).toBe(orphan);
  });

  test("makes a modulator for a parameter that another modulator drives already, which a second cable is allowed", () => {
    // (The detune of the first lane is driven by its LFO.)
    const detune = param(lane0, "detune");
    expect(cableOfLfo.to).toEqual({ node: lane0.id, port: detune });
    const server = fresh();
    server.apply(modulate(lane0.id, detune, "envelope"));
    const { cable } = made(snapshot, server.snapshot);
    expect(cable.to).toEqual({ node: lane0.id, port: detune });
    expect(cable.id).not.toBe(cableOfLfo.id);
  });
});
