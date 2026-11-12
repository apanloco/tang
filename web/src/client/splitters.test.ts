import { describe, expect, test } from "vitest";

import type { Intent, Snapshot, StampedEvent } from "../protocol";
import { MASTER, shapeOf, splittersFixture } from "./fixtures";
import type { Shape } from "./fixtures";
import { planOf } from "./history";
import { MockServer } from "./mock";
import { nextSplitterName, newSplitter, splitterFieldBefore, splitterTitle } from "./splitters";

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

const LISTS = ["nodes", "audio", "cables", "sends", "routes", "sources", "patterns", "splitters", "notes", "hears", "order", "meters", "modulation"] as const;
/** A value as text, the keys of an object in order of their names (the controller's JSON has them so, and the page's events have them in the order they were made in). */
const canonical = (value: unknown): string => JSON.stringify(value, (_key, part: unknown) => (part !== null && typeof part === "object" && !Array.isArray(part) ? Object.fromEntries(Object.entries(part).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))) : part));
const same = (a: unknown, b: unknown): boolean => canonical(a) === canonical(b);
const listOf = (shape: Shape, list: (typeof LISTS)[number]): string[] => shape[list] ?? [];

const fixtureCase = (name: string): (typeof splittersFixture.cases)[number] => {
  const found = splittersFixture.cases.find((entry) => entry.name === name);
  if (!found) throw new Error(`the fixture has no case called ${name}`);
  return found;
};

