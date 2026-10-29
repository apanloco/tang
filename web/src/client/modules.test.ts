import { describe, expect, test } from "vitest";

import type { CatalogEntry, Intent, ServerMessage, Snapshot, TelemetryLayout } from "../protocol";
import { builtinCatalog, MASTER, moduleGraphsFixture, modulesFixture, tracerSnapshot } from "./fixtures";
import { MockServer } from "./mock";
import { chainOf, cleanModuleName, moduleEntry, moduleId, moduleNameOf, moduleOf, moduleOfNodes, pluginCount, withModules } from "./modules";
import type { Module, ModuleEffect, ModuleLane } from "./modules";

// Chains of effects saved as modules (`Intent::SaveModule`), against the mock engine: what the controller does (`save_module_intent`, `load_module`), with a library that is kept in memory.
// `fixtures/modules.json` is what the controller made of a script on a session of its own, step by step; the mock is held to it here.

/** The layout of the telemetry with the same fingerprint, which the controller's comes from a hasher and the mock's from a counter. */
const layoutOf = (layout: TelemetryLayout): TelemetryLayout => ({ ...layout, rev: 0 });

/** A module of one lane that has no instrument and these effects, and no cables: the chain that a module was before it was a graph. */
const chain = (...effects: ModuleEffect[]): Module => ({ lanes: [{ instrument: null, effects }], cables: [] });

