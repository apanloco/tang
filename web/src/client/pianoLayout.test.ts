import { describe, expect, test } from "vitest";

import { firstBlock, isWhite, keyboardLayout, keyLabel, MAX_OCTAVES, MIN_WHITE_KEY, octavesFor, roleOf } from "./pianoLayout";

describe("how many octaves fit", () => {
  const octave = 7 * MIN_WHITE_KEY;

  test("is as many whole octaves as there is room for at the narrowest a white key is drawn", () => {
    expect(octavesFor(octave * 2)).toBe(2);
    expect(octavesFor(octave * 2 - 1)).toBe(1);
    expect(octavesFor(octave * 4 + 5)).toBe(4);
  });

  test("is at least one, whatever the width (a width that is not known yet is 0)", () => {
    for (const width of [0, 1, octave - 1, octave, Number.NaN, -5]) expect(octavesFor(width), String(width)).toBe(1);
  });

  test("is at most as many as a page shows", () => {
    expect(MAX_OCTAVES).toBeGreaterThanOrEqual(3);
    expect(octavesFor(octave * MAX_OCTAVES)).toBe(MAX_OCTAVES);
    expect(octavesFor(octave * MAX_OCTAVES * 3)).toBe(MAX_OCTAVES);
    expect(octavesFor(Number.POSITIVE_INFINITY)).toBe(MAX_OCTAVES);
  });
});

describe("where the window of octaves is", () => {
  test("has the notes the keys of the computer play in the middle of it", () => {
    // The computer's keys play from the C of octave n (note 12n) for 29 semitones. With 5 octaves shown, the window starts one before.
    expect(firstBlock(4, 5)).toBe(3);
    expect(firstBlock(4, 3)).toBe(4);
    expect(firstBlock(4, 1)).toBe(5);
    expect(firstBlock(4, 6)).toBe(2);
  });

  test("stays on the keyboard: it starts at the lowest C, and ends at the top of the notes", () => {
    expect(firstBlock(0, 5)).toBe(0);
    expect(firstBlock(0, 6)).toBe(0);
    for (let octaves = 1; octaves <= MAX_OCTAVES; octaves += 1) {
      expect(firstBlock(8, octaves), `${String(octaves)} octaves`).toBe(Math.min(11 - octaves, 9 - Math.floor(octaves / 2)));
      for (let octave = 0; octave <= 8; octave += 1) {
        const first = firstBlock(octave, octaves);
        expect(first, `${String(octave)}, ${String(octaves)}`).toBeGreaterThanOrEqual(0);
        expect(first + octaves, `${String(octave)}, ${String(octaves)}`).toBeLessThanOrEqual(11);
      }
    }
  });
});

describe("the keys that are drawn", () => {
  test("are the seven white keys and the five black ones of each octave, from its C", () => {
    const one = keyboardLayout(5, 1);
    expect(one.whites).toEqual([60, 62, 64, 65, 67, 69, 71]);
    // A black key is at a boundary between two white keys: after as many white keys as come before it.
    expect(one.blacks).toEqual([
      { note: 61, after: 1 },
      { note: 63, after: 2 },
      { note: 66, after: 4 },
      { note: 68, after: 5 },
      { note: 70, after: 6 },
    ]);
  });

  test("go on from octave to octave, the black keys counting the white keys of the octaves before", () => {
    const two = keyboardLayout(5, 2);
    expect(two.whites).toHaveLength(14);
    expect(two.whites.at(-1)).toBe(83);
    expect(two.blacks).toHaveLength(10);
    expect(two.blacks[5]).toEqual({ note: 73, after: 8 });
  });

  test("stop at the last note there is, 127", () => {
    const top = keyboardLayout(10, 1);
    expect(top.whites).toEqual([120, 122, 124, 125, 127]);
    expect(top.blacks).toEqual([
      { note: 121, after: 1 },
      { note: 123, after: 2 },
      { note: 126, after: 4 },
    ]);
  });

  test("every note of the window is on a key, once, and every black key is between two white ones", () => {
    for (const [first, octaves] of [[0, 3], [3, 4], [5, 6], [10, 1]] as const) {
      const { whites, blacks } = keyboardLayout(first, octaves);
      const notes = [...whites, ...blacks.map((black) => black.note)].sort((a, b) => a - b);
      const expected = Array.from({ length: Math.min(128, (first + octaves) * 12) - first * 12 }, (_, i) => first * 12 + i);
      expect(notes, `${String(first)}, ${String(octaves)}`).toEqual(expected);
      for (const { note, after } of blacks) {
        expect(whites[after - 1], `below ${String(note)}`).toBe(note - 1);
        expect(whites[after], `above ${String(note)}`).toBe(note + 1);
      }
    }
  });
});

describe("a white key", () => {
  test("is a note whose pitch class has no sharp or flat", () => {
    expect([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11].map((pitchClass) => isWhite(60 + pitchClass))).toEqual([
      true, false, true, false, true, true, false, true, false, true, false, true,
    ]);
    expect(isWhite(127)).toBe(true);
    expect(isWhite(0)).toBe(true);
  });
});

describe("what a key is in the scale", () => {
  const cMajor = [0, 2, 4, 5, 7, 9, 11];
  const fSharpDorian = [1, 3, 4, 6, 8, 9, 11];

  test("is the root where the pitch class is the root's, in every octave", () => {
    expect([36, 48, 60, 72, 127 - 7].map((note) => roleOf(note, 0, cMajor))).toEqual(["root", "root", "root", "root", "root"]);
    expect(roleOf(66, 6, fSharpDorian)).toBe("root");
    expect(roleOf(61, 6, fSharpDorian)).toBe("scale");
  });

  test("is a note of the scale where the pitch class is one of its notes, and off it where it is not", () => {
    expect([60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71].map((note) => roleOf(note, 0, cMajor))).toEqual([
      "root", "off", "scale", "off", "scale", "scale", "off", "scale", "off", "scale", "off", "scale",
    ]);
    expect(roleOf(62, 6, fSharpDorian)).toBe("off");
  });

  test("is off the scale when the scale is not known", () => {
    expect(roleOf(60, 0, [])).toBe("root");
    expect(roleOf(62, 0, [])).toBe("off");
  });
});

describe("what a key says", () => {
  test("is its letter, and its octave as well on a C, the way the TUI names notes", () => {
    expect([48, 50, 52, 53, 55, 57, 59, 60, 62].map(keyLabel)).toEqual(["C3", "D", "E", "F", "G", "A", "B", "C4", "D"]);
    expect([keyLabel(0), keyLabel(12), keyLabel(120), keyLabel(127)]).toEqual(["C-1", "C0", "C9", "G"]);
    // (The letters of the notes of the lowest octave are letters: the octave there is -1.)
    expect([keyLabel(2), keyLabel(11), keyLabel(5)]).toEqual(["D", "B", "F"]);
  });

  test("is nothing on a black key, which is too narrow to say it", () => {
    expect([49, 51, 54, 56, 58, 61].map(keyLabel)).toEqual(["", "", "", "", "", ""]);
  });
});
