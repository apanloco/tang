import { afterEach, describe, expect, test, vi } from "vitest";

import type { Node, NodeId, PatternNote, PatternStatus, ServerMessage, Snapshot } from "../protocol";
import { patternBoxesFixture } from "./fixtures";
import { MockConnection, MockServer } from "./mock";

afterEach(() => {
  vi.useRealTimers();
});

// What the mock engine does with the pattern boxes that it is told to record, play and stop, which the controller leaves to the audio thread: the box counts in, takes a pass, plays; what it is doing is told to the clients as a session (there is no event for it),
// and a pass that ends is the notes of the box, which the controller folds in as a step of the history. What the real player makes of takes is pinned in `patternBoxTakes.test.ts`; this is what the server does round it. The tempo is 120 a minute: a beat is
// half a second. The box is the plain one of `patternBoxes.json` (5, called Pattern, 4 beats, no notes, a cable out and none in) or Bass (4, 8 beats, overdubbing, two notes, a cable from the keys).
const PLAIN = 5;
const BASS = 4;
const BEAT = 0.5;

const sessionOf = (name: string): Snapshot => {
  const found = patternBoxesFixture.cases.find((each) => each.name === name)?.snapshot;
  if (!found) throw new Error(`the fixture has a session called ${name}`);
  return found;
};

/** A connection to the server and what it is told (the client of a page, with no page). */
function listener(server: MockServer) {
  const told: ServerMessage[] = [];
  const connection = new MockConnection(server);
  connection.connect({
    open: () => undefined,
    text: (text) => {
      told.push(JSON.parse(text) as ServerMessage);
    },
    binary: () => undefined,
    close: () => undefined,
  });
  server.receive(connection, JSON.stringify({ type: "subscribe" }));
  return { connection, told };
}

/** A mock engine whose clock is the test's own, with a connection that has subscribed. */
async function engine(snapshot: Snapshot = sessionOf("boxes")) {
  const time = { seconds: 1000 };
  const server = new MockServer({ snapshot, clock: () => time.seconds });
  const { connection, told } = listener(server);
  const flush = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));
  await flush();
  const key = (note: number, velocity = 100): void => {
    server.receive(connection, JSON.stringify({ type: "noteOn", note, velocity }));
  };
  const release = (note: number): void => {
    server.receive(connection, JSON.stringify({ type: "noteOff", note }));
  };
  /** Go on by `beats` beats and look, as the server's own timer does. */
  const beats = (count: number): void => {
    time.seconds += count * BEAT;
    server.settle();
  };
  const transport = (node: NodeId, action: "record" | "finishRecording" | "play" | "stop"): void => {
    server.apply({ type: "patternBoxTransport", node, action });
  };
  return { server, time, told, key, release, beats, transport, flush, connection };
}

const boxOf = (server: MockServer, node: NodeId = PLAIN): Extract<Node, { kind: "pattern" }> => {
  const found = server.snapshot.graph.nodes.find((n) => n.id === node);
  if (found?.kind !== "pattern") throw new Error(`node ${String(node)} is a box`);
  return found;
};
const statusOf = (server: MockServer, node: NodeId = PLAIN): PatternStatus => boxOf(server, node).status;
const note = (start: number, length: number, pitch: number, velocity: number): PatternNote => ({ start, length, note: pitch, velocity });
const snapshots = (told: ServerMessage[]): Snapshot[] => told.flatMap((message) => (message.type === "snapshot" ? [message.snapshot] : []));

