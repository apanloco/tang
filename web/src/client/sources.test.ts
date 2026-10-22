import { describe, expect, test } from "vitest";

import type { Intent, Snapshot, StampedEvent } from "../protocol";
import { shapeOf, sourcesFixture, tracerSnapshot } from "./fixtures";
import type { Shape } from "./fixtures";
import { planOf, takesOut } from "./history";
import { MockServer } from "./mock";

/** What the mock engine makes of the intent on the session `snapshot`: the events it told, whether it changed the structure and the session after, or the words it refused with. */
function made(snapshot: Snapshot, intent: Intent): { events: StampedEvent[]; restructured: boolean; after: Snapshot } | { refused: string } {
  const server = new MockServer({ snapshot });
  try {
    const told = server.apply(intent);
    return { events: told.events, restructured: told.restructured, after: server.snapshot };
  } catch (error) {
    return { refused: error instanceof Error ? error.message : String(error) };
  }
}

const LISTS = ["nodes", "audio", "cables", "sends", "routes", "sources", "notes", "hears", "order", "meters", "modulation"] as const;
/** A value as text, the keys of an object in order of their names (the controller's JSON has them so, and the page's events have them in the order they were made in). */
const canonical = (value: unknown): string => JSON.stringify(value, (_key, part: unknown) => (part !== null && typeof part === "object" && !Array.isArray(part) ? Object.fromEntries(Object.entries(part).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))) : part));
const same = (a: unknown, b: unknown): boolean => canonical(a) === canonical(b);
const listOf = (shape: Shape, list: (typeof LISTS)[number]): string[] => shape[list] ?? [];

const fixtureCase = (name: string): (typeof sourcesFixture.cases)[number] => {
  const found = sourcesFixture.cases.find((entry) => entry.name === name);
  if (!found) throw new Error(`the fixture has no case called ${name}`);
  return found;
};

