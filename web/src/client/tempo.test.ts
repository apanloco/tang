import { describe, expect, test } from "vitest";

import { SYNC_DIVISIONS } from "../protocol";
import { syncFixture } from "./fixtures";
import { beatsOf, rateAt } from "./tempo";

// What a note length comes to: the page's rule for how fast an LFO or a step sequencer goes when it follows the tempo, which is the engine's (`rate_at_tempo`), pinned to it by the rates
// that the engine makes of every note length at five tempos (the fixture).
describe("the beats of a note length", () => {
  test("are those of the place in the list, from 1, and none for 0 and for a place that the list has not", () => {
    expect(SYNC_DIVISIONS.map((_, index) => beatsOf(index + 1))).toEqual(SYNC_DIVISIONS.map((division) => division.beats));
    expect([0, 17, 99, -1, 2.5, Number.NaN].map(beatsOf)).toEqual([0, 0, 0, 0, 0, 0]);
    expect([beatsOf(3), beatsOf(8), beatsOf(11), beatsOf(16)], "a bar is four beats").toEqual([4, 1, 0.5, 0.125]);
  });
});

describe("the rate that a modulator goes at", () => {
  test("is its own rate when it is free, whatever the tempo", () => {
    expect([rateAt(3.5, 0, 120), rateAt(3.5, 0, 20), rateAt(0.01, 0, 300)]).toEqual([3.5, 3.5, 0.01]);
  });

  test("is the tempo's when it follows it: the beats a minute, over sixty, over the beats of the note length, and not its own", () => {
    expect(rateAt(1, 14, 120), "a sixteenth note at 120 beats a minute").toBeCloseTo(8, 6);
    expect(rateAt(1, 8, 120)).toBeCloseTo(2, 6);
    expect(rateAt(1, 3, 120), "a bar").toBeCloseTo(0.5, 6);
    expect(rateAt(1, 12, 120), "a triplet of eighths: three in the time of two").toBeCloseTo(6, 6);
    expect(rateAt(1, 11, 60)).toBeCloseTo(2, 6);
  });

  test("is what the engine makes of every note length at each of the tempos of the fixture", () => {
    expect(syncFixture.rows.length).toBe(SYNC_DIVISIONS.length * 5);
    const wrong = syncFixture.rows.flatMap((row) => {
      const mine = rateAt(1, row.place, row.bpm);
      // (The engine's is a 32-bit number and this a 64-bit one: they agree to a millionth.)
      return Math.abs(mine - row.rate) <= row.rate * 1e-6 ? [] : [`${row.name} at ${String(row.bpm)}: ${String(mine)}, and the engine says ${String(row.rate)}`];
    });
    expect(wrong).toEqual([]);
    expect(new Set(syncFixture.rows.map((row) => row.bpm)).size, "five tempos, the slowest and the fastest there can be among them").toBe(5);
  });
});
