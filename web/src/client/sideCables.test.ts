import { describe, expect, test } from "vitest";

import type { Edge, Intent, NodeId, Snapshot } from "../protocol";
import { MASTER, builtinEffects, shapeOf, sideCablesFixture } from "./fixtures";
import type { Shape } from "./fixtures";
import { MockServer } from "./mock";
import { displayNames, endLabel } from "./names";
import { sendItems, sendTarget } from "./sends";
import { audioTakers, cableEnds, cablesOf, cableTakers, linkOrCable } from "./wiring";

const { snapshot, rows } = sideCablesFixture;
const { graph } = snapshot;

/** What the mock engine makes of the intent on the session of the fixture: the session after, or the words it refused with. */
function made(intent: Intent): { after: Snapshot } | { refused: string } {
  const server = new MockServer({ snapshot });
  try {
    server.apply(intent);
  } catch (error) {
    return { refused: error instanceof Error ? error.message : String(error) };
  }
  return { after: server.snapshot };
}

const LISTS = ["nodes", "audio", "cables", "meters", "modulation"] as const;
const same = (a: unknown, b: unknown): boolean => JSON.stringify(a) === JSON.stringify(b);
const byId = (a: NodeId, b: NodeId): number => a - b;

// The session of the fixture (`fixtures/sideCables.json`, `testutil::SIDED` with a free LFO and two cables drawn): group 0 has a math effect (1) on its bus; lane 2 is an oscillator with a filter (3) and
// a math effect (4); lane 5 is an oscillator whose way out is cut; 6 is the LFO. Three audio cables end at the second input of lane 2's math effect: 0 from lane 5's instrument (the file's), 1
// from the group's output and 2 from the filter.
const [GROUP, BUS_MATH, LANE, FILTER, MATH, OTHER_LANE] = [0, 1, 2, 3, 4, 5];

describe("the mock engine's second input of an effect", () => {
  test("is what the controller makes of each intent of its fixture: its words when it refuses one, the audio cables the session has after a cable or a level, and everything the session comes to after the rest", () => {
    const wrong: string[] = [];
    const before: Shape = shapeOf(snapshot);
    for (const row of rows) {
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
      const shape: Shape = shapeOf(result.after);
      if ("sends" in row) {
        if (!same(shape.sends ?? [], row.sends)) wrong.push(`${what}: the audio cables are ${JSON.stringify(shape.sends ?? [])}, not ${JSON.stringify(row.sends)}`);
        // A cable that is drawn, and a level that is set, change nothing else.
        for (const list of LISTS) {
          if (!same(shape[list], before[list])) wrong.push(`${what}: ${list} are ${JSON.stringify(shape[list])}, and were ${JSON.stringify(before[list])}`);
        }
        continue;
      }
      for (const list of [...LISTS, "sends"] as const) {
        const got = list === "sends" ? (shape.sends ?? []) : shape[list];
        const wanted = list === "sends" ? (row.after.sends ?? []) : row.after[list];
        if (!same(got, wanted)) wrong.push(`${what}: ${list} are ${JSON.stringify(got)}, not ${JSON.stringify(wanted)}`);
      }
    }
    expect(wrong).toEqual([]);
  });

  test("has tried each way that the controller refuses a cable at a second input and takes one: the fixture is not empty", () => {
    const words = new Set(rows.flatMap((row) => ("error" in row ? [row.error] : [])));
    for (const sentence of [
      "that effect has no second input",
      "the mix has no second input",
      "a group has one input, its bus",
      "there is a cable between those two already",
      "that cable would make the audio go round: it would feed what it comes from",
      "putting that lane in that group would make the audio go round, by the cables there are",
      "moving that effect there would make the audio go round, by the cables there are",
      "an audio cable starts at an output of audio",
      "an audio cable ends at an input of audio",
    ]) {
      expect(words.has(sentence), sentence).toBe(true);
    }
    const toSide = rows.filter((row) => row.intent.type === "addCable" && row.intent.to.port.type === "audioInSide");
    expect(toSide.filter((row) => !("error" in row)).length).toBeGreaterThanOrEqual(8);
    expect(toSide.filter((row) => "error" in row).length).toBeGreaterThanOrEqual(60);
    // (The cables that are drawn at the second input say so in the shape's lines.)
    expect(shapeOf(snapshot).sends).toEqual(["#0 5>4 side @1", "#1 0>4 side @1", "#2 3>4 side @1"]);
  });

  test("keeps a cable at the first input of an effect apart from one at its second: both can be drawn between the same two nodes, and each only once", () => {
    const server = new MockServer({ snapshot });
    const sends = (): string[] => shapeOf(server.snapshot).sends ?? [];
    // (The other lane's instrument has its cable at the second input of the math effect already.)
    server.apply({ type: "addCable", from: { node: OTHER_LANE, port: { type: "audioOut" } }, to: { node: MATH, port: { type: "audioIn" } }, gain: null });
    expect(sends()).toEqual(["#0 5>4 side @1", "#1 0>4 side @1", "#2 3>4 side @1", "#3 5>4 @1"]);
    expect(() => {
      server.apply({ type: "addCable", from: { node: OTHER_LANE, port: { type: "audioOut" } }, to: { node: MATH, port: { type: "audioInSide" } }, gain: null });
    }).toThrow("there is a cable between those two already");
    expect(() => {
      server.apply({ type: "addCable", from: { node: OTHER_LANE, port: { type: "audioOut" } }, to: { node: MATH, port: { type: "audioIn" } }, gain: 0.5 });
    }).toThrow("there is a cable between those two already");
    // Undo takes the cable at the first input out, and leaves the one at the second.
    server.apply({ type: "undo" });
    expect(sends()).toEqual(["#0 5>4 side @1", "#1 0>4 side @1", "#2 3>4 side @1"]);
    server.apply({ type: "redo" });
    expect(sends()).toContain("#3 5>4 @1");
  });

  test("gives a math effect that is added its two inputs, as the controller's node has them, and a cable can be drawn to its second", () => {
    expect(builtinEffects["builtin:math"]?.side).toEqual({ main: "A", side: "B" });
    expect(builtinEffects["builtin:filter"]?.side).toBeUndefined();
    const server = new MockServer({ snapshot });
    server.apply({ type: "addFreeEffect", plugin: "builtin:math" });
    const added = server.snapshot.graph.nodes.filter((n) => n.kind === "effect" && n.name === "Math");
    expect(added).toHaveLength(3);
    const effect = added[2];
    expect(effect?.kind === "effect" ? effect.side : undefined).toEqual({ main: "A", side: "B" });
    // (The new lane is bare: nothing feeds the module that stands by itself but the cables.)
    server.apply({ type: "addCable", from: { node: OTHER_LANE, port: { type: "audioOut" } }, to: { node: effect?.id ?? -1, port: { type: "audioInSide" } }, gain: null });
    expect(shapeOf(server.snapshot).sends?.at(-1)).toBe(`#3 5>${String(effect?.id)} side @1`);
    // An effect that is added to a lane's chain has its two inputs too, and one that has none has none.
    server.apply({ type: "addEffect", parent: GROUP, plugin: "builtin:math" });
    server.apply({ type: "addEffect", parent: GROUP, plugin: "builtin:reverb" });
    const bus = server.snapshot.graph.nodes.filter((n) => n.kind === "effect" && n.parent === GROUP);
    expect(bus.map((n) => (n.kind === "effect" ? n.side?.side : "?"))).toEqual(["B", "B", undefined]);
  });
});

