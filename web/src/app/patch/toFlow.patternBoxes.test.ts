import { describe, expect, test } from "vitest";

import { patternBoxesFixture } from "../../client/fixtures";
import { newBox } from "../../client/patternBoxes";
import type { Graph, Node, NodeId } from "../../protocol";
import { canScope, COLUMN, controlCount, heightOfData, NODE_WIDTH, nodeHeight, PATTERN_BOX_HEIGHT, PATTERN_HEIGHT, PATTERN_ROLL_HEIGHT, ROW_GAP, sameStructure, SOURCE_HEIGHT, toFlow } from "./toFlow";
import type { PatchNode } from "./toFlow";

// The pattern boxes in the layout of the canvas (step 124 C): how tall a box is, which column it is in, and where it stays: the session of the fixture with boxes (the keys 2, a device 3, the boxes Bass 4 and Pattern 5, the lanes 0 and 1,
// the master). A node is laid out by the sizes that the components' styles are built to (measured in a browser), so what is held here is what the layout assumes of a box and where it puts it.
const found = patternBoxesFixture.cases.find((entry) => entry.name === "boxes");
if (!found) throw new Error("the fixture has the session with boxes");
// (The fixture puts Bass at a place of its own, to try the places that a session keeps: here every node is left to the layout.)
const graph: Graph = structuredClone(found.snapshot.graph);
for (const node of graph.nodes) delete node.ui;
const [KEYS, DEVICE, BASS, PLAIN, FIRST, SECOND] = [2, 3, 4, 5, 0, 1];

const at = (flow: ReturnType<typeof toFlow>, id: NodeId): PatchNode => {
  const node = flow.nodes.find((each) => each.id === String(id));
  if (!node) throw new Error(`no node ${String(id)} on the canvas`);
  return node;
};
const nodeOf = (id: NodeId): Node => {
  const node = graph.nodes.find((each) => each.id === id);
  if (!node) throw new Error(`no node ${String(id)}`);
  return node;
};
/** `graph` without the nodes in `gone` (and the cables that end at them). */
const without = (...gone: NodeId[]): Graph => ({
  nodes: graph.nodes.filter((node) => !gone.includes(node.id)),
  edges: graph.edges.filter((edge) => !gone.includes(edge.from.node) && !gone.includes(edge.to.node)),
});

describe("how tall a pattern box is", () => {
  test("is as the browser measured it, which the styles are made to (`.tang-roll` is 40 pixels tall and the grid's gap is 8): a number that changes here is a style that changes there", () => {
    expect(PATTERN_ROLL_HEIGHT).toBe(40);
    expect(PATTERN_BOX_HEIGHT).toBe(189);
  });

  test("is its header and the pattern cell of a lane with the roll in it, as tall whatever it shows", () => {
    expect(PATTERN_BOX_HEIGHT).toBe(44 + PATTERN_HEIGHT + 8 + PATTERN_ROLL_HEIGHT);
    expect(nodeHeight(nodeOf(BASS), 0)).toBe(PATTERN_BOX_HEIGHT);
    // (Not the knobs, the cables, the scope or the other things that the other nodes are taller for.)
    expect(nodeHeight(nodeOf(PLAIN), 5, 3, true, true, true, 4)).toBe(PATTERN_BOX_HEIGHT);
  });

  test("is the height that the canvas hands the layout and the node's data says (a box has no knobs, no cables listed and no scope)", () => {
    const flow = toFlow(graph);
    expect(heightOfData(nodeOf(BASS), at(flow, BASS).data)).toBe(PATTERN_BOX_HEIGHT);
    expect(controlCount(nodeOf(BASS), 0)).toBe(0);
    expect(canScope(nodeOf(BASS))).toBe(false);
    expect(at(flow, BASS).data.scope).toBe(false);
  });

  test("is taller than a source, which has a line and not a cell of controls", () => {
    expect(PATTERN_BOX_HEIGHT).toBeGreaterThan(SOURCE_HEIGHT);
  });
});