const plain = (plugin: string): ModuleEffect => ({ plugin, preset: null, params: {}, mix: 1, pinned: [], fed: true });

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

  test("adds a module that a hand wrote as the controller adds it: a preset that the plugin has not, a parameter it has not, a value beyond its range, a choice that is not a whole number, a mix beyond 1, and knobs that are named twice or name nothing", () => {
    // (This is what the file `modulesFixture.odd.file` says, as the library of the mock keeps a module.)
    const odd = chain({ plugin: "builtin:filter", preset: "No such preset", params: { cutoff: 7, type: 2.6, "no such parameter": 1 }, mix: 1.5, pinned: ["resonance", "no such parameter", "resonance", "drive"], fed: true });
    const server = new MockServer({ snapshot: modulesFixture.snapshot, modules: { Odd: odd } });
    const { step } = modulesFixture.odd;
    const told = server.apply(step.intent);
    expect(told.events).toEqual(step.events);
    expect(server.history).toEqual(step.history);
    expect(server.snapshot.graph).toEqual(step.graph);
    expect(layoutOf(server.snapshot.telemetry)).toEqual(layoutOf(step.telemetry));
    // In words as well, so that a fixture that is written again cannot take the rules away unseen: the value is held to the range and the choice is a whole number, the mix is held to 1, the preset that is not
    // there is no preset, and the knobs are the plugin's own, once each, in the order they were named.
    const filter = server.snapshot.graph.nodes.findLast((node) => node.kind === "effect");
    if (filter?.kind !== "effect") throw new Error("the module has an effect");
    const value = (name: string): number | undefined => filter.plugin.params.find((p) => p.name === name)?.value;
    expect([value("cutoff"), value("type"), filter.mix]).toEqual([1, 3, 1]);
    expect(filter.plugin.pinned).toEqual([1, 3]);
    expect(filter.plugin.currentPreset).toBeNull();
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
    // (A connection that has not subscribed is told nothing of the library.)
    const idle: ServerMessage[] = [];
    const bystander = server.transport();
    bystander.connect({
      open: () => undefined,
      text: (data) => {
        idle.push(JSON.parse(data) as ServerMessage);
      },
      binary: () => undefined,
      close: () => undefined,
    });
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
    // A module that is saved again under the same name with another number of effects is another entry: the catalog is told again.
    server.apply({ type: "addFreeEffect", plugin: "builtin:delay" });
    const holder = server.snapshot.graph.nodes.findLast((node) => node.kind === "instrument");
    if (holder?.kind !== "instrument") throw new Error("a lane was added");
    server.apply({ type: "saveModule", node: holder.id, name: "Room" });
    await quiet();
    expect(catalogs()).toHaveLength(3);
    expect(modulesOf(catalogs().at(-1))).toEqual([{ id: "module:Room", name: "Room", format: "Module", isInstrument: false, params: 1, presets: 0 }]);
    expect(idle.filter((message) => message.type === "catalog")).toEqual([]);
    transport.close();
    bystander.close();
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
      Needs: chain(plain("builtin:filter"), plain("clap:com.example.nothing")),
      Plays: chain(plain("builtin:osc")),
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
    const module = chain(plain("builtin:filter"));
    expect(moduleEntry("Space", module)).toEqual({ id: "module:Space", name: "Space", format: "Module", isInstrument: false, params: 1, presets: 0 });
  });

  test("the catalog lists the modules with the plugins by name without regard to case, a plugin before a module that has its name, and leaves what it was given alone", () => {
    const plugins: CatalogEntry[] = [
      { id: "clap:zeta", name: "Zeta", format: "CLAP", isInstrument: false, params: 1, presets: 0 },
      { id: "builtin:space", name: "Space", format: "Built-in", isInstrument: false, params: 1, presets: 0 },
    ];
    const copy = [...plugins];
    const one = chain(plain("builtin:filter"));
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
    expect(module.lanes).toHaveLength(1);
    const effects = module.lanes[0]?.effects ?? [];
    expect(module.lanes[0]?.instrument).toBeNull();
    expect(effects.map((effect) => effect.plugin)).toEqual(["builtin:filter", "builtin:reverb", "builtin:math"]);
    expect(effects[0]).toEqual({ plugin: "builtin:filter", preset: null, params: { cutoff: 0.25 }, mix: 0.8, pinned: ["resonance", "cutoff"], fed: true });
    expect(effects[1]).toMatchObject({ plugin: "builtin:reverb", preset: "Cathedral", mix: 0.4, pinned: ["size"], fed: false });
    expect(effects[1]?.params.size).toBe(0.9);
    expect(effects[2]?.params).toEqual({ mode: 4 });
    // (Two cables between the effects, one beside a link that is cut and one at the second input of the math; the cable from the instrument is the session's.)
    expect(module.cables).toEqual([
      { from: { lane: 0, effect: 0 }, to: { lane: 0, effect: 1, side: false }, gain: 0.5 },
      { from: { lane: 0, effect: 0 }, to: { lane: 0, effect: 2, side: true }, gain: 0.7 },
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

describe("the mock engine's library of graph modules", () => {
  const fixture = moduleGraphsFixture;

  test("is what the controller makes of plugins that are chosen and saved, added, undone and done again, step by step: the events, the nodes, the cables, the layout of the telemetry, what Undo and Redo would do, and the modules the catalog lists", () => {
    const server = new MockServer({ snapshot: fixture.snapshot });
    for (const [i, step] of fixture.steps.entries()) {
      const told = server.apply(step.intent);
      const what = `step ${String(i + 1)}: ${JSON.stringify(step.intent)}`;
      expect(told.events, what).toEqual(step.events);
      expect(told.restructured, what).toBe(step.restructured);
      expect(server.history, what).toEqual(step.history);
      expect(server.snapshot.graph, what).toEqual(step.graph);
      expect(layoutOf(server.snapshot.telemetry), what).toEqual(layoutOf(step.telemetry));
    }
  });

  test("lists as the controller lists them: by how many plugins each has, the instruments and the effects", async () => {
    const server = new MockServer({ snapshot: fixture.snapshot });
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
    for (const step of fixture.steps) {
      server.apply(step.intent);
      await quiet(5);
      // (The catalog is told again when what it lists changes, so the last that was told is what it lists now.)
      const catalog = heard.findLast((message): message is Extract<ServerMessage, { type: "catalog" }> => message.type === "catalog");
      const listed = catalog?.entries.filter((entry) => entry.format === "Module").map((entry) => ({ id: entry.id, name: entry.name, params: entry.params }));
      expect(listed, JSON.stringify(step.intent)).toEqual(step.modules);
    }
    transport.close();
  });

  test("turns away what the controller turns away when plugins are chosen, in its words, and changes nothing", () => {
    expect(fixture.refusals.length).toBeGreaterThan(20);
    const wrong: string[] = [];
    for (const row of fixture.refusals) {
      const server = new MockServer({ snapshot: fixture.snapshot });
      const got = refusedWith(server, row.intent);
      if (got !== row.error) wrong.push(`${JSON.stringify(row.intent)}: ${got}, and the controller says ${row.error}`);
      expect(server.snapshot, JSON.stringify(row.intent)).toBe(fixture.snapshot);
    }
    expect(wrong).toEqual([]);
  });

  test("adds a module that a hand wrote that has an instrument as the controller adds it: a volume beyond its range, a preset that the plugin has not, a value beyond its range, a choice that is not a whole number, knobs named twice or that name nothing, and a cable beyond the level a cable has", () => {
    // (This is what the file `fixture.odd.file` says, as the library of the mock keeps a module.)
    const odd: Module = {
      lanes: [
        { instrument: { plugin: "builtin:osc", preset: "No such preset", params: { detune: 99, waveform: 1.6, "no such parameter": 1 }, volume: 7.5, pinned: ["no such parameter", "detune", "detune"] }, effects: [] },
        { instrument: null, effects: [plain("builtin:filter")] },
      ],
      cables: [{ from: { lane: 0, effect: null }, to: { lane: 1, effect: 0, side: false }, gain: 3 }],
    };
    const server = new MockServer({ snapshot: fixture.snapshot, modules: { OddVoice: odd } });
    const { step } = fixture.odd;
    const told = server.apply(step.intent);
    expect(told.events).toEqual(step.events);
    expect(server.history).toEqual(step.history);
    expect(server.snapshot.graph).toEqual(step.graph);
    expect(layoutOf(server.snapshot.telemetry)).toEqual(layoutOf(step.telemetry));
    // In words as well, so that a fixture that is written again cannot take the rules away unseen: the volume is held to 4, the value to its range, the choice is a whole number, the preset that is not there is no preset, the
    // knobs are the plugin's own and once each, the cable's level is held to 2, and the lane is bare.
    const lane = server.snapshot.graph.nodes.findLast((node) => node.kind === "instrument" && node.plugin !== null);
    if (lane?.kind !== "instrument" || !lane.plugin) throw new Error("the module has an instrument");
    const value = (name: string): number | undefined => lane.plugin?.params.find((p) => p.name === name)?.value;
    expect([lane.volume, value("detune"), value("waveform"), lane.route]).toEqual([4, 24, 2, "none"]);
    expect(lane.plugin.pinned).toEqual([lane.plugin.params.find((p) => p.name === "detune")?.index]);
    expect(lane.plugin.currentPreset).toBeNull();
    const cable = server.snapshot.graph.edges.findLast((edge) => edge.kind === "cable");
    expect(cable?.kind === "cable" ? cable.gain : null).toBe(2);
  });

  test("turns away a module that a hand wrote whose plugins cannot be loaded, in the controller's words, whichever lane the fault is in, and makes no lane", () => {
    const effects = (...plugins: string[]): ModuleLane => ({ instrument: null, effects: plugins.map(plain) });
    const instrument = (plugin: string): ModuleLane["instrument"] => ({ plugin, preset: null, params: {}, volume: 1, pinned: [] });
    const library: Record<string, Module> = {
      // The second lane's second plugin is not installed: the first lane is not made.
      Late: { lanes: [effects("builtin:reverb"), effects("builtin:filter", "clap:com.example.nothing")], cables: [] },
      // An instrument slot that names an effect, an effect slot that names an instrument, and an instrument that is not installed.
      Wrong: { lanes: [{ instrument: instrument("builtin:filter"), effects: [] }], cables: [] },
      Backwards: { lanes: [effects("builtin:reverb"), effects("builtin:osc")], cables: [] },
      Missing: { lanes: [{ instrument: instrument("clap:com.example.nothing"), effects: [plain("builtin:reverb")] }], cables: [] },
    };
    const server = new MockServer({ snapshot: fixture.snapshot, modules: library });
    const graph = server.snapshot.graph;
    const refused = (name: string): string => refusedWith(server, { type: "addFreeEffect", plugin: `module:${name}` });
    expect(refused("Late")).toBe("the module Late cannot be added: clap:com.example.nothing is not a plugin that is installed");
    expect(refused("Wrong")).toBe("the module Wrong cannot be added: Filter is an effect, not an instrument");
    expect(refused("Backwards")).toBe("the module Backwards cannot be added: Oscillator is an instrument, not an effect");
    expect(refused("Missing")).toBe("the module Missing cannot be added: clap:com.example.nothing is not a plugin that is installed");
    expect(server.snapshot.graph).toBe(graph);
  });

  test("says over the wire, with the words, that a node is not found when a chosen list names one that is not there, the master too, and that what a module cannot be made of is invalid", async () => {
    const server = new MockServer({ snapshot: fixture.snapshot });
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
    // (`apply` throws the words alone: the code is what the server's answer has.)
    const refusal = async (nodes: number[]): Promise<string[]> => {
      heard.length = 0;
      transport.send(JSON.stringify({ type: "request", reqId: 1, intent: { type: "saveSelectionAsModule", nodes, name: "Room" } satisfies Intent }));
      await quiet(5);
      const answer = heard.find((message) => message.type === "rejected");
      return answer?.type === "rejected" ? [answer.error.code, answer.error.message] : [];
    };
    expect(await refusal([MASTER])).toEqual(["notFound", `there is no node ${String(MASTER)}`]);
    expect(await refusal([9_999])).toEqual(["notFound", "there is no node 9999"]);
    expect(await refusal([])).toEqual(["invalid", "nothing is chosen: a module is made of the effects and instruments that are"]);
    transport.close();
  });

  test("tells the catalog again when a module is saved that was not there or that has another number of plugins, and not for one that is saved again as it was", async () => {
    const server = new MockServer({ snapshot: fixture.snapshot });
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
    const catalogs = (): number => heard.filter((message) => message.type === "catalog").length;
    const [pair, , again, tone] = fixture.steps.map((step) => step.intent);
    if (!pair || !again || !tone) throw new Error("the fixture saves a pair, the same again and a tone");
    const before = catalogs();
    server.apply(pair);
    await quiet();
    expect(catalogs()).toBe(before + 1);
    // (The same plugins under the same name, as they are: the same entry, which is no news.)
    server.apply(again);
    await quiet();
    expect(catalogs()).toBe(before + 1);
    // Another module is news, and so is the same name with another number of plugins.
    server.apply(tone);
    await quiet();
    expect(catalogs()).toBe(before + 2);
    const nodes = fixture.snapshot.graph.nodes;
    const osc = nodes.find((node) => node.kind === "instrument" && node.plugin !== null);
    const filter = nodes.find((node) => node.kind === "effect" && node.name === "Filter" && node.parent === osc?.id);
    const math = nodes.find((node) => node.kind === "effect" && node.name === "Math");
    if (!filter || !osc || !math) throw new Error("the session has a filter, an oscillator and a math effect");
    // (Two other plugins under the name that has two is not news either: the entry says how many, and not which.)
    server.apply({ type: "saveSelectionAsModule", nodes: [filter.id, osc.id], name: "Pair" });
    await quiet();
    expect(catalogs()).toBe(before + 2);
    server.apply({ type: "saveSelectionAsModule", nodes: [filter.id, osc.id, math.id], name: "Pair" });
    await quiet();
    expect(catalogs()).toBe(before + 3);
    transport.close();
  });
});

describe("the plugins that a module is made of", () => {
  const snapshot: Snapshot = moduleGraphsFixture.snapshot;
  const nodeOf = (name: string, kind: "instrument" | "effect", parent?: number) => {
    const found = snapshot.graph.nodes.find((node) => node.kind === kind && node.name === name && (parent === undefined || node.parent === parent));
    if (!found) throw new Error(`the fixture has no ${name}`);
    return found;
  };
  const osc = nodeOf("Oscillator", "instrument");
  const filter = nodeOf("Filter", "effect", osc.id);
  const delay = nodeOf("Delay", "effect", osc.id);
  const reverb = nodeOf("Reverb", "effect", osc.id);
  const math = nodeOf("Math", "effect");
  const feeds = (module: ReturnType<typeof moduleOfNodes>): boolean[][] => (typeof module === "string" ? [] : module.lanes.map((lane) => lane.effects.map((effect) => effect.fed)));

  test("are the plugins that are chosen, lane by lane in the session's order whatever the order they are given in, each with what a file says of it, and the cables that have both ends among them", () => {
    const module = moduleOfNodes(snapshot, [math, filter]);
    if (typeof module === "string") throw new Error(module);
    expect(module.lanes.map((lane) => [lane.instrument, lane.effects.map((effect) => effect.plugin)])).toEqual([[null, ["builtin:filter"]], [null, ["builtin:math"]]]);
    expect(module.lanes[0]?.effects[0]?.mix).toBe(0.9);
    expect(module.lanes[1]?.effects[0]?.params).toEqual({ mode: 4 });
    expect(pluginCount(module)).toBe(2);
    // (Of four cables, the one that has both ends among what is chosen: the filter into the math's second input.)
    expect(module.cables).toEqual([{ from: { lane: 0, effect: 0 }, to: { lane: 1, effect: 0, side: true }, gain: 0.7 }]);
    const voice = moduleOfNodes(snapshot, [osc, filter, math]);
    if (typeof voice === "string") throw new Error(voice);
    expect(voice.lanes[0]?.instrument).toEqual({ plugin: "builtin:osc", preset: null, params: { detune: 3 }, volume: 0.8, pinned: ["detune"] });
    expect(voice.cables).toEqual([
      { from: { lane: 0, effect: null }, to: { lane: 1, effect: 0, side: false }, gain: 0.3 },
      { from: { lane: 0, effect: 0 }, to: { lane: 1, effect: 0, side: true }, gain: 0.7 },
    ]);
    expect(moduleOfNodes(snapshot, [math, filter, osc])).toEqual(voice);
  });

  test("have an effect fed by the chain only where the stage before it was chosen too, and the first effect of a lane that has no instrument as it was", () => {
    expect(feeds(moduleOfNodes(snapshot, [osc, filter, delay, reverb]))).toEqual([[true, true, true]]);
    expect(feeds(moduleOfNodes(snapshot, [filter, delay, reverb]))).toEqual([[true, true, true]]);
    expect(feeds(moduleOfNodes(snapshot, [filter, reverb]))).toEqual([[true, false]]);
    expect(feeds(moduleOfNodes(snapshot, [osc, filter, reverb]))).toEqual([[true, false]]);
    expect(feeds(moduleOfNodes(snapshot, [osc, delay, reverb]))).toEqual([[false, true]]);
    expect(feeds(moduleOfNodes(snapshot, [osc, reverb]))).toEqual([[false]]);
    expect(feeds(moduleOfNodes(snapshot, [osc, filter]))).toEqual([[true]]);
    expect(feeds(moduleOfNodes(snapshot, [delay, reverb]))).toEqual([[true, true]]);
    expect(feeds(moduleOfNodes(snapshot, [delay]))).toEqual([[true]]);
    // A link that was cut stays cut whatever is chosen.
    const cut = { ...snapshot, graph: { ...snapshot.graph, nodes: snapshot.graph.nodes.map((node) => (node.id === delay.id && node.kind === "effect" ? { ...node, feed: "none" as const } : node)) } };
    expect(feeds(moduleOfNodes(cut, [osc, filter, delay, reverb]))).toEqual([[true, false, true]]);
    expect(feeds(moduleOfNodes(cut, [delay]))).toEqual([[false]]);
  });

  test("name an effect by the place that it has in the module and not by the place it had in the session: the cable into the reverb, with the delay between the two left out", () => {
    const module = moduleOfNodes(snapshot, [filter, reverb, math]);
    if (typeof module === "string") throw new Error(module);
    expect(module.lanes.map((lane) => lane.effects.map((effect) => effect.plugin))).toEqual([["builtin:filter", "builtin:reverb"], ["builtin:math"]]);
    expect(module.cables).toEqual([
      { from: { lane: 0, effect: 0 }, to: { lane: 1, effect: 0, side: true }, gain: 0.7 },
      { from: { lane: 1, effect: 0 }, to: { lane: 0, effect: 1, side: false }, gain: 1 },
    ]);
  });

  test("turn away an effect that is in no lane: its parent is neither a lane nor a group, or is not there", () => {
    const modulator = snapshot.graph.nodes.find((node) => node.kind === "modulator");
    if (!modulator) throw new Error("the fixture has a modulator");
    const words = "a module is made of effects and instruments: that effect is in no lane";
    // (No server makes one: an effect is in a lane or on a bus. A graph of another make is told what it is.)
    expect(moduleOfNodes(snapshot, [{ ...reverb, parent: modulator.id }])).toBe(words);
    expect(moduleOfNodes(snapshot, [{ ...reverb, parent: 424_242 }])).toBe(words);
  });

  test("are refused, in the controller's words, for what a module has none of: a group's bus, a modulator, a source, a lane that has no instrument, and nothing", () => {
    const group = snapshot.graph.nodes.find((node) => node.kind === "group");
    const bus = snapshot.graph.nodes.find((node) => node.kind === "effect" && node.parent === group?.id);
    const modulator = snapshot.graph.nodes.find((node) => node.kind === "modulator");
    const source = snapshot.graph.nodes.find((node) => node.kind === "source");
    const holder = snapshot.graph.nodes.find((node) => node.id === math.parent);
    if (!group || !bus || !modulator || !source || !holder) throw new Error("the fixture has a group with a bus effect, a modulator, a source and a holder");
    expect(moduleOfNodes(snapshot, [])).toBe("nothing is chosen: a module is made of the effects and instruments that are");
    expect(moduleOfNodes(snapshot, [filter, group])).toBe("a module is made of effects and instruments: a group's bus is not saved");
    expect(moduleOfNodes(snapshot, [bus, math])).toBe("a module is made of effects and instruments: a group's bus is not saved");
    expect(moduleOfNodes(snapshot, [filter, modulator])).toBe("a modulator is not part of a module yet: a module is made of effects and instruments");
    expect(moduleOfNodes(snapshot, [source])).toBe("a source of notes is not part of a module: a module is made of effects and instruments");
    expect(moduleOfNodes(snapshot, [holder])).toBe("a lane that has no instrument has none to save: choose its effects");
  });

  test("are a chain's when the chosen are the effects of one lane, and the chain's refusals are the chain's own", () => {
    const chain = chainOf(snapshot, filter);
    if (typeof chain === "string") throw new Error(chain);
    expect(chain.map((node) => node.id)).toEqual([filter.id, delay.id, reverb.id]);
    expect(moduleOfNodes(snapshot, chain)).toEqual(moduleOf(snapshot, filter));
    const empty = snapshot.graph.nodes.find((node) => node.kind === "instrument" && node.plugin === null && !snapshot.graph.nodes.some((n) => n.parent === node.id));
    if (!empty) throw new Error("the fixture has an empty lane");
    expect(chainOf(snapshot, empty)).toBe("a lane that has no effect has no chain to save");
    const group = snapshot.graph.nodes.find((node) => node.kind === "group");
    if (!group) throw new Error("the fixture has a group");
    expect(chainOf(snapshot, group)).toBe("a module is the chain of a lane: a group's bus is not saved");
  });

  test("are counted by the plugins in a module, and the catalog's entry has that many parameters", () => {
    const module = moduleOfNodes(snapshot, [osc, filter, delay, reverb, math]);
    if (typeof module === "string") throw new Error(module);
    expect(pluginCount(module)).toBe(5);
    expect(pluginCount({ lanes: [], cables: [] })).toBe(0);
    expect(moduleEntry("Voice", module).params).toBe(5);
  });
});
