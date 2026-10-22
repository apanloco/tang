import { describe, expect, test } from "vitest";

import type { Intent } from "../protocol";
import { tracerSnapshot } from "./fixtures";
import { MockServer } from "./mock";

// The controller has sources of notes and cables from them since P4b of docs/patch-graph.md §11.6; the page's own engine has none until the page draws them (P4c).
describe("the mock engine's sources of notes", () => {
  const refusals: [string, Intent, string][] = [
    ["the keys as a source", { type: "addSource", source: { type: "keys" } }, "this engine has no sources of notes yet"],
    ["a device as a source", { type: "addSource", source: { type: "device", name: "Seaboard" } }, "this engine has no sources of notes yet"],
    [
      "a cable from an output of events",
      { type: "addCable", from: { node: 0, port: { type: "eventsOut" } }, to: { node: 1, port: { type: "audioIn" } }, gain: null },
      "this engine has no cables of notes yet",
    ],
    [
      "a cable to an input of events",
      { type: "addCable", from: { node: 1, port: { type: "audioOut" } }, to: { node: 0, port: { type: "eventsIn" } }, gain: 0.5 },
      "this engine has no cables of notes yet",
    ],
  ];

  test.each(refusals)("are not there yet: %s is refused in words that say so, and nothing changes", (_what, intent, words) => {
    const server = new MockServer({ snapshot: tracerSnapshot });
    const before = JSON.stringify(server.snapshot);
    expect(() => {
      server.apply(intent);
    }).toThrow(words);
    expect(JSON.stringify(server.snapshot)).toBe(before);
  });

  test("have no step in the history, since a refusal leaves none", () => {
    const server = new MockServer({ snapshot: tracerSnapshot });
    expect(() => {
      server.apply({ type: "addSource", source: { type: "keys" } });
    }).toThrow();
    expect(server.history.undo).toBeNull();
  });
});
