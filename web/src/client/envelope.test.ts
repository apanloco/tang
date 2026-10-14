import { describe, expect, test } from "vitest";

import { envelopeAreaPath, envelopeDot, envelopePath, placeIn, shapeOf, stageWeight } from "./envelope";

/** The envelope the engine starts a new one as: attack 10 ms, decay 0.3 s, sustain 0.7, release 0.5 s. */
const NEW = { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 };

describe("how much room a stage has in the shape of an envelope", () => {
  test("is the logarithm of how many milliseconds it is, plus one: nothing for no time, and more for more", () => {
    expect(stageWeight(0)).toBe(0);
    expect(stageWeight(0.001)).toBeCloseTo(Math.log10(2), 12);
    expect(stageWeight(1)).toBeCloseTo(Math.log10(1001), 12);
    expect(stageWeight(10)).toBeCloseTo(Math.log10(10001), 12);
    const weights = [0.001, 0.002, 0.01, 0.1, 0.5, 1, 3, 10].map(stageWeight);
    expect(weights).toEqual([...weights].sort((a, b) => a - b));
    expect(new Set(weights).size).toBe(weights.length);
  });

  test("is none for a time that is not one (below nothing, not a number, or without an end)", () => {
    expect(stageWeight(-1)).toBe(0);
    expect(stageWeight(Number.NaN)).toBe(0);
    expect(stageWeight(Number.POSITIVE_INFINITY)).toBe(0);
  });
});

describe("the shape of an envelope", () => {
  test("has its stages one after the other from the left edge to the right, the hold of the sustain between the decay and the release", () => {
    const shape = shapeOf(NEW);
    expect(shape.attackEnd).toBeGreaterThan(0);
    expect(shape.decayEnd).toBeGreaterThan(shape.attackEnd);
    expect(shape.holdEnd).toBeGreaterThan(shape.decayEnd);
    expect(shape.holdEnd).toBeLessThan(1);
    expect(shape.sustain).toBe(0.7);
  });

  test("gives a stage that is longer more width, whichever stage it is", () => {
    const width = (stages: typeof NEW) => {
      const s = shapeOf(stages);
      return { attack: s.attackEnd, decay: s.decayEnd - s.attackEnd, release: 1 - s.holdEnd };
    };
    expect(width({ ...NEW, attack: 5 }).attack).toBeGreaterThan(width({ ...NEW, attack: 0.005 }).attack);
    expect(width({ ...NEW, decay: 5 }).decay).toBeGreaterThan(width({ ...NEW, decay: 0.005 }).decay);
    expect(width({ ...NEW, release: 5 }).release).toBeGreaterThan(width({ ...NEW, release: 0.005 }).release);
  });

  test("keeps a stage of a millisecond visible next to one of ten seconds, which a scale of time that is not logarithmic would not", () => {
    const shape = shapeOf({ attack: 0.001, decay: 10, sustain: 0.5, release: 10 });
    expect(shape.attackEnd).toBeGreaterThan(0.02);
  });

  test("has a hold for the sustain even where every stage is nothing, and the shape is the same width as any other", () => {
    const shape = shapeOf({ attack: 0, decay: 0, sustain: 1, release: 0 });
    expect(shape.attackEnd).toBe(0);
    expect(shape.decayEnd).toBe(0);
    expect(shape.holdEnd).toBe(1);
  });

  test("keeps the sustain level within 0 and 1, and at 0 for one that is not a number", () => {
    expect(shapeOf({ ...NEW, sustain: 1.5 }).sustain).toBe(1);
    expect(shapeOf({ ...NEW, sustain: -0.5 }).sustain).toBe(0);
    expect(shapeOf({ ...NEW, sustain: Number.NaN }).sustain).toBe(0);
  });
});

