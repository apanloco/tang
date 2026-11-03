import { useCallback } from "react";

import { pageFrames } from "../client/frameloop";
import type { FrameLoop } from "../client/frameloop";
import { ClipLight } from "./ClipLight";
import { LiveBar } from "./LiveBar";
import { decibels, FLOOR_DB, peakDecibels } from "./meter-math";
import "./Meter.css";

export interface MeterProps {
  label: string;
  /** The latest peak, linear (1 is full scale), read on every frame; undefined when there is none. Keep the function the same from one render to the next. */
  readPeak: () => number | undefined;
  /** How many buffers have clipped, ever; the meter has a CLIP light if it is given this. Keep the function the same too. */
  readClips?: () => number | undefined;
  frames?: FrameLoop;
}

/** A level meter: the peak on a scale of decibels from -60 to 0, and a CLIP light. */
export function Meter({ label, readPeak, readClips, frames = pageFrames }: MeterProps) {
  const readDecibels = useCallback(() => {
    const peak = readPeak();
    return peak === undefined ? undefined : peakDecibels(peak);
  }, [readPeak]);
  return (
    <div className="tang-meter">
      <LiveBar label={label} read={readDecibels} min={FLOOR_DB} max={0} format={decibels} frames={frames} />
      {readClips ? <ClipLight readClips={readClips} frames={frames} /> : null}
    </div>
  );
}
