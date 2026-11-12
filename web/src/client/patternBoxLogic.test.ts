import { describe, expect, test } from "vitest";

import { PATTERN_BOX_DEFAULT_LENGTH, PATTERN_BOX_LENGTH_RANGE, PATTERN_BOX_NOTE_LIMIT } from "../protocol";
import type { Graph, Node, PatternNote } from "../protocol";
import { patternBoxesFixture } from "./fixtures";
import { noteFromItems, noteToItems } from "./noteItems";
import { eventCableEnds, noteTakers, withHears } from "./noteWiring";
import { boxFieldBefore, clampLength, newBox, nextBoxName, patternTitle, recordsKeys, sameNotes, sanitize } from "./patternBoxes";
import type { PatternBoxNode } from "./patternBoxes";

const note = (start: number, length: number, pitch: number, velocity: number): PatternNote => ({ start, length, note: pitch, velocity });

describe("the length of a pattern box", () => {
  test("is held to the shortest and the longest loop there can be, and a number that is none is the shortest", () => {
    const [low, high] = PATTERN_BOX_LENGTH_RANGE;
    expect([low, high]).toEqual([0.25, 256]);
    for (const [asked, held] of [
      [4, 4],
      [low, low],
      [high, high],
      [0.2499, low],
      [256.001, high],
      [0, low],
      [-3, low],
      [1000, high],
      [Number.POSITIVE_INFINITY, low],
      [Number.NEGATIVE_INFINITY, low],
      [Number.NaN, low],
    ] as const) {
      expect(clampLength(asked), String(asked)).toBe(held);
    }
  });
});

describe("the notes of a pattern box", () => {
  test("are held as the engine holds them: what is not a note is left out and each is brought into range", () => {
    const kept = sanitize([
      note(1, 1, 60, 100),
      note(Number.NaN, 1, 60, 100),
      note(1, Number.POSITIVE_INFINITY, 60, 100),
      note(1, 1, 128, 100),
      note(1, 1, -1, 100),
      note(1, 1, 60.5, 100),
      note(-2, -1, 61, 0),
      note(2, 1, 62, 255),
    ]);
    expect(kept).toEqual([note(0, 0, 61, 1), note(1, 1, 60, 100), note(2, 1, 62, 127)]);
    // (A pitch of 0 and one of 127 are notes, and so are a velocity of 1 and one of 127.)
    expect(sanitize([note(0, 1, 0, 1), note(0, 1, 127, 127)])).toEqual([note(0, 1, 0, 1), note(0, 1, 127, 127)]);
  });

  test("are in the order of their starts, then their pitches, their lengths and their velocities, and two notes that are one are two", () => {
    const sorted = sanitize([note(1, 1, 60, 100), note(0, 2, 64, 90), note(0, 1, 64, 90), note(0, 1, 62, 90), note(0, 1, 62, 80), note(0, 1, 62, 80)]);
    expect(sorted).toEqual([note(0, 1, 62, 80), note(0, 1, 62, 80), note(0, 1, 62, 90), note(0, 1, 64, 90), note(0, 2, 64, 90), note(1, 1, 60, 100)]);
  });

  test("are the first that there is room for, taken before they are put in order, and there is room for 4096", () => {
    expect(PATTERN_BOX_NOTE_LIMIT).toBe(4096);
    // (A late note that comes first in the list is kept, and an early one that comes after the 4096th is not.)
    const many = [note(10_000, 1, 60, 100), ...Array.from({ length: PATTERN_BOX_NOTE_LIMIT - 1 }, (_, i) => note(i, 1, 61, 100)), note(0, 1, 62, 100)];
    const kept = sanitize(many);
    expect(kept.length).toBe(PATTERN_BOX_NOTE_LIMIT);
    expect(kept.some((each) => each.note === 62)).toBe(false);
    expect(kept.at(-1)).toEqual(note(10_000, 1, 60, 100));
    // (What is not a note does not take a place.)
    expect(sanitize([...Array.from({ length: 5 }, () => note(Number.NaN, 1, 60, 100)), ...Array.from({ length: PATTERN_BOX_NOTE_LIMIT }, (_, i) => note(i, 1, 60, 100))]).length).toBe(PATTERN_BOX_NOTE_LIMIT);
  });

  test("are the same when each is the other's, in the same place", () => {
    expect(sameNotes([], [])).toBe(true);
    expect(sameNotes([note(0, 1, 60, 100)], [note(0, 1, 60, 100)])).toBe(true);
    for (const other of [note(1, 1, 60, 100), note(0, 2, 60, 100), note(0, 1, 61, 100), note(0, 1, 60, 99)]) {
      expect(sameNotes([note(0, 1, 60, 100)], [other]), JSON.stringify(other)).toBe(false);
    }
    expect(sameNotes([note(0, 1, 60, 100)], [])).toBe(false);
    expect(sameNotes([], [note(0, 1, 60, 100)])).toBe(false);
    expect(sameNotes([note(0, 1, 60, 100), note(1, 1, 61, 100)], [note(0, 1, 60, 100), note(1, 1, 62, 100)])).toBe(false);
  });
});