describe("a pattern box on the canvas", () => {
  test("is a node of a kind of its own, the width of the others, that can be moved, with the data that says it is neither a lane, a source nor a module", () => {
    const flow = toFlow(graph);
    const box = at(flow, BASS);
    expect(box.type).toBe("pattern");
    expect(box.width).toBe(NODE_WIDTH);
    expect(box.draggable).toBe(true);
    expect(box.data).toMatchObject({ node: BASS, name: "Bass", format: null, knobs: [], notes: false, groupCell: false, source: null, heldIn: null, side: null, box: null, module: null });
  });

  test("is in a column of its own between the sources and the lanes, one under the other, a gap of a row apart, and level with the middle of the lanes as a whole", () => {
    const flow = toFlow(graph);
    expect([BASS, PLAIN].map((id) => at(flow, id).position.x)).toEqual([-COLUMN, -COLUMN]);
    expect([KEYS, DEVICE].map((id) => at(flow, id).position.x)).toEqual([-2 * COLUMN, -2 * COLUMN]);
    expect([FIRST, SECOND].map((id) => at(flow, id).position.x)).toEqual([0, 0]);
    // (The middle of the lanes as a whole: from the top of the first to the bottom of the last, by the heights that the canvas gives each.)
    const bottom = Math.max(...[FIRST, SECOND].map((id) => at(flow, id).position.y + heightOfData(nodeOf(id), at(flow, id).data)));
    const middle = bottom / 2;
    const top = middle - (2 * PATTERN_BOX_HEIGHT + ROW_GAP) / 2;
    expect(top).toBeGreaterThan(0);
    expect(at(flow, BASS).position.y).toBe(top);
    expect(at(flow, PLAIN).position.y - at(flow, BASS).position.y).toBe(PATTERN_BOX_HEIGHT + ROW_GAP);
  });

  test("puts the sources next to the lanes while the session has no box, as it always did, and a column further off while it has one", () => {
    const none = toFlow(without(BASS, PLAIN));
    expect([KEYS, DEVICE].map((id) => at(none, id).position.x)).toEqual([-COLUMN, -COLUMN]);
    const one = toFlow(without(PLAIN));
    expect([KEYS, DEVICE].map((id) => at(one, id).position.x)).toEqual([-2 * COLUMN, -2 * COLUMN]);
    expect(at(one, BASS).position.x).toBe(-COLUMN);
  });

  test("puts the sources a column further off for a box whatever it is set to: one that does not loop is a box too", () => {
    const quiet: Graph = { ...graph, nodes: graph.nodes.map((node) => (node.kind === "pattern" ? { ...node, looping: false, overdub: true, sync: "host", click: false, lengthBeats: 1 } : node)) };
    const flow = toFlow(quiet);
    expect([KEYS, DEVICE].map((id) => at(flow, id).position.x)).toEqual([-2 * COLUMN, -2 * COLUMN]);
    expect([BASS, PLAIN].map((id) => at(flow, id).position.x)).toEqual([-COLUMN, -COLUMN]);
  });

  test("leaves the lanes and the master where they would be without it: a box takes no room that they have", () => {
    const base = toFlow(without(BASS, PLAIN));
    const flow = toFlow(graph);
    for (const id of [FIRST, SECOND, 4294967295]) expect(at(flow, id).position, `node ${String(id)}`).toEqual(at(base, id).position);
  });

  test("starts at the top of the canvas when the boxes are taller than the lanes are, and not above it", () => {
    const many: Graph = {
      nodes: [...graph.nodes, ...[100, 101, 102, 103].map((id) => newBox(id, `Box ${String(id)}`))],
      edges: graph.edges,
    };
    const flow = toFlow(many);
    const boxes = [BASS, PLAIN, 100, 101, 102, 103];
    expect(boxes.map((id) => at(flow, id).position.y)).toEqual(boxes.map((_, i) => i * (PATTERN_BOX_HEIGHT + ROW_GAP)));
  });

  test("is not remembered as moved when the session placed it: a box that has a place is where the session says, and the layout remembers nothing of it", () => {
    const placed: Graph = { ...graph, nodes: graph.nodes.map((node) => (node.id === BASS ? { ...node, ui: { x: 33, y: -44 } } : node)) };
    const flow = toFlow(placed);
    expect(at(flow, BASS).position).toEqual({ x: 33, y: -44 });
    expect(flow.places.has(BASS)).toBe(false);
    expect(flow.places.has(PLAIN)).toBe(true);
  });
});

