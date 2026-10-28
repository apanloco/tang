import { describe, expect, test } from "vitest";

import { MASTER, audioCablesFixture, fieldsFixture, freeReachFixture, reachFixture, sideCablesFixture, sourcesFixture, tracerSnapshot } from "../../client/fixtures";
import type { Port } from "../../protocol";
import { audioCableOf, audioTakersOf, cableOf, canConnect, canDraw, canDrawAudio, canDrawNotes, drawableFrom, drawCable, keyOf, kindOfDrag, notesCableOf, notesTakersOf, takersOf } from "./connection";
import { handleOf, toFlow } from "./toFlow";

/** The port of a parameter, by the plugin's own index. */
const param = (index: number): Port => ({ type: "param", index });

describe("a connection that is dragged on the canvas", () => {
  const connection = (source: string, sourceHandle: string | null, target: string, targetHandle: string | null) => ({ source, sourceHandle, target, targetHandle });

  test("is a cable when it goes from a modulator's output to a parameter's port, by the ids of the nodes and the plugin's own index", () => {
    expect(cableOf(connection("2", "modOut", "1", "param-3"))).toEqual({ modulator: 2, node: 1, port: param(3) });
    expect(cableOf(connection("12", "modOut", "30", "param-0"))).toEqual({ modulator: 12, node: 30, port: param(0) });
  });

  test("is a cable when it goes from a modulator's output to a rate or a stage of a modulator", () => {
    for (const type of ["rate", "attack", "decay", "sustain", "release"] as const) {
      expect(cableOf(connection("2", "modOut", "7", type)), type).toEqual({ modulator: 2, node: 7, port: { type } });
    }
  });

  test("is a cable when it goes from a modulator's output to the depth of a cable, by the id of the cable, at the node of the modulator whose cable it is", () => {
    expect(cableOf(connection("13", "modOut", "4", "depth-0"))).toEqual({ modulator: 13, node: 4, port: { type: "depth", edge: 0 } });
    expect(cableOf(connection("2", "modOut", "7", "depth-123"))).toEqual({ modulator: 2, node: 7, port: { type: "depth", edge: 123 } });
  });

  test("is read with the handles' names as the canvas gives them (`handleOf`)", () => {
    expect(cableOf(connection("2", handleOf({ type: "modOut" }), "1", handleOf(param(17))))).toEqual({ modulator: 2, node: 1, port: param(17) });
    expect(cableOf(connection("2", handleOf({ type: "modOut" }), "7", handleOf({ type: "depth", edge: 5 })))).toEqual({ modulator: 2, node: 7, port: { type: "depth", edge: 5 } });
    for (const type of ["rate", "attack", "decay", "sustain", "release"] as const) {
      expect(cableOf(connection("2", handleOf({ type: "modOut" }), "7", handleOf({ type }))), type).toEqual({ modulator: 2, node: 7, port: { type } });
    }
  });

  test("is not a cable when it starts or ends anywhere else", () => {
    const refused = [
      connection("2", "audioOut", "1", "param-3"),
      connection("2", "modOut", "1", "audioIn"),
      connection("2", "modOut", "1", "audioOut"),
      connection("2", "modOut", "1", "modOut"),
      connection("2", "modOut", "1", "param-"),
      connection("2", "modOut", "1", "param-x"),
      connection("2", "modOut", "1", "param--1"),
      connection("2", "modOut", "1", "param-1.5"),
      connection("2", "modOut", "1", "xparam-3"),
      connection("2", "modOut", "1", "param_3"), // (what comes after the prefix is a number, and the prefix is not it)
      connection("2", "modOut", "1", "depth"), // (the port of a depth is named after the cable)
      connection("2", "modOut", "1", "depth-"),
      connection("2", "modOut", "1", "depth-x"),
      connection("2", "modOut", "1", "depth--1"),
      connection("2", "modOut", "1", "depth-1.5"),
      connection("2", "modOut", "1", "depth-1 "),
      connection("2", "modOut", "1", "xdepth-3"),
      connection("2", "modOut", "1", "depth_3"),
      connection("2", "modOut", "1", "Depth-3"),
      connection("2", "modOut", "1", "waveform"), // (a choice, which no cable ends at)
      connection("2", "modOut", "1", "Rate"),
      connection("2", "modOut", "1", "rate "),
      connection("2", "modOut", "1", "rate-1"),
      connection("2", "modOut", "1", "ate"),
      connection("2", null, "1", "param-3"),
      connection("2", "modOut", "1", null),
      connection("2", "modOut", "1", ""),
      connection("two", "modOut", "1", "param-3"),
      connection("2", "modOut", "one", "param-3"),
      connection("", "modOut", "1", "param-3"),
      connection("two", "modOut", "1", "rate"),
      connection("2", "modOut", "one", "rate"),
    ];
    for (const one of refused) expect(cableOf(one), JSON.stringify(one)).toBeNull();
  });

  test("can be drawn when the modulator can still drive that parameter, and not otherwise", () => {
    const { nodes } = toFlow(tracerSnapshot.graph);
    // The LFO (2) drives the filter's cutoff (1, 0) already; the oscillator's detune (0, 1) and the filter's resonance (1, 1) are free.
    expect(canDraw(nodes, { modulator: 2, node: 0, port: param(1) })).toBe(true);
    expect(canDraw(nodes, { modulator: 2, node: 1, port: param(1) })).toBe(true);
    expect(canDraw(nodes, { modulator: 2, node: 1, port: param(0) })).toBe(false); // already
    expect(canDraw(nodes, { modulator: 2, node: 1, port: param(99) })).toBe(false); // no such parameter
    expect(canDraw(nodes, { modulator: 2, node: 2, port: param(0) })).toBe(false); // a modulator is no plugin
    expect(canDraw(nodes, { modulator: 0, node: 1, port: param(1) })).toBe(false); // an oscillator is no modulator
    expect(canDraw(nodes, { modulator: 99, node: 1, port: param(1) })).toBe(false); // nothing
  });

  test("can be drawn as far as the controller's reach goes: to a plugin of any lane or group, whatever the modulator hears, and to nothing that is no plugin", () => {
    const { nodes } = toFlow(reachFixture.snapshot.graph);
    // The LFO that hears lane 4 (12) and the LFO that hears group 0 (15) reach the oscillators (4, 6, 7, 9), the filters (5, 10, and the bus's, 1) and the reverbs (8, and the bus's, 3).
    for (const modulator of [12, 15]) {
      for (const node of [1, 3, 4, 5, 6, 7, 8, 9, 10]) expect(canDraw(nodes, { modulator, node, port: param(0) }), `${String(modulator)} to ${String(node)}`).toBe(true);
      // Not a group, a lane that has no instrument or a modulator.
      for (const node of [0, 2, 11, 13]) expect(canDraw(nodes, { modulator, node, port: param(0) }), `${String(modulator)} to ${String(node)}`).toBe(false);
    }
  });

  test("lists what can be drawn from a modulator, as the nodes and parameters a port of a canvas is of", () => {
    const { nodes } = toFlow(tracerSnapshot.graph);
    const keys = drawableFrom(nodes, 2);
    expect(keys.has("0:param-1")).toBe(true);
    expect(keys.has("1:param-1")).toBe(true);
    expect(keys.has("1:param-0")).toBe(false);
    // (The oscillator's output volume and the filter's mix, which the host keeps, are ports too.)
    expect(keys.has("0:volume")).toBe(true);
    expect(keys.has("1:mix")).toBe(true);
    expect(keys.has("1:volume")).toBe(false);
    expect(keys.size).toBe(20 + 3 + 2);
    expect(drawableFrom(nodes, 0).size).toBe(0);
    expect(drawableFrom(nodes, 99).size).toBe(0);
  });

  test("has a key for the port of a node that is the name of the node and of the handle", () => {
    expect(keyOf(1, "param-0")).toBe("1:param-0");
    expect(keyOf(12, "rate")).toBe("12:rate");
    expect(keyOf(1, "rate")).not.toBe(keyOf(11, "rate"));
    expect(keyOf(1, "param-0")).not.toBe(keyOf(10, "param-0"));
  });
});

