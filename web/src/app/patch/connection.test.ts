import { describe, expect, test } from "vitest";

import { MASTER, audioCablesFixture, fieldsFixture, reachFixture, tracerSnapshot } from "../../client/fixtures";
import type { Port } from "../../protocol";
import { audioCableOf, audioTakersOf, cableOf, canConnect, canDraw, canDrawAudio, drawableFrom, keyOf, takersOf } from "./connection";
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

  test("can be drawn as far as the controller's reach goes: not to another lane, nor to another group", () => {
    const { nodes } = toFlow(reachFixture.snapshot.graph);
    // Lane 0's LFO (6) reaches its oscillator (4) and filter (5); not the second lane's oscillator (7), the bus filter (1) or a lane of its own (11).
    expect(canDraw(nodes, { modulator: 6, node: 4, port: param(0) })).toBe(true);
    expect(canDraw(nodes, { modulator: 6, node: 5, port: param(0) })).toBe(true);
    for (const node of [7, 1, 11, 9, 14]) expect(canDraw(nodes, { modulator: 6, node, port: param(0) }), `to ${String(node)}`).toBe(false);
    // The group's LFO (15) reaches the members' plugins and the bus.
    for (const node of [1, 4, 5, 7]) expect(canDraw(nodes, { modulator: 15, node, port: param(0) }), `to ${String(node)}`).toBe(true);
    for (const node of [3, 9, 10, 11, 12]) expect(canDraw(nodes, { modulator: 15, node, port: param(0) }), `to ${String(node)}`).toBe(false);
  });

  test("lists what can be drawn from a modulator, as the nodes and parameters a port of a canvas is of", () => {
    const { nodes } = toFlow(tracerSnapshot.graph);
    const keys = drawableFrom(nodes, 2);
    expect(keys.has("0:param-1")).toBe(true);
    expect(keys.has("1:param-1")).toBe(true);
    expect(keys.has("1:param-0")).toBe(false);
    expect(keys.size).toBe(20 + 3);
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
  // Group 1's rack has an LFO (13), an envelope (14) and a MIDI control (15); its first member (3) has an LFO (4), an envelope (5) and a MIDI control (6).
  const { nodes } = toFlow(fieldsFixture.snapshot.graph);
  const settings = (modulator: number): string[] => [...drawableFrom(nodes, modulator)].filter((key) => !key.includes("param-")).sort();

  test("can be drawn to the rate of an LFO and the stages of an envelope that the modulator reaches, and to nothing else of a modulator", () => {
    expect(canDraw(nodes, { modulator: 5, node: 4, port: { type: "rate" } })).toBe(true); // the lane's envelope to its LFO's rate
    expect(canDraw(nodes, { modulator: 4, node: 5, port: { type: "attack" } })).toBe(true);
    expect(canDraw(nodes, { modulator: 6, node: 5, port: { type: "release" } })).toBe(true);
    expect(canDraw(nodes, { modulator: 13, node: 14, port: { type: "decay" } })).toBe(true); // within a group's rack
    expect(canDraw(nodes, { modulator: 13, node: 8, port: { type: "rate" } })).toBe(true); // to a member's modulator
    expect(canDraw(nodes, { modulator: 4, node: 4, port: { type: "rate" } })).toBe(false); // itself
    expect(canDraw(nodes, { modulator: 4, node: 5, port: { type: "rate" } })).toBe(false); // an envelope has no rate
    expect(canDraw(nodes, { modulator: 5, node: 4, port: { type: "attack" } })).toBe(false); // an LFO has no attack
    expect(canDraw(nodes, { modulator: 5, node: 6, port: { type: "rate" } })).toBe(false); // a MIDI control has no settings
    expect(canDraw(nodes, { modulator: 4, node: 8, port: { type: "rate" } })).toBe(false); // another lane's, which a lane's modulator does not reach
    expect(canDraw(nodes, { modulator: 4, node: 13, port: { type: "rate" } })).toBe(false); // the group's
    expect(canDraw(nodes, { modulator: 13, node: 12, port: { type: "rate" } })).toBe(false); // a lane of another group
    expect(canDraw(nodes, { modulator: 13, node: 10, port: { type: "rate" } })).toBe(false); // a lane in no group
  });

  test("can be drawn to the depth of a cable of a modulator that the modulator reaches, and to no other", () => {
    // The lane's LFO (4) to the depth of the envelope's cables (1 and 2) and the MIDI control's (3), which it reaches; the envelope (5) to the LFO's (0).
    expect(canDraw(nodes, { modulator: 4, node: 5, port: { type: "depth", edge: 1 } })).toBe(true);
    expect(canDraw(nodes, { modulator: 4, node: 5, port: { type: "depth", edge: 2 } })).toBe(true);
    expect(canDraw(nodes, { modulator: 4, node: 6, port: { type: "depth", edge: 3 } })).toBe(true);
    expect(canDraw(nodes, { modulator: 5, node: 4, port: { type: "depth", edge: 0 } })).toBe(true);
    expect(canDraw(nodes, { modulator: 13, node: 14, port: { type: "depth", edge: 5 } })).toBe(true); // within a group's rack
    expect(canDraw(nodes, { modulator: 13, node: 4, port: { type: "depth", edge: 0 } })).toBe(true); // to a member's modulator
    expect(canDraw(nodes, { modulator: 4, node: 4, port: { type: "depth", edge: 0 } })).toBe(false); // the depth of its own cable
    expect(canDraw(nodes, { modulator: 4, node: 5, port: { type: "depth", edge: 0 } })).toBe(false); // a cable that is another modulator's
    expect(canDraw(nodes, { modulator: 4, node: 5, port: { type: "depth", edge: 99 } })).toBe(false); // no such cable
    expect(canDraw(nodes, { modulator: 4, node: 14, port: { type: "depth", edge: 5 } })).toBe(false); // the group's, which a lane's modulator does not reach
    expect(canDraw(nodes, { modulator: 4, node: 8, port: { type: "depth", edge: 5 } })).toBe(false); // another lane's, which has no such cable
    expect(canDraw(nodes, { modulator: 13, node: 12, port: { type: "depth", edge: 0 } })).toBe(false); // a lane of another group
    expect(canDraw(nodes, { modulator: 0, node: 5, port: { type: "depth", edge: 1 } })).toBe(false); // not a modulator
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
      edges: [...graph.edges, { kind: "modulation" as const, id: 99, amount: 0.5, from: { node: 5, port: { type: "modOut" as const } }, to: { node: 4, port: { type: "rate" as const } } }],
    };
    const after = toFlow(driven).nodes;
    expect(canDraw(nodes, { modulator: 5, node: 4, port: { type: "rate" } })).toBe(true);
    expect(canDraw(after, { modulator: 5, node: 4, port: { type: "rate" } })).toBe(false);
    // (Another modulator may drive it too: the two add up.)
    expect(canDraw(after, { modulator: 6, node: 4, port: { type: "rate" } })).toBe(true);
  });

  test("is not a port of the depth it drives already, and the depths of the other cables of that modulator are other ports", () => {
    const graph = fieldsFixture.snapshot.graph;
    const driven = {
      ...graph,
      edges: [...graph.edges, { kind: "modulation" as const, id: 99, amount: 0.5, from: { node: 4, port: { type: "modOut" as const } }, to: { node: 5, port: { type: "depth" as const, edge: 1 } } }],
    };
    const after = toFlow(driven).nodes;
    expect(canDraw(nodes, { modulator: 4, node: 5, port: { type: "depth", edge: 1 } })).toBe(true);
    expect(canDraw(after, { modulator: 4, node: 5, port: { type: "depth", edge: 1 } })).toBe(false);
    expect(canDraw(after, { modulator: 4, node: 5, port: { type: "depth", edge: 2 } })).toBe(true);
    // (Another modulator may drive it too: the two add up.)
    expect(canDraw(after, { modulator: 6, node: 5, port: { type: "depth", edge: 1 } })).toBe(true);
  });
});

