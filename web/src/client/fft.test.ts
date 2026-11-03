import { describe, expect, test } from "vitest";

import { Spectrum, FLOOR_DB } from "./fft";

/** A sine of `amplitude` that goes round `cycles` times in a block of `size` samples (a whole number of times is exactly on a bin). */
function sine(size: number, cycles: number, amplitude = 1): Float32Array {
  return Float32Array.from({ length: size }, (_, n) => amplitude * Math.sin((2 * Math.PI * cycles * n) / size));
}

/** The spectrum of a block, in dB. */
function levels(samples: Float32Array): Float32Array {
  return new Spectrum(samples.length).decibels(samples);
}

describe("the spectrum of a block of samples", () => {
  test("has a level for each frequency from nothing to half the sample rate", () => {
    expect(levels(new Float32Array(64))).toHaveLength(33);
    expect(levels(new Float32Array(2048))).toHaveLength(1025);
  });

  test("has a sine of full scale that is exactly on a frequency at 0 dB there, 6 dB under it on each side, and nothing further off", () => {
    const db = levels(sine(2048, 100));
    expect(db[100]).toBeCloseTo(0, 2);
    expect(db[99]).toBeCloseTo(-6.02, 2);
    expect(db[101]).toBeCloseTo(-6.02, 2);
    for (const [bin, level] of db.entries()) {
      if (Math.abs(bin - 100) > 1) expect(level, `bin ${String(bin)}`).toBeLessThan(-100);
      expect(level, `bin ${String(bin)}`).toBeGreaterThanOrEqual(FLOOR_DB);
    }
  });

  test("has a signal that alternates at every sample at half the sample rate, the last of the frequencies, 6 dB over a sine of its size", () => {
    const alternating = Float32Array.from({ length: 128 }, (_, n) => (n % 2 === 0 ? 1 : -1));
    const db = levels(alternating);
    expect(db[64]).toBeCloseTo(6.02, 2);
    // (The window spreads it over the frequency beside it, 6 dB under, as for any other.)
    expect(db[63]).toBeCloseTo(0, 2);
    expect(db[62]).toBeLessThan(-100);
  });

  test("goes down by 6 dB for each halving of the amplitude", () => {
    expect(levels(sine(1024, 37, 0.5))[37]).toBeCloseTo(-6.02, 2);
    expect(levels(sine(1024, 37, 0.1))[37]).toBeCloseTo(-20, 2);
    expect(levels(sine(1024, 37, 0.001))[37]).toBeCloseTo(-60, 2);
  });

  test("puts a sine between two frequencies under 0 dB by what the window costs there (1.4 dB), at the two of them", () => {
    const db = levels(sine(2048, 100.5));
    expect(db[100]).toBeCloseTo(-1.42, 1);
    expect(db[101]).toBeCloseTo(-1.42, 1);
  });

  test("shows each of two sines at its own frequency and level", () => {
    const low = sine(2048, 50, 1);
    const high = sine(2048, 400, 0.25);
    const db = levels(low.map((s, n) => s + (high[n] ?? 0)));
    expect(db[50]).toBeCloseTo(0, 1);
    expect(db[400]).toBeCloseTo(-12.04, 1);
    expect(db[200]).toBeLessThan(-100);
  });

  test("puts a constant, which a real signal has once and not twice, 6 dB over the sine of its size", () => {
    expect(levels(new Float32Array(256).fill(1))[0]).toBeCloseTo(6.02, 2);
  });

  test("is as low as it goes, not minus infinity, for silence", () => {
    const db = levels(new Float32Array(512));
    expect(FLOOR_DB).toBeLessThan(-100);
    expect(Array.from(db).every((level) => level === FLOOR_DB)).toBe(true);
  });

  test("agrees with a sum made the slow way, for any block", () => {
    const size = 64;
    let seed = 12345;
    const noise = Float32Array.from({ length: size }, () => {
      seed = (seed * 1664525 + 1013904223) % 4294967296;
      return (seed / 4294967296) * 2 - 1;
    });
    const db = levels(noise);
    for (let bin = 0; bin <= size / 2; bin += 1) {
      let re = 0;
      let im = 0;
      for (let n = 0; n < size; n += 1) {
        const windowed = (noise[n] ?? 0) * (0.5 - 0.5 * Math.cos((2 * Math.PI * n) / size));
        re += windowed * Math.cos((2 * Math.PI * bin * n) / size);
        im -= windowed * Math.sin((2 * Math.PI * bin * n) / size);
      }
      const expected = Math.max(FLOOR_DB, 20 * Math.log10(Math.hypot(re, im) / (size / 4)));
      expect(db[bin], `bin ${String(bin)}`).toBeCloseTo(expected, 3);
    }
  });

  test("is made of a block whose length is a power of two, 4 or more, and nothing else", () => {
    for (const bad of [0, 1, 2, 3, 6, 100, 1000, 2047]) expect(() => new Spectrum(bad), String(bad)).toThrow(RangeError);
    expect(() => new Spectrum(4)).not.toThrow();
    expect(() => new Spectrum(4096)).not.toThrow();
  });

  test("is of the block it was made for: another length is refused", () => {
    expect(() => new Spectrum(64).decibels(new Float32Array(32))).toThrow(RangeError);
  });

  test("can be made again from other blocks, into an array that is given, without the one before showing", () => {
    const analyser = new Spectrum(256);
    const out = new Float32Array(129);
    analyser.decibels(sine(256, 20), out);
    expect(out[20]).toBeCloseTo(0, 2);
    analyser.decibels(sine(256, 40), out);
    expect(out[20]).toBeLessThan(-100);
    expect(out[40]).toBeCloseTo(0, 2);
  });
});
