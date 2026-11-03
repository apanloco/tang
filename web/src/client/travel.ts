// How the values of a number are spread over its range, evenly or on a log scale: where on a knob's travel a value is, and the value at a place on it. A knob spreads them over its
// sweep (`ui/knob-geometry.ts`) and a modulator moves a setting that is turned on a log scale a fraction of that travel (`client/modulation.ts`). Pure functions of numbers.

const clamp = (value: number, low: number, high: number): number => Math.min(Math.max(value, low), high);

/**
 * How the values of a knob are spread over its sweep: evenly, or, for a rate or a time that
 * runs over decades, so that equal ratios are equally far apart (which needs a range that
 * starts above nothing: a range that does not is spread evenly whatever is asked).
 */
export type Scale = "linear" | "log";

/** Whether a range is spread on a log scale: asked for, and above nothing (a log scale has no zero). */
export const isLog = (scale: Scale, min: number): boolean => scale === "log" && min > 0;

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
