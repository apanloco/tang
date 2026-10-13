import { describe, expect, test } from "vitest";

import { decibels, FLOOR_DB, peakDecibels } from "./meter-math";

describe("a peak in decibels", () => {
  test("is 0 dB at full scale, and 6 dB less for every halving", () => {
    expect(peakDecibels(1)).toBe(0);
    expect(peakDecibels(0.5)).toBeCloseTo(-6.0206, 4);
    expect(peakDecibels(0.25)).toBeCloseTo(-12.0412, 4);
    expect(peakDecibels(0.1)).toBeCloseTo(-20, 10);
  });

  test("is above 0 dB beyond full scale, which is a clip", () => {
    expect(peakDecibels(2)).toBeCloseTo(6.0206, 4);
    expect(peakDecibels(10)).toBeCloseTo(20, 10);
  });

  test("does not go below the floor, however quiet", () => {
    expect(FLOOR_DB).toBe(-60);
    expect(peakDecibels(0.001)).toBeCloseTo(-60, 10);
    expect(peakDecibels(0.0001)).toBe(-60);
    expect(peakDecibels(1e-30)).toBe(-60);
    expect(peakDecibels(0)).toBe(-60);
    expect(peakDecibels(-1)).toBe(-60);
    expect(peakDecibels(Number.NEGATIVE_INFINITY)).toBe(-60);
  });

  test("is no level when there is no number", () => {
    expect(peakDecibels(Number.NaN)).toBeNaN();
  });

  test("is written with a decimal and the unit", () => {
    expect(decibels(-12.34)).toBe("-12.3 dB");
    expect(decibels(0)).toBe("0.0 dB");
    expect(decibels(-60)).toBe("-60.0 dB");
  });
});
