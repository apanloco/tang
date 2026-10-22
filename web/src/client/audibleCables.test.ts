import { describe, expect, test } from "vitest";

import type { Edge, Graph, Node, NodeId, Plugin } from "../protocol";
import { audibleOf, fedOf, heardOf, ownAudibleOf } from "./audible";
import { audibleCablesFixture } from "./fixtures";

// What of a session's lanes and groups can be heard when cables carry audio (docs/web-ui.md D78, P3c-2b of docs/patch-graph.md §11.6): the page's `audibleOf` and `ownAudibleOf`, which have to be
// what the engine's law of mute and solo for cables makes of them (`Schedule::set_gates`), pinned to a run of the engine over every combination of the flags of two groups and four lanes that
// have cables between them.

const GROUPS = [10, 11];
const LANES = [20, 21, 22, 23];
const LANE_EFFECTS = [30, 31, 32, 33];
const BUS_EFFECTS = [40, 41];

const plugin: Plugin = { id: "builtin:reverb", format: "Built-in", isInstrument: false, params: [], presets: [], currentPreset: null, pinned: [] };

interface Flags {
  muted: boolean;
  solo: boolean;
}

/** The node a cable of the fixture names: what it starts at or ends at, and which. */
function nodeOf(kind: string, at: number): NodeId {
  const found = { instrument: LANES, effect: LANE_EFFECTS, "bus effect": BUS_EFFECTS, group: GROUPS }[kind]?.[at];
  if (found === undefined) throw new Error(`no ${kind} ${String(at)}`);
  return found;
}

/**
 * The graph of the fixture's two groups and four lanes, each lane with an effect and each group with a bus effect, and its cables (at a small level: that they are there is all that matters),
 * with the flags of case `flags`: two bits for each node (muted, soloed), the groups first.
 */
function graphOf(flags: number, cables: boolean | readonly Edge[] = true): Graph {
  const bits = (node: number): Flags => ({ muted: ((flags >> (2 * node)) & 1) === 1, solo: ((flags >> (2 * node)) & 2) === 2 });
  const nodes: Node[] = [
    ...GROUPS.map((id, place): Node => ({ id, parent: null, name: `Group ${String(place + 1)}`, kind: "group", volume: 1, pan: 0, route: "out", ...bits(place) })),
    ...LANES.map((id, place): Node => {
      const group = audibleCablesFixture.lanes[place];
      return { id, parent: group === null || group === undefined ? null : (GROUPS[group] ?? null), name: "Oscillator", kind: "instrument", plugin: null, range: null, transpose: 0, volume: 1, route: "out", pattern: null, ...bits(2 + place) };
    }),
    ...LANE_EFFECTS.map((id, place): Node => ({ id, parent: LANES[place] ?? null, name: "Offset", kind: "effect", plugin, mix: 1 })),
    ...BUS_EFFECTS.map((id, place): Node => ({ id, parent: GROUPS[place] ?? null, name: "Offset", kind: "effect", plugin, mix: 1 })),
  ];
  const edges: Edge[] =
    typeof cables === "boolean"
      ? cables
        ? audibleCablesFixture.cables.map(([from, fromAt, to, toAt], place): Edge => cable(100 + place, nodeOf(from, fromAt), nodeOf(to, toAt)))
        : []
      : [...cables];
  return { nodes, edges };
}

function cable(id: number, from: NodeId, to: NodeId, gain = 1): Edge {
  return { kind: "cable", id, gain, from: { node: from, port: { type: "audioOut" } }, to: { node: to, port: { type: "audioIn" } } };
}

/** The graph with the way out of the lane `id` cut. */
function cutAt(graph: Graph, id: NodeId): Graph {
  return { ...graph, nodes: graph.nodes.map((node) => (node.id === id && node.kind === "instrument" ? { ...node, route: "none" as const } : node)) };
}

/** The graph with the flags of the nodes `ids` set to `flags`. */
function flagged(graph: Graph, ids: readonly NodeId[], flags: Partial<Flags>): Graph {
  return { ...graph, nodes: graph.nodes.map((node) => ((node.kind === "instrument" || node.kind === "group") && ids.includes(node.id) ? { ...node, ...flags } : node)) };
}