describe("a cable dragged to a setting of another modulator", () => {
  // An LFO (13), an envelope (14) and a MIDI control (15) hear Group 1; its first member (3) is heard by an LFO (7), an envelope (8) and a MIDI control (9), and the second (4) by an LFO (10).
  const { nodes } = toFlow(fieldsFixture.snapshot.graph);
  const settings = (modulator: number): string[] => [...drawableFrom(nodes, modulator)].filter((key) => !key.includes("param-") && !/:(volume|mix|pan)$/.test(key)).sort();

  test("can be drawn to the rate of an LFO and the stages of an envelope, whatever either of them hears, and to nothing else of a modulator", () => {
    expect(canDraw(nodes, { modulator: 8, node: 7, port: { type: "rate" } })).toBe(true); // the envelope that hears the lane to the LFO's rate
    expect(canDraw(nodes, { modulator: 7, node: 8, port: { type: "attack" } })).toBe(true);
    expect(canDraw(nodes, { modulator: 9, node: 8, port: { type: "release" } })).toBe(true);
    expect(canDraw(nodes, { modulator: 13, node: 14, port: { type: "decay" } })).toBe(true); // two that hear the group
    expect(canDraw(nodes, { modulator: 13, node: 10, port: { type: "rate" } })).toBe(true); // to one that hears a member
    expect(canDraw(nodes, { modulator: 7, node: 10, port: { type: "rate" } })).toBe(true); // to one that hears another lane
    expect(canDraw(nodes, { modulator: 7, node: 13, port: { type: "rate" } })).toBe(true); // to one that hears the group
    expect(canDraw(nodes, { modulator: 13, node: 12, port: { type: "rate" } })).toBe(true); // to one that hears a lane of another group
    expect(canDraw(nodes, { modulator: 13, node: 11, port: { type: "rate" } })).toBe(true); // to one that hears a lane in no group
    expect(canDraw(nodes, { modulator: 7, node: 7, port: { type: "rate" } })).toBe(false); // itself
    expect(canDraw(nodes, { modulator: 7, node: 8, port: { type: "rate" } })).toBe(false); // an envelope has no rate
    expect(canDraw(nodes, { modulator: 8, node: 7, port: { type: "attack" } })).toBe(false); // an LFO has no attack
    expect(canDraw(nodes, { modulator: 8, node: 9, port: { type: "rate" } })).toBe(false); // a MIDI control has no settings
  });

  test("can be drawn to the depth of a cable of another modulator, and to no other", () => {
    // The LFO (7) to the depth of the envelope's cables (1 and 2) and the MIDI control's (3); the envelope (8) to the LFO's (0).
    expect(canDraw(nodes, { modulator: 7, node: 8, port: { type: "depth", edge: 1 } })).toBe(true);
    expect(canDraw(nodes, { modulator: 7, node: 8, port: { type: "depth", edge: 2 } })).toBe(true);
    expect(canDraw(nodes, { modulator: 7, node: 9, port: { type: "depth", edge: 3 } })).toBe(true);
    expect(canDraw(nodes, { modulator: 8, node: 7, port: { type: "depth", edge: 0 } })).toBe(true);
    expect(canDraw(nodes, { modulator: 13, node: 14, port: { type: "depth", edge: 5 } })).toBe(true); // two that hear the group
    expect(canDraw(nodes, { modulator: 13, node: 7, port: { type: "depth", edge: 0 } })).toBe(true); // to one that hears a member
    expect(canDraw(nodes, { modulator: 7, node: 14, port: { type: "depth", edge: 5 } })).toBe(true); // to one that hears the group
    expect(canDraw(nodes, { modulator: 7, node: 7, port: { type: "depth", edge: 0 } })).toBe(false); // the depth of its own cable
    expect(canDraw(nodes, { modulator: 7, node: 8, port: { type: "depth", edge: 0 } })).toBe(false); // a cable that is another modulator's
    expect(canDraw(nodes, { modulator: 7, node: 8, port: { type: "depth", edge: 99 } })).toBe(false); // no such cable
    expect(canDraw(nodes, { modulator: 7, node: 10, port: { type: "depth", edge: 5 } })).toBe(false); // another modulator's, which has no such cable
    expect(canDraw(nodes, { modulator: 13, node: 12, port: { type: "depth", edge: 0 } })).toBe(false); // the same
    expect(canDraw(nodes, { modulator: 0, node: 8, port: { type: "depth", edge: 1 } })).toBe(false); // not a modulator
  });

  test("is what the controller allows: the settings of the modulators it reaches and the depths of their cables", () => {
    for (const [id, listed] of Object.entries(fieldsFixture.fields)) {
      const keysListed = listed.map((field) => keyOf(field.node, handleOf(field.port)));
      expect(settings(Number(id)), `modulator ${id}`).toEqual(keysListed.sort());
    }
    // (Some of what it allows are depths, so that the agreement says something of them.)
    const depths = Object.values(fieldsFixture.fields).flatMap((listed) => listed.filter((field) => field.port.type === "depth"));
    expect(depths.length).toBeGreaterThanOrEqual(5);
  });

  test("is not a port of the settings it drives already", () => {
    const graph = fieldsFixture.snapshot.graph;
    const driven = {
      ...graph,
      edges: [...graph.edges, { kind: "modulation" as const, id: 99, amount: 0.5, from: { node: 8, port: { type: "modOut" as const } }, to: { node: 7, port: { type: "rate" as const } } }],
    };
    const after = toFlow(driven).nodes;
    expect(canDraw(nodes, { modulator: 8, node: 7, port: { type: "rate" } })).toBe(true);
    expect(canDraw(after, { modulator: 8, node: 7, port: { type: "rate" } })).toBe(false);
    // (Another modulator may drive it too: the two add up.)
    expect(canDraw(after, { modulator: 9, node: 7, port: { type: "rate" } })).toBe(true);
  });

  test("is not a port of the depth it drives already, and the depths of the other cables of that modulator are other ports", () => {
    const graph = fieldsFixture.snapshot.graph;
    const driven = {
      ...graph,
      edges: [...graph.edges, { kind: "modulation" as const, id: 99, amount: 0.5, from: { node: 7, port: { type: "modOut" as const } }, to: { node: 8, port: { type: "depth" as const, edge: 1 } } }],
    };
    const after = toFlow(driven).nodes;
    expect(canDraw(nodes, { modulator: 7, node: 8, port: { type: "depth", edge: 1 } })).toBe(true);
    expect(canDraw(after, { modulator: 7, node: 8, port: { type: "depth", edge: 1 } })).toBe(false);
    expect(canDraw(after, { modulator: 7, node: 8, port: { type: "depth", edge: 2 } })).toBe(true);
    // (Another modulator may drive it too: the two add up.)
    expect(canDraw(after, { modulator: 9, node: 8, port: { type: "depth", edge: 1 } })).toBe(true);
  });
});

