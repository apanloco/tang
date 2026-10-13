import { useEffect, useRef } from "react";

import { pageFrames } from "../client/frameloop";
import type { FrameLoop } from "../client/frameloop";
import "./ClipLight.css";

export interface ClipLightProps {
  /**
   * How many buffers have clipped, ever: a count that goes up, read on every frame. Keep
   * the function the same from one render to the next (the light is subscribed to it).
   */
  readClips: () => number | undefined;
  /** How long the light stays on after the count last went up, in milliseconds. */
  holdMs?: number;
  frames?: FrameLoop;
}

/** The CLIP light of a meter: on for a while after the count of clipped buffers goes up, drawn without React. */
export function ClipLight({ readClips, holdMs = 2000, frames = pageFrames }: ClipLightProps) {
  const light = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    let last: number | undefined;
    let litUntil = Number.NEGATIVE_INFINITY;
    let lit = false;
    return frames.subscribe((time) => {
      const clips = readClips();
      if (clips !== undefined) {
        // The first reading is where the count is, not news (a page opened after a clip).
        if (last !== undefined && clips > last) litUntil = time + holdMs;
        last = clips;
      }
      const now = time < litUntil;
      if (now !== lit) {
        lit = now;
        light.current?.toggleAttribute("data-lit", now);
      }
    });
  }, [frames, readClips, holdMs]);

  return (
    <span ref={light} className="tang-clip" role="img" aria-label="Clip indicator">
      CLIP
    </span>
  );
}
