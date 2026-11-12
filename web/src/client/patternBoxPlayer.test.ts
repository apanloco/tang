import { describe, expect, test } from "vitest";

import { PATTERN_BOX_NOTE_LIMIT, PATTERN_COUNT_IN_BEATS } from "../protocol";
import type { Graph, PatternNote } from "../protocol";
import { newBox } from "./patternBoxes";
import { BoxRun, boxViewOf } from "./patternBoxPlayer";
import type { BoxView } from "./patternBoxPlayer";

// What the mock engine's player of a pattern box does, as `BoxPlayer` (`src/patternbox.rs`) does it, in the part that the page shows: what the box is doing at each beat, and the notes that a pass makes of the keys that are played. The beats here go by
// a tempo of 120 a minute: a beat is half a second.
const BPM = 120;
const BEAT = 60 / BPM;
const note = (start: number, length: number, pitch: number, velocity: number): PatternNote => ({ start, length, note: pitch, velocity });

/** A box that is stopped, with the loop and the settings that a test says, and a way to go on in beats. */
function rig(over: Partial<BoxView> = {}) {
  const view: BoxView = { length: 4, looping: true, overdub: false, clock: "own", notes: [], ...over };
  const run = new BoxRun(0);
  const passes: PatternNote[][] = [];
  let seconds = 0;
  return {
    run,
    view,
    passes,
    /** Go on by `beats` beats, as the server does when it looks (the notes that a pass makes are the box's as the pass ends). */
    beats(count: number, bpm = BPM): void {
      seconds += count * (60 / bpm);
      run.advance(seconds, bpm, () => view, (notes) => {
        passes.push(notes);
        view.notes = notes;
      });
    },
    /** Do what a key does, at the beat the box is at. */
    key(on: boolean, pitch: number, velocity = 100): void {
      run.hear(on, pitch, velocity);
    },
    now: () => seconds,
  };
}

