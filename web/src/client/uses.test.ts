import { describe, expect, test } from "vitest";

import type { Edge, Graph, Node, NodeId, Param, UsedCondition, UsedWhen } from "../protocol";
import { sideCablesFixture, usesFixture } from "./fixtures";
import { conditionHolds, sideIsPatched, unusedBecause } from "./uses";

const { graph } = sideCablesFixture.snapshot;

// The session of the fixture (`testutil::SIDED`, see `sideCables.test.ts`): the group's math effect (node 1) has nothing at its second input, and the lane's (node 4) has three cables there.
const [BUS_MATH, LANE_MATH] = [1, 4];

function effectOf(graph: Graph, id: NodeId): Node & { kind: "effect" } {
  const node = graph.nodes.find((candidate) => candidate.id === id);
  if (node?.kind !== "effect") throw new Error(`node ${String(id)} is not an effect`);
  return node;
}

function paramOf(node: Node & { kind: "effect" }, name: string): Param {
  const param = node.plugin.params.find((candidate) => candidate.name === name);
  if (!param) throw new Error(`${name} is not a parameter of ${node.name}`);
  return param;
}

/** The graph of the fixture with the mode of the math effect in node `id` set to `mode`, and the cables at its second input as they are or taken out. */
function graphWith(id: NodeId, mode: number, cables: "as they were" | "taken out"): Graph {
  const copy = structuredClone(graph);
  paramOf(effectOf(copy, id), "mode").value = mode;
  if (cables === "taken out") copy.edges = copy.edges.filter((edge) => !(edge.kind === "cable" && edge.to.node === id && edge.to.port.type === "audioInSide"));
  return copy;
}

describe("when a parameter has an effect", () => {
  test("is what the model says, parameter by parameter, for every mode of both math effects with their cables as they are and taken out", () => {
    const wrong: string[] = [];
    for (const row of usesFixture.rows) {
      const copy = graphWith(row.node, row.mode, row.cables);
      const node = effectOf(copy, row.node);
      // (Every parameter of the plugin is in the row, so that none is left out of the comparison.)
      expect(Object.keys(row.unused).sort()).toEqual(node.plugin.params.map((param) => param.name).sort());
      for (const param of node.plugin.params) {
        const got = unusedBecause(copy, node, param);
        if (got !== row.unused[param.name]) wrong.push(`node ${String(row.node)}, cables ${row.cables}, mode ${String(row.mode)}, ${param.name}: ${String(got)}, and the model says ${String(row.unused[param.name])}`);
      }
    }
    expect(wrong).toEqual([]);
    expect(usesFixture.rows).toHaveLength(48);
  });

  test("has some of the parameters unused and some used, in the rows of the fixture (so that the comparison above is not of nothing)", () => {
    const unused = usesFixture.rows.flatMap((row) => Object.values(row.unused)).filter((why) => why !== null);
    expect(unused.length).toBeGreaterThan(100);
    expect(new Set(unused)).toEqual(new Set(["only in the modes am, fold, warp, compare, hold, follow", "only in the modes xor, and, or", "only in the mode follow", "only while nothing is patched at B"]));
    expect(usesFixture.rows.some((row) => row.unused.pitch === null) && usesFixture.rows.some((row) => row.unused.pitch !== null)).toBe(true);
    expect(usesFixture.rows.every((row) => row.unused.mode === null && row.unused.level === null)).toBe(true);
  });
});

describe("the cables at the second input of an effect", () => {
  test("are those that end at the port `audioInSide` of that effect, and no others", () => {
    expect(sideIsPatched(graph, LANE_MATH)).toBe(true);
    expect(sideIsPatched(graph, BUS_MATH)).toBe(false);
    expect(sideIsPatched(graphWith(LANE_MATH, 0, "taken out"), LANE_MATH)).toBe(false);
    // A cable at the first input of an effect, and a cable that is not one (a modulation cable, a link of the audio path), do not count; nor does one at another effect's second input.
    const cable = (to: Edge["to"], kind: "cable" | "audio"): Edge => (kind === "cable" ? { from: { node: 5, port: { type: "audioOut" } }, to, kind, id: 99, gain: 1 } : { from: { node: 5, port: { type: "audioOut" } }, to, kind });
    const bare: Graph = { nodes: graph.nodes, edges: [] };
    expect(sideIsPatched({ ...bare, edges: [cable({ node: BUS_MATH, port: { type: "audioIn" } }, "cable")] }, BUS_MATH)).toBe(false);
    expect(sideIsPatched({ ...bare, edges: [cable({ node: BUS_MATH, port: { type: "audioInSide" } }, "audio")] }, BUS_MATH)).toBe(false);
    expect(sideIsPatched({ ...bare, edges: [cable({ node: LANE_MATH, port: { type: "audioInSide" } }, "cable")] }, BUS_MATH)).toBe(false);
    expect(sideIsPatched({ ...bare, edges: [cable({ node: BUS_MATH, port: { type: "audioInSide" } }, "cable")] }, BUS_MATH)).toBe(true);
  });
});

