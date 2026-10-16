import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent, PointerEvent } from "react";

import { pageFrames } from "../client/frameloop";
import type { FrameLoop } from "../client/frameloop";
import { columnAt, paintTo, playheadStep, stepValueAt } from "../client/stepbars";
import type { Paint } from "../client/stepbars";
import { formatValue } from "./Knob";
import { afterKey } from "./knob-geometry";
import "./StepBars.css";

export interface StepBarsProps {
  /** What the bars are, in words: the group's name to what reads the page. */
  label: string;
  /** The value of every step, from -1 to 1: the bars are controlled, they draw this and ask for changes with `onChange`. */
  values: readonly number[];
  /** How many of the steps are in use: the rest are drawn dimmed, and stay as they are. */
  length: number;
  /** All the values as a stroke or a key has made them; without it the bars only show. */
  onChange?: ((values: number[]) => void) | undefined;
  /** A gesture that changed values is over (a key press is one of its own). */
  onCommit?: (() => void) | undefined;
  /**
   * Where the step sequencer is, read on every frame to place the playhead: the step as the whole part of the number. It does not render the bars again: the
   * playhead is written to the DOM. Keep the function the same from one render to the next (the playhead is subscribed to it).
   */
  read?: (() => number | undefined) | undefined;
  frames?: FrameLoop;
}

/** The value of step `index` in `values` made `value`. */
const with_ = (values: readonly number[], index: number, value: number): number[] => values.map((was, at) => (at === index ? value : was));

/**
 * The bars of a step sequencer: a bar for each step, up from the middle for a value above nothing and down for one below it, painted by dragging across them and moved
 * bar by bar with the keys, with a playhead that is on the bar the sequencer is at (docs/web-ui.md, step 57). The steps that are past the length are dimmed.
 */
export function StepBars({ label, values, length, onChange, onCommit, read, frames = pageFrames }: StepBarsProps) {
  const bars = useRef<(HTMLDivElement | null)[]>([]);
  // The list that the stroke in hand has made so far, and where it last set a bar: the props are not what it goes on from, since they come back a render later.
  const stroke = useRef<{ values: number[]; last: Paint } | null>(null);
  // The bar that is in the tab order (the others are got to with the arrows that go across).
  const [tabbed, setTabbed] = useState(0);
  const readOnly = !onChange;

  useEffect(() => {
    if (!read) return undefined;
    let shown = -1;
    const show = (step: number): void => {
      if (step === shown) return;
      if (shown >= 0) bars.current[shown]?.removeAttribute("data-playing");
      if (step >= 0) bars.current[step]?.setAttribute("data-playing", "");
      shown = step;
    };
    const unsubscribe = frames.subscribe(() => {
      show(playheadStep(read(), length));
    });
    return () => {
      unsubscribe();
      show(-1);
    };
  }, [frames, read, length]);

  const placeOf = (event: PointerEvent<HTMLDivElement>): Paint => {
    const box = event.currentTarget.getBoundingClientRect();
    return { column: columnAt(event.clientX - box.left, box.width, values.length), value: stepValueAt(event.clientY - box.top, box.height) };
  };

  const onPointerDown = (event: PointerEvent<HTMLDivElement>): void => {
    if (!onChange || event.button !== 0) return;
    // (jsdom has no pointer capture, and a pointer that is gone before it is captured is no harm.)
    try {
      event.currentTarget.setPointerCapture(event.pointerId);
    } catch {
      // nothing to capture
    }
    const place = placeOf(event);
    const painted = paintTo(values, null, place);
    stroke.current = { values: painted, last: place };
    bars.current[place.column]?.focus();
    onChange(painted);
  };

  const onPointerMove = (event: PointerEvent<HTMLDivElement>): void => {
    const gesture = stroke.current;
    if (!gesture || !onChange) return;
    const place = placeOf(event);
    if (place.column === gesture.last.column && place.value === gesture.last.value) return;
    const painted = paintTo(gesture.values, gesture.last, place);
    stroke.current = { values: painted, last: place };
    onChange(painted);
  };

  const endStroke = (): void => {
    if (!stroke.current) return;
    stroke.current = null;
    onCommit?.();
  };

  const onKeyDown = (index: number, event: KeyboardEvent<HTMLDivElement>): void => {
    // The arrows that go across move the focus from bar to bar; the keys that go up and down change the value.
    if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
      event.preventDefault();
      bars.current[Math.min(Math.max(index + (event.key === "ArrowRight" ? 1 : -1), 0), values.length - 1)]?.focus();
      return;
    }
    if (!onChange) return;
    const value = values[index] ?? 0;
    const next = afterKey(event.key, event.shiftKey, value, -1, 1);
    if (next === undefined) return;
    event.preventDefault();
    if (next === value) return;
    onChange(with_(values, index, next));
    onCommit?.();
  };

  const onDoubleClick = (index: number): void => {
    if (!onChange || (values[index] ?? 0) === 0) return;
    onChange(with_(values, index, 0));
    onCommit?.();
  };

  return (
    <div
      className="tang-stepbars nodrag nopan nowheel"
      role="group"
      aria-label={label}
      data-readonly={readOnly ? "" : undefined}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endStroke}
      onPointerCancel={endStroke}
    >
      {values.map((value, index) => (
        <div
          // (A bar is the step it is: the bars never come or go.)
          key={index}
          ref={(element) => {
            bars.current[index] = element;
          }}
          className="tang-stepbars__step"
          role="slider"
          tabIndex={index === tabbed ? 0 : -1}
          aria-label={`Step ${String(index + 1)}`}
          aria-orientation="vertical"
          aria-valuemin={-1}
          aria-valuemax={1}
          aria-valuenow={value}
          aria-valuetext={formatValue(value)}
          aria-readonly={readOnly || undefined}
          data-negative={value < 0 ? "" : undefined}
          data-unused={index >= length ? "" : undefined}
          onFocus={() => {
            setTabbed(index);
          }}
          onKeyDown={(event) => {
            onKeyDown(index, event);
          }}
          onDoubleClick={() => {
            onDoubleClick(index);
          }}
        >
          <span className="tang-stepbars__fill" style={{ height: `${String(Math.abs(value) * 50)}%` }} />
        </div>
      ))}
    </div>
  );
}
