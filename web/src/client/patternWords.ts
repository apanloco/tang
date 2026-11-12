// What the page says of a pattern, in words: how long it is, how many notes it has, what a recording that is going on is doing, and what the button that starts one does. The pattern box's words are at the foot
// (step 124 C): what a box is doing, and what each of its buttons does for a person who has not used it.
import { PATTERN_COUNT_IN_BEATS } from "../protocol";
import type { PatternInfo, PatternStatus } from "../protocol";
import type { PatternBoxNode } from "./patternBoxes";

/** "1 beat", "4 beats", "2.5 beats". */
export const beatsWords = (beats: number): string => `${String(beats)} ${beats === 1 ? "beat" : "beats"}`;

/** "no notes", "1 note", "3 notes". */
export const notesWords = (notes: number): string => (notes === 0 ? "no notes" : `${String(notes)} ${notes === 1 ? "note" : "notes"}`);

/**
 * The line under a lane's pattern controls: that it has none; or how long it is, how many notes it has and the tempo it was recorded at (its own: a pattern keeps it when the session's
 * tempo is changed); or, while a recording is going on, that it counts in and then how long it takes at the tempo the session has, which is the one the recording is made at (the line is
 * as short as the node is wide: how many beats are counted in is in the hint of the button that records).
 */
export function patternLine(pattern: PatternInfo | null, sessionBpm: number): string {
  if (pattern === null) return "No pattern yet";
  if (pattern.recording) return `Counting in, then ${beatsWords(pattern.lengthBeats)} at ${String(sessionBpm)} BPM: play now`;
  return `${beatsWords(pattern.lengthBeats)} · ${notesWords(pattern.notes)} · ${String(pattern.bpm)} BPM`;
}

/** What the button that starts a recording says it does, for a person who has not used it: it counts in, and then takes the notes that are played for as long as the pattern is. */
export const recordHint = (lengthBeats: number, sessionBpm: number): string =>
  `Record a pattern: ${beatsWords(PATTERN_COUNT_IN_BEATS)} are counted in, then the notes you play for ${beatsWords(lengthBeats)} at ${String(sessionBpm)} BPM are the pattern`;

/** What a box needs of itself for its words: how long it loops, whether a recording is added to its notes, whose clock it goes by, its notes and what it is doing. */
type BoxWords = Pick<PatternBoxNode, "lengthBeats" | "overdub" | "sync" | "notes" | "status">;

/** Whether a box is recording: counting in, waiting for the start of the host's loop, or making a pass. */
export const isRecording = (status: PatternStatus): boolean => status === "countingIn" || status === "waiting" || status === "recording";

/**
 * The line under a pattern box's controls: how long it is and how many notes it has; and what it is doing when it is doing something: playing, counting in (and how long the pass is then), making a pass (a recording that
 * overdubs goes on with pass after pass until it is finished), or waiting for the host's loop to begin. As short as the node is wide: what each button does is in its hint.
 */
export function boxLine(box: BoxWords): string {
  const held = `${beatsWords(box.lengthBeats)} · ${notesWords(box.notes.length)}`;
  switch (box.status) {
    case "stopped":
      return held;
    case "playing":
      return `Playing · ${held}`;
    case "countingIn":
      return `Counting in, then ${beatsWords(box.lengthBeats)}: play now`;
    case "waiting":
      return "Waiting for the host's loop to begin, then recording";
    case "recording":
      return box.overdub ? "Recording on top of the notes: Finish ends it" : `Recording ${beatsWords(box.lengthBeats)}: play now`;
  }
}

/** What the button that starts a recording in a box says it does: how it begins (a count-in, or the host's loop), and what becomes of the notes it has. */
export function boxRecordHint(box: BoxWords): string {
  const begins = box.sync === "host" ? "when the host's loop next begins" : `after ${beatsWords(PATTERN_COUNT_IN_BEATS)} are counted in`;
  const takes = box.overdub
    ? `the notes you play are added to the ones the box has, pass after pass of ${beatsWords(box.lengthBeats)} until you finish`
    : `the notes you play for ${beatsWords(box.lengthBeats)} replace the ones the box has`;
  return `Record into the box: ${begins}, ${takes}`;
}

/** What the button that finishes a recording says it does, while one is going on. */
export const BOX_FINISH_HINT = "Finish recording: the pass that is going on is the last (a count-in that is finished leaves the box as it was)";

/** What the button that stops a box says it does, for what the box is doing: a pass that is being recorded is lost. */
export const boxStopHint = (status: PatternStatus): string =>
  isRecording(status) ? "Stop: the pass that is being recorded is lost, and the box keeps the notes it had" : "Stop playing";

/** What the button that plays a box says it does: from the start of its loop, on its own clock or the host's. */
export const boxPlayHint = (box: Pick<BoxWords, "sync" | "lengthBeats">): string =>
  box.sync === "host" ? `Play the box with the host's beat (it is silent while the host is stopped)` : `Play the box from its start: ${beatsWords(box.lengthBeats)}, round and round if it loops`;
