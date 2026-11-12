// Pattern boxes as the mock engine and the page read them (docs/patch-graph.md §11.6, P4h-C): a node that stores notes in beats and plays them as a source of notes of its own. What a box is when it is new and what it is called
// (`newBox`, `nextBoxName`, as `PatternBox::new` and `SessionModel::next_pattern_name`), the name that a person gives it (`patternTitle`, as `pattern_title`), the length and the notes as a box holds them (`clampLength` and `sanitize`,
// as `BoxPlan::clamp_length` and `patternbox::sanitize`), and the words of the steps that its settings make in the history (`boxFieldBefore`, as `pattern_field_before`).
import { PATTERN_BOX_DEFAULT_LENGTH, PATTERN_BOX_LENGTH_RANGE, PATTERN_BOX_NOTE_LIMIT } from "../protocol";
import type { Node, NodeId, PatternBoxField, PatternNote } from "../protocol";
import { tidyName } from "./modules";

/** A pattern box, as the graph has it. */
export type PatternBoxNode = Extract<Node, { kind: "pattern" }>;

/** What a box is called when it has no name of its own (`DEFAULT_PATTERN_NAME`): what a name of nothing comes to. */
const DEFAULT_NAME = "Pattern";

/** The shortest and the longest loop that a box has, in beats (`MIN_LENGTH_BEATS`, `MAX_LENGTH_BEATS`). */
const [MIN_LENGTH, MAX_LENGTH] = PATTERN_BOX_LENGTH_RANGE;

/** The length of a loop as a box holds it: the shortest to the longest there can be, and the shortest for a number that is none (`BoxPlan::clamp_length`). */
export function clampLength(beats: number): number {
  return Number.isFinite(beats) ? Math.min(Math.max(beats, MIN_LENGTH), MAX_LENGTH) : MIN_LENGTH;
}

/** A note as the box holds it, or none if it is not one: a pitch above 127 or a number that is not finite is left out, a velocity is brought into 1 to 127, a start and a length below 0 to 0 (`BoxPlan::tidy`). */
function tidy(note: PatternNote): PatternNote | null {
  if (!Number.isFinite(note.start) || !Number.isFinite(note.length) || !Number.isInteger(note.note) || note.note < 0 || note.note > 127) return null;
  return { start: Math.max(note.start, 0), length: Math.max(note.length, 0), note: note.note, velocity: Math.min(Math.max(note.velocity, 1), 127) };
}

/** How the notes of a box are put in order: by start, then pitch, then length and velocity (`note_order`). */
const noteOrder = (a: PatternNote, b: PatternNote): number => a.start - b.start || a.note - b.note || a.length - b.length || a.velocity - b.velocity;

/**
 * The notes as a box holds them (`patternbox::sanitize`): what is not a note left out, the first `PATTERN_BOX_NOTE_LIMIT` of what is, each brought into range, in the order of their starts. The session keeps its notes so,
 * whoever gives them.
 */
export function sanitize(notes: readonly PatternNote[]): PatternNote[] {
  return notes
    .flatMap((note) => tidy(note) ?? [])
    .slice(0, PATTERN_BOX_NOTE_LIMIT)
    .sort(noteOrder);
}

/** Whether two lists of notes are the same notes, each as the other has it. */
export function sameNotes(a: readonly PatternNote[], b: readonly PatternNote[]): boolean {
  return a.length === b.length && a.every((note, i) => note.start === b[i]?.start && note.length === b[i].length && note.note === b[i].note && note.velocity === b[i].velocity);
}

/** What a box is called, from the text that was given: the text without the spaces round it, by a group's rule, and `Pattern` for a name of nothing (`pattern_title`); else what is wrong with it, in the controller's words. */
export function patternTitle(name: string): { name: string } | { error: string } {
  const tidied = tidyName(name);
  if ("error" in tidied) return tidied;
  return { name: tidied.name === "" ? DEFAULT_NAME : tidied.name };
}

/** What a box that is added is called: `Pattern N` for the lowest N, from 1, that no box is called (`SessionModel::next_pattern_name`). */
export function nextBoxName(nodes: readonly Node[]): string {
  const taken = new Set(nodes.flatMap((n) => (n.kind === "pattern" ? [n.name] : [])));
  for (let n = 1; ; n += 1) {
    if (!taken.has(`${DEFAULT_NAME} ${String(n)}`)) return `${DEFAULT_NAME} ${String(n)}`;
  }
}

/** A box that is new, as `AddPattern` makes it: no note, a loop of `PATTERN_BOX_DEFAULT_LENGTH` beats that goes round on its own clock, overwrites and clicks, and does nothing (`PatternBox::new`). */
export function newBox(id: NodeId, name: string): PatternBoxNode {
  return { id, parent: null, name, kind: "pattern", looping: true, sync: "own", overdub: false, click: true, lengthBeats: PATTERN_BOX_DEFAULT_LENGTH, notes: [], status: "stopped" };
}

/** What a setting of a box is about to be set to: the words for the step, the setting as the box has it now (what undo sets) and whether the box has what is asked for already (no step is kept for it), as `pattern_field_before` says. */
export function boxFieldBefore(box: PatternBoxNode, field: PatternBoxField): { label: string; undo: PatternBoxField; same: boolean } {
  const onOff = (on: boolean): string => (on ? "on" : "off");
  switch (field.type) {
    case "name": {
      const title = patternTitle(field.name);
      return { label: `rename ${box.name}`, undo: { type: "name", name: box.name }, same: "name" in title && title.name === box.name };
    }
    case "lengthBeats":
      return { label: `set the length of ${box.name}`, undo: { type: "lengthBeats", beats: box.lengthBeats }, same: clampLength(field.beats) === box.lengthBeats };
    case "looping":
      return { label: `turn looping ${onOff(field.on)} for ${box.name}`, undo: { type: "looping", on: box.looping }, same: field.on === box.looping };
    case "sync":
      return { label: `let ${box.name} go by ${field.sync === "host" ? "the host's beat" : "its own clock"}`, undo: { type: "sync", sync: box.sync }, same: field.sync === box.sync };
    case "overdub":
      return { label: `turn overdub ${onOff(field.on)} for ${box.name}`, undo: { type: "overdub", on: box.overdub }, same: field.on === box.overdub };
    case "click":
      return { label: `turn the click ${onOff(field.on)} for ${box.name}`, undo: { type: "click", on: box.click }, same: field.on === box.click };
  }
}
