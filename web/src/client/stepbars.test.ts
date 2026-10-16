import { describe, expect, test } from "vitest";

import { columnAt, paintTo, playheadStep, stepValueAt } from "./stepbars";

describe("the bar that a pointer is over", () => {
  test("is counted from the left, a bar to every tenth of the width when there are ten", () => {
    expect(columnAt(0, 160, 16)).toBe(0);
    expect(columnAt(9.9, 160, 16)).toBe(0);
    expect(columnAt(10, 160, 16)).toBe(1);
    expect(columnAt(80, 160, 16)).toBe(8);
    expect(columnAt(159.9, 160, 16)).toBe(15);
    expect(columnAt(55, 100, 10)).toBe(5);
  });

  test("is the one at the end for a pointer that has gone past it, and the first for a box with no width", () => {
    expect(columnAt(-30, 160, 16)).toBe(0);
    expect(columnAt(160, 160, 16)).toBe(15);
    expect(columnAt(900, 160, 16)).toBe(15);
    expect(columnAt(50, 0, 16)).toBe(0);
    expect(columnAt(50, 160, 0)).toBe(0);
  });
});

describe("the value that a pointer sets", () => {
  test("is 1 at the top, 0 in the middle and -1 at the bottom, and goes evenly between", () => {
    expect(stepValueAt(0, 64)).toBe(1);
    expect(stepValueAt(32, 64)).toBe(0);
    expect(stepValueAt(64, 64)).toBe(-1);
    expect(stepValueAt(16, 64)).toBe(0.5);
    expect(stepValueAt(48, 64)).toBe(-0.5);
  });

  test("is held to the ends for a pointer that has gone past them, and is none for a box with no height", () => {
    expect(stepValueAt(-40, 64)).toBe(1);
    expect(stepValueAt(200, 64)).toBe(-1);
    expect(stepValueAt(10, 0)).toBe(0);
  });

  test("is to a hundredth, so that a session file does not hold the pixel the pointer was on", () => {
    expect(stepValueAt(64 * (1 - 0.3333) * 0.5, 64)).toBe(0.33);
    expect(stepValueAt(64 * (1 - 0.6666) * 0.5, 64)).toBe(0.67);
    expect(stepValueAt(64 * (1 + 0.2549) * 0.5, 64)).toBe(-0.25);
  });

  test("is nothing a little above and below the middle, where a hand cannot stop, and not a hundredth more than that", () => {
    for (const raw of [0.04, 0.02, 0, -0.02, -0.04]) expect(Object.is(stepValueAt(64 * (1 - raw) * 0.5, 64), 0), `at ${String(raw)}`).toBe(true);
    expect(stepValueAt(64 * (1 - 0.05) * 0.5, 64)).toBe(0.05);
    expect(stepValueAt(64 * (1 + 0.05) * 0.5, 64)).toBe(-0.05);
  });
});

describe("what a stroke sets", () => {
  const rest = Array.from({ length: 8 }, () => 0);

  test("is a bar, at the start of a stroke, and nothing else; the list that it was made from is not changed", () => {
    const made = paintTo(rest, null, { column: 3, value: 0.5 });
    expect(made).toEqual([0, 0, 0, 0.5, 0, 0, 0, 0]);
    expect(rest).toEqual(Array.from({ length: 8 }, () => 0));
  });

  test("is the bars between two places of one stroke, along the line from one to the other, so that a quick stroke leaves no gaps", () => {
    expect(paintTo(rest, { column: 1, value: 0 }, { column: 5, value: 1 })).toEqual([0, 0, 0.25, 0.5, 0.75, 1, 0, 0]);
    expect(paintTo(rest, { column: 6, value: 1 }, { column: 2, value: -1 })).toEqual([0, 0, -1, -0.5, 0, 0.5, 0, 0]);
  });

  test("keeps the bar the stroke came from, and sets the one it is at when the stroke has not left its bar", () => {
    const from = { column: 2, value: 0.4 };
    expect(paintTo([0.1, 0.2, 0.4, 0.3], from, { column: 2, value: 0.9 })).toEqual([0.1, 0.2, 0.9, 0.3]);
    expect(paintTo([0.1, 0.2, 0.4, 0.3], from, { column: 3, value: 0 })).toEqual([0.1, 0.2, 0.4, 0]);
  });

  test("is made of hundredths, and of no negative zero", () => {
    const made = paintTo(rest, { column: 0, value: 0.01 }, { column: 3, value: 0 });
    // (The bar that the stroke came from was set when it was there: it is not set again.)
    expect(made.slice(0, 4)).toEqual([0, 0.01, 0, 0]);
    expect(made.every((value) => !Object.is(value, -0))).toBe(true);
    const started = paintTo(rest, null, { column: 0, value: 1 });
    expect(paintTo(started, { column: 0, value: 1 }, { column: 3, value: 0 }).slice(0, 4)).toEqual([1, 0.67, 0.33, 0]);
  });
});

describe("the bar that the playhead is on", () => {
  test("is the step that the position is in, the whole part of it", () => {
    expect(playheadStep(0, 16)).toBe(0);
    expect(playheadStep(0.99, 16)).toBe(0);
    expect(playheadStep(2.5, 16)).toBe(2);
    expect(playheadStep(15.99, 16)).toBe(15);
  });

  test("goes round at the length, for a position that is past it (the length was shortened) or before the start", () => {
    expect(playheadStep(3.2, 3)).toBe(0);
    expect(playheadStep(7.5, 3)).toBe(1);
    expect(playheadStep(-0.5, 3)).toBe(2);
  });

  test("is none when there is no position to follow, and the first step for a length of none", () => {
    expect(playheadStep(undefined, 16)).toBe(-1);
    expect(playheadStep(Number.NaN, 16)).toBe(-1);
    expect(playheadStep(Number.POSITIVE_INFINITY, 16)).toBe(-1);
    expect(playheadStep(2.5, 0)).toBe(0);
  });
});