describe("an audio cable dragged on the canvas", () => {
  const connection = (source: string, sourceHandle: string | null, target: string, targetHandle: string | null) => ({ source, sourceHandle, target, targetHandle });
  // The session of the audio cables fixture: group 0 (Pad) with Filter 1, Reverb 2 and Delay 3 on its bus; lane 4 in it with Filter 5 and Delay 6; lane 8 with Drive 9; empty lane 10.
  // The cables: 1 from lane 8 to Filter 5, 2 from Filter 5 to Delay 6, 3 from the group's output to Drive 9, 4 from Filter 1 to Reverb 2, 5 from Delay 6 to the group's bus, 6 from Reverb 2 to the mix.
  const { graph } = audioCablesFixture.snapshot;

  test("is read when it goes from an output of audio to an input of audio, by the ids of the nodes", () => {
    expect(audioCableOf(connection("8", "audioOut", "5", "audioIn"))).toEqual({ from: 8, to: 5 });
    expect(audioCableOf(connection("0", "audioOut", String(MASTER), "audioIn"))).toEqual({ from: 0, to: MASTER });
    expect(audioCableOf(connection("12", "audioOut", "30", "audioIn"))).toEqual({ from: 12, to: 30 });
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
    expect(canDrawAudio(graph, { from: 8, to: 9 })).toBe(true); // a lane's own effect, beside the chain's link
    expect(canDrawAudio(graph, { from: 8, to: 1 })).toBe(true); // a bus effect
    expect(canDrawAudio(graph, { from: 8, to: MASTER })).toBe(true);
    expect(canDrawAudio(graph, { from: 8, to: 5 })).toBe(false); // there already
    expect(canDrawAudio(graph, { from: 9, to: 5 })).toBe(false); // round
    expect(canDrawAudio(graph, { from: 0, to: 0 })).toBe(false); // a group's output to its own bus
    expect(canDrawAudio(graph, { from: MASTER, to: 5 })).toBe(false); // the mix has no output
    expect(canDrawAudio(graph, { from: 8, to: 10 })).toBe(false); // a lane has no input
    expect(canDrawAudio(graph, { from: 7, to: 5 })).toBe(false); // a modulator has no audio
    expect(canDrawAudio(graph, { from: 8, to: 77 })).toBe(false); // nothing
    expect(canDrawAudio(graph, { from: 77, to: 5 })).toBe(false);
  });

  test("has, for the ports of the canvas, the inputs that take a cable from an output, by the name of the node and of the handle", () => {
    expect([...audioTakersOf(graph, 8)].sort()).toEqual(["0:audioIn", "1:audioIn", "2:audioIn", "3:audioIn", `${String(MASTER)}:audioIn`, "6:audioIn", "9:audioIn"].sort());
    expect([...audioTakersOf(graph, 0)]).toEqual([`${String(MASTER)}:audioIn`]);
    expect(audioTakersOf(graph, 7).size).toBe(0);
    expect(audioTakersOf(graph, MASTER).size).toBe(0);
    expect(audioTakersOf(graph, 77).size).toBe(0);
    // (A key of an input is not that of a parameter's or a setting's port of the same node.)
    expect(keyOf(9, "audioIn")).toBe("9:audioIn");
    expect(keyOf(9, "audioIn")).not.toBe(keyOf(9, "audioOut"));
  });

  test("is drawn from the tracer's oscillator to its filter and to the mix, and to nothing else", () => {
    // (The tracer: oscillator 0, filter 1, LFO 2: a cable from the oscillator may end at the filter, beside the link of the chain, and at the mix.)
    expect([...audioTakersOf(tracerSnapshot.graph, 0)].sort()).toEqual(["1:audioIn", `${String(MASTER)}:audioIn`].sort());
    expect([...audioTakersOf(tracerSnapshot.graph, 1)]).toEqual([`${String(MASTER)}:audioIn`]);
  });
});

