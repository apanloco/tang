import { describe, expect, test } from "vitest";

import type { CatalogEntry, Intent, ServerMessage, Snapshot, TelemetryLayout } from "../protocol";
import { builtinCatalog, modulesFixture, tracerSnapshot } from "./fixtures";
import { MockServer } from "./mock";
import { cleanModuleName, moduleEntry, moduleId, moduleNameOf, moduleOf, withModules } from "./modules";
import type { Module } from "./modules";

// Chains of effects saved as modules (`Intent::SaveModule`), against the mock engine: what the controller does (`save_module_intent`, `load_module`), with a library that is kept in memory.
// `fixtures/modules.json` is what the controller made of a script on a session of its own, step by step; the mock is held to it here.

/** The layout of the telemetry with the same fingerprint, which the controller's comes from a hasher and the mock's from a counter. */
const layoutOf = (layout: TelemetryLayout): TelemetryLayout => ({ ...layout, rev: 0 });

const quiet = (ms = 40): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

const refusedWith = (server: MockServer, intent: Intent): string => {
  try {
    server.apply(intent);
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  }
  return "done";
};

describe("the mock engine's library of modules", () => {
  test("is what the controller makes of a chain that is saved and added, undone and done again, step by step: the events, the nodes, the cables, the layout of the telemetry, what Undo and Redo would do, and the modules the catalog lists", () => {
    const server = new MockServer({ snapshot: modulesFixture.snapshot });
    for (const [i, step] of modulesFixture.steps.entries()) {
      const told = server.apply(step.intent);
      const what = `step ${i + 1}: ${JSON.stringify(step.intent)}`;
      expect(told.events, what).toEqual(step.events);
      expect(told.restructured, what).toBe(step.restructured);
      expect(server.history, what).toEqual(step.history);
      expect(server.snapshot.graph, what).toEqual(step.graph);
      expect(layoutOf(server.snapshot.telemetry), what).toEqual(layoutOf(step.telemetry));
    }
  });

  test("lists the modules to a client that subscribes, with the plugins, and tells the catalog again when one is saved that was not there or that has another number of effects, and not for one that is saved again as it was", async () => {
    const server = new MockServer({ snapshot: modulesFixture.snapshot });
    const heard: ServerMessage[] = [];
    const transport = server.transport();
    transport.connect({
      open: () => undefined,
      text: (data) => {
        heard.push(JSON.parse(data) as ServerMessage);
      },
      binary: () => undefined,
      close: () => undefined,
    });
    transport.send(JSON.stringify({ type: "subscribe" }));
    await quiet();
    const catalogs = (): Extract<ServerMessage, { type: "catalog" }>[] => heard.filter((message): message is Extract<ServerMessage, { type: "catalog" }> => message.type === "catalog");
    const modulesOf = (message: Extract<ServerMessage, { type: "catalog" }> | undefined): CatalogEntry[] => message?.entries.filter((entry) => entry.format === "Module") ?? [];
    expect(catalogs()).toHaveLength(1);
    expect(modulesOf(catalogs().at(-1))).toEqual([]);

    const [first, again] = modulesFixture.steps.map((step) => step.intent);
    if (!first || !again) throw new Error("the fixture saves a module twice");
    server.apply(first);
    await quiet();
    expect(catalogs()).toHaveLength(2);
    expect(modulesOf(catalogs().at(-1))).toEqual([{ id: "module:Room", name: "Room", format: "Module", isInstrument: false, params: 3, presets: 0 }]);
    // The plugins are still there, and the module is among them by its name.
    expect(catalogs().at(-1)?.entries.length).toBeGreaterThan(1);
    // (The same module again: the same entry, which is no news.)
    server.apply(again);
    await quiet();
    expect(catalogs()).toHaveLength(2);
    transport.close();
  });

  test("turns away what the controller turns away, in its words, and changes nothing", () => {
    const wrong: string[] = [];
    for (const row of modulesFixture.refusals) {
      const server = new MockServer({ snapshot: modulesFixture.snapshot });
      const got = refusedWith(server, row.intent);
      if (got !== row.error) wrong.push(`${JSON.stringify(row.intent)}: ${got}, and the controller says ${row.error}`);
      expect(server.snapshot, JSON.stringify(row.intent)).toBe(modulesFixture.snapshot);
    }
    expect(wrong).toEqual([]);
  });

  test("is nothing a refusal uses: a module that cannot be added leaves no lane and uses no id, whatever the reason", () => {
    const library: Record<string, Module> = {
      Needs: { effects: [{ plugin: "builtin:filter", preset: null, params: {}, mix: 1, pinned: [], fed: true }, { plugin: "clap:com.example.nothing", preset: null, params: {}, mix: 1, pinned: [], fed: true }], cables: [] },
      Plays: { effects: [{ plugin: "builtin:osc", preset: null, params: {}, mix: 1, pinned: [], fed: true }], cables: [] },
    };
    const server = new MockServer({ snapshot: tracerSnapshot, modules: library });
    const nodes = server.snapshot.graph.nodes.length;
    expect(refusedWith(server, { type: "addFreeEffect", plugin: "module:Needs" })).toBe("the module Needs cannot be added: clap:com.example.nothing is not a plugin that is installed");
    expect(refusedWith(server, { type: "addFreeEffect", plugin: "module:Plays" })).toBe("the module Plays cannot be added: Oscillator is an instrument, not an effect");
    expect(server.snapshot.graph.nodes).toHaveLength(nodes);
    // The next lane that is made has the ids that the refusals left: the first free one.
    server.apply({ type: "addFreeEffect", plugin: "builtin:reverb" });
    const lane = server.snapshot.graph.nodes.findLast((node) => node.kind === "instrument");
    expect(lane?.id).toBe(1 + Math.max(...tracerSnapshot.graph.nodes.filter((node) => node.kind !== "master").map((node) => node.id)));
  });
});

