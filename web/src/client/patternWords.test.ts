import { describe, expect, test } from "vitest";

import { PATTERN_COUNT_IN_BEATS } from "../protocol";
import type { PatternInfo } from "../protocol";
import { beatsWords, notesWords, patternLine, recordHint } from "./patternWords";

const pattern = (over: Partial<PatternInfo> = {}): PatternInfo => ({ recording: false, enabled: true, looping: true, inKey: false, lengthBeats: 4, bpm: 120, notes: 3, ...over });

describe("beats in words", () => {
  test("are a number and a beat or beats, for a length that is whole or has a fraction", () => {
    expect([1, 2, 4, 32].map(beatsWords)).toEqual(["1 beat", "2 beats", "4 beats", "32 beats"]);
    expect([2.5, 0.5, 1.25].map(beatsWords)).toEqual(["2.5 beats", "0.5 beats", "1.25 beats"]);
  });
});

describe("notes in words", () => {
  test("are none, one note, or a number of notes", () => {
    expect([0, 1, 2, 17].map(notesWords)).toEqual(["no notes", "1 note", "2 notes", "17 notes"]);
  });
});

describe("the line that says what a pattern is", () => {
  test("says that a lane has none, while it has none", () => {
    expect(patternLine(null, 120)).toBe("No pattern yet");
  });

  test("says how long a pattern is, how many notes it has and the tempo it was recorded at, which is its own and not the session's", () => {
    expect(patternLine(pattern({ lengthBeats: 2, notes: 3, bpm: 90 }), 120)).toBe("2 beats · 3 notes · 90 BPM");
    expect(patternLine(pattern({ lengthBeats: 1, notes: 1, bpm: 96.5 }), 120)).toBe("1 beat · 1 note · 96.5 BPM");
  });

  test("says that a pattern has no notes when it has none", () => {
    expect(patternLine(pattern({ notes: 0, enabled: false }), 120)).toBe("4 beats · no notes · 120 BPM");
  });

  test("says what a recording that is going on is doing: that it counts in, and then the beats at the tempo the session has, which is the one it is recorded at", () => {
    expect(patternLine(pattern({ recording: true, lengthBeats: 4, bpm: 90 }), 120)).toBe("Counting in, then 4 beats at 120 BPM: play now");
    expect(patternLine(pattern({ recording: true, lengthBeats: 1 }), 300)).toBe("Counting in, then 1 beat at 300 BPM: play now");
  });
});

describe("what the record button says it does", () => {
  test("is the count-in, the length, and the tempo of the session", () => {
    expect(recordHint(4, 120)).toBe(`Record a pattern: ${String(PATTERN_COUNT_IN_BEATS)} beats are counted in, then the notes you play for 4 beats at 120 BPM are the pattern`);
    expect(recordHint(1, 96.5)).toBe(`Record a pattern: ${String(PATTERN_COUNT_IN_BEATS)} beats are counted in, then the notes you play for 1 beat at 96.5 BPM are the pattern`);
  });
});
