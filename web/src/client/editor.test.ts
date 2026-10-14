import { afterEach, describe, expect, test } from "vitest";

import { Client } from "./connection";
import { MASTER, nodeNamed, reachFixture, tracerSnapshot } from "./fixtures";
import { MockServer } from "./mock";
import type { MockOptions } from "./mock";
import { Editor } from "./editor";
import { Notices } from "./notices";
import { Reveals } from "./reveals";

const clients: Client[] = [];
afterEach(() => {
  for (const client of clients.splice(0)) client.stop();
});

async function until(condition: () => boolean, what: string): Promise<void> {
  const deadline = Date.now() + 2000;
  while (!condition()) {
    if (Date.now() > deadline) throw new Error(`gave up waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 2));
  }
}

/** An editor of a session on a mock engine, with its notices. */
async function open(options: MockOptions = {}) {
  const server = new MockServer(options);
  const client = new Client({ transport: server.transport, reconnectDelay: () => 5 });
  clients.push(client);
  client.start();
  await until(() => client.state.status === "open", "the session");
  const notices = new Notices();
  const reveals = new Reveals();
  const shown: number[] = [];
  reveals.subscribe((node) => {
    // (What is shown is in the session the client has by then: the canvas has something to look for.)
    expect(client.state.snapshot?.graph.nodes.some((n) => n.id === node), `node ${String(node)} is shown before the client has it`).toBe(true);
    shown.push(node);
  });
  return { server, client, notices, shown, editor: new Editor(client, notices, reveals) };
}

const said = (notices: Notices): string[] => notices.getItems().map((notice) => notice.text);
const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");
const lfo = nodeNamed(tracerSnapshot, "LFO");
const param = (node: typeof filter, name: string): number => {
  if (node.kind !== "instrument" && node.kind !== "effect") throw new Error("no plugin");
  const found = node.plugin?.params.find((p) => p.name === name);
  if (!found) throw new Error(`no ${name}`);
  return found.index;
};

describe("adding a modulator", () => {
  test("resolves with the node it made, which is in the session then, and says nothing", async () => {
    const { client, notices, editor } = await open();
    const id = await editor.addModulator(oscillator.id, "envelope");
    expect(id).toBe(3);
    const node = client.state.snapshot?.graph.nodes.find((n) => n.id === id);
    expect(node).toMatchObject({ kind: "modulator", parent: oscillator.id, name: "ADSR" });
    expect(said(notices)).toEqual([]);
    // The next is the next.
    expect(await editor.addModulator(oscillator.id, "lfo")).toBe(4);
  });

  test("finds its own node when another front-end added one to another lane at the same time", async () => {
    const { server, editor } = await open({ snapshot: reachFixture.snapshot });
    server.apply({ type: "addModulator", parent: 7, kind: "lfo" }); // another front-end's, to the second lane, before this one is answered
    const id = await editor.addModulator(4, "lfo");
    const mine = server.snapshot.graph.nodes.find((n) => n.id === id);
    expect(mine?.parent).toBe(4);
    expect(server.snapshot.graph.nodes.filter((n) => n.kind === "modulator" && n.parent === 7).length).toBe(2);
  });

  test("takes the last of the modulators that are new to the lane, which is its own, when another front-end added one to the same lane just before", async () => {
    const { server, editor } = await open();
    server.apply({ type: "addModulator", parent: oscillator.id, kind: "envelope" });
    const id = await editor.addModulator(oscillator.id, "lfo");
    const names = server.snapshot.graph.nodes.filter((n) => n.parent === oscillator.id && n.kind === "modulator").map((n) => [n.id, n.name]);
    expect(names).toEqual([[2, "LFO"], [3, "ADSR"], [4, "LFO"]]);
    expect(id).toBe(4);
  });

  test("is refused for what is no lane or group: it resolves with null, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.addModulator(filter.id, "lfo")).toBeNull();
    expect(await editor.addModulator(lfo.id, "midiLearn")).toBeNull();
    expect(await editor.addModulator(MASTER, "envelope")).toBeNull();
    expect(said(notices)).toEqual([
      "Could not add an LFO to Filter: a modulator goes in a lane or in a group",
      "Could not add a MIDI control to LFO: a modulator goes in a lane or in a group",
      "Could not add an envelope to Master: there is no node 4294967295",
    ]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.addModulator(oscillator.id, "lfo")).toBeNull();
    expect(said(notices)).toEqual(["Could not add an LFO to Oscillator: Not connected to the server."]);
  });
});

describe("adding an effect", () => {
  test("resolves with the node it made, which is in the lane's chain then, and says nothing", async () => {
    const { client, notices, editor } = await open();
    const id = await editor.addEffect(oscillator.id, "builtin:reverb");
    expect(id).toBe(3);
    const node = client.state.snapshot?.graph.nodes.find((n) => n.id === id);
    expect(node).toMatchObject({ kind: "effect", parent: oscillator.id, name: "Reverb", mix: 0.5 });
    expect(said(notices)).toEqual([]);
    // The next is the next, and a filter goes in fully wet.
    expect(await editor.addEffect(oscillator.id, "builtin:filter")).toBe(4);
  });

  test("finds its own node when another front-end added one to another lane at the same time", async () => {
    const { server, editor } = await open({ snapshot: reachFixture.snapshot });
    server.apply({ type: "addEffect", parent: 7, plugin: "builtin:filter" }); // another front-end's, to the second lane of group 1, before this one is answered
    const id = await editor.addEffect(4, "builtin:reverb");
    const mine = server.snapshot.graph.nodes.find((n) => n.id === id);
    expect(mine).toMatchObject({ parent: 4, name: "Reverb" });
  });

  test("takes the last of the effects that are new to the lane, which is its own, when another front-end added one to the same lane just before", async () => {
    const { server, editor } = await open();
    server.apply({ type: "addEffect", parent: oscillator.id, plugin: "builtin:filter" });
    const id = await editor.addEffect(oscillator.id, "builtin:reverb");
    const names = server.snapshot.graph.nodes.filter((n) => n.parent === oscillator.id && n.kind === "effect").map((n) => [n.id, n.name]);
    expect(names).toEqual([[1, "Filter"], [3, "Filter"], [4, "Reverb"]]);
    expect(id).toBe(4);
  });

  test("is refused for what is no lane or group, or a plugin that is not installed: it resolves with null, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.addEffect(filter.id, "builtin:reverb")).toBeNull();
    expect(await editor.addEffect(oscillator.id, "./examples/plugin.clap")).toBeNull();
    expect(await editor.addEffect(oscillator.id, "builtin:osc")).toBeNull();
    expect(said(notices)).toEqual([
      "Could not add Reverb to Filter: an effect goes in a lane or in a group",
      "Could not add ./examples/plugin.clap to Oscillator: ./examples/plugin.clap is not a plugin that is installed",
      "Could not add Oscillator to Oscillator: Oscillator is an instrument, not an effect",
    ]);
    // (A node that is not there is named as the page names what it cannot find.)
    expect(await editor.addEffect(MASTER, "builtin:reverb")).toBeNull();
    expect(said(notices).at(-1)).toBe("Could not add Reverb to Master: there is no node 4294967295");
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.addEffect(oscillator.id, "builtin:reverb")).toBeNull();
    expect(said(notices)).toEqual(["Could not add Reverb to Oscillator: Not connected to the server."]);
  });
});

describe("adding an instrument", () => {
  test("resolves with the node of the lane it made, which is in the session then, and says nothing", async () => {
    const { client, notices, editor } = await open();
    const id = await editor.addInstrument("builtin:osc");
    expect(id).toBe(3);
    const node = client.state.snapshot?.graph.nodes.find((n) => n.id === id);
    expect(node).toMatchObject({ kind: "instrument", parent: null, name: "Oscillator" });
    expect(said(notices)).toEqual([]);
    // The next is the next.
    expect(await editor.addInstrument("builtin:osc")).toBe(4);
  });

  test("takes the lane that is new to the session, which is its own, when another front-end added one just before", async () => {
    const { server, editor } = await open();
    server.apply({ type: "addInstrument", plugin: "builtin:osc" });
    const id = await editor.addInstrument("builtin:osc");
    expect(id).toBe(4);
    expect(server.snapshot.graph.nodes.filter((n) => n.kind === "instrument").map((n) => n.id)).toEqual([0, 3, 4]);
  });

  test("is refused for a plugin that is not installed or is not an instrument: it resolves with null, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.addInstrument("./examples/plugin.clap")).toBeNull();
    expect(await editor.addInstrument("builtin:reverb")).toBeNull();
    expect(said(notices)).toEqual([
      "Could not add ./examples/plugin.clap as an instrument: ./examples/plugin.clap is not a plugin that is installed",
      "Could not add Reverb as an instrument: Reverb is an effect, not an instrument",
    ]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.addInstrument("builtin:osc")).toBeNull();
    expect(said(notices)).toEqual(["Could not add Oscillator as an instrument: Not connected to the server."]);
  });
});

describe("replacing an instrument", () => {
  test("resolves true when the lane has the instrument, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.replaceInstrument(oscillator.id, "builtin:osc")).toBe(true);
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    expect(said(notices)).toEqual([]);
  });

  test("is refused for a node that is not a lane, a plugin that is not installed and one that is not an instrument: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.replaceInstrument(filter.id, "builtin:osc")).toBe(false);
    expect(await editor.replaceInstrument(oscillator.id, "./examples/plugin.clap")).toBe(false);
    expect(await editor.replaceInstrument(oscillator.id, "builtin:reverb")).toBe(false);
    expect(said(notices)).toEqual([
      "Could not change the instrument of Filter to Oscillator: an instrument goes in a lane",
      "Could not change the instrument of Oscillator to ./examples/plugin.clap: ./examples/plugin.clap is not a plugin that is installed",
      "Could not change the instrument of Oscillator to Reverb: Reverb is an effect, not an instrument",
    ]);
    expect(await editor.replaceInstrument(MASTER, "builtin:osc")).toBe(false);
    expect(said(notices).at(-1)).toBe("Could not change the instrument of Master to Oscillator: there is no node 4294967295");
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.replaceInstrument(oscillator.id, "builtin:osc")).toBe(false);
    expect(said(notices)).toEqual(["Could not change the instrument of Oscillator to Oscillator: Not connected to the server."]);
  });
});

describe("loading a preset", () => {
  /** The tracer's reverb on a lane that has one (an effect with presets: the reach fixture's reverb of lane 9, in a group). */
  const LANE_REVERB = 10;

  test("resolves true when the plugin has the preset, which the session shows, and says nothing", async () => {
    const { client, notices, editor } = await open({ snapshot: reachFixture.snapshot });
    expect(await editor.loadPreset(LANE_REVERB, "cathedral")).toBe(true);
    const node = client.state.snapshot?.graph.nodes.find((n) => n.id === LANE_REVERB);
    expect(node?.kind === "effect" && node.plugin.currentPreset).toBe("Cathedral");
    expect(said(notices)).toEqual([]);
  });

  test("is refused for a preset the plugin does not have, a bus effect and a node that has no plugin: it resolves false, and the page says what it could not do and why, by the preset's name", async () => {
    const { client, notices, editor } = await open({ snapshot: reachFixture.snapshot });
    expect(await editor.loadPreset(LANE_REVERB, "no-such-preset")).toBe(false);
    expect(await editor.loadPreset(3, "cathedral")).toBe(false); // group 2's bus effect
    expect(await editor.loadPreset(0, "cathedral")).toBe(false); // a group
    expect(said(notices)).toEqual([
      "Could not load no-such-preset into Reverb 2: the plugin has no preset no-such-preset",
      "Could not load Cathedral into Reverb 1: a bus effect has no presets to load",
      "Could not load cathedral into Group 1: that node has no plugin",
    ]);
    expect(client.state.snapshot).toEqual(reachFixture.snapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open({ snapshot: reachFixture.snapshot });
    client.stop();
    expect(await editor.loadPreset(LANE_REVERB, "plate")).toBe(false);
    expect(said(notices)).toEqual(["Could not load Vintage Plate into Reverb 2: Not connected to the server."]);
  });
});

describe("setting the keys of a lane", () => {
  const keys = (client: Client, node: number = oscillator.id) => {
    const found = client.state.snapshot?.graph.nodes.find((n) => n.id === node);
    return found?.kind === "instrument" ? found.range : "not a lane";
  };

  test("resolves true when the lane has the keys, which the session shows, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.setRange(oscillator.id, { low: 36, high: 84 })).toBe(true);
    expect(keys(client)).toEqual({ low: 36, high: 84 });
    expect(await editor.setRange(oscillator.id, null)).toBe(true);
    expect(keys(client)).toBeNull();
    expect(said(notices)).toEqual([]);
  });

  test("is refused for a range that is not one and for a node that is not a lane: it resolves false, and the page says what it could not do and why, with the keys written as the TUI writes them", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.setRange(oscillator.id, { low: 90, high: 40 })).toBe(false);
    expect(await editor.setRange(filter.id, { low: 0, high: 59 })).toBe(false);
    expect(await editor.setRange(lfo.id, null)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not set the keys of Oscillator to F#6-E2: the range goes from F#6 down to E2: its low note has to be the lower",
      "Could not set the keys of Filter to -B3: a key range belongs to a lane",
      "Could not set the keys of LFO to all keys: a key range belongs to a lane",
    ]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.setRange(oscillator.id, { low: 60, high: 127 })).toBe(false);
    expect(said(notices)).toEqual(["Could not set the keys of Oscillator to C4-: Not connected to the server."]);
  });
});

describe("moving a node", () => {
  const placeOf = (client: Client, node: number) => client.state.snapshot?.graph.nodes.find((n) => n.id === node)?.ui;

  test("resolves true when the node has the place, which the session shows, and says nothing; none gives it back to the layout", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.moveNode(filter.id, { x: 376, y: -40 })).toBe(true);
    expect(placeOf(client, filter.id)).toEqual({ x: 376, y: -40 });
    expect(await editor.moveNode(MASTER, { x: 900, y: 0 })).toBe(true);
    expect(placeOf(client, MASTER)).toEqual({ x: 900, y: 0 });
    expect(await editor.moveNode(filter.id, null)).toBe(true);
    expect(placeOf(client, filter.id)).toBeUndefined();
    expect(said(notices)).toEqual([]);
  });

  test("is refused for a node that is not there: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.moveNode(424242, { x: 1, y: 1 })).toBe(false);
    expect(await editor.moveNode(424242, null)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not move that node: there is no node 424242",
      "Could not give that node back to the layout: there is no node 424242",
    ]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.moveNode(lfo.id, { x: 1, y: 2 })).toBe(false);
    expect(said(notices)).toEqual(["Could not move LFO: Not connected to the server."]);
  });
});

describe("setting the scale of the piano", () => {
  const piano = (client: Client) => client.state.snapshot?.meta.piano;

  test("resolves true when the piano has the scale, which the session shows, and says nothing", async () => {
    const { server, client, notices, editor } = await open();
    expect(await editor.setScale("F# dorian")).toBe(true);
    // (The words are the server's to read: they go as they were given.)
    expect(server.received.flatMap((message) => (message.type === "request" ? [message.intent] : []))).toEqual([{ type: "setScale", scale: "F# dorian" }]);
    expect(piano(client)).toEqual({ scale: "F# dorian", root: 6, kind: "dorian", notes: [1, 3, 4, 6, 8, 9, 11], locked: false });
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    expect(said(notices)).toEqual([]);
  });

  test("is refused for what is not a scale: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.setScale("banana")).toBe(false);
    expect(said(notices)).toEqual(['Could not set the scale of the piano to banana: "banana" is not a scale: a root and a kind of scale, like C major or F# dorian']);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.setScale("A minor")).toBe(false);
    expect(said(notices)).toEqual(["Could not set the scale of the piano to A minor: Not connected to the server."]);
  });
});

describe("locking the piano to its scale", () => {
  test("resolves true when the piano is locked and when it is let go, which the session shows, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(tracerSnapshot.meta.piano.locked).toBe(false);
    expect(await editor.setLocked(true)).toBe(true);
    expect(client.state.snapshot?.meta.piano).toEqual({ ...tracerSnapshot.meta.piano, locked: true });
    expect(await editor.setLocked(false)).toBe(true);
    expect(client.state.snapshot?.meta.piano).toEqual(tracerSnapshot.meta.piano);
    expect(said(notices)).toEqual([]);
  });

  test("with no connection says so, in the words of what it was to do", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.setLocked(true)).toBe(false);
    expect(await editor.setLocked(false)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not lock the piano to its scale: Not connected to the server.",
      "Could not let the piano play every note: Not connected to the server.",
    ]);
  });
});

describe("showing what was added", () => {
  test("is asked for each node the page added, in the order they were, once the session has it", async () => {
    const { editor, shown } = await open();
    const lfo = await editor.addModulator(oscillator.id, "lfo");
    const effect = await editor.addEffect(oscillator.id, "builtin:reverb");
    const lane = await editor.addInstrument("builtin:osc");
    expect(shown).toEqual([lfo, effect, lane]);
    expect(shown).toEqual([3, 4, 5]);
  });

  test("is not asked for what was refused, nor for what was not added but changed or taken out, nor for a cable", async () => {
    const { editor, shown } = await open();
    expect(await editor.addEffect(filter.id, "builtin:reverb")).toBeNull();
    expect(await editor.addInstrument("builtin:reverb")).toBeNull();
    expect(await editor.replaceInstrument(oscillator.id, "builtin:osc")).toBe(true);
    expect(await editor.addCable(lfo.id, oscillator.id, param(oscillator, "detune"))).toBe(true);
    expect(await editor.removeNode(lfo.id)).toBe(true);
    expect(shown).toEqual([]);
  });

  test("is not needed: an editor with nobody to show things to adds as it did", async () => {
    const { server } = await open();
    const client = new Client({ transport: server.transport, reconnectDelay: () => 5 });
    clients.push(client);
    client.start();
    await until(() => client.state.status === "open", "the session");
    const editor = new Editor(client, new Notices());
    expect(await editor.addInstrument("builtin:osc")).toBe(3);
  });
});

describe("drawing a cable", () => {
  test("joins a modulator to a parameter, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.addCable(lfo.id, oscillator.id, param(oscillator, "detune"))).toBe(true);
    expect(client.state.snapshot?.graph.edges.filter((e) => e.kind === "modulation")).toHaveLength(2);
    expect(said(notices)).toEqual([]);
  });

  test("that is refused says which cable, in the names the canvas has, and why", async () => {
    const { notices, editor } = await open();
    const cutoff = param(filter, "cutoff");
    // (The LFO drives the cutoff already.)
    expect(await editor.addCable(lfo.id, filter.id, cutoff)).toBe(false);
    expect(await editor.addCable(oscillator.id, filter.id, cutoff)).toBe(false);
    expect(await editor.addCable(lfo.id, filter.id, 999)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not draw a cable from LFO to Filter: cutoff: that modulator drives that parameter already",
      "Could not draw a cable from Oscillator to Filter: cutoff: a cable starts at a modulator",
      "Could not draw a cable from LFO to Filter: the plugin has no parameter 999",
    ]);
  });

  test("names two nodes of one name by their numbers", async () => {
    const { notices, editor } = await open();
    await editor.addModulator(oscillator.id, "lfo");
    expect(await editor.addCable(3, filter.id, param(filter, "cutoff"))).toBe(true);
    expect(await editor.addCable(3, filter.id, param(filter, "cutoff"))).toBe(false);
    expect(said(notices)).toEqual(["Could not draw a cable from LFO 2 to Filter: cutoff: that modulator drives that parameter already"]);
  });
});

describe("taking a node out", () => {
  test("takes a modulator with its cables, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.removeNode(lfo.id)).toBe(true);
    expect(client.state.snapshot?.graph.nodes.map((n) => n.name)).toEqual(["Oscillator", "Filter", "Master"]);
    expect(said(notices)).toEqual([]);
  });

  test("takes an effect with the cables that drive it, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.removeNode(filter.id)).toBe(true);
    expect(client.state.snapshot?.graph.nodes.map((n) => n.name)).toEqual(["Oscillator", "LFO", "Master"]);
    expect(client.state.snapshot?.graph.edges.filter((e) => e.kind === "modulation")).toEqual([]);
    expect(said(notices)).toEqual([]);
  });

  test("that cannot be taken out says what and why", async () => {
    const { notices, editor } = await open();
    expect(await editor.removeNode(oscillator.id)).toBe(false);
    expect(await editor.removeNode(999)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not remove Oscillator: only a modulator or an effect can be removed so far",
      "Could not remove that node: there is no node 999",
    ]);
  });
});

describe("taking a cable out", () => {
  test("takes it out, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.removeCable(0)).toBe(true);
    expect(client.state.snapshot?.graph.edges.filter((e) => e.kind === "modulation")).toEqual([]);
    expect(said(notices)).toEqual([]);
  });

  test("that is not there says so, and one that is names what it joins", async () => {
    const { server, notices, editor } = await open();
    expect(await editor.removeCable(99)).toBe(false);
    expect(await editor.removeCable(0)).toBe(true);
    expect(await editor.removeCable(0)).toBe(false);
    expect(said(notices)).toEqual(["Could not take out that cable: there is no cable 99", "Could not take out that cable: there is no cable 0"]);
    expect(server.snapshot.graph.edges.filter((e) => e.kind === "modulation")).toEqual([]);
  });

  test("when the connection is gone names the cable it could not take out", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.removeCable(0)).toBe(false);
    expect(said(notices)).toEqual(["Could not take out the cable LFO → Filter: cutoff: Not connected to the server."]);
  });
});

describe("saving", () => {
  const noFile = (): MockOptions => ({ snapshot: { ...tracerSnapshot, meta: { ...tracerSnapshot.meta, file: null } } });

  test("writes the session, which has nothing unsaved after it, and says nothing", async () => {
    const { client, notices, editor } = await open();
    await client.request({ type: "setGain", node: oscillator.id, gain: "volume", value: 0.5 });
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    expect(await editor.save()).toBe(true);
    expect(client.state.snapshot?.meta.dirty).toBe(false);
    expect(said(notices)).toEqual([]);
  });

  test("that is refused says so in a notice, and the session is as unsaved as it was", async () => {
    const { client, notices, editor } = await open(noFile());
    await client.request({ type: "setGain", node: oscillator.id, gain: "volume", value: 0.5 });
    expect(await editor.save()).toBe(false);
    expect(said(notices)).toEqual(["Could not save the session: No session path — cannot save"]);
    expect(client.state.snapshot?.meta.dirty).toBe(true);
  });

  test("when the connection is gone says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.save()).toBe(false);
    expect(said(notices)).toEqual(["Could not save the session: Not connected to the server."]);
  });
});
