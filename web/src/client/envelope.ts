// The shape of an ADSR envelope as the node of one draws it (docs/web-ui.md §9.3): its four stages one after the other, left to
// right, and where the envelope is on that shape while it runs. A pure function of the four settings and of the level the envelope
// puts out, so that the node is drawn from the numbers the session has and the dot from the telemetry, and nothing here knows of a page.

/** The settings of an envelope: the times of the attack, the decay and the release, in seconds, and the level it holds, 0 to 1. */
export interface Stages {
  attack: number;
  decay: number;
  sustain: number;
  release: number;
}

/**
 * Where the stages end, as shares of the width of the shape (0 to 1), and the level the sustain holds: the attack goes from nothing to
 * the top by `attackEnd`, the decay from the top to the sustain by `decayEnd`, the sustain holds until `holdEnd`, and the release goes
 * from the sustain to nothing by the right edge (1).
 */
export interface Shape {
  attackEnd: number;
  decayEnd: number;
  holdEnd: number;
  sustain: number;
}

/** The room a box of the shape has: its size, and the margin that keeps the line at its ends whole. */
export interface Box {
  width: number;
  height: number;
  pad: number;
}

/** What the engine's stages are measured against: a stage of this many seconds weighs `log10(2)`. */
const MILLISECOND = 0.001;

/** Below this level the envelope is at rest (the engine's ramps end at exactly 0). */
const IDLE = 1e-4;

/** A change of the level smaller than this is the level standing still (the telemetry is exact, so this is rounding). */
const TREND = 1e-5;

/** Within this of the sustain is the sustain. */
const AT_SUSTAIN = 1e-3;

/**
 * How long, in milliseconds, a level that was rising stands where it is before it is steady and not a reading that has not come yet: three
 * readings of the telemetry, which comes thirty times a second.
 */
const STEADY = 100;

const clamp01 = (value: number): number => Math.min(Math.max(value, 0), 1);

/**
 * How much room a stage of `seconds` has in the shape: the logarithm of how many milliseconds it is, plus one. A stage can be 1 ms or 10 s
 * (the controller's range), and on a scale of time that is not logarithmic the short one would be a hair's breadth next to the long.
 * Nothing for no time, and for a time that is not one.
 */
export function stageWeight(seconds: number): number {
  return Number.isFinite(seconds) && seconds > 0 ? Math.log10(1 + seconds / MILLISECOND) : 0;
}

/**
 * The shape of an envelope with these settings. The sustain has no time (it holds for as long as the key does): it is given the mean
 * width of the three stages that have, and never less than one weight, so that it is there where every stage is nothing.
 */
export function shapeOf({ attack, decay, sustain, release }: Stages): Shape {
  const [a, d, r] = [stageWeight(attack), stageWeight(decay), stageWeight(release)];
  const hold = Math.max(1, (a + d + r) / 3);
  const total = a + d + hold + r;
  return { attackEnd: a / total, decayEnd: (a + d) / total, holdEnd: (a + d + hold) / total, sustain: Number.isFinite(sustain) ? clamp01(sustain) : 0 };
}

/** Where a share of the width and a level are in a box, inside its margin: the bottom left is nothing at the start, the top right the top at the end. */
export function placeIn({ width, height, pad }: Box, share: number, level: number): { x: number; y: number } {
  return { x: pad + share * (width - 2 * pad), y: pad + (1 - level) * (height - 2 * pad) };
}

/** The five corners of the shape in a box: the bottom left, the top, the start and the end of the hold, and the bottom right. */
function cornersOf(shape: Shape, box: Box): { x: number; y: number }[] {
  return [placeIn(box, 0, 0), placeIn(box, shape.attackEnd, 1), placeIn(box, shape.decayEnd, shape.sustain), placeIn(box, shape.holdEnd, shape.sustain), placeIn(box, 1, 0)];
}

