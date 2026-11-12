import { afterEach, describe, expect, test } from "vitest";

import type { Intent, PatternNote } from "../protocol";
import { Client } from "./connection";
import { patternBoxesFixture } from "./fixtures";
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
const note = (start: number, length: number, pitch: number, velocity: number): PatternNote => ({ start, length, note: pitch, velocity });

// The session of the fixture: the keys (2) and a device (3), two boxes, Bass (4, 8 beats, with two notes) and a plain one (5), two lanes (0, 1), and three cables of notes.
const session = (name: string) => {
  const found = patternBoxesFixture.cases.find((each) => each.name === name);
  if (!found) throw new Error(`the fixture has a session called ${name}`);
  return found.snapshot;
};
const [BASS, PLAIN, LANE0, KEYS] = [4, 5, 0, 2];

describe("adding a pattern box", () => {
  test("resolves with the node it made, which is shown and is in the session then, called by the lowest number there is, and says nothing", async () => {
    const { client, notices, shown, editor } = await open({ snapshot: session("bare") });
    const id = await editor.addPattern();
    expect(id).toBe(2);
    expect(client.state.snapshot?.graph.nodes.find((n) => n.id === id)).toMatchObject({ kind: "pattern", name: "Pattern 1", notes: [], looping: true, lengthBeats: 4 });
    expect(shown).toEqual([2]);
    expect(said(notices)).toEqual([]);
    // The next is the next.
    expect(await editor.addPattern()).toBe(3);
    expect(client.state.snapshot?.graph.nodes.find((n) => n.id === 3)?.name).toBe("Pattern 2");
  });

  test("puts the box where a menu of the canvas says, which is a move of its own, and does not ask the canvas to show it", async () => {
    const { server, client, shown, editor } = await open({ snapshot: session("bare") });
    const id = await editor.addPattern({ x: 120, y: 80 });
    expect(asked(server)).toEqual(["addPattern", "setNodeUi"]);
    expect(client.state.snapshot?.graph.nodes.find((n) => n.id === id)?.ui).toEqual({ x: 120, y: 80 });
    expect(shown).toEqual([]);
  });

  test("finds its own box when another front-end added one at the same time, as the last there is", async () => {
    const { server, editor } = await open({ snapshot: session("bare") });
    server.apply({ type: "addPattern" }); // another front-end's, before this one is answered
    const id = await editor.addPattern();
    expect(server.snapshot.graph.nodes.filter((n) => n.kind === "pattern").map((n) => n.id)).toEqual([2, 3]);
    expect(id).toBe(3);
  });

  test("is refused with the reason as a notice when the session has the most boxes there can be, and resolves with null", async () => {
    const { server, notices, shown, editor } = await open({ snapshot: session("full") });
    expect(await editor.addPattern()).toBeNull();
    expect(said(notices)).toEqual(["Could not add a pattern box: a session has at most 16 pattern boxes"]);
    expect(shown).toEqual([]);
    expect(server.snapshot.graph.nodes.filter((n) => n.kind === "pattern").length).toBe(16);
  });
});

describe("the settings of a pattern box", () => {
  test("are each asked of the server, which keeps them: the name, the length, the switches and whose beat the box goes by", async () => {
    const { server, notices, editor } = await open({ snapshot: session("boxes") });
    const box = () => server.snapshot.graph.nodes.find((n) => n.id === BASS);
    expect(await editor.setPatternBoxField(BASS, { type: "name", name: " Lead " })).toBe(true);
    expect(await editor.setPatternBoxField(BASS, { type: "lengthBeats", beats: 2.5 })).toBe(true);
    expect(await editor.setPatternBoxField(BASS, { type: "looping", on: false })).toBe(true);
    expect(await editor.setPatternBoxField(BASS, { type: "sync", sync: "host" })).toBe(true);
    expect(await editor.setPatternBoxField(BASS, { type: "overdub", on: false })).toBe(true);
    expect(await editor.setPatternBoxField(BASS, { type: "click", on: false })).toBe(true);
    expect(box()).toMatchObject({ name: "Lead", lengthBeats: 2.5, looping: false, sync: "host", overdub: false, click: false });
    expect(said(notices)).toEqual([]);
  });

  test("are refused with the reason as a notice that says what could not be done, in the name the canvas shows, and resolve with false", async () => {
    const { server, notices, editor } = await open({ snapshot: session("boxes") });
    const refused: [number, Parameters<Editor["setPatternBoxField"]>[1], string][] = [
      [BASS, { type: "name", name: "x".repeat(65) }, "Could not rename Bass: a name is at most 64 characters"],
      [LANE0, { type: "lengthBeats", beats: 2 }, "Could not set the length of Oscillator 1: that node is not a pattern box"],
      [LANE0, { type: "looping", on: false }, "Could not turn looping on or off for Oscillator 1: that node is not a pattern box"],
      [KEYS, { type: "sync", sync: "host" }, "Could not set whose beat Keys goes by: that node is not a pattern box"],
      [LANE0, { type: "overdub", on: true }, "Could not turn overdub on or off for Oscillator 1: that node is not a pattern box"],
      [900, { type: "click", on: false }, "Could not turn the click on or off for that node: there is no node 900"],
    ];
    for (const [node, field, notice] of refused) {
      expect(await editor.setPatternBoxField(node, field), notice).toBe(false);
      expect(lastSaid(notices), notice).toBe(notice);
    }
    expect(await editor.setPatternBoxField(PLAIN, { type: "overdub", on: true })).toBe(true);
    expect(server.snapshot.graph.nodes.find((n) => n.id === BASS)?.name).toBe("Bass");
  });
});

