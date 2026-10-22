import { describe, expect, test } from "vitest";

import { DEVICE_NAME_LIMIT } from "../protocol";
import type { Edge, Endpoint, Graph, NodeId } from "../protocol";
import { MASTER, sourcesFixture } from "./fixtures";
import { deviceName, eventCableEnds, noteTakers, withHears } from "./noteWiring";

// The session of the fixture (`testutil::NOTES`): the keys (7) and a device (8) are the sources, then group 0 ("Pad", node 0, a reverb on its bus), lane 0 (node 2) in it with a filter and an LFO, a plain lane (5) and an empty one (6); a
// cable of notes (1) from the keys to lane 1 (node 5) and an audio cable (2) from lane 1 to lane 0's filter.
const wired = sourcesFixture.cases.find((entry) => entry.name === "wired");
if (!wired) throw new Error("the fixture has a session with sources");
const graph = wired.snapshot.graph;
const named = (name: string): NodeId => {
  const found = graph.nodes.find((n) => n.name === name);
  if (!found) throw new Error(`the session has no node called ${name}`);
  return found.id;
};
const keys = named("Keys");
const launchkey = named("Launchkey Mini MK4 37");
const lanes = graph.nodes.filter((n) => n.kind === "instrument").map((n) => n.id);
const [lane0, lane1, lane2] = lanes;
if (lane0 === undefined || lane1 === undefined || lane2 === undefined) throw new Error("the session has three lanes");
const filter = named("Filter");

const out = (node: NodeId): Endpoint => ({ node, port: { type: "eventsOut" } });
const into = (node: NodeId): Endpoint => ({ node, port: { type: "eventsIn" } });
const words = (result: ReturnType<typeof eventCableEnds>): string => ("error" in result ? `${result.error.code}: ${result.error.message}` : "ends");

describe("the ends of a cable of notes", () => {
  test("are a source and a lane: the node of the source at the start, and the lane's own node at the end", () => {
    expect(eventCableEnds(graph, out(keys), into(lane0))).toEqual({ source: keys, lane: lane0 });
    expect(eventCableEnds(graph, out(launchkey), into(lane2))).toEqual({ source: launchkey, lane: lane2 });
  });

  test("are refused in the controller's words and its order: the ports, then the mix as a start, a start that is not there or is no source, the mix as an end, an end that is not there or is no lane", () => {
    const audioOut: Endpoint = { node: keys, port: { type: "audioOut" } };
    const audioIn: Endpoint = { node: lane0, port: { type: "audioIn" } };
    expect(words(eventCableEnds(graph, audioOut, into(lane0)))).toBe("invalid: a cable of notes starts at an output of events");
    expect(words(eventCableEnds(graph, out(keys), audioIn))).toBe("invalid: a cable of notes ends at an input of events");
    // (The ports first: a start that is no source, and an end that is not a lane, are not looked at.)
    expect(words(eventCableEnds(graph, audioOut, audioIn))).toBe("invalid: a cable of notes starts at an output of events");
    expect(words(eventCableEnds(graph, out(MASTER), into(lane0)))).toBe("invalid: the mix has no output of events");
    expect(words(eventCableEnds(graph, out(9_999), into(lane0)))).toBe("notFound: there is no node 9999");
    expect(words(eventCableEnds(graph, out(lane0), into(lane1)))).toBe("invalid: only a source of notes has an output of events");
    expect(words(eventCableEnds(graph, out(filter), into(lane1)))).toBe("invalid: only a source of notes has an output of events");
    expect(words(eventCableEnds(graph, out(keys), into(MASTER)))).toBe("invalid: the mix has no input of events");
    expect(words(eventCableEnds(graph, out(keys), into(9_999)))).toBe("notFound: there is no node 9999");
    expect(words(eventCableEnds(graph, out(keys), into(filter)))).toBe("invalid: a cable of notes ends at a lane: it is the lanes that have an input of events");
    expect(words(eventCableEnds(graph, out(keys), into(launchkey)))).toBe("invalid: a cable of notes ends at a lane: it is the lanes that have an input of events");
    // (A start that is not there is looked at before the end, and a start that is no source before the end that is not there.)
    expect(words(eventCableEnds(graph, out(9_999), into(9_998)))).toBe("notFound: there is no node 9999");
    expect(words(eventCableEnds(graph, out(lane0), into(9_998)))).toBe("invalid: only a source of notes has an output of events");
    expect(words(eventCableEnds(graph, out(MASTER), into(MASTER)))).toBe("invalid: the mix has no output of events");
  });
});

