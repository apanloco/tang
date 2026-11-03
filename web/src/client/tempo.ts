import { SYNC_DIVISIONS } from "../protocol";

/**
 * How many beats of the tempo a cycle (an LFO's) or a step (a step sequencer's) is, for the place `sync` in the list of note lengths that a modulator can follow the tempo by
 * (`SYNC_DIVISIONS`, from 1): none, 0, for 0 and for a place that the list has not (the engine's `sync` is that number of beats, and 0 is none).
 */
export function beatsOf(sync: number): number {
  return SYNC_DIVISIONS[sync - 1]?.beats ?? 0;
}

/**
 * The rate, in cycles or steps a second, that a modulator goes at, at `bpm` beats a minute: the tempo's if it follows it (its note length, `sync`, is not none), which is the
 * beats a minute over sixty over the beats of the note length, and its `own` rate if it does not. As the engine's `effective_rate` and `rate_at_tempo` (pinned to them by
 * `fixtures/sync.json`).
 */
export function rateAt(own: number, sync: number, bpm: number): number {
  const beats = beatsOf(sync);
  return beats > 0 ? bpm / (60 * beats) : own;
}
