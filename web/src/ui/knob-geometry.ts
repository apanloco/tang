// The arithmetic of a knob, apart from any drawing: where on its sweep a value is, the
// path of an arc between two values, and what a drag or a key press does to a value.

/** A knob turns through three quarters of a circle, from lower left to lower right. */
export const SWEEP_DEGREES = 270;
const START_DEGREES = -135;

/** How many pixels of vertical drag turn the knob through its whole range. */
export const DRAG_PIXELS = 200;

const clamp = (value: number, low: number, high: number): number => Math.min(Math.max(value, low), high);

/**
 * How the values of a knob are spread over its sweep: evenly, or, for a rate or a time that
 * runs over decades, so that equal ratios are equally far apart (which needs a range that
 * starts above nothing: a range that does not is spread evenly whatever is asked).
 */
export type Scale = "linear" | "log";

const isLog = (scale: Scale, min: number): boolean => scale === "log" && min > 0;

/** Where `value` is between `min` and `max`, from 0 to 1 (0 for a range that is nothing). */
export function fractionOf(value: number, min: number, max: number, scale: Scale = "linear"): number {
  if (!(max > min)) return 0;
  if (isLog(scale, min)) {
    // (A value at or below the start has no logarithm to speak of, and is at the start.)
    return value <= min ? 0 : clamp(Math.log(value / min) / Math.log(max / min), 0, 1);
  }
  return clamp((value - min) / (max - min), 0, 1);
}

/** The value at `fraction` of the way from `min` to `max` (the ends themselves, exactly, at 0 and 1). */
export function valueAt(fraction: number, min: number, max: number, scale: Scale = "linear"): number {
  const at = clamp(fraction, 0, 1);
  if (isLog(scale, min)) return at <= 0 ? min : at >= 1 ? max : min * (max / min) ** at;
  return min + at * (max - min);
}

/** A point at `radius` from the centre, `degrees` clockwise from straight up. */
export function pointAt(centre: number, radius: number, degrees: number): { x: number; y: number } {
  const radians = (degrees * Math.PI) / 180;
  return { x: centre + radius * Math.sin(radians), y: centre - radius * Math.cos(radians) };
}

/** The angle, clockwise from straight up, of a fraction of the sweep. */
export function angleOf(fraction: number): number {
  return START_DEGREES + clamp(fraction, 0, 1) * SWEEP_DEGREES;
}

/** Two decimals are enough to draw with, and keep the paths short. */
const n = (value: number): string => String(Math.round(value * 100) / 100);

/**
 * The SVG path of the arc from one fraction of the sweep to another, clockwise (the other
 * way round if `to` is before `from`, which a modulation of a negative depth has). Empty
 * for an arc of no length.
 */
export function arcPath(centre: number, radius: number, from: number, to: number): string {
  const [low, high] = from <= to ? [from, to] : [to, from];
  if (high - low < 1e-4) return "";
  const start = pointAt(centre, radius, angleOf(low));
  const end = pointAt(centre, radius, angleOf(high));
  const large = (high - low) * SWEEP_DEGREES > 180 ? 1 : 0;
  return `M ${n(start.x)} ${n(start.y)} A ${n(radius)} ${n(radius)} 0 ${large} 1 ${n(end.x)} ${n(end.y)}`;
}

/**
 * The value after the pointer has moved `pixels` down the screen (negative is up, which
 * raises the value), at `speed` times the usual rate (a tenth for a fine adjustment).
 */
export function dragged(value: number, pixels: number, min: number, max: number, speed = 1, scale: Scale = "linear"): number {
  if (isLog(scale, min)) {
    // A drag that has not moved leaves the value as it is, not an ulp away from it.
    return pixels === 0 ? value : valueAt(fractionOf(value, min, max, scale) - (pixels / DRAG_PIXELS) * speed, min, max, scale);
  }
  return clamp(value - (pixels / DRAG_PIXELS) * (max - min) * speed, min, max);
}

/** The value after `steps` steps of `size` of the range, or of the sweep on a logarithmic scale (negative steps go down). */
export function stepped(value: number, steps: number, size: number, min: number, max: number, scale: Scale = "linear"): number {
  if (isLog(scale, min)) {
    return steps === 0 ? value : valueAt(fractionOf(value, min, max, scale) + steps * size, min, max, scale);
  }
  return clamp(value + steps * size * (max - min), min, max);
}

/**
 * The value after a key press: a step for the arrows (a tenth of a step with shift held), ten
 * for the page keys, the ends for Home and End. `undefined` for a key that is not the knob's.
 */
export function afterKey(key: string, fine: boolean, value: number, min: number, max: number, scale: Scale = "linear"): number | undefined {
  const size = fine ? 0.001 : 0.01;
  switch (key) {
    case "ArrowUp":
    case "ArrowRight":
      return stepped(value, 1, size, min, max, scale);
    case "ArrowDown":
    case "ArrowLeft":
      return stepped(value, -1, size, min, max, scale);
    case "PageUp":
      return stepped(value, 10, size, min, max, scale);
    case "PageDown":
      return stepped(value, -10, size, min, max, scale);
    case "Home":
      return min;
    case "End":
      return max;
    default:
      return undefined;
  }
}
