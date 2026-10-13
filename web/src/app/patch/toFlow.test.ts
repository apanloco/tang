import { describe, expect, test } from "vitest";

import type { Edge, Graph, Node, NodeId, Param, Plugin } from "../../protocol";
import { MASTER, tracerSnapshot } from "../../client/fixtures";
import {
  CABLE_GAP,
  COLUMN,
  controlCount,
  handleOf,
  MASTER_HEIGHT,
  MAX_PINNED,
  MODULATOR_HEIGHT,
  NODE_WIDTH,
  nodeHeight,
  pinnedParams,
  ROW_GAP,
  sameStructure,
  toFlow,
} from "./toFlow";
import type { PatchNode } from "./toFlow";

// -- Graphs made by hand --

function params(count: number): Param[] {
  return Array.from({ length: count }, (_, index) => ({
    index: index * 2 + 1, // not the position: the plugin's own numbers are not 0, 1, 2…
    name: `p${String(index)}`,
    min: 0,
    max: 1,
    default: 0,
    value: 0.5,
    labels: null,
    unit: null,
    group: [],
    stepCount: 0,
  }));
}

const plugin = (count: number, id = "builtin:osc"): Plugin => ({ id, format: "Built-in", isInstrument: true, params: params(count), presets: [], currentPreset: null });

const instrumentOf = (id: NodeId, withPlugin: Plugin | null, parent: NodeId | null = null): Node => ({
  id,
  parent,
  name: `Instrument ${String(id)}`,
  kind: "instrument",
  plugin: withPlugin,
  range: null,
  transpose: 0,
  volume: 1,
});

const instrument = (id: NodeId, count = 3, parent: NodeId | null = null): Node => instrumentOf(id, plugin(count), parent);

const effect = (id: NodeId, parent: NodeId, count = 3): Node => ({ id, parent, name: `Effect ${String(id)}`, kind: "effect", plugin: plugin(count, "builtin:filter"), mix: 1 });

const modulator = (id: NodeId, parent: NodeId): Node => ({
  id,
  parent,
  name: `Mod ${String(id)}`,
  kind: "modulator",
  source: { type: "lfo", waveform: "sine", rate: 1 },
});

const group = (id: NodeId): Node => ({ id, parent: null, name: `Group ${String(id)}`, kind: "group", volume: 1, pan: 0 });

const master: Node = { id: MASTER, parent: null, name: "Master", kind: "master" };

const audio = (from: NodeId, to: NodeId): Edge => ({ kind: "audio", from: { node: from, port: { type: "audioOut" } }, to: { node: to, port: { type: "audioIn" } } });
const member = (from: NodeId, to: NodeId): Edge => ({ kind: "member", from: { node: from, port: { type: "audioOut" } }, to: { node: to, port: { type: "audioIn" } } });
const cable = (id: number, from: NodeId, to: NodeId, param: number, amount = 0.5): Edge => ({
  kind: "modulation",
  id,
  amount,
  from: { node: from, port: { type: "modOut" } },
  to: { node: to, port: { type: "param", index: param } },
});

/** Two instruments, the first with two effects and a modulator on the second of them. */
const twoLanes: Graph = {
  nodes: [instrument(1), effect(2, 1), effect(3, 1), modulator(4, 1), instrument(5, 8), effect(6, 5, 12), master],
  edges: [audio(1, 2), audio(2, 3), audio(3, MASTER), audio(5, 6), audio(6, MASTER), cable(0, 4, 3, 1)],
};

/**
 * A lane outside a group, then two lanes in it, with a bus effect and a modulator on the
 * group. (As the server lists them: the group first, then the lanes.)
 */
const grouped: Graph = {
  nodes: [
    group(10),
    effect(11, 10),
    modulator(12, 10),
    instrument(4),
    effect(5, 4),
    effect(6, 4),
    instrument(1, 3, 10),
    effect(2, 1),
    instrument(3, 3, 10),
    master,
  ],
  edges: [
    audio(10, 11),
    audio(11, MASTER),
    audio(1, 2),
    member(2, 10),
    member(3, 10),
    audio(4, 5),
    audio(5, 6),
    audio(6, MASTER),
    cable(0, 12, 11, 1),
  ],
};

function nodeOf(graph: Graph, id: NodeId): Node {
  const found = graph.nodes.find((node) => node.id === id);
  if (!found) throw new Error(`the graph has no node ${String(id)}`);
  return found;
}