describe("a pattern box's name", () => {
  test("is the text without the spaces round it, `Pattern` for none, and refused when it is too long or has a control character in it", () => {
    expect(patternTitle("  Lead  ")).toEqual({ name: "Lead" });
    expect(patternTitle(" Lead ")).toEqual({ name: "Lead" });
    expect(patternTitle("")).toEqual({ name: "Pattern" });
    expect(patternTitle("   ")).toEqual({ name: "Pattern" });
    expect(patternTitle("x".repeat(64))).toEqual({ name: "x".repeat(64) });
    expect(patternTitle("x".repeat(65))).toEqual({ error: "a name is at most 64 characters" });
    expect(patternTitle("a\u0001b")).toEqual({ error: "a name has no control characters" });
  });

  test("is `Pattern N` for the lowest N from 1 that no box is called, whatever else is called so", () => {
    const named = (...names: string[]): Node[] => names.map((name, i) => newBox(i, name));
    expect(nextBoxName([])).toBe("Pattern 1");
    expect(nextBoxName(named("Pattern 1"))).toBe("Pattern 2");
    expect(nextBoxName(named("Pattern 1", "Pattern 2", "Pattern 4"))).toBe("Pattern 3");
    expect(nextBoxName(named("Pattern", "Bass"))).toBe("Pattern 1");
    // (What is no box is not counted: a lane may be called what it likes.)
    const lane: Node = { id: 9, parent: null, name: "Pattern 1", kind: "group", volume: 1, pan: 0, muted: false, solo: false, route: "out" } as unknown as Node;
    expect(nextBoxName([lane])).toBe("Pattern 1");
  });
});

describe("a pattern box that is new", () => {
  test("is empty, as long as a loop is by default, goes round on its own clock, overwrites, clicks and does nothing", () => {
    expect(PATTERN_BOX_DEFAULT_LENGTH).toBe(4);
    expect(newBox(7, "Pattern 1")).toEqual({ id: 7, parent: null, name: "Pattern 1", kind: "pattern", looping: true, sync: "own", overdub: false, click: true, lengthBeats: 4, notes: [], status: "stopped" });
  });
});