describe("the outline of an envelope in a box", () => {
  const box = { width: 100, height: 40, pad: 4 };

  test("is drawn from the bottom left, up to the top where the attack ends, down to the sustain, along it, and down to the bottom right", () => {
    const shape = shapeOf({ attack: 0.1, decay: 0.1, sustain: 0.5, release: 0.1 });
    // Each stage 2.0414 wide (log10(101)), the hold the mean of them: a fifth each, the hold of the sustain the same as the others' width.
    const path = envelopePath(shape, box);
    const numbers = (path.match(/-?\d+(?:\.\d+)?/g) ?? []).map(Number);
    expect(path.startsWith("M")).toBe(true);
    expect(numbers).toHaveLength(10);
    const [x0, y0, x1, y1, x2, y2, x3, y3, x4, y4] = numbers as [number, number, number, number, number, number, number, number, number, number];
    expect([x0, y0]).toEqual([4, 36]);
    expect(y1).toBe(4);
    expect(y2).toBe(20);
    expect(y3).toBe(20);
    expect([x4, y4]).toEqual([96, 36]);
    expect(x1).toBeGreaterThan(x0);
    expect(x2).toBeGreaterThan(x1);
    expect(x3).toBeGreaterThan(x2);
    expect(x4).toBeGreaterThan(x3);
  });

  test("is written to a tenth of a pixel, which is as fine as a line is seen and keeps it short", () => {
    const path = envelopePath(shapeOf(NEW), { width: 272, height: 56, pad: 4 });
    const numbers = path.match(/-?\d+(?:\.\d+)?/g) ?? [];
    expect(numbers).toHaveLength(10);
    for (const number of numbers) expect(number.split(".")[1]?.length ?? 0).toBeLessThanOrEqual(1);
    expect(numbers.some((number) => number.includes("."))).toBe(true);
  });

  test("has the same points as the outline that is filled, which is closed along the bottom, where it begins and ends", () => {
    const shape = shapeOf(NEW);
    expect(envelopeAreaPath(shape, box)).toBe(`${envelopePath(shape, box)} Z`);
  });

  test("puts a sustain of 1 at the top and a sustain of 0 at the bottom", () => {
    const top = envelopePath(shapeOf({ ...NEW, sustain: 1 }), box);
    const bottom = envelopePath(shapeOf({ ...NEW, sustain: 0 }), box);
    const ys = (path: string) => (path.match(/-?\d+(?:\.\d+)?/g) ?? []).map(Number).filter((_, i) => i % 2 === 1);
    expect(ys(top)).toEqual([36, 4, 4, 4, 36]);
    expect(ys(bottom)).toEqual([36, 4, 36, 36, 36]);
  });
});

describe("a place on the shape in a box", () => {
  const box = { width: 100, height: 40, pad: 4 };

  test("is the share of the width and the level, inside the margin: the bottom left is nothing at the start, the top right the top at the end", () => {
    expect(placeIn(box, 0, 0)).toEqual({ x: 4, y: 36 });
    expect(placeIn(box, 1, 1)).toEqual({ x: 96, y: 4 });
    expect(placeIn(box, 0.5, 0.5)).toEqual({ x: 50, y: 20 });
  });

  test("is where the outline has its corners", () => {
    const shape = shapeOf(NEW);
    const corner = placeIn(box, shape.attackEnd, 1);
    expect(envelopePath(shape, box)).toContain(`L${String(Math.round(corner.x * 10) / 10)} ${String(corner.y)}`);
  });
});