function at(flow: ReturnType<typeof toFlow>, id: NodeId): PatchNode {
  const found = flow.nodes.find((node) => node.id === String(id));
  if (!found) throw new Error(`no node ${String(id)} on the canvas`);
  return found;
}

/** The rectangles of a flow's nodes, by the sizes the layout assumes. */
function rectangles(graph: Graph) {
  const flow = toFlow(graph);
  return flow.nodes.map((placed) => {
    const node = graph.nodes.find((candidate) => String(candidate.id) === placed.id);
    if (!node) throw new Error("a node of the canvas that the graph does not have");
    return { id: node.id, left: placed.position.x, top: placed.position.y, right: placed.position.x + NODE_WIDTH, bottom: placed.position.y + nodeHeight(node, placed.data.pinned.length) };
  });
}

describe("the nodes on the canvas", () => {
  test("are one for each node of the session, named by its id, of its kind, all the same width and fixed", () => {
    const flow = toFlow(tracerSnapshot.graph);
    expect(flow.nodes.map((node) => [node.id, node.type])).toEqual([
      ["0", "instrument"],
      ["1", "effect"],
      ["2", "modulator"],
      [String(MASTER), "master"],
    ]);
    for (const node of flow.nodes) {
      expect(node.width).toBe(NODE_WIDTH);
      expect(node.draggable).toBe(false);
      expect(node.deletable).toBe(false);
      expect(node.data.node).toBe(Number(node.id));
    }
  });

  test("are told their names, and the format of their plugins when they have one", () => {
    const flow = toFlow(tracerSnapshot.graph);
    expect(flow.nodes.map((node) => [node.data.name, node.data.format])).toEqual([
      ["Oscillator", "Built-in"],
      ["Filter", "Built-in"],
      ["LFO 0.5Hz sine", null],
      ["Master", null],
    ]);
    const empty: Graph = { nodes: [instrumentOf(1, null)], edges: [] };
    expect(at(toFlow(empty), 1).data.format).toBeNull();
    const formats: Graph = { nodes: [{ ...effect(2, 1), plugin: { ...plugin(1), format: "CLAP" } } as Node], edges: [] };
    expect(at(toFlow(formats), 2).data.format).toBe("CLAP");
  });

  test("have an instrument and its effects in a row, left to right, in the order of the chain, with room for a cable between", () => {
    expect(COLUMN).toBeGreaterThan(NODE_WIDTH);
    const flow = toFlow(twoLanes);
    expect([1, 2, 3].map((id) => at(flow, id).position.x)).toEqual([0, COLUMN, 2 * COLUMN]);
    expect(new Set([1, 2, 3].map((id) => at(flow, id).position.y)).size).toBe(1);
    expect([5, 6].map((id) => at(flow, id).position.x)).toEqual([0, COLUMN]);
    expect(at(flow, 5).position.y).toBe(at(flow, 6).position.y);
    // The modulator drives the second effect, and is over it, not over the audio's first stop.
    expect(at(flow, 4).position.x).toBe(2 * COLUMN);
  });

  test("have the lanes one below the other, in the order of the session, with a gap", () => {
    const flow = toFlow(twoLanes);
    const first = at(flow, 1).position.y;
    const second = at(flow, 5).position.y;
    // Room for the modulators above the first, then the tallest of its row, then the gap.
    const tallest = Math.max(...[1, 2, 3].map((id) => nodeHeight(nodeOf(twoLanes, id), at(flow, id).data.pinned.length)));
    expect(first).toBe(MODULATOR_HEIGHT + CABLE_GAP);
    expect(second).toBe(first + tallest + ROW_GAP);
  });

  test("have the next lane below the tallest node of a row, not the first or the last", () => {
    const graph: Graph = {
      nodes: [instrumentOf(1, plugin(3)), effect(2, 1, 12), effect(3, 1, 1), instrument(4, 2), master],
      edges: [audio(1, 2), audio(2, 3), audio(3, MASTER), audio(4, MASTER)],
    };
    const flow = toFlow(graph);
    const heights = [1, 2, 3].map((id) => nodeHeight(nodeOf(graph, id), at(flow, id).data.pinned.length));
    expect(heights[1]).toBeGreaterThan(heights[0] ?? 0);
    expect(heights[1]).toBeGreaterThan(heights[2] ?? 0);
    expect(at(flow, 4).position.y).toBe(at(flow, 1).position.y + (heights[1] ?? 0) + ROW_GAP);
  });

  test("know how tall the modulators and the master are", () => {
    expect(nodeHeight(modulator(1, 0), 0)).toBe(MODULATOR_HEIGHT);
    expect(nodeHeight(master, 0)).toBe(MASTER_HEIGHT);
  });

  test("have the modulators above their strip, over the node each drives", () => {
    const flow = toFlow(tracerSnapshot.graph);
    const lfo = at(flow, 2);
    const filter = at(flow, 1);
    expect(lfo.position.x).toBe(filter.position.x);
    expect(lfo.position.y).toBe(0);
    expect(filter.position.y).toBe(MODULATOR_HEIGHT + CABLE_GAP);
    expect(lfo.position.y + MODULATOR_HEIGHT + CABLE_GAP).toBe(filter.position.y);
  });

  test("have a modulator that drives nothing in the first column, and give several of them a column each", () => {
    const graph: Graph = {
      nodes: [instrument(1), effect(2, 1), modulator(3, 1), modulator(4, 1), modulator(5, 1), master],
      edges: [audio(1, 2), audio(2, MASTER), cable(0, 4, 2, 1)],
    };
    const flow = toFlow(graph);
    // 4 drives the effect (column 1); 3 and 5 drive nothing and take the first columns that are free.
    expect(at(flow, 4).position.x).toBe(COLUMN);
    expect(at(flow, 3).position.x).toBe(0);
    expect(at(flow, 5).position.x).toBe(COLUMN * 2);
    expect(new Set([3, 4, 5].map((id) => at(flow, id).position.y))).toEqual(new Set([0]));
  });

  test("have two modulators that drive the same node side by side and not on top of each other", () => {
    const graph: Graph = {
      nodes: [instrument(1), effect(2, 1), modulator(3, 1), modulator(4, 1), master],
      edges: [audio(1, 2), audio(2, MASTER), cable(0, 3, 2, 1), cable(1, 4, 2, 3)],
    };
    const flow = toFlow(graph);
    expect(at(flow, 3).position.x).toBe(COLUMN);
    expect(at(flow, 4).position.x).toBe(COLUMN * 2);
  });

  test("have a modulator over the first of the nodes it drives", () => {
    const graph: Graph = {
      nodes: [instrument(1), effect(2, 1), effect(3, 1), modulator(4, 1), master],
      edges: [audio(1, 2), audio(2, 3), audio(3, MASTER), cable(0, 4, 3, 1), cable(1, 4, 2, 1)],
    };
    expect(at(toFlow(graph), 4).position.x).toBe(COLUMN);
  });

  test("have the master to the right of everything, level with the middle of the lanes", () => {
    const flow = toFlow(twoLanes);
    const rects = rectangles(twoLanes);
    const others = rects.filter((rect) => rect.id !== MASTER);
    const m = rects.find((rect) => rect.id === MASTER);
    expect(m?.left).toBe(Math.max(...others.map((rect) => rect.left)) + COLUMN);
    expect(m?.left).toBeGreaterThanOrEqual(Math.max(...others.map((rect) => rect.right)));
    // The span of the lanes runs from the top of the canvas to the bottom of the lowest node.
    const bottom = Math.max(...others.map((rect) => rect.bottom));
    expect(m && (m.top + m.bottom) / 2).toBeCloseTo(bottom / 2, 6);
    expect(at(flow, MASTER).type).toBe("master");
  });

  test("have the master level with the middle of the groups too, when they reach lower than the lanes", () => {
    // A group nobody is a member of goes below the lanes, and the master is in the middle of all of it.
    const graph: Graph = { nodes: [instrument(1), group(10), master], edges: [audio(1, MASTER), audio(10, MASTER)] };
    const rects = rectangles(graph);
    const others = rects.filter((rect) => rect.id !== MASTER);
    const m = rects.find((rect) => rect.id === MASTER);
    const bottom = Math.max(...others.map((rect) => rect.bottom));
    const lanes = Math.max(...others.filter((rect) => rect.id === 1).map((rect) => rect.bottom));
    expect(bottom).toBeGreaterThan(lanes);
    expect(m && (m.top + m.bottom) / 2).toBeCloseTo(bottom / 2, 6);
  });

  test("have a group to the right of the lanes, its row level with the middle of its members' rows, its bus effects after it", () => {
    const flow = toFlow(grouped);
    const groupColumn = 3 * COLUMN; // the lane of 4 has an instrument and two effects: three columns
    expect(at(flow, 10).position.x).toBe(groupColumn);
    expect(at(flow, 11).position.x).toBe(groupColumn + COLUMN);
    expect(at(flow, 10).position.y).toBe(at(flow, 11).position.y);
    // Its members are lanes 1 and 3, the second and the third of the session's lanes, not lane 4.
    const members = [1, 3].map((id) => at(flow, id).position.y);
    expect(at(flow, 4).position.y).toBeLessThan(Math.min(...members));
    expect(at(flow, 10).position.y).toBe((members[0] ?? 0) / 2 + (members[1] ?? 0) / 2);
    // The group's modulator is over it, and the master after its strip.
    expect(at(flow, 12).position.y).toBe(at(flow, 10).position.y - (MODULATOR_HEIGHT + CABLE_GAP));
    expect(at(flow, MASTER).position.x).toBe(groupColumn + 2 * COLUMN);
  });

  test("have a group that would have its modulator above the top of the canvas lower, so that it is not", () => {
    // The group's only member is the first lane, at the top: the modulator needs room above the row.
    const graph: Graph = {
      nodes: [group(10), modulator(11, 10), instrument(1, 3, 10), master],
      edges: [member(1, 10), audio(10, MASTER)],
    };
    const flow = toFlow(graph);
    expect(at(flow, 11).position.y).toBeGreaterThanOrEqual(0);
    expect(at(flow, 10).position.y).toBe(at(flow, 11).position.y + MODULATOR_HEIGHT + CABLE_GAP);
  });

  test("have the groups in the order of their members, whatever the order of the session's list", () => {
    // Group 10 comes first in the list, but its member is the lower lane.
    const graph: Graph = {
      nodes: [group(10), group(11), instrument(1, 3, 11), instrument(2, 3, 10), master],
      edges: [member(1, 11), member(2, 10), audio(10, MASTER), audio(11, MASTER)],
    };
    const flow = toFlow(graph);
    expect(at(flow, 11).position.y).toBe(at(flow, 1).position.y);
    expect(at(flow, 10).position.y).toBe(at(flow, 2).position.y);
    expect(at(flow, 11).position.y).toBeLessThan(at(flow, 10).position.y);
  });

  test("have a group with no members below the lanes, and several groups apart", () => {
    const graph: Graph = {
      nodes: [instrument(1), group(10), group(11), effect(12, 11), master],
      edges: [audio(1, MASTER), audio(10, MASTER), audio(11, 12), audio(12, MASTER)],
    };
    const rects = rectangles(graph);
    const g10 = rects.find((rect) => rect.id === 10);
    const g11 = rects.find((rect) => rect.id === 11);
    expect(g10 && g11 && (g10.bottom + ROW_GAP <= g11.top || g11.bottom + ROW_GAP <= g10.top)).toBe(true);
  });

  test("do not overlap, whatever the session", () => {
    const orphan: Graph = { nodes: [...twoLanes.nodes, effect(99, 12345)], edges: twoLanes.edges };
    const cases: [string, Graph][] = [
      ["the tracer", tracerSnapshot.graph],
      ["two lanes", twoLanes],
      ["groups", grouped],
      ["a node whose parent is not there", orphan],
      ["nothing but the master", { nodes: [master], edges: [] }],
    ];
    for (const [name, graph] of cases) {
      const rects = rectangles(graph);
      for (const [i, a] of rects.entries()) {
        for (const b of rects.slice(i + 1)) {
          const apart = a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top;
          expect(apart, `${name}: ${String(a.id)} and ${String(b.id)}`).toBe(true);
        }
      }
    }
  });

  test("put a node whose parent is not there in a row below everything else, rather than lose it", () => {
    const graph: Graph = { nodes: [...twoLanes.nodes, effect(99, 12345), effect(98, 12345)], edges: twoLanes.edges };
    const flow = toFlow(graph);
    const rects = rectangles(graph);
    const lanesBottom = Math.max(...rects.filter((rect) => rect.id !== 99 && rect.id !== 98).map((rect) => rect.bottom));
    expect(at(flow, 99).position.y).toBeGreaterThanOrEqual(lanesBottom);
    expect(at(flow, 98).position.y).toBe(at(flow, 99).position.y);
    expect(at(flow, 98).position.x).not.toBe(at(flow, 99).position.x);
  });

  test("are none for a session that has none", () => {
    expect(toFlow({ nodes: [], edges: [] })).toEqual({ nodes: [], edges: [] });
  });

  test("do not move when what changes is a value", () => {
    const moved = structuredClone(tracerSnapshot.graph);
    const filter = moved.nodes.find((node) => node.kind === "effect");
    if (filter?.kind === "effect") {
      const first = filter.plugin.params[0];
      if (first) first.value = 0.9;
    }
    expect(toFlow(moved)).toEqual(toFlow(tracerSnapshot.graph));
  });
});

