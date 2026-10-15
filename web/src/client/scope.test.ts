import { describe, expect, test } from "vitest";

import { plotSpectrum, plotWave, triggerStart } from "./scope";

/** `length` samples of a cosine that goes round once in `period` samples, from sample `from` on (half a sample later than that, so that none is exactly on the middle). */
function cosine(length: number, period: number, from = 0, amplitude = 1): Float32Array {
  return Float32Array.from({ length }, (_, n) => amplitude * Math.cos((2 * Math.PI * (n + from + 0.5)) / period));
}

describe("where a picture of the wave starts", () => {
  test("is where the wave goes up through the middle, the first time it does", () => {
    // A cosine that goes round in 100 samples (half a sample late) is under the middle from the 25th to the 75th: it goes up through at the 75th.
    const samples = cosine(2048, 100);
    const start = triggerStart(samples, 300);
    expect(start).toBe(75);
    expect(samples[start - 1]).toBeLessThan(0);
    expect(samples[start]).toBeGreaterThanOrEqual(0);
  });

  test("shows the same picture whatever the block began in, so that a wave stands still", () => {
    const pictures = [0, 17, 41, 93].map((from) => {
      const samples = cosine(2048, 100, from);
      const start = triggerStart(samples, 300);
      return Array.from(samples.slice(start, start + 100), (s) => Math.round(s * 1e4) / 1e4);
    });
    for (const picture of pictures) expect(picture).toEqual(pictures[0]);
  });

  test("is the latest picture that fits, with nothing to start from: silence, a constant, a wave that stays on one side", () => {
    expect(triggerStart(new Float32Array(2048), 300)).toBe(2048 - 300);
    expect(triggerStart(new Float32Array(2048).fill(0.5), 300)).toBe(2048 - 300);
    expect(triggerStart(new Float32Array(2048).fill(-0.5), 300)).toBe(2048 - 300);
    expect(triggerStart(cosine(2048, 100, 0, 0.4).map((s) => s + 1), 300)).toBe(2048 - 300);
  });

  test("leaves room for the whole picture: a crossing too near the end of the block is not one", () => {
    // Under the middle for 1000 samples and then over it, from the 1000th on: a picture of 1500 does not fit after it.
    const samples = Float32Array.from({ length: 2048 }, (_, n) => (n < 1000 ? -0.5 : 0.5));
    expect(triggerStart(samples, 1000)).toBe(1000);
    expect(triggerStart(samples, 1500)).toBe(2048 - 1500);
  });

  test("is the start of the block for a picture as long as the block, or longer", () => {
    expect(triggerStart(cosine(512, 100), 512)).toBe(0);
    expect(triggerStart(cosine(512, 100), 2000)).toBe(0);
  });

  test("does not take a wobble round the middle for a wave that goes up through it", () => {
    // A hum of 0.01 either way, which is under the 0.02 that the wave has to go below before it may go up through.
    const hum = Float32Array.from({ length: 2048 }, (_, n) => (n % 2 === 0 ? 0.01 : -0.01));
    expect(triggerStart(hum, 300)).toBe(2048 - 300);
    const louder = Float32Array.from({ length: 2048 }, (_, n) => (n % 2 === 0 ? 0.05 : -0.05));
    expect(triggerStart(louder, 300)).toBe(2);
  });

  test("can be where the wave goes up through another level", () => {
    const samples = cosine(2048, 100);
    const start = triggerStart(samples, 300, 0.5);
    expect(samples[start - 1]).toBeLessThan(0.5);
    expect(samples[start]).toBeGreaterThanOrEqual(0.5);
    expect(start).toBe(83);
  });
});

describe("the points of a wave", () => {
  const size = { width: 100, height: 40, pad: 4 };

  test("run from the left edge to the right one, a sample each, with +1 at the top and -1 at the bottom of the room that the padding leaves", () => {
    const out = new Float32Array(2 * 5);
    const count = plotWave(Float32Array.from([1, 0.5, 0, -0.5, -1]), 0, 5, size, out);
    expect(count).toBe(5);
    expect(Array.from(out)).toEqual([0, 4, 25, 12, 50, 20, 75, 28, 100, 36]);
  });

  test("are those of the picture that starts where it is told, and as long as it is told", () => {
    const out = new Float32Array(2 * 3);
    plotWave(Float32Array.from([9, 9, 0, 1, -1, 9]), 2, 3, size, out);
    expect(Array.from(out)).toEqual([0, 20, 50, 4, 100, 36]);
  });

  test("stop at the edges for what goes beyond full scale", () => {
    const out = new Float32Array(2 * 2);
    plotWave(Float32Array.from([3, -3]), 0, 2, size, out);
    expect(Array.from(out)).toEqual([0, 4, 100, 36]);
  });

  test("are no more than the picture that is asked for, or the block has", () => {
    const out = new Float32Array(2 * 10);
    expect(plotWave(new Float32Array(4), 2, 10, size, out)).toBe(2);
    expect(plotWave(new Float32Array(4), 0, 10, size, new Float32Array(2 * 3))).toBe(3);
    expect(plotWave(new Float32Array(4), 9, 3, size, out)).toBe(0);
  });

  test("are one point in the middle of the width for a picture of one sample", () => {
    const out = new Float32Array(2);
    expect(plotWave(Float32Array.from([0]), 0, 1, size, out)).toBe(1);
    expect(Array.from(out)).toEqual([50, 20]);
  });
});

