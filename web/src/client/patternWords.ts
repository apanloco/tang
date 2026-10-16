// What the page says of a pattern, in words: how long it is, how many notes it has, what a recording that is going on is doing, and what the button that starts one does.
import { PATTERN_COUNT_IN_BEATS } from "../protocol";
import type { PatternInfo } from "../protocol";

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
