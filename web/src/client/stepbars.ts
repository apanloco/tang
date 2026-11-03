// What a person does to the bars of a step sequencer, as numbers: which bar a pointer is over, the value it is at, what a stroke across them sets, and which bar the
// playhead is on. The component (`ui/StepBars.tsx`) draws and listens; what it makes of a place is decided here, where it is tested without a browser.
import { MAX_STEPS } from "../protocol";
import { stepsPlace } from "./steps";

/** A bar is set to a hundredth: a session file holds the number that a hand meant, not the pixel that it was on. */
const RESOLUTION = 100;
/** A pointer this near the middle of a bar's travel (a twentieth of it, either way) sets it to nothing: a hand cannot stop on the line, and nothing is a value that is wanted. */
const CENTRE_GRIP = 0.04;

/** A number to a hundredth, with no negative zero (a bar that is nothing is 0, not -0, in every list and file). */
const tidy = (value: number): number => Math.round(value * RESOLUTION) / RESOLUTION + 0;

/** The bar of `count` that the place `x` is over in a box `width` wide, counted from the left: a pointer that has gone past an end is over the bar there. */
export function columnAt(x: number, width: number, count: number = MAX_STEPS): number {
  if (!(width > 0) || count < 1) return 0;
  return Math.min(Math.max(Math.floor((x / width) * count), 0), count - 1);
}

/** The value of a bar that the place `y` is at in a box `height` high, counted from the top: 1 at the top, 0 in the middle and -1 at the bottom; to a hundredth, and 0 near the middle. */
export function stepValueAt(y: number, height: number): number {
  if (!(height > 0)) return 0;
  const value = tidy(Math.min(Math.max(1 - (2 * y) / height, -1), 1));
  return Math.abs(value) <= CENTRE_GRIP ? 0 : value;
}

/** Where a stroke set a bar last: which, and to what. */
export interface Paint {
  column: number;
  value: number;
}

/**
 * `values` with the bar at `to` set. A stroke that has come from `from` (the pointer is moved a few bars between two events when it is quick) sets the bars between the two as well, along
 * the line from one value to the other, so that it leaves no gaps; the start of a stroke has none. The list that is given is not changed.
 */
export function paintTo(values: readonly number[], from: Paint | null, to: Paint): number[] {
  const painted = [...values];
  if (from === null || from.column === to.column) {
    painted[to.column] = to.value;
    return painted;
  }
  const span = Math.abs(to.column - from.column);
  const direction = Math.sign(to.column - from.column);
  for (let step = 1; step <= span; step += 1) {
    painted[from.column + direction * step] = tidy(from.value + ((to.value - from.value) * step) / span);
  }
  return painted;
}

/**
 * The bar that the playhead is on for a step sequencer of `length` steps in use whose position (as the engine publishes it: the step as the whole part of the number) is `position`:
 * none (-1) when there is no position to follow. A position that is past the length (it was shortened since) goes round at it.
 */
export function playheadStep(position: number | undefined, length: number): number {
  if (position === undefined || !Number.isFinite(position)) return -1;
  const steps = Math.min(Math.max(Math.trunc(length), 1), MAX_STEPS);
  return Math.min(Math.floor(stepsPlace(length, position, MAX_STEPS)), steps - 1);
}