describe("the names and the entries of modules", () => {
  test("a module is known by the id that the catalog gives it, and a plugin is not", () => {
    expect(moduleId("Space")).toBe("module:Space");
    expect(moduleNameOf("module:Space")).toBe("Space");
    expect(moduleNameOf("module:")).toBe("");
    expect(moduleNameOf("builtin:reverb")).toBeNull();
    expect(moduleNameOf("clap:com.example.module:x")).toBeNull();
  });

  test("the name of a module is a plain file name without its extension, and not one that ends in it twice", () => {
    expect(cleanModuleName("Space")).toEqual({ name: "Space" });
    expect(cleanModuleName("  Space  ")).toEqual({ name: "Space" });
    expect(cleanModuleName("Space.toml")).toEqual({ name: "Space" });
    expect(cleanModuleName("Big room 2")).toEqual({ name: "Big room 2" });
    expect(cleanModuleName("a.tomlx")).toEqual({ name: "a.tomlx" });
    expect(cleanModuleName("x.toml ")).toEqual({ name: "x" });
    for (const bad of ["", "   ", ".toml", "a/b", "..\\b", "a:b", "what?", "con", "tab\tname", "x".repeat(300)]) {
      expect(cleanModuleName(bad), JSON.stringify(bad)).toHaveProperty("error");
    }
    expect(cleanModuleName("a.toml.toml")).toEqual({ error: "the name of a module does not end in .toml: that is the extension of its file" });
  });

  test("a module's entry is no instrument, has as many effects as parameters and no presets", () => {
    const module: Module = { effects: [{ plugin: "builtin:filter", preset: null, params: {}, mix: 1, pinned: [], fed: true }], cables: [] };
    expect(moduleEntry("Space", module)).toEqual({ id: "module:Space", name: "Space", format: "Module", isInstrument: false, params: 1, presets: 0 });
  });

  test("the catalog lists the modules with the plugins by name without regard to case, a plugin before a module that has its name, and leaves what it was given alone", () => {
    const plugins: CatalogEntry[] = [
      { id: "clap:zeta", name: "Zeta", format: "CLAP", isInstrument: false, params: 1, presets: 0 },
      { id: "builtin:space", name: "Space", format: "Built-in", isInstrument: false, params: 1, presets: 0 },
    ];
    const copy = [...plugins];
    const one: Module = { effects: [{ plugin: "builtin:filter", preset: null, params: {}, mix: 1, pinned: [], fed: true }], cables: [] };
    const listed = withModules(plugins, new Map([["hall", one], ["Space", one]]));
    expect(listed.map((entry) => entry.id)).toEqual(["module:hall", "builtin:space", "module:Space", "clap:zeta"]);
    expect(plugins).toEqual(copy);
    expect(withModules(builtinCatalog, new Map())).toEqual(builtinCatalog);
  });
});