describe("a pattern box that is told to record", () => {
  test("counts in, takes a pass and plays, the clients being told a session each time that it is doing something else, and the session not being unsaved by it", async () => {
    const { server, told, beats, transport, flush } = await engine();
    expect(statusOf(server)).toBe("stopped");
    const before = snapshots(told).length;
    transport(PLAIN, "record");
    expect(statusOf(server)).toBe("countingIn");
    beats(3.5);
    expect(statusOf(server)).toBe("countingIn");
    beats(0.5);
    expect(statusOf(server)).toBe("recording");
    beats(3.5);
    expect(statusOf(server)).toBe("recording");
    expect(server.snapshot.meta.dirty).toBe(false);
    beats(0.5);
    expect(statusOf(server)).toBe("playing");
    await flush();
    const told_ = snapshots(told).slice(before);
    // (One for each change of what the box is doing: counting in, recording and playing. A pass in which nothing was played changes nothing that is saved.)
    expect(told_.map((s) => s.graph.nodes.find((n) => n.id === PLAIN && n.kind === "pattern")?.kind === "pattern" && (s.graph.nodes.find((n) => n.id === PLAIN) as Extract<Node, { kind: "pattern" }>).status)).toEqual(["countingIn", "recording", "playing"]);
    expect(told_.map((s) => s.meta.dirty)).toEqual([false, false, false]);
    // (Each has a revision of its own, after the last that the clients were told.)
    const revs = told_.map((s) => s.rev);
    expect(revs.every((rev, i) => i === 0 || rev > (revs[i - 1] ?? 0))).toBe(true);
    expect(server.snapshot.rev).toBe(revs.at(-1));
  });

  test("takes the keys that are played in the pass at the beat that each is at, and the notes are the box's, which is a step of the history that undo takes back and redo puts again", async () => {
    const { server, beats, transport, key, release } = await engine();
    transport(PLAIN, "record");
    beats(4);
    beats(1);
    key(60, 90);
    beats(1);
    release(60);
    beats(1);
    key(64);
    // (The pass ends four beats after it began: a key that is down then ends there.)
    beats(0.99);
    expect(statusOf(server)).toBe("recording");
    expect(boxOf(server).notes).toEqual([]);
    beats(0.01);
    expect(statusOf(server)).toBe("playing");
    expect(boxOf(server).notes).toEqual([note(1, 1, 60, 90), note(3, 1, 64, 100)]);
    expect(server.snapshot.meta.dirty).toBe(true);
    expect(server.history).toEqual({ undo: "record into Pattern", redo: null });
    server.apply({ type: "undo" });
    expect(boxOf(server).notes).toEqual([]);
    expect(server.history).toEqual({ undo: null, redo: "record into Pattern" });
    server.apply({ type: "redo" });
    expect(boxOf(server).notes).toEqual([note(1, 1, 60, 90), note(3, 1, 64, 100)]);
    expect(server.history).toEqual({ undo: "record into Pattern", redo: null });
  });

  test("makes no step of a pass that takes what the box has already, a pass that takes nothing from a box that has none", async () => {
    const { server, beats, transport } = await engine();
    transport(PLAIN, "record");
    beats(8);
    expect(statusOf(server)).toBe("playing");
    expect(server.history).toEqual({ undo: null, redo: null });
    expect(server.snapshot.meta.dirty).toBe(false);
  });

  test("makes a step that tells the clients what Undo and Redo would do, as the server does at its next look", async () => {
    const { server, beats, transport, told, flush, key, release } = await engine();
    transport(PLAIN, "record");
    beats(4);
    key(60);
    beats(1);
    release(60);
    beats(3);
    await flush();
    const histories = told.flatMap((message) => (message.type === "history" ? [message] : []));
    expect(histories.at(-1)).toMatchObject({ type: "history", undo: "record into Pattern", redo: null });
    expect(server.history.undo).toBe("record into Pattern");
  });

  test("goes on, a step after another, when it records again, and a recording that is made after an undo ends what could be redone", async () => {
    const { server, beats, transport, key, release } = await engine();
    const take = (pitch: number): void => {
      transport(PLAIN, "record");
      beats(4);
      beats(1);
      key(pitch);
      beats(1);
      release(pitch);
      beats(2);
    };
    take(60);
    take(62);
    expect(boxOf(server).notes).toEqual([note(1, 1, 62, 100)]);
    expect(server.history.undo).toBe("record into Pattern");
    server.apply({ type: "undo" });
    expect(boxOf(server).notes).toEqual([note(1, 1, 60, 100)]);
    expect(server.history.redo).toBe("record into Pattern");
    take(65);
    expect(boxOf(server).notes).toEqual([note(1, 1, 65, 100)]);
    expect(server.history.redo).toBeNull();
    server.apply({ type: "undo" });
    server.apply({ type: "undo" });
    expect(boxOf(server).notes).toEqual([]);
  });

  test("takes a key at the beat that it comes at, though nothing has looked at the box since the time went by: a key that goes down, and one that comes up", async () => {
    const { server, beats, time, transport, key, release } = await engine();
    transport(PLAIN, "record");
    beats(4);
    // (Two beats go by, which no timer has looked at: the key is two beats into the pass, and it is down for two beats more.)
    time.seconds += 2 * BEAT;
    key(60);
    time.seconds += 2 * BEAT;
    release(60);
    beats(1);
    expect(boxOf(server).notes).toEqual([note(2, 2, 60, 100)]);
  });

  test("takes a key that goes down with no velocity as one that comes up, and keys that are played before the pass or after it are not in it", async () => {
    const { server, beats, transport, key, release } = await engine();
    transport(PLAIN, "record");
    key(59);
    beats(4);
    beats(1);
    key(60);
    beats(1);
    key(60, 0);
    beats(2);
    key(61);
    release(61);
    expect(statusOf(server)).toBe("playing");
    expect(boxOf(server).notes).toEqual([note(1, 1, 60, 100)]);
  });

  test("makes a step of each pass that changes the notes, though they end one after another in a moment: they are not one step", async () => {
    const { server, beats, transport, key, release } = await engine();
    server.apply({ type: "setPatternBoxField", node: PLAIN, field: { type: "lengthBeats", beats: 0.25 } });
    server.apply({ type: "setPatternBoxField", node: PLAIN, field: { type: "overdub", on: true } });
    // (Two steps that the settings made, which this is not about: their history is set aside.)
    server.apply({ type: "undo" });
    server.apply({ type: "undo" });
    server.apply({ type: "redo" });
    server.apply({ type: "redo" });
    transport(PLAIN, "record");
    beats(4);
    beats(0.0625);
    key(60);
    beats(0.0625);
    release(60);
    beats(0.125);
    // The first pass has ended, and the second begins: a key of its own.
    beats(0.0625);
    key(62);
    beats(0.0625);
    release(62);
    beats(0.125);
    transport(PLAIN, "stop");
    expect(boxOf(server).notes).toEqual([note(0.0625, 0.0625, 60, 100), note(0.0625, 0.0625, 62, 100)]);
    expect(server.history.undo).toBe("record into Pattern");
    server.apply({ type: "undo" });
    expect(boxOf(server).notes).toEqual([note(0.0625, 0.0625, 60, 100)]);
    server.apply({ type: "undo" });
    expect(boxOf(server).notes).toEqual([]);
  });

  test("overdubs when the box says so: the notes it had are kept and the pass is added, and it goes on with another until it is told to finish", async () => {
    const { server, beats, transport, key, release } = await engine();
    transport(BASS, "record");
    beats(4);
    beats(1);
    key(60);
    beats(1);
    release(60);
    beats(6);
    expect(statusOf(server, BASS)).toBe("recording");
    expect(boxOf(server, BASS).notes).toEqual([note(0, 1, 36, 100), note(1, 1, 60, 100), note(2, 0.5, 43, 90)]);
    transport(BASS, "finishRecording");
    expect(statusOf(server, BASS)).toBe("recording");
    beats(8);
    expect(statusOf(server, BASS)).toBe("playing");
    expect(boxOf(server, BASS).notes.length).toBe(3);
    // The first pass is a step, and the second, which took nothing, is none.
    expect(server.history.undo).toBe("record into Bass");
    server.apply({ type: "undo" });
    expect(boxOf(server, BASS).notes).toEqual([note(0, 1, 36, 100), note(2, 0.5, 43, 90)]);
  });
});