describe("where the envelope is on its shape, from the level it puts out and the level it put out before", () => {
  const shape = shapeOf(NEW);

  test("is at the start while it puts out nothing", () => {
    expect(envelopeDot(shape, 0, 0)).toEqual({ x: 0, y: 0 });
    expect(envelopeDot(shape, 0, 0.4)).toEqual({ x: 0, y: 0 });
    expect(envelopeDot(shape, 0.00001, 0)).toEqual({ x: 0, y: 0 });
  });

  test("is on the attack while the level rises: the level of the way up, of the width of the attack", () => {
    const dot = envelopeDot(shape, 0.5, 0.2);
    expect(dot.y).toBe(0.5);
    expect(dot.x).toBeCloseTo(0.5 * shape.attackEnd, 12);
    // A note that comes while the envelope is still dying away goes on from where the level is.
    expect(envelopeDot(shape, 0.3, 0.2).x).toBeCloseTo(0.3 * shape.attackEnd, 12);
  });

  test("is on the decay while the level falls and is above the sustain: the share of the way down from the top to the sustain", () => {
    const halfway = 1 - (1 - 0.7) / 2;
    const dot = envelopeDot(shape, halfway, 0.95);
    expect(dot.y).toBeCloseTo(halfway, 12);
    expect(dot.x).toBeCloseTo((shape.attackEnd + shape.decayEnd) / 2, 12);
  });

  test("is on the release while the level falls from the sustain: the share of the way down from the sustain to nothing, in the width of the release", () => {
    const dot = envelopeDot(shape, 0.35, 0.7);
    expect(dot.y).toBe(0.35);
    expect(dot.x).toBeCloseTo((shape.holdEnd + 1) / 2, 12);
    // A fifth of the sustain is four fifths of the way down.
    expect(envelopeDot(shape, 0.14, 0.3).x).toBeCloseTo(shape.holdEnd + 0.8 * (1 - shape.holdEnd), 12);
    // At the end of the release, the start again.
    expect(envelopeDot(shape, 0, 0.0001)).toEqual({ x: 0, y: 0 });
  });

  test("is in the middle of the hold while the level stays at the sustain, and at the top of the attack while it stays at the top", () => {
    const held = envelopeDot(shape, 0.7, 0.7);
    expect(held.y).toBe(0.7);
    expect(held.x).toBeCloseTo((shape.decayEnd + shape.holdEnd) / 2, 12);
    expect(envelopeDot(shape, 1, 1)).toEqual({ x: shape.attackEnd, y: 1 });
  });

  test("takes a change too small to be one for the level standing still, and a level a hair from the sustain for the sustain", () => {
    const held = (shape.decayEnd + shape.holdEnd) / 2;
    expect(envelopeDot(shape, 0.7 + 5e-6, 0.7).x).toBeCloseTo(held, 12);
    expect(envelopeDot(shape, 0.7 - 5e-6, 0.7).x).toBeCloseTo(held, 12);
    expect(envelopeDot(shape, 0.7 + 5e-4, 0.7 + 5e-4).x).toBeCloseTo(held, 12);
    expect(envelopeDot(shape, 0.7 - 5e-4, 0.7 - 5e-4).x).toBeCloseTo(held, 12);
    // A change that is one is: a little above the sustain and falling is the decay, a little below it the release.
    expect(envelopeDot(shape, 0.7 + 2e-3, 0.7 + 4e-3).x).toBeLessThan(held);
    expect(envelopeDot(shape, 0.7 - 2e-3, 0.7 - 5e-4).x).toBeGreaterThan(held);
    // And a level a hair under the top that stands still is the top of the attack.
    expect(envelopeDot(shape, 1 - 5e-4, 1 - 5e-4).x).toBe(shape.attackEnd);
    expect(envelopeDot(shape, 1 - 2e-3, 1 - 2e-3).x).toBeGreaterThan(shape.attackEnd);
  });

  test("is at the end of the decay when the level falls onto the sustain, and finite for a sustain of 1 whatever the level before", () => {
    expect(envelopeDot(shape, 0.7, 0.75).x).toBeCloseTo(shape.decayEnd, 12);
    const full = shapeOf({ ...NEW, sustain: 1 });
    const dot = envelopeDot(full, 1, 9);
    expect(Number.isFinite(dot.x) && Number.isFinite(dot.y)).toBe(true);
    expect(dot.x).toBeLessThanOrEqual(1);
  });

  test("is on the decay or the release by the level when it is steady somewhere else: above the sustain the decay, below it the release", () => {
    expect(envelopeDot(shape, 0.85, 0.85).x).toBeGreaterThan(shape.attackEnd);
    expect(envelopeDot(shape, 0.85, 0.85).x).toBeLessThan(shape.decayEnd);
    expect(envelopeDot(shape, 0.3, 0.3).x).toBeGreaterThan(shape.holdEnd);
  });

  test("never leaves the box, whatever the level: it is kept within 0 and 1, and a level that is not a number is at the start", () => {
    for (const [level, before] of [[1.7, 0], [-0.4, 0.5], [0.5, 9], [Number.NaN, 0.5], [0.5, Number.NaN]] as const) {
      const dot = envelopeDot(shape, level, before);
      expect(dot.x, `${String(level)} after ${String(before)}`).toBeGreaterThanOrEqual(0);
      expect(dot.x).toBeLessThanOrEqual(1);
      expect(dot.y).toBeGreaterThanOrEqual(0);
      expect(dot.y).toBeLessThanOrEqual(1);
    }
    expect(envelopeDot(shape, Number.NaN, 0.5)).toEqual({ x: 0, y: 0 });
  });

  test("is on the release from the top, and on the attack and nothing else, for a sustain of 1 (there is no decay to be on) and a sustain of 0 (the release is nothing)", () => {
    const full = shapeOf({ ...NEW, sustain: 1 });
    expect(envelopeDot(full, 0.6, 1).x).toBeGreaterThan(full.holdEnd);
    expect(envelopeDot(full, 1, 1).y).toBe(1);
    const none = shapeOf({ ...NEW, sustain: 0 });
    // Falling from the top to nothing is all decay; at the end it is idle.
    const dot = envelopeDot(none, 0.5, 0.9);
    expect(dot.x).toBeGreaterThan(none.attackEnd);
    expect(dot.x).toBeLessThan(none.decayEnd);
    expect(envelopeDot(none, 0, 0.1)).toEqual({ x: 0, y: 0 });
  });
});
