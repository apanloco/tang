import { useEffect, useRef } from "react";

import { pageFrames } from "../client/frameloop";
import type { FrameLoop } from "../client/frameloop";
import { formatValue } from "./Knob";
import { fractionOf } from "./knob-geometry";
import "./LiveBar.css";

export interface LiveBarProps {
  label: string;
  /**
   * The value to draw, read on every frame; undefined when there is none. It does not make
   * the bar render again: the bar is written straight to the DOM. Keep the function the
   * same from one render to the next (the bar is subscribed to it).
   */
  read: () => number | undefined;
  /** The values at the two ends of the bar. */
  min: number;
  max: number;
  /** Fill from the middle of the bar, for a value that goes both ways (a LFO's output). */
  centred?: boolean;
  /** How `aria-valuetext` says the value. */
  format?: (value: number) => string;
  frames?: FrameLoop;
}

/** A bar that shows a value that changes every frame (a modulator's output, a level), drawn without React. */
export function LiveBar({ label, read, min, max, centred = false, format = formatValue, frames = pageFrames }: LiveBarProps) {
  const bar = useRef<HTMLDivElement>(null);
  const fill = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let last: number | undefined;
    return frames.subscribe(() => {
      const element = bar.current;
      const filled = fill.current;
      if (!element || !filled) return;
      const value = read();
      if (value === last) return;
      last = value;
      if (value === undefined || !Number.isFinite(value)) {
        filled.style.setProperty("--to", "0");
        filled.style.setProperty("--from", "0");
        element.removeAttribute("aria-valuenow");
        element.removeAttribute("aria-valuetext");
        return;
      }
      const fraction = fractionOf(value, min, max);
      filled.style.setProperty("--from", String(centred ? Math.min(0.5, fraction) : 0));
      filled.style.setProperty("--to", String(centred ? Math.max(0.5, fraction) : fraction));
      element.setAttribute("aria-valuenow", String(value));
      element.setAttribute("aria-valuetext", format(value));
    });
  }, [frames, read, min, max, centred, format]);

  return (
    <div ref={bar} className="tang-livebar" role="meter" aria-label={label} aria-valuemin={min} aria-valuemax={max} data-centred={centred ? "" : undefined}>
      <div ref={fill} className="tang-livebar__fill" />
    </div>
  );
}
