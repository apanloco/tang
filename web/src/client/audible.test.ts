import { describe, expect, test } from "vitest";

import type { Graph, Node } from "../protocol";
import { audibleOf } from "./audible";
import { audibleFixture } from "./fixtures";

// What of a session's lanes and groups can be heard (docs/web-ui.md D53): the page's `audibleOf`, which has to be what the engine's mute and solo make of them
// (`AudioGraph::process`), pinned to a run of the engine over every combination of the flags of two groups and four lanes.

/** The graph of the fixture's two groups and four lanes, with the flags of case `flags`: two bits for each node (muted, soloed), the groups first. */
function graphOf(flags: number): { graph: Graph; groups: number[]; lanes: number[] } {
  const bits = (node: number) => ({ muted: ((flags >> (2 * node)) & 1) === 1, solo: ((flags >> (2 * node)) & 2) === 2 });
  const groups = [10, 11];
  const lanes = [20, 21, 22, 23];
  const nodes: Node[] = [
    ...groups.map((id, place): Node => ({ id, parent: null, name: `Group ${String(place + 1)}`, kind: "group", volume: 1, pan: 0, route: "out", ...bits(place) })),
    ...lanes.map((id, place): Node => {
      const group = audibleFixture.lanes[place];
      return { id, parent: group === null || group === undefined ? null : (groups[group] ?? null), name: "Oscillator", kind: "instrument", plugin: null, range: null, transpose: 0, volume: 1, route: "out", pattern: null, ...bits(2 + place) };
    }),
  ];
  return { graph: { nodes, edges: [] }, groups, lanes };
}

describe("what is heard", () => {
  test("is what the engine makes of every combination of mute and solo of two groups and four lanes", () => {
    expect(audibleFixture.cases).toHaveLength(4 ** 6);
    for (const [flags, sum] of audibleFixture.cases) {
      const { graph, groups, lanes } = graphOf(flags);
      const heard = audibleOf(graph);
      const expected = [...lanes.map((_, place) => (sum >> place) & 1), ...groups.map((_, place) => (sum >> (4 + place)) & 1)];
      const got = [...lanes, ...groups].map((id) => (heard.get(id) ? 1 : 0));
      expect(got, `flags ${String(flags)} (sum ${String(sum)})`).toEqual(expected);
    }
  });

  test("says it of lanes and groups, and of nothing else", () => {
    const { graph } = graphOf(0);
    const effect: Node = { id: 30, parent: 20, name: "Reverb", kind: "effect", plugin: { id: "builtin:reverb", format: "Built-in", isInstrument: false, params: [], presets: [], currentPreset: null, pinned: [] }, mix: 1, feed: "chain" };
    const heard = audibleOf({ ...graph, nodes: [...graph.nodes, effect] });
    expect([...heard.keys()].sort()).toEqual([10, 11, 20, 21, 22, 23]);
  });

  test("is worked out once for a graph: the same graph has the same answer, and another graph has its own", () => {
    const { graph } = graphOf(0);
    expect(audibleOf(graph)).toBe(audibleOf(graph));
    expect(audibleOf({ ...graph })).not.toBe(audibleOf(graph));
  });

  test("takes a lane whose group is not in the graph as one that is in none", () => {
    const { graph } = graphOf(0);
    const lone = graph.nodes.map((node) => (node.id === 20 && node.kind === "instrument" ? { ...node, parent: 99, muted: false, solo: false } : node));
    expect(audibleOf({ ...graph, nodes: lone }).get(20)).toBe(true);
    const soloed = lone.map((node) => (node.id === 21 && node.kind === "instrument" ? { ...node, solo: true } : node));
    expect(audibleOf({ ...graph, nodes: soloed }).get(20)).toBe(false);
  });
});
