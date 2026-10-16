import { afterEach, describe, expect, test } from "vitest";

import type { Edge, Port } from "../protocol";
import { Client } from "./connection";
import { MASTER, groupsFixture, movesFixture, nodeNamed, reachFixture, tracerSnapshot } from "./fixtures";
import { MockServer } from "./mock";
import type { MockOptions } from "./mock";
import { Editor } from "./editor";
import { Notices } from "./notices";
import { samePort } from "./reach";
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
/** The port of a parameter of a node, by its name. */
const paramPort = (node: typeof filter, name: string): Port => ({ type: "param", index: param(node, name) });

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

  test("says a macro that it could not add is a macro", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.addModulator(filter.id, "macro")).toBeNull();
    expect(said(notices)).toEqual(["Could not add a macro to Filter: a modulator goes in a lane or in a group"]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.addModulator(oscillator.id, "lfo")).toBeNull();
    expect(said(notices)).toEqual(["Could not add an LFO to Oscillator: Not connected to the server."]);
  });
});

describe("modulating a parameter", () => {
  /** The cables of the session that end at the port `to` of `node`. */
  const cablesTo = (cables: readonly Edge[], node: number, to: Port): Edge[] => cables.filter((e) => e.kind === "modulation" && e.to.node === node && samePort(e.to.port, to));
  const requests = (server: MockServer) => server.received.flatMap((message) => (message.type === "request" ? [message.intent] : []));

  test("resolves with the node of the modulator it made, which is in the lane then, with a cable from it to the parameter at half depth, and says nothing; the canvas is asked to show it", async () => {
    const { client, notices, shown, editor } = await open();
    const cutoff = paramPort(filter, "cutoff");
    const id = await editor.modulate(filter.id, cutoff, "envelope");
    expect(id).toBe(3);
    const graph = client.state.snapshot?.graph;
    expect(graph?.nodes.find((n) => n.id === id)).toMatchObject({ kind: "modulator", parent: oscillator.id, name: "ADSR" });
    const [cable] = cablesTo(graph?.edges ?? [], filter.id, cutoff).filter((e) => e.kind === "modulation" && e.from.node === id);
    expect(cable).toMatchObject({ kind: "modulation", from: { node: id, port: { type: "modOut" } }, amount: 0.5 });
    expect(said(notices)).toEqual([]);
    expect(shown).toEqual([id]);
  });

  test("asks for one intent, at the depth the TUI's m gives a cable (none given: the controller's), and the depth the TUI's l gives a control that learns, which is armed", async () => {
    const { server, client, editor } = await open();
    const cutoff = paramPort(filter, "cutoff");
    const first = await editor.modulate(filter.id, cutoff, "lfo");
    const learner = await editor.modulate(filter.id, cutoff, "midiLearn");
    expect(requests(server)).toEqual([
      { type: "modulate", to: { node: filter.id, port: cutoff }, kind: "lfo", depth: null },
      { type: "modulate", to: { node: filter.id, port: cutoff }, kind: "midiLearn", depth: 1 },
    ]);
    const graph = client.state.snapshot?.graph;
    expect(graph?.nodes.find((n) => n.id === learner)).toMatchObject({ kind: "modulator", source: { type: "midiLearn", control: null, learning: true } });
    // (The cutoff has a cable of the tracer's own, at 0.4.)
    const made = cablesTo(graph?.edges ?? [], filter.id, cutoff).filter((e) => e.kind === "modulation" && (e.from.node === first || e.from.node === learner));
    expect(made.map((e) => (e.kind === "modulation" ? e.amount : null))).toEqual([0.5, 1]);
  });

  test("makes a sibling that drives a setting of a modulator, and shows it", async () => {
    const { client, shown, editor } = await open();
    const id = await editor.modulate(lfo.id, { type: "rate" }, "lfo");
    const graph = client.state.snapshot?.graph;
    expect(graph?.nodes.find((n) => n.id === id)).toMatchObject({ kind: "modulator", parent: lfo.parent });
    expect(cablesTo(graph?.edges ?? [], lfo.id, { type: "rate" }).some((e) => e.kind === "modulation" && e.from.node === id)).toBe(true);
    expect(shown).toEqual([id]);
  });

  test("finds its own node, the one that has the cable to the port, when another front-end made one for the same parameter just before", async () => {
    const { server, editor } = await open();
    const cutoff = paramPort(filter, "cutoff");
    server.apply({ type: "modulate", to: { node: filter.id, port: cutoff }, kind: "envelope", depth: null });
    const id = await editor.modulate(filter.id, cutoff, "lfo");
    const mine = server.snapshot.graph.nodes.find((n) => n.id === id);
    expect(mine).toMatchObject({ kind: "modulator", name: "LFO" });
    expect(id).toBe(4);
  });

  test("is refused as the controller refuses it: it resolves with null, nothing is made, and the page says what it could not do and why, in the names the canvas shows", async () => {
    const { client, notices, shown, editor } = await open({ snapshot: reachFixture.snapshot });
    const before = client.state.snapshot;
    const group = reachFixture.snapshot.graph.nodes.find((n) => n.kind === "group");
    if (!group) throw new Error("the fixture has a group");
    expect(await editor.modulate(group.id, { type: "param", index: 1 }, "lfo")).toBeNull();
    expect(await editor.modulate(MASTER, { type: "param", index: 1 }, "envelope")).toBeNull();
    expect(await editor.modulate(group.id, { type: "modOut" }, "midiLearn")).toBeNull();
    expect(said(notices)).toEqual([
      `Could not modulate ${group.name} with an LFO: a modulator drives a parameter of a plugin or a setting of a modulator, and that is neither`,
      "Could not modulate Master with an envelope: there is no node 4294967295",
      `Could not modulate ${group.name} with a MIDI control: a modulator drives a parameter of a plugin or a setting of a modulator, and that is neither`,
    ]);
    expect(client.state.snapshot).toEqual(before);
    expect(shown).toEqual([]);
  });

  test("names what it could not modulate by the port: a setting of a modulator by its name, a parameter that the plugin has not by the node alone", async () => {
    const { notices, editor } = await open();
    expect(await editor.modulate(filter.id, { type: "param", index: 9_999 }, "lfo")).toBeNull();
    expect(await editor.modulate(lfo.id, { type: "attack" }, "envelope")).toBeNull();
    expect(said(notices)).toEqual([
      "Could not modulate Filter with an LFO: the plugin has no parameter 9999",
      "Could not modulate LFO: attack with an envelope: that modulator has no attack",
    ]);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.modulate(filter.id, paramPort(filter, "cutoff"), "lfo")).toBeNull();
    expect(said(notices)).toEqual(["Could not modulate Filter: cutoff with an LFO: Not connected to the server."]);
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
    // (A lane that is new has no pattern, as the controller makes it.)
    expect(node).toMatchObject({ kind: "instrument", parent: null, name: "Oscillator", pattern: null });
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

// The session of the groups fixture: three groups (0, 1 and 2, the last empty and called Lead; the first has two modulators, 13 and 14, the second one,
// 15), four lanes (3 and 10 in the first group, 9 in the second, 5 in none; each has an instrument called Oscillator, and the canvas numbers what is called
// alike across the session in the order of its graph), and the cables of the groups to their lanes.
describe("adding a group", () => {
  const groups = { snapshot: groupsFixture.snapshot };
  const parentOf = (client: Client, node: number) => client.state.snapshot?.graph.nodes.find((n) => n.id === node)?.parent;

  test("resolves with the node of the group it made, which is in the session then, and says nothing; the canvas is asked to show it", async () => {
    const { client, notices, shown, editor } = await open(groups);
    const id = await editor.addGroup();
    expect(id).toBe(16);
    expect(client.state.snapshot?.graph.nodes.find((n) => n.id === id)).toMatchObject({ kind: "group", parent: null, name: "Group 4" });
    expect(said(notices)).toEqual([]);
    expect(shown).toEqual([16]);
    // The next is the next.
    expect(await editor.addGroup()).toBe(17);
  });

  test("puts the lanes it is given in the group it makes, whatever group they were in", async () => {
    const { client, editor } = await open(groups);
    const id = await editor.addGroup([5, 10]);
    expect(id).toBe(16);
    expect([3, 5, 9, 10].map((lane) => parentOf(client, lane))).toEqual([0, id, 1, id]);
  });

  test("takes the group that is new to the session, which is its own, when another front-end added one just before", async () => {
    const { server, editor } = await open(groups);
    server.apply({ type: "addGroup", members: [] });
    expect(await editor.addGroup()).toBe(17);
    expect(server.snapshot.graph.nodes.filter((n) => n.kind === "group").map((n) => n.id)).toEqual([0, 1, 2, 16, 17]);
  });

  test("is refused for a member that is not a lane: it resolves with null, no group is made, and the page says what it could not do and why", async () => {
    const { client, notices, shown, editor } = await open(groups);
    expect(await editor.addGroup([0])).toBeNull();
    expect(await editor.addGroup([5, 9999])).toBeNull();
    expect(said(notices)).toEqual([
      "Could not put Group 1 in a new group: only a lane can be put in a group",
      "Could not put Oscillator 2, that node in a new group: there is no node 9999",
    ]);
    expect(client.state.snapshot).toEqual(groupsFixture.snapshot);
    expect(shown).toEqual([]);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open(groups);
    client.stop();
    expect(await editor.addGroup()).toBeNull();
    expect(await editor.addGroup([5])).toBeNull();
    expect(said(notices)).toEqual(["Could not add a group: Not connected to the server.", "Could not put Oscillator 2 in a new group: Not connected to the server."]);
  });
});

describe("putting a lane in a group", () => {
  const groups = { snapshot: groupsFixture.snapshot };
  const parentOf = (client: Client, node: number) => client.state.snapshot?.graph.nodes.find((n) => n.id === node)?.parent;

  test("resolves true when the lane is in the group, which the session shows, and says nothing; and for null when it is in none", async () => {
    const { client, notices, editor } = await open(groups);
    expect(await editor.setLaneGroup(5, 1)).toBe(true);
    expect(parentOf(client, 5)).toBe(1);
    expect(await editor.setLaneGroup(5, null)).toBe(true);
    expect(parentOf(client, 5)).toBeNull();
    expect(said(notices)).toEqual([]);
  });

  test("asks the canvas to show the group the lane was put in, which moves with it, and nothing when the lane is taken out of its group or the lane was refused", async () => {
    const { shown, editor } = await open(groups);
    expect(await editor.setLaneGroup(5, 1)).toBe(true);
    expect(shown).toEqual([1]);
    expect(await editor.setLaneGroup(5, null)).toBe(true);
    expect(await editor.setLaneGroup(5, 5)).toBe(false);
    expect(await editor.setLaneGroup(5, 9999)).toBe(false);
    expect(shown).toEqual([1]);
  });

  test("takes the group's cables to a lane that leaves it out of the session, and the cable that drives the depth of one of them", async () => {
    const { client, editor } = await open(groups);
    const cables = () => client.state.snapshot?.graph.edges.flatMap((e) => (e.kind === "modulation" ? [e.id] : []));
    expect(cables()).toEqual([0, 1, 2, 3, 4]);
    expect(await editor.setLaneGroup(3, null)).toBe(true);
    expect(cables()).toEqual([1, 2, 3, 4]);
    // Cable 3 (the first group's envelope) drives the depth of cable 1 (the LFO's, to the pan of lane 10), so it goes when cable 1 does.
    expect(await editor.setLaneGroup(10, 1)).toBe(true);
    expect(cables()).toEqual([4]);
  });

  test("is refused for a node that is not a lane and for one that is not a group: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open(groups);
    // (The notices keep the last few: each is read as it is said.)
    const cases: [number, number | null, string][] = [
      [0, 1, "Could not put Group 1 in Group 2: only a lane can be put in a group"],
      [5, 3, "Could not put Oscillator 2 in Oscillator 1: that node is not a group"],
      [5, 9999, "Could not put Oscillator 2 in that node: there is no node 9999"],
      [9999, null, "Could not take that node out of its group: there is no node 9999"],
    ];
    for (const [lane, group, words] of cases) {
      expect(await editor.setLaneGroup(lane, group), words).toBe(false);
      expect(said(notices).at(-1)).toBe(words);
    }
    expect(client.state.snapshot).toEqual(groupsFixture.snapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open(groups);
    client.stop();
    expect(await editor.setLaneGroup(5, 1)).toBe(false);
    expect(await editor.setLaneGroup(3, null)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not put Oscillator 2 in Group 2: Not connected to the server.",
      "Could not take Oscillator 1 out of its group: Not connected to the server.",
    ]);
  });
});

describe("naming a group", () => {
  const groups = { snapshot: groupsFixture.snapshot };
  const nameOf = (client: Client, node: number) => client.state.snapshot?.graph.nodes.find((n) => n.id === node)?.name;

  test("resolves true when the group has the name, which the session shows, and says nothing; a name of nothing gives the default one back", async () => {
    const { client, notices, editor } = await open(groups);
    expect(await editor.renameGroup(1, "  Strings ")).toBe(true);
    expect(nameOf(client, 1)).toBe("Strings");
    expect(await editor.renameGroup(1, "")).toBe(true);
    expect(nameOf(client, 1)).toBe("Group 2");
    expect(said(notices)).toEqual([]);
  });

  test("is refused for a name that is too long or has a control character, and for a node that is not a group: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open(groups);
    // (The notices keep the last few: each is read as it is said.)
    const cases: [number, string, string][] = [
      [0, "x".repeat(65), `Could not give Group 1 the name "${"x".repeat(24)}…": a name is at most 64 characters`],
      // (A character is not always one unit of a string: the quote is cut after 24 of them, not after 24 units.)
      [0, "😀".repeat(65), `Could not give Group 1 the name "${"😀".repeat(24)}…": a name is at most 64 characters`],
      [0, "two\nlines", 'Could not give Group 1 the name "two\\nlines": a name has no control characters'],
      [3, "Strings", 'Could not give Oscillator 1 the name "Strings": only a group has a name to change'],
      [9999, "Strings", 'Could not give that node the name "Strings": there is no node 9999'],
    ];
    for (const [node, name, words] of cases) {
      expect(await editor.renameGroup(node, name), words).toBe(false);
      expect(said(notices).at(-1)).toBe(words);
    }
    expect(client.state.snapshot).toEqual(groupsFixture.snapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open(groups);
    client.stop();
    expect(await editor.renameGroup(0, "Strings")).toBe(false);
    expect(said(notices)).toEqual(['Could not give Group 1 the name "Strings": Not connected to the server.']);
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

describe("moving an effect", () => {
  // The session of the moves fixture: a group (0) with four bus effects (1 to 4), a lane in it (5) with four effects (6 to 9), and a lane
  // in no group (11) with three (12 to 14). The names the canvas gives are numbered across the session in the order of its graph: the bus's
  // are Filter 1, Reverb 1, Filter 2 and Reverb 2, the first lane's Filter 3, Reverb 3, Filter 4 and Reverb 4.
  const chain = (client: Client, parent: number): number[] =>
    (client.state.snapshot?.graph.nodes ?? []).filter((n) => n.kind === "effect" && n.parent === parent).map((n) => n.id);

  test("resolves true when the effect is in its new place in its lane's chain, which the session shows, and says nothing", async () => {
    const { client, notices, editor } = await open({ snapshot: movesFixture.snapshot });
    expect(await editor.moveEffect(6, 2)).toBe(true);
    expect(chain(client, 5)).toEqual([7, 8, 6, 9]);
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    // The first place: the effects it passes move the other way.
    expect(await editor.moveEffect(9, 0)).toBe(true);
    expect(chain(client, 5)).toEqual([9, 7, 8, 6]);
    expect(said(notices)).toEqual([]);
  });

  test("moves a bus effect in its group's bus", async () => {
    const { client, notices, editor } = await open({ snapshot: movesFixture.snapshot });
    expect(await editor.moveEffect(1, 3)).toBe(true);
    expect(chain(client, 0)).toEqual([2, 3, 4, 1]);
    expect(chain(client, 5)).toEqual([6, 7, 8, 9]);
    expect(said(notices)).toEqual([]);
  });

  test("is refused for what is no effect, a place the chain has not and the place the effect is at: it resolves false, and the page says what it could not do and why, with the place counted from 1", async () => {
    const { client, notices, editor } = await open({ snapshot: movesFixture.snapshot });
    // (The notices keep the last few: each is read as it is said.)
    for (const [node, index, words] of [
      [5, 0, "Could not move Oscillator 1 to place 1 of its chain: only an effect can be moved"],
      [6, 4, "Could not move Filter 3 to place 5 of its chain: the chain has no place 4"],
      [6, 0, "Could not move Filter 3 to place 1 of its chain: that effect is at that place already"],
      [MASTER, 1, "Could not move Master to place 2 of its chain: there is no node 4294967295"],
      [9999, 1, "Could not move that node to place 2 of its chain: there is no node 9999"],
    ] as const) {
      expect(await editor.moveEffect(node, index)).toBe(false);
      expect(said(notices).at(-1)).toBe(words);
    }
    expect(client.state.snapshot).toEqual(movesFixture.snapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open({ snapshot: movesFixture.snapshot });
    client.stop();
    expect(await editor.moveEffect(6, 2)).toBe(false);
    expect(said(notices)).toEqual(["Could not move Filter 3 to place 3 of its chain: Not connected to the server."]);
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

describe("muting and soloing a lane or a group", () => {
  const flags = (client: Client, node: number) => {
    const found = client.state.snapshot?.graph.nodes.find((n) => n.id === node);
    return found?.kind === "instrument" || found?.kind === "group" ? { muted: found.muted, solo: found.solo } : "neither";
  };

  test("resolves true when the lane has the flag, which the session shows, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.setMute(oscillator.id, true)).toBe(true);
    expect(flags(client, oscillator.id)).toEqual({ muted: true, solo: false });
    expect(await editor.setSolo(oscillator.id, true)).toBe(true);
    expect(flags(client, oscillator.id)).toEqual({ muted: true, solo: true });
    expect(await editor.setMute(oscillator.id, false)).toBe(true);
    expect(await editor.setSolo(oscillator.id, false)).toBe(true);
    expect(flags(client, oscillator.id)).toEqual({ muted: false, solo: false });
    expect(said(notices)).toEqual([]);
  });

  test("is refused for a node that is neither: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.setMute(filter.id, true)).toBe(false);
    expect(await editor.setSolo(lfo.id, true)).toBe(false);
    expect(await editor.setMute(filter.id, false)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not mute Filter: only a lane or a group can be muted",
      "Could not solo LFO: only a lane or a group can be soloed",
      "Could not unmute Filter: only a lane or a group can be muted",
    ]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.setMute(oscillator.id, true)).toBe(false);
    expect(await editor.setSolo(oscillator.id, true)).toBe(false);
    expect(said(notices)).toEqual(["Could not mute Oscillator: Not connected to the server.", "Could not solo Oscillator: Not connected to the server."]);
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

describe("pinning parameters to a node", () => {
  const pinnedOf = (client: Client, node: number) => {
    const found = client.state.snapshot?.graph.nodes.find((n) => n.id === node);
    return found?.kind === "instrument" || found?.kind === "effect" ? found.plugin?.pinned : undefined;
  };

  test("resolves true when the plugin has them pinned, in the order given, which the session shows, and says nothing; none gives the node back to the page's choice", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.setPinned(filter.id, [param(filter, "resonance"), param(filter, "cutoff")])).toBe(true);
    expect(pinnedOf(client, filter.id)).toEqual([param(filter, "resonance"), param(filter, "cutoff")]);
    expect(await editor.setPinned(oscillator.id, [param(oscillator, "detune")])).toBe(true);
    expect(pinnedOf(client, oscillator.id)).toEqual([param(oscillator, "detune")]);
    expect(await editor.setPinned(filter.id, [])).toBe(true);
    expect(pinnedOf(client, filter.id)).toEqual([]);
    expect(said(notices)).toEqual([]);
  });

  test("is refused as the controller refuses it: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    // (Each is read as it is said: the page keeps the last few notices only.)
    const refusals: [number, number[], string][] = [
      [424242, [], "Could not choose the knobs of that node: there is no node 424242"],
      [lfo.id, [0], "Could not choose the knobs of LFO: that node has no plugin"],
      [filter.id, [0, 0], "Could not choose the knobs of Filter: parameter 0 is listed twice"],
      [filter.id, [4000], "Could not choose the knobs of Filter: the plugin has no parameter 4000"],
    ];
    for (const [node, params, notice] of refusals) {
      expect(await editor.setPinned(node, params), notice).toBe(false);
      expect(said(notices).at(-1)).toBe(notice);
    }
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.setPinned(filter.id, [0])).toBe(false);
    expect(said(notices)).toEqual(["Could not choose the knobs of Filter: Not connected to the server."]);
  });
});

describe("showing a parameter as a knob on its node, or not", () => {
  /** The tracer with eight more parameters (indices 100 to 107) on the oscillator, which has eight: so that there are some the node does not show. */
  const wide = () => {
    const nodes = tracerSnapshot.graph.nodes.map((node) => {
      if (node.id !== oscillator.id || node.kind !== "instrument" || !node.plugin) return node;
      const more = Array.from({ length: 8 }, (_, i) => ({ ...(node.plugin?.params[0] ?? ({} as never)), index: 100 + i, name: `extra ${String(i)}` }));
      return { ...node, plugin: { ...node.plugin, params: [...node.plugin.params, ...more] } };
    });
    return { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes } };
  };
  const first = Array.from({ length: 8 }, (_, i) => i);
  const pinnedOf = (client: Client, node: number) => {
    const found = client.state.snapshot?.graph.nodes.find((n) => n.id === node);
    return found?.kind === "instrument" || found?.kind === "effect" ? found.plugin?.pinned : undefined;
  };

  test("pins one that is not shown after what is shown: the first eight stay, and the next is after them", async () => {
    const { client, notices, editor } = await open({ snapshot: wide() });
    expect(pinnedOf(client, oscillator.id)).toEqual([]);
    expect(await editor.pinParam(oscillator.id, 100, true)).toBe(true);
    expect(pinnedOf(client, oscillator.id)).toEqual([...first, 100]);
    expect(await editor.pinParam(oscillator.id, 105, true)).toBe(true);
    expect(pinnedOf(client, oscillator.id)).toEqual([...first, 100, 105]);
    expect(said(notices)).toEqual([]);
  });

  test("takes one off what is shown: with none pinned that is the first eight less that one, and the last one off gives the node back to the first eight", async () => {
    const { client, editor } = await open({ snapshot: wide() });
    expect(await editor.pinParam(oscillator.id, 3, false)).toBe(true);
    expect(pinnedOf(client, oscillator.id)).toEqual(first.filter((i) => i !== 3));
    await editor.setPinned(oscillator.id, [101]);
    expect(await editor.pinParam(oscillator.id, 101, false)).toBe(true);
    expect(pinnedOf(client, oscillator.id)).toEqual([]);
  });

  test("resolves true and asks nothing when it is so already", async () => {
    const { client, notices, editor } = await open({ snapshot: wide() });
    const before = client.state.snapshot?.rev;
    expect(await editor.pinParam(oscillator.id, 2, true)).toBe(true); // shown already
    expect(await editor.pinParam(oscillator.id, 104, false)).toBe(true); // not shown
    expect(await editor.pinParam(oscillator.id, 999, false)).toBe(true); // not there
    expect(client.state.snapshot?.rev).toBe(before);
    expect(said(notices)).toEqual([]);
  });

  test("says what it could not do, and why, for a parameter the plugin has not, a node with no plugin and a node that is not there", async () => {
    const { client, notices, editor } = await open({ snapshot: wide() });
    const refusals: [number, number, string][] = [
      [oscillator.id, 999, "Could not pin parameter 999 to Oscillator: the plugin has no parameter 999"],
      [lfo.id, 0, "Could not pin parameter 0 to LFO: that node has no plugin"],
      [424242, 0, "Could not pin parameter 0 to that node: there is no node 424242"],
    ];
    for (const [node, index, notice] of refusals) {
      expect(await editor.pinParam(node, index, true), notice).toBe(false);
      expect(said(notices).at(-1)).toBe(notice);
    }
    expect(client.state.snapshot).toEqual(wide());
  });

  test("names the parameter, and where it is pinned to or taken off, in what it says", async () => {
    const { notices, editor, client } = await open({ snapshot: wide() });
    client.stop();
    expect(await editor.pinParam(oscillator.id, 101, true)).toBe(false);
    expect(said(notices).at(-1)).toBe("Could not pin extra 1 to Oscillator: Not connected to the server.");
    expect(await editor.pinParam(oscillator.id, 3, false)).toBe(false);
    expect(said(notices).at(-1)).toBe("Could not unpin attack from Oscillator: Not connected to the server.");
  });

  test("says a node can show only so many, without asking the server, when it shows that many", async () => {
    const huge = (() => {
      const base = wide();
      const nodes = base.graph.nodes.map((node) => {
        if (node.id !== oscillator.id || node.kind !== "instrument" || !node.plugin) return node;
        const params = Array.from({ length: 70 }, (_, i) => ({ ...(node.plugin?.params[0] ?? ({} as never)), index: i, name: `p${String(i)}` }));
        return { ...node, plugin: { ...node.plugin, params, pinned: Array.from({ length: 64 }, (_, i) => i) } };
      });
      return { ...base, graph: { ...base.graph, nodes } };
    })();
    const { client, notices, editor } = await open({ snapshot: huge });
    const before = client.state.snapshot?.rev;
    expect(await editor.pinParam(oscillator.id, 65, true)).toBe(false);
    expect(said(notices).at(-1)).toBe("Could not pin p65 to Oscillator: a node can show at most 64 parameters");
    expect(client.state.snapshot?.rev).toBe(before);
  });
});

describe("arranging the nodes again", () => {
  const placeOf = (client: Client, node: number) => client.state.snapshot?.graph.nodes.find((n) => n.id === node)?.ui;

  test("gives every node that has a place back to the layout, the master too, and says nothing", async () => {
    const { client, notices, editor } = await open();
    await editor.moveNode(filter.id, { x: 1, y: 2 });
    await editor.moveNode(MASTER, { x: 3, y: 4 });
    await editor.moveNode(lfo.id, { x: 5, y: 6 });
    expect(await editor.resetLayout()).toBe(true);
    for (const node of client.state.snapshot?.graph.nodes ?? []) expect(placeOf(client, node.id), `node ${String(node.id)}`).toBeUndefined();
    expect(said(notices)).toEqual([]);
  });

  test("leaves the nodes that have no place alone: a request for each that has one, and none when there is none", async () => {
    const { server, editor } = await open();
    const sent = (): number => server.received.filter((message) => message.type === "request").length;
    expect(await editor.resetLayout()).toBe(true);
    expect(sent()).toBe(0);
    await editor.moveNode(filter.id, { x: 1, y: 2 });
    expect(sent()).toBe(1);
    await editor.resetLayout();
    expect(sent()).toBe(2);
    expect(server.received.at(-1)).toMatchObject({ type: "request", intent: { type: "setNodeUi", node: filter.id, ui: null } });
  });

  test("with no connection says so, for each node it could not give back (it goes on with the rest), and resolves false", async () => {
    const { client, notices, editor } = await open();
    await editor.moveNode(filter.id, { x: 1, y: 2 });
    await editor.moveNode(lfo.id, { x: 3, y: 4 });
    client.stop();
    expect(await editor.resetLayout()).toBe(false);
    expect(said(notices)).toEqual([
      "Could not give Filter back to the layout: Not connected to the server.",
      "Could not give LFO back to the layout: Not connected to the server.",
    ]);
  });
});

describe("saving the session under another name", () => {
  test("resolves true when the session has that file, which the session shows with nothing unsaved, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(client.state.snapshot?.meta.file).toBe("tracer.toml");
    expect(await editor.saveAs("copy")).toBe(true);
    expect(client.state.snapshot?.meta).toMatchObject({ file: "copy.toml", dirty: false });
    expect(said(notices)).toEqual([]);
  });

  test("asks for the name as it was typed, and says the name without the spaces round it", async () => {
    const { server, editor, notices } = await open();
    expect(await editor.saveAs("  ../x ")).toBe(false);
    expect(server.received.at(-1)).toMatchObject({ type: "request", intent: { type: "saveAs", name: "  ../x " } });
    expect(said(notices)).toEqual(['Could not save the session as "../x": a file name has no folder in it: `/` and `\\` are not allowed']);
  });

  test("says the name it was refused for in quotes and cut after 24 characters, as a group's name is, whatever else the name has", async () => {
    const { notices, editor } = await open();
    const folder = "a file name has no folder in it: `/` and `\\` are not allowed";
    const control = "a file name cannot have `:`, `*`, `?`, `\"`, `<`, `>`, `|` or a control character";
    // (The notices keep the last few: each is read as it is said. Exactly as many characters as it quotes is not cut, and one more is.)
    const cases: [string, string][] = [
      ["😀".repeat(200), `Could not save the session as "${"😀".repeat(24)}…": a file name is at most 255 bytes long`],
      ["two\nlines", `Could not save the session as "two\\nlines": ${control}`],
      ["a/".repeat(12), `Could not save the session as "${"a/".repeat(12)}": ${folder}`],
      [`${"a/".repeat(12)}a`, `Could not save the session as "${"a/".repeat(12)}…": ${folder}`],
    ];
    for (const [name, words] of cases) {
      expect(await editor.saveAs(name), words).toBe(false);
      expect(said(notices).at(-1)).toBe(words);
    }
  });

  test("is refused as the controller refuses it: false, the page says what it could not do and why, and nothing changes", async () => {
    const { client, notices, editor } = await open({ files: ["other.toml"] });
    expect(await editor.saveAs("other")).toBe(false);
    expect(await editor.saveAs("")).toBe(false);
    expect(await editor.saveAs("con")).toBe(false);
    expect(said(notices)).toEqual([
      'Could not save the session as "other": a file called other.toml is there already',
      'Could not save the session as "": a file name is needed',
      'Could not save the session as "con": con is a name that Windows keeps for itself',
    ]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.saveAs("copy")).toBe(false);
    expect(said(notices)).toEqual(['Could not save the session as "copy": Not connected to the server.']);
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

describe("setting the tempo", () => {
  test("resolves true when the tempo is set, which the session shows, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(client.state.snapshot?.meta.bpm).toBe(120);
    expect(await editor.setBpm(96.5)).toBe(true);
    expect(client.state.snapshot?.meta.bpm).toBe(96.5);
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    expect(said(notices)).toEqual([]);
  });

  test("holds a tempo to the tempos there can be, which the session shows", async () => {
    const { client, editor } = await open();
    expect(await editor.setBpm(1000)).toBe(true);
    expect(client.state.snapshot?.meta.bpm).toBe(300);
    expect(await editor.setBpm(5)).toBe(true);
    expect(client.state.snapshot?.meta.bpm).toBe(20);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.setBpm(96.5)).toBe(false);
    expect(said(notices)).toEqual(["Could not set the tempo to 96.5: Not connected to the server."]);
  });
});

describe("the pattern of a lane", () => {
  const patternOf = (client: Client, node: number = oscillator.id) => {
    const found = client.state.snapshot?.graph.nodes.find((n) => n.id === node);
    return found?.kind === "instrument" ? found.pattern : "not a lane";
  };
  /** The pattern a lane is given when it has none and something is asked of it: four beats, looping, not in key, not heard, at the tempo of the session. */
  const empty = { recording: false, enabled: false, looping: true, inKey: false, lengthBeats: 4, bpm: 120, notes: 0 };

  test("a recording is started and stopped: the session shows the pattern of the lane, recording and then not, and nothing is said", async () => {
    const { client, notices, editor } = await open();
    expect(patternOf(client)).toBeNull();
    expect(await editor.setRecording(oscillator.id, true)).toBe(true);
    expect(patternOf(client)).toEqual({ ...empty, recording: true });
    expect(await editor.setRecording(oscillator.id, false)).toBe(true);
    expect(patternOf(client)).toEqual(empty);
    expect(said(notices)).toEqual([]);
  });

  test("a setting is set, held to its range and a switch rounded, and a lane that has no pattern is given an empty one with it", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.setPatternField(oscillator.id, "length", 8)).toBe(true);
    expect(patternOf(client)).toEqual({ ...empty, lengthBeats: 8 });
    expect(await editor.setPatternField(oscillator.id, "length", 100)).toBe(true);
    expect(patternOf(client)).toEqual({ ...empty, lengthBeats: 32 });
    expect(await editor.setPatternField(oscillator.id, "enabled", 1)).toBe(true);
    expect(await editor.setPatternField(oscillator.id, "looping", 0.4)).toBe(true);
    expect(await editor.setPatternField(oscillator.id, "transpose", 0.6)).toBe(true);
    expect(patternOf(client)).toEqual({ ...empty, lengthBeats: 32, enabled: true, looping: false, inKey: true });
    expect(said(notices)).toEqual([]);
  });

  test("a pattern is cleared, and what has none cannot be cleared: it resolves false, and the page says so", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.clearPattern(oscillator.id)).toBe(false);
    expect(await editor.setPatternField(oscillator.id, "enabled", 1)).toBe(true);
    expect(await editor.clearPattern(oscillator.id)).toBe(true);
    expect(patternOf(client)).toBeNull();
    expect(said(notices)).toEqual(["Could not clear the pattern of Oscillator: that lane has no pattern"]);
  });

  /** What the page says of each thing that is asked and refused, in turn (it keeps three notices at a time, so each is read as it comes). */
  const refusals = async (notices: Notices, asks: readonly (() => Promise<boolean>)[]): Promise<string[]> => {
    const words: string[] = [];
    const stop = notices.subscribe(() => {
      words.push(said(notices).at(-1) ?? "nothing was said");
    });
    for (const ask of asks) expect(await ask()).toBe(false);
    stop();
    return words;
  };

  test("is refused for a node that is not a lane, in the words of what it was to do, and the session is as it was", async () => {
    const { client, notices, editor } = await open();
    expect(
      await refusals(notices, [
        () => editor.setRecording(filter.id, true),
        () => editor.setRecording(lfo.id, false),
        () => editor.clearPattern(filter.id),
        () => editor.setPatternField(lfo.id, "length", 2),
        () => editor.setPatternField(MASTER, "enabled", 1),
      ]),
    ).toEqual([
      "Could not start recording a pattern on Filter: a pattern belongs to a lane",
      "Could not stop recording the pattern of LFO: a pattern belongs to a lane",
      "Could not clear the pattern of Filter: a pattern belongs to a lane",
      "Could not set the length of the pattern of LFO to 2 beats: a pattern belongs to a lane",
      `Could not turn the pattern of Master on: there is no node ${String(MASTER)}`,
    ]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so, in the words of each setting it was to change", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    const why = "Not connected to the server.";
    expect(
      await refusals(notices, [
        () => editor.setRecording(oscillator.id, true),
        () => editor.setRecording(oscillator.id, false),
        () => editor.clearPattern(oscillator.id),
        () => editor.setPatternField(oscillator.id, "length", 2.5),
        () => editor.setPatternField(oscillator.id, "enabled", 1),
        () => editor.setPatternField(oscillator.id, "enabled", 0),
        () => editor.setPatternField(oscillator.id, "looping", 1),
        () => editor.setPatternField(oscillator.id, "looping", 0),
        () => editor.setPatternField(oscillator.id, "transpose", 1),
        () => editor.setPatternField(oscillator.id, "transpose", 0),
      ]),
    ).toEqual([
      `Could not start recording a pattern on Oscillator: ${why}`,
      `Could not stop recording the pattern of Oscillator: ${why}`,
      `Could not clear the pattern of Oscillator: ${why}`,
      `Could not set the length of the pattern of Oscillator to 2.5 beats: ${why}`,
      `Could not turn the pattern of Oscillator on: ${why}`,
      `Could not turn the pattern of Oscillator off: ${why}`,
      `Could not make the pattern of Oscillator loop: ${why}`,
      `Could not make the pattern of Oscillator play once: ${why}`,
      `Could not transpose the pattern of Oscillator by the degrees of the scale: ${why}`,
      `Could not transpose the pattern of Oscillator by semitones: ${why}`,
    ]);
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
    expect(await editor.addCable(lfo.id, oscillator.id, paramPort(oscillator, "detune"))).toBe(true);
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
    expect(await editor.addCable(lfo.id, oscillator.id, paramPort(oscillator, "detune"))).toBe(true);
    expect(client.state.snapshot?.graph.edges.filter((e) => e.kind === "modulation")).toHaveLength(2);
    expect(said(notices)).toEqual([]);
  });

  test("that is refused says which cable, in the names the canvas has, and why", async () => {
    const { notices, editor } = await open();
    const cutoff = paramPort(filter, "cutoff");
    // (The LFO drives the cutoff already.)
    expect(await editor.addCable(lfo.id, filter.id, cutoff)).toBe(false);
    expect(await editor.addCable(oscillator.id, filter.id, cutoff)).toBe(false);
    expect(await editor.addCable(lfo.id, filter.id, { type: "param", index: 999 })).toBe(false);
    expect(said(notices)).toEqual([
      "Could not draw a cable from LFO to Filter: cutoff: that modulator drives that parameter already",
      "Could not draw a cable from Oscillator to Filter: cutoff: a cable starts at a modulator",
      "Could not draw a cable from LFO to Filter: the plugin has no parameter 999",
    ]);
  });

  test("names two nodes of one name by their numbers", async () => {
    const { notices, editor } = await open();
    await editor.addModulator(oscillator.id, "lfo");
    expect(await editor.addCable(3, filter.id, paramPort(filter, "cutoff"))).toBe(true);
    expect(await editor.addCable(3, filter.id, paramPort(filter, "cutoff"))).toBe(false);
    expect(said(notices)).toEqual(["Could not draw a cable from LFO 2 to Filter: cutoff: that modulator drives that parameter already"]);
  });
});

describe("drawing a cable to a setting of another modulator", () => {
  /** The tracer with an envelope added to the oscillator's lane, whose node is 3. */
  async function withEnvelope() {
    const rig = await open();
    expect(await rig.editor.addModulator(oscillator.id, "envelope")).toBe(3);
    return rig;
  }

  test("joins a modulator to the rate of an LFO or a stage of an envelope, and says nothing", async () => {
    const { client, notices, editor } = await withEnvelope();
    expect(await editor.addCable(3, lfo.id, { type: "rate" })).toBe(true);
    expect(await editor.addCable(lfo.id, 3, { type: "attack" })).toBe(true);
    const cables = client.state.snapshot?.graph.edges.filter((e) => e.kind === "modulation").map((e) => [e.from.node, e.to.node, e.to.port.type]);
    // (A modulator's cables are listed together, in the order of the modulators.)
    expect(cables).toEqual([
      [lfo.id, filter.id, "param"],
      [lfo.id, 3, "attack"],
      [3, lfo.id, "rate"],
    ]);
    expect(said(notices)).toEqual([]);
  });

  test("that is refused says which cable, in the names the canvas has, and why", async () => {
    const { notices, editor } = await withEnvelope();
    expect(await editor.addCable(3, lfo.id, { type: "rate" })).toBe(true);
    // (Each is said when it is refused: only the last few notices are kept.)
    const refused: [string, () => Promise<boolean>][] = [
      ["Could not draw a cable from ADSR to LFO: rate: that modulator drives that setting already", () => editor.addCable(3, lfo.id, { type: "rate" })],
      ["Could not draw a cable from LFO to ADSR: rate: that modulator has no rate", () => editor.addCable(lfo.id, 3, { type: "rate" })],
      ["Could not draw a cable from ADSR to ADSR: decay: a modulator cannot drive its own settings", () => editor.addCable(3, 3, { type: "decay" })],
      ["Could not draw a cable from LFO to Oscillator: rate: a cable ends at the settings of a modulator, and that is not one", () => editor.addCable(lfo.id, oscillator.id, { type: "rate" })],
      ["Could not draw a cable from LFO to that node: rate: there is no node 99", () => editor.addCable(lfo.id, 99, { type: "rate" })],
      ["Could not draw a cable from LFO to ADSR: a cable can only end at a plugin's parameter, or at a modulator's rate, stage or depth", () => editor.addCable(lfo.id, 3, { type: "modOut" })],
    ];
    for (const [words, draw] of refused) {
      expect(await draw()).toBe(false);
      expect(said(notices).at(-1)).toBe(words);
    }
  });

  test("names two modulators of one name by their numbers", async () => {
    const { notices, editor } = await open();
    expect(await editor.addModulator(oscillator.id, "lfo")).toBe(3);
    expect(await editor.addCable(3, lfo.id, { type: "rate" })).toBe(true);
    expect(await editor.addCable(3, lfo.id, { type: "rate" })).toBe(false);
    expect(said(notices)).toEqual(["Could not draw a cable from LFO 2 to LFO 1: rate: that modulator drives that setting already"]);
  });

  test("is told of, when it cannot be taken out, by the setting it ends at", async () => {
    const { client, notices, editor } = await withEnvelope();
    expect(await editor.addCable(3, lfo.id, { type: "rate" })).toBe(true);
    client.stop();
    expect(await editor.removeCable(1)).toBe(false);
    expect(said(notices)).toEqual(["Could not take out the cable ADSR → LFO: rate: Not connected to the server."]);
  });
});

describe("drawing a cable to the depth of a cable", () => {
  /** The tracer with an envelope added to the oscillator's lane, whose node is 3; the LFO's cable to the filter's cutoff is the cable 0. */
  async function withEnvelope() {
    const rig = await open();
    expect(await rig.editor.addModulator(oscillator.id, "envelope")).toBe(3);
    return rig;
  }

  test("joins a modulator to how deep a cable of another goes, and says nothing", async () => {
    const { client, notices, editor } = await withEnvelope();
    expect(await editor.addCable(3, lfo.id, { type: "depth", edge: 0 })).toBe(true);
    const ends = client.state.snapshot?.graph.edges.filter((e) => e.kind === "modulation").map((e) => [e.from.node, e.to.node, e.to.port]);
    expect(ends).toEqual([
      [lfo.id, filter.id, paramPort(filter, "cutoff")],
      [3, lfo.id, { type: "depth", edge: 0 }],
    ]);
    expect(said(notices)).toEqual([]);
  });

  test("that is refused says which cable, with the end told by the cable whose depth it is, and why", async () => {
    const { notices, editor } = await withEnvelope();
    expect(await editor.addCable(3, lfo.id, { type: "depth", edge: 0 })).toBe(true);
    const refused: [string, () => Promise<boolean>][] = [
      ["Could not draw a cable from ADSR to depth of LFO to Filter: cutoff: that modulator drives that setting already", () => editor.addCable(3, lfo.id, { type: "depth", edge: 0 })],
      ["Could not draw a cable from LFO to depth of LFO to Filter: cutoff: a modulator cannot drive its own settings", () => editor.addCable(lfo.id, lfo.id, { type: "depth", edge: 0 })],
      ["Could not draw a cable from ADSR to LFO: depth: there is no cable 99", () => editor.addCable(3, lfo.id, { type: "depth", edge: 99 })],
    ];
    for (const [words, draw] of refused) {
      expect(await draw()).toBe(false);
      expect(said(notices).at(-1)).toBe(words);
    }
  });

  test("is told of, when it cannot be taken out, by the cable whose depth it ends at", async () => {
    const { client, notices, editor } = await withEnvelope();
    expect(await editor.addCable(3, lfo.id, { type: "depth", edge: 0 })).toBe(true);
    client.stop();
    expect(await editor.removeCable(1)).toBe(false);
    expect(said(notices)).toEqual(["Could not take out the cable ADSR → depth of LFO → Filter: cutoff: Not connected to the server."]);
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

  test("that cannot be taken out (the last lane of a session, a node that is not there) says what and why", async () => {
    const { notices, editor } = await open();
    expect(await editor.removeNode(oscillator.id)).toBe(false);
    expect(await editor.removeNode(999)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not remove Oscillator: a session keeps its last lane",
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

describe("undoing and redoing", () => {
  const volume = (client: Client): number | undefined => {
    const node = client.state.snapshot?.graph.nodes.find((n) => n.id === oscillator.id);
    return node?.kind === "instrument" ? node.volume : undefined;
  };

  test("put back what a change set and set it again, and say nothing", async () => {
    const { client, notices, editor } = await open();
    const before = volume(client);
    await client.request({ type: "setGain", node: oscillator.id, gain: "volume", value: 0.25 });
    expect(await editor.undo()).toBe(true);
    expect(volume(client)).toBe(before);
    expect(await editor.redo()).toBe(true);
    expect(volume(client)).toBe(0.25);
    expect(said(notices)).toEqual([]);
  });

  test("with nothing to undo or redo are refused, and say so in a notice", async () => {
    const { notices, editor } = await open();
    expect(await editor.undo()).toBe(false);
    expect(await editor.redo()).toBe(false);
    expect(said(notices)).toEqual(["Could not undo: there is nothing to undo", "Could not redo: there is nothing to redo"]);
  });

  test("when the connection is gone say so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.undo()).toBe(false);
    expect(await editor.redo()).toBe(false);
    expect(said(notices)).toEqual(["Could not undo: Not connected to the server.", "Could not redo: Not connected to the server."]);
  });
});
