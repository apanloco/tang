import { describe, expect, test } from "vitest";

import type { Intent, Snapshot, StampedEvent } from "../protocol";
import { patternBoxesFixture, shapeOf } from "./fixtures";
import type { Shape } from "./fixtures";
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

const LISTS = ["nodes", "audio", "cables", "sends", "routes", "sources", "patterns", "notes", "hears", "order", "meters", "modulation"] as const;
/** A value as text, the keys of an object in order of their names (the controller's JSON has them so, and the page's events have them in the order they were made in). */
const canonical = (value: unknown): string => JSON.stringify(value, (_key, part: unknown) => (part !== null && typeof part === "object" && !Array.isArray(part) ? Object.fromEntries(Object.entries(part).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))) : part));
const same = (a: unknown, b: unknown): boolean => canonical(a) === canonical(b);
const listOf = (shape: Shape, list: (typeof LISTS)[number]): string[] => shape[list] ?? [];

const fixtureCase = (name: string): (typeof patternBoxesFixture.cases)[number] => {
  const found = patternBoxesFixture.cases.find((entry) => entry.name === name);
  if (!found) throw new Error(`the fixture has no case called ${name}`);
  return found;
};

// The sessions of the fixture (`fixtures/patternBoxes.json`, `testutil::PATTERNS`, a session of two lanes and one of sixteen boxes): "boxes" has the keys and a device, two boxes (Bass, 8 beats, overdubbing, with two notes, and a plain
// one), two lanes, and cables of notes from the keys to Bass, from Bass to the first lane and from the plain box to the second; "bare" has no source and no box; "full" has the sixteen boxes that a session can have.
describe("the mock engine's pattern boxes", () => {
  test("are what the controller makes of each intent of its fixture: its words when it refuses one, and else the events it made, whether it changed the structure and everything the session comes to", () => {
    const wrong: string[] = [];
    for (const { name, snapshot, rows } of patternBoxesFixture.cases) {
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
    const rows = fixtureCase("boxes").rows;
    const tried = [
      ["addPattern", 1, 0],
      ["setPatternBoxField", 30, 10],
      ["clearPatternBox", 2, 2],
      ["setPatternBoxNotes", 5, 2],
      ["patternBoxTransport", 8, 8],
      ["addCable", 6, 18],
      ["removeEdge", 3, 1],
      ["removeNode", 6, 2],
      ["setNodeUi", 4, 0],
    ] as const;
    for (const [type, done, refused] of tried) {
      expect(rows.filter((row) => row.intent.type === type && !("error" in row)).length, `${type} done`).toBeGreaterThanOrEqual(done);
      expect(rows.filter((row) => row.intent.type === type && "error" in row).length, `${type} refused`).toBeGreaterThanOrEqual(refused);
    }
    // (Every ask that names a node and is not for a box is refused, each of its kinds, when it names a box.)
    for (const type of [
      "setParam",
      "setGain",
      "loadPreset",
      "setModulatorField",
      "addFreeModulator",
      "addModEdge",
      "modulate",
      "addEffect",
      "replaceInstrument",
      "clearInstrument",
      "setRange",
      "setMute",
      "setSolo",
      "setRoute",
      "setModulatorEar",
      "setFeed",
      "setRecording",
      "clearPattern",
      "setPatternField",
      "setPinned",
      "setSteps",
      "addGroup",
      "setLaneGroup",
      "renameGroup",
      "renameMacro",
      "exposeMacro",
      "collapseIntoModule",
      "ungroupModule",
      "moveEffect",
      "openEditor",
    ]) {
      const asked = rows.filter((row) => row.intent.type === type);
      expect(asked.length, `${type} asked`).toBeGreaterThanOrEqual(1);
      expect(asked.every((row) => "error" in row), `${type} refused`).toBe(true);
    }
    const words = new Set(patternBoxesFixture.cases.flatMap((each) => each.rows.flatMap((row) => ("error" in row ? [row.error] : []))));
    for (const sentence of [
      "a name is at most 64 characters",
      "a name has no control characters",
      "that node is not a pattern box",
      "there is no node 900",
      "there is a cable between those two already",
      "a pattern box cannot send its notes to another box yet: it sends them to a lane",
      "only a source of notes has an output of events",
      "the mix has no input of events",
      "the mix has no output of events",
      "a pattern box has no input of audio: it has an input of events",
      "a pattern box has no output of audio: it has an output of events",
      "a cable of notes has no level: it carries notes, which are heard or not",
      "a pattern box is not collapsed into a module: a module holds lanes and modulators",
      "a session has at most 16 pattern boxes",
    ]) {
      expect(words.has(sentence), sentence).toBe(true);
    }
    expect(fixtureCase("bare").rows.length).toBeGreaterThanOrEqual(4);
    expect(fixtureCase("full").rows.length).toBeGreaterThanOrEqual(1);
  });

  // What the fixture's rows do not say: a box that is told to do something uses a revision, and tells the clients nothing else (`pattern_box_transport_intent`: `Applied::of(rev, [])`); what the box does then is the engine's, and comes as a
  // snapshot of its own when it is something else than it was (`BoxNotification::Status`). A box that is stopped, told to stop or to finish recording, does nothing: only the revision is used.
  test("use a revision for what a box is told to do, and tell no event and no change of the structure", () => {
    const server = new MockServer({ snapshot: fixtureCase("boxes").snapshot });
    const rev = server.snapshot.rev;
    for (const action of ["stop", "finishRecording"] as const) {
      const told = server.apply({ type: "patternBoxTransport", node: 4, action });
      expect(told, action).toEqual({ events: [], restructured: false });
    }
    expect(server.snapshot.rev).toBe(rev + 2);
    expect(server.history).toEqual({ undo: null, redo: null });
    // (A box that is not there is refused, and uses none.)
    expect(() => server.apply({ type: "patternBoxTransport", node: 900, action: "play" })).toThrow("there is no node 900");
    expect(server.snapshot.rev).toBe(rev + 2);
  });

  // What the fixture cannot hold, since the wire has no number that is not finite: the controller's 32-bit numbers are infinite at 1e39, and the words for it are the controller's (`set_pattern_box_field_intent`).
  test("refuse a length that is infinite in the controller's 32 bits, in its words, and keep one that is large and finite", () => {
    const { snapshot } = fixtureCase("boxes");
    const lengthOf = (beats: number): string => {
      const result = made(snapshot, { type: "setPatternBoxField", node: 4, field: { type: "lengthBeats", beats } });
      if ("refused" in result) return result.refused;
      const box = result.after.graph.nodes.find((n) => n.id === 4);
      return box?.kind === "pattern" ? String(box.lengthBeats) : "no box";
    };
    expect(lengthOf(1e39)).toBe("inf is not a length in beats");
    expect(lengthOf(-1e39)).toBe("-inf is not a length in beats");
    expect(lengthOf(1e38)).toBe("256");
    expect(lengthOf(-1e38)).toBe("0.25");
  });

  // What the controller refuses before it looks at a library (the fixture's controller has none): a pattern box is not made into a module, whatever way a module is asked for.
  test("are not part of a module that is saved from the chosen nodes: the words are the controller's", () => {
    const { snapshot } = fixtureCase("boxes");
    const saved = (nodes: number[]): string => {
      const result = made(snapshot, { type: "saveSelectionAsModule", nodes, name: "Mine" });
      return "refused" in result ? result.refused : "saved";
    };
    const words = "a pattern box is not part of a module: a module is made of effects and instruments, and the modulators that go with them";
    expect(saved([4])).toBe(words);
    expect(saved([5, 0])).toBe(words);
    expect(saved([0, 4])).toBe(words);
    // (A source is refused in its own words, and what is chosen with a box that is not one is not looked at first.)
    expect(saved([2])).toBe("a source of notes is not part of a module: a module is made of effects and instruments, and the modulators that go with them");
  });
});
