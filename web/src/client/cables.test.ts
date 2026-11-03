import { describe, expect, test } from "vitest";

import type { Edge, EdgeId, Port } from "../protocol";
import { cablesThatGo, drivesRoundToItself, isAudioCable, isLink, isModulation } from "./cables";

/** A cable that drives the depth of the cable `of`, or a parameter if there is none. */
function cable(id: EdgeId, of: EdgeId | null): Edge {
  const port: Port = of === null ? { type: "param", index: id } : { type: "depth", edge: of };
  return { id, kind: "modulation", from: { node: 100 + id, port: { type: "modOut" } }, to: { node: 200, port }, amount: 0.5 };
}

const link: Edge = { kind: "audio", from: { node: 1, port: { type: "audioOut" } }, to: { node: 2, port: { type: "audioIn" } } };

const member: Edge = { kind: "member", from: { node: 1, port: { type: "audioOut" } }, to: { node: 3, port: { type: "audioIn" } } };
const send: Edge = { kind: "cable", id: 9, gain: 0.5, from: { node: 1, port: { type: "audioOut" } }, to: { node: 2, port: { type: "audioIn" } } };

describe("a cable of modulation", () => {
  test("is told from the other edges of the graph", () => {
    expect(isModulation(cable(1, null))).toBe(true);
    expect(isModulation(link)).toBe(false);
    expect(isModulation(send)).toBe(false);
  });
});

describe("the three kinds of edge that are not a cable of modulation", () => {
  test("are told from each other: a link of the audio path that a lane and a group have by themselves, a member link, and an audio cable that has been drawn", () => {
    expect([isLink(link), isLink(member), isLink(send), isLink(cable(1, null))]).toEqual([true, true, false, false]);
    expect([isAudioCable(link), isAudioCable(member), isAudioCable(send), isAudioCable(cable(1, null))]).toEqual([false, false, true, false]);
  });
});

describe("the cables that go with one", () => {
  test("are it, and the cables that drive its depth, and those that drive theirs in turn, and no others", () => {
    // 1 is driven by 2 and 3, 2 by 4; 5 drives a parameter, and 6 the depth of 5.
    const cables = [cable(1, null), cable(2, 1), cable(3, 1), cable(4, 2), cable(5, null), cable(6, 5), link];
    expect([...cablesThatGo(cables, [1])].sort()).toEqual([1, 2, 3, 4]);
    expect([...cablesThatGo(cables, [2])].sort()).toEqual([2, 4]);
    expect([...cablesThatGo(cables, [4])]).toEqual([4]);
    expect([...cablesThatGo(cables, [5])].sort()).toEqual([5, 6]);
  });

  test("are the starting cables together, and each only once, for cables that drive each other's depths", () => {
    const cables = [cable(1, 2), cable(2, 1), cable(3, null)];
    expect([...cablesThatGo(cables, [1])].sort()).toEqual([1, 2]);
    expect([...cablesThatGo(cables, [1, 3])].sort()).toEqual([1, 2, 3]);
    expect([...cablesThatGo(cables, [])]).toEqual([]);
  });
});

describe("cables that drive each other's depths", () => {
  const goes = (cables: Edge[], ...ids: EdgeId[]): boolean => drivesRoundToItself(cables, new Set(ids));

  test("are found when one of the cables that go drives a cable that drives it, directly or through others", () => {
    expect(goes([cable(1, 2), cable(2, 1)], 1, 2)).toBe(true);
    expect(goes([cable(1, 2), cable(2, 3), cable(3, 1)], 1, 2, 3)).toBe(true);
    expect(goes([cable(1, 1)], 1)).toBe(true);
  });

  test("are not found in cables that drive a parameter, or each other's depths one way round only", () => {
    expect(goes([cable(1, null), cable(2, null)], 1, 2)).toBe(false);
    expect(goes([cable(1, 2), cable(2, 3), cable(3, null)], 1, 2, 3)).toBe(false);
    expect(goes([], 1)).toBe(false);
    expect(goes([cable(1, 2), cable(2, 1)])).toBe(false);
  });

  test("are not found for a cable that drives into a cycle without being on it, unless one of those on it goes too", () => {
    // 1 drives the depth of 2, which drives 3, which drives 2.
    const cables = [cable(1, 2), cable(2, 3), cable(3, 2)];
    expect(goes(cables, 1)).toBe(false);
    expect(goes(cables, 1, 2)).toBe(true);
    expect(goes(cables, 3)).toBe(true);
  });

  test("are not found in a cable that drives the depth of one that is not there", () => {
    expect(goes([cable(1, 9)], 1)).toBe(false);
  });
});
