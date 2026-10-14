// What a trace is drawn as on a canvas (docs/web-ui.md §9.3): a line through the points of a plot, a dot at the newest, and, for a
// value that goes both ways, a mark across the middle. Only the context is touched, and a context of a test's own will do.
import type { Plot } from "../client/trace";

/** The part of a canvas's 2D context that a trace is drawn with, so that a test can give it a context of its own. */
export interface TraceContext {
  setTransform: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
  clearRect: (x: number, y: number, width: number, height: number) => void;
  beginPath: () => void;
  moveTo: (x: number, y: number) => void;
  lineTo: (x: number, y: number) => void;
  stroke: () => void;
  arc: (x: number, y: number, radius: number, start: number, end: number) => void;
  fill: () => void;
  strokeStyle: string | CanvasGradient | CanvasPattern;
  fillStyle: string | CanvasGradient | CanvasPattern;
  lineWidth: number;
  lineJoin: CanvasLineJoin;
  lineCap: CanvasLineCap;
}

/** The colours of a trace: the line and the dot, and the mark across the middle. */
export interface TraceStyle {
  line: string;
  centre: string;
}

/** The size of what is drawn on: in the pixels of the page, and how many pixels of the device each is. */
export interface TraceSize {
  width: number;
  height: number;
  scale: number;
}

const LINE_WIDTH = 1.5;
const HEAD_RADIUS = 2.5;
/** How far from the right edge the dot is held, so that the whole of it is drawn. */
const HEAD_INSET = 3;

/** Draw the points of `plot` (its first `count`) on `context`, over what there was, which is cleared. */
export function drawTrace(context: TraceContext, plot: Plot, size: TraceSize, centred: boolean, style: TraceStyle): void {
  const { width, height, scale } = size;
  context.setTransform(scale, 0, 0, scale, 0, 0);
  context.clearRect(0, 0, width, height);
  if (centred) {
    // (On the half pixel, so that it is one line and not two grey ones.)
    const middle = Math.round(height / 2) + 0.5;
    context.strokeStyle = style.centre;
    context.lineWidth = 1;
    context.beginPath();
    context.moveTo(0, middle);
    context.lineTo(width, middle);
    context.stroke();
  }
  const { points, count } = plot;
  if (count === 0) return;
  if (count > 1) {
    context.strokeStyle = style.line;
    context.lineWidth = LINE_WIDTH;
    context.lineJoin = "round";
    context.lineCap = "round";
    context.beginPath();
    context.moveTo(points[0] ?? 0, points[1] ?? 0);
    for (let point = 1; point < count; point += 1) context.lineTo(points[2 * point] ?? 0, points[2 * point + 1] ?? 0);
    context.stroke();
  }
  context.fillStyle = style.line;
  context.beginPath();
  context.arc(Math.min(points[2 * (count - 1)] ?? 0, width - HEAD_INSET), points[2 * (count - 1) + 1] ?? 0, HEAD_RADIUS, 0, 2 * Math.PI);
  context.fill();
}