describe("a box that records on its own clock", () => {
  test("counts in four beats and then takes a pass of its length, and plays what it took from the start of the loop", () => {
    expect(PATTERN_COUNT_IN_BEATS).toBe(4);
    const box = rig();
    expect(box.run.status).toBe("stopped");
    box.run.record(box.view, 0);
    expect(box.run.status).toBe("countingIn");
    box.beats(3.99);
    expect(box.run.status).toBe("countingIn");
    box.beats(0.01);
    expect(box.run.status).toBe("recording");
    box.beats(3.99);
    expect(box.run.status).toBe("recording");
    expect(box.passes).toEqual([]);
    box.beats(0.01);
    expect(box.run.status).toBe("playing");
    expect(box.passes).toEqual([[]]);
    // (It plays round for as long as it is let: a loop that goes on needs nothing more.)
    box.beats(100);
    expect(box.run.status).toBe("playing");
  });

  test("goes at the tempo that the session has, a tempo that changes in the middle of a pass moving the beats that are left", () => {
    const box = rig();
    box.run.record(box.view, 0);
    // Four beats at 120 are two seconds; at 60 a beat is a second.
    box.beats(4);
    box.beats(1, 60);
    expect(box.run.status).toBe("recording");
    box.key(true, 60);
    box.beats(1, 240);
    box.key(false, 60);
    // (A beat at 240 is a quarter of a second: the key was down for a beat.)
    box.beats(2, 240);
    expect(box.run.status).toBe("playing");
    expect(box.passes).toEqual([[note(1, 1, 60, 100)]]);
  });

  test("takes the notes that are played in the pass, at the beat that each is at, for as long as each is down, with the velocity it went down at", () => {
    const box = rig();
    box.run.record(box.view, 0);
    box.beats(4);
    box.beats(0.5);
    box.key(true, 60, 90);
    box.beats(1);
    box.key(true, 64, 40);
    box.beats(0.25);
    box.key(false, 60);
    box.beats(0.75);
    box.key(false, 64);
    box.beats(1.5);
    expect(box.passes).toEqual([[note(0.5, 1.25, 60, 90), note(1.5, 1, 64, 40)]]);
    expect(box.run.status).toBe("playing");
  });

  test("does not take what is played in the count-in, and takes the keys that come up that went down before the pass as nothing", () => {
    const box = rig();
    box.run.record(box.view, 0);
    box.beats(2);
    box.key(true, 60);
    box.beats(2.5);
    box.key(false, 60);
    box.beats(3.5);
    expect(box.passes).toEqual([[]]);
  });

  test("ends a note that is still down at the end of the pass there, gives a key that comes up as it went down the shortest note, and lets a key that goes down again end the note it held", () => {
    const box = rig();
    box.run.record(box.view, 0);
    box.beats(4);
    box.beats(1);
    box.key(true, 62);
    box.key(false, 62);
    box.beats(1);
    box.key(true, 64, 80);
    box.beats(0.5);
    box.key(true, 64, 70);
    box.beats(0.5);
    box.key(true, 67, 60);
    box.beats(1);
    expect(box.passes).toEqual([[note(1, 1 / 64, 62, 100), note(2, 0.5, 64, 80), note(2.5, 1.5, 64, 70), note(3, 1, 67, 60)]]);
  });

  test("takes what is not a pitch as nothing (a key that goes down with no velocity is one that comes up: the server says so)", () => {
    const box = rig();
    box.run.record(box.view, 0);
    box.beats(4);
    box.key(true, 60, 100);
    box.beats(1);
    box.key(false, 60, 0);
    box.key(true, 128);
    box.key(true, -1);
    box.key(true, 60.5);
    box.beats(3);
    expect(box.passes).toEqual([[note(0, 1, 60, 100)]]);
  });

  test("replaces the notes that it had, or keeps them and adds the notes of the pass if it overdubs, and goes on with another pass until it is told to finish", () => {
    const had = [note(0, 1, 36, 100)];
    const over = rig({ notes: had, overdub: false });
    over.run.record(over.view, 0);
    over.beats(4);
    over.key(true, 60);
    over.beats(1);
    over.key(false, 60);
    over.beats(3);
    expect(over.passes).toEqual([[note(0, 1, 60, 100)]]);
    expect(over.run.status).toBe("playing");

    const dub = rig({ notes: had, overdub: true });
    dub.run.record(dub.view, 0);
    dub.beats(4);
    dub.key(true, 60);
    dub.beats(1);
    dub.key(false, 60);
    dub.beats(3);
    expect(dub.passes).toEqual([[note(0, 1, 36, 100), note(0, 1, 60, 100)]]);
    expect(dub.run.status).toBe("recording");
    // The second pass adds to what the first made, and the third is the last: the box was told to finish in it.
    dub.beats(2);
    dub.key(true, 62);
    dub.beats(1);
    dub.key(false, 62);
    dub.run.finish();
    expect(dub.run.status).toBe("recording");
    dub.beats(1);
    expect(dub.passes.length).toBe(2);
    expect(dub.passes[1]).toEqual([note(0, 1, 36, 100), note(0, 1, 60, 100), note(2, 1, 62, 100)]);
    expect(dub.run.status).toBe("playing");
  });

  test("goes through many passes in one look when much time has gone by, each making the notes that the next adds to", () => {
    const box = rig({ overdub: true, length: 1 });
    box.run.record(box.view, 0);
    box.beats(4);
    box.beats(10);
    expect(box.passes.length).toBe(10);
    expect(box.run.status).toBe("recording");
  });

  test("gives up a count-in that it is told to finish, and a box that stops loses the pass that it was making", () => {
    const counting = rig();
    counting.run.record(counting.view, 0);
    counting.beats(1);
    counting.run.finish();
    expect(counting.run.status).toBe("stopped");
    expect(counting.run.stopped).toBe(true);

    const passing = rig({ notes: [note(0, 1, 36, 100)] });
    passing.run.record(passing.view, 0);
    passing.beats(4);
    passing.beats(1);
    passing.key(true, 60);
    passing.run.stop();
    passing.beats(10);
    expect(passing.passes).toEqual([]);
    expect(passing.view.notes).toEqual([note(0, 1, 36, 100)]);
    expect(passing.run.status).toBe("stopped");
  });

  test("begins again when it is told to record while it records, and a key that was down in the pass that was lost is not in the new one", () => {
    const box = rig();
    box.run.record(box.view, 0);
    box.beats(5);
    box.key(true, 60);
    box.run.record(box.view, box.now());
    expect(box.run.status).toBe("countingIn");
    box.beats(4);
    box.beats(1);
    box.key(false, 60);
    box.beats(3);
    expect(box.passes).toEqual([[]]);
  });

  test("plays once and stops, with the pass that it made its play, when it does not loop", () => {
    const box = rig({ looping: false });
    box.run.record(box.view, 0);
    box.beats(4);
    box.key(true, 60);
    box.beats(2);
    box.beats(2);
    expect(box.passes).toEqual([[note(0, 4, 60, 100)]]);
    expect(box.run.status).toBe("stopped");
    expect(box.run.stopped).toBe(true);
  });

  test("keeps at most the notes there is room for, and what is not a note is no note", () => {
    const box = rig({ overdub: true, length: 100 });
    box.view.notes = Array.from({ length: PATTERN_BOX_NOTE_LIMIT - 1 }, (_, i) => note(i, 1, 40, 100));
    box.run.record(box.view, 0);
    box.beats(4);
    for (const pitch of [60, 61, 62]) {
      box.key(true, pitch);
      box.key(false, pitch);
    }
    box.beats(100);
    expect(box.passes[0]?.length).toBe(PATTERN_BOX_NOTE_LIMIT);
  });
});

