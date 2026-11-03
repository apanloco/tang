import { describe, expect, test } from "vitest";

import { plotTrace, TraceBuffer } from "./trace";
import type { Plot } from "./trace";

/** A buffer holding the samples `[time, value]`, in order. */
function bufferOf(capacity: number, samples: readonly (readonly [number, number])[]): TraceBuffer {
  const buffer = new TraceBuffer(capacity);
  for (const [time, value] of samples) buffer.push(time, value);
  return buffer;
}

/** What a buffer holds, oldest first. */
function held(buffer: TraceBuffer): [number, number][] {
  const seen: [number, number][] = [];
  buffer.forEach((time, value) => {
    seen.push([time, value]);
  });
  return seen;
}

describe("the recent past of a value", () => {
  test("holds what was pushed, oldest first, with the time of each", () => {
    const buffer = bufferOf(8, [
      [0, 0.5],
      [16, 0.25],
      [33, -1],
    ]);
    expect(buffer.length).toBe(3);
    expect(held(buffer)).toEqual([
      [0, 0.5],
      [16, 0.25],
      [33, -1],
    ]);
  });

  test("keeps the last `capacity` samples and lets the oldest go, however many come", () => {
    const buffer = new TraceBuffer(4);
    for (let i = 0; i < 11; i += 1) buffer.push(i * 10, i);
    expect(buffer.length).toBe(4);
    expect(held(buffer)).toEqual([
      [70, 7],
      [80, 8],
      [90, 9],
      [100, 10],
    ]);
  });

  test("holds a sample as a 32-bit number (that is all the telemetry has), and its time as it was given", () => {
    const buffer = bufferOf(4, [[1234.5678, 0.1]]);
    const [only] = held(buffer);
    expect(only?.[0]).toBe(1234.5678);
    expect(only?.[1]).toBe(Math.fround(0.1));
  });

  test("takes no sample that is not a number", () => {
    const buffer = new TraceBuffer(4);
    buffer.push(0, Number.NaN);
    buffer.push(1, Number.POSITIVE_INFINITY);
    buffer.push(2, Number.NEGATIVE_INFINITY);
    buffer.push(Number.NaN, 1);
    buffer.push(Number.POSITIVE_INFINITY, 1);
    expect(buffer.length).toBe(0);
    buffer.push(3, 1);
    expect(held(buffer)).toEqual([[3, 1]]);
  });

  test("can be emptied, and is then as new", () => {
    const buffer = bufferOf(4, [
      [0, 1],
      [1, 2],
    ]);
    buffer.clear();
    expect(buffer.length).toBe(0);
    expect(held(buffer)).toEqual([]);
    expect(buffer.changedAt).toBeUndefined();
    buffer.push(5, 3);
    expect(held(buffer)).toEqual([[5, 3]]);
  });

  test("has room for at least two samples, which a line needs", () => {
    expect(() => new TraceBuffer(1)).toThrow();
    expect(() => new TraceBuffer(0)).toThrow();
    expect(() => new TraceBuffer(2.5)).toThrow();
    expect(new TraceBuffer(2).length).toBe(0);
  });

  test("says when the value last changed: the time of the first of the samples it has had since", () => {
    const buffer = new TraceBuffer(16);
    expect(buffer.changedAt).toBeUndefined();
    buffer.push(10, 0.5);
    expect(buffer.changedAt).toBe(10);
    buffer.push(20, 0.5);
    buffer.push(30, 0.5);
    expect(buffer.changedAt).toBe(10);
    buffer.push(40, 0.75);
    expect(buffer.changedAt).toBe(40);
    buffer.push(50, 0.75);
    expect(buffer.changedAt).toBe(40);
    buffer.push(60, 0.5);
    expect(buffer.changedAt).toBe(60);
  });

  test("says a value that is not a 32-bit number has not changed when it is pushed again: it is held as one", () => {
    const buffer = new TraceBuffer(8);
    buffer.push(10, 0.1);
    buffer.push(20, 0.1);
    buffer.push(30, 0.1);
    expect(buffer.changedAt).toBe(10);
  });

  test("says when the value last changed even after the samples before have gone", () => {
    const buffer = new TraceBuffer(3);
    buffer.push(0, 1);
    for (let i = 1; i <= 10; i += 1) buffer.push(i * 10, 2);
    expect(buffer.changedAt).toBe(10);
    expect(buffer.length).toBe(3);
  });
});

