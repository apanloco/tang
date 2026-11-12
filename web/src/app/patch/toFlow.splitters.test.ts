import { describe, expect, test } from "vitest";

import { patternBoxesFixture } from "../../client/fixtures";
import type { Graph, Node } from "../../protocol";
import { handleOf } from "./handles";
import { toFlow } from "./toFlow";

// A note splitter (step 127, P4i-B) in the graph that the page gets, before the page can draw one (step 128): the canvas must not break on the node or on the cable that starts at its second output. The session of the fixture
// with boxes has the keys (2), a device (3), the boxes Bass (4) and Pattern (5), the lanes 0 and 1 and the master; a splitter (node 90) is put in it, with the keys into it and both of its outputs to the lanes.
const found = patternBoxesFixture.cases.find((entry) => entry.name === "boxes");
if (!found) throw new Error("the fixture has the session with boxes");
const SPLITTER = 90;
const splitter: Node = { id: SPLITTER, parent: null, name: "Bass line", kind: "splitter", pick: "lowest" };
const graph: Graph = structuredClone(found.snapshot.graph);
graph.nodes.push(splitter);
graph.edges.push(
  { from: { node: 2, port: { type: "eventsOut" } }, to: { node: SPLITTER, port: { type: "eventsIn" } }, kind: "events", id: 90 },
  { from: { node: SPLITTER, port: { type: "eventsOut" } }, to: { node: 0, port: { type: "eventsIn" } }, kind: "events", id: 91 },
  { from: { node: SPLITTER, port: { type: "eventsOutRest" } }, to: { node: 1, port: { type: "eventsIn" } }, kind: "events", id: 92 },
);

describe("a note splitter on the canvas before the page draws one", () => {
  test("is a node of the canvas, of its kind, with the name that the server gave it, and the nodes that were there are as they were", () => {
    const flow = toFlow(graph);
    const found = flow.nodes.find((node) => node.id === String(SPLITTER));
    expect(found?.type).toBe("splitter");
    expect(found?.data.name).toBe("Bass line");
    // (The session has no effect held in a lane and no module, so that every node of the graph is on the canvas.)
    expect(flow.nodes.map((node) => node.id).sort()).toEqual(graph.nodes.map((node) => String(node.id)).sort());
  });

  test("has an edge for each of its cables, from the handle of the port that it starts at", () => {
    const flow = toFlow(graph);
    const from = (edge: number): string | null | undefined => flow.edges.find((each) => each.id.endsWith(String(edge)) && each.source === String(SPLITTER))?.sourceHandle;
    expect(from(91)).toBe("eventsOut");
    expect(from(92)).toBe("eventsOutRest");
  });

  test("names the handle of its second output as it names every handle: by the kind of the port", () => {
    expect(handleOf({ type: "eventsOutRest" })).toBe("eventsOutRest");
    expect(handleOf({ type: "eventsOut" })).toBe("eventsOut");
  });
});
