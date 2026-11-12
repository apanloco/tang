import { describe, expect, test } from "vitest";

import type { Graph, Node } from "../protocol";
import { MASTER, sourcesFixture, splittersFixture, tracerSnapshot } from "./fixtures";
import { noteFromChosen, noteFromItems, noteToItems, sourceChosen, sourceItems } from "./noteItems";

// The session of the sources fixture (`testutil::NOTES`): the keys (7) and a device (8, Launchkey Mini MK4 37) are the sources; group 0 (Pad) with Reverb 1 on its bus, lane 2 in it (with Filter 3), lane 4
// (an Oscillator, as lane 2 is: the canvas numbers them) and an empty lane 5, and an LFO (6) that hears lane 2; a cable of notes (1) from the keys to lane 4.
const found = sourcesFixture.cases.find((entry) => entry.name === "wired");
if (!found) throw new Error("the fixture has the session of the sources");
const wired = found.snapshot.graph;

const device = (id: number, name: string): Node => ({ id, parent: null, name, kind: "source", source: { type: "device", name } });
const withDevices = (...names: string[]): Graph => ({ nodes: [...names.map((name, i) => device(100 + i, name)), ...tracerSnapshot.graph.nodes], edges: tracerSnapshot.graph.edges });

describe("the sources that a menu offers to add", () => {
  test("are the keys when the session has none, and nothing else while no input is open", () => {
    expect(sourceItems(tracerSnapshot.graph, [])).toEqual([{ id: "keys", label: "Keys" }]);
  });

  test("are not the keys when the session has them already, and nothing at all when no input is open either", () => {
    expect(sourceItems(wired, [])).toEqual([]);
    const withKeys: Graph = { nodes: [{ id: 100, parent: null, name: "Keys", kind: "source", source: { type: "keys" } }, ...tracerSnapshot.graph.nodes], edges: tracerSnapshot.graph.edges };
    expect(sourceItems(withKeys, [])).toEqual([]);
  });

  test("are not the keys when a device of the session is called Keys: that is a device, and the keys are not there", () => {
    expect(sourceItems(withDevices("Keys"), [])).toEqual([{ id: "keys", label: "Keys" }]);
  });

  test("are each open input, in the order they were opened, under a heading, after the keys", () => {
    expect(sourceItems(tracerSnapshot.graph, ["Seaboard RISE", "IAC Driver Bus 1"])).toEqual([
      { id: "keys", label: "Keys" },
      { id: "device:Seaboard RISE", label: "Seaboard RISE", group: "MIDI inputs" },
      { id: "device:IAC Driver Bus 1", label: "IAC Driver Bus 1", group: "MIDI inputs" },
    ]);
  });

  test("are not an input that a device of the session stands for: one that it is called as, and each that it is part of the name of, whatever the case", () => {
    const open = ["Launchkey Mini MK4 37 MIDI Out", "Launchkey Mini MK4 37 DAW Out", "Seaboard RISE", "IAC Driver Bus 1"];
    // (The device of the fixture is called Launchkey Mini MK4 37: both of its ports are it.)
    expect(sourceItems(wired, open).map((item) => item.label)).toEqual(["Seaboard RISE", "IAC Driver Bus 1"]);
    expect(sourceItems(withDevices("IAC Driver Bus 1"), open).map((item) => item.label)).toEqual(["Keys", ...open.slice(0, 3)]);
    expect(sourceItems(withDevices("launchkey"), open).map((item) => item.label)).toEqual(["Keys", "Seaboard RISE", "IAC Driver Bus 1"]);
    expect(sourceItems(withDevices("  SEABOARD  "), open).map((item) => item.label)).toEqual(["Keys", ...open.filter((name) => name !== "Seaboard RISE")]);
  });

  test("are an input that has Keys in its name, though the keys are a source of the session: the keys are no device, and stand for no input", () => {
    expect(sourceItems(wired, ["Keystation 61 MK3", "Komplete Kontrol Keys"]).map((item) => item.label)).toEqual(["Keystation 61 MK3", "Komplete Kontrol Keys"]);
  });

  test("are each input that no device of the session stands for, when the devices there are stand for other inputs, or are called by what no input is", () => {
    expect(sourceItems(withDevices("Lightpad", "Pianoteq"), ["Seaboard RISE"]).map((item) => item.label)).toEqual(["Keys", "Seaboard RISE"]);
    // (A device with a name that is blank stands for none, as the controller has it.)
    expect(sourceItems(withDevices(" "), ["Seaboard RISE"]).map((item) => item.label)).toEqual(["Keys", "Seaboard RISE"]);
  });

  test("are one for each name, though two inputs are called the same: a menu has one item for an id", () => {
    expect(sourceItems(wired, ["Seaboard RISE", "Seaboard RISE", "IAC Driver Bus 1"]).map((item) => item.id)).toEqual(["device:Seaboard RISE", "device:IAC Driver Bus 1"]);
  });

  test("give back the source that an item stands for, by its id: the keys, and a device by its whole name, whatever is in it", () => {
    expect(sourceChosen("keys")).toEqual({ type: "keys" });
    expect(sourceChosen("device:Seaboard RISE")).toEqual({ type: "device", name: "Seaboard RISE" });
    expect(sourceChosen("device:device:odd")).toEqual({ type: "device", name: "device:odd" });
    expect(sourceChosen("device:keys")).toEqual({ type: "device", name: "keys" });
    for (const item of sourceItems(tracerSnapshot.graph, ["Seaboard RISE", "Launchkey Mini MK4 37 MIDI Out"])) {
      const source = sourceChosen(item.id);
      expect(source.type === "keys" ? "keys" : `device:${source.name}`).toBe(item.id);
    }
  });
});

