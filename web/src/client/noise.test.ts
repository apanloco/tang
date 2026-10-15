import { describe, expect, test } from "vitest";

import { noiseFixture } from "./fixtures";
import { noise } from "./noise";

// The random numbers of an LFO (`plugin::chain::noise`): the page's hash, pinned to the engine's by what the engine makes of a spread of seeds and cycles (the fixture).
describe("the number of a cycle of a random LFO", () => {
  test("is what the engine makes of each seed and cycle of its fixture, to the last bit", () => {
    expect(noiseFixture.numbers.length).toBeGreaterThan(100);
    const wrong = noiseFixture.numbers.filter((entry) => noise(entry.seed, entry.cycle) !== entry.value);
    expect(wrong).toEqual([]);
  });

  test("has been tried at the ends of the 32 bits and in between: the fixture is not thin", () => {
    const seeds = new Set(noiseFixture.numbers.map((entry) => entry.seed));
    const cycles = new Set(noiseFixture.numbers.map((entry) => entry.cycle));
    for (const end of [0, 0xffff_ffff]) {
      expect(seeds.has(end), `seed ${String(end)}`).toBe(true);
      expect(cycles.has(end), `cycle ${String(end)}`).toBe(true);
    }
    expect(cycles.has(0x8000_0000)).toBe(true);
    expect(seeds.size).toBeGreaterThanOrEqual(8);
    expect(new Set(noiseFixture.numbers.map((entry) => entry.value)).size, "the numbers are not alike").toBeGreaterThan(noiseFixture.numbers.length * 0.95);
  });

  test("is from -1 up to but not including 1, and is spread over it", () => {
    const numbers = Array.from({ length: 20_000 }, (_, cycle) => noise(3, cycle));
    expect(numbers.every((n) => n >= -1 && n < 1)).toBe(true);
    const mean = numbers.reduce((sum, n) => sum + n, 0) / numbers.length;
    expect(Math.abs(mean)).toBeLessThan(0.02);
    for (let fifth = 0; fifth < 5; fifth += 1) {
      const low = -1 + fifth * 0.4;
      const count = numbers.filter((n) => n >= low && n < low + 0.4).length;
      expect(count, `fifth ${String(fifth)}`).toBeGreaterThan(3_600);
      expect(count, `fifth ${String(fifth)}`).toBeLessThan(4_400);
    }
  });

  test("goes by the low 32 bits of the seed and of the cycle, as the engine's counters do", () => {
    expect(noise(5, 2 ** 32 + 3)).toBe(noise(5, 3));
    expect(noise(2 ** 32 + 5, 3)).toBe(noise(5, 3));
    expect(noise(5, -1)).toBe(noise(5, 0xffff_ffff));
  });
});