describe("the step that a setting of a pattern box makes", () => {
  const bass: PatternBoxNode = { ...newBox(4, "Bass"), lengthBeats: 8, overdub: true, notes: [note(0, 1, 36, 100)] };

  test("is in the words of the setting, goes back to the setting as the box has it, and is none when the box has what is asked for", () => {
    const rows: [Parameters<typeof boxFieldBefore>[1], string, Parameters<typeof boxFieldBefore>[1], boolean][] = [
      [{ type: "name", name: "Lead" }, "rename Bass", { type: "name", name: "Bass" }, false],
      [{ type: "name", name: "  Bass " }, "rename Bass", { type: "name", name: "Bass" }, true],
      [{ type: "name", name: "" }, "rename Bass", { type: "name", name: "Bass" }, false],
      [{ type: "name", name: "x".repeat(65) }, "rename Bass", { type: "name", name: "Bass" }, false],
      [{ type: "lengthBeats", beats: 4 }, "set the length of Bass", { type: "lengthBeats", beats: 8 }, false],
      [{ type: "lengthBeats", beats: 8 }, "set the length of Bass", { type: "lengthBeats", beats: 8 }, true],
      [{ type: "lengthBeats", beats: 8.1 }, "set the length of Bass", { type: "lengthBeats", beats: 8 }, false],
      [{ type: "looping", on: false }, "turn looping off for Bass", { type: "looping", on: true }, false],
      [{ type: "looping", on: true }, "turn looping on for Bass", { type: "looping", on: true }, true],
      [{ type: "sync", sync: "host" }, "let Bass go by the host's beat", { type: "sync", sync: "own" }, false],
      [{ type: "sync", sync: "own" }, "let Bass go by its own clock", { type: "sync", sync: "own" }, true],
      [{ type: "overdub", on: false }, "turn overdub off for Bass", { type: "overdub", on: true }, false],
      [{ type: "overdub", on: true }, "turn overdub on for Bass", { type: "overdub", on: true }, true],
      [{ type: "click", on: false }, "turn the click off for Bass", { type: "click", on: true }, false],
      [{ type: "click", on: true }, "turn the click on for Bass", { type: "click", on: true }, true],
    ];
    for (const [field, label, undo, same] of rows) {
      expect(boxFieldBefore(bass, field), JSON.stringify(field)).toEqual({ label, undo, same });
    }
  });

  test("is none for a length that comes to the length the box has, though it is another number", () => {
    const long: PatternBoxNode = { ...bass, lengthBeats: 256 };
    expect(boxFieldBefore(long, { type: "lengthBeats", beats: 300 }).same).toBe(true);
    const short: PatternBoxNode = { ...bass, lengthBeats: 0.25 };
    expect(boxFieldBefore(short, { type: "lengthBeats", beats: 0.1 }).same).toBe(true);
    expect(boxFieldBefore(short, { type: "lengthBeats", beats: Number.NaN }).same).toBe(true);
  });
});

// The session of the fixture: the keys (2) and a device (3), two boxes, Bass (4) and a plain one (5), two lanes (0, 1) and the cables of notes from the keys to Bass, from Bass to the first lane and from the second box to the second.
const boxed = patternBoxesFixture.cases.find((each) => each.name === "boxes")?.snapshot.graph;
if (!boxed) throw new Error("the fixture has a session with boxes");
const [KEYS, DEVICE, BASS, PLAIN, LANE0, LANE1] = [2, 3, 4, 5, 0, 1];
const cable = (graph: Graph, from: number, to: number): Graph => ({
  ...graph,
  edges: [...graph.edges, { from: { node: from, port: { type: "eventsOut" } }, to: { node: to, port: { type: "eventsIn" } }, kind: "events", id: 100 + graph.edges.length }],
});

describe("the places that a cable of notes can be drawn to", () => {
  test("from a source are the lanes and the boxes that have no cable from it yet, in the order of the graph", () => {
    expect(noteTakers(boxed, KEYS)).toEqual([PLAIN, LANE0, LANE1]);
    expect(noteTakers(boxed, DEVICE)).toEqual([BASS, PLAIN, LANE0, LANE1]);
  });

  test("from a box are the lanes that have none from it yet, and no box", () => {
    expect(noteTakers(boxed, BASS)).toEqual([LANE1]);
    expect(noteTakers(boxed, PLAIN)).toEqual([LANE0]);
    expect(noteTakers(cable(boxed, BASS, LANE1), BASS)).toEqual([]);
  });

  test("from what is neither are none, and a lane that takes no notes is not one", () => {
    expect(noteTakers(boxed, LANE0)).toEqual([]);
    expect(noteTakers(boxed, 4294967295)).toEqual([]);
    expect(noteTakers(boxed, 9999)).toEqual([]);
    const silent: Graph = { ...boxed, nodes: boxed.nodes.map((n) => (n.id === LANE1 && n.kind === "instrument" ? { ...n, plugin: n.plugin === null ? null : { ...n.plugin, takesNotes: false } } : n)) };
    expect(noteTakers(silent, BASS)).toEqual([]);
    expect(noteTakers(silent, KEYS)).toEqual([PLAIN, LANE0]);
  });

  test("are the ends that eventCableEnds gives a cable from a source or a box to a lane, and from a source to a box", () => {
    const ends = (from: number, to: number): unknown => eventCableEnds(boxed, { node: from, port: { type: "eventsOut" } }, { node: to, port: { type: "eventsIn" } });
    expect(ends(KEYS, PLAIN)).toEqual({ source: KEYS, output: "pick", lane: PLAIN });
    expect(ends(BASS, LANE1)).toEqual({ source: BASS, output: "pick", lane: LANE1 });
    expect(ends(DEVICE, LANE0)).toEqual({ source: DEVICE, output: "pick", lane: LANE0 });
    expect(ends(BASS, PLAIN)).toMatchObject({ error: { code: "invalid" } });
    expect(ends(LANE0, BASS)).toMatchObject({ error: { code: "invalid", message: "only a source of notes has an output of events" } });
  });
});