describe("a pattern box that does not record", () => {
  test("plays and stops when it is told to, and is stopped when a pass that it was making is given up, with the notes that it had", async () => {
    const { server, beats, transport, key, release } = await engine();
    transport(BASS, "play");
    expect(statusOf(server, BASS)).toBe("playing");
    beats(100);
    expect(statusOf(server, BASS)).toBe("playing");
    transport(BASS, "record");
    beats(5);
    key(60);
    release(60);
    transport(BASS, "stop");
    expect(statusOf(server, BASS)).toBe("stopped");
    beats(20);
    expect(boxOf(server, BASS).notes).toEqual([note(0, 1, 36, 100), note(2, 0.5, 43, 90)]);
    expect(server.history).toEqual({ undo: null, redo: null });
    expect(server.snapshot.meta.dirty).toBe(false);
  });

  test("plays once and stops, if it does not loop, and goes by the tempo that the session has when it is asked", async () => {
    const { server, beats, transport, time } = await engine();
    server.apply({ type: "setPatternBoxField", node: PLAIN, field: { type: "looping", on: false } });
    transport(PLAIN, "play");
    beats(3.9);
    expect(statusOf(server)).toBe("playing");
    // At 60 a minute a beat is a second: the box has 0.1 beat of its four left, which is a tenth of a second at the tempo it has then.
    server.apply({ type: "setBpm", bpm: 60 });
    time.seconds += 0.05;
    server.settle();
    expect(statusOf(server)).toBe("playing");
    time.seconds += 0.06;
    server.settle();
    expect(statusOf(server)).toBe("stopped");
  });

  test("does not record the keys when it has a cable of notes from a device and none from the keys, and does when the cable is from the keys", async () => {
    const snapshot = sessionOf("boxes");
    // (Bass has a cable from the keys, 2; here it has one from the device, 3.)
    const fromDevice: Snapshot = { ...snapshot, graph: { ...snapshot.graph, edges: snapshot.graph.edges.map((e) => (e.kind === "events" && e.to.node === BASS ? { ...e, from: { ...e.from, node: 3 } } : e)) } };
    for (const [session, notes] of [
      [fromDevice, [note(0, 1, 36, 100), note(2, 0.5, 43, 90)]],
      [snapshot, [note(0, 1, 36, 100), note(1, 1, 60, 100), note(2, 0.5, 43, 90)]],
    ] as const) {
      const { server, beats, transport, key, release } = await engine(session);
      server.apply({ type: "setPatternBoxField", node: BASS, field: { type: "overdub", on: true } });
      transport(BASS, "record");
      beats(4);
      beats(1);
      key(60);
      beats(1);
      release(60);
      beats(6);
      expect(boxOf(server, BASS).notes).toEqual(notes);
    }
  });
});

