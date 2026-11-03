// The scales of the piano. The server tells a page the notes of its scale (`PianoMeta.notes`) and the page never works them
// out; this is for what stands in for the server, the mock engine, which has to answer a `setScale` as the controller does
// (`ScaleSetting::parse` in `src/scale.rs`): the roots and the kinds are the server's lists (`SCALE_KINDS`, `SCALE_ROOTS`).
import { SCALE_KINDS, SCALE_ROOTS } from "../protocol";

/** What the server tells of a scale: how it is saved, its root and kind, and its notes. */
export interface PianoScale {
  /** The root, a space and the short name of the kind: `F# dorian`. */
  scale: string;
  /** The pitch class of the root: 0 C to 11 B. */
  root: number;
  /** The short name of the kind of scale. */
  kind: string;
  /** The pitch classes of the notes, lowest first. */
  notes: number[];
}

/** The roots as a person may write them: the sharps first (`C#` is not `C` and something), then the flats, then the letters. */
const ROOTS: readonly (readonly [string, number])[] = [
  ...SCALE_ROOTS.flatMap((name, pitchClass): [string, number][] => (name.length === 2 ? [[name, pitchClass]] : [])),
  ...Object.entries({ Db: 1, Eb: 3, Gb: 6, Ab: 8, Bb: 10 }),
  ...SCALE_ROOTS.flatMap((name, pitchClass): [string, number][] => (name.length === 1 ? [[name, pitchClass]] : [])),
];

/**
 * The scale that `text` names, as the controller reads it: a root (a letter, with a sharp or a flat) and a kind (its short name
 * or its long one), in any case, with spaces round them; or null if it is not one.
 */
export function parseScale(text: string): PianoScale | null {
  const trimmed = text.trim();
  const found = ROOTS.find(([name]) => trimmed.slice(0, name.length).toLowerCase() === name.toLowerCase());
  if (!found) return null;
  const [name, root] = found;
  const rest = trimmed.slice(name.length).trim().toLowerCase();
  const kind = SCALE_KINDS.find((candidate) => candidate.short.toLowerCase() === rest || candidate.name.toLowerCase() === rest);
  if (!kind) return null;
  const notes = [...new Set(kind.intervals.map((interval) => (root + interval) % 12))].sort((a, b) => a - b);
  return { scale: `${SCALE_ROOTS[root] ?? "?"} ${kind.short}`, root, kind: kind.short, notes };
}
