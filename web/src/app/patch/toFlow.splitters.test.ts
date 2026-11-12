import { describe, expect, test } from "vitest";

import { splittersFixture } from "../../client/fixtures";
import type { Graph, Node, NodeId } from "../../protocol";
import { handleOf } from "./handles";
import { COLUMN, heightOfData, NODE_WIDTH, nodeHeight, ROW_GAP, sameStructure, SOURCE_HEIGHT, SPLITTER_HEIGHT, SPLITTER_REST_AT, PATTERN_BOX_HEIGHT, toFlow } from "./toFlow";
import type { PatchNode } from "./toFlow";

// The note splitters in the layout of the canvas (step 128 C): how tall a splitter is, which column it is in, where it stays, and how its cables are drawn: the session of the fixture of splitters (`testutil::SPLITTERS`), with the lanes 0, 1 and 2,
// the keys 3, a device 4, a box (Bass) 5, the splitters Bass line (6, which picks the lowest note) and Splitter (7, which picks the highest), and the cables of notes from the keys to 6, the box and the device to 7, the pick of 6 to the lane 0, its
// rest to the lane 1 and the pick of 7 to the lane 2. A node is laid out by the sizes that the components' styles are built to (measured in a browser), so what is held here is what the layout assumes of a splitter and where it puts it.
const found = splittersFixture.cases.find((entry) => entry.name === "splitters");
if (!found) throw new Error("the fixture has the session of the splitters");
// (The fixture puts the second splitter at a place of its own, to try the places that a session keeps: here every node is left to the layout.)
const graph: Graph = structuredClone(found.snapshot.graph);
for (const node of graph.nodes) delete node.ui;
const [FIRST_LANE, SECOND_LANE, THIRD_LANE, KEYS, DEVICE, BASS, SPLIT, OTHER] = [0, 1, 2, 3, 4, 5, 6, 7];

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
/** `graph` without the nodes in `gone` (and the cables that start or end at them). */
const without = (...gone: NodeId[]): Graph => ({
  nodes: graph.nodes.filter((node) => !gone.includes(node.id)),
  edges: graph.edges.filter((edge) => !gone.includes(edge.from.node) && !gone.includes(edge.to.node)),
});
const x = (flow: ReturnType<typeof toFlow>, ...ids: NodeId[]): number[] => ids.map((id) => at(flow, id).position.x);

describe("how tall a note splitter is", () => {
  test("is as the browser is to measure it, which the styles are made to (`.tang-splitter` has three rows of 22 pixels, a gap of 8, a padding of 8 above and 12 below, and a line above): a number that changes here is a style that changes there", () => {
    expect(SPLITTER_HEIGHT).toBe(147);
    expect(SPLITTER_HEIGHT).toBe(44 + 1 + 8 + 22 + 8 + 22 + 8 + 22 + 12);
    // (Where its second output is: the middle of the last row.)
    expect(SPLITTER_REST_AT).toBe(124);
    expect(SPLITTER_REST_AT).toBe(SPLITTER_HEIGHT - 12 - 11);
  });

  test("is as tall whatever it picks, shows or is given: not the knobs, the cables, the scope or the other things that the other nodes are taller for", () => {
    expect(nodeHeight(nodeOf(SPLIT), 0)).toBe(SPLITTER_HEIGHT);
    expect(nodeHeight(nodeOf(OTHER), 5, 3, true, true, true, 4)).toBe(SPLITTER_HEIGHT);
  });

  test("is the height that the canvas hands the layout and the node's data says, and is less than a pattern box's and more than a source's", () => {
    const flow = toFlow(graph);
    expect(heightOfData(nodeOf(SPLIT), at(flow, SPLIT).data)).toBe(SPLITTER_HEIGHT);
    expect(SPLITTER_HEIGHT).toBeGreaterThan(SOURCE_HEIGHT);
    expect(SPLITTER_HEIGHT).toBeLessThan(PATTERN_BOX_HEIGHT);
  });
});