// The sessions of the fixture (`fixtures/sources.json`, `testutil::NOTES` and a session of two lanes): "wired" has the keys and a device, a group ("Pad") with a reverb on its bus, lane 0 in it with
// a filter and an LFO that drives it, a plain lane (1) and an empty one (2), a cable of notes from the keys to lane 1 and an audio cable from lane 1 to lane 0's filter; "bare" has no source.
describe("the mock engine's sources of notes and cables of notes", () => {
  test("are what the controller makes of each intent of its fixture: its words when it refuses one, and else the events it made, whether it changed the structure and everything the session comes to", () => {
    const wrong: string[] = [];
    for (const { name, snapshot, rows } of sourcesFixture.cases) {
      for (const row of rows) {
        const what = `${name}: ${JSON.stringify(row.intent)}`;
        const result = made(snapshot, row.intent);
        if ("error" in row) {
          const got = "refused" in result ? result.refused : "done";
          if (got !== row.error) wrong.push(`${what}: ${got}, and the controller says ${row.error}`);
          continue;
        }
        if ("refused" in result) {
          wrong.push(`${what}: refused (${result.refused}), and the controller took it`);
          continue;
        }
        if (result.restructured !== row.restructured) wrong.push(`${what}: restructured is ${String(result.restructured)}, and the controller says ${String(row.restructured)}`);
        if (!same(result.events, row.events)) wrong.push(`${what}: the events are ${JSON.stringify(result.events)}, not ${JSON.stringify(row.events)}`);
        const shape = shapeOf(result.after);
        for (const list of LISTS) {
          if (!same(listOf(shape, list), listOf(row.after, list))) wrong.push(`${what}: ${list} are ${JSON.stringify(listOf(shape, list))}, not ${JSON.stringify(listOf(row.after, list))}`);
        }
      }
    }
    expect(wrong).toEqual([]);
  });

  test("have been tried in each of the ways the controller refuses them and takes them: the fixture is not thin", () => {
    const rows = fixtureCase("wired").rows;
    const tried = [
      ["addSource", 8, 10],
      ["addCable", 6, 100],
      ["removeEdge", 3, 1],
      ["removeNode", 7, 2],
      ["setNodeUi", 4, 0],
      ["addInstrument", 1, 0],
      ["addGroup", 2, 2],
    ] as const;
    for (const [type, done, refused] of tried) {
      expect(rows.filter((row) => row.intent.type === type && !("error" in row)).length, `${type} done`).toBeGreaterThanOrEqual(done);
      expect(rows.filter((row) => row.intent.type === type && "error" in row).length, `${type} refused`).toBeGreaterThanOrEqual(refused);
    }
    const words = new Set(rows.flatMap((row) => ("error" in row ? [row.error] : [])));
    for (const sentence of [
      "the keys are a source already",
      "Launchkey Mini MK4 37 is a source already",
      "a device's name is blank: a device is called by its name, or as much of it as tells it",
      "a device's name is longer than 256 characters",
      "a device's name has a control character in it",
      "a cable of notes starts at an output of events",
      "a cable of notes ends at an input of events",
      "the mix has no output of events",
      "the mix has no input of events",
      "only a source of notes has an output of events",
      "a cable of notes ends at a lane: it is the lanes that have an input of events",
      "a cable of notes has no level: it carries notes, which are heard or not",
      "there is a cable between those two already",
      "a source of notes has no output of audio: it has an output of events",
      "a source of notes has no input of audio",
      "there is no node 9999",
      "there is no cable 9999",
    ]) {
      expect(words.has(sentence), sentence).toBe(true);
    }
    expect(fixtureCase("bare").rows.length).toBeGreaterThanOrEqual(8);
  });

  test("hand out the id after the last cable there is, of any kind, and the id that undo gives back to a cable that was taken out", () => {
    const { snapshot } = fixtureCase("wired");
    const server = new MockServer({ snapshot });
    const [keys, launchkey] = snapshot.graph.nodes.filter((n) => n.kind === "source");
    const [first, , empty] = snapshot.graph.nodes.filter((n) => n.kind === "instrument");
    if (!keys || !launchkey || !first || !empty) throw new Error("the session has two sources and three lanes");
    const cables = (): number[] => server.snapshot.graph.edges.flatMap((e) => (e.kind === "events" ? [e.id] : []));
    const draw = (from: number, to: number): void => {
      server.apply({ type: "addCable", from: { node: from, port: { type: "eventsOut" } }, to: { node: to, port: { type: "eventsIn" } }, gain: null });
    };
    // (The session's cables are 0, a cable of modulation, 1, a cable of notes, and 2, an audio cable: the next is 3.)
    expect(cables()).toEqual([1]);
    draw(keys.id, first.id);
    expect(cables()).toEqual([1, 3]);
    draw(launchkey.id, empty.id);
    expect(cables()).toEqual([1, 3, 4]);
    server.apply({ type: "removeEdge", edge: 3 });
    expect(cables()).toEqual([1, 4]);
    server.apply({ type: "undo" });
    expect(cables()).toEqual([1, 3, 4]);
    server.apply({ type: "undo" });
    server.apply({ type: "undo" });
    expect(cables()).toEqual([1]);
    server.apply({ type: "redo" });
    server.apply({ type: "redo" });
    expect(cables()).toEqual([1, 3, 4]);
  });

  test("hand out the id after the last cable there is when that is a cable of notes", () => {
    const { snapshot } = fixtureCase("wired");
    // (The audio cable, 2, is the last cable of the session: without it the cable of notes, 1, is.)
    const withoutAudio: Snapshot = { ...snapshot, graph: { ...snapshot.graph, edges: snapshot.graph.edges.filter((e) => e.kind !== "cable") } };
    const server = new MockServer({ snapshot: withoutAudio });
    const [keys] = snapshot.graph.nodes.filter((n) => n.kind === "source");
    const [first] = snapshot.graph.nodes.filter((n) => n.kind === "instrument");
    if (!keys || !first) throw new Error("the session has a source and a lane");
    server.apply({ type: "addCable", from: { node: keys.id, port: { type: "eventsOut" } }, to: { node: first.id, port: { type: "eventsIn" } }, gain: null });
    expect(server.snapshot.graph.edges.flatMap((e) => (e.kind === "events" ? [e.id] : []))).toEqual([1, 2]);
  });

  test("keep a step in the history for a source that is added, a cable that is drawn and each that is taken out, in the words of the controller's", () => {
    const [oscillator] = tracerSnapshot.graph.nodes.filter((n) => n.kind === "instrument");
    if (!oscillator) throw new Error("the tracer has a lane");
    const keys: Intent = { type: "addSource", source: { type: "keys" } };
    expect(planOf(tracerSnapshot, keys, 0, [])).toMatchObject({ added: { label: "add Keys" } });
    expect(planOf(tracerSnapshot, { type: "addSource", source: { type: "device", name: "  Seaboard RISE " } }, 0, [])).toMatchObject({ added: { label: "add Seaboard RISE" } });
    const draw: Intent = { type: "addCable", from: { node: 7, port: { type: "eventsOut" } }, to: { node: oscillator.id, port: { type: "eventsIn" } }, gain: null };
    expect(planOf(tracerSnapshot, draw, 0, [])).toMatchObject({ added: { label: "draw a cable of notes" } });
    // (An audio cable keeps its words: the ports tell which a cable is.)
    const audio: Intent = { type: "addCable", from: { node: oscillator.id, port: { type: "audioOut" } }, to: { node: oscillator.id, port: { type: "audioIn" } }, gain: null };
    expect(planOf(tracerSnapshot, audio, 0, [])).toMatchObject({ added: { label: "draw an audio cable" } });

    const { snapshot } = fixtureCase("wired");
    const [keysNode, deviceNode] = snapshot.graph.nodes.filter((n) => n.kind === "source");
    const notes = snapshot.graph.edges.find((e) => e.kind === "events");
    if (!keysNode || !deviceNode || notes?.kind !== "events") throw new Error("the session has two sources and a cable of notes");
    expect(planOf(snapshot, { type: "removeEdge", edge: notes.id }, 0, [])).toMatchObject({ removed: { label: "remove a cable of notes" } });
    expect(planOf(snapshot, { type: "removeNode", node: keysNode.id }, 0, [])).toMatchObject({ removed: { label: "remove Keys" } });
    expect(planOf(snapshot, { type: "removeNode", node: deviceNode.id }, 0, [])).toMatchObject({ removed: { label: "remove Launchkey Mini MK4 37" } });
  });

  test("are taken out by what an add made: the source and the cable of notes, each by its own intent", () => {
    const { snapshot } = fixtureCase("wired");
    const server = new MockServer({ snapshot });
    const [keys] = snapshot.graph.nodes.filter((n) => n.kind === "source");
    const [, , empty] = snapshot.graph.nodes.filter((n) => n.kind === "instrument");
    if (!keys || !empty) throw new Error("the session has a source and three lanes");
    server.apply({ type: "addCable", from: { node: keys.id, port: { type: "eventsOut" } }, to: { node: empty.id, port: { type: "eventsIn" } }, gain: null });
    const cable = server.snapshot.graph.edges.find((e) => e.kind === "events" && e.to.node === empty.id);
    if (cable?.kind !== "events") throw new Error("the cable that was drawn");
    const draw: Intent = { type: "addCable", from: cable.from, to: cable.to, gain: null };
    expect(takesOut(server.snapshot, draw, { nodes: [], edges: [cable.id] })).toEqual({ type: "removeEdge", edge: cable.id });
    // (A cable that is not there, and a cable that made a node as well, are not taken out by it.)
    expect(takesOut(server.snapshot, draw, { nodes: [], edges: [9_999] })).toBeNull();
    expect(takesOut(server.snapshot, draw, { nodes: [3], edges: [cable.id] })).toBeNull();
    expect(takesOut(server.snapshot, draw, { nodes: [], edges: [cable.id, 9_999] })).toBeNull();
    const add: Intent = { type: "addSource", source: { type: "device", name: "Seaboard" } };
    server.apply(add);
    const added = server.snapshot.graph.nodes.findLast((n) => n.kind === "source");
    if (!added) throw new Error("the source that was added");
    expect(takesOut(server.snapshot, add, { nodes: [added.id], edges: [] })).toEqual({ type: "removeNode", node: added.id });
  });
});