describe("the sources that a lane can be given notes from", () => {
  test("are those that have no cable of notes to it, by the id of the node, in the names of the canvas, in the order of the graph", () => {
    expect(noteFromItems(wired, 2)).toEqual([
      { id: "7", label: "Keys" },
      { id: "8", label: "Launchkey Mini MK4 37" },
    ]);
    // (Lane 4 has a cable from the keys already.)
    expect(noteFromItems(wired, 4)).toEqual([{ id: "8", label: "Launchkey Mini MK4 37" }]);
    expect(noteFromItems(wired, 5).map((item) => item.id)).toEqual(["7", "8"]);
  });

  test("are none for a node that is no lane: an effect, a modulator, a group, the mix, a source, or no node", () => {
    for (const node of [3, 6, 0, MASTER, 7, 77]) expect(noteFromItems(wired, node), String(node)).toEqual([]);
  });

  test("are none when the session has no source, and none left when each has a cable to the lane", () => {
    expect(noteFromItems(tracerSnapshot.graph, 0)).toEqual([]);
    const both: Graph = {
      nodes: wired.nodes,
      edges: [
        ...wired.edges,
        { kind: "events", id: 60, from: { node: 8, port: { type: "eventsOut" } }, to: { node: 4, port: { type: "eventsIn" } } },
      ],
    };
    expect(noteFromItems(both, 4)).toEqual([]);
  });
});

describe("the lanes that a source can send notes to", () => {
  test("are those that it has no cable of notes to, by the id of the node, in the names of the canvas, in the order of the graph", () => {
    // (Lanes 2 and 4 are both Oscillators, which the canvas numbers; lane 5 has no instrument.)
    expect(noteToItems(wired, 7)).toEqual([
      { id: "2", label: "Oscillator 1" },
      { id: "5", label: "(empty)" },
    ]);
    expect(noteToItems(wired, 8)).toEqual([
      { id: "2", label: "Oscillator 1" },
      { id: "4", label: "Oscillator 2" },
      { id: "5", label: "(empty)" },
    ]);
  });

  test("are none for a node that is no source: a lane, an effect, the mix, or no node", () => {
    for (const node of [2, 3, 6, 0, MASTER, 77]) expect(noteToItems(wired, node), String(node)).toEqual([]);
  });

  test("are not those that only hold effects: the canvas draws no card for such a lane, and has nothing to show a cable of notes to it by", () => {
    const filter = wired.nodes.find((node) => node.id === 3);
    if (filter?.kind !== "effect") throw new Error("the session has a filter in lane 2");
    // (Lane 5 holds a filter now, and nothing else: it is the filter that is on the canvas.)
    const holding: Graph = { nodes: [...wired.nodes, { ...filter, id: 90, parent: 5 }], edges: wired.edges };
    expect(noteToItems(holding, 7)).toEqual([{ id: "2", label: "Oscillator 1" }]);
    expect(noteToItems(holding, 8)).toEqual([
      { id: "2", label: "Oscillator 1" },
      { id: "4", label: "Oscillator 2" },
    ]);
  });
});

