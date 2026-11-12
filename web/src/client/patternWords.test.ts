import { describe, expect, test } from "vitest";

import { PATTERN_COUNT_IN_BEATS } from "../protocol";
import type { PatternInfo, PatternNote, PatternStatus } from "../protocol";
import { BOX_FINISH_HINT, beatsWords, boxLine, boxPlayHint, boxRecordHint, boxStopHint, isRecording, notesWords, patternLine, recordHint } from "./patternWords";

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

// The words of a pattern box (step 124 C): what it is doing, and what each of its buttons does for a person who has not used it.
const NOTE: PatternNote = { start: 0, length: 1, note: 60, velocity: 100 };
const box = (over: { status?: PatternStatus; lengthBeats?: number; overdub?: boolean; sync?: "own" | "host"; notes?: PatternNote[] } = {}) => ({
  status: "stopped" as PatternStatus,
  lengthBeats: 4,
  overdub: false,
  sync: "own" as const,
  notes: [NOTE, NOTE, NOTE],
  ...over,
});

describe("whether a box is recording", () => {
  test("is so while it counts in, waits for the host's loop or makes a pass, and not while it plays or is stopped", () => {
    const statuses: PatternStatus[] = ["stopped", "playing", "countingIn", "waiting", "recording"];
    expect(statuses.map(isRecording)).toEqual([false, false, true, true, true]);
  });
});

describe("the line that says what a box is doing", () => {
  test("says how long a box is and how many notes it has while it is stopped, and that it has none", () => {
    expect(boxLine(box())).toBe("4 beats · 3 notes");
    expect(boxLine(box({ notes: [], lengthBeats: 1 }))).toBe("1 beat · no notes");
    expect(boxLine(box({ notes: [NOTE], lengthBeats: 2.5 }))).toBe("2.5 beats · 1 note");
  });

  test("says that a box plays, with what it has", () => {
    expect(boxLine(box({ status: "playing" }))).toBe("Playing · 4 beats · 3 notes");
  });

  test("says that a box counts in, and for how long the pass is that follows, which is the length it has now", () => {
    expect(boxLine(box({ status: "countingIn" }))).toBe("Counting in, then 4 beats: play now");
    expect(boxLine(box({ status: "countingIn", lengthBeats: 8 }))).toBe("Counting in, then 8 beats: play now");
  });

  test("says that a box makes a pass and for how long, and that an overdub goes on until it is finished", () => {
    expect(boxLine(box({ status: "recording", lengthBeats: 2 }))).toBe("Recording 2 beats: play now");
    expect(boxLine(box({ status: "recording", overdub: true }))).toBe("Recording on top of the notes: Finish ends it");
  });

  test("says that a box waits for the start of the host's loop", () => {
    expect(boxLine(box({ status: "waiting", sync: "host" }))).toBe("Waiting for the host's loop to begin, then recording");
  });
});

describe("what the buttons of a box say they do", () => {
  test("record begins with a count-in on the box's own clock and with the host's loop on the host's, and says what becomes of the notes the box has", () => {
    expect(boxRecordHint(box())).toBe(`Record into the box: after ${beatsWords(PATTERN_COUNT_IN_BEATS)} are counted in, the notes you play for 4 beats replace the ones the box has`);
    expect(boxRecordHint(box({ sync: "host", lengthBeats: 8 }))).toBe("Record into the box: when the host's loop next begins, the notes you play for 8 beats replace the ones the box has");
    expect(boxRecordHint(box({ overdub: true, lengthBeats: 2 }))).toBe(
      `Record into the box: after ${beatsWords(PATTERN_COUNT_IN_BEATS)} are counted in, the notes you play are added to the ones the box has, pass after pass of 2 beats until you finish`,
    );
  });

  test("finish says that the pass that goes on is the last, and that a count-in that is finished leaves the box as it was", () => {
    expect(BOX_FINISH_HINT).toContain("the pass that is going on is the last");
    expect(BOX_FINISH_HINT).toContain("leaves the box as it was");
  });

  test("stop says that a pass that is being recorded is lost while one is, and that it stops playing when it plays", () => {
    for (const status of ["countingIn", "waiting", "recording"] as const) expect(boxStopHint(status), status).toBe("Stop: the pass that is being recorded is lost, and the box keeps the notes it had");
    expect(boxStopHint("playing")).toBe("Stop playing");
  });

  test("play says how long the box is on its own clock and that it is silent while the host is stopped on the host's", () => {
    expect(boxPlayHint(box({ lengthBeats: 8 }))).toBe("Play the box from its start: 8 beats, round and round if it loops");
    expect(boxPlayHint(box({ sync: "host" }))).toBe("Play the box with the host's beat (it is silent while the host is stopped)");
  });
});
