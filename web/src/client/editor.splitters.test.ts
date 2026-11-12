import { afterEach, describe, expect, test } from "vitest";

import type { Intent } from "../protocol";
import { Client } from "./connection";
import { splittersFixture } from "./fixtures";
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

/** An editor of a session on a mock engine, with its notices and the nodes that it was asked to show. */
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
/** What the notice board said last (it keeps only the last few). */
const lastSaid = (notices: Notices): string | undefined => said(notices).at(-1);
/** The kinds of intent that the clients asked, in order. */
const asked = (server: MockServer): string[] => server.received.flatMap((message) => (message.type === "request" ? [message.intent.type] : []));

// The session of the fixture: the lanes (0, 1, 2), the keys (3) and a device (4), a box called Bass (5), two splitters, Bass line (6, which picks the lowest note) and one called Splitter (7, which picks the highest), and cables of notes: the keys to 6, the
// box and the device to 7, the pick of 6 to the lane 0, its rest to the lane 1 and the pick of 7 to the lane 2.
const session = (name: string) => {
  const found = splittersFixture.cases.find((each) => each.name === name);
  if (!found) throw new Error(`the fixture has a session called ${name}`);
  return found.snapshot;
};
const [LANE0, LANE1, KEYS, FIRST, SECOND, BASS] = [0, 1, 3, 6, 7, 5];

describe("adding a note splitter", () => {
  test("resolves with the node it made, which is shown and is in the session then, called by the lowest number there is, and says nothing", async () => {
    const { client, notices, shown, editor } = await open({ snapshot: session("bare") });
    const id = await editor.addSplitter();
    expect(id).toBe(2);
    expect(client.state.snapshot?.graph.nodes.find((n) => n.id === id)).toMatchObject({ kind: "splitter", name: "Splitter 1", pick: "lowest" });
    expect(shown).toEqual([2]);
    expect(said(notices)).toEqual([]);
    // The next is the next.
    expect(await editor.addSplitter()).toBe(3);
    expect(client.state.snapshot?.graph.nodes.find((n) => n.id === 3)?.name).toBe("Splitter 2");
  });

  test("puts the splitter where a menu of the canvas says, which is a move of its own, and does not ask the canvas to show it", async () => {
    const { server, client, shown, notices, editor } = await open({ snapshot: session("bare") });
    const id = await editor.addSplitter({ x: 120, y: 80 });
    expect(asked(server)).toEqual(["addSplitter", "setNodeUi"]);
    expect(client.state.snapshot?.graph.nodes.find((n) => n.id === id)?.ui).toEqual({ x: 120, y: 80 });
    expect(shown).toEqual([]);
    expect(said(notices)).toEqual([]);
  });

  test("resolves with null and says why when the session has all it can have: a notice, and no node", async () => {
    const { client, notices, shown, editor } = await open({ snapshot: session("full") });
    const before = client.state.snapshot?.graph.nodes.length;
    expect(await editor.addSplitter()).toBeNull();
    expect(lastSaid(notices)).toBe("Could not add a note splitter: a session has at most 16 note splitters");
    expect(client.state.snapshot?.graph.nodes.length).toBe(before);
    expect(shown).toEqual([]);
  });

  test("is one step of the history, which undo takes out and redo puts back", async () => {
    const { server, client, editor } = await open({ snapshot: session("bare") });
    await editor.addSplitter();
    expect(server.history.undo).toBe("add a splitter");
    expect(await editor.undo()).toBe(true);
    await until(() => !client.state.snapshot?.graph.nodes.some((n) => n.kind === "splitter"), "the splitter to go");
    expect(await editor.redo()).toBe(true);
    await until(() => client.state.snapshot?.graph.nodes.some((n) => n.kind === "splitter") === true, "the splitter to come back");
  });
});

