import { useEffect, useRef } from "react";

import { pageFrames } from "../client/frameloop";
import type { FrameLoop } from "../client/frameloop";
import { plotTrace, TraceBuffer } from "../client/trace";
import type { Plot } from "../client/trace";
import { formatValue } from "./Knob";
import { drawTrace } from "./trace-draw";
import type { TraceContext, TraceStyle } from "./trace-draw";
import "./LiveTrace.css";

/** How much of the past a trace shows, if it is not told. */
export const TRACE_SECONDS = 4;

/** How many samples it can hold: a display at 240 Hz fills a trace of four seconds with a thousand. */
const CAPACITY = 1024;

/** Room above and below the line, so that the line at the ends of the range is drawn whole. */
const PAD = 3;

export interface LiveTraceProps {
  label: string;
  /**
   * The value to draw, read on every frame; undefined when there is none. It does not make the trace render again: the
   * trace is drawn straight onto its canvas. Keep the function the same from one render to the next (the trace is
   * subscribed to it).
   */
  read: () => number | undefined;
  /** The values at the bottom and at the top of the trace. */
  min: number;
  max: number;
  /** For a value that goes both ways (a LFO's output): a mark across the middle. */
  centred?: boolean;
  /** How much of the past it shows, in seconds. */
  seconds?: number;
  /** How `aria-valuetext` says the value. */
  format?: (value: number) => string;
  frames?: FrameLoop;
}

/** The colours of a trace, from the tokens of the page. */
function styleOf(element: Element): TraceStyle {
  const css = getComputedStyle(element);
  return { line: css.getPropertyValue("--tang-modulation").trim() || "#d070d0", centre: css.getPropertyValue("--tang-border").trim() || "#353946" };
}

/**
 * A line that shows what a value has been for the last few seconds, as it goes by (a modulator's output: what a rate that another
 * modulator drives looks like), drawn on a canvas on every frame and not by React. It is a meter to what reads the page, as a
 * bar is. A value that has not moved for as long as the trace shows is drawn once: the picture is the same whenever.
 */
export function LiveTrace({ label, read, min, max, centred = false, seconds = TRACE_SECONDS, format = formatValue, frames = pageFrames }: LiveTraceProps) {
  const meter = useRef<HTMLDivElement>(null);
  const surface = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const element = meter.current;
    const canvas = surface.current;
    if (!element || !canvas) return;
    const context: TraceContext | null = canvas.getContext("2d");
    const style = styleOf(element);
    const span = seconds * 1000;
    const buffer = new TraceBuffer(CAPACITY);
    const plot: Plot = { points: new Float32Array(2 * 256), count: 0 };
    let last: number | undefined;
    // The size the bitmap was made at, in the pixels of the page and in how many pixels of the device each is.
    let width = 0;
    let height = 0;
    let ratio = 0;
    // Whether what was drawn is what will be drawn for as long as the value does not move (and which move it was after).
    let settled = false;
    let settledAfter: number | undefined;

    return frames.subscribe((time) => {
      const value = read();
      const known = value !== undefined && Number.isFinite(value);
      if (value !== last) {
        last = value;
        if (known) {
          element.setAttribute("aria-valuenow", String(value));
          element.setAttribute("aria-valuetext", format(value));
        } else {
          element.removeAttribute("aria-valuenow");
          element.removeAttribute("aria-valuetext");
          buffer.clear();
        }
      }
      if (known) buffer.push(time, value);

      const nowWidth = canvas.offsetWidth;
      const nowHeight = canvas.offsetHeight;
      const nowRatio = window.devicePixelRatio || 1;
      if (nowWidth !== width || nowHeight !== height || nowRatio !== ratio) {
        width = nowWidth;
        height = nowHeight;
        ratio = nowRatio;
        canvas.width = Math.round(width * ratio);
        canvas.height = Math.round(height * ratio);
        settled = false;
      }
      if (!context || width === 0 || height === 0) return;
      if (settled && buffer.changedAt === settledAfter) return;

      plotTrace(buffer, time, span, { width, height, min, max, pad: PAD }, plot);
      drawTrace(context, plot, { width, height, scale: ratio }, centred, style);
      settledAfter = buffer.changedAt;
      settled = settledAfter === undefined || time - settledAfter >= span;
    });
  }, [frames, read, min, max, centred, seconds, format]);

  return (
    <div ref={meter} className="tang-livetrace" role="meter" aria-label={label} aria-valuemin={min} aria-valuemax={max} data-centred={centred ? "" : undefined}>
      <canvas ref={surface} className="tang-livetrace__canvas" aria-hidden="true" />
    </div>
  );
}
