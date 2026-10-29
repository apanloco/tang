import { describe, expect, test } from "vitest";

import { Counters } from "./counters";
import { historyFixture } from "./fixtures";

// What the mock engine hands out, as the controller does (`NodeIds`, `SlotAllocator`): ids and slots go on from what the session has and are never given twice, except
// that what undo puts back has the ids it had.
describe("the ids and the slots that the mock engine hands out", () => {
  const { snapshot } = historyFixture;
  const fresh = (): Counters => new Counters(snapshot);
  const lastNode = Math.max(...snapshot.graph.nodes.filter((n) => n.kind !== "master").map((n) => n.id));
  const lastEdge = Math.max(...snapshot.graph.edges.flatMap((e) => (e.kind === "modulation" ? [e.id] : [])));
  // (The limiter's reduction is the last cell of the telemetry and is never handed out, so it is not a slot in use: `HANDED_OUT` of `src/telemetry.rs`.)
  const lastSlot = Math.max(...[...snapshot.telemetry.meters, ...snapshot.telemetry.modulation].filter((entry) => entry.kind !== "reduction").map((entry) => entry.slot));

  test("go on from what the session has, the master (which is no node of the model) left out", () => {
    const counters = fresh();
    expect([counters.node(), counters.edge(), counters.slot()]).toEqual([lastNode + 1, lastEdge + 1, lastSlot + 1]);
    expect([counters.node(), counters.edge(), counters.slot()]).toEqual([lastNode + 2, lastEdge + 2, lastSlot + 2]);
  });

  test("leave the limiter's reduction out of the slots in use: it is the last cell of the telemetry (4095), which is never handed out, so that the slots go on from the others", () => {
    expect(snapshot.telemetry.meters.filter((entry) => entry.kind === "reduction").map((entry) => entry.slot)).toEqual([4095]);
    const counters = fresh();
    const first = counters.slot();
    expect(first).toBe(lastSlot + 1);
    expect(first).toBeLessThan(4095);
  });

  test("put back come first, in the order they were said, and new ones go on where they were", () => {
    const counters = fresh();
    counters.reuse({ nodes: [4, 2], edges: [1, 0] });
    expect([counters.node(), counters.node()]).toEqual([4, 2]);
    expect([counters.edge(), counters.edge()]).toEqual([1, 0]);
    expect(counters.node()).toBe(lastNode + 1);
    expect(counters.edge()).toBe(lastEdge + 1);
  });

  test("put back are what was said last, not what was said before", () => {
    const counters = fresh();
    counters.reuse({ nodes: [4], edges: [1] });
    counters.reuse({ nodes: [2], edges: [0] });
    expect([counters.node(), counters.edge()]).toEqual([2, 0]);
    expect([counters.node(), counters.edge()]).toEqual([lastNode + 1, lastEdge + 1]);
  });

  test("put back that were not used are dropped, nodes and cables alike, and are not given to what is made next", () => {
    const counters = fresh();
    counters.reuse({ nodes: [4], edges: [1] });
    counters.dropReused();
    expect([counters.node(), counters.edge()]).toEqual([lastNode + 1, lastEdge + 1]);
  });

  test("take a slot of their own, which is never put back", () => {
    const counters = fresh();
    counters.reuse({ nodes: [4], edges: [1] });
    expect(counters.slot()).toBe(lastSlot + 1);
    expect(counters.slot()).toBe(lastSlot + 2);
  });

  test("are written down, in the order they are handed out, from the log that is begun to the log that is taken", () => {
    const counters = fresh();
    counters.node(); // (before the log: not in it)
    counters.begin();
    const [node, edge, other] = [counters.node(), counters.edge(), counters.node()];
    expect(counters.take()).toEqual({ nodes: [node, other], edges: [edge] });
    expect(counters.take(), "what is taken is not written to any more").toEqual({ nodes: [], edges: [] });
    counters.node();
    expect(counters.take()).toEqual({ nodes: [], edges: [] });
  });

  test("have what was put back in the log as any other, in its turn", () => {
    const counters = fresh();
    counters.begin();
    counters.reuse({ nodes: [2], edges: [0] });
    const made = [counters.node(), counters.node()];
    expect(made).toEqual([2, lastNode + 1]);
    expect(counters.edge()).toBe(0);
    expect(counters.take()).toEqual({ nodes: made, edges: [0] });
  });

  test("go back to where they were at a mark: what was handed out since is handed out again, slots included", () => {
    const counters = fresh();
    counters.node();
    counters.edge();
    counters.slot();
    const mark = counters.mark();
    expect([counters.node(), counters.edge(), counters.slot()]).toEqual([lastNode + 2, lastEdge + 2, lastSlot + 2]);
    counters.rewind(mark);
    expect([counters.node(), counters.edge(), counters.slot()]).toEqual([lastNode + 2, lastEdge + 2, lastSlot + 2]);
  });

  test("go back to the ids that were to be put back at a mark, in their order", () => {
    const counters = fresh();
    counters.reuse({ nodes: [4, 2, 7], edges: [1, 0] });
    expect([counters.node(), counters.edge()]).toEqual([4, 1]);
    const mark = counters.mark();
    expect([counters.node(), counters.node(), counters.node(), counters.edge(), counters.edge()]).toEqual([2, 7, lastNode + 1, 0, lastEdge + 1]);
    counters.rewind(mark);
    expect([counters.node(), counters.node(), counters.node(), counters.edge(), counters.edge()]).toEqual([2, 7, lastNode + 1, 0, lastEdge + 1]);
  });

  test("go back to a mark that was made before another, and to one more than once", () => {
    const counters = fresh();
    const first = counters.mark();
    counters.node();
    const second = counters.mark();
    counters.node();
    counters.rewind(second);
    expect(counters.node()).toBe(lastNode + 2);
    counters.rewind(first);
    expect(counters.node()).toBe(lastNode + 1);
    counters.rewind(first);
    expect(counters.node()).toBe(lastNode + 1);
  });

  test("have the log as it was at a mark: what was handed out since is not in it", () => {
    const counters = fresh();
    counters.begin();
    const before = [counters.node(), counters.edge()];
    const mark = counters.mark();
    counters.node();
    counters.edge();
    counters.edge();
    counters.rewind(mark);
    const after = [counters.node(), counters.edge()];
    expect(counters.take()).toEqual({ nodes: [before[0], after[0]], edges: [before[1], after[1]] });
  });

  test("have no log to go back in when none was begun, and none is begun by going back", () => {
    const counters = fresh();
    const mark = counters.mark();
    counters.node();
    counters.rewind(mark);
    expect(counters.take()).toEqual({ nodes: [], edges: [] });
    counters.node();
    expect(counters.take()).toEqual({ nodes: [], edges: [] });
  });

  test("have a log that is begun again start empty", () => {
    const counters = fresh();
    counters.begin();
    counters.node();
    counters.begin();
    expect(counters.take()).toEqual({ nodes: [], edges: [] });
  });
});