describe("what is heard when cables carry", () => {
  test("is what the engine makes of every combination of mute and solo of two groups and four lanes that have cables between them: the lanes' own sound, their chains and the buses", () => {
    expect(audibleCablesFixture.cases).toHaveLength(4 ** 6);
    for (const [flags, sum] of audibleCablesFixture.cases) {
      const graph = graphOf(flags);
      const [own, chain] = [ownAudibleOf(graph), audibleOf(graph)];
      const expected = [...LANES.map((_, place) => (sum >> place) & 1), ...LANES.map((_, place) => (sum >> (4 + place)) & 1), ...GROUPS.map((_, place) => (sum >> (8 + place)) & 1)];
      const got = [...LANES.map((id) => (own.get(id) ? 1 : 0)), ...LANES.map((id) => (chain.get(id) ? 1 : 0)), ...GROUPS.map((id) => (chain.get(id) ? 1 : 0))];
      expect(got, `flags ${String(flags)} (sum ${String(sum)})`).toEqual(expected);
    }
  });

  test("has been tried with cables that matter: the fixture is not one in which every case is as it would be without them", () => {
    // (The cases in which a lane's chain is heard and its own sound is not, which only a cable makes: bits 4 to 7 set where bits 0 to 3 are not.)
    const through = audibleCablesFixture.cases.filter(([, sum]) => LANES.some((_, place) => ((sum >> (4 + place)) & 1) === 1 && ((sum >> place) & 1) === 0));
    expect(through.length).toBeGreaterThan(200);
    // (And a bus that is heard while its only lane is neither heard for what it is nor for what it is sent: bit 9 set where bits 2 and 6 are not.)
    const busOnly = audibleCablesFixture.cases.filter(([, sum]) => ((sum >> 9) & 1) === 1 && ((sum >> 2) & 1) === 0 && ((sum >> 6) & 1) === 0);
    expect(busOnly.length).toBeGreaterThan(50);
  });

  test("is what it was without cables when there are none, whatever is soloed", () => {
    for (const flags of [0, 2, 8, 0b101000, 0b100010, 0b10101010]) {
      const without = graphOf(flags, false);
      const [own, heard, chain] = [ownAudibleOf(without), heardOf(without), audibleOf(without)];
      // (No cable: a lane is heard for what it is or not at all, and the bus it is in is heard for it.)
      for (const id of LANES) expect(own.get(id) === true, `lane ${String(id)} of ${String(flags)}`).toBe(chain.get(id) === true);
      for (const id of [...LANES, ...GROUPS]) expect(heard.has(id)).toBe(true);
    }
  });

  test("lets a lane that is sent to by the lane that is soloed be heard through its chain and not for itself: its effects are heard and its instrument is not", () => {
    // Lane 23 sends to lane 20's effect and is soloed: lane 20 is heard for what it is sent, and none of the other lanes is.
    const graph = flagged(graphOf(0, [cable(1, 23, 30)]), [23], { solo: true });
    expect(audibleOf(graph).get(20)).toBe(true);
    expect(ownAudibleOf(graph).get(20)).toBe(false);
    expect(audibleOf(graph).get(23)).toBe(true);
    expect(ownAudibleOf(graph).get(23)).toBe(true);
    // The lanes that nothing that is heard sends to are not heard.
    expect(audibleOf(graph).get(21)).toBe(false);
    expect(audibleOf(graph).get(22)).toBe(false);
    // Nothing is soloed: all are heard, each for what it is.
    expect([20, 21, 22, 23].map((id) => ownAudibleOf(graphOf(0, [cable(1, 23, 30)])).get(id))).toEqual([true, true, true, true]);
  });

  test("does not let a cable at nothing, or a lane that is muted, be heard for what it is sent", () => {
    const sends = [cable(1, 23, 30, 1)];
    const soloed = flagged(graphOf(0, sends), [23], { solo: true });
    expect(audibleOf(soloed).get(20)).toBe(true);
    expect(audibleOf({ ...soloed, edges: [cable(1, 23, 30, 0)] }).get(20)).toBe(false);
    expect(audibleOf(flagged(soloed, [20], { muted: true })).get(20)).toBe(false);
    expect(audibleOf(flagged(soloed, [23], { muted: true })).get(20)).toBe(false);
  });

  test("goes on through the cables of effects, and from the output of a group", () => {
    // The soloed lane 23 to lane 20's effect, and that effect to lane 22's: both are heard, the second for what the first sends on.
    const chained = flagged(graphOf(0, [cable(1, 23, 30), cable(2, 30, 32)]), [23], { solo: true });
    expect(audibleOf(chained).get(22)).toBe(true);
    expect(ownAudibleOf(chained).get(22)).toBe(false);
    // A cable from the output of a group that is soloed to a lane's effect: the lane is heard for it.
    const fromGroup = flagged(graphOf(0, [cable(1, 10, 33)]), [10], { solo: true });
    expect(audibleOf(fromGroup).get(23)).toBe(true);
    expect(ownAudibleOf(fromGroup).get(23)).toBe(false);
    // A group's own effect is a place to send to as well.
    const toBus = flagged(graphOf(0, [cable(1, 23, 41)]), [23], { solo: true });
    expect(audibleOf(toBus).get(11)).toBe(true);
    expect(audibleOf(toBus).get(10)).toBe(false);
  });

  test("lets in nothing by the cable of the instrument of a lane that is heard only for what it is sent (it has none of its own sound) and lets in what it is sent by the cables of its effects", () => {
    // Lane 23 (soloed) sends to lane 20's effect. From lane 20's instrument to lane 22's effect: lane 22 is not let in (nothing of lane 20's own goes on); from lane 20's effect: it is.
    const into = [cable(1, 23, 30)];
    const soloed = (edges: readonly Edge[]): Graph => flagged(graphOf(0, [...into, ...edges]), [23], { solo: true });
    expect(audibleOf(soloed([cable(2, 20, 32)])).get(22)).toBe(false);
    expect(audibleOf(soloed([cable(2, 30, 32)])).get(22)).toBe(true);
    // A lane that is heard for what it is does send by its instrument: lane 20 is soloed with it.
    expect(audibleOf(flagged(soloed([cable(2, 20, 32)]), [20], { solo: true })).get(22)).toBe(true);
    expect([...fedOf(soloed([cable(2, 20, 32)]))].sort()).toEqual([10, 20]);
  });

  test("says what a group is heard at for its own sound as it does for its chain, since it has no sound of its own to look at", () => {
    // Lane 23 (soloed) sends to the bus of group 11, which has no lane soloed: the group is heard for the send, and `ownAudibleOf` says so as well.
    const graph = flagged(graphOf(0, [cable(1, 23, 41)]), [23], { solo: true });
    expect(audibleOf(graph).get(11)).toBe(true);
    expect(ownAudibleOf(graph).get(11)).toBe(true);
    expect(ownAudibleOf(graph).get(10)).toBe(false);
    expect(ownAudibleOf(graph).get(22)).toBe(false);
  });

  test("hears a group for a lane in it that is heard for what is sent to it, and not for the other lanes in it", () => {
    // Lane 23 (soloed) sends to lane 20's effect, and lane 20 is in group 10, which is not soloed: the group is heard for it.
    const graph = flagged(graphOf(0, [cable(1, 23, 30)]), [23], { solo: true });
    expect(audibleOf(graph).get(10)).toBe(true);
    expect(audibleOf(graph).get(20)).toBe(true);
    expect(audibleOf(graph).get(21)).toBe(false);
    // With the lane's way out cut it does not carry what it is sent to its bus, and the group is not heard.
    const cut = cutAt(graph, 20);
    expect(audibleOf(cut).get(10)).toBe(false);
    expect(heardOf(cut).get(20)).toBe(true);
  });

  test("says which lanes and groups are fed by a cable from something that is heard: the lane or group that it ends at, and the group of a lane that goes to its bus", () => {
    // Lane 23 (in no group) to lane 20's effect (lane 20 is in group 10): lane 20 and its group are fed, whether or not anything is soloed.
    const sends = [cable(1, 23, 30)];
    expect([...fedOf(graphOf(0, sends))].sort()).toEqual([10, 20]);
    expect([...fedOf(flagged(graphOf(0, sends), [23], { solo: true }))].sort()).toEqual([10, 20]);
    // To the bus of group 11, and to its effect: the group is fed and no lane is.
    expect([...fedOf(graphOf(0, [cable(1, 23, 11)]))]).toEqual([11]);
    expect([...fedOf(graphOf(0, [cable(1, 23, 41)]))]).toEqual([11]);
    // Nothing is fed with no cable, with a cable at nothing, from a lane that is muted or is not heard, or to a lane that is muted.
    expect([...fedOf(graphOf(0, false))]).toEqual([]);
    expect([...fedOf(graphOf(0, [cable(1, 23, 30, 0)]))]).toEqual([]);
    expect([...fedOf(flagged(graphOf(0, sends), [23], { muted: true }))]).toEqual([]);
    expect([...fedOf(flagged(graphOf(0, sends), [21], { solo: true }))]).toEqual([]);
    expect([...fedOf(flagged(graphOf(0, sends), [20], { muted: true }))]).toEqual([]);
    // A lane whose way out is cut is fed all the same and puts none of it into its group, and a group that is muted is not fed, by a lane that goes to it or by a cable.
    expect([...fedOf(cutAt(graphOf(0, sends), 20))]).toEqual([20]);
    expect([...fedOf(flagged(graphOf(0, sends), [10], { muted: true }))]).toEqual([20]);
    expect([...fedOf(flagged(graphOf(0, [cable(1, 23, 11)]), [11], { muted: true }))]).toEqual([]);
  });

  test("is worked out once for a graph: the same graph has the same answer, and another graph has its own", () => {
    const graph = graphOf(0);
    expect(fedOf(graph)).toBe(fedOf(graph));
    expect(ownAudibleOf(graph)).toBe(ownAudibleOf(graph));
    expect(ownAudibleOf({ ...graph })).not.toBe(ownAudibleOf(graph));
    expect(audibleOf(graph)).toBe(audibleOf(graph));
    expect(heardOf(graph)).toBe(heardOf(graph));
  });
});
