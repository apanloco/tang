// What a pattern box's roll draws (docs/patch-graph.md §11.6, P4h-C part C): its notes as marks on a grid, the beats across and the semitones up, as far as the notes go. A picture and not a place to edit: the roll that is
// edited is slice 2. The geometry is made here, as plain numbers, so that it can be looked at without a browser; `app/patch/PatternRoll.tsx` draws it.
import type { PatternNote } from "../protocol";

/** The fewest semitones a roll is high: a box with one note, or with notes a few semitones apart, is not drawn with each as tall as the roll. */
export const ROLL_MIN_SPAN = 12;

/** The shortest a note is drawn, in beats: a note that has no length (a recording leaves none, and a file may) is still a mark, and one that is a hair long is one that can be seen. */
export const ROLL_MIN_WIDTH = 1 / 16;

/** The gap between a note and the one above it, as a part of a semitone, so that notes a semitone apart are two marks. */
const GAP = 0.15;

/** A note as a mark: where its top left is, in beats and in semitones from the top of the roll, how long it is, and how hard it was played, as the opacity of the mark (from 0.4, a soft note, to 1). */
export interface RollNote {
  x: number;
  y: number;
  width: number;
  height: number;
  opacity: number;
}

/** A line at a beat, across the roll: a bar's (a multiple of four) is drawn heavier than the others. */
export interface RollLine {
  at: number;
  bar: boolean;
}

/** What a roll draws: its size in beats and in semitones (the box of its picture), the marks of its notes and the lines of its beats. */
export interface Roll {
  beats: number;
  span: number;
  notes: RollNote[];
  lines: RollLine[];
}

/** How far apart the lines of the beats are, in beats: one for a loop that is short enough to tell them apart, a bar for a longer one, and four bars for the longest (the picture is about 280 pixels wide). */
export function lineEvery(length: number): number {
  return length <= 32 ? 1 : length <= 128 ? 4 : 16;
}

/**
 * The roll of `notes` in a loop of `length` beats: the notes in the order they are given, each drawn from its start for as long as it is (at least `ROLL_MIN_WIDTH`), at its pitch's height: the highest pitch that is
 * played is at the top and the lowest at the bottom, with the room that makes `ROLL_MIN_SPAN` semitones above and below them, in the middle. A note that begins past the end of the loop is not in it (the box does not play it
 * either); one that goes past the end is drawn to it.
 */
export function rollOf(notes: readonly PatternNote[], length: number): Roll {
  const inside = notes.filter((note) => note.start < length);
  const pitches = inside.map((note) => note.note);
  const low = inside.length > 0 ? Math.min(...pitches) : 0;
  const high = inside.length > 0 ? Math.max(...pitches) : 0;
  const used = inside.length > 0 ? high - low + 1 : 0;
  const span = Math.max(used, ROLL_MIN_SPAN);
  const top = high + 1 + (span - used) / 2;
  const marks = inside.map((note): RollNote => {
    const width = Math.min(Math.max(note.length, ROLL_MIN_WIDTH), length - note.start);
    return { x: note.start, y: top - note.note - 1 + GAP / 2, width, height: 1 - GAP, opacity: 0.4 + (0.6 * Math.min(Math.max(note.velocity, 0), 127)) / 127 };
  });
  const every = lineEvery(length);
  const lines: RollLine[] = [];
  for (let at = every; at < length; at += every) lines.push({ at, bar: at % 4 === 0 });
  return { beats: length, span, notes: marks, lines };
}
