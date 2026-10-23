import { describe, expect, test } from "vitest";

import type { Edge, Intent, Snapshot } from "../protocol";
import { MASTER, audioCablesFixture, shapeOf, tracerSnapshot } from "./fixtures";
import type { Shape } from "./fixtures";
import { planOf, takesOut } from "./history";
import { MockServer } from "./mock";

/** What the mock engine makes of the intent on the session of the fixture: the session after, or the words it refused with. */
function made(intent: Intent): { after: Snapshot } | { refused: string } {
  const server = new MockServer({ snapshot: audioCablesFixture.snapshot });
  try {
    server.apply(intent);
  } catch (error) {
    return { refused: error instanceof Error ? error.message : String(error) };
  }
  return { after: server.snapshot };
}

const LISTS = ["nodes", "audio", "cables", "meters", "modulation"] as const;
const same = (a: unknown, b: unknown): boolean => JSON.stringify(a) === JSON.stringify(b);

// The session of the fixture (`fixtures/audioCables.json`, `testutil::PATCHED`): group 0 ("Pad") has a filter, a reverb and a delay on its bus and lane 0 in it; lane 0 has a filter, a delay and an
// LFO that drives the filter (the cable of modulation, 0); lane 1, in no group, has a drive; lane 2 is empty. The audio cables: 1 from lane 1 to lane 0's filter, 2 from lane 0's filter to its
// delay, 3 from the group's output to lane 1's drive, 4 from the bus filter to the bus reverb, 5 from lane 0's delay to the group's bus, 6 from the bus reverb to the mix.
describe("the mock engine's audio cables", () => {
  test("are what the controller makes of each intent of its fixture: its words when it refuses one, the audio cables the session has after a cable or a level, and everything the session comes to after the rest", () => {
    const wrong: string[] = [];
    for (const row of audioCablesFixture.rows) {
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
        const before: Shape = shapeOf(audioCablesFixture.snapshot);
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

  test("give back a link of the audio path that is cut when a cable is asked for between its two ends, as the controller does, on the session with every link cut: its words when it refuses, else the links, what is cut and the cables it comes to", () => {
    const wrong: string[] = [];
    for (const row of audioCablesFixture.cutRows) {
      const what = JSON.stringify(row.intent);
      const server = new MockServer({ snapshot: audioCablesFixture.cutSnapshot });
      let refused: string | null = null;
      try {
        server.apply(row.intent);
      } catch (error) {
        refused = error instanceof Error ? error.message : String(error);
      }
      if ("error" in row) {
        if (refused !== row.error) wrong.push(`${what}: ${refused ?? "done"}, and the controller says ${row.error}`);
        continue;
      }
      if (refused !== null) {
        wrong.push(`${what}: refused (${refused}), and the controller took it`);
        continue;
      }
      const shape = shapeOf(server.snapshot);
      // (The lists one by one: the fixture's keys are in the order that Rust writes them, which is not the order of a shape's.)
      const got = { audio: shape.audio, feeds: shape.feeds ?? [], routes: shape.routes ?? [], sends: shape.sends ?? [] };
      for (const list of ["audio", "feeds", "routes", "sends"] as const) {
        if (!same(got[list], row.links[list])) wrong.push(`${what}: ${list} are ${JSON.stringify(got[list])}, not ${JSON.stringify(row.links[list])}`);
      }
    }
    expect(wrong).toEqual([]);
    // (Every start tried with every end: 13 by 13, of which a good part is a link given back.)
    expect(audioCablesFixture.cutRows).toHaveLength(169);
    expect(audioCablesFixture.cutRows.filter((row) => "links" in row).length).toBeGreaterThanOrEqual(60);
  });

  test("have been tried in each of the ways the controller refuses them and takes them: the fixture is not empty", () => {
    const rows = audioCablesFixture.rows;
    const tried = [
      ["addCable", 30, 100],
      ["setCableGain", 20, 8],
      ["setEdgeDepth", 1, 2],
      ["removeEdge", 6, 1],
      ["removeNode", 8, 2],
      ["setLaneGroup", 5, 1],
      ["moveEffect", 3, 5],
    ] as const;
    for (const [type, done, refused] of tried) {
      expect(rows.filter((row) => row.intent.type === type && !("error" in row)).length, `${type} done`).toBeGreaterThanOrEqual(done);
      expect(rows.filter((row) => row.intent.type === type && "error" in row).length, `${type} refused`).toBeGreaterThanOrEqual(refused);
    }
    const words = new Set(rows.flatMap((row) => ("error" in row ? [row.error] : [])));
    for (const sentence of [
      "an audio cable starts at an output of audio",
      "an audio cable ends at an input of audio",
      "the mix has no output of audio",
      "a modulator has no output of audio",
      "a modulator has no input of audio",
      "a lane has no input of audio: a cable ends at an effect, a group or the mix",
      "there is a cable between those two already",
      "that cable would make the audio go round: it would feed what it comes from",
      "putting that lane in that group would make the audio go round, by the cables there are",
      "moving that effect there would make the audio go round, by the cables there are",
      "an audio cable has a level, and no depth",
      "a modulation cable has a depth, and no level",
      "there is no node 9999",
      "there is no cable 9999",
    ]) {
      expect(words.has(sentence), sentence).toBe(true);
    }
  });

  test("hand out the id after the last cable there is, of either kind, and the id that undo gives back to a cable that was taken out", () => {
    const server = new MockServer({ snapshot: audioCablesFixture.snapshot });
    const lane = audioCablesFixture.snapshot.graph.nodes.filter((n) => n.kind === "instrument")[2];
    if (!lane) throw new Error("the session has three lanes");
    const cables = (): number[] => server.snapshot.graph.edges.flatMap((e) => (e.kind === "cable" ? [e.id] : []));
    expect(cables()).toEqual([1, 2, 3, 4, 5, 6]);
    // (A cable at a level that is not the way out's: the empty lane's instrument goes to the mix by its way out, which a cable at its level would be.)
    server.apply({ type: "addCable", from: { node: lane.id, port: { type: "audioOut" } }, to: { node: MASTER, port: { type: "audioIn" } }, gain: 0.5 });
    expect(cables()).toEqual([1, 2, 3, 4, 5, 6, 7]);
    server.apply({ type: "removeEdge", edge: 3 });
    expect(cables()).toEqual([1, 2, 4, 5, 6, 7]);
    server.apply({ type: "undo" });
    expect(cables()).toEqual([1, 2, 3, 4, 5, 6, 7]);
    server.apply({ type: "undo" });
    expect(cables()).toEqual([1, 2, 3, 4, 5, 6]);
    server.apply({ type: "redo" });
    expect(cables()).toEqual([1, 2, 3, 4, 5, 6, 7]);
  });

  test("keep a step in the history for a cable that is drawn, a level that is turned and a cable that is taken out, in the words of the controller's", () => {
    const [oscillator] = tracerSnapshot.graph.nodes.filter((n) => n.kind === "instrument");
    const [filter] = tracerSnapshot.graph.nodes.filter((n) => n.kind === "effect");
    if (!oscillator || !filter) throw new Error("the tracer has an oscillator and a filter");
    // (The oscillator to the mix: a cable between the oscillator and its filter would be the link that is there, and the filter to the mix the lane's way out.)
    const draw: Intent = { type: "addCable", from: { node: oscillator.id, port: { type: "audioOut" } }, to: { node: MASTER, port: { type: "audioIn" } }, gain: null };
    const plan = planOf(tracerSnapshot, draw, 0, []);
    expect(plan).toMatchObject({ added: { label: "draw an audio cable" } });
    const server = new MockServer({ snapshot: tracerSnapshot });
    expect(server.history).toEqual({ undo: null, redo: null });
    server.apply(draw);
    expect(server.history).toEqual({ undo: "draw an audio cable", redo: null });
    const edge = server.snapshot.graph.edges.find((e): e is Edge & { kind: "cable" } => e.kind === "cable");
    if (!edge) throw new Error("the cable is drawn");
    server.apply({ type: "setCableGain", edge: edge.id, gain: 0.5 });
    expect(server.history).toEqual({ undo: "set the level of an audio cable", redo: null });
    server.apply({ type: "removeEdge", edge: edge.id });
    expect(server.history).toEqual({ undo: "remove an audio cable", redo: null });
    server.apply({ type: "undo" });
    expect(server.snapshot.graph.edges.find((e) => e.kind === "cable")).toMatchObject({ gain: 0.5 });
    expect(server.history).toEqual({ undo: "set the level of an audio cable", redo: "remove an audio cable" });
  });

  test("have no step for a level of a cable that is not there, a cable that is refused, or a cable that is drawn on a session that cannot take it", () => {
    expect(planOf(tracerSnapshot, { type: "setCableGain", edge: 31, gain: 0.5 }, 0, [])).toBe("nothing");
    const server = new MockServer({ snapshot: audioCablesFixture.snapshot });
    const [first] = audioCablesFixture.snapshot.graph.nodes;
    if (!first) throw new Error("the session has nodes");
    expect(() => {
      server.apply({ type: "addCable", from: { node: first.id, port: { type: "audioIn" } }, to: { node: first.id, port: { type: "audioIn" } }, gain: null });
    }).toThrow("an audio cable starts at an output of audio");
    expect(server.history).toEqual({ undo: null, redo: null });
  });

  test("refuse a level that is not a number, in the controller's words, for a cable that is drawn and for one that is turned, and change nothing", () => {
    const server = new MockServer({ snapshot: audioCablesFixture.snapshot });
    const draw = (gain: number): Intent => ({ type: "addCable", from: { node: 10, port: { type: "audioOut" } }, to: { node: MASTER, port: { type: "audioIn" } }, gain });
    // (Rust writes the infinite 32-bit numbers `inf` and `-inf`; a number that is finite to the page and too big for 32 bits is one of them there.)
    for (const [gain, words] of [[Number.NaN, "NaN"], [Infinity, "inf"], [-Infinity, "-inf"], [1e39, "inf"], [-1e39, "-inf"]] as const) {
      expect(() => server.apply(draw(gain)), `draw ${words}`).toThrow(`${words} is not a value for the level of a cable`);
      expect(() => server.apply({ type: "setCableGain", edge: 1, gain }), `turn ${words}`).toThrow(`${words} is not a value for the level of a cable`);
    }
    expect(server.snapshot).toBe(audioCablesFixture.snapshot);
    expect(server.history).toEqual({ undo: null, redo: null });
  });

  test("take out, for a draw, the one cable it made and no more: not what made two, none, a node besides, or a cable that is not there", () => {
    const { snapshot } = audioCablesFixture;
    const draw: Intent = { type: "addCable", from: { node: 10, port: { type: "audioOut" } }, to: { node: MASTER, port: { type: "audioIn" } }, gain: null };
    expect(takesOut(snapshot, draw, { nodes: [], edges: [3] })).toEqual({ type: "removeEdge", edge: 3 });
    expect(takesOut(snapshot, draw, { nodes: [], edges: [] })).toBeNull();
    expect(takesOut(snapshot, draw, { nodes: [], edges: [3, 4] })).toBeNull();
    expect(takesOut(snapshot, draw, { nodes: [4], edges: [3] })).toBeNull();
    expect(takesOut(snapshot, draw, { nodes: [], edges: [99] })).toBeNull();
    // (An edge that is a cable of modulation is not what a draw made.)
    expect(takesOut(snapshot, draw, { nodes: [], edges: [0] })).toBeNull();
  });

  test("list the cables that have been drawn between two places as the sends of a session, and nothing else changes with them", () => {
    const [oscillator] = tracerSnapshot.graph.nodes.filter((n) => n.kind === "instrument");
    const [filter] = tracerSnapshot.graph.nodes.filter((n) => n.kind === "effect");
    if (!oscillator || !filter) throw new Error("the tracer has an oscillator and a filter");
    const send: Edge = { kind: "cable", id: 31, gain: 0.5, from: { node: oscillator.id, port: { type: "audioOut" } }, to: { node: filter.id, port: { type: "audioIn" } } };
    const drawn = { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, edges: [...tracerSnapshot.graph.edges, send] } };
    expect(shapeOf(tracerSnapshot).sends).toBeUndefined();
    expect(shapeOf(drawn)).toEqual({ ...shapeOf(tracerSnapshot), sends: [`#31 ${String(oscillator.id)}>${String(filter.id)} @0.5`] });
  });
});