describe("the points of a spectrum", () => {
  const size = { width: 100, height: 50, pad: 0 };
  const range = { minHz: 20, maxHz: 20_000, floorDb: -90, topDb: 0 };
  const sampleRate = 48_000;
  /** The levels of a block of 2048 samples (a bin is 23.4 Hz): at the lowest there is, but for the given bins. */
  function floorWith(levels: Record<number, number>): Float32Array {
    const db = new Float32Array(1025).fill(-120);
    for (const [bin, level] of Object.entries(levels)) db[Number(bin)] = level;
    return db;
  }
  const plot = (db: Float32Array): { count: number; points: Float32Array } => {
    const points = new Float32Array(2 * 101);
    return { count: plotSpectrum(db, sampleRate, size, range, points), points };
  };
  const yAt = (points: Float32Array, column: number): number => points[2 * column + 1] ?? Number.NaN;

  test("have a point for each pixel across, from the left edge to the right one, at the bottom where there is nothing", () => {
    const { count, points } = plot(floorWith({}));
    expect(count).toBe(101);
    expect([points[0], points[2 * 100]]).toEqual([0, 100]);
    for (let column = 0; column < count; column += 1) expect(yAt(points, column)).toBe(50);
  });

  test("have the frequencies spread over the decades, as many pixels to each, from the lowest to the highest", () => {
    // A level at 1 kHz (between the bins 42 and 43) is 100 * ln(1000 / 20) / ln(20000 / 20) = 56.6 of the way across: the column of 1026 Hz is the 57th.
    // The level halfway between the floor and the top is halfway up.
    const { points } = plot(floorWith({ 42: -45, 43: -45 }));
    expect(yAt(points, 57)).toBeCloseTo(25, 5);
    expect(yAt(points, 20)).toBe(50);
    expect(yAt(points, 90)).toBe(50);
  });

  test("have a level between the two bins that it is between, where a pixel is narrower than a bin", () => {
    // The lowest pixels are a few hertz wide, and a bin 23.4: the line goes from one bin's level to the next's.
    const { points } = plot(floorWith({ 1: -90, 2: 0 }));
    const lows = Array.from({ length: 30 }, (_, column) => yAt(points, column));
    // (Column 0 is 20 Hz, which is nearly the bin 1 (23.4 Hz); the bin 2 is 46.9 Hz, near column 14.)
    expect(lows[0]).toBeCloseTo(50, 0);
    // Column 8 is 34.8 Hz, which is 0.48 of the way from the bin 1 (-90) to the bin 2 (0): 46.5 under the top, which is 25.8 down of the 50.
    expect(lows[8]).toBeCloseTo(25.8, 0);
    expect(Math.min(...lows)).toBeLessThan(5);
    const lowest = lows.indexOf(Math.min(...lows));
    for (let column = 1; column <= lowest; column += 1) expect(lows[column] ?? 99).toBeLessThanOrEqual((lows[column - 1] ?? 0) + 1e-6);
  });

  test("show the highest level of the frequencies that a pixel holds where it holds many, so that a peak is not hidden", () => {
    // The bin 700 (16.4 kHz) is near the right edge, where a pixel holds twenty bins.
    const { points } = plot(floorWith({ 700: 0 }));
    expect(Math.min(...Array.from({ length: 101 }, (_, column) => yAt(points, column)))).toBe(0);
  });

  test("have each level in one pixel, the one that its frequency is in: a peak of one bin is not hidden and not drawn twice", () => {
    const shown = (db: Float32Array): number[] => {
      const { points } = plot(db);
      return Array.from({ length: 101 }, (_, column) => column).filter((column) => yAt(points, column) < 50);
    };
    // A level between two bins widens to the bins beside it, and a pixel of the low end is narrower than a bin: from the bin where a pixel holds
    // more than one, 43 (1 kHz, a pixel is 70 Hz there), a bin is in one pixel and only that.
    for (let bin = 43; bin <= 700; bin += 1) {
      expect(shown(floorWith({ [bin]: -45 })), `the bin ${String(bin)}`).toHaveLength(1);
    }
  });

  test("are kept between the bottom and the top for levels beyond the floor and the top level", () => {
    const { points } = plot(floorWith({ 43: 20, 100: -200 }));
    for (let column = 0; column < 101; column += 1) {
      expect(yAt(points, column)).toBeGreaterThanOrEqual(0);
      expect(yAt(points, column)).toBeLessThanOrEqual(50);
    }
    expect(Math.min(...Array.from({ length: 101 }, (_, column) => yAt(points, column)))).toBe(0);
  });

  test("are no more than the room for them", () => {
    expect(plotSpectrum(floorWith({}), sampleRate, size, range, new Float32Array(2 * 10))).toBe(10);
  });
});