describe("the links that a source has to what hears every source", () => {
  test("go to each lane and each box that has no cable of notes ending at it: the lanes first, then the boxes", () => {
    const hears = (graph: Graph): string[] => withHears(graph).edges.flatMap((e) => (e.kind === "hears" ? [`${String(e.from.node)}>${String(e.to.node)}`] : []));
    // (Bass has a cable from the keys, the lanes have cables from boxes: only the plain box has none.)
    expect(hears(boxed)).toEqual(["2>5", "3>5"]);
    const bare: Graph = { ...boxed, edges: boxed.edges.filter((e) => e.kind !== "events") };
    expect(hears(bare)).toEqual(["2>0", "2>1", "2>4", "2>5", "3>0", "3>1", "3>4", "3>5"]);
    // (A box that no cable of notes ends at is heard by no lane, and its link is not drawn from it.)
    expect(hears({ ...bare, nodes: bare.nodes.filter((n) => n.kind !== "source") })).toEqual([]);
  });

  test("are those of the graph, and what the graph has already is left out first", () => {
    const once = withHears({ ...boxed, edges: boxed.edges.filter((e) => e.kind !== "events") });
    const twice = withHears(once);
    expect(twice.edges).toEqual(once.edges);
  });
});

describe("the keys that a pattern box records", () => {
  test("are every input when no cable of notes ends at the box, whatever cables there are elsewhere, and else the sources that the cables that end at it start at", () => {
    // (Bass has a cable from the keys, the plain box has none: the cables of the lanes are from the boxes.)
    expect(recordsKeys(boxed, BASS)).toBe(true);
    expect(recordsKeys(boxed, PLAIN)).toBe(true);
    // A cable from the device in place of the keys: Bass records nothing of the keys here, and the plain box, which has none that ends at it, still records them, though the only cable from a source is not to it.
    const fromDevice: Graph = { ...boxed, edges: boxed.edges.map((e) => (e.kind === "events" && e.to.node === BASS ? { ...e, from: { ...e.from, node: DEVICE } } : e)) };
    expect(recordsKeys(fromDevice, BASS)).toBe(false);
    expect(recordsKeys(fromDevice, PLAIN)).toBe(true);
    // A cable from the keys to the plain box and one from the device: it records the keys.
    expect(recordsKeys(cable(cable(fromDevice, DEVICE, PLAIN), KEYS, PLAIN), PLAIN)).toBe(true);
    expect(recordsKeys(cable(fromDevice, DEVICE, PLAIN), PLAIN)).toBe(false);
  });
});

describe("the menus of cables of notes", () => {
  test("of a box that takes notes list the sources that it has no cable from, and a box that sends them lists the lanes", () => {
    const ids = (items: { id: string }[]): string[] => items.map((item) => item.id);
    expect(ids(noteFromItems(boxed, BASS))).toEqual(["3"]);
    expect(ids(noteFromItems(boxed, PLAIN))).toEqual(["2", "3"]);
    expect(ids(noteToItems(boxed, BASS))).toEqual(["1"]);
    expect(ids(noteToItems(boxed, PLAIN))).toEqual(["0"]);
  });

  test("of a lane list the sources and the boxes that it has no cable from, and of a source the boxes and the lanes", () => {
    const ids = (items: { id: string }[]): string[] => items.map((item) => item.id);
    expect(ids(noteFromItems(boxed, LANE0))).toEqual(["2", "3", "5"]);
    expect(ids(noteFromItems(boxed, LANE1))).toEqual(["2", "3", "4"]);
    expect(ids(noteToItems(boxed, KEYS))).toEqual(["5", "0", "1"]);
  });
});