describe("a drag that starts at a handle of the canvas", () => {
  const audio = audioCablesFixture.snapshot.graph;
  const tracer = tracerSnapshot.graph;
  const start = (nodeId: string | null, handleType: "source" | "target" | null, handleId: string | null) => ({ nodeId, handleType, handleId });
  const connection = (source: string, sourceHandle: string | null, target: string, targetHandle: string | null) => ({ source, sourceHandle, target, targetHandle });

  test("from an output of audio can end at the inputs that take audio from it, by the name of the node and of the handle, whatever the canvas has laid out", () => {
    const keys = takersOf(audio, [], start("8", "source", "audioOut"));
    expect([...(keys ?? [])].sort()).toEqual([...audioTakersOf(audio, 8)].sort());
    expect(keys?.has("9:audioIn")).toBe(true);
    expect(keys?.has("5:audioIn")).toBe(false); // there is a cable there already
    expect([...(takersOf(audio, [], start("0", "source", "audioOut")) ?? [])]).toEqual([`${String(MASTER)}:audioIn`]);
  });

  test("from a modulator's output can end at the ports it can still drive", () => {
    const { nodes } = toFlow(tracer);
    expect([...(takersOf(tracer, nodes, start("2", "source", "modOut")) ?? [])].sort()).toEqual([...drawableFrom(nodes, 2)].sort());
    expect(takersOf(tracer, nodes, start("2", "source", "modOut"))?.has("0:param-1")).toBe(true);
  });

  test("from an output that nothing is drawn from ends nowhere, and the set that says so is empty and not none: a cable is being dragged", () => {
    expect(takersOf(audio, [], start(String(MASTER), "source", "audioOut"))?.size).toBe(0);
    expect(takersOf(audio, [], start("7", "source", "audioOut"))?.size).toBe(0);
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
    expect(canConnect(audio, [], connection("8", "audioOut", "9", "audioIn"))).toBe(true);
    expect(canConnect(audio, [], connection("8", "audioOut", String(MASTER), "audioIn"))).toBe(true);
    expect(canConnect(audio, [], connection("8", "audioOut", "5", "audioIn"))).toBe(false); // there already
    expect(canConnect(audio, [], connection("9", "audioOut", "5", "audioIn"))).toBe(false); // round
    expect(canConnect(audio, [], connection("8", "audioOut", "10", "audioIn"))).toBe(false); // a lane has no input
    expect(canConnect(tracer, nodes, connection("2", "modOut", "0", "param-1"))).toBe(true);
    expect(canConnect(tracer, nodes, connection("2", "modOut", "1", "param-0"))).toBe(false); // already driven
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
