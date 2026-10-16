import { describe, expect, test } from "vitest";

import type { Edge, Graph, Node, NodeId, Param, Plugin } from "../../protocol";
import { MASTER, tracerSnapshot } from "../../client/fixtures";
import { DEFAULT_KNOBS } from "../../client/pinning";
import {
  CABLE_GAP,
  CABLE_ROW_HEIGHT,
  CABLES_GAP,
  COLUMN,
  controlCount,
  depthHandle,
  handleOf,
  MASTER_HEIGHT,
  ENVELOPE_NODE_HEIGHT,
  MODULATOR_HEIGHT,
  MODULATOR_SETTINGS_HEIGHT,
  NODE_SCOPE_HEIGHT,
  NODE_WIDTH,
  nodeHeight,
  PATTERN_HEIGHT,
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

const plugin = (count: number, id = "builtin:osc"): Plugin => ({ id, format: "Built-in", isInstrument: true, params: params(count), presets: [], currentPreset: null, pinned: [] });

const instrumentOf = (id: NodeId, withPlugin: Plugin | null, parent: NodeId | null = null): Node => ({
  id,
  parent,
  name: `Instrument ${String(id)}`,
  kind: "instrument",
  plugin: withPlugin,
  range: null,
  transpose: 0,
  volume: 1,
  pattern: null,
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

const envelope = (id: NodeId, parent: NodeId): Node => ({ id, parent, name: "ADSR", kind: "modulator", source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 } });
const control = (id: NodeId, parent: NodeId): Node => ({ id, parent, name: "CC 74", kind: "modulator", source: { type: "midiLearn", control: { type: "cc", number: 74 }, learning: false } });

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
    return { id: node.id, left: placed.position.x, top: placed.position.y, right: placed.position.x + NODE_WIDTH, bottom: placed.position.y + nodeHeight(node, placed.data.knobs.length, placed.data.cables.length, placed.data.scope) };
  });
}

/** The first pair of nodes of `flow` that overlap, by the sizes the layout assumes, or null. */
function overlapsIn(graph: Graph, flow: ReturnType<typeof toFlow>): string | null {
  const rects = flow.nodes.map((placed) => {
    const node = nodeOf(graph, Number(placed.id));
    return { id: node.id, left: placed.position.x, top: placed.position.y, right: placed.position.x + NODE_WIDTH, bottom: placed.position.y + nodeHeight(node, placed.data.knobs.length, placed.data.cables.length, placed.data.scope) };
  });
  for (const [i, a] of rects.entries()) {
    for (const b of rects.slice(i + 1)) {
      if (!(a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top)) return `${String(a.id)} and ${String(b.id)}`;
    }
  }
  return null;
}

/** How tall a modulator's node is in `graph`, as the layout assumes: it has a row for each cable that starts at it. */
function tall(graph: Graph, id: NodeId): number {
  const cables = graph.edges.filter((edge) => edge.kind === "modulation" && edge.from.node === id).length;
  return nodeHeight(nodeOf(graph, id), 0, cables);
}

describe("the height of a node", () => {
  test("is as much more for a lane as its pattern under its controls is, which no other node has: a lane and an effect with as many rows of controls differ by it", () => {
    // (A lane has the volume, the keys and the group beside its knobs, an effect only its mix: one knob and three make a row of four each.)
    expect(controlCount(nodeOf(twoLanes, 1), 1)).toBe(controlCount(nodeOf(twoLanes, 2), 3));
    expect(nodeHeight(nodeOf(twoLanes, 1), 1) - nodeHeight(nodeOf(twoLanes, 2), 3)).toBe(PATTERN_HEIGHT);
    // A group, and a modulator, have none (a group has two controls and no pattern).
    expect(nodeHeight(nodeOf(grouped, 10), 0)).toBe(nodeHeight(nodeOf(twoLanes, 2), 3));
  });
});

