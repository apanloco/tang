import { describe, expect, test } from "vitest";

import type { Graph } from "../protocol";
import { movesFixture } from "./fixtures";
import { MockServer } from "./mock";

/** The links of the audio path of a graph, in the order of its edges, as the fixture writes them: the node each leaves, the node it goes to, its kind. */
const audioOf = (graph: Graph) => graph.edges.flatMap((edge) => (edge.kind === "modulation" ? [] : [[edge.from.node, edge.to.node, edge.kind]]));

/** The modulation cables of a graph. */
const cablesOf = (graph: Graph) => graph.edges.filter((edge) => edge.kind === "modulation");

/** What the mock engine makes of the intent that moves `node` to place `index`: the session after, or the words it refused with. */
function moved(node: number, index: number): { after: MockServer["snapshot"] } | { refused: string } {
  const server = new MockServer({ snapshot: movesFixture.snapshot });
  try {
    server.apply({ type: "moveEffect", node, index });
  } catch (error) {
    return { refused: error instanceof Error ? error.message : String(error) };
  }
  return { after: server.snapshot };
}

describe("the mock engine's move of an effect", () => {
  test("is what the controller makes of it, for every node and every place its fixture has: the order of the nodes, the audio path through them, and the words of a refusal", () => {
    const wrong: string[] = [];
    for (const row of movesFixture.moves) {
      const what = `node ${String(row.node)} to place ${String(row.index)}`;
      const result = moved(row.node, row.index);
      if ("error" in row) {
        const got = "refused" in result ? result.refused : "done";
        if (got !== row.error) wrong.push(`${what}: ${got}, and the controller says ${row.error}`);
        continue;
      }
      if ("refused" in result) {
        wrong.push(`${what}: refused (${result.refused}), and the controller moved it`);
        continue;
      }
      const { graph } = result.after;
      if (JSON.stringify(graph.nodes.map((node) => node.id)) !== JSON.stringify(row.nodes)) wrong.push(`${what}: the nodes are ${JSON.stringify(graph.nodes.map((node) => node.id))}, not ${JSON.stringify(row.nodes)}`);
      if (JSON.stringify(audioOf(graph)) !== JSON.stringify(row.audio)) wrong.push(`${what}: the audio path is ${JSON.stringify(audioOf(graph))}, not ${JSON.stringify(row.audio)}`);
    }
    expect(wrong).toEqual([]);
  });

  test("leaves the cables and the layout of the telemetry as they were, since they name the plugins, which are the same nodes", () => {
    const before = movesFixture.snapshot;
    const done = movesFixture.moves.filter((row) => "nodes" in row);
    for (const row of done) {
      const result = moved(row.node, row.index);
      if ("refused" in result) throw new Error(`node ${String(row.node)} to place ${String(row.index)}: ${result.refused}`);
      expect(cablesOf(result.after.graph), `node ${String(row.node)} to place ${String(row.index)}`).toEqual(cablesOf(before.graph));
      expect(result.after.telemetry).toEqual(before.telemetry);
    }
  });

  test("keeps every node as it was, the effect's own parameters and mix with it: only the order of the nodes and the links of the audio path change", () => {
    const [row] = movesFixture.moves.filter((candidate) => "nodes" in candidate);
    if (!row) throw new Error("the fixture has no move that is done");
    const result = moved(row.node, row.index);
    if ("refused" in result) throw new Error(result.refused);
    const byId = (graph: Graph) => new Map(graph.nodes.map((node) => [node.id, node]));
    const after = byId(result.after.graph);
    for (const [id, node] of byId(movesFixture.snapshot.graph)) expect(after.get(id)).toEqual(node);
  });

  test("refuses a place that is none, a number that is not a whole one or is below the first place, as the chain having no such place, and does nothing", () => {
    // (The server does not get so far with these: a place is a whole number that is not negative, on the wire. The mock does not do something odd with them.)
    for (const index of [-1, 1.5, Number.NaN, Number.POSITIVE_INFINITY]) {
      const result = moved(6, index);
      expect("refused" in result ? result.refused : "done", String(index)).toBe(`the chain has no place ${String(index)}`);
    }
  });

  test("has been tried with moves that are done and with each of the refusals: the fixture is not empty", () => {
    const done = movesFixture.moves.filter((row) => "nodes" in row);
    const words = new Set(movesFixture.moves.flatMap((row) => ("error" in row ? [row.error] : [])));
    expect(done.length).toBeGreaterThan(20);
    for (const sentence of ["only an effect can be moved", "that effect is at that place already", "the chain has no place 99", "there is no node 9999"]) {
      expect(words.has(sentence), sentence).toBe(true);
    }
    // A chain of each kind was moved in: a lane's in a group (its last link goes into the group), a lane's in none (into the master) and a bus.
    const kinds = new Set(done.flatMap((row) => ("audio" in row ? row.audio.map(([, , kind]) => kind) : [])));
    expect([...kinds].sort()).toEqual(["audio", "member"]);
  });
});
