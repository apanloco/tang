import { describe, expect, test } from "vitest";

import type { Plot } from "../client/trace";
import { drawTrace } from "./trace-draw";
import type { TraceContext, TraceStyle } from "./trace-draw";
import { callsOf, recordingContext } from "./testing";

const style: TraceStyle = { line: "#d070d0", centre: "#353946" };
const size = { width: 200, height: 40, scale: 1 };

/** A plot of points, as the plot of a trace makes it. */
const plotOf = (points: readonly (readonly [number, number])[]): Plot => ({ points: Float32Array.from(points.flat()), count: points.length });

function draw(plot: Plot, options: { centred?: boolean; scale?: number } = {}) {
  const context = recordingContext();
  drawTrace(context as unknown as TraceContext, plot, { ...size, scale: options.scale ?? 1 }, options.centred ?? false, style);
  return context;
}

describe("drawing a trace", () => {
  test("begins by clearing what there was, in the pixels of the plot whatever the ratio of the device's", () => {
    const [first, second] = draw(plotOf([[10, 20]]), { scale: 2 }).calls;
    expect(first).toEqual(["setTransform", 2, 0, 0, 2, 0, 0]);
    expect(second).toEqual(["clearRect", 0, 0, 200, 40]);
  });

  test("draws the line from the first point to the last, in the colour of the line", () => {
    const context = draw(plotOf([[0, 30], [100, 10], [200, 20]]));
    expect(callsOf(context, "moveTo")).toEqual([[0, 30]]);
    expect(callsOf(context, "lineTo")).toEqual([[100, 10], [200, 20]]);
    // (Set before the stroke that it is for.)
    const names = context.calls.map((call) => call[0]);
    expect(names.indexOf("strokeStyle")).toBeLessThan(names.indexOf("stroke"));
    expect(context.calls.find((call) => call[0] === "strokeStyle")).toEqual(["strokeStyle", style.line]);
    expect(context.calls.find((call) => call[0] === "lineWidth")).toEqual(["lineWidth", 1.5]);
    expect(callsOf(context, "stroke")).toHaveLength(1);
    // Round, so that a line that turns sharply (a square wave) is not spiked.
    expect(context.calls.find((call) => call[0] === "lineJoin")).toEqual(["lineJoin", "round"]);
    expect(context.calls.find((call) => call[0] === "lineCap")).toEqual(["lineCap", "round"]);
  });

  test("draws a dot at the newest point, held inside the right edge so that it is not cut", () => {
    const context = draw(plotOf([[0, 30], [100, 10], [200, 20]]));
    const [x, y, radius, from, to] = callsOf(context, "arc")[0] ?? [];
    expect(x).toBe(197);
    expect(y).toBe(20);
    expect(radius).toBe(2.5);
    expect([from, to]).toEqual([0, 2 * Math.PI]);
    expect(callsOf(context, "fill")).toHaveLength(1);
    // In the colour of the line.
    expect(context.calls.filter((call) => call[0] === "fillStyle")).toEqual([["fillStyle", style.line]]);
  });

  test("keeps the dot where the newest point is when that is inside", () => {
    const [x] = callsOf(draw(plotOf([[0, 30], [120, 20]])), "arc")[0] ?? [];
    expect(x).toBe(120);
  });

  test("draws a mark across the middle first, in its own colour, when the trace goes both ways, and not when it does not", () => {
    const centred = draw(plotOf([[0, 30], [200, 20]]), { centred: true });
    const moves = callsOf(centred, "moveTo");
    expect(moves[0]).toEqual([0, 20.5]);
    expect(callsOf(centred, "lineTo")[0]).toEqual([200, 20.5]);
    expect(callsOf(centred, "stroke")).toHaveLength(2);
    const styles = centred.calls.filter((call) => call[0] === "strokeStyle").map((call) => call[1]);
    expect(styles).toEqual([style.centre, style.line]);
    expect(callsOf(draw(plotOf([[0, 30], [200, 20]])), "stroke")).toHaveLength(1);
  });

  test("draws a single point as a dot and no line", () => {
    const context = draw(plotOf([[200, 11]]));
    expect(callsOf(context, "moveTo")).toEqual([]);
    expect(callsOf(context, "stroke")).toEqual([]);
    expect(callsOf(context, "arc")).toHaveLength(1);
  });

  test("draws nothing but the clearing, and the mark if it is centred, when there is no point", () => {
    expect(draw(plotOf([])).calls.map((call) => call[0])).toEqual(["setTransform", "clearRect"]);
    const centred = draw(plotOf([]), { centred: true });
    expect(centred.calls.map((call) => call[0])).toEqual(["setTransform", "clearRect", "strokeStyle", "lineWidth", "beginPath", "moveTo", "lineTo", "stroke"]);
  });

  test("reads only as many points as the plot says it has: what is after them in the array is left from an earlier plot", () => {
    const plot: Plot = { points: Float32Array.from([0, 30, 100, 10, 7, 7, 8, 8]), count: 2 };
    const context = draw(plot);
    expect(callsOf(context, "lineTo")).toEqual([[100, 10]]);
  });
});
