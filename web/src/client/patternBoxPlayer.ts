// The engine's player of a pattern box, as the mock engine has it (docs/patch-graph.md §11.6, P4h-C part B): what `BoxPlayer` (`src/patternbox.rs`) does that the page shows, which is what a box is doing, the pass that it records and the notes that
// come of it, and not its frames. The mock has no audio and so no block: the box goes by the mock's clock, in seconds, at the tempo that the session has, and what it plays is not made (nothing is heard).
//
// A box is stopped, playing, or recording (counting in, waiting for the loop to begin on the host's clock, or making a pass). Record on the own clock counts in four beats and then takes one pass of the box's length, from the keys
// that are played, in beats; it plays what it took, from the start of the loop that the pass ended, or goes on with another pass if it overdubs, until it is told to finish (the pass that is going on is the last). A box that does not loop and
// plays plays once and stops. **The host's clock waits for ever here**: the mock has no host, as the engine has none outside a plugin, so a box that goes by it is counted as waiting and does not move.
import { PATTERN_BOX_NOTE_LIMIT, PATTERN_COUNT_IN_BEATS } from "../protocol";
import type { Graph, NodeId, PatternNote, PatternStatus } from "../protocol";
import { sanitize } from "./patternBoxes";

/** A beat that is this near a boundary is on it (`EPS` of the engine). */
const EPS = 1e-9;
/** The shortest a recorded note is, in beats: a key that came up in the instant it went down in. */
const MIN_NOTE_BEATS = 1 / 64;

/** What a box needs of the session to go: its loop and what it does with the notes, as the node says (the engine's plan and settings, which the controller keeps in step with the node). */
export interface BoxView {
  length: number;
  looping: boolean;
  overdub: boolean;
  clock: "own" | "host";
  notes: readonly PatternNote[];
}

/** What a box needs of the session to go, from the graph: the pattern box `node`, or none if it is not one (it has gone). */
export function boxViewOf(graph: Graph, node: NodeId): BoxView | undefined {
  const box = graph.nodes.find((n) => n.id === node);
  return box?.kind === "pattern" ? { length: box.lengthBeats, looping: box.looping, overdub: box.overdub, clock: box.sync, notes: box.notes } : undefined;
}

type Phase = { type: "countIn" } | { type: "waitLoop" } | { type: "pass"; start: number };
type State = { type: "stopped" } | { type: "playing" } | { type: "recording"; phase: Phase };

/** What a box that is running has: where it is, and what a pass has taken so far. A box that is stopped has none of it, and the mock keeps none. */
export class BoxRun {
  #state: State = { type: "stopped" };
  /** Where the box is on its clock, in beats: below 0 in a count-in, and from the start of the loop that it was in when it began, going on. */
  #clock = 0;
  /** When that was, in seconds of the server's clock. */
  #at: number;
  /** Whether the pass that is going on is the last (`FinishRecording`). */
  #finishing = false;
  /** The clock that the box was set going by. */
  #goesBy: "own" | "host" = "own";
  /** For each pitch that is down, the beat of the pass that it went down at and its velocity. */
  readonly #held = new Map<number, { began: number; velocity: number }>();
  /** The notes that the pass has taken so far, in the order they ended. */
  #taken: PatternNote[] = [];

  constructor(at: number) {
    this.#at = at;
  }

  /** What the box is doing, as the engine tells the controller. */
  get status(): PatternStatus {
    switch (this.#state.type) {
      case "stopped":
        return "stopped";
      case "playing":
        return "playing";
      case "recording":
        return this.#state.phase.type === "countIn" ? "countingIn" : this.#state.phase.type === "waitLoop" ? "waiting" : "recording";
    }
  }

  /** Whether the box is stopped: the mock lets go of a run that is. */
  get stopped(): boolean {
    return this.#state.type === "stopped";
  }

  /** Whose clock the box was set going by (`record` and `play` say): a box whose node says another clock now is stopped by the engine (`BoxPlayer::set_settings`). */
  get goesBy(): "own" | "host" {
    return this.#goesBy;
  }

  /** Whether the box will change what it is doing by itself as time goes by (a count-in, a pass, a box that plays once), and the mock has to look at it: one on the host's clock does not (it waits for a host that there is none of: nothing is due). */
  moving(view: BoxView): boolean {
    return this.#nextChange(view) < Number.POSITIVE_INFINITY;
  }

  /** Record: from the beginning of a count-in (the own clock), or from the next start of the loop (the host's). Pressed while it records, it begins again. */
  record(view: BoxView, at: number): void {
    this.#at = at;
    this.#finishing = false;
    this.#goesBy = view.clock;
    if (view.clock === "own") {
      this.#clock = -PATTERN_COUNT_IN_BEATS;
      this.#state = { type: "recording", phase: { type: "countIn" } };
    } else {
      this.#clock = 0;
      this.#state = { type: "recording", phase: { type: "waitLoop" } };
    }
  }

