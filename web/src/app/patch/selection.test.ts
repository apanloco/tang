import { describe, expect, test } from "vitest";

import { moduleGraphsFixture } from "../../client/fixtures";
import type { Node } from "../../protocol";
import { modulePluginsOf } from "./selection";

// What a module can hold of the nodes that are chosen on the canvas: the plugins (the instrument of a lane that has one, an effect that is in a lane), and nothing else, which is left out and counted.

const { graph } = moduleGraphsFixture.snapshot;

const find = (predicate: (node: Node) => boolean): Node => {
  const found = graph.nodes.find(predicate);
  if (!found) throw new Error("the fixture has no such node");
  return found;
};
const oscillator = find((node) => node.kind === "instrument" && node.plugin !== null);
const filter = find((node) => node.kind === "effect" && node.parent === oscillator.id);
const holder = find((node) => node.kind === "instrument" && node.plugin === null && graph.nodes.some((n) => n.parent === node.id));
const math = find((node) => node.kind === "effect" && node.parent === holder.id);
const empty = find((node) => node.kind === "instrument" && node.plugin === null && !graph.nodes.some((n) => n.parent === node.id));
const group = find((node) => node.kind === "group");
const bus = find((node) => node.kind === "effect" && node.parent === group.id);
const modulator = find((node) => node.kind === "modulator");
const source = find((node) => node.kind === "source");
const master = find((node) => node.kind === "master");

describe("what a module holds of what is chosen", () => {
  test("is the instrument of a lane that has one and the effects that are in a lane, in the order they were chosen in", () => {
    expect(modulePluginsOf(graph, [math.id, oscillator.id, filter.id])).toEqual({ nodes: [math.id, oscillator.id, filter.id], left: 0 });
    expect(modulePluginsOf(graph, [filter.id])).toEqual({ nodes: [filter.id], left: 0 });
    expect(modulePluginsOf(graph, [])).toEqual({ nodes: [], left: 0 });
  });

  test("leaves out what a module has none of, and counts it: a group and the bus effect in it, a modulator, a source of notes, an empty lane, the lane of effects alone, and the master", () => {
    for (const other of [group, bus, modulator, source, empty, holder, master]) {
      expect(modulePluginsOf(graph, [other.id]), `${other.kind} ${other.name}`).toEqual({ nodes: [], left: 1 });
    }
    expect(modulePluginsOf(graph, [filter.id, modulator.id, bus.id, math.id, master.id])).toEqual({ nodes: [filter.id, math.id], left: 3 });
  });

  test("leaves out a node that the session does not have, and everything for a session that is not there yet", () => {
    expect(modulePluginsOf(graph, [filter.id, 99_999])).toEqual({ nodes: [filter.id], left: 1 });
    expect(modulePluginsOf(null, [filter.id, math.id])).toEqual({ nodes: [], left: 2 });
  });

  test("takes an effect only if the node that holds it is a lane: one whose parent is a group is a bus effect, and one with no parent is in no lane", () => {
    const orphan = { ...filter, id: 88_888, parent: null } as Node;
    const moved = { ...graph, nodes: [...graph.nodes, orphan] };
    expect(modulePluginsOf(moved, [orphan.id])).toEqual({ nodes: [], left: 1 });
    expect(modulePluginsOf(moved, [bus.id])).toEqual({ nodes: [], left: 1 });
    expect(modulePluginsOf(moved, [filter.id])).toEqual({ nodes: [filter.id], left: 0 });
  });
});
