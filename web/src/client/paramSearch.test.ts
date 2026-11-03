import { describe, expect, test } from "vitest";

import { matchParams } from "./paramSearch";

const params = [
  { index: 0, name: "Condition" },
  { index: 1, name: "Dynamics" },
  { index: 7, name: "Post Effect Equalizer Gain 1" },
  { index: 9, name: "Post Effect Equalizer Gain 2" },
  { index: 12, name: "Reverb Duration" },
  { index: 20, name: "Velocity (Soft)" },
];

describe("finding a parameter by what its name says", () => {
  test("is every parameter, in the plugin's order, for nothing or for blanks", () => {
    expect(matchParams(params, "")).toEqual([0, 1, 7, 9, 12, 20]);
    expect(matchParams(params, "   ")).toEqual([0, 1, 7, 9, 12, 20]);
    expect(matchParams([], "")).toEqual([]);
  });

  test("is the parameters whose name has what was typed in it, whatever the case, by the plugin's own index", () => {
    expect(matchParams(params, "dyn")).toEqual([1]);
    expect(matchParams(params, "DYNAMICS")).toEqual([1]);
    expect(matchParams(params, "ion")).toEqual([0, 12]);
    expect(matchParams(params, "equalizer")).toEqual([7, 9]);
  });

  test("wants every word, in any order, and not that they are next to each other", () => {
    expect(matchParams(params, "gain post")).toEqual([7, 9]);
    expect(matchParams(params, "post 2")).toEqual([9]);
    expect(matchParams(params, "effect  equalizer")).toEqual([7, 9]);
    expect(matchParams(params, "reverb gain")).toEqual([]);
  });

  test("is nothing when nothing matches", () => {
    expect(matchParams(params, "zzz")).toEqual([]);
  });

  test("takes what was typed as text, not as a pattern", () => {
    expect(matchParams(params, "(soft)")).toEqual([20]);
    expect(matchParams(params, "(")).toEqual([20]);
    expect(matchParams(params, ".*")).toEqual([]);
    expect(matchParams(params, "[")).toEqual([]);
  });

  test("leaves what it was given alone", () => {
    const copy = structuredClone(params);
    matchParams(params, "post");
    expect(params).toEqual(copy);
  });
});