describe("a note splitter on the canvas", () => {
  test("is a node of its kind, with the name that the server gave it, the width of the others, that can be moved, with the data that says it is neither a lane, a source, a box nor a module", () => {
    const flow = toFlow(graph);
    const splitter = at(flow, SPLIT);
    expect(splitter.type).toBe("splitter");
    expect(splitter.width).toBe(NODE_WIDTH);
    expect(splitter.draggable).toBe(true);
    expect(splitter.data).toMatchObject({ node: SPLIT, name: "Bass line", format: null, knobs: [], notes: false, groupCell: false, source: null, heldIn: null, side: null, box: null, module: null });
    expect(at(flow, OTHER).data.name).toBe("Splitter");
    // (Every node of the graph is on the canvas: the session has no effect held in a lane and no module.)
    expect(flow.nodes.map((node) => node.id).sort()).toEqual(graph.nodes.map((node) => String(node.id)).sort());
  });

  test("is in the column nearest the lanes, with the boxes a column further off and the sources a column further still: the order that notes go in", () => {
    const flow = toFlow(graph);
    expect(x(flow, SPLIT, OTHER)).toEqual([-COLUMN, -COLUMN]);
    expect(x(flow, BASS)).toEqual([-2 * COLUMN]);
    expect(x(flow, KEYS, DEVICE)).toEqual([-3 * COLUMN, -3 * COLUMN]);
    expect(x(flow, FIRST_LANE, SECOND_LANE, THIRD_LANE)).toEqual([0, 0, 0]);
  });

  test("has the columns that the nodes of notes need and no more: a session without boxes has the sources next to the splitters, one without splitters is as it was, and one with neither has the sources next to the lanes", () => {
    const noBoxes = toFlow(without(BASS));
    expect(x(noBoxes, SPLIT, OTHER)).toEqual([-COLUMN, -COLUMN]);
    expect(x(noBoxes, KEYS, DEVICE)).toEqual([-2 * COLUMN, -2 * COLUMN]);
    const noSplitters = toFlow(without(SPLIT, OTHER));
    expect(x(noSplitters, BASS)).toEqual([-COLUMN]);
    expect(x(noSplitters, KEYS, DEVICE)).toEqual([-2 * COLUMN, -2 * COLUMN]);
    const neither = toFlow(without(BASS, SPLIT, OTHER));
    expect(x(neither, KEYS, DEVICE)).toEqual([-COLUMN, -COLUMN]);
  });

  test("is in a column one under the other, a gap of a row apart, and level with the middle of the lanes as a whole", () => {
    const flow = toFlow(graph);
    // (The middle of the lanes as a whole: from the top of the first to the bottom of the last, by the heights that the canvas gives each.)
    const bottom = Math.max(...[FIRST_LANE, SECOND_LANE, THIRD_LANE].map((id) => at(flow, id).position.y + heightOfData(nodeOf(id), at(flow, id).data)));
    const top = bottom / 2 - (2 * SPLITTER_HEIGHT + ROW_GAP) / 2;
    expect(top).toBeGreaterThan(0);
    expect(at(flow, SPLIT).position.y).toBe(top);
    expect(at(flow, OTHER).position.y - at(flow, SPLIT).position.y).toBe(SPLITTER_HEIGHT + ROW_GAP);
  });

  test("leaves the lanes and the master where they would be without it: a splitter takes no room that they have", () => {
    const base = toFlow(without(SPLIT, OTHER));
    const flow = toFlow(graph);
    for (const id of [FIRST_LANE, SECOND_LANE, THIRD_LANE, 4294967295]) expect(at(flow, id).position, `node ${String(id)}`).toEqual(at(base, id).position);
  });

  test("is not remembered as moved when the session placed it: a splitter that has a place is where the session says, and the layout remembers nothing of it", () => {
    const placed: Graph = { ...graph, nodes: graph.nodes.map((node) => (node.id === SPLIT ? { ...node, ui: { x: 33, y: -44 } } : node)) };
    const flow = toFlow(placed);
    expect(at(flow, SPLIT).position).toEqual({ x: 33, y: -44 });
    expect(flow.places.has(SPLIT)).toBe(false);
    expect(flow.places.has(OTHER)).toBe(true);
  });
});

