import { describe, expect, test } from "vitest";

import { fieldsFixture, reachFixture, tracerSnapshot } from "../../client/fixtures";
import type { Port } from "../../protocol";
import { cableOf, canDraw, drawableFrom, keyOf } from "./connection";
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

  test("is read with the handles' names as the canvas gives them (`handleOf`)", () => {
    expect(cableOf(connection("2", handleOf({ type: "modOut" }), "1", handleOf(param(17))))).toEqual({ modulator: 2, node: 1, port: param(17) });
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
      connection("2", "modOut", "1", "depth"), // (a cable's depth has no port on the canvas)
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
    expect(keys.size).toBe(8 + 3);
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
    expect(canDraw(nodes, { modulator: 4, node: 5, port: { type: "depth", edge: 1 } })).toBe(false); // (a depth has no port on the canvas)
  });

  test("is what the controller allows, but for the depths of cables, which the canvas has no port for", () => {
    for (const [id, listed] of Object.entries(fieldsFixture.fields)) {
      const settingsListed = listed.filter((field) => field.port.type !== "depth").map((field) => keyOf(field.node, handleOf(field.port) ?? "?"));
      expect(settings(Number(id)), `modulator ${id}`).toEqual(settingsListed.sort());
    }
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
});
