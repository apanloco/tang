// Where the keys of the piano are (docs/web-ui.md §9.5): which octaves there is room for, which of them the page shows, what is on
// each key, and what each key is in the scale. Pure: the component only draws what these say.
import { noteName } from "./notes";

/** The pitch classes that are the white keys of a piano. */
const WHITE_CLASSES: readonly number[] = [0, 2, 4, 5, 7, 9, 11];

/** The narrowest a white key is drawn, in pixels: narrower keys are not ones a pointer can play. */
export const MIN_WHITE_KEY = 30;

/** The most octaves the page shows at once. */
export const MAX_OCTAVES = 6;

/** How many blocks of twelve notes the notes of MIDI reach into: 0 to 127 is ten whole blocks and eight notes of an eleventh. */
const BLOCKS = 11;

/** How many octaves, whole ones, fit in `width` pixels with no key narrower than `MIN_WHITE_KEY`: one at least, `MAX_OCTAVES` at most. */
export function octavesFor(width: number): number {
  const fit = Math.floor(width / (7 * MIN_WHITE_KEY));
  if (Number.isNaN(fit)) return 1;
  return Math.min(MAX_OCTAVES, Math.max(1, fit));
}

/**
 * The first of the `octaves` blocks of twelve notes (block `n` has the notes 12n to 12n + 11, from a C) that the page shows when the
 * keys of the computer play from the C of block `octave`: the window is centred on what they play (an octave and a bit more of the
 * next, 29 semitones from the C), and it stays within the notes there are.
 */
export function firstBlock(octave: number, octaves: number): number {
  return Math.min(BLOCKS - octaves, Math.max(0, octave + 1 - Math.floor(octaves / 2)));
}

/** Whether `note` is a white key. */
export const isWhite = (note: number): boolean => WHITE_CLASSES.includes(note % 12);

/** What a key says on it: the letter of a white key, and the octave as well on a C (`C4`); a black key says nothing. */
export function keyLabel(note: number): string {
  if (!isWhite(note)) return "";
  return note % 12 === 0 ? noteName(note) : noteName(note).replace(/-?\d+$/, "");
}

/** The keys of a window of the piano. */
export interface KeyboardLayout {
  /** The white keys, left to right, by note. */
  whites: number[];
  /** The black keys by note, and how many white keys come before each: it is centred on the boundary after them. */
  blacks: { note: number; after: number }[];
}

/** The keys of `octaves` blocks of twelve notes from block `first`, as far as the last note there is, 127. */
export function keyboardLayout(first: number, octaves: number): KeyboardLayout {
  const whites: number[] = [];
  const blacks: { note: number; after: number }[] = [];
  const end = Math.min(128, (first + octaves) * 12);
  for (let note = first * 12; note < end; note += 1) {
    if (isWhite(note)) whites.push(note);
    else blacks.push({ note, after: whites.length });
  }
  return { whites, blacks };
}

/** What a key is in the scale: its root, another of its notes, or not one of them. */
export type KeyRole = "root" | "scale" | "off";

/** What `note` is in a scale with this `root` and these `notes` (pitch classes, 0 C to 11 B, as the server tells them). */
export function roleOf(note: number, root: number, notes: readonly number[]): KeyRole {
  const pitchClass = note % 12;
  if (pitchClass === root) return "root";
  return notes.includes(pitchClass) ? "scale" : "off";
}
