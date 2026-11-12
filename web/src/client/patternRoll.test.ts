import { describe, expect, test } from "vitest";

import type { PatternNote } from "../protocol";
import { lineEvery, ROLL_MIN_SPAN, ROLL_MIN_WIDTH, rollOf } from "./patternRoll";

// What a pattern box's roll draws: the geometry of its picture, as plain numbers (the picture is `app/patch/PatternRoll.tsx`).

const note = (start: number, length: number, pitch: number, velocity = 100): PatternNote => ({ start, length, note: pitch, velocity });

describe("the roll of a box that has no notes", () => {
  test("is the loop in beats and the fewest semitones, with a line at each beat inside the loop and none at its ends", () => {
    const roll = rollOf([], 4);
    expect(roll).toEqual({
      beats: 4,
      span: ROLL_MIN_SPAN,
      notes: [],
      lines: [
        { at: 1, bar: false },
        { at: 2, bar: false },
        { at: 3, bar: false },
      ],
    });
  });

  test("has no line in a loop that is no longer than a beat", () => {
    expect(rollOf([], 1).lines).toEqual([]);
    expect(rollOf([], 0.25).lines).toEqual([]);
  });
});

describe("the notes of a roll", () => {
  test("are marks from their start for as long as they are, at their pitch: the highest at the top, in the middle of the least height", () => {
    const roll = rollOf([note(1, 2, 60, 127)], 4);
    // (One semitone used in twelve: five and a half above it, and the mark has the gap that tells notes a semitone apart, half of it above and half below.)
    expect(roll.span).toBe(12);
    expect(roll.notes).toHaveLength(1);
    const [mark] = roll.notes;
    expect(mark).toBeDefined();
    expect(mark).toMatchObject({ x: 1, width: 2, opacity: 1 });
    expect(mark?.y).toBeCloseTo(5.5 + 0.075, 10);
    expect(mark?.height).toBeCloseTo(0.85, 10);
  });

  test("have the higher pitch above the lower, a semitone a row, and the notes of one pitch at one height", () => {
    const roll = rollOf([note(0, 1, 60), note(1, 1, 67), note(2, 1, 60)], 4);
    const [low, high, again] = roll.notes;
    expect(low?.y).toBeGreaterThan(high?.y ?? Number.NaN);
    expect((low?.y ?? 0) - (high?.y ?? 0)).toBeCloseTo(7, 10);
    expect(again?.y).toBe(low?.y);
  });

  test("are all inside the picture, from the lowest and highest pitch there can be to the range of a few: the roll is as high as the notes go when they go further than the least height", () => {
    const wide = rollOf([note(0, 1, 0), note(1, 1, 127)], 4);
    expect(wide.span).toBe(128);
    for (const mark of wide.notes) expect(mark.y >= 0 && mark.y + mark.height <= wide.span).toBe(true);
    const narrow = rollOf([note(0, 1, 60), note(1, 1, 64)], 4);
    expect(narrow.span).toBe(ROLL_MIN_SPAN);
    for (const mark of narrow.notes) expect(mark.y >= 0 && mark.y + mark.height <= narrow.span).toBe(true);
    // (A range of exactly the least height is drawn as it is, with no room round it.)
    const exact = rollOf([note(0, 1, 60), note(1, 1, 71)], 4);
    expect(exact.span).toBe(12);
    expect(exact.notes[0]?.y).toBeCloseTo(11 + 0.075, 10);
    expect(exact.notes[1]?.y).toBeCloseTo(0.075, 10);
  });

  test("are at least as long as the shortest mark, so that a note that has no length can be seen", () => {
    const roll = rollOf([note(1, 0, 60)], 4);
    expect(roll.notes[0]?.width).toBe(ROLL_MIN_WIDTH);
  });

  test("end at the end of the loop when they go past it, and a note that begins at or past it is not in the roll: the box does not play it", () => {
    const roll = rollOf([note(3, 4, 60), note(4, 1, 62), note(5, 1, 64)], 4);
    expect(roll.notes).toHaveLength(1);
    expect(roll.notes[0]).toMatchObject({ x: 3, width: 1 });
    // (And a note past the end does not make the roll higher.)
    expect(roll.span).toBe(ROLL_MIN_SPAN);
  });

  test("are drawn the fainter the softer they were played: the opacity is 0.4 for the softest and 1 for the hardest, and a velocity that is out of range is held to them", () => {
    const [soft, loud, wild, none] = rollOf([note(0, 1, 60, 1), note(1, 1, 60, 127), note(2, 1, 60, 200), note(3, 1, 60, -5)], 4).notes;
    expect(soft?.opacity).toBeCloseTo(0.4 + 0.6 / 127, 10);
    expect(loud?.opacity).toBe(1);
    expect(wild?.opacity).toBe(1);
    expect(none?.opacity).toBeCloseTo(0.4, 10);
  });

  test("keep the order that they are given in", () => {
    const roll = rollOf([note(2, 1, 64), note(0, 1, 60), note(1, 1, 62)], 4);
    expect(roll.notes.map((mark) => mark.x)).toEqual([2, 0, 1]);
  });
});

describe("the lines of the beats", () => {
  test("are a beat apart in a loop of up to 32 beats, a bar apart up to 128, and four bars apart beyond", () => {
    expect([0.25, 4, 32].map(lineEvery)).toEqual([1, 1, 1]);
    expect([33, 64, 128].map(lineEvery)).toEqual([4, 4, 4]);
    expect([129, 256].map(lineEvery)).toEqual([16, 16]);
  });

  test("are heavier at each bar, which is every fourth beat, wherever they begin", () => {
    const lines = rollOf([], 9).lines;
    expect(lines.map((line) => line.at)).toEqual([1, 2, 3, 4, 5, 6, 7, 8]);
    expect(lines.filter((line) => line.bar).map((line) => line.at)).toEqual([4, 8]);
  });

  test("are as many as fit in a long loop without being too close to tell apart", () => {
    expect(rollOf([], 64).lines).toHaveLength(15);
    expect(rollOf([], 256).lines).toHaveLength(15);
    expect(rollOf([], 32).lines).toHaveLength(31);
    expect(rollOf([], 256).lines.every((line) => line.bar)).toBe(true);
  });
});