describe("the nodes on the canvas", () => {
  test("are one for each node of the session, named by its id, of its kind, all the same width, which can be moved and not taken out", () => {
    const flow = toFlow(tracerSnapshot.graph);
    expect(flow.nodes.map((node) => [node.id, node.type])).toEqual([
      ["0", "instrument"],
      ["1", "effect"],
      ["2", "modulator"],
      [String(MASTER), "master"],
    ]);
    for (const node of flow.nodes) {
      expect(node.width).toBe(NODE_WIDTH);
      expect(node.draggable).toBe(true);
      expect(node.deletable).toBe(false);
      expect(node.data.node).toBe(Number(node.id));
    }
  });

  test("are told their names, and the format of their plugins when they have one", () => {
    const flow = toFlow(tracerSnapshot.graph);
    expect(flow.nodes.map((node) => [node.data.name, node.data.format])).toEqual([
      ["Oscillator", "Built-in"],
      ["Filter", "Built-in"],
      ["LFO", null],
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
    const tallest = Math.max(...[1, 2, 3].map((id) => nodeHeight(nodeOf(twoLanes, id), at(flow, id).data.knobs.length)));
    expect(first).toBe(tall(twoLanes, 4) + CABLE_GAP);
    expect(second).toBe(first + tallest + ROW_GAP);
  });

  test("have the next lane below the tallest node of a row, not the first or the last", () => {
    const graph: Graph = {
      nodes: [instrumentOf(1, plugin(3)), effect(2, 1, 12), effect(3, 1, 1), instrument(4, 2), master],
      edges: [audio(1, 2), audio(2, 3), audio(3, MASTER), audio(4, MASTER)],
    };
    const flow = toFlow(graph);
    const heights = [1, 2, 3].map((id) => nodeHeight(nodeOf(graph, id), at(flow, id).data.knobs.length));
    expect(heights[1]).toBeGreaterThan(heights[0] ?? 0);
    expect(heights[1]).toBeGreaterThan(heights[2] ?? 0);
    expect(at(flow, 4).position.y).toBe(at(flow, 1).position.y + (heights[1] ?? 0) + ROW_GAP);
  });

  test("know how tall the modulators and the master are: a modulator with settings has a row of controls more, and an envelope the shape of its settings too", () => {
    const envelope: Node = { id: 2, parent: 0, name: "ADSR", kind: "modulator", source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 } };
    const control: Node = { id: 3, parent: 0, name: "CC 74", kind: "modulator", source: { type: "midiLearn", control: { type: "cc", number: 74 }, learning: false } };
    expect(nodeHeight(modulator(1, 0), 0)).toBe(MODULATOR_SETTINGS_HEIGHT);
    expect(nodeHeight(envelope, 0)).toBe(ENVELOPE_NODE_HEIGHT);
    expect(nodeHeight(control, 0)).toBe(MODULATOR_HEIGHT);
    expect(MODULATOR_SETTINGS_HEIGHT).toBeGreaterThan(MODULATOR_HEIGHT);
    // (Measured in a browser: 315 for an envelope's node, 249 for an LFO's. The layout assumes a little more of each, never less.)
    expect(ENVELOPE_NODE_HEIGHT).toBeGreaterThanOrEqual(315);
    expect(ENVELOPE_NODE_HEIGHT).toBeLessThanOrEqual(325);
    expect(nodeHeight(master, 0)).toBe(MASTER_HEIGHT);
    // (Measured in a browser: 223 for the master's, with its scope and the choices of the view under its meter.)
    expect(MASTER_HEIGHT).toBeGreaterThanOrEqual(223);
    expect(MASTER_HEIGHT).toBeLessThanOrEqual(235);
  });

  test("leave room above a strip for the tallest of its modulators, and no more", () => {
    const control: Node = { id: 8, parent: 1, name: "CC 74", kind: "modulator", source: { type: "midiLearn", control: { type: "cc", number: 74 }, learning: false } };
    const only: Graph = { nodes: [instrument(1), control, master], edges: [audio(1, MASTER)] };
    expect(at(toFlow(only), 1).position.y).toBe(MODULATOR_HEIGHT + CABLE_GAP);
    const both: Graph = { nodes: [instrument(1), control, modulator(9, 1), master], edges: [audio(1, MASTER)] };
    expect(at(toFlow(both), 1).position.y).toBe(MODULATOR_SETTINGS_HEIGHT + CABLE_GAP);
    const lfoOnly: Graph = { nodes: [instrument(1), modulator(9, 1), master], edges: [audio(1, MASTER)] };
    expect(at(toFlow(lfoOnly), 1).position.y).toBe(MODULATOR_SETTINGS_HEIGHT + CABLE_GAP);
    // An envelope is taller than an LFO (it draws its shape): the room is its.
    const envelope: Node = { id: 10, parent: 1, name: "ADSR", kind: "modulator", source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 } };
    const withEnvelope: Graph = { nodes: [instrument(1), modulator(9, 1), envelope, master], edges: [audio(1, MASTER)] };
    expect(at(toFlow(withEnvelope), 1).position.y).toBe(ENVELOPE_NODE_HEIGHT + CABLE_GAP);
  });

  test("have the modulators above their strip, over the node each drives", () => {
    const flow = toFlow(tracerSnapshot.graph);
    const lfo = at(flow, 2);
    const filter = at(flow, 1);
    expect(lfo.position.x).toBe(filter.position.x);
    expect(lfo.position.y).toBe(0);
    // (The LFO lists its one cable: it is a row taller than one that has none.)
    expect(filter.position.y).toBe(tall(tracerSnapshot.graph, 2) + CABLE_GAP);
    expect(tall(tracerSnapshot.graph, 2)).toBeGreaterThan(MODULATOR_SETTINGS_HEIGHT);
    expect(lfo.position.y + tall(tracerSnapshot.graph, 2) + CABLE_GAP).toBe(filter.position.y);
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

  describe("when a modulator drives the settings of another", () => {
    /** The room a row of modulators of this graph takes above the strip: the tallest of them (each lists its cables) and the room for a cable to arrive. */
    const room = (graph: Graph, ...ids: NodeId[]): number => Math.max(...ids.map((id) => tall(graph, id))) + CABLE_GAP;
    /** A cable from the modulator `from` to a setting of the modulator `to`. */
    const drives = (id: number, from: NodeId, to: NodeId, setting: "rate" | "attack" = "rate"): Edge => ({
      kind: "modulation",
      id,
      amount: 0.5,
      from: { node: from, port: { type: "modOut" } },
      to: { node: to, port: { type: setting } },
    });
    /** No two nodes of the canvas overlap. */
    const noOverlap = (graph: Graph): void => {
      const rects = rectangles(graph);
      for (const a of rects) {
        for (const b of rects) {
          if (a.id >= b.id) continue;
          const apart = a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top;
          expect(apart, `${String(a.id)} and ${String(b.id)} overlap`).toBe(true);
        }
      }
    };

    test("is above the one it drives, in its column, with room for the cable, and the strip's row is below both", () => {
      const graph: Graph = {
        nodes: [instrument(1), effect(2, 1), modulator(3, 1), modulator(4, 1), master],
        edges: [audio(1, 2), audio(2, MASTER), cable(0, 4, 2, 1), drives(1, 3, 4)],
      };
      const flow = toFlow(graph);
      // 4 drives the effect (column 1) and is just above the row, as it was; 3 drives 4, and is above it, over the same column.
      expect(at(flow, 4).position.x).toBe(COLUMN);
      expect(at(flow, 3).position.x).toBe(COLUMN);
      expect(at(flow, 3).position.y).toBe(0);
      expect(at(flow, 4).position.y).toBe(room(graph, 3));
      expect(at(flow, 1).position.y).toBe(room(graph, 3) + room(graph, 4));
      noOverlap(graph);
    });

    test("has a row for each step of a chain, the one that drives nothing of the others' the nearest the strip", () => {
      const graph: Graph = {
        nodes: [instrument(1), effect(2, 1), modulator(3, 1), envelope(4, 1), modulator(5, 1), master],
        edges: [audio(1, 2), audio(2, MASTER), cable(0, 5, 2, 1), drives(1, 4, 5), drives(2, 3, 4, "attack")],
      };
      const flow = toFlow(graph);
      // (The envelope, in the middle row, is taller than the LFOs above and below it: it draws its shape.)
      expect(tall(graph, 4)).toBeGreaterThan(tall(graph, 3));
      expect([3, 4, 5].map((id) => at(flow, id).position.y)).toEqual([0, room(graph, 3), room(graph, 3) + room(graph, 4)]);
      expect(at(flow, 1).position.y).toBe(room(graph, 3) + room(graph, 4) + room(graph, 5));
      expect(new Set([3, 4, 5].map((id) => at(flow, id).position.x))).toEqual(new Set([COLUMN]));
      noOverlap(graph);
    });

    test("has the ones that drive the same modulator side by side, in the row above it", () => {
      const graph: Graph = {
        nodes: [instrument(1), effect(2, 1), modulator(3, 1), modulator(4, 1), modulator(5, 1), master],
        edges: [audio(1, 2), audio(2, MASTER), cable(0, 5, 2, 1), drives(1, 3, 5), drives(2, 4, 5)],
      };
      const flow = toFlow(graph);
      expect(at(flow, 5).position).toEqual({ x: COLUMN, y: room(graph, 3, 4) });
      // Both would be over the column of the one they drive: the first has it, the second the next that is free.
      expect(at(flow, 3).position).toEqual({ x: COLUMN, y: 0 });
      expect(at(flow, 4).position).toEqual({ x: 2 * COLUMN, y: 0 });
      noOverlap(graph);
    });

    test("is above all the ones it drives, however far above the strip the highest of them is", () => {
      // 3 drives 4 and 5, and 4 drives 5: 5 is nearest the strip, 4 above it, and 3 above both (whichever of the two it drives last).
      const graph: Graph = {
        nodes: [instrument(1), modulator(3, 1), modulator(4, 1), modulator(5, 1), master],
        edges: [audio(1, MASTER), drives(0, 3, 4), drives(1, 3, 5), drives(2, 4, 5)],
      };
      const flow = toFlow(graph);
      expect([3, 4, 5].map((id) => at(flow, id).position.y)).toEqual([0, room(graph, 3), room(graph, 3) + room(graph, 4)]);
      const reversed: Graph = { ...graph, edges: [audio(1, MASTER), drives(1, 3, 5), drives(0, 3, 4), drives(2, 4, 5)] };
      expect([3, 4, 5].map((id) => at(toFlow(reversed), id).position.y)).toEqual([0, room(reversed, 3), room(reversed, 3) + room(reversed, 4)]);
      noOverlap(graph);
    });

    test("is above the one it drives even if it drives a plugin too, over the first of what it drives", () => {
      const graph: Graph = {
        nodes: [instrument(1), effect(2, 1), modulator(3, 1), modulator(4, 1), master],
        edges: [audio(1, 2), audio(2, MASTER), cable(0, 3, 1, 1), cable(1, 4, 2, 1), drives(2, 3, 4)],
      };
      const flow = toFlow(graph);
      expect(at(flow, 4).position).toEqual({ x: COLUMN, y: room(graph, 3) });
      // (3 drives the instrument, in column 0, and the modulator 4, in column 1: it is over the first.)
      expect(at(flow, 3).position).toEqual({ x: 0, y: 0 });
      noOverlap(graph);
    });

    test("with nothing else to drive is over the one it drives, wherever that is", () => {
      const graph: Graph = {
        nodes: [instrument(1), effect(2, 1), effect(3, 1), modulator(4, 1), modulator(5, 1), master],
        edges: [audio(1, 2), audio(2, 3), audio(3, MASTER), cable(0, 5, 3, 1), drives(1, 4, 5)],
      };
      const flow = toFlow(graph);
      expect(at(flow, 5).position.x).toBe(2 * COLUMN);
      expect(at(flow, 4).position.x).toBe(2 * COLUMN);
    });

    test("is laid out, one above the other, when each drives the other: the first in the order of the graph is above", () => {
      const graph: Graph = {
        nodes: [instrument(1), modulator(3, 1), modulator(4, 1), master],
        edges: [audio(1, MASTER), drives(0, 3, 4), drives(1, 4, 3)],
      };
      const flow = toFlow(graph);
      expect(at(flow, 3).position.y).toBeLessThan(at(flow, 4).position.y);
      expect(at(flow, 4).position.y + room(graph, 4)).toBe(at(flow, 1).position.y);
      noOverlap(graph);
    });

    test("is laid out when it drives itself, by a cable that the controller does not allow", () => {
      const graph: Graph = { nodes: [instrument(1), modulator(3, 1), master], edges: [audio(1, MASTER), drives(0, 3, 3)] };
      expect(at(toFlow(graph), 3).position.y).toBe(0);
      expect(at(toFlow(graph), 1).position.y).toBe(room(graph, 3));
    });

    test("is as tall as the tallest in its row: a MIDI control above an envelope takes the room it needs", () => {
      const graph: Graph = {
        nodes: [instrument(1), control(3, 1), envelope(4, 1), master],
        edges: [audio(1, MASTER), drives(0, 3, 4, "attack")],
      };
      const flow = toFlow(graph);
      // (The control lists its cable to the envelope: it is a row taller than a control that drives nothing.)
      expect(tall(graph, 3)).toBe(MODULATOR_HEIGHT + CABLES_GAP + CABLE_ROW_HEIGHT);
      expect(at(flow, 4).position.y).toBe(tall(graph, 3) + CABLE_GAP);
      expect(at(flow, 3).position.y).toBe(0);
      expect(at(flow, 1).position.y).toBe(tall(graph, 3) + CABLE_GAP + ENVELOPE_NODE_HEIGHT + CABLE_GAP);
      noOverlap(graph);
    });

    test("is above the modulator whose cable's depth it drives, as for a setting: in its column, with room for the cable", () => {
      // 3 drives the effect; 4, which comes after it, drives the depth of that cable (which is a row of the node of 3).
      const depth: Edge = { kind: "modulation", id: 1, amount: 0.5, from: { node: 4, port: { type: "modOut" } }, to: { node: 3, port: { type: "depth", edge: 0 } } };
      const graph: Graph = {
        nodes: [instrument(1), effect(2, 1), modulator(3, 1), modulator(4, 1), master],
        edges: [audio(1, 2), audio(2, MASTER), cable(0, 3, 2, 1), depth],
      };
      const flow = toFlow(graph);
      expect(at(flow, 3).position).toEqual({ x: COLUMN, y: room(graph, 4) });
      expect(at(flow, 4).position).toEqual({ x: COLUMN, y: 0 });
      expect(at(flow, 1).position.y).toBe(room(graph, 4) + room(graph, 3));
      noOverlap(graph);
    });

    test("is a row of the chain that a depth makes of a setting too: the one that drives the depth of a cable of the one that drives a rate", () => {
      // 3 drives the rate of 4; 4 drives the effect, and 5 drives the depth of 4's cable: 4 is the nearest the strip, and 3 and 5 side by side above it.
      const depth: Edge = { kind: "modulation", id: 2, amount: 0.5, from: { node: 5, port: { type: "modOut" } }, to: { node: 4, port: { type: "depth", edge: 0 } } };
      const graph: Graph = {
        nodes: [instrument(1), effect(2, 1), modulator(3, 1), modulator(4, 1), modulator(5, 1), master],
        edges: [audio(1, 2), audio(2, MASTER), cable(0, 4, 2, 1), drives(1, 3, 4), depth],
      };
      const flow = toFlow(graph);
      expect(at(flow, 4).position).toEqual({ x: COLUMN, y: room(graph, 3, 5) });
      expect([3, 5].map((id) => at(flow, id).position.y)).toEqual([0, 0]);
      expect(new Set([3, 5].map((id) => at(flow, id).position.x)).size).toBe(2);
      noOverlap(graph);
    });

    test("is not a row of its own when it drives the depth of a cable of a modulator of another strip: a group's of a member lane's", () => {
      // (The group's modulator lists as many cables in both: one to a parameter of the member's plugin, and another to a parameter or a depth.)
      const base: Graph = {
        nodes: [group(10), modulator(11, 10), instrument(1, 3, 10), modulator(12, 1), master],
        edges: [member(1, 10), audio(10, MASTER), cable(0, 12, 1, 1), cable(1, 11, 1, 1), cable(2, 11, 1, 3)],
      };
      const across: Graph = { ...base, edges: [...base.edges.slice(0, 4), { kind: "modulation", id: 2, amount: 0.5, from: { node: 11, port: { type: "modOut" } }, to: { node: 12, port: { type: "depth", edge: 0 } } }] };
      for (const id of [10, 11, 1, 12, MASTER]) expect(at(toFlow(across), id).position, `node ${String(id)}`).toEqual(at(toFlow(base), id).position);
      expect(at(toFlow(across), 11).data.cables.map((row) => row.label)).toEqual(["Instrument 1: p0", "depth of Mod 12 → Instrument 1: p0"]);
    });

    test("lays a rack out as it always was while none of its modulators drives another", () => {
      const graph: Graph = {
        nodes: [instrument(1), effect(2, 1), modulator(3, 1), modulator(4, 1), modulator(5, 1), master],
        edges: [audio(1, 2), audio(2, MASTER), cable(0, 4, 2, 1)],
      };
      const flow = toFlow(graph);
      expect([3, 4, 5].map((id) => at(flow, id).position)).toEqual([
        { x: 0, y: 0 },
        { x: COLUMN, y: 0 },
        { x: 2 * COLUMN, y: 0 },
      ]);
      expect(at(flow, 1).position.y).toBe(room(graph, 3, 4, 5));
    });

    test("is above the one it drives in a group's rack, over the group's row", () => {
      const graph: Graph = {
        nodes: [group(10), modulator(11, 10), modulator(12, 10), instrument(1, 3, 10), master],
        edges: [member(1, 10), audio(10, MASTER), drives(0, 11, 12)],
      };
      const flow = toFlow(graph);
      expect(at(flow, 12).position.y).toBe(at(flow, 10).position.y - room(graph, 12));
      expect(at(flow, 11).position.y).toBe(at(flow, 12).position.y - room(graph, 11));
      expect(at(flow, 11).position.y).toBeGreaterThanOrEqual(0);
      expect(at(flow, 11).position.x).toBe(at(flow, 12).position.x);
      noOverlap(graph);
    });

    test("is not a row of its own when it drives a modulator of another strip: a group's of a member lane's", () => {
      // (The group's modulator lists a cable more in `across`: `base` has one to a parameter of the member's plugin, which is no row of modulators either.)
      const base: Graph = {
        nodes: [group(10), modulator(11, 10), instrument(1, 3, 10), modulator(12, 1), master],
        edges: [member(1, 10), audio(10, MASTER), cable(0, 11, 1, 1)],
      };
      const across: Graph = { ...base, edges: [...base.edges.slice(0, 2), drives(0, 11, 12)] };
      for (const id of [10, 11, 1, 12, MASTER]) expect(at(toFlow(across), id).position, `node ${String(id)}`).toEqual(at(toFlow(base), id).position);
    });

    test("keeps the lanes below one another with room for the rows above each, and no node on another", () => {
      const graph: Graph = {
        nodes: [instrument(1), effect(2, 1), modulator(3, 1), modulator(4, 1), instrument(5), modulator(6, 5), master],
        edges: [audio(1, 2), audio(2, MASTER), audio(5, MASTER), cable(0, 4, 2, 1), drives(1, 3, 4)],
      };
      const flow = toFlow(graph);
      // The second lane's row is below the first's, which has two rows of modulators above it; its own modulator is above it.
      expect(at(flow, 1).position.y).toBe(room(graph, 3) + room(graph, 4));
      expect(at(flow, 6).position.y).toBeGreaterThan(at(flow, 1).position.y);
      noOverlap(graph);
    });
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
    expect(at(flow, 12).position.y).toBe(at(flow, 10).position.y - (tall(grouped, 12) + CABLE_GAP));
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
    expect(at(flow, 10).position.y).toBe(at(flow, 11).position.y + MODULATOR_SETTINGS_HEIGHT + CABLE_GAP);
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

  /** Whether no two of the rectangles overlap; the pair that does, if two do. */
  function overlapping(graph: Graph): string | null {
    const rects = rectangles(graph);
    for (const [i, a] of rects.entries()) {
      for (const b of rects.slice(i + 1)) {
        const apart = a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top;
        if (!apart) return `${String(a.id)} [${String(a.left)}, ${String(a.top)}] and ${String(b.id)} [${String(b.left)}, ${String(b.top)}]`;
      }
    }
    return null;
  }

  test("do not overlap when a lane has more modulators than nodes in its row, over the groups and the master that come after it", () => {
    const cases: [string, Graph][] = [
      [
        "a lane in a group, with two modulators and no effect, and a group modulator",
        {
          nodes: [group(10), modulator(11, 10), instrument(1, 3, 10), modulator(2, 1), envelope(3, 1), master],
          edges: [member(1, 10), audio(10, MASTER), cable(0, 11, 1, 1), cable(1, 2, 1, 3), cable(2, 3, 1, 5)],
        },
      ],
      [
        "two lanes outside a group, the second with two modulators",
        {
          nodes: [instrument(1), instrument(2), modulator(3, 2), envelope(4, 2), master],
          edges: [audio(1, MASTER), audio(2, MASTER), cable(0, 3, 2, 1), cable(1, 4, 2, 3)],
        },
      ],
      [
        "four modulators on a lane of one node, a group of three above its row, and a bus effect",
        {
          nodes: [group(10), effect(11, 10), modulator(12, 10), envelope(13, 10), control(14, 10), instrument(1, 3, 10), modulator(2, 1), envelope(3, 1), control(4, 1), modulator(5, 1), master],
          edges: [member(1, 10), audio(10, 11), audio(11, MASTER)],
        },
      ],
      [
        "a group with more modulators than it has nodes, and the master after it",
        {
          nodes: [group(10), modulator(11, 10), envelope(12, 10), control(13, 10), instrument(1, 3, 10), master],
          edges: [member(1, 10), audio(10, MASTER)],
        },
      ],
    ];
    for (const [name, graph] of cases) {
      expect(overlapping(graph), name).toBeNull();
    }
  });

  test("do not overlap, whatever the session is made of (graphs of every shape, made up)", () => {
    // A small generator with a seed, so that a graph that overlaps can be made again.
    let state = 20260101;
    const next = (below: number): number => {
      state = (state * 1103515245 + 12345) % 2147483648;
      return Math.floor((state / 2147483648) * below);
    };
    for (let round = 0; round < 400; round += 1) {
      const nodes: Node[] = [];
      const edges: Edge[] = [];
      let id = 1;
      const groups = Array.from({ length: next(3) }, () => id++);
      const modulatorsFor = (parent: NodeId) => {
        for (let i = next(5); i > 0; i -= 1) {
          const make = [modulator, envelope, control][next(3)];
          nodes.push((make ?? modulator)(id++, parent));
        }
      };
      for (const g of groups) {
        nodes.push(group(g));
        for (let i = next(3); i > 0; i -= 1) nodes.push(effect(id++, g));
        modulatorsFor(g);
      }
      for (let lane = next(4) + 1; lane > 0; lane -= 1) {
        const parent = groups.length > 0 && next(2) === 0 ? (groups[next(groups.length)] ?? null) : null;
        const head = id++;
        nodes.push(instrument(head, 1 + next(12), parent));
        for (let i = next(3); i > 0; i -= 1) nodes.push(effect(id++, head, 1 + next(12)));
        modulatorsFor(head);
      }
      nodes.push(master);
      // A cable from a modulator to the node that is first in its scope, for some of them: where they go is part of the layout.
      for (const node of nodes) {
        if (node.kind !== "modulator" || next(2) === 0) continue;
        const aimed = nodes.filter((other) => (other.kind === "effect" || other.kind === "instrument") && (other.parent === node.parent || other.id === node.parent));
        const target = aimed[next(aimed.length)];
        if (target) edges.push(cable(edges.length, node.id, target.id, 1));
      }
      const graph: Graph = { nodes, edges };
      expect(overlapping(graph), `round ${String(round)}: ${JSON.stringify(nodes.map((n) => [n.id, n.kind, n.parent]))}`).toBeNull();
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
    expect(toFlow({ nodes: [], edges: [] })).toEqual({ nodes: [], edges: [], places: new Map(), arranged: true });
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

describe("where the nodes are", () => {
  /** `graph` with the places in `places` on the nodes they are for. */
  const placed = (graph: Graph, places: Record<number, { x: number; y: number }>): Graph => ({
    ...graph,
    nodes: graph.nodes.map((node) => {
      const ui = places[node.id];
      return ui ? { ...node, ui } : node;
    }),
  });

  test("a node the session has placed is drawn there, and one it has not is where the layout puts it", () => {
    const auto = toFlow(twoLanes);
    const flow = toFlow(placed(twoLanes, { 3: { x: 1234, y: -567.5 }, [MASTER]: { x: 9, y: 8 } }));
    expect(at(flow, 3).position).toEqual({ x: 1234, y: -567.5 });
    expect(at(flow, MASTER).position).toEqual({ x: 9, y: 8 });
    for (const id of [1, 2, 4, 5, 6]) expect(at(flow, id).position, `node ${String(id)}`).toEqual(at(auto, id).position);
  });

  test("a place that is the origin is a place, and not none", () => {
    const flow = toFlow(placed(twoLanes, { 2: { x: 0, y: 0 } }));
    expect(at(flow, 2).position).toEqual({ x: 0, y: 0 });
    expect(at(toFlow(twoLanes), 2).position).not.toEqual({ x: 0, y: 0 });
  });

  test("every kind of node can be placed: a lane, an effect, a modulator, a group, a bus effect, the master", () => {
    const places: Record<number, { x: number; y: number }> = {};
    for (const node of grouped.nodes) places[node.id] = { x: node.id * 10, y: -node.id };
    const flow = toFlow(placed(grouped, places));
    for (const node of grouped.nodes) expect(at(flow, node.id).position, `node ${String(node.id)}`).toEqual({ x: node.id * 10, y: -node.id });
  });

  test("the layout of the nodes that are not placed does not leave room for one that was: they are where they would be", () => {
    // (A person who drags a lane away has the others stay, and the space it leaves is theirs to use.)
    const auto = toFlow(grouped);
    const flow = toFlow(placed(grouped, { 4: { x: 5000, y: 5000 } }));
    for (const node of grouped.nodes) {
      if (node.id !== 4) expect(at(flow, node.id).position, `node ${String(node.id)}`).toEqual(at(auto, node.id).position);
    }
  });

  test("the place on the canvas is a copy: the graph's own is not the canvas's to change", () => {
    const graph = placed(twoLanes, { 2: { x: 1, y: 2 } });
    const flow = toFlow(graph);
    expect(at(flow, 2).position).not.toBe(nodeOf(graph, 2).ui);
  });
});

describe("a node that the layout places, where a person has placed another", () => {
  const placed = (graph: Graph, places: Record<number, { x: number; y: number }>): Graph => ({
    ...graph,
    nodes: graph.nodes.map((node) => {
      const ui = places[node.id];
      return ui ? { ...node, ui } : node;
    }),
  });

  /** The nodes of `graph` that overlap, as the layout sizes them: the first pair, or null. */
  function overlap(graph: Graph): string | null {
    const rects = rectangles(graph);
    for (const [i, a] of rects.entries()) {
      for (const b of rects.slice(i + 1)) {
        if (!(a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top)) return `${String(a.id)} and ${String(b.id)}`;
      }
    }
    return null;
  }

  const auto = toFlow(twoLanes);
  /** Where the layout puts a node of `twoLanes` when nothing is placed. */
  const slot = (id: NodeId): { x: number; y: number } => at(auto, id).position;

  test("is below it, in its column, with the gap between rows, when it would be on it: an effect added where a node was put", () => {
    // (The person has put the first effect of the lane where the second would be. The second goes below it, and not on it.)
    const graph = placed(twoLanes, { 2: slot(3) });
    const flow = toFlow(graph);
    expect(at(flow, 2).position).toEqual(slot(3));
    const heightOf2 = nodeHeight(nodeOf(graph, 2), at(flow, 2).data.knobs.length, 0);
    expect(at(flow, 3).position).toEqual({ x: slot(3).x, y: slot(3).y + heightOf2 + ROW_GAP });
    expect(overlap(graph)).toBeNull();
  });

  test("is above it when it is a modulator, which belongs above its strip: its bottom a gap above the node's top", () => {
    const graph = placed(twoLanes, { 3: slot(4) });
    const flow = toFlow(graph);
    expect(at(flow, 3).position).toEqual(slot(4));
    expect(at(flow, 4).position).toEqual({ x: slot(4).x, y: slot(4).y - ROW_GAP - tall(graph, 4) });
    expect(overlap(graph)).toBeNull();
  });

  test("is moved when it would be nearer than a little to a placed node, on any side of it, and not when it is as far as that", () => {
    // (The layout's sizes are about what the components are, to a few pixels: a node that is almost on another is on it.)
    const flat = nodeOf(twoLanes, 2);
    const wide = NODE_WIDTH;
    const high = nodeHeight(flat, at(auto, 2).data.knobs.length, 0);
    const target = slot(3);
    const sides: [string, (gap: number) => { x: number; y: number }][] = [
      ["to the right", (gap) => ({ x: target.x + wide + gap, y: target.y })],
      ["to the left", (gap) => ({ x: target.x - wide - gap, y: target.y })],
      ["below", (gap) => ({ x: target.x, y: target.y + high + gap })],
      ["above", (gap) => ({ x: target.x, y: target.y - high - gap })],
    ];
    for (const [side, put] of sides) {
      const near = at(toFlow(placed(twoLanes, { 2: put(15) })), 3).position;
      const apart = at(toFlow(placed(twoLanes, { 2: put(16) })), 3).position;
      expect(near, `15 pixels from it, ${side}`).not.toEqual(target);
      expect(apart, `16 pixels from it, ${side}`).toEqual(target);
    }
  });

  test("do not land on one another when several would be on the same node: the next goes below the last", () => {
    // Two lanes of effects in the same columns; a tall node placed over the third column of both.
    const lanes: Graph = {
      nodes: [instrument(1), effect(2, 1), effect(3, 1), instrument(4), effect(5, 4), effect(6, 4), instrument(7, 8), master],
      edges: [audio(1, 2), audio(2, 3), audio(3, MASTER), audio(4, 5), audio(5, 6), audio(6, MASTER)],
    };
    const base = toFlow(lanes);
    const graph = placed(lanes, { 7: { x: at(base, 3).position.x, y: 100 } });
    expect(overlap(graph)).toBeNull();
    // (Both were over it, so both moved, one below the other.)
    const [first, second] = [at(toFlow(graph), 3).position.y, at(toFlow(graph), 6).position.y].sort((a, b) => a - b);
    expect(first).toBeGreaterThan(100);
    expect(second).toBeGreaterThan((first ?? 0) + 100);
  });

  test("leaves every node that is clear of the placed ones where the layout puts it, and every placed node where it was put", () => {
    // (Placed far from all of them: nothing moves. And placed over one, only that one does.)
    const far = toFlow(placed(twoLanes, { 5: { x: 9000, y: 9000 } }));
    for (const node of twoLanes.nodes) {
      if (node.id !== 5) expect(at(far, node.id).position, `node ${String(node.id)}`).toEqual(slot(node.id));
    }
    const over = toFlow(placed(twoLanes, { 3: slot(2) }));
    expect(at(over, 3).position).toEqual(slot(2));
    for (const node of twoLanes.nodes) {
      if (![2, 3].includes(node.id)) expect(at(over, node.id).position, `node ${String(node.id)}`).toEqual(slot(node.id));
    }
    expect(at(over, 2).position).not.toEqual(slot(2));
  });

  test("do not overlap any other node, wherever a node is put: each node of a session put on each of the others' places", () => {
    const cases: [string, Graph][] = [
      ["two lanes", twoLanes],
      ["groups", grouped],
      ["the tracer", tracerSnapshot.graph],
    ];
    for (const [name, graph] of cases) {
      const base = toFlow(graph);
      for (const moved of graph.nodes) {
        for (const target of graph.nodes) {
          if (moved.id === target.id) continue;
          const there = placed(graph, { [moved.id]: at(base, target.id).position });
          expect(overlap(there), `${name}: ${String(moved.id)} put where ${String(target.id)} is`).toBeNull();
        }
      }
    }
  });

  test("do not move when nothing is placed, or when the node that is placed is a node of the layout itself, where it is", () => {
    const flow = toFlow(twoLanes);
    for (const node of twoLanes.nodes) {
      const same = toFlow(placed(twoLanes, { [node.id]: at(flow, node.id).position }));
      for (const other of twoLanes.nodes) expect(at(same, other.id).position, `${String(other.id)} with ${String(node.id)} placed where it is`).toEqual(at(flow, other.id).position);
    }
  });
});

describe("a layout that remembers where it put the nodes", () => {
  // A person who draws a cable does not expect the canvas to be rearranged under their hand: a node that the layout has put somewhere stays there when the session changes, and
  // what is new goes where the layout would put it, level with the strip it is of, and clear of what is there. The layout puts every node where it would have put it only when it
  // is asked to (`toFlow` with nothing remembered), which is what the control that arranges the nodes again does.
  const rack: Graph = {
    nodes: [instrument(1), effect(2, 1), modulator(3, 1), modulator(4, 1), master],
    edges: [audio(1, 2), audio(2, MASTER)],
  };
  const toRate: Edge = { kind: "modulation", id: 5, amount: 0.3, from: { node: 3, port: { type: "modOut" } }, to: { node: 4, port: { type: "rate" } } };
  const withCable: Graph = { nodes: rack.nodes, edges: [...rack.edges, toRate] };
  const positions = (flow: ReturnType<typeof toFlow>): Record<string, { x: number; y: number }> => Object.fromEntries(flow.nodes.map((node) => [node.id, node.position]));

  test("a cable from one modulator to a setting of another moves no node, though a layout that has not seen them puts the one that drives a tier above", () => {
    const before = toFlow(rack);
    const after = toFlow(withCable, before.places);
    expect(positions(after)).toEqual(positions(before));
    // (Without the memory the driver is a row above the one it drives and the strip below makes room for the two rows: that is what a person saw.)
    expect(at(toFlow(withCable), 4).position).not.toEqual(at(before, 4).position);
    expect(at(toFlow(withCable), 1).position).not.toEqual(at(before, 1).position);
  });

  test("nothing moves either when the cable is taken out again, or when a cable goes to a parameter, or to the depth of a cable", () => {
    const before = toFlow(rack);
    const drawn = toFlow(withCable, before.places);
    expect(positions(toFlow(rack, drawn.places))).toEqual(positions(before));
    const toParam: Graph = { nodes: rack.nodes, edges: [...rack.edges, cable(6, 3, 2, 1)] };
    expect(positions(toFlow(toParam, before.places))).toEqual(positions(before));
    const depth: Graph = { nodes: rack.nodes, edges: [...withCable.edges, { kind: "modulation", id: 7, amount: 0.2, from: { node: 4, port: { type: "modOut" } }, to: { node: 3, port: { type: "depth", edge: 5 } } }] };
    expect(positions(toFlow(depth, drawn.places))).toEqual(positions(before));
  });

  test("a layout of nothing remembered is the layout of every node where it goes, and says so", () => {
    const flow = toFlow(withCable);
    expect(flow.arranged).toBe(true);
    // (A layout that kept every node where it was is not, once what is drawn would have put a node elsewhere.)
    expect(toFlow(withCable, toFlow(rack).places).arranged).toBe(false);
    expect(toFlow(rack, toFlow(rack).places).arranged).toBe(true);
  });

  test("a node that was not remembered, an effect added at the end of a chain, goes next in the row of its lane, level with the others, though a tier is above it now", () => {
    const before = toFlow(rack);
    const drawn = toFlow(withCable, before.places);
    const longer: Graph = { nodes: [instrument(1), effect(2, 1), effect(6, 1), modulator(3, 1), modulator(4, 1), master], edges: [...withCable.edges, audio(6, MASTER)] };
    const after = toFlow(longer, drawn.places);
    expect(at(after, 6).position).toEqual({ x: at(before, 2).position.x + COLUMN, y: at(before, 2).position.y });
    for (const id of [1, 2, 3, 4]) expect(at(after, id).position, `node ${String(id)}`).toEqual(at(before, id).position);
    // (The master is the end of the canvas: it goes along, a column to the right of the lane that is longer.)
    expect(at(after, MASTER).position).toEqual({ x: at(before, MASTER).position.x + COLUMN, y: at(before, MASTER).position.y });
  });

  test("an effect that is moved takes the place of the one it passes, and they are the only nodes that move", () => {
    const chain: Graph = { nodes: [instrument(1), effect(2, 1), effect(3, 1), effect(4, 1), modulator(5, 1), master], edges: [] };
    const before = toFlow(chain);
    const swapped: Graph = { nodes: [instrument(1), effect(3, 1), effect(2, 1), effect(4, 1), modulator(5, 1), master], edges: [] };
    const after = toFlow(swapped, before.places);
    expect(at(after, 3).position).toEqual(at(before, 2).position);
    expect(at(after, 2).position).toEqual(at(before, 3).position);
    for (const id of [1, 4, 5, MASTER]) expect(at(after, id).position, `node ${String(id)}`).toEqual(at(before, id).position);
  });

  test("a node that is taken out leaves the others where they are, except the effects after it in its chain, which close up", () => {
    const chain: Graph = { nodes: [instrument(1), effect(2, 1), effect(3, 1), effect(4, 1), modulator(5, 1), modulator(6, 1), master], edges: [] };
    const before = toFlow(chain);
    const noModulator = toFlow({ nodes: chain.nodes.filter((node) => node.id !== 5), edges: [] }, before.places);
    for (const id of [1, 2, 3, 4, 6, MASTER]) expect(at(noModulator, id).position, `node ${String(id)}`).toEqual(at(before, id).position);
    const noEffect = toFlow({ nodes: chain.nodes.filter((node) => node.id !== 2), edges: [] }, before.places);
    expect(at(noEffect, 3).position).toEqual(at(before, 2).position);
    expect(at(noEffect, 4).position).toEqual(at(before, 3).position);
    for (const id of [1, 5, 6]) expect(at(noEffect, id).position, `node ${String(id)}`).toEqual(at(before, id).position);
    // (The master closes up with the lane, which is a column shorter.)
    expect(at(noEffect, MASTER).position).toEqual({ x: at(before, MASTER).position.x - COLUMN, y: at(before, MASTER).position.y });
  });

  test("a modulator that is added goes clear of the nodes that are there, in the row above its strip", () => {
    const before = toFlow(rack);
    const drawn = toFlow(withCable, before.places);
    const more: Graph = { nodes: [...withCable.nodes.slice(0, -1), modulator(8, 1), master], edges: withCable.edges };
    const after = toFlow(more, drawn.places);
    expect(overlapsIn(more, after)).toBeNull();
    // (Above the strip, where the others of its rack are, and not on the row of the strip.)
    expect(at(after, 8).position.y).toBeLessThan(at(after, 1).position.y);
    for (const id of [1, 2, 3, 4, MASTER]) expect(at(after, id).position, `node ${String(id)}`).toEqual(at(before, id).position);
  });

  test("the places of nodes that have a place of their own are the session's, and are not remembered: the node goes where the layout puts it when it has none", () => {
    const before = toFlow(rack);
    const placed: Graph = { nodes: rack.nodes.map((node) => (node.id === 2 ? { ...node, ui: { x: 900, y: 900 } } : node)), edges: rack.edges };
    const moved = toFlow(placed, before.places);
    expect(at(moved, 2).position).toEqual({ x: 900, y: 900 });
    expect(moved.places.has(2)).toBe(false);
    // Taken from where it was put (its place undone): it is laid out again, level with its lane, and does not stay where it had been.
    const back = toFlow(rack, moved.places);
    expect(at(back, 2).position).toEqual(at(before, 2).position);
  });

  test("what a node that is gone had is forgotten, and a node that is back is laid out as a new one", () => {
    const before = toFlow(rack);
    const without = toFlow({ nodes: rack.nodes.filter((node) => node.id !== 3), edges: [] }, before.places);
    expect([...without.places.keys()].sort()).toEqual([1, 2, 4, MASTER].sort());
  });

  /** `twoLanes` with the first instrument showing twelve knobs more than it did: a node a few rows taller than the place that was left for it. */
  const grownFirst: Graph = {
    nodes: twoLanes.nodes.map((node) =>
      node.id === 1 && node.kind === "instrument" && node.plugin
        ? { ...node, plugin: { ...node.plugin, params: [...node.plugin.params, ...params(12)].map((param, index) => ({ ...param, index })), pinned: Array.from({ length: 12 }, (_, index) => index) } }
        : node,
    ),
    edges: twoLanes.edges,
  };

  test("a node that grows, one that shows more knobs, makes room for itself: the strip below it goes down as it is, to where the layout has it, and the rest stays", () => {
    const before = toFlow(twoLanes);
    const grown = toFlow(grownFirst, before.places);
    const fresh = toFlow(grownFirst);
    expect(overlapsIn(grownFirst, grown)).toBeNull();
    // (The lane it is in, and the modulator above it, stay: the master is level with the middle of what is below the growth, so that it goes along.)
    for (const id of [1, 2, 3, 4]) expect(at(grown, id).position, `node ${String(id)}`).toEqual(at(before, id).position);
    // The strip below goes down by what the growth took, the instrument and its effect together, below the node that grew with the gap of a row: where the layout has them.
    expect(at(grown, 5).position.y).toBeGreaterThan(at(before, 5).position.y);
    expect(at(grown, 5).position).toEqual(at(fresh, 5).position);
    expect(at(grown, 6).position).toEqual(at(fresh, 6).position);
    expect(at(grown, 6).position.y).toBe(at(grown, 5).position.y);
  });

  /** `graph` with the plugin of the node `id` showing `extra` knobs more than it did (all of its parameters are pinned to it). */
  const grow = (graph: Graph, id: NodeId, extra: number): Graph => ({
    ...graph,
    nodes: graph.nodes.map((node) =>
      node.id === id && (node.kind === "instrument" || node.kind === "effect") && node.plugin
        ? { ...node, plugin: { ...node.plugin, params: [...node.plugin.params, ...params(extra)].map((param, index) => ({ ...param, index })), pinned: Array.from({ length: node.plugin.params.length + extra }, (_, index) => index) } }
        : node,
    ),
  });

  test("a row that comes to be over a short node of the row above and a taller one next to it is moved below both, one after the other", () => {
    const before = toFlow(twoLanes);
    // (The first instrument grows, and the effect after it by much more: the row below is over the first at once, and over the second when it is below the first.)
    const grown = grow(grow(twoLanes, 1, 5), 2, 24);
    const after = toFlow(grown, before.places);
    expect(overlapsIn(grown, after)).toBeNull();
    const bottomOf = (id: NodeId): number => at(after, id).position.y + nodeHeight(nodeOf(grown, id), at(after, id).data.knobs.length);
    expect(bottomOf(2)).toBeGreaterThan(bottomOf(1));
    expect(at(after, 5).position.y).toBe(bottomOf(2) + ROW_GAP);
    expect(at(after, 6).position.y).toBe(at(after, 5).position.y);
  });

  test("a node that room is made for and that comes to be on one that a person has placed is moved clear of it, and the one that was placed stays", () => {
    const before = toFlow(twoLanes);
    // The place is just below the first node of the second lane, clear of it, and in the way of it when it is pushed down.
    const place = { x: 20, y: at(before, 5).position.y + nodeHeight(nodeOf(twoLanes, 5), at(before, 5).data.knobs.length) + 24 };
    const withPlaced = (graph: Graph): Graph => ({ ...graph, nodes: graph.nodes.map((node) => (node.id === 3 ? { ...node, ui: place } : node)) });
    const settled = toFlow(withPlaced(twoLanes), before.places);
    expect(overlapsIn(withPlaced(twoLanes), settled)).toBeNull();
    const grown = withPlaced(grownFirst);
    const after = toFlow(grown, settled.places);
    expect(overlapsIn(grown, after)).toBeNull();
    expect(at(after, 3).position).toEqual(place);
    // (The row was pushed down by the growth, and the node that came to be on the one that was placed went below it.)
    expect(at(after, 5).position.y).toBeGreaterThan(place.y);
  });

  test("a node that grows by less than the gap below it moves nothing", () => {
    const before = toFlow(twoLanes);
    const room = at(before, 5).position.y - at(before, 1).position.y - nodeHeight(nodeOf(twoLanes, 1), at(before, 1).data.knobs.length);
    expect(room).toBeGreaterThanOrEqual(ROW_GAP);
    // (One more cable on the modulator above: its list is a row of 20 px taller, and it has the gap of a cable under it.)
    const more: Graph = { nodes: twoLanes.nodes, edges: [...twoLanes.edges, cable(1, 4, 3, 3)] };
    const after = toFlow(more, before.places);
    expect(positions(after)).toEqual(positions(before));
  });

  test("the strip of a group goes along with the group when a lane grows past it, with its bus effects and its modulators, at the height they were at, and the master goes after it", () => {
    const before = toFlow(grouped);
    const longer: Graph = { nodes: grouped.nodes.flatMap((node) => (node.id === 6 ? [node, effect(7, 4)] : [node])), edges: grouped.edges };
    const after = toFlow(longer, before.places);
    expect(overlapsIn(longer, after)).toBeNull();
    for (const id of [10, 11, 12, MASTER]) expect(at(after, id).position, `node ${String(id)}`).toEqual({ x: at(before, id).position.x + COLUMN, y: at(before, id).position.y });
    for (const id of [1, 2, 3, 4, 5, 6]) expect(at(after, id).position, `node ${String(id)}`).toEqual(at(before, id).position);
    expect(at(after, 7).position).toEqual({ x: 3 * COLUMN, y: at(before, 6).position.y });
  });

  test("the strip of a group closes up with the group when a lane is shorter, and its bus effects and its modulators go with it", () => {
    const before = toFlow(grouped);
    const shorter: Graph = { nodes: grouped.nodes.filter((node) => node.id !== 5), edges: grouped.edges.filter((edge) => !(edge.kind === "audio" && (edge.from.node === 5 || edge.to.node === 5))) };
    const after = toFlow(shorter, before.places);
    expect(overlapsIn(shorter, after)).toBeNull();
    for (const id of [10, 11, 12, MASTER]) expect(at(after, id).position, `node ${String(id)}`).toEqual({ x: at(before, id).position.x - COLUMN, y: at(before, id).position.y });
    expect(at(after, 6).position).toEqual(at(before, 5).position);
    for (const id of [1, 2, 3, 4]) expect(at(after, id).position, `node ${String(id)}`).toEqual(at(before, id).position);
  });

  test("a modulator that is added to a lane that has no room above its row makes room: the row and what is below go down, and the modulator is above its row as the layout has it", () => {
    const before = toFlow(twoLanes);
    const added: Graph = { nodes: [...twoLanes.nodes.slice(0, -1), modulator(9, 5), master], edges: twoLanes.edges };
    const after = toFlow(added, before.places);
    const fresh = toFlow(added);
    expect(overlapsIn(added, after)).toBeNull();
    for (const id of [1, 2, 3, 4]) expect(at(after, id).position, `node ${String(id)}`).toEqual(at(before, id).position);
    for (const id of [5, 6, 9]) expect(at(after, id).position, `node ${String(id)}`).toEqual(at(fresh, id).position);
    expect(at(after, 9).position.y).toBeLessThan(at(after, 5).position.y);
    expect(at(after, 5).position.y).toBeGreaterThan(at(before, 5).position.y);
  });

  test("a node that is put on one that stayed moves that one clear of it, and what is not near it stays", () => {
    const before = toFlow(twoLanes);
    const dropped: Graph = { nodes: twoLanes.nodes.map((node) => (node.id === 6 ? { ...node, ui: { x: 20, y: 380 } } : node)), edges: twoLanes.edges };
    const after = toFlow(dropped, before.places);
    expect(overlapsIn(dropped, after)).toBeNull();
    expect(at(after, 6).position).toEqual({ x: 20, y: 380 });
    // (The first instrument was under it: it goes below it, with the gap of a row.)
    expect(at(after, 1).position.x).toBe(at(before, 1).position.x);
    expect(at(after, 1).position.y).toBeGreaterThanOrEqual(380 + nodeHeight(nodeOf(dropped, 6), at(after, 6).data.knobs.length) + ROW_GAP);
    for (const id of [2, 3, 4, MASTER]) expect(at(after, id).position, `node ${String(id)}`).toEqual(at(before, id).position);
  });

  test("a lane that is added where a lane was taken out from above one that stayed goes below the one that stayed", () => {
    const lanes: Graph = { nodes: [instrument(1), effect(2, 1), instrument(3), effect(4, 3), instrument(5), effect(6, 5), master], edges: [] };
    const before = toFlow(lanes);
    const gap: Graph = { nodes: lanes.nodes.filter((node) => node.id !== 3 && node.id !== 4), edges: [] };
    const gapped = toFlow(gap, before.places);
    expect(at(gapped, 5).position).toEqual(at(before, 5).position);
    const added: Graph = { nodes: [...gap.nodes.slice(0, -1), instrument(7), master], edges: [] };
    const after = toFlow(added, gapped.places);
    expect(overlapsIn(added, after)).toBeNull();
    for (const id of [1, 2, 5, 6]) expect(at(after, id).position, `node ${String(id)}`).toEqual(at(before, id).position);
    expect(at(after, 7).position.x).toBe(0);
    expect(at(after, 7).position.y).toBeGreaterThanOrEqual(at(after, 5).position.y + nodeHeight(nodeOf(added, 5), at(after, 5).data.knobs.length) + ROW_GAP);
  });

  test("a modulator that is added to a rack that has a hole in it goes above the one that stayed where the layout would have had it", () => {
    const before = toFlow(rack);
    const noFirst: Graph = { nodes: rack.nodes.filter((node) => node.id !== 3), edges: [] };
    const closed = toFlow(noFirst, before.places);
    expect(at(closed, 4).position).toEqual(at(before, 4).position);
    const again: Graph = { nodes: [instrument(1), effect(2, 1), modulator(4, 1), modulator(5, 1), master], edges: [] };
    const after = toFlow(again, closed.places);
    expect(overlapsIn(again, after)).toBeNull();
    for (const id of [1, 2, 4, MASTER]) expect(at(after, id).position, `node ${String(id)}`).toEqual(at(closed, id).position);
    expect(at(after, 5).position.x).toBe(at(closed, 4).position.x);
    expect(at(after, 5).position.y).toBeLessThan(at(closed, 4).position.y);
  });

  test("a node that a person has placed is no obstacle at the place it was remembered at: a node that comes to it is where the layout puts it", () => {
    const before = toFlow(rack);
    // The second modulator is put somewhere else (and the first is gone) in the same change that adds another: the new one is level with the place the second had.
    const changed: Graph = { nodes: [instrument(1), effect(2, 1), { ...modulator(4, 1), ui: { x: 3000, y: -2000 } }, modulator(5, 1), master], edges: [] };
    const after = toFlow(changed, before.places);
    expect(at(after, 4).position).toEqual({ x: 3000, y: -2000 });
    expect(at(after, 5).position).toEqual(at(toFlow(changed), 5).position);
    expect(at(after, 5).position).toEqual(at(before, 4).position);
  });

  test("the layout is arranged when a node that was put on another has made it move clear, and not for a node that has a place of its own", () => {
    const dropped: Graph = { nodes: twoLanes.nodes.map((node) => (node.id === 6 ? { ...node, ui: { x: 20, y: 380 } } : node)), edges: twoLanes.edges };
    expect(toFlow(dropped).arranged).toBe(true);
    const far: Graph = { nodes: twoLanes.nodes.map((node) => (node.id === 6 ? { ...node, ui: { x: 9000, y: 9000 } } : node)), edges: twoLanes.edges };
    expect(toFlow(far).arranged).toBe(true);
    expect(toFlow(far, toFlow(twoLanes).places).arranged).toBe(true);
  });

  test("the layout is not arranged when a node stays where it was and the layout would have it in another column, though it is level with where the layout has it", () => {
    const before = toFlow(rack);
    const noFirst: Graph = { nodes: rack.nodes.filter((node) => node.id !== 3), edges: [] };
    const closed = toFlow(noFirst, before.places);
    expect(at(closed, 4).position.y).toBe(at(toFlow(noFirst), 4).position.y);
    expect(at(closed, 4).position.x).not.toBe(at(toFlow(noFirst), 4).position.x);
    expect(closed.arranged).toBe(false);
  });
});

describe("what a modulator's node can be drawn to", () => {
  test("is what it can still drive: the plugins it reaches and their parameters, but for those it drives already", () => {
    const flow = toFlow(tracerSnapshot.graph);
    const lfo = flow.nodes.find((node) => node.data.node === 2);
    expect(lfo?.data.targets.map((group) => [group.name, group.params.length])).toEqual([["Oscillator", 8], ["Filter", 3]]);
    expect(lfo?.data.targets[1]?.params.map((param) => param.name)).toEqual(["resonance", "type", "drive"]);
  });

  test("is nothing for a node that is no modulator", () => {
    const flow = toFlow(tracerSnapshot.graph);
    for (const node of flow.nodes.filter((n) => n.data.node !== 2)) expect(node.data.targets, node.data.name).toEqual([]);
  });

  test("follows the cables: one drawn takes its parameter off the list, one taken out puts it back", () => {
    const without: Graph = { ...tracerSnapshot.graph, edges: tracerSnapshot.graph.edges.filter((edge) => edge.kind !== "modulation") };
    const names = (graph: Graph) => toFlow(graph).nodes.find((node) => node.data.node === 2)?.data.targets.find((group) => group.name === "Filter")?.params.map((param) => param.name);
    expect(names(tracerSnapshot.graph)).toEqual(["resonance", "type", "drive"]);
    expect(names(without)).toEqual(["cutoff", "resonance", "type", "drive"]);
  });
});

describe("what the nodes are called", () => {
  const named = (node: Node, name: string): Node => ({ ...node, name });
  const lfo = (id: NodeId, parent: NodeId): Node => named(modulator(id, parent), "LFO");

  test("is the server's name for a node that has one of its own, and a number after it for each that shares it, from 1, in the order of the graph", () => {
    const graph: Graph = {
      nodes: [
        named(group(10), "Pad"),
        lfo(11, 10),
        named(instrument(1, 3, 10), "Oscillator"),
        named(effect(2, 1), "Filter"),
        lfo(3, 1),
        named(envelope(4, 1), "ADSR"),
        lfo(5, 1),
        named(instrument(6), "Oscillator"),
        named(effect(7, 6), "Filter"),
        named(effect(8, 6), "Reverb"),
        master,
      ],
      edges: [],
    };
    const flow = toFlow(graph);
    const names = Object.fromEntries(flow.nodes.map((node) => [node.id, node.data.name]));
    expect(names).toEqual({
      "10": "Pad",
      "11": "LFO 1", // the group's, first in the graph
      "1": "Oscillator 1",
      "2": "Filter 1",
      "3": "LFO 2",
      "4": "ADSR", // the only one
      "5": "LFO 3",
      "6": "Oscillator 2",
      "7": "Filter 2",
      "8": "Reverb",
      [String(MASTER)]: "Master",
    });
    // A screen reader hears the same: each node is a group of that name.
    expect(Object.fromEntries(flow.nodes.map((node) => [node.id, node.ariaLabel]))).toEqual(names);
  });

  test("is not numbered while it is alone, and is when another comes", () => {
    const one: Graph = { nodes: [instrument(1), lfo(2, 1), master], edges: [] };
    expect(toFlow(one).nodes.map((node) => node.data.name)).toEqual(["Instrument 1", "LFO", "Master"]);
    const two: Graph = { nodes: [instrument(1), lfo(2, 1), lfo(3, 1), master], edges: [] };
    expect(toFlow(two).nodes.map((node) => node.data.name)).toEqual(["Instrument 1", "LFO 1", "LFO 2", "Master"]);
  });

  test("tells the cables of two modulators to the same parameter apart", () => {
    const graph: Graph = {
      nodes: [instrument(1), effect(2, 1), lfo(3, 1), lfo(4, 1), master],
      edges: [audio(1, 2), audio(2, MASTER), cable(0, 3, 2, 1), cable(1, 4, 2, 1)],
    };
    expect(toFlow(graph).edges.filter((edge) => edge.type === "modulation").map((edge) => edge.data?.label)).toEqual([
      "LFO 1 → Effect 2: p0",
      "LFO 2 → Effect 2: p0",
    ]);
  });

  test("does not change what a node is called by anything but the names of the graph", () => {
    const a: Graph = { nodes: [instrument(1), lfo(2, 1), lfo(3, 1), master], edges: [] };
    const moved = structuredClone(a);
    const mod = moved.nodes[1];
    if (mod?.kind === "modulator" && mod.source.type === "lfo") mod.source.rate = 9;
    expect(toFlow(moved).nodes.map((node) => node.data.name)).toEqual(toFlow(a).nodes.map((node) => node.data.name));
  });
});

describe("the parameters a node shows", () => {
  test("are what the canvas gives each node, with the cable's target among them", () => {
    const flow = toFlow(twoLanes);
    expect(at(flow, 1).data.knobs).toEqual([1, 3, 5]);
    expect(at(flow, 5).data.knobs).toHaveLength(8);
    expect(at(flow, 6).data.knobs).toHaveLength(DEFAULT_KNOBS);
    // The tracer's LFO drives the filter's cutoff, which it shows.
    expect(at(toFlow(tracerSnapshot.graph), 1).data.knobs).toContain(0);
    expect(at(flow, 4).data.knobs).toEqual([]);
    expect(at(flow, MASTER).data.knobs).toEqual([]);
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

  test("are the ones pinned to the plugin when it has some, in order, with a cable's target after them", () => {
    const pinned: Plugin = { ...plugin(12), pinned: [23, 3] };
    const graph: Graph = {
      nodes: [instrumentOf(1, pinned), modulator(2, 1), master],
      edges: [audio(1, MASTER), cable(0, 2, 1, 9)],
    };
    expect(at(toFlow(graph), 1).data.knobs).toEqual([23, 3, 9]);
    // (And a node is as tall as it has knobs.)
    expect(nodeHeight(nodeOf(graph, 1), at(toFlow(graph), 1).data.knobs.length)).toBeLessThan(nodeHeight(nodeOf(graph, 1), DEFAULT_KNOBS));
  });

  test("lay the lane below further down when a pin makes a node of the lane a row taller, by what a row is", () => {
    // (An instrument has its volume and its keys among the controls: ten are three rows of four, thirteen are four.)
    const lanes = (pinned: number[]): Graph => ({
      nodes: [instrumentOf(1, { ...plugin(12), pinned }), instrumentOf(2, plugin(2)), master],
      edges: [audio(1, MASTER), audio(2, MASTER)],
    });
    const indices = plugin(12).params.map((param) => param.index);
    const eight = lanes(indices.slice(0, 8));
    const eleven = lanes(indices.slice(0, 11));
    const grown = at(toFlow(eleven), 2).position.y - at(toFlow(eight), 2).position.y;
    expect(grown).toBeGreaterThan(0);
    expect(grown).toBe(nodeHeight(nodeOf(eight, 1), 11) - nodeHeight(nodeOf(eight, 1), 8));
    // A pin that leaves the node as tall as it was moves nothing.
    expect(at(toFlow(lanes(indices.slice(0, 9))), 2).position.y).toBe(at(toFlow(eight), 2).position.y);
  });

  test("include a parameter that is beyond the first few when a cable ends at it", () => {
    const wide = plugin(12);
    const last = wide.params[11]?.index ?? -1;
    const graph: Graph = {
      nodes: [instrumentOf(1, wide), modulator(2, 1), master],
      edges: [audio(1, MASTER), cable(0, 2, 1, last)],
    };
    expect(at(toFlow(graph), 1).data.knobs).toContain(last);
  });

  test("are counted with the node's own gain, so that the layout knows how tall it is", () => {
    expect(controlCount(instrument(1), 3)).toBe(6); // the parameters, the volume, the keys and the group
    expect(controlCount(effect(2, 1), 8)).toBe(9);
    expect(controlCount(group(3), 0)).toBe(2);
    expect(controlCount(modulator(4, 1), 0)).toBe(0);
    expect(controlCount(master, 0)).toBe(0);
    // Four controls to a row: a fifth begins a second row, a ninth a third.
    expect(nodeHeight(effect(2, 1), 3)).toBe(nodeHeight(effect(2, 1), 0));
    expect(nodeHeight(effect(2, 1), 4)).toBeGreaterThan(nodeHeight(effect(2, 1), 3));
    expect(nodeHeight(effect(2, 1), 7)).toBe(nodeHeight(effect(2, 1), 4));
    expect(nodeHeight(effect(2, 1), 8)).toBeGreaterThan(nodeHeight(effect(2, 1), 7));
    // An instrument has its keys and its group as well as its volume: one parameter fills the first row with them, a second begins a second, a sixth a third.
    expect(nodeHeight(instrument(1), 1)).toBe(nodeHeight(instrument(1), 0));
    expect(nodeHeight(instrument(1), 2)).toBeGreaterThan(nodeHeight(instrument(1), 1));
    expect(nodeHeight(instrument(1), 5)).toBe(nodeHeight(instrument(1), 2));
    expect(nodeHeight(instrument(1), 6)).toBeGreaterThan(nodeHeight(instrument(1), 5));
  });
});

describe("the settings of a modulator that a cable ends at", () => {
  const toRate: Edge = { kind: "modulation", id: 5, amount: 0.3, from: { node: 4, port: { type: "modOut" } }, to: { node: 9, port: { type: "rate" } } };
  const toRelease: Edge = { kind: "modulation", id: 6, amount: 0.3, from: { node: 4, port: { type: "modOut" } }, to: { node: 13, port: { type: "release" } } };
  const toAttack: Edge = { kind: "modulation", id: 7, amount: 0.3, from: { node: 9, port: { type: "modOut" } }, to: { node: 13, port: { type: "attack" } } };
  const toDepth: Edge = { kind: "modulation", id: 8, amount: 0.3, from: { node: 4, port: { type: "modOut" } }, to: { node: 9, port: { type: "depth", edge: 5 } } };
  const nodes = [...twoLanes.nodes, modulator(9, 1), envelope(13, 1)];

  test("are listed in the data of the node, by the names of their handles, in the order a modulator has them", () => {
    const flow = toFlow({ nodes, edges: [...twoLanes.edges, toRate, toRelease, toAttack] });
    expect(at(flow, 9).data.drivenSettings).toEqual(["rate"]);
    expect(at(flow, 13).data.drivenSettings).toEqual(["attack", "release"]);
    // (Whichever order the cables are in.)
    expect(at(toFlow({ nodes, edges: [...twoLanes.edges, toAttack, toRelease] }), 13).data.drivenSettings).toEqual(["attack", "release"]);
  });

  test("are none for a modulator that no cable ends at, and for a node that is no modulator", () => {
    const flow = toFlow({ nodes, edges: [...twoLanes.edges, toRate, toRelease, toAttack] });
    expect(at(flow, 4).data.drivenSettings).toEqual([]);
    for (const id of [1, 2, 3, 5, 6, MASTER]) expect(at(flow, id).data.drivenSettings, `node ${String(id)}`).toEqual([]);
    for (const node of toFlow(twoLanes).nodes) expect(node.data.drivenSettings, node.data.name).toEqual([]);
  });

  test("are not the depth of a cable, nor a parameter", () => {
    const flow = toFlow({ nodes, edges: [...twoLanes.edges, toRate, toDepth] });
    expect(at(flow, 9).data.drivenSettings).toEqual(["rate"]);
    // (The cable of the tracer's cutoff ends at a parameter, which is a plugin node's to show as driven.)
    for (const node of toFlow(tracerSnapshot.graph).nodes) expect(node.data.drivenSettings, node.data.name).toEqual([]);
  });
});

describe("the cables a modulator's node lists", () => {
  // The cable of `twoLanes` (0) goes from the modulator 4 to the filter 3's first parameter; an envelope (13) of the same lane
  // drives how deep that cable goes (8), and the modulator 4 drives the envelope's release (5) and a parameter of the effect 2 (6).
  const toDepth: Edge = { kind: "modulation", id: 8, amount: 0.3, from: { node: 13, port: { type: "modOut" } }, to: { node: 4, port: { type: "depth", edge: 0 } } };
  const toRelease: Edge = { kind: "modulation", id: 5, amount: 0.3, from: { node: 4, port: { type: "modOut" } }, to: { node: 13, port: { type: "release" } } };
  const toEffect: Edge = cable(6, 4, 2, 1);
  const nodes = [...twoLanes.nodes, envelope(13, 1)];
  const rows = (edges: Edge[], id: NodeId, graphNodes: Node[] = nodes) => at(toFlow({ nodes: graphNodes, edges: [...twoLanes.edges, ...edges] }), id).data.cables;

  test("are, for a modulator, the cables that start at it, in the order of the graph, each with what it ends at in words", () => {
    expect(rows([toRelease, toEffect], 4)).toEqual([
      { edge: 0, label: "Effect 3: p0", driven: false },
      { edge: 5, label: "ADSR: release", driven: false },
      { edge: 6, label: "Effect 2: p0", driven: false },
    ]);
    // (The order of the graph, whichever way the cables were made.)
    expect(rows([toEffect, toRelease], 4).map((row) => row.edge)).toEqual([0, 6, 5]);
  });

  test("say what a cable that ends at the depth of another is: the words of that cable, which are told by where it goes", () => {
    expect(rows([toDepth], 13)).toEqual([{ edge: 8, label: "depth of Mod 4 → Effect 3: p0", driven: false }]);
  });

  test("say which of them a cable ends at the depth of, and no other", () => {
    expect(rows([toDepth, toRelease, toEffect], 4).map((row) => [row.edge, row.driven])).toEqual([
      [0, true],
      [5, false],
      [6, false],
    ]);
    // A cable that ends at a setting, or at a parameter, is not one that drives a depth.
    expect(rows([toRelease, toEffect], 4).some((row) => row.driven)).toBe(false);
  });

  test("are none for a modulator that drives nothing, and for a node that is no modulator", () => {
    const flow = toFlow({ nodes, edges: [...twoLanes.edges, toDepth] });
    expect(at(flow, 13).data.cables).toHaveLength(1);
    expect(rows([], 13)).toEqual([]);
    for (const node of flow.nodes.filter((n) => n.data.node !== 4 && n.data.node !== 13)) expect(node.data.cables, node.data.name).toEqual([]);
    expect(toFlow(grouped).nodes.find((node) => node.data.node === 12)?.data.cables).toEqual([{ edge: 0, label: "Effect 11: p0", driven: false }]);
  });

  test("make a node taller by a row for each, and by a little room before them if it has any", () => {
    const lfo = modulator(4, 1);
    expect(CABLE_ROW_HEIGHT).toBeGreaterThan(0);
    expect(nodeHeight(lfo, 0, 0)).toBe(MODULATOR_SETTINGS_HEIGHT);
    expect(nodeHeight(lfo, 0, 1)).toBe(MODULATOR_SETTINGS_HEIGHT + CABLES_GAP + CABLE_ROW_HEIGHT);
    expect(nodeHeight(lfo, 0, 3)).toBe(MODULATOR_SETTINGS_HEIGHT + CABLES_GAP + 3 * CABLE_ROW_HEIGHT);
    expect(nodeHeight(envelope(13, 1), 0, 2)).toBe(ENVELOPE_NODE_HEIGHT + CABLES_GAP + 2 * CABLE_ROW_HEIGHT);
    const control: Node = { id: 3, parent: 0, name: "CC 74", kind: "modulator", source: { type: "midiLearn", control: { type: "cc", number: 74 }, learning: false } };
    expect(nodeHeight(control, 0, 2)).toBe(MODULATOR_HEIGHT + CABLES_GAP + 2 * CABLE_ROW_HEIGHT);
    // A node that is no modulator lists none: what is passed for it is not a part of its height.
    expect(nodeHeight(instrument(1), 3, 5)).toBe(nodeHeight(instrument(1), 3, 0));
    expect(nodeHeight(master, 0, 5)).toBe(MASTER_HEIGHT);
  });

  test("leave room above a strip for them: the modulator that lists them is as tall as the layout says", () => {
    // The lane's one modulator drives two plugins of it: its node is two rows taller, and the strip is lower by that.
    const lane: Graph = { nodes: [instrument(1), effect(2, 1), modulator(4, 1), master], edges: [audio(1, 2), audio(2, MASTER), cable(0, 4, 1, 1), cable(1, 4, 2, 1)] };
    expect(at(toFlow(lane), 4).data.cables).toHaveLength(2);
    expect(at(toFlow(lane), 1).position.y).toBe(MODULATOR_SETTINGS_HEIGHT + CABLES_GAP + 2 * CABLE_ROW_HEIGHT + CABLE_GAP);
    const none: Graph = { ...lane, edges: lane.edges.slice(0, 2) };
    expect(at(toFlow(none), 1).position.y).toBe(MODULATOR_SETTINGS_HEIGHT + CABLE_GAP);
  });

  test("keep the nodes of a canvas from overlapping, with as many cables as a node has", () => {
    const many: Edge[] = [0, 1, 2, 3, 4, 5].map((id) => cable(30 + id, 4, 3, id % 2 === 0 ? 1 : 3));
    const g: Graph = { nodes: twoLanes.nodes, edges: [...twoLanes.edges.slice(0, 5), ...many] };
    const boxes = rectangles(g);
    for (const a of boxes) {
      for (const b of boxes) {
        if (a.id >= b.id) continue;
        const apart = a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top;
        expect(apart, `${String(a.id)} and ${String(b.id)}`).toBe(true);
      }
    }
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

  test("carry modulation from a modulator's output to the control it drives, as the cable that it is", () => {
    const flow = toFlow(tracerSnapshot.graph);
    const modulation = flow.edges.filter((edge) => edge.type === "modulation");
    expect(modulation).toHaveLength(1);
    const [edge] = modulation;
    expect(edge?.source).toBe("2");
    expect(edge?.sourceHandle).toBe("modOut");
    expect(edge?.target).toBe("1");
    expect(edge?.targetHandle).toBe("param-0");
    // (How deep it goes is a value, which the cable reads for itself: it is not in what the canvas is laid out by.
    // It knows its modulator, whose list the focus goes to when the cable is taken out.)
    expect(edge?.data).toEqual({ cable: 0, modulator: 2, label: "LFO → Filter: cutoff" });
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

  test("go to a modulator's rate or stage, from the modulator's output to the port of that setting, and say so", () => {
    const toRate: Edge = { kind: "modulation", id: 5, amount: 0.3, from: { node: 4, port: { type: "modOut" } }, to: { node: 9, port: { type: "rate" } } };
    const toStage: Edge = { kind: "modulation", id: 6, amount: 0.3, from: { node: 4, port: { type: "modOut" } }, to: { node: 13, port: { type: "release" } } };
    const graph: Graph = { nodes: [...twoLanes.nodes, modulator(9, 1), envelope(13, 1)], edges: [...twoLanes.edges, toRate, toStage] };
    const flow = toFlow(graph);
    const drawn = (id: string) => {
      const edge = flow.edges.find((candidate) => candidate.id === id);
      return edge && [edge.type, edge.source, edge.sourceHandle, edge.target, edge.targetHandle, edge.data];
    };
    expect(drawn("mod:5")).toEqual(["modulation", "4", "modOut", "9", "rate", { cable: 5, modulator: 4, label: "Mod 4 → Mod 9: rate" }]);
    expect(drawn("mod:6")).toEqual(["modulation", "4", "modOut", "13", "release", { cable: 6, modulator: 4, label: "Mod 4 → ADSR: release" }]);
    expect(flow.edges).toHaveLength(toFlow(twoLanes).edges.length + 2);
  });

  test("go to the depth of a cable, from the modulator's output to the port of that cable's row in the node of the modulator whose cable it is, and say so", () => {
    // The envelope (13) drives how deep the cable 0 goes, which starts at the modulator 4 and ends at the effect 3's first parameter.
    const depth: Edge = { kind: "modulation", id: 6, amount: 0.3, from: { node: 13, port: { type: "modOut" } }, to: { node: 4, port: { type: "depth", edge: 0 } } };
    const graph: Graph = { nodes: [...twoLanes.nodes, envelope(13, 1)], edges: [...twoLanes.edges, depth] };
    const flow = toFlow(graph);
    const drawn = flow.edges.find((edge) => edge.id === "mod:6");
    expect(drawn && [drawn.type, drawn.source, drawn.sourceHandle, drawn.target, drawn.targetHandle, drawn.data]).toEqual([
      "modulation",
      "13",
      "modOut",
      "4",
      "depth-0",
      { cable: 6, modulator: 13, label: "ADSR → depth of Mod 4 → Effect 3: p0" },
    ]);
    expect(flow.edges).toHaveLength(toFlow(twoLanes).edges.length + 1);
    // The row it ends at is in the node it goes to, and is marked as driven.
    expect(at(flow, 4).data.cables).toEqual([{ edge: 0, label: "Effect 3: p0", driven: true }]);
  });

  test("cannot be deleted from the canvas", () => {
    for (const edge of toFlow(twoLanes).edges) expect(edge.deletable).toBe(false);
  });

  test("have handles named after the ports they end at", () => {
    expect(handleOf({ type: "audioIn" })).toBe("audioIn");
    expect(handleOf({ type: "audioOut" })).toBe("audioOut");
    expect(handleOf({ type: "modOut" })).toBe("modOut");
    expect(handleOf({ type: "param", index: 7 })).toBe("param-7");
    // The settings of a modulator are named as what they are.
    for (const type of ["rate", "attack", "decay", "sustain", "release"] as const) expect(handleOf({ type })).toBe(type);
    // The depth of a cable is named after the cable.
    expect(handleOf({ type: "depth", edge: 3 })).toBe("depth-3");
    expect(handleOf({ type: "depth", edge: 3 })).toBe(depthHandle(3));
    expect(handleOf({ type: "depth", edge: 30 })).not.toBe(handleOf({ type: "depth", edge: 3 }));
  });
});

describe("a node that shows a scope", () => {
  // A person can look at what comes out of a lane's instrument, of an effect and of a group, in a scope in the node: the node is taller by it, and the layout has to know,
  // as it knows how many knobs a node shows, so that what is below it is not under it. Which nodes show one is the viewer's own, not the session's: `toFlow` is told.
  const positions = (flow: ReturnType<typeof toFlow>): Record<string, { x: number; y: number }> => Object.fromEntries(flow.nodes.map((node) => [node.id, node.position]));
  const shown = (flow: ReturnType<typeof toFlow>): string[] => flow.nodes.filter((node) => node.data.scope).map((node) => node.id);

  test("is taller by the height of a scope for an instrument, an effect and a group, and no other node is: a modulator has no audio, and the master has its scope always", () => {
    for (const node of [instrument(1), effect(2, 1), group(3), instrumentOf(4, null)]) {
      expect(nodeHeight(node, 3, 0, true), node.name).toBe(nodeHeight(node, 3, 0, false) + NODE_SCOPE_HEIGHT);
      // (Without being told, a node has none.)
      expect(nodeHeight(node, 3, 0), node.name).toBe(nodeHeight(node, 3, 0, false));
    }
    for (const node of [modulator(5, 1), envelope(6, 1), control(7, 1), master]) {
      expect(nodeHeight(node, 0, 2, true), node.name).toBe(nodeHeight(node, 0, 2, false));
    }
    expect(NODE_SCOPE_HEIGHT).toBeGreaterThan(0);
  });

  test("is said in the data of the node: of those asked for, the ones that can show one do, and the others, and the ones that are not there, do not", () => {
    expect(shown(toFlow(twoLanes))).toEqual([]);
    expect(shown(toFlow(twoLanes, undefined, new Set([1, 3, 6])))).toEqual(["1", "3", "6"]);
    // (A modulator and the master are not shown one, and a node that is not in the session is nothing to lay out.)
    expect(shown(toFlow(twoLanes, undefined, new Set([4, MASTER, 99])))).toEqual([]);
    expect(shown(toFlow(grouped, undefined, new Set([10, 11])))).toEqual(["10", "11"]);
  });

  test("is not shown by a lane that has no instrument, which has no signal, though it is asked for: it is as tall as it was, and shows one again when it has an instrument", () => {
    const empty: Graph = { nodes: [instrumentOf(1, null), effect(2, 1), master], edges: [audio(2, MASTER)] };
    const withInstrument: Graph = { nodes: [instrument(1), effect(2, 1), master], edges: [audio(2, MASTER)] };
    const asked = new Set([1, 2]);
    expect(shown(toFlow(empty, undefined, asked))).toEqual(["2"]);
    expect(shown(toFlow(withInstrument, undefined, asked))).toEqual(["1", "2"]);
  });

  test("lays the lane below further down by the height of the scope when the tallest node of the lane above shows one, and moves nothing else", () => {
    const closed = toFlow(twoLanes);
    const open = toFlow(twoLanes, undefined, new Set([1]));
    for (const id of [1, 2, 3, 4, MASTER]) expect(at(open, id).position.x, `x of ${String(id)}`).toBe(at(closed, id).position.x);
    for (const id of [1, 2, 3, 4]) expect(at(open, id).position.y, `y of ${String(id)}`).toBe(at(closed, id).position.y);
    for (const id of [5, 6]) expect(at(open, id).position.y - at(closed, id).position.y, `y of ${String(id)}`).toBe(NODE_SCOPE_HEIGHT);
    expect(overlapsIn(twoLanes, open)).toBeNull();
  });

  test("makes room when it is opened on a canvas that remembers where the nodes are: the rows below are moved down by the height of the scope, and the rest stays", () => {
    const closed = toFlow(twoLanes);
    const open = toFlow(twoLanes, closed.places, new Set([1]));
    expect(overlapsIn(twoLanes, open)).toBeNull();
    for (const id of [1, 2, 3, 4, MASTER]) expect(at(open, id).position, `node ${String(id)}`).toEqual(at(closed, id).position);
    for (const id of [5, 6]) expect(at(open, id).position.y - at(closed, id).position.y, `y of ${String(id)}`).toBe(NODE_SCOPE_HEIGHT);
  });

  test("leaves the nodes where they are when it is closed again, and the nodes are not arranged then, until the person arranges them", () => {
    const closed = toFlow(twoLanes);
    const open = toFlow(twoLanes, closed.places, new Set([1]));
    const again = toFlow(twoLanes, open.places, new Set());
    expect(positions(again)).toEqual(positions(open));
    expect(again.arranged).toBe(false);
    expect(shown(again)).toEqual([]);
    // (Laid out from nothing, they are where they were before.)
    expect(positions(toFlow(twoLanes, undefined, new Set()))).toEqual(positions(closed));
    expect(closed.arranged).toBe(true);
  });

  test("makes no more room than it needs: a scope on a node that has a tall one beside it, with room below it to hold the scope, moves nothing", () => {
    // (The instrument shows all twelve of its parameters, four rows of controls, and the effect after it is a row: a scope fits under the effect, in the room the instrument makes.)
    const indices = plugin(12).params.map((param) => param.index);
    const tall: Graph = {
      nodes: [instrumentOf(1, { ...plugin(12), pinned: indices }), effect(2, 1), instrument(3, 2), master],
      edges: [audio(1, 2), audio(2, MASTER), audio(3, MASTER)],
    };
    const closed = toFlow(tall);
    const room = at(closed, 3).position.y - at(closed, 2).position.y - nodeHeight(nodeOf(tall, 2), at(closed, 2).data.knobs.length);
    expect(room, "a scope fits under the effect").toBeGreaterThanOrEqual(NODE_SCOPE_HEIGHT + ROW_GAP);
    expect(positions(toFlow(tall, closed.places, new Set([2])))).toEqual(positions(closed));
    expect(positions(toFlow(tall, undefined, new Set([2])))).toEqual(positions(closed));
    // (A scope on the instrument, which is the tallest, moves the lane below, as it does anywhere.)
    expect(at(toFlow(tall, closed.places, new Set([1])), 3).position.y - at(closed, 3).position.y).toBe(NODE_SCOPE_HEIGHT);
  });

  test("puts the group below a group that has nothing in it and shows a scope, when they would be over each other, in the layout and where the places are remembered", () => {
    // (Two groups with nothing in them, so that they are as near as they can be whatever the lanes are: the second a row's gap below the first, which a scope on the first is in the way of.)
    const groups: Graph = { nodes: [group(10), group(20), master], edges: [audio(10, MASTER), audio(20, MASTER)] };
    const closed = toFlow(groups);
    expect(at(closed, 20).position.y - at(closed, 10).position.y).toBe(nodeHeight(nodeOf(groups, 10), 0) + ROW_GAP);
    expect(NODE_SCOPE_HEIGHT, "the scope is in the way, which is what this is about").toBeGreaterThan(ROW_GAP);
    const wanted = at(closed, 10).position.y + nodeHeight(nodeOf(groups, 10), 0, 0, true) + ROW_GAP;
    const fromNothing = toFlow(groups, undefined, new Set([10]));
    const remembered = toFlow(groups, closed.places, new Set([10]));
    for (const open of [fromNothing, remembered]) {
      expect(overlapsIn(groups, open)).toBeNull();
      expect(at(open, 10).position).toEqual(at(closed, 10).position);
      expect(at(open, 20).position.y).toBe(wanted);
    }
  });

  test("puts the group below a group that shows a scope further down when they would be over each other, in the layout and where the places are remembered", () => {
    // (Two lanes of one row of controls each, so that the groups are as near as they can be: the second a row's gap below the first, which a scope on the first is in the way of.)
    const groups: Graph = {
      nodes: [group(10), group(20), instrument(1, 1, 10), instrument(2, 1, 20), master],
      edges: [member(1, 10), member(2, 20), audio(10, MASTER), audio(20, MASTER)],
    };
    const closed = toFlow(groups);
    // (A row is as tall as the tallest node in it, which is the lane: it has its pattern under its controls.)
    const row = Math.max(nodeHeight(nodeOf(groups, 10), 0), nodeHeight(nodeOf(groups, 1), 1));
    expect(at(closed, 20).position.y - at(closed, 10).position.y).toBe(row + ROW_GAP);
    expect(nodeHeight(nodeOf(groups, 10), 0, 0, true), "the scope is in the way, which is what this is about").toBeGreaterThan(row);
    const wanted = at(closed, 10).position.y + nodeHeight(nodeOf(groups, 10), 0, 0, true) + ROW_GAP;
    const fromNothing = toFlow(groups, undefined, new Set([10]));
    const remembered = toFlow(groups, closed.places, new Set([10]));
    for (const open of [fromNothing, remembered]) {
      expect(overlapsIn(groups, open)).toBeNull();
      expect(at(open, 10).position).toEqual(at(closed, 10).position);
      expect(at(open, 20).position.y).toBe(wanted);
      expect(at(open, 1).position).toEqual(at(closed, 1).position);
    }
    // (The layout puts a lane where its row goes, and the group below goes where it is wanted, which is no longer level with its lane. A canvas that remembers moves the row, the lane and
    // the group that are level, together, as it does for a node that has grown.)
    expect(at(fromNothing, 2).position).toEqual(at(closed, 2).position);
    expect(at(remembered, 2).position).toEqual({ x: at(closed, 2).position.x, y: wanted });
  });

  test("keeps the nodes from lying over one another, whichever of them show a scope, laid out from nothing and on a canvas that remembers the places", () => {
    const canShow = grouped.nodes.filter((node) => node.kind === "instrument" || node.kind === "effect" || node.kind === "group").map((node) => node.id);
    const remembered = toFlow(grouped);
    for (let subset = 0; subset < 2 ** canShow.length; subset += 1) {
      const scopes = new Set(canShow.filter((_, bit) => (subset >> bit) & 1));
      const label = `scopes on ${[...scopes].join(", ") || "none"}`;
      const fresh = toFlow(grouped, undefined, scopes);
      expect(shown(fresh).map(Number).sort((a, b) => a - b), label).toEqual([...scopes].sort((a, b) => a - b));
      expect(overlapsIn(grouped, fresh), `${label}, from nothing`).toBeNull();
      expect(overlapsIn(grouped, toFlow(grouped, remembered.places, scopes)), `${label}, remembered`).toBeNull();
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

  test("they are when only a modulator's settings or a cable's depth differ: those are turned at the pace of the frames", () => {
    const turned = structuredClone(base);
    for (const edge of turned.edges) if (edge.kind === "modulation") edge.amount = -0.9;
    expect(sameStructure(base, turned)).toBe(true);
    expect(sameStructure(turned, base)).toBe(true);
    const faster = structuredClone(base);
    for (const node of faster.nodes) if (node.kind === "modulator" && node.source.type === "lfo") Object.assign(node.source, { rate: 12, waveform: "square" });
    expect(sameStructure(base, faster)).toBe(true);
    const staged = structuredClone(base);
    const envelope: Node = { id: 60, parent: 0, name: "ADSR", kind: "modulator", source: { type: "envelope", attack: 0.1, decay: 0.2, sustain: 0.3, release: 0.4 } };
    const withEnvelope: Graph = { nodes: [...staged.nodes, envelope], edges: staged.edges };
    const slower: Graph = { nodes: [...staged.nodes, { ...envelope, source: { type: "envelope", attack: 5, decay: 6, sustain: 0.9, release: 7 } }], edges: staged.edges };
    expect(sameStructure(withEnvelope, slower)).toBe(true);
  });

  test("they are when only an unrelated part of an unchanged node is a new object", () => {
    const copy: Graph = { nodes: base.nodes.map((node) => ({ ...node })), edges: [...base.edges] };
    expect(sameStructure(base, copy)).toBe(true);
  });

  test("they are not when a node is placed, moved or given back to the layout, and they are when a place is the same in another object", () => {
    const placedAt = (x: number, y: number): Graph => ({ nodes: base.nodes.map((node, i) => (i === 1 ? { ...node, ui: { x, y } } : node)), edges: base.edges });
    expect(sameStructure(base, placedAt(1, 2))).toBe(false);
    expect(sameStructure(placedAt(1, 2), base)).toBe(false);
    expect(sameStructure(placedAt(1, 2), placedAt(1, 3))).toBe(false);
    expect(sameStructure(placedAt(1, 2), placedAt(2, 2))).toBe(false);
    expect(sameStructure(placedAt(1, 2), placedAt(1, 2))).toBe(true);
  });

  test("they are not when the parameters pinned to a plugin differ (the node's knobs, and so its height), and they are when the list is the same in another object", () => {
    const pinnedTo = (params: number[]): Graph => ({
      nodes: base.nodes.map((node) => (node.kind === "effect" ? { ...node, plugin: { ...node.plugin, pinned: [...params] } } : node)),
      edges: base.edges,
    });
    expect(sameStructure(base, pinnedTo([1]))).toBe(false);
    expect(sameStructure(pinnedTo([1]), base)).toBe(false);
    expect(sameStructure(pinnedTo([1]), pinnedTo([0]))).toBe(false);
    expect(sameStructure(pinnedTo([1, 0]), pinnedTo([0, 1]))).toBe(false);
    expect(sameStructure(pinnedTo([1]), pinnedTo([1, 2]))).toBe(false);
    expect(sameStructure(pinnedTo([1, 0]), pinnedTo([1, 0]))).toBe(true);
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

  test("they are not when a cable is added, removed or retargeted", () => {
    expect(sameStructure(base, { nodes: base.nodes, edges: base.edges.slice(1) })).toBe(false);
    expect(sameStructure(base, { nodes: base.nodes, edges: [...base.edges, cable(9, 2, 1, 1)] })).toBe(false);
    const retargeted = structuredClone(base);
    for (const edge of retargeted.edges) if (edge.kind === "modulation") edge.to.port = { type: "param", index: 1 };
    expect(sameStructure(base, retargeted)).toBe(false);
    // The audio is cabled as it is too: an effect that is fed by another is not the same as one that is not.
    const rerouted = structuredClone(base);
    const audio = rerouted.edges.find((edge) => edge.kind === "audio");
    if (audio) audio.to.node = 99;
    expect(sameStructure(base, rerouted)).toBe(false);
    // (Nor when one cable is another, though it is as deep: its id is what it is.)
    const renumbered = structuredClone(base);
    for (const edge of renumbered.edges) if (edge.kind === "modulation") edge.id = 8;
    expect(sameStructure(base, renumbered)).toBe(false);
  });
});