describe("where a note splitter, the boxes and the sources stay", () => {
  test("a splitter stays where it was when another is added, which goes below it", () => {
    const before = toFlow(without(OTHER));
    const after = toFlow(graph, before.places);
    expect(at(after, SPLIT).position).toEqual(at(before, SPLIT).position);
    expect(at(after, OTHER).position.x).toBe(-COLUMN);
    expect(at(after, OTHER).position.y).toBeGreaterThanOrEqual(at(before, SPLIT).position.y + SPLITTER_HEIGHT + ROW_GAP);
  });

  test("a splitter stays where it was when one that is above it is taken out", () => {
    const before = toFlow(graph);
    const after = toFlow(without(SPLIT), before.places);
    expect(at(after, OTHER).position).toEqual(at(before, OTHER).position);
  });

  test("the boxes and the sources are laid out again when the first splitter is added, a column further off each, and again when the last goes, back where they were", () => {
    const none = toFlow(without(SPLIT, OTHER));
    expect(at(none, BASS).position.x).toBe(-COLUMN);
    expect(x(none, KEYS, DEVICE)).toEqual([-2 * COLUMN, -2 * COLUMN]);
    const added = toFlow(without(OTHER), none.places);
    expect(at(added, SPLIT).position.x).toBe(-COLUMN);
    expect(at(added, BASS).position.x).toBe(-2 * COLUMN);
    expect(x(added, KEYS, DEVICE)).toEqual([-3 * COLUMN, -3 * COLUMN]);
    // (With the second splitter the others stay where they are: they are in the slots that they were laid out for.)
    const more = toFlow(graph, added.places);
    for (const id of [SPLIT, BASS, KEYS, DEVICE]) expect(at(more, id).position, `node ${String(id)}`).toEqual(at(added, id).position);
    const gone = toFlow(without(SPLIT, OTHER), more.places);
    expect(at(gone, BASS).position.x).toBe(-COLUMN);
    expect(x(gone, KEYS, DEVICE)).toEqual([-2 * COLUMN, -2 * COLUMN]);
  });

  test("a source that a person put somewhere stays there when a splitter is added: the layout does not move what a person placed", () => {
    const placed = (g: Graph): Graph => ({ ...g, nodes: g.nodes.map((node) => (node.id === KEYS ? { ...node, ui: { x: -900, y: 40 } } : node)) });
    const before = toFlow(placed(without(SPLIT, OTHER)));
    const after = toFlow(placed(graph), before.places);
    expect(at(after, KEYS).position).toEqual({ x: -900, y: 40 });
  });

  test("no node is on another: not a splitter, a box, a source, a lane or the master, with or without splitters, and as they are added one by one", () => {
    let places: ReturnType<typeof toFlow>["places"] | undefined;
    const steps: Graph[] = [without(SPLIT, OTHER), without(OTHER), graph];
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

describe("the structure of a note splitter", () => {
  test("is the node, its name and what it picks: a splitter that picks another note is laid out again, and a node that is the same is not", () => {
    expect(sameStructure(graph, structuredClone(graph))).toBe(true);
    const picking = (pick: "lowest" | "highest"): Graph => ({ ...graph, nodes: graph.nodes.map((node) => (node.id === SPLIT && node.kind === "splitter" ? { ...node, pick } : node)) });
    expect(sameStructure(graph, picking("lowest"))).toBe(true);
    expect(sameStructure(graph, picking("highest"))).toBe(false);
    const renamed: Graph = { ...graph, nodes: graph.nodes.map((node) => (node.id === SPLIT ? { ...node, name: "Lead" } : node)) };
    expect(sameStructure(graph, renamed)).toBe(false);
  });
});

describe("the cables of a note splitter on the canvas", () => {
  const flow = toFlow(graph);
  const cable = (from: NodeId, to: NodeId) => flow.edges.find((edge) => edge.source === String(from) && edge.target === String(to));

  test("leave it at the handle of the output they start at: the first at `eventsOut`, as every node's output is, and the second at `eventsOutRest`, and come in at `eventsIn`", () => {
    expect(cable(SPLIT, FIRST_LANE)).toMatchObject({ type: "notes", sourceHandle: "eventsOut", targetHandle: "eventsIn" });
    expect(cable(SPLIT, SECOND_LANE)).toMatchObject({ type: "notes", sourceHandle: "eventsOutRest", targetHandle: "eventsIn" });
    expect(cable(OTHER, THIRD_LANE)).toMatchObject({ type: "notes", sourceHandle: "eventsOut", targetHandle: "eventsIn" });
    // (A cable into a splitter, from a source and from a box.)
    expect(cable(KEYS, SPLIT)).toMatchObject({ type: "notes", sourceHandle: "eventsOut", targetHandle: "eventsIn" });
    expect(cable(BASS, OTHER)).toMatchObject({ type: "notes", sourceHandle: "eventsOut", targetHandle: "eventsIn" });
    expect(handleOf({ type: "eventsOutRest" })).toBe("eventsOutRest");
    expect(handleOf({ type: "eventsOut" })).toBe("eventsOut");
  });

  test("say which output they start at in their words, which a lane that has a cable from each output of one splitter needs to tell them apart", () => {
    expect(cable(SPLIT, FIRST_LANE)?.data?.label).toBe("Bass line → Oscillator 1");
    expect(cable(SPLIT, SECOND_LANE)?.data?.label).toBe("Bass line (rest) → Oscillator 2");
    expect(cable(KEYS, SPLIT)?.data?.label).toBe("Keys → Bass line");
  });

  test("have a link, drawn faintly, from each source to a splitter that no cable ends at (it hears every input) and from no splitter", () => {
    const bare: Graph = { nodes: graph.nodes, edges: graph.edges.filter((edge) => edge.kind !== "events" && edge.kind !== "hears") };
    const links = (g: Graph): string[] => toFlow(g).edges.filter((edge) => edge.type === "hears").map((edge) => `${edge.source}>${edge.target}`);
    // (With no cable at all,a link from each source goes to every lane, to the box and to both splitters: the graph that the server sends has them, and the page draws the one that it is given.)
    expect(links(bare)).toEqual([]);
    const sent = (g: Graph): Graph => ({ nodes: g.nodes, edges: [...g.edges, ...[KEYS, DEVICE].flatMap((from) => [FIRST_LANE, BASS, SPLIT, OTHER].map((to) => ({ from: { node: from, port: { type: "eventsOut" as const } }, to: { node: to, port: { type: "eventsIn" as const } }, kind: "hears" as const })))] });
    expect(links(sent(bare))).toEqual(["3>0", "3>5", "3>6", "3>7", "4>0", "4>5", "4>6", "4>7"]);
  });
});
