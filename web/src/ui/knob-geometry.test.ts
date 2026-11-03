import { describe, expect, test } from "vitest";

import { afterKey, angleOf, arcPath, dragged, DRAG_PIXELS, fractionOf, pointAt, stepped, SWEEP_DEGREES, valueAt } from "./knob-geometry";

describe("where a value is on the sweep", () => {
  test("is a fraction of the range, clamped", () => {
    expect(fractionOf(0, 0, 10)).toBe(0);
    expect(fractionOf(5, 0, 10)).toBe(0.5);
    expect(fractionOf(10, 0, 10)).toBe(1);
    expect(fractionOf(-3, 0, 10)).toBe(0);
    expect(fractionOf(30, 0, 10)).toBe(1);
    expect(fractionOf(0, -24, 24)).toBe(0.5);
  });

  test("is nothing for a range that is nothing", () => {
    expect(fractionOf(5, 5, 5)).toBe(0);
    expect(fractionOf(5, 7, 3)).toBe(0);
  });

  test("goes back to the value", () => {
    expect(valueAt(0.25, 0, 8)).toBe(2);
    expect(valueAt(0, -24, 24)).toBe(-24);
    expect(valueAt(1.5, 0, 8)).toBe(8);
    expect(valueAt(fractionOf(3.5, 1, 9), 1, 9)).toBeCloseTo(3.5, 10);
  });

  test("is an angle from lower left to lower right through straight up", () => {
    expect(angleOf(0)).toBe(-135);
    expect(angleOf(0.5)).toBe(0);
    expect(angleOf(1)).toBe(135);
    expect(angleOf(2)).toBe(135);
    expect(SWEEP_DEGREES).toBe(270);
  });
});

describe("a point on the knob", () => {
  test("is where the angle says, clockwise from up", () => {
    const up = pointAt(24, 10, 0);
    expect([up.x, up.y]).toEqual([24, 14]);
    const right = pointAt(24, 10, 90);
    expect(right.x).toBeCloseTo(34, 10);
    expect(right.y).toBeCloseTo(24, 10);
    const down = pointAt(24, 10, 180);
    expect(down.x).toBeCloseTo(24, 10);
    expect(down.y).toBeCloseTo(34, 10);
    const left = pointAt(24, 10, -90);
    expect(left.x).toBeCloseTo(14, 10);
  });
});

describe("an arc", () => {
  test("runs clockwise from the first fraction to the second", () => {
    // The whole sweep, from lower left to lower right, on a circle of 10 about (24, 24).
    const path = arcPath(24, 10, 0, 1);
    const [, sx, sy, , rx, ry, , large, sweep, ex, ey] = path.split(" ");
    expect([Number(sx), Number(sy)]).toEqual([16.93, 31.07]);
    expect([Number(ex), Number(ey)]).toEqual([31.07, 31.07]);
    expect([rx, ry]).toEqual(["10", "10"]);
    expect([large, sweep]).toEqual(["1", "1"]);
  });

  test("is a small arc for a short way and a large one for more than half a circle", () => {
    expect(arcPath(24, 10, 0.4, 0.6).split(" ")[7]).toBe("0");
    expect(arcPath(24, 10, 0, 0.7).split(" ")[7]).toBe("1");
    expect(arcPath(24, 10, 0, 2 / 3).split(" ")[7]).toBe("0"); // exactly 180 degrees is not more
  });

  test("is the same arc whichever way round its ends are given", () => {
    expect(arcPath(24, 10, 0.7, 0.2)).toBe(arcPath(24, 10, 0.2, 0.7));
  });

  test("is nothing when it has no length", () => {
    expect(arcPath(24, 10, 0.5, 0.5)).toBe("");
    expect(arcPath(24, 10, 0.5, 0.50001)).toBe("");
  });
});

