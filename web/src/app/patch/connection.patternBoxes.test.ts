import type { Connection } from "@xyflow/react";
import { describe, expect, test } from "vitest";

import { patternBoxesFixture } from "../../client/fixtures";
import type { NodeId } from "../../protocol";
import { canConnect, drawCable, kindOfDrag, takersOf } from "./connection";

// Cables of notes at the ends of a pattern box (step 124 C): what a drag from a port can end at, what is drawn when it is dropped, and what the controller would refuse. The session is the fixture's with boxes: the keys (2) with a cable to Bass
// (4), Bass with a cable to the first lane (0), the device (3), Pattern (5) with a cable to the second lane (1) and no cable of notes in (the sources reach it by two links).
const found = patternBoxesFixture.cases.find((entry) => entry.name === "boxes");
if (!found) throw new Error("the fixture has the session with boxes");
const graph = found.snapshot.graph;
const [KEYS, DEVICE, BASS, PLAIN, FIRST, SECOND] = [2, 3, 4, 5, 0, 1];

/** The connection that a drag from the output of events of `from` to the input of events of `to` asks for. */
const notes = (from: NodeId, to: NodeId): Connection => ({ source: String(from), target: String(to), sourceHandle: "eventsOut", targetHandle: "eventsIn" });
const dragFrom = (node: NodeId) => takersOf(graph, [], { nodeId: String(node), handleType: "source", handleId: "eventsOut" });
const keysOf = (...ends: [NodeId, string][]): Set<string> => new Set(ends.map(([node, handle]) => `${String(node)}:${handle}`));

describe("a cable of notes dragged from a pattern box", () => {
  test("is a cable of notes, whatever it is dragged from: an output of events", () => {
    expect(kindOfDrag("eventsOut")).toBe("notes");
  });

  test("can end at an input of events of a lane that has no cable from the box, and at no box: a box does not send to another yet", () => {
    // (Bass has a cable to the first lane: the second can still be drawn to. The boxes are not takers.)
    expect(dragFrom(BASS)).toEqual(keysOf([SECOND, "eventsIn"]));
    expect(dragFrom(PLAIN)).toEqual(keysOf([FIRST, "eventsIn"]));
  });

  test("is drawn to the lane that it is dropped on", () => {
    const asked: [string, unknown[]][] = [];
    const editor = {
      addAudioCable: (...args: unknown[]) => Promise.resolve(void asked.push(["addAudioCable", args])),
      addNotesCable: (...args: unknown[]) => Promise.resolve(void asked.push(["addNotesCable", args])),
      addCable: (...args: unknown[]) => Promise.resolve(void asked.push(["addCable", args])),
    };
    drawCable(editor as never, notes(BASS, SECOND));
    expect(asked).toEqual([["addNotesCable", [BASS, SECOND, "pick"]]]);
  });

  test("is one that the controller would take when it ends at a lane that has none from the box, and refuses a lane that has, a box, and a box to itself", () => {
    expect(canConnect(graph, [], notes(BASS, SECOND))).toBe(true);
    expect(canConnect(graph, [], notes(BASS, FIRST))).toBe(false);
    expect(canConnect(graph, [], notes(PLAIN, FIRST))).toBe(true);
    expect(canConnect(graph, [], notes(BASS, PLAIN))).toBe(false);
    expect(canConnect(graph, [], notes(PLAIN, BASS))).toBe(false);
    expect(canConnect(graph, [], notes(BASS, BASS))).toBe(false);
  });
});

describe("a cable of notes dragged from a source to a pattern box", () => {
  test("can end at the input of events of each box that the source has no cable to, and at the lanes that it has none to", () => {
    // (The keys have a cable to Bass: Pattern, which none ends at, and both lanes are takers. The device has none anywhere.)
    expect(dragFrom(KEYS)).toEqual(keysOf([PLAIN, "eventsIn"], [FIRST, "eventsIn"], [SECOND, "eventsIn"]));
    expect(dragFrom(DEVICE)).toEqual(keysOf([BASS, "eventsIn"], [PLAIN, "eventsIn"], [FIRST, "eventsIn"], [SECOND, "eventsIn"]));
  });

  test("is one that the controller would take when the box has none from the source, and refuses the cable that is there already", () => {
    expect(canConnect(graph, [], notes(KEYS, PLAIN))).toBe(true);
    expect(canConnect(graph, [], notes(DEVICE, BASS))).toBe(true);
    expect(canConnect(graph, [], notes(KEYS, BASS))).toBe(false);
  });

  test("is drawn to the box that it is dropped on", () => {
    const asked: unknown[][] = [];
    const editor = {
      addAudioCable: () => Promise.resolve(),
      addNotesCable: (...args: unknown[]) => Promise.resolve(void asked.push(args)),
      addCable: () => Promise.resolve(),
    };
    drawCable(editor as never, notes(DEVICE, PLAIN));
    expect(asked).toEqual([[DEVICE, PLAIN, "pick"]]);
  });
});