describe("the chain that a module is made of", () => {
  const snapshot: Snapshot = modulesFixture.snapshot;
  const named = (name: string) => snapshot.graph.nodes.find((node) => node.name === name && node.kind === "instrument");
  const lane = snapshot.graph.nodes.find((node) => node.kind === "instrument" && node.plugin !== null);
  const empty = snapshot.graph.nodes.find((node) => node.kind === "instrument" && node.plugin === null);

  test("is the effects of a lane in order, each with what a file says of it: the preset, the values over it, the mix, the knobs pinned and whether it is fed; and the cables between them and no others", () => {
    if (!lane) throw new Error("the fixture has a lane");
    const module = moduleOf(snapshot, lane);
    if (typeof module === "string") throw new Error(module);
    expect(module.effects.map((effect) => effect.plugin)).toEqual(["builtin:filter", "builtin:reverb", "builtin:math"]);
    expect(module.effects[0]).toEqual({ plugin: "builtin:filter", preset: null, params: { cutoff: 0.25 }, mix: 0.8, pinned: ["resonance", "cutoff"], fed: true });
    expect(module.effects[1]).toMatchObject({ plugin: "builtin:reverb", preset: "Cathedral", mix: 0.4, pinned: ["size"], fed: false });
    expect(module.effects[1]?.params.size).toBe(0.9);
    expect(module.effects[2]?.params).toEqual({ mode: 4 });
    // (Two cables between the effects, one beside a link that is cut and one at the second input of the math; the cable from the instrument is the session's.)
    expect(module.cables).toEqual([
      { from: 0, to: 1, side: false, gain: 0.5 },
      { from: 0, to: 2, side: true, gain: 0.7 },
    ]);
  });

  test("is the same from an effect of the chain as from its lane, and is refused for what is no chain, in the controller's words", () => {
    if (!lane) throw new Error("the fixture has a lane");
    const effect = snapshot.graph.nodes.find((node) => node.kind === "effect" && node.parent === lane.id);
    if (!effect) throw new Error("the fixture has an effect");
    expect(moduleOf(snapshot, effect)).toEqual(moduleOf(snapshot, lane));
    const group = snapshot.graph.nodes.find((node) => node.kind === "group");
    const bus = snapshot.graph.nodes.find((node) => node.kind === "effect" && node.parent === group?.id);
    const modulator = snapshot.graph.nodes.find((node) => node.kind === "modulator");
    const master = snapshot.graph.nodes.find((node) => node.kind === "master");
    if (!group || !bus || !modulator || !master || !empty) throw new Error("the fixture has a group with a bus effect, a modulator, a master and an empty lane");
    expect(moduleOf(snapshot, group)).toBe("a module is the chain of a lane: a group's bus is not saved");
    expect(moduleOf(snapshot, bus)).toBe("a module is the chain of a lane: a group's bus is not saved");
    expect(moduleOf(snapshot, modulator)).toBe("a module is made of the chain of a lane");
    expect(moduleOf(snapshot, master)).toBe("a module is made of the chain of a lane");
    expect(moduleOf(snapshot, empty)).toBe("a lane that has no effect has no chain to save");
    expect(named("nothing")).toBeUndefined();
  });
});
