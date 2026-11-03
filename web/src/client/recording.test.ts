import { afterEach, describe, expect, test, vi } from "vitest";

import { PATTERN_COUNT_IN_BEATS } from "../protocol";
import type { Intent, NodeId, PatternInfo, ServerMessage, Snapshot } from "../protocol";
import { nodeNamed, patternsFixture, tracerSnapshot } from "./fixtures";
import { MockConnection, MockServer } from "./mock";

afterEach(() => {
  vi.useRealTimers();
});

// What the mock engine does with a recording, which the controller leaves to the audio thread: it counts in, takes the length of the pattern at the tempo there is, counts the
// notes that are played, and when it is done the lane has the pattern it made, which every client is given as a session (there is no event for it: it comes from the engine's
// report, which the controller folds in when it polls). What the intents say of a recording that starts and ends is the controller's, and is in `patterns.test.ts`.

const oscillator = nodeNamed(tracerSnapshot, "Oscillator").id;
/** The tracer's tempo is 120 a minute: a beat is half a second. */
const BEAT = 0.5;
/** How long a recording of `beats` beats takes, counted in. */
const taking = (beats: number): number => (PATTERN_COUNT_IN_BEATS + beats) * BEAT;

/** A connection to the server and what it is told (the client of a page, with no page): it has subscribed if it is to hear of changes. */
function listener(server: MockServer, subscribed: boolean) {
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
  if (subscribed) server.receive(connection, JSON.stringify({ type: "subscribe" }));
  return { connection, told };
}

/** A mock engine whose clock is the test's own (`time.seconds`), with a connection that has subscribed and what it has been told. */
async function engine(snapshot: Snapshot = tracerSnapshot) {
  const time = { seconds: 1000 };
  const server = new MockServer({ snapshot, clock: () => time.seconds });
  const { connection, told } = listener(server, true);
  const play = (note: number): void => {
    server.receive(connection, JSON.stringify({ type: "noteOn", note, velocity: 100 }));
  };
  const flush = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));
  // (What a connection is told comes after a moment: the hello, and the session it asked for, are said before anything that is done now.)
  await flush();
  return { server, time, told, play, flush };
}

const recording = (node: NodeId, on: boolean): Intent => ({ type: "setRecording", node, recording: on });
const patternOf = (server: MockServer, node: NodeId = oscillator): PatternInfo | null | undefined => {
  const found = server.snapshot.graph.nodes.find((n) => n.id === node);
  return found?.kind === "instrument" ? found.pattern : undefined;
};
const empty = (over: Partial<PatternInfo> = {}): PatternInfo => ({ recording: false, enabled: false, looping: true, inKey: false, lengthBeats: 4, bpm: 120, notes: 0, ...over });

