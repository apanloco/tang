import { useEffect, useMemo, useRef } from "react";

import { envelopeAreaPath, envelopeDot, envelopePath, placeIn, shapeOf } from "../client/envelope";
import type { Box, Stages } from "../client/envelope";
import { pageFrames } from "../client/frameloop";
import type { FrameLoop } from "../client/frameloop";
import "./LiveEnvelope.css";

/** The size of the drawing in its own units, which the page scales to the room it has (the aspect is kept, so the dot stays round). */
export const ENVELOPE_WIDTH = 272;
export const ENVELOPE_HEIGHT = 56;
/** The margin that keeps the line and the dot whole at the ends of the shape. */
export const ENVELOPE_PAD = 5;

const BOX: Box = { width: ENVELOPE_WIDTH, height: ENVELOPE_HEIGHT, pad: ENVELOPE_PAD };

export interface LiveEnvelopeProps {
  /** What the drawing is, in words, with the settings it is drawn from (it is an image to what reads the page). */
  label: string;
  /** The settings it is drawn from: it is drawn again when one changes. */
  stages: Stages;
  /**
   * The level the envelope puts out, read on every frame; undefined when there is none. It does not make the drawing render again: the dot is
   * moved straight in the DOM. Keep the function the same from one render to the next (the dot is subscribed to it).
   */
  read: () => number | undefined;
  frames?: FrameLoop;
}

/**
 * The shape an ADSR envelope makes of its four settings, a line from nothing up to the top, down to the sustain, along it and down to nothing,
 * and a dot on it where the envelope is while it runs (docs/web-ui.md §9.3). The line is drawn by React from the settings, which change when a
 * person turns a knob; the dot is not: it is moved on the frames its level moves in (`envelopeDot` says which stage that is from which way the
 * level was going), and is hidden while there is no level to follow.
 */
export function LiveEnvelope({ label, stages, read, frames = pageFrames }: LiveEnvelopeProps) {
  const dot = useRef<SVGCircleElement>(null);
  // The level of the frame before: which way the level goes says which stage the dot is on, and a change of the settings does not forget it.
  const previous = useRef<number | undefined>(undefined);
  const { attack, decay, sustain, release } = stages;
  const shape = useMemo(() => shapeOf({ attack, decay, sustain, release }), [attack, decay, sustain, release]);

  useEffect(() => {
    const element = dot.current;
    if (!element) return;
    // What the dot shows now: whether it is hidden, and whether it was placed with the level standing still, so that a frame that finds the level where
    // it was has nothing to write (a write is a change of the DOM, and a dot that is written on every frame is one that is written about for ever).
    let hidden = element.getAttribute("visibility") !== "visible";
    let settled = false;
    return frames.subscribe(() => {
      const level = read();
      if (level === undefined || !Number.isFinite(level)) {
        if (!hidden) element.setAttribute("visibility", "hidden");
        hidden = true;
        previous.current = undefined;
        settled = false;
        return;
      }
      if (settled && level === previous.current) return;
      const was = previous.current ?? level;
      settled = level === was;
      previous.current = level;
      const on = envelopeDot(shape, level, was);
      const place = placeIn(BOX, on.x, on.y);
      element.setAttribute("cx", String(place.x));
      element.setAttribute("cy", String(place.y));
      if (hidden) element.setAttribute("visibility", "visible");
      hidden = false;
    });
  }, [frames, read, shape]);

  return (
    <svg className="tang-envelope" viewBox={`0 0 ${String(ENVELOPE_WIDTH)} ${String(ENVELOPE_HEIGHT)}`} role="img" aria-label={label}>
      <path className="tang-envelope__fill" d={envelopeAreaPath(shape, BOX)} />
      <path className="tang-envelope__line" d={envelopePath(shape, BOX)} />
      <circle ref={dot} className="tang-envelope__dot" r="3.2" visibility="hidden" aria-hidden="true" />
    </svg>
  );
}
