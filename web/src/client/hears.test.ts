import { describe, expect, test } from "vitest";

import type { Graph, Node, NodeId } from "../protocol";
import { freeReachFixture } from "./fixtures";
import { EVERY_NOTE, earItems, earWords, hearsOf } from "./hears";
import { displayNames } from "./names";

// Whose notes a modulator hears, as the menu on its node has it (step 93; every modulator is the session's own since step 96). The fixture is a session of groups, lanes and modulators: the nodes
// 18 (an LFO) and 19 (an envelope) hear every note, 12 to 17 hear a lane or a group or nothing; the lanes are 4, 6, 7, 9 (oscillators) and 11 (empty), the groups 0 and 2.

const base = freeReachFixture.snapshot.graph;
const FREE = 18;

/** The graph with the modulator `node` hearing `hears` (nothing for none). */
const hearing = (node: NodeId, hears?: NodeId, graph: Graph = base): Graph => ({
  ...graph,
  nodes: graph.nodes.map((n): Node => {
    if (n.id !== node || n.kind !== "modulator") return n;
    const copy = { ...n };
    if (hears === undefined) delete copy.hears;
    else copy.hears = hears;
    return copy;
  }),
});

describe("whom a modulator hears", () => {
  test("is every note for a modulator that has no ear, a lane or a group that is there for one that has, and none for what is not a modulator", () => {
    expect(hearsOf(base, FREE)).toBeUndefined();
    expect(hearsOf(hearing(FREE, 7), FREE)).toBe(7);
    expect(hearsOf(hearing(FREE, 2), FREE)).toBe(2);
    // An ear that names an effect, a modulator, the master or a node that is not there is no ear (the graph the server gives says none for a lane that is gone).
    for (const lost of [5, 19, 4294967295, 9999]) expect(hearsOf(hearing(FREE, lost), FREE), String(lost)).toBeUndefined();
    // The fixture's modulators that have an ear hear it, and what is no modulator hears nothing.
    expect([12, 15, 16, 17].map((modulator) => hearsOf(base, modulator))).toEqual([4, 0, 2, undefined]);
    expect(hearsOf(base, 4)).toBeUndefined();
    expect(hearsOf(base, 9999)).toBeUndefined();
  });

  test("is said in the words of the canvas: the name that it gives the lane or the group, and every note", () => {
    const names = displayNames(base);
    expect(earWords(base, FREE)).toBe("every note");
    expect(earWords(hearing(FREE, 7), FREE)).toBe(names.get(7));
    expect(names.get(7)).toMatch(/^Oscillator \d$/);
    expect(earWords(hearing(FREE, 2), FREE)).toBe("Group 2");
  });
});

describe("the items of the menu that chooses whom a modulator hears", () => {
  test("are every note, then the lanes and then the groups by the names that the canvas gives them, in the order of the graph, each under its heading", () => {
    const names = displayNames(base);
    const items = earItems(base, FREE);
    expect(items[0]).toEqual({ id: EVERY_NOTE, label: "Every note", current: true });
    expect(items.slice(1).map((item) => [item.id, item.label, item.group])).toEqual([
      ["4", names.get(4), "Lanes"],
      ["6", names.get(6), "Lanes"],
      ["7", names.get(7), "Lanes"],
      ["9", names.get(9), "Lanes"],
      ["11", names.get(11), "Lanes"],
      ["0", "Group 1", "Groups"],
      ["2", "Group 2", "Groups"],
    ]);
    expect(new Set(items.slice(1).map((item) => item.label)).size).toBe(7);
  });

  test("mark the one that the modulator hears, and every note when it hears none", () => {
    const current = (graph: Graph): string[] => earItems(graph, FREE).filter((item) => item.current).map((item) => item.id);
    expect(current(base)).toEqual([EVERY_NOTE]);
    expect(current(hearing(FREE, 9))).toEqual(["9"]);
    expect(current(hearing(FREE, 0))).toEqual(["0"]);
    expect(current(hearing(FREE, 5)), "an ear that is no lane or group is every note").toEqual([EVERY_NOTE]);
    // (Another modulator's ear is not this one's.)
    expect(current(hearing(19, 9))).toEqual([EVERY_NOTE]);
  });

  test("leave out a lane that only holds effects, which the canvas draws no card for", () => {
    // The empty lane (11) is given an effect and nothing else: a holder, drawn as the effect it holds.
    const reverb = base.nodes.find((n) => n.kind === "effect" && n.name === "Reverb");
    if (reverb?.kind !== "effect") throw new Error("the fixture has a reverb");
    const held: Graph = { ...base, nodes: [...base.nodes, { ...reverb, id: 99, parent: 11 }] };
    const ids = earItems(held, FREE).map((item) => item.id);
    expect(ids).not.toContain("11");
    expect(ids).toContain("4");
  });

  test("are the same for every modulator, the one it hears marked, and none for what is no modulator", () => {
    expect(earItems(base, 12).filter((item) => item.current).map((item) => item.id)).toEqual(["4"]);
    expect(earItems(base, 16).filter((item) => item.current).map((item) => item.id)).toEqual(["2"]);
    expect(earItems(base, 4)).toEqual([]);
    expect(earItems(base, 9999)).toEqual([]);
  });
});
