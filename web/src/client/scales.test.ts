import { describe, expect, test } from "vitest";

import { SCALE_KINDS, SCALE_ROOTS } from "../protocol";
import { parseScale } from "./scales";

// The cases of `ScaleSetting::parse` and `pitch_classes` in `src/scale.rs` and `src/app/apply.rs`: what stands in for the
// server has to answer as it does.
describe("a scale as the controller reads it", () => {
  test("is a root and a kind, as a session file says them, with the notes it has", () => {
    expect(parseScale("C major")).toEqual({ scale: "C major", root: 0, kind: "major", notes: [0, 2, 4, 5, 7, 9, 11] });
    // F# dorian (F# G# A B C# D# E) goes round the octave: the notes are lowest first, not from the root.
    expect(parseScale("F# dorian")).toEqual({ scale: "F# dorian", root: 6, kind: "dorian", notes: [1, 3, 4, 6, 8, 9, 11] });
    expect(parseScale("A min pent")?.notes).toEqual([0, 2, 4, 7, 9]);
    expect(parseScale("G# chromatic")?.notes).toEqual([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
  });

  test("takes flats, long names, any case and spaces round it, and says it as it is saved", () => {
    expect(parseScale("Bb Natural Minor")).toEqual({ scale: "A# minor", root: 10, kind: "minor", notes: [0, 1, 3, 5, 6, 8, 10] });
    expect(parseScale("db MAJOR")).toEqual({ scale: "C# major", root: 1, kind: "major", notes: [0, 1, 3, 5, 6, 8, 10] });
    expect(parseScale("  e  blues ")).toEqual({ scale: "E blues", root: 4, kind: "blues", notes: [2, 4, 7, 9, 10, 11] });
    expect(parseScale("G# maj pent")).toEqual({ scale: "G# maj pent", root: 8, kind: "maj pent", notes: [0, 3, 5, 8, 10] });
  });

  test("is not a scale when it is not a root and a kind of scale", () => {
    for (const text of ["", "banana", "C", "C wibble", "H major", "major", "Cé", "日本語"]) {
      expect(parseScale(text), JSON.stringify(text)).toBeNull();
    }
  });

  test("has every root with every kind: each is what it is said to be, and comes back as it went", () => {
    for (const [root, name] of SCALE_ROOTS.entries()) {
      for (const kind of SCALE_KINDS) {
        const scale = `${name} ${kind.short}`;
        const parsed = parseScale(scale);
        expect(parsed?.scale, scale).toBe(scale);
        expect([parsed?.root, parsed?.kind], scale).toEqual([root, kind.short]);
        const notes = parsed?.notes ?? [];
        expect(notes, scale).toEqual([...notes].sort((a, b) => a - b));
        expect(new Set(notes).size, scale).toBe(notes.length);
        expect(notes, scale).toContain(root);
        expect(parseScale(`${name} ${kind.name}`)?.scale, `${name} ${kind.name}`).toBe(scale);
      }
    }
  });
});
