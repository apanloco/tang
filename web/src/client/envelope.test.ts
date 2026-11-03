import { describe, expect, test } from "vitest";

import { courseAfter, envelopeAreaPath, envelopeDot, envelopePath, placeIn, shapeOf, stageWeight } from "./envelope";

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

describe("which way the level of an envelope is going", () => {
  test("is not known to be rising when there is no course before it: the first level read, or the first after a gap", () => {
    expect(courseAfter(0.5, undefined, 40)).toEqual({ level: 0.5, rising: false, since: 40 });
    expect(courseAfter(0, undefined, 0)).toEqual({ level: 0, rising: false, since: 0 });
  });

  test("is rising when the level has gone up and not when it has gone down, and is the course of the level it moved to, from the time of the move", () => {
    expect(courseAfter(0.6, { level: 0.5, rising: false, since: 10 }, 50)).toEqual({ level: 0.6, rising: true, since: 50 });
    expect(courseAfter(0.4, { level: 0.5, rising: true, since: 10 }, 50)).toEqual({ level: 0.4, rising: false, since: 50 });
    expect(courseAfter(0.7, { level: 0.5, rising: true, since: 10 }, 50)).toEqual({ level: 0.7, rising: true, since: 50 });
    expect(courseAfter(0.3, { level: 0.5, rising: false, since: 10 }, 50)).toEqual({ level: 0.3, rising: false, since: 50 });
  });

  test("is the same course, the very same, while the level stands where it was: the telemetry is slower than the frames, so that is no news", () => {
    const rising = { level: 0.5, rising: true, since: 100 };
    const falling = { level: 0.5, rising: false, since: 100 };
    expect(courseAfter(0.5, rising, 116)).toBe(rising);
    expect(courseAfter(0.5, rising, 199)).toBe(rising);
    // (A fall that stands still has nothing to change into: where the level is says what it is.)
    expect(courseAfter(0.5, falling, 116)).toBe(falling);
    expect(courseAfter(0.5, falling, 100_000)).toBe(falling);
  });

  test("is no longer rising when the level has stood where it was for a hundred milliseconds, three readings of the telemetry: it is steady, not late", () => {
    const rising = { level: 0.7, rising: true, since: 100 };
    expect(courseAfter(0.7, rising, 200)).toEqual({ level: 0.7, rising: false, since: 100 });
    expect(courseAfter(0.7, rising, 5000)).toEqual({ level: 0.7, rising: false, since: 100 });
    // And it stays that course, the same one, once it is.
    const steady = courseAfter(0.7, rising, 200);
    expect(courseAfter(0.7, steady, 300)).toBe(steady);
  });

  test("counts from the time of a move: a level that moves is not steady, whatever it did before, and is when it has stood for as long as that", () => {
    const rising = { level: 0.5, rising: true, since: 100 };
    const moved = courseAfter(0.6, rising, 190);
    expect(moved).toEqual({ level: 0.6, rising: true, since: 190 });
    expect(courseAfter(0.6, moved, 289)).toBe(moved);
    expect(courseAfter(0.6, moved, 290).rising).toBe(false);
  });

  test("takes a move that is too small to be one for the level standing where it was, and the least that is one for a move", () => {
    const rising = { level: 0.5, rising: true, since: 100 };
    const falling = { level: 0.5, rising: false, since: 100 };
    expect(courseAfter(0.5 - 5e-6, rising, 116)).toBe(rising);
    expect(courseAfter(0.5 + 5e-6, falling, 116)).toBe(falling);
    expect(courseAfter(0.5 - 2e-5, rising, 116)).toEqual({ level: 0.5 - 2e-5, rising: false, since: 116 });
    expect(courseAfter(0.5 + 2e-5, falling, 116)).toEqual({ level: 0.5 + 2e-5, rising: true, since: 116 });
    // A move of exactly 1e-5 is not more than a rounding.
    expect(courseAfter(0.5 + 1e-5, falling, 116)).toBe(falling);
    expect(courseAfter(0.5 - 1e-5, rising, 116)).toBe(rising);
  });

  test("measures a move from the level that settled the course, so that a creep that adds up to a move is one", () => {
    const settled = { level: 0.5, rising: false, since: 100 };
    const creeping = courseAfter(0.5 + 6e-6, settled, 116);
    expect(creeping).toBe(settled);
    expect(courseAfter(0.5 + 1.2e-5, creeping, 133)).toEqual({ level: 0.5 + 1.2e-5, rising: true, since: 133 });
  });

  test("is the course it was for a level that is not a number: there is nothing in it to tell a move by", () => {
    const rising = { level: 0.5, rising: true, since: 100 };
    expect(courseAfter(Number.NaN, rising, 116)).toBe(rising);
  });
});