describe("an audio cable dragged on the canvas", () => {
  const connection = (source: string, sourceHandle: string | null, target: string, targetHandle: string | null) => ({ source, sourceHandle, target, targetHandle });
  // The session of the audio cables fixture: group 0 (Pad) with Filter 1, Reverb 2 and Delay 3 on its bus; lane 4 in it with Filter 5 and Delay 6; lane 7 with Drive 8; empty lane 9; an LFO (10).
  // The cables: 1 from lane 7 to Filter 5, 2 from Filter 5 to Delay 6, 3 from the group's output to Drive 8, 4 from Filter 1 to Reverb 2, 5 from Delay 6 to the group's bus, 6 from Reverb 2 to the mix.
  const { graph } = audioCablesFixture.snapshot;

  test("is read when it goes from an output of audio to an input of audio, by the ids of the nodes", () => {
    expect(audioCableOf(connection("8", "audioOut", "5", "audioIn"))).toEqual({ from: 8, to: 5, side: false });
    expect(audioCableOf(connection("0", "audioOut", String(MASTER), "audioIn"))).toEqual({ from: 0, to: MASTER, side: false });
    expect(audioCableOf(connection("12", "audioOut", "30", "audioIn"))).toEqual({ from: 12, to: 30, side: false });
  });

  test("is read as a cable to the second input of an effect when it ends at that handle", () => {
    expect(audioCableOf(connection("8", "audioOut", "5", "audioInSide"))).toEqual({ from: 8, to: 5, side: true });
    expect(audioCableOf(connection("8", handleOf({ type: "audioOut" }), "5", handleOf({ type: "audioInSide" })))).toEqual({ from: 8, to: 5, side: true });
    // (A second input starts nothing, as the first does not.)
    expect(audioCableOf(connection("8", "audioInSide", "5", "audioIn"))).toBeNull();
    expect(audioCableOf(connection("8", "audioInSide", "5", "audioInSide"))).toBeNull();
    expect(audioCableOf(connection("8", "audioOut", "5", "audioInSide "))).toBeNull();
    expect(audioCableOf(connection("8", "audioOut", "5", "AudioInSide"))).toBeNull();
    expect(audioCableOf(connection("8", "audioOut", "five", "audioInSide"))).toBeNull();
  });

  test("is not read when it starts or ends anywhere else: a modulator's output, a parameter, a setting, or no handle", () => {
    const refused = [
      connection("8", "modOut", "5", "audioIn"),
      connection("2", "modOut", "1", "param-3"),
      connection("8", "audioOut", "5", "param-3"),
      connection("8", "audioOut", "5", "rate"),
      connection("8", "audioOut", "5", "audioOut"),
      connection("8", "audioIn", "5", "audioIn"),
      connection("8", "audioOut", "5", "AudioIn"),
      connection("8", "audioOut", "5", "audioIn "),
      connection("8", "audioOut", "5", "depth-3"),
      connection("8", null, "5", "audioIn"),
      connection("8", "audioOut", "5", null),
      connection("8", "audioOut", "5", ""),
      connection("eight", "audioOut", "5", "audioIn"),
      connection("8", "audioOut", "five", "audioIn"),
      connection("", "audioOut", "5", "audioIn"),
      connection("8", "audioOut", "-5", "audioIn"),
      connection("8", "audioOut", "5.5", "audioIn"),
    ];
    for (const one of refused) expect(audioCableOf(one), JSON.stringify(one)).toBeNull();
  });

  test("is not a cable of modulation, and a cable of modulation is not an audio cable", () => {
    expect(cableOf(connection("8", "audioOut", "5", "audioIn"))).toBeNull();
    expect(audioCableOf(connection("2", "modOut", "1", "param-3"))).toBeNull();
  });

  test("can be drawn where the controller would draw it: not where one is drawn already, not where the audio would go round, not from the mix or to a lane", () => {
    expect(canDrawAudio(graph, { from: 7, to: 8, side: false })).toBe(false); // the link of the chain, which is a cable that is there (a link that is cut is given back by one: step 88)
    expect(canDrawAudio(graph, { from: 8, to: MASTER, side: false })).toBe(false); // the way out of the lane, the same
    expect(canDrawAudio(graph, { from: 7, to: 1, side: false })).toBe(true); // a bus effect
    expect(canDrawAudio(graph, { from: 7, to: MASTER, side: false })).toBe(true); // beside the way out of the lane, which starts at the end of its chain
    expect(canDrawAudio(graph, { from: 7, to: 5, side: false })).toBe(false); // there already
    expect(canDrawAudio(graph, { from: 8, to: 5, side: false })).toBe(false); // round
    expect(canDrawAudio(graph, { from: 0, to: 0, side: false })).toBe(false); // a group's output to its own bus
    expect(canDrawAudio(graph, { from: MASTER, to: 5, side: false })).toBe(false); // the mix has no output
    expect(canDrawAudio(graph, { from: 7, to: 9, side: false })).toBe(false); // a lane has no input
    expect(canDrawAudio(graph, { from: 10, to: 5, side: false })).toBe(false); // a modulator has no audio
    expect(canDrawAudio(graph, { from: 7, to: 77, side: false })).toBe(false); // nothing
    expect(canDrawAudio(graph, { from: 77, to: 5, side: false })).toBe(false);
  });

  test("has, for the ports of the canvas, the inputs that take a cable from an output, by the name of the node and of the handle", () => {
    expect([...audioTakersOf(graph, 7)].sort()).toEqual(["0:audioIn", "1:audioIn", "2:audioIn", "3:audioIn", `${String(MASTER)}:audioIn`, "6:audioIn"].sort());
    expect([...audioTakersOf(graph, 0)]).toEqual([`${String(MASTER)}:audioIn`]);
    expect(audioTakersOf(graph, 10).size).toBe(0);
    expect(audioTakersOf(graph, MASTER).size).toBe(0);
    expect(audioTakersOf(graph, 77).size).toBe(0);
    // (A key of an input is not that of a parameter's or a setting's port of the same node.)
    expect(keyOf(9, "audioIn")).toBe("9:audioIn");
    expect(keyOf(9, "audioIn")).not.toBe(keyOf(9, "audioOut"));
  });

  test("is drawn from the tracer's oscillator to the mix and to nothing else (its filter has the link of the chain, which is a cable that is there), and from its filter to nothing", () => {
    // (The tracer: oscillator 0, filter 1, LFO 2. The oscillator's link into the filter and the filter's way out to the mix are there, and are cables: a cable from the oscillator may end at the mix, beside the filter's way out,
    // and the filter has nowhere else to send to, since the one place there is for it is the link it has.)
    expect([...audioTakersOf(tracerSnapshot.graph, 0)]).toEqual([`${String(MASTER)}:audioIn`]);
    expect(audioTakersOf(tracerSnapshot.graph, 1).size).toBe(0);
  });
});

