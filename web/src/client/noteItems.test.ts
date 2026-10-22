import { describe, expect, test } from "vitest";

import type { Graph, Node } from "../protocol";
import { MASTER, sourcesFixture, tracerSnapshot } from "./fixtures";
import { noteFromItems, noteToItems, sourceChosen, sourceItems } from "./noteItems";

// The session of the sources fixture (`testutil::NOTES`): the keys (7) and a device (8, Launchkey Mini MK4 37) are the sources; group 0 (Pad) with Reverb 1 on its bus, lane 2 in it (with Filter 3 and an LFO 4), lane 5
// (an Oscillator, as lane 2 is: the canvas numbers them) and an empty lane 6; a cable of notes (1) from the keys to lane 5.
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
    // (Lane 5 has a cable from the keys already.)
    expect(noteFromItems(wired, 5)).toEqual([{ id: "8", label: "Launchkey Mini MK4 37" }]);
    expect(noteFromItems(wired, 6).map((item) => item.id)).toEqual(["7", "8"]);
  });

  test("are none for a node that is no lane: an effect, a modulator, a group, the mix, a source, or no node", () => {
    for (const node of [3, 4, 0, MASTER, 7, 77]) expect(noteFromItems(wired, node), String(node)).toEqual([]);
  });

  test("are none when the session has no source, and none left when each has a cable to the lane", () => {
    expect(noteFromItems(tracerSnapshot.graph, 0)).toEqual([]);
    const both: Graph = {
      nodes: wired.nodes,
      edges: [
        ...wired.edges,
        { kind: "events", id: 60, from: { node: 8, port: { type: "eventsOut" } }, to: { node: 5, port: { type: "eventsIn" } } },
      ],
    };
    expect(noteFromItems(both, 5)).toEqual([]);
  });
});

describe("the lanes that a source can send notes to", () => {
  test("are those that it has no cable of notes to, by the id of the node, in the names of the canvas, in the order of the graph", () => {
    // (Lanes 2 and 5 are both Oscillators, which the canvas numbers; lane 6 has no instrument.)
    expect(noteToItems(wired, 7)).toEqual([
      { id: "2", label: "Oscillator 1" },
      { id: "6", label: "(empty)" },
    ]);
    expect(noteToItems(wired, 8)).toEqual([
      { id: "2", label: "Oscillator 1" },
      { id: "5", label: "Oscillator 2" },
      { id: "6", label: "(empty)" },
    ]);
  });

  test("are none for a node that is no source: a lane, an effect, the mix, or no node", () => {
    for (const node of [2, 3, 4, 0, MASTER, 77]) expect(noteToItems(wired, node), String(node)).toEqual([]);
  });
});