describe("where the envelope is on its shape, from the level it puts out and whether the level is rising", () => {
  const shape = shapeOf(NEW);
  const held = (shape.decayEnd + shape.holdEnd) / 2;

  test("is at the start while it puts out nothing", () => {
    expect(envelopeDot(shape, 0, false)).toEqual({ x: 0, y: 0 });
    expect(envelopeDot(shape, 0, true)).toEqual({ x: 0, y: 0 });
    expect(envelopeDot(shape, 0.00001, true)).toEqual({ x: 0, y: 0 });
  });

  test("is on the attack while the level is rising: the level of the way up, of the width of the attack, at the top it is at the end of it", () => {
    const dot = envelopeDot(shape, 0.5, true);
    expect(dot.y).toBe(0.5);
    expect(dot.x).toBeCloseTo(0.5 * shape.attackEnd, 12);
    // A note that comes while the envelope is still dying away goes on from where the level is.
    expect(envelopeDot(shape, 0.3, true).x).toBeCloseTo(0.3 * shape.attackEnd, 12);
    expect(envelopeDot(shape, 1, true)).toEqual({ x: shape.attackEnd, y: 1 });
  });

  test("is on the attack through the level that the sustain holds, which the attack goes through on its way up: it is not the hold", () => {
    expect(envelopeDot(shape, 0.7, true).x).toBeCloseTo(0.7 * shape.attackEnd, 12);
    expect(envelopeDot(shape, 0.7 + 5e-4, true).x).toBeCloseTo((0.7 + 5e-4) * shape.attackEnd, 12);
  });

  test("is on the decay while the level is not rising and is above the sustain: the share of the way down from the top to the sustain", () => {
    const halfway = 1 - (1 - 0.7) / 2;
    const dot = envelopeDot(shape, halfway, false);
    expect(dot.y).toBeCloseTo(halfway, 12);
    expect(dot.x).toBeCloseTo((shape.attackEnd + shape.decayEnd) / 2, 12);
  });

  test("is on the release while the level is not rising and is below the sustain: the share of the way down from the sustain to nothing", () => {
    const dot = envelopeDot(shape, 0.35, false);
    expect(dot.y).toBe(0.35);
    expect(dot.x).toBeCloseTo((shape.holdEnd + 1) / 2, 12);
    // A fifth of the sustain is four fifths of the way down.
    expect(envelopeDot(shape, 0.14, false).x).toBeCloseTo(shape.holdEnd + 0.8 * (1 - shape.holdEnd), 12);
    // At the end of the release, the start again.
    expect(envelopeDot(shape, 0, false)).toEqual({ x: 0, y: 0 });
  });

  test("is in the middle of the hold at the sustain when the level is not rising, and at the top of the attack at the top", () => {
    const dot = envelopeDot(shape, 0.7, false);
    expect(dot.y).toBe(0.7);
    expect(dot.x).toBeCloseTo(held, 12);
    expect(envelopeDot(shape, 1, false)).toEqual({ x: shape.attackEnd, y: 1 });
  });

  test("takes the sustain for what is in a hair of it, the 32-bit number of the telemetry among them (a hair under the session's), and no further", () => {
    expect(Math.fround(0.7), "the premise: the 32-bit number is under the session's").toBeLessThan(0.7);
    expect(envelopeDot(shape, Math.fround(0.7), false).x).toBeCloseTo(held, 12);
    expect(envelopeDot(shape, 0.7 + 5e-4, false).x).toBeCloseTo(held, 12);
    expect(envelopeDot(shape, 0.7 - 5e-4, false).x).toBeCloseTo(held, 12);
    // A little further is the decay above and the release below.
    expect(envelopeDot(shape, 0.7 + 2e-3, false).x).toBeLessThan(held);
    expect(envelopeDot(shape, 0.7 + 2e-3, false).x).toBeGreaterThan(shape.attackEnd);
    expect(envelopeDot(shape, 0.7 - 2e-3, false).x).toBeGreaterThan(held);
    expect(envelopeDot(shape, 0.7 - 2e-3, false).x).toBeGreaterThan(shape.holdEnd);
  });

  test("takes the top for what is in a hair of it, and no further: a little further down is the decay", () => {
    expect(envelopeDot(shape, 1 - 5e-4, false).x).toBe(shape.attackEnd);
    expect(envelopeDot(shape, 1 - 2e-3, false).x).toBeGreaterThan(shape.attackEnd);
    expect(envelopeDot(shape, 1 - 2e-3, false).x).toBeLessThan(shape.decayEnd);
  });

  test("is on the decay or the release by the level where the way is not known: above the sustain the decay, below it the release", () => {
    expect(envelopeDot(shape, 0.85, false).x).toBeGreaterThan(shape.attackEnd);
    expect(envelopeDot(shape, 0.85, false).x).toBeLessThan(shape.decayEnd);
    expect(envelopeDot(shape, 0.3, false).x).toBeGreaterThan(shape.holdEnd);
  });

  test("is in the hold at the top for a sustain of 1 whichever way the level goes (the attack ends in it, and there is no decay to be on), and finite", () => {
    const full = shapeOf({ ...NEW, sustain: 1 });
    for (const rising of [true, false]) {
      const dot = envelopeDot(full, 1, rising);
      expect(dot.x, String(rising)).toBeCloseTo((full.decayEnd + full.holdEnd) / 2, 12);
      expect(dot.y).toBe(1);
    }
    // On the way up to it, the attack; on the way down from it, the release.
    expect(envelopeDot(full, 0.6, true).x).toBeCloseTo(0.6 * full.attackEnd, 12);
    expect(envelopeDot(full, 0.6, false).x).toBeGreaterThan(full.holdEnd);
  });

  test("is on the decay all the way down for a sustain of 0, where there is no release to be on, and at the start at the end", () => {
    const none = shapeOf({ ...NEW, sustain: 0 });
    const dot = envelopeDot(none, 0.5, false);
    expect(dot.x).toBeGreaterThan(none.attackEnd);
    expect(dot.x).toBeLessThan(none.decayEnd);
    expect(envelopeDot(none, 0, false)).toEqual({ x: 0, y: 0 });
    expect(Number.isFinite(envelopeDot(none, 0.0005, false).x)).toBe(true);
  });

  test("never leaves the box, whatever the level: it is kept within 0 and 1, and a level that is not a number is at the start", () => {
    for (const rising of [true, false]) {
      for (const level of [1.7, -0.4, 0.5, 0.7, 1, Number.NaN]) {
        const dot = envelopeDot(shape, level, rising);
        expect(dot.x, `${String(level)} ${String(rising)}`).toBeGreaterThanOrEqual(0);
        expect(dot.x).toBeLessThanOrEqual(1);
        expect(dot.y).toBeGreaterThanOrEqual(0);
        expect(dot.y).toBeLessThanOrEqual(1);
      }
      expect(envelopeDot(shape, Number.NaN, rising)).toEqual({ x: 0, y: 0 });
    }
  });
});
