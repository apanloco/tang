// What a level meter shows: a linear peak (1 is full scale) in decibels, which is how a
// person hears a level, from a floor under which there is nothing to see.

/** The level at the bottom of a meter, in decibels: what is quieter is silence. */
export const FLOOR_DB = -60;

/** A linear peak in decibels (1 is 0 dB), not below the floor; NaN stays NaN, since there is no level to show (`Math.max` does that). */
export function peakDecibels(peak: number): number {
  if (peak <= 0) return FLOOR_DB;
  return Math.max(FLOOR_DB, 20 * Math.log10(peak));
}

/** A level as it is written on a meter: one decimal and the unit. */
export const decibels = (value: number): string => `${value.toFixed(1)} dB`;