/** The session of the sources fixture (`testutil::NOTES`): the keys (7) and a device (8) are the sources; lane 2 is in the group Pad, lane 4 is outside it and lane 5 is empty, and an LFO (6) hears lane 2; a cable of notes (1) goes from the keys to lane 4. */
describe("an audio cable dragged to the second input of an effect", () => {
  const connection = (source: string, sourceHandle: string | null, target: string, targetHandle: string | null) => ({ source, sourceHandle, target, targetHandle });
  // The session of the second input's fixture: group 0 has a math effect 1 on its bus; lane 2 has Filter 3 and Math 4, and three cables end at the second input of Math 4 (from lane 5's instrument, from the
  // group's output and from Filter 3); lane 5's way out is cut; 6 is a free LFO.
  const { graph } = sideCablesFixture.snapshot;
  const start = (nodeId: string | null, handleType: "source" | "target" | null, handleId: string | null) => ({ nodeId, handleType, handleId });

  test("has the second input of every effect that takes the cable, beside the first, by the name of the node and of the handle", () => {
    expect([...audioTakersOf(graph, 5)].sort()).toEqual(["0:audioIn", "1:audioIn", "1:audioInSide", "3:audioIn", "4:audioIn", `${String(MASTER)}:audioIn`].sort());
    // (Lane 2's own math effect has the other lane's cable at its second input already: only the bus's math effect takes one from there.)
    expect([...audioTakersOf(graph, 2)].filter((key) => key.endsWith("audioInSide")).sort()).toEqual(["1:audioInSide", "4:audioInSide"]);
    expect([...audioTakersOf(graph, 4)].filter((key) => key.endsWith("audioInSide"))).toEqual([]);
    expect(keyOf(4, "audioIn")).not.toBe(keyOf(4, "audioInSide"));
  });

  test("can be drawn where the controller would take it: to a second input that has none from there and that the audio does not go round to, and not to another effect's", () => {
    expect(canDrawAudio(graph, { from: 5, to: 1, side: true })).toBe(true); // the bus's math effect
    expect(canDrawAudio(graph, { from: 5, to: 4, side: true })).toBe(false); // there already
    expect(canDrawAudio(graph, { from: 5, to: 4, side: false })).toBe(true); // the first input of the same effect is another end
    expect(canDrawAudio(graph, { from: 4, to: 4, side: true })).toBe(false); // an effect to its own second input: round
    expect(canDrawAudio(graph, { from: 4, to: 1, side: true })).toBe(false); // round, by the group's cable to the math effect that is after it
    expect(canDrawAudio(graph, { from: 5, to: 3, side: true })).toBe(false); // the filter has no second input
    expect(canDrawAudio(graph, { from: 5, to: 0, side: true })).toBe(false); // a group has one input
    expect(canDrawAudio(graph, { from: 5, to: MASTER, side: true })).toBe(false); // the mix has one
    expect(canDrawAudio(graph, { from: 5, to: 2, side: true })).toBe(false); // a lane has none
  });

  test("is a drag that can end at the second inputs that take it, and the drop is judged by the same: an input that is not the one it was started for is another cable", () => {
    const keys = takersOf(graph, [], start("5", "source", "audioOut"));
    expect(keys?.has("1:audioInSide")).toBe(true);
    expect(keys?.has("4:audioInSide")).toBe(false);
    expect(keys?.has("4:audioIn")).toBe(true);
    expect(canConnect(graph, [], connection("5", "audioOut", "1", "audioInSide"))).toBe(true);
    expect(canConnect(graph, [], connection("5", "audioOut", "4", "audioInSide"))).toBe(false);
    expect(canConnect(graph, [], connection("5", "audioOut", "4", "audioIn"))).toBe(true);
    expect(canConnect(graph, [], connection("5", "audioOut", "3", "audioInSide"))).toBe(false);
    expect(canConnect(null, [], connection("5", "audioOut", "1", "audioInSide"))).toBe(false);
  });
});