describe("what a pattern box is told to do", () => {
  test("is asked of the server for each of the four things, which keeps no step of it and leaves the box its notes", async () => {
    const { server, notices, editor } = await open({ snapshot: session("boxes") });
    for (const action of ["record", "finishRecording", "play", "stop"] as const) {
      expect(await editor.patternBoxTransport(BASS, action), action).toBe(true);
    }
    expect(asked(server)).toEqual(["patternBoxTransport", "patternBoxTransport", "patternBoxTransport", "patternBoxTransport"]);
    // (It is no step: there is nothing to undo, and the box has the notes it had.)
    expect(server.history).toEqual({ undo: null, redo: null });
    const bass = server.snapshot.graph.nodes.find((n) => n.id === BASS);
    expect(bass?.kind === "pattern" ? bass.notes : []).toEqual([note(0, 1, 36, 100), note(2, 0.5, 43, 90)]);
    expect(said(notices)).toEqual([]);
  });

  test("is refused with the reason as a notice, in the words of each, when the node is no box", async () => {
    const { notices, editor } = await open({ snapshot: session("boxes") });
    for (const [action, words] of [
      ["record", "record into"],
      ["finishRecording", "finish recording into"],
      ["play", "play"],
      ["stop", "stop"],
    ] as const) {
      expect(await editor.patternBoxTransport(LANE0, action), action).toBe(false);
      expect(lastSaid(notices), action).toBe(`Could not ${words} Oscillator 1: that node is not a pattern box`);
    }
  });
});

describe("the notes of a pattern box", () => {
  test("are cleared and set as a box holds them, and undo gives back what was cleared", async () => {
    const { server, notices, editor } = await open({ snapshot: session("boxes") });
    const notes = () => {
      const box = server.snapshot.graph.nodes.find((n) => n.id === BASS);
      return box?.kind === "pattern" ? box.notes : undefined;
    };
    expect(notes()).toEqual([note(0, 1, 36, 100), note(2, 0.5, 43, 90)]);
    expect(await editor.clearPatternBox(BASS)).toBe(true);
    expect(notes()).toEqual([]);
    expect(server.history.undo).toBe("clear Bass");
    expect(await editor.setPatternBoxNotes(BASS, [note(1, 1, 62, 0), note(0, 1, 60, 100)])).toBe(true);
    expect(notes()).toEqual([note(0, 1, 60, 100), note(1, 1, 62, 1)]);
    expect(server.history.undo).toBe("edit Bass");
    expect(said(notices)).toEqual([]);
  });

  test("are refused with the reason as a notice when the node is no box", async () => {
    const { notices, editor } = await open({ snapshot: session("boxes") });
    expect(await editor.clearPatternBox(LANE0)).toBe(false);
    expect(lastSaid(notices)).toBe("Could not clear Oscillator 1: that node is not a pattern box");
    expect(await editor.setPatternBoxNotes(LANE0, [])).toBe(false);
    expect(lastSaid(notices)).toBe("Could not edit the notes of Oscillator 1: that node is not a pattern box");
  });
});

describe("the cables of notes of a pattern box", () => {
  test("are drawn with the editor's own method, in the words of the boxes' names, and refused with the reason when a box would send to a box", async () => {
    const { server, notices, editor } = await open({ snapshot: session("boxes") });
    expect(await editor.addNotesCable(3, PLAIN)).toBe(true);
    expect(server.snapshot.graph.edges.filter((e) => e.kind === "events").length).toBe(4);
    expect(await editor.addNotesCable(BASS, PLAIN)).toBe(false);
    expect(said(notices)).toEqual(["Could not draw a cable of notes from Bass to Pattern: a pattern box cannot send its notes to another box yet: it sends them to a lane"]);
    const intents: Intent["type"][] = ["addCable", "addCable"];
    expect(asked(server)).toEqual(intents);
  });
});
