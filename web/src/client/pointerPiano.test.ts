import { describe, expect, test } from "vitest";

import { VELOCITY } from "./keyboard";
import { PointerPiano } from "./pointerPiano";

function rig() {
  const sent: string[] = [];
  const piano = new PointerPiano({
    noteOn: (note, velocity) => sent.push(`on ${String(note)} @${String(velocity)}`),
    noteOff: (note) => sent.push(`off ${String(note)}`),
  });
  return { piano, sent };
}

describe("a pointer on the keys", () => {
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