describe("the lanes that can still be given a cable of notes from a source", () => {
  test("are those that have none from it already: the keys have a cable to lane 1, the device has none", () => {
    expect(noteTakers(graph, keys)).toEqual([lane0, lane2]);
    expect(noteTakers(graph, launchkey)).toEqual([lane0, lane1, lane2]);
  });

  test("are none for what is no source, and none for what is not there", () => {
    expect(noteTakers(graph, lane0)).toEqual([]);
    expect(noteTakers(graph, filter)).toEqual([]);
    expect(noteTakers(graph, MASTER)).toEqual([]);
    expect(noteTakers(graph, 9_999)).toEqual([]);
  });

  test("do not count a cable of notes from another source, or an audio cable, to the lane", () => {
    // (Lane 1 has a cable of notes from the keys and an audio cable of its own: the device may still be wired to it, and the keys may not.)
    expect(noteTakers(graph, launchkey)).toContain(lane1);
    expect(noteTakers(graph, keys)).not.toContain(lane1);
  });
});

const hearsOf = (g: Graph): [NodeId, NodeId][] => g.edges.flatMap((e) => (e.kind === "hears" ? [[e.from.node, e.to.node] as [NodeId, NodeId]] : []));
const kinds = (g: Graph): string[] => g.edges.map((e) => e.kind);

describe("the links of the lanes that hear every source", () => {
  test("are what the graph of the fixture has: the graph is what it derives from its own cables", () => {
    expect(withHears(graph)).toEqual(graph);
    // (And it has some: the keys are wired to lane 1, so that every source is heard by lanes 0 and 2 and the device by none other.)
    expect(hearsOf(graph)).toEqual([
      [keys, lane0],
      [keys, lane2],
      [launchkey, lane0],
      [launchkey, lane2],
    ]);
  });

  test("go from each source in order to each lane that has no cable of notes, in order, after the links of the audio path and before the cables", () => {
    const order = kinds(graph);
    const first = order.indexOf("hears");
    expect(order.slice(0, first).every((kind) => kind === "audio" || kind === "member")).toBe(true);
    expect(order.slice(first, first + 4)).toEqual(["hears", "hears", "hears", "hears"]);
    expect(order.slice(first + 4)).toEqual(["cable", "events", "modulation"]);
  });

  test("are made again from the cables: a lane that loses its last cable of notes hears every source, and one that gets its first hears the others no more", () => {
    const cable = graph.edges.find((e) => e.kind === "events");
    if (!cable) throw new Error("the session has a cable of notes");
    const without: Graph = { nodes: graph.nodes, edges: graph.edges.filter((e) => e !== cable) };
    expect(hearsOf(withHears(without))).toEqual([
      [keys, lane0],
      [keys, lane1],
      [keys, lane2],
      [launchkey, lane0],
      [launchkey, lane1],
      [launchkey, lane2],
    ]);
    const added: Edge = { from: out(launchkey), to: into(lane0), kind: "events", id: 77 };
    const more: Graph = { nodes: graph.nodes, edges: [...graph.edges, added] };
    expect(hearsOf(withHears(more))).toEqual([
      [keys, lane2],
      [launchkey, lane2],
    ]);
  });

  test("are not kept from the graph that is given: they are left out first, whatever they were, and nothing is made twice", () => {
    const stale: Edge = { from: out(keys), to: into(lane1), kind: "hears" };
    const once = withHears({ nodes: graph.nodes, edges: [stale, ...graph.edges] });
    expect(once).toEqual(graph);
    expect(withHears(once)).toEqual(graph);
  });

  test("are none where there is no source, and none for what is no lane", () => {
    const bare = sourcesFixture.cases.find((entry) => entry.name === "bare");
    if (!bare) throw new Error("the fixture has a session with no source");
    expect(hearsOf(withHears(bare.snapshot.graph))).toEqual([]);
    const lanesGone: Graph = { nodes: graph.nodes.filter((n) => n.kind !== "instrument"), edges: graph.edges.filter((e) => e.kind !== "events" && e.kind !== "hears" && e.kind !== "cable") };
    expect(hearsOf(withHears(lanesGone))).toEqual([]);
  });

  test("go at the end of a graph that has nothing but the links of the audio path", () => {
    const links = graph.edges.filter((e) => e.kind === "audio" || e.kind === "member");
    const only: Graph = { nodes: graph.nodes, edges: links };
    const made = withHears(only);
    expect(made.edges.slice(0, links.length)).toEqual(links);
    expect(hearsOf(made)).toHaveLength(6);
    expect(made.edges.slice(links.length).every((e) => e.kind === "hears")).toBe(true);
  });
});