describe("the parameters a node shows", () => {
  test("are the first few of the plugin's, by position and not by the plugin's own index", () => {
    const many = plugin(12);
    const shown = pinnedParams(many, new Set());
    expect(shown).toHaveLength(MAX_PINNED);
    expect(shown).toEqual(many.params.slice(0, MAX_PINNED).map((param) => param.index));
  });

  test("and every one a modulator drives, wherever it is", () => {
    const many = plugin(12);
    const last = many.params[11]?.index ?? -1;
    const shown = pinnedParams(many, new Set([last]));
    expect(shown).toHaveLength(MAX_PINNED + 1);
    expect(shown.at(-1)).toBe(last);
    // In the plugin's order, and not twice.
    const early = many.params[2]?.index ?? -1;
    expect(pinnedParams(many, new Set([early, last]))).toEqual([...many.params.slice(0, MAX_PINNED).map((param) => param.index), last]);
  });

  test("are none for a node without a plugin, and a few for a small plugin", () => {
    expect(pinnedParams(null, new Set([1]))).toEqual([]);
    expect(pinnedParams(plugin(2), new Set())).toEqual([1, 3]);
  });

  test("are what the canvas gives each node, with the cable's target among them", () => {
    const flow = toFlow(twoLanes);
    expect(at(flow, 1).data.pinned).toEqual([1, 3, 5]);
    expect(at(flow, 5).data.pinned).toHaveLength(8);
    expect(at(flow, 6).data.pinned).toHaveLength(MAX_PINNED);
    // The tracer's LFO drives the filter's cutoff, which it shows.
    expect(at(toFlow(tracerSnapshot.graph), 1).data.pinned).toContain(0);
    expect(at(flow, 4).data.pinned).toEqual([]);
    expect(at(flow, MASTER).data.pinned).toEqual([]);
  });

  test("say which of them a cable ends at, in order", () => {
    const flow = toFlow(twoLanes);
    // The cable of twoLanes drives the second parameter (index 3) of the effect 3.
    expect(at(flow, 3).data.modulated).toEqual([1]);
    expect(at(flow, 2).data.modulated).toEqual([]);
    expect(at(flow, 4).data.modulated).toEqual([]);
    const graph: Graph = {
      nodes: [instrument(1), modulator(2, 1), master],
      edges: [audio(1, MASTER), cable(0, 2, 1, 5), cable(1, 2, 1, 1), cable(2, 2, 1, 3)],
    };
    expect(at(toFlow(graph), 1).data.modulated).toEqual([1, 3, 5]);
    expect(at(toFlow(tracerSnapshot.graph), 1).data.modulated).toEqual([0]);
    expect(at(toFlow(tracerSnapshot.graph), 0).data.modulated).toEqual([]);
  });

  test("include a parameter that is beyond the first few when a cable ends at it", () => {
    const wide = plugin(12);
    const last = wide.params[11]?.index ?? -1;
    const graph: Graph = {
      nodes: [instrumentOf(1, wide), modulator(2, 1), master],
      edges: [audio(1, MASTER), cable(0, 2, 1, last)],
    };
    expect(at(toFlow(graph), 1).data.pinned).toContain(last);
  });

  test("are counted with the node's own gain, so that the layout knows how tall it is", () => {
    expect(controlCount(instrument(1), 3)).toBe(4);
    expect(controlCount(effect(2, 1), 8)).toBe(9);
    expect(controlCount(group(3), 0)).toBe(2);
    expect(controlCount(modulator(4, 1), 0)).toBe(0);
    expect(controlCount(master, 0)).toBe(0);
    // Four controls to a row: a fifth begins a second row, a ninth a third.
    expect(nodeHeight(effect(2, 1), 3)).toBe(nodeHeight(effect(2, 1), 0));
    expect(nodeHeight(effect(2, 1), 4)).toBeGreaterThan(nodeHeight(effect(2, 1), 3));
    expect(nodeHeight(effect(2, 1), 7)).toBe(nodeHeight(effect(2, 1), 4));
    expect(nodeHeight(effect(2, 1), 8)).toBeGreaterThan(nodeHeight(effect(2, 1), 7));
  });
});