describe("a recording on the mock engine", () => {
  test("is counted in and then takes the length of the pattern at the tempo of the session, and is done by itself: the lane has the pattern it made", async () => {
    const { server, time } = await engine();
    server.apply(recording(oscillator, true));
    expect(patternOf(server)).toEqual(empty({ recording: true }));
    // (Fractions of a second that a 64-bit number holds exactly: the end is a sum, and a second that is a hair short is not the end.)
    time.seconds += taking(4) - 0.125;
    server.settle();
    expect(patternOf(server)?.recording).toBe(true);
    time.seconds += 0.125;
    server.settle();
    expect(patternOf(server)).toEqual(empty());
  });

  test("counts the notes that are played from its start to its end, those of the count-in too, and makes a pattern that is heard of them", async () => {
    const { server, time, play } = await engine();
    server.apply(recording(oscillator, true));
    play(60);
    time.seconds += 1;
    play(62);
    play(64);
    // (Two seconds of count-in are over: this is the recording itself.)
    time.seconds += 1.5;
    play(65);
    time.seconds += 1.375;
    play(67);
    time.seconds += 0.125;
    // The recording has ended when the next note is played: that one is not in it.
    play(69);
    expect(patternOf(server)).toEqual(empty({ enabled: true, notes: 5 }));
    time.seconds += 10;
    server.settle();
    expect(patternOf(server)?.notes).toBe(5);
  });

  test("takes the notes that are in the keys of its lane and no others", async () => {
    const { server, time, play } = await engine();
    server.apply({ type: "setRange", node: oscillator, range: { low: 60, high: 72 } });
    server.apply(recording(oscillator, true));
    for (const note of [59, 60, 66, 72, 73]) play(note);
    // (Stopped after the count-in, which is the one that records what was played in it.)
    time.seconds += taking(0);
    server.apply(recording(oscillator, false));
    expect(patternOf(server)?.notes).toBe(3);
  });

  test("makes a pattern that is not heard of a recording that has no notes", async () => {
    const { server, time } = await engine();
    server.apply(recording(oscillator, true));
    time.seconds += taking(4);
    server.settle();
    expect(patternOf(server)).toEqual(empty({ enabled: false, notes: 0 }));
  });

  test("keeps the settings of the pattern the lane has, takes its length as it is when it is done, and says the tempo the session has by then", async () => {
    const { server, time, play } = await engine();
    server.apply({ type: "setPatternField", node: oscillator, field: "looping", value: 0 });
    server.apply({ type: "setPatternField", node: oscillator, field: "transpose", value: 1 });
    server.apply({ type: "setPatternField", node: oscillator, field: "length", value: 2 });
    server.apply(recording(oscillator, true));
    play(60);
    // The tempo is changed while it records: the recording is counted in what it began with, and the pattern is told at the tempo the session has when it is done.
    time.seconds += 1;
    server.apply({ type: "setBpm", bpm: 60 });
    time.seconds += taking(2) - 1;
    server.settle();
    expect(patternOf(server)).toEqual({ recording: false, enabled: true, looping: false, inKey: true, lengthBeats: 2, bpm: 60, notes: 1 });
  });

  test("moves its end when the length is set while it records", async () => {
    const { server, time } = await engine();
    server.apply(recording(oscillator, true));
    time.seconds += 1;
    server.apply({ type: "setPatternField", node: oscillator, field: "length", value: 8 });
    time.seconds += taking(4) - 1;
    server.settle();
    expect(patternOf(server)).toMatchObject({ recording: true, lengthBeats: 8 });
    time.seconds += taking(8) - taking(4);
    server.settle();
    expect(patternOf(server)).toMatchObject({ recording: false, lengthBeats: 8 });
  });

  test("is counted in at the tempo the session has when it begins", async () => {
    const { server, time } = await engine();
    server.apply({ type: "setBpm", bpm: 60 });
    server.apply(recording(oscillator, true));
    // (A beat is a second: eight of them, and not the four seconds that the tracer's 120 a minute takes.)
    time.seconds += taking(4);
    server.settle();
    expect(patternOf(server)?.recording).toBe(true);
    time.seconds += taking(4);
    server.settle();
    expect(patternOf(server)).toEqual(empty({ bpm: 60 }));
  });

  test("is stopped in the count-in, or after it: before it is over nothing has been recorded, and when it is over what has been played is the pattern", async () => {
    for (const [seconds, notes] of [
      [1.875, 1],
      [2, 3],
    ] as const) {
      const { server, time, play } = await engine();
      // The lane has a pattern of one note to begin with.
      server.apply(recording(oscillator, true));
      play(60);
      time.seconds += taking(4);
      server.settle();
      expect(patternOf(server)?.notes).toBe(1);
      server.apply(recording(oscillator, true));
      play(62);
      play(64);
      play(65);
      time.seconds += seconds;
      server.apply(recording(oscillator, false));
      expect(patternOf(server)?.notes, `stopped ${String(seconds)} seconds in`).toBe(notes);
    }
  });

  test("is counted in at the tempo it began with, whatever the tempo is changed to", async () => {
    const { server, time, play } = await engine();
    server.apply(recording(oscillator, true));
    play(60);
    time.seconds += 1;
    server.apply({ type: "setBpm", bpm: 60 });
    // Two seconds in: the count-in of 120 a minute is over, which the count-in of 60 would not be.
    time.seconds += 1;
    server.apply(recording(oscillator, false));
    expect(patternOf(server)).toEqual(empty({ enabled: true, notes: 1, bpm: 60 }));
  });

  test("begins again, from now and with no notes, when it is started after a pattern is cleared or after it is stopped", async () => {
    for (const end of [{ type: "clearPattern", node: oscillator }, recording(oscillator, false)] as const) {
      const { server, time, play } = await engine();
      server.apply(recording(oscillator, true));
      play(60);
      time.seconds += 1;
      server.apply(end);
      server.apply(recording(oscillator, true));
      // The one that was ended would be done by now: this one has a second more to go.
      time.seconds += taking(4) - 1;
      server.settle();
      expect(patternOf(server)?.recording, end.type).toBe(true);
      time.seconds += 1;
      server.settle();
      expect(patternOf(server), end.type).toEqual(empty());
    }
  });

  test("makes the session one that is not saved when it is done, even if it was saved while it went on", async () => {
    const { server, time } = await engine();
    server.apply(recording(oscillator, true));
    server.apply({ type: "save" });
    expect(server.snapshot.meta.dirty).toBe(false);
    time.seconds += taking(4);
    server.settle();
    expect(server.snapshot.meta.dirty).toBe(true);
  });

  test("tells the clients that have subscribed and not the others", async () => {
    const { server, time, told, flush } = await engine();
    const quiet = listener(server, false);
    await flush();
    server.apply(recording(oscillator, true));
    time.seconds += taking(4);
    server.settle();
    await flush();
    expect(told.at(-1)?.type).toBe("snapshot");
    expect(quiet.told.map((message) => message.type)).toEqual(["hello"]);
  });

  test("is stopped in the count-in with nothing recorded: the lane has the pattern it had", async () => {
    const { server, time, play } = await engine(patternsFixture.snapshot);
    const lane = patternsFixture.snapshot.graph.nodes.find((n) => n.kind === "instrument" && n.pattern !== null)?.id;
    if (lane === undefined) throw new Error("the fixture has a lane with a pattern");
    const before = patternOf(server, lane);
    server.apply(recording(lane, true));
    play(60);
    play(62);
    time.seconds += 1;
    server.apply(recording(lane, false));
    time.seconds += 10;
    server.settle();
    expect(patternOf(server, lane)).toEqual(before);
  });

  test("is stopped in the recording with what has been played: the pattern is made at once, and told as a session", async () => {
    const { server, time, told, play, flush } = await engine();
    server.apply(recording(oscillator, true));
    time.seconds += 2.2;
    play(60);
    play(64);
    time.seconds += 0.3;
    server.apply(recording(oscillator, false));
    expect(patternOf(server)).toEqual(empty({ enabled: true, notes: 2 }));
    await flush();
    const last = told.at(-1);
    expect(last?.type).toBe("snapshot");
    // (The stop was told as an event, and the pattern that came of it as the session at the next revision.)
    expect(told.flatMap((message) => (message.type === "event" ? [message.event.type] : []))).toEqual(["patternSet", "patternSet"]);
    expect(last?.type === "snapshot" ? last.snapshot.rev : -1).toBe(server.snapshot.rev);
    expect(server.snapshot.rev).toBe(3);
    // It is not made again when the time it would have taken is up.
    time.seconds += 10;
    server.settle();
    expect(server.snapshot.rev).toBe(3);
  });

  test("is told to a client that is subscribed as a session at the revision after, and no event, when it is done by itself", async () => {
    const { server, time, told, flush } = await engine();
    server.apply(recording(oscillator, true));
    await flush();
    const events = told.length;
    expect(server.snapshot.rev).toBe(1);
    time.seconds += taking(4);
    server.settle();
    await flush();
    const news = told.slice(events);
    expect(news.map((message) => message.type)).toEqual(["snapshot"]);
    const [snapshot] = news;
    expect(snapshot?.type === "snapshot" ? [snapshot.snapshot.rev, snapshot.snapshot.meta.dirty] : null).toEqual([2, true]);
    expect(snapshot?.type === "snapshot" ? snapshot.snapshot : null).toEqual(server.snapshot);
  });

  test("is let go of when the pattern is cleared: it is not made again, and the lane has none", async () => {
    const { server, time } = await engine();
    server.apply(recording(oscillator, true));
    time.seconds += 1;
    server.apply({ type: "clearPattern", node: oscillator });
    time.seconds += 20;
    server.settle();
    expect(patternOf(server)).toBeNull();
  });

  test("is not begun again by a start while it is going on: it ends when it would have", async () => {
    const { server, time, play } = await engine();
    server.apply(recording(oscillator, true));
    play(60);
    time.seconds += 1;
    server.apply(recording(oscillator, true));
    play(62);
    time.seconds += taking(4) - 1;
    server.settle();
    // (Its notes are those of the one recording, from the start.)
    expect(patternOf(server)).toEqual(empty({ enabled: true, notes: 2 }));
  });

  test("starts again, with no notes, after one that is done", async () => {
    const { server, time, play } = await engine();
    server.apply(recording(oscillator, true));
    play(60);
    // (It has run out, and nothing has looked: the intent that comes first finds it done.)
    time.seconds += taking(4);
    server.apply(recording(oscillator, true));
    expect(patternOf(server)).toMatchObject({ recording: true, notes: 1 });
    time.seconds += 1;
    play(62);
    play(64);
    time.seconds += taking(4) - 1;
    server.settle();
    expect(patternOf(server)).toEqual(empty({ enabled: true, notes: 2 }));
  });

  test("has each lane's own notes and its own end, when several record at once", async () => {
    const { server, time, play } = await engine();
    server.apply({ type: "addInstrument", plugin: "builtin:osc" });
    const first = oscillator;
    const second = server.snapshot.graph.nodes.filter((n) => n.kind === "instrument").map((n) => n.id).at(-1);
    if (second === undefined) throw new Error("a lane was added");
    server.apply({ type: "setRange", node: first, range: { low: 0, high: 59 } });
    server.apply({ type: "setRange", node: second, range: { low: 60, high: 127 } });
    server.apply(recording(first, true));
    time.seconds += 1;
    server.apply(recording(second, true));
    play(48);
    play(72);
    play(73);
    // The first began a second before the second: it is done a second before.
    time.seconds += taking(4) - 1;
    server.settle();
    expect([patternOf(server, first)?.recording, patternOf(server, second)?.recording]).toEqual([false, true]);
    // (The notes of the one that goes on are the engine's until it is done.)
    expect([patternOf(server, first)?.notes, patternOf(server, second)?.notes]).toEqual([1, 0]);
    time.seconds += 1;
    server.settle();
    expect([patternOf(server, first)?.recording, patternOf(server, second)?.recording]).toEqual([false, false]);
    expect([patternOf(server, first)?.notes, patternOf(server, second)?.notes]).toEqual([1, 2]);
  });

  test("is done by a timer, with nobody to ask, and the timer goes when nothing records: a lane that is taken out is let go of", async () => {
    vi.useFakeTimers();
    const server = new MockServer({ clock: () => Date.now() / 1000 });
    const { told } = listener(server, true);
    await vi.advanceTimersByTimeAsync(0);
    expect(vi.getTimerCount()).toBe(0);
    server.apply(recording(oscillator, true));
    expect(vi.getTimerCount()).toBe(1);
    // (One timer for all the recordings there are.)
    server.apply({ type: "addInstrument", plugin: "builtin:osc" });
    const other = server.snapshot.graph.nodes.filter((n) => n.kind === "instrument").map((n) => n.id).at(-1);
    if (other === undefined) throw new Error("a lane was added");
    server.apply(recording(other, true));
    expect(vi.getTimerCount()).toBe(1);
    server.apply({ type: "clearPattern", node: other });
    vi.advanceTimersByTime(taking(4) * 1000 - 100);
    expect(patternOf(server)?.recording).toBe(true);
    vi.advanceTimersByTime(200);
    expect(patternOf(server)?.recording).toBe(false);
    expect(vi.getTimerCount()).toBe(0);
    await vi.advanceTimersByTimeAsync(0);
    expect(told.at(-1)?.type).toBe("snapshot");

    // A recording of a lane that is taken out is not waited for.
    server.apply({ type: "addInstrument", plugin: "builtin:osc" });
    const added = server.snapshot.graph.nodes.filter((n) => n.kind === "instrument").map((n) => n.id).at(-1);
    if (added === undefined) throw new Error("a lane was added");
    server.apply(recording(added, true));
    expect(vi.getTimerCount()).toBe(1);
    server.apply({ type: "removeNode", node: added });
    vi.advanceTimersByTime(taking(4) * 1000 + 100);
    expect(vi.getTimerCount()).toBe(0);
  });
});
