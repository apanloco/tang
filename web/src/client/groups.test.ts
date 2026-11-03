import { describe, expect, test } from "vitest";

import type { Intent, Snapshot } from "../protocol";
import { groupsFixture, shapeOf } from "./fixtures";
import { MockServer } from "./mock";

/** What the mock engine makes of the intent: the session after, or the words it refused with. */
function made(intent: Intent): { after: Snapshot } | { refused: string } {
  const server = new MockServer({ snapshot: groupsFixture.snapshot });
  try {
    server.apply(intent);
  } catch (error) {
    return { refused: error instanceof Error ? error.message : String(error) };
  }
  return { after: server.snapshot };
}

const nameOf = (snapshot: Snapshot, node: number): string | undefined => snapshot.graph.nodes.find((n) => n.id === node)?.name;

describe("the mock engine's groups", () => {
  test("are what the controller makes of each intent of its fixture: the words of a refusal, the name a group has, and the nodes, the audio path, the cables and the layout of the telemetry after one that is taken", () => {
    const wrong: string[] = [];
    for (const row of groupsFixture.rows) {
      const what = JSON.stringify(row.intent);
      const result = made(row.intent);
      if ("error" in row) {
        const got = "refused" in result ? result.refused : "done";
        if (got !== row.error) wrong.push(`${what}: ${got}, and the controller says ${row.error}`);
        continue;
      }
      if ("refused" in result) {
        wrong.push(`${what}: refused (${result.refused}), and the controller took it`);
        continue;
      }
      if ("name" in row) {
        const node = "node" in row.intent ? row.intent.node : -1;
        if (nameOf(result.after, node) !== row.name) wrong.push(`${what}: the group is called ${String(nameOf(result.after, node))}, not ${row.name}`);
        continue;
      }
      const shape = shapeOf(result.after);
      for (const list of ["nodes", "audio", "cables", "meters", "modulation"] as const) {
        if (JSON.stringify(shape[list]) !== JSON.stringify(row.after[list])) wrong.push(`${what}: ${list} are ${JSON.stringify(shape[list])}, not ${JSON.stringify(row.after[list])}`);
      }
      // (The lanes and groups whose way out is cut: a list only for a session that has some.)
      if (JSON.stringify(shape.routes ?? []) !== JSON.stringify(row.after.routes ?? [])) wrong.push(`${what}: the ways out that are cut are ${JSON.stringify(shape.routes ?? [])}, not ${JSON.stringify(row.after.routes ?? [])}`);
    }
    expect(wrong).toEqual([]);
  });

  test("refuse to take out the last lane of a session, in the controller's words", () => {
    const server = new MockServer();
    const lanes = server.snapshot.graph.nodes.filter((n) => n.kind === "instrument");
    expect(lanes.map((n) => n.id)).toEqual([groupsFixture.lastLane.node]);
    expect(() => {
      server.apply({ type: "removeNode", node: groupsFixture.lastLane.node });
    }).toThrow(groupsFixture.lastLane.error);
    expect(server.snapshot.graph.nodes.filter((n) => n.kind === "instrument")).toHaveLength(1);
  });

  test("have been tried with each of the four intents, done and refused in each of their ways: the fixture is not empty", () => {
    const rows = groupsFixture.rows;
    const tried = [
      ["addGroup", 5, 2],
      ["setLaneGroup", 5, 2],
      ["renameGroup", 5, 2],
      ["removeNode", 5, 1],
    ] as const;
    for (const [type, done, refused] of tried) {
      expect(rows.filter((row) => row.intent.type === type && !("error" in row)).length, `${type} done`).toBeGreaterThanOrEqual(done);
      expect(rows.filter((row) => row.intent.type === type && "error" in row).length, `${type} refused`).toBeGreaterThanOrEqual(refused);
    }
    const words = new Set(rows.flatMap((row) => ("error" in row ? [row.error] : [])));
    for (const sentence of [
      "only a lane can be put in a group",
      "that node is not a group",
      "only a group has a name to change",
      "a name is at most 64 characters",
      "a name has no control characters",
      "there is no node 9999",
      "there is no node 4294967295",
    ]) {
      expect(words.has(sentence), sentence).toBe(true);
    }
  });
});