describe("where a pattern box and the sources stay", () => {
  test("a box stays where it was when another is added, which goes below it", () => {
    const before = toFlow(without(PLAIN));
    const after = toFlow(graph, before.places);
    expect(at(after, BASS).position).toEqual(at(before, BASS).position);
    expect(at(after, PLAIN).position.x).toBe(-COLUMN);
    expect(at(after, PLAIN).position.y).toBeGreaterThanOrEqual(at(before, BASS).position.y + PATTERN_BOX_HEIGHT + ROW_GAP);
  });

  test("a box stays where it was when one that is above it is taken out", () => {
    const before = toFlow(graph);
    const after = toFlow(without(BASS), before.places);
    expect(at(after, PLAIN).position).toEqual(at(before, PLAIN).position);
  });

  test("the sources are laid out again when the first box is added, a column further off, and again when the last goes, back next to the lanes", () => {
    const none = toFlow(without(BASS, PLAIN));
    expect(at(none, KEYS).position.x).toBe(-COLUMN);
    const added = toFlow(without(PLAIN), none.places);
    expect(at(added, KEYS).position.x).toBe(-2 * COLUMN);
    expect(at(added, BASS).position.x).toBe(-COLUMN);
    // (With the second box the sources stay where they are: they are in the slot that they were laid out for.)
    const more = toFlow(graph, added.places);
    expect(at(more, KEYS).position).toEqual(at(added, KEYS).position);
    expect(at(more, DEVICE).position).toEqual(at(added, DEVICE).position);
    const gone = toFlow(without(BASS, PLAIN), more.places);
    expect([KEYS, DEVICE].map((id) => at(gone, id).position.x)).toEqual([-COLUMN, -COLUMN]);
  });

  test("the sources are laid out again for a first box that does not loop as for one that does: whatever the box is set to, it is a box", () => {
    const none = toFlow(without(BASS, PLAIN));
    const oneBox = without(PLAIN);
    const quiet: Graph = { ...oneBox, nodes: oneBox.nodes.map((node) => (node.kind === "pattern" ? { ...node, looping: false } : node)) };
    const added = toFlow(quiet, none.places);
    expect([KEYS, DEVICE].map((id) => at(added, id).position.x)).toEqual([-2 * COLUMN, -2 * COLUMN]);
    expect(at(added, BASS).position.x).toBe(-COLUMN);
  });

  test("a source that a person put somewhere stays there when a box is added: the layout does not move what a person placed", () => {
    const placed = (g: Graph): Graph => ({ ...g, nodes: g.nodes.map((node) => (node.id === KEYS ? { ...node, ui: { x: -900, y: 40 } } : node)) });
    const before = toFlow(placed(without(BASS, PLAIN)));
    const after = toFlow(placed(graph), before.places);
    expect(at(after, KEYS).position).toEqual({ x: -900, y: 40 });
  });

  test("no node is on another: not a box, a source, a lane or the master, with or without boxes, and as they are added one by one", () => {
    let places: ReturnType<typeof toFlow>["places"] | undefined;
    const steps: Graph[] = [without(BASS, PLAIN), without(PLAIN), graph];
    for (const step of steps) {
      const flow = toFlow(step, places);
      places = flow.places;
      const rects = flow.nodes.map((node) => ({ id: node.id, left: node.position.x, top: node.position.y, right: node.position.x + NODE_WIDTH, bottom: node.position.y + heightOfData(nodeOf(Number(node.id)), node.data) }));
      for (const a of rects) {
        for (const b of rects) {
          if (a.id >= b.id) continue;
          const over = a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;
          expect(over, `${a.id} and ${b.id}`).toBe(false);
        }
      }
    }
  });
});

describe("the structure of a box", () => {
  test("is the node and its name, and not what it holds or is doing: a recording does not lay the canvas out again", () => {
    const changed: Graph = {
      ...graph,
      nodes: graph.nodes.map((node) => (node.id === BASS && node.kind === "pattern" ? { ...node, status: "recording", notes: [], lengthBeats: 16, looping: false, overdub: false, sync: "host", click: false } : node)),
    };
    expect(sameStructure(graph, changed)).toBe(true);
    const renamed: Graph = { ...graph, nodes: graph.nodes.map((node) => (node.id === BASS ? { ...node, name: "Lead" } : node)) };
    expect(sameStructure(graph, renamed)).toBe(false);
  });

  test("has its cables in the flow, by the names of its ports: notes leave a box at its output and come in at its input", () => {
    const flow = toFlow(graph);
    const cable = (from: NodeId, to: NodeId) => flow.edges.find((edge) => edge.source === String(from) && edge.target === String(to));
    expect(cable(KEYS, BASS)).toMatchObject({ type: "notes", sourceHandle: "eventsOut", targetHandle: "eventsIn" });
    expect(cable(BASS, FIRST)).toMatchObject({ type: "notes", sourceHandle: "eventsOut", targetHandle: "eventsIn" });
    expect(cable(PLAIN, SECOND)).toMatchObject({ type: "notes", sourceHandle: "eventsOut", targetHandle: "eventsIn" });
    // (The links by which the sources reach a box that no cable of notes ends at.)
    expect(cable(KEYS, PLAIN)).toMatchObject({ type: "hears", sourceHandle: "eventsOut", targetHandle: "eventsIn" });
    expect(cable(DEVICE, PLAIN)).toMatchObject({ type: "hears", sourceHandle: "eventsOut", targetHandle: "eventsIn" });
  });
});

describe("the arrangement of the nodes (the button that arranges them) with pattern boxes in the session", () => {
  test("puts every node somewhere, none on another, and the boxes where the cables of notes through them run from the sources to the lanes", () => {
    const flow = toFlow(graph, new Map(), new Set(), { arrange: true });
    expect(flow.nodes.map((node) => node.id).sort()).toEqual(graph.nodes.map((node) => String(node.id)).sort());
    const rects = flow.nodes.map((node) => ({ id: node.id, left: node.position.x, top: node.position.y, right: node.position.x + NODE_WIDTH, bottom: node.position.y + heightOfData(nodeOf(Number(node.id)), node.data) }));
    for (const a of rects) {
      for (const b of rects) {
        if (a.id >= b.id) continue;
        expect(a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom, `${a.id} and ${b.id}`).toBe(false);
      }
    }
    // (Notes go from the keys to Bass and from Bass to the first lane: left to right.)
    expect(at(flow, KEYS).position.x).toBeLessThan(at(flow, BASS).position.x);
    expect(at(flow, BASS).position.x).toBeLessThan(at(flow, FIRST).position.x);
    expect(at(flow, PLAIN).position.x).toBeLessThan(at(flow, SECOND).position.x);
  });
});