describe("the settings of a note splitter", () => {
  test("are set with the editor's own method, in the words of the splitter's name, and are steps of the history in the words of the controller's", async () => {
    const { server, client, notices, editor } = await open({ snapshot: session("splitters") });
    expect(await editor.setSplitterField(FIRST, { type: "name", name: "  Lead " })).toBe(true);
    expect(client.state.snapshot?.graph.nodes.find((n) => n.id === FIRST)?.name).toBe("Lead");
    expect(server.history.undo).toBe("rename Bass line");
    expect(await editor.setSplitterField(FIRST, { type: "pick", pick: "highest" })).toBe(true);
    expect(client.state.snapshot?.graph.nodes.find((n) => n.id === FIRST)).toMatchObject({ kind: "splitter", pick: "highest" });
    expect(server.history.undo).toBe("let Lead pick the highest note");
    expect(said(notices)).toEqual([]);
    expect(asked(server)).toEqual(["setSplitterField", "setSplitterField"]);
  });

  test("are no step when the splitter has what is asked for already, and are refused with the reason as a notice when the node is no splitter", async () => {
    const { server, notices, editor } = await open({ snapshot: session("splitters") });
    expect(await editor.setSplitterField(FIRST, { type: "pick", pick: "lowest" })).toBe(true);
    expect(server.history.undo).toBeNull();
    expect(await editor.setSplitterField(LANE0, { type: "pick", pick: "highest" })).toBe(false);
    expect(lastSaid(notices)).toBe("Could not set the note that Oscillator 1 picks: that node is not a note splitter");
    expect(await editor.setSplitterField(KEYS, { type: "name", name: "x" })).toBe(false);
    expect(lastSaid(notices)).toBe("Could not rename Keys: that node is not a note splitter");
    expect(await editor.setSplitterField(FIRST, { type: "name", name: "x".repeat(65) })).toBe(false);
    expect(lastSaid(notices)).toBe("Could not rename Bass line: a name is at most 64 characters");
  });
});

describe("the cables of notes of a note splitter", () => {
  test("are drawn from either output with the editor's own method, in the words of the output they start at, and the rest is a cable at the second port", async () => {
    const { server, client, notices, editor } = await open({ snapshot: session("splitters") });
    expect(await editor.addNotesCable(FIRST, LANE1)).toBe(true);
    expect(await editor.addNotesCable(FIRST, 2, "rest")).toBe(true);
    const cables = client.state.snapshot?.graph.edges.filter((e) => e.kind === "events") ?? [];
    expect(cables.slice(-2).map((e) => [e.from.node, e.from.port.type, e.to.node])).toEqual([
      [FIRST, "eventsOut", LANE1],
      [FIRST, "eventsOutRest", 2],
    ]);
    expect(said(notices)).toEqual([]);
    expect(server.history.undo).toBe("draw a cable of notes");
    const intents: Intent["type"][] = ["addCable", "addCable"];
    expect(asked(server)).toEqual(intents);
  });

  test("are refused with the reason, in the words of the output they start at, when the cable cannot be drawn", async () => {
    const { notices, editor } = await open({ snapshot: session("splitters") });
    expect(await editor.addNotesCable(FIRST, LANE1, "rest")).toBe(false);
    expect(lastSaid(notices)).toBe("Could not draw a cable of notes from Bass line (rest) to Oscillator 2: there is a cable between those two already");
    expect(await editor.addNotesCable(FIRST, SECOND)).toBe(false);
    expect(lastSaid(notices)).toBe("Could not draw a cable of notes from Bass line to Splitter: a note splitter cannot take the notes of another splitter: it takes those of a source or a pattern box");
    expect(await editor.addNotesCable(FIRST, BASS, "rest")).toBe(false);
    expect(lastSaid(notices)).toBe("Could not draw a cable of notes from Bass line (rest) to Bass: a note splitter cannot send its notes to a pattern box yet: it sends them to a lane");
    expect(await editor.addNotesCable(KEYS, LANE0, "rest")).toBe(false);
    expect(lastSaid(notices)).toBe("Could not draw a cable of notes from Keys (rest) to Oscillator 1: only a note splitter has a second output of events");
  });
});
