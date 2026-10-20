import { describe, expect, test } from "vitest";

import type { Edge, Intent } from "../protocol";
import { shapeOf, tracerSnapshot } from "./fixtures";
import { planOf } from "./history";
import { MockServer } from "./mock";

// Audio cables (P3c of docs/patch-graph.md §11.6) are drawn by the controller and not yet by the mock engine: the page draws them in P3d, and teaches the mock as it does. Until
// then the mock refuses the intents, keeps no step for them, and is told nothing by a snapshot that has one (a real server may have).
const oscillator = tracerSnapshot.graph.nodes.find((n) => n.kind === "instrument");
const filter = tracerSnapshot.graph.nodes.find((n) => n.kind === "effect");
if (!oscillator || !filter) throw new Error("the tracer has an oscillator and a filter");
const addCable: Intent = { type: "addCable", from: { node: oscillator.id, port: { type: "audioOut" } }, to: { node: filter.id, port: { type: "audioIn" } }, gain: null };
const setCableGain: Intent = { type: "setCableGain", edge: 31, gain: 0.5 };

describe("the mock engine and the audio cables of the controller", () => {
  test.each([addCable, setCableGain])("refuses the intent %#, and nothing changes", (intent) => {
    const server = new MockServer({ snapshot: tracerSnapshot });
    expect(() => server.apply(intent)).toThrow("the mock engine has no audio cables yet");
    expect(server.snapshot).toBe(tracerSnapshot);
    expect(server.history).toEqual({ undo: null, redo: null });
  });

  test.each([addCable, setCableGain])("keeps no step of the history for the intent %#", (intent) => {
    expect(planOf(tracerSnapshot, intent, 0, [])).toBe("nothing");
  });

  test("lists the links of the audio path in the shape of a session and not the cables that have been drawn between two places", () => {
    const send: Edge = { kind: "cable", id: 31, gain: 0.5, from: { node: oscillator.id, port: { type: "audioOut" } }, to: { node: filter.id, port: { type: "audioIn" } } };
    const drawn = { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, edges: [...tracerSnapshot.graph.edges, send] } };
    expect(shapeOf(drawn)).toEqual(shapeOf(tracerSnapshot));
  });
});