function notesSession() {
  const found = sourcesFixture.cases.find((entry) => entry.name === "wired");
  if (!found) throw new Error("the fixture has the session of the sources");
  return found.snapshot.graph;
}

describe("a cable of notes dragged on the canvas", () => {
  const connection = (source: string, sourceHandle: string | null, target: string, targetHandle: string | null) => ({ source, sourceHandle, target, targetHandle });
  const graph = notesSession();

  test("is read when it goes from an output of events to an input of events, by the ids of the nodes: the source it starts at and the lane it ends at", () => {
    expect(notesCableOf(connection("7", "eventsOut", "5", "eventsIn"))).toEqual({ source: 7, lane: 5 });
    expect(notesCableOf(connection("12", "eventsOut", "30", "eventsIn"))).toEqual({ source: 12, lane: 30 });
  });

  test("is not read when it starts or ends anywhere else: an output of audio, a modulator's, a parameter, a setting, or no handle", () => {
    const refused = [
      connection("7", "audioOut", "5", "audioIn"),
      connection("7", "eventsOut", "5", "audioIn"),
      connection("7", "audioOut", "5", "eventsIn"),
      connection("7", "modOut", "5", "eventsIn"),
      connection("7", "eventsOut", "5", "param-3"),
      connection("7", "eventsOut", "5", "rate"),
      connection("7", "eventsOut", "5", "eventsOut"),
      connection("7", "eventsIn", "5", "eventsIn"),
      connection("7", "eventsOut", "5", "EventsIn"),
      connection("7", "eventsOut", "5", "eventsIn "),
      connection("7", "eventsOut", "5", "depth-3"),
      connection("7", null, "5", "eventsIn"),
      connection("7", "eventsOut", "5", null),
      connection("7", "eventsOut", "5", ""),
      connection("seven", "eventsOut", "5", "eventsIn"),
      connection("7", "eventsOut", "five", "eventsIn"),
      connection("", "eventsOut", "5", "eventsIn"),
      connection("7", "eventsOut", "-5", "eventsIn"),
      connection("7", "eventsOut", "5.5", "eventsIn"),
    ];
    for (const one of refused) expect(notesCableOf(one), JSON.stringify(one)).toBeNull();
  });

  test("is not an audio cable or a cable of modulation, and neither of them is a cable of notes", () => {
    expect(audioCableOf(connection("7", "eventsOut", "5", "eventsIn"))).toBeNull();
    expect(cableOf(connection("7", "eventsOut", "5", "eventsIn"))).toBeNull();
    expect(notesCableOf(connection("8", "audioOut", "5", "audioIn"))).toBeNull();
    expect(notesCableOf(connection("2", "modOut", "1", "param-3"))).toBeNull();
  });

  test("can be drawn where the controller would draw it: from a source to a lane that has none from it yet, and not from a lane, to a source, to the mix or to nothing", () => {
    expect(canDrawNotes(graph, { source: 7, lane: 2 })).toBe(true);
    expect(canDrawNotes(graph, { source: 7, lane: 5 })).toBe(true); // an empty lane is a lane
    expect(canDrawNotes(graph, { source: 8, lane: 4 })).toBe(true); // another source's, to a lane that has one from the keys
    expect(canDrawNotes(graph, { source: 7, lane: 4 })).toBe(false); // there already
    expect(canDrawNotes(graph, { source: 2, lane: 4 })).toBe(false); // a lane is no source
    expect(canDrawNotes(graph, { source: 7, lane: 8 })).toBe(false); // a source is no lane
    expect(canDrawNotes(graph, { source: 7, lane: 3 })).toBe(false); // an effect has no input of events
    expect(canDrawNotes(graph, { source: 7, lane: MASTER })).toBe(false); // the mix has none
    expect(canDrawNotes(graph, { source: MASTER, lane: 4 })).toBe(false); // nor an output
    expect(canDrawNotes(graph, { source: 7, lane: 77 })).toBe(false);
    expect(canDrawNotes(graph, { source: 77, lane: 4 })).toBe(false);
  });

  test("has, for the ports of the canvas, the inputs that take a cable from a source, by the name of the node and of the handle", () => {
    expect([...notesTakersOf(graph, 7)]).toEqual(["2:eventsIn", "5:eventsIn"]);
    expect([...notesTakersOf(graph, 8)]).toEqual(["2:eventsIn", "4:eventsIn", "5:eventsIn"]);
    expect(notesTakersOf(graph, 2).size).toBe(0); // (a lane is no source)
    expect(notesTakersOf(graph, MASTER).size).toBe(0);
    expect(notesTakersOf(graph, 77).size).toBe(0);
    // (A key of an input of events is not that of an input of audio of the same node.)
    expect(keyOf(5, "eventsIn")).toBe("5:eventsIn");
    expect(keyOf(5, "eventsIn")).not.toBe(keyOf(5, "audioIn"));
  });
});

