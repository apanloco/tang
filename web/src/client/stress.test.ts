import { describe, expect, test } from "vitest";

import { MASTER } from "./fixtures";
import { stressSnapshot } from "./stress";
import { applyEvent } from "./reducers";

const snapshot = stressSnapshot();
const nodes = snapshot.graph.nodes;
const edges = snapshot.graph.edges;
const cables = edges.flatMap((edge) => (edge.kind === "modulation" ? [edge] : []));

describe("the session for the performance test", () => {
  test("has 32 modulators and cables to 64 parameters: the budget of docs/web-ui.md §9.4", () => {
    expect(nodes.filter((node) => node.kind === "modulator")).toHaveLength(32);
    const driven = new Set(cables.map((edge) => `${String(edge.to.node)}:${edge.to.port.type === "param" ? String(edge.to.port.index) : "?"}`));
    expect(driven.size).toBe(64);
    expect(cables).toHaveLength(64);
  });

  test("has eight lanes, each an oscillator through a filter to the master, and a master", () => {
    expect(nodes.filter((node) => node.kind === "instrument")).toHaveLength(8);
    expect(nodes.filter((node) => node.kind === "effect")).toHaveLength(8);
    expect(nodes.filter((node) => node.kind === "master").map((node) => node.id)).toEqual([MASTER]);
    expect(edges.filter((edge) => edge.kind === "audio")).toHaveLength(16);
    expect(edges.filter((edge) => edge.kind === "audio" && edge.to.node === MASTER)).toHaveLength(8);
  });

  test("has unique ids, and no cable to a node or parameter that is not there", () => {
    const ids = nodes.map((node) => node.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const edge of edges) {
      expect(ids, JSON.stringify(edge)).toContain(edge.from.node);
      expect(ids, JSON.stringify(edge)).toContain(edge.to.node);
      if (edge.kind === "modulation" && edge.to.port.type === "param") {
        const index = edge.to.port.index;
        const target = nodes.find((node) => node.id === edge.to.node);
        const plugin = target?.kind === "instrument" || target?.kind === "effect" ? target.plugin : null;
        expect(plugin?.params.some((param) => param.index === index), JSON.stringify(edge)).toBe(true);
        expect(plugin?.params.find((param) => param.index === index)?.labels, "a name, not a number").toBeNull();
      }
    }
    const parents = new Map(nodes.map((node) => [node.id, node.parent]));
    for (const node of nodes) if (node.parent !== null) expect(parents.has(node.parent)).toBe(true);
  });

  test("has each filter, and each LFO, in the lane of the oscillator it belongs to", () => {
    const lanes = nodes.filter((node) => node.kind === "instrument");
    for (const lane of lanes) {
      const children = nodes.filter((node) => node.parent === lane.id);
      expect(children.filter((node) => node.kind === "effect")).toHaveLength(1);
      expect(children.filter((node) => node.kind === "modulator")).toHaveLength(4);
    }
    expect(nodes.filter((node) => node.parent === null).map((node) => node.kind).sort()).toEqual(["instrument", "instrument", "instrument", "instrument", "instrument", "instrument", "instrument", "instrument", "master"]);
  });

  test("has cables that do something: every depth is more than nothing, and not more than all", () => {
    for (const cable of cables) {
      expect(Math.abs(cable.amount)).toBeGreaterThan(0);
      expect(Math.abs(cable.amount)).toBeLessThanOrEqual(1);
    }
    expect(new Set(cables.map((cable) => cable.id)).size).toBe(cables.length);
  });

  test("names a parameter once in the layout, however many cables end at it", () => {
    // Twelve cables on the ten parameters of a lane that can be driven: two of them are driven twice.
    const crowded = stressSnapshot({ lanes: 1, lfosPerLane: 1, targetsPerLfo: 12 });
    expect(crowded.graph.edges.filter((edge) => edge.kind === "modulation")).toHaveLength(12);
    expect(crowded.telemetry.modulation.filter((entry) => entry.kind === "effective")).toHaveLength(10);
  });

  test("has a telemetry layout with a slot for every output, every driven parameter and every meter, none twice", () => {
    const { meters, modulation } = snapshot.telemetry;
    expect(modulation.filter((entry) => entry.kind === "modOutput")).toHaveLength(32);
    expect(modulation.filter((entry) => entry.kind === "effective")).toHaveLength(64);
    expect(meters.filter((entry) => entry.kind === "peak")).toHaveLength(9); // the master and the lanes
    expect(meters.filter((entry) => entry.kind === "clips")).toHaveLength(9);
    const slots = [...meters, ...modulation].map((entry) => entry.slot);
    expect(new Set(slots).size).toBe(slots.length);
    // The outputs come before the parameters, as the server lists them.
    expect(modulation.findLastIndex((entry) => entry.kind === "modOutput")).toBeLessThan(modulation.findIndex((entry) => entry.kind === "effective"));
  });

  test("has the layout's entries at ports the nodes have", () => {
    for (const entry of snapshot.telemetry.modulation) {
      const node = nodes.find((candidate) => candidate.id === entry.node);
      expect(node, JSON.stringify(entry)).toBeDefined();
      expect(node?.kind === "modulator" ? "modOutput" : "effective").toBe(entry.kind);
    }
  });

  test("gives every lane its own LFO rates, so that nothing moves in step", () => {
    const rates = nodes.flatMap((node) => (node.kind === "modulator" && node.source.type === "lfo" ? [node.source.rate] : []));
    expect(new Set(rates).size).toBe(32);
  });

  test("takes events as the tracer does: a parameter of the 5th lane's filter changes", () => {
    const filter = nodes.filter((node) => node.kind === "effect")[4];
    if (filter?.kind !== "effect") throw new Error("no fifth filter");
    const applied = applyEvent(snapshot, { rev: 1, type: "paramSet", node: filter.id, param: 0, value: 0.9 });
    expect(applied.ok).toBe(true);
  });

  test("can be made smaller, which is what a test that only wants the shape does", () => {
    const small = stressSnapshot({ lanes: 2, lfosPerLane: 1, targetsPerLfo: 3 });
    expect(small.graph.nodes.filter((node) => node.kind === "modulator")).toHaveLength(2);
    expect(small.graph.edges.filter((edge) => edge.kind === "modulation")).toHaveLength(6);
    expect(small.telemetry.modulation.filter((entry) => entry.kind === "effective")).toHaveLength(6);
  });
});