describe("a condition", () => {
  const choice: UsedCondition = { type: "choice", param: 3, values: [0, 2] };
  const at = (value: number | undefined, side = false): boolean => conditionHolds(choice, (index) => (index === 3 ? value : undefined), side);

  test("of a choice holds when the value that is set rounds to one of its places, as Rust rounds, and never below the list", () => {
    expect([0, 0.4, 2, 1.6].map((value) => at(value))).toEqual([true, true, true, true]);
    expect([1, 0.6, 3, 7].map((value) => at(value))).toEqual([false, false, false, false]);
    // (Rust rounds a half away from nothing: -0.5 is -1, which is below the list, where `Math.round` makes it a negative zero, which is the first place.)
    expect([-0.5, -1, -3, Number.NaN].map((value) => at(value))).toEqual([false, false, false, false]);
    expect(at(2.5)).toBe(false);
    expect(at(0.5)).toBe(false);
  });

  test("of a choice reads the parameter that it names and no other, and a rule that names none is no rule", () => {
    expect(conditionHolds(choice, (index) => (index === 4 ? 0 : index === 3 ? 1 : undefined), false)).toBe(false);
    expect(conditionHolds(choice, () => undefined, false)).toBe(true);
    expect(at(2, true)).toBe(true);
  });

  test("of a carrier holds while nothing is patched at the second input, whatever the values are", () => {
    const carrier: UsedCondition = { type: "sideUnpatched" };
    expect(conditionHolds(carrier, () => undefined, false)).toBe(true);
    expect(conditionHolds(carrier, () => 7, false)).toBe(true);
    expect(conditionHolds(carrier, () => undefined, true)).toBe(false);
    expect(conditionHolds(carrier, () => 0, true)).toBe(false);
  });
});

describe("the parameter that a rule names", () => {
  test("is read by its own index and not by its place among the plugin's parameters, and what is set is what counts", () => {
    const param = (index: number, name: string, value: number, usedWhen?: UsedWhen): Param => ({ index, name, min: 0, max: 11, default: 0, value, labels: null, unit: null, group: [], stepCount: 0, ...(usedWhen ? { usedWhen } : {}) });
    const rule: UsedWhen = { condition: { type: "choice", param: 7, values: [2] }, why: "only in the third mode" };
    // The mode has the index 7 and is the third in the list; the first has the index 0 and a value that would hold, the second has one that would not.
    const [first, second, mode, controlled] = [param(0, "first", 2), param(3, "second", 5), param(7, "mode", 2), param(9, "controlled", 0, rule)] as const;
    const filter = effectOf(graph, 3);
    const effect: Node & { kind: "effect" } = { ...filter, plugin: { ...filter.plugin, params: [first, second, mode, controlled] } };
    expect(unusedBecause(graph, effect, controlled)).toBeNull();
    mode.value = 4;
    expect(unusedBecause(graph, effect, controlled)).toBe("only in the third mode");
    mode.value = 2;
    first.value = 4;
    expect(unusedBecause(graph, effect, controlled)).toBeNull();
  });
});

describe("a parameter that a plugin says nothing of", () => {
  test("is used, and so is anything that is not the parameter of a plugin", () => {
    const filter = effectOf(graph, 3);
    for (const param of filter.plugin.params) expect(unusedBecause(graph, filter, param)).toBeNull();
    const lfo = graph.nodes.find((node) => node.kind === "modulator");
    const pitch = paramOf(effectOf(graph, LANE_MATH), "pitch");
    expect(lfo).toBeDefined();
    if (lfo) expect(unusedBecause(graph, lfo, pitch)).toBeNull();
    expect(unusedBecause(graph, effectOf(graph, LANE_MATH), pitch)).toBe("only while nothing is patched at B");
  });
});
