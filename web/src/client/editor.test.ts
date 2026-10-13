import { afterEach, describe, expect, test } from "vitest";

import { Client } from "./connection";
import { MASTER, nodeNamed, reachFixture, tracerSnapshot } from "./fixtures";
import { MockServer } from "./mock";
import type { MockOptions } from "./mock";
import { Editor } from "./editor";
import { Notices } from "./notices";

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
  return { server, client, notices, editor: new Editor(client, notices) };
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

describe("taking a modulator out", () => {
  test("takes it with its cables, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.removeModulator(lfo.id)).toBe(true);
    expect(client.state.snapshot?.graph.nodes.map((n) => n.name)).toEqual(["Oscillator", "Filter", "Master"]);
    expect(said(notices)).toEqual([]);
  });

  test("that cannot be taken out says what and why", async () => {
    const { notices, editor } = await open();
    expect(await editor.removeModulator(oscillator.id)).toBe(false);
    expect(await editor.removeModulator(999)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not remove Oscillator: only a modulator can be removed so far",
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