describe("what a box has of what it was told before", () => {
  test("goes on with the passes of an overdub when it is told to record again, though the last time it was told to finish: the pass that was the last of that is not the last of this", () => {
    const box = rig({ overdub: true });
    box.run.record(box.view, 0);
    box.beats(4);
    box.beats(1);
    box.run.finish();
    box.beats(3);
    expect(box.run.status).toBe("playing");
    box.run.record(box.view, box.now());
    box.beats(4);
    box.beats(4);
    expect(box.passes.length).toBe(2);
    expect(box.run.status).toBe("recording");
  });

  test("plays the whole of its loop when it plays again after it was stopped half way, and after it played once and stopped by itself, on a loop that does not go round", () => {
    const stopped = rig({ looping: false, length: 4 });
    stopped.run.play(stopped.view, 0);
    stopped.beats(2.5);
    stopped.run.stop();
    stopped.run.play(stopped.view, stopped.now());
    stopped.beats(1.6);
    expect(stopped.run.status).toBe("playing");
    stopped.beats(2.4);
    expect(stopped.run.status).toBe("stopped");

    const once = rig({ looping: false, length: 2 });
    once.run.play(once.view, 0);
    once.beats(2);
    expect(once.run.status).toBe("stopped");
    once.run.play(once.view, once.now());
    once.beats(1.5);
    expect(once.run.status).toBe("playing");
    once.beats(0.5);
    expect(once.run.status).toBe("stopped");
  });
});

describe("the notes of a pass, at the finest", () => {
  test("have no length below the shortest, for a key that goes down again at once, and for a key that is down when the pass ends a moment after it went down", () => {
    const again = rig();
    again.run.record(again.view, 0);
    again.beats(4);
    again.beats(1);
    again.key(true, 60, 100);
    again.key(true, 60, 90);
    again.beats(3);
    expect(again.passes).toEqual([[note(1, 1 / 64, 60, 100), note(1, 3, 60, 90)]]);

    const late = rig();
    late.run.record(late.view, 0);
    late.beats(4);
    late.beats(3.999);
    late.key(true, 62, 100);
    late.beats(0.001);
    expect(late.passes).toEqual([[note(3.999, 1 / 64, 62, 100)].map((n) => ({ ...n, start: Math.fround(n.start) }))]);
  });

  test("are held to the 32 bits that the engine has for a note, as they are when the time of a key is not a number of the engine's", () => {
    const box = rig();
    box.run.record(box.view, 0);
    box.beats(4);
    box.beats(1 / 3);
    box.key(true, 60);
    box.beats(1 / 7);
    box.key(false, 60);
    box.beats(4);
    const [taken] = box.passes[0] ?? [];
    expect(taken).toBeDefined();
    expect(taken?.start).toBe(Math.fround(taken?.start ?? Number.NaN));
    expect(taken?.length).toBe(Math.fround(taken?.length ?? Number.NaN));
    expect(taken?.start).not.toBe(1 / 3);
    expect(taken?.start).toBeCloseTo(1 / 3, 6);
    expect(taken?.length).toBeCloseTo(1 / 7, 6);
  });
});