describe("a pattern box that goes while it is doing something", () => {
  test("is let go of with the pass that it was making, and is back stopped when undo puts it back", async () => {
    const { server, beats, transport, key, release } = await engine();
    transport(PLAIN, "record");
    beats(5);
    key(60);
    server.apply({ type: "removeNode", node: PLAIN });
    expect(server.snapshot.graph.nodes.some((n) => n.id === PLAIN)).toBe(false);
    release(60);
    beats(20);
    expect(server.history.undo).toBe("remove Pattern");
    server.apply({ type: "undo" });
    expect(statusOf(server)).toBe("stopped");
    expect(boxOf(server).notes).toEqual([]);
    beats(20);
    expect(statusOf(server)).toBe("stopped");
    expect(server.history.undo).toBeNull();
  });

  test("is let go of when what made it goes with an undo, and is back stopped when redo puts it back, whatever it was doing", async () => {
    const { server, beats, transport } = await engine();
    server.apply({ type: "addPattern" });
    const added = server.snapshot.graph.nodes.filter((n) => n.kind === "pattern").at(-1)?.id;
    if (added === undefined) throw new Error("a box was added");
    transport(added, "record");
    beats(1);
    expect(statusOf(server, added)).toBe("countingIn");
    server.apply({ type: "undo" });
    expect(server.snapshot.graph.nodes.some((n) => n.id === added)).toBe(false);
    beats(20);
    server.apply({ type: "redo" });
    expect(statusOf(server, added)).toBe("stopped");
    beats(20);
    expect(statusOf(server, added)).toBe("stopped");
    expect(boxOf(server, added).notes).toEqual([]);
  });

  test("is stopped by a change of the clock that it goes by, as the engine stops a player that is given another", async () => {
    const { server, transport } = await engine();
    transport(PLAIN, "play");
    expect(statusOf(server)).toBe("playing");
    // (The same clock again is no change.)
    server.apply({ type: "setPatternBoxField", node: PLAIN, field: { type: "sync", sync: "own" } });
    expect(statusOf(server)).toBe("playing");
    server.apply({ type: "setPatternBoxField", node: PLAIN, field: { type: "sync", sync: "host" } });
    expect(statusOf(server)).toBe("stopped");
  });

  test("goes by the host's clock only in words: it waits for ever to record, and says so", async () => {
    const { server, beats, transport } = await engine();
    server.apply({ type: "setPatternBoxField", node: PLAIN, field: { type: "sync", sync: "host" } });
    transport(PLAIN, "record");
    expect(statusOf(server)).toBe("waiting");
    beats(100);
    expect(statusOf(server)).toBe("waiting");
    transport(PLAIN, "finishRecording");
    expect(statusOf(server)).toBe("stopped");
    transport(PLAIN, "play");
    beats(100);
    expect(statusOf(server)).toBe("playing");
  });
});