// The sessions of the fixture (`fixtures/splitters.json`, `testutil::SPLITTERS`, a session of two lanes, one with two splitters in a session of the keys, a device, a box and three lanes, one with no node of notes and one of sixteen splitters):
// "splitters" has the keys and a device, a box (Bass), two splitters (Bass line, which picks the lowest note and takes the keys, and one that picks the highest and takes the box and the device) and three lanes with cables of notes from the first
// splitter's pick to the first lane and its rest to the second, and from the second splitter's pick to the third; "bare" has two lanes and nothing else; "numbered" has "Splitter 1" and "Splitter 3" and one that has no name; "full" has the sixteen
// splitters that a session can have.
describe("the mock engine's note splitters", () => {
  test("are what the controller makes of each intent of its fixture: its words when it refuses one, and else the events it made, whether it changed the structure and everything the session comes to", () => {
    const wrong: string[] = [];
    for (const { name, snapshot, rows } of splittersFixture.cases) {
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
    const rows = fixtureCase("splitters").rows;
    const tried = [
      ["addSplitter", 1, 0],
      ["setSplitterField", 12, 14],
      ["addCable", 11, 40],
      ["removeEdge", 6, 1],
      ["removeNode", 8, 2],
      ["setNodeUi", 4, 0],
    ] as const;
    for (const [type, done, refused] of tried) {
      expect(rows.filter((row) => row.intent.type === type && !("error" in row)).length, `${type} done`).toBeGreaterThanOrEqual(done);
      expect(rows.filter((row) => row.intent.type === type && "error" in row).length, `${type} refused`).toBeGreaterThanOrEqual(refused);
    }
    // (Every ask that names a node and is not for a splitter is refused, each of its kinds, when it names a splitter.)
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
      "setPatternBoxField",
      "clearPatternBox",
      "setPatternBoxNotes",
      "patternBoxTransport",
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
    const words = new Set(splittersFixture.cases.flatMap((each) => each.rows.flatMap((row) => ("error" in row ? [row.error] : []))));
    for (const sentence of [
      "a name is at most 64 characters",
      "a name has no control characters",
      "that node is not a note splitter",
      "there is no node 900",
      "there is a cable between those two already",
      "a note splitter cannot take the notes of another splitter: it takes those of a source or a pattern box",
      "a note splitter cannot send its notes to a pattern box yet: it sends them to a lane",
      "only a note splitter has a second output of events",
      "only a source of notes has an output of events",
      "the mix has no input of events",
      "the mix has no output of events",
      "a note splitter has no input of audio: it has an input of events",
      "a note splitter has no output of audio: it has two outputs of events",
      "a cable of notes has no level: it carries notes, which are heard or not",
      "a note splitter is not collapsed into a module: a module holds lanes and modulators",
      "a session has at most 16 note splitters",
    ]) {
      expect(words.has(sentence), sentence).toBe(true);
    }
    expect(fixtureCase("bare").rows.length).toBeGreaterThanOrEqual(5);
    expect(fixtureCase("numbered").rows.length).toBeGreaterThanOrEqual(1);
    expect(fixtureCase("full").rows.length).toBeGreaterThanOrEqual(1);
  });

  // What the fixture's rows do not say: a splitter that is added is in the order that the graph has (after the boxes, and before everything else), under the next node that is handed out.
  test("go after the boxes, the first splitter of a session that has none before the lanes, under the next node there is", () => {
    const { snapshot } = fixtureCase("bare");
    const added = made(snapshot, { type: "addSplitter" });
    if ("refused" in added) throw new Error(added.refused);
    expect(added.after.graph.nodes.map((n) => n.name)).toEqual(["Splitter 1", ...snapshot.graph.nodes.map((n) => n.name)]);
    expect(added.after.graph.nodes[0]).toEqual({ id: Math.max(...snapshot.graph.nodes.filter((n) => n.kind !== "master").map((n) => n.id)) + 1, parent: null, name: "Splitter 1", kind: "splitter", pick: "lowest" });
  });

  // What the controller refuses before it looks at a library (the fixture's controller has none): a splitter is not made into a module, whatever way a module is asked for.
  test("are not part of a module that is saved from the chosen nodes: the words are the controller's", () => {
    const { snapshot } = fixtureCase("splitters");
    const first = snapshot.graph.nodes.find((n) => n.kind === "splitter");
    const lane = snapshot.graph.nodes.find((n) => n.kind === "instrument");
    if (!first || !lane) throw new Error("the fixture has a splitter and a lane");
    const saved = (nodes: number[]): string => {
      const result = made(snapshot, { type: "saveSelectionAsModule", nodes, name: "Mine" });
      return "refused" in result ? result.refused : "saved";
    };
    const words = "a note splitter is not part of a module: a module is made of effects and instruments, and the modulators that go with them";
    expect(saved([first.id])).toBe(words);
    expect(saved([lane.id, first.id])).toBe(words);
    expect(saved([first.id, lane.id])).toBe(words);
  });
});

describe("a note splitter as the page makes it", () => {
  test("is new as the controller makes one: it picks the lowest note, and has no place of its own", () => {
    expect(newSplitter(7, "Splitter 1")).toEqual({ id: 7, parent: null, name: "Splitter 1", kind: "splitter", pick: "lowest" });
  });

  test("is called Splitter N for the lowest N that no splitter is called, whatever else is called so", () => {
    const named = (...names: string[]) => names.map((name, i) => newSplitter(i, name));
    expect(nextSplitterName([])).toBe("Splitter 1");
    expect(nextSplitterName(named("Splitter 1"))).toBe("Splitter 2");
    expect(nextSplitterName(named("Splitter 1", "Splitter 3"))).toBe("Splitter 2");
    expect(nextSplitterName(named("Splitter", "Bass line"))).toBe("Splitter 1");
    expect(nextSplitterName([{ id: 9, parent: null, name: "Splitter 1", kind: "source", source: { type: "keys" } }])).toBe("Splitter 1");
  });

  test("is called what it is told to be, without the spaces round it, and Splitter for nothing, by a group's rule", () => {
    expect(splitterTitle("  Bass  ")).toEqual({ name: "Bass" });
    expect(splitterTitle("")).toEqual({ name: "Splitter" });
    expect(splitterTitle("   ")).toEqual({ name: "Splitter" });
    expect(splitterTitle("x".repeat(65))).toEqual({ error: "a name is at most 64 characters" });
    expect(splitterTitle("a\u0001b")).toEqual({ error: "a name has no control characters" });
  });

  test("has its settings put in words for the history: what undo sets, and whether the splitter has it already", () => {
    const splitter = newSplitter(7, "Bass line");
    expect(splitterFieldBefore(splitter, { type: "name", name: "Lead" })).toEqual({ label: "rename Bass line", undo: { type: "name", name: "Bass line" }, same: false });
    expect(splitterFieldBefore(splitter, { type: "name", name: "  Bass line " }).same).toBe(true);
    expect(splitterFieldBefore(splitter, { type: "pick", pick: "highest" })).toEqual({ label: "let Bass line pick the highest note", undo: { type: "pick", pick: "lowest" }, same: false });
    expect(splitterFieldBefore({ ...splitter, pick: "highest" }, { type: "pick", pick: "lowest" })).toEqual({ label: "let Bass line pick the lowest note", undo: { type: "pick", pick: "highest" }, same: false });
    expect(splitterFieldBefore(splitter, { type: "pick", pick: "lowest" }).same).toBe(true);
  });
});

// What the history makes of a splitter's intents before they are done (`planOf`, as `plan_history` of the controller): the mock refuses what is refused before it looks at a plan, so the fixture's scripts do not reach the plan of an intent that
// is refused, and these do.
describe("the history's plan for the intents of a note splitter", () => {
  const { snapshot } = fixtureCase("splitters");
  const plan = (intent: Intent) => planOf(snapshot, intent, 100, []);

  test("is no step for a setting of a node that is no splitter, or is not there: the controller refuses it, and the plan is not looked at", () => {
    for (const node of [0, 3, 4, 5, 9_999, MASTER]) {
      expect(plan({ type: "setSplitterField", node, field: { type: "pick", pick: "highest" } }), String(node)).toBe("nothing");
      expect(plan({ type: "setSplitterField", node, field: { type: "name", name: "x" } }), String(node)).toBe("nothing");
    }
  });

  test("is an add for a splitter that is added, and a step for a setting in the words of the splitter's name, which puts back what the splitter has", () => {
    expect(plan({ type: "addSplitter" })).toEqual({ added: { label: "add a splitter", intent: { type: "addSplitter" }, at: 100 } });
    expect(plan({ type: "setSplitterField", node: 6, field: { type: "pick", pick: "highest" } })).toMatchObject({
      step: { label: "let Bass line pick the highest note", undo: [{ type: "setSplitterField", node: 6, field: { type: "pick", pick: "lowest" } }], redo: [{ type: "setSplitterField", node: 6, field: { type: "pick", pick: "highest" } }], key: "splitterfield:6:pick" },
    });
    expect(plan({ type: "setSplitterField", node: 7, field: { type: "name", name: " Pad " } })).toMatchObject({
      step: { label: "rename Splitter", undo: [{ type: "setSplitterField", node: 7, field: { type: "name", name: "Splitter" } }], key: "splitterfield:7:name" },
    });
  });

  test("is no step for a setting that the splitter has already, and a removal for a splitter that goes, in the words of its name", () => {
    expect(plan({ type: "setSplitterField", node: 6, field: { type: "pick", pick: "lowest" } })).toBe("nothing");
    expect(plan({ type: "setSplitterField", node: 6, field: { type: "name", name: "  Bass line " } })).toBe("nothing");
    expect(plan({ type: "removeNode", node: 6 })).toEqual({ removed: { label: "remove Bass line", intent: { type: "removeNode", node: 6 }, at: 100 } });
  });
});
