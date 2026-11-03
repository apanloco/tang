import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent, PointerEvent } from "react";

import { pageFrames } from "../client/frameloop";
import type { FrameLoop } from "../client/frameloop";
import { releaseLost, startsDrag, useDragEnd } from "./dragEnd";
import { afterKey, angleOf, arcPath, dragged, fractionOf, pointAt } from "./knob-geometry";
import type { Scale } from "./knob-geometry";
import "./Knob.css";

const CENTRE = 24;
const TRACK_RADIUS = 18;
const RING_RADIUS = 21.5;
const POINTER_START = 5;

/** A number as a person would write it, to three figures. */
export const formatValue = (value: number): string => String(Number(value.toPrecision(3)));

export interface KnobProps {
  label: string;
  /** What is written under the knob, if it is not the label (which is its name to a screen reader): the master's `Master volume` is written `Volume`, which is what fits under it. */
  caption?: string;
  /**
   * What the parameter is set to (not what modulation makes of it). The knob is controlled:
   * it draws this and asks for changes with `onChange`, and whoever holds the value decides
   * what the knob shows, which is how a knob stays under the pointer while the server's
   * echoes of its earlier values come in.
   */
  value: number;
  min: number;
  max: number;
  /** How the values are spread over the sweep: evenly (the default), or, for a rate or a time over decades, by their ratios. */
  scale?: Scale;
  /** What a double click puts the value back to. */
  defaultValue?: number;
  format?: (value: number) => string;
  /** How far modulation swings the value, as the values at the two ends of the swing: the ring. */
  modulation?: { low: number; high: number } | undefined;
  /** The value changes to this, as the knob is dragged or a key is pressed; without it the knob only shows. */
  onChange?: (value: number) => void;
  /** A gesture that changed the value is over, at this value (a key press is a gesture of its own). */
  onCommit?: (value: number) => void;
  /**
   * What the parameter is set to now, modulation included, read on every frame to place the
   * dot. It does not make the knob render again: the dot moves without React. Keep the
   * function the same from one render to the next (the dot is subscribed to it).
   */
  readEffective?: () => number | undefined;
  disabled?: boolean;
  frames?: FrameLoop;
}

export function Knob({
  label,
  caption = label,
  value,
  min,
  max,
  scale = "linear",
  defaultValue,
  format = formatValue,
  modulation,
  onChange,
  onCommit,
  readEffective,
  disabled = false,
  frames = pageFrames,
}: KnobProps) {
  const [dragging, setDragging] = useState(false);
  // Where the pointer was and what the value was then, at the last change; `moved` is whether
  // the gesture has changed the value at all.
  const drag = useRef<{ y: number; value: number; moved: boolean } | null>(null);
  const dot = useRef<SVGCircleElement>(null);
  const readOnly = !onChange;
  const inert = disabled || readOnly;

  const fraction = fractionOf(value, min, max, scale);
  const pointerAngle = angleOf(fraction);
  const from = pointAt(CENTRE, POINTER_START, pointerAngle);
  const to = pointAt(CENTRE, TRACK_RADIUS, pointerAngle);
  const ring = modulation ? arcPath(CENTRE, RING_RADIUS, fractionOf(modulation.low, min, max, scale), fractionOf(modulation.high, min, max, scale)) : "";

  useEffect(() => {
    if (!readEffective) return undefined;
    let last: number | undefined;
    return frames.subscribe(() => {
      const circle = dot.current;
      if (!circle) return;
      const effective = readEffective();
      if (effective === last) return;
      last = effective;
      if (effective === undefined || !Number.isFinite(effective)) {
        circle.setAttribute("visibility", "hidden");
        return;
      }
      const at = pointAt(CENTRE, TRACK_RADIUS, angleOf(fractionOf(effective, min, max, scale)));
      circle.setAttribute("cx", String(at.x));
      circle.setAttribute("cy", String(at.y));
      circle.setAttribute("visibility", "visible");
    });
  }, [frames, readEffective, min, max, scale]);

  const onPointerDown = (event: PointerEvent<HTMLDivElement>): void => {
    if (inert || !startsDrag(event)) return;
    // (jsdom has no pointer capture, and a pointer that is gone before it is captured is no harm.)
    try {
      event.currentTarget.setPointerCapture(event.pointerId);
    } catch {
      // nothing to capture
    }
    event.currentTarget.focus();
    drag.current = { y: event.clientY, value, moved: false };
    setDragging(true);
  };

  const onPointerMove = (event: PointerEvent<HTMLDivElement>): void => {
    const gesture = drag.current;
    if (!gesture || !onChange) return;
    // A mouse that moves with no button down is one whose release was never seen (the button went up elsewhere): the drag is over, at the value that it has.
    if (releaseLost(event)) {
      endDrag();
      return;
    }
    // Rebased on each move, so that a change of speed (shift) does not make the knob jump,
    // and so that a pointer that has gone past an end comes back at once.
    const next = dragged(gesture.value, event.clientY - gesture.y, min, max, event.shiftKey ? 0.1 : 1, scale);
    drag.current = { y: event.clientY, value: next, moved: gesture.moved || next !== gesture.value };
    if (next !== gesture.value) onChange(next);
  };

  const endDrag = (): void => {
    const gesture = drag.current;
    if (!gesture) return;
    drag.current = null;
    setDragging(false);
    if (gesture.moved) onCommit?.(gesture.value);
  };

  // The other ways for a drag to be over: the release goes up anywhere, the window loses the focus, a context menu opens.
  useDragEnd(dragging, endDrag);

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>): void => {
    if (inert) return;
    const next = afterKey(event.key, event.shiftKey, value, min, max, scale);
    if (next === undefined) return;
    event.preventDefault();
    if (next === value) return;
    onChange(next);
    onCommit?.(next);
  };

  const onDoubleClick = (): void => {
    if (inert || defaultValue === undefined || defaultValue === value) return;
    onChange(defaultValue);
    onCommit?.(defaultValue);
  };

  return (
    <div className="tang-knob" data-readonly={readOnly ? "" : undefined} data-disabled={disabled ? "" : undefined} data-dragging={dragging ? "" : undefined}>
      <div
        className="tang-knob__dial"
        role="slider"
        tabIndex={disabled ? -1 : 0}
        aria-label={label}
        aria-valuemin={min}
        aria-valuemax={max}
        aria-valuenow={value}
        aria-valuetext={format(value)}
        aria-readonly={readOnly || undefined}
        aria-disabled={disabled || undefined}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
        onLostPointerCapture={endDrag}
        onKeyDown={onKeyDown}
        onDoubleClick={onDoubleClick}
      >
        <svg viewBox="0 0 48 48" aria-hidden="true" focusable="false">
          <path className="tang-knob__track" d={arcPath(CENTRE, TRACK_RADIUS, 0, 1)} />
          <path className="tang-knob__arc" d={arcPath(CENTRE, TRACK_RADIUS, 0, fraction)} />
          {ring ? <path className="tang-knob__ring" d={ring} /> : null}
          <line className="tang-knob__pointer" x1={from.x} y1={from.y} x2={to.x} y2={to.y} />
          <circle ref={dot} className="tang-knob__dot" r="2.6" cx={CENTRE} cy={CENTRE} visibility="hidden" />
        </svg>
      </div>
      <span className="tang-knob__label">{caption}</span>
      <span className="tang-knob__value">{format(value)}</span>
    </div>
  );
}