describe("a drag that starts at a handle of the canvas", () => {
  const audio = audioCablesFixture.snapshot.graph;
  const tracer = tracerSnapshot.graph;
  const start = (nodeId: string | null, handleType: "source" | "target" | null, handleId: string | null) => ({ nodeId, handleType, handleId });
  const connection = (source: string, sourceHandle: string | null, target: string, targetHandle: string | null) => ({ source, sourceHandle, target, targetHandle });

  test("from an output of audio can end at the inputs that take audio from it, by the name of the node and of the handle, whatever the canvas has laid out", () => {
    const keys = takersOf(audio, [], start("7", "source", "audioOut"));
    expect([...(keys ?? [])].sort()).toEqual([...audioTakersOf(audio, 7)].sort());
    expect(keys?.has("1:audioIn")).toBe(true); // a bus effect
    expect(keys?.has("8:audioIn")).toBe(false); // the link of the lane's chain, which is a cable that is there
    expect(keys?.has("5:audioIn")).toBe(false); // there is a cable there already
    expect([...(takersOf(audio, [], start("0", "source", "audioOut")) ?? [])]).toEqual([`${String(MASTER)}:audioIn`]);
  });

  test("from a modulator's output can end at the ports it can still drive", () => {
    const { nodes } = toFlow(tracer);
    expect([...(takersOf(tracer, nodes, start("2", "source", "modOut")) ?? [])].sort()).toEqual([...drawableFrom(nodes, 2)].sort());
    expect(takersOf(tracer, nodes, start("2", "source", "modOut"))?.has("0:param-1")).toBe(true);
  });

  test("from an output of events can end at the inputs of the lanes that take notes from that source, by the name of the node and of the handle", () => {
    const notes = notesSession();
    expect([...(takersOf(notes, [], start("7", "source", "eventsOut")) ?? [])]).toEqual(["2:eventsIn", "5:eventsIn"]);
    expect([...(takersOf(notes, [], start("8", "source", "eventsOut")) ?? [])]).toEqual([...notesTakersOf(notes, 8)]);
    expect(takersOf(notes, [], start("8", "source", "eventsOut"))?.has("4:eventsIn")).toBe(true);
  });

  test("from an output of events that no lane takes ends nowhere, and the set that says so is empty and not none: a node that is no source, a node that is not there", () => {
    const notes = notesSession();
    expect(takersOf(notes, [], start("2", "source", "eventsOut"))?.size).toBe(0);
    expect(takersOf(notes, [], start("77", "source", "eventsOut"))?.size).toBe(0);
    expect(takersOf(notes, [], start(String(MASTER), "source", "eventsOut"))?.size).toBe(0);
    // (A source that has a cable to every lane has nowhere to go.)
    const full: typeof notes = { nodes: notes.nodes, edges: [...notes.edges, ...[2, 5].map((lane, i) => ({ kind: "events" as const, id: 50 + i, from: { node: 7, port: { type: "eventsOut" as const } }, to: { node: lane, port: { type: "eventsIn" as const } } }))] };
    expect(takersOf(full, [], start("7", "source", "eventsOut"))?.size).toBe(0);
  });

  test("from an output of events with no session to say what is where is no drag of a cable, and from an input of events, or an output that is not events, is none either", () => {
    const notes = notesSession();
    expect(takersOf(null, [], start("7", "source", "eventsOut"))).toBeNull();
    expect(takersOf(notes, [], start("7", "target", "eventsOut"))).toBeNull();
    expect(takersOf(notes, [], start("5", "target", "eventsIn"))).toBeNull();
    expect(takersOf(notes, [], start("5", "source", "eventsIn"))).toBeNull();
    expect(takersOf(notes, [], start("7", "source", "EventsOut"))).toBeNull();
    expect(takersOf(notes, [], start("seven", "source", "eventsOut"))).toBeNull();
  });

  test("from an output that nothing is drawn from ends nowhere, and the set that says so is empty and not none: a cable is being dragged", () => {
    expect(takersOf(audio, [], start(String(MASTER), "source", "audioOut"))?.size).toBe(0);
    expect(takersOf(audio, [], start("10", "source", "audioOut"))?.size).toBe(0); // (an LFO)
    expect(takersOf(audio, [], start("77", "source", "audioOut"))?.size).toBe(0);
  });

  test("from anywhere else is no drag of a cable: an input, a port of a control, no handle, no node, a node that is no number, a session that is not there yet", () => {
    const { nodes } = toFlow(tracer);
    expect(takersOf(audio, [], start("8", "target", "audioIn"))).toBeNull();
    expect(takersOf(audio, [], start("8", "target", "audioOut"))).toBeNull();
    expect(takersOf(audio, [], start("8", "source", "audioIn"))).toBeNull();
    expect(takersOf(audio, [], start("8", "source", "param-1"))).toBeNull();
    expect(takersOf(audio, [], start("8", "source", null))).toBeNull();
    expect(takersOf(audio, [], start("8", null, "audioOut"))).toBeNull();
    expect(takersOf(audio, [], start(null, "source", "audioOut"))).toBeNull();
    expect(takersOf(audio, [], start("eight", "source", "audioOut"))).toBeNull();
    expect(takersOf(audio, [], start("8.5", "source", "audioOut"))).toBeNull();
    expect(takersOf(null, [], start("8", "source", "audioOut"))).toBeNull();
    expect(takersOf(tracer, nodes, start("2", "target", "modOut"))).toBeNull();
    expect(takersOf(tracer, nodes, start("2", "source", "audioOut"))?.size).toBe(0); // (an LFO has no audio to send)
  });

  test("is a cable when the controller would take it: an audio cable where one can be drawn, and a cable of modulation where the modulator can drive the port", () => {
    const { nodes } = toFlow(tracer);
    expect(canConnect(audio, [], connection("7", "audioOut", "1", "audioIn"))).toBe(true);
    expect(canConnect(audio, [], connection("7", "audioOut", String(MASTER), "audioIn"))).toBe(true);
    expect(canConnect(audio, [], connection("7", "audioOut", "8", "audioIn"))).toBe(false); // the link of the chain, which is a cable that is there
    expect(canConnect(audio, [], connection("7", "audioOut", "5", "audioIn"))).toBe(false); // there already
    expect(canConnect(audio, [], connection("8", "audioOut", "5", "audioIn"))).toBe(false); // round
    expect(canConnect(audio, [], connection("7", "audioOut", "9", "audioIn"))).toBe(false); // a lane has no input
    expect(canConnect(tracer, nodes, connection("2", "modOut", "0", "param-1"))).toBe(true);
    expect(canConnect(tracer, nodes, connection("2", "modOut", "1", "param-0"))).toBe(false); // already driven
  });

  test("is a cable of notes when the controller would take it: where a source has none to the lane yet", () => {
    const notes = notesSession();
    expect(canConnect(notes, [], connection("7", "eventsOut", "2", "eventsIn"))).toBe(true);
    expect(canConnect(notes, [], connection("8", "eventsOut", "4", "eventsIn"))).toBe(true);
    expect(canConnect(notes, [], connection("7", "eventsOut", "4", "eventsIn"))).toBe(false); // there already
    expect(canConnect(notes, [], connection("2", "eventsOut", "4", "eventsIn"))).toBe(false); // a lane is no source
    expect(canConnect(notes, [], connection("7", "eventsOut", "8", "eventsIn"))).toBe(false); // a source is no lane
    expect(canConnect(null, [], connection("7", "eventsOut", "2", "eventsIn"))).toBe(false); // no session to say
  });

  test("is no cable of notes when it ends where it does not fit: notes at an input of audio, audio at an input of events, a modulator at one", () => {
    const notes = notesSession();
    expect(canConnect(notes, [], connection("7", "eventsOut", "3", "audioIn"))).toBe(false);
    expect(canConnect(notes, [], connection("4", "audioOut", "2", "eventsIn"))).toBe(false);
    expect(canConnect(notes, [], connection("6", "modOut", "2", "eventsIn"))).toBe(false);
    expect(canConnect(notes, [], connection("7", "eventsOut", "3", "param-0"))).toBe(false);
  });

  test("is no cable when it ends anywhere that does not fit where it starts: audio at a parameter, a modulator at an input of audio, either with the session gone", () => {
    const { nodes } = toFlow(tracer);
    expect(canConnect(tracer, nodes, connection("0", "audioOut", "1", "param-1"))).toBe(false);
    expect(canConnect(tracer, nodes, connection("2", "modOut", "1", "audioIn"))).toBe(false);
    expect(canConnect(tracer, nodes, connection("0", "audioIn", "1", "audioOut"))).toBe(false);
    expect(canConnect(null, nodes, connection("0", "audioOut", String(MASTER), "audioIn"))).toBe(false);
    // (A cable of modulation is judged by the nodes of the canvas, which need no session of their own.)
    expect(canConnect(null, nodes, connection("2", "modOut", "0", "param-1"))).toBe(true);
  });
});