// The session of the fixture of splitters (`testutil::SPLITTERS`): the lanes are 0, 1 and 2, the keys 3, the device 4, the box (Bass) 5, the first splitter (Bass line, which picks the lowest note) 6 and the second (which picks the highest) 7. The cables
// of notes are the keys to 6, the box and the device to 7, the pick of 6 to the lane 0, its rest to the lane 1 and the pick of 7 to the lane 2.
describe("the menus of notes with note splitters", () => {
  const found = splittersFixture.cases.find((entry) => entry.name === "splitters");
  if (!found) throw new Error("the fixture has the session of the splitters");
  const splits = found.snapshot.graph;

  test("list a splitter once for each of its outputs, by what the first picks and the rest, in the order of the graph: the sources, the boxes, the splitters", () => {
    // (The lane 2 has a cable from the pick of the second splitter, and none from anything else.)
    expect(noteFromItems(splits, 2)).toEqual([
      { id: "3", label: "Keys" },
      { id: "4", label: "Launchkey Mini MK4 37" },
      { id: "5", label: "Bass" },
      { id: "6", label: "Bass line · lowest" },
      { id: "6:rest", label: "Bass line · rest" },
      { id: "7:rest", label: "Splitter · rest" },
    ]);
  });

  test("have the first output of a splitter that picks the highest note called so, and leave out an output that has a cable to the lane already", () => {
    const highest: Graph = { nodes: splits.nodes.map((n) => (n.kind === "splitter" && n.id === 6 ? { ...n, pick: "highest" } : n)), edges: splits.edges };
    // (The lane 1 has the rest of the first splitter: it is not listed; the pick of it is.)
    expect(noteFromItems(highest, 1).map((item) => item.label)).toEqual(["Keys", "Launchkey Mini MK4 37", "Bass", "Bass line · highest", "Splitter · highest", "Splitter · rest"]);
    expect(noteFromItems(highest, 1).map((item) => item.id)).toEqual(["3", "4", "5", "6", "7", "7:rest"]);
  });

  test("list the splitters among what takes the notes of a source or a box, and not among what takes a splitter's", () => {
    // (The keys have a cable to the first splitter already: the second takes theirs, and so does the box that has none from them.)
    expect(noteToItems(splits, 3).map((item) => item.id)).toEqual(["5", "7", "0", "1", "2"]);
    expect(noteToItems(splits, 5).map((item) => item.id)).toEqual(["6", "0", "1", "2"]);
    // (A splitter's takers are lanes, and which lanes follows the output: the pick has a cable to the lane 0 and the rest has one to the lane 1.)
    expect(noteToItems(splits, 6).map((item) => item.id)).toEqual(["1", "2"]);
    expect(noteToItems(splits, 6, "rest").map((item) => item.id)).toEqual(["0", "2"]);
    expect(noteToItems(splits, 7, "rest").map((item) => item.id)).toEqual(["0", "1", "2"]);
    expect(noteToItems(splits, 3, "rest")).toEqual([]);
  });

  test("have items that say the node and the output that they stand for: a node's id is its first output and a splitter's id with `:rest` after it is its second", () => {
    expect(noteFromChosen("12")).toEqual({ node: 12, output: "pick" });
    expect(noteFromChosen("6:rest")).toEqual({ node: 6, output: "rest" });
    expect(noteFromChosen("0")).toEqual({ node: 0, output: "pick" });
    for (const item of noteFromItems(splits, 2)) expect(noteFromChosen(item.id).node, item.id).toBe(Number.parseInt(item.id, 10));
  });
});
