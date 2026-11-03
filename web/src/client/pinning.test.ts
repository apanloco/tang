import { describe, expect, test } from "vitest";

import { PINNED_LIMIT } from "../protocol";
import type { Param, Plugin } from "../protocol";
import { DEFAULT_KNOBS, knobsOf, pinnedWith, shownParams } from "./pinning";

const param = (index: number): Param => ({ index, name: `p${String(index)}`, min: 0, max: 1, default: 0, value: 0, labels: null, unit: null, group: [], stepCount: 0 });

/** A plugin whose parameters have these indices (a plugin's own, which need not be where they stand in the list), with these pinned. */
const plugin = (indices: readonly number[], pinned: readonly number[] = []): Plugin => ({
  id: "builtin:test",
  format: "Built-in",
  isInstrument: false,
  params: indices.map(param),
  presets: [],
  currentPreset: null,
  pinned: [...pinned],
});

/** Twelve parameters, whose indices are 10, 20 ... 120: so that a place and an index are never mistaken for each other. */
const TWELVE = Array.from({ length: 12 }, (_, i) => (i + 1) * 10);
const FIRST_EIGHT = TWELVE.slice(0, DEFAULT_KNOBS);

describe("the parameters a node shows as knobs for its plugin", () => {
  test("are the first eight when none is pinned, by the plugin's own indices and not by where they stand", () => {
    expect(DEFAULT_KNOBS).toBe(8);
    expect(shownParams(plugin(TWELVE))).toEqual(FIRST_EIGHT);
  });

  test("are all of them when the plugin has fewer than eight, and none when it has none", () => {
    expect(shownParams(plugin([4, 9]))).toEqual([4, 9]);
    expect(shownParams(plugin([]))).toEqual([]);
  });

  test("are the ones pinned, in the order they were pinned, when some are: the first eight are no longer shown", () => {
    expect(shownParams(plugin(TWELVE, [30, 10]))).toEqual([30, 10]);
    expect(shownParams(plugin(TWELVE, [120]))).toEqual([120]);
  });

  test("leave out a pin that the plugin has no parameter for (a session of another version of it), and are the first eight if that leaves none", () => {
    expect(shownParams(plugin(TWELVE, [30, 999, 10]))).toEqual([30, 10]);
    expect(shownParams(plugin(TWELVE, [999]))).toEqual(FIRST_EIGHT);
  });
});

describe("the knobs a node draws", () => {
  test("are none for a node without a plugin", () => {
    expect(knobsOf(null, new Set([1]))).toEqual([]);
  });

  test("are the ones it shows, and, after them, every one a cable ends at that is not among them, in the plugin's order and not twice", () => {
    const many = plugin(TWELVE);
    expect(knobsOf(many, new Set())).toEqual(FIRST_EIGHT);
    expect(knobsOf(many, new Set([120, 90]))).toEqual([...FIRST_EIGHT, 90, 120]);
    expect(knobsOf(many, new Set([20, 120]))).toEqual([...FIRST_EIGHT, 120]);
  });

  test("are the pinned ones and the cables' targets when some are pinned: the first eight are not added", () => {
    const pinned = plugin(TWELVE, [110, 30]);
    expect(knobsOf(pinned, new Set())).toEqual([110, 30]);
    expect(knobsOf(pinned, new Set([30, 50, 10]))).toEqual([110, 30, 10, 50]);
  });

  test("leave out a cable's target that the plugin has no parameter for", () => {
    expect(knobsOf(plugin([1, 3]), new Set([2, 3]))).toEqual([1, 3]);
  });
});

describe("pinning a parameter to a node, or taking it off", () => {
  test("puts one that is not shown after the ones that are: with none pinned, the first eight stay and it is the ninth", () => {
    expect(pinnedWith(plugin(TWELVE), 110, true)).toEqual({ list: [...FIRST_EIGHT, 110] });
  });

  test("adds to the list when there is one, at the end", () => {
    expect(pinnedWith(plugin(TWELVE, [30, 10]), 110, true)).toEqual({ list: [30, 10, 110] });
  });

  test("changes nothing for a parameter that is shown already, pinned or by the first eight", () => {
    expect(pinnedWith(plugin(TWELVE), 30, true)).toEqual({ unchanged: true });
    expect(pinnedWith(plugin(TWELVE, [30, 10]), 30, true)).toEqual({ unchanged: true });
  });

  test("is refused for a parameter the plugin has not, and for one more than a node may show", () => {
    expect(pinnedWith(plugin(TWELVE), 999, true)).toEqual({ refused: "the plugin has no parameter 999" });
    const huge = Array.from({ length: PINNED_LIMIT + 1 }, (_, i) => i);
    expect(pinnedWith(plugin(huge, huge.slice(0, PINNED_LIMIT)), PINNED_LIMIT, true)).toEqual({ refused: `a node can show at most ${String(PINNED_LIMIT)} parameters` });
    // (The last one that fits is taken.)
    expect(pinnedWith(plugin(huge, huge.slice(0, PINNED_LIMIT - 1)), PINNED_LIMIT - 1, true)).toEqual({ list: huge.slice(0, PINNED_LIMIT) });
  });

  test("takes one off: with none pinned the first eight are the list, less that one", () => {
    expect(pinnedWith(plugin(TWELVE), 30, false)).toEqual({ list: [10, 20, 40, 50, 60, 70, 80] });
  });

  test("takes one off the list, keeping the order of the others, and the last one off gives the node back to the first eight", () => {
    expect(pinnedWith(plugin(TWELVE, [30, 10, 110]), 10, false)).toEqual({ list: [30, 110] });
    expect(pinnedWith(plugin(TWELVE, [30]), 30, false)).toEqual({ list: [] });
  });

  test("changes nothing for a parameter that is not shown, or that the plugin has not", () => {
    expect(pinnedWith(plugin(TWELVE), 110, false)).toEqual({ unchanged: true });
    expect(pinnedWith(plugin(TWELVE, [30]), 10, false)).toEqual({ unchanged: true });
    expect(pinnedWith(plugin(TWELVE), 999, false)).toEqual({ unchanged: true });
  });
});
