import { describe, expect, test } from "vitest";

import type { Plot } from "../client/trace";
import { drawSpectrum, drawWave } from "./scope-draw";
import type { ScopeContext, ScopeStyle } from "./scope-draw";
import { callsOf, recordingContext } from "./testing";

const style: ScopeStyle = { line: "#7aa2f7", second: "#5fc86f", grid: "#353946", fill: "rgba(122, 162, 247, 0.18)" };
const size = { width: 200, height: 100, pad: 4, scale: 1 };

/** A plot of points, as `plotWave` and `plotSpectrum` make it. */
const plotOf = (points: readonly (readonly [number, number])[]): Plot => ({ points: Float32Array.from(points.flat()), count: points.length });

function wave(left: Plot, right: Plot | null = null, scale = 1) {
  const context = recordingContext();
  drawWave(context as unknown as ScopeContext, left, right, { ...size, scale }, style);
  return context;
}

function spectrum(plot: Plot, grid = { columns: [50, 100, 150], rows: [25, 50, 75] }) {
  const context = recordingContext();
  drawSpectrum(context as unknown as ScopeContext, plot, size, grid, style);
  return context;
}

/** The colour that was set to stroke with before each stroke, in order. */
function strokes(context: ReturnType<typeof recordingContext>): unknown[] {
  const colours: unknown[] = [];
  let colour: unknown;
  for (const [name, value] of context.calls) {
    if (name === "strokeStyle") colour = value;
    if (name === "stroke") colours.push(colour);
  }
  return colours;
}

describe("drawing a wave", () => {
  test("begins by clearing what there was, in the pixels of the plot whatever the ratio of the device's", () => {
    const [first, second] = wave(plotOf([[0, 50]]), null, 2).calls;
    expect(first).toEqual(["setTransform", 2, 0, 0, 2, 0, 0]);
    expect(second).toEqual(["clearRect", 0, 0, 200, 100]);
  });

  test("draws a line across the middle and one half way to each end first, on the half pixel, in the colour of the grid", () => {
    const context = wave(plotOf([[0, 50], [200, 50]]));
    // The room is from 4 to 96, the middle 50, and half way 27 and 73.
    expect(callsOf(context, "moveTo").slice(0, 3)).toEqual([[0, 50.5], [0, 27.5], [0, 73.5]]);
    expect(callsOf(context, "lineTo").slice(0, 3)).toEqual([[200, 50.5], [200, 27.5], [200, 73.5]]);
    expect(strokes(wave(plotOf([[0, 50], [200, 50]])))[0]).toBe(style.grid);
  });

  test("draws the line from the first point to the last, in the colour of the line, round", () => {
    const context = wave(plotOf([[0, 30], [100, 10], [200, 20]]));
    expect(callsOf(context, "moveTo").at(-1)).toEqual([0, 30]);
    expect(callsOf(context, "lineTo").slice(-2)).toEqual([[100, 10], [200, 20]]);
    expect(strokes(context).at(-1)).toBe(style.line);
    expect(context.calls.find((call) => call[0] === "lineJoin")).toEqual(["lineJoin", "round"]);
  });

  test("draws the right side first and the left over it, in a colour of its own, so that a sound that is the same on both is seen as the left", () => {
    const context = wave(plotOf([[0, 30], [200, 20]]), plotOf([[0, 70], [200, 60]]));
    expect(strokes(context)).toEqual([style.grid, style.grid, style.grid, style.second, style.line]);
    expect(callsOf(context, "moveTo").slice(-2)).toEqual([[0, 70], [0, 30]]);
  });

  test("draws no line for a plot of fewer than two points, and the grid still", () => {
    expect(strokes(wave(plotOf([[10, 20]])))).toEqual([style.grid, style.grid, style.grid]);
    expect(strokes(wave(plotOf([])))).toEqual([style.grid, style.grid, style.grid]);
  });
});

describe("drawing a spectrum", () => {
  test("begins by clearing what there was", () => {
    const [first, second] = spectrum(plotOf([[0, 90]])).calls;
    expect(first).toEqual(["setTransform", 1, 0, 0, 1, 0, 0]);
    expect(second).toEqual(["clearRect", 0, 0, 200, 100]);
  });

  test("draws the lines of the grid down and across first, on the half pixel, in the colour of the grid", () => {
    const context = spectrum(plotOf([[0, 90], [200, 90]]));
    expect(callsOf(context, "moveTo").slice(0, 6)).toEqual([[50.5, 0], [100.5, 0], [150.5, 0], [0, 25.5], [0, 50.5], [0, 75.5]]);
    expect(callsOf(context, "lineTo").slice(0, 6)).toEqual([[50.5, 100], [100.5, 100], [150.5, 100], [200, 25.5], [200, 50.5], [200, 75.5]]);
    expect(strokes(context).slice(0, 6)).toEqual(Array<string>(6).fill(style.grid));
  });

  test("fills the area under the line, down to the bottom of the room, and then draws the line over it", () => {
    const context = spectrum(plotOf([[0, 90], [100, 20], [200, 60]]));
    const names = context.calls.map((call) => call[0]);
    expect(callsOf(context, "closePath")).toHaveLength(1);
    expect(callsOf(context, "fill")).toHaveLength(1);
    expect(context.calls.filter((call) => call[0] === "fillStyle")).toEqual([["fillStyle", style.fill]]);
    // The area is from the left edge at the bottom (96), along the points, to the right edge at the bottom.
    const moves = callsOf(context, "moveTo");
    expect(moves.at(-2)).toEqual([0, 96]);
    const lines = callsOf(context, "lineTo");
    expect(lines.slice(-6, -2)).toEqual([[0, 90], [100, 20], [200, 60], [200, 96]]);
    expect(names.lastIndexOf("fill")).toBeLessThan(names.lastIndexOf("stroke"));
    expect(strokes(context).at(-1)).toBe(style.line);
    expect(moves.at(-1)).toEqual([0, 90]);
    expect(lines.slice(-2)).toEqual([[100, 20], [200, 60]]);
  });

  test("draws no area and no line for a plot of fewer than two points, and the grid still", () => {
    for (const plot of [plotOf([[10, 20]]), plotOf([])]) {
      const context = spectrum(plot);
      expect(callsOf(context, "fill")).toHaveLength(0);
      expect(strokes(context)).toEqual(Array<string>(6).fill(style.grid));
    }
  });
});