  /** Play from the start, if it is stopped (a box that plays, or records, goes on). */
  play(view: BoxView, at: number): void {
    if (this.#state.type !== "stopped") return;
    this.#at = at;
    this.#goesBy = view.clock;
    this.#state = { type: "playing" };
  }

  /** Stop. A pass that is going on is lost, and the notes that the box had before it are as they were. */
  stop(): void {
    this.#state = { type: "stopped" };
    this.#clock = 0;
  }

  /** Finish recording: a count-in or a wait is given up (the box stops), and a pass that is going on is the last one. */
  finish(): void {
    if (this.#state.type !== "recording") return;
    if (this.#state.phase.type === "pass") this.#finishing = true;
    else this.stop();
  }

  /**
   * Go on from where the box was to `to`, in seconds of the server's clock, at `bpm` beats a minute: the changes that fall in between are made, in order. `finished` is told the notes that each pass that ends has made the box's, as it ends (what the engine says of a
   * pass that is over, `BoxNotification::Recorded`: the notes the box has then), so that what is asked next is of a box that has them. `view` is asked at each change, since the notes and the length are the node's and a person may change them while the box goes.
   */
  advance(to: number, bpm: number, view: () => BoxView, finished: (notes: PatternNote[]) => void): void {
    // (A tempo that is no number above 0 is no way to go anywhere: time goes by, and the box is where it was. A box that goes by the host's clock has nothing due, and one that is stopped none either: neither changes what it is doing as time goes by.)
    if (!(bpm > 0) || !Number.isFinite(bpm)) {
      this.#at = to;
      return;
    }
    const c1 = this.#clock + ((to - this.#at) * bpm) / 60;
    let t = this.#clock;
    // (The engine cuts a block where the box changes what it is doing: a count-in is over, a pass ends, a box that plays once has played it.)
    for (let cuts = 0; cuts < 100_000; cuts += 1) {
      const now = view();
      const end = Math.min(this.#nextChange(now), c1);
      const moved = end > t + EPS;
      if (moved) t = end;
      const changed = this.#changeAt(t, now, finished);
      if (!moved && !changed) break;
    }
    // A box that plays once, and has played it, is at its start again.
    this.#clock = this.stopped ? 0 : c1;
    this.#at = to;
  }

  /** A key goes down (`on`) or comes up, now (the box has been brought up to now): taken into the pass if one is going on, at the beat of it. A key that goes down again before it came up ends the note that it held. */
  hear(on: boolean, pitch: number, velocity: number): void {
    if (this.#state.type !== "recording" || this.#state.phase.type !== "pass" || !Number.isInteger(pitch) || pitch < 0 || pitch > 127) return;
    const pos = this.#clock - this.#state.phase.start;
    const down = this.#held.get(pitch);
    if (on) {
      if (down !== undefined) this.#push(down.began, Math.max(pos - down.began, MIN_NOTE_BEATS), pitch, down.velocity);
      this.#held.set(pitch, { began: pos, velocity });
    } else if (down !== undefined) {
      this.#held.delete(pitch);
      this.#push(down.began, Math.max(pos - down.began, MIN_NOTE_BEATS), pitch, down.velocity);
    }
  }

  /** The clock at which the box next changes what it is doing: infinity if it does not. */
  #nextChange(view: BoxView): number {
    const state = this.#state;
    if (state.type === "recording") {
      if (state.phase.type === "countIn") return 0;
      if (state.phase.type === "pass") return state.phase.start + view.length;
      return Number.POSITIVE_INFINITY;
    }
    return state.type === "playing" && view.clock === "own" && !view.looping ? view.length : Number.POSITIVE_INFINITY;
  }

  /** The change that is due at the clock `t`, made if there is one: whether there was. */
  #changeAt(t: number, view: BoxView, finished: (notes: PatternNote[]) => void): boolean {
    const state = this.#state;
    if (state.type === "recording" && state.phase.type === "countIn" && t >= -EPS) {
      this.#taken = [];
      this.#held.clear();
      this.#state = { type: "recording", phase: { type: "pass", start: 0 } };
      return true;
    }
    if (state.type === "recording" && state.phase.type === "pass" && t >= state.phase.start + view.length - EPS) {
      finished(this.#finishPass(view));
      this.#state = view.overdub && !this.#finishing ? { type: "recording", phase: { type: "pass", start: state.phase.start + view.length } } : { type: "playing" };
      return true;
    }
    if (state.type === "playing" && view.clock === "own" && !view.looping && t >= view.length - EPS) {
      this.#state = { type: "stopped" };
      return true;
    }
    return false;
  }

  /** The pass is over: what is still held ends at the loop's end, and what was taken is the box's notes (all of them, or added to those that it had), as the box holds them. */
  #finishPass(view: BoxView): PatternNote[] {
    for (const [pitch, down] of [...this.#held].sort(([a], [b]) => a - b)) this.#push(down.began, Math.max(view.length - down.began, MIN_NOTE_BEATS), pitch, down.velocity);
    this.#held.clear();
    const had = view.overdub ? [...view.notes] : [];
    const room = Math.max(PATTERN_BOX_NOTE_LIMIT - had.length, 0);
    const notes = sanitize([...had, ...this.#taken.slice(0, room)]);
    this.#taken = [];
    return notes;
  }

  /** A note that the pass has taken, as the engine holds the numbers of a note (32 bits). */
  #push(start: number, length: number, note: number, velocity: number): void {
    if (this.#taken.length < PATTERN_BOX_NOTE_LIMIT) this.#taken.push({ start: Math.fround(start), length: Math.fround(length), note, velocity });
  }
}
