import { describe, expect, test } from "vitest";

import { NOTE_NAMES, noteName, rangeLabel } from "./notes";

describe("the name of a note", () => {
  test("is the letter, a sharp if it has one, and the octave, which starts at C: 60 is the middle C, C4, as in the TUI", () => {
    expect(noteName(60)).toBe("C4");
    expect(noteName(61)).toBe("C#4");
    expect(noteName(71)).toBe("B4");
    expect(noteName(72)).toBe("C5");
    expect(noteName(21)).toBe("A0");
    expect(noteName(0)).toBe("C-1");
    expect(noteName(11)).toBe("B-1");
    expect(noteName(127)).toBe("G9");
  });

  test("is in a list of the whole keyboard, 128 of them, at the place of its number", () => {
    expect(NOTE_NAMES).toHaveLength(128);
    expect(NOTE_NAMES[0]).toBe("C-1");
    expect(NOTE_NAMES[60]).toBe("C4");
    expect(NOTE_NAMES[127]).toBe("G9");
    expect(NOTE_NAMES.every((name, note) => name === noteName(note))).toBe(true);
    expect(new Set(NOTE_NAMES).size).toBe(128);
  });
});

describe("the label of a range of keys", () => {
  test("is both ends, as the session file writes them", () => {
    expect(rangeLabel({ low: 36, high: 84 })).toBe("C2-C6");
    expect(rangeLabel({ low: 60, high: 60 })).toBe("C4-C4");
  });

  test("is one end for a range that is open at the other, as the file writes it", () => {
    expect(rangeLabel({ low: 0, high: 59 })).toBe("-B3");
    expect(rangeLabel({ low: 60, high: 127 })).toBe("C4-");
  });

  test("is all keys for none, and for the whole keyboard, which is the same", () => {
    expect(rangeLabel(null)).toBe("all keys");
    expect(rangeLabel({ low: 0, high: 127 })).toBe("all keys");
  });
});
