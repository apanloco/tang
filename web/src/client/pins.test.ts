import { describe, expect, test } from "vitest";

import type { Node, NodeId } from "../protocol";
import { pinsFixture } from "./fixtures";
import { MockServer } from "./mock";

/** The parameters pinned to the plugin of a node of a session, if it has one. */
function pinnedOf(nodes: readonly Node[], id: NodeId): number[] | undefined {
  const node = nodes.find((n) => n.id === id);
  return node?.kind === "instrument" || node?.kind === "effect" ? node.plugin?.pinned : undefined;
}

/** What the mock engine makes of the intent that pins `params` to `node`: the session after, or the words it refused with. */
function pinned(node: number, params: number[]): { after: MockServer["snapshot"] } | { refused: string } {
  const server = new MockServer({ snapshot: pinsFixture.snapshot });
  try {
    server.apply({ type: "setPinned", node, params });
  } catch (error) {
    return { refused: error instanceof Error ? error.message : String(error) };
  }
  return { after: server.snapshot };
}

describe("the mock engine's pinning of parameters", () => {
  test("is what the controller makes of it, for every node and every list its fixture has: what is pinned afterwards, and the words of a refusal", () => {
    const wrong: string[] = [];
    for (const row of pinsFixture.pins) {
      const what = `${JSON.stringify(row.params)} to node ${String(row.node)}`;
      const result = pinned(row.node, row.params);
      if ("error" in row) {
        const got = "refused" in result ? result.refused : "done";
        if (got !== row.error) wrong.push(`${what}: ${got}, and the controller says ${row.error}`);
        continue;
      }
      if ("refused" in result) {
        wrong.push(`${what}: refused (${result.refused}), and the controller took it`);
        continue;
      }
      const got = pinnedOf(result.after.graph.nodes, row.node);
      if (JSON.stringify(got) !== JSON.stringify(row.pinned)) wrong.push(`${what}: ${JSON.stringify(got)} is pinned, not ${JSON.stringify(row.pinned)}`);
    }
    expect(wrong).toEqual([]);
  });

  test("changes the plugin of that node and nothing else: every other node is as it was, and so are the cables and the layout of the telemetry", () => {
    const done = pinsFixture.pins.find((row) => "pinned" in row && row.params.length > 1);
    if (!done) throw new Error("the fixture has no list that is taken");
    const result = pinned(done.node, done.params);
    if ("refused" in result) throw new Error(result.refused);
    const before = pinsFixture.snapshot;
    for (const node of before.graph.nodes) {
      const now = result.after.graph.nodes.find((n) => n.id === node.id);
      if (node.id === done.node) continue;
      expect(now, `node ${String(node.id)}`).toEqual(node);
    }
    expect(result.after.graph.edges).toEqual(before.graph.edges);
    expect(result.after.telemetry).toEqual(before.telemetry);
  });

  test("has been tried with lists that are taken and with each of the refusals: the fixture is not empty", () => {
    const taken = pinsFixture.pins.filter((row) => "pinned" in row);
    const words = new Set(pinsFixture.pins.flatMap((row) => ("error" in row ? [row.error] : [])));
    expect(taken.length).toBeGreaterThan(10);
    for (const sentence of [
      "there is no node 9999",
      "that node has no plugin",
      "that instrument has no plugin",
      "the plugin has no parameter 999",
      "at most 64 parameters can be pinned to a node",
    ]) {
      expect(words.has(sentence), sentence).toBe(true);
    }
    expect([...words].some((sentence) => /^parameter \d+ is listed twice$/.test(sentence))).toBe(true);
    // An instrument, an effect and a bus effect each had a list taken.
    const nodesTaken = new Set(taken.map((row) => row.node));
    expect(nodesTaken.size).toBe(3);
  });
});