describe("the name of a device", () => {
  test("is the text without the spaces round it, and the case is the name's", () => {
    expect(deviceName("Seaboard")).toEqual({ name: "Seaboard" });
    expect(deviceName("  Seaboard RISE \t")).toEqual({ name: "Seaboard RISE" });
    expect(deviceName(" Seaboard ")).toEqual({ name: "Seaboard" });
    expect(deviceName("\u0085Seaboard\u0085")).toEqual({ name: "Seaboard" });
    expect(deviceName("seaboard")).toEqual({ name: "seaboard" });
    expect(deviceName("Roland A-49 (2)")).toEqual({ name: "Roland A-49 (2)" });
    // (What the page's own `trim` takes and Rust's does not, the byte order mark, is not a space here.)
    expect(deviceName("﻿Seaboard")).toEqual({ name: "﻿Seaboard" });
  });

  test("is something: a blank name is refused", () => {
    const blank = { why: "is blank: a device is called by its name, or as much of it as tells it" };
    expect(deviceName("")).toEqual(blank);
    expect(deviceName("   ")).toEqual(blank);
    expect(deviceName(" \t\n ")).toEqual(blank);
  });

  test("is at most as many characters as the controller says, counted as a person counts them", () => {
    expect(DEVICE_NAME_LIMIT).toBe(256);
    const long = { why: "is longer than 256 characters" };
    expect(deviceName("x".repeat(256))).toEqual({ name: "x".repeat(256) });
    expect(deviceName("x".repeat(257))).toEqual(long);
    // (A character that a string counts as two units, a face and an accent that is a character of its own: each is one.)
    expect(deviceName("\u{1d11e}".repeat(256))).toEqual({ name: "\u{1d11e}".repeat(256) });
    expect(deviceName("\u{1d11e}".repeat(257))).toEqual(long);
    expect(deviceName("é".repeat(256))).toEqual({ name: "é".repeat(256) });
    // (The spaces round it are not counted: it is the name that is counted.)
    expect(deviceName(` ${"x".repeat(256)} `)).toEqual({ name: "x".repeat(256) });
  });

  test("has no control character in it, and a character that is no control character is a name", () => {
    const control = { why: "has a control character in it" };
    for (const text of ["A\nB", "A\tB", "A\u007fB", "A\u0085B", "A\u0000B", "A\u009fB", "A\u001fB"]) expect(deviceName(text), JSON.stringify(text)).toEqual(control);
    for (const text of ["A B", "A B", "A¡B", "A B", "Ünïcode"]) expect(deviceName(text), JSON.stringify(text)).toEqual({ name: text });
  });

  test("is judged blank first, then long, then for a control character", () => {
    expect(deviceName(`${"x".repeat(257)}\n`)).toEqual({ why: "is longer than 256 characters" });
    expect(deviceName(" \n ")).toEqual({ why: "is blank: a device is called by its name, or as much of it as tells it" });
    expect(deviceName(`${"x".repeat(256)}\u0001`)).toEqual({ why: "is longer than 256 characters" });
  });
});