describe("the page's rules for the second input of an effect", () => {
  const output = (node: NodeId) => ({ node, port: { type: "audioOut" } }) as const;
  const input = (node: NodeId) => ({ node, port: { type: "audioIn" } }) as const;
  const second = (node: NodeId) => ({ node, port: { type: "audioInSide" } }) as const;

  test("read the ends of a cable at a second input as the controller does, and refuse as it does, in its order", () => {
    expect(cableEnds(graph, output(OTHER_LANE), second(MATH))).toEqual({ from: { kind: "instrument", node: OTHER_LANE }, to: { kind: "effectSide", node: MATH } });
    expect(cableEnds(graph, output(FILTER), input(MATH))).toEqual({ from: { kind: "effect", node: FILTER }, to: { kind: "effect", node: MATH } });
    expect(cableEnds(graph, output(OTHER_LANE), second(BUS_MATH))).toEqual({ from: { kind: "instrument", node: OTHER_LANE }, to: { kind: "effectSide", node: BUS_MATH } });
    expect(cableEnds(graph, output(LANE), second(FILTER))).toEqual({ error: { code: "invalid", message: "that effect has no second input" } });
    expect(cableEnds(graph, output(LANE), second(MASTER))).toEqual({ error: { code: "invalid", message: "the mix has no second input" } });
    expect(cableEnds(graph, output(LANE), second(GROUP))).toEqual({ error: { code: "invalid", message: "a group has one input, its bus" } });
    expect(cableEnds(graph, output(LANE), second(OTHER_LANE))).toEqual({ error: { code: "invalid", message: "a lane has no input of audio: a cable ends at an effect, a group or the mix" } });
    expect(cableEnds(graph, output(LANE), second(99))).toEqual({ error: { code: "notFound", message: "there is no node 99" } });
    // (Not the port of a start, and not the port of an end that is not audio's.)
    expect(cableEnds(graph, second(LANE), second(MATH))).toEqual({ error: { code: "invalid", message: "an audio cable starts at an output of audio" } });
    expect(cableEnds(graph, output(LANE), { node: MATH, port: { type: "eventsIn" } })).toEqual({ error: { code: "invalid", message: "an audio cable ends at an input of audio" } });
  });

  test("have the cables of the graph end at an input of an effect, the first or the second, by the port the edge says", () => {
    const ends = cablesOf(graph);
    expect(ends.map((cable) => cable.to.kind)).toEqual(["effectSide", "effectSide", "effectSide"]);
    expect(ends.map((cable) => cable.to.node)).toEqual([MATH, MATH, MATH]);
  });

  test("leave out an edge that the controller never draws, a second input of a group or of an effect that has none, as one with a node that is not there is left out", () => {
    const stray = (to: NodeId, id: number): Edge => ({ kind: "cable", id, gain: 1, from: output(LANE), to: second(to) });
    expect(cablesOf({ ...graph, edges: [...graph.edges, stray(GROUP, 90), stray(FILTER, 91)] })).toEqual(cablesOf(graph));
    // (And the same edge at the second input of an effect that has one is a cable.)
    expect(cablesOf({ ...graph, edges: [...graph.edges, stray(BUS_MATH, 92)] })).toHaveLength(cablesOf(graph).length + 1);
  });

  test("never take a cable at a second input for a link of the audio path: it is a cable, drawn or refused as one", () => {
    const intent: Intent = { type: "addCable", from: output(FILTER), to: second(MATH), gain: null };
    expect(linkOrCable(graph, intent)).toBe(intent);
    // (The link into the first input of the same effect is there: a cable there is refused as a link's.)
    expect(linkOrCable(graph, { type: "addCable", from: output(FILTER), to: input(MATH), gain: null })).toEqual({
      error: { code: "invalid", message: "there is a cable between those two already" },
    });
  });

  test("are those of the controller's AddCable for every start of the fixture: the first and the second inputs that take a cable without a level of its own are the takers", () => {
    const starts = graph.nodes.filter((n) => n.kind === "instrument" || n.kind === "effect" || n.kind === "group").map((n) => n.id);
    const taken = (from: NodeId, port: "audioIn" | "audioInSide"): NodeId[] =>
      rows.flatMap((row) =>
        row.intent.type === "addCable" && row.intent.gain === null && row.intent.from.node === from && row.intent.from.port.type === "audioOut" && row.intent.to.port.type === port && !("error" in row)
          ? [row.intent.to.node]
          : [],
      );
    for (const start of starts) {
      const takers = audioTakers(graph, start);
      expect([...takers.main].sort(byId), `the first inputs from ${String(start)}`).toEqual(taken(start, "audioIn").sort(byId));
      expect([...takers.side].sort(byId), `the second inputs from ${String(start)}`).toEqual(taken(start, "audioInSide").sort(byId));
      expect(cableTakers(graph, start), `cableTakers is the first inputs from ${String(start)}`).toEqual(takers.main);
    }
    // (The second inputs that take a cable from each start: the math effect of lane 2 has three cables at its own, so no other start is offered it more than once, and the effects that would feed what feeds them are not offered.)
    expect(Object.fromEntries(starts.map((start) => [start, audioTakers(graph, start).side]))).toEqual({ [GROUP]: [], [BUS_MATH]: [MATH], [LANE]: [BUS_MATH, MATH], [FILTER]: [BUS_MATH], [MATH]: [], [OTHER_LANE]: [BUS_MATH] });
    // (A node that has no output of audio, and one that is not there, has none.)
    for (const none of [MASTER, 6, 99]) expect(audioTakers(graph, none), String(none)).toEqual({ main: [], side: [] });
  });

  test("name the two inputs of an effect that has two by what its plugin calls them, and an effect that has one by its name alone", () => {
    const names = displayNames(graph);
    expect(endLabel(graph, names, second(MATH))).toBe("Math 2: B");
    expect(endLabel(graph, names, input(MATH))).toBe("Math 2: A");
    expect(endLabel(graph, names, second(BUS_MATH))).toBe("Math 1: B");
    expect(endLabel(graph, names, input(FILTER))).toBe("Filter");
    expect(endLabel(graph, names, second(FILTER))).toBe("Filter");
  });

  test("offer a menu both inputs of an effect that has two, by name, each only while it takes the cable, and read an item back as its node and input", () => {
    const items = sendItems(graph, OTHER_LANE).filter((item) => item.group === "Effects");
    // (The other lane's instrument has a cable at the second input of lane 2's math effect already: that one is not offered. Its way out is cut, so it feeds nothing else.)
    expect(items.map((item) => `${item.id} ${item.label}`)).toEqual(["1 Math 1 (A)", "1:side Math 1 (B)", "3 Filter", "4 Math 2 (A)"]);
    expect(sendTarget("4")).toEqual({ node: 4, side: false });
    expect(sendTarget("4:side")).toEqual({ node: 4, side: true });
    for (const item of items) expect(sendTarget(item.id), item.id).toEqual({ node: Number.parseInt(item.id, 10), side: item.id.endsWith(":side") });
  });
});
