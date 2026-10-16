import { describe, expect, test } from "vitest";

import { VELOCITY } from "./keyboard";
import { PointerPiano, SOFTEST, velocityAt } from "./pointerPiano";

function rig() {
  const sent: string[] = [];
  const piano = new PointerPiano({
    noteOn: (note, velocity) => sent.push(`on ${String(note)} @${String(velocity)}`),
    noteOff: (note) => sent.push(`off ${String(note)}`),
  });
  return { piano, sent };
}

describe("how hard a pointer plays a key", () => {
  test("is the softest at the top edge of the key, the hardest at its front edge, and goes evenly between them", () => {
    expect(velocityAt(0)).toBe(SOFTEST);
    expect(velocityAt(1)).toBe(127);
    expect(velocityAt(0.5)).toBe(74);
    expect(velocityAt(0.25)).toBe(47);
    // (Whole numbers, as a note has: 20 and 107 steps over the key.)
    for (const fraction of [0.1, 0.33, 0.6, 0.9]) expect(Number.isInteger(velocityAt(fraction)), String(fraction)).toBe(true);
  });

  test("never goes outside the velocities that a note can have, however far off the key the pointer is, and is the keys' own for a place that is not one", () => {
    expect(velocityAt(-3)).toBe(SOFTEST);
    expect(velocityAt(7)).toBe(127);
    expect(velocityAt(Number.NaN)).toBe(VELOCITY);
    expect(velocityAt(Number.POSITIVE_INFINITY)).toBe(127);
    expect(SOFTEST).toBeGreaterThan(0);
  });

  test("is harder lower on the key, at every step", () => {
    let last = 0;
    for (let step = 0; step <= 100; step += 1) {
      const velocity = velocityAt(step / 100);
      expect(velocity).toBeGreaterThanOrEqual(last);
      last = velocity;
    }
  });
});

describe("a pointer on the keys", () => {
  test("plays a key as hard as it is asked to, and as hard as the keys of the computer do when it is not", () => {
    const { piano, sent } = rig();
    piano.press(1, 60, 33);
    piano.press(2, 62);
    expect(sent).toEqual(["on 60 @33", `on 62 @${String(VELOCITY)}`]);
  });

  test("plays the keys it moves on to as hard as it is asked to there", () => {
    const { piano, sent } = rig();
    piano.press(1, 60, 33);
    piano.move(1, 62, 120);
    piano.move(1, 64);
    piano.release(1);
    expect(sent).toEqual(["on 60 @33", "off 60", "on 62 @120", "off 62", `on 64 @${String(VELOCITY)}`, "off 64"]);
  });

  test("plays the key it goes down on as hard as the keys of the computer do, and ends the note when it goes up", () => {
    const { piano, sent } = rig();
    piano.press(1, 60);
    expect(sent).toEqual([`on 60 @${String(VELOCITY)}`]);
    piano.release(1);
    expect(sent).toEqual([`on 60 @${String(VELOCITY)}`, "off 60"]);
  });

  test("plays the keys it moves over while it is down, one note at a time (a glissando)", () => {
    const { piano, sent } = rig();
    piano.press(1, 60);
    piano.move(1, 62);
    piano.move(1, 64);
    piano.release(1);
    expect(sent).toEqual([`on 60 @${String(VELOCITY)}`, "off 60", `on 62 @${String(VELOCITY)}`, "off 62", `on 64 @${String(VELOCITY)}`, "off 64"]);
  });

  test("does not play the key again while it moves on the key it is on", () => {
    const { piano, sent } = rig();
    piano.press(1, 60);
    piano.move(1, 60);
    piano.move(1, 60);
    expect(sent).toEqual([`on 60 @${String(VELOCITY)}`]);
  });

  test("ends the note where it leaves the keys, and plays again where it comes back to one", () => {
    const { piano, sent } = rig();
    piano.press(1, 60);
    piano.move(1, null);
    expect(sent).toEqual([`on 60 @${String(VELOCITY)}`, "off 60"]);
    piano.move(1, null);
    piano.move(1, 61);
    piano.release(1);
    expect(sent).toEqual([`on 60 @${String(VELOCITY)}`, "off 60", `on 61 @${String(VELOCITY)}`, "off 61"]);
  });

  test("plays nothing where it goes down off the keys, and plays what it moves on to", () => {
    const { piano, sent } = rig();
    piano.press(1, null);
    expect(sent).toEqual([]);
    piano.move(1, 65);
    piano.release(1);
    expect(sent).toEqual([`on 65 @${String(VELOCITY)}`, "off 65"]);
  });

  test("plays nothing while it is not down: a pointer that only moves over the keys is not playing", () => {
    const { piano, sent } = rig();
    piano.move(1, 60);
    piano.move(1, 62);
    piano.release(1);
    expect(sent).toEqual([]);
  });

  test("has gone for good when it goes up: it plays nothing more until it is pressed again", () => {
    const { piano, sent } = rig();
    piano.press(1, 60);
    piano.release(1);
    piano.move(1, 62);
    expect(sent).toEqual([`on 60 @${String(VELOCITY)}`, "off 60"]);
  });

  test("is let go of by a second press: a release that was lost does not leave a note on", () => {
    const { piano, sent } = rig();
    piano.press(1, 60);
    piano.press(1, 64);
    expect(sent).toEqual([`on 60 @${String(VELOCITY)}`, "off 60", `on 64 @${String(VELOCITY)}`]);
  });
});

describe("whether a pointer is down", () => {
  test("is said for a pointer that went down and has not gone up, whether or not it is on a key", () => {
    const { piano } = rig();
    expect(piano.isDown(1)).toBe(false);
    piano.press(1, null);
    piano.press(2, 60);
    expect([piano.isDown(1), piano.isDown(2), piano.isDown(3)]).toEqual([true, true, false]);
    piano.release(1);
    expect([piano.isDown(1), piano.isDown(2)]).toEqual([false, true]);
    piano.releaseAll();
    expect(piano.isDown(2)).toBe(false);
  });
});

describe("two pointers on the keys", () => {
  test("play a chord, each its own note", () => {
    const { piano, sent } = rig();
    piano.press(1, 60);
    piano.press(2, 64);
    piano.release(1);
    expect(sent).toEqual([`on 60 @${String(VELOCITY)}`, `on 64 @${String(VELOCITY)}`, "off 60"]);
    piano.move(2, 67);
    piano.release(2);
    expect(sent.slice(3)).toEqual(["off 64", `on 67 @${String(VELOCITY)}`, "off 67"]);
  });

  test("do not end each other's notes when one goes up", () => {
    const { piano, sent } = rig();
    piano.press(1, 60);
    piano.press(2, 64);
    piano.release(3);
    expect(sent).toEqual([`on 60 @${String(VELOCITY)}`, `on 64 @${String(VELOCITY)}`]);
  });
});

describe("every pointer at once", () => {
  test("is let go of when the page loses the pointers (it loses the focus, or is hidden)", () => {
    const { piano, sent } = rig();
    piano.press(1, 60);
    piano.press(2, 64);
    piano.press(3, null);
    piano.releaseAll();
    expect(sent.slice(2).sort()).toEqual(["off 60", "off 64"]);
    // Nothing is down any more.
    piano.move(1, 62);
    piano.move(3, 62);
    expect(sent).toHaveLength(4);
  });
});