describe("dragging", () => {
  test("raises the value by the share of the range that the pointer has moved up", () => {
    expect(dragged(5, -DRAG_PIXELS / 4, 0, 10)).toBe(7.5);
    expect(dragged(5, DRAG_PIXELS / 4, 0, 10)).toBe(2.5);
    expect(dragged(0, -DRAG_PIXELS, -24, 24)).toBe(24);
  });

  test("is slower when asked to be", () => {
    expect(dragged(5, -DRAG_PIXELS / 4, 0, 10, 0.1)).toBeCloseTo(5.25, 10);
  });

  test("stays in the range", () => {
    expect(dragged(9, -DRAG_PIXELS, 0, 10)).toBe(10);
    expect(dragged(1, DRAG_PIXELS, 0, 10)).toBe(0);
  });
});

describe("stepping", () => {
  test("moves by a share of the range, in either direction, and stays in it", () => {
    expect(stepped(5, 1, 0.01, 0, 10)).toBeCloseTo(5.1, 10);
    expect(stepped(5, -10, 0.01, 0, 10)).toBeCloseTo(4, 10);
    expect(stepped(9.95, 1, 0.01, 0, 10)).toBe(10);
    expect(stepped(0.05, -1, 0.01, 0, 10)).toBe(0);
  });
});

describe("a key press", () => {
  test("steps by a hundredth of the range for an arrow, either way", () => {
    expect(afterKey("ArrowUp", false, 5, 0, 10)).toBeCloseTo(5.1, 10);
    expect(afterKey("ArrowRight", false, 5, 0, 10)).toBeCloseTo(5.1, 10);
    expect(afterKey("ArrowDown", false, 5, 0, 10)).toBeCloseTo(4.9, 10);
    expect(afterKey("ArrowLeft", false, 5, 0, 10)).toBeCloseTo(4.9, 10);
  });

  test("steps by a thousandth when asked to be fine", () => {
    expect(afterKey("ArrowUp", true, 5, 0, 10)).toBeCloseTo(5.01, 10);
    expect(afterKey("ArrowDown", true, 5, 0, 10)).toBeCloseTo(4.99, 10);
    expect(afterKey("PageUp", true, 5, 0, 10)).toBeCloseTo(5.1, 10);
  });

  test("steps by ten for the page keys", () => {
    expect(afterKey("PageUp", false, 5, 0, 10)).toBeCloseTo(6, 10);
    expect(afterKey("PageDown", false, 5, 0, 10)).toBeCloseTo(4, 10);
  });

  test("goes to the ends for Home and End, whatever the range is", () => {
    expect(afterKey("Home", false, 5, -24, 24)).toBe(-24);
    expect(afterKey("End", false, 5, -24, 24)).toBe(24);
  });

  test("stays in the range", () => {
    expect(afterKey("ArrowUp", false, 10, 0, 10)).toBe(10);
    expect(afterKey("PageDown", false, 0.5, 0, 10)).toBe(0);
  });

  test("is nothing for a key that is not the knob's", () => {
    for (const key of ["a", "Enter", "Tab", "Escape", " ", "Shift"]) {
      expect(afterKey(key, false, 5, 0, 10), key).toBeUndefined();
    }
  });
});

