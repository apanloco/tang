import { describe, expect, test } from "vitest";

import type { Intent, Port, Snapshot } from "../protocol";
import { groupsFixture } from "./fixtures";
import type { Shape } from "./fixtures";
import { MockServer } from "./mock";

const portKey = (port: Port): string => `${port.type}${"index" in port ? String(port.index) : ""}${"edge" in port ? `#${String(port.edge)}` : ""}`;

/**
 * What a session comes to, as the fixture writes it (`shape_of` in `app/apply.rs`, by the same rules): a line for each node (its id, its
 * parent, its name), each link of the audio path (the node it leaves, the node it goes to, its kind), each cable (its id, the node it leaves
 * and the one it goes to, the port, its depth) and each entry of the two lists of the layout of the telemetry (the slot, the node, the port,
 * the kind); the fingerprint of the layout is not part of it.
 */
export function shapeOf(snapshot: Snapshot): Shape {
  const { nodes, edges } = snapshot.graph;
  return {
    nodes: nodes.map((n) => `${String(n.id)} ${n.parent === null ? "-" : String(n.parent)} ${n.name}`),
    audio: edges.flatMap((e) => (e.kind === "modulation" ? [] : [`${String(e.from.node)}>${String(e.to.node)} ${e.kind}`])),
    cables: edges.flatMap((e) => (e.kind === "modulation" ? [`#${String(e.id)} ${String(e.from.node)}>${String(e.to.node)} ${portKey(e.to.port)} @${String(e.amount)}`] : [])),
    meters: snapshot.telemetry.meters.map((m) => `${String(m.slot)} ${String(m.node)} ${portKey(m.port)} ${m.kind}`),
    modulation: snapshot.telemetry.modulation.map((m) => `${String(m.slot)} ${String(m.node)} ${portKey(m.port)} ${m.kind}`),
  };
}

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
