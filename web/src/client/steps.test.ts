import { describe, expect, test } from "vitest";

import { stepsFixture } from "./fixtures";
import { stepsOutput, stepsPlace } from "./steps";

// What a step sequencer puts out (`plugin::chain::steps_output`): the page's law, pinned to the engine's by what the engine makes of a spread of cases (the fixture).
describe("what a step sequencer puts out", () => {
  test("is what the engine makes of each case of its fixture, to a millionth", () => {
    expect(stepsFixture.cases.length).toBeGreaterThan(500);
    const wrong = stepsFixture.cases.filter((entry) => {
      const values = stepsFixture.patterns[entry.pattern];
      return values === undefined || Math.abs(stepsOutput(values, entry.length, entry.position, entry.glide) - entry.output) > 1e-6;
    });
    expect(wrong).toEqual([]);
  });

  test("has been tried with a glide and without, with one step and with all of them, at the start and at the end: the fixture is not thin", () => {
    const has = (test: (entry: (typeof stepsFixture.cases)[number]) => boolean) => stepsFixture.cases.some(test);
    expect(has((entry) => entry.glide === 0)).toBe(true);
    expect(has((entry) => entry.glide === 1)).toBe(true);
    expect(has((entry) => entry.glide > 0 && entry.glide < 1)).toBe(true);
    expect(has((entry) => entry.length === 1)).toBe(true);
    expect(has((entry) => entry.length === 16 && entry.position > 15.9)).toBe(true);
    expect(has((entry) => entry.position === 0)).toBe(true);
    expect(new Set(stepsFixture.cases.map((entry) => entry.output)).size, "the outputs are not alike").toBeGreaterThan(20);
  });

  const values = [0.5, 1, -1, 0.9, ...Array<number>(12).fill(0)];
  test("is the value of the step all through it, with no glide", () => {
    for (const position of [0, 0.25, 0.999, 1, 1.5, 2.999]) expect(stepsOutput(values, 3, position, 0), `at ${String(position)}`).toBe(values[Math.floor(position)]);
  });

  test("goes to the value of the next step over the last part of the step that the glide says, and from the last to the first", () => {
    const at = (position: number) => stepsOutput(values, 3, position, 0.5);
    expect(at(0)).toBe(0.5);
    expect(at(0.5), "the glide begins after the half").toBe(0.5);
    expect(at(0.75)).toBeCloseTo(0.75, 9);
    expect(at(1)).toBe(1);
    expect(at(1.75)).toBeCloseTo(0, 9);
    expect(at(2.75), "the last goes to the first, and not to the step that is not in use").toBeCloseTo(-0.25, 9);
    expect(stepsOutput(values, 3, 0.25, 1)).toBeCloseTo(0.625, 9);
    expect(stepsOutput(values, 3, 1.5, 1)).toBeCloseTo(0, 9);
  });

  test("has nothing to go to when it is one step, and keeps a glide to 0 to 1", () => {
    expect(stepsOutput(values, 1, 0.9, 1)).toBe(0.5);
    expect(stepsOutput(values, 3, 0.25, 7)).toBe(stepsOutput(values, 3, 0.25, 1));
    expect(stepsOutput(values, 3, 0.9, -2)).toBe(0.5);
  });

  test("is kept to the steps there are, goes round at the length, and is the beginning for a place that is not a number", () => {
    const ends = [...Array<number>(15).fill(0), -0.6];
    ends[0] = 0.3;
    expect(stepsOutput(ends, 0, 0, 0), "a length of none is one").toBe(0.3);
    expect(stepsOutput(ends, 99, 15.5, 0), "and one of too many is all of them").toBe(-0.6);
    expect(stepsOutput(ends, 99, 16, 0), "which goes round").toBe(0.3);
    expect(stepsOutput(values, 3, Number.NaN, 0.5)).toBe(0.5);
    expect(stepsOutput(values, 3, -0.5, 0)).toBe(-1);
  });
});

// Where a step sequencer is, as the engine publishes it for a playhead: the step it is at as the whole part of the number, how far into it as the fraction.
describe("where a step sequencer is", () => {
  test("goes round at the length: the whole part is the step, the fraction how far into it", () => {
    expect(stepsPlace(3, 0, 16)).toBe(0);
    expect(stepsPlace(3, 2.5, 16)).toBe(2.5);
    expect(stepsPlace(3, 3, 16)).toBe(0);
    expect(stepsPlace(3, 4.25, 16)).toBe(1.25);
    expect(stepsPlace(3, -0.5, 16)).toBe(2.5);
  });

  test("counts a length of none as one and one of too many as all the steps, and a place that is not a number as the beginning", () => {
    expect(stepsPlace(0, 2.5, 16)).toBe(0.5);
    expect(stepsPlace(99, 17.5, 16)).toBe(1.5);
    expect(stepsPlace(3, Number.NaN, 16)).toBe(0);
    expect(stepsPlace(3, Number.POSITIVE_INFINITY, 16)).toBe(0);
  });

  test("is the place that the output of the step sequencer is worked out from", () => {
    const values = Array.from({ length: 16 }, (_, step) => step / 10);
    for (const position of [0, 0.5, 2.999, 3, 7.25, 15.5, 16.5, -1.5]) {
      const place = stepsPlace(5, position, 16);
      expect(stepsOutput(values, 5, position, 0), `at ${String(position)}`).toBe(values[Math.floor(place)]);
    }
  });
});
