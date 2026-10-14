import { describe, expect, test } from "vitest";

import { NoteHolds } from "./holds";

/** The notes that went to the sink, as `on 60 @100` and `off 60`, in order. */
function sink() {
  const sent: string[] = [];
  return {
    sent,
    noteOn: (note: number, velocity?: number) => sent.push(`on ${String(note)} @${String(velocity)}`),
    noteOff: (note: number) => sent.push(`off ${String(note)}`),
  };
}

describe("the notes the page holds", () => {
  test("start when the first player takes a note, and end when the last one lets go of it", () => {
    const out = sink();
    const holds = new NoteHolds(out);
    holds.noteOn(60, 100);
    holds.noteOn(60, 100);
    expect(out.sent).toEqual(["on 60 @100"]);
    holds.noteOff(60);
    expect(out.sent).toEqual(["on 60 @100"]);
    holds.noteOff(60);
    expect(out.sent).toEqual(["on 60 @100", "off 60"]);
  });

  test("are each their own: another note neither starts nor ends with this one", () => {
    const out = sink();
    const holds = new NoteHolds(out);
    holds.noteOn(60, 100);
    holds.noteOn(64, 90);
    holds.noteOff(60);
    expect(out.sent).toEqual(["on 60 @100", "on 64 @90", "off 60"]);
  });

  test("start again when they are taken again after the last player has let go", () => {
    const out = sink();
    const holds = new NoteHolds(out);
    holds.noteOn(60, 100);
    holds.noteOff(60);
    holds.noteOn(60, 80);
    holds.noteOff(60);
    expect(out.sent).toEqual(["on 60 @100", "off 60", "on 60 @80", "off 60"]);
  });

  test("end for no player that did not take them: a note that is not held has nothing to end", () => {
    const out = sink();
    const holds = new NoteHolds(out);
    holds.noteOff(60);
    expect(out.sent).toEqual([]);
    // (And it does not leave the note owing a release: the next player takes it as the first.)
    holds.noteOn(60, 100);
    holds.noteOff(60);
    expect(out.sent).toEqual(["on 60 @100", "off 60"]);
  });

  test("go to the sink with the velocity of the one that started them", () => {
    const out = sink();
    const holds = new NoteHolds(out);
    holds.noteOn(60);
    expect(out.sent).toEqual(["on 60 @undefined"]);
  });
});