describe("a box that plays", () => {
  test("goes round from the start for ever if it loops, and plays once and stops if it does not", () => {
    const loops = rig();
    loops.run.play(loops.view, 0);
    expect(loops.run.status).toBe("playing");
    loops.beats(1000);
    expect(loops.run.status).toBe("playing");

    const once = rig({ looping: false, length: 2 });
    once.run.play(once.view, 0);
    once.beats(1.99);
    expect(once.run.status).toBe("playing");
    once.beats(0.01);
    expect(once.run.status).toBe("stopped");
    expect(once.run.stopped).toBe(true);
  });

  test("does nothing when it is told to play and it plays or records already, and stops when it is told to", () => {
    const box = rig();
    box.run.record(box.view, 0);
    box.beats(1);
    box.run.play(box.view, box.now());
    expect(box.run.status).toBe("countingIn");
    box.run.stop();
    expect(box.run.status).toBe("stopped");
    box.run.play(box.view, box.now());
    expect(box.run.status).toBe("playing");
    box.run.play(box.view, box.now());
    box.run.stop();
    expect(box.run.status).toBe("stopped");
  });

  test("changes by itself with time only when it counts in, makes a pass or plays once on its own clock", () => {
    const loops = rig();
    expect(loops.run.moving(loops.view)).toBe(false);
    loops.run.play(loops.view, 0);
    expect(loops.run.moving(loops.view)).toBe(false);
    loops.run.record(loops.view, 0);
    expect(loops.run.moving(loops.view)).toBe(true);
    const once = rig({ looping: false });
    once.run.play(once.view, 0);
    expect(once.run.moving(once.view)).toBe(true);
    once.run.stop();
    expect(once.run.moving(once.view)).toBe(false);
  });
});

describe("a box that goes by the host's clock", () => {
  test("waits for ever when it is told to record, since the mock has no host, and does not move", () => {
    const box = rig({ clock: "host" });
    box.run.record(box.view, 0);
    expect(box.run.status).toBe("waiting");
    expect(box.run.goesBy).toBe("host");
    expect(box.run.moving(box.view)).toBe(false);
    box.beats(1000);
    expect(box.run.status).toBe("waiting");
    expect(box.passes).toEqual([]);
    // Told to finish while it waits, it gives up.
    box.run.finish();
    expect(box.run.status).toBe("stopped");
  });

  test("plays, and says so, though nothing is heard of it, and does not stop at the end of its loop", () => {
    const box = rig({ clock: "host", looping: false, length: 1 });
    box.run.play(box.view, 0);
    box.beats(10);
    expect(box.run.status).toBe("playing");
    expect(box.run.goesBy).toBe("host");
  });
});

describe("what a box is, read from the graph", () => {
  test("is the length, the settings and the notes of the node, and nothing for a node that is no box", () => {
    const box = { ...newBox(4, "Bass"), lengthBeats: 8, looping: false, overdub: true, sync: "host" as const, notes: [note(0, 1, 36, 100)] };
    const graph: Graph = { nodes: [box, { id: 9, parent: null, name: "Master", kind: "master", volume: 1, limiter: true, ceiling: -1 } as unknown as Graph["nodes"][number]], edges: [] };
    expect(boxViewOf(graph, 4)).toEqual({ length: 8, looping: false, overdub: true, clock: "host", notes: [note(0, 1, 36, 100)] });
    expect(boxViewOf(graph, 9)).toBeUndefined();
    expect(boxViewOf(graph, 99)).toBeUndefined();
  });
});

describe("the beats of a box", () => {
  test("go by the seconds that go by at the tempo that is given, and by none for a tempo that is no number above 0 or time that goes back", () => {
    const box = rig();
    box.run.record(box.view, 0);
    let seconds = 0;
    const go = (by: number, bpm: number): void => {
      seconds += by;
      box.run.advance(seconds, bpm, () => box.view, () => undefined);
    };
    go(1, 0);
    go(1, Number.NaN);
    go(1, Number.POSITIVE_INFINITY);
    go(1, -120);
    expect(box.run.status).toBe("countingIn");
    // (Nothing of that was a beat: four are still to go, which are two seconds at this tempo.)
    go(2 - 0.02, BPM);
    expect(box.run.status).toBe("countingIn");
    go(0.02, BPM);
    expect(box.run.status).toBe("recording");
    // (A look at a time that is not after the last is no move.)
    box.run.advance(0, BPM, () => box.view, () => undefined);
    expect(box.run.status).toBe("recording");
    expect(BEAT).toBe(0.5);
  });
});