describe("the cables on the canvas", () => {
  test("carry audio from the output of one node to the input of the next", () => {
    const flow = toFlow(tracerSnapshot.graph);
    const audios = flow.edges.filter((edge) => edge.type === "audio");
    expect(audios.map((edge) => [edge.id, edge.source, edge.sourceHandle, edge.target, edge.targetHandle])).toEqual([
      ["audio:0>1", "0", "audioOut", "1", "audioIn"],
      [`audio:1>${String(MASTER)}`, "1", "audioOut", String(MASTER), "audioIn"],
    ]);
  });

  test("carry a lane into its group as audio, too", () => {
    const flow = toFlow(grouped);
    const into = flow.edges.find((edge) => edge.id === "audio:2>10");
    expect(into && [into.type, into.sourceHandle, into.targetHandle]).toEqual(["audio", "audioOut", "audioIn"]);
  });

  test("carry modulation from a modulator's output to the control it drives, with its depth", () => {
    const flow = toFlow(tracerSnapshot.graph);
    const modulation = flow.edges.filter((edge) => edge.type === "modulation");
    expect(modulation).toHaveLength(1);
    const [edge] = modulation;
    expect(edge?.source).toBe("2");
    expect(edge?.sourceHandle).toBe("modOut");
    expect(edge?.target).toBe("1");
    expect(edge?.targetHandle).toBe("param-0");
    expect(edge?.data?.amount).toBeCloseTo(0.4, 6);
    expect(edge?.id).toBe("mod:0");
    // Over the nodes it passes by, rather than behind them.
    expect(edge?.zIndex ?? 0).toBeGreaterThan(0);
  });

  test("are named so that two cables of one modulator are two", () => {
    const flow = toFlow(twoLanes);
    const ids = flow.edges.map((edge) => edge.id);
    expect(new Set(ids).size).toBe(ids.length);
    const graph: Graph = {
      nodes: twoLanes.nodes,
      edges: [...twoLanes.edges, cable(7, 4, 3, 3), cable(8, 4, 3, 5)],
    };
    const two = toFlow(graph).edges.filter((edge) => edge.type === "modulation");
    expect(two.map((edge) => edge.id)).toEqual(["mod:0", "mod:7", "mod:8"]);
  });

  test("are not drawn when they end where the canvas has no handle: a modulator's rate, stages or depth", () => {
    const rate: Edge = { kind: "modulation", id: 5, amount: 0.3, from: { node: 4, port: { type: "modOut" } }, to: { node: 9, port: { type: "rate" } } };
    const depth: Edge = { kind: "modulation", id: 6, amount: 0.3, from: { node: 4, port: { type: "modOut" } }, to: { node: 9, port: { type: "depth", edge: 0 } } };
    const graph: Graph = { nodes: [...twoLanes.nodes, modulator(9, 1)], edges: [...twoLanes.edges, rate, depth] };
    const flow = toFlow(graph);
    expect(flow.edges.some((edge) => edge.id === "mod:5" || edge.id === "mod:6")).toBe(false);
    expect(flow.edges).toHaveLength(toFlow(twoLanes).edges.length);
  });

  test("cannot be deleted from the canvas", () => {
    for (const edge of toFlow(twoLanes).edges) expect(edge.deletable).toBe(false);
  });

  test("have handles named after the ports they end at", () => {
    expect(handleOf({ type: "audioIn" })).toBe("audioIn");
    expect(handleOf({ type: "audioOut" })).toBe("audioOut");
    expect(handleOf({ type: "modOut" })).toBe("modOut");
    expect(handleOf({ type: "param", index: 7 })).toBe("param-7");
    for (const port of [{ type: "rate" }, { type: "attack" }, { type: "decay" }, { type: "sustain" }, { type: "release" }, { type: "depth", edge: 3 }] as const) {
      expect(handleOf(port)).toBeNull();
    }
  });
});