describe("the kind of cable that a drag draws", () => {
  test("is told by the handle it starts at: audio from an output of audio, notes from an output of events, modulation from a modulator's output", () => {
    expect(kindOfDrag("audioOut")).toBe("audio");
    expect(kindOfDrag("eventsOut")).toBe("notes");
    expect(kindOfDrag("modOut")).toBe("modulation");
  });

  test("is modulation for a handle that starts no other kind, and for none", () => {
    for (const handle of ["audioIn", "audioInSide", "eventsIn", "param-1", "rate", "AudioOut", null]) expect(kindOfDrag(handle), String(handle)).toBe("modulation");
  });
});

describe("a cable that is dropped on the canvas", () => {
  const connection = (source: string, sourceHandle: string | null, target: string, targetHandle: string | null) => ({ source, sourceHandle, target, targetHandle });
  /** An editor that says what it was asked for, and nothing else. */
  function asked() {
    const calls: unknown[][] = [];
    const editor = {
      addAudioCable: (...args: unknown[]) => {
        calls.push(["addAudioCable", ...args]);
        return Promise.resolve(true);
      },
      addNotesCable: (...args: unknown[]) => {
        calls.push(["addNotesCable", ...args]);
        return Promise.resolve(true);
      },
      addCable: (...args: unknown[]) => {
        calls.push(["addCable", ...args]);
        return Promise.resolve(true);
      },
    };
    return { calls, editor };
  }

  test("is an audio cable from an output of audio to an input of audio: the editor is asked to draw it between the two nodes", () => {
    const { calls, editor } = asked();
    drawCable(editor, connection("8", "audioOut", "5", "audioIn"));
    expect(calls).toEqual([["addAudioCable", 8, 5, null, false]]);
  });

  test("is an audio cable to the second input of an effect when it ends at that handle: the editor is asked for the second input", () => {
    const { calls, editor } = asked();
    drawCable(editor, connection("8", "audioOut", "5", "audioInSide"));
    expect(calls).toEqual([["addAudioCable", 8, 5, null, true]]);
  });

  test("is a cable of notes from a source's output of events to a lane's input of events: the editor is asked to draw it from the source to the lane", () => {
    const { calls, editor } = asked();
    drawCable(editor, connection("7", "eventsOut", "5", "eventsIn"));
    expect(calls).toEqual([["addNotesCable", 7, 5]]);
  });

  test("is a cable of modulation from a modulator's output to a port: the editor is asked to draw it from the modulator to the port of the node", () => {
    const { calls, editor } = asked();
    drawCable(editor, connection("2", "modOut", "1", "param-3"));
    drawCable(editor, connection("2", "modOut", "7", "rate"));
    expect(calls).toEqual([
      ["addCable", 2, 1, { type: "param", index: 3 }],
      ["addCable", 2, 7, { type: "rate" }],
    ]);
  });

  test("is no cable when it goes from one kind of port to another, or from or to no handle: the editor is asked for nothing", () => {
    const { calls, editor } = asked();
    for (const one of [connection("7", "eventsOut", "5", "audioIn"), connection("8", "audioOut", "5", "eventsIn"), connection("2", "modOut", "5", "eventsIn"), connection("7", "eventsOut", "1", "param-3"), connection("8", null, "5", "audioIn")]) {
      drawCable(editor, one);
    }
    expect(calls).toEqual([]);
  });
});