/** A coordinate to a tenth of a pixel, which is as fine as a line can be seen and keeps what is written short and the same from run to run. */
const tenth = (value: number): number => Math.round(value * 10) / 10;

/** The line of the envelope in a box, as the `d` of an SVG path. */
export function envelopePath(shape: Shape, box: Box): string {
  return cornersOf(shape, box)
    .map(({ x, y }, i) => `${i === 0 ? "M" : "L"}${String(tenth(x))} ${String(tenth(y))}`)
    .join(" ");
}

/** The same line, closed along the bottom, for the fill under it. */
export function envelopeAreaPath(shape: Shape, box: Box): string {
  return `${envelopePath(shape, box)} Z`;
}

/**
 * Which way the level of an envelope is going, as far as it is known: the level at which that was last settled, whether it was a rise, and
 * the time of the frame (milliseconds) that found it there. The telemetry comes more slowly than the page draws (30 readings a second against
 * 60 frames), so most frames find the level exactly where the frame before did, and that is no news: the level goes on the way it was going.
 */
export interface Course {
  level: number;
  rising: boolean;
  since: number;
}

/**
 * The course after a frame at `time` that read `level`, from the course before it (none on the first frame, and on the first after a gap in the
 * telemetry: the level is not known to be rising). A level that has moved by more than a rounding from the one that settled the course sets it,
 * up or down, and is the one that settles it from then on. One that has not moved is the same course, the very same object, until a rise has
 * stood still for `STEADY`: then it is a level that is steady (a rise that stops at the sustain: the attack and the decay were over between two
 * readings), which is a course that is not rising.
 */
export function courseAfter(level: number, course: Course | undefined, time: number): Course {
  if (!course) return { level, rising: false, since: time };
  if (level > course.level + TREND) return { level, rising: true, since: time };
  if (level < course.level - TREND) return { level, rising: false, since: time };
  if (course.rising && time - course.since >= STEADY) return { ...course, rising: false };
  return course;
}

/**
 * Where the envelope is on its shape, as a share of the width and a level (both 0 to 1), from the level it puts out and whether that level is
 * rising. The level alone is not enough (the same level is on the attack, the decay and the release), so which way it is going says which stage:
 * rising is the attack, through the level of the sustain too (the attack goes through it on its way up, and a note that comes while the envelope is
 * dying away goes on from where it is); not rising, the level at the sustain is the hold, at the top the top of the attack, above the sustain the
 * decay and below it the release. At rest it is at the start. A note that ends during the decay is drawn on the decay until the level is below
 * the sustain. The level is at the sustain when it is within a hair of it, and not only when it is it: the telemetry holds 32 bits and the
 * sustain of the session does not, so where the decay ends is a hair over or under it.
 */
export function envelopeDot(shape: Shape, level: number, rising: boolean): { x: number; y: number } {
  if (!Number.isFinite(level) || level < IDLE) return { x: 0, y: 0 };
  const y = Math.min(level, 1);
  const { attackEnd, decayEnd, holdEnd, sustain } = shape;
  const atSustain = Math.abs(y - sustain) <= AT_SUSTAIN;
  const atTop = y >= 1 - AT_SUSTAIN;
  const hold = { x: (decayEnd + holdEnd) / 2, y };
  // A sustain of 1: the attack ends in the hold, with no decay between them to be on.
  if (atSustain && atTop) return hold;
  if (rising) return { x: y * attackEnd, y };
  if (atSustain) return hold;
  if (atTop) return { x: attackEnd, y };
  // (A level at or above the sustain gets here only below the top, since at a sustain of 1 the top is the hold: the decay does not divide by nothing.
  // And below the sustain there is a sustain to release from, which is above nothing.)
  if (y >= sustain) return { x: attackEnd + ((1 - y) / (1 - sustain)) * (decayEnd - attackEnd), y };
  return { x: holdEnd + (1 - y / sustain) * (1 - holdEnd), y };
}
