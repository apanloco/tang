import { describe, expect, test } from "vitest";

import { reachFixture, tracerSnapshot } from "../../client/fixtures";
import type { Port } from "../../protocol";
import { cableOf, canDraw, drawableFrom } from "./connection";
import { handleOf, toFlow } from "./toFlow";

describe("a connection that is dragged on the canvas", () => {
  const connection = (source: string, sourceHandle: string | null, target: string, targetHandle: string | null) => ({ source, sourceHandle, target, targetHandle });

  test("is a cable when it goes from a modulator's output to a parameter's port, by the ids of the nodes and the plugin's own index", () => {
    expect(cableOf(connection("2", "modOut", "1", "param-3"))).toEqual({ modulator: 2, node: 1, param: 3 });
    expect(cableOf(connection("12", "modOut", "30", "param-0"))).toEqual({ modulator: 12, node: 30, param: 0 });
  });

  test("is read with the handles' names as the canvas gives them (`handleOf`)", () => {
    const param: Port = { type: "param", index: 17 };
    expect(cableOf(connection("2", handleOf({ type: "modOut" }), "1", handleOf(param)))).toEqual({ modulator: 2, node: 1, param: 17 });
  });

  test("is not a cable when it starts or ends anywhere else", () => {
    const refused = [
      connection("2", "audioOut", "1", "param-3"),
      connection("2", "modOut", "1", "audioIn"),
      connection("2", "modOut", "1", "param-"),
      connection("2", "modOut", "1", "param-x"),
      connection("2", "modOut", "1", "param--1"),
      connection("2", "modOut", "1", "param-1.5"),
      connection("2", "modOut", "1", "xparam-3"),
      connection("2", "modOut", "1", "param_3"), // (what comes after the prefix is a number, and the prefix is not it)
      connection("2", null, "1", "param-3"),
      connection("2", "modOut", "1", null),
      connection("two", "modOut", "1", "param-3"),
      connection("2", "modOut", "one", "param-3"),
      connection("", "modOut", "1", "param-3"),
    ];
    for (const one of refused) expect(cableOf(one), JSON.stringify(one)).toBeNull();
  });

  test("can be drawn when the modulator can still drive that parameter, and not otherwise", () => {
    const { nodes } = toFlow(tracerSnapshot.graph);
    // The LFO (2) drives the filter's cutoff (1, 0) already; the oscillator's detune (0, 1) and the filter's resonance (1, 1) are free.
    expect(canDraw(nodes, { modulator: 2, node: 0, param: 1 })).toBe(true);
    expect(canDraw(nodes, { modulator: 2, node: 1, param: 1 })).toBe(true);
    expect(canDraw(nodes, { modulator: 2, node: 1, param: 0 })).toBe(false); // already
    expect(canDraw(nodes, { modulator: 2, node: 1, param: 99 })).toBe(false); // no such parameter
    expect(canDraw(nodes, { modulator: 2, node: 2, param: 0 })).toBe(false); // a modulator is no plugin
    expect(canDraw(nodes, { modulator: 0, node: 1, param: 1 })).toBe(false); // an oscillator is no modulator
    expect(canDraw(nodes, { modulator: 99, node: 1, param: 1 })).toBe(false); // nothing
  });

  test("can be drawn as far as the controller's reach goes: not to another lane, nor to another group", () => {
    const { nodes } = toFlow(reachFixture.snapshot.graph);
    // Lane 0's LFO (6) reaches its oscillator (4) and filter (5); not the second lane's oscillator (7), the bus filter (1) or a lane of its own (11).
    expect(canDraw(nodes, { modulator: 6, node: 4, param: 0 })).toBe(true);
    expect(canDraw(nodes, { modulator: 6, node: 5, param: 0 })).toBe(true);
    for (const node of [7, 1, 11, 9, 14]) expect(canDraw(nodes, { modulator: 6, node, param: 0 }), `to ${String(node)}`).toBe(false);
    // The group's LFO (15) reaches the members' plugins and the bus.
    for (const node of [1, 4, 5, 7]) expect(canDraw(nodes, { modulator: 15, node, param: 0 }), `to ${String(node)}`).toBe(true);
    for (const node of [3, 9, 10, 11, 12]) expect(canDraw(nodes, { modulator: 15, node, param: 0 }), `to ${String(node)}`).toBe(false);
  });

  test("lists what can be drawn from a modulator, as the nodes and parameters a port of a canvas is of", () => {
    const { nodes } = toFlow(tracerSnapshot.graph);
    const keys = drawableFrom(nodes, 2);
    expect(keys.has("0:1")).toBe(true);
    expect(keys.has("1:1")).toBe(true);
    expect(keys.has("1:0")).toBe(false);
    expect(keys.size).toBe(8 + 3);
    expect(drawableFrom(nodes, 0).size).toBe(0);
    expect(drawableFrom(nodes, 99).size).toBe(0);
  });
});