describe("a logarithmic scale", () => {
  // From 0.01 to 100 is four decades: each decade is a quarter of the sweep, so that a rate of 1 is in the middle.
  test("spaces equal ratios equally", () => {
    expect(fractionOf(0.01, 0.01, 100, "log")).toBe(0);
    expect(fractionOf(0.1, 0.01, 100, "log")).toBeCloseTo(0.25, 10);
    expect(fractionOf(1, 0.01, 100, "log")).toBeCloseTo(0.5, 10);
    expect(fractionOf(10, 0.01, 100, "log")).toBeCloseTo(0.75, 10);
    expect(fractionOf(100, 0.01, 100, "log")).toBe(1);
  });

  test("keeps a value that is out of the range, or no value at all, at an end", () => {
    expect(fractionOf(0.001, 0.01, 100, "log")).toBe(0);
    expect(fractionOf(0, 0.01, 100, "log")).toBe(0);
    expect(fractionOf(-5, 0.01, 100, "log")).toBe(0);
    expect(fractionOf(1000, 0.01, 100, "log")).toBe(1);
  });

  test("is a linear scale for a range that starts at nothing or below, which has no logarithm", () => {
    expect(fractionOf(5, 0, 10, "log")).toBe(0.5);
    expect(fractionOf(0, -24, 24, "log")).toBe(0.5);
    expect(valueAt(0.5, 0, 10, "log")).toBe(5);
    expect(dragged(5, -DRAG_PIXELS / 4, 0, 10, 1, "log")).toBe(7.5);
  });

  test("goes back to the value, and to the ends exactly", () => {
    expect(valueAt(0.5, 0.01, 100, "log")).toBeCloseTo(1, 10);
    expect(valueAt(0.75, 0.01, 100, "log")).toBeCloseTo(10, 10);
    expect(valueAt(0, 0.01, 100, "log")).toBe(0.01);
    expect(valueAt(1, 0.001, 10, "log")).toBe(10);
    expect(valueAt(-1, 0.01, 100, "log")).toBe(0.01);
    expect(valueAt(2, 0.01, 100, "log")).toBe(100);
    // (A range for which the arithmetic of the top is not exact, 7.000000000000001, and the top is still 7.)
    expect(valueAt(1, 0.003, 7, "log")).toBe(7);
    expect(valueAt(fractionOf(3.7, 0.001, 10, "log"), 0.001, 10, "log")).toBeCloseTo(3.7, 10);
  });

  test("is dragged by a share of the sweep, not of the range", () => {
    // A quarter of the drag up from 1 (the middle) is a quarter of the sweep on: 1 to 10.
    expect(dragged(1, -DRAG_PIXELS / 4, 0.01, 100, 1, "log")).toBeCloseTo(10, 9);
    expect(dragged(1, DRAG_PIXELS / 4, 0.01, 100, 1, "log")).toBeCloseTo(0.1, 10);
    expect(dragged(1, -DRAG_PIXELS / 4, 0.01, 100, 0.1, "log")).toBeCloseTo(10 ** 0.1, 9);
    expect(dragged(50, -DRAG_PIXELS, 0.01, 100, 1, "log")).toBe(100);
    expect(dragged(0.02, DRAG_PIXELS, 0.01, 100, 1, "log")).toBe(0.01);
  });

  test("is not moved by a drag that has not moved", () => {
    expect(dragged(3.3, 0, 0.01, 100, 1, "log")).toBe(3.3);
  });

  test("is stepped by a share of the sweep", () => {
    expect(stepped(1, 1, 0.25, 0.01, 100, "log")).toBeCloseTo(10, 9);
    expect(stepped(1, -2, 0.25, 0.01, 100, "log")).toBeCloseTo(0.01, 9);
    expect(stepped(50, 1, 0.25, 0.01, 100, "log")).toBe(100);
    expect(stepped(3.3, 0, 0.25, 0.01, 100, "log")).toBe(3.3);
  });

  test("is moved by the keys by a hundredth of the sweep, to the ends by Home and End", () => {
    expect(afterKey("ArrowUp", false, 1, 0.01, 100, "log")).toBeCloseTo(10 ** 0.04, 9);
    expect(afterKey("ArrowDown", true, 1, 0.01, 100, "log")).toBeCloseTo(10 ** -0.004, 9);
    expect(afterKey("PageUp", false, 1, 0.01, 100, "log")).toBeCloseTo(10 ** 0.4, 9);
    expect(afterKey("Home", false, 1, 0.01, 100, "log")).toBe(0.01);
    expect(afterKey("End", false, 1, 0.01, 100, "log")).toBe(100);
    // At the end there is nowhere to go, and the value that comes back is the end itself.
    expect(afterKey("ArrowUp", false, 100, 0.01, 100, "log")).toBe(100);
    expect(afterKey("ArrowDown", false, 0.01, 0.01, 100, "log")).toBe(0.01);
  });

  test("is the linear scale unless asked for", () => {
    expect(fractionOf(1, 0.01, 100)).toBeCloseTo(0.99 / 99.99, 10);
    expect(dragged(5, -DRAG_PIXELS / 4, 0, 10)).toBe(7.5);
  });
});
