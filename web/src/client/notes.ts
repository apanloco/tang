// The names of the notes, as the TUI writes them: the letter, a sharp if it has one, and the octave, which starts at C, so
// that 60 is the middle C, C4 (`note_name` in `src/main.rs`; the range of a lane is written the same in the session file).
import type { NoteRange } from "../protocol";
import { SCALE_ROOTS } from "../protocol";

/** The name of MIDI note `note` (0 to 127): 0 is C-1, 60 is C4, 127 is G9. (The letters are the server's twelve, `SCALE_ROOTS`, as the scales' are.) */
export const noteName = (note: number): string => `${SCALE_ROOTS[note % 12] ?? "?"}${String(Math.floor(note / 12) - 1)}`;

/** All the notes of the keyboard by name, at the place of their number. */
export const NOTE_NAMES: readonly string[] = Array.from({ length: 128 }, (_, note) => noteName(note));

/**
 * What a lane's keys are called: both ends as the session file writes them (`C2-C6`), one for an end that is open (`-B3`,
 * `C4-`), and `all keys` for no range, or for the whole keyboard, which is the same.
 */
export function rangeLabel(range: NoteRange | null): string {
  if (range === null || (range.low === 0 && range.high === 127)) return "all keys";
  if (range.low === 0) return `-${noteName(range.high)}`;
  if (range.high === 127) return `${noteName(range.low)}-`;
  return `${noteName(range.low)}-${noteName(range.high)}`;
}
