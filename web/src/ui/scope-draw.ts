// What the scope is drawn as on a canvas (docs/web-ui.md §9.3): a graticule and one or two lines through the points of a plot, and for a spectrum
// the area under the line. Only the context is touched, and a context of a test's own will do.
import type { Plot } from "../client/trace";

/** The part of a canvas's 2D context that the scope is drawn with, so that a test can give it a context of its own. */
export interface ScopeContext {
  setTransform: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
  clearRect: (x: number, y: number, width: number, height: number) => void;
  beginPath: () => void;
  moveTo: (x: number, y: number) => void;
  lineTo: (x: number, y: number) => void;
  closePath: () => void;
  stroke: () => void;
  fill: () => void;
  strokeStyle: string | CanvasGradient | CanvasPattern;
  fillStyle: string | CanvasGradient | CanvasPattern;
  lineWidth: number;
  lineJoin: CanvasLineJoin;
  lineCap: CanvasLineCap;
}

/** The colours of a scope: the line (the left side, or the spectrum), the right side, the lines of the graticule and the area under a spectrum. */
export interface ScopeStyle {
  line: string;
  second: string;
  grid: string;
  fill: string;
}

/** The size of what is drawn on, in the pixels of the page, how many pixels of the device each is, and the room that the line keeps clear of the top and bottom. */
export interface ScopeSize {
  width: number;
  height: number;
  pad: number;
  scale: number;
}

const LINE_WIDTH = 1.5;

/** A line from `(x1, y1)` to `(x2, y2)` on the half pixel, so that it is one line and not two grey ones. */
function gridLine(context: ScopeContext, x1: number, y1: number, x2: number, y2: number): void {
  context.beginPath();
  context.moveTo(x1, y1);
  context.lineTo(x2, y2);
  context.stroke();
}

function clear(context: ScopeContext, size: ScopeSize): void {
  context.setTransform(size.scale, 0, 0, size.scale, 0, 0);
  context.clearRect(0, 0, size.width, size.height);
}

function pathOf(context: ScopeContext, plot: Plot): void {
  const { points, count } = plot;
  context.beginPath();
  context.moveTo(points[0] ?? 0, points[1] ?? 0);
  for (let point = 1; point < count; point += 1) context.lineTo(points[2 * point] ?? 0, points[2 * point + 1] ?? 0);
}

function strokeLine(context: ScopeContext, plot: Plot, colour: string): void {
  if (plot.count < 2) return;
  context.strokeStyle = colour;
  context.lineWidth = LINE_WIDTH;
  context.lineJoin = "round";
  context.lineCap = "round";
  pathOf(context, plot);
  context.stroke();
}

/**
 * Draw the wave of `left`, and of `right` under it if there is one, over what there was, which is cleared: a line across the middle and one half way to each
 * end first. The right side is drawn first, so that a sound that is the same on both sides is seen as the left, and a difference shows as the other colour.
 */
export function drawWave(context: ScopeContext, left: Plot, right: Plot | null, size: ScopeSize, style: ScopeStyle): void {
  clear(context, size);
  const room = size.height - 2 * size.pad;
  context.strokeStyle = style.grid;
  context.lineWidth = 1;
  for (const at of [size.pad + room / 2, size.pad + room / 4, size.pad + (3 * room) / 4]) {
    const y = Math.round(at) + 0.5;
    gridLine(context, 0, y, size.width, y);
  }
  if (right) strokeLine(context, right, style.second);
  strokeLine(context, left, style.line);
}

/** Where the lines of a spectrum's graticule are, in pixels: the columns (one for each decade) and the rows (one for each level). */
export interface SpectrumGrid {
  columns: readonly number[];
  rows: readonly number[];
}

/** Draw a spectrum over what there was, which is cleared: the lines of `grid`, then the area under the line, which is filled, and the line over it. */
export function drawSpectrum(context: ScopeContext, plot: Plot, size: ScopeSize, grid: SpectrumGrid, style: ScopeStyle): void {
  clear(context, size);
  context.strokeStyle = style.grid;
  context.lineWidth = 1;
  for (const column of grid.columns) {
    const x = Math.round(column) + 0.5;
    gridLine(context, x, 0, x, size.height);
  }
  for (const row of grid.rows) {
    const y = Math.round(row) + 0.5;
    gridLine(context, 0, y, size.width, y);
  }
  if (plot.count < 2) return;
  const bottom = size.height - size.pad;
  context.fillStyle = style.fill;
  context.beginPath();
  context.moveTo(plot.points[0] ?? 0, bottom);
  for (let point = 0; point < plot.count; point += 1) context.lineTo(plot.points[2 * point] ?? 0, plot.points[2 * point + 1] ?? 0);
  context.lineTo(plot.points[2 * (plot.count - 1)] ?? 0, bottom);
  context.closePath();
  context.fill();
  strokeLine(context, plot, style.line);
}