describe("the mock server's timer for the boxes", () => {
  test("is there while a box counts in, takes a pass or plays once, does the work by itself, and goes when nothing changes by itself any more", async () => {
    vi.useFakeTimers();
    const server = new MockServer({ snapshot: sessionOf("boxes"), clock: () => Date.now() / 1000 });
    const { told } = listener(server);
    await vi.advanceTimersByTimeAsync(0);
    expect(vi.getTimerCount()).toBe(0);
    server.apply({ type: "patternBoxTransport", node: PLAIN, action: "record" });
    expect(vi.getTimerCount()).toBe(1);
    // (One timer for all the boxes there are, and the lane recordings'.)
    server.apply({ type: "patternBoxTransport", node: BASS, action: "record" });
    expect(vi.getTimerCount()).toBe(1);
    vi.advanceTimersByTime(4 * 1000 * BEAT - 100);
    expect(statusOf(server)).toBe("countingIn");
    vi.advanceTimersByTime(200);
    expect(statusOf(server)).toBe("recording");
    expect(vi.getTimerCount()).toBe(1);
    // The plain box's pass ends and it plays on its own; Bass overdubs, so it goes on recording, and the timer with it.
    vi.advanceTimersByTime(4 * 1000 * BEAT);
    expect(statusOf(server)).toBe("playing");
    expect(statusOf(server, BASS)).toBe("recording");
    expect(vi.getTimerCount()).toBe(1);
    server.apply({ type: "patternBoxTransport", node: BASS, action: "stop" });
    expect(vi.getTimerCount()).toBe(0);
    await vi.advanceTimersByTimeAsync(0);
    expect(snapshots(told).length).toBeGreaterThan(0);
  });

  test("is not there for a box that plays for ever or waits for a host", () => {
    vi.useFakeTimers();
    const server = new MockServer({ snapshot: sessionOf("boxes"), clock: () => Date.now() / 1000 });
    server.apply({ type: "patternBoxTransport", node: PLAIN, action: "play" });
    expect(vi.getTimerCount()).toBe(0);
    server.apply({ type: "patternBoxTransport", node: BASS, action: "stop" });
    server.apply({ type: "setPatternBoxField", node: BASS, field: { type: "sync", sync: "host" } });
    server.apply({ type: "patternBoxTransport", node: BASS, action: "record" });
    expect(statusOf(server, BASS)).toBe("waiting");
    expect(vi.getTimerCount()).toBe(0);
  });
});