describe("whether two graphs are laid out the same", () => {
  const base = tracerSnapshot.graph;

  test("they are when they are the same, or only values differ", () => {
    expect(sameStructure(base, base)).toBe(true);
    const changed = structuredClone(base);
    for (const node of changed.nodes) {
      if (node.kind === "instrument") node.volume = 0.2;
      if (node.kind === "effect") {
        node.mix = 0.3;
        for (const param of node.plugin.params) param.value = param.max;
      }
    }
    expect(sameStructure(base, changed)).toBe(true);
    expect(sameStructure(changed, base)).toBe(true);
  });

  test("they are when only an unrelated part of an unchanged node is a new object", () => {
    const copy: Graph = { nodes: base.nodes.map((node) => ({ ...node })), edges: [...base.edges] };
    expect(sameStructure(base, copy)).toBe(true);
  });

  test("they are not when a node is added, removed or renamed", () => {
    expect(sameStructure(base, { nodes: [...base.nodes, effect(77, 0)], edges: base.edges })).toBe(false);
    expect(sameStructure(base, { nodes: base.nodes.slice(1), edges: base.edges })).toBe(false);
    const renamed = structuredClone(base);
    const first = renamed.nodes[0];
    if (first) first.name = "Another";
    expect(sameStructure(base, renamed)).toBe(false);
  });

  test("they are not when a node is of another kind, the rest being the same", () => {
    const left: Graph = { nodes: [group(5)], edges: [] };
    const right: Graph = { nodes: [{ id: 5, parent: null, name: "Group 5", kind: "master" }], edges: [] };
    expect(sameStructure(left, right)).toBe(false);
    expect(sameStructure(right, left)).toBe(false);
  });

  test("they are not when a node moves in the tree, or changes kind or id", () => {
    const moved = structuredClone(base);
    const effectNode = moved.nodes.find((node) => node.kind === "effect");
    if (effectNode) effectNode.parent = null;
    expect(sameStructure(base, moved)).toBe(false);
    const renumbered = structuredClone(base);
    const lfo = renumbered.nodes.find((node) => node.kind === "modulator");
    if (lfo) lfo.id = 50;
    expect(sameStructure(base, renumbered)).toBe(false);
  });

  test("they are not when a plugin has other parameters, or is another plugin", () => {
    const fewer = structuredClone(base);
    const effectNode = fewer.nodes.find((node) => node.kind === "effect");
    if (effectNode?.kind === "effect") effectNode.plugin.params.pop();
    expect(sameStructure(base, fewer)).toBe(false);
    expect(sameStructure(fewer, base)).toBe(false);
    const other = structuredClone(base);
    const instrumentNode = other.nodes.find((node) => node.kind === "instrument");
    if (instrumentNode?.kind === "instrument" && instrumentNode.plugin) instrumentNode.plugin.id = "builtin:other";
    expect(sameStructure(base, other)).toBe(false);
    const renumbered = structuredClone(base);
    const filter = renumbered.nodes.find((node) => node.kind === "effect");
    if (filter?.kind === "effect") {
      const first = filter.plugin.params[0];
      if (first) first.index = 40;
    }
    expect(sameStructure(base, renumbered)).toBe(false);
    const emptied = structuredClone(base);
    const lane = emptied.nodes.find((node) => node.kind === "instrument");
    if (lane?.kind === "instrument") lane.plugin = null;
    expect(sameStructure(base, emptied)).toBe(false);
    expect(sameStructure(emptied, base)).toBe(false);
  });

  test("they are not when a modulator is another kind of source", () => {
    const other = structuredClone(base);
    const lfo = other.nodes.find((node) => node.kind === "modulator");
    if (lfo?.kind === "modulator") lfo.source = { type: "envelope", attack: 0.1, decay: 0.1, sustain: 0.5, release: 0.1 };
    expect(sameStructure(base, other)).toBe(false);
  });

  test("they are not when a cable is added, removed, retargeted or changes its depth", () => {
    expect(sameStructure(base, { nodes: base.nodes, edges: base.edges.slice(1) })).toBe(false);
    expect(sameStructure(base, { nodes: base.nodes, edges: [...base.edges, cable(9, 2, 1, 1)] })).toBe(false);
    const retargeted = structuredClone(base);
    for (const edge of retargeted.edges) if (edge.kind === "modulation") edge.to.port = { type: "param", index: 1 };
    expect(sameStructure(base, retargeted)).toBe(false);
    const deeper = structuredClone(base);
    for (const edge of deeper.edges) if (edge.kind === "modulation") edge.amount = 0.9;
    expect(sameStructure(base, deeper)).toBe(false);
  });
});