describe("a cable dragged to a volume, a mix or a pan that the host keeps", () => {
  const { snapshot, gains } = freeReachFixture;
  const { nodes } = toFlow(snapshot.graph);
  const modulators = snapshot.graph.nodes.filter((node) => node.kind === "modulator").map((node) => node.id);
  const volumes = (modulator: number): string[] => [...drawableFrom(nodes, modulator)].filter((key) => /:(volume|mix|pan)$/.test(key)).sort();

  test("is a port by the name of the value: the handle of a lane's or a group's volume, of an effect's mix, of a group's pan", () => {
    expect(handleOf({ type: "volume" })).toBe("volume");
    expect(handleOf({ type: "mix" })).toBe("mix");
    expect(handleOf({ type: "pan" })).toBe("pan");
    expect(cableOf({ source: "5", target: "7", sourceHandle: "modOut", targetHandle: "mix" })).toEqual({ modulator: 5, node: 7, port: { type: "mix" } });
    expect(cableOf({ source: "5", target: "7", sourceHandle: "modOut", targetHandle: "volume" })?.port).toEqual({ type: "volume" });
    expect(cableOf({ source: "5", target: "7", sourceHandle: "modOut", targetHandle: "pan" })?.port).toEqual({ type: "pan" });
    // (A port that is none of them is none: a drag that ends at what a node has not is no cable.)
    expect(cableOf({ source: "5", target: "7", sourceHandle: "modOut", targetHandle: "loudness" })).toBeNull();
  });

  test("can be drawn where the controller lets it be drawn, for every modulator", () => {
    for (const modulator of modulators) {
      const allowed = (gains[String(modulator)] ?? []).map((found) => keyOf(found.node, handleOf(found.port))).sort();
      expect(volumes(modulator), `modulator ${String(modulator)}`).toEqual(allowed);
    }
    expect(volumes(modulators[0] ?? -1).length).toBeGreaterThanOrEqual(10);
  });

  test("is a cable of a modulator that is dropped on the port, and is not on a node that has not the value, or on one that is driven already", () => {
    const [first, second] = modulators;
    const group = snapshot.graph.nodes.find((node) => node.kind === "group");
    const effect = snapshot.graph.nodes.find((node) => node.kind === "effect");
    const empty = snapshot.graph.nodes.find((node) => node.kind === "instrument" && node.plugin === null);
    if (first === undefined || second === undefined || !group || !effect || !empty) throw new Error("the session lacks what the test draws to");
    expect(canDraw(nodes, { modulator: first, node: group.id, port: { type: "pan" } })).toBe(true);
    expect(canDraw(nodes, { modulator: first, node: effect.id, port: { type: "mix" } })).toBe(true);
    expect(canDraw(nodes, { modulator: first, node: group.id, port: { type: "mix" } })).toBe(false);
    expect(canDraw(nodes, { modulator: first, node: effect.id, port: { type: "volume" } })).toBe(false);
    expect(canDraw(nodes, { modulator: first, node: empty.id, port: { type: "volume" } })).toBe(false);
    const driven = toFlow({ ...snapshot.graph, edges: [...snapshot.graph.edges, { kind: "modulation", id: 700, amount: 0.5, from: { node: first, port: { type: "modOut" } }, to: { node: group.id, port: { type: "pan" } } }] }).nodes;
    expect(canDraw(driven, { modulator: first, node: group.id, port: { type: "pan" } })).toBe(false);
    // (Another modulator may drive it too: the two add up.)
    expect(canDraw(driven, { modulator: second, node: group.id, port: { type: "pan" } })).toBe(true);
  });
});