describe("where the samples of a trace are drawn", () => {
  const box = { width: 200, height: 40, min: -1, max: 1, pad: 2 };
  const SPAN = 1000;
  /** Plot `buffer` at `now`, and read what was drawn as points. */
  function drawn(buffer: TraceBuffer, now: number, options: Partial<typeof box> & { span?: number } = {}): { x: number; y: number }[] {
    const { span = SPAN, ...rest } = options;
    const out: Plot = { points: new Float32Array(2 * 64), count: 0 };
    plotTrace(buffer, now, span, { ...box, ...rest }, out);
    return Array.from({ length: out.count }, (_, i) => ({ x: out.points[2 * i] ?? Number.NaN, y: out.points[2 * i + 1] ?? Number.NaN }));
  }

  test("has the newest at the right edge and the rest to the left by how old each is, the whole span being the width", () => {
    const buffer = bufferOf(16, [
      [0, 0],
      [500, 0],
      [750, 0],
      [1000, 0],
    ]);
    // (At time 1000 a span of 1000 ms is 200 px: 5 ms a pixel.)
    expect(drawn(buffer, 1000).map((p) => p.x)).toEqual([0, 100, 150, 200]);
    // A moment later everything has gone left by as much, and the last value is held to the right edge.
    expect(drawn(buffer, 1100).map((p) => p.x)).toEqual([-20, 80, 130, 180, 200]);
  });

  test("puts the least value at the bottom and the most at the top, inside the padding, and the middle in the middle", () => {
    const buffer = bufferOf(16, [
      [900, -1],
      [950, 0],
      [1000, 1],
    ]);
    expect(drawn(buffer, 1000).map((p) => p.y)).toEqual([38, 20, 2]);
  });

  test("keeps a value beyond the range at its edge", () => {
    const buffer = bufferOf(16, [
      [900, -5],
      [1000, 7],
    ]);
    expect(drawn(buffer, 1000).map((p) => p.y)).toEqual([38, 2]);
  });

  test("goes by the range it is given: 0 to 1 has its middle at a half", () => {
    const buffer = bufferOf(16, [
      [900, 0],
      [950, 0.5],
      [1000, 1],
    ]);
    expect(drawn(buffer, 1000, { min: 0, max: 1 }).map((p) => p.y)).toEqual([38, 20, 2]);
  });

  test("is the middle of the plot all through when the range is nothing", () => {
    const buffer = bufferOf(16, [
      [900, 3],
      [1000, 3],
    ]);
    expect(drawn(buffer, 1000, { min: 3, max: 3 }).map((p) => p.y)).toEqual([20, 20]);
  });

  test("starts at the sample that is the last before the span, so that the line comes in from the left edge, and no earlier", () => {
    const buffer = bufferOf(16, [
      [-500, 1],
      [-100, -1],
      [200, 0],
      [600, 0],
      [1000, 0],
    ]);
    // The span runs from 0 to 1000. -100 is the one the line comes in from; -500 is not drawn.
    expect(drawn(buffer, 1000).map((p) => p.x)).toEqual([-20, 40, 120, 200]);
    expect(drawn(buffer, 1000).map((p) => p.y)).toEqual([38, 20, 20, 20]);
  });

  test("holds the newest value to the right edge when it is older than now, and not when it is now", () => {
    const buffer = bufferOf(16, [
      [100, 1],
      [200, -1],
    ]);
    const points = drawn(buffer, 400);
    expect(points.map((p) => p.x)).toEqual([140, 160, 200]);
    expect(points.map((p) => p.y)).toEqual([2, 38, 38]);
    expect(drawn(buffer, 200).map((p) => p.x)).toEqual([180, 200]);
  });

  test("draws nothing when there is no sample, and a single sample as a dot at the right edge", () => {
    expect(drawn(new TraceBuffer(4), 1000)).toEqual([]);
    expect(drawn(bufferOf(4, [[1000, 0.5]]), 1000)).toEqual([{ x: 200, y: 11 }]);
  });

  test("draws nothing of a trace whose newest sample is older than the span, but for the held value across the plot", () => {
    const buffer = bufferOf(4, [[0, 0]]);
    expect(drawn(buffer, 5000).map((p) => p.x)).toEqual([-800, 200]);
  });

  test("reuses the array it is given when it is big enough, and grows it when it is not", () => {
    const buffer = new TraceBuffer(256);
    for (let i = 0; i < 200; i += 1) buffer.push(i * 5, Math.sin(i / 10));
    const small: Plot = { points: new Float32Array(4), count: 0 };
    plotTrace(buffer, 995, 1000, box, small);
    expect(small.count).toBeGreaterThan(100);
    expect(small.points.length).toBeGreaterThanOrEqual(2 * small.count);
    const kept = small.points;
    plotTrace(buffer, 996, 1000, box, small);
    expect(small.points).toBe(kept);
    // (The last point, the newest value held to the right edge, is in the array that grew, and not only counted.)
    const grown: Plot = { points: new Float32Array(2), count: 0 };
    plotTrace(buffer, 1200, 1000, box, grown);
    expect(grown.points[2 * (grown.count - 1)]).toBe(200);
    expect(grown.points[2 * (grown.count - 1) + 1]).toBeCloseTo(drawn(buffer, 1200).at(-1)?.y ?? Number.NaN, 4);
  });

  test("draws nothing for a span that is not a time", () => {
    const buffer = bufferOf(4, [[100, 0.5]]);
    for (const span of [0, -1000, Number.NaN]) expect(drawn(buffer, 100, { span }), `span ${String(span)}`).toEqual([]);
  });
});
